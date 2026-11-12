//! Wrappers, pages of links and pastes, unwrapped into files.

use super::*;

/// A link pasted while one download waits for a person is that download's;
/// text, or a link while none waits, is for the Add dialog.
#[tokio::test]
async fn a_pasted_link_goes_to_the_one_download_that_waits_for_it() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    let text = format!("{0}/files/x.bin {0}/files/y.bin", t.ip);
    let pasted = engine.paste(&text).unwrap();
    assert_eq!(pasted.open_add.as_deref(), Some(text.as_str()));
    let lone = format!("{}/files/e.bin", t.ip);
    assert_eq!(engine.paste(&lone).unwrap().used_for, None, "none waits");
    engine.add_text(&format!("{}/page/getfile", t.ip), "");
    let s = wait_for(&engine, "captcha", |s| {
        s.items[0].status.as_str() == "captcha"
    })
    .await;
    let pasted = engine.paste(&format!(" {lone}\n")).unwrap();
    assert_eq!(pasted.used_for.as_deref(), Some(s.items[0].id.as_str()));
    assert_eq!(pasted.open_add, None);
    wait_for(&engine, "done", all_done).await;
    assert_eq!(t.downloaded("e.bin"), content("e.bin", SIZE));
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
    assert_eq!(
        (it.name.as_str(), it.status.as_str()),
        ("kept.bin", "paused")
    );
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
    assert_eq!(s.wrappers[0].status.as_str(), "done");
    let mut rows: Vec<(&str, &str, &str)> = s
        .items
        .iter()
        .map(|i| (i.name.as_str(), i.package.as_str(), i.status.as_str()))
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
        s.items.iter().any(|i| i.status.as_str() == "captcha")
    })
    .await;
}

/// `n/a` in a container's table is no file's name: its links are not one file
/// on several hosts, nor is any row called that.
#[tokio::test]
async fn a_containers_rows_without_names_stay_separate() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.add_text("https://filecrypt.cc/Container/unnamed.html", "");
    let s = wait_for(&engine, "the container's links", |s| {
        s.wrappers
            .first()
            .is_some_and(|w| w.status.as_str() == "done")
            && !s.items.is_empty()
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
/// A wrapper's file only on a host without its account is listed and starts
/// with the others: no other host has it, so that one is used.
#[tokio::test]
async fn a_wrappers_file_only_on_a_host_without_its_account_starts_with_the_others() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.add_text("https://filecrypt.cc/Container/for-account.html", "");
    let s = wait_for(&engine, "the container's links", |s| {
        s.items.len() == 3 && s.items.iter().all(|i| i.status.as_str() == "paused")
    })
    .await;
    assert!(s.items.iter().all(|i| i.list.is_some()), "all listed");
    assert_eq!(s.counts.all, 3);
    assert_eq!(s.wrapper_cards[0].files, 3);
    let id = s.wrappers[0].id.clone();
    assert_eq!(engine.start_wrapper(&id), Some(3));
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
        s.wrappers.iter().any(|w| w.status.as_str() == "done") && s.items.len() == 2
    })
    .await;
    assert_eq!(s.wrappers.len(), 1, "the page is a wrapper");
    let mut rows: Vec<(&str, &str)> = s
        .items
        .iter()
        .map(|i| (i.name.as_str(), i.status.as_str()))
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
            && s.wrappers.iter().any(|w| w.status.as_str() == "done")
            && s.wrappers.iter().any(|w| w.status.as_str() == "captcha")
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
    let container = s
        .wrappers
        .iter()
        .find(|w| w.status.as_str() == "captcha")
        .unwrap();
    engine
        .set_direct(
            &container.id,
            "https://filecrypt.cc/DLC/abc123.dlc",
            BTreeMap::new(),
            None,
        )
        .unwrap();
    let s = wait_for(&engine, "the container's files", |s| {
        s.items.len() == 2 && s.wrappers.iter().all(|w| w.status.as_str() == "done")
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

/// A page passed out of sight that turns out to list nothing (a paste
/// holding only a host's name) fails saying so, rather than ask a person.
#[tokio::test]
async fn a_page_that_lists_nothing_fails_without_asking() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.set_pages_pass(true);
    engine.add_text(&format!("{}/blog/guarded", t.ip), "");
    let s = wait_for(&engine, "the page passed out of sight", |s| {
        s.items.iter().any(|i| i.status.as_str() == "passing")
    })
    .await;
    let id = s.items[0].id.clone();
    engine.page_gone(&id, "the paste on zpaste.net lists no links");
    let s = wait_for(&engine, "the page to fail", |s| {
        s.items.iter().all(|i| i.status.as_str() == "error")
    })
    .await;
    assert_eq!(s.items[0].error, "the paste on zpaste.net lists no links");
    assert_eq!(s.items[0].retry_in, 0, "not tried again on a timer");
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
        s.wrappers.iter().any(|w| w.status.as_str() == "done")
            && s.items.len() == 2
            && s.items.iter().all(|i| !i.tentative)
    })
    .await;
    assert!(s.items.iter().all(|i| i.mirrors == ["pixeldrain.com"]));
    assert_eq!(s.counts.all, 2);
    assert_eq!(s.counts.need_you, 0, "nobody asked about the plain page");
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
        s.wrappers.len() == 2 && s.wrappers.iter().all(|w| w.status.as_str() == "done")
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

