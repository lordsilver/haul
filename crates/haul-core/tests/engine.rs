//! The engine end to end against a local file server reachable under two
//! host names (`127.0.0.1` and `localhost`), so mirror logic sees two hosts.

use std::{
    collections::{BTreeMap, HashMap},
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
    extract::{Form, Path as UrlPath, Query, State},
    http::{HeaderMap, Method, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use haul_core::{
    Config, Engine, Event, Extract, Found, ItemAction, Options, Route, Snapshot, StartError,
    Verifying, config::Endpoints,
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

/// filecrypt.cc's checkbox, as the test server's container pages show it.
const FC_GATE: &str = r#"<h2>Security Check</h2><form id="cform">
  <div class="pow-captcha" id="pow-captcha" data-session="/captchasession/T1.json" data-px="">
    <input type="hidden" name="pow_id" value=""></div></form>"#;

/// A solved container page: its links as Click'n'Load data, two of the test
/// server's own files.
fn fc_solved(host: &str) -> String {
    use aes::cipher::{BlockModeEncrypt, KeyIvInit, block_padding::NoPadding};
    use base64::{Engine as _, engine::general_purpose::STANDARD};

    let key = *b"6229761275967424";
    let plain = format!("http://{host}/files/one.bin\r\nhttp://{host}/files/two.bin");
    let mut buf = plain.into_bytes();
    let len = buf.len().div_ceil(16) * 16;
    buf.resize(len, 0);
    let crypted = STANDARD.encode(
        cbc::Encryptor::<aes::Aes128>::new_from_slices(&key, &key)
            .unwrap()
            .encrypt_padded::<NoPadding>(&mut buf, len)
            .unwrap(),
    );
    let jk: String = key.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "<h2>Open Release</h2><form onsubmit=\"CNLPOP('X1', '{jk}', '{crypted}', 'Open Release - filecrypt.cc'); return false;\">"
    )
}

/// The `.dlc` the test server hands out and the key service's answer for it,
/// the way JDownloader builds one: two of the server's own files.
fn dlc_fixture(host: &str) -> (Vec<u8>, String) {
    use aes::cipher::{BlockModeEncrypt, KeyIvInit, block_padding::NoPadding};
    use base64::{Engine as _, engine::general_purpose::STANDARD};

    let (key, iv) = (b"cb99b5cbc24db398", b"9bc24cb995cb8db3");
    let encrypt = |key: &[u8], iv: &[u8], plain: &[u8]| {
        let mut buf = plain.to_vec();
        buf.resize(plain.len().div_ceil(16) * 16, 0);
        let len = buf.len();
        cbc::Encryptor::<aes::Aes128>::new_from_slices(key, iv)
            .unwrap()
            .encrypt_padded::<NoPadding>(&mut buf, len)
            .unwrap()
            .to_vec()
    };
    let e = |s: &str| STANDARD.encode(s);
    let xml = format!(
        r#"<dlc><header/><content><package name="{}"><file><url>{}</url><filename>{}</filename></file><file><url>{}</url></file></package></content></dlc>"#,
        e("My Package"),
        e(&format!("http://{host}/files/one.bin")),
        e("one.bin"),
        e(&format!("http://{host}/files/two.bin")),
    );
    let file_key = *b"0123456789abcdef";
    let body = STANDARD.encode(encrypt(
        &file_key,
        &file_key,
        STANDARD.encode(xml).as_bytes(),
    ));
    let dlc = format!("{body}{}", "K".repeat(88)).into_bytes();
    let rc = STANDARD.encode(encrypt(key, iv, &file_key));
    (dlc, rc)
}

#[derive(Clone, Default)]
struct Server {
    /// "path range" for every file request, in order.
    log: Arc<Mutex<Vec<String>>>,
    flaky: Arc<AtomicUsize>,
    limited: Arc<AtomicUsize>,
    /// Files /busy is sending right now.
    busy: Arc<AtomicUsize>,
    /// Times the stand-in router was asked to redial; its address follows.
    redials: Arc<AtomicUsize>,
    /// The address the internet sees stays the same across redials, as
    /// behind a carrier's shared address.
    public_kept: Arc<std::sync::atomic::AtomicBool>,
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
            "/nolength/{name}",
            // A file whose size the server never says: streamed, no length.
            get(|State(s): State<Server>, UrlPath(name): UrlPath<String>, headers: HeaderMap| async move {
                log(&s, &format!("/nolength/{name}"), &headers);
                let data = Bytes::from(content(&name, SIZE));
                let stream = futures_util::stream::once(async move { Ok::<_, Infallible>(data) });
                Response::builder().body(Body::from_stream(stream)).unwrap()
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
            "/v1/download/get_token.cgi",
            // 1fichier's API: its download link names nothing, as the real
            // one's does; the name comes with the download (/hidden).
            post(|headers: HeaderMap, Json(body): Json<serde_json::Value>| async move {
                let id = body["url"].as_str().unwrap_or_default().rsplit('?').next().unwrap_or_default().to_string();
                let host = headers.get(header::HOST).and_then(|h| h.to_str().ok()).unwrap_or_default().to_string();
                Json(json!({"status": "OK", "url": format!("http://{host}/hidden/{id}")}))
            }),
        )
        .route(
            "/page/{name}",
            get(|| async { ([(header::CONTENT_TYPE, "text/html")], "<html><title>Please solve the captcha</title></html>") }),
        )
        .route(
            "/twin/{id}",
            // Another vault listing kept.bin under another link; its proof
            // of work is answered as the main vault's is.
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/html")],
                    r#"<form method="post" action="/twin/copy">
                         <input type="hidden" name="csrf_token" value="tok-1">
                         <altcha-widget challengeurl="/altcha_challenge.php"></altcha-widget>
                       </form>"#,
                )
            })
            .post(|headers: HeaderMap| async move {
                let host = headers
                    .get(header::HOST)
                    .and_then(|h| h.to_str().ok())
                    .unwrap_or_default()
                    .to_string();
                (
                    [(header::CONTENT_TYPE, "text/html")],
                    format!(
                        r#"<div class="link-group" data-host="example.com">
                             <a class="target-link" href="http://{host}/files/kept.bin?copy=2">local</a>
                           </div>"#
                    ),
                )
            }),
        )
        .route(
            "/vault/{id}",
            // A wrapper page: a no-person proof of work, then the links.
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/html")],
                    r#"<form method="post" action="/vault/abc">
                         <input type="hidden" name="csrf_token" value="tok-1">
                         <altcha-widget challengeurl="/altcha_challenge.php"></altcha-widget>
                       </form>"#,
                )
            })
            .post(
                |State(s): State<Server>,
                 UrlPath(id): UrlPath<String>,
                 headers: HeaderMap,
                 Form(form): Form<HashMap<String, String>>| async move {
                    use base64::{Engine as _, engine::general_purpose::STANDARD};

                    log(&s, &format!("/vault/{id} solved"), &headers);
                    assert_eq!(form.get("csrf_token").map(String::as_str), Some("tok-1"));
                    let raw = STANDARD.decode(form.get("altcha").unwrap()).unwrap();
                    let solved: serde_json::Value = serde_json::from_slice(&raw).unwrap();
                    assert_eq!(solved["number"].as_u64(), Some(7));
                    let host = headers
                        .get(header::HOST)
                        .and_then(|h| h.to_str().ok())
                        .unwrap_or_default();
                    let port = host.rsplit(':').next().unwrap_or_default();
                    if id == "held" {
                        // One file on gofile (its page passes in the desktop
                        // app) and on a host Haul has no code for.
                        return (
                            [(header::CONTENT_TYPE, "text/html")],
                            format!(
                                r#"<div class="link-group" data-host="gofile.io">
                                     <a class="target-link" href="https://gofile.io/d/held">1</a>
                                   </div>
                                   <div class="link-group" data-host="example.com">
                                     <a class="target-link" href="http://{host}/files/held.bin">1</a>
                                   </div>"#
                            ),
                        );
                    }
                    if id == "parts" {
                        return (
                            [(header::CONTENT_TYPE, "text/html")],
                            format!(
                                r#"<div class="folder-title"><h1>Release R</h1></div>
                                   <div class="link-group" data-host="pixeldrain.com">
                                     <a class="target-link" href="https://pixeldrain.com/u/p1">1</a>
                                     <a class="target-link" href="https://pixeldrain.com/u/p2">2</a>
                                   </div>
                                   <div class="link-group" data-host="example.com">
                                     <a class="target-link" href="http://{host}/files/r.part1.rar">1</a>
                                     <a class="target-link" href="http://{host}/files/r.part2.rar">2</a>
                                   </div>"#
                            ),
                        );
                    }
                    (
                        [(header::CONTENT_TYPE, "text/html")],
                        format!(
                            r#"<div class="link-group" data-host="pixeldrain.com">
                                 <a class="target-link" href="https://pixeldrain.com/u/kept">pixeldrain</a>
                               </div>
                               <div class="link-group" data-host="example.com">
                                 <a class="target-link" href="http://{host}/files/kept.bin">local</a>
                               </div>
                               <div class="link-group" data-host="example.com">
                                 <a class="target-link" href="http://localhost:{port}/page/kept">page</a>
                               </div>"#
                        ),
                    )
                },
            ),
        )
        .route(
            "/altcha_challenge.php",
            get(|| async {
                Json(json!({
                    "algorithm": "SHA-256",
                    "challenge": sha256_hex(b"haul-test7"),
                    "maxNumber": 1000,
                    "salt": "haul-test",
                    "signature": "sig",
                }))
            }),
        )
        .route(
            "/Container/{page}",
            // filecrypt.cc's containers: "abc" turns every answer to its
            // checkbox away, so it waits for a person; "open" takes it.
            get(|UrlPath(page): UrlPath<String>| async move {
                match page.as_str() {
                    "abc.html" | "open.html" => FC_GATE.into_response(),
                    _ => StatusCode::NOT_FOUND.into_response(),
                }
            })
            .post(|headers: HeaderMap, UrlPath(page): UrlPath<String>| async move {
                let host = headers
                    .get(header::HOST)
                    .and_then(|h| h.to_str().ok())
                    .unwrap_or_default();
                if page == "open.html" {
                    fc_solved(host)
                } else {
                    FC_GATE.to_string()
                }
            }),
        )
        .route(
            "/captchasession/T1.json",
            post(|| async {
                Json(json!({"success": true, "autosolve": false,
                    "challenge": {"id": "C1", "challenge": "c0ffee", "difficulty": 4}}))
            }),
        )
        .route(
            "/DLC/{file}",
            get(|headers: HeaderMap, UrlPath(file): UrlPath<String>| async move {
                if file != "abc123.dlc" {
                    return StatusCode::NOT_FOUND.into_response();
                }
                let host = headers
                    .get(header::HOST)
                    .and_then(|h| h.to_str().ok())
                    .unwrap_or_default();
                dlc_fixture(host).0.into_response()
            }),
        )
        .route(
            "/dlc",
            // The key service that turns a `.dlc` into its links.
            get(|Query(q): Query<HashMap<String, String>>| async move {
                assert_eq!(q.get("srcType").map(String::as_str), Some("dlc"));
                assert_eq!(q.get("data").map(String::len), Some(88));
                format!("<rc>{}</rc>", dlc_fixture("unused").1)
            }),
        )
        .route(
            "/igddesc.xml",
            // A stand-in router: an Internet Gateway Device with a WAN service.
            get(|| async {
                r#"<root><device><modelName>Test Box 1</modelName><device><device>
                <service><serviceType>urn:schemas-upnp-org:service:WANIPConnection:1</serviceType>
                <controlURL>/igdupnp/control/WANIPConn1</controlURL></service></device></device></device></root>"#
            }),
        )
        .route(
            "/igdupnp/control/WANIPConn1",
            post(|State(s): State<Server>, headers: HeaderMap| async move {
                let action = headers
                    .get("soapaction")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.rsplit('#').next())
                    .unwrap_or_default()
                    .to_string();
                let redials = s.redials.load(Ordering::SeqCst);
                let answer = match action.as_str() {
                    "ForceTermination" => {
                        s.redials.fetch_add(1, Ordering::SeqCst);
                        String::new()
                    }
                    "GetExternalIPAddress" => format!("<NewExternalIPAddress>10.0.0.{}</NewExternalIPAddress>", redials + 1),
                    "GetStatusInfo" => "<NewConnectionStatus>Connected</NewConnectionStatus>".into(),
                    _ => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
                };
                format!("<s:Envelope><s:Body>{answer}</s:Body></s:Envelope>").into_response()
            }),
        )
        .route(
            "/ip",
            // The address the internet sees: what the router last got, or
            // the same one throughout when the line keeps it.
            get(|State(s): State<Server>| async move {
                let redials = s.redials.load(Ordering::SeqCst);
                if s.public_kept.load(Ordering::SeqCst) {
                    "203.0.113.9".to_string()
                } else {
                    format!("203.0.113.{}", redials + 1)
                }
            }),
        )
        .route(
            "/api/misc/rate_limits",
            // This address is past its day's full-speed bytes.
            get(|| async {
                Json(json!({"server_overload": false, "speed_limit": 1_048_576, "download_limit": 10000,
                    "download_limit_used": 3, "transfer_limit": 6_000_000_000u64, "transfer_limit_used": 9_494_305_080u64}))
            }),
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
            "/blog/{post}",
            // A release's post on a site Haul has no code for: its links to
            // hosts Haul knows, among the site's own and an ad.
            get(|UrlPath(post): UrlPath<String>| async move {
                let body = match post.as_str() {
                    "release" => r#"<html><head><title>Release R</title></head><body>
                        <a href="/blog/other">Older post</a>
                        <a href="https://casino.example/win">Win big</a>
                        <a href="https://pixeldrain.com/u/postone">Part 1</a>
                        <a href="https://pixeldrain.com/u/posttwo">Part 2</a>
                        </body></html>"#,
                    _ => "<html><head><title>Nothing here</title></head><body><a href=\"/\">Home</a></body></html>",
                };
                ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], body)
            }),
        )
        .route(
            "/archive/{name}",
            // The archives in tests/fixtures/archives, by name.
            get(|State(s): State<Server>, UrlPath(name): UrlPath<String>, headers: HeaderMap| async move {
                log(&s, &format!("/archive/{name}"), &headers);
                let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/archives");
                match std::fs::read(fixtures.join(&name)) {
                    Ok(data) => file_response(data, &headers),
                    Err(_) => StatusCode::NOT_FOUND.into_response(),
                }
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
                fileguard: self.ip.clone(),
                filecrypt: self.ip.clone(),
                dlc_service: format!("{}/dlc", self.ip),
                router: format!("{}/igddesc.xml", self.ip),
                ip_echo: format!("{}/ip", self.ip),
                nordvpn: self.ip.clone(),
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

/// Downloads under way from `host`.
fn active(s: &Snapshot, host: &str) -> usize {
    s.hosts
        .iter()
        .find(|h| h.name == host)
        .map_or(0, |h| h.active)
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

    // A finished download has nothing to pause, resume or retry.
    let id = s.items[0].id.clone();
    for action in [
        ItemAction::Pause,
        ItemAction::Resume,
        ItemAction::Retry,
        ItemAction::RetryManual,
    ] {
        assert!(engine.item_action(&id, action));
        let s = engine.snapshot();
        assert_eq!(
            (s.items[0].status, s.items[0].existing),
            ("done", false),
            "{action:?}"
        );
    }
    assert!(
        engine
            .set_direct(
                &id,
                &format!("{}/files/a.bin", t.name),
                BTreeMap::new(),
                None
            )
            .is_err(),
        "no second download through a link handed in late"
    );
    assert_eq!(engine.snapshot().items[0].status, "done");
}

#[tokio::test]
async fn a_saved_row_without_links_is_dropped() {
    let t = Setup::new().await;
    std::fs::create_dir_all(t.root.join("data")).unwrap();
    std::fs::write(
        t.root.join("data/queue.json"),
        r#"{"empty":{"mirrors":[],"status":"queued"},"kept":{"mirrors":["http://127.0.0.1:9/k.bin"],"status":"paused"}}"#,
    )
    .unwrap();
    let engine = t.engine().await;
    let ids: Vec<String> = engine.snapshot().items.into_iter().map(|i| i.id).collect();
    assert_eq!(ids, ["kept"]);
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
    // A host's page for the file (`name.html`) names the same file.
    let added = engine.add_text(
        &format!("{0}/missing/c.bin.html\n{1}/files/c.bin", t.ip, t.name),
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
        s.items[0].status == "paused" && active(s, "127.0.0.1") == 1
    })
    .await;
    let kept = std::fs::metadata(t.partial(&f)).unwrap().len();
    assert!(kept > 0 && kept < SIZE as u64, "{kept}");
    assert!(s.items[0].done >= kept);

    assert!(engine.item_action(&g, ItemAction::Remove));
    wait_for(&engine, "removal", |s| {
        s.items.len() == 1 && active(s, "127.0.0.1") == 0
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
    engine.set_host_limit("127.0.0.1", 2).unwrap();
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

    // A file that vanishes (an editor saving it, say) is no reason to fall
    // back to the defaults.
    std::fs::remove_file(&path).unwrap();
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let s = engine.snapshot();
    assert_eq!((s.max_total, s.config_error), (5, None));
    std::fs::write(&path, "max_total = 6\n").unwrap();
    wait_for(&engine, "the file back", |s| s.max_total == 6).await;
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
    let s = wait_for(&engine, "a stop", |s| active(s, "127.0.0.1") == 0).await;
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

/// The wrapper in the snapshot once it is unwrapped (or waits for a person).
async fn wrapper_settled(engine: &Engine, status: &str) -> Snapshot {
    wait_for(engine, &format!("the wrapper to be {status}"), |s| {
        s.wrappers.len() == 1 && s.wrappers[0].status == status
    })
    .await
}

/// A wrapper is unwrapped as soon as it is added, but nothing it lists
/// downloads until the user starts it.
#[tokio::test]
async fn a_wrapper_unwraps_into_files_that_wait_to_be_started() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    let mut events = engine.subscribe();
    let added = engine.add_text("https://fileguard.cc/vault/abc", "");
    assert_eq!((added.added, added.wrappers), (0, 1));
    let s = wrapper_settled(&engine, "done").await;
    let w = &s.wrappers[0];
    assert_eq!(w.found, Some(Found { files: 1, links: 3 }));
    // One file, every host a mirror, waiting for the user.
    let s = wait_for(&engine, "the file", |s| s.items.len() == 1).await;
    let it = &s.items[0];
    assert_eq!((it.name.as_str(), it.status), ("kept.bin", "paused"));
    assert_eq!(it.source.as_deref(), Some(w.id.as_str()));
    let mut hosts = it.mirrors.clone();
    hosts.sort();
    assert_eq!(hosts, ["127.0.0.1", "localhost", "pixeldrain.com"]);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        t.server.requests("/api/file/kept").is_empty(),
        "not started"
    );
    // With a file still to go, Clear completed leaves the wrapper alone.
    engine.clear_done();
    assert_eq!(engine.snapshot().wrappers.len(), 1);

    assert_eq!(engine.start_wrapper(&w.id), Some(1));
    wait_for(&engine, "the file", all_done).await;
    // The default preference is pixeldrain: the file came from there, and
    // the mirrors that would need a step of their own were never asked.
    assert_eq!(t.downloaded("kept.bin"), content("kept", SIZE));
    assert!(t.server.requests("/files/kept.bin").is_empty());
    assert!(t.server.requests("/page/kept").is_empty());
    while let Ok(e) = events.try_recv() {
        assert!(!matches!(e, Event::NeedsUser { .. }), "{e:?}");
    }
    // Its files cleared, the finished wrapper goes too.
    engine.clear_done();
    let s = engine.snapshot();
    assert_eq!((s.items.len(), s.wrappers.len()), (0, 0));
}

/// A wrapper whose last file is removed goes with it: it stood for its
/// files, and there is nothing left to start.
#[tokio::test]
async fn removing_a_wrappers_last_file_takes_the_wrapper() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.add_text("https://fileguard.cc/vault/abc", "");
    wrapper_settled(&engine, "done").await;
    let s = wait_for(&engine, "the file", |s| s.items.len() == 1).await;
    assert!(engine.remove(&s.items[0].id).await.unwrap());
    let s = engine.snapshot();
    assert_eq!((s.items.len(), s.wrappers.len()), (0, 0));
}

/// A web page on a site Haul has no code for, linking files on hosts it
/// knows, stands for those links, as if its text were pasted: no wrapper,
/// no row of its own. One linking none waits for a person.
#[tokio::test]
async fn a_page_of_links_adds_its_links() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.add_text(&format!("{}/blog/release", t.ip), "Pkg");
    let s = wait_for(&engine, "the page's files, named", |s| {
        s.items.len() == 2 && s.items.iter().all(|i| i.name.ends_with(".bin"))
    })
    .await;
    assert!(s.wrappers.is_empty(), "the page is not a wrapper");
    let mut rows: Vec<(&str, &str)> = s
        .items
        .iter()
        .map(|i| (i.name.as_str(), i.package.as_str()))
        .collect();
    rows.sort();
    assert_eq!(rows, [("postone.bin", "Pkg"), ("posttwo.bin", "Pkg")]);
    wait_for(&engine, "both downloaded", all_done).await;

    engine.add_text(&format!("{}/blog/empty", t.ip), "");
    wait_for(&engine, "the page without links", |s| {
        s.items.iter().any(|i| i.status == "captcha")
    })
    .await;
}

/// A host that gives nothing without an account is off until one is set:
/// its links are not tried, it says why, and setting the account tries them.
#[tokio::test]
async fn a_host_that_needs_an_account_is_off_without_one() {
    let t = Setup::new().await;
    let mut cfg = t.config();
    cfg.endpoints.rapidgator = t.ip.clone();
    let engine = Engine::start(Options {
        config: Some(cfg),
        ..t.options()
    })
    .await
    .unwrap();
    engine.add_text("https://rapidgator.net/file/abc/x.rar.html", "");
    let s = wait_for(&engine, "the file to wait for an account", |s| {
        s.items.iter().any(|i| i.status == "error")
    })
    .await;
    assert_eq!(
        s.items[0].error,
        "only on hosts that are off: rapidgator.net (needs an account)"
    );
    let host = s.hosts.iter().find(|h| h.name == "rapidgator.net").unwrap();
    assert!(host.needs_account && !host.disabled);
    assert!(
        t.server.requests("/api/v2/user/login").is_empty(),
        "not asked"
    );

    let account = json!({"rapidgator_user": "u", "rapidgator_pass": "p"});
    engine
        .update_settings(account.as_object().unwrap())
        .unwrap();
    let s = wait_for(&engine, "the file tried with the account", |s| {
        !s.items[0].error.starts_with("only on hosts that are off")
    })
    .await;
    assert!(!s.hosts.iter().any(|h| h.needs_account));
}

/// Where the desktop app passes pages, a host whose page passes needs no
/// account: its link waits for the page, and the folder the page hands
/// back becomes one row per file.
#[tokio::test]
async fn a_page_the_desktop_passes_stands_in_for_an_account() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.set_pages_pass(true);
    engine.add_text("https://gofile.io/d/folder", "");
    // Waiting for the desktop app to pass its page: nothing for the person.
    let s = wait_for(&engine, "the page to wait its turn", |s| {
        s.items.iter().any(|i| i.status == "passing")
    })
    .await;
    assert!(!s.hosts.iter().any(|h| h.needs_account));
    let id = s.items[0].id.clone();
    // Its window came forward: now it needs the person.
    engine.page_wants_person(&id);
    wait_for(&engine, "the page to want a person", |s| {
        s.items.iter().any(|i| i.status == "captcha")
    })
    .await;
    let file = |name: &str| haul_core::hosts::Entry {
        url: format!("{}/files/{name}", t.ip),
        name: Some(name.to_string()),
        ..Default::default()
    };
    engine
        .set_files(&id, vec![file("one.bin"), file("two.bin")])
        .unwrap();
    let s = wait_for(&engine, "both files", all_done).await;
    let mut names: Vec<&str> = s.items.iter().map(|i| i.name.as_str()).collect();
    names.sort();
    assert_eq!(names, ["one.bin", "two.bin"]);

    // Without the desktop app, the same host needs its account.
    engine.set_pages_pass(false);
    engine.add_text("https://gofile.io/d/other", "");
    let s = wait_for(&engine, "the account asked for", |s| {
        s.items
            .iter()
            .any(|i| i.error.contains("gofile.io (needs an account)"))
    })
    .await;
    assert!(
        s.hosts
            .iter()
            .any(|h| h.name == "gofile.io" && h.needs_account)
    );
}

