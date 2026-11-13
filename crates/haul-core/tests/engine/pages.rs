//! Pages that want a person or a browser: Needs you, passes, handed-back links.

use super::*;

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
            (s.items[0].status.as_str(), s.items[0].existing),
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

/// A link whose host says the file was deleted fails as soon as it is
/// looked up, with the host's words, rather than on its turn.
#[tokio::test]
async fn a_deleted_file_fails_when_it_is_looked_up() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.set_paused(true);
    engine.add_text("https://1fichier.com/?gone123", "");
    let s = wait_for(&engine, "the deleted file to fail", |s| {
        s.items
            .first()
            .is_some_and(|i| i.status.as_str() == "error")
    })
    .await;
    assert!(s.items[0].error.contains("deleted"), "{}", s.items[0].error);
    assert_eq!(s.items[0].retry_in, 0, "not tried again on a timer");
}

#[tokio::test]
async fn page_needs_a_person_then_their_link_is_used() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    let page = format!("{}/page/getfile", t.ip);
    engine.add_text(&page, "");
    let s = wait_for(&engine, "captcha", |s| {
        s.items[0].status.as_str() == "captcha"
    })
    .await;
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
    assert_eq!(s.items[0].status.as_str(), "checking");
    assert_eq!(s.items[0].list, None, "in no list while it is read");
    assert_eq!(s.counts.all, 0, "not counted among the downloads");
    assert_eq!(s.counts.reading, 1, "the status bar says it is worked on");

    // Its files show once their host names them, never as bare links.
    let s = wait_for(&engine, "the page's files", |s| {
        s.items.len() == 2 && s.counts.all == 2
    })
    .await;
    assert_eq!(s.wrappers.len(), 1);

    engine.add_text(&format!("{}/files/report.bin", t.ip), "");
    let s = engine.snapshot();
    let file = s.items.iter().find(|i| i.name == "report.bin").unwrap();
    assert!(
        file.list.is_some(),
        "a file's link is a download from the start"
    );
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

/// A copied page is fetched once to be judged and read.
#[tokio::test]
async fn a_copied_page_costs_one_request() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.add_from_clipboard(&format!("{}/blog/links", t.ip));
    wait_for(&engine, "the page unwrapped", |s| {
        s.wrappers.iter().any(|w| w.status.as_str() == "done") && s.items.len() == 2
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
        s.wrappers.len() == 2 && s.wrappers.iter().all(|w| w.status.as_str() == "done")
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
        s.items.iter().any(|i| i.status.as_str() == "captcha")
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

/// A page that only leads elsewhere (a link shortener, walked out of sight
/// by the desktop app) is no row once it has: the files it led to take its
/// place, started as it was, and its site is no host to show.
#[tokio::test]
async fn a_page_that_led_elsewhere_gives_way_to_what_it_led_to() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    let page = format!("{}/blog/guarded", t.ip);
    engine.add_text(&page, "Pkg");
    let s = wait_for(&engine, "the page to need a browser", |s| {
        s.items.iter().any(|i| i.status.as_str() == "captcha")
    })
    .await;
    let id = s.items[0].id.clone();
    let led = [
        "https://pixeldrain.com/u/postone".to_string(),
        "https://pixeldrain.com/u/posttwo".to_string(),
    ];
    assert!(engine.page_led_to(&id, &led));
    let s = wait_for(&engine, "the files it led to, named", |s| {
        s.items.len() == 2 && s.items.iter().all(|i| i.name.ends_with(".bin"))
    })
    .await;
    assert!(
        s.wrappers.is_empty(),
        "no row for the page: {:?}",
        s.wrappers
    );
    for i in &s.items {
        assert!(i.id != id && i.source.is_none() && i.package == "Pkg");
        assert_ne!(i.status.as_str(), "paused", "started, as the link was");
    }
    // Led only to its own address, nothing is taken.
    assert!(!engine.page_led_to(&s.items[0].id, &[]));
}

/// A page that led only to links the list holds already just goes, as a
/// duplicate does.
#[tokio::test]
async fn a_page_that_led_to_what_the_list_holds_goes() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.add_text("https://pixeldrain.com/u/postone", "");
    wait_for(&engine, "the file, named", |s| {
        s.items.len() == 1 && s.items[0].name.ends_with(".bin")
    })
    .await;
    engine.add_text(&format!("{}/blog/guarded", t.ip), "");
    let s = wait_for(&engine, "the page to need a browser", |s| {
        s.items.iter().any(|i| i.status.as_str() == "captcha")
    })
    .await;
    let id = s
        .items
        .iter()
        .find(|i| i.status.as_str() == "captcha")
        .unwrap()
        .id
        .clone();
    assert!(engine.page_led_to(&id, &["https://pixeldrain.com/u/postone".to_string()]));
    let s = engine.snapshot();
    assert_eq!(s.items.len(), 1, "the file, once");
    assert!(s.items[0].id != id && s.items[0].also_from.is_empty());
    assert!(
        s.wrappers.is_empty(),
        "no row for the page: {:?}",
        s.wrappers
    );
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
        s.items.iter().any(|i| i.status.as_str() == "passing")
    })
    .await;
    let id = s.items[0].id.clone();
    assert!(s.items[0].tentative && engine.is_tentative(&id));
    let passing = s.counts.passing;
    assert_eq!((s.counts.all, passing.now + passing.queued), (0, 1));
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
        s.wrappers.iter().any(|w| w.status.as_str() == "done")
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
    assert_eq!(s.counts.reading, 0);
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
        s.items.iter().any(|i| i.status.as_str() == "passing")
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
    assert_eq!(it.list.map(|l| l.as_str()), Some("done"));
    assert_eq!((s.counts.all, s.counts.reading), (1, 0));
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
    // Waiting for the desktop app to pass its page: nothing for the person.
    let s = wait_for(&engine, "the page to wait its turn", |s| {
        s.items.iter().any(|i| i.status.as_str() == "passing")
    })
    .await;
    let id = s.items[0].id.clone();
    // Its window came forward: now it needs the person, after its own
    // tries.
    assert!(!engine.page_wants_person(&id), "the download stays");
    wait_for(&engine, "the page waiting for its own try", |s| {
        s.items.iter().any(|i| i.status.as_str() == "waiting")
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
        s.items.iter().any(|i| i.status.as_str() == "captcha")
    })
    .await;
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
        s.items[0].status.as_str() == "passing"
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

