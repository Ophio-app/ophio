// SPDX-License-Identifier: Apache-2.0
use super::*;
use crate::{Retention, Store};
use protocol::RunId;

fn check(name: &str) -> Check {
    Check {
        check_id: CheckId::new(),
        name: name.into(),
        about: "Proves that the app still works.".into(),
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
fn result(check: &Check, outcome: CheckOutcome) -> CheckResult {
    CheckResult {
        check_id: check.check_id,
        name: check.name.clone(),
        outcome,
        duration_secs: 1.5,
        exit_code: Some(0),
        output: "passed".into(),
        inputs_changed: vec![],
    }
}
fn add(check: &Check) -> CheckDraftItem {
    CheckDraftItem::Add {
        check: check.clone(),
        note: "Make sure the app works.".into(),
    }
}
fn database() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("db.sqlite"), Retention::default()).unwrap();
    (dir, store)
}
fn error_code<T: std::fmt::Debug>(result: Result<T>, code: ErrorCode) {
    match result.unwrap_err() {
        crate::Error::Rule(error) => assert_eq!(error.code, code),
        error => panic!("wrong error: {error:?}"),
    }
}

#[test]
fn proposals_are_drafts_and_replace_only_the_same_tasks_open_draft() {
    let (dir, mut db) = database();
    let project = ProjectId::new();
    let task = TaskId::new();
    let first_check = check("App tests pass");
    let first = db
        .write(Timestamp(1), |tx| {
            tx.propose_checks(
                project,
                0,
                Some(task),
                Some("Set up checks".into()),
                vec![add(&first_check)],
                dir.path(),
            )
        })
        .unwrap()
        .value;
    db.read(|r| {
        assert!(r.project_checks(project)?.is_none());
        assert_eq!(r.task_checks_draft(task)?.unwrap(), first);
        assert!(r.checks_history(project)?.is_empty());
        Ok(())
    })
    .unwrap();
    let second_check = check("App builds correctly");
    let second = db
        .write(Timestamp(2), |tx| {
            tx.propose_checks(
                project,
                0,
                Some(task),
                None,
                vec![add(&second_check)],
                dir.path(),
            )
        })
        .unwrap()
        .value;
    assert_ne!(first.id, second.id);
    db.read(|r| {
        assert_eq!(
            r.checks_draft(first.id)?.unwrap().state,
            ChecksDraftState::Discarded
        );
        assert_eq!(r.task_checks_draft(task)?.unwrap().id, second.id);
        assert_eq!(
            r.checks_drafts(project)?
                .iter()
                .filter(|draft| open_draft(draft.state))
                .count(),
            1
        );
        Ok(())
    })
    .unwrap();
    error_code(
        db.write(Timestamp(3), |tx| {
            tx.finish_checks_proof(first.id, vec![result(&first_check, CheckOutcome::Passed)])
        })
        .map(|commit| commit.value),
        ErrorCode::AlreadyResolved,
    );
    let other = db
        .write(Timestamp(4), |tx| {
            tx.propose_checks(
                project,
                0,
                Some(TaskId::new()),
                None,
                vec![add(&first_check)],
                dir.path(),
            )
        })
        .unwrap()
        .value;
    assert_eq!(other.state, ChecksDraftState::Proving);
    assert_eq!(
        db.read(|r| r.checks_drafts(project))
            .unwrap()
            .iter()
            .filter(|draft| open_draft(draft.state))
            .count(),
        2
    );
}

#[test]
fn proof_is_required_and_partial_apply_is_a_single_attributed_version() {
    let (dir, mut db) = database();
    let project = ProjectId::new();
    let one = check("App tests pass");
    let two = check("App builds correctly");
    let draft = db
        .write(Timestamp(1), |tx| {
            tx.propose_checks(
                project,
                0,
                None,
                None,
                vec![add(&one), add(&two)],
                dir.path(),
            )
        })
        .unwrap()
        .value;
    error_code(
        db.write(Timestamp(2), |tx| {
            tx.apply_checks_draft(draft.id, &[one.check_id], None, dir.path())
        })
        .map(|commit| commit.value),
        ErrorCode::Conflict,
    );
    error_code(
        db.write(Timestamp(3), |tx| {
            tx.finish_checks_proof(draft.id, vec![result(&one, CheckOutcome::Passed)])
        })
        .map(|commit| commit.value),
        ErrorCode::InvalidRequest,
    );
    db.write(Timestamp(4), |tx| {
        tx.finish_checks_proof(
            draft.id,
            vec![
                result(&one, CheckOutcome::Failed),
                result(&two, CheckOutcome::Passed),
            ],
        )
    })
    .unwrap();
    error_code(
        db.write(Timestamp(5), |tx| {
            tx.apply_checks_draft(draft.id, &[CheckId::new()], None, dir.path())
        })
        .map(|commit| commit.value),
        ErrorCode::InvalidRequest,
    );
    error_code(
        db.write(Timestamp(5), |tx| {
            tx.apply_checks_draft(draft.id, &[], None, dir.path())
        })
        .map(|commit| commit.value),
        ErrorCode::InvalidRequest,
    );
    let device_id = Some(DeviceId::new());
    let approved = db
        .write(Timestamp(6), |tx| {
            tx.apply_checks_draft(draft.id, &[one.check_id], device_id, dir.path())
        })
        .unwrap()
        .value;
    assert_eq!(approved.version, 1);
    assert_eq!(approved.checks, vec![one.clone()]);
    assert_eq!(
        approved.changed_by,
        ChecksChangedBy::Draft {
            draft_id: draft.id,
            device_id
        }
    );
    assert_eq!(approved.changed_at, Timestamp(6));
    assert_eq!(
        db.read(|r| r.checks_draft(draft.id))
            .unwrap()
            .unwrap()
            .state,
        ChecksDraftState::Applied
    );
    error_code(
        db.write(Timestamp(7), |tx| {
            tx.apply_checks_draft(draft.id, &[one.check_id], device_id, dir.path())
        })
        .map(|commit| commit.value),
        ErrorCode::AlreadyResolved,
    );
}

