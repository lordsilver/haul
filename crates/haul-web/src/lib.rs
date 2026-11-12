//! HTTP API and the bundled web UI for a running [`Engine`].
//!
//! The server listens on loopback only, but any web page the user visits can
//! still send requests there. So every request has to name this server in
//! `Host` (which defeats DNS rebinding), and a browser request that changes
//! anything has to come from this server's own origin.

use std::{
    collections::BTreeMap,
    convert::Infallible,
    sync::{Arc, OnceLock},
    time::Duration,
};

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
    routing::{MethodRouter, get, post},
};
use futures_util::Stream;
use haul_core::{
    Added, Batch, Engine, ItemAction, Route, SettingError, SettingsView, Snapshot, WrapperAction,
    config,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::{
    net::TcpListener,
    sync::{broadcast, watch},
};

mod debug;
mod os;

/// What only the desktop app can do.
pub trait Desktop: Send + Sync + 'static {
    /// Opens `url` for item `id` in an in-app browser window. A download the
    /// user starts there goes to the engine instead of being saved.
    fn browse(&self, id: &str, url: &str, user_agent: Option<String>) -> Result<(), String>;

    /// Asks the user for a folder, starting in `start`; blocks until they
    /// answer. `None` when they cancel.
    fn pick_folder(&self, start: &std::path::Path) -> Option<std::path::PathBuf>;

    /// Asks the user where to save a file named `name`, starting in `start`;
    /// blocks until they answer. `None` when they cancel.
    fn pick_save(&self, start: &std::path::Path, name: &str) -> Option<std::path::PathBuf>;

    /// Runs the newer Haul's setup program at `installer`, which installs it
    /// over this one and starts it again, and closes the app.
    fn install_update(&self, installer: &std::path::Path) -> Result<(), String>;

    /// Debug mode: what the desktop app holds (pages being passed, their
    /// browsers), as JSON.
    fn debug_inspect(&self) -> serde_json::Value {
        serde_json::Value::Null
    }

    /// Debug mode: what item `id`'s page shows while it is passed out of
    /// sight; with `{"eval": script}`, what the script answers when run in
    /// it; with `{"click": selector}`, a person's click on that element.
    fn debug_page(&self, _id: &str, _ask: &serde_json::Value) -> Result<serde_json::Value, String> {
        Err("no page of that item is being passed".into())
    }
}

#[derive(Clone)]
struct App {
    engine: Engine,
    desktop: Option<Arc<dyn Desktop>>,
    port: u16,
    /// The state as the stream sends it ([`App::states`]).
    states: Arc<OnceLock<watch::Receiver<Arc<str>>>>,
    /// What the engine has to tell, as the stream sends it ([`App::notices`]).
    notices: Arc<OnceLock<broadcast::Sender<Arc<str>>>>,
}

#[derive(Serialize, ts_rs::TS)]
#[ts(export)]
#[ts(rename = "State")]
struct View {
    #[serde(flatten)]
    snapshot: Snapshot,
    desktop: bool,
    version: &'static str,
}

impl App {
    /// The desktop app, or the answer for a request only it can serve.
    fn desktop(&self, why: &str) -> Result<Arc<dyn Desktop>, Box<Response>> {
        self.desktop
            .clone()
            .ok_or_else(|| Box::new(error(StatusCode::NOT_IMPLEMENTED, why)))
    }

    /// The state as JSON, made once for every open view: on every change
    /// (bursts merged), and at least once a second for progress. Started by
    /// the first view that asks; idle while no view is connected, and
    /// nothing is sent that was sent already.
    fn states(&self) -> watch::Receiver<Arc<str>> {
        self.states
            .get_or_init(|| {
                let (tx, rx) = watch::channel(self.view_json());
                let app = self.clone();
                tokio::spawn(async move {
                    let mut changes = app.engine.watch();
                    loop {
                        // The receiver kept in `states` counts as one: no
                        // view is connected. One that connects takes a fresh
                        // state itself ([`events`]).
                        if tx.receiver_count() <= 1 {
                            tokio::time::sleep(Duration::from_secs(1)).await;
                            continue;
                        }
                        tokio::select! {
                            changed = changes.changed() => if changed.is_err() { return },
                            () = tokio::time::sleep(Duration::from_secs(1)) => {}
                        }
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        changes.borrow_and_update();
                        let next = app.view_json();
                        tx.send_if_modified(|shown| {
                            let changed = **shown != *next;
                            if changed {
                                *shown = next;
                            }
                            changed
                        });
                    }
                });
                rx
            })
            .clone()
    }

