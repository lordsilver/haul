//! Any host without its own resolver: a link that answers with a file is used
//! as is; one that answers with a web page needs a person, unless the page
//! links files on hosts Haul knows, wrappers, or files on other sites (a
//! post on a blog or forum that lists download links, say): then those links are added as
//! if pasted
//! ([`look`]).

use std::sync::LazyLock;

use regex::Regex;
use reqwest::header::{ACCEPT_ENCODING, RANGE};

use super::{
    Cx, Entry, FILE_EXTENSIONS, Look, Need, Probed, ResolveError, content_length, content_range,
    content_type, disposition_name, gates, gone_or_broken, guess_name, header_checksum, host_of,
    known, rate_limited, real, site_of, wraps,
};

pub(super) async fn resolve(cx: &Cx<'_>, url: &str) -> Result<Vec<Entry>, ResolveError> {
    // HEAD first, for the headers it is cheaper for. A 405/501, or a host
    // that never answers it (transfer.it stalls HEAD), gets one byte through
    // a ranged GET instead.
    // Sizes as stored: a compressed answer would report another length.
    let head = cx
        .http
        .client
        .head(url)
        .header(ACCEPT_ENCODING, "identity")
        .send()
        .await;
    let get = || {
        cx.http
            .client
            .get(url)
            .header(RANGE, "bytes=0-0")
            .header(ACCEPT_ENCODING, "identity")
            .send()
    };
    let (r, headed) = match head {
        Ok(r) if !matches!(r.status().as_u16(), 405 | 501) => (r, true),
        _ => (get().await?, false),
    };
    let status = r.status().as_u16();
    // A rate limit carrying a check is a wait for the host, not a box for
    // a person (plan PASS-07): waited out first.
    if status == 429 {
        rate_limited(&r)?;
    }
    if challenged(r.headers()) {
        // A check answers HEAD with no page, which names its kind.
        let r = match headed {
            true => get()
                .await
                .ok()
                .filter(|g| challenged(g.headers()))
                .unwrap_or(r),
            false => r,
        };
        return Err(guarded_by(url, r).await);
    }
    rate_limited(&r)?;
    if status < 400 && !content_type(r.headers()).contains("text/html") {
        let size = match content_range(r.headers()) {
            Some((_, total)) => total,
            None => content_length(r.headers()),
        };
        // Its real form in stand-in mode, as a list names the same file.
        let final_url = real(r.url().as_str()).into_owned();
        let name = disposition_name(r.headers())
            .or_else(|| guess_name(&final_url))
            .or_else(|| guess_name(url));
        return Ok(vec![Entry {
            url: final_url,
            name,
            size,
            checksum: header_checksum(r.headers(), status == 200),
            ..Entry::default()
        }]);
    }
    gone_or_broken(&r)?;
    // The page's start, for the gate it shows: the row can then say what
    // stands in the way. A HEAD gave no body to read it from.
    let gate = match cx
        .http
        .client
        .get(url)
        .header(ACCEPT_ENCODING, "identity")
        .send()
        .await
    {
        Ok(mut page) if is_html(&page) => {
            gates::detect(&String::from_utf8_lossy(&read(&mut page, PEEK).await))
        }
        _ => None,
    };
    Err(gated_page(url, gate))
}

/// Why a page on a site Haul has no code for wants a browser: the gate it
/// shows, or no gate Haul knows (a timer, a button). One a browser gets past
/// by itself, or none, is passed out of sight by the desktop app, which looks
/// for the page's download there ([`Need::Page`]); one that needs a person
/// goes to them.
fn gated_page(url: &str, gate: Option<gates::Gate>) -> ResolveError {
    match gate {
        Some(gate) if gate.passes_in_browser() => ResolveError::NeedsUser(
            Need::Page,
            format!(
                "{} shows {}, which a browser passes by itself",
                host_of(url),
                gate.name()
            ),
        ),
        Some(gate) => ResolveError::person(format!(
            "{} shows {}: it wants a person",
            host_of(url),
            gate.name()
        )),
        None => ResolveError::NeedsUser(
            Need::Page,
            format!(
                "{} answers with a web page: it wants a browser (captcha, timer or a click)",
                host_of(url)
            ),
        ),
    }
}