#[test]
fn stale_person_edits_and_stale_drafts_never_overwrite_newer_checks() {
    let (dir, mut db) = database();
    let project = ProjectId::new();
    let one = check("App tests pass");
    let draft = db
        .write(Timestamp(1), |tx| {
            tx.propose_checks(project, 0, None, None, vec![add(&one)], dir.path())
        })
        .unwrap()
        .value;
    db.write(Timestamp(2), |tx| {
        tx.finish_checks_proof(draft.id, vec![result(&one, CheckOutcome::Passed)])
    })
    .unwrap();
    let person_check = check("App builds correctly");
    let approved = db
        .write(Timestamp(3), |tx| {
            tx.change_checks(
                project,
                0,
                CheckEdit::Save {
                    check: person_check.clone(),
                },
                None,
                dir.path(),
            )
        })
        .unwrap()
        .value;
    assert_eq!(
        db.read(|r| r.checks_draft(draft.id))
            .unwrap()
            .unwrap()
            .state,
        ChecksDraftState::Stale
    );
    error_code(
        db.write(Timestamp(4), |tx| {
            tx.apply_checks_draft(draft.id, &[one.check_id], None, dir.path())
        })
        .map(|commit| commit.value),
        ErrorCode::StaleRevision,
    );
    error_code(
        db.write(Timestamp(4), |tx| {
            tx.change_checks(
                project,
                0,
                CheckEdit::Delete {
                    check_id: person_check.check_id,
                },
                None,
                dir.path(),
            )
        })
        .map(|commit| commit.value),
        ErrorCode::StaleRevision,
    );
    error_code(
        db.write(Timestamp(4), |tx| {
            tx.propose_checks(project, 0, None, None, vec![add(&one)], dir.path())
        })
        .map(|commit| commit.value),
        ErrorCode::StaleRevision,
    );
    assert_eq!(
        db.read(|r| r.project_checks(project)).unwrap(),
        Some(approved)
    );
    assert_eq!(
        db.read(|r| r.checks_draft(draft.id))
            .unwrap()
            .unwrap()
            .state,
        ChecksDraftState::Stale
    );
    let discarded = db
        .write(Timestamp(5), |tx| tx.discard_checks_draft(draft.id))
        .unwrap()
        .value;
    assert_eq!(discarded.state, ChecksDraftState::Discarded);
}

#[test]
fn history_keeps_twenty_versions_and_undo_appends_instead_of_rewinding() {
    let (dir, mut db) = database();
    let project = ProjectId::new();
    let one = check("App tests pass");
    let first = db
        .write(Timestamp(1), |tx| {
            tx.change_checks(
                project,
                0,
                CheckEdit::Save { check: one.clone() },
                None,
                dir.path(),
            )
        })
        .unwrap()
        .value;
    let empty = db
        .write(Timestamp(2), |tx| {
            tx.change_checks(project, 1, CheckEdit::Undo, None, dir.path())
        })
        .unwrap()
        .value;
    assert_eq!(empty.version, 2);
    assert!(empty.checks.is_empty());
    let restored = db
        .write(Timestamp(3), |tx| {
            tx.change_checks(project, 2, CheckEdit::Undo, None, dir.path())
        })
        .unwrap()
        .value;
    assert_eq!(restored.version, 3);
    assert_eq!(restored.checks, first.checks);
    for version in 3..25 {
        db.write(Timestamp(version as i64 + 1), |tx| {
            tx.change_checks(
                project,
                version,
                CheckEdit::SetEnabled {
                    check_id: one.check_id,
                    enabled: version % 2 == 0,
                },
                None,
                dir.path(),
            )
        })
        .unwrap();
    }
    let history = db.read(|r| r.checks_history(project)).unwrap();
    assert_eq!(history.len(), 20);
    assert_eq!(history.first().unwrap().version, 25);
    assert_eq!(history.last().unwrap().version, 6);
    let previous = history[1].clone();
    let device_id = Some(DeviceId::new());
    let undone = db
        .write(Timestamp(26), |tx| {
            tx.change_checks(project, 25, CheckEdit::Undo, device_id, dir.path())
        })
        .unwrap()
        .value;
    assert_eq!(undone.version, 26);
    assert_eq!(undone.checks, previous.checks);
    assert_eq!(undone.input_fingerprints, previous.input_fingerprints);
    assert_eq!(undone.changed_by, ChecksChangedBy::Person { device_id });
    assert_eq!(db.read(|r| r.checks_history(project)).unwrap().len(), 20);
    let path = dir.path().join("db.sqlite");
    drop(db);
    let mut db = Store::open(path, Retention::default()).unwrap();
    assert_eq!(
        db.read(|r| r.project_checks(project)).unwrap(),
        Some(undone)
    );
    assert_eq!(db.read(|r| r.checks_history(project)).unwrap().len(), 20);
}

