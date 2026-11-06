//! A new public address for this machine's own line, from the router or a
//! command of the user's: when a host has slowed the address for the day
//! or asked it for a long wait, and no proxy can take over.
//!
//! While the router redials, nothing new starts and the downloads on this
//! line stop; they resume from where they were once the line is back, and
//! what was learned about the old address (limits, waits, allowance) goes.
//! Automatic redials need the setting, come at most every ten minutes, and
//! stop after a redial brings the same address back: that line keeps it. One
//! on its own lets the downloads running well finish first ([`Engine::drain`]);
//! nothing that runs well is cut off for a host that isn't its own.

use std::time::{Duration, Instant};

use serde::Serialize;

use super::{Engine, Exit};
use crate::{
    config::secret,
    hosts,
    item::{Status, Trouble},
    router::{self, RedialError, Router},
};

/// A host's wait for this address worth a redial.
const LONG_WAIT: Duration = Duration::from_secs(300);
/// This machine's own connection.
pub(super) const DIRECT: Exit = 0;

#[derive(Debug, Default)]
pub(super) struct Reconnects {
    busy: bool,
    last: Option<Instant>,
    /// What the last redial did, for the settings view.
    note: Option<String>,
    /// When the note was written, in seconds since the Unix epoch.
    note_at: u64,
    failed: bool,
    /// A redial brought the same address back: automatic ones stop.
    keeps_address: bool,
    router: Option<Router>,
    looked: bool,
}

impl Reconnects {
    /// What the last attempt came to, for the settings view, from now on.
    fn note(&mut self, note: String, failed: bool) {
        self.note = Some(note);
        self.note_at = super::unix_now();
        self.failed = failed;
    }
}

/// What the settings view shows about getting a new address.
#[derive(Debug, Clone, Serialize, PartialEq, ts_rs::TS)]
#[ts(export)]
pub struct ReconnectView {
    /// The router found, as it names itself.
    pub router: Option<String>,
    pub busy: bool,
    pub note: Option<String>,
    /// When the note was written, in seconds since the Unix epoch: it stays
    /// until the next attempt, so the view says how old it is.
    pub note_at: Option<u64>,
    /// Redials on its own when the address holds downloads back.
    pub auto: bool,
    /// Why no redial can work as things are set (a FRITZ!Box without its
    /// login, no router and no command); the switch stays off meanwhile.
    pub blocked: Option<String>,
    /// The last redial failed: its note is an error to show.
    pub failed: bool,
    /// A redial on its own waits for downloads to finish, and nothing new
    /// starts: why it would help.
    pub draining: Option<String>,
}

impl Engine {
    fn reconnects(&self) -> std::sync::MutexGuard<'_, Reconnects> {
        self.0.reconnects.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn reconnect_view(&self) -> ReconnectView {
        let r = self.reconnects();
        let cfg = self.cfg();
        let blocked = blocker(&cfg, &r);
        ReconnectView {
            router: r.router.as_ref().map(|r| r.model.clone()),
            busy: r.busy,
            note: r.note.clone(),
            note_at: r.note.as_ref().map(|_| r.note_at),
            auto: cfg.reconnect && !r.keeps_address && blocked.is_none(),
            blocked,
            failed: r.failed,
            draining: None,
        }
    }

    /// Whether a redial could come on its own, as things are set.
    pub(super) fn redials_on_its_own(&self, cfg: &crate::Config) -> bool {
        let r = self.reconnects();
        cfg.reconnect && !r.keeps_address && blocker(cfg, &r).is_none()
    }

    /// Why no redial can work as things are set, if none can.
    pub fn redial_blocker(&self, cfg: &crate::Config) -> Option<String> {
        blocker(cfg, &self.reconnects())
    }

    /// Looks for the router again, as the settings do when shown.
    pub async fn find_router(&self) -> Option<Router> {
        self.reconnects().looked = false;
        self.router().await
    }

