// SPDX-License-Identifier: Apache-2.0
use super::*;
use async_trait::async_trait;
use ports::{HarnessError, HarnessEvent, RunControl, harness::*};
use protocol::capability::Availability;
use std::sync::{Mutex, atomic::AtomicUsize};

#[derive(Debug)]
struct Source {
    scans: AtomicUsize,
    stops: AtomicUsize,
    cwd: PathBuf,
    records: Mutex<Vec<AgentSessionRecord>>,
    state: Mutex<ChildState>,
    fail: std::sync::atomic::AtomicBool,
}
#[async_trait]
impl AgentSessionSource for Source {
    async fn scan(&self) -> Result<AgentSessionScan, HarnessError> {
        self.scans.fetch_add(1, Ordering::SeqCst);
        if self.fail.load(Ordering::SeqCst) {
            return Err(HarnessError::NotActive);
        }
        Ok(AgentSessionScan {
            processes: vec![AgentProcess {
                pid: 4242,
                ancestors: vec![],
                process_group: Some(4242),
                start_key: "process-generation-1".into(),
                harness: HarnessKind::ClaudeCode,
                executable: "fixture-claude".into(),
                cwd: Some(self.cwd.clone()),
                started_at: Some(Timestamp(1)),
            }],
            records: self.records.lock().unwrap().clone(),
            coverage: vec![],
        })
    }
    async fn stop(&self, process: &AgentProcess) -> Result<ChildStopResult, HarnessError> {
        assert_eq!(process.pid, 4242);
        assert_eq!(process.start_key, "process-generation-1");
        self.stops.fetch_add(1, Ordering::SeqCst);
        Ok(ChildStopResult {
            state: *self.state.lock().unwrap(),
            detail: "Fixture process evidence".into(),
        })
    }
}
fn source(f: &Fixture) -> Arc<Source> {
    std::fs::create_dir_all(f.project_dir()).unwrap();
    Arc::new(Source {
        scans: AtomicUsize::new(0),
        stops: AtomicUsize::new(0),
        cwd: ports::library::canonical(&f.project_dir()).unwrap(),
        records: Mutex::new(vec![]),
        state: Mutex::new(ChildState::Stopped),
        fail: std::sync::atomic::AtomicBool::new(false),
    })
}
async fn outside(f: &Fixture) -> protocol::observed::ObservedSessions {
    match f
        .hub
        .query(&Caller::local_owner(), Query::ObservedSessions)
        .await
        .unwrap()
    {
        QueryData::ObservedSessions { view } => view,
        _ => panic!("wrong result"),
    }
}