/// Two wrappers listing the same file share its row: each counts it, either
/// starts it, and removing one leaves it to the other.
#[tokio::test]
async fn wrappers_listing_the_same_file_share_it() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.add_text("https://fileguard.cc/vault/abc", "");
    wrapper_settled(&engine, "done").await;
    engine.add_text("https://fileguard.cc/twin/copy", "");
    wait_for(&engine, "both wrappers", |s| {
        s.wrappers.len() == 2 && s.wrappers.iter().all(|w| w.status == "done")
    })
    .await;
    let s = wait_for(&engine, "one file, its mirrors joined", |s| {
        s.items.len() == 1 && s.items[0].mirrors.len() == 4
    })
    .await;
    let file = &s.items[0];
    let twin = s
        .wrappers
        .iter()
        .find(|w| w.url.ends_with("copy"))
        .unwrap()
        .id
        .clone();
    let first = s
        .wrappers
        .iter()
        .find(|w| w.url.ends_with("abc"))
        .unwrap()
        .id
        .clone();
    assert_eq!(file.source.as_deref(), Some(first.as_str()));
    assert_eq!(file.also_from, std::slice::from_ref(&twin));

    // Removing one wrapper leaves the file to the other.
    assert!(engine.remove_wrapper(&first));
    let s = engine.snapshot();
    assert_eq!(s.items.len(), 1, "still listed by the twin");
    assert_eq!(s.items[0].source.as_deref(), Some(twin.as_str()));
    // And that one starts it.
    assert_eq!(engine.start_wrapper(&twin), Some(1));
}

