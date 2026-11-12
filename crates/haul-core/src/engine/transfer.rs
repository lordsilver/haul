//! One attempt at a download, from its turn in the queue to its end: the
//! link resolved, the bytes fetched into the partial file, the file placed,
//! and `finish`, which settles what the attempt's outcome means for the row.

use std::{
    path::{Path, PathBuf},
    sync::atomic::Ordering,
    time::{Duration, Instant},
};

use bytes::Bytes;
use reqwest::header::{ACCEPT_ENCODING, RANGE};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt, BufWriter};
use tokio_util::sync::CancellationToken;

use super::{Engine, Event, Exit, SETTLE, State, display_name, drop_tentative_row, noted_quietly};
use crate::{
    hosts::{self, Entry, Http, Need, ResolveError},
    item::{Item, Status, Trouble},
    paths,
    throttle::Pace,
};

/// Refusals in a row from a busy host before a download counts as failed.
const BUSY_REFUSALS: u32 = 10;

/// Seconds a download refused by a busy host waits before it may try again
/// (and only then if the host has a free slot).
const BUSY_PAUSE: u64 = 5;

/// How much of a partial file another host's bytes have to agree with before
/// it is resumed from there. Hosts hold compressed archives, so 64 KiB that
/// agree at that very offset are the same file; a file of one name and exact
/// size can be another one, as sets split alike are. It costs next to nothing.
const OVERLAP: u64 = 64 * 1024;

/// Counts a download as under way with its host, from the request to the end
/// of the transfer.
struct Serving<'a> {
    engine: &'a Engine,
    host: &'a str,
    exit: Exit,
}

impl<'a> Serving<'a> {
    fn start(engine: &'a Engine, host: &'a str, exit: Exit) -> Self {
        engine.lock().lane(host, exit).serving += 1;
        Self { engine, host, exit }
    }
}

impl Drop for Serving<'_> {
    fn drop(&mut self) {
        let mut st = self.engine.lock();
        let lane = st.lane(self.host, self.exit);
        lane.serving = lane.serving.saturating_sub(1);
        if lane.cap.is_some() {
            lane.settle_until = Some(Instant::now() + SETTLE);
        }
    }
}

/// How an attempt ended without an error.
enum Outcome {
    Done,
    /// The row is gone: merged into another, or expanded into a folder's or
    /// a wrapper's files.
    Gone,
    /// The file was in the download folder already, at this path.
    Existing(PathBuf),
    /// Too big to download next to the files already downloading, within
    /// `max_active_gb`: back in the queue.
    Deferred,
    /// A wrapper's files are in the queue, not started; the row is done.
    Unwrapped,
}

/// How often a transfer tells the state how far it is.
const TELL_EVERY: Duration = Duration::from_millis(250);

enum Fail {
    Cancelled,
    Wait {
        seconds: u64,
        message: String,
    },
    NeedsUser(Need, String),
    Error(String),
    /// The host says the file is no more.
    Gone(String),
    /// The connection broke mid-transfer; worth resuming on the same mirror.
    Transient(String),
    /// The host refused one more download while `allowed` others from it ran.
    Busy {
        allowed: usize,
        message: String,
    },
    /// This computer's own trouble (no room, no permission): no host's doing,
    /// so no host is blamed and no mirror is used up.
    Local {
        message: String,
        /// The disk is full: every other download is about to fail the same way.
        full: bool,
    },
}

impl From<ResolveError> for Fail {
    fn from(e: ResolveError) -> Self {
        match e {
            ResolveError::Wait { seconds, message } => Fail::Wait { seconds, message },
            ResolveError::NeedsUser(need, m) => Fail::NeedsUser(need, m),
            ResolveError::Network(m) => Fail::Transient(m),
            ResolveError::Failed(m) => Fail::Error(m),
            ResolveError::Gone(m) => Fail::Gone(m),
        }
    }
}

impl From<reqwest::Error> for Fail {
    fn from(e: reqwest::Error) -> Self {
        // A host that cannot be reached right now may well be back shortly.
        ResolveError::from(e).into()
    }
}

impl From<std::io::Error> for Fail {
    fn from(e: std::io::Error) -> Self {
        use std::io::ErrorKind;
        Fail::Local {
            full: matches!(e.kind(), ErrorKind::StorageFull | ErrorKind::QuotaExceeded),
            message: e.to_string(),
        }
    }
}

async fn or_cancel<T>(cancel: &CancellationToken, fut: impl Future<Output = T>) -> Result<T, Fail> {
    tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(Fail::Cancelled),
        v = fut => Ok(v),
    }
}

impl Engine {
    // ---- attempts

    pub(super) async fn work(
        self,
        id: String,
        url: String,
        host: String,
        exit: Exit,
        cancel: CancellationToken,
    ) {
        let result = self.attempt(&id, &url, &host, exit, &cancel).await;
        self.finish(&id, &url, &host, exit, result);
    }

