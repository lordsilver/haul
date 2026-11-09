//! A report of what Haul is doing and did, for the user to save and send
//! along with a problem, so what happened can be read rather than guessed.
//! Laid out for whoever diagnoses it (an agent, mostly):
//!
//! - `timeline.txt`: every event of the logs, oldest first, one line each,
//!   its row named, each run of Haul under a line saying its version;
//! - `report.json`: Haul and its system, the settings without secrets, what
//!   the UI shows (`view`), what the engine holds beyond that (`engine`: each
//!   row's links and what became of each, its timers, the hosts and their
//!   lanes) and the desktop app's (`desktop`: its passes out of sight);
//! - the logs themselves, and the last picture of each site's page that
//!   wanted a person (under `pages/`).
//!
//! Every link in all of them loses its `#…` part, which holds mega.nz's keys;
//! queries stay, since some hosts (1fichier) name the file in theirs.

use std::{
    collections::HashMap,
    fmt::Write as _,
    io::{BufRead, Read, Seek, SeekFrom},
    path::Path,
};

use serde_json::{Value, json};

use super::Engine;
use crate::logging::{utc, without_keys};

/// How much of each log the timeline reads, from its end.
const LOG_TAIL: u64 = 4 * 1024 * 1024;

/// The longest a field's value runs in the timeline.
const FIELD_AT_MOST: usize = 600;

impl Engine {
    /// The report as one file to send, named for when it was made. `desktop`:
    /// the desktop app's insides, `null` elsewhere.
    pub fn report_zip(&self, desktop: Value) -> Result<(String, Vec<u8>), String> {
        use std::io::{Cursor, Write};

        use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

        let report = self.diagnostics(desktop);
        let stamp: String = report["about"]["created"]
            .as_str()
            .unwrap_or_default()
            .chars()
            .filter(char::is_ascii_digit)
            .collect();
        let packed = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
        let mut add = |name: &str, bytes: &[u8]| -> Result<(), String> {
            zip.start_file(name, packed).map_err(|e| e.to_string())?;
            zip.write_all(bytes).map_err(|e| e.to_string())
        };
        let dir = &self.0.data_dir;
        let logs: Vec<_> = std::iter::once(crate::logging::path(dir))
            .chain(crate::logging::older(dir).into_iter().take(1))
            .collect();
        add("timeline.txt", self.timeline(&logs).as_bytes())?;
        add(
            "report.json",
            &serde_json::to_vec_pretty(&report).map_err(|e| e.to_string())?,
        )?;
        // Line by line through the same stripping as the timeline: a log
        // written before the log stripped keys itself still holds them.
        for log in &logs {
            if let (Some(name), Ok(file)) = (log.file_name(), std::fs::File::open(log)) {
                let mut text = String::new();
                for line in std::io::BufReader::new(file)
                    .split(b'\n')
                    .map_while(Result::ok)
                {
                    let _ = writeln!(text, "{}", unkeyed(&String::from_utf8_lossy(&line)));
                }
                add(&name.to_string_lossy(), text.as_bytes())?;
            }
        }
        if let Ok(pages) = std::fs::read_dir(crate::paths::pass_profiles(dir)) {
            for page in pages.flatten() {
                let name = page.file_name().to_string_lossy().into_owned();
                if name.ends_with(crate::paths::WANTS_PERSON)
                    && let Ok(bytes) = std::fs::read(page.path())
                {
                    add(&format!("pages/{name}"), &bytes)?;
                }
            }
        }
        let bytes = zip.finish().map_err(|e| e.to_string())?.into_inner();
        Ok((format!("haul-report-{stamp}.zip"), bytes))
    }

