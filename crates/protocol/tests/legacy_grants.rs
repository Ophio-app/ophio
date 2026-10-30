// SPDX-License-Identifier: Apache-2.0
use protocol::{Command, auth::Grants};
use ts_rs::TS;

#[test]
fn stored_grants_allow_a_missing_baseline_but_the_phone_type_requires_one() {
    let body = serde_json::json!({
        "command": "set_device_grants",
        "device_id": "00000000-0000-4000-8000-000000000001",
        "grants": Grants::default(),
    });
    assert!(matches!(
        serde_json::from_value::<Command>(body).unwrap(),
        Command::SetDeviceGrants { expected: None, .. }
    ));
    let ts = Command::decl(&ts_rs::Config::default());
    assert!(ts.contains("expected: Grants"), "{ts}");
    assert!(!ts.contains("expected?:"));
}
