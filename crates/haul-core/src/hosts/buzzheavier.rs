//! buzzheavier: the download endpoint answers an htmx request with the file's
//! address in `hx-redirect` (or `Location`).

use std::collections::BTreeMap;

use reqwest::header::{LOCATION, REFERER};

use super::{Cx, Entry, ResolveError, retry_after};

pub(super) async fn resolve(cx: &Cx<'_>, url: &str) -> Result<Vec<Entry>, ResolveError> {
    let id = url
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or_default();
    let base = &cx.cfg.endpoints.buzzheavier;
    let r = cx
        .http
        .no_redirect
        .get(format!("{base}/{id}/download"))
        .header("HX-Request", "true")
        .header(REFERER, url)
        .send()
        .await?;
    if r.status() == 429 {
        let seconds = retry_after(r.headers(), 120);
        return Err(ResolveError::Wait {
            seconds,
            message: format!("buzzheavier rate limit, wait {seconds}s"),
        });
    }
    let location = [r.headers().get("hx-redirect"), r.headers().get(LOCATION)]
        .into_iter()
        .flatten()
        .find_map(|v| {
            v.to_str()
                .ok()
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        });
    let Some(location) = location else {
        return Err(ResolveError::NeedsUser(
            "buzzheavier: no download link (file removed, or it needs a browser)".into(),
        ));
    };
    let location = if location.starts_with('/') {
        format!("{base}{location}")
    } else {
        location
    };
    Ok(vec![Entry {
        url: location,
        name: None,
        size: None,
        headers: BTreeMap::from([("Referer".into(), url.to_string())]),
        exec: None,
    }])
}
