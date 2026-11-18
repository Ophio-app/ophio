// SPDX-License-Identifier: Apache-2.0
//! Admission tests use real private snapshots and real store transactions, but
//! do not wake execution: the writer handover is inspected before any process.
use super::*;
use crate::check_lifecycle::{DEFERRED_END, DeferredEnd, FINALIZING, JOB_INPUTS};
use ports::Clock;
use protocol::checks::*;
use std::collections::BTreeMap;

#[tokio::test]
#[ignore = "Explicit 50,000-file transaction benchmark"]
async fn measure_end_snapshot_transaction_50k() {
    for phase in [
        "cold-no-baseline",
        "warm-with-baseline",
        "warm-with-baseline-repeat",
    ] {
        let (f, project_id, _, run_id) = running().await;
        approve(&f, project_id, 0, definition("printf approved"));
        for directory in 0..100 {
            let dir = f.project_dir().join(format!("source-{directory:03}"));
            std::fs::create_dir(&dir).unwrap();
            for file in 0..500 {
                std::fs::write(
                    dir.join(format!("file-{file:03}.txt")),
                    format!("source {directory} {file}\n"),
                )
                .unwrap();
            }
        }
        // Cold means no reusable hashes or saved blobs; warm reuses the start
        // manifest. This does not flush the operating system's page cache.
        if phase != "cold-no-baseline" {
            f.hub.capture_checks_start(run_id).await.unwrap();
            std::fs::write(f.project_dir().join("app.txt"), "after").unwrap();
        }
        let start = std::time::Instant::now();
        let hub = f.hub.clone();
        let preparing = tokio::spawn(async move { hub.prepare_checks_end(&end(run_id)).await });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            !preparing.is_finished(),
            "The benchmark must overlap preparation"
        );
        let concurrent = std::time::Instant::now();
        transaction(&f, |tx| tx.put("snapshot_benchmark", run_id, &true));
        let concurrent_write = concurrent.elapsed();
        assert!(
            !preparing.is_finished(),
            "The small write must finish before preparation does"
        );
        preparing.await.unwrap().unwrap();
        let preparation = start.elapsed();
        let mut held = std::time::Duration::ZERO;
        let total = std::time::Instant::now();
        let rolled_back = f.hub.inner.db.lock().unwrap().write(f.clock.now(), |tx| {
            let start = std::time::Instant::now();
            let mut run: Run = tx.read().require("run", run_id)?;
            let mut task: Task = tx.read().require("task", run.task_id)?;
            assert!(
                f.hub
                    .defer_run_checks(tx, &mut run, &mut task, end(run_id))?
            );
            held = start.elapsed();
            Err::<(), _>(store::Error::IntegerExhausted)
        });
        assert!(rolled_back.is_err());
        assert!(
            held < Duration::from_millis(200),
            "Prepared handover must not walk the project"
        );
        eprintln!(
            "END_SNAPSHOT_AFTER phase={phase} files=50001 preparation_ms={:.3} transaction_body_ms={:.3} transaction_total_ms={:.3} concurrent_write_ms={:.3}",
            preparation.as_secs_f64() * 1000.0,
            held.as_secs_f64() * 1000.0,
            total.elapsed().as_secs_f64() * 1000.0,
            concurrent_write.as_secs_f64() * 1000.0
        );
        f.close().await;
    }
}

fn definition(command: &str) -> Check {
    Check {
        check_id: CheckId::new(),
        name: "The app still works".into(),
        about: "A failure means the change needs a look.".into(),
        run: command.into(),
        setup: None,
        paths: vec![],
        when: CheckWhen::EveryTask,
        timeout_secs: 10,
        network: false,
        inputs: vec![],
        enabled: true,
        typical_secs: None,
    }
}

