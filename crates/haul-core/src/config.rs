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

use crate::paths;

/// File name of the configuration inside the data directory.
pub const FILE: &str = "config.toml";

/// User agent for every request a host does not get a browser's for.
pub const DEFAULT_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) Haul/1.0";

/// Per-host parallel downloads when `limits` does not name the host.
pub const FALLBACK_LIMIT: usize = 32;

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Where finished files go.
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
    /// Automatic retries of a mirror after a dropped connection or a
    /// network error, before it counts as failed.
    pub resume_attempts: u32,
    /// A download slower than this many KiB/s over `min_speed_seconds` gives
    /// up its mirror: the next one takes over, or it fails until it is tried
    /// again. 0 turns that off; so does a speed limit, which would make every
    /// download look slow.
    pub min_speed_kbps: u32,
    pub min_speed_seconds: u64,
    /// Minutes before a failed download is tried again on its own (at most
    /// three times); 0 turns that off.
    pub retry_failed_minutes: u64,
    /// Proxy every host goes through unless the user approves a direct route
    /// for it, such as `socks5://user:pass@host:1080`.
    pub proxy: Option<String>,
    /// Off: the proxy stays configured, but every host connects directly.
    pub use_proxy: bool,
    pub user_agent: String,
    /// Parallel downloads per host; unlisted hosts get [`FALLBACK_LIMIT`].
    pub limits: BTreeMap<String, usize>,
    /// The user's route choice per host when a proxy is configured.
    pub routes: BTreeMap<String, Route>,
    /// Subfolder of the download folder for the files from a host.
    pub folders: BTreeMap<String, String>,
    /// Hosts a file with mirrors downloads from first, in this order.
    pub preferred_hosts: Vec<String>,
    #[serde(rename = "1fichier_key")]
    pub onefichier_key: Option<String>,
    pub pixeldrain_key: Option<String>,
    pub rapidgator_user: Option<String>,
    pub rapidgator_pass: Option<String>,
    pub nitroflare_user: Option<String>,
    pub nitroflare_key: Option<String>,
    pub gofile_token: Option<String>,
    /// `megatools` executable for mega.nz; looked up on `PATH` when unset.
    pub megatools: Option<PathBuf>,
    #[serde(skip)]
    pub endpoints: Endpoints,
    /// Keys an environment variable set, with the variable's name.
    #[serde(skip)]
    pub from_env: BTreeMap<&'static str, &'static str>,
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
            recycle_on_remove: false,
            skip_add_dialog: false,
            verify_existing: false,
            resume_attempts: 5,
            min_speed_kbps: 0,
            min_speed_seconds: 60,
            retry_failed_minutes: 15,
            proxy: None,
            use_proxy: true,
            user_agent: DEFAULT_USER_AGENT.into(),
            limits: BTreeMap::new(),
            routes: BTreeMap::new(),
            folders: BTreeMap::new(),
            preferred_hosts: Vec::new(),
            onefichier_key: None,
            pixeldrain_key: None,
            rapidgator_user: None,
            rapidgator_pass: None,
            nitroflare_user: None,
            nitroflare_key: None,
            gofile_token: None,
            megatools: None,
            endpoints: Endpoints::default(),
            from_env: BTreeMap::new(),
        }
    }
}

