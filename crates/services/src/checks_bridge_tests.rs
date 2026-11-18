// SPDX-License-Identifier: Apache-2.0
//! The real bridge handler and store, without an agent process or check runner.
use crate::{CompanionHub, bridge::BridgeConfig, testing::FakeClock};
use ports::ToolBridge;
use ports::bridge::{BridgeBinding, BridgeHandler, BridgeRequest, BridgeResponse, ChecksCall};
use protocol::{auth::Principal, capability::HarnessKind, checks::*, model::*, time::Timestamp, *};
use std::sync::Arc;

struct Fixture {
    hub: CompanionHub,
    dir: tempfile::TempDir,
}

impl Fixture {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let hub = CompanionHub::builder()
            .data_dir(dir.path().join("data"))
            .clock(Arc::new(FakeClock::default()))
            .bridge(BridgeConfig::new("synthetic-bridge", "synthetic-socket"))
            .build()
            .await
            .unwrap();
        Self { hub, dir }
    }

    async fn run(
        &self,
        mode: TaskMode,
        kind: TaskKind,
        answering: bool,
        approved: Vec<Check>,
    ) -> (Task, Run, ToolBridge, BridgeBinding) {
        let now = Timestamp(1_000);
        let project_id = ProjectId::new();
        let task_id = TaskId::new();
        let run_id = RunId::new();
        let root = self.dir.path().join(project_id.to_string());
        std::fs::create_dir_all(&root).unwrap();
        let project = Project {
            project_id,
            name: "Sample app".into(),
            host_path: root.to_string_lossy().into_owned(),
            default_harness: None,
            default_model: None,
            default_effort: None,
            chat_access: None,
            auto_start: None,
            active_writer: Some(run_id),
            vcs: None,
            created_at: now,
            stage: None,
            one_off: false,
            home: false,
        };
        let task = Task {
            checking: None,
            task_id,
            project_id,
            kind,
            origin: None,
            from_chat: None,
            mode,
            title: "Improve the app".into(),
            state: TaskState::Dispatching,
            revision: 1,
            harness: HarnessKind::ClaudeCode,
            model: None,
            effort: None,
            launch_policy: None,
            branch: None,
            start: None,
            browser: false,
            conversation: false,
            access: None,
            access_blocked: None,
            github: None,
            review: None,
            wake: None,
            asked_by: None,
            report: None,
            created_by: Principal::LocalOwner,
            created_at: now,
            updated_at: now,
            current_run: Some(run_id),
            target_id: None,
            mission: None,
            attention_count: 0,
            status_line: None,
            notify_done: false,
        };
        let run = Run {
            chat_handover: None,
            helpers_checked: None,
            human_resolution: None,
            carried_on: None,
            force_stoppable: false,
            cleanup_pending: false,
            nothing_running: false,
            run_id,
            task_id,
            harness: HarnessKind::ClaudeCode,
            requested_model: None,
            actual_model: None,
            requested_effort: None,
            launch_policy: None,
            actual_effort: None,
            account_label: None,
            native_session: None,
            state: RunState::Dispatching,
            admitted_at: now,
            dispatched_at: None,
            acknowledged_at: None,
            ended_at: None,
            children: vec![],
            earlier_children: None,
            outcome: None,
            holds_writer: mode == TaskMode::Work,
            mode: Some(mode),
            revisions: None,
            follow_up: None,
        };
        self.hub
            .write(|tx| {
                tx.put("project", project_id, &project)?;
                tx.put("task", task_id, &task)?;
                tx.put("run", run_id, &run)?;
                if answering {
                    tx.put("checklist_answering", task_id, &true)?;
                }
                if !approved.is_empty() {
                    tx.put(
                        store::checks::APPROVED,
                        project_id,
                        &ProjectChecks {
                            project_id,
                            version: 3,
                            checks: approved,
                            input_fingerprints: vec![],
                            changed_at: now,
                            changed_by: ChecksChangedBy::Person { device_id: None },
                        },
                    )?;
                }
                Ok(())
            })
            .unwrap();
        let bridge = self.hub.spawn_config(run_id).await.unwrap();
        let token = bridge
            .env
            .iter()
            .find(|(name, _)| name == ports::bridge::RUN_TOKEN_ENV)
            .unwrap()
            .1
            .clone();
        let binding = self.hub.bind(&token).await.unwrap();
        (task, run, bridge, binding)
    }

    async fn call(&self, binding: &BridgeBinding, action: ChecksCall) -> BridgeResponse {
        self.hub
            .handle(binding, BridgeRequest::Checks { action })
            .await
    }

    fn draft(&self, task_id: TaskId) -> Option<ChecksDraft> {
        self.hub
            .read(|read| read.task_checks_draft(task_id))
            .unwrap()
    }

    async fn close(self) {
        self.hub.shutdown().await.unwrap();
    }
}