    async fn attempt(
        &self,
        id: &str,
        url: &str,
        host: &str,
        exit: Exit,
        cancel: &CancellationToken,
    ) -> Result<Outcome, Fail> {
        let (pre, name, http, key_service) = {
            let mut st = self.lock();
            let it = st.items.get_mut(id).ok_or(Fail::Cancelled)?;
            it.attempt_from = it.done;
            let pre = it
                .pre
                .clone()
                .filter(|_| it.pre_for.as_deref().is_none_or(|p| p == url));
            let name = it.name.clone();
            (
                pre,
                name,
                self.clients().on(exit).for_page(url),
                self.key_service(&st),
            )
        };
        let entry = match pre {
            Some(entry) if !entry.deferred => entry,
            pre => {
                // A wrapper link the user handed back stands in for the
                // row's own link.
                let source = pre.as_ref().map_or(url, |p| p.url.as_str());
                let cfg = self.cfg();
                let cx = self.cx(&http, &cfg, &key_service);
                // A wrapper holds files, not a file: they join the queue, not
                // started, and the row keeps what it found.
                if hosts::lists(source) || self.lock().items.get(id).is_some_and(|i| i.wrapper) {
                    let found = or_cancel(cancel, hosts::unwrap(&cx, source)).await??;
                    self.take_unwrapped(id, source, found);
                    return Ok(Outcome::Unwrapped);
                }
                let mut entries = or_cancel(cancel, hosts::resolve(&cx, source)).await??;
                match (entries.len(), name) {
                    (0, _) => {
                        return Err(Fail::Error("nothing to download (empty folder?)".into()));
                    }
                    (1, _) => entries.remove(0),
                    // A row that came from this folder before: find its file again.
                    (_, Some(name)) => entries
                        .into_iter()
                        .find(|e| {
                            e.name.as_deref().map(hosts::name_key) == Some(hosts::name_key(&name))
                        })
                        .ok_or_else(|| Fail::Error(format!("{name} is no longer in the folder")))?,
                    (_, None) => {
                        self.expand(id, source, entries);
                        return Ok(Outcome::Gone);
                    }
                }
            }
        };
        {
            let mut st = self.lock();
            let it = st.items.get_mut(id).ok_or(Fail::Cancelled)?;
            it.learn(
                entry.name.as_deref(),
                false,
                entry.size,
                entry.checksum.clone(),
            );
        }
        if let Some(known) = self.known_file(id, cancel).await? {
            return Ok(known);
        }
        // A size learned only now may not fit next to what downloads already.
        let size_cap = self.size_cap();
        if size_cap > 0 {
            let st = self.lock();
            let size = st.items.get(id).map_or(0, |i| i.size);
            let others = running_size(&st, Some(id));
            if others > 0 && others + size > size_cap {
                return Ok(Outcome::Deferred);
            }
        }
        {
            let mut st = self.lock();
            let it = st.items.get_mut(id).ok_or(Fail::Cancelled)?;
            it.status = Status::Downloading;
            it.clear_error();
            self.touch(&mut st);
        }
        self.fetch(id, &entry, (host, exit), &http, cancel).await
    }

