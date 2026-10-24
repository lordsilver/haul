//! vikingfile: a file's page asks for a Cloudflare Turnstile check and, once
//! it passes, posts its token back to the page for the file's link. The
//! token is only minted on the host's own page, so a person's browser has
//! to show it: from most addresses the check passes by itself, from some
//! (VPNs, some mobile networks) it shows a box to tick. The desktop app
//! opens the page on its own, out of sight, takes the link the page
//! receives, and brings the window forward only when the box shows.
//!
//! `/api/check-file` says without a check whether a file is there, and its
//! name and size, so a gone file is not sent to a person and mirrors merge
//! before anyone opens a page.

use std::{
    sync::LazyLock,
    time::{Duration, Instant},
};

use regex::Regex;
use serde::Deserialize;

use super::{Cx, Entry, Look, Probed, ResolveError, retry_after};

/// The least time between two `check-file` lookups.
const SPACING: Duration = Duration::from_millis(400);

static LINK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"/f/([A-Za-z0-9]+)").unwrap());

#[derive(Deserialize)]
struct File {
    exist: bool,
    #[serde(default)]
    name: String,
    size: Option<u64>,
}

async fn check(cx: &Cx<'_>, url: &str) -> Result<File, ResolveError> {
    let id = LINK
        .captures(url)
        .map(|c| c[1].to_string())
        .ok_or_else(|| ResolveError::failed(format!("vikingfile: not a file link: {url}")))?;
    let base = &cx.cfg.endpoints.vikingfile;
    // It turns away a burst (a wrapper's thirty parts looked up at once)
    // with 429, so lookups go one at a time and spaced out, and one turned
    // away waits as long as it is asked to before the next goes.
    let mut next = cx.tokens.vikingfile_next.lock().await;
    let mut tries = 0;
    let r = loop {
        if let Some(at) = *next {
            tokio::time::sleep_until(at.into()).await;
        }
        let r = cx
            .http
            .client
            .post(format!("{base}/api/check-file"))
            .form(&[("hash", id.as_str())])
            .send()
            .await;
        tries += 1;
        let now = Instant::now();
        *next = Some(now + SPACING);
        let r = r?;
        if r.status() != 429 {
            break r;
        }
        let seconds = retry_after(r.headers(), 2 << tries.min(4)).clamp(1, 30);
        *next = Some(now + Duration::from_secs(seconds));
        if tries == 5 {
            return Err(ResolveError::Wait {
                seconds: seconds.max(30),
                message: "vikingfile: too many requests".into(),
            });
        }
    };
    drop(next);
    if !r.status().is_success() {
        return Err(ResolveError::Network(format!(
            "vikingfile: check-file answered {}",
            r.status()
        )));
    }
    let files: Vec<File> = r
        .json()
        .await
        .map_err(|e| ResolveError::failed(format!("vikingfile: check-file: {e}")))?;
    files
        .into_iter()
        .next()
        .ok_or_else(|| ResolveError::failed("vikingfile: check-file said nothing"))
}

pub(super) async fn resolve(cx: &Cx<'_>, url: &str) -> Result<Vec<Entry>, ResolveError> {
    if !check(cx, url).await?.exist {
        return Err(ResolveError::failed("vikingfile: the file was deleted"));
    }
    Err(ResolveError::NeedsUser(
        "vikingfile wants its Cloudflare check passed in a browser: the desktop app does it on its own and shows the page only if it wants a box ticked".into(),
    ))
}

pub(super) async fn look(cx: &Cx<'_>, url: &str) -> Look {
    match check(cx, url).await {
        Ok(file) if file.exist => Look::Found(Probed {
            name: file.name,
            size: file.size,
            checksum: None,
        }),
        Ok(_) => Look::Gone("vikingfile: the file was deleted".into()),
        Err(_) => Look::Unknown,
    }
}
