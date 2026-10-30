// SPDX-License-Identifier: Apache-2.0
//! Identity-only reconciliation of observed sessions, including finished runs.
use crate::{Read, Result};
use protocol::capability::HarnessKind;
use rusqlite::{Connection, params};

pub(super) fn install(connection: &Connection) -> Result<()> {
    // Additive and maintained by SQLite, including writes by older hosts.
    // Existing payloads are read once when this index is first installed.
    connection.execute_batch(
        "CREATE INDEX IF NOT EXISTS run_native_session ON documents(json_extract(json,'$.harness'),json_extract(json,'$.native_session')) WHERE kind='run';",
    )?;
    Ok(())
}

const HAS_SESSION: &str = "SELECT EXISTS(SELECT 1 FROM documents INDEXED BY run_native_session WHERE kind='run' AND json_extract(json,'$.harness')=json_extract(?1,'$') AND json_extract(json,'$.native_session')=?2)";

impl Read<'_> {
    /// One index seek for a candidate identity; does not read or decode a Run.
    /// Finished runs still own their native session identities.
    pub fn has_native_session(&self, harness: HarnessKind, session: &str) -> Result<bool> {
        Ok(self
            .connection
            .prepare_cached(HAS_SESSION)?
            .query_row(params![serde_json::to_string(&harness)?, session], |r| {
                r.get(0)
            })?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Retention, Store};
    use rusqlite::StatementStatus;
    use serde_json::json;

    fn put(c: &Connection, kind: &str, id: &str, harness: HarnessKind, session: Option<&str>) {
        c.execute(
            "INSERT INTO documents VALUES(?1,?2,?3) ON CONFLICT(kind,id) DO UPDATE SET json=excluded.json",
            params![kind, id, json!({"task_id": "task", "harness": harness,
                "native_session": session, "ended_at": 1, "holds_writer": false}).to_string()],
        ).unwrap();
    }

    #[test]
    fn existing_and_changed_run_identities_survive_reopen_and_rollback() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessions.sqlite");
        let db = Store::open(&path, Retention::default()).unwrap();
        // Simulate an older database with runs but no session identity index.
        db.connection
            .execute_batch("DROP INDEX run_native_session")
            .unwrap();
        put(
            &db.connection,
            "run",
            "finished",
            HarnessKind::ClaudeCode,
            Some("shared"),
        );
        put(
            &db.connection,
            "run",
            "duplicate",
            HarnessKind::ClaudeCode,
            Some("shared"),
        );
        put(
            &db.connection,
            "run",
            "no-session",
            HarnessKind::Codex,
            None,
        );
        put(
            &db.connection,
            "meta",
            "other-kind",
            HarnessKind::Codex,
            Some("shared"),
        );
        drop(db);
        let db = Store::open(&path, Retention::default()).unwrap();
        let c = &db.connection;
        let read = Read { connection: c };
        assert!(
            read.has_native_session(HarnessKind::ClaudeCode, "shared")
                .unwrap()
        );
        assert!(
            !read
                .has_native_session(HarnessKind::Codex, "shared")
                .unwrap()
        );
        assert!(!read.has_native_session(HarnessKind::Codex, "").unwrap());
        assert!(
            !read
                .has_native_session(HarnessKind::ClaudeCode, "SHARED")
                .unwrap()
        );
        let literal = "quote'\"; Unicode 🐍";
        put(c, "run", "finished", HarnessKind::Codex, Some(literal));
        assert!(
            read.has_native_session(HarnessKind::Codex, literal)
                .unwrap()
        );
        c.execute_batch("BEGIN; DELETE FROM documents WHERE kind='run' AND id='duplicate';")
            .unwrap();
        assert!(
            !read
                .has_native_session(HarnessKind::ClaudeCode, "shared")
                .unwrap()
        );
        c.execute_batch("ROLLBACK").unwrap();
        assert!(
            read.has_native_session(HarnessKind::ClaudeCode, "shared")
                .unwrap()
        );
        c.execute(
            "DELETE FROM documents WHERE kind='run' AND id='duplicate'",
            [],
        )
        .unwrap();
        put(c, "run", "finished", HarnessKind::Codex, None);
        assert!(
            !read
                .has_native_session(HarnessKind::Codex, literal)
                .unwrap()
        );
        drop(db);
        let mut db = Store::open(&path, Retention::default()).unwrap();
        assert!(
            !db.read(|r| r.has_native_session(HarnessKind::ClaudeCode, "shared"))
                .unwrap()
        );
    }

    #[test]
    fn candidate_lookups_seek_the_index_without_scanning_history() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE documents(kind TEXT,id TEXT,json TEXT,PRIMARY KEY(kind,id));",
        )
        .unwrap();
        install(&c).unwrap();
        c.execute_batch("BEGIN").unwrap();
        for n in 0..10000 {
            put(
                &c,
                "run",
                &n.to_string(),
                HarnessKind::ClaudeCode,
                Some(&format!("saved-{n}")),
            );
        }
        c.execute_batch("COMMIT").unwrap();
        for (session, found) in [("saved-0", true), ("saved-9999", true), ("missing", false)] {
            let mut statement = c.prepare(HAS_SESSION).unwrap();
            let actual: bool = statement
                .query_row(params!["\"claude_code\"", session], |r| r.get(0))
                .unwrap();
            assert_eq!(actual, found);
            assert_eq!(statement.get_status(StatementStatus::FullscanStep), 0);
            let steps = statement.get_status(StatementStatus::VmStep);
            assert!(steps < 40, "{session}: {steps} VM steps");
        }
    }
}
