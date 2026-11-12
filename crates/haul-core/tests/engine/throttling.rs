//! Hosts that slow an address, the line, redials, proxies and the pool.

use super::*;

/// With a proxy set, the update check goes through it as any host does:
/// GitHub here is reachable only through the stand-in proxy.
#[tokio::test]
async fn the_update_check_goes_through_the_proxy() {
    let t = Setup::new().await;
    let mut cfg = t.config();
    cfg.proxies = vec![t.ip.clone()];
    cfg.endpoints.github_api = "http://api.example".into();
    cfg.check_updates = false;
    let engine = t.engine_with(cfg).await;
    assert_eq!(
        engine.look_for_update().await,
        Ok(Some("99.0.0".to_string())),
    );
    let asked = t.server.requests("/repos/lordsilver/haul/releases/latest");
    assert_eq!(asked.len(), 1, "{asked:?}");
}

#[tokio::test]
async fn rate_limited_host_cools_down() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.add_text(&format!("{}/limited/d.bin", t.ip), "");
    let s = wait_for(&engine, "cooldown", |s| {
        s.items[0].status.as_str() == "waiting"
    })
    .await;
    assert!(s.items[0].error.contains("429"), "{}", s.items[0].error);
    wait_for(&engine, "done", all_done).await;
    assert_eq!(t.downloaded("d.bin"), content("d.bin", SIZE));
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
        s.items[0].status.as_str() == "waiting"
    })
    .await;
    assert!(s.items[0].wait <= 2, "as long as the page said");
    assert_eq!(
        s.items[0].list.map(|l| l.as_str()),
        Some("active"),
        "not under Needs you"
    );
    assert_eq!(s.counts.need_you, 0);
    assert!(!engine.page_passes(page), "no pass while it holds");

    let s = wait_for(&engine, "the row to go again", |s| {
        s.items[0].status.as_str() == "passing"
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
    assert!(!engine.page_wants_person(&wants), "its own tries first");
    wait_for(&engine, "the row under Needs you", |s| {
        s.counts.need_you == 1
    })
    .await;

    engine.page_later(&id, page, "ddownload: limit", Some(3600));
    let s = wait_for(&engine, "every row to wait", |s| {
        s.items.iter().all(|i| i.status.as_str() == "waiting")
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
        s.items.len() == 4 && s.items.iter().all(|i| i.status.as_str() != "checking")
    })
    .await;
    tokio::time::sleep(Duration::from_millis(600)).await;
    let s = engine.snapshot();
    assert!(
        !s.items
            .iter()
            .any(|i| matches!(i.status.as_str(), "passing" | "captcha")),
        "{:?}",
        s.items
            .iter()
            .map(|i| i.status.as_str())
            .collect::<Vec<_>>()
    );
    assert_eq!(s.counts.need_you, 0);
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
        s.items.iter().any(|i| i.status.as_str() == "passing")
    })
    .await;
    engine.page_later(&s.items[0].id, &page, "a limit", Some(3600));
    wait_for(&engine, "the row to wait", |s| {
        s.items[0].status.as_str() == "waiting"
    })
    .await;
    engine.find_router().await;
    engine.reconnect("a test").await.unwrap();
    wait_for(&engine, "the row to go again", |s| {
        s.items[0].status.as_str() == "passing"
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
        s.items[0].status.as_str() == "waiting"
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
        s.items[0].status.as_str() == "passing"
    })
    .await;
    assert!(t.server.requests("/files/held.bin").is_empty());

    let page = "http://ddownload.com/dd/ddownload-held";
    engine.page_later(&id, page, "ddownload: limit", Some(3600));
    wait_for(&engine, "the other mirror", all_done).await;
    assert!(!t.server.requests("/files/held.bin").is_empty());
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
            .any(|i| i.status.as_str() == "waiting" && i.error.contains("at a time")),
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
            .any(|i| i.name == "u.bin" && i.status.as_str() == "error")
    })
    .await;
    let error = &s.items.iter().find(|i| i.name == "u.bin").unwrap().error;
    assert!(error.contains("slower than 100 KiB/s for 1s"), "{error}");
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
        let downloading = s
            .items
            .iter()
            .filter(|i| i.status.as_str() == "downloading")
            .count();
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
        s.items.len() == 2 && s.items.iter().all(|i| i.status.as_str() == "queued")
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

