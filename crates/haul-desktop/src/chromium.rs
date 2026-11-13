//! Hosts' pages passed in a full browser Haul drives over DevTools: on
//! Windows, the Edge that comes with it (or Chrome). Cloudflare Turnstile
//! asks for its box in an embedded WebView2 every time, and passes by itself
//! in Edge, Chrome and WebKitGTK alike (docs/decisions.md has the
//! measurements), so Windows passes such pages here; Linux keeps its webview.
//!
//! The browser runs off screen with a profile of its own, one per site, kept.
//! Haul only adds its two page scripts (`overlays.js`, `handoff.js`), reads
//! what the hand-off leaves in the page and closes the tabs ads open; a page
//! that still wants a person after its time ends the pass, and the person
//! opens it in Haul's own window. It never turns on the DevTools domains
//! automation is told apart by (`Runtime.enable`).

use std::{
    net::{SocketAddr, TcpStream},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
    time::{Duration, Instant},
};

use haul_core::{
    hosts,
    pass_ends::{Closed, Process},
    presser::{Candidate, Presser},
    walker::{Next, Step, Walker},
};
use serde_json::{Value, json};
use tungstenite::{Message, WebSocket, stream::MaybeTlsStream};

/// The browser to pass pages in first ([`browsers`]).
pub fn find() -> Option<PathBuf> {
    browsers().into_iter().next().map(|b| b.exe)
}

/// A browser to pass pages in.
#[derive(Debug, Clone)]
pub struct Found {
    /// `Edge` or `Chrome`, or `HAUL_CHROMIUM` for the one it names.
    pub name: &'static str,
    pub exe: PathBuf,
}

/// The browsers to pass pages in, the first first: `HAUL_CHROMIUM` alone
/// when set (any platform, for trying it out), else on Windows the Edge that
/// comes with it, then Chrome, each where it is installed. A pass that keeps
/// closing by itself in the first is tried in the other
/// ([`haul_core::pass_ends`]).
pub fn browsers() -> Vec<Found> {
    if let Some(path) = std::env::var_os("HAUL_CHROMIUM").map(PathBuf::from) {
        return path
            .is_file()
            .then_some(Found {
                name: "HAUL_CHROMIUM",
                exe: path,
            })
            .into_iter()
            .collect();
    }
    if !cfg!(windows) {
        return Vec::new();
    }
    let roots = ["ProgramFiles(x86)", "ProgramFiles", "LOCALAPPDATA"];
    let names = [
        ("Edge", r"Microsoft\Edge\Application\msedge.exe"),
        ("Chrome", r"Google\Chrome\Application\chrome.exe"),
    ];
    names
        .iter()
        .filter_map(|&(name, exe)| {
            roots
                .iter()
                .filter_map(|root| std::env::var_os(root).map(|r| PathBuf::from(r).join(exe)))
                .find(|p| p.is_file())
                .map(|exe| Found { name, exe })
        })
        .collect()
}

/// Lets the app stop a page being passed, and, in debug mode, see and ask
/// what it shows.
#[derive(Clone, Default)]
pub struct Handle {
    pub stop: Arc<AtomicBool>,
    /// Stopped because its row went or stopped waiting (removed, paused):
    /// nobody is asked about it.
    pub abandoned: Arc<AtomicBool>,
    /// What the pass is about, for debug mode: its page, profile, browser
    /// process and the page's last message to Haul.
    pub info: Arc<Mutex<Value>>,
    /// Questions about the page for the pass to answer between its looks
    /// ([`Handle::ask`]).
    asks: Arc<Mutex<Vec<Ask>>>,
}

/// A question about the page, or a request of it.
struct Ask {
    what: PageAsk,
    reply: std::sync::mpsc::Sender<Result<Value, String>>,
}

/// What debug mode asks of a page being passed.
pub enum PageAsk {
    /// What it shows ([`LOOK`]), with a screenshot.
    Look,
    /// What a script run in it answers.
    Eval(String),
    /// A click, as a person's mouse makes it, in the middle of the first
    /// element a CSS selector finds.
    Click(String),
    /// The browser's cookies for the page, those its scripts cannot read
    /// included, and its user agent.
    Cookies,
}

impl Handle {
    /// Asks the pass ([`PageAsk`]), which answers between its looks at the
    /// page, within a second or so.
    pub fn ask(&self, what: PageAsk) -> Result<Value, String> {
        let (reply, answer) = std::sync::mpsc::channel();
        haul_core::lock::held(&self.asks).push(Ask { what, reply });
        answer
            .recv_timeout(Duration::from_secs(20))
            .map_err(|_| "the pass did not answer (it may have ended)".to_string())?
    }

    fn note(&self, key: &str, value: Value) {
        if let Value::Object(info) = &mut *haul_core::lock::held(&self.info) {
            info.insert(key.into(), value);
        }
    }
}

/// One file the page handed back, with the browser's cookies for its link.
#[derive(Debug)]
pub struct File {
    pub url: String,
    pub name: Option<String>,
    pub size: Option<u64>,
    pub cookie: Option<String>,
}

#[derive(Debug)]
pub enum Outcome {
    Files {
        files: Vec<File>,
        agent: Option<String>,
        /// On a wrapper's page Haul's own code reads, the parts of it that
        /// code reads, which stand in for the files when Haul finds what
        /// the page holds.
        page: Option<String>,
    },
    /// The page said its file is gone.
    Gone(String),
    /// The page says something that may not last: the mirror failed for now,
    /// nobody is asked.
    Failed(String),
    /// The page could not hand out its link this time, and maybe says how
    /// long its host limits this address.
    Later {
        reason: String,
        seconds: Option<u64>,
    },
    /// The browser is past the check in front of the page's site and got
    /// these cookies for it (`Set-Cookie` lines) as user agent `agent`:
    /// Haul reads the page itself with both ([`Pass::hand_clearance`]).
    Cleared {
        cookies: Vec<String>,
        agent: Option<String>,
    },
    /// The walked page went, by its own steps, to a page on a site Haul
    /// has code for (a wrapper, a host, a paste), or offers steps that link
    /// to such pages (a mirror index's hosts): Haul goes on from these
    /// addresses itself ([`Pass::walk`]).
    LedTo(Vec<String>),
    /// The page showed links Haul took ([`Pass::take_links`]).
    Listed,
    /// The page still wants someone after its time (its check asks for a
    /// box to tick): the pass ends, and the person opens the page in Haul.
    WantsPerson(Look),
    /// The page passed every check and showed nothing Haul took
    /// ([`Pass::ends_settled`]).
    Settled,
    /// The pass ended without the page's word: it was stopped, or the
    /// browser went away or closed the page.
    Closed(Closed),
}

pub struct Pass<'a> {
    pub exe: &'a Path,
    /// The row whose page it is, for the signals.
    pub item: &'a str,
    pub url: &'a str,
    /// The site's own folder, kept for its next pages.
    pub profile: PathBuf,
    /// Haul's local bridge to the proxy, for a proxied host.
    pub proxy: Option<SocketAddr>,
    /// Run in every page before its own scripts.
    pub scripts: &'a [&'a str],
    /// How long the page gets before it is taken to want a person.
    pub show_after: Duration,
    pub handle: Handle,
    /// For a page that lists links: offered every address the page shows,
    /// and those written out in its text, as more appear; true once it took
    /// some, which ends the pass.
    pub take_links: Option<TakeLinks<'a>>,
    /// The pass ends as soon as the page settles past its checks without
    /// links Haul takes: on a paste, nothing more is coming.
    pub ends_settled: bool,
    /// The page is a file's page on a host Haul has no recipe for: its
    /// download button is pressed, as a person would, page after page, until
    /// the file comes ([`press_download`]).
    pub press_download: bool,
    /// The page is kept on the site it lands on ([`Cdp::keep_to_site`]): a
    /// page that is no host's own (a wrapper's, one behind a check), whose
    /// files are read off it rather than reached by going elsewhere.
    pub keep_to_site: bool,
    /// The pass ends as soon as the browser is past Cloudflare's check in
    /// front of the page's site, with the clearance it got
    /// ([`Outcome::Cleared`]): for a page Haul reads itself once it may.
    pub hand_clearance: bool,
    /// The page is on a site Haul has no code for, walked as a person would
    /// ([`haul_core::walker`]): its captcha waited for, its steps
    /// ("Continue", "Get Link", a download) pressed page after page, and
    /// let to another site only as a press's doing, until it shows links
    /// Haul takes, hands over a file or reaches a site Haul knows.
    pub walk: bool,
}

/// Offered a page's addresses; true once it took some.
pub type TakeLinks<'a> = &'a dyn Fn(&Shown) -> bool;

/// What a page that lists links shows: every address on it, those written
/// out in its text, and its title.
#[derive(Debug, Default, PartialEq)]
pub struct Shown {
    pub links: Vec<String>,
    pub text: Vec<String>,
    pub title: Option<String>,
}

