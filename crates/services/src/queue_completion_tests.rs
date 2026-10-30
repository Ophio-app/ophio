// SPDX-License-Identifier: Apache-2.0
use super::*;
use ports::Clock;

#[tokio::test]
async fn continue_ui_envelope_requires_the_revision_actually_reviewed() {
    let f = Fixture::new().await;
    let owner = Caller::local_owner();
    let project = f.project().await;
    let (task_id, _) = f
        .task(project, TaskPlacement::StartNow, &owner, false)
        .await;
    let run = run_started(&f, task_id).await;
    f.harness
        .emit(
            run,
            ports::HarnessEvent::Exited {
                exit: ports::RunExit::Completed,
                outcome: None,
            },
        )
        .await;
    let task = loop {
        let task: Task = f.hub.read(|r| r.require("task", task_id)).unwrap();
        if task.state == TaskState::Completed {
            break task;
        }
        tokio::task::yield_now().await;
    };
    // The exact JSON shape built by Composer's text-only Continue path.
    for expected in [None, Some(task.revision - 1), Some(task.revision)] {
        let wire = serde_json::json!({
            "command_id": CommandId::new(), "issued_at": f.clock.now(), "expires_at": null,
            "expected_revision": expected,
            "body": { "command": "resume_task", "task_id": task_id, "text": "Check the updated tests" }
        });
        let envelope: CommandEnvelope = serde_json::from_value(wire).unwrap();
        let receipt = f.hub.submit(&owner, envelope).await;
        if expected == Some(task.revision) {
            assert_eq!(receipt.status, ReceiptStatus::Accepted);
        } else {
            assert_eq!(receipt.error.unwrap().code, ErrorCode::StaleRevision);
        }
    }
    f.close().await;
}

#[tokio::test]
async fn queue_permission_expiry_revision_and_current_grants_gate_advancement() {
    for refusal in ["expired", "revision", "task", "grant"] {
        let f = Fixture::new().await;
        let device = f.device().await;
        let project = f.project().await;
        let (task, _) = f
            .task(
                project,
                TaskPlacement::Queue {
                    position: None,
                    authorization: QueueAuthorization::AutoStart,
                },
                &device,
                false,
            )
            .await;
        let queue: QueueView = f.hub.read(|r| r.require("queue", project)).unwrap();
        assert!(queue.items[0].authorization_expires_at.is_some());
        f.hub
            .write(|tx| {
                let mut queue: QueueView = tx.read().require("queue", project)?;
                queue.paused = false;
                if refusal == "revision" {
                    queue.revision += 1;
                }
                if refusal == "grant"
                    && let Principal::Device { device_id } = device.principal
                {
                    let mut record: crate::identity::Device =
                        tx.read().require("device", device_id)?;
                    record.record.grants.tasks = false;
                    tx.put("device", device_id, &record)?;
                }
                if refusal == "task" {
                    let mut task: Task = tx.read().require("task", task)?;
                    task.revision += 1;
                    tx.put("task", task.task_id, &task)?;
                }
                tx.put("queue", project, &queue)
            })
            .unwrap();
        if refusal == "expired" {
            f.clock
                .advance(Duration::from_millis(crate::runs::QUEUE_PERMIT_MS as u64));
        }
        f.hub.advance_queues().await.unwrap();
        let task: Task = f.hub.read(|r| r.require("task", task)).unwrap();
        assert_eq!(task.state, TaskState::Accepted, "{refusal}");
        assert_eq!(f.harness.starts.load(Ordering::SeqCst), 0);
        // Nor does it wait at the front as if about to start: the person
        // is asked for the go-ahead again.
        let queue: QueueView = f.hub.read(|r| r.require("queue", project)).unwrap();
        assert_eq!(
            queue.items[0].authorization,
            QueueAuthorization::AskBeforeStart,
            "{refusal}"
        );
        assert_eq!(
            queue.items[0].state,
            QueueItemState::AwaitingStart,
            "{refusal}"
        );
        let asked = f
            .hub
            .read(|r| r.get::<AttentionId>("queue_attention", project))
            .unwrap();
        let asked: AttentionItem = f
            .hub
            .read(|r| r.require("attention", asked.unwrap()))
            .unwrap();
        assert_eq!(asked.task_id, Some(task.task_id), "{refusal}");
        f.close().await;
    }
}

#[tokio::test]
async fn queued_prompt_edit_and_reviewed_start_use_persisted_request() {
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
    let queue: QueueView = f.hub.read(|r| r.require("queue", project)).unwrap();
    let item = &queue.items[0];
    assert_eq!(item.state, QueueItemState::AwaitingStart);
    assert_eq!(item.request.as_ref().unwrap().prompt, "test");
    f.hub
        .write(|tx| {
            let mut legacy = queue.clone();
            legacy.items[0].request = None;
            tx.put("queue", project, &legacy)
        })
        .unwrap();
    let restored = f.hub.queue_view(project).unwrap();
    assert_eq!(restored.items[0].request.as_ref().unwrap().prompt, "test");
    assert_eq!(f.harness.starts.load(Ordering::SeqCst), 0);
    let changed = f
        .hub
        .submit(
            &owner,
            edit(
                Command::QueueEditPrompt {
                    item_id: item.item_id,
                    prompt: "Inspect the changed parser".into(),
                },
                queue.revision,
            ),
        )
        .await;
    assert_eq!(changed.status, ReceiptStatus::Accepted);
    let stale = f
        .hub
        .submit(
            &owner,
            edit(
                Command::QueueStart {
                    item_id: item.item_id,
                    task_revision: 1,
                },
                queue.revision,
            ),
        )
        .await;
    assert_eq!(stale.error.unwrap().code, ErrorCode::StaleRevision);
    let queue: QueueView = f.hub.read(|r| r.require("queue", project)).unwrap();
    assert_eq!(
        queue.items[0].request.as_ref().unwrap().prompt,
        "Inspect the changed parser"
    );
    let resume = edit(
        Command::QueueResume {
            project_id: project,
        },
        queue.revision,
    );
    let id = resume.command_id;
    f.hub.submit(&owner, resume).await;
    settle(&f.hub, id).await;
    let queue: QueueView = f.hub.read(|r| r.require("queue", project)).unwrap();
    let task: Task = f.hub.read(|r| r.require("task", task_id)).unwrap();
    let receipt = f
        .hub
        .submit(
            &owner,
            edit(
                Command::QueueStart {
                    item_id: queue.items[0].item_id,
                    task_revision: task.revision,
                },
                queue.revision,
            ),
        )
        .await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted);
    settle(&f.hub, receipt.command_id).await;
    assert_eq!(
        f.harness.specs.lock().unwrap()[0].prompt.text,
        "Inspect the changed parser"
    );
    f.close().await;
}

