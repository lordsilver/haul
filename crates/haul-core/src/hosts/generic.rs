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
    Cx, Entry, FILE_EXTENSIONS, Look, Probed, ResolveError, content_length, content_range,
    content_type, disposition_name, guess_name, header_checksum, host_of, known, retry_after,
    site_of, wraps,
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
    let r = match head {
        Ok(r) if !matches!(r.status().as_u16(), 405 | 501) => r,
        _ => {
            cx.http
                .client
                .get(url)
                .header(RANGE, "bytes=0-0")
                .header(ACCEPT_ENCODING, "identity")
                .send()
                .await?
        }
    };
    let status = r.status().as_u16();
    if challenged(r.headers()) {
        return Err(guarded(url));
    }
    if matches!(status, 429 | 503) {
        let seconds = retry_after(r.headers(), 90);
        return Err(ResolveError::Wait {
            seconds,
            message: format!("{status} rate limited"),
        });
    }
    if status < 400 && !content_type(r.headers()).contains("text/html") {
        let size = match content_range(r.headers()) {
            Some((_, total)) => total,
            None => content_length(r.headers()),
        };
        let final_url = r.url().to_string();
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
    // Gone or broken is not something a person can fix in a browser.
    if matches!(status, 404 | 410) || status >= 500 {
        return Err(ResolveError::Failed(format!("HTTP {status}")));
    }
    Err(ResolveError::NeedsUser(format!(
        "{} answers with a web page: it wants a browser (captcha, timer or a click)",
        host_of(url)
    )))
}

/// Whether a site answered with Cloudflare's browser check instead of the
/// page: a person's browser usually passes it by itself.
pub(super) fn challenged(headers: &reqwest::header::HeaderMap) -> bool {
    headers
        .get("cf-mitigated")
        .is_some_and(|v| v.as_bytes().eq_ignore_ascii_case(b"challenge"))
}

/// What a page behind a browser check needs: a browser, which the desktop
/// app opens out of sight ([`is_guarded`]).
pub(super) fn guarded(url: &str) -> ResolveError {
    ResolveError::NeedsUser(format!("{} {GUARD}", host_of(url)))
}

const GUARD: &str = "is behind a browser check (Cloudflare's)";

/// Whether a resolver's request for a person is [`guarded`]'s: a page whose
/// check a browser usually passes by itself.
pub(super) fn is_guarded(message: &str) -> bool {
    message.ends_with(GUARD)
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
        links: links_in(&page, r.url(), url),
    }
}

/// The most of a page read for its links.
const PAGE: usize = 4 * 1024 * 1024;

/// Addresses in a page: in `href`s, and written out in its text.
static HREF: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)\bhref\s*=\s*["']([^"'#\s]+)"#).unwrap());
static BARE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"https?://[^\s"'<>()\[\]{}]+"#).unwrap());

/// The links of a web page to files on hosts Haul knows, to wrappers and to
/// files on other sites, each once, in the page's order. Links elsewhere (the site's own pages,
/// ads) are left out; a page with none waits for a person as before.
/// With them, the page's own name for what it holds.
pub(super) async fn page(
    cx: &Cx<'_>,
    url: &str,
) -> Result<(Option<String>, Vec<String>), ResolveError> {
    let mut r = cx.http.client.get(url).send().await?;
    let status = r.status().as_u16();
    if challenged(r.headers()) {
        return Err(guarded(url));
    }
    if matches!(status, 429 | 503) {
        let seconds = retry_after(r.headers(), 90);
        return Err(ResolveError::Wait {
            seconds,
            message: format!("{status} rate limited"),
        });
    }
    if status >= 400 {
        return Err(ResolveError::Failed(format!("HTTP {status}")));
    }
    if !content_type(r.headers()).contains("html") {
        return Err(ResolveError::failed("not a web page"));
    }
    let base = r.url().clone();
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
            return Err(ResolveError::failed(format!(
                "the paste on {} lists no links",
                host_of(url)
            )));
        }
    }
    if urls.is_empty() {
        return Err(ResolveError::NeedsUser(format!(
            "{} answers with a web page that links no host Haul knows: it wants a browser",
            host_of(url)
        )));
    }
    Ok((page_title(&page), urls))
}

