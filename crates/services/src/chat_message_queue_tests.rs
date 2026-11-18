// SPDX-License-Identifier: Apache-2.0
use super::*;
use crate::{Hub, testing::FakeHarness};
use protocol::capability::*;
use std::sync::{Arc, atomic::Ordering};

struct Fixture {
    hub: CompanionHub,
    path: PathBuf,
    claude: Arc<FakeHarness>,
    codex: Arc<FakeHarness>,
    task_id: TaskId,
}

impl Fixture {
    async fn build(path: &Path, claude: Arc<FakeHarness>, codex: Arc<FakeHarness>) -> CompanionHub {
        let hub = CompanionHub::builder()
            .data_dir(path)
            .harness(claude)
            .harness(codex)
            .bridge(crate::bridge::BridgeConfig::new(
                "synthetic-bridge",
                "synthetic-socket",
            ))
            .build()
            .await
            .unwrap();
        hub.write(|tx| {
            let mut infos: Vec<HarnessInfo> = tx.read().require("meta", "harnesses")?;
            for info in &mut infos {
                info.modes = [TaskMode::Ask, TaskMode::Plan, TaskMode::Work]
                    .into_iter()
                    .map(|mode| ModeSupport {
                        mode,
                        availability: Availability::Available,
                        enforcement: "Synthetic policy".into(),
                    })
                    .collect();
            }
            tx.put("meta", "harnesses", &infos)
        })
        .unwrap();
        hub
    }

    async fn new() -> Self {
        Self::with_writing(false).await
    }

    async fn with_writing(writing: bool) -> Self {
        let path =
            std::env::temp_dir().join(format!("ophio-queued-choice-{}", uuid::Uuid::new_v4()));
        let claude = Arc::new(FakeHarness::default());
        let codex = Arc::new(FakeHarness::of_kind(HarnessKind::Codex));
        let hub = Self::build(&path, claude.clone(), codex.clone()).await;
        let project = path.with_extension("project");
        std::fs::create_dir_all(&project).unwrap();
        let receipt = hub
            .submit(
                &Caller::local_owner(),
                CommandEnvelope::new(Command::AddProject {
                    host_path: project.to_string_lossy().into_owned(),
                    name: Some("Synthetic queue".into()),
                }),
            )
            .await;
        let Some(CommandResult::ProjectAdded { project_id }) = receipt.result else {
            panic!("{receipt:?}")
        };
        let command = serde_json::from_value(serde_json::json!({
            "command":"create_task", "project_id":project_id, "kind":"coding", "mode":"ask",
            "title":"Chat", "prompt":"original", "harness":"claude_code", "model":"old-model",
            "effort":"old-effort", "placement":{"mode":"start_now"}, "conversation":true,
            "access": writing.then_some(protocol::access::ConversationAccess { whole_read: true, whole_write: true, internet: true, ..Default::default() })
        }))
        .unwrap();
        let receipt = hub
            .submit(&Caller::local_owner(), CommandEnvelope::new(command))
            .await;
        let Some(CommandResult::TaskCreated { task_id, .. }) = receipt.result else {
            panic!("{receipt:?}")
        };
        let f = Self {
            hub,
            path,
            claude,
            codex,
            task_id,
        };
        f.started().await;
        f
    }