/// Whether a site answered with Cloudflare's browser check instead of the
/// page: a person's browser usually passes it by itself.
pub(super) fn challenged(headers: &reqwest::header::HeaderMap) -> bool {
    headers
        .get("cf-mitigated")
        .is_some_and(|v| v.as_bytes().eq_ignore_ascii_case(b"challenge"))
}

/// What a page behind a browser check needs: a browser, which the desktop
/// app opens out of sight. The check's own page `r` says what kind of check
/// it is ([`challenge_kind`]), which the row names.
/// Where `r` answered from, in its real form in stand-in mode
/// ([`real`]): the base a page's relative links are read against, so they
/// name the same addresses as its absolute ones.
fn answered_at(r: &reqwest::Response) -> url::Url {
    url::Url::parse(&real(r.url().as_str())).unwrap_or_else(|_| r.url().clone())
}

pub(super) async fn guarded_by(url: &str, mut r: reqwest::Response) -> ResolveError {
    let page = read(&mut r, PEEK).await;
    let kind = challenge_kind(&String::from_utf8_lossy(&page))
        .map(|k| format!(", {k}"))
        .unwrap_or_default();
    ResolveError::NeedsUser(
        Need::Check,
        format!(
            "{} is behind a browser check (Cloudflare's{kind})",
            host_of(url)
        ),
    )
}

static CHALLENGE_KIND: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"cType:\s*'([A-Za-z][\w-]*)'").unwrap());

/// The kind of check Cloudflare's check page says it is (its `cType`), in
/// words: "managed", "non interactive" (a wait page), "interactive" (a box
/// to tick), or another it names, such as filecrypt.cc's "precursor
/// interstitial" (2026-11-07).
fn challenge_kind(page: &str) -> Option<String> {
    let kind = &CHALLENGE_KIND.captures(page)?[1];
    Some(kind.replace(['_', '-'], " "))
}

/// What a page needs when it answers with a gate Haul has no code for, past
/// any check in front of its site: a browser, which may be let through. The
/// gate, when Haul knows it, is named.
pub(super) fn gated(url: &str, gate: Option<gates::Gate>) -> ResolveError {
    ResolveError::NeedsUser(Need::Gate, gate_message(url, gate))
}

/// What the row says of a gate Haul cannot answer on `url`'s page.
pub(crate) fn gate_message(url: &str, gate: Option<gates::Gate>) -> String {
    let named = gate.map(|g| format!(" ({})", g.name())).unwrap_or_default();
    format!("{} shows a gate Haul cannot answer{named}", host_of(url))
}

const PEEK: usize = 64 * 1024;

static OG_TITLE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)property="og:title"\s+content="([^"]+)""#).unwrap());
static TITLE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)<title>([^<]+)</title>").unwrap());
static FILE_IN_TITLE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(r#"(?i)([^\s<>/:*?"|]+\.(?:{FILE_EXTENSIONS})\b)"#)).unwrap()
});

/// Name and size from the headers of a file, or the name from the title of a
/// page, reading at most the first 64 KiB.
pub(super) async fn probe(cx: &Cx<'_>, url: &str) -> Option<Probed> {
    let mut r = cx
        .http
        .client
        .get(url)
        .header(RANGE, format!("bytes=0-{}", PEEK - 1))
        .header(ACCEPT_ENCODING, "identity")
        .send()
        .await
        .ok()?;
    if !is_html(&r) {
        return file_probed(&r);
    }
    let mut page = read(&mut r, PEEK).await;
    page.truncate(PEEK);
    named_in_title(&String::from_utf8_lossy(&page)).map(|name| Probed {
        name,
        size: None,
        checksum: None,
    })
}

/// Whether `r` is a web page that answered (not an error's page).
fn is_html(r: &reqwest::Response) -> bool {
    r.status().as_u16() < 400 && content_type(r.headers()).contains("html")
}

/// What the headers of a file (or of an error) say of it.
fn file_probed(r: &reqwest::Response) -> Option<Probed> {
    let status = r.status().as_u16();
    let name = disposition_name(r.headers()).or_else(|| {
        r.status()
            .is_success()
            .then(|| guess_name(r.url().as_str()))
            .flatten()
    })?;
    let size = match status {
        206 => content_range(r.headers()).and_then(|(_, total)| total),
        200 => content_length(r.headers()),
        _ => None,
    };
    Some(Probed {
        name,
        size,
        checksum: header_checksum(r.headers(), status == 200),
    })
}

