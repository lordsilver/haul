//! The walk suite: pages that put something between a link and what it
//! leads to, each walked in a real browser by [`pass`], as the desktop app
//! walks a page on a site Haul has no code for. One scenario per kind of
//! obstacle (an ad's layer over the button, a pop-under, a cookie dialog
//! that traps clicks, a countdown, a step that opens in a new tab, a script
//! that loads late, an ad that moves the tab, a captcha that stalls), and
//! the suite grows by one whenever a real page defeats the walk.
//!
//! Everything is made up and served from this machine: the sites are
//! `<name>.localhost`, which a browser resolves to itself, and the walk
//! ends at addresses on hosts Haul knows that are never loaded. A scenario
//! marked `gap` is one the walk is known to fail: it is run, told in the
//! scorecard, and fails the suite once it passes, so the mark comes off.
//!
//! Runs where Edge or Chrome is found (Windows, or `HAUL_CHROMIUM`), and is
//! skipped elsewhere.

use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use super::*;

/// What a scenario's walk should end with.
enum Expect {
    /// The page went to, or offered as steps, these addresses on sites
    /// Haul knows.
    LedTo(&'static [&'static str]),
    /// The page, read for its links, showed these on sites Haul knows.
    Listed(&'static [&'static str]),
    /// The page handed over a file whose address ends so.
    File(&'static str),
}

struct Scenario {
    name: &'static str,
    /// Where the walk starts: `<site>/<path>`, on this machine.
    start: &'static str,
    /// The row is a page read for its links (a link added on its own),
    /// rather than one a release page listed.
    lists: bool,
    expect: Expect,
    /// The seconds the pages themselves take: their countdowns, and what
    /// else no walk can hurry. What a walk takes beyond it is its own.
    floor: u64,
    /// The walk's time, at most, in seconds.
    within: u64,
    /// A known failure, told and not counted.
    gap: bool,
}

const SCENARIOS: &[Scenario] = &[
    Scenario {
        name: "two sites, a press each",
        start: "short.localhost/plain",
        lists: true,
        expect: Expect::LedTo(&["https://pixeldrain.com/u/plain001"]),
        floor: 0,
        within: 15,
        gap: false,
    },
    Scenario {
        name: "an ad's layer over the button, and a pop-under on every click",
        start: "short.localhost/adlayer",
        lists: true,
        expect: Expect::LedTo(&["https://pixeldrain.com/u/adlayer01"]),
        floor: 0,
        within: 15,
        gap: false,
    },
    Scenario {
        name: "a cookie dialog that swallows clicks outside it",
        start: "articles.localhost/consent",
        lists: true,
        expect: Expect::LedTo(&["https://pixeldrain.com/u/consent01"]),
        floor: 0,
        within: 15,
        gap: false,
    },
    Scenario {
        name: "countdowns, and a step at the foot of a long page",
        start: "short.localhost/countdown",
        lists: true,
        expect: Expect::LedTo(&["https://pixeldrain.com/u/countdown1"]),
        floor: 8,
        within: 25,
        gap: false,
    },
    Scenario {
        name: "a step whose script loads late, in a link around a block",
        start: "short.localhost/late",
        lists: true,
        expect: Expect::LedTo(&["https://pixeldrain.com/u/latescript"]),
        floor: 5,
        within: 20,
        gap: false,
    },
    Scenario {
        name: "a captcha that answers itself, needed by the next page",
        start: "short.localhost/captcha",
        lists: true,
        expect: Expect::LedTo(&["https://pixeldrain.com/u/captcha001"]),
        floor: 3,
        within: 18,
        gap: false,
    },
    Scenario {
        name: "a captcha that stalls until its page is loaded again",
        start: "short.localhost/stall",
        lists: true,
        expect: Expect::LedTo(&["https://pixeldrain.com/u/stall0001"]),
        floor: 1,
        within: 40,
        gap: false,
    },
    Scenario {
        name: "an ad that sends the tab away during a countdown",
        start: "articles.localhost/adredirect",
        lists: true,
        expect: Expect::LedTo(&["https://pixeldrain.com/u/adredirect"]),
        floor: 21,
        within: 35,
        gap: false,
    },
    Scenario {
        name: "an article's own links that read like steps",
        start: "articles.localhost/decoys",
        lists: true,
        expect: Expect::LedTo(&["https://pixeldrain.com/u/decoys001"]),
        floor: 6,
        within: 20,
        gap: false,
    },
    Scenario {
        name: "an advert's dialog that freezes the page",
        start: "short.localhost/alert",
        lists: true,
        expect: Expect::LedTo(&["https://pixeldrain.com/u/alert0001"]),
        floor: 0,
        within: 15,
        gap: false,
    },
    Scenario {
        name: "steps in other languages",
        start: "short.localhost/words",
        lists: true,
        expect: Expect::LedTo(&["https://pixeldrain.com/u/words0001"]),
        floor: 0,
        within: 15,
        gap: false,
    },
    Scenario {
        name: "a mirror index whose list opens in a new tab, from a release",
        start: "index.localhost/mirror",
        lists: false,
        expect: Expect::LedTo(&[
            "https://gofile.io/d/abc123",
            "https://1fichier.com/?abcdefghijklmnopqrst",
        ]),
        floor: 0,
        within: 12,
        gap: false,
    },
    Scenario {
        name: "a mirror index whose list opens in a new tab, added on its own",
        start: "index.localhost/mirror",
        lists: true,
        expect: Expect::Listed(&[
            "https://gofile.io/d/abc123",
            "https://1fichier.com/?abcdefghijklmnopqrst",
        ]),
        floor: 0,
        within: 12,
        gap: false,
    },
    Scenario {
        name: "a last page that links where it leads",
        start: "short.localhost/anchor",
        lists: false,
        expect: Expect::LedTo(&["https://mega.nz/file/abc123#AAAAAAAA"]),
        floor: 0,
        within: 10,
        gap: false,
    },
    Scenario {
        name: "a file's page on a host Haul has no code for",
        start: "files.localhost/f/abc123",
        lists: false,
        expect: Expect::File("/get/archive.part1.rar"),
        floor: 3,
        within: 15,
        gap: false,
    },
    Scenario {
        name: "a checkbox under an advert's frame, then a jigsaw",
        start: "gate.localhost/jigsaw",
        lists: true,
        expect: Expect::LedTo(&["https://pixeldrain.com/u/jigsaw001"]),
        // A tick and three drags at a hand's pace.
        floor: 6,
        within: 30,
        gap: false,
    },
    Scenario {
        name: "a step that is only a picture",
        start: "short.localhost/icon",
        lists: true,
        expect: Expect::LedTo(&["https://pixeldrain.com/u/icononly1"]),
        floor: 0,
        within: 70,
        gap: true,
    },
    Scenario {
        name: "a check page that starts over when its fields are looked up",
        start: "check.localhost/moment",
        lists: true,
        expect: Expect::LedTo(&["https://pixeldrain.com/u/moment001"]),
        floor: 3,
        within: 20,
        gap: false,
    },
];