/// The links a download's host page shows (its ads, its other files) never
/// stand in for the download: only a row that is the page itself, added on
/// its own, takes them. A wrapper's file stays that file.
#[tokio::test]
async fn a_downloads_host_page_never_turns_it_into_a_list() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.set_pages_pass(true);
    engine.set_disabled("gofile.io", false).unwrap();
    engine.add_text("https://fileguard.cc/vault/held", "");
    wrapper_settled(&engine, "done").await;
    let s = wait_for(&engine, "the file", |s| s.items.len() == 1).await;
    let (id, name, mirrors) = (
        s.items[0].id.clone(),
        s.items[0].name.clone(),
        s.items[0].mirrors.clone(),
    );
    // Its link on a site Haul has no code for, as the vault lists it.
    let unknown = format!("{}/files/held.bin", t.ip);
    assert!(hosts::may_list_links(&unknown));
    assert!(!engine.page_lists(&id, &unknown));
    let shown = ["https://pixeldrain.com/u/adone".to_string()];
    assert_eq!(engine.page_listed(&id, &shown, &[], None), 0);
    let s = engine.snapshot();
    assert_eq!(s.items.len(), 1);
    assert_eq!(s.items[0].name, name, "still the file");
    assert_eq!(s.items[0].mirrors, mirrors);

    // The row of a link added on its own, the newest.
    let added = async |url: &str, count: usize| {
        engine.add_text(url, "");
        let s = wait_for(&engine, "its row", |s| s.items.len() == count).await;
        s.items.last().expect("a row").id.clone()
    };
    let page = format!("{}/page/added", t.ip);
    let row = added(&page, 2).await;
    assert!(engine.page_lists(&row, &page), "a page added on its own");
    let named = format!("{}/page/archive.part2.rar", t.ip);
    let row = added(&named, 3).await;
    assert!(
        !engine.page_lists(&row, &named),
        "a link whose path names a file is that file's page"
    );
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
        s.items[0].status.as_str() == "passing"
    })
    .await;
    engine.page_later(
        &id,
        "https://gofile.io/d/held",
        "vikingfile: too many requests",
        None,
    );
    let s = engine.snapshot();
    assert_eq!(s.items[0].status.as_str(), "waiting");
    assert!(s.items[0].wait > 0, "it counts down to the next pass");
    assert_eq!(s.items[0].error, "vikingfile: too many requests");
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
        s.items.iter().any(|i| i.status.as_str() == "passing")
    })
    .await;
    assert!(engine.page_passes(&page));
    engine.add_text(&format!("{}/limited-long/d.bin", t.ip), "");
    let s = wait_for(&engine, "both to wait", |s| {
        s.items.len() == 2 && s.items.iter().all(|i| i.status.as_str() == "waiting")
    })
    .await;
    assert_eq!(s.counts.need_you, 0, "no moment under Needs you");
    assert!(!engine.page_passes(&page), "no pass meanwhile");
}

