//! The engine end to end against a local file server reachable under two
//! host names (`127.0.0.1` and `localhost`), so mirror logic sees two hosts.

use std::{
    collections::BTreeMap,
    sync::atomic::Ordering,
    time::{Duration, Instant},
};

use axum::{Json, Router, routing::get};
use haul_core::{
    Config, Engine, Event, Extract, Found, ItemAction, Options, Route, Snapshot, StartError,
    Verifying, config::Endpoints,
};
use serde_json::json;

mod support;

use support::*;

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
        r#"{"empty":{"mirrors":[],"name":null,"package":"","status":"queued","error":"","pre":null,"size":0},"kept":{"mirrors":["http://127.0.0.1:9/k.bin"],"name":null,"package":"","status":"paused","error":"","pre":null,"size":0}}"#,
    )
    .unwrap();
    let engine = t.engine().await;
    let ids: Vec<String> = engine.snapshot().items.into_iter().map(|i| i.id).collect();
    assert_eq!(ids, ["kept"]);
}

/// A set of files uploaded to two hosts under other names (each host's parts
/// named after an id of its own) downloads once: the second copy's parts,
/// as many and each the same size to the byte, become more links of the
/// first copy's. A set that differs stays as it is.
#[tokio::test]
async fn a_copy_of_an_archive_on_another_host_becomes_its_backup() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.set_paused(true);
    let links: Vec<String> = [
        "A.part1.rar",
        "A.part2.rar",
        "B.part1.rar",
        "B.part2.rar",
        "C.part1.rar",
        "C.part2.rar",
        "C.part3.rar",
    ]
    .iter()
    .map(|n| format!("{}/id/{}", t.ip, n.replace('.', "_")))
    .collect();
    engine.add_text(&links.join("\n"), "Example");
    let s = wait_for(
        &engine,
        "the copy folded into the first, every row named",
        |s| s.items.len() == 5 && s.items.iter().all(|i| i.name.ends_with(".rar")),
    )
    .await;
    let mut rows: Vec<(String, usize)> = s
        .items
        .iter()
        .map(|i| (i.name.clone(), engine.debug_links(&i.id).len()))
        .collect();
    rows.sort();
    assert_eq!(
        rows,
        [
            ("A.part1.rar".to_string(), 2),
            ("A.part2.rar".to_string(), 2),
            ("C.part1.rar".to_string(), 1),
            ("C.part2.rar".to_string(), 1),
            ("C.part3.rar".to_string(), 1),
        ],
        "B's parts stand by A's; C, a set of its own, stays"
    );
    let a1 = s.items.iter().find(|i| i.name == "A.part1.rar").unwrap();
    assert_eq!(
        engine.debug_links(&a1.id),
        [links[0].clone(), links[2].clone()]
    );
}

/// A link that names its file after `#` is named so from the start, and
/// joins the same file's link on another host.
#[tokio::test]
async fn a_link_named_after_its_fragment_joins_the_same_file() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    // Not downloaded: the two paths serve different bytes.
    engine.set_paused(true);
    let added = engine.add_text(
        &format!("{}/files/abc#kept.bin {}/files/kept.bin", t.ip, t.name),
        "",
    );
    assert_eq!((added.added, added.merged), (1, 1));
    let s = engine.snapshot();
    assert_eq!(s.items.len(), 1);
    assert_eq!(s.items[0].name, "kept.bin");
    assert_eq!(engine.debug_links(&s.items[0].id).len(), 2);
}

/// A newer Haul on GitHub shows in the state, and its setup program
/// downloads checked against the digest GitHub gives.
#[tokio::test]
async fn a_newer_haul_is_found_and_its_installer_checked() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    assert!(engine.snapshot().update.is_none());
    engine.check_update().await;
    let update = engine.snapshot().update.expect("found");
    assert_eq!(update.version, "99.0.0");
    assert!(update.notes.contains("Everything"));
    let installer = engine.download_update().await.unwrap();
    assert_eq!(
        installer.file_name().unwrap(),
        "haul-desktop-v99.0.0-windows-x64-setup.exe"
    );
    assert_eq!(
        std::fs::read(&installer).unwrap(),
        content("setup.exe", SIZE)
    );
}

/// The timer keeps to the setting; a person asking does not have to.
#[tokio::test]
async fn a_person_can_look_for_updates_with_the_setting_off() {
    let t = Setup::new().await;
    let engine = t
        .engine_with(Config {
            check_updates: false,
            ..t.config()
        })
        .await;
    engine.check_update().await;
    assert!(engine.snapshot().update.is_none(), "the timer keeps off");
    assert_eq!(
        engine.look_for_update().await,
        Ok(Some("99.0.0".to_string())),
    );
    assert!(engine.snapshot().update.is_some());
}

/// A file on several hosts downloads from the one the user picked, even
/// over the hosts they prefer in general.
#[tokio::test]
async fn a_file_downloads_from_the_host_picked_for_it() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.set_paused(true);
    // pixeldrain.com, preferred from the start, and a direct link: one file.
    engine.add_text(
        &format!("https://pixeldrain.com/u/pick\n{}/files/pick.bin", t.ip),
        "",
    );
    let s = wait_for(&engine, "one row on two hosts", |s| {
        s.items.len() == 1 && s.items[0].mirrors.len() == 2
    })
    .await;
    let id = s.items[0].id.clone();
    assert_eq!(
        engine.download_from(std::slice::from_ref(&id), Some("127.0.0.1")),
        1
    );
    assert_eq!(
        engine.snapshot().items[0].download_from.as_deref(),
        Some("127.0.0.1")
    );
    engine.set_paused(false);
    engine.item_action(&id, ItemAction::Resume);
    wait_for(&engine, "done", all_done).await;
    assert!(
        !t.server.requests("/files/pick.bin").is_empty(),
        "the picked host served it"
    );
    // pixeldrain was only asked what the file is, never for it.
    assert!(
        t.server
            .requests("/api/file/pick")
            .iter()
            .all(|r| r.contains("/info")),
        "pixeldrain stood by"
    );
}

/// A link that names its file is still looked up when its host says what a
/// file is through an API: its size counts before it downloads.
#[tokio::test]
async fn a_named_link_learns_its_size_from_its_hosts_api() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.set_paused(true);
    engine.add_text(
        "https://www.mediafire.com/file/abc12345/archive.part1.rar/file",
        "",
    );
    wait_for(&engine, "its size", |s| {
        s.items.len() == 1 && s.items[0].size == SIZE as u64
    })
    .await;
    assert!(
        t.server.requests("/api/1.5/file/get_info.php").len() == 1,
        "asked once, while downloads are paused"
    );
}

/// A link whose host says the file was deleted fails as soon as it is
/// looked up, with the host's words, rather than on its turn.
#[tokio::test]
async fn a_deleted_file_fails_when_it_is_looked_up() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.set_paused(true);
    engine.add_text("https://1fichier.com/?gone123", "");
    let s = wait_for(&engine, "the deleted file to fail", |s| {
        s.items.first().is_some_and(|i| i.status == "error")
    })
    .await;
    assert!(s.items[0].error.contains("deleted"), "{}", s.items[0].error);
    assert_eq!(s.items[0].retry_in, 0, "not tried again on a timer");
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

/// A preferred host shows its wait before anything came from it, as any other
/// host does.
#[tokio::test]
async fn a_preferred_host_shows_its_wait_before_any_download_finished() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine
        .set_preferred_hosts(&["127.0.0.1".to_string()])
        .unwrap();
    engine.add_text(&format!("{}/limited-long/d.bin", t.ip), "");
    let s = wait_for(&engine, "cooldown", |s| {
        s.items.first().is_some_and(|i| i.status == "waiting")
    })
    .await;
    let host = s.hosts.iter().find(|h| h.name == "127.0.0.1").unwrap();
    assert!(host.cooldown > 0, "shows its wait: {}", host.cooldown);
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
    engine
        .update_settings(json!({"max_total": 3}).as_object().unwrap())
        .unwrap();
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
    let engine = t.engine_with(config).await;
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
    engine
        .update_settings(json!({"rate_limit_mbps": 1.0}).as_object().unwrap())
        .unwrap();
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
    engine
        .update_settings(json!({"max_total": 1}).as_object().unwrap())
        .unwrap();
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
    // pixeldrain was only asked what the file is.
    assert!(
        t.server
            .requests("/api/file/kept")
            .iter()
            .all(|r| r.contains("/info")),
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
/// knows, is a wrapper of them: named after the page, its files join the
/// queue not started, as a wrapper's do. One linking none waits for a
/// person.
#[tokio::test]
async fn a_page_of_links_is_a_wrapper_of_them() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.add_text(&format!("{}/blog/links", t.ip), "Pkg");
    let s = wait_for(&engine, "the page's files, named", |s| {
        s.items.len() == 2 && s.items.iter().all(|i| i.name.ends_with(".bin"))
    })
    .await;
    assert_eq!(s.wrappers.len(), 1, "the page is a wrapper");
    assert_eq!(s.wrappers[0].title.as_deref(), Some("Example R"));
    assert_eq!(s.wrappers[0].status, "done");
    let mut rows: Vec<(&str, &str, &str)> = s
        .items
        .iter()
        .map(|i| (i.name.as_str(), i.package.as_str(), i.status))
        .collect();
    rows.sort();
    assert_eq!(
        rows,
        [
            ("postone.bin", "Pkg", "paused"),
            ("posttwo.bin", "Pkg", "paused")
        ],
        "not started: the user picks, as for any wrapper's files"
    );
    for i in &s.items {
        engine.item_action(&i.id, ItemAction::Resume);
    }
    wait_for(&engine, "both downloaded", all_done).await;

    engine.add_text(&format!("{}/blog/empty", t.ip), "");
    wait_for(&engine, "the page without links", |s| {
        s.items.iter().any(|i| i.status == "captcha")
    })
    .await;
}

