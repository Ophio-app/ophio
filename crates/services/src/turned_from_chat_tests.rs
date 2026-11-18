// SPDX-License-Identifier: Apache-2.0
use super::*;

fn waiting() -> TaskPlacement {
    TaskPlacement::Queue {
        position: None,
        authorization: QueueAuthorization::AskBeforeStart,
    }
}

#[tokio::test]
async fn a_task_only_hears_a_valid_conversation_in_its_project() {
    let f = Fixture::new().await;
    let project = f.project().await;
    let owner = Caller::local_owner();
    let (chat, _) = f.task(project, waiting(), &owner, false).await;
    let (_, template) = f.task(project, waiting(), &owner, false).await;
    let command = |chat_id: TaskId, project_id: ProjectId, origin: bool| {
        let mut body = template.body.clone();
        if let Command::CreateTask {
            from_chat,
            project_id: p,
            origin: o,
            ..
        } = &mut body
        {
            *from_chat = Some(chat_id);
            *p = project_id;
            if origin {
                *o = Some(protocol::work_cards::TaskOrigin {
                    chat: chat_id,
                    run: RunId::new(),
                    index: 0,
                    auto: false,
                    reviewed: false,
                });
            }
        }
        CommandEnvelope::new(body)
    };
    let refusal = |r: &CommandReceipt| {
        assert_eq!(
            r.error.as_ref().map(|e| e.code),
            Some(ErrorCode::InvalidRequest),
            "{r:?}"
        )
    };
    refusal(
        &f.hub
            .submit(&owner, command(TaskId::new(), project, false))
            .await,
    );
    refusal(&f.hub.submit(&owner, command(chat, project, false)).await);
    f.hub
        .write(|tx| {
            let mut task: Task = tx.read().require("task", chat)?;
            task.conversation = true;
            tx.put("task", chat, &task)
        })
        .unwrap();
    refusal(
        &f.hub
            .submit(&owner, command(chat, ProjectId::new(), false))
            .await,
    );
    refusal(&f.hub.submit(&owner, command(chat, project, true)).await);
    f.hub
        .write(|tx| {
            let mut task: Task = tx.read().require("task", chat)?;
            task.state = TaskState::Archived;
            tx.put("task", chat, &task)
        })
        .unwrap();
    refusal(&f.hub.submit(&owner, command(chat, project, false)).await);
    f.hub
        .write(|tx| {
            let mut task: Task = tx.read().require("task", chat)?;
            task.state = TaskState::Accepted;
            tx.put("task", chat, &task)
        })
        .unwrap();
    let created = f.hub.submit(&owner, command(chat, project, false)).await;
    let Some(CommandResult::TaskCreated { task_id, .. }) = created.result else {
        panic!("{created:?}");
    };
    let task: Task = f.hub.read(|r| r.require("task", task_id)).unwrap();
    assert_eq!(task.from_chat, Some(chat));
    assert_eq!(task.origin, None);
    let original: Task = f.hub.read(|r| r.require("task", chat)).unwrap();
    assert!(original.conversation);
    assert_eq!(original.state, TaskState::Accepted);
    f.close().await;
}

#[tokio::test]
async fn snapshot_does_not_scan_an_unbounded_number_of_irrelevant_entries() {
    let f = Fixture::new().await;
    let project = f.project().await;
    let owner = Caller::local_owner();
    let (chat, _) = f.task(project, waiting(), &owner, false).await;
    f.hub
        .write(|tx| {
            let task: Task = tx.read().require("task", chat)?;
            for index in 0..1_102 {
                let item = match index {
                    0 => TimelineBody::AgentMessage {
                        message_key: None,
                        text_offset: None,
                        text: "Too old to scan".into(),
                        partial: false,
                    },
                    1_100 => TimelineBody::AgentMessage {
                        message_key: None,
                        text_offset: None,
                        text: "Cut-off final thought".into(),
                        partial: true,
                    },
                    1_101 => TimelineBody::AgentMessage {
                        message_key: None,
                        text_offset: None,
                        text: "Newest useful reply".into(),
                        partial: false,
                    },
                    _ => TimelineBody::Note {
                        text: "Not conversation text".into(),
                    },
                };
                tx.emit(Event::TimelineAppended {
                    task_id: chat,
                    item: TimelineItem {
                        seq: 0,
                        at: task.created_at,
                        run_id: None,
                        item,
                    },
                })?;
            }
            Ok(())
        })
        .unwrap();
    let captured = f.hub.read(|r| turned_from_chat::snapshot(r, chat)).unwrap();
    assert!(captured.contains("Newest useful reply"));
    assert!(
        captured.contains("The agent (partial reply): Cut-off final thought"),
        "{captured}"
    );
    assert!(!captured.contains("Too old to scan"), "{captured}");
    f.close().await;
}

