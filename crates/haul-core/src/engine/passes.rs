//! Which page the desktop app's browser passes next, out of sight: a row
//! waiting on a page that passes in a browser without a person
//! ([`Engine::page_passes`]) gets it opened, one page at a time and each page
//! once per wait, so the person is asked only when the check wants them.

use std::{
    collections::{HashMap, HashSet},
    time::Instant,
};

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

/// The rows waiting their turn for a pass, in their order: each row's page
/// and since when it waits. Kept between plans, so the log says when a row
/// joined the queue, its place, and when its pass started or it left.
pub(super) type Queue = Vec<(String, String, Instant)>;

/// A row waiting on a page the browser passes by itself.
struct Waiting {
    id: String,
    page: String,
    /// Its pass's turn, rather than shown to the person after it.
    turn: bool,
    clearance: bool,
    /// Opened only to look its file up.
    lookup: bool,
}

/// Pages passed out of sight at once, each on a site of its own.
pub const PASSES_AT_ONCE: usize = 3;

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
        // A few pages at a time out of sight, never two of one site: two at
        // once on one site made Cloudflare ask for a person on pages that
        // pass one by one, and a site's pages share its browser profile. A
        // walk takes a minute or two, so links on different sites are not
        // made to wait for each other. A pass still running whose row went
        // counts for every site, since its browser holds a profile until it
        // closes; one shown to the person holds up no other.
        let running: Vec<&Waiting> = waiting
            .iter()
            .filter(|w| w.turn && st.tried.contains(&key(w)) && (b.has_page)(&w.id))
            .collect();
        let rowless = b
            .out_of_sight
            .iter()
            .any(|id| !running.iter().any(|w| w.id == *id));
        let sites: HashSet<String> = running
            .iter()
            .map(|w| crate::hosts::site_of(&w.page))
            .collect();
        let busy = rowless || running.len().max(b.out_of_sight.len()) >= PASSES_AT_ONCE;
        // A page opened only to look a file up waits for its host's turn
        // (plan LOOK-01).
        let due = |w: &Waiting| !w.lookup || st.budgets.ready(&crate::hosts::host_of(&w.page), now);
        let next = (!busy)
            .then(|| {
                waiting.iter().find(|w| {
                    w.turn
                        && !st.tried.contains(&key(w))
                        && !with_person.contains(&w.id.as_str())
                        && !sites.contains(&crate::hosts::site_of(&w.page))
                        && due(w)
                })
            })
            .flatten();
        let start = next.map(|w| {
            st.tried.insert(key(w));
            (w.id.clone(), w.page.clone())
        });
        // A lookup's pass takes its host's turn.
        if let Some(w) = next.filter(|w| w.lookup) {
            st.budgets
                .of(&crate::hosts::host_of(&w.page), now)
                .take(now);
        }
        let queued: Vec<&Waiting> = waiting
            .iter()
            .filter(|w| w.turn && !st.tried.contains(&key(w)))
            .filter(|w| !with_person.contains(&w.id.as_str()))
            .collect();
        tell_queue(&mut st.pass_queue, &queued, start.as_ref(), now);
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
            .filter(|it| it.status == Status::Captcha && !it.tentative && !it.unread)
            .filter(|it| !matches!(it.trouble, crate::item::Trouble::Browser(_)))
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
        // The rows that wait on each page, found once for every page rather
        // than among every row for each.
        let mut waiting_on: HashMap<&str, Vec<&crate::item::Item>> = HashMap::new();
        for it in st.items.values() {
            for page in &it.manual_urls {
                waiting_on.entry(page.as_str()).or_default().push(it);
            }
        }
        let passes_now = |page: &str| {
            let rows = waiting_on.get(page).into_iter().flatten().copied();
            self.passes_now_among(st, page, now, rows)
        };
        let waiting = st
            .items
            .values()
            .filter(|it| it.status == Status::Captcha)
            .filter_map(|it| {
                let page = it
                    .mirrors
                    .iter()
                    .filter(|m| it.manual_urls.contains(m.as_str()))
                    .find(|m| passes_now(m))?;
                Some(Waiting {
                    id: it.id.clone(),
                    page: page.url.clone(),
                    turn: self.passing(st, it, now),
                    clearance: takes_clearance(st, page),
                    lookup: false,
                })
            });
        // Lookups by need (plan LOOK-05): first those of releases with a
        // part no live link serves, then the rest.
        let mut plans: HashMap<String, bool> = HashMap::new();
        let mut needed = |it: &crate::item::Item| {
            let root = super::friction::release_of(&st.items, &it.id);
            *plans.entry(root.clone()).or_insert_with(|| {
                let plan = super::release_plan(st, &self.cfg(), &root);
                let main = plan.main.map_or(plan.sets.first(), |m| plan.sets.get(m));
                main.is_none_or(|set| set.ids.len() < set.parts as usize)
            })
        };
        let mut looked_up: Vec<(bool, Waiting)> = st
            .items
            .values()
            .filter_map(|it| {
                let page = self.look_up_page(st, it).filter(|m| passes_now(m))?;
                Some((
                    needed(it),
                    Waiting {
                        id: it.id.clone(),
                        page: page.to_string(),
                        turn: true,
                        clearance: takes_clearance(st, page),
                        lookup: true,
                    },
                ))
            })
            .collect();
        looked_up.sort_by_key(|(needed, _)| !*needed);
        let looked_up = looked_up.into_iter().map(|(_, w)| w);
        waiting.chain(looked_up).collect()
    }
}

/// Signals what changed in the pass queue since the last plan (`pass_queue`
/// in the log): rows that joined it, with their place, the row whose pass
/// starts, with how long it waited, and rows that left it unpassed.
fn tell_queue(
    queue: &mut Queue,
    queued: &[&Waiting],
    start: Option<&(String, String)>,
    now: Instant,
) {
    use serde_json::json;

    let of = queued.len();
    let was = |id: &str| {
        queue
            .iter()
            .find(|(q, ..)| q == id)
            .map(|(_, _, since)| *since)
    };
    let mut next: Queue = Vec::with_capacity(of);
    for (place, w) in queued.iter().enumerate() {
        let since = was(&w.id).unwrap_or_else(|| {
            crate::debug::emit(
                "pass_queue",
                json!({ "event": "queued", "id": w.id, "url": w.page, "place": place + 1, "of": of }),
            );
            now
        });
        next.push((w.id.clone(), w.page.clone(), since));
    }
    if let Some((id, url)) = start {
        // A row whose pass starts as it comes is first in the queue too.
        if was(id).is_none() {
            crate::debug::emit(
                "pass_queue",
                json!({ "event": "queued", "id": id, "url": url, "place": 1, "of": of + 1 }),
            );
        }
        let waited = was(id).map_or(0, |since| now.duration_since(since).as_secs());
        crate::debug::emit(
            "pass_queue",
            json!({ "event": "started", "id": id, "url": url, "waited_secs": waited, "behind": of }),
        );
    }
    for (id, url, _) in queue.iter() {
        let gone = !next.iter().any(|(q, ..)| q == id) && start.is_none_or(|(s, _)| s != id);
        if gone {
            crate::debug::emit(
                "pass_queue",
                json!({ "event": "left", "id": id, "url": url }),
            );
        }
    }
    *queue = next;
}
