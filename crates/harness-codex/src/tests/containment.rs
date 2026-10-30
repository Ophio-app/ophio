// SPDX-License-Identifier: Apache-2.0
//! Stop, the end of a run and shutdown end what the agent left running,
//! however it moved that work into the background.
#![cfg(target_os = "linux")]
use super::*;
use test_support::detached;

async fn detaching(mode: &str) -> (tempfile::TempDir, RunStarted, Vec<(&'static str, u32)>) {
    let (dir, adapter, mut spec) = fixture();
    std::fs::write(dir.path().join("detach.sh"), detached::SCRIPT).unwrap();
    spec.prompt.text = format!("detach {mode}");
    // A finishing turn waits until every child is seen running.
    let gate = (mode == "complete")
        .then(|| tokio::net::UnixListener::bind(dir.path().join("turn.sock")).unwrap());
    let mut run = adapter.start_run(spec).await.unwrap();
    while !matches!(event(&mut run.events).await, HarnessEvent::TurnStarted) {}
    let children = detached::children(dir.path());
    for (way, pid) in &children {
        assert!(detached::running(*pid), "{way} child isn't running");
    }
    if let Some(gate) = gate {
        use tokio::io::AsyncWriteExt;
        gate.accept()
            .await
            .unwrap()
            .0
            .write_all(b"g")
            .await
            .unwrap();
    }
    (dir, run, children)
}

async fn exit(run: &mut RunStarted) -> RunExit {
    loop {
        if let HarnessEvent::Exited { exit, .. } = event(&mut run.events).await {
            return exit;
        }
    }
}

fn complete(run: &RunStarted) -> bool {
    let containment = run.control.containment().unwrap();
    println!("containment: {containment:?}");
    containment == Containment::Complete
}

#[tokio::test(flavor = "multi_thread")]
async fn stop_ends_what_the_agent_left_running() {
    let (_dir, mut run, children) = detaching("wait").await;
    run.control.interrupt().await.unwrap();
    assert_eq!(exit(&mut run).await, RunExit::Interrupted);
    detached::assert_ended(complete(&run), &children);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_finished_run_ends_what_the_agent_left_running() {
    let (_dir, mut run, children) = detaching("complete").await;
    assert_eq!(exit(&mut run).await, RunExit::Completed);
    detached::assert_ended(complete(&run), &children);
}

#[tokio::test(flavor = "multi_thread")]
async fn shutdown_ends_what_the_agent_left_running() {
    let (_dir, mut run, children) = detaching("wait").await;
    run.control.shutdown().await.unwrap();
    exit(&mut run).await;
    detached::assert_ended(complete(&run), &children);
}

#[tokio::test(flavor = "multi_thread")]
async fn force_stop_confirms_that_nothing_is_left() {
    let (_dir, run, children) = detaching("wait").await;
    run.control.terminate().await.unwrap();
    detached::assert_ended(complete(&run), &children);
}

/// Reads the run to its end, which must come with nothing left unconfirmed.
async fn confirmed_exit(run: &mut RunStarted) -> RunExit {
    loop {
        match event(&mut run.events).await {
            HarnessEvent::Warning { text } if text == ::containment::STILL_RUNNING => {
                panic!("the run's end wasn't confirmed")
            }
            HarnessEvent::CleanupPending => panic!("the run's end wasn't confirmed"),
            HarnessEvent::Exited { exit, .. } => return exit,
            _ => {}
        }
    }
}

// With only the agent's process group, the agent is reaped only once its
// group is confirmed ended: the group is never signalled by an id another
// could have taken, and the unreaped agent doesn't hold its end open.
#[tokio::test(flavor = "multi_thread")]
async fn a_finished_run_with_only_a_process_group_ends_what_it_left_there() {
    let (_dir, mut run, children) =
        ::containment::hooks::process_group(detaching("complete")).await;
    assert_eq!(confirmed_exit(&mut run).await, RunExit::Completed);
    assert!(!complete(&run));
    detached::assert_ended(false, &children);
}

#[tokio::test(flavor = "multi_thread")]
async fn stop_with_only_a_process_group_ends_what_the_agent_left_there() {
    let (_dir, mut run, children) = ::containment::hooks::process_group(detaching("wait")).await;
    run.control.interrupt().await.unwrap();
    assert_eq!(confirmed_exit(&mut run).await, RunExit::Interrupted);
    detached::assert_ended(false, &children);
}

#[tokio::test(flavor = "multi_thread")]
async fn force_stop_with_only_a_process_group_confirms_its_end() {
    let (_dir, run, children) = ::containment::hooks::process_group(detaching("wait")).await;
    run.control.terminate().await.unwrap();
    detached::assert_ended(false, &children);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_run_whose_end_is_unconfirmed_stays_open_until_it_is() {
    let (_dir, adapter, mut spec) = fixture();
    spec.prompt.text = "complete".into();
    // Its first three ends go unconfirmed: the run's own, Force stop's, and
    // the first try again.
    let mut run = ::containment::hooks::unconfirmed(3, adapter.start_run(spec))
        .await
        .unwrap();
    loop {
        match event(&mut run.events).await {
            HarnessEvent::Warning { text } if text == ::containment::STILL_RUNNING => break,
            HarnessEvent::Exited { .. } => panic!("the run closed before its end was confirmed"),
            _ => {}
        }
    }
    assert_eq!(event(&mut run.events).await, HarnessEvent::CleanupPending);
    assert!(run.control.terminate().await.is_err());
    let last = tokio::time::timeout(Duration::from_secs(15), run.events.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        last,
        HarnessEvent::Exited {
            exit: RunExit::Completed,
            outcome: None
        }
    );
    run.control.terminate().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stop_while_the_end_is_unconfirmed_tries_again_at_once() {
    let (_dir, adapter, mut spec) = fixture();
    spec.prompt.text = "complete".into();
    // The run's own end goes unconfirmed, and it would wait an hour to try
    // again.
    let mut run =
        ::containment::hooks::unconfirmed(1, ::containment::hooks::held(adapter.start_run(spec)))
            .await
            .unwrap();
    loop {
        match event(&mut run.events).await {
            HarnessEvent::CleanupPending => break,
            HarnessEvent::Exited { .. } => panic!("the run closed before its end was confirmed"),
            _ => {}
        }
    }
    assert!(matches!(
        run.control.interrupt().await,
        Err(HarnessError::NotActive)
    ));
    let last = tokio::time::timeout(Duration::from_secs(10), run.events.recv())
        .await
        .expect("Stop didn't have the end tried again")
        .unwrap();
    assert_eq!(
        last,
        HarnessEvent::Exited {
            exit: RunExit::Completed,
            outcome: None
        }
    );
}
