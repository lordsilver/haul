//! One attempt at a download, from its turn in the queue to its end: the
//! link resolved, the bytes fetched into the partial file, the file placed,
//! and `finish`, which settles what the attempt's outcome means for the row.

use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use reqwest::header::{ACCEPT_ENCODING, RANGE};
use tokio::io::{AsyncWriteExt, BufWriter};
use tokio_util::sync::CancellationToken;

use super::{
    Engine, Event, Exit, SETTLE, State, display_name, drop_tentative_row, noted_quietly, unix_now,
};
use crate::{
    hosts::{self, Cx, Entry, Http, ResolveError},
    item::{Item, Status},
    paths,
};

/// Refusals in a row from a busy host before a download counts as failed.
const BUSY_REFUSALS: u32 = 10;

/// Seconds a download refused by a busy host waits before it may try again
/// (and only then if the host has a free slot).
const BUSY_PAUSE: u64 = 5;

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

enum Fail {
    Cancelled,
    Wait {
        seconds: u64,
        message: String,
    },
    NeedsUser(String),
    Error(String),
    /// The connection broke mid-transfer; worth resuming on the same mirror.
    Transient(String),
    /// The host refused one more download while `allowed` others from it ran.
    Busy {
        allowed: usize,
        message: String,
    },
}

