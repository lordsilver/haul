//! One row of the queue: a file and every mirror it can come from.

use std::{collections::HashSet, time::Instant};

use serde::{Deserialize, Deserializer, Serialize};
use tokio_util::sync::CancellationToken;

use crate::hosts::{Checksum, Entry, guess_name, name_key, tidy_name};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Queued,
    Resolving,
    Downloading,
    Waiting,
    Paused,
    /// Every remaining mirror needs a person (captcha or browser step).
    Captcha,
    Done,
    Error,
}

/// Where a row stands with comparing a file already on disk to the
/// checksum its host published. Serialized as `""`, `"waiting"` and
/// `"comparing"`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum Verifying {
    #[default]
    #[serde(rename = "")]
    No,
    /// For its turn, or while downloads are paused.
    Waiting,
    Comparing,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Queued => "queued",
            Status::Resolving => "resolving",
            Status::Downloading => "downloading",
            Status::Waiting => "waiting",
            Status::Paused => "paused",
            Status::Captcha => "captcha",
            Status::Done => "done",
            Status::Error => "error",
        }
    }
}

/// Where the archive a row is a part of stands with being unpacked; every
/// part of it carries the same. Serialized as `""`, `"waiting"`,
/// `"extracting"`, `"done"`, `"password"` and `"failed"`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum Extract {
    #[default]
    #[serde(rename = "")]
    No,
    /// For its turn: one archive is unpacked at a time.
    Waiting,
    Extracting,
    Done,
    /// It is encrypted, and no password Haul has fits: one is wanted.
    Password,
    Failed,
}

