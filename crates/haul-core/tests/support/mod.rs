//! The stand-in the engine's tests download from: a local server under two
//! host names, with stand-ins for the hosts' APIs and pages, and what the
//! tests set up around it.

use std::{
    collections::HashMap,
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
use haul_core::{Config, Engine, Options, Snapshot, config::Endpoints};
use serde_json::json;

/// Deterministic, name-dependent bytes, so a mixed-up file shows.
pub(super) fn content(name: &str, len: usize) -> Vec<u8> {
    let seed = name
        .bytes()
        .fold(7u32, |a, b| a.wrapping_mul(31).wrapping_add(u32::from(b)));
    (0..len)
        .map(|i| (seed.wrapping_add(i as u32).wrapping_mul(2_654_435_761) >> 24) as u8)
        .collect()
}

pub(super) const SIZE: usize = 600 * 1024;

fn sha256_hex(data: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// filecrypt.cc's checkbox, as the test server's container pages show it.
pub(super) const FC_GATE: &str = r#"<h2>Security Check</h2><form id="cform">
  <div class="pow-captcha" id="pow-captcha" data-session="/captchasession/T1.json" data-px="">
    <input type="hidden" name="pow_id" value=""></div></form>"#;

/// A solved container page: its links as Click'n'Load data, two of the test
/// server's own files.
fn fc_solved(host: &str) -> String {
    fc_solved_with(
        "Open Example",
        &[
            format!("http://{host}/files/one.bin"),
            format!("http://{host}/files/two.bin"),
        ],
    )
}

/// A solved container titled `title` whose Click'n'Load data holds `links`.
pub(super) fn fc_solved_with(title: &str, links: &[String]) -> String {
    use aes::cipher::{BlockModeEncrypt, KeyIvInit, block_padding::NoPadding};
    use base64::{Engine as _, engine::general_purpose::STANDARD};

    let key = *b"6229761275967424";
    let plain = links.join("\r\n");
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
        "<h2>{title}</h2><form onsubmit=\"CNLPOP('X1', '{jk}', '{crypted}', '{title} - filecrypt.cc'); return false;\">"
    )
}

/// A solved container with its link table: two parts of `name` on `host`,
/// showing `sizes`.
fn fc_sized(host: &str, name: &str, sizes: [&str; 2]) -> String {
    let file = |n: usize| format!("{name}.part{n}.rar");
    let links: Vec<String> = (1..=2)
        .map(|n| format!("http://{host}/files/{}", file(n)))
        .collect();
    let rows: String = sizes
        .iter()
        .enumerate()
        .map(|(i, size)| {
            let file = file(i + 1);
            format!(
                r#"<tr class="kwj3"><td class="status"><i class="online small"></i></td><td title="{file}">{file}<span><a href="http://{host}" class="external_link">{host}</a></span></td><td>{size}</td><td><a href="/Link/abc.html" class="button download"></a></td></tr>"#
            )
        })
        .collect();
    format!("{}<table>{rows}</table>", fc_solved_with("Folder", &links))
}

/// A solved container whose link table has no names: two different folders
/// on `host`, each row showing `n/a` where a file's name goes.
fn fc_unnamed(host: &str) -> String {
    let links = [
        format!("http://{host}/d/abc123"),
        format!("http://{host}/d/def456"),
    ];
    let row = format!(
        r#"<tr class="kwj3"><td class="status"><i class="online small"></i></td><td title="n/a">n/a<span><a href="http://{host}" class="external_link">{host}</a></span></td><td>n/a</td><td><a href="/Link/abc.html" class="button download"></a></td></tr>"#
    );
    format!(
        "{}<table>{row}{row}</table>",
        fc_solved_with("Example", &links)
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
pub(super) struct Server {
    /// "path range" for every file request, in order.
    log: Arc<Mutex<Vec<String>>>,
    flaky: Arc<AtomicUsize>,
    limited: Arc<AtomicUsize>,
    /// Files /busy is sending right now.
    pub(super) busy: Arc<AtomicUsize>,
    /// Times the stand-in router was asked to redial; its address follows.
    pub(super) redials: Arc<AtomicUsize>,
    /// The address the internet sees stays the same across redials, as
    /// behind a carrier's shared address.
    pub(super) public_kept: Arc<std::sync::atomic::AtomicBool>,
    /// /eases stops slowing, as a host lifting its limit does.
    pub(super) eased: Arc<std::sync::atomic::AtomicBool>,
}

/// A file sent at the KiB/s `rate` says for each moment, from where it is
/// asked, 50 ms apart; logs `done <path>` once all of it went out, which an
/// aborted transfer never reaches. `sized`: says its length.
fn paced(
    s: &Server,
    path: String,
    data: Vec<u8>,
    headers: &HeaderMap,
    sized: bool,
    rate: impl Fn() -> usize + Send + 'static,
) -> Response {
    let len = data.len();
    let start = range_start(headers).unwrap_or(0).min(len);
    let log = s.log.clone();
    let stream = futures_util::stream::unfold(start, move |at| {
        let chunk = (rate() * 1024 / 20).max(1);
        let end = (at + chunk).min(len);
        let bytes = Bytes::copy_from_slice(&data[at..end]);
        let log = log.clone();
        let path = path.clone();
        async move {
            if at == len {
                return None;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
            if end == len {
                log.lock().unwrap().push(format!("done {path}"));
            }
            Some((Ok::<_, Infallible>(bytes), end))
        }
    });
    let mut resp = Response::builder();
    if sized {
        resp = resp.header(header::CONTENT_LENGTH, len - start);
    }
    if start > 0 {
        resp = resp.status(StatusCode::PARTIAL_CONTENT).header(
            header::CONTENT_RANGE,
            format!("bytes {start}-{}/{len}", len - 1),
        );
    }
    resp.body(Body::from_stream(stream)).unwrap()
}

/// Counts a /busy transfer out when its body is done or dropped.
struct BusyOut(Arc<AtomicUsize>);

impl Drop for BusyOut {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl Server {
    /// Where the first logged line starting with `line` stands in the log.
    pub(super) fn at(&self, line: &str) -> Option<usize> {
        self.log
            .lock()
            .unwrap()
            .iter()
            .position(|l| l.starts_with(line))
    }

    pub(super) fn requests(&self, path: &str) -> Vec<String> {
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

pub(super) async fn start_server() -> (Server, u16) {
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
            "/paced/{rate}/{name}",
            // A steady speed in KiB/s, for a host that holds a download at one.
            get(|State(s): State<Server>, method: Method, UrlPath((rate, name)): UrlPath<(usize, String)>, headers: HeaderMap| async move {
                if method == Method::HEAD {
                    return file_response(content(&name, SIZE), &headers);
                }
                let path = format!("/paced/{rate}/{name}");
                log(&s, &path, &headers);
                paced(&s, path, content(&name, SIZE), &headers, true, move || rate)
            }),
        )
        .route(
            "/unsized/{rate}/{name}",
            // The same, never saying how big the file is.
            get(|State(s): State<Server>, method: Method, UrlPath((rate, name)): UrlPath<(usize, String)>, headers: HeaderMap| async move {
                if method == Method::HEAD {
                    return StatusCode::OK.into_response();
                }
                let path = format!("/unsized/{rate}/{name}");
                log(&s, &path, &headers);
                paced(&s, path, content(&name, SIZE), &headers, false, move || rate)
            }),
        )
        .route(
            "/eases/{name}",
            // 40 KiB/s until the host eases off or the address changes, then
            // 300 KiB/s.
            get(|State(s): State<Server>, method: Method, UrlPath(name): UrlPath<String>, headers: HeaderMap| async move {
                if method == Method::HEAD {
                    return file_response(content(&name, SIZE), &headers);
                }
                let path = format!("/eases/{name}");
                log(&s, &path, &headers);
                let (eased, redials) = (s.eased.clone(), s.redials.clone());
                paced(&s, path, content(&name, SIZE), &headers, true, move || {
                    if eased.load(Ordering::SeqCst) || redials.load(Ordering::SeqCst) > 0 {
                        300
                    } else {
                        40
                    }
                })
            }),
        )
        .route(
            "/other/{name}",
            // A file by the same name and size that holds other bytes.
            get(|State(s): State<Server>, method: Method, UrlPath(name): UrlPath<String>, headers: HeaderMap| async move {
                if method != Method::HEAD {
                    log(&s, &format!("/other/{name}"), &headers);
                }
                file_response(content(&format!("other {name}"), SIZE), &headers)
            }),
        )
        .route(
            "/lost-check/{name}",
            // Holds other bytes, and its first answer to a range promises them
            // but breaks off before sending any.
            get(|State(s): State<Server>, method: Method, UrlPath(name): UrlPath<String>, headers: HeaderMap| async move {
                let data = content(&format!("other {name}"), SIZE);
                if method == Method::HEAD {
                    return file_response(data, &headers);
                }
                log(&s, &format!("/lost-check/{name}"), &headers);
                let start = range_start(&headers).filter(|_| s.flaky.fetch_add(1, Ordering::SeqCst) == 0);
                let Some(start) = start else {
                    return file_response(data, &headers);
                };
                let stream = futures_util::stream::once(async {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    Err::<Bytes, _>(std::io::Error::other("connection dropped"))
                });
                Response::builder()
                    .status(StatusCode::PARTIAL_CONTENT)
                    .header(header::CONTENT_LENGTH, SIZE - start)
                    .header(header::CONTENT_RANGE, format!("bytes {start}-{}/{SIZE}", SIZE - 1))
                    .body(Body::from_stream(stream))
                    .unwrap()
            }),
        )
        .route(
            "/expired/{name}",
            // A link that has run out: past the start of the file it answers
            // with a bare 416, as if it had nothing there.
            get(|State(s): State<Server>, UrlPath(name): UrlPath<String>, headers: HeaderMap| async move {
                log(&s, &format!("/expired/{name}"), &headers);
                if range_start(&headers).is_some_and(|start| start > 0) {
                    return StatusCode::RANGE_NOT_SATISFIABLE.into_response();
                }
                file_response(content(&name, SIZE), &headers)
            }),
        )
        .route(
            "/cut/{name}",
            // Answers a range as if it had the file, and sends nothing.
            get(|State(s): State<Server>, UrlPath(name): UrlPath<String>, headers: HeaderMap| async move {
                log(&s, &format!("/cut/{name}"), &headers);
                let Some(start) = range_start(&headers) else {
                    return file_response(content(&name, SIZE), &headers);
                };
                Response::builder()
                    .status(StatusCode::PARTIAL_CONTENT)
                    .header(header::CONTENT_RANGE, format!("bytes {start}-{}/*", SIZE - 1))
                    .body(Body::empty())
                    .unwrap()
            }),
        )
        .route(
            "/gate/{kind}",
            // A page behind a gate, as the product behind it draws one.
            get(|UrlPath(kind): UrlPath<String>| async move {
                let widget = match kind.as_str() {
                    "recaptcha" => r#"<script src="https://www.google.com/recaptcha/api.js"></script><div class="g-recaptcha" data-sitekey="x"></div>"#,
                    "hcaptcha" => r#"<script src="https://js.hcaptcha.com/1/api.js"></script><div class="h-captcha" data-sitekey="x"></div>"#,
                    "turnstile" => r#"<div class="cf-turnstile" data-sitekey="x"></div>"#,
                    _ => "<p>Press the button</p>",
                };
                axum::response::Html(format!("<html><head><title>Download</title></head><body><form>{widget}<button>Download</button></form></body></html>"))
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
            "/limited-long/{name}",
            get(|| async { (StatusCode::TOO_MANY_REQUESTS, [(header::RETRY_AFTER, "30")]) }),
        )
        .route(
            "/id/{key}",
            // A host whose links are ids: the name (`A_part1_rar` for
            // A.part1.rar) comes only in the answer.
            get(|State(s): State<Server>, UrlPath(key): UrlPath<String>, headers: HeaderMap| async move {
                log(&s, &format!("/id/{key}"), &headers);
                let name = key.replace('_', ".");
                let mut resp = file_response(content(&name, SIZE), &headers);
                resp.headers_mut()
                    .insert(header::CONTENT_DISPOSITION, format!("attachment; filename=\"{name}\"").parse().unwrap());
                resp
            }),
        )
        .route(
            "/repos/lordsilver/haul/releases/latest",
            // GitHub's latest release of Haul, far in the future, with its
            // setup program and that file's digest.
            get(|State(s): State<Server>, headers: HeaderMap| async move {
                log(&s, "/repos/lordsilver/haul/releases/latest", &headers);
                let host = headers.get(header::HOST).unwrap().to_str().unwrap().to_string();
                Json(json!({
                    "tag_name": "v99.0.0",
                    "body": "### Added\n\n- Everything.",
                    "html_url": "https://github.com/lordsilver/haul/releases/tag/v99.0.0",
                    "draft": false,
                    "prerelease": false,
                    "assets": [
                        {"name": "haul-desktop-v99.0.0-windows-x64-setup.exe", "browser_download_url": format!("http://{host}/files/setup.exe"), "digest": format!("sha256:{}", sha256_hex(&content("setup.exe", SIZE)))},
                        ]
                }))
            }),
        )
        .route(
            "/check_links.pl",
            // 1fichier's link checker: a link with "gone" in it was deleted.
            post(|body: String| async move {
                let link = body.split('=').nth(1).unwrap_or_default().to_string();
                let link = percent_encoding::percent_decode_str(&link).decode_utf8_lossy().into_owned();
                if link.contains("gone") {
                    format!("{link};;;NOT FOUND\n")
                } else {
                    String::new()
                }
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
                    // One file on ddownload, whose page passes in the desktop
                    // app, and on another host, both reached with the stand-in
                    // as a proxy: one Haul has no code for, one whose page
                    // passes too, or one whose page only a person can do.
                    let beside_ddownload = match id.as_str() {
                        "ddownload-held" => Some("http://files.example/files/held.bin"),
                        "ddownload-slow" => Some("http://fileditchfiles.st/paced/40/held.bin"),
                        "ddownload-gofile" => Some("https://gofile.io/d/both"),
                        "ddownload-rapidgator" => Some("https://rapidgator.net/file/person"),
                        _ => None,
                    };
                    if let Some(other) = beside_ddownload {
                        return (
                            [(header::CONTENT_TYPE, "text/html")],
                            format!(
                                r#"<div class="link-group" data-host="ddownload.com">
                                     <a class="target-link" href="http://ddownload.com/dd/{id}">1</a>
                                   </div>
                                   <div class="link-group" data-host="other.example">
                                     <a class="target-link" href="{other}">1</a>
                                   </div>"#
                            ),
                        );
                    }
                    if id == "loose-gofile" {
                        // Two files on pixeldrain, which names them, and
                        // three links on gofile, which names nothing without
                        // its account: the counts differ, so each link is a
                        // row of its own.
                        return (
                            [(header::CONTENT_TYPE, "text/html")],
                            r#"<div class="link-group" data-host="pixeldrain.com">
                                 <a class="target-link" href="https://pixeldrain.com/u/p1">1</a>
                                 <a class="target-link" href="https://pixeldrain.com/u/p2">2</a>
                               </div>
                               <div class="link-group" data-host="gofile.io">
                                 <a class="target-link" href="https://gofile.io/d/same">1</a>
                                 <a class="target-link" href="https://gofile.io/d/dead">2</a>
                                 <a class="target-link" href="https://gofile.io/d/shy">3</a>
                               </div>"#
                                .to_string(),
                        );
                    }
                    if id == "parts" {
                        return (
                            [(header::CONTENT_TYPE, "text/html")],
                            format!(
                                r#"<div class="folder-title"><h1>Example R</h1></div>
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
            // "cf-once" is behind Cloudflare's check the first time it is
            // asked, as until a browser passed it; "cf-always" every time.
            get(|UrlPath(page): UrlPath<String>| async move {
                static CF_ONCE: AtomicUsize = AtomicUsize::new(0);
                // "missing-…" answers Not Found the first time it is asked,
                // then is a container like "open".
                static MISSED: Mutex<Vec<String>> = Mutex::new(Vec::new());
                if page.starts_with("missing-") {
                    let mut missed = MISSED.lock().unwrap();
                    if !missed.contains(&page) {
                        missed.push(page);
                        return StatusCode::NOT_FOUND.into_response();
                    }
                    return FC_GATE.into_response();
                }
                let challenged = match page.as_str() {
                    "cf-once.html" => CF_ONCE.fetch_add(1, Ordering::SeqCst) == 0,
                    "cf-always.html" => true,
                    _ => false,
                };
                if challenged {
                    return (
                        StatusCode::FORBIDDEN,
                        [(header::HeaderName::from_static("cf-mitigated"), "challenge")],
                        "<html><head><title>Just a moment...</title></head></html>",
                    )
                        .into_response();
                }
                match page.as_str() {
                    "abc.html" | "open.html" | "cf-once.html" | "host-a.html" | "host-b.html"
                    | "unnamed.html" | "for-account.html" | "ids-a.html" | "ids-b.html" | "same-a.html" | "same-b.html"
                    | "other-a.html" | "other-b.html" => FC_GATE.into_response(),
                    // Another gate than the checkbox, past the check in front
                    // of the site.
                    "other-gate.html" => "<h2>Security Check</h2><div class=\"other\"></div>".into_response(),
                    _ => StatusCode::NOT_FOUND.into_response(),
                }
            })
            .post(|headers: HeaderMap, UrlPath(page): UrlPath<String>| async move {
                let host = headers
                    .get(header::HOST)
                    .and_then(|h| h.to_str().ok())
                    .unwrap_or_default();
                // One set of files, a container per host, each host naming the
                // parts its own way.
                let other = host.replace("127.0.0.1", "localhost");
                if page == "host-a.html" {
                    fc_solved_with(
                        "Example.Set",
                        &[
                            format!("http://{host}/files/a1b2c3.part1.rar"),
                            format!("http://{host}/files/a1b2c3.part2.rar"),
                        ],
                    )
                } else if page == "host-b.html" {
                    fc_solved_with(
                        "Example.Set",
                        &[
                            format!("http://{other}/files/Example.Archive.part1.rar"),
                            format!("http://{other}/files/Example.Archive.part2.rar"),
                        ],
                    )
                } else if page == "for-account.html" {
                    // Two files here, and one only on a host off for want of
                    // its account, whose table has no name for it.
                    let name = |n: &str| {
                        format!(
                            r#"<tr class="kwj3"><td class="status"><i class="online small"></i></td><td title="{n}">{n}<span><a href="http://{host}" class="external_link">{host}</a></span></td><td>n/a</td><td><a href="/Link/abc.html" class="button download"></a></td></tr>"#
                        )
                    };
                    format!(
                        "{}<table>{}{}{}</table>",
                        fc_solved_with(
                            "Example",
                            &[
                                format!("http://{host}/files/one.bin"),
                                format!("http://{host}/files/two.bin"),
                                "https://gofile.io/d/abc123".to_string(),
                            ],
                        ),
                        name("one.bin"),
                        name("two.bin"),
                        name("n/a"),
                    )
                } else if page == "unnamed.html" {
                    fc_unnamed(host)
                } else if page == "same-a.html" {
                    fc_sized(host, "Archive.One", ["2.00 GB", "880.00 MB"])
                } else if page == "same-b.html" {
                    fc_sized(&other, "Archive.Two", ["2.00 GB", "880.00 MB"])
                } else if page == "other-a.html" {
                    fc_sized(host, "Archive.Three", ["2.00 GB", "880.00 MB"])
                } else if page == "other-b.html" {
                    fc_sized(&other, "Archive.Four", ["2.00 GB", "500.00 MB"])
                } else if page == "ids-a.html" || page == "ids-b.html" {
                    // Named after nothing but ids on both hosts, under a
                    // title that says nothing either.
                    let (at, id) = if page == "ids-a.html" {
                        (host.to_string(), "0123456789abcdef")
                    } else {
                        (other, "0123456789abcdef0000")
                    };
                    fc_solved_with(
                        "Folder",
                        &[
                            format!("http://{at}/files/{id}.part1.rar"),
                            format!("http://{at}/files/{id}.part2.rar"),
                        ],
                    )
                } else if page == "open.html"
                    || page == "cf-once.html"
                    || page.starts_with("missing-")
                {
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
                        s.log.lock().unwrap().push("/redial".to_string());
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
            "/api/1.5/file/get_info.php",
            // mediafire's: what a file is, by its key.
            get(|State(s): State<Server>, Query(q): Query<HashMap<String, String>>, headers: HeaderMap| async move {
                log(&s, "/api/1.5/file/get_info.php", &headers);
                let key = q.get("quick_key").cloned().unwrap_or_default();
                Json(json!({"response": {"file_info": {"filename": format!("{key}.bin"), "size": SIZE.to_string()}}}))
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
            // A post on a site Haul has no code for: its links to
            // hosts Haul knows, among the site's own and an ad.
            get(|State(s): State<Server>, UrlPath(post): UrlPath<String>, headers: HeaderMap| async move {
                log(&s, &format!("/blog/{post}"), &headers);
                // An archive in parts on another site (this server by its
                // other name) and, in the text, its password.
                if post == "with-password" {
                    let port = headers
                        .get(header::HOST)
                        .and_then(|h| h.to_str().ok())
                        .and_then(|h| h.rsplit(':').next())
                        .unwrap_or_default()
                        .to_string();
                    return axum::response::Html(format!(
                        r#"<html><head><title>Example V</title></head><body>
                        <a href="http://localhost:{port}/archive/PV.part1.rar">Part 1</a>
                        <a href="http://localhost:{port}/archive/PV.part2.rar">Part 2</a>
                        <p>Password: "open sesame"</p>
                        </body></html>"#
                    ))
                    .into_response();
                }
                let body = match post.as_str() {
                    // An address that reads like a file's name.
                    "links" | "Example.Set.v1.25.html" => r#"<html><head><title>Example R</title></head><body>
                        <a href="/blog/other">Older post</a>
                        <a href="https://casino.example/win">Win big</a>
                        <a href="https://pixeldrain.com/u/postone">Part 1</a>
                        <a href="https://pixeldrain.com/u/posttwo">Part 2</a>
                        </body></html>"#,
                    // Another site's post of the same files: the same links.
                    "links-copy" => r#"<html><head><title>Example R, again</title></head><body>
                        <a href="https://pixeldrain.com/u/postone">Part 1</a>
                        <a href="https://pixeldrain.com/u/posttwo">Part 2</a>
                        </body></html>"#,
                    // The same files, a container per host.
                    "per-host" => r#"<html><head><title>Example P</title></head><body>
                        <a href="https://filecrypt.cc/Container/host-a.html">Mirror A</a>
                        <a href="https://filecrypt.cc/Container/host-b.html">Mirror B</a>
                        </body></html>"#,
                    "per-host-same" => r#"<html><head><title>Example S</title></head><body>
                        <a href="https://filecrypt.cc/Container/same-a.html">Mirror A</a>
                        <a href="https://filecrypt.cc/Container/same-b.html">Mirror B</a>
                        </body></html>"#,
                    "two-sets" => r#"<html><head><title>Example T</title></head><body>
                        <a href="https://filecrypt.cc/Container/other-a.html">Set A</a>
                        <a href="https://filecrypt.cc/Container/other-b.html">Set B</a>
                        </body></html>"#,
                    "per-host-ids" => r#"<html><head><title>Example I</title></head><body>
                        <a href="https://filecrypt.cc/Container/ids-a.html">Mirror A</a>
                        <a href="https://filecrypt.cc/Container/ids-b.html">Mirror B</a>
                        </body></html>"#,
                    // Kept behind a check a browser passes (Cloudflare's).
                    "guarded" => {
                        return (
                            StatusCode::FORBIDDEN,
                            [(header::CONTENT_TYPE, "text/html; charset=utf-8"), (header::HeaderName::from_static("cf-mitigated"), "challenge")],
                            "<html><head><title>Just a moment...</title></head><body>turnstile</body></html>",
                        )
                            .into_response();
                    }
                    // A page's title, which names no file.
                    "notes" => r#"<html><head><title>Example.Notes.2026.Final | Blog</title></head><body>
                        <a href="https://pixeldrain.com/u/postone">Part 1</a>
                        <a href="https://pixeldrain.com/u/posttwo">Part 2</a>
                        </body></html>"#,
                    "Plain.Notes.v1.25.html" => "<html><head><title>Plain notes</title></head><body><p>No links.</p></body></html>",
                    // Files on another site (the stand-in by its name), on a
                    // host Haul has no code for.
                    "elsewhere" => {
                        let other = headers
                            .get(header::HOST)
                            .and_then(|h| h.to_str().ok())
                            .unwrap_or_default()
                            .replace("127.0.0.1", "localhost");
                        let body = format!(
                            r#"<html><head><title>Example E</title></head><body>
                        <a href="/blog/other">Older post</a>
                        <a href="/files/site.rar">This site's file</a>
                        <a href="http://{other}/files/r.part1.rar">Part 1</a>
                        <a href="http://{other}/files/r.part2.rar">Part 2</a>
                        </body></html>"#
                        );
                        return (StatusCode::OK, [(header::CONTENT_TYPE, "text/html; charset=utf-8")], body).into_response();
                    }
                    // A container still to be unwrapped, and a trailer the
                    // page plays from another site.
                    "trailer" => {
                        let other = headers
                            .get(header::HOST)
                            .and_then(|h| h.to_str().ok())
                            .unwrap_or_default()
                            .replace("127.0.0.1", "localhost");
                        let body = format!(
                            r#"<html><head><title>Example T</title></head><body>
                        <video autoplay muted><source src="http://{other}/files/trailer.mp4" type="video/mp4"></video>
                        <a href="https://filecrypt.cc/Container/abc.html">Mirror 1</a>
                        </body></html>"#
                        );
                        return (StatusCode::OK, [(header::CONTENT_TYPE, "text/html; charset=utf-8")], body).into_response();
                    }
                    _ => "<html><head><title>Nothing here</title></head><body><a href=\"/\">Home</a></body></html>",
                };
(StatusCode::OK, [(header::CONTENT_TYPE, "text/html; charset=utf-8")], body).into_response()
            }),
        )
        .route(
            "/dd/{id}",
            // ddownload.com's file page, with the stand-in as the proxy.
            get(|UrlPath(id): UrlPath<String>| async move {
                (
                    [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
                    format!(r#"<html><body><span class="dk-dl-name" title="{id}.rar"></span></body></html>"#),
                )
            }),
        )
        .route(
            "/paste/{id}",
            // A paste holding no address, with the stand-in as the proxy.
            get(|UrlPath(_): UrlPath<String>| async move {
                (
                    [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
                    "<html><head><title>Notes</title></head><body><p>Only a host's name.</p></body></html>",
                )
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

pub(super) struct Setup {
    _tmp: tempfile::TempDir,
    pub(super) root: PathBuf,
    pub(super) server: Server,
    pub(super) ip: String,
    pub(super) name: String,
}

impl Setup {
    pub(super) async fn new() -> Self {
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

    pub(super) fn config(&self) -> Config {
        Config {
            download_dir: Some(self.root.join("dl")),
            endpoints: Endpoints {
                pixeldrain: self.ip.clone(),
                onefichier_site: self.ip.clone(),
                mega: self.ip.clone(),
                rootz: self.ip.clone(),
                mediafire: self.ip.clone(),
                github_api: self.ip.clone(),
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

    pub(super) fn options(&self) -> Options {
        Options {
            data_dir: self.root.join("data"),
            download_dir: None,
            config: Some(self.config()),
            recycle_bin: Some(self.root.join("bin")),
        }
    }

    pub(super) async fn engine(&self) -> Engine {
        Engine::start(self.options()).await.unwrap()
    }

    /// An engine on this config instead of the plain `config()`.
    pub(super) async fn engine_with(&self, config: Config) -> Engine {
        Engine::start(Options {
            config: Some(config),
            ..self.options()
        })
        .await
        .unwrap()
    }

    pub(super) fn downloaded(&self, rel: &str) -> Vec<u8> {
        std::fs::read(self.root.join("dl").join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
    }

    pub(super) fn partial(&self, id: &str) -> PathBuf {
        self.root
            .join("dl")
            .join(".partial")
            .join(format!("{id}.part"))
    }
}

pub(super) async fn wait_for(
    engine: &Engine,
    what: &str,
    f: impl Fn(&Snapshot) -> bool,
) -> Snapshot {
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

pub(super) fn all_done(s: &Snapshot) -> bool {
    !s.items.is_empty() && s.items.iter().all(|i| i.status.as_str() == "done")
}

/// Downloads under way from `host`.
pub(super) fn active(s: &Snapshot, host: &str) -> usize {
    s.hosts
        .iter()
        .find(|h| h.name == host)
        .map_or(0, |h| h.active)
}
