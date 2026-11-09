//! User settings, all of them in `config.toml` in the data directory.
//!
//! The app writes a key back when the user changes it in the GUI and reloads
//! the file when it changes on disk, so the GUI and a text editor are two ways
//! to the same settings. `HAUL_*` environment variables override some keys;
//! the GUI shows those as set by the environment.

use std::{
    collections::BTreeMap,
    io,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::{paths, store};

/// File name of the configuration inside the data directory.
pub const FILE: &str = "config.toml";

/// The hosts a file downloads from first until the user says otherwise:
/// those that hand a free file over fastest, without an account. megaup.net
/// gave about 9 MB/s once the desktop app pressed its buttons (2026-11-06),
/// against 1fichier's 1 MB/s for the same file.
pub const DEFAULT_PREFERRED: [&str; 4] =
    ["pixeldrain.com", "vikingfile.com", "mega.nz", "megaup.net"];

/// The hosts turned off until the user turns them on, so no file waits on
/// them: ddownload, whose free download is too slow to be worth having (about
/// 30 KB/s after its wait, measured 2026-11-06), and rapidgator, whose free
/// download is a wait and a reCAPTCHA for a person on every file (off at the
/// owner's word, 2026-11-06).
pub const DEFAULT_DISABLED: [&str; 2] = ["ddownload.com", "rapidgator.net"];

/// User agent for every request a host does not get a browser's for. Hosts
/// answered it as they did a current Edge's (2026-11-06), and Cloudflare's
/// check tells a browser by its TLS handshake, not by its name (2026-11-07).
pub const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) Haul/1.0";