impl From<ResolveError> for Fail {
    fn from(e: ResolveError) -> Self {
        match e {
            ResolveError::Wait { seconds, message } => Fail::Wait { seconds, message },
            ResolveError::NeedsUser(m) => Fail::NeedsUser(m),
            ResolveError::Network(m) => Fail::Transient(m),
            ResolveError::Failed(m) => Fail::Error(m),
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
        Fail::Error(e.to_string())
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
                let cx = Cx {
                    http: &http,
                    cfg: &cfg,
                    tokens: &self.0.tokens,
                    key_service: &key_service,
                };
                // A wrapper holds files, not a file: they join the queue, not
                // started, and the row keeps what it found.
                if hosts::lists(source) || self.lock().items.get(id).is_some_and(|i| i.wrapper) {
                    let found = or_cancel(cancel, hosts::unwrap(&cx, source)).await??;
                    for id in self.unwrapped(id, source, found) {
                        self.spawn_probe(id);
                    }
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
            if it.name.is_none() {
                it.name = entry.name.as_deref().map(hosts::tidy_name);
            }
            if let Some(size) = entry.size.filter(|s| *s > 0) {
                it.size = size;
            }
            if it.checksum.is_none() {
                it.checksum = entry.checksum.clone();
            }
            if self.dedupe(&mut st, id) {
                st.items.shift_remove(id);
                self.touch(&mut st);
                return Ok(Outcome::Gone);
            }
        }
        if let Some(path) = or_cancel(cancel, self.existing_file(id)).await? {
            return Ok(Outcome::Existing(path));
        }
        // A size learned only now may not fit next to what downloads already.
        let size_cap = self.size_cap();
        if size_cap > 0 {
            let st = self.lock();
            let size = st.items.get(id).map_or(0, |i| i.size);
            let others: u64 = st
                .items
                .values()
                .filter(|i| i.running() && i.id != id)
                .map(|i| i.size)
                .sum();
            if others > 0 && others + size > size_cap {
                return Ok(Outcome::Deferred);
            }
        }
        {
            let mut st = self.lock();
            let it = st.items.get_mut(id).ok_or(Fail::Cancelled)?;
            it.status = Status::Downloading;
            it.error.clear();
            self.touch(&mut st);
        }
        self.fetch(id, &entry, (host, exit), &http, cancel).await
    }

    fn finish(&self, id: &str, url: &str, host: &str, exit: Exit, result: Result<Outcome, Fail>) {
        let mut events = Vec::new();
        let mut removed = Vec::new();
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
                Err(Fail::NeedsUser(message)) => {
                    st.note_check(url, &message);
                    // A copied page nobody passes for it asks no one: it goes.
                    if !(self.pages_pass() && st.passes(url)) {
                        drop_tentative_row(&mut st, id, "wants a person");
                    }
                    if let Some(it) = st.items.get_mut(id) {
                        drop_used_pre(it, url);
                        it.manual_urls.insert(url.to_string());
                        it.error = message;
                        it.status = Status::Queued;
                    }
                }
                Err(Fail::Wait { seconds, message }) => {
                    let until = now + Duration::from_secs(seconds);
                    // The wait is asked of this address.
                    st.lane(host, exit).cooldown = Some(until);
                    if let Some(it) = st.items.get_mut(id) {
                        it.status = Status::Waiting;
                        it.not_before = Some(until);
                        it.error = message;
                    }
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
                    it.status = Status::Waiting;
                    it.not_before = Some(now + Duration::from_secs(wait));
                    // The views count the wait down themselves.
                    it.error = format!("{host}: {message}");
                }
                Err(Fail::Busy { allowed, .. })
                    if st.items.get(id).is_some_and(|it| it.busy < BUSY_REFUSALS) =>
                {
                    let lane = st.lane(host, exit);
                    lane.cap = Some(lane.cap.map_or(allowed, |cap| cap.min(allowed)));
                    lane.cap_since = Some(now);
                    let it = st.items.get_mut(id).expect("checked by the guard");
                    it.busy += 1;
                    it.status = Status::Waiting;
                    it.not_before = Some(now + Duration::from_secs(BUSY_PAUSE));
                    it.error = format!("{host} takes {allowed} at a time");
                }
                Err(
                    Fail::Error(message) | Fail::Transient(message) | Fail::Busy { message, .. },
                ) => {
                    // A copied page that fails is no download: it goes.
                    if !drop_tentative_row(&mut st, id, "failed")
                        && let Some(e) = self.fail_mirror(&mut st, id, url, host, message)
                    {
                        events.push(e);
                    }
                }
            }
            match st.items.get_mut(id) {
                Some(it) => {
                    it.speed = 0.0;
                    it.task = None;
                    it.exit = None;
                }
                // Removed or merged away while it ran.
                None => removed.push(id.to_string()),
            }
            if complete {
                self.extract_when_complete(&mut st, id);
            }
            self.touch(&mut st);
        }
        self.discard_parts(removed);
        for e in events {
            let _ = self.0.events.send(e);
        }
        self.0.wake.notify_one();
    }

    /// Counts a failed attempt against its mirror (or, for the user's own
    /// link, asks for a new one). Returns an event when no mirror is left.
    pub(super) fn fail_mirror(
        &self,
        st: &mut State,
        id: &str,
        url: &str,
        host: &str,
        message: String,
    ) -> Option<Event> {
        st.host(host).penalize(Instant::now());
        let it = st.items.get_mut(id)?;
        it.error = format!("{host}: {message}");
        // The next mirror gets its own retries.
        it.retries = 0;
        let was_manual = it.manual;
        drop_used_pre(it, url);
        if was_manual {
            // The user's link died or expired: ask again.
            it.manual_urls.insert(url.to_string());
        } else {
            it.failed.insert(url.to_string());
        }
        it.status = if it.failed.len() < it.mirrors.len() {
            Status::Queued
        } else {
            Status::Error
        };
        if it.status == Status::Error {
            self.plan_auto_retry(it, Instant::now());
        }
        let it = &st.items[id];
        (it.status == Status::Error && !noted_quietly(st, it)).then(|| Event::Failed {
            id: id.to_string(),
            name: display_name(it),
            error: it.error.clone(),
        })
    }

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
        let mut have = tokio::fs::metadata(&part)
            .await
            .map(|m| m.len())
            .unwrap_or(0);

        let mut req = entry
            .headers
            .iter()
            .fold(http.client.get(&entry.url), |r, (k, v)| r.header(k, v))
            // The file as stored: its length and offsets are what resuming counts.
            .header(ACCEPT_ENCODING, "identity");
        if have > 0 {
            req = req.header(RANGE, format!("bytes={have}-"));
        }
        // Counted from the request to the end of the transfer.
        let _serving = Serving::start(self, host, exit);
        let mut resp = or_cancel(cancel, req.send()).await??;
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
        if status == 416 && have > 0 {
            // Nothing left to send: the partial file is complete, unless we
            // know it should be longer.
            if known_size > 0 && have < known_size {
                let _ = tokio::fs::remove_file(&part).await;
                return Err(Fail::Error(format!(
                    "HTTP 416 at {have} of {known_size} bytes; starting over"
                )));
            }
            if let Some(it) = self.lock().items.get_mut(id) {
                it.done = have;
            }
        } else if status != 200 && status != 206 {
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
        } else {
            let name = self.lock().items.get(id).and_then(|i| i.name.clone());
            let wants_a_page = name.as_deref().is_some_and(|n| {
                Path::new(n).extension().is_some_and(|ext| {
                    ext.eq_ignore_ascii_case("html") || ext.eq_ignore_ascii_case("htm")
                })
            });
            if hosts::content_type(resp.headers()).contains("text/html") && !wants_a_page {
                return Err(Fail::NeedsUser(
                    "got a web page, not the file (link dead or needs a browser)".into(),
                ));
            }
            let range = hosts::content_range(resp.headers());
            if status == 206 && range.is_some_and(|(start, _)| start != have) {
                let _ = tokio::fs::remove_file(&part).await;
                return Err(Fail::Error(
                    "host resumed at the wrong offset; starting over".into(),
                ));
            }
            if status == 200 {
                have = 0;
            }
            let total = match (status, range) {
                (206, Some((_, Some(total)))) => Some(total),
                _ => hosts::content_length(resp.headers()).map(|len| have + len),
            };
            let named_now = {
                let mut st = self.lock();
                let it = st.items.get_mut(id).ok_or(Fail::Cancelled)?;
                let unnamed = it.name.is_none();
                if unnamed {
                    it.name = hosts::disposition_name(resp.headers()).map(|n| hosts::tidy_name(&n));
                }
                let named_now = unnamed && it.name.is_some();
                if let Some(total) = total {
                    it.size = total;
                }
                it.done = have;
                it.attempt_from = have;
                self.touch(&mut st);
                named_now
            };
            // Some hosts (1fichier among them) name the file only as it
            // starts: before the body comes, the same checks a name found by
            // a probe or a resolver gets. The file may be done or downloading
            // in another row, or be in the download folder already.
            if named_now {
                {
                    let mut st = self.lock();
                    if self.dedupe(&mut st, id) {
                        st.items.shift_remove(id);
                        self.touch(&mut st);
                        return Ok(Outcome::Gone);
                    }
                }
                if let Some(path) = or_cancel(cancel, self.existing_file(id)).await? {
                    return Ok(Outcome::Existing(path));
                }
            }
            let file = tokio::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .append(have > 0)
                .truncate(have == 0)
                .open(&part)
                .await?;
            let mut file = BufWriter::with_capacity(1 << 20, file);
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
                                "slower than {} KB/s for {}s ({:.0} KB/s)",
                                cfg.min_speed_kbps,
                                window.as_secs(),
                                speed / 1024.0
                            )));
                        }
                        (window_at, window_bytes) = (Instant::now(), done);
                    }
                    let Some(chunk) = chunk else { continue };
                    let Some(chunk) = chunk else { return Ok(()) };
                    let chunk = match &entry.decrypt {
                        Some(key) => {
                            let mut plain = chunk.to_vec();
                            key.apply(&mut plain, done);
                            plain.into()
                        }
                        None => chunk,
                    };
                    file.write_all(&chunk).await?;
                    done += chunk.len() as u64;
                    self.0.limiter.throttle(chunk.len()).await;
                    let mut st = self.lock();
                    if let Some(it) = st.items.get_mut(id) {
                        it.done = done;
                    }
                    let elapsed = mark.elapsed().as_secs_f64();
                    let speed = (elapsed >= 1.0).then(|| (done - mark_bytes) as f64 / elapsed);
                    if let Some(speed) = speed {
                        if let Some(it) = st.items.get_mut(id) {
                            it.speed = speed;
                        }
                        (mark, mark_bytes) = (Instant::now(), done);
                    }
                    let h = st.host(host);
                    h.bytes += chunk.len() as u64;
                    if let Some(speed) = speed {
                        h.ema = if h.ema == 0.0 {
                            speed
                        } else {
                            h.ema * 0.7 + speed * 0.3
                        };
                        h.measured = unix_now();
                    }
                }
            }
            .await;
            file.flush().await?;
            drop(file);
            streamed?;
            if let Some(total) = total
                && done < total
            {
                return Err(Fail::Transient(format!(
                    "connection closed at {done} of {total} bytes"
                )));
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
        let _guard = self.0.rename_lock.lock().unwrap_or_else(|e| e.into_inner());
        let target = paths::unused_name(out_dir, &paths::safe(&name));
        std::fs::rename(part, &target)?;
        if let Some(it) = self.lock().items.get_mut(id) {
            it.path = Some(target.display().to_string());
        }
        Ok(())
    }
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
        Err(Fail::NeedsUser(message)) => {
            tracing::info!(item = id, url, "needs a person: {message}")
        }
        Err(Fail::Transient(message)) => {
            tracing::warn!(item = id, url, "transfer broke: {message}")
        }
        Err(Fail::Busy { allowed, message }) => {
            tracing::info!(item = id, url, allowed, "host busy: {message}")
        }
        Err(Fail::Error(message)) => tracing::warn!(item = id, url, "attempt failed: {message}"),
    }
}

fn drop_used_pre(it: &mut Item, url: &str) {
    if it.pre.is_some() && it.pre_for.as_deref().is_none_or(|p| p == url) {
        it.pre = None;
        it.pre_for = None;
        it.manual = false;
    }
}
