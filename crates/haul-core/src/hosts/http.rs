//! The HTTP clients a resolver uses, per route, with the clearances a browser handed over.

use super::*;

/// HTTP clients for one route (direct or through the proxy). Both share a
/// cookie jar, as one browser session would.
#[derive(Clone)]
pub struct Http {
    pub client: reqwest::Client,
    pub no_redirect: reqwest::Client,
    pub via_proxy: bool,
    /// The User-Agent both clients send, for a page that also reports it.
    pub user_agent: String,
    /// The cookie jar both clients share.
    pub jar: Arc<reqwest::cookie::Jar>,
    pub(super) proxy: Option<String>,
    pub(super) fresh: bool,
    /// Clients that send a browser's user agent to the site (by
    /// [`site_of`]) whose check that browser passed: Cloudflare's clearance
    /// holds only with the user agent it was given to.
    pub(super) as_browser: Arc<Mutex<HashMap<String, AsBrowser>>>,
}

/// [`Http`]'s clients with a browser's user agent, on the same cookie jar.
#[derive(Clone)]
pub(super) struct AsBrowser {
    client: reqwest::Client,
    no_redirect: reqwest::Client,
    user_agent: String,
}

impl Http {
    pub fn new(proxy: Option<&str>, user_agent: &str) -> Result<Self, String> {
        Self::with(proxy, user_agent, false)
    }

    /// With `fresh`, no connection is kept for the next request, so a proxy
    /// that gives each connection another address gives each download one.
    pub fn with(proxy: Option<&str>, user_agent: &str, fresh: bool) -> Result<Self, String> {
        let jar = Arc::new(reqwest::cookie::Jar::default());
        let (client, no_redirect) = clients(proxy, user_agent, fresh, &jar)?;
        Ok(Self {
            client,
            no_redirect,
            via_proxy: proxy.is_some(),
            user_agent: user_agent.to_string(),
            jar,
            proxy: proxy.map(str::to_string),
            fresh,
            as_browser: Arc::default(),
        })
    }

    /// Takes the clearance a browser with `user_agent` got past the check
    /// in front of `page`'s site (its cookies, `Set-Cookie` lines): requests
    /// to that site carry them and send that user agent from now on
    /// ([`Http::for_page`]), as Cloudflare lets the clearance through only
    /// with the user agent it was given to.
    pub fn add_clearance(&self, page: &str, cookies: &[String], user_agent: Option<&str>) {
        let Ok(url) = url::Url::parse(page) else {
            return;
        };
        // The clients first: cookies sent without the user agent they hold
        // with would only be turned away.
        let as_browser = match user_agent.filter(|a| !a.is_empty() && *a != self.user_agent) {
            Some(agent) => match clients(self.proxy.as_deref(), agent, self.fresh, &self.jar) {
                Ok((client, no_redirect)) => Some(AsBrowser {
                    client,
                    no_redirect,
                    user_agent: agent.to_string(),
                }),
                Err(e) => {
                    tracing::warn!("clients with the browser's user agent: {e}");
                    return;
                }
            },
            None => None,
        };
        if let Some(b) = as_browser {
            self.as_browser
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(site_of(page), b);
        }
        for cookie in cookies {
            self.jar.add_cookie_str(cookie, &url);
        }
    }

    /// These clients for requests to `url`'s site: with the user agent of
    /// the browser whose clearance they carry there ([`Http::add_clearance`]),
    /// else as they are.
    pub fn for_page(&self, url: &str) -> Http {
        let as_browser = self.as_browser.lock().unwrap_or_else(|e| e.into_inner());
        let Some(b) = (!as_browser.is_empty())
            .then(|| as_browser.get(&site_of(url)))
            .flatten()
        else {
            return self.clone();
        };
        Http {
            client: b.client.clone(),
            no_redirect: b.no_redirect.clone(),
            user_agent: b.user_agent.clone(),
            ..self.clone()
        }
    }
}

/// A route's two clients, following redirects and not, on cookie jar `jar`.
pub(super) fn clients(
    proxy: Option<&str>,
    user_agent: &str,
    fresh: bool,
    jar: &Arc<reqwest::cookie::Jar>,
) -> Result<(reqwest::Client, reqwest::Client), String> {
    let build = |follow: bool| {
        let mut b = reqwest::Client::builder()
            .no_proxy()
            .user_agent(user_agent)
            .cookie_provider(jar.clone())
            .connect_timeout(Duration::from_secs(if proxy.is_some() { 30 } else { 20 }))
            .read_timeout(Duration::from_secs(60));
        if !follow {
            b = b.redirect(reqwest::redirect::Policy::none());
        }
        if fresh {
            b = b.pool_max_idle_per_host(0);
        }
        if let Some(p) = proxy {
            b = b.proxy(reqwest::Proxy::all(remote_dns(p)).map_err(|e| format!("proxy: {e}"))?);
        }
        b.build().map_err(|e| e.to_string())
    };
    Ok((build(true)?, build(false)?))
}

/// Resolve host names on the proxy, so DNS does not leak around it.
pub(super) fn remote_dns(proxy: &str) -> String {
    match proxy.strip_prefix("socks5://") {
        Some(rest) => format!("socks5h://{rest}"),
        None => proxy.to_string(),
    }
}

/// Session tokens resolvers reuse between calls.
#[derive(Debug, Default)]
pub struct Tokens {
    pub(crate) rapidgator: Mutex<Option<String>>,
    /// When vikingfile's `check-file` may be asked next: one lookup at a
    /// time, spaced out, since it turns a burst away with 429.
    pub(crate) vikingfile_next: tokio::sync::Mutex<Option<std::time::Instant>>,
}

/// Everything a resolver may use.
pub struct Cx<'a> {
    pub http: &'a Http,
    pub cfg: &'a Config,
    pub tokens: &'a Tokens,
    /// The route to JDownloader's key service, which a `.dlc` is decrypted
    /// through: the service's own, not that of the host the file came from.
    pub key_service: &'a Http,
}