/// The file a page's title names (`Download Archive.part1.rar - Host`).
fn named_in_title(page: &str) -> Option<String> {
    [&OG_TITLE, &TITLE].into_iter().find_map(|pattern| {
        let title = pattern.captures(page)?;
        Some(FILE_IN_TITLE.captures(&title[1])?[1].to_string())
    })
}

/// Up to about `most` bytes of `r`'s body.
async fn read(r: &mut reqwest::Response, most: usize) -> Vec<u8> {
    let mut page = Vec::new();
    while page.len() < most {
        match r.chunk().await {
            Ok(Some(chunk)) => page.extend_from_slice(&chunk),
            _ => break,
        }
    }
    page
}

/// What `url` is, from one request: a file (its headers, as [`probe`]
/// reads them), a page behind a browser check, or a web page read for its
/// links and for the file its title names. A page that sent only its start
/// for the range asked is fetched once more, whole.
pub(super) async fn look(cx: &Cx<'_>, url: &str) -> Look {
    let peek = |range: bool| {
        let get = cx.http.client.get(url).header(ACCEPT_ENCODING, "identity");
        match range {
            true => get.header(RANGE, format!("bytes=0-{}", PEEK - 1)),
            false => get,
        }
        .send()
    };
    let Ok(mut r) = peek(true).await else {
        return Look::Unknown;
    };
    if challenged(r.headers()) {
        return Look::Guarded;
    }
    if !is_html(&r) {
        return file_probed(&r).into();
    }
    let mut page = read(&mut r, PAGE).await;
    let more = r.status().as_u16() == 206
        && content_range(r.headers())
            .is_none_or(|(_, total)| total.is_none_or(|total| total > page.len() as u64));
    if more {
        match peek(false).await {
            Ok(whole) if is_html(&whole) => {
                r = whole;
                page = read(&mut r, PAGE).await;
            }
            _ => {}
        }
    }
    let page = String::from_utf8_lossy(&page);
    Look::Page {
        title: page_title(&page),
        named: named_in_title(&page),
        links: links_in(&page, &answered_at(&r), url),
        password: password_in(&page),
        gate: gates::detect(&page),
    }
}

/// The most of a page read for its links.
const PAGE: usize = 4 * 1024 * 1024;

/// Addresses in a page: in `href`s, and written out in its text.
static HREF: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)\bhref\s*=\s*["']([^"'#\s]+)"#).unwrap());
static BARE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"https?://[^\s"'<>()\[\]{}]+"#).unwrap());

/// An address written out in a paste's text, with or without `http(s)://`
/// (`site.example/abc123`): those without are kept only on hosts, wrappers
/// and pastes Haul knows ([`pasted_links`]).
static WRITTEN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)(?:https?://)?(?:[a-z0-9-]+\.)+[a-z]{2,}/[^\s"'<>()\[\]{}]+"#).unwrap()
});

/// The tags of what a page embeds (videos, sounds, pictures): their
/// addresses are what the page plays or shows, not links it lists.
static EMBEDDED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)<(?:video|audio|source|track|img|embed|picture)\b[^>]*>").unwrap()
});

/// The links of a web page to files on hosts Haul knows, to wrappers and to
/// files on other sites, each once, in the page's order. Links elsewhere (the site's own pages,
/// ads) are left out; a page with none waits for a person as before.
/// With them, the page's own name for what it holds.
pub(super) async fn page(
    cx: &Cx<'_>,
    url: &str,
) -> Result<(Option<String>, Vec<String>, Option<String>), ResolveError> {
    let mut r = cx.http.client.get(url).send().await?;
    let status = r.status().as_u16();
    if status == 429 {
        rate_limited(&r)?;
    }
    if challenged(r.headers()) {
        return Err(guarded_by(url, r).await);
    }
    rate_limited(&r)?;
    if status >= 400 {
        return Err(ResolveError::Failed(format!("HTTP {status}")));
    }
    if !content_type(r.headers()).contains("html") {
        return Err(ResolveError::failed("not a web page"));
    }
    let base = answered_at(&r);
    let page = read(&mut r, PAGE).await;
    let page = String::from_utf8_lossy(&page);
    let mut urls = links_in(&page, &base, url);
    if is_paste(url) {
        // A paste's whole list, on any host, as its window would give it.
        for link in paste_text_links(&page, url) {
            if !urls.contains(&link) {
                urls.push(link);
            }
        }
        if urls.is_empty() {
            keep_paste_text(url, &page);
            return Err(ResolveError::gone(super::lists_nothing(url)));
        }
    }
    if urls.is_empty() {
        return Err(match gates::detect(&page) {
            Some(gate) => gated_page(url, Some(gate)),
            None => ResolveError::NeedsUser(
                Need::Page,
                format!(
                    "{} answers with a web page that links no host Haul knows: it wants a browser",
                    host_of(url)
                ),
            ),
        });
    }
    Ok((page_title(&page), urls, password_in(&page)))
}