#[test]
fn fingerprints_are_taken_at_approval_and_only_explicit_inputs_are_accepted() {
    let (dir, mut db) = database();
    let project = ProjectId::new();
    std::fs::write(dir.path().join("test.sh"), "before").unwrap();
    let mut one = check("App tests pass");
    one.inputs = vec!["test.sh".into(), "missing.sh".into()];
    let draft = db
        .write(Timestamp(1), |tx| {
            tx.propose_checks(project, 0, None, None, vec![add(&one)], dir.path())
        })
        .unwrap()
        .value;
    db.write(Timestamp(2), |tx| {
        tx.finish_checks_proof(draft.id, vec![result(&one, CheckOutcome::Passed)])
    })
    .unwrap();
    std::fs::write(dir.path().join("test.sh"), "abc").unwrap();
    let approved = db
        .write(Timestamp(3), |tx| {
            tx.apply_checks_draft(draft.id, &[one.check_id], None, dir.path())
        })
        .unwrap()
        .value;
    assert_eq!(
        approved.input_fingerprints[0].sha256.as_deref(),
        Some("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
    );
    assert!(approved.input_fingerprints[1].sha256.is_none());
    assert!(
        changed_inputs(dir.path(), &approved, &one)
            .unwrap()
            .is_empty()
    );
    std::fs::write(dir.path().join("test.sh"), "after").unwrap();
    std::fs::write(dir.path().join("missing.sh"), "new").unwrap();
    assert_eq!(
        changed_inputs(dir.path(), &approved, &one).unwrap(),
        vec!["test.sh", "missing.sh"]
    );
    let disabled = db
        .write(Timestamp(4), |tx| {
            tx.change_checks(
                project,
                1,
                CheckEdit::SetEnabled {
                    check_id: one.check_id,
                    enabled: false,
                },
                None,
                dir.path(),
            )
        })
        .unwrap()
        .value;
    assert_eq!(disabled.input_fingerprints, approved.input_fingerprints);
    let other = check("App builds correctly");
    let other_saved = db
        .write(Timestamp(5), |tx| {
            tx.change_checks(
                project,
                2,
                CheckEdit::Save {
                    check: other.clone(),
                },
                None,
                dir.path(),
            )
        })
        .unwrap()
        .value;
    assert_eq!(other_saved.input_fingerprints, approved.input_fingerprints);
    let kept = db
        .write(Timestamp(6), |tx| {
            tx.change_checks(
                project,
                3,
                CheckEdit::KeepInputs {
                    check_id: one.check_id,
                },
                None,
                dir.path(),
            )
        })
        .unwrap()
        .value;
    assert!(changed_inputs(dir.path(), &kept, &one).unwrap().is_empty());
    let undone = db
        .write(Timestamp(7), |tx| {
            tx.change_checks(project, 4, CheckEdit::Undo, None, dir.path())
        })
        .unwrap()
        .value;
    assert_eq!(undone.input_fingerprints, approved.input_fingerprints);
    std::fs::remove_file(dir.path().join("test.sh")).unwrap();
    assert!(
        changed_inputs(dir.path(), &undone, &one)
            .unwrap()
            .contains(&"test.sh".to_string())
    );
}

#[test]
fn invalid_definitions_and_unsafe_inputs_are_rejected_without_writes() {
    let (dir, mut db) = database();
    let project = ProjectId::new();
    let valid = check("App tests pass");
    let mut invalid_checks = Vec::new();
    let mut value = valid.clone();
    value.name.clear();
    invalid_checks.push(value);
    let mut value = valid.clone();
    value.run = " ".into();
    invalid_checks.push(value);
    let mut value = valid.clone();
    value.paths = vec!["[".into()];
    invalid_checks.push(value);
    let mut value = valid.clone();
    value.timeout_secs = 9;
    invalid_checks.push(value);
    let mut value = valid.clone();
    value.timeout_secs = 3_601;
    invalid_checks.push(value);
    let mut value = valid.clone();
    value.inputs = vec!["../outside".into()];
    invalid_checks.push(value);
    let mut value = valid.clone();
    value.inputs = vec!["/etc/passwd".into()];
    invalid_checks.push(value);
    let mut value = valid.clone();
    value.inputs = vec!["C:\\outside".into()];
    invalid_checks.push(value);
    let mut value = valid.clone();
    value.inputs = vec!["script:stream".into()];
    invalid_checks.push(value);
    let mut value = valid.clone();
    value.inputs = vec!["one".into(), "one".into()];
    invalid_checks.push(value);
    let mut value = valid.clone();
    value.inputs = vec!["subdir".into()];
    invalid_checks.push(value);
    std::fs::create_dir(dir.path().join("subdir")).unwrap();
    for value in invalid_checks {
        assert!(
            db.write(Timestamp(1), |tx| tx.propose_checks(
                project,
                0,
                None,
                None,
                vec![add(&value)],
                dir.path()
            ))
            .is_err(),
            "accepted {value:?}"
        );
    }
    db.read(|r| {
        assert!(r.project_checks(project)?.is_none());
        assert!(r.checks_drafts(project)?.is_empty());
        Ok(())
    })
    .unwrap();
    error_code(
        db.write(Timestamp(2), |tx| {
            tx.propose_checks(
                project,
                0,
                None,
                None,
                vec![add(&valid), add(&valid)],
                dir.path(),
            )
        })
        .map(|commit| commit.value),
        ErrorCode::InvalidRequest,
    );
}

#[cfg(unix)]
#[test]
fn fingerprinting_confines_symlinks_and_rejects_special_and_large_files() {
    use std::os::unix::fs::symlink;
    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("secret"), "must not read").unwrap();
    symlink(outside.path().join("secret"), dir.path().join("escape")).unwrap();
    symlink(outside.path(), dir.path().join("escape_dir")).unwrap();
    let mut one = check("App tests pass");
    for path in ["escape", "escape_dir/secret"] {
        one.inputs = vec![path.into()];
        error_code(
            fingerprint_check(dir.path(), &one),
            ErrorCode::PathNotAllowed,
        );
    }
    std::fs::write(dir.path().join("inside"), "abc").unwrap();
    symlink("inside", dir.path().join("safe")).unwrap();
    one.inputs = vec!["safe".into()];
    assert!(
        fingerprint_check(dir.path(), &one).unwrap()[0]
            .sha256
            .is_some()
    );
    let file = std::fs::File::create(dir.path().join("large")).unwrap();
    file.set_len(INPUT_BYTE_LIMIT + 1).unwrap();
    one.inputs = vec!["large".into()];
    error_code(
        fingerprint_check(dir.path(), &one),
        ErrorCode::LimitExceeded,
    );
    assert!(
        std::process::Command::new("mkfifo")
            .arg(dir.path().join("pipe"))
            .status()
            .unwrap()
            .success()
    );
    one.inputs = vec!["pipe".into()];
    error_code(
        fingerprint_check(dir.path(), &one),
        ErrorCode::PathNotAllowed,
    );
}