/// A wrapper's file on a host that names nothing without its account
/// (gofile) is looked up through its page while it waits, not started and
/// asking nobody: a page that names its file joins it to the same file on
/// other hosts, one that says it is gone takes it off the list, and one that
/// tells nothing leaves it as it was, not opened for that again.
#[tokio::test]
async fn a_wrappers_unnamed_file_is_looked_up_through_its_page() {
    use haul_core::Browsing;

    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.set_pages_pass(true);
    engine.set_disabled("gofile.io", false).unwrap();
    engine.add_text("https://fileguard.cc/vault/loose-gofile", "");
    wrapper_settled(&engine, "done").await;
    let s = wait_for(&engine, "five rows, the pixeldrain ones named", |s| {
        s.items.len() == 5 && s.items.iter().filter(|i| i.name.ends_with(".bin")).count() == 2
    })
    .await;
    let gofile: Vec<String> = s
        .items
        .iter()
        .filter(|i| i.mirrors == ["gofile.io"])
        .map(|i| i.id.clone())
        .collect();
    assert_eq!(gofile.len(), 3);
    assert!(s.items.iter().all(|i| i.status.as_str() == "paused"));
    // Until they are looked up, they are in no list, and the card counts
    // them as links being checked rather than as its files.
    let looking: Vec<&str> = s
        .items
        .iter()
        .filter(|i| i.looking_up)
        .map(|i| i.id.as_str())
        .collect();
    assert_eq!(looking.len(), 3, "{looking:?}");
    assert!(
        s.items
            .iter()
            .filter(|i| i.looking_up)
            .all(|i| i.tentative && i.list.is_none())
    );
    assert_eq!((s.counts.all, s.counts.ready), (2, 2));
    assert_eq!(s.counts.checking, 3, "the files being looked up");
    assert_eq!(
        (s.wrapper_cards[0].files, s.wrapper_cards[0].checking),
        (2, 3)
    );
    assert!(!s.wrapper_cards[0].clearable);
    let idle = Browsing {
        out_of_sight: &[],
        with_person: &|_| false,
        has_page: &|_| false,
    };
    // At gofile's pace (plan LOOK-01): two at once, then one per three
    // seconds.
    let mut planned = Vec::new();
    let began = std::time::Instant::now();
    while planned.len() < 3 {
        if let Some((id, page)) = engine.plan_passes(&idle).start {
            assert!(page.starts_with("https://gofile.io/d/"), "{page}");
            planned.push((id, page));
        }
        assert!(began.elapsed() < Duration::from_secs(10), "a lookup");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        began.elapsed() >= Duration::from_millis(2500),
        "the third waits its turn"
    );
    assert_eq!(engine.plan_passes(&idle).start, None, "each page once");
    let row = |name: &str| {
        planned
            .iter()
            .find(|(_, page)| page.ends_with(name))
            .unwrap()
            .0
            .clone()
    };

    // Named: the same file as pixeldrain's p1.bin, which takes the link.
    let named = haul_core::hosts::Entry {
        url: "https://store.gofile.test/download/p1.bin".into(),
        name: Some("p1.bin".into()),
        ..Default::default()
    };
    engine.set_files(&row("same"), vec![named]).unwrap();
    // Gone: the row leaves, nobody told.
    engine.page_gone(&row("dead"), "gofile: the folder was deleted");
    // Nothing told: the row stays as it was.
    assert!(!engine.page_wants_person(&row("shy")), "nobody is asked");
    let s = wait_for(&engine, "the named and the gone row to go", |s| {
        s.items.len() == 3
    })
    .await;
    let p1 = s.items.iter().find(|i| i.name == "p1.bin").unwrap();
    assert!(
        p1.mirrors.contains(&"gofile.io".to_string()),
        "{:?}",
        p1.mirrors
    );
    let shy = s.items.iter().find(|i| i.id == row("shy")).unwrap();
    assert_eq!(shy.status.as_str(), "paused");
    // Its page told nothing: no row to show, it waits on its card to be
    // asked again later (D-4), and is not asked again now.
    assert!(shy.tentative && shy.list.is_none(), "{shy:?}");
    assert_eq!(s.wrapper_cards[0].checking, 1);
    assert!(shy.error.is_empty(), "{}", shy.error);
    assert_eq!(engine.plan_passes(&idle).start, None, "asked again later");
    assert!(s.items.iter().all(|i| i.status.as_str() == "paused"));
    assert_eq!(engine.waiting_for_person(), (vec![], vec![]));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(engine.plan_passes(&idle).start, None, "not looked up again");

    // Nor after a restart: when to ask again is saved (plan LOOK-04).
    engine.shutdown().await;
    drop(engine);
    let engine = start_when_free(|| t.options()).await;
    engine.set_pages_pass(true);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(engine.plan_passes(&idle).start, None, "not asked again yet");
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
    assert_eq!(s.counts.reading, 0, "nothing left to read");
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
    assert_eq!(s.counts.reading, 0, "nothing left to read");
}

