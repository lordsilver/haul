//! When downloads start: a tick goes through the queue and starts what may
//! start, each on the mirror `pick` finds, and the hosts that say what they
//! still allow an address are asked for it, for the views and for `pick`.

use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use tokio_util::sync::CancellationToken;

use super::{Engine, Event, Exit, OFF_ONLY, State, display_name, exit_name, noted_quietly};
use crate::{
    hosts::{self, Cx},
    item::Status,
};

/// Why a row waits when every page it could go to is on a host that asked
/// this address to wait.
pub(super) const ASKED_TO_WAIT: &str = "asks this address to wait";

impl Engine {
    // ---- scheduler

    /// Asks the hosts that tell ([`hosts::Known::tells_allowance`]),
    /// through each exit, how much they still allow that address at full
    /// speed today, while the queue holds their links: at once when one is
    /// added, then once a minute, as downloads use it up.
    pub(super) async fn watch_quotas(self) {
        let mut changes = self.watch();
        let mut checked: HashMap<String, Instant> = HashMap::new();
        loop {
            let wanted: Vec<String> = {
                let st = self.lock();
                let mut hosts: Vec<String> = st
                    .items
                    .values()
                    .filter(|i| i.status != Status::Done)
                    .flat_map(|i| i.mirrors.iter().map(|m| hosts::host_of(m)))
                    .filter(|h| hosts::known(h).is_some_and(|k| k.tells_allowance))
                    .collect();
                hosts.sort();
                hosts.dedup();
                hosts
            };
            let cfg = self.cfg();
            for host in wanted {
                let due = checked
                    .get(&host)
                    .is_none_or(|t| t.elapsed() >= Duration::from_secs(60));
                if !due || cfg.is_off(&host) {
                    continue;
                }
                checked.insert(host.clone(), Instant::now());
                let exits = self.exits_for(&host, &self.lock());
                for exit in exits {
                    let http = self.clients().on(exit);
                    let cx = Cx {
                        http: &http,
                        cfg: &cfg,
                        tokens: &self.0.tokens,
                        key_service: &http,
                    };
                    let quota = hosts::allowance(&cx, &host).await;
                    let mut st = self.lock();
                    let lane = st.lane(&host, exit);
                    // Less used than before: another address (a router that
                    // got a new one, say) or a new day. What held for the
                    // old one does not.
                    if let (Some(old), Some(new)) = (lane.quota, quota)
                        && new.used < old.used
                    {
                        tracing::info!(exit = %exit_name(exit), "{host} sees a fresh address: its limits start over");
                        lane.fresh_start();
                    }
                    if quota.is_some() && lane.quota != quota {
                        if let Some(q) = quota {
                            tracing::debug!(exit = %exit_name(exit), used = q.used, limit = q.limit, "{host} allowance");
                        }
                        lane.quota = quota;
                        self.touch(&mut st);
                    }
                }
            }
            // A link just added is asked about at once.
            tokio::select! {
                _ = self.0.shutdown.cancelled() => return,
                _ = tokio::time::sleep(Duration::from_secs(60)) => {}
                _ = changes.changed() => {}
            }
        }
    }

    pub(super) async fn scheduler(self) {
        loop {
            tokio::select! {
                _ = self.0.shutdown.cancelled() => return,
                _ = self.0.wake.notified() => {}
                _ = tokio::time::sleep(Duration::from_millis(400)) => {}
            }
            self.schedule();
        }
    }