/// The desktop app passes one page at a time, each once per wait: the engine
/// plans the next pass, holds it while another runs or a person has the
/// page, and stops a pass whose row stopped waiting.
#[tokio::test]
async fn passes_go_one_at_a_time_and_once_each() {
    use haul_core::Browsing;

    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.set_pages_pass(true);
    let page = format!("{}/blog/guarded", t.ip);
    engine.add_text(&page, "");
    let s = wait_for(&engine, "the page to wait its turn", |s| {
        s.items.iter().any(|i| i.status.as_str() == "passing")
    })
    .await;
    let id = s.items[0].id.clone();
    fn idle(out: &[String]) -> Browsing<'_> {
        Browsing {
            out_of_sight: out,
            with_person: &|_| false,
            has_page: &|_| false,
        }
    }
    let plan = engine.plan_passes(&idle(&[]));
    assert_eq!(plan.start, Some((id.clone(), page.clone())));
    assert_eq!(engine.plan_passes(&idle(&[])).start, None, "once per wait");
    let running = [id.clone()];
    assert!(
        engine.plan_passes(&idle(&running)).abandon.is_empty(),
        "its row still waits"
    );
    assert!(engine.item_action(&id, ItemAction::Pause));
    assert_eq!(
        engine.plan_passes(&idle(&running)).abandon,
        running,
        "paused: its pass stops"
    );
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
        s.items[0].status.as_str() == "passing" && s.items[0].open_urls.len() == 2
    })
    .await;
    let page = "http://ddownload.com/dd/ddownload-gofile";
    assert_eq!(s.items[0].open_urls[0], page);

    engine.page_later(&id, page, "ddownload: limit", Some(3600));
    tokio::time::sleep(Duration::from_millis(600)).await;
    let s = engine.snapshot();
    assert_eq!(
        s.items[0].status.as_str(),
        "passing",
        "{}",
        s.items[0].error
    );
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
    let mut options = through_stand_in(&t);
    if let Some(cfg) = options.config.as_mut() {
        // Used like any other host though its account isn't set.
        cfg.accountless_hosts = vec!["rapidgator.net".into()];
    }
    let engine = Engine::start(options).await.unwrap();
    engine.set_pages_pass(true);
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
        s.items[0].status.as_str() == "captcha"
    })
    .await;
    assert_eq!(s.counts.need_you, 1);
    assert_eq!(
        engine.page_for(&id).as_deref(),
        Some("https://rapidgator.net/file/person")
    );
}

