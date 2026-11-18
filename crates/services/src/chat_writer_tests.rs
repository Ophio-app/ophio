// SPDX-License-Identifier: Apache-2.0
use super::*;
use protocol::access::ConversationAccess;

fn writable(project: ProjectId) -> CommandEnvelope {
    conversation_access::create(
        project,
        ConversationAccess {
            write: vec![project],
            ..Default::default()
        },
    )
}

async fn created(f: &Fixture, command: CommandEnvelope) -> (TaskId, ports::RunSpec) {
    let receipt = f.hub.submit(&Caller::local_owner(), command).await;
    let Some(CommandResult::TaskCreated { task_id, .. }) = receipt.result else {
        panic!("{receipt:?}");
    };
    let spec = conversation_access::dispatched(f, task_id).await;
    (task_id, spec)
}

fn writer(f: &Fixture, project: ProjectId) -> Option<RunId> {
    f.hub
        .read(|r| Ok(r.require::<Project>("project", project)?.active_writer))
        .unwrap()
}

fn hold(f: &Fixture, run: RunId) -> Option<TaskId> {
    f.hub
        .read(|r| r.require(crate::access::CHAT_WRITER_HOLD, run))
        .unwrap()
}

#[tokio::test]
async fn writable_chat_reserves_project_until_queued_work_can_start() {
    let backend = Arc::new(FakeSandboxes::default());
    let f = Fixture::with(|b| b.sandboxes(backend)).await;
    let project = f.project().await;
    more_access_tests::all_modes(&f);
    let (chat, first) = created(&f, writable(project)).await;
    let run: Run = f.hub.read(|r| r.require("run", first.run_id)).unwrap();
    assert!(run.holds_writer);
    assert_eq!(writer(&f, project), Some(first.run_id));
    assert_eq!(hold(&f, first.run_id), None);

    let (work, _) = f
        .task(
            project,
            TaskPlacement::Queue {
                position: None,
                authorization: QueueAuthorization::AutoStart,
            },
            &Caller::local_owner(),
            false,
        )
        .await;
    f.hub.advance_queues().await.unwrap();
    let pending: Task = f.hub.read(|r| r.require("task", work)).unwrap();
    assert!(pending.current_run.is_none());
    assert_eq!(writer(&f, project), Some(first.run_id));

    f.hub
        .finish_run(first.run_id, RunState::Completed, None)
        .unwrap();
    f.hub.advance_queues().await.unwrap();
    let started: Task = f.hub.read(|r| r.require("task", work)).unwrap();
    let next = started
        .current_run
        .expect("queued Work starts after the chat");
    assert_ne!(next, first.run_id);
    assert_eq!(writer(&f, project), Some(next));
    assert!(
        !f.hub
            .read(|r| r.require::<Run>("run", first.run_id))
            .unwrap()
            .holds_writer
    );
    assert_eq!(
        f.hub
            .read(|r| r.require::<Task>("task", chat))
            .unwrap()
            .current_run,
        Some(first.run_id)
    );
    f.close().await;
}

#[tokio::test]
async fn second_writable_chat_starts_without_taking_the_existing_writer() {
    let backend = Arc::new(FakeSandboxes::default());
    let f = Fixture::with(|b| b.sandboxes(backend.clone())).await;
    let project = f.project().await;
    more_access_tests::all_modes(&f);
    let (first_task, first) = created(&f, writable(project)).await;
    let (second_task, second) = created(&f, writable(project)).await;
    let second_run: Run = f.hub.read(|r| r.require("run", second.run_id)).unwrap();
    assert!(!second_run.holds_writer);
    assert_eq!(writer(&f, project), Some(first.run_id));
    assert_eq!(hold(&f, second.run_id), Some(first_task));
    let reach = backend.specs.lock().unwrap()[1].reach.clone();
    assert!(reach.write.is_empty());
    assert!(
        !reach.read.is_empty(),
        "the blocked chat may still read the project"
    );
    assert!(
        second
            .prompt
            .text
            .contains("Read them, but do not change them this turn.")
    );
    assert_ne!(first_task, second_task);
    session(&f, second.run_id).await;
    f.hub
        .finish_run(second.run_id, RunState::Completed, None)
        .unwrap();
    assert_eq!(writer(&f, project), Some(first.run_id));
    f.hub
        .finish_run(first.run_id, RunState::Completed, None)
        .unwrap();
    assert_eq!(writer(&f, project), None);
    f.close().await;
}

