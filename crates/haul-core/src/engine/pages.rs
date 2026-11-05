//! Hosts' pages, as the desktop app's browser passes them: which pages it
//! passes, and what became of one, whether it handed back a link or a
//! folder's files, listed links, said its file is gone, or wants a person.

use std::{
    collections::BTreeMap,
    sync::atomic::Ordering,
    time::{Duration, Instant},
};

use super::{AUTO_RETRIES, Engine, State};
use crate::{
    hosts::{self, Entry},
    item::{Item, Status},
};

/// How long a file waits before its host's page is passed again, after the
/// page could not hand out its link.
const PAGE_LATER: Duration = Duration::from_secs(90);

/// The longest wait a page's own word brings: a wrong number on a page stops
/// a host for a day at worst.
const PAGE_LIMIT_AT_MOST: Duration = Duration::from_secs(24 * 3600);

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
    /// item shows as needing one, its page passed again on the failed
    /// downloads' timer ([`Engine::auto_retry`]) until one comes or its tries
    /// run out. False when nobody is to be told: a copied page, which goes
    /// instead, or a try of its own, the person having been told before.
    pub fn page_wants_person(&self, id: &str) -> bool {
        if self.drop_tentative(id, "wants a person") {
            return false;
        }
        let mut st = self.lock();
        let mut tell = true;
        if let Some(it) = st.items.get_mut(id) {
            tell = !it.page_retrying;
            it.page_wants_person = true;
            if it.status == Status::Captcha && it.task.is_none() {
                // Held for that page: its other mirrors may go now, which
                // plans the next try if they come to nothing. A wrapper has
                // none and waits for the person from now on, downloads paused
                // or not, and a try of its own had nothing else left either.
                if it.wrapper || it.page_retrying {
                    let at = self.page_retry_at(&st, &st.items[id], Instant::now());
                    if let Some(it) = st.items.get_mut(id) {
                        it.retry_at = at;
                    }
                } else {
                    it.status = Status::Queued;
                }
            }
        }
        self.publish(st);
        tell
    }

    /// As [`Engine::page_wants_person`], for a page past its checks that
    /// shows all it will: only a click from the person leads on, so it is
    /// not passed again on a timer.
    pub fn page_needs_click(&self, id: &str) -> bool {
        let tell = self.page_wants_person(id);
        let mut st = self.lock();
        if let Some(it) = st.items.get_mut(id) {
            it.auto_retries = AUTO_RETRIES;
            it.retry_at = None;
        }
        self.touch(&mut st);
        tell
    }

    /// A person has item `id`'s page open in Haul's window, which is theirs
    /// to finish: the next try of its own starts counting again from now,
    /// and one just come due goes back to them without using up a try.
    pub fn page_with_person(&self, id: &str) {
        let mut st = self.lock();
        let Some(it) = st.items.get(id) else {
            return;
        };
        let came_due = it.page_retrying
            && !it.page_wants_person
            && it.status == Status::Captcha
            && it.task.is_none();
        if !came_due && it.retry_at.is_none() {
            return;
        }
        if came_due && let Some(it) = st.items.get_mut(id) {
            tracing::info!(item = %id, "its page is open with a person: its try on its own waits");
            it.page_wants_person = true;
            it.auto_retries = it.auto_retries.saturating_sub(1);
        }
        let at = self.page_retry_at(&st, &st.items[id], Instant::now());
        if let Some(it) = st.items.get_mut(id) {
            it.retry_at = at;
        }
        // Without one, the desktop app asks again every few seconds: nothing
        // for the views to redraw.
        if came_due {
            self.touch(&mut st);
        }
    }

    /// A host's page passed out of sight could not hand out its link this
    /// time (a rate limit, an error it shows, no answer): the item waits a
    /// while and the page is passed again, asking nobody. With `seconds`, page
    /// `page` said how long, which is its host's limit on this address rather
    /// than this file's ([`Engine::hold_passes`]).
    pub fn page_later(&self, id: &str, page: &str, reason: &str, seconds: Option<u64>) {
        let mut st = self.lock();
        let now = Instant::now();
        let said = seconds.map(|s| Duration::from_secs(s).min(PAGE_LIMIT_AT_MOST));
        if let Some(wait) = said {
            let host = hosts::host_of(page);
            // The way out the page's pass took, and the next one's.
            if let Some(&exit) = self.exits_for(&host, &st).first() {
                st.lane(&host, exit).cooldown = Some(now + wait);
            }
        }
        if let Some(it) = st.items.get_mut(id) {
            tracing::info!(item = %id, "page later: {reason}");
            it.error = reason.to_string();
            if it.status == Status::Captcha && it.task.is_none() {
                if said.is_some() {
                    it.status = Status::Queued;
                } else {
                    it.status = Status::Waiting;
                    it.not_before = Some(now + PAGE_LATER);
                }
            }
        }
        if said.is_some() {
            self.hold_passes(&mut st, now);
        }
        self.publish(st);
    }

    /// Until when page `url` is not to be passed: a wait its host asked of
    /// the address its pass goes out from, the first of the host's, as
    /// [`Engine::proxy_for`] gives it.
    pub(super) fn pass_held_until(&self, st: &State, url: &str, now: Instant) -> Option<Instant> {
        let host = hosts::host_of(url);
        let exit = *self.exits_for(&host, st).first()?;
        st.lanes
            .get(&(host, exit))?
            .cooldown
            .filter(|until| now < *until)
    }

    /// When every page row `it` waits for is one the desktop app passes and
    /// is on hold: when the first of them may go. A row with a page for a
    /// person is theirs to take on, with no need to wait.
    pub(super) fn pages_held_until(&self, st: &State, it: &Item, now: Instant) -> Option<Instant> {
        if !self.pages_pass() {
            return None;
        }
        let cfg = self.cfg();
        it.manual_urls
            .iter()
            .filter(|m| !cfg.is_off(&hosts::host_of(m)))
            .map(|m| {
                st.passes(m)
                    .then(|| self.pass_held_until(st, m, now))
                    .flatten()
            })
            .collect::<Option<Vec<Instant>>>()?
            .into_iter()
            .min()
    }

    /// A host asked an address to wait: rows waiting on a page that is now
    /// on hold, a pass's turn or a person, take their next turn in the queue,
    /// which sends them to their other mirrors or has them wait it out, one
    /// wait over the host's cooldown rather than one for each row.
    pub(super) fn hold_passes(&self, st: &mut State, now: Instant) {
        let held: Vec<String> = st
            .items
            .values()
            .filter(|it| it.status == Status::Captcha && it.task.is_none())
            .filter(|it| self.pages_held_until(st, it, now).is_some())
            .map(|it| it.id.clone())
            .collect();
        for id in held {
            if let Some(it) = st.items.get_mut(&id) {
                tracing::info!(item = %id, "its page's host asked this address to wait");
                it.status = Status::Queued;
            }
        }
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

    /// Whether the desktop app passes page `url` by itself, out of sight, now:
    /// a host's whose page does ([`hosts::page_passes`]), or one that answered
    /// with a browser check a browser usually passes, unless its host asked
    /// the address to wait.
    pub fn page_passes(&self, url: &str) -> bool {
        let st = self.lock();
        st.passes(url) && self.pass_held_until(&st, url, Instant::now()).is_none()
    }

    /// The page a person should open for this item: not one whose host asked
    /// the address to wait, while it has another.
    pub fn page_for(&self, id: &str) -> Option<String> {
        let st = self.lock();
        let it = st.items.get(id)?;
        let now = Instant::now();
        let open = || it.mirrors.iter().filter(|m| it.manual_urls.contains(*m));
        open()
            .find(|m| self.pass_held_until(&st, m, now).is_none())
            .or_else(|| open().next())
            .or_else(|| it.mirrors.first())
            .cloned()
    }
}
