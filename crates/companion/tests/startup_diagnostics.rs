// SPDX-License-Identifier: Apache-2.0
// Separate process verifies that logging starts BEFORE application recovery and
// that the same safe cause is printed by the actual `ophio run` binary.
#![cfg(target_os = "linux")]

#[test]
fn failed_recovery_prints_and_logs_the_cause_without_stored_secrets() {
    use std::os::unix::fs::DirBuilderExt;
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data/ophio");
    // The companion refuses a data directory others can read; the store would
    // create this one following the umask.
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&data)
        .unwrap();
    let mut db = store::Store::open(
        data.join("db/companion.sqlite"),
        store::Retention::default(),
    )
    .unwrap();
    db.write(protocol::time::Timestamp(0), |tx| {
        tx.put(
            "approval",
            "synthetic",
            &serde_json::json!({"private": "synthetic-secret-token"}),
        )
    })
    .unwrap();
    drop(db);
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_ophio"))
        .arg("run")
        .env_clear()
        .env("PATH", dir.path().join("empty-path"))
        .env("XDG_CONFIG_HOME", dir.path().join("config"))
        .env("XDG_DATA_HOME", dir.path().join("data"))
        .env("XDG_RUNTIME_DIR", dir.path().join("runtime"))
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("missing field `approval_id`"), "{stderr}");
    assert!(!stderr.contains("synthetic-secret-token"));
    let logs: String = std::fs::read_dir(data.join("logs"))
        .unwrap()
        .map(|entry| std::fs::read_to_string(entry.unwrap().path()).unwrap())
        .collect();
    assert!(logs.contains("missing field `approval_id`"), "{logs}");
    assert!(!logs.contains("synthetic-secret-token"));
}