    fn schedule(&self) {
        let mut events = Vec::new();
        {
            let mut st = self.lock();
            if st.paused || st.moving || st.reconnecting {
                return;
            }
            let now = Instant::now();
            let mut changed = self.auto_retry(&mut st, now);
            let mut running = st.items.values().filter(|i| i.running()).count();
            // What downloads now against max_active_gb, by full size.
            let size_cap = self.size_cap();
            let mut busy: u64 = st
                .items
                .values()
                .filter(|i| i.running())
                .map(|i| i.size)
                .sum();
            // Downloads with data on disk go first, the nearest to done
            // first; the rest keep the queue's order.
            // (By bytes on disk where the size is not known yet.)
            let mut ids: Vec<(String, f64, u64)> = st
                .items
                .iter()
                .map(|(k, i)| {
                    let near = if i.size > 0 {
                        i.done as f64 / i.size as f64
                    } else {
                        0.0
                    };
                    (k.clone(), near, i.done)
                })
                .collect();
            ids.sort_by(|a, b| {
                (b.2 > 0)
                    .cmp(&(a.2 > 0))
                    .then(b.1.total_cmp(&a.1))
                    .then(b.2.cmp(&a.2))
            });
            for (id, _, _) in ids {
                if running >= st.max_total {
                    break;
                }
                let Some(it) = st.items.get(&id) else {
                    continue;
                };
                if !matches!(it.status, Status::Queued | Status::Waiting)
                    || !it.probed
                    || it.running()
                    || it.not_before.is_some_and(|t| now < t)
                {
                    continue;
                }
                // In queue order, as with max_total: the next file waits for
                // room rather than let smaller ones behind it go first.
                if size_cap > 0 && busy > 0 && busy + it.size > size_cap {
                    break;
                }
                let size = it.size;
                let before = it.status;
                match self.pick(&mut st, &id, now) {
                    Some((url, exit)) => {
                        let host = hosts::host_of(&url);
                        let via = exit_name(exit);
                        tracing::debug!(item = %id, url = %url, "starting from {host} via {via}");
                        st.lane(&host, exit).active += 1;
                        let cancel = CancellationToken::new();
                        let it = st.items.get_mut(&id).expect("item checked above");
                        it.host = host.clone();
                        it.status = Status::Resolving;
                        it.task = Some(cancel.clone());
                        it.exit = Some(exit);
                        running += 1;
                        busy += size;
                        changed = true;
                        tokio::spawn(self.clone().work(id, url, host, exit, cancel));
                    }
                    None => {
                        let it = &st.items[&id];
                        if it.status != before {
                            changed = true;
                            // A copied page on trial is nobody's download yet.
                            if it.tentative {
                                continue;
                            }
                            if it.status == Status::Captcha && !it.page_retrying {
                                events.push(Event::NeedsUser {
                                    id: id.clone(),
                                    name: display_name(it),
                                });
                            } else if it.status == Status::Error && !noted_quietly(&st, it) {
                                events.push(Event::Failed {
                                    id: id.clone(),
                                    name: display_name(it),
                                    error: it.error.clone(),
                                });
                            }
                        }
                    }
                }
            }
            if changed {
                self.touch(&mut st);
            }
        }
        for e in events {
            let _ = self.0.events.send(e);
        }
    }

