//! filecrypt.cc: a container page behind a proof-of-work checkbox, "I am a
//! human". The page's widget (`pow_captcha.js`) asks `/captchasession/` for
//! a challenge, sending what its automation checks found (`pow_x`) and an id
//! from a probe endpoint (`pow_y`); finds the nonce whose SHA-1 with the
//! challenge starts with enough zero bits; and posts the form with that
//! proof and its report of the visit (`pow_data`): where the pointer moved
//! and clicked, and what the browser can draw. filecrypt.cc turns away a
//! report with an automation check flagged, or without the pointer's moves
//! and click, and asks for nothing more.
//!
//! Haul does what the widget does, without a browser: it works the proof,
//! reports no automation found and the moves and click a person makes on
//! the box, and leaves canvas, WebGL, audio, fonts and screen unreported, as
//! a client without them must. The solved page holds the container's links
//! twice: as Click'n'Load data, encrypted with a key beside it, which Haul
//! decrypts itself, and as a `.dlc`, unpacked through JDownloader's key
//! service, the fallback. A solve turned away waits for a person, whose
//! browser hands back the `.dlc` link the solved page offers; that link is
//! not gated.

use std::{
    collections::HashSet,
    sync::{
        Arc, LazyLock,
        atomic::{AtomicBool, Ordering},
    },
};

use regex::Regex;
use reqwest::header::{ORIGIN, REFERER};
use serde::Serialize;
use sha1::{Digest, Sha1};

use super::{
    Cx, Entry, ResolveError, StopOnDrop, Unwrapped, content_type, group_named, guess_name,
    page_text, retry_after,
};
use crate::container;

/// One solve at a time: filecrypt.cc asks for more work from an address
/// that solves in bursts.
static SOLVING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

static DLC: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^/DLC/([\w-]+)\.dlc/?$").unwrap());
static CONTAINER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^/Container/([\w-]+)(?:\.html)?/?$").unwrap());
/// The gate's widget, whose `data-*` attributes say where to ask.
static WIDGET: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?is)<div\b[^>]*\bid="pow-captcha"[^>]*>"#).unwrap());
static DATA: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"\bdata-([a-z-]+)="([^"]*)""#).unwrap());
/// A row of a solved page's link table: its status, then the file's name.
static ROW_NAME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?s)<tr\b[^>]*>\s*<td class="status">.*?</td>\s*<td title="([^"]*)""#).unwrap()
});
/// A solved page's Click'n'Load data: `CNLPOP(id, key, links, package)`.
static CNL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"CNLPOP\(\s*'[^']*'\s*,\s*'([0-9A-Fa-f]+)'\s*,\s*'([^']+)'").unwrap()
});
/// The attribute of a solved page's `.dlc` button that holds its id.
static DLC_BUTTON: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"DownloadDLC\(this\.getAttribute\('([\w-]+)'\)\)").unwrap());
static COMMENT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)<!--.*?-->").unwrap());
static PASSWORD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)<input\b[^>]*\bname="password""#).unwrap());
/// The container's name, above its links.
static HEADING: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?is)<h2\b[^>]*>(.*?)</h2>").unwrap());

/// What the widget's automation checks (`m.js`) send: their format, "2",
/// then which of its six checks flagged something (none), which found
/// nothing (all six) and base64 of the JSON list of what they noted (`[]`).
const CHECKS: &str = "2.0.63.W10=";

/// How many SHA-1 hashes the widget's worker tries per millisecond in a
/// desktop browser, about what V8 manages: `pow_elapsed` is how long the
/// page itself would have taken, since Haul's own search takes a fraction
/// of that.
const BROWSER_HASHES_PER_MS: u64 = 3_000;

/// The container id in a `.dlc` link, when the link is one.
fn dlc_id(url: &str) -> Option<String> {
    let path = url::Url::parse(url).ok()?.path().to_string();
    Some(DLC.captures(&path)?[1].to_string())
}

/// The container id in a container page's link, when the link is one.
fn container_id(url: &str) -> Option<String> {
    let path = url::Url::parse(url).ok()?.path().to_string();
    Some(CONTAINER.captures(&path)?[1].to_string())
}

/// What a link points at: a container's page, or one `.dlc` it handed out.
pub(super) fn key(url: &str) -> Option<String> {
    if let Some(id) = dlc_id(url) {
        return Some(format!("filecrypt.cc/dlc/{id}"));
    }
    Some(format!("filecrypt.cc/container/{}", container_id(url)?))
}

