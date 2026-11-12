//! Every kind of site Haul supports, in one table: what each one's link leads
//! to, against local stand-ins that answer the way that kind of site does.
//! This is the model half of the host suite (`docs/hosts.md`); the live half
//! is the scorecard skill, run on real links on the user's machine.
//!
//! A new host, or an exception found on a real one, becomes a row here. The
//! rule the suite keeps: an exception changes a general rule, and every older
//! row must still pass. When a row fails, the whole table is printed, so the
//! regression shows beside everything that still works.

use std::time::{Duration, Instant};

use haul_core::{Engine, Options, Snapshot, config::Endpoints};

#[allow(dead_code)]
mod support;

use support::*;

/// What a link comes to, as the person would see it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    /// The file downloaded, with the bytes the stand-in sent.
    File,
    /// A wrapper or page that listed this many files.
    Listed(usize),
    /// Under Needs you: a page or a captcha only a person passes.
    NeedsYou,
    /// Failed, and the reason says the file or its list is gone.
    Gone,
    /// Failed for another reason.
    Failed,
    /// Waiting for a wait the host asked for.
    Waits,
}

/// How the engine reaches the stand-in: straight at its address, through
/// it as a proxy, so a real host's address (`https://pixeldrain.com/…`) gets
/// the stand-in's answer, or in stand-in mode (`HAUL_STAND_IN`), where
/// `<site>.localhost:<port>` is `<site>` and every real address goes there,
/// as in a test copy of the desktop app.
#[derive(Clone, Copy)]
enum Reach {
    Direct,
    AsProxy,
    StandIn,
}

struct Case {
    /// The kind of site, as the scorecard names it.
    site: &'static str,
    /// The link, given the stand-in's address.
    link: fn(&Setup) -> String,
    reach: Reach,
    expect: Outcome,
    /// The file a `File` outcome must hold, or one a `Listed` outcome must
    /// then download: its name at the stand-in.
    file: Option<&'static str>,
    /// Words its reason must hold, such as the gate it names.
    says: Option<&'static str>,
}

