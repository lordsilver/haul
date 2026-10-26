//! Hosts' pages passed in a full browser Haul drives over DevTools: on
//! Windows, the Edge that comes with it (or Chrome). Cloudflare Turnstile
//! asks for its box in an embedded WebView2 every time, and passes by itself
//! in Edge, Chrome and WebKitGTK alike (docs/decisions.md has the
//! measurements), so Windows passes such pages here; Linux keeps its webview.
//!
//! The browser runs off screen with a profile of its own, one per site, kept.
//! Haul only adds its two page scripts (`overlays.js`, `handoff.js`), reads
//! what the hand-off leaves in the page and closes the tabs ads open; a page
//! that still wants a person after its time ends the pass, and the person
//! opens it in Haul's own window. It never turns on the DevTools domains
//! automation is told apart by (`Runtime.enable`).

use std::{
    net::{SocketAddr, TcpStream},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use haul_core::hosts;
use serde_json::{Value, json};
use tungstenite::{Message, WebSocket, stream::MaybeTlsStream};

/// The browser to pass pages in: `HAUL_CHROMIUM` when set (any platform, for
/// trying it out), else on Windows the Edge that comes with it, or Chrome.
pub fn find() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("HAUL_CHROMIUM").map(PathBuf::from) {
        return path.is_file().then_some(path);
    }
    if !cfg!(windows) {
        return None;
    }
    let roots = ["ProgramFiles(x86)", "ProgramFiles", "LOCALAPPDATA"];
    let names = [
        r"Microsoft\Edge\Application\msedge.exe",
        r"Google\Chrome\Application\chrome.exe",
    ];
    names
        .iter()
        .flat_map(|name| {
            roots
                .iter()
                .filter_map(move |root| std::env::var_os(root).map(|r| PathBuf::from(r).join(name)))
        })
        .find(|p| p.is_file())
}

/// Lets the app stop a page being passed, and, in debug mode, see and ask
/// what it shows.
#[derive(Clone, Default)]
pub struct Handle {
    pub stop: Arc<AtomicBool>,
    /// Stopped because its row went or stopped waiting (removed, paused):
    /// nobody is asked about it.
    pub abandoned: Arc<AtomicBool>,
    /// What the pass is about, for debug mode: its page, profile, browser
    /// process and the page's last message to Haul.
    pub info: Arc<Mutex<Value>>,
    /// Questions about the page for the pass to answer between its looks
    /// ([`Handle::ask`]).
    asks: Arc<Mutex<Vec<Ask>>>,
}

/// A question about the page, or a request of it.
struct Ask {
    what: PageAsk,
    reply: std::sync::mpsc::Sender<Result<Value, String>>,
}

/// What debug mode asks of a page being passed.
pub enum PageAsk {
    /// What it shows ([`LOOK`]), with a screenshot.
    Look,
    /// What a script run in it answers.
    Eval(String),
    /// A click, as a person's mouse makes it, in the middle of the first
    /// element a CSS selector finds.
    Click(String),
    /// The browser's cookies for the page, those its scripts cannot read
    /// included, and its user agent.
    Cookies,
}

impl Handle {
    /// Asks the pass ([`PageAsk`]), which answers between its looks at the
    /// page, within a second or so.
    pub fn ask(&self, what: PageAsk) -> Result<Value, String> {
        let (reply, answer) = std::sync::mpsc::channel();
        self.asks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(Ask { what, reply });
        answer
            .recv_timeout(Duration::from_secs(20))
            .map_err(|_| "the pass did not answer (it may have ended)".to_string())?
    }

    fn note(&self, key: &str, value: Value) {
        if let Value::Object(info) = &mut *self.info.lock().unwrap_or_else(|e| e.into_inner()) {
            info.insert(key.into(), value);
        }
    }
}

/// One file the page handed back, with the browser's cookies for its link.
#[derive(Debug)]
pub struct File {
    pub url: String,
    pub name: Option<String>,
    pub size: Option<u64>,
    pub cookie: Option<String>,
}