    /// What the engine has to tell the person, as JSON for `notice` events:
    /// a burst ([`haul_core::gather`]) told in a few lines, as the
    /// desktop app's notifications are. Downloads that need a person stay a
    /// count in the status bar. Started by the first view that asks.
    fn notices(&self) -> broadcast::Receiver<Arc<str>> {
        self.notices
            .get_or_init(|| {
                let (tx, _) = broadcast::channel(16);
                let mut events = self.engine.subscribe();
                let out = tx.clone();
                tokio::spawn(async move {
                    while let Some(burst) = haul_core::gather(&mut events).await {
                        for told in haul_core::summarize_burst(&burst) {
                            if told.kind != haul_core::Told::NeedsYou {
                                let json = serde_json::to_string(&told).unwrap_or_default();
                                let _ = out.send(json.into());
                            }
                        }
                    }
                });
                tx
            })
            .subscribe()
    }

    fn view_json(&self) -> Arc<str> {
        serde_json::to_string(&self.view())
            .unwrap_or_default()
            .into()
    }

    /// The desktop app's insides, for debug mode and the report; `null`
    /// on the server.
    pub(crate) fn desktop_inspect(&self) -> serde_json::Value {
        self.desktop
            .as_ref()
            .map_or(serde_json::Value::Null, |d| d.debug_inspect())
    }

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
    haul_core::debug::watch_statuses(engine.clone());
    axum::serve(listener, router(engine, desktop, port)).await
}

include!(concat!(env!("OUT_DIR"), "/assets.rs"));

pub fn router(engine: Engine, desktop: Option<Arc<dyn Desktop>>, port: u16) -> Router {
    let app = App {
        engine,
        desktop,
        port,
        states: Arc::default(),
        notices: Arc::default(),
    };
    let mut router = Router::new();
    for &(path, file, content_type, body) in ASSETS {
        router = router.route(path, get(move || page(file, content_type, body)));
        if path == "/icon.svg" {
            router = router.route("/favicon.ico", get(move || page(file, content_type, body)));
        }
    }
    api_routes()
        .into_iter()
        .fold(router, |router, (path, method)| router.route(path, method))
        .merge(debug::routes(&app))
        .layer(middleware::from_fn_with_state(app.clone(), guard))
        .layer(middleware::map_response(json_errors))
        .with_state(app)
}