/// Passes the page; blocks until it hands something back, says its file is
/// gone, wants a person, or is stopped. The browser never comes on screen or
/// onto the taskbar. Its profile (`p.profile`, one per site) is kept, so a
/// check passed once (Cloudflare's clearance cookie) holds for the site's
/// next pages.
pub fn pass(p: Pass<'_>) -> Result<Outcome, String> {
    std::fs::create_dir_all(&p.profile).map_err(|e| format!("{}: {e}", p.profile.display()))?;
    mark_closed_cleanly(&p.profile);
    #[cfg(windows)]
    end_stray(&p, "before");
    // A port of our own choosing: with `--remote-debugging-port=0` (the
    // browser picks one) Turnstile asked for its box every time, with a
    // port named it passed (docs/decisions.md).
    let port = std::net::TcpListener::bind(("127.0.0.1", 0))
        .and_then(|l| l.local_addr())
        .map_err(|e| format!("a port for DevTools: {e}"))?
        .port();
    let mut args = vec![
        format!("--remote-debugging-port={port}"),
        format!("--user-data-dir={}", p.profile.display()),
        "--no-first-run".into(),
        "--no-default-browser-check".into(),
        "--window-position=-20000,-20000".into(),
        // Off screen, Windows counts the window as covered and Chrome hides
        // its page (`visibilityState` "hidden"): it stops drawing, mouse
        // input never reaches it, and checks see a page no person looks
        // at. These keep it a visible page.
        // Prerendering lets an ad's page be swapped in without a navigation
        // the keep-to-site guard could refuse.
        "--disable-features=CalculateNativeWinOcclusion,Prerender2,NoStatePrefetch".into(),
        "--disable-backgrounding-occluded-windows".into(),
        "--window-size=1100,800".into(),
        // A pass browser Haul had to end (an update, a crash) leaves no
        // "Restore pages" bubble behind for the next one.
        "--hide-crash-restore-bubble".into(),
    ];
    if let Some(addr) = p.proxy {
        args.push(format!("--proxy-server=socks5://{addr}"));
        // WebRTC could otherwise reveal the real address to the page.
        args.push("--force-webrtc-ip-handling-policy=disable_non_proxied_udp".into());
    }
    args.push("about:blank".into());
    let child = Command::new(p.exe)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("{}: {e}", p.exe.display()))?;
    let mut browser = Browser {
        #[cfg(windows)]
        _job: Job::holding(&child),
        hide: Arc::new(AtomicU32::new(child.id())),
        child,
    };
    *haul_core::lock::held(&p.handle.info) = json!({
        "url": p.url,
        "profile": p.profile.display().to_string(),
        "pid": browser.child.id(),
        "started": haul_core::debug::now(),
        "show_after_secs": p.show_after.as_secs(),
    });
    haul_core::debug::emit(
        "pass",
        json!({ "event": "start", "id": p.item, "url": p.url, "profile": p.profile.display().to_string(), "pid": browser.child.id() }),
    );
    // Its windows stay off the taskbar from the first moment, not only once
    // the page is driven.
    let hiding = Arc::new(AtomicBool::new(true));
    {
        let (hiding, pid) = (hiding.clone(), browser.hide.clone());
        std::thread::spawn(move || {
            while hiding.load(Ordering::Relaxed) {
                taskbar::hide(pid.load(Ordering::Relaxed));
                std::thread::sleep(Duration::from_millis(50));
            }
        });
    }
    let result = drive(&mut browser, port, &p);
    // Closed as a person closes it, so the profile is left as closed
    // cleanly; killed only if that does not end it.
    close_gently(&mut browser, port);
    hiding.store(false, Ordering::Relaxed);
    // The process Haul started exited during the pass: the browser that
    // answered for it may hold the profile outside Haul's job.
    let handed_over = browser.child.try_wait().ok().flatten().is_some();
    drop(browser);
    #[cfg(windows)]
    if handed_over {
        end_stray(&p, "after");
    }
    #[cfg(not(windows))]
    let _ = handed_over;
    {
        // The outcome without the cookies a file carries: to the log too,
        // debug mode or not.
        let outcome = match &result {
            Ok(Outcome::Files { files, .. }) => {
                json!({ "files": files.iter().map(|f| &f.url).collect::<Vec<_>>() })
            }
            Ok(Outcome::Gone(why)) => json!({ "gone": why }),
            Ok(Outcome::Failed(why)) => json!({ "failed": why }),
            Ok(Outcome::Later { reason, seconds }) => {
                json!({ "later": reason, "seconds": seconds })
            }
            // Their names only, not what they hold.
            Ok(Outcome::Cleared { cookies, agent }) => json!({
                "cleared": cookies.iter().filter_map(|c| c.split('=').next()).collect::<Vec<_>>(),
                "agent": agent,
            }),
            Ok(Outcome::Listed) => json!("listed"),
            Ok(Outcome::LedTo(to)) => json!({ "led_to": to }),
            Ok(Outcome::WantsPerson(look)) => {
                json!({ "wants_person": serde_json::from_str::<Value>(&look.page).unwrap_or(Value::Null) })
            }
            Ok(Outcome::Settled) => json!("settled"),
            Ok(Outcome::Closed(how)) => {
                if *how != Closed::Stopped {
                    tracing::warn!(item = %p.item, "the pass of {} closed: {}", p.url, how.cause());
                }
                json!({ "closed": how.kind(), "cause": how.cause() })
            }
            Err(e) => json!({ "error": e }),
        };
        haul_core::debug::emit(
            "pass",
            json!({ "event": "end", "id": p.item, "url": p.url, "outcome": outcome, "started": haul_core::lock::held(&p.handle.info).get("started").cloned() }),
        );
    }
    result
}

/// The browser process, ended when dropped.
struct Browser {
    child: Child,
    /// The browser process whose windows are kept off the taskbar: the one
    /// Haul started, or the one answering for it once it exited.
    hide: Arc<AtomicU32>,
    /// Ends the browser and every process it started when Haul ends,
    /// however it ends (a crash, the task manager), not only when this drops.
    #[cfg(windows)]
    _job: Option<Job>,
}

/// A Windows job whose processes end when its handle closes, which the
/// system does for a process that ended without closing it.
#[cfg(windows)]
struct Job(windows_sys::Win32::Foundation::HANDLE);

#[cfg(windows)]
impl Job {
    /// A job holding `child`, and what it starts from now on.
    fn holding(child: &Child) -> Option<Self> {
        use std::os::windows::io::AsRawHandle;

        use windows_sys::Win32::System::JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject,
        };

