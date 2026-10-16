//! fileguard.cc: a wrapper page (a "vault") that lists a release's files on
//! many hosts behind an ALTCHA proof of work. That gate asks for no person —
//! the challenge is a hash (SHA-256 today) the client can search — so one
//! solve here yields every link at once.

use std::{
    collections::HashSet,
    sync::{
        Arc, LazyLock,
        atomic::{AtomicBool, Ordering},
    },
};

use base64::Engine as _;
use regex::Regex;
use reqwest::header::REFERER;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256, Sha512};

use super::{Cx, ResolveError, StopOnDrop, Unwrapped, group_mirrors, page_text, retry_after};

/// One vault at a time: a burst of solves from one address is refused, and
/// a session that has passed a vault's gate is shown its links straight away.
static SOLVING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

static CSRF: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"name="csrf_token"\s+value="([^"]+)""#).unwrap());
/// Where the page's widget fetches its challenge.
static CHALLENGE_URL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)<altcha-widget\s[^>]*challengeurl="([^"]+)""#).unwrap());
static ANCHOR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"(?is)<a\s[^>]*>"#).unwrap());
/// The vault's name, above its links.
static TITLE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?is)class="folder-title"[^>]*>\s*<h1[^>]*>(.*?)</h1>"#).unwrap()
});
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

/// The number whose hash of `salt + number` is `challenge`, searched up to
/// `max_number` unless `stop` is set first.
fn solve<D: Digest + Clone>(
    salt: &str,
    challenge: &[u8],
    max_number: u64,
    stop: &AtomicBool,
) -> Option<u64> {
    let mut salted = D::new();
    salted.update(salt.as_bytes());
    (0..=max_number)
        .take_while(|number| !number.is_multiple_of(4096) || !stop.load(Ordering::Relaxed))
        .find(|number| {
            let mut hash = salted.clone();
            hash.update(number.to_string().as_bytes());
            hash.finalize()[..] == *challenge
        })
}

/// The search for the algorithm a challenge names; ALTCHA's are spelled
/// `SHA-256` and the like.
type Search = fn(&str, &[u8], u64, &AtomicBool) -> Option<u64>;

fn search_for(algorithm: &str) -> Option<Search> {
    match algorithm.to_ascii_uppercase().as_str() {
        "SHA-256" => Some(solve::<Sha256>),
        "SHA-512" => Some(solve::<Sha512>),
        _ => None,
    }
}

/// `hex` as bytes, when it is hexadecimal.
fn unhex(hex: &str) -> Option<Vec<u8>> {
    if !hex.len().is_multiple_of(2) {
        return None;
    }
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok())
        .collect()
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

/// The vault a link points at: every `/<vault>/<host>` link of one vault
/// shows the same page, the host only choosing where it scrolls to.
pub(super) fn key(url: &str) -> Option<String> {
    let url = url::Url::parse(url).ok()?;
    let vault = url.path_segments()?.find(|s| !s.is_empty())?;
    Some(format!("fileguard.cc/{vault}"))
}

/// The vault's name, as its page shows it.
fn title(html: &str) -> Option<String> {
    page_text(&TITLE.captures(html)?[1])
}