/// What a container listed is kept as read, each row with its place, the
/// text and size beside it and its mark: after a restart, and after one of
/// its files is removed, whose links it still knows (plan MODEL-01).
#[tokio::test]
async fn a_containers_listing_survives_a_restart_and_its_files() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.add_text("https://filecrypt.cc/Container/open.html", "");
    let s = wrapper_settled(&engine, "done").await;
    let id = s.wrappers[0].id.clone();
    let listing = engine.debug_listing(&id);
    assert!(!listing.is_empty());
    assert_eq!(listing[0].at, 1);
    assert!(listing.iter().all(|l| !l.urls.is_empty()));
    assert!(engine.item_action(&s.items[0].id, ItemAction::Remove));
    engine.shutdown().await;
    drop(engine);

    let engine = start_when_free(|| t.options()).await;
    assert_eq!(engine.debug_listing(&id), listing);
}

/// Read again, a card's removed files come back, not started, with the same
/// links, and nothing it holds is added twice (plan CARD-03). Adding its
/// link again does the same, and says so (CARD-04).
#[tokio::test]
async fn a_card_read_again_brings_back_its_removed_files() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.add_text("https://filecrypt.cc/Container/open.html", "");
    let s = wrapper_settled(&engine, "done").await;
    let card = s.wrappers[0].id.clone();
    let before: Vec<Vec<String>> = {
        let mut links: Vec<Vec<String>> =
            s.items.iter().map(|i| engine.debug_links(&i.id)).collect();
        links.sort();
        links
    };
    assert_eq!(before.len(), 2);
    assert!(engine.item_action(&s.items[0].id, ItemAction::Remove));
    wait_for(&engine, "one file left", |s| s.items.len() == 1).await;

    let batch = engine.wrappers_action(
        std::slice::from_ref(&card),
        haul_core::WrapperAction::Reread,
    );
    assert_eq!(batch.done, 1);
    let s = wait_for(&engine, "the removed file back", |s| {
        s.items.len() == 2 && s.wrappers[0].status.as_str() == "done"
    })
    .await;
    let mut after: Vec<Vec<String>> = s.items.iter().map(|i| engine.debug_links(&i.id)).collect();
    after.sort();
    assert_eq!(after, before, "the same links, once each");
    assert!(s.items.iter().all(|i| i.not_started));

    // Adding its link again reads it again, rather than a duplicate.
    assert!(engine.item_action(&s.items[0].id, ItemAction::Remove));
    wait_for(&engine, "one file left", |s| s.items.len() == 1).await;
    let added = engine.add_text("https://filecrypt.cc/Container/open.html", "");
    assert_eq!((added.reread, added.duplicates), (1, 0));
    wait_for(&engine, "the removed file back", |s| s.items.len() == 2).await;
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
        s.items.iter().any(|i| i.status.as_str() == "downloading")
    })
    .await;
    engine.add_text("https://filecrypt.cc/Container/missing-busy.html", "");
    let s = wrapper_settled(&engine, "error").await;
    assert!(engine.debug_retry_now(&s.wrappers[0].id));
    wrapper_settled(&engine, "done").await;
    let s = engine.snapshot();
    assert!(
        s.items.iter().any(|i| i.status.as_str() == "downloading"),
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
    assert!(s.wrappers[0].lists_nothing, "{}", s.wrappers[0].error);
    assert_eq!(s.wrappers[0].retry_in, 0, "not tried again on a timer");
    engine.shutdown().await;
    drop(engine);

    let engine = start_when_free(|| through_stand_in(&t)).await;
    let s = engine.snapshot();
    assert_eq!(s.wrappers[0].status.as_str(), "error");
    assert!(s.wrappers[0].lists_nothing, "after a restart too");
    assert_eq!(s.wrappers[0].retry_in, 0, "nor after a restart");
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
    assert!(!engine.page_wants_person(&id), "its own tries first");
    let s = wrapper_settled(&engine, "captcha").await;
    assert!(s.wrapper_cards[0].retry_in > 0, "the card says when");
    assert_eq!(s.wrapper_cards[0].need_you, 1);
    for round in 1..=3 {
        assert!(engine.debug_retry_now(&id), "try {round}");
        wrapper_settled(&engine, "passing").await;
        assert_eq!(auto_retries(&engine, &id), round);
        assert_eq!(
            engine.page_wants_person(&id),
            round == 3,
            "told after its last try"
        );
        let s = wrapper_settled(&engine, "captcha").await;
        if round < 3 {
            assert!(s.wrappers[0].retry_in > 0, "try {round} is not the last");
        }
    }
    let s = engine.snapshot();
    assert_eq!(s.wrappers[0].retry_in, 0, "its tries are used up");
    assert_eq!(s.wrapper_cards[0].retry_in, 0, "the card says none");
    assert!(!engine.debug_retry_now(&id));
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

/// Every link of a vault shows the same page: added once, solved once. One
/// of its links added again later reads it again (plan CARD-04).
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
            .reread,
        1,
        "its card is read again"
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(t.server.requests("/vault/abc solved").len(), 2);
    assert_eq!(engine.snapshot().items.len(), 1, "its file once");
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
    assert_eq!(
        s.items[0].status.as_str(),
        "queued",
        "still the user's download"
    );
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
            .map(|i| (i.name.as_str(), i.status.as_str()))
            .collect::<Vec<_>>(),
        [("r.part1.rar", "queued")]
    );
    assert!(!engine.remove_wrapper(&w), "gone");
    assert_eq!(engine.start_wrapper(&w), None);
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
        s.wrappers[0].status.as_str() == "captcha" && s.wrappers[0].error.contains("404")
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
        s.wrappers.len() == 2 && s.wrappers.iter().all(|w| w.status.as_str() == "passing")
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
            .any(|w| w.url == once && w.status.as_str() == "done")
            && s.wrappers
                .iter()
                .any(|w| w.url == always && w.status.as_str() == "passing")
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

