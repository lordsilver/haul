//! VPN services whose servers join the proxies (see [`crate::vpn`]): their
//! list is fetched when the service is set up or its settings change, and
//! once an hour after; the servers picked are checked, one that does not
//! answer gives way to the next, and they become exits after the proxies
//! of the settings. When the pick changes, what was learned about the proxy
//! exits (limits, waits, allowances) goes, since an exit may now be another
//! address.
//!
//! Credentials NordVPN turns away keep the servers anyway, with the error
//! shown: downloads meant for the VPN then wait, rather than leave through
//! the user's own line.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use serde::Serialize;

use super::{Clients, Engine};
use crate::{
    config::secret,
    vpn::{self, Server},
};

/// How old a service's server list may get before it is fetched again.
const REFRESH: Duration = Duration::from_secs(3600);

#[derive(Debug, Default)]
pub(super) struct Services {
    /// NordVPN's SOCKS5 servers, least loaded first, as last fetched.
    nordvpn: Vec<Server>,
    fetched: Option<Instant>,
    /// The settings the last fetch and pick were for.
    asked: Option<String>,
    /// The servers in use, and their proxy addresses.
    picked: Vec<Server>,
    proxies: Vec<String>,
    error: Option<String>,
}

/// What the Connections view shows about VPN services.
#[derive(Debug, Clone, Serialize, PartialEq, ts_rs::TS)]
#[ts(export)]
pub struct ServicesView {
    /// NordVPN's servers in use.
    pub nordvpn: Vec<Server>,
    /// The countries NordVPN has SOCKS5 servers in, with how many.
    pub nordvpn_countries: Vec<(String, usize)>,
    pub error: Option<String>,
}

impl Engine {
    fn services(&self) -> std::sync::MutexGuard<'_, Services> {
        self.0.services.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The proxy addresses VPN services add to the pool.
    pub(super) fn service_proxies(&self) -> Vec<String> {
        self.services().proxies.clone()
    }

    pub fn services_view(&self) -> ServicesView {
        let s = self.services();
        let mut countries: Vec<(String, usize)> = Vec::new();
        for server in &s.nordvpn {
            match countries.iter_mut().find(|(c, _)| *c == server.country) {
                Some((_, n)) => *n += 1,
                None => countries.push((server.country.clone(), 1)),
            }
        }
        countries.sort();
        ServicesView {
            nordvpn: s.picked.clone(),
            nordvpn_countries: countries,
            error: s.error.clone(),
        }
    }

    /// Fetches NordVPN's server list now, for the view to offer its
    /// countries before the service is set up.
    pub async fn nordvpn_list(&self) -> Result<usize, String> {
        let api = self.cfg().endpoints.nordvpn.clone();
        let list = vpn::nordvpn_servers(&self.clients().on(0).client, &api).await?;
        let n = list.len();
        {
            let mut s = self.services();
            s.nordvpn = list;
            s.fetched = Some(Instant::now());
            s.asked = None;
        }
        self.refresh_services().await;
        Ok(n)
    }

    /// Picks the servers again from the list (fetched again when old or
    /// when the settings changed), and makes them exits when the pick
    /// changed.
    async fn refresh_services(&self) {
        let cfg = self.cfg();
        let wanted =
            cfg.nordvpn_on && secret(&cfg.nordvpn_password).is_some() && cfg.nordvpn_servers > 0;
        let asked = format!(
            "{:?}|{:?}|{}|{:?}|{}|{}",
            secret(&cfg.nordvpn_user),
            secret(&cfg.nordvpn_password),
            cfg.nordvpn_servers,
            cfg.nordvpn_countries,
            cfg.use_proxy,
            cfg.nordvpn_on
        );
        let stale = {
            let s = self.services();
            s.fetched.is_none_or(|t| t.elapsed() >= REFRESH) || s.asked.as_deref() != Some(&asked)
        };
        let mut checked = None;
        if wanted && stale {
            let api = cfg.endpoints.nordvpn.clone();
            match vpn::nordvpn_servers(&self.clients().on(0).client, &api).await {
                Ok(list) => {
                    let mut s = self.services();
                    s.nordvpn = list;
                    s.fetched = Some(Instant::now());
                    s.error = None;
                }
                Err(e) => {
                    tracing::warn!("{e}");
                    self.services().error = Some(e);
                }
            }
            let candidates = vpn::nordvpn_candidates(&cfg, &self.services().nordvpn);
            checked = Some(self.check_servers(candidates).await);
        }
        let (picked, proxies) = {
            let mut s = self.services();
            let picked = match checked {
                Some((picked, error)) => {
                    if error.is_some() {
                        s.error = error;
                    }
                    picked
                }
                // Between checks the servers in use stay, as checked.
                None if wanted => s.picked.clone(),
                None => Vec::new(),
            };
            let proxies = vpn::nordvpn_proxies(&cfg, &picked);
            (picked, proxies)
        };
        let changed = {
            let mut s = self.services();
            s.asked = Some(asked);
            let changed = s.proxies != proxies;
            s.picked = picked;
            s.proxies = proxies.clone();
            changed
        };
        if !changed {
            self.touch(&mut self.lock());
            return;
        }
        match Clients::new(&cfg, &proxies) {
            Ok(clients) => {
                *self.0.clients.write().unwrap_or_else(|e| e.into_inner()) = Arc::new(clients);
                tracing::info!("{} NordVPN servers among the proxies", proxies.len());
                let mut st = self.lock();
                // Exits may now be other addresses.
                st.lanes.retain(|(_, exit), _| *exit == 0);
                self.touch(&mut st);
            }
            Err(e) => {
                self.services().error = Some(e);
                self.touch(&mut self.lock());
            }
        }
        self.0.wake.notify_one();
    }