/// An ad's empty layer over `#go` that takes every click for a pop-up and
/// never goes away, and a pop-under on any click anywhere.
const AD_LAYER: &str = r#"<script>
addEventListener('load', () => {
  const box = document.getElementById('go').getBoundingClientRect();
  const layer = document.createElement('div');
  layer.style.cssText = `position:fixed;z-index:2147483647;cursor:pointer;left:${box.x}px;top:${box.y}px;width:${box.width}px;height:${box.height}px`;
  layer.onclick = () => window.open('http://ads.localhost:{P}/pop', '_blank');
  document.body.append(layer);
});
document.addEventListener('click', () => window.open('http://ads.localhost:{P}/under', '_blank'), true);
</script>"#;

const BUTTON: &str = "font-size:20px;padding:10px 30px";

/// The page a site answers `method` `path` with, the `visit`-th time it is
/// asked for with GET; `{P}` stands for the server's port.
fn page(site: &str, method: &str, path: &str, body: &str, visit: u32) -> Response {
    let to = |known: &str| {
        format!(r#"<button style="{BUTTON}" onclick="location.href='{known}'">Get Link</button>"#)
    };
    match (site, method, path) {
        ("short.localhost", "GET", "/plain") => html(&format!(
            r#"<title>Short</title><form action="http://articles.localhost:{{P}}/plain"><button style="{BUTTON}">Continue</button></form>"#
        )),
        ("articles.localhost", "GET", "/plain") => html(&format!(
            "<title>Ten things</title><p>Made-up words.</p>{}",
            to("https://pixeldrain.com/u/plain001")
        )),

        ("short.localhost", "GET", "/adlayer") => html(&format!(
            r#"<title>Short</title><form method="post" action="/adlayer"><button id="go" style="{BUTTON}">Click here to continue</button></form>
<p><a href="http://ads.localhost:{{P}}/premium">Download faster with Premium</a></p>{AD_LAYER}"#
        )),
        ("short.localhost", "POST", "/adlayer") => {
            redirect("http://articles.localhost:{P}/adlayer")
        }
        ("articles.localhost", "GET", "/adlayer") => html(&format!(
            "<title>Ten things</title>{}",
            to("https://pixeldrain.com/u/adlayer01")
        )),

        ("articles.localhost", "GET", "/consent") => html(&format!(
            r#"<title>Ten things</title>
<div id="consent" role="dialog" style="position:fixed;inset:0;z-index:99999;background:rgba(0,0,0,.6)">
<div style="background:#fff;margin:20vh auto;width:420px;padding:20px"><p>This made-up site keeps cookies. Agree to go on.</p>
<button id="more">SETTINGS</button> <button id="no">DISAGREE</button> <button id="yes">AGREE</button></div></div>
<button id="go" style="{BUTTON}" onclick="location.href='https://pixeldrain.com/u/consent01'">Continue</button>
<script>
// The dialog's focus trap: while it is up, and after its markup is taken
// away without an answer, clicks outside it go nowhere.
let trapped = true;
document.addEventListener('click', (e) => {{
  if (trapped && !e.target.closest('#consent')) {{ e.preventDefault(); e.stopImmediatePropagation(); }}
}}, true);
for (const id of ['yes', 'no']) document.getElementById(id).onclick = () => {{ trapped = false; document.getElementById('consent').remove(); }};
</script>"#
        )),

        ("short.localhost", "GET", "/countdown") => html(&format!(
            r#"<title>Short</title><form action="http://articles.localhost:{{P}}/countdown"><button style="{BUTTON}">Continue</button></form>"#
        )),
        ("articles.localhost", "GET", "/countdown") => html(
            r#"<title>Ten things</title><h1>Ten things to know</h1>
<a id="start" onclick="window.open('http://ads.localhost:{P}/pop', '_blank')"><p style="margin:9px"> Continue...</p></a>
<div id="timer" style="display:none"></div>
<p style="height:1600px">Made-up words about nothing in particular.</p>
<form id="foot" style="display:none" action="http://short.localhost:{P}/countdown-last">
<button type="submit" onclick="window.open('http://ads.localhost:{P}/pop', '_blank')" style="height:45px">Get Link</button></form>
<script>
document.getElementById('start').addEventListener('click', function () {
  this.style.display = 'none';
  const timer = document.getElementById('timer');
  timer.style.display = 'block';
  let left = 5;
  timer.innerText = `Please Wait ${left} Seconds...`;
  const tick = setInterval(() => {
    left--;
    if (left > 0) return void (timer.innerText = `Please Wait ${left} Seconds...`);
    clearInterval(tick);
    timer.style.display = 'none';
    document.getElementById('foot').style.display = 'block';
  }, 1000);
});
</script>"#,
        ),
        ("short.localhost", "GET", "/countdown-last") => html(&format!(
            r#"<title>Short</title><h1>Your link is almost ready.</h1><p id="t">3 Seconds</p>
<button id="get" disabled style="{BUTTON}">Getting link...</button>
<script>
let left = 3;
const tick = setInterval(() => {{
  left--;
  document.getElementById('t').textContent = `${{left}} Seconds`;
  if (left > 0) return;
  clearInterval(tick);
  const get = document.getElementById('get');
  get.disabled = false;
  get.textContent = 'Get Link';
  get.onclick = () => {{ location.href = 'https://pixeldrain.com/u/countdown1'; }};
}}, 1000);
</script>"#
        )),

        ("short.localhost", "GET", "/late") => html(
            r#"<title>Short</title><p>Made-up words.</p>
<a id="start"><p style="margin:9px 0 0 120px;padding:14px 40px;background:#e44;color:#fff;display:inline-block"> Continue...</p></a>
<script>
// As a page whose scripts wait for the first move of the mouse: the first
// presses find nobody listening.
setTimeout(() => {
  document.getElementById('start').addEventListener('click', () => { location.href = 'https://pixeldrain.com/u/latescript'; });
}, 5000);
</script>"#,
        ),

        ("short.localhost", "GET", "/captcha") => captcha_page("/captcha", 3000),
        ("short.localhost", "POST", "/captcha") => {
            answered(body, "https://pixeldrain.com/u/captcha001")
        }
        // Its first page's captcha never answers; loaded again, it does.
        ("short.localhost", "GET", "/stall") if visit == 1 => captcha_page("/stall", 3_600_000),
        ("short.localhost", "GET", "/stall") => captcha_page("/stall", 1000),
        ("short.localhost", "POST", "/stall") => {
            answered(body, "https://pixeldrain.com/u/stall0001")
        }

        ("articles.localhost", "GET", "/adredirect") => html(&format!(
            r#"<title>Ten things</title><p id="t"></p>
<button id="go" style="{BUTTON}">Continue</button>
<button id="get" style="{BUTTON};display:none" onclick="location.href='https://pixeldrain.com/u/adredirect'">Get Link</button>
<script>
document.getElementById('go').onclick = function () {{
  this.style.display = 'none';
  document.getElementById('t').textContent = 'Please wait 21 seconds...';
  // An ad takes the whole tab, long after the press.
  setTimeout(() => {{ location.href = 'http://ads.localhost:{{P}}/landing'; }}, 18000);
  setTimeout(() => {{ document.getElementById('t').textContent = ''; document.getElementById('get').style.display = ''; }}, 21000);
}};
</script>"#
        )),

        // While its countdown runs the page offers nothing but its own
        // article links, whose words read like steps and lead away.
        ("articles.localhost", "GET", "/decoys") => html(
            r#"<title>Ten things</title><h1>Ten things to know</h1>
<a id="start"><p style="margin:9px"> Continue...</p></a>
<div id="timer" style="display:none"></div>
<ul><li><a href="/lost">Your Go-to Guide To Example Things</a></li>
<li><a href="/lost">Continue reading</a></li>
<li><a href="/lost">How to download videos</a></li>
<li><a href="/lost">Next</a></li>
<li><a href="/lost">Get link building tips</a></li></ul>
<button id="get" style="display:none;font-size:20px;padding:10px 30px" onclick="location.href='https://pixeldrain.com/u/decoys001'">Get Link</button>
<script>
document.getElementById('start').addEventListener('click', function () {
  this.style.display = 'none';
  const timer = document.getElementById('timer');
  timer.style.display = 'block';
  let left = 6;
  timer.innerText = `Please Wait ${left} Seconds...`;
  const tick = setInterval(() => {
    left--;
    if (left > 0) return void (timer.innerText = `Please Wait ${left} Seconds...`);
    clearInterval(tick);
    timer.style.display = 'none';
    document.getElementById('get').style.display = '';
  }, 1000);
});
</script>"#,
        ),
        ("articles.localhost", "GET", "/lost") => {
            html("<title>Another article</title><p>Made-up words, and no way on.</p>")
        }

        ("short.localhost", "GET", "/alert") => html(&format!(
            r#"<title>Short</title><button style="{BUTTON}" onclick="location.href='https://pixeldrain.com/u/alert0001'">Continue</button>
<script>
// A dialog as the page loads and on the first click, as an advert's.
setTimeout(() => alert('A made-up prize is waiting.'), 300);
document.addEventListener('click', () => confirm('Leave this made-up page?'), {{capture: true, once: true}});
</script>"#
        )),

        ("short.localhost", "GET", "/words") => html(&format!(
            r#"<title>Kurz</title><form action="/words-2"><button style="{BUTTON}">Weiter</button></form>"#
        )),
        ("short.localhost", "GET", "/words-2") => html(&format!(
            r#"<title>Court</title><form action="/words-3"><button style="{BUTTON}">Continuer</button></form>"#
        )),
        ("short.localhost", "GET", "/words-3") => html(&format!(
            r#"<title>Corto</title><button style="{BUTTON}" onclick="location.href='https://pixeldrain.com/u/words0001'">Siguiente</button>"#
        )),

        ("index.localhost", "GET", "/mirror") => html(&format!(
            r#"<title>Download archive.part1.rar</title>
<form action="/mirror-list" method="post" target="win12345" onsubmit="window.open('','win12345')">
<a href="http://ads.localhost:{{P}}/plus" style="display:block;{BUTTON}"><strong>Download faster with Example Plus (NO LIMITS)</strong></a>
<button id="download-button" type="submit" style="display:block;{BUTTON}"><strong>Download</strong></button></form>"#
        )),
        ("index.localhost", "POST", "/mirror-list") => html(
            r#"<title>Mirror list archive.part1.rar</title>
<p><a class="btn" href="https://gofile.io/d/abc123">Download (126)</a></p>
<p><a class="btn" href="https://1fichier.com/?abcdefghijklmnopqrst">Download (170)</a></p>
<p><a class="btn" href="http://otherhost.localhost:{P}/f/abc123">Download (114)</a></p>"#,
        ),

        ("short.localhost", "GET", "/anchor") => html(
            r#"<title>Short</title><h1>Your link is ready.</h1>
<a class="btn" style="display:inline-block;padding:10px 30px" href="https://mega.nz/file/abc123#AAAAAAAA">Get Link</a>"#,
        ),

        ("files.localhost", "GET", "/f/abc123") => html(&format!(
            r#"<title>archive.part1.rar</title><h1>archive.part1.rar</h1><p>3 MB</p>
<a href="http://ads.localhost:{{P}}/premium" style="display:block">Premium download: high speed</a>
<button id="dl" disabled style="{BUTTON}">Please wait 3</button>
<script>
let left = 3;
const tick = setInterval(() => {{
  left--;
  const dl = document.getElementById('dl');
  if (left > 0) return void (dl.textContent = `Please wait ${{left}}`);
  clearInterval(tick);
  dl.disabled = false;
  dl.textContent = 'Download';
  dl.onclick = () => {{ location.href = 'http://cdn.localhost:{{P}}/get/archive.part1.rar'; }};
}}, 1000);
</script>"#
        )),
        ("cdn.localhost", "GET", "/get/archive.part1.rar") => Response {
            status: 200,
            headers: vec![
                ("Content-Type".into(), "application/octet-stream".into()),
                (
                    "Content-Disposition".into(),
                    r#"attachment; filename="archive.part1.rar""#.into(),
                ),
            ],
            body: vec![0; 4096],
        },

        ("short.localhost", "GET", "/icon") => html(
            r#"<title>Short</title>
<button style="width:120px;height:60px" onclick="location.href='https://pixeldrain.com/u/icononly1'">
<svg width="40" height="24" viewBox="0 0 40 24"><path d="M2 12h30M24 4l10 8-10 8" fill="none" stroke="currentColor" stroke-width="3"/></svg></button>"#,
        ),

        // As Cloudflare's own check page: it answers 403, and starts over
        // whenever a script in its world looks up a captcha's field or
        // frame (the made-up part: how the real one tells is not known,
        // only that it does). Left alone, it passes in 3 s.
        ("check.localhost", "GET", "/moment") => {
            let mut r = html(
                r#"<title>Just a moment...</title><p>Checking your browser.</p>
<input type="hidden" name="cf-turnstile-response" value="">
<script>
window._cf_chl_opt = { cType: 'managed' };
const looked = (selector) => {
  if (/-response|recaptcha|hcaptcha|turnstile/i.test(String(selector))) location.reload();
};
for (const of of [Document.prototype, Element.prototype]) {
  for (const name of ['querySelector', 'querySelectorAll']) {
    const own = of[name];
    of[name] = function (selector) { looked(selector); return own.call(this, selector); };
  }
}
setTimeout(() => { location.href = '/moment-past'; }, 3000);
</script>"#,
            );
            r.status = 403;
            r
        }
        ("check.localhost", "GET", "/moment-past") => html(&format!(
            "<title>Example</title>{}",
            to("https://pixeldrain.com/u/moment001")
        )),

        ("gate.localhost", "GET", "/jigsaw") => html(&jigsaw_page()),
        // The stand-in widget's service: this visit does not pass by itself.
        ("gate.localhost", "POST", "/cutcaptcha/v1/challenge") => Response {
            status: 200,
            headers: vec![("Content-Type".into(), "application/json".into())],
            body: br#"{"data":{"auto":false},"success":true}"#.to_vec(),
        },

        ("ads.localhost", "GET", "/setup.exe") => Response {
            status: 200,
            headers: vec![
                ("Content-Type".into(), "application/octet-stream".into()),
                (
                    "Content-Disposition".into(),
                    r#"attachment; filename="setup.exe""#.into(),
                ),
            ],
            body: vec![0; 4096],
        },
        ("ads.localhost", ..) => html("<title>An ad</title><p>An ad.</p>"),
        _ => Response {
            status: 404,
            headers: Vec::new(),
            body: b"not found".to_vec(),
        },
    }
}

/// A page behind a stand-in for the CutCaptcha widget, built as the real
/// one is (closed shadow roots, its parts under its class names) around a
/// made-up puzzle, far down its page and started only once it is seen: a
/// checkbox that takes only a hand's click, under an
/// advert's frame that covers the whole page and a layer whose click
/// starts an advert's download; then a jigsaw whose pieces
/// drag with the mouse and whose Verify gives the answer field its token
/// when every piece is home. With the token the page shows its way on.
fn jigsaw_page() -> String {
    use haul_core::jigsaw::made_up;
    let made = made_up::puzzle(7, 3);
    let picture = made.picture_uri();
    let pieces: Vec<String> = made
        .piece_uris()
        .into_iter()
        .enumerate()
        .map(|(at, (image, width, (left, top)))| {
            format!(
                r#"{{image: "{image}", w: {width}, left: {}, top: 335, ax: {left}, ay: {top}}}"#,
                20 + at * 150,
            )
        })
        .collect();
    r##"<title>Gate</title><h1>Security check</h1>
<form style="margin-top:1600px"><div class="cutcaptcha" style="min-height:60px"></div></form>
<iframe src="http://ads.localhost:{P}/pop" style="position:fixed;z-index:2147483646;left:0;top:0;width:100vw;height:100vh;border:0;opacity:0.02"></iframe>
<div onclick="location.href='http://ads.localhost:{P}/setup.exe'" style="position:fixed;z-index:2147483647;left:0;top:0;width:100vw;height:260px"></div>
<div id="after" style="display:none"><button style="font-size:20px;padding:10px 30px" onclick="location.href='https://pixeldrain.com/u/jigsaw001'">Get Link</button></div>
<script>
const puzzle = {picture: "PICTURE", pieces: [PIECES]};
// Far down the page, the widget starts once it is seen.
let root;
new IntersectionObserver((seen, watch) => {
  if (!seen.some((s) => s.isIntersecting)) return;
  watch.disconnect();
  document.querySelector('.cutcaptcha').innerHTML = '<input type="hidden" name="cutcaptcha-response" value=""><div id="host"></div>';
  fetch('/cutcaptcha/v1/challenge', {method: 'POST', body: '{}'}).then((said) => said.json());
  root = document.getElementById('host').attachShadow({mode: 'closed'});
  start();
}).observe(document.querySelector('.cutcaptcha'));
function start() {
root.innerHTML = '<div class="cc"><div class="cc__box" role="checkbox" tabindex="0" style="width:300px;height:60px;border:1px solid #888;display:flex;align-items:center;padding-left:14px;font:16px sans-serif">No script here</div></div>';
root.querySelector('.cc__box').addEventListener('click', (e) => { if (e.isTrusted) open(); }, {once: true});
}
function open() {
  const modal = document.createElement('div');
  document.body.append(modal);
  const m = modal.attachShadow({mode: 'closed'});
  m.innerHTML = `<div class="ccm" style="position:fixed;inset:0;z-index:1000;background:rgba(0,0,0,.6)"><div class="ccm__panel" style="position:absolute;left:100px;top:20px;background:#222;padding:12px"><div class="ccm__stage" style="position:relative;width:480px;height:480px"><img class="ccm__pic" draggable="false" width="480" height="320" src="${puzzle.picture}"></div><button type="button" class="ccm__btn ccm__re">Reload</button> <button type="button" class="ccm__btn ccm__go">Verify</button></div></div>`;
  const stage = m.querySelector('.ccm__stage');
  for (const p of puzzle.pieces) {
    const piece = document.createElement('div');
    piece.className = 'ccm__piece';
    piece.style.cssText = `position:absolute;left:${p.left}px;top:${p.top}px;width:${p.w}px;height:${p.w}px;`;
    piece.innerHTML = `<img draggable="false" src="${p.image}" style="width:100%;height:100%">`;
    stage.append(piece);
    piece.addEventListener('mousedown', (down) => {
      const [left, top] = [parseFloat(piece.style.left), parseFloat(piece.style.top)];
      const move = (e) => { piece.style.left = `${left + e.clientX - down.clientX}px`; piece.style.top = `${top + e.clientY - down.clientY}px`; };
      const up = () => { removeEventListener('mousemove', move); removeEventListener('mouseup', up); };
      addEventListener('mousemove', move);
      addEventListener('mouseup', up);
    });
  }
  m.querySelector('.ccm__go').addEventListener('click', () => {
    const home = [...m.querySelectorAll('.ccm__piece')].every((piece, at) => Math.abs(parseFloat(piece.style.left) - puzzle.pieces[at].ax) < 6 && Math.abs(parseFloat(piece.style.top) - puzzle.pieces[at].ay) < 6);
    if (!home) return void (m.querySelector('.ccm__go').textContent = 'Not yet');
    document.querySelector('[name=cutcaptcha-response]').value = 'made-up-token';
    modal.remove();
    document.getElementById('after').style.display = '';
  });
}
</script>"##
        .replace("PICTURE", &picture)
        .replace("PIECES", &pieces.join(", "))
}

/// A page whose form carries a captcha's answer, which comes by itself
/// after `after_ms`, under a Continue.
fn captcha_page(action: &str, after_ms: u32) -> Response {
    html(&format!(
        r#"<title>Short</title><h1>Your link is almost ready</h1>
<form method="post" action="{action}"><input type="hidden" name="cf-turnstile-response" value="">
<button type="submit" style="{BUTTON}">Continue</button></form>
<script>setTimeout(() => {{ document.querySelector('[name=cf-turnstile-response]').value = 'made-up-answer'; }}, {after_ms});</script>"#
    ))
}

/// What a captcha page's form is answered with: the way on with the
/// captcha's answer, a dead end without.
fn answered(body: &str, known: &str) -> Response {
    if body.contains("cf-turnstile-response=made-up-answer") {
        html(&format!(
            r#"<title>Short</title><button style="{BUTTON}" onclick="location.href='{known}'">Get Link</button>"#
        ))
    } else {
        html("<title>Short</title><p>The captcha was not answered.</p>")
    }
}

struct Response {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

fn html(page: &str) -> Response {
    Response {
        status: 200,
        headers: vec![("Content-Type".into(), "text/html; charset=utf-8".into())],
        body: format!("<!doctype html><html><body>{page}</body></html>").into_bytes(),
    }
}

fn redirect(to: &str) -> Response {
    Response {
        status: 302,
        headers: vec![("Location".into(), to.into())],
        body: Vec::new(),
    }
}

/// Serves the scenarios' sites on a port of this machine.
fn serve() -> u16 {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("a port for the suite's sites");
    let port = listener.local_addr().expect("its address").port();
    let visits: Arc<Mutex<HashMap<String, u32>>> = Arc::default();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let visits = visits.clone();
            std::thread::spawn(move || {
                let _ = answer(stream, port, &visits);
            });
        }
    });
    port
}

