//! Settings while everything runs: a change from the GUI is checked, written
//! to `config.toml` and applied at once; an edit of the file itself is picked
//! up within a second and applied the same way.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, atomic::Ordering},
    time::{Duration, Instant, SystemTime},
};

use serde::Serialize;
use serde_json::{Map, Value};

use super::{AUTO_RETRIES, Clients, Engine, Event, State};
use crate::{
    config::{self, ACCOUNTS, Config, FALLBACK_LIMIT, Route, Theme},
    hosts::Http,
    item::{Item, Status},
    paths, store,
};

/// Every setting, for the settings view. Unlike the state stream this
/// carries the account secrets, so it is only sent when asked for.
#[derive(Debug, Serialize)]
pub struct SettingsView {
    pub download_dir: String,
    /// Given on the command line, so it cannot change here.
    pub download_dir_fixed: bool,
    pub max_total: usize,
    pub rate_limit_mbps: f64,
    pub theme: Theme,
    pub watch_clipboard: bool,
    pub recycle_on_remove: bool,
    pub skip_add_dialog: bool,
    pub resume_attempts: u32,
    pub retry_failed_minutes: u64,
    pub proxy: String,
    pub user_agent: String,
    pub default_user_agent: &'static str,
    /// Account keys by their name in the file; empty when unset.
    pub accounts: BTreeMap<&'static str, String>,
    pub megatools: String,
    /// Keys an environment variable set, with its name; read-only here.
    pub from_env: BTreeMap<&'static str, &'static str>,
    pub config_file: Option<String>,
    /// Why the file on disk is not in effect.
    pub error: Option<String>,
}

/// The file's modification time and length, to notice edits.
pub(super) fn stamp(path: &Path) -> Option<(SystemTime, u64)> {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.modified().ok()?, meta.len()))
}

type Write = (Vec<String>, Option<toml_edit::Value>);

fn top(key: &str, value: Option<toml_edit::Value>) -> Write {
    (vec![key.to_string()], value)
}

fn text(value: &Value, key: &str) -> Result<String, String> {
    value
        .as_str()
        .map(|s| s.trim().to_string())
        .ok_or_else(|| format!("{key} must be text"))
}

fn number(value: &Value, key: &str) -> Result<u64, String> {
    value
        .as_u64()
        .ok_or_else(|| format!("{key} must be a whole number"))
}

impl Engine {
    /// Every setting, for the settings view.
    pub fn settings(&self) -> SettingsView {
        let cfg = self.cfg();
        let shown = |v: &Option<String>| v.clone().unwrap_or_default();
        SettingsView {
            download_dir: self.download_dir().display().to_string(),
            download_dir_fixed: self.0.download_dir_fixed,
            max_total: cfg.max_total,
            rate_limit_mbps: cfg.rate_limit_mbps,
            theme: cfg.theme,
            watch_clipboard: cfg.watch_clipboard,
            recycle_on_remove: cfg.recycle_on_remove,
            skip_add_dialog: cfg.skip_add_dialog,
            resume_attempts: cfg.resume_attempts,
            retry_failed_minutes: cfg.retry_failed_minutes,
            proxy: shown(&cfg.proxy),
            user_agent: cfg.user_agent.clone(),
            default_user_agent: config::DEFAULT_USER_AGENT,
            accounts: ACCOUNTS
                .iter()
                .map(|(key, _)| (*key, shown(cfg.account(key).expect("listed account"))))
                .collect(),
            megatools: cfg
                .megatools
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
            from_env: cfg.from_env.clone(),
            config_file: self.0.config_path.as_ref().map(|p| p.display().to_string()),
            error: self.config_error(),
        }
    }

    pub fn theme(&self) -> Theme {
        self.cfg().theme
    }

    pub fn watch_clipboard(&self) -> bool {
        self.cfg().watch_clipboard
    }

    /// The settings file, if the engine was started from one.
    pub fn config_path(&self) -> Option<&Path> {
        self.0.config_path.as_deref()
    }