fn check() -> Check {
    Check {
        check_id: CheckId::new(),
        name: "App builds correctly".into(),
        about: "The app can be built. A failure means it cannot be installed.".into(),
        run: "npm run build".into(),
        setup: None,
        paths: vec![],
        when: CheckWhen::EveryTask,
        timeout_secs: 600,
        network: false,
        inputs: vec![],
        enabled: true,
        typical_secs: None,
    }
}
fn add(check: Check) -> ChecksCall {
    ChecksCall::Propose {
        base_version: 0,
        items: vec![CheckDraftItem::Add {
            check,
            note: "Check that the app builds.".into(),
        }],
    }
}
fn text(response: BridgeResponse) -> String {
    match response {
        BridgeResponse::ChecksResult { text } => text,
        other => panic!("expected checks result, got {other:?}"),
    }
}
fn refused(response: BridgeResponse) -> String {
    match response {
        BridgeResponse::Refused { reason } => reason,
        other => panic!("expected refusal, got {other:?}"),
    }
}

#[tokio::test]
async fn legacy_blank_setup_requests_propose_without_approving_checks() {
    let f = Fixture::new().await;
    let (task, _, _, binding) = f.run(TaskMode::Work, TaskKind::Coding, false, vec![]).await;
    let before = text(f.call(&binding, ChecksCall::Read).await);
    for request in ["", " \r\n\t"] {
        f.hub
            .write(|tx| tx.put("checks_request", task.task_id, &request))
            .unwrap();
        let draft: ChecksDraft =
            serde_json::from_str(&text(f.call(&binding, add(check())).await)).unwrap();
        assert_eq!(draft.request, None);
        assert_eq!(draft.state, ChecksDraftState::Proving);
        assert_eq!(text(f.call(&binding, ChecksCall::Read).await), before);
        let mut invalid = check();
        invalid.run = "".into();
        assert!(matches!(
            f.call(&binding, add(invalid)).await,
            BridgeResponse::Refused { .. }
        ));
        assert_eq!(f.draft(task.task_id), Some(draft));
    }
    f.close().await;
}

