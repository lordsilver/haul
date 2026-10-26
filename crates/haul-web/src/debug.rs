//! Debug mode's API ([`haul_core::debug`]): the live signal stream, the
//! engine's and the desktop app's insides, waiting for a signal, and a look
//! into (or a script run in) a page being passed. Routed only in debug
//! mode, and answered only with the token in `X-Haul-Debug`.

use std::{convert::Infallible, time::Duration};

use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::{Path, Query, Request, State},
    http::{StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
};
use haul_core::debug;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::broadcast::error::RecvError;

use crate::{App, error};

/// Longest `/api/debug/wait`.
const WAIT_MAX: u64 = 600;

pub(crate) fn routes(app: &App) -> Router<App> {
    Router::new()
        .route("/api/debug/stream", get(stream))
        .route("/api/debug/recent", get(recent))
        .route("/api/debug/inspect", get(inspect))
        .route("/api/debug/wait", axum::routing::post(wait))
        .route("/api/debug/pass/{id}", get(look).post(eval))
        .route("/api/debug/emit", axum::routing::post(emit))
        .route_layer(middleware::from_fn_with_state(app.clone(), token))
}

async fn token(req: Request, next: Next) -> Response {
    let Some(bus) = debug::bus() else {
        return error(StatusCode::NOT_FOUND, "debug mode is off");
    };
    let sent = req
        .headers()
        .get("x-haul-debug")
        .and_then(|v| v.to_str().ok());
    if sent != Some(bus.token.as_str()) {
        return error(
            StatusCode::FORBIDDEN,
            "send the token in debug-token (in the data folder) as X-Haul-Debug",
        );
    }
    next.run(req).await
}

#[derive(Deserialize)]
struct StreamQuery {
    /// Send the signals kept so far first.
    #[serde(default)]
    replay: bool,
}

/// Every signal as it happens, one JSON object per line: a `hello` first
/// (version, process, data folder), the kept ones with `?replay=true`,
/// then the live ones; `lagged` says how many a slow reader missed.
async fn stream(State(app): State<App>, Query(q): Query<StreamQuery>) -> Response {
    let Some(bus) = debug::bus() else {
        return error(StatusCode::NOT_FOUND, "debug mode is off");
    };
    let rx = bus.subscribe();
    let hello = json!({
        "t": "",
        "kind": "hello",
        "version": env!("CARGO_PKG_VERSION"),
        "pid": std::process::id(),
        "desktop": app.desktop.is_some(),
        "data_dir": app.engine.snapshot().data_dir,
    });
    let mut first = vec![hello];
    if q.replay {
        first.extend(bus.recent());
    }
    let line = |v: &Value| Bytes::from(format!("{v}\n"));
    let head = futures_util::stream::iter(
        first
            .iter()
            .map(line)
            .map(Ok::<_, Infallible>)
            .collect::<Vec<_>>(),
    );
    let live = futures_util::stream::unfold(rx, move |mut rx| async move {
        let next = match rx.recv().await {
            Ok(v) => v,
            Err(RecvError::Lagged(n)) => json!({ "kind": "lagged", "missed": n }),
            Err(RecvError::Closed) => return None,
        };
        Some((Ok::<_, Infallible>(Bytes::from(format!("{next}\n"))), rx))
    });
    let body = Body::from_stream(futures_util::StreamExt::chain(head, live));
    (
        [
            (header::CONTENT_TYPE, "application/x-ndjson"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        body,
    )
        .into_response()
}

/// The signals kept so far, oldest first.
async fn recent() -> Response {
    match debug::bus() {
        Some(bus) => Json(bus.recent()).into_response(),
        None => error(StatusCode::NOT_FOUND, "debug mode is off"),
    }
}

/// The state the UI shows, the engine's insides, and the desktop app's.
async fn inspect(State(app): State<App>) -> Json<Value> {
    let desktop = app
        .desktop
        .as_ref()
        .map_or(Value::Null, |d| d.debug_inspect());
    Json(json!({
        "state": app.view(),
        "engine": app.engine.debug_inspect(),
        "desktop": desktop,
    }))
}

#[derive(Deserialize)]
struct WaitBody {
    /// The fields a signal must hold ([`debug::matches`]).
    #[serde(rename = "match")]
    want: Value,
    /// Seconds, at most [`WAIT_MAX`]; 60 when left out.
    #[serde(default)]
    timeout: Option<u64>,
    /// Also look among the signals kept so far, newest last.
    #[serde(default)]
    recent: bool,
}

/// Answers with the first signal that matches, or 408 after the timeout.
async fn wait(Json(body): Json<WaitBody>) -> Response {
    let Some(bus) = debug::bus() else {
        return error(StatusCode::NOT_FOUND, "debug mode is off");
    };
    let mut rx = bus.subscribe();
    if body.recent
        && let Some(found) = bus
            .recent()
            .into_iter()
            .find(|s| debug::matches(s, &body.want))
    {
        return Json(found).into_response();
    }
    let secs = body.timeout.unwrap_or(60).min(WAIT_MAX);
    let found = tokio::time::timeout(Duration::from_secs(secs), async {
        loop {
            match rx.recv().await {
                Ok(s) if debug::matches(&s, &body.want) => return Some(s),
                Ok(_) | Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => return None,
            }
        }
    })
    .await;
    match found {
        Ok(Some(s)) => Json(s).into_response(),
        Ok(None) => error(StatusCode::GONE, "the bus closed"),
        Err(_) => error(
            StatusCode::REQUEST_TIMEOUT,
            format!("no matching signal in {secs} s"),
        ),
    }
}

/// What item `id`'s page shows while the desktop app passes it: title,
/// address, frames, Turnstile answers, buttons, links and a screenshot.
async fn look(State(app): State<App>, Path(id): Path<String>) -> Response {
    page(app, id, Value::Null).await
}

/// Runs a script in item `id`'s page while the desktop app passes it
/// (`{"eval": "…"}`), or clicks an element there as a person would
/// (`{"click": "css selector"}`).
async fn eval(State(app): State<App>, Path(id): Path<String>, Json(body): Json<Value>) -> Response {
    if body["eval"].as_str().is_none()
        && body["click"].as_str().is_none()
        && body["cookies"].as_bool() != Some(true)
    {
        return error(
            StatusCode::BAD_REQUEST,
            r#"send {"eval": "…"}, {"click": "css selector"} or {"cookies": true}"#,
        );
    }
    page(app, id, body).await
}

async fn page(app: App, id: String, ask: Value) -> Response {
    let Some(desktop) = app.desktop.clone() else {
        return error(
            StatusCode::NOT_IMPLEMENTED,
            "only the desktop app passes pages",
        );
    };
    match tokio::task::spawn_blocking(move || desktop.debug_page(&id, &ask)).await {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(e)) => error(StatusCode::CONFLICT, e),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

/// Puts a signal of the client's own on the bus (`{"kind": "note", …}`),
/// to mark a step of a test in the stream.
async fn emit(Json(mut body): Json<Value>) -> Response {
    let kind = body
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("note")
        .to_string();
    if let Value::Object(fields) = &mut body {
        fields.remove("kind");
    }
    debug::emit(&kind, body);
    StatusCode::NO_CONTENT.into_response()
}
