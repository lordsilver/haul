//! The in-app browser for hosts that need a person: the user solves the
//! host's page in a window of its own, and when the page starts the download,
//! the app stops it and hands the link (with the page's cookies) to the
//! engine, which downloads it with resume and the usual limits. On a page
//! whose links Haul knows where to find (`handoff.js` has a recipe per host:
//! vikingfile's button, gofile's folder listing), the window takes them as
//! soon as the page has them, without the download button.
//!
//! With a proxy configured, the window goes through it too, by way of the
//! local bridge, because browser engines cannot pass proxy credentials.
//!
//! A host's page stays on the host's site ([`Stay`]): an ad that sends it
//! elsewhere on a click is sent back, and a window it opens on its own site
//! loads in place, since the window has no tabs to open it in. A layer an
//! ad lays over the page to catch the first click is taken off it.
//!
//! Pages passed out of sight go to a full browser where there is one to
//! drive (Edge on Windows, [`chromium`]): Cloudflare Turnstile asks for its
//! box in an embedded WebView2 every time, and passes by itself in Edge.

use std::{
    collections::{BTreeMap, HashMap},
    net::SocketAddr,
    path::PathBuf,
    sync::{Arc, Mutex, MutexGuard},
    time::{Duration, Instant},
};

use haul_core::{
    Engine,
    bridge::Bridge,
    hosts,
    stay::{self, SendBack, Stay},
};
use haul_web::Desktop;
use tauri::{
    AppHandle, Manager, Url, WebviewUrl, WebviewWindow, WebviewWindowBuilder, Wry,
    webview::{DownloadEvent, NewWindowResponse, PageLoadEvent},
};

use crate::{chromium, notify};

pub struct Browser {
    app: AppHandle,
    engine: Engine,
    /// The local bridge to the proxy, started the first time a page needs it.
    bridge: Mutex<Option<Bridge>>,
    /// Pages being passed in a full browser, by item.
    passes: Arc<Mutex<HashMap<String, chromium::Handle>>>,
}

impl Browser {
    pub fn new(app: AppHandle, engine: Engine) -> Self {
        Self {
            app,
            engine,
            bridge: Mutex::new(None),
            passes: Arc::default(),
        }
    }

    /// Whether item `id`'s page is open, in a window or a full browser.
    pub fn is_passing(&self, id: &str) -> bool {
        self.app
            .get_webview_window(&format!("browse-{id}"))
            .is_some()
            || passes(&self.passes).contains_key(id)
    }

    /// The bridge's address, pointed at the proxy currently configured.
    fn bridge_to(&self, proxy: &str) -> Result<SocketAddr, String> {
        let mut slot = self.bridge.lock().unwrap_or_else(|e| e.into_inner());
        if slot.is_none() {
            *slot = Some(Bridge::start().map_err(|e| format!("proxy bridge: {e}"))?);
        }
        let bridge = slot.as_ref().expect("started above");
        bridge
            .set_upstream(Some(proxy))
            .map_err(|e| format!("proxy bridge: {e}"))?;
        Ok(bridge.addr())
    }
}

impl Browser {
    /// Opens `url`'s page for item `id` out of sight, for a host whose
    /// check passes by itself ([`hosts::page_passes`]): the link is taken as
    /// usual, and the window comes forward only if the page is still there
    /// after [`SHOW_AFTER`], its check wanting a person.
    pub fn pass(&self, id: &str, url: &str) -> Result<(), String> {
        let label = format!("browse-{id}");
        if self.is_passing(id) {
            return Ok(());
        }
        if let Some(exe) = chromium::find() {
            return self.pass_in(exe, id, url);
        }
        self.open(id, url, None, true)?;
        let app = self.app.clone();
        let host = hosts::host_of(url);
        let (engine, id) = (self.engine.clone(), id.to_string());
        std::thread::spawn(move || {
            std::thread::sleep(SHOW_AFTER);
            if let Some(window) = app.get_webview_window(&label) {
                tracing::info!("{host} wants a person for its check");
                engine.page_wants_person(&id);
                bring_forward(&window);
                crate::notify_waiting(
                    &app,
                    &format!("{host} wants you"),
                    "Its check did not pass by itself: tick the box on its page and the download starts.",
                );
            }
        });
        Ok(())
    }