        // SAFETY: no attributes and no name, as the call allows.
        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if handle.is_null() {
            return None;
        }
        let job = Job(handle);
        // SAFETY: a plain C struct; all zeroes is its empty value.
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: `info` is the struct this information class takes, and
        // lives through the call.
        let set = unsafe {
            SetInformationJobObject(
                job.0,
                JobObjectExtendedLimitInformation,
                std::ptr::from_ref(&info).cast(),
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        // SAFETY: both handles are open: the job's here, the child's while
        // `child` lives.
        let assigned = set != 0
            && unsafe { AssignProcessToJobObject(job.0, child.as_raw_handle().cast()) } != 0;
        if !assigned {
            tracing::warn!(
                "the pass browser is not tied to Haul: {}",
                std::io::Error::last_os_error()
            );
            return None;
        }
        Some(job)
    }
}

/// Ends a browser still running on `p`'s profile. A browser started on a
/// profile another one holds hands its page to that one and exits at once
/// (exit code 0, its DevTools never open), so every pass of the site would
/// fail. Only passes start browsers on these profiles, one at a time, so one
/// found here is a stray: Edge registers its command line with Windows'
/// restart manager, which starts it again after a reboot, outside any job of
/// Haul's, and it keeps the profile from then on.
///
/// Chromium finds the browser holding a profile by a message-only window of
/// class `Chrome_MessageWindow` titled with the profile's path; so does this.
///
/// After a pass (`when` is `after`, else `before`) it ends the browser that
/// answered for one Haul started that exited, which may be outside Haul's
/// job: every browser on the pass's profile ends with the pass. Those in
/// the job get [`STRAY_AFTER`] to go first, so none is told as left over.
#[cfg(windows)]
fn end_stray(p: &Pass<'_>, when: &str) {
    use std::os::windows::ffi::OsStrExt;

    use windows_sys::Win32::{
        Foundation::CloseHandle,
        System::Threading::{
            OpenProcess, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, TerminateProcess,
            WaitForSingleObject,
        },
        UI::WindowsAndMessaging::{FindWindowExW, GetWindowThreadProcessId, HWND_MESSAGE},
    };

    let wide = |s: &std::ffi::OsStr| s.encode_wide().chain(Some(0)).collect::<Vec<u16>>();
    let class = wide("Chrome_MessageWindow".as_ref());
    let title = wide(p.profile.as_os_str());
    // SAFETY: both strings are NUL-terminated and outlive the call.
    let holder = || unsafe {
        FindWindowExW(
            HWND_MESSAGE,
            std::ptr::null_mut(),
            class.as_ptr(),
            title.as_ptr(),
        )
    };
    // After a pass, the browsers in Haul's job are on their way out, the job
    // closed: only one still holding the profile after a while is a
    // leftover.
    if when == "after" {
        let until = Instant::now() + STRAY_AFTER;
        while !holder().is_null() && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    // Once more should one start just as the last ends.
    for _ in 0..3 {
        let window = holder();
        if window.is_null() {
            return;
        }
        let mut pid = 0;
        // SAFETY: `window` is a window handle, `pid` a place for its process.
        unsafe { GetWindowThreadProcessId(window, &mut pid) };
        // SAFETY: no handle is inherited; the result is checked.
        let process = unsafe { OpenProcess(PROCESS_TERMINATE | PROCESS_SYNCHRONIZE, 0, pid) };
        if pid == 0 || process.is_null() {
            tracing::warn!(
                item = %p.item,
                "a browser (pid {pid}) holds {}, and Haul cannot end it: {}",
                p.profile.display(),
                std::io::Error::last_os_error()
            );
            return;
        }
        // SAFETY: `process` is open with the rights these take, and closed
        // once, here.
        let ended = unsafe {
            let ended = TerminateProcess(process, 1) != 0;
            WaitForSingleObject(process, 5_000);
            CloseHandle(process);
            ended
        };
        tracing::warn!(
            item = %p.item,
            "a browser left over (pid {pid}) held {} {when} the pass; ended: {ended}",
            p.profile.display()
        );
        haul_core::debug::emit(
            "pass",
            json!({ "event": "stray", "id": p.item, "url": p.url, "profile": p.profile.display().to_string(), "pid": pid, "ended": ended, "when": when }),
        );
        if !ended {
            return;
        }
    }
}

/// How long a browser in Haul's job gets to go after a pass before one
/// still holding the pass's profile counts as left over ([`end_stray`]).
#[cfg(windows)]
const STRAY_AFTER: Duration = Duration::from_secs(3);

#[cfg(windows)]
impl Drop for Job {
    fn drop(&mut self) {
        // SAFETY: the handle is the job's, closed once, here.
        unsafe { windows_sys::Win32::Foundation::CloseHandle(self.0) };
    }
}

impl Drop for Browser {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn drive(browser: &mut Browser, port: u16, p: &Pass<'_>) -> Result<Outcome, String> {
    let endpoint = wait_for_endpoint(browser, port)?;
    let (mut cdp, target, session) = match set_up(&endpoint, p) {
        Ok(set) => set,
        // DevTools answered, then the browser went away or would not take
        // the page: a close like any other, tried again.
        Err(error) => {
            return Ok(Outcome::Closed(Closed::SetUp {
                error,
                process: process(browser, port),
            }));
        }
    };
    let s = Some(session.as_str());
    let start = Instant::now();
    // The addresses the page last offered: a new page with as many (a
    // paste's list after its gate) is offered too.
    let mut offered: Vec<String> = Vec::new();
    // Download buttons pressed, and those found to be ads'.
    let mut presser = Presser::default();
    // Times the page was brought back from another site.
    let mut sent_back = 0;
    let mut refused = cdp.refused;
    // A browser is alive while its DevTools answers, whatever became of the
    // process Haul started: a starter, or a browser that hands over to
    // another (one updating itself), exits while the page goes on.
    let mut exited = false;
    loop {
        if p.handle.stop.load(Ordering::Relaxed) {
            return Ok(Outcome::Closed(Closed::Stopped));
        }
        if !exited && let Some(status) = browser.child.try_wait().ok().flatten() {
            exited = true;
            answered_for(&mut cdp, browser, port, p, &status);
        }
        // Waiting, but answering the page's paused navigations meanwhile.
        cdp.pump(Duration::from_millis(500));
        if let Some(to) = cdp.led_to.take() {
            return Ok(Outcome::LedTo(vec![to]));
        }
        if let Some(download) = cdp.download.take() {
            let agent = cdp
                .call(
                    "Runtime.evaluate",
                    json!({ "expression": "navigator.userAgent", "returnByValue": true }),
                    s,
                )
                .ok()
                .and_then(|a| a["result"]["value"].as_str().map(str::to_string));
            let cookie = cookies_for(&mut cdp, &download.url, s);
            return Ok(Outcome::Files {
                files: vec![File {
                    url: download.url,
                    name: download.name,
                    size: None,
                    cookie,
                }],
                agent,
                page: None,
            });
        }
        // Tabs an ad opened go; without the page's own, the window is closed.
        let Ok(targets) = cdp.call("Target.getTargets", json!({}), None) else {
            return Ok(Outcome::Closed(Closed::NoDevTools(process(browser, port))));
        };
        let open = pages(&targets);
        // Where the browser says the page is: what it shows, however it got
        // there (a navigation, or a page swapped in without one).
        let at = targets["targetInfos"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|t| t["targetId"] == target.as_str())
            .and_then(|t| t["url"].as_str())
            .map(str::to_string);
        if !open.contains(&target) {
            return Ok(Outcome::Closed(Closed::PageGone(process(browser, port))));
        }
        let mut popped = false;
        for other in open.iter().filter(|t| **t != target) {
            popped = true;
            let _ = cdp.call("Target.closeTarget", json!({ "targetId": other }), None);
        }
        // A press that sent the page off its site, or opened a tab, was an
        // ad's: that button is not pressed again.
        if (popped || cdp.refused != refused)
            && let Some(ad) = presser.left(Instant::now())
        {
            haul_core::debug::emit(
                "pass",
                json!({ "event": "ad_button", "id": p.item, "url": p.url, "button": ad }),
            );
        }
        refused = cdp.refused;
        // Debug mode's questions about the page, between looks.
        let asks: Vec<Ask> = std::mem::take(&mut *haul_core::lock::held(&p.handle.asks));
        for ask in asks {
            let answer = match ask.what {
                PageAsk::Click(selector) => click(&mut cdp, s, &selector),
                PageAsk::Cookies => cdp.call("Network.getCookies", json!({}), s).and_then(|c| {
                    let agent = cdp.call(
                        "Runtime.evaluate",
                        json!({ "expression": "navigator.userAgent", "returnByValue": true }),
                        s,
                    )?;
                    Ok(json!({ "cookies": c["cookies"], "user_agent": agent["result"]["value"] }))
                }),
                PageAsk::Look => {
                    use base64::Engine as _;
                    let seen = look(&mut cdp, s);
                    Ok(json!({
                        "page": serde_json::from_str::<Value>(&seen.page).unwrap_or(Value::String(seen.page)),
                        "screenshot_png_base64": seen.shot.map(|png| base64::engine::general_purpose::STANDARD.encode(png)),
                    }))
                }
                PageAsk::Eval(script) => cdp
                    .call(
                        "Runtime.evaluate",
                        json!({ "expression": script, "returnByValue": true, "awaitPromise": true }),
                        s,
                    )
                    .map(|a| a["result"]["value"].clone()),
            };
            let _ = ask.reply.send(answer);
        }
        if p.hand_clearance
            && let Some(cookies) = clearance(&mut cdp, p.url, s)
        {
            // Cloudflare lets the clearance through only with the user agent
            // it was given to.
            let agent = cdp
                .call(
                    "Runtime.evaluate",
                    json!({ "expression": "navigator.userAgent", "returnByValue": true }),
                    s,
                )
                .ok()
                .and_then(|a| a["result"]["value"].as_str().map(str::to_string));
            return Ok(Outcome::Cleared { cookies, agent });
        }
        // An ad that got the page off its site past the guard (a pop-under
        // that moves the tab, a redirect the guard did not see): the page
        // is sent back to where it belongs, a few times at most, rather than
        // the pass ending on the ad as if the site wanted a person.
        // Home is where the page loaded first, or, before it has, the site
        // of its own address: an ad can move it before its first load ends.
        // A walked page has no home: it leaves by its own steps, and its
        // walk refuses what no press sent it to.
        if !p.walk
            && let Some(home) = cdp
                .keep
                .as_ref()
                .map(|k| k.site.clone().unwrap_or_else(|| hosts::site_of(p.url)))
            && let Some(href) = at.clone().or_else(|| current_address(&mut cdp, s))
            && href.starts_with("http")
            && hosts::site_of(&href) != home
            && !hosts::names_a_file(&href)
        {
            sent_back += 1;
            haul_core::debug::emit(
                "pass",
                json!({ "event": "sent back", "id": p.item, "url": href, "times": sent_back }),
            );
            if sent_back <= SEND_BACK_AT_MOST {
                let _ = cdp.call("Page.navigate", json!({ "url": p.url }), s);
                continue;
            }
        }
        // A walk ends by its own clock ([`haul_core::walker`]), and by its
        // longest here when the page never let it look (still loading).
        let limit = if p.walk {
            Walker::LONGEST
        } else {
            p.show_after
        };
        if start.elapsed() >= limit {
            return Ok(Outcome::WantsPerson(look(&mut cdp, s)));
        }
        if p.walk
            && let Some(over) = walk(&mut cdp, s, p)
        {
            return Ok(over);
        }
        if p.press_download
            && let Some(text) = press_download(&mut cdp, s, &mut presser)
        {
            haul_core::debug::emit(
                "pass",
                json!({ "event": "press", "id": p.item, "url": p.url, "button": text }),
            );
        }
        let answer = match cdp.call(
            "Runtime.evaluate",
            json!({ "expression": "JSON.stringify(window.__haulHandoff || null)", "returnByValue": true }),
            s,
        ) {
            Ok(answer) => answer,
            // Between pages, or busy: ask again.
            Err(_) => continue,
        };
        let Some(sent) = answer["result"]["value"]
            .as_str()
            .and_then(|v| serde_json::from_str::<Value>(v).ok())
            .filter(|v| !v.is_null())
        else {
            continue;
        };
        // Each new message from the page, for debug mode and the detailed
        // log.
        if haul_core::lock::held(&p.handle.info)["page_said"] != sent {
            p.handle.note("page_said", sent.clone());
            haul_core::debug::emit(
                "pass",
                json!({ "event": "page", "id": p.item, "url": p.url, "said": sent }),
            );
        }
        if let Some(site) = sent["answered"]["site"].as_str() {
            let http = sent["answered"]["http"].as_u64().unwrap_or(0) as u16;
            let status = sent["answered"]["status"].as_str().unwrap_or_default();
            return Ok(match hosts::page_answer(site, http, status) {
                hosts::PageAnswer::Gone(reason) => Outcome::Gone(reason),
                hosts::PageAnswer::Later { reason, seconds } => Outcome::Later {
                    reason,
                    seconds: Some(seconds),
                },
                hosts::PageAnswer::Failed(reason) => Outcome::Failed(reason),
            });
        }
        if let Some(reason) = sent["gone"].as_str() {
            return Ok(Outcome::Gone(reason.to_string()));
        }
        if let Some(reason) = sent["failed"].as_str() {
            return Ok(Outcome::Failed(reason.to_string()));
        }
        if let Some(reason) = sent["later"].as_str() {
            return Ok(Outcome::Later {
                reason: reason.to_string(),
                seconds: sent["seconds"].as_u64(),
            });
        }
        if let (Some(take), Some(_)) = (p.take_links, sent["links"].as_array()) {
            let strings = |key: &str| -> Vec<String> {
                sent[key]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|l| l.as_str().map(str::to_string))
                    .collect()
            };
            let links = strings("links");
            if links != offered {
                offered.clone_from(&links);
                let shown = Shown {
                    links,
                    text: strings("shown"),
                    title: sent["title"].as_str().map(str::to_string),
                };
                if take(&shown) {
                    return Ok(Outcome::Listed);
                }
            }
            if p.ends_settled && sent["settled"].as_bool() == Some(true) {
                // A paste that lists nothing: what it showed, for the
                // report's log.
                if hosts::is_paste(p.url)
                    && let Some(text) = sent["text"].as_str()
                {
                    let length = sent["length"].as_u64().map(|n| n as usize);
                    hosts::keep_shown_text(&hosts::real(p.url), text, length);
                }
                return Ok(Outcome::Settled);
            }
            continue;
        }
        let mut files = Vec::new();
        for f in sent["files"].as_array().into_iter().flatten() {
            let Some(url) = f["url"].as_str().filter(|u| u.starts_with("http")) else {
                continue;
            };
            files.push(File {
                url: url.to_string(),
                name: f["name"]
                    .as_str()
                    .filter(|n| !n.is_empty())
                    .map(str::to_string),
                size: f["size"].as_u64(),
                cookie: cookies_for(&mut cdp, url, s),
            });
        }
        if !files.is_empty() {
            let agent = sent["agent"].as_str().map(str::to_string);
            let page = sent["page"]
                .as_str()
                .filter(|p| !p.is_empty())
                .map(str::to_string);
            return Ok(Outcome::Files { files, agent, page });
        }
    }
}

/// Connects to the browser's DevTools at `endpoint`, attaches to its page,
/// adds the pass's scripts and sends the page to `p.url`: the connection,
/// the page's target and its session.
fn set_up(endpoint: &str, p: &Pass<'_>) -> Result<(Cdp, String, String), String> {
    let mut cdp = Cdp::connect(endpoint)?;
    let targets = cdp.call("Target.getTargets", json!({}), None)?;
    let target = pages(&targets)
        .first()
        .cloned()
        .ok_or("the browser opened no page")?;
    let attached = cdp.call(
        "Target.attachToTarget",
        json!({ "targetId": target, "flatten": true }),
        None,
    )?;
    let session = attached["sessionId"]
        .as_str()
        .ok_or("no DevTools session")?
        .to_string();
    let s = Some(session.as_str());
    // Scripts for new documents run only with the page domain on (unlike
    // the runtime domain, it tells no automation apart).
    cdp.call("Page.enable", json!({}), s)?;
    cdp.call(
        "Page.addScriptToEvaluateOnNewDocument",
        json!({ "source": "window.__haulDevTools = true;" }),
        s,
    )?;
    for script in p.scripts {
        cdp.call(
            "Page.addScriptToEvaluateOnNewDocument",
            json!({ "source": script }),
            s,
        )?;
    }
    // A walk decides where the page may go, which takes its navigations.
    if p.keep_to_site || p.walk {
        // A wrapper's or a paste's page belongs to its own site from the
        // start; another page may first move to its real site, which then
        // becomes its home.
        let home = (hosts::wraps(p.url) || hosts::is_paste(p.url)).then(|| hosts::site_of(p.url));
        cdp.keep_to_site(p.item, &target, home, s)?;
    }
    if p.walk {
        cdp.walk = Some(Walker::new(Instant::now()));
    }
    if let Err(e) = cdp.take_downloads() {
        tracing::warn!("the browser will not report downloads: {e}");
    }
    cdp.call("Page.navigate", json!({ "url": p.url }), s)?;
    Ok((cdp, target, session))
}

/// How long DevTools' port is still watched after the process Haul started
/// exited before it opened: a starter, or a browser handing over to another,
/// exits while the browser it started opens DevTools (Edge takes about half
/// a second).
const EXITED_AT_START: Duration = Duration::from_secs(5);

/// The browser's DevTools WebSocket, from its `/json/version` once it
/// listens. (`DevToolsActivePort` in the profile is written only when the
/// browser picks the port itself.)
/// Times a pass brings its page back from another site before it lets the
/// page be.
const SEND_BACK_AT_MOST: u32 = 5;

/// The address the page's main frame is at, as it says.
fn current_address(cdp: &mut Cdp, s: Option<&str>) -> Option<String> {
    cdp.call(
        "Runtime.evaluate",
        json!({ "expression": "location.href", "returnByValue": true }),
        s,
    )
    .ok()?["result"]["value"]
        .as_str()
        .map(str::to_string)
}

/// Asks the browser on DevTools' `port` to close, and waits a little for the
/// process Haul started to exit; what is left is killed as before.
fn close_gently(browser: &mut Browser, port: u16) {
    if browser.child.try_wait().ok().flatten().is_some() {
        return;
    }
    let Some(endpoint) = version(port)
        .as_ref()
        .and_then(|v| v["webSocketDebuggerUrl"].as_str())
        .map(str::to_string)
    else {
        return;
    };
    if let Ok(mut cdp) = Cdp::connect(&endpoint) {
        let _ = cdp.call("Browser.close", json!({}), None);
    }
    let until = Instant::now() + Duration::from_secs(3);
    while Instant::now() < until {
        if browser.child.try_wait().ok().flatten().is_some() {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Marks the profile at `profile` as closed cleanly, whatever ended its last
/// browser (Haul, an update, a crash): Edge then shows no "Restore pages"
/// bubble. A profile with no preferences yet is left alone.
pub(crate) fn mark_closed_cleanly(profile: &std::path::Path) {
    let file = profile.join("Default").join("Preferences");
    let Ok(text) = std::fs::read_to_string(&file) else {
        return;
    };
    let Ok(mut prefs) = serde_json::from_str::<Value>(&text) else {
        return;
    };
    if prefs["profile"]["exit_type"] == "Normal" && prefs["profile"]["exited_cleanly"] == true {
        return;
    }
    if !prefs["profile"].is_object() {
        prefs["profile"] = json!({});
    }
    prefs["profile"]["exit_type"] = json!("Normal");
    prefs["profile"]["exited_cleanly"] = json!(true);
    if let Ok(text) = serde_json::to_string(&prefs) {
        let _ = std::fs::write(&file, text);
    }
}

fn wait_for_endpoint(browser: &mut Browser, port: u16) -> Result<String, String> {
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut exited = None;
    while Instant::now() < deadline {
        if exited.is_none()
            && let Some(status) = browser.child.try_wait().ok().flatten()
        {
            exited = Some((status, Instant::now()));
        }
        if let Some(url) = version(port)
            .as_ref()
            .and_then(|v| v["webSocketDebuggerUrl"].as_str())
        {
            return Ok(url.to_string());
        }
        if let Some((status, at)) = exited
            && at.elapsed() >= EXITED_AT_START
        {
            return Err(format!(
                "the browser exited at start ({status}), and nothing opened DevTools on its port"
            ));
        }
        std::thread::sleep(Duration::from_millis(150));
    }
    Err("the browser did not open DevTools in time".into())
}

/// What answers on DevTools' port: the browser's name and version.
fn answers(port: u16) -> Option<String> {
    let v = version(port)?;
    Some(v["Browser"].as_str().unwrap_or("a DevTools").to_string())
}

/// What became of the process Haul started, and what answers on DevTools'
/// port, as a pass ends.
fn process(browser: &mut Browser, port: u16) -> Process {
    // A browser that went drops its DevTools connection before Windows has
    // it gone (a killed Edge took about 0.3 s): it gets a moment to be, so
    // one that died is not told as running.
    let until = Instant::now() + PROCESS_GOING;
    let exited = loop {
        match browser.child.try_wait() {
            Ok(Some(status)) => break Some(status.to_string()),
            Ok(None) if Instant::now() < until => std::thread::sleep(Duration::from_millis(50)),
            _ => break None,
        }
    };
    Process {
        exited,
        answers: answers(port),
    }
}

/// How long the process Haul started gets to be gone once DevTools stopped
/// answering or lost the page, before it is told as still running.
const PROCESS_GOING: Duration = Duration::from_secs(1);

/// The process Haul started exited (`status`) while DevTools answers: says
/// so, with what answers and its process, whose windows are kept off the
/// taskbar from now on.
fn answered_for(
    cdp: &mut Cdp,
    browser: &Browser,
    port: u16,
    p: &Pass<'_>,
    status: &std::process::ExitStatus,
) {
    let pid = cdp
        .call("SystemInfo.getProcessInfo", json!({}), None)
        .ok()
        .and_then(|info| {
            info["processInfo"]
                .as_array()?
                .iter()
                .find(|p| p["type"] == "browser")?["id"]
                .as_u64()
        })
        .and_then(|pid| u32::try_from(pid).ok());
    if let Some(pid) = pid {
        browser.hide.store(pid, Ordering::Relaxed);
    }
    let answers = answers(port);
    tracing::warn!(
        item = %p.item,
        "the browser Haul started (pid {}) exited ({status}); DevTools still answers: {} (pid {})",
        browser.child.id(),
        answers.as_deref().unwrap_or("nothing"),
        pid.map_or("unknown".into(), |pid| pid.to_string()),
    );
    haul_core::debug::emit(
        "pass",
        json!({ "event": "exited", "id": p.item, "url": p.url, "pid": browser.child.id(), "exit": status.to_string(), "answers": answers, "answering_pid": pid }),
    );
}

/// `GET /json/version`, in plain HTTP/1.1: the browser's name
/// (`Browser`) and its `webSocketDebuggerUrl`.
fn version(port: u16) -> Option<Value> {
    use std::io::{Read, Write};
    let mut stream = TcpStream::connect(("127.0.0.1", port)).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
    write!(
        stream,
        "GET /json/version HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
    )
    .ok()?;
    // The browser keeps the connection open: read up to the body's length.
    let mut answer = Vec::new();
    let mut chunk = [0u8; 4096];
    let body = loop {
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            break String::from_utf8_lossy(&answer)
                .split_once("\r\n\r\n")?
                .1
                .to_string();
        }
        answer.extend_from_slice(&chunk[..n]);
        let text = String::from_utf8_lossy(&answer);
        if let Some((head, body)) = text.split_once("\r\n\r\n") {
            let length = head
                .lines()
                .find_map(|l| {
                    l.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(|v| v.trim().parse::<usize>().ok())
                })
                .flatten();
            if length.is_some_and(|len| body.len() >= len) {
                break body.to_string();
            }
        }
    };
    serde_json::from_str(body.trim()).ok()
}

/// The ids of the browser's pages (tabs), in its order.
fn pages(targets: &Value) -> Vec<String> {
    targets["targetInfos"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|t| t["type"] == "page")
        .filter_map(|t| t["targetId"].as_str().map(str::to_string))
        .collect()
}

/// The browser's cookies for `url`, as a `Cookie` header.
fn cookies_for(cdp: &mut Cdp, url: &str, session: Option<&str>) -> Option<String> {
    let got = cdp
        .call("Network.getCookies", json!({ "urls": [url] }), session)
        .ok()?;
    let pairs: Vec<String> = got["cookies"]
        .as_array()?
        .iter()
        .filter_map(|c| Some(format!("{}={}", c["name"].as_str()?, c["value"].as_str()?)))
        .collect();
    (!pairs.is_empty()).then(|| pairs.join("; "))
}

/// Whether the page is on its site past Cloudflare's check: loaded, and
/// not the check's own page, which answers 403 (or 503) and reloads the
/// page once it passes. The check's scripts and title tell nothing: the
/// page past it may keep `_cf_chl_opt` (filecrypt.cc's does), and the
/// title is in the browser's language.
const PAST_CHECK: &str = r#"(() => {
  if (!location.protocol.startsWith('http') || document.readyState === 'loading') return false;
  const status = performance.getEntriesByType('navigation')[0]?.responseStatus;
  return status ? status < 400 : !/^just a moment/i.test(document.title);
})()"#;

/// Once the page is past the check in front of its site, the cookies
/// Cloudflare set for it (its clearance among them), as `Set-Cookie` lines;
/// `None` while the check is still on.
fn clearance(cdp: &mut Cdp, url: &str, session: Option<&str>) -> Option<Vec<String>> {
    let got = cdp
        .call("Network.getCookies", json!({ "urls": [url] }), session)
        .ok()?;
    let theirs: Vec<&Value> = got["cookies"]
        .as_array()?
        .iter()
        .filter(|c| {
            c["name"]
                .as_str()
                .is_some_and(|n| n.starts_with("cf_") || n.starts_with("__cf"))
        })
        .collect();
    if !theirs.iter().any(|c| c["name"] == "cf_clearance") {
        return None;
    }
    let past = cdp
        .call(
            "Runtime.evaluate",
            json!({ "expression": PAST_CHECK, "returnByValue": true }),
            session,
        )
        .ok()?;
    if past["result"]["value"].as_bool() != Some(true) {
        return None;
    }
    let lines = theirs
        .iter()
        .filter_map(|c| {
            let mut line = format!(
                "{}={}; Domain={}; Path={}",
                c["name"].as_str()?,
                c["value"].as_str()?,
                c["domain"].as_str()?,
                c["path"].as_str().unwrap_or("/"),
            );
            if c["secure"].as_bool() == Some(true) {
                line.push_str("; Secure");
            }
            if c["httpOnly"].as_bool() == Some(true) {
                line.push_str("; HttpOnly");
            }
            Some(line)
        })
        .collect();
    Some(lines)
}

/// A DevTools connection to the browser.
struct Cdp {
    ws: WebSocket<MaybeTlsStream<TcpStream>>,
    next: u64,
    /// Keeps the page on its site ([`Cdp::keep_to_site`]).
    keep: Option<Keep>,
    /// The file the page's site answered with instead of a page, which the
    /// browser was told not to save ([`Cdp::take_downloads`]).
    download: Option<Download>,
    /// Navigations of the page refused so far ([`Cdp::keep_to_site`]).
    refused: u32,
    /// The walk of a page on a site Haul has no code for ([`Pass::walk`]),
    /// which decides where its main frame may go.
    walk: Option<Walker>,
    /// Where the walked page went on a site Haul has code for, refused so
    /// Haul goes on from there itself ([`Outcome::LedTo`]).
    led_to: Option<String>,
}

/// A file the browser would have saved: its final address, past redirects.
#[derive(Debug, PartialEq)]
struct Download {
    url: String,
    name: Option<String>,
}

/// The file `Browser.downloadWillBegin` tells of.
fn download_in(event: &Value) -> Option<Download> {
    if event["method"].as_str() != Some("Browser.downloadWillBegin") {
        return None;
    }
    let url = event["params"]["url"].as_str()?;
    Some(Download {
        url: url.starts_with("http").then(|| url.to_string())?,
        name: event["params"]["suggestedFilename"]
            .as_str()
            .filter(|n| !n.is_empty())
            .map(str::to_string),
    })
}

/// A page kept on the site it landed on: its main frame is refused any
/// other site once its first page has loaded there.
struct Keep {
    /// The item whose page it is, for the log.
    item: String,
    /// The page's main frame (the target's id).
    frame: String,
    /// The site of the last page the main frame was let go to.
    last: Option<String>,
    /// Where its first page loaded: set at the first load.
    site: Option<String>,
}

impl Cdp {
    fn connect(endpoint: &str) -> Result<Self, String> {
        let (ws, _) = tungstenite::connect(endpoint).map_err(|e| format!("DevTools: {e}"))?;
        if let MaybeTlsStream::Plain(stream) = ws.get_ref() {
            let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
        }
        Ok(Self {
            ws,
            next: 0,
            keep: None,
            download: None,
            refused: 0,
            walk: None,
            led_to: None,
        })
    }

    /// Has the browser report files instead of saving them. A site that
    /// answers a form with the file (ddownload.com does) would have it go to
    /// the person's Downloads folder, out of sight, with Haul none the wiser:
    /// reported, Haul downloads it itself, and can resume it.
    fn take_downloads(&mut self) -> Result<(), String> {
        self.call(
            "Browser.setDownloadBehavior",
            json!({ "behavior": "deny", "eventsEnabled": true }),
            None,
        )
        .map(|_| ())
    }

    /// Keeps page `frame` (of `session`) on the site its first page lands
    /// on: a page that sends the whole tab to an ad's site (filecrypt.cc
    /// does, half a second after it shows) is refused that navigation, as
    /// Haul's own window refuses it. Frames inside the page (a check's) and
    /// the way to the first page go as they will.
    fn keep_to_site(
        &mut self,
        item: &str,
        frame: &str,
        home: Option<String>,
        session: Option<&str>,
    ) -> Result<(), String> {
        // Home is the page's own site from the start: an ad that moves the
        // page before its first load ends would otherwise become it.
        self.keep = Some(Keep {
            item: item.to_string(),
            frame: frame.to_string(),
            last: home.clone(),
            site: home,
        });
        self.call(
            "Fetch.enable",
            json!({ "patterns": [{ "resourceType": "Document", "requestStage": "Request" }] }),
            session,
        )
        .map(|_| ())
    }

    /// Answers an event that waits for one: a paused navigation, let go or
    /// refused ([`Cdp::keep_to_site`]); and notes the first page's load.
    fn on_event(&mut self, event: &Value) {
        if let Some(download) = download_in(event) {
            tracing::info!(url = %download.url, "the page's site answered with a file");
            self.download = Some(download);
            return;
        }
        let Some(keep) = self.keep.as_mut() else {
            return;
        };
        match event["method"].as_str() {
            Some("Page.loadEventFired") => {
                if keep.site.is_none() {
                    keep.site.clone_from(&keep.last);
                }
            }
            Some("Fetch.requestPaused") => {
                let p = &event["params"];
                let url = p["request"]["url"].as_str().unwrap_or_default();
                let main = p["frameId"].as_str() == Some(keep.frame.as_str());
                let site = url.starts_with("http").then(|| hosts::site_of(url));
                let elsewhere = main && site.is_some() && site != keep.last;
                let refuse = match self.walk.as_mut() {
                    // A walked page goes to another site only as a press's
                    // doing, and not onto a site Haul has code for, where
                    // Haul goes on itself.
                    Some(walk) if elsewhere => {
                        let real = hosts::real(url);
                        if hosts::walk_ends_at(&real) && !hosts::names_a_file(url) {
                            tracing::info!(item = %keep.item, url = %real, "the walked page leads to a site Haul knows");
                            self.led_to = Some(real.into_owned());
                            true
                        } else {
                            !walk.may_leave(Instant::now())
                        }
                    }
                    Some(_) => false,
                    None => main && site.is_some() && keep.site.is_some() && site != keep.site,
                };
                if main && !refuse && site.is_some() {
                    keep.last = site.clone();
                }
                // Going to another site for a file (a host's download server,
                // a CDN) is the file, whichever site the page is on: Haul
                // takes it, and the page stays where it is.
                if refuse && hosts::names_a_file(url) {
                    tracing::info!(url, "the page went on to a file on another site");
                    self.download = Some(Download {
                        url: url.to_string(),
                        name: hosts::guess_name(url),
                    });
                }
                let (method, params) = if refuse {
                    self.refused += 1;
                    haul_core::debug::emit(
                        "pass",
                        json!({ "event": "refused", "id": keep.item, "url": url }),
                    );
                    (
                        // A navigation answered with no content goes nowhere:
                        // the page stays (failing it shows an error page).
                        "Fetch.fulfillRequest",
                        json!({ "requestId": p["requestId"], "responseCode": 204 }),
                    )
                } else {
                    (
                        "Fetch.continueRequest",
                        json!({ "requestId": p["requestId"] }),
                    )
                };
                self.next += 1;
                let mut message = json!({ "id": self.next, "method": method, "params": params });
                if let Some(session) = event["sessionId"].as_str() {
                    message["sessionId"] = json!(session);
                }
                let _ = self.ws.send(Message::text(message.to_string()));
            }
            _ => {}
        }
    }

    /// Reads what comes for up to `wait`, answering events that wait.
    fn pump(&mut self, wait: Duration) {
        let until = Instant::now() + wait;
        while Instant::now() < until {
            match self.ws.read() {
                Ok(Message::Text(text)) => {
                    if let Ok(event) = serde_json::from_str::<Value>(text.as_str())
                        && event.get("method").is_some()
                    {
                        self.on_event(&event);
                    }
                }
                Ok(_) => {}
                Err(tungstenite::Error::Io(e))
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) => {}
                Err(_) => return,
            }
        }
    }

    /// Calls `method` (on the page when `session` names it) and waits for
    /// its answer, passing over the events that come meanwhile.
    fn call(
        &mut self,
        method: &str,
        params: Value,
        session: Option<&str>,
    ) -> Result<Value, String> {
        self.next += 1;
        let id = self.next;
        let mut message = json!({ "id": id, "method": method, "params": params });
        if let Some(session) = session {
            message["sessionId"] = json!(session);
        }
        self.ws
            .send(Message::text(message.to_string()))
            .map_err(|e| format!("DevTools: {e}"))?;
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if Instant::now() > deadline {
                return Err(format!("DevTools: no answer to {method}"));
            }
            match self.ws.read() {
                Ok(Message::Text(text)) => {
                    let Ok(answer) = serde_json::from_str::<Value>(text.as_str()) else {
                        continue;
                    };
                    if answer.get("method").is_some() {
                        self.on_event(&answer);
                        continue;
                    }
                    if answer["id"].as_u64() != Some(id) {
                        continue;
                    }
                    if let Some(error) = answer.get("error") {
                        return Err(format!("DevTools {method}: {error}"));
                    }
                    return Ok(answer["result"].clone());
                }
                Ok(_) => {}
                Err(tungstenite::Error::Io(e))
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) => {}
                Err(e) => return Err(format!("DevTools: {e}")),
            }
        }
    }
}

