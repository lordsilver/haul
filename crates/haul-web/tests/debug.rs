//! Debug mode's API in front of a real engine. Debug mode is on for the
//! whole process once turned on, so it has a test binary of its own.

use std::time::Duration;

use haul_core::{Config, Engine, Options};
use reqwest::StatusCode;
use serde_json::{Value, json};

#[tokio::test]
async fn debug_mode_streams_signals_and_shows_the_insides() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    haul_core::logging::init(&data, false, true);
    let token = std::fs::read_to_string(haul_core::debug::token_path(&data)).unwrap();
    let engine = Engine::start(Options {
        data_dir: data.clone(),
        download_dir: Some(tmp.path().join("dl")),
        config: Some(Config::default()),
        recycle_bin: Some(tmp.path().join("bin")),
    })
    .await
    .unwrap();
    // Nothing should leave the machine during this test.
    engine.set_paused(true);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    tokio::spawn(haul_web::serve(listener, engine.clone(), None));
    let http = reqwest::Client::new();
    let get = |path: &str| {
        http.get(format!("{base}{path}"))
            .header("x-haul-debug", &token)
    };

    // Without the token, nothing.
    let refused = http
        .get(format!("{base}/api/debug/inspect"))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), StatusCode::FORBIDDEN);

    // The stream says hello, then carries what happens: a test's own note,
    // a row's status, the engine's log.
    let mut stream = get("/api/debug/stream").send().await.unwrap();
    assert_eq!(stream.status(), StatusCode::OK);
    let mut lines: Vec<Value> = Vec::new();
    let mut pending = String::new();
    let mut read_until = async |want: &dyn Fn(&Value) -> bool| {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Some(found) = lines.iter().find(|v| want(v)) {
                    return found.clone();
                }
                let chunk = stream.chunk().await.unwrap().expect("the stream went on");
                pending.push_str(&String::from_utf8_lossy(&chunk));
                while let Some(end) = pending.find('\n') {
                    let line: String = pending.drain(..=end).collect();
                    lines.push(serde_json::from_str(line.trim()).unwrap());
                }
            }
        })
        .await
        .expect("the signal came")
    };
    let hello = read_until(&|v| v["kind"] == "hello").await;
    assert_eq!(hello["desktop"], false);

    let note = http
        .post(format!("{base}/api/debug/emit"))
        .header("x-haul-debug", &token)
        .json(&json!({"kind": "note", "step": "adding"}))
        .send()
        .await
        .unwrap();
    assert_eq!(note.status(), StatusCode::NO_CONTENT);
    read_until(&|v| v["kind"] == "note" && v["step"] == "adding").await;

    let link = "https://files.example/f/some.part1.rar";
    http.post(format!("{base}/api/add"))
        .json(&json!({"text": link}))
        .send()
        .await
        .unwrap();
    let status = read_until(&|v| v["kind"] == "status" && v["name"] == "some.part1.rar").await;
    assert!(status["from"].is_null(), "{status}");
    assert!(status["t"].as_str().is_some_and(|t| t.ends_with('Z')));
    read_until(&|v| v["kind"] == "log" && v["level"].is_string()).await;

    // A wait answers with the first signal that matches, and one that
    // never comes ends after its timeout.
    let waited = http
        .post(format!("{base}/api/debug/wait"))
        .header("x-haul-debug", &token)
        .json(&json!({"match": {"kind": "status", "name": "part1"}, "recent": true, "timeout": 5}))
        .send()
        .await
        .unwrap();
    assert_eq!(waited.status(), StatusCode::OK);
    assert_eq!(waited.json::<Value>().await.unwrap()["kind"], "status");
    let never = http
        .post(format!("{base}/api/debug/wait"))
        .header("x-haul-debug", &token)
        .json(&json!({"match": {"kind": "nothing"}, "timeout": 1}))
        .send()
        .await
        .unwrap();
    assert_eq!(never.status(), StatusCode::REQUEST_TIMEOUT);

    // The insides: the row with its links, the engine's flags.
    let inside: Value = get("/api/debug/inspect")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let row = &inside["engine"]["items"][0];
    assert_eq!(row["mirrors"], json!([link]));
    assert_eq!(inside["engine"]["paused"], true);
    assert!(inside["state"]["items"].is_array());
    assert!(inside["desktop"].is_null());

    // A page is looked into only where the desktop app passes it.
    let page = get(&format!("/api/debug/pass/{}", row["id"].as_str().unwrap()))
        .send()
        .await
        .unwrap();
    assert_eq!(page.status(), StatusCode::NOT_IMPLEMENTED);

    let recent: Vec<Value> = get("/api/debug/recent")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(recent.iter().any(|v| v["kind"] == "status"));
}
