//! What the views are sent: the state as one picture, taken under the lock
//! and filled in with what `views.rs` works out from it.

use std::{
    collections::{HashMap, HashSet},
    time::Instant,
};

use serde::Serialize;

use super::{
    ArchiveView, BIN, Command, Counts, Engine, Exit, HostStats, PoolSummary, ReconnectView,
    ServicesView, WrapperCard, display_name, unpack,
    views::{self, HostGroup, List, RowState, SiteKind, WrapperState},
};
use crate::{
    config::{Route, Theme},
    hosts,
    item::{Extract, Found, Item, Status, Trouble, Verifying},
};

/// The addresses in use: this machine's line, proxies of the settings and
/// a VPN's servers. One pool; the status bar switches the proxies and the
/// VPN off together, each source also has a switch of its own.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, ts_rs::TS)]
#[ts(export)]
pub struct PoolView {
    /// Downloads may leave through this machine's own line.
    pub line: bool,
    pub proxies: usize,
    pub vpn: usize,
    /// The status bar's switch (`use_proxy`).
    pub on: bool,
}

#[derive(Debug, Serialize, ts_rs::TS)]
#[ts(export)]
pub struct Snapshot {
    pub items: Vec<ItemView>,
    /// The archives in parts among the items.
    pub archives: Vec<ArchiveView>,
    pub wrappers: Vec<WrapperView>,
    /// The wrappers as the Wrappers view shows them, one card per set of files.
    pub wrapper_cards: Vec<WrapperCard>,
    pub counts: Counts,
    pub hosts: Vec<HostView>,
    pub paused: bool,
    /// Bytes per second over every transfer.
    pub speed: f64,
    pub max_total: usize,
    /// Global cap in bytes per second; 0 means none.
    pub rate: f64,
    pub proxy: bool,
    /// The addresses downloads go out through, for the status bar.
    pub pool: PoolView,
    /// The pool in words, and each source of addresses.
    pub pool_summary: PoolSummary,
    pub theme: Theme,
    pub watch_clipboard: bool,
    /// Removing a finished download moves its file to the [`BIN`].
    pub recycle_on_remove: bool,
    /// Dropped links and files are added at once, without the Add dialog.
    pub skip_add_dialog: bool,
    /// [`BIN`], for the views to name it.
    pub bin: &'static str,
    /// Moves whenever a setting changes, so views can fetch them again.
    pub config_version: u64,
    /// Why the settings file on disk is not in effect.
    pub config_error: Option<String>,
    pub download_dir: String,
    pub data_dir: String,
    /// The settings file, when the engine was started from one.
    pub config_file: Option<String>,
    /// The hosts that take an account, and what each asks for.
    pub accounts: Vec<hosts::AccountForm>,
    /// Getting a new address from the router.
    pub reconnect: ReconnectView,
    /// VPN services whose servers join the proxies.
    pub services: ServicesView,
    /// A newer Haul, when GitHub has one.
    pub update: Option<crate::update::UpdateView>,
}