#[tokio::test]
async fn queue_reorder_refuses_to_move_a_dependency_after_its_dependent() {
    let f = Fixture::new().await;
    let owner = Caller::local_owner();
    let project = f.project().await;
    for _ in 0..2 {
        f.task(
            project,
            TaskPlacement::Queue {
                position: None,
                authorization: QueueAuthorization::AskBeforeStart,
            },
            &owner,
            false,
        )
        .await;
    }
    let queue: QueueView = f.hub.read(|r| r.require("queue", project)).unwrap();
    f.hub
        .submit(
            &owner,
            edit(
                Command::QueueSetDependencies {
                    item_id: queue.items[1].item_id,
                    depends_on: vec![queue.items[0].item_id],
                },
                queue.revision,
            ),
        )
        .await;
    let queue: QueueView = f.hub.read(|r| r.require("queue", project)).unwrap();
    let receipt = f
        .hub
        .submit(
            &owner,
            edit(
                Command::QueueMove {
                    item_id: queue.items[0].item_id,
                    position: 1,
                },
                queue.revision,
            ),
        )
        .await;
    assert_eq!(receipt.error.unwrap().code, ErrorCode::Conflict);
    assert_eq!(
        f.hub
            .read(|r| r.require::<QueueView>("queue", project))
            .unwrap(),
        queue
    );
    f.close().await;
}

struct WithdrawingControl {
    held: std::sync::Mutex<Vec<ports::MessageRef>>,
    /// How the agent takes a follow-up: held for its next turn unless a test
    /// says otherwise.
    delivery: std::sync::Mutex<DeliveryState>,
    /// A test holds this to keep a follow-up on its way to the agent.
    accepting: tokio::sync::Mutex<()>,
    /// One permit for each follow-up that has reached the agent.
    arrived: tokio::sync::Semaphore,
}
#[async_trait::async_trait]
impl ports::RunControl for WithdrawingControl {
    async fn send_message(
        &self,
        reference: ports::MessageRef,
        _: ports::PromptInput,
        _: MessageDelivery,
    ) -> Result<DeliveryState, ports::HarnessError> {
        self.arrived.add_permits(1);
        let _accepting = self.accepting.lock().await;
        let delivery = *self.delivery.lock().unwrap();
        if delivery == DeliveryState::QueuedForBoundary {
            self.held.lock().unwrap().push(reference);
        }
        Ok(delivery)
    }
    async fn cancel_message(
        &self,
        reference: ports::MessageRef,
    ) -> Result<(), ports::HarnessError> {
        let mut held = self.held.lock().unwrap();
        let index = held
            .iter()
            .position(|r| *r == reference)
            .ok_or(ports::HarnessError::MessageNotQueued)?;
        held.remove(index);
        Ok(())
    }
    async fn interrupt(&self) -> Result<(), ports::HarnessError> {
        Ok(())
    }
    fn close_admission(&self) {}
    async fn resolve_approval(
        &self,
        _: &str,
        _: &ApprovalDecision,
    ) -> Result<(), ports::HarnessError> {
        Ok(())
    }
    async fn shutdown(&self) -> Result<(), ports::HarnessError> {
        Ok(())
    }
}

/// A working task whose agent holds follow-ups until its turn ends and can
/// withdraw them.
async fn holding_task(f: &Fixture, owner: &Caller) -> (TaskId, Arc<WithdrawingControl>) {
    let project = f.project().await;
    let (task, _) = f.task(project, TaskPlacement::StartNow, owner, false).await;
    let run = run_started(f, task).await;
    f.hub
        .write(|tx| {
            let mut catalog: Vec<protocol::capability::HarnessInfo> =
                tx.read().require("meta", "harnesses")?;
            catalog[0].capabilities.cancel_queued_message =
                Some(protocol::capability::Availability::Available);
            tx.put("meta", "harnesses", &catalog)
        })
        .unwrap();
    let control = Arc::new(WithdrawingControl {
        held: std::sync::Mutex::new(Vec::new()),
        delivery: std::sync::Mutex::new(DeliveryState::QueuedForBoundary),
        accepting: tokio::sync::Mutex::new(()),
        arrived: tokio::sync::Semaphore::new(0),
    });
    f.hub.inner.runs.lock().await.insert(run, control.clone());
    f.harness
        .emit(
            run,
            ports::HarnessEvent::SessionStarted {
                native_session: "fixture-session".into(),
                actual_model: None,
                actual_effort: None,
                account_label: None,
            },
        )
        .await;
    loop {
        let current: Task = f.hub.read(|r| r.require("task", task)).unwrap();
        if current.state == TaskState::Working {
            break;
        }
        tokio::task::yield_now().await;
    }
    (task, control)
}

/// A chat the owner linked, and the caller it acts as.
async fn linked_chat(f: &Fixture, owner: &Caller) -> (ChannelIdentity, Caller) {
    let code = f
        .hub
        .request_channel_link(crate::api::ChannelLinkRequest {
            channel: ChannelKind::Telegram,
            external_user: "7".into(),
            external_chat: "42".into(),
            display_name: "Chat user".into(),
        })
        .await
        .unwrap();
    let receipt = f
        .hub
        .submit(
            owner,
            CommandEnvelope::new(Command::ApproveChannelLink { link_code: code }),
        )
        .await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted);
    let identity = f
        .hub
        .channel_identity(ChannelKind::Telegram, "7", "42")
        .await
        .unwrap();
    let chat = Caller {
        principal: Principal::Channel {
            identity_id: identity.id,
        },
        grants: identity.grants.clone(),
    };
    (identity, chat)
}