#[derive(Debug)]
pub enum Outcome {
    Files {
        files: Vec<File>,
        agent: Option<String>,
    },
    /// The page said its file is gone.
    Gone(String),
    /// The page could not hand out its link this time.
    Later(String),
    /// The browser is past the check in front of the page's site and got
    /// these cookies for it (`Set-Cookie` lines): Haul reads the page itself
    /// with them ([`Pass::hand_clearance`]).
    Cleared(Vec<String>),
    /// The page showed links Haul took ([`Pass::take_links`]).
    Listed,
    /// The page still wants someone after its time (its check asks for a
    /// box to tick): the pass ends, and the person opens the page in Haul.
    WantsPerson(Look),
    /// The page passed every check and showed nothing Haul took
    /// ([`Pass::ends_settled`]).
    Settled,
    /// The browser went away, or the pass was stopped.
    Closed,
}

pub struct Pass<'a> {
    pub exe: &'a Path,
    pub url: &'a str,
    /// The site's own folder, kept for its next pages.
    pub profile: PathBuf,
    /// Haul's local bridge to the proxy, for a proxied host.
    pub proxy: Option<SocketAddr>,
    /// Run in every page before its own scripts.
    pub scripts: &'a [&'a str],
    /// How long the page gets before it is taken to want a person.
    pub show_after: Duration,
    pub handle: Handle,
    /// For a page that lists links: offered every address the page shows,
    /// and those written out in its text, as more appear; true once it took
    /// some, which ends the pass.
    pub take_links: Option<TakeLinks<'a>>,
    /// The pass ends as soon as the page settles past its checks without
    /// links Haul takes: on a paste, nothing more is coming.
    pub ends_settled: bool,
    /// The page is kept on the site it lands on ([`Cdp::keep_to_site`]): a
    /// page that is no host's own (a wrapper's, one behind a check), whose
    /// files are read off it rather than reached by going elsewhere.
    pub keep_to_site: bool,
    /// The pass ends as soon as the browser is past Cloudflare's check in
    /// front of the page's site, with the clearance it got
    /// ([`Outcome::Cleared`]): for a page Haul reads itself once it may.
    pub hand_clearance: bool,
}

/// Offered a page's addresses; true once it took some.
pub type TakeLinks<'a> = &'a dyn Fn(&Shown) -> bool;

/// What a page that lists links shows: every address on it, those written
/// out in its text, and its title.
#[derive(Debug, Default)]
pub struct Shown {
    pub links: Vec<String>,
    pub text: Vec<String>,
    pub title: Option<String>,
}

