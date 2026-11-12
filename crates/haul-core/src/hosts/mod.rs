//! Per-host resolvers: a page link in, direct links out, with the headers,
//! name and size the host reports.
//!
//! The resolvers use these routes today: public APIs, guest tokens, the
//! user's own premium or API-key accounts, and a wrapper page's own form once
//! its gate is passed. A gate Haul can answer without a browser is answered
//! here: the proof of work behind fileguard's ALTCHA and filecrypt's
//! checkbox. A host
//! that needs a person (a captcha, a browser check) raises
//! [`ResolveError::NeedsUser`] and the item waits for one, and a host's wait
//! timer is waited out.

mod buzzheavier;
mod ddownload;
mod filecrypt;
mod fileguard;
pub mod gates;
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

mod checksum;
mod headers;
mod http;
mod known;
mod names;
mod sites;
mod stand_in;

pub use checksum::*;
pub(crate) use headers::*;
pub use http::*;
pub use known::*;
pub use names::*;
use sites::Site;
pub(crate) use stand_in::asked as stand_in_asked;
pub use stand_in::{reach, real, set_stand_in, stand_in};

pub(crate) use generic::gate_message;
pub use generic::{is_paste, is_paste_host, keep_shown_text, pasted_links, short_title};
pub use gofile::{PageAnswer, page_answer};

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
    /// The rounded size a wrapper's page shows for the file, to tell its
    /// files from another wrapper's. Never the file's size: that is
    /// [`Entry::size`], from its host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub listed_size: Option<u64>,
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
    /// The archive password the page gives beside its links
    /// ([`generic::password_in`]), tried first on their archives.
    pub password: Option<String>,
}

/// What a probe learns about a file without downloading it.
#[derive(Debug, Clone, PartialEq)]
pub struct Probed {
    pub name: String,
    pub size: Option<u64>,
    pub checksum: Option<Checksum>,
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ResolveError {
    /// The host asked for a pause (rate limit or free-tier timer).
    #[error("{message}")]
    Wait { seconds: u64, message: String },
    /// The link needs a person, or a browser, before it gives its file.
    #[error("{1}")]
    NeedsUser(Need, String),
    /// The host could not be reached; worth trying again shortly.
    #[error("{0}")]
    Network(String),
    #[error("{0}")]
    Failed(String),
    /// The host says the file is no more (deleted, taken down, a paste that
    /// lists nothing): no use trying the link again on a timer.
    #[error("{0}")]
    Gone(String),
}

/// What a link that needs someone needs: a person, or a browser that may get
/// past by itself, which the desktop app opens out of sight.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Need {
    /// A captcha, a click, an account: a person.
    Person,
    /// A browser check in front of the site (Cloudflare's), which a browser
    /// usually passes by itself.
    Check,
    /// A gate past that check that Haul cannot answer: the browser reads the
    /// page, since a clearance handed to Haul would only bring it back here.
    Gate,
    /// A page on a site Haul has no code for, which a browser may get through
    /// by itself, pressing its download.
    Page,
}

impl ResolveError {
    pub(crate) fn failed(message: impl Into<String>) -> Self {
        Self::Failed(message.into())
    }

    /// A link that needs a person.
    pub(crate) fn person(message: impl Into<String>) -> Self {
        Self::NeedsUser(Need::Person, message.into())
    }