    /// The best mirror whose host has a free slot, ranked by measured speed.
    /// Hosts the user turned off are never picked. Parks the item when no
    /// mirror is left.
    fn pick(&self, st: &mut State, id: &str, now: Instant) -> Option<(String, Exit)> {
        let cfg = self.cfg();
        let pages = self.pages_pass();
        let off = |m: &String| cfg.is_off(&hosts::host_of(m));
        let it = &st.items[id];
        // A mirror whose page the desktop app passes by itself, waiting its
        // turn in the browser: the file waits for it rather than go to a
        // host Haul knows nothing of, or one measured slower, unless that
        // host has asked this address to wait.
        let pass_host = (pages && !it.page_wants_person)
            .then(|| {
                it.manual_urls
                    .iter()
                    .find(|m| {
                        st.passes(m)
                            && !off(m)
                            && !it.failed.contains(*m)
                            && self.pass_held_until(st, m, now).is_none()
                    })
                    .map(|m| hosts::host_of(m))
            })
            .flatten();
        let candidates: Vec<String> = it
            .mirrors
            .iter()
            .filter(|m| !it.failed.contains(*m) && !it.manual_urls.contains(*m) && !off(m))
            .cloned()
            .collect();
        if candidates.is_empty() {
            let asks = it.manual_urls.iter().any(|m| !off(m));
            let only_off: Vec<String> = it
                .mirrors
                .iter()
                .filter(|m| !it.failed.contains(*m) && off(m))
                .map(|m| hosts::host_of(m))
                .collect();
            let held = self
                .pages_held_until(st, it, now)
                .zip(it.mirrors.iter().find(|m| it.manual_urls.contains(*m)));
            let held = held.map(|(until, page)| (until, hosts::host_of(page)));
            let retry_at = it
                .retry_at
                .or_else(|| {
                    it.page_wants_person
                        .then(|| self.page_retry_at(st, it, now))
                        .flatten()
                })
                .filter(|_| asks);
            let it = st.items.get_mut(id).expect("caller checked");
            if let Some((until, host)) = held {
                // Nothing to do but wait for the host, and no use asking a
                // person: its page would say the same to them.
                it.status = Status::Waiting;
                it.not_before = Some(until);
                it.error = format!("{host}: {ASKED_TO_WAIT}");
            } else if asks && it.status != Status::Error {
                it.status = Status::Captcha;
                it.retry_at = retry_at;
                // Said while it held, and it no longer does.
                if it.error.ends_with(ASKED_TO_WAIT) {
                    it.error.clear();
                }
            } else if !only_off.is_empty() {
                // Nothing to retry until the user turns a host back on.
                let mut names = only_off;
                names.dedup();
                it.status = Status::Error;
                it.error = format!("{OFF_ONLY}{}", names.join(", "));
                tracing::info!(item = %it.id, "{}", it.error);
            } else {
                it.status = Status::Error;
                if it.error.is_empty() {
                    it.error = "all mirrors failed".into();
                }
                self.plan_auto_retry(it, now);
            }
            return None;
        }
        // The host the user picked for this file, while it has it: waited
        // for when only busy, passed over while it cools down or once its
        // link failed, the others standing by.
        if let Some(want) = st.items[id].prefer.clone()
            && let Some(m) = candidates.iter().find(|m| hosts::host_of(m) == want)
        {
            let exits = self.exits_for(&want, st);
            if !st.cooling(&want, &exits, now) {
                return st
                    .open_exit(&want, &exits, &cfg, now)
                    .map(|exit| (m.clone(), exit));
            }
        }
        // Preferred hosts first, in the user's order. One that is only busy
        // is waited for; one cooling down is passed over like any other.
        let mut preferred: Vec<(usize, String)> = Vec::new();
        for m in &candidates {
            let name = hosts::host_of(m);
            let exits = self.exits_for(&name, st);
            if let Some(rank) = cfg.preference(&name)
                && !st.cooling(&name, &exits, now)
            {
                preferred.push((rank, m.clone()));
            }
        }
        if !preferred.is_empty() {
            preferred.sort_by_key(|(rank, _)| *rank);
            for (_, m) in preferred {
                let name = hosts::host_of(&m);
                let exits = self.exits_for(&name, st);
                if let Some(exit) = st.open_exit(&name, &exits, &cfg, now) {
                    return Some((m, exit));
                }
            }
            return None;
        }
        let pass_speed = pass_host
            .as_ref()
            .map(|h| st.host(h).ema)
            .unwrap_or_default();
        let mut best: Option<(String, Exit, f64)> = None;
        let mut held = false;
        for m in candidates {
            let name = hosts::host_of(&m);
            if pass_host.is_some() {
                let known = hosts::known(&name).is_some();
                let ema = st.host(&name).ema;
                if !known || ema < pass_speed {
                    held = true;
                    continue;
                }
            }
            let exits = self.exits_for(&name, st);
            let Some(exit) = st.open_exit(&name, &exits, &cfg, now) else {
                continue;
            };
            let score = st.host(&name).score(now);
            if best.as_ref().is_none_or(|(_, _, s)| score > *s) {
                best = Some((m, exit, score));
            }
        }
        if best.is_none() && held {
            // Shown as passing until its page's turn comes.
            if let Some(it) = st.items.get_mut(id) {
                it.status = Status::Captcha;
            }
        }
        best.map(|(m, exit, _)| (m, exit))
    }
}
