//! The HTTP API in front of a real engine, as a browser and as scripts see it.

use std::time::Duration;

use haul_core::{Config, Engine, Options};
use reqwest::{
    StatusCode,
    header::{CONTENT_SECURITY_POLICY, CONTENT_TYPE, HOST, ORIGIN},
};
use serde_json::{Value, json};

struct Server {
    _tmp: tempfile::TempDir,
    base: String,
    port: u16,
    http: reqwest::Client,
}

async fn start() -> Server {
    let tmp = tempfile::tempdir().unwrap();
    let engine = Engine::start(Options {
        data_dir: tmp.path().join("data"),
        download_dir: Some(tmp.path().join("dl")),
        config: Some(Config::default()),
        recycle_bin: Some(tmp.path().join("bin")),
    })
    .await
    .unwrap();
    // Nothing should leave the machine during these tests.
    engine.set_paused(true);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(haul_web::serve(listener, engine, None));
    Server {
        _tmp: tmp,
        base: format!("http://127.0.0.1:{port}"),
        port,
        http: reqwest::Client::new(),
    }
}

impl Server {
    async fn post(&self, path: &str, body: Value) -> reqwest::Response {
        self.http
            .post(format!("{}{path}", self.base))
            .json(&body)
            .send()
            .await
            .unwrap()
    }

    async fn state(&self) -> Value {
        self.http
            .get(format!("{}/api/state", self.base))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap()
    }
}

#[tokio::test]
async fn serves_the_ui_with_a_strict_policy() {
    let s = start().await;
    let page = s.http.get(format!("{}/", s.base)).send().await.unwrap();
    assert_eq!(page.status(), StatusCode::OK);
    let csp = page
        .headers()
        .get(CONTENT_SECURITY_POLICY)
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert!(csp.contains("frame-ancestors 'none'"), "{csp}");
    assert!(page.text().await.unwrap().contains("<title>Haul</title>"));
    for asset in ["/app.js", "/app.css", "/icons.js", "/theme.js", "/icon.svg"] {
        let r = s
            .http
            .get(format!("{}{asset}", s.base))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::OK, "{asset}");
    }
}

#[tokio::test]
async fn refuses_foreign_hosts_and_origins() {
    let s = start().await;
    // DNS rebinding: the right address under someone else's name.
    let r = s
        .http
        .get(format!("{}/api/state", s.base))
        .header(HOST, "evil.example")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::FORBIDDEN);

    let add = |origin: Option<&'static str>| {
        let mut req = s
            .http
            .post(format!("{}/api/add", s.base))
            .json(&json!({"text": "https://a.example/x.zip"}));
        if let Some(o) = origin {
            req = req.header(ORIGIN, o);
        }
        req.send()
    };
    assert_eq!(
        add(Some("https://evil.example")).await.unwrap().status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        add(Some("null")).await.unwrap().status(),
        StatusCode::FORBIDDEN
    );
    let own = format!("http://localhost:{}", s.port).leak();
    assert_eq!(add(Some(own)).await.unwrap().status(), StatusCode::OK);
    assert_eq!(
        add(None).await.unwrap().status(),
        StatusCode::OK,
        "scripts send no Origin"
    );
    assert_eq!(s.state().await["items"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn adds_imports_and_controls_items() {
    let s = start().await;
    let r: Value = s
        .post("/api/add", json!({"text": "x https://a.example/f/one.rar y https://b.example/g/one.rar ftp://z", "package": "Pkg"}))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(
        r,
        json!({"added": 1, "merged": 1, "duplicates": 0, "invalid": 0})
    );

    let links_file = || {
        reqwest::multipart::Form::new().part(
            "file",
            reqwest::multipart::Part::bytes(
                b"https://c.example/two.zip\nhttps://a.example/f/one.rar".to_vec(),
            )
            .file_name("links.txt"),
        )
    };
    // Read for the Add dialog: the links come back and nothing is added.
    let before = s.state().await["items"].as_array().unwrap().len();
    let r: Value = s
        .http
        .post(format!("{}/api/container/read", s.base))
        .multipart(links_file())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        r,
        json!({"links": ["https://c.example/two.zip", "https://a.example/f/one.rar"], "package": ""})
    );
    assert_eq!(s.state().await["items"].as_array().unwrap().len(), before);

    let form = links_file();
    let r: Value = s
        .http
        .post(format!("{}/api/container", s.base))
        .multipart(form)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        (r["added"].as_u64(), r["duplicates"].as_u64()),
        (Some(1), Some(1))
    );

    let state = s.state().await;
    assert_eq!(
        (state["desktop"].as_bool(), state["paused"].as_bool()),
        (Some(false), Some(true))
    );
    let items = state["items"].as_array().unwrap();
    assert_eq!(items[0]["mirrors"], json!(["a.example", "b.example"]));
    assert_eq!(items[0]["package"], "Pkg");
    assert_eq!(items[0]["verifying"], "", "not comparing anything");
    let id = items[0]["id"].as_str().unwrap();

    assert_eq!(
        s.post(&format!("/api/item/{id}/browse"), json!({}))
            .await
            .status(),
        StatusCode::NOT_IMPLEMENTED
    );
    assert_eq!(
        s.post(&format!("/api/item/{id}/explode"), json!({}))
            .await
            .status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        s.post("/api/item/nope/pause", json!({})).await.status(),
        StatusCode::NOT_FOUND
    );
    let bad = s
        .post(
            &format!("/api/item/{id}/direct"),
            json!({"url": "file:///etc/passwd"}),
        )
        .await;
    assert_eq!(bad.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        s.post(&format!("/api/item/{id}/pause"), json!({}))
            .await
            .status(),
        StatusCode::OK
    );
    assert_eq!(s.state().await["items"][0]["status"], "paused");

    s.post(
        "/api/settings",
        json!({"max_total": 3, "rate_limit_mbps": 2.5}),
    )
    .await;
    s.post("/api/host/a.example/limit/0", json!({})).await;
    let state = s.state().await;
    assert_eq!(
        (state["max_total"].as_u64(), state["rate"].as_f64()),
        (Some(3), Some(2.5e6))
    );

    assert_eq!(
        s.post("/api/host/a.example/route/sideways", json!({}))
            .await
            .status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        s.post(&format!("/api/item/{id}/remove"), json!({}))
            .await
            .status(),
        StatusCode::OK
    );
    assert_eq!(s.state().await["items"].as_array().unwrap().len(), 1);
}