    fn finish(&self, id: &str, url: &str, host: &str, exit: Exit, result: Result<Outcome, Fail>) {
        let mut events = Vec::new();
        let mut removed = Vec::new();
        let mut disk_full = false;
        let complete = matches!(result, Ok(Outcome::Done | Outcome::Existing(_)));
        {
            let mut st = self.lock();
            let now = Instant::now();
            {
                let lane = st.lane(host, exit);
                lane.active = lane.active.saturating_sub(1);
            }
            log_outcome(id, url, &result);
            // An attempt that got data in before it stopped starts the counts
            // over: a flaky connection is not a dead mirror.
            if let Some(it) = st.items.get_mut(id)
                && it.done > it.attempt_from
            {
                it.retries = 0;
                it.busy = 0;
            }
            match result {
                Ok(Outcome::Gone | Outcome::Unwrapped) => {}
                Ok(Outcome::Deferred) => {
                    if let Some(it) = st.items.get_mut(id) {
                        it.status = Status::Queued;
                    }
                }
                Ok(Outcome::Existing(path)) => {
                    if let Some(it) = st.items.get_mut(id) {
                        it.adopt(&path);
                    }
                    // Its partial file, if any, is no longer needed.
                    removed.push(id.to_string());
                }
                Ok(Outcome::Done) => {
                    st.host(host).ok += 1;
                    if let Some(it) = st.items.get_mut(id) {
                        it.status = Status::Done;
                        it.reset_retries();
                        if it.size == 0 {
                            it.size = it.done;
                        }
                        events.push(Event::Finished {
                            id: id.to_string(),
                            name: display_name(it),
                        });
                        let (name, size) = (it.key_name(), it.size);
                        // Rows for the same file are no longer needed.
                        if let Some(name) = name {
                            let leftovers: Vec<String> = st
                                .items
                                .iter()
                                .filter(|(k, o)| {
                                    k.as_str() != id
                                        && o.status != Status::Done
                                        && !o.running()
                                        && o.same_file(&name, size)
                                })
                                .map(|(k, _)| k.clone())
                                .collect();
                            for k in &leftovers {
                                st.items.shift_remove(k);
                            }
                            removed.extend(leftovers);
                        }
                    }
                }
                Err(Fail::Cancelled) => {
                    if let Some(it) = st.items.get_mut(id)
                        && it.status != Status::Paused
                    {
                        it.status = Status::Queued;
                    }
                }
                Err(Fail::NeedsUser(need, message)) => {
                    let tentative = st.items.get(id).is_some_and(|i| i.tentative);
                    st.note_check(url, need, tentative);
                    // A copied page nobody passes for it asks no one: it goes.
                    if !(self.pages_pass() && st.passes(url)) {
                        drop_tentative_row(&mut st, id, "wants a person");
                    }
                    if let Some(it) = st.items.get_mut(id) {
                        drop_used_pre(it, url);
                        it.manual_urls.insert(url.to_string());
                        it.set_error(message, Trouble::Needs(need));
                        it.status = Status::Queued;
                    }
                }
                Err(Fail::Wait { seconds, message }) => {
                    let until = now + Duration::from_secs(seconds);
                    // The wait is asked of this address.
                    st.lane(host, exit).asked_to_wait(until);
                    if let Some(it) = st.items.get_mut(id) {
                        it.wait_until(until, message, Trouble::Other);
                    }
                    self.hold_passes(&mut st, now);
                }
                Err(Fail::Transient(message))
                    if st
                        .items
                        .get(id)
                        .is_some_and(|it| it.retries < self.cfg().resume_attempts) =>
                {
                    st.host(host).penalize(now);
                    let it = st.items.get_mut(id).expect("checked by the guard");
                    it.retries += 1;
                    let wait = (2u64 << (it.retries - 1).min(5)).min(60);
                    // The views count the wait down themselves.
                    it.wait_until(
                        now + Duration::from_secs(wait),
                        format!("{host}: {message}"),
                        Trouble::Other,
                    );
                }
                Err(Fail::Busy { allowed, .. })
                    if st.items.get(id).is_some_and(|it| it.busy < BUSY_REFUSALS) =>
                {
                    let lane = st.lane(host, exit);
                    lane.cap = Some(lane.cap.map_or(allowed, |cap| cap.min(allowed)));
                    lane.cap_since = Some(now);
                    let it = st.items.get_mut(id).expect("checked by the guard");
                    it.busy += 1;
                    it.wait_until(
                        now + Duration::from_secs(BUSY_PAUSE),
                        format!("{host} takes {allowed} at a time"),
                        Trouble::Other,
                    );
                }
                Err(
                    Fail::Error(message) | Fail::Transient(message) | Fail::Busy { message, .. },
                ) => {
                    // A copied page that fails is no download: it goes.
                    if !drop_tentative_row(&mut st, id, "failed")
                        && let Some(e) = self.fail_mirror(&mut st, id, url, host, message, false)
                    {
                        events.push(e);
                    }
                }
                Err(Fail::Local { message, full }) => {
                    if let Some(it) = st.items.get_mut(id) {
                        drop_used_pre(it, url);
                        it.set_error(format!("this computer: {message}"), Trouble::Other);
                        if full {
                            // Back in the queue, which pauses until there is room.
                            disk_full = true;
                            it.status = Status::Queued;
                        } else {
                            it.status = Status::Error;
                            self.plan_auto_retry(it, now);
                            events.push(Event::Failed {
                                id: id.to_string(),
                                name: display_name(it),
                                error: it.error.clone(),
                            });
                        }
                    }
                }
                Err(Fail::Gone(message)) => {
                    if !drop_tentative_row(&mut st, id, "is gone")
                        && let Some(e) = self.fail_mirror(&mut st, id, url, host, message, true)
                    {
                        events.push(e);
                    }
                }
            }
            match st.items.get_mut(id) {
                Some(it) => it.end_attempt(),
                // Removed or merged away while it ran.
                None => removed.push(id.to_string()),
            }
            if complete {
                self.extract_when_complete(&mut st, id);
            }
            self.touch(&mut st);
        }
        self.discard_parts(removed);
        if disk_full && !self.0.paused_now.load(Ordering::Relaxed) {
            tracing::warn!("the disk is full: pausing every download");
            self.set_paused(true);
            events.push(Event::Notice {
                title: "Downloads paused".into(),
                body: "The disk is full. Make room, then resume.".into(),
            });
        }
        self.emit(events);
        self.0.wake.notify_one();
    }

    /// Row `id`'s file, named now, as the queue or the download folder has it
    /// already: done or downloading in another row ([`Engine::dedupe`]), or
    /// on disk ([`Engine::existing_file`]).
    async fn known_file(
        &self,
        id: &str,
        cancel: &CancellationToken,
    ) -> Result<Option<Outcome>, Fail> {
        {
            let mut st = self.lock();
            if self.dedupe(&mut st, id) {
                st.items.shift_remove(id);
                self.touch(&mut st);
                drop(st);
                self.settle_parts();
                return Ok(Some(Outcome::Gone));
            }
        }
        self.settle_parts();
        let path = or_cancel(cancel, self.existing_file(id)).await?;
        Ok(path.map(Outcome::Existing))
    }

