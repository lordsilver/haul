//! Hosts' pages, as the desktop app's browser passes them: which pages it
//! passes, and what became of one, whether it handed back a link or a
//! folder's files, listed links, said its file is gone, or wants a person.

use std::{
    collections::BTreeMap,
    sync::atomic::Ordering,
    time::{Duration, Instant},
};

use super::Engine;
use crate::{
    hosts::{self, Entry},
    item::{Item, Status},
};

/// How long a file waits before its host's page is passed again, after the
/// page could not hand out its link.
const PAGE_LATER: Duration = Duration::from_secs(90);

impl Engine {
    /// Says the desktop app passes the pages of hosts that allow it out of
    /// sight ([`hosts::Known::page_passes`]).
    pub fn set_pages_pass(&self, on: bool) {
        self.0.pages_pass.store(on, Ordering::Relaxed);
        self.publish(self.lock());
    }

    pub(crate) fn pages_pass(&self) -> bool {
        self.0.pages_pass.load(Ordering::Relaxed)
    }

    /// What a host's page handed back for item `id`: its file's link, or a
    /// folder's files, each row with its own. One file keeps the row and its
    /// other mirrors ([`Engine::set_direct`]); several take its place.
    pub fn set_files(&self, id: &str, files: Vec<Entry>) -> Result<(), String> {
        match files.len() {
            0 => Err("the page handed back no file".into()),
            1 => {
                let file = files.into_iter().next().expect("one");
                self.set_direct(id, &file.url, file.headers, file.name)
            }
            _ => {
                let page = self.page_for(id).ok_or("no such item")?;
                if files.iter().any(|f| !hosts::is_http(&f.url)) {
                    return Err("not an http(s) link".into());
                }
                self.expand(id, &page, files);
                self.0.wake.notify_one();
                Ok(())
            }
        }
    }

    /// Whether item `id` is a copied page still on trial, so the desktop
    /// app ends its pass as soon as the page settles.
    pub fn is_tentative(&self, id: &str) -> bool {
        self.lock().items.get(id).is_some_and(|it| it.tentative)
    }

    /// The desktop app's page for item `id` wants a person after all: the
    /// item shows as needing one from now on, until it is tried again.
    /// False when it was a copied page, which goes instead: nobody is told.
    pub fn page_wants_person(&self, id: &str) -> bool {
        if self.drop_tentative(id, "wants a person") {
            return false;
        }
        let mut st = self.lock();
        if let Some(it) = st.items.get_mut(id) {
            it.page_wants_person = true;
            // Held for that page: its other mirrors may go now. A wrapper
            // has none, and waits for the person from now on, downloads
            // paused or not.
            if it.status == Status::Captcha && it.task.is_none() && !it.wrapper {
                it.status = Status::Queued;
            }
        }
        self.publish(st);
        true
    }

    /// A host's page passed out of sight could not hand out its link this
    /// time (a rate limit, an error it shows, no answer): the item waits a
    /// while and the page is passed again, asking nobody.
    pub fn page_later(&self, id: &str, reason: &str) {
        let mut st = self.lock();
        if let Some(it) = st.items.get_mut(id) {
            tracing::info!(item = %id, "page later: {reason}");
            it.error = reason.to_string();
            if it.status == Status::Captcha && it.task.is_none() {
                it.status = Status::Waiting;
                it.not_before = Some(Instant::now() + PAGE_LATER);
            }
        }
        self.publish(st);
    }

    /// Whether the desktop app, passing page `url` out of sight, hands Haul
    /// the clearance once the browser is past the check in front of its
    /// site ([`Engine::page_cleared`]): for a page Haul's own code reads
    /// ([`hosts::reads_past_check`]), unless a clearance was handed for it
    /// before and Haul was turned away all the same.
    pub fn takes_clearance(&self, url: &str) -> bool {
        hosts::reads_past_check(url) && !self.lock().cleared.contains(url)
    }