/// The taskbar buttons of the browser's windows. A page passed out of sight
/// shows nowhere, but each browser window still gets a button, and a queue
/// of pages had Edge buttons come and go one after another.
#[cfg(windows)]
mod taskbar {
    use windows_sys::{
        Win32::{
            Foundation::{HWND, LPARAM},
            UI::WindowsAndMessaging::{
                EnumWindows, GW_OWNER, GWL_EXSTYLE, GetWindow, GetWindowLongPtrW,
                GetWindowThreadProcessId, IsWindowVisible, SW_HIDE, SW_SHOWNOACTIVATE,
                SetWindowLongPtrW, ShowWindow, WS_EX_APPWINDOW, WS_EX_TOOLWINDOW,
            },
        },
        core::BOOL,
    };

    /// The visible top-level windows of process `pid` that own no other.
    fn windows_of(pid: u32) -> Vec<HWND> {
        struct Search {
            pid: u32,
            found: Vec<HWND>,
        }
        unsafe extern "system" fn each(hwnd: HWND, lparam: LPARAM) -> BOOL {
            // SAFETY: `lparam` is the `Search` below, alive for the call.
            let search = unsafe { &mut *(lparam as *mut Search) };
            let mut owner = 0;
            // SAFETY: plain queries on a window handle Windows gave us.
            unsafe {
                GetWindowThreadProcessId(hwnd, &mut owner);
                if owner == search.pid
                    && IsWindowVisible(hwnd) != 0
                    && GetWindow(hwnd, GW_OWNER).is_null()
                {
                    search.found.push(hwnd);
                }
            }
            1
        }
        let mut search = Search {
            pid,
            found: Vec::new(),
        };
        // SAFETY: the callback only touches `search`, which outlives the call.
        unsafe { EnumWindows(Some(each), &mut search as *mut Search as LPARAM) };
        search.found
    }

