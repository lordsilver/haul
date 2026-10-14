//! gofile: content API with the user's account token, or a guest token.

use std::collections::BTreeMap;

use serde_json::Value;

use super::{Algo, Checksum, Cx, Entry, ResolveError};
use crate::config::secret;

/// The `wt` parameter the content API expects.
const WEBSITE_TOKEN: &str = "4fd6sg89d7s6";

async fn token(cx: &Cx<'_>) -> Result<String, ResolveError> {
    if let Some(t) = secret(&cx.cfg.gofile_token) {
        return Ok(t.to_string());
    }
    if let Some(t) = cx
        .tokens
        .gofile
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
    {
        return Ok(t);
    }
    let base = &cx.cfg.endpoints.gofile;
    let j: Value = cx
        .http
        .client
        .post(format!("{base}/accounts"))
        .send()
        .await?
        .json()
        .await?;
    let t = j["data"]["token"]
        .as_str()
        .ok_or_else(|| ResolveError::failed("gofile: no guest token"))?
        .to_string();
    *cx.tokens.gofile.lock().unwrap_or_else(|e| e.into_inner()) = Some(t.clone());
    Ok(t)
}

pub(super) async fn resolve(cx: &Cx<'_>, url: &str) -> Result<Vec<Entry>, ResolveError> {
    let id = url
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or_default();
    let token = token(cx).await?;
    let base = &cx.cfg.endpoints.gofile;
    let j: Value = cx
        .http
        .client
        .get(format!("{base}/contents/{id}?wt={WEBSITE_TOKEN}"))
        .bearer_auth(&token)
        .send()
        .await?
        .json()
        .await?;
    match j["status"].as_str().unwrap_or_default() {
        "ok" => {}
        // Guests cannot use the API for every file, and passwords are typed in
        // the browser; either way a person has to open the page.
        status @ ("error-notPremium" | "error-passwordRequired") => {
            return Err(ResolveError::NeedsUser(format!("gofile: {status}")));
        }
        status => return Err(ResolveError::Failed(format!("gofile: {status}"))),
    }
    let mut out = Vec::new();
    walk(&j["data"], &token, &mut out);
    Ok(out)
}

fn walk(node: &Value, token: &str, out: &mut Vec<Entry>) {
    let Some(children) = node["children"].as_object() else {
        return;
    };
    for child in children.values() {
        if child["type"] == "folder" {
            walk(child, token, out);
        } else if let Some(link) = child["link"].as_str() {
            out.push(Entry {
                url: link.to_string(),
                name: child["name"].as_str().map(str::to_string),
                size: child["size"].as_u64(),
                headers: BTreeMap::from([("Cookie".into(), format!("accountToken={token}"))]),
                exec: None,
                checksum: child["md5"]
                    .as_str()
                    .and_then(|h| Checksum::from_hex(Algo::Md5, h)),
            });
        }
    }
}
