//! Settings, the settings file, restarts, the queue and the download folder.

use super::*;

/// A partial file that cannot be written is this computer's trouble: no
/// host is blamed and no mirror is used up for it.
#[tokio::test]
async fn a_file_that_cannot_be_written_blames_no_host() {
    let t = Setup::new().await;
    // A file where the partial folder should be.
    std::fs::create_dir_all(t.root.join("dl")).unwrap();
    std::fs::write(t.root.join("dl/.partial"), b"in the way").unwrap();
    let engine = t.engine().await;
    engine.set_paused(true);
    engine.add_text(&format!("{}/files/w.bin\n{}/files/w.bin", t.ip, t.name), "");
    wait_for(&engine, "one row on two hosts", |s| {
        s.items.len() == 1 && s.items[0].mirrors.len() == 2
    })
    .await;
    engine.set_paused(false);
    let s = wait_for(&engine, "the failure", |s| {
        s.items[0].status.as_str() == "error"
    })
    .await;
    assert!(
        s.items[0].error.starts_with("this computer:"),
        "{}",
        s.items[0].error
    );
    let inspect = engine.debug_inspect();
    assert_eq!(
        inspect["items"][0]["failed"],
        json!([]),
        "no mirror used up"
    );
    for h in &s.hosts {
        assert_eq!(h.fails, 0, "{} not blamed", h.name);
    }
}

/// A partial file that cannot move to the new download folder keeps the
/// downloads where they are, says why, and leaves every partial file there.
#[tokio::test]
async fn a_move_that_cannot_finish_keeps_the_old_folder() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.set_paused(true);
    engine.add_text(&format!("{0}/files/a.bin {0}/files/b.bin", t.ip), "");
    let s = wait_for(&engine, "the rows", |s| s.items.len() == 2).await;
    std::fs::create_dir_all(t.root.join("dl/.partial")).unwrap();
    for it in &s.items {
        std::fs::write(t.partial(&it.id), b"bytes").unwrap();
    }
    // Where the new folder's partial folder should be, a file.
    let new = t.root.join("elsewhere");
    std::fs::create_dir_all(&new).unwrap();
    std::fs::write(new.join(".partial"), b"in the way").unwrap();
    let change = json!({ "download_dir": new.display().to_string() });
    engine.update_settings(change.as_object().unwrap()).unwrap();
    let s = wait_for(&engine, "the move to give up", |s| s.config_error.is_some()).await;
    assert!(
        s.config_error
            .as_deref()
            .unwrap()
            .contains("could not move"),
        "{:?}",
        s.config_error
    );
    assert_eq!(s.download_dir, t.root.join("dl").display().to_string());
    for it in &s.items {
        assert!(t.partial(&it.id).exists(), "{} stays", it.name);
    }
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

/// Two settings changed at once both land, in memory and in the file.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn settings_changed_at_once_agree_with_the_file() {
    let t = Setup::new().await;
    let engine = start_from_file(&t).await;
    let a = tokio::task::spawn_blocking({
        let engine = engine.clone();
        move || {
            for i in 0..40 {
                engine.set_disabled("a.example", i % 2 == 0).unwrap();
            }
        }
    });
    let b = tokio::task::spawn_blocking({
        let engine = engine.clone();
        move || {
            for i in 0..40 {
                engine
                    .set_host_folder("b.example", &format!("x{i}"))
                    .unwrap();
            }
        }
    });
    a.await.unwrap();
    b.await.unwrap();
    let file = Config::load(&t.root.join("data/config.toml")).unwrap();
    let cfg = engine.cfg();
    assert_eq!(cfg.disabled_hosts, file.disabled_hosts);
    assert_eq!(cfg.folders, file.folders);
    assert_eq!(*cfg, file);
    assert_eq!(
        cfg.folders.get("b.example").map(String::as_str),
        Some("x39")
    );
    assert!(!cfg.is_disabled("a.example"));
}

