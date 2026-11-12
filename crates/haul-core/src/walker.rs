//! Walking a page on a site Haul has no code for until it leads somewhere
//! Haul knows: a link shortener's captcha, countdowns and "Continue" pages,
//! a mirror index's button, a file page's download. The pass presses what a
//! person would, page after page and site after site, and this decides
//! which button, how often, and whether the page may leave its site.
//!
//! Nothing here knows a site. A button is one of the page's own steps by
//! its words; a page may change site only as a press's doing (soon after
//! one, or along the hops that follow it), so an ad that sends the whole
//! tab away by itself is refused; and a walk that does nothing for a while
//! has nothing left to do.

use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

/// A step the page offers, in the page's order.
#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    /// What tells it apart on its page: its place among the page's buttons
    /// and its words.
    pub key: String,
    /// Its words, for the log.
    pub text: String,
    /// Where it links, when that is another site.
    pub leads: Option<String>,
}

/// What a walk does next on the page it is on.
#[derive(Debug, PartialEq)]
pub enum Next<'a> {
    /// Press this step.
    Press(&'a Step),
    /// The page's captcha has had its time without an answer: load the page
    /// again, which gives it a new one.
    Reload,
    /// Nothing to do now: a captcha answers itself, a countdown runs, or
    /// the last press is still at work.
    Wait,
    /// Nothing happened for [`Walker::IDLE`]: the walk is over.
    Over,
}

/// One pass's walk.
#[derive(Debug)]
pub struct Walker {
    /// Presses of each step, by page address and step key.
    pressed: HashMap<(String, String), u32>,
    presses: u32,
    /// The last press, or the walk's start: what a change of site soon
    /// after is the doing of.
    moved: Instant,
    /// The last thing that happened (a press, a hop, a new page, a captcha
    /// answered), for [`Walker::IDLE`].
    busy: Instant,
    started: Instant,
    last_press: Option<Instant>,
    /// The page the walk is on, and since when its captcha waits.
    page: String,
    captcha_since: Option<Instant>,
    reloaded: HashMap<String, u32>,
}

impl Walker {
    /// Presses of one step on one page, at most: a page whose scripts load
    /// late takes a few, and an ad's layer takes the first.
    pub const AT_MOST: u32 = 6;
    /// Presses in one walk, all steps together.
    pub const IN_ALL: u32 = 80;
    /// Between two presses: the page's next step (a countdown, a page
    /// loading) comes first.
    pub const EVERY: Duration = Duration::from_secs(3);
    /// A page that changes site this long after a press, or after the hop
    /// before it, goes where the press sent it.
    pub const AFTER_PRESS: Duration = Duration::from_secs(15);
    /// A captcha that answers itself has by now, or will not: its page is
    /// loaded again, [`Walker::RELOADS`] times at most.
    pub const CAPTCHA_FOR: Duration = Duration::from_secs(40);
    pub const RELOADS: u32 = 2;
    /// A walk with nothing pressed, no new page and no captcha at work for
    /// this long is over.
    pub const IDLE: Duration = Duration::from_secs(45);
    /// A walk ends this long after it started, whatever it is doing: a page
    /// that keeps it busy for ever (a dialog that comes back, hops that
    /// never end) holds up the pages passed after it.
    pub const LONGEST: Duration = Duration::from_secs(300);

    pub fn new(now: Instant) -> Self {
        Self {
            pressed: HashMap::new(),
            presses: 0,
            moved: now,
            busy: now,
            started: now,
            last_press: None,
            page: String::new(),
            captcha_since: None,
            reloaded: HashMap::new(),
        }
    }

    /// Whether the page may go to another site at `now`: on the way to its
    /// first page, soon after a press, or along the hops after one. Counts
    /// the hop when it may.
    pub fn may_leave(&mut self, now: Instant) -> bool {
        let may = now.duration_since(self.moved) <= Self::AFTER_PRESS;
        if may {
            self.moved = now;
            self.busy = now;
        }
        may
    }

