//! The download engine: queue, mirror grouping, scheduling and transfers.
//!
//! One mutex guards all queue state and is never held across an await. Each
//! attempt runs as its own task holding a slot on its host; a scheduler tick
//! starts new attempts, picking for every item the mirror whose host is free
//! and has measured the best throughput.

use std::{
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
use reqwest::header::RANGE;
use serde::Serialize;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt, BufReader, BufWriter},
    sync::{Notify, Semaphore, broadcast, watch},
};
use tokio_util::sync::CancellationToken;

use crate::{
    config::{self, Config, Route, Theme},
    container::{self, Link},
    hosts::{self, Cx, Entry, Http, ResolveError, Tokens},
    item::{Item, Status, Verifying},
    limiter::Limiter,
    paths, store,
};

mod settings;

pub use settings::SettingsView;

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
            _ => return Err(()),
        })
    }
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
}

/// What adding links did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Added {
    /// New rows.
    pub added: usize,
    /// Links that joined an existing row as another mirror.
    pub merged: usize,
    /// Links already in the queue.
    pub duplicates: usize,
    /// Links that are not http(s).
    pub invalid: usize,
}

#[derive(Debug, Serialize)]
pub struct Snapshot {
    pub items: Vec<ItemView>,
    pub hosts: Vec<HostView>,
    pub paused: bool,
    /// Bytes per second over every transfer.
    pub speed: f64,
    pub max_total: usize,
    /// Global cap in bytes per second; 0 means none.
    pub rate: f64,
    pub proxy: bool,
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
}

#[derive(Debug, Serialize)]
pub struct ItemView {
    pub id: String,
    pub name: String,
    pub package: String,
    pub status: &'static str,
    pub error: String,
    pub size: u64,
    pub done: u64,
    pub speed: f64,
    pub percent: Option<f32>,
    pub host: String,
    /// Host of every mirror.
    pub mirrors: Vec<String>,
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
}

#[derive(Debug, Serialize)]
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
    /// The most downloads it takes at once, learned when it refused one more
    /// than it was serving; `None` until it does.
    pub cap: Option<usize>,
}

struct HostStats {
    limit: usize,
    /// Download requests to it in flight and transfers from it in progress:
    /// what a refusal is measured against. Counting requests that may still be
    /// refused can only overstate it, and the next refusal corrects that.
    serving: usize,
    /// No new download from it before this; see [`SETTLE`].
    settle_until: Option<Instant>,
    /// See [`HostView::cap`]. Kept until a restart or until the user sets the
    /// host's slots.
    cap: Option<usize>,
    active: usize,
    cooldown: Option<Instant>,
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

    fn cooling(&self, now: Instant) -> bool {
        self.cooldown.is_some_and(|c| now < c)
    }

    /// Downloads that may run at once: the user's limit, or fewer when the
    /// host showed it takes fewer.
    fn slots(&self) -> usize {
        self.cap.map_or(self.limit, |cap| cap.min(self.limit))
    }

    fn free(&self, now: Instant) -> bool {
        self.active < self.slots()
            && !self.cooling(now)
            && self.settle_until.is_none_or(|t| now >= t)
    }