    /// Sets or clears the tool-window style, which keeps a window off the
    /// taskbar; the change shows once the window is hidden and shown again.
    fn set_tool(hwnd: HWND, tool: bool) {
        // SAFETY: reading and writing a window's extended style.
        unsafe {
            let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
            let want = if tool {
                (style | WS_EX_TOOLWINDOW) & !WS_EX_APPWINDOW
            } else {
                (style & !WS_EX_TOOLWINDOW) | WS_EX_APPWINDOW
            };
            if want == style {
                return;
            }
            ShowWindow(hwnd, SW_HIDE);
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, want as isize);
            ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        }
    }

    pub fn hide(pid: u32) {
        for hwnd in windows_of(pid) {
            set_tool(hwnd, true);
        }
    }
}

#[cfg(not(windows))]
mod taskbar {
    pub fn hide(_pid: u32) {}
}

/// What a page showed when it was given up on, for the log.
#[derive(Debug)]
pub struct Look {
    /// Its title, address, frames, scripts, Turnstile answers, buttons and
    /// first 50 links, as JSON.
    pub page: String,
    /// A PNG of it, when the browser gave one.
    pub shot: Option<Vec<u8>>,
}

const LOOK: &str = r#"JSON.stringify({
  title: document.title,
  url: location.href,
  frames: [...document.querySelectorAll('iframe')].map((f) => f.src.slice(0, 100)),
  scripts: [...document.scripts].map((s) => s.src).filter((s) => /^https?:/.test(s)).map((s) => s.slice(0, 100)).slice(0, 30),
  turnstile: [...document.querySelectorAll('[name="cf-turnstile-response"]')].map((i) => (i.value ? 'answered' : 'empty')),
  buttons: [...document.querySelectorAll('button, input[type=submit]')]
    .filter((b) => b.offsetParent)
    .map((b) => (b.innerText || b.value || '').trim().slice(0, 40)),
  links: [...document.querySelectorAll('a[href]')].map((a) => a.href).filter((h) => /^https?:/.test(h)).slice(0, 50),
})"#;

