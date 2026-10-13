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
    sync::{Arc, Mutex, MutexGuard},
    time::{Duration, Instant},
};

use indexmap::IndexMap;
use reqwest::header::RANGE;
use serde::{Deserialize, Serialize};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt, BufReader, BufWriter},
    sync::{Notify, Semaphore, broadcast, watch},
};
use tokio_util::sync::CancellationToken;

use crate::{
    config::{self, Config, Setting, Theme},
    container::{self, Link},
    hosts::{self, Cx, Entry, Http, ResolveError, Tokens},
    item::{Item, Status},
    limiter::Limiter,
    paths, store,
};

pub struct Options {
    /// Holds `config.toml`, the queue and the instance lock.
    pub data_dir: PathBuf,
    /// Overrides `download_dir` from the config.
    pub download_dir: Option<PathBuf>,
    /// Used instead of reading `config.toml` and the environment.
    pub config: Option<Config>,
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

/// How a host is reached when a proxy is configured.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Route {
    Proxy,
    Direct,
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
    /// Mirrors a person has to open.
    pub open_urls: Vec<String>,
    /// Where the finished file was saved.
    pub path: Option<String>,
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
}

struct HostStats {
    limit: usize,
    active: usize,
    cooldown: Option<Instant>,
    /// Smoothed bytes per second of one stream.
    ema: f64,
    fails: u32,
    ok: u32,
    bytes: u64,
    blocked: bool,
}

impl HostStats {
    /// Unknown hosts score high, so each gets tried and the ranking learns.
    fn score(&self) -> f64 {
        let speed = if self.ema > 0.0 { self.ema } else { 1e6 };
        speed / (1.0 + f64::from(self.fails) * 0.5)
    }

    fn free(&self, now: Instant) -> bool {
        self.active < self.limit && self.cooldown.is_none_or(|c| now >= c)
    }
}

struct State {
    items: IndexMap<String, Item>,
    hosts: IndexMap<String, HostStats>,
    routes: BTreeMap<String, Route>,
    paused: bool,
    max_total: usize,
    dirty: bool,
}

impl State {
    fn host(&mut self, name: &str, cfg: &Config) -> &mut HostStats {
        self.hosts
            .entry(name.to_string())
            .or_insert_with(|| HostStats {
                limit: cfg.limit_for(name),
                active: 0,
                cooldown: None,
                ema: 0.0,
                fails: 0,
                ok: 0,
                bytes: 0,
                blocked: false,
            })
    }
}

/// How an attempt ended without an error.
enum Outcome {
    Done,
    /// The row is gone: merged into another, or expanded into a folder's files.
    Gone,
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
}

/// Resumes after a broken transfer before the mirror counts as failed.
const MAX_RETRIES: u32 = 3;

impl From<ResolveError> for Fail {
    fn from(e: ResolveError) -> Self {
        match e {
            ResolveError::Wait { seconds, message } => Fail::Wait { seconds, message },
            ResolveError::NeedsUser(m) => Fail::NeedsUser(m),
            ResolveError::Failed(m) => Fail::Error(m),
        }
    }
}