/// Serialized for the settings view ([`crate::SettingsView`]), which says
/// where downloads go itself.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize, ts_rs::TS)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Where finished files go.
    #[serde(skip_serializing)]
    #[ts(skip)]
    pub download_dir: Option<PathBuf>,
    /// Files downloaded at the same time.
    pub max_total: usize,
    /// Global speed limit in MB/s; 0 means none.
    pub rate_limit_mbps: f64,
    /// The most gigabytes of files downloading at once, by their full size;
    /// 0 means no limit. A bigger file still downloads on its own.
    pub max_active_gb: f64,
    pub theme: Theme,
    /// Add links copied anywhere (desktop app).
    pub watch_clipboard: bool,
    /// Look for a newer Haul on GitHub now and then.
    pub check_updates: bool,
    /// Removing a finished download from the list also moves its file to the
    /// Recycle Bin.
    pub recycle_on_remove: bool,
    /// Links and files dropped on the window, or opened with Haul, are added
    /// at once instead of filling the Add dialog first.
    pub skip_add_dialog: bool,
    /// A file already in the download folder with the same name and size is
    /// also read in full and compared with the checksum its host publishes,
    /// before its download is skipped. Off: name and size decide.
    pub verify_existing: bool,
    /// An archive in parts is unpacked beside them once the last part is
    /// downloaded.
    pub auto_extract: bool,
    /// After an archive is unpacked without error, its parts go to the
    /// Recycle Bin.
    pub delete_after_extract: bool,
    /// Passwords tried in order on an encrypted archive, after the one
    /// given for it.
    pub archive_passwords: Vec<String>,
    /// Haul's debug events in the log too: every request, retry and mirror
    /// choice, for reproducing a problem.
    pub detailed_log: bool,
    /// Debug mode's live connection ([`crate::debug`]): a client on this
    /// machine with the token Haul writes to `debug-token` follows
    /// everything Haul does as it happens.
    pub live_debug: bool,
    /// Automatic retries of a mirror after a dropped connection or a
    /// network error, before it counts as failed.
    pub resume_attempts: u32,
    /// A download slower than this many KiB/s over `min_speed_seconds` gives
    /// up its mirror: the next one takes over, or it fails until it is tried
    /// again. 0 turns that off; so does a speed limit, which would make every
    /// download look slow.
    pub min_speed_kbps: u32,
    pub min_speed_seconds: u64,
    /// A host that holds a big download far below what it or the line can do
    /// is taken to be limiting this address: the download goes on from another
    /// mirror or a new address, and until then keeps going slowly. No threshold
    /// to set: see [`crate::throttle`].
    pub leave_slow_hosts: bool,
    /// Minutes before a failed download is tried again on its own (at most
    /// three times); 0 turns that off.
    pub retry_failed_minutes: u64,
    /// Proxy every host goes through unless the user approves a direct route
    /// for it, such as `socks5://user:pass@host:1080`.
    pub proxy: Option<String>,
    /// More proxies beside `proxy`: downloads spread over all of them, so a
    /// host's limits per address apply to each.
    pub proxies: Vec<String>,
    /// This machine's own connection is one more exit beside the proxies.
    pub proxy_with_direct: bool,
    /// A new connection for every download through a proxy, for a proxy
    /// that gives each connection another address (a rotating endpoint).
    pub proxy_fresh_connections: bool,
    /// Off: the proxies and VPN servers stay configured, but every host
    /// connects directly. The status bar's switch.
    pub use_proxy: bool,
    /// Off: the proxies of `proxy` and `proxies` stay listed but unused,
    /// while VPN servers go on.
    pub proxies_on: bool,
    /// Off: NordVPN's settings stay, but its servers are not used.
    pub nordvpn_on: bool,
    /// The user's route choice per host when a proxy is configured.
    pub routes: BTreeMap<String, Route>,
    /// Subfolder of the download folder for the files from a host.
    pub folders: BTreeMap<String, String>,
    /// Hosts a file with mirrors downloads from first, in this order
    /// ([`DEFAULT_PREFERRED`] until the list is changed).
    pub preferred_hosts: Vec<String>,
    /// Hosts never downloaded from: their links are left alone
    /// ([`DEFAULT_DISABLED`] until the list is changed).
    pub disabled_hosts: Vec<String>,
    /// Hosts that need an account ([`crate::hosts::Known::needs_account`])
    /// the user wants used like any other without one. The others are used
    /// only when no other host has the file, until their account is set.
    pub accountless_hosts: Vec<String>,
    /// Account keys by their name ([`account_keys`]): the hosts' (whose
    /// [`crate::hosts::Known::accounts`] name them), the router's login and
    /// NordVPN's service credentials.
    pub accounts: BTreeMap<String, String>,
    /// How many of NordVPN's SOCKS5 servers join the proxies, least loaded
    /// first; 0 for none.
    pub nordvpn_servers: usize,
    /// The countries they come from; every one when empty.
    pub nordvpn_countries: Vec<String>,
    /// Gets a new public address on its own when the one this machine has
    /// is what holds downloads back.
    pub reconnect: bool,
    /// Gets a new address instead of asking the router: any command, run in
    /// the system's shell.
    pub reconnect_command: Option<String>,
    #[serde(skip)]
    pub endpoints: Endpoints,
    /// The numbers [`Config::leave_slow_hosts`] goes by. Only tests change them.
    #[serde(skip)]
    pub throttling: crate::throttle::Throttling,
    /// Keys an environment variable set, with the variable's name.
    #[serde(skip)]
    pub from_env: BTreeMap<String, String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            download_dir: None,
            max_total: 8,
            rate_limit_mbps: 0.0,
            max_active_gb: 0.0,
            theme: Theme::System,
            watch_clipboard: false,
            check_updates: true,
            recycle_on_remove: false,
            skip_add_dialog: false,
            verify_existing: false,
            auto_extract: false,
            delete_after_extract: false,
            archive_passwords: Vec::new(),
            detailed_log: true,
            live_debug: false,
            resume_attempts: 5,
            min_speed_kbps: 0,
            min_speed_seconds: 60,
            leave_slow_hosts: true,
            retry_failed_minutes: 15,
            proxy: None,
            proxies: Vec::new(),
            proxy_with_direct: false,
            proxy_fresh_connections: false,
            use_proxy: true,
            proxies_on: true,
            nordvpn_on: true,
            routes: BTreeMap::new(),
            folders: BTreeMap::new(),
            preferred_hosts: DEFAULT_PREFERRED.iter().map(|h| h.to_string()).collect(),
            disabled_hosts: DEFAULT_DISABLED.iter().map(|h| h.to_string()).collect(),
            accountless_hosts: Vec::new(),
            accounts: BTreeMap::new(),
            nordvpn_servers: 0,
            nordvpn_countries: Vec::new(),
            reconnect: false,
            reconnect_command: None,
            endpoints: Endpoints::default(),
            throttling: crate::throttle::Throttling::default(),
            from_env: BTreeMap::new(),
        }
    }
}

/// The account keys that are no host's: the router's login, for one that
/// redials only with it (a FRITZ!Box over TR-064), and NordVPN's service
/// credentials (not the account's login), for its SOCKS5 servers.
const SERVICE_ACCOUNTS: [&str; 4] = [
    "router_user",
    "router_password",
    "nordvpn_user",
    "nordvpn_password",
];

/// Every account key `[accounts]` takes: the hosts', then the services'.
pub fn account_keys() -> impl Iterator<Item = &'static str> {
    crate::hosts::all()
        .flat_map(|k| k.accounts.iter().map(|f| f.key))
        .chain(SERVICE_ACCOUNTS)
}

/// The environment variable that fills account key `key` when it is unset:
/// `HAUL_` and the key in capitals (`HAUL_1FICHIER_KEY`).
fn env_var(key: &str) -> String {
    format!("HAUL_{}", key.to_ascii_uppercase())
}

