// SPDX-License-Identifier: Apache-2.0
//! Real Windows pseudo-consoles. These run only on Windows.
#![cfg(windows)]
use ports::{
    clock::Clock,
    terminal::{TerminalEvent, TerminalHost, TerminalSpec},
};
use protocol::{ids::TerminalId, time::Timestamp};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use terminal::{PtyConfig, PtyHost};
use tokio::sync::broadcast::Receiver;

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
fn host() -> PtyHost {
    PtyHost::new(PtyConfig::default(), Arc::new(FixedClock(Instant::now()))).unwrap()
}
fn spec(program: Option<(&str, &[&str])>) -> TerminalSpec {
    TerminalSpec {
        title: "Fixture".into(),
        cwd: std::env::current_dir().unwrap(),
        program: program.map(|(name, args)| {
            (
                name.into(),
                args.iter().map(|arg| (*arg).to_owned()).collect(),
            )
        }),
        env: vec![],
        project_id: None,
        harness: None,
        cols: 100,
        rows: 30,
    }
}

/// Output until `done` holds, or a failure naming what did arrive.
async fn until(
    output: &mut Vec<u8>,
    stream: &mut Receiver<TerminalEvent>,
    done: impl Fn(&str) -> bool,
) {
    let waited = tokio::time::timeout(Duration::from_secs(30), async {
        while !done(&String::from_utf8_lossy(output)) {
            match stream.recv().await.unwrap() {
                TerminalEvent::Output { bytes, .. } => output.extend(bytes),
                TerminalEvent::Exited { code } => panic!("the shell exited with {code:?}"),
                _ => {}
            }
        }
    })
    .await;
    assert!(
        waited.is_ok(),
        "timed out; output so far: {:?}",
        String::from_utf8_lossy(output)
    );
}

/// A shell shows its prompt with nobody attached and nothing typed, and the
/// console's own cursor request never reaches the output.
async fn starts_unattended(
    program: Option<(&str, &[&str])>,
    prompt: &str,
    line: &[u8],
    answer: &str,
) {
    let host = host();
    let info = host.open(spec(program)).await.unwrap();
    let id: TerminalId = info.terminal_id;
    let mut output = Vec::new();
    // Nobody attaches until the prompt is already there.
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let (replay, _) = host.attach(id, Some(0)).await.unwrap();
            if String::from_utf8_lossy(&replay.bytes).contains(prompt) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("no prompt containing {prompt:?} without input"));
    let (replay, mut stream) = host.attach(id, Some(0)).await.unwrap();
    assert!(
        !replay.bytes.windows(4).any(|w| w == b"\x1b[6n"),
        "the cursor request reached the output: {:?}",
        String::from_utf8_lossy(&replay.bytes)
    );
    host.write(id, line).await.unwrap();
    until(&mut output, &mut stream, |text| text.contains(answer)).await;
    host.close(id).await.unwrap();
}

#[tokio::test]
async fn the_default_shell_starts_and_answers() {
    starts_unattended(None, "PS ", b"Write-Output ('CON'+'PTY_OK')\r", "CONPTY_OK").await;
}

#[tokio::test]
async fn command_prompt_starts_and_answers() {
    starts_unattended(
        Some(("cmd.exe", &["/d"])),
        ">",
        b"echo CON^PTY_OK\r",
        "CONPTY_OK",
    )
    .await;
}