/// A proxy taken out of the pool while a download runs through another
/// leaves that download's lane as it was: still counted, under the name
/// its proxy now has.
#[tokio::test]
async fn a_pool_change_keeps_the_lanes_of_running_downloads() {
    let t = Setup::new().await;
    let (_second, port) = start_server().await;
    let other = format!("http://127.0.0.1:{port}");
    let mut cfg = t.config();
    cfg.proxies = vec![t.ip.clone(), other.clone()];
    cfg.max_total = 1;
    cfg.rate_limit_mbps = 0.05;
    let engine = t.engine_with(cfg.clone()).await;
    engine.add_text("http://files.example/files/a.bin", "");
    wait_for(&engine, "the download", |s| {
        s.items.iter().any(|i| i.status.as_str() == "downloading")
    })
    .await;
    let exit = engine.debug_inspect()["items"][0]["exit"]
        .as_str()
        .unwrap()
        .to_string();
    // The proxy it does not run through goes.
    let (kept, gone) = if exit == "proxy 1" {
        (t.ip.clone(), other)
    } else {
        (other, t.ip.clone())
    };
    let change = json!({ "proxies": [kept] });
    engine.update_settings(change.as_object().unwrap()).unwrap();
    let inspect = engine.debug_inspect();
    assert_eq!(
        inspect["items"][0]["exit"], "proxy 1",
        "{gone} left the pool"
    );
    let lanes: Vec<&serde_json::Value> = inspect["lanes"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|l| l["host"] == "files.example")
        .collect();
    assert!(
        lanes
            .iter()
            .any(|l| l["exit"] == "proxy 1" && l["active"] == 1),
        "{lanes:?}"
    );
    assert_eq!(active(&engine.snapshot(), "files.example"), 1);
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
    assert!(s.reconnect.note.unwrap().contains("Got a new IP"));
}

/// A redial whose caller goes away halfway (a request whose page reloaded)
/// still ends, and lets the line and the queue go again.
#[tokio::test]
async fn a_redial_whose_caller_goes_away_lets_the_line_go() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.find_router().await;
    let asked = tokio::spawn({
        let engine = engine.clone();
        async move { engine.reconnect("a test").await }
    });
    wait_for(&engine, "the redial to start", |s| s.reconnect.busy).await;
    asked.abort();
    wait_for(&engine, "the line let go", |s| !s.reconnect.busy).await;
    assert_eq!(engine.debug_inspect()["reconnecting"], false);
    engine.add_text(&format!("{}/files/a.bin", t.ip), "");
    wait_for(&engine, "a download after it", all_done).await;
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
    assert!(s.reconnect.note.unwrap().contains("No new IP"));
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
        cfg.accounts.insert("nordvpn_user".into(), "svc".into());
        cfg.accounts
            .insert("nordvpn_password".into(), password.into());
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