/// Lists the buttons a person might press to download the file on a host's
/// page Haul has no recipe for, in the page's order: shown, enabled, saying
/// "download" (but not a paid tier's, an app's or a countdown's), in the page
/// itself rather than a layer an ad floats over it, and leading nowhere off
/// the site (an ad's link does). Each is keyed by its place among the
/// page's buttons and its words; Rust picks one ([`Presser`]).
const LIST_DOWNLOADS: &str = r#"(() => {
  if (document.readyState === 'loading') return null;
  const site = (host) => host.split('.').slice(-2).join('.');
  const here = site(location.hostname);
  const says = /download/i;
  const not = /premium|fast(er)?\s*download|\bvip\b|install|extension|\bapp\b|torrent|manager|browser|register|sign\s*up|log\s*in|creating|generating|please\s+wait|\bwait\b/i;
  const floats = (el) => {
    for (let e = el; e && e !== document.body; e = e.parentElement) {
      if (getComputedStyle(e).position === 'fixed') return true;
    }
    return false;
  };
  const offSite = (el) => {
    const href = el.getAttribute('href');
    if (!href || href.startsWith('#') || href.startsWith('javascript:')) return false;
    try { return site(new URL(href, location.href).hostname) !== here; } catch { return true; }
  };
  const words = (el) => (el.innerText || el.value || el.getAttribute('aria-label') || '').trim();
  const all = [...document.querySelectorAll('a, button, input[type=submit], input[type=button]')];
  return all.flatMap((el, at) => {
    const text = words(el);
    if (!says.test(text) || not.test(text) || text.length > 60) return [];
    if (el.disabled || el.getAttribute('aria-disabled') === 'true') return [];
    if (!el.offsetParent || floats(el) || offSite(el)) return [];
    const box = el.getBoundingClientRect();
    if (box.width <= 0 || box.height <= 0) return [];
    return [{ key: `${at} ${text.slice(0, 40)}`, text: text.slice(0, 40) }];
  });
})()"#;

/// Brings the button keyed `KEY` ([`LIST_DOWNLOADS`]) into view and gives
/// its middle; `null` when the page changed under it.
const BUTTON_AT: &str = r#"((key) => {
  const at = parseInt(key, 10);
  const el = document.querySelectorAll('a, button, input[type=submit], input[type=button]')[at];
  const text = el && (el.innerText || el.value || el.getAttribute('aria-label') || '').trim().slice(0, 40);
  if (!el || `${at} ${text}` !== key) return null;
  el.scrollIntoView({ block: 'center' });
  const box = el.getBoundingClientRect();
  return { x: box.x + box.width / 2, y: box.y + box.height / 2 };
})(KEY)"#;

/// Presses the download button [`Presser`] picks among those the page
/// lists ([`LIST_DOWNLOADS`]), as a person would; the button's words, when
/// one was pressed.
fn press_download(cdp: &mut Cdp, s: Option<&str>, presser: &mut Presser) -> Option<String> {
    let listed = cdp
        .call(
            "Runtime.evaluate",
            json!({ "expression": LIST_DOWNLOADS, "returnByValue": true }),
            s,
        )
        .ok()?["result"]["value"]
        .clone();
    let listed: Vec<Candidate> = listed
        .as_array()?
        .iter()
        .filter_map(|c| {
            Some(Candidate {
                key: c["key"].as_str()?.to_string(),
                text: c["text"].as_str().unwrap_or_default().to_string(),
            })
        })
        .collect();
    let next = presser.pick(&listed, Instant::now())?;
    let find = BUTTON_AT.replace("KEY", &serde_json::to_string(&next.key).ok()?);
    let at = cdp
        .call(
            "Runtime.evaluate",
            json!({ "expression": find, "returnByValue": true }),
            s,
        )
        .ok()?["result"]["value"]
        .clone();
    let (Some(x), Some(y)) = (at["x"].as_f64(), at["y"].as_f64()) else {
        return None;
    };
    click_at(cdp, s, x, y).ok()?;
    Some(next.text.clone())
}

/// Clicks at `x`, `y` with the mouse events a person's click makes.
fn click_at(cdp: &mut Cdp, s: Option<&str>, x: f64, y: f64) -> Result<(), String> {
    for (kind, buttons) in [("mouseMoved", 0), ("mousePressed", 1), ("mouseReleased", 0)] {
        cdp.call(
            "Input.dispatchMouseEvent",
            json!({ "type": kind, "x": x, "y": y, "button": "left", "buttons": buttons, "clickCount": 1 }),
            s,
        )?;
    }
    Ok(())
}

/// The steps a walked page offers ([`haul_core::walker`]), and whether its
/// captcha still waits for its answer: what a person would press to go on
/// (shown, enabled, a label of a few words that says go on, get the link,
/// verify or download, and not a paid tier's, an app's or a sign-up's; a
/// link to another page only when it is drawn as a button, since an
/// article's own links use the same words), those in view and on the page's
/// own site first. Each is keyed by its place among the
/// page's buttons and its words, and says where it links when that is
/// another site. `null` while the page loads.
const WALK_STEPS: &str = r#"(() => {
  if (document.readyState === 'loading') return null;
  const GO = /\b(continue|get\s*(the\s*|your\s*)?link|go\s*to\s*(the\s*|your\s*)?(link|url|download)|skip\s*ad|verify|proceed|click\s*here|unlock|open\s*link|i'?m?\s*(am\s*)?not\s*a\s*robot|generate|show\s*link|visit\s*link|get\s*url|download|weiter|fortfahren|continuer|suivant|continuar|siguiente|continua|avanti|prosegui|t[ée]l[ée]charger|herunterladen|descargar|scarica(re)?|baixar)\b|далее|продолжить|скачать|继续|下一步|下载|ダウンロード/i;
  // Words that are a step only as the whole label.
  const ALONE = /^\W*(go|next|link|skip|human|start)\W*$/i;
  const AD = /\b(premium|usenet|vpn|install|sponsor|advert|sign\s*up|register|log\s*in|login|subscribe|notification|allow|app|extension|faster|high.?speed|payment|payout|proof|torrent)\b/i;
  const site = (host) => host.split('.').slice(-2).join('.');
  const shown = (el) => {
    const box = el.getBoundingClientRect();
    if (box.width < 8 || box.height < 8) return false;
    for (let e = el; e && e.nodeType === 1; e = e.parentElement) {
      const s = getComputedStyle(e);
      if (s.display === 'none' || s.visibility === 'hidden' || Number(s.opacity) < 0.1) return false;
    }
    return true;
  };
  const words = (el) => (el.innerText || el.value || el.getAttribute('aria-label') || el.title || '').trim().replace(/\s+/g, ' ');
  // A link drawn as a button: by its class, or a filled or framed box of
  // its own or of what it holds.
  const drawn = (el) => {
    if (/btn|button/i.test(`${el.className} ${el.id}`)) return true;
    return [el, ...el.children].some((n) => {
      const s = getComputedStyle(n);
      const filled = s.backgroundColor && !/rgba\(0, 0, 0, 0\)|transparent/.test(s.backgroundColor);
      return filled || (parseFloat(s.borderTopWidth) > 0 && s.borderTopStyle !== 'none');
    });
  };
  const all = [...document.querySelectorAll('button, input[type=submit], input[type=button], a, [role=button], [onclick]')];
  const steps = all.flatMap((el, at) => {
    const text = words(el).slice(0, 50);
    if (!text || text.length > 40 || text.split(' ').length > 6 || AD.test(text) || !shown(el)) return [];
    if (!GO.test(text) && !ALONE.test(text)) return [];
    if (el.disabled || el.getAttribute('aria-disabled') === 'true' || /\bdisabled\b/.test(el.className)) return [];
    let leads = null;
    const href = el.tagName === 'A' ? el.getAttribute('href') || '' : '';
    if (href && !/^(#|javascript:)/i.test(href)) {
      try {
        const to = new URL(href, location.href);
        if (/^https?:$/.test(to.protocol) && site(to.hostname) !== site(location.hostname)) leads = to.href;
      } catch {}
    }
    // A link in the page's text that goes to another page is the page's
    // own (an article's title, "Continue reading"), whatever its words.
    const plain = el.tagName === 'A' && Boolean(href) && !/^javascript:/i.test(href) && el.getAttribute('role') !== 'button' && !el.hasAttribute('onclick') && !drawn(el);
    const box = el.getBoundingClientRect();
    const rank = (leads ? 2 : 0) + (box.top >= 0 && box.bottom <= innerHeight ? 0 : 1);
    return [{ key: `${at} ${text}`, text, leads, rank, plain }];
  });
  steps.sort((a, b) => a.rank - b.rank);
  const waits = [...document.querySelectorAll('[name=cf-turnstile-response], [name=g-recaptcha-response], [name=h-captcha-response]')].some((e) => !e.value);
  return { url: location.href, waits, steps };
})()"#;

