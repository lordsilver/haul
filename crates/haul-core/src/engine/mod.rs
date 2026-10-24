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
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime},
};

use indexmap::IndexMap;
use reqwest::header::{ACCEPT_ENCODING, RANGE};
use serde::Serialize;
use tokio::{
    io::{AsyncWriteExt, BufWriter},
    sync::{Notify, Semaphore, broadcast, watch},
};
use tokio_util::sync::CancellationToken;

use crate::{
    config::{self, Config, Route, Theme},
    container::{self, Link},
    extract,
    hosts::{self, Cx, Entry, Http, ResolveError, Tokens},
    item::{Extract, Found, Item, Status, Verifying},
    limiter::Limiter,
    paths, store,
};

mod diagnostics;
mod reconnect;
mod services;
mod settings;
mod unpack;
mod views;

pub use reconnect::ReconnectView;
pub use services::ServicesView;

pub use settings::SettingsView;
pub use views::{ArchiveView, Command, Counts, PoolSource, PoolSummary, Tally, WrapperCard};

/// Automatic retries of a failed download, when `retry_failed_minutes` is on.
const AUTO_RETRIES: u32 = 3;

/// Refusals in a row from a busy host before a download counts as failed.
const BUSY_REFUSALS: u32 = 10;

/// Seconds a download refused by a busy host waits before it may try again
/// (and only then if the host has a free slot).
const BUSY_PAUSE: u64 = 5;

/// After a transfer from a host that takes a limited number at once, the next
/// one waits this long, so the host has counted the finished one out.
const SETTLE: Duration = Duration::from_secs(2);

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

/// How a download whose links are all on hosts that are off fails, before
/// their names; turning one of them on (or setting its account) tries it
/// again.
const OFF_ONLY: &str = "only on hosts that are off: ";

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

/// The addresses in use: this machine's line, proxies of the settings and
/// a VPN's servers. One pool; the status bar switches the proxies and the
/// VPN off together, each source also has a switch of its own.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, ts_rs::TS)]
#[ts(export)]
pub struct PoolView {
    /// Downloads may leave through this machine's own line.
    pub line: bool,
    pub proxies: usize,
    pub vpn: usize,
    /// The status bar's switch (`use_proxy`).
    pub on: bool,
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
}

#[derive(Debug, Serialize, ts_rs::TS)]
#[ts(export)]
pub struct Snapshot {
    pub items: Vec<ItemView>,
    /// The archives in parts among the items.
    pub archives: Vec<ArchiveView>,
    pub wrappers: Vec<WrapperView>,
    /// The wrappers as the Wrappers view shows them, one card per release.
    pub wrapper_cards: Vec<WrapperCard>,
    pub counts: Counts,
    pub hosts: Vec<HostView>,
    pub paused: bool,
    /// Bytes per second over every transfer.
    pub speed: f64,
    pub max_total: usize,
    /// Global cap in bytes per second; 0 means none.
    pub rate: f64,
    pub proxy: bool,
    /// The addresses downloads go out through, for the status bar.
    pub pool: PoolView,
    /// The pool in words, and each source of addresses.
    pub pool_summary: PoolSummary,
    /// The most downloads a host can be set to run at once.
    pub slots_max: usize,
    pub theme: Theme,
    pub watch_clipboard: bool,
    /// Removing a finished download moves its file to the [`BIN`].
    pub recycle_on_remove: bool,
    /// Dropped links and files are added at once, without the Add dialog.
    pub skip_add_dialog: bool,
    /// [`BIN`], for the views to name it.
    pub bin: &'static str,
    /// Moves whenever a setting changes, so views can fetch them again.
    pub config_version: u64,
    /// Why the settings file on disk is not in effect.
    pub config_error: Option<String>,
    pub download_dir: String,
    pub data_dir: String,
    /// The settings file, when the engine was started from one.
    pub config_file: Option<String>,
    /// The hosts that take an account, and what each asks for.
    pub accounts: Vec<hosts::AccountForm>,
    /// Getting a new address from the router.
    pub reconnect: ReconnectView,
    /// VPN services whose servers join the proxies.
    pub services: ServicesView,
    /// A newer Haul, when GitHub has one.
    pub update: Option<crate::update::UpdateView>,
}

#[derive(Debug, Serialize, ts_rs::TS)]
#[ts(export)]
pub struct ItemView {
    pub id: String,
    pub name: String,
    pub package: String,
    #[ts(
        type = "\"queued\" | \"resolving\" | \"checking\" | \"downloading\" | \"waiting\" | \"passing\" | \"paused\" | \"captcha\" | \"done\" | \"error\""
    )]
    pub status: &'static str,
    pub error: String,
    pub size: u64,
    pub done: u64,
    pub speed: f64,
    pub percent: Option<f32>,
    pub host: String,
    /// Host of every mirror.
    pub mirrors: Vec<String>,
    /// The host the user picked to download it from, when they did.
    pub download_from: Option<String>,
    /// Seconds until a waiting item may try again.
    pub wait: u64,
    /// Seconds until a failed item is tried again on its own; 0 when never.
    pub retry_in: u64,
    /// Mirrors a person has to open.
    pub open_urls: Vec<String>,
    /// Where the finished file was saved.
    pub path: Option<String>,
    /// Finished without downloading: the file was in the download folder.
    pub existing: bool,
    /// Comparing a file on disk with the host's checksum.
    pub verifying: Verifying,
    /// The wrapper it came from.
    pub source: Option<String>,
    /// Other wrappers that list it too.
    pub also_from: Vec<String>,
    /// Unpacking the archive in parts it is one of.
    pub extract: Extract,
    /// Why unpacking it failed.
    pub extract_error: String,
    /// How far unpacking is, between 0 and 1, while it runs.
    pub extract_progress: Option<f32>,
    /// The list it is in beside All downloads: `active`, `attention`,
    /// `failed`, `paused` or `done`.
    #[ts(type = "\"active\" | \"attention\" | \"failed\" | \"paused\" | \"done\"")]
    pub list: &'static str,
    /// Paused before any byte came: what a wrapper's files are until
    /// started.
    pub not_started: bool,
    /// The host it downloads from, or before it starts the one it would
    /// try first.
    pub shown_host: String,
    /// That host is a preferred one.
    pub preferred_host: bool,
    /// Other hosts that are on and could take over.
    pub other_hosts: usize,
    /// Its error without `shown_host`'s name in front.
    pub error_shown: String,
    /// What its buttons do.
    pub commands: Vec<Command>,
    /// The key of the archive in parts it is one of.
    pub archive: Option<String>,
    /// The name of the wrapper it came from.
    pub source_name: Option<String>,
}

/// A wrapper link: a page or container of further links, which unwrapping
/// turns into files that wait for the user to start them.
#[derive(Debug, Serialize, ts_rs::TS)]
#[ts(export)]
pub struct WrapperView {
    pub id: String,
    /// The link as added.
    pub url: String,
    pub host: String,
    /// The page's own name for what it holds, once unwrapped.
    pub title: Option<String>,
    pub package: String,
    /// `unwrapping`, `waiting` (to try again), `passing` (the desktop app
    /// passes its page's check by itself), `captcha` (a person has to pass
    /// its gate), `paused`, `error` or `done` (unwrapped).
    #[ts(
        type = "\"unwrapping\" | \"waiting\" | \"passing\" | \"captcha\" | \"paused\" | \"error\" | \"done\""
    )]
    pub status: &'static str,
    pub error: String,
    /// Seconds until it is tried again.
    pub wait: u64,
    /// Seconds until a failed one is tried again on its own; 0 when never.
    pub retry_in: u64,
    /// Pages a person has to open.
    pub open_urls: Vec<String>,
    /// What unwrapping found.
    pub found: Option<Found>,
    /// The hosts its links are on.
    pub hosts: Vec<String>,
    /// The wrapper it was found on (a paste a release's page links).
    pub source: Option<String>,
}

/// A host's daily allowance at full speed, over the addresses it was asked
/// from.
#[derive(Debug, Clone, Serialize, PartialEq, ts_rs::TS)]
#[ts(export)]
pub struct QuotaView {
    /// Bytes per address per day at full speed.
    pub limit: u64,
    /// The least used of the addresses: what the next download gets.
    pub used: u64,
    /// Addresses past it, and the speed they get.
    pub slowed: usize,
    pub slowed_to: u64,
    /// Addresses it was asked from.
    pub known: usize,
}

#[derive(Debug, Serialize, ts_rs::TS)]
#[ts(export)]
pub struct HostView {
    pub name: String,
    pub limit: usize,
    pub active: usize,
    pub speed: f64,
    pub fails: u32,
    pub ok: u32,
    pub bytes: u64,
    /// Answered 403/451 through the proxy, so it may block VPN addresses.
    pub blocked: bool,
    pub route: Route,
    pub cooldown: u64,
    /// Subfolder of the download folder for its files; empty for none.
    pub folder: String,
    /// Where it stands among the preferred hosts, 0 being the first.
    pub preferred: Option<usize>,
    /// Turned off by the user: its links are never tried.
    pub disabled: bool,
    /// It needs an account that is not set, so its links are left out too.
    pub needs_account: bool,
    /// Addresses it may be reached from: its slots and its learned limit
    /// count for each.
    pub exits: usize,
    /// What it allows at full speed per address today, when it says.
    pub quota: Option<QuotaView>,
    /// What a free user gets from it, where Haul knows.
    pub free: Option<hosts::Free>,
    /// The user set the account it takes, which may lift that.
    pub account: bool,
    /// The most downloads it takes at once, learned when it refused one more
    /// than it was serving; `None` until it does.
    pub cap: Option<usize>,
    /// `preferred`, `other` or `off`: the group the Hosts view shows it in.
    #[ts(type = "\"preferred\" | \"other\" | \"off\"")]
    pub group: &'static str,
    /// The fastest host measured.
    pub fastest: bool,
    /// Downloads it runs at once: `limit`, or `cap` when lower.
    pub slots: usize,
    /// A limit is set or was learned, so its slot buttons show.
    pub limited: bool,
    /// It can trade places with another preferred host.
    pub movable: bool,
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
    /// See [`HostView::cap`]. Kept until a restart or until the user sets the
    /// host's slots.
    cap: Option<usize>,
    /// When `cap` last changed: one more is allowed after [`CAP_RELAX`]
    /// without a refusal.
    cap_since: Option<Instant>,
    active: usize,
    cooldown: Option<Instant>,
    /// What the host says this address may still fetch at full speed today
    /// (pixeldrain publishes it).
    quota: Option<hosts::Allowance>,
}

impl Lane {
    /// A limit the host showed is tried one higher after [`CAP_RELAX`]
    /// without a refusal, and dropped once it reaches the user's: a refusal
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
    }

    fn slowed(&self) -> bool {
        self.quota.is_some_and(|q| q.slowed())
    }

    fn cooling(&self, now: Instant) -> bool {
        self.cooldown.is_some_and(|c| now < c)
    }

    /// Downloads that may run at once: the user's limit, or fewer when the
    /// host showed it takes fewer.
    fn slots(&self, limit: usize) -> usize {
        self.cap.map_or(limit, |cap| cap.min(limit))
    }

    fn free(&self, limit: usize, now: Instant) -> bool {
        self.active < self.slots(limit)
            && !self.cooling(now)
            && self.settle_until.is_none_or(|t| now >= t)
    }
}