/// The report holds no link's key, in any of its files, nor the redial
/// command, which often holds the router's login.
#[tokio::test]
async fn the_report_holds_no_keys_and_no_command() {
    let t = Setup::new().await;
    std::fs::create_dir_all(t.root.join("data")).unwrap();
    // A log written before the log stripped keys itself.
    std::fs::write(
        haul_core::logging::path(&t.root.join("data")),
        "{\"message\":\"adding https://mega.nz/file/abc#AAAAAAAA\"}\nplain https://mega.nz/file/abc#AAAAAAAA\n",
    )
    .unwrap();
    let engine = t.engine().await;
    let command = json!({"reconnect_command": "router-cli --password hunter2"});
    engine
        .update_settings(command.as_object().unwrap())
        .unwrap();
    let (_, bytes) = engine.report_zip(serde_json::Value::Null).unwrap();
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
    let mut names = Vec::new();
    for i in 0..zip.len() {
        let mut member = zip.by_index(i).unwrap();
        let name = member.name().unwrap().to_string();
        let mut text = String::new();
        std::io::Read::read_to_string(&mut member, &mut text).unwrap();
        assert!(!text.contains("AAAAAAAA"), "{name}: {text}");
        assert!(!text.contains("hunter2"), "{name}: {text}");
        names.push(name);
    }
    assert!(names.contains(&"haul.log".to_string()), "{names:?}");
    let report = engine.diagnostics(serde_json::Value::Null);
    assert_eq!(report["settings"]["reconnect_command"], "(set)");
}

/// The heart of it: a big file crawling beside a host that serves well goes on
/// from its fast mirror, and its host goes on cooldown.
#[tokio::test]
async fn a_throttled_transfer_goes_on_from_a_fast_mirror() {
    let t = Setup::new().await;
    let engine = t.engine_with(quick(&t)).await;
    engine.set_preferred("127.0.0.1", true).unwrap();
    engine.add_text(
        &format!("{}/paced/40/x.bin {}/files/x.bin", t.ip, t.name),
        "",
    );
    engine.add_text(&format!("{}/paced/200/h.bin", t.name), "");
    let s = wait_for(&engine, "both done", all_done).await;
    assert_eq!(t.downloaded("x.bin"), content("x.bin", SIZE));
    assert_eq!(t.downloaded("h.bin"), content("h.bin", SIZE));
    assert_eq!(t.server.requests("/paced/40/x.bin").len(), 1);
    assert_eq!(asked_for_bytes(&t, "/files/x.bin").len(), 1);
    assert_eq!(row(&s, "x.bin").shown_host, "localhost");
    let wait = cooldown(&s, "127.0.0.1");
    assert!(wait > 1700, "the Hosts page shows the wait: {wait}");
}

/// The user's own speed limit explains a slow transfer: nothing moves.
#[tokio::test]
async fn the_users_own_speed_limit_is_not_a_throttle() {
    let t = Setup::new().await;
    let mut cfg = Config {
        rate_limit_mbps: 0.05,
        ..quick_to_judge(&t)
    };
    // A limited transfer comes in bursts, and Windows' coarse timers make
    // them bigger: windows this short could read one as a stall.
    cfg.throttling.window = Duration::from_secs(2);
    cfg.throttling.compare_over = Duration::from_millis(600);
    let engine = t.engine_with(cfg).await;
    engine.set_preferred("127.0.0.1", true).unwrap();
    engine.add_text(
        &format!("{}/paced/100/x.bin {}/files/x.bin", t.ip, t.name),
        "",
    );
    wait_for(&engine, "progress", |s| row(s, "x.bin").done > 0).await;
    tokio::time::sleep(Duration::from_secs(5)).await;
    let s = engine.snapshot();
    assert_eq!(
        row(&s, "x.bin").shown_host,
        "127.0.0.1",
        "{:#}",
        engine.debug_inspect()["engine"]["items"]
    );
    assert!(t.server.requests("/files/x.bin").is_empty());
    assert_eq!(row(&s, "x.bin").slowed, None);
    assert_eq!(cooldown(&s, "127.0.0.1"), 0);
}

/// Transfers from other hosts slow at the same moment point at the line:
/// neither host is blamed.
#[tokio::test]
async fn other_hosts_slow_too_point_at_the_line() {
    let t = Setup::new().await;
    let engine = t.engine_with(quick_to_judge(&t)).await;
    engine.set_preferred("127.0.0.1", true).unwrap();
    engine.add_text(
        &format!("{}/paced/40/x.bin {}/files/x.bin", t.ip, t.name),
        "",
    );
    engine.add_text(&format!("{}/paced/40/y.bin", t.name), "");
    a_few_windows().await;
    let s = engine.snapshot();
    assert_eq!(row(&s, "x.bin").shown_host, "127.0.0.1");
    assert!(t.server.requests("/files/x.bin").is_empty());
    assert_eq!(
        (
            row(&s, "x.bin").slowed.clone(),
            row(&s, "y.bin").slowed.clone()
        ),
        (None, None)
    );
    assert_eq!(cooldown(&s, "127.0.0.1") + cooldown(&s, "localhost"), 0);
}