    /// [`Browser::pass`] in a full browser driven over DevTools.
    fn pass_in(&self, exe: PathBuf, id: &str, url: &str) -> Result<(), String> {
        let host = hosts::host_of(url);
        let proxy = match self.engine.proxy_for(&host) {
            Some(proxy) => Some(self.bridge_to(&proxy)?),
            None => None,
        };
        let handle = chromium::Handle::default();
        passes(&self.passes).insert(id.to_string(), handle.clone());
        let (app, engine, all) = (self.app.clone(), self.engine.clone(), self.passes.clone());
        let (id, url) = (id.to_string(), url.to_string());
        let profile = engine.data_dir().join("webview").join("chromium").join(&id);
        std::thread::spawn(move || {
            let shown = {
                let (app, host, engine, id) =
                    (app.clone(), host.clone(), engine.clone(), id.clone());
                move || {
                    tracing::info!("{host} wants a person for its check");
                    engine.page_wants_person(&id);
                    crate::notify_waiting(
                        &app,
                        &format!("{host} wants you"),
                        "Its check did not pass by itself: tick the box on its page and the download starts.",
                    );
                }
            };
            let pass = chromium::Pass {
                exe: &exe,
                url: &url,
                profile,
                proxy,
                scripts: &[OVERLAYS, HANDOFF],
                show_after: SHOW_AFTER,
                handle,
            };
            let outcome = chromium::pass(pass, shown);
            passes(&all).remove(&id);
            match outcome {
                Ok(chromium::Outcome::Files { files, agent }) => {
                    let entries = files
                        .into_iter()
                        .map(|f| {
                            let mut headers =
                                BTreeMap::from([("Referer".to_string(), url.clone())]);
                            if let Some(cookie) = f.cookie {
                                headers.insert("Cookie".to_string(), cookie);
                            }
                            if let Some(agent) = &agent {
                                headers.insert("User-Agent".to_string(), agent.clone());
                            }
                            hosts::Entry {
                                url: f.url,
                                name: f.name,
                                size: f.size,
                                headers,
                                ..hosts::Entry::default()
                            }
                        })
                        .collect::<Vec<_>>();
                    let count = entries.len();
                    match engine.set_files(&id, entries) {
                        Ok(()) => tracing::info!("took over {count} file(s) for item {id}"),
                        Err(e) => notify(&app, "Could not use that download", &e),
                    }
                }
                Ok(chromium::Outcome::Gone(reason)) => engine.page_gone(&id, &reason),
                Ok(chromium::Outcome::Later(reason)) => engine.page_later(&id, &reason),
                Ok(chromium::Outcome::Closed) => engine.page_wants_person(&id),
                Err(e) => {
                    tracing::warn!(item = %id, "passing {url}: {e}");
                    engine.page_wants_person(&id);
                }
            }
        });
        Ok(())
    }