/// The first nonce, counting from 0 as the page's worker does, whose SHA-1
/// of `challenge:nonce` starts with `difficulty` zero bits; `None` once
/// `stop` is set.
fn work(challenge: &str, difficulty: u32, stop: &AtomicBool) -> Option<u64> {
    let mut prefix = Sha1::new();
    prefix.update(challenge.as_bytes());
    prefix.update(b":");
    let mut digits = [0u8; 20];
    (0..u64::MAX)
        .take_while(|nonce| !nonce.is_multiple_of(4096) || !stop.load(Ordering::Relaxed))
        .find(|&nonce| {
            let mut hash = prefix.clone();
            hash.update(decimal(nonce, &mut digits));
            zero_bits(&hash.finalize()) >= difficulty
        })
}

/// `n` in decimal, written into `buf` without allocating.
fn decimal(mut n: u64, buf: &mut [u8; 20]) -> &[u8] {
    let mut at = buf.len();
    loop {
        at -= 1;
        buf[at] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            return &buf[at..];
        }
    }
}

/// How many zero bits a digest starts with.
fn zero_bits(digest: &[u8]) -> u32 {
    let mut bits = 0;
    for &byte in digest {
        if byte != 0 {
            return bits + byte.leading_zeros();
        }
        bits += 8;
    }
    bits
}

/// A random number below `n`, for what has to vary between visits.
fn random(n: u32) -> u32 {
    (uuid::Uuid::new_v4().as_u128() % u128::from(n.max(1))) as u32
}

/// The widget's nonce for the probe endpoints: two random 32-bit numbers in
/// base 36.
fn probe_nonce() -> String {
    let base36 = |mut n: u32| {
        let mut out = Vec::new();
        loop {
            out.push(b"0123456789abcdefghijklmnopqrstuvwxyz"[(n % 36) as usize]);
            n /= 36;
            if n == 0 {
                break;
            }
        }
        out.reverse();
        String::from_utf8(out).unwrap_or_default()
    };
    let n = uuid::Uuid::new_v4().as_u128();
    base36(n as u32) + &base36((n >> 32) as u32)
}

/// `navigator.platform` for the system Haul runs on.
fn platform() -> String {
    match std::env::consts::OS {
        "windows" => "Win32".into(),
        "macos" => "MacIntel".into(),
        _ => format!("Linux {}", std::env::consts::ARCH),
    }
}

/// What the widget's signals script (`s.js`) reports when the form is
/// posted, field for field and in its order. A client without a browser
/// has no canvas, WebGL, audio, fonts or screen to describe: those say
/// "NA" or 0, as they do when a browser cannot tell.
#[derive(Serialize)]
struct Report<'a> {
    /// Where the pointer first moved, and where it clicked the box.
    x: u32,
    y: u32,
    click_x: u32,
    click_y: u32,
    moves: u32,
    is_touch: bool,
    #[serde(rename = "workerData")]
    worker: Worker<'a>,
    /// The click came from a pointer.
    ui: bool,
    ua: Agent<'a>,
    #[serde(rename = "audioFp")]
    audio: &'a str,
    fonts: &'a str,
    #[serde(rename = "webGpu")]
    webgpu: &'a str,
    #[serde(rename = "webGpuLimits")]
    webgpu_limits: &'a str,
    pframes: u8,
    pframes_src: &'a str,
    pframes_func: &'a str,
    is_adguard: u8,
    /// `navigator.userActivation`: active, and active before.
    usa: [u8; 2],
    tz: &'a str,
    /// Milliseconds into the visit of the first move and of the click.
    ta: f64,
    tb: f64,
    sw: u32,
    sh: u32,
    ow: u32,
    oh: u32,
    iw: u32,
    ih: u32,
    cd: u32,
    aw: u32,
    ae: u32,
    dm: u32,
    cg: &'a str,
    pt: &'a str,
    ho: &'a str,
}