/// The API's routes, which [`router`] serves and a test finds in
/// `docs/api.md`.
fn api_routes() -> Vec<(&'static str, MethodRouter<App>)> {
    let upload = || DefaultBodyLimit::max(64 << 20);
    vec![
        ("/api/state", get(state)),
        ("/api/events", get(events)),
        ("/api/add", post(add)),
        ("/api/paste", post(paste)),
        ("/api/container", post(container).layer(upload())),
        ("/api/container/read", post(read_container).layer(upload())),
        ("/api/item/{id}/direct", post(direct)),
        ("/api/item/{id}/browse", post(browse)),
        ("/api/item/{id}/reveal", post(reveal)),
        ("/api/item/{id}/extract", post(extract)),
        ("/api/item/{id}/{action}", post(item)),
        ("/api/wrapper/{id}/{action}", post(wrapper)),
        ("/api/items/{action}", post(items)),
        ("/api/wrappers/{action}", post(wrappers)),
        ("/api/wrappers/drop-host", post(drop_wrapper_host)),
        ("/api/host/{name}/folder", post(host_folder)),
        ("/api/host/{name}/preferred", post(host_preferred)),
        ("/api/host/{name}/disabled", post(host_disabled)),
        ("/api/host/{name}/accountless", post(host_accountless)),
        ("/api/preferred-hosts", post(preferred_hosts)),
        ("/api/host/{name}/route/{mode}", post(host_route)),
        ("/api/global/{action}", post(global)),
        ("/api/router", post(find_router)),
        ("/api/reconnect", post(reconnect)),
        ("/api/download-from", post(download_from)),
        ("/api/update/check", post(check_update)),
        ("/api/update/install", post(install_update)),
        ("/api/nordvpn/servers", post(nordvpn_servers)),
        ("/api/settings", get(settings).post(update_settings)),
        ("/api/settings/download-dir/pick", post(pick_download_dir)),
        ("/api/diagnostics", get(diagnostics)),
        ("/api/diagnostics/save", post(save_diagnostics)),
        ("/api/open/{place}", post(open_place)),
    ]
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

/// Debug builds read the UI from `HAUL_UI_DIR` when it is set (a `dist`
/// folder `pnpm build` wrote), so a new build shows on the next reload
/// without rebuilding Haul.
fn ui_from_disk(file: &str) -> Option<Vec<u8>> {
    if !cfg!(debug_assertions) {
        return None;
    }
    let dir = std::env::var_os("HAUL_UI_DIR")?;
    std::fs::read(std::path::Path::new(&dir).join(file)).ok()
}

async fn page(file: &'static str, content_type: &'static str, body: &'static [u8]) -> Response {
    let mut resp = match ui_from_disk(file) {
        Some(bytes) => bytes.into_response(),
        None => body.into_response(),
    };
    let h = resp.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    // No framing: a page that framed the UI could trick clicks on it. Styles
    // may be inline: the toasts and dialogs add theirs from script.
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'self'; img-src 'self' data:; style-src 'self' 'unsafe-inline'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'"),
    );
    resp
}

/// Runs `work`, which blocks (a dialog, a file, a zip), off the async
/// threads.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tokio::task::spawn_blocking(work)
        .await
        .unwrap_or_else(|e| Err(e.to_string()))
}

fn error(status: StatusCode, message: impl Into<String>) -> Response {
    (status, Json(json!({ "error": message.into() }))).into_response()
}

fn ok() -> Response {
    Json(json!({})).into_response()
}

