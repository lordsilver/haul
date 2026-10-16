//! HTTP API and the bundled web UI for a running [`Engine`].
//!
//! The server listens on loopback only, but any web page the user visits can
//! still send requests there. So every request has to name this server in
//! `Host` (which defeats DNS rebinding), and a browser request that changes
//! anything has to come from this server's own origin.

use std::{collections::BTreeMap, convert::Infallible, sync::Arc, time::Duration};

use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::{DefaultBodyLimit, Multipart, Path, Request, State},
    http::{HeaderValue, Method, StatusCode, header},
    middleware::{self, Next},
    response::{
        IntoResponse, Response,
        sse::{Event, KeepAlive, Sse},
    },
    routing::{get, post},
};
use futures_util::Stream;
use haul_core::{Added, Engine, ItemAction, Route, SettingsView, Snapshot, config};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::net::TcpListener;

mod os;

/// What only the desktop app can do.
pub trait Desktop: Send + Sync + 'static {
    /// Opens `url` for item `id` in an in-app browser window. A download the
    /// user starts there goes to the engine instead of being saved.
    fn browse(&self, id: &str, url: &str, user_agent: Option<String>) -> Result<(), String>;

    /// Asks the user for a folder, starting in `start`; blocks until they
    /// answer. `None` when they cancel.
    fn pick_folder(&self, start: &std::path::Path) -> Option<std::path::PathBuf>;

    /// Asks the user where to save `contents`, suggesting `name`, and saves
    /// it there; blocks until they answer. `None` when they cancel.
    fn save_file(&self, name: &str, contents: &[u8]) -> Result<Option<std::path::PathBuf>, String>;
}

#[derive(Clone)]
struct App {
    engine: Engine,
    desktop: Option<Arc<dyn Desktop>>,
    port: u16,
}

#[derive(Serialize)]
struct View {
    #[serde(flatten)]
    snapshot: Snapshot,
    desktop: bool,
    version: &'static str,
}

impl App {
    fn view(&self) -> View {
        View {
            snapshot: self.engine.snapshot(),
            desktop: self.desktop.is_some(),
            version: env!("CARGO_PKG_VERSION"),
        }
    }
}

/// Serves the UI and API until the listener fails.
pub async fn serve(
    listener: TcpListener,
    engine: Engine,
    desktop: Option<Arc<dyn Desktop>>,
) -> std::io::Result<()> {
    let port = listener.local_addr()?.port();
    axum::serve(listener, router(engine, desktop, port)).await
}

/// The UI, built into the binary: path, file in `ui/`, content type, body.
const ASSETS: [(&str, &str, &str, &str); 7] = [
    (
        "/",
        "index.html",
        "text/html; charset=utf-8",
        include_str!("../ui/index.html"),
    ),
    (
        "/app.css",
        "app.css",
        "text/css; charset=utf-8",
        include_str!("../ui/app.css"),
    ),
    ("/app.js", "app.js", JS, include_str!("../ui/app.js")),
    ("/icons.js", "icons.js", JS, include_str!("../ui/icons.js")),
    ("/theme.js", "theme.js", JS, include_str!("../ui/theme.js")),
    (
        "/icon.svg",
        "icon.svg",
        "image/svg+xml",
        include_str!("../ui/icon.svg"),
    ),
    (
        "/favicon.ico",
        "icon.svg",
        "image/svg+xml",
        include_str!("../ui/icon.svg"),
    ),
];
const JS: &str = "text/javascript; charset=utf-8";