/// What the script's worker reports, compared with the page's own view.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Worker<'a> {
    #[serde(rename = "webGLVendor")]
    webgl_vendor: &'a str,
    #[serde(rename = "webGLRenderer")]
    webgl_renderer: &'a str,
    user_agent: &'a str,
    languages: &'a [&'a str],
    platform: &'a str,
    hardware_concurrency: usize,
    cdp_check1: bool,
    is_same_as_main_js_context: bool,
    webdriver: bool,
    canvas_fp: &'a str,
    web_gl_fp: &'a str,
    web_gl_params_fp: &'a str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Agent<'a> {
    user_agent: &'a str,
    platform: &'a str,
    languages: &'a [&'a str],
    hardware_concurrency: usize,
}

/// A report's JSON as the form sends it: base64 without padding, `a`
/// written `@` and `e` written `.`, reversed, after `signal_`.
fn signals(json: &str) -> String {
    use base64::Engine as _;
    let reversed: String = base64::engine::general_purpose::STANDARD_NO_PAD
        .encode(json)
        .chars()
        .rev()
        .map(|c| match c {
            'a' => '@',
            'e' => '.',
            c => c,
        })
        .collect();
    format!("signal_{reversed}")
}

/// A person's visit to `page`, as the widget reports it: the pointer comes
/// in from the right, moves to the box and clicks it a second or so later.
fn visit(user_agent: &str, tz: &str, page: &str) -> String {
    let platform = platform();
    let languages = ["en-US", "en"];
    let cores = std::thread::available_parallelism().map_or(4, std::num::NonZero::get);
    let first_move = f64::from(700 + random(900)) + f64::from(1 + random(9)) / 10.0;
    let click = first_move + f64::from(600 + random(800)) + f64::from(1 + random(9)) / 10.0;
    let report = Report {
        x: 900 + random(500),
        y: 120 + random(180),
        // The box's checkbox, on a page 1920 pixels wide.
        click_x: 793 + random(22),
        click_y: 413 + random(22),
        moves: 25 + random(36),
        is_touch: false,
        worker: Worker {
            webgl_vendor: "NA",
            webgl_renderer: "NA",
            user_agent,
            languages: &languages,
            platform: &platform,
            hardware_concurrency: cores,
            cdp_check1: false,
            is_same_as_main_js_context: true,
            webdriver: false,
            canvas_fp: "NA",
            web_gl_fp: "NA",
            web_gl_params_fp: "NA",
        },
        ui: true,
        ua: Agent {
            user_agent,
            platform: &platform,
            languages: &languages,
            hardware_concurrency: cores,
        },
        audio: "NA",
        fonts: "NA",
        webgpu: "NA",
        webgpu_limits: "NA",
        pframes: 0,
        pframes_src: page,
        pframes_func: "",
        is_adguard: 0,
        usa: [1, 1],
        tz,
        ta: first_move,
        tb: click,
        sw: 0,
        sh: 0,
        ow: 0,
        oh: 0,
        iw: 0,
        ih: 0,
        cd: 0,
        aw: 0,
        ae: 0,
        dm: 0,
        cg: "0",
        pt: "0",
        ho: "0",
    };
    signals(&serde_json::to_string(&report).unwrap_or_default())
}

/// An answer that is not the page to read: a rate limit to wait out, a
/// container that is gone, or a refusal a person may get past.
fn refusal(r: &reqwest::Response, what: &str) -> Option<ResolveError> {
    // Cloudflare's check in front of the page (with a 403, or a 503): a
    // browser passes it, which the desktop app does out of sight.
    if super::generic::challenged(r.headers()) {
        return Some(super::generic::guarded(r.url().as_str()));
    }
    let status = r.status().as_u16();
    if matches!(status, 429 | 503) {
        return Some(ResolveError::Wait {
            seconds: retry_after(r.headers(), 90),
            message: format!("{status} rate limited"),
        });
    }
    if matches!(status, 404 | 410) || status >= 500 {
        return Some(ResolveError::Failed(format!("HTTP {status}")));
    }
    (status >= 400).then(|| {
        ResolveError::NeedsUser(format!(
            "filecrypt.cc answered {what} with HTTP {status}; open the page"
        ))
    })
}