    /// The first `nordvpn_servers` of `candidates` that work, each checked
    /// through itself, a few at a time; and why there are fewer, when there
    /// are. Credentials turned away stop the checks: the servers asked for
    /// are kept as they are, with the error.
    async fn check_servers(&self, candidates: Vec<Server>) -> (Vec<Server>, Option<String>) {
        let cfg = self.cfg();
        let want = cfg.nordvpn_servers.min(candidates.len());
        let mut picked: Vec<Server> = Vec::new();
        let mut down = 0;
        let mut rest = candidates.iter();
        while picked.len() < want {
            let batch: Vec<Server> = rest.by_ref().take(want - picked.len()).cloned().collect();
            if batch.is_empty() {
                break;
            }
            let mut checks = tokio::task::JoinSet::new();
            for (n, server) in batch.iter().enumerate() {
                let proxy = vpn::nordvpn_proxies(&cfg, std::slice::from_ref(server)).remove(0);
                let (api, agent) = (cfg.endpoints.nordvpn.clone(), cfg.user_agent.clone());
                checks.spawn(async move { (n, vpn::nordvpn_check(&api, &proxy, &agent).await) });
            }
            let mut results = checks.join_all().await;
            results.sort_by_key(|(n, _)| *n);
            for (n, result) in results {
                let server = &batch[n];
                match result {
                    vpn::Checked::Works(address) => {
                        tracing::info!("NordVPN {}: {address}", server.host);
                        picked.push(Server {
                            address: Some(address),
                            ..server.clone()
                        });
                    }
                    vpn::Checked::Refused => {
                        tracing::warn!("NordVPN turned the service credentials away");
                        let mut kept = candidates;
                        kept.truncate(want);
                        return (
                            kept,
                            Some(
                                "NordVPN turned the service credentials away: use the ones under manual setup in your Nord Account, not your login"
                                    .into(),
                            ),
                        );
                    }
                    vpn::Checked::Down(why) => {
                        tracing::warn!("NordVPN {} left out: {why}", server.host);
                        down += 1;
                    }
                }
            }
        }
        let short = (picked.len() < want).then(|| {
            format!(
                "{} of {want} servers answered ({down} did not)",
                picked.len()
            )
        });
        (picked, short)
    }

    /// Keeps the services' servers up to date: when their settings change,
    /// and once an hour.
    pub(super) async fn watch_services(self) {
        let mut changes = self.watch();
        let mut seen = None;
        let mut last = Instant::now();
        loop {
            let cfg = self.cfg();
            let now_asked = (
                cfg.nordvpn_on,
                secret(&cfg.nordvpn_password).is_some(),
                cfg.nordvpn_servers,
                cfg.nordvpn_countries.clone(),
                cfg.use_proxy,
                secret(&cfg.nordvpn_user).map(str::to_string),
            );
            if seen.as_ref() != Some(&now_asked) || last.elapsed() >= REFRESH {
                seen = Some(now_asked);
                last = Instant::now();
                self.refresh_services().await;
            }
            tokio::select! {
                _ = self.0.shutdown.cancelled() => return,
                _ = tokio::time::sleep(REFRESH) => {}
                _ = changes.changed() => {}
            }
        }
    }
}
