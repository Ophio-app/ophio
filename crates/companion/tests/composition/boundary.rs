// SPDX-License-Identifier: Apache-2.0
use super::*;
use tokio::net::{UnixListener, UnixStream};

async fn gate(listener: &UnixListener) -> UnixStream {
    let (mut stream, _) = tokio::time::timeout(Duration::from_secs(10), listener.accept())
        .await
        .unwrap()
        .unwrap();
    let mut ready = [0; 5];
    stream.read_exact(&mut ready).await.unwrap();
    assert_eq!(&ready, b"ready");
    stream
}

async fn snapshot(phone: &mut Phone) -> Box<Snapshot> {
    let QueryData::Snapshot(snapshot) = phone
        .query(Query::Snapshot {
            reads_withheld: None,
        })
        .await
    else {
        panic!()
    };
    snapshot
}

async fn ended(phone: &mut Phone, task: TaskId, run: RunId, state: RunState) {
    phone.event(|event| matches!(event, Event::RunUpserted { run: r } if r.run_id == run && r.state == state)).await;
    let snapshot = snapshot(phone).await;
    let task = snapshot.tasks.iter().find(|t| t.task_id == task).unwrap();
    assert_eq!(
        task.state,
        match state {
            RunState::Completed => TaskState::Completed,
            RunState::Interrupted => TaskState::Interrupted,
            _ => panic!(),
        }
    );
    assert!(
        !snapshot
            .runs
            .iter()
            .find(|r| r.run_id == run)
            .unwrap()
            .holds_writer
    );
    assert!(
        snapshot
            .projects
            .iter()
            .find(|p| p.project_id == task.project_id)
            .unwrap()
            .active_writer
            .is_none()
    );
    let QueryData::Queue { queue } = phone
        .query(Query::Queue {
            project_id: task.project_id,
        })
        .await
    else {
        panic!()
    };
    assert!(queue.writer.is_none());
}