    fn new(limit: usize) -> Self {
        Self {
            limit,
            serving: 0,
            settle_until: None,
            cap: None,
            active: 0,
            cooldown: None,
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
    routes: BTreeMap<String, Route>,
    paused: bool,
    /// The download folder is changing: start nothing new.
    moving: bool,
    max_total: usize,
    dirty: bool,
}

impl State {
    fn host(&mut self, name: &str, cfg: &Config) -> &mut HostStats {
        self.hosts
            .entry(name.to_string())
            .or_insert_with(|| HostStats::new(cfg.limit_for(name)))
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

/// Counts a download as under way with its host, from the request to the end
/// of the transfer.
struct Serving<'a> {
    engine: &'a Engine,
    host: &'a str,
}

impl<'a> Serving<'a> {
    fn start(engine: &'a Engine, host: &'a str) -> Self {
        engine.lock().host(host, &engine.cfg()).serving += 1;
        Self { engine, host }
    }
}

impl Drop for Serving<'_> {
    fn drop(&mut self) {
        if let Some(h) = self.engine.lock().hosts.get_mut(self.host) {
            h.serving = h.serving.saturating_sub(1);
            if h.cap.is_some() {
                h.settle_until = Some(Instant::now() + SETTLE);
            }
        }
    }
}

/// How an attempt ended without an error.
enum Outcome {
    Done,
    /// The row is gone: merged into another, or expanded into a folder's files.
    Gone,
    /// The file was in the download folder already, at this path.
    Existing(PathBuf),
    /// Too big to download next to the files already downloading, within
    /// `max_active_gb`: back in the queue.
    Deferred,
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

/// HTTP clients for both routes, rebuilt when the proxy or user agent changes.
struct Clients {
    direct: Http,
    proxy: Option<Http>,
}

impl Clients {
    fn new(cfg: &Config) -> Result<Self, String> {
        Ok(Self {
            direct: Http::new(None, &cfg.user_agent)?,
            proxy: cfg
                .proxy()
                .map(|p| Http::new(Some(p), &cfg.user_agent))
                .transpose()?,
        })
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
    /// `State::paused`, for comparisons running outside the lock.
    paused_now: Arc<AtomicBool>,
    save_lock: Mutex<()>,
    rename_lock: Mutex<()>,
    shutdown: CancellationToken,
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
        let download_dir_fixed = opts.download_dir.is_some();
        let download_dir = opts
            .download_dir
            .or_else(|| cfg.download_dir.clone())
            .unwrap_or_else(paths::download_dir);
        let clients = Clients::new(&cfg).map_err(StartError::Proxy)?;

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
        let items: IndexMap<String, Item> = saved
            .into_iter()
            .map(|(id, s)| (id.clone(), Item::restore(id, s)))
            .collect();
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
                routes,
                paused,
                moving: false,
                max_total: cfg.max_total.max(1),
                dirty: false,
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
            paused_now: Arc::new(AtomicBool::new(paused)),
            save_lock: Mutex::new(()),
            rename_lock: Mutex::new(()),
            shutdown: CancellationToken::new(),
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
        tokio::spawn(engine.clone().saver());
        tokio::spawn(engine.clone().watch_config());
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

    fn http_for(&self, host: &str, st: &State) -> Http {
        let clients = self.clients();
        match &clients.proxy {
            Some(proxy) if st.routes.get(host) != Some(&Route::Direct) => proxy.clone(),
            _ => clients.direct.clone(),
        }
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
        self.cfg().proxy().map(str::to_string)
    }

    /// The proxy that `host` is reached through: the configured one, unless
    /// the user approved a direct route for that host.
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
                let name = link
                    .name
                    .filter(|n| !n.is_empty())
                    .or_else(|| hosts::guess_name(&link.url));
                // The same file name on another host is another mirror.
                if let Some(name) = &name
                    && let Some(it) = st.items.values_mut().find(|it| {
                        it.status != Status::Done && it.key_name().as_deref() == Some(name)
                    })
                {
                    it.mirrors.push(link.url);
                    added.merged += 1;
                    continue;
                }
                let it = Item::new(vec![link.url], name, link.package);
                if !it.probed {
                    probe.push(it.id.clone());
                }
                st.items.insert(it.id.clone(), it);
                added.added += 1;
            }
            self.touch(&mut st);
        }
        for id in probe {
            tokio::spawn(self.clone().probe(id));
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
        let http = {
            let st = self.lock();
            self.http_for("service.jdownloader.org", &st)
        };
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
        let Ok(permit) = self.0.probes.acquire().await else {
            return;
        };
        let (url, http) = {
            let st = self.lock();
            let Some(it) = st.items.get(&id) else { return };
            let url = it.mirrors[0].clone();
            let http = self.http_for(&hosts::host_of(&url), &st);
            (url, http)
        };
        let cfg = self.cfg();
        let cx = Cx {
            http: &http,
            cfg: &cfg,
            tokens: &self.0.tokens,
        };
        let found = tokio::time::timeout(Duration::from_secs(25), hosts::probe(&cx, &url))
            .await
            .ok()
            .flatten();
        // The slot is for asking hosts; comparing with a file on disk waits
        // for its own turn below, so other links keep being looked up.
        drop(permit);
        let learned = found.is_some();
        {
            let mut st = self.lock();
            let Some(it) = st.items.get_mut(&id) else {
                return;
            };
            if let Some(found) = found {
                it.name = Some(found.name);
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
        self.touch(&mut st);
        drop(st);
        if adopted {
            self.discard_parts([id]);
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
            if st.paused || st.moving {
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
            let ids: Vec<String> = st.items.keys().cloned().collect();
            for id in ids {
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
                    Some(url) => {
                        let host = hosts::host_of(&url);
                        st.host(&host, &self.cfg()).active += 1;
                        let cancel = CancellationToken::new();
                        let it = st.items.get_mut(&id).expect("item checked above");
                        it.host = host.clone();
                        it.status = Status::Resolving;
                        it.task = Some(cancel.clone());
                        running += 1;
                        busy += size;
                        changed = true;
                        tokio::spawn(self.clone().work(id, url, host, cancel));
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
    /// Parks the item when no mirror is left.
    fn pick(&self, st: &mut State, id: &str, now: Instant) -> Option<String> {
        let it = &st.items[id];
        let candidates: Vec<String> = it
            .mirrors
            .iter()
            .filter(|m| !it.failed.contains(*m) && !it.manual_urls.contains(*m))
            .cloned()
            .collect();
        if candidates.is_empty() {
            let it = st.items.get_mut(id).expect("caller checked");
            if !it.manual_urls.is_empty() && it.status != Status::Error {
                it.status = Status::Captcha;
            } else {
                it.status = Status::Error;
                if it.error.is_empty() {
                    it.error = "all mirrors failed".into();
                }
                self.plan_auto_retry(it, now);
            }
            return None;
        }
        let cfg = self.cfg();
        // Preferred hosts first, in the user's order. One that is only busy
        // is waited for; one cooling down is passed over like any other.
        let mut preferred: Vec<(usize, String)> = Vec::new();
        for m in &candidates {
            let name = hosts::host_of(m);
            if let Some(rank) = cfg.preference(&name)
                && !st.host(&name, &cfg).cooling(now)
            {
                preferred.push((rank, m.clone()));
            }
        }
        if !preferred.is_empty() {
            preferred.sort_by_key(|(rank, _)| *rank);
            return preferred
                .into_iter()
                .find(|(_, m)| st.host(&hosts::host_of(m), &cfg).free(now))
                .map(|(_, m)| m);
        }
        let mut best: Option<(String, f64)> = None;
        for m in candidates {
            let host = st.host(&hosts::host_of(&m), &cfg);
            if !host.free(now) {
                continue;
            }
            let score = host.score(now);
            if best.as_ref().is_none_or(|(_, s)| score > *s) {
                best = Some((m, score));
            }
        }
        best.map(|(m, _)| m)
    }

    // ---- attempts

    async fn work(self, id: String, url: String, host: String, cancel: CancellationToken) {
        let result = self.attempt(&id, &url, &host, &cancel).await;
        self.finish(&id, &url, &host, result);
    }

    async fn attempt(
        &self,
        id: &str,
        url: &str,
        host: &str,
        cancel: &CancellationToken,
    ) -> Result<Outcome, Fail> {
        let (pre, name, http) = {
            let mut st = self.lock();
            let it = st.items.get_mut(id).ok_or(Fail::Cancelled)?;
            it.attempt_from = it.done;
            let pre = it
                .pre
                .clone()
                .filter(|_| it.pre_for.as_deref().is_none_or(|p| p == url));
            let name = it.name.clone();
            (pre, name, self.http_for(host, &st))
        };
        let entry = match pre {
            Some(entry) => entry,
            None => {
                let cfg = self.cfg();
                let cx = Cx {
                    http: &http,
                    cfg: &cfg,
                    tokens: &self.0.tokens,
                };
                let mut entries = or_cancel(cancel, hosts::resolve(&cx, url)).await??;
                match (entries.len(), name) {
                    (0, _) => {
                        return Err(Fail::Error("nothing to download (empty folder?)".into()));
                    }
                    (1, _) => entries.remove(0),
                    // A row that came from this folder before: find its file again.
                    (_, Some(name)) => entries
                        .into_iter()
                        .find(|e| e.name.as_deref() == Some(name.as_str()))
                        .ok_or_else(|| Fail::Error(format!("{name} is no longer in the folder")))?,
                    (_, None) => {
                        self.expand(id, url, entries);
                        return Ok(Outcome::Gone);
                    }
                }
            }
        };
        {
            let mut st = self.lock();
            let it = st.items.get_mut(id).ok_or(Fail::Cancelled)?;
            if it.name.is_none() {
                it.name = entry.name.clone();
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
        match &entry.exec {
            Some(exec) => self.run_exec(id, &entry, host, exec, cancel).await?,
            None => self.fetch(id, &entry, host, &http, cancel).await?,
        }
        Ok(Outcome::Done)
    }

    /// Replaces a folder row with one row per file, in the same place.
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

    fn finish(&self, id: &str, url: &str, host: &str, result: Result<Outcome, Fail>) {
        let mut events = Vec::new();
        let mut removed = Vec::new();
        {
            let mut st = self.lock();
            let now = Instant::now();
            {
                let h = st.host(host, &self.cfg());
                h.active = h.active.saturating_sub(1);
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
                Ok(Outcome::Gone) => {}
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
                    if let Some(it) = st.items.get_mut(id) {
                        drop_used_pre(it, url);
                        it.manual_urls.insert(url.to_string());
                        it.error = message;
                        it.status = Status::Queued;
                    }
                }
                Err(Fail::Wait { seconds, message }) => {
                    let until = now + Duration::from_secs(seconds);
                    st.host(host, &self.cfg()).cooldown = Some(until);
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
                    let h = st.host(host, &self.cfg());
                    h.cap = Some(h.cap.map_or(allowed, |cap| cap.min(allowed)));
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
                }
                // Removed or merged away while it ran.
                None => removed.push(id.to_string()),
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
    /// it (which then gets `id`'s mirrors). A folded row's partial file carries
    /// over when both sizes are known and equal, and is deleted otherwise.
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
        for other in others {
            let o = &st.items[&other];
            if o.status == Status::Done {
                return true;
            }
            if o.running() {
                let extra: Vec<String> = st.items[id]
                    .mirrors
                    .iter()
                    .filter(|m| !st.items[&other].mirrors.contains(m))
                    .cloned()
                    .collect();
                st.items
                    .get_mut(&other)
                    .expect("listed above")
                    .mirrors
                    .extend(extra);
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
        host: &str,
        http: &Http,
        cancel: &CancellationToken,
    ) -> Result<(), Fail> {
        let part = self.part_path(id);
        tokio::fs::create_dir_all(part.parent().expect("part has a parent")).await?;
        let mut have = tokio::fs::metadata(&part)
            .await
            .map(|m| m.len())
            .unwrap_or(0);

        let mut req = entry
            .headers
            .iter()
            .fold(http.client.get(&entry.url), |r, (k, v)| r.header(k, v));
        if have > 0 {
            req = req.header(RANGE, format!("bytes={have}-"));
        }
        // Counted from the request to the end of the transfer.
        let _serving = Serving::start(self, host);
        let mut resp = or_cancel(cancel, req.send()).await??;
        let status = resp.status().as_u16();
        if matches!(status, 429 | 503) {
            let seconds = hosts::retry_after(resp.headers(), 90);
            return Err(Fail::Wait {
                seconds,
                message: format!("{status} rate limited"),
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
            let others = self
                .lock()
                .hosts
                .get(host)
                .map_or(0, |h| h.serving.saturating_sub(1));
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
            {
                let mut st = self.lock();
                let it = st.items.get_mut(id).ok_or(Fail::Cancelled)?;
                if it.name.is_none() {
                    it.name = hosts::disposition_name(resp.headers());
                }
                if let Some(total) = total {
                    it.size = total;
                }
                it.done = have;
                it.attempt_from = have;
                self.touch(&mut st);
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
        self.place(id, &part, &out_dir)
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

    /// Runs an external downloader (megatools) and follows its percentage.
    async fn run_exec(
        &self,
        id: &str,
        entry: &Entry,
        host: &str,
        exec: &[String],
        cancel: &CancellationToken,
    ) -> Result<(), Fail> {
        let package = self
            .lock()
            .items
            .get(id)
            .map(|i| i.package.clone())
            .unwrap_or_default();
        let out = self.out_dir(host, if package.is_empty() { "mega" } else { &package });
        tokio::fs::create_dir_all(&out).await?;
        if let Some(it) = self.lock().items.get_mut(id) {
            it.path = Some(out.display().to_string());
        }
        let (program, args) = exec
            .split_first()
            .ok_or_else(|| Fail::Error("empty command".into()))?;
        let mut cmd = tokio::process::Command::new(program);
        cmd.args(args)
            .arg("--path")
            .arg(&out)
            .arg(&entry.url)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        #[cfg(windows)]
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        let mut child = cmd
            .spawn()
            .map_err(|e| Fail::Error(format!("{program}: {e}")))?;
        let stdout = child.stdout.take().expect("piped");
        let stderr = child.stderr.take().expect("piped");
        let follow = async {
            tokio::join!(
                self.follow_percent(id, stdout),
                self.follow_percent(id, stderr)
            );
            child.wait().await
        };
        let status = or_cancel(cancel, follow).await??;
        if !status.success() {
            return Err(Fail::Error("megatools failed (quota? bad link?)".into()));
        }
        Ok(())
    }

    async fn follow_percent(&self, id: &str, stream: impl tokio::io::AsyncRead + Unpin) {
        static PERCENT: std::sync::LazyLock<regex::Regex> =
            std::sync::LazyLock::new(|| regex::Regex::new(r"(\d+(?:\.\d+)?)%").unwrap());
        // Progress lines end in carriage returns, so split on both.
        let mut reader = BufReader::new(stream);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            let mut byte = [0u8; 1];
            loop {
                match reader.read(&mut byte).await {
                    Ok(1) if byte[0] != b'\r' && byte[0] != b'\n' => buf.push(byte[0]),
                    Ok(1) => break,
                    _ => {
                        if buf.is_empty() {
                            return;
                        }
                        break;
                    }
                }
            }
            let line = String::from_utf8_lossy(&buf);
            let Some(pct) = PERCENT
                .captures_iter(&line)
                .last()
                .and_then(|c| c[1].parse::<f32>().ok())
            else {
                continue;
            };
            let mut st = self.lock();
            if let Some(it) = st.items.get_mut(id) {
                it.percent = Some(pct.min(100.0));
                if it.size > 0 {
                    it.done = (it.size as f64 * f64::from(pct) / 100.0) as u64;
                }
            }
        }
    }

    // ---- controls

    /// Returns false when there is no such item. A finished download only
    /// goes away: there is nothing to pause, resume or retry.
    pub fn item_action(&self, id: &str, action: ItemAction) -> bool {
        let mut st = self.lock();
        let Some(it) = st.items.get_mut(id) else {
            return false;
        };
        if it.status == Status::Done && action != ItemAction::Remove {
            return true;
        }
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
            }
            ItemAction::Remove => {
                let running = it.running();
                if let Some(t) = &it.task {
                    t.cancel();
                }
                st.items.shift_remove(id);
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
        if it.name.is_none() {
            it.name = name_hint
                .filter(|n| !n.is_empty())
                .or_else(|| hosts::guess_name(url));
        }
        it.pre = Some(Entry {
            url: url.into(),
            name: it.name.clone(),
            size: None,
            headers,
            exec: None,
            checksum: None,
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

    pub fn retry_failed(&self) {
        let ids: Vec<String> = self
            .lock()
            .items
            .values()
            .filter(|i| i.status == Status::Error)
            .map(|i| i.id.clone())
            .collect();
        for id in ids {
            self.item_action(&id, ItemAction::Retry);
        }
    }

    pub fn clear_done(&self) {
        let mut st = self.lock();
        st.items.retain(|_, i| i.status != Status::Done);
        self.touch(&mut st);
    }

    /// Takes every failed download off the list, with its partial file.
    pub fn clear_failed(&self) {
        let gone: Vec<String> = {
            let mut st = self.lock();
            let gone = st
                .items
                .values()
                .filter(|i| i.status == Status::Error && !i.running())
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
        let st = self.lock();
        let now = Instant::now();
        let has_proxy = self.clients().proxy.is_some();
        let cfg = self.cfg();
        let items: Vec<ItemView> = st
            .items
            .values()
            .map(|it| ItemView {
                id: it.id.clone(),
                name: display_name(it),
                package: it.package.clone(),
                status: if it.status == Status::Queued && !it.probed {
                    "checking"
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
                wait: it
                    .not_before
                    .map_or(0, |t| t.saturating_duration_since(now).as_secs()),
                retry_in: it
                    .retry_at
                    .map_or(0, |t| t.saturating_duration_since(now).as_secs().max(1)),
                open_urls: it
                    .mirrors
                    .iter()
                    .filter(|m| it.manual_urls.contains(*m))
                    .cloned()
                    .collect(),
                path: it.path.clone(),
                existing: it.existing,
                verifying: it.verifying,
            })
            .collect();
        let mut hosts: Vec<HostView> = st
            .hosts
            .iter()
            .map(|(name, h)| HostView {
                name: name.clone(),
                limit: h.limit,
                active: h.active,
                speed: h.ema,
                fails: h.fails,
                ok: h.ok,
                bytes: h.bytes,
                blocked: h.blocked,
                route: st.routes.get(name).copied().unwrap_or(if has_proxy {
                    Route::Proxy
                } else {
                    Route::Direct
                }),
                cooldown: h
                    .cooldown
                    .map_or(0, |c| c.saturating_duration_since(now).as_secs()),
                folder: cfg.folder_for(name).display().to_string(),
                preferred: cfg.preference(name),
                cap: h.cap,
            })
            .collect();
        // In the order downloads pick them: preferred, then fastest.
        hosts.sort_by(|a, b| {
            let rank = |h: &HostView| h.preferred.unwrap_or(usize::MAX);
            rank(a)
                .cmp(&rank(b))
                .then_with(|| b.speed.total_cmp(&a.speed))
        });
        Snapshot {
            speed: items.iter().map(|i| i.speed).sum(),
            items,
            hosts,
            paused: st.paused,
            max_total: st.max_total,
            rate: self.0.limiter.rate(),
            proxy: has_proxy,
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
        }
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
/// already, or it is a folder (megatools downloads into one that other
/// downloads share).
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
    it.key_name()
        .unwrap_or_else(|| it.mirrors.first().cloned().unwrap_or_default())
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