    /// The walk's next move on the page at `address`, which offers `steps`
    /// and whose captcha still `waits` for its answer.
    pub fn next<'a>(
        &mut self,
        address: &str,
        steps: &'a [Step],
        waits: bool,
        now: Instant,
    ) -> Next<'a> {
        if now.duration_since(self.started) >= Self::LONGEST {
            return Next::Over;
        }
        if address != self.page {
            self.page = address.to_string();
            self.captcha_since = None;
            self.busy = now;
        }
        if waits {
            let since = *self.captcha_since.get_or_insert(now);
            if now.duration_since(since) >= Self::CAPTCHA_FOR {
                let tries = self.reloaded.entry(self.page.clone()).or_default();
                if *tries >= Self::RELOADS {
                    return self.idle(now);
                }
                *tries += 1;
                self.captcha_since = None;
                self.busy = now;
                // The page loads again by the walk's doing.
                self.moved = now;
                return Next::Reload;
            }
            // A captcha at work is not idling, until its time is up.
            self.busy = now;
            return Next::Wait;
        }
        if self.captcha_since.take().is_some() {
            self.busy = now;
        }
        if self
            .last_press
            .is_some_and(|at| now.duration_since(at) < Self::EVERY)
        {
            return Next::Wait;
        }
        let left = |s: &&Step| {
            self.pressed
                .get(&(self.page.clone(), s.key.clone()))
                .copied()
                .unwrap_or(0)
                < Self::AT_MOST
        };
        let Some(step) = steps
            .iter()
            .find(left)
            .filter(|_| self.presses < Self::IN_ALL)
        else {
            return self.idle(now);
        };
        *self
            .pressed
            .entry((self.page.clone(), step.key.clone()))
            .or_default() += 1;
        self.presses += 1;
        self.last_press = Some(now);
        self.moved = now;
        self.busy = now;
        Next::Press(step)
    }

    /// A press did not reach its step (a dialog covered it and was answered
    /// instead): it does not count against the step.
    pub fn not_pressed(&mut self, step: &Step) {
        if let Some(n) = self.pressed.get_mut(&(self.page.clone(), step.key.clone())) {
            *n = n.saturating_sub(1);
        }
        self.presses = self.presses.saturating_sub(1);
    }

    fn idle<'a>(&self, now: Instant) -> Next<'a> {
        if now.duration_since(self.busy) >= Self::IDLE {
            Next::Over
        } else {
            Next::Wait
        }
    }

    /// Presses so far, all steps together.
    pub fn presses(&self) -> u32 {
        self.presses
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn steps(keys: &[&str]) -> Vec<Step> {
        keys.iter()
            .map(|k| Step {
                key: (*k).into(),
                text: (*k).into(),
                leads: None,
            })
            .collect()
    }

    const PAGE: &str = "https://short.example/abc123";

    /// A press takes the page to the next site, whose hops follow at once;
    /// a tab sent away long after any press is an ad's doing.
    #[test]
    fn a_page_leaves_its_site_only_as_a_presss_doing() {
        let t = Instant::now();
        let mut w = Walker::new(t);
        assert!(w.may_leave(t + Duration::from_secs(1)), "the first page");
        let page = steps(&["0 Continue"]);
        let late = t + Duration::from_secs(60);
        assert!(!w.may_leave(late), "an ad, a minute after anything");
        assert_eq!(w.next(PAGE, &page, false, late), Next::Press(&page[0]));
        assert!(w.may_leave(late + Duration::from_secs(4)));
        assert!(w.may_leave(late + Duration::from_secs(12)), "the hop after");
        assert!(!w.may_leave(late + Duration::from_secs(40)));
    }

    /// Nothing is pressed while the page's captcha waits for its answer; one
    /// that never comes has its page loaded again, twice, and then the walk
    /// ends.
    #[test]
    fn a_captcha_is_waited_for_then_its_page_loaded_again() {
        let t = Instant::now();
        let mut w = Walker::new(t);
        let page = steps(&["0 Continue"]);
        assert_eq!(w.next(PAGE, &page, true, t), Next::Wait);
        let at = |s| t + Duration::from_secs(s);
        assert_eq!(w.next(PAGE, &page, true, at(39)), Next::Wait);
        assert_eq!(w.next(PAGE, &page, true, at(40)), Next::Reload);
        assert_eq!(w.next(PAGE, &page, true, at(41)), Next::Wait);
        assert_eq!(w.next(PAGE, &page, true, at(81)), Next::Reload);
        assert_eq!(w.next(PAGE, &page, true, at(82)), Next::Wait);
        assert_eq!(w.next(PAGE, &page, true, at(122)), Next::Wait);
        assert_eq!(w.next(PAGE, &page, true, at(170)), Next::Over);
        // Answered in time, the step is pressed.
        let mut w = Walker::new(t);
        assert_eq!(w.next(PAGE, &page, true, t), Next::Wait);
        assert_eq!(w.next(PAGE, &page, false, at(3)), Next::Press(&page[0]));
    }

    /// A step is pressed a few seconds apart, six times at most on one
    /// page, and anew on the next page; with nothing left the walk ends
    /// after a while.
    #[test]
    fn a_step_is_pressed_a_few_times_and_an_idle_walk_ends() {
        let t = Instant::now();
        let mut w = Walker::new(t);
        let page = steps(&["0 Continue"]);
        assert!(matches!(w.next(PAGE, &page, false, t), Next::Press(_)));
        assert_eq!(
            w.next(PAGE, &page, false, t + Duration::from_secs(1)),
            Next::Wait
        );
        for n in 1..Walker::AT_MOST {
            assert!(matches!(
                w.next(PAGE, &page, false, t + Walker::EVERY * n),
                Next::Press(_)
            ));
        }
        let spent = t + Walker::EVERY * Walker::AT_MOST;
        assert_eq!(w.next(PAGE, &page, false, spent), Next::Wait);
        assert_eq!(w.next(PAGE, &page, false, spent + Walker::IDLE), Next::Over);
        let other = "https://short.example/next";
        assert!(matches!(
            w.next(other, &page, false, spent + Walker::IDLE),
            Next::Press(_)
        ));
        assert_eq!(w.presses(), Walker::AT_MOST + 1);
    }

    /// A press a dialog took instead does not count against its step.
    #[test]
    fn a_press_a_dialog_took_is_not_counted() {
        let t = Instant::now();
        let mut w = Walker::new(t);
        let page = steps(&["0 Get Link"]);
        for n in 0..20 {
            let now = t + Walker::EVERY * n;
            let Next::Press(step) = w.next(PAGE, &page, false, now) else {
                panic!("pressed each time");
            };
            w.not_pressed(step);
        }
        assert_eq!(w.presses(), 0);
        assert_eq!(w.next(PAGE, &page, false, t + Walker::LONGEST), Next::Over);
    }
}