/// The `.dlc` id a solved page's button holds.
fn dlc_on(html: &str) -> Option<String> {
    let attribute = &DLC_BUTTON.captures(html)?[1];
    let value = Regex::new(&format!(r#"(?i)\b{}="([\w-]+)""#, regex::escape(attribute))).ok()?;
    Some(value.captures(html)?[1].to_string())
}

/// The links of a solved page: its Click'n'Load data, decrypted here, or
/// else its `.dlc`.
async fn solved(cx: &Cx<'_>, html: &str) -> Result<Unwrapped, ResolveError> {
    let title = HEADING.captures(html).and_then(|h| page_text(&h[1]));
    let urls: Vec<String> = CNL
        .captures_iter(html)
        .filter_map(|cnl| container::parse_cnl(&cnl[1], &cnl[2]).ok())
        .flatten()
        .collect();
    if !urls.is_empty() {
        // The table above lists each link's file in the same order: names
        // for hosts whose links carry none, so their files merge with the
        // same ones elsewhere before anything downloads.
        let names = row_names(html);
        let names = if names.len() == urls.len() {
            names.into_iter().map(Some).collect()
        } else {
            vec![None; urls.len()]
        };
        let mut seen = HashSet::new();
        let links = urls
            .into_iter()
            .zip(names)
            .filter(|(url, _)| seen.insert(url.clone()))
            .collect();
        return Ok(Unwrapped {
            title,
            files: group_named(links),
        });
    }
    let Some(id) = dlc_on(html) else {
        return Err(ResolveError::failed(
            "solved without links (expired, or the page changed)",
        ));
    };
    let found = from_dlc(cx, &id).await?;
    Ok(Unwrapped {
        title: title.or(found.title),
        files: found.files,
    })
}

/// The file name of each row of a solved page's link table, in order.
fn row_names(html: &str) -> Vec<String> {
    ROW_NAME
        .captures_iter(html)
        .filter_map(|c| page_text(&c[1]))
        .collect()
}

/// Whether a page shows the container's links rather than its gate.
fn shows_links(html: &str) -> bool {
    CNL.is_match(html) || DLC_BUTTON.is_match(html)
}

pub(super) async fn unwrap(cx: &Cx<'_>, url: &str) -> Result<Unwrapped, ResolveError> {
    if let Some(id) = dlc_id(url) {
        return from_dlc(cx, &id).await;
    }
    let Some(id) = container_id(url) else {
        return Err(ResolveError::NeedsUser(
            "filecrypt.cc needs a browser for this link: open it, then add the .dlc link the page offers"
                .into(),
        ));
    };
    let base = &cx.cfg.endpoints.filecrypt;
    let page = format!("{base}/Container/{id}.html");
    let _one_at_a_time = SOLVING.lock().await;
    let r = cx.http.client.get(&page).send().await?;
    if let Some(e) = refusal(&r, "its page") {
        return Err(e);
    }
    let html = r.text().await?;
    // A session that passed this container's gate before is shown its links.
    if shows_links(&html) {
        return solved(cx, &html).await;
    }
    if PASSWORD.is_match(&COMMENT.replace_all(&html, "")) {
        return Err(ResolveError::NeedsUser(
            "filecrypt.cc asks for this container's password; open the page".into(),
        ));
    }
    let Some(widget) = WIDGET.find(&html) else {
        return Err(ResolveError::NeedsUser(
            "filecrypt.cc answered without its checkbox; open the page".into(),
        ));
    };
    let data: Vec<(String, String)> = DATA
        .captures_iter(widget.as_str())
        .map(|c| (c[1].to_ascii_lowercase(), c[2].replace("&amp;", "&")))
        .collect();
    let attr = |name: &str| {
        data.iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    };
    let resolve = |link: &str| url::Url::parse(&page).ok()?.join(link.trim()).ok();
    let Some(session) = attr("session").and_then(resolve) else {
        return Err(ResolveError::NeedsUser(
            "filecrypt.cc's checkbox says nowhere to ask; open the page".into(),
        ));
    };

    // What the widget sends for its challenge: an id one of the probe
    // endpoints hands out for a nonce (none when none answers, as on the
    // page), the system's time zone, and that this is no app's web view.
    let nonce = probe_nonce();
    let mut probe_id = String::new();
    let probes = attr("px").unwrap_or_default().split(',');
    for mut probe in probes.filter(|p| !p.trim().is_empty()).filter_map(resolve) {
        probe.query_pairs_mut().append_pair("t", &nonce);
        let answer = cx
            .http
            .client
            .get(probe)
            .header(ORIGIN, base.as_str())
            .header(REFERER, format!("{base}/"))
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await;
        let id = match answer {
            Ok(r) if r.status().is_success() => r.json::<serde_json::Value>().await.ok(),
            _ => None,
        };
        if let Some(id) = id
            .as_ref()
            .and_then(|v| v["cid"].as_str())
            .filter(|id| !id.is_empty())
        {
            probe_id = id.to_string();
            break;
        }
    }
    let tz = iana_time_zone::get_timezone().unwrap_or_else(|_| "UTC".into());
    let r = cx
        .http
        .client
        .post(session)
        .header(ORIGIN, base.as_str())
        .header(REFERER, &page)
        .form(&[
            ("pow_x", CHECKS),
            ("pow_y", probe_id.as_str()),
            ("pow_yn", nonce.as_str()),
            ("tz", tz.as_str()),
            ("wt", "0"),
        ])
        .send()
        .await?;
    if let Some(e) = refusal(&r, "for its challenge") {
        return Err(e);
    }
    let answer: serde_json::Value = match r.json().await {
        Ok(answer) => answer,
        Err(e) if e.is_decode() => serde_json::Value::Null,
        Err(e) => return Err(e.into()),
    };
    let challenge = &answer["challenge"];
    let (Some(challenge_id), Some(challenge_hex)) =
        (challenge["id"].as_str(), challenge["challenge"].as_str())
    else {
        return Err(ResolveError::NeedsUser(
            "filecrypt.cc answered without its challenge; open the page".into(),
        ));
    };
    // The page reads it as `parseInt`: a number, or one in a string.
    let difficulty = challenge["difficulty"]
        .as_u64()
        .or_else(|| challenge["difficulty"].as_str()?.trim().parse().ok())
        .and_then(|d| u32::try_from(d).ok())
        .filter(|d| *d <= 160)
        .ok_or_else(|| {
            ResolveError::NeedsUser(
                "filecrypt.cc asks for a proof of work SHA-1 cannot give; open the page".into(),
            )
        })?;
    let found = {
        let challenge = challenge_hex.to_string();
        let stop = Arc::new(AtomicBool::new(false));
        // Dropping this resolve (the download paused or removed) stops the
        // search as well; the host decides how long it may be.
        let _stop = StopOnDrop(stop.clone());
        // CPU work with no await in it: keep it off the reactor.
        tokio::task::spawn_blocking(move || work(&challenge, difficulty, &stop))
            .await
            .map_err(|e| ResolveError::failed(e.to_string()))?
    };
    let Some(found) = found else {
        return Err(ResolveError::failed("the proof of work was stopped"));
    };
    let (nonce, elapsed) = (
        found.to_string(),
        ((found + 1) / BROWSER_HASHES_PER_MS).max(1).to_string(),
    );
    let report = visit(&cx.http.user_agent, &tz, &page);
    let r = cx
        .http
        .client
        .post(&page)
        .header(ORIGIN, base.as_str())
        .header(REFERER, &page)
        .form(&[
            ("pow_id", challenge_id),
            ("pow_nonce", nonce.as_str()),
            ("pow_elapsed", elapsed.as_str()),
            ("pow_pauses", "0"),
            ("pow_data", report.as_str()),
            ("pow_x", CHECKS),
        ])
        .send()
        .await?;
    if let Some(e) = refusal(&r, "its form") {
        return Err(e);
    }
    let html = r.text().await?;
    if shows_links(&html) {
        return solved(cx, &html).await;
    }
    if WIDGET.is_match(&html) {
        return Err(ResolveError::NeedsUser(
            "filecrypt.cc turned Haul's answer to its checkbox away; open the page".into(),
        ));
    }
    Err(ResolveError::failed(
        "solved without links (expired, or the page changed)",
    ))
}

/// The container a solved page's `.dlc` id names, unpacked through
/// JDownloader's key service. The `.dlc` link is not gated.
async fn from_dlc(cx: &Cx<'_>, id: &str) -> Result<Unwrapped, ResolveError> {
    let r = cx
        .http
        .client
        .get(format!("{}/DLC/{id}.dlc", cx.cfg.endpoints.filecrypt))
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
            "filecrypt.cc answered HTTP {status} on its .dlc link; solve its gate in a browser"
        )));
    }
    if content_type(r.headers()).contains("text/html") {
        return Err(ResolveError::NeedsUser(
            "filecrypt.cc answered its .dlc link with a web page; solve its gate in a browser"
                .into(),
        ));
    }
    let bytes = r.bytes().await?;
    if bytes.trim_ascii().is_empty() {
        // What filecrypt.cc sends for an id its page has not handed out.
        return Err(ResolveError::failed(
            "sent an empty .dlc: use the .dlc link its solved page offers",
        ));
    }
    let links = container::parse_dlc(
        &cx.key_service.client,
        &cx.cfg.endpoints.dlc_service,
        &bytes,
    )
    .await
    .map_err(ResolveError::failed)?;
    if links.is_empty() {
        return Err(ResolveError::failed("the .dlc held no links"));
    }
    // The container's package names what it holds.
    let title = links
        .iter()
        .map(|link| link.package.trim())
        .find(|package| !package.is_empty())
        .map(str::to_string);
    let files = links
        .into_iter()
        .map(|link| Entry {
            name: link.name.or_else(|| guess_name(&link.url)),
            package: (!link.package.is_empty()).then_some(link.package),
            url: link.url,
            deferred: true,
            ..Entry::default()
        })
        .collect();
    Ok(Unwrapped { title, files })
}

