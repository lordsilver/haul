//! What the views are sent: the state as one picture, taken under the lock
//! and filled in with what `views.rs` works out from it.

use std::{sync::atomic::Ordering, time::Instant};

use serde::Serialize;

use super::{
    ArchiveView, BIN, Command, Counts, Engine, Exit, HostStats, PoolSummary, ReconnectView,
    ServicesView, WrapperCard, display_name, unpack, views, waits_for_account,
};
use crate::{
    config::{Route, Theme},
    hosts,
    item::{Extract, Found, Item, Status, Verifying},
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
    #[ts(
        type = "\"queued\" | \"resolving\" | \"checking\" | \"downloading\" | \"waiting\" | \"passing\" | \"paused\" | \"captcha\" | \"done\" | \"error\""
    )]
    pub status: &'static str,
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
    /// Seconds until a waiting item may try again.
    pub wait: u64,
    /// Seconds until a failed item, or the page of one under Needs you, is
    /// tried again on its own; 0 when never.
    pub retry_in: u64,
    /// Mirrors a person has to open.
    pub open_urls: Vec<String>,
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
    /// The list it is in beside All downloads: `active`, `attention`,
    /// `failed`, `paused` or `done`.
    #[ts(type = "\"active\" | \"attention\" | \"failed\" | \"paused\" | \"done\"")]
    pub list: &'static str,
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
    /// `unwrapping`, `waiting` (to try again), `passing` (the desktop app
    /// passes its page's check by itself), `captcha` (a person has to pass
    /// its gate), `paused`, `error` or `done` (unwrapped).
    #[ts(
        type = "\"unwrapping\" | \"waiting\" | \"passing\" | \"captcha\" | \"paused\" | \"error\" | \"done\""
    )]
    pub status: &'static str,
    pub error: String,
    /// Seconds until it is tried again.
    pub wait: u64,
    /// Seconds until a failed one, or the page of one waiting for a person,
    /// is tried again on its own; 0 when never.
    pub retry_in: u64,
    /// Pages a person has to open.
    pub open_urls: Vec<String>,
    /// What unwrapping found.
    pub found: Option<Found>,
    /// The hosts its links are on.
    pub hosts: Vec<String>,
    /// The wrapper it was found on (a paste a download page links).
    pub source: Option<String>,
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
    /// Off until its account is set or the user switches it on without
    /// one ([`hosts::Known::off_for_account`]): its links are left out.
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
    /// The most downloads it takes at once, learned when it refused one more
    /// than it was serving; `None` until it does.
    pub cap: Option<usize>,
    /// What the site is to Haul: `host` (a file host Haul has code for),
    /// `wrapper` (filecrypt.cc's, fileguard.cc's…), `paste`, or `site`
    /// (anything else, listed only when the user starred or switched it off).
    #[ts(type = "\"host\" | \"wrapper\" | \"paste\" | \"site\"")]
    pub kind: &'static str,
    /// The Hosts view lists it: any but a `site` the user gave no setting.
    pub listed: bool,
    /// The group the Hosts view shows it in: a host's `preferred` or
    /// `other` (on or off alike), or its kind for the others.
    #[ts(type = "\"preferred\" | \"other\" | \"wrapper\" | \"paste\" | \"site\"")]
    pub group: &'static str,
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

