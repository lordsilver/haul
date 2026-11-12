//! One row of the queue: a file and every mirror it can come from.

use std::{
    collections::{BTreeMap, BTreeSet},
    time::Instant,
};

use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::{
    hosts::{Checksum, Entry, Need, guess_name, name_key, tidy_name},
    throttle::Pace,
};

crate::names::named! {
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
    pub enum Status {
        #[default]
        Queued = "queued",
        Resolving = "resolving",
        Downloading = "downloading",
        Waiting = "waiting",
        Paused = "paused",
        /// Every remaining mirror needs a person (captcha or browser step).
        Captcha = "captcha",
        Done = "done",
        Error = "error",
    }
}

crate::names::named! {
    /// Where a row stands with comparing a file already on disk to the
    /// checksum its host published. Serialized as `""`, `"waiting"` and
    /// `"comparing"`.
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, ts_rs::TS)]
    #[ts(export)]
    pub enum Verifying {
        #[default]
        No = "",
        /// For its turn, or while downloads are paused.
        Waiting = "waiting",
        Comparing = "comparing",
    }
}

crate::names::named! {
    /// Where the archive a row is a part of stands with being unpacked; every
    /// part of it carries the same. Serialized as `""`, `"waiting"`,
    /// `"extracting"`, `"done"`, `"password"` and `"failed"`.
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
    #[ts(export)]
    pub enum Extract {
        #[default]
        No = "",
        /// For its turn: one archive is unpacked at a time.
        Waiting = "waiting",
        Extracting = "extracting",
        Done = "done",
        /// It is encrypted, and no password Haul has fits: one is wanted.
        Password = "password",
        Failed = "failed",
    }
}

impl Extract {
    fn is_no(&self) -> bool {
        *self == Extract::No
    }
}

/// Where a row stands with a page the desktop app passes for it, out of
/// sight. The person is told once: a page that wants them again after its
/// own tries says nothing more.
///
/// - `InTurn`: waiting for, or in, its pass.
/// - `WithPerson`: the pass wanted a person ([`crate::Engine::page_wants_person`]);
///   tried again on a timer.
/// - `OnItsOwn`: the timer came due, and the page is passed again without
///   telling anyone.
/// - `WithPersonAgain`: that try wanted a person too, or the person opened
///   the page as it came due ([`crate::Engine::page_with_person`]); told
///   nobody.
///
/// The person retrying by hand starts it over at `InTurn`; retries counted
/// over again ([`Item::reset_retries`]) leave what the person was told.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum PageTurn {
    #[default]
    InTurn,
    WithPerson,
    OnItsOwn,
    WithPersonAgain,
}

/// What looking one link up came to, short of an answer, and when it is
/// asked again (seconds since the Unix epoch).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Fate {
    /// Its page told nothing: asked again after 15 minutes, doubling, a day
    /// at most.
    ToldNothing { tries: u32, next: u64 },
    /// Its host asked this address to wait.
    Limited { until: u64 },
}

impl Fate {
    /// Whether the link may be asked again at `now`.
    pub fn due(self, now: u64) -> bool {
        match self {
            Fate::ToldNothing { next, .. } => now >= next,
            Fate::Limited { until } => now >= until,
        }
    }

    /// The fate of a link that told nothing again, at `now`.
    pub fn told_nothing(before: Option<Fate>, now: u64) -> Fate {
        let tries = match before {
            Some(Fate::ToldNothing { tries, .. }) => tries + 1,
            _ => 1,
        };
        let wait = ((15 * 60u64) << (tries - 1).min(10)).min(24 * 3600);
        Fate::ToldNothing {
            tries,
            next: now + wait,
        }
    }
}

/// A host is limiting this row's download ([`crate::throttle`]) and it stays
/// on it for now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Slowed {
    pub host: String,
    /// Something other than its slowness shows the host is the cause.
    pub sure: bool,
    /// Why it stays, as the row says it.
    pub why: &'static str,
}

/// What a row's error is about, where the engine acts on it rather than only
/// shows it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Trouble {
    #[default]
    Other,
    /// Only hosts the user turned off have it: turning one on tries it again.
    OnlyOffHosts,
    /// Every page it could go to is on a host that asked this address to wait.
    AskedToWait,
    /// Its page wants a person, or a browser that may get past by itself.
    Needs(Need),
    /// A paste that lists no links: on a card with others that do, a note.
    ListsNothing,
    /// Haul's browser could not show its page: tried again by itself once
    /// the browser may work, using none of the page's own tries.
    Browser(crate::pass_ends::BrowserFailure),
}

