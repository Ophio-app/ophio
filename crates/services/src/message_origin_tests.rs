// SPDX-License-Identifier: Apache-2.0
use super::*;
use crate::followups::Followup;
use ports::{Clock, HarnessEvent};

fn notes(task: TaskId, now: Timestamp) -> Vec<Followup> {
    vec![
        Followup::answer(
            task,
            ApprovalId::new(),
            "internal approval answer".into(),
            now,
        ),
        Followup::reply(task, TaskId::new(), "internal helper reply".into(), now),
        Followup::wake(task, "internal wake".into(), now, now),
        Followup::checklist(
            "checklist:origin".into(),
            task,
            "internal checklist note".into(),
            now,
        ),
    ]
}

async fn acknowledge(f: &Fixture, task: TaskId) -> RunId {
    let run = run_started(f, task).await;
    f.harness
        .emit(
            run,
            HarnessEvent::SessionStarted {
                native_session: format!("synthetic-{run}"),
                actual_model: None,
                actual_effort: None,
                account_label: None,
            },
        )
        .await;
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if f.hub
                .read(|r| Ok(r.require::<Run>("run", run)?.native_session.is_some()))
                .unwrap()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap();
    run
}

async fn finish(f: &Fixture, run: RunId) {
    f.harness
        .emit(
            run,
            HarnessEvent::Exited {
                exit: ports::RunExit::Completed,
                outcome: None,
            },
        )
        .await;
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let ended = f
                .hub
                .read(|r| {
                    let run: Run = r.require("run", run)?;
                    Ok(run.state == RunState::Completed && !crate::runs::still_finishing(&run))
                })
                .unwrap();
            if ended {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap();
}

fn resume(task: TaskId, text: &str) -> CommandEnvelope {
    CommandEnvelope::new(Command::ResumeTask {
        observed_reply: None,
        task_id: task,
        harness: None,
        model: None,
        effort: None,
        mode: None,
        text: Some(text.into()),
        bundle: None,
        carried_plan: None,
        read_only: false,
    })
}

fn handover(f: &Fixture, task: TaskId) -> String {
    f.hub
        .read(|r| crate::turned_from_chat::handover(r, task, None, r.head()?))
        .unwrap()
}

fn assert_delivered_but_not_handed_over(f: &Fixture, task: TaskId, text: &str) {
    f.hub.read(|r| {
        let (items, _) = r.timeline(task, None, 200)?;
        let seq = items.iter().find_map(|item| match &item.item {
            TimelineBody::UserMessage { text: found, .. } if found == text => Some(item.seq),
            _ => None,
        }).expect("internal message was written to the timeline");
        assert!(items.iter().any(|item| matches!(item.item,
            TimelineBody::Delivery { message_seq, state: DeliveryState::NativeDelivered, .. } if message_seq == seq
        )), "internal message was not delivered: {text}");
        Ok(())
    }).unwrap();
    assert!(!handover(f, task).contains(text), "handed over {text}");
}

#[tokio::test]
async fn delivered_internal_answers_helpers_wakes_and_checklists_are_not_person_messages() {
    let f = Fixture::new().await;
    let project = f.project().await;
    let (task, _) = f
        .task(
            project,
            TaskPlacement::StartNow,
            &Caller::local_owner(),
            false,
        )
        .await;
    let run = acknowledge(&f, task).await;
    let notes = notes(task, f.clock.now());
    for mut note in notes.clone() {
        // Exercise the common live delivery path without the guard consuming
        // the record first. Normal wakes wait until the current turn ends.
        note.due_at = Timestamp(f.clock.now().0 + 60_000);
        f.hub.write(|tx| note.put(tx)).unwrap();
        f.hub
            .send_message_with_followup(
                &Caller::local_owner(),
                CommandId::new(),
                run,
                &note.text,
                None,
                MessageDelivery::Steer,
                Some(&note),
            )
            .await
            .unwrap();
    }
    let after = CommandEnvelope::new(Command::SendMessage {
        task_id: task,
        text: "person after live notes".into(),
        bundle: None,
        delivery: MessageDelivery::Steer,
        harness: None,
        model: None,
        effort: None,
    });
    assert_eq!(
        f.hub
            .submit(&Caller::local_owner(), after.clone())
            .await
            .status,
        ReceiptStatus::Accepted
    );
    settle(&f.hub, after.command_id).await;
    for note in notes {
        assert_delivered_but_not_handed_over(&f, task, &note.text);
    }
    let context = handover(&f, task);
    assert!(context.contains("The person: test"));
    assert!(context.contains("The person: person after live notes"));
    f.close().await;
}

#[tokio::test]
async fn internal_followups_starting_turns_are_not_handed_over_as_person_messages() {
    let f = Fixture::new().await;
    let project = f.project().await;
    let (task, _) = f
        .task(
            project,
            TaskPlacement::StartNow,
            &Caller::local_owner(),
            false,
        )
        .await;
    let run = acknowledge(&f, task).await;
    finish(&f, run).await;
    let notes = notes(task, f.clock.now());
    for note in &notes {
        f.hub.write(|tx| note.put(tx)).unwrap();
        f.hub.pass_on_followups().await.unwrap();
        let run = acknowledge(&f, task).await;
        assert!(
            f.harness
                .specs
                .lock()
                .unwrap()
                .iter()
                .any(|spec| spec.run_id == run && spec.prompt.text.contains(&note.text))
        );
        finish(&f, run).await;
        assert_delivered_but_not_handed_over(&f, task, &note.text);
    }
    let mut after = resume(task, "person after resumed notes");
    after.expected_revision = Some(
        f.hub
            .read(|r| Ok(r.require::<Task>("task", task)?.revision))
            .unwrap(),
    );
    let receipt = f.hub.submit(&Caller::local_owner(), after).await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    acknowledge(&f, task).await;
    let context = handover(&f, task);
    assert!(context.contains("The person: test"));
    assert!(context.contains("The person: person after resumed notes"));
    f.close().await;
}

#[tokio::test]
async fn a_person_starting_next_up_after_restart_does_not_change_the_queued_messages_origin() {
    let f = Fixture::new().await;
    let project = f.project().await;
    let (task, _) = f
        .task(
            project,
            TaskPlacement::StartNow,
            &Caller::local_owner(),
            false,
        )
        .await;
    let run = acknowledge(&f, task).await;
    finish(&f, run).await;
    let mut queued = CommandEnvelope::new(Command::QueueContinuation {
        task_id: task,
        text: "internal queued note".into(),
        bundle: None,
        authorization: QueueAuthorization::AskBeforeStart,
    });
    queued.expected_revision = Some(
        f.hub
            .read(|r| Ok(r.require::<Task>("task", task)?.revision))
            .unwrap(),
    );
    let receipt = f.hub.admit(&Caller::local_owner(), queued).await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    let f = f.reopened(|builder| builder).await;
    let (item_id, revision, queue_revision) = f
        .hub
        .read(|r| {
            let queue: QueueView = r.require("queue", project)?;
            let task: Task = r.require("task", task)?;
            Ok((
                queue
                    .items
                    .iter()
                    .find(|i| i.task_id == task.task_id && i.is_pending())
                    .unwrap()
                    .item_id,
                task.revision,
                queue.revision,
            ))
        })
        .unwrap();
    let mut start = CommandEnvelope::new(Command::QueueStart {
        item_id,
        task_revision: revision,
    });
    start.expected_revision = Some(queue_revision);
    let receipt = f.hub.submit(&Caller::local_owner(), start).await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    acknowledge(&f, task).await;
    assert_delivered_but_not_handed_over(&f, task, "internal queued note");
    assert!(handover(&f, task).contains("The person: test"));
    f.close().await;
}
