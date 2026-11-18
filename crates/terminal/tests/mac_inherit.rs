// SPDX-License-Identifier: Apache-2.0
//! On the Mac the standard library makes a start's pipes first and marks them
//! close-on-exec afterwards. A terminal's program starts only while nothing
//! private is being made, as other starts do, so an agent started meanwhile
//! keeps none of those pipes and this start never waits for that agent.
//! This is a program of its own: it holds the one process-wide lock for a
//! while, which would hold up other tests' starts.
#![cfg(target_os = "macos")]
use ports::terminal::{TerminalHost, TerminalSpec};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};
use terminal::PtyHost;

#[tokio::test]
async fn a_terminal_starts_its_program_only_between_private_steps() {
    let ended = Arc::new(AtomicBool::new(false));
    let (inside, is_inside) = mpsc::channel();
    let step = {
        let ended = ended.clone();
        thread::spawn(move || {
            containment::inherit::private(|| {
                inside.send(()).unwrap();
                thread::sleep(Duration::from_millis(300));
                ended.store(true, Ordering::SeqCst);
            })
        })
    };
    is_inside.recv().unwrap();
    let host = PtyHost::default();
    let opened = host
        .open(TerminalSpec {
            title: "Fixture".into(),
            cwd: std::env::current_dir().unwrap(),
            program: Some(("/bin/sh".into(), vec!["-c".into(), "exit 0".into()])),
            env: vec![],
            project_id: None,
            harness: None,
            cols: 80,
            rows: 24,
        })
        .await;
    let started_after = ended.load(Ordering::SeqCst);
    step.join().unwrap();
    opened.unwrap();
    assert!(
        started_after,
        "a terminal's program started during a private step"
    );
}
