//! The in-app browser for hosts that need a person: the user solves the
//! host's page in a window of its own, and when the page starts the download,
//! the app stops it and hands the link (with the page's cookies) to the
//! engine, which downloads it with resume and the usual limits. On a page
//! whose links Haul knows where to find (`handoff.js` has a recipe per host:
//! vikingfile's button, gofile's folder listing, filecrypt's solved page),
//! the window takes them as soon as the page has them, without the download
//! button.
//!
//! With a proxy configured, the window goes through it too, by way of the
//! local bridge, because browser engines cannot pass proxy credentials.
//!
//! A host's page stays on the host's site ([`Stay`]): an ad that sends it
//! elsewhere on a click is sent back, and a window it opens on its own site
//! loads in place, since the window has no tabs to open it in. A wrapper's
//! page is refused other sites before they load. A layer an ad lays over the
//! page to catch the first click is taken off it. The window's menu takes it
//! back to the page it was opened on, should anything still lead it away.
//! The log says what the window did, by item: opened, each page it loaded,
//! what it refused, and closed.
//!
//! Pages passed out of sight go to a full browser where there is one to
//! drive (Edge on Windows, [`chromium`]): Cloudflare Turnstile asks for its
//! box in an embedded WebView2 every time, and passes by itself in Edge.

use std::{
    collections::{BTreeMap, HashMap},
    net::SocketAddr,
    sync::{Arc, LazyLock, Mutex, MutexGuard},
    time::{Duration, Instant},
};