#[tokio::test]
async fn blocked_chat_next_turn_claims_writer_after_holder_finishes() {
    let backend = Arc::new(FakeSandboxes::default());
    let f = Fixture::with(|b| b.sandboxes(backend)).await;
    let project = f.project().await;
    more_access_tests::all_modes(&f);
    let (_, first) = created(&f, writable(project)).await;
    let (second_task, second) = created(&f, writable(project)).await;
    session(&f, second.run_id).await;
    f.hub
        .finish_run(second.run_id, RunState::Completed, None)
        .unwrap();
    f.hub
        .finish_run(first.run_id, RunState::Completed, None)
        .unwrap();

    let command: Command = serde_json::from_value(serde_json::json!({
        "command": "resume_task", "task_id": second_task, "text": "continue"
    }))
    .unwrap();
    let receipt = f
        .hub
        .submit(
            &Caller::local_owner(),
            CommandEnvelope {
                expected_revision: Some(
                    f.hub
                        .read(|r| Ok(r.require::<Task>("task", second_task)?.revision))
                        .unwrap(),
                ),
                ..CommandEnvelope::new(command)
            },
        )
        .await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    let task: Task = f.hub.read(|r| r.require("task", second_task)).unwrap();
    let resumed = task.current_run.unwrap();
    assert_ne!(resumed, second.run_id);
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if f.harness
                .specs
                .lock()
                .unwrap()
                .iter()
                .any(|spec| spec.run_id == resumed)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        f.hub
            .read(|r| r.require::<Run>("run", resumed))
            .unwrap()
            .holds_writer
    );
    assert_eq!(hold(&f, resumed), None);
    assert_eq!(writer(&f, project), Some(resumed));
    f.close().await;
}

#[tokio::test]
async fn failed_writer_releases_project_for_the_next_chat() {
    let backend = Arc::new(FakeSandboxes::default());
    let f = Fixture::with(|b| b.sandboxes(backend)).await;
    let project = f.project().await;
    more_access_tests::all_modes(&f);
    let (_, first) = created(&f, writable(project)).await;
    f.hub
        .finish_run(first.run_id, RunState::Failed, None)
        .unwrap();
    assert_eq!(writer(&f, project), None);
    assert!(
        !f.hub
            .read(|r| r.require::<Run>("run", first.run_id))
            .unwrap()
            .holds_writer
    );
    let (_, next) = created(&f, writable(project)).await;
    assert_eq!(writer(&f, project), Some(next.run_id));
    assert_eq!(hold(&f, next.run_id), None);
    f.close().await;
}

async fn session(f: &Fixture, run: RunId) {
    f.harness
        .emit(
            run,
            ports::HarnessEvent::SessionStarted {
                native_session: format!("chat-{run}"),
                actual_model: None,
                actual_effort: None,
                account_label: None,
            },
        )
        .await;
    wait_run(f, run, |r| r.native_session.is_some()).await;
}

async fn wait_run(f: &Fixture, run: RunId, check: impl Fn(&Run) -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let row: Run = f.hub.read(|r| r.require("run", run)).unwrap();
            if check(&row) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}

async fn resume(f: &Fixture, task: TaskId, side: bool) -> ports::RunSpec {
    let command: Command = serde_json::from_value(serde_json::json!({
        "command": "resume_task", "task_id": task, "text": "next", "read_only": side
    }))
    .unwrap();
    let receipt = f
        .hub
        .submit(
            &Caller::local_owner(),
            CommandEnvelope {
                expected_revision: Some(
                    f.hub
                        .read(|r| Ok(r.require::<Task>("task", task)?.revision))
                        .unwrap(),
                ),
                ..CommandEnvelope::new(command)
            },
        )
        .await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    current_spec(f, task).await
}