fn cases() -> Vec<Case> {
    use Outcome::*;
    use Reach::*;
    vec![
        // Any site, with no code of its own: the general way.
        Case {
            site: "any site: a direct file",
            link: |t| format!("{}/files/plain.bin", t.ip),
            reach: Direct,
            expect: File,
            file: Some("plain.bin"),
            says: None,
        },
        Case {
            site: "any site: a file without its size",
            link: |t| format!("{}/nolength/unsized.bin", t.ip),
            reach: Direct,
            expect: File,
            file: Some("unsized.bin"),
            says: None,
        },
        Case {
            site: "any site: a refused link",
            link: |t| format!("{}/refuse/x.bin", t.ip),
            reach: Direct,
            expect: Failed,
            file: None,
            says: None,
        },
        Case {
            site: "any site: a page only a person passes",
            link: |t| format!("{}/page/x", t.ip),
            reach: Direct,
            expect: NeedsYou,
            file: None,
            says: None,
        },
        Case {
            site: "any site: a page that lists links",
            link: |t| format!("{}/blog/links", t.ip),
            reach: Direct,
            expect: Listed(2),
            file: None,
            says: None,
        },
        Case {
            site: "any site: a short rate limit, then the file",
            link: |t| format!("{}/limited/later.bin", t.ip),
            reach: Direct,
            expect: File,
            file: Some("later.bin"),
            says: None,
        },
        Case {
            site: "any site: a long rate limit",
            link: |t| format!("{}/limited-long/later.bin", t.ip),
            reach: Direct,
            expect: Waits,
            file: None,
            says: None,
        },
        Case {
            site: "any site: a post with its archive's password",
            link: |t| format!("{}/blog/with-password", t.ip),
            reach: Direct,
            expect: Listed(2),
            file: None,
            says: None,
        },
        // Gates, known by their own marks whatever the site.
        Case {
            site: "any site: behind reCAPTCHA",
            link: |t| format!("{}/gate/recaptcha", t.ip),
            reach: Direct,
            expect: NeedsYou,
            file: None,
            says: Some("shows reCAPTCHA"),
        },
        Case {
            site: "any site: behind hCaptcha",
            link: |t| format!("{}/gate/hcaptcha", t.ip),
            reach: Direct,
            expect: NeedsYou,
            file: None,
            says: Some("shows hCaptcha"),
        },
        Case {
            site: "any site: behind Turnstile",
            link: |t| format!("{}/gate/turnstile", t.ip),
            reach: Direct,
            expect: NeedsYou,
            file: None,
            says: Some("shows Cloudflare Turnstile"),
        },
        Case {
            site: "any site: behind Cloudflare's check",
            link: |t| format!("{}/cf/managed", t.ip),
            reach: Direct,
            expect: NeedsYou,
            file: None,
            says: Some("(Cloudflare's, managed)"),
        },
        // Hosts with code of their own.
        Case {
            site: "pixeldrain.com",
            link: |_| "https://pixeldrain.com/u/abc".into(),
            reach: Direct,
            expect: File,
            file: None,
            says: None,
        },
        Case {
            site: "1fichier.com: a deleted file",
            link: |_| "https://1fichier.com/?gone123".into(),
            reach: Direct,
            expect: Gone,
            file: None,
            says: None,
        },
        Case {
            site: "rapidgator.net, without its account",
            link: |_| "https://rapidgator.net/file/abc/x.rar.html".into(),
            reach: AsProxy,
            expect: NeedsYou,
            file: None,
            says: None,
        },
        Case {
            site: "gofile.io, without the desktop app",
            link: |_| "https://gofile.io/d/abc123".into(),
            reach: AsProxy,
            expect: NeedsYou,
            file: None,
            says: None,
        },
        // Wrappers and pastes.
        Case {
            site: "fileguard.cc",
            link: |_| "https://fileguard.cc/vault/abc".into(),
            reach: Direct,
            expect: Listed(1),
            file: None,
            says: None,
        },
        Case {
            site: "filecrypt.cc",
            link: |_| "https://filecrypt.cc/Container/open.html".into(),
            reach: Direct,
            expect: Listed(2),
            file: None,
            says: None,
        },
        Case {
            site: "paste2.org: a paste that lists no links",
            link: |_| "http://paste2.org/paste/abc123".into(),
            reach: AsProxy,
            expect: Gone,
            file: None,
            says: None,
        },
        // Stand-in mode: a stand-in claims a real site by its name.
        Case {
            site: "stand-in filecrypt.cc, by its .localhost address",
            link: |t| t.stand_in("filecrypt.cc", "/Container/real-links.html"),
            reach: StandIn,
            expect: Listed(2),
            file: Some("one.bin"),
            says: None,
        },
        Case {
            site: "stand-in filecrypt.cc, by its real address",
            link: |_| "https://www.filecrypt.cc/Container/real-links.html".into(),
            reach: StandIn,
            expect: Listed(2),
            file: Some("two.bin"),
            says: None,
        },
        Case {
            site: "stand-in paste2.org: a paste that lists no links",
            link: |t| t.stand_in("paste2.org", "/paste/abc123"),
            reach: StandIn,
            expect: Gone,
            file: None,
            says: None,
        },
    ]
}

impl Setup {
    /// `path` on `site`'s stand-in, in stand-in mode.
    fn stand_in(&self, site: &str, path: &str) -> String {
        format!("http://{site}.localhost:{}{path}", self.port())
    }

    fn port(&self) -> u16 {
        self.ip
            .rsplit(':')
            .next()
            .and_then(|p| p.parse().ok())
            .unwrap()
    }
}

/// Where `snapshot` has left the case's one row or wrapper, once it settled.
fn outcome(s: &Snapshot) -> Option<Outcome> {
    if let Some(w) = s.wrappers.first() {
        return match w.status.as_str() {
            "done" => Some(Outcome::Listed(w.found.map_or(0, |f| f.files))),
            "captcha" => Some(Outcome::NeedsYou),
            "error" if w.gone => Some(Outcome::Gone),
            "error" => Some(Outcome::Failed),
            _ => None,
        };
    }
    let it = s.items.first()?;
    match it.status.as_str() {
        "done" => Some(Outcome::File),
        "captcha" => Some(Outcome::NeedsYou),
        "waiting" if it.wait > 0 => Some(Outcome::Waits),
        "error" if it.gone => Some(Outcome::Gone),
        "error" => Some(Outcome::Failed),
        _ => None,
    }
}

