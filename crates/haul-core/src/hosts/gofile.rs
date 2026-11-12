//! gofile: the content API, with the user's account token. gofile serves
//! that API only to premium accounts and to its own website, which signs
//! each request with a token derived from a secret it rotates; a request
//! signed with a retired secret gets the address banned. Haul does not
//! sign: without an account token it leaves the link to a person, without
//! asking gofile anything.

use std::collections::BTreeMap;

use serde_json::Value;

use super::{Algo, Checksum, Cx, Entry, ResolveError};

pub(super) async fn resolve(cx: &Cx<'_>, url: &str) -> Result<Vec<Entry>, ResolveError> {
    let Some(token) = cx.cfg.account("gofile_token").map(str::to_string) else {
        return Err(ResolveError::person(
            "gofile lets only premium accounts and its own website fetch links: open the page, or set a premium gofile_token",
        ));
    };
    let id = super::last_path_part(url);
    let base = &cx.cfg.endpoints.gofile;
    let j = cx
        .http
        .client
        .get(format!("{base}/contents/{id}"))
        .bearer_auth(&token)
        .send()
        .await?;
    let j: Value = super::api_json(j, "gofile").await?;
    match j["status"].as_str().unwrap_or_default() {
        "ok" => {}
        // A free account is turned away as a guest is, and passwords are
        // typed in the browser; either way a person has to open the page.
        status @ ("error-notPremium" | "error-passwordRequired") => {
            return Err(ResolveError::person(format!("gofile: {status}")));
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
                checksum: child["md5"]
                    .as_str()
                    .and_then(|h| Checksum::from_hex(Algo::Md5, h)),
                ..Entry::default()
            });
        }
    }
}

/// What a host's page was answered when it asked its own API for its file,
/// as the hand-off script reports it ([`page_answer`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PageAnswer {
    /// The file or folder is no more.
    Gone(String),
    /// The host asked this address to wait: a cooldown for the host, and the
    /// row asked again after it.
    Later { reason: String, seconds: u64 },
    /// Any other error: failed for now, tried again later.
    Failed(String),
}

/// How long gofile's "Too many requests" holds the host back, at first.
const GOFILE_LIMIT_WAIT: u64 = 120;

/// What a page on `site` was answered by its host's API (`http`, the
/// answer's own `status`), when that is not its files.
pub fn page_answer(site: &str, http: u16, status: &str) -> PageAnswer {
    let site = site.trim_start_matches("www.");
    let limited = http == 429 || status.to_ascii_lowercase().contains("ratelimit");
    if limited {
        return PageAnswer::Later {
            reason: format!("{site}: too many requests from this address"),
            seconds: GOFILE_LIMIT_WAIT,
        };
    }
    match status {
        "error-notFound" => PageAnswer::Gone(format!("{site}: the folder was deleted")),
        _ if http == 404 => PageAnswer::Gone(format!("{site}: the folder was deleted")),
        "" => PageAnswer::Failed(format!("{site}: answered {http}")),
        other => PageAnswer::Failed(format!("{site}: {other} ({http})")),
    }
}

#[cfg(test)]
mod answer_tests {
    use super::*;

    #[test]
    fn a_rate_limit_is_a_wait_and_not_found_is_gone() {
        assert!(matches!(
            page_answer("gofile.io", 429, "error-rateLimit"),
            PageAnswer::Later {
                seconds: GOFILE_LIMIT_WAIT,
                ..
            }
        ));
        assert!(matches!(
            page_answer("gofile.io", 200, "error-rateLimit"),
            PageAnswer::Later { .. }
        ));
        assert_eq!(
            page_answer("gofile.io", 404, "error-notFound"),
            PageAnswer::Gone("gofile.io: the folder was deleted".into())
        );
        assert_eq!(
            page_answer("gofile.io", 500, ""),
            PageAnswer::Failed("gofile.io: answered 500".into())
        );
        assert_eq!(
            page_answer("gofile.io", 403, "error-notPublic"),
            PageAnswer::Failed("gofile.io: error-notPublic (403)".into())
        );
    }
}
