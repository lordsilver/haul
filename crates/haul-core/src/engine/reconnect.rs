//! A new public address for this machine's own line, from the router or a
//! command of the user's: when a host has slowed the address for the day
//! or asked it for a long wait, and no proxy can take over.
//!
//! While the router redials, nothing new starts and the downloads on this
//! line stop; they resume from where they were once the line is back, and
//! what was learned about the old address (limits, waits, allowance) goes.
//! Automatic redials need the setting, come at most every ten minutes, and
//! stop after a redial brings the same address back: that line keeps it.

use std::time::{Duration, Instant};

use serde::Serialize;

use super::{Engine, Exit};
use crate::{
    config::secret,
    hosts,
    item::Status,
    router::{self, Router},
};

/// The least time between automatic redials.
const GAP: Duration = Duration::from_secs(600);
/// A host's wait for this address worth a redial.
const LONG_WAIT: Duration = Duration::from_secs(300);
/// This machine's own connection.
const DIRECT: Exit = 0;

#[derive(Debug, Default)]
pub(super) struct Reconnects {
    busy: bool,
    last: Option<Instant>,
    /// What the last redial did, for the settings view.
    note: Option<String>,
    /// A redial brought the same address back: automatic ones stop.
    keeps_address: bool,
    router: Option<Router>,
    looked: bool,
}

/// What the settings view shows about getting a new address.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ReconnectView {
    /// The router found, as it names itself.
    pub router: Option<String>,
    pub busy: bool,
    pub note: Option<String>,
    /// Redials on its own when the address holds downloads back.
    pub auto: bool,
}

impl Engine {
    fn reconnects(&self) -> std::sync::MutexGuard<'_, Reconnects> {
        self.0.reconnects.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn reconnect_view(&self) -> ReconnectView {
        let r = self.reconnects();
        ReconnectView {
            router: r.router.as_ref().map(|r| r.model.clone()),
            busy: r.busy,
            note: r.note.clone(),
            auto: self.cfg().reconnect && !r.keeps_address,
        }
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
                r.note = Some(format!("Got a new address ({why})"));
                drop(r);
                let mut st = self.lock();
                for ((_, exit), lane) in st.lanes.iter_mut() {
                    if *exit == DIRECT {
                        lane.fresh_start();
                        lane.quota = None;
                    }
                }
                self.touch(&mut st);
            }
            Err(e) => {
                tracing::warn!("no new address: {e}");
                r.keeps_address |= e.contains("same address");
                r.note = Some(format!("No new address: {e}"));
                drop(r);
                self.touch(&mut self.lock());
            }
        }
        self.0.wake.notify_one();
        result
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

    async fn redial(&self) -> Result<String, String> {
        // The stopped downloads let go of the line first.
        for _ in 0..50 {
            if !self.lock().items.values().any(|i| i.exit == Some(DIRECT)) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
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
        let router = self
            .router()
            .await
            .ok_or("no router answered on this network; set a reconnect command instead")?;
        let login = match (secret(&cfg.router_user), secret(&cfg.router_password)) {
            (user, Some(password)) => Some((user.unwrap_or_default(), password)),
            _ => None,
        };
        router.redial(&http, login).await
    }

    /// Why a new address would help now, when automatic redials are on: a
    /// host slowed this machine's address for the day, or asked it for a
    /// long wait, for a download that still needs that host, and no proxy
    /// can take over.
    pub(super) fn wants_new_address(&self) -> Option<String> {
        let cfg = self.cfg();
        if !cfg.reconnect {
            return None;
        }
        {
            let r = self.reconnects();
            if r.busy || r.keeps_address || r.last.is_some_and(|t| t.elapsed() < GAP) {
                return None;
            }
        }
        let st = self.lock();
        let now = Instant::now();
        let mut hosts: Vec<String> = st
            .items
            .values()
            .filter(|i| {
                matches!(
                    i.status,
                    Status::Queued | Status::Waiting | Status::Downloading
                )
            })
            .flat_map(|i| i.mirrors.iter().map(|m| hosts::host_of(m)))
            .filter(|h| !cfg.is_off(h, self.pages_pass()))
            .collect();
        hosts.sort();
        hosts.dedup();
        for host in hosts {
            let exits = self.exits_for(&host, &st);
            if !exits.contains(&DIRECT) {
                continue;
            }
            let lane = |e: Exit| st.lanes.get(&(host.clone(), e));
            let proxy_free = exits
                .iter()
                .any(|&e| e != DIRECT && lane(e).is_none_or(|l| !l.slowed() && !l.cooling(now)));
            if proxy_free {
                continue;
            }
            let Some(direct) = lane(DIRECT) else { continue };
            if let Some(q) = direct.quota.filter(|q| q.slowed()) {
                return Some(format!(
                    "{host} slowed this address to {} KB/s for the day",
                    q.slowed_to / 1024
                ));
            }
            if let Some(until) = direct.cooldown
                && until.saturating_duration_since(now) >= LONG_WAIT
            {
                return Some(format!(
                    "{host} asked this address to wait {} minutes",
                    until.saturating_duration_since(now).as_secs() / 60
                ));
            }
        }
        None
    }

    /// Redials on its own whenever [`Engine::wants_new_address`] says so.
    /// The router is looked for only once redials are on (or the settings
    /// ask), so nothing searches the network for those who never use it.
    pub(super) async fn watch_reconnects(self) {
        loop {
            if self.cfg().reconnect {
                self.router().await;
            }
            tokio::select! {
                _ = self.0.shutdown.cancelled() => return,
                _ = tokio::time::sleep(Duration::from_secs(30)) => {}
            }
            if let Some(why) = self.wants_new_address() {
                let _ = self.reconnect(&why).await;
            }
        }
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