impl Config {
    /// Reads `path` (a missing file means defaults), then fills unset account
    /// keys from `HAUL_<KEY>` and lets `HAUL_PROXY` replace the proxy.
    pub fn load(path: &Path) -> Result<Self, String> {
        let mut cfg = Self::parse(&match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(e.to_string()),
        })?;
        cfg.apply_env(|k| std::env::var(k).ok());
        Ok(cfg)
    }

    /// Parses the file's text; an error names the line it is on.
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut cfg: Self = toml::from_str(text).map_err(|e| e.to_string().trim().to_string())?;
        if let Some(key) = cfg
            .accounts
            .keys()
            .find(|k| !account_keys().any(|a| a == *k))
        {
            return Err(format!("unknown account key `{key}` in [accounts]"));
        }
        cfg.hosts_by_main_name();
        Ok(cfg)
    }

    /// Names every host in the per-host settings by its main domain
    /// ([`crate::hosts::main_name`]), as links are counted: a star or a
    /// switch set on a host's other domain is the host's. The first entry
    /// for a host wins; the order stays.
    fn hosts_by_main_name(&mut self) {
        let main = crate::hosts::main_name;
        let list = |hosts: &mut Vec<String>| {
            let mut seen = Vec::new();
            hosts.retain_mut(|h| {
                *h = main(h);
                if seen.contains(h) {
                    return false;
                }
                seen.push(h.clone());
                true
            });
        };
        list(&mut self.preferred_hosts);
        list(&mut self.disabled_hosts);
        list(&mut self.accountless_hosts);
        fn keys<V>(map: &mut BTreeMap<String, V>) {
            let old = std::mem::take(map);
            for (host, value) in old {
                map.entry(crate::hosts::main_name(&host)).or_insert(value);
            }
        }
        keys(&mut self.routes);
        keys(&mut self.folders);
    }

    fn apply_env(&mut self, var: impl Fn(&str) -> Option<String>) {
        let get = |k: &str| var(k).filter(|v| !v.is_empty());
        for key in account_keys() {
            let variable = env_var(key);
            if self.account(key).is_none()
                && let Some(value) = get(&variable)
            {
                self.accounts.insert(key.to_string(), value);
                self.from_env.insert(key.to_string(), variable);
            }
        }
        if let Some(proxy) = get("HAUL_PROXY") {
            self.proxy = Some(proxy);
            self.from_env.insert("proxy".into(), "HAUL_PROXY".into());
        }
    }

    /// An account key's value, when it is set and not empty.
    pub fn account(&self, key: &str) -> Option<&str> {
        self.accounts
            .get(key)
            .map(String::as_str)
            .filter(|v| !v.is_empty())
    }

    /// The first proxy in use: configured, and not switched off.
    pub fn proxy(&self) -> Option<&str> {
        self.proxy_pool().first().copied()
    }

    /// Every proxy in use, `proxy` first, each once; none when switched off.
    pub fn proxy_pool(&self) -> Vec<&str> {
        if !self.use_proxy || !self.proxies_on {
            return Vec::new();
        }
        let mut pool: Vec<&str> = Vec::new();
        let listed = secret(&self.proxy)
            .into_iter()
            .chain(self.proxies.iter().map(|p| p.trim()));
        for proxy in listed {
            if !proxy.is_empty() && !pool.contains(&proxy) {
                pool.push(proxy);
            }
        }
        pool
    }

    /// Where `host` stands among the preferred hosts, 0 being the first.
    pub fn preference(&self, host: &str) -> Option<usize> {
        self.preferred_hosts
            .iter()
            .position(|h| h.eq_ignore_ascii_case(host))
    }

    /// Whether the user turned `host` off: its links are never tried. One
    /// that lacks its account is used, after every other
    /// ([`crate::hosts::Known::last_resort`]).
    pub fn is_disabled(&self, host: &str) -> bool {
        self.disabled_hosts
            .iter()
            .any(|h| h.eq_ignore_ascii_case(host))
    }

    /// Whether the user switched `host` on without the account it needs.
    pub fn is_accountless(&self, host: &str) -> bool {
        self.accountless_hosts
            .iter()
            .any(|h| h.eq_ignore_ascii_case(host))
    }

    /// Whether `url`'s host is used only when no other host has the file;
    /// `pages`: the desktop app passes pages by itself.
    pub fn is_last_resort(&self, url: &str, pages: bool) -> bool {
        self.is_last_resort_host(&crate::hosts::host_of(url), pages)
    }

    /// [`Config::is_last_resort`] for host `name`.
    pub fn is_last_resort_host(&self, name: &str, pages: bool) -> bool {
        crate::hosts::known(name).is_some_and(|k| k.last_resort(self, pages))
    }

    /// The subfolder for files from `host`, relative to the download folder;
    /// empty when the host has none.
    pub fn folder_for(&self, host: &str) -> PathBuf {
        self.folders
            .get(host)
            .map(|f| paths::subfolder(f))
            .unwrap_or_default()
    }
}

