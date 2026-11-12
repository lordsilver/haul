//! The release suite: whole releases as a release page links them, read end
//! to end in stand-in mode, each in several arrival orders, and what Haul
//! made of them held against what the person should see (plan §1, §5
//! TEST-02).
//!
//! A case is a release in a small table language ([`Release`]): its sets of
//! files ([`Set`]) and its sources ([`Source`], one filecrypt container per
//! host, and a paste), each with how its table names and sizes its rows, the
//! rows that are odd ([`Odd`]), and what its host says of its links when
//! asked ([`Gone`]). The stand-in ([`Stand`]) serves all of it under the
//! sites' real names, the desktop app's part (passes, notifications) is
//! played by [`Desktop`], and each case's checks ([`Check`]) look at what
//! came of every order.
//!
//! A check that fails today names the task that fixes it (plan §5): the
//! table shows it as `xfail TASK`. When that task's layer makes it pass, the
//! suite fails with `XPASS TASK` until the mark is taken off, so each layer
//! turns its rows green. A failing check prints the whole table, as
//! `tests/suite.rs` does; `--nocapture` prints it always, with each run's
//! friction count (§4.7).
//!
//! Stand-in mode is one setting for the whole process, so this suite is a
//! test binary of its own (`cargo test -p haul-core --test releases`), and
//! its tests take turns ([`ONE_AT_A_TIME`]).

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU32, Ordering},
    },
    time::{Duration, Instant},
};

use axum::{
    Router,
    body::Bytes,
    extract::State,
    http::{HeaderMap, Method, StatusCode, Uri, header},
    response::{IntoResponse, Response},
};
use haul_core::{
    Browsing, Config, Engine, Event, Friction, Options, Snapshot, Told, WrapperAction,
    config::Endpoints, engine::List, hosts, parts::PartName,
};
use serde_json::json;

#[path = "../support/mod.rs"]
#[allow(dead_code)]
mod support;

/// The suite's tests take turns: stand-in mode and the log are the whole
/// process's.
static ONE_AT_A_TIME: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

const GIB: u64 = 1 << 30;

// ---------------------------------------------------------------------------
// The table language.

/// A host a source's links are on, with the form of its links.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Host {
    OneFichier,
    Ddownload,
    Gofile,
    Megaup,
    Pixeldrain,
    Rapidgator,
}

impl Host {
    fn name(self) -> &'static str {
        match self {
            Host::OneFichier => "1fichier.com",
            Host::Ddownload => "ddownload.com",
            Host::Gofile => "gofile.io",
            Host::Megaup => "megaup.net",
            Host::Pixeldrain => "pixeldrain.com",
            Host::Rapidgator => "rapidgator.net",
        }
    }

    /// The link to file `key` (a source's key and the part's number), named
    /// `file` where the host's links carry the name.
    fn link(self, key: &str, file: &str) -> String {
        match self {
            Host::OneFichier => format!("https://1fichier.com/?{key}"),
            Host::Ddownload => format!("https://ddownload.com/{key}"),
            Host::Gofile => format!("https://gofile.io/d/{key}"),
            Host::Megaup => format!("https://megaup.net/{key}/{file}"),
            Host::Pixeldrain => format!("https://pixeldrain.com/u/{key}"),
            Host::Rapidgator => format!("https://rapidgator.net/file/{key}/{file}.html"),
        }
    }

    /// Whether Haul asks it for a file's name and exact size (an API).
    fn tells_size(self) -> bool {
        matches!(self, Host::OneFichier | Host::Pixeldrain)
    }
}

/// One set of files: an archive in `parts` parts.
#[derive(Clone)]
struct Set {
    /// Its descriptive name: the parts are `<name>.partNN.rar`.
    name: &'static str,
    parts: u32,
    /// Part `n`'s exact size.
    size: fn(u32) -> u64,
}

impl Set {
    /// Part `n` named after `base`, numbered as wide as the last part.
    fn part_of(&self, base: &str, n: u32) -> String {
        let width = self.parts.to_string().len();
        format!("{base}.part{n:0width$}.rar")
    }

    /// The archive's name, as the archives view gives it.
    fn archive(&self) -> String {
        format!("{}.rar", self.name)
    }
}

/// How a source's table, and its host, name its files.
#[derive(Debug, Clone, Copy)]
enum Names {
    /// By the set's name.
    Set,
    /// By an ID in the set's name's place (`0123abcd.part01.rar`).
    Id(&'static str),
    /// By the set's name with a browser's copy mark (`… .part1 (2).rar`).
    Marked,
}

/// A row of a source's table that is not as the rest.
#[derive(Debug, Clone, Copy)]
enum Odd {
    /// Shows `n/a` for its name and size; its host names the file.
    Unnamed(u32),
    /// Shows this text where its name goes (an interstitial's title).
    Label(u32, &'static str),
    /// Not in this source at all.
    Missing(u32),
}

/// Which of a source's files its host says are deleted.
#[derive(Debug, Clone, Copy)]
enum Gone {
    None,
    All,
    Parts(&'static [u32]),
}

/// One source of a release: a filecrypt container of one set on one host.
#[derive(Clone)]
struct Source {
    /// Its container's ID (`AAAAAAAA`); its links' keys start with it.
    id: &'static str,
    host: Host,
    /// Its set, by its place among the release's.
    set: usize,
    names: Names,
    /// Whether its table shows sizes, rounded as filecrypt shows them.
    sizes: bool,
    odd: Vec<Odd>,
    gone: Gone,
    /// It shows a puzzle on every try.
    gated: bool,
    /// Its heading, when not the release's title.
    heading: Option<&'static str>,
    /// Its host turns away every n-th lookup (gofile's rate limit).
    limit_every: Option<u32>,
}

fn source(id: &'static str, host: Host, set: usize) -> Source {
    Source {
        id,
        host,
        set,
        names: Names::Set,
        sizes: true,
        odd: Vec::new(),
        gone: Gone::None,
        gated: false,
        heading: None,
        limit_every: None,
    }
}

impl Source {
    fn named(mut self, names: Names) -> Self {
        self.names = names;
        self
    }
    fn no_sizes(mut self) -> Self {
        self.sizes = false;
        self
    }
    fn odd(mut self, odd: Odd) -> Self {
        self.odd.push(odd);
        self
    }
    fn gone(mut self, gone: Gone) -> Self {
        self.gone = gone;
        self
    }
    fn headed(mut self, heading: &'static str) -> Self {
        self.heading = Some(heading);
        self
    }
    fn limited(mut self, every: u32) -> Self {
        self.limit_every = Some(every);
        self
    }

    /// The key of its file `n`, which its link carries.
    fn key(&self, n: u32) -> String {
        format!("{}{n:02}", self.id.to_ascii_lowercase())
    }

    fn missing(&self, n: u32) -> bool {
        self.odd
            .iter()
            .any(|o| matches!(o, Odd::Missing(m) if *m == n))
    }

    fn unnamed(&self, n: u32) -> bool {
        self.odd
            .iter()
            .any(|o| matches!(o, Odd::Unnamed(m) if *m == n))
    }

    fn is_gone(&self, n: u32) -> bool {
        match self.gone {
            Gone::None => false,
            Gone::All => true,
            Gone::Parts(parts) => parts.contains(&n),
        }
    }

