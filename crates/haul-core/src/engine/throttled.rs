//! What the engine does about a host that slows this address down: running
//! transfers are looked at every tick against the rules in [`crate::throttle`],
//! and one found throttled puts its host on cooldown, goes on from a mirror
//! that can take it now, or keeps going and says why. A redial, the other way
//! out, is `reconnect.rs`'s.

use std::time::Instant;

use super::{Engine, Exit, State, reconnect::DIRECT, transfer::drop_used_pre};
use crate::{
    config::Config,
    hosts,
    item::{Item, Slowed},
    throttle::{self, Around, OwnLimit, Pace, Reading, Throttling, Verdict},
};

/// Ticks late past which the watcher itself was held up, so what its readings
/// span isn't the transfers' doing.
const FROZEN_TICKS: u32 = 5;

/// One running transfer, read for the others to be compared with.
struct Run {
    id: String,
    host: String,
    exit: Exit,
    /// Its speed over a whole window past its grace; `None` before that.
    window: Option<f64>,
    /// Its speed just now.
    recent: Option<f64>,
    past_grace: bool,
    slowed: bool,
    left: Option<u64>,
}

fn kb(speed: f64) -> String {
    format!("{:.0} KB/s", speed / 1024.0)
}

fn mb(bytes: u64) -> String {
    format!("{:.0} MB", bytes as f64 / 1e6)
}

impl Engine {
    /// Looks at the running transfers every tick.
    pub(super) async fn watch_speeds(self) {
        let mut last = Instant::now();
        loop {
            let tick = self.cfg().throttling.tick;
            if !self.idle(tick).await {
                return;
            }
            // Far later than asked: the process stood still (a stalled machine,
            // a computer asleep), and that time says nothing about any host.
            let frozen = last.elapsed() > tick * FROZEN_TICKS;
            last = Instant::now();
            self.judge_speeds(frozen);
        }
    }

    fn judge_speeds(&self, frozen: bool) {
        let cfg = self.cfg();
        let t = &cfg.throttling;
        // Before the state's lock: the two are always taken in this order.
        let may_redial = self.redials_on_its_own(&cfg);
        let rate = self.0.limiter.rate();
        let mut st = self.lock();
        if !cfg.leave_slow_hosts {
            self.forget_slowness(&mut st);
            return;
        }
        // Everything is stopping or about to start again.
        if st.paused || st.moving || st.reconnecting {
            return;
        }
        let now = Instant::now();
        let mut total = 0.0;
        let mut runs = Vec::new();
        for it in st.items.values_mut() {
            let (Some(exit), Some(pace)) = (it.exit, it.pace.as_mut()) else {
                continue;
            };
            if frozen {
                *pace = Pace::new(now, it.done);
            }
            pace.sample(now, it.done, t);
            total += it.speed;
            runs.push(Run {
                id: it.id.clone(),
                host: it.host.clone(),
                exit,
                window: pace.over_window(now, t),
                recent: pace.recent(now, t),
                past_grace: pace.past_grace(now, t),
                slowed: it.slowed.is_some(),
                left: (it.size > 0).then(|| it.size.saturating_sub(it.done)),
            });
        }
        let limit = (rate > 0.0).then_some(OwnLimit {
            rate,
            total,
            running: runs.len(),
        });
        let mut changed = false;
        for run in &runs {
            let Some(speed) = run.window else { continue };
            let reading = Reading {
                speed,
                left: run.left,
                normal: st.host(&run.host).normal,
            };
            let others = || runs.iter().filter(|o| o.host != run.host);
            let around = Around {
                // Any exit: every one of them rides on the line.
                well_elsewhere: others().any(|o| o.recent.is_some_and(|s| s >= t.slow)),
                // One already found throttled is explained by its own host.
                slow_elsewhere: others().any(|o| {
                    o.exit == run.exit
                        && o.past_grace
                        && !o.slowed
                        && o.recent.is_some_and(|s| s < t.slow)
                }),
                line_best: st
                    .hosts
                    .iter()
                    .filter(|(name, _)| **name != run.host)
                    .map(|(_, h)| h.normal.max(h.ema))
                    .fold(0.0, f64::max),
                limit,
            };
            let Some(pace) = st.items.get_mut(&run.id).and_then(|it| it.pace.as_mut()) else {
                continue;
            };
            let slow_for = pace.slow_for(now, throttle::is_slow(&reading, t));
            let verdict = throttle::judge(&reading, &around, slow_for, t);
            if pace.verdict != verdict {
                tracing::debug!(
                    item = %run.id,
                    "{} at {}: {}",
                    run.host,
                    kb(speed),
                    verdict.as_str()
                );
                pace.verdict = verdict;
            }
            match verdict {
                Verdict::Fine => {
                    st.host(&run.host).ran_well(speed);
                    st.lane(&run.host, run.exit).strikes = 0;
                    changed |= self.not_slowed(&mut st, run, true, now);
                }
                Verdict::OwnLimit | Verdict::Line => {
                    changed |= self.not_slowed(&mut st, run, false, now);
                }
                Verdict::Throttled { sure } => {
                    changed |= self.throttled(&mut st, run, &runs, speed, sure, may_redial, now);
                }
                Verdict::TooEarly | Verdict::NearlyDone | Verdict::Unsure => {}
            }
        }
        if changed {
            self.publish(st);
        }
    }

