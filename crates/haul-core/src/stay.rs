//! Where the in-app browser goes while a person passes a host's step.
//!
//! Host pages carry ads that act on a click: they send the page to an ad's
//! site, or open the page the click was for in a new window, which the
//! browser refuses, and send this one to the ad. A host's page therefore
//! stays on the host's site: a page from another site that replaces it is
//! sent back, to the page the click opened in a new window when there was
//! one, else to where the window was; and a new window on the host's own
//! site loads in place, unless the page goes on by itself. Downloads never
//! reach this: they replace no page.
//!
//! The host's site is the page's, and wherever its first page lands (a host
//! may serve its pages from a domain of its own). A site the page keeps
//! going to by itself, three times within half a minute, is taken as part
//! of the host's flow rather than an ad.
//!
//! A wrapper's page is Haul's to read once a person is past its gate, so it
//! has no flow of its own to follow off its site: once its first page has
//! loaded, the window is refused any other site before it loads, but for a
//! host or wrapper Haul knows (a link the person follows) and a file. Ads on
//! such pages send the window to a new throwaway site each time, which
//! sending back, and remembering the sites, would not keep up with. A page
//! that lists links, on a site Haul has no code for, goes where it will.
//!
//! A site a window was sent back from is an ad's: for as long as Haul runs,
//! a page from it is not even loaded, in any window. Only a page that
//! loaded is remembered so, never a download.

use std::{
    collections::{HashMap, HashSet},
    sync::{LazyLock, Mutex},
    time::{Duration, Instant},
};

use crate::hosts::{self, site_of};

/// How long a new window on the host's site waits for the page to go on by
/// itself before it loads in place.
pub const GRACE: Duration = Duration::from_millis(800);
/// How long a sending back may take before the window is sent again.
const SENDING: Duration = Duration::from_secs(2);
/// A site sent back from this many times within [`WITHIN`] is let be.
const TIMES: u32 = 3;
const WITHIN: Duration = Duration::from_secs(30);

/// The sites windows were sent back from, while Haul runs.
static ADS: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(Mutex::default);

fn ads() -> std::sync::MutexGuard<'static, HashSet<String>> {
    ADS.lock().unwrap_or_else(|e| e.into_inner())
}

/// Where to send a window that left the host's site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendBack {
    pub page: String,
    /// Go back in the window's history first, which keeps the page as it
    /// was (a countdown that has run, say); `page` if that leaves the window
    /// away still. When false, `page` is the page a click opened in a new
    /// window and is loaded straight away.
    pub history: bool,
}

/// How a window keeps to its page's site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Keep {
    /// A host's page: a page from another site that replaces it is sent
    /// back.
    Host,
    /// A wrapper's page: another site is refused before it loads.
    Wrapper,
    /// A page that lists links, on a site Haul has no code for: it goes
    /// where it will.
    Free,
}

/// What the window may do about a navigation of its page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Go {
    /// It goes ahead.
    Let,
    /// Refused: a site a window was sent back from.
    Ad,
    /// Refused: another site, on a wrapper's page.
    OffSite,
}

/// One window's way around a host's site.
#[derive(Debug)]
pub struct Stay {
    keep: Keep,
    sites: HashSet<String>,
    landed: bool,
    last: String,
    /// A new window the page opened on the host's site, not loaded yet.
    opened: Option<(String, Instant)>,
    sending: Option<Instant>,
    sent_from: HashMap<String, (Instant, u32)>,
}

fn is_web(url: &str) -> bool {
    url.starts_with("http://") || url.starts_with("https://")
}

/// Whether a wrapper's page going to `url` leads on rather than to an ad: a
/// host or wrapper Haul knows, or a file (a download, which the window takes
/// instead of loading).
fn leads_on(url: &str) -> bool {
    hosts::known(&hosts::host_of(url)).is_some() || hosts::names_a_file(url)
}

impl Stay {
    /// For a window opened at `page`.
    pub fn new(page: &str, keep: Keep) -> Self {
        Self {
            keep,
            sites: HashSet::from([site_of(page)]),
            landed: false,
            last: page.to_string(),
            opened: None,
            sending: None,
            sent_from: HashMap::new(),
        }
    }

    /// Whether `url` is on the host's site, or no web page at all.
    pub fn is_home(&self, url: &str) -> bool {
        !is_web(url) || self.sites.contains(&site_of(url))
    }

