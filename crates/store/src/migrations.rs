// SPDX-License-Identifier: Apache-2.0
use super::*;
use protocol::{
    ApprovalId, ProjectId, RunId, TaskId,
    model::{Approval, Task, TaskMode},
};
use serde_json::json;

#[test]
fn future_schema_refusal_preserves_data_and_explains_backup_recovery() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("companion.sqlite");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch("PRAGMA user_version=2; CREATE TABLE sentinel(value TEXT); INSERT INTO sentinel VALUES('keep');").unwrap();
    drop(conn);
    let error = Store::open(&path, Retention::default()).unwrap_err();
    assert!(matches!(error, Error::FutureSchema));
    let message = error.to_string();
    for part in [
        "newer Øphio",
        "stop Øphio",
        ".before-v<schema>-<id>.sqlite",
        "previous version",
        "-wal/-shm",
    ] {
        assert!(message.contains(part));
    }
    let conn = Connection::open(path).unwrap();
    assert_eq!(
        conn.pragma_query_value(None, "user_version", |r| r.get::<_, u32>(0))
            .unwrap(),
        2
    );
    assert_eq!(
        conn.query_row("SELECT value FROM sentinel", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "keep"
    );
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}

#[test]
fn current_schema_loads_records_before_optional_fields_were_added() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("companion.sqlite");
    let mut db = Store::open(&path, Retention::default()).unwrap();
    let task = TaskId::new();
    let project = ProjectId::new();
    let approval = ApprovalId::new();
    // Historical record shapes: no task mode or approval outcome.
    let task_json = json!({ "task_id": task, "project_id": project, "kind":"coding", "title":"Retry check",
        "state":"awaiting_decision", "revision":1, "harness":"claude_code", "model":null, "effort":null,
        "created_by":{"kind":"local_owner"}, "created_at":1, "updated_at":2,
        "current_run":null,"target_id":null,"mission":null,"attention_count":1,"status_line":null });
    let approval_json = json!({"approval_id":approval,"run_id":RunId::new(),"task_id":task,"project_id":project,
        "native_request_id":"fixture-approval", "kind":"tool", "title":"Run check", "detail":null,
        "raw_request":null,"scope":"one command","revision":1,"state":"pending","requested_at":2,
        "expires_at":null,"resolved_by":null,"resolved_at":null});
    db.write(Timestamp(2), |tx| {
        tx.put("task", task, &task_json)?;
        tx.put("approval", approval, &approval_json)
    })
    .unwrap();
    drop(db);
    let mut db = Store::open(&path, Retention::default()).unwrap();
    db.read(|r| {
        assert_eq!(r.require::<Task>("task", task)?.mode, TaskMode::Work);
        assert!(
            r.require::<Approval>("approval", approval)?
                .outcome
                .is_none()
        );
        Ok(())
    })
    .unwrap();
}

#[test]
fn pre_migration_backup_is_a_readable_snapshot_including_committed_wal() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("companion.sqlite");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE previous(value INTEGER); INSERT INTO previous VALUES(42);").unwrap();
    let db = Store::open(&path, Retention::default()).unwrap();
    let backup = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .contains(".before-v1-")
        })
        .unwrap();
    let before = Connection::open(backup).unwrap();
    assert_eq!(
        before
            .pragma_query_value(None, "user_version", |r| r.get::<_, u32>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        before
            .query_row("SELECT value FROM previous", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        42
    );
    drop(db);
    drop(conn);
}
