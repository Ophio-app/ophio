// SPDX-License-Identifier: Apache-2.0
#![cfg(unix)]
use ports::{
    clock::Clock,
    terminal::{TerminalError, TerminalEvent, TerminalHost, TerminalSpec},
};
use protocol::{ids::TerminalId, terminal::TerminalState, time::Timestamp};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use terminal::{PtyConfig, PtyHost};

#[derive(Debug)]
struct FixedClock(Instant);
impl Clock for FixedClock {
    fn now(&self) -> Timestamp {
        Timestamp(1234)
    }
    fn monotonic(&self) -> Instant {
        self.0
    }
}
fn host(config: PtyConfig) -> PtyHost {
    PtyHost::new(config, Arc::new(FixedClock(Instant::now()))).unwrap()
}
fn spec(script: &str) -> TerminalSpec {
    TerminalSpec {
        title: "Fixture".into(),
        cwd: std::env::current_dir().unwrap(),
        program: Some(("/bin/sh".into(), vec!["-c".into(), script.into()])),
        env: vec![],
        project_id: None,
        harness: None,
        cols: 80,
        rows: 24,
    }
}
async fn exited(host: &PtyHost, id: TerminalId) {
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            if matches!(
                host.info(id).await.unwrap().state,
                TerminalState::Exited { .. }
            ) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn real_shell_exit_code_and_replay_offsets() {
    let host = host(PtyConfig::default());
    let info = host
        .open(spec("printf 'hello'; printf ' world'; exit 7"))
        .await
        .unwrap();
    assert_eq!(info.created_at, Timestamp(1234));
    assert_eq!(info.last_output_at, None);
    let (replay, mut stream) = host.attach(info.terminal_id, Some(0)).await.unwrap();
    let mut offset = replay.from + replay.bytes.len() as u64;
    let mut bytes = replay.bytes.to_vec();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match stream.recv().await.unwrap() {
                TerminalEvent::Output {
                    offset: at,
                    bytes: chunk,
                } => {
                    assert_eq!(at, offset);
                    offset += chunk.len() as u64;
                    bytes.extend(chunk);
                }
                TerminalEvent::Exited { code } => {
                    assert_eq!(code, Some(7));
                    break;
                }
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(bytes, b"hello world");
    assert_eq!(
        host.info(info.terminal_id).await.unwrap().last_output_at,
        Some(Timestamp(1234))
    );
    assert_eq!(host.list().await[0].last_output_at, Some(Timestamp(1234)));
    assert_eq!(
        host.info(info.terminal_id).await.unwrap().output_offset,
        offset
    );
    let (replay, mut stream) = host.attach(info.terminal_id, Some(6)).await.unwrap();
    assert_eq!(&replay.bytes[..], b"world");
    assert_eq!(
        stream.recv().await.unwrap(),
        TerminalEvent::Exited { code: Some(7) }
    );
    assert!(
        host.attach(info.terminal_id, Some(offset + 1))
            .await
            .is_err()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn repeated_attach_during_heavy_output_has_no_gaps_or_duplicates() {
    for round in 0..8 {
        let host = Arc::new(host(PtyConfig {
            event_capacity: 4096,
            ..PtyConfig::default()
        }));
        let terminal = host
            .open(spec(
                r#"i=0; while [ $i -lt 4000 ]; do printf '%04d,' "$i"; i=$((i+1)); done"#,
            ))
            .await
            .unwrap()
            .terminal_id;
        let mut tasks = Vec::new();
        for start in 0..6 {
            let host = host.clone();
            tasks.push(tokio::spawn(async move {
                for _ in 0..(start + round) {
                    tokio::task::yield_now().await;
                }
                let (replay, mut live) = host.attach(terminal, Some(0)).await.unwrap();
                assert_eq!(replay.from, 0);
                assert!(!replay.truncated);
                let mut bytes = replay.bytes.to_vec();
                tokio::time::timeout(Duration::from_secs(8), async {
                    loop {
                        match live.recv().await.unwrap() {
                            TerminalEvent::Output {
                                offset,
                                bytes: chunk,
                            } => {
                                assert_eq!(offset, bytes.len() as u64);
                                bytes.extend(chunk);
                            }
                            TerminalEvent::Exited { .. } => break,
                            _ => {}
                        }
                    }
                })
                .await
                .unwrap();
                let expected = (0..4000).map(|n| format!("{n:04},")).collect::<String>();
                assert_eq!(bytes, expected.as_bytes());
            }));
        }
        for task in tasks {
            task.await.unwrap();
        }
    }
}

#[tokio::test]
async fn truncated_replay_and_lagged_receiver_can_resume() {
    let host = host(PtyConfig {
        scrollback_bytes: 32,
        event_capacity: 1,
        ..PtyConfig::default()
    });
    let terminal = host
        .open(spec(
            "read answer; i=0; while [ $i -lt 2000 ]; do printf 0123456789; i=$((i+1)); done",
        ))
        .await
        .unwrap()
        .terminal_id;
    let (_, mut live) = host.attach(terminal, Some(0)).await.unwrap();
    host.write(terminal, b"go\n").await.unwrap();
    exited(&host, terminal).await;
    assert!(matches!(
        live.recv().await,
        Err(tokio::sync::broadcast::error::RecvError::Lagged(_))
    ));
    let info = host.info(terminal).await.unwrap();
    let (replay, _) = host.attach(terminal, Some(0)).await.unwrap();
    assert!(replay.truncated);
    assert_eq!(replay.bytes.len(), 32);
    assert_eq!(replay.from + 32, info.output_offset);
    let (tail, _) = host
        .attach(terminal, Some(info.output_offset - 10))
        .await
        .unwrap();
    assert!(!tail.truncated);
    let (latest, _) = host.attach(terminal, None).await.unwrap();
    assert!(!latest.truncated);
    assert_eq!(latest.bytes.len(), 32);
    // The line discipline may echo the typed newline after the script's
    // output on a loaded machine, so the resumed bytes are checked against
    // the scrollback rather than a literal.
    assert_eq!(&tail.bytes[..], &latest.bytes[22..]);
    assert!(latest.bytes.windows(10).any(|w| w == b"0123456789"));
}

#[tokio::test]
async fn input_resize_environment_and_close_work() {
    let host = host(PtyConfig::default());
    let mut launch = spec(
        "printf '%s:%s\\n' \"$TERM\" \"$FIXTURE_VAR\"; read answer; printf 'got:%s\\n' \"$answer\"; read again",
    );
    launch.env.push(("FIXTURE_VAR".into(), "value".into()));
    let terminal = host.open(launch).await.unwrap().terminal_id;
    let (replay, mut stream) = host.attach(terminal, None).await.unwrap();
    let mut received = replay.bytes.to_vec();
    host.resize(terminal, 120, 40).await.unwrap();
    host.write(terminal, b"hello\n").await.unwrap();
    let mut resized = false;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match stream.recv().await.unwrap() {
                TerminalEvent::Output { bytes, .. } => received.extend(bytes),
                TerminalEvent::Resized { cols, rows } => {
                    assert_eq!((cols, rows), (120, 40));
                    resized = true;
                }
                _ => {}
            }
            let text = String::from_utf8_lossy(&received);
            if resized && text.contains("got:hello") && text.contains("xterm-256color:value") {
                break;
            }
        }
    })
    .await
    .unwrap();
    host.close(terminal).await.unwrap();
    assert!(matches!(
        host.info(terminal).await.unwrap().state,
        TerminalState::Exited { .. }
    ));
    assert!(matches!(
        host.write(terminal, b"x").await,
        Err(TerminalError::Exited)
    ));
    host.close(terminal).await.unwrap();
}

#[tokio::test]
async fn concurrent_admission_limit_and_failed_spawns_do_not_leak_slots() {
    let host = Arc::new(host(PtyConfig {
        max_terminals: 1,
        ..PtyConfig::default()
    }));
    let mut bad = spec("");
    bad.program = Some(("/no-such-fixture-program".into(), vec![]));
    assert!(matches!(host.open(bad).await, Err(TerminalError::Spawn(_))));
    let (a, b) = tokio::join!(
        host.open(spec("read answer")),
        host.open(spec("read answer"))
    );
    let terminal = match (a, b) {
        (Ok(info), Err(TerminalError::Limit)) | (Err(TerminalError::Limit), Ok(info)) => {
            info.terminal_id
        }
        other => panic!("unexpected {other:?}"),
    };
    host.close(terminal).await.unwrap();
    let next = host.open(spec("exit 0")).await.unwrap();
    exited(&host, next.terminal_id).await;
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn close_kills_background_descendants() {
    let host = host(PtyConfig::default());
    let terminal = host
        .open(spec("sleep 120 & printf '%s\\n' \"$!\"; wait"))
        .await
        .unwrap()
        .terminal_id;
    let (replay, mut stream) = host.attach(terminal, Some(0)).await.unwrap();
    let mut bytes = replay.bytes.to_vec();
    tokio::time::timeout(Duration::from_secs(3), async {
        while !bytes.contains(&b'\n') {
            if let TerminalEvent::Output { bytes: chunk, .. } = stream.recv().await.unwrap() {
                bytes.extend(chunk);
            }
        }
    })
    .await
    .unwrap();
    let pid = String::from_utf8_lossy(&bytes)
        .trim()
        .parse::<u32>()
        .unwrap();
    host.close(terminal).await.unwrap();
    // A killed grandchild can briefly remain a zombie until the host init reaps it.
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"));
    assert!(stat.is_err() || stat.unwrap().rsplit_once(") ").unwrap().1.starts_with('Z'));
}

#[test]
fn close_is_not_blocked_by_a_full_input_pipe() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let test = runtime.spawn(async {
        async fn output_until(
            received: &mut Vec<u8>,
            stream: &mut tokio::sync::broadcast::Receiver<TerminalEvent>,
            marker: &[u8],
        ) {
            tokio::time::timeout(Duration::from_secs(5), async {
                while !received.ends_with(marker) {
                    match stream.recv().await.unwrap() {
                        TerminalEvent::Output { bytes, .. } => received.extend(bytes),
                        event => panic!("expected fixture output, got {event:?}"),
                    }
                }
            })
            .await
            .expect("fixture did not reach its input handshake");
        }

        // Closing this FIFO's writer releases the fixture even if an assertion
        // panics before Close can terminate it.
        let dir = tempfile::tempdir().unwrap();
        let lifetime = dir.path().join("lifetime");
        nix::unistd::mkfifo(
            &lifetime,
            nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR,
        )
        .unwrap();
        let _keep_alive = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&lifetime)
            .unwrap();

        let host = Arc::new(host(PtyConfig::default()));
        let mut launch = spec(
            r#"exec 3<"$FIXTURE_LIFETIME"
    stty -icanon -echo min 1 time 0 || exit 1
    printf 'ready;'
    dd bs=1 count=1 of=/dev/null 2>/dev/null || exit 1
    printf 'input-started;'
    exec cat <&3"#,
        );
        launch
            .env
            .push(("FIXTURE_LIFETIME".into(), lifetime.to_str().unwrap().into()));
        let terminal = host.open(launch).await.unwrap().terminal_id;
        let (replay, mut stream) = host.attach(terminal, Some(0)).await.unwrap();
        let mut received = replay.bytes.to_vec();
        output_until(&mut received, &mut stream, b"ready;").await;

        let input_host = host.clone();
        let mut writer =
            tokio::spawn(async move { input_host.write(terminal, &vec![b'x'; 1024 * 1024]).await });
        // The fixture acknowledges an actual byte, so this cannot pass merely
        // because the blocking worker has not been scheduled yet. It consumes no
        // more PTY input, and noncanonical mode cannot discard an overlong line.
        output_until(&mut received, &mut stream, b"input-started;").await;
        assert!(
            tokio::time::timeout(Duration::from_millis(100), &mut writer)
                .await
                .is_err(),
            "input must remain blocked before Close"
        );
        tokio::time::timeout(Duration::from_secs(6), host.close(terminal))
            .await
            .expect("Close waited for blocked input instead of terminating the fixture")
            .expect("Close failed while input was blocked");
        let _ = tokio::time::timeout(Duration::from_secs(5), writer)
            .await
            .expect("pending input did not settle after Close")
            .unwrap();
        assert!(matches!(
            host.info(terminal).await.unwrap().state,
            TerminalState::Exited { .. }
        ));
    });
    let result = runtime.block_on(test);
    // A stuck native write cannot be cancelled. Report the task's assertion
    // failure instead of hanging the entire test binary during runtime drop.
    // On failure, any remaining blocking thread ends with the test process.
    runtime.shutdown_background();
    result.unwrap();
}