fn answer(
    mut stream: TcpStream,
    port: u16,
    visits: &Mutex<HashMap<String, u32>>,
) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let mut words = line.split_whitespace();
    let method = words.next().unwrap_or_default().to_string();
    let target = words.next().unwrap_or_default().to_string();
    let (mut site, mut length) = (String::new(), 0usize);
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header)? == 0 || header.trim().is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            match name.to_ascii_lowercase().as_str() {
                "host" => {
                    site = value
                        .trim()
                        .split(':')
                        .next()
                        .unwrap_or_default()
                        .to_string();
                }
                "content-length" => length = value.trim().parse().unwrap_or(0),
                _ => {}
            }
        }
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;
    let path = target.split('?').next().unwrap_or_default();
    let visit = if method == "GET" {
        let mut visits = haul_core::lock::held(visits);
        let n = visits.entry(format!("{site}{path}")).or_default();
        *n += 1;
        *n
    } else {
        0
    };
    let mut response = page(&site, &method, path, &String::from_utf8_lossy(&body), visit);
    let port = port.to_string();
    for (_, value) in &mut response.headers {
        *value = value.replace("{P}", &port);
    }
    if response
        .headers
        .iter()
        .any(|(n, v)| n == "Content-Type" && v.starts_with("text/html"))
    {
        response.body = String::from_utf8_lossy(&response.body)
            .replace("{P}", &port)
            .into_bytes();
    }
    let mut head = format!(
        "HTTP/1.1 {} OK\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\n",
        response.status,
        response.body.len()
    );
    for (name, value) in &response.headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes())?;
    stream.write_all(&response.body)?;
    stream.flush()
}