/// A row that left a slow mirror doesn't go back while that one cools down,
/// even when the next is slow as well.
#[tokio::test]
async fn a_row_does_not_bounce_between_slow_mirrors() {
    let t = Setup::new().await;
    let engine = t.engine_with(quick_to_judge(&t)).await;
    engine.set_preferred("127.0.0.1", true).unwrap();
    engine.add_text(
        &format!("{}/paced/40/x.bin {}/paced/41/x.bin", t.ip, t.name),
        "",
    );
    engine.add_text(&format!("{}/paced/200/h.bin", t.name), "");
    wait_for(&engine, "the move", |s| {
        row(s, "x.bin").shown_host == "localhost"
    })
    .await;
    tokio::time::sleep(Duration::from_millis(3000)).await;
    let s = engine.snapshot();
    assert_eq!(row(&s, "x.bin").shown_host, "localhost");
    assert_eq!(t.server.requests("/paced/40/x.bin").len(), 1);
    assert_eq!(t.server.requests("/paced/41/x.bin").len(), 1);
    assert!(row(&s, "x.bin").slowed.is_some(), "it stays, and says so");
}

/// A host tried for the first time can be far slower than the one the row
/// left for slowness: the row goes back to the faster one, its cooldown or
/// not, rather than crawling on.
#[tokio::test]
async fn a_row_goes_back_from_a_host_far_slower_than_the_one_it_left() {
    let t = Setup::new().await;
    let engine = t.engine_with(quick_to_judge(&t)).await;
    engine.set_preferred("127.0.0.1", true).unwrap();
    engine.add_text(
        &format!("{}/paced/60/x.bin {}/paced/10/x.bin", t.ip, t.name),
        "",
    );
    engine.add_text(&format!("{}/paced/200/h.bin", t.name), "");
    wait_for(&engine, "the move to the unmeasured host", |s| {
        row(s, "x.bin").shown_host == "localhost"
    })
    .await;
    wait_for(&engine, "the way back", |s| {
        row(s, "x.bin").shown_host == "127.0.0.1"
    })
    .await;
    wait_for(&engine, "done", |s| row(s, "x.bin").status == "done").await;
    assert_eq!(t.downloaded("x.bin"), content("x.bin", SIZE));
    assert_eq!(t.server.requests("/paced/10/x.bin").len(), 1, "not again");
}

/// The order the user asked for: with no mirror and redials on, new downloads
/// stop starting, the healthy one finishes untouched, and only then the line
/// redials; the slowed row resumes and everything goes again.
#[tokio::test]
async fn a_new_address_waits_for_healthy_downloads_to_finish() {
    let t = Setup::new().await;
    let engine = t
        .engine_with(Config {
            reconnect: true,
            ..quick(&t)
        })
        .await;
    engine.add_text(&format!("{}/eases/x.bin", t.ip), "");
    engine.add_text(&format!("{}/paced/200/h.bin", t.name), "");
    let s = wait_for(&engine, "the hold", |s| s.reconnect.draining.is_some()).await;
    assert_eq!(
        row(&s, "x.bin").slowed.as_deref(),
        Some("Slowed by 127.0.0.1: a new IP once other downloads finish")
    );
    assert_eq!(t.server.redials.load(Ordering::SeqCst), 0);
    assert_eq!(row(&s, "h.bin").status, "downloading");
    engine.add_text(&format!("{}/files/new.bin", t.name), "");
    wait_for(&engine, "everything done", all_done).await;
    let log = |line: &str| t.server.at(line).unwrap_or_else(|| panic!("no {line}"));
    let (finished, redial, started) = (
        log("done /paced/200/h.bin"),
        log("/redial"),
        log("/files/new.bin"),
    );
    assert!(
        finished < redial && redial < started,
        "{finished} {redial} {started}"
    );
    assert_eq!(
        t.server.requests("/paced/200/h.bin").len(),
        1,
        "never stopped"
    );
    assert_eq!(t.server.redials.load(Ordering::SeqCst), 1);
    let resumed = t.server.requests("/eases/x.bin");
    assert_eq!(resumed.len(), 2, "{resumed:?}");
    assert!(resumed[1].contains("bytes="), "{resumed:?}");
    assert_eq!(t.downloaded("x.bin"), content("x.bin", SIZE));
    assert_eq!(t.downloaded("h.bin"), content("h.bin", SIZE));
    assert_eq!(engine.snapshot().reconnect.draining, None);
}

