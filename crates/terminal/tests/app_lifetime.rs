// SPDX-License-Identifier: Apache-2.0
//! A foreground app can exit while its terminal shell stays alive.
#![cfg(any(target_os = "linux", windows))]

use ports::{TerminalEvent, TerminalHost, TerminalSpec};
use protocol::terminal::TerminalState;
use std::time::Duration;
use terminal::PtyHost;

#[tokio::test]
async fn a_live_app_is_distinct_from_its_idle_terminal_shell() {
    let host = PtyHost::default();
    #[cfg(target_os = "linux")]
    let program = (
        "/bin/sh".into(),
        vec![
            "-c".into(),
            "sh -c 'printf APP_LIVE; read answer'; printf SHELL_IDLE; read final".into(),
        ],
    );
    #[cfg(windows)]
    let program = (
        "cmd.exe".into(),
        vec!["/d".into(), "/q".into(), "/v:off".into()],
    );
    let info = host
        .open(TerminalSpec {
            title: "App lifetime fixture".into(),
            cwd: std::env::current_dir().unwrap(),
            program: Some(program),
            env: vec![],
            project_id: None,
            harness: None,
            cols: 100,
            rows: 30,
        })
        .await
        .unwrap();
    let id = info.terminal_id;
    let (replay, mut events) = host.attach(id, Some(0)).await.unwrap();
    // open() has assigned the shell to its job before it can create this
    // child. Construct the markers so echoed input cannot satisfy the test.
    #[cfg(windows)]
    host.write(id, b"set \"ophio_test_marker=APP_\" & cmd.exe /d /q /v:on /c \"set /p answer=!ophio_test_marker!LIVE\" & echo SHELL_^IDLE\r")
        .await.unwrap();
    let mut output = replay.bytes.to_vec();
    let result = tokio::time::timeout(Duration::from_secs(15), async {
        for marker in ["APP_LIVE", "SHELL_IDLE"] {
            while !String::from_utf8_lossy(&output).contains(marker) {
                match events.recv().await.unwrap() {
                    TerminalEvent::Output { bytes, .. } => output.extend(bytes),
                    TerminalEvent::Exited { .. } => panic!("shell exited before {marker}"),
                    _ => {}
                }
            }
            let expected = marker == "APP_LIVE";
            let activity = host.has_running_children(id).await.unwrap();
            #[cfg(target_os = "linux")]
            if activity.is_none() {
                eprintln!(
                    "No delegated cgroup: app lifetime is conservatively unknown on this host"
                );
                return;
            }
            assert_eq!(activity, Some(expected));
            assert_eq!(host.info(id).await.unwrap().state, TerminalState::Running);
            if expected {
                host.write(id, b"done\r").await.unwrap();
            }
        }
    })
    .await;
    host.close(id).await.unwrap();
    result.unwrap();
}