/// Passes the page; blocks until it hands something back, says its file is
/// gone, wants a person, or is stopped. The browser never comes on screen or
/// onto the taskbar. Its profile (`p.profile`, one per site) is kept, so a
/// check passed once (Cloudflare's clearance cookie) holds for the site's
/// next pages.
pub fn pass(p: Pass<'_>) -> Result<Outcome, String> {
    std::fs::create_dir_all(&p.profile).map_err(|e| format!("{}: {e}", p.profile.display()))?;
    // A port of our own choosing: with `--remote-debugging-port=0` (the
    // browser picks one) Turnstile asked for its box every time, with a
    // port named it passed (docs/decisions.md).
    let port = std::net::TcpListener::bind(("127.0.0.1", 0))
        .and_then(|l| l.local_addr())
        .map_err(|e| format!("a port for DevTools: {e}"))?
        .port();
    let mut args = vec![
        format!("--remote-debugging-port={port}"),
        format!("--user-data-dir={}", p.profile.display()),
        "--no-first-run".into(),
        "--no-default-browser-check".into(),
        "--window-position=-20000,-20000".into(),
        // Off screen, Windows counts the window as covered and Chrome hides
        // its page (`visibilityState` "hidden"): it stops drawing, mouse
        // input never reaches it, and checks see a page no person looks
        // at. These keep it a visible page.
        "--disable-features=CalculateNativeWinOcclusion".into(),
        "--disable-backgrounding-occluded-windows".into(),
        "--window-size=1100,800".into(),
    ];
    if let Some(addr) = p.proxy {
        args.push(format!("--proxy-server=socks5://{addr}"));
        // WebRTC could otherwise reveal the real address to the page.
        args.push("--force-webrtc-ip-handling-policy=disable_non_proxied_udp".into());
    }
    args.push("about:blank".into());
    let child = Command::new(p.exe)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("{}: {e}", p.exe.display()))?;
    let mut browser = Browser { child };
    *p.handle.info.lock().unwrap_or_else(|e| e.into_inner()) = json!({
        "url": p.url,
        "profile": p.profile.display().to_string(),
        "pid": browser.child.id(),
        "started": haul_core::debug::now(),
        "show_after_secs": p.show_after.as_secs(),
    });
    haul_core::debug::emit(
        "pass",
        json!({ "event": "start", "url": p.url, "profile": p.profile.display().to_string(), "pid": browser.child.id() }),
    );
    // Its windows stay off the taskbar from the first moment, not only once
    // the page is driven.
    let hiding = Arc::new(AtomicBool::new(true));
    {
        let (hiding, pid) = (hiding.clone(), browser.child.id());
        std::thread::spawn(move || {
            while hiding.load(Ordering::Relaxed) {
                taskbar::hide(pid);
                std::thread::sleep(Duration::from_millis(50));
            }
        });
    }
    let result = drive(&mut browser, port, &p);
    hiding.store(false, Ordering::Relaxed);
    drop(browser);
    if haul_core::debug::enabled() {
        // The outcome without the cookies a file carries.
        let outcome = match &result {
            Ok(Outcome::Files { files, .. }) => {
                json!({ "files": files.iter().map(|f| &f.url).collect::<Vec<_>>() })
            }
            Ok(Outcome::Gone(why)) => json!({ "gone": why }),
            Ok(Outcome::Later(why)) => json!({ "later": why }),
            // Their names only, not what they hold.
            Ok(Outcome::Cleared(cookies)) => json!({
                "cleared": cookies.iter().filter_map(|c| c.split('=').next()).collect::<Vec<_>>()
            }),
            Ok(Outcome::Listed) => json!("listed"),
            Ok(Outcome::WantsPerson(look)) => {
                json!({ "wants_person": serde_json::from_str::<Value>(&look.page).unwrap_or(Value::Null) })
            }
            Ok(Outcome::Settled) => json!("settled"),
            Ok(Outcome::Closed) => json!("closed"),
            Err(e) => json!({ "error": e }),
        };
        haul_core::debug::emit(
            "pass",
            json!({ "event": "end", "url": p.url, "outcome": outcome, "started": p.handle.info.lock().unwrap_or_else(|e| e.into_inner()).get("started").cloned() }),
        );
    }
    result
}

/// The browser process, ended when dropped.
struct Browser {
    child: Child,
}