    /// Counts a failed attempt against its mirror (or, for the user's own
    /// link, asks for a new one); `gone`: the host says the file is no more,
    /// so the link is not tried again on a timer. Returns an event when no
    /// mirror is left.
    pub(super) fn fail_mirror(
        &self,
        st: &mut State,
        id: &str,
        url: &str,
        host: &str,
        message: String,
        gone: bool,
    ) -> Option<Event> {
        st.host(host).penalize(Instant::now());
        let it = st.items.get_mut(id)?;
        // Named next by the host it would try first, not this one.
        if it.host == host {
            it.host.clear();
        }
        let mut trouble = Trouble::Other;
        if gone {
            it.gone.insert(url.to_string());
            if hosts::is_paste(url) {
                trouble = Trouble::ListsNothing;
            }
        }
        it.set_error(format!("{host}: {message}"), trouble);
        // The next mirror gets its own retries.
        it.retries = 0;
        // A link handed back for another mirror stays that mirror's.
        let was_manual = it.manual && it.pre_for.as_deref().is_none_or(|p| p == url);
        drop_used_pre(it, url);
        if was_manual {
            // The user's link died or expired: ask again.
            it.manual_urls.insert(url.to_string());
        } else {
            it.failed.insert(url.to_string());
        }
        let left = it.failed.len() < it.mirrors.len();
        it.status = match (left, it.unstarted) {
            // Nobody started it: a lost mirror leaves it waiting for them.
            (true, true) => Status::Paused,
            (true, false) => Status::Queued,
            (false, _) => Status::Error,
        };
        if it.status == Status::Error && !it.unstarted {
            self.plan_auto_retry(it, Instant::now());
        }
        let it = &st.items[id];
        // Told only of a file the person started.
        (it.status == Status::Error && !it.unstarted && !noted_quietly(st, it)).then(|| Event::Failed {
            id: id.to_string(),
            name: display_name(it),
            error: it.error.clone(),
        })
    }

    /// Downloads row `id`'s file from `entry` into its partial file, going on
    /// from the bytes there (checking another host's against this one's
    /// first), and places it once whole.
    async fn fetch(
        &self,
        id: &str,
        entry: &Entry,
        (host, exit): (&str, Exit),
        http: &Http,
        cancel: &CancellationToken,
    ) -> Result<Outcome, Fail> {
        let part = self.part_path(id);
        tokio::fs::create_dir_all(part.parent().expect("part has a parent")).await?;
        let have = tokio::fs::metadata(&part)
            .await
            .map(|m| m.len())
            .unwrap_or(0);
        let (request, ask) = self.ask(id, entry, host, http, have)?;
        // Counted from the request to the end of the transfer.
        let _serving = Serving::start(self, host, exit);
        let resp = or_cancel(cancel, request.send()).await??;
        match self
            .answer(id, (host, exit), http, &part, &ask, resp)
            .await?
        {
            Answer::Complete => {}
            Answer::Body(body) => {
                if let Some(known) = self.stream(id, entry, host, &part, *body, cancel).await? {
                    return Ok(known);
                }
            }
        }
        let package = self
            .lock()
            .items
            .get(id)
            .map(|i| i.package.clone())
            .ok_or(Fail::Cancelled)?;
        let out_dir = self.out_dir(host, &package);
        tokio::fs::create_dir_all(&out_dir).await?;
        self.place(id, &part, &out_dir)?;
        Ok(Outcome::Done)
    }

    /// The request for row `id`'s file, `have` bytes of it on disk: from
    /// where they end, or, when they came from another host, from a little
    /// before, so this host's bytes can be compared with them.
    fn ask(
        &self,
        id: &str,
        entry: &Entry,
        host: &str,
        http: &Http,
        have: u64,
    ) -> Result<(reqwest::RequestBuilder, Ask), Fail> {
        let mut request = entry
            .headers
            .iter()
            .fold(http.client.get(&entry.url), |r, (k, v)| r.header(k, v))
            // The file as stored: its length and offsets are what resuming counts.
            .header(ACCEPT_ENCODING, "identity");
        let (part_host, part_size) = {
            let st = self.lock();
            let it = st.items.get(id).ok_or(Fail::Cancelled)?;
            (it.part_host.clone(), it.part_size)
        };
        let crossing = have > 0 && part_host.as_deref() != Some(host);
        let overlap = if crossing { have.min(OVERLAP) } else { 0 };
        let from = have - overlap;
        if have > 0 {
            request = request.header(RANGE, format!("bytes={from}-"));
        }
        Ok((
            request,
            Ask {
                have,
                from,
                overlap,
                crossing,
                part_size,
            },
        ))
    }