    pub(crate) fn gone(message: impl Into<String>) -> Self {
        Self::Gone(message.into())
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

/// The allowance of the address `cx` reaches `host` from, for a host that
/// tells ([`Known::tells_allowance`]).
pub async fn allowance(cx: &Cx<'_>, host: &str) -> Option<Allowance> {
    known(host)?.site.allowance(cx).await
}

/// Whether a link is worth adding when it shows up on the clipboard: a host
/// Haul knows, or a link to a file rather than to a web page.
pub fn is_download_link(url: &str) -> bool {
    if known_of(url).is_some() || is_paste(url) {
        return true;
    }
    // A page that names a file (`name.rar.html`), or whose address ends
    // like a folder's (a forum's `/threads/name.12345/`), is still a page.
    let folder = url::Url::parse(url).is_ok_and(|u| u.path().ends_with('/'));
    !folder
        && last_segment(url).is_some_and(|last| page_extension(&last).is_none())
        && guess_name(url).is_some()
}

/// Whether `url` is a well-formed `http` or `https` address.
pub fn is_http(url: &str) -> bool {
    url::Url::parse(url).is_ok_and(|u| matches!(u.scheme(), "http" | "https"))
}

/// Whether `url` is a web page that may list download links: on a site
/// Haul has no code for, and not the site's front page.
pub fn is_web_page(url: &str) -> bool {
    may_list_links(url)
        && url::Url::parse(url).is_ok_and(|u| {
            matches!(u.scheme(), "http" | "https") && u.path().trim_matches('/').len() > 1
        })
}

/// Whether clipboard watching looks at `url` as a copied page: a web page
/// ([`is_web_page`]) whose address does not look like a one-time link
/// ([`ONE_TIME_WORDS`]).
pub fn is_copied_page(url: &str) -> bool {
    is_web_page(url) && !looks_one_time(url)
}

/// Words that make a copied page's address look like a one-time link, as
/// whole words of a path segment or a query key (`access_token`,
/// `verify-email`, `sign-in`, but not `authors`), in any case: a plain
/// GET can use such a link up (a sign-in, an email's confirmation, an
/// unsubscribe, a password reset), so clipboard watching does not look at
/// it. The user approved this, and only for copied pages
/// ([`is_copied_page`]), on 2026-10-24; links pasted into Add, dropped or
/// in containers, and copied links to known hosts, files, wrappers and
/// pastes, are not affected.
const ONE_TIME_WORDS: [&str; 16] = [
    "token",
    "confirm",
    "verify",
    "verification",
    "unsubscribe",
    "login",
    "signin",
    "sign-in",
    "logout",
    "reset",
    "activate",
    "magic",
    "otp",
    "auth",
    "callback",
    "oauth",
];

/// Whether a path segment or query key of `url` holds one of
/// [`ONE_TIME_WORDS`].
fn looks_one_time(url: &str) -> bool {
    let Ok(u) = url::Url::parse(url) else {
        return false;
    };
    let segments = u.path_segments().into_iter().flatten().map(|s| {
        percent_encoding::percent_decode_str(s)
            .decode_utf8_lossy()
            .into_owned()
    });
    let keys = u.query_pairs().map(|(key, _)| key.into_owned());
    segments.chain(keys).any(|part| {
        let part = part.to_ascii_lowercase();
        let words: Vec<&str> = part
            .split(|c: char| !c.is_ascii_alphanumeric())
            .filter(|w| !w.is_empty())
            .collect();
        // Two words in a row stand for a hyphenated one (`sign-in`).
        let pairs = words.windows(2).map(|w| format!("{}-{}", w[0], w[1]));
        words
            .iter()
            .map(|w| (*w).to_string())
            .chain(pairs)
            .any(|w| ONE_TIME_WORDS.contains(&w.as_str()))
    })
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

/// Whether the link is a wrapper's, gated page or container alike: it
/// stands for further links, never for one file, whether it is added or a
/// person hands it back.
pub fn wraps(url: &str) -> bool {
    wraps_host(&host_of(url))
}

/// [`wraps`] for host `name`.
pub fn wraps_host(name: &str) -> bool {
    known(name).is_some_and(Known::wraps)
}
/// The files in a wrapper's list of links. A wrapper lists each host's links
/// in the same order (one link per host for a single file; part 1, part 2…
/// on every host for a multi-part archive), so when every host has as many
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
    known_of(url)
        .and_then(|k| k.site.key(url))
        .unwrap_or_else(|| url.to_string())
}

/// The files behind a wrapper link, once its gate is passed. A web page on
/// a site Haul has no code for (a post that lists links, a paste) unwraps to the
/// links it shows.
pub async fn unwrap(cx: &Cx<'_>, url: &str) -> Result<Unwrapped, ResolveError> {
    match known_of(url) {
        Some(k) => k.site.unwrap(cx, url).await,
        None => {
            let (title, links, password) = generic::page(cx, url).await?;
            Ok(Unwrapped {
                password,
                ..listed(title, links)
            })
        }
    }
}

/// What a wrapper's page holds, read off the page as the desktop app's
/// browser shows it past a gate Haul cannot answer itself; `None` when it
/// shows nothing Haul reads there.
pub fn shown(url: &str, html: &str) -> Option<Unwrapped> {
    known_of(url)?.site.shown(html)
}

/// A page's links as what unwrapping it found: one entry each, resolved
/// when its turn comes.
pub fn listed(title: Option<String>, links: Vec<String>) -> Unwrapped {
    Unwrapped {
        password: None,
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

/// Whether `url` stands for more links rather than a file: a wrapper's, or
/// a paste.
pub fn lists(url: &str) -> bool {
    wraps(url) || is_paste(url)
}

/// What a paste that lists no links says of itself, whether Haul read it or
/// the desktop app passed it out of sight.
pub fn lists_nothing(url: &str) -> String {
    format!("the paste on {} lists no links", host_of(url))
}

/// What a host whose free download is its page's (a wait, then a captcha)
/// asks of a person without its account.
pub(super) const FREE_ON_PAGE: &str =
    "a free download is on its page (a wait, then a captcha): open it, or set the account";

/// Whether link `url`'s address names a file (an archive's part, a video):
/// one the desktop app's browser was sent to is the file itself.
pub fn names_a_file(url: &str) -> bool {
    generic::names_file(url)
}

/// Whether `url` may be a web page that lists links: a host Haul has no
/// code for.
pub fn may_list_links(url: &str) -> bool {
    known_of(url).is_none()
}

/// Whether Haul's own code reads `url` once a browser has passed the
/// check in front of its site (Cloudflare's, [`is_guarded`]): a wrapper's
/// or a host's page, but not one whose own page is passed for its file
/// ([`page_passes`]).
pub fn reads_past_check(url: &str) -> bool {
    known_of(url).is_some() && !page_passes(url)
}

/// The links in `text` (addresses, one per line or in a page's markup) to
/// hosts Haul knows, to wrappers and to pastes, as [`look`] keeps them
/// from the page at `page`.
pub fn listed_links(text: &str, page: &str) -> Vec<String> {
    url::Url::parse(page).map_or_else(|_| Vec::new(), |base| generic::links_in(text, &base, page))
}

/// Direct links for `url`. Folders and lists expand to several entries.
pub async fn resolve(cx: &Cx<'_>, url: &str) -> Result<Vec<Entry>, ResolveError> {
    match known_of(url) {
        Some(k) => k.site.resolve(cx, url).await,
        None => generic::resolve(cx, url).await,
    }
}

/// What a lookup learns of a file without downloading it.
#[derive(Debug, Clone, PartialEq)]
pub enum Look {
    Found(Probed),
    /// The host says it deleted the file (or took it down), in these words.
    Gone(String),
    /// Nothing either way: the host will not say, or did not answer.
    Unknown,
    /// A web page on a host Haul has no code for, read: its own name for
    /// what it holds, the file its title names, and the links it lists
    /// ([`listed_links`]).
    Page {
        title: Option<String>,
        named: Option<String>,
        links: Vec<String>,
        /// The archive password it gives beside them.
        password: Option<String>,
        /// The gate it shows instead, when it shows one ([`gates::detect`]).
        gate: Option<gates::Gate>,
    },
    /// A page behind a browser check a browser usually passes by itself
    /// (Cloudflare's).
    Guarded,
}

impl Look {
    /// The file this is, as far as it tells: a page counts by the file its
    /// title names.
    pub fn found(self) -> Option<Probed> {
        match self {
            Self::Found(p) => Some(p),
            Self::Page {
                named: Some(name), ..
            } => Some(Probed {
                name,
                size: None,
                checksum: None,
            }),
            _ => None,
        }
    }
}

impl From<Option<Probed>> for Look {
    fn from(p: Option<Probed>) -> Self {
        p.map_or(Self::Unknown, Self::Found)
    }
}

/// Whether `url`'s host says what a file is (its size, whether it is still
/// there) through an API, without a download or its page: such a link is
/// looked up as it is added even when it names its file, so its size counts
/// in its archive's total before it downloads.
pub fn tells_size(url: &str) -> bool {
    known(&host_of(url)).is_some_and(|k| k.tells_size)
}

/// What a lookup that can tell a deleted file came to, as a [`Look`].
pub(super) fn look_from(found: Result<Option<Probed>, ResolveError>) -> Look {
    match found {
        Ok(Some(p)) => Look::Found(p),
        Err(ResolveError::Gone(m)) => Look::Gone(m),
        Ok(None) | Err(_) => Look::Unknown,
    }
}

/// A cheap lookup of the name, size and checksum that does not start a
/// download, so the same file on several hosts can be merged, and a file
/// already downloaded recognised, before anything is fetched; from a host
/// that can also say the file was deleted, so a row knows before its turn
/// that a link is dead.
pub async fn look(cx: &Cx<'_>, url: &str) -> Look {
    match known_of(url) {
        Some(k) => k.site.look(cx, url).await,
        None => generic::look(cx, url).await,
    }
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
        // A host's other domain counts as its main one; its site stays its own.
        assert_eq!(host_of("https://vik1ngfile.site/f/abc"), "vikingfile.com");
        assert_eq!(host_of("https://ddl.to/abc"), "ddownload.com");
        assert_eq!(host_of("https://transfer.it/t/abc"), "transfer.it");
        assert_eq!(site_of("https://vik1ngfile.site/f/abc"), "vik1ngfile.site");
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
            guess_name("https://rapidgator.net/file/0123abcd/archive.part1.rar.html").as_deref(),
            Some("archive.part1.rar")
        );
        assert_eq!(
            guess_name("https://h.example/f/Clip%20One.mkv.HTM").as_deref(),
            Some("Clip One.mkv")
        );
        assert_eq!(guess_name("https://h.example/getfile.html"), None);
        assert_eq!(guess_name("https://h.example/download.php?id=7"), None);
        // A link that names its file after `#`; a key or an anchor names none.
        assert_eq!(
            guess_name("https://fuckingfast.net/abc#Archive.part01.rar").as_deref(),
            Some("Archive.part01.rar")
        );
        assert_eq!(guess_name("https://mega.nz/file/abc#k3yAbC"), None);
        assert_eq!(guess_name("https://mega.nz/#!abc!k3y.zip"), None);
        assert_eq!(guess_name("https://news.example/post#comments"), None);
        assert_eq!(
            guess_name("https://h.example/f/file.rar#x.zip").as_deref(),
            Some("file.rar")
        );
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
        assert_eq!(known("mega.co.nz").map(|k| k.names[0]), Some("mega.nz"));
        // A host's other domain is one of its entry's names, so the table and
        // the aliases cannot drift apart (not every name is an alias:
        // transfer.it is mega's but keeps its own name). A paste site's other
        // domain stands for the paste site.
        for (alias, name) in ALIASES {
            match known(name) {
                Some(entry) => assert!(
                    entry.names.contains(alias),
                    "{alias} is not among {name}'s names"
                ),
                None => assert!(
                    generic::PASTES.contains(name),
                    "{name}, which {alias} stands for, is neither a known host nor a paste site"
                ),
            }
        }
        // pixeldrain's free limit holds until its account is set.
        let pixeldrain = known("pixeldrain.com").unwrap();
        let mut cfg = Config::default();
        assert_eq!(pixeldrain.at_once(&cfg), Some(5));
        cfg.accounts.insert("pixeldrain_key".into(), "key".into());
        assert_eq!(pixeldrain.at_once(&cfg), None);
        assert!(pixeldrain.has_account(&cfg));
        // Both halves of a login, or it is none.
        let rapidgator = known("rapidgator.net").unwrap();
        cfg.accounts.insert("rapidgator_user".into(), "me".into());
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
        assert!(!is_download_link("https://github.com/lordsilver/haul"));
        // A forum's thread, whatever its number looks like.
        assert!(!is_download_link(
            "https://forum.example/threads/some-files.12345/"
        ));
        // A link that names its file after `#` is a file's.
        assert!(is_download_link(
            "https://fuckingfast.net/abc123def456#Archive.part01.rar"
        ));
        assert!(is_download_link("https://fileguard.cc/9f8e7d6c5b/1fichier"));
        assert!(is_download_link("https://filecrypt.cc/DLC/0123ABCDEF.dlc"));
        // Pastes are lists of links to take.
        assert!(is_download_link("https://zpaste.net/p/q7r6s"));
        // Any other page may list download links: looked at, not taken
        // as is; a site's front page is neither.
        assert!(is_web_page("https://downloads.example/files/some-files"));
        assert!(!is_web_page("https://downloads.example/"));
        assert!(!is_web_page("https://pixeldrain.com/u/x"));
        // A copied page whose address looks like a one-time link is not
        // looked at: a plain GET can use it up.
        for url in [
            "https://shop.example/account/confirm?token=abc123",
            "https://news.example/unsubscribe/abc",
            "https://forum.example/login",
            "https://app.example/verify-email/abc",
            "https://app.example/account/sign-in",
            "https://app.example/landing?access_token=x",
        ] {
            assert!(!is_copied_page(url), "{url}");
        }
        // Whole words only: a word that merely holds one is no sign.
        for url in [
            "https://downloads.example/files/some-files",
            "https://forum.example/threads/name.12345/",
            "https://blog.example/authors/some-files",
            "https://blog.example/tokenomics-files",
        ] {
            assert!(is_copied_page(url), "{url}");
        }
        assert!(is_download_link(
            "https://filecrypt.cc/Container/0123ABCDEF.html"
        ));
    }

    #[test]
    fn only_wrapper_links_wrap() {
        assert!(wraps("https://fileguard.cc/9f8e7d6c5b/1fichier"));
        assert!(wraps("https://fileguard.cc/9f8e7d6c5b"));
        assert!(wraps("https://filecrypt.cc/DLC/0123ABCDEF.dlc"));
        assert!(wraps("https://www.filecrypt.cc/Container/0123ABCDEF.html"));
        assert!(!wraps("https://pixeldrain.com/u/AbCd1234"));
    }

    fn names(entries: &[Entry]) -> Vec<Option<&str>> {
        entries.iter().map(|e| e.name.as_deref()).collect()
    }

    #[test]
    fn links_group_into_files() {
        // One link per host: one file, named after the link that names it,
        // every host a mirror.
        let one = group_mirrors(vec![
            "https://1fichier.com/?a1b2c3d4e5f6g7h8i9j0".into(),
            "https://pixeldrain.com/u/AbCd1234".into(),
            "https://rapidgator.net/file/xyz/Example.Set.rar".into(),
        ]);
        assert_eq!(names(&one), [Some("Example.Set.rar")]);
        assert_eq!(one[0].url, "https://1fichier.com/?a1b2c3d4e5f6g7h8i9j0");
        assert_eq!(
            one[0].mirrors,
            [
                "https://pixeldrain.com/u/AbCd1234",
                "https://rapidgator.net/file/xyz/Example.Set.rar"
            ]
        );

        // A multi-part archive: every host lists part 1, then part 2.
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
            "https://rapidgator.net/file/xyz/Example.Set.rar".into(),
            "https://fileguard.cc/0123456789/pixeldrain".into(),
            "https://pixeldrain.com/u/AbCd1234".into(),
            "https://filecrypt.cc/DLC/ABCDEF.dlc".into(),
        ]);
        assert_eq!(
            nested
                .iter()
                .map(|e| (e.url.as_str(), e.name.as_deref(), e.mirrors.len()))
                .collect::<Vec<_>>(),
            [
                (
                    "https://rapidgator.net/file/xyz/Example.Set.rar",
                    Some("Example.Set.rar"),
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
            tidy_name("Example.Set.part1 (2).rar"),
            "Example.Set.part1.rar"
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
                    .matches_steered(&file, &|| Steer::Go)
                    .unwrap()
            );
        }
        let zeros = Checksum::from_hex(Algo::Sha256, &"0".repeat(64)).unwrap();
        assert!(!zeros.matches_steered(&file, &|| Steer::Go).unwrap());
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