/// Account keys, by their name in the file and their environment variable.
pub const ACCOUNTS: [(&str, &str); 7] = [
    ("1fichier_key", "HAUL_1FICHIER_KEY"),
    ("pixeldrain_key", "HAUL_PIXELDRAIN_KEY"),
    ("rapidgator_user", "HAUL_RAPIDGATOR_USER"),
    ("rapidgator_pass", "HAUL_RAPIDGATOR_PASS"),
    ("nitroflare_user", "HAUL_NITROFLARE_USER"),
    ("nitroflare_key", "HAUL_NITROFLARE_KEY"),
    ("gofile_token", "HAUL_GOFILE_TOKEN"),
];

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
        toml::from_str(text).map_err(|e| e.to_string().trim().to_string())
    }

    fn apply_env(&mut self, var: impl Fn(&str) -> Option<String>) {
        let get = |k: &str| var(k).filter(|v| !v.is_empty());
        for (key, variable) in ACCOUNTS {
            let slot = self.account_mut(key).expect("listed account");
            if secret(slot).is_none()
                && let Some(value) = get(variable)
            {
                *slot = Some(value);
                self.from_env.insert(key, variable);
            }
        }
        if let Some(proxy) = get("HAUL_PROXY") {
            self.proxy = Some(proxy);
            self.from_env.insert("proxy", "HAUL_PROXY");
        }
    }

    /// An account key's value, by its name in the file.
    pub fn account(&self, key: &str) -> Option<&Option<String>> {
        Some(match key {
            "1fichier_key" => &self.onefichier_key,
            "pixeldrain_key" => &self.pixeldrain_key,
            "rapidgator_user" => &self.rapidgator_user,
            "rapidgator_pass" => &self.rapidgator_pass,
            "nitroflare_user" => &self.nitroflare_user,
            "nitroflare_key" => &self.nitroflare_key,
            "gofile_token" => &self.gofile_token,
            _ => return None,
        })
    }

    pub fn account_mut(&mut self, key: &str) -> Option<&mut Option<String>> {
        Some(match key {
            "1fichier_key" => &mut self.onefichier_key,
            "pixeldrain_key" => &mut self.pixeldrain_key,
            "rapidgator_user" => &mut self.rapidgator_user,
            "rapidgator_pass" => &mut self.rapidgator_pass,
            "nitroflare_user" => &mut self.nitroflare_user,
            "nitroflare_key" => &mut self.nitroflare_key,
            "gofile_token" => &mut self.gofile_token,
            _ => return None,
        })
    }

    /// Parallel downloads allowed for `host`.
    pub fn limit_for(&self, host: &str) -> usize {
        self.limits
            .get(host)
            .copied()
            .unwrap_or(FALLBACK_LIMIT)
            .clamp(1, FALLBACK_LIMIT)
    }

    /// The proxy in use: configured, and not switched off.
    pub fn proxy(&self) -> Option<&str> {
        secret(&self.proxy).filter(|_| self.use_proxy)
    }

    /// Where `host` stands among the preferred hosts, 0 being the first.
    pub fn preference(&self, host: &str) -> Option<usize> {
        self.preferred_hosts
            .iter()
            .position(|h| h.eq_ignore_ascii_case(host))
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

/// The app's appearance; `System` follows the operating system.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    #[default]
    System,
    Light,
    Dark,
}

impl Theme {
    pub fn as_str(self) -> &'static str {
        match self {
            Theme::System => "system",
            Theme::Light => "light",
            Theme::Dark => "dark",
        }
    }
}

/// How a host is reached when a proxy is configured.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Route {
    Proxy,
    Direct,
}

impl Route {
    pub fn as_str(self) -> &'static str {
        match self {
            Route::Proxy => "proxy",
            Route::Direct => "direct",
        }
    }
}

/// Sets one key in the file at `path`, or removes it with `None`, keeping
/// everything else, comments included. A missing file starts from
/// [`TEMPLATE`]. `key` is a top-level key or `[table, key]`.
pub fn write_key(path: &Path, key: &[&str], value: Option<toml_edit::Value>) -> io::Result<()> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => TEMPLATE.to_string(),
        Err(e) => return Err(e),
    };
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
    write_atomic(path, &doc.to_string())
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

fn write_atomic(path: &Path, text: &str) -> io::Result<()> {
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
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

# Retries of a mirror after a dropped connection or a network error. An attempt
# that gets data in before it stops starts the count over.
# resume_attempts = 5

# A download slower than this many KiB/s over min_speed_seconds gives up its
# mirror: the next mirror takes over, or it fails until it is tried again.
# 0 turns that off; so does a speed limit (rate_limit_mbps).
# min_speed_kbps = 0
# min_speed_seconds = 60

# Minutes before a failed download is tried again on its own (up to three
# times); 0 turns that off.
# retry_failed_minutes = 15

# Every host goes through this proxy unless you allow a direct route for it
# in Hosts. HAUL_PROXY overrides it.
# proxy = "socks5://user:pass@host:1080"
# false keeps the proxy above but connects every host directly.
# use_proxy = true

# Accounts. HAUL_<KEY> environment variables (HAUL_1FICHIER_KEY, ...) fill
# the keys left unset here.
# 1fichier_key = ""
# pixeldrain_key = ""
# rapidgator_user = ""
# rapidgator_pass = ""
# nitroflare_user = ""
# nitroflare_key = ""
# gofile_token = ""

# megatools for mega.nz links (default: found on PATH).
# megatools = "C:/Tools/megatools/megatools.exe"

# Hosts a file with mirrors downloads from first, in this order; the rest
# follow by measured speed. A file waits for a free slot on a preferred host
# rather than starting on another one, unless the preferred host is cooling
# down or has failed for that file.
# preferred_hosts = ["rapidgator.net", "pixeldrain.com"]

# Sent to hosts with every request.
# user_agent = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) Haul/1.0"

# Files downloaded from one host at the same time; unlisted hosts get 32.
# [limits]
# "1fichier.com" = 1

# Hosts allowed to bypass the proxy ("direct"), or kept on it ("proxy").
# [routes]
# "example.com" = "direct"

# Subfolders of the download folder for the files from these hosts.
# [folders]
# "pixeldrain.com" = "pixeldrain"
"#;

/// A configured, non-empty value.
pub fn secret(v: &Option<String>) -> Option<&str> {
    v.as_deref().filter(|s| !s.is_empty())
}

/// Where each host's API lives. Only tests point these elsewhere.
#[derive(Debug, Clone, PartialEq)]
pub struct Endpoints {
    pub pixeldrain: String,
    pub gofile: String,
    pub buzzheavier: String,
    pub onefichier: String,
    pub rapidgator: String,
    pub nitroflare: String,
    pub dlc_service: String,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self {
            pixeldrain: "https://pixeldrain.com".into(),
            gofile: "https://api.gofile.io".into(),
            buzzheavier: "https://buzzheavier.com".into(),
            onefichier: "https://api.1fichier.com".into(),
            rapidgator: "https://rapidgator.net".into(),
            nitroflare: "https://nitroflare.com".into(),
            dlc_service: "http://service.jdownloader.org/dlcrypt/service.php".into(),
        }
    }
}