    /// The router on this network, looked for once and remembered.
    pub async fn router(&self) -> Option<Router> {
        {
            let r = self.reconnects();
            if r.looked {
                return r.router.clone();
            }
        }
        let found = router::find(
            &self.clients().on(DIRECT).client,
            &self.cfg().endpoints.router,
        )
        .await;
        if let Some(found) = &found {
            tracing::info!("router: {}", found.model);
        }
        {
            let mut r = self.reconnects();
            r.looked = true;
            r.router = found.clone();
        }
        // Never with the reconnect lock held: a snapshot takes the two
        // the other way round.
        self.touch(&mut self.lock());
        found
    }

    /// Gets this machine's line a new public address now: stops the
    /// downloads on it, has the router redial (or runs the user's command),
    /// and lets them go on. Returns the new address, or why there is none.
    pub async fn reconnect(&self, why: &str) -> Result<String, String> {
        {
            let mut r = self.reconnects();
            if r.busy {
                return Err("already getting a new address".into());
            }
            // One that cannot work stops no download.
            if let Some(why_not) = blocker(&self.cfg(), &r) {
                r.note(format!("No new address: {why_not}"), true);
                drop(r);
                self.touch(&mut self.lock());
                return Err(why_not);
            }
            r.busy = true;
        }
        tracing::info!("getting a new address: {why}");
        self.hold_direct(true);
        let result = self.redial().await;
        self.hold_direct(false);
        let mut r = self.reconnects();
        r.busy = false;
        r.last = Some(Instant::now());
        match &result {
            Ok(address) => {
                tracing::info!("new address {address}");
                r.note(format!("Got a new address ({why})"), false);
                drop(r);
                let mut st = self.lock();
                for ((_, exit), lane) in st.lanes.iter_mut() {
                    if *exit == DIRECT {
                        lane.fresh_start();
                        lane.quota = None;
                    }
                }
                // Rows waiting out a limit on the old address go again now;
                // one still held elsewhere is parked again on its turn.
                for it in st.items.values_mut() {
                    if it.status == Status::Waiting && it.trouble == Trouble::AskedToWait {
                        it.not_before = None;
                    }
                    it.slow_on.clear();
                }
                self.touch(&mut st);
            }
            Err(e) => {
                tracing::warn!("no new address: {e}");
                r.keeps_address |= matches!(e, RedialError::SameAddress(_));
                r.note(format!("No new address: {e}"), true);
                // Learned: this router redials only with its login.
                if matches!(e, RedialError::NeedsLogin(_))
                    && let Some(router) = r.router.as_mut()
                {
                    router.upnp_refused = true;
                }
                drop(r);
                self.touch(&mut self.lock());
                self.emit([super::Event::Notice {
                    title: "Could not get a new IP".into(),
                    body: e.to_string(),
                }]);
            }
        }
        self.0.wake.notify_one();
        result.map_err(|e| e.to_string())
    }

    /// Stops new starts and the downloads on this machine's own line, or
    /// lets them go again.
    fn hold_direct(&self, hold: bool) {
        let mut st = self.lock();
        st.reconnecting = hold;
        if hold {
            for it in st.items.values_mut() {
                if it.exit == Some(DIRECT)
                    && let Some(task) = &it.task
                {
                    task.cancel();
                }
            }
        }
        self.touch(&mut st);
    }

    /// The address the internet sees for this machine's own line, asked
    /// of a public echo service through it.
    async fn public_address(&self) -> Option<String> {
        let http = self.clients().on(DIRECT).client;
        let url = self.cfg().endpoints.ip_echo.clone();
        let text = http
            .get(url)
            .timeout(Duration::from_secs(10))
            .send()
            .await
            .ok()?
            .text()
            .await
            .ok()?;
        let ip = text.trim();
        ip.parse::<std::net::IpAddr>().ok().map(|_| ip.to_string())
    }