/// A page's name for itself: its `og:title`, else its `<title>`, the site's
/// name after a separator left off.
pub(super) fn page_title(page: &str) -> Option<String> {
    let raw = [&OG_TITLE, &TITLE]
        .into_iter()
        .find_map(|re| re.captures(page).map(|c| c[1].to_string()))?;
    short_title(&super::decode_entities(&raw))
}

/// A page's title without the site's name after a separator ("Example |
/// Site" is "Example").
pub fn short_title(raw: &str) -> Option<String> {
    let name = [" | ", " – ", " — ", " - ", " » "]
        .iter()
        .fold(raw, |name, sep| name.split(sep).next().unwrap_or(name))
        .trim();
    // Download sites title every page "Download …".
    let name = name
        .strip_prefix("Download ")
        .or_else(|| name.strip_prefix("Descargar "))
        .unwrap_or(name)
        .trim()
        .to_string();
    (!name.is_empty()).then_some(name)
}

/// Whether `url` is a paste (zpaste.net, paste2.org…): a list of links,
/// taken as a wrapper from the start.
pub fn is_paste(url: &str) -> bool {
    is_paste_host(&host_of(url))
}

/// [`is_paste`] for host `name`.
pub fn is_paste_host(name: &str) -> bool {
    !super::generic_only() && PASTES.contains(&name)
}

/// The paste sites Haul recognises today, a fixed list: sites that hold
/// lists of links. A download page often links one per host instead of
/// the hosts' links, so they are read in turn, each as a page of links.
pub(super) const PASTES: &[&str] = &[
    "zpaste.net",
    "paste2.org",
    "pastebin.com",
    "rentry.co",
    "justpaste.it",
    "controlc.com",
    "pastes.io",
    "paste.ee",
    "privatebin.net",
    "anotepad.com",
];

/// Scripts and styles, whose addresses are no paste's list.
static SCRIPT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?is)<script\b.*?</script\s*>|<style\b.*?</style\s*>").unwrap());
static TAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)<[^>]*>").unwrap());

/// The most of a paste's text the log keeps.
const PASTE_KEPT: usize = 2048;

/// What a paste that lists no links (at `url`, its markup `page`) holds, in
/// the log only (a `paste` signal), never in a notification or the UI: its
/// text's length and its first 2 KB, so a report shows what it held.
fn keep_paste_text(url: &str, page: &str) {
    keep_shown_text(url, &paste_text(page), None);
}

/// [`keep_paste_text`] for the text a browser showed of the paste: `text`,
/// of `length` characters in all when it was cut short.
pub fn keep_shown_text(url: &str, text: &str, length: Option<usize>) {
    let mut cut = text.len().min(PASTE_KEPT);
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    crate::debug::emit(
        "paste",
        serde_json::json!({ "url": url, "length": length.unwrap_or_else(|| text.chars().count()), "text": &text[..cut] }),
    );
}

