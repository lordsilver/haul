//! The in-app browser for hosts that need a person: the user solves the
//! host's page in a window of its own, and when the page starts the download,
//! the app stops it and hands the link (with the page's cookies) to the
//! engine, which downloads it with resume and the usual limits. On a page
//! whose link Haul knows where to find (vikingfile's), the window takes it as
//! soon as the page has it, without the download button.
//!
//! With a proxy configured, the window goes through it too, by way of the
//! local bridge, because browser engines cannot pass proxy credentials.
//!
//! A host's page stays on the host's site ([`Stay`]): an ad that sends it
//! elsewhere on a click is sent back, and a window it opens on its own site
//! loads in place, since the window has no tabs to open it in. A layer an
//! ad lays over the page to catch the first click is taken off it.

use std::{
    collections::BTreeMap,
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

use crate::notify;

pub struct Browser {
    app: AppHandle,
    engine: Engine,
    /// The local bridge to the proxy, started the first time a page needs it.
    bridge: Mutex<Option<Bridge>>,
}

impl Browser {
    pub fn new(app: AppHandle, engine: Engine) -> Self {
        Self {
            app,
            engine,
            bridge: Mutex::new(None),
        }
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
        if self.app.get_webview_window(&label).is_some() {
            return Ok(());
        }
        self.open(id, url, None, true)?;
        let app = self.app.clone();
        let host = hosts::host_of(url);
        std::thread::spawn(move || {
            std::thread::sleep(SHOW_AFTER);
            if let Some(window) = app.get_webview_window(&label) {
                tracing::info!("{host} wants a person for its check");
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
                Some(Handoff { link, name, agent }) => {
                    on_handoff.take_as(link, name, agent);
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

/// What the hand-off script sends: the link, the file's name and the
/// window's user agent, which the download then uses too.
#[derive(Debug, PartialEq)]
struct Handoff {
    link: Url,
    name: Option<String>,
    agent: Option<String>,
}

/// The hand-off script's message, when `url` is one.
fn handed_off(url: &Url) -> Option<Handoff> {
    if url.host_str() != Some("haul.invalid") || url.path() != "/take" {
        return None;
    }
    let pair = |key: &str| {
        url.query_pairs()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.into_owned())
    };
    let link: Url = pair("url")?.parse().ok()?;
    if !matches!(link.scheme(), "http" | "https") {
        return None;
    }
    Some(Handoff {
        link,
        name: pair("name").filter(|n| !n.is_empty()),
        agent: pair("agent").filter(|a| !a.is_empty()),
    })
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
        self.take_as(url, name, None);
    }

    /// [`Capture::take`], with the user agent the page itself reports when
    /// the window was opened without one.
    fn take_as(&self, url: Url, name: Option<String>, agent: Option<String>) {
        let mut this = self.clone();
        if this.user_agent.is_none() {
            this.user_agent = agent;
        }
        // Reading cookies needs the main thread, which is running this handler.
        std::thread::spawn(move || {
            let window = this.app.get_webview_window(&this.label);
            let mut headers = BTreeMap::new();
            if let Some(window) = &window {
                if let Ok(cookies) = window.cookies_for_url(url.clone())
                    && !cookies.is_empty()
                {
                    let cookie = cookies
                        .iter()
                        .map(|c| format!("{}={}", c.name(), c.value()))
                        .collect::<Vec<_>>()
                        .join("; ");
                    headers.insert("Cookie".to_string(), cookie);
                }
                if let Ok(page) = window.url() {
                    headers.insert("Referer".to_string(), page.to_string());
                }
            }
            if let Some(agent) = &this.user_agent {
                headers.insert("User-Agent".to_string(), agent.clone());
            }
            match this
                .engine
                .set_direct(&this.id, url.as_str(), headers, name)
            {
                Ok(()) => {
                    tracing::info!("took over the download for item {}", this.id);
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
    fn a_page_hands_off_its_link() {
        let sent: Url =
            "https://haul.invalid/take?url=https%3A%2F%2Fcdn.example%2Fa%20b.rar&name=a+b.rar"
                .parse()
                .unwrap();
        let got = handed_off(&sent).unwrap();
        assert_eq!(got.link.as_str(), "https://cdn.example/a%20b.rar");
        assert_eq!(got.name.as_deref(), Some("a b.rar"));
        assert_eq!(got.agent, None);
        for other in [
            "https://vik1ngfile.site/f/x",
            "https://haul.invalid/take?url=javascript%3Aalert(1)",
            "https://haul.invalid/take",
        ] {
            assert_eq!(handed_off(&other.parse().unwrap()), None, "{other}");
        }
    }
}