/// Brings the step keyed `KEY` ([`WALK_STEPS`]) into view, keeps what it
/// opens in this tab, and gives the middle of it to click. What lies over
/// it there is seen to first: an empty layer or an ad's frame stops taking
/// clicks; a layer with words is a dialog (a cookie banner), answered with
/// its accept or close button when it has one (`answer`), or told of
/// (`blocked`). `null` when the page changed under it.
const WALK_STEP_AT: &str = r#"((key) => {
  const at = parseInt(key, 10);
  const el = document.querySelectorAll('button, input[type=submit], input[type=button], a, [role=button], [onclick]')[at];
  const words = (e) => (e.innerText || e.value || e.getAttribute('aria-label') || e.title || '').trim().replace(/\s+/g, ' ');
  if (!el || `${at} ${words(el).slice(0, 50)}` !== key) return null;
  const form = el.closest('form');
  if (form && form.target && form.target !== '_self') form.target = '_self';
  const link = el.closest('a[target]');
  if (link) link.target = '_self';
  el.scrollIntoView({ block: 'center', behavior: 'instant' });
  let best = null;
  for (const n of [el, ...el.querySelectorAll('*')]) {
    const r = n.getBoundingClientRect();
    const area = r.width * r.height;
    if (area > 0 && (!best || area > best.area)) best = { area, r };
  }
  if (!best) return null;
  const x = best.r.x + best.r.width / 2;
  const y = best.r.y + best.r.height / 2;
  const YES = /^(accept|agree|consent|i agree|allow all|accept all|got it|ok|okay|close|no,? thanks|continue|×|x|yes)\b/i;
  for (let k = 0; k < 8; k++) {
    const over = document.elementFromPoint(x, y);
    if (!over || over === el || el.contains(over) || over.contains(el)) break;
    const said = (over.innerText || '').trim();
    if (said && over.tagName !== 'IFRAME') {
      let root = over;
      for (let p = over; p && p !== document.body; p = p.parentElement) {
        const s = getComputedStyle(p);
        if (s.position === 'fixed' || s.position === 'absolute' || p.getAttribute('role') === 'dialog' || p.tagName === 'DIALOG') root = p;
      }
      const yes = [...root.querySelectorAll('button, a, [role=button], input[type=button], input[type=submit]')]
        .map((b) => ({ t: words(b).slice(0, 40), r: b.getBoundingClientRect() }))
        .find((b) => b.t && b.r.width > 4 && YES.test(b.t));
      const blocked = said.replace(/\s+/g, ' ').slice(0, 160);
      return yes ? { blocked, answer: { text: yes.t, x: yes.r.x + yes.r.width / 2, y: yes.r.y + yes.r.height / 2 } } : { blocked };
    }
    over.style.setProperty('pointer-events', 'none', 'important');
  }
  return { x, y };
})(KEY)"#;

/// One move of the walk ([`Pass::walk`]) on the page as it is now: a step
/// pressed, the page loaded again for a new captcha, or nothing yet. Once
/// the walk is over, the pass's outcome: the page wants a person while its
/// captcha never answered itself, and has shown all it will otherwise.
fn walk(cdp: &mut Cdp, s: Option<&str>, p: &Pass<'_>) -> Option<Outcome> {
    let seen = cdp
        .call(
            "Runtime.evaluate",
            json!({ "expression": WALK_STEPS, "returnByValue": true }),
            s,
        )
        .ok()?["result"]["value"]
        .clone();
    let address = seen["url"].as_str()?.to_string();
    let steps: Vec<Step> = seen["steps"]
        .as_array()?
        .iter()
        .filter_map(|c| {
            let leads = c["leads"].as_str().map(|l| hosts::real(l).into_owned());
            // A plain link is a step only as the way to a site Haul knows.
            if c["plain"].as_bool() == Some(true)
                && !leads.as_deref().is_some_and(hosts::walk_ends_at)
            {
                return None;
            }
            Some(Step {
                key: c["key"].as_str()?.to_string(),
                text: c["text"].as_str().unwrap_or_default().to_string(),
                leads,
            })
        })
        .collect();
    let waits = seen["waits"].as_bool().unwrap_or(false);
    // Steps that link to pages on sites Haul has code for are where the
    // walk was going: all of them, since a mirror index offers one per
    // host and any of them may be gone. A page read for its links hands
    // over every link it shows instead ([`Pass::take_links`]).
    let mut known: Vec<String> = Vec::new();
    for to in steps.iter().filter_map(|s| s.leads.as_ref()) {
        if hosts::walk_ends_at(to) && !known.contains(to) {
            known.push(to.clone());
        }
    }
    if !known.is_empty() && p.take_links.is_none() {
        return Some(Outcome::LedTo(known));
    }
    let next = cdp
        .walk
        .as_mut()?
        .next(&address, &steps, waits, Instant::now());
    let emit = |event: &str, more: Value| {
        let mut signal = json!({ "event": event, "id": p.item, "url": p.url, "at": address });
        if let (Value::Object(signal), Value::Object(more)) = (&mut signal, more) {
            signal.extend(more);
        }
        haul_core::debug::emit("pass", signal);
    };
    match next {
        Next::Wait => None,
        Next::Over if waits => Some(Outcome::WantsPerson(look(cdp, s))),
        Next::Over => Some(Outcome::Settled),
        Next::Reload => {
            emit("reload", json!({}));
            let _ = cdp.call("Page.reload", json!({}), s);
            None
        }
        Next::Press(step) => {
            let step = step.clone();
            let find = WALK_STEP_AT.replace("KEY", &serde_json::to_string(&step.key).ok()?);
            let at = cdp
                .call(
                    "Runtime.evaluate",
                    json!({ "expression": find, "returnByValue": true }),
                    s,
                )
                .ok()?["result"]["value"]
                .clone();
            let answer = &at["answer"];
            if let (Some(x), Some(y)) = (answer["x"].as_f64(), answer["y"].as_f64()) {
                // The dialog took the press: answered, and the step is
                // pressed next time as if for the first.
                emit(
                    "dialog",
                    json!({ "said": at["blocked"], "button": answer["text"] }),
                );
                let _ = click_at(cdp, s, x, y);
                cdp.walk.as_mut()?.not_pressed(&step);
                return None;
            }
            if at["blocked"].is_string() {
                emit("dialog", json!({ "said": at["blocked"] }));
                return None;
            }
            let (Some(x), Some(y)) = (at["x"].as_f64(), at["y"].as_f64()) else {
                // The page changed under it.
                cdp.walk.as_mut()?.not_pressed(&step);
                return None;
            };
            click_at(cdp, s, x, y).ok()?;
            emit("press", json!({ "button": step.text, "leads": step.leads }));
            None
        }
    }
}

/// Clicks the middle of the first element `selector` finds, with the
/// mouse events a person's click makes (DevTools' input, which the page
/// cannot tell from a person's).
fn click(cdp: &mut Cdp, s: Option<&str>, selector: &str) -> Result<Value, String> {
    let find = format!(
        "(() => {{ const e = document.querySelector({}); if (!e) return null; const r = e.getBoundingClientRect(); return [r.x + r.width / 2, r.y + r.height / 2]; }})()",
        serde_json::to_string(selector).unwrap_or_default()
    );
    let at = cdp.call(
        "Runtime.evaluate",
        json!({ "expression": find, "returnByValue": true }),
        s,
    )?["result"]["value"]
        .clone();
    let (Some(x), Some(y)) = (at[0].as_f64(), at[1].as_f64()) else {
        return Err(format!("nothing on the page matches {selector}"));
    };
    click_at(cdp, s, x, y)?;
    Ok(json!({ "clicked": [x, y] }))
}

fn look(cdp: &mut Cdp, s: Option<&str>) -> Look {
    let page = cdp
        .call(
            "Runtime.evaluate",
            json!({ "expression": LOOK, "returnByValue": true }),
            s,
        )
        .ok()
        .and_then(|a| a["result"]["value"].as_str().map(str::to_string))
        .unwrap_or_else(|| "(the page did not answer)".into());
    let shot = cdp
        .call("Page.captureScreenshot", json!({ "format": "png" }), s)
        .ok()
        .and_then(|a| {
            use base64::Engine as _;
            base64::engine::general_purpose::STANDARD
                .decode(a["data"].as_str()?)
                .ok()
        });
    Look { page, shot }
}

#[cfg(test)]
mod download_tests {
    use super::*;

    /// A profile whose last browser was ended reads as closed cleanly
    /// afterwards, and its other preferences stay.
    #[test]
    fn a_killed_profile_is_marked_closed_cleanly() {
        let dir = std::env::temp_dir().join(format!("haul-prefs-{}", std::process::id()));
        let prefs = dir.join("Default");
        std::fs::create_dir_all(&prefs).unwrap();
        std::fs::write(
            prefs.join("Preferences"),
            r#"{"profile":{"exit_type":"Crashed","exited_cleanly":false,"name":"Example"},"other":1}"#,
        )
        .unwrap();
        super::mark_closed_cleanly(&dir);
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(prefs.join("Preferences")).unwrap())
                .unwrap();
        assert_eq!(v["profile"]["exit_type"], "Normal");
        assert_eq!(v["profile"]["exited_cleanly"], true);
        assert_eq!(v["profile"]["name"], "Example");
        assert_eq!(v["other"], 1);
        // No profile yet: nothing is made.
        let empty = dir.join("empty");
        super::mark_closed_cleanly(&empty);
        assert!(!empty.join("Default").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_the_browser_reports_is_read_from_its_event() {
        let event = json!({
            "method": "Browser.downloadWillBegin",
            "params": {
                "url": "https://files.example/abc123/archive.part1.rar",
                "suggestedFilename": "archive.part1.rar",
            },
        });
        assert_eq!(
            download_in(&event),
            Some(Download {
                url: "https://files.example/abc123/archive.part1.rar".into(),
                name: Some("archive.part1.rar".into()),
            })
        );
        let unnamed = json!({
            "method": "Browser.downloadWillBegin",
            "params": { "url": "https://files.example/abc123", "suggestedFilename": "" },
        });
        assert_eq!(download_in(&unnamed).unwrap().name, None);
    }

    #[test]
    fn only_a_file_on_the_web_counts() {
        let blob = json!({
            "method": "Browser.downloadWillBegin",
            "params": { "url": "blob:https://site.example/abc123" },
        });
        assert_eq!(download_in(&blob), None);
        let other = json!({ "method": "Page.loadEventFired", "params": {} });
        assert_eq!(download_in(&other), None);
    }
}

#[cfg(test)]
#[path = "walk_suite.rs"]
mod walk_suite;

#[cfg(test)]
mod drive_tests {
    use std::{
        io::{Read, Write},
        net::TcpListener,
        sync::atomic::AtomicUsize,
    };

