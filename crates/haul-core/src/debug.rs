//! Debug mode: everything Haul does, as it happens, for an agent (or a
//! person) diagnosing or testing it, rather than a log read afterwards.
//!
//! Off unless asked for (`HAUL_DEBUG=1`, or the server's `--debug`). On, a
//! bus carries one JSON object per signal, each with `t` (ISO 8601, UTC)
//! and `kind`:
//!
//! - `log`: every event the app logs, at every level, with its fields;
//! - `status`: a download or wrapper changing status (`from`, `to`, its
//!   `error`);
//! - and whatever else the app emits with [`emit`] (the desktop app's page
//!   passes, the notifications it shows).
//!
//! The web server streams the bus and answers questions about Haul's
//! insides (`/api/debug/…`), to a client on this machine that sends the
//! token written to `debug-token` in the data folder: the bus shows page
//! contents and links, and can run code in a page being passed.

use std::{
    collections::{HashMap, VecDeque},
    path::Path,
    sync::{Mutex, OnceLock},
};

use serde_json::{Map, Value, json};
use tokio::sync::broadcast;

use crate::Engine;

/// Signals kept for a slow reader before it misses some (it is told).
const BACKLOG: usize = 8192;
/// The last signals kept for a reader that comes later ([`Bus::recent`]).
const RECENT: usize = 2000;

pub struct Bus {
    tx: broadcast::Sender<Value>,
    recent: Mutex<VecDeque<Value>>,
    /// What a debug client sends in `X-Haul-Debug`.
    pub token: String,
}

static BUS: OnceLock<Bus> = OnceLock::new();

/// Whether the environment asks for debug mode (`HAUL_DEBUG`, set and not
/// `0`).
pub fn asked() -> bool {
    std::env::var_os("HAUL_DEBUG").is_some_and(|v| !v.is_empty() && v != "0")
}

/// Turns debug mode on: the bus, and a fresh token in `debug-token` in
/// `data_dir`. Once per process; later calls change nothing.
pub fn enable(data_dir: &Path) -> &'static Bus {
    BUS.get_or_init(|| {
        let token = uuid::Uuid::new_v4().simple().to_string();
        let _ = std::fs::create_dir_all(data_dir);
        let _ = std::fs::write(token_path(data_dir), &token);
        let (tx, _) = broadcast::channel(BACKLOG);
        Bus {
            tx,
            recent: Mutex::new(VecDeque::with_capacity(RECENT)),
            token,
        }
    })
}

/// The file the token is written to.
pub fn token_path(data_dir: &Path) -> std::path::PathBuf {
    data_dir.join("debug-token")
}

/// The bus, while debug mode is on.
pub fn bus() -> Option<&'static Bus> {
    BUS.get()
}

pub fn enabled() -> bool {
    BUS.get().is_some()
}

/// Now, as signals and the log write it: ISO 8601, UTC, to the millisecond.
pub fn now() -> String {
    crate::logging::now()
}

impl Bus {
    pub fn subscribe(&self) -> broadcast::Receiver<Value> {
        self.tx.subscribe()
    }

    /// The last signals sent, oldest first.
    pub fn recent(&self) -> Vec<Value> {
        self.recent
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .cloned()
            .collect()
    }
}

/// Sends a signal of `kind` with `fields` (an object) on the bus, when
/// debug mode is on.
pub fn emit(kind: &str, fields: Value) {
    let Some(bus) = BUS.get() else { return };
    let mut signal = Map::new();
    signal.insert("t".into(), json!(crate::logging::now()));
    signal.insert("kind".into(), json!(kind));
    if let Value::Object(fields) = fields {
        signal.extend(fields);
    }
    let signal = Value::Object(signal);
    {
        let mut recent = bus.recent.lock().unwrap_or_else(|e| e.into_inner());
        if recent.len() == RECENT {
            recent.pop_front();
        }
        recent.push_back(signal.clone());
    }
    let _ = bus.tx.send(signal);
}

/// Emits a `status` signal whenever a download or wrapper changes status,
/// by comparing each state the engine publishes with the one before.
pub fn watch_statuses(engine: Engine) {
    if !enabled() {
        return;
    }
    let mut changes = engine.watch();
    tokio::spawn(async move {
        let mut before: HashMap<String, (&'static str, String)> = HashMap::new();
        loop {
            let s = engine.snapshot();
            let mut now = HashMap::new();
            let rows = s
                .items
                .iter()
                .map(|i| ("item", &i.id, i.name.as_str(), i.status, &i.error))
                .chain(s.wrappers.iter().map(|w| {
                    (
                        "wrapper",
                        &w.id,
                        w.title.as_deref().unwrap_or(&w.url),
                        w.status,
                        &w.error,
                    )
                }));
            for (kind, id, name, status, error) in rows {
                let was = before.get(id.as_str()).map(|(s, _)| *s);
                if was != Some(status) {
                    emit(
                        "status",
                        json!({ "row": kind, "id": id, "name": name, "from": was, "to": status, "error": error }),
                    );
                }
                now.insert(id.clone(), (status, error.clone()));
            }
            for (id, (status, _)) in &before {
                if !now.contains_key(id) {
                    emit(
                        "status",
                        json!({ "id": id, "from": status, "to": "removed" }),
                    );
                }
            }
            before = now;
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
