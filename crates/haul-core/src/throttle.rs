//! Telling a host that limits this address from a slow moment, a slow line or
//! the user's own speed limit. The rules and every number live here, with why;
//! what the engine does about the answer is in `engine/throttled.rs`. Nothing
//! here waits or looks at a clock, so all of it is tested with made-up times.

use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

/// Every number the rules go by. Tests shorten them; nothing else does.
#[derive(Debug, Clone, PartialEq)]
pub struct Throttling {
    /// Bytes per second under which a transfer is slow. "About 1 MB/s", set
    /// just above pixeldrain's 1 MiB/s for a slowed address, which is a
    /// throttle by the host's own word.
    pub slow: f64,
    /// How long a transfer runs before its bytes count: transfers start
    /// slowly, and a host may burst and then settle, which would flatter the
    /// average.
    pub grace: Duration,
    /// How long it has to be slow, as an average, so no bad moment decides.
    pub window: Duration,
    /// How often transfers are looked at. Under a window's tenth, so a window
    /// has enough samples; often enough that a mirror coming free is seen at
    /// once.
    pub tick: Duration,
    /// Time left under which a slow transfer stays where it is. Moving costs
    /// a lookup, up to two minutes of a page's pass and the next host's own
    /// grace, so a file done sooner would not gain.
    pub worth_moving: Duration,
    /// A host this many times below its own normal counts as slow above
    /// `slow` too, up to `far_below_ceiling`.
    pub far_below: f64,
    pub far_below_ceiling: f64,
    /// A transfer within this share of its host's normal is that host being
    /// what it is: nothing has changed, so nothing is limiting it.
    pub near_normal: f64,
    /// Alone, with nothing to compare with: this many times below what the
    /// host or the line has done proves the host is the cause.
    pub alone_below: f64,
    /// Alone with no history at all: slow for this long more counts as unsure,
    /// enough to try another mirror, not enough to redial.
    pub unsure_after: Duration,
    /// How much of a transfer another one's speed is read from. Shorter says
    /// little; a window would make the comparison a minute old.
    pub compare_over: Duration,
    /// Others slow at the same moment point at the line, but not for ever:
    /// past this, and with the line known to do far better, two hosts
    /// throttling at once are told apart from a slow line.
    pub line_patience: Duration,
    /// The first cooldown of a host that slows an address. How long its limit
    /// lasts isn't known, so it starts low and doubles while the host stays
    /// slow each time it is tried: ddownload's two hours is found in three
    /// tries, each costing a minute and a half.
    pub cooldown: Duration,
    /// The longest it doubles to: free limits rarely outlast a working day,
    /// and a host that lifted its limit shouldn't sit idle for longer.
    pub cooldown_at_most: Duration,
    /// How long new downloads may be held back so those under way can finish
    /// before a redial. Past it the redial is put off, not forced: it must
    /// not abort a healthy download. Long enough for what is about to finish,
    /// short enough not to stall the queue for what isn't.
    pub drain_limit: Duration,
    /// How often Haul asks itself whether a new address would help.
    pub redial_check: Duration,
    /// The least time between redials on its own: each cuts the line for the
    /// whole home.
    pub redial_gap: Duration,
}

impl Default for Throttling {
    fn default() -> Self {
        Self {
            slow: 1_200_000.0,
            grace: Duration::from_secs(30),
            window: Duration::from_secs(60),
            tick: Duration::from_secs(1),
            worth_moving: Duration::from_secs(10 * 60),
            far_below: 10.0,
            far_below_ceiling: 5_000_000.0,
            near_normal: 0.5,
            alone_below: 4.0,
            unsure_after: Duration::from_secs(120),
            compare_over: Duration::from_secs(10),
            line_patience: Duration::from_secs(5 * 60),
            cooldown: Duration::from_secs(30 * 60),
            cooldown_at_most: Duration::from_secs(4 * 3600),
            drain_limit: Duration::from_secs(10 * 60),
            redial_check: Duration::from_secs(30),
            redial_gap: Duration::from_secs(10 * 60),
        }
    }
}

