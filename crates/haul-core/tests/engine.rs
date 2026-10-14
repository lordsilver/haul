//! The engine end to end against a local file server reachable under two
//! host names (`127.0.0.1` and `localhost`), so mirror logic sees two hosts.

use std::{
    collections::BTreeMap,
    convert::Infallible,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::{Path as UrlPath, State},
    http::{HeaderMap, Method, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use haul_core::{
    Config, Engine, Event, ItemAction, Options, Route, Snapshot, StartError, config::Endpoints,
};
use serde_json::json;

/// Deterministic, name-dependent bytes, so a mixed-up file shows.
fn content(name: &str, len: usize) -> Vec<u8> {
    let seed = name
        .bytes()
        .fold(7u32, |a, b| a.wrapping_mul(31).wrapping_add(u32::from(b)));
    (0..len)
        .map(|i| (seed.wrapping_add(i as u32).wrapping_mul(2_654_435_761) >> 24) as u8)
        .collect()
}

const SIZE: usize = 600 * 1024;

fn sha256_hex(data: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[derive(Clone, Default)]
struct Server {
    /// "path range" for every file request, in order.
    log: Arc<Mutex<Vec<String>>>,
    flaky: Arc<AtomicUsize>,
    limited: Arc<AtomicUsize>,
    /// Files /busy is sending right now.
    busy: Arc<AtomicUsize>,
}

/// Counts a /busy transfer out when its body is done or dropped.
struct BusyOut(Arc<AtomicUsize>);

impl Drop for BusyOut {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl Server {
    fn requests(&self, path: &str) -> Vec<String> {
        self.log
            .lock()
            .unwrap()
            .iter()
            .filter(|l| l.starts_with(path))
            .cloned()
            .collect()
    }
}

fn range_start(headers: &HeaderMap) -> Option<usize> {
    let v = headers.get(header::RANGE)?.to_str().ok()?;
    v.strip_prefix("bytes=")?.split('-').next()?.parse().ok()
}

/// A file with `Range` support.
fn file_response(data: Vec<u8>, headers: &HeaderMap) -> Response {
    let len = data.len();
    match range_start(headers) {
        Some(start) if start >= len => (
            StatusCode::RANGE_NOT_SATISFIABLE,
            [(header::CONTENT_RANGE, format!("bytes */{len}"))],
        )
            .into_response(),
        Some(start) => (
            StatusCode::PARTIAL_CONTENT,
            [(
                header::CONTENT_RANGE,
                format!("bytes {start}-{}/{len}", len - 1),
            )],
            data[start..].to_vec(),
        )
            .into_response(),
        None => data.into_response(),
    }
}

fn log(s: &Server, path: &str, headers: &HeaderMap) {
    let range = headers
        .get(header::RANGE)
        .map(|v| v.to_str().unwrap().to_string())
        .unwrap_or_default();
    s.log
        .lock()
        .unwrap()
        .push(format!("{path} {range}").trim_end().to_string());
}

async fn start_server() -> (Server, u16) {
    let server = Server::default();
    let app = Router::new()
        .route(
            "/files/{name}",
            get(|State(s): State<Server>, UrlPath(name): UrlPath<String>, headers: HeaderMap| async move {
                log(&s, &format!("/files/{name}"), &headers);
                file_response(content(&name, SIZE), &headers)
            }),
        )
        .route(
            "/flaky/{name}",
            // The first answer promises the whole file and breaks off a third in.
            get(|State(s): State<Server>, method: Method, UrlPath(name): UrlPath<String>, headers: HeaderMap| async move {
                log(&s, &format!("/flaky/{name}"), &headers);
                let data = content(&name, SIZE);
                if method == Method::HEAD || s.flaky.fetch_add(1, Ordering::SeqCst) > 0 {
                    return file_response(data, &headers);
                }
                let head = Bytes::from(data[..SIZE / 3].to_vec());
                // Pause before breaking, so the first third is on the wire.
                let stream = futures_util::stream::unfold(0, move |step| {
                    let head = head.clone();
                    async move {
                        match step {
                            0 => Some((Ok(head), 1)),
                            1 => {
                                tokio::time::sleep(Duration::from_millis(200)).await;
                                Some((Err(std::io::Error::other("connection dropped")), 2))
                            }
                            _ => None,
                        }
                    }
                });
                Response::builder()
                    .header(header::CONTENT_LENGTH, SIZE)
                    .body(Body::from_stream(stream))
                    .unwrap()
            }),
        )
        .route(
            "/slow/{name}",
            get(|State(s): State<Server>, UrlPath(name): UrlPath<String>, headers: HeaderMap| async move {
                log(&s, &format!("/slow/{name}"), &headers);
                let data = content(&name, SIZE);
                let start = range_start(&headers).unwrap_or(0);
                let chunks: Vec<Bytes> = data[start..].chunks(8 * 1024).map(Bytes::copy_from_slice).collect();
                let stream = futures_util::stream::unfold(chunks.into_iter(), |mut it| async move {
                    let chunk = it.next()?;
                    tokio::time::sleep(Duration::from_millis(15)).await;
                    Some((Ok::<_, Infallible>(chunk), it))
                });
                let mut resp = Response::builder().header(header::CONTENT_LENGTH, SIZE - start);
                if start > 0 {
                    resp = resp
                        .status(StatusCode::PARTIAL_CONTENT)
                        .header(header::CONTENT_RANGE, format!("bytes {start}-{}/{SIZE}", SIZE - 1));
                }
                resp.body(Body::from_stream(stream)).unwrap()
            }),
        )
        .route(
            "/busy/{name}",
            // Sends at most two files at once and refuses more, saying why.
            get(|State(s): State<Server>, method: Method, UrlPath(name): UrlPath<String>, headers: HeaderMap| async move {
                log(&s, &format!("/busy/{name}"), &headers);
                let data = content(&name, SIZE);
                if method == Method::HEAD {
                    return file_response(data, &headers);
                }
                if s.busy.fetch_add(1, Ordering::SeqCst) >= 2 {
                    s.busy.fetch_sub(1, Ordering::SeqCst);
                    return (StatusCode::FORBIDDEN, Json(json!({"message": "too many downloads at once"}))).into_response();
                }
                let out = BusyOut(s.busy.clone());
                let start = range_start(&headers).unwrap_or(0);
                let chunks: Vec<Bytes> = data[start..].chunks(16 * 1024).map(Bytes::copy_from_slice).collect();
                let stream = futures_util::stream::unfold((chunks.into_iter(), out), |(mut it, out)| async move {
                    let chunk = it.next()?;
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    Some((Ok::<_, Infallible>(chunk), (it, out)))
                });
                let mut resp = Response::builder().header(header::CONTENT_LENGTH, SIZE - start);
                if start > 0 {
                    resp = resp
                        .status(StatusCode::PARTIAL_CONTENT)
                        .header(header::CONTENT_RANGE, format!("bytes {start}-{}/{SIZE}", SIZE - 1));
                }
                resp.body(Body::from_stream(stream)).unwrap()
            }),
        )
        .route(
            "/refuse/{name}",
            get(|method: Method, UrlPath(name): UrlPath<String>, headers: HeaderMap| async move {
                if method == Method::HEAD {
                    return file_response(content(&name, SIZE), &headers);
                }
                (StatusCode::FORBIDDEN, Json(json!({"success": false, "message": "this link has expired"}))).into_response()
            }),
        )
        .route(
            "/drip/{name}",
            // Sends a third of the file from where it is asked, then drops the
            // connection, until the last third.
            get(|State(s): State<Server>, UrlPath(name): UrlPath<String>, headers: HeaderMap| async move {
                log(&s, &format!("/drip/{name}"), &headers);
                let data = content(&name, SIZE);
                let start = range_start(&headers).unwrap_or(0);
                let end = (start + SIZE / 3).min(SIZE);
                let head = Bytes::from(data[start..end].to_vec());
                let last = end == SIZE;
                let stream = futures_util::stream::unfold(0, move |step| {
                    let head = head.clone();
                    async move {
                        match step {
                            0 => Some((Ok(head), 1)),
                            1 if !last => {
                                tokio::time::sleep(Duration::from_millis(100)).await;
                                Some((Err(std::io::Error::other("connection dropped")), 2))
                            }
                            _ => None,
                        }
                    }
                });
                let mut resp = Response::builder().header(header::CONTENT_LENGTH, SIZE - start);
                if start > 0 {
                    resp = resp
                        .status(StatusCode::PARTIAL_CONTENT)
                        .header(header::CONTENT_RANGE, format!("bytes {start}-{}/{SIZE}", SIZE - 1));
                }
                resp.body(Body::from_stream(stream)).unwrap()
            }),
        )
        .route(
            "/trickle/{name}",
            // About 40 KiB/s: 4 KiB every 100 ms.
            get(|State(s): State<Server>, UrlPath(name): UrlPath<String>, headers: HeaderMap| async move {
                log(&s, &format!("/trickle/{name}"), &headers);
                let data = content(&name, SIZE);
                let start = range_start(&headers).unwrap_or(0);
                let chunks: Vec<Bytes> = data[start..].chunks(4 * 1024).map(Bytes::copy_from_slice).collect();
                let stream = futures_util::stream::unfold(chunks.into_iter(), |mut it| async move {
                    let chunk = it.next()?;
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    Some((Ok::<_, Infallible>(chunk), it))
                });
                let mut resp = Response::builder().header(header::CONTENT_LENGTH, SIZE - start);
                if start > 0 {
                    resp = resp
                        .status(StatusCode::PARTIAL_CONTENT)
                        .header(header::CONTENT_RANGE, format!("bytes {start}-{}/{SIZE}", SIZE - 1));
                }
                resp.body(Body::from_stream(stream)).unwrap()
            }),
        )
        .route(
            "/limited/{name}",
            get(|State(s): State<Server>, UrlPath(name): UrlPath<String>, headers: HeaderMap| async move {
                log(&s, &format!("/limited/{name}"), &headers);
                if s.limited.fetch_add(1, Ordering::SeqCst) < 2 {
                    // Turned away twice; the second time after the first cooldown.
                    return (StatusCode::TOO_MANY_REQUESTS, [(header::RETRY_AFTER, "1")]).into_response();
                }
                file_response(content(&name, SIZE), &headers)
            }),
        )
        .route(
            "/hidden/{key}",
            get(|State(s): State<Server>, UrlPath(key): UrlPath<String>, headers: HeaderMap| async move {
                log(&s, &format!("/hidden/{key}"), &headers);
                let mut resp = file_response(content("same.bin", SIZE), &headers);
                resp.headers_mut().insert(header::CONTENT_DISPOSITION, "attachment; filename=\"same.bin\"".parse().unwrap());
                resp
            }),
        )
        .route(
            "/page/{name}",
            get(|| async { ([(header::CONTENT_TYPE, "text/html")], "<html><title>Please solve the captcha</title></html>") }),
        )
        .route(
            "/api/list/{id}",
            get(|UrlPath(id): UrlPath<String>| async move {
                Json(match id.as_str() {
                    "empty" => json!({"files": []}),
                    // The first file's link is dead; the same name lives elsewhere.
                    "broken" => json!({"files": [
                        {"id": "missing-1", "name": "keep.bin", "size": SIZE},
                        {"id": "other.rar", "name": "other.rar", "size": SIZE}
                    ]}),
                    _ => json!({"files": [
                        {"id": "first.rar", "name": "first.rar", "size": SIZE},
                        {"id": "second.rar", "name": "second.rar", "size": SIZE}
                    ]}),
                })
            }),
        )
        .route(
            "/api/file/{id}/info",
            // Publishes the SHA-256 of what /api/file/{id} serves.
            get(|State(s): State<Server>, UrlPath(id): UrlPath<String>, headers: HeaderMap| async move {
                log(&s, &format!("/api/file/{id}/info"), &headers);
                Json(json!({"name": format!("{id}.bin"), "size": SIZE, "hash_sha256": sha256_hex(&content(&id, SIZE))}))
            }),
        )
        .route(
            "/api/file/{id}",
            get(|State(s): State<Server>, UrlPath(id): UrlPath<String>, headers: HeaderMap| async move {
                log(&s, &format!("/api/file/{id}"), &headers);
                if id.starts_with("missing") {
                    return StatusCode::NOT_FOUND.into_response();
                }
                file_response(content(&id, SIZE), &headers)
            }),
        )
        .with_state(server.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (server, port)
}

struct Setup {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    server: Server,
    ip: String,
    name: String,
}

impl Setup {
    async fn new() -> Self {
        let (server, port) = start_server().await;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        Self {
            _tmp: tmp,
            root,
            server,
            ip: format!("http://127.0.0.1:{port}"),
            name: format!("http://localhost:{port}"),
        }
    }

    fn config(&self) -> Config {
        Config {
            download_dir: Some(self.root.join("dl")),
            endpoints: Endpoints {
                pixeldrain: self.ip.clone(),
                ..Endpoints::default()
            },
            ..Config::default()
        }
    }

    fn options(&self) -> Options {
        Options {
            data_dir: self.root.join("data"),
            download_dir: None,
            config: Some(self.config()),
            recycle_bin: Some(self.root.join("bin")),
        }
    }

    async fn engine(&self) -> Engine {
        Engine::start(self.options()).await.unwrap()
    }

    fn downloaded(&self, rel: &str) -> Vec<u8> {
        std::fs::read(self.root.join("dl").join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
    }

    fn partial(&self, id: &str) -> PathBuf {
        self.root
            .join("dl")
            .join(".partial")
            .join(format!("{id}.part"))
    }
}

async fn wait_for(engine: &Engine, what: &str, f: impl Fn(&Snapshot) -> bool) -> Snapshot {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let s = engine.snapshot();
        if f(&s) {
            return s;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {what}: {s:#?}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

fn all_done(s: &Snapshot) -> bool {
    !s.items.is_empty() && s.items.iter().all(|i| i.status == "done")
}

#[tokio::test]
async fn downloads_a_direct_link() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    let added = engine.add_text(&format!("get {}/files/a.bin please", t.ip), "");
    assert_eq!(added.added, 1);
    let s = wait_for(&engine, "done", all_done).await;
    assert_eq!(t.downloaded("a.bin"), content("a.bin", SIZE));
    assert_eq!(
        (s.items[0].size, s.items[0].done),
        (SIZE as u64, SIZE as u64)
    );
    let host = s.hosts.iter().find(|h| h.name == "127.0.0.1").unwrap();
    assert_eq!((host.ok, host.fails, host.active), (1, 0, 0));
    assert!(
        !t.root.join("dl/.partial").read_dir().unwrap().any(|_| true),
        "no partial files left"
    );

    let again = engine.add_text(&format!("{}/files/a.bin", t.ip), "");
    assert_eq!((again.added, again.duplicates), (0, 1));
    assert_eq!(engine.add_text("ftp://nope.example/x.bin", "").added, 0);
}

#[tokio::test]
async fn resumes_after_a_broken_connection() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.add_text(&format!("{}/flaky/b.bin", t.ip), "");
    let s = wait_for(&engine, "a retry", |s| {
        s.items[0].status == "waiting" && !s.items[0].error.is_empty()
    })
    .await;
    assert!(
        !s.items[0].error.contains("trying again"),
        "the countdown is the views' job: {}",
        s.items[0].error
    );
    wait_for(&engine, "done", all_done).await;
    assert_eq!(t.downloaded("b.bin"), content("b.bin", SIZE));
    let gets = t.server.requests("/flaky/b.bin");
    assert_eq!(
        gets.last().unwrap(),
        &format!("/flaky/b.bin bytes={}-", SIZE / 3),
        "{gets:?}"
    );
}

#[tokio::test]
async fn merges_mirrors_by_name_and_fails_over() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    let added = engine.add_text(
        &format!("{0}/missing/c.bin\n{1}/files/c.bin", t.ip, t.name),
        "pkg",
    );
    assert_eq!((added.added, added.merged), (1, 1));
    let s = engine.snapshot();
    assert_eq!(s.items.len(), 1);
    assert_eq!(s.items[0].mirrors, ["127.0.0.1", "localhost"]);
    let s = wait_for(&engine, "done", all_done).await;
    assert_eq!(t.downloaded("pkg/c.bin"), content("c.bin", SIZE));
    let ip = s.hosts.iter().find(|h| h.name == "127.0.0.1").unwrap();
    let name = s.hosts.iter().find(|h| h.name == "localhost").unwrap();
    assert_eq!((ip.fails, name.ok), (1, 1));
}

#[tokio::test]
async fn probe_merges_links_that_hide_the_name() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.set_paused(true);
    let added = engine.add_text(&format!("{}/hidden/one {}/hidden/two", t.ip, t.name), "");
    assert_eq!(added.added, 2, "nothing to merge on before probing");
    let s = wait_for(&engine, "probes", |s| {
        s.items.iter().all(|i| i.status != "checking")
    })
    .await;
    assert_eq!(s.items.len(), 1, "{s:#?}");
    assert_eq!(
        (s.items[0].name.as_str(), s.items[0].size),
        ("same.bin", SIZE as u64)
    );
    assert_eq!(s.items[0].mirrors.len(), 2);
    assert!(
        t.server
            .requests("/hidden")
            .iter()
            .all(|r| r.ends_with("bytes=0-65535")),
        "only probes so far"
    );
    engine.set_paused(false);
    wait_for(&engine, "done", all_done).await;
    assert_eq!(t.downloaded("same.bin"), content("same.bin", SIZE));
    let names: Vec<_> = std::fs::read_dir(t.root.join("dl"))
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(
        names
            .iter()
            .filter(|n| n.to_string_lossy().ends_with(".bin"))
            .count(),
        1,
        "{names:?}"
    );
}

#[tokio::test]
async fn rate_limited_host_cools_down() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.add_text(&format!("{}/limited/d.bin", t.ip), "");
    let s = wait_for(&engine, "cooldown", |s| s.items[0].status == "waiting").await;
    assert!(s.items[0].error.contains("429"), "{}", s.items[0].error);
    wait_for(&engine, "done", all_done).await;
    assert_eq!(t.downloaded("d.bin"), content("d.bin", SIZE));
}

#[tokio::test]
async fn page_needs_a_person_then_their_link_is_used() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    let page = format!("{}/page/getfile", t.ip);
    engine.add_text(&page, "");
    let s = wait_for(&engine, "captcha", |s| s.items[0].status == "captcha").await;
    assert_eq!(s.items[0].open_urls, std::slice::from_ref(&page));
    assert_eq!(
        engine.page_for(&s.items[0].id).as_deref(),
        Some(page.as_str())
    );

    let id = s.items[0].id.clone();
    assert!(
        engine
            .set_direct(&id, "javascript:alert(1)", BTreeMap::new(), None)
            .is_err()
    );
    let headers = BTreeMap::from([("Referer".to_string(), page.clone())]);
    engine
        .set_direct(&id, &format!("{}/files/e.bin", t.ip), headers, None)
        .unwrap();
    wait_for(&engine, "done", all_done).await;
    assert_eq!(t.downloaded("e.bin"), content("e.bin", SIZE));
}

#[tokio::test]
async fn pause_resume_and_remove() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.add_text(&format!("{0}/slow/f.bin {0}/slow/g.bin", t.ip), "");
    let s = wait_for(&engine, "progress", |s| s.items.iter().all(|i| i.done > 0)).await;
    let (f, g) = (s.items[0].id.clone(), s.items[1].id.clone());

    assert!(engine.item_action(&f, ItemAction::Pause));
    // Paused once its attempt has let go of the host slot and the file.
    let s = wait_for(&engine, "pause", |s| {
        s.items[0].status == "paused" && s.hosts[0].active == 1
    })
    .await;
    let kept = std::fs::metadata(t.partial(&f)).unwrap().len();
    assert!(kept > 0 && kept < SIZE as u64, "{kept}");
    assert!(s.items[0].done >= kept);

    assert!(engine.item_action(&g, ItemAction::Remove));
    wait_for(&engine, "removal", |s| {
        s.items.len() == 1 && s.hosts[0].active == 0
    })
    .await;
    // The attempt deletes it right after giving its slot back.
    let deadline = Instant::now() + Duration::from_secs(5);
    while t.partial(&g).exists() {
        assert!(
            Instant::now() < deadline,
            "a removed row's partial file goes"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    assert!(engine.item_action(&f, ItemAction::Resume));
    wait_for(&engine, "done", all_done).await;
    assert_eq!(t.downloaded("f.bin"), content("f.bin", SIZE));
    let gets = t.server.requests("/slow/f.bin");
    assert!(
        gets.iter().any(|r| r.contains(&format!("bytes={kept}-"))),
        "{gets:?} resumed at {kept}"
    );
}

#[tokio::test]
async fn folder_expands_in_place() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.set_paused(true);
    engine.add_text(
        &format!(
            "{0}/files/before.bin https://pixeldrain.com/l/list1 {0}/files/after.bin",
            t.ip
        ),
        "",
    );
    engine.set_paused(false);
    let s = wait_for(&engine, "done", |s| s.items.len() == 4 && all_done(s)).await;
    let names: Vec<_> = s.items.iter().map(|i| i.name.as_str()).collect();
    assert_eq!(
        names,
        ["before.bin", "first.rar", "second.rar", "after.bin"]
    );
    assert_eq!(t.downloaded("second.rar"), content("second.rar", SIZE));
}

/// Starts from `config.toml` in the data folder, as the apps do.
async fn start_from_file(t: &Setup) -> Engine {
    start_when_free(|| Options {
        config: None,
        ..t.options()
    })
    .await
}

/// Starts once the previous engine on the same data folder has let go of it.
async fn start_when_free(options: impl Fn() -> Options) -> Engine {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match Engine::start(options()).await {
            Ok(e) => return e,
            // The previous engine's tasks are still letting go of the lock.
            Err(StartError::Locked(_)) if Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(20)).await
            }
            Err(e) => panic!("{e}"),
        }
    }
}

