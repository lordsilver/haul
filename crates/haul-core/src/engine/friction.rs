//! The friction count (plan §4.7): per release, how often a person was
//! asked, how many notifications they were shown, and how many clicks the
//! release needed beyond adding its link and starting it. The engine keeps
//! it for the run, every report shows it (`friction` in `report.json`), each
//! ask is a signal in the log too (`ask`), and the release suite asserts it.
//!
//! A release is what one added link stands for: the row at the top of a
//! row's chain of sources (a release page, a container added on its own, a
//! loose link).

use std::{
    collections::{BTreeMap, BTreeSet},
    time::Instant,
};

use indexmap::IndexMap;
use serde::Serialize;
use serde_json::json;

use super::{Engine, Event, State, Telling, display_name};
use crate::{
    hosts::{self, Need, gates},
    item::{Item, Status, Trouble},
    logging::unix_now,
    parts::PartName,
};

/// One time a person was asked for a release's page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Ask {
    /// When, in seconds since 1970.
    pub at: u64,
    /// The row whose page it was.
    pub row: String,
    /// The page's site.
    pub site: String,
    /// What stood in the way, as the row's trouble says: `check` (a browser
    /// check), `gate` (a captcha past it), `person` (a step only a person
    /// takes), `page` (a page with no recipe) or `other`.
    pub need: &'static str,
    /// The gate by its product, when Haul knows which it is.
    pub gate: Option<&'static str>,
    /// Whether the release needed that page: false when, without it, the
    /// release already had a whole set with a usable link for every part
    /// (§4.7, step 2), or the file another usable link.
    pub needed: bool,
}

/// What one release cost the person.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Friction {
    /// The release's row: its page, container or link as it was added.
    pub release: String,
    /// Its name, as the row last showed it.
    pub name: String,
    pub asks: Vec<Ask>,
    /// Notifications shown about its rows: one shown about rows of several
    /// releases counts for each of them.
    pub notifications: u32,
    /// What the person did beyond adding the link and starting it: a retry
    /// by hand, a link handed back for a row.
    pub clicks: u32,
}

/// The friction of each release, by its row's id.
pub(super) type Frictions = BTreeMap<String, Friction>;

/// How long a release's reading may take before it is told as read anyway.
const READ_AT_MOST: std::time::Duration = std::time::Duration::from_secs(60);

impl Engine {
    /// The releases whose page and sources have been read and whose first
    /// lookups are done (or a minute has passed), each told once: what is
    /// ready, on how many hosts, and the notes of its card.
    pub(super) fn releases_read(&self, st: &mut State, now: Instant) -> Vec<Event> {
        let roots: BTreeSet<String> = st
            .items
            .values()
            .filter(|it| it.wrapper && !it.tentative)
            .map(|it| release_of(&st.items, &it.id))
            .collect();
        st.releases.retain(|root, _| roots.contains(root));
        let cfg = self.cfg();
        let mut events = Vec::new();
        for root in roots {
            let (since, told) = *st.releases.entry(root.clone()).or_insert((now, false));
            if told {
                continue;
            }
            let ours: Vec<&Item> = st
                .items
                .values()
                .filter(|it| release_of(&st.items, &it.id) == root)
                .collect();
            let reading = ours.iter().any(|it| {
                it.wrapper
                    && (matches!(it.status, Status::Queued | Status::Resolving)
                        || it.status == Status::Captcha && self.passing(st, it, now))
            });
            let looking = ours.iter().any(|it| {
                !it.wrapper
                    && (!it.probed && it.name.is_none() || self.look_up_page(st, it).is_some())
            });
            let files = ours.iter().filter(|it| !it.wrapper).count();
            if files == 0 || (reading || looking) && now.duration_since(since) < READ_AT_MOST {
                continue;
            }
            let plan = super::release_plan(st, &cfg, &root);
            let main = plan.main.map_or(plan.sets.first(), |m| plan.sets.get(m));
            let (count, hosts) = match main {
                Some(set) => (
                    format!("{} parts", set.parts),
                    set.hosts.iter().filter(|h| !cfg.is_disabled(h)).count(),
                ),
                None => {
                    let hosts: BTreeSet<&str> = ours
                        .iter()
                        .filter(|it| !it.wrapper)
                        .flat_map(|it| it.mirrors.iter().map(|m| &*m.host))
                        .filter(|h| !cfg.is_disabled(h))
                        .collect();
                    (format!("{files} files"), hosts.len())
                }
            };
            let mut body = format!(
                "{count} ready on {hosts} host{}.",
                if hosts == 1 { "" } else { "s" }
            );
            for split in plan.dead_splits() {
                body.push_str(&format!(" Another copy ({} parts) is gone.", split.parts));
            }
            let root_row = &st.items[&root];
            let name = root_row
                .title
                .clone()
                .unwrap_or_else(|| display_name(root_row));
            crate::debug::emit(
                "release_read",
                json!({ "release": root, "name": name, "body": body }),
            );
            events.push(Event::ReleaseRead {
                id: root.clone(),
                name,
                body,
            });
            if let Some(r) = st.releases.get_mut(&root) {
                r.1 = true;
            }
        }
        events
    }