#[tokio::test]
async fn checks_read_is_project_bound_and_proposals_only_replace_the_tasks_draft() {
    let f = Fixture::new().await;
    let (task, _, bridge, binding) = f.run(TaskMode::Work, TaskKind::Coding, false, vec![]).await;
    assert!(bridge.checks && binding.checks);
    assert!(bridge.check_names.is_empty());
    let approved: ProjectChecks =
        serde_json::from_str(&text(f.call(&binding, ChecksCall::Read).await)).unwrap();
    assert_eq!(approved.project_id, task.project_id);
    assert_eq!(approved.version, 0);
    assert!(approved.checks.is_empty());
    let request = "Add a check that catches app build failures.";
    f.hub
        .write(|tx| tx.put("checks_request", task.task_id, &request))
        .unwrap();
    let first: ChecksDraft =
        serde_json::from_str(&text(f.call(&binding, add(check())).await)).unwrap();
    assert_eq!(first.request.as_deref(), Some(request));
    assert_eq!(first.project_id, task.project_id);
    assert_eq!(first.task_id, Some(task.task_id));
    assert_eq!(first.state, ChecksDraftState::Proving);
    assert!(first.proofs.is_empty());
    assert_eq!(f.draft(task.task_id), Some(first.clone()));
    let second: ChecksDraft =
        serde_json::from_str(&text(f.call(&binding, add(check())).await)).unwrap();
    assert_ne!(first.id, second.id);
    assert_eq!(f.draft(task.task_id), Some(second));
    let discarded = f
        .hub
        .read(|read| read.checks_draft(first.id))
        .unwrap()
        .unwrap();
    assert_eq!(discarded.state, ChecksDraftState::Discarded);
    let after: ProjectChecks =
        serde_json::from_str(&text(f.call(&binding, ChecksCall::Read).await)).unwrap();
    assert_eq!(after, approved, "agent proposals must not approve checks");
    let (other, _, _, other_binding) = f.run(TaskMode::Work, TaskKind::Coding, false, vec![]).await;
    let other_checks: ProjectChecks =
        serde_json::from_str(&text(f.call(&other_binding, ChecksCall::Read).await)).unwrap();
    assert_eq!(other_checks.project_id, other.project_id);
    assert!(other_checks.checks.is_empty());
    assert!(f.draft(other.task_id).is_none());
    f.close().await;
}

#[tokio::test]
async fn validation_errors_reach_the_bridge_without_discarding_the_previous_draft() {
    let f = Fixture::new().await;
    let (task, _, _, binding) = f.run(TaskMode::Work, TaskKind::Coding, false, vec![]).await;
    let good = check();
    let first: ChecksDraft =
        serde_json::from_str(&text(f.call(&binding, add(good.clone())).await)).unwrap();
    for problem in [
        "name",
        "run",
        "timeout_short",
        "timeout_long",
        "paths",
        "inputs",
    ] {
        let mut invalid = good.clone();
        let expected = match problem {
            "name" => {
                invalid.name = " ".into();
                "name"
            }
            "run" => {
                invalid.run.clear();
                "command"
            }
            "timeout_short" => {
                invalid.timeout_secs = 9;
                "time limit"
            }
            "timeout_long" => {
                invalid.timeout_secs = 3_601;
                "time limit"
            }
            "paths" => {
                invalid.paths = vec!["[".into()];
                "pattern"
            }
            "inputs" => {
                invalid.inputs = vec!["../outside".into()];
                "inside the project"
            }
            _ => unreachable!(),
        };
        let reason = refused(f.call(&binding, add(invalid)).await);
        assert!(reason.contains(expected), "{problem}: {reason}");
        assert_eq!(f.draft(task.task_id), Some(first.clone()));
    }
    let stale = ChecksCall::Propose {
        base_version: 1,
        items: vec![CheckDraftItem::Add {
            check: good.clone(),
            note: "Check the app build.".into(),
        }],
    };
    assert!(!refused(f.call(&binding, stale).await).is_empty());
    assert_eq!(f.draft(task.task_id), Some(first.clone()));
    let corrected: ChecksDraft =
        serde_json::from_str(&text(f.call(&binding, add(good)).await)).unwrap();
    assert_ne!(corrected.id, first.id);
    assert_eq!(f.draft(task.task_id), Some(corrected));
    f.close().await;
}

#[tokio::test]
async fn read_only_runs_read_approved_checks_but_cannot_propose_even_with_forged_bindings() {
    let f = Fixture::new().await;
    for mode in [TaskMode::Ask, TaskMode::Plan] {
        let approved = check();
        let (task, _, bridge, binding) = f
            .run(mode, TaskKind::Coding, false, vec![approved.clone()])
            .await;
        assert!(bridge.checks && binding.checks && binding.read_only);
        let read: ProjectChecks =
            serde_json::from_str(&text(f.call(&binding, ChecksCall::Read).await)).unwrap();
        assert_eq!(read.checks, vec![approved]);
        assert!(refused(f.call(&binding, add(check())).await).contains("only reads"));
        let mut forged = binding.clone();
        forged.read_only = false;
        assert!(matches!(
            f.call(&forged, add(check())).await,
            BridgeResponse::Refused { .. }
        ));
        let direct = f.hub.checks_request(&forged, add(check())).unwrap_err();
        assert_eq!(direct.code, ErrorCode::PermissionDenied);
        assert!(direct.message.contains("only reads"));
        assert!(f.draft(task.task_id).is_none());
    }
    f.close().await;
}