#[tokio::test]
async fn queue_and_settings_survive_restart_and_a_second_instance_is_refused() {
    let t = Setup::new().await;
    std::fs::create_dir_all(t.root.join("data")).unwrap();
    // A route choice saved by an older version.
    std::fs::write(
        t.root.join("data/routes.json"),
        r#"{"old.example":"direct"}"#,
    )
    .unwrap();
    let engine = start_from_file(&t).await;
    assert!(matches!(
        Engine::start(t.options()).await,
        Err(StartError::Locked(_))
    ));

    engine.set_paused(true);
    engine.add_text(&format!("{}/files/h.bin", t.ip), "keep");
    let id = engine.snapshot().items[0].id.clone();
    engine.item_action(&id, ItemAction::Pause);
    engine.set_route("127.0.0.1", Route::Direct).unwrap();
    engine.set_max_total(3);
    engine.set_host_limit("127.0.0.1", 2);
    engine.shutdown().await;
    drop(engine);

    let engine = start_from_file(&t).await;
    let s = engine.snapshot();
    assert_eq!(s.items.len(), 1);
    assert_eq!(
        (
            s.items[0].id.as_str(),
            s.items[0].status,
            s.items[0].package.as_str()
        ),
        (id.as_str(), "paused", "keep")
    );
    assert_eq!(s.max_total, 3);
    let cfg = engine.cfg();
    assert_eq!(cfg.routes.get("127.0.0.1"), Some(&Route::Direct));
    assert_eq!(
        cfg.routes.get("old.example"),
        Some(&Route::Direct),
        "migrated"
    );
    assert_eq!(cfg.limit_for("127.0.0.1"), 2);
    assert!(!t.root.join("data/routes.json").exists());
    let file = std::fs::read_to_string(t.root.join("data/config.toml")).unwrap();
    assert!(file.starts_with("# Haul settings"), "{file}");
}

