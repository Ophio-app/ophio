// SPDX-License-Identifier: Apache-2.0
use crate::{Error, Result};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use std::path::Path;

pub const DAY_MS: i64 = 86_400_000;
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Stage {
    Triage,
    Repro,
    Fix,
    Gates,
    Review,
    Ci,
    Approval,
    Release,
}
impl Stage {
    fn model(self) -> bool {
        matches!(self, Self::Triage | Self::Fix | Self::Review)
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BudgetConfig {
    pub run_attempts: u32,
    pub run_wall_ms: i64,
    pub run_input_tokens: u64,
    pub run_output_tokens: u64,
    pub stage_input_tokens: u64,
    pub stage_output_tokens: u64,
    pub daily_tokens: u64,
    pub daily_ci_runs: u32,
    pub daily_releases: u32,
    pub daily_reporter_runs: u32,
    /// Test submissions checked per UTC day, overall and per reporter key.
    /// A check runs their test twice, at their build and at main.
    pub daily_checks: u32,
    pub daily_reporter_checks: u32,
    /// The most a check's two contained runs may take together.
    pub check_wall_ms: i64,
}
impl Default for BudgetConfig {
    fn default() -> Self {
        Self {
            run_attempts: 6,
            run_wall_ms: 30 * 60 * 1000,
            run_input_tokens: 80_000,
            run_output_tokens: 20_000,
            stage_input_tokens: 12_000,
            stage_output_tokens: 3_000,
            daily_tokens: 200_000,
            daily_ci_runs: 6,
            daily_releases: 1,
            daily_reporter_runs: 2,
            daily_checks: 24,
            daily_reporter_checks: 3,
            check_wall_ms: 25 * 60 * 1000,
        }
    }
}
/// Failure to read the remote switch must stop the pipeline.
pub trait PauseSource {
    fn paused(&self) -> Result<bool>;
}
#[derive(Debug)]
pub struct LocalOnly;
impl PauseSource for LocalOnly {
    fn paused(&self) -> Result<bool> {
        Ok(false)
    }
}
#[derive(Debug, Clone, Copy)]
pub struct Context<'a> {
    pub run_id: &'a str,
    pub reporter_key: &'a str,
    pub now_ms: i64,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum StopReason {
    Paused,
    PauseUnavailable,
    Banned,
    ReporterRuns,
    Attempts,
    WallTime,
    ClockMovedBack,
    RunTokens,
    DailyTokens,
    CiRuns,
    Releases,
    ReservationExceeded,
    /// Today's test submission checks are spent, overall or for a reporter.
    Checks,
    ReporterChecks,
}
#[derive(Debug)]
pub enum Decision {
    Allowed(Ticket),
    Stop(StopReason),
}
/// An atomic reservation, persisted before the stage starts. Dropping a ticket
/// leaves its reservation charged; a crash cannot restore spent quota.
#[derive(Debug)]
pub struct Ticket {
    id: i64,
    run: String,
    input_limit: u64,
    output_limit: u64,
    deadline_ms: i64,
}
impl Ticket {
    pub fn input_limit(&self) -> u64 {
        self.input_limit
    }
    pub fn output_limit(&self) -> u64 {
        self.output_limit
    }
    pub fn deadline_ms(&self) -> i64 {
        self.deadline_ms
    }
}
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}
#[derive(Debug)]
pub struct Budget {
    db: Connection,
    config: BudgetConfig,
}
fn integer(n: u64) -> Result<i64> {
    i64::try_from(n).map_err(|_| Error::Invalid("budget integer overflow"))
}
impl Budget {
    pub fn open(path: &Path, config: BudgetConfig) -> Result<Self> {
        for n in [
            config.run_input_tokens,
            config.run_output_tokens,
            config.stage_input_tokens,
            config.stage_output_tokens,
            config.daily_tokens,
        ] {
            integer(n)?;
        }
        if config.run_wall_ms <= 0
            || !(60_000..=50 * 60 * 1000).contains(&config.check_wall_ms)
            || config
                .stage_input_tokens
                .checked_add(config.stage_output_tokens)
                .is_none_or(|n| n > i64::MAX as u64)
        {
            return Err(Error::Invalid("budget configuration"));
        }
        let db = Connection::open(path)?;
        db.busy_timeout(std::time::Duration::from_secs(5))?;
        db.pragma_update(None, "journal_mode", "WAL")?;
        db.execute_batch("CREATE TABLE IF NOT EXISTS pipeline_runs(id TEXT PRIMARY KEY, reporter TEXT NOT NULL, start INTEGER NOT NULL, last INTEGER NOT NULL, attempts INTEGER NOT NULL, input INTEGER NOT NULL, output INTEGER NOT NULL);
        CREATE TABLE IF NOT EXISTS pipeline_waits(run TEXT PRIMARY KEY, since INTEGER NOT NULL);
        CREATE TABLE IF NOT EXISTS pipeline_days(day INTEGER PRIMARY KEY, tokens INTEGER NOT NULL, ci INTEGER NOT NULL, releases INTEGER NOT NULL);
        CREATE TABLE IF NOT EXISTS pipeline_reporters(day INTEGER NOT NULL, reporter TEXT NOT NULL, runs INTEGER NOT NULL, PRIMARY KEY(day,reporter));
        CREATE TABLE IF NOT EXISTS pipeline_bans(reporter TEXT PRIMARY KEY, reason TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS pipeline_settings(id INTEGER PRIMARY KEY CHECK(id=1), paused INTEGER NOT NULL);
        INSERT OR IGNORE INTO pipeline_settings VALUES(1,0);
        CREATE TABLE IF NOT EXISTS pipeline_check_days(day INTEGER PRIMARY KEY, checks INTEGER NOT NULL);
        CREATE TABLE IF NOT EXISTS pipeline_check_reporters(day INTEGER NOT NULL, reporter TEXT NOT NULL, checks INTEGER NOT NULL, PRIMARY KEY(day,reporter));
        CREATE TABLE IF NOT EXISTS pipeline_checked(report TEXT PRIMARY KEY, deadline INTEGER NOT NULL);
        CREATE TABLE IF NOT EXISTS pipeline_tickets(id INTEGER PRIMARY KEY, run TEXT NOT NULL, day INTEGER NOT NULL, input INTEGER NOT NULL, output INTEGER NOT NULL, recorded INTEGER NOT NULL DEFAULT 0);")?;
        Ok(Self { db, config })
    }
    pub fn set_local_pause(&self, paused: bool) -> Result<()> {
        self.db.execute(
            "UPDATE pipeline_settings SET paused=?1 WHERE id=1",
            [paused],
        )?;
        Ok(())
    }
    /// Only orchestrator-controlled question/CI/owner waiting is excluded from the active execution cap.
    pub fn begin_wait(&self, run: &str, now_ms: i64) -> Result<()> {
        self.db.execute(
            "INSERT OR IGNORE INTO pipeline_waits VALUES(?1,?2)",
            params![run, now_ms],
        )?;
        Ok(())
    }
    pub fn end_wait(&mut self, run: &str, now_ms: i64) -> Result<()> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let since: Option<i64> = tx
            .query_row(
                "SELECT since FROM pipeline_waits WHERE run=?1",
                [run],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(since) = since {
            if now_ms < since {
                return Err(Error::Invalid("wait clock moved back"));
            }
            let start: i64 =
                tx.query_row("SELECT start FROM pipeline_runs WHERE id=?1", [run], |r| {
                    r.get(0)
                })?;
            let start = start
                .checked_add(now_ms - since)
                .ok_or(Error::Invalid("wait overflow"))?;
            tx.execute(
                "UPDATE pipeline_runs SET start=?2,last=?3 WHERE id=?1",
                params![run, start, now_ms],
            )?;
            tx.execute("DELETE FROM pipeline_waits WHERE run=?1", [run])?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn ban(&self, reporter: &str, reason: &str) -> Result<()> {
        if reason.is_empty() || reason.len() > 256 || !crate::schema::safe_text(reason) {
            return Err(Error::Invalid("ban reason"));
        }
        self.db.execute(
            "INSERT OR REPLACE INTO pipeline_bans VALUES(?1,?2)",
            params![reporter, reason],
        )?;
        Ok(())
    }
    pub fn ban_reason(&self, reporter: &str) -> Result<Option<String>> {
        Ok(self
            .db
            .query_row(
                "SELECT reason FROM pipeline_bans WHERE reporter=?1",
                [reporter],
                |r| r.get(0),
            )
            .optional()?)
    }
    pub fn unban(&self, reporter: &str) -> Result<()> {
        self.db
            .execute("DELETE FROM pipeline_bans WHERE reporter=?1", [reporter])?;
        Ok(())
    }
    /// Call before every stage; an allowed decision has already reserved its
    /// maximum model usage and charged attempts, CI, releases and reporter runs.
    pub fn check(
        &mut self,
        stage: Stage,
        ctx: Context<'_>,
        pause: &impl PauseSource,
    ) -> Result<Decision> {
        if ctx.run_id.is_empty()
            || ctx.run_id.len() > 160
            || ctx.reporter_key.is_empty()
            || ctx.reporter_key.len() > 160
            || ctx.now_ms < 0
        {
            return Err(Error::Invalid("budget context"));
        }
        match pause.paused() {
            Ok(true) => return Ok(Decision::Stop(StopReason::Paused)),
            Err(_) => return Ok(Decision::Stop(StopReason::PauseUnavailable)),
            Ok(false) => {}
        }
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let local: bool =
            tx.query_row("SELECT paused FROM pipeline_settings WHERE id=1", [], |r| {
                r.get(0)
            })?;
        if local {
            return Ok(Decision::Stop(StopReason::Paused));
        }
        if tx
            .query_row(
                "SELECT 1 FROM pipeline_bans WHERE reporter=?1",
                [ctx.reporter_key],
                |_| Ok(()),
            )
            .optional()?
            .is_some()
        {
            return Ok(Decision::Stop(StopReason::Banned));
        }
        let day = ctx.now_ms.div_euclid(DAY_MS);
        let run: Option<(String, i64, i64, i64, i64, i64)> = tx
            .query_row(
                "SELECT reporter,start,last,attempts,input,output FROM pipeline_runs WHERE id=?1",
                [ctx.run_id],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                    ))
                },
            )
            .optional()?;
        let new_run = run.is_none();
        let (reporter, start, last, attempts, input, output) =
            run.unwrap_or_else(|| (ctx.reporter_key.into(), ctx.now_ms, ctx.now_ms, 0, 0, 0));
        if reporter != ctx.reporter_key {
            return Err(Error::Invalid("run reporter changed"));
        }
        if ctx.now_ms < last {
            return Ok(Decision::Stop(StopReason::ClockMovedBack));
        }
        let waiting: Option<i64> = tx
            .query_row(
                "SELECT since FROM pipeline_waits WHERE run=?1",
                [ctx.run_id],
                |r| r.get(0),
            )
            .optional()?;
        if waiting.is_some_and(|since| ctx.now_ms < since) {
            return Ok(Decision::Stop(StopReason::ClockMovedBack));
        }
        let effective_start = start.saturating_add(waiting.map_or(0, |since| ctx.now_ms - since));
        if ctx.now_ms - effective_start >= self.config.run_wall_ms {
            return Ok(Decision::Stop(StopReason::WallTime));
        }
        let (reserve_in, reserve_out) = if stage.model() {
            (
                integer(self.config.stage_input_tokens)?,
                integer(self.config.stage_output_tokens)?,
            )
        } else {
            (0, 0)
        };
        if stage.model() && attempts >= i64::from(self.config.run_attempts) {
            return Ok(Decision::Stop(StopReason::Attempts));
        }
        if input
            .checked_add(reserve_in)
            .is_none_or(|n| n > self.config.run_input_tokens as i64)
            || output
                .checked_add(reserve_out)
                .is_none_or(|n| n > self.config.run_output_tokens as i64)
        {
            return Ok(Decision::Stop(StopReason::RunTokens));
        }
        let (tokens, ci, releases): (i64, i64, i64) = tx
            .query_row(
                "SELECT tokens,ci,releases FROM pipeline_days WHERE day=?1",
                [day],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?
            .unwrap_or((0, 0, 0));
        if tokens
            .checked_add(reserve_in + reserve_out)
            .is_none_or(|n| n > self.config.daily_tokens as i64)
        {
            return Ok(Decision::Stop(StopReason::DailyTokens));
        }
        if stage == Stage::Ci && ci >= i64::from(self.config.daily_ci_runs) {
            return Ok(Decision::Stop(StopReason::CiRuns));
        }
        if stage == Stage::Release && releases >= i64::from(self.config.daily_releases) {
            return Ok(Decision::Stop(StopReason::Releases));
        }
        let runs: i64 = tx
            .query_row(
                "SELECT runs FROM pipeline_reporters WHERE day=?1 AND reporter=?2",
                params![day, ctx.reporter_key],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(0);
        if new_run && runs >= i64::from(self.config.daily_reporter_runs) {
            return Ok(Decision::Stop(StopReason::ReporterRuns));
        }
        if new_run {
            tx.execute(
                "INSERT INTO pipeline_runs VALUES(?1,?2,?3,?3,0,0,0)",
                params![ctx.run_id, ctx.reporter_key, ctx.now_ms],
            )?;
            tx.execute("INSERT INTO pipeline_reporters VALUES(?1,?2,1) ON CONFLICT(day,reporter) DO UPDATE SET runs=runs+1",params![day,ctx.reporter_key])?;
        }
        tx.execute("UPDATE pipeline_runs SET last=?2,attempts=attempts+?3,input=input+?4,output=output+?5 WHERE id=?1",params![ctx.run_id,ctx.now_ms,i64::from(stage.model()),reserve_in,reserve_out])?;
        tx.execute("INSERT INTO pipeline_days VALUES(?1,?2,?3,?4) ON CONFLICT(day) DO UPDATE SET tokens=tokens+excluded.tokens,ci=ci+excluded.ci,releases=releases+excluded.releases",params![day,reserve_in+reserve_out,i64::from(stage==Stage::Ci),i64::from(stage==Stage::Release)])?;
        tx.execute(
            "INSERT INTO pipeline_tickets(run,day,input,output) VALUES(?1,?2,?3,?4)",
            params![ctx.run_id, day, reserve_in, reserve_out],
        )?;
        let ticket = Ticket {
            id: tx.last_insert_rowid(),
            run: ctx.run_id.into(),
            input_limit: reserve_in as u64,
            output_limit: reserve_out as u64,
            deadline_ms: effective_start
                .saturating_add(self.config.run_wall_ms)
                .min((day + 1).saturating_mul(DAY_MS)),
        };
        tx.commit()?;
        Ok(Decision::Allowed(ticket))
    }
    /// Whether a test submission check could start now: nothing paused
    /// locally and today's overall checks not spent. Read before claiming,
    /// so a spent day doesn't take submissions it can't check.
    pub fn check_room(&self, now_ms: i64) -> Result<bool> {
        let paused: bool =
            self.db
                .query_row("SELECT paused FROM pipeline_settings WHERE id=1", [], |r| {
                    r.get(0)
                })?;
        let spent: i64 = self
            .db
            .query_row(
                "SELECT checks FROM pipeline_check_days WHERE day=?1",
                [now_ms.div_euclid(DAY_MS)],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(0);
        Ok(!paused && spent < i64::from(self.config.daily_checks))
    }
    /// Charges one check of `report` before any work, against today's
    /// overall and per-reporter caps, and returns the deadline for its two
    /// runs. A report already charged is not charged again, so retrying
    /// after our own trouble costs the reporter nothing; it gets a fresh
    /// deadline. Exhaustion stops without blaming the reporter.
    pub fn reserve_check(
        &mut self,
        report: &str,
        reporter: &str,
        now_ms: i64,
    ) -> Result<std::result::Result<i64, StopReason>> {
        if report.is_empty()
            || report.len() > 160
            || reporter.is_empty()
            || reporter.len() > 160
            || now_ms < 0
        {
            return Err(Error::Invalid("check context"));
        }
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let paused: bool =
            tx.query_row("SELECT paused FROM pipeline_settings WHERE id=1", [], |r| {
                r.get(0)
            })?;
        if paused {
            return Ok(Err(StopReason::Paused));
        }
        if tx
            .query_row(
                "SELECT 1 FROM pipeline_bans WHERE reporter=?1",
                [reporter],
                |_| Ok(()),
            )
            .optional()?
            .is_some()
        {
            return Ok(Err(StopReason::Banned));
        }
        let deadline = now_ms.saturating_add(self.config.check_wall_ms);
        let charged = tx
            .query_row(
                "SELECT 1 FROM pipeline_checked WHERE report=?1",
                [report],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if !charged {
            let day = now_ms.div_euclid(DAY_MS);
            let overall: i64 = tx
                .query_row(
                    "SELECT checks FROM pipeline_check_days WHERE day=?1",
                    [day],
                    |r| r.get(0),
                )
                .optional()?
                .unwrap_or(0);
            if overall >= i64::from(self.config.daily_checks) {
                return Ok(Err(StopReason::Checks));
            }
            let theirs: i64 = tx
                .query_row(
                    "SELECT checks FROM pipeline_check_reporters WHERE day=?1 AND reporter=?2",
                    params![day, reporter],
                    |r| r.get(0),
                )
                .optional()?
                .unwrap_or(0);
            if theirs >= i64::from(self.config.daily_reporter_checks) {
                return Ok(Err(StopReason::ReporterChecks));
            }
            tx.execute("INSERT INTO pipeline_check_days VALUES(?1,1) ON CONFLICT(day) DO UPDATE SET checks=checks+1", [day])?;
            tx.execute("INSERT INTO pipeline_check_reporters VALUES(?1,?2,1) ON CONFLICT(day,reporter) DO UPDATE SET checks=checks+1", params![day, reporter])?;
        }
        tx.execute(
            "INSERT INTO pipeline_checked VALUES(?1,?2) ON CONFLICT(report) DO UPDATE SET deadline=excluded.deadline",
            params![report, deadline],
        )?;
        tx.commit()?;
        Ok(Ok(deadline))
    }
    /// The relay must enforce the ticket's configured per-stage input/output
    /// caps during the stage. An overrun is charged and stops the next action.
    pub fn record(&mut self, ticket: Ticket, usage: Usage) -> Result<Option<StopReason>> {
        let i = integer(usage.input_tokens)?;
        let o = integer(usage.output_tokens)?;
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (day, ri, ro): (i64, i64, i64) = tx.query_row(
            "SELECT day,input,output FROM pipeline_tickets WHERE id=?1 AND run=?2 AND recorded=0",
            params![ticket.id, ticket.run],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        let di = i - ri;
        let do_ = o - ro;
        // SQLite promotes overflowing integer addition to REAL, so cap input
        // before addition rather than silently losing an exact token count.
        let (old_i, old_o): (i64, i64) = tx.query_row(
            "SELECT input,output FROM pipeline_runs WHERE id=?1",
            [&ticket.run],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let old_day: i64 = tx.query_row(
            "SELECT tokens FROM pipeline_days WHERE day=?1",
            [day],
            |r| r.get(0),
        )?;
        let ni = old_i
            .checked_add(di)
            .ok_or(Error::Invalid("token overflow"))?;
        let no = old_o
            .checked_add(do_)
            .ok_or(Error::Invalid("token overflow"))?;
        let nd = old_day
            .checked_add(di)
            .and_then(|n| n.checked_add(do_))
            .ok_or(Error::Invalid("token overflow"))?;
        tx.execute(
            "UPDATE pipeline_runs SET input=?2,output=?3 WHERE id=?1",
            params![ticket.run, ni, no],
        )?;
        tx.execute(
            "UPDATE pipeline_days SET tokens=?2 WHERE day=?1",
            params![day, nd],
        )?;
        tx.execute(
            "UPDATE pipeline_tickets SET recorded=1 WHERE id=?1",
            [ticket.id],
        )?;
        if i > ri || o > ro {
            tx.execute("UPDATE pipeline_settings SET paused=1 WHERE id=1", [])?;
        }
        tx.commit()?;
        Ok((i > ri || o > ro).then_some(StopReason::ReservationExceeded))
    }
}
