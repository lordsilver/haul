//! What HTTP headers say: a file's name, type, length and range, and how long to wait.

use super::*;

/// RFC 5987's `filename*=UTF-8'<language>'<percent-encoded name>`, with the
/// language usually left empty.
pub(super) static CD_EXT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)filename\*\s*=\s*UTF-8'[^']*'([^;]+)"#).unwrap());
pub(super) static CD_PLAIN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)filename\s*=\s*"?([^";]+)"#).unwrap());

/// The file name in `Content-Disposition`, preferring the encoded form.
pub(crate) fn disposition_name(headers: &HeaderMap) -> Option<String> {
    let cd = headers.get(CONTENT_DISPOSITION)?.to_str().ok()?;
    let raw = CD_EXT
        .captures(cd)
        .or_else(|| CD_PLAIN.captures(cd))?
        .get(1)?
        .as_str()
        .trim();
    let name = percent_encoding::percent_decode_str(raw)
        .decode_utf8_lossy()
        .trim()
        .to_string();
    (!name.is_empty()).then_some(name)
}

pub(crate) fn content_type(headers: &HeaderMap) -> &str {
    headers
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
}

/// `Content-Length` as sent. Read from the header because reqwest reports a
/// HEAD response's body length, which is always zero.
pub(crate) fn content_length(headers: &HeaderMap) -> Option<u64> {
    headers
        .get(CONTENT_LENGTH)?
        .to_str()
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// `(start, total)` from `Content-Range: bytes START-END/TOTAL`.
pub(crate) fn content_range(headers: &HeaderMap) -> Option<(u64, Option<u64>)> {
    let v = headers.get(CONTENT_RANGE)?.to_str().ok()?.trim();
    let rest = v.strip_prefix("bytes")?.trim_start();
    let (range, total) = rest.split_once('/')?;
    let start = range.split_once('-')?.0.trim().parse().ok()?;
    Some((start, total.trim().parse().ok()))
}

/// The size a 416 states (`Content-Range: bytes */1234`), when it does.
pub(crate) fn unsatisfied_total(headers: &HeaderMap) -> Option<u64> {
    let v = headers.get(CONTENT_RANGE)?.to_str().ok()?;
    v.trim()
        .strip_prefix("bytes")?
        .trim_start()
        .strip_prefix("*/")?
        .trim()
        .parse()
        .ok()
}

/// Seconds from `Retry-After`, or `default` when absent or a date.
pub(crate) fn retry_after(headers: &HeaderMap, default: u64) -> u64 {
    headers
        .get(RETRY_AFTER)
        .and_then(|v| v.to_str().ok()?.trim().parse().ok())
        .unwrap_or(default)
}

/// A 429 or 503 as the wait it asks for: a rate limit, not a refusal.
pub(crate) fn rate_limited(r: &reqwest::Response) -> Result<(), ResolveError> {
    wait_from(r, "", 90)
}

/// A 429 or 503 from `host` as the wait it asks for, `otherwise` seconds
/// when it names none.
pub(crate) fn wait_from(
    r: &reqwest::Response,
    host: &str,
    otherwise: u64,
) -> Result<(), ResolveError> {
    let status = r.status().as_u16();
    if matches!(status, 429 | 503) {
        let seconds = retry_after(r.headers(), otherwise);
        let host = if host.is_empty() {
            String::new()
        } else {
            format!("{host}: ")
        };
        return Err(ResolveError::Wait {
            seconds,
            message: format!("{host}{status} rate limited"),
        });
    }
    Ok(())
}

/// A 404, 410 or 5xx as a failure: gone or broken is not something a person
/// can fix in a browser.
pub(super) fn gone_or_broken(r: &reqwest::Response) -> Result<(), ResolveError> {
    let status = r.status().as_u16();
    if matches!(status, 404 | 410) || status >= 500 {
        return Err(ResolveError::Failed(format!("HTTP {status}")));
    }
    Ok(())
}

/// A host API's JSON answer. A rate limit is waited out, a server's trouble
/// is tried again, a missing file is gone, and an answer that is no JSON (a
/// gateway's error page) is the host's passing trouble too, not a dead link.
pub(super) async fn api_json<T: serde::de::DeserializeOwned>(
    r: reqwest::Response,
    host: &str,
) -> Result<T, ResolveError> {
    rate_limited(&r)?;
    let status = r.status().as_u16();
    if matches!(status, 404 | 410) {
        return Err(ResolveError::gone(format!("{host}: HTTP {status}")));
    }
    if status >= 500 {
        return Err(ResolveError::Network(format!("{host}: HTTP {status}")));
    }
    // A refusal may say why in the host's own words, which the resolver
    // reads; one that does not is a plain refusal.
    let refused = status >= 400;
    r.json().await.map_err(|e| {
        if refused {
            ResolveError::Failed(format!("{host}: HTTP {status}"))
        } else {
            ResolveError::Network(format!(
                "{host}: not the answer expected ({})",
                error_text(e)
            ))
        }
    })
}
