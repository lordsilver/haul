//! gofile: the content API, with the user's account token. gofile serves
//! that API only to premium accounts and to its own website, which signs
//! each request with a token derived from a secret it rotates; a request
//! signed with a retired secret gets the address banned. Haul does not
//! sign: without an account token it leaves the link to a person, without
//! asking gofile anything.

use std::collections::BTreeMap;

use serde_json::Value;

use super::{Algo, Checksum, Cx, Entry, ResolveError};
use crate::config::secret;

pub(super) async fn resolve(cx: &Cx<'_>, url: &str) -> Result<Vec<Entry>, ResolveError> {
    let Some(token) = secret(&cx.cfg.gofile_token).map(str::to_string) else {
        return Err(ResolveError::NeedsUser(
            "gofile lets only premium accounts and its own website fetch links: open the page, or set a premium gofile_token".into(),
        ));
    };
    let id = url
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or_default();
    let base = &cx.cfg.endpoints.gofile;
    let j: Value = cx
        .http
        .client
        .get(format!("{base}/contents/{id}"))
        .bearer_auth(&token)
        .send()
        .await?
        .json()
        .await?;
    match j["status"].as_str().unwrap_or_default() {
        "ok" => {}
        // A free account is turned away as a guest is, and passwords are
        // typed in the browser; either way a person has to open the page.
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
                package: None,
                size: child["size"].as_u64(),
                headers: BTreeMap::from([("Cookie".into(), format!("accountToken={token}"))]),
                exec: None,
                checksum: child["md5"]
                    .as_str()
                    .and_then(|h| Checksum::from_hex(Algo::Md5, h)),
                deferred: false,
                mirrors: Vec::new(),
                decrypt: None,
            });
        }
    }
}