fn follow_up(task_id: TaskId, text: &str) -> CommandEnvelope {
    CommandEnvelope::new(Command::SendMessage {
        task_id,
        text: text.into(),
        bundle: None,
        delivery: MessageDelivery::NextTurn,
    })
}

fn revoke(identity: &ChannelIdentity) -> CommandEnvelope {
    CommandEnvelope::new(Command::RevokeChannelIdentity {
        identity_id: identity.id,
    })
}

/// The timeline sequence of the message `text` held for the next turn.
async fn held_seq(f: &Fixture, text: &str) -> u64 {
    let Backlog::Events(events) = f
        .hub
        .subscribe(&Caller::local_owner(), Some(0))
        .await
        .unwrap()
        .backlog
    else {
        panic!("fresh journal")
    };
    events
        .iter()
        .find_map(|e| match &e.event {
            Event::TimelineAppended { item, .. } => match &item.item {
                TimelineBody::UserMessage { text: sent, .. } if sent == text => Some(item.seq),
                _ => None,
            },
            _ => None,
        })
        .unwrap()
}

#[tokio::test]
async fn held_message_withdrawal_is_durable_and_a_second_withdrawal_is_refused() {
    let f = Fixture::new().await;
    let owner = Caller::local_owner();
    let (task, control) = holding_task(&f, &owner).await;
    let receipt = f
        .hub
        .submit(
            &owner,
            CommandEnvelope::new(Command::SendMessage {
                task_id: task,
                text: "Withdraw this correction".into(),
                bundle: None,
                delivery: MessageDelivery::NextTurn,
            }),
        )
        .await;
    settle(&f.hub, receipt.command_id).await;
    let seq = held_seq(&f, "Withdraw this correction").await;
    let receipt = f
        .hub
        .submit(
            &owner,
            CommandEnvelope::new(Command::CancelMessage {
                task_id: task,
                message_seq: seq,
            }),
        )
        .await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted);
    settle(&f.hub, receipt.command_id).await;
    assert!(control.held.lock().unwrap().is_empty());
    let Backlog::Events(events) = f.hub.subscribe(&owner, Some(0)).await.unwrap().backlog else {
        panic!("fresh journal")
    };
    assert_eq!(events.iter().filter(|e| matches!(&e.event, Event::TimelineAppended { item, .. } if matches!(item.item, TimelineBody::Delivery { message_seq, state: DeliveryState::Cancelled, withdrawn_by_stop: false, withdrawn_by_revocation: false } if message_seq == seq))).count(), 1);
    let receipt = f
        .hub
        .submit(
            &owner,
            CommandEnvelope::new(Command::CancelMessage {
                task_id: task,
                message_seq: seq,
            }),
        )
        .await;
    assert_eq!(receipt.error.unwrap().code, ErrorCode::Conflict);
    f.close().await;
}

#[tokio::test]
async fn revoking_a_chat_withdraws_the_messages_it_left_held_and_says_so() {
    let f = Fixture::new().await;
    let owner = Caller::local_owner();
    let (task, control) = holding_task(&f, &owner).await;
    let (identity, chat) = linked_chat(&f, &owner).await;
    for (caller, text) in [(&chat, "From the chat"), (&owner, "From the phone")] {
        let receipt = f.hub.submit(caller, follow_up(task, text)).await;
        settle(&f.hub, receipt.command_id).await;
    }
    let (from_chat, from_phone) = (
        held_seq(&f, "From the chat").await,
        held_seq(&f, "From the phone").await,
    );
    assert_eq!(control.held.lock().unwrap().len(), 2);

    let receipt = f.hub.submit(&owner, revoke(&identity)).await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted);
    settle(&f.hub, receipt.command_id).await;
    // Only the phone's message is still held, and the chat's is recorded as
    // withdrawn because its chat lost access.
    assert_eq!(control.held.lock().unwrap().len(), 1);
    let Backlog::Events(events) = f.hub.subscribe(&owner, Some(0)).await.unwrap().backlog else {
        panic!("fresh journal")
    };
    let withdrawn: Vec<_> = events
        .iter()
        .filter_map(|e| match &e.event {
            Event::TimelineAppended { item, .. } => match item.item {
                TimelineBody::Delivery {
                    message_seq,
                    state: DeliveryState::Cancelled,
                    withdrawn_by_stop: false,
                    withdrawn_by_revocation,
                } => Some((message_seq, withdrawn_by_revocation)),
                _ => None,
            },
            _ => None,
        })
        .collect();
    assert_eq!(withdrawn, [(from_chat, true)]);
    let status = f
        .hub
        .query(
            &owner,
            Query::CommandStatus {
                command_id: receipt.command_id,
            },
        )
        .await
        .unwrap();
    assert!(matches!(
        status,
        QueryData::CommandStatus {
            stage: Some(CommandStage::Observed),
            ..
        }
    ));
    let receipt = f
        .hub
        .submit(
            &owner,
            CommandEnvelope::new(Command::CancelMessage {
                task_id: task,
                message_seq: from_phone,
            }),
        )
        .await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted);
    f.close().await;
}

async fn command_status(
    f: &Fixture,
    command_id: CommandId,
) -> (Option<CommandStage>, Option<ErrorCode>) {
    match f
        .hub
        .query(&Caller::local_owner(), Query::CommandStatus { command_id })
        .await
        .unwrap()
    {
        QueryData::CommandStatus { stage, error, .. } => (stage, error.map(|e| e.code)),
        _ => panic!("command status"),
    }
}

