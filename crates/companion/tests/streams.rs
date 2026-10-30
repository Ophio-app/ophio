// SPDX-License-Identifier: Apache-2.0
mod support;
use protocol::{auth::ConnectionKind, media::*, terminal::*, transfer::*, *};
use services::api::*;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use support::*;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

fn frame(seq: u64, keyframe: bool) -> bytes::Bytes {
    build_tile_frame(
        TileFrameHeader {
            seq,
            keyframe,
            cursor: None,
            tile: 64,
            geometry_revision: 1,
            enc_width: 64,
            enc_height: 64,
            src_width: 64,
            src_height: 64,
            region: None,
            count: 0,
        },
        &[],
    )
    .into()
}
async fn until(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while !condition() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn media_window_keeps_latest_keyframe_and_forwards_controls() {
    let fixture = Fixture::new().await;
    let control = Arc::new(MediaMock::default());
    let (tx, rx) = mpsc::channel(16);
    *fixture.hub.media.lock().unwrap() = Some(MediaSession {
        output: rx,
        control: control.clone(),
    });
    let mut client = fixture.authenticated(ConnectionKind::Media).await;
    let target_id = TargetId::new();
    client
        .send(&MediaClientFrame::Open {
            target_id,
            params: StreamParams::default(),
        })
        .await;
    tx.send(MediaOut::Control(MediaServerFrame::Opened {
        session_id: MediaSessionId::new(),
        target_id,
        geometry: protocol::control::TargetGeometry {
            width: 64,
            height: 64,
            scale: 1.0,
            rotation_degrees: 0,
            revision: 1,
        },
        tile: 64,
    }))
    .await
    .unwrap();
    assert!(matches!(
        client.receive::<MediaServerFrame>().await,
        MediaServerFrame::Opened { .. }
    ));
    for seq in 1..=2 {
        tx.send(MediaOut::Frame(frame(seq, seq == 1)))
            .await
            .unwrap();
        let Message::Binary(bytes) = client.message().await else {
            panic!("expected frame")
        };
        assert_eq!(parse_tile_frame(&bytes).unwrap().0.seq, seq);
    }
    for seq in 3..=5 {
        tx.send(MediaOut::Frame(frame(seq, true))).await.unwrap();
    }
    tx.send(MediaOut::Control(MediaServerFrame::Paused {
        reason: "barrier".into(),
    }))
    .await
    .unwrap();
    assert!(matches!(
        client.receive::<MediaServerFrame>().await,
        MediaServerFrame::Paused { .. }
    ));
    client.send(&MediaClientFrame::Ack { seq: 2 }).await;
    let Message::Binary(bytes) = client.message().await else {
        panic!("expected latest frame")
    };
    let header = parse_tile_frame(&bytes).unwrap().0;
    assert_eq!(header.seq, 5);
    assert!(header.keyframe);
    client
        .send(&MediaClientFrame::Adjust {
            params: StreamParams {
                quality: 60,
                ..Default::default()
            },
        })
        .await;
    client.send(&MediaClientFrame::RequestKeyframe).await;
    client.send(&MediaClientFrame::Close).await;
    until(|| control.closed.load(Ordering::SeqCst)).await;
    assert_eq!(*control.acks.lock().unwrap(), vec![2]);
    assert_eq!(control.adjusted.load(Ordering::SeqCst), 1);
    assert!(control.keyframes.load(Ordering::SeqCst) >= 1);
    fixture.stop().await;
}

#[tokio::test]
async fn terminal_offsets_input_resize_and_detach() {
    let fixture = Fixture::new().await;
    let terminal_id = TerminalId::new();
    let control = Arc::new(TerminalMock::default());
    let (tx, rx) = mpsc::channel(8);
    *fixture.hub.terminal.lock().unwrap() = Some(TerminalAttachment {
        terminal: TerminalInfo {
            terminal_id,
            title: "test".into(),
            cwd: "/test".into(),
            project_id: None,
            harness: None,
            cols: 80,
            rows: 24,
            state: TerminalState::Running,
            created_at: protocol::time::Timestamp(0),
            output_offset: 47,
            last_output_at: None,
        },
        replay_from: 42,
        replay_truncated: true,
        output: rx,
        input: control.clone(),
    });
    let mut client = fixture.authenticated(ConnectionKind::Terminal).await;
    client
        .send(&TerminalClientFrame::Attach {
            terminal_id,
            from_offset: Some(0),
        })
        .await;
    assert!(matches!(
        client.receive::<TerminalServerFrame>().await,
        TerminalServerFrame::Attached {
            replay_from: 42,
            replay_truncated: true,
            ..
        }
    ));
    tx.send(TerminalOut::Output {
        offset: 42,
        bytes: bytes::Bytes::from_static(b"hello"),
    })
    .await
    .unwrap();
    let Message::Binary(bytes) = client.message().await else {
        panic!("expected terminal bytes")
    };
    assert_eq!(split_output_frame(&bytes), Some((42, b"hello".as_slice())));
    client.binary(b"input\r").await;
    client
        .send(&TerminalClientFrame::Resize {
            cols: 100,
            rows: 30,
        })
        .await;
    client.send(&TerminalClientFrame::Detach).await;
    until(|| control.detached.load(Ordering::SeqCst)).await;
    assert_eq!(*control.bytes.lock().unwrap(), b"input\r");
    assert_eq!(*control.resized.lock().unwrap(), vec![(100, 30)]);
    fixture.stop().await;
}

#[tokio::test]
async fn transfer_upload_resumes_and_download_uses_requested_offset() {
    let fixture = Fixture::new().await;
    let transfer_id = TransferId::new();
    let mut client = fixture.authenticated(ConnectionKind::Transfer).await;
    client
        .send(&TransferClientFrame::Upload { transfer_id })
        .await;
    assert!(matches!(
        client.receive::<TransferServerFrame>().await,
        TransferServerFrame::ResumeAt { offset: 0, .. }
    ));
    client.binary(b"first").await;
    until(|| fixture.hub.uploaded.lock().unwrap().len() == 5).await;
    drop(client);
    let mut client = fixture.authenticated(ConnectionKind::Transfer).await;
    client
        .send(&TransferClientFrame::Upload { transfer_id })
        .await;
    assert!(matches!(
        client.receive::<TransferServerFrame>().await,
        TransferServerFrame::ResumeAt { offset: 5, .. }
    ));
    client.binary(b"second").await;
    client
        .send(&TransferClientFrame::Finish { transfer_id })
        .await;
    assert!(
        matches!(client.receive::<TransferServerFrame>().await, TransferServerFrame::Completed { sha256, .. } if sha256 == companion::identity::fingerprint(b"firstsecond"))
    );
    assert_eq!(*fixture.hub.uploaded.lock().unwrap(), b"firstsecond");
    let data = vec![23; MAX_CHUNK_BYTES * 2 + 19];
    *fixture.hub.download_bytes.lock().unwrap() = data.clone();
    let mut client = fixture.authenticated(ConnectionKind::Transfer).await;
    client
        .send(&TransferClientFrame::Download {
            transfer_id,
            offset: 7,
        })
        .await;
    assert!(matches!(
        client.receive::<TransferServerFrame>().await,
        TransferServerFrame::Sending { offset: 7, .. }
    ));
    let mut received = vec![];
    loop {
        match client.message().await {
            Message::Binary(bytes) => {
                assert!(bytes.len() <= MAX_CHUNK_BYTES);
                received.extend_from_slice(&bytes);
            }
            Message::Text(text) => {
                assert!(matches!(
                    serde_json::from_str::<TransferServerFrame>(&text).unwrap(),
                    TransferServerFrame::Completed { .. }
                ));
                break;
            }
            other => panic!("unexpected {other:?}"),
        }
    }
    assert_eq!(received, data[7..]);
    fixture.stop().await;
}

/// A phone that changes networks mid-upload leaves its connection open but
/// silent. The upload is let go after the silence limit, and the phone's
/// next connection continues from the last chunk written.
#[tokio::test]
async fn a_silent_upload_connection_lets_go_so_the_upload_can_resume() {
    let silence = std::time::Duration::from_millis(300);
    let fixture = Fixture::with_limits(companion::server::Limits {
        upload_silence: silence,
        ..Default::default()
    })
    .await;
    let transfer_id = TransferId::new();
    let mut silent = fixture.authenticated(ConnectionKind::Transfer).await;
    silent
        .send(&TransferClientFrame::Upload { transfer_id })
        .await;
    assert!(matches!(
        silent.receive::<TransferServerFrame>().await,
        TransferServerFrame::ResumeAt { offset: 0, .. }
    ));
    silent.binary(b"first").await;
    until(|| fixture.hub.uploaded.lock().unwrap().len() == 5).await;
    // Until then, the silent connection still holds the upload.
    let mut early = fixture.authenticated(ConnectionKind::Transfer).await;
    early
        .send(&TransferClientFrame::Upload { transfer_id })
        .await;
    assert!(
        matches!(early.receive::<ServerFrame>().await, ServerFrame::Fatal(error) if error.code == ErrorCode::Conflict)
    );
    let started = std::time::Instant::now();
    assert!(
        matches!(silent.receive::<ServerFrame>().await, ServerFrame::Fatal(error) if error.code == ErrorCode::Expired)
    );
    assert!(started.elapsed() < silence * 10);
    until(|| !fixture.hub.attached.load(Ordering::SeqCst)).await;
    let mut resumed = fixture.authenticated(ConnectionKind::Transfer).await;
    resumed
        .send(&TransferClientFrame::Upload { transfer_id })
        .await;
    assert!(matches!(
        resumed.receive::<TransferServerFrame>().await,
        TransferServerFrame::ResumeAt { offset: 5, .. }
    ));
    resumed.binary(b"second").await;
    resumed
        .send(&TransferClientFrame::Finish { transfer_id })
        .await;
    assert!(
        matches!(resumed.receive::<TransferServerFrame>().await, TransferServerFrame::Completed { sha256, .. } if sha256 == companion::identity::fingerprint(b"firstsecond"))
    );
    fixture.stop().await;
}

#[tokio::test]
async fn oversized_chunks_wrong_transfer_ids_and_cancel() {
    let fixture = Fixture::new().await;
    for oversized in [true, false] {
        let mut client = fixture.authenticated(ConnectionKind::Transfer).await;
        let transfer_id = TransferId::new();
        client
            .send(&TransferClientFrame::Upload { transfer_id })
            .await;
        let _: TransferServerFrame = client.receive().await;
        if oversized {
            client.binary(&vec![0; MAX_CHUNK_BYTES + 1]).await;
        } else {
            client
                .send(&TransferClientFrame::Finish {
                    transfer_id: TransferId::new(),
                })
                .await;
        }
        assert!(
            matches!(client.receive::<ServerFrame>().await, ServerFrame::Fatal(error) if error.code == ErrorCode::InvalidRequest)
        );
    }
    let mut client = fixture.authenticated(ConnectionKind::Transfer).await;
    client
        .send(&TransferClientFrame::Cancel {
            transfer_id: TransferId::new(),
        })
        .await;
    until(|| fixture.hub.cancelled.load(Ordering::SeqCst)).await;
    fixture.stop().await;
}

#[tokio::test]
async fn speech_pcm16_finish_and_invalid_sample_cleanup() {
    let fixture = Fixture::new().await;
    for valid in [true, false] {
        let (tx, rx) = mpsc::channel(8);
        let control = Arc::new(SpeechMock {
            bytes: Mutex::new(vec![]),
            finished: AtomicBool::new(false),
            cancelled: AtomicBool::new(false),
            output: tx.clone(),
        });
        *fixture.hub.speech.lock().unwrap() = Some(SpeechStream {
            output: rx,
            input: control.clone(),
        });
        let mut client = fixture.authenticated(ConnectionKind::Speech).await;
        let session_id = SpeechSessionId::new();
        client
            .send(&SpeechClientFrame::Start {
                session_id,
                language: Some("en".into()),
                keep_audio: false,
            })
            .await;
        tx.send(SpeechOut::Ready {
            engine: "test".into(),
        })
        .await
        .unwrap();
        assert!(
            matches!(client.receive::<SpeechServerFrame>().await, SpeechServerFrame::Ready { session_id: id, .. } if id == session_id)
        );
        if valid {
            client.binary(&[1, 0, 2, 0]).await;
            client.send(&SpeechClientFrame::Finish).await;
            assert!(
                matches!(client.receive::<SpeechServerFrame>().await, SpeechServerFrame::Finished { session_id: id, .. } if id == session_id)
            );
            assert_eq!(*control.bytes.lock().unwrap(), vec![1, 0, 2, 0]);
            assert!(control.finished.load(Ordering::SeqCst));
            assert!(!control.cancelled.load(Ordering::SeqCst));
        } else {
            client.binary(&[1]).await;
            assert!(
                matches!(client.receive::<ServerFrame>().await, ServerFrame::Fatal(error) if error.code == ErrorCode::InvalidRequest)
            );
            assert!(control.cancelled.load(Ordering::SeqCst));
        }
    }
    fixture.stop().await;
}

#[tokio::test]
async fn changed_terminal_grant_rejects_input_before_cleanup_poll() {
    let fixture = Fixture::new().await;
    let terminal_id = TerminalId::new();
    let input = Arc::new(TerminalMock::default());
    let (_tx, rx) = mpsc::channel(1);
    *fixture.hub.terminal.lock().unwrap() = Some(TerminalAttachment {
        terminal: TerminalInfo {
            terminal_id,
            title: "test".into(),
            cwd: "/test".into(),
            project_id: None,
            harness: None,
            cols: 80,
            rows: 24,
            state: TerminalState::Running,
            created_at: protocol::time::Timestamp(0),
            output_offset: 0,
            last_output_at: None,
        },
        replay_from: 0,
        replay_truncated: false,
        output: rx,
        input: input.clone(),
    });
    let mut client = fixture.authenticated(ConnectionKind::Terminal).await;
    client
        .send(&TerminalClientFrame::Attach {
            terminal_id,
            from_offset: None,
        })
        .await;
    let _: TerminalServerFrame = client.receive().await;
    fixture
        .hub
        .credentials
        .lock()
        .unwrap()
        .get_mut(&fixture.hub.device)
        .unwrap()
        .grants
        .terminal = false;
    client.binary(b"must never reach terminal").await;
    assert!(
        matches!(client.receive::<ServerFrame>().await, ServerFrame::Fatal(error) if error.code == ErrorCode::PermissionDenied)
    );
    assert!(input.bytes.lock().unwrap().is_empty());
    assert!(input.detached.load(Ordering::SeqCst));
    fixture.stop().await;
}

struct ExitDuringResize {
    output: mpsc::Sender<TerminalOut>,
    detached: AtomicBool,
}
#[async_trait::async_trait]
impl TerminalInput for ExitDuringResize {
    async fn write(&self, _: &[u8]) -> Result<(), ProtocolError> {
        Ok(())
    }
    async fn resize(&self, _: u16, _: u16) -> Result<(), ProtocolError> {
        // The PTY exits after select picks a resize, before resize completes.
        self.output
            .send(TerminalOut::Exited { code: Some(7) })
            .await
            .unwrap();
        Err(
            ProtocolError::new(ErrorCode::Conflict, "Terminal has exited")
                .with_detail(serde_json::json!({"terminal_state": "exited"})),
        )
    }
    async fn detach(&self) {
        self.detached.store(true, Ordering::SeqCst);
    }
}

#[tokio::test]
async fn terminal_exit_during_resize_drains_exit_before_close_and_detaches() {
    let fixture = Fixture::new().await;
    let terminal_id = TerminalId::new();
    let (tx, rx) = mpsc::channel(8);
    let control = Arc::new(ExitDuringResize {
        output: tx,
        detached: AtomicBool::new(false),
    });
    *fixture.hub.terminal.lock().unwrap() = Some(TerminalAttachment {
        terminal: TerminalInfo {
            terminal_id,
            title: "exit race".into(),
            cwd: "/test".into(),
            project_id: None,
            harness: None,
            cols: 80,
            rows: 24,
            state: TerminalState::Running,
            created_at: protocol::time::Timestamp(0),
            output_offset: 0,
            last_output_at: None,
        },
        replay_from: 0,
        replay_truncated: false,
        output: rx,
        input: control.clone(),
    });
    let mut client = fixture.authenticated(ConnectionKind::Terminal).await;
    client
        .send(&TerminalClientFrame::Attach {
            terminal_id,
            from_offset: Some(0),
        })
        .await;
    let _: TerminalServerFrame = client.receive().await;
    client
        .send(&TerminalClientFrame::Resize {
            cols: 100,
            rows: 30,
        })
        .await;
    assert!(matches!(
        client.receive::<TerminalServerFrame>().await,
        TerminalServerFrame::Exited { code: Some(7) }
    ));
    assert!(matches!(client.message().await, Message::Close(_)));
    until(|| control.detached.load(Ordering::SeqCst)).await;
    fixture.stop().await;
}
