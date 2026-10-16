//! Per-host resolvers: a page link in, direct links out, with the headers,
//! name and size the host reports.
//!
//! Only legitimate routes are used: public APIs, guest tokens, the user's own
//! premium or API-key accounts, and a wrapper page's own form once its gate is
//! passed. A gate that asks for no person, such as fileguard's ALTCHA proof of
//! work, is solved here; a host that needs a person (a captcha, a browser
//! check, a click) raises [`ResolveError::NeedsUser`] and the item waits for
//! one, and a host's wait timer is waited out.

mod buzzheavier;
mod filecrypt;
mod fileguard;
mod generic;
mod gofile;
mod mega;
mod nitroflare;
mod onefichier;
mod pixeldrain;
mod rapidgator;

use std::{
    collections::BTreeMap,
    io::Read,
    path::Path,
    sync::{Arc, LazyLock, Mutex},
    time::Duration,
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
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
    /// Run this program with `--path <dir> <url>` instead of downloading.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exec: Option<Vec<String>>,
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

/// Hosts with a resolver of their own.
const KNOWN_HOSTS: [&str; 11] = [
    "pixeldrain.com",
    "gofile.io",
    "buzzheavier.com",
    "1fichier.com",
    "rapidgator.net",
    "nitroflare.com",
    "mega.nz",
    "mega.co.nz",
    "fileditchfiles.st",
    "fileguard.cc",
    "filecrypt.cc",
];

/// Whether a link is worth adding when it shows up on the clipboard: a host
/// Haul knows, or a link to a file rather than to a web page.
pub fn is_download_link(url: &str) -> bool {
    if KNOWN_HOSTS.contains(&host_of(url).as_str()) {
        return true;
    }
    // A page that names a file (`name.rar.html`) is still a page.
    last_segment(url).is_some_and(|last| page_extension(&last).is_none())
        && guess_name(url).is_some()
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
}

impl Http {
    pub fn new(proxy: Option<&str>, user_agent: &str) -> Result<Self, String> {
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
            if let Some(p) = proxy {
                b = b.proxy(reqwest::Proxy::all(remote_dns(p)).map_err(|e| format!("proxy: {e}"))?);
            }
            b.build().map_err(|e| e.to_string())
        };
        Ok(Self {
            client: build(true)?,
            no_redirect: build(false)?,
            via_proxy: proxy.is_some(),
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
    pub(crate) gofile: Mutex<Option<String>>,
    pub(crate) rapidgator: Mutex<Option<String>>,
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

/// Whether this link holds further links to resolve (a wrapper page) rather
/// than a file to download: a link a person hands back takes the same path
/// as one added in the dialog.
pub fn unwraps(url: &str) -> bool {
    match host_of(url).as_str() {
        "fileguard.cc" => true,
        "filecrypt.cc" => filecrypt::is_dlc(url),
        _ => false,
    }
}

/// Whether the link is a wrapper's, gated page or container alike: it
/// stands for further links, never for one file.
pub fn wraps(url: &str) -> bool {
    matches!(host_of(url).as_str(), "fileguard.cc" | "filecrypt.cc")
}

/// What a wrapper link points at, so two links to it count as one: every
/// fileguard.cc link of a vault (`/<vault>/<host>`) shows the same page.
pub fn wrapper_key(url: &str) -> String {
    let key = match host_of(url).as_str() {
        "fileguard.cc" => fileguard::key(url),
        "filecrypt.cc" => filecrypt::key(url),
        _ => None,
    };
    key.unwrap_or_else(|| url.to_string())
}

/// The files behind a wrapper link, once its gate is passed.
pub async fn unwrap(cx: &Cx<'_>, url: &str) -> Result<Unwrapped, ResolveError> {
    match host_of(url).as_str() {
        "fileguard.cc" => fileguard::unwrap(cx, url).await,
        "filecrypt.cc" => filecrypt::unwrap(cx, url).await,
        _ => Err(ResolveError::failed("not a wrapper link")),
    }
}

/// Direct links for `url`. Folders and lists expand to several entries.
pub async fn resolve(cx: &Cx<'_>, url: &str) -> Result<Vec<Entry>, ResolveError> {
    match host_of(url).as_str() {
        "pixeldrain.com" => pixeldrain::resolve(cx, url).await,
        "gofile.io" => gofile::resolve(cx, url).await,
        "buzzheavier.com" => buzzheavier::resolve(cx, url).await,
        "1fichier.com" => onefichier::resolve(cx, url).await,
        "rapidgator.net" => rapidgator::resolve(cx, url).await,
        "nitroflare.com" => nitroflare::resolve(cx, url).await,
        "mega.nz" | "mega.co.nz" => mega::resolve(cx, url),
        "fileditchfiles.st" => Ok(vec![Entry {
            url: url.into(),
            name: guess_name(url),
            ..Entry::default()
        }]),
        "fileguard.cc" => fileguard::unwrap(cx, url).await.map(|u| u.files),
        "filecrypt.cc" => filecrypt::unwrap(cx, url).await.map(|u| u.files),
        _ => generic::resolve(cx, url).await,
    }
}

/// A cheap lookup of the name, size and checksum that does not start a
/// download, so the same file on several hosts can be merged, and a file
/// already downloaded recognised, before anything is fetched. `None` when the
/// host will not say.
pub async fn probe(cx: &Cx<'_>, url: &str) -> Option<Probed> {
    match host_of(url).as_str() {
        "pixeldrain.com" => pixeldrain::probe(cx, url).await,
        // No free metadata route; resolve at download time.
        "mega.nz" | "mega.co.nz" | "gofile.io" | "1fichier.com" => None,
        // A wrapper says nothing useful before its solve; that is the
        // resolver's work, once, at download time.
        "fileguard.cc" | "filecrypt.cc" => None,
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
        assert!(is_download_link(
            "https://filecrypt.cc/Container/0007530C13.html"
        ));
    }

    #[test]
    fn only_wrapper_pages_unwrap() {
        assert!(unwraps("https://fileguard.cc/93d9847d92/1fichier"));
        assert!(unwraps("https://fileguard.cc/93d9847d92"));
        assert!(unwraps("https://filecrypt.cc/DLC/0007530C13.dlc"));
        assert!(!unwraps("https://filecrypt.cc/Container/0007530C13.html"));
        assert!(!unwraps("https://pixeldrain.com/u/VF18oYXa"));
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