/// The report back from the form's encoding, as filecrypt.cc reads it.
#[cfg(test)]
pub(super) fn decode_report(form: &str) -> serde_json::Value {
    use base64::Engine as _;
    let encoded: String = form
        .strip_prefix("signal_")
        .unwrap()
        .chars()
        .rev()
        .map(|c| match c {
            '@' => 'a',
            '.' => 'e',
            c => c,
        })
        .collect();
    let json = base64::engine::general_purpose::STANDARD_NO_PAD
        .decode(encoded)
        .unwrap();
    serde_json::from_slice(&json).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_solved_page_names_its_rows() {
        let row = |name: &str, link: &str| {
            format!(
                r#"<tr class="kwj3"><td class="status"><i class="online small"></i></td><td title="{name}">{name}<span><a href="http://vikingfile.com" class="external_link">vikingfile.com</a></span></td><td>2.00 GB</td><td><a href="/Link/{link}.html" class="button download"></a></td></tr>"#
            )
        };
        let html = format!(
            "<table><tr><th>Status</th><th>Name</th></tr>{}{}</table>",
            row("R.part01.rar", "998E063144"),
            row("R &amp; Co.part02.rar", "11AA")
        );
        assert_eq!(row_names(&html), ["R.part01.rar", "R & Co.part02.rar"]);
    }

    #[test]
    fn proof_of_work_is_the_workers() {
        let go = AtomicBool::new(false);
        let challenge = "f8eab71c2805dda97141a78d6faeb075";
        // Found by the page's own worker for this challenge at difficulty 20.
        assert_eq!(work(challenge, 20, &go), Some(448_581));
        let mut hash = Sha1::new();
        hash.update(format!("{challenge}:448581"));
        assert!(zero_bits(&hash.finalize()) >= 20);
        assert_eq!(work(challenge, 0, &go), Some(0));
        let stop = AtomicBool::new(true);
        assert_eq!(
            work(challenge, 160, &stop),
            None,
            "a stopped search gives up"
        );
        let mut digits = [0; 20];
        assert_eq!(decimal(0, &mut digits), b"0");
        assert_eq!(decimal(u64::MAX, &mut digits), b"18446744073709551615");
        assert_eq!(zero_bits(&[0, 0x10, 0xff]), 11);
        assert_eq!(zero_bits(&[0; 4]), 32);
    }

    #[test]
    fn the_report_is_what_the_widget_sends() {
        // As the page's own encoding writes them.
        assert_eq!(
            signals(r#"{"x":1,"ua":"Haul/1.0 é"}"#),
            "signal_0nIpOMIw4SMvwWdhhkI6ISY1JCLxojI4Jy."
        );
        assert_eq!(signals("{}"), "signal_03.");
        assert_eq!(CHECKS.rsplit('.').next(), Some("W10="), "base64 of []");

        // The fields, named and ordered as the page's script writes them.
        let report = Report {
            x: 1,
            y: 2,
            click_x: 3,
            click_y: 4,
            moves: 5,
            is_touch: false,
            worker: Worker {
                webgl_vendor: "NA",
                webgl_renderer: "NA",
                user_agent: "u",
                languages: &["en"],
                platform: "p",
                hardware_concurrency: 8,
                cdp_check1: false,
                is_same_as_main_js_context: true,
                webdriver: false,
                canvas_fp: "NA",
                web_gl_fp: "NA",
                web_gl_params_fp: "NA",
            },
            ui: true,
            ua: Agent {
                user_agent: "u",
                platform: "p",
                languages: &["en"],
                hardware_concurrency: 8,
            },
            audio: "NA",
            fonts: "NA",
            webgpu: "NA",
            webgpu_limits: "NA",
            pframes: 0,
            pframes_src: "s",
            pframes_func: "",
            is_adguard: 0,
            usa: [1, 1],
            tz: "UTC",
            ta: 1.5,
            tb: 2.5,
            sw: 0,
            sh: 0,
            ow: 0,
            oh: 0,
            iw: 0,
            ih: 0,
            cd: 0,
            aw: 0,
            ae: 0,
            dm: 0,
            cg: "0",
            pt: "0",
            ho: "0",
        };
        assert_eq!(
            serde_json::to_string(&report).unwrap(),
            concat!(
                r#"{"x":1,"y":2,"click_x":3,"click_y":4,"moves":5,"is_touch":false,"#,
                r#""workerData":{"webGLVendor":"NA","webGLRenderer":"NA","userAgent":"u","languages":["en"],"platform":"p","hardwareConcurrency":8,"#,
                r#""cdpCheck1":false,"isSameAsMainJsContext":true,"webdriver":false,"canvasFp":"NA","webGlFp":"NA","webGlParamsFp":"NA"},"#,
                r#""ui":true,"ua":{"userAgent":"u","platform":"p","languages":["en"],"hardwareConcurrency":8},"#,
                r#""audioFp":"NA","fonts":"NA","webGpu":"NA","webGpuLimits":"NA","pframes":0,"pframes_src":"s","pframes_func":"","is_adguard":0,"#,
                r#""usa":[1,1],"tz":"UTC","ta":1.5,"tb":2.5,"sw":0,"sh":0,"ow":0,"oh":0,"iw":0,"ih":0,"cd":0,"aw":0,"ae":0,"dm":0,"#,
                r#""cg":"0","pt":"0","ho":"0"}"#
            )
        );

        // A visit: the pointer moves, then clicks the box, from a pointer.
        let page = "https://filecrypt.cc/Container/ABC.html";
        let form = visit("Haul/1.0", "Europe/Rome", page);
        assert!(!form["signal_".len()..].contains(['a', 'e', '=']), "{form}");
        let report = decode_report(&form);
        assert!((25..=60).contains(&report["moves"].as_u64().unwrap()));
        assert!(report["ta"].as_f64().unwrap() < report["tb"].as_f64().unwrap());
        assert_eq!(report["ui"], true);
        assert_eq!(report["workerData"]["userAgent"], "Haul/1.0");
        assert_eq!(report["workerData"]["webdriver"], false);
        assert_eq!(report["ua"]["platform"], report["workerData"]["platform"]);
        assert_eq!(report["tz"], "Europe/Rome");
        assert_eq!(report["pframes_src"], page);
    }

    #[test]
    fn pages_say_what_they_hold() {
        assert_eq!(
            key("https://filecrypt.cc/Container/0123ABCDEF.html").as_deref(),
            Some("filecrypt.cc/container/0123ABCDEF")
        );
        assert_eq!(
            key("https://www.filecrypt.cc/DLC/FEDCBA9876.dlc").as_deref(),
            Some("filecrypt.cc/dlc/FEDCBA9876")
        );
        assert_eq!(key("https://filecrypt.cc/Link/abc.html"), None);
        // The `.dlc` button's attribute changes name from page to page.
        let button = r#"<button class="dlcdownload" onclick="DownloadDLC(this.getAttribute('data-Oft'));" data-Oft="92BE2E1700">"#;
        assert_eq!(dlc_on(button).as_deref(), Some("92BE2E1700"));
        assert!(shows_links(button));
        assert_eq!(dlc_on("<button>"), None);
        // A password field left in a comment asks for nothing.
        let commented = r#"<!-- <input type="password" name="password" id="p4assw0rt"> -->"#;
        assert!(!PASSWORD.is_match(&COMMENT.replace_all(commented, "")));
        assert!(PASSWORD.is_match(r#"<input type="password" name="password">"#));
        let nonce = probe_nonce();
        assert!(
            nonce.len() >= 2
                && nonce
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() && !b.is_ascii_uppercase()),
            "{nonce}"
        );
    }
}
