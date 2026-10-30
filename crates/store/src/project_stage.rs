// SPDX-License-Identifier: Apache-2.0
//! Stage history lives outside journal retention. Legacy project JSON loads
//! with no stage; no schema rewrite or inferred backfill is needed.
use crate::{Read, Result, Write, safe_json};
use protocol::{
    Event, ProjectId,
    model::Project,
    project::{ProjectStage, ProjectStageHistory, ProjectStageTransition, STAGE_NOTE_LIMIT},
};
use rusqlite::params;

impl Read<'_> {
    pub fn project_stages(
        &self,
        project_id: ProjectId,
        before: Option<u64>,
        limit: u32,
    ) -> Result<ProjectStageHistory> {
        let project: Project = self.require("project", project_id)?;
        let limit = limit.clamp(1, 100) as usize;
        let mut statement = self.connection.prepare_cached(
            "SELECT json FROM documents WHERE kind=?1 AND id < ?2 ORDER BY id DESC LIMIT ?3",
        )?;
        let mut transitions = statement
            .query_map(
                params![
                    format!("project_stage:{project_id}"),
                    format!("{:016}", before.unwrap_or(protocol::MAX_SAFE_INTEGER + 1)),
                    (limit + 1) as i64
                ],
                |r| r.get::<_, String>(0),
            )?
            .map(|row| Ok(serde_json::from_str(&row?)?))
            .collect::<Result<Vec<ProjectStageTransition>>>()?;
        let has_more = transitions.len() > limit;
        transitions.truncate(limit);
        Ok(ProjectStageHistory {
            project_id,
            current: project.stage,
            transitions,
            has_more,
            stages: ProjectStage::ALL.to_vec(),
            note_limit: STAGE_NOTE_LIMIT,
        })
    }
}

impl Write<'_> {
    /// Append exactly the next decision; never overwrite an existing history row.
    /// Projection, history and event are committed with the command receipt.
    pub fn append_project_stage(
        &mut self,
        project_id: ProjectId,
        transition: &ProjectStageTransition,
    ) -> Result<()> {
        let mut project: Project = self.read().require("project", project_id)?;
        let previous = project.stage.as_ref();
        if previous.map_or(0, |t| t.revision).checked_add(1) != Some(transition.revision)
            || previous.map(|t| t.to) != transition.from
        {
            return Err(protocol::ProtocolError::new(
                protocol::ErrorCode::StaleRevision,
                "Project stage history changed",
            )
            .into());
        }
        let kind = format!("project_stage:{project_id}");
        self.tx
            .prepare_cached("INSERT INTO documents(kind,id,json) VALUES(?1,?2,?3)")?
            .execute(params![
                kind,
                format!("{:016}", transition.revision),
                safe_json(transition)?
            ])?;
        self.changed(&kind);
        project.stage = Some(transition.clone());
        self.put("project", project_id, &project)?;
        self.emit(Event::ProjectUpserted { project })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Retention, Store};
    use protocol::{auth::Principal, time::Timestamp};

    #[test]
    fn legacy_database_opens_unset_and_history_survives_retention_restart_and_rollback() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("db.sqlite");
        let id = ProjectId::new();
        // Build the pre-stage schema and record directly, not via the new model.
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch("CREATE TABLE documents (kind TEXT NOT NULL, id TEXT NOT NULL, json TEXT NOT NULL, PRIMARY KEY(kind,id)); CREATE TABLE commands (id TEXT PRIMARY KEY, json TEXT NOT NULL); CREATE TABLE journal (seq INTEGER PRIMARY KEY AUTOINCREMENT, at INTEGER NOT NULL, json TEXT NOT NULL); CREATE INDEX journal_at ON journal(at); PRAGMA user_version=1;").unwrap();
        let old = serde_json::json!({"project_id":id,"name":"Existing project","host_path":"/project","default_harness":null,"default_model":null,"default_effort":null,"active_writer":null,"vcs":null,"created_at":1});
        conn.execute(
            "INSERT INTO documents VALUES('project',?1,?2)",
            params![id.to_string(), old.to_string()],
        )
        .unwrap();
        drop(conn);
        let mut db = Store::open(
            &path,
            Retention {
                max_events: 1,
                max_age_ms: 1,
            },
        )
        .unwrap();
        db.read(|r| {
            assert!(r.require::<Project>("project", id)?.stage.is_none());
            let history = r.project_stages(id, None, 10)?;
            assert!(history.current.is_none() && history.transitions.is_empty());
            Ok(())
        })
        .unwrap();
        let mut from = None;
        for (i, to) in ProjectStage::ALL.into_iter().enumerate() {
            let t = ProjectStageTransition {
                revision: i as u64 + 1,
                from,
                to,
                note: None,
                at: Timestamp(i as i64 + 10),
                principal: Principal::LocalOwner,
                actor_name: "Owner".into(),
            };
            let commit = db
                .write(t.at, |tx| tx.append_project_stage(id, &t))
                .unwrap();
            assert!(commit.changed(&format!("project_stage:{id}")) && commit.changed("project"));
            from = Some(to);
        }
        let current = db
            .read(|r| r.require::<Project>("project", id))
            .unwrap()
            .stage
            .unwrap();
        assert!(
            db.write(Timestamp(20), |tx| tx.append_project_stage(id, &current))
                .is_err()
        );
        let next = ProjectStageTransition {
            revision: 8,
            from,
            to: ProjectStage::Planning,
            ..current.clone()
        };
        let failed: crate::Result<crate::Commit<()>> = db.write(Timestamp(20), |tx| {
            tx.append_project_stage(id, &next)?;
            Err(
                protocol::ProtocolError::new(protocol::ErrorCode::Internal, "rollback fixture")
                    .into(),
            )
        });
        assert!(failed.is_err());
        drop(db);
        let mut db = Store::open(&path, Retention::default()).unwrap();
        db.read(|r| {
            let newest = r.project_stages(id, None, 2)?;
            assert_eq!(newest.current, Some(current));
            assert_eq!(
                newest
                    .transitions
                    .iter()
                    .map(|t| t.revision)
                    .collect::<Vec<_>>(),
                vec![7, 6]
            );
            assert!(newest.has_more);
            let earlier = r.project_stages(id, Some(6), 100)?;
            assert_eq!(
                earlier
                    .transitions
                    .iter()
                    .map(|t| t.revision)
                    .collect::<Vec<_>>(),
                vec![5, 4, 3, 2, 1]
            );
            assert!(!earlier.has_more);
            assert_eq!(r.journal(0)?.events.len(), 1);
            Ok(())
        })
        .unwrap();
    }
}
