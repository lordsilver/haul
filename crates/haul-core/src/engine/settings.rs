//! Settings while everything runs: a change from the GUI is checked, written
//! to `config.toml` and applied at once; an edit of the file itself is picked
//! up within a second and applied the same way.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant, SystemTime},
};

use serde::Serialize;
use serde_json::{Map, Value};

use super::{AUTO_RETRIES, Engine, Event, State};
use crate::{
    config::{self, Config, Route, Theme},
    hosts::{self, Http},
    item::{Item, PageTurn, Status},
    paths,
};

/// Every setting, for the settings view, as the file names them. Unlike the
/// state stream this carries the account secrets, so it is only sent when
/// asked for.
#[derive(Debug, Serialize, ts_rs::TS)]
#[ts(export)]
pub struct SettingsView {
    #[serde(flatten)]
    pub config: Config,
    /// Where downloads go now.
    pub download_dir: String,
    /// Given on the command line, so it cannot change here.
    pub download_dir_fixed: bool,
    /// Keys an environment variable set, with its name; read-only here.
    pub from_env: BTreeMap<String, String>,
    pub config_file: Option<String>,
    /// Why the file on disk is not in effect.
    pub error: Option<String>,
    /// The settings file is in the data folder, as it is unless the command
    /// line named another one.
    pub config_inside_data_dir: bool,
    /// The bounds of each whole-number setting, by its name in the file: what
    /// a field allows is what the engine keeps a value within.
    pub limits: BTreeMap<String, Limit>,
}

/// The least and most a whole-number setting takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ts_rs::TS)]
#[ts(export)]
pub struct Limit {
    pub least: u64,
    pub most: u64,
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

/// A list sent as a list of texts or as text with one entry per line; empty
/// entries go, and with `trim` the spaces around each.
fn text_list(value: &Value, key: &str, trim: bool) -> Result<Vec<String>, String> {
    let list: Vec<String> = match value.as_array() {
        Some(items) => items
            .iter()
            .map(|v| v.as_str().map(str::to_string))
            .collect::<Option<_>>()
            .ok_or_else(|| format!("{key} is a list of texts"))?,
        None => text(value, key)?.lines().map(str::to_string).collect(),
    };
    Ok(list
        .into_iter()
        .map(|p| {
            if trim {
                p.trim().to_string()
            } else {
                p.trim_end_matches('\r').to_string()
            }
        })
        .filter(|p| !p.is_empty())
        .collect())
}

/// The whole-number settings: name, least, most, and where it goes.
type Number = (&'static str, u64, u64, fn(&mut Config, u64));

const NUMBERS: &[Number] = &[
    ("max_total", 1, 64, |c, n| c.max_total = n as usize),
    ("resume_attempts", 0, 20, |c, n| {
        c.resume_attempts = n as u32
    }),
    ("min_speed_kbps", 0, 1 << 20, |c, n| {
        c.min_speed_kbps = n as u32
    }),
    ("min_speed_seconds", 1, 3600, |c, n| c.min_speed_seconds = n),
    ("retry_failed_minutes", 0, 24 * 60, |c, n| {
        c.retry_failed_minutes = n
    }),
];

/// The settings that are a number 0 or more: name, what the message calls
/// it, and where it goes.
type Decimal = (&'static str, &'static str, fn(&mut Config, f64));

const DECIMALS: &[Decimal] = &[
    ("rate_limit_mbps", "the speed limit", |c, v| {
        c.rate_limit_mbps = v
    }),
    ("max_active_gb", "the size limit", |c, v| {
        c.max_active_gb = v
    }),
];

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
    ("live_debug", |c| &mut c.live_debug),
    ("skip_add_dialog", |c| &mut c.skip_add_dialog),
    ("proxies_on", |c| &mut c.proxies_on),
    ("nordvpn_on", |c| &mut c.nordvpn_on),
    ("proxy_with_direct", |c| &mut c.proxy_with_direct),
    ("proxy_fresh_connections", |c| {
        &mut c.proxy_fresh_connections
    }),
    ("use_proxy", |c| &mut c.use_proxy),
];

/// Why a setting was not changed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SettingError {
    /// The request asked for something that cannot be: the asker's to fix.
    #[error("{0}")]
    Invalid(String),
    /// The settings file could not be written: this computer's trouble.
    #[error("{0}")]
    Write(String),
}