impl Drop for Browser {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn drive(browser: &mut Browser, port: u16, p: &Pass<'_>) -> Result<Outcome, String> {
    let endpoint = wait_for_endpoint(browser, port)?;
    let mut cdp = Cdp::connect(&endpoint)?;
    let targets = cdp.call("Target.getTargets", json!({}), None)?;
    let target = pages(&targets)
        .first()
        .cloned()
        .ok_or("the browser opened no page")?;
    let attached = cdp.call(
        "Target.attachToTarget",
        json!({ "targetId": target, "flatten": true }),
        None,
    )?;
    let session = attached["sessionId"]
        .as_str()
        .ok_or("no DevTools session")?
        .to_string();
    let s = Some(session.as_str());
    // Scripts for new documents run only with the page domain on (unlike
    // the runtime domain, it tells no automation apart).
    cdp.call("Page.enable", json!({}), s)?;
    cdp.call(
        "Page.addScriptToEvaluateOnNewDocument",
        json!({ "source": "window.__haulDevTools = true;" }),
        s,
    )?;
    for script in p.scripts {
        cdp.call(
            "Page.addScriptToEvaluateOnNewDocument",
            json!({ "source": script }),
            s,
        )?;
    }
    if p.keep_to_site {
        cdp.keep_to_site(&target, s)?;
    }
    cdp.call("Page.navigate", json!({ "url": p.url }), s)?;
    let start = Instant::now();
    // The addresses the page last offered: a new page with as many (a
    // paste's list after its gate) is offered too.
    let mut offered: Vec<String> = Vec::new();
    loop {
        if p.handle.stop.load(Ordering::Relaxed)
            || browser.child.try_wait().ok().flatten().is_some()
        {
            return Ok(Outcome::Closed);
        }
        // Waiting, but answering the page's paused navigations meanwhile.
        cdp.pump(Duration::from_millis(500));
        // Tabs an ad opened go; without the page's own, the window is closed.
        let Ok(targets) = cdp.call("Target.getTargets", json!({}), None) else {
            return Ok(Outcome::Closed);
        };
        let open = pages(&targets);
        if !open.contains(&target) {
            return Ok(Outcome::Closed);
        }
        for other in open.iter().filter(|t| **t != target) {
            let _ = cdp.call("Target.closeTarget", json!({ "targetId": other }), None);
        }
        // Debug mode's questions about the page, between looks.
        let asks: Vec<Ask> =
            std::mem::take(&mut *p.handle.asks.lock().unwrap_or_else(|e| e.into_inner()));
        for ask in asks {
            let answer = match ask.what {
                PageAsk::Click(selector) => click(&mut cdp, s, &selector),
                PageAsk::Cookies => cdp.call("Network.getCookies", json!({}), s).and_then(|c| {
                    let agent = cdp.call(
                        "Runtime.evaluate",
                        json!({ "expression": "navigator.userAgent", "returnByValue": true }),
                        s,
                    )?;
                    Ok(json!({ "cookies": c["cookies"], "user_agent": agent["result"]["value"] }))
                }),
                PageAsk::Look => {
                    use base64::Engine as _;
                    let seen = look(&mut cdp, s);
                    Ok(json!({
                        "page": serde_json::from_str::<Value>(&seen.page).unwrap_or(Value::String(seen.page)),
                        "screenshot_png_base64": seen.shot.map(|png| base64::engine::general_purpose::STANDARD.encode(png)),
                    }))
                }
                PageAsk::Eval(script) => cdp
                    .call(
                        "Runtime.evaluate",
                        json!({ "expression": script, "returnByValue": true, "awaitPromise": true }),
                        s,
                    )
                    .map(|a| a["result"]["value"].clone()),
            };
            let _ = ask.reply.send(answer);
        }
        if p.hand_clearance
            && let Some(cookies) = clearance(&mut cdp, p.url, s)
        {
            return Ok(Outcome::Cleared(cookies));
        }
        if start.elapsed() >= p.show_after {
            return Ok(Outcome::WantsPerson(look(&mut cdp, s)));
        }
        let answer = match cdp.call(
            "Runtime.evaluate",
            json!({ "expression": "JSON.stringify(window.__haulHandoff || null)", "returnByValue": true }),
            s,
        ) {
            Ok(answer) => answer,
            // Between pages, or busy: ask again.
            Err(_) => continue,
        };
        let Some(sent) = answer["result"]["value"]
            .as_str()
            .and_then(|v| serde_json::from_str::<Value>(v).ok())
            .filter(|v| !v.is_null())
        else {
            continue;
        };
        // Each new message from the page, for debug mode.
        if haul_core::debug::enabled()
            && p.handle.info.lock().unwrap_or_else(|e| e.into_inner())["page_said"] != sent
        {
            p.handle.note("page_said", sent.clone());
            haul_core::debug::emit(
                "pass",
                json!({ "event": "page", "url": p.url, "said": sent }),
            );
        }
        if let Some(reason) = sent["gone"].as_str() {
            return Ok(Outcome::Gone(reason.to_string()));
        }
        if let Some(reason) = sent["later"].as_str() {
            return Ok(Outcome::Later(reason.to_string()));
        }
        if let (Some(take), Some(_)) = (p.take_links, sent["links"].as_array()) {
            let strings = |key: &str| -> Vec<String> {
                sent[key]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|l| l.as_str().map(str::to_string))
                    .collect()
            };
            let links = strings("links");
            if links != offered {
                offered.clone_from(&links);
                let shown = Shown {
                    links,
                    text: strings("shown"),
                    title: sent["title"].as_str().map(str::to_string),
                };
                if take(&shown) {
                    return Ok(Outcome::Listed);
                }
            }
            if p.ends_settled && sent["settled"].as_bool() == Some(true) {
                return Ok(Outcome::Settled);
            }
            continue;
        }
        let mut files = Vec::new();
        for f in sent["files"].as_array().into_iter().flatten() {
            let Some(url) = f["url"].as_str().filter(|u| u.starts_with("http")) else {
                continue;
            };
            files.push(File {
                url: url.to_string(),
                name: f["name"]
                    .as_str()
                    .filter(|n| !n.is_empty())
                    .map(str::to_string),
                size: f["size"].as_u64(),
                cookie: cookies_for(&mut cdp, url, s),
            });
        }
        if !files.is_empty() {
            let agent = sent["agent"].as_str().map(str::to_string);
            return Ok(Outcome::Files { files, agent });
        }
    }
}

