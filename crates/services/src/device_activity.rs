// SPDX-License-Identifier: Apache-2.0
//! Device observation gaps for access-review presentation. Names do not prove identity.
use protocol::{auth::DeviceRecord, time::Timestamp};
use std::time::Duration;

/// How long an active device must go unseen before the owner is asked whether
/// it is still used. Owners pair a tablet beside a phone, or a new phone the
/// day the old one is put away, so a pairing on its own says nothing about the
/// other devices. A device still in use reconnects within days; two weeks of
/// silence is a real gap rather than a quiet weekend.
pub const REVIEW_AFTER: Duration = Duration::from_secs(14 * 24 * 60 * 60);

/// When an active record was last seen (or paired, if it never connected),
/// once that is at least [`REVIEW_AFTER`] ago and another device has paired
/// since. This is a review hint and must never authorize automatic revocation.
pub fn not_seen_since(
    device: &DeviceRecord,
    devices: &[DeviceRecord],
    now: Timestamp,
) -> Option<Timestamp> {
    if device.revoked_at.is_some() || device.is_current {
        return None;
    }
    let seen = device.last_seen_at.unwrap_or(device.paired_at);
    let quiet = now.0.saturating_sub(seen.0) >= REVIEW_AFTER.as_millis() as i64;
    let replaced = devices
        .iter()
        .any(|d| d.device_id != device.device_id && d.paired_at > seen);
    (quiet && replaced).then_some(seen)
}
