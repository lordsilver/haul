//! A report of what Haul is doing, for the user to save and send along with
//! a problem, so what happened can be read rather than guessed.

use std::{
    io::{Read, Seek, SeekFrom},
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use serde_json::{Value, json};

use super::Engine;
use crate::item::Item;

/// How much of each log the report carries, from its end.
const LOG_TAIL: u64 = 256 * 1024;

impl Engine {
    /// The report: Haul's version and system, the settings with every secret
    /// left out (an account key says only whether it is set, a proxy loses
    /// its credentials), each download and wrapper with its links, state and
    /// error and its last events, the hosts, and the end of the log and of
    /// the one before it, each line an object.
    /// Links lose their `#…` part, which holds mega.nz's keys, and a link
    /// handed back from a browser its query, which can hold a token.
    pub fn diagnostics(&self) -> Value {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let snapshot = self.snapshot();
        let mut settings = serde_json::to_value(self.settings()).unwrap_or(Value::Null);
        if let Some(accounts) = settings.get_mut("accounts").and_then(Value::as_object_mut) {
            for value in accounts.values_mut() {
                *value = json!(value.as_str().is_some_and(|v| !v.is_empty()));
            }
        }
        if let Some(proxy) = settings.get_mut("proxy") {
            *proxy = json!(without_credentials(proxy.as_str().unwrap_or_default()));
        }
        let cfg = self.cfg();
        let queue: Vec<Value> = self.lock().items.values().map(item_report).collect();
        json!({
            "haul": env!("CARGO_PKG_VERSION"),
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
            "created": utc(now),
            "paused": snapshot.paused,
            "speed": snapshot.speed,
            "download_dir": snapshot.download_dir,
            "data_dir": snapshot.data_dir,
            "config_error": snapshot.config_error,
            "settings": settings,
            "preferred_hosts": cfg.preferred_hosts,
            "routes": cfg.routes,
            "limits": cfg.limits,
            "folders": cfg.folders,
            "queue": queue,
            "hosts": snapshot.hosts,
            "detailed_log": cfg.detailed_log,
            "log": tail(&crate::logging::path(&self.0.data_dir)),
            "previous_log": crate::logging::older(&self.0.data_dir)
                .first()
                .map_or(Value::Null, |p| tail(p)),
        })
    }
}

fn item_report(it: &Item) -> Value {
    let bare = |links: &mut dyn Iterator<Item = &String>| -> Vec<String> {
        let mut links: Vec<String> = links.map(|l| without_fragment(l)).collect();
        links.sort();
        links
    };
    json!({
        "id": it.id,
        "wrapper": it.wrapper,
        "source": it.source,
        "title": it.title,
        "found": it.found,
        "name": it.name,
        "package": it.package,
        "status": it.status.as_str(),
        "probed": it.probed,
        "error": it.error,
        "size": it.size,
        "done": it.done,
        "host": it.host,
        "mirrors": it.mirrors.iter().map(|m| without_fragment(m)).collect::<Vec<_>>(),
        "failed": bare(&mut it.failed.iter()),
        "needs_you": bare(&mut it.manual_urls.iter()),
        "handed_back": it.pre.as_ref().filter(|_| it.manual).map(|p| page_only(&p.url)),
        "retries": it.retries,
        "auto_retries": it.auto_retries,
        "busy": it.busy,
        "existing": it.existing,
        "path": it.path,
        "history": crate::logging::history(&it.id)
            .into_iter()
            .map(|m| json!({"at": m.at, "level": m.level, "message": without_fragment_in(&m.message)}))
            .collect::<Vec<_>>(),
    })
}

/// `text` with the `#…` part of every link in it left out.
fn without_fragment_in(text: &str) -> String {
    text.split(' ')
        .map(|word| match word.find("://") {
            Some(_) => without_fragment(word),
            None => word.to_string(),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn without_fragment(link: &str) -> String {
    link.split('#').next().unwrap_or_default().to_string()
}

/// A link without its query or fragment.
fn page_only(link: &str) -> String {
    match url::Url::parse(link) {
        Ok(mut url) => {
            url.set_query(None);
            url.set_fragment(None);
            url.to_string()
        }
        Err(_) => String::new(),
    }
}

/// A proxy address without the user and password in it.
fn without_credentials(proxy: &str) -> String {
    match url::Url::parse(proxy) {
        Ok(mut url) if !url.username().is_empty() || url.password().is_some() => {
            let _ = url.set_username("");
            let _ = url.set_password(None);
            format!("{url} (with credentials)")
        }
        _ => proxy.to_string(),
    }
}

/// The last [`LOG_TAIL`] bytes of a log, from a line's start; `null` when
/// there is no such file.
fn tail(path: &Path) -> Value {
    let Ok(mut file) = std::fs::File::open(path) else {
        return Value::Null;
    };
    let len = file.metadata().map_or(0, |m| m.len());
    let from = len.saturating_sub(LOG_TAIL);
    if file.seek(SeekFrom::Start(from)).is_err() {
        return Value::Null;
    }
    let mut bytes = Vec::new();
    if file.read_to_end(&mut bytes).is_err() {
        return Value::Null;
    }
    let text = String::from_utf8_lossy(&bytes);
    let text = match (from > 0, text.find('\n')) {
        (true, Some(cut)) => &text[cut + 1..],
        _ => &text,
    };
    // One object per line, as haul.log writes them; a line that is not one
    // (a log from before 0.8.1) stays text.
    Value::Array(
        text.lines()
            .map(|line| {
                serde_json::from_str::<Value>(line)
                    .map(redact_fragments)
                    .unwrap_or_else(|_| json!(without_fragment_in(line)))
            })
            .collect(),
    )
}

/// A log line with the `#…` part of its links left out: mega.nz keeps its
/// keys there.
fn redact_fragments(mut line: Value) -> Value {
    if let Some(fields) = line.as_object_mut() {
        for value in fields.values_mut() {
            if let Some(text) = value.as_str() {
                *value = json!(without_fragment_in(text));
            }
        }
    }
    line
}

/// `secs` since 1970 as an ISO 8601 UTC time, such as `2026-10-16T12:30:00Z`.
fn utc(secs: u64) -> String {
    let (days, rest) = (secs / 86_400, secs % 86_400);
    // Days since 1970-01-01 to a civil date (Howard Hinnant's algorithm).
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3_600,
        rest % 3_600 / 60,
        rest % 60
    )
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
            without_fragment("https://mega.nz/file/abc#secret-key"),
            "https://mega.nz/file/abc"
        );
        assert_eq!(
            page_only("https://cdn.example/f/a.bin?token=t#x"),
            "https://cdn.example/f/a.bin"
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
        assert_eq!(tail(&log), Value::Null);
        let long = format!("{}\nlast line\n", "x".repeat(LOG_TAIL as usize + 10));
        std::fs::write(&log, long).unwrap();
        assert_eq!(tail(&log), json!(["last line"]), "from a line's start");
        std::fs::write(&log, "{\"message\":\"got https://mega.nz/file/a#key\",\"item\":\"x\"}\nplain https://mega.nz/file/b#k2\n").unwrap();
        assert_eq!(
            tail(&log),
            json!([{"message": "got https://mega.nz/file/a", "item": "x"}, "plain https://mega.nz/file/b"])
        );
    }
}
