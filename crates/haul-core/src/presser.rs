//! Which download button a pass presses on a file's page, and which it
//! never presses again (plan PASS-08). The page script only lists the
//! buttons that say "download"; what a press did decides the rest: a button
//! whose press made the page try to leave its site, or open a pop-up, is an
//! ad, pressed once at most. No list of words tells an ad from the real
//! button any more than its first press does.

use std::{
    collections::{HashMap, HashSet},
    time::{Duration, Instant},
};

/// A button the page lists, in the page's order.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    /// What tells it apart on this page: its place among the page's buttons
    /// and its words.
    pub key: String,
    /// Its words, for the log.
    pub text: String,
}

/// The presses of one pass.
#[derive(Debug, Default)]
pub struct Presser {
    pressed: HashMap<String, u32>,
    ads: HashSet<String>,
    /// The last press, and when: what a refusal or a pop-up soon after it
    /// is blamed on.
    last: Option<(String, Instant)>,
    at: Option<Instant>,
}

impl Presser {
    /// Presses of one button, at most: a page that takes a few to start
    /// its file.
    pub const AT_MOST: u32 = 3;
    /// Presses in one pass, all buttons together: past that the page wants
    /// more than a button (a captcha a person solves).
    pub const IN_ALL: u32 = 12;
    /// Between two presses: the page's next step (a short countdown, a
    /// link being made) comes first.
    pub const EVERY: Duration = Duration::from_secs(3);
    /// What the page does this long after a press is the press's doing.
    pub const WATCH: Duration = Duration::from_secs(3);

    /// The button to press next among `listed`, if one is due: the first
    /// that is no ad and has presses left. Counts the press.
    pub fn pick<'a>(&mut self, listed: &'a [Candidate], now: Instant) -> Option<&'a Candidate> {
        if self.presses() >= Self::IN_ALL
            || self
                .at
                .is_some_and(|at| now.duration_since(at) < Self::EVERY)
        {
            return None;
        }
        let next = listed.iter().find(|c| {
            !self.ads.contains(&c.key)
                && self.pressed.get(&c.key).copied().unwrap_or(0) < Self::AT_MOST
        })?;
        *self.pressed.entry(next.key.clone()).or_default() += 1;
        self.last = Some((next.key.clone(), now));
        self.at = Some(now);
        Some(next)
    }

    /// The page tried to leave its site, or opened a pop-up, at `now`: the
    /// button pressed just before is an ad, never pressed again. Returns
    /// its key, when one is blamed.
    pub fn left(&mut self, now: Instant) -> Option<String> {
        let (key, at) = self.last.as_ref()?;
        if now.duration_since(*at) > Self::WATCH || self.ads.contains(key) {
            return None;
        }
        self.ads.insert(key.clone());
        // The next button needn't wait out the ad's turn.
        self.at = None;
        Some(key.clone())
    }

    /// Presses so far, all buttons together.
    pub fn presses(&self) -> u32 {
        self.pressed.values().sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listed(keys: &[&str]) -> Vec<Candidate> {
        keys.iter()
            .map(|k| Candidate {
                key: (*k).into(),
                text: format!("{k} Download"),
            })
            .collect()
    }

    /// An ad's button above the real one: pressed once, it tries to leave,
    /// and the real one is pressed next and from then on.
    #[test]
    fn an_ads_button_is_pressed_once_and_the_next_one_taken() {
        let page = listed(&["0 High-speed", "1 Free"]);
        let mut p = Presser::default();
        let t = Instant::now();
        assert_eq!(p.pick(&page, t).unwrap().key, "0 High-speed");
        assert_eq!(
            p.left(t + Duration::from_secs(1)).as_deref(),
            Some("0 High-speed")
        );
        assert_eq!(
            p.pick(&page, t + Duration::from_secs(1)).unwrap().key,
            "1 Free"
        );
        let later = t + Duration::from_secs(20);
        assert_eq!(p.pick(&page, later).unwrap().key, "1 Free");
        assert_eq!(p.pressed["0 High-speed"], 1);
    }

    /// A press that leads nowhere else is pressed again, three times at
    /// most, a few seconds apart; a refusal long after it is no press's.
    #[test]
    fn a_quiet_button_is_pressed_three_times_at_most() {
        let page = listed(&["0 Free"]);
        let mut p = Presser::default();
        let t = Instant::now();
        assert!(p.pick(&page, t).is_some());
        assert!(
            p.pick(&page, t + Duration::from_secs(1)).is_none(),
            "too soon"
        );
        assert_eq!(p.left(t + Duration::from_secs(10)), None);
        for n in 1..=4 {
            p.pick(&page, t + Presser::EVERY * n);
        }
        assert_eq!(p.presses(), Presser::AT_MOST);
    }
}