/// A file whose page the desktop app passes by itself waits for that page
/// rather than go to a host Haul has no code for; once the page wants a
/// person, its other mirrors may go.
#[tokio::test]
async fn a_file_waits_for_its_page_before_an_unknown_host() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.set_pages_pass(true);
    engine.add_text("https://fileguard.cc/vault/held", "");
    let w = wrapper_settled(&engine, "done").await.wrappers[0]
        .id
        .clone();
    let s = wait_for(&engine, "the file", |s| s.items.len() == 1).await;
    assert_eq!(s.items[0].mirrors.len(), 2, "{:?}", s.items[0].mirrors);
    let id = s.items[0].id.clone();
    engine.start_wrapper(&w);
    wait_for(&engine, "the file held for its page", |s| {
        s.items[0].status == "passing"
    })
    .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        t.server.requests("/files/held.bin").is_empty(),
        "not from the unknown host"
    );
    assert_eq!(engine.snapshot().items[0].status, "passing");

    engine.page_wants_person(&id);
    wait_for(&engine, "the other mirror", all_done).await;
}

/// A page that passed its check but gave no link this time (vikingfile's
/// "Error : …" or a request left generating) is tried again later rather
/// than handed to a person.
#[tokio::test]
async fn a_page_without_its_link_is_passed_again_later() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.set_pages_pass(true);
    engine.add_text("https://fileguard.cc/vault/held", "");
    let w = wrapper_settled(&engine, "done").await.wrappers[0]
        .id
        .clone();
    let s = wait_for(&engine, "the file", |s| s.items.len() == 1).await;
    let id = s.items[0].id.clone();
    engine.start_wrapper(&w);
    wait_for(&engine, "the file held for its page", |s| {
        s.items[0].status == "passing"
    })
    .await;
    engine.page_later(&id, "vikingfile: too many requests");
    let s = engine.snapshot();
    assert_eq!(s.items[0].status, "waiting");
    assert!(s.items[0].wait > 0, "it counts down to the next pass");
    assert_eq!(s.items[0].error, "vikingfile: too many requests");
}

