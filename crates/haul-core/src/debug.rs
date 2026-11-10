//! Debug mode: everything Haul does, as it happens, for an agent (or a
//! person) diagnosing or testing it, rather than a log read afterwards.
//!
//! Off unless asked for: the Live connection switch in Settings
//! (`live_debug`), `HAUL_DEBUG=1`, or the server's `--debug`, and it can be
//! switched on and off while Haul runs. On, a bus carries one JSON object
//! per signal, each with `t` (ISO 8601, UTC) and `kind`:
//!
//! - `log`: every event the app logs, at every level, with its fields;
//! - `status`: a download or wrapper changing status (`from`, `to`, its
//!   `error`);
//! - and whatever else the app emits with [`emit`] (the desktop app's page
//!   passes, the notifications it shows).
//!
//! Every signal but `log` goes to `haul.log` too, debug mode or not, so a
//! report holds the same story (target `haul_core::signal`).
//!
//! The web server streams the bus and answers questions about Haul's
//! insides (`/api/debug/…`), to a client on this machine that sends the
//! token written to `debug-token` in the data folder: the bus shows page
//! contents and links, and can run code in a page being passed.

use std::{
    collections::{HashMap, VecDeque},
    path::{Path, PathBuf},
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
};

use serde_json::{Map, Value, json};
use tokio::sync::broadcast;

use crate::Engine;
pub use crate::logging::now;

/// The target the log writes signals under.
pub const SIGNAL_TARGET: &str = "haul_core::signal";

/// Signals kept for a slow reader before it misses some (it is told).
const BACKLOG: usize = 8192;
/// The last signals kept for a reader that comes later ([`Bus::recent`]).
const RECENT: usize = 2000;

pub struct Bus {
    tx: broadcast::Sender<Value>,
    recent: Mutex<VecDeque<Value>>,
    on: AtomicBool,
    /// Asked for by the environment or the command line: on for the whole
    /// run, whatever the settings say.
    forced: AtomicBool,
    /// What a debug client sends in `X-Haul-Debug`, while on.
    token: Mutex<String>,
    data_dir: PathBuf,
}

static BUS: OnceLock<Bus> = OnceLock::new();

/// Whether the environment asks for debug mode (`HAUL_DEBUG`, set and not
/// `0`).
pub fn asked() -> bool {
    std::env::var_os("HAUL_DEBUG").is_some_and(|v| !v.is_empty() && v != "0")
}

/// Sets the bus up, off, for `data_dir`. Once per process; later calls
/// change nothing.
pub fn init(data_dir: &Path) -> &'static Bus {
    BUS.get_or_init(|| {
        let (tx, _) = broadcast::channel(BACKLOG);
        // A token left by a run that ended without taking it away.
        let _ = std::fs::remove_file(token_path(data_dir));
        Bus {
            tx,
            recent: Mutex::new(VecDeque::with_capacity(RECENT)),
            on: AtomicBool::new(false),
            forced: AtomicBool::new(false),
            token: Mutex::new(String::new()),
            data_dir: data_dir.to_path_buf(),
        }
    })
}

/// Turns debug mode on for the whole run, whatever the settings say: the
/// bus, and a fresh token in `debug-token` in `data_dir`.
pub fn enable(data_dir: &Path) -> &'static Bus {
    let bus = init(data_dir);
    bus.forced.store(true, Ordering::Relaxed);
    bus.set_on(true);
    bus
}

/// Puts debug mode on or off, as the Live connection setting says, unless
/// it was asked for at the start. On writes a fresh token; off takes it
/// away, and what was kept with it.
pub fn set_live(on: bool) {
    if let Some(bus) = BUS.get()
        && !bus.forced.load(Ordering::Relaxed)
    {
        bus.set_on(on);
    }
}

/// The file the token is written to.
pub fn token_path(data_dir: &Path) -> PathBuf {
    data_dir.join("debug-token")
}

/// The bus, while debug mode is on.
pub fn bus() -> Option<&'static Bus> {
    BUS.get().filter(|b| b.on.load(Ordering::Relaxed))
}

pub fn enabled() -> bool {
    bus().is_some()
}

impl Bus {
    pub fn subscribe(&self) -> broadcast::Receiver<Value> {
        self.tx.subscribe()
    }

    /// The last signals sent, oldest first.
    pub fn recent(&self) -> Vec<Value> {
        crate::lock::held(&self.recent).iter().cloned().collect()
    }

    /// Whether `sent` is the token.
    pub fn accepts(&self, sent: Option<&str>) -> bool {
        let token = crate::lock::held(&self.token);
        !token.is_empty() && sent == Some(token.as_str())
    }

    fn set_on(&self, on: bool) {
        let mut token = crate::lock::held(&self.token);
        if on == self.on.load(Ordering::Relaxed) {
            return;
        }
        let path = token_path(&self.data_dir);
        if on {
            *token = uuid::Uuid::new_v4().simple().to_string();
            let _ = std::fs::create_dir_all(&self.data_dir);
            let _ = std::fs::write(&path, token.as_str());
        } else {
            token.clear();
            let _ = std::fs::remove_file(&path);
            crate::lock::held(&self.recent).clear();
        }
        self.on.store(on, Ordering::Relaxed);
        drop(token);
        tracing::info!(on, "live connection {}", if on { "on" } else { "off" });
    }
}