fn transaction<T>(f: &Fixture, op: impl FnOnce(&mut store::Write<'_>) -> store::Result<T>) -> T {
    f.hub
        .inner
        .db
        .lock()
        .unwrap()
        .write(f.clock.now(), op)
        .unwrap()
        .value
}

async fn running() -> (Fixture, ProjectId, TaskId, RunId) {
    let f = Fixture::new().await;
    let project_id = f.project().await;
    std::fs::write(f.project_dir().join("app.txt"), "before").unwrap();
    let (task_id, _) = f
        .task(
            project_id,
            TaskPlacement::StartNow,
            &Caller::local_owner(),
            false,
        )
        .await;
    let run_id = run_started(&f, task_id).await;
    (f, project_id, task_id, run_id)
}

fn approve(f: &Fixture, project_id: ProjectId, base: u64, check: Check) {
    transaction(f, |tx| {
        tx.change_checks(
            project_id,
            base,
            CheckEdit::Save { check },
            None,
            &f.project_dir(),
        )?;
        Ok(())
    });
}

fn end(run_id: RunId) -> DeferredEnd {
    DeferredEnd {
        run_id,
        state: RunState::Completed,
        outcome: None,
        helpers_running: false,
        why: None,
    }
}

async fn admit(f: &Fixture, run_id: RunId, ending: DeferredEnd) -> bool {
    f.hub.prepare_checks_end(&ending).await.unwrap();
    transaction(f, |tx| {
        let mut run: Run = tx.read().require("run", run_id)?;
        let mut task: Task = tx.read().require("task", run.task_id)?;
        f.hub.defer_run_checks(tx, &mut run, &mut task, ending)
    })
}

fn assert_queued_task_cannot_start(f: &Fixture, task_id: TaskId) {
    transaction(f, |tx| {
        let mut task: Task = tx.read().require("task", task_id)?;
        assert!(task.current_run.is_none());
        let attempted = tx.attempt(|tx| {
            f.hub
                .new_run(tx, &mut task, CommandId::new(), None, None, false)
        });
        assert!(
            matches!(attempted, Err(store::Error::Rule(ref cause)) if cause.code == ErrorCode::Conflict),
            "The next task must not be admitted while checks, proof, or original finalization hold the writer: {attempted:?}"
        );
        Ok(())
    });
}

fn jobs(f: &Fixture, task: TaskId) -> Vec<CheckJob> {
    f.hub.read(|r| r.task_check_jobs(task)).unwrap()
}

fn writer(f: &Fixture, project_id: ProjectId) -> RunId {
    f.hub
        .read(|r| {
            let project: Project = r.require("project", project_id)?;
            let queue: QueueView = r.require("queue", project_id)?;
            assert_eq!(project.active_writer, queue.writer);
            Ok(project
                .active_writer
                .expect("The writer must stay reserved"))
        })
        .unwrap()
}

fn finish(f: &Fixture, id: CheckJobId, stopped: bool) -> Option<DeferredEnd> {
    transaction(f, |tx| {
        let mut job: CheckJob = tx.read().require(store::checks::JOBS, id)?;
        job.state = CheckJobState::Finished;
        job.finished_at = Some(f.clock.now());
        if stopped {
            job.message = Some("Not checked: stopped".into());
        }
        for check in &job.checks {
            if !job
                .results
                .iter()
                .any(|result| result.check_id == check.check_id)
            {
                job.results.push(protocol::checks::CheckResult {
                    check_id: check.check_id,
                    name: check.name.clone(),
                    outcome: if stopped {
                        CheckOutcome::Stopped
                    } else {
                        CheckOutcome::Passed
                    },
                    duration_secs: 1.0,
                    exit_code: (!stopped).then_some(0),
                    output: String::new(),
                    inputs_changed: vec![],
                });
            }
        }
        tx.put_check_job(&job)?;
        f.hub.advance_task_checks(tx, id)
    })
}

#[tokio::test]
async fn frozen_approved_commands_never_take_the_agents_candidate_or_later_approval() {
    let (f, project_id, task_id, run_id) = running().await;
    let approved = definition("printf approved");
    approve(&f, project_id, 0, approved.clone());
    f.hub.capture_checks_start(run_id).await.unwrap();
    let mut candidate = approved.clone();
    candidate.run = "printf candidate".into();
    let draft = transaction(&f, |tx| {
        tx.propose_checks(
            project_id,
            1,
            Some(task_id),
            None,
            vec![CheckDraftItem::Change {
                check: candidate.clone(),
                note: "Try a different command.".into(),
            }],
            &f.project_dir(),
        )
    });
    std::fs::write(f.project_dir().join("app.txt"), "after").unwrap();
    assert!(admit(&f, run_id, end(run_id)).await);
    let admitted = jobs(&f, task_id);
    let checked = admitted
        .iter()
        .find(|job| job.purpose == CheckJobPurpose::AfterRun)
        .unwrap();
    let proof = admitted.iter().find(|job| matches!(job.purpose, CheckJobPurpose::DraftProof { draft_id } if draft_id == draft.id)).unwrap();
    assert_eq!(checked.checks, vec![approved.clone()]);
    assert_eq!(proof.checks, vec![candidate]);
    assert_eq!(checked.checks_version, 1);
    assert_eq!(writer(&f, project_id), RunId(checked.id.0));
    let mut later = approved;
    later.run = "printf later-approval".into();
    approve(&f, project_id, 1, later);
    let persisted = jobs(&f, task_id);
    assert_eq!(
        persisted
            .iter()
            .find(|job| job.id == checked.id)
            .unwrap()
            .checks[0]
            .run,
        "printf approved"
    );
    f.close().await;
}

#[tokio::test]
async fn actual_snapshot_excludes_no_change_even_when_the_agent_claims_changed_files() {
    let (f, project_id, task_id, run_id) = running().await;
    approve(&f, project_id, 0, definition("printf approved"));
    f.hub.capture_checks_start(run_id).await.unwrap();
    let mut ending = end(run_id);
    ending.outcome = Some(RunOutcome {
        summary: None,
        changed_files: vec![ChangedFile {
            path: "invented.txt".into(),
            change: FileChange::Added,
            added: None,
            removed: None,
            diff: None,
            provenance: None,
        }],
        checks: vec![],
        still_active: vec![],
        unknowns: vec![],
        usage: None,
        report: None,
        verification: None,
    });
    assert!(!admit(&f, run_id, ending).await);
    assert!(jobs(&f, task_id).is_empty());
    assert_eq!(writer(&f, project_id), run_id);
    let record: serde_json::Value = f
        .hub
        .read(|r| r.require("check_run_snapshots", run_id))
        .unwrap();
    assert!(record["before"].is_string());
    assert!(record["after"].is_string());
    f.close().await;
}

#[tokio::test]
async fn actual_file_changes_select_checks_and_record_off_skipped_and_changed_inputs() {
    let (f, project_id, task_id, run_id) = running().await;
    let mut selected = definition("printf selected");
    selected.paths = vec!["src/**".into()];
    selected.inputs = vec!["package.json".into()];
    std::fs::write(f.project_dir().join("package.json"), "approved input").unwrap();
    approve(&f, project_id, 0, selected.clone());
    let mut skipped = definition("printf skipped");
    skipped.paths = vec!["docs/**".into()];
    approve(&f, project_id, 1, skipped.clone());
    let mut off = definition("printf off");
    off.enabled = false;
    approve(&f, project_id, 2, off.clone());
    let mut merge = definition("printf before-merge");
    merge.when = CheckWhen::BeforeMerge;
    approve(&f, project_id, 3, merge.clone());
    f.hub.capture_checks_start(run_id).await.unwrap();
    std::fs::create_dir(f.project_dir().join("src")).unwrap();
    std::fs::write(f.project_dir().join("src/new.rs"), "changed").unwrap();
    std::fs::write(f.project_dir().join("package.json"), "changed input").unwrap();
    assert!(admit(&f, run_id, end(run_id)).await);
    let job = jobs(&f, task_id).remove(0);
    assert_eq!(job.checks_version, 4);
    assert_eq!(job.checks.len(), 4);
    assert_eq!(job.results.len(), 3);
    assert!(
        !job.results
            .iter()
            .any(|result| result.check_id == selected.check_id)
    );
    for check in [&skipped, &merge] {
        assert_eq!(
            job.results
                .iter()
                .find(|result| result.check_id == check.check_id)
                .unwrap()
                .outcome,
            CheckOutcome::SkippedPaths
        );
    }
    assert_eq!(
        job.results
            .iter()
            .find(|result| result.check_id == off.check_id)
            .unwrap()
            .outcome,
        CheckOutcome::Off
    );
    let notices: BTreeMap<CheckId, Vec<String>> =
        f.hub.read(|r| r.require(JOB_INPUTS, job.id)).unwrap();
    assert_eq!(notices[&selected.check_id], vec!["package.json"]);
    f.close().await;
}

#[tokio::test]
async fn no_checks_or_only_disabled_checks_preserve_the_original_writer_and_task() {
    for disabled in [false, true] {
        let (f, project_id, task_id, run_id) = running().await;
        if disabled {
            let mut check = definition("printf off");
            check.enabled = false;
            approve(&f, project_id, 0, check);
        }
        f.hub.capture_checks_start(run_id).await.unwrap();
        std::fs::write(f.project_dir().join("app.txt"), "after").unwrap();
        let before: Task = f.hub.read(|r| r.require("task", task_id)).unwrap();
        assert!(!admit(&f, run_id, end(run_id)).await);
        assert!(jobs(&f, task_id).is_empty());
        let after: Task = f.hub.read(|r| r.require("task", task_id)).unwrap();
        assert_eq!(after, before);
        assert_eq!(writer(&f, project_id), run_id);
        f.close().await;
    }
}

#[tokio::test]
async fn ask_plan_read_only_interrupted_uncertain_helpers_and_needs_person_are_excluded() {
    for exclusion in [
        "ask",
        "plan",
        "read-only",
        "interrupted",
        "uncertain",
        "already-uncertain",
        "helpers",
        "cleanup",
        "needs-person",
        "questions",
        "pending-question",
    ] {
        let (f, project_id, task_id, run_id) = running().await;
        approve(&f, project_id, 0, definition("printf approved"));
        f.hub.capture_checks_start(run_id).await.unwrap();
        std::fs::write(f.project_dir().join("app.txt"), "after").unwrap();
        let mut ending = end(run_id);
        transaction(&f, |tx| {
            let mut run: Run = tx.read().require("run", run_id)?;
            match exclusion {
                "ask" => run.mode = Some(TaskMode::Ask),
                "plan" => run.mode = Some(TaskMode::Plan),
                "read-only" => run.holds_writer = false,
                "already-uncertain" => run.state = RunState::ReconciliationRequired,
                "cleanup" => run.cleanup_pending = true,
                "interrupted" => ending.state = RunState::Interrupted,
                "uncertain" => ending.state = RunState::ReconciliationRequired,
                "helpers" => ending.helpers_running = true,
                "needs-person" | "questions" => {
                    ending.outcome = Some(RunOutcome {
                        summary: None,
                        changed_files: vec![],
                        checks: vec![],
                        still_active: vec![],
                        unknowns: vec![],
                        usage: None,
                        report: Some(RunReport {
                            status: if exclusion == "needs-person" {
                                ReportStatus::NeedsYou
                            } else {
                                ReportStatus::Done
                            },
                            questions: if exclusion == "questions" {
                                vec!["Which account?".into()]
                            } else {
                                vec![]
                            },
                            remaining: vec![],
                            new_work: vec![],
                        }),
                        verification: None,
                    })
                }
                "pending-question" => {
                    let approval = Approval {
                        approval_id: ApprovalId::new(),
                        run_id,
                        task_id,
                        project_id,
                        native_request_id: "question-1".into(),
                        kind: ApprovalKind::Question,
                        title: "Which account?".into(),
                        headline: None,
                        facts: vec![],
                        detail: None,
                        detail_lines: vec![],
                        questions: vec![],
                        raw_request: None,
                        scope: "one question".into(),
                        shortened: false,
                        revision: 1,
                        state: ApprovalState::Pending,
                        access_request: None,
                        actions: None,
                        delivery: None,
                        outcome: None,
                        requested_at: f.clock.now(),
                        expires_at: None,
                        resolved_by: None,
                        resolved_at: None,
                    };
                    tx.put("approval", approval.approval_id, &approval)?;
                }
                _ => unreachable!(),
            }
            tx.put("run", run_id, &run)
        });
        assert!(
            !admit(&f, run_id, ending).await,
            "Unexpected checks for {exclusion}"
        );
        assert!(
            jobs(&f, task_id).is_empty(),
            "Unexpected job for {exclusion}"
        );
        assert_eq!(writer(&f, project_id), run_id);
        f.close().await;
    }
}

#[tokio::test]
async fn first_setup_and_unchanged_work_still_get_an_explicit_candidate_proof() {
    for with_approved in [false, true] {
        let (f, project_id, task_id, run_id) = running().await;
        let base = if with_approved {
            approve(&f, project_id, 0, definition("printf approved"));
            1
        } else {
            0
        };
        f.hub.capture_checks_start(run_id).await.unwrap();
        let candidate = definition("printf candidate");
        let draft = transaction(&f, |tx| {
            tx.propose_checks(
                project_id,
                base,
                Some(task_id),
                None,
                vec![CheckDraftItem::Add {
                    check: candidate.clone(),
                    note: "Prove this new check.".into(),
                }],
                &f.project_dir(),
            )
        });
        assert!(admit(&f, run_id, end(run_id)).await);
        let admitted = jobs(&f, task_id);
        assert_eq!(admitted.len(), 1);
        assert_eq!(
            admitted[0].purpose,
            CheckJobPurpose::DraftProof { draft_id: draft.id }
        );
        assert_eq!(admitted[0].checks, vec![candidate]);
        assert_eq!(writer(&f, project_id), RunId(admitted[0].id.0));
        if !with_approved {
            assert!(
                f.hub
                    .read(|r| r.get::<serde_json::Value>("check_run_snapshots", run_id))
                    .unwrap()
                    .is_none(),
                "A proof-only first setup must not do an end snapshot"
            );
        }
        f.close().await;
    }
}

#[tokio::test]
async fn handovers_reserve_checks_then_proof_then_finalization_without_an_unowned_writer() {
    let (f, project_id, task_id, run_id) = running().await;
    let (next_task, _) = f
        .task(
            project_id,
            TaskPlacement::Queue {
                position: None,
                authorization: QueueAuthorization::AutoStart,
            },
            &Caller::local_owner(),
            false,
        )
        .await;
    assert_queued_task_cannot_start(&f, next_task);
    approve(&f, project_id, 0, definition("printf approved"));
    f.hub.capture_checks_start(run_id).await.unwrap();
    transaction(&f, |tx| {
        tx.propose_checks(
            project_id,
            1,
            Some(task_id),
            None,
            vec![CheckDraftItem::Add {
                check: definition("printf proof"),
                note: "Prove it.".into(),
            }],
            &f.project_dir(),
        )
    });
    std::fs::write(f.project_dir().join("app.txt"), "after").unwrap();
    assert!(admit(&f, run_id, end(run_id)).await);
    let admitted = jobs(&f, task_id);
    let after = admitted
        .iter()
        .find(|job| job.purpose == CheckJobPurpose::AfterRun)
        .unwrap();
    let proof = admitted
        .iter()
        .find(|job| matches!(job.purpose, CheckJobPurpose::DraftProof { .. }))
        .unwrap();
    assert_eq!(writer(&f, project_id), RunId(after.id.0));
    let legacy_run: Run = f.hub.read(|r| r.require("run", run_id)).unwrap();
    assert!(
        legacy_run.cleanup_pending,
        "Older phones must keep Stop available during checks"
    );
    assert!(
        !legacy_run.holds_writer,
        "Only the check job owns the actual writer"
    );
    assert_queued_task_cannot_start(&f, next_task);
    assert!(finish(&f, after.id, false).is_none());
    assert_eq!(writer(&f, project_id), RunId(proof.id.0));
    assert_queued_task_cannot_start(&f, next_task);
    assert_eq!(
        f.hub
            .read(|r| r.require::<Task>("task", task_id))
            .unwrap()
            .checking
            .unwrap()
            .job_id,
        proof.id
    );
    assert!(
        transaction(&f, |tx| f.hub.advance_task_checks(tx, after.id)).is_none(),
        "A repeated earlier callback must not seize the proof's writer"
    );
    let original = finish(&f, proof.id, false).expect("All checks return original completion");
    assert_eq!(original.run_id, run_id);
    assert_eq!(original.state, RunState::Completed);
    assert_eq!(
        writer(&f, project_id),
        run_id,
        "Finalization still owns the writer until its completion effects commit"
    );
    assert_queued_task_cannot_start(&f, next_task);
    assert!(
        !f.hub
            .read(|r| r.require::<Run>("run", run_id))
            .unwrap()
            .cleanup_pending,
        "Finalization must clear the compatibility cleanup flag"
    );
    assert!(
        f.hub
            .read(|r| r.require::<bool>(FINALIZING, run_id))
            .unwrap()
    );
    assert!(
        f.hub
            .read(|r| r.require::<Run>("run", run_id))
            .unwrap()
            .holds_writer
    );
    assert!(
        f.hub
            .read(|r| r.require::<Task>("task", task_id))
            .unwrap()
            .checking
            .is_some(),
        "Old Stop remains routable until end_run replay"
    );
    let recovered = f.hub.recoverable_deferred_ends().unwrap();
    assert_eq!(recovered.len(), 1);
    assert_eq!(recovered[0].run_id, run_id);
    assert!(transaction(&f, |tx| f.hub.advance_task_checks(tx, proof.id)).is_none());
    assert!(
        !admit(&f, run_id, end(run_id)).await,
        "Finalization cannot schedule checks again"
    );
    f.close().await;
}

#[tokio::test]
async fn stop_cancels_the_pending_proof_and_keeps_original_completion_with_stopped_note() {
    let (f, project_id, task_id, run_id) = running().await;
    approve(&f, project_id, 0, definition("printf approved"));
    f.hub.capture_checks_start(run_id).await.unwrap();
    transaction(&f, |tx| {
        tx.propose_checks(
            project_id,
            1,
            Some(task_id),
            None,
            vec![CheckDraftItem::Add {
                check: definition("printf proof"),
                note: "Prove it.".into(),
            }],
            &f.project_dir(),
        )
    });
    std::fs::write(f.project_dir().join("app.txt"), "after").unwrap();
    let mut ending = end(run_id);
    ending.why = Some("Original ending.".into());
    assert!(admit(&f, run_id, ending).await);
    let admitted = jobs(&f, task_id);
    let after = admitted
        .iter()
        .find(|job| job.purpose == CheckJobPurpose::AfterRun)
        .unwrap();
    let proof = admitted
        .iter()
        .find(|job| matches!(job.purpose, CheckJobPurpose::DraftProof { .. }))
        .unwrap();
    let original = finish(&f, after.id, true).unwrap();
    assert_eq!(
        original.why.as_deref(),
        Some("Original ending.\n\nNot checked: stopped")
    );
    assert_eq!(original.state, RunState::Completed);
    let cancelled: CheckJob = f
        .hub
        .read(|r| r.require(store::checks::JOBS, proof.id))
        .unwrap();
    assert_eq!(cancelled.state, CheckJobState::Finished);
    assert_eq!(cancelled.results[0].outcome, CheckOutcome::Stopped);
    let draft = f
        .hub
        .read(|r| r.task_checks_draft(task_id))
        .unwrap()
        .unwrap();
    assert_eq!(draft.state, ChecksDraftState::Ready);
    assert_eq!(draft.proofs, cancelled.results);
    assert_eq!(writer(&f, project_id), run_id);
    let metadata: serde_json::Value = f.hub.read(|r| r.require(DEFERRED_END, task_id)).unwrap();
    assert_eq!(metadata["job_ids"].as_array().unwrap().len(), 2);
    f.close().await;
}

#[tokio::test]
async fn a_failed_normal_work_run_is_checked_and_retains_its_original_outcome() {
    let (f, project_id, task_id, run_id) = running().await;
    approve(&f, project_id, 0, definition("printf approved"));
    f.hub.capture_checks_start(run_id).await.unwrap();
    std::fs::write(f.project_dir().join("app.txt"), "after").unwrap();
    let mut ending = end(run_id);
    ending.state = RunState::Failed;
    assert!(admit(&f, run_id, ending).await);
    let job = jobs(&f, task_id).remove(0);
    assert_eq!(finish(&f, job.id, false).unwrap().state, RunState::Failed);
    f.close().await;
}

#[tokio::test]
async fn missing_start_snapshot_is_never_mistaken_for_no_changes() {
    let (f, project_id, task_id, run_id) = running().await;
    approve(&f, project_id, 0, definition("printf approved"));
    // Approval arrived during Work; there was deliberately no start record.
    assert!(admit(&f, run_id, end(run_id)).await);
    let job = jobs(&f, task_id).remove(0);
    assert!(job.results.is_empty());
    assert!(job.message.unwrap().contains("conservatively"));
    f.close().await;
}

#[tokio::test]
async fn failed_admission_transaction_rolls_back_jobs_progress_and_both_writers_together() {
    let (f, project_id, task_id, run_id) = running().await;
    approve(&f, project_id, 0, definition("printf approved"));
    f.hub.capture_checks_start(run_id).await.unwrap();
    std::fs::write(f.project_dir().join("app.txt"), "after").unwrap();
    f.hub.prepare_checks_end(&end(run_id)).await.unwrap();
    let original_task: Task = f.hub.read(|r| r.require("task", task_id)).unwrap();
    let original_run: Run = f.hub.read(|r| r.require("run", run_id)).unwrap();
    let rejected = f.hub.inner.db.lock().unwrap().write(f.clock.now(), |tx| {
        let mut run = original_run.clone();
        let mut task = original_task.clone();
        assert!(
            f.hub
                .defer_run_checks(tx, &mut run, &mut task, end(run_id))?
        );
        let project: Project = tx.read().require("project", project_id)?;
        let queue: QueueView = tx.read().require("queue", project_id)?;
        assert_eq!(project.active_writer, queue.writer);
        assert_ne!(project.active_writer, Some(run_id));
        assert!(project.active_writer.is_some());
        Err::<(), _>(store::Error::IntegerExhausted)
    });
    assert!(rejected.is_err());
    assert!(jobs(&f, task_id).is_empty());
    assert_eq!(writer(&f, project_id), run_id);
    assert_eq!(
        f.hub.read(|r| r.require::<Task>("task", task_id)).unwrap(),
        original_task
    );
    assert_eq!(
        f.hub.read(|r| r.require::<Run>("run", run_id)).unwrap(),
        original_run
    );
    assert!(
        f.hub
            .read(|r| r.get::<serde_json::Value>(DEFERRED_END, task_id))
            .unwrap()
            .is_none()
    );
    assert!(
        admit(&f, run_id, end(run_id)).await,
        "A rolled-back admission can be safely retried"
    );
    f.close().await;
}

#[tokio::test]
async fn snapshots_are_only_started_for_work_writers_and_never_recaptured_on_retry() {
    for mode in ["ask", "plan", "read-only", "no-checks", "work"] {
        let (f, project_id, _, run_id) = running().await;
        if mode != "no-checks" {
            approve(&f, project_id, 0, definition("printf approved"));
        }
        transaction(&f, |tx| {
            let mut run: Run = tx.read().require("run", run_id)?;
            match mode {
                "ask" => run.mode = Some(TaskMode::Ask),
                "plan" => run.mode = Some(TaskMode::Plan),
                "read-only" => run.holds_writer = false,
                _ => {}
            }
            tx.put("run", run_id, &run)
        });
        f.hub.capture_checks_start(run_id).await.unwrap();
        let before: Option<serde_json::Value> = f
            .hub
            .read(|r| r.get("check_run_snapshots", run_id))
            .unwrap();
        if mode == "work" {
            assert!(before.is_some());
            std::fs::write(f.project_dir().join("app.txt"), "after").unwrap();
            f.hub.capture_checks_start(run_id).await.unwrap();
            let retry: Option<serde_json::Value> = f
                .hub
                .read(|r| r.get("check_run_snapshots", run_id))
                .unwrap();
            assert_eq!(
                before, retry,
                "Dispatch retry must keep the original start record"
            );
            assert!(admit(&f, run_id, end(run_id)).await);
        } else {
            assert!(before.is_none(), "Unexpected start snapshot for {mode}");
        }
        f.close().await;
    }
}

#[tokio::test]
async fn reported_needs_person_in_a_native_summary_excludes_checks_before_finalization() {
    let (f, project_id, task_id, run_id) = running().await;
    approve(&f, project_id, 0, definition("printf approved"));
    f.hub.capture_checks_start(run_id).await.unwrap();
    std::fs::write(f.project_dir().join("app.txt"), "after").unwrap();
    let mut ending = end(run_id);
    ending.outcome = Some(RunOutcome {
        summary: Some("Please choose.\n```ophio-outcome\n{\"status\":\"needs_you\",\"questions\":[\"Which account?\"]}\n```".into()),
        changed_files: vec![], checks: vec![], still_active: vec![], unknowns: vec![], usage: None, report: None, verification: None,
    });
    assert!(!admit(&f, run_id, ending).await);
    assert!(jobs(&f, task_id).is_empty());
    assert_eq!(writer(&f, project_id), run_id);
    f.close().await;
}

#[tokio::test]
async fn work_conversations_need_project_write_access_not_just_the_writer_slot() {
    for grant in [
        "read-only-task",
        "read-only-chat",
        "read-only-at-launch",
        "writable-project",
    ] {
        let (f, project_id, task_id, run_id) = running().await;
        approve(&f, project_id, 0, definition("printf approved"));
        transaction(&f, |tx| {
            let mut task: Task = tx.read().require("task", task_id)?;
            task.conversation = grant == "read-only-chat";
            let mut access = protocol::access::ConversationAccess::default();
            if matches!(grant, "writable-project" | "read-only-at-launch") {
                access.write.push(project_id);
            }
            task.access = Some(access);
            tx.put("task", task_id, &task)?;
            if grant == "read-only-at-launch" {
                tx.put(
                    "run_access",
                    run_id,
                    &Some(protocol::access::ConversationAccess::default()),
                )?;
            }
            Ok(())
        });
        f.hub.capture_checks_start(run_id).await.unwrap();
        let snapshot: Option<serde_json::Value> = f
            .hub
            .read(|r| r.get("check_run_snapshots", run_id))
            .unwrap();
        assert_eq!(
            snapshot.is_some(),
            grant == "writable-project",
            "Unexpected capture for {grant}"
        );
        std::fs::write(f.project_dir().join("app.txt"), "after").unwrap();
        assert_eq!(
            admit(&f, run_id, end(run_id)).await,
            grant == "writable-project",
            "Unexpected admission for {grant}"
        );
        assert_eq!(jobs(&f, task_id).is_empty(), grant != "writable-project");
        f.close().await;
    }
}

#[tokio::test]
async fn a_discarded_pending_proof_never_gets_the_writer_or_changes_original_completion() {
    let (f, project_id, task_id, run_id) = running().await;
    approve(&f, project_id, 0, definition("printf approved"));
    f.hub.capture_checks_start(run_id).await.unwrap();
    let draft = transaction(&f, |tx| {
        tx.propose_checks(
            project_id,
            1,
            Some(task_id),
            None,
            vec![CheckDraftItem::Add {
                check: definition("printf discarded"),
                note: "Try this.".into(),
            }],
            &f.project_dir(),
        )
    });
    std::fs::write(f.project_dir().join("app.txt"), "after").unwrap();
    assert!(admit(&f, run_id, end(run_id)).await);
    transaction(&f, |tx| tx.discard_checks_draft(draft.id));
    let admitted = jobs(&f, task_id);
    let after = admitted
        .iter()
        .find(|job| job.purpose == CheckJobPurpose::AfterRun)
        .unwrap();
    let proof = admitted
        .iter()
        .find(|job| matches!(job.purpose, CheckJobPurpose::DraftProof { .. }))
        .unwrap();
    let ending =
        finish(&f, after.id, false).expect("Discarded proof does not delay original completion");
    assert_eq!(ending.state, RunState::Completed);
    assert!(ending.why.is_none(), "Discarding is not stopping the task");
    assert_eq!(writer(&f, project_id), run_id);
    let cancelled: CheckJob = f
        .hub
        .read(|r| r.require(store::checks::JOBS, proof.id))
        .unwrap();
    assert_eq!(cancelled.state, CheckJobState::Finished);
    assert!(cancelled.started_at.is_none());
    assert!(cancelled.process.is_none());
    assert!(
        matches!(&cancelled.results[0].outcome, CheckOutcome::CouldNotRun { reason } if reason.contains("discarded"))
    );
    f.close().await;
}

#[tokio::test]
async fn an_already_stopped_later_proof_is_not_reactivated_or_silently_treated_as_checked() {
    let (f, project_id, task_id, run_id) = running().await;
    approve(&f, project_id, 0, definition("printf approved"));
    f.hub.capture_checks_start(run_id).await.unwrap();
    transaction(&f, |tx| {
        tx.propose_checks(
            project_id,
            1,
            Some(task_id),
            None,
            vec![CheckDraftItem::Add {
                check: definition("printf cancelled"),
                note: "Try this.".into(),
            }],
            &f.project_dir(),
        )
    });
    std::fs::write(f.project_dir().join("app.txt"), "after").unwrap();
    assert!(admit(&f, run_id, end(run_id)).await);
    let admitted = jobs(&f, task_id);
    let after = admitted
        .iter()
        .find(|job| job.purpose == CheckJobPurpose::AfterRun)
        .unwrap();
    let proof = admitted
        .iter()
        .find(|job| matches!(job.purpose, CheckJobPurpose::DraftProof { .. }))
        .unwrap();
    assert!(
        finish(&f, proof.id, true).is_none(),
        "Finishing an unowned later job cannot seize the writer"
    );
    assert_eq!(writer(&f, project_id), RunId(after.id.0));
    let ending = finish(&f, after.id, false).unwrap();
    assert_eq!(ending.why.as_deref(), Some("Not checked: stopped"));
    assert_eq!(writer(&f, project_id), run_id);
    let cancelled: CheckJob = f
        .hub
        .read(|r| r.require(store::checks::JOBS, proof.id))
        .unwrap();
    assert_eq!(cancelled.state, CheckJobState::Finished);
    assert!(cancelled.started_at.is_none());
    f.close().await;
}

#[tokio::test]
async fn handover_without_preparation_fails_instead_of_reading_project_files() {
    let (f, project_id, task_id, run_id) = running().await;
    approve(&f, project_id, 0, definition("printf approved"));
    let rejected = f.hub.inner.db.lock().unwrap().write(f.clock.now(), |tx| {
        let mut run: Run = tx.read().require("run", run_id)?;
        let mut task: Task = tx.read().require("task", task_id)?;
        f.hub.defer_run_checks(tx, &mut run, &mut task, end(run_id))
    });
    assert!(
        matches!(rejected, Err(store::Error::Rule(ref cause)) if cause.code == ErrorCode::Conflict)
    );
    assert!(jobs(&f, task_id).is_empty());
    assert_eq!(writer(&f, project_id), run_id);
    assert!(
        f.hub
            .read(|r| r.get::<serde_json::Value>("check_run_snapshots", run_id))
            .unwrap()
            .is_none()
    );
    f.close().await;
}

#[tokio::test]
async fn preparation_database_failure_keeps_the_original_end_for_retry() {
    let (f, project_id, task_id, run_id) = running().await;
    approve(&f, project_id, 0, definition("printf approved"));
    f.hub.capture_checks_start(run_id).await.unwrap();
    std::fs::write(f.project_dir().join("app.txt"), "the original end").unwrap();
    let db = rusqlite::Connection::open(f.path.join("companion.sqlite")).unwrap();
    db.execute_batch(
        "CREATE TRIGGER reject_prepared_end BEFORE UPDATE ON documents
        WHEN NEW.kind = 'check_run_snapshots'
        BEGIN SELECT RAISE(ABORT, 'synthetic snapshot disk failure'); END;",
    )
    .unwrap();
    assert!(f.hub.prepare_checks_end(&end(run_id)).await.is_err());
    let pending = f
        .path
        .join("check-snapshots/prepared-ends")
        .join(format!("{run_id}.json"));
    let saved: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&pending).unwrap()).unwrap();
    assert!(saved["after"].is_string());
    let original: serde_json::Value = f
        .hub
        .read(|r| r.require("check_run_snapshots", run_id))
        .unwrap();
    assert!(original["after"].is_null());
    assert_eq!(writer(&f, project_id), run_id);
    assert!(jobs(&f, task_id).is_empty());
    db.execute_batch("DROP TRIGGER reject_prepared_end;")
        .unwrap();
    drop(db);
    std::fs::write(f.project_dir().join("app.txt"), "later unrelated contents").unwrap();
    f.hub.prepare_checks_end(&end(run_id)).await.unwrap();
    let retried: serde_json::Value = f
        .hub
        .read(|r| r.require("check_run_snapshots", run_id))
        .unwrap();
    assert_eq!(retried["before"], saved["before"]);
    assert_eq!(
        retried["after"], saved["after"],
        "A failed database write must not recapture the end"
    );
    assert!(!pending.exists());
    assert!(admit(&f, run_id, end(run_id)).await);
    f.close().await;
}

