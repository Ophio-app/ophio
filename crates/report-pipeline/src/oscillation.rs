// SPDX-License-Identifier: Apache-2.0
use crate::{Result, digest, gates::CommitDiff, schema::Area};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::path::Path;

pub const DEFAULT_WINDOW_MS: i64 = 30 * 24 * 60 * 60 * 1000;
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Conflict {
    pub previous_report: String,
    pub new_report: String,
    pub path: Option<String>,
    pub reversal: bool,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OscillationReport {
    pub conflicts: Vec<Conflict>,
}
impl OscillationReport {
    pub fn stop_for_owner(&self) -> bool {
        self.conflicts.iter().any(|c| c.reversal)
    }
    pub fn flagged(&self) -> bool {
        !self.conflicts.is_empty()
    }
}
#[derive(Debug)]
pub struct Oscillation {
    db: Connection,
    window_ms: i64,
}
fn hashes(lines: &[String]) -> Vec<[u8; 32]> {
    // Ignore whitespace and braces-only lines; shared scaffolding is not a reversal.
    lines
        .iter()
        .map(|s| s.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|s| s.chars().any(|c| c.is_alphanumeric()))
        .map(|s| digest(s.as_bytes()))
        .collect()
}
impl Oscillation {
    pub fn remembered(&self, report: &str) -> Result<bool> {
        Ok(self
            .db
            .query_row(
                "SELECT 1 FROM pipeline_merges WHERE report=?1",
                [report],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }
    pub fn open(path: &Path, window_ms: i64) -> Result<Self> {
        if window_ms <= 0 {
            return Err(crate::Error::Invalid("oscillation window"));
        }
        let db = Connection::open(path)?;
        db.busy_timeout(std::time::Duration::from_secs(5))?;
        db.pragma_update(None, "journal_mode", "WAL")?;
        db.execute_batch("CREATE TABLE IF NOT EXISTS pipeline_merges(report TEXT PRIMARY KEY, signature TEXT NOT NULL, area TEXT NOT NULL, time INTEGER NOT NULL);
        CREATE TABLE IF NOT EXISTS pipeline_hunks(report TEXT NOT NULL, path TEXT NOT NULL, removed TEXT NOT NULL, added TEXT NOT NULL);
        CREATE INDEX IF NOT EXISTS pipeline_merge_time ON pipeline_merges(time);")?;
        Ok(Self { db, window_ms })
    }
    /// Call only for merged fixes, with a signature digest rather than report text.
    pub fn remember(
        &mut self,
        report: &str,
        signature: [u8; 32],
        area: Area,
        time_ms: i64,
        diff: &CommitDiff,
    ) -> Result<()> {
        if report.len() > 160 || report.is_empty() || time_ms < 0 {
            return Err(crate::Error::Invalid("merge identity"));
        }
        let tx = self.db.transaction()?;
        tx.execute(
            "INSERT INTO pipeline_merges VALUES(?1,?2,?3,?4)",
            params![
                report,
                crate::hex(&signature),
                serde_json::to_string(&area)?,
                time_ms
            ],
        )?;
        for f in diff.files() {
            tx.execute(
                "INSERT INTO pipeline_hunks VALUES(?1,?2,?3,?4)",
                params![
                    report,
                    f.path,
                    serde_json::to_string(&hashes(&f.removed))?,
                    serde_json::to_string(&hashes(&f.added))?
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn check(
        &self,
        report: &str,
        signature: [u8; 32],
        area: Area,
        now_ms: i64,
        diff: &CommitDiff,
    ) -> Result<OscillationReport> {
        let mut q=self.db.prepare("SELECT m.report,m.signature,m.area,h.path,h.removed,h.added FROM pipeline_merges m JOIN pipeline_hunks h ON h.report=m.report WHERE m.time>=?1")?;
        let rows = q.query_map([now_ms.saturating_sub(self.window_ms)], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
            ))
        })?;
        let mut conflicts = vec![];
        let area = serde_json::to_string(&area)?;
        for row in rows {
            let (old, sig, old_area, path, removed, added) = row?;
            let removed: Vec<[u8; 32]> = serde_json::from_str(&removed)?;
            let added: Vec<[u8; 32]> = serde_json::from_str(&added)?;
            let reversal = diff.files().iter().any(|f| {
                f.path == path
                    && (hashes(&f.removed).iter().any(|h| added.contains(h))
                        || hashes(&f.added).iter().any(|h| removed.contains(h)))
            });
            if reversal {
                conflicts.push(Conflict {
                    previous_report: old.clone(),
                    new_report: report.into(),
                    path: Some(path),
                    reversal: true,
                });
            } else if sig != crate::hex(&signature)
                && area == old_area
                && !conflicts.iter().any(|c| c.previous_report == old)
            {
                conflicts.push(Conflict {
                    previous_report: old,
                    new_report: report.into(),
                    path: None,
                    reversal: false,
                });
            }
        }
        Ok(OscillationReport { conflicts })
    }
}
