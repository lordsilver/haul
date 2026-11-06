//! Settings while everything runs: a change from the GUI is checked, written
//! to `config.toml` and applied at once; an edit of the file itself is picked
//! up within a second and applied the same way.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, atomic::Ordering},
    time::{Duration, Instant, SystemTime},
};

use reqwest::header::HeaderValue;
use serde::Serialize;
use serde_json::{Map, Value};

use super::{AUTO_RETRIES, Clients, Engine, Event, State};
use crate::{
    config::{self, ACCOUNTS, Config, Route, Theme},
    hosts::{self, Http},
    item::{Item, Status},
    paths,
};

/// Every setting, for the settings view. Unlike the state stream this
/// carries the account secrets, so it is only sent when asked for.
#[derive(Debug, Serialize, ts_rs::TS)]
#[ts(export)]
pub struct SettingsView {
    pub download_dir: String,
    /// Given on the command line, so it cannot change here.
    pub download_dir_fixed: bool,
    pub max_total: usize,
    pub rate_limit_mbps: f64,
    pub max_active_gb: f64,
    pub theme: Theme,
    pub watch_clipboard: bool,
    pub check_updates: bool,
    pub recycle_on_remove: bool,
    pub skip_add_dialog: bool,
    pub verify_existing: bool,
    pub auto_extract: bool,
    pub delete_after_extract: bool,
    pub archive_passwords: Vec<String>,
    pub detailed_log: bool,
    pub reconnect: bool,
    pub reconnect_command: String,
    pub nordvpn_servers: usize,
    pub nordvpn_countries: Vec<String>,
    pub resume_attempts: u32,
    pub min_speed_kbps: u32,
    pub min_speed_seconds: u64,
    pub leave_slow_hosts: bool,
    pub retry_failed_minutes: u64,
    pub proxy: String,
    pub proxies: Vec<String>,
    pub proxy_with_direct: bool,
    pub proxy_fresh_connections: bool,
    pub use_proxy: bool,
    pub proxies_on: bool,
    pub nordvpn_on: bool,
    pub user_agent: String,
    pub default_user_agent: &'static str,
    /// Account keys by their name in the file; empty when unset.
    pub accounts: BTreeMap<&'static str, String>,
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

/// A text setting that empty unsets: its value, and the write that sets or
/// removes its key.
fn optional_text(value: &Value, key: &str) -> Result<(Option<String>, Write), String> {
    let given = Some(text(value, key)?).filter(|t| !t.is_empty());
    let write = top(key, given.as_deref().map(Into::into));
    Ok((given, write))
}

fn str_list(list: &[String]) -> toml_edit::Value {
    toml_edit::Value::Array(list.iter().map(String::as_str).collect())
}

type Flag = fn(&mut Config) -> &mut bool;

/// The settings that are simply on or off, by their names in the file.
const FLAGS: &[(&str, Flag)] = &[
    ("watch_clipboard", |c| &mut c.watch_clipboard),
    ("check_updates", |c| &mut c.check_updates),
    ("recycle_on_remove", |c| &mut c.recycle_on_remove),
    ("verify_existing", |c| &mut c.verify_existing),
    ("auto_extract", |c| &mut c.auto_extract),
    ("delete_after_extract", |c| &mut c.delete_after_extract),
    ("reconnect", |c| &mut c.reconnect),
    ("leave_slow_hosts", |c| &mut c.leave_slow_hosts),
    ("detailed_log", |c| &mut c.detailed_log),
    ("skip_add_dialog", |c| &mut c.skip_add_dialog),
    ("proxies_on", |c| &mut c.proxies_on),
    ("nordvpn_on", |c| &mut c.nordvpn_on),
    ("proxy_with_direct", |c| &mut c.proxy_with_direct),
    ("proxy_fresh_connections", |c| {
        &mut c.proxy_fresh_connections
    }),
    ("use_proxy", |c| &mut c.use_proxy),
];

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
            max_active_gb: cfg.max_active_gb,
            theme: cfg.theme,
            watch_clipboard: cfg.watch_clipboard,
            check_updates: cfg.check_updates,
            recycle_on_remove: cfg.recycle_on_remove,
            skip_add_dialog: cfg.skip_add_dialog,
            verify_existing: cfg.verify_existing,
            auto_extract: cfg.auto_extract,
            delete_after_extract: cfg.delete_after_extract,
            archive_passwords: cfg.archive_passwords.clone(),
            detailed_log: cfg.detailed_log,
            reconnect: cfg.reconnect,
            reconnect_command: cfg.reconnect_command.clone().unwrap_or_default(),
            nordvpn_servers: cfg.nordvpn_servers,
            nordvpn_countries: cfg.nordvpn_countries.clone(),
            resume_attempts: cfg.resume_attempts,
            min_speed_kbps: cfg.min_speed_kbps,
            min_speed_seconds: cfg.min_speed_seconds,
            leave_slow_hosts: cfg.leave_slow_hosts,
            retry_failed_minutes: cfg.retry_failed_minutes,
            proxy: shown(&cfg.proxy),
            proxies: cfg.proxies.clone(),
            proxy_with_direct: cfg.proxy_with_direct,
            proxy_fresh_connections: cfg.proxy_fresh_connections,
            use_proxy: cfg.use_proxy,
            proxies_on: cfg.proxies_on,
            nordvpn_on: cfg.nordvpn_on,
            user_agent: cfg.user_agent.clone(),
            default_user_agent: config::DEFAULT_USER_AGENT,
            accounts: ACCOUNTS
                .iter()
                .map(|(key, _)| (*key, shown(cfg.account(key).expect("listed account"))))
                .collect(),
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
                "max_active_gb" => {
                    let gb = value
                        .as_f64()
                        .filter(|v| v.is_finite() && *v >= 0.0)
                        .ok_or("the size limit must be a number, 0 or more")?;
                    cfg.max_active_gb = gb;
                    writes.push(top(key, Some(gb.into())));
                }
                "theme" => {
                    let theme: Theme = serde_json::from_value(value.clone())
                        .map_err(|_| "theme is system, light or dark")?;
                    cfg.theme = theme;
                    writes.push(top(key, Some(theme.as_str().into())));
                }
                "archive_passwords" => {
                    // A list, or text with one password per line. A password
                    // keeps its spaces: only empty lines go.
                    let list: Vec<String> = match value.as_array() {
                        Some(items) => items
                            .iter()
                            .map(|v| v.as_str().map(str::to_string))
                            .collect::<Option<_>>()
                            .ok_or("archive passwords are text")?,
                        None => text(value, key)?.lines().map(str::to_string).collect(),
                    };
                    let mut list: Vec<String> = list
                        .into_iter()
                        .map(|p| p.trim_end_matches('\r').to_string())
                        .filter(|p| !p.is_empty())
                        .collect();
                    let mut seen = std::collections::HashSet::new();
                    list.retain(|p| seen.insert(p.clone()));
                    writes.push(top(key, Some(str_list(&list))));
                    cfg.archive_passwords = list;
                }
                "nordvpn_servers" => {
                    let n = value
                        .as_u64()
                        .filter(|n| *n <= 100)
                        .ok_or("nordvpn_servers is a number from 0 to 100")?;
                    cfg.nordvpn_servers = n as usize;
                    writes.push(top(key, Some((n as i64).into())));
                }
                "nordvpn_countries" => {
                    let list: Vec<String> = value
                        .as_array()
                        .ok_or("nordvpn_countries is a list")?
                        .iter()
                        .map(|v| v.as_str().map(|s| s.trim().to_string()))
                        .collect::<Option<_>>()
                        .ok_or("countries are text")?;
                    writes.push(top(key, Some(str_list(&list))));
                    cfg.nordvpn_countries = list;
                }
                "reconnect_command" => {
                    let (command, write) = optional_text(value, key)?;
                    writes.push(write);
                    cfg.reconnect_command = command;
                }
                "resume_attempts" => {
                    let n = number(value, key)?.min(20) as u32;
                    cfg.resume_attempts = n;
                    writes.push(top(key, Some(i64::from(n).into())));
                }
                "min_speed_kbps" => {
                    let n = number(value, key)?.min(1 << 20) as u32;
                    cfg.min_speed_kbps = n;
                    writes.push(top(key, Some(i64::from(n).into())));
                }
                "min_speed_seconds" => {
                    let n = number(value, key)?.clamp(1, 3600);
                    cfg.min_speed_seconds = n;
                    writes.push(top(key, Some((n as i64).into())));
                }
                "retry_failed_minutes" => {
                    let n = number(value, key)?.min(24 * 60);
                    cfg.retry_failed_minutes = n;
                    writes.push(top(key, Some((n as i64).into())));
                }
                "proxy" => {
                    let (proxy, write) = optional_text(value, key)?;
                    if let Some(proxy) = &proxy {
                        check_proxy(proxy, &cfg.user_agent)?;
                    }
                    writes.push(write);
                    cfg.proxy = proxy;
                }
                "proxies" => {
                    // A list, or text with one proxy per line.
                    let list: Vec<String> = match value.as_array() {
                        Some(items) => items
                            .iter()
                            .map(|v| v.as_str().map(str::trim).map(str::to_string))
                            .collect::<Option<_>>()
                            .ok_or("proxies are text")?,
                        None => text(value, key)?
                            .lines()
                            .map(|l| l.trim().to_string())
                            .collect(),
                    };
                    let list: Vec<String> = list.into_iter().filter(|p| !p.is_empty()).collect();
                    for proxy in &list {
                        check_proxy(proxy, &cfg.user_agent)?;
                    }
                    writes.push(top(key, Some(str_list(&list))));
                    cfg.proxies = list;
                }
                "user_agent" => {
                    let (agent, write) = optional_text(value, key)?;
                    if agent
                        .as_deref()
                        .is_some_and(|a| HeaderValue::from_str(a).is_err())
                    {
                        return Err("the user agent must be text on one line".into());
                    }
                    writes.push(write);
                    cfg.user_agent = agent.unwrap_or_else(|| config::DEFAULT_USER_AGENT.into());
                }
                "download_dir" => {
                    if self.0.download_dir_fixed {
                        return Err("the download folder was given on the command line".into());
                    }
                    let (dir, write) = optional_text(value, key)?;
                    if let Some(dir) = &dir {
                        usable_folder(Path::new(dir))?;
                    }
                    writes.push(write);
                    cfg.download_dir = dir.map(PathBuf::from);
                }
                account if cfg.account(account).is_some() => {
                    let (secret, write) = optional_text(value, key)?;
                    *cfg.account_mut(account).expect("checked") = secret;
                    writes.push(write);
                }
                _ => {
                    let Some((_, flag)) = FLAGS.iter().find(|(name, _)| name == key) else {
                        return Err(format!("unknown setting {key}"));
                    };
                    let on = value
                        .as_bool()
                        .ok_or_else(|| format!("{key} is true or false"))?;
                    *flag(&mut cfg) = on;
                    writes.push(top(key, Some(on.into())));
                }
            }
        }
        // Redialing on its own is switched on only when a redial can work.
        // Checked here, against the whole change: a login set in the same
        // request counts.
        if cfg.reconnect
            && changes.contains_key("reconnect")
            && let Some(why) = self.redial_blocker(&cfg)
        {
            return Err(format!("cannot get a new IP on its own: {why}"));
        }
        self.write_and_apply(cfg, writes)
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
            // A host the user prefers is one they want used.
            if cfg.is_disabled(host) {
                cfg.disabled_hosts.retain(|h| !h.eq_ignore_ascii_case(host));
                self.write_hosts(cfg)?;
                self.retry_turned_on(host);
                return Ok(());
            }
        } else {
            cfg.preferred_hosts
                .retain(|h| !h.eq_ignore_ascii_case(host));
        }
        self.write_preferred(cfg)
    }

    /// The preferred hosts, all of them, in this order: how the list is
    /// reordered. A blank name, or a host named twice (in any case), leaves
    /// the settings as they were.
    pub fn set_preferred_hosts(&self, hosts: &[String]) -> Result<(), String> {
        let mut list: Vec<String> = Vec::with_capacity(hosts.len());
        for host in hosts {
            let host = host.trim();
            if host.is_empty() {
                return Err("a preferred host needs a name".into());
            }
            if list.iter().any(|h| h.eq_ignore_ascii_case(host)) {
                return Err(format!("{host} is in the list twice"));
            }
            list.push(host.to_string());
        }
        let mut cfg = (*self.cfg()).clone();
        if cfg.preferred_hosts == list {
            return Ok(());
        }
        cfg.preferred_hosts = list;
        self.write_preferred(cfg)
    }

    /// Turns `host` off (its links are never tried, and it leaves the
    /// preferred hosts) or back on, which starts again the downloads that
    /// failed for having only turned-off hosts.
    pub fn set_disabled(&self, host: &str, disabled: bool) -> Result<(), String> {
        let host = host.trim();
        if host.is_empty() {
            return Err("a host needs a name".into());
        }
        let mut cfg = (*self.cfg()).clone();
        if disabled == cfg.is_off(host) {
            return Ok(());
        }
        if disabled {
            cfg.disabled_hosts.push(host.to_ascii_lowercase());
            cfg.preferred_hosts
                .retain(|h| !h.eq_ignore_ascii_case(host));
        } else {
            cfg.disabled_hosts.retain(|h| !h.eq_ignore_ascii_case(host));
        }
        self.write_hosts(cfg)?;
        if !disabled {
            self.retry_turned_on(host);
        }
        Ok(())
    }

    /// Puts `cfg` in effect with the host lists written whole.
    fn write_hosts(&self, cfg: Config) -> Result<(), String> {
        let writes = vec![
            top("preferred_hosts", Some(str_list(&cfg.preferred_hosts))),
            top("disabled_hosts", Some(str_list(&cfg.disabled_hosts))),
            top("accountless_hosts", Some(str_list(&cfg.accountless_hosts))),
        ];
        self.write_and_apply(cfg, writes)
    }

    /// Puts `cfg` in effect with its preferred hosts written whole. Written
    /// even when empty: a key left out would bring the defaults
    /// ([`config::DEFAULT_PREFERRED`]) back on the next load.
    fn write_preferred(&self, cfg: Config) -> Result<(), String> {
        let list = str_list(&cfg.preferred_hosts);
        self.write_and_apply(cfg, vec![top("preferred_hosts", Some(list))])
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
        if new.detailed_log != old.detailed_log {
            crate::logging::set_detailed(new.detailed_log);
        }
        // The switch also takes the VPN servers in or out, which the pool of
        // the settings does not show.
        if new.proxy_pool() != old.proxy_pool()
            || new.use_proxy != old.use_proxy
            || new.proxy_fresh_connections != old.proxy_fresh_connections
            || new.user_agent != old.user_agent
        {
            match Clients::new(&new, &self.service_proxies()) {
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
        // Hosts an account was just set for, used like any other from now on:
        // what failed on them goes again.
        let unlocked: Vec<&str> = hosts::all()
            .filter(|k| k.last_resort(&old, false) && !k.last_resort(&new, false))
            .flat_map(|k| k.names.iter().copied())
            .collect();
        let retry_changed = new.retry_failed_minutes != old.retry_failed_minutes;
        {
            let mut st = self.lock();
            st.max_total = new.max_total.max(1);
            st.routes = new.routes.clone();
            *self.0.cfg.write().unwrap_or_else(|e| e.into_inner()) = Arc::new(new);
            if retry_changed {
                let now = Instant::now();
                for it in st.items.values_mut() {
                    if it.status == Status::Error {
                        self.plan_auto_retry(it, now);
                    }
                }
                let asking: Vec<String> = st
                    .items
                    .values()
                    .filter(|it| it.status == Status::Captcha && it.page_wants_person)
                    .map(|it| it.id.clone())
                    .collect();
                for id in asking {
                    let at = self.page_retry_at(&st, &st.items[&id], now);
                    if let Some(it) = st.items.get_mut(&id) {
                        it.retry_at = at;
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
        for host in unlocked {
            self.retry_turned_on(host);
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
            if now.is_none() {
                // Deleted, or an editor that removes the old file before it
                // writes the new one: the defaults would drop the proxy and
                // move the download folder, so what is in effect stays.
                tracing::warn!("{} is gone; the settings in effect stay", path.display());
                continue;
            }
            match Config::load(&path) {
                Ok(mut cfg) => {
                    cfg.endpoints = self.cfg().endpoints.clone();
                    cfg.throttling = self.cfg().throttling.clone();
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
        // Every link is gone for good: another try would find the same.
        let gone = it.mirrors.iter().all(|m| it.gone.contains(m));
        it.retry_at = (minutes > 0 && it.auto_retries < AUTO_RETRIES && !gone)
            .then(|| now + Duration::from_secs(minutes * 60));
    }

    /// The pages of row `it` under Needs you that the desktop app could pass
    /// again by itself: not one marked gone, nor on a host that is off.
    fn pages_to_pass<'a>(&self, st: &State, it: &'a Item) -> Vec<&'a String> {
        let cfg = self.cfg();
        it.manual_urls
            .iter()
            .filter(|m| {
                st.passes(m)
                    && !cfg.is_off(&hosts::host_of(m))
                    && !it.failed.contains(*m)
                    && !it.gone.contains(*m)
            })
            .collect()
    }

    /// When row `it`, under Needs you, has its page passed again, on the same
    /// timer and limit as a failed download: never when no such try is left,
    /// or no page could pass without a person.
    pub(super) fn page_retry_at(&self, st: &State, it: &Item, now: Instant) -> Option<Instant> {
        let minutes = self.cfg().retry_failed_minutes;
        (minutes > 0
            && it.auto_retries < AUTO_RETRIES
            && self.pages_pass()
            && !self.pages_to_pass(st, it).is_empty())
        .then(|| now + Duration::from_secs(minutes * 60))
    }

    /// Queues the failed items whose automatic retry is due, and passes the
    /// pages of those under Needs you whose try is.
    pub(super) fn auto_retry(&self, st: &mut State, now: Instant) -> bool {
        let mut any = self.retry_pages(st, now);
        let mut unwrap = Vec::new();
        for it in st.items.values_mut() {
            if it.status == Status::Error && it.retry_at.is_some_and(|t| now >= t) {
                it.retry_at = None;
                it.auto_retries += 1;
                tracing::info!(item = %it.id, "trying again on its own ({} of {AUTO_RETRIES}) after: {}", it.auto_retries, it.error);
                it.status = Status::Queued;
                it.failed.clear();
                it.retries = 0;
                it.not_before = None;
                it.clear_error();
                // A wrapper is unwrapped at once, as when it was added: its
                // files only wait for the user, so it takes no download's
                // turn and waits for none. What this cannot settle is left to
                // its turn in the queue, as then.
                if it.wrapper {
                    it.probed = false;
                    unwrap.push(it.id.clone());
                }
                any = true;
            }
        }
        for id in unwrap {
            self.spawn_probe(id);
        }
        any
    }

    /// The rows under Needs you whose next try is due go back to passing
    /// their page, out of sight and telling nobody; `passing.rs` in the
    /// desktop app opens it in its turn. One whose page's host still asks
    /// the address to wait waits as long, and one with no page left that
    /// could pass by itself has no more tries.
    fn retry_pages(&self, st: &mut State, now: Instant) -> bool {
        let due: Vec<String> = st
            .items
            .values()
            .filter(|it| {
                it.status == Status::Captcha
                    && it.page_wants_person
                    && it.task.is_none()
                    && it.retry_at.is_some_and(|t| now >= t)
            })
            .map(|it| it.id.clone())
            .collect();
        for id in &due {
            let it = &st.items[id];
            let pages = self.pages_to_pass(st, it);
            let usable = self.pages_pass() && !pages.is_empty();
            let held = pages
                .iter()
                .map(|m| self.pass_held_for(st, it, m, now))
                .collect::<Option<Vec<Instant>>>()
                .and_then(|untils| untils.into_iter().min())
                .filter(|_| usable);
            let it = st.items.get_mut(id).expect("listed above");
            if let Some(until) = held {
                it.retry_at = Some(until);
            } else if usable {
                it.retry_at = None;
                it.auto_retries += 1;
                it.page_retrying = true;
                it.page_wants_person = false;
                tracing::info!(item = %id, "passing its page again on its own ({} of {AUTO_RETRIES})", it.auto_retries);
            } else {
                it.retry_at = None;
            }
        }
        !due.is_empty()
    }
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

/// A proxy address Haul can use: `socks5://`, `socks5h://`, `http://` or
/// `https://`, with credentials if it needs them.
fn check_proxy(proxy: &str, user_agent: &str) -> Result<(), String> {
    let scheme = proxy.split("://").next().unwrap_or_default();
    if !matches!(scheme, "socks5" | "socks5h" | "http" | "https") || !proxy.contains("://") {
        return Err(format!(
            "{} is not a socks5:// or http:// address",
            redact_proxy(proxy)
        ));
    }
    Http::new(Some(proxy), user_agent).map(|_| ())
}

/// A proxy address without its user and password, for messages.
fn redact_proxy(proxy: &str) -> String {
    match proxy.rsplit_once('@') {
        Some((head, host)) => {
            let scheme = head.split("://").next().unwrap_or_default();
            format!("{scheme}://…@{host}")
        }
        None => proxy.to_string(),
    }
}
