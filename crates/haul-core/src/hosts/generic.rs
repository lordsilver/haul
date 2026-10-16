//! Any host without its own resolver: a link that answers with a file is used
//! as is; one that answers with a web page needs a person, unless the page
//! links files on hosts Haul knows (a release's page on a blog or forum,
//! say): then it is a wrapper of those links ([`page_links`]).

use std::sync::LazyLock;

use regex::Regex;
use reqwest::header::{ACCEPT_ENCODING, RANGE};

use super::{
    Cx, Entry, Probed, ResolveError, Unwrapped, content_length, content_range, content_type,
    disposition_name, group_mirrors, guess_name, header_checksum, host_of, known, page_text,
    retry_after, wraps,
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
/// each once, in the page's order; mirrors of one file grouped as a
/// wrapper's are. Links elsewhere (the site's own pages, ads) are left out,
/// and a page with none is not one: it waits for a person as before.
pub(super) async fn page_links(cx: &Cx<'_>, url: &str) -> Result<Unwrapped, ResolveError> {
    let mut r = cx.http.client.get(url).send().await?;
    let status = r.status().as_u16();
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
    let title = [&OG_TITLE, &TITLE]
        .into_iter()
        .find_map(|pattern| page_text(&pattern.captures(&page)?[1]));
    Ok(Unwrapped {
        title,
        files: group_mirrors(urls),
    })
}

/// The links in `page` to known hosts and wrappers, absolute, each once.
fn links_in(page: &str, base: &url::Url, own: &str) -> Vec<String> {
    let found = HREF
        .captures_iter(page)
        .map(|c| c.get(1).expect("one group").as_str())
        .chain(BARE.find_iter(page).map(|m| m.as_str()));
    let mut seen = std::collections::HashSet::new();
    found
        .filter_map(|raw| base.join(&raw.replace("&amp;", "&")).ok())
        .filter(|u| matches!(u.scheme(), "http" | "https"))
        .map(|u| u.to_string())
        .filter(|u| u != own && (wraps(u) || known(&host_of(u)).is_some_and(|k| !k.wraps())))
        // A host's front page is not a file.
        .filter(|u| url::Url::parse(u).is_ok_and(|p| p.path().len() > 1 || p.query().is_some()))
        .filter(|u| seen.insert(u.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_page_gives_its_links_to_known_hosts() {
        let page = r#"<a href="/about">About</a>
            <a href="https://casino.example/win">Ad</a>
            <a href="https://filecrypt.cc/Container/679EC150BC.html">Mirror 1</a>
            <a href='https://rapidgator.net/file/abc/R.part1.rar.html'>Part 1</a>
            <a href="https://rapidgator.net/">Rapidgator</a>
            <p>Also: https://pixeldrain.com/u/xyz and https://filecrypt.cc/Container/679EC150BC.html again</p>"#;
        let base = url::Url::parse("https://blog.example/post/1").unwrap();
        assert_eq!(
            links_in(page, &base, "https://blog.example/post/1"),
            [
                "https://filecrypt.cc/Container/679EC150BC.html",
                "https://rapidgator.net/file/abc/R.part1.rar.html",
                "https://pixeldrain.com/u/xyz",
            ]
        );
    }
}