#[test]
fn proof_changes_and_removals_apply_only_selected_items_and_discard_is_final() {
    let (dir, mut db) = database();
    let project = ProjectId::new();
    let one = check("App tests pass");
    let two = check("App builds correctly");
    db.write(Timestamp(1), |tx| {
        tx.change_checks(
            project,
            0,
            CheckEdit::Save { check: one.clone() },
            None,
            dir.path(),
        )
    })
    .unwrap();
    db.write(Timestamp(2), |tx| {
        tx.change_checks(
            project,
            1,
            CheckEdit::Save { check: two.clone() },
            None,
            dir.path(),
        )
    })
    .unwrap();
    let mut changed = one.clone();
    changed.network = true;
    let draft = db
        .write(Timestamp(3), |tx| {
            tx.propose_checks(
                project,
                2,
                None,
                None,
                vec![
                    CheckDraftItem::Change {
                        check: changed.clone(),
                        note: "Use the internet for this check.".into(),
                    },
                    CheckDraftItem::Remove {
                        check_id: two.check_id,
                        reason: "The first check covers this too.".into(),
                    },
                ],
                dir.path(),
            )
        })
        .unwrap()
        .value;
    db.write(Timestamp(4), |tx| {
        tx.finish_checks_proof(draft.id, vec![result(&changed, CheckOutcome::Passed)])
    })
    .unwrap();
    let approved = db
        .write(Timestamp(5), |tx| {
            tx.apply_checks_draft(draft.id, &[two.check_id], None, dir.path())
        })
        .unwrap()
        .value;
    assert_eq!(approved.checks, vec![one.clone()]);
    assert_eq!(approved.version, 3);
    let draft = db
        .write(Timestamp(6), |tx| {
            tx.propose_checks(project, 3, None, None, vec![add(&two)], dir.path())
        })
        .unwrap()
        .value;
    db.write(Timestamp(7), |tx| tx.discard_checks_draft(draft.id))
        .unwrap();
    error_code(
        db.write(Timestamp(8), |tx| {
            tx.apply_checks_draft(draft.id, &[two.check_id], None, dir.path())
        })
        .map(|commit| commit.value),
        ErrorCode::AlreadyResolved,
    );
    assert_eq!(
        db.read(|r| r.project_checks(project)).unwrap().unwrap(),
        approved
    );
}

#[test]
fn an_error_rolls_back_approved_documents_history_and_draft_state_together() {
    let (dir, mut db) = database();
    let project = ProjectId::new();
    let one = check("App tests pass");
    let draft = db
        .write(Timestamp(1), |tx| {
            tx.propose_checks(project, 0, None, None, vec![add(&one)], dir.path())
        })
        .unwrap()
        .value;
    db.write(Timestamp(2), |tx| {
        tx.finish_checks_proof(draft.id, vec![result(&one, CheckOutcome::Passed)])
    })
    .unwrap();
    let rolled_back: Result<crate::Commit<()>> = db.write(Timestamp(3), |tx| {
        tx.apply_checks_draft(draft.id, &[one.check_id], None, dir.path())?;
        Err(invalid("Fail the transaction after applying the draft."))
    });
    assert!(rolled_back.is_err());
    db.read(|r| {
        assert!(r.project_checks(project)?.is_none());
        assert!(r.checks_history(project)?.is_empty());
        assert_eq!(
            r.checks_draft(draft.id)?.unwrap().state,
            ChecksDraftState::Ready
        );
        Ok(())
    })
    .unwrap();
}

