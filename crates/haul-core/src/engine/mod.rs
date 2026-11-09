//! The download engine: queue, mirror grouping, scheduling and transfers.
//!
//! One mutex guards all queue state and is never held across an await. Each
//! attempt runs as its own task holding a slot on its host; a scheduler tick
//! starts new attempts, picking for every item the mirror whose host is free
//! and has measured the best throughput.

use std::{
    collections::HashMap,
    collections::{BTreeMap, HashSet},
    fs::File,
    path::{Path, PathBuf},
    str::FromStr,
    sync::{
        Arc, Mutex, MutexGuard, RwLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant, SystemTime},
};

use indexmap::IndexMap;
use serde::Serialize;
use tokio::sync::{Notify, Semaphore, broadcast, watch};
use tokio_util::sync::CancellationToken;

use crate::{
    config::{self, Config, Route, Theme},
    container::{self, Link},
    extract,
    hosts::{self, Cx, Entry, Http, Need, ResolveError, Tokens},
    item::{Found, Item, Mirror, Status, Trouble, Verifying},
    limiter::Limiter,
    logging::unix_now,
    parts, paths, store,
};

mod diagnostics;
mod inspect;
mod pages;
mod passes;
mod reconnect;
mod schedule;
mod services;
mod settings;
mod snapshot;
mod throttled;
mod transfer;
mod unpack;
mod views;

pub use passes::{Browsing, PassPlan};
pub use reconnect::ReconnectView;
pub use services::ServicesView;

pub use settings::{SettingError, SettingsView};
pub use snapshot::{HostView, ItemView, PoolView, QuotaView, Snapshot, WrapperView};
pub use views::{
    Alert, ArchiveView, Command, Counts, HostGroup, List, PoolSource, PoolSummary, RowState,
    SiteKind, Tally, WrapperCard, WrapperState,
};

/// Automatic retries of a failed download, when `retry_failed_minutes` is on.
const AUTO_RETRIES: u32 = 3;

/// After a transfer from a host that takes a limited number at once, the next
/// one waits this long, so the host has counted the finished one out.
const SETTLE: Duration = Duration::from_secs(2);

/// The most downloads Haul runs at once from one host, per exit.
const AT_ONCE: usize = 32;

/// What the system calls the place deleted files go.
pub const BIN: &str = if cfg!(windows) {
    "Recycle Bin"
} else {
    "trash"
};

pub struct Options {
    /// Holds `config.toml`, the queue and the instance lock.
    pub data_dir: PathBuf,
    /// Overrides `download_dir` from the config.
    pub download_dir: Option<PathBuf>,
    /// Used instead of reading `config.toml` and the environment.
    pub config: Option<Config>,
    /// A folder that removed files move to instead of the system's
    /// [`BIN`]; only tests set it.
    pub recycle_bin: Option<PathBuf>,
}

#[derive(Debug, thiserror::Error)]
pub enum StartError {
    #[error("another Haul is already using {}", .0.display())]
    Locked(PathBuf),
    #[error("{}: {source}", path.display())]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{}: {message}; move it away to start with an empty queue", path.display())]
    Corrupt { path: PathBuf, message: String },
    #[error("{}: {message}", path.display())]
    Config { path: PathBuf, message: String },
    #[error("{0}")]
    Proxy(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemAction {
    Pause,
    Resume,
    Retry,
    /// Try the mirrors that wanted a person again.
    RetryManual,
    Remove,
    /// Unpack the archive in parts the item is one of.
    Extract,
}

impl FromStr for ItemAction {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, ()> {
        Ok(match s {
            "pause" => Self::Pause,
            "resume" => Self::Resume,
            "retry" => Self::Retry,
            "retry_manual" => Self::RetryManual,
            "remove" => Self::Remove,
            "extract" => Self::Extract,
            _ => return Err(()),
        })
    }
}

/// The partial files in `dir/.partial`, by the id of the row each belongs to,
/// with their lengths.
fn partial_sizes(dir: &Path) -> HashMap<String, u64> {
    let Ok(entries) = std::fs::read_dir(dir.join(".partial")) else {
        return HashMap::new();
    };
    entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let id = name.strip_suffix(".part")?.to_string();
            let meta = e.metadata().ok()?;
            meta.is_file().then_some((id, meta.len()))
        })
        .collect()
}

/// The file in `.partial` that names the data folder whose queue the partial
/// files belong to.
const PARTIAL_OWNER: &str = "owner";

/// Whether the Haul on `data_dir` may delete partial files in
/// `download_dir/.partial` no row of its own claims. Another Haul with a data
/// folder of its own (a test copy, say) may share the download folder, and
/// its partial files are not this one's to delete: the folder is owned by
/// whichever data folder came first, as long as that one still has a queue.
fn owns_partials(download_dir: &Path, data_dir: &Path) -> bool {
    let folder = download_dir.join(".partial");
    if !folder.is_dir() {
        return true;
    }
    let marker = folder.join(PARTIAL_OWNER);
    let me = std::fs::canonicalize(data_dir).unwrap_or_else(|_| data_dir.to_path_buf());
    if let Ok(owner) = std::fs::read_to_string(&marker) {
        let owner = PathBuf::from(owner.trim());
        if owner == me {
            return true;
        }
        if owner.join(store::QUEUE).exists() {
            return false;
        }
    }
    let _ = std::fs::write(&marker, me.to_string_lossy().as_bytes());
    true
}

/// What a wrapper's buttons do ([`Engine::wrappers_action`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WrapperAction {
    /// Start the files it added that wait.
    Start,
    /// Take it off the list, with its files that never started.
    Remove,
    /// Unwrap it again, after it failed.
    Retry,
}

impl FromStr for WrapperAction {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, ()> {
        Ok(match s {
            "start" => Self::Start,
            "remove" => Self::Remove,
            "retry" => Self::Retry,
            _ => return Err(()),
        })
    }
}

/// Why a batch left a row as it was.
const NO_SUCH: &str = "no such item";

/// What an action on many rows came to.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, ts_rs::TS)]
#[ts(export)]
pub struct Batch {
    /// Rows it was done to.
    pub done: usize,
    /// A wrapper's files started, each counted once.
    pub started: usize,
    /// The rows it was not done to, and why.
    pub failed: Vec<BatchFailure>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ts_rs::TS)]
#[ts(export)]
pub struct BatchFailure {
    pub id: String,
    pub error: String,
}

impl Batch {
    fn failed(&mut self, id: &str, error: impl Into<String>) {
        self.failed.push(BatchFailure {
            id: id.to_string(),
            error: error.into(),
        });
    }
}

/// Takes off the list the unwrapped wrappers none of whose files is on it
/// any more: they stood for those files, and starting or removing them
/// would do nothing.
fn drop_spent_wrappers(items: &mut IndexMap<String, Item>) {
    let sources: HashSet<String> = items
        .values()
        .flat_map(|i| i.source.iter().chain(&i.also_from).cloned())
        .collect();
    items.retain(|id, i| !(i.wrapper && i.status == Status::Done && !sources.contains(id)));
}

/// Things worth telling the user about outside the queue view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Finished {
        id: String,
        name: String,
    },
    NeedsUser {
        id: String,
        name: String,
    },
    Failed {
        id: String,
        name: String,
        error: String,
    },
    /// The theme changed, in the app or in the settings file.
    ThemeChanged(Theme),
    /// Something the person should hear about that is no download's.
    Notice {
        title: String,
        body: String,
    },
}

/// What kind of thing [`summarize`] tells.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Told {
    Finished,
    Failed,
    NeedsYou,
    Notice,
}

/// One thing to tell the person, worded for a notification or a toast.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Telling {
    pub title: String,
    pub body: String,
    pub kind: Told,
}

/// A burst of events as the few things worth saying: downloads finished
/// together are one, as are failures, and downloads waiting for a person.
pub fn summarize(events: &[Event]) -> Vec<Telling> {
    let mut finished = Vec::new();
    let mut waiting = Vec::new();
    let mut failed = Vec::new();
    let mut notices = Vec::new();
    for e in events {
        match e {
            Event::Finished { name, .. } => finished.push(name.as_str()),
            Event::NeedsUser { name, .. } => waiting.push(name.as_str()),
            Event::Failed { name, error, .. } => failed.push(format!("{name}: {error}")),
            Event::ThemeChanged(_) => {}
            Event::Notice { title, body } => notices.push((title.clone(), body.clone())),
        }
    }
    let list = |names: &[&str]| {
        let shown = names.iter().take(3).copied().collect::<Vec<_>>().join(", ");
        if names.len() > 3 {
            format!("{shown} and {} more", names.len() - 3)
        } else {
            shown
        }
    };
    let tell = |title: String, body: String, kind| Telling { title, body, kind };
    let mut out = Vec::new();
    match finished.len() {
        0 => {}
        1 => out.push(tell(
            "Downloaded".into(),
            finished[0].to_string(),
            Told::Finished,
        )),
        n => out.push(tell(
            format!("{n} downloads finished"),
            list(&finished),
            Told::Finished,
        )),
    }
    if !waiting.is_empty() {
        out.push(tell(
            "Needs you".into(),
            format!(
                "{}: open it in Haul to solve the host's page",
                list(&waiting)
            ),
            Told::NeedsYou,
        ));
    }
    for (title, body) in notices {
        out.push(tell(title, body, Told::Notice));
    }
    match failed.len() {
        0 => {}
        1 => out.push(tell(
            "Download failed".into(),
            failed[0].clone(),
            Told::Failed,
        )),
        n => out.push(tell(
            format!("{n} downloads failed"),
            failed
                .iter()
                .take(3)
                .cloned()
                .collect::<Vec<_>>()
                .join("\n"),
            Told::Failed,
        )),
    }
    out
}

#[cfg(test)]
mod summarize_tests {
    use super::*;

    #[test]
    fn a_burst_is_told_in_a_few_lines() {
        let events = [
            Event::NeedsUser {
                id: "waiting".into(),
                name: "a.bin".into(),
            },
            Event::Finished {
                id: "done".into(),
                name: "b.bin".into(),
            },
            Event::Finished {
                id: "done2".into(),
                name: "c.bin".into(),
            },
        ];
        let out = summarize(&events);
        let waiting: Vec<_> = out.iter().filter(|t| t.kind == Told::NeedsYou).collect();
        assert_eq!(waiting.len(), 1, "{out:?}");
        assert_eq!(waiting[0].title, "Needs you");
        assert!(waiting[0].body.contains("a.bin"));
        assert!(
            out.iter()
                .any(|t| t.title == "2 downloads finished" && t.body == "b.bin, c.bin")
        );
    }
}

/// What is downloading ([`Engine::activity`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Activity {
    pub downloading: usize,
    /// Bytes per second over them.
    pub speed: f64,
    pub paused: bool,
}

/// What adding links did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, ts_rs::TS)]
#[ts(export)]
pub struct Added {
    /// New rows.
    pub added: usize,
    /// Links that joined an existing row as another mirror.
    pub merged: usize,
    /// Links already in the queue.
    pub duplicates: usize,
    /// Links that are not http(s).
    pub invalid: usize,
    /// Wrapper links (fileguard.cc, filecrypt.cc), unwrapped as they come.
    pub wrappers: usize,
    /// Copied web pages being looked at for links; those that list none go.
    pub pages: usize,
}

impl std::ops::AddAssign for Added {
    fn add_assign(&mut self, other: Self) {
        self.added += other.added;
        self.merged += other.merged;
        self.duplicates += other.duplicates;
        self.invalid += other.invalid;
        self.wrappers += other.wrappers;
        self.pages += other.pages;
    }
}

/// Where a download leaves this machine from: 0 is its own connection, `n`
/// the n-th proxy of the pool ([`Config::proxy_pool`]).
pub type Exit = usize;

