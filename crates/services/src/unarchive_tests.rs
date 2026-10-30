// SPDX-License-Identifier: Apache-2.0
//! Undo after archiving: an archived task comes back to where it was, once.
use super::*;

fn task(f: &Fixture, task_id: TaskId) -> Task {
    f.hub.read(|r| r.require("task", task_id)).unwrap()
}

/// A task whose first run has completed.
async fn finished(f: &Fixture) -> TaskId {
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
    task_id
}

async fn archive(f: &Fixture, task_id: TaskId) {
    let archive = edit(Command::ArchiveTask { task_id }, task(f, task_id).revision);
    let receipt = f.hub.submit(&Caller::local_owner(), archive).await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    assert_eq!(task(f, task_id).state, TaskState::Archived);
}

/// Unarchives without the task's revision, as Undo does.
async fn unarchive(f: &Fixture, task_id: TaskId) -> CommandReceipt {
    let command = CommandEnvelope::new(Command::UnarchiveTask { task_id });
    f.hub.submit(&Caller::local_owner(), command).await
}

#[tokio::test]
async fn an_archived_task_comes_back_where_it_was() {
    let f = Fixture::new().await;
    let task_id = finished(&f).await;
    archive(&f, task_id).await;
    let archived = task(&f, task_id).revision;
    let receipt = unarchive(&f, task_id).await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    let back = task(&f, task_id);
    assert_eq!(back.state, TaskState::Completed);
    assert!(back.revision > archived);
    // Once back, it isn't archived to bring back again.
    let again = unarchive(&f, task_id).await;
    assert_eq!(again.status, ReceiptStatus::Rejected, "{again:?}");
    assert_eq!(again.error.unwrap().code, ErrorCode::Conflict);
    // Archived again, it comes back again.
    archive(&f, task_id).await;
    let receipt = unarchive(&f, task_id).await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    assert_eq!(task(&f, task_id).state, TaskState::Completed);
    f.close().await;
}

#[tokio::test]
async fn archiving_twice_keeps_where_it_came_from() {
    let f = Fixture::new().await;
    let task_id = finished(&f).await;
    archive(&f, task_id).await;
    archive(&f, task_id).await;
    let receipt = unarchive(&f, task_id).await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    assert_eq!(task(&f, task_id).state, TaskState::Completed);
    f.close().await;
}

#[tokio::test]
async fn only_a_task_that_had_ended_comes_back() {
    let f = Fixture::new().await;
    let task_id = finished(&f).await;
    // Archived before Undo existed, nothing says where it was.
    f.hub
        .write(|tx| {
            let mut t: Task = tx.read().require("task", task_id)?;
            t.state = TaskState::Archived;
            tx.put("task", task_id, &t)
        })
        .unwrap();
    let receipt = unarchive(&f, task_id).await;
    assert_eq!(receipt.status, ReceiptStatus::Rejected, "{receipt:?}");
    assert_eq!(receipt.error.unwrap().code, ErrorCode::Conflict);
    // One archived while it waited to start has lost its place in Next up.
    f.hub
        .write(|tx| tx.put("task_archived_from", task_id, &TaskState::Accepted))
        .unwrap();
    let receipt = unarchive(&f, task_id).await;
    assert_eq!(receipt.status, ReceiptStatus::Rejected, "{receipt:?}");
    assert_eq!(task(&f, task_id).state, TaskState::Archived);
    f.close().await;
}

#[tokio::test]
async fn a_task_not_archived_is_left_alone() {
    let f = Fixture::new().await;
    let task_id = finished(&f).await;
    let before = task(&f, task_id);
    let receipt = unarchive(&f, task_id).await;
    assert_eq!(receipt.status, ReceiptStatus::Rejected, "{receipt:?}");
    assert_eq!(task(&f, task_id), before);
    f.close().await;
}
