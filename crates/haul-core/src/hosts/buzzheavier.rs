//! buzzheavier: the download endpoint answers an htmx request with the file's
//! address in `hx-redirect` (or `Location`).

use std::collections::BTreeMap;

use reqwest::header::{LOCATION, REFERER};

use super::{Cx, Entry, ResolveError};

pub(super) async fn resolve(cx: &Cx<'_>, url: &str) -> Result<Vec<Entry>, ResolveError> {
    let id = super::last_path_part(url);
    let base = &cx.cfg.endpoints.buzzheavier;
    let r = cx
        .http
        .no_redirect
        .get(format!("{base}/{id}/download"))
        .header("HX-Request", "true")
        .header(REFERER, url)
        .send()
        .await?;
    super::wait_from(&r, "buzzheavier", 120)?;
    // Cloudflare scores each request to the site: Haul's passes this one
    // where it is turned away from the file's page (2026-11-07), but one
    // turned away is a check a browser passes, whose clearance Haul then
    // carries, not a file that is gone. A 429 above may carry a check too,
    // and is waited out first.
    if super::generic::challenged(r.headers()) {
        return Err(super::generic::guarded_by(url, r).await);
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
        return Err(ResolveError::person(
            "buzzheavier: no download link (file removed, or it needs a browser)",
        ));
    };
    let location = if location.starts_with('/') {
        format!("{base}{location}")
    } else {
        location
    };
    Ok(vec![Entry {
        url: location,
        headers: BTreeMap::from([("Referer".into(), url.to_string())]),
        ..Entry::default()
    }])
}
