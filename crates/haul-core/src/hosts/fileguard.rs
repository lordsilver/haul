//! fileguard.cc: a wrapper page that lists one file's links on many hosts
//! behind an ALTCHA proof of work. That gate asks for no person — the
//! challenge is a SHA-256 hash the client can search — so one solve here
//! yields every mirror link at once.

use std::{collections::HashSet, sync::LazyLock};

use base64::Engine as _;
use regex::Regex;
use reqwest::header::REFERER;
use serde::{Deserialize, Serialize};
use sha2::Digest;

use super::{Cx, Entry, ResolveError, guess_name, host_of, retry_after, to_hex};

static CSRF: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"name="csrf_token"\s+value="([^"]+)""#).unwrap());
static ANCHOR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"(?is)<a\s[^>]*>"#).unwrap());
static HREF: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"\shref="([^"]+)""#).unwrap());

/// The proof-of-work challenge, as the page's `altcha_challenge.php` sends it.
#[derive(Deserialize)]
struct Challenge {
    algorithm: String,
    challenge: String,
    salt: String,
    #[serde(rename = "maxNumber")]
    max_number: u64,
    #[serde(default)]
    signature: String,
}

/// The solved challenge; base64 of this JSON goes to the page as `altcha`.
#[derive(Serialize)]
struct Solution<'a> {
    algorithm: &'a str,
    challenge: &'a str,
    number: u64,
    salt: &'a str,
    signature: &'a str,
}

/// The number whose `sha256(salt + number)` is the challenge.
fn solve(salt: &str, challenge: &str, max_number: u64) -> Option<u64> {
    (0..=max_number).find(|number| {
        let mut hash = sha2::Sha256::new();
        hash.update(salt.as_bytes());
        hash.update(number.to_string().as_bytes());
        to_hex(&hash.finalize()).eq_ignore_ascii_case(challenge)
    })
}

/// The links on the solved page: every anchor marked as a target.
fn target_links(html: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    ANCHOR
        .find_iter(html)
        .filter(|tag| tag.as_str().contains("target-link"))
        .filter_map(|tag| HREF.captures(tag.as_str()))
        .map(|href| href[1].replace("&amp;", "&"))
        .filter(|url| url.starts_with("http") && seen.insert(url.clone()))
        .collect()
}

/// The rows for a solved page. A vault that lists one link per host lists one
/// release's mirrors; when the links that name the file agree on one name,
/// every entry gets it, so their rows merge before anything downloads.
/// Several links on one host, or names that disagree, are several files and
/// stay as they are.
fn rows(urls: Vec<String>) -> Vec<Entry> {
    let hosts: HashSet<String> = urls.iter().map(|url| host_of(url)).collect();
    let names: HashSet<String> = urls.iter().filter_map(|url| guess_name(url)).collect();
    let shared = (urls.len() == hosts.len() && names.len() == 1)
        .then(|| names.into_iter().next().expect("one name"));
    urls.into_iter()
        .map(|url| {
            let name = shared.clone().or_else(|| guess_name(&url));
            Entry {
                name,
                url,
                deferred: true,
                ..Entry::default()
            }
        })
        .collect()
}

pub(super) async fn resolve(cx: &Cx<'_>, url: &str) -> Result<Vec<Entry>, ResolveError> {
    let base = &cx.cfg.endpoints.fileguard;
    let path = url::Url::parse(url)
        .ok()
        .map(|u| u.path().to_string())
        .filter(|path| path.len() > 1)
        .ok_or_else(|| ResolveError::failed("bad fileguard url"))?;
    let page = format!("{base}{path}");
    let r = cx.http.client.get(&page).send().await?;
    let status = r.status().as_u16();
    if matches!(status, 429 | 503) {
        let seconds = retry_after(r.headers(), 90);
        return Err(ResolveError::Wait {
            seconds,
            message: format!("{status} rate limited"),
        });
    }
    if matches!(status, 404 | 410) || status >= 500 {
        return Err(ResolveError::Failed(format!("HTTP {status}")));
    }
    if status >= 400 {
        return Err(ResolveError::NeedsUser(format!(
            "fileguard.cc answered HTTP {status}; open the page"
        )));
    }
    let html = r.text().await?;
    let Some(csrf) = CSRF.captures(&html).map(|c| c[1].to_string()) else {
        return Err(ResolveError::NeedsUser(
            "fileguard.cc answered without its proof-of-work form; open the page".into(),
        ));
    };
    let r = cx
        .http
        .client
        .get(format!("{base}/altcha_challenge.php"))
        .header(REFERER, &page)
        .send()
        .await?;
    let status = r.status().as_u16();
    if matches!(status, 429 | 503) {
        let seconds = retry_after(r.headers(), 90);
        return Err(ResolveError::Wait {
            seconds,
            message: format!("{status} rate limited"),
        });
    }
    if matches!(status, 404 | 410) || status >= 500 {
        return Err(ResolveError::Failed(format!("HTTP {status}")));
    }
    if status >= 400 {
        return Err(ResolveError::NeedsUser(format!(
            "fileguard.cc challenge answered HTTP {status}; open the page"
        )));
    }
    let challenge: Challenge = r.json().await?;
    let number = {
        let (salt, hash, max) = (
            challenge.salt.clone(),
            challenge.challenge.clone(),
            challenge.max_number,
        );
        // The search is CPU work with no await in it: keep it off the reactor.
        tokio::task::spawn_blocking(move || solve(&salt, &hash, max))
            .await
            .map_err(|e| ResolveError::failed(e.to_string()))?
            .ok_or_else(|| ResolveError::failed("proof of work out of range"))?
    };
    let solution = Solution {
        algorithm: &challenge.algorithm,
        challenge: &challenge.challenge,
        number,
        salt: &challenge.salt,
        signature: &challenge.signature,
    };
    let payload = base64::engine::general_purpose::STANDARD
        .encode(serde_json::to_string(&solution).map_err(|e| ResolveError::failed(e.to_string()))?);
    let r = cx
        .http
        .client
        .post(&page)
        .header(REFERER, &page)
        .form(&[("csrf_token", csrf), ("altcha", payload)])
        .send()
        .await?;
    let status = r.status().as_u16();
    if matches!(status, 429 | 503) {
        let seconds = retry_after(r.headers(), 90);
        return Err(ResolveError::Wait {
            seconds,
            message: format!("{status} rate limited"),
        });
    }
    if matches!(status, 404 | 410) || status >= 500 {
        return Err(ResolveError::Failed(format!("HTTP {status}")));
    }
    if status >= 400 {
        return Err(ResolveError::NeedsUser(format!(
            "fileguard.cc answered HTTP {status} to the solve; open the page"
        )));
    }
    let solved = r.text().await?;
    let urls = target_links(&solved);
    if urls.is_empty() {
        return Err(ResolveError::failed(
            "solved without links (expired, or the page changed)",
        ));
    }
    Ok(rows(urls))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proof_of_work_finds_the_number() {
        let salt = "cc348abfe0f7d4fded4b8dba?expires=1792121341";
        let number = 13186;
        let challenge = {
            let mut hash = sha2::Sha256::new();
            hash.update(salt.as_bytes());
            hash.update(number.to_string().as_bytes());
            to_hex(&hash.finalize())
        };
        assert_eq!(solve(salt, &challenge, 180_000), Some(number));
        assert_eq!(solve(salt, &challenge, number - 1), None);
        assert_eq!(solve(salt, &"0".repeat(64), 100), None);
    }

    #[test]
    fn one_file_pages_name_every_mirror() {
        // One link per host, one of them named: every row gets that name,
        // so their rows merge into the file's mirrors.
        let named = rows(vec![
            "https://1fichier.com/?cu5oghhjkskhuzap8w43".into(),
            "https://pixeldrain.com/u/VF18oYXa".into(),
            "https://rapidgator.net/file/xyz/Sledders.RUNE.rar".into(),
        ]);
        assert_eq!(
            named
                .iter()
                .filter(|e| e.name.as_deref() == Some("Sledders.RUNE.rar"))
                .count(),
            3
        );

        // Two links on one host are two files: names stay as they are.
        let several = rows(vec![
            "https://host.example/a/one.rar".into(),
            "https://host.example/b/two.rar".into(),
            "https://other.example/x".into(),
        ]);
        assert_eq!(
            several
                .iter()
                .map(|e| e.name.as_deref())
                .collect::<Vec<_>>(),
            [Some("one.rar"), Some("two.rar"), None]
        );

        // Names that disagree on a one-link-per-host page stay their own.
        let disagree = rows(vec![
            "https://host.example/a/one.rar".into(),
            "https://other.example/b/two.rar".into(),
        ]);
        assert_eq!(
            disagree
                .iter()
                .map(|e| e.name.as_deref())
                .collect::<Vec<_>>(),
            [Some("one.rar"), Some("two.rar")]
        );
    }

    #[test]
    fn links_are_the_target_anchors() {
        let html = r#"
            <a class="nav-link" href="/login.php">Login</a>
            <div class="link-group" data-host="1fichier.com">
              <a class="target-link" href="https://1fichier.com/?cu5oghhjkskhuzap8w43&amp;af=2701919">one</a>
            </div>
            <div class="link-group" data-host="pixeldrain.com">
              <a class="target-link" href="https://pixeldrain.com/u/VF18oYXa">two</a>
              <a class="target-link" href="https://pixeldrain.com/u/VF18oYXa">same again</a>
            </div>
        "#;
        assert_eq!(
            target_links(html),
            [
                "https://1fichier.com/?cu5oghhjkskhuzap8w43&af=2701919",
                "https://pixeldrain.com/u/VF18oYXa"
            ]
        );
        assert!(target_links("<a href=\"https://x.example/\">x</a>").is_empty());
    }
}
