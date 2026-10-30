// SPDX-License-Identifier: Apache-2.0
#![cfg(target_os = "linux")]

use serde_json::json;
use std::{fs, os::unix::fs::MetadataExt, path::Path, process::Command};

fn identity(path: &Path) -> serde_json::Value {
    let metadata = fs::metadata(path).unwrap();
    json!({"device": metadata.dev(), "inode": metadata.ino()})
}

/// Exercise main(), including early --version / --help exits, rather than
/// calling the recovery function ourselves. The unit subprocess tests create
/// interrupted states by killing the actual replacement code at each move.
#[test]
fn installed_cli_recovers_before_command_parsing() {
    check_installed_cli(false);
}

#[test]
fn installed_cli_recovers_under_symlinked_home() {
    check_installed_cli(true);
}

fn check_installed_cli(symlinked_home: bool) {
    for programs_last in [false, true] {
        for service in [false, true] {
            let home = tempfile::tempdir().unwrap();
            let startup_home = if symlinked_home {
                let link = home.path().join("home-link");
                std::os::unix::fs::symlink(home.path(), &link).unwrap();
                link
            } else {
                home.path().to_owned()
            };
            let opt = home.path().join(".local/opt/ophio");
            let replacement = opt.join("replacement");
            for root in [&opt, &replacement] {
                // Match the installer's owner-only directories regardless of
                // the test runner's umask (CI commonly uses 022).
                companion::identity::private_dir(&root.join("bin")).unwrap();
                companion::identity::private_dir(&root.join("app")).unwrap();
            }
            // The build cache need not share a filesystem with this home.
            fs::copy(env!("CARGO_BIN_EXE_ophio"), opt.join("bin/ophio")).unwrap();
            // A new inode forces recovery to execute the installed version.
            fs::copy(env!("CARGO_BIN_EXE_ophio"), replacement.join("bin/ophio")).unwrap();
            fs::write(opt.join("app/ophio.apk"), "old").unwrap();
            fs::write(opt.join("installed.json"), r#"{"version":"old"}"#).unwrap();
            fs::write(replacement.join("app/ophio.apk"), "new").unwrap();
            fs::write(replacement.join("installed.json"), r#"{"version":"new"}"#).unwrap();
            let new_bin = identity(&replacement.join("bin"));
            let journal = json!({
                "bin": new_bin,
                "app": identity(&replacement.join("app")),
                "installed": identity(&replacement.join("installed.json")),
                "previous": null,
                "programs_last": programs_last,
            });
            fs::write(opt.join("replacement.json"), journal.to_string()).unwrap();
            let links = home.path().join(".local/bin");
            fs::create_dir_all(&links).unwrap();
            let command = links.join("ophio");
            std::os::unix::fs::symlink(opt.join("bin/ophio"), &command).unwrap();
            let entry = if service {
                opt.join("bin/ophio")
            } else {
                command
            };
            // The service path is exercised with help, without starting a
            // companion or touching the computer's actual systemd service.
            let args = if service {
                vec!["run", "--help"]
            } else {
                vec!["--version"]
            };
            for _ in 0..2 {
                let output = Command::new(&entry)
                    .args(&args)
                    .env_clear()
                    .env("HOME", &startup_home)
                    .env("PATH", home.path().join("empty-path"))
                    .output()
                    .unwrap();
                assert!(output.status.success(), "{output:?}");
                assert_eq!(identity(&opt.join("bin")), new_bin);
                assert_eq!(
                    fs::read_to_string(opt.join("app/ophio.apk")).unwrap(),
                    "new"
                );
                assert_eq!(
                    fs::read_to_string(opt.join("installed.json")).unwrap(),
                    r#"{"version":"new"}"#
                );
                assert!(!opt.join("replacement.json").exists());
                assert!(!replacement.exists());
            }
        }
    }
}