/// A page is read before it is known to be a file or a wrapper, so it waits
/// in no list until then, or it would flash through the downloads on its way
/// to the wrappers; the status bar counts it. A plain file stays in its list.
#[tokio::test]
async fn a_page_being_read_is_in_no_list_until_it_shows_what_it_is() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.add_text(&format!("{}/blog/links", t.ip), "");
    let s = engine.snapshot();
    assert_eq!(s.items.len(), 1, "added, and not read yet");
    assert_eq!(s.items[0].status, "checking");
    assert_eq!(s.items[0].list, "", "in no list while it is read");
    assert_eq!(s.counts.all, 0, "not counted among the downloads");
    assert_eq!(
        s.counts.unwrapping, 1,
        "the status bar says it is worked on"
    );

    let s = wait_for(&engine, "the page's files", |s| s.items.len() == 2).await;
    assert_eq!(s.wrappers.len(), 1);
    assert_eq!(s.counts.all, 2);

    engine.add_text(&format!("{}/files/report.bin", t.ip), "");
    let s = engine.snapshot();
    let file = s.items.iter().find(|i| i.name == "report.bin").unwrap();
    assert_ne!(file.list, "", "a file's link is a download from the start");
}

/// `n/a` in a container's table is no file's name: its links are not one file
/// on several hosts, nor is any row called that.
#[tokio::test]
async fn a_containers_rows_without_names_stay_separate() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.add_text("https://filecrypt.cc/Container/unnamed.html", "");
    let s = wait_for(&engine, "the container's links", |s| {
        s.wrappers.first().is_some_and(|w| w.status == "done") && !s.items.is_empty()
    })
    .await;
    assert_eq!(s.items.len(), 2, "two links, two rows");
    let wrapper = s.hosts.iter().find(|h| h.name == "filecrypt.cc").unwrap();
    assert_eq!(wrapper.ok, 1, "its page served counts for the wrapper");
    assert!(s.items.iter().all(|i| i.name != "n/a"));
    assert!(s.items.iter().all(|i| i.mirrors.len() == 1));
}

/// A wrapper's file only on a host that is off until its account is set is in
/// no list and not started with the rest; the wrapper's card counts it, and it
/// comes back when the host is switched on.
#[tokio::test]
async fn a_wrappers_file_only_on_a_host_without_its_account_stays_out_of_the_lists() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.add_text("https://filecrypt.cc/Container/for-account.html", "");
    let s = wait_for(&engine, "the container's links", |s| {
        s.items.len() == 3 && s.items.iter().all(|i| i.status == "paused")
    })
    .await;
    let hidden = |s: &Snapshot| s.items.iter().filter(|i| i.list.is_empty()).count();
    assert_eq!(hidden(&s), 1, "the file only on gofile.io");
    assert_eq!(s.counts.all, 2);
    assert_eq!(s.counts.ready, 2, "the status bar counts the other two");
    assert_eq!(s.wrapper_cards[0].files, 2);
    assert_eq!(s.wrapper_cards[0].waiting_for_account, 1);

    let id = s.wrappers[0].id.clone();
    assert_eq!(engine.start_wrapper(&id), Some(2), "only the two it can");
    let s = wait_for(&engine, "the two to finish", |s| {
        s.items.iter().filter(|i| i.status == "done").count() == 2
    })
    .await;
    assert_eq!(hidden(&s), 1, "and the third waits on, with no error");

    engine.set_disabled("gofile.io", false).unwrap();
    let s = engine.snapshot();
    assert_eq!(hidden(&s), 0, "switched on, it shows");
    assert_eq!(s.wrapper_cards[0].waiting_for_account, 0);
}

/// A page's links to files on a host Haul has no code for, on another
/// site, are its files: not started, and downloaded from that host once
/// started. Its own site's file stays out.
#[tokio::test]
async fn a_page_linking_files_elsewhere_is_a_wrapper_of_them() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.add_text(&format!("{}/blog/elsewhere", t.ip), "");
    let s = wait_for(&engine, "the page's files", |s| {
        s.wrappers.iter().any(|w| w.status == "done") && s.items.len() == 2
    })
    .await;
    assert_eq!(s.wrappers.len(), 1, "the page is a wrapper");
    let mut rows: Vec<(&str, &str)> = s
        .items
        .iter()
        .map(|i| (i.name.as_str(), i.status))
        .collect();
    rows.sort();
    assert_eq!(
        rows,
        [("r.part1.rar", "paused"), ("r.part2.rar", "paused")],
        "not started, and not the site's own file"
    );
    for i in &s.items {
        engine.item_action(&i.id, ItemAction::Resume);
    }
    wait_for(&engine, "both downloaded", all_done).await;
    assert!(t.server.requests("/files/site.rar").is_empty());
    assert!(!t.server.requests("/files/r.part1.rar").is_empty());
}

/// A page's card shows no host until a wrapper it links is unwrapped, then
/// the hosts of the files that wrapper found: the video the page plays
/// from another site is no file of its.
#[tokio::test]
async fn a_pages_card_shows_hosts_once_its_wrappers_unwrap() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.add_text(&format!("{}/blog/trailer", t.ip), "");
    let s = wait_for(&engine, "the page read, its container waiting", |s| {
        s.wrappers.len() == 2
            && s.wrappers.iter().any(|w| w.status == "done")
            && s.wrappers.iter().any(|w| w.status == "captcha")
    })
    .await;
    assert!(s.items.is_empty(), "{:?}", s.items);
    assert_eq!(
        s.wrapper_cards.len(),
        1,
        "the container is on the page's card"
    );
    let card = &s.wrapper_cards[0];
    assert_eq!(
        (card.files, card.hosts.as_slice()),
        (0, &[][..]),
        "{card:?}"
    );
    // The container unwrapped (its link handed back, as by the in-app
    // browser): its files' hosts show on the card.
    let container = s.wrappers.iter().find(|w| w.status == "captcha").unwrap();
    engine
        .set_direct(
            &container.id,
            "https://filecrypt.cc/DLC/abc123.dlc",
            BTreeMap::new(),
            None,
        )
        .unwrap();
    let s = wait_for(&engine, "the container's files", |s| {
        s.items.len() == 2 && s.wrappers.iter().all(|w| w.status == "done")
    })
    .await;
    let mut hosts: Vec<String> = s.items.iter().flat_map(|i| i.mirrors.clone()).collect();
    hosts.sort();
    hosts.dedup();
    assert!(!hosts.is_empty());
    assert_eq!(s.wrapper_cards.len(), 1);
    let mut shown = s.wrapper_cards[0].hosts.clone();
    shown.sort();
    assert_eq!((s.wrapper_cards[0].files, shown), (2, hosts));
}

/// A page whose address reads like a file's name is read for its links
/// all the same.
#[tokio::test]
async fn a_page_named_like_a_file_is_read_for_links() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.add_text(&format!("{}/blog/Example.Set.v1.25.html", t.ip), "");
    two_files_from_a_wrapper(&engine).await;
}

/// A page's title (`Example.Notes.2026.Final`) names no file:
/// its page is read for links, not taken for a file named `Some.Mov`.
#[tokio::test]
async fn a_release_title_is_no_file_name() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.add_text(&format!("{}/blog/notes", t.ip), "");
    two_files_from_a_wrapper(&engine).await;
}

async fn two_files_from_a_wrapper(engine: &Engine) {
    let s = wait_for(engine, "the page's files, named", |s| {
        s.items.len() == 2 && s.items.iter().all(|i| i.name.ends_with(".bin"))
    })
    .await;
    assert_eq!(s.wrappers.len(), 1);
    assert_eq!(s.wrappers[0].status, "done");
    assert!(
        s.items.iter().all(|i| i.status == "paused"),
        "{:?}",
        s.items
    );
}

/// A copied page is fetched once to be judged and read.
#[tokio::test]
async fn a_copied_page_costs_one_request() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.add_from_clipboard(&format!("{}/blog/links", t.ip));
    wait_for(&engine, "the page unwrapped", |s| {
        s.wrappers.iter().any(|w| w.status == "done") && s.items.len() == 2
    })
    .await;
    let asked: Vec<String> = t
        .server
        .requests("/blog/links")
        .into_iter()
        .filter(|r| r.split(' ').next() == Some("/blog/links"))
        .collect();
    assert_eq!(asked.len(), 1, "{asked:?}");
}

/// A second page listing the same links shares their files: its card has
/// them too, and they stay when the first page is removed.
#[tokio::test]
async fn a_second_page_with_the_same_links_shares_their_files() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.add_text(&format!("{}/blog/links", t.ip), "");
    wait_for(&engine, "the first page's files, named", |s| {
        s.items.len() == 2 && s.items.iter().all(|i| i.name.ends_with(".bin"))
    })
    .await;
    engine.add_text(&format!("{}/blog/links-copy", t.ip), "");
    let s = wait_for(&engine, "both pages unwrapped", |s| {
        s.wrappers.len() == 2 && s.wrappers.iter().all(|w| w.status == "done")
    })
    .await;
    let id_of = |post: &str| {
        let url = format!("{}/blog/{post}", t.ip);
        s.wrappers.iter().find(|w| w.url == url).unwrap().id.clone()
    };
    let (first, second) = (id_of("links"), id_of("links-copy"));
    assert_eq!(s.items.len(), 2, "the same two files");
    assert_eq!(s.wrapper_cards.len(), 2);
    for card in &s.wrapper_cards {
        assert_eq!(card.files, 2, "{}", card.name);
    }
    for i in &s.items {
        assert!(i.also_from.contains(&second), "{:?}", i.also_from);
    }
    assert!(engine.remove_wrapper(&first));
    let s = engine.snapshot();
    assert_eq!(s.items.len(), 2, "the files stay, as the second page's");
    for i in &s.items {
        assert_eq!(i.source.as_deref(), Some(second.as_str()));
    }
}

