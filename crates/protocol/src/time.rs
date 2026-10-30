// SPDX-License-Identifier: Apache-2.0
//! Wall-clock timestamps on the wire are Unix epoch milliseconds (UTC). They are
//! observations for display and ordering hints only; safety deadlines use the
//! owning process's monotonic clock and are never computed from these values.

use serde::{Deserialize, Serialize};

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
    ts_rs::TS,
)]
#[serde(transparent)]
#[ts(export, type = "number")]
pub struct Timestamp(pub i64);

impl Timestamp {
    pub fn now() -> Self {
        let ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        Self(ms)
    }

    pub fn millis(&self) -> i64 {
        self.0
    }

    /// A time another program reported in Unix epoch seconds, or `None` when
    /// its milliseconds would leave the wire's integer range.
    pub fn from_unix_seconds(seconds: i64) -> Option<Self> {
        seconds
            .checked_mul(1000)
            .filter(|ms| ms.unsigned_abs() <= crate::MAX_SAFE_INTEGER)
            .map(Self)
    }
}
