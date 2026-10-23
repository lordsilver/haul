//! 1fichier: the API with the user's key, or one free attempt that honours
//! the host's own cooldown and stops at its captcha.
//!
//! Its link checker (`/check_links.pl`, no key) says whether a file is there,
//! and its name and size, so a deleted file fails before anyone is sent to
//! its page and mirrors merge before anything downloads.

use std::sync::LazyLock;

use regex::Regex;
use serde_json::{Value, json};

use super::{Cx, Entry, Probed, ResolveError};
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

/// The file's id: the query's first parameter, or the old `ID.1fichier.com`
/// form's subdomain.
fn file_id(url: &str) -> Option<String> {
    let u = url::Url::parse(url).ok()?;
    let from_query = u.query().and_then(|q| {
        q.split('&')
            .find(|p| !p.is_empty() && *p != "af" && !p.starts_with("af="))
            .and_then(|p| p.split('=').next())
            .map(str::to_string)
    });
    from_query.or_else(|| {
        u.host_str()?
            .strip_suffix(".1fichier.com")
            .filter(|s| !s.is_empty() && *s != "www")
            .map(str::to_string)
    })
}

/// What the link checker says of a file.
#[derive(Debug, PartialEq)]
enum Checked {
    Found {
        name: String,
        size: u64,
    },
    Gone,
    /// Private or behind a password: there, but nothing more is said.
    Private,
}

/// The checker's line for file `id`: `link;name;size`, or `link;;;NOT FOUND`
/// (also `BAD LINK`, `PRIVATE`).
fn parse_check(answer: &str, id: &str) -> Option<Checked> {
    let line = answer.lines().find(|l| l.contains(id))?;
    let fields: Vec<&str> = line.trim().split(';').collect();
    match fields.as_slice() {
        [_, "", "", "NOT FOUND" | "BAD LINK"] => Some(Checked::Gone),
        [_, "", "", "PRIVATE"] => Some(Checked::Private),
        [_, name, size, ..] if !name.is_empty() => Some(Checked::Found {
            name: (*name).to_string(),
            size: size.parse().ok()?,
        }),
        _ => None,
    }
}

/// Asks the link checker about `url`; `None` when it does not answer.
async fn check(cx: &Cx<'_>, url: &str) -> Option<Checked> {
    let id = file_id(url)?;
    let site = &cx.cfg.endpoints.onefichier_site;
    let r = cx
        .http
        .client
        .post(format!("{site}/check_links.pl"))
        .form(&[("links[]", clean(url))])
        .send()
        .await
        .ok()?;
    if !r.status().is_success() {
        return None;
    }
    parse_check(&r.text().await.ok()?, &id)
}

pub(super) async fn probe(cx: &Cx<'_>, url: &str) -> Option<Probed> {
    match check(cx, url).await? {
        Checked::Found { name, size } => Some(Probed {
            name,
            size: Some(size),
            checksum: None,
        }),
        Checked::Gone | Checked::Private => None,
    }
}

const GONE: &str = "1fichier: the file was deleted";

pub(super) async fn resolve(cx: &Cx<'_>, url: &str) -> Result<Vec<Entry>, ResolveError> {
    let clean = clean(url);
    if check(cx, url).await == Some(Checked::Gone) {
        return Err(ResolveError::failed(GONE));
    }
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
    // The checker did not answer, and the page says the file is not there.
    if html.contains("File not found") || html.contains("Fichier introuvable") {
        return Err(ResolveError::failed(GONE));
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
    use super::{Checked, clean, file_id, parse_check};

    #[test]
    fn reads_the_link_checker() {
        let answer = "https://1fichier.com/?gyyg43jyog3r82pn2bhw;;;NOT FOUND\n\
                      https://1fichier.com/?abc123;Release.part1.rar;4294967296\n\
                      https://1fichier.com/?priv9;;;PRIVATE\n";
        assert_eq!(
            parse_check(answer, "gyyg43jyog3r82pn2bhw"),
            Some(Checked::Gone)
        );
        assert_eq!(
            parse_check(answer, "abc123"),
            Some(Checked::Found {
                name: "Release.part1.rar".into(),
                size: 4_294_967_296
            })
        );
        assert_eq!(parse_check(answer, "priv9"), Some(Checked::Private));
        assert_eq!(parse_check(answer, "missing"), None);
    }

    #[test]
    fn finds_the_file_id() {
        assert_eq!(
            file_id("https://1fichier.com/?abc123&af=27019").as_deref(),
            Some("abc123")
        );
        assert_eq!(
            file_id("https://1fichier.com/?af=1&abc").as_deref(),
            Some("abc")
        );
        assert_eq!(
            file_id("https://abc123.1fichier.com/").as_deref(),
            Some("abc123")
        );
        assert_eq!(file_id("https://1fichier.com/"), None);
    }

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