impl From<reqwest::Error> for Fail {
    fn from(e: reqwest::Error) -> Self {
        Fail::Error(e.without_url().to_string())
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

struct Inner {
    state: Mutex<State>,
    cfg: Config,
    /// `config.toml`, which settings changed in the app are written back to.
    config_path: Option<PathBuf>,
    theme: Mutex<Theme>,
    data_dir: PathBuf,
    download_dir: PathBuf,
    direct: Http,
    proxy: Option<Http>,
    tokens: Tokens,
    limiter: Limiter,
    wake: Notify,
    version: watch::Sender<u64>,
    events: broadcast::Sender<Event>,
    probes: Semaphore,
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
        let download_dir = opts
            .download_dir
            .or_else(|| cfg.download_dir.clone())
            .unwrap_or_else(paths::download_dir);
        let direct = Http::new(None, &cfg.user_agent).map_err(StartError::Proxy)?;
        let proxy = cfg
            .proxy()
            .map(|p| Http::new(Some(p), &cfg.user_agent))
            .transpose()
            .map_err(StartError::Proxy)?;

        let saved = store::load_queue(&data_dir)?;
        let routes = store::load_routes(&data_dir)?;
        let items: IndexMap<String, Item> = saved
            .into_iter()
            .map(|(id, s)| (id.clone(), Item::restore(id, s)))
            .collect();
        let unprobed: Vec<String> = items
            .values()
            .filter(|i| !i.probed && i.status == Status::Queued)
            .map(|i| i.id.clone())
            .collect();

        let engine = Engine(Arc::new(Inner {
            state: Mutex::new(State {
                items,
                hosts: IndexMap::new(),
                routes,
                paused: false,
                max_total: cfg.max_total.max(1),
                dirty: false,
            }),
            limiter: Limiter::new(cfg.rate_limit_mbps * 1e6),
            theme: Mutex::new(cfg.theme),
            cfg,
            config_path,
            data_dir,
            download_dir,
            direct,
            proxy,
            tokens: Tokens::default(),
            wake: Notify::new(),
            version: watch::Sender::new(0),
            events: broadcast::Sender::new(64),
            probes: Semaphore::new(4),
            save_lock: Mutex::new(()),
            rename_lock: Mutex::new(()),
            shutdown: CancellationToken::new(),
            _lock: lock,
        }));
        for id in unprobed {
            tokio::spawn(engine.clone().probe(id));
        }
        tokio::spawn(engine.clone().scheduler());
        tokio::spawn(engine.clone().saver());
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

    fn http_for(&self, host: &str, st: &State) -> Http {
        match &self.0.proxy {
            Some(proxy) if st.routes.get(host) != Some(&Route::Direct) => proxy.clone(),
            _ => self.0.direct.clone(),
        }
    }

    pub fn data_dir(&self) -> &Path {
        &self.0.data_dir
    }

    pub fn download_dir(&self) -> &Path {
        &self.0.download_dir
    }

    /// The configured proxy.
    pub fn proxy(&self) -> Option<&str> {
        self.0.cfg.proxy()
    }

    /// The proxy that `host` is reached through: the configured one, unless
    /// the user approved a direct route for that host.
    pub fn proxy_for(&self, host: &str) -> Option<&str> {
        let direct = self.lock().routes.get(host) == Some(&Route::Direct);
        if direct { None } else { self.0.cfg.proxy() }
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
        let http = {
            let st = self.lock();
            self.http_for("service.jdownloader.org", &st)
        };
        let links = container::read(
            &http.client,
            &self.0.cfg.endpoints.dlc_service,
            filename,
            bytes,
        )
        .await?;
        Ok(self.add_links(links))
    }

    /// Learns the name and size up front, so the same file on several hosts
    /// is merged before anything downloads.
    async fn probe(self, id: String) {
        let Ok(_permit) = self.0.probes.acquire().await else {
            return;
        };
        let (url, http) = {
            let st = self.lock();
            let Some(it) = st.items.get(&id) else { return };
            let url = it.mirrors[0].clone();
            let http = self.http_for(&hosts::host_of(&url), &st);
            (url, http)
        };
        let cx = Cx {
            http: &http,
            cfg: &self.0.cfg,
            tokens: &self.0.tokens,
        };
        let found = tokio::time::timeout(Duration::from_secs(25), hosts::probe(&cx, &url))
            .await
            .ok()
            .flatten();
        let mut st = self.lock();
        let Some(it) = st.items.get_mut(&id) else {
            return;
        };
        it.probed = true;
        if let Some((name, size)) = found {
            it.name = Some(name);
            if let Some(size) = size.filter(|s| *s > 0) {
                it.size = size;
            }
            if self.dedupe(&mut st, &id) {
                st.items.shift_remove(&id);
            }
        }
        self.touch(&mut st);
        drop(st);
        self.0.wake.notify_one();
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
            if st.paused {
                return;
            }
            let now = Instant::now();
            let mut running = st.items.values().filter(|i| i.running()).count();
            let ids: Vec<String> = st.items.keys().cloned().collect();
            let mut changed = false;
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
                let before = it.status;
                match self.pick(&mut st, &id, now) {
                    Some(url) => {
                        let host = hosts::host_of(&url);
                        st.host(&host, &self.0.cfg).active += 1;
                        let cancel = CancellationToken::new();
                        let it = st.items.get_mut(&id).expect("item checked above");
                        it.host = host.clone();
                        it.status = Status::Resolving;
                        it.task = Some(cancel.clone());
                        running += 1;
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
            }
            return None;
        }
        let mut best: Option<(String, f64)> = None;
        for m in candidates {
            let host = st.host(&hosts::host_of(&m), &self.0.cfg);
            if !host.free(now) {
                continue;
            }
            let score = host.score();
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
            let st = self.lock();
            let it = st.items.get(id).ok_or(Fail::Cancelled)?;
            let pre = it
                .pre
                .clone()
                .filter(|_| it.pre_for.as_deref().is_none_or(|p| p == url));
            (pre, it.name.clone(), self.http_for(host, &st))
        };
        let entry = match pre {
            Some(entry) => entry,
            None => {
                let cx = Cx {
                    http: &http,
                    cfg: &self.0.cfg,
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
            if self.dedupe(&mut st, id) {
                st.items.shift_remove(id);
                self.touch(&mut st);
                return Ok(Outcome::Gone);
            }
            let it = st.items.get_mut(id).expect("still present");
            it.status = Status::Downloading;
            it.error.clear();
            self.touch(&mut st);
        }
        match &entry.exec {
            Some(exec) => self.run_exec(id, &entry, exec, cancel).await?,
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
                let h = st.host(host, &self.0.cfg);
                h.active = h.active.saturating_sub(1);
            }
            log_outcome(id, url, &result);
            match result {
                Ok(Outcome::Gone) => {}
                Ok(Outcome::Done) => {
                    st.host(host, &self.0.cfg).ok += 1;
                    if let Some(it) = st.items.get_mut(id) {
                        it.status = Status::Done;
                        it.retries = 0;
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
                    st.host(host, &self.0.cfg).cooldown = Some(until);
                    if let Some(it) = st.items.get_mut(id) {
                        it.status = Status::Waiting;
                        it.not_before = Some(until);
                        it.error = message;
                    }
                }
                Err(Fail::Transient(message))
                    if st.items.get(id).is_some_and(|it| it.retries < MAX_RETRIES) =>
                {
                    let it = st.items.get_mut(id).expect("checked by the guard");
                    it.retries += 1;
                    let wait = 2u64 << (it.retries - 1);
                    it.status = Status::Waiting;
                    it.not_before = Some(now + Duration::from_secs(wait));
                    it.error = format!("{host}: {message}; resuming in {wait}s");
                }
                Err(Fail::Error(message) | Fail::Transient(message)) => {
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
        st.host(host, &self.0.cfg).fails += 1;
        let it = st.items.get_mut(id)?;
        it.error = format!("{host}: {message}");
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

    /// Deletes the partial files of rows that are gone for good.
    fn discard_parts(&self, ids: impl IntoIterator<Item = String>) {
        for id in ids {
            let _ = std::fs::remove_file(self.part_path(&id));
        }
    }

    fn part_path(&self, id: &str) -> PathBuf {
        self.0
            .download_dir
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
        let package = self
            .lock()
            .items
            .get(id)
            .map(|i| i.package.clone())
            .ok_or(Fail::Cancelled)?;
        let out_dir = if package.is_empty() {
            self.0.download_dir.clone()
        } else {
            self.0.download_dir.join(paths::safe(&package))
        };
        let part = self.part_path(id);
        tokio::fs::create_dir_all(&out_dir).await?;
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
            let mut note = "";
            if matches!(status, 403 | 451) && http.via_proxy {
                // Flag only: never switch to a direct connection on our own.
                self.lock().host(host, &self.0.cfg).blocked = true;
                note = " (host may block VPN IPs)";
            }
            return Err(Fail::Error(format!("HTTP {status}{note}")));
        } else {
            let name = self.lock().items.get(id).and_then(|i| i.name.clone());
            if hosts::content_type(resp.headers()).contains("text/html")
                && !name.as_deref().is_some_and(|n| n.ends_with(".html"))
            {
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
            let streamed: Result<(), Fail> = async {
                loop {
                    let chunk = tokio::select! {
                        biased;
                        _ = cancel.cancelled() => return Err(Fail::Cancelled),
                        c = resp.chunk() => c.map_err(|e| Fail::Transient(e.without_url().to_string()))?,
                    };
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
                    let h = st.host(host, &self.0.cfg);
                    h.bytes += chunk.len() as u64;
                    if let Some(speed) = speed {
                        h.ema = if h.ema == 0.0 { speed } else { h.ema * 0.7 + speed * 0.3 };
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
        self.place(id, &part, &out_dir)
    }

    /// Moves a finished partial file to its final name, never overwriting.
    fn place(&self, id: &str, part: &Path, out_dir: &Path) -> Result<(), Fail> {
        let name = self
            .lock()
            .items
            .get(id)
            .and_then(|i| i.name.clone())
            .unwrap_or_else(|| format!("{id}.bin"));
        let _guard = self.0.rename_lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut target = out_dir.join(paths::safe(&name));
        if target.exists() {
            let stem = target
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            let ext = target
                .extension()
                .map(|e| format!(".{}", e.to_string_lossy()))
                .unwrap_or_default();
            target = target.with_file_name(format!("{stem}.{id}{ext}"));
        }
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
        exec: &[String],
        cancel: &CancellationToken,
    ) -> Result<(), Fail> {
        let package = self
            .lock()
            .items
            .get(id)
            .map(|i| i.package.clone())
            .unwrap_or_default();
        let out = self.0.download_dir.join(paths::safe(if package.is_empty() {
            "mega"
        } else {
            &package
        }));
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

    pub fn item_action(&self, id: &str, action: ItemAction) -> bool {
        let mut st = self.lock();
        let Some(it) = st.items.get_mut(id) else {
            return false;
        };
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

    pub fn set_max_total(&self, n: usize) {
        let n = n.max(1);
        let mut st = self.lock();
        st.max_total = n;
        self.touch(&mut st);
        drop(st);
        self.0.wake.notify_one();
        self.remember(Setting::MaxTotal(n));
    }

    /// Global speed limit in MB/s; 0 removes it.
    pub fn set_rate_mbps(&self, mbps: f64) {
        let mbps = if mbps.is_finite() { mbps.max(0.0) } else { 0.0 };
        self.0.limiter.set_rate(mbps * 1e6);
        let mut st = self.lock();
        self.touch(&mut st);
        drop(st);
        self.remember(Setting::RateLimit(mbps));
    }

    pub fn set_host_limit(&self, host: &str, n: usize) {
        let n = n.clamp(1, config::FALLBACK_LIMIT);
        let mut st = self.lock();
        st.host(host, &self.0.cfg).limit = n;
        self.touch(&mut st);
        drop(st);
        self.0.wake.notify_one();
        self.remember(Setting::HostLimit(host, n));
    }

    pub fn theme(&self) -> Theme {
        *self.0.theme.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn set_theme(&self, theme: Theme) {
        *self.0.theme.lock().unwrap_or_else(|e| e.into_inner()) = theme;
        let mut st = self.lock();
        self.touch(&mut st);
        drop(st);
        self.remember(Setting::Theme(theme));
    }

    /// The settings file, if the engine was started from one.
    pub fn config_path(&self) -> Option<&Path> {
        self.0.config_path.as_deref()
    }

    /// Writes a setting the user changed back to `config.toml`.
    fn remember(&self, setting: Setting<'_>) {
        if let Some(path) = &self.0.config_path
            && let Err(e) = config::write_setting(path, setting)
        {
            tracing::error!("saving {setting:?} to {}: {e}", path.display());
        }
    }

    /// Where a finished item was saved.
    pub fn item_path(&self, id: &str) -> Option<PathBuf> {
        self.lock().items.get(id)?.path.as_ref().map(PathBuf::from)
    }

    /// The user's choice of route for one host. `Direct` leaves the proxy and
    /// uses the real address, so it is never chosen automatically.
    pub fn set_route(&self, host: &str, route: Route) -> std::io::Result<()> {
        let mut st = self.lock();
        st.routes.insert(host.to_string(), route);
        st.host(host, &self.0.cfg).blocked = false;
        let routes = st.routes.clone();
        self.touch(&mut st);
        drop(st);
        store::write(&self.0.data_dir, store::ROUTES, &routes)
    }

    pub fn snapshot(&self) -> Snapshot {
        let st = self.lock();
        let now = Instant::now();
        let has_proxy = self.0.proxy.is_some();
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
                open_urls: it
                    .mirrors
                    .iter()
                    .filter(|m| it.manual_urls.contains(*m))
                    .cloned()
                    .collect(),
                path: it.path.clone(),
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
            })
            .collect();
        hosts.sort_by(|a, b| b.speed.total_cmp(&a.speed));
        Snapshot {
            speed: items.iter().map(|i| i.speed).sum(),
            items,
            hosts,
            paused: st.paused,
            max_total: st.max_total,
            rate: self.0.limiter.rate(),
            proxy: has_proxy,
            theme: self.theme(),
            download_dir: self.0.download_dir.display().to_string(),
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
        let queue: IndexMap<String, _> = {
            let mut st = self.lock();
            if !st.dirty && !force {
                return;
            }
            st.dirty = false;
            st.items
                .iter()
                .map(|(k, v)| (k.clone(), v.saved()))
                .collect()
        };
        if let Err(e) = store::write(&self.0.data_dir, store::QUEUE, &queue) {
            tracing::error!("saving the queue: {e}");
            self.lock().dirty = true;
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
        Ok(Outcome::Gone) | Err(Fail::Cancelled) => {}
        Err(Fail::Wait { seconds, message }) => {
            tracing::info!(item = id, url, seconds, "host asks to wait: {message}")
        }
        Err(Fail::NeedsUser(message)) => {
            tracing::info!(item = id, url, "needs a person: {message}")
        }
        Err(Fail::Transient(message)) => {
            tracing::warn!(item = id, url, "transfer broke: {message}")
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

fn display_name(it: &Item) -> String {
    it.key_name()
        .unwrap_or_else(|| it.mirrors.first().cloned().unwrap_or_default())
}