#[test]
fn older_project_documents_need_no_backfill_and_empty_checks_survive_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db.sqlite");
    let project = ProjectId::new();
    let connection = Connection::open(&path).unwrap();
    connection.execute_batch("CREATE TABLE documents (kind TEXT NOT NULL,id TEXT NOT NULL,json TEXT NOT NULL,PRIMARY KEY(kind,id)); CREATE TABLE commands (id TEXT PRIMARY KEY,json TEXT NOT NULL); CREATE TABLE journal (seq INTEGER PRIMARY KEY AUTOINCREMENT,at INTEGER NOT NULL,json TEXT NOT NULL); CREATE INDEX journal_at ON journal(at); PRAGMA user_version=1;").unwrap();
    let old_project = serde_json::json!({"project_id":project,"name":"Existing project","host_path":"/project","default_harness":null,"default_model":null,"default_effort":null,"active_writer":null,"vcs":null,"created_at":1});
    connection
        .execute(
            "INSERT INTO documents VALUES('project',?1,?2)",
            rusqlite::params![project.to_string(), old_project.to_string()],
        )
        .unwrap();
    drop(connection);
    let mut db = Store::open(&path, Retention::default()).unwrap();
    db.read(|r| {
        assert!(r.project_checks(project)?.is_none());
        assert!(r.checks_drafts(project)?.is_empty());
        assert!(r.pending_check_jobs()?.is_empty());
        assert_eq!(
            r.require::<protocol::model::Project>("project", project)?
                .project_id,
            project
        );
        Ok(())
    })
    .unwrap();
    db.write(Timestamp(2), |tx| {
        tx.change_checks(
            project,
            0,
            CheckEdit::Save {
                check: check("App tests pass"),
            },
            None,
            dir.path(),
        )
    })
    .unwrap();
    db.write(Timestamp(3), |tx| {
        tx.change_checks(project, 1, CheckEdit::Undo, None, dir.path())
    })
    .unwrap();
    drop(db);
    let mut db = Store::open(path, Retention::default()).unwrap();
    let approved = db.read(|r| r.project_checks(project)).unwrap().unwrap();
    assert_eq!(approved.version, 2);
    assert!(approved.checks.is_empty());
}

#[test]
fn pending_job_process_identity_and_frozen_definitions_survive_restart() {
    let (dir, mut db) = database();
    let project = ProjectId::new();
    let task = TaskId::new();
    let one = check("App tests pass");
    let mut job = CheckJob {
        id: CheckJobId::new(),
        project_id: project,
        task_id: Some(task),
        run_id: Some(RunId::new()),
        purpose: CheckJobPurpose::AfterRun,
        checks_version: 1,
        checks: vec![one.clone()],
        state: CheckJobState::Pending,
        process: None,
        created_at: Timestamp(1),
        started_at: None,
        finished_at: None,
        sandbox: None,
        results: vec![],
        message: None,
    };
    db.write(Timestamp(1), |tx| tx.put_check_job(&job)).unwrap();
    job.state = CheckJobState::Running;
    job.started_at = Some(Timestamp(2));
    job.process = Some(CheckProcessIdentity {
        pid: 123,
        start_time: "98765".into(),
        sandbox_unit: Some("check.service".into()),
    });
    job.sandbox = Some(CheckSandboxLevel::Contained);
    db.write(Timestamp(2), |tx| tx.put_check_job(&job)).unwrap();
    drop(db);
    let mut db = Store::open(dir.path().join("db.sqlite"), Retention::default()).unwrap();
    db.read(|r| {
        assert_eq!(r.pending_check_jobs()?, vec![job.clone()]);
        assert_eq!(r.task_check_jobs(task)?, vec![job.clone()]);
        assert_eq!(r.project_check_jobs(project)?, vec![job.clone()]);
        assert_eq!(r.check_job(job.id)?, Some(job.clone()));
        Ok(())
    })
    .unwrap();
    let mut replaced = job.clone();
    replaced.checks[0].run = "different".into();
    error_code(
        db.write(Timestamp(3), |tx| tx.put_check_job(&replaced))
            .map(|commit| commit.value),
        ErrorCode::InvalidRequest,
    );
    job.state = CheckJobState::Uncertain;
    job.message = Some("Ophio could not confirm the process stopped.".into());
    db.write(Timestamp(3), |tx| tx.put_check_job(&job)).unwrap();
    assert_eq!(
        db.read(|r| r.pending_check_jobs()).unwrap(),
        vec![job.clone()]
    );
    job.state = CheckJobState::Finished;
    job.finished_at = Some(Timestamp(4));
    job.results = vec![result(&one, CheckOutcome::Passed)];
    db.write(Timestamp(4), |tx| tx.put_check_job(&job)).unwrap();
    assert!(db.read(|r| r.pending_check_jobs()).unwrap().is_empty());
    assert_eq!(
        db.read(|r| r.task_check_jobs(task)).unwrap()[0]
            .results
            .len(),
        1
    );
}

#[test]
fn factual_descriptions_do_not_repeat_agent_claims_or_commands() {
    let mut before = check("App tests pass");
    before.about = "This never needs the internet.".into();
    before.run = "curl secret-command".into();
    let mut after = before.clone();
    after.network = true;
    after.timeout_secs = 120;
    after.paths = vec!["src/**".into()];
    after.typical_secs = Some(72.0);
    let facts = check_facts(&after);
    assert_eq!(facts.internet, "Uses the internet");
    assert_eq!(
        facts.typical_time.as_deref(),
        Some("Takes about 1 minute 12 seconds")
    );
    assert!(!facts.when.contains("curl"));
    let differences = check_differences(&before, &after);
    assert!(differences.contains(&"Now uses the internet".into()));
    assert!(
        differences
            .iter()
            .any(|line| line.contains("10 minutes to 2 minutes"))
    );
    assert!(!differences.iter().any(|line| line.contains("never needs")));
}