#[cfg(test)]
mod tests {
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
            retry_failed_minutes = 0
            proxy = "socks5://u:p@host:1080"
            1fichier_key = "k1"
            preferred_hosts = ["B.example", "a.example"]
            [limits]
            "1fichier.com" = 1
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
        assert_eq!(cfg.onefichier_key.as_deref(), Some("k1"));
        assert_eq!(cfg.limit_for("1fichier.com"), 1);
        assert_eq!(cfg.limit_for("other.example"), FALLBACK_LIMIT);
        assert_eq!(cfg.routes.get("a.example"), Some(&Route::Direct));
        assert!(cfg.recycle_on_remove && cfg.skip_add_dialog && cfg.verify_existing);
        assert_eq!(
            (cfg.preference("b.example"), cfg.preference("a.example")),
            (Some(0), Some(1))
        );
        assert_eq!(cfg.preference("c.example"), None);
        assert_eq!(cfg.folder_for("a.example"), Path::new("Hosts").join("a"));
        assert_eq!(cfg.folder_for("other.example"), PathBuf::new());
        assert_eq!(cfg.user_agent, DEFAULT_USER_AGENT);
        let typo = Config::parse("max_totl = 3").unwrap_err();
        assert!(typo.contains("max_totl"), "{typo}");
        assert_eq!(Config::parse(TEMPLATE).unwrap(), Config::default());
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
            pixeldrain_key: Some("file".into()),
            proxy: Some("socks5://file:1".into()),
            ..Config::default()
        };
        cfg.apply_env(|k| match k {
            "HAUL_PIXELDRAIN_KEY" => Some("env".into()),
            "HAUL_1FICHIER_KEY" => Some("env1f".into()),
            "HAUL_PROXY" => Some("socks5://env:2".into()),
            _ => None,
        });
        assert_eq!(cfg.pixeldrain_key.as_deref(), Some("file"));
        assert_eq!(cfg.onefichier_key.as_deref(), Some("env1f"));
        assert_eq!(cfg.proxy(), Some("socks5://env:2"));
        assert_eq!(
            cfg.from_env.into_iter().collect::<Vec<_>>(),
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
            "# mine\nproxy = \"socks5://h:1\" # keep\nmax_total = 2\n[limits]\n\"a.example\" = 2\n",
        )
        .unwrap();
        write_key(&path, &["max_total"], Some(3.into())).unwrap();
        write_key(&path, &["rate_limit_mbps"], Some(1.5.into())).unwrap();
        write_key(&path, &["theme"], Some("dark".into())).unwrap();
        write_key(&path, &["limits", "b.example"], Some(1.into())).unwrap();
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
        assert_eq!(
            (cfg.limit_for("a.example"), cfg.limit_for("b.example")),
            (2, 1)
        );
        assert_eq!(cfg.routes.get("c.example"), Some(&Route::Direct));
        assert_eq!(cfg.proxy, None);
        write_key(&path, &["routes", "c.example"], None).unwrap();
        assert!(Config::load(&path).unwrap().routes.is_empty());

        // A new file starts from the template; values land under their comments.
        let fresh = dir.path().join("fresh.toml");
        write_key(&fresh, &["routes", "127.0.0.1"], Some("direct".into())).unwrap();
        write_key(&fresh, &["theme"], Some("light".into())).unwrap();
        write_key(&fresh, &["max_total"], Some(5.into())).unwrap();
        write_key(&fresh, &["1fichier_key"], Some("k".into())).unwrap();
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
        assert!(text.contains("\n1fichier_key = \"k\"\n"), "{text}");
        assert!(
            text.contains("\n[routes]\n\"127.0.0.1\" = \"direct\"\n"),
            "{text}"
        );
        let cfg = Config::parse(&text).unwrap();
        assert_eq!((cfg.theme, cfg.max_total), (Theme::Light, 5));
        ensure_file(&fresh).unwrap();
        assert!(
            std::fs::read_to_string(&fresh)
                .unwrap()
                .contains("theme = \"light\""),
            "never overwritten"
        );
    }
}