#[derive(Debug, Serialize, ts_rs::TS)]
#[ts(export)]
pub struct ItemView {
    pub id: String,
    pub name: String,
    pub package: String,
    pub status: RowState,
    pub error: String,
    pub size: u64,
    pub done: u64,
    pub speed: f64,
    pub host: String,
    /// Host of every mirror.
    pub mirrors: Vec<String>,
    /// The host the user picked to download it from, when they did.
    pub download_from: Option<String>,
    /// A copied page being looked at for links: in no list until it shows
    /// some ([`Engine::add_from_clipboard`]).
    pub tentative: bool,
    /// A wrapper's file nothing named yet, whose page the desktop app opens
    /// out of sight to learn what it is ([`Engine::looks_up`]): in no list,
    /// as a page on trial, until it is named (and joins the same file on
    /// other hosts), found gone (and leaves), or its page told nothing.
    pub looking_up: bool,
    /// Its place among the pages waiting for Haul's browser, while it waits.
    pub queue: Option<InQueue>,
    /// Seconds until a waiting item may try again.
    pub wait: u64,
    /// Seconds until a failed item, or the page of one under Needs you, is
    /// tried again on its own; 0 when never.
    pub retry_in: u64,
    /// Mirrors a person has to open.
    pub open_urls: Vec<String>,
    /// Their hosts, in the same order.
    pub open_hosts: Vec<String>,
    /// Where the finished file was saved.
    pub path: Option<String>,
    /// Finished without downloading: the file was in the download folder.
    pub existing: bool,
    /// Comparing a file on disk with the host's checksum.
    pub verifying: Verifying,
    /// The wrapper it came from.
    pub source: Option<String>,
    /// Other wrappers that list it too.
    pub also_from: Vec<String>,
    /// Unpacking the archive in parts it is one of.
    pub extract: Extract,
    /// Why unpacking it failed.
    pub extract_error: String,
    /// How far unpacking is, between 0 and 1, while it runs.
    pub extract_progress: Option<f32>,
    /// The list it is in beside All downloads; none for a copied page on
    /// trial.
    pub list: Option<List>,
    /// Paused before any byte came: what a wrapper's files are until
    /// started.
    pub not_started: bool,
    /// The host it downloads from, or before it starts the one it would
    /// try first.
    pub shown_host: String,
    /// That host is a preferred one.
    pub preferred_host: bool,
    /// Other hosts that are on and could take over.
    pub other_hosts: usize,
    /// Its error without `shown_host`'s name in front.
    pub error_shown: String,
    /// What the row says after its host: why it failed, waits or wants a
    /// person, or what its host is slowing; empty for none.
    pub reason: String,
    /// Seconds until its page is passed again on its own while it waits for
    /// a person; 0 when never.
    pub again: u64,
    /// The hosts whose link failed for it, or says the file is gone: not among
    /// the hosts that could take over.
    pub failed_hosts: Vec<String>,
    /// Every one of its hosts says the file is no more: not tried again.
    pub gone: bool,
    /// The host limiting its download and why it stays there, for the row to
    /// say instead of a time left that means nothing.
    pub slowed: Option<String>,
    /// What its buttons do.
    pub commands: Vec<Command>,
    /// The key of the archive in parts it is one of.
    pub archive: Option<String>,
    /// The name of the wrapper it came from.
    pub source_name: Option<String>,
}

/// A wrapper link: a page or container of further links, which unwrapping
/// turns into files that wait for the user to start them.
#[derive(Debug, Serialize, ts_rs::TS)]
#[ts(export)]
pub struct WrapperView {
    pub id: String,
    /// The link as added.
    pub url: String,
    pub host: String,
    /// The page's own name for what it holds, once unwrapped.
    pub title: Option<String>,
    pub package: String,
    pub status: WrapperState,
    pub error: String,
    /// Seconds until it is tried again.
    pub wait: u64,
    /// Seconds until a failed one, or the page of one waiting for a person,
    /// is tried again on its own; 0 when never.
    pub retry_in: u64,
    /// Pages a person has to open.
    pub open_urls: Vec<String>,
    /// Their hosts, in the same order.
    pub open_hosts: Vec<String>,
    /// What unwrapping found.
    pub found: Option<Found>,
    /// The hosts its links are on.
    pub hosts: Vec<String>,
    /// The wrapper it was found on (a paste a download page links).
    pub source: Option<String>,
    /// A paste that lists no links: on a card with others, a note.
    pub lists_nothing: bool,
    /// Its host says what it stands for is no more: not tried again.
    pub gone: bool,
    /// Its place among the pages waiting for Haul's browser, while it waits.
    pub queue: Option<InQueue>,
    /// Set aside unread: its gate did not pass by itself, and its release is
    /// whole without it.
    pub unread: bool,
}

