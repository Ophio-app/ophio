// SPDX-License-Identifier: Apache-2.0
//! Time source, injectable for deterministic tests.

use std::time::Instant;

use protocol::time::Timestamp;

pub trait Clock: Send + Sync + std::fmt::Debug + 'static {
    /// Wall-clock time for records shown to people.
    fn now(&self) -> Timestamp;
    /// Monotonic time for deadlines (lease expiry, timeouts).
    fn monotonic(&self) -> Instant;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Timestamp {
        Timestamp::now()
    }

    fn monotonic(&self) -> Instant {
        Instant::now()
    }
}
