//! Pages Haul passes by itself: a download waiting on a host whose check
//! passes in a browser without a person ([`Engine::page_passes`]: a host's
//! page known to, or one behind Cloudflare's check) gets its page opened
//! out of sight ([`Browser::pass`]), [`AT_ONCE`] at a time and once per
//! wait, so the user is asked only when the check wants them.

use std::{collections::HashSet, sync::Arc, time::Duration};

use haul_core::Engine;

use crate::browser::Browser;

/// Pages passed out of sight at once: each in a browser of its own.
const AT_ONCE: usize = 2;

pub fn spawn(engine: Engine, browser: Arc<Browser>) {
    let mut changes = engine.watch();
    tauri::async_runtime::spawn(async move {
        // Downloads whose page was opened during their current wait.
        let mut tried: HashSet<String> = HashSet::new();
        loop {
            let snapshot = engine.snapshot();
            // Downloads and wrappers (pages and pastes) alike.
            let rows = snapshot
                .items
                .into_iter()
                .map(|i| (i.id, i.status, i.open_urls))
                .chain(
                    snapshot
                        .wrappers
                        .into_iter()
                        .map(|w| (w.id, w.status, w.open_urls)),
                );
            let waiting: Vec<(String, String, bool)> = rows
                // Waiting their turn ("passing"), or shown to the person after
                // their turn ("captcha"): kept, so the tried set stays theirs.
                .filter(|(_, status, _)| *status == "passing" || *status == "captcha")
                .filter_map(|(id, status, open)| {
                    let page = open.iter().find(|u| engine.page_passes(u))?;
                    Some((id, page.clone(), status == "passing"))
                })
                .collect();
            tried.retain(|id| waiting.iter().any(|(w, ..)| w == id));
            // A few pages at a time out of sight; one shown to the person
            // (its check wanted them) holds up no other.
            let busy = waiting
                .iter()
                .filter(|(id, _, passing)| *passing && tried.contains(id) && browser.is_passing(id))
                .count();
            if busy < AT_ONCE
                && let Some((id, page, _)) = waiting
                    .iter()
                    .find(|(id, _, passing)| *passing && !tried.contains(id))
            {
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