/// The browser's DevTools WebSocket, from its `/json/version` once it
/// listens. (`DevToolsActivePort` in the profile is written only when the
/// browser picks the port itself.)
fn wait_for_endpoint(browser: &mut Browser, port: u16) -> Result<String, String> {
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if let Some(status) = browser.child.try_wait().ok().flatten() {
            return Err(format!("the browser exited at start ({status})"));
        }
        if let Some(url) = version(port) {
            return Ok(url);
        }
        std::thread::sleep(Duration::from_millis(150));
    }
    Err("the browser did not open DevTools in time".into())
}

/// `webSocketDebuggerUrl` from `GET /json/version`, in plain HTTP/1.1.
fn version(port: u16) -> Option<String> {
    use std::io::{Read, Write};
    let mut stream = TcpStream::connect(("127.0.0.1", port)).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
    write!(
        stream,
        "GET /json/version HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
    )
    .ok()?;
    // The browser keeps the connection open: read up to the body's length.
    let mut answer = Vec::new();
    let mut chunk = [0u8; 4096];
    let body = loop {
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            break String::from_utf8_lossy(&answer)
                .split_once("\r\n\r\n")?
                .1
                .to_string();
        }
        answer.extend_from_slice(&chunk[..n]);
        let text = String::from_utf8_lossy(&answer);
        if let Some((head, body)) = text.split_once("\r\n\r\n") {
            let length = head
                .lines()
                .find_map(|l| {
                    l.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(|v| v.trim().parse::<usize>().ok())
                })
                .flatten();
            if length.is_some_and(|len| body.len() >= len) {
                break body.to_string();
            }
        }
    };
    let json: Value = serde_json::from_str(body.trim()).ok()?;
    json["webSocketDebuggerUrl"].as_str().map(str::to_string)
}

/// The ids of the browser's pages (tabs), in its order.
fn pages(targets: &Value) -> Vec<String> {
    targets["targetInfos"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|t| t["type"] == "page")
        .filter_map(|t| t["targetId"].as_str().map(str::to_string))
        .collect()
}

/// The browser's cookies for `url`, as a `Cookie` header.
fn cookies_for(cdp: &mut Cdp, url: &str, session: Option<&str>) -> Option<String> {
    let got = cdp
        .call("Network.getCookies", json!({ "urls": [url] }), session)
        .ok()?;
    let pairs: Vec<String> = got["cookies"]
        .as_array()?
        .iter()
        .filter_map(|c| Some(format!("{}={}", c["name"].as_str()?, c["value"].as_str()?)))
        .collect();
    (!pairs.is_empty()).then(|| pairs.join("; "))
}

/// Whether the page is on its site past Cloudflare's check: loaded, and
/// not the check's own page, which answers 403 (or 503) and reloads the
/// page once it passes. The check's scripts and title tell nothing: the
/// page past it may keep `_cf_chl_opt` (filecrypt.cc's does), and the
/// title is in the browser's language.
const PAST_CHECK: &str = r#"(() => {
  if (!location.protocol.startsWith('http') || document.readyState === 'loading') return false;
  const status = performance.getEntriesByType('navigation')[0]?.responseStatus;
  return status ? status < 400 : !/^just a moment/i.test(document.title);
})()"#;