/// A page behind a check Haul cannot pass waits for a person; the links its
/// window then shows (the person past the check, or on a paste the page
/// led to) stand in for it, the site's own and ads left out.
#[tokio::test]
async fn a_guarded_page_takes_the_links_its_window_shows() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    let page = format!("{}/blog/guarded", t.ip);
    engine.add_text(&page, "Pkg");
    let s = wait_for(&engine, "the guarded page to need a person", |s| {
        s.items.iter().any(|i| i.status == "captcha")
    })
    .await;
    let id = s.items[0].id.clone();
    assert!(
        engine.page_passes(&page),
        "a page behind Cloudflare's check is passed out of sight: {}",
        s.items[0].error
    );
    // Nothing known yet (the check itself, the site's menu): the row stays.
    assert_eq!(
        engine.page_listed(&id, &[format!("{}/blog/other", t.ip)], &[], None),
        0
    );
    let shown = [
        format!("{}/blog/other", t.ip),
        "https://casino.example/win".to_string(),
        "https://pixeldrain.com/u/postone".to_string(),
        "https://pixeldrain.com/u/posttwo".to_string(),
    ];
    assert_eq!(engine.page_listed(&id, &shown, &[], Some("Example R")), 2);
    let s = wait_for(&engine, "the page's files, named", |s| {
        s.items.len() == 2 && s.items.iter().all(|i| i.name.ends_with(".bin"))
    })
    .await;
    assert!(s.items.iter().all(|i| i.package == "Pkg" && i.id != id));
}

/// A page passed out of sight that turns out to list nothing (a paste
/// holding only a host's name) fails saying so, rather than ask a person.
#[tokio::test]
async fn a_page_that_lists_nothing_fails_without_asking() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.set_pages_pass(true);
    engine.add_text(&format!("{}/blog/guarded", t.ip), "");
    let s = wait_for(&engine, "the page passed out of sight", |s| {
        s.items.iter().any(|i| i.status == "passing")
    })
    .await;
    let id = s.items[0].id.clone();
    engine.page_gone(&id, "the paste on zpaste.net lists no links");
    let s = wait_for(&engine, "the page to fail", |s| {
        s.items.iter().all(|i| i.status == "error")
    })
    .await;
    assert_eq!(s.items[0].error, "the paste on zpaste.net lists no links");
    assert_eq!(s.items[0].retry_in, 0, "not tried again on a timer");
}

/// A download whose every link is gone is not tried again on a timer, after
/// a restart either.
#[tokio::test]
async fn a_gone_download_is_not_retried_after_a_restart() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.set_pages_pass(true);
    engine.add_text(&format!("{}/blog/guarded", t.ip), "");
    let s = wait_for(&engine, "the page passed out of sight", |s| {
        s.items.iter().any(|i| i.status == "passing")
    })
    .await;
    let id = s.items[0].id.clone();
    engine.page_gone(&id, "the folder was deleted");
    let s = wait_for(&engine, "the download to fail", |s| {
        s.items.iter().all(|i| i.status == "error")
    })
    .await;
    assert_eq!(s.items[0].retry_in, 0, "not tried again on a timer");
    engine.shutdown().await;
    drop(engine);

    let engine = start_when_free(|| t.options()).await;
    engine.set_pages_pass(true);
    let s = engine.snapshot();
    assert_eq!(s.items[0].status, "error");
    assert_eq!(s.items[0].retry_in, 0, "nor after a restart");
}

/// A web page copied to the clipboard is looked at for links: one that
/// lists some becomes a wrapper of them, an ordinary page goes quietly, and
/// a site's front page is not taken at all.
#[tokio::test]
async fn a_copied_page_is_kept_only_if_it_lists_links() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    let added = engine.add_from_clipboard(&format!("{0}/blog/links {0}/blog/plain {0}/", t.ip));
    assert_eq!((added.added, added.pages), (0, 2));
    let s = wait_for(&engine, "the page unwrapped, the plain page gone", |s| {
        s.wrappers.iter().any(|w| w.status == "done")
            && s.items.len() == 2
            && s.items.iter().all(|i| !i.tentative)
    })
    .await;
    assert!(s.items.iter().all(|i| i.mirrors == ["pixeldrain.com"]));
    assert_eq!(s.counts.all, 2);
    assert_eq!(s.counts.need_you, 0, "nobody asked about the plain page");
}

/// A copied page whose address looks like a one-time link (an email's
/// confirmation, say) is never fetched: a plain GET could use it up.
#[tokio::test]
async fn a_copied_one_time_link_is_left_alone() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    let added = engine.add_from_clipboard(&format!("{}/blog/confirm?token=x", t.ip));
    assert_eq!((added.added, added.pages), (0, 0));
    // A page copied after it is looked at, so the engine had its chance.
    engine.add_from_clipboard(&format!("{}/blog/links", t.ip));
    wait_for(&engine, "the page unwrapped", |s| {
        s.wrappers.iter().any(|w| w.status == "done") && s.items.len() == 2
    })
    .await;
    assert!(t.server.requests("/blog/confirm").is_empty());
}

/// A copied page behind a check the desktop app passes waits, out of the
/// lists, for its pass; if the check wants a person after all, it goes.
#[tokio::test]
async fn a_copied_guarded_page_goes_if_it_wants_a_person() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.set_pages_pass(true);
    engine.add_from_clipboard(&format!("{}/blog/guarded", t.ip));
    let s = wait_for(&engine, "the page waiting for its pass", |s| {
        s.items.iter().any(|i| i.status == "passing")
    })
    .await;
    let id = s.items[0].id.clone();
    assert!(s.items[0].tentative && engine.is_tentative(&id));
    assert_eq!((s.counts.all, s.counts.unwrapping), (0, 1));
    assert!(!engine.page_wants_person(&id), "the page went");
    let s = engine.snapshot();
    assert!(s.items.is_empty() && s.counts.need_you == 0);
}

/// A copied page whose address reads like a file's name is still looked
/// at for links: kept as a wrapper if it lists some, gone if not, never
/// left in no list or asking anyone.
#[tokio::test]
async fn a_copied_page_named_like_a_file_is_still_judged() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    let mut events = engine.subscribe();
    engine.set_pages_pass(true);
    let added = engine.add_from_clipboard(&format!(
        "{0}/blog/Example.Set.v1.25.html {0}/blog/Plain.Notes.v1.25.html",
        t.ip
    ));
    assert_eq!(added.pages, 2);
    let s = wait_for(&engine, "the page unwrapped, the plain page gone", |s| {
        s.wrappers.iter().any(|w| w.status == "done")
            && s.items.len() == 2
            && s.items
                .iter()
                .all(|i| !i.tentative && i.name.ends_with(".bin"))
    })
    .await;
    assert_eq!(s.wrappers.len(), 1);
    let mut names: Vec<&str> = s.items.iter().map(|i| i.name.as_str()).collect();
    names.sort();
    assert_eq!(names, ["postone.bin", "posttwo.bin"]);
    assert_eq!(s.counts.unwrapping, 0);
    while let Ok(e) = events.try_recv() {
        assert!(!matches!(e, Event::NeedsUser { .. }), "{e:?}");
    }
}

/// A copied page whose pass hands back a file is a download like any.
#[tokio::test]
async fn a_copied_page_that_hands_back_a_file_is_a_download() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.set_pages_pass(true);
    engine.add_from_clipboard(&format!("{}/blog/guarded", t.ip));
    let s = wait_for(&engine, "the page waiting for its pass", |s| {
        s.items.iter().any(|i| i.status == "passing")
    })
    .await;
    let id = s.items[0].id.clone();
    let file = haul_core::hosts::Entry {
        url: format!("{}/files/one.bin", t.ip),
        name: Some("one.bin".to_string()),
        ..Default::default()
    };
    engine.set_files(&id, vec![file]).unwrap();
    let s = wait_for(&engine, "the file downloaded", all_done).await;
    let it = &s.items[0];
    assert!(!it.tentative);
    assert_eq!(it.list, "done");
    assert_eq!((s.counts.all, s.counts.unwrapping), (1, 0));
}