/// A site whose check came back after a clearance has its other pages read
/// by the browser alone for a while: Haul asks the site nothing first, and
/// hands no clearance back, so each container is asked for once.
#[tokio::test]
async fn a_site_whose_clearance_does_not_hold_is_read_in_the_browser() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.set_pages_pass(true);
    let first = "https://filecrypt.cc/Container/cf-always-held.html";
    engine.add_text(first, "");
    let s = wrapper_settled(&engine, "passing").await;
    engine.page_cleared(
        &s.wrappers[0].id,
        &["cf_clearance=ok; Path=/".to_string()],
        Some("Browser/1.0"),
    );
    wait_for(&engine, "the check again after the clearance", |_| {
        container_asks("cf-always-held.html") >= 2
    })
    .await;
    wrapper_settled(&engine, "passing").await;

    let next = "https://filecrypt.cc/Container/cf-always-held-2.html";
    engine.add_text(next, "");
    let s = wait_for(&engine, "the next container waiting for the browser", |s| {
        s.wrappers
            .iter()
            .any(|w| w.url == next && w.status.as_str() == "passing")
    })
    .await;
    assert_eq!(
        container_asks("cf-always-held-2.html"),
        0,
        "Haul asked the site nothing"
    );
    assert!(!engine.takes_clearance(next), "the browser reads it itself");
    assert!(s.wrappers.iter().all(|w| !w.error.is_empty()));
}

