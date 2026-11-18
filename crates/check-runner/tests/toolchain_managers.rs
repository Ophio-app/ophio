// SPDX-License-Identifier: Apache-2.0
#![cfg(unix)]

use check_runner::{
    runner::{self, CheckOutcome, CheckRequest, RunnerOptions},
    toolchains::readable_toolchains,
};
use std::{fs, os::unix::fs::PermissionsExt, process::Command, time::Duration};

// Use subprocesses instead of mutating HOME/PATH in a multithreaded test process.
#[test]
#[cfg_attr(
    target_os = "linux",
    ignore = "Requires live bubblewrap outside the agent namespace"
)]
fn nvm_tools_run_with_only_the_service_path() {
    toolchain_fixture("project-pin");
}

#[test]
#[cfg_attr(
    target_os = "linux",
    ignore = "Requires live bubblewrap outside the agent namespace"
)]
fn nvm_global_default_is_used_without_exposing_its_file() {
    toolchain_fixture("global-default");
}

fn toolchain_fixture(mode: &str) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let home = root.join("home");
    fs::create_dir(&home).unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "service_path_toolchain_fixture", "--nocapture"])
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("HOME", home)
        .env("OPHIO_TOOLCHAIN_MANAGER_FIXTURE", mode)
        .env("OPHIO_TOOLCHAIN_FIXTURE_ROOT", root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "toolchain fixture failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test]
async fn service_path_toolchain_fixture() {
    let Ok(mode) = std::env::var("OPHIO_TOOLCHAIN_MANAGER_FIXTURE") else {
        return;
    };
    assert_eq!(
        std::env::var("PATH").unwrap(),
        "/usr/bin:/bin:/usr/sbin:/sbin"
    );
    let root = std::path::PathBuf::from(std::env::var_os("OPHIO_TOOLCHAIN_FIXTURE_ROOT").unwrap());
    let home = root.join("home");
    let project = root.join("project");
    fs::create_dir_all(&project).unwrap();
    let default = home.join(".nvm/alias/default");
    fs::create_dir_all(default.parent().unwrap()).unwrap();
    fs::write(&default, "18.20.5\n").unwrap();
    let selected = if mode == "project-pin" {
        fs::write(project.join(".nvmrc"), "22.14.0\n").unwrap();
        "22.14.0"
    } else {
        assert_eq!(mode, "global-default");
        "18.20.5"
    };
    // npm invokes node by name, as a real installation does. Its attempt to
    // read the host default must fail even though Ophio used that default.
    let quoted_default = format!("'{}'", default.to_str().unwrap().replace('\'', "'\\''"));
    for version in ["18.20.5", "22.14.0"] {
        let bin = home.join(format!(".nvm/versions/node/v{version}/bin"));
        fs::create_dir_all(&bin).unwrap();
        for (name, script) in [
            (
                "node",
                format!(
                    "#!/bin/sh\n[ \"$1\" = '--version' ] || exit 9\nprintf 'nvm-fixture-node-v{version}\\n'\n"
                ),
            ),
            (
                "npm",
                format!(
                    "#!/bin/sh\nif cat {quoted_default} >/dev/null 2>&1; then exit 23; fi\nnode --version || exit $?\nprintf 'nvm-fixture-npm-executed\\n'\n"
                ),
            ),
        ] {
            let path = bin.join(name);
            fs::write(&path, script).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
    let home = home.canonicalize().unwrap();
    let grants = readable_toolchains(&home, std::slice::from_ref(&home), false).unwrap();
    assert_eq!(grants, vec![home.join(".nvm/versions")]);
    let mut request = CheckRequest::new(
        &project,
        root.join("data"),
        "npm --version",
        Duration::from_secs(20),
    );
    request.read_toolchains = grants;
    let output = runner::start_with_options(
        request,
        RunnerOptions {
            without_systemd: true,
        },
    )
    .await
    .unwrap()
    .wait()
    .await
    .unwrap();
    assert_eq!(output.outcome, CheckOutcome::Passed, "{output:?}");
    assert!(
        output
            .output_tail
            .contains(&format!("nvm-fixture-node-v{selected}")),
        "{output:?}"
    );
    assert!(
        output.output_tail.contains("nvm-fixture-npm-executed"),
        "{output:?}"
    );
}
