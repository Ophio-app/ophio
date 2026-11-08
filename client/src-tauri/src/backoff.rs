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
/// How long a connection has to last before the next one starts from the
/// shortest wait again. One the computer accepts and then ends at once is a
/// failure like any other, so its retries wait longer each time.
pub const STABLE: Duration = Duration::from_secs(10);
/// Who asked the link to try again now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Ask {
    /// The page, for no reason it says.
    Page,
    /// The app came back to the screen, or the network came back.
    Event,
    /// The person tapped Retry.
    Person,
}
impl Ask {
    pub fn named(name: Option<&str>) -> Self {
        match name {
            Some("person") => Self::Person,
            Some("event") => Self::Event,
            _ => Self::Page,
        }
    }
    /// How soon after the last request that ended a wait this one may.
    fn spacing(self) -> Duration {
        match self {
            Self::Person => Duration::ZERO,
            Self::Event => Duration::from_secs(1),
            Self::Page => Duration::from_secs(5),
        }
    }
}
/// Requests to try now. One ends the wait at once unless another did too
/// recently for its kind; then it is dropped, not held, and the wait runs
/// its course, so a page asking on every failure can't turn the waits into a
/// storm or flatten their doubling.
#[derive(Debug, Default)]
pub struct Asks {
    last: Option<Instant>,
}
impl Asks {
    /// Whether `ask`, made at `now`, ends the wait.
    pub fn take(&mut self, ask: Ask, now: Instant) -> bool {
        let soon = self
            .last
            .is_some_and(|last| now.saturating_duration_since(last) < ask.spacing());
        if !soon {
            self.last = Some(now);
        }
        !soon
    }
}

impl Backoff {
    pub fn reset(&mut self) {
        self.failures = 0;
    }
    /// A connection ended after lasting `lasted`.
    pub fn ended_after(&mut self, lasted: Duration) {
        if lasted >= STABLE {
            self.reset();
        }
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
    fn a_page_asking_on_every_failure_is_dropped_but_the_person_is_not() {
        let start = Instant::now();
        let at = |s: f64| start + Duration::from_secs_f64(s);
        let mut asks = Asks::default();
        // The first ends the wait; the page's next ones within 5 s are
        // dropped, and dropping one doesn't push the next turn later.
        assert!(asks.take(Ask::Page, at(0.0)));
        assert!(!asks.take(Ask::Page, at(0.1)));
        assert!(!asks.take(Ask::Page, at(4.9)));
        assert!(asks.take(Ask::Page, at(5.0)));
        // Coming back to the app or the network waits a second at most.
        assert!(!asks.take(Ask::Event, at(5.5)));
        assert!(asks.take(Ask::Event, at(6.0)));
        // The person's Retry always goes through.
        assert!(asks.take(Ask::Person, at(6.0)));
        assert!(asks.take(Ask::Person, at(6.01)));
        assert_eq!(Ask::named(Some("person")), Ask::Person);
        assert_eq!(Ask::named(Some("event")), Ask::Event);
        assert_eq!(Ask::named(None), Ask::Page);
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
        // A connection that ended at once doesn't start the waits over.
        b.next(&clock, 0);
        b.ended_after(Duration::from_millis(20));
        assert_eq!(
            b.next(&clock, 0).at.duration_since(clock.mono).as_millis(),
            6400
        );
        b.ended_after(STABLE);
        clock.wall = -1000;
        assert_eq!(
            b.next(&clock, u16::MAX).at.duration_since(clock.mono),
            Duration::from_millis(4800)
        );
    }
}