    /// Switched off: nothing shows or holds anything back for slowness.
    fn forget_slowness(&self, st: &mut State) {
        let mut changed = false;
        for it in st.items.values_mut() {
            changed |= it.slowed.take().is_some();
            it.slow_on.clear();
        }
        for lane in st.lanes.values_mut() {
            if lane.slowing {
                lane.end_slowing();
                lane.strikes = 0;
                changed = true;
            }
        }
        if changed {
            self.touch(st);
        }
    }

    /// Row `run` isn't being held back by its host: it says so no more. With
    /// `recovered` it runs well, and so its host's cooldown for slowness ends
    /// when no other transfer on that address still shows it.
    fn not_slowed(&self, st: &mut State, run: &Run, recovered: bool, now: Instant) -> bool {
        let mut changed = false;
        if let Some(it) = st.items.get_mut(&run.id)
            && it.slowed.take().is_some()
        {
            tracing::info!(item = %run.id, "{} no longer slows this download", run.host);
            changed = true;
        }
        let still_slowed = st
            .items
            .values()
            .any(|i| i.slowed.is_some() && i.host == run.host && i.exit == Some(run.exit));
        let lane = st.lane(&run.host, run.exit);
        if recovered && !still_slowed && lane.slowing && lane.cooling(now) {
            lane.end_slowing();
            tracing::info!("{} runs well again: its cooldown ends", run.host);
            changed = true;
        }
        changed
    }

    /// Row `run`'s host is limiting it: remembers that, and has the row go on
    /// from a mirror that can take it now, or stay and say why.
    #[allow(clippy::too_many_arguments)]
    fn throttled(
        &self,
        st: &mut State,
        run: &Run,
        runs: &[Run],
        speed: f64,
        sure: bool,
        may_redial: bool,
        now: Instant,
    ) -> bool {
        let cfg = self.cfg();
        let t = &cfg.throttling;
        let (host, exit) = (run.host.as_str(), run.exit);
        let first = st.items.get(&run.id).is_some_and(|i| i.slowed.is_none());
        let Some(it) = st.items.get(&run.id) else {
            return false;
        };
        let url = it.url.clone().unwrap_or_default();
        let pinned = it.prefer.as_deref() == Some(host);
        // While draining nothing starts: going elsewhere would only stop it.
        let next = (!pinned && st.draining.is_none())
            .then(|| self.next_mirror(st, &run.id, host, exit, speed, now))
            .flatten();
        // Another transfer doing well on the same address: this one's trouble
        // isn't the address's.
        let address_fine = runs.iter().any(|o| {
            o.id != run.id
                && o.host == run.host
                && o.exit == run.exit
                && o.recent.is_some_and(|s| s >= t.slow)
        });
        // Unsure, it may be a slow line or a host that is slow for everyone:
        // nothing is held back for that, unless the row leaves and must not
        // come straight back.
        if !address_fine && (sure || next.is_some()) {
            remember_slowing(st, host, exit, sure, t, now);
            if first {
                self.hold_passes(st, now);
            }
        }
        if first && sure {
            // As slow on a new address as on the old one.
            let h = st.host(host);
            if h.redialed_for && !h.redial_useless {
                h.redial_useless = true;
                h.redial_useless_at = super::unix_now();
            }
        }
        let left = run.left.map_or("?".to_string(), mb);
        let it = st.items.get_mut(&run.id).expect("found above");
        // It may come back here if nothing else works out.
        it.slow_on.insert(url.clone());
        if let Some(next) = next {
            tracing::info!(
                item = %run.id,
                "{host} is slowing this download ({}, {left} left): going on from {next}, keeping its {}",
                kb(speed),
                mb(it.done)
            );
            if it.manual {
                drop_used_pre(it, &url);
            }
            it.slowed = None;
            // Not looked at again while it stops.
            it.pace = None;
            it.host.clear();
            it.cancel();
            return true;
        }
        let why = self.why_it_stays(st, &run.id, host, exit, sure, may_redial);
        let it = st.items.get_mut(&run.id).expect("found above");
        let slowed = Slowed {
            host: host.to_string(),
            sure,
            why,
        };
        if it.slowed.as_ref() == Some(&slowed) {
            return false;
        }
        if first {
            tracing::info!(
                item = %run.id,
                "{host} is slowing this download ({}, {left} left): {why}",
                kb(speed)
            );
        }
        it.slowed = Some(slowed);
        true
    }

