//! What the views show that follows from the state rather than being part
//! of it: which list a download belongs to, the host it is shown under,
//! what can be done with it, an archive's parts summed up, the wrappers
//! that share a card, how the hosts group, and what the pool of addresses
//! is made of. Worked out here once per snapshot, so the pages only lay it
//! out and every rule is tested with the engine.

use std::collections::HashMap;

use serde::Serialize;

use super::{HostView, ItemView, WrapperView, unpack::ArchiveSet};
use crate::{
    config::{Config, FALLBACK_LIMIT},
    hosts,
    item::{Extract, Found},
};

/// States of a download that is running or will run on its own.
const ACTIVE: [&str; 6] = [
    "downloading",
    "resolving",
    "waiting",
    "queued",
    "checking",
    "passing",
];

/// What needs attention first, for a row that stands for several.
const URGENCY: [&str; 10] = [
    "error",
    "captcha",
    "downloading",
    "resolving",
    "passing",
    "waiting",
    "checking",
    "queued",
    "paused",
    "done",
];

/// What a row's buttons do; a view gives each its label and icon.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum Command {
    Pause,
    /// Resume a download that never started.
    Start,
    Resume,
    Retry,
    /// Show the finished file in its folder.
    Reveal,
    /// Take it off the list and its file to the bin.
    Recycle,
    Remove,
    /// Remove, partial file and all, which asks twice.
    RemovePartial,
    /// Take a finished download off the list; its file stays.
    Clear,
    Extract,
    /// Extract once more, after it failed.
    ExtractAgain,
    /// Ask for the archive's password, then extract with it.
    Password,
}

/// Which list a download is in, beside All downloads.
pub fn list_of(status: &str) -> &'static str {
    match status {
        s if ACTIVE.contains(&s) => "active",
        "captcha" => "attention",
        "error" => "failed",
        "paused" => "paused",
        _ => "done",
    }
}

/// `message` without the `host: ` errors start with, when the host shows
/// beside it already.
fn without_host(message: &str, host: &str) -> String {
    message
        .strip_prefix(host)
        .and_then(|m| m.strip_prefix(": "))
        .filter(|_| !host.is_empty())
        .unwrap_or(message)
        .to_string()
}

/// Fills in what follows for each download: its list, the host it is shown
/// under (the one it downloads from, or before it starts the one it would
/// try first), how many other hosts that are on could take over, its error
/// without that host's name, and its buttons.
pub(super) fn items(
    items: &mut [ItemView],
    hosts: &[HostView],
    wrappers: &[WrapperView],
    recycle_on_remove: bool,
) {
    let sources: HashMap<&str, String> = wrappers
        .iter()
        .map(|w| (w.id.as_str(), wrapper_name(w)))
        .collect();
    let known: HashMap<&str, &HostView> = hosts.iter().map(|h| (h.name.as_str(), h)).collect();
    let off = |m: &str| known.get(m).is_some_and(|h| h.disabled || h.needs_account);
    let rank = |m: &str| known.get(m).and_then(|h| h.preferred).unwrap_or(usize::MAX);
    for it in items {
        // A copied page on trial is in no list until it shows links.
        it.list = if it.tentative { "" } else { list_of(it.status) };
        it.not_started = it.status == "paused" && it.done == 0;
        it.shown_host = if it.host.is_empty() {
            let mut on: Vec<&String> = it.mirrors.iter().filter(|m| !off(m)).collect();
            // The host the user picked comes first, then their preferred ones.
            on.sort_by_key(|m| (it.download_from.as_deref() != Some(m.as_str()), rank(m)));
            on.first()
                .or(it.mirrors.first().as_ref())
                .map(|m| m.to_string())
                .unwrap_or_default()
        } else {
            it.host.clone()
        };
        it.preferred_host = known
            .get(it.shown_host.as_str())
            .is_some_and(|h| h.preferred.is_some());
        it.other_hosts = it
            .mirrors
            .iter()
            .filter(|m| **m != it.shown_host && !off(m))
            .count();
        it.error_shown = without_host(&it.error, &it.shown_host);
        it.source_name = it.source.as_deref().and_then(|s| sources.get(s)).cloned();
        it.commands = item_commands(it, recycle_on_remove);
    }
}

fn item_commands(it: &ItemView, recycle_on_remove: bool) -> Vec<Command> {
    let mut out = Vec::new();
    match it.status {
        s if ACTIVE.contains(&s) => out.push(Command::Pause),
        "paused" if it.not_started => out.push(Command::Start),
        "paused" => out.push(Command::Resume),
        "error" => out.push(Command::Retry),
        _ => {}
    }
    let finished = it.status == "done";
    if finished && it.path.is_some() {
        out.push(Command::Reveal);
    }
    out.push(if finished && it.path.is_some() && recycle_on_remove {
        Command::Recycle
    } else if !finished && it.done > 0 {
        Command::RemovePartial
    } else if finished {
        Command::Clear
    } else {
        Command::Remove
    });
    out
}

/// How a set of downloads stands, counted by state.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, ts_rs::TS)]
#[ts(export)]
pub struct Tally {
    pub done: usize,
    pub downloading: usize,
    /// Queued, waiting their turn or a retry, or being looked up.
    pub waiting: usize,
    pub not_started: usize,
    /// Paused after they had started.
    pub paused: usize,
    /// Waiting for a person.
    pub captcha: usize,
    pub failed: usize,
}

fn tally<'a>(items: impl IntoIterator<Item = &'a ItemView>) -> Tally {
    let mut t = Tally::default();
    for it in items {
        match it.status {
            "done" => t.done += 1,
            "downloading" => t.downloading += 1,
            "queued" | "waiting" | "checking" | "resolving" => t.waiting += 1,
            "paused" if it.not_started => t.not_started += 1,
            "paused" => t.paused += 1,
            "captcha" => t.captcha += 1,
            "error" => t.failed += 1,
            _ => {}
        }
    }
    t
}