impl Throttling {
    /// The speed under which a transfer from a host of this `normal` is slow.
    fn slow_for(&self, normal: f64) -> f64 {
        self.slow
            .max((normal / self.far_below).min(self.far_below_ceiling))
    }

    /// How long a host stays on cooldown after it slowed an address `strikes`
    /// times in a row.
    pub(crate) fn cooldown_for(&self, strikes: u32) -> Duration {
        let doubled = self
            .cooldown
            .saturating_mul(1u32 << strikes.saturating_sub(1).min(16));
        doubled.min(self.cooldown_at_most)
    }
}

/// How one transfer's speed has gone, read every tick.
#[derive(Debug)]
pub(crate) struct Pace {
    started: Instant,
    samples: VecDeque<(Instant, u64)>,
    /// Since when it has been slow without a break.
    slow_since: Option<Instant>,
    /// The last answer, for the debug view and for telling a change.
    pub verdict: Verdict,
}

impl Pace {
    pub fn new(now: Instant, done: u64) -> Self {
        Self {
            started: now,
            samples: VecDeque::from([(now, done)]),
            slow_since: None,
            verdict: Verdict::TooEarly,
        }
    }

    /// Notes the bytes it has at `now`, keeping as much as the readings below
    /// go back.
    pub fn sample(&mut self, now: Instant, done: u64, t: &Throttling) {
        self.samples.push_back((now, done));
        let keep = t.window.max(t.compare_over);
        while self
            .samples
            .get(1)
            .is_some_and(|(at, _)| now.saturating_duration_since(*at) >= keep)
        {
            self.samples.pop_front();
        }
    }

    pub fn past_grace(&self, now: Instant, t: &Throttling) -> bool {
        now.saturating_duration_since(self.started) >= t.grace
    }

    /// The average speed over the last `over`, from the samples since `from`;
    /// `None` until that much is there.
    fn average(&self, now: Instant, over: Duration, from: Instant) -> Option<f64> {
        let (last_at, last) = *self.samples.back()?;
        // The newest sample at least that old: ticks run late, never early.
        let (first_at, first) = *self
            .samples
            .iter()
            .rev()
            .find(|(at, _)| *at >= from && now.saturating_duration_since(*at) >= over)?;
        let span = last_at.saturating_duration_since(first_at);
        Some(last.saturating_sub(first) as f64 / span.as_secs_f64())
    }

    /// Its speed just now, to compare another transfer with.
    pub fn recent(&self, now: Instant, t: &Throttling) -> Option<f64> {
        self.average(now, t.compare_over, self.started)
    }

    /// Its speed over a whole window lying past the grace.
    pub fn over_window(&self, now: Instant, t: &Throttling) -> Option<f64> {
        self.average(now, t.window, self.started + t.grace)
    }

    /// Notes whether it counts as slow now, and says for how long it has.
    pub fn slow_for(&mut self, now: Instant, slow: bool) -> Duration {
        if !slow {
            self.slow_since = None;
            return Duration::ZERO;
        }
        now.saturating_duration_since(*self.slow_since.get_or_insert(now))
    }
}

/// One transfer as the rules see it.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Reading {
    /// Its average over the window, past the grace.
    pub speed: f64,
    /// Bytes left, when its size is known.
    pub left: Option<u64>,
    /// What its host usually gives ([`crate::engine`]'s `HostStats::normal`).
    pub normal: f64,
}

/// What else tells whether the host is the cause.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Around {
    /// A transfer from another host runs well: the line is fine.
    pub well_elsewhere: bool,
    /// Transfers from other hosts on this exit are slow at the same moment.
    pub slow_elsewhere: bool,
    /// The best the line has shown on another host lately.
    pub line_best: f64,
    /// The user's speed limit and how it is being used.
    pub limit: Option<OwnLimit>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct OwnLimit {
    /// Bytes per second.
    pub rate: f64,
    /// What every transfer together gets now.
    pub total: f64,
    pub running: usize,
}

