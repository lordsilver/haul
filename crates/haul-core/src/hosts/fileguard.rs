//! fileguard.cc: a wrapper page that lists one file's links on many hosts
//! behind an ALTCHA proof of work. That gate asks for no person — the
//! challenge is a hash (SHA-256 today) the client can search — so one solve
//! here yields every mirror link at once.

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

use super::{Cx, Entry, ResolveError, guess_name, host_of, is_wrapper, retry_after};

static CSRF: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"name="csrf_token"\s+value="([^"]+)""#).unwrap());
/// Where the page's widget fetches its challenge.
static CHALLENGE_URL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)<altcha-widget\s[^>]*challengeurl="([^"]+)""#).unwrap());
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

/// Tells a search on another thread to stop once the resolve that started it
/// is dropped, as a paused or removed download's is.
struct StopOnDrop(Arc<AtomicBool>);

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
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
/// stay as they are. A link to another wrapper is more links, not the file,
/// so it keeps a row of its own.
fn rows(urls: Vec<String>) -> Vec<Entry> {
    let hosts: HashSet<String> = urls.iter().map(|url| host_of(url)).collect();
    let names: HashSet<String> = urls
        .iter()
        .filter(|url| !is_wrapper(url))
        .filter_map(|url| guess_name(url))
        .collect();
    let shared = (urls.len() == hosts.len() && names.len() == 1)
        .then(|| names.into_iter().next().expect("one name"));
    urls.into_iter()
        .map(|url| {
            let name = match &shared {
                Some(name) if !is_wrapper(&url) => Some(name.clone()),
                _ => guess_name(&url),
            };
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

        // Another wrapper's links are more links, not this file: they neither
        // take its name nor stop the others sharing it.
        let nested = rows(vec![
            "https://rapidgator.net/file/xyz/Sledders.RUNE.rar".into(),
            "https://pixeldrain.com/u/VF18oYXa".into(),
            "https://fileguard.cc/0123456789/pixeldrain".into(),
            "https://filecrypt.cc/DLC/ABCDEF.dlc".into(),
        ]);
        assert_eq!(
            nested.iter().map(|e| e.name.as_deref()).collect::<Vec<_>>(),
            [
                Some("Sledders.RUNE.rar"),
                Some("Sledders.RUNE.rar"),
                None,
                Some("ABCDEF.dlc")
            ]
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
