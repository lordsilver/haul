//! Stand-in mode: a test seam that lets a local stand-in be any site.
//!
//! With `HAUL_STAND_IN=<port>` and debug mode asked for at the start
//! (`HAUL_DEBUG=1` or the server's `--debug`, never the Live connection
//! setting), `<site>.localhost:<port>` stands for `<site>`:
//!
//! - [`host_of`] and [`site_of`] drop the `.localhost`, so recognition
//!   (known hosts, wrappers, pastes, a site's browser profile, the page
//!   recipes) sees the real name;
//! - [`reach`] sends every address with a real name to its stand-in, at the
//!   two places requests leave Haul: the HTTP client ([`Client`]) and the
//!   desktop app's page windows. So a stand-in may list links in their real
//!   form, and they still come to it.
//!
//! The stand-in answers by the `Host` header it is sent
//! (`filecrypt.cc.localhost:8799`). Off, which is how Haul always runs
//! unless both are set at its start, nothing here changes an address.

use std::{
    borrow::Cow,
    net::{Ipv4Addr, SocketAddr},
    sync::atomic::{AtomicU16, Ordering},
};

/// The stand-in's port, 0 while off.
static PORT: AtomicU16 = AtomicU16::new(0);

/// What a stand-in's name ends in: a name every browser and Haul's client
/// take to this computer.
const SUFFIX: &str = ".localhost";

/// The stand-in's port while stand-in mode is on.
pub fn stand_in() -> Option<u16> {
    Some(PORT.load(Ordering::Relaxed)).filter(|p| *p != 0)
}

/// Puts stand-in mode on, for stand-ins on `port`, or off. Haul calls it
/// once, as debug mode starts ([`crate::debug::enable`]); a test may call
/// it around a case of its own.
pub fn set_stand_in(port: Option<u16>) {
    PORT.store(port.unwrap_or(0), Ordering::Relaxed);
    if let Some(port) = port {
        tracing::warn!("stand-in mode: every site is reached at <site>.localhost:{port}");
    }
}

/// The port `HAUL_STAND_IN` names, if it names one.
pub(crate) fn asked() -> Option<u16> {
    std::env::var("HAUL_STAND_IN")
        .ok()?
        .trim()
        .parse()
        .ok()
        .filter(|p| *p != 0)
}

/// `host` (lowercase) without the stand-in's `.localhost`, in stand-in mode.
pub(super) fn real_name(host: &str) -> &str {
    match stand_in() {
        Some(_) => without_suffix(host),
        None => host,
    }
}

fn without_suffix(host: &str) -> &str {
    host.strip_suffix(SUFFIX)
        .filter(|h| !h.is_empty())
        .unwrap_or(host)
}

/// Where a request for `url` goes: in stand-in mode, an address with a real
/// name goes to its stand-in (`https://filecrypt.cc/Container/X.html` to
/// `http://filecrypt.cc.localhost:<port>/Container/X.html`); an address on
/// this computer (an IP address, `localhost`, a stand-in's own) and every
/// address while stand-in mode is off go where they say.
pub fn reach(url: &str) -> Cow<'_, str> {
    match stand_in() {
        Some(port) => reach_on(url, port),
        None => Cow::Borrowed(url),
    }
}

fn reach_on(url: &str, port: u16) -> Cow<'_, str> {
    let Ok(mut u) = url::Url::parse(url) else {
        return Cow::Borrowed(url);
    };
    let Some(url::Host::Domain(name)) = u.host() else {
        return Cow::Borrowed(url);
    };
    let name = name.trim_end_matches('.').to_ascii_lowercase();
    if !matches!(u.scheme(), "http" | "https") || name == "localhost" || name.ends_with(SUFFIX) {
        return Cow::Borrowed(url);
    }
    let moved = u.set_scheme("http").is_ok()
        && u.set_host(Some(&format!("{name}{SUFFIX}"))).is_ok()
        && u.set_port(Some(port)).is_ok();
    if moved {
        Cow::Owned(u.into())
    } else {
        Cow::Borrowed(url)
    }
}

/// Haul's resolver in stand-in mode: a stand-in's name is this computer,
/// whatever the system's own resolver makes of `*.localhost` (Windows'
/// and many Linux setups' give nothing); every other name is looked up as
/// usual.
pub(super) struct Resolver;

impl reqwest::dns::Resolve for Resolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let name = name.as_str().trim_end_matches('.').to_ascii_lowercase();
        Box::pin(async move {
            if name == "localhost" || name.ends_with(SUFFIX) {
                let here = SocketAddr::from((Ipv4Addr::LOCALHOST, 0));
                return Ok(Box::new(std::iter::once(here)) as reqwest::dns::Addrs);
            }
            let found = tokio::net::lookup_host((name.as_str(), 0)).await?;
            Ok(Box::new(found.collect::<Vec<_>>().into_iter()) as reqwest::dns::Addrs)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_real_address_goes_to_its_stand_in() {
        assert_eq!(
            reach_on("https://filecrypt.cc/Container/AAAAAAAA.html", 8799),
            "http://filecrypt.cc.localhost:8799/Container/AAAAAAAA.html"
        );
        assert_eq!(
            reach_on(
                "https://www.Pixeldrain.com:8443/api/file/abc123?download#x",
                8799
            ),
            "http://www.pixeldrain.com.localhost:8799/api/file/abc123?download#x",
            "its port, its www, its query"
        );
    }

    #[test]
    fn an_address_on_this_computer_stays() {
        for url in [
            "http://127.0.0.1:8799/guard/swap",
            "http://[::1]:8799/x",
            "http://localhost:8799/files/a.rar",
            "http://filecrypt.cc.localhost:8799/Container/AAAAAAAA.html",
            "ftp://site.example/a.rar",
            "not a link",
        ] {
            assert_eq!(reach_on(url, 8799), url, "{url}");
        }
    }

    #[test]
    fn a_stand_in_s_name_is_the_real_one() {
        assert_eq!(without_suffix("filecrypt.cc.localhost"), "filecrypt.cc");
        assert_eq!(without_suffix("localhost"), "localhost");
        assert_eq!(without_suffix("site.example"), "site.example");
    }

    #[tokio::test]
    async fn a_stand_in_s_name_resolves_to_this_computer() {
        use reqwest::dns::Resolve;
        for name in ["filecrypt.cc.localhost", "localhost"] {
            let addrs: Vec<_> = Resolver
                .resolve(name.parse().unwrap())
                .await
                .unwrap()
                .collect();
            assert_eq!(
                addrs,
                [SocketAddr::from((Ipv4Addr::LOCALHOST, 0))],
                "{name}"
            );
        }
    }
}
