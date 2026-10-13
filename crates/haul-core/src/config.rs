//! User settings: `config.toml` in the data directory, with `HAUL_*`
//! environment variables for the proxy and account secrets.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

/// File name of the configuration inside the data directory.
pub const FILE: &str = "config.toml";

/// User agent for every request a host does not get a browser's for.
pub const DEFAULT_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) Haul/1.0";

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Where finished files go; `--dir` overrides it.
    pub download_dir: Option<PathBuf>,
    /// Files downloaded at the same time.
    pub max_total: usize,
    /// Global speed limit in MB/s at start-up; 0 means none.
    pub rate_limit_mbps: f64,
    /// Proxy every host goes through unless the user approves a direct route
    /// for it, such as `socks5://user:pass@host:1080`.
    pub proxy: Option<String>,
    pub user_agent: String,
    /// Parallel downloads per host; unlisted hosts get [`FALLBACK_LIMIT`].
    pub limits: BTreeMap<String, usize>,
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
    pub theme: Theme,
    #[serde(skip)]
    pub endpoints: Endpoints,
}

/// Per-host parallel downloads when `limits` does not name the host.
pub const FALLBACK_LIMIT: usize = 32;

impl Default for Config {
    fn default() -> Self {
        Self {
            download_dir: None,
            max_total: 8,
            rate_limit_mbps: 0.0,
            proxy: None,
            user_agent: DEFAULT_USER_AGENT.into(),
            limits: BTreeMap::new(),
            onefichier_key: None,
            pixeldrain_key: None,
            rapidgator_user: None,
            rapidgator_pass: None,
            nitroflare_user: None,
            nitroflare_key: None,
            gofile_token: None,
            megatools: None,
            theme: Theme::System,
            endpoints: Endpoints::default(),
        }
    }
}