/// An archive in parts, as one row over its parts.
#[derive(Debug, Serialize, ts_rs::TS)]
#[ts(export)]
pub struct ArchiveView {
    pub key: String,
    pub name: String,
    /// Its parts' ids, in part order.
    pub ids: Vec<String>,
    /// Parts its names show are missing.
    pub missing: Vec<String>,
    /// The state that needs attention first among the parts.
    #[ts(
        type = "\"queued\" | \"resolving\" | \"checking\" | \"downloading\" | \"waiting\" | \"passing\" | \"paused\" | \"captcha\" | \"done\" | \"error\""
    )]
    pub status: &'static str,
    /// Every part done.
    pub complete: bool,
    pub size: u64,
    pub done: u64,
    pub speed: f64,
    /// How far the parts have come, between 0 and 1: by bytes when every
    /// unfinished part's size is known, by parts otherwise.
    pub share: f64,
    pub tally: Tally,
    /// The hosts its parts are shown under, and how many others that are
    /// on could take over.
    pub hosts: Vec<String>,
    pub other_hosts: usize,
    /// Every host its parts are on, for choosing one.
    pub all_hosts: Vec<String>,
    /// The host the user picked for every part, when they picked one.
    pub download_from: Option<String>,
    pub package: String,
    /// The wrapper its first part came from, and that wrapper's name.
    pub source: Option<String>,
    pub source_name: Option<String>,
    pub extract: Extract,
    pub extract_error: String,
    pub extract_progress: Option<f32>,
    pub commands: Vec<Command>,
}

/// Whether a file's name is only an id an uploader gave it (a UUID, or a
/// long run of hex), which says nothing of what it holds.
fn says_nothing(stem: &str) -> bool {
    let hex = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_hexdigit());
    let groups: Vec<&str> = stem.split('-').collect();
    let uuid = groups.iter().map(|g| g.len()).eq([8, 4, 4, 4, 12]) && groups.iter().all(|g| hex(g));
    uuid || (stem.len() >= 16 && hex(stem))
}

/// An archive's name: its files' own, or the wrapper's name for the files
/// when the files are named after nothing but an id.
fn archive_name(name: String, source: Option<&str>) -> String {
    let (stem, ext) = name.rsplit_once('.').unwrap_or((&name, ""));
    match source.map(str::trim).filter(|r| !r.is_empty()) {
        Some(source) if says_nothing(stem) && !ext.is_empty() => format!("{source}.{ext}"),
        Some(source) if says_nothing(stem) => source.to_string(),
        _ => name,
    }
}

/// Each archive in parts, its parts marked with its key.
pub(super) fn archives(
    items: &mut [ItemView],
    hosts: &[HostView],
    sets: Vec<ArchiveSet>,
) -> Vec<ArchiveView> {
    let index: HashMap<String, usize> = items
        .iter()
        .enumerate()
        .map(|(i, it)| (it.id.clone(), i))
        .collect();
    let off = |m: &str| {
        hosts
            .iter()
            .any(|h| h.name == m && (h.disabled || h.needs_account))
    };
    let mut out = Vec::with_capacity(sets.len());
    for set in sets {
        let parts: Vec<&ItemView> = set
            .ids
            .iter()
            .filter_map(|id| index.get(id).map(|&i| &items[i]))
            .collect();
        let Some(first) = parts.first() else {
            continue;
        };
        let status = URGENCY
            .into_iter()
            .find(|s| parts.iter().any(|p| p.status == *s))
            .unwrap_or("queued");
        let complete = parts.iter().all(|p| p.status == "done");
        let size: u64 = parts.iter().map(|p| p.size).sum();
        let done: u64 = parts
            .iter()
            .map(|p| {
                if p.status == "done" {
                    p.size
                } else {
                    p.done.min(if p.size > 0 { p.size } else { p.done })
                }
            })
            .sum();
        let sized = parts.iter().all(|p| p.size > 0 || p.status == "done");
        let tally = tally(parts.iter().copied());
        let share = if sized && size > 0 {
            done as f64 / size as f64
        } else {
            tally.done as f64 / parts.len() as f64
        };
        let mut shown: Vec<String> = Vec::new();
        for p in &parts {
            if !shown.contains(&p.shown_host) {
                shown.push(p.shown_host.clone());
            }
        }
        let mut others: Vec<&String> = parts
            .iter()
            .flat_map(|p| &p.mirrors)
            .filter(|m| !shown.contains(m) && !off(m))
            .collect();
        others.sort();
        others.dedup();
        let mut all_hosts: Vec<String> = Vec::new();
        for m in parts.iter().flat_map(|p| &p.mirrors) {
            if !all_hosts.contains(m) {
                all_hosts.push(m.clone());
            }
        }
        let download_from = first
            .download_from
            .clone()
            .filter(|h| parts.iter().all(|p| p.download_from.as_deref() == Some(h)));
        let extract = if complete { first.extract } else { Extract::No };
        let mut commands = Vec::new();
        if parts.iter().any(|p| ACTIVE.contains(&p.status)) {
            commands.push(Command::Pause);
        } else if parts.iter().any(|p| p.status == "paused") {
            let all_fresh = parts
                .iter()
                .filter(|p| p.status == "paused")
                .all(|p| p.not_started);
            commands.push(if all_fresh {
                Command::Start
            } else {
                Command::Resume
            });
        }
        if tally.failed > 0 {
            commands.push(Command::Retry);
        }
        if complete && set.missing.is_empty() {
            match extract {
                Extract::No => commands.push(Command::Extract),
                Extract::Failed => commands.push(Command::ExtractAgain),
                Extract::Password => commands.push(Command::Password),
                _ => {}
            }
        }
        // Removing them all, as each part's own button would: asked twice
        // when one loses a partial file, to the bin when finished ones go
        // there, cleared from the list when every part is finished.
        commands.extend(
            [
                Command::RemovePartial,
                Command::Recycle,
                Command::Remove,
                Command::Clear,
            ]
            .into_iter()
            .find(|c| parts.iter().any(|p| p.commands.contains(c))),
        );
        out.push(ArchiveView {
            key: set.key.clone(),
            name: archive_name(set.name, first.source_name.as_deref()),
            missing: set.missing,
            status,
            complete,
            size,
            done,
            speed: parts.iter().map(|p| p.speed).sum(),
            share,
            tally,
            hosts: shown,
            other_hosts: others.len(),
            all_hosts,
            download_from,
            package: first.package.clone(),
            source: first.source.clone(),
            source_name: first.source_name.clone(),
            extract,
            extract_error: if matches!(extract, Extract::Failed | Extract::Password) {
                first.extract_error.clone()
            } else {
                String::new()
            },
            extract_progress: first.extract_progress,
            commands,
            ids: set.ids,
        });
        for id in &out.last().expect("just pushed").ids {
            if let Some(&i) = index.get(id) {
                items[i].archive = Some(set.key.clone());
            }
        }
    }
    out
}