impl Trouble {
    fn is_other(&self) -> bool {
        *self == Trouble::Other
    }

    /// A browser check, or a gate past it, stands in the way.
    pub fn is_guarded(self) -> bool {
        matches!(self, Trouble::Needs(Need::Check | Need::Gate))
    }
}

/// What unwrapping a wrapper found on its page.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export)]
pub struct Found {
    /// Files: the rows it added, or merged into rows already there.
    pub files: usize,
    /// Links to those files, mirrors counted.
    pub links: usize,
}

/// One link a file can come from, with its host as [`crate::hosts::host_of`]
/// names it, worked out once: the scheduler, the views and the planners ask
/// a mirror's host many times a second. Saved as the link alone.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
pub(crate) struct Mirror {
    pub url: String,
    pub host: std::sync::Arc<str>,
}

impl From<String> for Mirror {
    fn from(url: String) -> Self {
        let host = crate::hosts::host_of(&url).into();
        Self { url, host }
    }
}

impl From<&str> for Mirror {
    fn from(url: &str) -> Self {
        url.to_string().into()
    }
}

impl From<Mirror> for String {
    fn from(m: Mirror) -> Self {
        m.url
    }
}

impl Mirror {
    pub fn as_str(&self) -> &str {
        &self.url
    }
}

/// Reads as its link wherever a `&str` is wanted.
impl std::ops::Deref for Mirror {
    type Target = str;
    fn deref(&self) -> &str {
        &self.url
    }
}

impl std::borrow::Borrow<str> for Mirror {
    fn borrow(&self) -> &str {
        &self.url
    }
}

impl std::fmt::Display for Mirror {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.url)
    }
}

impl PartialEq<str> for Mirror {
    fn eq(&self, other: &str) -> bool {
        self.url == other
    }
}

impl PartialEq<&str> for Mirror {
    fn eq(&self, other: &&str) -> bool {
        self.url == *other
    }
}

impl PartialEq<String> for Mirror {
    fn eq(&self, other: &String) -> bool {
        &self.url == other
    }
}