struct HostStats {
    /// The user's limit of downloads at once, per exit.
    limit: usize,
    /// Smoothed bytes per second of one stream.
    ema: f64,
    /// When `ema` last changed, in seconds since 1970.
    measured: u64,
    fails: u32,
    /// Failures weighed by age: each halves every [`PENALTY_HALF_LIFE`].
    penalty: f64,
    penalty_at: Instant,
    ok: u32,
    bytes: u64,
    blocked: bool,
}

const PENALTY_HALF_LIFE: f64 = 600.0;

/// How long a file waits before its host's page is passed again, after the
/// page could not hand out its link.
const PAGE_LATER: Duration = Duration::from_secs(90);

/// How long a limit a host showed holds before one more is tried.
const CAP_RELAX: Duration = Duration::from_secs(600);

/// Saved host speeds older than this are forgotten, so a host that had a bad
/// day gets measured again.
const SPEED_MEMORY: u64 = 7 * 24 * 3600;

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

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

    /// Unknown hosts score high, so each gets tried and the ranking learns;
    /// recent failures weigh more than old ones.
    fn score(&self, now: Instant) -> f64 {
        let speed = if self.ema > 0.0 { self.ema } else { 1e6 };
        speed / (1.0 + self.penalty(now) * 0.5)
    }

    fn new(limit: usize) -> Self {
        Self {
            limit,
            ema: 0.0,
            measured: 0,
            fails: 0,
            penalty: 0.0,
            penalty_at: Instant::now(),
            ok: 0,
            bytes: 0,
            blocked: false,
        }
    }

    /// What was learned in an earlier run.
    fn restore(limit: usize, saved: &store::SavedHost, now: u64) -> Self {
        let fresh = now.saturating_sub(saved.measured) < SPEED_MEMORY;
        Self {
            ema: if fresh { saved.speed as f64 } else { 0.0 },
            measured: if fresh { saved.measured } else { 0 },
            ok: saved.ok,
            fails: saved.fails,
            bytes: saved.bytes,
            ..Self::new(limit)
        }
    }

    fn saved(&self) -> store::SavedHost {
        store::SavedHost {
            speed: self.ema.round() as u64,
            measured: self.measured,
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
    max_total: usize,
    dirty: bool,
    /// Pages that answered with a browser check (Cloudflare's) a browser
    /// usually passes by itself: the desktop app passes them out of sight,
    /// as a host's whose page passes ([`hosts::page_passes`]).
    guarded: HashSet<String>,
}

impl State {
    /// Whether the desktop app passes page `url` by itself.
    fn passes(&self, url: &str) -> bool {
        hosts::page_passes(url) || self.guarded.contains(url)
    }

    fn host(&mut self, name: &str, cfg: &Config) -> &mut HostStats {
        self.hosts
            .entry(name.to_string())
            .or_insert_with(|| HostStats::new(cfg.limit_for(name)))
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
        // What a free user gets, where it is known, holds from the start.
        let limit = match hosts::known(host).and_then(|k| k.at_once(cfg)) {
            Some(at_once) => self.host(host, cfg).limit.min(at_once),
            None => self.host(host, cfg).limit,
        };
        // An address the host has slowed for the day comes last.
        let mut best: Option<(Exit, (bool, usize))> = None;
        for &exit in exits {
            let lane = self.lane(host, exit);
            lane.relax(limit, now);
            let rank = (lane.slowed(), lane.active);
            if lane.free(limit, now) && best.is_none_or(|(_, r)| rank < r) {
                best = Some((exit, rank));
            }
        }
        best.map(|(exit, _)| exit)
    }

    /// Whether every exit of `host` is cooling down.
    fn cooling(&mut self, host: &str, exits: &[Exit], now: Instant) -> bool {
        exits.iter().all(|e| self.lane(host, *e).cooling(now))
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

fn quota_view(quotas: impl Iterator<Item = hosts::Allowance>) -> Option<QuotaView> {
    let quotas: Vec<_> = quotas.collect();
    let first = quotas.first()?;
    Some(QuotaView {
        limit: first.limit,
        used: quotas.iter().map(|q| q.used).min().unwrap_or(0),
        slowed: quotas.iter().filter(|q| q.slowed()).count(),
        slowed_to: first.slowed_to,
        known: quotas.len(),
    })
}

/// An exit as the log names it: `direct`, or `proxy 2`.
fn exit_name(exit: Exit) -> String {
    if exit == 0 {
        "direct".into()
    } else {
        format!("proxy {exit}")
    }
}

/// Counts a download as under way with its host, from the request to the end
/// of the transfer.
struct Serving<'a> {
    engine: &'a Engine,
    host: &'a str,
    exit: Exit,
}

impl<'a> Serving<'a> {
    fn start(engine: &'a Engine, host: &'a str, exit: Exit) -> Self {
        engine.lock().lane(host, exit).serving += 1;
        Self { engine, host, exit }
    }
}

impl Drop for Serving<'_> {
    fn drop(&mut self) {
        let mut st = self.engine.lock();
        let lane = st.lane(self.host, self.exit);
        lane.serving = lane.serving.saturating_sub(1);
        if lane.cap.is_some() {
            lane.settle_until = Some(Instant::now() + SETTLE);
        }
    }
}

/// How an attempt ended without an error.
enum Outcome {
    Done,
    /// The row is gone: merged into another, or expanded into a folder's or
    /// a wrapper's files.
    Gone,
    /// The file was in the download folder already, at this path.
    Existing(PathBuf),
    /// Too big to download next to the files already downloading, within
    /// `max_active_gb`: back in the queue.
    Deferred,
    /// A wrapper's files are in the queue, not started; the row is done.
    Unwrapped,
}

enum Fail {
    Cancelled,
    Wait {
        seconds: u64,
        message: String,
    },
    NeedsUser(String),
    Error(String),
    /// The connection broke mid-transfer; worth resuming on the same mirror.
    Transient(String),
    /// The host refused one more download while `allowed` others from it ran.
    Busy {
        allowed: usize,
        message: String,
    },
}

impl From<ResolveError> for Fail {
    fn from(e: ResolveError) -> Self {
        match e {
            ResolveError::Wait { seconds, message } => Fail::Wait { seconds, message },
            ResolveError::NeedsUser(m) => Fail::NeedsUser(m),
            ResolveError::Network(m) => Fail::Transient(m),
            ResolveError::Failed(m) => Fail::Error(m),
        }
    }
}

impl From<reqwest::Error> for Fail {
    fn from(e: reqwest::Error) -> Self {
        // A host that cannot be reached right now may well be back shortly.
        let network = e.is_connect() || e.is_timeout();
        let message = hosts::error_text(e);
        if network {
            Fail::Transient(message)
        } else {
            Fail::Error(message)
        }
    }
}

impl From<std::io::Error> for Fail {
    fn from(e: std::io::Error) -> Self {
        Fail::Error(e.to_string())
    }
}

async fn or_cancel<T>(cancel: &CancellationToken, fut: impl Future<Output = T>) -> Result<T, Fail> {
    tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(Fail::Cancelled),
        v = fut => Ok(v),
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
        let mut exits = vec![Http::new(None, &cfg.user_agent)?];
        let services = services
            .iter()
            .map(String::as_str)
            .filter(|_| cfg.use_proxy);
        for proxy in cfg.proxy_pool().into_iter().chain(services) {
            exits.push(Http::with(
                Some(proxy),
                &cfg.user_agent,
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
    config_version: AtomicU64,
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
    save_lock: Mutex<()>,
    rename_lock: Mutex<()>,
    shutdown: CancellationToken,
    reconnects: Mutex<reconnect::Reconnects>,
    services: Mutex<services::Services>,
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
                let stats = HostStats::restore(cfg.limit_for(name), saved, now);
                (name.clone(), stats)
            })
            .collect();
        let routes = cfg.routes.clone();
        let mut items: IndexMap<String, Item> = saved
            .into_iter()
            .map(|(id, s)| (id.clone(), Item::restore(id, s)))
            .collect();
        // Left by versions that kept them.
        drop_spent_wrappers(&mut items);
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
                max_total: cfg.max_total.max(1),
                dirty: false,
                guarded: HashSet::new(),
            }),
            limiter: Limiter::new(cfg.rate_limit_mbps * 1e6),
            config_seen: Mutex::new(config_path.as_deref().and_then(settings::stamp)),
            cfg: RwLock::new(Arc::new(cfg)),
            config_path,
            config_error: Mutex::new(None),
            config_version: AtomicU64::new(0),
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
            save_lock: Mutex::new(()),
            rename_lock: Mutex::new(()),
            shutdown: CancellationToken::new(),
            reconnects: Mutex::new(reconnect::Reconnects::default()),
            services: Mutex::new(services::Services::default()),
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
        engine.migrate_routes();
        for id in unprobed {
            tokio::spawn(engine.clone().probe(id));
        }
        tokio::spawn(engine.clone().scheduler());
        tokio::spawn(engine.clone().watch_quotas());
        tokio::spawn(engine.clone().watch_reconnects());
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

    /// A counter that moves whenever the queue changes.
    pub fn watch(&self) -> watch::Receiver<u64> {
        self.0.version.subscribe()
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.0.events.subscribe()
    }

    // ---- queue

    /// Adds every link found in `text`.
    pub fn add_text(&self, text: &str, package: &str) -> Added {
        let links = container::extract_urls(text)
            .into_iter()
            .map(|url| Link {
                url,
                name: None,
                package: package.to_string(),
            })
            .collect();
        self.add_links(links)
    }

    /// Adds the links in copied text that point at a known host or a file;
    /// links to ordinary web pages are left alone.
    pub fn add_from_clipboard(&self, text: &str) -> Added {
        let links = container::extract_urls(text)
            .into_iter()
            .filter(|url| hosts::is_download_link(url))
            .map(|url| Link {
                url,
                name: None,
                package: String::new(),
            })
            .collect::<Vec<_>>();
        if links.is_empty() {
            return Added::default();
        }
        self.add_links(links)
    }

    pub fn add_links(&self, links: Vec<Link>) -> Added {
        let mut added = Added::default();
        let mut probe = Vec::new();
        {
            let mut st = self.lock();
            let mut known: HashSet<String> = st
                .items
                .values()
                .flat_map(|i| i.mirrors.iter().cloned())
                .collect();
            let mut wrappers = wrapper_keys(&st);
            for link in links {
                let ok = url::Url::parse(&link.url)
                    .is_ok_and(|u| matches!(u.scheme(), "http" | "https"));
                if !ok {
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
                    // Unwrapped right away, as a name is looked up.
                    probe.push(it.id.clone());
                    tracing::debug!(item = %it.id, url = %it.mirrors[0], "added as a wrapper");
                    st.items.insert(it.id.clone(), it);
                    added.wrappers += 1;
                    continue;
                }
                let name = link
                    .name
                    .filter(|n| !n.is_empty())
                    .or_else(|| hosts::guess_name(&link.url));
                // The same file name on another host is another mirror.
                if let Some(name) = &name
                    && let Some(it) = st
                        .items
                        .values_mut()
                        .find(|it| it.status != Status::Done && it.same_file(name, 0))
                {
                    tracing::debug!(item = %it.id, url = %link.url, "another host for {name}");
                    it.mirrors.push(link.url);
                    added.merged += 1;
                    continue;
                }
                let it = Item::new(vec![link.url], name, link.package);
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
                .find(|m| !cfg.is_off(&hosts::host_of(m), self.pages_pass()))
                .cloned();
            let http = self.http_for(&hosts::host_of(url.as_deref().unwrap_or_default()), &st);
            (url, http, self.key_service(&st))
        };
        let cfg = self.cfg();
        let cx = Cx {
            http: &http,
            cfg: &cfg,
            tokens: &self.0.tokens,
            key_service: &key_service,
        };
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
            let mut st = self.lock();
            let mut event = None;
            if let Some(it) = st.items.get_mut(&id)
                && !it.running()
            {
                it.probed = true;
                it.gone.insert(url.clone());
                event = self.fail_mirror(&mut st, &id, url, &hosts::host_of(url), message.clone());
            }
            self.touch(&mut st);
            drop(st);
            if let Some(e) = event {
                let _ = self.0.events.send(e);
            }
            self.0.wake.notify_one();
            return;
        }
        let found = looked.found();
        // A web page on a host Haul has no code for may list a release's
        // links (a blog's or forum's post): they stand in for it, as if the
        // page's text had been pasted.
        let listed = match (&found, &url) {
            (None, Some(url)) if hosts::may_list_links(url) => {
                tokio::time::timeout(Duration::from_secs(30), hosts::page(&cx, url))
                    .await
                    .ok()
                    .and_then(Result::ok)
                    .filter(|(_, links)| !links.is_empty())
            }
            _ => None,
        };
        // The slot is for asking hosts; comparing with a file on disk waits
        // for its own turn below, so other links keep being looked up.
        drop(permit);
        // A page that lists links is a wrapper of them: its links join the
        // queue under it, not started, as a wrapper's files do.
        if let (Some((title, links)), Some(url)) = (listed, &url) {
            tracing::info!(item = %id, "{url} links {} files and wrappers", links.len());
            for id in self.unwrapped(&id, url, hosts::listed(title, links)) {
                self.spawn_probe(id);
            }
            return;
        }
        let learned = found.is_some();
        {
            let mut st = self.lock();
            let Some(it) = st.items.get_mut(&id) else {
                return;
            };
            if let Some(found) = found {
                it.name = Some(hosts::tidy_name(&found.name));
                if let Some(size) = found.size.filter(|s| *s > 0) {
                    it.size = size;
                }
                if it.checksum.is_none() {
                    it.checksum = found.checksum;
                }
                if self.dedupe(&mut st, &id) {
                    st.items.shift_remove(&id);
                    self.touch(&mut st);
                    drop(st);
                    self.0.wake.notify_one();
                    return;
                }
            }
        }
        // The row is not probed yet, so nothing starts it meanwhile.
        let existing = match learned {
            true => self.existing_file(&id).await,
            false => None,
        };
        let mut st = self.lock();
        let Some(it) = st.items.get_mut(&id) else {
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
            self.extract_when_complete(&mut st, &id);
        }
        // With its size known, this row may complete a copy of another
        // archive.
        if learned {
            self.merge_copies(&mut st);
        }
        self.touch(&mut st);
        drop(st);
        if adopted {
            self.discard_parts([id]);
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
            let st = self.lock();
            let Some(it) = st.items.get(&id) else { return };
            let url = it.mirrors[0].clone();
            let http = self.http_for(&hosts::host_of(&url), &st);
            (url, http, self.key_service(&st))
        };
        let cfg = self.cfg();
        let cx = Cx {
            http: &http,
            cfg: &cfg,
            tokens: &self.0.tokens,
            key_service: &key_service,
        };
        let found = tokio::time::timeout(Duration::from_secs(60), hosts::unwrap(&cx, &url)).await;
        match found {
            Ok(Ok(found)) => {
                for id in self.unwrapped(&id, &url, found) {
                    self.spawn_probe(id);
                }
            }
            // A gate for a person (filecrypt.cc's) says so at once, even
            // while downloads are paused.
            Ok(Err(ResolveError::NeedsUser(message))) => {
                let mut st = self.lock();
                if hosts::is_guarded(&message) {
                    st.guarded.insert(url.clone());
                }
                let mut event = None;
                if let Some(it) = st.items.get_mut(&id)
                    && !it.running()
                {
                    it.probed = true;
                    it.manual_urls.insert(url.clone());
                    it.error = message;
                    it.status = Status::Captcha;
                    event = Some(Event::NeedsUser {
                        id: id.clone(),
                        name: display_name(it),
                    });
                }
                self.touch(&mut st);
                drop(st);
                if let Some(e) = event {
                    let _ = self.0.events.send(e);
                }
            }
            // So does a link that is dead (a vault gone, 404), failing as a
            // download's dead mirror does: retried later on its own if set.
            Ok(Err(ResolveError::Failed(message))) => {
                let mut st = self.lock();
                let mut event = None;
                if let Some(it) = st.items.get_mut(&id)
                    && !it.running()
                {
                    it.probed = true;
                    event = self.fail_mirror(&mut st, &id, &url, &hosts::host_of(&url), message);
                }
                self.touch(&mut st);
                drop(st);
                if let Some(e) = event {
                    let _ = self.0.events.send(e);
                }
            }
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

    /// A file already in the download folder that row `id` would fetch again,
    /// where one of its hosts would put it: the same name and, when the host
    /// reported it, the same size; when `verify_existing` is on and the host
    /// published a checksum, also the same content. Needs the name.
    async fn existing_file(&self, id: &str) -> Option<PathBuf> {
        let (name, size, checksum, package, hosts) = {
            let st = self.lock();
            let it = st.items.get(id)?;
            let name = it.name.clone()?;
            let hosts: Vec<String> = it.mirrors.iter().map(|m| hosts::host_of(m)).collect();
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

    // ---- scheduler

    /// Asks the hosts that tell ([`hosts::Known::tells_allowance`]),
    /// through each exit, how much they still allow that address at full
    /// speed today, while the queue holds their links: at once when one is
    /// added, then once a minute, as downloads use it up.
    async fn watch_quotas(self) {
        let mut changes = self.watch();
        let mut checked: HashMap<String, Instant> = HashMap::new();
        loop {
            let wanted: Vec<String> = {
                let st = self.lock();
                let mut hosts: Vec<String> = st
                    .items
                    .values()
                    .filter(|i| i.status != Status::Done)
                    .flat_map(|i| i.mirrors.iter().map(|m| hosts::host_of(m)))
                    .filter(|h| hosts::known(h).is_some_and(|k| k.tells_allowance))
                    .collect();
                hosts.sort();
                hosts.dedup();
                hosts
            };
            let cfg = self.cfg();
            for host in wanted {
                let due = checked
                    .get(&host)
                    .is_none_or(|t| t.elapsed() >= Duration::from_secs(60));
                if !due || cfg.is_off(&host, self.pages_pass()) {
                    continue;
                }
                checked.insert(host.clone(), Instant::now());
                let exits = self.exits_for(&host, &self.lock());
                for exit in exits {
                    let http = self.clients().on(exit);
                    let cx = Cx {
                        http: &http,
                        cfg: &cfg,
                        tokens: &self.0.tokens,
                        key_service: &http,
                    };
                    let quota = hosts::allowance(&cx, &host).await;
                    let mut st = self.lock();
                    let lane = st.lane(&host, exit);
                    // Less used than before: another address (a router that
                    // got a new one, say) or a new day. What held for the
                    // old one does not.
                    if let (Some(old), Some(new)) = (lane.quota, quota)
                        && new.used < old.used
                    {
                        tracing::info!(exit = %exit_name(exit), "{host} sees a fresh address: its limits start over");
                        lane.fresh_start();
                    }
                    if quota.is_some() && lane.quota != quota {
                        if let Some(q) = quota {
                            tracing::debug!(exit = %exit_name(exit), used = q.used, limit = q.limit, "{host} allowance");
                        }
                        lane.quota = quota;
                        self.touch(&mut st);
                    }
                }
            }
            // A link just added is asked about at once.
            tokio::select! {
                _ = self.0.shutdown.cancelled() => return,
                _ = tokio::time::sleep(Duration::from_secs(60)) => {}
                _ = changes.changed() => {}
            }
        }
    }

    async fn scheduler(self) {
        loop {
            tokio::select! {
                _ = self.0.shutdown.cancelled() => return,
                _ = self.0.wake.notified() => {}
                _ = tokio::time::sleep(Duration::from_millis(400)) => {}
            }
            self.schedule();
        }
    }

    fn schedule(&self) {
        let mut events = Vec::new();
        {
            let mut st = self.lock();
            if st.paused || st.moving || st.reconnecting {
                return;
            }
            let now = Instant::now();
            let mut changed = self.auto_retry(&mut st, now);
            let mut running = st.items.values().filter(|i| i.running()).count();
            // What downloads now against max_active_gb, by full size.
            let size_cap = self.size_cap();
            let mut busy: u64 = st
                .items
                .values()
                .filter(|i| i.running())
                .map(|i| i.size)
                .sum();
            // Downloads with data on disk go first, the nearest to done
            // first; the rest keep the queue's order.
            // (By bytes on disk where the size is not known yet.)
            let mut ids: Vec<(String, f64, u64)> = st
                .items
                .iter()
                .map(|(k, i)| {
                    let near = if i.size > 0 {
                        i.done as f64 / i.size as f64
                    } else {
                        0.0
                    };
                    (k.clone(), near, i.done)
                })
                .collect();
            ids.sort_by(|a, b| {
                (b.2 > 0)
                    .cmp(&(a.2 > 0))
                    .then(b.1.total_cmp(&a.1))
                    .then(b.2.cmp(&a.2))
            });
            for (id, _, _) in ids {
                if running >= st.max_total {
                    break;
                }
                let Some(it) = st.items.get(&id) else {
                    continue;
                };
                if !matches!(it.status, Status::Queued | Status::Waiting)
                    || !it.probed
                    || it.running()
                    || it.not_before.is_some_and(|t| now < t)
                {
                    continue;
                }
                // In queue order, as with max_total: the next file waits for
                // room rather than let smaller ones behind it go first.
                if size_cap > 0 && busy > 0 && busy + it.size > size_cap {
                    break;
                }
                let size = it.size;
                let before = it.status;
                match self.pick(&mut st, &id, now) {
                    Some((url, exit)) => {
                        let host = hosts::host_of(&url);
                        let via = exit_name(exit);
                        tracing::debug!(item = %id, url = %url, "starting from {host} via {via}");
                        st.lane(&host, exit).active += 1;
                        let cancel = CancellationToken::new();
                        let it = st.items.get_mut(&id).expect("item checked above");
                        it.host = host.clone();
                        it.status = Status::Resolving;
                        it.task = Some(cancel.clone());
                        it.exit = Some(exit);
                        running += 1;
                        busy += size;
                        changed = true;
                        tokio::spawn(self.clone().work(id, url, host, exit, cancel));
                    }
                    None => {
                        let it = &st.items[&id];
                        if it.status != before {
                            changed = true;
                            if it.status == Status::Captcha {
                                events.push(Event::NeedsUser {
                                    id: id.clone(),
                                    name: display_name(it),
                                });
                            } else if it.status == Status::Error {
                                events.push(Event::Failed {
                                    id: id.clone(),
                                    name: display_name(it),
                                    error: it.error.clone(),
                                });
                            }
                        }
                    }
                }
            }
            if changed {
                self.touch(&mut st);
            }
        }
        for e in events {
            let _ = self.0.events.send(e);
        }
    }

    /// The best mirror whose host has a free slot, ranked by measured speed.
    /// Hosts the user turned off are never picked. Parks the item when no
    /// mirror is left.
    fn pick(&self, st: &mut State, id: &str, now: Instant) -> Option<(String, Exit)> {
        let cfg = self.cfg();
        let pages = self.pages_pass();
        let off = |m: &String| cfg.is_off(&hosts::host_of(m), pages);
        let it = &st.items[id];
        // A mirror whose page the desktop app passes by itself, waiting its
        // turn in the browser: the file waits for it rather than go to a
        // host Haul knows nothing of, or one measured slower.
        let pass_host = (pages && !it.page_wants_person)
            .then(|| {
                it.manual_urls
                    .iter()
                    .find(|m| st.passes(m) && !off(m) && !it.failed.contains(*m))
                    .map(|m| hosts::host_of(m))
            })
            .flatten();
        let candidates: Vec<String> = it
            .mirrors
            .iter()
            .filter(|m| !it.failed.contains(*m) && !it.manual_urls.contains(*m) && !off(m))
            .cloned()
            .collect();
        if candidates.is_empty() {
            let asks = it.manual_urls.iter().any(|m| !off(m));
            let only_off: Vec<String> = it
                .mirrors
                .iter()
                .filter(|m| !it.failed.contains(*m) && off(m))
                .map(|m| hosts::host_of(m))
                .collect();
            let it = st.items.get_mut(id).expect("caller checked");
            if asks && it.status != Status::Error {
                it.status = Status::Captcha;
            } else if !only_off.is_empty() {
                // Nothing to retry until the user turns a host back on.
                let mut names: Vec<String> = only_off
                    .into_iter()
                    .map(|h| match hosts::known(&h) {
                        Some(k) if k.lacks_account(&cfg, pages) && !cfg.is_disabled(&h) => {
                            format!("{h} (needs an account)")
                        }
                        _ => h,
                    })
                    .collect();
                names.dedup();
                it.status = Status::Error;
                it.error = format!("{OFF_ONLY}{}", names.join(", "));
                tracing::info!(item = %it.id, "{}", it.error);
            } else {
                it.status = Status::Error;
                if it.error.is_empty() {
                    it.error = "all mirrors failed".into();
                }
                self.plan_auto_retry(it, now);
            }
            return None;
        }
        // The host the user picked for this file, while it has it: waited
        // for when only busy, passed over while it cools down or once its
        // link failed, the others standing by.
        if let Some(want) = st.items[id].prefer.clone()
            && let Some(m) = candidates.iter().find(|m| hosts::host_of(m) == want)
        {
            let exits = self.exits_for(&want, st);
            if !st.cooling(&want, &exits, now) {
                return st
                    .open_exit(&want, &exits, &cfg, now)
                    .map(|exit| (m.clone(), exit));
            }
        }
        // Preferred hosts first, in the user's order. One that is only busy
        // is waited for; one cooling down is passed over like any other.
        let mut preferred: Vec<(usize, String)> = Vec::new();
        for m in &candidates {
            let name = hosts::host_of(m);
            let exits = self.exits_for(&name, st);
            if let Some(rank) = cfg.preference(&name)
                && !st.cooling(&name, &exits, now)
            {
                preferred.push((rank, m.clone()));
            }
        }
        if !preferred.is_empty() {
            preferred.sort_by_key(|(rank, _)| *rank);
            for (_, m) in preferred {
                let name = hosts::host_of(&m);
                let exits = self.exits_for(&name, st);
                if let Some(exit) = st.open_exit(&name, &exits, &cfg, now) {
                    return Some((m, exit));
                }
            }
            return None;
        }
        let pass_speed = pass_host
            .as_ref()
            .map(|h| st.host(h, &cfg).ema)
            .unwrap_or_default();
        let mut best: Option<(String, Exit, f64)> = None;
        let mut held = false;
        for m in candidates {
            let name = hosts::host_of(&m);
            if pass_host.is_some() {
                let known = hosts::known(&name).is_some();
                let ema = st.host(&name, &cfg).ema;
                if !known || ema < pass_speed {
                    held = true;
                    continue;
                }
            }
            let exits = self.exits_for(&name, st);
            let Some(exit) = st.open_exit(&name, &exits, &cfg, now) else {
                continue;
            };
            let score = st.host(&name, &cfg).score(now);
            if best.as_ref().is_none_or(|(_, _, s)| score > *s) {
                best = Some((m, exit, score));
            }
        }
        if best.is_none() && held {
            // Shown as passing until its page's turn comes.
            if let Some(it) = st.items.get_mut(id) {
                it.status = Status::Captcha;
            }
        }
        best.map(|(m, exit, _)| (m, exit))
    }

    // ---- attempts

    async fn work(
        self,
        id: String,
        url: String,
        host: String,
        exit: Exit,
        cancel: CancellationToken,
    ) {
        let result = self.attempt(&id, &url, &host, exit, &cancel).await;
        self.finish(&id, &url, &host, exit, result);
    }

    async fn attempt(
        &self,
        id: &str,
        url: &str,
        host: &str,
        exit: Exit,
        cancel: &CancellationToken,
    ) -> Result<Outcome, Fail> {
        let (pre, name, http, key_service) = {
            let mut st = self.lock();
            let it = st.items.get_mut(id).ok_or(Fail::Cancelled)?;
            it.attempt_from = it.done;
            let pre = it
                .pre
                .clone()
                .filter(|_| it.pre_for.as_deref().is_none_or(|p| p == url));
            let name = it.name.clone();
            (pre, name, self.clients().on(exit), self.key_service(&st))
        };
        let entry = match pre {
            Some(entry) if !entry.deferred => entry,
            pre => {
                // A wrapper link the user handed back stands in for the
                // row's own link.
                let source = pre.as_ref().map_or(url, |p| p.url.as_str());
                let cfg = self.cfg();
                let cx = Cx {
                    http: &http,
                    cfg: &cfg,
                    tokens: &self.0.tokens,
                    key_service: &key_service,
                };
                // A wrapper holds files, not a file: they join the queue, not
                // started, and the row keeps what it found.
                if hosts::lists(source) || self.lock().items.get(id).is_some_and(|i| i.wrapper) {
                    let found = or_cancel(cancel, hosts::unwrap(&cx, source)).await??;
                    for id in self.unwrapped(id, source, found) {
                        self.spawn_probe(id);
                    }
                    return Ok(Outcome::Unwrapped);
                }
                let mut entries = or_cancel(cancel, hosts::resolve(&cx, source)).await??;
                match (entries.len(), name) {
                    (0, _) => {
                        return Err(Fail::Error("nothing to download (empty folder?)".into()));
                    }
                    (1, _) => entries.remove(0),
                    // A row that came from this folder before: find its file again.
                    (_, Some(name)) => entries
                        .into_iter()
                        .find(|e| {
                            e.name.as_deref().map(hosts::name_key) == Some(hosts::name_key(&name))
                        })
                        .ok_or_else(|| Fail::Error(format!("{name} is no longer in the folder")))?,
                    (_, None) => {
                        self.expand(id, source, entries);
                        return Ok(Outcome::Gone);
                    }
                }
            }
        };
        {
            let mut st = self.lock();
            let it = st.items.get_mut(id).ok_or(Fail::Cancelled)?;
            if it.name.is_none() {
                it.name = entry.name.as_deref().map(hosts::tidy_name);
            }
            if let Some(size) = entry.size.filter(|s| *s > 0) {
                it.size = size;
            }
            if it.checksum.is_none() {
                it.checksum = entry.checksum.clone();
            }
            if self.dedupe(&mut st, id) {
                st.items.shift_remove(id);
                self.touch(&mut st);
                return Ok(Outcome::Gone);
            }
        }
        if let Some(path) = or_cancel(cancel, self.existing_file(id)).await? {
            return Ok(Outcome::Existing(path));
        }
        // A size learned only now may not fit next to what downloads already.
        let size_cap = self.size_cap();
        if size_cap > 0 {
            let st = self.lock();
            let size = st.items.get(id).map_or(0, |i| i.size);
            let others: u64 = st
                .items
                .values()
                .filter(|i| i.running() && i.id != id)
                .map(|i| i.size)
                .sum();
            if others > 0 && others + size > size_cap {
                return Ok(Outcome::Deferred);
            }
        }
        {
            let mut st = self.lock();
            let it = st.items.get_mut(id).ok_or(Fail::Cancelled)?;
            it.status = Status::Downloading;
            it.error.clear();
            self.touch(&mut st);
        }
        self.fetch(id, &entry, (host, exit), &http, cancel).await
    }

    /// Replaces a folder row with one row per file, in the same place, each
    /// with its entry resolved from the folder's link.
    fn expand(&self, id: &str, url: &str, entries: Vec<Entry>) {
        let mut st = self.lock();
        let Some(mut at) = st.items.get_index_of(id) else {
            return;
        };
        let package = st.items[at].package.clone();
        st.items.shift_remove_index(at);
        for e in entries {
            let mut it = Item::new(vec![url.to_string()], e.name.clone(), package.clone());
            it.size = e.size.unwrap_or(0);
            it.probed = true;
            it.pre = Some(e);
            it.pre_for = Some(url.to_string());
            let new_id = it.id.clone();
            let at_most = at.min(st.items.len());
            st.items.shift_insert(at_most, new_id.clone(), it);
            if self.dedupe(&mut st, &new_id) {
                st.items.shift_remove(&new_id);
            } else if let Some(i) = st.items.get_index_of(&new_id) {
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
        let mut known: HashSet<String> = st
            .items
            .iter()
            .filter(|(k, _)| k.as_str() != id)
            .flat_map(|(_, i)| i.mirrors.iter().cloned())
            .collect();
        known.insert(url.to_string());
        let mut wrappers = wrapper_keys(&st);
        wrappers.insert(hosts::wrapper_key(url));
        let mut seen = Found::default();
        let mut found_hosts: Vec<String> = Vec::new();
        let mut after = id.to_string();
        let cfg = self.cfg();
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
            if !nested {
                seen.files += 1;
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
                // Nothing a wrapper lists starts on its own: the user picks.
                it.status = Status::Paused;
                it.size = e.size.unwrap_or(0);
                it
            };
            it.source = Some(id.to_string());
            let new_id = it.id.clone();
            let unprobed = !it.probed;
            let at = st
                .items
                .get_index_of(&after)
                .map_or(st.items.len(), |i| i + 1);
            st.items.shift_insert(at, new_id.clone(), it);
            if self.dedupe(&mut st, &new_id) {
                st.items.shift_remove(&new_id);
            } else {
                after.clone_from(&new_id);
                if unprobed {
                    probe.push(new_id);
                }
            }
        }
        if let Some(row) = st.items.get_mut(id) {
            if !row.wrapper {
                // A row handed a wrapper's link is that wrapper now.
                row.wrapper = true;
                row.mirrors = vec![url.to_string()];
                row.name = None;
            }
            row.status = Status::Done;
            row.found = Some(seen);
            row.found_hosts = found_hosts;
            row.title = found.title;
            row.error.clear();
            row.pre = None;
            row.pre_for = None;
            row.manual = false;
            row.manual_urls.clear();
            row.failed.clear();
            row.retries = 0;
            row.auto_retries = 0;
            row.retry_at = None;
            row.not_before = None;
        }
        self.touch(&mut st);
        probe
    }

    fn finish(&self, id: &str, url: &str, host: &str, exit: Exit, result: Result<Outcome, Fail>) {
        let mut events = Vec::new();
        let mut removed = Vec::new();
        let complete = matches!(result, Ok(Outcome::Done | Outcome::Existing(_)));
        {
            let mut st = self.lock();
            let now = Instant::now();
            {
                let lane = st.lane(host, exit);
                lane.active = lane.active.saturating_sub(1);
            }
            log_outcome(id, url, &result);
            // An attempt that got data in before it stopped starts the counts
            // over: a flaky connection is not a dead mirror.
            if let Some(it) = st.items.get_mut(id)
                && it.done > it.attempt_from
            {
                it.retries = 0;
                it.busy = 0;
            }
            match result {
                Ok(Outcome::Gone | Outcome::Unwrapped) => {}
                Ok(Outcome::Deferred) => {
                    if let Some(it) = st.items.get_mut(id) {
                        it.status = Status::Queued;
                    }
                }
                Ok(Outcome::Existing(path)) => {
                    if let Some(it) = st.items.get_mut(id) {
                        it.adopt(&path);
                    }
                    // Its partial file, if any, is no longer needed.
                    removed.push(id.to_string());
                }
                Ok(Outcome::Done) => {
                    st.host(host, &self.cfg()).ok += 1;
                    if let Some(it) = st.items.get_mut(id) {
                        it.status = Status::Done;
                        it.retries = 0;
                        it.auto_retries = 0;
                        it.retry_at = None;
                        it.error.clear();
                        it.percent = None;
                        if it.size == 0 {
                            it.size = it.done;
                        }
                        events.push(Event::Finished {
                            id: id.to_string(),
                            name: display_name(it),
                        });
                        let (name, size) = (it.key_name(), it.size);
                        // Rows for the same file are no longer needed.
                        if let Some(name) = name {
                            let leftovers: Vec<String> = st
                                .items
                                .iter()
                                .filter(|(k, o)| {
                                    k.as_str() != id
                                        && o.status != Status::Done
                                        && !o.running()
                                        && o.same_file(&name, size)
                                })
                                .map(|(k, _)| k.clone())
                                .collect();
                            for k in &leftovers {
                                st.items.shift_remove(k);
                            }
                            removed.extend(leftovers);
                        }
                    }
                }
                Err(Fail::Cancelled) => {
                    if let Some(it) = st.items.get_mut(id)
                        && it.status != Status::Paused
                    {
                        it.status = Status::Queued;
                    }
                }
                Err(Fail::NeedsUser(message)) => {
                    if hosts::is_guarded(&message) {
                        st.guarded.insert(url.to_string());
                    }
                    if let Some(it) = st.items.get_mut(id) {
                        drop_used_pre(it, url);
                        it.manual_urls.insert(url.to_string());
                        it.error = message;
                        it.status = Status::Queued;
                    }
                }
                Err(Fail::Wait { seconds, message }) => {
                    let until = now + Duration::from_secs(seconds);
                    // The wait is asked of this address.
                    st.lane(host, exit).cooldown = Some(until);
                    if let Some(it) = st.items.get_mut(id) {
                        it.status = Status::Waiting;
                        it.not_before = Some(until);
                        it.error = message;
                    }
                }
                Err(Fail::Transient(message))
                    if st
                        .items
                        .get(id)
                        .is_some_and(|it| it.retries < self.cfg().resume_attempts) =>
                {
                    st.host(host, &self.cfg()).penalize(now);
                    let it = st.items.get_mut(id).expect("checked by the guard");
                    it.retries += 1;
                    let wait = (2u64 << (it.retries - 1).min(5)).min(60);
                    it.status = Status::Waiting;
                    it.not_before = Some(now + Duration::from_secs(wait));
                    // The views count the wait down themselves.
                    it.error = format!("{host}: {message}");
                }
                Err(Fail::Busy { allowed, .. })
                    if st.items.get(id).is_some_and(|it| it.busy < BUSY_REFUSALS) =>
                {
                    let lane = st.lane(host, exit);
                    lane.cap = Some(lane.cap.map_or(allowed, |cap| cap.min(allowed)));
                    lane.cap_since = Some(now);
                    let it = st.items.get_mut(id).expect("checked by the guard");
                    it.busy += 1;
                    it.status = Status::Waiting;
                    it.not_before = Some(now + Duration::from_secs(BUSY_PAUSE));
                    it.error = format!("{host} takes {allowed} at a time");
                }
                Err(
                    Fail::Error(message) | Fail::Transient(message) | Fail::Busy { message, .. },
                ) => {
                    if let Some(e) = self.fail_mirror(&mut st, id, url, host, message) {
                        events.push(e);
                    }
                }
            }
            match st.items.get_mut(id) {
                Some(it) => {
                    it.speed = 0.0;
                    it.task = None;
                    it.exit = None;
                }
                // Removed or merged away while it ran.
                None => removed.push(id.to_string()),
            }
            if complete {
                self.extract_when_complete(&mut st, id);
            }
            self.touch(&mut st);
        }
        self.discard_parts(removed);
        for e in events {
            let _ = self.0.events.send(e);
        }
        self.0.wake.notify_one();
    }

    /// Counts a failed attempt against its mirror (or, for the user's own
    /// link, asks for a new one). Returns an event when no mirror is left.
    fn fail_mirror(
        &self,
        st: &mut State,
        id: &str,
        url: &str,
        host: &str,
        message: String,
    ) -> Option<Event> {
        st.host(host, &self.cfg()).penalize(Instant::now());
        let it = st.items.get_mut(id)?;
        it.error = format!("{host}: {message}");
        // The next mirror gets its own retries.
        it.retries = 0;
        let was_manual = it.manual;
        drop_used_pre(it, url);
        if was_manual {
            // The user's link died or expired: ask again.
            it.manual_urls.insert(url.to_string());
        } else {
            it.failed.insert(url.to_string());
        }
        it.status = if it.failed.len() < it.mirrors.len() {
            Status::Queued
        } else {
            Status::Error
        };
        if it.status == Status::Error {
            self.plan_auto_retry(it, Instant::now());
        }
        (it.status == Status::Error).then(|| Event::Failed {
            id: id.to_string(),
            name: display_name(it),
            error: it.error.clone(),
        })
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
                let extra: Vec<String> = st.items[id]
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
            let it = st.items.get_mut(id).expect("checked above");
            for m in o.mirrors {
                if !it.mirrors.contains(&m) {
                    it.mirrors.push(m);
                }
            }
            it.manual_urls.extend(o.manual_urls);
            it.merge_wrappers(o.source.as_ref(), &o.also_from);
            if it.checksum.is_none() {
                it.checksum = o.checksum;
            }
            let (from, to) = (self.part_path(&other), self.part_path(id));
            if from.exists() {
                if size > 0 && o.size == size && !to.exists() {
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

    async fn fetch(
        &self,
        id: &str,
        entry: &Entry,
        (host, exit): (&str, Exit),
        http: &Http,
        cancel: &CancellationToken,
    ) -> Result<Outcome, Fail> {
        let part = self.part_path(id);
        tokio::fs::create_dir_all(part.parent().expect("part has a parent")).await?;
        let mut have = tokio::fs::metadata(&part)
            .await
            .map(|m| m.len())
            .unwrap_or(0);

        let mut req = entry
            .headers
            .iter()
            .fold(http.client.get(&entry.url), |r, (k, v)| r.header(k, v))
            // The file as stored: its length and offsets are what resuming counts.
            .header(ACCEPT_ENCODING, "identity");
        if have > 0 {
            req = req.header(RANGE, format!("bytes={have}-"));
        }
        // Counted from the request to the end of the transfer.
        let _serving = Serving::start(self, host, exit);
        let mut resp = or_cancel(cancel, req.send()).await??;
        let status = resp.status().as_u16();
        if matches!(status, 429 | 503) {
            let seconds = hosts::retry_after(resp.headers(), 90);
            return Err(Fail::Wait {
                seconds,
                message: format!("{status} rate limited"),
            });
        }
        // The host's transfer allowance for this address is used up (MEGA's
        // free quota answers so): it comes back over time.
        if status == 509 {
            let seconds = hosts::retry_after(resp.headers(), 1800);
            return Err(Fail::Wait {
                seconds,
                message: "509 transfer allowance used up for now".into(),
            });
        }
        let known_size = self.lock().items.get(id).map(|i| i.size).unwrap_or(0);
        if status == 416 && have > 0 {
            // Nothing left to send: the partial file is complete, unless we
            // know it should be longer.
            if known_size > 0 && have < known_size {
                let _ = tokio::fs::remove_file(&part).await;
                return Err(Fail::Error(format!(
                    "HTTP 416 at {have} of {known_size} bytes; starting over"
                )));
            }
            if let Some(it) = self.lock().items.get_mut(id) {
                it.done = have;
            }
        } else if status != 200 && status != 206 {
            // Other downloads from the host under way: a refusal then means it
            // takes no more at once.
            // Counted on this exit: a host limits one address.
            let others = self.lock().lane(host, exit).serving.saturating_sub(1);
            let mut note = "";
            if matches!(status, 403 | 451) && http.via_proxy && others == 0 {
                // Flag only: never switch to a direct connection on our own.
                self.lock().host(host, &self.cfg()).blocked = true;
                note = " (host may block VPN IPs)";
            }
            let message = match hosts::refusal_reason(resp).await {
                Some(reason) => format!("HTTP {status}: {reason}{note}"),
                None => format!("HTTP {status}{note}"),
            };
            if status == 403 && others > 0 {
                return Err(Fail::Busy {
                    allowed: others,
                    message,
                });
            }
            return Err(Fail::Error(message));
        } else {
            let name = self.lock().items.get(id).and_then(|i| i.name.clone());
            let wants_a_page = name.as_deref().is_some_and(|n| {
                Path::new(n).extension().is_some_and(|ext| {
                    ext.eq_ignore_ascii_case("html") || ext.eq_ignore_ascii_case("htm")
                })
            });
            if hosts::content_type(resp.headers()).contains("text/html") && !wants_a_page {
                return Err(Fail::NeedsUser(
                    "got a web page, not the file (link dead or needs a browser)".into(),
                ));
            }
            let range = hosts::content_range(resp.headers());
            if status == 206 && range.is_some_and(|(start, _)| start != have) {
                let _ = tokio::fs::remove_file(&part).await;
                return Err(Fail::Error(
                    "host resumed at the wrong offset; starting over".into(),
                ));
            }
            if status == 200 {
                have = 0;
            }
            let total = match (status, range) {
                (206, Some((_, Some(total)))) => Some(total),
                _ => hosts::content_length(resp.headers()).map(|len| have + len),
            };
            let named_now = {
                let mut st = self.lock();
                let it = st.items.get_mut(id).ok_or(Fail::Cancelled)?;
                let unnamed = it.name.is_none();
                if unnamed {
                    it.name = hosts::disposition_name(resp.headers()).map(|n| hosts::tidy_name(&n));
                }
                let named_now = unnamed && it.name.is_some();
                if let Some(total) = total {
                    it.size = total;
                }
                it.done = have;
                it.attempt_from = have;
                self.touch(&mut st);
                named_now
            };
            // Some hosts (1fichier among them) name the file only as it
            // starts: before the body comes, the same checks a name found by
            // a probe or a resolver gets. The file may be done or downloading
            // in another row, or be in the download folder already.
            if named_now {
                {
                    let mut st = self.lock();
                    if self.dedupe(&mut st, id) {
                        st.items.shift_remove(id);
                        self.touch(&mut st);
                        return Ok(Outcome::Gone);
                    }
                }
                if let Some(path) = or_cancel(cancel, self.existing_file(id)).await? {
                    return Ok(Outcome::Existing(path));
                }
            }
            let file = tokio::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .append(have > 0)
                .truncate(have == 0)
                .open(&part)
                .await?;
            let mut file = BufWriter::with_capacity(1 << 20, file);
            let mut done = have;
            let (mut mark, mut mark_bytes) = (Instant::now(), have);
            // The slowest it may be over each window, in bytes per second; 0
            // when that is off. A speed limit of our own turns it off, since
            // it would make every download look slow.
            let cfg = self.cfg();
            let floor = if cfg.rate_limit_mbps > 0.0 {
                0.0
            } else {
                f64::from(cfg.min_speed_kbps) * 1024.0
            };
            let window = Duration::from_secs(cfg.min_speed_seconds.max(1));
            let (mut window_at, mut window_bytes) = (Instant::now(), have);
            let streamed: Result<(), Fail> = async {
                loop {
                    // Woken at the window's end too, so a stalled connection
                    // is judged as well.
                    let judge_at = tokio::time::Instant::from_std(window_at + window);
                    let chunk = tokio::select! {
                        biased;
                        _ = cancel.cancelled() => return Err(Fail::Cancelled),
                        c = resp.chunk() => Some(c.map_err(|e| Fail::Transient(hosts::error_text(e)))?),
                        _ = tokio::time::sleep_until(judge_at), if floor > 0.0 => None,
                    };
                    if floor > 0.0 && window_at.elapsed() >= window {
                        let speed = (done - window_bytes) as f64 / window_at.elapsed().as_secs_f64();
                        if speed < floor {
                            return Err(Fail::Error(format!(
                                "slower than {} KB/s for {}s ({:.0} KB/s)",
                                cfg.min_speed_kbps,
                                window.as_secs(),
                                speed / 1024.0
                            )));
                        }
                        (window_at, window_bytes) = (Instant::now(), done);
                    }
                    let Some(chunk) = chunk else { continue };
                    let Some(chunk) = chunk else { return Ok(()) };
                    let chunk = match &entry.decrypt {
                        Some(key) => {
                            let mut plain = chunk.to_vec();
                            key.apply(&mut plain, done);
                            plain.into()
                        }
                        None => chunk,
                    };
                    file.write_all(&chunk).await?;
                    done += chunk.len() as u64;
                    self.0.limiter.throttle(chunk.len()).await;
                    let mut st = self.lock();
                    if let Some(it) = st.items.get_mut(id) {
                        it.done = done;
                    }
                    let elapsed = mark.elapsed().as_secs_f64();
                    let speed = (elapsed >= 1.0).then(|| (done - mark_bytes) as f64 / elapsed);
                    if let Some(speed) = speed {
                        if let Some(it) = st.items.get_mut(id) {
                            it.speed = speed;
                        }
                        (mark, mark_bytes) = (Instant::now(), done);
                    }
                    let h = st.host(host, &cfg);
                    h.bytes += chunk.len() as u64;
                    if let Some(speed) = speed {
                        h.ema = if h.ema == 0.0 {
                            speed
                        } else {
                            h.ema * 0.7 + speed * 0.3
                        };
                        h.measured = unix_now();
                    }
                }
            }
            .await;
            file.flush().await?;
            drop(file);
            streamed?;
            if let Some(total) = total
                && done < total
            {
                return Err(Fail::Transient(format!(
                    "connection closed at {done} of {total} bytes"
                )));
            }
        }
        let package = self
            .lock()
            .items
            .get(id)
            .map(|i| i.package.clone())
            .ok_or(Fail::Cancelled)?;
        let out_dir = self.out_dir(host, &package);
        tokio::fs::create_dir_all(&out_dir).await?;
        self.place(id, &part, &out_dir)?;
        Ok(Outcome::Done)
    }

    /// Where a finished file goes: the download folder, then the host's
    /// subfolder if it has one, then the package's folder. Decided when the
    /// file is complete, so a changed setting applies to running downloads.
    fn out_dir(&self, host: &str, package: &str) -> PathBuf {
        let mut dir = self.download_dir();
        let folder = self.cfg().folder_for(host);
        if !folder.as_os_str().is_empty() {
            dir.push(folder);
        }
        if !package.is_empty() {
            dir.push(paths::safe(package));
        }
        dir
    }

    /// Moves a finished partial file to its final name, never overwriting: a
    /// different file by that name already there makes it `name (1).ext`.
    fn place(&self, id: &str, part: &Path, out_dir: &Path) -> Result<(), Fail> {
        let name = self
            .lock()
            .items
            .get(id)
            .and_then(|i| i.name.clone())
            .unwrap_or_else(|| format!("{id}.bin"));
        let _guard = self.0.rename_lock.lock().unwrap_or_else(|e| e.into_inner());
        let target = paths::unused_name(out_dir, &paths::safe(&name));
        std::fs::rename(part, &target)?;
        if let Some(it) = self.lock().items.get_mut(id) {
            it.path = Some(target.display().to_string());
        }
        Ok(())
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
        let Some(it) = st.items.get_mut(id) else {
            return false;
        };
        if it.status == Status::Done && action != ItemAction::Remove {
            return true;
        }
        tracing::debug!(item = %it.id, "{action:?} by the user");
        match action {
            ItemAction::Pause => {
                it.status = Status::Paused;
                if let Some(t) = &it.task {
                    t.cancel();
                }
            }
            ItemAction::Resume | ItemAction::Retry => {
                if !it.running() {
                    it.status = Status::Queued;
                }
                it.error.clear();
                it.failed.clear();
                it.not_before = None;
                it.retries = 0;
                it.auto_retries = 0;
                it.retry_at = None;
            }
            ItemAction::RetryManual => {
                if !it.running() {
                    it.status = Status::Queued;
                }
                it.manual_urls.clear();
                it.page_wants_person = false;
            }
            ItemAction::Extract => {}
            ItemAction::Remove => {
                let running = it.running();
                if let Some(t) = &it.task {
                    t.cancel();
                }
                st.items.shift_remove(id);
                drop_spent_wrappers(&mut st.items);
                if !running {
                    self.discard_parts([id.to_string()]);
                }
            }
        }
        self.touch(&mut st);
        drop(st);
        self.0.wake.notify_one();
        true
    }

    /// Takes an item off the list, and its partial file with it. A finished
    /// file stays where it is unless `recycle_on_remove` is on: then it goes
    /// to the [`BIN`] first, and if that fails the item stays as well.
    /// Returns false when there is no such item.
    pub async fn remove(&self, id: &str) -> Result<bool, String> {
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
        Ok(self.item_action(id, ItemAction::Remove))
    }

    /// Uses a direct link the user supplied (pasted, or caught from the
    /// in-app browser) for this item.
    /// Says the desktop app passes the pages of hosts that allow it out of
    /// sight ([`hosts::Known::page_passes`]), so such a host needs no
    /// account here.
    pub fn set_pages_pass(&self, on: bool) {
        self.0.pages_pass.store(on, Ordering::Relaxed);
        self.touch(&mut self.lock());
        self.0.wake.notify_one();
    }

    pub(crate) fn pages_pass(&self) -> bool {
        self.0.pages_pass.load(Ordering::Relaxed)
    }

    /// What a host's page handed back for item `id`: its file's link, or a
    /// folder's files, each row with its own. One file keeps the row and its
    /// other mirrors ([`Engine::set_direct`]); several take its place.
    pub fn set_files(&self, id: &str, files: Vec<Entry>) -> Result<(), String> {
        match files.len() {
            0 => Err("the page handed back no file".into()),
            1 => {
                let file = files.into_iter().next().expect("one");
                self.set_direct(id, &file.url, file.headers, file.name)
            }
            _ => {
                let page = self.page_for(id).ok_or("no such item")?;
                if files.iter().any(|f| {
                    !url::Url::parse(&f.url).is_ok_and(|u| matches!(u.scheme(), "http" | "https"))
                }) {
                    return Err("not an http(s) link".into());
                }
                self.expand(id, &page, files);
                self.0.wake.notify_one();
                Ok(())
            }
        }
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
            let has = host.is_none_or(|h| it.mirrors.iter().any(|m| hosts::host_of(m) == h));
            if has && !it.wrapper {
                it.prefer = host.map(str::to_string);
                n += 1;
            }
        }
        self.touch(&mut st);
        drop(st);
        self.0.wake.notify_one();
        n
    }

    /// Item `id`'s links, for tests of what they merge into.
    #[doc(hidden)]
    pub fn debug_links(&self, id: &str) -> Vec<String> {
        self.lock()
            .items
            .get(id)
            .map(|it| it.mirrors.clone())
            .unwrap_or_default()
    }

    /// Sets item `id`'s downloaded bytes, for tests of what goes first.
    #[doc(hidden)]
    pub fn debug_set_done(&self, id: &str, done: u64) {
        if let Some(it) = self.lock().items.get_mut(id) {
            it.done = done;
        }
    }

    /// The desktop app's page for item `id` wants a person after all: the
    /// item shows as needing one from now on, until it is tried again.
    pub fn page_wants_person(&self, id: &str) {
        let mut st = self.lock();
        if let Some(it) = st.items.get_mut(id) {
            it.page_wants_person = true;
            // Held for that page: its other mirrors may go now.
            if it.status == Status::Captcha && it.task.is_none() {
                it.status = Status::Queued;
            }
        }
        self.touch(&mut st);
        drop(st);
        self.0.wake.notify_one();
    }

    /// A host's page passed out of sight could not hand out its link this
    /// time (a rate limit, an error it shows, no answer): the item waits a
    /// while and the page is passed again, asking nobody.
    pub fn page_later(&self, id: &str, reason: &str) {
        let mut st = self.lock();
        if let Some(it) = st.items.get_mut(id) {
            tracing::info!(item = %id, "page later: {reason}");
            it.error = reason.to_string();
            if it.status == Status::Captcha && it.task.is_none() {
                it.status = Status::Waiting;
                it.not_before = Some(Instant::now() + PAGE_LATER);
            }
        }
        self.touch(&mut st);
        drop(st);
        self.0.wake.notify_one();
    }

    /// A page opened for item `id` in the desktop app's browser (out of
    /// sight or by a person, past a check that kept Haul out) shows links to
    /// hosts Haul knows, wrappers or pastes: the row becomes a wrapper of
    /// them, named `title`, as a page Haul reads itself does. `links` is
    /// every address the page shows, the rest left out as for such a page;
    /// `shown` are those written out in its text, which on a paste are its
    /// list and count on any host. Returns how many it took; with none the
    /// row stays as it is.
    pub fn page_listed(
        &self,
        id: &str,
        links: &[String],
        shown: &[String],
        title: Option<&str>,
    ) -> usize {
        let Some(page) = self.page_for(id) else {
            return 0;
        };
        let mut known = hosts::listed_links(&links.join("\n"), &page);
        for link in hosts::pasted_links(shown, &page) {
            if !known.contains(&link) {
                known.push(link);
            }
        }
        if known.is_empty() {
            return 0;
        }
        let count = known.len();
        tracing::info!(item = %id, "{page} links {count} files and wrappers, in its window");
        let title = title.and_then(hosts::short_title);
        for id in self.unwrapped(id, &page, hosts::listed(title, known)) {
            self.spawn_probe(id);
        }
        count
    }

    /// A host's page, passed out of sight, said its file is gone: that
    /// mirror failed like any other, and the item goes on to the next one
    /// (or fails with `reason` when none is left), asking nobody.
    pub fn page_gone(&self, id: &str, reason: &str) {
        let Some(page) = self.page_for(id) else {
            return;
        };
        let mut st = self.lock();
        if let Some(it) = st.items.get_mut(id) {
            tracing::info!(item = %id, "{page}: {reason}");
            it.manual_urls.remove(&page);
            it.gone.insert(page.clone());
            it.failed.insert(page);
            it.error = reason.to_string();
            if it.status == Status::Captcha {
                it.status = Status::Queued;
            }
        }
        self.touch(&mut st);
        drop(st);
        self.0.wake.notify_one();
    }

    pub fn set_direct(
        &self,
        id: &str,
        url: &str,
        headers: BTreeMap<String, String>,
        name_hint: Option<String>,
    ) -> Result<(), String> {
        if !url::Url::parse(url).is_ok_and(|u| matches!(u.scheme(), "http" | "https")) {
            return Err("not an http(s) link".into());
        }
        let mut st = self.lock();
        let it = st.items.get_mut(id).ok_or("no such item")?;
        if it.status == Status::Done {
            // Another mirror may have finished it while the user was busy.
            return Err("it is downloaded already".into());
        }
        // A wrapper link (the `.dlc` a gate hands out) is further links, not
        // the file: the row unpacks it on its next turn and becomes its
        // files. The row's own links stay, so one that fails asks for the page
        // again like any link handed back, and it names nothing.
        let deferred = hosts::wraps(url);
        if it.wrapper && !deferred {
            // A file's own link, found on a wrapper's page: one more of the
            // wrapper's files, not started, with what the browser sent.
            let package = it.package.clone();
            let name = name_hint
                .filter(|n| !n.is_empty())
                .or_else(|| hosts::guess_name(url));
            let mut file = Item::new(vec![url.to_string()], name.clone(), package);
            file.status = Status::Paused;
            file.source = Some(id.to_string());
            file.probed = true;
            if !headers.is_empty() {
                file.pre = Some(Entry {
                    url: url.into(),
                    name,
                    headers,
                    ..Entry::default()
                });
                file.manual = true;
            }
            let new_id = file.id.clone();
            let at = st.items.get_index_of(id).map_or(st.items.len(), |i| i + 1);
            st.items.shift_insert(at, new_id.clone(), file);
            if self.dedupe(&mut st, &new_id) {
                st.items.shift_remove(&new_id);
            }
            self.touch(&mut st);
            return Ok(());
        }
        if it.name.is_none() && !deferred {
            it.name = name_hint
                .filter(|n| !n.is_empty())
                .or_else(|| hosts::guess_name(url))
                .map(|n| hosts::tidy_name(&n));
        }
        it.pre = Some(Entry {
            url: url.into(),
            name: if deferred { None } else { it.name.clone() },
            headers,
            deferred,
            ..Entry::default()
        });
        it.pre_for = None;
        it.manual = true;
        it.probed = true;
        if !it.running() {
            it.status = Status::Queued;
        }
        it.error.clear();
        it.not_before = None;
        it.manual_urls.clear();
        it.failed.clear();
        it.retries = 0;
        it.auto_retries = 0;
        it.retry_at = None;
        self.touch(&mut st);
        drop(st);
        self.0.wake.notify_one();
        Ok(())
    }

    /// The page a person should open for this item.
    /// Whether the desktop app passes page `url` by itself, out of sight: a
    /// host's whose page does ([`hosts::page_passes`]), or one that answered
    /// with a browser check a browser usually passes.
    pub fn page_passes(&self, url: &str) -> bool {
        self.lock().passes(url)
    }

    pub fn page_for(&self, id: &str) -> Option<String> {
        let st = self.lock();
        let it = st.items.get(id)?;
        it.mirrors
            .iter()
            .find(|m| it.manual_urls.contains(*m))
            .or_else(|| it.mirrors.first())
            .cloned()
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
                if let Some(t) = &it.task {
                    t.cancel();
                }
            }
        }
        self.touch(&mut st);
        drop(st);
        self.0.wake.notify_one();
    }

    /// Tries every failed download again; wrappers have their own section.
    pub fn retry_failed(&self) {
        let ids: Vec<String> = self
            .lock()
            .items
            .values()
            .filter(|i| i.status == Status::Error && !i.wrapper)
            .map(|i| i.id.clone())
            .collect();
        for id in ids {
            self.item_action(&id, ItemAction::Retry);
        }
    }

    /// Starts again the downloads that failed for having only hosts the user
    /// turned off, now that `host` is back on and holds one of their links.
    pub(crate) fn retry_turned_on(&self, host: &str) {
        let ids: Vec<String> = self
            .lock()
            .items
            .values()
            .filter(|i| {
                i.status == Status::Error
                    && !i.wrapper
                    && i.error.starts_with(OFF_ONLY)
                    && i.mirrors
                        .iter()
                        .any(|m| hosts::host_of(m).eq_ignore_ascii_case(host))
            })
            .map(|i| i.id.clone())
            .collect();
        for id in ids {
            self.item_action(&id, ItemAction::Retry);
        }
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
        self.touch(&mut st);
        drop(st);
        self.0.wake.notify_one();
        Some(started)
    }

    /// Takes a wrapper off the list with the files it added that never
    /// started; those that did start stay. Returns false when there is no
    /// such wrapper.
    pub fn remove_wrapper(&self, id: &str) -> bool {
        let gone: Vec<String> = {
            let mut st = self.lock();
            let Some(w) = st.items.get(id).filter(|w| w.wrapper) else {
                return false;
            };
            if let Some(t) = &w.task {
                t.cancel();
            }
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
            for k in &gone {
                st.items.shift_remove(k);
            }
            st.items.shift_remove(id);
            self.touch(&mut st);
            gone
        };
        self.discard_parts(gone);
        self.0.wake.notify_one();
        true
    }

    /// Takes every failed download off the list, with its partial file.
    pub fn clear_failed(&self) {
        let gone: Vec<String> = {
            let mut st = self.lock();
            let gone = st
                .items
                .values()
                .filter(|i| i.status == Status::Error && !i.running() && !i.wrapper)
                .map(|i| i.id.clone())
                .collect::<Vec<_>>();
            for id in &gone {
                st.items.shift_remove(id);
            }
            self.touch(&mut st);
            gone
        };
        self.discard_parts(gone);
    }

    /// Where a finished item was saved.
    pub fn item_path(&self, id: &str) -> Option<PathBuf> {
        self.lock().items.get(id)?.path.as_ref().map(PathBuf::from)
    }

    pub fn snapshot(&self) -> Snapshot {
        // Before the state's lock: the two are always taken in this order.
        let reconnect = self.reconnect_view();
        let services = self.services_view();
        let pages = self.pages_pass();
        let pool = {
            let cfg = self.cfg();
            let proxies = cfg.proxy_pool().len();
            let vpn = if cfg.use_proxy {
                self.service_proxies().len()
            } else {
                0
            };
            PoolView {
                line: proxies + vpn == 0 || cfg.proxy_with_direct,
                proxies,
                vpn,
                on: cfg.use_proxy,
            }
        };
        let pool_summary = views::pool(
            &self.cfg(),
            pool,
            views::VpnFound {
                servers: services.nordvpn.len(),
                error: services.error.clone(),
            },
        );
        let st = self.lock();
        let now = Instant::now();
        let has_proxy = self.clients().exits.len() > 1;
        let cfg = self.cfg();
        let wait = |it: &Item| {
            it.not_before
                .map_or(0, |t| t.saturating_duration_since(now).as_secs())
        };
        let retry_in = |it: &Item| {
            it.retry_at
                .map_or(0, |t| t.saturating_duration_since(now).as_secs().max(1))
        };
        let open_urls = |it: &Item| -> Vec<String> {
            it.mirrors
                .iter()
                .filter(|m| it.manual_urls.contains(*m))
                .cloned()
                .collect()
        };
        let wrappers: Vec<WrapperView> = st
            .items
            .values()
            .filter(|it| it.wrapper)
            .map(|it| WrapperView {
                id: it.id.clone(),
                url: it.mirrors.first().cloned().unwrap_or_default(),
                host: it
                    .mirrors
                    .first()
                    .map(|m| hosts::host_of(m))
                    .unwrap_or_default(),
                title: it.title.clone(),
                package: it.package.clone(),
                status: match it.status {
                    Status::Queued | Status::Resolving | Status::Downloading => "unwrapping",
                    // The desktop app passes its page by itself (a page or
                    // paste behind Cloudflare's check): nothing for the
                    // person yet.
                    Status::Captcha
                        if pages
                            && !it.page_wants_person
                            && it.manual_urls.iter().any(|u| st.passes(u)) =>
                    {
                        "passing"
                    }
                    other => other.as_str(),
                },
                source: it.source.clone(),
                error: it.error.clone(),
                wait: wait(it),
                retry_in: retry_in(it),
                open_urls: open_urls(it),
                found: it.found,
                hosts: it.found_hosts.clone(),
            })
            .collect();
        let unpacked = self.unpack_progress();
        let items: Vec<ItemView> = st
            .items
            .values()
            .filter(|it| !it.wrapper)
            .map(|it| ItemView {
                id: it.id.clone(),
                name: display_name(it),
                package: it.package.clone(),
                status: if it.status == Status::Queued && !it.probed {
                    "checking"
                } else if it.status == Status::Captcha
                    && pages
                    && !it.page_wants_person
                    && it.manual_urls.iter().any(|u| st.passes(u))
                {
                    // The desktop app passes one of its pages by itself, in
                    // turn: nothing for the person yet.
                    "passing"
                } else {
                    it.status.as_str()
                },
                error: it.error.clone(),
                size: it.size,
                done: it.done,
                speed: it.speed,
                percent: it.percent,
                host: it.host.clone(),
                mirrors: it.mirrors.iter().map(|m| hosts::host_of(m)).collect(),
                wait: wait(it),
                retry_in: retry_in(it),
                open_urls: open_urls(it),
                path: it.path.clone(),
                existing: it.existing,
                verifying: it.verifying,
                source: it.source.clone(),
                also_from: it.also_from.clone(),
                extract: it.extract,
                extract_error: it.extract_error.clone(),
                extract_progress: unpacked.filter(|_| it.extract == Extract::Extracting),
                list: "",
                not_started: false,
                shown_host: String::new(),
                preferred_host: false,
                other_hosts: 0,
                error_shown: String::new(),
                commands: Vec::new(),
                archive: None,
                source_name: None,
                download_from: it.prefer.clone(),
            })
            .collect();
        let route = |name: &str| {
            st.routes.get(name).copied().unwrap_or(if has_proxy {
                Route::Proxy
            } else {
                Route::Direct
            })
        };
        let mut hosts: Vec<HostView> = st
            .hosts
            .iter()
            .map(|(name, h)| {
                let exits = self.exits_for(name, &st);
                let lane = |e: &Exit| st.lanes.get(&(name.clone(), *e));
                // Waiting only when every address it may use was asked to.
                let cooldown = exits
                    .iter()
                    .map(|e| {
                        lane(e)
                            .and_then(|l| l.cooldown)
                            .map_or(0, |c| c.saturating_duration_since(now).as_secs())
                    })
                    .min()
                    .unwrap_or(0);
                HostView {
                    name: name.clone(),
                    limit: h.limit,
                    active: st.lanes_of(name).map(|l| l.active).sum(),
                    speed: h.ema,
                    fails: h.fails,
                    ok: h.ok,
                    bytes: h.bytes,
                    blocked: h.blocked,
                    route: route(name),
                    cooldown,
                    folder: cfg.folder_for(name).display().to_string(),
                    preferred: cfg.preference(name),
                    disabled: cfg.is_disabled(name),
                    needs_account: hosts::known(name)
                        .is_some_and(|k| k.lacks_account(&cfg, self.pages_pass())),
                    cap: st.lanes_of(name).filter_map(|l| l.cap).min(),
                    quota: quota_view(exits.iter().filter_map(|e| lane(e).and_then(|l| l.quota))),
                    free: hosts::known(name)
                        .map(|k| k.free)
                        .filter(|f| *f != hosts::Free::default()),
                    account: hosts::known(name).is_some_and(|k| k.has_account(&cfg)),
                    exits: exits.len(),
                    group: "",
                    fastest: false,
                    slots: 0,
                    limited: false,
                    movable: false,
                }
            })
            .collect();
        // A host shows before Haul has reached it when it is preferred or
        // turned off, so the choice can be seen and changed, and when a
        // download has a link on it, so it can be turned off before it is
        // tried. A wrapper's site is a source of links, not a host.
        let mut names: Vec<String> = cfg
            .preferred_hosts
            .iter()
            .chain(&cfg.disabled_hosts)
            .map(|h| h.to_ascii_lowercase())
            .collect();
        for it in st.items.values().filter(|i| !i.wrapper) {
            names.extend(it.mirrors.iter().map(|m| hosts::host_of(m)));
        }
        hosts.retain(|h| !hosts::wraps(&format!("https://{}/", h.name)));
        for name in names {
            if name.is_empty()
                || hosts::wraps(&format!("https://{name}/"))
                || hosts.iter().any(|h| h.name == name)
            {
                continue;
            }
            hosts.push(HostView {
                limit: cfg.limit_for(&name),
                active: 0,
                speed: 0.0,
                fails: 0,
                ok: 0,
                bytes: 0,
                blocked: false,
                route: route(&name),
                cooldown: 0,
                folder: cfg.folder_for(&name).display().to_string(),
                preferred: cfg.preference(&name),
                disabled: cfg.is_disabled(&name),
                needs_account: hosts::known(&name)
                    .is_some_and(|k| k.lacks_account(&cfg, self.pages_pass())),
                cap: None,
                exits: self.exits_for(&name, &st).len(),
                free: hosts::known(&name)
                    .map(|k| k.free)
                    .filter(|f| *f != hosts::Free::default()),
                account: hosts::known(&name).is_some_and(|k| k.has_account(&cfg)),
                quota: quota_view(
                    self.exits_for(&name, &st)
                        .iter()
                        .filter_map(|e| st.lanes.get(&(name.clone(), *e)).and_then(|l| l.quota)),
                ),
                name,
                group: "",
                fastest: false,
                slots: 0,
                limited: false,
                movable: false,
            });
        }
        // In the order downloads pick them: preferred, then fastest, and the
        // ones turned off last.
        hosts.sort_by(|a, b| {
            let rank = |h: &HostView| h.preferred.unwrap_or(usize::MAX);
            (a.disabled || a.needs_account)
                .cmp(&(b.disabled || b.needs_account))
                .then_with(|| rank(a).cmp(&rank(b)))
                .then_with(|| b.speed.total_cmp(&a.speed))
                .then_with(|| a.name.cmp(&b.name))
        });
        let mut items = items;
        views::hosts(&mut hosts);
        views::items(&mut items, &hosts, &wrappers, cfg.recycle_on_remove);
        let archives = views::archives(&mut items, &hosts, unpack::archives(&st.items));
        let wrapper_cards = views::wrapper_cards(&wrappers, &items);
        let counts = views::counts(&items, &wrappers, wrapper_cards.len(), st.paused);
        Snapshot {
            speed: items.iter().map(|i| i.speed).sum(),
            items,
            archives,
            wrappers,
            wrapper_cards,
            counts,
            hosts,
            paused: st.paused,
            max_total: st.max_total,
            rate: self.0.limiter.rate(),
            proxy: has_proxy,
            pool,
            pool_summary,
            slots_max: config::FALLBACK_LIMIT,
            theme: cfg.theme,
            watch_clipboard: cfg.watch_clipboard,
            recycle_on_remove: cfg.recycle_on_remove,
            skip_add_dialog: cfg.skip_add_dialog,
            bin: BIN,
            config_version: self.0.config_version.load(Ordering::Relaxed),
            config_error: self
                .0
                .config_error
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
            download_dir: self.download_dir().display().to_string(),
            data_dir: self.0.data_dir.display().to_string(),
            config_file: self.0.config_path.as_ref().map(|p| p.display().to_string()),
            accounts: hosts::account_forms(&cfg),
            reconnect,
            services,
            update: self
                .0
                .update
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref()
                .map(crate::update::Update::view),
        }
    }

    /// Looks for a newer Haul shortly after start and every six hours, while
    /// the settings allow.
    async fn watch_updates(self) {
        tokio::select! {
            () = self.0.shutdown.cancelled() => return,
            () = tokio::time::sleep(Duration::from_secs(20)) => {}
        }
        loop {
            self.check_update().await;
            tokio::select! {
                () = self.0.shutdown.cancelled() => return,
                () = tokio::time::sleep(Duration::from_secs(6 * 3600)) => {}
            }
        }
    }

    /// Asks GitHub for the latest release once, now.
    pub async fn check_update(&self) {
        let cfg = self.cfg();
        if !cfg.check_updates {
            return;
        }
        let http = self.clients().on(0);
        match crate::update::latest(&http, &cfg.endpoints.github_api, env!("CARGO_PKG_VERSION"))
            .await
        {
            Ok(found) => {
                if let Some(u) = &found {
                    tracing::info!("Haul {} is out", u.version);
                }
                *self.0.update.lock().unwrap_or_else(|e| e.into_inner()) = found;
                let mut st = self.lock();
                self.touch(&mut st);
            }
            Err(e) => tracing::debug!("looking for an update: {e}"),
        }
    }

    /// Downloads the newer Haul's setup program into the data folder,
    /// checked against the release's checksums. Returns where it is.
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
            tokio::select! {
                _ = self.0.shutdown.cancelled() => return,
                _ = tokio::time::sleep(Duration::from_secs(1)) => {}
            }
            self.save(false);
        }
    }

    fn save(&self, force: bool) {
        let _guard = self.0.save_lock.lock().unwrap_or_else(|e| e.into_inner());
        let (queue, hosts): (IndexMap<String, _>, BTreeMap<String, _>) = {
            let mut st = self.lock();
            if !st.dirty && !force {
                return;
            }
            st.dirty = false;
            (
                st.items
                    .iter()
                    .map(|(k, v)| (k.clone(), v.saved()))
                    .collect(),
                st.hosts
                    .iter()
                    .map(|(k, h)| (k.clone(), h.saved()))
                    .collect(),
            )
        };
        if let Err(e) = store::write(&self.0.data_dir, store::QUEUE, &queue) {
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
        {
            let mut st = self.lock();
            st.paused = true;
            for it in st.items.values() {
                if let Some(t) = &it.task {
                    t.cancel();
                }
            }
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline && self.lock().items.values().any(Item::running) {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        self.save(true);
    }
}

fn log_outcome(id: &str, url: &str, result: &Result<Outcome, Fail>) {
    match result {
        Ok(Outcome::Done) => tracing::info!(item = id, url, "downloaded"),
        Ok(Outcome::Existing(path)) => {
            tracing::info!(item = id, url, "already downloaded: {}", path.display())
        }
        Ok(Outcome::Gone | Outcome::Deferred) | Err(Fail::Cancelled) => {}
        Ok(Outcome::Unwrapped) => tracing::info!(item = id, url, "unwrapped"),
        Err(Fail::Wait { seconds, message }) => {
            tracing::info!(item = id, url, seconds, "host asks to wait: {message}")
        }
        Err(Fail::NeedsUser(message)) => {
            tracing::info!(item = id, url, "needs a person: {message}")
        }
        Err(Fail::Transient(message)) => {
            tracing::warn!(item = id, url, "transfer broke: {message}")
        }
        Err(Fail::Busy { allowed, message }) => {
            tracing::info!(item = id, url, allowed, "host busy: {message}")
        }
        Err(Fail::Error(message)) => tracing::warn!(item = id, url, "attempt failed: {message}"),
    }
}

/// Forgets a resolved link once an attempt with it failed: a user's link is
/// asked for again, a folder's link is resolved afresh.
fn drop_used_pre(it: &mut Item, url: &str) {
    if it.pre.is_some() && it.pre_for.as_deref().is_none_or(|p| p == url) {
        it.pre = None;
        it.pre_for = None;
        it.manual = false;
    }
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
    let link = || it.mirrors.first().cloned().unwrap_or_default();
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
        };
        let now = 10 * SPEED_MEMORY;
        let fresh = HostStats::restore(4, &saved(now - 60), now);
        assert_eq!((fresh.ema, fresh.ok, fresh.limit), (5e6, 3, 4));
        let stale = HostStats::restore(4, &saved(now - SPEED_MEMORY - 1), now);
        assert_eq!(
            (stale.ema, stale.measured, stale.ok, stale.fails),
            (0.0, 0, 3, 1)
        );
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
            max_total: 8,
            dirty: false,
            guarded: HashSet::new(),
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
