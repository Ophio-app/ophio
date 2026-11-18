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
    // Claude Code would read the written message in a turn of its own.
    let _echo = UnixListener::bind(dir.path().join("echo.sock")).unwrap();
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
    let mut started = 0;
    let mut completed = 0;
    loop {
        match event(&mut run.events).await {
            HarnessEvent::MessageDelivery { message_ref, state } => {
                deliveries.push((message_ref, state));
            }
            HarnessEvent::TurnStarted => started += 1,
            HarnessEvent::TurnCompleted { .. } => completed += 1,
            HarnessEvent::Exited { exit, .. } => {
                assert_eq!(exit, RunExit::Completed);
                break;
            }
            _ => {}
        }
    }
    assert_eq!((started, completed), (1, 1));
    // After Stop no turn starts on the written message: it is not claimed
    // as read, and never reported withdrawn.
    assert_eq!(
        deliveries,
        [
            (written, DeliveryState::HostAccepted),
            (held, DeliveryState::Cancelled),
            (written, DeliveryState::Unknown),
        ]
    );
    // HostAccepted is the written proof. The fixture logs a line only once it
    // reads it, and the process may be ended before it reads the steer.
    let raw = std::fs::read_to_string(dir.path().join("stdin.jsonl")).unwrap();
    assert!(!raw.contains("held until stop") && !raw.contains("sent after stop"));
    assert_eq!(turns(&dir), 1);
}

fn turns(dir: &tempfile::TempDir) -> usize {
    std::fs::read_to_string(dir.path().join("turns.jsonl"))
        .map(|turns| turns.lines().count())
        .unwrap_or(0)
}

/// Reads events up to the first turn's end, keeping its deliveries.
async fn first_turn(run: &mut RunStarted, deliveries: &mut Vec<(MessageRef, DeliveryState)>) {
    loop {
        match event(&mut run.events).await {
            HarnessEvent::MessageDelivery { message_ref, state } => {
                deliveries.push((message_ref, state));
            }
            HarnessEvent::TurnCompleted { .. } => return,
            HarnessEvent::Exited { exit, .. } => panic!("run ended early: {exit:?}"),
            _ => {}
        }
    }
}

/// Reads events to the run's end: its exit and how many turns completed.
async fn rest(
    run: &mut RunStarted,
    deliveries: &mut Vec<(MessageRef, DeliveryState)>,
) -> (RunExit, usize) {
    let mut turns = 0;
    loop {
        match event(&mut run.events).await {
            HarnessEvent::MessageDelivery { message_ref, state } => {
                deliveries.push((message_ref, state));
            }
            HarnessEvent::TurnCompleted { .. } => turns += 1,
            HarnessEvent::Exited { exit, .. } => return (exit, turns),
            _ => {}
        }
    }
}