use haul_core::{
    Engine,
    bridge::Bridge,
    hosts,
    pass_ends::{Closed, Next, Tries},
    paths,
    stay::{self, Go, Keep, SendBack, Stay},
};
use haul_web::Desktop;
use serde_json::json;
use tauri::{
    AppHandle, Manager, Url, WebviewUrl, WebviewWindow, WebviewWindowBuilder, WindowEvent, Wry,
    menu::{Menu, MenuItem, Submenu},
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

    /// The items whose page a full browser is passing out of sight.
    pub fn passing_out_of_sight(&self) -> Vec<String> {
        passes(&self.passes).keys().cloned().collect()
    }

    /// Stops passing item `id`'s page, its row having gone or stopped
    /// waiting: nobody is asked about it.
    pub fn abandon(&self, id: &str) {
        if let Some(handle) = passes(&self.passes).get(id) {
            tracing::info!(item = %id, "no longer waiting: its page's pass stops");
            handle
                .abandoned
                .store(true, std::sync::atomic::Ordering::Relaxed);
            handle
                .stop
                .store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// Whether item `id`'s page is open, in a window or a full browser.
    pub fn is_passing(&self, id: &str) -> bool {
        self.app
            .get_webview_window(&format!("browse-{id}"))
            .is_some()
            || passes(&self.passes).contains_key(id)
    }

    /// Whether a person has item `id`'s page before them in Haul's window: on
    /// screen, unlike a page passed out of sight where there is no Edge.
    pub fn person_has_page(&self, id: &str) -> bool {
        self.app
            .get_webview_window(&format!("browse-{id}"))
            .is_some_and(|window| !off_screen(&window))
    }

    /// The bridge's address, pointed at the proxy currently configured.
    /// The local bridge to the proxy `host`'s pages go through, when they go
    /// through one.
    fn bridge_for(&self, host: &str) -> Result<Option<SocketAddr>, String> {
        self.engine
            .proxy_for(host)
            .map(|proxy| self.bridge_to(&proxy))
            .transpose()
    }

    fn bridge_to(&self, proxy: &str) -> Result<SocketAddr, String> {
        let mut slot = haul_core::lock::held(&self.bridge);
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
    /// after [`SHOW_AFTER`] (plus the time the host's page takes on its own,
    /// [`hosts::page_takes`]), its check wanting a person.
    pub fn pass(&self, id: &str, url: &str) -> Result<(), String> {
        let label = format!("browse-{id}");
        if self.is_passing(id) {
            return Ok(());
        }
        let browsers = chromium::browsers();
        if !browsers.is_empty() {
            return self.pass_in(browsers, id, url);
        }
        self.open(id, url, None, true)?;
        let app = self.app.clone();
        let host = hosts::host_of(url);
        let wait = SHOW_AFTER + hosts::page_takes(url);
        let (engine, id) = (self.engine.clone(), id.to_string());
        std::thread::spawn(move || {
            std::thread::sleep(wait);
            if let Some(window) = app.get_webview_window(&label) {
                tracing::info!("{host} wants a person for its check");
                if engine.page_wants_person(&id) {
                    bring_forward(&window);
                    crate::notify_waiting(
                        &app,
                        std::slice::from_ref(&id),
                        &format!("{host} wants you"),
                        "Its check did not pass by itself: tick the box on its page and the download starts.",
                    );
                } else if off_screen(&window) {
                    // A copied page went instead, and its window with it, or
                    // this was a try of its own and nobody is asked. A window
                    // a person brought forward meanwhile is theirs.
                    let _ = window.close();
                }
            }
        });
        Ok(())
    }

    /// [`Browser::pass`] in a full browser driven over DevTools: the first
    /// of `browsers`, and the other when it keeps closing the page by
    /// itself ([`haul_core::pass_ends`]).
    fn pass_in(&self, browsers: Vec<chromium::Found>, id: &str, url: &str) -> Result<(), String> {
        let host = hosts::host_of(url);
        let proxy = self.bridge_for(&host)?;
        let handle = chromium::Handle::default();
        passes(&self.passes).insert(id.to_string(), handle.clone());
        let abandoned = handle.abandoned.clone();
        let (app, engine, all) = (self.app.clone(), self.engine.clone(), self.passes.clone());
        let (id, url) = (id.to_string(), url.to_string());
        // One profile per site, kept: a check passed on one of its pages
        // (Cloudflare's clearance cookie) holds for the next. The other
        // browser keeps one of its own.
        let site = hosts::site_of(&url);
        let shot = paths::wants_person_shot(engine.data_dir(), &site);
        std::thread::spawn(move || {
            // A page that lists links hands them over as they show; Haul
            // keeps those to hosts it knows, which ends the pass.
            let lists = engine.page_lists(&id, &url);
            // From this thread, outside the async runtime adding links (and
            // starting their lookups) needs.
            let take = |s: &chromium::Shown| {
                tauri::async_runtime::block_on(async {
                    engine.page_listed(&id, &s.links, &s.text, s.title.as_deref()) > 0
                })
            };
            let names: Vec<&str> = browsers.iter().map(|b| b.name).collect();
            // The engine knows the page by its link; the browser goes to
            // its stand-in in stand-in mode.
            let reached = hosts::reach(&url);
            let scripts = page_scripts(lists);
            // Passed again at once while the browser closes the page by
            // itself, the row's pass running meanwhile.
            let outcome = run_tries(&names, &handle.stop, &id, &url, |browser| {
                chromium::pass(chromium::Pass {
                    exe: &browsers[browser].exe,
                    item: &id,
                    url: &reached,
                    profile: paths::pass_profile(engine.data_dir(), &site, browser > 0),
                    proxy,
                    scripts: &scripts,
                    show_after: SHOW_AFTER + hosts::page_takes(&url),
                    handle: handle.clone(),
                    take_links: if lists { Some(&take) } else { None },
                    // A page that lists links has shown all it will once it
                    // settles past its checks.
                    ends_settled: lists,
                    // A file's page on a host Haul has no recipe for: its
                    // download button is pressed, as a person would. Never
                    // on a wrapper's or a paste's page.
                    press_download: hosts::presses_download(&url, lists),
                    // A host's own page may lead to its file elsewhere; any
                    // other page shows what it has where it is.
                    keep_to_site: !hosts::page_passes(&url),
                    // A page Haul reads itself, once the browser is past the
                    // check in front of its site.
                    hand_clearance: engine.takes_clearance(&url),
                })
            });
            passes(&all).remove(&id);
            match outcome {
                Ended::Page(chromium::Outcome::Files { files, agent, page }) => {
                    let said = PageSaid::Files {
                        files,
                        shown: page,
                        referer: Some(url.clone()),
                        agent,
                    };
                    tell_engine(&app, &engine, &id, &url, said);
                }
                Ended::Page(chromium::Outcome::Gone(reason)) => {
                    tell_engine(&app, &engine, &id, &url, PageSaid::Gone(reason));
                }
                Ended::Page(chromium::Outcome::Failed(reason)) => engine.page_failed(&id, &reason),
                Ended::Page(chromium::Outcome::Later { reason, seconds }) => {
                    tell_engine(
                        &app,
                        &engine,
                        &id,
                        &url,
                        PageSaid::Later { reason, seconds },
                    );
                }
                Ended::Page(chromium::Outcome::Cleared { cookies, agent }) => {
                    tracing::info!(item = %id, "{host} let the browser past its check: Haul reads {url} itself");
                    // Unwrapping again starts tasks, which need the async
                    // runtime this thread is outside of.
                    tauri::async_runtime::block_on(async {
                        engine.page_cleared(&id, &cookies, agent.as_deref())
                    });
                }
                Ended::Page(chromium::Outcome::Listed) => {
                    tracing::info!("took the links {url} shows");
                }
                // A paste, or a copied page on trial, has nothing more to
                // give; any other page may want a click on it.
                Ended::Page(chromium::Outcome::Settled)
                    if hosts::is_paste(&url) || engine.is_tentative(&id) =>
                {
                    engine.page_gone(&id, &hosts::lists_nothing(&url));
                }
                Ended::Page(chromium::Outcome::Settled) => {
                    tracing::info!(item = %id, "{url} passed its check but shows no links Haul takes");
                    if engine.page_needs_click(&id) && first_ask_in_a_while(&host) {
                        crate::notify_waiting(
                            &app,
                            std::slice::from_ref(&id),
                            &format!("{host} wants you"),
                            "Its page shows no links Haul can use by itself: open it in Haul and go on to them.",
                        );
                    }
                }
                Ended::Page(chromium::Outcome::WantsPerson(look)) => {
                    // The gate it shows, when Haul knows it by its marks.
                    let gate = hosts::gates::detect(&look.page);
                    let named = gate.map_or("its check", |g| g.name());
                    tracing::info!(item = %id, page = %look.page, "{host} wants a person for {named}");
                    if let Some(gate) = gate {
                        engine.page_shows_gate(&id, &url, gate);
                    }
                    // The last such page of each site, to see what it asked.
                    if let Some(png) = look.shot {
                        let _ = std::fs::write(&shot, png);
                    }
                    if engine.page_wants_person(&id) && first_ask_in_a_while(&host) {
                        let body = match gate {
                            Some(g) => format!(
                                "It shows {}, which did not pass by itself: open it in Haul, solve it, and the download goes on.",
                                g.name()
                            ),
                            None => "Its check did not pass by itself: open it in Haul, tick the box, and the download goes on.".into(),
                        };
                        crate::notify_waiting(
                            &app,
                            std::slice::from_ref(&id),
                            &format!("{host} wants you"),
                            &body,
                        );
                    }
                }
                ended => match tell_of(ended, abandoned.load(std::sync::atomic::Ordering::Relaxed))
                {
                    Tell::Nothing => {}
                    Tell::WantsPerson => {
                        engine.page_wants_person(&id);
                    }
                    Tell::Failed(reason) => {
                        tracing::warn!(item = %id, "passing {url}: {reason}");
                        engine.page_failed(&id, &reason);
                    }
                    Tell::NotPassed(e) => {
                        tracing::warn!(item = %id, "passing {url}: {e}");
                        engine.page_not_passed(&id, &e);
                    }
                },
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
        // The engine knows the page by its link; the window goes to its
        // stand-in in stand-in mode, and keeps to that.
        let reached = hosts::reach(url);
        let page: Url = reached.parse().map_err(|e| format!("{url}: {e}"))?;
        let host = hosts::host_of(url);
        // Pages for a proxied host go through the same proxy as its downloads.
        let bridge = self.bridge_for(&host)?;
        let proxied = bridge.is_some();
        // One browser profile per route, so pages opened through the proxy
        // never share cookies or settings with direct ones.
        let profile =
            self.engine
                .data_dir()
                .join("webview")
                .join(if proxied { "proxy" } else { "direct" });
        // A page on a site Haul has no code for (a post that lists links, a
        // paste) leads to other hosts: such a window goes where it will, and
        // the page's script hands over the links it shows.
        let lists = self.engine.page_lists(id, url);
        let keep = if hosts::wraps(url) {
            Keep::Wrapper
        } else if lists {
            Keep::Free
        } else {
            Keep::Host
        };
        let stay = Arc::new(Mutex::new(Stay::new(&reached, keep)));
        let capture = Capture {
            app: self.app.clone(),
            engine: self.engine.clone(),
            id: id.to_string(),
            page: url.to_string(),
            label: label.clone(),
            user_agent,
            stay: stay.clone(),
        };
        let on_download = capture.clone();
        let on_handoff = capture.clone();
        let on_navigation = stay.clone();
        let item = id.to_string();
        let (on_load, loaded) = (item.clone(), page.clone());
        let mut builder = WebviewWindowBuilder::new(&self.app, &label, WebviewUrl::External(page))
            .title(format!("{host}: solve the page to start the download"))
            .menu(page_menu(&self.app, id).map_err(|e| e.to_string())?)
            .inner_size(1100.0, 800.0)
            .data_directory(profile)
            .initialization_script(if hosts::stand_in().is_some() {
                STAND_IN
            } else {
                ""
            })
            .initialization_script(OVERLAYS)
            .initialization_script(if lists { LISTS } else { "" })
            .initialization_script(HANDOFF)
            .on_download(move |_, event| on_download.on_download(event))
            .on_new_window(move |url, _| capture.on_popup(url))
            .on_navigation(move |url| match said(url) {
                Some(Said::Files { files, agent, page }) => {
                    on_handoff.take_files(files, agent, page);
                    false
                }
                Some(Said::Gone(reason)) => {
                    on_handoff.gone(&reason);
                    false
                }
                Some(Said::Links(shown)) => {
                    on_handoff.listed(shown);
                    false
                }
                Some(Said::Later { reason, seconds }) => {
                    on_handoff.later(&reason, seconds);
                    false
                }
                None => {
                    let go = lock(&on_navigation).navigating(url.as_str());
                    if go != Go::Let {
                        refused(&item, url.as_str(), go);
                    }
                    go == Go::Let
                }
            })
            .on_page_load(move |window, load| {
                // A page, not a download, is taking the window's place.
                if load.event() != PageLoadEvent::Started {
                    return;
                }
                let url = load.url().as_str();
                if url != "about:blank" {
                    haul_core::debug::emit(
                        "page",
                        json!({ "event": "loads", "id": on_load, "url": url }),
                    );
                }
                if let Some(back) = lock(&stay).landed(url, Instant::now()) {
                    haul_core::debug::emit(
                        "page",
                        json!({ "event": "sent back", "id": on_load, "url": url, "to": back.page }),
                    );
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
        let window = builder
            .theme(crate::window_theme(self.engine.theme()))
            .build()
            .map_err(|e| e.to_string())?;
        haul_core::debug::emit(
            "page",
            json!({ "event": "opened", "id": id, "url": url, "quiet": quiet, "keep": format!("{keep:?}") }),
        );
        let (back, reload) = (format!("back-{id}"), format!("reload-{id}"));
        let (on_menu, start) = (window.clone(), loaded);
        window.on_menu_event(move |_, event| {
            let to = if event.id.as_ref() == back.as_str() {
                start.clone()
            } else if event.id.as_ref() == reload.as_str() {
                match on_menu.url() {
                    Ok(url) => url,
                    Err(_) => return,
                }
            } else {
                return;
            };
            let _ = on_menu.navigate(to);
        });
        let closed = id.to_string();
        window.on_window_event(move |event| {
            if matches!(event, WindowEvent::Destroyed) {
                haul_core::debug::emit("page", json!({ "event": "closed", "id": closed }));
            }
        });
        Ok(())
    }
}

/// The page window's menu: back to the page it was opened on, and the page
/// it shows loaded again.
fn page_menu(app: &AppHandle, id: &str) -> tauri::Result<Menu<Wry>> {
    let back = MenuItem::with_id(
        app,
        format!("back-{id}"),
        "Back to the page",
        true,
        Some("Alt+Home"),
    )?;
    let reload = MenuItem::with_id(app, format!("reload-{id}"), "Reload", true, Some("F5"))?;
    let page = Submenu::with_items(app, "Page", true, &[&back, &reload])?;
    Menu::with_items(app, &[&page])
}

/// Logs a navigation of item `id`'s page window that was refused.
fn refused(id: &str, url: &str, why: Go) {
    let why = match why {
        Go::Ad => "a site a window was sent back from",
        _ => "another site than the wrapper's",
    };
    haul_core::debug::emit(
        "page",
        json!({ "event": "refused", "id": id, "url": url, "why": why }),
    );
}

/// Runs a page's tries: `run(browser)` passes it once in browser number
/// `browser` of `names`. One that closes by itself runs again as [`Tries`]
/// says, until the page says something, Haul stops it (`stop`), or the tries
/// are used up.
fn run_tries(
    names: &[&str],
    stop: &std::sync::atomic::AtomicBool,
    id: &str,
    url: &str,
    mut run: impl FnMut(usize) -> Result<chromium::Outcome, String>,
) -> Ended {
    let mut tries = Tries::new(names.iter().map(|n| n.to_string()).collect());
    loop {
        let how = match run(tries.browser()) {
            Ok(chromium::Outcome::Closed(how)) => how,
            Err(e) => Closed::NotStarted(e),
            Ok(said) => return Ended::Page(said),
        };
        // Stopped while it ended some other way: stopped all the same.
        let how = match stop.load(std::sync::atomic::Ordering::Relaxed) {
            true => Closed::Stopped,
            false => how,
        };
        match tries.ended(how) {
            Next::Again { browser } => {
                let name = names[browser];
                let number = tries.number();
                tracing::warn!(item = %id, "passing {url} again, in {name} (try {number})");
                haul_core::debug::emit(
                    "pass",
                    json!({ "event": "again", "id": id, "url": url, "browser": name, "try": number }),
                );
            }
            Next::Stop => return Ended::Stopped,
            Next::NotStarted(e) => return Ended::NotStarted(e),
            Next::Fail(reason) => return Ended::Browser(reason),
        }
    }
}

/// What the engine is told of a pass that ended without the page's word.
#[derive(Debug, PartialEq)]
enum Tell {
    Nothing,
    /// The page is the person's ([`Engine::page_wants_person`]).
    WantsPerson,
    /// The page failed for now, nobody asked ([`Engine::page_failed`]).
    Failed(String),
    /// The browser never showed it ([`Engine::page_not_passed`]).
    NotPassed(String),
}

/// What the engine is told of `ended`, a pass that ended without the page's
/// word; `abandoned` when Haul stopped it because its row went or stopped
/// waiting.
fn tell_of(ended: Ended, abandoned: bool) -> Tell {
    match ended {
        // Told by the page; `run_tries` never lets a close through.
        Ended::Page(_) => Tell::Nothing,
        // Stopped because the row went or stopped waiting: nobody is asked.
        Ended::Stopped if abandoned => Tell::Nothing,
        // Stopped because a person opened the page in Haul's window: theirs
        // now.
        Ended::Stopped => Tell::WantsPerson,
        // The browser kept closing the page by itself: its failure, not the
        // page's, and nobody is asked.
        Ended::Browser(reason) => Tell::Failed(reason),
        Ended::NotStarted(e) => Tell::NotPassed(e),
    }
}

/// How a page's pass ended, its tries told.
#[derive(Debug)]
enum Ended {
    /// With the page's word.
    Page(chromium::Outcome),
    /// Haul stopped it.
    Stopped,
    /// The browser never showed the page on the first try.
    NotStarted(String),
    /// The browser kept closing the page by itself, for this reason.
    Browser(String),
}

impl Desktop for Browser {
    fn debug_inspect(&self) -> serde_json::Value {
        let passes: serde_json::Map<String, serde_json::Value> = passes(&self.passes)
            .iter()
            .map(|(id, h)| {
                let mut info = haul_core::lock::held(&h.info).clone();
                if let serde_json::Value::Object(o) = &mut info {
                    o.insert(
                        "stopping".into(),
                        h.stop.load(std::sync::atomic::Ordering::Relaxed).into(),
                    );
                    o.insert(
                        "abandoned".into(),
                        h.abandoned
                            .load(std::sync::atomic::Ordering::Relaxed)
                            .into(),
                    );
                }
                (id.clone(), info)
            })
            .collect();
        let windows: Vec<String> = self
            .app
            .webview_windows()
            .into_keys()
            .filter(|label| label.starts_with("browse-"))
            .collect();
        serde_json::json!({
            "edge": chromium::find().map(|p| p.display().to_string()),
            "passes_out_of_sight": passes,
            "browse_windows": windows,
        })
    }

    fn debug_page(&self, id: &str, ask: &serde_json::Value) -> Result<serde_json::Value, String> {
        let handle = passes(&self.passes)
            .get(id)
            .cloned()
            .ok_or("no page of that item is being passed out of sight")?;
        let what = if let Some(script) = ask["eval"].as_str() {
            chromium::PageAsk::Eval(script.to_string())
        } else if let Some(selector) = ask["click"].as_str() {
            chromium::PageAsk::Click(selector.to_string())
        } else if ask["cookies"].as_bool() == Some(true) {
            chromium::PageAsk::Cookies
        } else {
            chromium::PageAsk::Look
        };
        handle.ask(what)
    }

    fn browse(&self, id: &str, url: &str, user_agent: Option<String>) -> Result<(), String> {
        // Being passed out of sight: that pass stops (its browser never
        // comes on screen), and the page opens here.
        if let Some(handle) = passes(&self.passes).get(id) {
            handle
                .stop
                .store(true, std::sync::atomic::Ordering::Relaxed);
        }
        if let Some(window) = self.app.get_webview_window(&format!("browse-{id}")) {
            bring_forward(&window);
            let _ = window.set_focus();
            return Ok(());
        }
        self.open(id, url, user_agent, false)
    }

    fn pick_folder(&self, start: &std::path::Path) -> Option<std::path::PathBuf> {
        crate::dialogs::pick_folder(&self.app, start)
    }

    fn pick_save(&self, start: &std::path::Path, name: &str) -> Option<std::path::PathBuf> {
        crate::dialogs::pick_save(&self.app, start, name)
    }

    /// Tauri's installer installs over this version (`/UPDATE`, which skips
    /// asking to uninstall it first), shows its progress without asking
    /// anything (`/P`), and starts Haul again when done (`/R`). Haul closes
    /// first, as its files are being replaced.
    fn install_update(&self, installer: &std::path::Path) -> Result<(), String> {
        crate::dialogs::run_installer(installer, "/P /UPDATE /R")?;
        let app = self.app.clone();
        std::thread::spawn(move || {
            // Long enough for the answer to reach the page.
            std::thread::sleep(std::time::Duration::from_millis(500));
            app.exit(0);
        });
        Ok(())
    }
}

/// What a page told, wherever it was open (passed out of sight, or in
/// Haul's own window): said to the engine the same way by [`tell_engine`].
enum PageSaid {
    /// It handed over files, with the cookies each goes with; or, for a
    /// wrapper's page Haul reads (`shown`), what it holds.
    Files {
        files: Vec<chromium::File>,
        shown: Option<String>,
        referer: Option<String>,
        agent: Option<String>,
    },
    /// Its file is no more.
    Gone(String),
    /// It could not hand out its link this time.
    Later {
        reason: String,
        seconds: Option<u64>,
    },
}

/// Tells the engine what page `url` of row `id` said. Answers whether the
/// row has what it waited for, so its page can go.
fn tell_engine(app: &AppHandle, engine: &Engine, id: &str, url: &str, said: PageSaid) -> bool {
    match said {
        PageSaid::Files {
            files,
            shown,
            referer,
            agent,
        } => {
            // A wrapper's page Haul finds what it holds in unwraps to that,
            // which starts lookups: tasks, which need the async runtime the
            // caller's thread is outside of.
            if let Some(html) = &shown
                && tauri::async_runtime::block_on(async { engine.page_shown(id, html) }) > 0
            {
                return true;
            }
            hand_over(app, engine, id, files, referer.as_deref(), agent.as_deref())
        }
        PageSaid::Gone(reason) => {
            engine.page_gone(id, &reason);
            true
        }
        PageSaid::Later { reason, seconds } => {
            engine.page_later(id, url, &reason, seconds);
            true
        }
    }
}

/// Whether to tell the person that `host`'s check wants them: once a minute
/// at most, however many of its pages ask (a page's pastes, say).
fn first_ask_in_a_while(host: &str) -> bool {
    static LAST: LazyLock<Mutex<HashMap<String, Instant>>> = LazyLock::new(Mutex::default);
    let mut last = haul_core::lock::held(&LAST);
    let now = Instant::now();
    if last
        .get(host)
        .is_some_and(|at| now.duration_since(*at) < Duration::from_secs(60))
    {
        return false;
    }
    last.insert(host.to_string(), now);
    true
}

/// Takes the layers ads lay over a page off it (see the script).
const OVERLAYS: &str = include_str!("overlays.js");

/// Hands Haul the link a host's page receives (see the script).
const HANDOFF: &str = include_str!("handoff.js");
/// Tells `handoff.js` the page lists links, to hand over as they show.
const LISTS: &str = "window.__haulLists = true;";
/// Tells `handoff.js` that `<site>.localhost` stands for `<site>`
/// ([`hosts::reach`]).
const STAND_IN: &str = "window.__haulStandIn = true;";

/// The scripts a page passed out of sight runs before its own: the flags
/// for `handoff.js` (stand-in mode, a page that lists links), then the two
/// scripts.
fn page_scripts(lists: bool) -> Vec<&'static str> {
    let flags = [(hosts::stand_in().is_some(), STAND_IN), (lists, LISTS)];
    flags
        .into_iter()
        .filter_map(|(on, flag)| on.then_some(flag))
        .chain([OVERLAYS, HANDOFF])
        .collect()
}

/// How long a page opened out of sight gets to pass its check by itself.
/// Passes that work take 10 to 15 seconds, more on a slow line; a page that
/// shows all it has sooner ends its pass then.
const SHOW_AFTER: Duration = Duration::from_secs(30);

/// Whether a window is still where a page passed out of sight opens.
fn off_screen(window: &WebviewWindow) -> bool {
    window.outer_position().is_ok_and(|p| p.x < -10_000)
}

/// Brings a window opened out of sight onto the screen; one already there
/// stays where it is.
fn bring_forward(window: &WebviewWindow) {
    if off_screen(window) {
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

/// What the hand-off script says, as a link to `haul.invalid` it sends the
/// window to.
#[derive(Debug, PartialEq)]
enum Said {
    /// A host's files (one, or a folder's) and the window's user agent,
    /// which the downloads then use too; on a wrapper's page Haul's own code
    /// reads, the parts of it that code reads. Only http(s) links count; a
    /// message with none is none.
    Files {
        files: Vec<(Url, Handed)>,
        agent: Option<String>,
        page: Option<String>,
    },
    /// The page says its file is gone.
    Gone(String),
    /// The page could not hand out its link this time, and how long it says
    /// its host limits this address, if it does.
    Later {
        reason: String,
        seconds: Option<u64>,
    },
    /// What a page that lists links shows.
    Links(chromium::Shown),
}

/// The hand-off script's message, when `url` is one.
fn said(url: &Url) -> Option<Said> {
    if url.host_str() != Some("haul.invalid") {
        return None;
    }
    let pair = |key: &str| {
        url.query_pairs()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.into_owned())
            .filter(|v| !v.is_empty())
    };
    let reason = || pair("reason").unwrap_or_else(|| "the page says the file was deleted".into());
    let list = |key: &str| -> Vec<String> {
        pair(key)
            .and_then(|v| serde_json::from_str(&v).ok())
            .unwrap_or_default()
    };
    match url.path() {
        "/take" => {
            let sent: Vec<Handed> = serde_json::from_str(&pair("files")?).ok()?;
            let files: Vec<(Url, Handed)> = sent
                .into_iter()
                .filter_map(|f| {
                    let link: Url = f.url.parse().ok()?;
                    matches!(link.scheme(), "http" | "https").then_some((link, f))
                })
                .collect();
            (!files.is_empty()).then(|| Said::Files {
                files,
                agent: pair("agent"),
                page: pair("page"),
            })
        }
        "/gone" => Some(Said::Gone(reason())),
        "/later" => Some(Said::Later {
            reason: reason(),
            seconds: pair("seconds").and_then(|s| s.parse().ok()),
        }),
        "/links" => Some(Said::Links(chromium::Shown {
            links: list("links"),
            text: list("shown"),
            title: pair("title"),
        })),
        _ => None,
    }
}

/// Hands a page's files to the engine for row `id`: each link with the
/// browser's cookies for it, the page as the referrer and the browser's user
/// agent, so the host serves them as it did the browser. Says so when the
/// engine cannot use them; returns whether it took them.
fn hand_over(
    app: &AppHandle,
    engine: &Engine,
    id: &str,
    files: Vec<chromium::File>,
    referer: Option<&str>,
    agent: Option<&str>,
) -> bool {
    let entries: Vec<hosts::Entry> = files
        .into_iter()
        .map(|f| {
            let mut headers = BTreeMap::new();
            let wanted = [
                ("Referer", referer.map(str::to_string)),
                ("Cookie", f.cookie),
                ("User-Agent", agent.map(str::to_string)),
            ];
            for (name, value) in wanted {
                if let Some(value) = value {
                    headers.insert(name.to_string(), value);
                }
            }
            hosts::Entry {
                url: f.url,
                name: f.name.filter(|n| !n.is_empty()),
                size: f.size,
                headers,
                ..hosts::Entry::default()
            }
        })
        .collect();
    let count = entries.len();
    match engine.set_files(id, entries) {
        Ok(()) => {
            tracing::info!("took over {count} file(s) for item {id}");
            true
        }
        Err(e) => {
            notify(app, "Could not use that download", &e);
            false
        }
    }
}

fn passes(
    all: &Mutex<HashMap<String, chromium::Handle>>,
) -> MutexGuard<'_, HashMap<String, chromium::Handle>> {
    haul_core::lock::held(all)
}

fn lock(stay: &Mutex<Stay>) -> MutexGuard<'_, Stay> {
    haul_core::lock::held(stay)
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
    /// The page the window was opened on.
    page: String,
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
        let in_place = lock(&self.stay).opened(url.as_str(), at);
        haul_core::debug::emit(
            "page",
            json!({ "event": "popup", "id": self.id, "url": url.as_str(), "in_place": in_place }),
        );
        if in_place {
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
        self.take_files(vec![(url, file)], None, None);
    }

    /// The page shows links to hosts Haul knows, or wrappers: they stand in
    /// for the row, and the window closes. Other links leave it open.
    fn listed(&self, shown: chromium::Shown) {
        let this = self.clone();
        // Adding links starts their lookups, which need the async runtime;
        // this runs on the window's own thread, which has none.
        tauri::async_runtime::spawn(async move {
            let s = shown;
            if this
                .engine
                .page_listed(&this.id, &s.links, &s.text, s.title.as_deref())
                == 0
            {
                return;
            }
            if let Some(window) = this.app.get_webview_window(&this.label) {
                let _ = window.close();
            }
        });
    }

    /// The page says its file is gone: the mirror failed, and the window
    /// closes without asking anyone.
    fn gone(&self, reason: &str) {
        tell_engine(
            &self.app,
            &self.engine,
            &self.id,
            &self.page,
            PageSaid::Gone(reason.to_string()),
        );
        self.close_soon();
    }

    /// The page could not hand out its link this time: it is tried again
    /// later, and the window closes without asking anyone.
    fn later(&self, reason: &str, seconds: Option<u64>) {
        let said = PageSaid::Later {
            reason: reason.to_string(),
            seconds,
        };
        tell_engine(&self.app, &self.engine, &self.id, &self.page, said);
        self.close_soon();
    }

    /// Closes the window, not from inside the webview's own event.
    fn close_soon(&self) {
        if let Some(window) = self.app.get_webview_window(&self.label) {
            std::thread::spawn(move || {
                let _ = window.close();
            });
        }
    }

    /// Hands what the page gave (a file, or a folder's files) to the engine,
    /// each link with the page's cookies for it ([`hand_over`]), then closes
    /// the window. The user agent the page reports stands in when the window
    /// was opened without one. A wrapper's page Haul finds what it holds in
    /// (`shown`, the parts of it Haul reads) unwraps to that instead.
    fn take_files(&self, files: Vec<(Url, Handed)>, agent: Option<String>, shown: Option<String>) {
        let mut this = self.clone();
        if this.user_agent.is_none() {
            this.user_agent = agent;
        }
        // Reading cookies needs the main thread, which is running this handler.
        std::thread::spawn(move || {
            let window = this.app.get_webview_window(&this.label);
            let page = window.as_ref().and_then(|w| w.url().ok());
            let cookie = |link: &Url| -> Option<String> {
                let cookies = window.as_ref()?.cookies_for_url(link.clone()).ok()?;
                (!cookies.is_empty()).then(|| {
                    cookies
                        .iter()
                        .map(|c| format!("{}={}", c.name(), c.value()))
                        .collect::<Vec<_>>()
                        .join("; ")
                })
            };
            let files = files
                .into_iter()
                .map(|(link, file)| chromium::File {
                    cookie: cookie(&link),
                    url: link.to_string(),
                    name: file.name,
                    size: file.size,
                })
                .collect();
            let said = PageSaid::Files {
                files,
                shown,
                referer: page.as_ref().map(Url::to_string),
                agent: this.user_agent.clone(),
            };
            if tell_engine(&this.app, &this.engine, &this.id, &this.page, said)
                && let Some(window) = window
            {
                let _ = window.close();
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `handoff.js`'s `real` gives a stand-in's address in its real form,
    /// as `hosts::real` does, and leaves every other address alone. Run in
    /// Node (CI has it); skipped where there is none.
    #[test]
    fn handoff_names_a_stand_in_by_its_real_address() {
        let start = HANDOFF
            .find("const real = ")
            .expect("handoff.js defines real");
        // The statement's own `;`, whatever line endings the checkout has.
        let end = start + HANDOFF[start..].find(';').expect("one statement");
        let cases = [
            (
                "http://filecrypt.cc.localhost:8799/Container/AAAAAAAA.html",
                "https://filecrypt.cc/Container/AAAAAAAA.html",
            ),
            (
                "http://api.gofile.io.localhost:8799/contents/abc123?wt=x",
                "https://api.gofile.io/contents/abc123?wt=x",
            ),
            ("http://gofile.io.localhost:8799", "https://gofile.io"),
            (
                "http://localhost:8799/files/a.rar",
                "http://localhost:8799/files/a.rar",
            ),
            (
                "http://127.0.0.1:8799/files/a.rar",
                "http://127.0.0.1:8799/files/a.rar",
            ),
            ("https://site.example/a.rar", "https://site.example/a.rar"),
        ];
        let inputs = serde_json::to_string(&cases.map(|(i, _)| i)).unwrap();
        let script = format!(
            "const window = {{ __haulStandIn: true }}; {}; console.log(JSON.stringify({inputs}.map(real)));",
            &HANDOFF[start..end]
        );
        let Ok(out) = std::process::Command::new("node")
            .args(["-e", &script])
            .output()
        else {
            eprintln!("no node here: skipped");
            return;
        };
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let got: Vec<String> = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(got, cases.map(|(_, want)| want));
    }

    #[test]
    fn a_page_hands_off_its_files() {
        let files = r#"[{"url":"https://cdn.example/a b.rar","name":"a b.rar","size":5},{"url":"https://cdn.example/c.rar"},{"url":"javascript:alert(1)"}]"#;
        let mut sent = Url::parse("https://haul.invalid/take").unwrap();
        sent.query_pairs_mut()
            .append_pair("files", files)
            .append_pair("agent", "UA");
        let Some(Said::Files { files, agent, .. }) = said(&sent) else {
            panic!("no files");
        };
        let links: Vec<&str> = files.iter().map(|(l, _)| l.as_str()).collect();
        assert_eq!(
            links,
            ["https://cdn.example/a%20b.rar", "https://cdn.example/c.rar"],
            "http(s) only"
        );
        assert_eq!(files[0].1.name.as_deref(), Some("a b.rar"));
        assert_eq!(files[0].1.size, Some(5));
        assert_eq!(agent.as_deref(), Some("UA"));
        for other in [
            "https://vik1ngfile.site/f/x",
            "https://haul.invalid/take?files=%5B%7B%22url%22%3A%22javascript%3Ax%22%7D%5D",
            "https://haul.invalid/take",
        ] {
            assert_eq!(said(&other.parse().unwrap()), None, "{other}");
        }
    }

    #[test]
    fn a_page_says_how_long_its_host_limits_the_address() {
        let later = |url: &str| said(&url.parse().unwrap());
        let limit = |seconds| {
            Some(Said::Later {
                reason: "limit".into(),
                seconds,
            })
        };
        assert_eq!(
            later("https://haul.invalid/later?reason=limit&seconds=6939"),
            limit(Some(6939))
        );
        assert_eq!(
            later("https://haul.invalid/later?reason=limit"),
            limit(None)
        );
        assert_eq!(
            later("https://haul.invalid/gone?reason=limit"),
            Some(Said::Gone("limit".into()))
        );
    }

    fn closed() -> Result<chromium::Outcome, String> {
        Ok(chromium::Outcome::Closed(Closed::NoDevTools(
            haul_core::pass_ends::Process::default(),
        )))
    }

    const URL: &str = "https://site.example/abc123";

    #[test]
    fn a_page_the_browser_keeps_closing_fails_without_asking_anyone() {
        let stop = std::sync::atomic::AtomicBool::new(false);
        let mut ran = Vec::new();
        let ended = run_tries(&["Edge", "Chrome"], &stop, "abc123", URL, |browser| {
            ran.push(browser);
            closed()
        });
        assert_eq!(
            ran,
            [0, 0, 0, 0, 1],
            "four tries in Edge, then one in Chrome"
        );
        let Tell::Failed(reason) = tell_of(ended, false) else {
            panic!("the page fails, under nobody's name");
        };
        assert!(
            reason.starts_with("Haul's browser closed the page by itself"),
            "{reason}"
        );
    }

    #[test]
    fn a_page_the_browser_shows_after_closing_is_taken() {
        let stop = std::sync::atomic::AtomicBool::new(false);
        let mut n = 0;
        let ended = run_tries(&["Edge"], &stop, "abc123", URL, |_| {
            n += 1;
            if n < 3 {
                closed()
            } else {
                Ok(chromium::Outcome::Listed)
            }
        });
        assert!(
            matches!(ended, Ended::Page(chromium::Outcome::Listed)),
            "{ended:?}"
        );
        assert_eq!(tell_of(ended, false), Tell::Nothing);
    }

    #[test]
    fn a_pass_stopped_while_it_closed_is_not_tried_again() {
        let stop = std::sync::atomic::AtomicBool::new(true);
        let mut n = 0;
        let ended = run_tries(&["Edge", "Chrome"], &stop, "abc123", URL, |_| {
            n += 1;
            closed()
        });
        assert_eq!(n, 1);
        assert!(matches!(ended, Ended::Stopped), "{ended:?}");
        // Abandoned: nobody is asked; else a person opened it in Haul.
        assert!(matches!(tell_of(Ended::Stopped, true), Tell::Nothing));
        assert_eq!(tell_of(Ended::Stopped, false), Tell::WantsPerson);
    }

    #[test]
    fn a_first_try_that_never_opened_devtools_is_told_as_before() {
        let stop = std::sync::atomic::AtomicBool::new(false);
        let why = "the browser did not open DevTools in time";
        let ended = run_tries(&["Edge", "Chrome"], &stop, "abc123", URL, |_| {
            Err(why.into())
        });
        assert_eq!(tell_of(ended, false), Tell::NotPassed(why.into()));
    }
}
