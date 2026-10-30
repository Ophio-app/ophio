// SPDX-License-Identifier: Apache-2.0
#![cfg(target_os = "linux")]
use std::{
    path::Path,
    process::{Command, Output},
};
fn cli(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ophio"))
        .args(args)
        .env_clear()
        .env("HOME", root)
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env("XDG_RUNTIME_DIR", root.join("runtime"))
        .output()
        .unwrap()
}
fn ok(root: &Path, args: &[&str]) -> String {
    let result = cli(root, args);
    assert!(
        result.status.success(),
        "{:?}: {}",
        args,
        String::from_utf8_lossy(&result.stderr)
    );
    String::from_utf8(result.stdout).unwrap()
}
#[test]
fn default_downloads_config_status_and_uninstall_keep_received_files() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    std::fs::create_dir_all(root.join("config")).unwrap();
    // Missing Downloads is an unconfigured inbox, not a CLI/startup error.
    assert!(ok(root, &["inbox"]).contains("Set one with `ophio inbox set <PATH>`"));
    std::fs::create_dir(root.join("Downloads")).unwrap();
    let fallback = root.join("Downloads/ophio");
    assert!(ok(root, &["inbox"]).contains(fallback.to_str().unwrap()));
    assert!(!fallback.exists());
    std::fs::write(
        root.join("config/user-dirs.dirs"),
        "XDG_DOWNLOAD_DIR=\"$HOME/My Downloads\"\n",
    )
    .unwrap();
    std::fs::create_dir(root.join("My Downloads")).unwrap();
    assert!(ok(root, &["inbox"]).contains(root.join("My Downloads/ophio").to_str().unwrap()));
    ok(root, &["retention", "set", "14"]);
    let saved = companion::config::Config::load(&root.join("config/ophio/config.toml")).unwrap();
    assert!(saved.inbox_path.is_none());
    // A settings save must not pin the old Downloads location.
    std::fs::write(
        root.join("config/user-dirs.dirs"),
        "XDG_DOWNLOAD_DIR=\"$HOME/Downloads\"\n",
    )
    .unwrap();
    assert!(ok(root, &["inbox"]).contains(fallback.to_str().unwrap()));
    let received = root.join("Received files");
    ok(root, &["inbox", "set", received.to_str().unwrap()]);
    assert!(!received.exists());
    assert!(ok(root, &["inbox"]).contains(received.to_str().unwrap()));
    assert!(ok(root, &["status"]).contains(received.to_str().unwrap()));
    let bad = root.join("data/ophio/nested/inbox");
    assert!(
        !cli(root, &["inbox", "set", bad.to_str().unwrap()])
            .status
            .success()
    );
    assert!(!bad.exists());
    std::fs::create_dir_all(&received).unwrap();
    std::fs::write(received.join("new.txt"), "kept").unwrap();
    let legacy = root.join("data/ophio/db/inbox");
    std::fs::create_dir_all(&legacy).unwrap();
    std::fs::write(legacy.join("old.txt"), "also kept").unwrap();
    std::fs::write(root.join("data/ophio/db/companion.sqlite"), "owned").unwrap();
    assert!(ok(root, &["status"]).contains(legacy.to_str().unwrap()));
    let output = ok(root, &["uninstall-data", "--yes"]);
    assert!(output.contains(received.to_str().unwrap()));
    assert!(output.contains(legacy.to_str().unwrap()));
    assert!(output.contains("kept"));
    assert_eq!(
        std::fs::read_to_string(received.join("new.txt")).unwrap(),
        "kept"
    );
    assert_eq!(
        std::fs::read_to_string(legacy.join("old.txt")).unwrap(),
        "also kept"
    );
    assert!(!root.join("data/ophio/db/companion.sqlite").exists());
}
#[test]
fn uninstall_names_received_files_only_when_there_are_some() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    std::fs::create_dir_all(root.join("config")).unwrap();
    std::fs::create_dir(root.join("Downloads")).unwrap();
    ok(root, &["retention", "set", "14"]);
    let output = ok(root, &["uninstall-data", "--yes"]);
    assert!(!output.contains("Received files"), "{output}");
    assert!(!root.join("Downloads/ophio").exists());
}