async fn lifecycle(harness: HarnessKind) {
    let mut rig = Rig::new(true).await;
    rig.pair().await;
    let mut phone = Phone::new(&rig).await;
    let dir = rig.project_dir("turn-boundary");
    let turn_path = dir.join("turn.sock");
    let boundary_path = dir.join("boundary.sock");
    let turn_gate = UnixListener::bind(&turn_path).unwrap();
    let boundary_gate = UnixListener::bind(&boundary_path).unwrap();
    let project = phone.project(&dir).await;
    let task = phone.task(project, harness, "complete").await;
    let mut turn = gate(&turn_gate).await;
    phone.event(|e| matches!(e, Event::RunUpserted { run } if run.task_id == task && run.state == RunState::Working)).await;
    let first = snapshot(&mut phone)
        .await
        .tasks
        .iter()
        .find(|t| t.task_id == task)
        .unwrap()
        .current_run
        .unwrap();
    phone
        .effect(Command::SendMessage {
            task_id: task,
            text: "later".into(),
            bundle: None,
            delivery: MessageDelivery::NextTurn,
        })
        .await;
    std::fs::remove_file(turn_path).unwrap();
    turn.write_all(b"x").await.unwrap();
    let mut boundary = gate(&boundary_gate).await;
    // The fake is alive, waiting after it observed EOF. A phone message in
    // this exact window must be durably rejected, never reported as delivered.
    let receipt = phone
        .command(
            Command::SendMessage {
                task_id: task,
                text: "boundary message".into(),
                bundle: None,
                delivery: MessageDelivery::NextTurn,
            },
            None,
        )
        .await;
    let event = phone.event(|e| matches!(e, Event::CommandProgress { command_id, stage: CommandStage::Failed, .. } if *command_id == receipt.command_id)).await;
    let Event::CommandProgress {
        error: Some(error), ..
    } = event
    else {
        panic!()
    };
    assert_eq!(error.code, ErrorCode::Conflict);
    assert_eq!(
        error.message,
        "This task finished; send it again to continue"
    );
    assert_eq!(error.detail.unwrap()["action"], "resume_task");
    let QueryData::TaskTimeline { items, .. } = phone
        .query(Query::TaskTimeline {
            task_id: task,
            before_seq: None,
            limit: 100,
        })
        .await
    else {
        panic!()
    };
    let messages: Vec<_> = items
        .iter()
        .filter(|item| matches!(&item.item, TimelineBody::UserMessage { text, .. } if text == "boundary message"))
        .collect();
    assert_eq!(messages.len(), 1);
    let message = messages[0];
    assert_eq!(message.run_id, Some(first));
    assert!(matches!(
        message.item,
        TimelineBody::UserMessage {
            delivery: DeliveryState::HostAccepted,
            ..
        }
    ));
    // Admission saves the words before the adapter reports its refusal.
    // The later failure updates that same conversation row by sequence.
    let updates: Vec<_> = items
        .iter()
        .filter(|item| matches!(item.item, TimelineBody::Delivery { message_seq, .. } if message_seq == message.seq))
        .collect();
    assert_eq!(updates.len(), 1);
    assert_eq!(updates[0].run_id, Some(first));
    assert!(matches!(
        updates[0].item,
        TimelineBody::Delivery {
            state: DeliveryState::Failed,
            ..
        }
    ));
    std::fs::remove_file(boundary_path).unwrap();
    boundary.write_all(b"x").await.unwrap();
    ended(&mut phone, task, first, RunState::Completed).await;
    let snapshot = snapshot(&mut phone).await;
    let first_run = snapshot.runs.iter().find(|r| r.run_id == first).unwrap();
    let native_session = first_run.native_session.clone().unwrap();
    assert_eq!(
        first_run
            .outcome
            .as_ref()
            .unwrap()
            .usage
            .as_ref()
            .unwrap()
            .input_tokens,
        Some(12)
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("turns.jsonl"))
            .unwrap()
            .lines()
            .count(),
        2
    );

    let revision = snapshot
        .tasks
        .iter()
        .find(|t| t.task_id == task)
        .unwrap()
        .revision;
    phone
        .command(
            Command::ResumeTask {
                mode: None,
                task_id: task,
                text: Some("complete".into()),
                bundle: None,
                carried_plan: None,
                read_only: false,
                model: None,
                effort: None,
            },
            Some(revision),
        )
        .await;
    let event = phone.event(|e| matches!(e, Event::RunUpserted { run } if run.task_id == task && run.run_id != first && run.state == RunState::Completed)).await;
    let Event::RunUpserted { run: resumed } = event else {
        panic!()
    };
    assert_eq!(
        resumed.native_session.as_deref(),
        Some(native_session.as_str())
    );
    ended(&mut phone, task, resumed.run_id, RunState::Completed).await;
    let input = std::fs::read_to_string(dir.join("stdin.jsonl")).unwrap();
    assert!(!input.contains("boundary message"));
    if harness == HarnessKind::ClaudeCode {
        let args: Vec<String> =
            serde_json::from_slice(&std::fs::read(dir.join("argv.json")).unwrap()).unwrap();
        assert!(
            args.windows(2)
                .any(|p| p == ["--resume", native_session.as_str()])
        );
    } else {
        let frames: Vec<serde_json::Value> = input
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(
            frames
                .iter()
                .find(|f| f["method"] == "thread/resume")
                .unwrap()["params"]["threadId"],
            native_session
        );
    }

    // The freed writer can admit a new Work task. Stop must finish this run
    // even though its native process stays alive after the interrupted turn.
    let stopped = phone.task(project, harness, "wait").await;
    let event = phone.event(|e| matches!(e, Event::RunUpserted { run } if run.task_id == stopped && run.state == RunState::Working)).await;
    let Event::RunUpserted { run } = event else {
        panic!()
    };
    phone
        .command(
            Command::StopTask {
                task_id: stopped,
                force: false,
            },
            None,
        )
        .await;
    ended(&mut phone, stopped, run.run_id, RunState::Interrupted).await;
    rig.shutdown().await;
}

#[tokio::test]
async fn claude_turn_boundary_releases_writer_reports_race_and_resumes_over_tls() {
    lifecycle(HarnessKind::ClaudeCode).await;
}

#[tokio::test]
async fn codex_turn_boundary_releases_writer_reports_race_and_resumes_over_tls() {
    lifecycle(HarnessKind::Codex).await;
}