    /// The page starts to navigate to `url`: refused when it is a page of
    /// a site a window was sent back from, or, on a wrapper's page past its
    /// first, of another site. On the host's site it is the page going on by
    /// itself, so a window it opened is not loaded in place.
    pub fn navigating(&mut self, url: &str) -> Go {
        if !is_web(url) {
            return Go::Let;
        }
        if self.is_home(url) {
            self.opened = None;
            return Go::Let;
        }
        match self.keep {
            Keep::Free => Go::Let,
            Keep::Host if ads().contains(&site_of(url)) => Go::Ad,
            Keep::Host => Go::Let,
            Keep::Wrapper if !self.landed || leads_on(url) => Go::Let,
            Keep::Wrapper => Go::OffSite,
        }
    }

    /// A page started loading in the window: where to send the window back
    /// to when it is from another site.
    pub fn landed(&mut self, url: &str, now: Instant) -> Option<SendBack> {
        if !is_web(url) {
            return None;
        }
        if !self.landed {
            self.landed = true;
            self.sites.insert(site_of(url));
        }
        if self.is_home(url) {
            self.last = url.to_string();
            self.opened = None;
            self.sending = None;
            return None;
        }
        if self.keep != Keep::Host {
            return None;
        }
        if self
            .sending
            .is_some_and(|at| now.duration_since(at) < SENDING)
        {
            // An ad's chain of redirects: the sending back under way covers it.
            return None;
        }
        let site = site_of(url);
        let (at, times) = self.sent_from.entry(site.clone()).or_insert((now, 0));
        if now.duration_since(*at) > WITHIN {
            (*at, *times) = (now, 0);
        }
        if *times >= TIMES {
            tracing::info!("{site} keeps coming back: taken as part of the host's pages");
            ads().remove(&site);
            self.sites.insert(site);
            self.last = url.to_string();
            return None;
        }
        *times += 1;
        self.sending = Some(now);
        tracing::info!("sent the window back from {site}");
        ads().insert(site);
        Some(match self.opened.take() {
            Some((page, _)) => SendBack {
                page,
                history: false,
            },
            None => SendBack {
                page: self.last.clone(),
                history: true,
            },
        })
    }

    /// The page asks for a new window at `url`: true when it is on the
    /// host's site, to load in place after [`GRACE`] unless the page goes on
    /// by itself first (see [`Stay::open_now`]).
    pub fn opened(&mut self, url: &str, now: Instant) -> bool {
        if !is_web(url) || !self.is_home(url) {
            return false;
        }
        self.opened = Some((url.to_string(), now));
        true
    }

