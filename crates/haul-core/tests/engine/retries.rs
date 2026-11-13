//! Failures, retries, resumes and what a broken answer costs.

use super::*;

/// A download that differs from the checksum its host published is not
/// kept: its bytes go, and that host's link is not tried again for it.
#[tokio::test]
async fn a_download_that_differs_from_its_checksum_is_not_kept() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.add_text("https://pixeldrain.com/u/badhash-x", "");
    let s = wait_for(&engine, "the failure", |s| {
        s.items
            .first()
            .is_some_and(|i| i.status.as_str() == "error")
    })
    .await;
    assert!(
        s.items[0].error.contains("differs from its"),
        "{}",
        s.items[0].error
    );
    assert!(!t.partial(&s.items[0].id).exists(), "its bytes went");
    assert!(!t.root.join("dl/badhash-x.bin").exists(), "nothing placed");
}

/// A resumed download is compared with its checksum once it is whole:
/// bytes from before that are wrong where no overlap check looks are
/// caught, and right ones pass.
#[tokio::test]
async fn a_resumed_download_is_compared_once_whole() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.set_paused(true);
    engine.add_text(
        "https://pixeldrain.com/u/good https://pixeldrain.com/u/spoilt",
        "",
    );
    let s = wait_for(&engine, "the rows", |s| {
        s.items.len() == 2 && s.items.iter().all(|i| i.size > 0)
    })
    .await;
    std::fs::create_dir_all(t.root.join("dl/.partial")).unwrap();
    for it in &s.items {
        let key = it.name.trim_end_matches(".bin");
        let mut bytes = content(key, SIZE)[..SIZE / 2].to_vec();
        if key == "spoilt" {
            // Wrong near the start, far before the bytes an overlap compares.
            bytes[10] ^= 0xff;
        }
        std::fs::write(t.partial(&it.id), bytes).unwrap();
    }
    engine.set_paused(false);
    let s = wait_for(&engine, "both settled", |s| {
        s.items
            .iter()
            .all(|i| matches!(i.status.as_str(), "done" | "error"))
    })
    .await;
    let status = |name: &str| {
        s.items
            .iter()
            .find(|i| i.name == name)
            .unwrap()
            .status
            .as_str()
    };
    assert_eq!(status("good.bin"), "done");
    assert_eq!(t.downloaded("good.bin"), content("good", SIZE));
    assert_eq!(status("spoilt.bin"), "error");
}