impl Extract {
    fn is_no(&self) -> bool {
        *self == Extract::No
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

#[derive(Debug)]
pub(crate) struct Item {
    pub id: String,
    pub mirrors: Vec<String>,
    pub failed: HashSet<String>,
    /// Mirrors that need a person before they can be used.
    pub manual_urls: HashSet<String>,
    /// The desktop app tried this item's page out of sight and it wants a
    /// person after all (its window came forward, or was closed).
    pub page_wants_person: bool,
    /// `pre` came from the user (a pasted or captured link).
    pub manual: bool,
    pub name: Option<String>,
    pub package: String,
    /// An already resolved entry: from a folder listing (`pre_for` names the
    /// folder link it belongs to) or from the user (`pre_for` is `None`).
    pub pre: Option<Entry>,
    pub pre_for: Option<String>,
    /// The name is known, so there is nothing to look up before scheduling.
    pub probed: bool,
    pub verifying: Verifying,
    pub status: Status,
    pub error: String,
    pub size: u64,
    pub done: u64,
    pub speed: f64,
    /// Progress reported by an external program when the size is unknown.
    pub percent: Option<f32>,
    pub host: String,
    pub not_before: Option<Instant>,
    /// Retries of the current mirror after network trouble.
    pub retries: u32,
    /// Refusals in a row from a host already busy with other downloads.
    pub busy: u32,
    /// Bytes downloaded when the current attempt started.
    pub attempt_from: u64,
    /// Automatic retries of the whole download after it failed.
    pub auto_retries: u32,
    /// When the next automatic retry happens.
    pub retry_at: Option<Instant>,
    /// Where the finished file went.
    pub path: Option<String>,
    /// The checksum a host published for the file.
    pub checksum: Option<Checksum>,
    /// Finished without downloading: the file was in the download folder.
    pub existing: bool,
    /// Cancels the running attempt; present exactly while one runs.
    pub task: Option<CancellationToken>,
    /// The exit the running attempt goes out from.
    pub exit: Option<usize>,
    /// A wrapper's row: a link to further links, never a file. Unwrapping it
    /// adds its files, not started, and leaves it done with what it found.
    pub wrapper: bool,
    /// The wrapper row a file came from.
    pub source: Option<String>,
    /// Other wrappers that list the same file (its links merged here as
    /// mirrors): it counts, starts and stays as theirs too.
    pub also_from: Vec<String>,
    /// A wrapper page's own name for what it holds.
    pub title: Option<String>,
    pub found: Option<Found>,
    /// The hosts a wrapper's links are on, in its order: what tells apart
    /// wrappers of one release, one per host.
    pub found_hosts: Vec<String>,
    /// Unpacking the archive this row is a part of.
    pub extract: Extract,
    /// Why unpacking it failed.
    pub extract_error: String,
    /// The password for its archive: the one the user gave, or the one that
    /// opened it.
    pub extract_password: Option<String>,
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

    pub fn new(urls: Vec<String>, name: Option<String>, package: String) -> Self {
        let name = name.map(|n| tidy_name(&n)).filter(|n| !n.is_empty());
        Self {
            id: uuid::Uuid::new_v4().simple().to_string()[..10].to_string(),
            mirrors: urls,
            failed: HashSet::new(),
            manual_urls: HashSet::new(),
            manual: false,
            probed: name.is_some(),
            verifying: Verifying::No,
            name,
            package,
            pre: None,
            pre_for: None,
            status: Status::Queued,
            error: String::new(),
            size: 0,
            done: 0,
            speed: 0.0,
            percent: None,
            host: String::new(),
            not_before: None,
            retries: 0,
            busy: 0,
            attempt_from: 0,
            auto_retries: 0,
            retry_at: None,
            path: None,
            checksum: None,
            existing: false,
            task: None,
            exit: None,
            wrapper: false,
            source: None,
            also_from: Vec::new(),
            page_wants_person: false,
            title: None,
            found: None,
            found_hosts: Vec::new(),
            extract: Extract::No,
            extract_error: String::new(),
            extract_password: None,
        }
    }

    pub fn running(&self) -> bool {
        self.task.is_some()
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

    pub fn saved(&self) -> Saved {
        Saved {
            mirrors: self.mirrors.clone(),
            name: self.name.clone(),
            package: Some(self.package.clone()),
            status: self.status,
            error: self.error.clone(),
            pre: self.pre.clone(),
            size: Some(self.size),
            pre_for: self.pre_for.clone(),
            manual: self.manual,
            path: self.path.clone(),
            checksum: self.checksum.clone(),
            existing: self.existing,
            wrapper: self.wrapper,
            source: self.source.clone(),
            also_from: self.also_from.clone(),
            title: self.title.clone(),
            found: self.found,
            found_hosts: self.found_hosts.clone(),
            extract: self.extract,
            extract_error: self.extract_error.clone(),
            extract_password: self.extract_password.clone(),
        }
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
        self.percent = None;
        self.error.clear();
        self.retries = 0;
        self.auto_retries = 0;
        self.retry_at = None;
        self.not_before = None;
    }

    /// Rebuilds a saved row. Anything that was in flight is queued again; the
    /// partial file on disk lets it resume.
    pub fn restore(id: String, s: Saved) -> Self {
        let mut it = Item::new(s.mirrors, s.name, s.package.unwrap_or_default());
        it.id = id;
        it.probed = it.name.is_some() || s.pre.is_some();
        it.pre = s.pre;
        it.pre_for = s.pre_for;
        it.manual = s.manual;
        it.size = s.size.unwrap_or(0);
        it.error = s.error;
        it.path = s.path;
        it.checksum = s.checksum;
        it.existing = s.existing;
        it.wrapper = s.wrapper;
        it.source = s.source;
        it.also_from = s.also_from;
        it.title = s.title;
        it.found = s.found;
        it.found_hosts = s.found_hosts;
        // An archive Haul stopped in the middle of is unpacked again.
        it.extract = match s.extract {
            Extract::Extracting => Extract::Waiting,
            other => other,
        };
        it.extract_error = s.extract_error;
        it.extract_password = s.extract_password;
        it.status = match s.status {
            Status::Done | Status::Paused | Status::Error => s.status,
            _ => Status::Queued,
        };
        // Downloaded bytes are not saved; a finished file has all of them.
        if it.status == Status::Done {
            it.done = it.size;
        }
        it
    }
}

/// The on-disk form of an item in `queue.json`, compatible with the queue
/// files of the original Python version.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Saved {
    pub mirrors: Vec<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub package: Option<String>,
    #[serde(deserialize_with = "lenient_status")]
    pub status: Status,
    #[serde(default)]
    pub error: String,
    #[serde(default)]
    pub pre: Option<Entry>,
    #[serde(default)]
    pub size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pre_for: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub manual: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checksum: Option<Checksum>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub existing: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub wrapper: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub also_from: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub found: Option<Found>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub found_hosts: Vec<String>,
    #[serde(default, skip_serializing_if = "Extract::is_no")]
    pub extract: Extract,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub extract_error: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extract_password: Option<String>,
}

fn lenient_status<'de, D: Deserializer<'de>>(d: D) -> Result<Status, D::Error> {
    let s = String::deserialize(d)?;
    Ok(serde_json::from_value(serde_json::Value::String(s)).unwrap_or(Status::Queued))
}