/// The text a paste's window shows (`page`, its markup): without its
/// scripts, styles and tags, references decoded, each run of blanks one.
fn paste_text(page: &str) -> String {
    let text = SCRIPT.replace_all(page, " ");
    let text = super::decode_entities(&TAG.replace_all(&text, " "));
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The addresses the paste at `url` writes out in its text (`page`, its
/// markup), on any host, in order: what its window shows, without the
/// addresses in its scripts, styles and tags (menus, ads).
fn paste_text_links(page: &str, url: &str) -> Vec<String> {
    let text = SCRIPT.replace_all(page, " ");
    let text = super::decode_entities(&TAG.replace_all(&text, " "));
    let shown: Vec<String> = WRITTEN
        .find_iter(&text)
        .map(|m| m.as_str().to_string())
        .collect();
    pasted_links(&shown, url)
}

/// The addresses a paste at `page` writes out, on any host: its list. None
/// when `page` is no paste, whose text may name anything.
pub fn pasted_links(shown: &[String], page: &str) -> Vec<String> {
    let site = host_of(page);
    if !PASTES.contains(&site.as_str()) {
        return Vec::new();
    }
    let mut seen = std::collections::HashSet::new();
    shown
        .iter()
        .filter_map(|s| {
            let lower = s.to_ascii_lowercase();
            if lower.starts_with("http://") || lower.starts_with("https://") {
                return url::Url::parse(s).ok();
            }
            // Written without its scheme: an address only on a site Haul
            // knows, where a file's or a list's name would be anything else.
            let u = url::Url::parse(&format!("https://{s}")).ok()?;
            let host = host_of(u.as_str());
            (super::known(&host).is_some() || PASTES.contains(&host.as_str())).then_some(u)
        })
        .filter(|u| matches!(u.scheme(), "http" | "https"))
        .filter(|u| host_of(u.as_str()) != site && (u.path().len() > 1 || u.query().is_some()))
        .map(|u| u.to_string())
        .filter(|u| seen.insert(u.clone()))
        .collect()
}

/// Whether link `u` names a file (in its path or after `#`) by an archive's,
/// a part's or a media extension ([`FILE_EXTENSIONS`]).
pub(super) fn names_file(u: &str) -> bool {
    guess_name(u).is_some_and(|name| FILE_EXTENSION.is_match(&name))
}

static FILE_EXTENSION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!(r"(?i)\.(?:{FILE_EXTENSIONS})$")).unwrap());

/// The links in `page` to known hosts, wrappers and pastes, and to files
/// on other sites ([`names_file`]), absolute, each once. Also those written escaped (`https:\/\/…`, `https&#58;//…`), and
/// those a link carries to a redirector or dereferer (`/?https://…`,
/// `?url=https%3A…`, or in base64). What the page embeds ([`EMBEDDED`]) is
/// left out.
pub(super) fn links_in(page: &str, base: &url::Url, own: &str) -> Vec<String> {
    let page = &EMBEDDED.replace_all(page, " ");
    let plain = super::decode_entities(
        &page
            .replace("\\/", "/")
            .replace("\\u002F", "/")
            .replace("\\u002f", "/"),
    );
    let in_page = |page: &str| -> Vec<String> {
        HREF.captures_iter(page)
            .map(|c| c[1].to_string())
            .chain(BARE.find_iter(page).map(|m| m.as_str().to_string()))
            .collect()
    };
    // The plain copy first, for the page's order; then whatever decoding
    // may have cut short in the page as written.
    let found = in_page(&plain).into_iter().chain(in_page(page));
    let own_site = host_of(own);
    let own_site_of = site_of(own);
    let mut seen = std::collections::HashSet::new();
    found
        .filter_map(|raw| base.join(&raw.replace("&amp;", "&")).ok())
        .flat_map(|u| {
            let carried = carried(&u);
            std::iter::once(u).chain(carried)
        })
        .filter(|u| matches!(u.scheme(), "http" | "https"))
        // A host's front page is not a file.
        .filter(|u| u.path().len() > 1 || u.query().is_some())
        .filter_map(|mut u| {
            let host = host_of(u.as_str());
            if PASTES.contains(&host.as_str()) {
                // A paste site's own menu (its other pastes, its terms) is
                // not what a page on it lists; a paste is the same paste
                // whatever part of it a link points at.
                if host == own_site {
                    return None;
                }
                u.set_fragment(None);
                return Some(u.to_string());
            }
            let u = u.to_string();
            (wraps(&u)
                || known(&host).is_some_and(|k| !k.wraps())
                || (site_of(&u) != own_site_of && names_file(&u)))
            .then_some(u)
        })
        .filter(|u| u != own && seen.insert(u.clone()))
        .collect()
}