#[tokio::test]
async fn outside_refresh_read_cost_is_independent_of_finished_run_diffs() {
    let f = Fixture::new().await;
    f.project().await;
    let source = source(&f);
    f.hub.set_session_observer(source.clone());
    let _subscription = f.hub.subscribe(&Caller::local_owner(), None).await.unwrap();
    store::take_read_cost();
    let first = outside(&f).await;
    let baseline = store::take_read_cost();
    let diff = "+".repeat(100 * 1024);
    f.hub
        .write(|tx| {
            for n in 0..1000 {
                let run: Run = serde_json::from_value(serde_json::json!({
                    "run_id": RunId::new(), "task_id": TaskId::new(),
                    "harness": "claude_code", "native_session": format!("saved-{n}"),
                    "state": "completed", "admitted_at": 1, "ended_at": 2,
                    "children": [], "holds_writer": false,
                    "outcome": {"changed_files": [{"path": "src/lib.rs", "change": "modified",
                        "diff": {"text": diff, "truncated": false}}],
                        "checks": [], "still_active": [], "unknowns": []}
                }))?;
                tx.put("run", run.run_id, &run)?;
            }
            Ok(())
        })
        .unwrap();
    let mut costs = Vec::new();
    for _ in 0..2 {
        f.clock.advance(Duration::from_secs(10));
        store::take_read_cost();
        let view = outside(&f).await;
        costs.push(store::take_read_cost());
        assert_eq!(view.sessions.len(), first.sessions.len());
        assert_eq!(view.sessions[0].observed_id, first.sessions[0].observed_id);
        assert!(view.sessions[0].stop.is_available());
    }
    // Finished managed sessions stay hidden; the same id in another harness
    // and records with no managed identity remain visible and cannot be stopped.
    *source.records.lock().unwrap() = [
        (HarnessKind::ClaudeCode, "saved-0"),
        (HarnessKind::Codex, "saved-0"),
        (HarnessKind::ClaudeCode, "outside"),
    ]
    .into_iter()
    .map(|(harness, session_id)| AgentSessionRecord {
        harness,
        session_id: session_id.into(),
        cwd: source.cwd.clone(),
        started_at: Some(Timestamp(1)),
        last_activity: Some(Timestamp(2)),
    })
    .collect();
    let mut candidate_costs = Vec::new();
    for _ in 0..2 {
        f.clock.advance(Duration::from_secs(10));
        store::take_read_cost();
        let view = outside(&f).await;
        candidate_costs.push(store::take_read_cost());
        assert_eq!(view.sessions.len(), 3);
        let rows = &view.sessions[1..];
        assert_eq!(rows[0].harness, HarnessKind::Codex);
        assert_eq!(rows[0].native_session.as_deref(), Some("saved-0"));
        assert_eq!(rows[1].native_session.as_deref(), Some("outside"));
        for row in rows {
            assert_eq!(row.project_id, first.sessions[0].project_id);
            assert_eq!(row.started_at, Some(Timestamp(1)));
            assert_eq!(row.last_activity, Some(Timestamp(2)));
            assert!(!row.running);
            assert!(!row.stop.is_available());
        }
    }
    source.fail.store(true, Ordering::SeqCst);
    f.clock.advance(Duration::from_secs(10));
    store::take_read_cost();
    let failed = outside(&f).await;
    let failure_cost = store::take_read_cost();
    assert!(failed.error.is_some());
    assert_eq!(
        failed.sessions[0].observed_id,
        first.sessions[0].observed_id
    );
    assert!(!failed.sessions[0].stop.is_available());
    assert_eq!(failed.sessions.len(), 3);
    assert_eq!(source.scans.load(Ordering::SeqCst), 6);
    f.close().await;
    eprintln!(
        "baseline={baseline:?}, empty-record refreshes={costs:?}, candidate refreshes={candidate_costs:?}, failed scan={failure_cost:?}"
    );
    assert_eq!(costs, vec![baseline; 2]);
    assert_eq!(candidate_costs, vec![baseline; 2]);
    assert_eq!(failure_cost, (0, 0));
}

#[tokio::test]
async fn outside_records_are_bounded_before_reconciliation() {
    let f = Fixture::new().await;
    let source = source(&f);
    *source.records.lock().unwrap() = (0..300)
        .map(|n| AgentSessionRecord {
            harness: HarnessKind::ClaudeCode,
            session_id: format!("outside-{n}"),
            cwd: source.cwd.clone(),
            started_at: None,
            last_activity: None,
        })
        .collect();
    f.hub.set_session_observer(source);
    let _subscription = f.hub.subscribe(&Caller::local_owner(), None).await.unwrap();
    let view = outside(&f).await;
    assert_eq!(view.sessions.len(), 257); // one process and 256 records
    assert_eq!(
        view.sessions[256].native_session.as_deref(),
        Some("outside-255")
    );
    f.close().await;
}

