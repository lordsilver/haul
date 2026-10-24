//! Per-host resolvers: a page link in, direct links out, with the headers,
//! name and size the host reports.
//!
//! Only legitimate routes are used: public APIs, guest tokens, the user's own
//! premium or API-key accounts, and a wrapper page's own form once its gate is
//! passed. A gate Haul can answer without a browser is answered here: the
//! proof of work behind fileguard's ALTCHA and filecrypt's checkbox. A host
//! that needs a person (a captcha, a browser check) raises
//! [`ResolveError::NeedsUser`] and the item waits for one, and a host's wait
//! timer is waited out.

mod buzzheavier;
mod ddownload;
mod filecrypt;
mod fileguard;
mod generic;
mod gofile;
mod mediafire;
mod mega;
mod mirrored;
mod nitroflare;
mod onefichier;
mod pixeldrain;
mod rapidgator;
mod rootz;
mod vikingfile;

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    io::Read,
    path::Path,
    sync::{
        Arc, LazyLock, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use indexmap::IndexMap;
use regex::Regex;
use reqwest::header::{
    CONTENT_DISPOSITION, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, HeaderMap, RETRY_AFTER,
};
use serde::{Deserialize, Serialize};
use sha2::Digest;

use crate::config::Config;

/// What a resolver hands back: one downloadable file, or — with `deferred`
/// — a link to resolve in its own turn.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub url: String,
    #[serde(default)]
    pub name: Option<String>,
    /// The package the host's own listing puts the file in, when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,
    #[serde(default)]
    pub size: Option<u64>,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    /// The file's checksum, when the host publishes one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checksum: Option<Checksum>,
    /// The URL is a page to resolve at download time, not a download: a
    /// wrapper's list is links like any pasted one, each resolved by its own
    /// host when its turn comes.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub deferred: bool,
    /// The same file's links on other hosts, for an entry a wrapper lists:
    /// one row with every host as a mirror.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mirrors: Vec<String>,
    /// The key the file's bytes are encrypted with (MEGA's), decrypted as
    /// they arrive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decrypt: Option<mega::Key>,
}

/// What a wrapper's page holds once its gate is passed.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Unwrapped {
    /// The page's own name for what it holds, when it gives one.
    pub title: Option<String>,
    /// One entry per file, each with its links on every host.
    pub files: Vec<Entry>,
}

/// What a probe learns about a file without downloading it.
#[derive(Debug, Clone, PartialEq)]
pub struct Probed {
    pub name: String,
    pub size: Option<u64>,
    pub checksum: Option<Checksum>,
}

/// A hash algorithm that hosts publish checksums in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Algo {
    Md5,
    Sha256,
    Sha512,
}

impl Algo {
    /// Length of the digest in bytes.
    fn len(self) -> usize {
        match self {
            Algo::Md5 => 16,
            Algo::Sha256 => 32,
            Algo::Sha512 => 64,
        }
    }
}

/// A file's checksum as its host publishes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checksum {
    pub algo: Algo,
    /// Lowercase hexadecimal.
    pub hex: String,
}

impl Checksum {
    /// From hexadecimal, as APIs send it; `None` unless it has the right length.
    pub fn from_hex(algo: Algo, hex: &str) -> Option<Self> {
        let hex = hex.trim().to_ascii_lowercase();
        (hex.len() == algo.len() * 2 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
            .then_some(Self { algo, hex })
    }

    /// From base64, as HTTP headers send it.
    pub fn from_base64(algo: Algo, value: &str) -> Option<Self> {
        let bytes = STANDARD.decode(value.trim()).ok()?;
        (bytes.len() == algo.len()).then(|| Self {
            algo,
            hex: to_hex(&bytes),
        })
    }

    /// Whether the file at `path` has this checksum. Reads all of it.
    pub fn matches(&self, path: &Path) -> std::io::Result<bool> {
        self.matches_steered(path, &|| Steer::Go)
    }

    /// [`Checksum::matches`], asking `steer` before every read whether to go
    /// on, wait (while downloads are paused, say) or give up, which fails
    /// with [`std::io::ErrorKind::Interrupted`].
    pub fn matches_steered(&self, path: &Path, steer: &dyn Fn() -> Steer) -> std::io::Result<bool> {
        let file = std::fs::File::open(path)?;
        let hex = match self.algo {
            Algo::Md5 => hash_reader::<md5::Md5>(file, steer)?,
            Algo::Sha256 => hash_reader::<sha2::Sha256>(file, steer)?,
            Algo::Sha512 => hash_reader::<sha2::Sha512>(file, steer)?,
        };
        Ok(hex == self.hex)
    }
}

/// What a long checksum read does next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Steer {
    Go,
    Wait,
    Stop,
}

fn hash_reader<D: Digest>(
    mut reader: impl Read,
    steer: &dyn Fn() -> Steer,
) -> std::io::Result<String> {
    let mut hasher = D::new();
    let mut buf = vec![0; 1 << 20];
    loop {
        loop {
            match steer() {
                Steer::Go => break,
                Steer::Wait => std::thread::sleep(Duration::from_millis(250)),
                Steer::Stop => return Err(std::io::ErrorKind::Interrupted.into()),
            }
        }
        match reader.read(&mut buf)? {
            0 => return Ok(to_hex(&hasher.finalize())),
            n => hasher.update(&buf[..n]),
        }
    }
}

fn to_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    bytes
        .iter()
        .flat_map(|b| [DIGITS[usize::from(b >> 4)], DIGITS[usize::from(b & 0xf)]])
        .map(char::from)
        .collect()
}