#[tokio::test]
async fn revoking_a_chat_withdraws_a_message_it_was_still_sending() {
    let f = Fixture::new().await;
    let owner = Caller::local_owner();
    let (task, control) = holding_task(&f, &owner).await;
    let (identity, chat) = linked_chat(&f, &owner).await;
    // The chat's message is on its way to the agent when the owner revokes it.
    let accepting = control.accepting.lock().await;
    let sent = f
        .hub
        .submit(&chat, follow_up(task, "Still on its way"))
        .await;
    control.arrived.acquire().await.unwrap().forget();
    let revoked = f.hub.submit(&owner, revoke(&identity)).await;
    assert_eq!(revoked.status, ReceiptStatus::Accepted);
    drop(accepting);
    settle(&f.hub, sent.command_id).await;
    settle(&f.hub, revoked.command_id).await;
    assert!(control.held.lock().unwrap().is_empty());
    assert_eq!(
        command_status(&f, revoked.command_id).await,
        (Some(CommandStage::Observed), None)
    );
    // One the chat sends once its access has ended never reaches the agent,
    // even when it gets past admission first.
    let run = f
        .hub
        .read(|r| r.require::<Task>("task", task))
        .unwrap()
        .current_run
        .unwrap();
    let late = f
        .hub
        .send_message(
            &chat,
            CommandId::new(),
            run,
            "Too late",
            None,
            MessageDelivery::NextTurn,
        )
        .await
        .unwrap_err();
    assert_eq!(late.code, ErrorCode::Unauthenticated);
    assert_eq!(control.arrived.available_permits(), 0);
    f.close().await;
}

#[tokio::test]
async fn revocation_before_dispatch_preserves_the_accepted_message() {
    let f = Fixture::new().await;
    let owner = Caller::local_owner();
    let (task, control) = holding_task(&f, &owner).await;
    let (identity, chat) = linked_chat(&f, &owner).await;
    let sent = f
        .hub
        .submit(&chat, follow_up(task, "Accepted before revocation"))
        .await;
    assert_eq!(sent.status, ReceiptStatus::Accepted);
    let revoked = f.hub.submit(&owner, revoke(&identity)).await;
    assert_eq!(revoked.status, ReceiptStatus::Accepted);
    // Current-thread admission has not yielded to either scheduled worker.
    // Run the revocation effect first, while the accepted send is still pending.
    assert!(
        f.hub
            .read(|r| r.command(sent.command_id))
            .unwrap()
            .unwrap()
            .pending
    );
    f.hub.withdraw_revoked(identity.id).await.unwrap();
    assert!(
        f.hub
            .read(|r| r.command(sent.command_id))
            .unwrap()
            .unwrap()
            .pending
    );
    settle(&f.hub, sent.command_id).await;
    settle(&f.hub, revoked.command_id).await;
    assert_eq!(
        command_status(&f, revoked.command_id).await,
        (Some(CommandStage::Observed), None)
    );
    assert_eq!(
        command_status(&f, sent.command_id).await,
        (Some(CommandStage::Failed), Some(ErrorCode::Unauthenticated))
    );
    assert_eq!(control.arrived.available_permits(), 0);
    let timeline: Vec<TimelineItem> = f.hub.read(|r| r.list(&format!("timeline:{task}"))).unwrap();
    let messages: Vec<_> = timeline
        .iter()
        .filter(|item| {
            matches!(&item.item,
                TimelineBody::UserMessage { text, .. } if text == "Accepted before revocation"
            )
        })
        .collect();
    assert_eq!(messages.len(), 1);
    assert_eq!(timeline.iter().filter(|item| matches!(item.item,
        TimelineBody::Delivery { message_seq, state: DeliveryState::Failed, .. } if message_seq == messages[0].seq
    )).count(), 1);
    f.close().await;
}

#[tokio::test]
async fn revoking_a_chat_says_when_a_message_it_sent_may_still_reach_the_agent() {
    for delivery in [DeliveryState::HostAccepted, DeliveryState::Unknown] {
        let f = Fixture::new().await;
        let owner = Caller::local_owner();
        let (task, control) = holding_task(&f, &owner).await;
        let (identity, chat) = linked_chat(&f, &owner).await;
        *control.delivery.lock().unwrap() = delivery;
        let sent = f
            .hub
            .submit(&chat, follow_up(task, "Already handed over"))
            .await;
        settle(&f.hub, sent.command_id).await;
        let revoked = f.hub.submit(&owner, revoke(&identity)).await;
        settle(&f.hub, revoked.command_id).await;
        assert_eq!(
            command_status(&f, revoked.command_id).await,
            (Some(CommandStage::Failed), Some(ErrorCode::Conflict))
        );
        f.close().await;
    }
}

#[tokio::test]
async fn reviewed_queue_start_runs_only_this_task_while_next_up_stays_paused() {
    let f = Fixture::new().await;
    let owner = Caller::local_owner();
    let project = f.project().await;
    let (first, _) = f
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
    let (second, _) = f
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
    // Adding one to start by itself resumed Next up; the person pauses it.
    f.hub
        .write(|tx| {
            let mut queue: QueueView = tx.read().require("queue", project)?;
            assert!(!queue.paused);
            queue.paused = true;
            tx.put("queue", project, &queue)
        })
        .unwrap();
    let queue: QueueView = f.hub.read(|r| r.require("queue", project)).unwrap();
    let task: Task = f.hub.read(|r| r.require("task", first)).unwrap();
    let receipt = f
        .hub
        .submit(
            &owner,
            edit(
                Command::QueueStart {
                    item_id: queue.items[0].item_id,
                    task_revision: task.revision,
                },
                queue.revision,
            ),
        )
        .await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    settle(&f.hub, receipt.command_id).await;
    let first_run = run_started(&f, first).await;
    f.hub
        .finish_run(first_run, RunState::Completed, None)
        .unwrap();
    f.hub.advance_queues().await.unwrap();
    let queue: QueueView = f.hub.read(|r| r.require("queue", project)).unwrap();
    assert!(queue.paused);
    let task: Task = f.hub.read(|r| r.require("task", second)).unwrap();
    assert_eq!(task.state, TaskState::Accepted);
    assert_eq!(f.harness.starts.load(Ordering::SeqCst), 1);
    f.close().await;
}

