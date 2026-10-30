// SPDX-License-Identifier: Apache-2.0
#![cfg(unix)]
use super::*;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{UnixListener, UnixStream},
};

pub(super) async fn gate(listener: &UnixListener) -> UnixStream {
    let (mut connection, _) = tokio::time::timeout(Duration::from_secs(10), listener.accept())
        .await
        .unwrap()
        .unwrap();
    let mut ready = [0; 5];
    connection.read_exact(&mut ready).await.unwrap();
    assert_eq!(&ready, b"ready");
    connection
}
async fn exit(run: &mut RunStarted) -> RunExit {
    loop {
        if let HarnessEvent::Exited { exit, .. } = event(&mut run.events).await {
            return exit;
        }
    }
}

#[tokio::test]
async fn idle_native_process_finishes_at_turn_boundary() {
    let (dir, adapter, mut spec) = fixture();
    spec.prompt.text = "complete".into();
    let mut run = adapter.start_run(spec).await.unwrap();
    assert_eq!(exit(&mut run).await, RunExit::Completed);
    assert!(dir.path().join("stdin-closed").exists());
    assert_eq!(
        std::fs::read_to_string(dir.path().join("turns.jsonl"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    assert!(run.events.recv().await.is_none());
}

#[tokio::test]
async fn message_after_boundary_before_exit_is_definitely_rejected() {
    let (dir, adapter, mut spec) = fixture();
    let listener = UnixListener::bind(dir.path().join("boundary.sock")).unwrap();
    spec.prompt.text = "complete".into();
    let mut run = adapter.start_run(spec).await.unwrap();
    let mut connection = gate(&listener).await;
    // EOF proves the adapter committed to closing; the fake is still alive
    // behind this socket barrier, so Exited cannot have been published yet.
    while let Ok(event) = run.events.try_recv() {
        assert!(!matches!(event, HarnessEvent::Exited { .. }));
    }
    assert!(matches!(
        run.control
            .send_message(
                MessageRef(protocol::CommandId::new()),
                PromptInput {
                    text: "too late".into(),
                    images: vec![]
                },
                MessageDelivery::NextTurn
            )
            .await,
        Err(HarnessError::NotActive)
    ));
    connection.write_all(b"x").await.unwrap();
    assert_eq!(exit(&mut run).await, RunExit::Completed);
    assert!(
        !std::fs::read_to_string(dir.path().join("stdin.jsonl"))
            .unwrap()
            .contains("too late")
    );
}

#[tokio::test]
async fn queued_followup_runs_in_same_process_before_automatic_exit() {
    let (dir, adapter, mut spec) = fixture();
    let socket = dir.path().join("turn.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    spec.prompt.text = "complete".into();
    let mut run = adapter.start_run(spec).await.unwrap();
    let mut connection = gate(&listener).await;
    assert_eq!(
        run.control
            .send_message(
                MessageRef(protocol::CommandId::new()),
                PromptInput {
                    text: "later".into(),
                    images: vec![]
                },
                MessageDelivery::NextTurn
            )
            .await
            .unwrap(),
        DeliveryState::QueuedForBoundary
    );
    std::fs::remove_file(socket).unwrap();
    connection.write_all(b"x").await.unwrap();
    let mut turns = 0;
    loop {
        match event(&mut run.events).await {
            HarnessEvent::TurnCompleted { .. } => turns += 1,
            HarnessEvent::Exited { exit, .. } => {
                assert_eq!(exit, RunExit::Completed);
                break;
            }
            _ => {}
        }
    }
    assert_eq!(turns, 2);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("turns.jsonl"))
            .unwrap()
            .lines()
            .count(),
        2
    );
    let raw = std::fs::read_to_string(dir.path().join("stdin.jsonl")).unwrap();
    assert_eq!(raw.matches("later").count(), 1);
    assert!(dir.path().join("stdin-closed").exists());
}

async fn send(
    run: &RunStarted,
    text: &str,
    delivery: MessageDelivery,
) -> (MessageRef, Result<DeliveryState, HarnessError>) {
    let message_ref = MessageRef(protocol::CommandId::new());
    let input = PromptInput {
        text: text.into(),
        images: vec![],
    };
    (
        message_ref,
        run.control.send_message(message_ref, input, delivery).await,
    )
}

#[tokio::test]
async fn closed_admission_withdraws_a_held_followup_but_never_a_written_one() {
    let (dir, adapter, mut spec) = fixture();
    let socket = dir.path().join("turn.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    spec.prompt.text = "complete".into();
    let mut run = adapter.start_run(spec).await.unwrap();
    let mut connection = gate(&listener).await;
    let (written, state) = send(&run, "steered before stop", MessageDelivery::Steer).await;
    assert_eq!(state.unwrap(), DeliveryState::HostAccepted);
    let (held, state) = send(&run, "held until stop", MessageDelivery::NextTurn).await;
    assert_eq!(state.unwrap(), DeliveryState::QueuedForBoundary);
    run.control.close_admission();
    let (_, state) = send(&run, "sent after stop", MessageDelivery::NextTurn).await;
    assert!(matches!(state, Err(HarnessError::NotActive)));
    // The turn ends on its own before any interrupt reaches the adapter.
    std::fs::remove_file(socket).unwrap();
    connection.write_all(b"x").await.unwrap();
    let mut deliveries = Vec::new();
    loop {
        match event(&mut run.events).await {
            HarnessEvent::MessageDelivery { message_ref, state } => {
                deliveries.push((message_ref, state));
            }
            HarnessEvent::Exited { exit, .. } => {
                assert_eq!(exit, RunExit::Completed);
                break;
            }
            _ => {}
        }
    }
    assert_eq!(
        deliveries,
        [
            (written, DeliveryState::HostAccepted),
            (held, DeliveryState::Cancelled),
            (written, DeliveryState::NativeDelivered),
        ]
    );
    let raw = std::fs::read_to_string(dir.path().join("stdin.jsonl")).unwrap();
    assert!(raw.contains("steered before stop"));
    assert!(!raw.contains("held until stop") && !raw.contains("sent after stop"));
}

#[tokio::test]
async fn stop_before_the_prompt_is_written_interrupts_without_writing_it() {
    let (dir, adapter, mut spec) = fixture();
    spec.prompt.text = "complete".into();
    let mut run = adapter.start_run(spec).await.unwrap();
    // This single-threaded runtime has not polled the adapter's actor yet.
    run.control.close_admission();
    assert_eq!(exit(&mut run).await, RunExit::Interrupted);
    assert!(!dir.path().join("stdin.jsonl").exists());
}

#[tokio::test]
async fn stop_reaches_interrupted_without_external_shutdown() {
    let (_dir, adapter, mut spec) = fixture();
    spec.prompt.text = "wait".into();
    let mut run = adapter.start_run(spec).await.unwrap();
    while !matches!(event(&mut run.events).await, HarnessEvent::TurnStarted) {}
    run.control.interrupt().await.unwrap();
    assert_eq!(exit(&mut run).await, RunExit::Interrupted);
}

#[tokio::test]
async fn stop_without_native_completion_is_bounded_and_interrupted() {
    let (dir, adapter, mut spec) = fixture();
    std::fs::write(dir.path().join("ignore-interrupt"), "").unwrap();
    let listener = UnixListener::bind(dir.path().join("interrupt.sock")).unwrap();
    spec.prompt.text = "wait".into();
    let mut run = adapter.start_run(spec).await.unwrap();
    while !matches!(event(&mut run.events).await, HarnessEvent::TurnStarted) {}
    let control = run.control.clone();
    let interrupt = tokio::spawn(async move { control.interrupt().await });
    let _connection = gate(&listener).await;
    tokio::time::pause();
    tokio::time::advance(STOP_TIMEOUT + Duration::from_millis(1)).await;
    tokio::time::resume();
    assert_eq!(exit(&mut run).await, RunExit::Interrupted);
    assert!(interrupt.await.unwrap().is_err()); // No invented native acknowledgement.
}

#[tokio::test]
async fn shutdown_grace_kills_a_stalled_process_but_preserves_turn_status() {
    let (dir, adapter, mut spec) = fixture();
    let listener = UnixListener::bind(dir.path().join("boundary.sock")).unwrap();
    spec.prompt.text = "complete".into();
    let mut run = adapter.start_run(spec).await.unwrap();
    let mut connection = gate(&listener).await;
    tokio::time::pause();
    tokio::time::advance(SHUTDOWN_GRACE + Duration::from_millis(1)).await;
    tokio::time::resume();
    assert_eq!(exit(&mut run).await, RunExit::Completed);
    assert_eq!(connection.read(&mut [0]).await.unwrap(), 0);
}

#[tokio::test]
async fn resume_after_boundary_uses_new_run_and_same_native_session() {
    let (dir, adapter, mut spec) = fixture();
    spec.prompt.text = "complete".into();
    spec.resume_session = None;
    let mut first = adapter.start_run(spec.clone()).await.unwrap();
    let session = loop {
        if let HarnessEvent::SessionStarted { native_session, .. } = event(&mut first.events).await
        {
            break native_session;
        }
    };
    assert_eq!(exit(&mut first).await, RunExit::Completed);
    spec.run_id = protocol::RunId::new();
    spec.resume_session = Some(session.clone());
    let mut second = adapter.start_run(spec).await.unwrap();
    assert_eq!(exit(&mut second).await, RunExit::Completed);
    let raw = std::fs::read_to_string(dir.path().join("stdin.jsonl")).unwrap();
    let args: Vec<String> =
        serde_json::from_slice(&std::fs::read(dir.path().join("argv.json")).unwrap()).unwrap();
    assert!(
        args.windows(2)
            .any(|args| args == ["--resume", session.as_str()])
    );
    assert_eq!(raw.matches("complete").count(), 2);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("turns.jsonl"))
            .unwrap()
            .lines()
            .count(),
        2
    );
}

#[tokio::test]
async fn native_turn_status_overrides_process_exit_status() {
    for exit_code in [0, 7] {
        for (result, expected) in [
            (
                json!({"type":"result","subtype":"success","is_error":false}),
                RunExit::Completed,
            ),
            (
                json!({"type":"result","subtype":"error_during_execution","is_error":true,"errors":["fixture failure"]}),
                RunExit::Failed {
                    reason: "fixture failure".into(),
                },
            ),
            (
                json!({"type":"result","subtype":"success","is_error":true,"errors":["fixture failure"]}),
                RunExit::Failed {
                    reason: "fixture failure".into(),
                },
            ),
        ] {
            let (dir, adapter, mut spec) = fixture();
            std::fs::write(dir.path().join("exit-code"), exit_code.to_string()).unwrap();
            std::fs::write(dir.path().join("result.json"), result.to_string()).unwrap();
            spec.prompt.text = "complete".into();
            let mut run = adapter.start_run(spec).await.unwrap();
            assert_eq!(exit(&mut run).await, expected);
        }
    }
}

#[tokio::test]
async fn unknown_native_status_is_never_success() {
    let (dir, adapter, mut spec) = fixture();
    std::fs::write(
        dir.path().join("result.json"),
        json!({"type":"result","subtype":"future_status","is_error":false}).to_string(),
    )
    .unwrap();
    spec.prompt.text = "complete".into();
    let mut run = adapter.start_run(spec).await.unwrap();
    assert!(matches!(exit(&mut run).await, RunExit::Unknown { .. }));
}

#[tokio::test]
async fn exit_after_turn_fixture_remains_supported() {
    let (dir, adapter, mut spec) = fixture();
    std::fs::write(dir.path().join("exit-after-turn"), "").unwrap();
    spec.prompt.text = "complete".into();
    let mut run = adapter.start_run(spec).await.unwrap();
    assert_eq!(exit(&mut run).await, RunExit::Completed);
    assert!(!dir.path().join("stdin-closed").exists());
}

#[tokio::test]
async fn acknowledged_stop_without_turn_completion_is_still_bounded() {
    let (dir, adapter, mut spec) = fixture();
    std::fs::write(dir.path().join("ignore-completion"), "").unwrap();
    let listener = UnixListener::bind(dir.path().join("interrupt.sock")).unwrap();
    spec.prompt.text = "wait".into();
    let mut run = adapter.start_run(spec).await.unwrap();
    while !matches!(event(&mut run.events).await, HarnessEvent::TurnStarted) {}
    run.control.interrupt().await.unwrap();
    let _connection = gate(&listener).await;
    tokio::time::pause();
    tokio::time::advance(STOP_TIMEOUT + Duration::from_millis(1)).await;
    tokio::time::resume();
    assert_eq!(exit(&mut run).await, RunExit::Interrupted);
}

#[tokio::test]
async fn late_interrupt_acknowledgement_cannot_interrupt_the_next_turn() {
    let (dir, adapter, mut spec) = fixture();
    std::fs::write(dir.path().join("late-interrupt-ack"), "").unwrap();
    spec.prompt.text = "wait".into();
    let mut run = adapter.start_run(spec).await.unwrap();
    while !matches!(event(&mut run.events).await, HarnessEvent::TurnStarted) {}
    assert_eq!(
        run.control
            .send_message(
                MessageRef(protocol::CommandId::new()),
                PromptInput {
                    text: "later".into(),
                    images: vec![]
                },
                MessageDelivery::InterruptAndSend
            )
            .await
            .unwrap(),
        DeliveryState::QueuedForBoundary
    );
    assert_eq!(exit(&mut run).await, RunExit::Completed);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("turns.jsonl"))
            .unwrap()
            .lines()
            .count(),
        2
    );
}

