// SPDX-License-Identifier: Apache-2.0
#![cfg(unix)]
use ports::terminal::{TerminalError, TerminalEvent, TerminalHost, TerminalSpec};
use protocol::{ids::TerminalId, terminal::TerminalState};
use std::{io::Write, sync::Arc, time::Duration};
use terminal::PtyHost;

fn spec(script: &str) -> TerminalSpec {
    TerminalSpec {
        title: "Input fixture".into(),
        cwd: std::env::current_dir().unwrap(),
        program: Some(("/bin/sh".into(), vec!["-c".into(), script.into()])),
        env: vec![],
        project_id: None,
        harness: None,
        cols: 80,
        rows: 24,
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    release: std::fs::File,
    output: std::path::PathBuf,
    launch: TerminalSpec,
}

impl Fixture {
    fn new(after_release: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let lifetime = dir.path().join("lifetime");
        let output = dir.path().join("input");
        nix::unistd::mkfifo(
            &lifetime,
            nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR,
        )
        .unwrap();
        let release = std::fs::File::options()
            .read(true)
            .write(true)
            .open(&lifetime)
            .unwrap();
        let mut launch = spec(&format!(
            r#"exec 3<"$FIXTURE_LIFETIME"
stty -icanon -echo min 1 time 0 || exit 1
printf 'ready;'
dd bs=1 count=1 of="$FIXTURE_OUTPUT" 2>/dev/null || exit 1
printf 'input-started;'
dd bs=1 count=1 <&3 of=/dev/null 2>/dev/null || exit 1
{after_release}"#
        ));
        launch.env = vec![
            ("FIXTURE_LIFETIME".into(), lifetime.to_str().unwrap().into()),
            ("FIXTURE_OUTPUT".into(), output.to_str().unwrap().into()),
        ];
        Self {
            _dir: dir,
            release,
            output,
            launch,
        }
    }
}

async fn marker(
    host: &PtyHost,
    id: TerminalId,
    received: &mut Vec<u8>,
    events: &mut tokio::sync::broadcast::Receiver<TerminalEvent>,
    marker: &[u8],
) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !received.ends_with(marker) {
            match events.recv().await.unwrap() {
                TerminalEvent::Output { bytes, .. } => received.extend(bytes),
                event => panic!("expected fixture output, got {event:?}"),
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("fixture did not reach {marker:?}"));
    assert_eq!(host.info(id).await.unwrap().state, TerminalState::Running);
}

#[test]
fn blocked_and_queued_input_settle_without_occupying_the_blocking_pool() {
    let (finished, result) = std::sync::mpsc::channel();
    // Keep the deadline independent of Tokio so a stalled runtime fails too.
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .max_blocking_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let test = runtime.spawn(async {
            for close in [true, false] {
                let mut fixture = Fixture::new("exit 0");
                let host = Arc::new(PtyHost::default());
                let id = host.open(fixture.launch.clone()).await.unwrap().terminal_id;
                let (replay, mut events) = host.attach(id, Some(0)).await.unwrap();
                let mut received = replay.bytes.to_vec();
                marker(&host, id, &mut received, &mut events, b"ready;").await;
                let input_host = host.clone();
                let mut writer =
                    tokio::spawn(async move { input_host.write(id, &vec![b'x'; 1024 * 1024]).await });
                marker(&host, id, &mut received, &mut events, b"input-started;").await;
                assert!(
                    tokio::time::timeout(Duration::from_millis(100), &mut writer)
                        .await
                        .is_err()
                );
                let mut queued = Vec::new();
                // Exceed the input queue's capacity to cover admission cancellation
                // as well as cancellation of accepted and active writes.
                for _ in 0..32 {
                    let host = host.clone();
                    queued.push(tokio::spawn(async move { host.write(id, b"tail").await }));
                }
                tokio::task::yield_now().await;

                tokio::time::timeout(Duration::from_secs(3), async {
                    let other = host
                        .open(spec(
                        "stty -echo; printf 'other-ready;'; read answer; printf 'answered;'; read final",
                    ))
                        .await
                        .unwrap()
                        .terminal_id;
                    let (replay, mut events) = host.attach(other, Some(0)).await.unwrap();
                    let mut received = replay.bytes.to_vec();
                    marker(&host, other, &mut received, &mut events, b"other-ready;").await;
                    host.write(other, b"hello\n").await.unwrap();
                    marker(&host, other, &mut received, &mut events, b"answered;").await;
                    host.close(other).await.unwrap();
                })
                .await
                .expect("blocked input stalled another terminal or the blocking pool");

                if close {
                    tokio::time::timeout(Duration::from_secs(6), host.close(id))
                        .await
                        .unwrap()
                        .unwrap();
                } else {
                    fixture.release.write_all(b"x").unwrap();
                }
                tokio::time::timeout(Duration::from_secs(5), async {
                    assert!(writer.await.unwrap().is_err());
                    for writer in queued {
                        assert!(writer.await.unwrap().is_err());
                    }
                    while host.info(id).await.unwrap().state == TerminalState::Running {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                    assert_eq!(tokio::task::spawn_blocking(|| 42).await.unwrap(), 42);
                })
                .await
                .expect("input or native workers remained stuck after exit");
                host.close(id).await.unwrap();
                assert!(matches!(
                    host.write(id, b"late").await,
                    Err(TerminalError::Exited)
                ));
            }
        });
        let result = runtime.block_on(test);
        runtime.shutdown_background();
        let _ = finished.send(result);
    });
    result
        .recv_timeout(Duration::from_secs(30))
        .expect("blocked input stalled the runtime")
        .unwrap();
}

#[tokio::test]
async fn accepted_input_finishes_in_order_when_its_caller_disconnects() {
    let first: Vec<_> = (0..64 * 1024).map(|i| b'A' + (i % 26) as u8).collect();
    let second = vec![b'!'; 8192];
    let mut fixture = Fixture::new(&format!(
        "dd bs=1 count={} >>\"$FIXTURE_OUTPUT\" 2>/dev/null; printf 'received;'; exec cat <&3",
        first.len() + second.len() - 1,
    ));
    let host = Arc::new(PtyHost::default());
    let id = host.open(fixture.launch.clone()).await.unwrap().terminal_id;
    let (replay, mut events) = host.attach(id, Some(0)).await.unwrap();
    let mut received = replay.bytes.to_vec();
    marker(&host, id, &mut received, &mut events, b"ready;").await;
    let input_host = host.clone();
    let input = first.clone();
    let mut writer = tokio::spawn(async move { input_host.write(id, &input).await });
    marker(&host, id, &mut received, &mut events, b"input-started;").await;
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut writer)
            .await
            .is_err()
    );
    writer.abort();
    assert!(writer.await.unwrap_err().is_cancelled());
    fixture.release.write_all(b"x").unwrap();
    tokio::time::timeout(Duration::from_secs(5), host.write(id, &second))
        .await
        .unwrap()
        .unwrap();
    marker(&host, id, &mut received, &mut events, b"received;").await;
    host.close(id).await.unwrap();
    let expected = [first, second].concat();
    assert_eq!(std::fs::read(&fixture.output).unwrap(), expected);
}
