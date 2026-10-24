//! rootz.so: a file's page carries a token (`pageToken`) that its own script
//! sends with the file's lookup (`/api/files/download-by-short`), which gives
//! the name, the exact size, the file's id and whether it was deleted. The
//! file downloads from `/api/files/proxy-download/<id>`, which sends on to
//! its storage. One download per address every two minutes: a 429 with
//! `Retry-After`, which the engine waits out.

use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use super::{Cx, Entry, Probed, ResolveError};

static LINK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"/d/([A-Za-z0-9_-]+)").unwrap());
/// The token in the page's data, escaped as it is there (`\"pageToken\":\"…\"`).
static TOKEN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"pageToken\\?"\s*:\s*\\?"([A-Za-z0-9._-]+)"#).unwrap());

/// A file's name, size and id, from its page's lookup.
struct File {
    name: String,
    size: u64,
    id: String,
}

async fn lookup(cx: &Cx<'_>, url: &str) -> Result<File, ResolveError> {
    let short = LINK
        .captures(url)
        .map(|c| c[1].to_string())
        .ok_or_else(|| ResolveError::failed(format!("rootz: not a file link: {url}")))?;
    let base = &cx.cfg.endpoints.rootz;
    let page = cx
        .http
        .client
        .get(format!("{base}/d/{short}"))
        .send()
        .await?
        .text()
        .await?;
    let token = TOKEN
        .captures(&page)
        .map(|c| c[1].to_string())
        .ok_or_else(|| ResolveError::failed("rootz: the page gave no token"))?;
    let answer: Value = cx
        .http
        .client
        .get(format!("{base}/api/files/download-by-short"))
        .query(&[("shortId", short.as_str())])
        .header("X-Page-Token", token)
        .send()
        .await?
        .json()
        .await
        .map_err(|e| ResolveError::Failed(format!("rootz: {e}")))?;
    if answer["success"] != true {
        let why = answer["error"].as_str().unwrap_or("no answer");
        return Err(ResolveError::Failed(format!("rootz: {why}")));
    }
    let d = &answer["data"];
    if d["status"] == "deleted" {
        return Err(ResolveError::failed("rootz: the file was deleted"));
    }
    if d["passwordProtected"] == true {
        return Err(ResolveError::NeedsUser(
            "rootz: the file wants a password: open its page".into(),
        ));
    }
    match (
        d["fileName"].as_str(),
        d["size"].as_u64(),
        d["fileId"].as_str(),
    ) {
        (Some(name), Some(size), Some(id)) => Ok(File {
            name: name.to_string(),
            size,
            id: id.to_string(),
        }),
        _ => Err(ResolveError::failed("rootz: the lookup named no file")),
    }
}

pub(super) async fn resolve(cx: &Cx<'_>, url: &str) -> Result<Vec<Entry>, ResolveError> {
    let file = lookup(cx, url).await?;
    let base = &cx.cfg.endpoints.rootz;
    Ok(vec![Entry {
        url: format!("{base}/api/files/proxy-download/{}", file.id),
        name: Some(file.name),
        size: Some(file.size),
        ..Entry::default()
    }])
}

pub(super) async fn probe(cx: &Cx<'_>, url: &str) -> Option<Probed> {
    let file = lookup(cx, url).await.ok()?;
    Some(Probed {
        name: file.name,
        size: Some(file.size),
        checksum: None,
    })
}