/// A host that gives no decent download without an account (rapidgator: a
/// wait and a captcha for every file) is off until its account is set: its
/// links are left out. Switched on without one, the file asks the person
/// for its page; switched off, it is back to that default; with the
/// account, it downloads through the host's API.
#[tokio::test]
async fn a_host_that_needs_an_account_is_off_until_switched_on() {
    let t = Setup::new().await;
    let mut cfg = t.config();
    cfg.endpoints.rapidgator = t.ip.clone();
    let engine = t.engine_with(cfg).await;
    // Off for want of an account, as the Hosts view shows it.
    let off = |s: &Snapshot| {
        let h = s.hosts.iter().find(|h| h.name == "rapidgator.net").unwrap();
        assert!(!h.disabled);
        h.needs_account
    };
    engine.add_text("https://rapidgator.net/file/abc/x.rar.html", "");
    let s = wait_for(&engine, "the link left out", |s| {
        s.items.iter().any(|i| {
            i.error
                .contains("only on hosts that are off: rapidgator.net")
        })
    })
    .await;
    assert!(off(&s));

    engine.set_disabled("rapidgator.net", false).unwrap();
    let s = wait_for(&engine, "the file to ask for its page", |s| {
        s.items.iter().any(|i| i.status == "captcha")
    })
    .await;
    assert!(
        s.items[0].error.contains("a free download is on its page"),
        "{}",
        s.items[0].error
    );
    assert!(!off(&s));
    assert_eq!(engine.cfg().accountless_hosts, ["rapidgator.net"]);
    assert!(
        t.server.requests("/api/v2/user/login").is_empty(),
        "no account to log in with"
    );

    engine.set_disabled("rapidgator.net", true).unwrap();
    assert!(off(&engine.snapshot()));
    assert!(engine.cfg().accountless_hosts.is_empty());
    assert!(
        engine.cfg().disabled_hosts.is_empty(),
        "off by default, not by the user"
    );

    let account = json!({"rapidgator_user": "u", "rapidgator_pass": "p"});
    engine
        .update_settings(account.as_object().unwrap())
        .unwrap();
    assert!(!off(&engine.snapshot()));
    engine.item_action(&s.items[0].id, ItemAction::RetryManual);
    // Through the API now, which checks the link (this one names no file).
    wait_for(&engine, "the file tried with the account", |s| {
        s.items[0].error.ends_with("bad rapidgator url")
    })
    .await;
}

/// Where the desktop app passes pages, a host whose page passes and that
/// the user switched on without an account has its link wait for the page,
/// and the folder the page hands back becomes one row per file. Until it
/// is switched on, its links are left out there too.
#[tokio::test]
async fn a_page_the_desktop_passes_stands_in_for_an_account_once_switched_on() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.set_pages_pass(true);
    engine.add_text("https://gofile.io/d/folder", "");
    wait_for(&engine, "the link left out", |s| {
        s.items
            .iter()
            .any(|i| i.error.contains("only on hosts that are off: gofile.io"))
    })
    .await;
    engine.set_disabled("gofile.io", false).unwrap();
    // Waiting for the desktop app to pass its page: nothing for the person.
    let s = wait_for(&engine, "the page to wait its turn", |s| {
        s.items.iter().any(|i| i.status == "passing")
    })
    .await;
    let gofile = s.hosts.iter().find(|h| h.name == "gofile.io").unwrap();
    assert!(!gofile.needs_account);
    let id = s.items[0].id.clone();
    // Its window came forward: now it needs the person.
    assert!(engine.page_wants_person(&id), "the download stays");
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

    // Without the desktop app, the same host asks the person for its page.
    engine.set_pages_pass(false);
    engine.add_text("https://gofile.io/d/other", "");
    wait_for(&engine, "the person asked", |s| {
        s.items.iter().any(|i| i.status == "captcha")
    })
    .await;
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
    engine.set_disabled("gofile.io", false).unwrap();
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
    engine.set_disabled("gofile.io", false).unwrap();
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
    engine.page_later(
        &id,
        "https://gofile.io/d/held",
        "vikingfile: too many requests",
        None,
    );
    let s = engine.snapshot();
    assert_eq!(s.items[0].status, "waiting");
    assert!(s.items[0].wait > 0, "it counts down to the next pass");
    assert_eq!(s.items[0].error, "vikingfile: too many requests");
}

/// An engine whose requests go out through the stand-in as their proxy, so
/// pages of hosts with no endpoint of their own (ddownload.com, paste2.org)
/// are answered by it, never by the real site. As with any proxy, each
/// host's pages open from that one address.
fn through_stand_in(t: &Setup) -> Options {
    let mut config = t.config();
    config.proxies = vec![t.ip.clone()];
    Options {
        config: Some(config),
        ..t.options()
    }
}

/// What the engine's own count of automatic retries says of row `id`.
fn auto_retries(engine: &Engine, id: &str) -> u64 {
    let seen = engine.debug_inspect();
    let items = seen["items"].as_array().unwrap();
    let it = items.iter().find(|i| i["id"] == id).unwrap();
    it["auto_retries"].as_u64().unwrap()
}

/// Waits for a link on ddownload to wait for its page's pass, and gives its
/// row's id.
async fn ddownload_waiting_its_pass(engine: &Engine, page: &str) -> String {
    engine.add_text(page, "");
    let links = |i: &haul_core::ItemView| engine.debug_links(&i.id).contains(&page.to_string());
    let s = wait_for(engine, "the page to wait its turn", |s| {
        s.items.iter().any(|i| links(i) && i.status == "passing")
    })
    .await;
    s.items.iter().find(|i| links(i)).unwrap().id.clone()
}

/// A wrapper that failed runs again when its retry comes due, here finding
/// what the first try did not.
#[tokio::test]
async fn a_failed_wrapper_runs_again_when_its_retry_comes_due() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.add_text("https://filecrypt.cc/Container/missing-live.html", "");
    let s = wrapper_settled(&engine, "error").await;
    assert!(s.wrappers[0].retry_in > 0, "due for a retry");
    assert!(engine.debug_retry_now(&s.wrappers[0].id));
    let s = wrapper_settled(&engine, "done").await;
    assert_eq!(s.wrappers[0].found.map(|f| f.files), Some(2));
    assert_eq!(s.counts.unwrapping, 0, "nothing left unwrapping");
}

/// So it does after a restart, which forgets that the wrapper was looked at.
#[tokio::test]
async fn a_failed_wrapper_runs_again_after_a_restart() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.add_text("https://filecrypt.cc/Container/missing-restart.html", "");
    wrapper_settled(&engine, "error").await;
    engine.shutdown().await;
    drop(engine);

    let engine = start_when_free(|| t.options()).await;
    let s = engine.snapshot();
    assert!(s.wrappers[0].retry_in > 0, "due for a retry again");
    assert!(engine.debug_retry_now(&s.wrappers[0].id));
    let s = wrapper_settled(&engine, "done").await;
    assert_eq!(s.counts.unwrapping, 0, "nothing left unwrapping");
}

/// A wrapper's retry is unwrapped at once, as when it was added, whatever
/// the queue is doing: its files only wait for the user, so it takes no
/// download's turn and waits for none.
#[tokio::test]
async fn a_wrapper_retry_does_not_wait_behind_downloads() {
    let t = Setup::new().await;
    let mut config = t.config();
    config.max_total = 1;
    config.rate_limit_mbps = 0.05;
    let engine = t.engine_with(config).await;
    engine.add_text(&format!("{}/files/long.bin", t.ip), "");
    wait_for(&engine, "the download to take the only slot", |s| {
        s.items.iter().any(|i| i.status == "downloading")
    })
    .await;
    engine.add_text("https://filecrypt.cc/Container/missing-busy.html", "");
    let s = wrapper_settled(&engine, "error").await;
    assert!(engine.debug_retry_now(&s.wrappers[0].id));
    wrapper_settled(&engine, "done").await;
    let s = engine.snapshot();
    assert!(
        s.items.iter().any(|i| i.status == "downloading"),
        "the download still has the slot"
    );
}

/// A paste Haul read itself that lists no links is not tried again on a
/// timer, as with one the desktop app passed, after a restart either.
#[tokio::test]
async fn a_paste_that_lists_no_links_is_not_retried_on_a_timer() {
    let t = Setup::new().await;
    let engine = Engine::start(through_stand_in(&t)).await.unwrap();
    engine.add_text("http://paste2.org/paste/abc123", "");
    let s = wrapper_settled(&engine, "error").await;
    assert!(
        haul_core::hosts::lists_no_links(&s.wrappers[0].error),
        "{}",
        s.wrappers[0].error
    );
    assert_eq!(s.wrappers[0].retry_in, 0, "not tried again on a timer");
    engine.shutdown().await;
    drop(engine);

    let engine = start_when_free(|| through_stand_in(&t)).await;
    let s = engine.snapshot();
    assert_eq!(s.wrappers[0].status, "error");
    assert_eq!(s.wrappers[0].retry_in, 0, "nor after a restart");
}

/// A failed download whose name no host gave is looked up on its retry, not
/// left waiting for a lookup that never comes, after a restart.
#[tokio::test]
async fn a_failed_download_without_a_name_retries_after_a_restart() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.add_text(&format!("{}/missing/", t.ip), "");
    let s = wait_for(&engine, "the failure", |s| {
        s.items.iter().any(|i| i.status == "error")
    })
    .await;
    let id = s.items[0].id.clone();
    engine.shutdown().await;
    drop(engine);

    let engine = start_when_free(|| t.options()).await;
    assert!(engine.debug_retry_now(&id));
    wait_for(&engine, "the retry to fail again", |s| {
        auto_retries(&engine, &id) == 1 && s.items[0].status == "error"
    })
    .await;
}

/// A host's limit on an address, which its page states: the row waits it out
/// rather than ask a person, and goes again by itself when it ends.
#[tokio::test]
async fn a_host_limit_waits_and_goes_again_by_itself() {
    let t = Setup::new().await;
    let engine = Engine::start(through_stand_in(&t)).await.unwrap();
    engine.set_pages_pass(true);
    let page = "http://ddownload.com/dd/abc123";
    let id = ddownload_waiting_its_pass(&engine, page).await;

    engine.page_later(&id, page, "ddownload: limit", Some(2));
    let s = wait_for(&engine, "the row to wait", |s| {
        s.items[0].status == "waiting"
    })
    .await;
    assert!(s.items[0].wait <= 2, "as long as the page said");
    assert_eq!(s.items[0].list, "active", "not under Needs you");
    assert_eq!(s.counts.need_you, 0);
    assert!(!engine.page_passes(page), "no pass while it holds");

    let s = wait_for(&engine, "the row to go again", |s| {
        s.items[0].status == "passing"
    })
    .await;
    assert_eq!(s.items[0].open_urls, [page]);
    assert!(engine.page_passes(page), "passed again");
}

