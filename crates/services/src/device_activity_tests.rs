// SPDX-License-Identifier: Apache-2.0
use super::*;
use crate::device_activity::{REVIEW_AFTER, not_seen_since};
use ports::Clock;

async fn records(f: &Fixture) -> Vec<DeviceRecord> {
    match f
        .hub
        .query(&Caller::local_owner(), Query::Devices)
        .await
        .unwrap()
    {
        QueryData::Devices { devices, .. } => devices,
        _ => panic!("devices expected"),
    }
}

fn find(devices: &[DeviceRecord], id: &DeviceId) -> DeviceRecord {
    devices.iter().find(|d| &d.device_id == id).unwrap().clone()
}

#[tokio::test]
async fn a_device_paired_minutes_after_another_does_not_flag_it() {
    let f = Fixture::new().await;
    let Principal::Device { device_id: phone } = pair(&f.hub, 1).await.principal else {
        panic!("device expected")
    };
    f.hub.device_seen(&phone).await;
    let seen = f.clock.now();
    f.clock.advance(Duration::from_secs(5 * 60));
    pair(&f.hub, 2).await;
    let devices = records(&f).await;
    assert_eq!(devices.len(), 2);
    assert!(
        devices
            .iter()
            .all(|d| d.name == "Phone" && d.revoked_at.is_none())
    );
    assert_eq!(
        not_seen_since(&find(&devices, &phone), &devices, f.clock.now()),
        None
    );
    // Still unseen a day short of the threshold: no hint yet.
    f.clock
        .advance(REVIEW_AFTER - Duration::from_secs(5 * 60 + 24 * 60 * 60));
    assert_eq!(
        not_seen_since(&find(&devices, &phone), &devices, f.clock.now()),
        None
    );
    f.clock.advance(Duration::from_secs(24 * 60 * 60));
    assert_eq!(
        not_seen_since(&find(&devices, &phone), &devices, f.clock.now()),
        Some(seen)
    );
    f.close().await;
}

#[tokio::test]
async fn a_long_silence_alone_is_not_a_reason_to_review() {
    let f = Fixture::new().await;
    pair(&f.hub, 1).await;
    let Principal::Device { device_id: tablet } = pair(&f.hub, 2).await.principal else {
        panic!("device expected")
    };
    f.hub.device_seen(&tablet).await;
    f.clock.advance(REVIEW_AFTER * 3);
    let devices = records(&f).await;
    // Nothing paired after the tablet was last seen.
    assert_eq!(
        not_seen_since(&find(&devices, &tablet), &devices, f.clock.now()),
        None
    );
    f.close().await;
}

#[tokio::test]
async fn an_old_record_unseen_since_a_later_pairing_is_offered_for_review() {
    let f = Fixture::new().await;
    let first = pair(&f.hub, 1).await;
    let Principal::Device { device_id } = first.principal else {
        panic!("device expected")
    };
    let paired_at = f.clock.now();
    f.clock.advance(Duration::from_secs(1));
    pair(&f.hub, 2).await;
    f.clock.advance(REVIEW_AFTER);
    let now = f.clock.now();
    let devices = records(&f).await;
    let old = find(&devices, &device_id);
    assert_eq!(old.grants, first.grants);
    // Never connected: measured from its own pairing.
    assert_eq!(not_seen_since(&old, &devices, now), Some(paired_at));
    let newer = devices.iter().find(|d| d.device_id != device_id).unwrap();
    assert_eq!(not_seen_since(newer, &devices, now), None);
    let mut current = old.clone();
    current.is_current = true;
    assert_eq!(not_seen_since(&current, &devices, now), None);
    current.is_current = false;
    current.revoked_at = Some(now);
    assert_eq!(not_seen_since(&current, &devices, now), None);
    // An authentication after the later pairing clears the review hint.
    f.hub.device_seen(&device_id).await;
    let devices = records(&f).await;
    assert_eq!(
        not_seen_since(&find(&devices, &device_id), &devices, f.clock.now()),
        None
    );
    f.close().await;
}