pub(super) async fn unwrap(cx: &Cx<'_>, url: &str) -> Result<Unwrapped, ResolveError> {
    let base = &cx.cfg.endpoints.fileguard;
    let path = url::Url::parse(url)
        .ok()
        .map(|u| u.path().to_string())
        .filter(|path| path.len() > 1)
        .ok_or_else(|| ResolveError::failed("bad fileguard url"))?;
    let page = format!("{base}{path}");
    let _one_at_a_time = SOLVING.lock().await;
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
    // A session that passed this vault's gate before is shown its links.
    let shown = target_links(&html);
    if !shown.is_empty() {
        return Ok(Unwrapped {
            title: title(&html),
            files: group_mirrors(shown),
        });
    }
    let Some(csrf) = CSRF.captures(&html).map(|c| c[1].to_string()) else {
        return Err(ResolveError::NeedsUser(
            "fileguard.cc answered without its proof-of-work form; open the page".into(),
        ));
    };
    // Where the page's own widget asks, and where fileguard.cc has it otherwise.
    let challenge_url = CHALLENGE_URL
        .captures(&html)
        .and_then(|c| {
            let page = url::Url::parse(&page).ok()?;
            page.join(&c[1].replace("&amp;", "&")).ok()
        })
        .map_or_else(|| format!("{base}/altcha_challenge.php"), String::from);
    let r = cx
        .http
        .client
        .get(challenge_url)
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
    let challenge: Challenge = match r.json().await {
        Ok(challenge) => challenge,
        // A page where the challenge should be, a browser check say.
        Err(e) if e.is_decode() => {
            return Err(ResolveError::NeedsUser(
                "fileguard.cc answered without its proof-of-work challenge; open the page".into(),
            ));
        }
        Err(e) => return Err(e.into()),
    };
    // A challenge Haul cannot work is still one a browser can.
    let unsolved = |why: &str| {
        ResolveError::NeedsUser(format!("fileguard.cc's proof of work {why}; open the page"))
    };
    let Some(search) = search_for(&challenge.algorithm) else {
        return Err(unsolved(&format!(
            "uses {}, which Haul does not do",
            challenge.algorithm
        )));
    };
    let Some(target) = unhex(&challenge.challenge) else {
        return Err(unsolved("is not hexadecimal"));
    };
    let number = {
        let (salt, max) = (challenge.salt.clone(), challenge.max_number);
        let stop = Arc::new(AtomicBool::new(false));
        // Dropping this resolve (the download paused or removed) stops the
        // search as well; the host decides how long it may be.
        let _stop = StopOnDrop(stop.clone());
        // The search is CPU work with no await in it: keep it off the reactor.
        tokio::task::spawn_blocking(move || search(&salt, &target, max, &stop))
            .await
            .map_err(|e| ResolveError::failed(e.to_string()))?
            .ok_or_else(|| unsolved("has no answer Haul can find"))?
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
        // Seen as 403 when many solves came at once: worth another try later,
        // not a page for a person.
        return Err(ResolveError::Network(format!(
            "refused the solve (HTTP {status})"
        )));
    }
    let solved = r.text().await?;
    let urls = target_links(&solved);
    if urls.is_empty() {
        return Err(ResolveError::failed(
            "solved without links (expired, or the page changed)",
        ));
    }
    Ok(Unwrapped {
        title: title(&solved),
        files: group_mirrors(urls),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest<D: Digest>(salt: &str, number: u64) -> Vec<u8> {
        let mut hash = D::new();
        hash.update(salt.as_bytes());
        hash.update(number.to_string().as_bytes());
        hash.finalize().to_vec()
    }

    #[test]
    fn proof_of_work_finds_the_number() {
        let salt = "cc348abfe0f7d4fded4b8dba?expires=1792121341";
        let number = 13186;
        let go = AtomicBool::new(false);
        let challenge = digest::<Sha256>(salt, number);
        assert_eq!(
            solve::<Sha256>(salt, &challenge, 180_000, &go),
            Some(number)
        );
        assert_eq!(solve::<Sha256>(salt, &challenge, number - 1, &go), None);
        assert_eq!(solve::<Sha256>(salt, &[0; 32], 100, &go), None);
        // The algorithm the challenge names.
        let sha512 = search_for("SHA-512").unwrap();
        assert_eq!(
            sha512(salt, &digest::<Sha512>(salt, number), 180_000, &go),
            Some(number)
        );
        assert!(search_for("SHA-1").is_none());
        // A search told to stop gives up, however far it may go.
        let stop = AtomicBool::new(true);
        assert_eq!(solve::<Sha256>(salt, &challenge, u64::MAX, &stop), None);
        assert_eq!(unhex("0aFf"), Some(vec![0x0a, 0xff]));
        assert_eq!(unhex("0a0"), None);
        assert_eq!(unhex("zz"), None);
    }

    #[test]
    fn a_vault_is_one_key_and_has_a_title() {
        for url in [
            "https://fileguard.cc/663fd651cb/mega",
            "https://www.fileguard.cc/663fd651cb/transfer?x=1",
            "https://fileguard.cc/663fd651cb",
        ] {
            assert_eq!(
                key(url).as_deref(),
                Some("fileguard.cc/663fd651cb"),
                "{url}"
            );
        }
        assert_eq!(key("https://fileguard.cc/"), None);
        assert_eq!(
            title(r#"<div class="folder-title"><h1>Frosthaven-RUNE &amp; more</h1></div>"#)
                .as_deref(),
            Some("Frosthaven-RUNE & more")
        );
        assert_eq!(title("<h1>Elsewhere</h1>"), None);
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