#[tokio::test]
async fn edits_to_the_settings_file_apply_while_running() {
    let t = Setup::new().await;
    std::fs::create_dir_all(t.root.join("data")).unwrap();
    let path = t.root.join("data/config.toml");
    let engine = start_from_file(&t).await;
    let version = engine.snapshot().config_version;

    std::fs::write(
        &path,
        "max_total = 2\nrate_limit_mbps = 1.5\ntheme = \"dark\"\n",
    )
    .unwrap();
    let mut events = engine.subscribe();
    let s = wait_for(&engine, "the edit", |s| s.max_total == 2).await;
    assert_eq!((s.rate, s.theme), (1.5e6, haul_core::Theme::Dark));
    assert!(s.config_version > version);
    let theme = tokio::time::timeout(Duration::from_secs(5), events.recv()).await;
    assert!(
        matches!(theme, Ok(Ok(Event::ThemeChanged(haul_core::Theme::Dark)))),
        "{theme:?}"
    );

    // A broken file leaves the settings alone and says why.
    std::fs::write(&path, "max_total = \"many\"\n").unwrap();
    let s = wait_for(&engine, "the error", |s| s.config_error.is_some()).await;
    assert_eq!(s.max_total, 2);
    assert!(s.config_error.unwrap().contains("max_total"));
    std::fs::write(&path, "max_total = 5\n").unwrap();
    let s = wait_for(&engine, "the fix", |s| s.max_total == 5).await;
    assert_eq!(s.config_error, None);
}