#[tokio::test]
async fn resumes_after_a_broken_connection() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.add_text(&format!("{}/flaky/b.bin", t.ip), "");
    let s = wait_for(&engine, "a retry", |s| {
        s.items[0].status.as_str() == "waiting" && !s.items[0].error.is_empty()
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
        s.items
            .first()
            .is_some_and(|i| i.status.as_str() == "waiting")
    })
    .await;
    let host = s.hosts.iter().find(|h| h.name == "127.0.0.1").unwrap();
    assert!(host.cooldown > 0, "shows its wait: {}", host.cooldown);
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
        s.items[0].status.as_str() == "paused" && active(s, "127.0.0.1") == 1
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
            s.items[0].status.as_str(),
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
async fn failed_downloads_retry_on_their_own() {
    let t = Setup::new().await;
    let mut config = t.config();
    config.retry_failed_minutes = 0;
    let engine = t.engine_with(config).await;
    engine.add_text(&format!("{}/missing/o.bin", t.ip), "");
    let s = wait_for(&engine, "failure", |s| {
        s.items[0].status.as_str() == "error"
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

#[tokio::test]
async fn an_empty_folder_fails_instead_of_vanishing() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.add_text("https://pixeldrain.com/l/empty", "");
    let s = wait_for(&engine, "failure", |s| {
        s.items
            .first()
            .is_some_and(|i| i.status.as_str() == "error")
    })
    .await;
    assert!(
        s.items[0].error.contains("nothing to download"),
        "{}",
        s.items[0].error
    );
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
        s.items.iter().all(|i| i.status.as_str() != "checking")
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

/// A download whose every link is gone is not tried again on a timer, after
/// a restart either.
#[tokio::test]
async fn a_gone_download_is_not_retried_after_a_restart() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.set_pages_pass(true);
    engine.add_text(&format!("{}/blog/guarded", t.ip), "");
    let s = wait_for(&engine, "the page passed out of sight", |s| {
        s.items.iter().any(|i| i.status.as_str() == "passing")
    })
    .await;
    let id = s.items[0].id.clone();
    engine.page_gone(&id, "the folder was deleted");
    let s = wait_for(&engine, "the download to fail", |s| {
        s.items.iter().all(|i| i.status.as_str() == "error")
    })
    .await;
    assert_eq!(s.items[0].retry_in, 0, "not tried again on a timer");
    engine.shutdown().await;
    drop(engine);

    let engine = start_when_free(|| t.options()).await;
    engine.set_pages_pass(true);
    let s = engine.snapshot();
    assert_eq!(s.items[0].status.as_str(), "error");
    assert_eq!(s.items[0].retry_in, 0, "nor after a restart");
}

/// A failed download whose name no host gave is looked up on its retry, not
/// left waiting for a lookup that never comes, after a restart.
#[tokio::test]
async fn a_failed_download_without_a_name_retries_after_a_restart() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.add_text(&format!("{}/down/", t.ip), "");
    let s = wait_for(&engine, "the failure", |s| {
        s.items.iter().any(|i| i.status.as_str() == "error")
    })
    .await;
    let id = s.items[0].id.clone();
    engine.shutdown().await;
    drop(engine);

    let engine = start_when_free(|| t.options()).await;
    assert!(engine.debug_retry_now(&id));
    wait_for(&engine, "the retry to fail again", |s| {
        auto_retries(&engine, &id) == 1 && s.items[0].status.as_str() == "error"
    })
    .await;
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
        s.items.len() == 3 && s.items.iter().all(|i| i.status.as_str() == "queued")
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
            .any(|i| i.status.as_str() == "downloading" || i.status.as_str() == "resolving")
    })
    .await;
    let started: Vec<&str> = s
        .items
        .iter()
        .filter(|i| i.status.as_str() == "downloading" || i.status.as_str() == "resolving")
        .map(|i| i.name.as_str())
        .collect();
    assert_eq!(started, ["most.bin"]);
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
async fn a_refusal_says_why() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.add_text(&format!("{}/refuse/r.bin", t.ip), "");
    let s = wait_for(&engine, "the failure", |s| {
        s.items[0].status.as_str() == "error"
    })
    .await;
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
            && s.items
                .iter()
                .filter(|i| i.status.as_str() == "error")
                .count()
                == 2
            && s.items.iter().any(|i| i.status.as_str() == "done")
    })
    .await;
    engine.clear_failed();
    let s = engine.snapshot();
    assert_eq!(
        s.items
            .iter()
            .map(|i| i.status.as_str())
            .collect::<Vec<_>>(),
        ["done"],
        "only the finished download is left"
    );
}

/// Going on from another host keeps the bytes when that host's agree: it asks
/// for 64 KiB more than it needs and adds only what is new.
#[tokio::test]
async fn another_hosts_copy_resumes_when_its_bytes_agree() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.set_preferred("127.0.0.1", true).unwrap();
    engine.add_text(
        &format!("{}/paced/400/x.bin {}/files/x.bin", t.ip, t.name),
        "",
    );
    let s = wait_for(&engine, "enough of it", |s| {
        row(s, "x.bin").done > 100 * 1024
    })
    .await;
    let id = row(&s, "x.bin").id.clone();
    assert!(engine.item_action(&id, ItemAction::Pause));
    wait_for(&engine, "the pause", |s| active(s, "127.0.0.1") == 0).await;
    let kept = std::fs::metadata(t.partial(&id)).unwrap().len();
    engine.download_from(std::slice::from_ref(&id), Some("localhost"));
    assert!(engine.item_action(&id, ItemAction::Resume));
    wait_for(&engine, "done", all_done).await;
    assert_eq!(t.downloaded("x.bin"), content("x.bin", SIZE));
    let want = format!("/files/x.bin bytes={}-", kept - 64 * 1024);
    assert_eq!(asked_for_bytes(&t, "/files/x.bin"), [want]);
}