    /// What the host's answer to `ask` comes to: the file complete already, a
    /// body to read, or a refusal.
    async fn answer(
        &self,
        id: &str,
        (host, exit): (&str, Exit),
        http: &Http,
        part: &Path,
        ask: &Ask,
        resp: reqwest::Response,
    ) -> Result<Answer, Fail> {
        let Ask {
            have,
            from,
            crossing,
            part_size,
            overlap,
        } = *ask;
        let status = resp.status().as_u16();
        hosts::rate_limited(&resp)?;
        // The host's transfer allowance for this address is used up (MEGA's
        // free quota answers so): it comes back over time.
        if status == 509 {
            let seconds = hosts::retry_after(resp.headers(), 1800);
            return Err(Fail::Wait {
                seconds,
                message: "509 transfer allowance used up for now".into(),
            });
        }
        let known_size = self.lock().items.get(id).map(|i| i.size).unwrap_or(0);
        if status == 416 && crossing {
            // Only a size it states shows this is another file: a link that
            // has run out answers so too, and says nothing about these bytes.
            let total = hosts::unsatisfied_total(resp.headers());
            if let Some(total) = total.filter(|t| *t < have || (part_size > 0 && *t != part_size)) {
                let why = format!("has it at {total} bytes, not {part_size}");
                return Err(lost_partial(part, have, &why).await);
            }
            return Err(Fail::Error(
                "its link answers nothing past the start: run out, or not this file".into(),
            ));
        }
        if status == 416 && have > 0 {
            // Nothing left to send: the partial file is complete, but only
            // when its size is known to be that. A link that has run out
            // answers so to anything, and says nothing about these bytes.
            let stated = hosts::unsatisfied_total(resp.headers());
            if stated != Some(have) && known_size != have {
                let of = stated.or((known_size > 0).then_some(known_size));
                return Err(Fail::Error(match of {
                    Some(size) => format!("its link answers nothing past {have} of {size} bytes"),
                    None => format!(
                        "its link answers nothing past {have} bytes, and the file's size is not known"
                    ),
                }));
            }
            if let Some(it) = self.lock().items.get_mut(id) {
                it.done = have;
            }
            return Ok(Answer::Complete);
        }
        if status != 200 && status != 206 {
            // Other downloads from the host under way: a refusal then means it
            // takes no more at once.
            // Counted on this exit: a host limits one address.
            let others = self.lock().lane(host, exit).serving.saturating_sub(1);
            let mut note = "";
            if matches!(status, 403 | 451) && http.via_proxy && others == 0 {
                // Flag only: never switch to a direct connection on our own.
                self.lock().host(host).blocked = true;
                note = " (host may block VPN IPs)";
            }
            let message = match hosts::refusal_reason(resp).await {
                Some(reason) => format!("HTTP {status}: {reason}{note}"),
                None => format!("HTTP {status}{note}"),
            };
            if status == 403 && others > 0 {
                return Err(Fail::Busy {
                    allowed: others,
                    message,
                });
            }
            return Err(Fail::Error(message));
        }
        let name = self.lock().items.get(id).and_then(|i| i.name.clone());
        let wants_a_page = name.as_deref().is_some_and(|n| {
            Path::new(n).extension().is_some_and(|ext| {
                ext.eq_ignore_ascii_case("html") || ext.eq_ignore_ascii_case("htm")
            })
        });
        if hosts::content_type(resp.headers()).contains("text/html") && !wants_a_page {
            return Err(Fail::NeedsUser(
                Need::Person,
                "got a web page, not the file (link dead or needs a browser)".into(),
            ));
        }
        let range = hosts::content_range(resp.headers());
        if status == 206 && range.is_some_and(|(start, _)| start != from) {
            let _ = tokio::fs::remove_file(part).await;
            return Err(Fail::Error(
                "host resumed at the wrong offset; starting over".into(),
            ));
        }
        let (have, from) = if status == 200 {
            if crossing {
                tracing::info!(
                    item = id,
                    "{host} doesn't resume: starting over, without the {have} bytes from before"
                );
            }
            (0, 0)
        } else {
            (have, from)
        };
        let total = match (status, range) {
            (206, Some((_, Some(total)))) => Some(total),
            _ => hosts::content_length(resp.headers()).map(|len| from + len),
        };
        // The bytes here stay the other host's until this one's agree.
        let checking = crossing && have > 0;
        // The size it states, not one worked out from a short answer.
        let stated = range.and_then(|(_, total)| total);
        if checking && let Some(total) = stated.filter(|t| part_size > 0 && *t != part_size) {
            let why = format!("has it at {total} bytes, not {part_size}");
            return Err(lost_partial(part, have, &why).await);
        }
        let named_now = {
            let mut st = self.lock();
            let it = st.items.get_mut(id).ok_or(Fail::Cancelled)?;
            let unnamed = it.name.is_none();
            if unnamed {
                it.name = hosts::disposition_name(resp.headers()).map(|n| hosts::tidy_name(&n));
            }
            let named_now = unnamed && it.name.is_some();
            if !checking {
                if let Some(total) = total {
                    it.size = total;
                }
                it.part_size = total.unwrap_or(0);
                it.part_host = Some(host.to_string());
            }
            it.done = have;
            it.attempt_from = have;
            self.touch(&mut st);
            named_now
        };
        // What the partial file holds where this host's answer starts.
        let overlap = if checking {
            Overlap::new(read_tail(part, from, overlap).await?)
        } else {
            Overlap::new(Vec::new())
        };
        Ok(Answer::Body(Box::new(Body {
            resp,
            have,
            from,
            total,
            named_now,
            overlap,
        })))
    }