#[tokio::test]
async fn front_go_ahead_attention_is_unique_and_clears_with_queue_decisions() {
    for decision in ["start", "remove", "move", "pause"] {
        let f = Fixture::new().await;
        let owner = Caller::local_owner();
        let project = f.project().await;
        let (first, _) = f
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
        let (second, _) = f
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
        let open = || {
            f.hub
                .read(|r| r.list::<AttentionItem>("attention"))
                .unwrap()
                .into_iter()
                .filter(|a| a.kind == AttentionKind::WaitingForYou && a.resolved_at.is_none())
                .collect::<Vec<_>>()
        };
        assert!(open().is_empty(), "paused queues do not ask");
        let queue: QueueView = f.hub.read(|r| r.require("queue", project)).unwrap();
        let receipt = f
            .hub
            .submit(
                &owner,
                edit(
                    Command::QueueResume {
                        project_id: project,
                    },
                    queue.revision,
                ),
            )
            .await;
        assert_eq!(receipt.status, ReceiptStatus::Accepted);
        f.hub.advance_queues().await.unwrap();
        f.hub.advance_queues().await.unwrap();
        let attention = open();
        assert_eq!(attention.len(), 1);
        assert_eq!(attention[0].task_id, Some(first));
        assert_eq!(attention[0].run_id, None);
        let task: Task = f.hub.read(|r| r.require("task", first)).unwrap();
        assert_eq!(task.attention_count, 1);
        let queue: QueueView = f.hub.read(|r| r.require("queue", project)).unwrap();
        let item_id = queue.items[0].item_id;
        let command = match decision {
            "start" => Command::QueueStart {
                item_id,
                task_revision: task.revision,
            },
            "remove" => Command::QueueRemove { item_id },
            "move" => Command::QueueMove {
                item_id,
                position: 1,
            },
            _ => Command::QueuePause {
                project_id: project,
            },
        };
        let receipt = f.hub.submit(&owner, edit(command, queue.revision)).await;
        assert_eq!(
            receipt.status,
            ReceiptStatus::Accepted,
            "{decision}: {receipt:?}"
        );
        let old: AttentionItem = f
            .hub
            .read(|r| r.require("attention", attention[0].attention_id))
            .unwrap();
        assert!(old.resolved_at.is_some(), "{decision}");
        let task: Task = f.hub.read(|r| r.require("task", first)).unwrap();
        assert_eq!(task.attention_count, 0, "{decision}");
        if ["remove", "move"].contains(&decision) {
            assert_eq!(
                open().iter().map(|a| a.task_id).collect::<Vec<_>>(),
                vec![Some(second)]
            );
        } else {
            // A started task holds the writer, so the next go-ahead waits for it.
            assert!(open().is_empty(), "{decision}");
        }
        f.close().await;
    }
}

#[tokio::test]
async fn front_go_ahead_waits_for_the_writer_and_dates_from_its_release() {
    let f = Fixture::new().await;
    let owner = Caller::local_owner();
    let project = f.project().await;
    let (running, _) = f
        .task(project, TaskPlacement::StartNow, &owner, false)
        .await;
    let run = run_started(&f, running).await;
    let (queued, _) = f
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
    let queue: QueueView = f.hub.read(|r| r.require("queue", project)).unwrap();
    let receipt = f
        .hub
        .submit(
            &owner,
            edit(
                Command::QueueResume {
                    project_id: project,
                },
                queue.revision,
            ),
        )
        .await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted);
    f.hub.advance_queues().await.unwrap();
    let open = || {
        f.hub
            .read(|r| r.list::<AttentionItem>("attention"))
            .unwrap()
            .into_iter()
            .filter(|a| a.kind == AttentionKind::WaitingForYou && a.resolved_at.is_none())
            .collect::<Vec<_>>()
    };
    assert!(open().is_empty(), "the running task still holds the writer");
    f.clock.advance(Duration::from_secs(300));
    let released = f.clock.now();
    f.harness
        .emit(
            run,
            ports::HarnessEvent::Exited {
                exit: ports::RunExit::Completed,
                outcome: None,
            },
        )
        .await;
    let attention = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let attention = open();
            if !attention.is_empty() {
                return attention;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the go-ahead was not raised when the writer was released");
    assert_eq!(attention.len(), 1);
    assert_eq!(attention[0].task_id, Some(queued));
    assert_eq!(attention[0].created_at, released);
    f.close().await;
}

#[tokio::test]
async fn a_queued_start_that_cannot_begin_ends_with_its_reason() {
    let f = Fixture::new().await;
    let owner = Caller::local_owner();
    let device = f.device().await;
    let project = f.project().await;
    let (task_id, _) = f
        .task(
            project,
            TaskPlacement::Queue {
                position: None,
                authorization: QueueAuthorization::AskBeforeStart,
            },
            &owner,
            true,
        )
        .await;
    // The person holds control of the screen the queued task would use.
    let target = f.desktop.target.lock().unwrap().target_id;
    let lease = CommandEnvelope::new(Command::RequestHumanLease {
        target_id: target,
        geometry_revision: 1,
    });
    let id = lease.command_id;
    f.hub.submit(&device, lease).await;
    settle(&f.hub, id).await;
    let view: TargetControlView = f.hub.read(|r| r.require("control", target)).unwrap();
    assert!(view.lease.is_some());
    let queue: QueueView = f.hub.read(|r| r.require("queue", project)).unwrap();
    let task: Task = f.hub.read(|r| r.require("task", task_id)).unwrap();
    let receipt = f
        .hub
        .submit(
            &owner,
            edit(
                Command::QueueStart {
                    item_id: queue.items[0].item_id,
                    task_revision: task.revision,
                },
                queue.revision,
            ),
        )
        .await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    settle(&f.hub, receipt.command_id).await;
    let task: Task = f.hub.read(|r| r.require("task", task_id)).unwrap();
    let run: Run = f
        .hub
        .read(|r| r.require("run", task.current_run.unwrap()))
        .unwrap();
    assert_eq!(run.state, RunState::Failed);
    assert!(run.ended_at.is_some());
    assert_eq!(task.state, TaskState::Failed);
    let project: Project = f.hub.read(|r| r.require("project", project)).unwrap();
    assert_eq!(project.active_writer, None);
    let notes: Vec<TimelineItem> = f
        .hub
        .read(|r| r.list(&format!("timeline:{task_id}")))
        .unwrap();
    assert!(notes.iter().any(|item| matches!(
        &item.item,
        TimelineBody::Note { text } if text.starts_with("The agent didn’t start.")
    )));
    assert_eq!(f.harness.starts.load(Ordering::SeqCst), 0);
    f.close().await;
}

