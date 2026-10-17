//! Haul desktop app: the engine and its web UI in a native window, with a
//! tray icon, notifications, and an in-app browser for hosts that need a
//! person.
//!
//! The UI is the same page the headless server serves, loaded from the local
//! server this app runs; that server also keeps the HTTP API available to
//! other local tools while the app is open.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod browser;
mod clipboard;
mod passing;
mod tray;

use std::{
    net::Ipv4Addr,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use haul_core::{Engine, Event, Options, Theme, hosts, paths};
use haul_web::Desktop;
use tauri::{
    AppHandle, Manager, RunEvent, WebviewUrl, WebviewWindowBuilder, WindowEvent,
    webview::NewWindowResponse,
};
use tauri_plugin_dialog::{DialogExt, MessageDialogKind};
use tauri_plugin_notification::NotificationExt;
use tokio::{net::TcpListener, sync::broadcast::error::RecvError};

pub const MAIN: &str = "main";

/// The id of the "Needs you" notification; a click on it opens what waits.
const NEEDS_YOU: i32 = 1;

/// The port browser integrations expect; another free one when it is taken.
const PORT: u16 = 8765;

fn main() {
    let data_dir = paths::data_dir();
    let app = tauri::Builder::default()
        // First, so a second launch hands over its arguments and quits early.
        .plugin(tauri_plugin_single_instance::init(|app, args, cwd| {
            handle_args(app, args.into_iter().skip(1), Path::new(&cwd));
            show_main(app);
        }))
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(move |app| {
            // Only the instance that runs gets here, so a second launch
            // leaves this one's log and data alone.
            haul_core::logging::init(&data_dir, false);
            paths::adopt_old_data(&data_dir);
            let handle = app.handle().clone();
            if let Err(message) = tauri::async_runtime::block_on(start(&handle, data_dir)) {
                tracing::error!("{message}");
                app.dialog()
                    .message(message)
                    .title("Haul cannot start")
                    .kind(MessageDialogKind::Error)
                    .show(|_| {
                        std::process::exit(1);
                    });
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            // Closing the main window keeps downloading from the tray.
            if let WindowEvent::CloseRequested { api, .. } = event
                && window.label() == MAIN
            {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .build(tauri::generate_context!())
        .expect("building the app");
    app.run(|app, event| {
        if let RunEvent::Exit = event
            && let Some(engine) = app.try_state::<Engine>()
        {
            tauri::async_runtime::block_on(engine.shutdown());
        }
    });
}

async fn start(app: &AppHandle, data_dir: PathBuf) -> Result<(), String> {
    let engine = Engine::start(Options {
        data_dir,
        download_dir: None,
        config: None,
        recycle_bin: None,
    })
    .await
    .map_err(|e| e.to_string())?;
    // This app passes hosts' pages out of sight (see passing.rs).
    engine.set_pages_pass(true);
    let port = std::env::var("HAUL_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(PORT);
    let listener = match TcpListener::bind((Ipv4Addr::LOCALHOST, port)).await {
        Ok(l) => l,
        Err(e) => {
            tracing::warn!("port {port}: {e}; using another one");
            TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
                .await
                .map_err(|e| e.to_string())?
        }
    };
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    tracing::info!("UI and API on http://127.0.0.1:{port}/");

    let browser = Arc::new(browser::Browser::new(app.clone(), engine.clone()));
    app.manage(browser.clone());
    tauri::async_runtime::spawn(haul_web::serve(
        listener,
        engine.clone(),
        Some(browser.clone()),
    ));
    passing::spawn(app.clone(), engine.clone(), browser);
    app.manage(engine.clone());
    main_window(app, port, engine.theme()).map_err(|e| e.to_string())?;
    tray::create(app, &engine).map_err(|e| e.to_string())?;
    spawn_notification_actions(app.clone());
    spawn_notifications(app.clone(), &engine);
    spawn_theme_sync(app.clone(), &engine);
    clipboard::spawn(app.clone(), engine.clone());
    let cwd = std::env::current_dir().unwrap_or_default();
    handle_args(app, std::env::args().skip(1), &cwd);
    Ok(())
}

/// The window frame's theme; `None` follows the system.
pub fn window_theme(theme: Theme) -> Option<tauri::Theme> {
    match theme {
        Theme::System => None,
        Theme::Light => Some(tauri::Theme::Light),
        Theme::Dark => Some(tauri::Theme::Dark),
    }
}

fn main_window(app: &AppHandle, port: u16, theme: Theme) -> tauri::Result<()> {
    let home = format!("http://127.0.0.1:{port}/");
    let own = home.clone();
    WebviewWindowBuilder::new(
        app,
        MAIN,
        WebviewUrl::External(home.parse().expect("valid URL")),
    )
    .title("Haul")
    .theme(window_theme(theme))
    .inner_size(1180.0, 820.0)
    .min_inner_size(640.0, 480.0)
    // Lets the page take dropped .dlc files and links itself.
    .disable_drag_drop_handler()
    // Anything that is not the UI opens in the user's own browser.
    .on_navigation(move |url| {
        if url.as_str().starts_with(&own) {
            return true;
        }
        open_outside(url);
        false
    })
    .on_new_window(|url, _| {
        open_outside(&url);
        NewWindowResponse::Deny
    })
    .build()?;
    Ok(())
}

fn open_outside(url: &tauri::Url) {
    if matches!(url.scheme(), "http" | "https") {
        let _ = open::that_detached(url.as_str());
    }
}

pub fn show_main(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(MAIN) {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

pub fn notify(app: &AppHandle, title: &str, body: &str) {
    show_notification(app, title, body, false);
}

/// The "Needs you" notification. Its id tells a click on it that a host is
/// waiting, so the click can go straight to the download.
pub fn notify_waiting(app: &AppHandle, title: &str, body: &str) {
    show_notification(app, title, body, true);
}

fn show_notification(app: &AppHandle, title: &str, body: &str, waiting: bool) {
    let builder = app.notification().builder().title(title).body(body);
    let builder = if waiting {
        builder.id(NEEDS_YOU)
    } else {
        builder
    };
    if let Err(e) = builder.show() {
        tracing::debug!("notification: {e}");
    }
}

/// Links and container files given on the command line, or by the file
/// association for `.dlc`.
fn handle_args(app: &AppHandle, args: impl IntoIterator<Item = String>, cwd: &Path) {
    let Some(engine) = app.try_state::<Engine>().map(|e| e.inner().clone()) else {
        return;
    };
    let mut links = String::new();
    let mut files = Vec::new();
    for arg in args {
        if arg.starts_with("http://") || arg.starts_with("https://") {
            links.push_str(&arg);
            links.push('\n');
            continue;
        }
        let path = cwd.join(&arg);
        if !arg.starts_with('-') && path.is_file() {
            files.push(path);
        }
    }
    if links.is_empty() && files.is_empty() {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        // Like links and files dropped on the window: into the Add dialog,
        // unless the settings say to add them at once.
        let review = !engine.cfg().skip_add_dialog;
        for path in files {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let bytes = match tokio::fs::read(&path).await {
                Ok(bytes) => bytes,
                Err(e) => {
                    notify(&app, &format!("Could not import {name}"), &e.to_string());
                    continue;
                }
            };
            if review {
                match engine.read_container(&name, &bytes).await {
                    Ok(found) => {
                        for link in found {
                            links.push_str(&link.url);
                            links.push('\n');
                        }
                    }
                    Err(e) => notify(&app, &format!("Could not import {name}"), &e),
                }
            } else {
                match engine.import(&name, &bytes).await {
                    Ok(added) => tracing::info!("imported {name}: {added:?}"),
                    Err(e) => notify(&app, &format!("Could not import {name}"), &e),
                }
            }
        }
        if !links.is_empty() {
            if review {
                open_add_dialog(&app, &links);
            } else {
                engine.add_text(&links, "");
            }
        }
    });
}

/// Shows the main window with its Add dialog filled in with `links`.
fn open_add_dialog(app: &AppHandle, links: &str) {
    let Some(window) = app.get_webview_window(MAIN) else {
        return;
    };
    let opened = window.url().and_then(|mut url| {
        url.set_fragment(Some(&haul_web::add_route(links)));
        window.navigate(url)
    });
    if let Err(e) = opened {
        tracing::warn!("opening the Add dialog: {e}");
    }
    show_main(app);
}

/// Keeps every window frame on the theme chosen in the app or the file.
fn spawn_theme_sync(app: AppHandle, engine: &Engine) {
    let mut events = engine.subscribe();
    tauri::async_runtime::spawn(async move {
        loop {
            match events.recv().await {
                Ok(Event::ThemeChanged(theme)) => {
                    for window in app.webview_windows().values() {
                        let _ = window.set_theme(window_theme(theme));
                    }
                }
                Ok(_) | Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => return,
            }
        }
    });
}

/// Desktop notifications for finished, failed and stuck downloads, gathered
/// over two seconds and skipped while the window has focus.
fn spawn_notifications(app: AppHandle, engine: &Engine) {
    let mut events = engine.subscribe();
    let engine = engine.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            let first = match events.recv().await {
                Ok(e) => e,
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => return,
            };
            let mut batch = vec![first];
            let window = tokio::time::sleep(Duration::from_secs(2));
            tokio::pin!(window);
            loop {
                tokio::select! {
                    _ = &mut window => break,
                    next = events.recv() => match next {
                        Ok(e) => batch.push(e),
                        Err(RecvError::Lagged(_)) => {}
                        Err(RecvError::Closed) => break,
                    },
                }
            }
            // A page Haul passes by itself asks for nobody, unless its window
            // comes forward (which tells the user then).
            batch.retain(|e| {
                !matches!(e, Event::NeedsUser { id, .. }
                    if engine.page_for(id).is_some_and(|u| hosts::page_passes(&u)))
            });
            let focused = app
                .get_webview_window(MAIN)
                .is_some_and(|w| w.is_focused().unwrap_or(false));
            if !focused {
                for (title, body, waiting) in summarize(&batch) {
                    if waiting {
                        notify_waiting(&app, &title, &body);
                    } else {
                        notify(&app, &title, &body);
                    }
                }
            }
        }
    });
}

/// A click on a notification brings Haul forward. A click on "Needs you" also
/// lands on the list that waits, and with a single download waiting opens its
/// page right away: that page is the step the user was asked for.
fn spawn_notification_actions(app: AppHandle) {
    let _ = app.clone().notification().on_action(move |performed| {
        if performed.action_id() != "tap" {
            return;
        }
        let waiting = performed
            .notification()
            .is_some_and(|n| n.id() == NEEDS_YOU);
        reveal_waiting(&app, waiting);
    });
}

fn reveal_waiting(app: &AppHandle, from_waiting: bool) {
    let Some(engine) = app.try_state::<Engine>().map(|e| e.inner().clone()) else {
        return;
    };
    let (waiting, wrappers): (Vec<String>, Vec<String>) = if from_waiting {
        let s = engine.snapshot();
        let downloads = s.items.iter().filter(|it| it.status == "captcha");
        let wrappers = s.wrappers.iter().filter(|w| w.status == "captcha");
        (
            downloads.map(|it| it.id.clone()).collect(),
            wrappers.map(|w| w.id.clone()).collect(),
        )
    } else {
        Default::default()
    };
    // Haul opens on the list that waits, not on whatever was shown last:
    // the waiting downloads, or the wrappers when only those wait.
    let place = match (waiting.is_empty(), wrappers.is_empty()) {
        (false, _) => Some("/attention"),
        (true, false) => Some("/wrappers"),
        (true, true) => None,
    };
    if let Some(place) = place
        && let Some(window) = app.get_webview_window(MAIN)
        && let Ok(mut url) = window.url()
    {
        url.set_fragment(Some(place));
        let _ = window.navigate(url);
    }
    let waiting: Vec<String> = waiting.into_iter().chain(wrappers).collect();
    show_main(app);
    // One download waiting: the click meant that one, so open its page too.
    if let [id] = waiting.as_slice()
        && let Some(browser) = app.try_state::<Arc<browser::Browser>>()
        && let Some(page) = engine.page_for(id)
        && let Err(e) = browser.browse(id, &page, None)
    {
        tracing::warn!("opening {page}: {e}");
    }
}

fn summarize(events: &[Event]) -> Vec<(String, String, bool)> {
    let mut finished = Vec::new();
    let mut waiting = Vec::new();
    let mut failed = Vec::new();
    for e in events {
        match e {
            Event::Finished { name, .. } => finished.push(name.as_str()),
            Event::NeedsUser { name, .. } => waiting.push(name.as_str()),
            Event::Failed { name, error, .. } => failed.push(format!("{name}: {error}")),
            Event::ThemeChanged(_) => {}
        }
    }
    let list = |names: &[&str]| {
        let shown = names.iter().take(3).copied().collect::<Vec<_>>().join(", ");
        if names.len() > 3 {
            format!("{shown} and {} more", names.len() - 3)
        } else {
            shown
        }
    };
    let mut out = Vec::new();
    match finished.len() {
        0 => {}
        1 => out.push(("Downloaded".into(), finished[0].to_string(), false)),
        n => out.push((format!("{n} downloads finished"), list(&finished), false)),
    }
    if !waiting.is_empty() {
        out.push((
            "Needs you".into(),
            format!(
                "{}: open it in Haul to solve the host's page",
                list(&waiting)
            ),
            true,
        ));
    }
    match failed.len() {
        0 => {}
        1 => out.push(("Download failed".into(), failed[0].clone(), false)),
        n => out.push((
            format!("{n} downloads failed"),
            failed
                .iter()
                .take(3)
                .cloned()
                .collect::<Vec<_>>()
                .join("\n"),
            false,
        )),
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_waiting_download_is_marked_as_needing_the_user() {
        let events = [
            Event::NeedsUser {
                id: "waiting".into(),
                name: "a.bin".into(),
            },
            Event::Finished {
                id: "done".into(),
                name: "b.bin".into(),
            },
        ];
        let out = summarize(&events);
        let waiting: Vec<_> = out.iter().filter(|(_, _, waiting)| *waiting).collect();
        assert_eq!(waiting.len(), 1, "{out:?}");
        assert_eq!(waiting[0].0, "Needs you");
        assert!(waiting[0].1.contains("a.bin"));
        assert!(out.iter().any(|(title, _, _)| title == "Downloaded"));
    }

    #[test]
    fn the_data_folder_follows_the_app_identifier() {
        let conf = include_str!("../tauri.conf.json");
        let id = format!("\"identifier\": \"{}\"", haul_core::paths::APP_ID);
        assert!(
            conf.contains(&id),
            "tauri.conf.json and paths::APP_ID disagree"
        );
    }
}