    fn task(&self) -> Task {
        self.hub.read(|r| r.require("task", self.task_id)).unwrap()
    }
    fn run(&self) -> Run {
        let run_id = self.task().current_run.unwrap();
        self.hub.read(|r| r.require("run", run_id)).unwrap()
    }
    async fn spec(&self) -> ports::RunSpec {
        let run = self.run();
        let adapter = if run.harness == HarnessKind::ClaudeCode {
            &self.claude
        } else {
            &self.codex
        };
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                if let Some(spec) = adapter
                    .specs
                    .lock()
                    .unwrap()
                    .iter()
                    .find(|s| s.run_id == run.run_id)
                    .cloned()
                {
                    return spec;
                }
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
        .await
        .unwrap()
    }
    async fn started(&self) {
        self.spec().await;
        let run = self.run().run_id;
        self.hub
            .harness_event(
                run,
                HarnessEvent::SessionStarted {
                    native_session: format!("session-{run}"),
                    actual_model: None,
                    actual_effort: None,
                    account_label: None,
                },
            )
            .await
            .unwrap();
        self.hub
            .harness_event(run, HarnessEvent::TurnStarted)
            .await
            .unwrap();
    }
    fn finish(&self) {
        self.hub
            .finish_run(self.run().run_id, RunState::Completed, None)
            .unwrap();
    }
    async fn send(
        &self,
        text: &str,
        harness: Option<HarnessKind>,
    ) -> (CommandEnvelope, CommandReceipt) {
        let envelope = CommandEnvelope::new(Command::SendMessage {
            task_id: self.task_id,
            text: text.into(),
            bundle: None,
            delivery: MessageDelivery::NextTurn,
            harness,
            model: None,
            effort: None,
        });
        let receipt = self
            .hub
            .submit(&Caller::local_owner(), envelope.clone())
            .await;
        if receipt.status == ReceiptStatus::Accepted {
            tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    let pending = self
                        .hub
                        .read(|r| Ok(r.command(envelope.command_id)?.unwrap().pending))
                        .unwrap();
                    if !pending {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(2)).await;
                }
            })
            .await
            .unwrap();
        }
        (envelope, receipt)
    }
    async fn resume(&self, text: &str, harness: Option<HarnessKind>) -> CommandReceipt {
        let mut envelope = CommandEnvelope::new(Command::ResumeTask {
            observed_reply: None,
            task_id: self.task_id,
            text: Some(text.into()),
            bundle: None,
            harness,
            model: None,
            effort: None,
            carried_plan: None,
            mode: None,
            read_only: false,
        });
        envelope.expected_revision = Some(self.task().revision);
        self.hub.submit(&Caller::local_owner(), envelope).await
    }
    fn queued(&self) -> Vec<QueuedTurn> {
        let mut rows = self.hub.read(|r| r.list::<QueuedTurn>(QUEUED)).unwrap();
        rows.sort_by_key(|m| m.seq);
        rows
    }
    async fn close(self) {
        self.hub.shutdown().await.unwrap();
        drop(self.hub);
        std::fs::remove_dir_all(self.path.with_extension("project")).unwrap();
        std::fs::remove_dir_all(self.path).unwrap();
    }
}

#[tokio::test]
async fn queue_keeps_each_recipient_and_defaults_without_changing_the_running_task() {
    let f = Fixture::new().await;
    let original = f.task();
    let (first, receipt) = f.send("for-codex-only", Some(HarnessKind::Codex)).await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    let (_, receipt) = f.send("legacy-for-claude-only", None).await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    let (_, receipt) = f
        .send("same-agent-defaults", Some(HarnessKind::ClaudeCode))
        .await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    assert_eq!(f.task().harness, original.harness);
    assert_eq!(f.task().model, original.model);
    assert_eq!(f.task().revision, original.revision);
    let rows = f.queued();
    assert_eq!(
        rows[0].choice,
        AgentChoice {
            harness: HarnessKind::Codex,
            model: None,
            effort: None
        }
    );
    assert_eq!(rows[1].choice, AgentChoice::of(&original));
    assert_eq!(
        rows[2].choice,
        AgentChoice {
            harness: HarnessKind::ClaudeCode,
            model: None,
            effort: None
        }
    );
    let duplicate = f.hub.submit(&Caller::local_owner(), first).await;
    assert_eq!(duplicate.status, ReceiptStatus::Duplicate);
    assert_eq!(f.queued().len(), 3);
    assert_eq!(f.claude.controls.messages.load(Ordering::SeqCst), 0);
    assert_eq!(f.codex.controls.messages.load(Ordering::SeqCst), 0);

    f.finish();
    f.hub.advance_queues().await.unwrap();
    assert_eq!(f.task().harness, HarnessKind::Codex);
    assert!(f.task().model.is_none() && f.task().effort.is_none());
    let spec = f.spec().await;
    assert!(spec.prompt.text.contains("for-codex-only"));
    assert!(!spec.prompt.text.contains("legacy-for-claude-only"));
    assert!(!spec.prompt.text.contains("same-agent-defaults"));
    f.started().await;
    f.finish();
    f.hub.advance_queues().await.unwrap();
    assert_eq!(f.task().harness, HarnessKind::ClaudeCode);
    assert_eq!(f.task().model.as_deref(), Some("old-model"));
    assert_eq!(f.task().effort.as_deref(), Some("old-effort"));
    f.started().await;
    f.finish();
    f.hub.advance_queues().await.unwrap();
    assert_eq!(f.task().harness, HarnessKind::ClaudeCode);
    assert!(f.task().model.is_none() && f.task().effort.is_none());
    f.started().await;
    assert!(f.queued().is_empty());
    f.finish();
    f.close().await;
}