    /// Has the line redialed ([`Engine::redial_line`]) and confirms with
    /// the address the internet sees, before and after, that it changed:
    /// a router's own address can change behind a carrier's shared one
    /// that does not, and a command proves nothing by itself.
    async fn redial(&self) -> Result<String, RedialError> {
        // The stopped downloads let go of the line first.
        for _ in 0..50 {
            if !self.lock().items.values().any(|i| i.exit == Some(DIRECT)) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        let before = self.public_address().await;
        let reported = self.redial_line().await?;
        let Some(before) = before else {
            return Ok(reported);
        };
        // The line may take a moment after the router says it is up.
        let mut after = None;
        for _ in 0..15 {
            after = self.public_address().await;
            if after.as_ref().is_some_and(|a| *a != before) {
                break;
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        match after {
            Some(after) if after != before => Ok(after),
            // The router has a public address of its own, not the one hosts
            // see: this machine's traffic leaves some other way, which a
            // redial can't change. A private or shared one is the carrier's
            // network in between, as before.
            Some(after) if is_public(&reported) && reported != after => {
                Err(RedialError::Other(format!(
                    "downloads leave through something other than the router, such as a VPN on this PC: hosts see {after}, the router's own address is {reported}, so a new one changes nothing for them"
                )))
            }
            Some(after) => Err(RedialError::SameAddress(format!(
                "the same address came back: the internet still sees {after} (the router reported {reported})"
            ))),
            None => Ok(reported),
        }
    }

    async fn redial_line(&self) -> Result<String, RedialError> {
        let cfg = self.cfg();
        let http = self.clients().on(DIRECT).client;
        if let Some(command) = secret(&cfg.reconnect_command) {
            let router = self.router().await;
            let before = match &router {
                Some(r) => r.address(&http).await,
                None => None,
            };
            run(command).await?;
            return match router {
                Some(r) => r.settled(&http, before).await,
                None => Ok("unknown (no router to ask)".into()),
            };
        }
        let router = self.router().await.ok_or_else(|| {
            RedialError::Other(
                "no router answered on this network; set a reconnect command instead".into(),
            )
        })?;
        let login = match (cfg.account("router_user"), cfg.account("router_password")) {
            (user, Some(password)) => Some((user.unwrap_or_default(), password)),
            _ => None,
        };
        router.redial(&http, login).await
    }

    /// Why a new address would help now, when automatic redials are on: the
    /// address is what holds a download back, it has no other way to go, and
    /// no proxy can take over. A running download is held back only by the
    /// host it downloads from, and one waiting only when every one of its hosts
    /// holds this address back: a row doing well, or that can go elsewhere,
    /// wants nothing whatever its other mirrors are up to.
    pub(super) fn wants_new_address(&self) -> Option<String> {
        let cfg = self.cfg();
        if !cfg.reconnect {
            return None;
        }
        {
            let r = self.reconnects();
            if r.busy
                || r.keeps_address
                || blocker(&cfg, &r).is_some()
                || r.last
                    .is_some_and(|t| t.elapsed() < cfg.throttling.redial_gap)
            {
                return None;
            }
        }
        let st = self.lock();
        let now = Instant::now();
        // The line can help this host, or it no longer needs to: the host is
        // reached through it, no proxy is free for it, and a redial made for
        // it before didn't already show it doesn't limit by address.
        let line_helps = |host: &str| {
            let exits = self.exits_for(host, &st);
            let lane = |e: Exit| st.lanes.get(&(host.to_string(), e));
            let proxy_free = exits
                .iter()
                .any(|&e| e != DIRECT && lane(e).is_none_or(|l| !l.slowed() && !l.cooling(now)));
            let useless = st.hosts.get(host).is_some_and(|h| h.redial_useless);
            exits.contains(&DIRECT) && !proxy_free && !useless
        };
        let quota_slowed = |host: &str| {
            st.lanes
                .get(&(host.to_string(), DIRECT))
                .and_then(|l| l.quota)
                .filter(|q| q.slowed())
                .map(|q| {
                    format!(
                        "{host} slowed this address to {} KB/s for the day",
                        q.slowed_to / 1024
                    )
                })
        };
        for it in st.items.values() {
            if !matches!(
                it.status,
                Status::Queued | Status::Waiting | Status::Downloading
            ) {
                continue;
            }
            if it.running() {
                let host = it.host.as_str();
                if it.exit != Some(DIRECT) || !line_helps(host) {
                    continue;
                }
                if let Some(why) = quota_slowed(host) {
                    return Some(why);
                }
                let slowing = st
                    .lanes
                    .get(&(host.to_string(), DIRECT))
                    .is_some_and(|l| l.slowing && l.slowing_sure);
                if slowing && it.slowed.as_ref().is_some_and(|s| s.sure) {
                    return Some(format!("{host} is slowing this address"));
                }
                continue;
            }
            let mirrors: Vec<String> = it
                .mirrors
                .iter()
                .filter(|m| !it.failed.contains(*m))
                .map(|m| hosts::host_of(m))
                .filter(|h| !cfg.is_off(h))
                .collect();
            let held = |host: &String| {
                self.exits_for(host, &st).iter().all(|&e| {
                    st.lanes
                        .get(&(host.clone(), e))
                        .is_some_and(|l| l.slowed() || l.cooling(now))
                })
            };
            if mirrors.is_empty() || !mirrors.iter().all(held) {
                continue;
            }
            for host in mirrors.iter().filter(|h| line_helps(h)) {
                if let Some(why) = quota_slowed(host) {
                    return Some(why);
                }
                let Some(direct) = st.lanes.get(&(host.clone(), DIRECT)) else {
                    continue;
                };
                // A host's slowness nothing else confirms isn't worth cutting
                // the line for.
                if direct.slowing && !direct.slowing_sure {
                    continue;
                }
                if let Some(until) = direct.cooldown
                    && until.saturating_duration_since(now) >= LONG_WAIT
                {
                    return Some(if direct.slowing {
                        format!("{host} is slowing this address")
                    } else {
                        format!(
                            "{host} asked this address to wait {} minutes",
                            until.saturating_duration_since(now).as_secs() / 60
                        )
                    });
                }
            }
        }
        None
    }

    /// Redials on its own whenever [`Engine::wants_new_address`] says so, once
    /// the downloads running well have finished ([`Engine::drain`]). The router
    /// is looked for only once redials are on (or the settings ask), so nothing
    /// searches the network for those who never use it.
    pub(super) async fn watch_reconnects(self) {
        loop {
            if self.cfg().reconnect {
                self.router().await;
            }
            tokio::select! {
                _ = self.0.shutdown.cancelled() => return,
                _ = tokio::time::sleep(self.cfg().throttling.redial_check) => {}
            }
            if let Some(why) = self.wants_new_address() {
                self.drain(&why).await;
            }
        }
    }

    /// The downloads on the line running well: what a redial would cut off.
    /// One a host is slowing isn't among them, since it could take hours and
    /// loses nothing by stopping.
    fn running_well(&self) -> usize {
        self.lock()
            .items
            .values()
            .filter(|i| i.running() && i.exit == Some(DIRECT) && i.slowed.is_none())
            .count()
    }

    /// Whether what runs well on the line should be done within the limit, as
    /// far as can be told: holding the queue for one that won't would help
    /// nobody, and the new address can wait for it.
    fn could_drain(&self, limit: Duration) -> bool {
        let cfg = self.cfg();
        let now = Instant::now();
        let st = self.lock();
        st.items
            .values()
            .filter(|i| i.running() && i.exit == Some(DIRECT) && i.slowed.is_none())
            .all(|i| {
                // Before a row has a speed of its own, what it has shown so far.
                let speed = i
                    .pace
                    .as_ref()
                    .and_then(|p| p.recent(now, &cfg.throttling))
                    .unwrap_or(i.speed);
                speed <= 0.0
                    || i.size == 0
                    || i.size.saturating_sub(i.done) as f64 / speed <= limit.as_secs_f64()
            })
    }

    /// Gets a new address without cutting off anything that runs well: nothing
    /// new starts while the downloads running well on the line go on to their
    /// end, then [`Engine::reconnect`] stops what is left (only those a host is
    /// slowing) and redials, and everything goes again. Past the limit the
    /// redial is put off and the queue goes on; it comes up again after the
    /// usual gap. Downloads through a proxy aren't waited for: a redial doesn't
    /// touch them.
    async fn drain(&self, why: &str) {
        let throttling = self.cfg().throttling.clone();
        if !self.could_drain(throttling.drain_limit) {
            tracing::debug!("a new address would help ({why}): after what's running");
            return;
        }
        tracing::info!("a new address would help ({why}): starting nothing new meanwhile");
        self.set_draining(Some(why));
        let deadline = Instant::now() + throttling.drain_limit;
        let finished = loop {
            // Gone by itself (the slow one finished, the host recovered).
            if self.wants_new_address().is_none() {
                self.set_draining(None);
                return;
            }
            if self.running_well() == 0 {
                break true;
            }
            if Instant::now() >= deadline {
                break false;
            }
            tokio::select! {
                _ = self.0.shutdown.cancelled() => return,
                _ = tokio::time::sleep(throttling.tick) => {}
            }
        };
        if !finished {
            tracing::info!(
                "no new address: downloads still running after {} minutes; later",
                throttling.drain_limit.as_secs() / 60
            );
            {
                let mut r = self.reconnects();
                r.last = Some(Instant::now());
                r.note(
                    format!("Put off a new address ({why}): downloads still running"),
                    false,
                );
            }
            self.set_draining(None);
            return;
        }
        // The hosts this redial is for: with them still slow on the new
        // address, another would be no use ([`Engine::throttled`]).
        let for_hosts: Vec<String> = {
            let st = self.lock();
            st.lanes
                .iter()
                .filter(|((_, exit), l)| *exit == DIRECT && l.slowing && l.slowing_sure)
                .map(|((host, _), _)| host.clone())
                .collect()
        };
        let got = self.reconnect(why).await;
        {
            let mut st = self.lock();
            if got.is_ok() {
                for host in &for_hosts {
                    st.host(host).redialed_for = true;
                }
            }
        }
        self.set_draining(None);
    }

    fn set_draining(&self, why: Option<&str>) {
        let mut st = self.lock();
        st.draining = why.map(str::to_string);
        self.publish(st);
    }
}

/// Whether `address` is one the internet routes to: not a private, shared
/// (a carrier's, `100.64.0.0/10`), link-local or loopback one.
fn is_public(address: &str) -> bool {
    match address.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(a)) => {
            let [first, second, ..] = a.octets();
            let shared = first == 100 && (64..128).contains(&second);
            !(a.is_private() || a.is_loopback() || a.is_link_local() || shared)
        }
        Ok(std::net::IpAddr::V6(a)) => !(a.is_loopback() || (a.segments()[0] & 0xfe00) == 0xfc00),
        Err(_) => false,
    }
}

/// Runs the user's reconnect command in the system's shell.
async fn run(command: &str) -> Result<(), String> {
    let mut cmd = if cfg!(windows) {
        let mut c = tokio::process::Command::new("cmd");
        c.args(["/C", command]);
        c
    } else {
        let mut c = tokio::process::Command::new("sh");
        c.args(["-c", command]);
        c
    };
    let out = tokio::time::timeout(Duration::from_secs(120), cmd.output())
        .await
        .map_err(|_| "the reconnect command ran past two minutes".to_string())?
        .map_err(|e| format!("the reconnect command: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        let said = String::from_utf8_lossy(&out.stderr);
        Err(format!(
            "the reconnect command failed ({}): {}",
            out.status,
            said.lines().next().unwrap_or_default()
        ))
    }
}

/// Why no redial can work with `cfg` and what is known of the router: a
/// reconnect command always may; otherwise a router must have answered,
/// with its login when it needs one.
fn blocker(cfg: &crate::Config, r: &Reconnects) -> Option<String> {
    if secret(&cfg.reconnect_command).is_some() {
        return None;
    }
    match &r.router {
        None if r.looked => {
            Some("no router answered on this network: set a reconnect command".into())
        }
        None => None,
        Some(router) if router.needs_login() && cfg.account("router_password").is_none() => {
            Some(format!(
                "the {} redials only with its login: set it under Router login",
                router.model
            ))
        }
        Some(_) => None,
    }
}

#[cfg(test)]
mod address_tests {
    use super::is_public;

    #[test]
    fn only_a_routed_address_is_public() {
        assert!(is_public("203.0.113.9"));
        for local in [
            "10.0.0.2",
            "192.168.1.1",
            "172.16.0.1",
            "100.64.0.1",
            "127.0.0.1",
            "fd00::1",
        ] {
            assert!(!is_public(local), "{local}");
        }
        assert!(!is_public("unknown (no router to ask)"));
    }
}