/// Once the page is past the check in front of its site, the cookies
/// Cloudflare set for it (its clearance among them), as `Set-Cookie` lines;
/// `None` while the check is still on.
fn clearance(cdp: &mut Cdp, url: &str, session: Option<&str>) -> Option<Vec<String>> {
    let got = cdp
        .call("Network.getCookies", json!({ "urls": [url] }), session)
        .ok()?;
    let theirs: Vec<&Value> = got["cookies"]
        .as_array()?
        .iter()
        .filter(|c| {
            c["name"]
                .as_str()
                .is_some_and(|n| n.starts_with("cf_") || n.starts_with("__cf"))
        })
        .collect();
    if !theirs.iter().any(|c| c["name"] == "cf_clearance") {
        return None;
    }
    let past = cdp
        .call(
            "Runtime.evaluate",
            json!({ "expression": PAST_CHECK, "returnByValue": true }),
            session,
        )
        .ok()?;
    if past["result"]["value"].as_bool() != Some(true) {
        return None;
    }
    let lines = theirs
        .iter()
        .filter_map(|c| {
            let mut line = format!(
                "{}={}; Domain={}; Path={}",
                c["name"].as_str()?,
                c["value"].as_str()?,
                c["domain"].as_str()?,
                c["path"].as_str().unwrap_or("/"),
            );
            if c["secure"].as_bool() == Some(true) {
                line.push_str("; Secure");
            }
            if c["httpOnly"].as_bool() == Some(true) {
                line.push_str("; HttpOnly");
            }
            Some(line)
        })
        .collect();
    Some(lines)
}

/// A DevTools connection to the browser.
struct Cdp {
    ws: WebSocket<MaybeTlsStream<TcpStream>>,
    next: u64,
    /// Keeps the page on its site ([`Cdp::keep_to_site`]).
    keep: Option<Keep>,
}

/// A page kept on the site it landed on: its main frame is refused any
/// other site once its first page has loaded there.
struct Keep {
    /// The page's main frame (the target's id).
    frame: String,
    /// The site of the last page the main frame was let go to.
    last: Option<String>,
    /// Where its first page loaded: set at the first load.
    site: Option<String>,
}

impl Cdp {
    fn connect(endpoint: &str) -> Result<Self, String> {
        let (ws, _) = tungstenite::connect(endpoint).map_err(|e| format!("DevTools: {e}"))?;
        if let MaybeTlsStream::Plain(stream) = ws.get_ref() {
            let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
        }
        Ok(Self {
            ws,
            next: 0,
            keep: None,
        })
    }

    /// Keeps page `frame` (of `session`) on the site its first page lands
    /// on: a page that sends the whole tab to an ad's site (filecrypt.cc
    /// does, half a second after it shows) is refused that navigation, as
    /// Haul's own window refuses it. Frames inside the page (a check's) and
    /// the way to the first page go as they will.
    fn keep_to_site(&mut self, frame: &str, session: Option<&str>) -> Result<(), String> {
        self.keep = Some(Keep {
            frame: frame.to_string(),
            last: None,
            site: None,
        });
        self.call(
            "Fetch.enable",
            json!({ "patterns": [{ "resourceType": "Document", "requestStage": "Request" }] }),
            session,
        )
        .map(|_| ())
    }

    /// Answers an event that waits for one: a paused navigation, let go or
    /// refused ([`Cdp::keep_to_site`]); and notes the first page's load.
    fn on_event(&mut self, event: &Value) {
        let Some(keep) = self.keep.as_mut() else {
            return;
        };
        match event["method"].as_str() {
            Some("Page.loadEventFired") => {
                if keep.site.is_none() {
                    keep.site.clone_from(&keep.last);
                }
            }
            Some("Fetch.requestPaused") => {
                let p = &event["params"];
                let url = p["request"]["url"].as_str().unwrap_or_default();
                let main = p["frameId"].as_str() == Some(keep.frame.as_str());
                let site = url.starts_with("http").then(|| hosts::site_of(url));
                let refuse = main && site.is_some() && keep.site.is_some() && site != keep.site;
                if main && !refuse && site.is_some() {
                    keep.last = site.clone();
                }
                let (method, params) = if refuse {
                    tracing::info!(url, "kept the page on its site: refused going to {url}");
                    haul_core::debug::emit("pass", json!({ "event": "refused", "url": url }));
                    (
                        // A navigation answered with no content goes nowhere:
                        // the page stays (failing it shows an error page).
                        "Fetch.fulfillRequest",
                        json!({ "requestId": p["requestId"], "responseCode": 204 }),
                    )
                } else {
                    (
                        "Fetch.continueRequest",
                        json!({ "requestId": p["requestId"] }),
                    )
                };
                self.next += 1;
                let mut message = json!({ "id": self.next, "method": method, "params": params });
                if let Some(session) = event["sessionId"].as_str() {
                    message["sessionId"] = json!(session);
                }
                let _ = self.ws.send(Message::text(message.to_string()));
            }
            _ => {}
        }
    }

