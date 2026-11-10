//! One token bucket shared by every transfer, so a single speed cap covers all
//! parallel streams.

use std::{
    sync::Mutex,
    time::{Duration, Instant},
};

pub(crate) struct Limiter {
    /// Bytes per second (0 = unlimited) and when the bucket is next free.
    state: Mutex<(f64, Instant)>,
}

impl Limiter {
    pub fn new(rate: f64) -> Self {
        Self {
            state: Mutex::new((rate.max(0.0), Instant::now())),
        }
    }

    pub fn rate(&self) -> f64 {
        crate::lock::held(&self.state).0
    }

    pub fn set_rate(&self, rate: f64) {
        *crate::lock::held(&self.state) = (rate.max(0.0), Instant::now());
    }

    /// Waits until `n` more bytes fit under the cap.
    pub async fn throttle(&self, n: usize) {
        let delay = {
            let mut s = crate::lock::held(&self.state);
            if s.0 <= 0.0 {
                return;
            }
            let now = Instant::now();
            s.1 = s.1.max(now) + Duration::from_secs_f64(n as f64 / s.0);
            s.1 - now
        };
        if delay > Duration::from_millis(50) {
            tokio::time::sleep(delay).await;
        }
    }
}