crate::names::named! {
    /// The app's appearance; `System` follows the operating system.
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
    #[ts(export)]
    pub enum Theme {
        #[default]
        System = "system",
        Light = "light",
        Dark = "dark",
    }
}

crate::names::named! {
    /// How a host is reached when a proxy is configured.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
    #[ts(export)]
    pub enum Route {
        Proxy = "proxy",
        Direct = "direct",
    }
}

/// Sets one key in the file at `path`, or removes it with `None`, keeping
/// everything else, comments included. A missing file starts from
/// [`TEMPLATE`]. `key` is a top-level key or `[table, key]`.
pub fn write_key(path: &Path, key: &[&str], value: Option<toml_edit::Value>) -> io::Result<()> {
    let key: Vec<String> = key.iter().map(|k| (*k).to_string()).collect();
    write_keys(path, &[(key, value)])
}

/// Sets or removes several keys as [`write_key`] does, with one read and one
/// write of the file: all of them land, or none.
pub fn write_keys(
    path: &Path,
    writes: &[(Vec<String>, Option<toml_edit::Value>)],
) -> io::Result<()> {
    let mut text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => TEMPLATE.to_string(),
        Err(e) => return Err(e),
    };
    for (key, value) in writes {
        let key: Vec<&str> = key.iter().map(String::as_str).collect();
        text = with_key(&text, &key, value.clone())?;
    }
    store::write_atomic(path, text.as_bytes())
}

/// `text` with one key set or removed.
fn with_key(text: &str, key: &[&str], value: Option<toml_edit::Value>) -> io::Result<String> {
    let text = text.to_string();
    let mut doc: toml_edit::DocumentMut = text.parse().map_err(io::Error::other)?;
    match (key, value) {
        ([name], Some(value)) => {
            if !doc.contains_key(name)
                && let Some(enabled) = enable_commented(&text, name, &value)
                && let Ok(parsed) = enabled.parse()
            {
                doc = parsed;
            } else {
                doc[*name] = toml_edit::Item::Value(value);
            }
        }
        ([name], None) => {
            // Commenting the line out keeps the explanation above it.
            if let Some(disabled) = disable_line(&text, name)
                && let Ok(parsed) = disabled.parse()
            {
                doc = parsed;
            } else {
                doc.remove(name);
            }
        }
        ([table, name], Some(value)) => {
            if !doc.contains_table(table)
                && let Some(enabled) = enable_table(&text, table, name, &value)
                && let Ok(parsed) = enabled.parse()
            {
                doc = parsed;
            } else {
                if !doc.contains_table(table) {
                    doc[*table] = toml_edit::table();
                }
                doc[*table][*name] = toml_edit::Item::Value(value);
            }
        }
        ([table, name], None) => {
            if let Some(t) = doc
                .get_mut(table)
                .and_then(toml_edit::Item::as_table_like_mut)
            {
                t.remove(name);
            }
        }
        _ => return Err(io::Error::other(format!("bad settings key {key:?}"))),
    }
    Ok(doc.to_string())
}

/// Turns the commented `# key = …` line among the top-level keys into
/// `key = value`, so a new setting lands under its explanation.
fn enable_commented(text: &str, key: &str, value: &toml_edit::Value) -> Option<String> {
    let mut out = String::with_capacity(text.len());
    let mut found = false;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_start();
        if !found && trimmed.starts_with('[') {
            // Past the top-level keys: a line further down belongs to a table.
            return None;
        }
        let commented = trimmed
            .strip_prefix('#')
            .map(str::trim_start)
            .and_then(|rest| rest.strip_prefix(key))
            .is_some_and(|rest| rest.trim_start().starts_with('='));
        if !found && commented {
            out.push_str(&format!("{key} = {}\n", value.to_string().trim()));
            found = true;
        } else {
            out.push_str(line);
        }
    }
    found.then_some(out)
}

/// Turns the commented `# [table]` header into a real one with `name = value`
/// right below it, so the table stays where the template explains it.
fn enable_table(text: &str, table: &str, name: &str, value: &toml_edit::Value) -> Option<String> {
    let header = format!("[{table}]");
    let mut out = String::with_capacity(text.len() + 64);
    let mut found = false;
    for line in text.split_inclusive('\n') {
        let commented = line
            .trim_start()
            .strip_prefix('#')
            .is_some_and(|rest| rest.trim() == header);
        if !found && commented {
            let key = toml_edit::Key::new(name);
            out.push_str(&format!(
                "{header}\n{} = {}\n",
                key.display_repr(),
                value.to_string().trim()
            ));
            found = true;
        } else {
            out.push_str(line);
        }
    }
    found.then_some(out)
}