#[tokio::test]
async fn a_new_approval_requires_new_input_preparation_but_reuses_the_end_manifest() {
    let (f, project_id, task_id, run_id) = running().await;
    let mut check = definition("printf original");
    check.inputs = vec!["app.txt".into()];
    approve(&f, project_id, 0, check.clone());
    f.hub.capture_checks_start(run_id).await.unwrap();
    std::fs::write(f.project_dir().join("app.txt"), "after").unwrap();
    f.hub.prepare_checks_end(&end(run_id)).await.unwrap();
    let original: serde_json::Value = f
        .hub
        .read(|r| r.require("check_run_snapshots", run_id))
        .unwrap();
    check.run = "printf newly-approved".into();
    approve(&f, project_id, 1, check.clone());
    let rejected = f.hub.inner.db.lock().unwrap().write(f.clock.now(), |tx| {
        let mut run: Run = tx.read().require("run", run_id)?;
        let mut task: Task = tx.read().require("task", task_id)?;
        f.hub.defer_run_checks(tx, &mut run, &mut task, end(run_id))
    });
    assert!(
        matches!(rejected, Err(store::Error::Rule(ref cause)) if cause.code == ErrorCode::Conflict)
    );
    assert!(jobs(&f, task_id).is_empty());
    assert_eq!(writer(&f, project_id), run_id);
    assert!(admit(&f, run_id, end(run_id)).await);
    let retried: serde_json::Value = f
        .hub
        .read(|r| r.require("check_run_snapshots", run_id))
        .unwrap();
    assert_eq!(retried["after"], original["after"]);
    let job = jobs(&f, task_id).remove(0);
    assert_eq!(job.checks_version, 2);
    assert_eq!(job.checks, vec![check]);
    let inputs: BTreeMap<CheckId, Vec<String>> =
        f.hub.read(|r| r.require(JOB_INPUTS, job.id)).unwrap();
    assert!(
        inputs.values().all(Vec::is_empty),
        "The new approval accepted the input that used to be changed"
    );
    f.close().await;
}

