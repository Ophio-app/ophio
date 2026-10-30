// SPDX-License-Identifier: Apache-2.0
use super::*;

#[tokio::test]
async fn removing_pending_queue_item_archives_and_emits_task_without_starting() {
    let f = Fixture::new().await;
    let owner = Caller::local_owner();
    let project = f.project().await;
    let (task_id, _) = f
        .task(
            project,
            TaskPlacement::Queue {
                position: None,
                authorization: QueueAuthorization::AutoStart,
            },
            &owner,
            false,
        )
        .await;
    let queue: QueueView = f.hub.read(|r| r.require("queue", project)).unwrap();
    let before: Task = f.hub.read(|r| r.require("task", task_id)).unwrap();
    let mut command = CommandEnvelope::new(Command::QueueRemove {
        item_id: queue.items[0].item_id,
    });
    command.expected_revision = Some(queue.revision);
    assert_eq!(
        f.hub.submit(&owner, command.clone()).await.status,
        ReceiptStatus::Accepted
    );
    assert_eq!(
        f.hub.submit(&owner, command).await.status,
        ReceiptStatus::Duplicate
    );
    let task: Task = f.hub.read(|r| r.require("task", task_id)).unwrap();
    assert_eq!(task.state, TaskState::Archived);
    assert_eq!(task.current_run, None);
    assert_eq!(task.revision, before.revision + 1);
    assert_eq!(
        task.status_line.as_deref(),
        Some("Removed from Next up before starting")
    );
    let queue: QueueView = f.hub.read(|r| r.require("queue", project)).unwrap();
    assert!(queue.items.is_empty());
    let events = f.hub.read(|r| r.journal(0)).unwrap().events;
    assert!(events.iter().any(|e| matches!(&e.event, Event::TaskUpserted { task } if task.task_id == task_id && task.state == TaskState::Archived)));
    assert!(f.hub.read(|r| r.list::<Run>("run")).unwrap().is_empty());
    f.close().await;
}

#[tokio::test]
async fn reviewed_artifact_upload_links_source_and_validates_task_and_grants() {
    let f = Fixture::new().await;
    let owner = Caller::local_owner();
    let project = f.project().await;
    let (task_id, _) = f
        .task(
            project,
            TaskPlacement::Queue {
                position: None,
                authorization: QueueAuthorization::AskBeforeStart,
            },
            &owner,
            false,
        )
        .await;
    let declaration = |task_id, destination| Command::CreateUploadReviewed {
        name: "source.txt".into(),
        size: 6,
        sha256: hash(b"source"),
        destination,
        collision: UploadCollision::KeepBoth {
            name: "source (1).txt".into(),
        },
        task_id,
    };
    let missing = f
        .hub
        .submit(
            &owner,
            CommandEnvelope::new(declaration(
                Some(TaskId::new()),
                TransferDestination::Artifact,
            )),
        )
        .await;
    assert_eq!(missing.error.unwrap().code, ErrorCode::NotFound);
    let wrong_destination = f
        .hub
        .submit(
            &owner,
            CommandEnvelope::new(declaration(Some(task_id), TransferDestination::Inbox)),
        )
        .await;
    assert_eq!(
        wrong_destination.error.unwrap().code,
        ErrorCode::InvalidRequest
    );
    let device = f.device().await;
    let Principal::Device { device_id } = device.principal else {
        panic!("device caller")
    };
    f.hub
        .write(|tx| {
            let mut d: crate::identity::Device = tx.read().require("device", device_id)?;
            d.record.grants.tasks = false;
            tx.put("device", device_id, &d)
        })
        .unwrap();
    let denied = f
        .hub
        .submit(
            &device,
            CommandEnvelope::new(declaration(Some(task_id), TransferDestination::Artifact)),
        )
        .await;
    assert_eq!(denied.error.unwrap().code, ErrorCode::PermissionDenied);
    for task in [Some(task_id), None] {
        let receipt = f
            .hub
            .submit(
                &owner,
                CommandEnvelope::new(declaration(task, TransferDestination::Artifact)),
            )
            .await;
        let Some(CommandResult::Transfer { transfer }) = receipt.result else {
            panic!("upload declaration")
        };
        let mut sink = f.hub.upload(&owner, transfer.transfer_id).await.unwrap();
        sink.write(b"source").await.unwrap();
        let done = sink.finish().await.unwrap();
        let artifact: ArtifactMeta = f
            .hub
            .read(|r| r.require("artifact", done.artifact_id.unwrap()))
            .unwrap();
        assert_eq!(artifact.task_id, task);
        assert_eq!(
            artifact.links,
            task.into_iter()
                .map(|task_id| ArtifactLink {
                    task_id,
                    run_id: None,
                    annotation: None,
                    role: ArtifactRole::Source
                })
                .collect::<Vec<_>>()
        );
        assert_eq!(artifact.sha256, hash(b"source"));
    }
    f.close().await;
}
