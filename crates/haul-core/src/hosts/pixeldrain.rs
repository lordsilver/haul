//! pixeldrain: public API for files (`/u/ID`) and lists (`/l/ID`), with an
//! optional API key.

use std::{collections::BTreeMap, sync::LazyLock};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use regex::Regex;
use serde::Deserialize;

use super::{Algo, Checksum, Cx, Entry, Probed, ResolveError};
use crate::config::secret;

static LINK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"/(u|l)/([\w-]+)").unwrap());

#[derive(Deserialize)]
struct File {
    id: Option<String>,
    name: String,
    size: u64,
    /// Non-empty when the host wants a captcha first, such as
    /// `file_rate_limited_captcha_required`.
    #[serde(default)]
    availability: String,
    /// In file info; list entries may leave it out.
    #[serde(default)]
    hash_sha256: Option<String>,
}

impl File {
    fn checksum(&self) -> Option<Checksum> {
        Checksum::from_hex(Algo::Sha256, self.hash_sha256.as_deref()?)
    }
}

#[derive(Deserialize)]
struct List {
    files: Vec<File>,
}

fn auth(cx: &Cx<'_>) -> BTreeMap<String, String> {
    let mut headers = BTreeMap::new();
    if let Some(key) = secret(&cx.cfg.pixeldrain_key) {
        headers.insert(
            "Authorization".into(),
            format!("Basic {}", STANDARD.encode(format!(":{key}"))),
        );
    }
    headers
}

fn request(
    cx: &Cx<'_>,
    url: String,
    headers: &BTreeMap<String, String>,
) -> reqwest::RequestBuilder {
    headers
        .iter()
        .fold(cx.http.client.get(url), |req, (k, v)| req.header(k, v))
}

pub(super) async fn resolve(cx: &Cx<'_>, url: &str) -> Result<Vec<Entry>, ResolveError> {
    let headers = auth(cx);
    let caps = LINK
        .captures(url)
        .ok_or_else(|| ResolveError::failed("bad pixeldrain url"))?;
    let base = &cx.cfg.endpoints.pixeldrain;
    let id = &caps[2];
    if &caps[1] == "l" {
        let list: List = request(cx, format!("{base}/api/list/{id}"), &headers)
            .send()
            .await?
            .json()
            .await?;
        return Ok(list
            .files
            .into_iter()
            .filter_map(|f| {
                let checksum = f.checksum();
                Some(Entry {
                    url: format!("{base}/api/file/{}?download", f.id?),
                    name: Some(f.name),
                    package: None,
                    size: Some(f.size),
                    headers: headers.clone(),
                    checksum,
                    deferred: false,
                    mirrors: Vec::new(),
                    decrypt: None,
                })
            })
            .collect());
    }
    let r = request(cx, format!("{base}/api/file/{id}/info"), &headers)
        .send()
        .await?;
    if r.status() != 200 {
        return Err(ResolveError::Failed(format!(
            "pixeldrain {}",
            r.status().as_u16()
        )));
    }
    let file: File = r.json().await?;
    if file.availability.contains("captcha") {
        return Err(ResolveError::NeedsUser(format!(
            "pixeldrain: {}",
            file.availability
        )));
    }
    let checksum = file.checksum();
    Ok(vec![Entry {
        url: format!("{base}/api/file/{id}?download"),
        name: Some(file.name),
        package: None,
        size: Some(file.size),
        headers,
        checksum,
        deferred: false,
        mirrors: Vec::new(),
        decrypt: None,
    }])
}

pub(super) async fn probe(cx: &Cx<'_>, url: &str) -> Option<Probed> {
    let caps = LINK.captures(url).filter(|c| &c[1] == "u")?;
    let base = &cx.cfg.endpoints.pixeldrain;
    let r = request(cx, format!("{base}/api/file/{}/info", &caps[2]), &auth(cx))
        .send()
        .await
        .ok()?;
    if r.status() != 200 {
        return None;
    }
    let file: File = r.json().await.ok()?;
    Some(Probed {
        checksum: file.checksum(),
        name: file.name,
        size: Some(file.size),
    })
}

/// The allowance of the address `cx` reaches pixeldrain from: 6 GB a day at
/// full speed, then 1 MiB/s, per its `/api/misc/rate_limits`.
pub(super) async fn allowance(cx: &Cx<'_>) -> Option<super::Allowance> {
    let base = &cx.cfg.endpoints.pixeldrain;
    let r = request(cx, format!("{base}/api/misc/rate_limits"), &auth(cx))
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await
        .ok()?;
    if !r.status().is_success() {
        return None;
    }
    r.json().await.ok()
}