/// The addresses link `u` carries to a redirector or dereferer: its whole
/// query (`/?https://…`), a query value (percent-decoded), or a query value
/// or the last part of its path in base64 (standard or URL-safe, padded or
/// not), when it is an `http(s)://` address.
fn carried(u: &url::Url) -> Vec<url::Url> {
    use base64::{
        Engine as _,
        engine::general_purpose::{STANDARD_NO_PAD, URL_SAFE_NO_PAD},
    };
    let address = |s: &str| {
        let s = s.trim();
        (s.starts_with("http://") || s.starts_with("https://"))
            .then(|| url::Url::parse(s).ok())
            .flatten()
    };
    let unbase64 = |s: &str| {
        // Padded or not: the padding is dropped first.
        [STANDARD_NO_PAD, URL_SAFE_NO_PAD]
            .iter()
            .find_map(|engine| engine.decode(s.trim_end_matches('=')).ok())
            .and_then(|bytes| String::from_utf8(bytes).ok())
            .and_then(|s| address(&s))
    };
    let mut out = Vec::new();
    if let Some(query) = u.query() {
        out.extend(address(
            &percent_encoding::percent_decode_str(query).decode_utf8_lossy(),
        ));
    }
    for (_, value) in u.query_pairs() {
        out.extend(address(&value).or_else(|| unbase64(&value)));
    }
    if let Some(last) = u.path_segments().and_then(|mut s| s.next_back()) {
        out.extend(unbase64(last));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_title_names_a_file_only_by_its_whole_extension() {
        let named = |title: &str| named_in_title(&format!("<title>{title}</title>"));
        assert_eq!(
            named("Download Archive.part1.rar - Host").as_deref(),
            Some("Archive.part1.rar")
        );
        assert_eq!(named("Example.Notes.2026.Final | Blog"), None);
    }

    #[test]
    fn a_page_title_names_the_release_alone() {
        assert_eq!(
            short_title("Download Example Files [v2] | Example Site").as_deref(),
            Some("Example Files [v2]")
        );
        assert_eq!(short_title("Example R").as_deref(), Some("Example R"));
        assert_eq!(short_title(" | Site"), None);
        // The separator written as a character reference, as blogs do.
        assert_eq!(
            page_title(
                r#"<meta property="og:title" content="Example Set-GRP &#187; SITE.example" />"#
            )
            .as_deref(),
            Some("Example Set-GRP")
        );
    }

    #[test]
    fn a_paste_lists_what_its_text_shows_on_any_host() {
        let shown = [
            "https://fuckingfast.net/abc123def456".to_string(),
            "https://zpaste.net/p/other".to_string(),
            "https://fuckingfast.net/".to_string(),
        ];
        assert_eq!(
            pasted_links(&shown, "https://zpaste.net/p/a1b2c"),
            ["https://fuckingfast.net/abc123def456"]
        );
        assert!(
            pasted_links(&shown, "https://blog.example/post").is_empty(),
            "only a paste's text is a list"
        );
    }

    /// Addresses written without `https://` count on hosts, wrappers and
    /// pastes Haul knows, and nowhere else.
    #[test]
    fn a_paste_lists_known_addresses_written_without_a_scheme() {
        let page = "<pre>Part 1: pixeldrain.com/u/abc123\nPart 2: filecrypt.cc/Container/AAAAAAAA.html\nsee readme.example/notes and example.com/x</pre>";
        assert_eq!(
            paste_text_links(page, "https://paste2.org/paste/example-set"),
            [
                "https://pixeldrain.com/u/abc123",
                "https://filecrypt.cc/Container/AAAAAAAA.html"
            ]
        );
    }

    #[test]
    fn a_paste_read_by_haul_gives_its_whole_list() {
        let page = r#"<html><head><script>var cdn = "https://cdn.example/lib/app.js";</script>
            <style>body { background: url(https://img.example/bg.png) }</style></head><body>
            <a href="/p/other">Recent paste</a>
            <a href="https://zpaste.net/pages/terms">Terms</a>
            <a href="https://casino.example/win"><img src="https://casino.example/ad.png"></a>
            <pre>https://fuckingfast.net/abc123def456#Archive.part01.rar
https://megaup.net/abc/Archive.part02.rar
https://unknown.example/f/x?y=1&amp;z=2</pre></body></html>"#;
        assert_eq!(
            paste_text_links(page, "https://zpaste.net/p/x9y8z"),
            [
                "https://fuckingfast.net/abc123def456#Archive.part01.rar",
                "https://megaup.net/abc/Archive.part02.rar",
                "https://unknown.example/f/x?y=1&z=2",
            ]
        );
    }

    #[test]
    fn a_paste_gives_its_links_not_its_sites_menu() {
        let page = r#"<a href="/p/other#tm-mobile">Recent paste</a>
            <a href="/pages/terms">Terms</a>
            <pre>https://mega.nz/file/AbCdEfGh#AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8</pre>"#;
        let base = url::Url::parse("https://zpaste.net/p/x9y8z").unwrap();
        assert_eq!(
            links_in(page, &base, "https://zpaste.net/p/x9y8z"),
            ["https://mega.nz/file/AbCdEfGh#AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8"]
        );
    }

    #[test]
    fn an_anotepad_note_lists_the_links_its_text_shows() {
        // anotepad sends each link through its own checker, whose address
        // says nothing of where it goes; the note's text still names it.
        let url = "https://anotepad.com/notes/abc123";
        let page = r#"<html><head>
            <meta name="description" content="Download https://filecrypt.cc/Container/AAAAAAAA.html" />
            </head><body><a href="/notes/other">Other note</a>
            <div class="plaintext"><a href="/link-check?u=1.AAAAAAAA&amp;n=1" title="https://filecrypt.cc/Container/AAAAAAAA.html">https://filecrypt.cc/Container/AAAAAAAA.html</a>
            https://unknown.example/f/abc123</div>
            <a href="https://www.ads.example">Ad</a></body></html>"#;
        assert!(is_paste(url));
        let base = url::Url::parse(url).unwrap();
        assert_eq!(
            links_in(page, &base, url),
            ["https://filecrypt.cc/Container/AAAAAAAA.html"]
        );
        assert_eq!(
            paste_text_links(page, url),
            [
                "https://filecrypt.cc/Container/AAAAAAAA.html",
                "https://unknown.example/f/abc123",
            ]
        );
    }

    #[test]
    fn a_link_behind_a_redirector_or_written_escaped_is_found() {
        let base = url::Url::parse("https://blog.example/post/1").unwrap();
        let find = |page: &str| links_in(page, &base, "https://blog.example/post/1");
        for page in [
            r#"<a href="https://anon.example/?https://pixeldrain.com/u/abc">Part 1</a>"#,
            r#"<a href="/out?url=https%3A%2F%2Fpixeldrain.com%2Fu%2Fabc">Part 1</a>"#,
            r#"<a href="https://go.example/go?u=aHR0cHM6Ly9waXhlbGRyYWluLmNvbS91L2FiYw==">Part 1</a>"#,
            r#"<a href="https://go.example/to/aHR0cHM6Ly9waXhlbGRyYWluLmNvbS91L2FiYw">Part 1</a>"#,
            r#"<script>var links = {"one":"https:\/\/pixeldrain.com\/u\/abc"};</script>"#,
            r#"<script>var one = "https:\u002F\u002Fpixeldrain.com\u002Fu\u002Fabc";</script>"#,
            r#"<p>https&#58;//pixeldrain.com/u/abc</p>"#,
        ] {
            assert_eq!(find(page), ["https://pixeldrain.com/u/abc"], "{page}");
        }
        // An ad stays out however it is carried.
        let ads = r#"<a href="https://anon.example/?https://casino.example/win">Win</a>
            <a href="https://go.example/go?u=aHR0cHM6Ly9jYXNpbm8uZXhhbXBsZS93aW4=">Win</a>"#;
        assert!(find(ads).is_empty(), "{:?}", find(ads));
    }

    #[test]
    fn a_page_gives_its_links_to_files_on_other_sites() {
        let page = r#"<video poster="https://cdn.example/trailer.jpg"><source src="https://cdn.example/trailer.webm" type="video/webm">
              <source src='https://cdn.example/trailer.mp4'>
              <a href="https://cdn.example/files/video.mkv">Download the video</a></video>
            <img src="https://img.example/cover.png"><audio src="https://cdn.example/theme.mp3"></audio>
            <VIDEO><SOURCE SRC=https://cdn.example/clip.mp4></VIDEO>
            <a href="https://unknown.example/x/Archive.part01.rar">Part 1</a>
            <a href="https://ff.example/abc#Archive.part02.rar">Part 2</a>
            <a href="/files/site.rar">This site's</a>
            <a href="https://cdn.blog.example/files/also-site.rar">This site's, too</a>
            <a href="https://casino.example/win">Ad</a>"#;
        let base = url::Url::parse("https://blog.example/post/1").unwrap();
        assert_eq!(
            links_in(page, &base, "https://blog.example/post/1"),
            [
                "https://cdn.example/files/video.mkv",
                "https://unknown.example/x/Archive.part01.rar",
                "https://ff.example/abc#Archive.part02.rar",
            ],
            "an address in a media tag's attributes is not a link the page lists"
        );
    }

    #[test]
    fn a_page_gives_its_links_to_known_hosts() {
        let page = r#"<a href="/about">About</a>
            <a href="https://casino.example/win">Ad</a>
            <a href="https://filecrypt.cc/Container/ABCDEF0123.html">Mirror 1</a>
            <a href='https://rapidgator.net/file/abc/R.part1.rar.html'>Part 1</a>
            <a href="https://rapidgator.net/">Rapidgator</a>
            <a href="https://zpaste.net/p/x9y8z">MEGA</a>
            <a href="https://zpaste.net/p/x9y8z#tm-mobile">MEGA, on a phone</a>
            <a href="https://zpaste.net/">zpaste</a>
            <p>Also: https://pixeldrain.com/u/xyz and https://filecrypt.cc/Container/ABCDEF0123.html again</p>"#;
        let base = url::Url::parse("https://blog.example/post/1").unwrap();
        assert_eq!(
            links_in(page, &base, "https://blog.example/post/1"),
            [
                "https://filecrypt.cc/Container/ABCDEF0123.html",
                "https://rapidgator.net/file/abc/R.part1.rar.html",
                "https://zpaste.net/p/x9y8z",
                "https://pixeldrain.com/u/xyz",
            ]
        );
    }
}

/// "Password:" (or its like in a few languages) and what follows, in a page's
/// text, as a post or a paste gives an archive's password beside its links.
static PASSWORD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"(?i)\b(?:archive\s+|rar\s+)?(?:password|passwort|passwd|pass|pwd|pw|mot de passe|contraseña|senha)",
        r#"\s*[:=]\s*(?:"([^"<>]{1,80})"|“([^”<>]{1,80})”|([^\s<>"']{1,80}))"#,
    ))
    .unwrap()
});