pub fn router(engine: Engine, desktop: Option<Arc<dyn Desktop>>, port: u16) -> Router {
    let app = App {
        engine,
        desktop,
        port,
    };
    let mut router = Router::new();
    for (path, file, content_type, body) in ASSETS {
        router = router.route(path, get(move || page(file, content_type, body)));
    }
    router
        .route("/api/state", get(state))
        .route("/api/events", get(events))
        .route("/api/add", post(add))
        .route(
            "/api/container",
            post(container).layer(DefaultBodyLimit::max(64 << 20)),
        )
        .route(
            "/api/container/read",
            post(read_container).layer(DefaultBodyLimit::max(64 << 20)),
        )
        .route("/api/item/{id}/direct", post(direct))
        .route("/api/item/{id}/browse", post(browse))
        .route("/api/item/{id}/reveal", post(reveal))
        .route("/api/item/{id}/{action}", post(item))
        .route("/api/wrapper/{id}/{action}", post(wrapper))
        .route("/api/host/{name}/limit/{n}", post(host_limit))
        .route("/api/host/{name}/folder", post(host_folder))
        .route("/api/host/{name}/preferred", post(host_preferred))
        .route("/api/host/{name}/disabled", post(host_disabled))
        .route("/api/preferred-hosts", post(preferred_hosts))
        .route("/api/host/{name}/route/{mode}", post(host_route))
        .route("/api/global/{action}", post(global))
        .route("/api/router", post(find_router))
        .route("/api/reconnect", post(reconnect))
        .route("/api/nordvpn/servers", post(nordvpn_servers))
        .route("/api/settings", get(settings).post(update_settings))
        .route("/api/settings/download-dir/pick", post(pick_download_dir))
        .route("/api/diagnostics", get(diagnostics))
        .route("/api/diagnostics/save", post(save_diagnostics))
        .route("/api/open/{place}", post(open_place))
        .layer(middleware::from_fn_with_state(app.clone(), guard))
        .layer(middleware::map_response(json_errors))
        .with_state(app)
}

/// axum answers some errors itself, in plain text (a body or a path that does
/// not parse, an unknown route), and so does [`guard`]. The API answers every
/// error as `{"error": "…"}`, so those are wrapped.
async fn json_errors(resp: Response) -> Response {
    let status = resp.status();
    let is_json = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .is_some_and(|v| v.as_bytes().starts_with(b"application/json"));
    if !(status.is_client_error() || status.is_server_error()) || is_json {
        return resp;
    }
    let (mut parts, body) = resp.into_parts();
    let text = axum::body::to_bytes(body, 16 << 10)
        .await
        .unwrap_or_default();
    let text = String::from_utf8_lossy(&text);
    let message = match text.trim() {
        "" => status.canonical_reason().unwrap_or("error").to_lowercase(),
        text => text.to_string(),
    };
    parts.headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    parts.headers.remove(header::CONTENT_LENGTH);
    Response::from_parts(parts, Body::from(json!({ "error": message }).to_string()))
}

fn is_local(authority: &str, port: u16) -> bool {
    ["127.0.0.1", "localhost", "[::1]"]
        .iter()
        .any(|host| authority == format!("{host}:{port}"))
}

async fn guard(State(app): State<App>, req: Request, next: Next) -> Response {
    let host = req
        .headers()
        .get(header::HOST)
        .and_then(|h| h.to_str().ok());
    if !host.is_some_and(|h| is_local(h, app.port)) {
        return (StatusCode::FORBIDDEN, "unexpected Host header").into_response();
    }
    if req.method() != Method::GET && req.method() != Method::HEAD {
        // Browsers send Origin with every POST; other clients do not need one.
        if let Some(origin) = req.headers().get(header::ORIGIN) {
            let same = origin
                .to_str()
                .ok()
                .and_then(|o| o.strip_prefix("http://"))
                .is_some_and(|a| is_local(a, app.port));
            if !same {
                return (StatusCode::FORBIDDEN, "cross-origin request refused").into_response();
            }
        }
    }
    next.run(req).await
}

/// Debug builds read the UI from `HAUL_UI_DIR` when it is set, so a change to
/// the page shows on the next reload without a rebuild.
fn ui_from_disk(file: &str) -> Option<String> {
    if !cfg!(debug_assertions) {
        return None;
    }
    let dir = std::env::var_os("HAUL_UI_DIR")?;
    std::fs::read_to_string(std::path::Path::new(&dir).join(file)).ok()
}