#[cfg(unix)]
#[test]
fn an_input_replaced_by_an_escaping_symlink_is_reported_without_being_read() {
    use std::os::unix::fs::symlink;
    let (dir, mut db) = database();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("secret"), "must not read").unwrap();
    std::fs::write(dir.path().join("test.sh"), "before").unwrap();
    let project = ProjectId::new();
    let mut one = check("App tests pass");
    one.inputs = vec!["test.sh".into()];
    let approved = db
        .write(Timestamp(1), |tx| {
            tx.change_checks(
                project,
                0,
                CheckEdit::Save { check: one.clone() },
                None,
                dir.path(),
            )
        })
        .unwrap()
        .value;
    std::fs::remove_file(dir.path().join("test.sh")).unwrap();
    symlink(outside.path().join("secret"), dir.path().join("test.sh")).unwrap();
    assert_eq!(
        changed_inputs(dir.path(), &approved, &one).unwrap(),
        vec!["test.sh"]
    );
    error_code(
        db.write(Timestamp(2), |tx| {
            tx.change_checks(
                project,
                1,
                CheckEdit::KeepInputs {
                    check_id: one.check_id,
                },
                None,
                dir.path(),
            )
        })
        .map(|commit| commit.value),
        ErrorCode::PathNotAllowed,
    );
    assert_eq!(
        db.read(|r| r.project_checks(project)).unwrap().unwrap(),
        approved
    );
}

#[test]
fn measured_times_use_only_passing_completed_approved_jobs_without_new_versions() {
    let (dir, mut db) = database();
    let project = ProjectId::new();
    let one = check("App tests pass");
    let approved = db
        .write(Timestamp(1), |tx| {
            tx.change_checks(
                project,
                0,
                CheckEdit::Save { check: one.clone() },
                None,
                dir.path(),
            )
        })
        .unwrap()
        .value;
    let mut job = CheckJob {
        id: CheckJobId::new(),
        project_id: project,
        task_id: None,
        run_id: None,
        purpose: CheckJobPurpose::RunNow {
            check_ids: vec![one.check_id],
        },
        checks_version: 1,
        checks: vec![one.clone()],
        state: CheckJobState::Running,
        process: None,
        created_at: Timestamp(2),
        started_at: Some(Timestamp(2)),
        finished_at: None,
        sandbox: Some(CheckSandboxLevel::Contained),
        results: vec![result(&one, CheckOutcome::Passed)],
        message: None,
    };
    db.write(Timestamp(2), |tx| {
        tx.put_check_job(&job)?;
        tx.record_check_durations(job.id)
    })
    .unwrap();
    assert_eq!(
        db.read(|r| r.project_checks(project)).unwrap().unwrap(),
        approved
    );
    job.state = CheckJobState::Finished;
    job.finished_at = Some(Timestamp(3));
    job.results[0].outcome = CheckOutcome::Failed;
    db.write(Timestamp(3), |tx| {
        tx.put_check_job(&job)?;
        tx.record_check_durations(job.id)
    })
    .unwrap();
    assert_eq!(
        db.read(|r| r.project_checks(project)).unwrap().unwrap(),
        approved
    );
    job.results[0].outcome = CheckOutcome::Passed;
    job.results[0].duration_secs = 72.5;
    db.write(Timestamp(4), |tx| {
        tx.put_check_job(&job)?;
        tx.record_check_durations(job.id)
    })
    .unwrap();
    let measured = db.read(|r| r.project_checks(project)).unwrap().unwrap();
    assert_eq!(measured.checks[0].typical_secs, Some(72.5));
    assert_eq!(measured.version, approved.version);
    assert_eq!(measured.changed_at, approved.changed_at);
    assert_eq!(measured.changed_by, approved.changed_by);
    assert_eq!(
        db.read(|r| r.checks_history(project)).unwrap(),
        vec![measured.clone()]
    );
    db.write(Timestamp(5), |tx| tx.record_check_durations(job.id))
        .unwrap();
    assert_eq!(
        db.read(|r| r.project_checks(project)).unwrap().unwrap(),
        measured
    );

    // Even a matching check and approved version cannot make a draft proof an
    // approved measurement. The draft may have been changed by its agent.
    let mut proof = job.clone();
    proof.id = CheckJobId::new();
    proof.purpose = CheckJobPurpose::DraftProof {
        draft_id: CheckDraftId::new(),
    };
    proof.results[0].duration_secs = 5.0;
    db.write(Timestamp(6), |tx| {
        tx.put_check_job(&proof)?;
        tx.record_check_durations(proof.id)
    })
    .unwrap();
    assert_eq!(
        db.read(|r| r.project_checks(project)).unwrap().unwrap(),
        measured
    );

    let mut wrong_definition = job.clone();
    wrong_definition.id = CheckJobId::new();
    wrong_definition.checks[0].run = "different command".into();
    wrong_definition.results[0].duration_secs = 2.0;
    db.write(Timestamp(7), |tx| {
        tx.put_check_job(&wrong_definition)?;
        tx.record_check_durations(wrong_definition.id)
    })
    .unwrap();
    assert_eq!(
        db.read(|r| r.project_checks(project)).unwrap().unwrap(),
        measured
    );

    // A newer approval invalidates the old job's measurement even if its
    // command happened not to change.
    let newer = db
        .write(Timestamp(8), |tx| {
            tx.change_checks(
                project,
                1,
                CheckEdit::SetEnabled {
                    check_id: one.check_id,
                    enabled: false,
                },
                None,
                dir.path(),
            )
        })
        .unwrap()
        .value;
    job.results[0].duration_secs = 1.0;
    db.write(Timestamp(9), |tx| {
        tx.put_check_job(&job)?;
        tx.record_check_durations(job.id)
    })
    .unwrap();
    assert_eq!(
        db.read(|r| r.project_checks(project)).unwrap().unwrap(),
        newer
    );
}

