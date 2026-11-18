// SPDX-License-Identifier: Apache-2.0
use super::*;
use protocol::checks::*;

async fn fixture(checks: bool) -> (Fixture, TaskId, RunId, PathBuf) {
    let f = Fixture::new().await;
    f.hub.inner.checks.paused.store(true, Ordering::Release);
    let task = f.task(Some(TaskMode::Work)).await;
    let id = task.current_run.unwrap();
    f.dispatched(id).await;
    let root = PathBuf::from(
        f.hub
            .read(|r| Ok(r.require::<Project>("project", f.project)?.host_path))
            .unwrap(),
    );
    std::fs::write(root.join("app.txt"), "before").unwrap();
    if checks {
        f.hub
            .write(|tx| {
                tx.change_checks(
                    f.project,
                    0,
                    CheckEdit::Save {
                        check: Check {
                            check_id: CheckId::new(),
                            name: "App works".into(),
                            about: "Find broken changes".into(),
                            run: "printf checked".into(),
                            setup: None,
                            paths: vec![],
                            when: CheckWhen::EveryTask,
                            timeout_secs: 10,
                            network: false,
                            inputs: vec![],
                            enabled: true,
                            typical_secs: None,
                        },
                    },
                    None,
                    &root,
                )?;
                Ok(())
            })
            .unwrap();
        f.hub.capture_checks_start(id).await.unwrap();
    }
    (f, task.task_id, id, root)
}

#[tokio::test]
async fn changed_work_waits_for_helpers_then_hands_writer_to_checks() {
    let (f, task_id, id, root) = fixture(true).await;
    f.hub
        .harness_event(
            id,
            HarnessEvent::ParentEnded {
                exit: RunExit::Completed,
            },
        )
        .await
        .unwrap();
    let waiting: Task = f.hub.read(|r| r.require("task", task_id)).unwrap();
    assert_eq!(waiting.state, TaskState::Working);
    assert!(waiting.checking.is_none());
    assert_writer(&f, Some(id));
    assert!(
        f.hub
            .read(|r| r.project_check_jobs(f.project))
            .unwrap()
            .is_empty()
    );
    // Only the helper changes files: an early end snapshot would miss this.
    std::fs::write(root.join("app.txt"), "helper finished").unwrap();
    f.hub
        .harness_event(
            id,
            HarnessEvent::Exited {
                exit: RunExit::Completed,
                outcome: None,
            },
        )
        .await
        .unwrap();
    let checking: Task = f.hub.read(|r| r.require("task", task_id)).unwrap();
    assert_eq!(checking.state, TaskState::Working);
    let job_id = checking.checking.unwrap().job_id;
    assert_writer(&f, Some(RunId(job_id.0)));
    let job = f.hub.read(|r| r.check_job(job_id)).unwrap().unwrap();
    assert_eq!(job.purpose, CheckJobPurpose::AfterRun);
    assert!(job.results.is_empty());
    assert!(
        f.hub
            .read(|r| r.get::<crate::check_lifecycle::DeferredEnd>(
                crate::check_lifecycle::HELPERS_END,
                id
            ))
            .unwrap()
            .is_none()
    );
    f.close().await;
}

#[tokio::test]
async fn helpers_with_no_changes_or_no_checks_still_finish_without_jobs() {
    for checks in [true, false] {
        let (f, task_id, id, root) = fixture(checks).await;
        f.hub
            .harness_event(
                id,
                HarnessEvent::ParentEnded {
                    exit: RunExit::Completed,
                },
            )
            .await
            .unwrap();
        if !checks {
            std::fs::write(root.join("app.txt"), "changed without checks").unwrap();
            let task: Task = f.hub.read(|r| r.require("task", task_id)).unwrap();
            assert_eq!(task.state, TaskState::Completed);
        }
        assert_writer(&f, Some(id));
        f.hub
            .harness_event(
                id,
                HarnessEvent::Exited {
                    exit: RunExit::Completed,
                    outcome: None,
                },
            )
            .await
            .unwrap();
        let task: Task = f.hub.read(|r| r.require("task", task_id)).unwrap();
        assert_eq!(task.state, TaskState::Completed);
        assert!(task.checking.is_none());
        assert!(
            f.hub
                .read(|r| r.project_check_jobs(f.project))
                .unwrap()
                .is_empty()
        );
        assert_writer(&f, None);
        f.close().await;
    }
}

#[tokio::test]
async fn reconciliation_and_bypass_endings_remove_prepared_end_journal() {
    for state in [
        RunState::ReconciliationRequired,
        RunState::Interrupted,
        RunState::Completed,
    ] {
        let (f, _, id, _) = fixture(false).await;
        let journal = f
            .hub
            .inner
            .data
            .join("check-snapshots/prepared-ends")
            .join(format!("{id}.json"));
        std::fs::create_dir_all(journal.parent().unwrap()).unwrap();
        std::fs::write(&journal, "left by a failed completion write").unwrap();
        f.hub.finish_run(id, state, None).unwrap();
        assert!(!journal.exists(), "leftover journal after {state:?}");
        f.close().await;
    }
}

#[tokio::test]
async fn uncertain_or_stopped_helpers_explain_why_checks_did_not_run() {
    for exit in [
        Some(RunExit::Interrupted),
        Some(RunExit::Unknown {
            reason: "lost process".into(),
        }),
        None,
    ] {
        let (f, task_id, id, root) = fixture(true).await;
        f.hub
            .harness_event(
                id,
                HarnessEvent::ParentEnded {
                    exit: RunExit::Completed,
                },
            )
            .await
            .unwrap();
        std::fs::write(root.join("app.txt"), "changed by helper").unwrap();
        let journal = f
            .hub
            .inner
            .data
            .join("check-snapshots/prepared-ends")
            .join(format!("{id}.json"));
        std::fs::create_dir_all(journal.parent().unwrap()).unwrap();
        std::fs::write(&journal, "abandoned preparation").unwrap();
        if let Some(exit) = exit {
            f.hub
                .harness_event(
                    id,
                    HarnessEvent::Exited {
                        exit,
                        outcome: None,
                    },
                )
                .await
                .unwrap();
        } else {
            f.hub.events_lost(id).unwrap();
        }
        assert!(!journal.exists());
        assert!(
            f.hub
                .read(|r| r.project_check_jobs(f.project))
                .unwrap()
                .is_empty()
        );
        assert!(timeline(&f, task_id).iter().any(|item| matches!(&item.item,
            TimelineBody::Note { text } if text.contains("Not checked:"))));
        f.close().await;
    }
}

#[tokio::test]
async fn failed_bypass_completion_keeps_prepared_end_until_retry_commits() {
    let (f, _, id, _) = fixture(false).await;
    let journal = f
        .hub
        .inner
        .data
        .join("check-snapshots/prepared-ends")
        .join(format!("{id}.json"));
    std::fs::create_dir_all(journal.parent().unwrap()).unwrap();
    std::fs::write(&journal, "must survive failed write").unwrap();
    let db = refuse_exit_write(&f, "BEFORE UPDATE ON documents WHEN NEW.kind = 'run'");
    assert!(
        f.hub
            .finish_run(id, RunState::ReconciliationRequired, None)
            .is_err()
    );
    assert!(journal.exists());
    db.execute_batch("DROP TRIGGER refuse_exit").unwrap();
    drop(db);
    f.hub
        .finish_run(id, RunState::ReconciliationRequired, None)
        .unwrap();
    assert!(!journal.exists());
    f.close().await;
}