    fn config_error(&self) -> Option<String> {
        self.0
            .config_error
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn set_config_error(&self, error: Option<String>) {
        let mut slot = self
            .0
            .config_error
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if *slot != error {
            if let Some(e) = &error {
                tracing::warn!("settings not applied: {e}");
            }
            *slot = error;
            drop(slot);
            self.0.config_version.fetch_add(1, Ordering::Relaxed);
            let mut st = self.lock();
            self.touch(&mut st);
        }
    }

    /// Applies settings sent as `{key: value}` with the names the file uses,
    /// and writes each to the file. An empty text removes the key, which
    /// brings back its default.
    pub fn update_settings(&self, changes: &Map<String, Value>) -> Result<(), String> {
        let mut cfg = (*self.cfg()).clone();
        let mut writes = Vec::new();
        for (key, value) in changes {
            if let Some(variable) = cfg.from_env.get(key.as_str()) {
                return Err(format!(
                    "{key} is set by the {variable} environment variable"
                ));
            }
            match key.as_str() {
                "max_total" => {
                    let n = number(value, key)?.clamp(1, 64) as usize;
                    cfg.max_total = n;
                    writes.push(top(key, Some((n as i64).into())));
                }
                "rate_limit_mbps" => {
                    let mbps = value
                        .as_f64()
                        .filter(|v| v.is_finite() && *v >= 0.0)
                        .ok_or("the speed limit must be a number, 0 or more")?;
                    cfg.rate_limit_mbps = mbps;
                    writes.push(top(key, Some(mbps.into())));
                }
                "theme" => {
                    let theme: Theme = serde_json::from_value(value.clone())
                        .map_err(|_| "theme is system, light or dark")?;
                    cfg.theme = theme;
                    writes.push(top(key, Some(theme.as_str().into())));
                }
                "watch_clipboard" => {
                    let on = value.as_bool().ok_or("watch_clipboard is true or false")?;
                    cfg.watch_clipboard = on;
                    writes.push(top(key, Some(on.into())));
                }
                "recycle_on_remove" => {
                    let on = value
                        .as_bool()
                        .ok_or("recycle_on_remove is true or false")?;
                    cfg.recycle_on_remove = on;
                    writes.push(top(key, Some(on.into())));
                }
                "skip_add_dialog" => {
                    let on = value.as_bool().ok_or("skip_add_dialog is true or false")?;
                    cfg.skip_add_dialog = on;
                    writes.push(top(key, Some(on.into())));
                }
                "resume_attempts" => {
                    let n = number(value, key)?.min(20) as u32;
                    cfg.resume_attempts = n;
                    writes.push(top(key, Some(i64::from(n).into())));
                }
                "retry_failed_minutes" => {
                    let n = number(value, key)?.min(24 * 60);
                    cfg.retry_failed_minutes = n;
                    writes.push(top(key, Some((n as i64).into())));
                }
                "proxy" => {
                    let proxy = text(value, key)?;
                    if !proxy.is_empty() {
                        let scheme = proxy.split("://").next().unwrap_or_default();
                        if !matches!(scheme, "socks5" | "socks5h" | "http" | "https")
                            || !proxy.contains("://")
                        {
                            return Err("the proxy is a socks5:// or http:// address".into());
                        }
                        Http::new(Some(&proxy), &cfg.user_agent)?;
                    }
                    writes.push(top(key, (!proxy.is_empty()).then(|| proxy.as_str().into())));
                    cfg.proxy = (!proxy.is_empty()).then_some(proxy);
                }
                "user_agent" => {
                    let agent = text(value, key)?;
                    writes.push(top(key, (!agent.is_empty()).then(|| agent.as_str().into())));
                    cfg.user_agent = if agent.is_empty() {
                        config::DEFAULT_USER_AGENT.into()
                    } else {
                        agent
                    };
                }
                "megatools" => {
                    let exe = text(value, key)?;
                    writes.push(top(key, (!exe.is_empty()).then(|| exe.as_str().into())));
                    cfg.megatools = (!exe.is_empty()).then(|| PathBuf::from(exe));
                }
                "download_dir" => {
                    if self.0.download_dir_fixed {
                        return Err("the download folder was given on the command line".into());
                    }
                    let dir = text(value, key)?;
                    if dir.is_empty() {
                        cfg.download_dir = None;
                        writes.push(top(key, None));
                    } else {
                        let path = PathBuf::from(&dir);
                        usable_folder(&path)?;
                        cfg.download_dir = Some(path);
                        writes.push(top(key, Some(dir.as_str().into())));
                    }
                }
                account if cfg.account(account).is_some() => {
                    let secret = text(value, key)?;
                    *cfg.account_mut(account).expect("checked") =
                        (!secret.is_empty()).then(|| secret.clone());
                    writes.push(top(
                        key,
                        (!secret.is_empty()).then(|| secret.as_str().into()),
                    ));
                }
                _ => return Err(format!("unknown setting {key}")),
            }
        }
        self.write_and_apply(cfg, writes)
    }

    /// Files downloaded from `host` at the same time.
    pub fn set_host_limit(&self, host: &str, n: usize) {
        let n = n.clamp(1, FALLBACK_LIMIT);
        let mut cfg = (*self.cfg()).clone();
        cfg.limits.insert(host.to_string(), n);
        let write = (
            vec!["limits".into(), host.to_string()],
            Some((n as i64).into()),
        );
        self.log_failure(self.write_and_apply(cfg, vec![write]));
        // The user's choice replaces what Haul learned from the host.
        if let Some(h) = self.lock().hosts.get_mut(host) {
            h.cap = None;
        }
    }

    /// The subfolder of the download folder for files from `host`; an empty
    /// one puts them back in the download folder itself. Saved the way it is
    /// used, so the file shows the folder files actually go to.
    pub fn set_host_folder(&self, host: &str, folder: &str) -> Result<(), String> {
        let folder = paths::subfolder(folder)
            .to_string_lossy()
            .replace('\\', "/");
        let mut cfg = (*self.cfg()).clone();
        let key = vec!["folders".into(), host.to_string()];
        let write = if folder.is_empty() {
            cfg.folders.remove(host);
            (key, None)
        } else {
            cfg.folders.insert(host.to_string(), folder.clone());
            (key, Some(folder.as_str().into()))
        };
        self.write_and_apply(cfg, vec![write])
    }

    /// Adds `host` to the end of the preferred hosts, or takes it out; a host
    /// already preferred keeps its place.
    pub fn set_preferred(&self, host: &str, preferred: bool) -> Result<(), String> {
        let mut cfg = (*self.cfg()).clone();
        if preferred == cfg.preference(host).is_some() {
            return Ok(());
        }
        if preferred {
            cfg.preferred_hosts.push(host.to_string());
        } else {
            cfg.preferred_hosts
                .retain(|h| !h.eq_ignore_ascii_case(host));
        }
        let list = (!cfg.preferred_hosts.is_empty()).then(|| {
            toml_edit::Value::Array(cfg.preferred_hosts.iter().map(String::as_str).collect())
        });
        self.write_and_apply(cfg, vec![top("preferred_hosts", list)])
    }

    /// The user's choice of route for one host. `Direct` leaves the proxy and
    /// uses the real address, so it is never chosen automatically.
    pub fn set_route(&self, host: &str, route: Route) -> Result<(), String> {
        let mut cfg = (*self.cfg()).clone();
        cfg.routes.insert(host.to_string(), route);
        let write = (
            vec!["routes".into(), host.to_string()],
            Some(route.as_str().into()),
        );
        self.write_and_apply(cfg, vec![write])?;
        let mut st = self.lock();
        if let Some(h) = st.hosts.get_mut(host) {
            h.blocked = false;
        }
        self.touch(&mut st);
        Ok(())
    }

    pub fn set_max_total(&self, n: usize) {
        self.log_failure(self.update_settings(&one("max_total", n.into())));
    }

    /// Global speed limit in MB/s; 0 removes it.
    pub fn set_rate_mbps(&self, mbps: f64) {
        let mbps = if mbps.is_finite() { mbps.max(0.0) } else { 0.0 };
        self.log_failure(self.update_settings(&one("rate_limit_mbps", mbps.into())));
    }

    pub fn set_theme(&self, theme: Theme) {
        self.log_failure(self.update_settings(&one("theme", theme.as_str().into())));
    }

    fn log_failure(&self, result: Result<(), String>) {
        if let Err(e) = result {
            tracing::error!("{e}");
        }
    }

    fn write_and_apply(&self, cfg: Config, writes: Vec<Write>) -> Result<(), String> {
        if let Some(path) = &self.0.config_path {
            for (key, value) in writes {
                let key: Vec<&str> = key.iter().map(String::as_str).collect();
                config::write_key(path, &key, value)
                    .map_err(|e| format!("saving {}: {e}", path.display()))?;
            }
            *self.0.config_seen.lock().unwrap_or_else(|e| e.into_inner()) = stamp(path);
        }
        self.reconfigure(cfg);
        Ok(())
    }

    /// Puts `new` in effect, changing only what differs.
    fn reconfigure(&self, new: Config) {
        let old = self.cfg();
        if *old == new {
            return;
        }
        if new.proxy() != old.proxy() || new.user_agent != old.user_agent {
            match Clients::new(&new) {
                Ok(clients) => {
                    *self.0.clients.write().unwrap_or_else(|e| e.into_inner()) = Arc::new(clients);
                }
                Err(e) => {
                    self.set_config_error(Some(e));
                    return;
                }
            }
        }
        if new.rate_limit_mbps != old.rate_limit_mbps {
            self.0.limiter.set_rate(new.rate_limit_mbps * 1e6);
        }
        let folder = new.download_dir.clone().unwrap_or_else(paths::download_dir);
        let move_to =
            (!self.0.download_dir_fixed && folder != self.download_dir()).then_some(folder);
        let theme_changed = new.theme != old.theme;
        let retry_changed = new.retry_failed_minutes != old.retry_failed_minutes;
        {
            let mut st = self.lock();
            st.max_total = new.max_total.max(1);
            for (name, host) in st.hosts.iter_mut() {
                host.limit = new.limit_for(name);
            }
            st.routes = new.routes.clone();
            *self.0.cfg.write().unwrap_or_else(|e| e.into_inner()) = Arc::new(new);
            if retry_changed {
                let now = Instant::now();
                for it in st.items.values_mut() {
                    if it.status == Status::Error {
                        self.plan_auto_retry(it, now);
                    }
                }
            }
            self.0.config_version.fetch_add(1, Ordering::Relaxed);
            self.touch(&mut st);
        }
        if theme_changed {
            let _ = self.0.events.send(Event::ThemeChanged(self.theme()));
        }
        if let Some(folder) = move_to {
            tokio::spawn(self.clone().move_downloads(folder));
        }
        self.0.wake.notify_one();
    }

    /// Reloads `config.toml` when it changes on disk. A file that does not
    /// parse leaves the settings in effect alone and says why.
    pub(super) async fn watch_config(self) {
        let Some(path) = self.0.config_path.clone() else {
            return;
        };
        loop {
            tokio::select! {
                _ = self.0.shutdown.cancelled() => return,
                _ = tokio::time::sleep(Duration::from_secs(1)) => {}
            }
            let now = stamp(&path);
            {
                let mut seen = self.0.config_seen.lock().unwrap_or_else(|e| e.into_inner());
                if *seen == now {
                    continue;
                }
                *seen = now;
            }
            match Config::load(&path) {
                Ok(mut cfg) => {
                    cfg.endpoints = self.cfg().endpoints.clone();
                    self.set_config_error(None);
                    tracing::info!("{} changed; applying it", path.display());
                    self.reconfigure(cfg);
                }
                Err(e) => self.set_config_error(Some(format!("{}: {e}", path.display()))),
            }
        }
    }

    /// Moves to a new download folder: running transfers stop, their partial
    /// files move along, and they resume there.
    async fn move_downloads(self, folder: PathBuf) {
        {
            let mut st = self.lock();
            st.moving = true;
            for it in st.items.values() {
                if let Some(task) = &it.task {
                    task.cancel();
                }
            }
            self.touch(&mut st);
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline && self.lock().items.values().any(Item::running) {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        let from = self.download_dir().join(".partial");
        let to = folder.join(".partial");
        let moved = tokio::task::spawn_blocking(move || move_partials(&from, &to)).await;
        match moved {
            Ok(Ok(n)) if n > 0 => tracing::info!("moved {n} partial downloads"),
            Ok(Ok(_)) => {}
            Ok(Err(e)) => tracing::error!("moving partial downloads: {e}"),
            Err(e) => tracing::error!("moving partial downloads: {e}"),
        }
        tracing::info!("downloading to {}", folder.display());
        *self
            .0
            .download_dir
            .write()
            .unwrap_or_else(|e| e.into_inner()) = folder;
        {
            let mut st = self.lock();
            st.moving = false;
            self.touch(&mut st);
        }
        self.0.wake.notify_one();
    }

    /// Schedules the next automatic retry of a failed item, if any is left.
    pub(super) fn plan_auto_retry(&self, it: &mut Item, now: Instant) {
        let minutes = self.cfg().retry_failed_minutes;
        it.retry_at = (minutes > 0 && it.auto_retries < AUTO_RETRIES)
            .then(|| now + Duration::from_secs(minutes * 60));
    }

    /// Queues the failed items whose automatic retry is due.
    pub(super) fn auto_retry(&self, st: &mut State, now: Instant) -> bool {
        let mut any = false;
        for it in st.items.values_mut() {
            if it.status == Status::Error && it.retry_at.is_some_and(|t| now >= t) {
                it.retry_at = None;
                it.auto_retries += 1;
                it.status = Status::Queued;
                it.failed.clear();
                it.retries = 0;
                it.not_before = None;
                it.error.clear();
                any = true;
            }
        }
        any
    }

    /// Route choices saved by older versions in `routes.json` move into
    /// `config.toml`, once.
    pub(super) fn migrate_routes(&self) {
        let Some(path) = self.0.config_path.clone() else {
            return;
        };
        let legacy = self.0.data_dir.join(store::ROUTES);
        let Ok(routes) = store::load_routes(&self.0.data_dir) else {
            return;
        };
        if routes.is_empty() {
            return;
        }
        let mut cfg = (*self.cfg()).clone();
        let mut writes = Vec::new();
        for (host, route) in routes {
            if let std::collections::btree_map::Entry::Vacant(slot) = cfg.routes.entry(host) {
                writes.push((
                    vec!["routes".into(), slot.key().clone()],
                    Some(route.as_str().into()),
                ));
                slot.insert(route);
            }
        }
        match self.write_and_apply(cfg, writes) {
            Ok(()) => {
                let _ = std::fs::rename(&legacy, legacy.with_extension("json.migrated"));
                tracing::info!(
                    "moved the routes in {} to {}",
                    legacy.display(),
                    path.display()
                );
            }
            Err(e) => tracing::error!("{e}"),
        }
    }
}

fn one(key: &str, value: Value) -> Map<String, Value> {
    Map::from_iter([(key.to_string(), value)])
}

/// A folder downloads can go to: absolute, created if needed, and writable.
fn usable_folder(path: &Path) -> Result<(), String> {
    if !path.is_absolute() {
        return Err(format!("{} is not a full path", path.display()));
    }
    std::fs::create_dir_all(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let probe = path.join(".haul-write-test");
    std::fs::write(&probe, b"").map_err(|e| format!("{} is not writable: {e}", path.display()))?;
    let _ = std::fs::remove_file(probe);
    Ok(())
}

/// Moves every partial file from one `.partial` folder to another, copying
/// when the two are on different drives. Returns how many moved.
fn move_partials(from: &Path, to: &Path) -> std::io::Result<usize> {
    let entries = match std::fs::read_dir(from) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e),
    };
    std::fs::create_dir_all(to)?;
    let mut moved = 0;
    for entry in entries {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if std::fs::rename(entry.path(), &target).is_err() {
            std::fs::copy(entry.path(), &target)?;
            std::fs::remove_file(entry.path())?;
        }
        moved += 1;
    }
    let _ = std::fs::remove_dir(from);
    Ok(moved)
}
