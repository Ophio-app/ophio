// SPDX-License-Identifier: Apache-2.0
//! Guard reads are bounded by pending requests for the running turn. Resolved
//! history never enters the index; carried-on requests remain answerable but
//! no longer block their run.
use crate::{Read, Result, decode};
use protocol::{RunId, model::Approval};
use rusqlite::Connection;

pub(super) fn install(connection: &Connection) -> Result<()> {
    // Additive and maintained by SQLite, including writes from older hosts.
    // Existing approval payloads are read once when the index is installed.
    connection.execute_batch(
        "CREATE INDEX IF NOT EXISTS approval_pending_run ON documents(json_extract(json,'$.run_id'),id)
         WHERE kind='approval' AND json_extract(json,'$.state')='pending';",
    )?;
    Ok(())
}

// Force the partial index even without statistics: the primary key would
// supply id order but read every saved approval. Marker lookups use its key.
const PENDING: &str = "SELECT a.json FROM documents AS a INDEXED BY approval_pending_run
    WHERE a.kind='approval' AND json_extract(a.json,'$.state')='pending'
    AND json_extract(a.json,'$.run_id')=?1
    AND NOT EXISTS(SELECT 1 FROM documents AS carried WHERE carried.kind='approval_carried_on' AND carried.id=a.id)
    ORDER BY a.id";
const BLOCKED: &str = "SELECT EXISTS(SELECT 1 FROM documents AS a INDEXED BY approval_pending_run
    WHERE a.kind='approval' AND json_extract(a.json,'$.state')='pending'
    AND json_extract(a.json,'$.run_id')=?1
    AND NOT EXISTS(SELECT 1 FROM documents AS carried WHERE carried.kind='approval_carried_on' AND carried.id=a.id))";

