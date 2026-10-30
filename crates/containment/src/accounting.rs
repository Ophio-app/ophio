// SPDX-License-Identifier: Apache-2.0
//! Confirmation that a job has emptied, from its accounting snapshots.
use crate::{END_WITHIN, unconfirmed};
use ports::HarnessError;
use std::time::Duration;

/// An empty, never-assigned job cannot confirm a run. Query errors are kept by
/// the caller; only a successful accounting snapshot is accepted here.
fn empty(total: u32, active: u32) -> Result<bool, HarnessError> {
    if total == 0 || active > total {
        return Err(unconfirmed(
            "Native job membership could not be established",
        ));
    }
    Ok(active == 0)
}

pub(crate) async fn confirm(
    clock: &dyn ports::Clock,
    mut accounting: impl FnMut() -> Result<(u32, u32), HarnessError>,
) -> Result<(), HarnessError> {
    let start = clock.monotonic();
    loop {
        let (total, active) = accounting()?;
        if clock.monotonic().saturating_duration_since(start) >= END_WITHIN {
            return Err(unconfirmed(
                "Native process-tree exit confirmation timed out",
            ));
        }
        if empty(total, active)? {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        sync::atomic::{AtomicU64, Ordering},
        time::Instant,
    };

    #[derive(Debug)]
    struct Clock {
        start: Instant,
        milliseconds: AtomicU64,
    }
    impl Clock {
        fn new() -> Self {
            Self {
                start: Instant::now(),
                milliseconds: AtomicU64::new(0),
            }
        }
        fn advance(&self, ms: u64) {
            self.milliseconds.fetch_add(ms, Ordering::SeqCst);
        }
    }
    impl ports::Clock for Clock {
        fn now(&self) -> protocol::time::Timestamp {
            protocol::time::Timestamp(0)
        }
        fn monotonic(&self) -> Instant {
            self.start + Duration::from_millis(self.milliseconds.load(Ordering::SeqCst))
        }
    }

    #[test]
    fn failed_assignment_and_inconsistent_accounting_never_confirm_an_end() {
        assert!(empty(0, 0).is_err());
        assert!(empty(1, 2).is_err());
        assert!(!empty(2, 1).unwrap());
        assert!(empty(2, 0).unwrap());
    }

    #[tokio::test(start_paused = true)]
    async fn confirmation_waits_for_every_descendant() {
        let clock = Clock::new();
        let mut remaining = [2, 1, 0].into_iter();
        confirm(&clock, || {
            clock.advance(20);
            Ok((3, remaining.next().unwrap()))
        })
        .await
        .unwrap();
        assert_eq!(remaining.next(), None);
    }

    #[tokio::test(start_paused = true)]
    async fn bound_exceeded_is_uncertain_even_if_last_snapshot_is_empty() {
        for active in [0, 1] {
            let clock = Clock::new();
            let result = confirm(&clock, || {
                clock.advance(3000);
                Ok((1, active))
            })
            .await;
            assert!(
                matches!(result, Err(HarnessError::Protocol(ref s)) if s.contains("timed out") && s.contains("not confirmed"))
            );
        }
    }

    #[tokio::test]
    async fn accounting_failure_is_not_an_empty_job() {
        let result = confirm(&Clock::new(), || {
            Err(unconfirmed("Accounting query failed"))
        })
        .await;
        assert!(
            matches!(result, Err(HarnessError::Protocol(ref s)) if s.contains("Accounting query failed"))
        );
    }
}