#[tokio::test]
async fn process_exit_cannot_certify_an_undelivered_queued_turn() {
    let (dir, adapter, mut spec) = fixture();
    std::fs::write(dir.path().join("exit-after-turn"), "").unwrap();
    let path = dir.path().join("turn.sock");
    let listener = UnixListener::bind(&path).unwrap();
    spec.prompt.text = "complete".into();
    let mut run = adapter.start_run(spec).await.unwrap();
    let mut connection = gate(&listener).await;
    assert_eq!(
        run.control
            .send_message(
                MessageRef(protocol::CommandId::new()),
                PromptInput {
                    text: "later".into(),
                    images: vec![]
                },
                MessageDelivery::NextTurn
            )
            .await
            .unwrap(),
        DeliveryState::QueuedForBoundary
    );
    connection.write_all(b"x").await.unwrap();
    assert!(matches!(
        exit(&mut run).await,
        RunExit::Failed { .. } | RunExit::Unknown { .. }
    ));
}

#[tokio::test]
async fn force_stop_after_natural_exit_confirms_the_already_ended_actor() {
    let (_dir, adapter, mut spec) = fixture();
    spec.prompt.text = "complete".into();
    let mut run = adapter.start_run(spec).await.unwrap();
    assert_eq!(exit(&mut run).await, RunExit::Completed);
    run.control.terminate().await.unwrap();
    run.control.terminate().await.unwrap();
}
