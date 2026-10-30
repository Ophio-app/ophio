// SPDX-License-Identifier: Apache-2.0
//! Additive history indexes and a transactionally maintained, diff-free run
//! projection. SQL triggers keep it current even when an older host writes.
use crate::{Read, Result, decode};
use protocol::{ProjectId, RunId, TaskId, model::*};
use rusqlite::{Connection, params};
use serde::de::DeserializeOwned;

pub(super) fn install(connection: &mut Connection) -> Result<()> {
    let tx = connection.transaction()?;
    tx.execute_batch(include_str!("history.sql"))?;
    let exists: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='history_runs')",
        [],
        |r| r.get(0),
    )?;
    if !exists {
        // Only the first open reads old run payloads. The table and triggers
        // commit with the backfill; an interrupted install leaves none of them.
        tx.execute_batch(include_str!("history-projections.sql"))?;
        let summary = "CASE WHEN json_type(new.json,'$.outcome.changed_files')='array' THEN json_set(new.json,'$.outcome.changed_files',json((SELECT json_group_array(json_remove(value,'$.diff')) FROM json_each(new.json,'$.outcome.changed_files')))) ELSE new.json END";
        let values = format!(
            "new.id,json_extract(new.json,'$.task_id'),json_extract(new.json,'$.ended_at') IS NULL OR coalesce(json_extract(new.json,'$.holds_writer'),0) OR coalesce(json_extract(new.json,'$.cleanup_pending'),0),{summary}"
        );
        for operation in ["INSERT", "UPDATE"] {
            tx.execute_batch(&format!("CREATE TRIGGER history_run_{operation} AFTER {operation} ON documents WHEN new.kind='run' BEGIN
                INSERT INTO history_runs SELECT {values} ON CONFLICT(id) DO UPDATE SET task_id=excluded.task_id,open=excluded.open,json=excluded.json;
                END;"))?;
        }
        tx.execute_batch("CREATE TRIGGER history_run_delete AFTER DELETE ON documents WHEN old.kind='run' BEGIN DELETE FROM history_runs WHERE id=old.id; END;
            CREATE TRIGGER history_run_rekey AFTER UPDATE OF kind,id ON documents WHEN old.kind='run' AND (new.kind!='run' OR old.id!=new.id) BEGIN DELETE FROM history_runs WHERE id=old.id; END;")?;
        tx.execute_batch(&format!(
            "INSERT INTO history_runs SELECT {values} FROM documents AS new WHERE kind='run';"
        ))?;
    }
    tx.commit()?;
    Ok(())
}

// Two index seeks avoid SQLite scanning a long timestamp tie (or all newer
// tasks) when a row-value comparison contains expression-index columns.
const PAGE_AT: &str = "SELECT json FROM documents INDEXED BY task_history_page WHERE kind='task' AND json_extract(json,'$.project_id')=?1 AND json_extract(json,'$.updated_at')=?2 AND id<=?3 ORDER BY id DESC LIMIT ?4";
const PAGE_BEFORE: &str = "SELECT json FROM documents INDEXED BY task_history_page WHERE kind='task' AND json_extract(json,'$.project_id')=?1 AND json_extract(json,'$.updated_at')<?2 ORDER BY json_extract(json,'$.updated_at') DESC,id DESC LIMIT ?3";
const SNAPSHOT_FIRST: &str = "SELECT json FROM documents INDEXED BY task_history_snapshot WHERE kind='task' ORDER BY json_extract(json,'$.updated_at') DESC,id DESC LIMIT 1";
const SNAPSHOT_AT: &str = "SELECT json FROM documents INDEXED BY task_history_snapshot WHERE kind='task' AND json_extract(json,'$.updated_at')=?1 AND id<?2 ORDER BY id DESC LIMIT 1";
const SNAPSHOT_BEFORE: &str = "SELECT json FROM documents INDEXED BY task_history_snapshot WHERE kind='task' AND json_extract(json,'$.updated_at')<?1 ORDER BY json_extract(json,'$.updated_at') DESC,id DESC LIMIT 1";
// Seek once per distinct project rather than scanning every retained task
// for DISTINCT. Include projects whose task documents outlive the project.
const OLDER_PROJECTS: &str = "WITH RECURSIVE projects(project) AS (
    SELECT min(json_extract(json,'$.project_id')) FROM documents INDEXED BY task_history_page WHERE kind='task'
    UNION ALL
    SELECT (SELECT min(json_extract(json,'$.project_id')) FROM documents INDEXED BY task_history_page WHERE kind='task' AND json_extract(json,'$.project_id')>projects.project) FROM projects WHERE project IS NOT NULL
) SELECT project FROM projects WHERE project IS NOT NULL AND (
    EXISTS(SELECT 1 FROM documents INDEXED BY task_history_page WHERE kind='task' AND json_extract(json,'$.project_id')=projects.project AND json_extract(json,'$.updated_at')<?1)
    OR EXISTS(SELECT 1 FROM documents INDEXED BY task_history_page WHERE kind='task' AND json_extract(json,'$.project_id')=projects.project AND json_extract(json,'$.updated_at')=?1 AND id<=?2)
) ORDER BY project";
const LIVE: &str = "SELECT id FROM documents INDEXED BY task_history_live WHERE kind='task' AND json_extract(json,'$.state') IN ('accepted','dispatching','working','awaiting_decision')
    UNION SELECT task_id FROM history_runs INDEXED BY history_runs_open WHERE open=1
    UNION SELECT json_extract(json,'$.task_id') FROM documents INDEXED BY approval_history_live WHERE kind='approval' AND json_extract(json,'$.state')='pending'
    UNION SELECT json_extract(json,'$.task_id') FROM documents INDEXED BY attention_history_live WHERE kind='attention' AND json_extract(json,'$.resolved_at') IS NULL AND json_extract(json,'$.kind') NOT IN ('completed','failure')";

impl Read<'_> {
    /// The next task across all projects, strictly older than `before`.
    /// Read one at a time so a full snapshot never decodes the remaining tail.
    pub fn history_snapshot_task(&self, before: Option<TaskCursor>) -> Result<Option<Task>> {
        let Some(before) = before else {
            return Ok(self.history_query(SNAPSHOT_FIRST, [])?.pop());
        };
        let tied = self.history_query(
            SNAPSHOT_AT,
            params![before.updated_at.0, before.task_id.to_string()],
        )?;
        if let Some(task) = tied.into_iter().next() {
            return Ok(Some(task));
        }
        Ok(self
            .history_query(SNAPSHOT_BEFORE, [before.updated_at.0])?
            .pop())
    }
    /// Projects with tasks at or below the inclusive history cursor, using
    /// index keys only; no task or related payloads need to be decoded.
    pub fn history_older_projects(&self, from: TaskCursor) -> Result<Vec<ProjectId>> {
        let mut statement = self.connection.prepare_cached(OLDER_PROJECTS)?;
        statement
            .query_map(params![from.updated_at.0, from.task_id.to_string()], |r| {
                r.get::<_, String>(0)
            })?
            .map(|row| Ok(serde_json::from_value(serde_json::Value::String(row?))?))
            .collect()
    }
    /// A bounded inclusive cursor page in one project, newest first.
    pub fn history_tasks(
        &self,
        project: protocol::ProjectId,
        from: TaskCursor,
        limit: usize,
    ) -> Result<Vec<Task>> {
        let limit = limit.min(i64::MAX as usize);
        let mut tasks = self.history_query(
            PAGE_AT,
            params![
                project.to_string(),
                from.updated_at.0,
                from.task_id.to_string(),
                limit as i64
            ],
        )?;
        if tasks.len() < limit {
            tasks.extend(self.history_query::<Task>(
                PAGE_BEFORE,
                params![
                    project.to_string(),
                    from.updated_at.0,
                    (limit - tasks.len()) as i64
                ],
            )?);
        }
        Ok(tasks)
    }
    /// Task ids with live state or actionable records. Finished history is
    /// absent from these partial indexes, including runs' reported diffs.
    pub fn history_live_ids(&self) -> Result<Vec<TaskId>> {
        let mut statement = self.connection.prepare_cached(LIVE)?;
        statement
            .query_map([], |r| r.get::<_, Option<String>>(0))?
            .filter_map(|row| row.transpose())
            .map(|row| Ok(serde_json::from_value(serde_json::Value::String(row?))?))
            .collect()
    }
    /// Run summaries belonging to one task; admission needs only open runs
    /// and the task's current run. Full run reads retain their original diffs.
    pub fn history_runs(&self, task: &Task, live_only: bool) -> Result<Vec<Run>> {
        if live_only {
            self.history_query("SELECT json FROM history_runs WHERE id=?1 AND task_id=?2 UNION ALL SELECT json FROM history_runs INDEXED BY history_runs_open WHERE task_id=?2 AND open=1 AND id IS NOT ?1",
                params![task.current_run.map(|id| id.to_string()), task.task_id.to_string()])
        } else {
            self.history_query("SELECT json FROM history_runs INDEXED BY history_runs_task WHERE task_id=?1 ORDER BY id", [task.task_id.to_string()])
        }
    }
    /// The diff-free summary of the run a task had just before `run`. Run
    /// ids are time-ordered, so the one before is the one admitted before.
    pub fn history_run_before(&self, task: TaskId, run: RunId) -> Result<Option<Run>> {
        Ok(self
            .history_query(
                "SELECT json FROM history_runs INDEXED BY history_runs_task WHERE task_id=?1 AND id<?2 ORDER BY id DESC LIMIT 1",
                [task.to_string(), run.to_string()],
            )?
            .pop())
    }
    /// One indexed, diff-free run summary, including its outcome and helpers.
    pub fn history_run(&self, run: RunId) -> Result<Option<Run>> {
        Ok(self
            .history_query(
                "SELECT json FROM history_runs WHERE id=?1",
                [run.to_string()],
            )?
            .pop())
    }
    pub fn history_approvals(&self, task: TaskId, live_only: bool) -> Result<Vec<Approval>> {
        if live_only {
            self.history_query("SELECT json FROM documents INDEXED BY approval_history_live WHERE kind='approval' AND json_extract(json,'$.state')='pending' AND json_extract(json,'$.task_id')=?1 ORDER BY id", [task.to_string()])
        } else {
            self.history_query("SELECT json FROM documents INDEXED BY approval_history_task WHERE kind='approval' AND json_extract(json,'$.task_id')=?1 ORDER BY id", [task.to_string()])
        }
    }
    /// Admission carries unresolved notices, and reserves the latest resolved
    /// notice for each still-pending approval in case delivery raises it again.
    pub fn history_live_attention(
        &self,
        task: TaskId,
        pending: &[Approval],
    ) -> Result<Vec<AttentionItem>> {
        let mut items = self.history_query("SELECT json FROM documents INDEXED BY attention_history_unresolved WHERE kind='attention' AND json_extract(json,'$.resolved_at') IS NULL AND json_extract(json,'$.task_id')=?1 ORDER BY id", [task.to_string()])?;
        for approval in pending {
            let back: Vec<AttentionItem> = self.history_query("SELECT json FROM documents INDEXED BY attention_history_approval WHERE kind='attention' AND json_extract(json,'$.task_id')=?1 AND json_extract(json,'$.approval_id')=?2 ORDER BY json_extract(json,'$.created_at') DESC,id LIMIT 1", params![task.to_string(), approval.approval_id.to_string()])?;
            items.extend(back.into_iter().filter(|a| a.resolved_at.is_some()));
        }
        Ok(items)
    }
    /// Unresolved notices with no stored task still travel in snapshots.
    pub fn history_unattached(&self) -> Result<Vec<AttentionItem>> {
        self.history_query("SELECT a.json FROM history_orphan_attention AS o CROSS JOIN documents AS a WHERE a.kind='attention' AND a.id=o.id ORDER BY o.id", [])
    }
    fn history_query<T: DeserializeOwned>(
        &self,
        sql: &str,
        params: impl rusqlite::Params,
    ) -> Result<Vec<T>> {
        let mut statement = self.connection.prepare_cached(sql)?;
        statement
            .query_map(params, |r| r.get::<_, String>(0))?
            .map(|row| decode(&row?))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Retention, Store};
    use rusqlite::StatementStatus;
    use serde_json::{Value, json};

    fn legacy(path: &std::path::Path) -> Connection {
        let c = Connection::open(path).unwrap();
        c.execute_batch("CREATE TABLE documents(kind TEXT NOT NULL,id TEXT NOT NULL,json TEXT NOT NULL,PRIMARY KEY(kind,id));
            CREATE TABLE commands(id TEXT PRIMARY KEY,json TEXT NOT NULL);
            CREATE TABLE journal(seq INTEGER PRIMARY KEY AUTOINCREMENT,at INTEGER NOT NULL,json TEXT NOT NULL);
            CREATE INDEX journal_at ON journal(at); PRAGMA user_version=1;").unwrap();
        c
    }
    fn put(c: &Connection, kind: &str, id: &str, value: &Value) {
        c.execute("INSERT INTO documents VALUES(?1,?2,?3) ON CONFLICT(kind,id) DO UPDATE SET json=excluded.json", params![kind,id,value.to_string()]).unwrap();
    }
    fn summary(c: &Connection) -> (bool, Value) {
        c.query_row(
            "SELECT open,json FROM history_runs WHERE id='run'",
            [],
            |r| {
                Ok((
                    r.get(0)?,
                    serde_json::from_str(&r.get::<_, String>(1)?).unwrap(),
                ))
            },
        )
        .unwrap()
    }
    #[test]
    fn backfill_and_older_writers_keep_summaries_atomic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.sqlite");
        let c = legacy(&path);
        let run = json!({"task_id":"task","ended_at":1,"holds_writer":false,
            "outcome":{"summary":"kept","changed_files":[{"path":"a","diff":{"text":"x".repeat(102400)}},{"path":"b","diff":null}]}});
        put(&c, "run", "run", &run);
        drop(c);
        let mut db = Store::open(&path, Retention::default()).unwrap();
        let (open, listed) = summary(&db.connection);
        assert!(!open);
        assert_eq!(listed["outcome"]["summary"], "kept");
        assert_eq!(
            listed["outcome"]["changed_files"],
            json!([{"path":"a"},{"path":"b"}])
        );
        assert_eq!(db.read(|r| r.require::<Value>("run", "run")).unwrap(), run);
        // An older writer knows only documents. Its updates still reach the
        // summary and roll back with it, including changes in task association.
        db.connection.execute_batch("BEGIN").unwrap();
        let mut changed = run.clone();
        changed["task_id"] = json!("other");
        changed["cleanup_pending"] = json!(true);
        put(&db.connection, "run", "run", &changed);
        assert!(summary(&db.connection).0);
        assert_eq!(summary(&db.connection).1["task_id"], "other");
        db.connection.execute_batch("ROLLBACK").unwrap();
        assert_eq!(summary(&db.connection), (open, listed.clone()));
        drop(db);
        let db = Store::open(&path, Retention::default()).unwrap();
        assert_eq!(summary(&db.connection), (open, listed));
        changed["outcome"] = Value::Null;
        put(&db.connection, "run", "run", &changed);
        assert_eq!(summary(&db.connection).1, changed);
        db.connection
            .execute("DELETE FROM documents WHERE kind='run'", [])
            .unwrap();
        let count: i64 = db
            .connection
            .query_row("SELECT count(*) FROM history_runs", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn orphan_notices_follow_task_and_attention_lifecycle() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.sqlite");
        let c = legacy(&path);
        let notice = json!({"task_id":"task","resolved_at":null,"kind":"completed"});
        put(&c, "attention", "notice", &notice);
        drop(c);
        let db = Store::open(&path, Retention::default()).unwrap();
        let c = &db.connection;
        let count = || {
            c.query_row("SELECT count(*) FROM history_orphan_attention", [], |r| {
                r.get::<_, i64>(0)
            })
            .unwrap()
        };
        assert_eq!(count(), 1);
        put(c, "task", "task", &json!({"state":"completed"}));
        assert_eq!(count(), 0);
        c.execute_batch("BEGIN; DELETE FROM documents WHERE kind='task';")
            .unwrap();
        assert_eq!(count(), 1);
        c.execute_batch("ROLLBACK").unwrap();
        assert_eq!(count(), 0);
        c.execute("DELETE FROM documents WHERE kind='task'", [])
            .unwrap();
        assert_eq!(count(), 1);
        put(
            c,
            "attention",
            "notice",
            &json!({"task_id":"task","resolved_at":1}),
        );
        assert_eq!(count(), 0);
        put(c, "attention", "notice", &notice);
        assert_eq!(count(), 1);
        c.execute("DELETE FROM documents WHERE kind='attention'", [])
            .unwrap();
        assert_eq!(count(), 0);
    }

    fn steps(c: &Connection, sql: &str, params: impl rusqlite::Params) -> i32 {
        let mut s = c.prepare(sql).unwrap();
        let _ = s
            .query_map(params, |r| r.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        s.get_status(StatementStatus::VmStep)
    }
    #[test]
    fn older_projects_include_cursor_ties_but_exclude_newer_only_projects() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = Store::open(dir.path().join("history.sqlite"), Retention::default()).unwrap();
        let from = TaskCursor {
            updated_at: protocol::time::Timestamp(10),
            task_id: TaskId::new(),
        };
        let tied = ProjectId::new();
        let older = ProjectId::new();
        let newer = ProjectId::new();
        let newer_tie = ProjectId::new();
        for (project, at, id) in [
            (tied, 10, from.task_id),
            (older, 9, TaskId::new()),
            (newer, 11, TaskId::new()),
            (newer_tie, 10, TaskId::new()),
        ] {
            put(
                &db.connection,
                "task",
                &id.to_string(),
                &json!({"project_id":project,"updated_at":at,"state":"completed"}),
            );
        }
        // No project documents exist; retained tasks still need a history page.
        let mut expected = vec![tied, older];
        expected.sort();
        assert_eq!(
            db.read(|r| r.history_older_projects(from)).unwrap(),
            expected
        );
    }
    #[test]
    fn deep_pages_snapshots_and_admission_do_not_scan_finished_rows() {
        let dir = tempfile::tempdir().unwrap();
        let db = Store::open(dir.path().join("history.sqlite"), Retention::default()).unwrap();
        let c = &db.connection;
        c.execute_batch("BEGIN").unwrap();
        for n in 0..10000 {
            let id = format!("{n:08}");
            put(
                c,
                "task",
                &id,
                &json!({"project_id":"project","updated_at":n/5000,"state":"completed"}),
            );
            put(
                c,
                "run",
                &id,
                &json!({"task_id":id,"ended_at":1,"holds_writer":false}),
            );
            put(
                c,
                "attention",
                &id,
                &json!({"task_id":id,"kind":"completed","resolved_at":null}),
            );
            put(
                c,
                "approval",
                &id,
                &json!({"task_id":id,"state":"resolved"}),
            );
        }
        c.execute_batch("COMMIT").unwrap();
        for (at, id) in [(1, "00009999"), (1, "00005010"), (0, "00000010")] {
            let cost = steps(c, PAGE_AT, params!["project", at, id, 10]);
            assert!(cost < 200, "timestamp tie at {id}: {cost} VM steps");
            let cost = steps(c, SNAPSHOT_AT, params![at, id]);
            assert!(cost < 50, "snapshot tie at {id}: {cost} VM steps");
            let cost = steps(c, OLDER_PROJECTS, params![at, id]);
            assert!(cost < 200, "older projects at {id}: {cost} VM steps");
        }
        let cost = steps(c, SNAPSHOT_FIRST, []);
        assert!(cost < 50, "newest snapshot task: {cost} VM steps");
        let cost = steps(c, SNAPSHOT_BEFORE, [1]);
        assert!(cost < 50, "older snapshot task: {cost} VM steps");
        let cost = steps(c, PAGE_BEFORE, params!["project", 1, 10]);
        assert!(cost < 200, "deep page: {cost} VM steps");
        let cost = steps(c, LIVE, []);
        assert!(cost < 200, "live task lookup: {cost} VM steps");
        let cost = steps(
            c,
            "SELECT a.json FROM history_orphan_attention AS o CROSS JOIN documents AS a WHERE a.kind='attention' AND a.id=o.id ORDER BY o.id",
            [],
        );
        assert!(cost < 100, "orphan notices: {cost} VM steps");
    }
}
