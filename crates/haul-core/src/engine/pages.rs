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
    item::{Item, Mirror, PageTurn, Status, Trouble},
};

/// How long a file waits before its host's page is passed again, after the
/// page could not hand out its link.
const PAGE_LATER: Duration = Duration::from_secs(90);

/// The longest wait a page's own word brings: a wrong number on a page stops
/// a host for a day at worst.
const PAGE_LIMIT_AT_MOST: Duration = Duration::from_secs(24 * 3600);

/// What a paste came to ([`Engine::paste`]).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Pasted {
    /// The download that took the link.
    pub used_for: Option<String>,
    pub name: Option<String>,
    /// The text, for the Add dialog.
    pub open_add: Option<String>,
}

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

    /// Whether row `it` is a wrapper's file, not started, that nothing named
    /// (its host tells nothing without its account, as gofile's): the
    /// desktop app opens its page out of sight to learn its name and size,
    /// so it joins the same file on other hosts, or that it is gone
    /// ([`Engine::look_up_page`]). It stays as it is meanwhile, and a pass
    /// for it asks nobody.
    pub(crate) fn looks_up(&self, it: &Item) -> bool {
        self.pages_pass()
            && !it.wrapper
            && !it.tentative
            && it.status == Status::Paused
            && !it.running()
            && it.done == 0
            && it.probed
            && it.name.is_none()
            && it.source.is_some()
    }

    /// The page the desktop app opens to look row `it` up
    /// ([`Engine::looks_up`]): its first one the browser passes, not yet
    /// looked at and not failed.
    pub(super) fn look_up_page<'a>(&self, st: &State, it: &'a Item) -> Option<&'a str> {
        if !self.looks_up(it) {
            return None;
        }
        let cfg = self.cfg();
        it.mirrors
            .iter()
            .find(|m| {
                st.passes(m)
                    && !it.failed.contains(m.as_str())
                    && !it.looked.contains(m.as_str())
                    && !cfg.is_disabled(&m.host)
            })
            .map(|m| m.as_str())
    }

    /// Row `id`'s page being looked up, when its pass is for that.
    fn looking_up(&self, st: &State, id: &str) -> Option<String> {
        self.look_up_page(st, st.items.get(id)?).map(str::to_string)
    }

    /// The page looked up for row `id` told nothing this time: it is not
    /// opened for that again, and the row stays as it was. False when the
    /// pass was not a lookup.
    fn looked_up_nothing(&self, id: &str) -> bool {
        let mut st = self.lock();
        let Some(page) = self.looking_up(&st, id) else {
            return false;
        };
        tracing::info!(item = %id, "{page} told nothing of its file");
        if let Some(it) = st.items.get_mut(id) {
            it.looked.insert(page);
        }
        self.publish(st);
        true
    }

    /// What the page looked up for row `id` handed back: one file names the
    /// row, which then joins the same file on other hosts or is found on
    /// disk; a folder of several becomes a row each, not started either.
    /// Their links stay the page, passed again when they start. The files
    /// come back when the pass was not a lookup.
    fn looked_up(&self, id: &str, files: Vec<Entry>) -> Result<(), Vec<Entry>> {
        let mut st = self.lock();
        let Some(page) = self.looking_up(&st, id) else {
            return Err(files);
        };
        let named: Vec<hosts::Probed> = files
            .into_iter()
            .filter_map(|f| {
                Some(hosts::Probed {
                    name: f.file_name()?,
                    size: f.size,
                    checksum: f.checksum,
                })
            })
            .collect();
        let Some(it) = st.items.get_mut(id) else {
            return Ok(());
        };
        it.looked.insert(page.clone());
        if named.is_empty() {
            tracing::info!(item = %id, "{page} named no file");
            self.publish(st);
            return Ok(());
        }
        tracing::info!(item = %id, "{page} holds {} file(s)", named.len());
        let mut rows = vec![(id.to_string(), named[0].clone())];
        if named.len() > 1 {
            let after = st.items.get_index_of(id).map_or(st.items.len(), |i| i + 1);
            for (at, found) in (after..).zip(&named[1..]) {
                let it = &st.items[id];
                let mut row = Item::new(it.mirrors.clone(), None, it.package.clone());
                row.status = Status::Paused;
                row.unstarted = it.unstarted;
                row.probed = true;
                row.source.clone_from(&it.source);
                row.also_from.clone_from(&it.also_from);
                row.extract_password.clone_from(&it.extract_password);
                row.looked.insert(page.clone());
                rows.push((row.id.clone(), found.clone()));
                let at_most = st.items.len();
                st.items.shift_insert(at.min(at_most), row.id.clone(), row);
            }
        }
        self.touch(&mut st);
        drop(st);
        for (row, found) in rows {
            let engine = self.clone();
            self.0
                .runtime
                .spawn(async move { engine.take_found(&row, Some(found)).await });
        }
        Ok(())
    }

    /// The page looked up for row `id` says its file is gone: that link is
    /// dead, and a row left with no other goes, never having been started.
    /// False when the pass was not a lookup.
    fn looked_up_gone(&self, id: &str, reason: &str) -> bool {
        let mut st = self.lock();
        let Some(page) = self.looking_up(&st, id) else {
            return false;
        };
        let Some(it) = st.items.get_mut(id) else {
            return true;
        };
        tracing::info!(item = %id, "{page}: {reason}");
        it.gone.insert(page.clone());
        it.failed.insert(page);
        if it.mirrors.iter().all(|m| it.gone.contains(m.as_str())) {
            tracing::info!(item = %id, "every link of this file is gone: it leaves the list");
            st.items.shift_remove(id);
            super::drop_spent_wrappers(&mut st.items);
        }
        self.publish(st);
        true
    }

    /// What a host's page handed back for item `id`: its file's link, or a
    /// folder's files, each row with its own. One file keeps the row and its
    /// other mirrors ([`Engine::set_direct`]); several take its place.
    pub fn set_files(&self, id: &str, files: Vec<Entry>) -> Result<(), String> {
        // A file not started, looked up, only learns what it is.
        let Err(files) = self.looked_up(id, files) else {
            return Ok(());
        };
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
        if self.looked_up_nothing(id) || self.drop_tentative(id, "wants a person") {
            return false;
        }
        let mut st = self.lock();
        let mut tell = true;
        if let Some(it) = st.items.get(id) {
            // Nobody hears of it while its page has tries of its own left;
            // once they are spent, the person is told, once.
            let left = self.page_retry_at(&st, it, Instant::now()).is_some();
            tell = !left && !it.told_person;
            if tell {
                self.ask_in(&mut st, id);
                if let Some(it) = st.items.get_mut(id) {
                    it.told_person = true;
                }
            }
            let Some(it) = st.items.get_mut(id) else {
                return tell;
            };
            it.page = if it.page_retrying() {
                PageTurn::WithPersonAgain
            } else {
                PageTurn::WithPerson
            };
            if it.status == Status::Captcha && it.task.is_none() {
                // Held for that page: its other mirrors may go now, which
                // plans the next try if they come to nothing. A wrapper has
                // none and waits for the person from now on, downloads paused
                // or not, and a try of its own had nothing else left either.
                if it.wrapper || it.page_retrying() {
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

    /// The desktop app's browser found `gate` on item `id`'s page at `url`:
    /// a row that says it waits at a gate names that one, since what the
    /// browser loaded is surer than what Haul read in the page's code.
    pub fn page_shows_gate(&self, id: &str, url: &str, gate: hosts::gates::Gate) {
        let mut st = self.lock();
        let Some(it) = st.items.get_mut(id) else {
            return;
        };
        let said = hosts::gate_message(url, Some(gate));
        if it.trouble != Trouble::Needs(hosts::Need::Gate) || it.error == said {
            return;
        }
        it.set_error(said, Trouble::Needs(hosts::Need::Gate));
        self.publish(st);
    }

    /// As [`Engine::page_wants_person`], for a pass whose browser could not
    /// show the page at all (`why`): the row says so, not what stopped Haul
    /// reading the page before, since the page itself was never seen.
    pub fn page_not_passed(&self, id: &str, why: &str) -> bool {
        let tell = self.page_wants_person(id);
        let mut st = self.lock();
        if let Some(it) = st.items.get_mut(id) {
            let trouble = it.trouble;
            it.set_error(
                format!("Haul's browser did not open the page ({why}); open it here"),
                trouble,
            );
        }
        self.publish(st);
        tell
    }

    /// As [`Engine::page_wants_person`], for a page past its checks that
    /// shows all it will: only a click from the person leads on, so it is
    /// not passed again on a timer.
    pub fn page_needs_click(&self, id: &str) -> bool {
        if self.looked_up_nothing(id) {
            return false;
        }
        // Another look shows the same: no try of its own, the person is
        // told at once.
        if let Some(it) = self.lock().items.get_mut(id) {
            it.auto_retries = AUTO_RETRIES;
        }
        let tell = self.page_wants_person(id);
        let mut st = self.lock();
        if let Some(it) = st.items.get_mut(id) {
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
        // Without one coming due, the desktop app asks again every few
        // seconds: nothing for the views to redraw.
        if self.with_person(&mut st, id) {
            self.touch(&mut st);
        }
    }

    /// [`Engine::page_with_person`] under the lock; whether a try of its own
    /// had come due.
    pub(super) fn with_person(&self, st: &mut State, id: &str) -> bool {
        let Some(it) = st.items.get(id) else {
            return false;
        };
        let came_due = it.page_retrying()
            && !it.page_wants_person()
            && it.status == Status::Captcha
            && it.task.is_none();
        if !came_due && it.retry_at.is_none() {
            return false;
        }
        if came_due && let Some(it) = st.items.get_mut(id) {
            tracing::info!(item = %id, "its page is open with a person: its try on its own waits");
            it.page = PageTurn::WithPersonAgain;
            it.auto_retries = it.auto_retries.saturating_sub(1);
        }
        let at = self.page_retry_at(st, &st.items[id], Instant::now());
        if let Some(it) = st.items.get_mut(id) {
            it.retry_at = at;
        }
        came_due
    }

    /// A host's page passed out of sight could not hand out its link this
    /// time (a rate limit, an error it shows, no answer): the item waits a
    /// while and the page is passed again, asking nobody. With `seconds`, page
    /// `page` said how long, which is its host's limit on this address rather
    /// than this file's ([`Engine::hold_passes`]).
    pub fn page_later(&self, id: &str, page: &str, reason: &str, seconds: Option<u64>) {
        // A host's limit on the address is a wait, for a lookup too: its
        // page is passed again once the host's cooldown is over, not left
        // as one that told nothing.
        let limited_lookup = seconds.is_some() && {
            let st = self.lock();
            self.looking_up(&st, id).is_some()
        };
        let looked_up = limited_lookup || self.looked_up_nothing(id);
        let mut st = self.lock();
        let now = Instant::now();
        if limited_lookup {
            tracing::info!(item = %id, "page later, looked up again after the host's wait: {reason}");
            st.tried
                .retain(|(row, passed, _)| !(row == id && passed == page));
        }
        let said = seconds.map(|s| Duration::from_secs(s).min(PAGE_LIMIT_AT_MOST));
        if let Some(wait) = said {
            let host = hosts::host_of(page);
            // The way out the page's pass took, and the next one's.
            if let Some(&exit) = self.exits_for(&host, &st).first() {
                st.lane(&host, exit).asked_to_wait(now + wait);
            }
        }
        if let Some(it) = st.items.get_mut(id).filter(|_| !looked_up) {
            tracing::info!(item = %id, "page later: {reason}");
            it.set_error(reason, Trouble::Other);
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
        self.pass_lane(st, url)?
            .cooldown
            .filter(|until| now < *until)
    }

    /// Page `url`'s host as its pass reaches it: through the first of the
    /// host's exits.
    fn pass_lane<'a>(&self, st: &'a State, url: &str) -> Option<&'a super::Lane> {
        let host = hosts::host_of(url);
        let exit = *self.exits_for(&host, st).first()?;
        st.lanes.get(&host, exit)
    }

    /// As [`Engine::pass_held_until`], for page `url` of row `it`: a host that
    /// only slows the address still serves it, so its cooldown holds the page
    /// back only while the row has somewhere else to go. A wait a host asked
    /// for always holds.
    pub(super) fn pass_held_for(
        &self,
        st: &State,
        it: &Item,
        url: &str,
        now: Instant,
    ) -> Option<Instant> {
        let until = self.pass_held_until(st, url, now)?;
        let slowing = self.pass_lane(st, url).is_some_and(|l| l.slowing);
        (!slowing || self.has_another_way(st, it, url, now)).then_some(until)
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
            .filter(|m| !cfg.is_disabled(&hosts::host_of(m)))
            .map(|m| {
                st.passes(m)
                    .then(|| self.pass_held_for(st, it, m, now))
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
        takes_clearance(&self.lock(), url)
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
            if it.trouble.is_guarded() {
                it.clear_error();
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
        if !self.page_lists(id, &page) {
            return 0;
        }
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
        self.take_unwrapped(id, &page, hosts::listed(title, known));
        count
    }

    /// Whether the links page `url` shows may stand in for row `id`
    /// ([`Engine::page_listed`]): the row is that page, a wrapper or a link
    /// added on its own that turned out to be a page, on a site Haul has no
    /// code for. A download's host page is passed for its file, whatever
    /// links it shows besides (its ads, its other files): a part of an
    /// archive stays that part, and so does a link whose path names a file.
    pub fn page_lists(&self, id: &str, url: &str) -> bool {
        let st = self.lock();
        let Some(it) = st.items.get(id) else {
            return false;
        };
        let added_page = it.mirrors == [url]
            && it.source.is_none()
            && it.size == 0
            && hosts::guess_name(url).is_none();
        hosts::may_list_links(url) && (it.wrapper || it.tentative || added_page)
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
        self.take_unwrapped(id, &page, found);
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
        let looked_up = match for_good {
            true => self.looked_up_gone(id, reason),
            false => self.looked_up_nothing(id),
        };
        if looked_up || self.drop_tentative(id, reason) {
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
            // A paste the pass found empty is a note beside a card's others.
            let trouble = if for_good && hosts::is_paste(&page) {
                Trouble::ListsNothing
            } else {
                Trouble::Other
            };
            it.failed.insert(page);
            it.set_error(reason, trouble);
            if it.status == Status::Captcha {
                it.status = Status::Queued;
            }
        }
        self.publish(st);
    }

    /// A paste outside any field: one link while one download waits for a
    /// person is that download's link, handed back; anything else is text
    /// for the Add dialog.
    pub fn paste(&self, text: &str) -> Result<Pasted, String> {
        let link = text.trim();
        if hosts::is_http(link) && !link.chars().any(char::is_whitespace) {
            let waiting: Vec<(String, String)> = self
                .snapshot()
                .items
                .into_iter()
                .filter(|i| i.list == Some(super::List::Attention))
                .map(|i| (i.id, i.name))
                .collect();
            if let [(id, name)] = waiting.as_slice() {
                self.set_direct(id, link, BTreeMap::new(), None)?;
                super::friction::clicked(&mut self.lock(), id);
                return Ok(Pasted {
                    used_for: Some(id.clone()),
                    name: Some(name.clone()),
                    open_add: None,
                });
            }
        }
        Ok(Pasted {
            used_for: None,
            name: None,
            open_add: Some(text.to_string()),
        })
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
        let handed_for = self.handed_for(&st, id, url);
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
            let name = hosts::name_for(url, name_hint);
            let mut file = Item::new(vec![url.to_string()], name.clone(), package);
            file.status = Status::Paused;
            file.unstarted = true;
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
            let at = st.items.get_index_of(id).map_or(st.items.len(), |i| i + 1);
            self.insert_row(&mut st, at, file);
            self.touch(&mut st);
            return Ok(());
        }
        if it.name.is_none() && !deferred {
            it.name = hosts::name_for(url, name_hint).map(|n| hosts::tidy_name(&n));
        }
        it.pre = Some(Entry {
            url: url.into(),
            name: if deferred { None } else { it.name.clone() },
            headers,
            deferred,
            ..Entry::default()
        });
        it.pre_for = handed_for;
        it.manual = true;
        it.probed = true;
        // A copied page that hands back a file is a download.
        it.tentative = false;
        it.queue_if_idle();
        it.reset_retries();
        it.not_before = None;
        it.manual_urls.clear();
        // A copy reported deleted stays out, whatever else was handed back.
        let gone = it.gone.clone();
        it.failed.retain(|m| gone.contains(m));
        if let Some(m) = it.pre_for.clone() {
            it.failed.remove(&m);
            it.gone.remove(&m);
        }
        self.publish(st);
        Ok(())
    }

    /// The mirror of row `id` a link handed back belongs to, whichever domain
    /// the link itself is on (a host's files are often on a CDN of its own):
    /// one on the link's own host, or the page that was being passed, or the
    /// page a person was sent to. The row is then shown under that host, and
    /// what the transfer shows of the host counts for it.
    fn handed_for(&self, st: &State, id: &str, url: &str) -> Option<String> {
        let it = st.items.get(id)?;
        let now = Instant::now();
        let host = hosts::host_of(url);
        let open = || {
            it.mirrors
                .iter()
                .filter(|m| it.manual_urls.contains(m.as_str()))
        };
        let on_host = |m: &&Mirror| *m.host == *host;
        let passed = || {
            (self.pages_pass() && !it.page_wants_person())
                .then(|| {
                    open().find(|m| st.passes(m) && self.pass_held_for(st, it, m, now).is_none())
                })
                .flatten()
        };
        open()
            .find(on_host)
            .or_else(|| it.mirrors.iter().find(on_host))
            .or_else(passed)
            .or_else(|| open().find(|m| self.pass_held_for(st, it, m, now).is_none()))
            .or_else(|| open().next())
            .or_else(|| it.mirrors.first())
            .map(|m| m.url.clone())
    }

    /// Whether the desktop app passes page `url` by itself, out of sight, now:
    /// a host's whose page does ([`hosts::page_passes`]), or one that answered
    /// with a browser check a browser usually passes, unless its host asked
    /// the address to wait, or a new address is about to make what it hands
    /// out useless. A host only slowing the address holds it back unless a row
    /// has nowhere else to go.
    pub fn page_passes(&self, url: &str) -> bool {
        self.passes_now(&self.lock(), url, Instant::now())
    }

    /// [`Engine::page_passes`] under the lock.
    pub(super) fn passes_now(&self, st: &State, url: &str, now: Instant) -> bool {
        self.passes_now_among(st, url, now, st.items.values())
    }

    /// [`Engine::passes_now`], asking only `rows` (those that wait on page
    /// `url`, when the caller knows them) whether one is free to go.
    pub(super) fn passes_now_among<'a>(
        &self,
        st: &State,
        url: &str,
        now: Instant,
        mut rows: impl Iterator<Item = &'a Item>,
    ) -> bool {
        let mut free = || {
            self.pass_held_until(st, url, now).is_none()
                || rows.any(|it| {
                    it.manual_urls.contains(url) && self.pass_held_for(st, it, url, now).is_none()
                })
        };
        st.draining.is_none() && st.passes(url) && free()
    }

    /// Whether row `it`, waiting for a person, has the desktop app pass one
    /// of its pages by itself, in turn, first: nothing for the person yet.
    pub(super) fn passing(&self, st: &State, it: &Item, now: Instant) -> bool {
        self.pages_pass()
            && !it.page_wants_person()
            && it
                .manual_urls
                .iter()
                .any(|u| st.passes(u) && self.pass_held_for(st, it, u, now).is_none())
    }

    /// The page a person should open for this item: not one whose host asked
    /// the address to wait, while it has another.
    pub fn page_for(&self, id: &str) -> Option<String> {
        let st = self.lock();
        let it = st.items.get(id)?;
        let now = Instant::now();
        let open = || {
            it.mirrors
                .iter()
                .filter(|m| it.manual_urls.contains(m.as_str()))
        };
        open()
            .find(|m| self.pass_held_for(&st, it, m, now).is_none())
            .or_else(|| open().next())
            .or_else(|| it.mirrors.first())
            .map(|m| m.url.clone())
    }
}

/// Whether the desktop app, passing page `url`, hands Haul the clearance
/// ([`Engine::takes_clearance`]).
pub(super) fn takes_clearance(st: &State, url: &str) -> bool {
    hosts::reads_past_check(url) && !st.cleared.contains(url)
}