/// The checksum a server publishes for the whole file in its headers, by
/// preference: `Repr-Digest` (RFC 9530), `Digest` (RFC 3230), Google Cloud
/// Storage's `x-goog-hash`, then `Content-MD5`. That last one covers only the
/// body of this response, so it counts only when `whole_body` says the body is
/// the whole file (a 200, not a 206).
pub(crate) fn header_checksum(headers: &HeaderMap, whole_body: bool) -> Option<Checksum> {
    // `name: key=value, key=value` across every line of the header, keys in
    // lowercase; values keep their `=` padding.
    let pairs = |name: &str| -> Vec<(String, String)> {
        headers
            .get_all(name)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .flat_map(|v| v.split(','))
            .filter_map(|p| p.trim().split_once('='))
            .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
            .collect()
    };
    let find = |pairs: &[(String, String)], key: &str, algo: Algo| {
        pairs
            .iter()
            .filter(|(k, _)| k == key)
            .find_map(|(_, v)| Checksum::from_base64(algo, v.trim_matches(':')))
    };
    let repr = pairs("repr-digest");
    let digest = pairs("digest");
    let goog = pairs("x-goog-hash");
    find(&repr, "sha-512", Algo::Sha512)
        .or_else(|| find(&repr, "sha-256", Algo::Sha256))
        .or_else(|| find(&digest, "sha-512", Algo::Sha512))
        .or_else(|| find(&digest, "sha-256", Algo::Sha256))
        .or_else(|| find(&digest, "md5", Algo::Md5))
        .or_else(|| find(&goog, "md5", Algo::Md5))
        .or_else(|| {
            let value = headers.get("content-md5")?.to_str().ok()?;
            whole_body
                .then(|| Checksum::from_base64(Algo::Md5, value))
                .flatten()
        })
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ResolveError {
    /// The host asked for a pause (rate limit or free-tier timer).
    #[error("{message}")]
    Wait { seconds: u64, message: String },
    /// The host needs a person (captcha or browser step).
    #[error("{0}")]
    NeedsUser(String),
    /// The host could not be reached; worth trying again shortly.
    #[error("{0}")]
    Network(String),
    #[error("{0}")]
    Failed(String),
}

impl ResolveError {
    pub(crate) fn failed(message: impl Into<String>) -> Self {
        Self::Failed(message.into())
    }
}

/// The host's own words when it refuses a request: a JSON `message` (or
/// `value`, `error`, `detail`), or the first line of a short plain-text body.
/// Reads at most 4 KiB, for at most two seconds.
pub(crate) async fn refusal_reason(mut resp: reqwest::Response) -> Option<String> {
    let json = content_type(resp.headers()).contains("json");
    let mut body = Vec::new();
    while body.len() < 4096 {
        match tokio::time::timeout(Duration::from_secs(2), resp.chunk()).await {
            Ok(Ok(Some(chunk))) => body.extend_from_slice(&chunk),
            _ => break,
        }
    }
    body.truncate(4096);
    let text = String::from_utf8_lossy(&body);
    let text = text.trim();
    let clip = |s: &str| s.trim().chars().take(160).collect::<String>();
    if json || text.starts_with('{') {
        let value: serde_json::Value = serde_json::from_str(text).ok()?;
        return ["message", "value", "error", "detail"]
            .iter()
            .find_map(|key| value.get(key)?.as_str().filter(|s| !s.trim().is_empty()))
            .map(clip);
    }
    let line = text.lines().next()?.trim();
    (!line.is_empty() && !line.starts_with('<')).then(|| clip(line))
}

/// reqwest's message and the root cause under it, since "error sending
/// request" alone does not say whether DNS, the connection or TLS failed. The
/// URL is left out: it can carry account credentials in its query.
pub(crate) fn error_text(e: reqwest::Error) -> String {
    let e = e.without_url();
    let top = e.to_string();
    let mut cause: &dyn std::error::Error = &e;
    while let Some(next) = cause.source() {
        cause = next;
    }
    let root = cause.to_string();
    if root == top || top.ends_with(&root) {
        top
    } else {
        format!("{top}: {root}")
    }
}

impl From<reqwest::Error> for ResolveError {
    fn from(e: reqwest::Error) -> Self {
        let network = e.is_connect() || e.is_timeout();
        let message = error_text(e);
        if network {
            Self::Network(message)
        } else {
            Self::Failed(message)
        }
    }
}

/// Which code serves a host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Pixeldrain,
    Gofile,
    Buzzheavier,
    OneFichier,
    Rapidgator,
    Nitroflare,
    Mega,
    Vikingfile,
    Rootz,
    Mediafire,
    Ddownload,
    /// Its links are the files.
    Direct,
    Fileguard,
    Filecrypt,
    Mirrored,
}

/// What a free user gets from a host, where it is known; an account the
/// user sets may lift it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, ts_rs::TS)]
#[ts(export)]
pub struct Free {
    /// Downloads at once from one address.
    pub at_once: Option<usize>,
    /// Bytes a day one address gets at full speed.
    pub daily_bytes: Option<u64>,
    /// Bytes per second past them.
    pub slowed_to: Option<u64>,
}

/// A host Haul knows: the names it goes by, the code that serves it, the
/// account settings that unlock more, what a free user gets, and whether it
/// says how much an address has left today ([`allowance`]).
#[derive(Debug)]
pub struct Known {
    pub names: &'static [&'static str],
    kind: Kind,
    pub accounts: &'static [&'static str],
    pub free: Free,
    pub tells_allowance: bool,
    /// Its page's check passes by itself in a browser from most addresses,
    /// so the desktop app opens the page on its own, out of sight, and
    /// shows it only when the check wants a person after all.
    pub page_passes: bool,
    /// Without its account it never gives a file: off until one is set.
    pub needs_account: bool,
}

impl Known {
    /// A wrapper's site, which stands for further links rather than files.
    pub fn wraps(&self) -> bool {
        matches!(
            self.kind,
            Kind::Fileguard | Kind::Filecrypt | Kind::Mirrored
        )
    }

    /// Whether the user set every account setting this host takes.
    pub fn has_account(&self, cfg: &Config) -> bool {
        !self.accounts.is_empty()
            && self.accounts.iter().all(|key| {
                cfg.account(key)
                    .is_some_and(|v| crate::config::secret(v).is_some())
            })
    }

    /// It needs an account the user has not set, and no page passed out of
    /// sight stands in for one (`pages`: the desktop app passes pages):
    /// its links are left out.
    pub fn lacks_account(&self, cfg: &Config, pages: bool) -> bool {
        self.needs_account && !self.has_account(cfg) && !(pages && self.page_passes)
    }

    /// The downloads at once it allows one address, unless an account
    /// lifts it.
    pub fn at_once(&self, cfg: &Config) -> Option<usize> {
        self.free.at_once.filter(|_| !self.has_account(cfg))
    }
}

