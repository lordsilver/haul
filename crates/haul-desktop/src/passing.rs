//! Pages Haul passes by itself: a download waiting on a host whose check
//! passes in a browser without a person ([`Engine::page_passes`]: a host's
//! page known to, or one behind Cloudflare's check) gets its
//! page opened out of sight ([`Browser::pass`]), one page at a time and
//! each page once per wait, so the user is asked only when the check wants
//! them. The engine sends a page that wanted them back here on its timer
//! ([`Engine::page_wants_person`]), unless they have it open.

use std::{collections::HashSet, sync::Arc, time::Duration};

use haul_core::Engine;

use crate::browser::Browser;

pub fn spawn(engine: Engine, browser: Arc<Browser>) {
    let mut changes = engine.watch();
    tauri::async_runtime::spawn(async move {
        // Downloads and the page of theirs opened during their current wait:
        // when that page fails (a deleted folder), the next one it waits for
        // is opened in turn. A pass that handed Haul the clearance for a page
        // counts for that only: when Haul is turned away all the same, the
        // page is opened once more, for what it shows past the check.
        let mut tried: HashSet<(String, String, bool)> = HashSet::new();
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
            let waiting: Vec<(String, String, bool, bool)> = rows
                // Waiting their turn ("passing"), or shown to the person after
                // their turn ("captcha").
                .filter(|(_, status, _)| *status == "passing" || *status == "captcha")
                .filter_map(|(id, status, open)| {
                    let page = open.iter().find(|u| engine.page_passes(u))?;
                    let clearance = engine.takes_clearance(page);
                    Some((id, page.clone(), status == "passing", clearance))
                })
                .collect();
            let key =
                |id: &String, page: &String, clearance: bool| (id.clone(), page.clone(), clearance);
            // A pass counts while its row waits its turn. Back with the
            // person, the row's next try of its own (the engine's timer sends
            // it back to passing) is a pass of its own.
            tried.retain(|(id, page, c)| {
                waiting
                    .iter()
                    .any(|(w, p, passing, wc)| *passing && w == id && p == page && wc == c)
            });
            // A page a person has open is theirs: a try of its own coming due
            // waits, here and in the engine, so no pass fights their window.
            let with_person: Vec<&String> = waiting
                .iter()
                .map(|(id, ..)| id)
                .filter(|id| browser.person_has_page(id))
                .collect();
            for id in &with_person {
                engine.page_with_person(id);
            }
            // A pass whose row went or stopped waiting (removed, paused,
            // started by hand) stops, so it holds up no other.
            for id in browser.passing_out_of_sight() {
                if !waiting
                    .iter()
                    .any(|(w, _, passing, _)| *w == id && *passing)
                {
                    browser.abandon(&id);
                }
            }
            // One page at a time out of sight: two at once from one address
            // made Cloudflare ask for a person on pages that pass one by
            // one. Any pass still running counts, whatever became of its
            // row, since its browser holds the site's profile until it
            // closes; one shown to the person (its check wanted them) holds
            // up no other.
            let busy = !browser.passing_out_of_sight().is_empty()
                || waiting.iter().any(|(id, page, passing, c)| {
                    *passing && tried.contains(&key(id, page, *c)) && browser.is_passing(id)
                });
            if !busy
                && let Some((id, page, _, c)) = waiting.iter().find(|(id, page, passing, c)| {
                    *passing && !tried.contains(&key(id, page, *c)) && !with_person.contains(&id)
                })
            {
                tried.insert(key(id, page, *c));
                // A pass that cannot start asks the person rather than
                // leaving the row passing for good.
                if let Err(e) = browser.pass(id, page) {
                    tracing::warn!(item = %id, "opening {page}: {e}");
                    engine.page_wants_person(id);
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