#[tokio::test]
async fn duplicate_capture_after_handover_and_final_handback_cannot_create_a_new_baseline_or_end() {
    let (f, project_id, task_id, run_id) = running().await;
    approve(&f, project_id, 0, definition("printf approved"));
    f.hub.capture_checks_start(run_id).await.unwrap();
    std::fs::write(f.project_dir().join("app.txt"), "after").unwrap();
    assert!(admit(&f, run_id, end(run_id)).await);
    let job = jobs(&f, task_id).remove(0);
    transaction(&f, |tx| tx.delete("check_run_snapshots", run_id));
    for finalizing in [false, true] {
        if finalizing {
            assert!(finish(&f, job.id, false).is_some());
            assert_eq!(writer(&f, project_id), run_id);
        }
        std::fs::write(f.project_dir().join("app.txt"), "later contents").unwrap();
        f.hub.capture_checks_start(run_id).await.unwrap();
        f.hub.prepare_checks_end(&end(run_id)).await.unwrap();
        assert!(
            f.hub
                .read(|r| r.get::<serde_json::Value>("check_run_snapshots", run_id))
                .unwrap()
                .is_none()
        );
        assert_eq!(jobs(&f, task_id).len(), 1);
    }
    f.close().await;
}

#[tokio::test]
async fn thousands_of_input_files_are_prepared_before_handover_and_never_rehashed_inside_it() {
    let (f, project_id, task_id, run_id) = running().await;
    let mut checks = Vec::new();
    for index in 0..32 {
        let mut check = definition("printf approved");
        for input in 0..64 {
            let path = format!("input-{index:02}-{input:02}.txt");
            std::fs::write(f.project_dir().join(&path), "approved").unwrap();
            check.inputs.push(path);
        }
        approve(&f, project_id, index, check.clone());
        checks.push(check);
    }
    f.hub.capture_checks_start(run_id).await.unwrap();
    std::fs::write(f.project_dir().join(&checks[0].inputs[0]), "changed").unwrap();
    f.hub.prepare_checks_end(&end(run_id)).await.unwrap();
    // If admission reads or hashes files again, it would now report all 2,048
    // missing inputs. It must use the notices computed before this transaction.
    for check in &checks {
        for input in &check.inputs {
            std::fs::remove_file(f.project_dir().join(input)).unwrap();
        }
    }
    let start = std::time::Instant::now();
    assert!(transaction(&f, |tx| {
        let mut run: Run = tx.read().require("run", run_id)?;
        let mut task: Task = tx.read().require("task", task_id)?;
        f.hub.defer_run_checks(tx, &mut run, &mut task, end(run_id))
    }));
    eprintln!(
        "END_INPUTS_PREPARED inputs=2048 handover_ms={:.3}",
        start.elapsed().as_secs_f64() * 1000.0
    );
    let job = jobs(&f, task_id).remove(0);
    let notices: BTreeMap<CheckId, Vec<String>> =
        f.hub.read(|r| r.require(JOB_INPUTS, job.id)).unwrap();
    assert_eq!(
        notices[&checks[0].check_id],
        vec![checks[0].inputs[0].clone()]
    );
    for check in &checks[1..] {
        assert!(notices[&check.check_id].is_empty());
    }
    f.close().await;
}