/// Every host with code of its own. Free limits are what was measured:
/// pixeldrain's from its own refusal ("maximum number of open download
/// connections for free/anonymous accounts", past 5) and its
/// `/api/misc/rate_limits`.
static KNOWN: &[Known] = &[
    Known {
        names: &["pixeldrain.com"],
        kind: Kind::Pixeldrain,
        accounts: &["pixeldrain_key"],
        free: Free {
            at_once: Some(5),
            daily_bytes: Some(6_000_000_000),
            slowed_to: Some(1_048_576),
        },
        tells_allowance: true,
        page_passes: false,
        needs_account: false,
    },
    Known {
        names: &["gofile.io"],
        kind: Kind::Gofile,
        accounts: &["gofile_token"],
        free: Free {
            at_once: None,
            daily_bytes: None,
            slowed_to: None,
        },
        tells_allowance: false,
        page_passes: true,
        needs_account: true,
    },
    Known {
        names: &["buzzheavier.com"],
        kind: Kind::Buzzheavier,
        accounts: &[],
        free: Free {
            at_once: None,
            daily_bytes: None,
            slowed_to: None,
        },
        tells_allowance: false,
        page_passes: false,
        needs_account: false,
    },
    Known {
        names: &["1fichier.com"],
        kind: Kind::OneFichier,
        accounts: &["1fichier_key"],
        free: Free {
            at_once: None,
            daily_bytes: None,
            slowed_to: None,
        },
        tells_allowance: false,
        page_passes: false,
        needs_account: false,
    },
    Known {
        names: &["rapidgator.net"],
        kind: Kind::Rapidgator,
        accounts: &["rapidgator_user", "rapidgator_pass"],
        free: Free {
            at_once: None,
            daily_bytes: None,
            slowed_to: None,
        },
        tells_allowance: false,
        page_passes: false,
        needs_account: true,
    },
    Known {
        names: &["nitroflare.com"],
        kind: Kind::Nitroflare,
        accounts: &["nitroflare_user", "nitroflare_key"],
        free: Free {
            at_once: None,
            daily_bytes: None,
            slowed_to: None,
        },
        tells_allowance: false,
        page_passes: false,
        needs_account: true,
    },
    Known {
        names: &["mega.nz", "mega.co.nz", "transfer.it"],
        kind: Kind::Mega,
        accounts: &[],
        free: Free {
            at_once: None,
            daily_bytes: None,
            slowed_to: None,
        },
        tells_allowance: false,
        page_passes: false,
        needs_account: false,
    },
    Known {
        names: &["ddownload.com", "ddl.to"],
        kind: Kind::Ddownload,
        accounts: &[],
        free: Free {
            at_once: None,
            daily_bytes: None,
            slowed_to: None,
        },
        tells_allowance: false,
        // A 60-second wait and Cloudflare's Turnstile, which a browser
        // passes out of sight.
        page_passes: true,
        needs_account: false,
    },
    Known {
        names: &["mediafire.com"],
        kind: Kind::Mediafire,
        accounts: &[],
        free: Free {
            at_once: None,
            daily_bytes: None,
            slowed_to: None,
        },
        tells_allowance: false,
        page_passes: false,
        needs_account: false,
    },
    Known {
        names: &["rootz.so"],
        kind: Kind::Rootz,
        accounts: &[],
        // One download per address every two minutes.
        free: Free {
            at_once: Some(1),
            daily_bytes: None,
            slowed_to: None,
        },
        tells_allowance: false,
        page_passes: false,
        needs_account: false,
    },
    Known {
        names: &["vikingfile.com", "vik1ngfile.site"],
        kind: Kind::Vikingfile,
        accounts: &[],
        free: Free {
            at_once: None,
            daily_bytes: None,
            slowed_to: None,
        },
        tells_allowance: false,
        page_passes: true,
        needs_account: false,
    },
    Known {
        names: &["fileditchfiles.st"],
        kind: Kind::Direct,
        accounts: &[],
        free: Free {
            at_once: None,
            daily_bytes: None,
            slowed_to: None,
        },
        tells_allowance: false,
        page_passes: false,
        needs_account: false,
    },
    Known {
        names: &["fileguard.cc"],
        kind: Kind::Fileguard,
        accounts: &[],
        free: Free {
            at_once: None,
            daily_bytes: None,
            slowed_to: None,
        },
        tells_allowance: false,
        page_passes: false,
        needs_account: false,
    },
    Known {
        names: &["filecrypt.cc"],
        kind: Kind::Filecrypt,
        accounts: &[],
        free: Free {
            at_once: None,
            daily_bytes: None,
            slowed_to: None,
        },
        tells_allowance: false,
        page_passes: false,
        needs_account: false,
    },
    Known {
        names: &["mirrored.to"],
        kind: Kind::Mirrored,
        accounts: &[],
        free: Free {
            at_once: None,
            daily_bytes: None,
            slowed_to: None,
        },
        tells_allowance: false,
        page_passes: false,
        needs_account: false,
    },
];

/// Every host Haul knows.
pub fn all() -> impl Iterator<Item = &'static Known> {
    KNOWN.iter()
}

/// The known host a host name (as [`host_of`] gives it) belongs to.
pub fn known(host: &str) -> Option<&'static Known> {
    KNOWN.iter().find(|k| k.names.contains(&host))
}

/// Whether `url`'s page passes its check by itself (see
/// [`Known::page_passes`]).
/// How long a page that passes by itself takes on its own before it could
/// want a person: ddownload's makes everyone wait a minute first.
pub fn page_takes(url: &str) -> std::time::Duration {
    match kind_of(url) {
        Some(Kind::Ddownload) => std::time::Duration::from_secs(75),
        _ => std::time::Duration::ZERO,
    }
}

pub fn page_passes(url: &str) -> bool {
    known(&host_of(url)).is_some_and(|k| k.page_passes)
}

fn kind_of(url: &str) -> Option<Kind> {
    known(&host_of(url)).map(|k| k.kind)
}

/// What an address may still fetch at full speed today, as a host that
/// tells says it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub struct Allowance {
    #[serde(rename = "transfer_limit")]
    pub limit: u64,
    #[serde(rename = "transfer_limit_used")]
    pub used: u64,
    /// Bytes per second once `used` passes `limit`.
    #[serde(rename = "speed_limit")]
    pub slowed_to: u64,
}

impl Allowance {
    /// Past the day's full-speed bytes.
    pub fn slowed(&self) -> bool {
        self.limit > 0 && self.used >= self.limit
    }
}

/// The allowance of the address `cx` reaches `host` from, for a host that
/// tells ([`Known::tells_allowance`]).
pub async fn allowance(cx: &Cx<'_>, host: &str) -> Option<Allowance> {
    match known(host)?.kind {
        Kind::Pixeldrain => pixeldrain::allowance(cx).await,
        _ => None,
    }
}

/// Whether a link is worth adding when it shows up on the clipboard: a host
/// Haul knows, or a link to a file rather than to a web page.
pub fn is_download_link(url: &str) -> bool {
    if kind_of(url).is_some() || is_paste(url) || is_release_page(url) {
        return true;
    }
    // A page that names a file (`name.rar.html`) is still a page.
    last_segment(url).is_some_and(|last| page_extension(&last).is_none())
        && guess_name(url).is_some()
}

/// Sites whose pages are releases' posts, each linking a release's files or
/// pastes of them: a page copied from one is taken like a download link.
const RELEASE_SITES: &[&str] = &["elamigosgames.net", "gload.to"];

/// Whether `url` is a release's post on a site of [`RELEASE_SITES`] (not its
/// front page).
fn is_release_page(url: &str) -> bool {
    RELEASE_SITES.contains(&host_of(url).as_str())
        && url::Url::parse(url).is_ok_and(|u| u.path().trim_matches('/').len() > 1)
}

/// Extensions of web pages, as opposed to files.
const PAGE_EXTENSIONS: [&str; 9] = [
    "html", "htm", "shtml", "xhtml", "php", "asp", "aspx", "jsp", "cgi",
];

/// `name` without its extension when that extension is a web page's.
fn page_extension(name: &str) -> Option<&str> {
    let (stem, ext) = name.rsplit_once('.')?;
    PAGE_EXTENSIONS
        .iter()
        .any(|page| page.eq_ignore_ascii_case(ext))
        .then_some(stem)
}

/// HTTP clients for one route (direct or through the proxy). Both share a
/// cookie jar, as one browser session would.
#[derive(Clone)]
pub struct Http {
    pub client: reqwest::Client,
    pub no_redirect: reqwest::Client,
    pub via_proxy: bool,
    /// The User-Agent both clients send, for a page that also reports it.
    pub user_agent: String,
}

impl Http {
    pub fn new(proxy: Option<&str>, user_agent: &str) -> Result<Self, String> {
        Self::with(proxy, user_agent, false)
    }