/// A steer written while a turn runs and read inside it (echoed before the
/// result) is confirmed, and the run ends at that turn as before.
#[tokio::test]
async fn a_steer_read_inside_the_turn_ends_with_that_turn() {
    let (dir, adapter, mut spec) = fixture();
    let socket = dir.path().join("turn.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    spec.prompt.text = "wait".into();
    let mut run = adapter.start_run(spec).await.unwrap();
    while !matches!(event(&mut run.events).await, HarnessEvent::TurnStarted) {}
    let (steer, state) = send(&run, "while you work", MessageDelivery::Steer).await;
    assert_eq!(state.unwrap(), DeliveryState::HostAccepted);
    // The fixture reads the steer and answers it within the running turn.
    let mut connection = gate(&listener).await;
    std::fs::remove_file(socket).unwrap();
    connection.write_all(b"x").await.unwrap();
    let mut deliveries = Vec::new();
    let (exit, completed) = rest(&mut run, &mut deliveries).await;
    assert_eq!(exit, RunExit::Completed);
    assert_eq!(completed, 1);
    assert_eq!(
        deliveries,
        [
            (steer, DeliveryState::HostAccepted),
            (steer, DeliveryState::NativeDelivered),
        ]
    );
    assert!(dir.path().join("stdin-closed").exists());
}

/// A steer that missed the turn's last step is answered in a turn Claude
/// Code starts by itself. The run stays open for it past the shutdown grace,
/// and a held follow-up goes only after it.
#[tokio::test]
async fn a_steer_after_the_last_step_gets_its_own_turn_before_a_held_followup() {
    for with_followup in [false, true] {
        let (dir, adapter, mut spec) = fixture();
        let socket = dir.path().join("turn.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let echo_socket = dir.path().join("echo.sock");
        let echo = UnixListener::bind(&echo_socket).unwrap();
        spec.prompt.text = "complete".into();
        let mut run = adapter.start_run(spec).await.unwrap();
        let mut connection = gate(&listener).await;
        let (steer, state) = send(&run, "steered late", MessageDelivery::Steer).await;
        assert_eq!(state.unwrap(), DeliveryState::HostAccepted);
        let held = if with_followup {
            let (held, state) = send(&run, "held after", MessageDelivery::NextTurn).await;
            assert_eq!(state.unwrap(), DeliveryState::QueuedForBoundary);
            Some(held)
        } else {
            None
        };
        std::fs::remove_file(socket).unwrap();
        connection.write_all(b"x").await.unwrap();
        let mut deliveries = Vec::new();
        first_turn(&mut run, &mut deliveries).await;
        // Claude Code is slow to start the next turn: longer than the grace
        // a finished run gets, which would have ended it.
        let mut echo_connection = gate(&echo).await;
        std::fs::remove_file(echo_socket).unwrap();
        tokio::time::pause();
        tokio::time::advance(SHUTDOWN_GRACE + Duration::from_millis(1)).await;
        tokio::time::resume();
        tokio::task::yield_now().await;
        while let Ok(event) = run.events.try_recv() {
            assert!(!matches!(event, HarnessEvent::Exited { .. }));
        }
        assert!(!dir.path().join("stdin-closed").exists());
        echo_connection.write_all(b"x").await.unwrap();
        let (exit, completed) = rest(&mut run, &mut deliveries).await;
        assert_eq!(exit, RunExit::Completed);
        let mut expected = vec![
            (steer, DeliveryState::HostAccepted),
            (steer, DeliveryState::NativeDelivered),
        ];
        if let Some(held) = held {
            expected.extend([
                (held, DeliveryState::HostAccepted),
                (held, DeliveryState::NativeDelivered),
            ]);
        }
        assert_eq!(deliveries, expected);
        assert_eq!(completed, if with_followup { 2 } else { 1 });
        assert_eq!(turns(&dir), if with_followup { 3 } else { 2 });
        let raw = std::fs::read_to_string(dir.path().join("stdin.jsonl")).unwrap();
        assert_eq!(raw.matches("steered late").count(), 1);
        if with_followup {
            assert!(raw.find("steered late").unwrap() < raw.find("held after").unwrap());
        }
        assert!(dir.path().join("stdin-closed").exists());
    }
}

/// When the turn for a steer that missed the last step never starts, the
/// wait ends: the steer is not claimed as read and the run ends as the
/// finished turn would have.
#[tokio::test]
async fn a_steer_whose_turn_never_starts_is_left_unknown_and_the_run_ends() {
    let (dir, adapter, mut spec) = fixture();
    let socket = dir.path().join("turn.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let echo = UnixListener::bind(dir.path().join("echo.sock")).unwrap();
    spec.prompt.text = "complete".into();
    let mut run = adapter.start_run(spec).await.unwrap();
    let mut connection = gate(&listener).await;
    let (steer, state) = send(&run, "never read", MessageDelivery::Steer).await;
    assert_eq!(state.unwrap(), DeliveryState::HostAccepted);
    connection.write_all(b"x").await.unwrap();
    let mut deliveries = Vec::new();
    first_turn(&mut run, &mut deliveries).await;
    // The fixture took the message in and never reads it.
    let _held = gate(&echo).await;
    tokio::time::pause();
    tokio::time::advance(CARRY_WAIT + Duration::from_millis(1)).await;
    tokio::time::resume();
    loop {
        match event(&mut run.events).await {
            HarnessEvent::MessageDelivery { message_ref, state } => {
                deliveries.push((message_ref, state));
                break;
            }
            HarnessEvent::Exited { exit, .. } => panic!("run ended early: {exit:?}"),
            _ => {}
        }
    }
    assert_eq!(
        deliveries,
        [
            (steer, DeliveryState::HostAccepted),
            (steer, DeliveryState::Unknown),
        ]
    );
    // Input is closed as at any turn end; the stalled process is then ended.
    tokio::time::pause();
    tokio::time::advance(SHUTDOWN_GRACE + Duration::from_millis(1)).await;
    tokio::time::resume();
    assert_eq!(exit(&mut run).await, RunExit::Completed);
    assert_eq!(turns(&dir), 1);
}

/// Stop while Claude Code is about to start a turn on a steer that missed
/// the last step: that turn never runs.
#[tokio::test]
async fn stop_before_a_carried_steer_is_read_runs_nothing_more() {
    let (dir, adapter, mut spec) = fixture();
    let socket = dir.path().join("turn.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let echo = UnixListener::bind(dir.path().join("echo.sock")).unwrap();
    spec.prompt.text = "complete".into();
    let mut run = adapter.start_run(spec).await.unwrap();
    let mut connection = gate(&listener).await;
    let (steer, state) = send(&run, "steered late", MessageDelivery::Steer).await;
    assert_eq!(state.unwrap(), DeliveryState::HostAccepted);
    let (held, state) = send(&run, "held after", MessageDelivery::NextTurn).await;
    assert_eq!(state.unwrap(), DeliveryState::QueuedForBoundary);
    connection.write_all(b"x").await.unwrap();
    let mut deliveries = Vec::new();
    first_turn(&mut run, &mut deliveries).await;
    let _held = gate(&echo).await;
    run.control.close_admission();
    run.control.interrupt().await.unwrap();
    let (exit, _) = rest(&mut run, &mut deliveries).await;
    assert_eq!(exit, RunExit::Interrupted);
    assert_eq!(
        deliveries,
        [
            (steer, DeliveryState::HostAccepted),
            (held, DeliveryState::Cancelled),
            (steer, DeliveryState::Unknown),
        ]
    );
    assert_eq!(turns(&dir), 1);
    let raw = std::fs::read_to_string(dir.path().join("stdin.jsonl")).unwrap();
    assert!(!raw.contains("held after"));
}

/// Ophio shutting down while the run waits for a turn Claude Code has not
/// begun: the finished reply stands and the steer is not claimed as read.
#[tokio::test]
async fn shutdown_while_a_carried_steer_waits_keeps_the_finished_reply() {
    let (dir, adapter, mut spec) = fixture();
    let socket = dir.path().join("turn.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let echo = UnixListener::bind(dir.path().join("echo.sock")).unwrap();
    spec.prompt.text = "complete".into();
    let mut run = adapter.start_run(spec).await.unwrap();
    let mut connection = gate(&listener).await;
    let (steer, state) = send(&run, "steered late", MessageDelivery::Steer).await;
    assert_eq!(state.unwrap(), DeliveryState::HostAccepted);
    let (_held, state) = send(&run, "held after", MessageDelivery::NextTurn).await;
    assert_eq!(state.unwrap(), DeliveryState::QueuedForBoundary);
    connection.write_all(b"x").await.unwrap();
    let mut deliveries = Vec::new();
    first_turn(&mut run, &mut deliveries).await;
    let _unread = gate(&echo).await;
    let control = run.control.clone();
    let shutdown = tokio::spawn(async move { control.shutdown().await });
    loop {
        match event(&mut run.events).await {
            HarnessEvent::MessageDelivery { message_ref, state } => {
                deliveries.push((message_ref, state));
                break;
            }
            HarnessEvent::Exited { exit, .. } => panic!("run ended early: {exit:?}"),
            _ => {}
        }
    }
    // The stalled process is ended after the usual grace.
    tokio::time::pause();
    tokio::time::advance(SHUTDOWN_GRACE + Duration::from_millis(1)).await;
    tokio::time::resume();
    let (exit, completed) = rest(&mut run, &mut deliveries).await;
    assert_eq!(exit, RunExit::Completed);
    assert_eq!(completed, 0);
    shutdown.await.unwrap().unwrap();
    // The held follow-up was never written, so it is not given an outcome here.
    assert_eq!(
        deliveries,
        [
            (steer, DeliveryState::HostAccepted),
            (steer, DeliveryState::Unknown),
        ]
    );
    assert_eq!(turns(&dir), 1);
    let raw = std::fs::read_to_string(dir.path().join("stdin.jsonl")).unwrap();
    assert!(!raw.contains("held after"));
}

/// Claude Code exiting before it reads a steer that missed the last step:
/// the finished reply stands and the steer is not claimed as read.
#[tokio::test]
async fn claude_code_exiting_while_a_carried_steer_waits_keeps_the_finished_reply() {
    for exit_code in [0, 3] {
        let (dir, adapter, mut spec) = fixture();
        let socket = dir.path().join("turn.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        std::fs::write(dir.path().join("exit-after-turn"), "").unwrap();
        std::fs::write(dir.path().join("exit-code"), exit_code.to_string()).unwrap();
        spec.prompt.text = "complete".into();
        let mut run = adapter.start_run(spec).await.unwrap();
        let mut connection = gate(&listener).await;
        let (steer, state) = send(&run, "steered late", MessageDelivery::Steer).await;
        assert_eq!(state.unwrap(), DeliveryState::HostAccepted);
        connection.write_all(b"x").await.unwrap();
        let mut deliveries = Vec::new();
        let (exit, completed) = rest(&mut run, &mut deliveries).await;
        assert_eq!(exit, RunExit::Completed, "exit code {exit_code}");
        assert_eq!(completed, 1);
        assert_eq!(
            deliveries,
            [
                (steer, DeliveryState::HostAccepted),
                (steer, DeliveryState::Unknown),
            ]
        );
        assert_eq!(turns(&dir), 1);
    }
}

/// The echo is recognised by its id alone: one whose content is a plain
/// string, or has no text part, still confirms the message, and the run
/// ends at once instead of waiting for a turn that already happened.
#[tokio::test]
async fn an_echo_of_any_shape_confirms_the_message() {
    for shape in ["string", "no-text"] {
        let (dir, adapter, mut spec) = fixture();
        let socket = dir.path().join("turn.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        std::fs::write(dir.path().join("echo-shape"), shape).unwrap();
        spec.prompt.text = "complete".into();
        let mut run = adapter.start_run(spec).await.unwrap();
        let mut connection = gate(&listener).await;
        let (held, state) = send(&run, "held after", MessageDelivery::NextTurn).await;
        assert_eq!(state.unwrap(), DeliveryState::QueuedForBoundary);
        std::fs::remove_file(socket).unwrap();
        connection.write_all(b"x").await.unwrap();
        let mut deliveries = Vec::new();
        let (exit, completed) = rest(&mut run, &mut deliveries).await;
        assert_eq!(exit, RunExit::Completed, "{shape}");
        assert_eq!(completed, 2, "{shape}");
        assert_eq!(
            deliveries,
            [
                (held, DeliveryState::HostAccepted),
                (held, DeliveryState::NativeDelivered),
            ],
            "{shape}"
        );
        assert!(dir.path().join("stdin-closed").exists());
    }
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