/// Whether `got` holds exactly the addresses `want`, in any order.
fn same(got: &[String], want: &[&str]) -> bool {
    got.len() == want.len() && want.iter().all(|w| got.iter().any(|g| g == w))
}

/// Walks scenario `at` in the browser at `exe`: its line of the scorecard,
/// and what went wrong when something did.
fn walk_one(
    at: usize,
    scenario: &Scenario,
    exe: &Path,
    port: u16,
    profiles: &Path,
    base: f32,
) -> (String, Option<String>, f32) {
    let url = format!(
        "http://{}",
        scenario.start.replacen('/', &format!(":{port}/"), 1)
    );
    let taken: Mutex<Vec<String>> = Mutex::default();
    // As the engine takes a page's links: those on sites Haul knows.
    let take = |shown: &Shown| {
        let known: Vec<String> = shown
            .links
            .iter()
            .filter(|l| hosts::walk_ends_at(l))
            .cloned()
            .collect();
        let took = !known.is_empty();
        if took {
            *haul_core::lock::held(&taken) = known;
        }
        took
    };
    let scripts = crate::browser::page_scripts(scenario.lists, true);
    let item = format!("walk{at}-{}", (base * 10.0) as u32);
    let started = Instant::now();
    let outcome = pass(Pass {
        exe,
        item: &item,
        url: &url,
        profile: profiles.join(&item),
        proxy: None,
        scripts: &scripts,
        show_after: Duration::from_secs(30),
        handle: Handle::default(),
        take_links: if scenario.lists { Some(&take) } else { None },
        ends_settled: false,
        press_download: false,
        keep_to_site: true,
        hand_clearance: false,
        walk: true,
    });
    let took = started.elapsed().as_secs_f32();
    let taken = haul_core::lock::held(&taken).clone();
    let (reached, told) = match (&scenario.expect, &outcome) {
        (Expect::LedTo(want), Ok(Outcome::LedTo(got))) => (same(got, want), got.join(" ")),
        (Expect::Listed(want), Ok(Outcome::Listed)) => (same(&taken, want), taken.join(" ")),
        (Expect::File(end), Ok(Outcome::Files { files, .. })) => (
            files.len() == 1 && files[0].url.ends_with(end),
            files
                .iter()
                .map(|f| f.url.as_str())
                .collect::<Vec<_>>()
                .join(" "),
        ),
        (_, Ok(Outcome::WantsPerson(look))) => (false, format!("wants a person: {}", look.page)),
        (_, other) => (false, format!("{other:?}")),
    };
    // The limits are for a machine whose browser starts and stops in no
    // time: this one's share is added, with room for four starting at once.
    let in_time = took <= scenario.within as f32 + SLACK * base;
    let over = (took - scenario.floor as f32 - base).max(0.0);
    let verdict = match (reached && in_time, scenario.gap) {
        (true, false) => "ok",
        (false, true) => "gap",
        (true, true) => "GAP CLOSED",
        (false, false) if reached => "SLOW",
        (false, false) => "FAILED",
    };
    let fine = verdict == "ok" || verdict == "gap";
    // What the pass did, for a scenario that went wrong: its signals.
    let signals: String = if fine {
        String::new()
    } else {
        haul_core::debug::bus()
            .map(|bus| bus.recent())
            .unwrap_or_default()
            .iter()
            .filter(|s| s["kind"] == "pass" && s["id"] == item.as_str())
            .map(|s| {
                let at = s["t"].as_str().unwrap_or_default();
                let mut s = s.clone();
                for told in ["t", "kind", "id", "url"] {
                    s.as_object_mut().map(|s| s.remove(told));
                }
                let said: String = s.to_string().chars().take(300).collect();
                format!("\n              {} {said}", at.get(11..23).unwrap_or(at))
            })
            .collect()
    };
    let line = format!(
        "{verdict:>10}  {took:>5.1} s  {:>2} s its pages  {over:>5.1} s the walk  (limit {:>2} s and the browser)  {}{}",
        scenario.floor,
        scenario.within,
        scenario.name,
        if fine {
            String::new()
        } else {
            format!(
                "\n            {}{signals}",
                told.chars().take(400).collect::<String>()
            )
        }
    );
    (
        line,
        (!fine).then(|| format!("{}: {verdict}", scenario.name)),
        if scenario.gap { 0.0 } else { over },
    )
}