    /// The report's `report.json`: Haul and its system, the settings with
    /// every secret left out (an account key says only whether it is set, a
    /// proxy loses its credentials, archive passwords are counted, a redial
    /// command is said to be set), what the UI shows, the engine's insides
    /// and `desktop`'s. Links lose their `#…` part, which holds mega.nz's keys.
    pub fn diagnostics(&self, desktop: Value) -> Value {
        let now = crate::logging::unix_now();
        let snapshot = self.snapshot();
        let mut settings = serde_json::to_value(self.settings()).unwrap_or(Value::Null);
        // Every account key, saying only whether it is set.
        let cfg = self.cfg();
        settings["accounts"] = crate::config::account_keys()
            .map(|key| (key.to_string(), json!(cfg.account(key).is_some())))
            .collect::<serde_json::Map<_, _>>()
            .into();
        if let Some(passwords) = settings.get_mut("archive_passwords") {
            *passwords = json!(passwords.as_array().map_or(0, Vec::len));
        }
        // A command often holds the router's login.
        if let Some(command) = settings.get_mut("reconnect_command")
            && !command.is_null()
        {
            *command = json!("(set)");
        }
        if let Some(proxy) = settings.get_mut("proxy") {
            *proxy = json!(without_credentials(proxy.as_str().unwrap_or_default()));
        }
        if let Some(proxies) = settings.get_mut("proxies").and_then(Value::as_array_mut) {
            for proxy in proxies {
                *proxy = json!(without_credentials(proxy.as_str().unwrap_or_default()));
            }
        }
        let mut engine = self.debug_inspect();
        let mut view = serde_json::to_value(&snapshot).unwrap_or(Value::Null);
        // Each row once, in the queue's order: what the engine holds, and
        // what the UI shows of it that the engine's fields don't say.
        let mut shown: HashMap<String, Value> = HashMap::new();
        for list in ["items", "wrappers"] {
            for v in take(&mut view, list).as_array_mut().into_iter().flatten() {
                if let Some(id) = v["id"].as_str() {
                    shown.insert(id.to_string(), v.take());
                }
            }
        }
        let mut rows = take(&mut engine, "items");
        for row in rows.as_array_mut().into_iter().flatten() {
            let id = row["id"].as_str().unwrap_or_default().to_string();
            if let Some(v) = shown.remove(&id) {
                row["view"] = only_new(v, row);
            }
            // Its last events, as the row keeps them, debug ones included:
            // the timeline holds them already when the detailed log is on.
            if !cfg.detailed_log {
                row["history"] = crate::logging::history(&id)
                    .into_iter()
                    .map(|m| json!({"at": m.at, "level": m.level, "message": m.message}))
                    .collect();
            }
        }
        // Each host once: what the UI shows of it, what the engine measured
        // that this doesn't say, and its lanes, one per exit.
        let mut measured: HashMap<String, Value> = HashMap::new();
        for h in take(&mut engine, "hosts")
            .as_array_mut()
            .into_iter()
            .flatten()
        {
            if let Some(name) = h["name"].as_str() {
                measured.insert(name.to_string(), h.take());
            }
        }
        let mut lanes: HashMap<String, Vec<Value>> = HashMap::new();
        for mut lane in take(&mut engine, "lanes")
            .as_array_mut()
            .into_iter()
            .flatten()
            .map(Value::take)
        {
            let host = lane
                .as_object_mut()
                .and_then(|o| o.remove("host"))
                .and_then(|h| h.as_str().map(str::to_string))
                .unwrap_or_default();
            lanes.entry(host).or_default().push(without_defaults(lane));
        }
        let mut hosts = take(&mut view, "hosts");
        let list = hosts.as_array_mut();
        let mut all: Vec<Value> = list.map(std::mem::take).unwrap_or_default();
        for name in measured.keys().chain(lanes.keys()) {
            if !all.iter().any(|h| h["name"] == *name.as_str()) {
                all.push(json!({ "name": name }));
            }
        }
        // Only the hosts this run dealt with or the settings name: what Haul
        // knows of the others is in its code.
        all.retain_mut(|h| {
            let name = h["name"].as_str().unwrap_or_default().to_string();
            let mut met = false;
            if let Some(m) = measured.remove(&name) {
                h["measured"] = only_new(m, h);
                met = true;
            }
            if let Some(l) = lanes.remove(&name) {
                h["lanes"] = json!(l);
                met = true;
            }
            let named = !h["preferred"].is_null()
                || h["disabled"] == true
                || h["account"] == true
                || h["folder"].as_str().is_some_and(|f| !f.is_empty());
            *h = without_defaults(h.take());
            met || named
        });
        let mut report = json!({
            "about": {
                "haul": env!("CARGO_PKG_VERSION"),
                "os": std::env::consts::OS,
                "arch": std::env::consts::ARCH,
                "created": utc(now),
                "desktop": !desktop.is_null(),
                // The download folder, the settings file and its error are
                // under `settings`.
                "data_dir": snapshot.data_dir,
                "detailed_log": cfg.detailed_log,
                "live_debug": crate::debug::enabled(),
                "paused": snapshot.paused,
                "speed": snapshot.speed,
                "update": take(&mut view, "update"),
            },
            "settings": settings,
            "counts": take(&mut view, "counts"),
            "rows": rows,
            "cards": take(&mut view, "wrapper_cards"),
            "archives": take(&mut view, "archives"),
            "hosts": all,
            "network": {
                "proxy": take(&mut view, "proxy"),
                "rate": take(&mut view, "rate"),
                "max_total": take(&mut view, "max_total"),
                "pool": take(&mut view, "pool"),
                "pool_summary": take(&mut view, "pool_summary"),
                "reconnect": take(&mut view, "reconnect"),
                "services": take(&mut view, "services"),
            },
            // The engine's own state beside its rows and hosts.
            "engine": engine,
            "desktop": desktop,
        });
        redact(&mut report);
        report
    }