    /// What its host calls file `n` of `set`.
    fn file(&self, set: &Set, n: u32) -> String {
        match self.names {
            Names::Set => set.part_of(set.name, n),
            Names::Id(id) => set.part_of(id, n),
            Names::Marked => set.part_of(set.name, n).replace(".rar", " (2).rar"),
        }
    }

    /// What its table shows for file `n`: the name, and the size.
    fn row(&self, set: &Set, n: u32) -> (String, String) {
        let size = if self.sizes {
            shown((set.size)(n))
        } else {
            "n/a".to_string()
        };
        for odd in &self.odd {
            match *odd {
                Odd::Unnamed(m) if m == n => return ("n/a".into(), "n/a".into()),
                Odd::Label(m, text) if m == n => return (text.into(), size),
                _ => {}
            }
        }
        (self.file(set, n), size)
    }
}

/// A size as filecrypt's table shows it, in 1024s.
fn shown(bytes: u64) -> String {
    for (unit, power) in [("GB", 3), ("MB", 2), ("KB", 1)] {
        let of = 1024u64.pow(power);
        if bytes >= of {
            return format!("{:.2} {unit}", bytes as f64 / of as f64);
        }
    }
    format!("{bytes} B")
}

/// In which order the sources arrive, and what happens on the way.
#[derive(Debug, Clone, Copy)]
enum Order {
    /// As the page lists them.
    Listed,
    Reversed,
    /// Shuffled by this seed.
    Shuffled(u64),
    /// As listed, with Haul restarted after this many of the browser's
    /// lookups were answered.
    RestartAfter(usize),
    /// As listed; then every row removed and the page added again.
    AddedAgain,
}

impl Order {
    /// The order the release page lists `n` sources in.
    fn listing(self, n: usize) -> Vec<usize> {
        let mut at: Vec<usize> = (0..n).collect();
        match self {
            Order::Reversed => at.reverse(),
            Order::Shuffled(seed) => {
                let mut x = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
                for i in (1..n).rev() {
                    x = x
                        .wrapping_mul(6_364_136_223_846_793_005)
                        .wrapping_add(1_442_695_040_888_963_407);
                    at.swap(i, (x >> 33) as usize % (i + 1));
                }
            }
            _ => {}
        }
        at
    }

    fn label(self) -> String {
        match self {
            Order::Listed => "listed".into(),
            Order::Reversed => "reversed".into(),
            Order::Shuffled(seed) => format!("shuffled {seed}"),
            Order::RestartAfter(n) => format!("restart after {n} lookups"),
            Order::AddedAgain => "removed and added again".into(),
        }
    }
}

/// What a case's run must come to.
#[derive(Debug, Clone)]
enum Check {
    /// One card, named as the release, which never counts its wrappers:
    /// never two at once, and one key from the page's adding to the end.
    OneCard,
    /// Each set the table describes with live links is one archive under
    /// its name: every part once, each with a link from every source that
    /// holds it alive (a link only its host could name may wait unplaced),
    /// and no other row.
    Sets,
    /// No listed row is without a file's name, in any snapshot.
    NeverUnnamed,
    /// No row is listed and then gone again.
    NoRowVanishes,
    /// Every part of a set some source's host sizes has its exact size,
    /// and its archive the total.
    ExactSizes,
    /// Nothing is under Failed.
    NothingFailed,
    /// The card has a note holding this text.
    Note(&'static str),
    /// No notification says a download failed.
    NoFailureTold,
    /// This many notifications about the release.
    Notifications(u32),
    /// This many times a person was asked (the friction count).
    Asks(usize),
    /// filecrypt gets one request per container and added page.
    OneReadPerContainer,
    /// The status bar never adds pages and links up as "Unwrapping N".
    SeparateCounts,
    /// Every order comes to the same sets.
    SameEveryOrder,
}

impl Check {
    fn says(&self) -> String {
        match self {
            Check::OneCard => "one card, named as the release".into(),
            Check::Sets => "one archive per set, every link on its part".into(),
            Check::NeverUnnamed => "no unnamed row at any moment".into(),
            Check::NoRowVanishes => "no row appears and then disappears".into(),
            Check::ExactSizes => "every part's exact size".into(),
            Check::NothingFailed => "nothing under Failed".into(),
            Check::Note(text) => format!("a note \"{text}\""),
            Check::NoFailureTold => "no notification that a download failed".into(),
            Check::Notifications(n) => format!("{n} notification(s)"),
            Check::Asks(n) => format!("{n} ask(s)"),
            Check::OneReadPerContainer => "one request per container".into(),
            Check::SeparateCounts => "never \"Unwrapping N links\"".into(),
            Check::SameEveryOrder => "the same in every order".into(),
        }
    }
}

/// A check, and the task that makes it pass when it fails today.
struct Want {
    check: Check,
    until: Option<&'static str>,
}

/// One case: a release, its checks and its orders.
struct Release {
    case: &'static str,
    /// The release page's title and address (`releases.example/<slug>`).
    title: &'static str,
    slug: &'static str,
    sets: Vec<Set>,
    sources: Vec<Source>,
    /// A paste the page links (paste2.org), with its text.
    paste: Option<&'static str>,
    wants: Vec<Want>,
    orders: Vec<Order>,
}

impl Release {
    fn new(case: &'static str, title: &'static str, slug: &'static str) -> Self {
        Self {
            case,
            title,
            slug,
            sets: Vec::new(),
            sources: Vec::new(),
            paste: None,
            wants: Vec::new(),
            orders: vec![Order::Listed, Order::Reversed],
        }
    }
    fn set(mut self, name: &'static str, parts: u32, size: fn(u32) -> u64) -> Self {
        self.sets.push(Set { name, parts, size });
        self
    }
    fn source(mut self, s: Source) -> Self {
        self.sources.push(s);
        self
    }
    fn paste(mut self, text: &'static str) -> Self {
        self.paste = Some(text);
        self
    }
    /// Must pass now.
    fn holds(mut self, check: Check) -> Self {
        self.wants.push(Want { check, until: None });
        self
    }
    /// Fails today; `task` makes it pass.
    fn fails_until(mut self, check: Check, task: &'static str) -> Self {
        self.wants.push(Want {
            check,
            until: Some(task),
        });
        self
    }
    fn orders(mut self, orders: &[Order]) -> Self {
        self.orders = orders.to_vec();
        self
    }