    /// The browser passed the check in front of item `id`'s page (its
    /// site's Cloudflare check) and got `cookies` (`Set-Cookie` lines) for
    /// it as `user_agent`: Haul's own requests to the site carry them, and
    /// that user agent, from now on, so that page and the site's other pages
    /// waiting for a pass are read by Haul again, as before the site put the
    /// check in front of them. Called within the async runtime: a wrapper's
    /// unwrapping starts as a task.
    pub fn page_cleared(&self, id: &str, cookies: &[String], user_agent: Option<&str>) {
        let Some(page) = self.page_for(id) else {
            return;
        };
        let site = hosts::site_of(&page);
        let mut st = self.lock();
        self.http_for(&hosts::host_of(&page), &st)
            .add_clearance(&page, cookies, user_agent);
        let waiting: Vec<String> = st
            .guarded
            .iter()
            .filter(|u| hosts::site_of(u) == site && !st.cleared.contains(*u))
            .cloned()
            .collect();
        let mut again = Vec::new();
        for it in st.items.values_mut() {
            let cleared: Vec<String> = it
                .manual_urls
                .iter()
                .filter(|m| waiting.contains(m))
                .cloned()
                .collect();
            if cleared.is_empty() {
                continue;
            }
            tracing::info!(item = %it.id, "{site} passed its browser check: Haul reads its page again");
            for m in &cleared {
                it.manual_urls.remove(m);
            }
            if hosts::is_guarded(&it.error) {
                it.error.clear();
            }
            if it.status == Status::Captcha && it.task.is_none() {
                it.status = Status::Queued;
                // A wrapper is unwrapped again at once, as when it was
                // added, through the route the clearance is for.
                if it.wrapper {
                    it.probed = false;
                    again.push(it.id.clone());
                }
            }
        }
        st.cleared.extend(waiting);
        self.touch(&mut st);
        drop(st);
        for id in again {
            self.spawn_probe(id);
        }
        self.0.wake.notify_one();
    }

    /// A page opened for item `id` in the desktop app's browser (out of
    /// sight or by a person, past a check that kept Haul out) shows links to
    /// hosts Haul knows, wrappers or pastes: the row becomes a wrapper of
    /// them, named `title`, as a page Haul reads itself does. `links` is
    /// every address the page shows, the rest left out as for such a page;
    /// `shown` are those written out in its text, which on a paste are its
    /// list and count on any host. Returns how many it took; with none the
    /// row stays as it is.
    pub fn page_listed(
        &self,
        id: &str,
        links: &[String],
        shown: &[String],
        title: Option<&str>,
    ) -> usize {
        let Some(page) = self.page_for(id) else {
            return 0;
        };
        let mut known = hosts::listed_links(&links.join("\n"), &page);
        for link in hosts::pasted_links(shown, &page) {
            if !known.contains(&link) {
                known.push(link);
            }
        }
        if known.is_empty() {
            return 0;
        }
        let count = known.len();
        tracing::info!(item = %id, "{page} links {count} files and wrappers, in its window");
        let title = title.and_then(hosts::short_title);
        for id in self.unwrapped(id, &page, hosts::listed(title, known)) {
            self.spawn_probe(id);
        }
        count
    }

    /// Wrapper `id`'s page as the desktop app's browser shows it (`html`,
    /// the parts of it Haul reads), past a gate Haul cannot answer itself
    /// (filecrypt.cc's picture puzzle): the row unwraps to what it holds, as
    /// when Haul reads the page itself ([`hosts::shown`]). Returns how many
    /// files it took; with none the row stays as it is. Called within the
    /// async runtime: the files' lookups start as tasks.
    pub fn page_shown(&self, id: &str, html: &str) -> usize {
        let Some(page) = self.page_for(id) else {
            return 0;
        };
        let Some(found) = hosts::shown(&page, html) else {
            return 0;
        };
        let count = found.files.len();
        tracing::info!(item = %id, "{page} shows {count} files, in its window");
        for id in self.unwrapped(id, &page, found) {
            self.spawn_probe(id);
        }
        count
    }

    /// A host's page, passed out of sight, said its file is gone: that
    /// mirror failed like any other, and the item goes on to the next one
    /// (or fails with `reason` when none is left), asking nobody.
    pub fn page_gone(&self, id: &str, reason: &str) {
        self.page_ended(id, reason, true);
    }