    /// The logs `logs` (newest first) as one timeline, oldest first: a line
    /// per event, `MM-DD HH:MM:SS.mmm L target [row] message key=value…`,
    /// rows named as the queue or the log last named them.
    fn timeline(&self, logs: &[std::path::PathBuf]) -> String {
        let lines: Vec<String> = logs.iter().rev().flat_map(|p| tail_lines(p)).collect();
        let mut names: HashMap<String, String> = HashMap::new();
        let parsed: Vec<Result<Value, &String>> = lines
            .iter()
            .map(|l| serde_json::from_str::<Value>(l).map_err(|_| l))
            .collect();
        for line in parsed.iter().flatten() {
            let signal = line["signal"]
                .as_str()
                .and_then(|s| serde_json::from_str::<Value>(s).ok());
            if let Some(s) = signal
                && let (Some(id), Some(name)) = (s["id"].as_str(), s["name"].as_str())
                && !name.is_empty()
            {
                names.insert(id.to_string(), name.to_string());
            }
        }
        for it in self.lock().items.values() {
            names.insert(it.id.clone(), super::display_name(it));
        }
        let mut out = String::new();
        for line in parsed {
            match line {
                Ok(v) => timeline_line(&mut out, &v, &names),
                Err(raw) => {
                    let _ = writeln!(out, "{}", unkeyed(raw));
                }
            }
        }
        out
    }
}