    fn open(
        &self,
        id: &str,
        url: &str,
        user_agent: Option<String>,
        quiet: bool,
    ) -> Result<(), String> {
        let label = format!("browse-{id}");
        let page: Url = url.parse().map_err(|e| format!("{url}: {e}"))?;
        let host = hosts::host_of(url);
        // Pages for a proxied host go through the same proxy as its downloads.
        let bridge = match self.engine.proxy_for(&host) {
            Some(proxy) => Some(self.bridge_to(&proxy)?),
            None => None,
        };
        let proxied = bridge.is_some();
        // One browser profile per route, so pages opened through the proxy
        // never share cookies or settings with direct ones.
        let profile =
            self.engine
                .data_dir()
                .join("webview")
                .join(if proxied { "proxy" } else { "direct" });
        // A wrapper's page leads to other hosts: its window is not sent back.
        let stay = Arc::new(Mutex::new(Stay::new(url, !hosts::wraps(url))));
        let capture = Capture {
            app: self.app.clone(),
            engine: self.engine.clone(),
            id: id.to_string(),
            label: label.clone(),
            user_agent,
            stay: stay.clone(),
        };
        let on_download = capture.clone();
        let on_handoff = capture.clone();
        let on_navigation = stay.clone();
        let mut builder = WebviewWindowBuilder::new(&self.app, &label, WebviewUrl::External(page))
            .title(format!("{host}: solve the page to start the download"))
            .inner_size(1100.0, 800.0)
            .data_directory(profile)
            .initialization_script(OVERLAYS)
            .initialization_script(HANDOFF)
            .on_download(move |_, event| on_download.on_download(event))
            .on_new_window(move |url, _| capture.on_popup(url))
            .on_navigation(move |url| match handed_off(url) {
                Some(Handoff { files, agent }) => {
                    on_handoff.take_files(files, agent);
                    false
                }
                None if let Some(reason) = gone(url) => {
                    on_handoff.gone(&reason);
                    false
                }
                None if let Some(reason) = later(url) => {
                    on_handoff.later(&reason);
                    false
                }
                None => lock(&on_navigation).navigating(url.as_str()),
            })
            .on_page_load(move |window, load| {
                // A page, not a download, is taking the window's place.
                if load.event() == PageLoadEvent::Started
                    && let Some(back) = lock(&stay).landed(load.url().as_str(), Instant::now())
                {
                    send_back(window, back, stay.clone());
                }
            });
        if quiet {
            // Drawn and running as any window, so the page's check sees a
            // visible page, but off every screen and out of the taskbar.
            builder = builder
                .position(-20000.0, -20000.0)
                .skip_taskbar(true)
                .focused(false);
        }
        if let Some(addr) = bridge {
            builder = builder.proxy_url(format!("socks5://{addr}").parse().expect("valid URL"));
        }
        // Tauri's own arguments, and: a window out of sight still counts as
        // shown (Chromium would otherwise slow an off-screen page's timers);
        // through the proxy, WebRTC cannot reveal the real address. The same
        // for every window of a profile, as WebView2 requires.
        #[cfg(windows)]
        {
            let features = "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection,\
                            CalculateNativeWinOcclusion";
            builder = builder.additional_browser_args(&if proxied {
                format!("{features} --force-webrtc-ip-handling-policy=disable_non_proxied_udp")
            } else {
                features.to_string()
            });
        }
        builder
            .theme(crate::window_theme(self.engine.theme()))
            .build()
            .map_err(|e| e.to_string())?;
        Ok(())
    }
}

impl Desktop for Browser {
    fn browse(&self, id: &str, url: &str, user_agent: Option<String>) -> Result<(), String> {
        // Being passed in a full browser: that window comes on screen.
        if let Some(handle) = passes(&self.passes).get(id) {
            handle
                .show
                .store(true, std::sync::atomic::Ordering::Relaxed);
            return Ok(());
        }
        if let Some(window) = self.app.get_webview_window(&format!("browse-{id}")) {
            bring_forward(&window);
            let _ = window.set_focus();
            return Ok(());
        }
        self.open(id, url, user_agent, false)
    }

    fn pick_folder(&self, start: &std::path::Path) -> Option<std::path::PathBuf> {
        use tauri_plugin_dialog::DialogExt;
        let mut dialog = self
            .app
            .dialog()
            .file()
            .set_title("Download folder")
            .set_directory(start);
        if let Some(window) = self.app.get_webview_window(crate::MAIN) {
            dialog = dialog.set_parent(&window);
        }
        dialog.blocking_pick_folder()?.into_path().ok()
    }

    fn save_file(&self, name: &str, contents: &[u8]) -> Result<Option<PathBuf>, String> {
        use tauri_plugin_dialog::DialogExt;
        let mut dialog = self
            .app
            .dialog()
            .file()
            .set_title("Save diagnostics")
            .set_file_name(name)
            .add_filter("JSON", &["json"]);
        if let Some(window) = self.app.get_webview_window(crate::MAIN) {
            dialog = dialog.set_parent(&window);
        }
        let Some(path) = dialog.blocking_save_file() else {
            return Ok(None);
        };
        let path = path.into_path().map_err(|e| e.to_string())?;
        std::fs::write(&path, contents).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(Some(path))
    }
}

/// Takes the layers ads lay over a page off it (see the script).
const OVERLAYS: &str = include_str!("overlays.js");

/// Hands Haul the link a host's page receives (see the script).
const HANDOFF: &str = include_str!("handoff.js");

/// How long a page opened out of sight gets to pass its check by itself.
const SHOW_AFTER: Duration = Duration::from_secs(15);

/// Brings a window opened out of sight onto the screen; one already there
/// stays where it is.
fn bring_forward(window: &WebviewWindow) {
    let off_screen = window.outer_position().is_ok_and(|p| p.x < -10_000);
    if off_screen {
        let _ = window.center();
        let _ = window.set_skip_taskbar(false);
    }
    let _ = window.show();
    let _ = window.request_user_attention(Some(tauri::UserAttentionType::Informational));
}