/// Saved in `queue.json` as it is, but for what only matters while Haul runs
/// (`#[serde(skip)]`), by its id.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Item {
    #[serde(skip)]
    pub id: String,
    pub mirrors: Vec<Mirror>,
    #[serde(skip)]
    pub failed: BTreeSet<String>,
    /// Mirrors whose host or page said the file is gone (deleted, or a
    /// paste that lists nothing): no use trying them again on a timer.
    #[serde(skip_serializing_if = "BTreeSet::is_empty")]
    pub gone: BTreeSet<String>,
    /// Mirrors that need a person before they can be used.
    #[serde(skip)]
    pub manual_urls: BTreeSet<String>,
    /// Pages the desktop app opened out of sight only to learn what this
    /// file, not started, is ([`crate::Engine::looks_up`]), which told
    /// nothing: not opened for that again until a restart.
    #[serde(skip)]
    pub looked: BTreeSet<String>,
    /// What looking a link up came to when it gave no answer yet, and when
    /// to ask again: saved, so a restart neither forgets it nor asks again
    /// at once (plan LOOK-04).
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub fates: BTreeMap<String, Fate>,
    /// Where it stands with the page the desktop app passes for it.
    #[serde(skip)]
    pub page: PageTurn,
    /// The person was told its page wants them: not again until they act.
    #[serde(skip)]
    pub told_person: bool,
    /// A source whose gate its own tries did not pass, set aside unread:
    /// its release is whole without it, so nobody is asked (plan FLOW-02).
    /// Tried again quietly once a day, or read on request.
    #[serde(skip)]
    pub unread: bool,
    /// Listed by a wrapper and not started by the person yet: nothing about
    /// it is told as a failure or as wanting them, and losing a mirror
    /// leaves it waiting ([`Item::start`] clears it).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub unstarted: bool,
    /// `pre` came from the user (a pasted or captured link).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub manual: bool,
    pub name: Option<String>,
    pub package: String,
    /// An already resolved entry: from a folder listing (`pre_for` names the
    /// folder link it belongs to) or from the user (`pre_for` is `None`).
    pub pre: Option<Entry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pre_for: Option<String>,
    /// The name is known, so there is nothing to look up before scheduling.
    #[serde(skip)]
    pub probed: bool,
    #[serde(skip)]
    pub verifying: Verifying,
    pub status: Status,
    /// What went wrong last, in words; set with [`Item::set_error`].
    pub error: String,
    /// What `error` is about.
    #[serde(skip_serializing_if = "Trouble::is_other")]
    pub trouble: Trouble,
    pub size: u64,
    #[serde(skip)]
    pub done: u64,
    #[serde(skip)]
    pub speed: f64,
    #[serde(skip)]
    pub host: String,
    #[serde(skip)]
    pub not_before: Option<Instant>,
    /// Retries of the current mirror after network trouble.
    #[serde(skip)]
    pub retries: u32,
    /// Refusals in a row from a host already busy with other downloads.
    #[serde(skip)]
    pub busy: u32,
    /// Bytes downloaded when the current attempt started.
    #[serde(skip)]
    pub attempt_from: u64,
    /// Automatic retries of the whole download after it failed.
    #[serde(skip)]
    pub auto_retries: u32,
    /// When the next automatic retry happens.
    #[serde(skip)]
    pub retry_at: Option<Instant>,
    /// The mirror the running attempt goes to.
    #[serde(skip)]
    pub url: Option<String>,
    /// How the running transfer's speed has gone.
    #[serde(skip)]
    pub pace: Option<Pace>,
    /// The host limiting the running transfer, when one is.
    #[serde(skip)]
    pub slowed: Option<Slowed>,
    /// Mirrors that slowed this row, with its speed on each as it left: it
    /// goes back to one only when nothing else can take it, which beats
    /// sitting idle, or when the host it went on to is far slower.
    #[serde(skip)]
    pub slow_on: BTreeMap<String, f64>,
    /// The mirror it left for slowness that it goes back to next, the host
    /// it went on to being far slower ([`Item::slow_on`]): once.
    #[serde(skip)]
    pub back_to: Option<String>,
    /// The host the partial file's last bytes came from; `None` when not known
    /// (a restart, a merged row), which counts as another's.
    #[serde(skip)]
    pub part_host: Option<String>,
    /// The size of the file the partial file is part of; 0 when not known.
    #[serde(skip)]
    pub part_size: u64,
    /// Where the finished file went.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// The checksum a host published for the file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checksum: Option<Checksum>,
    /// Finished without downloading: the file was in the download folder.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub existing: bool,
    /// Cancels the running attempt; present exactly while one runs.
    #[serde(skip)]
    pub task: Option<CancellationToken>,
    /// The exit the running attempt goes out from.
    #[serde(skip)]
    pub exit: Option<crate::engine::Exit>,
    /// A wrapper's row: a link to further links, never a file. Unwrapping it
    /// adds its files, not started, and leaves it done with what it found.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub wrapper: bool,
    /// The wrapper row a file came from.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Other wrappers that list the same file (its links merged here as
    /// mirrors): it counts, starts and stays as theirs too.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub also_from: Vec<String>,
    /// The host the user picked to download it from, while it has the file;
    /// the others stand by.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prefer: Option<String>,
    /// A web page copied to the clipboard, kept only if it turns out to
    /// list links: one that lists none, or whose check wants a person, goes
    /// quietly.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub tentative: bool,
    /// A wrapper page's own name for what it holds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub found: Option<Found>,
    /// What a wrapper listed, row by row, as it was read: kept, so its
    /// files are known again without reading it again (plan MODEL-01).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub listing: Vec<crate::hosts::Listed>,
    /// The hosts a wrapper's links are on, in its order: what tells apart
    /// wrappers of one set of files, one per host.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub found_hosts: Vec<String>,
    /// The rounded size the wrapper's page showed for this file, to tell its
    /// files from another wrapper's.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub listed_size: Option<u64>,
    /// Unpacking the archive this row is a part of.
    #[serde(skip_serializing_if = "Extract::is_no")]
    pub extract: Extract,
    /// Why unpacking it failed.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub extract_error: String,
    /// The password for its archive: the one the user gave, or the one that
    /// opened it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extract_password: Option<String>,
    /// Named for its archive, as its parts on other hosts are, or by the
    /// link or list it came from and looked up only for its size: a host's
    /// own name for the file, when it is looked up, does not replace it.
    #[serde(skip)]
    pub named: bool,
}

impl Item {
    /// Whether wrapper `id` lists this file, as its source or as another
    /// wrapper whose links merged here.
    pub fn listed_by(&self, id: &str) -> bool {
        self.source.as_deref() == Some(id) || self.also_from.iter().any(|w| w == id)
    }

    /// Takes in the wrappers a merged row came from, beside this file's
    /// own. A row the user added keeps no source: it stays theirs.
    pub fn merge_wrappers(&mut self, source: Option<&String>, also: &[String]) {
        for w in source.into_iter().chain(also) {
            if self.source.as_ref() != Some(w) && !self.also_from.contains(w) {
                self.also_from.push(w.clone());
            }
        }
    }