    /// Reads `body` into the partial file at `part`, comparing what overlaps
    /// the bytes there first. Some hosts (1fichier among them) name the file
    /// only as it starts: before the body comes, the same checks a name found
    /// by a probe or a resolver gets, and the file found already (done or
    /// downloading in another row, or in the download folder) is the
    /// outcome then.
    async fn stream(
        &self,
        id: &str,
        entry: &Entry,
        host: &str,
        part: &Path,
        body: Body,
        cancel: &CancellationToken,
    ) -> Result<Option<Outcome>, Fail> {
        let Body {
            mut resp,
            have,
            from,
            total,
            named_now,
            mut overlap,
        } = body;
        if named_now && let Some(known) = self.known_file(id, cancel).await? {
            return Ok(Some(known));
        }
        let file = tokio::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .append(have > 0)
            .truncate(have == 0)
            .open(part)
            .await?;
        let mut file = BufWriter::with_capacity(1 << 20, file);
        // The host's checksum, worked out as the bytes come when the file is
        // read from its start; a resumed one is read once more at its end.
        let checksum = self.lock().items.get(id).and_then(|i| i.checksum.clone());
        let mut hashing = checksum
            .as_ref()
            .filter(|_| have == 0)
            .map(|c| hosts::Hashing::new(c.algo));
        let mut differs = false;
        // MEGA's files come encrypted: one cipher for the transfer, at the
        // answer's start.
        let mut decrypting = entry.decrypt.as_ref().map(|key| key.decrypting(from));
        let mut done = have;
        let (mut mark, mut mark_bytes) = (Instant::now(), have);
        // The slowest it may be over each window, in bytes per second; 0
        // when that is off. A speed limit of our own turns it off, since
        // it would make every download look slow.
        let cfg = self.cfg();
        let floor = if cfg.rate_limit_mbps > 0.0 {
            0.0
        } else {
            f64::from(cfg.min_speed_kbps) * 1024.0
        };
        let window = Duration::from_secs(cfg.min_speed_seconds.max(1));
        let (mut window_at, mut window_bytes) = (Instant::now(), have);
        if let Some(it) = self.lock().items.get_mut(id) {
            it.pace = Some(Pace::new(Instant::now(), have));
        }
        // Counted here and told to the state a few times a second, not
        // for every chunk: the state's lock is everyone's.
        let (mut told_at, mut untold) = (Instant::now(), 0u64);
        // Never coarser than the speeds are judged at.
        let tell_every = TELL_EVERY.min(cfg.throttling.tick);
        let streamed: Result<(), Fail> = async {
            loop {
                // Woken at the window's end too, so a stalled connection
                // is judged as well.
                let judge_at = tokio::time::Instant::from_std(window_at + window);
                let chunk = tokio::select! {
                    biased;
                    _ = cancel.cancelled() => return Err(Fail::Cancelled),
                    c = resp.chunk() => Some(c.map_err(|e| Fail::Transient(hosts::error_text(e)))?),
                    _ = tokio::time::sleep_until(judge_at), if floor > 0.0 => None,
                };
                if floor > 0.0 && window_at.elapsed() >= window {
                    let speed = (done - window_bytes) as f64 / window_at.elapsed().as_secs_f64();
                    if speed < floor {
                        return Err(Fail::Error(format!(
                            // In the setting's own unit, KiB/s.
                            "slower than {} KiB/s for {}s ({:.0} KiB/s)",
                            cfg.min_speed_kbps,
                            window.as_secs(),
                            speed / 1024.0
                        )));
                    }
                    (window_at, window_bytes) = (Instant::now(), done);
                }
                let Some(chunk) = chunk else { continue };
                let Some(chunk) = chunk else { return Ok(()) };
                let chunk = match &mut decrypting {
                    Some(d) => d.chunk(chunk),
                    None => chunk,
                };
                let chunk = match overlap.take(chunk) {
                    Ok(Taken::Comparing) => continue,
                    Ok(Taken::Agreed(rest)) => {
                        // This host's bytes agree: the partial file is its.
                        if let Some(it) = self.lock().items.get_mut(id) {
                            if let Some(total) = total {
                                it.size = total;
                                it.part_size = total;
                            }
                            it.part_host = Some(host.to_string());
                        }
                        rest
                    }
                    Ok(Taken::Past(chunk)) => chunk,
                    Err(Differs) => {
                        differs = true;
                        return Ok(());
                    }
                };
                if chunk.is_empty() {
                    continue;
                }
                file.write_all(&chunk).await?;
                if let Some(h) = &mut hashing {
                    h.update(&chunk);
                }
                done += chunk.len() as u64;
                untold += chunk.len() as u64;
                self.0.limiter.throttle(chunk.len()).await;
                if told_at.elapsed() >= tell_every {
                    let bytes = std::mem::take(&mut untold);
                    self.tell_progress(id, host, done, bytes, &mut mark, &mut mark_bytes);
                    told_at = Instant::now();
                }
            }
        }
        .await;
        // What came since the last time, before anything reads `done`.
        self.tell_progress(id, host, done, untold, &mut mark, &mut mark_bytes);
        file.flush().await?;
        drop(file);
        if differs {
            return Err(lost_partial(part, have, "holds other bytes there").await);
        }
        streamed?;
        // Its answer ended before the bytes could be compared: nothing is
        // known about them, and they stay.
        if !overlap.finished() {
            return Err(Fail::Transient(format!(
                "its answer ended after {} of the {} bytes to compare",
                overlap.checked,
                overlap.tail.len()
            )));
        }
        // What this host said the file's size is, when its answer does
        // not: a body sent in chunks may end early without an error.
        let expected = total.or(entry.size.filter(|s| *s > 0));
        if let Some(want) = expected
            && done < want
        {
            return Err(Fail::Transient(format!(
                "connection closed at {done} of {want} bytes"
            )));
        }
        if let Some(checksum) = checksum {
            let same = match hashing {
                Some(h) => Some(h.finish() == checksum.hex),
                None => {
                    or_cancel(
                        cancel,
                        self.matches_checksum(id, part.to_path_buf(), checksum.clone()),
                    )
                    .await?
                }
            };
            if same == Some(false) {
                // Bytes that are not the file the host described: none of them
                // is kept, and this host's link is not tried again for it.
                let _ = tokio::fs::remove_file(part).await;
                return Err(Fail::Error(format!(
                    "the file differs from its {:?} checksum; its bytes were deleted",
                    checksum.algo
                )));
            }
        }
        Ok(None)
    }