/// One host as seen from one exit: what it allows one address, the limit it
/// showed and the waits it asked for, since hosts count those per address.
#[derive(Debug, Default)]
struct Lane {
    /// Download requests to it in flight and transfers from it in progress:
    /// what a refusal is measured against. Counting requests that may still be
    /// refused can only overstate it, and the next refusal corrects that.
    serving: usize,
    /// No new download from it before this; see [`SETTLE`].
    settle_until: Option<Instant>,
    /// See [`HostView::cap`]. Kept until a restart.
    cap: Option<usize>,
    /// When `cap` last changed: one more is allowed after [`CAP_RELAX`]
    /// without a refusal.
    cap_since: Option<Instant>,
    active: usize,
    cooldown: Option<Instant>,
    /// `cooldown` is for the host slowing this address, not a wait it asked
    /// for: a transfer recovering ends it.
    slowing: bool,
    /// ... and something other than the slowness shows the host is the cause:
    /// worth a new address.
    slowing_sure: bool,
    /// Times in a row the host slowed this address, which `cooldown` doubles
    /// with ([`crate::throttle::Throttling::cooldown_for`]).
    strikes: u32,
    /// What the host says this address may still fetch at full speed today
    /// (pixeldrain publishes it).
    quota: Option<hosts::Allowance>,
}

impl Lane {
    /// A limit the host showed is tried one higher after [`CAP_RELAX`]
    /// without a refusal, and dropped once it reaches `limit`: a refusal
    /// can be a moment's crowding, and a host can loosen up.
    fn relax(&mut self, limit: usize, now: Instant) {
        if let (Some(cap), Some(since)) = (self.cap, self.cap_since)
            && now.duration_since(since) >= CAP_RELAX
        {
            if cap + 1 >= limit {
                self.cap = None;
                self.cap_since = None;
            } else {
                self.cap = Some(cap + 1);
                self.cap_since = Some(now);
            }
        }
    }

    /// The address behind this exit is a new one, or the host's day turned
    /// over: what was learned about the old one no longer holds.
    fn fresh_start(&mut self) {
        self.cap = None;
        self.cap_since = None;
        self.cooldown = None;
        self.settle_until = None;
        self.slowing = false;
        self.slowing_sure = false;
        self.strikes = 0;
    }

    /// The host asked this address to wait until `until`.
    fn asked_to_wait(&mut self, until: Instant) {
        self.cooldown = Some(until);
        self.slowing = false;
        self.slowing_sure = false;
    }

    fn slowed(&self) -> bool {
        self.quota.is_some_and(|q| q.slowed())
    }

    fn cooling(&self, now: Instant) -> bool {
        self.cooldown.is_some_and(|c| now < c)
    }

    /// Downloads that may run at once: `limit`, or fewer when the host
    /// showed it takes fewer.
    fn slots(&self, limit: usize) -> usize {
        self.cap.map_or(limit, |cap| cap.min(limit))
    }

    /// Whether one more download may start here now. With `slowing_too`,
    /// for a row with nowhere else to go, a host that slows the address
    /// still serves it; a wait the host asked for stands.
    fn free(&self, limit: usize, now: Instant, slowing_too: bool) -> bool {
        self.active < self.slots(limit)
            && ((slowing_too && self.slowing) || !self.cooling(now))
            && self.settle_until.is_none_or(|t| now >= t)
    }

    /// The host no longer slows this address: its cooldown for that ends.
    fn end_slowing(&mut self) {
        self.cooldown = None;
        self.slowing = false;
        self.slowing_sure = false;
    }
}

struct HostStats {
    /// Smoothed bytes per second of one stream.
    ema: f64,
    /// When `ema` last changed, in seconds since 1970.
    measured: u64,
    /// What one stream gets when the host isn't limiting the address: learned
    /// only from whole windows above the slow line, since `ema` follows a
    /// throttled transfer down within seconds. 0 until one was seen.
    normal: f64,
    /// A redial was made because of this host, and it hasn't been seen running
    /// well since.
    redialed_for: bool,
    /// It was as slow on the new address as on the old: it doesn't limit by
    /// address, so another redial would cut the line for nothing. For a week,
    /// across restarts, or until it runs well again.
    redial_useless: bool,
    /// When that was found, in seconds since 1970; 0 while it isn't.
    redial_useless_at: u64,
    fails: u32,
    /// Failures weighed by age: each halves every [`PENALTY_HALF_LIFE`].
    penalty: f64,
    penalty_at: Instant,
    ok: u32,
    bytes: u64,
    blocked: bool,
}

const PENALTY_HALF_LIFE: f64 = 600.0;

/// How long a limit a host showed holds before one more is tried.
const CAP_RELAX: Duration = Duration::from_secs(600);

/// Saved host speeds older than this are forgotten, so a host that had a bad
/// day gets measured again.
const SPEED_MEMORY: u64 = 7 * 24 * 3600;

impl HostStats {
    fn penalty(&self, now: Instant) -> f64 {
        let age = now.saturating_duration_since(self.penalty_at).as_secs_f64();
        self.penalty * 0.5f64.powf(age / PENALTY_HALF_LIFE)
    }

    fn penalize(&mut self, now: Instant) {
        self.penalty = self.penalty(now) + 1.0;
        self.penalty_at = now;
        self.fails += 1;
    }

    /// Takes in a whole window of a transfer that ran well: quick to learn the
    /// host can do more, slow to believe it does less.
    fn learn_normal(&mut self, speed: f64) {
        self.normal = if self.normal == 0.0 {
            speed
        } else if speed > self.normal {
            self.normal * 0.7 + speed * 0.3
        } else {
            self.normal * 0.99 + speed * 0.01
        };
    }

    /// Takes in one stream's speed over the last second.
    fn sample(&mut self, speed: f64) {
        self.ema = if self.ema == 0.0 {
            speed
        } else {
            self.ema * 0.7 + speed * 0.3
        };
        self.measured = unix_now();
    }

    /// A whole window ran well at `speed`: the host is not limiting this
    /// address, whatever a redial made for it showed.
    fn ran_well(&mut self, speed: f64) {
        self.learn_normal(speed);
        self.redialed_for = false;
        self.redial_useless = false;
        self.redial_useless_at = 0;
    }

    /// Unknown hosts score high, so each gets tried and the ranking learns;
    /// recent failures weigh more than old ones.
    fn score(&self, now: Instant) -> f64 {
        let speed = if self.ema > 0.0 { self.ema } else { 1e6 };
        speed / (1.0 + self.penalty(now) * 0.5)
    }

    fn new() -> Self {
        Self {
            ema: 0.0,
            measured: 0,
            normal: 0.0,
            redialed_for: false,
            redial_useless: false,
            redial_useless_at: 0,
            fails: 0,
            penalty: 0.0,
            penalty_at: Instant::now(),
            ok: 0,
            bytes: 0,
            blocked: false,
        }
    }

    /// What was learned in an earlier run.
    fn restore(saved: &store::SavedHost, now: u64) -> Self {
        let fresh = now.saturating_sub(saved.measured) < SPEED_MEMORY;
        Self {
            ema: if fresh { saved.speed as f64 } else { 0.0 },
            measured: if fresh { saved.measured } else { 0 },
            normal: if fresh { saved.normal as f64 } else { 0.0 },
            redial_useless: saved.redial_useless_at > 0
                && now.saturating_sub(saved.redial_useless_at) < SPEED_MEMORY,
            redial_useless_at: saved.redial_useless_at,
            ok: saved.ok,
            fails: saved.fails,
            bytes: saved.bytes,
            ..Self::new()
        }
    }

    fn saved(&self) -> store::SavedHost {
        store::SavedHost {
            speed: self.ema.round() as u64,
            measured: self.measured,
            normal: self.normal.round() as u64,
            redial_useless_at: if self.redial_useless {
                self.redial_useless_at
            } else {
                0
            },
            ok: self.ok,
            fails: self.fails,
            bytes: self.bytes,
        }
    }
}

struct State {
    items: IndexMap<String, Item>,
    hosts: IndexMap<String, HostStats>,
    /// Each host from each exit it was reached from.
    lanes: HashMap<(String, Exit), Lane>,
    routes: BTreeMap<String, Route>,
    paused: bool,
    /// The download folder is changing: start nothing new.
    moving: bool,
    /// The router is redialing: start nothing new.
    reconnecting: bool,
    /// A redial on its own waits for the downloads running well on the line:
    /// start nothing new meanwhile. Why it would help.
    draining: Option<String>,
    max_total: usize,
    dirty: bool,
    /// A lookup learned a size: archives copied on other hosts are looked
    /// for on the scheduler's next turn ([`Engine::merge_copies`]).
    merge_due: bool,
    /// Pages that answered with a browser check (Cloudflare's) a browser
    /// usually passes by itself: the desktop app passes them out of sight,
    /// as a host's whose page passes ([`hosts::page_passes`]).
    guarded: HashSet<String>,
    /// Guarded pages whose check a browser passed and handed Haul the
    /// clearance for ([`Engine::page_cleared`]): one still turned away after
    /// it is passed the usual way, so a clearance that does not hold is not
    /// handed over and over.
    cleared: HashSet<String>,
    /// Pages the desktop app passed during their rows' current waits.
    tried: passes::Tried,
    /// Getting a new address from the router.
    reconnects: reconnect::Reconnects,
    /// VPN services whose servers join the proxies.
    services: services::Services,
}

impl State {
    /// Whether the desktop app passes page `url` by itself.
    fn passes(&self, url: &str) -> bool {
        hosts::page_passes(url) || self.guarded.contains(url)
    }

    /// A resolver said page `url` needs `need`: it goes to the desktop app's
    /// browser when a browser usually passes it. A gate past the site's check
    /// takes no clearance: Haul would only read it again.
    fn note_check(&mut self, url: &str, need: Need, tentative: bool) {
        // A page on a site Haul has no code for is passed too, for its
        // download, but only one the person added: a page only seen on the
        // clipboard is never opened on its own.
        if matches!(need, Need::Check | Need::Gate) || (!tentative && need == Need::Page) {
            self.guarded.insert(url.to_string());
        }
        if need == Need::Gate {
            self.cleared.insert(url.to_string());
        }
    }

    /// A host's stats; a known host's are found without allocating.
    fn host(&mut self, name: &str) -> &mut HostStats {
        if !self.hosts.contains_key(name) {
            self.hosts.insert(name.to_string(), HostStats::new());
        }
        self.hosts.get_mut(name).expect("just put there")
    }

    fn lane(&mut self, host: &str, exit: Exit) -> &mut Lane {
        self.lanes.entry((host.to_string(), exit)).or_default()
    }

    /// The lanes of `host`, from whichever exits it was reached from.
    fn lanes_of<'a>(&'a self, host: &'a str) -> impl Iterator<Item = &'a Lane> + 'a {
        self.lanes
            .iter()
            .filter(move |((h, _), _)| h == host)
            .map(|(_, lane)| lane)
    }

    /// The exit among `exits` with a free slot for `host` and the fewest of
    /// its downloads running, so parallel downloads spread over addresses.
    fn open_exit(
        &mut self,
        host: &str,
        exits: &[Exit],
        cfg: &Config,
        now: Instant,
    ) -> Option<Exit> {
        self.best_exit(host, exits, cfg, now, false)
    }

    /// As [`State::open_exit`], for a row that has nowhere else to go than a
    /// host that is slowing the address.
    fn exit_while_slowing(
        &mut self,
        host: &str,
        exits: &[Exit],
        cfg: &Config,
        now: Instant,
    ) -> Option<Exit> {
        self.best_exit(host, exits, cfg, now, true)
    }

    fn best_exit(
        &mut self,
        host: &str,
        exits: &[Exit],
        cfg: &Config,
        now: Instant,
        slowing_too: bool,
    ) -> Option<Exit> {
        // What a free user gets, where it is known, holds from the start.
        let limit = hosts::known(host)
            .and_then(|k| k.at_once(cfg))
            .map_or(AT_ONCE, |at_once| at_once.min(AT_ONCE));
        // An address the host has slowed for the day comes last.
        let mut best: Option<(Exit, (bool, usize))> = None;
        for &exit in exits {
            let lane = self.lane(host, exit);
            lane.relax(limit, now);
            let rank = (lane.slowed(), lane.active);
            if lane.free(limit, now, slowing_too) && best.is_none_or(|(_, r)| rank < r) {
                best = Some((exit, rank));
            }
        }
        best.map(|(exit, _)| exit)
    }

    /// Whether every exit of `host` is cooling down.
    fn cooling(&self, host: &str, exits: &[Exit], now: Instant) -> bool {
        exits.iter().all(|e| {
            self.lanes
                .get(&(host.to_string(), *e))
                .is_some_and(|lane| lane.cooling(now))
        })
    }
}