#[tokio::test]
async fn outside_scans_require_watchers_and_are_throttled_across_queries() {
    let f = Fixture::new().await;
    let source = source(&f);
    f.hub.set_session_observer(source.clone());
    assert!(outside(&f).await.sessions.is_empty());
    assert_eq!(source.scans.load(Ordering::SeqCst), 0);
    let project = f.project().await;
    let subscription = f.hub.subscribe(&Caller::local_owner(), None).await.unwrap();
    let row = outside(&f).await.sessions.remove(0);
    assert_eq!(row.project_id, Some(project));
    assert!(row.running);
    assert_eq!(source.scans.load(Ordering::SeqCst), 1);
    for _ in 0..3 {
        outside(&f).await;
    }
    f.clock.advance(Duration::from_secs(9));
    outside(&f).await;
    assert_eq!(source.scans.load(Ordering::SeqCst), 1);
    f.clock.advance(Duration::from_secs(1));
    assert_eq!(outside(&f).await.sessions[0].observed_id, row.observed_id);
    assert_eq!(source.scans.load(Ordering::SeqCst), 2);
    drop(subscription);
    f.clock.advance(Duration::from_secs(20));
    outside(&f).await;
    assert_eq!(source.scans.load(Ordering::SeqCst), 2);
    f.close().await;
}
#[tokio::test]
async fn outside_stop_requires_confirmation_revision_and_broader_grant() {
    let f = Fixture::new().await;
    let source = source(&f);
    f.hub.set_session_observer(source.clone());
    let _subscription = f.hub.subscribe(&Caller::local_owner(), None).await.unwrap();
    let row = outside(&f).await.sessions.remove(0);
    let refused = f
        .hub
        .submit(
            &Caller::local_owner(),
            edit(
                Command::StopObservedSession {
                    observed_id: row.observed_id.clone(),
                    confirmed: true,
                    force: true,
                },
                row.revision,
            ),
        )
        .await;
    assert_eq!(refused.status, ReceiptStatus::Rejected);
    assert!(refused.error.unwrap().message.contains("terminal"));
    assert_eq!(source.stops.load(Ordering::SeqCst), 0);
    for (confirmed, revision) in [(false, row.revision), (true, row.revision + 1)] {
        let receipt = f
            .hub
            .submit(
                &Caller::local_owner(),
                edit(
                    Command::StopObservedSession {
                        force: false,
                        observed_id: row.observed_id.clone(),
                        confirmed,
                    },
                    revision,
                ),
            )
            .await;
        assert_eq!(receipt.status, ReceiptStatus::Rejected);
    }
    assert_eq!(source.stops.load(Ordering::SeqCst), 0);
    let command = edit(
        Command::StopObservedSession {
            force: false,
            observed_id: row.observed_id,
            confirmed: true,
        },
        row.revision,
    );
    let id = command.command_id;
    assert_eq!(
        f.hub
            .submit(&Caller::local_owner(), command.clone())
            .await
            .status,
        ReceiptStatus::Accepted
    );
    settle(&f.hub, id).await;
    assert_eq!(source.stops.load(Ordering::SeqCst), 1);
    assert_eq!(
        f.hub.submit(&Caller::local_owner(), command).await.status,
        ReceiptStatus::Duplicate
    );
    assert!(!outside(&f).await.sessions[0].running);
    let result = f.hub.read(|r| r.command(id)).unwrap().unwrap();
    assert!(
        matches!(result.result, Some(CommandResult::Intervention { receipt }) if receipt.facts[0].status == FactStatus::Done)
    );
    let denied = protocol::auth::Grants {
        tasks: true,
        ..Default::default()
    };
    assert!(!domain::grants::command(
        &denied,
        &Command::StopObservedSession {
            force: false,
            observed_id: "any".into(),
            confirmed: true
        }
    ));
    f.close().await;
}
#[tokio::test]
async fn failed_scans_keep_last_observation_and_disable_stop() {
    let f = Fixture::new().await;
    let source = source(&f);
    f.hub.set_session_observer(source.clone());
    let _subscription = f.hub.subscribe(&Caller::local_owner(), None).await.unwrap();
    let first = outside(&f).await;
    source.fail.store(true, Ordering::SeqCst);
    f.clock.advance(Duration::from_secs(10));
    let view = outside(&f).await;
    assert_eq!(view.checked_at, first.checked_at);
    assert!(view.error.is_some());
    assert!(!view.sessions[0].stop.is_available());
    f.close().await;
}
#[derive(Debug)]
struct ChildControl(ChildState);
#[async_trait]
impl RunControl for ChildControl {
    async fn send_message(
        &self,
        _: MessageRef,
        _: PromptInput,
        _: MessageDelivery,
    ) -> Result<DeliveryState, HarnessError> {
        Err(HarnessError::NotActive)
    }
    async fn interrupt(&self) -> Result<(), HarnessError> {
        Ok(())
    }
    fn close_admission(&self) {}
    async fn resolve_approval(&self, _: &str, _: &ApprovalDecision) -> Result<(), HarnessError> {
        Err(HarnessError::NotActive)
    }
    async fn shutdown(&self) -> Result<(), HarnessError> {
        Ok(())
    }
    fn process_id(&self) -> Option<u32> {
        Some(4242)
    }
    fn child_stop_availability(&self, _: &str) -> Availability {
        Availability::Available
    }
    async fn stop_child(&self, _: &str) -> Result<ChildStopResult, HarnessError> {
        Ok(ChildStopResult {
            state: self.0,
            detail: "Fixture native result".into(),
        })
    }
}
#[tokio::test]
async fn managed_processes_are_excluded_and_child_stop_survives_parent_completion() {
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
    let run_id = run_started(&f, task).await;
    f.hub
        .inner
        .runs
        .lock()
        .await
        .insert(run_id, Arc::new(ChildControl(ChildState::Stopped)));
    f.harness
        .emit(
            run_id,
            HarnessEvent::Child {
                lifetime: None,
                native_ref: "job".into(),
                label: "Build assets".into(),
                state: ChildState::Active,
            },
        )
        .await;
    f.harness
        .emit(
            run_id,
            HarnessEvent::ParentEnded {
                exit: RunExit::Completed,
            },
        )
        .await;
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let run: Run = f.hub.read(|r| r.require("run", run_id)).unwrap();
            if run.ended_at.is_some() && !run.children.is_empty() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    f.hub.set_session_observer(source(&f));
    let _subscription = f.hub.subscribe(&Caller::local_owner(), None).await.unwrap();
    assert!(outside(&f).await.sessions.is_empty());
    let run: Run = f.hub.read(|r| r.require("run", run_id)).unwrap();
    let task: Task = f.hub.read(|r| r.require("task", task)).unwrap();
    let child_id = run.children[0].child_id;
    assert_eq!(
        f.hub
            .submit(
                &Caller::local_owner(),
                edit(Command::StopChild { run_id, child_id }, task.revision + 1)
            )
            .await
            .status,
        ReceiptStatus::Rejected
    );
    let command = edit(Command::StopChild { run_id, child_id }, task.revision);
    assert_eq!(
        f.hub
            .submit(&Caller::local_owner(), command.clone())
            .await
            .status,
        ReceiptStatus::Accepted
    );
    settle(&f.hub, command.command_id).await;
    let row = f
        .hub
        .read(|r| r.command(command.command_id))
        .unwrap()
        .unwrap();
    assert_eq!(row.stage, CommandStage::Observed);
    f.close().await;
}
#[test]
fn child_receipt_never_confuses_ignored_or_unknown_stop_with_done() {
    let child = ChildAgent {
        lifetime: None,
        child_id: ChildId::new(),
        native_ref: Some("job".into()),
        label: "Fixture".into(),
        state: ChildState::Active,
        started_at: Timestamp(1),
        ended_at: None,
        provenance: Provenance::Native,
        stop: Some(Availability::Available),
    };
    for (state, status) in [
        (ChildState::Stopped, FactStatus::Done),
        (ChildState::Completed, FactStatus::Done),
        (ChildState::Active, FactStatus::StillActive),
        (ChildState::Unknown, FactStatus::Unknown),
    ] {
        assert_eq!(
            crate::children::child_fact(
                &child,
                Ok(ChildStopResult {
                    state,
                    detail: "Native evidence".into()
                })
            )
            .status,
            status
        );
    }
    assert_eq!(
        crate::children::child_fact(&child, Err("connection ended".into())).status,
        FactStatus::Unknown
    );
}
#[tokio::test]
async fn native_sandbox_warning_reaches_readiness() {
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
    let run = run_started(&f, task).await;
    f.hub
        .native_readiness_warning(run, "Sandbox cannot start: fixture namespaces disabled")
        .unwrap();
    let QueryData::Readiness { report } = f
        .hub
        .query(&Caller::local_owner(), Query::Readiness)
        .await
        .unwrap()
    else {
        panic!("missing readiness")
    };
    assert!(report.checks.iter().any(|c| {
        c.status == CheckStatus::Failed
            && c.detail
                .as_deref()
                .is_some_and(|d| d.contains("fixture namespaces disabled"))
    }));
    f.close().await;
}