#[tokio::test]
async fn a_new_download_folder_takes_the_partial_files_along() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.add_text(&format!("{}/slow/n.bin", t.ip), "");
    wait_for(&engine, "progress", |s| s.items[0].done > 0).await;
    let moved = t.root.join("elsewhere");
    let changes = serde_json::json!({"download_dir": moved.display().to_string()});
    engine
        .update_settings(changes.as_object().unwrap())
        .unwrap();
    let s = wait_for(&engine, "done", all_done).await;
    assert_eq!(s.download_dir, moved.display().to_string());
    assert_eq!(
        std::fs::read(moved.join("n.bin")).unwrap(),
        content("n.bin", SIZE)
    );
    let gets = t.server.requests("/slow/n.bin");
    assert!(
        gets.iter()
            .any(|r| r.contains("bytes=") && !r.ends_with("bytes=0-")),
        "resumed rather than restarted: {gets:?}"
    );
    let relative = serde_json::json!({"download_dir": "relative/dir"});
    assert!(
        engine
            .update_settings(relative.as_object().unwrap())
            .is_err()
    );
}

#[tokio::test]
async fn failed_downloads_retry_on_their_own() {
    let t = Setup::new().await;
    let mut config = t.config();
    config.retry_failed_minutes = 0;
    let engine = Engine::start(Options {
        config: Some(config),
        ..t.options()
    })
    .await
    .unwrap();
    engine.add_text(&format!("{}/missing/o.bin", t.ip), "");
    let s = wait_for(&engine, "failure", |s| s.items[0].status == "error").await;
    assert_eq!(s.items[0].retry_in, 0, "turned off");

    let on = serde_json::json!({"retry_failed_minutes": 1});
    engine.update_settings(on.as_object().unwrap()).unwrap();
    let s = engine.snapshot();
    assert!(
        (50..=60).contains(&s.items[0].retry_in),
        "{}",
        s.items[0].retry_in
    );
}