impl From<String> for SettingError {
    fn from(e: String) -> Self {
        Self::Invalid(e)
    }
}

impl From<&str> for SettingError {
    fn from(e: &str) -> Self {
        Self::Invalid(e.to_string())
    }
}

impl Engine {
    /// Every setting, for the settings view.
    pub fn settings(&self) -> SettingsView {
        let cfg = self.cfg();
        let config_inside_data_dir = self
            .0
            .config_path
            .as_deref()
            .is_some_and(|p| p.parent() == Some(self.0.data_dir.as_path()));
        SettingsView {
            config_inside_data_dir,
            limits: NUMBERS
                .iter()
                .map(|(name, least, most, _)| {
                    (
                        (*name).to_string(),
                        Limit {
                            least: *least,
                            most: *most,
                        },
                    )
                })
                .collect(),
            download_dir: self.download_dir().display().to_string(),
            download_dir_fixed: self.0.download_dir_fixed,
            from_env: cfg.from_env.clone(),
            config_file: self.0.config_path.as_ref().map(|p| p.display().to_string()),
            error: self.config_error(),
            config: (*cfg).clone(),
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
        crate::lock::held(&self.0.config_error).clone()
    }

    fn set_config_error(&self, error: Option<String>) {
        let mut slot = crate::lock::held(&self.0.config_error);
        if *slot != error {
            if let Some(e) = &error {
                tracing::warn!("settings not applied: {e}");
            }
            *slot = error;
            drop(slot);
            self.0.config_version.send_modify(|v| *v += 1);
            let mut st = self.lock();
            self.touch(&mut st);
        }
    }

    /// Applies settings sent as `{key: value}` with the names the file uses,
    /// and writes each to the file. An empty text removes the key, which
    /// brings back its default.
    pub fn update_settings(&self, changes: &Map<String, Value>) -> Result<(), SettingError> {
        let _settings = self.settings_held();
        let mut cfg = (*self.cfg()).clone();
        let mut writes = Vec::new();
        // Created and tried only once every other key has passed.
        let mut new_folder = None;
        for (key, value) in changes {
            if let Some(variable) = cfg.from_env.get(key.as_str()) {
                return Err(SettingError::Invalid(format!(
                    "{key} is set by the {variable} environment variable"
                )));
            }
            if let Some((_, least, most, set)) = NUMBERS.iter().find(|n| n.0 == key) {
                let n = number(value, key)?.clamp(*least, *most);
                set(&mut cfg, n);
                writes.push(top(key, Some((n as i64).into())));
                continue;
            }
            if let Some((_, what, set)) = DECIMALS.iter().find(|d| d.0 == key) {
                let v = value
                    .as_f64()
                    .filter(|v| v.is_finite() && *v >= 0.0)
                    .ok_or_else(|| format!("{what} must be a number, 0 or more"))?;
                set(&mut cfg, v);
                writes.push(top(key, Some(v.into())));
                continue;
            }
            match key.as_str() {
                "theme" => {
                    let theme: Theme = serde_json::from_value(value.clone())
                        .map_err(|_| "theme is system, light or dark")?;
                    cfg.theme = theme;
                    writes.push(top(key, Some(theme.as_str().into())));
                }
                "archive_passwords" => {
                    // A password keeps its spaces: only empty lines go.
                    let mut list = text_list(value, key, false)?;
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
                    let list = text_list(value, key, true)?;
                    writes.push(top(key, Some(str_list(&list))));
                    cfg.nordvpn_countries = list;
                }
                "reconnect_command" => {
                    let (command, write) = optional_text(value, key)?;
                    writes.push(write);
                    cfg.reconnect_command = command;
                }
                "proxy" => {
                    let (proxy, write) = optional_text(value, key)?;
                    if let Some(proxy) = &proxy {
                        check_proxy(proxy)?;
                    }
                    writes.push(write);
                    cfg.proxy = proxy;
                }
                "proxies" => {
                    let list = text_list(value, key, true)?;
                    for proxy in &list {
                        check_proxy(proxy)?;
                    }
                    writes.push(top(key, Some(str_list(&list))));
                    cfg.proxies = list;
                }
                "download_dir" => {
                    if self.0.download_dir_fixed {
                        return Err("the download folder was given on the command line".into());
                    }
                    let (dir, write) = optional_text(value, key)?;
                    if let Some(dir) = &dir
                        && !Path::new(dir).is_absolute()
                    {
                        return Err(SettingError::Invalid(format!("{dir} is not a full path")));
                    }
                    new_folder.clone_from(&dir);
                    writes.push(write);
                    cfg.download_dir = dir.map(PathBuf::from);
                }
                account if config::account_keys().any(|k| k == account) => {
                    let secret = Some(text(value, key)?).filter(|t| !t.is_empty());
                    let write = vec!["accounts".to_string(), key.clone()];
                    match &secret {
                        Some(s) => cfg.accounts.insert(key.clone(), s.clone()),
                        None => cfg.accounts.remove(key),
                    };
                    writes.push((write, secret.map(Into::into)));
                }
                _ => {
                    let Some((_, flag)) = FLAGS.iter().find(|(name, _)| name == key) else {
                        return Err(SettingError::Invalid(format!("unknown setting {key}")));
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
            return Err(SettingError::Invalid(format!(
                "cannot get a new IP on its own: {why}"
            )));
        }
        if let Some(dir) = new_folder {
            usable_folder(Path::new(&dir))?;
        }
        self.write_and_apply(cfg, writes)
    }

    /// The subfolder of the download folder for files from `host`; an empty
    /// one puts them back in the download folder itself. Saved the way it is
    /// used, so the file shows the folder files actually go to.
    pub fn set_host_folder(&self, host: &str, folder: &str) -> Result<(), SettingError> {
        let folder = paths::subfolder(folder)
            .to_string_lossy()
            .replace('\\', "/");
        let _settings = self.settings_held();
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
    pub fn set_preferred(&self, host: &str, preferred: bool) -> Result<(), SettingError> {
        let _settings = self.settings_held();
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
    pub fn set_preferred_hosts(&self, hosts: &[String]) -> Result<(), SettingError> {
        let mut list: Vec<String> = Vec::with_capacity(hosts.len());
        for host in hosts {
            let host = host.trim();
            if host.is_empty() {
                return Err("a preferred host needs a name".into());
            }
            if list.iter().any(|h| h.eq_ignore_ascii_case(host)) {
                return Err(SettingError::Invalid(format!(
                    "{host} is in the list twice"
                )));
            }
            list.push(host.to_string());
        }
        let _settings = self.settings_held();
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
    pub fn set_disabled(&self, host: &str, disabled: bool) -> Result<(), SettingError> {
        let host = host.trim();
        if host.is_empty() {
            return Err("a host needs a name".into());
        }
        let _settings = self.settings_held();
        let mut cfg = (*self.cfg()).clone();
        if disabled == cfg.is_disabled(host) {
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

    /// Has `host`, which gives no decent download without its account, used
    /// like any other host without one (`accountless_hosts`), or only when no
    /// other host has the file again. Switched on, what failed for having
    /// only such hosts goes again.
    pub fn set_accountless(&self, host: &str, on: bool) -> Result<(), SettingError> {
        let host = host.trim();
        if hosts::known(host).is_none_or(|k| !k.needs_account) {
            return Err(SettingError::Invalid(format!(
                "{host} is used like any other host already"
            )));
        }
        let _settings = self.settings_held();
        let mut cfg = (*self.cfg()).clone();
        if on == cfg.is_accountless(host) {
            return Ok(());
        }
        if on {
            cfg.accountless_hosts.push(host.to_ascii_lowercase());
        } else {
            cfg.accountless_hosts
                .retain(|h| !h.eq_ignore_ascii_case(host));
        }
        self.write_hosts(cfg)?;
        if on {
            self.retry_turned_on(host);
        }
        Ok(())
    }

    /// Puts `cfg` in effect with the host lists written whole.
    fn write_hosts(&self, cfg: Config) -> Result<(), SettingError> {
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
    fn write_preferred(&self, cfg: Config) -> Result<(), SettingError> {
        let list = str_list(&cfg.preferred_hosts);
        self.write_and_apply(cfg, vec![top("preferred_hosts", Some(list))])
    }

    /// The user's choice of route for one host. `Direct` leaves the proxy and
    /// uses the real address, so it is never chosen automatically.
    pub fn set_route(&self, host: &str, route: Route) -> Result<(), SettingError> {
        let _settings = self.settings_held();
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

    /// Held from reading the settings in effect to putting the changed ones
    /// in effect, so two changes at once cannot each start from the old
    /// settings and leave the file and memory disagreeing.
    fn settings_held(&self) -> std::sync::MutexGuard<'_, ()> {
        crate::lock::held(&self.0.settings_lock)
    }

    /// Writes the keys to the file in one go and puts `cfg` in effect. The
    /// caller holds [`Engine::settings_held`].
    fn write_and_apply(&self, cfg: Config, writes: Vec<Write>) -> Result<(), SettingError> {
        if let Some(path) = &self.0.config_path {
            config::write_keys(path, &writes)
                .map_err(|e| SettingError::Write(format!("saving {}: {e}", path.display())))?;
            *crate::lock::held(&self.0.config_seen) = stamp(path);
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
        if new.live_debug != old.live_debug {
            crate::debug::set_live(new.live_debug);
        }
        // The switch also takes the VPN servers in or out, which the pool of
        // the settings does not show.
        if (new.proxy_pool() != old.proxy_pool()
            || new.use_proxy != old.use_proxy
            || new.proxy_fresh_connections != old.proxy_fresh_connections)
            && let Err(e) = self.set_clients(&new, &self.service_proxies())
        {
            self.set_config_error(Some(e));
            return;
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
            *crate::lock::write(&self.0.cfg) = Arc::new(new);
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
                    .filter(|it| it.status == Status::Captcha && it.page_wants_person())
                    .map(|it| it.id.clone())
                    .collect();
                for id in asking {
                    let at = self.page_retry_at(&st, &st.items[&id], now);
                    if let Some(it) = st.items.get_mut(&id) {
                        it.retry_at = at;
                    }
                }
            }
            self.0.config_version.send_modify(|v| *v += 1);
            self.touch(&mut st);
        }
        if theme_changed {
            self.emit([Event::ThemeChanged(self.theme())]);
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
            if !self.idle(Duration::from_secs(1)).await {
                return;
            }
            // A change from the app and one in the file go one at a time.
            let _settings = self.settings_held();
            let now = stamp(&path);
            {
                let mut seen = crate::lock::held(&self.0.config_seen);
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
        // One move at a time: a second waits, and starts from where the
        // first left the files.
        let _moving = self.0.move_lock.lock().await;
        if folder == self.download_dir() {
            return;
        }
        {
            let mut st = self.lock();
            st.moving = true;
            self.touch(&mut st);
        }
        self.stop_all(Duration::from_secs(10)).await;
        let from = self.download_dir().join(".partial");
        let to = folder.join(".partial");
        let moved = tokio::task::spawn_blocking(move || move_partials(&from, &to))
            .await
            .unwrap_or_else(|e| Err(e.to_string()));
        match moved {
            Ok(n) => {
                if n > 0 {
                    tracing::info!("moved {n} partial downloads");
                }
                tracing::info!("downloading to {}", folder.display());
                *crate::lock::write(&self.0.download_dir) = folder;
            }
            // What could not move would be left behind, its download
            // starting over: the downloads stay where they are.
            Err(why) => {
                let old = self.download_dir();
                tracing::error!("moving partial downloads: {why}");
                self.set_config_error(Some(format!(
                    "the partial downloads could not move to {}: {why}; downloads stay in {}",
                    folder.display(),
                    old.display()
                )));
            }
        }
        {
            let mut st = self.lock();
            st.moving = false;
            self.touch(&mut st);
        }
        self.0.wake.notify_one();
    }

    /// Schedules the next automatic retry of a failed item, if any is left.
    pub(super) fn plan_auto_retry(&self, it: &mut Item, now: Instant) {
        // Every link is gone for good: another try would find the same.
        let gone = it.mirrors.iter().all(|m| it.gone.contains(m.as_str()));
        it.retry_at = self.next_try(it, now).filter(|_| !gone);
    }

    /// When row `it` may be tried again on its own, if it has a try left and
    /// the setting is on.
    fn next_try(&self, it: &Item, now: Instant) -> Option<Instant> {
        let minutes = self.cfg().retry_failed_minutes;
        (minutes > 0 && it.auto_retries < AUTO_RETRIES)
            .then(|| now + Duration::from_secs(minutes * 60))
    }

    /// The pages of row `it` under Needs you that the desktop app could pass
    /// again by itself: not one marked gone, nor on a host that is off.
    fn pages_to_pass<'a>(&self, st: &State, it: &'a Item) -> Vec<&'a String> {
        let cfg = self.cfg();
        it.manual_urls
            .iter()
            .filter(|m| {
                st.passes(m)
                    && !cfg.is_disabled(&hosts::host_of(m))
                    && !it.failed.contains(*m)
                    && !it.gone.contains(*m)
            })
            .collect()
    }

    /// When row `it`, under Needs you, has its page passed again, on the same
    /// timer and limit as a failed download: never when no such try is left,
    /// or no page could pass without a person.
    pub(super) fn page_retry_at(&self, st: &State, it: &Item, now: Instant) -> Option<Instant> {
        self.next_try(it, now)
            .filter(|_| self.pages_pass() && !self.pages_to_pass(st, it).is_empty())
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
                    && it.page_wants_person()
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
                it.page = PageTurn::OnItsOwn;
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
fn move_partials(from: &Path, to: &Path) -> Result<usize, String> {
    let entries = match std::fs::read_dir(from) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(format!("{}: {e}", from.display())),
    };
    let files: Vec<_> = entries
        .flatten()
        .filter(|e| e.file_name() != super::PARTIAL_OWNER)
        .map(|e| e.file_name())
        .collect();
    if files.is_empty() {
        return Ok(0);
    }
    std::fs::create_dir_all(to).map_err(|e| format!("{}: {e}", to.display()))?;
    let mut moved = Vec::new();
    for name in files {
        if let Err(e) = move_file(&from.join(&name), &to.join(&name)) {
            // All of them or none: the ones moved go back.
            for back in moved.iter().rev() {
                let _ = move_file(&to.join(back), &from.join(back));
            }
            return Err(format!("{}: {e}", Path::new(&name).display()));
        }
        moved.push(name);
    }
    // Which Haul owns a folder is the folder's own to say.
    let _ = std::fs::remove_file(from.join(super::PARTIAL_OWNER));
    let _ = std::fs::remove_dir(from);
    Ok(moved.len())
}

/// Moves one file, copying it when the two places are on different drives.
fn move_file(from: &Path, to: &Path) -> std::io::Result<()> {
    if std::fs::rename(from, to).is_err() {
        std::fs::copy(from, to)?;
        std::fs::remove_file(from)?;
    }
    Ok(())
}

/// A proxy address Haul can use: `socks5://`, `socks5h://`, `http://` or
/// `https://`, with credentials if it needs them.
fn check_proxy(proxy: &str) -> Result<(), String> {
    let scheme = proxy.split("://").next().unwrap_or_default();
    if !matches!(scheme, "socks5" | "socks5h" | "http" | "https") || !proxy.contains("://") {
        return Err(format!(
            "{} is not a socks5:// or http:// address",
            super::diagnostics::without_credentials(proxy)
        ));
    }
    Http::new(Some(proxy), config::USER_AGENT).map(|_| ())
}