    use super::*;

    /// What the stand-in browser does.
    #[derive(Clone, Copy, PartialEq)]
    enum Acts {
        /// Its page hands back a file.
        HandsFile,
        /// Its page hands back a file after two seconds.
        HandsFileLate,
        /// It stops answering after a few calls, as a browser that went.
        Goes,
        /// It stops answering while Haul sets up its page.
        GoesAtSetUp,
        /// Its page is closed.
        ClosesPage,
    }

    /// A stand-in for a browser's DevTools on a port of its own: `/json/version`
    /// and a WebSocket answering what a pass asks. Its calls are counted.
    fn devtools(acts: Acts) -> (u16, Arc<AtomicUsize>) {
        let born = Instant::now();
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = calls.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let calls = counted.clone();
                std::thread::spawn(move || serve(stream, port, acts, born, &calls));
            }
        });
        (port, calls)
    }

    fn serve(mut stream: TcpStream, port: u16, acts: Acts, born: Instant, calls: &AtomicUsize) {
        let mut head = [0u8; 2048];
        let upgrade = loop {
            let Ok(n) = stream.peek(&mut head) else {
                return;
            };
            let text = String::from_utf8_lossy(&head[..n]).to_ascii_lowercase();
            if text.contains("\r\n\r\n") {
                break text.contains("upgrade: websocket");
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        if !upgrade {
            let _ = stream.read(&mut head);
            let body = json!({
                "Browser": "Stand-in/1.0",
                "webSocketDebuggerUrl": format!("ws://127.0.0.1:{port}/devtools/browser/abc123"),
            })
            .to_string();
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );
            return;
        }
        let Ok(mut ws) = tungstenite::accept(stream) else {
            return;
        };
        loop {
            let Ok(Message::Text(text)) = ws.read() else {
                return;
            };
            let call: Value = serde_json::from_str(text.as_str()).unwrap();
            let n = calls.fetch_add(1, Ordering::SeqCst);
            if acts == Acts::Goes && n >= 12 || acts == Acts::GoesAtSetUp && n >= 2 {
                return;
            }
            let method = call["method"].as_str().unwrap_or_default();
            let result = match method {
                "Target.getTargets" if acts == Acts::ClosesPage && n >= 12 => {
                    json!({ "targetInfos": [] })
                }
                "Target.getTargets" => {
                    json!({ "targetInfos": [{ "type": "page", "targetId": "PAGE1" }] })
                }
                "Target.attachToTarget" => json!({ "sessionId": "SESSION1" }),
                "Network.getCookies" => json!({ "cookies": [] }),
                "SystemInfo.getProcessInfo" => {
                    json!({ "processInfo": [{ "type": "browser", "id": 4242, "cpuTime": 0 }] })
                }
                "Runtime.evaluate"
                    if (acts == Acts::HandsFile
                        || acts == Acts::HandsFileLate
                            && born.elapsed() > Duration::from_secs(2))
                        && call["params"]["expression"]
                            .as_str()
                            .is_some_and(|e| e.contains("__haulHandoff")) =>
                {
                    let said = json!({ "files": [{
                        "url": "https://files.example/abc123/archive.part1.rar",
                        "name": "archive.part1.rar",
                    }] });
                    json!({ "result": { "type": "string", "value": said.to_string() } })
                }
                "Runtime.evaluate" => json!({ "result": { "type": "object", "value": null } }),
                _ => json!({}),
            };
            let mut answer = json!({ "id": call["id"], "result": result });
            if let Some(session) = call.get("sessionId") {
                answer["sessionId"] = session.clone();
            }
            if ws.send(Message::text(answer.to_string())).is_err() {
                return;
            }
        }
    }

    /// A process that exits at once, as a starter that hands over to a
    /// browser it launched, or a browser that hands over to another: this
    /// test binary, which refuses the browser's arguments.
    fn exits_at_once() -> Child {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .arg("--remote-debugging-port=1")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        child.wait().unwrap();
        child
    }

    /// A process that runs a second, then exits, as a browser that hands
    /// over to another once DevTools is open: this test binary, running
    /// [`lives_a_second`].
    fn exits_in_a_second() -> Child {
        Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "chromium::drive_tests::lives_a_second",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
    }

    #[test]
    #[ignore = "run by exits_in_a_second"]
    fn lives_a_second() {
        std::thread::sleep(Duration::from_secs(1));
    }

    /// A process that exits a moment after its DevTools went, as a killed
    /// browser does: this test binary, running [`lives_briefly`].
    fn exits_shortly() -> Child {
        Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "chromium::drive_tests::lives_briefly",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
    }

    #[test]
    #[ignore = "run by exits_shortly"]
    fn lives_briefly() {
        std::thread::sleep(Duration::from_millis(300));
    }

    fn browser(child: Child) -> Browser {
        Browser {
            #[cfg(windows)]
            _job: None,
            hide: Arc::new(AtomicU32::new(child.id())),
            child,
        }
    }

    fn pass_of<'a>(exe: &'a Path, handle: Handle) -> Pass<'a> {
        Pass {
            exe,
            item: "abc123",
            url: "https://site.example/abc123",
            profile: std::env::temp_dir().join("haul-test-profile"),
            proxy: None,
            scripts: &[],
            show_after: Duration::from_secs(20),
            handle,
            take_links: None,
            ends_settled: false,
            press_download: false,
            keep_to_site: false,
            hand_clearance: false,
            walk: false,
        }
    }

    #[test]
    fn a_browser_whose_started_process_exited_is_driven_while_devtools_answers() {
        let (port, _) = devtools(Acts::HandsFile);
        let exe = std::env::current_exe().unwrap();
        let mut b = browser(exits_at_once());
        let outcome = drive(&mut b, port, &pass_of(&exe, Handle::default()));
        let Ok(Outcome::Files { files, .. }) = outcome else {
            panic!("expected the page's file, got {outcome:?}");
        };
        assert_eq!(
            files[0].url,
            "https://files.example/abc123/archive.part1.rar"
        );
    }

    #[test]
    fn a_browser_whose_started_process_exits_mid_pass_is_driven_while_devtools_answers() {
        let (port, _) = devtools(Acts::HandsFileLate);
        let exe = std::env::current_exe().unwrap();
        let mut b = browser(exits_in_a_second());
        let outcome = drive(&mut b, port, &pass_of(&exe, Handle::default()));
        let Ok(Outcome::Files { files, .. }) = outcome else {
            panic!("expected the page's file, got {outcome:?}");
        };
        assert_eq!(files[0].name.as_deref(), Some("archive.part1.rar"));
        assert!(
            b.child.try_wait().unwrap().is_some(),
            "the process exited meanwhile"
        );
        assert_eq!(
            b.hide.load(Ordering::Relaxed),
            4242,
            "the answering browser is hidden"
        );
    }

    #[test]
    fn devtools_that_stops_answering_ends_the_pass_with_its_cause() {
        let (port, _) = devtools(Acts::Goes);
        let exe = std::env::current_exe().unwrap();
        let mut b = browser(exits_at_once());
        let outcome = drive(&mut b, port, &pass_of(&exe, Handle::default()));
        let Ok(Outcome::Closed(Closed::NoDevTools(process))) = outcome else {
            panic!("expected DevTools gone, got {outcome:?}");
        };
        assert!(process.exited.is_some(), "{process:?}");
        assert_eq!(process.answers.as_deref(), Some("Stand-in/1.0"));
    }

    #[test]
    fn a_page_closed_in_the_browser_ends_the_pass_with_its_cause() {
        let (port, _) = devtools(Acts::ClosesPage);
        let exe = std::env::current_exe().unwrap();
        let mut b = browser(exits_in_a_second());
        let outcome = drive(&mut b, port, &pass_of(&exe, Handle::default()));
        let Ok(Outcome::Closed(Closed::PageGone(_))) = outcome else {
            panic!("expected the page gone, got {outcome:?}");
        };
    }

    #[test]
    fn a_pass_told_to_stop_says_so() {
        let (port, _) = devtools(Acts::HandsFileLate);
        let exe = std::env::current_exe().unwrap();
        let handle = Handle::default();
        handle.stop.store(true, Ordering::Relaxed);
        let mut b = browser(exits_at_once());
        let outcome = drive(&mut b, port, &pass_of(&exe, handle));
        assert!(
            matches!(outcome, Ok(Outcome::Closed(Closed::Stopped))),
            "{outcome:?}"
        );
    }

    #[test]
    fn a_browser_that_goes_while_its_page_is_set_up_closes_the_pass() {
        let (port, _) = devtools(Acts::GoesAtSetUp);
        let exe = std::env::current_exe().unwrap();
        let mut b = browser(exits_in_a_second());
        let outcome = drive(&mut b, port, &pass_of(&exe, Handle::default()));
        let Ok(Outcome::Closed(Closed::SetUp { error, process })) = outcome else {
            panic!("expected a close while setting up, got {outcome:?}");
        };
        assert!(error.starts_with("DevTools"), "{error}");
        assert_eq!(process.answers.as_deref(), Some("Stand-in/1.0"));
    }

    #[test]
    fn a_process_that_exits_with_nothing_on_its_port_fails_after_a_while() {
        let port = TcpListener::bind(("127.0.0.1", 0))
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let mut b = browser(exits_at_once());
        let start = Instant::now();
        let Err(why) = wait_for_endpoint(&mut b, port) else {
            panic!("nothing answers");
        };
        assert!(why.starts_with("the browser exited at start ("), "{why}");
        assert!(
            why.ends_with("and nothing opened DevTools on its port"),
            "{why}"
        );
        let took = start.elapsed();
        assert!(
            took >= EXITED_AT_START && took < EXITED_AT_START + Duration::from_secs(3),
            "{took:?}"
        );
    }

    #[test]
    fn a_browser_that_dies_just_after_devtools_goes_is_told_as_exited() {
        let (port, _) = devtools(Acts::GoesAtSetUp);
        let exe = std::env::current_exe().unwrap();
        let mut b = browser(exits_shortly());
        let outcome = drive(&mut b, port, &pass_of(&exe, Handle::default()));
        let Ok(Outcome::Closed(Closed::SetUp { process, .. })) = outcome else {
            panic!("expected a close while setting up, got {outcome:?}");
        };
        assert!(process.exited.is_some(), "{process:?}");
    }
}
