// SPDX-License-Identifier: Apache-2.0
//! Alone in its own test binary, so no other terminal's threads are counted.
#![cfg(target_os = "linux")]
use ports::terminal::{TerminalEvent, TerminalHost, TerminalSpec};
use std::time::Duration;
use terminal::PtyHost;

/// Scheduler switches so far of this process's threads named `name`.
fn switches(name: &str) -> u64 {
    std::fs::read_dir("/proc/self/task")
        .unwrap()
        .flatten()
        .filter(|task| {
            std::fs::read_to_string(task.path().join("comm")).is_ok_and(|c| c.trim_end() == name)
        })
        .filter_map(|task| std::fs::read_to_string(task.path().join("status")).ok())
        .flat_map(|status| {
            status
                .lines()
                .filter(|l| l.contains("ctxt_switches:"))
                .filter_map(|l| l.split_whitespace().nth(1)?.parse::<u64>().ok())
                .collect::<Vec<_>>()
        })
        .sum()
}

#[tokio::test]
async fn an_idle_terminal_leaves_its_threads_asleep() {
    let host = PtyHost::default();
    let terminal = host
        .open(TerminalSpec {
            title: "Idle".into(),
            cwd: std::env::current_dir().unwrap(),
            program: Some((
                "/bin/sh".into(),
                vec!["-c".into(), "printf ready; sleep 120".into()],
            )),
            env: vec![],
            project_id: None,
            harness: None,
            cols: 80,
            rows: 24,
        })
        .await
        .unwrap()
        .terminal_id;
    let (replay, mut stream) = host.attach(terminal, Some(0)).await.unwrap();
    if replay.bytes.is_empty() {
        tokio::time::timeout(Duration::from_secs(5), async {
            while !matches!(stream.recv().await.unwrap(), TerminalEvent::Output { .. }) {}
        })
        .await
        .unwrap();
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    let before = [switches("terminal-output"), switches("terminal-reaper")];
    tokio::time::sleep(Duration::from_millis(500)).await;
    let after = [switches("terminal-output"), switches("terminal-reaper")];
    host.close(terminal).await.unwrap();
    assert!(
        after[0] - before[0] <= 5 && after[1] - before[1] <= 5,
        "idle wakeups in 500 ms: output {}, reaper {}",
        after[0] - before[0],
        after[1] - before[1]
    );
}
