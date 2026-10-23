//! Any host without its own resolver: a link that answers with a file is used
//! as is; one that answers with a web page needs a person, unless the page
//! links files on hosts Haul knows or wrappers (a release's post on a blog
//! or forum, say): then those links are added as if pasted
//! ([`page`]).

use std::sync::LazyLock;

use regex::Regex;
use reqwest::header::{ACCEPT_ENCODING, RANGE};

use super::{
    Cx, Entry, Probed, ResolveError, content_length, content_range, content_type, disposition_name,
    guess_name, header_checksum, host_of, known, retry_after, wraps,
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
fn challenged(headers: &reqwest::header::HeaderMap) -> bool {
    headers
        .get("cf-mitigated")
        .is_some_and(|v| v.as_bytes().eq_ignore_ascii_case(b"challenge"))
}

/// What a page behind a browser check needs: a browser, which the desktop
/// app opens out of sight ([`is_guarded`]).
fn guarded(url: &str) -> ResolveError {
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
    Regex::new(
        r#"(?i)([^\s<>/:*?"|]+\.(?:rar|zip|7z|iso|exe|msi|bin|mkv|mp4|avi|mov|mp3|flac|pdf|epub|cbz|cbr|tar|gz|xz|bz2|apk|dmg|img|part\d+\.rar|r\d\d|z\d\d))"#,
    )
    .unwrap()
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
    let status = r.status().as_u16();
    if status >= 400 || !content_type(r.headers()).contains("html") {
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
        return Some(Probed {
            name,
            size,
            checksum: header_checksum(r.headers(), status == 200),
        });
    }
    let mut page = Vec::new();
    while page.len() < PEEK {
        match r.chunk().await {
            Ok(Some(chunk)) => page.extend_from_slice(&chunk),
            _ => break,
        }
    }
    page.truncate(PEEK);
    let page = String::from_utf8_lossy(&page);
    [&OG_TITLE, &TITLE].into_iter().find_map(|pattern| {
        let title = pattern.captures(&page)?;
        Some(Probed {
            name: FILE_IN_TITLE.captures(&title[1])?[1].to_string(),
            size: None,
            checksum: None,
        })
    })
}

/// The most of a page read for its links.
const PAGE: usize = 4 * 1024 * 1024;

/// Addresses in a page: in `href`s, and written out in its text.
static HREF: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)\bhref\s*=\s*["']([^"'#\s]+)"#).unwrap());
static BARE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"https?://[^\s"'<>()\[\]{}]+"#).unwrap());

/// The links of a web page to files on hosts Haul knows and to wrappers,
/// each once, in the page's order. Links elsewhere (the site's own pages,
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
    let mut page = Vec::new();
    while page.len() < PAGE {
        match r.chunk().await {
            Ok(Some(chunk)) => page.extend_from_slice(&chunk),
            _ => break,
        }
    }
    let page = String::from_utf8_lossy(&page);
    let urls = links_in(&page, &base, url);
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

/// Sites that hold lists of links (pastes): a release's page often links
/// one per host instead of the hosts' links, so they are read in turn, each
/// as a page of links.
const PASTES: &[&str] = &[
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

/// The links in `page` to known hosts, wrappers and pastes, absolute, each
/// once.
pub(super) fn links_in(page: &str, base: &url::Url, own: &str) -> Vec<String> {
    let found = HREF
        .captures_iter(page)
        .map(|c| c.get(1).expect("one group").as_str())
        .chain(BARE.find_iter(page).map(|m| m.as_str()));
    let own_site = host_of(own);
    let mut seen = std::collections::HashSet::new();
    found
        .filter_map(|raw| base.join(&raw.replace("&amp;", "&")).ok())
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
            (wraps(&u) || known(&host).is_some_and(|k| !k.wraps())).then_some(u)
        })
        .filter(|u| u != own && seen.insert(u.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_page_title_names_the_release_alone() {
        assert_eq!(
            short_title("Download Naruto X Boruto [PC] | ElAmigos Games").as_deref(),
            Some("Naruto X Boruto [PC]")
        );
        assert_eq!(short_title("Release R").as_deref(), Some("Release R"));
        assert_eq!(short_title(" | Site"), None);
    }

    #[test]
    fn a_paste_lists_what_its_text_shows_on_any_host() {
        let shown = [
            "https://fuckingfast.net/dt50dvannvhf".to_string(),
            "https://zpaste.net/p/other".to_string(),
            "https://fuckingfast.net/".to_string(),
        ];
        assert_eq!(
            pasted_links(&shown, "https://zpaste.net/p/91f42"),
            ["https://fuckingfast.net/dt50dvannvhf"]
        );
        assert!(
            pasted_links(&shown, "https://blog.example/post").is_empty(),
            "only a paste's text is a list"
        );
    }

    #[test]
    fn a_paste_gives_its_links_not_its_sites_menu() {
        let page = r#"<a href="/p/other#tm-mobile">Recent paste</a>
            <a href="/pages/terms">Terms</a>
            <pre>https://mega.nz/file/LrBUURwJ#yQvKtW0qGYmJGFoW_4g8NSiAS9IJ4ms2ROfv7FRpk54</pre>"#;
        let base = url::Url::parse("https://zpaste.net/p/pae3o").unwrap();
        assert_eq!(
            links_in(page, &base, "https://zpaste.net/p/pae3o"),
            ["https://mega.nz/file/LrBUURwJ#yQvKtW0qGYmJGFoW_4g8NSiAS9IJ4ms2ROfv7FRpk54"]
        );
    }

    #[test]
    fn a_page_gives_its_links_to_known_hosts() {
        let page = r#"<a href="/about">About</a>
            <a href="https://casino.example/win">Ad</a>
            <a href="https://filecrypt.cc/Container/679EC150BC.html">Mirror 1</a>
            <a href='https://rapidgator.net/file/abc/R.part1.rar.html'>Part 1</a>
            <a href="https://rapidgator.net/">Rapidgator</a>
            <a href="https://zpaste.net/p/pae3o">MEGA</a>
            <a href="https://zpaste.net/p/pae3o#tm-mobile">MEGA, on a phone</a>
            <a href="https://zpaste.net/">zpaste</a>
            <p>Also: https://pixeldrain.com/u/xyz and https://filecrypt.cc/Container/679EC150BC.html again</p>"#;
        let base = url::Url::parse("https://blog.example/post/1").unwrap();
        assert_eq!(
            links_in(page, &base, "https://blog.example/post/1"),
            [
                "https://filecrypt.cc/Container/679EC150BC.html",
                "https://rapidgator.net/file/abc/R.part1.rar.html",
                "https://zpaste.net/p/pae3o",
                "https://pixeldrain.com/u/xyz",
            ]
        );
    }
}