/// A place in the queue of pages waiting for Haul's browser: 3rd of 6.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ts_rs::TS)]
#[ts(export)]
pub struct InQueue {
    pub place: u32,
    pub of: u32,
}

/// A host's daily allowance at full speed, over the addresses it was asked
/// from.
#[derive(Debug, Clone, Serialize, PartialEq, ts_rs::TS)]
#[ts(export)]
pub struct QuotaView {
    /// Bytes per address per day at full speed.
    pub limit: u64,
    /// The least used of the addresses: what the next download gets.
    pub used: u64,
    /// Addresses past it, and the speed they get.
    pub slowed: usize,
    pub slowed_to: u64,
    /// Addresses it was asked from.
    pub known: usize,
}

#[derive(Debug, Serialize, ts_rs::TS)]
#[ts(export)]
pub struct HostView {
    pub name: String,
    pub active: usize,
    pub speed: f64,
    pub fails: u32,
    pub ok: u32,
    pub bytes: u64,
    /// Answered 403/451 through the proxy, so it may block VPN addresses.
    pub blocked: bool,
    pub route: Route,
    pub cooldown: u64,
    /// Subfolder of the download folder for its files; empty for none.
    pub folder: String,
    /// Where it stands among the preferred hosts, 0 being the first.
    pub preferred: Option<usize>,
    /// Turned off by the user: its links are never tried.
    pub disabled: bool,
    /// Its account isn't set ([`hosts::Known::last_resort`]): its links are
    /// used only when no other host has the file.
    pub needs_account: bool,
    /// Addresses it may be reached from: its slots and its learned limit
    /// count for each.
    pub exits: usize,
    /// What it allows at full speed per address today, when it says.
    pub quota: Option<QuotaView>,
    /// What a free user gets from it, where Haul knows.
    pub free: Option<hosts::Free>,
    /// The user set the account it takes, which may lift that.
    pub account: bool,
    /// For a host that gives no decent download without its account: whether
    /// the user has it used like any other host without one; `null` for
    /// every other host.
    pub accountless: Option<bool>,
    /// The most downloads it takes at once, learned when it refused one more
    /// than it was serving; `None` until it does.
    pub cap: Option<usize>,
    /// What the site is to Haul; a `site` is listed only when the user
    /// starred or switched it off.
    pub kind: SiteKind,
    /// The Hosts view lists it: any but a `site` the user gave no setting.
    pub listed: bool,
    /// The group the Hosts view shows it in: a host's `preferred` or
    /// `other` (on or off alike), or its kind for the others.
    pub group: HostGroup,
    /// It can trade places with another preferred host.
    pub movable: bool,
}

fn quota_view(quotas: impl Iterator<Item = hosts::Allowance>) -> Option<QuotaView> {
    let quotas: Vec<_> = quotas.collect();
    let first = quotas.first()?;
    Some(QuotaView {
        limit: first.limit,
        used: quotas.iter().map(|q| q.used).min().unwrap_or(0),
        slowed: quotas.iter().filter(|q| q.slowed()).count(),
        slowed_to: first.slowed_to,
        known: quotas.len(),
    })
}

/// Where download `it` stands, as the views show it; `passing`: the desktop
/// app passes its page first ([`Engine::passing`]).
fn row_state(it: &Item, passing: bool) -> RowState {
    match it.status {
        Status::Queued if !it.probed => RowState::Checking,
        Status::Captcha if passing => RowState::Passing,
        other => other.into(),
    }
}

fn wrapper_state(it: &Item, passing: bool) -> WrapperState {
    match it.status {
        Status::Queued | Status::Resolving | Status::Downloading => WrapperState::Unwrapping,
        // A page or paste behind Cloudflare's check, say.
        Status::Captcha if passing => WrapperState::Passing,
        Status::Captcha => WrapperState::Captcha,
        Status::Waiting => WrapperState::Waiting,
        Status::Paused => WrapperState::Paused,
        Status::Error => WrapperState::Error,
        Status::Done => WrapperState::Done,
    }
}