#[tokio::test]
async fn children_that_finish_during_interrupt_still_have_individual_receipt_facts() {
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
    let id = run_started(&f, task).await;
    let child = ChildAgent {
        lifetime: None,
        child_id: ChildId::new(),
        native_ref: Some("job".into()),
        label: "Build preview".into(),
        state: ChildState::Active,
        started_at: Timestamp(1),
        ended_at: None,
        provenance: Provenance::Native,
        stop: Some(Availability::Available),
    };
    let before = vec![child.clone()];
    f.hub
        .write(|tx| {
            let mut run: Run = tx.read().require("run", id)?;
            run.children = vec![ChildAgent {
                state: ChildState::Stopped,
                ended_at: Some(Timestamp(2)),
                ..child.clone()
            }];
            tx.put("run", id, &run)?;
            Ok(())
        })
        .unwrap();
    let facts = f.hub.stop_run_children(id, before, true).await.unwrap();
    assert_eq!(facts.len(), 1);
    assert_eq!(facts[0].label, "Child: Build preview");
    assert_eq!(facts[0].status, FactStatus::Done);
    f.close().await;
}

#[tokio::test]
async fn a_helper_only_its_turn_can_stop_is_reported_by_that_turn() {
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
    let id = run_started(&f, task).await;
    let child = ChildAgent {
        lifetime: None,
        child_id: ChildId::new(),
        native_ref: Some("c1".into()),
        label: "Fixture child".into(),
        state: ChildState::Active,
        started_at: Timestamp(1),
        ended_at: None,
        provenance: Provenance::Native,
        stop: Some(Availability::Unsupported {
            reason: "Stop it in the agent on the computer.".into(),
        }),
    };
    let set = |state| {
        f.hub
            .write(|tx| {
                let mut run: Run = tx.read().require("run", id)?;
                run.children = vec![ChildAgent {
                    state,
                    ..child.clone()
                }];
                tx.put("run", id, &run)?;
                Ok(())
            })
            .unwrap()
    };
    set(ChildState::Active);
    // Without an acknowledged interrupt the helper keeps running, so the
    // agent's own advice stands.
    let facts = f
        .hub
        .stop_run_children(id, vec![child.clone()], false)
        .await
        .unwrap();
    assert_eq!(facts[0].status, FactStatus::Unknown);
    assert_eq!(
        facts[0].detail.as_deref(),
        Some("Stop it in the agent on the computer.")
    );
    // The turn's end arrives after Stop was acknowledged.
    let (facts, ()) = tokio::join!(
        f.hub.stop_run_children(id, vec![child.clone()], true),
        async {
            tokio::time::sleep(Duration::from_millis(200)).await;
            set(ChildState::Stopped);
        }
    );
    let facts = facts.unwrap();
    assert_eq!(facts[0].label, "Child: Fixture child");
    assert_eq!(facts[0].status, FactStatus::Done);
    // A turn that never reports it leaves the helper unknown, without
    // pointing at a stop control that doesn't exist.
    set(ChildState::Active);
    let facts = f
        .hub
        .stop_run_children(id, vec![child.clone()], true)
        .await
        .unwrap();
    assert_eq!(facts[0].status, FactStatus::Unknown);
    assert!(
        facts[0]
            .detail
            .as_deref()
            .unwrap()
            .contains("didn’t report this helper ending")
    );
    f.close().await;
}