    /// With `fresh`, no connection is kept for the next request, so a proxy
    /// that gives each connection another address gives each download one.
    pub fn with(proxy: Option<&str>, user_agent: &str, fresh: bool) -> Result<Self, String> {
        let jar = Arc::new(reqwest::cookie::Jar::default());
        let build = |follow: bool| {
            let mut b = reqwest::Client::builder()
                .no_proxy()
                .user_agent(user_agent)
                .cookie_provider(jar.clone())
                .connect_timeout(Duration::from_secs(if proxy.is_some() { 30 } else { 20 }))
                .read_timeout(Duration::from_secs(60));
            if !follow {
                b = b.redirect(reqwest::redirect::Policy::none());
            }
            if fresh {
                b = b.pool_max_idle_per_host(0);
            }
            if let Some(p) = proxy {
                b = b.proxy(reqwest::Proxy::all(remote_dns(p)).map_err(|e| format!("proxy: {e}"))?);
            }
            b.build().map_err(|e| e.to_string())
        };
        Ok(Self {
            client: build(true)?,
            no_redirect: build(false)?,
            via_proxy: proxy.is_some(),
            user_agent: user_agent.to_string(),
        })
    }
}

/// Resolve host names on the proxy, so DNS does not leak around it.
fn remote_dns(proxy: &str) -> String {
    match proxy.strip_prefix("socks5://") {
        Some(rest) => format!("socks5h://{rest}"),
        None => proxy.to_string(),
    }
}

/// Session tokens resolvers reuse between calls.
#[derive(Debug, Default)]
pub struct Tokens {
    pub(crate) rapidgator: Mutex<Option<String>>,
    /// When vikingfile's `check-file` may be asked next: one lookup at a
    /// time, spaced out, since it turns a burst away with 429.
    pub(crate) vikingfile_next: tokio::sync::Mutex<Option<std::time::Instant>>,
}

/// Everything a resolver may use.
pub struct Cx<'a> {
    pub http: &'a Http,
    pub cfg: &'a Config,
    pub tokens: &'a Tokens,
    /// The route to JDownloader's key service, which a `.dlc` is decrypted
    /// through: the service's own, not that of the host the file came from.
    pub key_service: &'a Http,
}

/// Lower-case host name without port or `www.`.
pub fn host_of(url: &str) -> String {
    let host = url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_ascii_lowercase))
        .unwrap_or_default();
    match host.strip_prefix("www.") {
        Some(rest) => rest.to_string(),
        None => host,
    }
}

/// The site a link's host belongs to, as far as who runs it goes: the host
/// name's last two labels, or three under a country's second level
/// (`example.co.uk`); an address stays as it is.
pub fn site_of(url: &str) -> String {
    let host = host_of(url);
    if host.parse::<std::net::IpAddr>().is_ok() || host.starts_with('[') {
        return host;
    }
    let labels: Vec<&str> = host.trim_end_matches('.').split('.').collect();
    let n = labels.len();
    let country_second_level = n >= 3
        && labels[n - 1].len() == 2
        && matches!(
            labels[n - 2],
            "co" | "com" | "net" | "org" | "gov" | "edu" | "ac" | "or" | "ne" | "go" | "gob"
        );
    let keep = if country_second_level { 3 } else { 2 };
    labels[n.saturating_sub(keep)..].join(".")
}

static FILE_NAME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\.\w{2,5}$").unwrap());

/// The last part of a link's path, decoded.
fn last_segment(url: &str) -> Option<String> {
    let url = url::Url::parse(url).ok()?;
    let last = url.path().trim_end_matches('/').rsplit('/').next()?;
    Some(
        percent_encoding::percent_decode_str(last)
            .decode_utf8_lossy()
            .into_owned(),
    )
}

/// The file name in a link's path, when it looks like one. File hosts' pages
/// often add `.html` to the name of the file they offer (`name.rar.html`),
/// which names `name.rar`; a page of its own (`index.php`) names no file.
pub fn guess_name(url: &str) -> Option<String> {
    let last = last_segment(url)?;
    let name = page_extension(&last).unwrap_or(&last);
    FILE_NAME.is_match(name).then(|| name.to_string())
}

/// Whether the link is a wrapper's, gated page or container alike: it
/// stands for further links, never for one file, whether it is added or a
/// person hands it back.
pub fn wraps(url: &str) -> bool {
    known(&host_of(url)).is_some_and(Known::wraps)
}

/// The files in a wrapper's list of links. A wrapper lists each host's links
/// in the same order (one link per host for a single file; part 1, part 2…
/// on every host for a release in parts), so when every host has as many
/// links as the others, the n-th link of each host is the same file: one
/// entry per file with every host's link as a mirror, named after the links
/// that name it. When hosts differ in count, or a file's links disagree on
/// its name, each link is an entry of its own, named after its own path. A
/// link to another wrapper stands for more links, not a file, so it is an
/// entry of its own.
fn group_mirrors(urls: Vec<String>) -> Vec<Entry> {
    group_named(urls.into_iter().map(|url| (url, None)).collect())
}

/// [`group_mirrors`] for links a wrapper names: a link's own name wins over
/// the one its address suggests.
fn group_named(links: Vec<(String, Option<String>)>) -> Vec<Entry> {
    let (nested, links): (Vec<_>, Vec<_>) = links.into_iter().partition(|(u, _)| wraps(u));
    let named: HashMap<String, String> = links
        .iter()
        .filter_map(|(u, n)| Some((u.clone(), n.clone()?)))
        .collect();
    let guess_name = |url: &str| named.get(url).cloned().or_else(|| guess_name(url));
    let mut hosts: IndexMap<String, Vec<String>> = IndexMap::new();
    for (url, _) in links {
        hosts.entry(host_of(&url)).or_default().push(url);
    }
    let count = hosts.values().next().map_or(0, Vec::len);
    let aligned = hosts.values().all(|links| links.len() == count);
    let mut grouped = Vec::with_capacity(count);
    if aligned {
        for n in 0..count {
            let links: Vec<String> = hosts.values().map(|links| links[n].clone()).collect();
            let names: Vec<String> = links.iter().filter_map(|url| guess_name(url)).collect();
            if names
                .iter()
                .map(|n| name_key(n))
                .collect::<HashSet<_>>()
                .len()
                > 1
            {
                grouped.clear();
                break;
            }
            grouped.push((links, names.into_iter().reduce(better_name)));
        }
    }
    let mut out: Vec<Entry> = if grouped.len() == count && aligned {
        grouped
            .into_iter()
            .map(|(mut links, name)| Entry {
                url: links.remove(0),
                mirrors: links,
                name,
                deferred: true,
                ..Entry::default()
            })
            .collect()
    } else {
        hosts
            .into_values()
            .flatten()
            .map(|url| Entry {
                name: guess_name(&url),
                url,
                deferred: true,
                ..Entry::default()
            })
            .collect()
    };
    out.extend(nested.into_iter().map(|(url, _)| Entry {
        url,
        deferred: true,
        ..Entry::default()
    }));
    out
}

