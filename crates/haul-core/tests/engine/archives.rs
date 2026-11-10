//! Archives in parts: grouping, extraction, passwords.

use super::*;

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
                i.status.as_str(),
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
        s.items.len() == 2 && s.items.iter().all(|i| i.status.as_str() == "paused")
    })
    .await;
    assert!(s.items.iter().all(|i| i.package == "My Package"));
    assert_eq!(engine.start_wrapper(&id), Some(2));
    wait_for(&engine, "the downloads", all_done).await;
    assert_eq!(t.downloaded("My Package/one.bin"), content("one.bin", SIZE));
    assert_eq!(t.downloaded("My Package/two.bin"), content("two.bin", SIZE));
    assert_eq!(files_in(&t.root.join("dl")), ["My Package"]);
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
    assert!(s.items.iter().all(|i| i.status.as_str() == "paused"));
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
        s.items.len() == 2 && s.items.iter().all(|i| i.status.as_str() == "paused")
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
        s.items.len() == 2 && s.items.iter().all(|i| i.status.as_str() == "paused")
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
            ("Example.Archive.part1.rar".to_string(), 2),
            ("Example.Archive.part2.rar".to_string(), 2)
        ],
        "{:?}",
        s.items
    );
    assert_eq!(s.archives.len(), 1);
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
    assert!(s.items.iter().all(|i| i.status.as_str() == "done"));
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
        engine.settings().config.archive_passwords,
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
    let report = engine.diagnostics(serde_json::Value::Null).to_string();
    assert!(!report.contains("open sesame"), "{report}");
}

/// The password given with the links opens their archive with no one asked.
#[tokio::test]
async fn a_password_given_with_the_links_opens_their_archive() {
    let t = Setup::new().await;
    let engine = t
        .engine_with(Config {
            auto_extract: true,
            ..t.config()
        })
        .await;
    engine.add_text_with(
        &format!("{0}/archive/PV.part1.rar {0}/archive/PV.part2.rar", t.ip),
        "",
        Some("open sesame"),
    );
    wait_for(&engine, "extracted", |s| {
        s.items.len() == 2 && s.items.iter().all(|i| i.extract == Extract::Done)
    })
    .await;
    assert_eq!(
        std::fs::read_to_string(t.root.join("dl/Secret/note.txt")).unwrap(),
        "secret stuff\n"
    );
}

/// A post that gives its archive's password beside the links: the archive
/// opens with it, no one asked.
#[tokio::test]
async fn a_password_on_the_page_opens_the_archive_it_lists() {
    let t = Setup::new().await;
    let engine = t
        .engine_with(Config {
            auto_extract: true,
            ..t.config()
        })
        .await;
    engine.add_text(&format!("{}/blog/with-password", t.ip), "");
    let s = wait_for(&engine, "the post's parts", |s| {
        s.items.len() == 2 && s.items.iter().all(|i| i.status.as_str() == "paused")
    })
    .await;
    assert_eq!(engine.start_wrapper(&s.wrappers[0].id), Some(2));
    wait_for(&engine, "extracted", |s| {
        s.items.iter().all(|i| i.extract == Extract::Done)
    })
    .await;
    assert_eq!(
        std::fs::read_to_string(t.root.join("dl/Secret/note.txt")).unwrap(),
        "secret stuff\n"
    );
}