#[tokio::test]
async fn computer_and_checklist_turns_get_no_checks_even_with_forged_flags() {
    let f = Fixture::new().await;
    for (kind, answering) in [(TaskKind::ComputerUse, false), (TaskKind::Coding, true)] {
        let (task, _, bridge, binding) =
            f.run(TaskMode::Work, kind, answering, vec![check()]).await;
        assert!(!bridge.checks && !binding.checks);
        assert!(bridge.check_names.is_empty());
        assert!(matches!(
            f.call(&binding, ChecksCall::Read).await,
            BridgeResponse::Refused { .. }
        ));
        let mut forged = binding;
        forged.checks = true;
        forged.computer_use = false;
        forged.checklist_answers = false;
        assert!(matches!(
            f.call(&forged, add(check())).await,
            BridgeResponse::Refused { .. }
        ));
        assert!(f.hub.checks_request(&forged, add(check())).is_err());
        assert!(f.draft(task.task_id).is_none());
    }
    f.close().await;
}

#[tokio::test]
async fn stopped_or_revoked_runs_cannot_read_or_draft_checks() {
    let f = Fixture::new().await;
    let (task, run, _, binding) = f.run(TaskMode::Work, TaskKind::Coding, false, vec![]).await;
    f.hub
        .write(|tx| tx.put("stopped", run.run_id, &true))
        .unwrap();
    for action in [ChecksCall::Read, add(check())] {
        assert!(refused(f.call(&binding, action.clone()).await).contains("no longer active"));
        assert!(f.hub.checks_request(&binding, action).is_err());
    }
    f.hub
        .write(|tx| {
            tx.delete("stopped", run.run_id)?;
            tx.delete("run_token", run.run_id)
        })
        .unwrap();
    assert!(matches!(
        f.call(&binding, add(check())).await,
        BridgeResponse::Refused { .. }
    ));
    assert!(f.hub.checks_request(&binding, add(check())).is_err());
    assert!(f.draft(task.task_id).is_none());
    f.close().await;
}

#[tokio::test]
async fn run_prompt_names_include_only_enabled_checks_that_run_after_work() {
    let f = Fixture::new().await;
    let enabled = check();
    let mut disabled = check();
    disabled.enabled = false;
    let mut before_merge = check();
    before_merge.when = CheckWhen::BeforeMerge;
    let approved = vec![enabled.clone(), disabled, before_merge];
    let (_, _, bridge, binding) = f
        .run(TaskMode::Work, TaskKind::Coding, false, approved.clone())
        .await;
    assert_eq!(bridge.check_names, vec![enabled.name]);
    let read: ProjectChecks =
        serde_json::from_str(&text(f.call(&binding, ChecksCall::Read).await)).unwrap();
    assert_eq!(read.version, 3);
    assert_eq!(
        read.checks, approved,
        "reading includes disabled and future-phase checks in full"
    );
    f.close().await;
}

#[tokio::test]
async fn a_run_launched_read_only_cannot_draft_after_a_mode_record_changes() {
    let f = Fixture::new().await;
    let (task, run, _, binding) = f.run(TaskMode::Work, TaskKind::Coding, false, vec![]).await;
    f.hub
        .write(|tx| tx.put(crate::runs::LAUNCHED, run.run_id, &TaskMode::Ask))
        .unwrap();
    assert!(refused(f.call(&binding, add(check())).await).contains("only reads"));
    assert!(f.draft(task.task_id).is_none());
    f.close().await;
}