    /// The friction count of every release this run dealt with.
    pub fn friction(&self) -> Vec<Friction> {
        self.lock().friction.values().cloned().collect()
    }

    /// Which events of a burst become notifications, worded: a page Haul
    /// passes by itself asks nobody yet (its window comes forward, or the
    /// browser asks, if it must), so a row waiting on one is left out.
    pub fn tellings(&self, events: &[Event]) -> Vec<Telling> {
        self.tell_burst(&super::Burst {
            events: events.to_vec(),
            missed: 0,
        })
    }

    /// [`Engine::tellings`] for a gathered burst ([`super::gather`]), with
    /// the events it lost counted.
    pub fn tell_burst(&self, burst: &super::Burst) -> Vec<Telling> {
        let kept: Vec<Event> = burst
            .events
            .iter()
            .filter(|e| {
                !matches!(e, Event::NeedsUser { id, .. }
                    if self.page_for(id).is_some_and(|u| self.page_passes(&u)))
            })
            .cloned()
            .collect();
        super::summarize_burst(&super::Burst {
            events: kept,
            missed: burst.missed,
        })
    }

    /// A notification about rows `rows` was shown: one more for each of
    /// their releases (a row no longer on the list counts for none).
    /// Returns those releases.
    pub fn notified(&self, rows: &[String]) -> Vec<String> {
        let mut st = self.lock();
        let releases: BTreeSet<String> = rows
            .iter()
            .filter(|id| st.items.contains_key(id.as_str()))
            .map(|id| release_of(&st.items, id))
            .collect();
        for release in &releases {
            entry(&mut st, release).notifications += 1;
        }
        releases.into_iter().collect()
    }

    /// The rows these events say wait for a person, and are not first
    /// passed by the browser: each is an ask.
    pub(super) fn asked(&self, events: &[Event]) {
        let rows: Vec<&String> = events
            .iter()
            .filter_map(|e| match e {
                Event::NeedsUser { id, .. } => Some(id),
                _ => None,
            })
            .collect();
        if rows.is_empty() {
            return;
        }
        let mut st = self.lock();
        let now = Instant::now();
        for id in rows {
            let passing = st
                .items
                .get(id.as_str())
                .is_some_and(|it| self.passing(&st, it, now));
            if !passing {
                self.ask_in(&mut st, id);
            }
        }
    }

    /// Row `id`'s page asks a person now: an ask for its release.
    pub(super) fn ask_in(&self, st: &mut State, id: &str) {
        let Some(it) = st.items.get(id) else {
            return;
        };
        let page = it
            .mirrors
            .iter()
            .find(|m| it.manual_urls.contains(m.as_str()))
            .or_else(|| it.mirrors.first())
            .map(|m| m.url.clone())
            .unwrap_or_default();
        let release = release_of(&st.items, id);
        let (need, gate) = what_stood(it);
        let ask = Ask {
            at: unix_now(),
            row: id.to_string(),
            site: hosts::site_of(&page),
            need,
            gate,
            needed: self.needed(st, &release, id, &page),
        };
        crate::debug::emit(
            "ask",
            json!({ "release": release, "id": id, "url": page, "site": ask.site, "need": need, "gate": gate, "needed": ask.needed }),
        );
        entry(st, &release).asks.push(ask);
    }

