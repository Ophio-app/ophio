// SPDX-License-Identifier: Apache-2.0
use super::*;

fn background(mode: &str) -> (tempfile::TempDir, ClaudeAdapter, RunSpec) {
    let (dir, _, mut spec) = fixture();
    let binary = dir.path().join("background-claude");
    test_support::executable(
        &binary,
        include_str!("../../../test-support/fixtures/claude_background.py"),
    )
    .unwrap();
    spec.prompt.text = mode.into();
    (dir, ClaudeAdapter::with_binary(binary), spec)
}
async fn parent_ended(run: &mut RunStarted) {
    while !matches!(
        event(&mut run.events).await,
        HarnessEvent::ParentEnded { .. }
    ) {}
}
#[tokio::test]
async fn completed_parent_keeps_both_children_visible_and_independently_stoppable() {
    let (_dir, adapter, spec) = background("complete");
    let mut run = adapter.start_run(spec).await.unwrap();
    parent_ended(&mut run).await;
    assert!(run.control.child_stop_availability("job-1").is_available());
    assert_eq!(
        run.control.stop_child("job-1").await.unwrap().state,
        ChildState::Stopped
    );
    assert!(run.control.child_stop_availability("job-2").is_available());
    assert_eq!(
        run.control.stop_child("job-2").await.unwrap().state,
        ChildState::Stopped
    );
    while !matches!(event(&mut run.events).await, HarnessEvent::Exited { .. }) {}
    assert!(run.events.recv().await.is_none());
}
#[tokio::test]
async fn stop_interrupts_then_stops_every_live_background_task() {
    let (dir, adapter, spec) = background("wait");
    let mut run = adapter.start_run(spec).await.unwrap();
    while !matches!(event(&mut run.events).await, HarnessEvent::Child { native_ref, .. } if native_ref == "job-2")
    {
    }
    run.control.interrupt().await.unwrap();
    let mut stopped = Vec::new();
    loop {
        match event(&mut run.events).await {
            HarnessEvent::Child {
                native_ref,
                state: ChildState::Stopped,
                ..
            } => stopped.push(native_ref),
            HarnessEvent::Exited {
                exit: RunExit::Interrupted,
                ..
            } => break,
            _ => {}
        }
    }
    stopped.sort();
    assert_eq!(stopped, ["job-1", "job-2"]);
    let requests: Vec<Value> = std::fs::read_to_string(dir.path().join("controls.jsonl"))
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(requests[0]["subtype"], "interrupt");
    assert_eq!(
        requests
            .iter()
            .filter(|r| r["subtype"] == "stop_task")
            .count(),
        2
    );
}
#[tokio::test]
async fn an_ignored_stop_remains_active_and_does_not_block_another_child() {
    let (_dir, adapter, spec) = background("ignore");
    let mut run = adapter.start_run(spec).await.unwrap();
    parent_ended(&mut run).await;
    let (ignored, stopped) = tokio::join!(
        run.control.stop_child("job-2"),
        run.control.stop_child("job-1")
    );
    assert_eq!(ignored.unwrap().state, ChildState::Active);
    assert_eq!(stopped.unwrap().state, ChildState::Stopped);
    assert!(matches!(
        run.control.stop_child("unreported").await,
        Err(HarnessError::Unsupported(_))
    ));
    run.control.shutdown().await.unwrap();
}
#[tokio::test]
async fn natural_completion_is_not_labelled_stopped_and_exit_is_not_cleanup_proof() {
    for mode in ["natural", "exit"] {
        let (_dir, adapter, spec) = background(mode);
        let mut run = adapter.start_run(spec).await.unwrap();
        let mut completed = 0;
        loop {
            match event(&mut run.events).await {
                HarnessEvent::Child {
                    state: ChildState::Completed,
                    ..
                } => completed += 1,
                HarnessEvent::Child {
                    state: ChildState::Stopped,
                    ..
                } => panic!("unsolicited stop"),
                HarnessEvent::Exited { .. } => break,
                _ => {}
            }
        }
        assert_eq!(completed, if mode == "natural" { 2 } else { 0 });
        assert!(!run.control.child_stop_availability("job-1").is_available());
    }
}
#[tokio::test]
async fn a_resumed_session_answers_after_reporting_lost_background_work() {
    let (_dir, adapter, spec) = background("resumed");
    let mut run = adapter.start_run(spec).await.unwrap();
    let (mut replied, mut usage) = (false, None);
    let exit = loop {
        match event(&mut run.events).await {
            HarnessEvent::AgentMessage { text, .. } => replied = text == "Carrying on",
            HarnessEvent::TurnCompleted { usage: u } => usage = u,
            HarnessEvent::Exited { exit, .. } => break exit,
            _ => {}
        }
    };
    assert!(replied, "the queued message was never answered");
    assert_eq!(usage.unwrap().output_tokens, Some(4));
    assert!(matches!(exit, RunExit::Completed), "{exit:?}");
}
