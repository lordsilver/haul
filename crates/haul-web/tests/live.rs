//! The Live connection setting switches debug mode on and off while Haul
//! runs. Debug mode is one per process, so this has a test binary of its own.

use haul_core::{Config, Engine, Options};
use reqwest::StatusCode;
use serde_json::json;

#[tokio::test]
async fn the_live_connection_comes_and_goes_with_its_setting() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    haul_core::logging::init(&data, false, false);
    let token_file = haul_core::debug::token_path(&data);
    let engine = Engine::start(Options {
        data_dir: data.clone(),
        download_dir: Some(tmp.path().join("dl")),
        config: Some(Config::default()),
        recycle_bin: Some(tmp.path().join("bin")),
    })
    .await
    .unwrap();
    engine.set_paused(true);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    tokio::spawn(haul_web::serve(listener, engine.clone(), None));
    let http = reqwest::Client::new();
    let recent = |token: &str| {
        http.get(format!("{base}/api/debug/recent"))
            .header("x-haul-debug", token)
            .send()
    };
    let live = |on: bool| {
        http.post(format!("{base}/api/settings"))
            .json(&json!({ "live_debug": on }))
            .send()
    };

    // Off: no token, and the routes answer as if they were not there.
    assert!(!token_file.exists());
    assert_eq!(recent("x").await.unwrap().status(), StatusCode::NOT_FOUND);

    assert!(live(true).await.unwrap().status().is_success());
    let token = std::fs::read_to_string(&token_file).unwrap();
    assert_eq!(
        recent("wrong").await.unwrap().status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(recent(&token).await.unwrap().status(), StatusCode::OK);

    assert!(live(false).await.unwrap().status().is_success());
    assert!(!token_file.exists(), "the token goes with it");
    assert_eq!(
        recent(&token).await.unwrap().status(),
        StatusCode::NOT_FOUND
    );

    // On again: a new token, the old one refused.
    assert!(live(true).await.unwrap().status().is_success());
    let again = std::fs::read_to_string(&token_file).unwrap();
    assert_ne!(again, token);
    assert_eq!(
        recent(&token).await.unwrap().status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(recent(&again).await.unwrap().status(), StatusCode::OK);
}
