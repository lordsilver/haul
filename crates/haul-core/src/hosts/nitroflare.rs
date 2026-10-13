//! nitroflare: API v2 with the user's premium key.

use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use super::{Cx, Entry, ResolveError, guess_name};
use crate::config::secret;

static FILE_ID: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"/view/(\w+)").unwrap());

pub(super) async fn resolve(cx: &Cx<'_>, url: &str) -> Result<Vec<Entry>, ResolveError> {
    let (Some(user), Some(key)) = (
        secret(&cx.cfg.nitroflare_user),
        secret(&cx.cfg.nitroflare_key),
    ) else {
        return Err(ResolveError::failed(
            "nitroflare needs premium: set nitroflare_user/nitroflare_key",
        ));
    };
    let id = FILE_ID
        .captures(url)
        .ok_or_else(|| ResolveError::failed("bad nitroflare url"))?;
    let base = &cx.cfg.endpoints.nitroflare;
    let j: Value = cx
        .http
        .client
        .get(format!("{base}/api/v2/getDownloadLink"))
        .query(&[("user", user), ("premiumKey", key), ("file", &id[1])])
        .send()
        .await?
        .json()
        .await?;
    if j["type"] != "success" {
        return Err(ResolveError::Failed(format!(
            "nitroflare: {}",
            j["message"].as_str().unwrap_or("error")
        )));
    }
    let result = &j["result"];
    let link = result["url"]
        .as_str()
        .ok_or_else(|| ResolveError::failed("nitroflare: no url"))?;
    let size = match &result["size"] {
        Value::Number(n) => n.as_u64(),
        Value::String(s) => s.parse().ok(),
        _ => None,
    };
    Ok(vec![Entry {
        url: link.into(),
        name: result["name"]
            .as_str()
            .filter(|n| !n.is_empty())
            .map(str::to_string)
            .or_else(|| guess_name(url)),
        size: size.filter(|s| *s > 0),
        ..Entry::default()
    }])
}