/// The limit is the address's, not the file's: other rows on that host wait
/// out the same cooldown, and none opens a page meanwhile, a row under
/// Needs you or one added since included.
#[tokio::test]
async fn a_host_limit_holds_every_row_on_that_host() {
    let t = Setup::new().await;
    let engine = Engine::start(through_stand_in(&t)).await.unwrap();
    engine.set_pages_pass(true);
    let page = "http://ddownload.com/dd/abc123";
    let id = ddownload_waiting_its_pass(&engine, page).await;
    let other = "http://ddownload.com/dd/def456";
    ddownload_waiting_its_pass(&engine, other).await;
    let wanted = "http://ddownload.com/dd/ghi789";
    let wants = ddownload_waiting_its_pass(&engine, wanted).await;
    assert!(engine.page_wants_person(&wants));
    wait_for(&engine, "the row under Needs you", |s| {
        s.counts.need_you == 1
    })
    .await;

    engine.page_later(&id, page, "ddownload: limit", Some(3600));
    let s = wait_for(&engine, "every row to wait", |s| {
        s.items.iter().all(|i| i.status == "waiting")
    })
    .await;
    assert_eq!(s.counts.need_you, 0);
    for it in &s.items {
        assert!(
            (3590..=3600).contains(&it.wait),
            "one wait for all: {}",
            it.wait
        );
    }
    let host = s.hosts.iter().find(|h| h.name == "ddownload.com").unwrap();
    assert!((3590..=3600).contains(&host.cooldown), "{}", host.cooldown);
    for link in [page, other, wanted] {
        assert!(!engine.page_passes(link), "{link} is not passed");
    }

    // One added since waits too, and opens no page either.
    engine.add_text("http://ddownload.com/dd/jkl012", "");
    wait_for(&engine, "the new row, looked up", |s| {
        s.items.len() == 4 && s.items.iter().all(|i| i.status != "checking")
    })
    .await;
    tokio::time::sleep(Duration::from_millis(600)).await;
    let s = engine.snapshot();
    assert!(
        !s.items
            .iter()
            .any(|i| matches!(i.status, "passing" | "captcha")),
        "{:?}",
        s.items.iter().map(|i| i.status).collect::<Vec<_>>()
    );
    assert_eq!(s.counts.need_you, 0);
}

/// Any wait a host asks of the address holds its pages' passes the same way:
/// here a 429 to another of the host's links, while a page waits its pass.
#[tokio::test]
async fn a_hosts_rate_limit_holds_its_pages_passes() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.set_pages_pass(true);
    let page = format!("{}/blog/guarded", t.ip);
    engine.add_text(&page, "");
    wait_for(&engine, "the page to wait its turn", |s| {
        s.items.iter().any(|i| i.status == "passing")
    })
    .await;
    assert!(engine.page_passes(&page));
    engine.add_text(&format!("{}/limited-long/d.bin", t.ip), "");
    let s = wait_for(&engine, "both to wait", |s| {
        s.items.len() == 2 && s.items.iter().all(|i| i.status == "waiting")
    })
    .await;
    assert_eq!(s.counts.need_you, 0, "no moment under Needs you");
    assert!(!engine.page_passes(&page), "no pass meanwhile");
}

/// A new address from the router ends the limit on the old one: the row goes
/// again rather than wait out what no longer holds.
#[tokio::test]
async fn a_new_address_ends_a_host_limit() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.set_pages_pass(true);
    let page = format!("{}/blog/guarded", t.ip);
    engine.add_text(&page, "");
    let s = wait_for(&engine, "the page to wait its turn", |s| {
        s.items.iter().any(|i| i.status == "passing")
    })
    .await;
    engine.page_later(&s.items[0].id, &page, "a limit", Some(3600));
    wait_for(&engine, "the row to wait", |s| {
        s.items[0].status == "waiting"
    })
    .await;
    engine.find_router().await;
    engine.reconnect("a test").await.unwrap();
    wait_for(&engine, "the row to go again", |s| {
        s.items[0].status == "passing"
    })
    .await;
    assert!(engine.page_passes(&page));
}

/// A page's word on how long is taken within reason: a day at most.
#[tokio::test]
async fn a_host_limit_is_a_day_at_most() {
    let t = Setup::new().await;
    let engine = Engine::start(through_stand_in(&t)).await.unwrap();
    engine.set_pages_pass(true);
    let page = "http://ddownload.com/dd/abc123";
    let id = ddownload_waiting_its_pass(&engine, page).await;
    engine.page_later(&id, page, "ddownload: limit", Some(u64::MAX));
    let s = wait_for(&engine, "the row to wait", |s| {
        s.items[0].status == "waiting"
    })
    .await;
    assert!(
        (86_390..=86_400).contains(&s.items[0].wait),
        "{}",
        s.items[0].wait
    );
}

/// A row with another usable mirror goes on to it at once rather than wait
/// out the limit, even a host Haul knows nothing of, which it held back for
/// the page's pass.
#[tokio::test]
async fn a_host_limit_sends_the_row_to_its_other_mirror() {
    let t = Setup::new().await;
    let engine = Engine::start(through_stand_in(&t)).await.unwrap();
    engine.set_pages_pass(true);
    engine.add_text("http://fileguard.cc/vault/ddownload-held", "");
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
    assert!(t.server.requests("/files/held.bin").is_empty());

    let page = "http://ddownload.com/dd/ddownload-held";
    engine.page_later(&id, page, "ddownload: limit", Some(3600));
    wait_for(&engine, "the other mirror", all_done).await;
    assert!(!t.server.requests("/files/held.bin").is_empty());
}

/// A row with another page to pass goes on to that one while the limit holds:
/// the pass and the person's button both leave the limited page out.
#[tokio::test]
async fn a_host_limit_leaves_the_rows_other_page_to_pass() {
    let t = Setup::new().await;
    let engine = Engine::start(through_stand_in(&t)).await.unwrap();
    engine.set_pages_pass(true);
    engine.set_disabled("gofile.io", false).unwrap();
    engine.add_text("http://fileguard.cc/vault/ddownload-gofile", "");
    let w = wrapper_settled(&engine, "done").await.wrappers[0]
        .id
        .clone();
    let s = wait_for(&engine, "the file", |s| s.items.len() == 1).await;
    let id = s.items[0].id.clone();
    engine.start_wrapper(&w);
    let s = wait_for(&engine, "both pages to wait", |s| {
        s.items[0].status == "passing" && s.items[0].open_urls.len() == 2
    })
    .await;
    let page = "http://ddownload.com/dd/ddownload-gofile";
    assert_eq!(s.items[0].open_urls[0], page);

    engine.page_later(&id, page, "ddownload: limit", Some(3600));
    tokio::time::sleep(Duration::from_millis(600)).await;
    let s = engine.snapshot();
    assert_eq!(s.items[0].status, "passing", "{}", s.items[0].error);
    assert!(!engine.page_passes(page));
    assert!(engine.page_passes("https://gofile.io/d/both"));
    assert_eq!(
        engine.page_for(&id).as_deref(),
        Some("https://gofile.io/d/both")
    );
}

/// A row beside the limit with a page only a person can do is for that
/// person: under Needs you, its button on that page.
#[tokio::test]
async fn a_host_limit_leaves_a_page_for_a_person_under_needs_you() {
    let t = Setup::new().await;
    let engine = Engine::start(through_stand_in(&t)).await.unwrap();
    engine.set_pages_pass(true);
    engine.set_disabled("rapidgator.net", false).unwrap();
    engine.add_text("http://fileguard.cc/vault/ddownload-rapidgator", "");
    let w = wrapper_settled(&engine, "done").await.wrappers[0]
        .id
        .clone();
    let s = wait_for(&engine, "the file", |s| s.items.len() == 1).await;
    let id = s.items[0].id.clone();
    engine.start_wrapper(&w);
    wait_for(&engine, "both pages to wait", |s| {
        s.items[0].open_urls.len() == 2
    })
    .await;
    let page = "http://ddownload.com/dd/ddownload-rapidgator";
    engine.page_later(&id, page, "ddownload: limit", Some(3600));
    let s = wait_for(&engine, "the person's page", |s| {
        s.items[0].status == "captcha"
    })
    .await;
    assert_eq!(s.counts.need_you, 1);
    assert_eq!(
        engine.page_for(&id).as_deref(),
        Some("https://rapidgator.net/file/person")
    );
}