/// One file the hand-off script sends.
#[derive(Debug, PartialEq, serde::Deserialize)]
struct Handed {
    url: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    size: Option<u64>,
}

/// What the hand-off script sends: a host's files (one, or a folder's) and
/// the window's user agent, which the downloads then use too.
#[derive(Debug, PartialEq)]
struct Handoff {
    files: Vec<(Url, Handed)>,
    agent: Option<String>,
}

/// The hand-off script's message, when `url` is one. Only http(s) links
/// count; a message with none is not one.
fn handed_off(url: &Url) -> Option<Handoff> {
    if url.host_str() != Some("haul.invalid") || url.path() != "/take" {
        return None;
    }
    let pair = |key: &str| {
        url.query_pairs()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.into_owned())
    };
    let sent: Vec<Handed> = serde_json::from_str(&pair("files")?).ok()?;
    let files: Vec<(Url, Handed)> = sent
        .into_iter()
        .filter_map(|f| {
            let link: Url = f.url.parse().ok()?;
            matches!(link.scheme(), "http" | "https").then_some((link, f))
        })
        .collect();
    (!files.is_empty()).then(|| Handoff {
        files,
        agent: pair("agent").filter(|a| !a.is_empty()),
    })
}

/// The reason the hand-off script gives when the page says its file is
/// gone, when `url` is that message.
fn gone(url: &Url) -> Option<String> {
    message(url, "/gone")
}

/// The reason the hand-off script gives when the page could not hand out
/// its link this time, when `url` is that message.
fn later(url: &Url) -> Option<String> {
    message(url, "/later")
}

fn message(url: &Url, path: &str) -> Option<String> {
    if url.host_str() != Some("haul.invalid") || url.path() != path {
        return None;
    }
    let reason = url
        .query_pairs()
        .find(|(k, _)| k == "reason")
        .map(|(_, v)| v.into_owned())
        .unwrap_or_default();
    Some(if reason.is_empty() {
        "the page says the file is gone".into()
    } else {
        reason
    })
}

fn passes(
    all: &Mutex<HashMap<String, chromium::Handle>>,
) -> MutexGuard<'_, HashMap<String, chromium::Handle>> {
    all.lock().unwrap_or_else(|e| e.into_inner())
}

fn lock(stay: &Mutex<Stay>) -> MutexGuard<'_, Stay> {
    stay.lock().unwrap_or_else(|e| e.into_inner())
}

/// Sends the window back to the host's page: back in its history first,
/// which keeps the page as it was, then to the page itself if that left it
/// away still. Off the handler's thread, since navigating from inside the
/// webview's own event would re-enter it.
fn send_back(window: WebviewWindow, back: SendBack, stay: Arc<Mutex<Stay>>) {
    std::thread::spawn(move || {
        let Ok(page) = back.page.parse::<Url>() else {
            return;
        };
        if back.history {
            let _ = window.eval("history.back()");
            std::thread::sleep(Duration::from_millis(1500));
            let away = window
                .url()
                .is_ok_and(|now| !lock(&stay).is_home(now.as_str()));
            if !away {
                return;
            }
        }
        let _ = window.navigate(page);
    });
}

#[derive(Clone)]
struct Capture {
    app: AppHandle,
    engine: Engine,
    id: String,
    label: String,
    user_agent: Option<String>,
    stay: Arc<Mutex<Stay>>,
}

impl Capture {
    fn on_download(&self, event: DownloadEvent<'_>) -> bool {
        let DownloadEvent::Requested { url, destination } = event else {
            return true;
        };
        let name = destination
            .file_name()
            .map(|n| n.to_string_lossy().into_owned());
        self.take(url, name);
        false
    }

    /// Popups are mostly ads; one that opens a file is the download itself,
    /// and one on the host's own site loads in place, unless the page goes
    /// on by itself first.
    fn on_popup(&self, url: Url) -> NewWindowResponse<Wry> {
        if matches!(url.scheme(), "http" | "https") && hosts::guess_name(url.as_str()).is_some() {
            self.take(url, None);
            return NewWindowResponse::Deny;
        }
        let at = Instant::now();
        if lock(&self.stay).opened(url.as_str(), at) {
            let this = self.clone();
            std::thread::spawn(move || {
                std::thread::sleep(stay::GRACE);
                let page = lock(&this.stay).open_now(at).and_then(|p| p.parse().ok());
                if let (Some(page), Some(window)) = (page, this.app.get_webview_window(&this.label))
                {
                    let _ = window.navigate(page);
                }
            });
        }
        NewWindowResponse::Deny
    }