/// A row comparing a file on disk with its host's checksum. Dropped when the
/// comparison ends or its attempt is cancelled: clears the row's stage and
/// tells the read, which runs on another thread, to stop.
struct Comparing {
    engine: Engine,
    id: String,
    gone: Arc<AtomicBool>,
}

impl Drop for Comparing {
    fn drop(&mut self) {
        self.gone.store(true, Ordering::Relaxed);
        self.engine.set_verifying(&self.id, Verifying::No);
    }
}

/// An exit as the log names it: `direct`, or `proxy 2`.
fn exit_name(exit: Exit) -> String {
    if exit == 0 {
        "direct".into()
    } else {
        format!("proxy {exit}")
    }
}

#[derive(Clone)]
pub struct Engine(Arc<Inner>);

/// HTTP clients for every exit, rebuilt when the proxies or the user agent
/// change: `exits[0]` is the direct connection, `exits[n]` the n-th proxy.
struct Clients {
    exits: Vec<Http>,
}

impl Clients {
    /// Exits for the proxies of the settings, then for `services`' (a VPN's
    /// servers), unless proxies are switched off.
    fn new(cfg: &Config, services: &[String]) -> Result<Self, String> {
        let mut exits = vec![Http::new(None, crate::config::USER_AGENT)?];
        let services = services
            .iter()
            .map(String::as_str)
            .filter(|_| cfg.use_proxy);
        for proxy in cfg.proxy_pool().into_iter().chain(services) {
            exits.push(Http::with(
                Some(proxy),
                crate::config::USER_AGENT,
                cfg.proxy_fresh_connections,
            )?);
        }
        Ok(Self { exits })
    }

    fn on(&self, exit: Exit) -> Http {
        self.exits.get(exit).unwrap_or(&self.exits[0]).clone()
    }
}

struct Inner {
    state: Mutex<State>,
    /// The settings in effect, replaced whole when they change.
    cfg: RwLock<Arc<Config>>,
    /// `config.toml`, which settings changed in the app are written back to.
    config_path: Option<PathBuf>,
    /// The file's modification time and length when last read or written.
    config_seen: Mutex<Option<(SystemTime, u64)>>,
    /// Why the file on disk is not in effect, when it does not parse.
    config_error: Mutex<Option<String>>,
    /// Moves whenever the settings in effect change ([`Engine::watch_settings`]).
    config_version: watch::Sender<u64>,
    data_dir: PathBuf,
    download_dir: RwLock<PathBuf>,
    /// Given on the command line, so the settings do not move it.
    download_dir_fixed: bool,
    recycle_bin: Option<PathBuf>,
    clients: RwLock<Arc<Clients>>,
    tokens: Tokens,
    limiter: Limiter,
    wake: Notify,
    version: watch::Sender<u64>,
    events: broadcast::Sender<Event>,
    probes: Semaphore,
    /// One comparison with a file on disk at a time: they read whole files,
    /// and several at once make a spinning disk seek between them.
    verifier: Semaphore,
    /// One archive unpacked at a time.
    extractor: Semaphore,
    /// How far the archive being unpacked is.
    unpacking: Mutex<Option<Arc<extract::Progress>>>,
    /// `State::paused`, for comparisons running outside the lock.
    paused_now: Arc<AtomicBool>,
    /// The desktop app passes hosts' pages out of sight ([`Engine::set_pages_pass`]).
    pages_pass: AtomicBool,
    /// The runtime the engine started in, for tasks started from outside
    /// it (the desktop app's browser threads).
    runtime: tokio::runtime::Handle,
    save_lock: Mutex<()>,
    rename_lock: Mutex<()>,
    /// Held while settings change ([`Engine::settings_held`]).
    settings_lock: Mutex<()>,
    shutdown: CancellationToken,
    /// A newer Haul, when GitHub has one.
    update: Mutex<Option<crate::update::Update>>,
    _lock: File,
}

