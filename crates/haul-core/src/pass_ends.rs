//! How a page's pass in the desktop app's browser ended when its page said
//! nothing, and what comes next.
//!
//! A pass that ends "closed" without anyone stopping it is the browser's
//! doing, not the page's: nobody is asked about it. It is tried again at
//! once, [`AGAIN`] times, then once in the second browser the desktop app
//! found (Chrome, after Edge) when there is one, and only then fails, as the browser
//! failing, with its reason. A pass Haul stopped (its row went or stopped
//! waiting, or a person opened the page) is not tried again.

/// How many times a pass that closed by itself is tried again at once in the
/// same browser, before the other one.
pub const AGAIN: u32 = 3;

/// Why a pass ended without the page's word.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Closed {
    /// Haul stopped it.
    Stopped,
    /// The browser's DevTools no longer answers.
    NoDevTools(Process),
    /// DevTools answers, but no longer has the pass's page.
    PageGone(Process),
    /// DevTools answered, then failed while Haul set up the page (attached
    /// to it, added its scripts, sent it to its address): the browser went
    /// away, or would not take the page.
    SetUp { error: String, process: Process },
    /// The browser never opened DevTools: it exited at start, or took too
    /// long.
    NotStarted(String),
}

/// What became of the process Haul started, and what answers on the port it
/// was told to open DevTools on, when the pass ended.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Process {
    /// How it exited (`exit code: 0`), or `None` while it runs.
    pub exited: Option<String>,
    /// What answers on DevTools' port: the browser's name and version
    /// (`/json/version`), or `None` for nothing.
    pub answers: Option<String>,
}

impl Closed {
    /// What ended the pass, in words for the log and the row.
    pub fn cause(&self) -> String {
        let process = |p: &Process| {
            let exited = match &p.exited {
                Some(how) => format!("the browser Haul started exited ({how})"),
                None => "the browser Haul started still runs".into(),
            };
            let answers = match &p.answers {
                Some(what) => format!("{what} answers on its DevTools port"),
                None => "nothing answers on its DevTools port".into(),
            };
            format!("{exited}; {answers}")
        };
        match self {
            Closed::Stopped => "Haul stopped it".into(),
            Closed::NoDevTools(p) => format!("its DevTools stopped answering: {}", process(p)),
            Closed::PageGone(p) => format!("its page was closed: {}", process(p)),
            Closed::SetUp { error, process: p } => {
                format!(
                    "it failed while Haul set up its page ({error}): {}",
                    process(p)
                )
            }
            Closed::NotStarted(why) => why.clone(),
        }
    }

    /// A short name of the cause, for the signals.
    pub fn kind(&self) -> &'static str {
        match self {
            Closed::Stopped => "stopped",
            Closed::NoDevTools(_) => "devtools",
            Closed::PageGone(_) => "page",
            Closed::SetUp { .. } => "setup",
            Closed::NotStarted(_) => "not_started",
        }
    }
}

/// What the desktop app does after a pass ended ([`Tries::ended`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Next {
    /// Nothing, and nobody is asked: Haul stopped the pass.
    Stop,
    /// Passes the page again at once, in browser number `browser` of those
    /// [`Tries::new`] was given.
    Again { browser: usize },
    /// The first try's browser never opened DevTools: told as such a pass
    /// is today, with its reason.
    NotStarted(String),
    /// Gives up, as the browser failing rather than the page wanting a
    /// person: the page fails with this reason.
    Fail(String),
}

/// A page's tries in the browsers the desktop app found, in their order:
/// the first, then the other.
#[derive(Debug, Clone)]
pub struct Tries {
    browsers: Vec<String>,
    browser: usize,
    /// Tries ended so far, in each browser.
    ended: Vec<u32>,
    last: Option<Closed>,
}

impl Tries {
    /// For the browsers found, named (`Edge`, `Chrome`), first first. At
    /// least one is needed for a pass at all; any past the second are not
    /// tried.
    pub fn new(browsers: Vec<String>) -> Self {
        let ended = vec![0; browsers.len()];
        Self {
            browsers,
            browser: 0,
            ended,
            last: None,
        }
    }

    /// The browser the next try runs in, by its number.
    pub fn browser(&self) -> usize {
        self.browser
    }

    /// The try that is next, from 1.
    pub fn number(&self) -> u32 {
        self.ended.iter().sum::<u32>() + 1
    }

    /// The pass ended `how`: what to do next.
    pub fn ended(&mut self, how: Closed) -> Next {
        if how == Closed::Stopped {
            return Next::Stop;
        }
        if let Closed::NotStarted(why) = &how
            && self.number() == 1
        {
            return Next::NotStarted(why.clone());
        }
        if let Some(n) = self.ended.get_mut(self.browser) {
            *n += 1;
        }
        self.last = Some(how);
        if self.browser == 0 && self.ended[0] <= AGAIN {
            return Next::Again { browser: 0 };
        }
        if self.browser == 0 && self.browsers.len() > 1 {
            self.browser = 1;
            return Next::Again { browser: 1 };
        }
        Next::Fail(self.reason())
    }

