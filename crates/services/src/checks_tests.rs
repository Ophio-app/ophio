// SPDX-License-Identifier: Apache-2.0
//! Checks requests use the same admission, dispatch and grants as ordinary work.
use super::*;
use protocol::{checks::*, launchers::StartedFrom};

fn check() -> Check {
    Check {
        check_id: CheckId::new(),
        name: "App tests pass".into(),
        about: "Makes sure the app still works. A failure means something needs fixing.".into(),
        run: "printf passed".into(),
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

fn proof(check: &Check) -> protocol::checks::CheckResult {
    protocol::checks::CheckResult {
        check_id: check.check_id,
        name: check.name.clone(),
        outcome: CheckOutcome::Passed,
        duration_secs: 1.0,
        exit_code: Some(0),
        output: "passed".into(),
        inputs_changed: vec![],
    }
}

async fn accepted(f: &Fixture, body: Command) -> CommandReceipt {
    let receipt = f
        .hub
        .submit(&Caller::local_owner(), CommandEnvelope::new(body))
        .await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    receipt
}

async fn command(f: &Fixture, body: Command) -> CommandResult {
    accepted(f, body)
        .await
        .result
        .expect("An immediate checks result")
}

async fn save(f: &Fixture, project_id: ProjectId, base_version: u64, check: Check) -> CheckJobId {
    // These tests cover admission, before a check process is launched.
    f.hub.inner.checks.paused.store(true, Ordering::Release);
    let CommandResult::ChecksSaved { approved, job_id } = command(
        f,
        Command::ChecksSave {
            project_id,
            base_version,
            edit: CheckEdit::Save { check },
        },
    )
    .await
    else {
        panic!("Expected saved checks");
    };
    assert_eq!(approved.version, base_version + 1);
    job_id.expect("Saving a definition schedules its proof")
}

async fn stop(f: &Fixture, job_id: CheckJobId) -> CheckJob {
    let CommandResult::CheckJob { job } = command(
        f,
        Command::ChecksStopJob {
            job_id,
            confirmed_stopped: false,
        },
    )
    .await
    else {
        panic!("Expected a check job");
    };
    job
}

async fn list(f: &Fixture, project_id: ProjectId) -> ProjectChecksView {
    let QueryData::ChecksList { mut projects, .. } = f
        .hub
        .query(
            &Caller::local_owner(),
            Query::ChecksList {
                project_id: Some(project_id),
            },
        )
        .await
        .unwrap()
    else {
        panic!("Expected checks");
    };
    assert_eq!(projects.len(), 1);
    projects.remove(0)
}

#[tokio::test]
async fn asking_without_checks_starts_one_ordinary_task_with_the_project_defaults() {
    let folder = tempfile::tempdir().unwrap();
    let dirs = LauncherDirs {
        setup_skills: Some(folder.path().join("skills")),
        ..Default::default()
    };
    let codex = Arc::new(FakeHarness::of_kind(HarnessKind::Codex));
    let other = codex.clone();
    let f = Fixture::with(move |b| b.launchers(dirs).harness(other)).await;
    let project_id = f.project().await;
    accepted(
        &f,
        Command::UpdateProjectDefaults {
            project_id,
            harness: Some(HarnessKind::Codex),
            model: Some("project-model".into()),
            effort: None,
        },
    )
    .await;
    let envelope = CommandEnvelope::new(Command::ChecksAsk {
        project_id,
        check_id: None,
        request: String::new(),
    });
    let receipt = f.hub.submit(&Caller::local_owner(), envelope.clone()).await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    let Some(CommandResult::TaskCreated {
        task_id,
        queue_item_id,
    }) = receipt.result
    else {
        panic!("Expected an ordinary task result");
    };
    assert!(queue_item_id.is_none());
    run_started(&f, task_id).await;
    let task: Task = f.hub.read(|r| r.require("task", task_id)).unwrap();
    assert_eq!(task.harness, HarnessKind::Codex);
    assert_eq!(task.model.as_deref(), Some("project-model"));
    assert_eq!(task.mode, TaskMode::Work);
    assert_eq!(task.title, "Set up checks");
    assert!(!task.conversation);
    assert!(task.review.is_none());
    assert_eq!(
        task.start,
        Some(StartedFrom::Skill {
            name: "Set up checks".into()
        })
    );
    let spec = codex.specs.lock().unwrap().last().cloned().unwrap();
    let Some(ports::RunStart::Skill {
        name,
        instructions: Some(instructions),
        ..
    }) = spec.start
    else {
        panic!("The built-in skill must reach the dispatched agent");
    };
    assert_eq!(name, "Set up checks");
    assert!(instructions.contains("checks_propose"));
    let (brief, _): (String, Option<Box<protocol::annotation::AnnotationBundle>>) =
        f.hub.read(|r| r.require("prompt", task_id)).unwrap();
    assert!(brief.contains("suggest them for me to approve"));
    let stored = f
        .hub
        .read(|r| r.get::<String>("checks_request", task_id))
        .unwrap();
    assert_eq!(
        stored, None,
        "setup must not store an invalid empty request"
    );
    let draft = f
        .hub
        .write(|tx| {
            tx.propose_checks(
                project_id,
                0,
                Some(task_id),
                stored,
                vec![CheckDraftItem::Add {
                    check: check(),
                    note: "Catch failures".into(),
                }],
                folder.path(),
            )
        })
        .unwrap();
    assert_eq!(draft.request, None);
    assert_eq!(draft.state, ChecksDraftState::Proving);
    assert!(list(&f, project_id).await.approved.checks.is_empty());

    let again = f.hub.submit(&Caller::local_owner(), envelope).await;
    assert!(
        matches!(again.result, Some(CommandResult::TaskCreated { task_id: same, .. }) if same == task_id)
    );
    assert_eq!(f.hub.read(|r| r.list::<Task>("task")).unwrap().len(), 1);
    f.close().await;
}

#[tokio::test]
async fn asking_about_one_check_preserves_its_identity_and_the_persons_exact_words() {
    let folder = tempfile::tempdir().unwrap();
    let f = Fixture::with(|b| {
        b.launchers(LauncherDirs {
            setup_skills: Some(folder.path().join("skills")),
            ..Default::default()
        })
    })
    .await;
    let project_id = f.project().await;
    let check = check();
    let job_id = save(&f, project_id, 0, check.clone()).await;
    stop(&f, job_id).await;
    let request = "  Also check the release build.\nKeep the current test.\n";
    let CommandResult::TaskCreated { task_id, .. } = command(
        &f,
        Command::ChecksAsk {
            project_id,
            check_id: Some(check.check_id),
            request: request.into(),
        },
    )
    .await
    else {
        panic!("Expected a task");
    };
    run_started(&f, task_id).await;
    let task: Task = f.hub.read(|r| r.require("task", task_id)).unwrap();
    assert_eq!(task.title, "Change checks");
    assert!(task.review.is_none());
    assert_eq!(
        task.start,
        Some(StartedFrom::Skill {
            name: "Change checks".into()
        })
    );
    let (brief, _): (String, Option<Box<protocol::annotation::AnnotationBundle>>) =
        f.hub.read(|r| r.require("prompt", task_id)).unwrap();
    assert!(brief.contains(&format!(
        "Selected check: {} ({})",
        check.name, check.check_id
    )));
    assert!(brief.ends_with(request));
    f.close().await;
}

#[tokio::test]
async fn invalid_checks_requests_are_rejected_before_creating_a_task() {
    let folder = tempfile::tempdir().unwrap();
    let f = Fixture::with(|b| {
        b.launchers(LauncherDirs {
            setup_skills: Some(folder.path().join("skills")),
            ..Default::default()
        })
    })
    .await;
    let project_id = f.project().await;
    let selected = check();
    let job = save(&f, project_id, 0, selected.clone()).await;
    stop(&f, job).await;
    for (check_id, request) in [
        (None, "bad\0request".to_owned()),
        (None, "\u{000b}".to_owned()),
        (None, "é".repeat(8193)),
        (None, " ".repeat(16_385)),
        // Valid raw bytes, but selected-check context exceeds the draft limit.
        (Some(selected.check_id), "é".repeat(8192)),
    ] {
        let receipt = f
            .hub
            .submit(
                &Caller::local_owner(),
                CommandEnvelope::new(Command::ChecksAsk {
                    project_id,
                    check_id,
                    request,
                }),
            )
            .await;
        assert_eq!(receipt.error.unwrap().code, ErrorCode::InvalidRequest);
        assert!(f.hub.read(|r| r.list::<Task>("task")).unwrap().is_empty());
    }
    f.close().await;
}

async fn grants(f: &Fixture, caller: &mut Caller, tasks: bool, library_write: bool) {
    let Principal::Device { device_id } = caller.principal else {
        panic!("Expected a phone")
    };
    let device: crate::identity::Device = f.hub.read(|r| r.require("device", device_id)).unwrap();
    let mut updated = device.record.grants.clone();
    updated.tasks = tasks;
    updated.library_write = library_write;
    accepted(
        f,
        Command::SetDeviceGrants {
            device_id,
            expected: Some(device.record.grants),
            grants: updated.clone(),
        },
    )
    .await;
    caller.grants = updated;
}

#[tokio::test]
async fn checks_enforce_tasks_for_reading_and_both_grants_for_asking() {
    let f = Fixture::new().await;
    let project_id = f.project().await;
    let mut phone = f.device().await;
    for (tasks, write) in [(false, false), (false, true), (true, false)] {
        grants(&f, &mut phone, tasks, write).await;
        let receipt = f
            .hub
            .submit(
                &phone,
                CommandEnvelope::new(Command::ChecksAsk {
                    project_id,
                    check_id: None,
                    request: String::new(),
                }),
            )
            .await;
        assert_eq!(receipt.error.unwrap().code, ErrorCode::PermissionDenied);
        let query = f
            .hub
            .query(
                &phone,
                Query::ChecksList {
                    project_id: Some(project_id),
                },
            )
            .await;
        if tasks {
            assert!(query.is_ok());
        } else {
            assert_eq!(query.unwrap_err().code, ErrorCode::PermissionDenied);
        }
        let receipt = f
            .hub
            .submit(
                &phone,
                CommandEnvelope::new(Command::ChecksSave {
                    project_id,
                    base_version: 0,
                    edit: CheckEdit::Save { check: check() },
                }),
            )
            .await;
        if write {
            assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
            let Some(CommandResult::ChecksSaved {
                job_id: Some(id), ..
            }) = receipt.result
            else {
                panic!("Expected a saved check")
            };
            stop(&f, id).await;
        } else {
            assert_eq!(receipt.error.unwrap().code, ErrorCode::PermissionDenied);
        }
    }
    assert!(f.hub.read(|r| r.list::<Task>("task")).unwrap().is_empty());
    f.close().await;
}

#[tokio::test]
async fn saving_and_querying_uses_computed_facts_and_tracks_changed_inputs() {
    let f = Fixture::new().await;
    let project_id = f.project().await;
    std::fs::write(f.project_dir().join("check.sh"), "first version").unwrap();
    let mut check = check();
    check.about = "Uses the internet and runs only when asked.".into();
    check.inputs = vec!["check.sh".into()];
    let job_id = save(&f, project_id, 0, check.clone()).await;
    let view = list(&f, project_id).await;
    assert_eq!(view.approved.version, 1);
    assert_eq!(view.approved.checks, vec![check.clone()]);
    assert_eq!(
        view.checks[0].facts.when,
        "After every task that changes files"
    );
    assert_eq!(view.checks[0].facts.internet, "Does not use the internet");
    assert_eq!(view.checks[0].facts.time_limit, "Time limit: 10 minutes");
    assert!(view.checks[0].last_result.is_none());
    assert!(view.checks[0].inputs_changed.is_empty());
    std::fs::write(f.project_dir().join("check.sh"), "changed version").unwrap();
    assert_eq!(
        list(&f, project_id).await.checks[0].inputs_changed,
        vec!["check.sh"]
    );
    stop(&f, job_id).await;
    let receipt = f
        .hub
        .submit(
            &Caller::local_owner(),
            CommandEnvelope::new(Command::ChecksSave {
                project_id,
                base_version: 0,
                edit: CheckEdit::SetEnabled {
                    check_id: check.check_id,
                    enabled: false,
                },
            }),
        )
        .await;
    assert_eq!(receipt.error.unwrap().code, ErrorCode::StaleRevision);
    assert_eq!(list(&f, project_id).await.approved.version, 1);
    let QueryData::ChecksHistory { versions } = f
        .hub
        .query(&Caller::local_owner(), Query::ChecksHistory { project_id })
        .await
        .unwrap()
    else {
        panic!("Expected history")
    };
    assert_eq!(versions.len(), 1);
    assert_eq!(versions[0].version, 1);
    f.close().await;
}

#[tokio::test]
async fn a_persons_saved_check_is_approved_at_once_while_the_project_is_working() {
    let f = Fixture::new().await;
    let project_id = f.project().await;
    let (task_id, _) = f
        .task(
            project_id,
            TaskPlacement::StartNow,
            &Caller::local_owner(),
            false,
        )
        .await;
    let run_id = run_started(&f, task_id).await;
    let before: Project = f.hub.read(|r| r.require("project", project_id)).unwrap();
    assert_eq!(before.active_writer, Some(run_id));
    let mut check = check();
    let first_job = save(&f, project_id, 0, check.clone()).await;
    check.timeout_secs = 900;
    let second_job = save(&f, project_id, 1, check.clone()).await;
    assert_ne!(first_job, second_job);
    let view = list(&f, project_id).await;
    assert_eq!(view.approved.version, 2);
    assert_eq!(view.approved.checks, vec![check.clone()]);
    for (job_id, version, timeout) in [(first_job, 1, 600), (second_job, 2, 900)] {
        let QueryData::CheckJob { job } = f
            .hub
            .query(&Caller::local_owner(), Query::CheckJob { job_id })
            .await
            .unwrap()
        else {
            panic!("Expected a queued proof job")
        };
        assert_eq!(job.state, CheckJobState::Pending);
        assert!(job.process.is_none());
        assert_eq!(job.checks_version, version);
        assert_eq!(job.checks[0].timeout_secs, timeout);
    }
    let after: Project = f.hub.read(|r| r.require("project", project_id)).unwrap();
    assert_eq!(after.active_writer, Some(run_id));
    let receipt = f
        .hub
        .submit(
            &Caller::local_owner(),
            CommandEnvelope::new(Command::ChecksRunNow {
                project_id,
                check_ids: vec![check.check_id],
            }),
        )
        .await;
    assert_eq!(receipt.error.unwrap().code, ErrorCode::Conflict);
    stop(&f, first_job).await;
    stop(&f, second_job).await;
    f.close().await;
}

#[tokio::test]
async fn pending_jobs_are_busy_and_stopping_before_spawn_finishes_them_idempotently() {
    let f = Fixture::new().await;
    let project_id = f.project().await;
    let check = check();
    let saved_job = save(&f, project_id, 0, check.clone()).await;
    let receipt = f
        .hub
        .submit(
            &Caller::local_owner(),
            CommandEnvelope::new(Command::ChecksRunNow {
                project_id,
                check_ids: vec![check.check_id],
            }),
        )
        .await;
    assert_eq!(receipt.error.unwrap().code, ErrorCode::Conflict);
    let stopped = stop(&f, saved_job).await;
    assert_eq!(stopped.state, CheckJobState::Finished);
    assert!(stopped.process.is_none());
    assert!(stopped.finished_at.is_some());
    assert_eq!(stopped.results[0].outcome, CheckOutcome::Stopped);
    assert_eq!(stop(&f, saved_job).await, stopped);
    let CommandResult::CheckJob { job } = command(
        &f,
        Command::ChecksRunNow {
            project_id,
            check_ids: vec![],
        },
    )
    .await
    else {
        panic!("Expected a pending run-now job")
    };
    assert_eq!(job.state, CheckJobState::Pending);
    assert!(job.process.is_none());
    assert_eq!(job.checks, vec![check.clone()]);
    assert_eq!(job.checks_version, 1);
    let QueryData::CheckJob { job: stored } = f
        .hub
        .query(&Caller::local_owner(), Query::CheckJob { job_id: job.id })
        .await
        .unwrap()
    else {
        panic!("Expected a job")
    };
    assert_eq!(stored, job);
    stop(&f, job.id).await;
    f.hub
        .write(|tx| {
            let mut project: Project = tx.read().require("project", project_id)?;
            project.active_writer = Some(RunId::new());
            tx.put("project", project_id, &project)
        })
        .unwrap();
    let receipt = f
        .hub
        .submit(
            &Caller::local_owner(),
            CommandEnvelope::new(Command::ChecksRunNow {
                project_id,
                check_ids: vec![check.check_id],
            }),
        )
        .await;
    assert_eq!(receipt.error.unwrap().code, ErrorCode::Conflict);
    assert_eq!(f.harness.starts.load(Ordering::SeqCst), 0);
    f.close().await;
}

#[tokio::test]
async fn draft_item_positions_map_to_check_ids_and_stale_approvals_are_refused() {
    let f = Fixture::new().await;
    let project_id = f.project().await;
    let original = check();
    let saved_job = save(&f, project_id, 0, original.clone()).await;
    stop(&f, saved_job).await;
    let mut first = check();
    first.name = "App builds correctly".into();
    let mut second = check();
    second.name = "App types match".into();
    let draft = f
        .hub
        .write(|tx| {
            let draft = tx.propose_checks(
                project_id,
                1,
                None,
                Some("Add useful checks".into()),
                vec![
                    CheckDraftItem::Add {
                        check: first.clone(),
                        note: "Make sure the app builds.".into(),
                    },
                    CheckDraftItem::Add {
                        check: second.clone(),
                        note: "Make sure the app fits together.".into(),
                    },
                ],
                &f.project_dir(),
            )?;
            tx.finish_checks_proof(draft.id, vec![proof(&first), proof(&second)])
        })
        .unwrap();
    for keep_items in [vec![2], vec![0, 0]] {
        let receipt = f
            .hub
            .submit(
                &Caller::local_owner(),
                CommandEnvelope::new(Command::ChecksApplyDraft {
                    draft_id: draft.id,
                    base_version: 1,
                    keep_items,
                }),
            )
            .await;
        assert_eq!(receipt.error.unwrap().code, ErrorCode::InvalidRequest);
        assert_eq!(list(&f, project_id).await.approved.version, 1);
    }
    let CommandResult::ChecksSaved { approved, .. } = command(
        &f,
        Command::ChecksApplyDraft {
            draft_id: draft.id,
            base_version: 1,
            keep_items: vec![1],
        },
    )
    .await
    else {
        panic!("Expected approved checks")
    };
    assert_eq!(approved.version, 2);
    assert_eq!(
        approved
            .checks
            .iter()
            .map(|c| c.check_id)
            .collect::<Vec<_>>(),
        vec![original.check_id, second.check_id]
    );
    let stale = f
        .hub
        .write(|tx| {
            let draft = tx.propose_checks(
                project_id,
                2,
                None,
                None,
                vec![CheckDraftItem::Remove {
                    check_id: original.check_id,
                    reason: "No longer needed.".into(),
                }],
                &f.project_dir(),
            )?;
            tx.finish_checks_proof(draft.id, vec![])
        })
        .unwrap();
    command(
        &f,
        Command::ChecksSave {
            project_id,
            base_version: 2,
            edit: CheckEdit::SetEnabled {
                check_id: second.check_id,
                enabled: false,
            },
        },
    )
    .await;
    let receipt = f
        .hub
        .submit(
            &Caller::local_owner(),
            CommandEnvelope::new(Command::ChecksApplyDraft {
                draft_id: stale.id,
                base_version: 2,
                keep_items: vec![0],
            }),
        )
        .await;
    assert_eq!(receipt.error.unwrap().code, ErrorCode::StaleRevision);
    let view = list(&f, project_id).await;
    assert_eq!(view.approved.version, 3);
    assert!(
        view.approved
            .checks
            .iter()
            .any(|c| c.check_id == original.check_id)
    );
    f.close().await;
}

#[tokio::test]
async fn checks_view_recovers_standalone_uncertain_writer_before_newer_unfinished_jobs() {
    let f = Fixture::new().await;
    let project_id = f.project().await;
    let job_id = save(&f, project_id, 0, check()).await;
    let (uncertain, newer) = f
        .hub
        .write(|tx| {
            let mut uncertain: CheckJob = tx.read().require(store::checks::JOBS, job_id)?;
            uncertain.state = CheckJobState::Uncertain;
            assert!(uncertain.task_id.is_none());
            assert!(uncertain.run_id.is_none());
            tx.put(store::checks::JOBS, job_id, &uncertain)?;
            let mut newer = uncertain.clone();
            newer.id = CheckJobId::new();
            newer.state = CheckJobState::Pending;
            newer.created_at = Timestamp(uncertain.created_at.0 + 1);
            tx.put(store::checks::JOBS, newer.id, &newer)?;
            let mut project: Project = tx.read().require("project", project_id)?;
            project.active_writer = Some(crate::check_execution::job_writer(job_id));
            tx.put("project", project_id, &project)?;
            Ok((uncertain, newer))
        })
        .unwrap();
    assert_eq!(
        serde_json::to_value(list(&f, project_id).await).unwrap()["unfinished_job"],
        serde_json::to_value(&uncertain).unwrap()
    );
    // An after-run job can retain its original run's writer identity instead.
    f.hub
        .write(|tx| {
            let mut after_run = uncertain.clone();
            after_run.purpose = CheckJobPurpose::AfterRun;
            after_run.run_id = Some(RunId::new());
            after_run.state = CheckJobState::Cancelling;
            tx.put(store::checks::JOBS, job_id, &after_run)?;
            let mut project: Project = tx.read().require("project", project_id)?;
            project.active_writer = after_run.run_id;
            tx.put("project", project_id, &project)
        })
        .unwrap();
    assert_eq!(
        serde_json::to_value(list(&f, project_id).await).unwrap()["unfinished_job"]["id"],
        serde_json::to_value(job_id).unwrap()
    );
    f.hub
        .write(|tx| {
            let mut project: Project = tx.read().require("project", project_id)?;
            project.active_writer = None;
            tx.put("project", project_id, &project)
        })
        .unwrap();
    assert_eq!(
        serde_json::to_value(list(&f, project_id).await).unwrap()["unfinished_job"],
        serde_json::to_value(&newer).unwrap()
    );
    let mut proof = newer.clone();
    proof.state = CheckJobState::Running;
    proof.purpose = CheckJobPurpose::DraftProof {
        draft_id: CheckDraftId::new(),
    };
    f.hub
        .write(|tx| tx.put(store::checks::JOBS, proof.id, &proof))
        .unwrap();
    assert_eq!(
        serde_json::to_value(list(&f, project_id).await).unwrap()["unfinished_job"],
        serde_json::to_value(&proof).unwrap()
    );
    f.hub
        .write(|tx| {
            for mut job in [uncertain.clone(), proof.clone()] {
                job.state = CheckJobState::Finished;
                tx.put(store::checks::JOBS, job.id, &job)?;
            }
            Ok(())
        })
        .unwrap();
    assert!(
        serde_json::to_value(list(&f, project_id).await)
            .unwrap()
            .get("unfinished_job")
            .is_none()
    );
    f.close().await;
}

#[tokio::test]
async fn resolved_drafts_are_refused_before_service_version_comparison() {
    let f = Fixture::new().await;
    let project_id = f.project().await;
    for applied in [true, false] {
        let definition = check();
        let base = list(&f, project_id).await.approved.version;
        let draft = f
            .hub
            .write(|tx| {
                let draft = tx.propose_checks(
                    project_id,
                    base,
                    None,
                    None,
                    vec![CheckDraftItem::Add {
                        check: definition.clone(),
                        note: "Check the app.".into(),
                    }],
                    &f.project_dir(),
                )?;
                tx.finish_checks_proof(draft.id, vec![proof(&definition)])
            })
            .unwrap();
        if applied {
            command(
                &f,
                Command::ChecksApplyDraft {
                    draft_id: draft.id,
                    base_version: base,
                    keep_items: vec![0],
                },
            )
            .await;
        } else {
            command(&f, Command::ChecksDiscardDraft { draft_id: draft.id }).await;
            let job = save(&f, project_id, base, definition).await;
            stop(&f, job).await;
        }
        for base_version in [base, base + 1, base + 10] {
            let receipt = f
                .hub
                .submit(
                    &Caller::local_owner(),
                    CommandEnvelope::new(Command::ChecksApplyDraft {
                        draft_id: draft.id,
                        base_version,
                        keep_items: vec![0],
                    }),
                )
                .await;
            assert_eq!(receipt.error.unwrap().code, ErrorCode::AlreadyResolved);
        }
        let stored: ChecksDraft = f
            .hub
            .read(|r| r.require(store::checks::DRAFTS, draft.id))
            .unwrap();
        assert_eq!(
            stored.state,
            if applied {
                ChecksDraftState::Applied
            } else {
                ChecksDraftState::Discarded
            }
        );
        assert_eq!(list(&f, project_id).await.approved.version, base + 1);
    }
    f.close().await;
}