/// A page that wanted a person is passed again by itself, on the failed
/// downloads' timer and limit, telling nobody; once the tries are used up it
/// stays for the person. In between it waits for them as before.
#[tokio::test]
async fn a_page_that_wants_a_person_is_tried_again_by_itself() {
    let t = Setup::new().await;
    let engine = Engine::start(through_stand_in(&t)).await.unwrap();
    engine.set_pages_pass(true);
    let page = "http://ddownload.com/dd/abc123";
    let mut events = engine.subscribe();
    let id = ddownload_waiting_its_pass(&engine, page).await;

    assert!(engine.page_wants_person(&id), "the person is told once");
    let s = wait_for(&engine, "the row under Needs you", |s| {
        s.items[0].status == "captcha"
    })
    .await;
    assert_eq!(s.items[0].list, "attention");
    assert_eq!(s.items[0].open_urls, [page], "Open here has its page");
    assert!(
        (890..=900).contains(&s.items[0].retry_in),
        "{}",
        s.items[0].retry_in
    );
    while events.try_recv().is_ok() {}

    for round in 1..=3 {
        assert!(engine.debug_retry_now(&id), "try {round}");
        let s = wait_for(&engine, "the page's next pass", |s| {
            s.items[0].status == "passing"
        })
        .await;
        assert_eq!(s.items[0].retry_in, 0, "a try under way");
        assert_eq!(auto_retries(&engine, &id), round);
        assert!(!engine.page_wants_person(&id), "nobody is told");
        let s = wait_for(&engine, "the row back under Needs you", |s| {
            s.items[0].status == "captcha"
        })
        .await;
        assert_eq!(s.items[0].open_urls, [page]);
        if round < 3 {
            assert!(s.items[0].retry_in > 0, "try {round} is not the last");
        }
    }
    let s = engine.snapshot();
    assert_eq!(s.items[0].retry_in, 0, "its tries are used up");
    assert!(!engine.debug_retry_now(&id));
    tokio::time::sleep(Duration::from_millis(600)).await;
    let s2 = engine.snapshot();
    assert_eq!(s2.items[0].status, "captcha", "it stays for the person");
    assert!(
        !std::iter::from_fn(|| events.try_recv().ok())
            .any(|e| matches!(e, Event::NeedsUser { .. })),
        "nobody was told again"
    );
}

/// So is a wrapper's page, whose card says when.
#[tokio::test]
async fn a_wrappers_page_that_wants_a_person_is_tried_again_by_itself() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.set_pages_pass(true);
    engine.add_text("https://filecrypt.cc/Container/cf-always.html", "");
    let s = wrapper_settled(&engine, "passing").await;
    let id = s.wrappers[0].id.clone();
    assert!(engine.page_wants_person(&id));
    let s = wrapper_settled(&engine, "captcha").await;
    assert!(s.wrapper_cards[0].retry_in > 0, "the card says when");
    assert_eq!(s.wrapper_cards[0].need_you, 1);
    assert!(engine.debug_retry_now(&id));
    wrapper_settled(&engine, "passing").await;
    assert!(!engine.page_wants_person(&id), "nobody is told");
    let s = wrapper_settled(&engine, "captcha").await;
    assert!(s.wrappers[0].retry_in > 0);
}

/// The retries follow the setting: none while it is off, planned when it
/// comes on.
#[tokio::test]
async fn a_page_is_tried_again_only_while_retries_are_on() {
    let t = Setup::new().await;
    let mut options = through_stand_in(&t);
    options.config.as_mut().unwrap().retry_failed_minutes = 0;
    let engine = Engine::start(options).await.unwrap();
    engine.set_pages_pass(true);
    let id = ddownload_waiting_its_pass(&engine, "http://ddownload.com/dd/abc123").await;
    assert!(engine.page_wants_person(&id));
    let s = wait_for(&engine, "the row under Needs you", |s| {
        s.items[0].status == "captcha"
    })
    .await;
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

/// What a person does comes first: trying again by hand starts the count
/// over, a link they hand back ends the retries, and so does removing the row.
#[tokio::test]
async fn a_persons_action_ends_a_pages_retries() {
    let t = Setup::new().await;
    let engine = Engine::start(through_stand_in(&t)).await.unwrap();
    engine.set_pages_pass(true);
    let id = ddownload_waiting_its_pass(&engine, "http://ddownload.com/dd/abc123").await;
    let under_needs_you = |s: &Snapshot| s.items[0].status == "captcha";
    assert!(engine.page_wants_person(&id));
    wait_for(&engine, "the row under Needs you", under_needs_you).await;
    assert!(engine.debug_retry_now(&id));
    wait_for(&engine, "its pass", |s| s.items[0].status == "passing").await;
    assert!(!engine.page_wants_person(&id));
    wait_for(&engine, "the row back", under_needs_you).await;
    assert_eq!(auto_retries(&engine, &id), 1);

    // Tried again by hand: the page's first pass again, and the person is
    // told again if it wants them.
    assert!(engine.item_action(&id, ItemAction::RetryManual));
    let s = wait_for(&engine, "its pass", |s| s.items[0].status == "passing").await;
    assert_eq!(s.items[0].retry_in, 0);
    assert_eq!(auto_retries(&engine, &id), 0);
    assert!(engine.page_wants_person(&id), "told again");

    // A link handed back.
    wait_for(&engine, "the row back", under_needs_you).await;
    let link = format!("{}/files/abc123.rar", t.ip);
    engine
        .set_direct(&id, &link, BTreeMap::new(), None)
        .unwrap();
    let s = wait_for(&engine, "the file", all_done).await;
    assert_eq!(s.items[0].retry_in, 0);

    // Removed.
    let other = ddownload_waiting_its_pass(&engine, "http://ddownload.com/dd/def456").await;
    assert!(engine.page_wants_person(&other));
    wait_for(&engine, "the other row under Needs you", |s| {
        s.counts.need_you == 1
    })
    .await;
    assert!(engine.item_action(&other, ItemAction::Remove));
    assert!(!engine.debug_retry_now(&other));
}

/// A page a person has open is theirs: a try that comes due meanwhile puts
/// itself off, without using up a try, and the row stays under Needs you.
#[tokio::test]
async fn a_page_a_person_has_open_is_not_tried_again() {
    let t = Setup::new().await;
    let engine = Engine::start(through_stand_in(&t)).await.unwrap();
    engine.set_pages_pass(true);
    let id = ddownload_waiting_its_pass(&engine, "http://ddownload.com/dd/abc123").await;
    assert!(engine.page_wants_person(&id));
    wait_for(&engine, "the row under Needs you", |s| {
        s.items[0].status == "captcha"
    })
    .await;
    assert!(engine.debug_retry_now(&id));
    wait_for(&engine, "the try to come due", |s| {
        s.items[0].status == "passing"
    })
    .await;

    engine.page_with_person(&id);
    let s = engine.snapshot();
    assert_eq!(s.items[0].status, "captcha", "back with the person");
    assert!(s.items[0].retry_in > 800, "{}", s.items[0].retry_in);
    assert_eq!(auto_retries(&engine, &id), 0, "that try did not count");
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(engine.snapshot().items[0].status, "captcha");

    // A row on its first pass is not the person's yet: nothing changes.
    let other = ddownload_waiting_its_pass(&engine, "http://ddownload.com/dd/def456").await;
    engine.page_with_person(&other);
    let s = engine.snapshot();
    let row = s.items.iter().find(|i| i.id == other).unwrap();
    assert_eq!(row.status, "passing");
}

/// What cannot succeed by itself is not tried again: a page on a host now
/// off for want of its account, a mirror marked gone, a page only a person
/// can do, or a page that shows all it will and wants a click on it.
#[tokio::test]
async fn a_page_that_cannot_pass_by_itself_is_not_tried_again() {
    let t = Setup::new().await;
    let engine = Engine::start(through_stand_in(&t)).await.unwrap();
    engine.set_pages_pass(true);

    // A host off for want of its account.
    engine.set_disabled("gofile.io", false).unwrap();
    engine.add_text("https://gofile.io/d/abc123", "");
    let s = wait_for(&engine, "the page to wait its turn", |s| {
        s.items.iter().any(|i| i.status == "passing")
    })
    .await;
    let id = s.items[0].id.clone();
    assert!(engine.page_wants_person(&id));
    wait_for(&engine, "the row under Needs you", |s| {
        s.items[0].status == "captcha" && s.items[0].retry_in > 0
    })
    .await;
    engine.set_disabled("gofile.io", true).unwrap();
    assert!(engine.debug_retry_now(&id));
    let s = wait_for(&engine, "the try to be dropped", |s| {
        s.items[0].retry_in == 0
    })
    .await;
    assert_ne!(s.items[0].status, "passing");
    assert_eq!(auto_retries(&engine, &id), 0);
    assert!(engine.item_action(&id, ItemAction::Remove));

    // A mirror marked gone, leaving a page only a person can do.
    engine.set_disabled("rapidgator.net", false).unwrap();
    engine.add_text("http://fileguard.cc/vault/ddownload-rapidgator", "");
    let w = wait_for(&engine, "the wrapper", |s| {
        s.wrappers.iter().any(|w| w.status == "done")
    })
    .await
    .wrappers[0]
        .id
        .clone();
    let s = wait_for(&engine, "the file", |s| s.items.len() == 1).await;
    let id = s.items[0].id.clone();
    engine.start_wrapper(&w);
    wait_for(&engine, "both pages to wait", |s| {
        s.items[0].open_urls.len() == 2
    })
    .await;
    engine.page_gone(&id, "ddownload: the file was deleted");
    let s = wait_for(&engine, "the person's page", |s| {
        s.items[0].open_urls == ["https://rapidgator.net/file/person"]
            && s.items[0].status == "captcha"
    })
    .await;
    assert_eq!(s.items[0].retry_in, 0);
    assert!(engine.page_wants_person(&id));
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(engine.snapshot().items[0].retry_in, 0, "only a person can");
    assert!(engine.item_action(&id, ItemAction::Remove));

    // A page that shows all it will and wants a click.
    let id = ddownload_waiting_its_pass(&engine, "http://ddownload.com/dd/abc123").await;
    assert!(engine.page_needs_click(&id), "the person is told");
    let s = wait_for(&engine, "the row under Needs you", |s| {
        s.items[0].status == "captcha"
    })
    .await;
    assert_eq!(s.items[0].retry_in, 0, "another look shows the same");
}

/// Downloads with data on disk start first, the nearest to done first.
#[tokio::test]
async fn the_nearest_to_done_resumes_first() {
    let t = Setup::new().await;
    let mut cfg = t.config();
    cfg.max_total = 1;
    let engine = t.engine_with(cfg).await;
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

/// A multi-part archive lists part 1, part 2… on every host: one row per part,
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
    assert_eq!(s.wrappers[0].title.as_deref(), Some("Example R"));
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
    // Each looked up at its host.
    let s = wait_for(&engine, "the parts", |s| {
        s.items.len() == 2 && s.items.iter().all(|i| i.size > 0)
    })
    .await;
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

/// A container whose gate Haul cannot answer waits for a person; the page
/// the browser then shows (the parts of it Haul reads) unpacks it as Haul's
/// own read of the page would, while a page still at its gate changes
/// nothing.
#[tokio::test]
async fn a_container_page_the_browser_shows_unpacks() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.add_text("https://filecrypt.cc/Container/abc.html", "");
    let s = wrapper_settled(&engine, "captcha").await;
    let id = s.wrappers[0].id.clone();
    assert_eq!(engine.page_shown(&id, FC_GATE), 0);
    assert_eq!(engine.snapshot().wrappers[0].status, "captcha");

    let host = t.ip.trim_start_matches("http://");
    let shown = fc_solved_with(
        "Example.Set",
        &[
            format!("http://{host}/files/one.bin"),
            format!("http://{host}/files/two.bin"),
        ],
    );
    assert_eq!(engine.page_shown(&id, &shown), 2);
    let s = wrapper_settled(&engine, "done").await;
    assert_eq!(s.wrappers[0].title.as_deref(), Some("Example.Set"));
    assert_eq!(s.wrappers[0].found, Some(Found { files: 2, links: 2 }));
    wait_for(&engine, "the container's files", |s| {
        s.items.len() == 2 && s.items.iter().all(|i| i.status == "paused")
    })
    .await;
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
    assert_eq!(s.wrappers[0].title.as_deref(), Some("Open Example"));
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

/// Containers behind Cloudflare's check wait for the desktop app to pass
/// one; the clearance it hands back lets Haul answer each waiting one's
/// checkbox itself. One still turned away is passed the usual way after,
/// with no clearance handed over and over.
#[tokio::test]
async fn a_clearance_lets_haul_read_the_sites_waiting_containers() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.set_pages_pass(true);
    let (once, always) = (
        "https://filecrypt.cc/Container/cf-once.html",
        "https://filecrypt.cc/Container/cf-always.html",
    );
    engine.add_text(&format!("{once}\n{always}"), "");
    let s = wait_for(&engine, "both containers passed out of sight", |s| {
        s.wrappers.len() == 2 && s.wrappers.iter().all(|w| w.status == "passing")
    })
    .await;
    assert!(engine.takes_clearance(once) && engine.takes_clearance(always));
    let id = s
        .wrappers
        .iter()
        .find(|w| w.url == once)
        .unwrap()
        .id
        .clone();
    engine.page_cleared(
        &id,
        &["cf_clearance=ok; Path=/".to_string()],
        Some("Browser/1.0"),
    );
    let s = wait_for(&engine, "the cleared container unpacked", |s| {
        s.wrappers
            .iter()
            .any(|w| w.url == once && w.status == "done")
            && s.wrappers
                .iter()
                .any(|w| w.url == always && w.status == "passing")
    })
    .await;
    let opened = s.wrappers.iter().find(|w| w.url == once).unwrap();
    assert_eq!(opened.found, Some(Found { files: 2, links: 2 }));
    assert!(
        !engine.takes_clearance(always),
        "turned away after a clearance, it is passed the usual way"
    );
    let still = s
        .wrappers
        .iter()
        .find(|w| w.url == always)
        .unwrap()
        .id
        .clone();
    assert!(engine.item_action(&still, ItemAction::RetryManual));
    assert!(engine.takes_clearance(always), "tried again by hand");
}

/// A wrapper whose page passed out of sight still wants a person shows so
/// at once, even while downloads are paused: it has no other link to go on
/// with, so the notification asking for the person is never left without a
/// card to act on.
#[tokio::test]
async fn a_wrapper_that_wants_a_person_says_so_while_paused() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.set_pages_pass(true);
    engine.set_paused(true);
    engine.add_text("https://filecrypt.cc/Container/cf-always.html", "");
    let s = wrapper_settled(&engine, "passing").await;
    assert!(engine.page_wants_person(&s.wrappers[0].id));
    let s = wrapper_settled(&engine, "captcha").await;
    assert_eq!(s.wrapper_cards[0].need_you, 1);
}

