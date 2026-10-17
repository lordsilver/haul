//! Hosts' pages passed in a full browser Haul drives over DevTools: on
//! Windows, the Edge that comes with it (or Chrome). Cloudflare Turnstile
//! asks for its box in an embedded WebView2 every time, and passes by itself
//! in Edge, Chrome and WebKitGTK alike (docs/decisions.md has the
//! measurements), so Windows passes such pages here; Linux keeps its webview.
//!
//! The browser runs off screen with a profile of its own, deleted after.
//! Haul only adds its two page scripts (`overlays.js`, `handoff.js`), reads
//! what the hand-off leaves in the page, closes the tabs ads open, and
//! brings the window on screen when the page wants a person. It never turns
//! on the DevTools domains automation is told apart by (`Runtime.enable`).

use std::{
    net::{SocketAddr, TcpStream},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

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

/// Lets the app steer a page being passed: bring it on screen, or stop.
#[derive(Clone, Default)]
pub struct Handle {
    pub show: Arc<AtomicBool>,
    pub stop: Arc<AtomicBool>,
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
    /// The person closed the window, or the browser went away.
    Closed,
}

pub struct Pass<'a> {
    pub exe: &'a Path,
    pub url: &'a str,
    /// A folder of its own, deleted after.
    pub profile: PathBuf,
    /// Haul's local bridge to the proxy, for a proxied host.
    pub proxy: Option<SocketAddr>,
    /// Run in every page before its own scripts.
    pub scripts: &'a [&'a str],
    /// How long the page gets before it comes on screen.
    pub show_after: Duration,
    pub handle: Handle,
}

/// Passes the page; blocks until it hands something back, says its file is
/// gone, or is closed. `on_show` runs when the window comes on screen.
pub fn pass(p: Pass<'_>, on_show: impl FnOnce()) -> Result<Outcome, String> {
    let _ = std::fs::remove_dir_all(&p.profile);
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
    let mut browser = Browser {
        child,
        profile: p.profile.clone(),
    };
    let result = drive(&mut browser, &p, on_show);
    drop(browser);
    result
}

/// The browser process, ended and its profile deleted when dropped.
struct Browser {
    child: Child,
    profile: PathBuf,
}

impl Drop for Browser {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        // Its files stay locked a moment after it exits.
        for _ in 0..20 {
            if std::fs::remove_dir_all(&self.profile).is_ok() || !self.profile.exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    }
}

fn drive(browser: &mut Browser, p: &Pass<'_>, on_show: impl FnOnce()) -> Result<Outcome, String> {
    let endpoint = wait_for_endpoint(browser)?;
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
    cdp.call("Page.navigate", json!({ "url": p.url }), s)?;
    let start = Instant::now();
    let mut on_show = Some(on_show);
    loop {
        if p.handle.stop.load(Ordering::Relaxed)
            || browser.child.try_wait().ok().flatten().is_some()
        {
            return Ok(Outcome::Closed);
        }
        std::thread::sleep(Duration::from_millis(500));
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
        let wants_person = start.elapsed() >= p.show_after || p.handle.show.load(Ordering::Relaxed);
        if wants_person && let Some(show) = on_show.take() {
            bring_on_screen(&mut cdp, &target, s);
            show();
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
        if let Some(reason) = sent["gone"].as_str() {
            return Ok(Outcome::Gone(reason.to_string()));
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

/// The DevTools address the browser writes into its profile once up.
fn wait_for_endpoint(browser: &mut Browser) -> Result<String, String> {
    let file = browser.profile.join("DevToolsActivePort");
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if let Some(status) = browser.child.try_wait().ok().flatten() {
            return Err(format!("the browser exited at start ({status})"));
        }
        if let Ok(text) = std::fs::read_to_string(&file) {
            let mut lines = text.lines();
            if let (Some(port), Some(path)) = (lines.next(), lines.next()) {
                return Ok(format!("ws://127.0.0.1:{}{}", port.trim(), path.trim()));
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err("the browser did not open DevTools in time".into())
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

fn bring_on_screen(cdp: &mut Cdp, target: &str, session: Option<&str>) {
    if let Ok(w) = cdp.call(
        "Browser.getWindowForTarget",
        json!({ "targetId": target }),
        None,
    ) && let Some(id) = w["windowId"].as_u64()
    {
        let _ = cdp.call(
            "Browser.setWindowBounds",
            json!({ "windowId": id, "bounds": { "left": 160, "top": 120, "width": 1100, "height": 800, "windowState": "normal" } }),
            None,
        );
    }
    let _ = cdp.call("Page.bringToFront", json!({}), session);
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

/// A DevTools connection to the browser.
struct Cdp {
    ws: WebSocket<MaybeTlsStream<TcpStream>>,
    next: u64,
}

impl Cdp {
    fn connect(endpoint: &str) -> Result<Self, String> {
        let (ws, _) = tungstenite::connect(endpoint).map_err(|e| format!("DevTools: {e}"))?;
        if let MaybeTlsStream::Plain(stream) = ws.get_ref() {
            let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
        }
        Ok(Self { ws, next: 0 })
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