#[tokio::test]
async fn explicit_choices_are_refused_for_steering_structured_tasks_and_mode_conversions() {
    let f = Fixture::new().await;
    let mut body = Command::SendMessage {
        task_id: f.task_id,
        text: "steer".into(),
        bundle: None,
        delivery: MessageDelivery::Steer,
        harness: Some(HarnessKind::Codex),
        model: None,
        effort: None,
    };
    let receipt = f
        .hub
        .submit(&Caller::local_owner(), CommandEnvelope::new(body.clone()))
        .await;
    assert_eq!(receipt.status, ReceiptStatus::Rejected);
    assert_eq!(receipt.error.unwrap().code, ErrorCode::InvalidRequest);
    let mut task = f.task();
    task.conversation = false;
    f.hub.write(|tx| tx.put("task", f.task_id, &task)).unwrap();
    if let Command::SendMessage { delivery, .. } = &mut body {
        *delivery = MessageDelivery::NextTurn;
    }
    let receipt = f
        .hub
        .submit(&Caller::local_owner(), CommandEnvelope::new(body))
        .await;
    assert_eq!(receipt.status, ReceiptStatus::Rejected);
    assert!(f.queued().is_empty());
    f.finish();
    for (conversation, mode, read_only) in [
        (false, None, false),
        (true, Some(TaskMode::Work), false),
        (true, None, true),
    ] {
        let mut task = f.task();
        task.conversation = conversation;
        f.hub.write(|tx| tx.put("task", f.task_id, &task)).unwrap();
        let mut envelope = CommandEnvelope::new(Command::ResumeTask {
            observed_reply: None,
            task_id: f.task_id,
            text: Some("continue".into()),
            bundle: None,
            harness: Some(HarnessKind::Codex),
            model: None,
            effort: None,
            carried_plan: None,
            mode,
            read_only,
        });
        envelope.expected_revision = Some(task.revision);
        let receipt = f.hub.submit(&Caller::local_owner(), envelope).await;
        assert_eq!(receipt.status, ReceiptStatus::Rejected, "{receipt:?}");
        assert_eq!(receipt.error.unwrap().code, ErrorCode::InvalidRequest);
        assert_eq!(f.task().harness, HarnessKind::ClaudeCode);
    }
    f.close().await;
}