/// What the row or wrapper said, for the table.
fn said(s: &Snapshot) -> String {
    let row = s.items.first().map(|i| i.error.clone());
    let wrapper = s.wrappers.first().map(|w| w.error.clone());
    wrapper.or(row).unwrap_or_default()
}

/// Runs one case in an engine of its own: what it came to and what the row
/// said, or why it came to nothing.
async fn run(case: &Case) -> Result<(Outcome, String), String> {
    let t = Setup::new().await;
    let options = match case.reach {
        Reach::Direct | Reach::StandIn => t.options(),
        Reach::AsProxy => {
            let mut config = t.config();
            config.proxies = vec![t.ip.clone()];
            Options {
                config: Some(config),
                ..t.options()
            }
        }
    };
    let mut options = options;
    if let Some(config) = options.config.as_mut() {
        config.endpoints.rapidgator = t.ip.clone();
    }
    // Stand-in mode is one for the whole process: the rows run one at a
    // time, the other tests here wait their turn, and it is off again
    // before the next.
    let stand_in = matches!(case.reach, Reach::StandIn);
    if stand_in && let Some(config) = options.config.as_mut() {
        config.endpoints = Endpoints::default();
    }
    let _one = ONE_AT_A_TIME.lock().await;
    haul_core::hosts::set_stand_in(stand_in.then(|| t.port()));
    let got = settle(case, &t, options).await;
    haul_core::hosts::set_stand_in(None);
    got
}

