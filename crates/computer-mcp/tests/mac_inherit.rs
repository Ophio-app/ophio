// SPDX-License-Identifier: Apache-2.0
//! On the Mac a socket is made first and marked close-on-exec afterwards, so a
//! process started in between keeps it. The bridge's listener and the
//! connections it accepts are made only while no process is being started:
//! an agent started as another run connects must not hold that run's end.
//! This is a program of its own: it holds the one process-wide lock for a
//! while, which would hold up other tests' starts.
#![cfg(target_os = "macos")]
use async_trait::async_trait;
use containment::inherit::start;
use ports::bridge::*;
use protocol::ids::RunId;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

struct Handler;
#[async_trait]
impl BridgeHandler for Handler {
    async fn bind(&self, token: &str) -> Option<BridgeBinding> {
        (token == "fixture-token").then(|| BridgeBinding {
            run_id: RunId::new(),
            target_id: None,
            computer_use: false,
            browser: false,
            github: false,
            checks: false,
            checklist_answers: false,
            read_only: false,
        })
    }
    async fn handle(&self, _: &BridgeBinding, _: BridgeRequest) -> BridgeResponse {
        BridgeResponse::Done
    }
    async fn unbound(&self, _: &BridgeBinding) {}
}

/// A start that lasts a while on another thread, and says when it has ended.
struct Starting {
    ended: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}
impl Starting {
    fn begin() -> Self {
        let ended = Arc::new(AtomicBool::new(false));
        let (inside, is_inside) = mpsc::channel();
        let thread = {
            let ended = ended.clone();
            thread::spawn(move || {
                start(|| {
                    inside.send(()).unwrap();
                    thread::sleep(Duration::from_millis(300));
                    ended.store(true, Ordering::SeqCst);
                })
            })
        };
        is_inside.recv().unwrap();
        Self {
            ended,
            thread: Some(thread),
        }
    }
    fn ended(&self) -> bool {
        self.ended.load(Ordering::SeqCst)
    }
}
impl Drop for Starting {
    fn drop(&mut self) {
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_bridge_listens_and_accepts_only_between_starts() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("private").join("bridge.sock");

    let starting = Starting::begin();
    let (ready, is_ready) = tokio::sync::oneshot::channel();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(computer_mcp::serve::serve_bridge_ready(
        path.clone(),
        Arc::new(Handler),
        async {
            let _ = stopped.await;
        },
        Some(ready),
    ));
    is_ready.await.unwrap();
    assert!(starting.ended(), "the bridge listened during a start");
    drop(starting);

    // The kernel queues the connection whatever the bridge does; only its
    // answer shows that it accepted.
    let starting = Starting::begin();
    let stream = tokio::net::UnixStream::connect(&path).await.unwrap();
    let (reader, mut writer) = stream.into_split();
    let mut hello = serde_json::to_vec(&BridgeRequest::Hello {
        run_token: "fixture-token".into(),
    })
    .unwrap();
    hello.push(b'\n');
    writer.write_all(&hello).await.unwrap();
    let mut answer = String::new();
    BufReader::new(reader).read_line(&mut answer).await.unwrap();
    assert!(
        starting.ended(),
        "the bridge accepted a connection during a start"
    );
    drop(starting);
    assert!(
        matches!(
            serde_json::from_str(&answer).unwrap(),
            BridgeResponse::Welcome { .. }
        ),
        "{answer}"
    );

    stop.send(()).unwrap();
    server.await.unwrap().unwrap();
}