    /// The window the page opened at `at`, when it is still waiting: the
    /// page did not go on by itself, so it loads in place.
    pub fn open_now(&mut self, at: Instant) -> Option<String> {
        match &self.opened {
            Some((_, when)) if *when == at => self.opened.take().map(|(url, _)| url),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: &str = "https://www.host.example/f/abc";

    #[test]
    fn an_ad_redirect_is_sent_back() {
        let now = Instant::now();
        let mut stay = Stay::new(PAGE, Keep::Host);
        assert_eq!(stay.landed(PAGE, now), None);
        assert_eq!(stay.landed("https://host.example/f/abc?wait=1", now), None);
        let back = stay.landed("https://ads.example/x?z=1", now).unwrap();
        assert_eq!(
            back,
            SendBack {
                page: "https://host.example/f/abc?wait=1".into(),
                history: true
            }
        );
        // The ad's next redirect is part of the same sending back.
        assert_eq!(stay.landed("https://popup.example/y", now), None);
        assert!(!stay.is_home("https://popup.example/y"));
        assert!(stay.is_home("about:blank"), "no web page");
        // Back on the host, a later ad is sent back again.
        assert_eq!(stay.landed(PAGE, now), None);
        assert!(stay.landed("https://popup.example/y", now).is_some());
    }

    #[test]
    fn an_ads_site_is_not_loaded_again() {
        let now = Instant::now();
        let mut first = Stay::new("https://one.example/f/a", Keep::Host);
        first.landed("https://one.example/f/a", now);
        assert_eq!(
            first.navigating("https://casino-ad.test/x"),
            Go::Let,
            "not known yet"
        );
        assert!(first.landed("https://casino-ad.test/x", now).is_some());
        // Any window, from then on: refused before it loads.
        let mut other = Stay::new("https://two.example/f/b", Keep::Host);
        other.landed("https://two.example/f/b", now);
        assert_eq!(other.navigating("https://www.casino-ad.test/y"), Go::Ad);
        assert_eq!(other.navigating("https://two.example/next"), Go::Let);
        assert_eq!(
            other.navigating("https://cdn.elsewhere.test/file.bin"),
            Go::Let
        );
        // A page that lists links goes where it will.
        let mut free = Stay::new("https://forum.example/post/1", Keep::Free);
        free.landed("https://forum.example/post/1", now);
        assert_eq!(free.navigating("https://casino-ad.test/z"), Go::Let);
    }

    #[test]
    fn the_host_is_where_its_first_page_lands() {
        let now = Instant::now();
        let mut stay = Stay::new("https://vikingfile.com/f/x", Keep::Host);
        assert_eq!(stay.landed("https://vik1ngfile.site/f/x", now), None);
        assert_eq!(
            stay.landed("https://cdn.vik1ngfile.site/f/x/next", now),
            None
        );
        assert_eq!(stay.landed("https://vikingfile.com/f/x", now), None);
        assert!(stay.landed("https://ads.example/", now).is_some());
    }

    #[test]
    fn a_site_the_page_keeps_going_to_is_let_be() {
        let mut now = Instant::now();
        let mut stay = Stay::new(PAGE, Keep::Host);
        stay.landed(PAGE, now);
        for _ in 0..TIMES {
            now += SENDING;
            assert!(stay.landed("https://mirror.example/dl", now).is_some());
            stay.landed(PAGE, now);
        }
        now += SENDING;
        assert_eq!(stay.landed("https://mirror.example/dl", now), None);
        assert!(stay.is_home("https://mirror.example/other"));
        // Times further apart than that are ads coming back, not a flow.
        let mut stay = Stay::new(PAGE, Keep::Host);
        stay.landed(PAGE, now);
        for _ in 0..=TIMES {
            now += WITHIN + Duration::from_secs(1);
            assert!(stay.landed("https://ads.example/", now).is_some());
            stay.landed(PAGE, now);
        }
    }

    #[test]
    fn a_wrappers_window_stays_on_its_site() {
        let now = Instant::now();
        let page = "https://filecrypt.cc/Container/ABC123.html";
        let mut stay = Stay::new(page, Keep::Wrapper);
        // The way to its first page goes as it will.
        assert_eq!(stay.navigating("https://check.example/verify"), Go::Let);
        stay.landed(page, now);
        // Its own pages, and Cloudflare's check in front of them.
        assert_eq!(
            stay.navigating("https://www.filecrypt.cc/Link/1.html"),
            Go::Let
        );
        // Ads, each time on a new site, are refused before they load.
        for ad in [
            "https://abcdefghijk.site/ad/visit.php?al=1",
            "https://lmnopqrstuv.space/ad/visit.php?al=1",
            "https://checkout.example/offer?cid=1",
        ] {
            assert_eq!(stay.navigating(ad), Go::OffSite, "{ad}");
        }
        // A link the person follows to a host Haul knows, and a file.
        assert_eq!(stay.navigating("https://1fichier.com/?abc123"), Go::Let);
        assert_eq!(
            stay.navigating("https://cdn.files.example/archive.part1.rar"),
            Go::Let
        );
        // Nothing is sent back: what loads was let go.
        assert_eq!(stay.landed("https://1fichier.com/?abc123", now), None);
        // Its own pages opened in a new window still load in place; others
        // do not open at all.
        assert!(stay.opened("https://filecrypt.cc/Link/1.html", now));
        assert!(!stay.opened("https://ads.example/", now));
    }

    #[test]
    fn a_new_window_on_the_host_loads_in_place() {
        let now = Instant::now();
        let later = now + GRACE;
        let mut stay = Stay::new(PAGE, Keep::Host);
        stay.landed(PAGE, now);
        assert!(!stay.opened("https://ads.example/", now));
        assert!(stay.opened("https://host.example/f/abc/go", now));
        // A newer window replaces it.
        assert!(stay.opened("https://host.example/f/abc/go2", later));
        assert_eq!(stay.open_now(now), None);
        assert_eq!(
            stay.open_now(later).as_deref(),
            Some("https://host.example/f/abc/go2")
        );
        assert_eq!(stay.open_now(later), None, "once");

        // The page went on by itself: its window was the host's own upsell.
        assert!(stay.opened("https://host.example/premium", now));
        stay.navigating("https://host.example/f/abc/next");
        assert_eq!(stay.open_now(now), None);

        // A tab-under: the wanted page in a new window, this one to an ad.
        assert!(stay.opened("https://host.example/f/abc/wanted", now));
        stay.navigating("https://ads.example/");
        let back = stay.landed("https://ads.example/", now).unwrap();
        assert_eq!(
            back,
            SendBack {
                page: "https://host.example/f/abc/wanted".into(),
                history: false
            }
        );
        assert_eq!(stay.open_now(now), None, "taken by the sending back");
    }
}