#[tokio::test]
async fn resume_refuses_to_pass_an_older_queued_chat_message_and_advances_it() {
    let f = Fixture::new().await;
    let (_, receipt) = f.send("older-held-message", None).await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    assert_eq!(f.queued().len(), 1);
    // Model the old turn completing without scheduling queue progression: the
    // refused resume must schedule it, rather than leaving the held message stuck.
    f.hub
        .write(|tx| {
            let mut task: Task = tx.read().require("task", f.task_id)?;
            task.state = TaskState::Completed;
            tx.put("task", f.task_id, &task)?;
            let mut run: Run = tx.read().require("run", task.current_run.unwrap())?;
            run.state = RunState::Completed;
            tx.put("run", run.run_id, &run)
        })
        .unwrap();
    let mut envelope = CommandEnvelope::new(Command::ResumeTask {
        observed_reply: None,
        task_id: f.task_id,
        text: Some("later-resume".into()),
        bundle: None,
        harness: None,
        model: None,
        effort: None,
        carried_plan: None,
        mode: None,
        read_only: false,
    });
    envelope.expected_revision = Some(f.task().revision);
    let previous_run = f.run().run_id;
    let receipt = f.hub.submit(&Caller::local_owner(), envelope).await;
    assert_eq!(receipt.status, ReceiptStatus::Rejected, "{receipt:?}");
    let error = receipt.error.unwrap();
    assert_eq!(error.code, ErrorCode::Conflict);
    assert_eq!(
        error.message,
        "An earlier message in this chat is about to be sent. Send this again in a moment."
    );
    tokio::time::timeout(Duration::from_secs(10), async {
        while f.task().current_run == Some(previous_run) {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap();
    let spec = f.spec().await;
    assert!(spec.prompt.text.contains("older-held-message"));
    assert!(!spec.prompt.text.contains("later-resume"));
    assert!(f.queued().is_empty());
    f.started().await;
    f.finish();
    f.close().await;
}

#[tokio::test]
async fn unchanged_full_choice_is_allowed_during_a_side_turn_but_changes_are_refused() {
    let f = Fixture::new().await;
    let run_id = f.run().run_id;
    f.hub
        .write(|tx| tx.put(crate::runs::SIDE_TURN, run_id, &TaskState::Accepted))
        .unwrap();
    let send = |model: &str| {
        CommandEnvelope::new(Command::SendMessage {
            task_id: f.task_id,
            text: "side-turn message".into(),
            bundle: None,
            delivery: MessageDelivery::NextTurn,
            harness: Some(HarnessKind::ClaudeCode),
            model: Some(model.into()),
            effort: Some("old-effort".into()),
        })
    };
    let receipt = f
        .hub
        .submit(&Caller::local_owner(), send("old-model"))
        .await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    let receipt = f
        .hub
        .submit(&Caller::local_owner(), send("new-model"))
        .await;
    assert_eq!(receipt.status, ReceiptStatus::Rejected, "{receipt:?}");
    assert_eq!(receipt.error.unwrap().code, ErrorCode::InvalidRequest);
    assert_eq!(f.queued().len(), 1);
    f.hub
        .write(|tx| tx.delete(crate::runs::SIDE_TURN, run_id))
        .unwrap();
    f.finish();
    f.hub.advance_queues().await.unwrap();
    f.started().await;
    f.finish();
    f.close().await;
}

#[tokio::test]
async fn queued_choices_recheck_readiness_and_cancel_without_native_support() {
    let f = Fixture::new().await;
    let (_, receipt) = f.send("cancel-this", Some(HarnessKind::Codex)).await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted);
    let seq = f.queued()[0].seq;
    f.hub.cancel_message(f.task_id, seq, false).await.unwrap();
    assert!(f.queued().is_empty());
    let (_, receipt) = f.send("sign-in-went-away", Some(HarnessKind::Codex)).await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted);
    f.hub
        .write(|tx| {
            let mut infos: Vec<HarnessInfo> = tx.read().require("meta", "harnesses")?;
            infos
                .iter_mut()
                .find(|h| h.kind == HarnessKind::Codex)
                .unwrap()
                .account = AccountReadiness::SignedOut {
                remedy: "Sign in again".into(),
            };
            tx.put("meta", "harnesses", &infos)
        })
        .unwrap();
    f.finish();
    f.hub.advance_queues().await.unwrap();
    assert_eq!(f.task().harness, HarnessKind::ClaudeCode);
    assert!(f.queued().is_empty());
    assert_eq!(f.codex.starts.load(Ordering::SeqCst), 0);
    f.close().await;
}

#[tokio::test]
async fn queued_choices_survive_reopening_after_safe_completion() {
    let f = Fixture::new().await;
    let (_, receipt) = f.send("saved-for-codex", Some(HarnessKind::Codex)).await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted);
    // Ensure the first command worker has settled before simulating restart.
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let row = f.queued().remove(0);
            let pending = f
                .hub
                .read(|r| Ok(r.command(row.command_id)?.unwrap().pending))
                .unwrap();
            if !pending {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap();
    f.finish();
    assert_eq!(f.queued()[0].choice.harness, HarnessKind::Codex);
    f.hub.shutdown().await.unwrap();
    crate::testing::drop_hub_and_wait(f.hub, std::time::Duration::from_secs(10)).await;
    let hub = Fixture::build(&f.path, f.claude.clone(), f.codex.clone()).await;
    let f = Fixture { hub, ..f };
    // Startup itself may already have admitted the saved message.
    f.hub.advance_queues().await.unwrap();
    assert_eq!(f.task().harness, HarnessKind::Codex);
    assert!(f.task().model.is_none() && f.task().effort.is_none());
    f.started().await;
    assert!(f.queued().is_empty());
    f.finish();
    f.close().await;
}

#[tokio::test]
async fn pending_next_turn_message_fails_on_restart_and_chat_accepts_a_retry() {
    let f = Fixture::new().await;
    let (command, receipt) = f.send("interrupted-send", Some(HarnessKind::Codex)).await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted);
    let message = f.queued().remove(0);
    let original_run = f.run().run_id;
    f.finish();
    // A send worker held by the run's message lock can still be pending
    // after the reply finishes. Restore that durable crash-window state.
    f.hub
        .write(|tx| {
            let mut work = tx.read().command(command.command_id)?.unwrap();
            work.pending = true;
            work.stage = CommandStage::HostAccepted;
            tx.command(&work)
        })
        .unwrap();
    assert_eq!(f.task().state, TaskState::Completed);
    assert_eq!(f.queued().len(), 1);
    f.hub.shutdown().await.unwrap();
    crate::testing::drop_hub_and_wait(f.hub, std::time::Duration::from_secs(10)).await;
    let hub = Fixture::build(&f.path, f.claude.clone(), f.codex.clone()).await;
    let f = Fixture { hub, ..f };
    assert!(f.queued().is_empty());
    let work = f
        .hub
        .read(|r| Ok(r.command(command.command_id)?.unwrap()))
        .unwrap();
    assert_eq!(work.stage, CommandStage::ReconciliationRequired);
    assert!(!work.pending);
    assert_eq!(f.run().run_id, original_run);
    let timeline: Vec<TimelineItem> = f
        .hub
        .read(|r| r.list(&format!("timeline:{}", f.task_id)))
        .unwrap();
    assert_eq!(
        timeline
            .iter()
            .filter(|item| matches!(item.item, TimelineBody::Delivery {
                message_seq,
                state: DeliveryState::Failed,
                ..
            } if message_seq == message.seq))
            .count(),
        1
    );
    let receipt = f.resume("send-again", Some(HarnessKind::Codex)).await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    f.started().await;
    assert_ne!(f.run().run_id, original_run);
    assert_eq!(f.task().harness, HarnessKind::Codex);
    assert!(f.queued().is_empty());
    f.finish();
    assert_eq!(f.task().state, TaskState::Completed);
    f.close().await;
}

