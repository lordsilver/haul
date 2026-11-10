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
    Verifying, config::Endpoints, hosts, throttle::Throttling,
};
use serde_json::json;

#[path = "../support/mod.rs"]
mod support;

use support::*;

mod archives;
mod mirrors;
mod pages;
mod retries;
mod settings;
mod throttling;
mod wrappers;

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

/// An action on many rows is one change of the state, not one per row.
#[tokio::test]
async fn an_action_on_many_rows_is_one_change() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.set_paused(true);
    engine.add_text(
        &format!("{0}/files/a.bin {0}/files/b.bin {0}/files/c.bin", t.ip),
        "",
    );
    let s = wait_for(&engine, "the rows", |s| s.items.len() == 3).await;
    let ids: Vec<String> = s.items.iter().map(|i| i.id.clone()).collect();
    // Whatever lookups there are have had their say.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let before = *engine.watch().borrow();
    let batch = engine.items_action(&ids, ItemAction::Pause);
    assert_eq!((batch.done, batch.failed.len()), (3, 0));
    assert_eq!(*engine.watch().borrow(), before + 1);
    assert!(
        engine
            .snapshot()
            .items
            .iter()
            .all(|i| i.status.as_str() == "paused")
    );
}

/// A new row named the same as a finished one goes, and folds nothing into
/// itself on the way: an idle row for the same file keeps its links.
#[tokio::test]
async fn a_row_that_goes_takes_no_other_row_with_it() {
    let t = Setup::new().await;
    std::fs::create_dir_all(t.root.join("data")).unwrap();
    // In this order in the queue: the idle row first.
    let queue = json!({
        "aidle00000": {"mirrors": [format!("{}/files/x.bin", t.name)], "name": "x.bin", "size": SIZE, "status": "paused"},
        "bdone00000": {"mirrors": [format!("{}/other/x.bin", t.ip)], "name": "x.bin", "size": SIZE, "status": "done"},
        "cnew000000": {"mirrors": [format!("{}/files/x.bin?fresh", t.ip)], "name": null, "size": 0, "status": "queued"},
    });
    std::fs::write(t.root.join("data/queue.json"), queue.to_string()).unwrap();
    let engine = t.engine().await;
    engine.set_paused(true);
    let s = wait_for(&engine, "the new row looked up", |s| {
        s.items.iter().all(|i| i.id != "cnew000000")
    })
    .await;
    let idle = s.items.iter().find(|i| i.id == "aidle00000");
    assert!(idle.is_some(), "the idle row stays: {:?}", s.items);
    assert_eq!(engine.debug_links("aidle00000").len(), 1);
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
async fn pause_all_pauses_running_transfers() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.add_text(&format!("{}/slow/m.bin", t.ip), "");
    wait_for(&engine, "progress", |s| s.items[0].done > 0).await;
    engine.set_paused(true);
    let s = wait_for(&engine, "a stop", |s| active(s, "127.0.0.1") == 0).await;
    assert_eq!(
        s.items[0].status.as_str(),
        "queued",
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

/// The wrapper in the snapshot once it is unwrapped (or waits for a person).
async fn wrapper_settled(engine: &Engine, status: &str) -> Snapshot {
    wait_for(engine, &format!("the wrapper to be {status}"), |s| {
        s.wrappers.len() == 1 && s.wrappers[0].status.as_str() == status
    })
    .await
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
    assert_eq!(s.wrappers[0].status.as_str(), "done");
    assert!(
        s.items.iter().all(|i| i.status.as_str() == "paused"),
        "{:?}",
        s.items
    );
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
        s.wrappers.iter().any(|w| w.status.as_str() == "done") && s.items.len() == 2
    })
    .await;
    assert!(t.server.requests("/blog/confirm").is_empty());
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
        s.items
            .iter()
            .any(|i| links(i) && i.status.as_str() == "passing")
    })
    .await;
    s.items.iter().find(|i| links(i)).unwrap().id.clone()
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
            .map(|h| h.kind.as_str())
    };
    assert_eq!(kind("filecrypt.cc"), Some("wrapper"));
    assert_eq!(kind("zpaste.net"), Some("paste"));
    assert_eq!(kind("1fichier.com"), Some("host"));
    assert_eq!(kind("vik1ngfile.site"), None, "vikingfile.com's");
    assert_eq!(kind("rentry.org"), None, "rentry.co's");
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
            accounts: [("1fichier_key".to_string(), "key".to_string())].into(),
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

