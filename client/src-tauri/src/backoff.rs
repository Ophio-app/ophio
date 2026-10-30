// SPDX-License-Identifier: Apache-2.0
use ports::Clock;
use std::time::{Duration, Instant};

#[derive(Debug, Default)]
pub struct Backoff {
    failures: u32,
}
#[derive(Debug)]
pub struct Retry {
    pub at: Instant,
    pub wall_ms: i64,
}
impl Backoff {
    pub fn reset(&mut self) {
        self.failures = 0;
    }
    pub fn next(&mut self, clock: &dyn Clock, entropy: u16) -> Retry {
        // Four seconds is the simulated link's reference interval. ±20% jitter
        // avoids synchronized reconnects; the total delay is capped at 30s.
        let base = 4000_u64.saturating_mul(1_u64 << self.failures.min(4));
        self.failures = self.failures.saturating_add(1);
        let delay = (base * (800 + u64::from(entropy) * 400 / 65535) / 1000).min(30000);
        Retry {
            at: clock.monotonic() + Duration::from_millis(delay),
            wall_ms: clock.now().0.saturating_add(delay as i64),
        }
    }
}
pub async fn wait(clock: &dyn Clock, end: Instant) {
    loop {
        let left = end.saturating_duration_since(clock.monotonic());
        if left.is_zero() {
            return;
        }
        tokio::time::sleep(left.min(Duration::from_millis(100))).await;
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Debug)]
    struct Fake {
        mono: Instant,
        wall: i64,
    }
    impl Clock for Fake {
        fn now(&self) -> protocol::time::Timestamp {
            protocol::time::Timestamp(self.wall)
        }
        fn monotonic(&self) -> Instant {
            self.mono
        }
    }
    #[test]
    fn exponential_jitter_cap_reset_and_wall_clock_independence() {
        let mut clock = Fake {
            mono: Instant::now(),
            wall: 100000,
        };
        let mut b = Backoff::default();
        for ms in [3200, 6400, 12800, 25600, 30000, 30000] {
            let retry = b.next(&clock, 0);
            assert_eq!(retry.at.duration_since(clock.mono).as_millis(), ms);
            assert_eq!(retry.wall_ms, clock.wall + ms as i64);
        }
        b.reset();
        clock.wall = -1000;
        assert_eq!(
            b.next(&clock, u16::MAX).at.duration_since(clock.mono),
            Duration::from_millis(4800)
        );
    }
}