    /// Reads what comes for up to `wait`, answering events that wait.
    fn pump(&mut self, wait: Duration) {
        let until = Instant::now() + wait;
        while Instant::now() < until {
            match self.ws.read() {
                Ok(Message::Text(text)) => {
                    if let Ok(event) = serde_json::from_str::<Value>(text.as_str())
                        && event.get("method").is_some()
                    {
                        self.on_event(&event);
                    }
                }
                Ok(_) => {}
                Err(tungstenite::Error::Io(e))
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) => {}
                Err(_) => return,
            }
        }
    }

    /// Calls `method` (on the page when `session` names it) and waits for
    /// its answer, passing over the events that come meanwhile.
    fn call(
        &mut self,
        method: &str,
        params: Value,
        session: Option<&str>,
    ) -> Result<Value, String> {
        self.next += 1;
        let id = self.next;
        let mut message = json!({ "id": id, "method": method, "params": params });
        if let Some(session) = session {
            message["sessionId"] = json!(session);
        }
        self.ws
            .send(Message::text(message.to_string()))
            .map_err(|e| format!("DevTools: {e}"))?;
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if Instant::now() > deadline {
                return Err(format!("DevTools: no answer to {method}"));
            }
            match self.ws.read() {
                Ok(Message::Text(text)) => {
                    let Ok(answer) = serde_json::from_str::<Value>(text.as_str()) else {
                        continue;
                    };
                    if answer.get("method").is_some() {
                        self.on_event(&answer);
                        continue;
                    }
                    if answer["id"].as_u64() != Some(id) {
                        continue;
                    }
                    if let Some(error) = answer.get("error") {
                        return Err(format!("DevTools {method}: {error}"));
                    }
                    return Ok(answer["result"].clone());
                }
                Ok(_) => {}
                Err(tungstenite::Error::Io(e))
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) => {}
                Err(e) => return Err(format!("DevTools: {e}")),
            }
        }
    }
}

/// The taskbar buttons of the browser's windows. A page passed out of sight
/// shows nowhere, but each browser window still gets a button, and a queue
/// of pages had Edge buttons come and go one after another.
#[cfg(windows)]
mod taskbar {
    use windows_sys::{
        Win32::{
            Foundation::{HWND, LPARAM},
            UI::WindowsAndMessaging::{
                EnumWindows, GW_OWNER, GWL_EXSTYLE, GetWindow, GetWindowLongPtrW,
                GetWindowThreadProcessId, IsWindowVisible, SW_HIDE, SW_SHOWNOACTIVATE,
                SetWindowLongPtrW, ShowWindow, WS_EX_APPWINDOW, WS_EX_TOOLWINDOW,
            },
        },
        core::BOOL,
    };

    /// The visible top-level windows of process `pid` that own no other.
    fn windows_of(pid: u32) -> Vec<HWND> {
        struct Search {
            pid: u32,
            found: Vec<HWND>,
        }
        unsafe extern "system" fn each(hwnd: HWND, lparam: LPARAM) -> BOOL {
            // SAFETY: `lparam` is the `Search` below, alive for the call.
            let search = unsafe { &mut *(lparam as *mut Search) };
            let mut owner = 0;
            // SAFETY: plain queries on a window handle Windows gave us.
            unsafe {
                GetWindowThreadProcessId(hwnd, &mut owner);
                if owner == search.pid
                    && IsWindowVisible(hwnd) != 0
                    && GetWindow(hwnd, GW_OWNER).is_null()
                {
                    search.found.push(hwnd);
                }
            }
            1
        }
        let mut search = Search {
            pid,
            found: Vec::new(),
        };
        // SAFETY: the callback only touches `search`, which outlives the call.
        unsafe { EnumWindows(Some(each), &mut search as *mut Search as LPARAM) };
        search.found
    }