/// A link that has run out says nothing about the partial file: it stays, and
/// the row goes on from another host.
#[tokio::test]
async fn an_expired_link_does_not_cost_the_partial_file() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.set_preferred("127.0.0.1", true).unwrap();
    engine.add_text(
        &format!("{}/paced/400/x.bin {}/expired/x.bin", t.ip, t.name),
        "",
    );
    let s = wait_for(&engine, "enough of it", |s| {
        row(s, "x.bin").done > 100 * 1024
    })
    .await;
    let id = row(&s, "x.bin").id.clone();
    assert!(engine.item_action(&id, ItemAction::Pause));
    wait_for(&engine, "the pause", |s| active(s, "127.0.0.1") == 0).await;
    engine.download_from(std::slice::from_ref(&id), Some("localhost"));
    assert!(engine.item_action(&id, ItemAction::Resume));
    wait_for(&engine, "the expired link's answer", |_| {
        !asked_for_bytes(&t, "/expired/x.bin").is_empty()
    })
    .await;
    wait_for(&engine, "done from the other host", all_done).await;
    assert_eq!(t.downloaded("x.bin"), content("x.bin", SIZE));
    let asked = t.server.requests("/paced/400/x.bin");
    let last = asked.last().unwrap();
    assert!(last.contains("bytes="), "went on from its bytes: {asked:?}");
}

/// An answer that ends early without an error, with no length of its own,
/// is not the whole file when its host said how big the file is.
#[tokio::test]
async fn a_chunked_answer_that_ends_short_is_not_the_file() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.add_text(&format!("{}/short/s.bin", t.ip), "");
    wait_for(&engine, "done", all_done).await;
    assert_eq!(t.downloaded("s.bin"), content("s.bin", SIZE));
    assert!(t.server.requests("/short/s.bin").len() >= 2, "asked again");
}

/// A bare 416 to a resume of a file whose size nobody said is a link that
/// ran out, not a finished file.
#[tokio::test]
async fn a_bare_416_does_not_finish_a_file_of_unknown_size() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.add_text(&format!("{}/unsized-expiring/u.bin", t.ip), "");
    let s = wait_for(&engine, "the link to give out", |s| {
        s.items[0].status.as_str() == "error" || s.items[0].status.as_str() == "done"
    })
    .await;
    assert_eq!(s.items[0].status.as_str(), "error", "not finished");
    assert!(
        s.items[0].error.contains("answers nothing past"),
        "{}",
        s.items[0].error
    );
    assert!(t.partial(&s.items[0].id).exists(), "its bytes stay");
}

/// An answer that ends before any byte was compared proves nothing either.
#[tokio::test]
async fn an_answer_cut_short_does_not_cost_the_partial_file() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.set_preferred("127.0.0.1", true).unwrap();
    engine.add_text(
        &format!("{}/paced/400/x.bin {}/cut/x.bin", t.ip, t.name),
        "",
    );
    let s = wait_for(&engine, "enough of it", |s| {
        row(s, "x.bin").done > 100 * 1024
    })
    .await;
    let id = row(&s, "x.bin").id.clone();
    assert!(engine.item_action(&id, ItemAction::Pause));
    wait_for(&engine, "the pause", |s| active(s, "127.0.0.1") == 0).await;
    let kept = std::fs::metadata(t.partial(&id)).unwrap().len();
    engine.download_from(std::slice::from_ref(&id), Some("localhost"));
    assert!(engine.item_action(&id, ItemAction::Resume));
    wait_for(&engine, "the short answer", |_| {
        !t.server.requests("/cut/x.bin").is_empty()
    })
    .await;
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert!(
        std::fs::metadata(t.partial(&id))
            .map(|m| m.len())
            .unwrap_or(0)
            >= kept,
        "the partial file was thrown away"
    );
}