/// What a settings change answers: `ok()`, a 400 for a request that asks
/// for what cannot be, a 500 when the file could not be written.
fn setting(result: Result<(), SettingError>) -> Response {
    match result {
        Ok(()) => ok(),
        Err(SettingError::Invalid(e)) => error(StatusCode::BAD_REQUEST, e),
        Err(SettingError::Write(e)) => error(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

/// What a request with nothing else to say answers: `ok()`, or why it failed.
fn answer(result: Result<(), String>, status: StatusCode) -> Response {
    match result {
        Ok(()) => ok(),
        Err(e) => error(status, e),
    }
}

async fn state(State(app): State<App>) -> Json<View> {
    Json(app.view())
}

/// The full state as `state` events: right away, then as [`App::states`]
/// makes it.
async fn events(State(app): State<App>) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let mut rx = app.states();
    rx.borrow_and_update();
    let now = app.view_json();
    let stream = futures_util::stream::unfold((rx, Some(now)), |(mut rx, now)| async move {
        let data = match now {
            Some(data) => data,
            None => {
                rx.changed().await.ok()?;
                rx.borrow_and_update().clone()
            }
        };
        Some((Ok(Event::default().event("state").data(&*data)), (rx, None)))
    });
    let notices = futures_util::stream::unfold(app.notices(), |mut rx| async move {
        loop {
            match rx.recv().await {
                Ok(data) => {
                    return Some((Ok(Event::default().event("notice").data(&*data)), rx));
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    });
    Sse::new(futures_util::stream::select(stream, notices)).keep_alive(KeepAlive::default())
}

#[derive(Deserialize)]
struct PasteBody {
    text: String,
}

/// A paste outside any field, for the engine to say what it is for.
async fn paste(State(app): State<App>, Json(body): Json<PasteBody>) -> Response {
    match app.engine.paste(&body.text) {
        Ok(pasted) => Json(pasted).into_response(),
        Err(e) => error(StatusCode::BAD_REQUEST, e),
    }
}

#[derive(Deserialize)]
struct AddBody {
    text: String,
    #[serde(default)]
    package: String,
    /// Tried first on the archives among these links' files.
    #[serde(default)]
    password: String,
}

async fn add(State(app): State<App>, Json(body): Json<AddBody>) -> Json<Added> {
    Json(
        app.engine
            .add_text_with(&body.text, body.package.trim(), Some(&body.password)),
    )
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
            Ok(a) => total += a,
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

/// The UI's route to the downloads waiting for the user, for the desktop
/// app to open from a notification. The UI names its views in `route.ts`.
pub const WAITING_ROUTE: &str = "/attention";

/// The UI's route to the wrappers, for the desktop app to open when only
/// wrappers wait for the user.
pub const WRAPPERS_ROUTE: &str = "/wrappers";

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
    answer(
        app.engine
            .set_direct(&id, body.url.trim(), body.headers, body.name),
        StatusCode::BAD_REQUEST,
    )
}

#[derive(Deserialize, Default)]
struct BrowseBody {
    user_agent: Option<String>,
}

async fn browse(State(app): State<App>, Path(id): Path<String>, body: Bytes) -> Response {
    let desktop = match app.desktop("only the desktop app has an in-app browser") {
        Ok(desktop) => desktop,
        Err(answer) => return *answer,
    };
    let Some(url) = app.engine.page_for(&id) else {
        return error(StatusCode::NOT_FOUND, "no such item");
    };
    let body: BrowseBody = serde_json::from_slice(&body).unwrap_or_default();
    answer(
        desktop.browse(&id, &url, body.user_agent),
        StatusCode::INTERNAL_SERVER_ERROR,
    )
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

#[derive(Deserialize)]
struct Ids {
    ids: Vec<String>,
}

/// One action on many items, in one change: `{"ids": […]}` answers
/// `{"done": n, "failed": [{"id", "error"}]}`.
async fn items(
    State(app): State<App>,
    Path(action): Path<String>,
    Json(body): Json<Ids>,
) -> Response {
    let Ok(action) = action.parse::<ItemAction>() else {
        return error(StatusCode::BAD_REQUEST, "unknown action");
    };
    let batch: Batch = match action {
        // May move files to the Recycle Bin, which can fail for some.
        ItemAction::Remove => app.engine.remove_many(&body.ids).await,
        _ => app.engine.items_action(&body.ids, action),
    };
    Json(batch).into_response()
}

/// One action on many wrappers (a card's, or every card's), in one change:
/// `start`, `remove` or `retry`. Answers as `/api/items/{action}` does, with
/// `started` the files started, each counted once.
async fn wrappers(
    State(app): State<App>,
    Path(action): Path<String>,
    Json(body): Json<Ids>,
) -> Response {
    let Ok(action) = action.parse::<WrapperAction>() else {
        return error(StatusCode::BAD_REQUEST, "unknown action");
    };
    Json(app.engine.wrappers_action(&body.ids, action)).into_response()
}

#[derive(Deserialize, Default)]
struct ExtractBody {
    #[serde(default)]
    password: Option<String>,
}

/// Unpacks the archive an item is a part of, with a password for it when
/// the body gives one; a script may send no body at all. Says why when the
/// archive cannot be unpacked yet.
async fn extract(State(app): State<App>, Path(id): Path<String>, body: Bytes) -> Response {
    let body: ExtractBody = if body.iter().all(u8::is_ascii_whitespace) {
        ExtractBody::default()
    } else {
        match serde_json::from_slice(&body) {
            Ok(body) => body,
            Err(e) => return error(StatusCode::BAD_REQUEST, e.to_string()),
        }
    };
    match app.engine.extract(&id, body.password.as_deref()) {
        Ok(true) => ok(),
        Ok(false) => error(StatusCode::NOT_FOUND, "no such item"),
        Err(e) => error(StatusCode::CONFLICT, e),
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

#[derive(Deserialize)]
struct DropHost {
    /// A card's wrappers.
    ids: Vec<String>,
    host: String,
}

/// Takes a host's links off the files a card's wrappers brought.
async fn drop_wrapper_host(State(app): State<App>, Json(body): Json<DropHost>) -> Response {
    let dropped = app.engine.drop_wrapper_host(&body.ids, &body.host);
    Json(json!({ "dropped": dropped })).into_response()
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
    setting(app.engine.set_host_folder(&name, &body.folder))
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
    setting(app.engine.set_preferred(&name, body.preferred))
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

/// Asks GitHub for a newer Haul now, and answers with its version, if any.
async fn check_update(State(app): State<App>) -> Response {
    match app.engine.look_for_update().await {
        Ok(version) => Json(serde_json::json!({ "version": version })).into_response(),
        Err(e) => error(StatusCode::BAD_GATEWAY, e),
    }
}

/// Downloads the newer Haul, checks it, and has the desktop app install it.
async fn install_update(State(app): State<App>) -> Response {
    let desktop = match app.desktop(
        "only the desktop app installs updates; download the new version from its release page",
    ) {
        Ok(desktop) => desktop,
        Err(answer) => return *answer,
    };
    let installer = match app.engine.download_update().await {
        Ok(installer) => installer,
        Err(e) => {
            tracing::warn!("downloading the update: {e}");
            return error(StatusCode::BAD_GATEWAY, e);
        }
    };
    // Windows' permission prompt may wait for the person a while.
    let started = blocking(move || desktop.install_update(&installer)).await;
    answer(
        started.inspect_err(|e| tracing::warn!("installing the update: {e}")),
        StatusCode::INTERNAL_SERVER_ERROR,
    )
}

#[derive(Deserialize)]
struct DownloadFrom {
    ids: Vec<String>,
    /// `null`: back to the usual order.
    host: Option<String>,
}

/// Has downloads go to the host the user picked while it has their file.
async fn download_from(State(app): State<App>, Json(body): Json<DownloadFrom>) -> Response {
    let changed = app.engine.download_from(&body.ids, body.host.as_deref());
    Json(serde_json::json!({ "changed": changed })).into_response()
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
    setting(app.engine.set_disabled(&name, body.disabled))
}

#[derive(Deserialize)]
struct Accountless {
    accountless: bool,
}

/// Has a host that wants its account used like any other without one, or
/// only when no other host has the file again.
async fn host_accountless(
    State(app): State<App>,
    Path(name): Path<String>,
    Json(body): Json<Accountless>,
) -> Response {
    setting(app.engine.set_accountless(&name, body.accountless))
}

#[derive(Deserialize)]
struct PreferredHosts {
    hosts: Vec<String>,
}

/// The preferred hosts, all of them, in a new order.
async fn preferred_hosts(State(app): State<App>, Json(body): Json<PreferredHosts>) -> Response {
    setting(app.engine.set_preferred_hosts(&body.hosts))
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
    setting(app.engine.set_route(&name, route))
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
    setting(app.engine.update_settings(&body))
}

/// Lets the user pick the download folder in the system's folder dialog.
async fn pick_download_dir(State(app): State<App>) -> Response {
    let desktop = match app.desktop("only the desktop app has a folder dialog") {
        Ok(desktop) => desktop,
        Err(answer) => return *answer,
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
    setting(app.engine.update_settings(&change))
}

/// The report, as a zip to save ([`Engine::report_zip`]): what Haul is doing,
/// with account keys and proxy credentials left out, and its logs.
async fn diagnostics(State(app): State<App>) -> Response {
    let engine = app.engine.clone();
    let desktop = app.desktop_inspect();
    match blocking(move || engine.report_zip(desktop)).await {
        Ok((name, body)) => (
            [
                (header::CONTENT_TYPE, "application/zip".to_string()),
                (
                    header::CONTENT_DISPOSITION,
                    format!("attachment; filename=\"{name}\""),
                ),
            ],
            body,
        )
            .into_response(),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

/// Desktop app only (501 elsewhere): asks where to save the report,
/// starting in the download folder, saves it there and shows it.
async fn save_diagnostics(State(app): State<App>) -> Response {
    let desktop = match app
        .desktop("only the desktop app saves the report itself; GET /api/diagnostics instead")
    {
        Ok(desktop) => desktop,
        Err(answer) => return *answer,
    };
    let engine = app.engine.clone();
    let inspect = app.desktop_inspect();
    let saved = blocking(move || -> Result<Option<std::path::PathBuf>, String> {
        // Made first, so it holds what Haul was doing when it was asked for.
        let (name, body) = engine.report_zip(inspect)?;
        let start = engine.download_dir();
        let _ = std::fs::create_dir_all(&start);
        let Some(path) = desktop.pick_save(&start, &name) else {
            return Ok(None);
        };
        std::fs::write(&path, body).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(Some(path))
    })
    .await;
    match saved {
        Ok(Some(path)) => {
            if let Err(e) = os::reveal(&path) {
                tracing::debug!("showing the report: {e}");
            }
            Json(json!({ "saved": path.display().to_string() })).into_response()
        }
        Ok(None) => Json(json!({ "cancelled": true })).into_response(),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

/// Shows a finished file in the system's file manager.
async fn reveal(State(app): State<App>, Path(id): Path<String>) -> Response {
    let Some(path) = app.engine.item_path(&id) else {
        return error(StatusCode::NOT_FOUND, "nothing saved for this item yet");
    };
    answer(
        os::reveal(&path).map_err(|e| format!("{}: {e}", path.display())),
        StatusCode::INTERNAL_SERVER_ERROR,
    )
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
    answer(
        prepared
            .and_then(|()| open::that_detached(&path))
            .map_err(|e| format!("{}: {e}", path.display())),
        StatusCode::INTERNAL_SERVER_ERROR,
    )
}

#[cfg(test)]
mod tests {
    use super::{WAITING_ROUTE, WRAPPERS_ROUTE, add_route, api_routes, debug};

    /// Every route the server answers is in the docs: the API's in
    /// `docs/api.md`, debug mode's in `docs/debugging.md`. A `{name}` in a
    /// route stands for any one segment, which the docs may spell out
    /// (`{id}`, `start`, `{direct\|proxy}`).
    #[test]
    fn every_route_is_documented() {
        // Whether `doc` holds `route` as a path of its own, every `{name}`
        // segment matching one segment there.
        let documented = |doc: &str, route: &str| {
            let want: Vec<&str> = route.split('/').collect();
            doc.split(|c: char| c.is_whitespace() || c == '`')
                .filter(|word| word.starts_with("/api/"))
                .any(|word| {
                    let path = word.split('?').next().unwrap_or_default();
                    let have: Vec<&str> = path.split('/').collect();
                    have.len() == want.len()
                        && have
                            .iter()
                            .zip(&want)
                            .all(|(h, w)| (w.starts_with('{') && !h.is_empty()) || h == w)
                })
        };
        for (routes, doc, name) in [
            (api_routes(), include_str!("../../../docs/api.md"), "api.md"),
            (
                debug::api_routes(),
                include_str!("../../../docs/debugging.md"),
                "debugging.md",
            ),
        ] {
            for (route, _) in routes {
                assert!(documented(doc, route), "{route} is not in docs/{name}");
            }
        }
    }

    /// The UI's own list of its views and routes, which these must match.
    const ROUTES: &str = include_str!("../ui/src/lib/route.ts");

    /// The names in `route.ts`'s `export const <list> = [...]`.
    fn views(list: &str) -> Vec<String> {
        let start = format!("export const {list} = [");
        let line = ROUTES
            .lines()
            .find_map(|l| l.strip_prefix(start.as_str()))
            .unwrap_or_else(|| panic!("route.ts names no {list}"));
        let names = line.split(']').next().unwrap_or_default();
        names
            .split(',')
            .map(|n| n.trim().trim_matches('\'').to_string())
            .collect()
    }

    #[test]
    fn the_routes_the_desktop_app_opens_are_the_uis() {
        let lists = views("LISTS");
        let pages = views("PAGES");
        let view = |route: &str| route.strip_prefix('/').unwrap().to_string();
        assert!(lists.contains(&view(WAITING_ROUTE)), "{lists:?}");
        assert!(pages.contains(&view(WRAPPERS_ROUTE)), "{pages:?}");
        assert!(
            ROUTES.contains(r"#\/add(?:\/(.*))?$"),
            "route.ts follows {}",
            add_route("")
        );
    }

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
