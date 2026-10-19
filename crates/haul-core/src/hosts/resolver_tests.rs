//! Resolvers against local stand-ins for each host's API. These pin the
//! request shapes the resolvers send and how they read the answers; whether
//! the real hosts still answer that way is a separate, live question.

use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use axum::{
    Form, Json, Router,
    extract::{Path, Query},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
};
use serde_json::{Value, json};

use super::*;
use crate::config::Endpoints;

async fn serve(app: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

fn config(base: &str) -> Config {
    Config {
        endpoints: Endpoints {
            pixeldrain: base.into(),
            gofile: base.into(),
            buzzheavier: base.into(),
            onefichier: base.into(),
            rapidgator: base.into(),
            nitroflare: base.into(),
            dlc_service: format!("{base}/dlc"),
            fileguard: base.into(),
            filecrypt: base.into(),
            vikingfile: base.into(),
            mirrored: base.into(),
            router: format!("{base}/igddesc.xml"),
            ip_echo: format!("{base}/ip"),
            nordvpn: base.into(),
        },
        ..Config::default()
    }
}

fn http() -> Http {
    Http::new(None, "test-agent").unwrap()
}

#[tokio::test]
async fn pixeldrain_file_list_and_captcha() {
    let app = Router::new()
        .route(
            "/api/file/{id}/info",
            get(|Path(id): Path<String>, headers: HeaderMap| async move {
                let auth = headers.get("authorization").map(|v| v.to_str().unwrap().to_string());
                match id.as_str() {
                    "abc" => Json(json!({"name": "a.zip", "size": 1234, "auth": auth, "hash_sha256": "AB".repeat(32)}))
                        .into_response(),
                    "busy" => Json(json!({"name": "b.zip", "size": 1, "availability": "file_rate_limited_captcha_required"}))
                        .into_response(),
                    _ => StatusCode::NOT_FOUND.into_response(),
                }
            }),
        )
        .route(
            "/api/list/{id}",
            get(|| async {
                Json(json!({"files": [{"id": "f1", "name": "one.rar", "size": 10}, {"id": "f2", "name": "two.rar", "size": 20}]}))
            }),
        );
    let base = serve(app).await;
    let mut cfg = config(&base);
    cfg.pixeldrain_key = Some("secret".into());
    let (http, tokens) = (http(), Tokens::default());
    let cx = Cx {
        http: &http,
        cfg: &cfg,
        tokens: &tokens,
        key_service: &http,
    };

    let file = pixeldrain::resolve(&cx, "https://pixeldrain.com/u/abc")
        .await
        .unwrap();
    assert_eq!(file.len(), 1);
    assert_eq!(file[0].url, format!("{base}/api/file/abc?download"));
    assert_eq!(
        (file[0].name.as_deref(), file[0].size),
        (Some("a.zip"), Some(1234))
    );
    let sha = Checksum::from_hex(Algo::Sha256, &"ab".repeat(32));
    assert_eq!(file[0].checksum, sha);
    // Basic auth with an empty user and the key as password.
    assert_eq!(
        file[0].headers.get("Authorization").map(String::as_str),
        Some("Basic OnNlY3JldA==")
    );

    let list = pixeldrain::resolve(&cx, "https://pixeldrain.com/l/xyz")
        .await
        .unwrap();
    let names: Vec<_> = list
        .iter()
        .map(|e| (e.name.clone().unwrap(), e.url.clone()))
        .collect();
    assert_eq!(
        names,
        [
            ("one.rar".into(), format!("{base}/api/file/f1?download")),
            ("two.rar".into(), format!("{base}/api/file/f2?download"))
        ]
    );

    assert!(matches!(
        pixeldrain::resolve(&cx, "https://pixeldrain.com/u/busy").await,
        Err(ResolveError::NeedsUser(m)) if m.contains("captcha")
    ));
    assert_eq!(
        pixeldrain::resolve(&cx, "https://pixeldrain.com/u/gone").await,
        Err(ResolveError::failed("pixeldrain 404"))
    );
    assert_eq!(
        pixeldrain::probe(&cx, "https://pixeldrain.com/u/abc").await,
        Some(Probed {
            name: "a.zip".into(),
            size: Some(1234),
            checksum: sha,
        })
    );
    assert_eq!(
        pixeldrain::probe(&cx, "https://pixeldrain.com/l/xyz").await,
        None
    );
}

#[tokio::test]
async fn gofile_asks_nothing_without_an_account_and_lists_folders_with_one() {
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    let app = Router::new().route(
        "/contents/{id}",
        get(move |Path(id): Path<String>, Query(q): Query<HashMap<String, String>>, headers: HeaderMap| {
            counter.fetch_add(1, Ordering::SeqCst);
            async move {
                assert!(!q.contains_key("wt"), "no website token");
                assert_eq!(headers.get("authorization").unwrap(), "Bearer premium");
                if id == "locked" {
                    return Json(json!({"status": "error-passwordRequired"}));
                }
                Json(json!({"status": "ok", "data": {"children": {
                    "1": {"type": "file", "name": "x.7z", "size": 5, "link": "https://store.example/x.7z", "md5": "0123456789abcdef0123456789abcdef"},
                    "2": {"type": "folder", "children": {
                        "3": {"type": "file", "name": "y.7z", "size": 6, "link": "https://store.example/y.7z"}
                    }}
                }}}))
            }
        }),
    );
    let base = serve(app).await;
    let mut cfg = config(&base);
    let (http, tokens) = (http(), Tokens::default());

    let cx = Cx {
        http: &http,
        cfg: &cfg,
        tokens: &tokens,
        key_service: &http,
    };
    assert!(matches!(
        gofile::resolve(&cx, "https://gofile.io/d/abc").await,
        Err(ResolveError::NeedsUser(_))
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 0, "gofile was not asked");

    cfg.gofile_token = Some("premium".into());
    let cx = Cx {
        http: &http,
        cfg: &cfg,
        tokens: &tokens,
        key_service: &http,
    };
    let entries = gofile::resolve(&cx, "https://gofile.io/d/abc")
        .await
        .unwrap();
    let names: Vec<_> = entries.iter().map(|e| e.name.clone().unwrap()).collect();
    assert_eq!(names, ["x.7z", "y.7z"]);
    assert_eq!(
        entries[0].checksum,
        Checksum::from_hex(Algo::Md5, "0123456789abcdef0123456789abcdef")
    );
    assert_eq!(entries[1].checksum, None);
    assert_eq!(
        entries[0].headers.get("Cookie").map(String::as_str),
        Some("accountToken=premium")
    );
    assert!(matches!(
        gofile::resolve(&cx, "https://gofile.io/d/locked").await,
        Err(ResolveError::NeedsUser(_))
    ));
}

#[tokio::test]
async fn buzzheavier_redirects_rate_limits_and_gone_files() {
    let app = Router::new().route(
        "/{id}/download",
        get(|Path(id): Path<String>, headers: HeaderMap| async move {
            assert_eq!(headers.get("hx-request").unwrap(), "true");
            assert!(headers.get("referer").is_some());
            match id.as_str() {
                "ok" => (StatusCode::NO_CONTENT, [("hx-redirect", "/dl/ok.bin")]).into_response(),
                "busy" => (StatusCode::TOO_MANY_REQUESTS, [("retry-after", "7")]).into_response(),
                _ => StatusCode::NO_CONTENT.into_response(),
            }
        }),
    );
    let base = serve(app).await;
    let cfg = config(&base);
    let (http, tokens) = (http(), Tokens::default());
    let cx = Cx {
        http: &http,
        cfg: &cfg,
        tokens: &tokens,
        key_service: &http,
    };

    let e = buzzheavier::resolve(&cx, "https://buzzheavier.com/ok")
        .await
        .unwrap();
    assert_eq!(e[0].url, format!("{base}/dl/ok.bin"));
    assert_eq!(
        e[0].headers.get("Referer").map(String::as_str),
        Some("https://buzzheavier.com/ok")
    );
    assert!(matches!(
        buzzheavier::resolve(&cx, "https://buzzheavier.com/busy").await,
        Err(ResolveError::Wait { seconds: 7, .. })
    ));
    assert!(matches!(
        buzzheavier::resolve(&cx, "https://buzzheavier.com/gone").await,
        Err(ResolveError::NeedsUser(_))
    ));
}

#[tokio::test]
async fn onefichier_api_key_and_free_mode() {
    let app = Router::new()
        .route(
            "/v1/download/get_token.cgi",
            post(|headers: HeaderMap, Json(body): Json<Value>| async move {
                assert_eq!(headers.get("authorization").unwrap(), "Bearer key");
                assert_eq!(body["url"], "https://1fichier.com/?abc");
                Json(json!({"status": "OK", "url": "https://a-1.1fichier.com/c123"}))
            }),
        )
        .route(
            "/",
            post(|Query(q): Query<HashMap<String, String>>, Form(form): Form<HashMap<String, String>>| async move {
                assert_eq!(form.get("dl_no_ssl").map(String::as_str), Some("on"));
                assert!(!q.contains_key("af"), "the affiliate parameter is dropped");
                if q.contains_key("ready") {
                    r#"<a href="https://a-2.1fichier.com/p456" class="ok btn-general btn-orange">Download</a>"#
                } else if q.contains_key("later") {
                    "You must wait 4 minutes between downloads"
                } else {
                    "<form>captcha</form>"
                }
            }),
        );
    let base = serve(app).await;
    let mut cfg = config(&base);
    let (http, tokens) = (http(), Tokens::default());
    let cx = Cx {
        http: &http,
        cfg: &cfg,
        tokens: &tokens,
        key_service: &http,
    };

    let e = onefichier::resolve(&cx, &format!("{base}/?ready&af=99"))
        .await
        .unwrap();
    assert_eq!(e[0].url, "https://a-2.1fichier.com/p456");
    assert_eq!(
        onefichier::resolve(&cx, &format!("{base}/?later")).await,
        Err(ResolveError::Wait {
            seconds: 245,
            message: "1fichier free cooldown".into()
        })
    );
    assert!(matches!(
        onefichier::resolve(&cx, &format!("{base}/?abc")).await,
        Err(ResolveError::NeedsUser(_))
    ));

    cfg.onefichier_key = Some("key".into());
    let cx = Cx {
        http: &http,
        cfg: &cfg,
        tokens: &tokens,
        key_service: &http,
    };
    let e = onefichier::resolve(&cx, "https://1fichier.com/?abc&af=1")
        .await
        .unwrap();
    assert_eq!(e[0].url, "https://a-1.1fichier.com/c123");
}

#[tokio::test]
async fn rapidgator_logs_in_once_and_relogs_after_errors() {
    let logins = Arc::new(AtomicUsize::new(0));
    let counter = logins.clone();
    let app = Router::new()
        .route(
            "/api/v2/user/login",
            get(move |Query(q): Query<HashMap<String, String>>| {
                counter.fetch_add(1, Ordering::SeqCst);
                async move {
                    assert_eq!((q["login"].as_str(), q["password"].as_str()), ("me", "pw"));
                    Json(json!({"status": 200, "response": {"token": "t1"}}))
                }
            }),
        )
        .route(
            "/api/v2/file/download",
            get(|Query(q): Query<HashMap<String, String>>| async move {
                assert_eq!(q["token"], "t1");
                if q["file_id"].starts_with("0") {
                    Json(json!({"status": 404, "details": "File not found"}))
                } else {
                    Json(json!({"status": 200, "response": {"download_url": "https://dl.rapidgator.example/f"}}))
                }
            }),
        );
    let base = serve(app).await;
    let mut cfg = config(&base);
    let (http, tokens) = (http(), Tokens::default());
    {
        let cx = Cx {
            http: &http,
            cfg: &cfg,
            tokens: &tokens,
            key_service: &http,
        };
        assert!(
            matches!(rapidgator::resolve(&cx, "https://rapidgator.net/file/x").await, Err(ResolveError::Failed(m)) if m.contains("premium"))
        );
    }
    cfg.rapidgator_user = Some("me".into());
    cfg.rapidgator_pass = Some("pw".into());
    let cx = Cx {
        http: &http,
        cfg: &cfg,
        tokens: &tokens,
        key_service: &http,
    };
    let good = "https://rapidgator.net/file/abcdefabcdefabcdefabcdefabcdefab/game.part1.rar.html";
    let e = rapidgator::resolve(&cx, good).await.unwrap();
    assert_eq!(e[0].url, "https://dl.rapidgator.example/f");
    assert_eq!(
        e[0].name.as_deref(),
        Some("game.part1.rar"),
        "the page's .html is no part of the file's name"
    );
    rapidgator::resolve(&cx, good).await.unwrap();
    assert_eq!(logins.load(Ordering::SeqCst), 1);
    assert!(
        matches!(rapidgator::resolve(&cx, "https://rapidgator.net/folder/1").await, Err(ResolveError::Failed(m)) if m.contains("url"))
    );
    assert_eq!(
        logins.load(Ordering::SeqCst),
        1,
        "a link that names no file costs no login"
    );
    let missing = "https://rapidgator.net/file/0bcdefabcdefabcdefabcdefabcdefab";
    assert_eq!(
        rapidgator::resolve(&cx, missing).await,
        Err(ResolveError::failed("rapidgator: File not found"))
    );
    rapidgator::resolve(&cx, good).await.unwrap();
    assert_eq!(logins.load(Ordering::SeqCst), 2, "an error drops the token");
}

#[tokio::test]
async fn nitroflare_premium_link() {
    let app = Router::new().route(
        "/api/v2/getDownloadLink",
        get(|Query(q): Query<HashMap<String, String>>| async move {
            assert_eq!((q["user"].as_str(), q["premiumKey"].as_str(), q["file"].as_str()), ("me", "k", "ABC123"));
            Json(json!({"type": "success", "result": {"url": "https://dl.nitro.example/f", "name": "n.zip", "size": "42"}}))
        }),
    );
    let base = serve(app).await;
    let mut cfg = config(&base);
    cfg.nitroflare_user = Some("me".into());
    cfg.nitroflare_key = Some("k".into());
    let (http, tokens) = (http(), Tokens::default());
    let cx = Cx {
        http: &http,
        cfg: &cfg,
        tokens: &tokens,
        key_service: &http,
    };
    let e = nitroflare::resolve(&cx, "https://nitroflare.com/view/ABC123/n.zip")
        .await
        .unwrap();
    assert_eq!(
        (e[0].url.as_str(), e[0].name.as_deref(), e[0].size),
        ("https://dl.nitro.example/f", Some("n.zip"), Some(42))
    );
}

#[tokio::test]
async fn generic_links_and_probes() {
    let app = Router::new()
        .route(
            "/file/data.bin",
            get(|| async {
                (
                    [("content-md5", "AAAAAAAAAAAAAAAAAAAAAA==")],
                    vec![7u8; 300],
                )
            }),
        )
        .route(
            "/nohead/data.bin",
            get(|headers: HeaderMap| async move {
                assert_eq!(headers.get("range").unwrap(), "bytes=0-0");
                (
                    StatusCode::PARTIAL_CONTENT,
                    [("content-range", "bytes 0-0/999")],
                    vec![1u8],
                )
            })
            .head(|| async { StatusCode::METHOD_NOT_ALLOWED }),
        )
        .route(
            "/stall/data.bin",
            get(|headers: HeaderMap| async move {
                assert_eq!(headers.get("range").unwrap(), "bytes=0-0");
                (
                    StatusCode::PARTIAL_CONTENT,
                    [("content-range", "bytes 0-0/999")],
                    vec![1u8],
                )
            })
            .head(|| async {
                // Never answers HEAD, like transfer.it.
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                StatusCode::OK
            }),
        )
        .route(
            "/page",
            get(|| async {
                (
                    [("content-type", "text/html")],
                    "<title>Download big.part2.rar - Host</title>",
                )
            }),
        )
        .route(
            "/hidden",
            get(|| async {
                (
                    StatusCode::PARTIAL_CONTENT,
                    [
                        ("content-disposition", "attachment; filename=\"same.bin\""),
                        ("content-range", "bytes 0-65535/500000"),
                        // Of these 16 bytes only, so not the file's.
                        ("content-md5", "AAAAAAAAAAAAAAAAAAAAAA=="),
                        (
                            "repr-digest",
                            "sha-256=:AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE=:",
                        ),
                    ],
                    vec![0u8; 16],
                )
            }),
        );
    let base = serve(app).await;
    let cfg = config(&base);
    let (http, tokens) = (http(), Tokens::default());
    let cx = Cx {
        http: &http,
        cfg: &cfg,
        tokens: &tokens,
        key_service: &http,
    };

    let e = generic::resolve(&cx, &format!("{base}/file/data.bin"))
        .await
        .unwrap();
    assert_eq!(
        (e[0].name.as_deref(), e[0].size),
        (Some("data.bin"), Some(300))
    );
    assert_eq!(
        e[0].checksum,
        Checksum::from_hex(Algo::Md5, &"0".repeat(32))
    );
    let e = generic::resolve(&cx, &format!("{base}/nohead/data.bin"))
        .await
        .unwrap();
    assert_eq!(e[0].size, Some(999));
    // A host that never answers HEAD still gets its chance: the stalled HEAD
    // times out and the same ranged GET is sent instead.
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_millis(300))
        .build()
        .unwrap();
    let quick = Http {
        no_redirect: client.clone(),
        client,
        via_proxy: false,
        user_agent: String::new(),
    };
    let stall_cx = Cx {
        http: &quick,
        cfg: &cfg,
        tokens: &tokens,
        key_service: &quick,
    };
    let e = generic::resolve(&stall_cx, &format!("{base}/stall/data.bin"))
        .await
        .unwrap();
    assert_eq!(
        (e[0].name.as_deref(), e[0].size),
        (Some("data.bin"), Some(999))
    );
    assert!(matches!(
        generic::resolve(&cx, &format!("{base}/page")).await,
        Err(ResolveError::NeedsUser(_))
    ));

    assert_eq!(
        generic::probe(&cx, &format!("{base}/hidden")).await,
        Some(Probed {
            name: "same.bin".into(),
            size: Some(500_000),
            checksum: Checksum::from_hex(Algo::Sha256, &"01".repeat(32)),
        })
    );
    assert_eq!(
        generic::probe(&cx, &format!("{base}/page")).await,
        Some(Probed {
            name: "big.part2.rar".into(),
            size: None,
            checksum: None,
        })
    );
    assert_eq!(generic::probe(&cx, &format!("{base}/missing")).await, None);
}

#[tokio::test]
async fn fileguard_solves_the_proof_of_work_and_lists_links() {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use sha2::Digest;

    let salt = "cc348abfe0f7d4fded4b8dba?expires=1792121341";
    let number = 13_186u64;
    let challenge = {
        let mut hash = sha2::Sha256::new();
        hash.update(salt.as_bytes());
        hash.update(number.to_string().as_bytes());
        to_hex(&hash.finalize())
    };
    let solved_html = r#"
        <div class="folder-title"><h1>Sledders New Horizons-RUNE</h1></div>
        <div class="link-group" data-host="1fichier.com">
          <a class="target-link" href="https://1fichier.com/?cu5oghhjkskhuzap8w43&amp;af=2701919">1fichier</a>
        </div>
        <div class="link-group" data-host="pixeldrain.com">
          <a class="target-link" href="https://pixeldrain.com/u/VF18oYXa">pixeldrain</a>
        </div>"#;
    let challenge_route = challenge.clone();
    let challenge_post = challenge.clone();
    // The gate's form, its widget fetching the challenge from `challenge_url`
    // (none: the widget does not say).
    let gate = |challenge_url: Option<&'static str>| {
        let widget = challenge_url
            .map(|url| format!(r#"<altcha-widget challengeurl="{url}" hidelogo></altcha-widget>"#))
            .unwrap_or_default();
        let html = format!(
            r#"<form method="post" action="/vault/abc/1fichier">
                 <input type="hidden" name="csrf_token" value="tok-1">
                 {widget}
               </form>"#
        );
        move || {
            let html = html.clone();
            async move { ([("content-type", "text/html")], html) }
        }
    };
    let sha1 = || async {
        Json(json!({
            "algorithm": "SHA-1",
            "challenge": "00".repeat(20),
            "maxNumber": 10u64,
            "salt": "s",
            "signature": "sig",
        }))
    };
    let app = Router::new()
        .route(
            "/vault/abc/1fichier",
            // The challenge where the page's widget says, not where
            // fileguard.cc has it today.
            get(gate(Some("/pow/challenge?v=1&amp;b=2"))).post(
                move |Form(form): Form<HashMap<String, String>>| {
                    let challenge = challenge_post.clone();
                    async move {
                        assert_eq!(form.get("csrf_token").map(String::as_str), Some("tok-1"));
                        let raw = STANDARD.decode(form.get("altcha").unwrap()).unwrap();
                        let solved: Value = serde_json::from_slice(&raw).unwrap();
                        assert_eq!(solved["number"].as_u64(), Some(13_186));
                        assert_eq!(solved["challenge"].as_str(), Some(challenge.as_str()));
                        ([("content-type", "text/html")], solved_html)
                    }
                },
            ),
        )
        .route(
            "/pow/challenge",
            get(move |Query(q): Query<HashMap<String, String>>| {
                let challenge = challenge_route.clone();
                async move {
                    assert_eq!(
                        (
                            q.get("v").map(String::as_str),
                            q.get("b").map(String::as_str)
                        ),
                        (Some("1"), Some("2"))
                    );
                    Json(json!({
                        "algorithm": "SHA-256",
                        "challenge": challenge,
                        "maxNumber": 180_000u64,
                        "salt": "cc348abfe0f7d4fded4b8dba?expires=1792121341",
                        "signature": "sig",
                    }))
                }
            }),
        )
        .route("/vault/sha1", get(gate(Some("/pow/sha1"))))
        .route("/pow/sha1", get(sha1))
        .route("/vault/check", get(gate(Some("/pow/check"))))
        .route(
            "/pow/check",
            get(|| async {
                (
                    [("content-type", "text/html")],
                    "<html>Checking your browser</html>",
                )
            }),
        )
        // A page whose widget does not say: fileguard.cc's own address.
        .route("/vault/plain", get(gate(None)))
        .route("/altcha_challenge.php", get(sha1))
        // A session that passed this vault's gate before: the links, no form.
        .route(
            "/vault/open/mega",
            get(move || async move { ([("content-type", "text/html")], solved_html) }),
        )
        // A solve refused, as a burst of them is.
        .route(
            "/vault/busy",
            get(gate(Some("/pow/challenge?v=1&amp;b=2")))
                .post(|| async { (StatusCode::FORBIDDEN, "Forbidden") }),
        );
    let base = serve(app).await;
    let cfg = config(&base);
    let (http, tokens) = (http(), Tokens::default());
    let cx = Cx {
        http: &http,
        cfg: &cfg,
        tokens: &tokens,
        key_service: &http,
    };

    let found = fileguard::unwrap(&cx, &format!("{base}/vault/abc/1fichier"))
        .await
        .unwrap();
    assert_eq!(found.title.as_deref(), Some("Sledders New Horizons-RUNE"));
    // One link per host: one file, every host a mirror.
    let file = &found.files[..];
    assert_eq!(file.len(), 1);
    assert_eq!(
        (file[0].url.as_str(), file[0].name.as_deref()),
        (
            "https://1fichier.com/?cu5oghhjkskhuzap8w43&af=2701919",
            None
        )
    );
    assert_eq!(file[0].mirrors, ["https://pixeldrain.com/u/VF18oYXa"]);

    // A page that shows its links already is read as it is.
    let open = fileguard::unwrap(&cx, &format!("{base}/vault/open/mega"))
        .await
        .unwrap();
    assert_eq!(open, found);

    let err = fileguard::unwrap(&cx, &format!("{base}/vault/busy"))
        .await
        .unwrap_err();
    assert!(
        matches!(&err, ResolveError::Network(m) if m.contains("refused the solve (HTTP 403)")),
        "worth another try, not a page for a person: {err:?}"
    );

    let err = fileguard::unwrap(&cx, &format!("{base}/vault/gone"))
        .await
        .unwrap_err();
    assert!(
        matches!(&err, ResolveError::Failed(m) if m.contains("404")),
        "{err:?}"
    );

    // What Haul cannot solve, a person in a browser still can.
    for (vault, says) in [
        ("sha1", "uses SHA-1"),
        ("check", "without its proof-of-work challenge"),
        ("plain", "uses SHA-1"),
    ] {
        let err = fileguard::unwrap(&cx, &format!("{base}/vault/{vault}"))
            .await
            .unwrap_err();
        assert!(
            matches!(&err, ResolveError::NeedsUser(m) if m.contains(says)),
            "{vault}: {err:?}"
        );
    }
}

#[tokio::test]
async fn filecrypt_dlc_links_become_entries() {
    use aes::cipher::{BlockModeEncrypt, KeyIvInit, block_padding::NoPadding};
    use base64::engine::general_purpose::STANDARD;

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
        r#"<dlc><header/><content><package name="{}"><file><url>{}</url><filename>{}</filename></file><file><url>{}</url></file><file><url>{}</url><filename>{}</filename></file></package></content></dlc>"#,
        e("My Package"),
        e("https://host.example/f/abc"),
        e("abc.part1.rar"),
        e("https://other.example/x.zip"),
        e("https://third.example/d/abc"),
        e("n.A."),
    );
    let empty_xml = format!(
        r#"<dlc><header/><content><package name="{}"/></content></dlc>"#,
        e("My Package"),
    );
    let file_key = *b"0123456789abcdef";
    let wrap = |xml: String| {
        let body = STANDARD.encode(encrypt(
            &file_key,
            &file_key,
            STANDARD.encode(xml).as_bytes(),
        ));
        format!("{body}{}", "K".repeat(88)).into_bytes()
    };
    let dlc: Vec<u8> = wrap(xml);
    let empty: Vec<u8> = wrap(empty_xml);
    let rc = STANDARD.encode(encrypt(key, iv, &file_key));

    let app = Router::new()
        .route(
            "/DLC/abc123.dlc",
            get(move || {
                let dlc = dlc.clone();
                async move { dlc }
            }),
        )
        .route(
            "/DLC/empty.dlc",
            get(move || {
                let empty = empty.clone();
                async move { empty }
            }),
        )
        .route(
            "/DLC/unlisted.dlc",
            // An id the solved page did not hand out: a container of nothing.
            get(|| async { "" }),
        )
        .route(
            "/DLC/gate.dlc",
            // A wrong id: the host answers its page rather than a container.
            get(|| async {
                (
                    [("content-type", "text/html")],
                    "<html><title>Please solve the captcha</title></html>",
                )
            }),
        )
        .route(
            "/dlc",
            get(
                move |Query(q): Query<HashMap<String, String>>, headers: HeaderMap| {
                    let rc = rc.clone();
                    async move {
                        // Reached through the key service's own route.
                        assert_eq!(
                            headers.get("user-agent").and_then(|v| v.to_str().ok()),
                            Some("key-service-agent")
                        );
                        assert_eq!(q.get("srcType").map(String::as_str), Some("dlc"));
                        assert_eq!(q.get("data").map(String::len), Some(88));
                        format!("<rc>{rc}</rc>")
                    }
                },
            ),
        );
    let base = serve(app).await;
    let cfg = config(&base);
    let (http, tokens) = (http(), Tokens::default());
    let key_service = Http::new(None, "key-service-agent").unwrap();
    let cx = Cx {
        http: &http,
        cfg: &cfg,
        tokens: &tokens,
        key_service: &key_service,
    };

    let found = filecrypt::unwrap(&cx, &format!("{base}/DLC/abc123.dlc"))
        .await
        .unwrap();
    assert_eq!(found.title.as_deref(), Some("My Package"));
    let entries = found.files;
    assert!(entries.iter().all(|e| e.deferred));
    assert_eq!(entries[0].package.as_deref(), Some("My Package"));
    assert_eq!(
        entries
            .iter()
            .map(|e| (e.url.as_str(), e.name.as_deref()))
            .collect::<Vec<_>>(),
        [
            ("https://host.example/f/abc", Some("abc.part1.rar")),
            ("https://other.example/x.zip", Some("x.zip")),
            // "n.A." is the container's way of saying the name is unknown.
            ("https://third.example/d/abc", None),
        ]
    );

    let err = filecrypt::unwrap(&cx, &format!("{base}/DLC/empty.dlc"))
        .await
        .unwrap_err();
    assert!(
        matches!(&err, ResolveError::Failed(m) if m.contains("no links")),
        "{err:?}"
    );

    let err = filecrypt::unwrap(&cx, &format!("{base}/DLC/unlisted.dlc"))
        .await
        .unwrap_err();
    assert!(
        matches!(&err, ResolveError::Failed(m) if m.contains("empty .dlc")),
        "{err:?}"
    );

    let err = filecrypt::unwrap(&cx, &format!("{base}/DLC/gate.dlc"))
        .await
        .unwrap_err();
    assert!(
        matches!(&err, ResolveError::NeedsUser(m) if m.contains("web page")),
        "{err:?}"
    );

    let err = filecrypt::unwrap(&cx, &format!("{base}/Link/abc.html"))
        .await
        .unwrap_err();
    assert!(
        matches!(&err, ResolveError::NeedsUser(m) if m.contains(".dlc")),
        "{err:?}"
    );
}

/// filecrypt.cc's checkbox, as its page and widget ask for it: a probe id,
/// a challenge for what the widget's checks found, a SHA-1 proof of work and
/// the report of a person's click; the solved page's Click'n'Load data holds
/// the links.
#[tokio::test]
async fn filecrypt_answers_its_checkbox() {
    use aes::cipher::{BlockModeEncrypt, KeyIvInit, block_padding::NoPadding};
    use base64::engine::general_purpose::STANDARD;
    use sha1::{Digest, Sha1};

    let key = *b"6229761275967424";
    let jk: String = key.iter().map(|b| format!("{b:02x}")).collect();
    let plain = b"https://host.example/f/abc.part1.rar\r\nhttps://other.example/d/xyz\r\n";
    let mut buf = plain.to_vec();
    buf.resize(plain.len().div_ceil(16) * 16, 0);
    let len = buf.len();
    let crypted = STANDARD.encode(
        cbc::Encryptor::<aes::Aes128>::new_from_slices(&key, &key)
            .unwrap()
            .encrypt_padded::<NoPadding>(&mut buf, len)
            .unwrap(),
    );
    let solved = format!(
        r#"<h2>My &amp; Release</h2><form onsubmit="try {{ CNLPOP('5E55537ED1', '{jk}', '{crypted}', 'My Release - filecrypt.cc'); }} catch(e){{}}">"#
    );
    let gate = |base: &str| {
        format!(
            r#"<h2>Security Check</h2><form id="cform"><div class="pow-captcha" id="pow-captcha" data-state="idle"
                 data-session="/captchasession/S1.json" data-worker="/js/w.js"
                 data-px="{base}/px-down,{base}/px">
               <input type="hidden" name="pow_id" value=""></div>
               <!-- <input type="password" name="password" id="p4assw0rt"> --></form>"#
        )
    };
    let sessions = Arc::new(AtomicUsize::new(0));
    let app = {
        let (solved, sessions) = (solved.clone(), sessions.clone());
        Router::new()
            .route(
                "/Container/{page}",
                get({
                    let solved = solved.clone();
                    move |Path(page): Path<String>, headers: HeaderMap| {
                        let solved = solved.clone();
                        async move {
                            let host = headers["host"].to_str().unwrap().to_string();
                            match page.as_str() {
                                "open.html" | "refuse.html" | "dlc.html" => gate(&format!("http://{host}")),
                                // A session that passed the gate before.
                                "seen.html" => solved,
                                "locked.html" => r#"<input type="password" name="password">"#.into(),
                                "plain.html" => "<p>Maintenance</p>".into(),
                                _ => return StatusCode::NOT_FOUND.into_response(),
                            }
                            .into_response()
                        }
                    }
                })
                .post({
                    let solved = solved.clone();
                    move |Path(page): Path<String>, Form(f): Form<HashMap<String, String>>| {
                        let solved = solved.clone();
                        async move {
                            assert_eq!(f["pow_id"], "C1");
                            assert_eq!(f["pow_x"], "2.0.63.W10=");
                            assert_eq!(f["pow_pauses"], "0");
                            assert!(f["pow_elapsed"].parse::<u64>().unwrap() >= 1);
                            let digest = Sha1::digest(format!("c0ffee:{}", f["pow_nonce"]));
                            assert!(digest[0] == 0 && digest[1] >> 4 == 0, "12 zero bits");
                            let report = filecrypt::decode_report(&f["pow_data"]);
                            assert!(report["moves"].as_u64().unwrap() > 0);
                            assert_eq!(report["ui"], true);
                            assert_eq!(report["workerData"]["userAgent"], "test-agent");
                            match page.as_str() {
                                "open.html" => solved,
                                "dlc.html" => r#"<h2>Dlc Only</h2><button onclick="DownloadDLC(this.getAttribute('data-Oft'));" data-Oft="unlisted">"#.into(),
                                _ => format!("<h2>Security Check</h2>{}", r#"<div id="pow-captcha" data-session="/captchasession/S1.json">"#),
                            }
                        }
                    }
                }),
            )
            .route(
                "/captchasession/S1.json",
                post(move |headers: HeaderMap, Form(f): Form<HashMap<String, String>>| {
                    let sessions = sessions.clone();
                    async move {
                        sessions.fetch_add(1, Ordering::SeqCst);
                        assert!(headers["referer"].to_str().unwrap().contains("/Container/"));
                        assert_eq!(f["pow_x"], "2.0.63.W10=");
                        assert_eq!(f["pow_y"], "probe-id", "the probe that answered");
                        assert!(!f["pow_yn"].is_empty() && !f["tz"].is_empty());
                        assert_eq!(f["wt"], "0");
                        Json(json!({"success": true, "autosolve": false,
                            "challenge": {"id": "C1", "challenge": "c0ffee", "difficulty": 12}}))
                    }
                }),
            )
            .route(
                "/px",
                get(|Query(q): Query<HashMap<String, String>>| async move {
                    assert!(!q["t"].is_empty());
                    Json(json!({"ok": 1, "cid": "probe-id"}))
                }),
            )
            .route("/px-down", get(|| async { StatusCode::BAD_GATEWAY }))
            .route("/DLC/unlisted.dlc", get(|| async { "" }))
    };
    let base = serve(app).await;
    let cfg = config(&base);
    let (http, tokens) = (http(), Tokens::default());
    let cx = Cx {
        http: &http,
        cfg: &cfg,
        tokens: &tokens,
        key_service: &http,
    };

    let found = filecrypt::unwrap(&cx, "https://filecrypt.cc/Container/open.html")
        .await
        .unwrap();
    assert_eq!(found.title.as_deref(), Some("My & Release"));
    assert_eq!(found.files.len(), 1, "one file, on two hosts");
    assert_eq!(found.files[0].url, "https://host.example/f/abc.part1.rar");
    assert_eq!(found.files[0].mirrors, ["https://other.example/d/xyz"]);
    assert_eq!(found.files[0].name.as_deref(), Some("abc.part1.rar"));
    assert!(found.files[0].deferred);
    assert_eq!(sessions.load(Ordering::SeqCst), 1);

    // A page that shows the links already needs no solve.
    let found = filecrypt::unwrap(&cx, "https://www.filecrypt.cc/Container/seen.html")
        .await
        .unwrap();
    assert_eq!(found.files.len(), 1);
    assert_eq!(sessions.load(Ordering::SeqCst), 1);

    // A solved page without Click'n'Load data: its .dlc.
    let err = filecrypt::unwrap(&cx, "https://filecrypt.cc/Container/dlc.html")
        .await
        .unwrap_err();
    assert!(
        matches!(&err, ResolveError::Failed(m) if m.contains("empty .dlc")),
        "{err:?}"
    );

    for (page, says) in [
        ("refuse.html", "turned Haul's answer"),
        ("locked.html", "password"),
        ("plain.html", "without its checkbox"),
    ] {
        let err = filecrypt::unwrap(&cx, &format!("https://filecrypt.cc/Container/{page}"))
            .await
            .unwrap_err();
        assert!(
            matches!(&err, ResolveError::NeedsUser(m) if m.contains(says)),
            "{page}: {err:?}"
        );
    }
    let err = filecrypt::unwrap(&cx, "https://filecrypt.cc/Container/gone.html")
        .await
        .unwrap_err();
    assert_eq!(err, ResolveError::Failed("HTTP 404".into()));
}

/// Pages may come compressed; a file's size and bytes are asked for as
/// stored, since resuming counts them.
#[tokio::test]
async fn files_are_asked_for_uncompressed() {
    let app = Router::new().route(
        "/f/data.bin",
        get(|headers: HeaderMap| async move {
            let encoding = headers.get("accept-encoding").and_then(|v| v.to_str().ok());
            assert_eq!(encoding, Some("identity"));
            ([("content-length", "4")], "data")
        }),
    );
    let base = serve(app).await;
    let cfg = config(&base);
    let (http, tokens) = (http(), Tokens::default());
    let cx = Cx {
        http: &http,
        cfg: &cfg,
        tokens: &tokens,
        key_service: &http,
    };
    let e = generic::resolve(&cx, &format!("{base}/f/data.bin"))
        .await
        .unwrap();
    assert_eq!(e[0].size, Some(4));
    assert!(
        generic::probe(&cx, &format!("{base}/f/data.bin"))
            .await
            .is_some()
    );
}

#[tokio::test]
async fn vikingfile_checks_the_file_then_wants_a_person() {
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let app = Router::new().route(
        "/api/check-file",
        post(
            move |Form(form): Form<HashMap<String, String>>| async move {
                // The first request meets a burst limit.
                if seen.fetch_add(1, Ordering::SeqCst) == 0 {
                    return (StatusCode::TOO_MANY_REQUESTS, [("retry-after", "1")]).into_response();
                }
                let hash = form["hash"].clone();
                Json(match hash.as_str() {
                    "acXWYKlHX0" => json!([{
                        "exist": true, "hash": hash, "name": "big.rar", "size": 5_320_312_809_u64
                    }]),
                    _ => json!([{ "exist": false, "hash": hash }]),
                })
                .into_response()
            },
        ),
    );
    let base = serve(app).await;
    let cfg = config(&base);
    let (http, tokens) = (http(), Tokens::default());
    let cx = Cx {
        http: &http,
        cfg: &cfg,
        tokens: &tokens,
        key_service: &http,
    };

    let probed = probe(&cx, "https://vikingfile.com/f/acXWYKlHX0")
        .await
        .unwrap();
    assert_eq!(
        (probed.name.as_str(), probed.size),
        ("big.rar", Some(5_320_312_809))
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2, "asked again after the 429");
    let spaced = tokens
        .vikingfile_next
        .lock()
        .await
        .expect("the next lookup is spaced");
    assert!(
        spaced > std::time::Instant::now(),
        "the next lookup waits its turn"
    );
    assert!(matches!(
        resolve(&cx, "https://vik1ngfile.site/f/acXWYKlHX0").await,
        Err(ResolveError::NeedsUser(_))
    ));
    assert!(probe(&cx, "https://vikingfile.com/f/gone").await.is_none());
    assert!(matches!(
        resolve(&cx, "https://vikingfile.com/f/gone").await,
        Err(ResolveError::Failed(m)) if m.contains("gone")
    ));
}

#[tokio::test]
async fn mirrored_lists_one_file_on_every_host_that_took_it() {
    let app = Router::new()
        .route(
            "/files/{id}/",
            get(|Path(id): Path<String>, Query(q): Query<HashMap<String, String>>| async move {
                assert_eq!(id, "9EC6E16F");
                if q.contains_key("hash") {
                    // The links page: its script asks for the hosts' table.
                    return r#"<script>ajaxRequest.open("GET", "/mirstats.php?uid=9EC6E16F&amp;tmpID=t", true);</script>"#.to_string();
                }
                r#"<h1>You have requested the file <span>R.part1.rar</span></h1>
                   <a href="https://www.mirrored.to/files/9EC6E16F/?hash=b8bd&amp;dl=0">Click to view the download links</a>"#
                    .to_string()
            }),
        )
        .route(
            "/mirstats.php",
            get(|Query(q): Query<HashMap<String, String>>| async move {
                assert_eq!(q.get("tmpID").map(String::as_str), Some("t"));
                r#"<table><tr><td><img alt="GoFileIo"></td><td><a href="/getlink/9EC6E16F/1/?hid=a&amp;tid=b">Download</a></td></tr>
                   <tr><td><img alt="OneFichier"></td><td><a href="/getlink/9EC6E16F/2/?hid=c&amp;tid=b">Download</a></td></tr>
                   <tr><td><img alt="TurboBit"></td><td>Not Available</td></tr></table>"#
            }),
        )
        .route(
            "/getlink/{id}/{n}/",
            get(|Path((_, n)): Path<(String, u32)>| async move {
                let link = match n {
                    1 => "https://gofile.io/d/6b52TMUH",
                    _ => "https://1fichier.com/?abc123",
                };
                format!(r#"<code>{link}</code><button data-clipboard-text="{link}">Copy</button>"#)
            }),
        );
    let base = serve(app).await;
    let cfg = config(&base);
    let (http, tokens) = (http(), Tokens::default());
    let cx = Cx {
        http: &http,
        cfg: &cfg,
        tokens: &tokens,
        key_service: &http,
    };
    let url = "https://www.mirrored.to/files/9EC6E16F/";
    assert!(wraps(url));
    assert_eq!(wrapper_key(url), "mirrored.to/9EC6E16F");
    let found = unwrap(&cx, url).await.unwrap();
    assert_eq!(found.title.as_deref(), Some("R.part1.rar"));
    assert_eq!(found.files.len(), 1);
    let file = &found.files[0];
    assert_eq!(
        (
            file.name.as_deref(),
            file.url.as_str(),
            file.mirrors.as_slice()
        ),
        (
            Some("R.part1.rar"),
            "https://gofile.io/d/6b52TMUH",
            ["https://1fichier.com/?abc123".to_string()].as_slice()
        )
    );
}
