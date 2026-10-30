// SPDX-License-Identifier: Apache-2.0
//! What waits on the person stops waiting once its task has moved on,
//! whichever way it moved.
use super::*;
use crate::github_watch::{headline_item, raise_item};
use ports::Clock;
use protocol::{event::Event, notifications::AlertTopic};

fn task(f: &Fixture, task_id: TaskId) -> Task {
    f.hub.read(|r| r.require("task", task_id)).unwrap()
}

fn open(f: &Fixture, id: AttentionId) -> bool {
    let item: AttentionItem = f.hub.read(|r| r.require("attention", id)).unwrap();
    item.resolved_at.is_none()
}

/// A task whose first run has ended, and that run.
async fn finished(f: &Fixture) -> (TaskId, RunId) {
    let project = f.project().await;
    let (task_id, _) = f
        .task(
            project,
            TaskPlacement::StartNow,
            &Caller::local_owner(),
            false,
        )
        .await;
    let run = run_started(f, task_id).await;
    let exited = ports::HarnessEvent::Exited {
        exit: ports::RunExit::Completed,
        outcome: None,
    };
    f.harness.emit(run, exited).await;
    let mut more = waiting();
    while task(f, task_id).state != TaskState::Completed {
        assert!(more(), "the run didn't end");
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    (task_id, run)
}

/// Asks the person about the task and not one of its runs, as a pull
/// request held for them does.
fn ask(f: &Fixture, task_id: TaskId) -> AttentionId {
    let item = headline_item(
        task_id,
        AttentionKind::WaitingForYou,
        AlertTopic::PrNeedsYou,
        "PR #7 needs you",
        None,
        f.clock.now(),
    );
    let id = item.attention_id;
    f.hub.write(|tx| raise_item(tx, item)).unwrap();
    assert!(open(f, id));
    id
}

#[tokio::test]
async fn archiving_a_task_ends_its_wait_on_the_person() {
    let f = Fixture::new().await;
    let (task_id, _) = finished(&f).await;
    let before = task(&f, task_id).attention_count;
    let asked = ask(&f, task_id);
    assert_eq!(task(&f, task_id).attention_count, before + 1);
    let archive = edit(Command::ArchiveTask { task_id }, task(&f, task_id).revision);
    let receipt = f.hub.submit(&Caller::local_owner(), archive).await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    assert!(!open(&f, asked));
    assert_eq!(task(&f, task_id).attention_count, 0);
    f.close().await;
}

fn open_about(f: &Fixture, task_id: TaskId) -> Vec<AttentionItem> {
    let items: Vec<AttentionItem> = f.hub.read(|r| r.list("attention")).unwrap();
    (items.into_iter())
        .filter(|a| a.task_id == Some(task_id) && a.resolved_at.is_none())
        .collect()
}

#[tokio::test]
async fn archiving_a_task_clears_its_notifications() {
    let f = Fixture::new().await;
    let (task_id, _) = finished(&f).await;
    let finished = open_about(&f, task_id);
    assert!(
        finished.iter().any(|a| a.kind == AttentionKind::Completed),
        "{finished:?}"
    );
    let archive = edit(Command::ArchiveTask { task_id }, task(&f, task_id).revision);
    let receipt = f.hub.submit(&Caller::local_owner(), archive).await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    assert_eq!(open_about(&f, task_id), []);
    f.close().await;
}

#[tokio::test]
async fn starting_clears_what_archived_tasks_left_showing() {
    let f = Fixture::new().await;
    let (task_id, _) = finished(&f).await;
    // As versions before archiving cleared them left it.
    f.hub
        .write(|tx| {
            let mut t: Task = tx.read().require("task", task_id)?;
            t.state = TaskState::Archived;
            tx.put("task", task_id, &t)
        })
        .unwrap();
    assert_ne!(open_about(&f, task_id), []);
    f.hub
        .write(|tx| crate::runs::settle_archived_attention(tx, f.clock.now()))
        .unwrap();
    assert_eq!(open_about(&f, task_id), []);
    assert_eq!(task(&f, task_id).attention_count, 0);
    f.close().await;
}

#[tokio::test]
async fn starting_a_task_again_ends_its_wait_on_the_person() {
    let f = Fixture::new().await;
    let (task_id, run) = finished(&f).await;
    let asked = ask(&f, task_id);
    // Asked during a turn, it waits past that turn.
    f.clock.advance(Duration::from_secs(60));
    assert!(open(&f, asked));
    let resume = Command::ResumeTask {
        task_id,
        mode: None,
        model: None,
        effort: None,
        text: Some("Carry on".into()),
        bundle: None,
        carried_plan: None,
        read_only: false,
    };
    let resume = edit(resume, task(&f, task_id).revision);
    let receipt = f.hub.submit(&Caller::local_owner(), resume).await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    assert_ne!(task(&f, task_id).current_run, Some(run));
    assert!(!open(&f, asked));
    f.close().await;
}

#[tokio::test]
async fn putting_a_task_in_next_up_ends_its_wait_on_the_person() {
    let f = Fixture::new().await;
    let (task_id, _) = finished(&f).await;
    let asked = ask(&f, task_id);
    f.clock.advance(Duration::from_secs(60));
    let queue = Command::QueueContinuation {
        task_id,
        text: "Then the docs".into(),
        bundle: None,
        authorization: QueueAuthorization::AskBeforeStart,
    };
    let queue = edit(queue, task(&f, task_id).revision);
    let receipt = f.hub.submit(&Caller::local_owner(), queue).await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    assert!(!open(&f, asked));
    assert_eq!(task(&f, task_id).attention_count, 0);
    f.close().await;
}

#[tokio::test]
async fn an_approval_that_expires_stops_waiting_on_the_person() {
    use ports::bridge::{BridgeHandler, BridgeRequest, RUN_TOKEN_ENV};
    let f = Fixture::new().await;
    let project = f.project().await;
    let (task_id, _) = f
        .task(
            project,
            TaskPlacement::StartNow,
            &Caller::local_owner(),
            false,
        )
        .await;
    run_started(&f, task_id).await;
    let token = (f.harness.specs.lock().unwrap()[0]
        .bridge
        .as_ref()
        .unwrap()
        .env
        .iter())
    .find(|(k, _)| k == RUN_TOKEN_ENV)
    .unwrap()
    .1
    .clone();
    let binding = f.hub.bind(&token).await.unwrap();
    let hub = f.hub.clone();
    let pending = tokio::spawn(async move {
        let prompt = BridgeRequest::PermissionPrompt {
            tool_name: "Bash".into(),
            input: serde_json::json!({"command": "fixture"}),
            tool_use_id: None,
            native_request_id: Some("2".into()),
        };
        hub.handle(&binding, prompt).await
    });
    let mut more = waiting();
    let asked = loop {
        let items: Vec<AttentionItem> = f.hub.read(|r| r.list("attention")).unwrap();
        if let Some(item) = items.into_iter().find(|a| a.approval_id.is_some()) {
            break item;
        }
        assert!(more(), "nothing was asked");
        tokio::time::sleep(Duration::from_millis(1)).await;
    };
    assert_eq!(task(&f, task_id).attention_count, 1);
    // As when no answer came in time.
    (f.hub.write(|tx| {
        let mut approval: Approval = tx.read().require("approval", asked.approval_id.unwrap())?;
        approval.state = ApprovalState::Expired;
        approval.revision += 1;
        tx.put("approval", approval.approval_id, &approval)?;
        tx.emit(Event::ApprovalUpserted { approval })
    }))
    .unwrap();
    assert!(!open(&f, asked.attention_id));
    assert_eq!(task(&f, task_id).attention_count, 0);
    pending.abort();
    let _ = pending.await;
    f.close().await;
}

#[tokio::test]
async fn what_an_earlier_version_left_waiting_is_settled_when_ophio_starts() {
    let f = Fixture::new().await;
    let (task_id, _) = finished(&f).await;
    let archive = edit(Command::ArchiveTask { task_id }, task(&f, task_id).revision);
    let receipt = f.hub.submit(&Caller::local_owner(), archive).await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    // Left open on an archived task, past what settles it as it's written.
    let now = f.clock.now();
    let item = headline_item(
        task_id,
        AttentionKind::WaitingForYou,
        AlertTopic::PrNeedsYou,
        "PR #7 needs you",
        None,
        now,
    );
    let id = item.attention_id;
    (f.hub.inner.db.lock().unwrap())
        .write(now, |tx| tx.put("attention", id, &item))
        .unwrap();
    assert!(open(&f, id));
    let f = f.reopened(|builder| builder).await;
    assert!(!open(&f, id));
    f.close().await;
}

#[tokio::test]
async fn a_slow_step_alert_ends_with_its_turn() {
    let f = Fixture::new().await;
    let project = f.project().await;
    let (task_id, _) = f
        .task(
            project,
            TaskPlacement::StartNow,
            &Caller::local_owner(),
            false,
        )
        .await;
    let run = run_started(&f, task_id).await;
    f.hub.alert_step(&task(&f, task_id), run, "Bash").unwrap();
    let items: Vec<AttentionItem> = f.hub.read(|r| r.list("attention")).unwrap();
    let alert = (items.into_iter())
        .find(|a| a.title == "A step is taking long")
        .unwrap();
    assert_eq!(alert.run_id, Some(run));
    let exited = ports::HarnessEvent::Exited {
        exit: ports::RunExit::Completed,
        outcome: None,
    };
    f.harness.emit(run, exited).await;
    let mut more = waiting();
    while open(&f, alert.attention_id) {
        assert!(more(), "the alert outlived its turn");
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    f.close().await;
}