/// A wrapper's name: the page's for what it holds, or its link.
fn wrapper_name(w: &WrapperView) -> String {
    w.title.clone().unwrap_or_else(|| w.url.clone())
}

/// The wrappers one card stands for: those of one set of files (the same name,
/// unwrapped, often one per host) share it; one still working, waiting for
/// a person or failed keeps a card of its own.
#[derive(Debug, Serialize, ts_rs::TS)]
#[ts(export)]
pub struct WrapperCard {
    pub key: String,
    /// Its wrappers' ids, the first one's state being the card's.
    pub ids: Vec<String>,
    /// The page's name for what it holds, or the link.
    pub name: String,
    pub urls: Vec<String>,
    pub host: String,
    /// The hosts their links are on, over every wrapper of the card.
    pub hosts: Vec<String>,
    #[ts(
        type = "\"unwrapping\" | \"waiting\" | \"passing\" | \"captcha\" | \"paused\" | \"error\" | \"done\""
    )]
    pub status: &'static str,
    /// Its error without the wrapper's host.
    pub error_shown: String,
    pub wait: u64,
    pub retry_in: u64,
    pub open_urls: Vec<String>,
    pub found: Option<Found>,
    /// The files its wrappers added, counted once.
    pub files: usize,
    /// Its pastes that list no links (a description, say), set
    /// aside while anything else on the card goes on or found files: a
    /// note, not a failure. 0 when the card failed for them.
    pub empty: usize,
    /// The wrapper whose state the card shows, the one to open when it
    /// needs a person.
    pub open_id: String,
    /// Its wrappers waiting for a person.
    pub need_you: usize,
    pub tally: Tally,
}

/// What needs attention first among a card's wrappers.
const WRAPPER_URGENCY: [&str; 7] = [
    "error",
    "captcha",
    "waiting",
    "passing",
    "unwrapping",
    "paused",
    "done",
];

pub(super) fn wrapper_cards(wrappers: &[WrapperView], items: &[ItemView]) -> Vec<WrapperCard> {
    let by_id: HashMap<&str, &WrapperView> = wrappers.iter().map(|w| (w.id.as_str(), w)).collect();
    // A wrapper found on another (a paste a download page links) is on the
    // card of the one it was found on, whatever state it is in.
    fn root<'a>(by_id: &HashMap<&str, &'a WrapperView>, mut w: &'a WrapperView) -> &'a WrapperView {
        for _ in 0..32 {
            match w.source.as_deref().and_then(|s| by_id.get(s)) {
                Some(up) => w = up,
                None => break,
            }
        }
        w
    }
    let mut groups: Vec<(String, Vec<&WrapperView>)> = Vec::new();
    for w in wrappers {
        let top = root(&by_id, w);
        let key = match (&top.title, top.status) {
            (Some(title), "done") => format!("t\u{0}{}\u{0}{title}", top.package),
            _ => format!("w\u{0}{}", top.id),
        };
        match groups.iter_mut().find(|(k, _)| *k == key) {
            Some((_, ws)) => ws.push(w),
            None => groups.push((key, vec![w])),
        }
    }
    groups
        .into_iter()
        .map(|(key, ws)| {
            let w = ws[0];
            let files: Vec<&ItemView> = items
                .iter()
                .filter(|it| {
                    ws.iter().any(|x| {
                        it.source.as_deref() == Some(x.id.as_str()) || it.also_from.contains(&x.id)
                    })
                })
                .collect();
            // A paste that lists no links (a description) is a
            // note on a card that has anything else going on or found,
            // not the card's failure.
            let lists_nothing =
                |x: &WrapperView| x.status == "error" && crate::hosts::lists_no_links(&x.error);
            let quiet = ws.iter().any(|x| !lists_nothing(x));
            let set_aside = |x: &WrapperView| quiet && lists_nothing(x);
            // The card shows what needs attention first among them.
            let urgent = WRAPPER_URGENCY
                .iter()
                .find_map(|s| ws.iter().find(|x| x.status == *s && !set_aside(x)))
                .copied()
                .unwrap_or(w);
            let empty = ws.iter().filter(|x| set_aside(x)).count();
            let mut hosts: Vec<String> = Vec::new();
            for h in ws.iter().flat_map(|x| &x.hosts) {
                if !hosts.contains(h) {
                    hosts.push(h.clone());
                }
            }
            WrapperCard {
                key,
                ids: ws.iter().map(|x| x.id.clone()).collect(),
                name: wrapper_name(w),
                urls: ws.iter().map(|x| x.url.clone()).collect(),
                host: w.host.clone(),
                hosts,
                status: urgent.status,
                error_shown: without_host(&urgent.error, &urgent.host),
                wait: urgent.wait,
                retry_in: urgent.retry_in,
                open_urls: urgent.open_urls.clone(),
                found: w.found,
                files: files.len(),
                empty,
                open_id: urgent.id.clone(),
                need_you: ws.iter().filter(|x| x.status == "captcha").count(),
                tally: tally(files),
            }
        })
        .collect()
}