/// A page that wanted a person is passed again by itself, after 5, 20 and
/// 60 minutes, telling nobody; once the tries are used up the person is
/// told, once, and it stays for them. In between it waits as before.
#[tokio::test]
async fn a_page_that_wants_a_person_is_tried_again_by_itself() {
    let t = Setup::new().await;
    let engine = Engine::start(through_stand_in(&t)).await.unwrap();
    engine.set_pages_pass(true);
    let page = "http://ddownload.com/dd/abc123";
    let mut events = engine.subscribe();
    let id = ddownload_waiting_its_pass(&engine, page).await;

    assert!(
        !engine.page_wants_person(&id),
        "nobody is told while it has tries of its own"
    );
    // Waiting for its own next try: in no list of the person's.
    let s = wait_for(&engine, "the row waiting for its own try", |s| {
        s.items[0].status.as_str() == "waiting"
    })
    .await;
    assert_eq!(s.items[0].list.map(|l| l.as_str()), Some("active"));
    assert_eq!(s.counts.need_you, 0);
    assert_eq!(s.items[0].open_urls, [page], "Open here has its page");
    assert!(
        (290..=300).contains(&s.items[0].retry_in),
        "{}",
        s.items[0].retry_in
    );
    while events.try_recv().is_ok() {}

    for round in 1..=3 {
        assert!(engine.debug_retry_now(&id), "try {round}");
        let s = wait_for(&engine, "the page's next pass", |s| {
            s.items[0].status.as_str() == "passing"
        })
        .await;
        assert_eq!(s.items[0].retry_in, 0, "a try under way");
        assert_eq!(auto_retries(&engine, &id), round);
        assert_eq!(
            engine.page_wants_person(&id),
            round == 3,
            "told once its last try wanted a person too"
        );
        let back = if round < 3 { "waiting" } else { "captcha" };
        let s = wait_for(&engine, "the row back", |s| {
            s.items[0].status.as_str() == back
        })
        .await;
        assert_eq!(s.items[0].open_urls, [page]);
        if round < 3 {
            let wait = [1200, 3600][round as usize - 1];
            assert!(
                (wait - 10..=wait).contains(&s.items[0].retry_in),
                "try {round} is not the last, and the wait grows: {}",
                s.items[0].retry_in
            );
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
    assert!(
        engine.page_wants_person(&id),
        "no try of its own: told at once"
    );
    let s = wait_for(&engine, "the row under Needs you", |s| {
        s.items[0].status.as_str() == "captcha"
    })
    .await;
    assert_eq!(s.items[0].retry_in, 0, "turned off");
    let on = serde_json::json!({"retry_failed_minutes": 1});
    engine.update_settings(on.as_object().unwrap()).unwrap();
    let s = engine.snapshot();
    assert!(
        (290..=300).contains(&s.items[0].retry_in),
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
    let under_needs_you =
        |s: &Snapshot| matches!(s.items[0].status.as_str(), "captcha" | "waiting");
    assert!(!engine.page_wants_person(&id));
    wait_for(&engine, "the row under Needs you", under_needs_you).await;
    assert!(engine.debug_retry_now(&id));
    wait_for(&engine, "its pass", |s| {
        s.items[0].status.as_str() == "passing"
    })
    .await;
    assert!(!engine.page_wants_person(&id));
    wait_for(&engine, "the row back", under_needs_you).await;
    assert_eq!(auto_retries(&engine, &id), 1);

    // Tried again by hand: the page's first pass again, with its tries of
    // its own before the person is told again.
    assert!(engine.item_action(&id, ItemAction::RetryManual));
    let s = wait_for(&engine, "its pass", |s| {
        s.items[0].status.as_str() == "passing"
    })
    .await;
    assert_eq!(s.items[0].retry_in, 0);
    assert_eq!(auto_retries(&engine, &id), 0);
    assert!(!engine.page_wants_person(&id), "its own tries first");

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
    assert!(!engine.page_wants_person(&other));
    wait_for(&engine, "the other row waiting", |s| {
        s.items
            .iter()
            .any(|i| i.id == other && i.status.as_str() == "waiting")
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
    assert!(!engine.page_wants_person(&id));
    wait_for(&engine, "the row waiting", |s| {
        s.items[0].status.as_str() == "waiting"
    })
    .await;
    assert!(engine.debug_retry_now(&id));
    wait_for(&engine, "the try to come due", |s| {
        s.items[0].status.as_str() == "passing"
    })
    .await;

    engine.page_with_person(&id);
    let s = engine.snapshot();
    assert_eq!(
        s.items[0].status.as_str(),
        "waiting",
        "back with the person, its tries ahead"
    );
    assert!(s.items[0].retry_in > 280, "{}", s.items[0].retry_in);
    assert_eq!(auto_retries(&engine, &id), 0, "that try did not count");
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(engine.snapshot().items[0].status, "waiting");

    // A row on its first pass is not the person's yet: nothing changes.
    let other = ddownload_waiting_its_pass(&engine, "http://ddownload.com/dd/def456").await;
    engine.page_with_person(&other);
    let s = engine.snapshot();
    let row = s.items.iter().find(|i| i.id == other).unwrap();
    assert_eq!(row.status.as_str(), "passing");
}

/// What cannot succeed by itself is not tried again: a page on a host now
/// off for want of its account, a mirror marked gone, a page only a person
/// can do, or a page that shows all it will and wants a click on it.
#[tokio::test]
async fn a_page_that_cannot_pass_by_itself_is_not_tried_again() {
    let t = Setup::new().await;
    let mut options = through_stand_in(&t);
    if let Some(cfg) = options.config.as_mut() {
        // Used like any other host though its account isn't set.
        cfg.accountless_hosts = vec!["rapidgator.net".into()];
    }
    let engine = Engine::start(options).await.unwrap();
    engine.set_pages_pass(true);

    // A host off for want of its account.
    engine.set_disabled("gofile.io", false).unwrap();
    engine.add_text("https://gofile.io/d/abc123", "");
    let s = wait_for(&engine, "the page to wait its turn", |s| {
        s.items.iter().any(|i| i.status.as_str() == "passing")
    })
    .await;
    let id = s.items[0].id.clone();
    assert!(!engine.page_wants_person(&id));
    wait_for(&engine, "the row under Needs you", |s| {
        s.items[0].status.as_str() == "waiting" && s.items[0].retry_in > 0
    })
    .await;
    engine.set_disabled("gofile.io", true).unwrap();
    assert!(engine.debug_retry_now(&id));
    let s = wait_for(&engine, "the try to be dropped", |s| {
        s.items[0].retry_in == 0
    })
    .await;
    assert_ne!(s.items[0].status.as_str(), "passing");
    assert_eq!(auto_retries(&engine, &id), 0);
    assert!(engine.item_action(&id, ItemAction::Remove));

    // A mirror marked gone, leaving a page only a person can do.
    engine.add_text("http://fileguard.cc/vault/ddownload-rapidgator", "");
    let w = wait_for(&engine, "the wrapper", |s| {
        s.wrappers.iter().any(|w| w.status.as_str() == "done")
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
            && s.items[0].status.as_str() == "captcha"
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
        s.items[0].status.as_str() == "captcha"
    })
    .await;
    assert_eq!(s.items[0].retry_in, 0, "another look shows the same");
}

/// The gate the browser finds on the page is the one the row names, as what
/// it loaded is surer than Haul's read of the page's code.
#[tokio::test]
async fn the_row_names_the_gate_the_browser_found() {
    use haul_core::hosts::gates::Gate;
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.set_pages_pass(true);
    let page = "https://filecrypt.cc/Container/other-gate.html";
    engine.add_text(page, "");
    let s = wrapper_settled(&engine, "passing").await;
    let id = s.wrappers[0].id.clone();
    engine.page_shows_gate(&id, page, Gate::Cutcaptcha);
    assert!(!engine.page_wants_person(&id), "tried again first");
    let s = wrapper_settled(&engine, "waiting").await;
    let said = &s.wrappers[0].error;
    assert!(
        said.contains("gate Haul cannot answer (a CutCaptcha puzzle)"),
        "{said}"
    );
}

/// A pass whose browser never showed the page is the browser's failure, a
/// state of its own (plan BROWSER-03): the row says which, nobody is asked
/// for it, the person hears of it once, and it is passed again later using
/// none of the page's own tries.
#[tokio::test]
async fn a_pass_that_could_not_start_says_so() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.set_pages_pass(true);
    let page = "https://filecrypt.cc/Container/other-gate.html";
    engine.add_text(page, "");
    let s = wrapper_settled(&engine, "passing").await;
    let id = s.wrappers[0].id.clone();
    assert!(
        engine.page_not_passed(&id, "the browser exited at start (exit code: 0)"),
        "told once"
    );
    let s = wrapper_settled(&engine, "waiting").await;
    let said = &s.wrappers[0].error;
    assert!(
        said.contains("Haul's browser exited as it started (the browser exited at start"),
        "{said}"
    );
    assert!(s.wrappers[0].retry_in > 0, "tried again later");
    assert_eq!(s.counts.need_you, 0, "nobody's job");
    assert_eq!(engine.waiting_for_person(), (vec![], vec![]));
    // Its retry passes the page again, in turn, with no try of its own used.
    assert!(engine.debug_retry_now(&id));
    wrapper_settled(&engine, "passing").await;
    assert_eq!(auto_retries(&engine, &id), 0);
    assert!(
        !engine.page_not_passed(&id, "the browser exited at start (exit code: 0)"),
        "not told again"
    );
}

/// A page Haul's browser kept closing by itself, in every try it gets, fails
/// as the browser's failure with its reason: not under Needs you, and tried
/// again on its timer.
#[tokio::test]
async fn a_page_the_browser_kept_closing_is_not_handed_to_a_person() {
    use haul_core::pass_ends::{Closed, Next, Process, Tries};
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.set_pages_pass(true);
    engine.add_text("https://filecrypt.cc/Container/other-gate.html", "");
    let s = wrapper_settled(&engine, "passing").await;
    let id = s.wrappers[0].id.clone();
    let mut tries = Tries::new(vec!["Edge".into(), "Chrome".into()]);
    let closed = || {
        Closed::NoDevTools(Process {
            exited: Some("exit code: 0".into()),
            answers: None,
        })
    };
    let reason = loop {
        match tries.ended(closed()) {
            Next::Again { .. } => {}
            Next::Fail(reason) => break reason,
            other => panic!("{other:?}"),
        }
    };
    engine.page_failed(&id, &reason);
    let s = wrapper_settled(&engine, "error").await;
    assert_eq!(s.wrappers[0].error, reason);
    assert!(s.wrappers[0].retry_in > 0, "tried again on its timer");
    assert_eq!(
        engine.waiting_for_person(),
        (vec![], vec![]),
        "nobody is asked"
    );
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
        s.items.iter().any(|i| i.status.as_str() == "passing")
    })
    .await;
    engine.page_failed(&s.items[0].id, "answered Not Found");
    let s = wait_for(&engine, "the download to fail", |s| {
        s.items.iter().all(|i| i.status.as_str() == "error")
    })
    .await;
    assert!(s.items[0].error.contains("answered Not Found"));
    assert!(s.items[0].retry_in > 0, "tried again on its timer");
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
    assert_eq!((h.kind.as_str(), h.group.as_str()), ("site", "site"));
}

/// A mirror only a person can use is no improvement: the download stays, and
/// the row says its other hosts are there.
#[tokio::test]
async fn a_mirror_that_needs_a_person_is_not_an_improvement() {
    let t = Setup::new().await;
    let mut cfg = quick(&t);
    cfg.endpoints.rapidgator = t.ip.clone();
    cfg.accountless_hosts = vec!["rapidgator.net".into()];
    let engine = t.engine_with(cfg).await;
    engine.set_preferred("127.0.0.1", true).unwrap();
    engine.add_text(
        &format!(
            "{}/paced/40/x.bin https://rapidgator.net/file/abc123/x.bin.html",
            t.ip
        ),
        "",
    );
    engine.add_text(&format!("{}/paced/200/h.bin", t.name), "");
    let s = wait_for(&engine, "the row to say so", |s| {
        row(s, "x.bin").slowed.is_some()
    })
    .await;
    let it = row(&s, "x.bin");
    assert_eq!(
        it.slowed.as_deref(),
        Some("Slowed by 127.0.0.1: its other hosts need you")
    );
    assert_eq!(
        (it.status.as_str(), it.list.map(|l| l.as_str())),
        ("downloading", Some("active"))
    );
    assert_eq!(it.other_hosts, 1);
    assert_eq!(s.counts.need_you, 0);
    // Never left, rather than turned away there and come back.
    assert_eq!(t.server.requests("/paced/40/x.bin").len(), 1);
}

/// A mirror that turns out to want a person after all sends the row back to
/// the slow one rather than leave it under Needs you.
#[tokio::test]
async fn a_row_goes_back_when_its_next_mirror_wants_a_person() {
    let t = Setup::new().await;
    let engine = t.engine_with(quick(&t)).await;
    engine.set_preferred("127.0.0.1", true).unwrap();
    // A web page where the file should be.
    engine.add_text(&format!("{}/paced/40/x.bin {}/dd/x.bin", t.ip, t.name), "");
    engine.add_text(&format!("{}/paced/200/h.bin", t.name), "");
    let s = wait_for(&engine, "the row to go back", |s| {
        t.server.requests("/paced/40/x.bin").len() == 2 && row(s, "x.bin").status == "downloading"
    })
    .await;
    let it = row(&s, "x.bin");
    assert_eq!(it.shown_host, "127.0.0.1");
    assert_eq!(it.list.map(|l| l.as_str()), Some("active"));
    assert_eq!(it.open_urls.len(), 1, "the page stays there for a person");
}

/// The other mirror is a page the desktop app passes and the slow one is a
/// known host: without a turn of its own the row would sit queued for the
/// whole cooldown, never passing that page.
#[tokio::test]
async fn a_row_leaving_a_slow_host_gets_its_page_passed() {
    let t = Setup::new().await;
    let mut options = through_stand_in(&t);
    let cfg = options.config.take().unwrap();
    options.config = Some(Config {
        throttling: quick(&t).throttling,
        ..cfg
    });
    let engine = Engine::start(options).await.unwrap();
    engine.set_pages_pass(true);
    engine.add_text("http://fileguard.cc/vault/ddownload-slow", "");
    let w = wrapper_settled(&engine, "done").await.wrappers[0]
        .id
        .clone();
    wait_for(&engine, "the file", |s| s.items.len() == 1).await;
    engine.start_wrapper(&w);
    // Its page wants a browser, so the host measured no worse goes first.
    wait_for(&engine, "the slow host", |_| {
        !t.server.requests("/paced/40/held.bin").is_empty()
    })
    .await;
    engine.add_text("http://files.example/paced/200/h.bin", "");
    let s = wait_for(&engine, "the page's turn", |s| {
        s.items[0].status.as_str() == "passing"
    })
    .await;
    assert_eq!(s.items[0].shown_host, "ddownload.com");
    assert!(cooldown(&s, "fileditchfiles.st") > 0);
    assert_eq!(t.server.requests("/paced/40/held.bin").len(), 1);
}

/// A preferred host whose page the desktop app passes goes before a host that
/// isn't preferred, though Haul knows that one and measured neither: the row
/// waits for the page's turn.
#[tokio::test]
async fn a_preferred_page_goes_before_a_host_that_isnt() {
    let t = Setup::new().await;
    let mut options = through_stand_in(&t);
    if let Some(cfg) = options.config.as_mut() {
        cfg.preferred_hosts = vec!["ddownload.com".into()];
    }
    let engine = Engine::start(options).await.unwrap();
    engine.set_pages_pass(true);
    engine.add_text("http://fileguard.cc/vault/ddownload-slow", "");
    let w = wrapper_settled(&engine, "done").await.wrappers[0]
        .id
        .clone();
    wait_for(&engine, "the file", |s| s.items.len() == 1).await;
    engine.start_wrapper(&w);
    let s = wait_for(&engine, "the page's turn", |s| {
        s.items[0].status.as_str() == "passing"
    })
    .await;
    assert_eq!(s.items[0].shown_host, "ddownload.com");
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert!(t.server.requests("/paced/40/held.bin").is_empty());
}

/// The row's own host is the one serving it, not a preferred mirror: a file a
/// page hands over, on an address of another domain, belongs to that page's
/// host, and a mirror reported deleted stays out.
#[tokio::test]
async fn a_handed_file_shows_under_the_host_it_came_from() {
    let t = Setup::new().await;
    let engine = Engine::start(through_stand_in(&t)).await.unwrap();
    engine.set_pages_pass(true);
    engine.set_disabled("gofile.io", false).unwrap();
    // The host the row would name if it went by preference.
    engine.set_preferred("gofile.io", true).unwrap();
    engine.add_text("http://fileguard.cc/vault/ddownload-gofile", "");
    let w = wrapper_settled(&engine, "done").await.wrappers[0]
        .id
        .clone();
    let s = wait_for(&engine, "the file", |s| s.items.len() == 1).await;
    let id = s.items[0].id.clone();
    engine.start_wrapper(&w);
    // The preferred host's page goes first; ddownload, not preferred, waits.
    wait_for(&engine, "the preferred page to wait", |s| {
        s.items[0].status.as_str() == "passing"
            && s.items[0].open_urls == ["https://gofile.io/d/both"]
    })
    .await;
    assert_eq!(
        engine.page_for(&id).as_deref(),
        Some("https://gofile.io/d/both")
    );
    engine.page_gone(&id, "the file was deleted");
    let s = wait_for(&engine, "its other page", |s| {
        s.items[0].status.as_str() == "passing" && s.items[0].open_urls.len() == 1
    })
    .await;
    assert_eq!(s.items[0].shown_host, "ddownload.com");
    let file = haul_core::hosts::Entry {
        url: "http://files.example/paced/200/handed.bin".to_string(),
        name: Some("handed.bin".to_string()),
        ..Default::default()
    };
    engine.set_files(&id, vec![file]).unwrap();
    let s = wait_for(&engine, "the transfer", |s| {
        s.items[0].status.as_str() == "downloading"
    })
    .await;
    let it = &s.items[0];
    assert_eq!(it.shown_host, "ddownload.com", "the host it came from");
    assert!(!it.preferred_host);
    assert_eq!(it.other_hosts, 0, "gofile.io's copy was deleted");
    assert_eq!(active(&s, "ddownload.com"), 1);
    assert_eq!(active(&s, "gofile.io"), 0);
    let s = wait_for(&engine, "done", all_done).await;
    assert_eq!(s.items[0].shown_host, "ddownload.com");
    let host = |name: &str| s.hosts.iter().find(|h| h.name == name).unwrap();
    assert_eq!((host("ddownload.com").ok, host("gofile.io").ok), (1, 0));
}

/// A handed file crawling is held against the host it came from, not its
/// file's domain: that host goes on cooldown and no other of its pages opens.
#[tokio::test]
async fn a_slow_handed_file_is_blamed_on_the_host_it_came_from() {
    let t = Setup::new().await;
    let mut options = through_stand_in(&t);
    let cfg = options.config.take().unwrap();
    options.config = Some(Config {
        throttling: quick(&t).throttling,
        ..cfg
    });
    let engine = Engine::start(options).await.unwrap();
    engine.set_pages_pass(true);
    let page = "http://ddownload.com/dd/abc123";
    let id = ddownload_waiting_its_pass(&engine, page).await;
    let file = haul_core::hosts::Entry {
        url: "http://files.example/paced/40/x.bin".to_string(),
        name: Some("x.bin".to_string()),
        ..Default::default()
    };
    engine.set_files(&id, vec![file]).unwrap();
    engine.add_text("http://other.example/paced/200/h.bin", "");
    let s = wait_for(&engine, "the row to say so", |s| {
        s.items.iter().any(|i| i.slowed.is_some())
    })
    .await;
    let it = s.items.iter().find(|i| i.id == id).unwrap();
    assert_eq!(
        it.slowed.as_deref(),
        Some("Slowed by ddownload.com: no other host has it")
    );
    assert!(cooldown(&s, "ddownload.com") > 0);
    assert!(!engine.page_passes("http://ddownload.com/dd/def456"));
    // A row with nowhere else to go passes its page all the same, and one with
    // another host goes there.
    let alone = "http://ddownload.com/dd/def456";
    ddownload_waiting_its_pass(&engine, alone).await;
    assert!(engine.page_passes(alone));
    engine.set_disabled("gofile.io", false).unwrap();
    engine.add_text("http://fileguard.cc/vault/ddownload-gofile", "");
    let w = wrapper_settled(&engine, "done").await.wrappers[0]
        .id
        .clone();
    engine.start_wrapper(&w);
    let s = wait_for(&engine, "the other page's turn", |s| {
        s.items
            .iter()
            .any(|i| i.name == "ddownload-gofile.rar" && i.status.as_str() == "passing")
    })
    .await;
    let both = row(&s, "ddownload-gofile.rar");
    assert_eq!(both.open_urls, ["https://gofile.io/d/both"]);
    assert!(!engine.page_passes("http://ddownload.com/dd/ddownload-gofile"));
    assert!(engine.page_passes("https://gofile.io/d/both"));
    assert_eq!(
        engine.page_for(&both.id).as_deref(),
        Some("https://gofile.io/d/both")
    );
    assert!(s.hosts.iter().all(|h| h.name != "files.example"));
}

/// Where the desktop app passes pages, a page on a site Haul has no code for
/// that wants a browser waits for its pass, which looks for its download; one
/// only seen on the clipboard is never opened on its own.
#[tokio::test]
async fn an_unknown_page_is_passed_only_when_the_person_added_it() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    engine.set_pages_pass(true);
    let page = format!("{}/page/added", t.ip);
    engine.add_text(&page, "");
    wait_for(&engine, "the page to wait for its pass", |s| {
        s.items.iter().any(|i| i.status.as_str() == "passing")
    })
    .await;
    assert!(engine.page_passes(&page));

    let copied = format!("{}/page/copied", t.ip);
    engine.add_from_clipboard(&copied);
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert!(!engine.page_passes(&copied), "never opened on its own");
}

/// The friction count (plan §4.7): a page only a person passes is an ask,
/// with its site, its gate and whether the release needed it; the
/// notification about it and a retry by hand count for the same release.
#[tokio::test]
async fn a_person_asked_counts_in_the_friction_count() {
    let t = Setup::new().await;
    let engine = t.engine().await;
    let mut events = engine.subscribe();
    engine.add_text(&format!("{}/gate/recaptcha", t.ip), "");
    let s = wait_for(&engine, "under Needs you", |s| {
        s.items
            .first()
            .is_some_and(|i| i.status.as_str() == "captcha")
    })
    .await;
    let id = s.items[0].id.clone();
    let friction = engine.friction();
    assert_eq!(friction.len(), 1, "{friction:?}");
    let [ask] = friction[0].asks.as_slice() else {
        panic!("one ask: {friction:?}");
    };
    assert_eq!(
        (ask.row.as_str(), ask.site.as_str()),
        (id.as_str(), "127.0.0.1")
    );
    assert_eq!(ask.gate, Some("reCAPTCHA"), "{ask:?}");
    assert!(ask.needed, "its only link");
    // What the desktop app tells of it, and shows.
    let event = events.recv().await.unwrap();
    let told = engine.tellings(&[event]);
    assert_eq!(told.len(), 1, "{told:?}");
    assert_eq!(told[0].rows, std::slice::from_ref(&id));
    assert_eq!(engine.notified(&told[0].rows), std::slice::from_ref(&id));
    engine.item_action(&id, ItemAction::RetryManual);
    let friction = engine.friction();
    assert_eq!((friction[0].notifications, friction[0].clicks), (1, 1));
    let report = engine.diagnostics(serde_json::Value::Null);
    assert_eq!(report["friction"][0]["asks"][0]["gate"], "reCAPTCHA");
}