#[tokio::test]
async fn global_speed_limit_slows_every_transfer() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.set_rate_mbps(1.0);
    let started = Instant::now();
    engine.add_text(
        &format!("{0}/files/i.bin {1}/files/j.bin", t.ip, t.name),
        "",
    );
    wait_for(&engine, "done", all_done).await;
    // 1.2 MB in total under a 1 MB/s cap.
    assert!(
        started.elapsed() >= Duration::from_millis(900),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(engine.snapshot().rate, 1e6);
}

#[tokio::test]
async fn imports_a_text_container() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.set_paused(true);
    let text = format!("{0}/files/k.bin\r\n{0}/files/l.bin\r\nnot a link", t.ip);
    let added = engine.import("links.txt", text.as_bytes()).await.unwrap();
    assert_eq!(added.added, 2);
    let err = engine.import("broken.dlc", b"nope").await.unwrap_err();
    assert!(err.contains("not a DLC"), "{err}");
}

#[tokio::test]
async fn an_empty_folder_fails_instead_of_vanishing() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.add_text("https://pixeldrain.com/l/empty", "");
    let s = wait_for(&engine, "failure", |s| {
        s.items.first().is_some_and(|i| i.status == "error")
    })
    .await;
    assert!(
        s.items[0].error.contains("nothing to download"),
        "{}",
        s.items[0].error
    );
}

