//! Haul's log: what the app and the engine did, kept over time in the data
//! folder for a report to carry.
//!
//! - `haul.log` holds one JSON object per line (time, level, target,
//!   message and fields such as `item`, `url`, `host`), the file a person
//!   can send. It is started afresh at 10 MB, keeping the 5 files before it
//!   (`haul.1.log` the newest of them).
//! - Notable events are logged; with the detailed log on (a setting that
//!   applies at once) Haul's own debug events are too. `RUST_LOG`, when
//!   set, decides instead.
//! - Every download keeps its last [`HISTORY`] events, debug ones included
//!   whatever the setting, so a report shows how each one got where it is.

use std::{
    collections::{HashMap, VecDeque},
    fmt::Write as _,
    fs::File,
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{LazyLock, Mutex, OnceLock},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::Serialize;
use tracing::{
    Event, Subscriber,
    field::{Field, Visit},
};
use tracing_subscriber::{
    EnvFilter, Layer, Registry, filter::LevelFilter, fmt::MakeWriter, layer::SubscriberExt, reload,
    util::SubscriberInitExt,
};

/// Events each download keeps.
pub const HISTORY: usize = 40;
/// Size at which `haul.log` is set aside and started afresh.
const ROTATE_AT: u64 = 10 * 1024 * 1024;
/// Older logs kept beside it.
const KEEP: usize = 5;
/// Downloads with a history at most; the longest quiet one goes first.
const ITEMS: usize = 2000;

const NOTABLE: &str = "info";
const DETAILED: &str = "info,haul_core=debug,haul_web=debug,haul_desktop=debug";

/// Swaps the level the log file and the console write at.
type Reload = Box<dyn Fn(EnvFilter) + Send + Sync>;
static RELOAD: OnceLock<Reload> = OnceLock::new();
static FROM_ENV: OnceLock<bool> = OnceLock::new();

/// One event in a download's history.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Moment {
    /// ISO 8601, UTC, to the millisecond.
    pub at: String,
    pub level: String,
    pub message: String,
}

#[derive(Default)]
struct Histories {
    items: HashMap<String, VecDeque<Moment>>,
    /// Ids in the order they were last written to.
    recent: VecDeque<String>,
}

static HISTORIES: LazyLock<Mutex<Histories>> = LazyLock::new(Mutex::default);

/// The last events of download `id`, oldest first.
pub fn history(id: &str) -> Vec<Moment> {
    HISTORIES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .items
        .get(id)
        .map(|h| h.iter().cloned().collect())
        .unwrap_or_default()
}

fn remember(id: String, moment: Moment) {
    let mut h = HISTORIES.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(at) = h.recent.iter().position(|i| *i == id) {
        h.recent.remove(at);
    }
    h.recent.push_back(id.clone());
    let events = h.items.entry(id).or_default();
    events.push_back(moment);
    if events.len() > HISTORY {
        events.pop_front();
    }
    while h.recent.len() > ITEMS {
        if let Some(old) = h.recent.pop_front() {
            h.items.remove(&old);
        }
    }
}

/// Collects an event's message and its other fields, `item` apart.
#[derive(Default)]
struct Fields {
    message: String,
    item: Option<String>,
    rest: String,
}

impl Visit for Fields {
    fn record_str(&mut self, field: &Field, value: &str) {
        match field.name() {
            "message" => self.message = value.to_string(),
            "item" => self.item = Some(value.to_string()),
            name => {
                let _ = write!(self.rest, " {name}={value}");
            }
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        match field.name() {
            "message" => self.message = format!("{value:?}"),
            "item" => self.item = Some(format!("{value:?}").trim_matches('"').to_string()),
            name => {
                let _ = write!(self.rest, " {name}={value:?}");
            }
        }
    }
}

/// Keeps every event that names a download in that download's history.
struct HistoryLayer;

impl<S: Subscriber> Layer<S> for HistoryLayer {
    fn on_event(&self, event: &Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
        let mut fields = Fields::default();
        event.record(&mut fields);
        let Some(item) = fields.item else { return };
        remember(
            item,
            Moment {
                at: now_utc(),
                level: event.metadata().level().to_string().to_lowercase(),
                message: format!("{}{}", fields.message, fields.rest),
            },
        );
    }
}

fn now_utc() -> String {
    let d = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let (days, secs) = ((d.as_secs() / 86_400) as i64, d.as_secs() % 86_400);
    // Days since 1970-01-01 to a civil date (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        secs / 3_600,
        secs % 3_600 / 60,
        secs % 60,
        d.subsec_millis()
    )
}

/// `haul.log`, set aside for a fresh one past [`ROTATE_AT`].
struct Rotating {
    path: PathBuf,
    file: Option<File>,
    written: u64,
}

impl Rotating {
    fn open(path: PathBuf) -> io::Result<Self> {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        let written = file.metadata().map_or(0, |m| m.len());
        Ok(Self {
            path,
            file: Some(file),
            written,
        })
    }

    fn rotate(&mut self) {
        self.file = None;
        rotate_files(&self.path);
        self.file = File::create(&self.path).ok();
        self.written = 0;
    }
}

