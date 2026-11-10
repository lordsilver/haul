//! Mirrors: which host a file comes from, and rows for the same file.

use super::*;

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

/// A file on several hosts downloads from the one the user picked, even
/// over the hosts they prefer in general.
#[tokio::test]
async fn a_file_downloads_from_the_host_picked_for_it() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.set_paused(true);
    // pixeldrain.com, preferred from the start, and a direct link: one file.
    // (pixeldrain publishes no checksum for it here: the stand-ins hold
    // other bytes under one name.)
    engine.add_text(
        &format!(
            "https://pixeldrain.com/u/nohash-pick\n{}/files/nohash-pick.bin",
            t.ip
        ),
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
        !t.server.requests("/files/nohash-pick.bin").is_empty(),
        "the picked host served it"
    );
    // pixeldrain was only asked what the file is, never for it.
    assert!(
        t.server
            .requests("/api/file/nohash-pick")
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

#[tokio::test]
async fn probe_merges_links_that_hide_the_name() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.set_paused(true);
    let added = engine.add_text(&format!("{}/hidden/one {}/hidden/two", t.ip, t.name), "");
    assert_eq!(added.added, 2, "nothing to merge on before probing");
    let s = wait_for(&engine, "probes", |s| {
        s.items.iter().all(|i| i.status.as_str() != "checking")
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

/// A host that gives no decent download without its account can be used
/// like any other without one, from the app; the choice is saved.
#[tokio::test]
async fn a_host_that_wants_its_account_can_be_used_without_one() {
    let t = Setup::new().await;
    let engine = start_from_file(&t).await;
    let host = |engine: &Engine| {
        let s = engine.snapshot();
        let h = s.hosts.iter().find(|h| h.name == "rapidgator.net").unwrap();
        (h.accountless, h.needs_account)
    };
    assert_eq!(host(&engine), (Some(false), true));
    engine.set_accountless("rapidgator.net", true).unwrap();
    assert_eq!(host(&engine), (Some(true), false));
    let file = Config::load(&t.root.join("data/config.toml")).unwrap();
    assert_eq!(file.accountless_hosts, ["rapidgator.net"]);
    assert!(
        engine.set_accountless("127.0.0.1", true).is_err(),
        "a host that needs no account"
    );
    let s = engine.snapshot();
    assert_eq!(
        s.hosts
            .iter()
            .find(|h| h.name == "127.0.0.1")
            .map(|h| h.accountless),
        None,
        "not reached yet"
    );
}

/// A host that gives no decent download without an account (rapidgator: a
/// wait and a captcha for every file) is off until its account is set: its
/// links are left out. Switched on without one, the file asks the person
/// for its page; switched off, it is back to that default; with the
/// account, it downloads through the host's API.
/// A host without its account goes only when no other host has the file:
/// beside one, it is never asked; alone, its page is the way; with its
/// account, its API.
#[tokio::test]
async fn a_host_without_its_account_goes_last() {
    let t = Setup::new().await;
    let mut cfg = t.config();
    cfg.endpoints.rapidgator = t.ip.clone();
    let engine = t.engine_with(cfg).await;
    let last = |s: &Snapshot| {
        let h = s.hosts.iter().find(|h| h.name == "rapidgator.net").unwrap();
        assert!(!h.disabled);
        h.needs_account
    };
    engine.add_text(
        &format!(
            "https://rapidgator.net/file/abc/x.rar.html {}/files/x.rar",
            t.ip
        ),
        "",
    );
    let s = wait_for(&engine, "the file from the other host", all_done).await;
    assert!(last(&s));
    assert_eq!(t.downloaded("x.rar"), content("x.rar", SIZE));
    assert!(
        t.server.requests("/api/v2").is_empty(),
        "rapidgator.net never asked"
    );

    engine.add_text("https://rapidgator.net/file/def/y.rar.html", "");
    let s = wait_for(&engine, "the file to ask for its page", |s| {
        s.items.iter().any(|i| i.status.as_str() == "captcha")
    })
    .await;
    let y = s
        .items
        .iter()
        .find(|i| i.status.as_str() == "captcha")
        .unwrap();
    assert!(
        y.error.contains("a free download is on its page"),
        "{}",
        y.error
    );
    let id = y.id.clone();

    let account = json!({"rapidgator_user": "u", "rapidgator_pass": "p"});
    engine
        .update_settings(account.as_object().unwrap())
        .unwrap();
    assert!(!last(&engine.snapshot()));
    engine.item_action(&id, ItemAction::RetryManual);
    // Through the API now, which checks the link (this one names no file).
    wait_for(&engine, "the file tried with the account", |s| {
        s.items
            .iter()
            .any(|i| i.id == id && i.error.contains("not a file link"))
    })
    .await;
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
            ("mega.nz", Some(2), 0),
            ("megaup.net", Some(3), 0)
        ]
    );
    for host in ["pixeldrain.com", "vikingfile.com", "mega.nz", "megaup.net"] {
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
        s.items.iter().any(|i| i.status.as_str() == "error")
    })
    .await;
    let failed = s
        .items
        .iter()
        .find(|i| i.status.as_str() == "error")
        .unwrap();
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

/// One throttled host stops nothing else: the healthy host's transfer runs to
/// its end in one go, and a new row still starts on that host.
#[tokio::test]
async fn a_healthy_host_is_unaffected() {
    let t = Setup::new().await;
    let engine = t.engine_with(quick(&t)).await;
    engine.add_text(&format!("{}/paced/40/x.bin", t.ip), "");
    engine.add_text(&format!("{}/paced/200/h.bin", t.name), "");
    wait_for(&engine, "the throttled row", |s| {
        row(s, "x.bin").slowed.is_some()
    })
    .await;
    engine.add_text(&format!("{}/files/new.bin", t.name), "");
    wait_for(&engine, "the healthy ones done", |s| {
        row(s, "h.bin").status == "done" && row(s, "new.bin").status == "done"
    })
    .await;
    assert_eq!(
        t.server.requests("/paced/200/h.bin").len(),
        1,
        "never stopped"
    );
    assert_eq!(t.downloaded("h.bin"), content("h.bin", SIZE));
    let s = engine.snapshot();
    assert_eq!(row(&s, "h.bin").slowed, None);
    assert_eq!(cooldown(&s, "localhost"), 0);
    assert!(cooldown(&s, "127.0.0.1") > 0);
}

/// With no mirror a throttled download keeps going, saying why, and a new row
/// only that host has starts all the same: there is nowhere else to go.
#[tokio::test]
async fn with_no_mirror_it_keeps_going_and_says_why() {
    let t = Setup::new().await;
    let engine = t.engine_with(quick(&t)).await;
    engine.add_text(&format!("{}/paced/80/x.bin", t.ip), "");
    engine.add_text(&format!("{}/paced/200/h.bin", t.name), "");
    let s = wait_for(&engine, "the row to say so", |s| {
        row(s, "x.bin").slowed.is_some()
    })
    .await;
    assert_eq!(
        row(&s, "x.bin").slowed.as_deref(),
        Some("Slowed by 127.0.0.1: no other host has it")
    );
    engine.add_text(&format!("{}/files/new.bin", t.ip), "");
    wait_for(&engine, "the new row to finish", |s| {
        row(s, "new.bin").status == "done"
    })
    .await;
    assert_eq!(row(&engine.snapshot(), "x.bin").status, "downloading");
    wait_for(&engine, "the slow one done", |s| {
        row(s, "x.bin").status == "done"
    })
    .await;
    assert_eq!(t.downloaded("x.bin"), content("x.bin", SIZE));
    assert_eq!(
        t.server.requests("/paced/80/x.bin").len(),
        1,
        "never stopped"
    );
}

/// A host that lifts its limit gets its cooldown ended and the row its plain
/// look back.
#[tokio::test]
async fn a_transfer_that_recovers_ends_its_hosts_cooldown() {
    let t = Setup::new().await;
    let engine = t.engine_with(quick(&t)).await;
    engine.add_text(&format!("{}/eases/x.bin", t.ip), "");
    engine.add_text(&format!("{}/paced/200/h.bin", t.name), "");
    let s = wait_for(&engine, "the row to say so", |s| {
        row(s, "x.bin").slowed.is_some()
    })
    .await;
    assert!(cooldown(&s, "127.0.0.1") > 0);
    t.server.eased.store(true, Ordering::SeqCst);
    wait_for(&engine, "it to recover", |s| {
        row(s, "x.bin").slowed.is_none() && cooldown(s, "127.0.0.1") == 0
    })
    .await;
    wait_for(&engine, "both done", all_done).await;
    assert_eq!(t.server.requests("/eases/x.bin").len(), 1, "never stopped");
}

/// Picking another host for a download that is running moves it there at
/// once, keeping what it has, rather than when it next starts.
#[tokio::test]
async fn picking_a_host_moves_a_running_download_there() {
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
    assert_eq!(row(&s, "x.bin").host, "127.0.0.1");
    engine.download_from(std::slice::from_ref(&id), Some("localhost"));
    wait_for(&engine, "done", all_done).await;
    assert_eq!(t.downloaded("x.bin"), content("x.bin", SIZE));
    let asked = asked_for_bytes(&t, "/files/x.bin");
    assert_eq!(asked.len(), 1, "{asked:?}");
    assert!(asked[0].contains("bytes="), "resumed: {asked:?}");
}

/// A copy by the same name and size that holds other bytes doesn't mix with
/// the partial file: those bytes go, and the download starts over from it.
#[tokio::test]
async fn another_hosts_other_bytes_start_it_over() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.set_preferred("127.0.0.1", true).unwrap();
    engine.add_text(
        &format!("{}/paced/400/x.bin {}/other/x.bin", t.ip, t.name),
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
    wait_for(&engine, "done", all_done).await;
    assert_eq!(t.downloaded("x.bin"), content("other x.bin", SIZE));
    let asked = t.server.requests("/other/x.bin");
    assert_eq!(asked.len(), 2, "{asked:?}");
    assert!(asked[0].contains("bytes="), "{asked:?}");
    assert!(!asked[1].contains("bytes="), "{asked:?}");
}

/// A hosts file saved before `normal` existed loads with what it holds, and one
/// that can't be read costs only the saved numbers.
#[tokio::test]
async fn an_older_or_damaged_hosts_file_does_not_stop_haul() {
    let t = Setup::new().await;
    let data = t.root.join("data");
    std::fs::create_dir_all(&data).unwrap();
    let hosts = data.join("hosts.json");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let older = format!(
        r#"{{"site.example":{{"speed":5000000,"measured":{now},"ok":3,"fails":1,"bytes":9}}}}"#
    );
    std::fs::write(&hosts, older).unwrap();
    let engine = t.engine().await;
    let s = engine.snapshot();
    let host = s.hosts.iter().find(|h| h.name == "site.example").unwrap();
    assert_eq!((host.ok, host.fails, host.bytes), (3, 1, 9));
    assert_eq!(host.speed, 5_000_000.0);
    let seen = engine.debug_inspect();
    let seen = seen["hosts"].as_array().unwrap();
    let seen = seen.iter().find(|h| h["name"] == "site.example").unwrap();
    assert_eq!(seen["normal"], 0.0);
    engine.shutdown().await;
    drop(engine);

    std::fs::write(&hosts, "not json").unwrap();
    let engine = start_when_free(|| t.options()).await;
    let s = engine.snapshot();
    assert!(s.hosts.iter().all(|h| h.name != "site.example"));
}
