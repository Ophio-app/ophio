// SPDX-License-Identifier: Apache-2.0
use crate::{
    Error, Result,
    config::{Bucket, Config},
    maintainer::*,
    validate,
};
use protocol::{ReportId, ReportQuestionId, reports::*, time::Timestamp};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::de::DeserializeOwned;
use std::{path::Path, time::Duration};

#[derive(Debug)]
pub struct Store {
    connection: Connection,
}

fn json<T: serde::Serialize>(value: &T) -> Result<String> {
    Ok(serde_json::to_string(value)?)
}
fn decode<T: DeserializeOwned>(value: String) -> Result<T> {
    Ok(serde_json::from_str(&value)?)
}

impl Store {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .mode(0o600)
                .open(path.as_ref())?;
        }
        let connection = Connection::open(path)?;
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "secure_delete", "ON")?;
        let version: u32 = connection.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version > 4 {
            return Err(Error::Invalid);
        }
        if version == 0 {
            connection.execute_batch(include_str!("migration.sql"))?;
        }
        if version < 2 {
            connection.execute_batch(
                "BEGIN IMMEDIATE;
                ALTER TABLE reports ADD COLUMN completed INTEGER NOT NULL DEFAULT 0;
                CREATE INDEX reports_claimable ON reports(completed,closed,received,id);
                PRAGMA user_version=2;
                COMMIT;",
            )?;
        }
        if version < 3 {
            // Reports from before admission stay where they are until the
            // owner chooses them.
            connection.execute_batch(
                "BEGIN IMMEDIATE;
                ALTER TABLE reports ADD COLUMN admitted INTEGER NOT NULL DEFAULT 0;
                PRAGMA user_version=3;
                COMMIT;",
            )?;
        }
        if version < 4 {
            // Test submissions and what checking them showed. A check is
            // owner-only and leaves with the submission's bundle.
            connection.execute_batch(
                "BEGIN IMMEDIATE;
                ALTER TABLE reports ADD COLUMN test INTEGER NOT NULL DEFAULT 0;
                UPDATE reports SET test=1 WHERE bundle IS NOT NULL AND json_type(bundle,'$.test')='object';
                CREATE INDEX reports_tests ON reports(test,closed,received,id);
                CREATE TABLE checks(report_id TEXT PRIMARY KEY, worker TEXT NOT NULL, lease_until INTEGER NOT NULL, result TEXT, failure_site TEXT);
                CREATE INDEX checks_site ON checks(failure_site);
                PRAGMA user_version=4;
                COMMIT;",
            )?;
        }
        Ok(Self { connection })
    }

    pub(crate) fn authorize(
        &mut self,
        key: &str,
        ip: &str,
        signature: &str,
        signed_at: i64,
        now: i64,
        config: &Config,
    ) -> Result<()> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute("DELETE FROM replays WHERE expires < ?1", [now])?;
        let inserted = tx.execute(
            "INSERT OR IGNORE INTO replays(signature,expires) VALUES(?1,?2)",
            params![signature, signed_at.saturating_add(MAX_CLOCK_SKEW_MS)],
        )?;
        if inserted == 0 {
            return Err(Error::Unauthorized);
        }
        let banned: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM bans WHERE key=?1)",
            [key],
            |r| r.get(0),
        )?;
        let result = if banned {
            Err(Error::Refused)
        } else {
            let key_limit = take(
                &tx,
                &format!("request:key:{key}"),
                config.requests_per_key,
                now,
            )?;
            let ip_limit = take(
                &tx,
                &format!("request:ip:{ip}"),
                config.requests_per_ip,
                now,
            )?;
            limited(key_limit.max(ip_limit))
        };
        tx.commit()?;
        result
    }

    pub(crate) fn reject(&self, reason: &str) -> Result<()> {
        self.connection.execute("INSERT INTO rejections(reason,count) VALUES(?1,1) ON CONFLICT(reason) DO UPDATE SET count=count+1", [reason])?;
        Ok(())
    }

    pub fn switches(&self) -> Result<Switches> {
        decode(self.connection.query_row(
            "SELECT value FROM settings WHERE name='switches'",
            [],
            |r| r.get(0),
        )?)
    }

    pub(crate) fn submit(
        &mut self,
        bundle: ReportBundle,
        key: &str,
        ip: &str,
        config: &Config,
        now: Timestamp,
    ) -> Result<IntakeReceipt> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let switches: Switches = decode(tx.query_row(
            "SELECT value FROM settings WHERE name='switches'",
            [],
            |r| r.get(0),
        )?)?;
        if switches.intake_closed {
            return Err(Error::Closed);
        }
        if let Some(record) = record(&tx, bundle.report_id, false)? {
            if record.key_fingerprint != key {
                return Err(Error::Refused);
            }
            return Ok(receipt(&record));
        }
        let key_limit = take(
            &tx,
            &format!("submit:key:{key}"),
            config.reports_per_key,
            now.0,
        )?;
        let ip_limit = take(
            &tx,
            &format!("submit:ip:{ip}"),
            config.reports_per_ip,
            now.0,
        )?;
        if let Err(error) = limited(key_limit.max(ip_limit)) {
            tx.commit()?;
            return Err(error);
        }
        let signature = bundle.signature.key();
        let known: Option<String> = tx
            .query_row(
                "SELECT issue FROM known WHERE signature=?1",
                [&signature],
                |r| r.get(0),
            )
            .optional()?;
        let status = if let Some(issue) = known {
            let issue: KnownIssue = decode(issue)?;
            if issue.affected_builds.is_empty()
                || issue.affected_builds.contains(&bundle.signature.build)
            {
                ReportProgress::Known {
                    fixed_in: issue.fixed_in,
                }
            } else {
                ReportProgress::Received
            }
        } else {
            ReportProgress::Received
        };
        let parent: Option<(String, String)> = if bundle.signature.groups() && bundle.test.is_none()
        {
            tx.query_row("SELECT id,group_id FROM reports WHERE signature=?1 AND closed IS NULL ORDER BY received,id LIMIT 1", [&signature], |r| Ok((r.get(0)?, r.get(1)?))).optional()?
        } else {
            None
        };
        let duplicate_of: Option<ReportId> = parent
            .as_ref()
            .map(|(id, _)| id.parse().map_err(|_| Error::Invalid))
            .transpose()?;
        let group_id = parent
            .map(|(_, group)| group.parse().map_err(|_| Error::Invalid))
            .transpose()?
            .unwrap_or(bundle.report_id);
        let report = ReportRecord {
            report_id: bundle.report_id,
            key_fingerprint: key.into(),
            signature: bundle.signature.clone(),
            received_at: now,
            updated_at: now,
            history: vec![StatusChange { status, at: now }],
            questions: vec![],
            duplicate_of,
            group_id,
            group_count: 1,
            lease: None,
            bundle: None,
            check: None,
        };
        tx.execute("INSERT INTO reports(id,key,signature,received,updated,status,record,bundle,group_id,test) VALUES(?1,?2,?3,?4,?4,?5,?6,?7,?8,?9)", params![report.report_id.to_string(), key, signature, now.0, json(&report.history[0].status)?, json(&report)?, json(&bundle)?, report.group_id.to_string(), bundle.test.is_some()])?;
        tx.commit()?;
        tracing::info!(report_id = %report.report_id, key_fingerprint = key, verdict = "accepted");
        Ok(receipt(&report))
    }

    pub(crate) fn status(
        &self,
        request: StatusRequest,
        key: &str,
        now: Timestamp,
    ) -> Result<StatusResponse> {
        let mut reports = vec![];
        for id in request.report_ids {
            if let Some(report) = record(&self.connection, id, false)?
                && report.key_fingerprint == key
                && request.since.is_none_or(|since| report.updated_at > since)
            {
                reports.push(update(report));
            }
        }
        Ok(StatusResponse {
            reports,
            now,
            paused: self.switches()?.intake_closed,
        })
    }

    pub(crate) fn answer(
        &mut self,
        answer: IntakeAnswer,
        key: &str,
        now: Timestamp,
    ) -> Result<StatusResponse> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut report = owned(&tx, answer.report_id, key)?;
        if report.history.last().is_some_and(|c| c.status.closed()) {
            return Err(Error::Refused);
        }
        answer_question(&tx, &mut report, answer.question_id, answer.answer, now)?;
        tx.commit()?;
        self.status(
            StatusRequest {
                report_ids: vec![answer.report_id],
                since: None,
            },
            key,
            now,
        )
    }

    pub(crate) fn withdraw(
        &mut self,
        id: ReportId,
        key: &str,
        now: Timestamp,
    ) -> Result<StatusResponse> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut report = owned(&tx, id, key)?;
        withdraw(&tx, &mut report, now)?;
        tx.commit()?;
        self.status(
            StatusRequest {
                report_ids: vec![id],
                since: None,
            },
            key,
            now,
        )
    }

    pub fn get(&self, id: ReportId) -> Result<ReportRecord> {
        record(&self.connection, id, true)?.ok_or(Error::NotFound)
    }

    pub fn list(&self, filter: ReportFilter) -> Result<ReportPage> {
        let status = filter.status.as_ref().map(json).transpose()?;
        let (after_time, after_id) = filter
            .after
            .map(|c| (c.received_at.0, c.report_id.to_string()))
            .unwrap_or((i64::MIN, String::new()));
        let limit = filter.limit.unwrap_or(100).clamp(1, MAX_LISTED_REPORTS);
        let mut statement = self.connection.prepare("SELECT id FROM reports WHERE (?1 IS NULL OR status=?1) AND (?2 IS NULL OR signature=?2) AND (?3 IS NULL OR updated>?3) AND (received>?4 OR (received=?4 AND id>?5)) AND (?7 IS NULL OR id IN (SELECT report_id FROM checks WHERE failure_site=?7 AND result IS NOT NULL)) ORDER BY received,id LIMIT ?6")?;
        let ids = statement
            .query_map(
                params![
                    status,
                    filter.signature,
                    filter.since.map(|s| s.0),
                    after_time,
                    after_id,
                    (limit + 1) as i64,
                    filter.failure_site
                ],
                |r| r.get::<_, String>(0),
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let more = ids.len() > limit;
        let reports: Vec<_> = ids
            .into_iter()
            .take(limit)
            .map(|id| {
                record(
                    &self.connection,
                    id.parse().map_err(|_| Error::Invalid)?,
                    false,
                )?
                .ok_or(Error::NotFound)
            })
            .collect::<Result<_>>()?;
        let next = if more {
            reports.last().map(|r| ReportCursor {
                received_at: r.received_at,
                report_id: r.report_id,
            })
        } else {
            None
        };
        Ok(ReportPage { reports, next })
    }

    pub fn releases(&self, now: Timestamp) -> Result<ReleaseFeed> {
        let mut statement = self
            .connection
            .prepare("SELECT entry FROM releases ORDER BY published DESC,build DESC")?;
        let values = statement
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(ReleaseFeed {
            releases: values.into_iter().map(decode).collect::<Result<_>>()?,
            now,
        })
    }

    pub fn known_issues(&self) -> Result<Vec<KnownIssue>> {
        let mut statement = self
            .connection
            .prepare("SELECT issue FROM known ORDER BY signature")?;
        let values = statement
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        values.into_iter().map(decode).collect()
    }

    /// Execute exactly the same audited operation used by the HTTP API and CLI.
    pub fn command(&mut self, command: Command, now: Timestamp) -> Result<CommandResult> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (operation, target) = audit_label(&command);
        let resume = matches!(command, Command::ResumeLease { .. });
        let excluded = match &command {
            Command::ClaimNext { excluding, .. } if excluding.len() <= 10000 => json(excluding)?,
            Command::ClaimNext { .. } => return Err(Error::Invalid),
            _ => "[]".into(),
        };
        let mut result = CommandResult::Done;
        match command {
            Command::Claim { worker, lease_ms }
            | Command::ClaimNext {
                worker, lease_ms, ..
            } => {
                validate::code(&worker)?;
                if worker.is_empty() || !(1..=3_600_000).contains(&lease_ms) {
                    return Err(Error::Invalid);
                }
                let switches: Switches = decode(tx.query_row(
                    "SELECT value FROM settings WHERE name='switches'",
                    [],
                    |r| r.get(0),
                )?)?;
                let id: Option<String> = if switches.pipeline_paused {
                    None
                } else {
                    tx.query_row("SELECT id FROM reports WHERE completed=0 AND admitted=1 AND closed IS NULL AND bundle IS NOT NULL AND id NOT IN (SELECT value FROM json_each(?2)) AND (lease_until IS NULL OR lease_until<=?1) ORDER BY received,id LIMIT 1", params![now.0,excluded], |r| r.get(0)).optional()?
                };
                let report = if let Some(id) = id {
                    let mut report = record(&tx, id.parse().map_err(|_| Error::Invalid)?, true)?
                        .ok_or(Error::NotFound)?;
                    report.lease = Some(Lease {
                        worker,
                        expires_at: Timestamp(now.0.saturating_add(lease_ms as i64)),
                    });
                    // Claims are private bookkeeping; they do not change reporter cursors.
                    persist(&tx, &report)?;
                    Some(Box::new(report))
                } else {
                    None
                };
                result = CommandResult::Claimed { report };
            }
            Command::ClaimCheck { worker, lease_ms } => {
                validate::code(&worker)?;
                if worker.is_empty() || !(1..=MAX_CHECK_LEASE_MS).contains(&lease_ms) {
                    return Err(Error::Invalid);
                }
                let switches: Switches = decode(tx.query_row(
                    "SELECT value FROM settings WHERE name='switches'",
                    [],
                    |r| r.get(0),
                )?)?;
                // Independent of admission and of the fixer's own lease.
                let id: Option<String> = if switches.pipeline_paused {
                    None
                } else {
                    tx.query_row("SELECT r.id FROM reports r LEFT JOIN checks c ON c.report_id=r.id WHERE r.test=1 AND r.closed IS NULL AND r.bundle IS NOT NULL AND (c.report_id IS NULL OR (c.result IS NULL AND c.lease_until<=?1)) ORDER BY r.received,r.id LIMIT 1", [now.0], |r| r.get(0)).optional()?
                };
                let report = if let Some(id) = id {
                    tx.execute("INSERT INTO checks(report_id,worker,lease_until) VALUES(?1,?2,?3) ON CONFLICT(report_id) DO UPDATE SET worker=excluded.worker,lease_until=excluded.lease_until", params![id, worker, now.0.saturating_add(lease_ms as i64)])?;
                    Some(Box::new(
                        record(&tx, id.parse().map_err(|_| Error::Invalid)?, true)?
                            .ok_or(Error::NotFound)?,
                    ))
                } else {
                    None
                };
                result = CommandResult::Claimed { report };
            }
            Command::RecordCheck {
                report_id,
                worker,
                check,
            } => {
                validate::check(&check)?;
                let lease: Option<(String, i64, Option<String>)> = tx
                    .query_row(
                        "SELECT c.worker,c.lease_until,c.result FROM checks c JOIN reports r ON r.id=c.report_id WHERE c.report_id=?1 AND r.bundle IS NOT NULL",
                        [report_id.to_string()],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                    )
                    .optional()?;
                let Some((holder, until, recorded)) = lease else {
                    record(&tx, report_id, false)?.ok_or(Error::NotFound)?;
                    return Err(Error::Refused);
                };
                if holder != worker {
                    return Err(Error::Refused);
                }
                match recorded {
                    // Recording the same check twice changes nothing.
                    Some(existing) if serde_json::from_str::<TestCheck>(&existing)? == check => {}
                    Some(_) => return Err(Error::Refused),
                    None if until <= now.0 => return Err(Error::Refused),
                    None => {
                        tx.execute(
                            "UPDATE checks SET result=?2,failure_site=?3 WHERE report_id=?1",
                            params![report_id.to_string(), json(&check)?, check.failure_site],
                        )?;
                    }
                }
            }
            Command::DeferCheck {
                report_id,
                worker,
                retry_ms,
            } => {
                if !(1..=MAX_CHECK_DEFER_MS).contains(&retry_ms) {
                    return Err(Error::Invalid);
                }
                let changed = tx.execute(
                    "UPDATE checks SET lease_until=?3 WHERE report_id=?1 AND worker=?2 AND result IS NULL AND lease_until>?4",
                    params![
                        report_id.to_string(),
                        worker,
                        now.0.saturating_add(retry_ms as i64),
                        now.0
                    ],
                )?;
                if changed == 0 {
                    record(&tx, report_id, false)?.ok_or(Error::NotFound)?;
                    return Err(Error::Refused);
                }
            }
            Command::FinishReport { report_id, worker } => {
                let mut report = record(&tx, report_id, false)?.ok_or(Error::NotFound)?;
                if report
                    .lease
                    .as_ref()
                    .is_none_or(|lease| lease.worker != worker)
                {
                    return Err(Error::Refused);
                }
                report.questions.retain(|q| q.answer.is_some());
                if report
                    .history
                    .last()
                    .is_some_and(|s| s.status == ReportProgress::Question)
                {
                    change(
                        &mut report,
                        ReportProgress::Declined {
                            reason: DeclineReason::NotEnoughInformation,
                        },
                        now,
                    );
                }
                save(&tx, &mut report, now)?;
                tx.execute(
                    "UPDATE reports SET completed=1 WHERE id=?1",
                    [report_id.to_string()],
                )?;
            }
            Command::Renew {
                report_id,
                worker,
                lease_ms,
            }
            | Command::ResumeLease {
                report_id,
                worker,
                lease_ms,
            } => {
                validate::code(&worker)?;
                if !(1..=3_600_000).contains(&lease_ms) {
                    return Err(Error::Invalid);
                }
                let mut report = record(&tx, report_id, false)?.ok_or(Error::NotFound)?;
                let lease = report.lease.as_mut().ok_or(Error::Refused)?;
                if lease.worker != worker || (!resume && lease.expires_at.0 <= now.0) {
                    return Err(Error::Refused);
                }
                lease.expires_at = Timestamp(now.0.saturating_add(lease_ms as i64));
                persist(&tx, &report)?;
            }
            Command::Admit { report_id } => {
                // Only an open report the fixer hasn't finished can be chosen.
                let admitted = tx.execute(
                    "UPDATE reports SET admitted=1 WHERE id=?1 AND closed IS NULL AND completed=0",
                    [report_id.to_string()],
                )?;
                if admitted == 0 {
                    record(&tx, report_id, false)?.ok_or(Error::NotFound)?;
                    return Err(Error::Refused);
                }
            }
            Command::SetStatus { report_id, status } => {
                validate::progress(&status)?;
                let mut report = record(&tx, report_id, false)?.ok_or(Error::NotFound)?;
                if matches!(
                    report.history.last().map(|s| &s.status),
                    Some(ReportProgress::Withdrawn)
                ) {
                    return Err(Error::Refused);
                }
                if status == ReportProgress::Withdrawn {
                    withdraw(&tx, &mut report, now)?;
                } else {
                    change(&mut report, status, now);
                    save(&tx, &mut report, now)?;
                }
            }
            Command::Ask {
                report_id,
                template,
            } => {
                validate::question(&template)?;
                let mut report = record(&tx, report_id, false)?.ok_or(Error::NotFound)?;
                if report.history.last().is_some_and(|s| s.status.closed()) {
                    return Err(Error::Refused);
                }
                let question = ReportQuestion {
                    question_id: ReportQuestionId::new(),
                    template,
                    asked_at: now,
                    answer: None,
                };
                report.questions.push(question.clone());
                change(&mut report, ReportProgress::Question, now);
                save(&tx, &mut report, now)?;
                result = CommandResult::Question { question };
            }
            Command::CloseQuestion {
                report_id,
                question_id,
            } => {
                let mut report = record(&tx, report_id, false)?.ok_or(Error::NotFound)?;
                let at = report
                    .questions
                    .iter()
                    .position(|q| q.question_id == question_id && q.answer.is_none())
                    .ok_or(Error::NotFound)?;
                report.questions.remove(at);
                if report.questions.iter().all(|q| q.answer.is_some())
                    && report
                        .history
                        .last()
                        .is_some_and(|s| s.status == ReportProgress::Question)
                {
                    change(&mut report, ReportProgress::Received, now);
                }
                save(&tx, &mut report, now)?;
            }
            Command::AnswerQuestion {
                report_id,
                question_id,
                answer,
            } => {
                let mut report = record(&tx, report_id, false)?.ok_or(Error::NotFound)?;
                answer_question(&tx, &mut report, question_id, answer, now)?;
            }
            Command::PutKnown { issue } => {
                validate::signature_key(&issue.signature)?;
                for build in &issue.affected_builds {
                    validate::code(build)?;
                }
                if let Some(build) = &issue.fixed_in {
                    validate::code(build)?;
                }
                tx.execute("INSERT INTO known(signature,issue) VALUES(?1,?2) ON CONFLICT(signature) DO UPDATE SET issue=excluded.issue", params![issue.signature,json(&issue)?])?;
            }
            Command::DeleteKnown { signature } => {
                tx.execute("DELETE FROM known WHERE signature=?1", [signature])?;
            }
            Command::Publish { mut release } => {
                validate::code(&release.build)?;
                for fix in &release.fixes {
                    validate::signature_key(fix)?;
                }
                if release.state == ReleaseState::Complete && release.ring != ReleaseRing::Everyone
                {
                    return Err(Error::Invalid);
                }
                if let Some(raw) = tx
                    .query_row(
                        "SELECT entry FROM releases WHERE build=?1",
                        [&release.build],
                        |r| r.get::<_, String>(0),
                    )
                    .optional()?
                {
                    let existing: ReleaseEntry = decode(raw)?;
                    if existing.state != ReleaseState::Rolling
                        || existing.ring != ReleaseRing::Owner
                        || release.state != ReleaseState::Rolling
                        || release.ring != ReleaseRing::Owner
                    {
                        return Err(Error::Refused);
                    }
                    release.fixes.extend(existing.fixes);
                    release.fixes.sort();
                    release.fixes.dedup();
                    release.published_at = existing.published_at;
                }
                tx.execute(
                    "INSERT INTO releases(build,published,entry) VALUES(?1,?2,?3) ON CONFLICT(build) DO UPDATE SET entry=excluded.entry",
                    params![release.build, release.published_at.0, json(&release)?],
                )?;
                apply_release(&tx, &release, now)?;
            }
            Command::Widen { build, ring } => {
                let mut release = release(&tx, &build)?;
                if ring < release.ring || release.state != ReleaseState::Rolling {
                    return Err(Error::Invalid);
                }
                release.ring = ring;
                save_release(&tx, &release, now)?;
            }
            Command::Halt { build } => {
                let mut release = release(&tx, &build)?;
                release.state = ReleaseState::Halted;
                save_release(&tx, &release, now)?;
            }
            Command::Complete { build } => {
                let mut release = release(&tx, &build)?;
                if release.state == ReleaseState::Halted {
                    return Err(Error::Refused);
                }
                release.ring = ReleaseRing::Everyone;
                release.state = ReleaseState::Complete;
                save_release(&tx, &release, now)?;
            }
            Command::Sweep { retention_days } => {
                result = CommandResult::Swept {
                    reports: sweep(&tx, now, retention_days)?,
                };
            }
            Command::PipelinePaused { paused } => set_switch(&tx, "pipeline_paused", paused)?,
            Command::IntakeClosed { closed } => set_switch(&tx, "intake_closed", closed)?,
            Command::Ban {
                key_fingerprint,
                banned,
            } => {
                if key_fingerprint.len() != 64
                    || !key_fingerprint
                        .bytes()
                        .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
                {
                    return Err(Error::Invalid);
                }
                if banned {
                    tx.execute(
                        "INSERT OR IGNORE INTO bans(key) VALUES(?1)",
                        [key_fingerprint],
                    )?;
                } else {
                    tx.execute("DELETE FROM bans WHERE key=?1", [key_fingerprint])?;
                }
            }
        }
        // Never audit request bodies: templates, statuses and reports may grow.
        tx.execute(
            "INSERT INTO audit(at,operation,target) VALUES(?1,?2,?3)",
            params![now.0, operation, target],
        )?;
        tx.commit()?;
        Ok(result)
    }

    pub fn counters(&self) -> Result<Counters> {
        fn counts(
            connection: &Connection,
            sql: &str,
        ) -> Result<std::collections::BTreeMap<String, u64>> {
            let mut statement = connection.prepare(sql)?;
            Ok(statement
                .query_map([], |r| Ok((r.get(0)?, r.get::<_, i64>(1)? as u64)))?
                .collect::<std::result::Result<_, _>>()?)
        }
        Ok(Counters {
            reports_per_day: counts(
                &self.connection,
                "SELECT strftime('%Y-%m-%d',received/1000,'unixepoch'),count(*) FROM reports GROUP BY 1",
            )?,
            reports_per_key: counts(
                &self.connection,
                "SELECT key,count(*) FROM reports GROUP BY key",
            )?,
            rejections: counts(&self.connection, "SELECT reason,count FROM rejections")?,
            audit_rows: self
                .connection
                .query_row("SELECT count(*) FROM audit", [], |r| r.get::<_, i64>(0))?
                as u64,
        })
    }

    pub fn retention_sweep(&mut self, now: Timestamp, days: u32) -> Result<usize> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let removed = sweep(&tx, now, days)?;
        tx.commit()?;
        Ok(removed)
    }
}