/// A wrapper whose page passed out of sight still wants a person shows so
/// at once on its card, even while downloads are paused: it has no other
/// link to go on with, so the notification that comes once its own tries
/// are spent is never left without a card to act on.
#[tokio::test]
async fn a_wrapper_that_wants_a_person_says_so_while_paused() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.set_pages_pass(true);
    engine.set_paused(true);
    engine.add_text("https://filecrypt.cc/Container/cf-always.html", "");
    let s = wrapper_settled(&engine, "passing").await;
    assert!(!engine.page_wants_person(&s.wrappers[0].id));
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
    assert_ne!(s.wrappers[0].status.as_str(), "done", "{:?}", s.wrappers[0]);
    let site = s.hosts.iter().find(|h| h.name == "filecrypt.cc").unwrap();
    assert_eq!(
        (site.kind.as_str(), site.group.as_str()),
        ("wrapper", "wrapper")
    );
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
            .filter(|w| w.host == "filecrypt.cc" && w.status.as_str() == "done")
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

/// Containers of one set where a host's table names only some files, or
/// shows something else than a file's name in a row, still make one archive:
/// every part's links on one row, under the name that says most.
#[tokio::test]
async fn containers_with_unnamed_rows_still_merge() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.add_text(&format!("{}/blog/per-host-mixed", t.ip), "");
    let s = wait_for(&engine, "the three containers unwrapped", |s| {
        s.wrappers
            .iter()
            .filter(|w| w.host == "filecrypt.cc" && w.status.as_str() == "done")
            .count()
            == 3
    })
    .await;
    let mut names: Vec<&str> = s.items.iter().map(|i| i.name.as_str()).collect();
    names.sort_unstable();
    assert!(
        !names.iter().any(|n| n.contains("moment")),
        "no row named after a page's title: {names:?}"
    );
    let parts: Vec<_> = s
        .items
        .iter()
        .filter(|i| i.name.starts_with("Example.Archive.part"))
        .collect();
    assert_eq!(parts.len(), 4, "{names:?}");
    // Part 3 is unnamed only in the third table: it still joins by its
    // link's name.
    for p in &parts {
        let hosts = p.mirrors.len();
        let want = if p.name.ends_with("part1.rar") || p.name.ends_with("part2.rar") {
            2
        } else {
            3
        };
        assert!(
            hosts >= want,
            "{} has {hosts} links: {:?}",
            p.name,
            p.mirrors
        );
    }
    assert_eq!(s.archives.len(), 1, "{:?}", s.archives);
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
            .filter(|w| w.host == "filecrypt.cc" && w.status.as_str() == "done")
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
            .filter(|w| w.host == "filecrypt.cc" && w.status.as_str() == "done")
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