struct RefusingControl;
#[async_trait::async_trait]
impl ports::RunControl for RefusingControl {
    async fn send_message(
        &self,
        _: ports::MessageRef,
        _: ports::PromptInput,
        _: MessageDelivery,
    ) -> Result<DeliveryState, ports::HarnessError> {
        Err(ports::HarnessError::Unsupported(
            "The images in this message are too large to send together. Send fewer or smaller images.".into(),
        ))
    }
    async fn interrupt(&self) -> Result<(), ports::HarnessError> {
        Ok(())
    }
    fn close_admission(&self) {}
    async fn resolve_approval(
        &self,
        _: &str,
        _: &ApprovalDecision,
    ) -> Result<(), ports::HarnessError> {
        Err(ports::HarnessError::Unsupported(
            "This request can’t be allowed. It’s still waiting; deny it instead.".into(),
        ))
    }
    async fn shutdown(&self) -> Result<(), ports::HarnessError> {
        Ok(())
    }
}

async fn working_with_refusing_control(f: &Fixture, task_id: TaskId) -> RunId {
    let run = run_started(f, task_id).await;
    f.harness
        .emit(
            run,
            ports::HarnessEvent::SessionStarted {
                native_session: "session".into(),
                actual_model: None,
                actual_effort: None,
                account_label: None,
            },
        )
        .await;
    tokio::time::timeout(Duration::from_secs(15), async {
        while f
            .hub
            .read(|r| r.require::<Task>("task", task_id))
            .unwrap()
            .state
            != TaskState::Working
        {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("run did not start working");
    f.hub
        .inner
        .runs
        .lock()
        .await
        .insert(run, Arc::new(RefusingControl));
    run
}

#[tokio::test]
async fn an_answer_the_agent_refuses_says_why_and_leaves_the_request_waiting() {
    let f = Fixture::new().await;
    let owner = Caller::local_owner();
    let project = f.project().await;
    let (task_id, _) = f
        .task(project, TaskPlacement::StartNow, &owner, false)
        .await;
    let run = working_with_refusing_control(&f, task_id).await;
    f.harness
        .emit(
            run,
            ports::HarnessEvent::ApprovalRequested(ports::NativeApproval {
                native_request_id: "permissions".into(),
                kind: ApprovalKind::Command,
                title: "Use the network".into(),
                detail: None,
                detail_lines: Vec::new(),
                questions: Vec::new(),
                raw_request: None,
                scope: "turn".into(),
                shortened: false,
                tool_call: None,
            }),
        )
        .await;
    let approval = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if let Some(approval) = f
                .hub
                .read(|r| r.list::<Approval>("approval"))
                .unwrap()
                .into_iter()
                .next()
            {
                return approval;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("approval was not recorded");
    let request = edit(
        Command::ResolveApproval {
            approval_id: approval.approval_id,
            decision: ApprovalDecision::Allow,
        },
        approval.revision.into(),
    );
    let id = request.command_id;
    assert_eq!(
        f.hub.submit(&owner, request).await.status,
        ReceiptStatus::Accepted
    );
    settle(&f.hub, id).await;
    let command = f.hub.read(|r| r.command(id)).unwrap().unwrap();
    assert_eq!(command.stage, CommandStage::Failed, "{command:?}");
    assert_eq!(command.error.unwrap().code, ErrorCode::Unsupported);
    let approval: Approval = f
        .hub
        .read(|r| r.require("approval", approval.approval_id))
        .unwrap();
    assert_eq!(approval.state, ApprovalState::Pending);
    f.close().await;
}

#[tokio::test]
async fn a_message_the_agent_refuses_before_writing_fails_with_its_reason() {
    let f = Fixture::new().await;
    let owner = Caller::local_owner();
    let project = f.project().await;
    let (task_id, _) = f
        .task(project, TaskPlacement::StartNow, &owner, false)
        .await;
    working_with_refusing_control(&f, task_id).await;
    let receipt = f
        .hub
        .submit(
            &owner,
            CommandEnvelope::new(Command::SendMessage {
                task_id,
                text: "Look at these".into(),
                bundle: None,
                delivery: MessageDelivery::Steer,
            }),
        )
        .await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    settle(&f.hub, receipt.command_id).await;
    let command = f
        .hub
        .read(|r| r.command(receipt.command_id))
        .unwrap()
        .unwrap();
    assert_eq!(command.stage, CommandStage::Failed, "{command:?}");
    let error = command.error.unwrap();
    assert_eq!(error.code, ErrorCode::Unsupported);
    assert!(error.message.contains("too large"), "{error:?}");
    let timeline: Vec<TimelineItem> = f
        .hub
        .read(|r| r.list(&format!("timeline:{task_id}")))
        .unwrap();
    let message = timeline
        .iter()
        .find(|item| {
            matches!(&item.item,
                TimelineBody::UserMessage { text, .. } if text == "Look at these"
            )
        })
        .unwrap();
    assert!(timeline.iter().any(|item| matches!(item.item,
        TimelineBody::Delivery { message_seq, state: DeliveryState::Failed, .. } if message_seq == message.seq
    )));
    f.close().await;
}

#[tokio::test]
async fn a_task_whose_project_folder_is_gone_does_not_start_and_says_why() {
    let f = Fixture::new().await;
    let owner = Caller::local_owner();
    let folder = f.path.with_extension("moved-project");
    std::fs::create_dir_all(&folder).unwrap();
    let receipt = f
        .hub
        .submit(
            &owner,
            CommandEnvelope::new(Command::AddProject {
                host_path: folder.to_string_lossy().into_owned(),
                name: Some("Moved".into()),
            }),
        )
        .await;
    let Some(CommandResult::ProjectAdded { project_id }) = receipt.result else {
        panic!("{receipt:?}")
    };
    std::fs::remove_dir(&folder).unwrap();
    let (task_id, command) = f
        .task(project_id, TaskPlacement::StartNow, &owner, false)
        .await;
    settle(&f.hub, command.command_id).await;
    let task: Task = f.hub.read(|r| r.require("task", task_id)).unwrap();
    assert_eq!(task.state, TaskState::Failed);
    let error = f
        .hub
        .read(|r| r.command(command.command_id))
        .unwrap()
        .unwrap()
        .error
        .unwrap();
    assert_eq!(error.code, ErrorCode::NotFound);
    assert!(error.message.contains("project folder"), "{error:?}");
    assert_eq!(f.harness.starts.load(Ordering::SeqCst), 0);
    f.close().await;
}

#[tokio::test]
async fn archiving_a_waiting_task_takes_it_out_of_next_up() {
    let f = Fixture::new().await;
    let owner = Caller::local_owner();
    let project = f.project().await;
    let mut tasks = Vec::new();
    for _ in 0..2 {
        let (task, _) = f
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
        tasks.push(task);
    }
    let task: Task = f.hub.read(|r| r.require("task", tasks[0])).unwrap();
    let receipt = f
        .hub
        .submit(
            &owner,
            edit(Command::ArchiveTask { task_id: tasks[0] }, task.revision),
        )
        .await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    let queue: QueueView = f.hub.read(|r| r.require("queue", project)).unwrap();
    assert_eq!(
        queue.items.iter().map(|i| i.task_id).collect::<Vec<_>>(),
        vec![tasks[1]]
    );
    // The task behind it can now start.
    let next: Task = f.hub.read(|r| r.require("task", tasks[1])).unwrap();
    let receipt = f
        .hub
        .submit(
            &owner,
            edit(
                Command::QueueStart {
                    item_id: queue.items[0].item_id,
                    task_revision: next.revision,
                },
                queue.revision,
            ),
        )
        .await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    f.close().await;
}

#[tokio::test]
async fn start_by_itself_outlasts_later_additions_and_a_long_task_ahead() {
    let f = Fixture::new().await;
    let owner = Caller::local_owner();
    let project = f.project().await;
    let (busy, _) = f
        .task(project, TaskPlacement::StartNow, &owner, false)
        .await;
    let busy_run = run_started(&f, busy).await;
    let by_itself = TaskPlacement::Queue {
        position: None,
        authorization: QueueAuthorization::AutoStart,
    };
    let (first, _) = f.task(project, by_itself, &owner, false).await;
    f.task(project, by_itself, &owner, false).await;
    let queue: QueueView = f.hub.read(|r| r.require("queue", project)).unwrap();
    // A new project's Next up starts paused; adding these resumed it, and
    // adding the second kept the first's permission.
    assert!(!queue.paused);
    assert!(
        (queue.items.iter()).all(|i| i.authorization == QueueAuthorization::AutoStart
            && i.state == QueueItemState::Waiting)
    );
    // Hours later the task ahead ends, and the first starts by itself.
    f.clock.advance(Duration::from_secs(3 * 3_600));
    f.hub
        .finish_run(busy_run, RunState::Completed, None)
        .unwrap();
    f.hub.advance_queues().await.unwrap();
    run_started(&f, first).await;
    assert_eq!(f.harness.starts.load(Ordering::SeqCst), 2);
    f.close().await;
}

#[tokio::test]
async fn what_was_ready_to_start_when_ophio_stopped_starts_when_it_is_back() {
    let f = Fixture::new().await;
    let device = f.device().await;
    let project = f.project().await;
    let by_itself = TaskPlacement::Queue {
        position: None,
        authorization: QueueAuthorization::AutoStart,
    };
    let (task_id, _) = f.task(project, by_itself, &device, false).await;
    // Ophio stops with it ready to start and nothing left to move Next up on.
    f.hub.shutdown().await.unwrap();
    f.hub
        .write(|tx| {
            let mut queue: QueueView = tx.read().require("queue", project)?;
            queue.paused = false;
            tx.put("queue", project, &queue)
        })
        .unwrap();
    let task: Task = f.hub.read(|r| r.require("task", task_id)).unwrap();
    assert!(task.current_run.is_none());
    let f = f.reopened(|b| b).await;
    let mut more = waiting();
    while (f.hub.read(|r| r.require::<Task>("task", task_id)).unwrap())
        .current_run
        .is_none()
    {
        assert!(more(), "it didn't start: {:?}", f.hub.queue_view(project));
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    run_started(&f, task_id).await;
    f.close().await;
}

/// Fix by itself queues a run of fixes; archiving or renaming another task
/// in Next up meanwhile left every one of them waiting for a go-ahead the
/// person had already given, and the first asked for it by notification.
#[tokio::test]
async fn archiving_or_renaming_a_task_keeps_the_rest_starting_by_themselves() {
    let f = Fixture::new().await;
    let owner = Caller::local_owner();
    let project = f.project().await;
    let (busy, _) = f
        .task(project, TaskPlacement::StartNow, &owner, false)
        .await;
    let busy_run = run_started(&f, busy).await;
    let ask = TaskPlacement::Queue {
        position: None,
        authorization: QueueAuthorization::AskBeforeStart,
    };
    let by_itself = TaskPlacement::Queue {
        position: None,
        authorization: QueueAuthorization::AutoStart,
    };
    let (archived, _) = f.task(project, ask, &owner, false).await;
    let (first, _) = f.task(project, by_itself, &owner, false).await;
    let (second, _) = f.task(project, by_itself, &owner, false).await;
    let task: Task = f.hub.read(|r| r.require("task", archived)).unwrap();
    let receipt = f
        .hub
        .submit(
            &owner,
            edit(Command::ArchiveTask { task_id: archived }, task.revision),
        )
        .await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    let task: Task = f.hub.read(|r| r.require("task", first)).unwrap();
    let receipt = f
        .hub
        .submit(
            &owner,
            edit(
                Command::RenameTask {
                    task_id: first,
                    title: "Renamed".into(),
                    expected_title: None,
                },
                task.revision,
            ),
        )
        .await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    let queue: QueueView = f.hub.read(|r| r.require("queue", project)).unwrap();
    assert_eq!(
        (queue.items.iter())
            .filter(|i| i.is_pending())
            .map(|i| (i.task_id, i.authorization, i.state.clone()))
            .collect::<Vec<_>>(),
        [first, second].map(|task| (task, QueueAuthorization::AutoStart, QueueItemState::Waiting))
    );
    let open = || {
        f.hub
            .read(|r| r.list::<AttentionItem>("attention"))
            .unwrap()
            .into_iter()
            .filter(|a| a.resolved_at.is_none() && a.kind == AttentionKind::WaitingForYou)
            .count()
    };
    f.hub
        .finish_run(busy_run, RunState::Completed, None)
        .unwrap();
    assert_eq!(open(), 0, "nothing asks for a go-ahead already given");
    f.hub.advance_queues().await.unwrap();
    run_started(&f, first).await;
    assert_eq!(open(), 0);
    f.close().await;
}

/// Taking a task out of Next up asks again only of what waited for it.
#[tokio::test]
async fn removing_a_task_asks_again_only_of_what_waited_for_it() {
    let f = Fixture::new().await;
    let owner = Caller::local_owner();
    let project = f.project().await;
    let (busy, _) = f
        .task(project, TaskPlacement::StartNow, &owner, false)
        .await;
    run_started(&f, busy).await;
    let by_itself = TaskPlacement::Queue {
        position: None,
        authorization: QueueAuthorization::AutoStart,
    };
    for _ in 0..3 {
        f.task(project, by_itself, &owner, false).await;
    }
    let queue: QueueView = f.hub.read(|r| r.require("queue", project)).unwrap();
    let items: Vec<_> = (queue.items.iter())
        .filter(|i| i.is_pending())
        .map(|i| i.item_id)
        .collect();
    let submit = |command: Command| async {
        let queue: QueueView = f.hub.read(|r| r.require("queue", project)).unwrap();
        let receipt = f.hub.submit(&owner, edit(command, queue.revision)).await;
        assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    };
    // The second waits on the first; saying so asks again of it, and the
    // person lets it start by itself once more.
    submit(Command::QueueSetDependencies {
        item_id: items[1],
        depends_on: vec![items[0]],
    })
    .await;
    let authorization = |item: QueueItemId| {
        let queue: QueueView = f.hub.read(|r| r.require("queue", project)).unwrap();
        (queue.items.iter())
            .find(|i| i.item_id == item)
            .map(|i| i.authorization)
    };
    assert_eq!(authorization(items[0]), Some(QueueAuthorization::AutoStart));
    assert_eq!(
        authorization(items[1]),
        Some(QueueAuthorization::AskBeforeStart)
    );
    assert_eq!(authorization(items[2]), Some(QueueAuthorization::AutoStart));
    submit(Command::QueueSetAuthorization {
        item_id: items[1],
        authorization: QueueAuthorization::AutoStart,
    })
    .await;
    submit(Command::QueueRemove { item_id: items[0] }).await;
    assert_eq!(authorization(items[0]), None);
    assert_eq!(
        authorization(items[1]),
        Some(QueueAuthorization::AskBeforeStart)
    );
    assert_eq!(authorization(items[2]), Some(QueueAuthorization::AutoStart));
    let permits = f
        .hub
        .read(|r| r.get::<crate::runs::QueuePermit>("queue_permit", items[2]))
        .unwrap()
        .unwrap();
    let queue: QueueView = f.hub.read(|r| r.require("queue", project)).unwrap();
    assert_eq!(permits.queue_revision, queue.revision);
    assert!(
        f.hub
            .read(|r| r.get::<crate::runs::QueuePermit>("queue_permit", items[1]))
            .unwrap()
            .is_none()
    );
    f.close().await;
}

#[tokio::test]
async fn a_go_ahead_put_away_and_back_asks_again_without_another_alert() {
    let f = Fixture::new().await;
    let owner = Caller::local_owner();
    let project = f.project().await;
    let ask = TaskPlacement::Queue {
        position: None,
        authorization: QueueAuthorization::AskBeforeStart,
    };
    let (first, _) = f.task(project, ask, &owner, false).await;
    let queue = || -> QueueView { f.hub.read(|r| r.require("queue", project)).unwrap() };
    let act = async |command: Command| {
        let envelope = edit(command, queue().revision);
        let id = envelope.command_id;
        let receipt = f.hub.submit(&owner, envelope).await;
        assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
        settle(&f.hub, id).await;
    };
    if queue().paused {
        act(Command::QueueResume {
            project_id: project,
        })
        .await;
    }
    // Each ask open now: its task, and whether it was quiet.
    let asking = || -> Vec<(TaskId, bool)> {
        let items = f
            .hub
            .read(|r| r.list::<AttentionItem>("attention"))
            .unwrap();
        (items.into_iter())
            .filter(|a| a.resolved_at.is_none() && a.kind == AttentionKind::WaitingForYou)
            .map(|a| (a.task_id.unwrap(), a.quiet))
            .collect()
    };
    assert_eq!(asking(), [(first, false)]);

    // Paused, it is put away; resumed, it asks again, quietly.
    act(Command::QueuePause {
        project_id: project,
    })
    .await;
    assert_eq!(asking(), []);
    act(Command::QueueResume {
        project_id: project,
    })
    .await;
    assert_eq!(asking(), [(first, true)]);

    // Another put in front of it is asked for out loud.
    let (second, _) = f.task(project, ask, &owner, false).await;
    let item_id = (queue().items.iter())
        .find(|i| i.task_id == second)
        .unwrap()
        .item_id;
    act(Command::QueueMove {
        item_id,
        position: 0,
    })
    .await;
    assert_eq!(asking(), [(second, false)]);
    f.close().await;
}