/// One row as debug mode's status signals name it ([`Engine::row_changes`]).
#[derive(Debug)]
pub struct RowStatus {
    /// `item` or `wrapper`.
    pub row: &'static str,
    pub id: String,
    pub name: String,
    /// Its state, as the views name it.
    pub state: &'static str,
    pub error: String,
}

/// What [`Engine::row_changes`] found.
#[derive(Debug, Default)]
pub struct RowChanges {
    /// Rows whose state differs from before, with the state they had (none
    /// for a row that is new).
    pub changed: Vec<(RowStatus, Option<&'static str>)>,
    /// Rows taken off the list: id, name and the state they had.
    pub removed: Vec<(String, String, &'static str)>,
}

impl Engine {
    /// The rows whose state differs from `before` (each row's state and name,
    /// by id), with `before` brought up to date. Nothing is copied for a row
    /// that did not change, so this costs little on every state change.
    pub fn row_changes(&self, before: &mut HashMap<String, (&'static str, String)>) -> RowChanges {
        let st = self.lock();
        let now = Instant::now();
        let mut out = RowChanges::default();
        for it in st.items.values() {
            let passing = self.passing(&st, it, now);
            let state = if it.wrapper {
                wrapper_state(it, passing).as_str()
            } else if self.look_up_page(&st, it).is_some() {
                // In no list until its page says what it is.
                "looking_up"
            } else {
                row_state(it, passing).as_str()
            };
            let was = before.get(&it.id).map(|(s, _)| *s);
            if was == Some(state) {
                continue;
            }
            let name = if it.wrapper {
                it.title
                    .clone()
                    .or_else(|| it.mirrors.first().map(|m| m.url.clone()))
                    .unwrap_or_default()
            } else {
                display_name(it)
            };
            before.insert(it.id.clone(), (state, name.clone()));
            out.changed.push((
                RowStatus {
                    row: if it.wrapper { "wrapper" } else { "item" },
                    id: it.id.clone(),
                    name,
                    state,
                    error: it.error.clone(),
                },
                was,
            ));
        }
        // Every row on the list is in `before` now: more there means some went.
        if before.len() > st.items.len() {
            before.retain(|id, (state, name)| {
                let kept = st.items.contains_key(id);
                if !kept {
                    out.removed.push((id.clone(), std::mem::take(name), *state));
                }
                kept
            });
        }
        out
    }

    pub fn snapshot(&self) -> Snapshot {
        let st = self.lock();
        let reconnect = self.reconnect_view_in(&st);
        let services = self.services_view_in(&st);
        let pages = self.pages_pass();
        let pool = {
            let cfg = self.cfg();
            let proxies = cfg.proxy_pool().len();
            let vpn = if cfg.use_proxy {
                st.services.proxies.len()
            } else {
                0
            };
            PoolView {
                line: proxies + vpn == 0 || cfg.proxy_with_direct,
                proxies,
                vpn,
                on: cfg.use_proxy,
            }
        };
        let pool_summary = views::pool(
            &self.cfg(),
            pool,
            views::VpnFound {
                servers: services.nordvpn.len(),
                error: services.error.clone(),
            },
        );
        let now = Instant::now();
        let has_proxy = self.clients().proxies().next().is_some();
        let cfg = self.cfg();
        let wait = |it: &Item| {
            it.not_before
                .map_or(0, |t| t.saturating_duration_since(now).as_secs())
        };
        let retry_in = |it: &Item| {
            it.retry_at
                .map_or(0, |t| t.saturating_duration_since(now).as_secs().max(1))
        };
        let open_urls = |it: &Item| -> Vec<String> {
            it.mirrors
                .iter()
                .filter(|m| it.manual_urls.contains(m.as_str()))
                .map(|m| m.url.clone())
                .collect()
        };
        let open_hosts = |it: &Item| -> Vec<String> {
            it.mirrors
                .iter()
                .filter(|m| it.manual_urls.contains(m.as_str()))
                .map(|m| m.host.to_string())
                .collect()
        };
        let passing = |it: &Item| self.passing(&st, it, now);
        // A link a source listed that nobody named yet is no row to show:
        // it waits on its set, counted on the card, until a host names it
        // or it is found gone (plan MODEL-09).
        // A file nobody started every link of which is gone is no failed
        // download: a missing part of its archive, or a note on the card
        // when its whole cut is gone (plan LOOK-06).
        let dead = |it: &Item| !it.wrapper && it.unstarted && it.done == 0 && all_gone(it);
        let looking_up = |it: &Item| {
            self.look_up_page(&st, it).is_some()
                || unplaced(it)
                    && it
                        .mirrors
                        .iter()
                        .any(|m| hosts::tells_size(m) || st.passes(m) && self.pages_pass())
        };
        let of = st.pass_queue.len() as u32;
        let queue = |it: &Item| {
            let at = st.pass_queue.iter().position(|(id, _, _)| *id == it.id)?;
            Some(InQueue {
                place: at as u32 + 1,
                of,
            })
        };
        let wrappers: Vec<WrapperView> = st
            .items
            .values()
            .filter(|it| it.wrapper)
            .map(|it| WrapperView {
                id: it.id.clone(),
                url: it
                    .mirrors
                    .first()
                    .map(|m| m.url.clone())
                    .unwrap_or_default(),
                host: it
                    .mirrors
                    .first()
                    .map(|m| m.host.to_string())
                    .unwrap_or_default(),
                title: it.title.clone(),
                package: it.package.clone(),
                status: wrapper_state(it, passing(it)),
                source: it.source.clone(),
                lists_nothing: it.trouble == Trouble::ListsNothing,
                gone: all_gone(it),
                error: it.error.clone(),
                wait: wait(it),
                retry_in: retry_in(it),
                open_urls: open_urls(it),
                open_hosts: open_hosts(it),
                found: it.found,
                hosts: it.found_hosts.clone(),
                queue: queue(it),
                unread: it.unread,
            })
            .collect();
        let unpacked = self.unpack_progress();
        let items: Vec<ItemView> = st
            .items
            .values()
            .filter(|it| !it.wrapper)
            .map(|it| ItemView {
                id: it.id.clone(),
                name: display_name(it),
                package: it.package.clone(),
                status: row_state(it, passing(it)),
                error: it.error.clone(),
                size: it.size,
                done: it.done,
                speed: it.speed,
                host: it.host.clone(),
                mirrors: it.mirrors.iter().map(|m| m.host.to_string()).collect(),
                wait: wait(it),
                retry_in: retry_in(it),
                open_urls: open_urls(it),
                open_hosts: open_hosts(it),
                path: it.path.clone(),
                existing: it.existing,
                verifying: it.verifying,
                source: it.source.clone(),
                also_from: it.also_from.clone(),
                extract: it.extract,
                extract_error: it.extract_error.clone(),
                extract_progress: unpacked.filter(|_| it.extract == Extract::Extracting),
                list: None,
                not_started: false,
                shown_host: String::new(),
                preferred_host: false,
                other_hosts: 0,
                error_shown: String::new(),
                reason: String::new(),
                again: 0,
                failed_hosts: it
                    .failed
                    .iter()
                    .chain(&it.gone)
                    .map(|m| hosts::host_of(m))
                    .collect(),
                gone: all_gone(it),
                slowed: it.slowed.as_ref().map(|s| {
                    let by = if s.sure { "Slowed by" } else { "Slow from" };
                    format!("{by} {}: {}", s.host, s.why)
                }),
                commands: Vec::new(),
                archive: None,
                source_name: None,
                download_from: it.prefer.clone(),
                tentative: it.tentative || being_read(it) || looking_up(it) || dead(it),
                looking_up: looking_up(it),
                queue: queue(it),
            })
            .collect();
        let route = |name: &str| {
            st.routes.get(name).copied().unwrap_or(if has_proxy {
                Route::Proxy
            } else {
                Route::Direct
            })
        };
        let unmeasured = HostStats::new();
        let host_view = |name: &str, stats: Option<&HostStats>| {
            let exits = self.exits_for(name, &st);
            let lane = |e: &Exit| st.lanes.get(name, *e);
            let cap = st.lanes_of(name).filter_map(|l| l.cap).min();
            // Waiting only when every address it may use was asked to.
            let cooldown = exits
                .iter()
                .map(|e| {
                    lane(e)
                        .and_then(|l| l.cooldown)
                        .map_or(0, |c| c.saturating_duration_since(now).as_secs())
                })
                .min()
                .unwrap_or(0);
            let h = stats.unwrap_or(&unmeasured);
            HostView {
                name: name.to_string(),
                active: st.lanes_of(name).map(|l| l.active).sum(),
                speed: h.ema,
                fails: h.fails,
                ok: h.ok,
                bytes: h.bytes,
                blocked: h.blocked,
                route: route(name),
                cooldown,
                folder: cfg.folder_for(name).display().to_string(),
                preferred: cfg.preference(name),
                disabled: cfg.is_disabled(name),
                needs_account: hosts::known(name).is_some_and(|k| k.last_resort(&cfg, pages)),
                cap,
                quota: quota_view(exits.iter().filter_map(|e| lane(e).and_then(|l| l.quota))),
                free: hosts::known(name)
                    .map(|k| k.free)
                    .filter(|f| *f != hosts::Free::default()),
                account: hosts::known(name).is_some_and(|k| k.has_account(&cfg)),
                accountless: hosts::known(name)
                    .filter(|k| k.needs_account)
                    .map(|_| cfg.is_accountless(name)),
                exits: exits.len(),
                kind: views::site_kind(name),
                listed: true,
                group: HostGroup::Other,
                movable: false,
            }
        };
        let mut hosts: Vec<HostView> = st
            .hosts
            .iter()
            .map(|(name, h)| host_view(name, Some(h)))
            .collect();
        // A host shows before Haul has reached it when it is preferred or
        // turned off, so the choice can be seen and changed, and when a
        // download or a wrapper has a link on it, so it can be turned off
        // before it is tried.
        let mut shown: HashSet<String> = hosts.iter().map(|h| h.name.clone()).collect();
        let mut more = |name: &str| {
            if !name.is_empty() && !shown.contains(name) {
                shown.insert(name.to_string());
                hosts.push(host_view(name, None));
            }
        };
        for h in cfg.preferred_hosts.iter().chain(&cfg.disabled_hosts) {
            more(&h.to_ascii_lowercase());
        }
        for it in st.items.values() {
            for m in &it.mirrors {
                more(&m.host);
            }
        }
        // Every site Haul knows, so one can be set up before a link is on it.
        for name in hosts::known_sites() {
            more(&name);
        }
        // Any other site (a page, or a host Haul has no code for) is used
        // without a setting of its own, so the Hosts view does not list it:
        // only one the user gave one (a star, the switch, a folder,
        // a route) shows, to be changed back.
        let set = |name: &str| cfg.folders.contains_key(name) || st.routes.contains_key(name);
        for h in &mut hosts {
            h.listed =
                h.kind != SiteKind::Site || h.disabled || h.preferred.is_some() || set(&h.name);
        }
        // File hosts first: the preferred ones in their order, then the rest
        // by name, those used only when no other has the file after them and
        // those turned off last; then the other groups by their names, those
        // turned off last.
        hosts.sort_by(|a, b| {
            let rank = |h: &HostView| h.preferred.unwrap_or(usize::MAX);
            views::group_kind(a)
                .cmp(&views::group_kind(b))
                .then_with(|| rank(a).cmp(&rank(b)))
                .then_with(|| a.disabled.cmp(&b.disabled))
                .then_with(|| a.needs_account.cmp(&b.needs_account))
                .then_with(|| a.name.cmp(&b.name))
        });
        let in_sets = unpack::archives(&st.items);
        // Each card's notes: a cut of its release every link of which is
        // gone (plan CARD-05), by the card's first wrapper.
        let release_of: HashMap<String, String> = wrappers
            .iter()
            .map(|w| (w.id.clone(), super::friction::release_of(&st.items, &w.id)))
            .collect();
        let roots: HashSet<&String> = release_of.values().collect();
        let notes: HashMap<String, Vec<String>> = roots
            .into_iter()
            .map(|root| {
                let root = root.clone();
                let plan = super::release_plan(&st, &cfg, &root);
                let notes = plan
                    .dead_splits()
                    .map(|split| {
                        // A host by its name alone, as a person says it.
                        let hosts: Vec<&str> = split
                            .hosts
                            .iter()
                            .map(|h| h.rsplit_once('.').map_or(h.as_str(), |(n, _)| n))
                            .collect();
                        format!(
                            "another copy, {} parts on {}, is gone",
                            split.parts,
                            hosts.join(" and ")
                        )
                    })
                    .collect();
                (root, notes)
            })
            .collect();
        let (paused, max_total) = (st.paused, st.max_total);
        // What follows only reads the views built above.
        drop(st);
        let mut items = items;
        views::hosts(&mut hosts);
        views::items(&mut items, &hosts, &wrappers, cfg.recycle_on_remove);
        let archives = views::archives(&mut items, &hosts, in_sets);
        let mut wrapper_cards = views::wrapper_cards(&wrappers, &items);
        for card in &mut wrapper_cards {
            let root = card.ids.first().and_then(|id| release_of.get(id));
            card.notes = root.and_then(|r| notes.get(r)).cloned().unwrap_or_default();
        }
        let counts = views::counts(&items, &wrappers, &wrapper_cards, paused);
        Snapshot {
            speed: items.iter().map(|i| i.speed).sum(),
            items,
            archives,
            wrappers,
            wrapper_cards,
            counts,
            hosts,
            paused,
            max_total,
            rate: self.0.limiter.rate(),
            proxy: has_proxy,
            pool,
            pool_summary,
            theme: cfg.theme,
            watch_clipboard: cfg.watch_clipboard,
            recycle_on_remove: cfg.recycle_on_remove,
            skip_add_dialog: cfg.skip_add_dialog,
            bin: BIN,
            config_version: *self.0.config_version.borrow(),
            config_error: crate::lock::held(&self.0.config_error).clone(),
            download_dir: self.download_dir().display().to_string(),
            data_dir: self.0.data_dir.display().to_string(),
            config_file: self.0.config_path.as_ref().map(|p| p.display().to_string()),
            accounts: hosts::account_forms(&cfg),
            reconnect,
            services,
            update: crate::lock::held(&self.0.update)
                .as_ref()
                .map(crate::update::Update::view),
        }
    }
}

/// Whether every link of `it` is gone for good.
fn all_gone(it: &Item) -> bool {
    !it.mirrors.is_empty() && it.mirrors.iter().all(|m| it.gone.contains(m.as_str()))
}

/// Whether `it` is a pasted web page not read yet: it may turn out a wrapper
/// or a file, so it waits in no list, or it would flash through the downloads
/// on its way to the wrappers.
/// A wrapper's link not started that nothing has named yet: in no list
/// while a host or its page may still name it.
fn unplaced(it: &Item) -> bool {
    !it.wrapper && it.unstarted && it.name.is_none() && it.source.is_some() && it.done == 0
}

fn being_read(it: &Item) -> bool {
    it.status == Status::Queued
        && !it.probed
        && !it.wrapper
        && it
            .mirrors
            .first()
            .is_some_and(|url| hosts::is_web_page(url) && !hosts::is_download_link(url))
}