/// The counts the navigation pane and the status bar show.
#[derive(Debug, Default, Serialize, ts_rs::TS)]
#[ts(export)]
pub struct Counts {
    pub all: usize,
    pub active: usize,
    pub attention: usize,
    pub failed: usize,
    pub paused: usize,
    pub done: usize,
    pub downloading: usize,
    /// Queued, waiting their turn or a retry, or being looked up.
    pub waiting: usize,
    /// Paused, and the pause holds something back that would run.
    pub held: bool,
    /// Bytes still to download in the Active list, over the files whose
    /// size is known: with the speed, when everything is done.
    pub left: u64,
    /// Wrapper cards, and whether one failed (`critical`) or waits for a
    /// person (`attention`), by the cards' states.
    pub wrappers: usize,
    #[ts(type = "\"\" | \"attention\" | \"critical\"")]
    pub wrappers_alert: &'static str,
    /// Wrappers being opened and pages being passed out of sight: work on
    /// links before their files show.
    pub unwrapping: usize,
    /// Downloads and wrappers waiting for a person.
    pub need_you: usize,
    /// Wrappers' files not started yet: they wait for the user to start
    /// them.
    pub ready: usize,
}

pub(super) fn counts(
    items: &[ItemView],
    wrappers: &[WrapperView],
    cards: &[WrapperCard],
    paused: bool,
) -> Counts {
    let t = tally(items.iter().filter(|i| !i.tentative));
    let in_list = |l: &str| items.iter().filter(|i| i.list == l).count();
    Counts {
        all: items.iter().filter(|i| !i.tentative).count(),
        active: in_list("active"),
        attention: in_list("attention"),
        failed: in_list("failed"),
        paused: in_list("paused"),
        done: in_list("done"),
        downloading: t.downloading,
        waiting: t.waiting,
        held: paused && items.iter().any(|i| i.list == "active"),
        left: items
            .iter()
            .filter(|i| i.list == "active")
            .map(|i| i.size.saturating_sub(i.done))
            .sum(),
        wrappers: cards.len(),
        unwrapping: wrappers
            .iter()
            .filter(|w| matches!(w.status, "unwrapping" | "passing"))
            .count()
            + items
                .iter()
                .filter(|i| {
                    i.status == "passing"
                        // A copied page on trial, while it is looked at.
                        || (i.tentative
                            && matches!(i.status, "queued" | "resolving" | "checking"))
                })
                .count(),
        need_you: in_list("attention") + wrappers.iter().filter(|w| w.status == "captcha").count(),
        ready: items
            .iter()
            .filter(|i| i.not_started && i.source.is_some() && !i.tentative)
            .count(),
        // From the cards, which set aside a paste that lists nothing.
        wrappers_alert: if cards.iter().any(|c| c.status == "error") {
            "critical"
        } else if cards.iter().any(|c| c.status == "captcha") {
            "attention"
        } else {
            ""
        },
    }
}

/// The kinds of site the Sites view groups, in its order ([`site_kind`]):
/// the hosts, then the rest by their groups' names (Other sites,
/// Pastebins, Wrappers).
pub(super) const SITE_KINDS: [&str; 4] = ["host", "site", "paste", "wrapper"];

/// What site `name` is to Haul: a wrapper's or a paste's by its name, a
/// host when Haul has code for it as one, else a site (a page Haul reads
/// links from, or a host it has no code for, used as any web server).
pub(super) fn site_kind(name: &str) -> &'static str {
    let url = format!("https://{name}/");
    if hosts::wraps(&url) {
        "wrapper"
    } else if hosts::is_paste(&url) {
        "paste"
    } else if hosts::known(name).is_some() {
        "host"
    } else {
        "site"
    }
}

/// Where each host stands in the Hosts view: its group, the downloads it
/// runs at once, and whether it can be moved in the preferred order.
/// Wrappers, pastes and sites are grouped by their kind, and have no order
/// or slots that matter.
pub(super) fn hosts(hosts: &mut [HostView]) {
    let preferred = hosts
        .iter()
        .filter(|h| h.kind == "host" && h.preferred.is_some())
        .count();
    for h in hosts {
        h.group = if h.kind != "host" {
            h.kind
        } else if h.preferred.is_some() {
            "preferred"
        } else {
            "other"
        };
        h.slots = h.cap.map_or(h.limit, |cap| cap.min(h.limit));
        h.limited = h.limit < FALLBACK_LIMIT || h.cap.is_some();
        h.movable = h.kind == "host" && h.preferred.is_some() && preferred > 1;
    }
}

/// One source of addresses and where it stands.
#[derive(Debug, Serialize, PartialEq, ts_rs::TS)]
#[ts(export)]
pub struct PoolSource {
    pub name: &'static str,
    pub status: String,
    /// The setting its switch flips, when it has one.
    pub key: Option<&'static str>,
    pub on: bool,
    /// Switched on, yet adding nothing.
    pub problem: bool,
    /// Its switch's name, when not its own.
    pub label: Option<&'static str>,
}