#[tokio::test]
async fn pause_all_pauses_running_transfers() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.add_text(&format!("{}/slow/m.bin", t.ip), "");
    wait_for(&engine, "progress", |s| s.items[0].done > 0).await;
    engine.set_paused(true);
    let s = wait_for(&engine, "a stop", |s| s.hosts[0].active == 0).await;
    assert_eq!(
        s.items[0].status, "queued",
        "it resumes with everything else"
    );
    let kept = s.items[0].done;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        engine.snapshot().items[0].done,
        kept,
        "nothing moves while paused"
    );
    engine.set_paused(false);
    wait_for(&engine, "done", all_done).await;
    assert_eq!(t.downloaded("m.bin"), content("m.bin", SIZE));
}

#[tokio::test]
async fn a_folder_row_fails_over_without_reusing_the_folder_link() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.set_max_total(1);
    engine.set_paused(true);
    engine.add_text(
        &format!("https://pixeldrain.com/l/broken {}/files/keep.bin", t.ip),
        "",
    );
    wait_for(&engine, "probes", |s| {
        s.items.iter().all(|i| i.status != "checking")
    })
    .await;
    engine.set_paused(false);
    let s = wait_for(&engine, "done", |s| s.items.len() == 2 && all_done(s)).await;
    let keep = s.items.iter().find(|i| i.name == "keep.bin").unwrap();
    assert_eq!(keep.mirrors, ["pixeldrain.com", "127.0.0.1"]);
    assert_eq!(t.downloaded("keep.bin"), content("keep.bin", SIZE));
    let dead = t.server.requests("/api/file/missing-1");
    assert_eq!(
        dead.len(),
        1,
        "the dead folder link is tried once: {dead:?}"
    );
}

#[tokio::test]
async fn host_folders_and_the_recycle_bin() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine
        .set_host_folder("localhost", " Hosts/../local:host ")
        .unwrap();
    engine.add_text(
        &format!(
            "{ip}/files/a.bin {name}/files/b.bin {name}/files/c.bin {name}/files/d.bin",
            ip = t.ip,
            name = t.name
        ),
        "",
    );
    let s = wait_for(&engine, "done", |s| s.items.len() == 4 && all_done(s)).await;
    let folder = std::path::Path::new("Hosts").join("local_host");
    assert_eq!(t.downloaded("a.bin"), content("a.bin", SIZE));
    assert_eq!(
        t.downloaded(&folder.join("b.bin").to_string_lossy()),
        content("b.bin", SIZE)
    );
    let local = s.hosts.iter().find(|h| h.name == "localhost").unwrap();
    assert_eq!(local.folder, folder.display().to_string());
    let id = |name: &str| s.items.iter().find(|i| i.name == name).unwrap().id.clone();
    let dl = t.root.join("dl");
    let bin = t.root.join("bin");

    // Off by default: removing a finished download only clears its row.
    assert!(engine.remove(&id("a.bin")).await.unwrap());
    assert!(dl.join("a.bin").is_file());

    // On: the file goes to the bin first; Clear completed never touches files.
    let on = json!({"recycle_on_remove": true});
    engine.update_settings(on.as_object().unwrap()).unwrap();
    assert!(engine.remove(&id("b.bin")).await.unwrap());
    assert!(!dl.join(&folder).join("b.bin").exists());
    assert_eq!(
        std::fs::read(bin.join("b.bin")).unwrap(),
        content("b.bin", SIZE)
    );
    // A file the user moved away already: the row goes, nothing else happens.
    std::fs::remove_file(dl.join(&folder).join("c.bin")).unwrap();
    assert!(engine.remove(&id("c.bin")).await.unwrap());
    assert!(!bin.join("c.bin").exists());
    engine.clear_done();
    assert!(dl.join(&folder).join("d.bin").is_file());
    assert!(engine.snapshot().items.is_empty());
    assert!(!engine.remove("no-such-id").await.unwrap());

    // An empty folder puts the host's files back in the download folder.
    engine.set_host_folder("localhost", " ").unwrap();
    engine.add_text(&format!("{}/files/e.bin", t.name), "");
    wait_for(&engine, "done", all_done).await;
    assert_eq!(t.downloaded("e.bin"), content("e.bin", SIZE));
}