/// Sends a signal of `kind` with `fields` (an object) on the bus, when
/// debug mode is on. Every kind but `log` goes to the log too, whatever the
/// mode: a pass's every page message at the debug level, the rest at info.
pub fn emit(kind: &str, fields: Value) {
    let signal = signal_of(kind, fields);
    if kind != "log" {
        log_signal(kind, &signal);
    }
    send(signal);
}

/// Signal `kind` with `fields`, stamped now.
fn signal_of(kind: &str, fields: Value) -> Value {
    let mut signal = Map::new();
    signal.insert("t".into(), json!(now()));
    signal.insert("kind".into(), json!(kind));
    if let Value::Object(fields) = fields {
        signal.extend(fields);
    }
    Value::Object(signal)
}

/// Puts `signal` on the bus, when debug mode is on.
fn send(signal: Value) {
    let Some(bus) = bus() else { return };
    {
        let mut recent = crate::lock::held(&bus.recent);
        if recent.len() == RECENT {
            recent.pop_front();
        }
        recent.push_back(signal.clone());
    }
    let _ = bus.tx.send(signal);
}

/// Writes signal `s` of `kind` to the log, in a line of its own words and
/// whole as `signal`.
fn log_signal(kind: &str, s: &Value) {
    let text = |k: &str| s.get(k).and_then(Value::as_str).unwrap_or_default();
    let mut whole = s.clone();
    if let Some(o) = whole.as_object_mut() {
        o.remove("t");
    }
    let whole = whole.to_string();
    let item = text("id");
    match (kind, text("event")) {
        ("status", _) => {
            let from = s.get("from").and_then(Value::as_str).unwrap_or("new");
            tracing::info!(target: SIGNAL_TARGET, item, signal = %whole, "{}: {from} -> {}", text("name"), text("to"));
        }
        ("pass", "page") => {
            tracing::debug!(target: SIGNAL_TARGET, signal = %whole, "pass: the page says {}", s["said"]);
        }
        ("pass", event) => {
            tracing::info!(target: SIGNAL_TARGET, item, signal = %whole, "pass {event}: {}", text("url"));
        }
        ("page", "closed") => {
            tracing::info!(target: SIGNAL_TARGET, item, signal = %whole, "page window closed");
        }
        ("page", event) => {
            tracing::info!(target: SIGNAL_TARGET, item, signal = %whole, "page window {event}: {}", text("url"));
        }
        ("notification", _) => {
            tracing::info!(target: SIGNAL_TARGET, signal = %whole, "notification: {}: {}", text("title"), text("body"));
        }
        (kind, _) => {
            tracing::info!(target: SIGNAL_TARGET, signal = %whole, "{kind}");
        }
    }
}

/// Emits a `status` signal whenever a download or wrapper changes status,
/// by comparing each state the engine publishes with the one before: rows
/// added (`from` null) and taken off the list (`to: "removed"`) too. The
/// rows already there as it starts are one line in the log, how many in
/// each state, rather than a line each; the bus gets each of them.
pub fn watch_statuses(engine: Engine) {
    let mut changes = engine.watch();
    tokio::spawn(async move {
        let mut before: HashMap<String, (&'static str, String)> = HashMap::new();
        let mut first = true;
        loop {
            let found = engine.row_changes(&mut before);
            let mut tally: std::collections::BTreeMap<&str, usize> = Default::default();
            for (row, was) in found.changed {
                let signal = json!({ "row": row.row, "id": row.id, "name": row.name, "from": was, "to": row.state, "error": row.error });
                if first {
                    *tally.entry(row.state).or_default() += 1;
                    send(signal_of("status", signal));
                } else {
                    emit("status", signal);
                }
            }
            if first {
                let rows: usize = tally.values().sum();
                tracing::info!(target: SIGNAL_TARGET, rows, by_state = %json!(tally), "{rows} rows at start");
            }
            for (id, name, status) in found.removed {
                emit(
                    "status",
                    json!({ "id": id, "name": name, "from": status, "to": "removed" }),
                );
            }
            first = false;
            if changes.changed().await.is_err() {
                return;
            }
        }
    });
}

/// Whether `signal` holds every field of `want`, compared as JSON (an
/// object in `want` matches the fields it names; a string matches as a
/// substring of a string), for `/api/debug/wait`.
pub fn matches(signal: &Value, want: &Value) -> bool {
    match (signal, want) {
        (Value::Object(have), Value::Object(want)) => want
            .iter()
            .all(|(k, w)| have.get(k).is_some_and(|h| matches(h, w))),
        (Value::String(have), Value::String(want)) => have.contains(want.as_str()),
        (have, want) => have == want,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wait_matches_the_fields_it_names() {
        let signal = json!({"kind": "status", "to": "done", "name": "a.part1.rar", "n": 3});
        assert!(matches(&signal, &json!({"kind": "status", "to": "done"})));
        assert!(matches(&signal, &json!({"name": "part1"})), "a substring");
        assert!(matches(&signal, &json!({"n": 3})));
        assert!(!matches(&signal, &json!({"to": "error"})));
        assert!(!matches(&signal, &json!({"missing": 1})));
    }
}
