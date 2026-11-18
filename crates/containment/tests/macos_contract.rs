// SPDX-License-Identifier: Apache-2.0
//! Synthetic evidence of the Mac process-group boundary, not stronger
//! containment. The detached fixture exits when its private input pipe closes.
#![cfg(target_os = "macos")]
use containment::{Container, ResourceLimits};
use ports::Containment;
use std::{process::Stdio, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::Command,
};

const MODE: &str = "OPHIO_MAC_CONTAINMENT_FIXTURE";

#[test]
fn detached_fixture() {
    if std::env::var(MODE).as_deref() != Ok("detached") {
        return;
    }
    rustix::process::setsid().unwrap();
    println!("FIXTURE detached ready");
    let mut byte = [0];
    // Closing the parent's owned pipe, including on panic/cancellation, ends
    // the fixture. No PID-based cleanup can signal a reused or unrelated PID.
    if std::io::Read::read(&mut std::io::stdin(), &mut byte).unwrap() == 1 {
        println!("FIXTURE detached survived group cleanup");
    }
}

#[test]
fn leader_fixture() {
    if std::env::var(MODE).as_deref() != Ok("leader") {
        return;
    }
    std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "detached_fixture",
            "--nocapture",
            "--test-threads=1",
        ])
        .env_clear()
        .env(MODE, "detached")
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::null())
        .spawn()
        .unwrap()
        .wait()
        .unwrap();
}

#[tokio::test]
async fn detached_descendant_is_not_claimed_as_complete_containment() {
    let container = Container::new().unwrap();
    assert!(matches!(
        container.containment(),
        Containment::ProcessGroup { .. }
    ));
    assert!(
        Container::limited(ResourceLimits {
            memory_bytes: 64 * 1024 * 1024,
            cpu_millis_per_second: 100,
            processes: 8
        })
        .is_err()
    );
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "leader_fixture",
            "--nocapture",
            "--test-threads=1",
        ])
        .env_clear()
        .env(MODE, "leader")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut leader = container.spawn(&mut command).await.unwrap();
    let mut input = leader.stdin.take().unwrap();
    let mut output = BufReader::new(leader.stdout.take().unwrap());
    let mut line = String::new();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            line.clear();
            assert_ne!(output.read_line(&mut line).await.unwrap(), 0);
            if line.contains("FIXTURE detached ready") {
                break;
            }
        }
    })
    .await
    .expect("detached fixture must start");
    container.end().await.unwrap();
    leader.wait().await.unwrap();
    // The detached process is still alive after successful *group* cleanup.
    // Let it demonstrate that, then close its pipe so it cannot remain behind.
    input.write_all(b"x").await.unwrap();
    drop(input);
    let mut rest = String::new();
    tokio::time::timeout(
        Duration::from_secs(5),
        tokio::io::AsyncReadExt::read_to_string(&mut output, &mut rest),
    )
    .await
    .expect("fixture must exit when its private pipe closes")
    .unwrap();
    assert!(
        rest.contains("FIXTURE detached survived group cleanup"),
        "{rest}"
    );
    assert!(matches!(
        container.containment(),
        Containment::ProcessGroup { .. }
    ));
}