#[tokio::test]
async fn preferred_hosts_go_first_and_are_waited_for() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.set_preferred("localhost", true).unwrap();
    engine.set_host_limit("localhost", 1);
    let both = |name: &str| format!("{}/slow/{name} {}/slow/{name}", t.ip, t.name);
    engine.add_text(&format!("{} {}", both("q1.bin"), both("q2.bin")), "");
    let s = wait_for(&engine, "done", |s| s.items.len() == 2 && all_done(s)).await;
    let ok = |name: &str| s.hosts.iter().find(|h| h.name == name).map_or(0, |h| h.ok);
    assert_eq!(
        (ok("localhost"), ok("127.0.0.1")),
        (2, 0),
        "the second file waited for the preferred host's only slot: {:#?}",
        s.hosts
    );
    assert_eq!(
        (s.hosts[0].name.as_str(), s.hosts[0].preferred),
        ("localhost", Some(0)),
        "listed first"
    );
    assert_eq!(t.downloaded("q2.bin"), content("q2.bin", SIZE));

    engine.set_preferred("localhost", false).unwrap();
    assert!(
        engine
            .snapshot()
            .hosts
            .iter()
            .all(|h| h.preferred.is_none())
    );
}

#[tokio::test]
async fn host_speeds_survive_a_restart() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.add_text(&format!("{}/slow/s.bin", t.ip), "");
    wait_for(&engine, "done", all_done).await;
    let host = |e: &Engine| {
        e.snapshot()
            .hosts
            .into_iter()
            .find(|h| h.name == "127.0.0.1")
    };
    let before = host(&engine).unwrap();
    assert_eq!((before.ok, before.bytes), (1, SIZE as u64));
    engine.shutdown().await;
    drop(engine);

    let engine = start_when_free(|| t.options()).await;
    let after = host(&engine).expect("remembered");
    assert_eq!((after.ok, after.bytes), (before.ok, before.bytes));
    assert!(
        (after.speed - before.speed).abs() < 1.0,
        "{} then {}",
        before.speed,
        after.speed
    );
}

/// The files in the download folder, without the partial-file folder.
fn files_in(dir: &std::path::Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|n| n != ".partial")
        .collect();
    names.sort();
    names
}

#[tokio::test]
async fn a_downloaded_file_is_not_fetched_again() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    let link = format!("{}/files/again.bin", t.ip);
    engine.add_text(&link, "");
    let s = wait_for(&engine, "done", all_done).await;
    let first = s.items[0].path.clone().unwrap();
    assert!(!s.items[0].existing);
    assert!(engine.remove(&s.items[0].id).await.unwrap());
    let before = t.server.requests("/files/again.bin").len();

    engine.add_text(&link, "");
    let s = wait_for(&engine, "done", all_done).await;
    assert_eq!(
        (
            s.items[0].existing,
            s.items[0].path.as_deref(),
            s.items[0].done
        ),
        (true, Some(first.as_str()), SIZE as u64)
    );
    // One HEAD request learned the size; a download would have made
    // another request and a second file, `again (1).bin`.
    assert_eq!(t.server.requests("/files/again.bin").len(), before + 1);
    assert_eq!(files_in(&t.root.join("dl")), ["again.bin"]);

    // Recycling removed files covers only files a row downloaded itself.
    let on = json!({"recycle_on_remove": true});
    engine.update_settings(on.as_object().unwrap()).unwrap();
    assert!(engine.remove(&s.items[0].id).await.unwrap());
    assert_eq!(t.downloaded("again.bin"), content("again.bin", SIZE));
}

#[tokio::test]
async fn a_different_file_by_the_same_name_is_numbered() {
    let t = Setup::new().await;
    std::fs::create_dir_all(t.root.join("dl")).unwrap();
    std::fs::write(t.root.join("dl/other.bin"), "not the same").unwrap();
    let engine = t.engine().await;
    engine.add_text(&format!("{}/files/other.bin", t.ip), "");
    let s = wait_for(&engine, "done", all_done).await;
    assert!(!s.items[0].existing);
    assert_eq!(t.downloaded("other (1).bin"), content("other.bin", SIZE));
    assert_eq!(t.downloaded("other.bin"), b"not the same");
}

#[tokio::test]
async fn a_published_checksum_decides() {
    let t = Setup::new().await;
    std::fs::create_dir_all(t.root.join("dl")).unwrap();
    // The host's bytes, and other bytes of the same size under the same name.
    std::fs::write(t.root.join("dl/kept.bin"), content("kept", SIZE)).unwrap();
    std::fs::write(t.root.join("dl/twin.bin"), content("not the twin", SIZE)).unwrap();
    let engine = t.engine().await;
    engine.add_text(
        "https://pixeldrain.com/u/kept https://pixeldrain.com/u/twin",
        "",
    );
    let s = wait_for(&engine, "done", |s| s.items.len() == 2 && all_done(s)).await;
    let existing = |name: &str| s.items.iter().find(|i| i.name == name).unwrap().existing;
    assert!(existing("kept.bin"));
    assert!(!existing("twin.bin"));
    assert!(
        t.server
            .requests("/api/file/kept")
            .iter()
            .all(|r| r.starts_with("/api/file/kept/info")),
        "kept.bin is not downloaded: {:?}",
        t.server.requests("/api/file/kept")
    );
    assert_eq!(t.downloaded("twin (1).bin"), content("twin", SIZE));
    assert_eq!(
        files_in(&t.root.join("dl")),
        ["kept.bin", "twin (1).bin", "twin.bin"]
    );
}