/// Walks every scenario, a few at a time as the desktop app passes pages
/// of different sites, and tells how each went. `HAUL_WALK_ONLY` keeps to
/// the scenarios whose name holds its words.
#[test]
fn every_scenario_is_walked_to_where_it_leads() {
    let Some(found) = browsers().into_iter().next() else {
        eprintln!("the walk suite needs Edge or Chrome (or HAUL_CHROMIUM): skipped");
        return;
    };
    let port = serve();
    let profiles = std::env::temp_dir().join(format!("haul-walk-suite-{}", std::process::id()));
    let only = std::env::var("HAUL_WALK_ONLY").ok();
    // The passes' signals are kept, to tell what a failed scenario did.
    haul_core::debug::enable(&profiles);
    // What this machine's browser takes to start, show a page and close:
    // the scenario with nothing to walk, alone. A walk's own time is told
    // without it, so the scorecard compares across machines.
    let nothing = SCENARIOS
        .iter()
        .position(|s| s.name == NOTHING_TO_WALK)
        .expect("the scenario with nothing to walk");
    let began = Instant::now();
    walk_one(
        nothing,
        &SCENARIOS[nothing],
        &found.exe,
        port,
        &profiles,
        0.0,
    );
    let base = began.elapsed().as_secs_f32();
    let next = std::sync::atomic::AtomicUsize::new(0);
    let done: Mutex<Vec<Walked>> = Mutex::default();
    let started = Instant::now();
    std::thread::scope(|scope| {
        for _ in 0..AT_ONCE {
            scope.spawn(|| {
                loop {
                    let at = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some(scenario) = SCENARIOS.get(at) else {
                        return;
                    };
                    if only.as_ref().is_some_and(|o| !scenario.name.contains(o)) {
                        continue;
                    }
                    let (line, wrong, over) =
                        walk_one(at, scenario, &found.exe, port, &profiles, base);
                    haul_core::lock::held(&done).push((at, line, wrong, over));
                }
            });
        }
    });
    let _ = std::fs::remove_dir_all(&profiles);
    let mut done = std::mem::take(&mut *haul_core::lock::held(&done));
    done.sort_by_key(|(at, ..)| *at);
    let card: Vec<&str> = done.iter().map(|(_, line, ..)| line.as_str()).collect();
    // The number to bring down: what the walks took beyond their pages'
    // own time, the known gaps left out.
    let over: f32 = done.iter().map(|(.., over)| over).sum();
    println!(
        "\nwalk suite, in {}, {} s in all; the browser's start and stop {base:.1} s; {over:.1} s the walks' own:\n{}\n",
        found.name,
        started.elapsed().as_secs(),
        card.join("\n")
    );
    let wrong: Vec<&str> = done
        .iter()
        .filter_map(|(_, _, w, _)| w.as_deref())
        .collect();
    assert!(
        wrong.is_empty(),
        "the walk suite (take the gap mark off a scenario that now passes):\n{}",
        wrong.join("\n")
    );
}

/// Scenarios walked at once.
const AT_ONCE: usize = 4;

/// The scenario that only starts and stops the browser.
const NOTHING_TO_WALK: &str = "a last page that links where it leads";

/// How many times the browser's start and stop a scenario's limit allows
/// for, with four browsers starting at once.
const SLACK: f32 = 3.0;

/// A scenario's place, its line of the scorecard, what went wrong, and the
/// seconds its walk took beyond its pages' own.
type Walked = (usize, String, Option<String>, f32);
