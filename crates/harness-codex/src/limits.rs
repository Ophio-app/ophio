// SPDX-License-Identifier: Apache-2.0
//! The account's rate-limit windows, as `account/rateLimits/updated` reports
//! them. A turn refused with `usageLimitExceeded` carries no reset time of
//! its own; the exhausted windows say when usage returns.
use protocol::time::Timestamp;
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq)]
struct Window {
    used_percent: f64,
    /// Epoch seconds.
    resets_at: Option<i64>,
}

#[derive(Debug, Default)]
pub(crate) struct RateWindows {
    primary: Option<Window>,
    secondary: Option<Window>,
}

impl RateWindows {
    /// Updates are sparse: an absent or null window keeps the last one seen.
    pub(crate) fn update(&mut self, snapshot: &Value) {
        for (slot, key) in [
            (&mut self.primary, "primary"),
            (&mut self.secondary, "secondary"),
        ] {
            if let Some(used_percent) = snapshot[key]["usedPercent"].as_f64() {
                *slot = Some(Window {
                    used_percent,
                    resets_at: snapshot[key]["resetsAt"].as_i64(),
                });
            }
        }
    }
    /// Usage returns once every exhausted window has reset.
    pub(crate) fn resets_at(&self) -> Option<Timestamp> {
        [self.primary, self.secondary]
            .into_iter()
            .flatten()
            .filter(|w| w.used_percent >= 100.0)
            .filter_map(|w| w.resets_at)
            .max()
            .and_then(Timestamp::from_unix_seconds)
    }
}

/// The app server's own code for a turn refused because the account is out
/// of usage, on an `error` notification or a failed turn.
pub(crate) fn usage_limit_exceeded(error: &Value) -> bool {
    error["codexErrorInfo"] == "usageLimitExceeded"
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_latest_exhausted_window_says_when_usage_returns() {
        let mut windows = RateWindows::default();
        windows.update(&json!({"primary": {"usedPercent": 40, "resetsAt": 1_790_000_000}}));
        assert_eq!(windows.resets_at(), None);
        windows.update(&json!({
            "primary": {"usedPercent": 100, "resetsAt": 1_790_000_000},
            "secondary": {"usedPercent": 100, "resetsAt": 1_790_500_000}
        }));
        // A sparse update leaves the other window as it was.
        windows.update(
            &json!({"primary": null, "secondary": {"usedPercent": 100, "resetsAt": 1_790_400_000}}),
        );
        assert_eq!(windows.resets_at(), Some(Timestamp(1_790_400_000_000)));
        // A reset time beyond the wire's integer range is not known.
        windows.update(&json!({"secondary": {"usedPercent": 100, "resetsAt": i64::MAX}}));
        assert_eq!(windows.resets_at(), None);
        assert!(usage_limit_exceeded(
            &json!({"message": "You've hit your usage limit.", "codexErrorInfo": "usageLimitExceeded"})
        ));
        assert!(!usage_limit_exceeded(
            &json!({"message": "Slow down", "codexErrorInfo": "rateLimitExceeded"})
        ));
    }
}
