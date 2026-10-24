//! ddownload.com: a free download is a 60-second wait, then Cloudflare's
//! Turnstile in the download form, then the file's link. A browser passes
//! that: the desktop app opens the page out of sight and its recipe
//! (`handoff.js`) presses the button once both are done, then hands the link
//! over. Here, only what the page says without them: the file's name, or
//! that it was deleted.

use std::sync::LazyLock;

use regex::Regex;

use super::{Cx, Entry, Look, Probed, ResolveError};

static NAME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"class="dk-dl-name"\s+title="([^"]+)""#).unwrap());

const DELETED: &str = "ddownload: the file was deleted";

/// What the page says: the file's name (when it gives it), or that it was
/// deleted (`None`).
async fn page(cx: &Cx<'_>, url: &str) -> Result<Option<Option<String>>, ResolveError> {
    let page = cx.http.client.get(url).send().await?.text().await?;
    if page.contains("File Not Found") {
        return Ok(None);
    }
    Ok(Some(
        NAME.captures(&page).map(|c| c[1].replace("&amp;", "&")),
    ))
}

pub(super) async fn resolve(cx: &Cx<'_>, url: &str) -> Result<Vec<Entry>, ResolveError> {
    if page(cx, url).await?.is_none() {
        return Err(ResolveError::failed(DELETED));
    }
    Err(ResolveError::NeedsUser(
        "ddownload wants a 60-second wait and Cloudflare's check passed in a browser: the desktop app does both on its own".into(),
    ))
}

pub(super) async fn look(cx: &Cx<'_>, url: &str) -> Look {
    match page(cx, url).await {
        Ok(Some(Some(name))) => Look::Found(Probed {
            name,
            size: None,
            checksum: None,
        }),
        Ok(None) => Look::Gone(DELETED.into()),
        _ => Look::Unknown,
    }
}

#[cfg(test)]
pub(super) async fn probe(cx: &Cx<'_>, url: &str) -> Option<Probed> {
    look(cx, url).await.found()
}