#[tokio::test]
async fn a_busy_host_is_capped_instead_of_failing() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    let links: Vec<String> = (1..=4).map(|i| format!("{}/busy/b{i}.bin", t.ip)).collect();
    engine.add_text(&links.join(" "), "");
    let host = |s: &Snapshot| {
        s.hosts
            .iter()
            .find(|h| h.name == "127.0.0.1")
            .map(|h| h.cap)
    };
    let s = wait_for(&engine, "the cap", |s| host(s) == Some(Some(2))).await;
    assert!(
        s.items
            .iter()
            .any(|i| i.status == "waiting" && i.error.contains("at a time")),
        "{:#?}",
        s.items
    );
    let s = wait_for(&engine, "done", |s| s.items.len() == 4 && all_done(s)).await;
    for i in 1..=4 {
        let name = format!("b{i}.bin");
        assert_eq!(t.downloaded(&name), content(&name, SIZE));
    }
    let h = s.hosts.iter().find(|h| h.name == "127.0.0.1").unwrap();
    assert_eq!(
        (h.cap, h.fails),
        (Some(2), 0),
        "never below what it sent at once"
    );

    // The user's own limit replaces what was learned.
    engine.set_host_limit("127.0.0.1", 6);
    let h = engine
        .snapshot()
        .hosts
        .into_iter()
        .find(|h| h.name == "127.0.0.1")
        .unwrap();
    assert_eq!((h.limit, h.cap), (6, None));
}

#[tokio::test]
async fn a_refusal_says_why() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.add_text(&format!("{}/refuse/r.bin", t.ip), "");
    let s = wait_for(&engine, "the failure", |s| s.items[0].status == "error").await;
    assert!(
        s.items[0].error.contains("HTTP 403: this link has expired"),
        "{}",
        s.items[0].error
    );
}

#[tokio::test]
async fn progress_starts_the_retry_count_over() {
    let t = Setup::new().await;
    // One retry only: each break would fail the download, were it not for the
    // data that arrived before it.
    let engine = Engine::start(Options {
        config: Some(Config {
            resume_attempts: 1,
            ..t.config()
        }),
        ..t.options()
    })
    .await
    .unwrap();
    engine.add_text(&format!("{}/drip/d.bin", t.ip), "");
    wait_for(&engine, "done", all_done).await;
    assert_eq!(t.downloaded("d.bin"), content("d.bin", SIZE));
    let resumed = t
        .server
        .requests("/drip/d.bin")
        .iter()
        .filter(|r| r.contains("bytes="))
        .count();
    assert_eq!(resumed, 2, "{:?}", t.server.requests("/drip/d.bin"));
}

#[tokio::test]
async fn clearing_failed_downloads_keeps_the_rest() {
    let t = Setup::new().await;
    let engine = Engine::start(Options {
        config: Some(Config {
            retry_failed_minutes: 0,
            ..t.config()
        }),
        ..t.options()
    })
    .await
    .unwrap();
    engine.add_text(
        &format!(
            "{0}/files/kept.bin {0}/api/file/missing-a {0}/api/file/missing-b",
            t.ip
        ),
        "",
    );
    wait_for(&engine, "two failures and one file", |s| {
        s.items.len() == 3
            && s.items.iter().filter(|i| i.status == "error").count() == 2
            && s.items.iter().any(|i| i.status == "done")
    })
    .await;
    engine.clear_failed();
    let s = engine.snapshot();
    assert_eq!(
        s.items.iter().map(|i| i.status).collect::<Vec<_>>(),
        ["done"],
        "only the finished download is left"
    );
}

#[tokio::test]
async fn a_slow_mirror_gives_way() {
    let t = Setup::new().await;
    let engine = Engine::start(Options {
        config: Some(Config {
            min_speed_kbps: 100,
            min_speed_seconds: 1,
            retry_failed_minutes: 0,
            ..t.config()
        }),
        ..t.options()
    })
    .await
    .unwrap();
    // The slow host first, so it has to give way to the fast one.
    engine.set_preferred("127.0.0.1", true).unwrap();
    engine.add_text(
        &format!("{}/trickle/t.bin {}/files/t.bin", t.ip, t.name),
        "",
    );
    wait_for(&engine, "done", all_done).await;
    assert_eq!(t.downloaded("t.bin"), content("t.bin", SIZE));
    assert!(
        !t.server.requests("/trickle/t.bin").is_empty(),
        "the slow host was tried"
    );
    assert!(
        t.server
            .requests("/files/t.bin")
            .iter()
            .any(|r| r.contains("bytes=")),
        "the fast host resumed what the slow one had sent: {:?}",
        t.server.requests("/files/t.bin")
    );

    // With no other mirror it fails, saying why.
    engine.add_text(&format!("{}/trickle/u.bin", t.ip), "");
    let s = wait_for(&engine, "the failure", |s| {
        s.items
            .iter()
            .any(|i| i.name == "u.bin" && i.status == "error")
    })
    .await;
    let error = &s.items.iter().find(|i| i.name == "u.bin").unwrap().error;
    assert!(error.contains("slower than 100 KB/s for 1s"), "{error}");
}