/// Comments out the top-level `key = …` line.
fn disable_line(text: &str, key: &str) -> Option<String> {
    let mut out = String::with_capacity(text.len() + 2);
    let mut found = false;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_start();
        if !found && trimmed.starts_with('[') {
            return None;
        }
        let set = trimmed
            .strip_prefix(key)
            .is_some_and(|rest| rest.trim_start().starts_with('='));
        if !found && set {
            out.push_str("# ");
            out.push_str(trimmed);
            found = true;
        } else {
            out.push_str(line);
        }
    }
    found.then_some(out)
}

/// Creates `path` from [`TEMPLATE`] unless it exists.
pub fn ensure_file(path: &Path) -> io::Result<()> {
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(mut f) => io::Write::write_all(&mut f, TEMPLATE.as_bytes()),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(e),
    }
}

/// A commented `config.toml` naming every key.
pub const TEMPLATE: &str = r#"# Haul settings. Everything here can also be changed in the app, which
# writes it back to this file; changes saved here apply within a second.

# Folder for finished downloads (default: Downloads\Haul).
# download_dir = "D:/Downloads"

# Files downloaded at the same time.
# max_total = 8

# Global speed limit in MB/s; 0 means none.
# rate_limit_mbps = 0

# The most gigabytes of files downloading at once, by their full size: 20 lets
# four 5 GB files run, or ten of 2 GB. 0 means no limit; a bigger file still
# downloads on its own.
# max_active_gb = 0

# Appearance: "system", "light" or "dark".
# theme = "system"

# Add links copied anywhere (desktop app).
# watch_clipboard = false

# Look for a newer Haul on GitHub after start and every hour; the
# desktop app offers to install it.
# check_updates = true

# Removing a finished download from the list also moves its file to the
# Recycle Bin (the trash outside Windows). Clear completed never does.
# recycle_on_remove = false

# Links and files dropped on the window, or opened with Haul, are added at once
# instead of opening in the Add dialog first.
# skip_add_dialog = false

# A file already in the download folder with the same name and size counts as
# downloaded. With this on, it is also read in full and compared with the
# checksum its host publishes (pixeldrain, gofile and some servers), one file at
# a time: thorough, but slow on big files and spinning disks.
# verify_existing = false

# An archive in parts (name.part1.rar, name.7z.001, ...) is unpacked beside its
# parts once the last one is downloaded. RAR, 7-Zip and zip, told apart by their
# contents; one archive at a time.
# auto_extract = false

# After an archive is unpacked without error, its parts go to the Recycle Bin
# (the trash outside Windows).
# delete_after_extract = false

# Passwords tried in order on an encrypted archive, after the one typed for it.
# archive_passwords = ["password one", "password two"]

# Writes Haul's debug events to haul.log too (every attempt, outcome and mirror
# choice), so a problem is on record the first time; false keeps notable
# events.
# detailed_log = true

# Lets a diagnosing tool on this computer follow everything Haul does as it
# happens, with the key Haul then writes to debug-token in its data folder
# (debug mode, docs/debugging.md). HAUL_DEBUG=1 turns it on for one run.
# live_debug = false

# Retries of a mirror after a dropped connection or a network error. An attempt
# that gets data in before it stops starts the count over.
# resume_attempts = 5

# A download slower than this many KiB/s over min_speed_seconds gives up its
# mirror: the next mirror takes over, or it fails until it is tried again.
# 0 turns that off; so does a speed limit (rate_limit_mbps).
# min_speed_kbps = 0
# min_speed_seconds = 60

# A host that holds a big download far below what it or your line can do is
# taken to be limiting your address (free tiers do that instead of refusing).
# The download goes on from another mirror or, with reconnect on, a new address
# once other downloads have finished; until then it keeps going slowly and says
# why. Your own speed limit, a slow line and a file nearly done don't count.
# leave_slow_hosts = true

# Minutes before a failed download is tried again on its own (up to three
# times), and a page Haul passes by itself that wanted you; 0 turns that off.
# retry_failed_minutes = 15