    /// Hands the download to the engine with the page's cookies, then closes
    /// the window.
    fn take(&self, url: Url, name: Option<String>) {
        let file = Handed {
            url: url.to_string(),
            name,
            size: None,
        };
        self.take_files(vec![(url, file)], None);
    }

    /// The page says its file is gone: the mirror failed, and the window
    /// closes without asking anyone.
    fn gone(&self, reason: &str) {
        self.engine.page_gone(&self.id, reason);
        if let Some(window) = self.app.get_webview_window(&self.label) {
            let window = window.clone();
            // Not from inside the webview's own event.
            std::thread::spawn(move || {
                let _ = window.close();
            });
        }
    }

    /// The page could not hand out its link this time: it is tried again
    /// later, and the window closes without asking anyone.
    fn later(&self, reason: &str) {
        self.engine.page_later(&self.id, reason);
        if let Some(window) = self.app.get_webview_window(&self.label) {
            let window = window.clone();
            std::thread::spawn(move || {
                let _ = window.close();
            });
        }
    }

    /// Hands what the page gave (a file, or a folder's files) to the engine,
    /// each link with the page's cookies for it, then closes the window. The
    /// user agent the page reports stands in when the window was opened
    /// without one.
    fn take_files(&self, files: Vec<(Url, Handed)>, agent: Option<String>) {
        let mut this = self.clone();
        if this.user_agent.is_none() {
            this.user_agent = agent;
        }
        // Reading cookies needs the main thread, which is running this handler.
        std::thread::spawn(move || {
            let window = this.app.get_webview_window(&this.label);
            let page = window.as_ref().and_then(|w| w.url().ok());
            let entries: Vec<hosts::Entry> = files
                .into_iter()
                .map(|(link, file)| {
                    let mut headers = BTreeMap::new();
                    if let Some(window) = &window
                        && let Ok(cookies) = window.cookies_for_url(link.clone())
                        && !cookies.is_empty()
                    {
                        let cookie = cookies
                            .iter()
                            .map(|c| format!("{}={}", c.name(), c.value()))
                            .collect::<Vec<_>>()
                            .join("; ");
                        headers.insert("Cookie".to_string(), cookie);
                    }
                    if let Some(page) = &page {
                        headers.insert("Referer".to_string(), page.to_string());
                    }
                    if let Some(agent) = &this.user_agent {
                        headers.insert("User-Agent".to_string(), agent.clone());
                    }
                    hosts::Entry {
                        url: link.to_string(),
                        name: file.name.filter(|n| !n.is_empty()),
                        size: file.size,
                        headers,
                        ..hosts::Entry::default()
                    }
                })
                .collect();
            let count = entries.len();
            match this.engine.set_files(&this.id, entries) {
                Ok(()) => {
                    tracing::info!("took over {count} file(s) for item {}", this.id);
                    if let Some(window) = window {
                        let _ = window.close();
                    }
                }
                Err(e) => notify(&this.app, "Could not use that download", &e),
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_page_hands_off_its_files() {
        let files = r#"[{"url":"https://cdn.example/a b.rar","name":"a b.rar","size":5},{"url":"https://cdn.example/c.rar"},{"url":"javascript:alert(1)"}]"#;
        let mut sent = Url::parse("https://haul.invalid/take").unwrap();
        sent.query_pairs_mut()
            .append_pair("files", files)
            .append_pair("agent", "UA");
        let got = handed_off(&sent).unwrap();
        let links: Vec<&str> = got.files.iter().map(|(l, _)| l.as_str()).collect();
        assert_eq!(
            links,
            ["https://cdn.example/a%20b.rar", "https://cdn.example/c.rar"],
            "http(s) only"
        );
        assert_eq!(got.files[0].1.name.as_deref(), Some("a b.rar"));
        assert_eq!(got.files[0].1.size, Some(5));
        assert_eq!(got.agent.as_deref(), Some("UA"));
        for other in [
            "https://vik1ngfile.site/f/x",
            "https://haul.invalid/take?files=%5B%7B%22url%22%3A%22javascript%3Ax%22%7D%5D",
            "https://haul.invalid/take",
        ] {
            assert_eq!(handed_off(&other.parse().unwrap()), None, "{other}");
        }
    }
}
