//! VPN services as more addresses for the proxy pool. A service's SOCKS5
//! servers are proxies like any other, so each is one more exit with a
//! host's limits of its own; what an integration adds is finding them, so
//! nobody types `socks5://…` addresses that change under them.
//!
//! NordVPN publishes its SOCKS5 servers (and their load) without a login;
//! the subscription's service credentials (not the account's login) open
//! them. Haul takes the least loaded ones in the countries chosen, and
//! checks each through itself with NordVPN's own "where am I" API: the
//! credentials are taken, the address it shows, and that NordVPN calls the
//! connection protected. One that does not answer gives way to the next.

use std::time::Duration;

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use serde::{Deserialize, Serialize};

use crate::config::{Config, secret};

/// One server of a service, as the views show it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, ts_rs::TS)]
#[ts(export)]
pub struct Server {
    pub host: String,
    pub country: String,
    pub city: String,
    /// Percent busy, as the service says.
    pub load: u32,
    /// The public address downloads through it come from, once checked.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
}

/// Characters a proxy address's user and password keep as they are.
const USERINFO: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'~');

impl Server {
    /// The proxy address for this server with the service's credentials:
    /// port 1080, unless the host name carries a port of its own.
    pub fn proxy(&self, user: &str, password: &str) -> String {
        let port = if self.host.contains(':') { "" } else { ":1080" };
        format!(
            "socks5://{}:{}@{}{port}",
            utf8_percent_encode(user, USERINFO),
            utf8_percent_encode(password, USERINFO),
            self.host
        )
    }
}

#[derive(Deserialize)]
struct NordServer {
    hostname: String,
    load: u32,
    locations: Vec<NordLocation>,
}

#[derive(Deserialize)]
struct NordLocation {
    country: NordCountry,
}

#[derive(Deserialize)]
struct NordCountry {
    name: String,
    city: NordCity,
}

#[derive(Deserialize)]
struct NordCity {
    name: String,
}

/// Every NordVPN server that takes SOCKS5, least loaded first.
pub async fn nordvpn_servers(http: &reqwest::Client, api: &str) -> Result<Vec<Server>, String> {
    let url =
        format!("{api}/v1/servers?limit=1000&filters[servers_technologies][identifier]=socks");
    let list: Vec<NordServer> = http
        .get(url)
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| format!("NordVPN's server list: {}", e.without_url()))?
        .json()
        .await
        .map_err(|e| format!("NordVPN's server list: {}", e.without_url()))?;
    let mut servers: Vec<Server> = list
        .into_iter()
        .filter_map(|s| {
            let place = s.locations.into_iter().next()?;
            Some(Server {
                host: s.hostname,
                country: place.country.name,
                city: place.country.city.name,
                load: s.load,
                address: None,
            })
        })
        .collect();
    servers.sort_by_key(|s| (s.load, s.host.clone()));
    Ok(servers)
}

/// The NordVPN servers to use: the least loaded `nordvpn_servers` in the
/// countries chosen (every country when none is), once the service
/// credentials are set.
pub fn nordvpn_pick(cfg: &Config, servers: &[Server]) -> Vec<Server> {
    let mut picked = nordvpn_candidates(cfg, servers);
    picked.truncate(cfg.nordvpn_servers);
    picked
}

/// Every server in the countries chosen, least loaded first; none until
/// the service credentials and a number of servers are set.
pub fn nordvpn_candidates(cfg: &Config, servers: &[Server]) -> Vec<Server> {
    if !cfg.nordvpn_on || secret(&cfg.nordvpn_password).is_none() || cfg.nordvpn_servers == 0 {
        return Vec::new();
    }
    servers
        .iter()
        .filter(|s| {
            cfg.nordvpn_countries.is_empty()
                || cfg
                    .nordvpn_countries
                    .iter()
                    .any(|c| c.eq_ignore_ascii_case(&s.country))
        })
        .cloned()
        .collect()
}