/// Reads the event stream until `want` shows up in it.
async fn read_until(events: &mut reqwest::Response, seen: &mut String, want: &str) {
    let found = tokio::time::timeout(Duration::from_secs(5), async {
        while !seen.contains(want) {
            let chunk = events.chunk().await.unwrap().unwrap();
            seen.push_str(&String::from_utf8_lossy(&chunk));
        }
    })
    .await;
    assert!(found.is_ok(), "no {want:?} in {seen}");
}

#[tokio::test]
async fn streams_state_events() {
    let s = start().await;
    let mut events = s
        .http
        .get(format!("{}/api/events", s.base))
        .send()
        .await
        .unwrap();
    assert_eq!(events.headers()["content-type"], "text/event-stream");
    let mut seen = String::new();
    read_until(&mut events, &mut seen, "event: state").await;
    s.post("/api/add", json!({"text": "https://a.example/pushed.zip"}))
        .await;
    read_until(&mut events, &mut seen, "pushed.zip").await;
}

#[tokio::test]
async fn settings_round_trip_and_secrets_stay_out_of_the_stream() {
    let s = start().await;
    assert_eq!(s.state().await["theme"], "system");
    let ok = s
        .post(
            "/api/settings",
            json!({"theme": "dark", "1fichier_key": "s3cret", "watch_clipboard": true, "recycle_on_remove": true}),
        )
        .await;
    assert_eq!(ok.status(), StatusCode::OK);
    let state = s.state().await;
    assert_eq!(
        (
            state["theme"].as_str(),
            state["watch_clipboard"].as_bool(),
            state["recycle_on_remove"].as_bool()
        ),
        (Some("dark"), Some(true), Some(true))
    );
    assert!(
        !state.to_string().contains("s3cret"),
        "secrets never ride the state stream"
    );

    let settings: Value = s
        .http
        .get(format!("{}/api/settings", s.base))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(settings["accounts"]["1fichier_key"], "s3cret");
    assert_eq!(settings["theme"], "dark");

    for bad in [
        json!({"theme": "purple"}),
        json!({"max_total": "many"}),
        json!({"proxy": "ftp://nope"}),
        json!({"download_dir": "relative/path"}),
        json!({"user_agent": "two\nlines"}),
        json!({"no_such_setting": 1}),
    ] {
        let r = s.post("/api/settings", bad.clone()).await;
        assert_eq!(r.status(), StatusCode::BAD_REQUEST, "{bad}");
    }
    let folder = s
        .post("/api/host/example.com/folder", json!({"folder": "Example"}))
        .await;
    assert_eq!(folder.status(), StatusCode::OK);
    let preferred = s
        .post(
            "/api/host/example.com/preferred",
            json!({"preferred": true}),
        )
        .await;
    assert_eq!(preferred.status(), StatusCode::OK);
    let missing = s.post("/api/host/example.com/folder", json!({})).await;
    assert!(missing.status().is_client_error(), "{}", missing.status());
    let gone = s.post("/api/item/no-such-id/remove", json!({})).await;
    assert_eq!(gone.status(), StatusCode::NOT_FOUND);

    let picker = s.post("/api/settings/download-dir/pick", json!({})).await;
    assert_eq!(picker.status(), StatusCode::NOT_IMPLEMENTED);
}