impl Config {
    /// Reads `path` (a missing file means defaults), then fills unset secrets
    /// from `HAUL_<KEY>` and lets `HAUL_PROXY` replace the proxy.
    pub fn load(path: &Path) -> Result<Self, String> {
        let mut cfg: Config = match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).map_err(|e| e.to_string())?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Config::default(),
            Err(e) => return Err(e.to_string()),
        };
        cfg.apply_env(|k| std::env::var(k).ok());
        Ok(cfg)
    }

    fn apply_env(&mut self, var: impl Fn(&str) -> Option<String>) {
        let get = |k: &str| var(k).filter(|v| !v.is_empty());
        for (slot, key) in [
            (&mut self.onefichier_key, "HAUL_1FICHIER_KEY"),
            (&mut self.pixeldrain_key, "HAUL_PIXELDRAIN_KEY"),
            (&mut self.rapidgator_user, "HAUL_RAPIDGATOR_USER"),
            (&mut self.rapidgator_pass, "HAUL_RAPIDGATOR_PASS"),
            (&mut self.nitroflare_user, "HAUL_NITROFLARE_USER"),
            (&mut self.nitroflare_key, "HAUL_NITROFLARE_KEY"),
            (&mut self.gofile_token, "HAUL_GOFILE_TOKEN"),
        ] {
            if secret(slot).is_none() {
                *slot = get(key);
            }
        }
        if let Some(proxy) = get("HAUL_PROXY") {
            self.proxy = Some(proxy);
        }
    }

    /// Parallel downloads allowed for `host` before the user changes it.
    pub fn limit_for(&self, host: &str) -> usize {
        self.limits
            .get(host)
            .copied()
            .unwrap_or(FALLBACK_LIMIT)
            .clamp(1, FALLBACK_LIMIT)
    }

    /// The proxy, if one is configured.
    pub fn proxy(&self) -> Option<&str> {
        secret(&self.proxy)
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

/// A setting the app changes and writes back to `config.toml`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Setting<'a> {
    MaxTotal(usize),
    RateLimit(f64),
    Theme(Theme),
    HostLimit(&'a str, usize),
}

/// Writes one setting into the file at `path` (starting from [`TEMPLATE`]
/// when there is none), keeping everything else, comments included.
pub fn write_setting(path: &Path, setting: Setting<'_>) -> std::io::Result<()> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => TEMPLATE.to_string(),
        Err(e) => return Err(e),
    };
    let mut doc: toml_edit::DocumentMut = text.parse().map_err(std::io::Error::other)?;
    let (key, value) = match setting {
        Setting::MaxTotal(n) => ("max_total", toml_edit::Value::from(n as i64)),
        Setting::RateLimit(mbps) => ("rate_limit_mbps", toml_edit::Value::from(mbps)),
        Setting::Theme(theme) => ("theme", toml_edit::Value::from(theme.as_str())),
        Setting::HostLimit(host, n) => {
            if !doc.contains_table("limits") {
                doc["limits"] = toml_edit::table();
            }
            doc["limits"][host] = toml_edit::value(n as i64);
            return write_atomic(path, &doc.to_string());
        }
    };
    if !doc.contains_key(key)
        && let Some(enabled) = enable_commented(&text, key, &value)
        && let Ok(parsed) = enabled.parse()
    {
        doc = parsed;
    } else {
        doc[key] = toml_edit::Item::Value(value);
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

fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

/// Creates `path` from [`TEMPLATE`] unless it exists.
pub fn ensure_file(path: &Path) -> std::io::Result<()> {
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(mut f) => std::io::Write::write_all(&mut f, TEMPLATE.as_bytes()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(e),
    }
}

/// A commented `config.toml` naming every key.
pub const TEMPLATE: &str = r#"# Haul settings. The app writes parallel files, the speed limit, host
# slots and the theme back here when you change them; restart Haul after
# editing anything else.

# Folder for finished downloads (default: Downloads\Haul).
# download_dir = "D:/Downloads"

# Files downloaded at the same time.
# max_total = 8

# Global speed limit in MB/s; 0 means none.
# rate_limit_mbps = 0

# Appearance: "system", "light" or "dark".
# theme = "system"

# Every host goes through this proxy unless you allow a direct route for it
# in Hosts. HAUL_PROXY overrides it.
# proxy = "socks5://user:pass@host:1080"

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

# Parallel downloads per host; unlisted hosts get 32.
# [limits]
# "1fichier.com" = 1
"#;

/// A configured, non-empty value.
pub fn secret(v: &Option<String>) -> Option<&str> {
    v.as_deref().filter(|s| !s.is_empty())
}

/// Where each host's API lives. Only tests point these elsewhere.
#[derive(Debug, Clone)]
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
        let cfg: Config = toml::from_str(
            r#"
            download_dir = "D:/Downloads"
            max_total = 4
            rate_limit_mbps = 2.5
            proxy = "socks5://u:p@host:1080"
            1fichier_key = "k1"
            pixeldrain_key = "k2"
            gofile_token = "t"
            [limits]
            "1fichier.com" = 1
            "#,
        )
        .unwrap();
        assert_eq!(cfg.max_total, 4);
        assert_eq!(cfg.onefichier_key.as_deref(), Some("k1"));
        assert_eq!(cfg.limit_for("1fichier.com"), 1);
        assert_eq!(cfg.limit_for("other.example"), FALLBACK_LIMIT);
        assert_eq!(cfg.user_agent, DEFAULT_USER_AGENT);
        assert!(
            toml::from_str::<Config>("max_totl = 3").is_err(),
            "typos are reported"
        );
    }

    #[test]
    fn template_parses_and_settings_write_back() {
        let parsed: Config = toml::from_str(TEMPLATE).unwrap();
        assert_eq!(parsed.max_total, Config::default().max_total);

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE);
        std::fs::write(
            &path,
            "# mine\nproxy = \"socks5://h:1\" # keep\n[limits]\n\"a.example\" = 2\n",
        )
        .unwrap();
        write_setting(&path, Setting::MaxTotal(3)).unwrap();
        write_setting(&path, Setting::RateLimit(1.5)).unwrap();
        write_setting(&path, Setting::Theme(Theme::Dark)).unwrap();
        write_setting(&path, Setting::HostLimit("b.example", 1)).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.starts_with("# mine") && text.contains("# keep"),
            "{text}"
        );
        let cfg: Config = toml::from_str(&text).unwrap();
        assert_eq!(
            (cfg.max_total, cfg.rate_limit_mbps, cfg.theme),
            (3, 1.5, Theme::Dark)
        );
        assert_eq!(
            (cfg.limit_for("a.example"), cfg.limit_for("b.example")),
            (2, 1)
        );

        let fresh = dir.path().join("fresh.toml");
        write_setting(&fresh, Setting::Theme(Theme::Light)).unwrap();
        write_setting(&fresh, Setting::MaxTotal(5)).unwrap();
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
        let cfg: Config = toml::from_str(&text).unwrap();
        assert_eq!((cfg.theme, cfg.max_total), (Theme::Light, 5));
        ensure_file(&fresh).unwrap();
        assert!(
            std::fs::read_to_string(&fresh)
                .unwrap()
                .contains("theme = \"light\""),
            "never overwritten"
        );
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
    }
}