#[test]
fn agents_cannot_supply_measured_times_in_new_checks_or_changed_commands() {
    let (dir, mut db) = database();
    let project = ProjectId::new();
    let mut one = check("App tests pass");
    one.typical_secs = Some(0.01);
    let draft = db
        .write(Timestamp(1), |tx| {
            tx.propose_checks(project, 0, None, None, vec![add(&one)], dir.path())
        })
        .unwrap()
        .value;
    let CheckDraftItem::Add {
        check: proposed, ..
    } = &draft.items[0]
    else {
        panic!("add item");
    };
    assert!(proposed.typical_secs.is_none());
    let saved = db
        .write(Timestamp(2), |tx| {
            tx.change_checks(
                project,
                0,
                CheckEdit::Save { check: one.clone() },
                None,
                dir.path(),
            )
        })
        .unwrap()
        .value;
    assert!(saved.checks[0].typical_secs.is_none());
    let mut changed = one.clone();
    changed.run = "different command".into();
    let draft = db
        .write(Timestamp(3), |tx| {
            tx.propose_checks(
                project,
                1,
                None,
                None,
                vec![CheckDraftItem::Change {
                    check: changed,
                    note: "Use the new app tests.".into(),
                }],
                dir.path(),
            )
        })
        .unwrap()
        .value;
    let CheckDraftItem::Change {
        check: proposed, ..
    } = &draft.items[0]
    else {
        panic!("change item");
    };
    assert!(proposed.typical_secs.is_none());
}

#[test]
fn when_descriptions_name_only_unambiguous_literal_folders() {
    let mut one = check("App tests pass");
    assert_eq!(
        check_facts(&one).when,
        "After every task that changes files"
    );
    for (paths, expected) in [
        (
            vec!["src/**"],
            "After a task changes watched files in the src folder",
        ),
        (
            vec!["src/**", "tests/**/*.rs", "src/*.ts"],
            "After a task changes watched files in the src and tests folders",
        ),
        (
            vec!["src/**", "tests/**", "docs/**"],
            "After a task changes watched files in the src, tests and docs folders",
        ),
        (
            vec!["src/**", "tests/**", "docs/**", "scripts/**", "assets/**"],
            "After a task changes watched files in the src, tests, docs and other watched folders",
        ),
        (
            vec!["src/[ab].rs", "src/{one,two}.rs"],
            "After a task changes watched files in the src folder",
        ),
        (
            vec!["package.json"],
            "After a task changes one of its watched files",
        ),
        (
            vec!["*.md"],
            "After a task changes one of its watched files",
        ),
        (
            vec!["src/**", "*.md"],
            "After a task changes one of its watched files",
        ),
        (
            vec!["src/**", "package.json"],
            "After a task changes one of its watched files",
        ),
        (
            vec!["**/*.rs"],
            "After a task changes one of its watched files",
        ),
        (
            vec!["[st]rc/**"],
            "After a task changes one of its watched files",
        ),
        (
            vec!["{src,tests}/**"],
            "After a task changes one of its watched files",
        ),
        (
            vec!["src?/file"],
            "After a task changes one of its watched files",
        ),
        (
            vec!["src\\*/**"],
            "After a task changes one of its watched files",
        ),
        (
            vec!["src/../other/**"],
            "After a task changes one of its watched files",
        ),
        (
            vec!["/src/**"],
            "After a task changes one of its watched files",
        ),
        (
            vec!["src/["],
            "After a task changes one of its watched files",
        ),
        (
            vec!["src//file"],
            "After a task changes one of its watched files",
        ),
        (
            vec!["src/"],
            "After a task changes one of its watched files",
        ),
    ] {
        one.paths = paths.iter().map(|path| (*path).to_owned()).collect();
        assert_eq!(check_facts(&one).when, expected, "patterns: {paths:?}");
    }
    one.paths = vec![format!("{}/**", "a".repeat(41))];
    assert_eq!(
        check_facts(&one).when,
        "After a task changes one of its watched files"
    );
    one.paths = vec!["src/**".into()];
    one.when = CheckWhen::BeforeMerge;
    assert_eq!(
        check_facts(&one).when,
        "Before merging work after changes to watched files in the src folder; not run automatically yet"
    );
    one.paths = vec!["*.md".into()];
    assert_eq!(
        check_facts(&one).when,
        "Before merging work after changes to one of its watched files; not run automatically yet"
    );
    one.paths.clear();
    assert_eq!(
        check_facts(&one).when,
        "Before merging work; not run automatically yet"
    );
}

#[test]
fn factual_differences_describe_the_new_path_scope_without_echoing_patterns() {
    let mut before = check("App tests pass");
    before.paths = vec!["src/**".into()];
    let mut after = before.clone();
    after.paths = vec!["tests/**/*.rs".into()];
    let differences = check_differences(&before, &after);
    assert!(differences.contains(&"After a task changes watched files in the tests folder".into()));
    assert!(differences.contains(&"The watched file patterns changed".into()));
    assert!(
        differences
            .iter()
            .all(|line| !line.contains("**") && !line.contains("*.rs"))
    );
    after.paths = vec!["*.md".into()];
    assert!(
        check_differences(&before, &after)
            .contains(&"After a task changes one of its watched files".into())
    );
}

fn multibyte_text(bytes: usize) -> String {
    let text = format!("{}{}", "é".repeat(bytes / 2), "x".repeat(bytes % 2));
    assert_eq!(text.len(), bytes);
    text
}