/// Once the queue is held for a new address, the hold stands while the host
/// still slows the address: the healthy download beside the slowed one dipping
/// for a moment reads as a slow line, and the slowed row stops saying it is
/// slowed until it passes, but nothing new starts meanwhile.
#[tokio::test]
async fn a_hold_for_a_new_address_outlasts_a_dip_beside_it() {
    let t = Setup::new().await;
    let engine = t
        .engine_with(Config {
            reconnect: true,
            ..quick(&t)
        })
        .await;
    engine.add_text(&format!("{}/eases/x.bin", t.ip), "");
    engine.add_text(&format!("{}/dips/200/h.bin", t.name), "");
    wait_for(&engine, "the hold", |s| s.reconnect.draining.is_some()).await;
    t.server.dipped.store(true, Ordering::SeqCst);
    wait_for(&engine, "the dip read as the line", |s| {
        row(s, "x.bin").slowed.is_none()
    })
    .await;
    engine.add_text(&format!("{}/files/new.bin", t.name), "");
    a_few_windows().await;
    let s = engine.snapshot();
    assert!(s.reconnect.draining.is_some(), "the hold stands");
    assert_eq!(row(&s, "new.bin").status, "queued");
    assert!(t.server.requests("/files/new.bin").is_empty());
    t.server.dipped.store(false, Ordering::SeqCst);
    wait_for(&engine, "everything done", all_done).await;
    let log = |line: &str| t.server.at(line).unwrap_or_else(|| panic!("no {line}"));
    let (finished, redial, started) = (
        log("done /dips/200/h.bin"),
        log("/redial"),
        log("/files/new.bin"),
    );
    assert!(
        finished < redial && redial < started,
        "{finished} {redial} {started}"
    );
    assert_eq!(
        t.server.requests("/dips/200/h.bin").len(),
        1,
        "never stopped"
    );
    assert_eq!(t.server.redials.load(Ordering::SeqCst), 1);
    assert_eq!(t.downloaded("x.bin"), content("x.bin", SIZE));
    assert_eq!(t.downloaded("h.bin"), content("h.bin", SIZE));
}

/// Holding the queue has a limit: past it the redial is put off, nothing is
/// aborted, and what was held back starts.
#[tokio::test]
async fn holding_for_a_new_address_has_a_limit() {
    let t = Setup::new().await;
    let mut cfg = quick(&t);
    cfg.reconnect = true;
    cfg.throttling.drain_limit = Duration::from_millis(500);
    let engine = t.engine_with(cfg).await;
    engine.add_text(&format!("{}/eases/x.bin", t.ip), "");
    // Its size unknown, so nothing says it won't make it.
    engine.add_text(&format!("{}/unsized/200/h.bin", t.name), "");
    wait_for(&engine, "the hold", |s| s.reconnect.draining.is_some()).await;
    engine.add_text(&format!("{}/files/new.bin", t.name), "");
    // A hold can also end by itself and come up again, when a moment's reading
    // no longer points at the host: what counts is how it ends at the limit.
    let s = wait_for(&engine, "the limit", |s| {
        s.reconnect
            .note
            .as_deref()
            .is_some_and(|n| n.contains("Put off"))
    })
    .await;
    assert!(!s.reconnect.failed);
    wait_for(&engine, "what was held back", |s| {
        row(s, "new.bin").status == "done"
    })
    .await;
    wait_for(&engine, "the healthy one", |s| {
        row(s, "h.bin").status == "done"
    })
    .await;
    assert_eq!(t.server.redials.load(Ordering::SeqCst), 0);
    assert_eq!(
        t.server.requests("/unsized/200/h.bin").len(),
        1,
        "never stopped"
    );
    assert_eq!(t.server.requests("/eases/x.bin").len(), 1, "never stopped");
}