/// Downloads with data on disk start first, the nearest to done first.
#[tokio::test]
async fn the_nearest_to_done_resumes_first() {
    let t = Setup::new().await;
    let mut cfg = t.config();
    cfg.max_total = 1;
    let engine = Engine::start(Options {
        config: Some(cfg),
        ..t.options()
    })
    .await
    .unwrap();
    engine.set_paused(true);
    for name in ["fresh", "half", "most"] {
        engine.add_text(&format!("{}/slow/{name}.bin", t.ip), "");
    }
    let s = wait_for(&engine, "three probed", |s| {
        s.items.len() == 3 && s.items.iter().all(|i| i.status == "queued")
    })
    .await;
    let id = |n: &str| {
        s.items
            .iter()
            .find(|i| i.name.starts_with(n))
            .unwrap()
            .id
            .clone()
    };
    engine.debug_set_done(&id("half"), SIZE as u64 / 2);
    engine.debug_set_done(&id("most"), SIZE as u64 * 9 / 10);
    engine.set_paused(false);
    let s = wait_for(&engine, "one started", |s| {
        s.items
            .iter()
            .any(|i| i.status == "downloading" || i.status == "resolving")
    })
    .await;
    let started: Vec<&str> = s
        .items
        .iter()
        .filter(|i| i.status == "downloading" || i.status == "resolving")
        .map(|i| i.name.as_str())
        .collect();
    assert_eq!(started, ["most.bin"]);
}

/// A release in parts lists part 1, part 2… on every host: one row per part,
/// each with every host as a mirror, named after the links that name it.
#[tokio::test]
async fn a_release_in_parts_unwraps_into_one_row_per_part() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    // The page lists pixeldrain first; the user prefers the other host.
    engine
        .set_preferred_hosts(&["127.0.0.1".to_string()])
        .unwrap();
    engine.add_text("https://fileguard.cc/vault/parts", "Pkg");
    let s = wrapper_settled(&engine, "done").await;
    assert_eq!(s.wrappers[0].title.as_deref(), Some("Release R"));
    assert_eq!(s.wrappers[0].found, Some(Found { files: 2, links: 4 }));
    let s = wait_for(&engine, "the parts", |s| s.items.len() == 2).await;
    assert_eq!(
        s.items
            .iter()
            .map(|i| (
                i.name.as_str(),
                i.status,
                i.mirrors.len(),
                i.package.as_str()
            ))
            .collect::<Vec<_>>(),
        [
            ("r.part1.rar", "paused", 2, "Pkg"),
            ("r.part2.rar", "paused", 2, "Pkg")
        ]
    );
    // The preferred host's link comes first: the one shown and probed.
    assert!(s.items.iter().all(|i| i.mirrors[0] == "127.0.0.1"));
}

/// What a wrapper's page says at once shows at once, even while downloads
/// are paused: a gate for a person, or a link that is dead.
#[tokio::test]
async fn a_wrapper_says_at_once_when_it_cannot_unwrap() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.set_paused(true);
    let mut events = engine.subscribe();
    engine.add_text("https://fileguard.cc/vault/gone/x", "");
    let s = wrapper_settled(&engine, "error").await;
    assert!(
        s.wrappers[0].error.contains("404"),
        "{}",
        s.wrappers[0].error
    );
    let e = events.recv().await.unwrap();
    assert!(matches!(e, Event::Failed { .. }), "{e:?}");
}

/// Every link of a vault shows the same page: added once, solved once.
#[tokio::test]
async fn a_wrapper_counts_once_whichever_of_its_links_comes() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    let added = engine.add_text(
        "https://fileguard.cc/vault/abc https://fileguard.cc/vault/abc/mega https://www.fileguard.cc/vault/abc/pixeldrain",
        "",
    );
    assert_eq!((added.wrappers, added.duplicates), (1, 2));
    wrapper_settled(&engine, "done").await;
    assert_eq!(
        engine
            .add_text("https://fileguard.cc/vault/abc/transfer", "")
            .duplicates,
        1,
        "one already unwrapped counts too"
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(t.server.requests("/vault/abc solved").len(), 1);
    assert_eq!(engine.snapshot().items.len(), 1);
}

/// A file a wrapper lists that the queue already wants keeps its row and
/// its turn: the wrapper's links join it as mirrors.
#[tokio::test]
async fn a_wrapper_file_joins_a_download_already_queued() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    // Nothing starts meanwhile; unwrapping goes on regardless.
    engine.set_paused(true);
    engine.add_text(&format!("{}/files/kept.bin", t.name), "");
    engine.add_text("https://fileguard.cc/vault/abc", "");
    wrapper_settled(&engine, "done").await;
    // Its own link, and the wrapper's three.
    let s = wait_for(&engine, "the merge", |s| {
        s.items.len() == 1 && s.items[0].mirrors.len() == 4
    })
    .await;
    assert_eq!(s.items[0].status, "queued", "still the user's download");
    assert_eq!(s.items[0].source, None);
}

/// Removing a wrapper takes the files it added that never started; what
/// the user started stays.
#[tokio::test]
async fn removing_a_wrapper_takes_its_files_that_never_started() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.set_paused(true);
    engine.add_text("https://fileguard.cc/vault/parts", "");
    let s = wrapper_settled(&engine, "done").await;
    let w = s.wrappers[0].id.clone();
    let s = wait_for(&engine, "the parts", |s| s.items.len() == 2).await;
    assert!(engine.item_action(&s.items[0].id, ItemAction::Resume));
    assert!(engine.remove_wrapper(&w));
    let s = engine.snapshot();
    assert!(s.wrappers.is_empty());
    assert_eq!(
        s.items
            .iter()
            .map(|i| (i.name.as_str(), i.status))
            .collect::<Vec<_>>(),
        [("r.part1.rar", "queued")]
    );
    assert!(!engine.remove_wrapper(&w), "gone");
    assert_eq!(engine.start_wrapper(&w), None);
}