#[tokio::test]
async fn queued_next_turn_messages_do_not_wait_on_terminal_send_commands() {
    for stage in [
        CommandStage::ReconciliationRequired,
        CommandStage::Uncertain,
        CommandStage::Failed,
    ] {
        let f = Fixture::new().await;
        let (command, receipt) = f.send("cannot-be-observed", None).await;
        assert_eq!(receipt.status, ReceiptStatus::Accepted);
        f.finish();
        f.hub
            .write(|tx| {
                let mut work = tx.read().command(command.command_id)?.unwrap();
                work.stage = stage;
                tx.command(&work)
            })
            .unwrap();
        f.hub.advance_queues().await.unwrap();
        assert!(f.queued().is_empty(), "terminal stage {stage:?}");
        let receipt = f.resume("send-again", None).await;
        assert_eq!(
            receipt.status,
            ReceiptStatus::Accepted,
            "{stage:?}: {receipt:?}"
        );
        f.started().await;
        f.finish();
        f.close().await;
    }
}

#[tokio::test]
async fn admission_rejects_unready_destinations_and_unsupported_choices() {
    let f = Fixture::new().await;
    let original: Vec<HarnessInfo> = f.hub.read(|r| r.require("meta", "harnesses")).unwrap();
    for case in 0..7 {
        let mut infos = original.clone();
        let info = infos
            .iter_mut()
            .find(|h| h.kind == HarnessKind::Codex)
            .unwrap();
        let unsupported = Availability::Unsupported {
            reason: "Synthetic refusal".into(),
        };
        let mut model = None;
        let mut effort = None;
        match case {
            0 => info.capabilities.start_run = unsupported,
            1 => {
                info.account = AccountReadiness::SignedOut {
                    remedy: "Sign in".into(),
                }
            }
            2 => {
                info.modes
                    .iter_mut()
                    .find(|m| m.mode == TaskMode::Ask)
                    .unwrap()
                    .availability = unsupported
            }
            3 => {
                info.capabilities.model_selection = unsupported;
                model = Some("chosen".into());
            }
            4 => {
                info.capabilities.effort_selection = unsupported;
                effort = Some("high".into());
            }
            5 => {
                effort = Some("not-supported".into());
                info.catalog = Some(CatalogObservation {
                    checked_at: protocol::time::Timestamp(1),
                    source: "Synthetic".into(),
                    availability: Availability::Available,
                    default_model: None,
                    default_effort: None,
                });
            }
            _ => {
                let mut task = f.task();
                task.launch_policy = Some(LaunchPolicy::AcceptEdits);
                f.hub.write(|tx| tx.put("task", f.task_id, &task)).unwrap();
            }
        }
        f.hub
            .write(|tx| tx.put("meta", "harnesses", &infos))
            .unwrap();
        let receipt = f
            .hub
            .submit(
                &Caller::local_owner(),
                CommandEnvelope::new(Command::SendMessage {
                    task_id: f.task_id,
                    text: "refused-choice".into(),
                    bundle: None,
                    delivery: MessageDelivery::NextTurn,
                    harness: Some(HarnessKind::Codex),
                    model,
                    effort,
                }),
            )
            .await;
        assert_eq!(
            receipt.status,
            ReceiptStatus::Rejected,
            "case {case}: {receipt:?}"
        );
        assert!(f.queued().is_empty());
        assert_eq!(f.task().harness, HarnessKind::ClaudeCode);
    }
    assert_eq!(f.codex.starts.load(Ordering::SeqCst), 0);
    f.finish();
    f.close().await;
}

