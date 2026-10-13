//! 1fichier: the API with the user's key, or one free attempt that honours
//! the host's own cooldown and stops at its captcha.

use std::sync::LazyLock;

use regex::Regex;
use serde_json::{Value, json};

use super::{Cx, Entry, ResolveError};
use crate::config::secret;

static OK_LINK: LazyLock<[Regex; 2]> = LazyLock::new(|| {
    [
        Regex::new(r#"href="(https?://[^"]+)"[^>]*class="ok btn-general"#).unwrap(),
        Regex::new(r#"class="ok btn-general[^>]*href="(https?://[^"]+)""#).unwrap(),
    ]
});
static WAIT: LazyLock<[Regex; 2]> = LazyLock::new(|| {
    [
        Regex::new(r"wait (\d+) min").unwrap(),
        Regex::new(r"(\d+) minutes?").unwrap(),
    ]
});

/// The link without the affiliate parameter, keeping 1fichier's bare `?ID` form.
fn clean(url: &str) -> String {
    match url.split_once('?') {
        Some((base, query)) => {
            let kept: Vec<&str> = query
                .split('&')
                .filter(|p| *p != "af" && !p.starts_with("af="))
                .collect();
            if kept.is_empty() {
                base.to_string()
            } else {
                format!("{base}?{}", kept.join("&"))
            }
        }
        None => url.to_string(),
    }
}

pub(super) async fn resolve(cx: &Cx<'_>, url: &str) -> Result<Vec<Entry>, ResolveError> {
    let clean = clean(url);
    if let Some(key) = secret(&cx.cfg.onefichier_key) {
        let base = &cx.cfg.endpoints.onefichier;
        let j: Value = cx
            .http
            .client
            .post(format!("{base}/v1/download/get_token.cgi"))
            .bearer_auth(key)
            .json(&json!({ "url": clean }))
            .send()
            .await?
            .json()
            .await?;
        if j["status"] != "OK" {
            return Err(ResolveError::Failed(format!(
                "1fichier api: {}",
                j["message"].as_str().unwrap_or("error")
            )));
        }
        let link = j["url"]
            .as_str()
            .ok_or_else(|| ResolveError::failed("1fichier api: no url"))?;
        return Ok(vec![Entry {
            url: link.into(),
            ..Entry::default()
        }]);
    }
    let html = cx
        .http
        .client
        .post(&clean)
        .form(&[("dl_no_ssl", "on"), ("dlinline", "on")])
        .send()
        .await?
        .text()
        .await?;
    if let Some(link) = OK_LINK.iter().find_map(|re| re.captures(&html)) {
        return Ok(vec![Entry {
            url: link[1].to_string(),
            ..Entry::default()
        }]);
    }
    let minutes = WAIT
        .iter()
        .find_map(|re| re.captures(&html)?[1].parse::<u64>().ok());
    if html.contains("must wait") || minutes.is_some() {
        let seconds = minutes.map_or(300, |m| m * 60 + 5);
        return Err(ResolveError::Wait {
            seconds,
            message: "1fichier free cooldown".into(),
        });
    }
    Err(ResolveError::NeedsUser(
        "1fichier: captcha (or set 1fichier_key)".into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::clean;

    #[test]
    fn strips_affiliate() {
        assert_eq!(
            clean("https://1fichier.com/?abc123&af=27019"),
            "https://1fichier.com/?abc123"
        );
        assert_eq!(
            clean("https://1fichier.com/?af=1&abc"),
            "https://1fichier.com/?abc"
        );
        assert_eq!(
            clean("https://1fichier.com/?abc"),
            "https://1fichier.com/?abc"
        );
        assert_eq!(clean("https://1fichier.com/?af=1"), "https://1fichier.com/");
    }
}