fn sweep(connection: &Connection, now: Timestamp, days: u32) -> Result<usize> {
    let cutoff = now.0.saturating_sub(i64::from(days) * 86_400_000);
    let ids = {
        let mut statement = connection.prepare("SELECT id FROM reports WHERE closed IS NOT NULL AND closed<=?1 AND (bundle IS NOT NULL OR EXISTS(SELECT 1 FROM json_tree(record,'$.questions') WHERE key='words' AND type='text'))")?;
        statement
            .query_map([cutoff], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?
    };
    for id in &ids {
        let mut report = record(connection, id.parse().map_err(|_| Error::Invalid)?, false)?
            .ok_or(Error::NotFound)?;
        for question in &mut report.questions {
            if let Some(answer) = &mut question.answer {
                answer.words = None;
            }
        }
        connection.execute("UPDATE reports SET bundle=NULL WHERE id=?1", [id])?;
        connection.execute("DELETE FROM checks WHERE report_id=?1", [id])?;
        persist(connection, &report)?;
    }
    connection.execute("DELETE FROM replays WHERE expires<?1", [now.0])?;
    // Forget buckets only once their configured refill has made them full.
    connection.execute("DELETE FROM buckets WHERE expires<?1", [now.0])?;
    Ok(ids.len())
}

fn record(
    connection: &Connection,
    id: ReportId,
    with_bundle: bool,
) -> Result<Option<ReportRecord>> {
    let row: Option<(String, Option<String>)> = connection
        .query_row(
            "SELECT record, CASE WHEN ?2 THEN bundle ELSE NULL END FROM reports WHERE id=?1",
            params![id.to_string(), with_bundle],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    row.map(|(value, bundle)| {
        let mut record: ReportRecord = decode(value)?;
        record.bundle = bundle.map(decode).transpose()?;
        record.check = connection
            .query_row(
                "SELECT result FROM checks WHERE report_id=?1 AND result IS NOT NULL",
                [id.to_string()],
                |r| r.get::<_, String>(0),
            )
            .optional()?
            .map(decode)
            .transpose()?;
        record.group_count = connection.query_row(
            "SELECT count(*) FROM reports WHERE group_id=?1",
            [record.group_id.to_string()],
            |r| r.get::<_, i64>(0),
        )? as u64;
        Ok(record)
    })
    .transpose()
}
fn owned(connection: &Connection, id: ReportId, key: &str) -> Result<ReportRecord> {
    record(connection, id, false)?
        .filter(|r| r.key_fingerprint == key)
        .ok_or(Error::NotFound)
}
fn receipt(report: &ReportRecord) -> IntakeReceipt {
    IntakeReceipt {
        report_id: report.report_id,
        reference: report_reference(&report.report_id),
        status: report
            .history
            .last()
            .map(|s| s.status.clone())
            .unwrap_or(ReportProgress::Received),
    }
}
fn update(report: ReportRecord) -> ReportUpdate {
    ReportUpdate {
        report_id: report.report_id,
        history: report.history,
        questions: report.questions,
    }
}
fn change(report: &mut ReportRecord, status: ReportProgress, now: Timestamp) {
    if report.history.last().is_none_or(|s| s.status != status) {
        report.history.push(StatusChange { status, at: now });
    }
}
fn persist(connection: &Connection, report: &ReportRecord) -> Result<()> {
    let mut metadata = report.clone();
    metadata.bundle = None;
    // Checks live in their own table.
    metadata.check = None;
    let status = metadata.history.last().ok_or(Error::Invalid)?;
    connection.execute("UPDATE reports SET record=?2,status=?3,updated=?4,closed=CASE WHEN ?5 THEN coalesce(closed,?4) ELSE NULL END,lease_until=?6,signature=?7,group_id=?8 WHERE id=?1", params![report.report_id.to_string(),json(&metadata)?,json(&status.status)?,report.updated_at.0,status.status.closed(),report.lease.as_ref().map(|l|l.expires_at.0),report.signature.key(),report.group_id.to_string()])?;
    Ok(())
}
fn save(connection: &Connection, report: &mut ReportRecord, now: Timestamp) -> Result<()> {
    report.updated_at = Timestamp(now.0.max(report.updated_at.0.saturating_add(1)));
    persist(connection, report)
}
fn withdraw(connection: &Connection, report: &mut ReportRecord, now: Timestamp) -> Result<()> {
    report.bundle = None;
    report.check = None;
    report.questions.clear();
    report.lease = None;
    report.duplicate_of = None;
    report.group_id = report.report_id;
    report.signature = ReportSignature {
        code: String::new(),
        screen: String::new(),
        build: String::new(),
    };
    report.history = vec![StatusChange {
        status: ReportProgress::Withdrawn,
        at: now,
    }];
    connection.execute(
        "UPDATE reports SET bundle=NULL WHERE id=?1",
        [report.report_id.to_string()],
    )?;
    connection.execute(
        "DELETE FROM checks WHERE report_id=?1",
        [report.report_id.to_string()],
    )?;
    save(connection, report, now)
}
fn limited(seconds: u64) -> Result<()> {
    if seconds > 0 {
        Err(Error::Limited(seconds))
    } else {
        Ok(())
    }
}
fn take(connection: &Connection, id: &str, bucket: Bucket, now: i64) -> Result<u64> {
    let previous: Option<(f64, i64)> = connection
        .query_row("SELECT tokens,at FROM buckets WHERE id=?1", [id], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .optional()?;
    let (tokens, at) = previous.unwrap_or((f64::from(bucket.capacity), now));
    let now = now.max(at);
    let tokens = (tokens
        + (now.saturating_sub(at).max(0) as f64 / (f64::from(bucket.refill_seconds) * 1000.0)))
        .min(f64::from(bucket.capacity));
    let (remaining, retry) = if tokens >= 1.0 {
        (tokens - 1.0, 0)
    } else {
        (
            tokens,
            ((1.0 - tokens) * f64::from(bucket.refill_seconds))
                .ceil()
                .max(1.0) as u64,
        )
    };
    let expires = now.saturating_add(
        i64::from(bucket.capacity)
            .saturating_mul(i64::from(bucket.refill_seconds))
            .saturating_mul(1000),
    );
    connection.execute("INSERT INTO buckets(id,tokens,at,expires) VALUES(?1,?2,?3,?4) ON CONFLICT(id) DO UPDATE SET tokens=excluded.tokens,at=excluded.at,expires=excluded.expires", params![id,remaining,now,expires])?;
    Ok(retry)
}
fn set_switch(connection: &Connection, field: &str, value: bool) -> Result<()> {
    let mut switches: Switches = decode(connection.query_row(
        "SELECT value FROM settings WHERE name='switches'",
        [],
        |r| r.get(0),
    )?)?;
    match field {
        "pipeline_paused" => switches.pipeline_paused = value,
        _ => switches.intake_closed = value,
    }
    connection.execute(
        "UPDATE settings SET value=?1 WHERE name='switches'",
        [json(&switches)?],
    )?;
    Ok(())
}
fn release(connection: &Connection, build: &str) -> Result<ReleaseEntry> {
    decode(
        connection
            .query_row("SELECT entry FROM releases WHERE build=?1", [build], |r| {
                r.get(0)
            })
            .optional()?
            .ok_or(Error::NotFound)?,
    )
}
fn save_release(connection: &Connection, release: &ReleaseEntry, now: Timestamp) -> Result<()> {
    connection.execute(
        "UPDATE releases SET entry=?2 WHERE build=?1",
        params![release.build, json(release)?],
    )?;
    apply_release(connection, release, now)
}
fn apply_release(connection: &Connection, release: &ReleaseEntry, now: Timestamp) -> Result<()> {
    if release.state == ReleaseState::Halted {
        return Ok(());
    }
    for signature in &release.fixes {
        let ids = {
            let mut statement = connection
                .prepare("SELECT id FROM reports WHERE signature=?1 AND closed IS NULL")?;
            statement
                .query_map([signature], |r| r.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?
        };
        for id in ids {
            let mut report = record(connection, id.parse().map_err(|_| Error::Invalid)?, false)?
                .ok_or(Error::NotFound)?;
            let status = if release.ring == ReleaseRing::Everyone {
                ReportProgress::Shipped {
                    build: release.build.clone(),
                }
            } else {
                ReportProgress::Fixed {
                    build: release.build.clone(),
                }
            };
            change(&mut report, status, now);
            save(connection, &mut report, now)?;
        }
    }
    Ok(())
}
fn answer_question(
    connection: &Connection,
    report: &mut ReportRecord,
    question_id: ReportQuestionId,
    mut answer: ReportAnswer,
    now: Timestamp,
) -> Result<()> {
    if report.history.last().is_some_and(|c| c.status.closed()) {
        return Err(Error::Refused);
    }
    let question = report
        .questions
        .iter_mut()
        .find(|q| q.question_id == question_id && q.answer.is_none())
        .ok_or(Error::NotFound)?;
    if !question.template.choices().contains(&answer.choice) {
        return Err(Error::Invalid);
    }
    answer.words = answer
        .words
        .map(|s| s.chars().take(MAX_REPORT_WORDS).collect());
    answer.at = now;
    question.answer = Some(answer);
    if report.questions.iter().all(|q| q.answer.is_some()) {
        change(report, ReportProgress::Received, now);
    }
    save(connection, report, now)?;
    Ok(())
}
fn audit_label(command: &Command) -> (&'static str, String) {
    match command {
        Command::Claim { .. } => ("claim", String::new()),
        Command::ClaimNext { .. } => ("claim", String::new()),
        Command::FinishReport { report_id, .. } => ("finish_report", report_id.to_string()),
        Command::ClaimCheck { .. } => ("claim_check", String::new()),
        Command::RecordCheck { report_id, .. } => ("record_check", report_id.to_string()),
        Command::DeferCheck { report_id, .. } => ("defer_check", report_id.to_string()),
        Command::Renew { report_id, .. } => ("renew", report_id.to_string()),
        Command::ResumeLease { report_id, .. } => ("resume_lease", report_id.to_string()),
        Command::Sweep { .. } => ("retention_sweep", String::new()),
        Command::Admit { report_id } => ("admit", report_id.to_string()),
        Command::SetStatus { report_id, .. } => ("set_status", report_id.to_string()),
        Command::Ask { report_id, .. } => ("ask", report_id.to_string()),
        Command::AnswerQuestion { report_id, .. } => ("answer_question", report_id.to_string()),
        Command::CloseQuestion { report_id, .. } => ("close_question", report_id.to_string()),
        Command::PutKnown { .. } => ("put_known", String::new()),
        Command::DeleteKnown { .. } => ("delete_known", String::new()),
        Command::Publish { .. } => ("publish", String::new()),
        Command::Widen { .. } => ("widen", String::new()),
        Command::Halt { .. } => ("halt", String::new()),
        Command::Complete { .. } => ("complete", String::new()),
        Command::PipelinePaused { .. } => ("pipeline_paused", String::new()),
        Command::IntakeClosed { .. } => ("intake_closed", String::new()),
        Command::Ban {
            key_fingerprint, ..
        } => ("ban", key_fingerprint.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn existing_databases_gain_durable_claim_completion() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.sqlite");
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(include_str!("migration.sql"))
            .unwrap();
        drop(connection);
        let store = Store::open(&path).unwrap();
        assert_eq!(
            store
                .connection
                .pragma_query_value(None, "user_version", |r| r.get::<_, u32>(0))
                .unwrap(),
            4
        );
        store
            .connection
            .prepare("SELECT completed, admitted, test FROM reports")
            .unwrap();
        store
            .connection
            .prepare("SELECT report_id, worker, lease_until, result, failure_site FROM checks")
            .unwrap();
        drop(store);
        Store::open(path).unwrap();
    }

    #[test]
    fn an_older_database_marks_its_test_submissions_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.sqlite");
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(include_str!("migration.sql"))
            .unwrap();
        connection
            .execute_batch(
                "ALTER TABLE reports ADD COLUMN completed INTEGER NOT NULL DEFAULT 0;
                ALTER TABLE reports ADD COLUMN admitted INTEGER NOT NULL DEFAULT 0;
                PRAGMA user_version=3;",
            )
            .unwrap();
        for (id, bundle) in [
            ("a", Some(r#"{"test":{"path":"x"}}"#)),
            ("b", Some(r#"{"words":"plain"}"#)),
            ("c", None),
        ] {
            connection
                .execute(
                    "INSERT INTO reports(id,key,signature,received,updated,status,record,bundle,group_id) VALUES(?1,'k','s',1,1,'\"received\"','{}',?2,?1)",
                    params![id, bundle],
                )
                .unwrap();
        }
        drop(connection);
        let store = Store::open(&path).unwrap();
        let marked: Vec<String> = store
            .connection
            .prepare("SELECT id FROM reports WHERE test=1")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        assert_eq!(marked, ["a"]);
    }

    #[test]
    fn replay_expiry_uses_the_signed_time_and_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.sqlite");
        let config = Config::default();
        let time = 1_000_000;
        {
            let mut store = Store::open(&path).unwrap();
            store
                .authorize(
                    "synthetic-key",
                    "synthetic-ip",
                    "synthetic-signature",
                    time,
                    time,
                    &config,
                )
                .unwrap();
        }
        let mut store = Store::open(&path).unwrap();
        assert!(matches!(
            store.authorize(
                "synthetic-key",
                "synthetic-ip",
                "synthetic-signature",
                time,
                time + MAX_CLOCK_SKEW_MS,
                &config
            ),
            Err(Error::Unauthorized)
        ));
        // The caller checks the timestamp; this checks cleanup of expired rows.
        store
            .authorize(
                "synthetic-key",
                "synthetic-ip",
                "another-signature",
                time + MAX_CLOCK_SKEW_MS + 1,
                time + MAX_CLOCK_SKEW_MS + 1,
                &config,
            )
            .unwrap();
        let old: i64 = store
            .connection
            .query_row(
                "SELECT count(*) FROM replays WHERE signature='synthetic-signature'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(old, 0);
    }

    #[test]
    fn buckets_refill_and_remain_limited_after_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.sqlite");
        let bucket = Bucket {
            capacity: 2,
            refill_seconds: 10,
        };
        {
            let store = Store::open(&path).unwrap();
            assert_eq!(
                take(&store.connection, "synthetic-key", bucket, 1000).unwrap(),
                0
            );
            assert_eq!(
                take(&store.connection, "synthetic-key", bucket, 1000).unwrap(),
                0
            );
            assert_eq!(
                take(&store.connection, "synthetic-key", bucket, 1000).unwrap(),
                10
            );
        }
        let store = Store::open(&path).unwrap();
        assert_eq!(
            take(&store.connection, "synthetic-key", bucket, 0).unwrap(),
            10
        );
        assert_eq!(
            take(&store.connection, "synthetic-key", bucket, 6000).unwrap(),
            5
        );
        assert_eq!(
            take(&store.connection, "synthetic-key", bucket, 11000).unwrap(),
            0
        );
        assert_eq!(
            take(&store.connection, "synthetic-key", bucket, 11000).unwrap(),
            10
        );
    }

    #[test]
    fn future_database_schema_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.sqlite");
        let connection = Connection::open(&path).unwrap();
        connection.pragma_update(None, "user_version", 5).unwrap();
        drop(connection);
        assert!(matches!(Store::open(path), Err(Error::Invalid)));
    }
}
