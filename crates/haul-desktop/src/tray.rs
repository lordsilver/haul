//! The tray icon: the app keeps downloading while its window is closed.

use std::time::Duration;

use haul_core::Engine;
use tauri::{
    AppHandle,
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
};

use crate::show_main;

pub fn create(app: &AppHandle, engine: &Engine) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "show", "Show Haul", true, None::<&str>)?;
    let pause = MenuItem::with_id(app, "pause", "Pause all", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit Haul", true, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[&show, &pause, &PredefinedMenuItem::separator(app)?, &quit],
    )?;
    let toggle = engine.clone();
    let mut builder = TrayIconBuilder::with_id("main")
        .tooltip("Haul")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(move |app, event| match event.id.as_ref() {
            "show" => show_main(app),
            "pause" => toggle.set_paused(!toggle.snapshot().paused),
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    let tray = builder.build(app)?;

    let engine = engine.clone();
    tauri::async_runtime::spawn(async move {
        let mut shown = (String::new(), false);
        loop {
            tokio::time::sleep(Duration::from_secs(2)).await;
            let s = engine.snapshot();
            let active = s.items.iter().filter(|i| i.status == "downloading").count();
            let tip = if active == 0 {
                "Haul".to_string()
            } else {
                format!("Haul: {active} downloading at {:.1} MB/s", s.speed / 1e6)
            };
            if tip != shown.0 {
                let _ = tray.set_tooltip(Some(&tip));
            }
            if s.paused != shown.1 {
                let _ = pause.set_text(if s.paused { "Resume all" } else { "Pause all" });
            }
            shown = (tip, s.paused);
        }
    });
    Ok(())
}
