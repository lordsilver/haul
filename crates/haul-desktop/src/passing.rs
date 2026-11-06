//! Pages Haul passes by itself: the engine says which page goes next
//! ([`Engine::plan_passes`]) and which passes to stop, and this opens them
//! out of sight ([`Browser::pass`]). The engine sends a page that wanted a
//! person back here on its timer ([`Engine::page_wants_person`]), unless they
//! have it open.

use std::{sync::Arc, time::Duration};

use haul_core::{Browsing, Engine};

use crate::browser::Browser;

pub fn spawn(engine: Engine, browser: Arc<Browser>) {
    let mut changes = engine.watch();
    tauri::async_runtime::spawn(async move {
        loop {
            let out_of_sight = browser.passing_out_of_sight();
            let plan = engine.plan_passes(&Browsing {
                out_of_sight: &out_of_sight,
                with_person: &|id| browser.person_has_page(id),
                has_page: &|id| browser.is_passing(id),
            });
            for id in plan.abandon {
                browser.abandon(&id);
            }
            // A pass that cannot start asks the person rather than leaving
            // the row passing for good.
            if let Some((id, page)) = plan.start
                && let Err(e) = browser.pass(&id, &page)
            {
                tracing::warn!(item = %id, "opening {page}: {e}");
                engine.page_wants_person(&id);
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