    /// A page passed out of sight says something that may not last (a site
    /// answering "Not Found" to an address it refuses): that mirror failed
    /// as with [`Engine::page_gone`], asking nobody, but the item stays due
    /// for its automatic retries.
    pub fn page_failed(&self, id: &str, reason: &str) {
        self.page_ended(id, reason, false);
    }

    fn page_ended(&self, id: &str, reason: &str, for_good: bool) {
        if self.drop_tentative(id, reason) {
            return;
        }
        let Some(page) = self.page_for(id) else {
            return;
        };
        let mut st = self.lock();
        if let Some(it) = st.items.get_mut(id) {
            tracing::info!(item = %id, "{page}: {reason}");
            it.manual_urls.remove(&page);
            if for_good {
                it.gone.insert(page.clone());
            }
            it.failed.insert(page);
            it.error = reason.to_string();
            if it.status == Status::Captcha {
                it.status = Status::Queued;
            }
        }
        self.publish(st);
    }

    /// Uses a direct link the user supplied (pasted, or caught from the
    /// in-app browser) for this item.
    pub fn set_direct(
        &self,
        id: &str,
        url: &str,
        headers: BTreeMap<String, String>,
        name_hint: Option<String>,
    ) -> Result<(), String> {
        if !hosts::is_http(url) {
            return Err("not an http(s) link".into());
        }
        let mut st = self.lock();
        let it = st.items.get_mut(id).ok_or("no such item")?;
        if it.status == Status::Done {
            // Another mirror may have finished it while the user was busy.
            return Err("it is downloaded already".into());
        }
        // A wrapper link (the `.dlc` a gate hands out) is further links, not
        // the file: the row unpacks it on its next turn and becomes its
        // files. The row's own links stay, so one that fails asks for the page
        // again like any link handed back, and it names nothing.
        let deferred = hosts::wraps(url);
        if it.wrapper && !deferred {
            // A file's own link, found on a wrapper's page: one more of the
            // wrapper's files, not started, with what the browser sent.
            let package = it.package.clone();
            let name = name_hint
                .filter(|n| !n.is_empty())
                .or_else(|| hosts::guess_name(url));
            let mut file = Item::new(vec![url.to_string()], name.clone(), package);
            file.status = Status::Paused;
            file.source = Some(id.to_string());
            file.probed = true;
            if !headers.is_empty() {
                file.pre = Some(Entry {
                    url: url.into(),
                    name,
                    headers,
                    ..Entry::default()
                });
                file.manual = true;
            }
            let new_id = file.id.clone();
            let at = st.items.get_index_of(id).map_or(st.items.len(), |i| i + 1);
            st.items.shift_insert(at, new_id.clone(), file);
            if self.dedupe(&mut st, &new_id) {
                st.items.shift_remove(&new_id);
            }
            self.touch(&mut st);
            return Ok(());
        }
        if it.name.is_none() && !deferred {
            it.name = name_hint
                .filter(|n| !n.is_empty())
                .or_else(|| hosts::guess_name(url))
                .map(|n| hosts::tidy_name(&n));
        }
        it.pre = Some(Entry {
            url: url.into(),
            name: if deferred { None } else { it.name.clone() },
            headers,
            deferred,
            ..Entry::default()
        });
        it.pre_for = None;
        it.manual = true;
        it.probed = true;
        // A copied page that hands back a file is a download.
        it.tentative = false;
        if !it.running() {
            it.status = Status::Queued;
        }
        it.reset_retries();
        it.not_before = None;
        it.manual_urls.clear();
        it.failed.clear();
        self.publish(st);
        Ok(())
    }

    /// Whether the desktop app passes page `url` by itself, out of sight: a
    /// host's whose page does ([`hosts::page_passes`]), or one that answered
    /// with a browser check a browser usually passes.
    pub fn page_passes(&self, url: &str) -> bool {
        self.lock().passes(url)
    }

    /// The page a person should open for this item.
    pub fn page_for(&self, id: &str) -> Option<String> {
        let st = self.lock();
        let it = st.items.get(id)?;
        it.mirrors
            .iter()
            .find(|m| it.manual_urls.contains(*m))
            .or_else(|| it.mirrors.first())
            .cloned()
    }
}