async fn page(file: &'static str, content_type: &'static str, body: &'static str) -> Response {
    let mut resp = match ui_from_disk(file) {
        Some(text) => text.into_response(),
        None => body.into_response(),
    };
    let h = resp.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    // No framing: a page that framed the UI could trick clicks on it.
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'self'; img-src 'self' data:; frame-ancestors 'none'; base-uri 'none'; form-action 'none'"),
    );
    resp
}

fn error(status: StatusCode, message: impl Into<String>) -> Response {
    (status, Json(json!({ "error": message.into() }))).into_response()
}

fn ok() -> Response {
    Json(json!({})).into_response()
}

async fn state(State(app): State<App>) -> Json<View> {
    Json(app.view())
}

/// The full state as `state` events: right away, on every change (bursts
/// merged), and at least once a second for progress.
async fn events(State(app): State<App>) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let rx = app.engine.watch();
    let stream = futures_util::stream::unfold((app, rx, true), |(app, mut rx, first)| async move {
        if !first {
            tokio::select! {
                changed = rx.changed() => if changed.is_err() { return None },
                _ = tokio::time::sleep(Duration::from_secs(1)) => {}
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        rx.borrow_and_update();
        let data = serde_json::to_string(&app.view()).unwrap_or_default();
        Some((
            Ok(Event::default().event("state").data(data)),
            (app, rx, false),
        ))
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
}

#[derive(Deserialize)]
struct AddBody {
    text: String,
    #[serde(default)]
    package: String,
}

async fn add(State(app): State<App>, Json(body): Json<AddBody>) -> Json<Added> {
    Json(app.engine.add_text(&body.text, body.package.trim()))
}

/// The files in a multipart upload, by name; the reason when it is malformed.
async fn uploaded(mut form: Multipart) -> Result<Vec<(String, Bytes)>, String> {
    let mut files = Vec::new();
    while let Some(field) = form.next_field().await.map_err(|e| e.body_text())? {
        let name = field.file_name().unwrap_or_default().to_string();
        let bytes = field.bytes().await.map_err(|e| e.body_text())?;
        files.push((name, bytes));
    }
    Ok(files)
}

async fn container(State(app): State<App>, form: Multipart) -> Response {
    let files = match uploaded(form).await {
        Ok(files) => files,
        Err(e) => return error(StatusCode::BAD_REQUEST, e),
    };
    let mut total = Added::default();
    for (name, bytes) in files {
        match app.engine.import(&name, &bytes).await {
            Ok(a) => {
                total.added += a.added;
                total.merged += a.merged;
                total.duplicates += a.duplicates;
                total.invalid += a.invalid;
            }
            Err(e) => return error(StatusCode::BAD_REQUEST, format!("{name}: {e}")),
        }
    }
    Json(total).into_response()
}

/// The links in uploaded container files, for the Add dialog: nothing is
/// added. `package` is the package they all share, empty when they differ.
async fn read_container(State(app): State<App>, form: Multipart) -> Response {
    let files = match uploaded(form).await {
        Ok(files) => files,
        Err(e) => return error(StatusCode::BAD_REQUEST, e),
    };
    let mut links = Vec::new();
    for (name, bytes) in files {
        match app.engine.read_container(&name, &bytes).await {
            Ok(found) => links.extend(found),
            Err(e) => return error(StatusCode::BAD_REQUEST, format!("{name}: {e}")),
        }
    }
    let package = links
        .first()
        .map(|l| l.package.clone())
        .filter(|p| links.iter().all(|l| &l.package == p))
        .unwrap_or_default();
    let urls: Vec<String> = links.into_iter().map(|l| l.url).collect();
    Json(json!({"links": urls, "package": package})).into_response()
}

/// The UI's route that opens the Add dialog filled in with `links`, for the
/// desktop app to show files and links it was asked to open.
pub fn add_route(links: &str) -> String {
    let encoded = percent_encoding::utf8_percent_encode(links, percent_encoding::NON_ALPHANUMERIC);
    format!("/add/{encoded}")
}

#[derive(Deserialize)]
struct DirectBody {
    url: String,
    #[serde(default)]
    headers: BTreeMap<String, String>,
    #[serde(default)]
    name: Option<String>,
}

/// The manual fallback for hosts that need a person: the user (or a browser
/// extension) supplies the direct link.
async fn direct(
    State(app): State<App>,
    Path(id): Path<String>,
    Json(body): Json<DirectBody>,
) -> Response {
    match app
        .engine
        .set_direct(&id, body.url.trim(), body.headers, body.name)
    {
        Ok(()) => ok(),
        Err(e) => error(StatusCode::BAD_REQUEST, e),
    }
}

#[derive(Deserialize, Default)]
struct BrowseBody {
    user_agent: Option<String>,
}

async fn browse(State(app): State<App>, Path(id): Path<String>, body: Bytes) -> Response {
    let Some(desktop) = &app.desktop else {
        return error(
            StatusCode::NOT_IMPLEMENTED,
            "only the desktop app has an in-app browser",
        );
    };
    let Some(url) = app.engine.page_for(&id) else {
        return error(StatusCode::NOT_FOUND, "no such item");
    };
    let body: BrowseBody = serde_json::from_slice(&body).unwrap_or_default();
    match desktop.browse(&id, &url, body.user_agent) {
        Ok(()) => ok(),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

async fn item(State(app): State<App>, Path((id, action)): Path<(String, String)>) -> Response {
    let Ok(action) = action.parse::<ItemAction>() else {
        return error(StatusCode::BAD_REQUEST, "unknown action");
    };
    let found = match action {
        // May move the file to the Recycle Bin, which can fail.
        ItemAction::Remove => match app.engine.remove(&id).await {
            Ok(found) => found,
            Err(e) => return error(StatusCode::INTERNAL_SERVER_ERROR, e),
        },
        _ => app.engine.item_action(&id, action),
    };
    if found {
        ok()
    } else {
        error(StatusCode::NOT_FOUND, "no such item")
    }
}

/// A wrapper's own actions: start the files it added that wait, or take it
/// off the list with those that never started. A wrapper's id also takes
/// the item actions that apply to it: `retry`, `browse`, `direct`.
async fn wrapper(State(app): State<App>, Path((id, action)): Path<(String, String)>) -> Response {
    match action.as_str() {
        "start" => match app.engine.start_wrapper(&id) {
            Some(started) => Json(json!({ "started": started })).into_response(),
            None => error(StatusCode::NOT_FOUND, "no such wrapper"),
        },
        "remove" if app.engine.remove_wrapper(&id) => ok(),
        "remove" => error(StatusCode::NOT_FOUND, "no such wrapper"),
        _ => error(StatusCode::BAD_REQUEST, "unknown action"),
    }
}

async fn host_limit(State(app): State<App>, Path((name, n)): Path<(String, usize)>) -> Response {
    match app.engine.set_host_limit(&name, n) {
        Ok(()) => ok(),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

#[derive(Deserialize)]
struct Folder {
    folder: String,
}

/// The subfolder for files from one host; empty for the download folder.
async fn host_folder(
    State(app): State<App>,
    Path(name): Path<String>,
    Json(body): Json<Folder>,
) -> Response {
    match app.engine.set_host_folder(&name, &body.folder) {
        Ok(()) => ok(),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

#[derive(Deserialize)]
struct Preferred {
    preferred: bool,
}

/// Adds a host to the end of the preferred hosts, or takes it out.
async fn host_preferred(
    State(app): State<App>,
    Path(name): Path<String>,
    Json(body): Json<Preferred>,
) -> Response {
    match app.engine.set_preferred(&name, body.preferred) {
        Ok(()) => ok(),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

/// Looks for the router on this network again.
async fn find_router(State(app): State<App>) -> Response {
    let router = app.engine.find_router().await;
    Json(serde_json::json!({ "router": router.map(|r| r.model) })).into_response()
}

/// Fetches NordVPN's list of SOCKS5 servers, so its countries can be picked.
async fn nordvpn_servers(State(app): State<App>) -> Response {
    match app.engine.nordvpn_list().await {
        Ok(n) => Json(serde_json::json!({ "servers": n })).into_response(),
        Err(e) => error(StatusCode::BAD_GATEWAY, e),
    }
}

/// Gets this machine's line a new public address now. Answers once the line
/// is back, with the new address, or with why there is none.
async fn reconnect(State(app): State<App>) -> Response {
    match app.engine.reconnect("asked in Haul").await {
        Ok(address) => Json(serde_json::json!({ "address": address })).into_response(),
        Err(e) => error(StatusCode::BAD_GATEWAY, e),
    }
}

#[derive(Deserialize)]
struct Disabled {
    disabled: bool,
}

/// Turns a host off, so its links are never tried, or back on.
async fn host_disabled(
    State(app): State<App>,
    Path(name): Path<String>,
    Json(body): Json<Disabled>,
) -> Response {
    match app.engine.set_disabled(&name, body.disabled) {
        Ok(()) => ok(),
        Err(e) => error(StatusCode::BAD_REQUEST, e),
    }
}

#[derive(Deserialize)]
struct PreferredHosts {
    hosts: Vec<String>,
}

/// The preferred hosts, all of them, in a new order.
async fn preferred_hosts(State(app): State<App>, Json(body): Json<PreferredHosts>) -> Response {
    match app.engine.set_preferred_hosts(&body.hosts) {
        Ok(()) => ok(),
        Err(e) => error(StatusCode::BAD_REQUEST, e),
    }
}

/// The user's approval to reach one host directly, or to go back to the proxy.
async fn host_route(
    State(app): State<App>,
    Path((name, mode)): Path<(String, String)>,
) -> Response {
    let route = match mode.as_str() {
        "direct" => Route::Direct,
        "proxy" => Route::Proxy,
        _ => return error(StatusCode::BAD_REQUEST, "route is direct or proxy"),
    };
    match app.engine.set_route(&name, route) {
        Ok(()) => ok(),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

async fn global(State(app): State<App>, Path(action): Path<String>) -> Response {
    match action.as_str() {
        "pause" => app.engine.set_paused(true),
        "resume" => app.engine.set_paused(false),
        "retry_failed" => app.engine.retry_failed(),
        "clear_done" => app.engine.clear_done(),
        "clear_failed" => app.engine.clear_failed(),
        _ => return error(StatusCode::BAD_REQUEST, "unknown action"),
    }
    ok()
}

/// Every setting, account secrets included; sent only when asked for, never
/// in the state stream.
async fn settings(State(app): State<App>) -> Json<SettingsView> {
    Json(app.engine.settings())
}

/// Changes settings, named as in `config.toml`; each is written there.
async fn update_settings(
    State(app): State<App>,
    Json(body): Json<serde_json::Map<String, serde_json::Value>>,
) -> Response {
    match app.engine.update_settings(&body) {
        Ok(()) => ok(),
        Err(e) => error(StatusCode::BAD_REQUEST, e),
    }
}

/// Lets the user pick the download folder in the system's folder dialog.
async fn pick_download_dir(State(app): State<App>) -> Response {
    let Some(desktop) = app.desktop.clone() else {
        return error(
            StatusCode::NOT_IMPLEMENTED,
            "only the desktop app has a folder dialog",
        );
    };
    let start = app.engine.download_dir();
    let picked = tokio::task::spawn_blocking(move || desktop.pick_folder(&start)).await;
    let Ok(Some(folder)) = picked else {
        return Json(json!({ "cancelled": true })).into_response();
    };
    let change = serde_json::Map::from_iter([(
        "download_dir".to_string(),
        serde_json::Value::String(folder.display().to_string()),
    )]);
    match app.engine.update_settings(&change) {
        Ok(()) => ok(),
        Err(e) => error(StatusCode::BAD_REQUEST, e),
    }
}

/// The diagnostics report and the name to save it under.
fn report(engine: &Engine) -> (String, Vec<u8>) {
    let report = engine.diagnostics();
    let stamp: String = report["created"]
        .as_str()
        .unwrap_or_default()
        .chars()
        .filter(char::is_ascii_digit)
        .collect();
    let body = serde_json::to_vec_pretty(&report).unwrap_or_default();
    (format!("haul-diagnostics-{stamp}.json"), body)
}

/// The diagnostics report, as a file to save: what Haul is doing, with
/// account keys and proxy credentials left out.
async fn diagnostics(State(app): State<App>) -> Response {
    let (name, body) = report(&app.engine);
    (
        [
            (header::CONTENT_TYPE, "application/json".to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{name}\""),
            ),
        ],
        body,
    )
        .into_response()
}

/// Desktop app only (501 elsewhere): saves the report where the user picks
/// in the system's dialog.
async fn save_diagnostics(State(app): State<App>) -> Response {
    let Some(desktop) = app.desktop.clone() else {
        return error(
            StatusCode::NOT_IMPLEMENTED,
            "only the desktop app has a save dialog; GET /api/diagnostics instead",
        );
    };
    let (name, body) = report(&app.engine);
    match tokio::task::spawn_blocking(move || desktop.save_file(&name, &body)).await {
        Ok(Ok(Some(path))) => Json(json!({ "saved": path.display().to_string() })).into_response(),
        Ok(Ok(None)) => Json(json!({ "cancelled": true })).into_response(),
        Ok(Err(e)) => error(StatusCode::INTERNAL_SERVER_ERROR, e),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

/// Shows a finished file in the system's file manager.
async fn reveal(State(app): State<App>, Path(id): Path<String>) -> Response {
    let Some(path) = app.engine.item_path(&id) else {
        return error(StatusCode::NOT_FOUND, "nothing saved for this item yet");
    };
    match os::reveal(&path) {
        Ok(()) => ok(),
        Err(e) => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("{}: {e}", path.display()),
        ),
    }
}

/// Opens one of the app's own places; never a path the caller names.
async fn open_place(State(app): State<App>, Path(place): Path<String>) -> Response {
    let path = match place.as_str() {
        "downloads" => app.engine.download_dir(),
        "data" => app.engine.data_dir().to_path_buf(),
        "config" => match app.engine.config_path() {
            Some(path) => path.to_path_buf(),
            None => return error(StatusCode::NOT_FOUND, "started without a settings file"),
        },
        _ => return error(StatusCode::BAD_REQUEST, "unknown place"),
    };
    let prepared = if place == "config" {
        config::ensure_file(&path)
    } else {
        std::fs::create_dir_all(&path)
    };
    match prepared.and_then(|()| open::that_detached(&path)) {
        Ok(()) => ok(),
        Err(e) => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("{}: {e}", path.display()),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::add_route;

    #[test]
    fn the_add_route_carries_links_intact() {
        let links = "https://a.example/x y?z=1&b=%20\nhttps://b.example/ü";
        let route = add_route(links);
        let encoded = route.strip_prefix("/add/").unwrap();
        assert!(
            encoded
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'%')
        );
        let back = percent_encoding::percent_decode_str(encoded)
            .decode_utf8()
            .unwrap();
        assert_eq!(back, links, "decodeURIComponent gets the links back");
    }
}