#[tokio::test]
async fn a_filecrypt_gate_hands_back_its_dlc_that_unpacks() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    let page = "https://filecrypt.cc/Container/abc.html".to_string();
    engine.add_text(&page, "");
    let s = wrapper_settled(&engine, "captcha").await;
    assert_eq!(s.wrappers[0].open_urls, std::slice::from_ref(&page));
    assert!(s.items.is_empty(), "a wrapper is no download");
    let id = s.wrappers[0].id.clone();
    // What the in-app browser or a paste hands back: the container's link.
    engine
        .set_direct(
            &id,
            "https://filecrypt.cc/DLC/abc123.dlc",
            BTreeMap::new(),
            None,
        )
        .unwrap();
    let s = wrapper_settled(&engine, "done").await;
    assert_eq!(s.wrappers[0].title.as_deref(), Some("My Package"));
    assert_eq!(s.wrappers[0].url, page, "still the container's page");
    let s = wait_for(&engine, "the container's files", |s| {
        s.items.len() == 2 && s.items.iter().all(|i| i.status == "paused")
    })
    .await;
    assert!(s.items.iter().all(|i| i.package == "My Package"));
    assert_eq!(engine.start_wrapper(&id), Some(2));
    wait_for(&engine, "the downloads", all_done).await;
    assert_eq!(t.downloaded("My Package/one.bin"), content("one.bin", SIZE));
    assert_eq!(t.downloaded("My Package/two.bin"), content("two.bin", SIZE));
    assert_eq!(files_in(&t.root.join("dl")), ["My Package"]);
}

/// A container link handed back that turns out dead asks for the gate's page
/// again, as any link handed back does, and does not name the wrapper.
#[tokio::test]
async fn a_dead_container_link_asks_for_the_page_again() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    let page = "https://filecrypt.cc/Container/abc.html".to_string();
    engine.add_text(&page, "");
    let s = wrapper_settled(&engine, "captcha").await;
    let id = s.wrappers[0].id.clone();
    engine
        .set_direct(
            &id,
            "https://filecrypt.cc/DLC/gone.dlc",
            BTreeMap::new(),
            Some("gone.dlc".into()),
        )
        .unwrap();
    let s = wait_for(&engine, "the gate again", |s| {
        s.wrappers[0].status == "captcha" && s.wrappers[0].error.contains("404")
    })
    .await;
    assert_eq!(s.wrappers[0].open_urls, std::slice::from_ref(&page));
    assert_eq!(engine.page_for(&id).as_deref(), Some(page.as_str()));
    assert_eq!(s.wrappers[0].title, None, "a container names nothing");

    // The page's next hand-back is the container that unpacks.
    engine
        .set_direct(
            &id,
            "https://filecrypt.cc/DLC/abc123.dlc",
            BTreeMap::new(),
            None,
        )
        .unwrap();
    wrapper_settled(&engine, "done").await;
    wait_for(&engine, "the container's files", |s| s.items.len() == 2).await;
}

#[tokio::test]
async fn a_dlc_link_is_a_wrapper_that_unpacks_its_container() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    let added = engine.add_text("https://filecrypt.cc/DLC/abc123.dlc", "");
    assert_eq!((added.added, added.wrappers), (0, 1));
    let s = wrapper_settled(&engine, "done").await;
    assert_eq!(s.wrappers[0].found, Some(Found { files: 2, links: 2 }));
    let s = wait_for(&engine, "the container's files", |s| s.items.len() == 2).await;
    assert!(s.items.iter().all(|i| i.status == "paused"));
}

/// A container whose checkbox Haul answers itself unpacks as soon as it is
/// added: no person, no `.dlc`, no key service.
#[tokio::test]
async fn a_filecrypt_container_answers_its_checkbox_and_unpacks() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    let mut events = engine.subscribe();
    engine.add_text("https://filecrypt.cc/Container/open.html", "");
    let s = wrapper_settled(&engine, "done").await;
    assert_eq!(s.wrappers[0].title.as_deref(), Some("Open Release"));
    assert_eq!(s.wrappers[0].found, Some(Found { files: 2, links: 2 }));
    let s = wait_for(&engine, "the container's files", |s| {
        s.items.len() == 2 && s.items.iter().all(|i| i.status == "paused")
    })
    .await;
    assert_eq!(engine.start_wrapper(&s.wrappers[0].id), Some(2));
    wait_for(&engine, "the downloads", all_done).await;
    assert_eq!(t.downloaded("one.bin"), content("one.bin", SIZE));
    assert_eq!(t.downloaded("two.bin"), content("two.bin", SIZE));
    while let Ok(e) = events.try_recv() {
        assert!(!matches!(e, Event::NeedsUser { .. }), "{e:?}");
    }
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
    // The mechanism itself, without the shipped default preference.
    let engine = Engine::start(Options {
        config: Some(Config {
            preferred_hosts: Vec::new(),
            ..t.config()
        }),
        ..t.options()
    })
    .await
    .unwrap();
    engine.set_preferred("localhost", true).unwrap();
    engine.set_host_limit("localhost", 1).unwrap();
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

/// The default preference is pixeldrain.com; taking it out writes an empty
/// list, because a key left out would bring the default back.
#[tokio::test]
async fn clearing_the_default_preference_sticks() {
    let t = Setup::new().await;
    // No config of its own: the engine starts from the settings file, whose
    // missing keys take the defaults.
    let engine = Engine::start(Options {
        config: None,
        download_dir: Some(t.root.join("dl")),
        ..t.options()
    })
    .await
    .unwrap();
    let path = engine
        .config_path()
        .expect("started from a settings file")
        .to_path_buf();
    // Listed before any download reached it, so it can be seen and unstarred.
    let hosts = engine.snapshot().hosts;
    assert_eq!(
        hosts
            .iter()
            .map(|h| (h.name.as_str(), h.preferred, h.ok))
            .collect::<Vec<_>>(),
        [("pixeldrain.com", Some(0), 0)]
    );
    engine.set_preferred("pixeldrain.com", false).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("preferred_hosts = []"), "{text}");
    assert!(Config::parse(&text).unwrap().preferred_hosts.is_empty());
    assert!(
        engine.snapshot().hosts.is_empty(),
        "never reached, not preferred"
    );
}