/// One log line `v` in the timeline.
fn timeline_line(out: &mut String, v: &Value, names: &HashMap<String, String>) {
    let text = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or_default();
    let message = unkeyed(text("message"));
    // A run's first line heads what it wrote.
    if text("target") == "haul_core::logging"
        && message.starts_with("Haul ")
        && message.ends_with(" starts")
    {
        let _ = writeln!(
            out,
            "==== {} {message} ({}, pid {}) ====",
            text("timestamp").get(..19).unwrap_or_default(),
            text("os"),
            v["pid"]
        );
        return;
    }
    let at = text("timestamp");
    let at: String = at
        .get(5..)
        .unwrap_or(at)
        .trim_end_matches('Z')
        .chars()
        .take(18)
        .collect::<String>()
        .replace('T', " ");
    let level = text("level").chars().next().unwrap_or('?');
    let target = text("target")
        .trim_start_matches("haul_core::")
        .trim_start_matches("haul_desktop::")
        .trim_start_matches("haul_web::")
        .trim_start_matches("engine::");
    let id = text("item");
    let row = (!id.is_empty()).then(|| {
        let name = names.get(id).map(String::as_str).unwrap_or_default();
        let short = id.get(..6).unwrap_or(id);
        let tail: String = {
            let chars: Vec<char> = name.chars().collect();
            let from = chars.len().saturating_sub(32);
            let cut: String = chars[from..].iter().collect();
            if from > 0 { format!("…{cut}") } else { cut }
        };
        (format!("[{short} {tail}] "), name.to_string())
    });
    let mut message = message;
    if let Some((_, name)) = &row
        && !name.is_empty()
        && let Some(rest) = message.strip_prefix(&format!("{name}: "))
    {
        message = rest.to_string();
    }
    let _ = write!(
        out,
        "{at} {level} {target:<16} {}{message}",
        row.as_ref().map_or("", |(label, _)| label.as_str())
    );
    let skip = ["timestamp", "level", "target", "message", "item", "signal"];
    if let Some(fields) = v.as_object() {
        for (k, value) in fields.iter().filter(|(k, _)| !skip.contains(&k.as_str())) {
            let shown = match value {
                Value::String(s) => unkeyed(s),
                other => other.to_string(),
            };
            let _ = write!(out, " {k}={}", clip(&shown));
        }
    }
    // What a pass ended with, which its message leaves out.
    let signal = v["signal"]
        .as_str()
        .and_then(|s| serde_json::from_str::<Value>(s).ok());
    if let Some(s) = signal {
        for k in ["outcome", "error", "pid"] {
            if let Some(value) = s.get(k).filter(|x| !x.is_null() && *x != "") {
                let _ = write!(out, " {k}={}", clip(&unkeyed(&value.to_string())));
            }
        }
    }
    out.push('\n');
}

/// `s`, cut to [`FIELD_AT_MOST`] characters.
fn clip(s: &str) -> String {
    match s.char_indices().nth(FIELD_AT_MOST) {
        Some((cut, _)) => format!("{}…", &s[..cut]),
        None => s.to_string(),
    }
}

/// Field `k` of `o`, taken out of it.
fn take(o: &mut Value, k: &str) -> Value {
    o.get_mut(k).map(Value::take).unwrap_or(Value::Null)
}

/// `v` without the fields `known` holds with the same value, nor its `id`
/// or `name`, nor those at their defaults: what it adds.
fn only_new(mut v: Value, known: &Value) -> Value {
    if let Some(o) = v.as_object_mut() {
        o.retain(|k, x| k != "id" && k != "name" && known.get(k) != Some(x));
    }
    without_defaults(v)
}

/// Object `v` without its fields at a default (`null`, `false`, `0`, `""`,
/// `[]`, `{}`): a field left out is at its default.
fn without_defaults(mut v: Value) -> Value {
    if let Some(o) = v.as_object_mut() {
        o.retain(|_, x| match x {
            Value::Null => false,
            Value::Bool(b) => *b,
            Value::Number(n) => n.as_f64() != Some(0.0),
            Value::String(s) => !s.is_empty(),
            Value::Array(a) => !a.is_empty(),
            Value::Object(m) => !m.is_empty(),
        });
    }
    v
}

/// Takes the `#…` part out of every link in `v`, as the timeline and the
/// logs in the report have it.
fn redact(v: &mut Value) {
    match v {
        Value::String(s) => *s = unkeyed(s),
        Value::Array(a) => a.iter_mut().for_each(redact),
        Value::Object(o) => o.values_mut().for_each(redact),
        _ => {}
    }
}

/// `text` with the `#…` part of every link in it left out.
fn unkeyed(text: &str) -> String {
    without_keys(text).into_owned()
}

/// A proxy address without the user and password in it, for messages and
/// the report.
pub(super) fn without_credentials(proxy: &str) -> String {
    match url::Url::parse(proxy) {
        Ok(mut url) if !url.username().is_empty() || url.password().is_some() => {
            let _ = url.set_username("");
            let _ = url.set_password(None);
            format!("{url} (with credentials)")
        }
        _ => proxy.to_string(),
    }
}