impl Engine {
    /// Locks the data directory, loads the queue and starts scheduling. Must
    /// run inside a Tokio runtime.
    pub async fn start(opts: Options) -> Result<Self, StartError> {
        let data_dir = opts.data_dir;
        std::fs::create_dir_all(&data_dir).map_err(|source| StartError::Io {
            path: data_dir.clone(),
            source,
        })?;
        let lock = store::lock(&data_dir)?;
        let (cfg, config_path) = match opts.config {
            Some(cfg) => (cfg, None),
            None => {
                let path = data_dir.join(config::FILE);
                let cfg = Config::load(&path).map_err(|message| StartError::Config {
                    path: path.clone(),
                    message,
                })?;
                (cfg, Some(path))
            }
        };
        if cfg.detailed_log {
            crate::logging::set_detailed(true);
        }
        if cfg.live_debug {
            crate::debug::set_live(true);
        }
        let download_dir_fixed = opts.download_dir.is_some();
        let download_dir = opts
            .download_dir
            .or_else(|| cfg.download_dir.clone())
            .unwrap_or_else(paths::download_dir);
        let clients = Clients::new(&cfg, &[]).map_err(StartError::Proxy)?;

        let mut saved = store::load_queue(&data_dir)?;
        // A row with no link to fetch it from (a hand-edited queue, say) could
        // only sit there.
        saved.retain(|id, s| {
            let usable = !s.mirrors.is_empty();
            if !usable {
                tracing::warn!(item = id, "dropping a saved download without links");
            }
            usable
        });
        let now = unix_now();
        let hosts: IndexMap<String, HostStats> = store::load_hosts(&data_dir)
            .iter()
            .map(|(name, saved)| {
                let stats = HostStats::restore(saved, now);
                (name.clone(), stats)
            })
            .collect();
        let routes = cfg.routes.clone();
        let mut items: IndexMap<String, Item> = saved
            .into_iter()
            .map(|(id, it)| (id.clone(), it.restored(id)))
            .collect();
        // Dropping a host from a wrapper's card can take its last file and
        // leave the wrapper on the list until now.
        drop_spent_wrappers(&mut items);
        // What each unfinished row holds on disk, which only a running
        // transfer counts: without it a paused row with bytes would read as
        // never started. A partial file no row claims can never be resumed.
        let mut on_disk = partial_sizes(&download_dir);
        for it in items.values_mut().filter(|it| it.status != Status::Done) {
            if let Some(len) = on_disk.remove(&it.id) {
                // Never more than the file's size: a host may have said a
                // smaller one since.
                it.done = if it.size > 0 { len.min(it.size) } else { len };
            }
        }
        if owns_partials(&download_dir, &data_dir) {
            for (id, len) in on_disk {
                tracing::info!(
                    "deleting partial file {id}.part ({len} bytes): no download owns it"
                );
                let _ =
                    std::fs::remove_file(download_dir.join(".partial").join(format!("{id}.part")));
            }
        } else if !on_disk.is_empty() {
            tracing::info!(
                "{} partial files belong to no download here; another Haul's data folder owns the folder, so they stay",
                on_disk.len()
            );
        }
        let unprobed: Vec<String> = items
            .values()
            .filter(|i| !i.probed && i.status == Status::Queued)
            .map(|i| i.id.clone())
            .collect();

        let paused = store::load_paused(&data_dir);
        let engine = Engine(Arc::new(Inner {
            state: Mutex::new(State {
                items,
                hosts,
                lanes: HashMap::new(),
                routes,
                paused,
                moving: false,
                reconnecting: false,
                draining: None,
                max_total: cfg.max_total.max(1),
                dirty: false,
                merge_due: false,
                guarded: HashSet::new(),
                cleared: HashSet::new(),
                tried: passes::Tried::new(),
                reconnects: reconnect::Reconnects::default(),
                services: services::Services::default(),
            }),
            limiter: Limiter::new(cfg.rate_limit_mbps * 1e6),
            config_seen: Mutex::new(config_path.as_deref().and_then(settings::stamp)),
            cfg: RwLock::new(Arc::new(cfg)),
            config_path,
            config_error: Mutex::new(None),
            config_version: watch::Sender::new(0),
            data_dir,
            download_dir: RwLock::new(download_dir),
            download_dir_fixed,
            recycle_bin: opts.recycle_bin,
            clients: RwLock::new(Arc::new(clients)),
            tokens: Tokens::default(),
            wake: Notify::new(),
            version: watch::Sender::new(0),
            events: broadcast::Sender::new(64),
            probes: Semaphore::new(4),
            verifier: Semaphore::new(1),
            extractor: Semaphore::new(1),
            unpacking: Mutex::new(None),
            paused_now: Arc::new(AtomicBool::new(paused)),
            pages_pass: AtomicBool::new(false),
            runtime: tokio::runtime::Handle::current(),
            save_lock: Mutex::new(()),
            rename_lock: Mutex::new(()),
            settings_lock: Mutex::new(()),
            shutdown: CancellationToken::new(),
            update: Mutex::new(None),
            _lock: lock,
        }));
        {
            // Failed rows from the last run get their automatic retries again.
            let mut st = engine.lock();
            let now = Instant::now();
            for it in st.items.values_mut() {
                if it.status == Status::Error {
                    engine.plan_auto_retry(it, now);
                }
            }
        }
        for id in unprobed {
            tokio::spawn(engine.clone().probe(id));
        }
        tokio::spawn(engine.clone().scheduler());
        tokio::spawn(engine.clone().watch_quotas());
        tokio::spawn(engine.clone().watch_reconnects());
        tokio::spawn(engine.clone().watch_speeds());
        tokio::spawn(engine.clone().watch_services());
        tokio::spawn(engine.clone().saver());
        tokio::spawn(engine.clone().watch_config());
        tokio::spawn(engine.clone().watch_updates());
        engine.resume_extractions();
        Ok(engine)
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.0.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Marks the queue for saving and tells watchers it changed.
    fn touch(&self, st: &mut State) {
        st.dirty = true;
        self.0.version.send_modify(|v| *v += 1);
    }

    /// Does what [`Engine::touch`] does, then lets go of the lock and wakes
    /// the scheduler.
    fn publish(&self, mut st: MutexGuard<'_, State>) {
        self.touch(&mut st);
        drop(st);
        self.0.wake.notify_one();
    }

    /// Waits for `time`; false, at once, when Haul is stopping.
    async fn idle(&self, time: Duration) -> bool {
        tokio::select! {
            () = self.0.shutdown.cancelled() => false,
            () = tokio::time::sleep(time) => true,
        }
    }

    /// The settings in effect.
    pub fn cfg(&self) -> Arc<Config> {
        self.0.cfg.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn clients(&self) -> Arc<Clients> {
        self.0
            .clients
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// The exits `host` may be reached from: every proxy of the pool (and
    /// this machine's connection too when the user added it), or only the
    /// direct connection when there is no proxy or the user let the host go
    /// direct.
    fn exits_for(&self, host: &str, st: &State) -> Vec<Exit> {
        let cfg = self.cfg();
        let proxies = self.clients().exits.len() - 1;
        if proxies == 0 || st.routes.get(host) == Some(&Route::Direct) {
            return vec![0];
        }
        let mut exits: Vec<Exit> = (1..=proxies).collect();
        if cfg.proxy_with_direct {
            exits.push(0);
        }
        exits
    }

    /// Whether every exit `host` may be reached from is cooling down.
    fn host_cooling(&self, st: &State, host: &str, now: Instant) -> bool {
        st.cooling(host, &self.exits_for(host, st), now)
    }

    /// The exit with a free slot for `host`, of those it may be reached from
    /// ([`State::open_exit`]).
    fn free_exit(&self, st: &mut State, host: &str, cfg: &Config, now: Instant) -> Option<Exit> {
        let exits = self.exits_for(host, st);
        st.open_exit(host, &exits, cfg, now)
    }

    /// What a resolver may use, through `http`.
    fn cx<'a>(&'a self, http: &'a Http, cfg: &'a Config, key_service: &'a Http) -> Cx<'a> {
        Cx {
            http,
            cfg,
            tokens: &self.0.tokens,
            key_service,
        }
    }

    /// The client for lookups on `host` (a probe, a wrapper's solve): its
    /// first exit.
    fn http_for(&self, host: &str, st: &State) -> Http {
        self.clients().on(self.exits_for(host, st)[0])
    }

    /// The route to JDownloader's key service, which decrypts `.dlc` files.
    fn key_service(&self, st: &State) -> Http {
        self.http_for(&hosts::host_of(&self.cfg().endpoints.dlc_service), st)
    }

    pub fn data_dir(&self) -> &Path {
        &self.0.data_dir
    }

    pub fn download_dir(&self) -> PathBuf {
        self.0
            .download_dir
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// The configured proxy.
    pub fn proxy(&self) -> Option<String> {
        let cfg = self.cfg();
        cfg.proxy().map(str::to_string).or_else(|| {
            self.service_proxies()
                .into_iter()
                .next()
                .filter(|_| cfg.use_proxy)
        })
    }

    /// The proxy a page of `host` opens through in the in-app browser: the
    /// first of the pool, unless the user approved a direct route for it.
    pub fn proxy_for(&self, host: &str) -> Option<String> {
        let direct = self.lock().routes.get(host) == Some(&Route::Direct);
        if direct { None } else { self.proxy() }
    }

    /// Whether downloads are paused.
    pub fn paused(&self) -> bool {
        self.lock().paused
    }

    /// What is downloading, for a glance (the tray's tooltip) without a
    /// whole [`Snapshot`].
    pub fn activity(&self) -> Activity {
        let st = self.lock();
        let running = st
            .items
            .values()
            .filter(|i| i.status == Status::Downloading);
        Activity {
            downloading: running.clone().count(),
            speed: running.map(|i| i.speed).sum(),
            paused: st.paused,
        }
    }

    /// A counter that moves whenever the queue changes.
    pub fn watch(&self) -> watch::Receiver<u64> {
        self.0.version.subscribe()
    }

    /// Moves whenever the settings in effect change, and only then.
    pub fn watch_settings(&self) -> watch::Receiver<u64> {
        self.0.config_version.subscribe()
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.0.events.subscribe()
    }

    // ---- queue

    /// Adds every link found in `text`.
    pub fn add_text(&self, text: &str, package: &str) -> Added {
        self.add_text_with(text, package, None)
    }

    /// Adds every link found in `text`, its archives opened with `password`
    /// first, and the files of its wrappers too.
    pub fn add_text_with(&self, text: &str, package: &str, password: Option<&str>) -> Added {
        let links = container::extract_urls(text)
            .into_iter()
            .map(|url| Link {
                url,
                name: None,
                package: package.to_string(),
            })
            .collect();
        self.add(links, false, password.filter(|p| !p.is_empty()))
    }

    /// Adds the links in copied text that point at a known host or a file.
    /// A link to any other web page (not a site's front page) is looked at
    /// for links, as a post or a paste lists them, and kept only
    /// if it has some: an ordinary page goes again quietly.
    pub fn add_from_clipboard(&self, text: &str) -> Added {
        let link = |url| Link {
            url,
            name: None,
            package: String::new(),
        };
        let (downloads, pages): (Vec<String>, Vec<String>) = container::extract_urls(text)
            .into_iter()
            .filter(|url| hosts::is_download_link(url) || hosts::is_copied_page(url))
            .partition(|url| hosts::is_download_link(url));
        let mut added = Added::default();
        if !downloads.is_empty() {
            added = self.add(downloads.into_iter().map(link).collect(), false, None);
        }
        if !pages.is_empty() {
            added.pages = self
                .add(pages.into_iter().map(link).collect(), true, None)
                .added;
        }
        added
    }

    pub fn add_links(&self, links: Vec<Link>) -> Added {
        self.add(links, false, None)
    }

    /// Adds `links`; `tentative`: copied pages, kept only if they list
    /// links ([`Item::tentative`]). `password`: the one the person gave with
    /// them, tried first on their archives.
    fn add(&self, links: Vec<Link>, tentative: bool, password: Option<&str>) -> Added {
        let mut added = Added::default();
        let mut probe = Vec::new();
        {
            let mut st = self.lock();
            let mut known: HashSet<String> = st
                .items
                .values()
                .flat_map(|i| i.mirrors.iter().map(|m| m.url.clone()))
                .collect();
            let mut wrappers = wrapper_keys(&st);
            for link in links {
                if !hosts::is_http(&link.url) {
                    added.invalid += 1;
                    continue;
                }
                if !known.insert(link.url.clone()) {
                    added.duplicates += 1;
                    continue;
                }
                if hosts::lists(&link.url) {
                    // One wrapper whichever of its links came: every link of a
                    // fileguard.cc vault shows the same page.
                    if !wrappers.insert(hosts::wrapper_key(&link.url)) {
                        added.duplicates += 1;
                        continue;
                    }
                    let mut it = Item::new(vec![link.url], None, link.package);
                    it.wrapper = true;
                    it.extract_password = password.map(str::to_string);
                    // Unwrapped right away, as a name is looked up.
                    probe.push(it.id.clone());
                    tracing::debug!(item = %it.id, url = %it.mirrors[0], "added as a wrapper");
                    st.items.insert(it.id.clone(), it);
                    added.wrappers += 1;
                    continue;
                }
                let name = hosts::name_for(&link.url, link.name);
                // The same file name on another host is another mirror; a
                // copied page is not judged by its address, so it joins none.
                if let Some(name) = &name
                    && !tentative
                    && let Some(it) = st
                        .items
                        .values_mut()
                        .find(|it| it.status != Status::Done && it.same_file(name, 0))
                {
                    tracing::debug!(item = %it.id, url = %link.url, "another host for {name}");
                    it.mirrors.push(link.url.into());
                    if let Some(password) = password {
                        it.extract_password = Some(password.to_string());
                    }
                    added.merged += 1;
                    continue;
                }
                // A web page is read for links whatever its address reads
                // like (`Example.Set.v1.25.html`); the name stays, for
                // merging. A copied one becomes a wrapper, stays a file, or
                // goes.
                let page = hosts::is_web_page(&link.url) && !hosts::is_download_link(&link.url);
                let mut it = Item::new(vec![link.url], name, link.package);
                it.tentative = tentative;
                it.extract_password = password.map(str::to_string);
                if tentative || page {
                    it.probed = false;
                } else if it.probed && hosts::tells_size(&it.mirrors[0]) {
                    // Looked up for its size; the name it came with stays.
                    it.probed = false;
                    it.named = true;
                }
                if !it.probed {
                    probe.push(it.id.clone());
                }
                tracing::debug!(item = %it.id, url = %it.mirrors[0], "added");
                st.items.insert(it.id.clone(), it);
                added.added += 1;
            }
            self.touch(&mut st);
        }
        for id in probe {
            self.spawn_probe(id);
        }
        self.0.wake.notify_one();
        added
    }

    /// Adds the links in a `.dlc` or text container.
    pub async fn import(&self, filename: &str, bytes: &[u8]) -> Result<Added, String> {
        let links = self.read_container(filename, bytes).await?;
        Ok(self.add_links(links))
    }

    /// The links in a `.dlc` or text container, without adding them.
    pub async fn read_container(
        &self,
        filename: &str,
        bytes: &[u8],
    ) -> Result<Vec<container::Link>, String> {
        let http = self.key_service(&self.lock());
        container::read(
            &http.client,
            &self.cfg().endpoints.dlc_service,
            filename,
            bytes,
        )
        .await
    }

    /// Learns the name and size up front, so the same file on several hosts
    /// is merged before anything downloads.
    async fn probe(self, id: String) {
        if self.lock().items.get(&id).is_some_and(|it| it.wrapper) {
            return self.unwrap_now(id).await;
        }
        let Ok(permit) = self.0.probes.acquire().await else {
            return;
        };
        let (url, http, key_service) = {
            let st = self.lock();
            let Some(it) = st.items.get(&id) else { return };
            // A host the user turned off is not asked either; with only such
            // hosts, nothing is.
            let cfg = self.cfg();
            let url = it
                .mirrors
                .iter()
                .find(|m| !cfg.is_disabled(&m.host))
                .cloned();
            let http = self.http_for(&hosts::host_of(url.as_deref().unwrap_or_default()), &st);
            let http = match url.as_deref() {
                Some(u) => http.for_page(u),
                None => http,
            };
            (url, http, self.key_service(&st))
        };
        let cfg = self.cfg();
        let cx = self.cx(&http, &cfg, &key_service);
        let looked = match &url {
            Some(url) => tokio::time::timeout(Duration::from_secs(25), hosts::look(&cx, url))
                .await
                .unwrap_or(hosts::Look::Unknown),
            None => hosts::Look::Unknown,
        };
        // The host deleted the file: that link is dead now, as it would be
        // found on its turn, and the row goes on to its other hosts.
        if let (hosts::Look::Gone(message), Some(url)) = (&looked, &url) {
            drop(permit);
            self.fail_unstarted(&id, url, message.clone(), true);
            self.0.wake.notify_one();
            return;
        }
        // A web page on a host Haul has no code for may list download
        // links (a blog's or forum's post): they stand in for it, as if the
        // page's text had been pasted, whatever file its title names.
        // Behind a check the desktop app passes, a page shows its links
        // only once passed.
        let (found, listed, guarded) = match looked {
            hosts::Look::Page {
                title,
                links,
                password,
                ..
            } if !links.is_empty() => (None, Some((title, links, password)), false),
            hosts::Look::Guarded => (None, None, self.pages_pass()),
            looked => (looked.found(), None, false),
        };
        // The slot is for asking hosts; comparing with a file on disk waits
        // for its own turn below, so other links keep being looked up.
        drop(permit);
        // A page that lists links is a wrapper of them: its links join the
        // queue under it, not started, as a wrapper's files do.
        if let (Some((title, links, password)), Some(url)) = (listed, &url) {
            tracing::info!(item = %id, "{url} links {} files and wrappers", links.len());
            let found = hosts::Unwrapped {
                password,
                ..hosts::listed(title, links)
            };
            self.take_unwrapped(&id, url, found);
            return;
        }
        if found.is_none() && !guarded && self.drop_tentative(&id, "lists no links") {
            return;
        }
        self.take_found(&id, found).await;
    }

    /// Takes what a lookup learned of row `id`'s file, if anything: its
    /// name, size and checksum, which may fold it into a row for the same
    /// file or find it downloaded already. The row is probed after it.
    async fn take_found(&self, id: &str, found: Option<hosts::Probed>) {
        let learned = found.is_some();
        {
            let mut st = self.lock();
            let Some(it) = st.items.get_mut(id) else {
                return;
            };
            if let Some(found) = found {
                // A copied link that turns out to be a file is a download.
                it.tentative = false;
                it.learn(Some(&found.name), true, found.size, found.checksum);
                if self.dedupe(&mut st, id) {
                    st.items.shift_remove(id);
                    self.publish(st);
                    self.discard_parts([id.to_string()]);
                    return;
                }
            }
        }
        // The row is not probed yet, so nothing starts it meanwhile.
        let existing = match learned {
            true => self.existing_file(id).await,
            false => None,
        };
        let mut st = self.lock();
        let Some(it) = st.items.get_mut(id) else {
            return;
        };
        it.probed = true;
        let adopted = match existing {
            Some(path) if !it.running() => {
                tracing::info!(item = id, "already downloaded: {}", path.display());
                it.adopt(&path);
                true
            }
            _ => false,
        };
        if adopted {
            self.extract_when_complete(&mut st, id);
        }
        // With its size known, this row may complete a copy of another
        // archive: looked for on the scheduler's next turn, once for a
        // wrapper's many lookups in a row.
        if learned {
            st.merge_due = true;
        }
        self.touch(&mut st);
        drop(st);
        if adopted {
            self.discard_parts([id.to_string()]);
        }
        self.0.wake.notify_one();
    }

    /// Looks a new row up in a task of its own: a wrapper is unwrapped, a
    /// file's name and size are probed.
    fn spawn_probe(&self, id: String) {
        tokio::spawn(self.clone().probe(id));
    }

    /// Unwraps a wrapper as soon as it is added, whatever the queue is
    /// doing, since its files only wait for the user. What this cannot
    /// settle (a page for a person, a refusal, the network) is left to the
    /// wrapper's turn in the queue, which retries, waits or asks as usual.
    async fn unwrap_now(self, id: String) {
        let (url, http, key_service) = {
            let mut st = self.lock();
            let Some(it) = st.items.get_mut(&id) else {
                return;
            };
            let url = it.mirrors[0].clone();
            // A site the user turned off is not asked: the queue's turn
            // says so, as for a download whose hosts are all off.
            if self.cfg().is_disabled(&hosts::host_of(&url)) {
                it.probed = true;
                self.publish(st);
                return;
            }
            let http = self.http_for(&hosts::host_of(&url), &st).for_page(&url);
            (url, http, self.key_service(&st))
        };
        let cfg = self.cfg();
        let cx = self.cx(&http, &cfg, &key_service);
        let found = tokio::time::timeout(Duration::from_secs(60), hosts::unwrap(&cx, &url)).await;
        match found {
            Ok(Ok(found)) => self.take_unwrapped(&id, &url, found),
            // A gate for a person (filecrypt.cc's) says so at once, even
            // while downloads are paused.
            Ok(Err(ResolveError::NeedsUser(need, message))) => {
                let mut st = self.lock();
                let tentative = st.items.get(&id).is_some_and(|i| i.tentative);
                st.note_check(&url, need, tentative);
                let mut event = None;
                if let Some(it) = st.items.get_mut(&id)
                    && !it.running()
                {
                    it.probed = true;
                    it.manual_urls.insert(url.url.clone());
                    it.set_error(message, Trouble::Needs(need));
                    it.status = Status::Captcha;
                    event = Some(Event::NeedsUser {
                        id: id.clone(),
                        name: display_name(it),
                    });
                }
                self.touch(&mut st);
                drop(st);
                self.emit(event);
            }
            // So does a link that is dead (a vault gone, 404), failing as a
            // download's dead mirror does: retried later on its own if set,
            // unless the host said it is no more.
            Ok(Err(ResolveError::Failed(message))) => {
                self.fail_unstarted(&id, &url, message, false)
            }
            Ok(Err(ResolveError::Gone(message))) => self.fail_unstarted(&id, &url, message, true),
            _ => {
                let mut st = self.lock();
                if let Some(it) = st.items.get_mut(&id) {
                    it.probed = true;
                }
                self.touch(&mut st);
            }
        }
        self.0.wake.notify_one();
    }

    /// Fails mirror `url` of row `id` from outside an attempt (a lookup, an
    /// unwrapping as it is added), unless an attempt has started meanwhile.
    fn fail_unstarted(&self, id: &str, url: &str, message: String, gone: bool) {
        let mut st = self.lock();
        let mut event = None;
        if let Some(it) = st.items.get_mut(id)
            && !it.running()
        {
            it.probed = true;
            event = self.fail_mirror(&mut st, id, url, &hosts::host_of(url), message, gone);
        }
        self.touch(&mut st);
        drop(st);
        self.emit(event);
    }

    /// Tells whoever listens, when there is something to tell.
    fn emit(&self, event: impl IntoIterator<Item = Event>) {
        for e in event {
            let _ = self.0.events.send(e);
        }
    }

    /// A file already in the download folder that row `id` would fetch again,
    /// where one of its hosts would put it: the same name and, when the host
    /// reported it, the same size; when `verify_existing` is on and the host
    /// published a checksum, also the same content. Needs the name.
    async fn existing_file(&self, id: &str) -> Option<PathBuf> {
        let (name, size, checksum, package, hosts) = {
            let st = self.lock();
            let it = st.items.get(id)?;
            let name = it.name.clone()?;
            let hosts: Vec<String> = it.mirrors.iter().map(|m| m.host.to_string()).collect();
            (
                name,
                it.size,
                it.checksum.clone(),
                it.package.clone(),
                hosts,
            )
        };
        let file = paths::safe(&name);
        let mut candidates: Vec<PathBuf> = Vec::new();
        for host in hosts {
            let path = self.out_dir(&host, &package).join(&file);
            if !candidates.contains(&path) {
                candidates.push(path);
            }
        }
        // The same name and size, from metadata alone; the name alone when the
        // host did not say how big the file is.
        let (same_size, other_sizes) = tokio::task::spawn_blocking(move || {
            let mut same = None;
            let mut others = Vec::new();
            for path in candidates {
                match std::fs::metadata(&path) {
                    Ok(m) if m.is_file() && (size == 0 || m.len() == size) && same.is_none() => {
                        same = Some(path)
                    }
                    Ok(m) if m.is_file() => others.push((path, m.len())),
                    _ => {}
                }
            }
            (same, others)
        })
        .await
        .ok()?;
        let Some(path) = same_size else {
            for (path, len) in other_sizes {
                tracing::info!(
                    item = id,
                    "{} has {len} bytes, not {size}: another file",
                    path.display()
                );
            }
            return None;
        };
        // Name and size decide, unless the user asked for checksums too.
        let Some(checksum) = checksum.filter(|_| size > 0 && self.cfg().verify_existing) else {
            return Some(path);
        };
        // A checksum means reading the whole file: one at a time, and not
        // while downloads are paused. Dropping this future (a cancelled
        // attempt) stops the read too.
        self.set_verifying(id, Verifying::Waiting);
        let comparing = Comparing {
            engine: self.clone(),
            id: id.to_owned(),
            gone: Arc::new(AtomicBool::new(false)),
        };
        let Ok(_turn) = self.0.verifier.acquire().await else {
            return None;
        };
        self.set_verifying(id, Verifying::Comparing);
        let gone = comparing.gone.clone();
        let paused = self.0.paused_now.clone();
        let target = path.clone();
        let same = tokio::task::spawn_blocking(move || {
            checksum.matches_steered(&target, &|| {
                if gone.load(Ordering::Relaxed) {
                    hosts::Steer::Stop
                } else if paused.load(Ordering::Relaxed) {
                    hosts::Steer::Wait
                } else {
                    hosts::Steer::Go
                }
            })
        })
        .await;
        drop(comparing);
        match same {
            Ok(Ok(true)) => Some(path),
            Ok(Ok(false)) => {
                tracing::info!(
                    item = id,
                    "{} differs from the host's checksum: another file",
                    path.display()
                );
                None
            }
            Ok(Err(e)) => {
                tracing::warn!(item = id, "reading {}: {e}", path.display());
                None
            }
            Err(_) => None,
        }
    }

    fn set_verifying(&self, id: &str, stage: Verifying) {
        let mut st = self.lock();
        if let Some(it) = st.items.get_mut(id)
            && it.verifying != stage
        {
            it.verifying = stage;
            self.touch(&mut st);
        }
    }

    /// Replaces a folder row with one row per file, in the same place, each
    /// with its entry resolved from the folder's link.
    fn expand(&self, id: &str, url: &str, entries: Vec<Entry>) {
        let mut st = self.lock();
        let Some(mut at) = st.items.get_index_of(id) else {
            return;
        };
        let package = st.items[at].package.clone();
        let password = st.items[at].extract_password.clone();
        st.items.shift_remove_index(at);
        for e in entries {
            let mut it = Item::new(vec![url.to_string()], e.name.clone(), package.clone());
            it.extract_password.clone_from(&password);
            it.size = e.size.unwrap_or(0);
            it.probed = true;
            it.pre = Some(e);
            it.pre_for = Some(url.to_string());
            if let Some((_, i)) = self.insert_row(&mut st, at, it) {
                at = i + 1;
            }
        }
        self.touch(&mut st);
    }

    /// Adds a wrapper's files to the queue after its row, not started, and
    /// marks the row done with what it found. A link the queue already holds
    /// stays where it is, as a pasted one does, so a mirror two wrappers share
    /// downloads nothing twice; another wrapper listed on the page gets a
    /// wrapper row of its own. Returns the new rows to probe.
    fn unwrapped(&self, id: &str, url: &str, found: hosts::Unwrapped) -> Vec<String> {
        let mut st = self.lock();
        let mut probe = Vec::new();
        let Some(row) = st.items.get(id) else {
            return probe;
        };
        let package = row.package.clone();
        // The one given with the link, else the one the page gives.
        let password = row
            .extract_password
            .clone()
            .or_else(|| found.password.clone());
        let mut known: HashSet<String> = st
            .items
            .iter()
            .filter(|(k, _)| k.as_str() != id)
            .flat_map(|(_, i)| i.mirrors.iter().map(|m| m.url.clone()))
            .collect();
        known.insert(url.to_string());
        let mut wrappers = wrapper_keys(&st);
        wrappers.insert(hosts::wrapper_key(url));
        let mut seen = Found::default();
        let mut found_hosts: Vec<String> = Vec::new();
        let mut after = id.to_string();
        let cfg = self.cfg();
        // An archive's parts a sibling wrapper brought already, each host
        // naming them its own way: its links join those rows by number, and
        // every part takes the name that says most of the ones the files
        // have, so they unpack as one archive. Never the wrappers' title,
        // which unrelated files can share.
        let by_part = sibling_parts(&st, id, found.title.as_deref(), &found.files);
        if let Some(rows) = &by_part {
            let names = rows
                .values()
                .filter_map(|row| st.items.get(row)?.key_name())
                .chain(
                    found
                        .files
                        .iter()
                        .filter(|e| !hosts::lists(&e.url))
                        .filter_map(|e| e.file_name().map(|n| hosts::tidy_name(&n))),
                );
            let base = best_archive_name(names);
            for row in rows.values() {
                if let Some(it) = st.items.get_mut(row) {
                    let own = it.key_name();
                    let shared = own
                        .zip(base.as_deref())
                        .and_then(|(n, b)| parts::rebased(&n, b));
                    if let Some(name) = shared {
                        it.name = Some(name);
                    } else if it.name.is_none() {
                        it.name = it.key_name();
                    }
                    it.named = true;
                }
            }
        }
        for e in found.files {
            let package = if package.is_empty() {
                e.package.clone().unwrap_or_default()
            } else {
                package.clone()
            };
            let nested = hosts::lists(&e.url);
            seen.links += 1 + e.mirrors.len();
            for u in std::iter::once(&e.url).chain(&e.mirrors) {
                let host = hosts::host_of(u);
                if !nested && !found_hosts.contains(&host) {
                    found_hosts.push(host);
                }
            }
            let part_row = by_part.as_ref().and_then(|rows| {
                let name = e.file_name()?;
                rows.get(&parts::PartName::parse(&name)?.number).cloned()
            });
            if let Some(row) = part_row.filter(|_| !nested) {
                seen.files += 1;
                let urls: Vec<String> = std::iter::once(e.url)
                    .chain(e.mirrors)
                    .filter(|u| known.insert(u.clone()))
                    .collect();
                if let Some(it) = st.items.get_mut(&row) {
                    it.mirrors.extend(urls.into_iter().map(Into::into));
                    it.merge_wrappers(Some(&id.to_string()), &[]);
                }
                continue;
            }
            if !nested {
                seen.files += 1;
                // The rows that hold its links already are this wrapper's
                // files too.
                let listed = |m: &Mirror| m.url == e.url || e.mirrors.contains(&m.url);
                for it in st
                    .items
                    .iter_mut()
                    .filter(|(k, i)| k.as_str() != id && !i.wrapper && i.mirrors.iter().any(listed))
                    .map(|(_, i)| i)
                {
                    it.merge_wrappers(Some(&id.to_string()), &[]);
                }
            }
            let mut urls: Vec<String> = std::iter::once(e.url)
                .chain(e.mirrors)
                .filter(|u| known.insert(u.clone()))
                .collect();
            // The hosts the user prefers first, in their order: the row shows
            // one of them, and a probe asks it for the name.
            urls.sort_by_key(|u| cfg.preference(&hosts::host_of(u)).unwrap_or(usize::MAX));
            if urls.is_empty() || nested && !wrappers.insert(hosts::wrapper_key(&urls[0])) {
                continue;
            }
            let mut it = if nested {
                let mut it = Item::new(urls, None, package);
                it.wrapper = true;
                it
            } else {
                let mut it = Item::new(urls, e.name, package);
                it.listed_size = e.listed_size;
                // Nothing a wrapper lists starts on its own: the user picks.
                it.status = Status::Paused;
                it.size = e.size.unwrap_or(0);
                // Its size from its host, when the wrapper gave none; the
                // name the wrapper gave stays.
                if it.probed && it.size == 0 && hosts::tells_size(&it.mirrors[0]) {
                    it.probed = false;
                    it.named = true;
                }
                it
            };
            it.source = Some(id.to_string());
            it.extract_password.clone_from(&password);
            let unprobed = !it.probed;
            let at = st
                .items
                .get_index_of(&after)
                .map_or(st.items.len(), |i| i + 1);
            if let Some((new_id, _)) = self.insert_row(&mut st, at, it) {
                after.clone_from(&new_id);
                if unprobed {
                    probe.push(new_id);
                }
            }
        }
        // A wrapper or a paste served its page: the Hosts view counts these as
        // it does a host's files.
        st.host(&hosts::host_of(url)).ok += 1;
        if let Some(row) = st.items.get_mut(id) {
            row.tentative = false;
            if !row.wrapper {
                // A row handed a wrapper's link is that wrapper now.
                row.wrapper = true;
                row.mirrors = vec![url.into()];
                row.name = None;
            }
            row.status = Status::Done;
            row.found = Some(seen);
            row.found_hosts = found_hosts;
            row.title = found.title;
            row.reset_retries();
            row.pre = None;
            row.pre_for = None;
            row.manual = false;
            row.manual_urls.clear();
            row.failed.clear();
            row.not_before = None;
        }
        self.touch(&mut st);
        probe
    }

    /// Puts new row `it` at `at` in the queue, unless it is a file the queue
    /// holds already ([`Engine::dedupe`]). Returns its id and place when it
    /// stays.
    fn insert_row(&self, st: &mut State, at: usize, it: Item) -> Option<(String, usize)> {
        let id = it.id.clone();
        st.items
            .shift_insert(at.min(st.items.len()), id.clone(), it);
        if self.dedupe(st, &id) {
            st.items.shift_remove(&id);
            return None;
        }
        let at = st.items.get_index_of(&id)?;
        Some((id, at))
    }

    /// Takes what unwrapping wrapper row `id` (from `url`) found, and looks
    /// its new rows up.
    fn take_unwrapped(&self, id: &str, url: &str, found: hosts::Unwrapped) {
        for id in self.unwrapped(id, url, found) {
            self.spawn_probe(id);
        }
    }

    /// Folds other rows for the same file into `id`. Returns true when `id`
    /// itself should go: the file is done already, or another row is fetching
    /// it or `id` waits for the user (the other row then gets `id`'s mirrors).
    /// A folded row's partial file carries over when both sizes are known and
    /// equal, and is deleted otherwise.
    fn dedupe(&self, st: &mut State, id: &str) -> bool {
        let Some(it) = st.items.get(id) else {
            return false;
        };
        let Some(name) = it.name.clone() else {
            return false;
        };
        let size = it.size;
        let others: Vec<String> = st
            .items
            .iter()
            .filter(|(k, o)| k.as_str() != id && o.same_file(&name, size))
            .map(|(k, _)| k.clone())
            .collect();
        // A row that waits for the user (a wrapper's file, not started) joins
        // the row already there instead of taking it over, which keeps going.
        let waits = st.items[id].status == Status::Paused && !st.items[id].running();
        for other in others {
            let o = &st.items[&other];
            if o.status == Status::Done {
                return true;
            }
            if o.running() || waits {
                let extra: Vec<Mirror> = st.items[id]
                    .mirrors
                    .iter()
                    .filter(|m| !st.items[&other].mirrors.contains(m))
                    .cloned()
                    .collect();
                let (source, also) = (st.items[id].source.clone(), st.items[id].also_from.clone());
                let o = st.items.get_mut(&other).expect("listed above");
                o.mirrors.extend(extra);
                o.merge_wrappers(source.as_ref(), &also);
                return true;
            }
            let o = st.items.shift_remove(&other).expect("listed above");
            let other_size = o.size;
            st.items.get_mut(id).expect("checked above").absorb(o);
            let (from, to) = (self.part_path(&other), self.part_path(id));
            if from.exists() {
                if size > 0 && other_size == size && !to.exists() {
                    let _ = std::fs::rename(&from, &to);
                } else {
                    let _ = std::fs::remove_file(&from);
                }
            }
        }
        false
    }

    /// `max_active_gb` in bytes; 0 when there is no such limit.
    fn size_cap(&self) -> u64 {
        (self.cfg().max_active_gb * 1e9) as u64
    }

    /// Takes rows `ids` off the list; their partial files are for the caller
    /// to discard once the lock is let go ([`Engine::discard_parts`]).
    fn remove_rows(&self, st: &mut State, ids: &[String]) {
        for id in ids {
            st.items.shift_remove(id);
        }
    }

    /// Deletes the partial files of rows that are gone for good.
    fn discard_parts(&self, ids: impl IntoIterator<Item = String>) {
        for id in ids {
            let _ = std::fs::remove_file(self.part_path(&id));
        }
    }

    fn part_path(&self, id: &str) -> PathBuf {
        self.download_dir()
            .join(".partial")
            .join(format!("{id}.part"))
    }

    // ---- controls

    /// Returns false when there is no such item. A finished download only
    /// goes away: there is nothing to pause, resume or retry.
    pub fn item_action(&self, id: &str, action: ItemAction) -> bool {
        if action == ItemAction::Extract {
            // Whether it could start is for [`Engine::extract`] to say.
            return self.extract(id, None).unwrap_or(true);
        }
        let mut st = self.lock();
        let Some(discard) = self.apply(&mut st, id, action) else {
            return false;
        };
        self.publish(st);
        self.discard_parts(discard);
        true
    }

    /// Does `action` to rows `ids` under one lock, with one change
    /// published. Answers how many rows there were, and why each of the
    /// others was left: no such row, or an archive that cannot be unpacked
    /// yet.
    pub fn items_action(&self, ids: &[String], action: ItemAction) -> Batch {
        let mut batch = Batch::default();
        if action == ItemAction::Extract {
            for id in ids {
                match self.extract(id, None) {
                    Ok(true) => batch.done += 1,
                    Ok(false) => batch.failed(id, "no such item"),
                    Err(e) => batch.failed(id, e),
                }
            }
            return batch;
        }
        let mut st = self.lock();
        let mut discard = Vec::new();
        for id in ids {
            match self.apply(&mut st, id, action) {
                Some(parts) => {
                    batch.done += 1;
                    discard.extend(parts);
                }
                None => batch.failed(id, "no such item"),
            }
        }
        self.publish(st);
        self.discard_parts(discard);
        batch
    }

    /// Does `action` (not [`ItemAction::Extract`]) to row `id`: `None` when
    /// there is no such row, else the partial files to discard once the
    /// lock is let go.
    fn apply(&self, st: &mut State, id: &str, action: ItemAction) -> Option<Vec<String>> {
        // Tried again by hand: a clearance may be handed over once more.
        if action == ItemAction::RetryManual
            && let Some(mirrors) = st.items.get(id).map(|it| it.mirrors.clone())
        {
            for m in &mirrors {
                st.cleared.remove(m.as_str());
            }
        }
        let it = st.items.get_mut(id)?;
        if it.status == Status::Done && action != ItemAction::Remove {
            return Some(Vec::new());
        }
        tracing::debug!(item = %it.id, "{action:?} by the user");
        match action {
            ItemAction::Pause => {
                it.status = Status::Paused;
                it.cancel();
            }
            ItemAction::Resume | ItemAction::Retry => {
                it.queue_if_idle();
                it.reset_retries();
                it.failed.clear();
                it.not_before = None;
            }
            ItemAction::RetryManual => {
                it.queue_if_idle();
                it.manual_urls.clear();
                it.page_wants_person = false;
                // The person's own try: the page's first pass again.
                it.auto_retries = 0;
                it.retry_at = None;
                it.page_retrying = false;
            }
            ItemAction::Extract => {}
            ItemAction::Remove => {
                let running = it.running();
                it.cancel();
                st.items.shift_remove(id);
                drop_spent_wrappers(&mut st.items);
                // A running one's transfer lets go of it as it stops.
                if !running {
                    return Some(vec![id.to_string()]);
                }
            }
        }
        Some(Vec::new())
    }

    /// Takes an item off the list, and its partial file with it. A finished
    /// file stays where it is unless `recycle_on_remove` is on: then it goes
    /// to the [`BIN`] first, and if that fails the item stays as well.
    /// Returns false when there is no such item.
    pub async fn remove(&self, id: &str) -> Result<bool, String> {
        let batch = self
            .remove_many(std::slice::from_ref(&id.to_string()))
            .await;
        match batch.failed.into_iter().next() {
            Some(f) if f.error == NO_SUCH => Ok(false),
            Some(f) => Err(f.error),
            None => Ok(true),
        }
    }

    /// Removes rows `ids` as [`Engine::remove`] does each, with one change
    /// published for those that go. A finished file that could not go to
    /// the bin keeps its row, and the batch says why.
    pub async fn remove_many(&self, ids: &[String]) -> Batch {
        let mut batch = Batch::default();
        let mut go = Vec::new();
        for id in ids {
            match self.recycle_finished(id).await {
                Ok(true) => go.push(id.clone()),
                Ok(false) => batch.failed(id, NO_SUCH),
                Err(e) => batch.failed(id, e),
            }
        }
        let more = self.items_action(&go, ItemAction::Remove);
        batch.done += more.done;
        batch.failed.extend(more.failed);
        batch
    }

    /// Moves row `id`'s finished file to the [`BIN`] when the settings say
    /// so. False when there is no such row.
    async fn recycle_finished(&self, id: &str) -> Result<bool, String> {
        let file = {
            let st = self.lock();
            let Some(it) = st.items.get(id) else {
                return Ok(false);
            };
            // A file the row found rather than downloaded stays where it is.
            (it.status == Status::Done && !it.existing && self.cfg().recycle_on_remove)
                .then(|| it.path.clone())
                .flatten()
        };
        if let Some(file) = file.map(PathBuf::from) {
            let bin = self.0.recycle_bin.clone();
            let shown = file.display().to_string();
            let recycled = tokio::task::spawn_blocking(move || recycle(&file, bin.as_deref()))
                .await
                .map_err(|e| e.to_string())?
                .map_err(|e| format!("could not move {shown} to the {BIN}: {e}"))?;
            if recycled {
                tracing::info!("moved {shown} to the {BIN}");
            }
        }
        Ok(true)
    }

    /// Has rows `ids` download from `host` while it has their file (`None`:
    /// back to the usual order). Returns how many rows took it.
    pub fn download_from(&self, ids: &[String], host: Option<&str>) -> usize {
        let mut st = self.lock();
        let mut n = 0;
        for id in ids {
            let Some(it) = st.items.get_mut(id) else {
                continue;
            };
            let has = host.is_none_or(|h| it.mirrors.iter().any(|m| *m.host == *h));
            if has && !it.wrapper {
                it.prefer = host.map(str::to_string);
                n += 1;
                let Some(h) = host else { continue };
                // A link another host's page handed back is not used before
                // the host picked: it would go on from that host again.
                let handed_for = it
                    .pre_for
                    .clone()
                    .or_else(|| it.pre.as_ref().map(|p| p.url.clone()));
                if it.manual && handed_for.is_some_and(|u| hosts::host_of(&u) != h) {
                    it.pre = None;
                    it.pre_for = None;
                    it.manual = false;
                }
                // Downloading from another host: it goes on from the one
                // picked now, keeping its bytes, as when it leaves a host
                // that slows it.
                if it.running() && !it.host.is_empty() && it.host != h {
                    tracing::info!(item = %id, "going on from {h}, as picked, keeping its {} bytes", it.done);
                    it.slowed = None;
                    it.pace = None;
                    it.host.clear();
                    it.cancel();
                }
            }
        }
        self.publish(st);
        n
    }

    /// Takes away item `id` if it is a copied page that did not turn out to
    /// list links (`why`), telling nobody. Returns whether it went.
    fn drop_tentative(&self, id: &str, why: &str) -> bool {
        let mut st = self.lock();
        if !drop_tentative_row(&mut st, id, why) {
            return false;
        }
        self.publish(st);
        true
    }

    /// Item `id`'s links, for tests of what they merge into.
    #[doc(hidden)]
    pub fn debug_links(&self, id: &str) -> Vec<String> {
        self.lock()
            .items
            .get(id)
            .map(|it| it.mirrors.iter().map(|m| m.url.clone()).collect())
            .unwrap_or_default()
    }

    /// Sets item `id`'s downloaded bytes, for tests of what goes first.
    #[doc(hidden)]
    pub fn debug_set_done(&self, id: &str, done: u64) {
        if let Some(it) = self.lock().items.get_mut(id) {
            it.done = done;
        }
    }

    /// Brings item `id`'s next automatic retry forward to now, for tests of
    /// what it does. Returns whether it had one.
    #[doc(hidden)]
    pub fn debug_retry_now(&self, id: &str) -> bool {
        let mut st = self.lock();
        let Some(it) = st.items.get_mut(id).filter(|it| it.retry_at.is_some()) else {
            return false;
        };
        it.retry_at = Some(Instant::now());
        self.publish(st);
        true
    }

    /// Stops starting downloads and pauses the running ones; they resume
    /// where they stopped.
    pub fn set_paused(&self, paused: bool) {
        self.0.paused_now.store(paused, Ordering::Relaxed);
        // Kept across restarts.
        if let Err(e) = store::save_paused(&self.0.data_dir, paused) {
            tracing::warn!("remembering the pause: {e}");
        }
        let mut st = self.lock();
        st.paused = paused;
        if paused {
            for it in st.items.values() {
                it.cancel();
            }
        }
        self.publish(st);
    }

    /// Tries every failed download again; wrappers have their own section.
    pub fn retry_failed(&self) {
        self.retry_where(|_| true);
    }

    /// Tries again the failed downloads (not wrappers) `which` picks.
    fn retry_where(&self, which: impl Fn(&Item) -> bool) {
        let ids: Vec<String> = self
            .lock()
            .items
            .values()
            .filter(|i| i.status == Status::Error && !i.wrapper && which(i))
            .map(|i| i.id.clone())
            .collect();
        if !ids.is_empty() {
            self.items_action(&ids, ItemAction::Retry);
        }
    }

    /// Starts again the downloads that failed for having only hosts the user
    /// turned off, now that `host` is back on and holds one of their links.
    pub(crate) fn retry_turned_on(&self, host: &str) {
        self.retry_where(|i| {
            i.trouble == Trouble::OnlyOffHosts
                && i.mirrors.iter().any(|m| m.host.eq_ignore_ascii_case(host))
        });
    }

    /// Clears finished downloads off the list, and an unwrapped wrapper with
    /// none of its files left: a wrapper still waiting for a person, failed,
    /// or with files to go stays in its section.
    pub fn clear_done(&self) {
        let mut st = self.lock();
        st.items
            .retain(|_, i| i.status != Status::Done || i.wrapper);
        drop_spent_wrappers(&mut st.items);
        self.touch(&mut st);
    }

    /// Starts the files a wrapper added that are waiting to be started.
    /// Returns how many, or `None` when there is no such wrapper.
    pub fn start_wrapper(&self, id: &str) -> Option<usize> {
        let mut st = self.lock();
        let started = Self::start_wrapper_in(&mut st, id)?;
        self.publish(st);
        Some(started)
    }

    fn start_wrapper_in(st: &mut State, id: &str) -> Option<usize> {
        if !st.items.get(id)?.wrapper {
            return None;
        }
        let mut started = 0;
        for it in st.items.values_mut() {
            if it.listed_by(id) && it.status == Status::Paused && !it.wrapper {
                it.status = Status::Queued;
                started += 1;
            }
        }
        Some(started)
    }

    /// Does `action` to wrappers `ids` (a card's, or every card's) under one
    /// lock, with one change published. A file two of them list is started
    /// once, and counted once in `started`.
    pub fn wrappers_action(&self, ids: &[String], action: WrapperAction) -> Batch {
        let mut batch = Batch::default();
        let mut discard = Vec::new();
        let mut st = self.lock();
        for id in ids {
            let done = match action {
                WrapperAction::Start => Self::start_wrapper_in(&mut st, id).map(|n| {
                    batch.started += n;
                }),
                WrapperAction::Remove => self.remove_wrapper_in(&mut st, id).map(|gone| {
                    discard.extend(gone);
                }),
                WrapperAction::Retry => match st.items.get(id) {
                    Some(w) if w.wrapper => self
                        .apply(&mut st, id, ItemAction::Retry)
                        .map(|parts| discard.extend(parts)),
                    _ => None,
                },
            };
            match done {
                Some(()) => batch.done += 1,
                None => batch.failed(id, "no such wrapper"),
            }
        }
        self.publish(st);
        self.discard_parts(discard);
        batch
    }

    /// Takes a wrapper off the list with the files it added that never
    /// started; those that did start stay. Returns false when there is no
    /// such wrapper.
    pub fn remove_wrapper(&self, id: &str) -> bool {
        let gone: Vec<String> = {
            let mut st = self.lock();
            let Some(gone) = self.remove_wrapper_in(&mut st, id) else {
                return false;
            };
            self.touch(&mut st);
            gone
        };
        self.discard_parts(gone);
        self.0.wake.notify_one();
        true
    }

    /// Takes wrapper `id` off the list as [`Engine::remove_wrapper`] says:
    /// `None` when there is no such wrapper, else the rows that went, whose
    /// partial files are for the caller to discard.
    fn remove_wrapper_in(&self, st: &mut State, id: &str) -> Option<Vec<String>> {
        {
            let w = st.items.get(id).filter(|w| w.wrapper)?;
            w.cancel();
            // A file another wrapper lists too stays, as that one's.
            for it in st
                .items
                .values_mut()
                .filter(|i| i.listed_by(id) && !i.wrapper)
            {
                it.also_from.retain(|w| w != id);
                if it.source.as_deref() == Some(id) && !it.also_from.is_empty() {
                    it.source = Some(it.also_from.remove(0));
                }
            }
            let gone: Vec<String> = st
                .items
                .values()
                .filter(|i| {
                    i.source.as_deref() == Some(id)
                        && !i.wrapper
                        && i.status == Status::Paused
                        && i.done == 0
                        && !i.running()
                })
                .map(|i| i.id.clone())
                .collect();
            self.remove_rows(st, &gone);
            st.items.shift_remove(id);
            Some(gone)
        }
    }

    /// Takes host `host`'s links off the files wrappers `ids` (a card's)
    /// brought: a file on other hosts too keeps those, one only on `host`
    /// goes from the list, a finished one stays as it is, and one running
    /// from `host` goes on from another of its hosts. Returns how many files
    /// lost a link.
    pub fn drop_wrapper_host(&self, ids: &[String], host: &str) -> usize {
        let (touched, gone) = {
            let mut st = self.lock();
            let mut touched = 0;
            let mut gone = Vec::new();
            for it in st.items.values_mut() {
                if !ids.iter().any(|id| it.listed_by(id) && !it.wrapper) {
                    continue;
                }
                if it.status == Status::Done {
                    continue;
                }
                let on_host: Vec<String> = it
                    .mirrors
                    .iter()
                    .filter(|m| *m.host == *host)
                    .map(|m| m.url.clone())
                    .collect();
                if on_host.is_empty() {
                    continue;
                }
                touched += 1;
                if on_host.len() == it.mirrors.len() {
                    it.cancel();
                    gone.push(it.id.clone());
                    continue;
                }
                if it.host == host {
                    // It starts again from another host on its next turn.
                    it.cancel();
                }
                for m in &on_host {
                    it.mirrors.retain(|x| x != m);
                    it.failed.remove(m);
                    it.gone.remove(m);
                    it.manual_urls.remove(m);
                    if it.pre_for.as_deref() == Some(m.as_str()) {
                        it.pre = None;
                        it.pre_for = None;
                    }
                }
                if it.prefer.as_deref() == Some(host) {
                    it.prefer = None;
                }
            }
            self.remove_rows(&mut st, &gone);
            // The cards no longer name it among their hosts.
            for id in ids {
                if let Some(w) = st.items.get_mut(id) {
                    w.found_hosts.retain(|h| h != host);
                }
            }
            self.touch(&mut st);
            (touched, gone)
        };
        tracing::info!("dropped {host} from {touched} file(s) of a wrapper");
        self.discard_parts(gone);
        self.0.wake.notify_one();
        touched
    }

    /// Takes every failed download off the list, with its partial file.
    pub fn clear_failed(&self) {
        let mut st = self.lock();
        let gone: Vec<String> = st
            .items
            .values()
            .filter(|i| i.status == Status::Error && !i.running() && !i.wrapper)
            .map(|i| i.id.clone())
            .collect();
        self.remove_rows(&mut st, &gone);
        self.touch(&mut st);
        drop(st);
        self.discard_parts(gone);
    }

    /// Where a finished item was saved.
    pub fn item_path(&self, id: &str) -> Option<PathBuf> {
        self.lock().items.get(id)?.path.as_ref().map(PathBuf::from)
    }

    /// Looks for a newer Haul shortly after start and every hour, while the
    /// settings allow.
    async fn watch_updates(self) {
        if !self.idle(Duration::from_secs(20)).await {
            return;
        }
        loop {
            self.check_update().await;
            if !self.idle(Duration::from_secs(3600)).await {
                return;
            }
        }
    }

    /// The timer's look at GitHub, while the settings allow.
    pub async fn check_update(&self) {
        if self.cfg().check_updates
            && let Err(e) = self.look_for_update().await
        {
            tracing::debug!("looking for an update: {e}");
        }
    }

    /// Asks GitHub for the latest release once, now, as a person can whatever
    /// the setting says. Returns the version when it is newer than this one.
    pub async fn look_for_update(&self) -> Result<Option<String>, String> {
        let cfg = self.cfg();
        let http = self.clients().on(0);
        let found =
            crate::update::latest(&http, &cfg.endpoints.github_api, env!("CARGO_PKG_VERSION"))
                .await?;
        let version = found.as_ref().map(|u| u.version.clone());
        if let Some(version) = &version {
            tracing::info!("Haul {version} is out");
        }
        *self.0.update.lock().unwrap_or_else(|e| e.into_inner()) = found;
        let mut st = self.lock();
        self.touch(&mut st);
        Ok(version)
    }

    /// Downloads the newer Haul's setup program into the data folder,
    /// checked against the published checksums. Returns where it is.
    pub async fn download_update(&self) -> Result<PathBuf, String> {
        let update = self
            .0
            .update
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .ok_or("no newer Haul is known")?;
        let http = self.clients().on(0);
        crate::update::fetch_installer(&http, &update, &self.0.data_dir.join("update")).await
    }

    // ---- persistence

    async fn saver(self) {
        loop {
            if !self.idle(Duration::from_secs(1)).await {
                return;
            }
            self.save(false);
        }
    }

    fn save(&self, force: bool) {
        let _guard = self.0.save_lock.lock().unwrap_or_else(|e| e.into_inner());
        let (queue, hosts): (_, BTreeMap<String, _>) = {
            let mut st = self.lock();
            if !st.dirty && !force {
                return;
            }
            st.dirty = false;
            (
                serde_json::to_vec(&st.items),
                st.hosts
                    .iter()
                    .map(|(k, h)| (k.clone(), h.saved()))
                    .collect(),
            )
        };
        let queue = queue
            .map_err(std::io::Error::other)
            .and_then(|bytes| store::write_atomic(&self.0.data_dir.join(store::QUEUE), &bytes));
        if let Err(e) = queue {
            tracing::error!("saving the queue: {e}");
            self.lock().dirty = true;
        }
        if let Err(e) = store::write(&self.0.data_dir, store::HOSTS, &hosts) {
            tracing::error!("saving the host speeds: {e}");
        }
    }

    /// Stops every transfer (partial files stay for resuming) and saves the
    /// queue. The engine does nothing afterwards.
    pub async fn shutdown(&self) {
        self.0.shutdown.cancel();
        self.lock().paused = true;
        self.stop_all(Duration::from_secs(3)).await;
        self.save(true);
    }

    /// Stops every transfer and waits, for `at_most`, for them to end.
    async fn stop_all(&self, at_most: Duration) {
        for it in self.lock().items.values() {
            it.cancel();
        }
        let deadline = Instant::now() + at_most;
        while Instant::now() < deadline && self.lock().items.values().any(Item::running) {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    /// Puts clients for the exits of `cfg` and `services` (a VPN's servers)
    /// in effect.
    fn set_clients(&self, cfg: &Config, services: &[String]) -> Result<(), String> {
        let clients = Clients::new(cfg, services)?;
        *self.0.clients.write().unwrap_or_else(|e| e.into_inner()) = Arc::new(clients);
        Ok(())
    }
}

/// Takes away item `id` if it is a copied page still on trial (`why`: what
/// became of it), telling nobody. Returns whether it went.
fn drop_tentative_row(st: &mut State, id: &str, why: &str) -> bool {
    let Some(it) = st.items.get(id).filter(|it| it.tentative) else {
        return false;
    };
    tracing::info!(item = %id, "copied page {} {why}: left alone", it.mirrors[0]);
    st.items.shift_remove(id);
    true
}

/// Of the names of an archive's parts, what the archive is best called: a
/// name over an id, then the one with more words, then the longer one (a
/// host's shortened name loses to the full one); the earlier of equals.
fn best_archive_name(names: impl Iterator<Item = String>) -> Option<String> {
    let says = |base: &str| {
        let words = base
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty());
        (!views::says_nothing(base), words.count(), base.len())
    };
    names
        .filter_map(|name| parts::archive_base(&name).map(str::to_string))
        .reduce(|best, base| {
            if says(&base) > says(&best) {
                base
            } else {
                best
            }
        })
}

/// Whether two sizes a page rounded can be one file's: within 2%.
fn about_the_same(a: u64, b: u64) -> bool {
    a.abs_diff(b).saturating_mul(50) <= a.max(b)
}

/// The rows another wrapper of the same page added under the same title
/// for the parts `entries` hold, by part number. A set of files a page links
/// once per host (a container per host) is one archive: part n of each is
/// the same file, though each host names it its own way. `None` unless
/// `entries` are an archive's parts and that wrapper's files are parts of
/// one archive with the very same numbers, none of a clearly other size than
/// the page shows for its counterpart: a shared title and count alone can be
/// two sets of files.
fn sibling_parts(
    st: &State,
    id: &str,
    title: Option<&str>,
    entries: &[Entry],
) -> Option<std::collections::BTreeMap<u32, String>> {
    let title = hosts::name_key(title?);
    let source = st.items.get(id)?.source.clone();
    // Its own parts: every entry one, of one archive, each number once.
    let numbers = |names: &mut dyn Iterator<Item = Option<String>>| {
        let mut form = None;
        let mut seen = std::collections::BTreeSet::new();
        for name in names {
            let part = parts::PartName::parse(&name?)?;
            if *form.get_or_insert(part.form) != part.form || !seen.insert(part.number) {
                return None;
            }
        }
        (seen.len() >= 2).then_some((form?, seen))
    };
    let own = numbers(
        &mut entries
            .iter()
            .filter(|e| !hosts::lists(&e.url))
            .map(Entry::file_name),
    )?;
    // What the page shows for each of its parts.
    let listed: HashMap<u32, u64> = entries
        .iter()
        .filter_map(|e| {
            let name = e.file_name()?;
            Some((parts::PartName::parse(&name)?.number, e.listed_size?))
        })
        .collect();
    st.items.values().find_map(|w| {
        let sibling = w.wrapper
            && w.id != id
            && w.source == source
            && w.title
                .as_deref()
                .is_some_and(|t| hosts::name_key(t) == title);
        if !sibling {
            return None;
        }
        let rows: Vec<&Item> = st
            .items
            .values()
            .filter(|i| !i.wrapper && i.listed_by(&w.id))
            .collect();
        if numbers(&mut rows.iter().map(|i| i.key_name()))? != own {
            return None;
        }
        let other_size = rows.iter().any(|i| {
            let number = i.key_name().and_then(|n| parts::PartName::parse(&n));
            let shown = number.and_then(|p| listed.get(&p.number).copied());
            shown
                .zip(i.listed_size)
                .is_some_and(|(a, b)| !about_the_same(a, b))
        });
        if other_size {
            return None;
        }
        Some(
            rows.iter()
                .filter_map(|i| {
                    Some((parts::PartName::parse(&i.key_name()?)?.number, i.id.clone()))
                })
                .collect(),
        )
    })
}

/// Whether a failure is only a note on its card, told nobody: a paste
/// a download page links that lists no links (its description, say),
/// while the card has other wrappers that do. The card shows
/// it set aside.
fn noted_quietly(st: &State, it: &Item) -> bool {
    let Some(source) = it.source.as_deref() else {
        return false;
    };
    it.wrapper
        && it.trouble == Trouble::ListsNothing
        && st.items.values().any(|o| {
            o.wrapper
                && o.id != it.id
                && o.source.as_deref() == Some(source)
                && o.trouble != Trouble::ListsNothing
        })
}

/// Moves a finished file to the system's bin, or into `bin` in tests.
/// Returns false, leaving it alone, when there is no such file: it is gone
/// already, or it is a folder (older Haul versions had megatools download
/// MEGA's files into one that other downloads share).
fn recycle(file: &Path, bin: Option<&Path>) -> Result<bool, String> {
    if !file.is_file() {
        return Ok(false);
    }
    match bin {
        Some(bin) => {
            let name = file.file_name().ok_or("no file name")?;
            std::fs::create_dir_all(bin)
                .and_then(|()| std::fs::rename(file, bin.join(name)))
                .map_err(|e| e.to_string())?;
        }
        None => trash::delete(file).map_err(|e| e.to_string())?,
    }
    Ok(true)
}

fn display_name(it: &Item) -> String {
    let link = || {
        it.mirrors
            .first()
            .map(|m| m.url.clone())
            .unwrap_or_default()
    };
    if it.wrapper {
        return it.title.clone().unwrap_or_else(link);
    }
    it.key_name().unwrap_or_else(link)
}

/// The wrappers in the queue, by what they point at.
fn wrapper_keys(st: &State) -> HashSet<String> {
    st.items
        .values()
        .filter(|i| i.wrapper)
        .filter_map(|i| i.mirrors.first())
        .map(|m| hosts::wrapper_key(m))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{HostStats, SPEED_MEMORY, store};

    #[test]
    fn old_speeds_are_forgotten() {
        let saved = |measured| store::SavedHost {
            speed: 5_000_000,
            measured,
            ok: 3,
            fails: 1,
            bytes: 9,
            normal: 7_000_000,
            redial_useless_at: measured,
        };
        let now = 10 * SPEED_MEMORY;
        let fresh = HostStats::restore(&saved(now - 60), now);
        assert_eq!((fresh.ema, fresh.normal, fresh.ok), (5e6, 7e6, 3));
        assert!(
            fresh.redial_useless,
            "a redial that didn't help is remembered"
        );
        assert_eq!(
            HostStats::restore(&fresh.saved(), now).redial_useless_at,
            now - 60
        );
        let stale = HostStats::restore(&saved(now - SPEED_MEMORY - 1), now);
        assert_eq!(
            (
                stale.ema,
                stale.normal,
                stale.measured,
                stale.ok,
                stale.fails
            ),
            (0.0, 0.0, 0, 3, 1)
        );
        assert!(!stale.redial_useless, "for a week");
    }

    /// The system's own bin, on the platforms that can list it: the file
    /// leaves its folder for the bin, and the test empties exactly that item.
    #[test]
    #[cfg(any(windows, all(unix, not(target_os = "macos"))))]
    fn recycles_into_the_system_bin() {
        use super::recycle;

        let dir = tempfile::tempdir().unwrap();
        let name = format!("haul-test-{}.bin", uuid::Uuid::new_v4());
        let file = dir.path().join(&name);
        std::fs::write(&file, b"x").unwrap();
        assert!(recycle(&file, None).unwrap());
        assert!(!file.exists());
        let mut ours = trash::os_limited::list().unwrap();
        ours.retain(|item| item.name == *name);
        assert_eq!(ours.len(), 1, "one item in the bin");
        trash::os_limited::purge_all(&ours).unwrap();

        assert!(!recycle(&file, None).unwrap(), "gone already");
        assert!(!recycle(dir.path(), None).unwrap(), "folders stay");
        assert!(dir.path().exists());
    }
}

#[cfg(test)]
mod exit_tests {
    use super::*;

    fn state() -> State {
        State {
            items: IndexMap::new(),
            hosts: IndexMap::new(),
            lanes: HashMap::new(),
            routes: BTreeMap::new(),
            paused: false,
            moving: false,
            reconnecting: false,
            draining: None,
            max_total: 8,
            dirty: false,
            merge_due: false,
            guarded: HashSet::new(),
            cleared: HashSet::new(),
            tried: passes::Tried::new(),
            reconnects: reconnect::Reconnects::default(),
            services: services::Services::default(),
        }
    }

    /// Downloads from one host spread over the exits, a limit the host showed
    /// one address holds for that address only, and a wait it asked of one
    /// leaves the others free.
    #[test]
    fn downloads_spread_over_exits() {
        let cfg = Config::default();
        let mut st = state();
        let now = Instant::now();
        let exits = [1, 2, 3];
        let mut used = Vec::new();
        for _ in 0..3 {
            let exit = st.open_exit("pixeldrain.com", &exits, &cfg, now).unwrap();
            st.lane("pixeldrain.com", exit).active += 1;
            used.push(exit);
        }
        used.sort();
        assert_eq!(used, [1, 2, 3], "one each before any gets a second");

        st.lane("pixeldrain.com", 1).cap = Some(1);
        st.lane("pixeldrain.com", 2).cooldown = Some(now + Duration::from_secs(60));
        assert_eq!(st.open_exit("pixeldrain.com", &exits, &cfg, now), Some(3));
        assert!(!st.cooling("pixeldrain.com", &exits, now));
        st.lane("pixeldrain.com", 1).cooldown = Some(now + Duration::from_secs(60));
        st.lane("pixeldrain.com", 3).cooldown = Some(now + Duration::from_secs(60));
        assert!(st.cooling("pixeldrain.com", &exits, now));
        assert_eq!(st.open_exit("pixeldrain.com", &exits, &cfg, now), None);
        // Another host is counted apart.
        assert_eq!(st.open_exit("gofile.io", &exits, &cfg, now), Some(1));
        assert_eq!(
            st.lanes_of("pixeldrain.com")
                .map(|l| l.active)
                .sum::<usize>(),
            3
        );
    }

    #[test]
    fn a_limit_the_host_showed_loosens_again() {
        let now = Instant::now();
        let mut lane = Lane {
            cap: Some(4),
            cap_since: Some(now),
            ..Lane::default()
        };
        lane.relax(32, now + Duration::from_secs(60));
        assert_eq!(lane.cap, Some(4), "not yet");
        lane.relax(32, now + CAP_RELAX);
        assert_eq!(lane.cap, Some(5), "one more after a quiet while");
        lane.relax(6, now + CAP_RELAX * 2);
        assert_eq!(lane.cap, None, "gone once it reaches the user's own limit");
        lane.cap = Some(2);
        lane.cooldown = Some(now);
        lane.fresh_start();
        assert_eq!((lane.cap, lane.cooldown), (None, None));
    }

    #[test]
    fn the_pool_is_every_proxy_once() {
        let mut cfg = Config {
            proxy: Some("socks5://a:1".into()),
            proxies: vec![
                " socks5://b:2 ".into(),
                "socks5://a:1".into(),
                String::new(),
            ],
            ..Config::default()
        };
        assert_eq!(cfg.proxy_pool(), ["socks5://a:1", "socks5://b:2"]);
        assert_eq!(cfg.proxy(), Some("socks5://a:1"));
        cfg.use_proxy = false;
        assert!(cfg.proxy_pool().is_empty());
        assert_eq!(exit_name(0), "direct");
        assert_eq!(exit_name(2), "proxy 2");
    }
}
