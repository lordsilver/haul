//! mirrored.to: one file uploaded to many hosts at once, behind a page per
//! file. Nothing asks for a person: the page links a second one, whose
//! script fetches the hosts' table (`mirstats.php`), and each host's row
//! links a page that shows the host's own link as text. The file becomes
//! one row with every host that took it as a mirror.

use std::sync::LazyLock;

use regex::Regex;
use reqwest::header::REFERER;

use super::{Cx, Entry, ResolveError, Unwrapped, page_text, retry_after};

/// The file's id in `/files/<id>/`.
static ID: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"/files/([A-Za-z0-9]+)").unwrap());
/// The page's link to its hosts ("Click to view the download links").
static LINKS_PAGE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"href="(https?://[^"]*/files/[A-Za-z0-9]+/\?hash=[0-9a-f]+&(?:amp;)?dl=0)""#)
        .unwrap()
});
/// The request the links page's script makes for the hosts' table.
static TABLE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#""(/mirstats\.php\?[^"]+)""#).unwrap());
/// A host's row in that table, when the upload to it succeeded.
static GETLINK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"href="(/getlink/[^"]+)""#).unwrap());
/// The host's link, as a host's page shows it to copy.
static SHOWN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"data-clipboard-text="(https?://[^"]+)""#).unwrap());
static NAME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?s)You have requested the file\s*(?:<[^>]+>\s*)*([^<]+)").unwrap()
});

/// One link per file, whichever address of it.
pub(super) fn key(url: &str) -> Option<String> {
    Some(format!("mirrored.to/{}", &ID.captures(url)?[1]))
}

pub(super) async fn unwrap(cx: &Cx<'_>, url: &str) -> Result<Unwrapped, ResolveError> {
    let id = ID
        .captures(url)
        .map(|c| c[1].to_string())
        .ok_or_else(|| ResolveError::failed("not a mirrored.to file link"))?;
    let base = &cx.cfg.endpoints.mirrored;
    let page_url = format!("{base}/files/{id}/");
    let page = fetch(cx, &page_url, base).await?;
    let name = NAME.captures(&page).and_then(|c| page_text(&c[1]));
    let links_url = LINKS_PAGE
        .captures(&page)
        .map(|c| c[1].replace("&amp;", "&"))
        .ok_or_else(|| ResolveError::failed("mirrored.to: no link to the file's hosts (gone?)"))?;
    // The address on the page names the public site; ask the same place.
    let links_url = match links_url.find("/files/") {
        Some(at) => format!("{base}{}", &links_url[at..]),
        None => links_url,
    };
    let links = fetch(cx, &links_url, &page_url).await?;
    let table = TABLE
        .captures(&links)
        .map(|c| c[1].replace("&amp;", "&"))
        .ok_or_else(|| ResolveError::failed("mirrored.to: the hosts' table moved"))?;
    let table = fetch(cx, &format!("{base}{table}"), &links_url).await?;
    let mut urls = Vec::new();
    for getlink in GETLINK.captures_iter(&table) {
        let getlink = format!("{base}{}", getlink[1].replace("&amp;", "&"));
        // A host's page that does not answer leaves that host out.
        if let Ok(shown) = fetch(cx, &getlink, &links_url).await
            && let Some(link) = SHOWN.captures(&shown)
            && !urls.contains(&link[1].to_string())
        {
            urls.push(link[1].to_string());
        }
    }
    if urls.is_empty() {
        return Err(ResolveError::failed(
            "mirrored.to: no host has the file any more",
        ));
    }
    let first = urls.remove(0);
    Ok(Unwrapped {
        title: name.clone(),
        files: vec![Entry {
            url: first,
            mirrors: urls,
            name,
            deferred: true,
            ..Entry::default()
        }],
    })
}

async fn fetch(cx: &Cx<'_>, url: &str, referer: &str) -> Result<String, ResolveError> {
    let r = cx
        .http
        .client
        .get(url)
        .header(REFERER, referer)
        .send()
        .await?;
    let status = r.status().as_u16();
    if matches!(status, 429 | 503) {
        let seconds = retry_after(r.headers(), 60);
        return Err(ResolveError::Wait {
            seconds,
            message: format!("mirrored.to: {status} rate limited"),
        });
    }
    if status >= 400 {
        return Err(ResolveError::Failed(format!("mirrored.to: HTTP {status}")));
    }
    Ok(r.text().await?)
}