# Every host goes through this proxy unless you allow a direct route for it
# in Hosts. HAUL_PROXY overrides it.
# proxy = "socks5://user:pass@host:1080"
# More proxies: each download takes the one with the fewest downloads from its
# host, so a host's limits per address (downloads at once, free quotas, waits)
# apply to each of them. A SOCKS5 server of a VPN subscription, a local
# WireGuard-to-SOCKS5 port or a bought list all fit.
# proxies = ["socks5://user:pass@amsterdam.example:1080", "http://127.0.0.1:8118"]
# This machine's own connection as one more of them (it shows your address).
# proxy_with_direct = false
# A new connection for every download, for a proxy that gives each connection
# another address (a rotating endpoint).
# proxy_fresh_connections = false
# false keeps the proxies above listed but leaves them out (VPN servers stay).
# proxies_on = true
# false keeps the proxies and VPN servers but connects every host directly.
# use_proxy = true

# NordVPN's SOCKS5 servers as more proxies: how many servers (least loaded
# first) and from which countries (every one when empty). Its service
# credentials are under [accounts].
# nordvpn_servers = 3
# nordvpn_countries = ["Sweden", "Netherlands"]
# false keeps all of the above but leaves NordVPN's servers out.
# nordvpn_on = true

# A new public address from the router when the one this machine has is what
# holds downloads back (a host's daily allowance used up, a long wait asked of
# it). Haul finds the router itself and asks it to redial over UPnP; a router
# that wants its login for that (a FRITZ!Box over TR-064) takes it here. Or a
# command of your own gets the new address. Every redial cuts the line for a
# few seconds, so it is off unless you switch it on. The router's login is
# under [accounts].
# reconnect = false
# reconnect_command = "my-router-cli reconnect"

# Hosts a file with mirrors downloads from first, in this order (default:
# pixeldrain.com, vikingfile.com, mega.nz, megaup.net); the rest follow by
# measured speed. A file waits for a free slot on a preferred host rather
# than starting on another one, unless the preferred host is cooling down or
# has failed for that file.
# preferred_hosts = ["rapidgator.net", "pixeldrain.com"]

# Hosts never downloaded from: their links are left alone, and a file whose
# links are all on these hosts fails until one of them is turned back on
# (default: ddownload.com, whose free downloads crawl, and rapidgator.net).
# disabled_hosts = ["mediafire.com"]

# Hosts that give no decent download without an account (gofile.io,
# rapidgator.net, nitroflare.com) are used only when no other host has the
# file, until their account is set. List one here to use it like any other
# host without its account: every file then asks you for a captcha, or, for
# gofile, comes only through its page.
# accountless_hosts = ["rapidgator.net"]

# Hosts allowed to bypass the proxy ("direct"), or kept on it ("proxy").
# [routes]
# "example.com" = "direct"

# Subfolders of the download folder for the files from these hosts.
# [folders]
# "pixeldrain.com" = "pixeldrain"

# Accounts: the hosts' keys and logins, the router's login (router_user,
# router_password) and NordVPN's service credentials (nordvpn_user,
# nordvpn_password: Nord Account › NordVPN › manual setup, not the account's
# login). HAUL_<KEY> environment variables (HAUL_1FICHIER_KEY, ...) fill the
# keys left unset here.
# [accounts]
# 1fichier_key = ""
# pixeldrain_key = ""
# rapidgator_user = ""
# rapidgator_pass = ""
# nitroflare_user = ""
# nitroflare_key = ""
# gofile_token = ""
"#;

/// A configured, non-empty value.
pub fn secret(v: &Option<String>) -> Option<&str> {
    v.as_deref().filter(|s| !s.is_empty())
}

/// Where each host's API or page lives. Only tests point these elsewhere.
#[derive(Debug, Clone, PartialEq)]
pub struct Endpoints {
    pub pixeldrain: String,
    pub gofile: String,
    pub buzzheavier: String,
    pub onefichier: String,
    /// 1fichier's site, whose link checker needs no key.
    pub onefichier_site: String,
    pub rapidgator: String,
    pub nitroflare: String,
    pub dlc_service: String,
    pub fileguard: String,
    pub filecrypt: String,
    pub vikingfile: String,
    /// MEGA's API.
    pub mega: String,
    pub rootz: String,
    pub mediafire: String,
    /// GitHub's API, for Haul's own releases.
    pub github_api: String,
    pub mirrored: String,
    /// The router's description; empty to look for it on the network.
    pub router: String,
    pub nordvpn: String,
    /// Answers the public address a request comes from, as plain text.
    pub ip_echo: String,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self {
            pixeldrain: "https://pixeldrain.com".into(),
            gofile: "https://api.gofile.io".into(),
            buzzheavier: "https://buzzheavier.com".into(),
            onefichier: "https://api.1fichier.com".into(),
            onefichier_site: "https://1fichier.com".into(),
            rapidgator: "https://rapidgator.net".into(),
            nitroflare: "https://nitroflare.com".into(),
            dlc_service: "http://service.jdownloader.org/dlcrypt/service.php".into(),
            fileguard: "https://fileguard.cc".into(),
            filecrypt: "https://filecrypt.cc".into(),
            vikingfile: "https://vikingfile.com".into(),
            mega: "https://g.api.mega.co.nz".into(),
            rootz: "https://www.rootz.so".into(),
            mediafire: "https://www.mediafire.com".into(),
            github_api: "https://api.github.com".into(),
            mirrored: "https://www.mirrored.to".into(),
            router: String::new(),
            nordvpn: "https://api.nordvpn.com".into(),
            ip_echo: "https://api.ipify.org".into(),
        }
    }
}