/// The whole list in a new order: how the Hosts view reorders it.
#[tokio::test]
async fn preferred_hosts_reorder_and_keep_their_order() {
    let t = Setup::new().await;
    let engine = start_from_file(&t).await;
    let path = engine.config_path().unwrap().to_path_buf();
    let order = |engine: &Engine| -> Vec<(String, Option<usize>)> {
        let s = engine.snapshot();
        s.hosts.into_iter().map(|h| (h.name, h.preferred)).collect()
    };
    let list = |names: &[&str]| names.iter().map(|n| n.to_string()).collect::<Vec<_>>();
    engine.set_preferred("rapidgator.net", true).unwrap();
    engine.set_preferred("1fichier.com", true).unwrap();
    let starred = std::fs::read_to_string(&path).unwrap();

    let new = list(&["1fichier.com", "pixeldrain.com", "rapidgator.net"]);
    engine.set_preferred_hosts(&new).unwrap();
    assert_eq!(
        order(&engine),
        [
            ("1fichier.com".into(), Some(0)),
            ("pixeldrain.com".into(), Some(1)),
            ("rapidgator.net".into(), Some(2)),
        ]
    );
    // Only the list's own line changes in the file.
    let text = std::fs::read_to_string(&path).unwrap();
    let changed: Vec<_> = starred
        .lines()
        .zip(text.lines())
        .filter(|(was, is)| was != is)
        .collect();
    assert_eq!(starred.lines().count(), text.lines().count());
    assert_eq!(changed.len(), 1, "{changed:?}");
    assert_eq!(Config::parse(&text).unwrap().preferred_hosts, new);

    // A blank name, or one host twice, changes nothing.
    for bad in [
        list(&["1fichier.com", " "]),
        list(&["1fichier.com", "pixeldrain.com", "1FICHIER.com"]),
    ] {
        assert!(engine.set_preferred_hosts(&bad).is_err(), "{bad:?}");
    }
    assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
    assert_eq!(engine.cfg().preferred_hosts, new);

    // Starring still appends; the order outlives a restart.
    engine.set_preferred("gofile.io", true).unwrap();
    engine.shutdown().await;
    drop(engine);
    let engine = start_from_file(&t).await;
    assert_eq!(
        engine.cfg().preferred_hosts,
        list(&[
            "1fichier.com",
            "pixeldrain.com",
            "rapidgator.net",
            "gofile.io"
        ])
    );
    // An empty list is a choice too: no preference, not the default.
    engine.set_preferred_hosts(&[]).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(Config::parse(&text).unwrap().preferred_hosts.is_empty());
    assert!(order(&engine).is_empty());
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

/// A host that names the file only as its download starts (1fichier): that
/// name is checked before the body comes, so a file done in another row, or
/// already in the download folder, is not downloaded again.
#[tokio::test]
async fn a_name_that_comes_with_the_download_is_checked_first() {
    let t = Setup::new().await;
    let engine = Engine::start(Options {
        config: Some(Config {
            onefichier_key: Some("key".into()),
            endpoints: Endpoints {
                onefichier: t.ip.clone(),
                ..t.config().endpoints
            },
            ..t.config()
        }),
        ..t.options()
    })
    .await
    .unwrap();
    engine.add_text(&format!("{}/files/same.bin", t.ip), "");
    let s = wait_for(&engine, "the named file", all_done).await;
    let done = s.items[0].id.clone();

    // The same file again, from a link that names nothing until it downloads.
    engine.add_text("https://1fichier.com/?again", "");
    let s = wait_for(&engine, "the second row to fold away", |s| {
        s.items.len() == 1 && t.server.requests("/hidden/again").len() == 1
    })
    .await;
    assert_eq!(s.items[0].id, done);
    assert_eq!(files_in(&t.root.join("dl")), ["same.bin"]);

    // Off the list the file stays on disk, where the next such link finds it.
    assert!(engine.item_action(&done, ItemAction::Remove));
    engine.add_text("https://1fichier.com/?third", "");
    let s = wait_for(&engine, "the file on disk", |s| {
        s.items.len() == 1 && all_done(s)
    })
    .await;
    assert!(s.items[0].existing, "found, not downloaded");
    assert_eq!(s.items[0].name, "same.bin");
    assert_eq!(files_in(&t.root.join("dl")), ["same.bin"]);
    assert_eq!(t.downloaded("same.bin"), content("same.bin", SIZE));
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
async fn a_published_checksum_decides_when_asked() {
    let t = Setup::new().await;
    std::fs::create_dir_all(t.root.join("dl")).unwrap();
    // The host's bytes, and other bytes of the same size under the same name.
    std::fs::write(t.root.join("dl/kept.bin"), content("kept", SIZE)).unwrap();
    std::fs::write(t.root.join("dl/twin.bin"), content("not the twin", SIZE)).unwrap();
    let engine = Engine::start(Options {
        config: Some(Config {
            verify_existing: true,
            ..t.config()
        }),
        ..t.options()
    })
    .await
    .unwrap();
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
    engine.set_host_limit("127.0.0.1", 6).unwrap();
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

#[tokio::test]
async fn the_size_cap_spaces_out_big_files() {
    let t = Setup::new().await;
    // Room for one of these files at a time, not two.
    let engine = Engine::start(Options {
        config: Some(Config {
            max_active_gb: (SIZE as f64 * 1.5) / 1e9,
            ..t.config()
        }),
        ..t.options()
    })
    .await
    .unwrap();
    let links: Vec<String> = (1..=3).map(|i| format!("{}/slow/z{i}.bin", t.ip)).collect();
    engine.add_text(&links.join(" "), "");
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let s = engine.snapshot();
        let downloading = s.items.iter().filter(|i| i.status == "downloading").count();
        assert!(downloading <= 1, "two files at once: {:#?}", s.items);
        if s.items.len() == 3 && all_done(&s) {
            break;
        }
        assert!(Instant::now() < deadline, "timed out: {:#?}", s.items);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    for i in 1..=3 {
        let name = format!("z{i}.bin");
        assert_eq!(t.downloaded(&name), content(&name, SIZE));
    }
}

#[tokio::test]
async fn name_and_size_decide_by_default() {
    let t = Setup::new().await;
    std::fs::create_dir_all(t.root.join("dl")).unwrap();
    // Other bytes than the host's, but the same name and size: taken as the
    // file, and nothing is read or downloaded.
    std::fs::write(t.root.join("dl/twin.bin"), content("not the twin", SIZE)).unwrap();
    let engine = t.engine().await;
    engine.add_text("https://pixeldrain.com/u/twin", "");
    let s = wait_for(&engine, "done", all_done).await;
    assert!(s.items[0].existing);
    assert!(
        t.server
            .requests("/api/file/twin")
            .iter()
            .all(|r| r.starts_with("/api/file/twin/info")),
        "{:?}",
        t.server.requests("/api/file/twin")
    );
}

#[tokio::test]
async fn the_name_decides_when_the_size_is_unknown() {
    let t = Setup::new().await;
    std::fs::create_dir_all(t.root.join("dl")).unwrap();
    std::fs::write(t.root.join("dl/unsized.bin"), "an earlier copy").unwrap();
    let engine = t.engine().await;
    engine.add_text(&format!("{}/nolength/unsized.bin", t.ip), "");
    let s = wait_for(&engine, "done", all_done).await;
    assert_eq!(
        (s.items[0].existing, s.items[0].size),
        (true, "an earlier copy".len() as u64),
        "the size comes from the file found"
    );
    assert_eq!(files_in(&t.root.join("dl")), ["unsized.bin"]);
}

#[tokio::test]
async fn a_pause_holds_comparisons_and_survives_a_restart() {
    let t = Setup::new().await;
    std::fs::create_dir_all(t.root.join("dl")).unwrap();
    std::fs::write(t.root.join("dl/held.bin"), content("held", SIZE)).unwrap();
    let options = || Options {
        config: Some(Config {
            verify_existing: true,
            ..t.config()
        }),
        ..t.options()
    };
    let engine = Engine::start(options()).await.unwrap();
    engine.set_paused(true);
    engine.add_text("https://pixeldrain.com/u/held", "");
    // The link is looked up, but the file on disk is not read while paused.
    wait_for(&engine, "the name", |s| {
        s.items[0].name == "held.bin" && s.items[0].verifying != Verifying::No
    })
    .await;
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert!(!all_done(&engine.snapshot()), "compared while paused");
    engine.set_paused(false);
    let s = wait_for(&engine, "done", all_done).await;
    assert!(s.items[0].existing);

    // A pause is kept across a restart.
    engine.set_paused(true);
    engine.shutdown().await;
    drop(engine);
    let engine = start_when_free(options).await;
    assert!(engine.snapshot().paused);
    engine.set_paused(false);
    engine.shutdown().await;
    drop(engine);
    let engine = start_when_free(options).await;
    assert!(!engine.snapshot().paused);
}

/// A host the user turned off is never tried: its file downloads from
/// another mirror, a file only on such hosts fails saying so, and turning a
/// host back on starts that file again.
#[tokio::test]
async fn hosts_turned_off_are_never_tried() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.set_disabled("127.0.0.1", true).unwrap();
    // Turning a host off takes it out of the preferred hosts.
    engine.set_preferred("localhost", true).unwrap();
    engine.set_disabled("localhost", true).unwrap();
    let s = engine.snapshot();
    let off: Vec<&str> = s
        .hosts
        .iter()
        .filter(|h| h.disabled)
        .map(|h| h.name.as_str())
        .collect();
    assert_eq!(off, ["127.0.0.1", "localhost"]);
    assert!(s.hosts.iter().all(|h| h.preferred.is_none() || !h.disabled));
    engine.set_disabled("localhost", false).unwrap();

    engine.add_text(
        &format!("{0}/files/off.bin {1}/files/off.bin", t.ip, t.name),
        "",
    );
    let s = wait_for(&engine, "the file", all_done).await;
    assert_eq!(s.items.len(), 1, "one file, two mirrors");
    assert_eq!(s.items[0].host, "localhost", "never the host turned off");

    engine.set_disabled("localhost", true).unwrap();
    engine.add_text(
        &format!("{0}/files/later.bin {1}/files/later.bin", t.ip, t.name),
        "",
    );
    let s = wait_for(&engine, "the file to fail", |s| {
        s.items.iter().any(|i| i.status == "error")
    })
    .await;
    let failed = s.items.iter().find(|i| i.status == "error").unwrap();
    assert!(
        failed.error.contains("only on hosts that are off"),
        "{}",
        failed.error
    );
    assert!(
        t.server.requests("/files/later.bin").is_empty(),
        "no host was asked"
    );

    engine.set_disabled("127.0.0.1", false).unwrap();
    let s = wait_for(&engine, "the file again", all_done).await;
    assert!(s.items.iter().any(|i| i.host == "127.0.0.1"));
}

/// With several proxies, downloads from one host spread over them: here two
/// test servers stand in as HTTP proxies, each answering the files it is
/// asked for, and each serves one of the two downloads.
#[tokio::test]
async fn downloads_from_one_host_spread_over_the_proxies() {
    let t = Setup::new().await;
    let (second, port) = start_server().await;
    let mut cfg = t.config();
    cfg.proxies = vec![t.ip.clone(), format!("http://127.0.0.1:{port}")];
    let engine = Engine::start(Options {
        config: Some(cfg),
        ..t.options()
    })
    .await
    .unwrap();
    engine.set_paused(true);
    // A host the machine cannot reach itself: only the proxies know it.
    engine.add_text(
        "http://files.example/files/a.bin http://files.example/files/b.bin",
        "",
    );
    wait_for(&engine, "both probed", |s| {
        s.items.len() == 2 && s.items.iter().all(|i| i.status == "queued")
    })
    .await;
    engine.set_paused(false);
    let s = wait_for(&engine, "the downloads", all_done).await;
    let first = t.server.requests("/files/").len();
    let other = second.requests("/files/").len();
    assert!(
        first >= 1 && other >= 1,
        "one through each proxy: {first} and {other}"
    );
    let host = s.hosts.iter().find(|h| h.name == "files.example").unwrap();
    assert_eq!(host.exits, 2);
    assert_eq!(t.downloaded("a.bin"), content("a.bin", SIZE));
    assert_eq!(t.downloaded("b.bin"), content("b.bin", SIZE));
}

/// pixeldrain says how much each address may still fetch at full speed
/// today; the Hosts view shows it.
#[tokio::test]
async fn pixeldrains_quota_shows_with_the_host() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.set_paused(true);
    engine.add_text("https://pixeldrain.com/u/abc", "");
    let s = wait_for(&engine, "the quota", |s| {
        s.hosts
            .iter()
            .any(|h| h.name == "pixeldrain.com" && h.quota.is_some())
    })
    .await;
    let q = s
        .hosts
        .iter()
        .find(|h| h.name == "pixeldrain.com")
        .unwrap()
        .quota
        .clone()
        .unwrap();
    assert_eq!(
        (q.limit, q.used, q.slowed, q.slowed_to, q.known),
        (6_000_000_000, 9_494_305_080, 1, 1_048_576, 1)
    );
}

/// The router redials for a new address; the address this machine had
/// starts over, and the setting shows the router found.
#[tokio::test]
async fn the_router_gets_a_new_address() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    assert_eq!(
        engine.find_router().await.map(|r| r.model).as_deref(),
        Some("Test Box 1")
    );
    assert_eq!(
        engine.snapshot().reconnect.router.as_deref(),
        Some("Test Box 1")
    );
    let address = engine.reconnect("a test").await.unwrap();
    assert_eq!(address, "203.0.113.2", "the address the internet sees");
    assert_eq!(t.server.redials.load(Ordering::SeqCst), 1);
    let s = engine.snapshot();
    assert!(!s.reconnect.busy);
    assert!(s.reconnect.note.unwrap().contains("Got a new address"));
}