impl Engine {
    pub fn snapshot(&self) -> Snapshot {
        // Before the state's lock: the two are always taken in this order.
        let reconnect = self.reconnect_view();
        let services = self.services_view();
        let pages = self.pages_pass();
        let pool = {
            let cfg = self.cfg();
            let proxies = cfg.proxy_pool().len();
            let vpn = if cfg.use_proxy {
                self.service_proxies().len()
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
        let st = self.lock();
        let now = Instant::now();
        let has_proxy = self.clients().exits.len() > 1;
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
                .filter(|m| it.manual_urls.contains(*m))
                .cloned()
                .collect()
        };
        // The desktop app passes one of its pages by itself, in turn: nothing
        // for the person yet.
        let passing = |it: &Item| {
            pages
                && !it.page_wants_person
                && it
                    .manual_urls
                    .iter()
                    .any(|u| st.passes(u) && self.pass_held_until(&st, u, now).is_none())
        };
        let wrappers: Vec<WrapperView> = st
            .items
            .values()
            .filter(|it| it.wrapper)
            .map(|it| WrapperView {
                id: it.id.clone(),
                url: it.mirrors.first().cloned().unwrap_or_default(),
                host: it
                    .mirrors
                    .first()
                    .map(|m| hosts::host_of(m))
                    .unwrap_or_default(),
                title: it.title.clone(),
                package: it.package.clone(),
                status: match it.status {
                    Status::Queued | Status::Resolving | Status::Downloading => "unwrapping",
                    // A page or paste behind Cloudflare's check, say.
                    Status::Captcha if passing(it) => "passing",
                    other => other.as_str(),
                },
                source: it.source.clone(),
                error: it.error.clone(),
                wait: wait(it),
                retry_in: retry_in(it),
                open_urls: open_urls(it),
                found: it.found,
                hosts: it.found_hosts.clone(),
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
                status: if it.status == Status::Queued && !it.probed {
                    "checking"
                } else if it.status == Status::Captcha && passing(it) {
                    "passing"
                } else {
                    it.status.as_str()
                },
                error: it.error.clone(),
                size: it.size,
                done: it.done,
                speed: it.speed,
                host: it.host.clone(),
                mirrors: it.mirrors.iter().map(|m| hosts::host_of(m)).collect(),
                wait: wait(it),
                retry_in: retry_in(it),
                open_urls: open_urls(it),
                path: it.path.clone(),
                existing: it.existing,
                verifying: it.verifying,
                source: it.source.clone(),
                also_from: it.also_from.clone(),
                extract: it.extract,
                extract_error: it.extract_error.clone(),
                extract_progress: unpacked.filter(|_| it.extract == Extract::Extracting),
                list: "",
                not_started: false,
                shown_host: String::new(),
                preferred_host: false,
                other_hosts: 0,
                error_shown: String::new(),
                commands: Vec::new(),
                archive: None,
                source_name: None,
                download_from: it.prefer.clone(),
                tentative: it.tentative || being_read(it) || waits_for_account(it, &cfg),
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
            let lane = |e: &Exit| st.lanes.get(&(name.to_string(), *e));
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
                needs_account: hosts::known(name).is_some_and(|k| k.off_for_account(&cfg)),
                cap,
                quota: quota_view(exits.iter().filter_map(|e| lane(e).and_then(|l| l.quota))),
                free: hosts::known(name)
                    .map(|k| k.free)
                    .filter(|f| *f != hosts::Free::default()),
                account: hosts::known(name).is_some_and(|k| k.has_account(&cfg)),
                exits: exits.len(),
                kind: "",
                listed: true,
                group: "",
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
        let mut names: Vec<String> = cfg
            .preferred_hosts
            .iter()
            .chain(&cfg.disabled_hosts)
            .map(|h| h.to_ascii_lowercase())
            .collect();
        for it in st.items.values() {
            names.extend(it.mirrors.iter().map(|m| hosts::host_of(m)));
        }
        // Every site Haul knows, so one can be set up before a link is on it.
        names.extend(hosts::known_sites());
        for name in names {
            if name.is_empty() || hosts.iter().any(|h| h.name == name) {
                continue;
            }
            hosts.push(host_view(&name, None));
        }
        for h in &mut hosts {
            h.kind = views::site_kind(&h.name);
        }
        // Any other site (a page, or a host Haul has no code for) is used
        // without a setting of its own, so the Hosts view does not list it:
        // only one the user gave one (a star, the switch, a folder,
        // a route) shows, to be changed back.
        let set = |name: &str| cfg.folders.contains_key(name) || st.routes.contains_key(name);
        for h in &mut hosts {
            h.listed = h.kind != "site" || h.disabled || h.preferred.is_some() || set(&h.name);
        }
        // File hosts first: the preferred ones in their order, then the rest
        // by name, on and off alike; then the other groups by their names.
        hosts.sort_by(|a, b| {
            let rank = |h: &HostView| h.preferred.unwrap_or(usize::MAX);
            let kind = |h: &HostView| views::SITE_KINDS.iter().position(|k| *k == h.kind);
            kind(a)
                .cmp(&kind(b))
                .then_with(|| rank(a).cmp(&rank(b)))
                .then_with(|| a.name.cmp(&b.name))
        });
        let mut items = items;
        views::hosts(&mut hosts);
        views::items(&mut items, &hosts, &wrappers, cfg.recycle_on_remove);
        let archives = views::archives(&mut items, &hosts, unpack::archives(&st.items));
        let wrapper_cards = views::wrapper_cards(&wrappers, &items);
        let counts = views::counts(&items, &wrappers, &wrapper_cards, st.paused);
        Snapshot {
            speed: items.iter().map(|i| i.speed).sum(),
            items,
            archives,
            wrappers,
            wrapper_cards,
            counts,
            hosts,
            paused: st.paused,
            max_total: st.max_total,
            rate: self.0.limiter.rate(),
            proxy: has_proxy,
            pool,
            pool_summary,
            theme: cfg.theme,
            watch_clipboard: cfg.watch_clipboard,
            recycle_on_remove: cfg.recycle_on_remove,
            skip_add_dialog: cfg.skip_add_dialog,
            bin: BIN,
            config_version: self.0.config_version.load(Ordering::Relaxed),
            config_error: self
                .0
                .config_error
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
            download_dir: self.download_dir().display().to_string(),
            data_dir: self.0.data_dir.display().to_string(),
            config_file: self.0.config_path.as_ref().map(|p| p.display().to_string()),
            accounts: hosts::account_forms(&cfg),
            reconnect,
            services,
            update: self
                .0
                .update
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref()
                .map(crate::update::Update::view),
        }
    }
}

/// Whether `it` is a pasted web page not read yet: it may turn out a wrapper
/// or a file, so it waits in no list, or it would flash through the downloads
/// on its way to the wrappers.
fn being_read(it: &Item) -> bool {
    it.status == Status::Queued
        && !it.probed
        && !it.wrapper
        && it
            .mirrors
            .first()
            .is_some_and(|url| hosts::is_web_page(url) && !hosts::is_download_link(url))
}