async fn current_spec(f: &Fixture, task: TaskId) -> ports::RunSpec {
    let id = f
        .hub
        .read(|r| Ok(r.require::<Task>("task", task)?.current_run.unwrap()))
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(spec) = f
                .harness
                .specs
                .lock()
                .unwrap()
                .iter()
                .find(|s| s.run_id == id)
                .cloned()
            {
                return spec;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn blocked_chat_hears_hold_once_keeps_tools_and_hears_release_once() {
    let backend = Arc::new(FakeSandboxes::default());
    let f = Fixture::with(|b| b.sandboxes(backend)).await;
    let project = f.project().await;
    more_access_tests::all_modes(&f);
    let (work, _) = f
        .task(
            project,
            TaskPlacement::StartNow,
            &Caller::local_owner(),
            false,
        )
        .await;
    let work_run = f
        .hub
        .read(|r| Ok(r.require::<Task>("task", work)?.current_run.unwrap()))
        .unwrap();
    let (chat, first) = created(&f, writable(project)).await;
    assert!(first.bridge.is_some());
    assert!(
        first
            .prompt
            .text
            .contains("suggest them as work using Ophio")
    );
    session(&f, first.run_id).await;
    f.hub
        .finish_run(first.run_id, RunState::Completed, None)
        .unwrap();
    let second = resume(&f, chat, false).await;
    assert!(
        !second
            .prompt
            .text
            .contains("suggest them as work using Ophio")
    );
    session(&f, second.run_id).await;
    session(&f, second.run_id).await;
    f.hub
        .finish_run(second.run_id, RunState::Completed, None)
        .unwrap();
    let notes = f.hub.read(|r| r.journal(0)).unwrap().events.into_iter().filter(|e| matches!(
        &e.event, Event::TimelineAppended { task_id, item } if *task_id == chat && matches!(&item.item, TimelineBody::Note {text} if text.contains("so replies here can read it"))
    )).count();
    assert_eq!(notes, 1);
    f.hub
        .finish_run(work_run, RunState::Completed, None)
        .unwrap();
    let third = resume(&f, chat, false).await;
    assert!(
        third
            .prompt
            .text
            .contains("This reply may change this project’s files")
    );
    assert!(!third.prompt.text.contains("the person changed"));
    session(&f, third.run_id).await;
    f.hub
        .finish_run(third.run_id, RunState::Completed, None)
        .unwrap();
    let fourth = resume(&f, chat, false).await;
    assert!(
        !fourth
            .prompt
            .text
            .contains("pause on changing this project")
    );
    assert_eq!(writer(&f, project), Some(fourth.run_id));
    f.close().await;
}

#[tokio::test]
async fn unrestricted_chat_is_only_told_not_to_change_a_busy_project() {
    let f = Fixture::new().await;
    let project = f.project().await;
    more_access_tests::all_modes(&f);
    let (work, _) = f
        .task(
            project,
            TaskPlacement::StartNow,
            &Caller::local_owner(),
            false,
        )
        .await;
    let before = writer(&f, project);
    let (_, spec) = created(
        &f,
        conversation_access::create(
            project,
            ConversationAccess {
                whole_read: true,
                whole_write: true,
                internet: true,
                ..Default::default()
            },
        ),
    )
    .await;
    assert_eq!(hold(&f, spec.run_id), Some(work));
    assert_eq!(writer(&f, project), before);
    assert!(spec.sandbox.is_none());
    assert!(spec.bridge.is_some());
    assert!(spec.prompt.text.contains("unrestricted access"));
    assert!(spec.prompt.text.contains("cannot enforce this instruction"));
    f.close().await;
}

#[tokio::test]
async fn read_only_chat_and_side_question_never_take_the_project() {
    let backend = Arc::new(FakeSandboxes::default());
    let f = Fixture::with(|b| b.sandboxes(backend)).await;
    let project = f.project().await;
    more_access_tests::all_modes(&f);
    let (_, read) = created(
        &f,
        conversation_access::create(project, ConversationAccess::default()),
    )
    .await;
    assert_eq!(writer(&f, project), None);
    f.hub
        .finish_run(read.run_id, RunState::Completed, None)
        .unwrap();
    let (chat, writing) = created(&f, writable(project)).await;
    session(&f, writing.run_id).await;
    f.hub
        .finish_run(writing.run_id, RunState::Completed, None)
        .unwrap();
    let side = resume(&f, chat, true).await;
    assert_eq!(writer(&f, project), None);
    assert!(
        !f.hub
            .read(|r| r.require::<Run>("run", side.run_id))
            .unwrap()
            .holds_writer
    );
    f.close().await;
}

#[tokio::test]
async fn chat_helpers_keep_ownership_until_their_confirmed_end() {
    let backend = Arc::new(FakeSandboxes::default());
    let f = Fixture::with(|b| b.sandboxes(backend)).await;
    let project = f.project().await;
    more_access_tests::all_modes(&f);
    let (_, spec) = created(&f, writable(project)).await;
    for event in [
        ports::HarnessEvent::Child {
            lifetime: Some(ChildLifetime::InProcess),
            native_ref: "helper".into(),
            label: "Helper".into(),
            state: ChildState::Active,
        },
        ports::HarnessEvent::ParentEnded {
            exit: ports::RunExit::Completed,
        },
    ] {
        f.harness.emit(spec.run_id, event).await;
    }
    assert_eq!(writer(&f, project), Some(spec.run_id));
    wait_run(&f, spec.run_id, |r| r.state == RunState::Completed).await;
    let ended: Run = f.hub.read(|r| r.require("run", spec.run_id)).unwrap();
    assert!(ended.holds_writer);
    assert_eq!(ended.state, RunState::Completed);
    f.harness
        .emit(
            spec.run_id,
            ports::HarnessEvent::Exited {
                exit: ports::RunExit::Completed,
                outcome: None,
            },
        )
        .await;
    wait_run(&f, spec.run_id, |r| !r.holds_writer).await;
    assert_eq!(writer(&f, project), None);
    f.close().await;
}

#[tokio::test]
async fn uncertain_chat_keeps_project_after_restart_until_person_checks_it() {
    let backend = Arc::new(FakeSandboxes::default());
    let f = Fixture::with(|b| b.sandboxes(backend)).await;
    let project = f.project().await;
    more_access_tests::all_modes(&f);
    let (chat, spec) = created(&f, writable(project)).await;
    session(&f, spec.run_id).await;
    let task: Task = f.hub.read(|r| r.require("task", chat)).unwrap();
    let receipt = f
        .hub
        .submit(
            &Caller::local_owner(),
            CommandEnvelope {
                expected_revision: Some(task.revision),
                ..CommandEnvelope::new(Command::StopTask {
                    task_id: chat,
                    force: true,
                })
            },
        )
        .await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    wait_run(&f, spec.run_id, |r| r.state == RunState::Stopping).await;
    assert_eq!(writer(&f, project), Some(spec.run_id));
    let f = f.reopened(|b| b).await;
    assert_eq!(writer(&f, project), Some(spec.run_id));
    assert!(
        f.hub
            .read(|r| r.require::<Run>("run", spec.run_id))
            .unwrap()
            .holds_writer
    );
    let task: Task = f.hub.read(|r| r.require("task", chat)).unwrap();
    let command: Command = serde_json::from_value(serde_json::json!({
        "command":"reconcile_task", "task_id":chat, "resolution":"mark_completed"
    }))
    .unwrap();
    let receipt = f
        .hub
        .submit(
            &Caller::local_owner(),
            CommandEnvelope {
                expected_revision: Some(task.revision),
                ..CommandEnvelope::new(command)
            },
        )
        .await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    assert_eq!(writer(&f, project), None);
    f.close().await;
}

#[tokio::test]
async fn confirmed_chat_stop_releases_project() {
    let backend = Arc::new(FakeSandboxes::default());
    let f = Fixture::with(|b| b.sandboxes(backend)).await;
    let project = f.project().await;
    more_access_tests::all_modes(&f);
    let (chat, spec) = created(&f, writable(project)).await;
    session(&f, spec.run_id).await;
    let task: Task = f.hub.read(|r| r.require("task", chat)).unwrap();
    let receipt = f
        .hub
        .submit(
            &Caller::local_owner(),
            CommandEnvelope {
                expected_revision: Some(task.revision),
                ..CommandEnvelope::new(Command::StopTask {
                    task_id: chat,
                    force: false,
                })
            },
        )
        .await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    assert_eq!(writer(&f, project), Some(spec.run_id));
    f.harness
        .emit(
            spec.run_id,
            ports::HarnessEvent::Exited {
                exit: ports::RunExit::Interrupted,
                outcome: None,
            },
        )
        .await;
    wait_run(&f, spec.run_id, |r| !r.holds_writer).await;
    assert_eq!(writer(&f, project), None);
    f.close().await;
}

#[tokio::test]
async fn failed_writing_chat_automatically_starts_waiting_work() {
    let backend = Arc::new(FakeSandboxes::default());
    let f = Fixture::with(|b| b.sandboxes(backend)).await;
    let project = f.project().await;
    more_access_tests::all_modes(&f);
    let (_, spec) = created(&f, writable(project)).await;
    let (work, _) = f
        .task(
            project,
            TaskPlacement::Queue {
                position: None,
                authorization: QueueAuthorization::AutoStart,
            },
            &Caller::local_owner(),
            false,
        )
        .await;
    f.hub.advance_queues().await.unwrap();
    assert!(
        f.hub
            .read(|r| r.require::<Task>("task", work))
            .unwrap()
            .current_run
            .is_none()
    );
    f.hub
        .finish_run(spec.run_id, RunState::Failed, None)
        .unwrap();
    let next = wait_for_work(&f, work).await;
    assert_eq!(writer(&f, project), Some(next));
    f.close().await;
}

async fn wait_for_work(f: &Fixture, task: TaskId) -> RunId {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(run) = f
                .hub
                .read(|r| Ok(r.require::<Task>("task", task)?.current_run))
                .unwrap()
            {
                return run;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn recovery_confirming_chat_processes_ended_frees_waiting_work() {
    let backend = Arc::new(FakeSandboxes::default());
    let f = Fixture::with(|b| b.sandboxes(backend)).await;
    let project = f.project().await;
    more_access_tests::all_modes(&f);
    let (_, spec) = created(&f, writable(project)).await;
    session(&f, spec.run_id).await;
    let group = f.path.join("synthetic-cgroup");
    std::fs::create_dir_all(&group).unwrap();
    std::fs::write(group.join("cgroup.events"), "populated 0\n").unwrap();
    f.hub
        .write(|tx| {
            tx.put(
                crate::cut_off::RUN_GROUP,
                spec.run_id,
                &crate::cut_off::RunGroup {
                    contained: true,
                    dir: Some(group),
                    boot: None,
                },
            )
        })
        .unwrap();
    let (work, _) = f
        .task(
            project,
            TaskPlacement::Queue {
                position: None,
                authorization: QueueAuthorization::AutoStart,
            },
            &Caller::local_owner(),
            false,
        )
        .await;
    f.hub
        .finish_run(spec.run_id, RunState::ReconciliationRequired, None)
        .unwrap();
    assert_eq!(writer(&f, project), Some(spec.run_id));
    let f = f.reopened(|b| b).await;
    more_access_tests::all_modes(&f);
    f.hub.progress_queues();
    let next = wait_for_work(&f, work).await;
    let ended: Run = f.hub.read(|r| r.require("run", spec.run_id)).unwrap();
    assert_eq!(ended.state, RunState::ReconciliationRequired);
    assert!(ended.nothing_running);
    assert!(!ended.holds_writer);
    assert_eq!(writer(&f, project), Some(next));
    f.close().await;
}

#[tokio::test]
async fn a_contained_chat_with_unknown_result_releases_when_its_process_exits() {
    let backend = Arc::new(FakeSandboxes::default());
    let f = Fixture::with(|b| b.sandboxes(backend)).await;
    *f.harness.controls.containment.lock().unwrap() = Some(ports::harness::Containment::Complete);
    let project = f.project().await;
    more_access_tests::all_modes(&f);
    let (_, spec) = created(&f, writable(project)).await;
    session(&f, spec.run_id).await;
    // The parent result arrived first, but only the later process exit
    // establishes that nothing can still change the project.
    f.hub
        .finish_run(spec.run_id, RunState::ReconciliationRequired, None)
        .unwrap();
    assert_eq!(writer(&f, project), Some(spec.run_id));
    f.harness
        .emit(
            spec.run_id,
            ports::HarnessEvent::Exited {
                exit: ports::RunExit::Unknown {
                    reason: "Synthetic unknown reply".into(),
                },
                outcome: None,
            },
        )
        .await;
    wait_run(&f, spec.run_id, |r| r.nothing_running).await;
    assert_eq!(writer(&f, project), None);
    assert_eq!(
        f.hub
            .read(|r| r.require::<Run>("run", spec.run_id))
            .unwrap()
            .state,
        RunState::ReconciliationRequired
    );
    f.close().await;
}