/// What downloads go out through, in words, and each source of addresses.
#[derive(Debug, Serialize, PartialEq, ts_rs::TS)]
#[ts(export)]
pub struct PoolSummary {
    /// "Line + 2 proxies + 5 NordVPN", or "Line only".
    pub label: String,
    /// A proxy or a VPN service is set up.
    pub configured: bool,
    pub sources: Vec<PoolSource>,
    /// Why the pool is only the line although proxies or VPN are on.
    pub why: Option<String>,
}

/// What the services found, for [`pool`].
pub(super) struct VpnFound {
    pub servers: usize,
    pub error: Option<String>,
}

pub(super) fn pool(cfg: &Config, p: super::PoolView, vpn: VpnFound) -> PoolSummary {
    let label = {
        let parts: Vec<String> = [
            p.line.then(|| "Line".to_string()),
            (p.proxies > 0).then(|| {
                format!(
                    "{} {}",
                    p.proxies,
                    if p.proxies == 1 { "proxy" } else { "proxies" }
                )
            }),
            (p.vpn > 0).then(|| format!("{} NordVPN", p.vpn)),
        ]
        .into_iter()
        .flatten()
        .collect();
        if parts.len() == 1 && p.line {
            "Line only".to_string()
        } else {
            parts.join(" + ")
        }
    };
    let proxies = usize::from(cfg.proxy.is_some()) + cfg.proxies.len();
    let nord = cfg
        .account("nordvpn_password")
        .and_then(Option::as_deref)
        .is_some_and(|s| !s.is_empty());
    let paused = !cfg.use_proxy;
    let mut sources = Vec::new();
    if proxies > 0 {
        let status = if !cfg.proxies_on {
            "Off".to_string()
        } else if paused {
            "Paused by the switch above".to_string()
        } else {
            format!("{} in use", p.proxies)
        };
        sources.push(PoolSource {
            name: "Proxies",
            status,
            key: Some("proxies_on"),
            on: cfg.proxies_on,
            problem: false,
            label: None,
        });
    }
    if nord {
        let where_ = if cfg.nordvpn_countries.is_empty() {
            "any country".to_string()
        } else {
            cfg.nordvpn_countries.join(", ")
        };
        let failing = vpn.error.is_some() && vpn.servers == 0;
        let status = if !cfg.nordvpn_on {
            "Off".to_string()
        } else if cfg.nordvpn_servers == 0 {
            "No servers chosen".to_string()
        } else if failing {
            vpn.error.clone().unwrap_or_default()
        } else if paused {
            "Paused by the switch above".to_string()
        } else if vpn.servers > 0 {
            format!(
                "{} {} · {where_}",
                vpn.servers,
                if vpn.servers == 1 {
                    "server"
                } else {
                    "servers"
                }
            )
        } else {
            "Looking for servers…".to_string()
        };
        sources.push(PoolSource {
            name: "NordVPN",
            status,
            key: Some("nordvpn_on"),
            on: cfg.nordvpn_on,
            problem: cfg.nordvpn_on && (cfg.nordvpn_servers == 0 || failing),
            label: None,
        });
    }
    if !sources.is_empty() {
        // Its switch adds it beside proxies and VPN; with nothing beside it,
        // it carries every download whatever the switch says.
        let alone = paused || p.proxies + p.vpn == 0;
        let status = if alone {
            "Every download"
        } else if cfg.proxy_with_direct {
            "One more address beside them"
        } else {
            "Not used while they are on"
        };
        sources.insert(
            0,
            PoolSource {
                name: "Your line",
                status: status.to_string(),
                key: (!alone).then_some("proxy_with_direct"),
                on: cfg.proxy_with_direct,
                problem: false,
                label: Some("Your line in the pool"),
            },
        );
    }
    // Nothing set up is no fault of any source: the views say so instead.
    let why = if sources.is_empty() || !cfg.use_proxy || p.proxies + p.vpn > 0 {
        None
    } else {
        Some(match sources.iter().find(|s| s.problem) {
            Some(s) if s.name == "NordVPN" && cfg.nordvpn_servers == 0 => {
                "Nothing added yet: NordVPN has no servers chosen. Set how many under VPN services › Servers.".to_string()
            }
            Some(s) => format!("Nothing added yet: {}: {}.", s.name, s.status),
            None => "Nothing added yet: every proxy and VPN source is off.".to_string(),
        })
    };
    PoolSummary {
        label,
        configured: !sources.is_empty(),
        sources,
        why,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{config::Route, item::Verifying};

    fn item(id: &str, status: &'static str) -> ItemView {
        ItemView {
            id: id.into(),
            name: id.into(),
            package: String::new(),
            status,
            error: String::new(),
            size: 100,
            done: if status == "done" { 100 } else { 0 },
            speed: 0.0,
            percent: None,
            host: String::new(),
            mirrors: vec!["a.example".into()],
            wait: 0,
            retry_in: 0,
            open_urls: Vec::new(),
            path: None,
            existing: false,
            verifying: Verifying::No,
            source: None,
            also_from: Vec::new(),
            extract: Extract::No,
            extract_error: String::new(),
            extract_progress: None,
            list: "",
            not_started: false,
            shown_host: String::new(),
            preferred_host: false,
            other_hosts: 0,
            error_shown: String::new(),
            commands: Vec::new(),
            archive: None,
            source_name: None,
            download_from: None,
            tentative: false,
        }
    }

    fn host(name: &str) -> HostView {
        HostView {
            name: name.into(),
            limit: FALLBACK_LIMIT,
            active: 0,
            speed: 0.0,
            fails: 0,
            ok: 0,
            bytes: 0,
            blocked: false,
            route: Route::Direct,
            cooldown: 0,
            folder: String::new(),
            preferred: None,
            disabled: false,
            needs_account: false,
            exits: 1,
            quota: None,
            free: None,
            account: false,
            cap: None,
            kind: "host",
            listed: true,
            group: "",
            slots: 0,
            limited: false,
            movable: false,
        }
    }

    fn wrapper(id: &str, status: &'static str, title: Option<&str>) -> WrapperView {
        WrapperView {
            id: id.into(),
            url: format!("https://w.example/{id}"),
            host: "w.example".into(),
            title: title.map(String::from),
            package: String::new(),
            status,
            error: String::new(),
            wait: 0,
            retry_in: 0,
            open_urls: Vec::new(),
            found: None,
            hosts: vec![format!("{id}.example")],
            source: None,
        }
    }

    #[test]
    fn pastes_found_on_a_release_page_share_its_card() {
        let page = wrapper("page", "done", Some("Example"));
        let mut done = wrapper("paste1", "done", Some("Paste one"));
        done.source = Some("page".into());
        let mut passing = wrapper("paste2", "passing", None);
        passing.source = Some("page".into());
        let other = wrapper("other", "captcha", None);
        let cards = wrapper_cards(&[page, done, passing, other], &[]);
        assert_eq!(
            cards.len(),
            2,
            "the download page and its pastes on one card, the other on its own"
        );
        assert_eq!(cards[0].ids, ["page", "paste1", "paste2"]);
        assert_eq!(cards[0].name, "Example");
        assert_eq!(cards[0].status, "passing", "what still goes on shows");
    }

    #[test]
    fn a_download_shows_under_the_host_it_would_try_first() {
        let mut hosts = vec![host("a.example"), host("b.example"), host("off.example")];
        hosts[1].preferred = Some(0);
        hosts[2].disabled = true;
        let mut it = item("x", "queued");
        it.mirrors = vec!["off.example".into(), "a.example".into(), "b.example".into()];
        it.error = "b.example: busy".into();
        let mut items = vec![it];
        super::items(&mut items, &hosts, &[], false);
        let it = &items[0];
        assert_eq!(
            (it.shown_host.as_str(), it.preferred_host),
            ("b.example", true)
        );
        assert_eq!(
            it.other_hosts, 1,
            "a.example; the one that is off does not count"
        );
        assert_eq!(it.error_shown, "busy");
        assert_eq!(it.list, "active");

        // Once it runs, the host it runs on.
        let mut it = item("y", "downloading");
        it.host = "a.example".into();
        it.error = "b.example: busy".into();
        let mut items = vec![it];
        super::items(&mut items, &hosts, &[], false);
        assert_eq!(items[0].shown_host, "a.example");
        assert_eq!(items[0].error_shown, "b.example: busy");
    }

    #[test]
    fn each_state_has_its_buttons() {
        let hosts = [host("a.example")];
        let cases: [(&'static str, u64, bool, bool, &[Command]); 7] = [
            (
                "downloading",
                50,
                false,
                false,
                &[Command::Pause, Command::RemovePartial],
            ),
            (
                "queued",
                0,
                false,
                false,
                &[Command::Pause, Command::Remove],
            ),
            (
                "paused",
                0,
                false,
                false,
                &[Command::Start, Command::Remove],
            ),
            (
                "paused",
                10,
                false,
                false,
                &[Command::Resume, Command::RemovePartial],
            ),
            ("error", 0, false, false, &[Command::Retry, Command::Remove]),
            ("done", 100, true, false, &[Command::Reveal, Command::Clear]),
            (
                "done",
                100,
                true,
                true,
                &[Command::Reveal, Command::Recycle],
            ),
        ];
        for (status, done, path, recycle, want) in cases {
            let mut it = item("x", status);
            it.done = done;
            it.path = path.then(|| "/dl/x".to_string());
            let mut items = vec![it];
            super::items(&mut items, &hosts, &[], recycle);
            assert_eq!(items[0].commands, want, "{status} {done} {recycle}");
        }
    }

    #[test]
    fn an_archive_named_after_an_id_takes_the_release_name() {
        let source = Some("Example.Files.Complete.Set");
        assert_eq!(
            archive_name("186d2b09-9d89-48af-8306-fa3348c01fe6.rar".into(), source),
            "Example.Files.Complete.Set.rar"
        );
        assert_eq!(
            archive_name("9f86d081884c7d659a2feaa0c55ad015.7z".into(), source),
            "Example.Files.Complete.Set.7z"
        );
        // A name that says something stays, and so does an id with no
        // files to name it.
        assert_eq!(
            archive_name("Holiday.Photos.rar".into(), source),
            "Holiday.Photos.rar"
        );
        assert_eq!(archive_name("deadbeef.rar".into(), source), "deadbeef.rar");
        assert_eq!(
            archive_name("186d2b09-9d89-48af-8306-fa3348c01fe6.rar".into(), None),
            "186d2b09-9d89-48af-8306-fa3348c01fe6.rar"
        );
    }

    fn set(key: &str, ids: &[&str], missing: &[&str]) -> ArchiveSet {
        ArchiveSet {
            key: key.into(),
            name: format!("{key}.rar"),
            ids: ids.iter().map(|s| s.to_string()).collect(),
            missing: missing.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn an_archive_sums_up_its_parts() {
        let hosts = [host("a.example")];
        let mut items = vec![
            item("p1", "done"),
            item("p2", "downloading"),
            item("p3", "error"),
        ];
        items[1].done = 50;
        super::items(&mut items, &hosts, &[], false);
        let a = archives(&mut items, &hosts, vec![set("R", &["p1", "p2", "p3"], &[])]);
        let a = &a[0];
        assert_eq!(a.status, "error", "a failure first");
        assert!(!a.complete);
        assert_eq!((a.size, a.done), (300, 150));
        assert!((a.share - 0.5).abs() < 1e-9);
        assert_eq!(
            (a.tally.done, a.tally.downloading, a.tally.failed),
            (1, 1, 1)
        );
        assert_eq!(
            a.commands,
            [Command::Pause, Command::Retry, Command::RemovePartial],
            "removing loses p2's partial file"
        );
        assert!(items.iter().all(|i| i.archive.as_deref() == Some("R")));
    }

    #[test]
    fn an_archive_offers_extraction_once_complete() {
        let hosts = [host("a.example")];
        let done = || {
            let mut items = vec![item("p1", "done"), item("p2", "done")];
            super::items(&mut items, &hosts, &[], false);
            items
        };
        let mut items = done();
        let a = archives(&mut items, &hosts, vec![set("R", &["p1", "p2"], &[])]);
        assert_eq!(a[0].commands, [Command::Extract, Command::Clear]);

        let mut items = done();
        for it in &mut items {
            it.extract = Extract::Failed;
            it.extract_error = "the archive needs a password".into();
        }
        let a = archives(&mut items, &hosts, vec![set("R", &["p1", "p2"], &[])]);
        assert_eq!(a[0].commands, [Command::ExtractAgain, Command::Clear]);
        assert_eq!(a[0].extract_error, "the archive needs a password");

        let mut items = done();
        for it in &mut items {
            it.extract = Extract::Password;
            it.extract_error = "none of the passwords fits".into();
        }
        let a = archives(&mut items, &hosts, vec![set("R", &["p1", "p2"], &[])]);
        assert_eq!(a[0].commands, [Command::Password, Command::Clear]);
        assert_eq!(a[0].extract_error, "none of the passwords fits");

        // A part the names show is missing: nothing to extract yet.
        let mut items = done();
        let a = archives(
            &mut items,
            &hosts,
            vec![set("R", &["p1", "p2"], &["R.part3.rar"])],
        );
        assert_eq!(a[0].commands, [Command::Clear]);
        assert_eq!(a[0].missing, ["R.part3.rar"]);
    }

    #[test]
    fn a_releases_wrappers_share_a_card() {
        let wrappers = [
            wrapper("w1", "done", Some("Example")),
            wrapper("w2", "done", Some("Example")),
            wrapper("w3", "error", Some("Example")),
            wrapper("w4", "unwrapping", None),
        ];
        let mut f1 = item("f1", "paused");
        f1.source = Some("w1".into());
        f1.also_from = vec!["w2".into()];
        let mut f2 = item("f2", "done");
        f2.source = Some("w2".into());
        let mut items = vec![f1, f2, item("other", "queued")];
        super::items(&mut items, &[host("a.example")], &wrappers, false);
        assert_eq!(items[0].source_name.as_deref(), Some("Example"));
        let cards = wrapper_cards(&wrappers, &items);
        assert_eq!(cards.len(), 3);
        assert_eq!(cards[0].ids, ["w1", "w2"]);
        assert_eq!(cards[0].hosts, ["w1.example", "w2.example"]);
        assert_eq!(cards[0].files, 2, "a file both list counts once");
        assert_eq!((cards[0].tally.not_started, cards[0].tally.done), (1, 1));
        assert_eq!(cards[1].ids, ["w3"], "a failed one keeps its own");
        assert_eq!(cards[2].name, "https://w.example/w4");

        let c = counts(&items, &wrappers, &cards, false);
        assert_eq!((c.all, c.active, c.paused, c.done), (3, 1, 1, 1));
        assert_eq!((c.wrappers, c.wrappers_alert), (3, "critical"));
        assert!(!c.held);
        assert_eq!(
            c.left, 100,
            "the queued file; paused and done ones wait for nothing"
        );
        assert!(
            counts(&items, &wrappers, &cards, true).held,
            "a pause that holds one back"
        );
    }

    #[test]
    fn counts_the_work_on_links_and_who_needs_you() {
        let wrappers = [
            wrapper("open", "unwrapping", None),
            wrapper("guarded", "passing", None),
            wrapper("asks", "captcha", None),
            wrapper("done", "done", None),
        ];
        let mut items = vec![item("page", "passing"), item("q", "queued")];
        super::items(&mut items, &[host("a.example")], &wrappers, false);
        let c = counts(&items, &wrappers, &wrapper_cards(&wrappers, &items), false);
        assert_eq!(c.unwrapping, 3, "two wrappers and a download's page");
        assert_eq!(c.need_you, 1, "the wrapper that asks");

        // Copied pages on trial count only while they are looked at.
        let mut items = ["queued", "passing", "captcha", "done"].map(|st| {
            let mut it = item(st, st);
            it.tentative = true;
            it
        });
        super::items(&mut items, &[host("a.example")], &[], false);
        let c = counts(&items, &[], &[], false);
        assert_eq!(c.unwrapping, 2, "the queued page and the one passing");
        assert_eq!((c.all, c.need_you), (0, 0));
    }

    #[test]
    fn a_paste_that_lists_no_links_is_a_quiet_note() {
        let page = wrapper("page", "done", Some("Example"));
        let mut empty = wrapper("paste", "error", None);
        empty.source = Some("page".into());
        empty.error = "zpaste.net: the paste on zpaste.net lists no links".into();
        let wrappers = [page, empty];
        let mut file = item("f", "paused");
        file.source = Some("page".into());
        let mut items = vec![file];
        super::items(&mut items, &[host("a.example")], &wrappers, false);
        let cards = wrapper_cards(&wrappers, &items);
        assert_eq!(cards.len(), 1, "the paste is on its page's card");
        assert_eq!((cards[0].status, cards[0].empty), ("done", 1));
        assert_eq!(cards[0].error_shown, "");
        let c = counts(&items, &wrappers, &cards, false);
        assert_eq!(c.wrappers_alert, "", "nothing failed that matters");

        // Only pastes that list nothing: the page found no files.
        let mut alone = wrapper("alone", "error", None);
        alone.error = "the paste on zpaste.net lists no links".into();
        let wrappers = [alone];
        let cards = wrapper_cards(&wrappers, &[]);
        assert_eq!((cards[0].status, cards[0].empty), ("error", 0));
        assert_eq!(
            counts(&[], &wrappers, &cards, false).wrappers_alert,
            "critical"
        );
    }

    /// A release page whose description paste lists nothing and whose
    /// containers wait for a person: the card asks for the person, not a
    /// failure, and opens a container.
    #[test]
    fn a_release_waiting_for_a_person_is_not_failed_by_its_description() {
        let page = wrapper("page", "done", Some("Example"));
        let mut empty = wrapper("paste", "error", None);
        empty.source = Some("page".into());
        empty.error = "paste2.org: the paste on paste2.org lists no links".into();
        let mut wrappers = vec![page, empty];
        for n in 0..6 {
            let mut c = wrapper(&format!("container{n}"), "captcha", None);
            c.source = Some("page".into());
            wrappers.push(c);
        }
        let cards = wrapper_cards(&wrappers, &[]);
        assert_eq!(cards.len(), 1);
        let card = &cards[0];
        assert_eq!((card.status, card.need_you, card.empty), ("captcha", 6, 1));
        assert!(card.open_id.starts_with("container"), "{}", card.open_id);
        assert_eq!(
            counts(&[], &wrappers, &cards, false).wrappers_alert,
            "attention"
        );
    }

    #[test]
    fn counts_a_wrappers_files_ready_to_start() {
        let wrappers = [wrapper("w", "done", None)];
        let from_w = |id: &str, status: &'static str| {
            let mut it = item(id, status);
            it.source = Some("w".into());
            it
        };
        let mut started = from_w("started", "paused");
        started.done = 10;
        let mut items = vec![
            from_w("ready1", "paused"),
            from_w("ready2", "paused"),
            started,
            from_w("going", "queued"),
            item("pasted", "paused"),
        ];
        super::items(&mut items, &[host("a.example")], &wrappers, false);
        let c = counts(&items, &wrappers, &wrapper_cards(&wrappers, &items), false);
        assert_eq!(c.ready, 2, "a wrapper's files nobody started");
    }

    #[test]
    fn hosts_group_and_rank() {
        let mut hs = vec![
            host("p1"),
            host("p2"),
            host("fast"),
            host("off"),
            host("capped"),
        ];
        hs[0].preferred = Some(0);
        hs[1].preferred = Some(1);
        hs[2].speed = 9.0;
        hs[0].speed = 3.0;
        hs[3].needs_account = true;
        hs[4].cap = Some(2);
        hosts(&mut hs);
        let groups: Vec<&str> = hs.iter().map(|h| h.group).collect();
        assert_eq!(
            groups,
            ["preferred", "preferred", "other", "other", "other"]
        );
        assert!(hs[0].movable && hs[1].movable && !hs[2].movable);
        assert_eq!((hs[4].slots, hs[4].limited), (2, true));
        assert_eq!((hs[2].slots, hs[2].limited), (FALLBACK_LIMIT, false));
    }

    #[test]
    fn sites_are_hosts_wrappers_pastes_or_pages() {
        assert_eq!(site_kind("pixeldrain.com"), "host");
        assert_eq!(site_kind("filecrypt.cc"), "wrapper");
        assert_eq!(site_kind("zpaste.net"), "paste");
        // A site Haul has no code for, files on it or not.
        assert_eq!(site_kind("files.example"), "site");
        assert_eq!(site_kind("links.example"), "site");
    }

    #[test]
    fn wrappers_pastes_and_sites_group_by_kind() {
        let mut hs = vec![host("a"), host("b"), host("w"), host("p"), host("s")];
        hs[0].preferred = Some(0);
        hs[0].speed = 1.0;
        for (h, kind) in hs[2..].iter_mut().zip(["wrapper", "paste", "site"]) {
            h.kind = kind;
            h.preferred = Some(1);
            h.speed = 9.0;
        }
        hs[4].disabled = true;
        hosts(&mut hs);
        let groups: Vec<&str> = hs.iter().map(|h| h.group).collect();
        assert_eq!(groups, ["preferred", "other", "wrapper", "paste", "site"]);
        assert!(!hs[0].movable, "only one host is preferred");
    }

    #[test]
    fn the_pool_in_words() {
        let none = || VpnFound {
            servers: 0,
            error: None,
        };
        let view = |line, proxies, vpn| super::super::PoolView {
            line,
            proxies,
            vpn,
            on: true,
        };
        let cfg = Config::default();
        let p = pool(&cfg, view(true, 0, 0), none());
        assert_eq!(p.label, "Line only");
        assert!(!p.configured && p.sources.is_empty() && p.why.is_none());

        let cfg = Config {
            proxy: Some("socks5://h:1080".into()),
            proxies: vec!["socks5://g:1080".into()],
            proxy_with_direct: true,
            ..Config::default()
        };
        let p = pool(&cfg, view(true, 2, 0), none());
        assert_eq!(p.label, "Line + 2 proxies");
        let names: Vec<&str> = p.sources.iter().map(|s| s.name).collect();
        assert_eq!(names, ["Your line", "Proxies"]);
        assert_eq!(p.sources[0].status, "One more address beside them");
        assert_eq!(p.sources[1].status, "2 in use");

        let cfg = Config {
            nordvpn_password: Some("pw".into()),
            nordvpn_servers: 0,
            ..Config::default()
        };
        let p = pool(&cfg, view(true, 0, 0), none());
        assert!(p.sources[1].problem);
        assert!(p.why.unwrap().contains("NordVPN has no servers chosen"));
    }
}