/// What a transfer's speed says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// Not a whole window past its grace yet.
    TooEarly,
    Fine,
    /// Slow, but done before moving would pay off.
    NearlyDone,
    /// The user's own speed limit explains it.
    OwnLimit,
    /// Other hosts are slow too, which points at the line.
    Line,
    /// Slow with nothing to compare with and no history: watching on.
    Unsure,
    /// The host is limiting it. `sure` once something other than its slowness
    /// shows that.
    Throttled {
        sure: bool,
    },
}

impl Verdict {
    /// Named for the debug view and the row's history.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TooEarly => "too early",
            Self::Fine => "fine",
            Self::NearlyDone => "nearly done",
            Self::OwnLimit => "own speed limit",
            Self::Line => "the line",
            Self::Unsure => "unsure",
            Self::Throttled { sure: true } => "throttled",
            Self::Throttled { sure: false } => "throttled, unsure",
        }
    }
}

/// Whether the reading counts as slow at all, whatever explains it.
pub(crate) fn is_slow(r: &Reading, t: &Throttling) -> bool {
    r.speed < t.slow_for(r.normal) && (r.normal == 0.0 || r.speed < r.normal * t.near_normal)
}

/// What a slow transfer's speed says, given how long it has been slow. The
/// order is the order of `docs/decisions.md`.
pub(crate) fn judge(r: &Reading, around: &Around, slow_for: Duration, t: &Throttling) -> Verdict {
    if !is_slow(r, t) {
        return Verdict::Fine;
    }
    if let Some(left) = r.left
        && r.speed > 0.0
        && left as f64 / r.speed < t.worth_moving.as_secs_f64()
    {
        return Verdict::NearlyDone;
    }
    if let Some(limit) = around.limit {
        let share = limit.rate / limit.running.max(1) as f64;
        if limit.total >= limit.rate * 0.7 || r.speed >= share * 0.5 {
            return Verdict::OwnLimit;
        }
    }
    let line_can = around.line_best >= t.slow && r.speed < around.line_best / t.alone_below;
    if around.well_elsewhere {
        return Verdict::Throttled { sure: true };
    }
    if around.slow_elsewhere {
        return if line_can && slow_for >= t.line_patience {
            Verdict::Throttled { sure: true }
        } else {
            Verdict::Line
        };
    }
    if line_can || (r.normal > 0.0 && r.speed < r.normal / t.alone_below) {
        return Verdict::Throttled { sure: true };
    }
    if slow_for >= t.unsure_after {
        Verdict::Throttled { sure: false }
    } else {
        Verdict::Unsure
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KB: f64 = 1_000.0;
    const MB: f64 = 1_000_000.0;
    const GB: u64 = 1_000_000_000;

    fn verdict(speed: f64, left: Option<u64>, normal: f64, around: Around) -> Verdict {
        let r = Reading {
            speed,
            left,
            normal,
        };
        judge(&r, &around, Duration::ZERO, &Throttling::default())
    }

    fn well() -> Around {
        Around {
            well_elsewhere: true,
            ..Around::default()
        }
    }

    /// The case behind all this: two 4.29 GB parts at 31 and 45 KB/s, beside
    /// another host serving well.
    #[test]
    fn a_big_file_crawling_beside_a_healthy_host_is_throttled() {
        let throttled = Verdict::Throttled { sure: true };
        assert_eq!(verdict(31.0 * KB, Some(4 * GB), 0.0, well()), throttled);
        assert_eq!(verdict(45.0 * KB, Some(4 * GB), 0.0, well()), throttled);
    }

    #[test]
    fn a_stalled_transfer_counts_as_slow() {
        assert_eq!(
            verdict(0.0, Some(4 * GB), 0.0, well()),
            Verdict::Throttled { sure: true }
        );
    }

    #[test]
    fn a_fast_transfer_is_fine() {
        assert_eq!(verdict(2.0 * MB, Some(4 * GB), 0.0, well()), Verdict::Fine);
        // The slow line is just above a mebibyte a second.
        assert_ne!(
            verdict(1_048_576.0, Some(4 * GB), 0.0, well()),
            Verdict::Fine,
            "pixeldrain's own speed for a slowed address"
        );
    }

    #[test]
    fn a_small_file_finishing_soon_is_left_alone() {
        // 12 MB at 200 KB/s: a minute.
        assert_eq!(
            verdict(200.0 * KB, Some(12_000_000), 0.0, well()),
            Verdict::NearlyDone
        );
        // A stall has no end in sight, however little is left.
        assert_eq!(
            verdict(0.0, Some(12_000_000), 0.0, well()),
            Verdict::Throttled { sure: true }
        );
        // No size: no telling it finishes soon.
        assert_eq!(
            verdict(200.0 * KB, None, 0.0, well()),
            Verdict::Throttled { sure: true }
        );
    }

    #[test]
    fn far_below_its_own_normal_counts_above_the_slow_line() {
        // 30 MB/s usually, 2 MB/s now.
        assert_eq!(
            verdict(2.0 * MB, Some(4 * GB), 30.0 * MB, well()),
            Verdict::Throttled { sure: true }
        );
        // Only so far up: 100 MB/s usually and 6 MB/s now is still fast.
        assert_eq!(
            verdict(6.0 * MB, Some(100 * GB), 100.0 * MB, well()),
            Verdict::Fine
        );
    }

    #[test]
    fn near_its_own_normal_is_only_a_slow_host() {
        assert_eq!(
            verdict(1.1 * MB, Some(4 * GB), 1.5 * MB, well()),
            Verdict::Fine
        );
        assert_eq!(
            verdict(600.0 * KB, Some(4 * GB), 1.5 * MB, well()),
            Verdict::Throttled { sure: true }
        );
    }

    #[test]
    fn the_users_own_limit_explains_it() {
        let limited = |rate, total, running| Around {
            limit: Some(OwnLimit {
                rate,
                total,
                running,
            }),
            ..Around::default()
        };
        // Everything at a 500 KB/s limit, taken up.
        assert_eq!(
            verdict(
                250.0 * KB,
                Some(4 * GB),
                0.0,
                limited(500.0 * KB, 500.0 * KB, 2)
            ),
            Verdict::OwnLimit
        );
        // Its fair share, though the limit isn't taken up.
        assert_eq!(
            verdict(
                300.0 * KB,
                Some(4 * GB),
                0.0,
                limited(500.0 * KB, 300.0 * KB, 1)
            ),
            Verdict::OwnLimit
        );
        // A limit far above it explains nothing.
        let far_above = Around {
            well_elsewhere: true,
            ..limited(50.0 * MB, 5.0 * MB, 2)
        };
        assert_eq!(
            verdict(31.0 * KB, Some(4 * GB), 0.0, far_above),
            Verdict::Throttled { sure: true }
        );
    }

    #[test]
    fn others_slow_too_point_at_the_line() {
        let around = Around {
            slow_elsewhere: true,
            line_best: 50.0 * MB,
            ..Around::default()
        };
        let r = Reading {
            speed: 31.0 * KB,
            left: Some(4 * GB),
            normal: 0.0,
        };
        let t = Throttling::default();
        assert_eq!(judge(&r, &around, Duration::ZERO, &t), Verdict::Line);
        // Not for ever, once the line is known to do far better.
        assert_eq!(
            judge(&r, &around, t.line_patience, &t),
            Verdict::Throttled { sure: true }
        );
        let unknown = Around {
            line_best: 0.0,
            ..around
        };
        assert_eq!(judge(&r, &unknown, t.line_patience, &t), Verdict::Line);
    }

    #[test]
    fn alone_it_goes_by_history_and_only_then_is_unsure() {
        let t = Throttling::default();
        let r = Reading {
            speed: 31.0 * KB,
            left: Some(4 * GB),
            normal: 0.0,
        };
        let alone = Around::default();
        assert_eq!(judge(&r, &alone, Duration::ZERO, &t), Verdict::Unsure);
        assert_eq!(
            judge(&r, &alone, t.unsure_after, &t),
            Verdict::Throttled { sure: false }
        );
        // Its own history.
        let known = Reading {
            normal: 10.0 * MB,
            ..r
        };
        assert_eq!(
            judge(&known, &alone, Duration::ZERO, &t),
            Verdict::Throttled { sure: true }
        );
        // What the line did on another host.
        let line = Around {
            line_best: 10.0 * MB,
            ..alone
        };
        assert_eq!(
            judge(&r, &line, Duration::ZERO, &t),
            Verdict::Throttled { sure: true }
        );
        // A line never seen above the slow line proves nothing.
        let poor = Around {
            line_best: 500.0 * KB,
            ..alone
        };
        assert_eq!(judge(&r, &poor, Duration::ZERO, &t), Verdict::Unsure);
    }

    #[test]
    fn cooldowns_double_up_to_a_ceiling() {
        let t = Throttling::default();
        let minutes = |strikes| t.cooldown_for(strikes).as_secs() / 60;
        assert_eq!(minutes(1), 30);
        assert_eq!(minutes(2), 60);
        assert_eq!(minutes(3), 120);
        assert_eq!(minutes(4), 240);
        assert_eq!(minutes(5), 240);
        assert_eq!(minutes(40), 240);
    }

    /// Feeds `pace` a transfer at `speed` for `for_secs`, a sample a second.
    fn feed(pace: &mut Pace, at: &mut Instant, done: &mut u64, speed: u64, for_secs: u64) {
        let t = Throttling::default();
        for _ in 0..for_secs {
            *at += Duration::from_secs(1);
            *done += speed;
            pace.sample(*at, *done, &t);
        }
    }

    #[test]
    fn the_window_starts_after_the_grace() {
        let t = Throttling::default();
        let mut at = Instant::now();
        let mut done = 0;
        let mut pace = Pace::new(at, 0);
        // A burst for the whole grace, then a crawl.
        feed(&mut pace, &mut at, &mut done, 50_000_000, 30);
        assert_eq!(pace.over_window(at, &t), None, "not yet");
        feed(&mut pace, &mut at, &mut done, 30_000, 30);
        assert_eq!(pace.over_window(at, &t), None, "half a window");
        feed(&mut pace, &mut at, &mut done, 30_000, 30);
        let speed = pace.over_window(at, &t).expect("a whole window");
        assert!(
            (speed - 30_000.0).abs() < 1.0,
            "the burst is left out: {speed}"
        );
    }

    #[test]
    fn one_bad_moment_does_not_make_a_window_slow() {
        let t = Throttling::default();
        let mut at = Instant::now();
        let mut done = 0;
        let mut pace = Pace::new(at, 0);
        feed(&mut pace, &mut at, &mut done, 5_000_000, 89);
        feed(&mut pace, &mut at, &mut done, 0, 3);
        let speed = pace.over_window(at, &t).expect("a whole window");
        assert!(speed > t.slow, "{speed}");
    }

    #[test]
    fn a_stall_makes_the_window_zero() {
        let t = Throttling::default();
        let mut at = Instant::now();
        let mut done = 1_000;
        let mut pace = Pace::new(at, done);
        feed(&mut pace, &mut at, &mut done, 0, 95);
        assert_eq!(pace.over_window(at, &t), Some(0.0));
    }

    #[test]
    fn being_slow_counts_up_and_a_good_moment_ends_it() {
        let mut pace = Pace::new(Instant::now(), 0);
        let at = Instant::now();
        assert_eq!(pace.slow_for(at, true), Duration::ZERO);
        let later = at + Duration::from_secs(5);
        assert_eq!(pace.slow_for(later, true), Duration::from_secs(5));
        assert_eq!(pace.slow_for(later, false), Duration::ZERO);
        assert_eq!(pace.slow_for(later, true), Duration::ZERO);
    }

    #[test]
    fn recent_speed_needs_a_compare_worth() {
        let t = Throttling::default();
        let mut at = Instant::now();
        let mut done = 0;
        let mut pace = Pace::new(at, 0);
        feed(&mut pace, &mut at, &mut done, 1_000_000, 5);
        assert_eq!(pace.recent(at, &t), None);
        feed(&mut pace, &mut at, &mut done, 1_000_000, 10);
        let speed = pace.recent(at, &t).expect("ten seconds");
        assert!((speed - 1_000_000.0).abs() < 1.0, "{speed}");
    }
}