/// What a test holds while it uses stand-in mode, or one that must not
/// see it.
static ONE_AT_A_TIME: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// [`run`]'s engine, from start to where the case's row settled.
async fn settle(case: &Case, t: &Setup, options: Options) -> Result<(Outcome, String), String> {
    let engine = Engine::start(options).await.map_err(|e| e.to_string())?;
    engine.add_text(&(case.link)(t), "");
    let deadline = Instant::now() + Duration::from_secs(20);
    let got = loop {
        let s = engine.snapshot();
        if let Some(got) = outcome(&s) {
            break (got, said(&s));
        }
        if Instant::now() > deadline {
            let row = s
                .items
                .first()
                .map(|i| format!("{} {}", i.status.as_str(), i.error));
            let wrapper = s
                .wrappers
                .first()
                .map(|w| format!("{} {}", w.status.as_str(), w.error));
            engine.shutdown().await;
            return Err(format!("never settled: {row:?} {wrapper:?}"));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    if matches!(got.0, Outcome::Listed(_)) && case.file.is_some() {
        // What the wrapper listed downloads, every file of it, once started.
        let wrapper = engine.snapshot().wrappers[0].id.clone();
        engine.start_wrapper(&wrapper);
        while !all_done(&engine.snapshot()) {
            if Instant::now() > deadline {
                let rows: Vec<String> = engine
                    .snapshot()
                    .items
                    .iter()
                    .map(|i| format!("{} {} {}", i.name, i.status.as_str(), i.error))
                    .collect();
                engine.shutdown().await;
                return Err(format!(
                    "{:?}, but its files never downloaded: {rows:?}",
                    got.0
                ));
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
    if matches!(got.0, Outcome::File | Outcome::Listed(_))
        && let Some(name) = case.file
        && t.downloaded(name) != content(name, SIZE)
    {
        engine.shutdown().await;
        return Err(format!("{name} holds other bytes"));
    }
    engine.shutdown().await;
    Ok(got)
}

#[tokio::test]
async fn every_kind_of_site_comes_to_what_it_should() {
    let mut table = Vec::new();
    let mut failed = 0;
    for case in cases() {
        let got = run(&case).await;
        let ok = got
            .as_ref()
            .is_ok_and(|(g, said)| *g == case.expect && case.says.is_none_or(|w| said.contains(w)));
        if !ok {
            failed += 1;
        }
        table.push(format!(
            "{} {:<45} expected {:<11} got {}",
            if ok { "pass" } else { "FAIL" },
            case.site,
            format!("{:?}", case.expect),
            match got {
                Ok((g, said)) if said.is_empty() => format!("{g:?}"),
                Ok((g, said)) => format!("{g:?} ({said})"),
                Err(e) => e,
            }
        ));
    }
    let table = table.join("\n");
    assert_eq!(failed, 0, "{failed} of the suite's rows failed:\n{table}");
    println!("{table}");
}

/// In stand-in mode a file is known by its real address, whichever form
/// it was added in and wherever its stand-in answered from: two links to
/// one file, one in each form, are one row with one link.
#[tokio::test]
async fn a_stand_in_s_file_keeps_its_real_address() {
    let t = Setup::new().await;
    let mut config = t.config();
    config.endpoints = Endpoints::default();
    let _one = ONE_AT_A_TIME.lock().await;
    haul_core::hosts::set_stand_in(Some(t.port()));
    let engine = Engine::start(Options {
        config: Some(config),
        ..t.options()
    })
    .await
    .unwrap();
    let real = "https://site.example/files/plain.bin";
    engine.add_text(
        &format!("{}\n{real}", t.stand_in("site.example", "/files/plain.bin")),
        "",
    );
    let deadline = Instant::now() + Duration::from_secs(20);
    let insides = loop {
        let insides = engine.debug_inspect();
        let items = insides["items"].as_array().cloned().unwrap_or_default();
        if items.iter().any(|i| i["status"] == "done") || Instant::now() > deadline {
            break items;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    engine.shutdown().await;
    haul_core::hosts::set_stand_in(None);
    assert_eq!(insides.len(), 1, "one row: {insides:#?}");
    assert_eq!(insides[0]["status"], "done", "{insides:#?}");
    assert_eq!(
        insides[0]["mirrors"],
        serde_json::json!([real]),
        "{insides:#?}"
    );
    assert_eq!(t.downloaded("plain.bin"), content("plain.bin", SIZE));
}

/// A clearance a browser got from a stand-in is kept for the stand-in,
/// where Haul's requests for the real site go in stand-in mode.
#[tokio::test]
async fn a_stand_in_s_clearance_is_kept_where_requests_go() {
    use reqwest::cookie::CookieStore;
    let _one = ONE_AT_A_TIME.lock().await;
    haul_core::hosts::set_stand_in(Some(8799));
    let http = haul_core::hosts::Http::new(None, "Haul").unwrap();
    http.add_clearance(
        "https://filecrypt.cc/Container/AAAAAAAA.html",
        &["cf_clearance=abc123; Path=/".to_string()],
        None,
    );
    haul_core::hosts::set_stand_in(None);
    let at = |u: &str| http.jar.cookies(&u.parse().unwrap());
    assert!(at("http://filecrypt.cc.localhost:8799/Container/BBBBBBBB.html").is_some());
    assert!(at("https://filecrypt.cc/Container/BBBBBBBB.html").is_none());
}

/// How long unpacking an archive takes, for comparing changes to how
/// archives are read. Not run with the rest: give it an archive whose parts
/// are in `HAUL_BENCH_PARTS` (paths, `;` between them) and run it with
/// `--ignored --nocapture`. What it unpacks is deleted afterwards.
#[test]
#[ignore]
fn time_an_extraction() {
    let Ok(list) = std::env::var("HAUL_BENCH_PARTS") else {
        return;
    };
    let parts: Vec<haul_core::extract::Part> = list
        .split(';')
        .map(|p| {
            let path = std::path::PathBuf::from(p);
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            haul_core::extract::Part { path, name }
        })
        .collect();
    let progress = haul_core::extract::Progress::default();
    let started = std::time::Instant::now();
    let out = haul_core::extract::extract(&parts, &[], &progress).unwrap();
    println!(
        "unpacked {} bytes in {:?}",
        progress.done.load(std::sync::atomic::Ordering::Relaxed),
        started.elapsed()
    );
    for path in out.placed {
        let _ = std::fs::remove_file(&path).or_else(|_| std::fs::remove_dir_all(&path));
    }
}
