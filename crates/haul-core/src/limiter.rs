//! One token bucket shared by every transfer, so a single speed cap covers all
//! parallel streams.

use std::{sync::Mutex, time::Duration};

use tokio::time::Instant;

/// How often a wait looks at the cap again, so a cap that was lifted or
/// changed lets go of the transfers waiting under the old one.
const LOOK_AGAIN: Duration = Duration::from_millis(250);

pub(crate) struct Limiter {
    state: Mutex<Bucket>,
}

struct Bucket {
    /// Bytes per second; 0 is no cap.
    rate: f64,
    /// When the bucket is next free.
    free_at: Instant,
    /// Counts the changes of the cap.
    changes: u64,
}

impl Limiter {
    pub fn new(rate: f64) -> Self {
        Self {
            state: Mutex::new(Bucket {
                rate: rate.max(0.0),
                free_at: Instant::now(),
                changes: 0,
            }),
        }
    }

    pub fn rate(&self) -> f64 {
        crate::lock::held(&self.state).rate
    }

    pub fn set_rate(&self, rate: f64) {
        let mut b = crate::lock::held(&self.state);
        b.rate = rate.max(0.0);
        b.free_at = Instant::now();
        b.changes += 1;
    }

    /// Waits until `n` more bytes fit under the cap. A wait the cap changes
    /// during ends at its next look: the next bytes are counted under the new
    /// cap.
    pub async fn throttle(&self, n: usize) {
        let (until, changes) = {
            let mut b = crate::lock::held(&self.state);
            if b.rate <= 0.0 {
                return;
            }
            let now = Instant::now();
            b.free_at = b.free_at.max(now) + Duration::from_secs_f64(n as f64 / b.rate);
            (b.free_at, b.changes)
        };
        loop {
            let left = until.saturating_duration_since(Instant::now());
            if left <= Duration::from_millis(50) {
                return;
            }
            tokio::time::sleep(left.min(LOOK_AGAIN)).await;
            if crate::lock::held(&self.state).changes != changes {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A cap lifted while a transfer waits under it lets the transfer go
    /// within a look, not when the old cap would have.
    #[tokio::test(start_paused = true)]
    async fn a_lifted_cap_lets_go_at_once() {
        let limiter = std::sync::Arc::new(Limiter::new(100.0));
        let waiting = tokio::spawn({
            let limiter = limiter.clone();
            async move { limiter.throttle(10_000).await }
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(!waiting.is_finished(), "held under the cap");
        limiter.set_rate(0.0);
        tokio::time::sleep(LOOK_AGAIN + Duration::from_millis(50)).await;
        assert!(waiting.is_finished(), "let go within a look");
    }
}