impl Read<'_> {
    /// Pending requests for one run, excluding those already carried on.
    pub fn run_pending_approvals(&self, run: RunId) -> Result<Vec<Approval>> {
        let mut statement = self.connection.prepare_cached(PENDING)?;
        statement
            .query_map([run.to_string()], |r| r.get::<_, String>(0))?
            .map(|json| decode(&json?))
            .collect()
    }

    /// Whether any pending request still blocks a run, without loading its
    /// payload. This includes kinds that cannot automatically carry on.
    pub fn run_has_blocking_approval(&self, run: RunId) -> Result<bool> {
        Ok(self
            .connection
            .prepare_cached(BLOCKED)?
            .query_row([run.to_string()], |r| r.get(0))?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Retention, Store};
    use protocol::{ApprovalId, ProjectId, TaskId, time::Timestamp};
    use rusqlite::{StatementStatus, params};
    use serde_json::{Value, json};

    fn approval(id: ApprovalId, run: RunId, state: &str) -> Value {
        // Historical shape, before optional fields were added.
        json!({
            "approval_id":id, "run_id":run, "task_id":TaskId::new(),
            "project_id":ProjectId::new(), "native_request_id":"request",
            "kind":"question", "title":"Question", "detail":null,
            "raw_request":"x".repeat(8 * 1024), "scope":"one request",
            "revision":1, "state":state, "requested_at":1,
            "expires_at":null, "resolved_by":null, "resolved_at":null
        })
    }

    fn put(c: &Connection, id: ApprovalId, value: &Value) {
        c.execute(
            "INSERT INTO documents(kind,id,json) VALUES('approval',?1,?2)
             ON CONFLICT(kind,id) DO UPDATE SET json=excluded.json",
            params![id.to_string(), value.to_string()],
        )
        .unwrap();
    }

    #[test]
    fn existing_approvals_and_carried_on_markers_stay_current() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("approvals.sqlite");
        let db = Store::open(&path, Retention::default()).unwrap();
        db.connection
            .execute_batch("DROP INDEX approval_pending_run")
            .unwrap();
        let run = RunId::new();
        let first = ApprovalId::new();
        let second = ApprovalId::new();
        put(&db.connection, first, &approval(first, run, "pending"));
        let mut other = approval(second, run, "pending");
        // Kinds with their own timeout must still block a run.
        other["kind"] = json!("github_post");
        put(&db.connection, second, &other);
        let unrelated = ApprovalId::new();
        put(
            &db.connection,
            unrelated,
            &approval(unrelated, RunId::new(), "pending"),
        );
        drop(db);
        let mut db = Store::open(&path, Retention::default()).unwrap();
        db.read(|r| {
            let ids: Vec<_> = r
                .run_pending_approvals(run)?
                .iter()
                .map(|a| a.approval_id)
                .collect();
            assert_eq!(ids, [first, second]);
            assert!(r.run_has_blocking_approval(run)?);
            Ok(())
        })
        .unwrap();
        db.write(Timestamp(2), |tx| {
            tx.put("approval_carried_on", first, &Timestamp(2))?;
            assert!(tx.read().run_has_blocking_approval(run)?);
            assert_eq!(tx.read().run_pending_approvals(run)?[0].approval_id, second);
            tx.put("approval_carried_on", second, &Timestamp(2))?;
            assert!(!tx.read().run_has_blocking_approval(run)?);
            assert!(tx.read().run_pending_approvals(run)?.is_empty());
            tx.delete("approval_carried_on", first)?;
            assert!(tx.read().run_has_blocking_approval(run)?);
            // Resolving and cancelling remove rows from the pending index.
            tx.put("approval", first, &approval(first, run, "allowed"))?;
            assert!(!tx.read().run_has_blocking_approval(run)?);
            tx.delete("approval_carried_on", second)?;
            assert!(tx.read().run_has_blocking_approval(run)?);
            other["state"] = json!("cancelled");
            tx.put("approval", second, &other)?;
            assert!(!tx.read().run_has_blocking_approval(run)?);
            assert!(tx.read().run_pending_approvals(run)?.is_empty());
            Ok(())
        })
        .unwrap();
        // A failed write rolls the index and the document back together.
        let failed: Result<_> = db.write(Timestamp(3), |tx| {
            tx.put("approval", first, &approval(first, run, "pending"))?;
            assert!(tx.read().run_has_blocking_approval(run)?);
            Err::<(), _>(crate::Error::IntegerExhausted)
        });
        assert!(failed.is_err());
        db.read(|r| {
            assert!(!r.run_has_blocking_approval(run)?);
            assert!(r.run_pending_approvals(run)?.is_empty());
            Ok(())
        })
        .unwrap();
    }

    fn steps(c: &Connection, sql: &str, run: RunId) -> i32 {
        let mut statement = c.prepare(sql).unwrap();
        let mut rows = statement.query([run.to_string()]).unwrap();
        while rows.next().unwrap().is_some() {}
        drop(rows);
        statement.get_status(StatementStatus::VmStep)
    }

    #[test]
    fn queries_do_not_scan_resolved_or_other_runs_pending_approvals() {
        let dir = tempfile::tempdir().unwrap();
        let db = Store::open(dir.path().join("approvals.sqlite"), Retention::default()).unwrap();
        let c = &db.connection;
        let run = RunId::new();
        let absent = RunId::new();
        let id = ApprovalId::new();
        put(c, id, &approval(id, run, "pending"));
        // Keep a following index range in both measurements. Reaching the
        // next run instead of the end of the index costs one extra VM step.
        let other = ApprovalId::new();
        put(c, other, &approval(other, RunId::new(), "pending"));
        let measure = || {
            [
                steps(c, PENDING, run),
                steps(c, BLOCKED, run),
                steps(c, PENDING, absent),
                steps(c, BLOCKED, absent),
            ]
        };
        let baseline = measure();
        c.execute_batch("BEGIN").unwrap();
        for _ in 0..1000 {
            let id = ApprovalId::new();
            put(c, id, &approval(id, run, "allowed"));
            let id = ApprovalId::new();
            put(c, id, &approval(id, RunId::new(), "pending"));
        }
        c.execute_batch("COMMIT").unwrap();
        let with_history = measure();
        eprintln!("approval query VM steps: before={baseline:?}, after={with_history:?}");
        assert_eq!(with_history, baseline);
    }
}