#[tokio::test]
async fn revoked_sender_cannot_start_the_saved_recipient() {
    use protocol::auth::{ChannelIdentity, ChannelKind, Grants, Principal};
    let f = Fixture::new().await;
    let identity_id = ChannelIdentityId::new();
    let mut channel = ChannelIdentity {
        id: identity_id,
        channel: ChannelKind::Telegram,
        external_user: "synthetic".into(),
        external_chat: "synthetic".into(),
        display_name: "Synthetic sender".into(),
        linked_at: protocol::time::Timestamp(1),
        revoked_at: None,
        grants: Grants::owner(),
        project_id: None,
    };
    f.hub
        .write(|tx| tx.put("channel", identity_id, &channel))
        .unwrap();
    let caller = Caller {
        principal: Principal::Channel { identity_id },
        grants: Grants::owner(),
    };
    let envelope = CommandEnvelope::new(Command::SendMessage {
        task_id: f.task_id,
        text: "revoked-before-delivery".into(),
        bundle: None,
        delivery: MessageDelivery::NextTurn,
        harness: Some(HarnessKind::Codex),
        model: None,
        effort: None,
    });
    let receipt = f.hub.submit(&caller, envelope).await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    channel.revoked_at = Some(protocol::time::Timestamp(2));
    f.hub
        .write(|tx| tx.put("channel", identity_id, &channel))
        .unwrap();
    f.finish();
    f.hub.advance_queues().await.unwrap();
    assert!(f.queued().is_empty());
    assert_eq!(f.task().harness, HarnessKind::ClaudeCode);
    assert_eq!(f.codex.starts.load(Ordering::SeqCst), 0);
    f.close().await;
}

#[tokio::test]
async fn stop_withdraws_queued_agent_switches() {
    let f = Fixture::new().await;
    let (_, receipt) = f.send("never-after-stop", Some(HarnessKind::Codex)).await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted);
    let receipt = f
        .hub
        .submit(
            &Caller::local_owner(),
            CommandEnvelope::new(Command::StopTask {
                task_id: f.task_id,
                force: false,
            }),
        )
        .await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    assert!(f.queued().is_empty());
    assert_eq!(f.task().harness, HarnessKind::ClaudeCode);
    f.hub
        .finish_run(f.run().run_id, RunState::Interrupted, None)
        .unwrap();
    f.hub.advance_queues().await.unwrap();
    assert_eq!(f.codex.starts.load(Ordering::SeqCst), 0);
    f.close().await;
}

#[tokio::test]
async fn held_messages_advance_between_writing_chat_turns_without_self_blocking() {
    let f = Fixture::with_writing(true).await;
    let first = f.run();
    assert!(first.holds_writer);
    let (_, receipt) = f.send("next writing turn", None).await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    assert_eq!(f.queued().len(), 1);
    f.finish();
    f.hub.advance_queues().await.unwrap();
    f.started().await;
    let next = f.run();
    assert_ne!(next.run_id, first.run_id);
    assert!(next.holds_writer);
    assert!(f.queued().is_empty());
    let project_id = f.task().project_id;
    let project: Project = f.hub.read(|r| r.require("project", project_id)).unwrap();
    assert_eq!(project.active_writer, Some(next.run_id));
    f.close().await;
}