/// A healthy download that can't finish within the limit isn't waited for
/// with the queue held: new rows go on, and the redial comes after it.
#[tokio::test]
async fn no_hold_for_a_download_that_cannot_finish_in_time() {
    let t = Setup::new().await;
    let mut cfg = quick(&t);
    cfg.reconnect = true;
    cfg.throttling.drain_limit = Duration::from_millis(500);
    // A busy machine can miss so short a limit, which only puts the redial
    // off: it has to come up again within the test.
    cfg.throttling.redial_gap = Duration::from_millis(100);
    // What a download is expected to take is read from its recent speed: a
    // burst over a moment this short, as Windows' coarse timers make, would
    // promise it finishes far sooner than it does.
    cfg.throttling.compare_over = Duration::from_millis(800);
    let engine = t.engine_with(cfg).await;
    engine.add_text(&format!("{}/eases/x.bin", t.ip), "");
    engine.add_text(&format!("{}/paced/200/h.bin", t.name), "");
    wait_for(&engine, "the throttled row", |s| {
        row(s, "x.bin").slowed.is_some()
    })
    .await;
    engine.add_text(&format!("{}/files/new.bin", t.name), "");
    wait_for(&engine, "the new row", |s| {
        row(s, "new.bin").status == "done"
    })
    .await;
    let s = wait_for(&engine, "the healthy one", |s| {
        // Near its end, within the limit, it can be held for.
        if row(s, "h.bin").done < SIZE as u64 * 3 / 4 {
            assert_eq!(
                s.reconnect.draining,
                None,
                "no hold while it has long to go: {:#}",
                engine.debug_inspect()["engine"]["items"]
            );
        }
        assert_eq!(t.server.redials.load(Ordering::SeqCst), 0);
        row(s, "h.bin").status == "done"
    })
    .await;
    assert_eq!(row(&s, "h.bin").status, "done");
    wait_for(&engine, "the redial after it", |_| {
        t.server.redials.load(Ordering::SeqCst) == 1
    })
    .await;
    assert_eq!(
        t.server.requests("/paced/200/h.bin").len(),
        1,
        "never stopped"
    );
}

/// The gap `wants_new_address` had: a row running well, with only a mirror on
/// a host cooling down, is held back by nothing. With a throttled host going
/// on cooldown, it would have had the line cut under the row that just moved.
#[tokio::test]
async fn a_healthy_row_with_a_cooling_mirror_wants_no_new_address() {
    let t = Setup::new().await;
    let engine = t
        .engine_with(Config {
            reconnect: true,
            ..quick(&t)
        })
        .await;
    engine.set_preferred("127.0.0.1", true).unwrap();
    engine.add_text(
        &format!("{}/paced/40/x.bin {}/paced/200/x.bin", t.ip, t.name),
        "",
    );
    engine.add_text(&format!("{}/paced/200/h.bin", t.name), "");
    wait_for(&engine, "the move", |s| {
        row(s, "x.bin").shown_host == "localhost"
    })
    .await;
    wait_for(&engine, "both done", |s| {
        assert_eq!(s.reconnect.draining, None);
        all_done(s)
    })
    .await;
    assert_eq!(t.server.redials.load(Ordering::SeqCst), 0);
    assert_eq!(
        t.server.requests("/paced/200/x.bin").len(),
        1,
        "never stopped"
    );
}