    /// Sets or clears the tool-window style, which keeps a window off the
    /// taskbar; the change shows once the window is hidden and shown again.
    fn set_tool(hwnd: HWND, tool: bool) {
        // SAFETY: reading and writing a window's extended style.
        unsafe {
            let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
            let want = if tool {
                (style | WS_EX_TOOLWINDOW) & !WS_EX_APPWINDOW
            } else {
                (style & !WS_EX_TOOLWINDOW) | WS_EX_APPWINDOW
            };
            if want == style {
                return;
            }
            ShowWindow(hwnd, SW_HIDE);
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, want as isize);
            ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        }
    }

    pub fn hide(pid: u32) {
        for hwnd in windows_of(pid) {
            set_tool(hwnd, true);
        }
    }
}

#[cfg(not(windows))]
mod taskbar {
    pub fn hide(_pid: u32) {}
}

/// What a page showed when it was given up on, for the log.
#[derive(Debug)]
pub struct Look {
    /// Its title, address, frames, Turnstile answers, buttons and first 50
    /// links, as JSON.
    pub page: String,
    /// A PNG of it, when the browser gave one.
    pub shot: Option<Vec<u8>>,
}

const LOOK: &str = r#"JSON.stringify({
  title: document.title,
  url: location.href,
  frames: [...document.querySelectorAll('iframe')].map((f) => f.src.slice(0, 100)),
  turnstile: [...document.querySelectorAll('[name="cf-turnstile-response"]')].map((i) => (i.value ? 'answered' : 'empty')),
  buttons: [...document.querySelectorAll('button, input[type=submit]')]
    .filter((b) => b.offsetParent)
    .map((b) => (b.innerText || b.value || '').trim().slice(0, 40)),
  links: [...document.querySelectorAll('a[href]')].map((a) => a.href).filter((h) => /^https?:/.test(h)).slice(0, 50),
})"#;

/// Clicks the middle of the first element `selector` finds, with the
/// mouse events a person's click makes (DevTools' input, which the page
/// cannot tell from a person's).
fn click(cdp: &mut Cdp, s: Option<&str>, selector: &str) -> Result<Value, String> {
    let find = format!(
        "(() => {{ const e = document.querySelector({}); if (!e) return null; const r = e.getBoundingClientRect(); return [r.x + r.width / 2, r.y + r.height / 2]; }})()",
        serde_json::to_string(selector).unwrap_or_default()
    );
    let at = cdp.call(
        "Runtime.evaluate",
        json!({ "expression": find, "returnByValue": true }),
        s,
    )?["result"]["value"]
        .clone();
    let (Some(x), Some(y)) = (at[0].as_f64(), at[1].as_f64()) else {
        return Err(format!("nothing on the page matches {selector}"));
    };
    for (kind, buttons) in [("mouseMoved", 0), ("mousePressed", 1), ("mouseReleased", 0)] {
        cdp.call(
            "Input.dispatchMouseEvent",
            json!({ "type": kind, "x": x, "y": y, "button": "left", "buttons": buttons, "clickCount": 1 }),
            s,
        )?;
    }
    Ok(json!({ "clicked": [x, y] }))
}

fn look(cdp: &mut Cdp, s: Option<&str>) -> Look {
    let page = cdp
        .call(
            "Runtime.evaluate",
            json!({ "expression": LOOK, "returnByValue": true }),
            s,
        )
        .ok()
        .and_then(|a| a["result"]["value"].as_str().map(str::to_string))
        .unwrap_or_else(|| "(the page did not answer)".into());
    let shot = cdp
        .call("Page.captureScreenshot", json!({ "format": "png" }), s)
        .ok()
        .and_then(|a| {
            use base64::Engine as _;
            base64::engine::general_purpose::STANDARD
                .decode(a["data"].as_str()?)
                .ok()
        });
    Look { page, shot }
}