/// Words that say there is no password rather than give one.
const NO_PASSWORD: &[&str] = &[
    "none", "no", "nothing", "n/a", "na", "-", "--", "?", "required", "*", "empty", "free",
];

/// The archive password a page gives in its text, when it gives one: what
/// follows the first "Password:" outside its scripts and tags. A guess, so it
/// is only tried first; a wrong one costs one try before the others.
pub(super) fn password_in(page: &str) -> Option<String> {
    let text = SCRIPT.replace_all(page, " ");
    let text = super::decode_entities(&TAG.replace_all(&text, " "));
    PASSWORD.captures_iter(&text).find_map(|c| {
        // Quoted, it may hold spaces; bare, it ends at the first one.
        let quoted = c
            .get(1)
            .or_else(|| c.get(2))
            .map(|m| m.as_str().to_string());
        let value = quoted.unwrap_or_else(|| {
            c.get(3)
                .map_or("", |m| m.as_str())
                .trim_end_matches(['.', ',', ';', ')'])
                .to_string()
        });
        (!value.is_empty() && !NO_PASSWORD.contains(&value.to_ascii_lowercase().as_str()))
            .then_some(value)
    })
}

#[cfg(test)]
mod password_tests {
    use super::password_in;

    #[test]
    fn a_page_gives_its_archives_password() {
        let cases = [
            ("<p>Password: open-sesame</p>", Some("open-sesame")),
            (
                "<b>Archive password</b>: <code>www.site.example</code>",
                Some("www.site.example"),
            ),
            ("Passwort = Beispiel123.", Some("Beispiel123")),
            ("pw: abc123", Some("abc123")),
            (
                r#"Password: "open sesame" (case sensitive)"#,
                Some("open sesame"),
            ),
            ("Password: none", None),
            ("No password needed", None),
            ("<input type=\"password\" name=\"password\">", None),
            ("<script>var password = 'x'</script><p>links</p>", None),
        ];
        for (page, want) in cases {
            assert_eq!(password_in(page).as_deref(), want, "{page}");
        }
    }
}

#[cfg(test)]
mod challenge_tests {
    use super::challenge_kind;

    #[test]
    fn a_check_page_names_its_kind() {
        let cases = [
            (
                "window._cf_chl_opt = {cvId: '3', cType: 'managed', cRay: 'abc123'};",
                Some("managed"),
            ),
            ("cType: 'non-interactive',", Some("non interactive")),
            ("cType:'interactive'", Some("interactive")),
            (
                "cType: 'precursor_interstitial',",
                Some("precursor interstitial"),
            ),
            ("<title>Just a moment...</title>", None),
        ];
        for (page, want) in cases {
            assert_eq!(challenge_kind(page).as_deref(), want, "{page}");
        }
    }
}
