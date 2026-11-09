//! rapidgator: API v2 with the user's premium account. Without one, a free
//! download is its page's (a wait, then a reCAPTCHA), which a person goes
//! through.

use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use super::{Cx, Entry, ResolveError, guess_name};

static FILE_ID: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"/file/([0-9a-f]{32})").unwrap());

fn details(j: &Value) -> String {
    match &j["details"] {
        Value::String(s) => s.clone(),
        Value::Null => "error".into(),
        other => other.to_string(),
    }
}

/// A session token: the one kept from before, or a new login's.
async fn token(cx: &Cx<'_>, user: &str, pass: &str, fresh: bool) -> Result<String, ResolveError> {
    let slot = || {
        cx.tokens
            .rapidgator
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    };
    if !fresh && let Some(t) = slot().clone() {
        return Ok(t);
    }
    let base = &cx.cfg.endpoints.rapidgator;
    let r = cx
        .http
        .client
        .get(format!("{base}/api/v2/user/login"))
        .query(&[("login", user), ("password", pass)])
        .send()
        .await?;
    let j: Value = super::api_json(r, "rapidgator").await?;
    if j["status"] != 200 {
        return Err(ResolveError::Failed(format!(
            "rapidgator login: {}",
            details(&j)
        )));
    }
    let t = j["response"]["token"]
        .as_str()
        .ok_or_else(|| ResolveError::failed("rapidgator login: no token"))?;
    *slot() = Some(t.to_string());
    Ok(t.to_string())
}

pub(super) async fn resolve(cx: &Cx<'_>, url: &str) -> Result<Vec<Entry>, ResolveError> {
    let (Some(user), Some(pass)) = (
        cx.cfg.account("rapidgator_user"),
        cx.cfg.account("rapidgator_pass"),
    ) else {
        return Err(ResolveError::person(super::FREE_ON_PAGE));
    };
    let id = super::link_id(&FILE_ID, url, "rapidgator")?;
    let base = &cx.cfg.endpoints.rapidgator;
    let had_token = cx
        .tokens
        .rapidgator
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .is_some();
    // A kept token may have run out: told so, Haul logs in once more and
    // asks again before failing.
    let mut fresh = false;
    loop {
        let token = token(cx, user, pass, fresh).await?;
        let r = cx
            .http
            .client
            .get(format!("{base}/api/v2/file/download"))
            .query(&[("file_id", id.as_str()), ("token", &token)])
            .send()
            .await?;
        let j: Value = super::api_json(r, "rapidgator").await?;
        if j["status"] != 200 {
            *cx.tokens
                .rapidgator
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = None;
            // 401: the session is no longer good, not the file.
            if j["status"] == 401 && had_token && !fresh {
                fresh = true;
                continue;
            }
            return Err(ResolveError::Failed(format!("rapidgator: {}", details(&j))));
        }
        let link = j["response"]["download_url"]
            .as_str()
            .ok_or_else(|| ResolveError::failed("rapidgator: no download_url"))?;
        return Ok(vec![Entry {
            url: link.into(),
            name: guess_name(url),
            ..Entry::default()
        }]);
    }
}
