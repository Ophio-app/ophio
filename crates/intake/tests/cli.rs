// SPDX-License-Identifier: Apache-2.0
use std::{path::Path, process::Command};

fn admin(data: &Path, args: &[&str]) -> serde_json::Value {
    let output = Command::new(env!("CARGO_BIN_EXE_ophio-intake"))
        .arg("admin")
        .arg("--data")
        .arg(data)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "admin failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}
#[test]
fn cli_initializes_and_administers_the_database_without_listeners() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let output = Command::new(env!("CARGO_BIN_EXE_ophio-intake"))
        .arg("init")
        .arg("--data")
        .arg(&data)
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(data.join("maintainer.token").is_file());
    admin(&data, &["pause", "true"]);
    admin(&data, &["close", "true"]);
    let state = admin(&data, &["switches"]);
    assert_eq!(state["pipeline_paused"], true);
    assert_eq!(state["intake_closed"], true);
    admin(&data, &["pause", "false"]);
    admin(&data, &["close", "false"]);
    assert!(
        admin(&data, &["list"])["reports"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(admin(&data, &["sweep"])["result"], "swept");
    assert_eq!(admin(&data, &["counters"])["audit_rows"], 5);
}
