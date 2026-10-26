//! What the engine holds beyond the state the UI shows, for debug mode's
//! `/api/debug/inspect`: each row's links and what became of each, its
//! timers and counters, and each host's lanes, cooldowns and measures.

use std::time::Instant;

use serde_json::{Value, json};

use super::Engine;

/// Seconds from now until `at`, or `None`; a time already past is 0.
fn until(at: Option<Instant>, now: Instant) -> Option<u64> {
    at.map(|t| t.saturating_duration_since(now).as_secs())
}

impl Engine {
    /// The engine's insides, as JSON.
    pub fn debug_inspect(&self) -> Value {
        let st = self.lock();
        let now = Instant::now();
        let items: Vec<Value> = st
            .items
            .values()
            .map(|it| {
                let mut failed: Vec<&String> = it.failed.iter().collect();
                failed.sort();
                let mut gone: Vec<&String> = it.gone.iter().collect();
                gone.sort();
                let mut manual: Vec<&String> = it.manual_urls.iter().collect();
                manual.sort();
                json!({
                    "id": it.id,
                    "name": it.name,
                    "status": it.status.as_str(),
                    "wrapper": it.wrapper,
                    "tentative": it.tentative,
                    "probed": it.probed,
                    "running": it.task.is_some(),
                    "exit": it.exit,
                    "host": it.host,
                    "mirrors": it.mirrors,
                    "failed": failed,
                    "gone": gone,
                    "manual_urls": manual,
                    "page_wants_person": it.page_wants_person,
                    "prefer": it.prefer,
                    "error": it.error,
                    "size": it.size,
                    "done": it.done,
                    "retries": it.retries,
                    "busy": it.busy,
                    "auto_retries": it.auto_retries,
                    "retry_in": until(it.retry_at, now),
                    "not_before_in": until(it.not_before, now),
                    "source": it.source,
                    "also_from": it.also_from,
                })
            })
            .collect();
        let hosts: Vec<Value> = st
            .hosts
            .iter()
            .map(|(name, h)| {
                json!({
                    "name": name,
                    "limit": h.limit,
                    "speed": h.ema,
                    "fails": h.fails,
                    "penalty": h.penalty(now),
                    "ok": h.ok,
                    "bytes": h.bytes,
                    "blocked": h.blocked,
                })
            })
            .collect();
        let mut lanes: Vec<Value> = st
            .lanes
            .iter()
            .map(|((host, exit), lane)| {
                json!({
                    "host": host,
                    "exit": exit,
                    "active": lane.active,
                    "serving": lane.serving,
                    "cap": lane.cap,
                    "cooldown_in": until(lane.cooldown, now),
                    "settle_in": until(lane.settle_until, now),
                })
            })
            .collect();
        lanes.sort_by_key(|l| {
            (
                l["host"].as_str().unwrap_or_default().to_string(),
                l["exit"].as_u64(),
            )
        });
        let mut guarded: Vec<&String> = st.guarded.iter().collect();
        guarded.sort();
        let mut cleared: Vec<&String> = st.cleared.iter().collect();
        cleared.sort();
        json!({
            "paused": st.paused,
            "moving": st.moving,
            "reconnecting": st.reconnecting,
            "max_total": st.max_total,
            "pages_pass": self.pages_pass(),
            "guarded": guarded,
            "cleared": cleared,
            "items": items,
            "hosts": hosts,
            "lanes": lanes,
        })
    }
}