/// A page's name for itself: its `og:title`, else its `<title>`, the site's
/// name after a separator left off.
pub(super) fn page_title(page: &str) -> Option<String> {
    let raw = [&OG_TITLE, &TITLE]
        .into_iter()
        .find_map(|re| re.captures(page).map(|c| c[1].to_string()))?;
    short_title(
        &raw.replace("&amp;", "&")
            .replace("&#039;", "'")
            .replace("&quot;", "\""),
    )
}

/// A page's title without the site's name after a separator.
pub(super) fn short_title(raw: &str) -> Option<String> {
    let name = [" | ", " – ", " — ", " - "]
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

/// Whether `url` is on a paste site (see [`PASTES`]): a list of links,
/// never a file.
pub(super) fn is_paste(url: &str) -> bool {
    PASTES.contains(&host_of(url).as_str())
}

/// The paste sites Haul recognises today, a fixed list: sites that hold
/// lists of links. A download page often links one per host instead of
/// the hosts' links, so they are read in turn, each as a page of links.
pub(super) const PASTES: &[&str] = &[
    "zpaste.net",
    "paste2.org",
    "pastebin.com",
    "rentry.co",
    "rentry.org",
    "justpaste.it",
    "controlc.com",
    "pastes.io",
    "paste.ee",
    "privatebin.net",
];

/// Scripts and styles, whose addresses are no paste's list.
static SCRIPT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?is)<script\b.*?</script\s*>|<style\b.*?</style\s*>").unwrap());
static TAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)<[^>]*>").unwrap());

/// The addresses the paste at `url` writes out in its text (`page`, its
/// markup), on any host, in order: what its window shows, without the
/// addresses in its scripts, styles and tags (menus, ads).
fn paste_text_links(page: &str, url: &str) -> Vec<String> {
    let text = SCRIPT.replace_all(page, " ");
    let text = super::decode_entities(&TAG.replace_all(&text, " "));
    let shown: Vec<String> = BARE
        .find_iter(&text)
        .map(|m| m.as_str().to_string())
        .collect();
    pasted_links(&shown, url)
}

/// See [`super::pasted_links`].
pub(super) fn pasted_links(shown: &[String], page: &str) -> Vec<String> {
    let site = host_of(page);
    if !PASTES.contains(&site.as_str()) {
        return Vec::new();
    }
    let mut seen = std::collections::HashSet::new();
    shown
        .iter()
        .filter_map(|s| url::Url::parse(s).ok())
        .filter(|u| matches!(u.scheme(), "http" | "https"))
        .filter(|u| host_of(u.as_str()) != site && (u.path().len() > 1 || u.query().is_some()))
        .map(|u| u.to_string())
        .filter(|u| seen.insert(u.clone()))
        .collect()
}

/// Whether link `u` names a file (in its path or after `#`) by an archive's,
/// a part's or a media extension ([`FILE_EXTENSIONS`]).
fn names_file(u: &str) -> bool {
    guess_name(u).is_some_and(|name| FILE_EXTENSION.is_match(&name))
}

static FILE_EXTENSION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!(r"(?i)\.(?:{FILE_EXTENSIONS})$")).unwrap());

/// The links in `page` to known hosts, wrappers and pastes, and to files
/// on other sites ([`names_file`]), absolute, each once. Also those written escaped (`https:\/\/…`, `https&#58;//…`), and
/// those a link carries to a redirector or dereferer (`/?https://…`,
/// `?url=https%3A…`, or in base64).
pub(super) fn links_in(page: &str, base: &url::Url, own: &str) -> Vec<String> {
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
        let page = r#"<a href="https://unknown.example/x/Archive.part01.rar">Part 1</a>
            <a href="https://ff.example/abc#Archive.part02.rar">Part 2</a>
            <a href="/files/site.rar">This site's</a>
            <a href="https://cdn.blog.example/files/also-site.rar">This site's, too</a>
            <a href="https://casino.example/win">Ad</a>"#;
        let base = url::Url::parse("https://blog.example/post/1").unwrap();
        assert_eq!(
            links_in(page, &base, "https://blog.example/post/1"),
            [
                "https://unknown.example/x/Archive.part01.rar",
                "https://ff.example/abc#Archive.part02.rar",
            ]
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
