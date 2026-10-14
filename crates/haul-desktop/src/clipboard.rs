//! Adds links the user copies anywhere, while "Watch the clipboard" is on.
//! Only links to known hosts and to files are taken; see
//! `hosts::is_download_link`.

use std::time::Duration;

use haul_core::Engine;
use tauri::{AppHandle, Manager};

use crate::{MAIN, notify};

pub fn spawn(app: AppHandle, engine: Engine) {
    let started = std::thread::Builder::new()
        .name("clipboard".into())
        .spawn(move || watch(&app, &engine));
    if let Err(e) = started {
        tracing::error!("clipboard watcher: {e}");
    }
}

/// Changes on every copy, so the clipboard is only read when it changed.
#[cfg(windows)]
fn sequence() -> u32 {
    // SAFETY: takes no arguments and only reads a counter.
    unsafe { windows_sys::Win32::System::DataExchange::GetClipboardSequenceNumber() }
}

#[cfg(not(windows))]
fn sequence() -> u32 {
    0
}

fn watch(app: &AppHandle, engine: &Engine) {
    let mut clipboard: Option<arboard::Clipboard> = None;
    // What was on the clipboard last; `None` while the watch is off, so text
    // copied before it was turned on is not taken.
    let mut last: Option<String> = None;
    let mut seen = 0;
    loop {
        std::thread::sleep(Duration::from_millis(700));
        if !engine.watch_clipboard() {
            last = None;
            continue;
        }
        let now = sequence();
        if cfg!(windows) && now == seen && last.is_some() {
            continue;
        }
        seen = now;
        if clipboard.is_none() {
            clipboard = arboard::Clipboard::new().ok();
        }
        let Some(text) = clipboard.as_mut().and_then(|c| c.get_text().ok()) else {
            last.get_or_insert_with(String::new);
            continue;
        };
        let new = last.as_ref().is_some_and(|previous| *previous != text);
        last = Some(text.clone());
        if !new {
            continue;
        }
        let (engine, app) = (engine.clone(), app.clone());
        tauri::async_runtime::spawn(async move {
            let added = engine.add_from_clipboard(&text);
            let count = added.added + added.merged;
            let focused = app
                .get_webview_window(MAIN)
                .is_some_and(|w| w.is_focused().unwrap_or(false));
            if count > 0 && !focused {
                let what = if count == 1 {
                    "1 link".to_string()
                } else {
                    format!("{count} links")
                };
                notify(
                    &app,
                    "Added from the clipboard",
                    &format!("Haul took {what} you copied."),
                );
            }
        });
    }
}