/// What checking a server through itself found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Checked {
    /// It works; downloads through it come from this address.
    Works(String),
    /// It turned the service credentials away.
    Refused,
    /// It did not answer, or not as a protected connection.
    Down(String),
}

#[derive(Deserialize)]
struct Insights {
    ip: String,
    #[serde(default)]
    protected: bool,
}

/// Checks `proxy` (a server's address with the credentials) by asking
/// NordVPN's API at `api`, through it, where the request comes from.
pub async fn nordvpn_check(api: &str, proxy: &str, user_agent: &str) -> Checked {
    let http = match crate::hosts::Http::with(Some(proxy), user_agent, true) {
        Ok(http) => http,
        Err(e) => return Checked::Down(e),
    };
    let answer = http
        .client
        .get(format!("{api}/v1/helpers/ips/insights"))
        .timeout(Duration::from_secs(20))
        .send()
        .await;
    let response = match answer {
        Ok(r) => r,
        Err(e) => {
            let said = crate::hosts::error_text(e);
            return if refused(&said) {
                Checked::Refused
            } else {
                Checked::Down(said)
            };
        }
    };
    match response.json::<Insights>().await {
        Ok(i) if i.protected => Checked::Works(i.ip),
        Ok(i) => Checked::Down(format!("NordVPN does not call {} protected", i.ip)),
        Err(e) => Checked::Down(e.without_url().to_string()),
    }
}

/// Whether a connection error is a SOCKS5 server turning the credentials
/// away.
fn refused(said: &str) -> bool {
    said.contains("credentials not accepted") || said.contains("authentication")
}

/// The proxy addresses of the servers picked.
pub fn nordvpn_proxies(cfg: &Config, picked: &[Server]) -> Vec<String> {
    let user = secret(&cfg.nordvpn_user).unwrap_or_default();
    let password = secret(&cfg.nordvpn_password).unwrap_or_default();
    picked.iter().map(|s| s.proxy(user, password)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server(host: &str, country: &str, load: u32) -> Server {
        Server {
            host: host.into(),
            country: country.into(),
            load,
            ..Server::default()
        }
    }

    #[test]
    fn a_refusal_is_told_from_a_server_down() {
        assert!(refused(
            "error sending request: SOCKS error: credentials not accepted"
        ));
        assert!(!refused("error sending request: connection refused"));
    }

    #[test]
    fn the_least_loaded_servers_in_the_countries_chosen() {
        let servers = vec![
            server("socks-us34.nordvpn.com", "United States", 4),
            server("socks-se17.nordvpn.com", "Sweden", 9),
            server("socks-se12.nordvpn.com", "Sweden", 10),
            server("socks-nl4.nordvpn.com", "Netherlands", 34),
        ];
        let mut cfg = Config {
            nordvpn_servers: 2,
            ..Config::default()
        };
        assert!(
            nordvpn_pick(&cfg, &servers).is_empty(),
            "no credentials, none"
        );
        cfg.nordvpn_user = Some("svc user".into());
        cfg.nordvpn_password = Some("p@ss:w/rd".into());
        let all = nordvpn_pick(&cfg, &servers);
        assert_eq!(
            all.iter().map(|s| s.host.as_str()).collect::<Vec<_>>(),
            ["socks-us34.nordvpn.com", "socks-se17.nordvpn.com"]
        );
        cfg.nordvpn_countries = vec!["sweden".into(), "Netherlands".into()];
        let picked = nordvpn_pick(&cfg, &servers);
        assert_eq!(
            picked.iter().map(|s| s.host.as_str()).collect::<Vec<_>>(),
            ["socks-se17.nordvpn.com", "socks-se12.nordvpn.com"]
        );
        assert_eq!(
            nordvpn_proxies(&cfg, &picked[..1]),
            ["socks5://svc%20user:p%40ss%3Aw%2Frd@socks-se17.nordvpn.com:1080"]
        );
        cfg.nordvpn_servers = 0;
        assert!(nordvpn_pick(&cfg, &servers).is_empty(), "switched off");
    }
}