#[cfg(test)]
mod tests {
    /// Every setting is in the file's template and in the docs, so neither
    /// can fall behind the code.
    #[test]
    fn every_setting_is_in_the_template_and_the_docs() {
        let docs = include_str!("../../../docs/configuration.md");
        let cfg = serde_json::to_value(Config::default()).unwrap();
        for key in cfg.as_object().unwrap().keys() {
            let table = matches!(key.as_str(), "routes" | "folders" | "accounts");
            let in_template = TEMPLATE.lines().any(|l| {
                let l = l.trim_start_matches('#').trim_start();
                if table {
                    l == format!("[{key}]")
                } else {
                    l.starts_with(&format!("{key} ="))
                }
            });
            assert!(in_template, "{key} is not in TEMPLATE");
            assert!(
                docs.contains(key.as_str()),
                "{key} is not in docs/configuration.md"
            );
        }
    }

    use super::*;

    #[test]
    fn parses_every_key() {
        let cfg = Config::parse(
            r#"
            download_dir = "D:/Downloads"
            max_total = 4
            rate_limit_mbps = 2.5
            watch_clipboard = true
            recycle_on_remove = true
            skip_add_dialog = true
            verify_existing = true
            auto_extract = true
            delete_after_extract = true
            archive_passwords = ["one", "two"]
            retry_failed_minutes = 0
            proxy = "socks5://u:p@host:1080"
            preferred_hosts = ["B.example", "a.example"]
            [accounts]
            1fichier_key = "k1"
            [routes]
            "a.example" = "direct"
            [folders]
            "a.example" = "Hosts/../a"
            "#,
        )
        .unwrap();
        assert_eq!(
            (cfg.max_total, cfg.watch_clipboard, cfg.retry_failed_minutes),
            (4, true, 0)
        );
        assert_eq!(cfg.account("1fichier_key"), Some("k1"));
        assert_eq!(cfg.routes.get("a.example"), Some(&Route::Direct));
        assert!(cfg.recycle_on_remove && cfg.skip_add_dialog && cfg.verify_existing);
        assert!(cfg.auto_extract && cfg.delete_after_extract);
        assert_eq!(cfg.archive_passwords, ["one", "two"]);
        assert_eq!(
            (cfg.preference("b.example"), cfg.preference("a.example")),
            (Some(0), Some(1))
        );
        assert_eq!(cfg.preference("c.example"), None);
        assert_eq!(cfg.folder_for("a.example"), Path::new("Hosts").join("a"));
        assert_eq!(cfg.folder_for("other.example"), PathBuf::new());
        let typo = Config::parse("max_totl = 3").unwrap_err();
        assert!(typo.contains("max_totl"), "{typo}");
        let typo = Config::parse("[accounts]\npixeldrain = \"k\"").unwrap_err();
        assert!(typo.contains("pixeldrain"), "{typo}");
        assert_eq!(Config::parse(TEMPLATE).unwrap(), Config::default());
    }

    #[test]
    fn a_hosts_other_domain_in_the_settings_is_the_host() {
        let cfg = Config::parse(
            "preferred_hosts = [\"mega.nz\", \"vik1ngfile.site\", \"vikingfile.com\"]\n\
             disabled_hosts = [\"DDL.to\"]\n\
             [routes]\n\"vik1ngfile.site\" = \"direct\"\n",
        )
        .unwrap();
        assert_eq!(cfg.preferred_hosts, ["mega.nz", "vikingfile.com"]);
        assert_eq!(cfg.disabled_hosts, ["ddownload.com"]);
        assert_eq!(cfg.routes.get("vikingfile.com"), Some(&Route::Direct));
    }

    #[test]
    fn hosts_too_slow_for_free_are_off_until_told_otherwise() {
        assert_eq!(
            Config::default().disabled_hosts,
            ["ddownload.com", "rapidgator.net"]
        );
        assert!(
            Config::parse("disabled_hosts = []")
                .unwrap()
                .disabled_hosts
                .is_empty()
        );
    }

