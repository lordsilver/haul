//! filecrypt.cc: the `.dlc` link its solved page hands out is a container,
//! and that link is not gated — the gate needs a browser, the container does
//! not. A link that is not the container says a person is needed.

use std::sync::LazyLock;

use regex::Regex;

use super::{Cx, Entry, ResolveError, Unwrapped, content_type, guess_name, retry_after};
use crate::container;

static DLC: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^/DLC/([\w-]+)\.dlc/?$").unwrap());
static CONTAINER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^/Container/([\w-]+)(?:\.html)?/?$").unwrap());

/// The container id in a `.dlc` link, when the link is one.
fn dlc_id(url: &str) -> Option<String> {
    let path = url::Url::parse(url).ok()?.path().to_string();
    Some(DLC.captures(&path)?[1].to_string())
}

/// Whether the link is the container itself rather than the gate's page.
pub(super) fn is_dlc(url: &str) -> bool {
    dlc_id(url).is_some()
}

/// What a link points at: a container's page, or one `.dlc` it handed out.
pub(super) fn key(url: &str) -> Option<String> {
    if let Some(id) = dlc_id(url) {
        return Some(format!("filecrypt.cc/dlc/{id}"));
    }
    let path = url::Url::parse(url).ok()?.path().to_string();
    let id = &CONTAINER.captures(&path)?[1];
    Some(format!("filecrypt.cc/container/{id}"))
}

pub(super) async fn unwrap(cx: &Cx<'_>, url: &str) -> Result<Unwrapped, ResolveError> {
    let Some(id) = dlc_id(url) else {
        return Err(ResolveError::NeedsUser(
            "filecrypt.cc needs a browser: solve its gate there, then add the .dlc link the page offers"
                .into(),
        ));
    };
    let r = cx
        .http
        .client
        .get(format!("{}/DLC/{id}.dlc", cx.cfg.endpoints.filecrypt))
        .send()
        .await?;
    let status = r.status().as_u16();
    if matches!(status, 429 | 503) {
        let seconds = retry_after(r.headers(), 90);
        return Err(ResolveError::Wait {
            seconds,
            message: format!("{status} rate limited"),
        });
    }
    if matches!(status, 404 | 410) || status >= 500 {
        return Err(ResolveError::Failed(format!("HTTP {status}")));
    }
    if status >= 400 {
        return Err(ResolveError::NeedsUser(format!(
            "filecrypt.cc answered HTTP {status} on its .dlc link; solve its gate in a browser"
        )));
    }
    if content_type(r.headers()).contains("text/html") {
        return Err(ResolveError::NeedsUser(
            "filecrypt.cc answered its .dlc link with a web page; solve its gate in a browser"
                .into(),
        ));
    }
    let bytes = r.bytes().await?;
    if bytes.trim_ascii().is_empty() {
        // What filecrypt.cc sends for an id its page has not handed out.
        return Err(ResolveError::failed(
            "sent an empty .dlc: use the .dlc link its solved page offers",
        ));
    }
    let links = container::parse_dlc(
        &cx.key_service.client,
        &cx.cfg.endpoints.dlc_service,
        &bytes,
    )
    .await
    .map_err(ResolveError::failed)?;
    if links.is_empty() {
        return Err(ResolveError::failed("the .dlc held no links"));
    }
    // The container's package names what it holds.
    let title = links
        .iter()
        .map(|link| link.package.trim())
        .find(|package| !package.is_empty())
        .map(str::to_string);
    let files = links
        .into_iter()
        .map(|link| Entry {
            name: link.name.or_else(|| guess_name(&link.url)),
            package: (!link.package.is_empty()).then_some(link.package),
            url: link.url,
            deferred: true,
            ..Entry::default()
        })
        .collect();
    Ok(Unwrapped { title, files })
}