#[tokio::test]
async fn outside_stop_reports_ignored_and_unknown_results_without_success() {
    for state in [ChildState::Active, ChildState::Unknown] {
        let f = Fixture::new().await;
        let source = source(&f);
        *source.state.lock().unwrap() = state;
        f.hub.set_session_observer(source.clone());
        let _subscription = f.hub.subscribe(&Caller::local_owner(), None).await.unwrap();
        let row = outside(&f).await.sessions.remove(0);
        let command = edit(
            Command::StopObservedSession {
                force: false,
                observed_id: row.observed_id,
                confirmed: true,
            },
            row.revision,
        );
        assert_eq!(
            f.hub
                .submit(&Caller::local_owner(), command.clone())
                .await
                .status,
            ReceiptStatus::Accepted
        );
        settle(&f.hub, command.command_id).await;
        let status = f
            .hub
            .read(|r| r.command(command.command_id))
            .unwrap()
            .unwrap();
        assert_eq!(status.stage, CommandStage::ReconciliationRequired);
        assert!(
            matches!(status.result, Some(CommandResult::Intervention { receipt }) if receipt.facts[0].status == if state == ChildState::Active { FactStatus::StillActive } else { FactStatus::Unknown })
        );
        assert!(outside(&f).await.sessions[0].running);
        assert!(!outside(&f).await.sessions[0].stop.is_available());
        f.close().await;
    }
}