    #[test]
    fn the_fast_free_hosts_are_preferred_until_told_otherwise() {
        assert_eq!(
            Config::default().preferred_hosts,
            ["pixeldrain.com", "vikingfile.com", "mega.nz", "megaup.net"]
        );
        // An explicit empty list means no preference, not the default.
        assert!(
            Config::parse("preferred_hosts = []")
                .unwrap()
                .preferred_hosts
                .is_empty()
        );
    }

    #[test]
    fn a_proxy_switched_off_stays_configured() {
        let cfg = Config::parse("proxy = \"socks5://h:1\"\nuse_proxy = false").unwrap();
        assert_eq!(cfg.proxy.as_deref(), Some("socks5://h:1"));
        assert_eq!(cfg.proxy(), None);
    }

    #[test]
    fn env_fills_gaps_and_proxy_wins() {
        let mut cfg = Config {
            accounts: BTreeMap::from([("pixeldrain_key".into(), "file".into())]),
            proxy: Some("socks5://file:1".into()),
            ..Config::default()
        };
        cfg.apply_env(|k| match k {
            "HAUL_PIXELDRAIN_KEY" => Some("env".into()),
            "HAUL_1FICHIER_KEY" => Some("env1f".into()),
            "HAUL_PROXY" => Some("socks5://env:2".into()),
            _ => None,
        });
        assert_eq!(cfg.account("pixeldrain_key"), Some("file"));
        assert_eq!(cfg.account("1fichier_key"), Some("env1f"));
        assert_eq!(cfg.proxy(), Some("socks5://env:2"));
        assert_eq!(
            cfg.from_env
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_str()))
                .collect::<Vec<_>>(),
            [
                ("1fichier_key", "HAUL_1FICHIER_KEY"),
                ("proxy", "HAUL_PROXY")
            ]
        );
    }

    #[test]
    fn keys_are_written_back_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE);
        std::fs::write(
            &path,
            "# mine\nproxy = \"socks5://h:1\" # keep\nmax_total = 2\n[routes]\n\"a.example\" = \"proxy\"\n",
        )
        .unwrap();
        write_key(&path, &["max_total"], Some(3.into())).unwrap();
        write_key(&path, &["rate_limit_mbps"], Some(1.5.into())).unwrap();
        write_key(&path, &["theme"], Some("dark".into())).unwrap();
        write_key(&path, &["routes", "c.example"], Some("direct".into())).unwrap();
        write_key(&path, &["proxy"], None).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.starts_with("# mine\n# proxy = "),
            "cleared, not deleted: {text}"
        );
        assert!(
            text.contains("\nmax_total = 3\n"),
            "changed in place: {text}"
        );
        let cfg = Config::parse(&text).unwrap();
        assert_eq!(
            (cfg.max_total, cfg.rate_limit_mbps, cfg.theme),
            (3, 1.5, Theme::Dark)
        );
        assert_eq!(cfg.routes.get("a.example"), Some(&Route::Proxy));
        assert_eq!(cfg.routes.get("c.example"), Some(&Route::Direct));
        assert_eq!(cfg.proxy, None);
        write_key(&path, &["routes", "c.example"], None).unwrap();
        let routes = Config::load(&path).unwrap().routes;
        assert_eq!(
            routes.into_iter().collect::<Vec<_>>(),
            [("a.example".to_string(), Route::Proxy)],
            "only the cleared key goes"
        );

        // A new file starts from the template; values land under their comments.
        let fresh = dir.path().join("fresh.toml");
        write_key(&fresh, &["routes", "127.0.0.1"], Some("direct".into())).unwrap();
        write_key(&fresh, &["theme"], Some("light".into())).unwrap();
        write_key(&fresh, &["max_total"], Some(5.into())).unwrap();
        write_key(&fresh, &["accounts", "1fichier_key"], Some("k".into())).unwrap();
        let text = std::fs::read_to_string(&fresh).unwrap();
        assert!(
            text.starts_with("# Haul settings"),
            "the header stays on top: {text}"
        );
        assert!(
            text.contains("# Files downloaded at the same time.\nmax_total = 5\n"),
            "{text}"
        );
        assert!(text.contains("\ntheme = \"light\"\n"), "{text}");
        assert!(
            text.contains("\n[accounts]\n1fichier_key = \"k\"\n"),
            "{text}"
        );
        assert!(
            text.contains("\n[routes]\n\"127.0.0.1\" = \"direct\"\n"),
            "{text}"
        );
        let cfg = Config::parse(&text).unwrap();
        assert_eq!((cfg.theme, cfg.max_total), (Theme::Light, 5));
        assert_eq!(cfg.account("1fichier_key"), Some("k"));
        ensure_file(&fresh).unwrap();
        assert!(
            std::fs::read_to_string(&fresh)
                .unwrap()
                .contains("theme = \"light\""),
            "never overwritten"
        );
    }
}