/// A gate Haul has no code for goes to the browser, which may be let through,
/// even with the site's clearance held: no clearance is taken from that pass,
/// which would only bring Haul back to the same gate.
#[tokio::test]
async fn a_container_behind_another_gate_is_passed_in_the_browser() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.set_pages_pass(true);
    let page = "https://filecrypt.cc/Container/other-gate.html";
    engine.add_text(page, "");
    let s = wrapper_settled(&engine, "passing").await;
    assert!(!engine.takes_clearance(page));
    let said = &s.wrappers[0].error;
    assert!(said.contains("gate Haul cannot answer"), "{said}");

    // What the browser then shows unpacks it, as for any page handed over.
    let host = t.ip.trim_start_matches("http://");
    let shown = fc_solved_with("Example.Set", &[format!("http://{host}/files/one.bin")]);
    assert_eq!(engine.page_shown(&s.wrappers[0].id, &shown), 1);
    wrapper_settled(&engine, "done").await;
}

/// Without a browser to pass it, the same gate waits for a person.
#[tokio::test]
async fn a_container_behind_another_gate_waits_for_a_person_without_a_browser() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.add_text("https://filecrypt.cc/Container/other-gate.html", "");
    wrapper_settled(&engine, "captcha").await;
}

/// A page passed out of sight that fails for now says why and stays due for
/// its retries, unlike one that says its file is gone.
#[tokio::test]
async fn a_page_that_fails_for_now_is_not_gone_for_good() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.set_pages_pass(true);
    engine.add_text(&format!("{}/blog/guarded", t.ip), "");
    let s = wait_for(&engine, "the page passed out of sight", |s| {
        s.items.iter().any(|i| i.status == "passing")
    })
    .await;
    engine.page_failed(&s.items[0].id, "answered Not Found");
    let s = wait_for(&engine, "the download to fail", |s| {
        s.items.iter().all(|i| i.status == "error")
    })
    .await;
    assert!(s.items[0].error.contains("answered Not Found"));
    assert!(s.items[0].retry_in > 0, "tried again on its timer");
}

/// A wrapper's site shows under Wrappers, not among the hosts, and one the
/// user turned off is left alone, saying so.
#[tokio::test]
async fn a_wrapper_site_turned_off_is_not_unwrapped() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.set_disabled("filecrypt.cc", true).unwrap();
    engine.add_text("https://filecrypt.cc/Container/open.html", "");
    let s = wait_for(&engine, "the wrapper left alone", |s| {
        s.wrappers.len() == 1 && s.wrappers[0].error.contains("filecrypt.cc")
    })
    .await;
    assert_ne!(s.wrappers[0].status, "done", "{:?}", s.wrappers[0]);
    let site = s.hosts.iter().find(|h| h.name == "filecrypt.cc").unwrap();
    assert_eq!((site.kind, site.group), ("wrapper", "wrapper"));
}

/// Dropping a host from a wrapper's card takes its links off the files the
/// wrapper brought; a file left on no host goes from the list.
#[tokio::test]
async fn a_host_dropped_from_a_wrapper_leaves_its_files() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.add_text("https://fileguard.cc/vault/abc", "");
    let s = wait_for(&engine, "the wrapper's file", |s| {
        s.items.len() == 1 && s.wrapper_cards.len() == 1
    })
    .await;
    let ids = s.wrapper_cards[0].ids.clone();
    assert_eq!(engine.drop_wrapper_host(&ids, "pixeldrain.com"), 1);
    let s = engine.snapshot();
    let mut hosts = s.items[0].mirrors.clone();
    hosts.sort();
    assert_eq!(hosts, ["127.0.0.1", "localhost"]);
    assert!(
        !s.wrapper_cards[0]
            .hosts
            .contains(&"pixeldrain.com".to_string())
    );
    assert_eq!(
        engine.drop_wrapper_host(&ids, "pixeldrain.com"),
        0,
        "gone already"
    );
    engine.drop_wrapper_host(&ids, "127.0.0.1");
    engine.drop_wrapper_host(&ids, "localhost");
    let s = engine.snapshot();
    assert!(s.items.is_empty(), "on no host any more: {:?}", s.items);
    assert_eq!(s.wrappers.len(), 1, "the wrapper stays");
}

/// A page that lists links is read for its links without a setting of its own, so
/// its site is not listed among the hosts, until the user switches it off.
#[tokio::test]
async fn a_page_site_is_listed_only_when_switched_off() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.add_text(&format!("{}/blog/links", t.ip), "");
    let s = wait_for(&engine, "the page's files", |s| s.items.len() == 2).await;
    let site = haul_core::hosts::host_of(&t.ip);
    let listed = |s: &Snapshot| {
        s.hosts
            .iter()
            .find(|h| h.name == site)
            .is_some_and(|h| h.listed)
    };
    assert!(!listed(&s), "{:?}", s.hosts);
    engine.set_disabled(&site, true).unwrap();
    let s = engine.snapshot();
    assert!(listed(&s));
    let h = s.hosts.iter().find(|h| h.name == site).unwrap();
    assert_eq!((h.kind, h.group), ("site", "site"));
}