    /// Takes in another row for the same file: its links and pages for a
    /// person, the wrappers it came from, and its checksum and archive
    /// password where this one has none.
    pub(crate) fn absorb(&mut self, other: Item) {
        for m in other.mirrors {
            if !self.mirrors.contains(&m) {
                self.mirrors.push(m);
            }
        }
        self.manual_urls.extend(other.manual_urls);
        self.merge_wrappers(other.source.as_ref(), &other.also_from);
        if self.checksum.is_none() {
            self.checksum = other.checksum;
        }
        if self.extract_password.is_none() {
            self.extract_password = other.extract_password;
        }
    }

    /// Takes in what a host says of its file. Its name stands in for one
    /// guessed from the link with `replace`, unless the row was named for
    /// its archive or list ([`Item::named`]); else only for none.
    pub(crate) fn learn(
        &mut self,
        name: Option<&str>,
        replace: bool,
        size: Option<u64>,
        checksum: Option<Checksum>,
    ) {
        if (self.name.is_none() || (replace && !self.named))
            && let Some(name) = name
        {
            self.name = Some(tidy_name(name));
        }
        if let Some(size) = size.filter(|s| *s > 0) {
            self.size = size;
        }
        if self.checksum.is_none() {
            self.checksum = checksum;
        }
    }

    pub fn new(
        urls: impl IntoIterator<Item = impl Into<Mirror>>,
        name: Option<String>,
        package: String,
    ) -> Self {
        let name = name.map(|n| tidy_name(&n)).filter(|n| !n.is_empty());
        Self {
            id: uuid::Uuid::new_v4().simple().to_string()[..10].to_string(),
            mirrors: urls.into_iter().map(Into::into).collect(),
            probed: name.is_some(),
            name,
            package,
            ..Self::default()
        }
    }

    pub fn running(&self) -> bool {
        self.task.is_some()
    }

    /// Stops its transfer, if one is running; the row's status stays.
    pub(crate) fn cancel(&self) {
        if let Some(task) = &self.task {
            task.cancel();
        }
    }

    /// Says what went wrong, and what that is about.
    pub(crate) fn set_error(&mut self, text: impl Into<String>, trouble: Trouble) {
        self.error = text.into();
        self.trouble = trouble;
    }

    pub(crate) fn clear_error(&mut self) {
        self.error.clear();
        self.trouble = Trouble::Other;
    }

    /// Waits until `until` before its next try, saying why.
    pub(crate) fn wait_until(&mut self, until: Instant, why: impl Into<String>, trouble: Trouble) {
        self.status = Status::Waiting;
        self.not_before = Some(until);
        self.set_error(why, trouble);
    }

    /// Back in the queue for its next turn, unless an attempt is running
    /// (which settles where it stands when it ends).
    pub(crate) fn queue_if_idle(&mut self) {
        self.unstarted = false;
        if !self.running() {
            self.status = Status::Queued;
        }
    }

    /// The person starts it: queued, and told of from now on.
    pub(crate) fn start(&mut self) {
        self.unstarted = false;
        self.status = Status::Queued;
    }

    /// Forgets what only its attempt that just ended had: its transfer, the
    /// exit and link it went through, its speed and what slowed it.
    pub(crate) fn end_attempt(&mut self) {
        self.speed = 0.0;
        self.task = None;
        self.exit = None;
        self.url = None;
        self.pace = None;
        self.slowed = None;
    }

    /// Starts its retry counts over and forgets what went wrong.
    pub(crate) fn reset_retries(&mut self) {
        self.clear_error();
        self.retries = 0;
        self.auto_retries = 0;
        self.retry_at = None;
        self.page = match self.page {
            PageTurn::WithPersonAgain => PageTurn::WithPerson,
            PageTurn::OnItsOwn => PageTurn::InTurn,
            turn => turn,
        };
    }

    /// The desktop app tried its page out of sight and it wants a person
    /// after all (its window came forward, or was closed).
    pub(crate) fn page_wants_person(&self) -> bool {
        matches!(self.page, PageTurn::WithPerson | PageTurn::WithPersonAgain)
    }

    /// Its page is passed again on its own after it wanted a person, who was
    /// told then: whatever comes of it tells nobody.
    pub(crate) fn page_retrying(&self) -> bool {
        matches!(self.page, PageTurn::OnItsOwn | PageTurn::WithPersonAgain)
    }