/// The rules with their waits shortened, so a test takes seconds: at these
/// speeds /paced/40 is slow and /paced/200 runs well, and a whole file at 40
/// KiB/s takes 15 seconds, which is worth leaving. Nothing is unsure for an
/// hour, unless a test wants it to be.
fn quick(t: &Setup) -> Config {
    Config {
        throttling: Throttling {
            slow: 100_000.0,
            grace: Duration::from_millis(200),
            window: Duration::from_millis(600),
            tick: Duration::from_millis(50),
            worth_moving: Duration::from_secs(3),
            far_below_ceiling: 100_000.0,
            unsure_after: Duration::from_secs(3600),
            compare_over: Duration::from_millis(200),
            line_patience: Duration::from_secs(3600),
            drain_limit: Duration::from_secs(10),
            redial_check: Duration::from_millis(100),
            redial_gap: Duration::from_secs(3600),
            ..Throttling::default()
        },
        retry_failed_minutes: 0,
        ..t.config()
    }
}

/// As [`quick`], with a slow transfer that nothing explains counted as
/// throttled soon: what a guard that fails would let through.
fn quick_to_judge(t: &Setup) -> Config {
    let mut cfg = quick(t);
    cfg.throttling.unsure_after = Duration::from_millis(100);
    cfg
}

fn row<'a>(s: &'a Snapshot, name: &str) -> &'a haul_core::ItemView {
    s.items.iter().find(|i| i.name == name).unwrap()
}

fn cooldown(s: &Snapshot, host: &str) -> u64 {
    s.hosts.iter().find(|h| h.name == host).unwrap().cooldown
}

/// The requests for `path` that fetch part of it, leaving out the lookup every
/// attempt starts with, which the stand-in's /files logs too.
fn asked_for_bytes(t: &Setup, path: &str) -> Vec<String> {
    t.server
        .requests(path)
        .into_iter()
        .filter(|r| r.contains("bytes="))
        .collect()
}

/// Long enough for several windows to pass.
async fn a_few_windows() {
    tokio::time::sleep(Duration::from_millis(2000)).await;
}

/// A small file that finishes soon is left alone, however slow.
#[tokio::test]
async fn a_small_file_finishing_soon_is_left_alone() {
    let t = Setup::new().await;
    let mut cfg = quick_to_judge(&t);
    cfg.throttling.worth_moving = Duration::from_secs(60);
    let engine = t.engine_with(cfg).await;
    engine.set_preferred("127.0.0.1", true).unwrap();
    engine.add_text(
        &format!("{}/paced/40/x.bin {}/files/x.bin", t.ip, t.name),
        "",
    );
    a_few_windows().await;
    let s = engine.snapshot();
    assert_eq!(row(&s, "x.bin").shown_host, "127.0.0.1");
    assert!(t.server.requests("/files/x.bin").is_empty());
    assert_eq!(row(&s, "x.bin").slowed, None);
    assert_eq!(cooldown(&s, "127.0.0.1"), 0);
}

/// The setting turns all of it off.
#[tokio::test]
async fn it_can_be_switched_off() {
    let t = Setup::new().await;
    let engine = t
        .engine_with(Config {
            leave_slow_hosts: false,
            ..quick(&t)
        })
        .await;
    engine.set_preferred("127.0.0.1", true).unwrap();
    engine.add_text(
        &format!("{}/paced/40/x.bin {}/files/x.bin", t.ip, t.name),
        "",
    );
    engine.add_text(&format!("{}/paced/200/h.bin", t.name), "");
    a_few_windows().await;
    let s = engine.snapshot();
    assert_eq!(row(&s, "x.bin").shown_host, "127.0.0.1");
    assert_eq!(row(&s, "x.bin").slowed, None);
    assert!(t.server.requests("/files/x.bin").is_empty());
    assert_eq!(cooldown(&s, "127.0.0.1"), 0);
}

/// A connection lost before the bytes were compared proves nothing: the next
/// try compares them too, rather than add another file's bytes to the partial.
#[tokio::test]
async fn a_connection_lost_during_the_check_is_checked_again() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.set_preferred("127.0.0.1", true).unwrap();
    engine.add_text(
        &format!("{}/paced/400/x.bin {}/lost-check/x.bin", t.ip, t.name),
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
    assert!(
        t.downloaded("x.bin") == content("other x.bin", SIZE),
        "the bytes of two files mixed"
    );
    let asked = t.server.requests("/lost-check/x.bin");
    let check = format!("/lost-check/x.bin bytes={}-", kept - 64 * 1024);
    assert_eq!(asked[..2], [check.clone(), check], "{asked:?}");
}