/// A page that links a container per host, each host naming the
/// parts its own way, is one archive: each part has every host's link, and
/// all parts take the name that says most of the files', so they unpack
/// together. The containers' title, which unrelated files can share, is not
/// it.
#[tokio::test]
async fn containers_per_host_merge_their_parts_by_number() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.add_text(&format!("{}/blog/per-host", t.ip), "");
    let s = wait_for(&engine, "both containers unwrapped", |s| {
        s.wrappers
            .iter()
            .filter(|w| w.host == "filecrypt.cc" && w.status == "done")
            .count()
            == 2
    })
    .await;
    let mut files: Vec<(String, usize)> = s
        .items
        .iter()
        .map(|i| (i.name.clone(), i.mirrors.len()))
        .collect();
    files.sort();
    assert_eq!(
        files,
        [
            ("Example.Archive.part1.rar".to_string(), 2),
            ("Example.Archive.part2.rar".to_string(), 2)
        ],
        "{:?}",
        s.items
    );
    assert_eq!(s.archives.len(), 1);
}

/// Containers under one title with the same part numbers are told apart by
/// the sizes their pages show. The same sizes are one archive.
#[tokio::test]
async fn containers_of_the_same_sizes_merge() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.add_text(&format!("{}/blog/per-host-same", t.ip), "");
    let s = wait_for(&engine, "both containers unwrapped", |s| {
        s.wrappers
            .iter()
            .filter(|w| w.host == "filecrypt.cc" && w.status == "done")
            .count()
            == 2
    })
    .await;
    assert_eq!(s.archives.len(), 1, "{:?}", s.archives);
    assert_eq!(s.items.len(), 2, "{:?}", s.items);
    assert!(
        s.items.iter().all(|i| i.mirrors.len() == 2),
        "{:?}",
        s.items
    );
}

/// A clearly other size, even of the last part alone as sets split alike
/// show, is another set of files: both stay whole, each under its own name.
#[tokio::test]
async fn containers_of_other_sizes_stay_apart() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.add_text(&format!("{}/blog/two-sets", t.ip), "");
    let s = wait_for(&engine, "both containers unwrapped", |s| {
        s.wrappers
            .iter()
            .filter(|w| w.host == "filecrypt.cc" && w.status == "done")
            .count()
            == 2
    })
    .await;
    let mut files: Vec<(String, usize)> = s
        .items
        .iter()
        .map(|i| (i.name.clone(), i.mirrors.len()))
        .collect();
    files.sort();
    assert_eq!(
        files,
        [
            ("Archive.Four.part1.rar".to_string(), 1),
            ("Archive.Four.part2.rar".to_string(), 1),
            ("Archive.Three.part1.rar".to_string(), 1),
            ("Archive.Three.part2.rar".to_string(), 1),
        ],
        "{:?}",
        s.items
    );
    assert_eq!(s.archives.len(), 2, "{:?}", s.archives);
}

/// With only ids to choose from the parts keep one, which unrelated files
/// won't share, and only their card shows the title.
#[tokio::test]
async fn containers_per_host_named_by_ids_keep_one_on_disk() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.add_text(&format!("{}/blog/per-host-ids", t.ip), "");
    let s = wait_for(&engine, "both containers unwrapped", |s| {
        s.wrappers
            .iter()
            .filter(|w| w.host == "filecrypt.cc" && w.status == "done")
            .count()
            == 2
    })
    .await;
    let mut files: Vec<(String, usize)> = s
        .items
        .iter()
        .map(|i| (i.name.clone(), i.mirrors.len()))
        .collect();
    files.sort();
    assert_eq!(
        files,
        [
            ("0123456789abcdef0000.part1.rar".to_string(), 2),
            ("0123456789abcdef0000.part2.rar".to_string(), 2)
        ],
        "{:?}",
        s.items
    );
    assert_eq!(s.archives.len(), 1);
    assert_eq!(s.archives[0].name, "Folder.rar");
}

/// Every site Haul knows is listed before any link is on it, so it can be
/// set up beforehand; a host's other domain is not listed apart.
#[tokio::test]
async fn every_known_site_is_listed() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    let s = engine.snapshot();
    let kind = |name: &str| {
        s.hosts
            .iter()
            .find(|h| h.name == name && h.listed)
            .map(|h| h.kind)
    };
    assert_eq!(kind("filecrypt.cc"), Some("wrapper"));
    assert_eq!(kind("zpaste.net"), Some("paste"));
    assert_eq!(kind("1fichier.com"), Some("host"));
    assert_eq!(kind("vik1ngfile.site"), None, "vikingfile.com's");
    assert_eq!(kind("rentry.org"), None, "rentry.co's");
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
async fn preferred_hosts_go_first() {
    let t = Setup::new().await;
    // The mechanism itself, without the shipped default preference.
    let engine = t
        .engine_with(Config {
            preferred_hosts: Vec::new(),
            ..t.config()
        })
        .await;
    engine.set_preferred("localhost", true).unwrap();
    let both = |name: &str| format!("{}/slow/{name} {}/slow/{name}", t.ip, t.name);
    engine.add_text(&format!("{} {}", both("q1.bin"), both("q2.bin")), "");
    let s = wait_for(&engine, "done", |s| s.items.len() == 2 && all_done(s)).await;
    let ok = |name: &str| s.hosts.iter().find(|h| h.name == name).map_or(0, |h| h.ok);
    assert_eq!(
        (ok("localhost"), ok("127.0.0.1")),
        (2, 0),
        "both files came from the preferred host: {:#?}",
        s.hosts
    );
    let local = s.hosts.iter().find(|h| h.name == "localhost").unwrap();
    assert_eq!(
        (local.preferred, local.listed),
        (Some(0), true),
        "listed, being preferred"
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
            .filter(|h| h.preferred.is_some())
            .map(|h| (h.name.as_str(), h.preferred, h.ok))
            .collect::<Vec<_>>(),
        [
            ("pixeldrain.com", Some(0), 0),
            ("vikingfile.com", Some(1), 0),
            ("mega.nz", Some(2), 0)
        ]
    );
    for host in ["pixeldrain.com", "vikingfile.com", "mega.nz"] {
        engine.set_preferred(host, false).unwrap();
    }
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("preferred_hosts = []"), "{text}");
    assert!(Config::parse(&text).unwrap().preferred_hosts.is_empty());
    assert!(
        !engine
            .snapshot()
            .hosts
            .iter()
            .any(|h| h.preferred.is_some()),
        "none preferred any more"
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
        s.hosts
            .into_iter()
            .filter(|h| h.preferred.is_some())
            .map(|h| (h.name, h.preferred))
            .collect()
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
    let engine = t
        .engine_with(Config {
            onefichier_key: Some("key".into()),
            endpoints: Endpoints {
                onefichier: t.ip.clone(),
                ..t.config().endpoints
            },
            ..t.config()
        })
        .await;
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
    let engine = t
        .engine_with(Config {
            verify_existing: true,
            ..t.config()
        })
        .await;
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
    let engine = t
        .engine_with(Config {
            resume_attempts: 1,
            ..t.config()
        })
        .await;
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
    let engine = t
        .engine_with(Config {
            retry_failed_minutes: 0,
            ..t.config()
        })
        .await;
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
    let engine = t
        .engine_with(Config {
            min_speed_kbps: 100,
            min_speed_seconds: 1,
            retry_failed_minutes: 0,
            ..t.config()
        })
        .await;
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
    let engine = t
        .engine_with(Config {
            max_active_gb: (SIZE as f64 * 1.5) / 1e9,
            ..t.config()
        })
        .await;
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
    let engine = t.engine_with(cfg).await;
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
    assert!(s.reconnect.note_at.is_some_and(|t| t > 0));
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
fn assert_example(dir: &std::path::Path) {
    assert_eq!(
        std::fs::metadata(dir.join("video.bin")).unwrap().len(),
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
    let engine = t
        .engine_with(Config {
            auto_extract: true,
            delete_after_extract: true,
            ..t.config()
        })
        .await;
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
    assert_example(&t.root.join("dl/Example"));
    assert_eq!(files_in(&t.root.join("dl")), ["Example"]);
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
    assert_example(&t.root.join("dl/Example"));
    // delete_after_extract is off: the parts stay.
    assert_eq!(
        files_in(&t.root.join("dl")),
        ["Example", "S.7z.001", "S.7z.002", "S.7z.003"]
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
    let engine = t
        .engine_with(Config {
            auto_extract: true,
            ..t.config()
        })
        .await;
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
    assert_eq!(
        a.commands,
        [haul_core::Command::Clear],
        "nothing to extract yet, only to clear"
    );
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
    assert_example(&t.root.join("dl/Example"));
}

#[tokio::test]
async fn a_zip_split_into_z01_pieces_extracts() {
    let t = Setup::new().await;
    let engine = t
        .engine_with(Config {
            auto_extract: true,
            ..t.config()
        })
        .await;
    engine.add_text(&format!("{0}/archive/ZS.z01 {0}/archive/ZS.zip", t.ip), "");
    let s = wait_for(&engine, "extracted", |s| {
        s.items.len() == 2 && s.items.iter().all(|i| i.extract == Extract::Done)
    })
    .await;
    assert_eq!(s.archives[0].name, "ZS.zip");
    assert_eq!(
        std::fs::read_to_string(t.root.join("dl/Example/zlast.txt")).unwrap(),
        "after the split\n"
    );
}

#[tokio::test]
async fn an_encrypted_archive_asks_for_its_password_and_keeps_it() {
    let t = Setup::new().await;
    let engine = t
        .engine_with(Config {
            auto_extract: true,
            archive_passwords: vec!["wrong".into()],
            ..t.config()
        })
        .await;
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
    assert_eq!(
        a.commands,
        [haul_core::Command::Password, haul_core::Command::Clear]
    );

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
    let engine = t
        .engine_with(Config {
            auto_extract: true,
            ..t.config()
        })
        .await;
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