#[test]
fn definition_text_limits_count_utf8_bytes_at_each_boundary() {
    type SetText = fn(&mut Check, String);
    let fields: [(usize, SetText, ErrorCode); 6] = [
        (120, |c, s| c.name = s, ErrorCode::InvalidRequest),
        (4_000, |c, s| c.about = s, ErrorCode::InvalidRequest),
        (65_536, |c, s| c.run = s, ErrorCode::InvalidRequest),
        (65_536, |c, s| c.setup = Some(s), ErrorCode::InvalidRequest),
        (1_024, |c, s| c.paths = vec![s], ErrorCode::InvalidRequest),
        (4_096, |c, s| c.inputs = vec![s], ErrorCode::PathNotAllowed),
    ];
    for (limit, set, code) in fields {
        for bytes in [limit - 1, limit] {
            let mut definition = check("App tests pass");
            set(&mut definition, multibyte_text(bytes));
            validate_check(&definition).unwrap();
        }
        let mut definition = check("App tests pass");
        set(&mut definition, multibyte_text(limit + 1));
        let crate::Error::Rule(error) = validate_check(&definition).unwrap_err() else {
            panic!("Expected a validation error");
        };
        assert_eq!(error.code, code);
        assert!(
            error.message.contains(&format!("{limit} UTF-8 bytes")),
            "{error:?}"
        );
    }
}

#[test]
fn draft_text_limits_count_utf8_bytes_at_each_boundary() {
    let (dir, mut db) = database();
    let project = ProjectId::new();
    let original = check("App tests pass");
    db.write(Timestamp(1), |tx| {
        tx.change_checks(
            project,
            0,
            CheckEdit::Save {
                check: original.clone(),
            },
            None,
            dir.path(),
        )
    })
    .unwrap();
    // The same byte limits apply to add notes, change notes, removal reasons,
    // and the optional request retained with the proposal.
    for field in 0..4 {
        let limit = if field == 3 { 16_384 } else { 2_000 };
        for bytes in [limit - 1, limit, limit + 1] {
            let text = multibyte_text(bytes);
            let (request, item) = match field {
                0 => (
                    None,
                    CheckDraftItem::Add {
                        check: check("Another check"),
                        note: text,
                    },
                ),
                1 => (
                    None,
                    CheckDraftItem::Change {
                        check: original.clone(),
                        note: text,
                    },
                ),
                2 => (
                    None,
                    CheckDraftItem::Remove {
                        check_id: original.check_id,
                        reason: text,
                    },
                ),
                _ => (Some(text), add(&check("Another check"))),
            };
            let proposed = db.write(Timestamp(2), |tx| {
                tx.propose_checks(project, 1, None, request, vec![item], dir.path())
            });
            if bytes <= limit {
                proposed.unwrap();
            } else {
                let crate::Error::Rule(error) = proposed.unwrap_err() else {
                    panic!("Expected a validation error");
                };
                assert_eq!(error.code, ErrorCode::InvalidRequest);
                assert!(
                    error.message.contains(&format!("{limit} UTF-8 bytes")),
                    "{error:?}"
                );
            }
        }
    }
}

#[test]
fn resolved_drafts_are_refused_before_store_version_comparison() {
    for applied in [true, false] {
        let (dir, mut db) = database();
        let project = ProjectId::new();
        let definition = check("App tests pass");
        let draft = db
            .write(Timestamp(1), |tx| {
                let draft =
                    tx.propose_checks(project, 0, None, None, vec![add(&definition)], dir.path())?;
                tx.finish_checks_proof(draft.id, vec![result(&definition, CheckOutcome::Passed)])
            })
            .unwrap()
            .value;
        db.write(Timestamp(2), |tx| {
            if applied {
                tx.apply_checks_draft(draft.id, &[definition.check_id], None, dir.path())?;
            } else {
                tx.discard_checks_draft(draft.id)?;
                tx.change_checks(
                    project,
                    0,
                    CheckEdit::Save {
                        check: definition.clone(),
                    },
                    None,
                    dir.path(),
                )?;
            }
            Ok(())
        })
        .unwrap();
        error_code(
            db.write(Timestamp(3), |tx| {
                tx.apply_checks_draft(draft.id, &[definition.check_id], None, dir.path())
            }),
            ErrorCode::AlreadyResolved,
        );
    }
}

#[test]
fn result_output_limit_counts_utf8_bytes_at_the_boundary() {
    let definition = check("App tests pass");
    for bytes in [8_191, 8_192, 8_193] {
        let mut measured = result(&definition, CheckOutcome::Passed);
        measured.output = multibyte_text(bytes);
        if bytes <= 8_192 {
            validate_result(&measured).unwrap();
        } else {
            let crate::Error::Rule(error) = validate_result(&measured).unwrap_err() else {
                panic!("Expected a validation error");
            };
            assert_eq!(error.code, ErrorCode::InvalidRequest);
            assert_eq!(
                error.message,
                "Check output must be at most 8192 UTF-8 bytes."
            );
        }
    }
}

#[test]
fn setup_request_normalization_preserves_text_and_rejects_invalid_blank_input() {
    for request in ["", " \t\r\n", "\u{2003}"] {
        assert_eq!(super::normalize_checks_request(request).unwrap(), None);
    }
    let request = "  Keep café tests.\r\n\tAlso check builds.  ";
    assert_eq!(
        super::normalize_checks_request(request).unwrap().as_deref(),
        Some(request)
    );
    for request in ["\0".to_owned(), "\u{000b}".to_owned(), " ".repeat(16_385)] {
        assert_eq!(
            super::normalize_checks_request(&request).unwrap_err().code,
            ErrorCode::InvalidRequest
        );
    }
}
