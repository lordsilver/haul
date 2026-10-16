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
async fn gofile_guest_token_and_folders() {
    let accounts = Arc::new(AtomicUsize::new(0));
    let counter = accounts.clone();
    let app = Router::new()
        .route(
            "/accounts",
            post(move || {
                counter.fetch_add(1, Ordering::SeqCst);
                async { Json(json!({"status": "ok", "data": {"token": "guest"}})) }
            }),
        )
        .route(
            "/contents/{id}",
            get(|Path(id): Path<String>, Query(q): Query<HashMap<String, String>>, headers: HeaderMap| async move {
                assert_eq!(q.get("wt").map(String::as_str), Some("4fd6sg89d7s6"));
                assert_eq!(headers.get("authorization").unwrap(), "Bearer guest");
                if id == "locked" {
                    return Json(json!({"status": "error-notPremium"}));
                }
                Json(json!({"status": "ok", "data": {"children": {
                    "1": {"type": "file", "name": "x.7z", "size": 5, "link": "https://store.example/x.7z", "md5": "0123456789abcdef0123456789abcdef"},
                    "2": {"type": "folder", "children": {
                        "3": {"type": "file", "name": "y.7z", "size": 6, "link": "https://store.example/y.7z"}
                    }}
                }}}))
            }),
        );
    let base = serve(app).await;
    let cfg = config(&base);
    let (http, tokens) = (http(), Tokens::default());
    let cx = Cx {
        http: &http,
        cfg: &cfg,
        tokens: &tokens,
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
        Some("accountToken=guest")
    );
    assert!(matches!(
        gofile::resolve(&cx, "https://gofile.io/d/locked").await,
        Err(ResolveError::NeedsUser(_))
    ));
    assert_eq!(
        accounts.load(Ordering::SeqCst),
        1,
        "the guest token is reused"
    );
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
    };
    let stall_cx = Cx {
        http: &quick,
        cfg: &cfg,
        tokens: &tokens,
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
    let app = Router::new()
        .route(
            "/vault/abc/1fichier",
            get(|| async {
                (
                    [("content-type", "text/html")],
                    r#"<form method="post" action="/vault/abc/1fichier">
                         <input type="hidden" name="csrf_token" value="tok-1">
                         <altcha-widget challengeurl="/altcha_challenge.php"></altcha-widget>
                       </form>"#,
                )
            })
            .post(move |Form(form): Form<HashMap<String, String>>| {
                let challenge = challenge_post.clone();
                async move {
                    assert_eq!(form.get("csrf_token").map(String::as_str), Some("tok-1"));
                    let raw = STANDARD.decode(form.get("altcha").unwrap()).unwrap();
                    let solved: Value = serde_json::from_slice(&raw).unwrap();
                    assert_eq!(solved["number"].as_u64(), Some(13_186));
                    assert_eq!(solved["challenge"].as_str(), Some(challenge.as_str()));
                    ([("content-type", "text/html")], solved_html)
                }
            }),
        )
        .route(
            "/altcha_challenge.php",
            get(move || {
                let challenge = challenge_route.clone();
                async move {
                    Json(json!({
                        "algorithm": "SHA-256",
                        "challenge": challenge,
                        "maxNumber": 180_000u64,
                        "salt": "cc348abfe0f7d4fded4b8dba?expires=1792121341",
                        "signature": "sig",
                    }))
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
    };

    let entries = fileguard::resolve(&cx, &format!("{base}/vault/abc/1fichier"))
        .await
        .unwrap();
    assert!(entries.iter().all(|e| e.deferred));
    assert_eq!(
        entries
            .iter()
            .map(|e| (e.url.as_str(), e.name.as_deref()))
            .collect::<Vec<_>>(),
        [
            (
                "https://1fichier.com/?cu5oghhjkskhuzap8w43&af=2701919",
                None
            ),
            ("https://pixeldrain.com/u/VF18oYXa", None),
        ]
    );

    let err = fileguard::resolve(&cx, &format!("{base}/vault/gone"))
        .await
        .unwrap_err();
    assert!(
        matches!(&err, ResolveError::Failed(m) if m.contains("404")),
        "{err:?}"
    );
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
            get(move |Query(q): Query<HashMap<String, String>>| {
                let rc = rc.clone();
                async move {
                    assert_eq!(q.get("srcType").map(String::as_str), Some("dlc"));
                    assert_eq!(q.get("data").map(String::len), Some(88));
                    format!("<rc>{rc}</rc>")
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
    };

    let entries = filecrypt::resolve(&cx, &format!("{base}/DLC/abc123.dlc"))
        .await
        .unwrap();
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

    let err = filecrypt::resolve(&cx, &format!("{base}/DLC/empty.dlc"))
        .await
        .unwrap_err();
    assert!(
        matches!(&err, ResolveError::Failed(m) if m.contains("no links")),
        "{err:?}"
    );

    let err = filecrypt::resolve(&cx, &format!("{base}/DLC/gate.dlc"))
        .await
        .unwrap_err();
    assert!(
        matches!(&err, ResolveError::NeedsUser(m) if m.contains("web page")),
        "{err:?}"
    );

    let err = filecrypt::resolve(&cx, &format!("{base}/Container/abc.html"))
        .await
        .unwrap_err();
    assert!(
        matches!(&err, ResolveError::NeedsUser(m) if m.contains(".dlc")),
        "{err:?}"
    );
}