    /// Whether row `id`'s `page` was needed by `release` when it asked. A
    /// file needs it unless another of its links is usable. A source needs
    /// it unless the release's other rows already hold a whole set: every
    /// part of an archive, each with a usable link (or, with no archive,
    /// every file usable). This is the rule as far as the queue can tell
    /// today; FLOW-02 decides it from the release's sets instead.
    fn needed(&self, st: &State, release: &str, id: &str, page: &str) -> bool {
        let cfg = self.cfg();
        let usable = |it: &Item, but: &str| {
            it.mirrors.iter().any(|m| {
                m.as_str() != but
                    && !it.failed.contains(m.as_str())
                    && !it.gone.contains(m.as_str())
                    && !it.manual_urls.contains(m.as_str())
                    && !cfg.is_disabled(&m.host)
            })
        };
        let Some(asked) = st.items.get(id) else {
            return true;
        };
        if !asked.wrapper {
            return !usable(asked, page);
        }
        let files: Vec<&Item> = st
            .items
            .values()
            .filter(|it| !it.wrapper && !it.tentative && it.id != id && it.name.is_some())
            .filter(|it| release_of(&st.items, &it.id) == release)
            .collect();
        if files.is_empty() {
            return true;
        }
        // Each archive's parts, by number, and whether each is usable.
        let mut sets: BTreeMap<String, BTreeMap<u32, bool>> = BTreeMap::new();
        let mut singles = Vec::new();
        for f in &files {
            match f.name.as_deref().and_then(PartName::parse) {
                Some(p) => {
                    let ok = sets
                        .entry(p.archive)
                        .or_default()
                        .entry(p.number)
                        .or_default();
                    *ok |= usable(f, "");
                }
                None => singles.push(usable(f, "")),
            }
        }
        if sets.is_empty() {
            return !singles.iter().all(|ok| *ok);
        }
        let whole = |parts: &BTreeMap<u32, bool>| {
            let last = parts
                .keys()
                .copied()
                .filter(|n| *n != u32::MAX)
                .max()
                .unwrap_or(0);
            (1..=last).all(|n| parts.get(&n) == Some(&true)) && parts.values().all(|ok| *ok)
        };
        !sets.values().any(whole)
    }
}

/// The person did something for row `id`'s release beyond adding and
/// starting it.
pub(super) fn clicked(st: &mut State, id: &str) {
    if st.items.contains_key(id) {
        let release = release_of(&st.items, id);
        entry(st, &release).clicks += 1;
    }
}

/// The friction of `release`, made when it is the first.
fn entry<'a>(st: &'a mut State, release: &str) -> &'a mut Friction {
    let name = st.items.get(release).map(display_name).unwrap_or_default();
    let f = st
        .friction
        .entry(release.to_string())
        .or_insert_with(|| Friction {
            release: release.to_string(),
            ..Friction::default()
        });
    if !name.is_empty() {
        f.name = name;
    }
    f
}

/// The release row `id` belongs to: the top of its chain of sources.
pub(super) fn release_of(items: &IndexMap<String, Item>, id: &str) -> String {
    let mut at = id;
    // A chain is a few rows long; the bound only guards against a loop.
    for _ in 0..32 {
        match items.get(at).and_then(|it| it.source.as_deref()) {
            Some(up) if items.contains_key(up) => at = up,
            _ => break,
        }
    }
    at.to_string()
}

/// What stood in the way of row `it`, as its trouble says, and the gate by
/// its product when Haul named one.
fn what_stood(it: &Item) -> (&'static str, Option<&'static str>) {
    let need = match it.trouble {
        Trouble::Needs(Need::Check) => "check",
        Trouble::Needs(Need::Gate) => "gate",
        Trouble::Needs(Need::Person) => "person",
        Trouble::Needs(Need::Page) => "page",
        _ => "other",
    };
    (need, gates::named_in(&it.error).map(gates::Gate::name))
}