#[tokio::test]
async fn reorders_the_preferred_hosts() {
    let s = start().await;
    // The preferred hosts in their order, as the state lists them.
    let preferred = |state: Value| -> Vec<String> {
        state["hosts"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|h| !h["preferred"].is_null())
            .map(|h| h["name"].as_str().unwrap().to_string())
            .collect()
    };
    let star = s
        .post(
            "/api/host/rapidgator.net/preferred",
            json!({"preferred": true}),
        )
        .await;
    assert_eq!(star.status(), StatusCode::OK);
    assert_eq!(
        preferred(s.state().await),
        ["pixeldrain.com", "rapidgator.net"]
    );

    let r = s
        .post(
            "/api/preferred-hosts",
            json!({"hosts": ["rapidgator.net", "pixeldrain.com"]}),
        )
        .await;
    assert_eq!(r.status(), StatusCode::OK);
    assert_eq!(
        preferred(s.state().await),
        ["rapidgator.net", "pixeldrain.com"]
    );

    for bad in [
        json!({"hosts": ["a.example", "A.example"]}),
        json!({"hosts": ["a.example", " "]}),
    ] {
        let r = s.post("/api/preferred-hosts", bad.clone()).await;
        assert_eq!(r.status(), StatusCode::BAD_REQUEST, "{bad}");
        let body: Value = r.json().await.unwrap();
        assert!(body["error"].as_str().is_some_and(|e| !e.is_empty()));
    }
    for bad in [
        json!({}),
        json!({"hosts": [1]}),
        json!({"hosts": "a.example"}),
    ] {
        let r = s.post("/api/preferred-hosts", bad.clone()).await;
        assert!(r.status().is_client_error(), "{bad}: {}", r.status());
    }
    assert_eq!(
        preferred(s.state().await),
        ["rapidgator.net", "pixeldrain.com"],
        "refused lists change nothing"
    );
}

#[tokio::test]
async fn every_error_is_json() {
    let s = start().await;
    let url = |path: &str| format!("{}{path}", s.base);
    let answers = [
        s.http
            .post(url("/api/add"))
            .header(CONTENT_TYPE, "application/json")
            .body("{")
            .send()
            .await
            .unwrap(),
        s.post("/api/add", json!({"txt": "no text field"})).await,
        s.post("/api/host/a.example/limit/many", json!({})).await,
        s.http.get(url("/api/no-such-route")).send().await.unwrap(),
        s.http.get(url("/api/add")).send().await.unwrap(),
        s.http
            .get(url("/api/state"))
            .header(HOST, "evil.example")
            .send()
            .await
            .unwrap(),
    ];
    for r in answers {
        let status = r.status();
        assert!(status.is_client_error(), "{status}");
        let body: Value = r.json().await.unwrap();
        assert!(
            body["error"].as_str().is_some_and(|e| !e.is_empty()),
            "{status}: {body}"
        );
    }
}

#[tokio::test]
async fn places_and_reveal_are_limited_to_the_app() {
    let s = start().await;
    // Only the app's own places open, and nothing a caller names.
    assert_eq!(
        s.post("/api/open/etc", json!({})).await.status(),
        StatusCode::BAD_REQUEST
    );
    s.post("/api/add", json!({"text": "https://a.example/f.zip"}))
        .await;
    let id = s.state().await["items"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let reveal = s.post(&format!("/api/item/{id}/reveal"), json!({})).await;
    assert_eq!(reveal.status(), StatusCode::NOT_FOUND, "nothing saved yet");
}