#[tokio::test]
async fn stop_after_preparation_prevents_admission_and_cannot_be_undone_by_an_exit_retry() {
    let (f, project_id, task_id, run_id) = running().await;
    approve(&f, project_id, 0, definition("printf approved"));
    f.hub.capture_checks_start(run_id).await.unwrap();
    std::fs::write(f.project_dir().join("app.txt"), "after").unwrap();
    f.hub.prepare_checks_end(&end(run_id)).await.unwrap();
    let prepared: serde_json::Value = f
        .hub
        .read(|r| r.require("check_run_snapshots", run_id))
        .unwrap();
    transaction(&f, |tx| {
        let mut run: Run = tx.read().require("run", run_id)?;
        run.state = RunState::Stopping;
        tx.put("run", run_id, &run)
    });
    assert!(!admit(&f, run_id, end(run_id)).await);
    assert!(jobs(&f, task_id).is_empty());
    assert_eq!(writer(&f, project_id), run_id);
    let retried: serde_json::Value = f
        .hub
        .read(|r| r.require("check_run_snapshots", run_id))
        .unwrap();
    assert_eq!(retried, prepared);
    f.close().await;
}

#[tokio::test]
async fn preparation_does_not_let_an_earlier_replys_report_exclude_a_later_completed_reply() {
    let (f, project_id, task_id, run_id) = running().await;
    approve(&f, project_id, 0, definition("printf approved"));
    f.hub.capture_checks_start(run_id).await.unwrap();
    transaction(&f, |tx| {
        tx.emit(Event::TimelineAppended {
            task_id,
            item: TimelineItem {
                seq: 0,
                at: f.clock.now(),
                run_id: Some(run_id),
                item: TimelineBody::AgentMessage {
                    text: "```ophio-outcome\n{\"status\":\"needs_you\"}\n```".into(),
                    partial: false,
                    message_key: None,
                    text_offset: None,
                },
            },
        })
    });
    std::fs::write(f.project_dir().join("app.txt"), "after").unwrap();
    f.hub.prepare_checks_end(&end(run_id)).await.unwrap();
    // end_run flushes the pending final reply after preparation. Admission
    // must see that report, not a decision made from the earlier reply.
    transaction(&f, |tx| {
        tx.emit(Event::TimelineAppended {
            task_id,
            item: TimelineItem {
                seq: 0,
                at: f.clock.now(),
                run_id: Some(run_id),
                item: TimelineBody::AgentMessage {
                    text: "```ophio-outcome\n{\"status\":\"done\"}\n```".into(),
                    partial: false,
                    message_key: None,
                    text_offset: None,
                },
            },
        })?;
        let mut run: Run = tx.read().require("run", run_id)?;
        let mut task: Task = tx.read().require("task", task_id)?;
        assert!(
            f.hub
                .defer_run_checks(tx, &mut run, &mut task, end(run_id))?
        );
        Ok(())
    });
    assert_eq!(jobs(&f, task_id).len(), 1);
    f.close().await;
}