    /// What can take row `id` over from `host` now: another mirror with a free
    /// slot, or the same host through another exit, which is a new address for
    /// free. Not one cooling down or that has failed, wanting a person (a
    /// throttled download beats a row under Needs you), or measured no better.
    fn next_mirror(
        &self,
        st: &mut State,
        id: &str,
        host: &str,
        exit: Exit,
        speed: f64,
        now: Instant,
    ) -> Option<String> {
        let cfg = self.cfg();
        let it = st.items.get(id)?;
        let mirrors = it.mirrors.clone();
        for m in mirrors {
            let name = hosts::host_of(&m);
            if !self.could_serve(st, id, &m, &cfg, now) {
                continue;
            }
            let same = name == host;
            // Normal says what it can do; `ema` is all there is before that.
            let known = st
                .hosts
                .get(&name)
                .map_or(0.0, |h| if h.normal > 0.0 { h.normal } else { h.ema });
            if !same && known > 0.0 && known < speed * 2.0 {
                continue;
            }
            let exits = self.exits_for(&name, st);
            match st.open_exit(&name, &exits, &cfg, now) {
                Some(open) if !same || open != exit => {
                    return Some(if same {
                        format!("{name} through another address")
                    } else {
                        name
                    });
                }
                _ => {}
            }
        }
        None
    }

    /// Whether mirror `m` of row `id` could serve it without a person.
    fn could_serve(&self, st: &State, id: &str, m: &String, cfg: &Config, now: Instant) -> bool {
        let Some(it) = st.items.get(id) else {
            return false;
        };
        if it.failed.contains(m) || cfg.is_disabled(&hosts::host_of(m)) {
            return false;
        }
        let pages = self.pages_pass();
        if it.manual_urls.contains(m) {
            return pages
                && !it.page_wants_person
                && st.passes(m)
                && self.pass_held_until(st, m, now).is_none();
        }
        !hosts::asks_a_person(m, cfg, pages)
    }

    /// Whether row `it` has a host other than page `url`'s that could take it
    /// now: not one cooling down, that has failed, or that wants a person.
    pub(super) fn has_another_way(&self, st: &State, it: &Item, url: &str, now: Instant) -> bool {
        let cfg = self.cfg();
        let host = hosts::host_of(url);
        it.mirrors.iter().any(|m| {
            let name = hosts::host_of(m);
            name != host
                && self.could_serve(st, &it.id, m, &cfg, now)
                && !self.host_cooling(st, &name, now)
        })
    }

    /// Why row `id` stays on `host`, as the row says it.
    fn why_it_stays(
        &self,
        st: &State,
        id: &str,
        host: &str,
        exit: Exit,
        sure: bool,
        may_redial: bool,
    ) -> &'static str {
        let cfg = self.cfg();
        let Some(it) = st.items.get(id) else {
            return "";
        };
        let useless = st.hosts.get(host).is_some_and(|h| h.redial_useless);
        if may_redial && sure && exit == DIRECT && !useless {
            return "a new address once other downloads finish";
        }
        if it.prefer.as_deref() == Some(host) {
            return "you picked this host";
        }
        let others: Vec<&String> = it
            .mirrors
            .iter()
            .filter(|m| hosts::host_of(m) != host)
            .filter(|m| !it.failed.contains(*m) && !cfg.is_disabled(&hosts::host_of(m)))
            .collect();
        let now = Instant::now();
        if others.is_empty() {
            "no other host has it"
        } else if others
            .iter()
            .all(|m| !self.could_serve(st, id, m, &cfg, now))
        {
            "its other hosts need you"
        } else {
            "its other hosts aren't free"
        }
    }
}

/// Puts the host on cooldown for this address, or keeps it there: a row with
/// somewhere else to go doesn't start on it or pass its pages. A host still
/// slowing it isn't a new strike; its cooldown only stops running out.
fn remember_slowing(
    st: &mut State,
    host: &str,
    exit: Exit,
    sure: bool,
    t: &Throttling,
    now: Instant,
) {
    let lane = st.lane(host, exit);
    let still = lane.slowing && lane.cooling(now);
    let strikes = if still {
        lane.strikes
    } else {
        lane.strikes + 1
    };
    let until = now + t.cooldown_for(strikes);
    // A longer wait the host itself asked for stands.
    if !still && lane.cooldown.is_some_and(|c| c >= until) {
        return;
    }
    if !still {
        lane.strikes = strikes;
        lane.slowing = true;
        lane.slowing_sure = false;
    }
    lane.cooldown = Some(until);
    lane.slowing_sure |= sure;
}