/// A redial whose router reports a new address while the internet still
/// sees the old one (a carrier's shared address) is no new address.
#[tokio::test]
async fn a_new_address_counts_only_when_the_internet_sees_it() {
    let t = Setup::new().await;
    t.server.public_kept.store(true, Ordering::SeqCst);
    let engine = t.engine().await;
    engine.find_router().await;
    let err = engine.reconnect("a test").await.unwrap_err();
    assert!(err.contains("still sees 203.0.113.9"), "{err}");
    let s = engine.snapshot();
    assert!(s.reconnect.failed);
    assert!(s.reconnect.note.unwrap().contains("No new address"));
}

/// A switch for redialing on its own is refused, and nothing is stopped,
/// while no redial can work: a FRITZ!Box without its login.
#[tokio::test]
async fn redialing_on_its_own_needs_a_way_to_redial() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.find_router().await;
    let on = json!({"reconnect": true});
    engine.update_settings(on.as_object().unwrap()).unwrap();
    assert!(
        engine.snapshot().reconnect.blocked.is_none(),
        "this router needs no login"
    );
    let cmd = json!({"reconnect": false});
    engine.update_settings(cmd.as_object().unwrap()).unwrap();
}

/// A stand-in SOCKS5 server: username and password auth with these
/// credentials, then CONNECT to wherever it is asked.
async fn socks5(user: &'static str, password: &'static str) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut c, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let mut head = [0u8; 2];
                c.read_exact(&mut head).await?;
                let mut methods = vec![0u8; head[1] as usize];
                c.read_exact(&mut methods).await?;
                c.write_all(&[5, 2]).await?;
                let field = |n: usize| vec![0u8; n];
                let mut b = [0u8; 2];
                c.read_exact(&mut b).await?;
                let mut u = field(b[1] as usize);
                c.read_exact(&mut u).await?;
                let mut n = [0u8; 1];
                c.read_exact(&mut n).await?;
                let mut p = field(n[0] as usize);
                c.read_exact(&mut p).await?;
                if u != user.as_bytes() || p != password.as_bytes() {
                    return c.write_all(&[1, 1]).await;
                }
                c.write_all(&[1, 0]).await?;
                let mut req = [0u8; 4];
                c.read_exact(&mut req).await?;
                let host = match req[3] {
                    1 => {
                        let mut ip = [0u8; 4];
                        c.read_exact(&mut ip).await?;
                        std::net::Ipv4Addr::from(ip).to_string()
                    }
                    _ => {
                        c.read_exact(&mut n).await?;
                        let mut name = field(n[0] as usize);
                        c.read_exact(&mut name).await?;
                        String::from_utf8_lossy(&name).into_owned()
                    }
                };
                let mut port = [0u8; 2];
                c.read_exact(&mut port).await?;
                let mut up =
                    tokio::net::TcpStream::connect((host.as_str(), u16::from_be_bytes(port)))
                        .await?;
                c.write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0]).await?;
                tokio::io::copy_bidirectional(&mut c, &mut up)
                    .await
                    .map(|_| ())
            });
        }
    });
    addr.to_string()
}

/// NordVPN's least loaded SOCKS5 servers in the countries chosen join the
/// proxies, one exit each, once its service credentials are set; each is
/// checked through itself first, and one that does not answer gives way to
/// the next. Credentials turned away are told, and the servers stay.
#[tokio::test]
async fn nordvpn_servers_join_the_proxies() {
    let t = Setup::new().await;
    let (good1, good2) = (socks5("svc", "pw").await, socks5("svc", "pw").await);
    // Nothing listens there.
    let dead = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().to_string()
    };
    let list = json!([
        {"hostname": dead, "load": 1, "locations": [{"country": {"name": "Netherlands", "city": {"name": "Amsterdam"}}}]},
        {"hostname": good1, "load": 9, "locations": [{"country": {"name": "Sweden", "city": {"name": "Stockholm"}}}]},
        {"hostname": "socks-us34.nordvpn.com", "load": 4, "locations": [{"country": {"name": "United States", "city": {"name": "Chicago"}}}]},
        {"hostname": good2, "load": 34, "locations": [{"country": {"name": "Netherlands", "city": {"name": "Amsterdam"}}}]}
    ]);
    let api = Router::new()
        .route("/v1/servers", get(move || async move { Json(list) }))
        .route(
            "/v1/helpers/ips/insights",
            get(|| async { Json(json!({"ip": "198.51.100.7", "protected": true})) }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let api_at = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, api).await.unwrap() });

    let start = |password: &str, data: &str| {
        let mut cfg = t.config();
        cfg.endpoints.nordvpn = api_at.clone();
        cfg.nordvpn_user = Some("svc".into());
        cfg.nordvpn_password = Some(password.into());
        cfg.nordvpn_servers = 2;
        cfg.nordvpn_countries = vec!["Sweden".into(), "Netherlands".into()];
        Engine::start(Options {
            config: Some(cfg),
            data_dir: t.root.join(data),
            ..t.options()
        })
    };
    let engine = start("pw", "data").await.unwrap();
    engine.set_paused(true);
    engine.add_text("https://pixeldrain.com/u/abc", "");
    let s = wait_for(&engine, "the servers", |s| s.services.nordvpn.len() == 2).await;
    let picked: Vec<(&str, Option<&str>)> = s
        .services
        .nordvpn
        .iter()
        .map(|v| (v.host.as_str(), v.address.as_deref()))
        .collect();
    assert_eq!(
        picked,
        [
            (good1.as_str(), Some("198.51.100.7")),
            (good2.as_str(), Some("198.51.100.7"))
        ],
        "least loaded in the countries chosen that answer, the dead one replaced"
    );
    assert_eq!(s.services.error, None);
    assert_eq!(
        s.services.nordvpn_countries,
        [
            ("Netherlands".to_string(), 2),
            ("Sweden".to_string(), 1),
            ("United States".to_string(), 1)
        ]
    );
    let s = wait_for(&engine, "two exits", |s| {
        s.hosts
            .iter()
            .any(|h| h.name == "pixeldrain.com" && h.exits == 2)
    })
    .await;
    assert!(s.proxy, "the status bar says proxies are on");
    engine.shutdown().await;

    let engine = start("wrong", "other").await.unwrap();
    let s = wait_for(&engine, "the refusal", |s| s.services.error.is_some()).await;
    assert!(
        s.services
            .error
            .as_deref()
            .unwrap()
            .contains("turned the service credentials away"),
        "{:?}",
        s.services.error
    );
    assert_eq!(s.services.nordvpn.len(), 2, "kept, so nothing goes direct");
}

