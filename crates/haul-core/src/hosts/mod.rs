//! Per-host resolvers: a page link in, direct links out, with the headers,
//! name and size the host reports.
//!
//! Only legitimate routes are used: public APIs, guest tokens, and the user's
//! own premium or API-key accounts. There is no captcha solving and no
//! wait-timer bypassing; a host that needs a person raises
//! [`ResolveError::NeedsUser`] and the item waits for one.

mod buzzheavier;
mod generic;
mod gofile;
mod mega;
mod nitroflare;
mod onefichier;
mod pixeldrain;
mod rapidgator;

use std::{
    collections::BTreeMap,
    sync::{Arc, LazyLock, Mutex},
    time::Duration,
};

use regex::Regex;
use reqwest::header::{
    CONTENT_DISPOSITION, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, HeaderMap, RETRY_AFTER,
};
use serde::{Deserialize, Serialize};

use crate::config::Config;

/// What a resolver hands back: one downloadable file.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub url: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub size: Option<u64>,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    /// Run this program with `--path <dir> <url>` instead of downloading.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exec: Option<Vec<String>>,
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

impl From<reqwest::Error> for ResolveError {
    fn from(e: reqwest::Error) -> Self {
        let network = e.is_connect() || e.is_timeout();
        // The URL can carry account credentials in its query.
        let message = e.without_url().to_string();
        if network {
            Self::Network(message)
        } else {
            Self::Failed(message)
        }
    }
}

/// Hosts with a resolver of their own.
const KNOWN_HOSTS: [&str; 9] = [
    "pixeldrain.com",
    "gofile.io",
    "buzzheavier.com",
    "1fichier.com",
    "rapidgator.net",
    "nitroflare.com",
    "mega.nz",
    "mega.co.nz",
    "fileditchfiles.st",
];

/// Whether a link is worth adding when it shows up on the clipboard: a host
/// Haul knows, or a link to a file rather than to a web page.
pub fn is_download_link(url: &str) -> bool {
    if KNOWN_HOSTS.contains(&host_of(url).as_str()) {
        return true;
    }
    guess_name(url).is_some_and(|name| {
        let ext = name
            .rsplit('.')
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        !matches!(
            ext.as_str(),
            "html" | "htm" | "shtml" | "xhtml" | "php" | "asp" | "aspx" | "jsp" | "cgi"
        )
    })
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

/// The file name in a link's path, when it looks like one.
pub fn guess_name(url: &str) -> Option<String> {
    let path = url::Url::parse(url).ok()?.path().to_string();
    let last = path.trim_end_matches('/').rsplit('/').next()?;
    let last = percent_encoding::percent_decode_str(last)
        .decode_utf8_lossy()
        .into_owned();
    FILE_NAME.is_match(&last).then_some(last)
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
        _ => generic::resolve(cx, url).await,
    }
}

/// A cheap `(name, size)` lookup that does not start a download, so the same
/// file on several hosts can be merged before anything is fetched. `None`
/// when the host will not say.
pub async fn probe(cx: &Cx<'_>, url: &str) -> Option<(String, Option<u64>)> {
    match host_of(url).as_str() {
        "pixeldrain.com" => pixeldrain::probe(cx, url).await,
        // No free metadata route; resolve at download time.
        "mega.nz" | "mega.co.nz" | "gofile.io" | "1fichier.com" => None,
        _ => generic::probe(cx, url).await,
    }
}

static CD_EXT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)filename\*\s*=\s*UTF-8''([^;]+)"#).unwrap());
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
        assert!(!is_download_link("https://github.com/garysassano/haul"));
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
}