/// A page's text for a heading: entities decoded, `None` when empty.
fn page_text(raw: &str) -> Option<String> {
    let text = decode_entities(raw.trim());
    (!text.is_empty()).then_some(text)
}

static ENTITY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"&(#[0-9]{1,7}|#[xX][0-9a-fA-F]{1,6}|[a-zA-Z]{2,6});").unwrap());

/// HTML's character references, named and numeric (filecrypt.cc writes a
/// name's brackets as `&#40;` and `&#41;`); an unknown one stays as it is.
fn decode_entities(text: &str) -> String {
    // Some pages escape a reference's own `&` too (`&amp;#40;`).
    let once = decode_once(text);
    if once.contains('&') {
        decode_once(&once)
    } else {
        once
    }
}

fn decode_once(text: &str) -> String {
    ENTITY
        .replace_all(text, |c: &regex::Captures<'_>| {
            let body = &c[1];
            let ch = if let Some(hex) = body.strip_prefix("#x").or_else(|| body.strip_prefix("#X"))
            {
                u32::from_str_radix(hex, 16).ok().and_then(char::from_u32)
            } else if let Some(dec) = body.strip_prefix('#') {
                dec.parse().ok().and_then(char::from_u32)
            } else {
                match body {
                    "amp" => Some('&'),
                    "lt" => Some('<'),
                    "gt" => Some('>'),
                    "quot" => Some('"'),
                    "apos" => Some('\''),
                    "nbsp" => Some(' '),
                    _ => None,
                }
            };
            ch.map_or_else(|| c[0].to_string(), String::from)
        })
        .into_owned()
}

/// A browser's mark for a second copy of a file, ` (2)` before its
/// extension, as some uploads keep it.
static COPY_MARK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(.+?) \(\d{1,3}\)(\.\w{2,5})$").unwrap());

/// A file name as Haul saves and shows it: character references decoded
/// and a copy mark (`name.part1 (2).rar`) taken off, since the parts of an
/// archive are only found by their plain names.
pub fn tidy_name(name: &str) -> String {
    let name = decode_entities(name.trim());
    match COPY_MARK.captures(&name) {
        Some(c) => format!("{}{}", &c[1], &c[2]),
        None => name,
    }
}

/// What a file name is recognised by on every host: [`tidy_name`], with
/// `_` for a space as some hosts write it, runs of spaces as one, and any
/// case.
pub fn name_key(name: &str) -> String {
    tidy_name(name)
        .replace('_', " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Of two names for the same file, the one to keep: the one a host wrote
/// with spaces rather than `_`, then the one without a copy mark.
pub fn better_name(a: String, b: String) -> String {
    let rank = |n: &str| {
        (
            n.matches('_').count(),
            COPY_MARK.is_match(n),
            n.contains('&'),
        )
    };
    if rank(&b) < rank(&a) { b } else { a }
}

/// Tells a search on another thread to stop once the resolve that started it
/// is dropped, as a paused or removed download's is.
struct StopOnDrop(Arc<AtomicBool>);

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

/// What a wrapper link points at, so two links to it count as one: every
/// fileguard.cc link of a vault (`/<vault>/<host>`) shows the same page.
pub fn wrapper_key(url: &str) -> String {
    let key = match kind_of(url) {
        Some(Kind::Fileguard) => fileguard::key(url),
        Some(Kind::Filecrypt) => filecrypt::key(url),
        Some(Kind::Mirrored) => mirrored::key(url),
        _ => None,
    };
    key.unwrap_or_else(|| url.to_string())
}

/// The files behind a wrapper link, once its gate is passed. A web page on
/// a site Haul has no code for (a release's post, a paste) unwraps to the
/// links it shows.
pub async fn unwrap(cx: &Cx<'_>, url: &str) -> Result<Unwrapped, ResolveError> {
    match kind_of(url) {
        Some(Kind::Fileguard) => fileguard::unwrap(cx, url).await,
        Some(Kind::Filecrypt) => filecrypt::unwrap(cx, url).await,
        Some(Kind::Mirrored) => mirrored::unwrap(cx, url).await,
        None => {
            let (title, links) = generic::page(cx, url).await?;
            Ok(listed(title, links))
        }
        _ => Err(ResolveError::failed("not a wrapper link")),
    }
}

/// A page's links as what unwrapping it found: one entry each, resolved
/// when its turn comes.
pub fn listed(title: Option<String>, links: Vec<String>) -> Unwrapped {
    Unwrapped {
        title,
        files: links
            .into_iter()
            .map(|url| Entry {
                name: guess_name(&url),
                url,
                deferred: true,
                ..Entry::default()
            })
            .collect(),
    }
}

/// A page's title without the site's name after a separator ("Release |
/// Site" is "Release").
pub fn short_title(raw: &str) -> Option<String> {
    generic::short_title(raw)
}

/// Whether `url` is a paste (zpaste.net, paste2.org…): a list of links,
/// taken as a wrapper from the start.
pub fn is_paste(url: &str) -> bool {
    generic::is_paste(url)
}

/// Whether `url` stands for more links rather than a file: a wrapper's, or
/// a paste.
pub fn lists(url: &str) -> bool {
    wraps(url) || is_paste(url)
}

/// Whether a resolver's request for a person is for a page behind a
/// browser check a browser usually passes by itself (Cloudflare's).
pub fn is_guarded(message: &str) -> bool {
    generic::is_guarded(message)
}

/// Whether `url` may be a web page that lists links: a host Haul has no
/// code for.
pub fn may_list_links(url: &str) -> bool {
    kind_of(url).is_none()
}

/// The links a web page holds to hosts Haul knows, to wrappers and to
/// pastes, as if the page's text were pasted (see [`generic::page`]),
/// with the page's own name for what it holds.
pub async fn page(cx: &Cx<'_>, url: &str) -> Result<(Option<String>, Vec<String>), ResolveError> {
    generic::page(cx, url).await
}

/// The links in `text` (addresses, one per line or in a page's markup) to
/// hosts Haul knows and to wrappers, as [`page`] keeps them from the
/// page at `page`.
pub fn listed_links(text: &str, page: &str) -> Vec<String> {
    url::Url::parse(page).map_or_else(|_| Vec::new(), |base| generic::links_in(text, &base, page))
}

/// The addresses a paste at `page` writes out, on any host: its list. None
/// when `page` is no paste, whose text may name anything.
pub fn pasted_links(shown: &[String], page: &str) -> Vec<String> {
    generic::pasted_links(shown, page)
}

/// Direct links for `url`. Folders and lists expand to several entries.
pub async fn resolve(cx: &Cx<'_>, url: &str) -> Result<Vec<Entry>, ResolveError> {
    match kind_of(url) {
        Some(Kind::Pixeldrain) => pixeldrain::resolve(cx, url).await,
        Some(Kind::Gofile) => gofile::resolve(cx, url).await,
        Some(Kind::Buzzheavier) => buzzheavier::resolve(cx, url).await,
        Some(Kind::OneFichier) => onefichier::resolve(cx, url).await,
        Some(Kind::Rapidgator) => rapidgator::resolve(cx, url).await,
        Some(Kind::Nitroflare) => nitroflare::resolve(cx, url).await,
        Some(Kind::Mega) => mega::resolve(cx, url).await,
        Some(Kind::Vikingfile) => vikingfile::resolve(cx, url).await,
        Some(Kind::Rootz) => rootz::resolve(cx, url).await,
        Some(Kind::Mediafire) => mediafire::resolve(cx, url).await,
        Some(Kind::Ddownload) => ddownload::resolve(cx, url).await,
        Some(Kind::Direct) => Ok(vec![Entry {
            url: url.into(),
            name: guess_name(url),
            ..Entry::default()
        }]),
        Some(Kind::Fileguard) => fileguard::unwrap(cx, url).await.map(|u| u.files),
        Some(Kind::Filecrypt) => filecrypt::unwrap(cx, url).await.map(|u| u.files),
        Some(Kind::Mirrored) => mirrored::unwrap(cx, url).await.map(|u| u.files),
        None => generic::resolve(cx, url).await,
    }
}

/// A cheap lookup of the name, size and checksum that does not start a
/// download, so the same file on several hosts can be merged, and a file
/// already downloaded recognised, before anything is fetched. `None` when the
/// host will not say.
pub async fn probe(cx: &Cx<'_>, url: &str) -> Option<Probed> {
    look(cx, url).await.found()
}

/// What a lookup learns of a file without downloading it.
#[derive(Debug, Clone, PartialEq)]
pub enum Look {
    Found(Probed),
    /// The host says it deleted the file (or took it down), in these words.
    Gone(String),
    /// Nothing either way: the host will not say, or did not answer.
    Unknown,
}

impl Look {
    pub fn found(self) -> Option<Probed> {
        match self {
            Self::Found(p) => Some(p),
            _ => None,
        }
    }
}

impl From<Option<Probed>> for Look {
    fn from(p: Option<Probed>) -> Self {
        p.map_or(Self::Unknown, Self::Found)
    }
}

/// [`probe`], from a host that can also say the file was deleted, so a row
/// knows before its turn that a link is dead.
pub async fn look(cx: &Cx<'_>, url: &str) -> Look {
    match kind_of(url) {
        Some(Kind::Vikingfile) => vikingfile::look(cx, url).await,
        Some(Kind::Rootz) => rootz::look(cx, url).await,
        Some(Kind::Ddownload) => ddownload::look(cx, url).await,
        Some(Kind::OneFichier) => onefichier::look(cx, url).await,
        Some(Kind::Mega) => mega::look(cx, url).await,
        _ => probe_quietly(cx, url).await.into(),
    }
}

/// The hosts that say only what a file is, not that it was deleted.
async fn probe_quietly(cx: &Cx<'_>, url: &str) -> Option<Probed> {
    match kind_of(url) {
        Some(Kind::Pixeldrain) => pixeldrain::probe(cx, url).await,
        Some(Kind::Mediafire) => mediafire::probe(cx, url).await,
        // No free metadata route; resolve at download time.
        Some(Kind::Gofile) => None,
        // A wrapper says nothing useful before its solve; that is the
        // resolver's work, once, at download time.
        Some(Kind::Fileguard | Kind::Filecrypt | Kind::Mirrored) => None,
        _ => generic::probe(cx, url).await,
    }
}

/// RFC 5987's `filename*=UTF-8'<language>'<percent-encoded name>`, with the
/// language usually left empty.
static CD_EXT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)filename\*\s*=\s*UTF-8'[^']*'([^;]+)"#).unwrap());
static CD_PLAIN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)filename\s*=\s*"?([^";]+)"#).unwrap());