/// The lines of the last [`LOG_TAIL`] bytes of a log, from a line's start;
/// none when there is no such file.
fn tail_lines(path: &Path) -> Vec<String> {
    let Ok(mut file) = std::fs::File::open(path) else {
        return Vec::new();
    };
    let len = file.metadata().map_or(0, |m| m.len());
    let from = len.saturating_sub(LOG_TAIL);
    if file.seek(SeekFrom::Start(from)).is_err() {
        return Vec::new();
    }
    let mut bytes = Vec::new();
    if file.read_to_end(&mut bytes).is_err() {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&bytes);
    let text = match (from > 0, text.find('\n')) {
        (true, Some(cut)) => &text[cut + 1..],
        _ => &text,
    };
    text.lines().map(str::to_string).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_helpers() {
        assert_eq!(utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(utc(1_792_152_000), "2026-10-16T12:00:00Z");
        assert_eq!(utc(951_825_600), "2000-02-29T12:00:00Z");
        assert_eq!(
            unkeyed("https://mega.nz/file/abc#secret-key"),
            "https://mega.nz/file/abc"
        );
        assert_eq!(
            without_credentials("socks5://me:pw@proxy.example:1080"),
            "socks5://proxy.example:1080 (with credentials)"
        );
        assert_eq!(
            without_credentials("http://proxy.example:8080"),
            "http://proxy.example:8080"
        );

        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("haul.log");
        assert!(tail_lines(&log).is_empty());
        let long = format!("{}\nlast line\n", "x".repeat(LOG_TAIL as usize + 10));
        std::fs::write(&log, long).unwrap();
        assert_eq!(tail_lines(&log), ["last line"], "from a line's start");

        let mut report = json!({
            "a": "https://mega.nz/file/a#key",
            "row": {
                "url": "https://cdn.example/f/a.bin?token=t",
                "mirrors": ["https://1fichier.com/?abc123"],
                "handed_back": {"url": "https://cdn.example/f/a.bin?token=t", "for": "https://1fichier.com/?abc123"},
            },
        });
        redact(&mut report);
        assert_eq!(
            report,
            json!({
                "a": "https://mega.nz/file/a",
                "row": {
                    "url": "https://cdn.example/f/a.bin?token=t",
                    "mirrors": ["https://1fichier.com/?abc123"],
                    "handed_back": {"url": "https://cdn.example/f/a.bin?token=t", "for": "https://1fichier.com/?abc123"},
                },
            })
        );
    }

    #[test]
    fn the_timeline_names_rows_and_heads_each_run() {
        let names = HashMap::from([("abc123def0".to_string(), "archive.part1.rar".to_string())]);
        let mut out = String::new();
        for line in [
            json!({"timestamp": "2026-11-06T13:42:17.000Z", "level": "INFO", "target": "haul_core::logging", "message": "Haul 0.7.0 starts", "version": "0.7.0", "os": "windows", "pid": 7}),
            json!({"timestamp": "2026-11-06T13:45:17.123456Z", "level": "INFO", "target": "haul_core::signal", "item": "abc123def0", "message": "archive.part1.rar: paused -> removed", "signal": "{\"kind\":\"status\"}"}),
            json!({"timestamp": "2026-11-06T13:45:18.5Z", "level": "DEBUG", "target": "haul_core::engine::schedule", "item": "abc123def0", "message": "starting from site.example", "url": "https://mega.nz/file/x#key"}),
            json!({"timestamp": "2026-11-06T13:45:19.000Z", "level": "INFO", "target": "haul_core::signal", "message": "pass end: https://site.example/f", "signal": "{\"kind\":\"pass\",\"outcome\":\"listed\"}"}),
        ] {
            timeline_line(&mut out, &line, &names);
        }
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(
            lines[0],
            "==== 2026-11-06T13:42:17 Haul 0.7.0 starts (windows, pid 7) ===="
        );
        assert_eq!(
            lines[1],
            "11-06 13:45:17.123 I signal           [abc123 archive.part1.rar] paused -> removed"
        );
        assert_eq!(
            lines[2],
            "11-06 13:45:18.5 D schedule         [abc123 archive.part1.rar] starting from site.example url=https://mega.nz/file/x"
        );
        assert_eq!(
            lines[3],
            "11-06 13:45:19.000 I signal           pass end: https://site.example/f outcome=\"listed\""
        );
    }
}