/// A host as slow on the new address as on the old doesn't limit by address:
/// the line isn't cut again for it.
#[tokio::test]
async fn a_redial_that_did_not_help_is_not_repeated() {
    let t = Setup::new().await;
    let mut cfg = quick(&t);
    cfg.reconnect = true;
    cfg.throttling.redial_gap = Duration::from_millis(100);
    let engine = t.engine_with(cfg).await;
    engine.add_text(&format!("{}/paced/40/x.bin", t.ip), "");
    engine.add_text(&format!("{}/paced/200/h.bin", t.name), "");
    wait_for(&engine, "the redial", |_| {
        t.server.redials.load(Ordering::SeqCst) == 1
    })
    .await;
    wait_for(&engine, "the row to resume", |_| {
        t.server.requests("/paced/40/x.bin").len() == 2
    })
    .await;
    let s = wait_for(&engine, "it to be slow again", |s| {
        row(s, "x.bin").slowed.is_some()
    })
    .await;
    assert_eq!(
        row(&s, "x.bin").slowed.as_deref(),
        Some("Slowed by 127.0.0.1: no other host has it")
    );
    a_few_windows().await;
    assert_eq!(t.server.redials.load(Ordering::SeqCst), 1);
}

/// A host the user picks is used, though it slowed this row before and another
/// host could take it.
#[tokio::test]
async fn a_host_picked_again_is_used_though_it_slowed_the_row() {
    let t = Setup::new().await;
    let mut cfg = quick_to_judge(&t);
    cfg.throttling.cooldown = Duration::from_millis(300);
    cfg.throttling.cooldown_at_most = Duration::from_millis(300);
    let engine = t.engine_with(cfg).await;
    engine.set_preferred("127.0.0.1", true).unwrap();
    engine.add_text(
        &format!("{}/paced/40/x.bin {}/paced/150/x.bin", t.ip, t.name),
        "",
    );
    let s = wait_for(&engine, "it to leave", |s| {
        let it = row(s, "x.bin");
        it.status.as_str() == "downloading" && it.shown_host == "localhost" && it.speed > 0.0
    })
    .await;
    let id = row(&s, "x.bin").id.clone();
    assert!(engine.item_action(&id, ItemAction::Pause));
    wait_for(&engine, "the pause", |s| active(s, "localhost") == 0).await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(cooldown(&engine.snapshot(), "127.0.0.1"), 0);
    engine.download_from(std::slice::from_ref(&id), Some("127.0.0.1"));
    assert!(engine.item_action(&id, ItemAction::Resume));
    wait_for(&engine, "the picked host", |s| {
        active(s, "127.0.0.1") == 1 && row(s, "x.bin").shown_host == "127.0.0.1"
    })
    .await;
}

/// Time the whole process stood still (a stalled machine, a computer asleep)
/// says nothing about a host: a transfer running well isn't blamed for it.
#[tokio::test]
async fn a_frozen_process_is_not_a_slow_host() {
    let t = Setup::new().await;
    let engine = t.engine_with(quick_to_judge(&t)).await;
    engine.set_preferred("127.0.0.1", true).unwrap();
    engine.add_text(
        &format!("{}/paced/200/x.bin {}/files/x.bin", t.ip, t.name),
        "",
    );
    wait_for(&engine, "progress", |s| row(s, "x.bin").done > 0).await;
    for _ in 0..3 {
        // Holds this test's one thread, which everything here runs on.
        std::thread::sleep(Duration::from_millis(800));
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let s = engine.snapshot();
    assert_eq!(row(&s, "x.bin").shown_host, "127.0.0.1");
    assert_eq!(cooldown(&s, "127.0.0.1"), 0);
    assert!(t.server.requests("/files/x.bin").is_empty());
}