/// The file name in `Content-Disposition`, preferring the encoded form.
pub(crate) fn disposition_name(headers: &HeaderMap) -> Option<String> {
    let cd = headers.get(CONTENT_DISPOSITION)?.to_str().ok()?;
    let raw = CD_EXT
        .captures(cd)
        .or_else(|| CD_PLAIN.captures(cd))?
        .get(1)?
        .as_str()
        .trim();
    let name = percent_encoding::percent_decode_str(raw)
        .decode_utf8_lossy()
        .trim()
        .to_string();
    (!name.is_empty()).then_some(name)
}

pub(crate) fn content_type(headers: &HeaderMap) -> &str {
    headers
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
}

/// `Content-Length` as sent. Read from the header because reqwest reports a
/// HEAD response's body length, which is always zero.
pub(crate) fn content_length(headers: &HeaderMap) -> Option<u64> {
    headers
        .get(CONTENT_LENGTH)?
        .to_str()
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// `(start, total)` from `Content-Range: bytes START-END/TOTAL`.
pub(crate) fn content_range(headers: &HeaderMap) -> Option<(u64, Option<u64>)> {
    let v = headers.get(CONTENT_RANGE)?.to_str().ok()?.trim();
    let rest = v.strip_prefix("bytes")?.trim_start();
    let (range, total) = rest.split_once('/')?;
    let start = range.split_once('-')?.0.trim().parse().ok()?;
    Some((start, total.trim().parse().ok()))
}

/// Seconds from `Retry-After`, or `default` when absent or a date.
pub(crate) fn retry_after(headers: &HeaderMap, default: u64) -> u64 {
    headers
        .get(RETRY_AFTER)
        .and_then(|v| v.to_str().ok()?.trim().parse().ok())
        .unwrap_or(default)
}

#[cfg(test)]
mod resolver_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::HeaderValue;

    #[test]
    fn hosts_and_names() {
        assert_eq!(
            host_of("https://www.Pixeldrain.com/u/abc"),
            "pixeldrain.com"
        );
        assert_eq!(host_of("http://user:pw@example.org:8080/x"), "example.org");
        assert_eq!(host_of("not a url"), "");
        assert_eq!(
            guess_name("https://h.example/a/file.part01.rar").as_deref(),
            Some("file.part01.rar")
        );
        assert_eq!(
            guess_name("https://h.example/a/My%20File.zip/").as_deref(),
            Some("My File.zip")
        );
        assert_eq!(guess_name("https://h.example/d/abc123"), None);
        assert_eq!(guess_name("https://1fichier.com/?abc&af=1"), None);
        // A host's page for a file names the file; a page of its own names none.
        assert_eq!(
            guess_name("https://rapidgator.net/file/0123abcd/game.part1.rar.html").as_deref(),
            Some("game.part1.rar")
        );
        assert_eq!(
            guess_name("https://h.example/f/Clip%20One.mkv.HTM").as_deref(),
            Some("Clip One.mkv")
        );
        assert_eq!(guess_name("https://h.example/getfile.html"), None);
        assert_eq!(guess_name("https://h.example/download.php?id=7"), None);
    }

    #[test]
    fn every_host_is_known_once() {
        let mut names: Vec<&str> = KNOWN.iter().flat_map(|k| k.names.iter().copied()).collect();
        let all = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), all, "a name in two entries");
        assert!(
            wraps("https://fileguard.cc/abc") && wraps("https://filecrypt.cc/Container/X.html")
        );
        assert!(!wraps("https://pixeldrain.com/u/abc"));
        assert_eq!(known("mega.co.nz").map(|k| k.kind), Some(Kind::Mega));
        // pixeldrain's free limit holds until its account is set.
        let pixeldrain = known("pixeldrain.com").unwrap();
        let mut cfg = Config::default();
        assert_eq!(pixeldrain.at_once(&cfg), Some(5));
        cfg.pixeldrain_key = Some("key".into());
        assert_eq!(pixeldrain.at_once(&cfg), None);
        assert!(pixeldrain.has_account(&cfg));
        // Both halves of a login, or it is none.
        let rapidgator = known("rapidgator.net").unwrap();
        cfg.rapidgator_user = Some("me".into());
        assert!(!rapidgator.has_account(&cfg));
        assert!(
            !known("mega.nz").unwrap().has_account(&cfg),
            "no account to set"
        );
    }

    #[test]
    fn sites() {
        assert_eq!(site_of("https://www.1fichier.com/?abc"), "1fichier.com");
        assert_eq!(site_of("https://a-7.1fichier.com/c123"), "1fichier.com");
        assert_eq!(site_of("https://dl.files.example.co.uk/x"), "example.co.uk");
        assert_eq!(site_of("https://cdn.host.example.de/x"), "example.de");
        assert_eq!(site_of("http://127.0.0.1:8080/x"), "127.0.0.1");
        assert_eq!(site_of("https://localhost/x"), "localhost");
        assert_eq!(site_of("not a url"), "");
    }

    #[test]
    fn clipboard_links() {
        assert!(is_download_link("https://1fichier.com/?abc"));
        assert!(is_download_link("https://www.pixeldrain.com/u/x"));
        assert!(is_download_link(
            "https://cdn.example/files/video.part2.rar"
        ));
        assert!(!is_download_link("https://news.example/story.html"));
        assert!(!is_download_link("https://example.com/index.php"));
        assert!(!is_download_link("https://files.example/a1/video.mkv.html"));
        assert!(!is_download_link("https://github.com/garysassano/haul"));
        assert!(is_download_link("https://fileguard.cc/93d9847d92/1fichier"));
        assert!(is_download_link("https://filecrypt.cc/DLC/0007530C13.dlc"));
        // Pastes and release sites' posts are lists of links to take.
        assert!(is_download_link("https://zpaste.net/p/pd73y"));
        assert!(is_download_link(
            "https://www.elamigosgames.net/games/naruto-x-boruto-ultimate-ninja-storm-pc-espanhol-mega"
        ));
        assert!(!is_download_link("https://www.elamigosgames.net/"));
        assert!(is_download_link(
            "https://filecrypt.cc/Container/0007530C13.html"
        ));
    }

    #[test]
    fn only_wrapper_links_wrap() {
        assert!(wraps("https://fileguard.cc/93d9847d92/1fichier"));
        assert!(wraps("https://fileguard.cc/93d9847d92"));
        assert!(wraps("https://filecrypt.cc/DLC/0007530C13.dlc"));
        assert!(wraps("https://www.filecrypt.cc/Container/0007530C13.html"));
        assert!(!wraps("https://pixeldrain.com/u/VF18oYXa"));
    }

    fn names(entries: &[Entry]) -> Vec<Option<&str>> {
        entries.iter().map(|e| e.name.as_deref()).collect()
    }

    #[test]
    fn links_group_into_files() {
        // One link per host: one file, named after the link that names it,
        // every host a mirror.
        let one = group_mirrors(vec![
            "https://1fichier.com/?cu5oghhjkskhuzap8w43".into(),
            "https://pixeldrain.com/u/VF18oYXa".into(),
            "https://rapidgator.net/file/xyz/Sledders.RUNE.rar".into(),
        ]);
        assert_eq!(names(&one), [Some("Sledders.RUNE.rar")]);
        assert_eq!(one[0].url, "https://1fichier.com/?cu5oghhjkskhuzap8w43");
        assert_eq!(
            one[0].mirrors,
            [
                "https://pixeldrain.com/u/VF18oYXa",
                "https://rapidgator.net/file/xyz/Sledders.RUNE.rar"
            ]
        );

        // A release in parts: every host lists part 1, then part 2.
        let parts = group_mirrors(vec![
            "https://mega.nz/file/a#k1".into(),
            "https://mega.nz/file/b#k2".into(),
            "https://rapidgator.net/file/1/R.part1.rar".into(),
            "https://rapidgator.net/file/2/R.part2.rar".into(),
            "https://pixeldrain.com/u/p1".into(),
            "https://pixeldrain.com/u/p2".into(),
        ]);
        assert_eq!(names(&parts), [Some("R.part1.rar"), Some("R.part2.rar")]);
        assert_eq!(parts[1].url, "https://mega.nz/file/b#k2");
        assert_eq!(
            parts[1].mirrors,
            [
                "https://rapidgator.net/file/2/R.part2.rar",
                "https://pixeldrain.com/u/p2"
            ]
        );

        // Hosts that list different counts: each link on its own.
        let uneven = group_mirrors(vec![
            "https://host.example/a/one.rar".into(),
            "https://host.example/b/two.rar".into(),
            "https://other.example/x".into(),
        ]);
        assert_eq!(names(&uneven), [Some("one.rar"), Some("two.rar"), None]);
        assert!(uneven.iter().all(|e| e.mirrors.is_empty()));

        // One link per host whose names disagree: two files, not one.
        let disagree = group_mirrors(vec![
            "https://host.example/a/one.rar".into(),
            "https://other.example/b/two.rar".into(),
        ]);
        assert_eq!(names(&disagree), [Some("one.rar"), Some("two.rar")]);

        // Another wrapper's links are more links: entries of their own,
        // neither a mirror nor a name for the file.
        let nested = group_mirrors(vec![
            "https://rapidgator.net/file/xyz/Sledders.RUNE.rar".into(),
            "https://fileguard.cc/0123456789/pixeldrain".into(),
            "https://pixeldrain.com/u/VF18oYXa".into(),
            "https://filecrypt.cc/DLC/ABCDEF.dlc".into(),
        ]);
        assert_eq!(
            nested
                .iter()
                .map(|e| (e.url.as_str(), e.name.as_deref(), e.mirrors.len()))
                .collect::<Vec<_>>(),
            [
                (
                    "https://rapidgator.net/file/xyz/Sledders.RUNE.rar",
                    Some("Sledders.RUNE.rar"),
                    1
                ),
                ("https://fileguard.cc/0123456789/pixeldrain", None, 0),
                ("https://filecrypt.cc/DLC/ABCDEF.dlc", None, 0),
            ]
        );
        assert_eq!(page_text(" A &amp; B ").as_deref(), Some("A & B"));
        assert_eq!(
            page_text("Dra.0.10 &#40;15.10.2026&#41; &#x41;&bogus;.rar").as_deref(),
            Some("Dra.0.10 (15.10.2026) A&bogus;.rar")
        );
        assert_eq!(page_text("  "), None);
    }

    #[test]
    fn names_are_tidied_and_recognised_across_hosts() {
        assert_eq!(
            tidy_name("Dra.Awa-ElAmigos.part1 (2).rar"),
            "Dra.Awa-ElAmigos.part1.rar"
        );
        assert_eq!(
            tidy_name("Dra.0.10 .1.1 &amp;#40;15.10.2026&amp;#41;.rar"),
            "Dra.0.10 .1.1 (15.10.2026).rar"
        );
        assert_eq!(
            tidy_name("Game (2024).rar"),
            "Game (2024).rar",
            "a year is not a copy mark"
        );
        assert_eq!(
            name_key("Dra.0.10_.1.1_(.10.202-_1.1.1_(15.10.2026).rar"),
            name_key("Dra.0.10 .1.1 (.10.202- 1.1.1 (15.10.2026).rar")
        );
        assert_eq!(name_key("a.part1 (2).rar"), name_key("A.part1.rar"));
        assert_ne!(name_key("a.part1.rar"), name_key("a.part2.rar"));
        assert_eq!(better_name("a_b.rar".into(), "a b.rar".into()), "a b.rar");
        assert_eq!(better_name("a b.rar".into(), "a_b.rar".into()), "a b.rar");
        // One file on two hosts that spell its name apart is still one file.
        let files = group_mirrors(vec![
            "https://megaup.net/x/A_B.part1.rar".into(),
            "https://rapidgator.net/file/y/A B.part1.rar.html".into(),
        ]);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].name.as_deref(), Some("A B.part1.rar"));
    }

    #[test]
    fn header_parsing() {
        let mut h = HeaderMap::new();
        h.insert(
            CONTENT_DISPOSITION,
            HeaderValue::from_static(
                r#"attachment; filename="a b.zip"; filename*=UTF-8''r%C3%A9sum%C3%A9.zip"#,
            ),
        );
        assert_eq!(disposition_name(&h).as_deref(), Some("résumé.zip"));
        h.insert(
            CONTENT_DISPOSITION,
            HeaderValue::from_static("attachment; filename*=utf-8'en'Caf%C3%A9.7z"),
        );
        assert_eq!(disposition_name(&h).as_deref(), Some("Café.7z"));
        h.insert(
            CONTENT_DISPOSITION,
            HeaderValue::from_static("attachment; filename=plain.rar"),
        );
        assert_eq!(disposition_name(&h).as_deref(), Some("plain.rar"));
        h.insert(
            CONTENT_RANGE,
            HeaderValue::from_static("bytes 100-199/1000"),
        );
        assert_eq!(content_range(&h), Some((100, Some(1000))));
        h.insert(CONTENT_RANGE, HeaderValue::from_static("bytes 0-0/*"));
        assert_eq!(content_range(&h), Some((0, None)));
        h.insert(
            RETRY_AFTER,
            HeaderValue::from_static("Wed, 21 Oct 2015 07:28:00 GMT"),
        );
        assert_eq!(retry_after(&h, 90), 90);
        h.insert(RETRY_AFTER, HeaderValue::from_static("12"));
        assert_eq!(retry_after(&h, 90), 12);
        assert_eq!(remote_dns("socks5://u:p@h:1080"), "socks5h://u:p@h:1080");
        assert_eq!(remote_dns("http://h:8080"), "http://h:8080");
    }

    #[test]
    fn checksums_from_apis_and_headers() {
        // Of the bytes "abc".
        let md5 = "900150983cd24fb0d6963f7d28e17f72";
        let sha256 = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        let sha512 = "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a\
                      2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f";
        assert_eq!(
            Checksum::from_hex(Algo::Md5, &md5.to_uppercase()).map(|c| c.hex),
            Some(md5.into())
        );
        assert_eq!(Checksum::from_hex(Algo::Sha256, md5), None, "too short");
        assert_eq!(
            Checksum::from_hex(Algo::Md5, &format!("zz{}", &md5[2..])),
            None
        );

        let b64 = |hex: &str| {
            let bytes: Vec<u8> = (0..hex.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
                .collect();
            STANDARD.encode(bytes)
        };
        let value = |v: String| HeaderValue::from_str(&v).unwrap();
        let mut h = HeaderMap::new();
        h.insert("content-md5", value(b64(md5)));
        assert_eq!(
            header_checksum(&h, false),
            None,
            "a 206 body is not the file"
        );
        assert_eq!(
            header_checksum(&h, true),
            Checksum::from_hex(Algo::Md5, md5)
        );
        h.insert(
            "x-goog-hash",
            value(format!("crc32c=n03x6A==, md5={}", b64(md5))),
        );
        assert_eq!(
            header_checksum(&h, false),
            Checksum::from_hex(Algo::Md5, md5)
        );
        h.insert(
            "digest",
            value(format!("MD5={}, SHA-256={}", b64(md5), b64(sha256))),
        );
        assert_eq!(
            header_checksum(&h, false),
            Checksum::from_hex(Algo::Sha256, sha256)
        );
        h.insert(
            "repr-digest",
            value(format!(
                "sha-256=:{}:, sha-512=:{}:",
                b64(sha256),
                b64(sha512)
            )),
        );
        assert_eq!(
            header_checksum(&h, false),
            Checksum::from_hex(Algo::Sha512, sha512)
        );
        h.insert("repr-digest", value("sha-256=:dG9vIHNob3J0:".into()));
        assert_eq!(
            header_checksum(&h, false),
            Checksum::from_hex(Algo::Sha256, sha256),
            "a digest of the wrong length is skipped"
        );

        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("abc");
        std::fs::write(&file, "abc").unwrap();
        for (algo, hex) in [
            (Algo::Md5, md5),
            (Algo::Sha256, sha256),
            (Algo::Sha512, sha512),
        ] {
            assert!(
                Checksum::from_hex(algo, hex)
                    .unwrap()
                    .matches(&file)
                    .unwrap()
            );
        }
        let zeros = Checksum::from_hex(Algo::Sha256, &"0".repeat(64)).unwrap();
        assert!(!zeros.matches(&file).unwrap());
    }

    #[tokio::test]
    async fn network_errors_say_what_failed() {
        // Nothing listens on port 1.
        let e = reqwest::get("http://127.0.0.1:1/file?key=secret")
            .await
            .unwrap_err();
        assert!(e.is_connect());
        let text = error_text(e);
        assert!(text.starts_with("error sending request: "), "{text}");
        assert!(text.len() > "error sending request: ".len() + 5, "{text}");
        assert!(
            !text.contains("secret") && !text.contains("127.0.0.1"),
            "{text}"
        );
    }

    #[test]
    fn a_checksum_read_can_wait_and_stop() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("abc");
        std::fs::write(&file, "abc").unwrap();
        let sha = Checksum::from_hex(
            Algo::Sha256,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        )
        .unwrap();
        let asked = std::cell::Cell::new(0);
        let waits_twice = || {
            asked.set(asked.get() + 1);
            if asked.get() <= 2 {
                Steer::Wait
            } else {
                Steer::Go
            }
        };
        assert!(sha.matches_steered(&file, &waits_twice).unwrap());
        assert!(asked.get() > 2);
        let stopped = sha.matches_steered(&file, &|| Steer::Stop).unwrap_err();
        assert_eq!(stopped.kind(), std::io::ErrorKind::Interrupted);
    }
}