    /// Tells the state how far row `id`'s transfer from `host` is: `done`
    /// bytes, `bytes` of them since it was last told. Its speed is measured
    /// over a second or more, from `mark`.
    fn tell_progress(
        &self,
        id: &str,
        host: &str,
        done: u64,
        bytes: u64,
        mark: &mut Instant,
        mark_bytes: &mut u64,
    ) {
        let mut st = self.lock();
        let elapsed = mark.elapsed().as_secs_f64();
        let speed = (elapsed >= 1.0).then(|| (done - *mark_bytes) as f64 / elapsed);
        if let Some(it) = st.items.get_mut(id) {
            it.done = done;
            if let Some(speed) = speed {
                it.speed = speed;
            }
        }
        if speed.is_some() {
            (*mark, *mark_bytes) = (Instant::now(), done);
        }
        let h = st.host(host);
        h.bytes += bytes;
        if let Some(speed) = speed {
            h.sample(speed);
        }
    }

    /// Where a finished file goes: the download folder, then the host's
    /// subfolder if it has one, then the package's folder. Decided when the
    /// file is complete, so a changed setting applies to running downloads.
    pub(super) fn out_dir(&self, host: &str, package: &str) -> PathBuf {
        let mut dir = self.download_dir();
        let folder = self.cfg().folder_for(host);
        if !folder.as_os_str().is_empty() {
            dir.push(folder);
        }
        if !package.is_empty() {
            dir.push(paths::safe(package));
        }
        dir
    }

    /// Moves a finished partial file to its final name, never overwriting: a
    /// different file by that name already there makes it `name (1).ext`.
    fn place(&self, id: &str, part: &Path, out_dir: &Path) -> Result<(), Fail> {
        let name = self
            .lock()
            .items
            .get(id)
            .and_then(|i| i.name.clone())
            .unwrap_or_else(|| format!("{id}.bin"));
        let _guard = crate::lock::held(&self.0.rename_lock);
        let target = paths::unused_name(out_dir, &paths::safe(&name));
        std::fs::rename(part, &target)?;
        if let Some(it) = self.lock().items.get_mut(id) {
            it.path = Some(target.display().to_string());
        }
        Ok(())
    }
}

/// What a request for a file was worked out from.
#[derive(Clone, Copy)]
struct Ask {
    /// Bytes of the partial file.
    have: u64,
    /// Where the request asks the host to start.
    from: u64,
    /// The bytes asked for again, to compare: up to [`OVERLAP`].
    overlap: u64,
    /// The partial file's bytes came from another host.
    crossing: bool,
    /// The size the host the bytes came from gave.
    part_size: u64,
}

/// What a host's answer to an [`Ask`] came to, when it is no refusal.
enum Answer {
    /// The partial file is the whole file already.
    Complete,
    Body(Box<Body>),
}

/// An answer with a body to read.
struct Body {
    resp: reqwest::Response,
    /// Bytes of the partial file this answer goes on from.
    have: u64,
    /// Where the answer starts in the file.
    from: u64,
    /// The file's size, when the answer says it.
    total: Option<u64>,
    /// The file got its name from this answer.
    named_now: bool,
    overlap: Overlap,
}

/// The bytes of the partial file where a host's answer starts, compared with
/// what the host sends before any of it is kept.
#[derive(Debug)]
struct Overlap {
    tail: Vec<u8>,
    checked: usize,
}

/// The answer holds other bytes than the partial file at the same place.
#[derive(Debug, PartialEq, Eq)]
struct Differs;

/// What a chunk of an answer came to, against the overlap.
#[derive(Debug, PartialEq, Eq)]
enum Taken {
    /// All of it agreed, and there is more to compare.
    Comparing,
    /// The comparison ended with it, agreeing: what follows the overlap.
    Agreed(Bytes),
    /// Past the overlap (or there was none): all of it is new.
    Past(Bytes),
}

impl Overlap {
    fn new(tail: Vec<u8>) -> Self {
        Self { tail, checked: 0 }
    }