    fn url(&self) -> String {
        format!("https://releases.example/{}", self.slug)
    }
}

// ---------------------------------------------------------------------------
// The cases: one row per break in the history, and the acceptance case.

fn two_gib(_: u32) -> u64 {
    2 * GIB
}
fn one_gib(_: u32) -> u64 {
    GIB
}
fn four_gib(_: u32) -> u64 {
    4 * GIB
}
/// 2 GiB, the last of 67 parts 1 GiB (the acceptance case).
fn acceptance_size(n: u32) -> u64 {
    if n < 67 { 2 * GIB } else { GIB }
}
/// Sizes no table shows exactly: rounded, they read the same.
fn odd_size(n: u32) -> u64 {
    if n < 4 {
        1_950_000_000 + u64::from(n) * 1_000
    } else {
        700_000_000
    }
}

/// The acceptance case's sources (plan §1), as `standin.py` serves them.
fn acceptance(case: &'static str) -> Release {
    const UNNAMED: [u32; 10] = [5, 12, 19, 26, 33, 40, 47, 54, 61, 67];
    const DEAD: &[u32] = &[5, 19, 26, 33, 40, 54, 61, 67];
    let mut gofile = source("CCCCCCCC", Host::Gofile, 0)
        .gone(Gone::Parts(DEAD))
        .limited(5);
    for n in UNNAMED {
        gofile = gofile.odd(Odd::Unnamed(n));
    }
    Release::new(case, "Example.Set", "example-set")
        .set("Example.Set", 67, acceptance_size)
        .set("Example.Set", 34, four_gib)
        .paste("Example.Set: a made-up release in 67 parts. Unpack part 01 with any archiver.")
        .source(
            source("AAAAAAAA", Host::OneFichier, 1)
                .named(Names::Id("0123abcd"))
                .gone(Gone::All),
        )
        .source(source("BBBBBBBB", Host::Ddownload, 0).named(Names::Id("4567cdef")))
        .source(gofile)
        .source(source("DDDDDDDD", Host::Megaup, 0).odd(Odd::Label(23, "Just a moment...")))
        .source(source("EEEEEEEE", Host::Pixeldrain, 0))
        .source(source("FFFFFFFF", Host::Rapidgator, 0).named(Names::Id("89efab01")))
}

fn cases() -> Vec<Release> {
    use Check::*;
    use Host::*;
    vec![
        Release::new("copy marks", "Example.Set", "copy-marks")
            .set("Example.Set", 4, two_gib)
            .source(source("AAAAAAAA", Pixeldrain, 0).named(Names::Marked))
            .source(source("BBBBBBBB", Rapidgator, 0).no_sizes())
            .holds(OneCard)
            .holds(Sets)
            .holds(NeverUnnamed)
            .holds(NoRowVanishes)
            .fails_until(ExactSizes, "LOOK-02")
            .holds(NothingFailed)
            .holds(SameEveryOrder),
        Release::new("ID names", "Example.Set", "id-names")
            .set("Example.Set", 4, two_gib)
            .source(source("AAAAAAAA", Ddownload, 0).named(Names::Id("4567cdef")))
            .source(source("BBBBBBBB", Pixeldrain, 0))
            .source(source("CCCCCCCC", Rapidgator, 0).named(Names::Id("89efab01")))
            .orders(&[Order::Listed, Order::Reversed, Order::Shuffled(7)])
            .holds(OneCard)
            .holds(Sets)
            .holds(NeverUnnamed)
            .holds(NoRowVanishes)
            .fails_until(ExactSizes, "LOOK-02")
            .holds(NothingFailed)
            .holds(SameEveryOrder),
        Release::new("rounded sizes", "Example.Set", "rounded-sizes")
            .set("Example.Set", 4, odd_size)
            .source(source("AAAAAAAA", Ddownload, 0).named(Names::Id("4567cdef")))
            .source(source("BBBBBBBB", Rapidgator, 0).named(Names::Id("89efab01")))
            .source(source("CCCCCCCC", Pixeldrain, 0))
            .orders(&[Order::Listed, Order::Reversed, Order::Shuffled(3)])
            .holds(OneCard)
            .holds(Sets)
            .holds(NeverUnnamed)
            .fails_until(ExactSizes, "LOOK-02")
            .holds(SameEveryOrder),
        Release::new(
            "a generic title shared by unrelated sets",
            "Folder",
            "folder",
        )
        .set("Example.One", 2, two_gib)
        .set("Example.Two", 3, one_gib)
        .source(source("AAAAAAAA", Pixeldrain, 0).headed("Folder"))
        .source(source("BBBBBBBB", Rapidgator, 1).headed("Folder"))
        .holds(OneCard)
        .holds(Sets)
        .holds(NeverUnnamed)
        .holds(SameEveryOrder),
        Release::new("n/a rows", "Example.Set", "unnamed-rows")
            .set("Example.Set", 4, two_gib)
            .source(
                source("AAAAAAAA", Gofile, 0)
                    .odd(Odd::Unnamed(2))
                    .odd(Odd::Unnamed(4)),
            )
            .source(source("BBBBBBBB", Pixeldrain, 0))
            .holds(OneCard)
            .holds(Sets)
            .fails_until(NeverUnnamed, "MODEL-09")
            .fails_until(NoRowVanishes, "MODEL-09")
            .fails_until(ExactSizes, "LOOK-02")
            .holds(SameEveryOrder),
        Release::new(
            "an interstitial's title as a name",
            "Example.Set",
            "interstitial",
        )
        .set("Example.Set", 4, two_gib)
        .source(source("AAAAAAAA", Megaup, 0).odd(Odd::Label(3, "Just a moment...")))
        .source(source("BBBBBBBB", Pixeldrain, 0))
        .holds(OneCard)
        .holds(Sets)
        .holds(NeverUnnamed)
        .fails_until(ExactSizes, "LOOK-02")
        .holds(SameEveryOrder),
        Release::new("two splits, one dead", "Example.Set", "two-splits")
            .set("Example.Set", 4, two_gib)
            .set("Example.Set", 2, four_gib)
            .source(source("AAAAAAAA", Pixeldrain, 0))
            .source(source("BBBBBBBB", Rapidgator, 0))
            .source(
                source("CCCCCCCC", OneFichier, 1)
                    .named(Names::Id("0123abcd"))
                    .gone(Gone::All),
            )
            .holds(OneCard)
            .holds(Sets)
            .fails_until(NothingFailed, "LOOK-06")
            .fails_until(
                Note("another copy, 2 parts on 1fichier, is gone"),
                "CARD-05",
            )
            .holds(NoFailureTold)
            .holds(SameEveryOrder),
        Release::new(
            "a part missing from some containers",
            "Example.Set",
            "missing-part",
        )
        .set("Example.Set", 4, two_gib)
        .source(source("AAAAAAAA", Pixeldrain, 0).odd(Odd::Missing(3)))
        .source(source("BBBBBBBB", Rapidgator, 0))
        .source(source("CCCCCCCC", Ddownload, 0).odd(Odd::Missing(2)))
        .holds(OneCard)
        .holds(Sets)
        .holds(NeverUnnamed)
        .holds(SameEveryOrder),
        Release::new(
            "two unrelated sets with the same title and count",
            "Example.Set",
            "same-title",
        )
        .set("Example.Set", 4, two_gib)
        .set("0123abcd", 4, one_gib)
        .source(source("AAAAAAAA", Pixeldrain, 0))
        .source(source("BBBBBBBB", OneFichier, 1))
        .holds(OneCard)
        .holds(Sets)
        .holds(ExactSizes)
        .holds(SameEveryOrder),
        acceptance("the acceptance case")
            .orders(&[
                Order::Listed,
                Order::Reversed,
                Order::Shuffled(11),
                Order::RestartAfter(4),
                Order::AddedAgain,
            ])
            .holds(OneCard)
            .fails_until(Sets, "MODEL-09")
            .fails_until(NeverUnnamed, "MODEL-09")
            .fails_until(NoRowVanishes, "MODEL-09")
            .fails_until(ExactSizes, "LOOK-02")
            .fails_until(NothingFailed, "LOOK-06")
            .fails_until(
                Note("another copy, 34 parts on 1fichier, is gone"),
                "CARD-05",
            )
            .holds(NoFailureTold)
            .fails_until(Notifications(1), "QUIET-04")
            .holds(SeparateCounts)
            .holds(OneReadPerContainer)
            .holds(Asks(0))
            .holds(SameEveryOrder),
        acceptance("the acceptance case, two containers behind a puzzle")
            .orders(&[Order::Listed])
            .gate("BBBBBBBB")
            .gate("FFFFFFFF")
            .fails_until(Sets, "MODEL-09")
            .fails_until(Asks(0), "FLOW-02"),
    ]
}

impl Release {
    /// Its source `id` shows a puzzle on every try.
    fn gate(mut self, id: &str) -> Self {
        for s in &mut self.sources {
            if s.id == id {
                s.gated = true;
            }
        }
        self
    }
}

// ---------------------------------------------------------------------------
// The stand-in: every site of a release, by the name it is asked under.

struct Stand {
    release: Arc<Release>,
    /// The order the release page lists its sources in.
    listing: Vec<usize>,
    /// Each link's key: its source and part.
    keys: HashMap<String, (usize, u32)>,
    /// Every request, as `site path`, in order.
    log: Mutex<Vec<String>>,
    lookups: AtomicU32,
}

impl Stand {
    async fn start(release: Arc<Release>, listing: Vec<usize>) -> (Arc<Self>, u16) {
        let mut keys = HashMap::new();
        for (at, s) in release.sources.iter().enumerate() {
            for n in 1..=release.sets[s.set].parts {
                keys.insert(s.key(n), (at, n));
            }
        }
        let stand = Arc::new(Self {
            release,
            listing,
            keys,
            log: Mutex::default(),
            lookups: AtomicU32::new(0),
        });
        let app = Router::new().fallback(answer).with_state(stand.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (stand, port)
    }

    fn requests(&self, line: &str) -> usize {
        self.log
            .lock()
            .unwrap()
            .iter()
            .filter(|l| *l == line)
            .count()
    }

    /// The containers in the order they were asked for.
    fn arrived(&self) -> Vec<String> {
        let mut seen = Vec::new();
        for line in self.log.lock().unwrap().iter() {
            if let Some(id) = line
                .strip_prefix("filecrypt.cc /Container/")
                .and_then(|p| p.strip_suffix(".html"))
                && !seen.iter().any(|s| s == id)
            {
                seen.push(id.to_string());
            }
        }
        seen
    }

    fn file(&self, key: &str) -> Option<(&Source, &Set, u32)> {
        let (at, n) = *self.keys.get(key)?;
        let s = &self.release.sources[at];
        Some((s, &self.release.sets[s.set], n))
    }

    fn page(&self) -> String {
        let r = &self.release;
        let paste = r.paste.map_or(String::new(), |_| {
            format!(
                "<p>Notes: <a href=\"https://paste2.org/paste/{0}\">https://paste2.org/paste/{0}</a></p>",
                r.slug
            )
        });
        let links: String = self
            .listing
            .iter()
            .map(|&at| {
                let s = &r.sources[at];
                format!(
                    "<li><a href=\"https://filecrypt.cc/Container/{}.html\">{}</a></li>",
                    s.id,
                    s.host.name()
                )
            })
            .collect();
        format!(
            "<html><head><title>{0}</title></head><body><h1>{0}</h1><p>A made-up release for Haul's tests.</p>{paste}<ul>{links}</ul><p><a href=\"/about\">About</a></p></body></html>",
            r.title
        )
    }

    fn container(&self, s: &Source) -> String {
        if s.gated {
            return "<html><head><title>Security Check</title></head><body><h2>Security Check</h2><div class=\"cutcaptcha\" data-sitekey=\"cc_abc123\"></div></body></html>".into();
        }
        let set = &self.release.sets[s.set];
        let mut links = Vec::new();
        let mut rows = String::new();
        for n in (1..=set.parts).filter(|n| !s.missing(*n)) {
            let link = s.host.link(&s.key(n), &s.file(set, n));
            let (label, size) = s.row(set, n);
            let mark = if s.is_gone(n) { "offline" } else { "online" };
            let host = s.host.name();
            rows.push_str(&format!(
                r#"<tr class="kwj3"><td class="status"><i class="{mark} small"></i></td><td title="{label}">{label}<span><a href="https://{host}" class="external_link">{host}</a></span></td><td>{size}</td><td><a href="/Link/{n:08}.html" class="button download"></a></td></tr>"#
            ));
            links.push(link);
        }
        let heading = s.heading.unwrap_or(self.release.title);
        format!(
            "{}<table>{rows}</table>",
            support::fc_solved_with(heading, &links)
        )
    }

    /// gofile's content API for folder `key`, as the desktop app's page
    /// sees it: its status and answer.
    fn gofile(&self, key: &str) -> (u16, serde_json::Value) {
        let calls = self.lookups.fetch_add(1, Ordering::SeqCst) + 1;
        let Some((s, set, n)) = self.file(key) else {
            return (404, json!({"status": "error-notFound", "data": {}}));
        };
        if s.limit_every
            .is_some_and(|every| calls.is_multiple_of(every))
        {
            return (429, json!({"status": "error-rateLimit", "data": {}}));
        }
        if s.is_gone(n) {
            return (404, json!({"status": "error-notFound", "data": {}}));
        }
        let name = s.file(set, n);
        let child = json!({"id": format!("file{n:02}"), "type": "file", "name": name, "size": (set.size)(n),
            "link": format!("https://store1.gofile.io/download/web/file{n:02}/{name}")});
        (
            200,
            json!({"status": "ok", "data": {"type": "folder", "children": {format!("file{n:02}"): child}}}),
        )
    }
}

async fn answer(
    State(stand): State<Arc<Stand>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let host = headers
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .unwrap_or_default();
    let name = host.split(':').next().unwrap_or_default();
    let site = name
        .strip_suffix(".localhost")
        .unwrap_or(name)
        .trim_start_matches("www.");
    let path = uri.path();
    stand.log.lock().unwrap().push(format!("{site} {path}"));
    let html =
        |body: String| ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], body).into_response();
    let r = &stand.release;
    match (site, method.as_str()) {
        ("releases.example", "GET") if path == format!("/{}", r.slug) => html(stand.page()),
        ("paste2.org", "GET") if path == format!("/paste/{}", r.slug) && r.paste.is_some() => {
            html(format!(
                "<html><head><title>Paste</title></head><body><nav>Home New paste</nav><pre>{}</pre></body></html>",
                r.paste.unwrap_or_default()
            ))
        }
        ("filecrypt.cc", "GET") => {
            let id = path
                .strip_prefix("/Container/")
                .and_then(|p| p.strip_suffix(".html"));
            match r.sources.iter().find(|s| Some(s.id) == id) {
                Some(s) => html(stand.container(s)),
                None => StatusCode::NOT_FOUND.into_response(),
            }
        }
        ("pixeldrain.com", "GET") => {
            let key = path
                .strip_prefix("/api/file/")
                .and_then(|p| p.strip_suffix("/info"))
                .unwrap_or_default();
            match stand.file(key) {
                Some((s, set, n)) if !s.is_gone(n) => axum::Json(json!({
                    "id": key, "name": s.file(set, n), "size": (set.size)(n),
                }))
                .into_response(),
                _ => (
                    StatusCode::NOT_FOUND,
                    axum::Json(json!({"success": false, "value": "not_found"})),
                )
                    .into_response(),
            }
        }
        ("1fichier.com", "POST") if path == "/check_links.pl" => {
            let mut out = String::new();
            for (k, link) in url::form_urlencoded::parse(&body) {
                if k != "links[]" {
                    continue;
                }
                let key = link.rsplit('?').next().unwrap_or_default();
                match stand.file(key) {
                    Some((s, set, n)) if !s.is_gone(n) => {
                        out.push_str(&format!("{link};{};{}\n", s.file(set, n), (set.size)(n)));
                    }
                    _ => out.push_str(&format!("{link};;;NOT FOUND\n")),
                }
            }
            out.into_response()
        }
        ("ddownload.com" | "megaup.net" | "rapidgator.net", "GET") => {
            let key = path.trim_start_matches("/file/").trim_start_matches('/');
            let key = key.split('/').next().unwrap_or_default();
            match stand.file(key) {
                Some((s, set, n)) if !s.is_gone(n) => {
                    let name = s.file(set, n);
                    html(format!(
                        r#"<html><head><title>{name}</title></head><body><h1>{name}</h1><span class="dk-dl-name" title="{name}"></span></body></html>"#
                    ))
                }
                _ => StatusCode::NOT_FOUND.into_response(),
            }
        }
        _ => (
            StatusCode::NOT_FOUND,
            format!("no stand-in for {site}{path}"),
        )
            .into_response(),
    }
}

// ---------------------------------------------------------------------------
// The desktop app's part, as it does it at this layer.

#[derive(Default)]
struct Desktop {
    /// Hosts that asked a person already (`first_ask_in_a_while`).
    asked: HashSet<String>,
    /// Lookups answered.
    lookups: usize,
    /// What it told, by kind, and the rows it was about.
    told: Vec<(Told, Vec<String>)>,
    /// Passes for pages the case did not expect.
    unexpected: Vec<String>,
}

impl Desktop {
    /// Passes row `id`'s `page`, as the desktop app's browser would, and
    /// tells the engine what the page did.
    fn pass(&mut self, engine: &Engine, stand: &Stand, id: &str, page: &str) {
        let host = hosts::host_of(page);
        if host == "gofile.io" {
            self.lookups += 1;
            let key = page.rsplit('/').next().unwrap_or_default();
            let (_, said) = stand.gofile(key);
            // As handoff.js's gofile recipe reads the API: files, gone, or
            // anything else dropped, which leaves the pass to end wanting a
            // person. LOOK-03 hands the status to Rust instead.
            match said["status"].as_str() {
                Some("ok") => {
                    let files = said["data"]["children"]
                        .as_object()
                        .into_iter()
                        .flat_map(|c| c.values())
                        .map(|f| hosts::Entry {
                            url: f["link"].as_str().unwrap_or_default().into(),
                            name: f["name"].as_str().map(str::to_string),
                            size: f["size"].as_u64(),
                            ..hosts::Entry::default()
                        })
                        .collect();
                    let _ = engine.set_files(id, files);
                }
                Some("error-notFound") => engine.page_gone(id, "gofile: the folder was deleted"),
                _ => self.wants_person(engine, id, &host),
            }
            return;
        }
        let gated = page.contains("/Container/")
            && stand
                .release
                .sources
                .iter()
                .any(|s| s.gated && page.contains(s.id));
        if gated {
            engine.page_shows_gate(id, page, hosts::gates::Gate::Cutcaptcha);
        } else {
            self.unexpected.push(page.to_string());
        }
        self.wants_person(engine, id, &host);
    }

