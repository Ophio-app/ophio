// SPDX-License-Identifier: Apache-2.0
//! A command whose output reader goes away, as in `ophio update | head -4`,
//! stops quietly. In one problem report, four such stops were recorded as
//! crashes, and the phone then said "Ophio closed unexpectedly" the next
//! time it opened, long after the command had ended.
#![cfg(unix)]

use std::{path::Path, process::Command};

fn crashes(home: &Path) -> Vec<serde_json::Value> {
    let mut found = Vec::new();
    for data in [home.join(".local/share/ophio"), home.join("data/ophio")] {
        if let Ok(text) = std::fs::read_to_string(data.join("report-crashes.json")) {
            found.extend(serde_json::from_str::<Vec<serde_json::Value>>(&text).unwrap());
        }
    }
    found
}

#[test]
fn a_closed_output_pipe_is_not_recorded_as_a_crash() {
    let home = tempfile::tempdir().unwrap();
    let (reader, writer) = std::io::pipe().unwrap();
    // The reader has read what it wanted and gone, before Ophio prints.
    drop(reader);
    let output = Command::new(env!("CARGO_BIN_EXE_ophio"))
        .arg("update")
        .env_clear()
        .env("HOME", home.path())
        .env("PATH", home.path().join("empty-path"))
        .stdout(writer)
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        crashes(home.path()).is_empty(),
        "recorded a crash: {:?}",
        crashes(home.path())
    );
    assert!(!stderr.contains("panicked"), "{stderr}");
    // It doesn't stop either: an update or install whose reader went away
    // finishes its work rather than stopping halfway.
    assert!(output.status.success(), "{output:?}");
}
