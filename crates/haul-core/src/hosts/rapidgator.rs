//! rapidgator: API v2 with the user's premium account. Without one, a free
//! download is its page's (a wait, then a reCAPTCHA), which a person goes
//! through.

use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use super::{Cx, Entry, ResolveError, guess_name};
use crate::config::secret;

static FILE_ID: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"/file/([0-9a-f]{32})").unwrap());

fn details(j: &Value) -> String {
    match &j["details"] {
        Value::String(s) => s.clone(),
        Value::Null => "error".into(),
        other => other.to_string(),
    }
}

pub(super) async fn resolve(cx: &Cx<'_>, url: &str) -> Result<Vec<Entry>, ResolveError> {
    let (Some(user), Some(pass)) = (
        secret(&cx.cfg.rapidgator_user),
        secret(&cx.cfg.rapidgator_pass),
    ) else {
        return Err(ResolveError::NeedsUser(super::FREE_ON_PAGE.into()));
    };
    let id = FILE_ID
        .captures(url)
        .ok_or_else(|| ResolveError::failed("bad rapidgator url"))?;
    let base = &cx.cfg.endpoints.rapidgator;
    let cached = cx
        .tokens
        .rapidgator
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let token = match cached {
        Some(t) => t,
        None => {
            let j: Value = cx
                .http
                .client
                .get(format!("{base}/api/v2/user/login"))
                .query(&[("login", user), ("password", pass)])
                .send()
                .await?
                .json()
                .await?;
            if j["status"] != 200 {
                return Err(ResolveError::Failed(format!(
                    "rapidgator login: {}",
                    details(&j)
                )));
            }
            let t = j["response"]["token"]
                .as_str()
                .ok_or_else(|| ResolveError::failed("rapidgator login: no token"))?;
            *cx.tokens
                .rapidgator
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = Some(t.to_string());
            t.to_string()
        }
    };
    let j: Value = cx
        .http
        .client
        .get(format!("{base}/api/v2/file/download"))
        .query(&[("file_id", &id[1]), ("token", &token)])
        .send()
        .await?
        .json()
        .await?;
    if j["status"] != 200 {
        *cx.tokens
            .rapidgator
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = None;
        return Err(ResolveError::Failed(format!("rapidgator: {}", details(&j))));
    }
    let link = j["response"]["download_url"]
        .as_str()
        .ok_or_else(|| ResolveError::failed("rapidgator: no download_url"))?;
    Ok(vec![Entry {
        url: link.into(),
        name: guess_name(url),
        ..Entry::default()
    }])
}