    fn wants_person(&mut self, engine: &Engine, id: &str, host: &str) {
        if engine.page_wants_person(id) && self.asked.insert(host.to_string()) {
            self.notify(engine, Told::NeedsYou, vec![id.to_string()]);
        }
    }

    fn notify(&mut self, engine: &Engine, kind: Told, rows: Vec<String>) {
        engine.notified(&rows);
        self.told.push((kind, rows));
    }

    /// The engine's events, gathered as the desktop app gathers them (two
    /// seconds from a burst's first), told with the window out of focus.
    fn tell(&mut self, engine: &Engine, events: &[(Instant, Event)]) {
        let mut at = 0;
        while at < events.len() {
            let until = events[at].0 + Duration::from_secs(2);
            let end = events[at..]
                .iter()
                .position(|(t, _)| *t >= until)
                .map_or(events.len(), |p| at + p);
            let burst: Vec<Event> = events[at..end].iter().map(|(_, e)| e.clone()).collect();
            for t in engine.tellings(&burst) {
                self.notify(engine, t.kind, t.rows);
            }
            at = end;
        }
    }
}

// ---------------------------------------------------------------------------
// A run: one order of one case.

/// What the snapshots showed on the way.
#[derive(Default)]
struct Seen {
    unnamed: Option<String>,
    /// The status bar lumped different work into one count somewhere.
    lumped: bool,
    /// The most links it said it was checking at once.
    checking: usize,
    listed: HashMap<String, String>,
    /// The most cards at once, and every card's key and name.
    most_cards: usize,
    cards: BTreeSet<(String, String)>,
}

impl Seen {
    fn take(&mut self, s: &Snapshot) {
        for i in s.items.iter().filter(|i| i.list.is_some() && !i.tentative) {
            self.listed
                .entry(i.id.clone())
                .or_insert_with(|| i.name.clone());
            if self.unnamed.is_none() && !is_file_name(&i.name) {
                self.unnamed = Some(i.name.clone());
            }
        }
        let counts = serde_json::to_value(&s.counts).unwrap();
        self.lumped |= counts.get("unwrapping").is_some();
        self.checking = self.checking.max(s.counts.checking);
        self.most_cards = self.most_cards.max(s.wrapper_cards.len());
        for c in &s.wrapper_cards {
            self.cards.insert((c.key.clone(), c.name.clone()));
        }
    }
}

fn is_file_name(name: &str) -> bool {
    !name.contains("://")
        && name
            .rsplit_once('.')
            .is_some_and(|(_, ext)| (2..=5).contains(&ext.len()))
}

struct Run {
    order: Order,
    arrived: Vec<String>,
    /// The last snapshot; none when the run never settled.
    end: Option<Snapshot>,
    seen: Seen,
    friction: Vec<Friction>,
    told: Vec<(Told, Vec<String>)>,
    lagged: u64,
    /// Requests for each container, and how many times the page was added.
    reads: Vec<(String, usize)>,
    adds: usize,
    unexpected: Vec<String>,
    error: Option<String>,
}

impl Run {
    fn friction_line(&self) -> String {
        let asks: usize = self.friction.iter().map(|f| f.asks.len()).sum();
        let needed: usize = self
            .friction
            .iter()
            .map(|f| f.asks.iter().filter(|a| a.needed).count())
            .sum();
        let notified: u32 = self.friction.iter().map(|f| f.notifications).sum();
        let clicks: u32 = self.friction.iter().map(|f| f.clicks).sum();
        let failures = self.told.iter().filter(|(k, _)| *k == Told::Failed).count();
        let sites: BTreeSet<String> = self
            .friction
            .iter()
            .flat_map(|f| &f.asks)
            .map(|a| {
                format!(
                    "{} ({}{})",
                    a.site,
                    a.need,
                    a.gate.map_or(String::new(), |g| format!(", {g}"))
                )
            })
            .collect();
        format!(
            "{asks} asks ({needed} needed{}), {notified} notifications ({failures} of a failure), {clicks} clicks{}",
            if sites.is_empty() {
                String::new()
            } else {
                format!(": {}", sites.into_iter().collect::<Vec<_>>().join(", "))
            },
            if self.lagged > 0 {
                format!(", {} events lost to a full channel", self.lagged)
            } else {
                String::new()
            }
        )
    }
}

fn options(root: &std::path::Path) -> Options {
    Options {
        data_dir: root.join("data"),
        download_dir: None,
        config: Some(Config {
            download_dir: Some(root.join("dl")),
            // In stand-in mode every real address goes to the stand-in.
            endpoints: Endpoints::default(),
            // Every host on, whatever the defaults turn off.
            disabled_hosts: Vec::new(),
            ..Config::default()
        }),
        recycle_bin: Some(root.join("bin")),
    }
}

/// One engine's life in a run: what it showed and told.
struct Phase {
    seen: Arc<Mutex<Seen>>,
    events: Arc<Mutex<Vec<(Instant, Event)>>>,
    lagged: Arc<AtomicU32>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl Phase {
    fn watch(engine: &Engine, seen: Arc<Mutex<Seen>>) -> Self {
        let events: Arc<Mutex<Vec<(Instant, Event)>>> = Arc::default();
        let lagged = Arc::new(AtomicU32::new(0));
        let sampler = {
            let (engine, seen) = (engine.clone(), seen.clone());
            tokio::spawn(async move {
                let mut changes = engine.watch();
                loop {
                    let s = engine.snapshot();
                    seen.lock().unwrap().take(&s);
                    if changes.changed().await.is_err() {
                        return;
                    }
                }
            })
        };
        let listener = {
            let (mut rx, events, lagged) = (engine.subscribe(), events.clone(), lagged.clone());
            tokio::spawn(async move {
                use tokio::sync::broadcast::error::RecvError;
                loop {
                    match rx.recv().await {
                        Ok(e) => events.lock().unwrap().push((Instant::now(), e)),
                        Err(RecvError::Lagged(n)) => {
                            lagged.fetch_add(n as u32, Ordering::SeqCst);
                        }
                        Err(RecvError::Closed) => return,
                    }
                }
            })
        };
        Self {
            seen,
            events,
            lagged,
            tasks: vec![sampler, listener],
        }
    }

    /// Ends watching: tells what the events come to, and takes the
    /// engine's friction count.
    async fn end(self, engine: &Engine, desktop: &mut Desktop, run: &mut Acc) {
        tokio::time::sleep(Duration::from_millis(50)).await;
        self.seen.lock().unwrap().take(&engine.snapshot());
        for t in self.tasks {
            t.abort();
        }
        let events = std::mem::take(&mut *self.events.lock().unwrap());
        desktop.tell(engine, &events);
        run.friction.extend(engine.friction());
        run.lagged += u64::from(self.lagged.load(Ordering::SeqCst));
    }
}

#[derive(Default)]
struct Acc {
    friction: Vec<Friction>,
    lagged: u64,
}

/// Drives `engine` as the desktop app would until nothing changes for a
/// while (or `stop` says so), at most a minute.
async fn drive(
    engine: &Engine,
    stand: &Stand,
    desktop: &mut Desktop,
    stop: impl Fn(&Desktop) -> bool,
) -> Result<(), String> {
    let idle = Browsing {
        out_of_sight: &[],
        with_person: &|_| false,
        has_page: &|_| false,
    };
    let deadline = Instant::now() + Duration::from_secs(60);
    let changes = engine.watch();
    let mut version = *changes.borrow();
    let mut quiet = Instant::now();
    loop {
        if stop(desktop) {
            return Ok(());
        }
        if let Some((id, page)) = engine.plan_passes(&idle).start {
            desktop.pass(engine, stand, &id, &page);
            quiet = Instant::now();
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
        let now = *changes.borrow();
        if now != version {
            version = now;
            quiet = Instant::now();
        }
        let reading = engine
            .snapshot()
            .wrappers
            .iter()
            .any(|w| w.status.as_str() == "unwrapping");
        if !reading && quiet.elapsed() > Duration::from_millis(600) {
            return Ok(());
        }
        if Instant::now() > deadline {
            return Err("never settled".into());
        }
    }
}

/// An engine on the data folder under `root` once the one before has let
/// go of it.
async fn start_when_free(root: &std::path::Path) -> Result<Engine, String> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match Engine::start(options(root)).await {
            Ok(engine) => return Ok(engine),
            Err(haul_core::StartError::Locked(_)) if Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Err(e) => return Err(e.to_string()),
        }
    }
}

async fn run(release: &Arc<Release>, order: Order) -> Run {
    let (stand, port) = Stand::start(release.clone(), order.listing(release.sources.len())).await;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    hosts::set_stand_in(Some(port));
    let mut desktop = Desktop::default();
    let mut acc = Acc::default();
    let seen = Arc::new(Mutex::new(Seen::default()));
    let mut adds = 1;
    let result = async {
        let mut engine = Engine::start(options(&root))
            .await
            .map_err(|e| e.to_string())?;
        engine.set_pages_pass(true);
        let mut phase = Phase::watch(&engine, seen.clone());
        engine.add_text(&release.url(), "");
        match order {
            Order::RestartAfter(after) => {
                drive(&engine, &stand, &mut desktop, |d| d.lookups >= after).await?;
                phase.end(&engine, &mut desktop, &mut acc).await;
                engine.shutdown().await;
                drop(engine);
                engine = start_when_free(&root).await?;
                engine.set_pages_pass(true);
                phase = Phase::watch(&engine, seen.clone());
            }
            Order::AddedAgain => {
                drive(&engine, &stand, &mut desktop, |_| false).await?;
                // Told while its rows are there, as the desktop app does.
                phase.end(&engine, &mut desktop, &mut acc).await;
                phase = Phase::watch(&engine, seen.clone());
                let s = engine.snapshot();
                let wrappers: Vec<String> = s.wrappers.iter().map(|w| w.id.clone()).collect();
                engine.wrappers_action(&wrappers, WrapperAction::Remove);
                let items: Vec<String> = engine
                    .snapshot()
                    .items
                    .iter()
                    .map(|i| i.id.clone())
                    .collect();
                engine.items_action(&items, haul_core::ItemAction::Remove);
                tokio::time::sleep(Duration::from_millis(100)).await;
                // What was taken off by hand did not vanish by itself, and
                // its card was the person's to remove.
                {
                    let mut seen = seen.lock().unwrap();
                    seen.listed.clear();
                    seen.cards.clear();
                }
                engine.add_text(&release.url(), "");
                adds = 2;
            }
            _ => {}
        }
        let settled = drive(&engine, &stand, &mut desktop, |_| false).await;
        phase.end(&engine, &mut desktop, &mut acc).await;
        let end = engine.snapshot();
        engine.shutdown().await;
        settled.map(|()| end)
    }
    .await;
    hosts::set_stand_in(None);
    let reads = release
        .sources
        .iter()
        .map(|s| {
            (
                s.id.to_string(),
                stand.requests(&format!("filecrypt.cc /Container/{}.html", s.id)),
            )
        })
        .collect();
    let seen = std::mem::take(&mut *seen.lock().unwrap());
    let (end, error) = match result {
        Ok(end) => (Some(end), None),
        Err(e) => (None, Some(e)),
    };
    Run {
        order,
        arrived: stand.arrived(),
        end,
        seen,
        friction: acc.friction,
        told: desktop.told,
        lagged: acc.lagged,
        reads,
        adds,
        unexpected: desktop.unexpected,
        error,
    }
}

// ---------------------------------------------------------------------------
// Checking a run.

/// What the table expects of one set.
struct Expected {
    archive: String,
    set: Set,
    /// Per part (from 1): hosts that must have a link on it, and those
    /// that may.
    must: Vec<BTreeSet<&'static str>>,
    may: Vec<BTreeSet<&'static str>>,
    /// A source's host tells this set's exact sizes.
    sized: bool,
}

fn expected(r: &Release) -> Vec<Expected> {
    r.sets
        .iter()
        .enumerate()
        .filter_map(|(at, set)| {
            let mut e = Expected {
                archive: set.archive(),
                set: set.clone(),
                must: vec![BTreeSet::new(); set.parts as usize],
                may: vec![BTreeSet::new(); set.parts as usize],
                sized: false,
            };
            for s in r.sources.iter().filter(|s| s.set == at && !s.gated) {
                for n in (1..=set.parts).filter(|n| !s.missing(*n) && !s.is_gone(*n)) {
                    let i = n as usize - 1;
                    if s.host == Host::Gofile && s.unnamed(n) {
                        e.may[i].insert(s.host.name());
                    } else {
                        e.must[i].insert(s.host.name());
                        e.sized |= s.host.tells_size();
                    }
                }
            }
            e.must.iter().any(|m| !m.is_empty()).then_some(e)
        })
        .collect()
}

/// The notes the release's card shows. Cards have none yet: CARD-02 gives
/// them typed notes, which this reads then.
fn notes(_: &Snapshot) -> Vec<String> {
    Vec::new()
}

/// Each archive's parts with their hosts, then the rows of no archive,
/// what is failed and how many cards: what every order must agree on. A
/// link only its host could name counts only where it must be.
fn digest(r: &Release, run: &Run) -> String {
    let Some(s) = &run.end else {
        return run.error.clone().unwrap_or_default();
    };
    let must: BTreeSet<&str> = expected(r)
        .iter()
        .flat_map(|e| e.must.iter().flatten().copied())
        .collect();
    let mut out = Vec::new();
    for a in &s.archives {
        let mut parts: Vec<String> = a
            .ids
            .iter()
            .filter_map(|id| s.items.iter().find(|i| &i.id == id))
            .map(|i| {
                let hosts: BTreeSet<&str> = i
                    .mirrors
                    .iter()
                    .map(String::as_str)
                    .filter(|h| must.contains(h))
                    .collect();
                let hosts: Vec<&str> = hosts.into_iter().collect();
                format!("{}={}", i.name, hosts.join("+"))
            })
            .collect();
        parts.sort();
        out.push(format!("{}[{}]", a.name, parts.join(" ")));
    }
    out.sort();
    let failed = s
        .items
        .iter()
        .filter(|i| i.list == Some(List::Failed))
        .count();
    format!(
        "{} | failed {failed} | cards {}",
        out.join(" "),
        s.wrapper_cards.len()
    )
}

fn check(r: &Release, run: &Run, c: &Check) -> Result<(), String> {
    let Some(s) = &run.end else {
        return Err(run.error.clone().unwrap_or_default());
    };
    match c {
        Check::OneCard => {
            let keys: BTreeSet<&str> = run.seen.cards.iter().map(|(k, _)| k.as_str()).collect();
            let counted = run
                .seen
                .cards
                .iter()
                .find(|(_, name)| name.contains("wrapper"));
            match s.wrapper_cards.as_slice() {
                _ if run.seen.most_cards > 1 => {
                    Err(format!("{} cards at once", run.seen.most_cards))
                }
                _ if keys.len() > 1 => {
                    Err(format!("the card changed its key {} times", keys.len() - 1))
                }
                _ if counted.is_some() => Err(format!("a card named {:?}", counted.map(|c| &c.1))),
                [card] if card.name == r.title => Ok(()),
                cards => Err(format!(
                    "cards {:?}",
                    cards.iter().map(|c| c.name.as_str()).collect::<Vec<_>>()
                )),
            }
        }
        Check::Sets => {
            let want = expected(r);
            let mut placed = HashSet::new();
            for e in &want {
                let Some(a) = s.archives.iter().find(|a| a.name == e.archive) else {
                    let have: Vec<_> = s.archives.iter().map(|a| a.name.as_str()).collect();
                    return Err(format!("no archive {}; archives {have:?}", e.archive));
                };
                let mut by_part: BTreeMap<u32, Vec<&haul_core::ItemView>> = BTreeMap::new();
                for i in a
                    .ids
                    .iter()
                    .filter_map(|id| s.items.iter().find(|i| &i.id == id))
                {
                    placed.insert(i.id.clone());
                    let n = PartName::parse(&i.name).map_or(0, |p| p.number);
                    by_part.entry(n).or_default().push(i);
                }
                for n in 1..=e.set.parts {
                    let rows = by_part.remove(&n).unwrap_or_default();
                    let [row] = rows.as_slice() else {
                        return Err(format!("{}: part {n} has {} rows", e.archive, rows.len()));
                    };
                    let hosts: BTreeSet<&str> = row.mirrors.iter().map(String::as_str).collect();
                    let (must, may) = (&e.must[n as usize - 1], &e.may[n as usize - 1]);
                    if !must.is_subset(&hosts)
                        || !hosts.iter().all(|h| must.contains(h) || may.contains(h))
                    {
                        return Err(format!(
                            "{}: part {n} on {hosts:?}, wants {must:?} (or {may:?})",
                            e.archive
                        ));
                    }
                }
                if let Some((n, rows)) = by_part.into_iter().next() {
                    return Err(format!("{}: {} row(s) as part {n}", e.archive, rows.len()));
                }
            }
            let extra: Vec<&str> = s
                .items
                .iter()
                .filter(|i| i.list.is_some() && !i.tentative && !placed.contains(&i.id))
                .filter(|i| !(i.gone && i.list == Some(List::Failed)))
                .map(|i| i.name.as_str())
                .collect();
            match extra.len() {
                0 => Ok(()),
                n => Err(format!("{n} other rows, as {:?}", &extra[..n.min(3)])),
            }
        }
        Check::NeverUnnamed => match &run.seen.unnamed {
            None => Ok(()),
            Some(name) => Err(format!("a row named {name}")),
        },
        Check::NoRowVanishes => {
            let gone: Vec<&String> = run
                .seen
                .listed
                .iter()
                .filter(|(id, _)| !s.items.iter().any(|i| &i.id == *id))
                .map(|(_, name)| name)
                .collect();
            match gone.len() {
                0 => Ok(()),
                n => Err(format!(
                    "{n} rows came and went, as {:?}",
                    &gone[..n.min(2)]
                )),
            }
        }
        Check::ExactSizes => {
            for e in expected(r).iter().filter(|e| e.sized) {
                let Some(a) = s.archives.iter().find(|a| a.name == e.archive) else {
                    return Err(format!("no archive {}", e.archive));
                };
                for i in a
                    .ids
                    .iter()
                    .filter_map(|id| s.items.iter().find(|i| &i.id == id))
                {
                    let n = PartName::parse(&i.name).map_or(0, |p| p.number);
                    if n >= 1 && i.size != (e.set.size)(n) {
                        return Err(format!("{} is {} bytes", i.name, i.size));
                    }
                }
                let total: u64 = (1..=e.set.parts).map(e.set.size).sum();
                if a.size != total {
                    return Err(format!(
                        "{} totals {} bytes, not {total}",
                        e.archive, a.size
                    ));
                }
            }
            Ok(())
        }
        Check::NothingFailed => match s.counts.failed {
            0 => Ok(()),
            n => Err(format!("{n} under Failed")),
        },
        Check::Note(text) => match notes(s).iter().any(|n| n.contains(text)) {
            true => Ok(()),
            false => Err(format!("notes {:?}", notes(s))),
        },
        Check::NoFailureTold => {
            let failed: usize = run.told.iter().filter(|(k, _)| *k == Told::Failed).count();
            match failed {
                0 => Ok(()),
                n => Err(format!("{n} notifications of a failure")),
            }
        }
        Check::Notifications(want) => {
            let n: u32 = run.friction.iter().map(|f| f.notifications).sum();
            if n == *want {
                Ok(())
            } else {
                Err(format!("{n} notifications"))
            }
        }
        Check::Asks(want) => {
            let n: usize = run.friction.iter().map(|f| f.asks.len()).sum();
            if n == *want {
                Ok(())
            } else {
                Err(format!("{n} asks"))
            }
        }
        Check::OneReadPerContainer => {
            let more: Vec<String> = run
                .reads
                .iter()
                .filter(|(id, n)| *n > run.adds && !r.sources.iter().any(|s| s.id == id && s.gated))
                .map(|(id, n)| format!("{id} {n}×"))
                .collect();
            if more.is_empty() {
                Ok(())
            } else {
                Err(more.join(", "))
            }
        }
        Check::SeparateCounts => match (run.seen.lumped, run.seen.checking) {
            (true, _) => Err("\"Unwrapping N links\"".into()),
            (false, 0) => Err("never said it checks links".into()),
            (false, _) => Ok(()),
        },
        Check::SameEveryOrder => Ok(()),
    }
}

// ---------------------------------------------------------------------------
// The suite.

#[tokio::test]
async fn every_release_comes_to_one_release() {
    let _turn = ONE_AT_A_TIME.lock().await;
    let only = std::env::var("HAUL_RELEASE_CASE").ok();
    let mut table = Vec::new();
    let mut wrong = 0;
    for release in cases() {
        if only.as_deref().is_some_and(|o| !release.case.contains(o)) {
            continue;
        }
        let release = Arc::new(release);
        let mut runs = Vec::new();
        for &order in &release.orders {
            runs.push(run(&release, order).await);
        }
        table.push(format!("{} ({}):", release.case, release.title));
        for r in &runs {
            table.push(format!(
                "       {:<26} read {:?}; {}{}",
                r.order.label(),
                r.arrived,
                r.friction_line(),
                if r.unexpected.is_empty() {
                    String::new()
                } else {
                    format!("; passes not stood in for: {:?}", r.unexpected)
                }
            ));
        }
        for want in &release.wants {
            let got: Result<(), String> = match &want.check {
                Check::SameEveryOrder => {
                    let first = digest(&release, &runs[0]);
                    match runs.iter().find(|r| digest(&release, r) != first) {
                        None => Ok(()),
                        Some(r) => Err(format!(
                            "{} differs from {}:\n           {}\n           {first}",
                            r.order.label(),
                            runs[0].order.label(),
                            digest(&release, r)
                        )),
                    }
                }
                c => runs
                    .iter()
                    .find_map(|r| {
                        check(&release, r, c)
                            .err()
                            .map(|e| format!("{}: {e}", r.order.label()))
                    })
                    .map_or(Ok(()), Err),
            };
            let (mark, ok) = match (&got, want.until) {
                (Ok(()), None) => ("pass".to_string(), true),
                (Err(_), None) => ("FAIL".to_string(), false),
                (Err(_), Some(task)) => (format!("xfail {task}"), true),
                (Ok(()), Some(task)) => (format!("XPASS {task}"), false),
            };
            if !ok {
                wrong += 1;
            }
            let detail = match &got {
                Ok(()) => String::new(),
                Err(e) => format!(": {e}"),
            };
            table.push(format!("  {mark:<14} {}{detail}", want.check.says()));
        }
    }
    let table = table.join("\n");
    assert_eq!(
        wrong, 0,
        "{wrong} of the release suite's checks failed, or passed while marked to fail:\n{table}"
    );
    println!("{table}");
}

/// What a report needs for these problems (TEST-04): a paste that lists
/// no links leaves its text in the report's log, and nowhere a person sees
/// it; each source's rows as read are a `listing`; the pass queue says who
/// joined it, at which place, and whose pass started; and each ask is an
/// `ask`, while the report's `friction` counts it.
#[tokio::test]
async fn a_report_shows_the_paste_the_listings_the_pass_queue_and_the_asks() {
    let _turn = ONE_AT_A_TIME.lock().await;
    let release = Arc::new(
        Release::new("report", "Example.Set", "example-set")
            .set("Example.Set", 2, two_gib)
            .paste("Example.Set: a made-up release in 2 parts. Unpack part 1 with any archiver.")
            .source(source("AAAAAAAA", Host::Pixeldrain, 0))
            .source(source("BBBBBBBB", Host::Rapidgator, 0))
            .gate("BBBBBBBB"),
    );
    let (stand, port) = Stand::start(release.clone(), vec![0, 1]).await;
    let tmp = tempfile::tempdir().unwrap();
    // The log the report holds: this test's, for the whole process.
    haul_core::logging::init(&tmp.path().join("data"), false, false);
    hosts::set_stand_in(Some(port));
    let engine = Engine::start(options(tmp.path())).await.unwrap();
    engine.set_pages_pass(true);
    engine.add_text(&release.url(), "");
    let mut desktop = Desktop::default();
    let settled = drive(&engine, &stand, &mut desktop, |_| false).await;
    let s = engine.snapshot();
    let (_, zip) = engine.report_zip(serde_json::Value::Null).unwrap();
    let report = engine.diagnostics(serde_json::Value::Null);
    engine.shutdown().await;
    hosts::set_stand_in(None);
    settled.unwrap();

    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(zip)).unwrap();
    let mut log = String::new();
    std::io::Read::read_to_string(&mut zip.by_name("haul.log").unwrap(), &mut log).unwrap();
    let lines = |what: &str| log.lines().filter(|l| l.contains(what)).count();
    let text = "a made-up release in 2 parts";
    assert_eq!(lines("paste lists no links"), 1, "{log}");
    assert!(
        log.contains(text) && log.contains(r#"\"length\":"#),
        "{log}"
    );
    assert!(
        !serde_json::to_string(&s).unwrap().contains(text),
        "never in the UI"
    );
    // The page and its two containers; the gated one listed nothing.
    assert_eq!(lines("listing: "), 2, "{log}");
    assert!(
        log.contains(r#"\"name\":\"Example.Set.part1.rar\""#),
        "{log}"
    );
    assert_eq!(lines("pass queue queued"), 1, "{log}");
    assert_eq!(lines("pass queue started"), 1, "{log}");
    assert_eq!(lines("asks a person: filecrypt.cc (gate)"), 1, "{log}");
    let friction = &report["friction"];
    assert_eq!(friction.as_array().map(Vec::len), Some(1), "{friction}");
    let ask = &friction[0]["asks"][0];
    assert_eq!(
        (
            ask["site"].as_str(),
            ask["need"].as_str(),
            ask["gate"].as_str(),
            ask["needed"].as_bool()
        ),
        (
            Some("filecrypt.cc"),
            Some("gate"),
            Some("a CutCaptcha puzzle"),
            Some(false)
        ),
        "the release is whole without it: {friction}"
    );
    assert_eq!(friction[0]["notifications"], 1, "{friction}");
}
