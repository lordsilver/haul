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

use haul_core::{Engine, Options, Snapshot};

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

/// How the engine reaches the stand-in: straight at its address, or through
/// it as a proxy, so a real host's address (`https://pixeldrain.com/…`) gets
/// the stand-in's answer.
#[derive(Clone, Copy)]
enum Reach {
    Direct,
    AsProxy,
}

struct Case {
    /// The kind of site, as the scorecard names it.
    site: &'static str,
    /// The link, given the stand-in's address.
    link: fn(&Setup) -> String,
    reach: Reach,
    expect: Outcome,
    /// The file a `File` outcome must hold: its name at the stand-in.
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
    ]
}

/// Where `snapshot` has left the case's one row or wrapper, once it settled.
fn outcome(s: &Snapshot) -> Option<Outcome> {
    if let Some(w) = s.wrappers.first() {
        return match w.status {
            "done" => Some(Outcome::Listed(w.found.map_or(0, |f| f.files))),
            "captcha" => Some(Outcome::NeedsYou),
            "error" if w.gone => Some(Outcome::Gone),
            "error" => Some(Outcome::Failed),
            _ => None,
        };
    }
    let it = s.items.first()?;
    match it.status {
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
        Reach::Direct => t.options(),
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
    let engine = Engine::start(options).await.map_err(|e| e.to_string())?;
    engine.add_text(&(case.link)(&t), "");
    let deadline = Instant::now() + Duration::from_secs(20);
    let got = loop {
        let s = engine.snapshot();
        if let Some(got) = outcome(&s) {
            break (got, said(&s));
        }
        if Instant::now() > deadline {
            let row = s.items.first().map(|i| format!("{} {}", i.status, i.error));
            let wrapper = s
                .wrappers
                .first()
                .map(|w| format!("{} {}", w.status, w.error));
            engine.shutdown().await;
            return Err(format!("never settled: {row:?} {wrapper:?}"));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    if got.0 == Outcome::File
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
