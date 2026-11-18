// SPDX-License-Identifier: Apache-2.0
use super::*;
use crate::{CompanionHub, Hub, api::Caller, testing::FakeHarness};
use ports::{HarnessEvent, RunSpec};
use protocol::{capability::*, command::*, time::Timestamp, *};
use std::{
    path::PathBuf,
    sync::{Arc, atomic::Ordering},
    time::Duration,
};

struct Fixture {
    hub: CompanionHub,
    path: PathBuf,
    claude: Arc<FakeHarness>,
    codex: Arc<FakeHarness>,
    task: TaskId,
}

impl Fixture {
    async fn build(
        path: &std::path::Path,
        claude: Arc<FakeHarness>,
        codex: Arc<FakeHarness>,
    ) -> CompanionHub {
        let hub = CompanionHub::builder()
            .data_dir(path)
            .harness(claude)
            .harness(codex)
            .sandboxes(Arc::new(crate::tests::FakeSandboxes::default()))
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
        Self::with_access(None).await
    }
    async fn with_access(access: Option<protocol::access::ConversationAccess>) -> Self {
        let path =
            std::env::temp_dir().join(format!("ophio-chat-handover-{}", uuid::Uuid::new_v4()));
        let claude = Arc::new(FakeHarness::default());
        let codex = Arc::new(FakeHarness::of_kind(HarnessKind::Codex));
        let hub = Self::build(&path, claude.clone(), codex.clone()).await;
        let project_path = path.with_extension("project");
        std::fs::create_dir_all(&project_path).unwrap();
        let added = hub
            .submit(
                &Caller::local_owner(),
                CommandEnvelope::new(Command::AddProject {
                    host_path: project_path.to_string_lossy().into_owned(),
                    name: Some("Synthetic chat".into()),
                }),
            )
            .await;
        let Some(CommandResult::ProjectAdded { project_id }) = added.result else {
            panic!("{added:?}")
        };
        let create = serde_json::from_value(serde_json::json!({
            "command": "create_task", "project_id": project_id, "kind": "coding", "mode": "ask",
            "title": "Chat", "prompt": "first-person-message", "bundle": null,
            "harness": "claude_code", "model": null, "effort": null, "mission": null,
            "target_id": null, "placement": {"mode":"start_now"}, "conversation":true,
            "access": access
        }))
        .unwrap();
        let started = hub
            .submit(&Caller::local_owner(), CommandEnvelope::new(create))
            .await;
        let Some(CommandResult::TaskCreated { task_id, .. }) = started.result else {
            panic!("{started:?}")
        };
        Self {
            hub,
            path,
            claude,
            codex,
            task: task_id,
        }
    }
    fn task(&self) -> Task {
        self.hub.read(|r| r.require("task", self.task)).unwrap()
    }
    fn run(&self) -> Run {
        let id = self.task().current_run.unwrap();
        self.hub.read(|r| r.require("run", id)).unwrap()
    }
    async fn spec(&self) -> RunSpec {
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
    async fn finish(&self, session: &str, reply: &str) {
        self.spec().await;
        let run = self.run().run_id;
        self.hub
            .harness_event(
                run,
                HarnessEvent::SessionStarted {
                    native_session: session.into(),
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
        self.hub
            .harness_event(
                run,
                HarnessEvent::AgentMessage {
                    text: reply.into(),
                    message_key: uuid::Uuid::new_v4().to_string(),
                },
            )
            .await
            .unwrap();
        self.hub.finish_run(run, RunState::Completed, None).unwrap();
    }
    async fn turn(&self, harness: HarnessKind, text: &str) -> CommandEnvelope {
        let mut command = CommandEnvelope::new(Command::ResumeTask {
            observed_reply: None,
            task_id: self.task,
            harness: Some(harness),
            text: Some(text.into()),
            model: None,
            effort: None,
            mode: None,
            bundle: None,
            carried_plan: None,
            read_only: false,
        });
        command.expected_revision = Some(self.task().revision);
        let receipt = self
            .hub
            .submit(&Caller::local_owner(), command.clone())
            .await;
        assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
        command
    }
    async fn reopened(self) -> Self {
        self.hub.shutdown().await.unwrap();
        crate::testing::drop_hub_and_wait(self.hub, std::time::Duration::from_secs(10)).await;
        let hub = Self::build(&self.path, self.claude.clone(), self.codex.clone()).await;
        Self { hub, ..self }
    }
    async fn close(self) {
        self.hub.shutdown().await.unwrap();
        drop(self.hub);
        std::fs::remove_dir_all(self.path.with_extension("project")).unwrap();
        std::fs::remove_dir_all(self.path).unwrap();
    }
}

#[tokio::test]
async fn switches_resume_only_the_agents_own_session_with_only_missed_chat() {
    let f = Fixture::new().await;
    let access = f.task().access;
    f.finish("claude-private-session", "claude-first-answer")
        .await;
    f.turn(HarnessKind::Codex, "codex-person-message").await;
    let spec = f.spec().await;
    assert!(spec.resume_session.is_none());
    assert!(spec.prompt.text.contains("first-person-message"));
    assert!(
        spec.prompt
            .text
            .contains("Claude Code: claude-first-answer")
    );
    assert!(!spec.prompt.text.contains("claude-private-session"));
    assert_eq!(spec.prompt.text.matches("codex-person-message").count(), 1);
    assert!(
        spec.prompt.text.find("claude-first-answer").unwrap()
            < spec.prompt.text.find("codex-person-message").unwrap()
    );
    assert!(!f.run().chat_handover.unwrap().resumed);
    assert_eq!(f.task().access, access);
    f.finish("codex-private-session", "codex-first-answer")
        .await;
    let f = f.reopened().await;
    f.turn(HarnessKind::ClaudeCode, "back-to-claude").await;
    let spec = f.spec().await;
    assert_eq!(
        spec.resume_session.as_deref(),
        Some("claude-private-session")
    );
    assert!(!spec.prompt.text.contains("first-person-message"));
    assert!(!spec.prompt.text.contains("claude-first-answer"));
    assert!(spec.prompt.text.contains("codex-person-message"));
    assert!(spec.prompt.text.contains("Codex: codex-first-answer"));
    assert!(!spec.prompt.text.contains("codex-private-session"));
    let boundary = f.run().chat_handover.unwrap();
    assert!(boundary.resumed && boundary.after_seq.is_some());
    f.finish("claude-private-session", "back-answer").await;
    f.turn(HarnessKind::ClaudeCode, "ordinary-next").await;
    let spec = f.spec().await;
    assert!(f.run().chat_handover.is_none());
    assert!(!spec.prompt.text.contains("codex-first-answer"));
    f.finish("claude-private-session", "done").await;
    f.close().await;
}

#[tokio::test]
async fn a_failed_switch_keeps_the_exact_handover_across_restart_and_retry() {
    let f = Fixture::new().await;
    f.finish("claude-session", "first-answer").await;
    f.codex.fail_spawn.store(true, Ordering::SeqCst);
    let original = f.turn(HarnessKind::Codex, "switch-message").await;
    f.spec().await;
    tokio::time::timeout(Duration::from_secs(15), async {
        while f.run().state != RunState::Failed {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap();
    let failed = f.run();
    let intent: RunIntent = f
        .hub
        .read(|r| r.require("run_intent", failed.run_id))
        .unwrap();
    let frozen = intent.chat_handover_text.unwrap();
    let f = f.reopened().await;
    let count = f.codex.starts.load(Ordering::SeqCst);
    let duplicate = f.hub.submit(&Caller::local_owner(), original).await;
    assert_eq!(duplicate.status, ReceiptStatus::Duplicate);
    assert_eq!(f.codex.starts.load(Ordering::SeqCst), count);
    // A later timeline update must not rewrite the frozen prompt on retry.
    f.hub
        .write(|tx| {
            tx.emit(Event::TimelineAppended {
                task_id: f.task,
                item: TimelineItem {
                    seq: 0,
                    at: Timestamp(1),
                    run_id: Some(failed.run_id),
                    item: TimelineBody::AgentMessage {
                        message_key: None,
                        text_offset: None,
                        text: "not-in-frozen-handover".into(),
                        partial: false,
                    },
                },
            })?;
            Ok(())
        })
        .unwrap();
    f.codex.fail_spawn.store(false, Ordering::SeqCst);
    f.turn(HarnessKind::Codex, "retry-message").await;
    let spec = f.spec().await;
    assert_eq!(f.run().chat_handover, failed.chat_handover);
    assert!(spec.prompt.text.starts_with(&frozen));
    assert_eq!(spec.prompt.text.matches("first-answer").count(), 1);
    assert!(!spec.prompt.text.contains("not-in-frozen-handover"));
    f.finish("codex-session", "done").await;
    f.turn(HarnessKind::Codex, "after-delivery").await;
    assert!(!f.spec().await.prompt.text.contains("first-answer"));
    f.finish("codex-session", "done-again").await;
    f.close().await;
}

#[tokio::test]
async fn failed_codex_session_without_a_turn_retries_fresh_instead_of_resuming_it() {
    let f = Fixture::new().await;
    f.finish("claude-session", "claude-answer").await;
    f.turn(HarnessKind::Codex, "switch-message").await;
    let original = f.spec().await;
    let failed_id = f.run().run_id;
    f.hub
        .harness_event(
            failed_id,
            HarnessEvent::SessionStarted {
                native_session: "undelivered-codex-session".into(),
                actual_model: None,
                actual_effort: None,
                account_label: None,
            },
        )
        .await
        .unwrap();
    f.hub.finish_run(failed_id, RunState::Failed, None).unwrap();
    f.turn(HarnessKind::Codex, "retry-message").await;
    let retry = f.spec().await;
    assert!(retry.resume_session.is_none());
    assert!(!f.run().chat_handover.unwrap().resumed);
    assert!(retry.prompt.text.contains("claude-answer"));
    assert!(retry.prompt.text.contains("retry-message"));
    assert!(!retry.prompt.text.contains("undelivered-codex-session"));
    assert_ne!(retry.prompt.text, original.prompt.text);
    f.finish("fresh-codex-session", "done").await;
    f.close().await;
}

#[tokio::test]
async fn changed_access_is_explained_fully_to_the_returning_agent() {
    let f = Fixture::with_access(Some(Default::default())).await;
    f.finish("claude-session", "claude-answer").await;
    f.turn(HarnessKind::Codex, "codex-message").await;
    f.finish("codex-session", "codex-answer").await;
    f.hub
        .write(|tx| {
            let mut task: Task = tx.read().require("task", f.task)?;
            task.access.as_mut().unwrap().internet = false;
            tx.put("task", f.task, &task)
        })
        .unwrap();
    // The task-wide record now describes what Codex heard, not Claude.
    f.turn(HarnessKind::Codex, "use-new-access").await;
    f.finish("codex-session", "codex-with-new-access").await;
    f.turn(HarnessKind::ClaudeCode, "back-with-new-access")
        .await;
    let spec = f.spec().await;
    assert_eq!(spec.resume_session.as_deref(), Some("claude-session"));
    assert!(
        spec.prompt
            .text
            .starts_with("Ophio holds this conversation"),
        "{}",
        spec.prompt.text
    );
    assert!(spec.prompt.text.contains("The internet is off"));
    assert!(spec.prompt.text.contains("don't work around it"));
    assert!(
        !spec
            .prompt
            .text
            .contains("Ophio: the person changed this conversation's access")
    );
    f.finish("claude-session", "done").await;
    f.close().await;
}

#[tokio::test]
async fn a_switch_back_without_resume_support_starts_fresh_with_recent_chat() {
    let f = Fixture::new().await;
    f.finish("claude-session", "claude-answer").await;
    f.turn(HarnessKind::Codex, "codex-message").await;
    f.finish("codex-session", "codex-answer").await;
    f.hub
        .write(|tx| {
            let mut infos: Vec<HarnessInfo> = tx.read().require("meta", "harnesses")?;
            infos
                .iter_mut()
                .find(|h| h.kind == HarnessKind::ClaudeCode)
                .unwrap()
                .capabilities
                .resume_session = Availability::Unsupported {
                reason: "Synthetic adapter cannot resume".into(),
            };
            tx.put("meta", "harnesses", &infos)
        })
        .unwrap();
    f.turn(HarnessKind::ClaudeCode, "back").await;
    let spec = f.spec().await;
    assert!(spec.resume_session.is_none());
    assert!(spec.prompt.text.contains("claude-answer"));
    assert!(spec.prompt.text.contains("codex-answer"));
    assert!(!f.run().chat_handover.unwrap().resumed);
    f.finish("fresh-claude-session", "done").await;
    f.close().await;
}

#[tokio::test]
async fn uncertain_delivery_does_not_append_the_handover_again_to_native_history() {
    let f = Fixture::new().await;
    f.finish("claude-session", "claude-answer").await;
    f.turn(HarnessKind::Codex, "switch-message").await;
    f.spec().await;
    let run_id = f.run().run_id;
    // Codex can create a session before confirming receipt of its input.
    f.hub
        .harness_event(
            run_id,
            HarnessEvent::SessionStarted {
                native_session: "uncertain-session".into(),
                actual_model: None,
                actual_effort: None,
                account_label: None,
            },
        )
        .await
        .unwrap();
    f.hub
        .finish_run(run_id, RunState::ReconciliationRequired, None)
        .unwrap();
    let task = f.task();
    let prepared = f.hub.read(|r| prepare(r, &task)).unwrap().unwrap();
    assert!(prepared.resume.is_none());
    assert!(!prepared.boundary.resumed);
    assert!(prepared.text.contains("claude-answer"));
    assert!(!prepared.text.contains("switch-message"));
    f.close().await;
}