/// A queue read whole at start is copied aside, and a damaged one says
/// where that copy is.
#[tokio::test]
async fn a_damaged_queue_points_at_its_copy() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.set_paused(true);
    engine.add_text(&format!("{}/files/a.bin", t.ip), "");
    engine.shutdown().await;
    drop(engine);
    let engine = start_when_free(|| t.options()).await;
    let backup = t.root.join("data/queue.bak.json");
    assert!(
        std::fs::read_to_string(&backup).unwrap().contains("a.bin"),
        "the copy holds the queue"
    );
    engine.shutdown().await;
    drop(engine);
    std::fs::write(t.root.join("data/queue.json"), "{ not json").unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let error = loop {
        match Engine::start(t.options()).await {
            Err(StartError::Locked(_)) if Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(20)).await
            }
            Err(e) => break e.to_string(),
            Ok(_) => panic!("a damaged queue started"),
        }
    };
    assert!(error.contains("queue.bak.json"), "{error}");
    assert!(
        std::fs::read_to_string(&backup).unwrap().contains("a.bin"),
        "a damaged queue is not copied over the good one"
    );
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

/// A paused file with bytes on disk is a started one after a restart too:
/// removing its wrapper keeps it, and removing it asks twice.
#[tokio::test]
async fn bytes_on_disk_count_after_a_restart() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.set_paused(true);
    engine.add_text("https://fileguard.cc/vault/parts", "");
    let s = wrapper_settled(&engine, "done").await;
    let w = s.wrappers[0].id.clone();
    let s = wait_for(&engine, "the parts", |s| {
        s.items.len() == 2 && s.items.iter().all(|i| i.size > 0)
    })
    .await;
    let id = s.items[0].id.clone();
    engine.shutdown().await;
    drop(engine);
    std::fs::create_dir_all(t.partial(&id).parent().unwrap()).unwrap();
    std::fs::write(t.partial(&id), b"some bytes").unwrap();

    let engine = start_when_free(|| t.options()).await;
    let s = engine.snapshot();
    let it = s.items.iter().find(|i| i.id == id).unwrap();
    assert!(!it.not_started, "it has bytes on disk");
    assert_eq!(it.done, 10);
    assert!(
        it.commands
            .contains(&haul_core::engine::Command::RemovePartial),
        "{:?}",
        it.commands
    );
    assert!(engine.remove_wrapper(&w));
    let s = engine.snapshot();
    assert_eq!(
        s.items.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(),
        [id.as_str()],
        "the started file stays"
    );
    assert!(t.partial(&id).exists());
}

/// A partial file no download owns is deleted at start, unless the folder
/// belongs to another Haul's data folder that is still there.
#[tokio::test]
async fn a_partial_file_no_download_owns_is_deleted_at_start() {
    let t = Setup::new().await;
    let partial = t.root.join("dl/.partial");
    std::fs::create_dir_all(&partial).unwrap();
    std::fs::write(partial.join("abc123.part"), b"left over").unwrap();
    let engine = t.engine().await;
    assert!(!partial.join("abc123.part").exists(), "deleted");
    engine.shutdown().await;
    drop(engine);

    // Another Haul's folder: its files stay.
    let other = t.root.join("other-data");
    std::fs::create_dir_all(&other).unwrap();
    std::fs::write(other.join("queue.json"), "{}").unwrap();
    let other = std::fs::canonicalize(&other).unwrap();
    std::fs::write(partial.join("owner"), other.to_string_lossy().as_bytes()).unwrap();
    std::fs::write(partial.join("abc123.part"), b"theirs").unwrap();
    let engine = start_when_free(|| t.options()).await;
    assert!(
        partial.join("abc123.part").exists(),
        "not this Haul's to delete"
    );
    engine.shutdown().await;
    drop(engine);

    // Once that one's queue is gone, the folder is this one's again.
    std::fs::remove_file(other.join("queue.json")).unwrap();
    let _engine = start_when_free(|| t.options()).await;
    assert!(!partial.join("abc123.part").exists());
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
