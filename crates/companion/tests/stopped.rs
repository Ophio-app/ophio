// SPDX-License-Identifier: Apache-2.0
//! What a person sees from commands that need Ophio when it is not running.
#![cfg(target_os = "linux")]
use std::{path::Path, process::Command};

fn cli(root: &Path, args: &[&str]) -> (bool, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_ophio"))
        .args(args)
        .env_clear()
        .env("HOME", root)
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env("XDG_RUNTIME_DIR", root.join("runtime"))
        .output()
        .unwrap();
    (
        output.status.success(),
        String::from_utf8(output.stdout).unwrap(),
        String::from_utf8(output.stderr).unwrap(),
    )
}

#[test]
fn commands_that_need_ophio_say_how_to_start_it() {
    let temp = tempfile::tempdir().unwrap();
    for args in [
        &["doctor"][..],
        &["pair"],
        &["pair", "confirm", "123456"],
        &["devices"],
        &["projects", "list"],
        &["stop"],
    ] {
        let (success, stdout, stderr) = cli(temp.path(), args);
        assert!(!success, "{args:?}");
        assert!(stdout.is_empty(), "{args:?}: {stdout}");
        assert_eq!(
            stderr,
            "Øphio is not running. Run `ophio run` to start it here, or `ophio service install` to start it now and whenever you log in.\n",
            "{args:?}"
        );
    }
    let (success, stdout, _) = cli(temp.path(), &["status"]);
    assert!(success);
    assert!(stdout.starts_with("Øphio is not running. Run `ophio run`"));
}
