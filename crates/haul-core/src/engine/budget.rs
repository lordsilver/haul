//! How fast Haul asks one host about files before any download (plan §4.3,
//! LOOK-01): a lookup through its API or a page opened to learn what a link
//! is. Each host gets a burst, then one lookup per spacing. A rate limit
//! doubles the spacing, and a run of clean answers eases it back toward the
//! start, so no number has to be exactly right: a measured one only improves
//! where it starts.

use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

/// A host's starting pace: lookups at once, then one per `spacing`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Pace {
    pub burst: u32,
    pub spacing: Duration,
}

/// The pace a host starts at. Known hosts by what they were seen to take
/// (docs/hosts.md); every other host a careful default.
pub(crate) fn start(host: &str) -> Pace {
    let pace = |burst, millis| Pace {
        burst,
        spacing: Duration::from_millis(millis),
    };
    match host {
        // An API made for it, which answers a wrapper's parts at once.
        "pixeldrain.com" | "1fichier.com" => pace(30, 100),
        // Turns a burst away with 429 (vikingfile's check-file) or locks
        // the address after about 50 requests in 2 minutes (buzzheavier).
        "vikingfile.com" | "buzzheavier.com" => pace(1, 2_500),
        // Its folder pages answer "Too many requests" to every fifth.
        "gofile.io" => pace(2, 3_000),
        _ => pace(4, 1_000),
    }
}

/// The longest spacing a rate limit brings a host to.
const SLOWEST: Duration = Duration::from_secs(120);
/// Clean answers in a row before the spacing eases back by half.
const EASE_AFTER: u32 = 10;

/// One host's lookups.
#[derive(Debug, Clone)]
pub(crate) struct Budget {
    start: Pace,
    spacing: Duration,
    /// Lookups that may go at once now.
    tokens: f64,
    at: Instant,
    clean: u32,
}

impl Budget {
    pub fn new(start: Pace, now: Instant) -> Self {
        Self {
            start,
            spacing: start.spacing,
            tokens: f64::from(start.burst),
            at: now,
            clean: 0,
        }
    }

    fn refill(&mut self, now: Instant) {
        let gained = now.saturating_duration_since(self.at).as_secs_f64()
            / self.spacing.as_secs_f64().max(0.001);
        self.tokens = (self.tokens + gained).min(f64::from(self.start.burst));
        self.at = now;
    }

    /// Takes a lookup's turn: how long it waits from `now` first.
    pub fn take(&mut self, now: Instant) -> Duration {
        self.refill(now);
        self.tokens -= 1.0;
        if self.tokens >= 0.0 {
            Duration::ZERO
        } else {
            self.spacing.mul_f64(-self.tokens)
        }
    }

    /// Whether a lookup may go at `now` without waiting.
    pub fn ready(&self, now: Instant) -> bool {
        let gained = now.saturating_duration_since(self.at).as_secs_f64()
            / self.spacing.as_secs_f64().max(0.001);
        (self.tokens + gained).min(f64::from(self.start.burst)) >= 1.0
    }

    /// The host turned a lookup away for asking too often.
    pub fn limited(&mut self, now: Instant) {
        self.refill(now);
        self.spacing = (self.spacing * 2).min(SLOWEST);
        self.tokens = self.tokens.min(0.0);
        self.clean = 0;
    }

    /// A lookup the host answered.
    pub fn answered(&mut self) {
        self.clean += 1;
        if self.clean >= EASE_AFTER {
            self.clean = 0;
            self.spacing = (self.spacing / 2).max(self.start.spacing);
        }
    }

    #[cfg(test)]
    pub fn spacing(&self) -> Duration {
        self.spacing
    }
}

/// Every host's budget.
#[derive(Debug, Default)]
pub(crate) struct Budgets(HashMap<String, Budget>);

impl Budgets {
    /// Whether a lookup on `host` may go at `now` without waiting.
    pub fn ready(&self, host: &str, now: Instant) -> bool {
        self.0.get(host).is_none_or(|b| b.ready(now))
    }

    pub fn of(&mut self, host: &str, now: Instant) -> &mut Budget {
        self.0
            .entry(host.to_string())
            .or_insert_with(|| Budget::new(start(host), now))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ten lookups on one host: a burst, then one per spacing.
    #[test]
    fn lookups_on_one_host_are_spaced() {
        let t = Instant::now();
        let mut b = Budget::new(
            Pace {
                burst: 2,
                spacing: Duration::from_secs(3),
            },
            t,
        );
        let waits: Vec<u64> = (0..10).map(|_| b.take(t).as_secs()).collect();
        assert_eq!(waits, [0, 0, 3, 6, 9, 12, 15, 18, 21, 24]);
    }

    /// A rate limit doubles the spacing and empties the burst; clean
    /// answers ease it back, never below where it started.
    #[test]
    fn a_rate_limit_slows_the_host_and_clean_answers_ease_it() {
        let t = Instant::now();
        let mut b = Budget::new(
            Pace {
                burst: 4,
                spacing: Duration::from_secs(1),
            },
            t,
        );
        b.limited(t);
        assert_eq!(b.spacing(), Duration::from_secs(2));
        assert!(!b.ready(t), "the lane cools down");
        assert_eq!(b.take(t), Duration::from_secs(2));
        b.limited(t);
        assert_eq!(b.spacing(), Duration::from_secs(4));
        for _ in 0..EASE_AFTER {
            b.answered();
        }
        assert_eq!(b.spacing(), Duration::from_secs(2));
        for _ in 0..EASE_AFTER * 3 {
            b.answered();
        }
        assert_eq!(b.spacing(), Duration::from_secs(1));
    }
}