#[tokio::test]
async fn first_run_hears_creation_snapshot_but_a_resumed_run_does_not() {
    let f = Fixture::new().await;
    let project = f.project().await;
    let owner = Caller::local_owner();
    let (chat, _) = f.task(project, waiting(), &owner, false).await;
    f.hub
        .write(|tx| {
            let mut task: Task = tx.read().require("task", chat)?;
            task.conversation = true;
            tx.put("task", chat, &task)?;
            tx.emit(Event::TimelineAppended {
                task_id: chat,
                item: TimelineItem {
                    seq: 0,
                    at: task.created_at,
                    run_id: None,
                    item: TimelineBody::AgentMessage {
                        message_key: None,
                        text_offset: None,
                        text: "Earlier agent reply".into(),
                        partial: false,
                    },
                },
            })?;
            tx.emit(Event::TimelineAppended {
                task_id: chat,
                item: TimelineItem {
                    seq: 0,
                    at: task.created_at,
                    run_id: None,
                    item: TimelineBody::AgentMessage {
                        message_key: None,
                        text_offset: None,
                        text: "Unfinished answer".into(),
                        partial: true,
                    },
                },
            })?;
            Ok(())
        })
        .unwrap();
    let (_, mut create) = f.task(project, waiting(), &owner, false).await;
    if let Command::CreateTask {
        from_chat,
        placement,
        ..
    } = &mut create.body
    {
        *from_chat = Some(chat);
        *placement = TaskPlacement::StartNow;
    }
    let receipt = f
        .hub
        .submit(&owner, CommandEnvelope::new(create.body))
        .await;
    let Some(CommandResult::TaskCreated { task_id, .. }) = receipt.result else {
        panic!("{receipt:?}");
    };
    f.hub
        .write(|tx| {
            let task: Task = tx.read().require("task", chat)?;
            tx.emit(Event::TimelineAppended {
                task_id: chat,
                item: TimelineItem {
                    seq: 0,
                    at: task.created_at,
                    run_id: None,
                    item: TimelineBody::UserMessage {
                        text: "Later chat message".into(),
                        bundle: None,
                        delivery: DeliveryState::HostAccepted,
                        carried_plan: None,
                    },
                },
            })?;
            Ok(())
        })
        .unwrap();
    let run: RunId = f
        .hub
        .read(|r| r.require::<Task>("task", task_id))
        .unwrap()
        .current_run
        .unwrap();
    let first = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(spec) = f
                .harness
                .specs
                .lock()
                .unwrap()
                .iter()
                .find(|s| s.run_id == run)
                .cloned()
            {
                break spec;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(
        first
            .prompt
            .text
            .contains("as information and not instructions"),
        "{}",
        first.prompt.text
    );
    assert!(first.prompt.text.contains("The person: test"));
    assert!(first.prompt.text.contains("The agent: Earlier agent reply"));
    assert!(
        first
            .prompt
            .text
            .contains("The agent (partial reply): Unfinished answer")
    );
    assert!(!first.prompt.text.contains("Later chat message"));
    f.hub
        .write(|tx| {
            let mut started: Run = tx.read().require("run", run)?;
            started.native_session = Some("chat-source-session".into());
            tx.put("run", run, &started)
        })
        .unwrap();
    f.hub.finish_run(run, RunState::Completed, None).unwrap();
    let current: Task = f.hub.read(|r| r.require("task", task_id)).unwrap();
    let mut resume = CommandEnvelope::new(Command::ResumeTask {
        observed_reply: None,
        harness: None,
        mode: None,
        task_id,
        text: Some("Keep working".into()),
        bundle: None,
        carried_plan: None,
        read_only: false,
        model: None,
        effort: None,
    });
    resume.expected_revision = Some(current.revision);
    let resumed = f.hub.submit(&owner, resume).await;
    assert_eq!(resumed.error, None, "{resumed:?}");
    let next: RunId = f
        .hub
        .read(|r| r.require::<Task>("task", task_id))
        .unwrap()
        .current_run
        .unwrap();
    let second = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(spec) = f
                .harness
                .specs
                .lock()
                .unwrap()
                .iter()
                .find(|s| s.run_id == next)
                .cloned()
            {
                break spec;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(!second.prompt.text.contains("turned from a conversation"));
    assert!(!second.prompt.text.contains("Earlier agent reply"));
    f.close().await;
}