    /// Why the page failed, once every try is used up.
    fn reason(&self) -> String {
        let times = |n: u32| match n {
            1 => "once".to_string(),
            2 => "twice".to_string(),
            n => format!("{n} times"),
        };
        let tried: Vec<String> = self
            .browsers
            .iter()
            .zip(&self.ended)
            .filter(|(_, n)| **n > 0)
            .map(|(name, n)| format!("{} in {name}", times(*n)))
            .collect();
        let cause = self.last.as_ref().map(Closed::cause).unwrap_or_default();
        format!(
            "Haul's browser closed the page by itself, {}; the last time {cause}",
            tried.join(" and ")
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gone() -> Closed {
        Closed::NoDevTools(Process {
            exited: Some("exit code: 0".into()),
            answers: None,
        })
    }

    fn both() -> Tries {
        Tries::new(vec!["Edge".into(), "Chrome".into()])
    }

    #[test]
    fn a_pass_that_closes_is_tried_again_three_times_then_in_the_other_browser() {
        let mut t = both();
        for n in 1..=AGAIN {
            assert_eq!(t.ended(gone()), Next::Again { browser: 0 }, "try {n}");
        }
        assert_eq!(t.number(), AGAIN + 1);
        assert_eq!(t.ended(gone()), Next::Again { browser: 1 });
        assert_eq!(t.browser(), 1);
        let Next::Fail(why) = t.ended(gone()) else {
            panic!("fails after the second browser");
        };
        assert_eq!(
            why,
            "Haul's browser closed the page by itself, 4 times in Edge and once in Chrome; \
             the last time its DevTools stopped answering: the browser Haul started exited \
             (exit code: 0); nothing answers on its DevTools port"
        );
    }

    #[test]
    fn with_one_browser_it_fails_after_its_tries() {
        let mut t = Tries::new(vec!["Chrome".into()]);
        for _ in 0..AGAIN {
            assert_eq!(t.ended(gone()), Next::Again { browser: 0 });
        }
        let Next::Fail(why) = t.ended(gone()) else {
            panic!("fails once its tries are used up");
        };
        assert!(why.contains("4 times in Chrome;"), "{why}");
    }

    #[test]
    fn a_pass_haul_stopped_is_not_tried_again() {
        let mut t = both();
        assert_eq!(t.ended(Closed::Stopped), Next::Stop);
        assert_eq!(t.ended(gone()), Next::Again { browser: 0 });
        assert_eq!(t.ended(Closed::Stopped), Next::Stop);
    }

    #[test]
    fn a_first_try_that_never_started_is_told_as_before() {
        let mut t = both();
        let why = "the browser exited at start (exit code: 0)";
        assert_eq!(
            t.ended(Closed::NotStarted(why.into())),
            Next::NotStarted(why.into())
        );
    }

    #[test]
    fn a_first_try_whose_browser_went_during_set_up_is_tried_again() {
        let mut t = both();
        let set_up = Closed::SetUp {
            error: "DevTools: Connection reset without closing handshake".into(),
            process: Process::default(),
        };
        assert_eq!(set_up.kind(), "setup");
        assert_eq!(t.ended(set_up), Next::Again { browser: 0 });
    }

    #[test]
    fn a_later_try_that_never_started_counts_as_a_try() {
        let mut t = both();
        assert_eq!(t.ended(gone()), Next::Again { browser: 0 });
        let not_started = || Closed::NotStarted("the browser did not open DevTools in time".into());
        for _ in 1..AGAIN {
            assert_eq!(t.ended(not_started()), Next::Again { browser: 0 });
        }
        assert_eq!(t.ended(not_started()), Next::Again { browser: 1 });
        let Next::Fail(why) = t.ended(not_started()) else {
            panic!("fails after the second browser");
        };
        assert!(
            why.ends_with("the last time the browser did not open DevTools in time"),
            "{why}"
        );
    }

    #[test]
    fn each_cause_is_told_apart() {
        let page = Closed::PageGone(Process {
            exited: None,
            answers: Some("Edg/157.0".into()),
        });
        assert_eq!(
            page.cause(),
            "its page was closed: the browser Haul started still runs; Edg/157.0 answers on its DevTools port"
        );
        assert_eq!(page.kind(), "page");
        assert_eq!(Closed::Stopped.kind(), "stopped");
        assert_eq!(gone().kind(), "devtools");
    }
}
