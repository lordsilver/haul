//! ddownload.com: a free download is a 60-second wait, then Cloudflare's
//! Turnstile in the download form, then the file's link. A browser passes
//! that: the desktop app opens the page out of sight and its recipe
//! (`handoff.js`) presses the button once both are done, then hands the link
//! over. Here, only what the page says without them: the file's name, or
//! that it was deleted.

use std::sync::LazyLock;

use regex::Regex;

use super::{Cx, Entry, Probed, ResolveError};

static NAME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"class="dk-dl-name"\s+title="([^"]+)""#).unwrap());

/// The file's name, or `None` when the page says it was deleted.
async fn page(cx: &Cx<'_>, url: &str) -> Result<Option<String>, ResolveError> {
    let page = cx.http.client.get(url).send().await?.text().await?;
    if page.contains("File Not Found") {
        return Ok(None);
    }
    Ok(NAME.captures(&page).map(|c| c[1].replace("&amp;", "&")))
}

pub(super) async fn resolve(cx: &Cx<'_>, url: &str) -> Result<Vec<Entry>, ResolveError> {
    if page(cx, url).await?.is_none() {
        return Err(ResolveError::failed("ddownload: the file was deleted"));
    }
    Err(ResolveError::NeedsUser(
        "ddownload wants a 60-second wait and Cloudflare's check passed in a browser: the desktop app does both on its own".into(),
    ))
}

pub(super) async fn probe(cx: &Cx<'_>, url: &str) -> Option<Probed> {
    Some(Probed {
        name: page(cx, url).await.ok()??,
        size: None,
        checksum: None,
    })
}