    /// Every byte to compare came and agreed.
    fn finished(&self) -> bool {
        self.checked == self.tail.len()
    }

    fn take(&mut self, chunk: Bytes) -> Result<Taken, Differs> {
        if self.finished() {
            return Ok(Taken::Past(chunk));
        }
        let n = (self.tail.len() - self.checked).min(chunk.len());
        if chunk[..n] != self.tail[self.checked..self.checked + n] {
            return Err(Differs);
        }
        self.checked += n;
        Ok(if self.finished() {
            Taken::Agreed(chunk.slice(n..))
        } else {
            Taken::Comparing
        })
    }
}

/// The host holds another file than the partial file's, so the bytes can't be
/// mixed: they go, and the download starts over.
async fn lost_partial(part: &Path, have: u64, why: &str) -> Fail {
    let _ = tokio::fs::remove_file(part).await;
    Fail::Transient(format!(
        "{why}: a different file, so the {have} bytes from before are lost; starting over"
    ))
}

/// The `len` bytes of the file at `path` from `from` on.
async fn read_tail(path: &Path, from: u64, len: u64) -> std::io::Result<Vec<u8>> {
    let mut file = tokio::fs::File::open(path).await?;
    file.seek(std::io::SeekFrom::Start(from)).await?;
    let mut buf = vec![0; len as usize];
    file.read_exact(&mut buf).await?;
    Ok(buf)
}

fn log_outcome(id: &str, url: &str, result: &Result<Outcome, Fail>) {
    match result {
        Ok(Outcome::Done) => tracing::info!(item = id, url, "downloaded"),
        Ok(Outcome::Existing(path)) => {
            tracing::info!(item = id, url, "already downloaded: {}", path.display())
        }
        Ok(Outcome::Gone | Outcome::Deferred) | Err(Fail::Cancelled) => {}
        Ok(Outcome::Unwrapped) => tracing::info!(item = id, url, "unwrapped"),
        Err(Fail::Wait { seconds, message }) => {
            tracing::info!(item = id, url, seconds, "host asks to wait: {message}")
        }
        Err(Fail::NeedsUser(_, message)) => {
            tracing::info!(item = id, url, "needs a person: {message}")
        }
        Err(Fail::Transient(message)) => {
            tracing::warn!(item = id, url, "transfer broke: {message}")
        }
        Err(Fail::Busy { allowed, message }) => {
            tracing::info!(item = id, url, allowed, "host busy: {message}")
        }
        Err(Fail::Error(message)) => tracing::warn!(item = id, url, "attempt failed: {message}"),
        Err(Fail::Gone(message)) => tracing::info!(item = id, url, "gone: {message}"),
        Err(Fail::Local { message, .. }) => {
            tracing::warn!(item = id, url, "this computer: {message}")
        }
    }
}

pub(super) fn drop_used_pre(it: &mut Item, url: &str) {
    if it.pre.is_some() && it.pre_for.as_deref().is_none_or(|p| p == url) {
        it.pre = None;
        it.pre_for = None;
        it.manual = false;
    }
}

/// The full size of the files downloading now, but for row `except`: what
/// counts against `max_active_gb`.
pub(super) fn running_size(st: &State, except: Option<&str>) -> u64 {
    st.items
        .values()
        .filter(|i| i.running() && Some(i.id.as_str()) != except)
        .map(|i| i.size)
        .sum()
}

#[cfg(test)]
mod tests {
    use super::{Bytes, Differs, Fail, Overlap, Taken};

    /// The bytes an answer starts with are compared with the partial file's
    /// across chunks of any size; what follows them is kept.
    #[test]
    fn an_overlap_is_compared_across_chunks() {
        let mut o = Overlap::new(b"abcdef".to_vec());
        assert_eq!(o.take(Bytes::from_static(b"ab")), Ok(Taken::Comparing));
        assert_eq!(
            o.take(Bytes::from_static(b"cdefgh")),
            Ok(Taken::Agreed(Bytes::from_static(b"gh")))
        );
        assert!(o.finished());
        assert_eq!(
            o.take(Bytes::from_static(b"ij")),
            Ok(Taken::Past(Bytes::from_static(b"ij")))
        );
        let mut o = Overlap::new(b"abc".to_vec());
        assert_eq!(o.take(Bytes::from_static(b"abX")), Err(Differs));
        let mut none = Overlap::new(Vec::new());
        assert!(none.finished());
        assert_eq!(
            none.take(Bytes::from_static(b"x")),
            Ok(Taken::Past(Bytes::from_static(b"x")))
        );
        let mut short = Overlap::new(b"abcd".to_vec());
        assert_eq!(short.take(Bytes::from_static(b"ab")), Ok(Taken::Comparing));
        assert!(!short.finished(), "an answer that ends here proves nothing");
    }

    #[test]
    fn a_full_disk_is_told_apart_from_other_local_trouble() {
        let full = std::io::Error::from(std::io::ErrorKind::StorageFull);
        assert!(matches!(Fail::from(full), Fail::Local { full: true, .. }));
        let denied = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        assert!(matches!(
            Fail::from(denied),
            Fail::Local { full: false, .. }
        ));
    }
}