    /// The name used to recognise the same file on other hosts.
    pub fn key_name(&self) -> Option<String> {
        self.name
            .clone()
            .or_else(|| self.mirrors.first().and_then(|m| guess_name(m)))
            .map(|n| tidy_name(&n))
    }

    /// Whether this row holds the file `name` of `size` bytes (0 = unknown):
    /// the names match, and so do the sizes when both are known.
    /// A wrapper's row holds no file, so it is never the same as one.
    /// Names are compared as [`name_key`] gives them, so `a_b.rar` on one
    /// host and `a b.rar` on another are one file.
    pub fn same_file(&self, name: &str, size: u64) -> bool {
        !self.wrapper
            && self
                .key_name()
                .is_some_and(|own| name_key(&own) == name_key(name))
            && (self.size == 0 || size == 0 || self.size == size)
    }

    /// Finishes this row with the file at `path`, which was in the download
    /// folder already.
    pub fn adopt(&mut self, path: &std::path::Path) {
        if self.size == 0 {
            // Found by name alone: the file says how big it is.
            self.size = std::fs::metadata(path).map_or(0, |m| m.len());
        }
        self.status = Status::Done;
        self.existing = true;
        self.path = Some(path.display().to_string());
        self.done = self.size;
        self.speed = 0.0;
        self.reset_retries();
        self.not_before = None;
    }

    /// A row as `queue.json` held it, under `id`. Anything that was in
    /// flight is queued again; the partial file on disk lets it resume.
    pub fn restored(mut self, id: String) -> Self {
        self.id = id;
        if let Some(name) = self.name.take() {
            self.name = Some(tidy_name(&name)).filter(|n| !n.is_empty());
        }
        // Only a row queued again is looked up as Haul starts. Any other (a
        // wrapper, say, which has no name) goes straight to its attempt once
        // it goes at all, or nothing would ever take it from the queue.
        self.probed = self.name.is_some()
            || self.pre.is_some()
            || matches!(self.status, Status::Done | Status::Paused | Status::Error);
        self.part_size = self.size;
        // An archive Haul stopped in the middle of is unpacked again.
        if self.extract == Extract::Extracting {
            self.extract = Extract::Waiting;
        }
        if !matches!(self.status, Status::Done | Status::Paused | Status::Error) {
            self.status = Status::Queued;
        }
        // Downloaded bytes are not saved; a finished file has all of them.
        if self.status == Status::Done {
            self.done = self.size;
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The form of a row in `queue.json`, checked against what this version
    /// writes, loads again.
    #[test]
    fn a_row_as_this_version_writes_it_loads() {
        let mut it = Item::new(
            vec!["https://site.example/archive.part1.rar".to_string()],
            Some("archive.part1.rar".into()),
            "Example".into(),
        );
        it.status = Status::Error;
        it.error = "HTTP 500".into();
        it.size = 1024;
        let written = r#"{"mirrors":["https://site.example/archive.part1.rar"],"name":"archive.part1.rar","package":"Example","status":"error","error":"HTTP 500","pre":null,"size":1024}"#;
        assert_eq!(
            serde_json::to_value(&it).unwrap(),
            serde_json::from_str::<serde_json::Value>(written).unwrap()
        );

        let back = serde_json::from_str::<Item>(written)
            .unwrap()
            .restored("abc123".into());
        assert_eq!(back.id, "abc123");
        assert_eq!(back.mirrors, it.mirrors);
        assert_eq!(back.name.as_deref(), Some("archive.part1.rar"));
        assert_eq!(back.package, "Example");
        assert_eq!(back.status, Status::Error);
        assert_eq!(back.error, "HTTP 500");
        assert_eq!(back.size, 1024);
    }

    /// A row loads whatever its state, with or without a name, and what was
    /// in flight is queued again.
    #[test]
    fn a_saved_row_in_any_state_loads() {
        for (status, restored) in [
            (Status::Queued, Status::Queued),
            (Status::Resolving, Status::Queued),
            (Status::Downloading, Status::Queued),
            (Status::Waiting, Status::Queued),
            (Status::Captcha, Status::Queued),
            (Status::Paused, Status::Paused),
            (Status::Done, Status::Done),
            (Status::Error, Status::Error),
        ] {
            let mut it = Item::new(
                vec!["https://site.example/video.mkv".to_string()],
                None,
                String::new(),
            );
            it.status = status;
            let text = serde_json::to_string(&it).unwrap();
            let saved: Item = serde_json::from_str(&text).unwrap();
            assert_eq!(saved.restored(it.id).status, restored, "{text}");
        }
    }
}
