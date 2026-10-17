//! Pages Haul passes by itself: a download waiting on a host whose check
//! passes in a browser without a person ([`hosts::page_passes`]) gets its
//! page opened out of sight ([`Browser::pass`]), one page at a time and
//! once per wait, so the user is asked only when the check wants them.

use std::{collections::HashSet, sync::Arc, time::Duration};

use haul_core::{Engine, hosts};

use crate::browser::Browser;

pub fn spawn(engine: Engine, browser: Arc<Browser>) {
    let mut changes = engine.watch();
    tauri::async_runtime::spawn(async move {
        // Downloads whose page was opened during their current wait.
        let mut tried: HashSet<String> = HashSet::new();
        loop {
            let waiting: Vec<(String, String)> = engine
                .snapshot()
                .items
                .into_iter()
                .filter(|i| i.status == "captcha")
                .filter_map(|i| {
                    let page = i.open_urls.iter().find(|u| hosts::page_passes(u))?;
                    Some((i.id, page.clone()))
                })
                .collect();
            tried.retain(|id| waiting.iter().any(|(w, _)| w == id));
            let open = |id: &str| browser.is_passing(id);
            let busy = tried.iter().any(|id| open(id));
            if !busy && let Some((id, page)) = waiting.iter().find(|(id, _)| !tried.contains(id)) {
                tried.insert(id.clone());
                if let Err(e) = browser.pass(id, page) {
                    tracing::warn!(item = %id, "opening {page}: {e}");
                }
            }
            // A closed window changes nothing in the engine: look again
            // now and then too.
            tokio::select! {
                changed = changes.changed() => if changed.is_err() { return },
                () = tokio::time::sleep(Duration::from_secs(5)) => {}
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    });
}
