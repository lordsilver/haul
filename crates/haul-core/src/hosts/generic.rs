//! Any host without its own resolver: a link that answers with a file is used
//! as is; one that answers with a web page needs a person.

use std::sync::LazyLock;

use regex::Regex;
use reqwest::header::RANGE;

use super::{
    Cx, Entry, Probed, ResolveError, content_length, content_range, content_type, disposition_name,
    guess_name, header_checksum, host_of, retry_after,
};

pub(super) async fn resolve(cx: &Cx<'_>, url: &str) -> Result<Vec<Entry>, ResolveError> {
    // HEAD first, for the headers it is cheaper for. A 405/501, or a host
    // that never answers it (transfer.it stalls HEAD), gets one byte through
    // a ranged GET instead.
    let head = cx.http.client.head(url).send().await;
    let r = match head {
        Ok(r) if !matches!(r.status().as_u16(), 405 | 501) => r,
        _ => {
            cx.http
                .client
                .get(url)
                .header(RANGE, "bytes=0-0")
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