/// `haul.log` becomes `haul.1.log`, `haul.1.log` becomes `haul.2.log`…, and
/// the oldest beyond [`KEEP`] goes.
fn rotate_files(path: &Path) {
    let numbered = |n: usize| path.with_extension(format!("{n}.log"));
    let _ = std::fs::remove_file(numbered(KEEP));
    for n in (1..KEEP).rev() {
        let _ = std::fs::rename(numbered(n), numbered(n + 1));
    }
    let _ = std::fs::rename(path, numbered(1));
}

impl Write for Rotating {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.written + buf.len() as u64 > ROTATE_AT && self.written > 0 {
            self.rotate();
        }
        let Some(file) = self.file.as_mut() else {
            return Ok(buf.len());
        };
        let n = file.write(buf)?;
        self.written += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.as_mut().map_or(Ok(()), Write::flush)
    }
}

struct LogFile(Mutex<Rotating>);

struct Guard<'a>(std::sync::MutexGuard<'a, Rotating>);

impl Write for Guard<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

impl<'a> MakeWriter<'a> for LogFile {
    type Writer = Guard<'a>;

    fn make_writer(&'a self) -> Self::Writer {
        Guard(self.0.lock().unwrap_or_else(|e| e.into_inner()))
    }
}

/// The log file in `data_dir`: `haul.log`.
pub fn path(data_dir: &Path) -> PathBuf {
    data_dir.join("haul.log")
}

/// The logs set aside before it, newest first.
pub fn older(data_dir: &Path) -> Vec<PathBuf> {
    (1..=KEEP)
        .map(|n| data_dir.join(format!("haul.{n}.log")))
        .filter(|p| p.exists())
        .collect()
}

/// Starts logging: JSON Lines to `haul.log` in `data_dir`, readable text
/// to the console when `console` (a server in a terminal), and every
/// download's history. Once per process; later calls do nothing.
pub fn init(data_dir: &Path, console: bool) {
    let from_env = std::env::var_os("RUST_LOG").is_some();
    let _ = FROM_ENV.set(from_env);
    let filter = || EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(NOTABLE));
    let (shown, handle) = reload::Layer::<EnvFilter, Registry>::new(filter());
    let file = std::fs::create_dir_all(data_dir)
        .and_then(|()| Rotating::open(path(data_dir)))
        .ok()
        .map(|r| {
            tracing_subscriber::fmt::layer()
                .json()
                .flatten_event(true)
                .with_current_span(false)
                .with_span_list(false)
                .with_writer(LogFile(Mutex::new(r)))
        });
    let console = console.then(|| tracing_subscriber::fmt::layer().with_ansi(false));
    let history = HistoryLayer.with_filter(
        EnvFilter::new("haul_core=debug,haul_web=debug,haul_desktop=debug")
            .add_directive(LevelFilter::INFO.into()),
    );
    // The history sees debug events whatever the log writes; each layer
    // filters for itself, so neither limits the other.
    let started = tracing_subscriber::registry()
        .with(Layer::and_then(file, console).with_filter(shown))
        .with(history)
        .try_init();
    if started.is_ok() {
        let _ = RELOAD.set(Box::new(move |filter| {
            let _ = handle.reload(filter);
        }));
    }
}

/// Puts the detailed log on or off, unless `RUST_LOG` decides.
pub fn set_detailed(on: bool) {
    if FROM_ENV.get().copied().unwrap_or(false) {
        return;
    }
    if let Some(handle) = RELOAD.get() {
        handle(EnvFilter::new(if on { DETAILED } else { NOTABLE }));
        tracing::info!(
            detailed = on,
            "detailed log {}",
            if on { "on" } else { "off" }
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_download_keeps_its_last_events() {
        for n in 0..HISTORY + 5 {
            remember(
                "it".into(),
                Moment {
                    at: now_utc(),
                    level: "info".into(),
                    message: format!("event {n}"),
                },
            );
        }
        let h = history("it");
        assert_eq!(h.len(), HISTORY);
        assert_eq!(h[0].message, "event 5");
        assert!(history("nobody").is_empty());
        let at = now_utc();
        assert_eq!(at.len(), "2026-10-16T12:00:00.000Z".len(), "{at}");
    }

    #[test]
    fn old_logs_move_down_and_the_oldest_goes() {
        let dir = tempfile::tempdir().unwrap();
        let log = path(dir.path());
        for n in 0..=KEEP + 1 {
            std::fs::write(&log, format!("run {n}")).unwrap();
            rotate_files(&log);
        }
        let kept = older(dir.path());
        assert_eq!(kept.len(), KEEP);
        assert_eq!(
            std::fs::read_to_string(&kept[0]).unwrap(),
            format!("run {}", KEEP + 1)
        );
        let mut w = Rotating::open(log.clone()).unwrap();
        w.write_all(b"line\n").unwrap();
        w.written = ROTATE_AT;
        w.write_all(b"next\n").unwrap();
        assert_eq!(std::fs::read_to_string(&log).unwrap(), "next\n");
        assert_eq!(std::fs::read_to_string(&kept[0]).unwrap(), "line\n");
    }
}
