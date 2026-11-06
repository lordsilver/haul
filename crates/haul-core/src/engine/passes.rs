//! Which page the desktop app's browser passes next, out of sight: a row
//! waiting on a page that passes in a browser without a person
//! ([`Engine::page_passes`]) gets it opened, one page at a time and each page
//! once per wait, so the person is asked only when the check wants them.

use std::{collections::HashSet, time::Instant};

use super::{Engine, State, pages::takes_clearance};
use crate::item::Status;

/// What the desktop app's browser is doing, for [`Engine::plan_passes`].
pub struct Browsing<'a> {
    /// Rows whose page it passes out of sight now.
    pub out_of_sight: &'a [String],
    /// Whether a person has row `id`'s page open in Haul's window.
    pub with_person: &'a dyn Fn(&str) -> bool,
    /// Whether it has row `id`'s page, out of sight or before a person.
    pub has_page: &'a dyn Fn(&str) -> bool,
}

/// What the browser should do now.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct PassPlan {
    /// Passes to stop: their row went or stopped waiting (removed, paused,
    /// started by hand), so they hold up no other.
    pub abandon: Vec<String>,
    /// The row whose page to pass next, and the page.
    pub start: Option<(String, String)>,
}

/// A page passed during its row's current wait, and whether the pass was to
/// hand Haul the clearance: one that did is passed once more when Haul is
/// turned away all the same, for what the page shows past the check.
pub(super) type Tried = HashSet<(String, String, bool)>;

/// A row waiting on a page the browser passes by itself.
struct Waiting {
    id: String,
    page: String,
    /// Its pass's turn, rather than shown to the person after it.
    turn: bool,
    clearance: bool,
}

impl Engine {
    /// The browser's next step. A pass that fails to start should be told
    /// with [`Engine::page_wants_person`], which asks the person instead.
    pub fn plan_passes(&self, b: &Browsing<'_>) -> PassPlan {
        let mut st = self.lock();
        let now = Instant::now();
        let waiting = self.waiting_passes(&st, now);
        let key = |w: &Waiting| (w.id.clone(), w.page.clone(), w.clearance);
        // A pass counts while its row waits its turn. Back with the person,
        // the row's next try of its own (the timer sends it back to its
        // turn) is a pass of its own.
        let counted: HashSet<_> = waiting.iter().filter(|w| w.turn).map(key).collect();
        st.tried.retain(|k| counted.contains(k));
        // A page a person has open is theirs: a try of its own coming due
        // waits, so no pass fights their window.
        let with_person: Vec<&str> = waiting
            .iter()
            .map(|w| w.id.as_str())
            .filter(|id| (b.with_person)(id))
            .collect();
        let mut changed = false;
        for id in &with_person {
            changed |= self.with_person(&mut st, id);
        }
        let abandon = b
            .out_of_sight
            .iter()
            .filter(|id| !waiting.iter().any(|w| w.turn && w.id == **id))
            .cloned()
            .collect();
        // One page at a time out of sight: two at once from one address made
        // Cloudflare ask for a person on pages that pass one by one. Any pass
        // still running counts, whatever became of its row, since its browser
        // holds the site's profile until it closes; one shown to the person
        // holds up no other.
        let busy = !b.out_of_sight.is_empty()
            || waiting
                .iter()
                .any(|w| w.turn && st.tried.contains(&key(w)) && (b.has_page)(&w.id));
        let next = (!busy)
            .then(|| {
                waiting.iter().find(|w| {
                    w.turn && !st.tried.contains(&key(w)) && !with_person.contains(&w.id.as_str())
                })
            })
            .flatten();
        let start = next.map(|w| {
            st.tried.insert(key(w));
            (w.id.clone(), w.page.clone())
        });
        if changed {
            self.touch(&mut st);
        }
        PassPlan { abandon, start }
    }

    /// The downloads, then the wrappers, waiting for a person: those under
    /// Needs you, not those whose page the browser passes first.
    pub fn waiting_for_person(&self) -> (Vec<String>, Vec<String>) {
        let st = self.lock();
        let now = Instant::now();
        let (wrappers, downloads): (Vec<_>, Vec<_>) = st
            .items
            .values()
            .filter(|it| it.status == Status::Captcha && !it.tentative)
            .filter(|it| !self.passing(&st, it, now))
            .partition(|it| it.wrapper);
        let ids = |rows: Vec<&crate::item::Item>| rows.iter().map(|it| it.id.clone()).collect();
        (ids(downloads), ids(wrappers))
    }

    /// The rows, downloads and wrappers alike, waiting on a page the browser
    /// passes by itself: in their pass's turn, or shown to the person after
    /// it. Then the files not started whose page is opened only to look them
    /// up ([`Engine::looks_up`]), after every row that waits.
    fn waiting_passes(&self, st: &State, now: Instant) -> Vec<Waiting> {
        let waiting = st
            .items
            .values()
            .filter(|it| it.status == Status::Captcha)
            .filter_map(|it| {
                let page = it
                    .mirrors
                    .iter()
                    .filter(|m| it.manual_urls.contains(*m))
                    .find(|m| self.passes_now(st, m, now))?;
                Some(Waiting {
                    id: it.id.clone(),
                    page: page.clone(),
                    turn: self.passing(st, it, now),
                    clearance: takes_clearance(st, page),
                })
            });
        let looked_up = st.items.values().filter_map(|it| {
            let page = self
                .look_up_page(st, it)
                .filter(|m| self.passes_now(st, m, now))?;
            Some(Waiting {
                id: it.id.clone(),
                page: page.clone(),
                turn: true,
                clearance: takes_clearance(st, page),
            })
        });
        waiting.chain(looked_up).collect()
    }
}