/// The files the fixture archives hold.
fn assert_release(dir: &std::path::Path) {
    assert_eq!(
        std::fs::metadata(dir.join("movie.bin")).unwrap().len(),
        60000
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("sub/readme.txt")).unwrap(),
        "hello from haul\n"
    );
}

#[tokio::test]
async fn an_archive_in_parts_unpacks_when_its_last_part_is_done() {
    let t = Setup::new().await;
    let engine = Engine::start(Options {
        config: Some(Config {
            auto_extract: true,
            delete_after_extract: true,
            ..t.config()
        }),
        ..t.options()
    })
    .await
    .unwrap();
    let mut events = engine.subscribe();
    let links: Vec<String> = (1..=3)
        .map(|n| format!("{}/archive/R5.part{n}.rar", t.ip))
        .collect();
    engine.add_text(&links.join("\n"), "");
    // Extracted, and then its parts moved to the bin, which their rows say
    // by keeping no path.
    let s = wait_for(&engine, "extracted and recycled", |s| {
        s.items.len() == 3
            && s.items
                .iter()
                .all(|i| i.extract == Extract::Done && i.path.is_none())
    })
    .await;
    assert_release(&t.root.join("dl/Release"));
    assert_eq!(files_in(&t.root.join("dl")), ["Release"]);
    assert_eq!(
        files_in(&t.root.join("bin")),
        ["R5.part1.rar", "R5.part2.rar", "R5.part3.rar"]
    );
    assert!(s.items.iter().all(|i| i.status == "done"));
    let notice = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(Event::Notice { title, body }) = events.recv().await {
                return (title, body);
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(notice, ("Extracted".to_string(), "R5.rar".to_string()));
}

#[tokio::test]
async fn an_archive_unpacks_when_asked_and_says_why_it_cannot() {
    let t = Setup::new().await;
    // auto_extract is off.
    let engine = t.engine().await;
    engine.add_text(
        &format!("{0}/archive/S.7z.001 {0}/archive/S.7z.002", t.ip),
        "",
    );
    let s = wait_for(&engine, "two parts", |s| s.items.len() == 2 && all_done(s)).await;
    assert!(
        s.items.iter().all(|i| i.extract == Extract::No),
        "not on its own"
    );
    let id = s.items[0].id.clone();
    // The third part is not on the list yet: the archive lacks it.
    assert!(engine.extract(&id, None).unwrap());
    let s = wait_for(&engine, "failed", |s| {
        s.items.iter().all(|i| i.extract == Extract::Failed)
    })
    .await;
    assert!(!s.items[0].extract_error.is_empty());
    assert_eq!(files_in(&t.root.join("dl")), ["S.7z.001", "S.7z.002"]);

    engine.add_text(&format!("{}/archive/S.7z.003", t.ip), "");
    let err = engine.extract(&id, None).unwrap_err();
    assert!(err.contains("S.7z.003"), "{err}");
    wait_for(&engine, "three parts", |s| {
        s.items.len() == 3 && all_done(s)
    })
    .await;
    assert!(engine.item_action(&id, ItemAction::Extract));
    wait_for(&engine, "extracted", |s| {
        s.items.iter().all(|i| i.extract == Extract::Done)
    })
    .await;
    assert_release(&t.root.join("dl/Release"));
    // delete_after_extract is off: the parts stay.
    assert_eq!(
        files_in(&t.root.join("dl")),
        ["Release", "S.7z.001", "S.7z.002", "S.7z.003"]
    );

    let single = engine.add_text(&format!("{}/files/single.bin", t.ip), "");
    assert_eq!(single.added, 1);
    let s = wait_for(&engine, "done", |s| s.items.len() == 4 && all_done(s)).await;
    let single = s.items.iter().find(|i| i.name == "single.bin").unwrap();
    assert!(
        engine.extract(&single.id, None).is_err(),
        "no archive in parts"
    );
    assert!(!engine.extract("nope", None).unwrap());
}

#[tokio::test]
async fn a_part_missing_from_the_list_holds_the_archive_back() {
    let t = Setup::new().await;
    let engine = Engine::start(Options {
        config: Some(Config {
            auto_extract: true,
            ..t.config()
        }),
        ..t.options()
    })
    .await
    .unwrap();
    engine.add_text(
        &format!("{0}/archive/R5.part1.rar {0}/archive/R5.part3.rar", t.ip),
        "",
    );
    let s = wait_for(&engine, "two parts", |s| s.items.len() == 2 && all_done(s)).await;
    assert_eq!(s.archives.len(), 1);
    let a = &s.archives[0];
    assert_eq!(
        (a.name.as_str(), a.missing.clone()),
        ("R5.rar", vec!["R5.part2.rar".to_string()])
    );
    assert!(a.commands.is_empty(), "nothing to extract yet");
    assert!(
        s.items
            .iter()
            .all(|i| i.archive.as_deref() == Some(a.key.as_str()))
    );
    let err = engine.extract(&a.ids[0], None).unwrap_err();
    assert_eq!(err, "R5.part2.rar is missing from the list");

    // With it on the list, the archive extracts by itself.
    engine.add_text(&format!("{}/archive/R5.part2.rar", t.ip), "");
    wait_for(&engine, "extracted", |s| {
        s.items.len() == 3 && s.items.iter().all(|i| i.extract == Extract::Done)
    })
    .await;
    assert_release(&t.root.join("dl/Release"));
}

#[tokio::test]
async fn a_zip_split_into_z01_pieces_extracts() {
    let t = Setup::new().await;
    let engine = Engine::start(Options {
        config: Some(Config {
            auto_extract: true,
            ..t.config()
        }),
        ..t.options()
    })
    .await
    .unwrap();
    engine.add_text(&format!("{0}/archive/ZS.z01 {0}/archive/ZS.zip", t.ip), "");
    let s = wait_for(&engine, "extracted", |s| {
        s.items.len() == 2 && s.items.iter().all(|i| i.extract == Extract::Done)
    })
    .await;
    assert_eq!(s.archives[0].name, "ZS.zip");
    assert_eq!(
        std::fs::read_to_string(t.root.join("dl/Release/zlast.txt")).unwrap(),
        "after the split\n"
    );
}

#[tokio::test]
async fn an_encrypted_archive_asks_for_its_password_and_keeps_it() {
    let t = Setup::new().await;
    let engine = Engine::start(Options {
        config: Some(Config {
            auto_extract: true,
            archive_passwords: vec!["wrong".into()],
            ..t.config()
        }),
        ..t.options()
    })
    .await
    .unwrap();
    engine.add_text(
        &format!("{0}/archive/PV.part1.rar {0}/archive/PV.part2.rar", t.ip),
        "",
    );
    let s = wait_for(&engine, "a password wanted", |s| {
        s.items.len() == 2 && s.items.iter().all(|i| i.extract == Extract::Password)
    })
    .await;
    let a = &s.archives[0];
    assert_eq!(a.extract_error, "none of the passwords fits");
    assert_eq!(a.commands, [haul_core::Command::Password]);

    // The one the user types opens it, and stays with the archive.
    assert!(engine.extract(&a.ids[0], Some("open sesame")).unwrap());
    wait_for(&engine, "extracted", |s| {
        s.items.iter().all(|i| i.extract == Extract::Done)
    })
    .await;
    assert_eq!(
        std::fs::read_to_string(t.root.join("dl/Secret/note.txt")).unwrap(),
        "secret stuff\n"
    );
    engine.shutdown().await;
    let queue = std::fs::read_to_string(t.root.join("data/queue.json")).unwrap();
    assert!(
        queue.contains("\"extract_password\":\"open sesame\""),
        "{queue}"
    );
    assert!(
        !serde_json::to_string(&engine.snapshot())
            .unwrap()
            .contains("open sesame"),
        "never in the state the views receive"
    );
}

#[tokio::test]
async fn a_password_from_the_settings_opens_an_archive_on_its_own() {
    let t = Setup::new().await;
    let engine = Engine::start(Options {
        config: Some(Config {
            auto_extract: true,
            ..t.config()
        }),
        ..t.options()
    })
    .await
    .unwrap();
    let list = json!({"archive_passwords": "wrong\nopen sesame\n\nwrong"});
    engine.update_settings(list.as_object().unwrap()).unwrap();
    assert_eq!(
        engine.settings().archive_passwords,
        ["wrong", "open sesame"]
    );
    engine.add_text(
        &format!("{0}/archive/PV.part1.rar {0}/archive/PV.part2.rar", t.ip),
        "",
    );
    wait_for(&engine, "extracted", |s| {
        s.items.len() == 2 && s.items.iter().all(|i| i.extract == Extract::Done)
    })
    .await;
    let report = engine.diagnostics().to_string();
    assert!(!report.contains("open sesame"), "{report}");
}
