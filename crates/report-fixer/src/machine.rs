// SPDX-License-Identifier: Apache-2.0
use crate::{repository, *};
use intake::{
    client::Client,
    maintainer::{
        AgentBlock, CheckOutcome, CheckRefusal, MAX_CHECK_DEFER_MS, MAX_CHECK_LEASE_MS,
        NotRunReason, ReportRecord, TestCheck,
    },
};
use protocol::{ReportId, reports::*, time::Timestamp};
use report_pipeline::{
    budget::{self, Budget, BudgetConfig, Context, Decision, Stage, Ticket},
    gates::{self, CommitDiff, GateConfig, GateReport, Outcome, TestEvidence, TestResult},
    honeytoken::Honeytokens,
    oscillation::{DEFAULT_WINDOW_MS, Oscillation},
    provenance::{self, PipelineKey, StopCause, StopStatement},
    render::Candidate,
    schema::{self, Category, ReproTest, ReviewDecision, ReviewVerdict, TriageContext},
    signals::Signal,
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

pub type Stop = StopCause;
/// Why no agent may be shown this submitted test, even reduced.
fn agent_block(test: &ReportTest) -> Option<AgentBlock> {
    let target = schema::TestTarget::new(test.kind, &test.path)?;
    match schema::ReducedTest::new(target, &test.source) {
        Err(_) => Some(AgentBlock::Unreducible),
        Ok(reduced) if !reduced.long_literals().is_empty() => Some(AgentBlock::LongLiteral),
        Ok(_) => None,
    }
}
const STRIKE: &str =
    "INSERT INTO fixer_strikes VALUES(?1,1) ON CONFLICT(reporter) DO UPDATE SET count=count+1";
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum State {
    Triage,
    Question,
    Repro,
    Fix,
    Gates,
    Review,
    Push,
    Ci,
    Approval,
    Merged,
    Release,
    Shipped,
    Stopped,
}
impl State {
    fn stage(self) -> Stage {
        match self {
            Self::Triage | Self::Question => Stage::Triage,
            Self::Repro => Stage::Repro,
            Self::Fix => Stage::Fix,
            Self::Gates => Stage::Gates,
            Self::Review => Stage::Review,
            Self::Push | Self::Ci => Stage::Ci,
            Self::Approval => Stage::Approval,
            Self::Merged => Stage::Gates,
            _ => Stage::Release,
        }
    }
}
/// What one round of the check stage did. Checks run apart from the single
/// active report slot, so a long wait there doesn't hold them up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckStep {
    Idle,
    Paused,
    /// Today's checks are spent; submissions wait unclaimed.
    Spent,
    /// Put off, for our own trouble or a limit, without blame.
    Deferred {
        report: ReportId,
    },
    Checked {
        report: ReportId,
        check: TestCheck,
    },
}
/// One side of a check: its outcome and where it failed, or a hostile signal.
enum Side {
    Ran(CheckOutcome, Option<String>),
    Hostile(Signal),
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    Idle,
    Paused,
    Advanced { report: ReportId, state: State },
    Waiting { report: ReportId, state: State },
    Stopped { report: ReportId, cause: Stop },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Run {
    report: ReportRecord,
    bundle_hash: [u8; 32],
    reference: String,
    state: State,
    busy: bool,
    #[serde(default)]
    admitted: Option<State>,
    base: String,
    fix: Option<String>,
    docs: Vec<String>,
    candidates: Vec<Candidate>,
    triage: Vec<u8>,
    test: Vec<u8>,
    base_tests: Option<TestRun>,
    fix_tests: Option<TestRun>,
    gates: Option<GateReport>,
    review: Option<ReviewVerdict>,
    pr: Option<PullRequest>,
    ci: Option<String>,
    approval: Option<ApprovalView>,
    merged: Option<Merged>,
    conflict: bool,
    stop: Option<Stop>,
    #[serde(default)]
    question: Option<ReportQuestion>,
    #[serde(default)]
    question_deadline_ms: i64,
    #[serde(default)]
    answers: Vec<ReportQuestion>,
    #[serde(default)]
    retry_attempts: u32,
    #[serde(default)]
    retry_at_ms: i64,
    #[serde(default)]
    retired: bool,
}

pub struct Orchestrator {
    _lock: std::fs::File,
    db: Connection,
    budget: Budget,
    oscillation: Oscillation,
    key: PipelineKey,
    intake: Client,
    agents: Arc<dyn AgentRunner>,
    tests: Arc<dyn TestRunner>,
    forge: Arc<dyn Forge>,
    approvals: Arc<dyn Approvals>,
    releases: Arc<dyn Releases>,
    repo: PathBuf,
    scratch: PathBuf,
    worker: String,
    build: Option<RunningBuild>,
    lease_ms: u64,
    recovery: config::RecoveryConfig,
    check_lease_ms: u64,
    check_wall_ms: i64,
    /// Main's tip from the last successful fetch, for a check that can't run.
    last_main: Option<String>,
}
impl std::fmt::Debug for Orchestrator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Orchestrator")
            .field("worker", &self.worker)
            .finish_non_exhaustive()
    }
}
pub struct Dependencies {
    pub intake: Client,
    pub agents: Arc<dyn AgentRunner>,
    pub tests: Arc<dyn TestRunner>,
    pub forge: Arc<dyn Forge>,
    pub approvals: Arc<dyn Approvals>,
    pub releases: Arc<dyn Releases>,
}
impl std::fmt::Debug for Dependencies {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Dependencies").finish_non_exhaustive()
    }
}
impl Orchestrator {
    pub fn open(
        directory: &Path,
        repo: PathBuf,
        worker: String,
        budgets: BudgetConfig,
        deps: Dependencies,
        build: Option<RunningBuild>,
    ) -> Result<Self> {
        std::fs::create_dir_all(directory)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))?;
        }
        let directory = directory.canonicalize()?;
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(directory.join("worker.lock"))?;
        lock.try_lock()
            .map_err(|_| Error::Operation("pipeline already running"))?;
        let db_path = directory.join("pipeline.sqlite");
        let db = Connection::open(&db_path)?;
        db.pragma_update(None, "journal_mode", "WAL")?;
        db.busy_timeout(Duration::from_secs(5))?;
        db.execute_batch("CREATE TABLE IF NOT EXISTS fixer_runs(id TEXT PRIMARY KEY, state TEXT NOT NULL, data BLOB NOT NULL);
            CREATE TABLE IF NOT EXISTS fixer_provenance(id TEXT NOT NULL, kind TEXT NOT NULL, data BLOB NOT NULL, PRIMARY KEY(id,kind));
            CREATE TABLE IF NOT EXISTS fixer_log(seq INTEGER PRIMARY KEY, report TEXT NOT NULL, stage TEXT NOT NULL, cause TEXT NOT NULL, at INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS fixer_strikes(reporter TEXT PRIMARY KEY, count INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS fixer_check_attempts(report TEXT PRIMARY KEY, attempts INTEGER NOT NULL, spent_ms INTEGER NOT NULL DEFAULT 0);
            CREATE TABLE IF NOT EXISTS fixer_check_strikes(report TEXT PRIMARY KEY);")?;
        let check_wall_ms = budgets.check_wall_ms;
        let check_lease_ms = (budgets.check_wall_ms.max(0) as u64)
            .saturating_add(300_000)
            .min(MAX_CHECK_LEASE_MS);
        let scratch = directory.join("scratch");
        std::fs::create_dir_all(&scratch)?;
        // Old stages never run again. No sandbox state is reused across restart.
        for e in std::fs::read_dir(&scratch)? {
            let p = e?.path();
            if p.is_dir() && !std::fs::symlink_metadata(&p)?.file_type().is_symlink() {
                std::fs::remove_dir_all(p)?;
            } else {
                std::fs::remove_file(p)?;
            }
        }
        Ok(Self {
            _lock: lock,
            db,
            budget: Budget::open(&db_path, budgets)?,
            oscillation: Oscillation::open(&db_path, DEFAULT_WINDOW_MS)?,
            key: PipelineKey::load_or_create(&directory)?,
            intake: deps.intake,
            agents: deps.agents,
            tests: deps.tests,
            forge: deps.forge,
            approvals: deps.approvals,
            releases: deps.releases,
            repo,
            scratch,
            worker,
            build,
            lease_ms: 420_000,
            recovery: config::RecoveryConfig::default(),
            check_lease_ms,
            check_wall_ms,
            last_main: None,
        })
    }
    pub fn set_local_pause(&self, value: bool) -> Result<()> {
        Ok(self.budget.set_local_pause(value)?)
    }
    pub fn set_recovery(&mut self, recovery: config::RecoveryConfig) {
        self.lease_ms =
            (u64::from(recovery.retry_max_seconds) * 1000 + 120_000).clamp(120_000, 3_600_000);
        self.recovery = recovery;
    }
    pub fn set_build(&mut self, build: RunningBuild) {
        self.build = Some(build);
    }
    pub fn state(&self, report: ReportId) -> Result<Option<State>> {
        self.db
            .query_row(
                "SELECT data FROM fixer_runs WHERE id=?1",
                [report.to_string()],
                |r| r.get::<_, Vec<u8>>(0),
            )
            .optional()?
            .map(|b| {
                serde_json::from_slice::<Run>(&b)
                    .map(|r| r.state)
                    .map_err(Error::from)
            })
            .transpose()
    }
    fn save(&self, run: &Run) -> Result<()> {
        self.db.execute("INSERT INTO fixer_runs VALUES(?1,?2,?3) ON CONFLICT(id) DO UPDATE SET state=excluded.state,data=excluded.data", params![run.report.report_id.to_string(), format!("{:?}", run.state), serde_json::to_vec(run)?])?;
        Ok(())
    }
    async fn pause(&mut self) -> Result<bool> {
        if self
            .scratch
            .parent()
            .is_some_and(|p| p.join("PAUSED").exists())
        {
            return Ok(true);
        }
        let local: bool =
            self.db
                .query_row("SELECT paused FROM pipeline_settings WHERE id=1", [], |r| {
                    r.get(0)
                })?;
        Ok(local
            || self
                .intake
                .switches()
                .await
                .map_err(Error::polling)?
                .pipeline_paused)
    }
    fn verdict(&self, run: &Run) -> Result<schema::TriageVerdict> {
        schema::parse_triage(
            &run.triage,
            TriageContext {
                docs: run.docs.len(),
                candidates: run.candidates.len(),
            },
        )
        .map_err(|e| Error::Signal(e.signal))
    }
    fn repro(&self, run: &Run) -> Result<ReproTest> {
        let verdict = self.verdict(run)?;
        Ok(ReproTest::validate(
            &self.repo,
            &run.base,
            verdict.repro().ok_or(Error::Operation("missing repro"))?,
            &run.test,
        )?)
    }
    fn diff(&self, run: &Run) -> Result<CommitDiff> {
        Ok(CommitDiff::inspect(
            &self.repo,
            &run.base,
            run.fix.as_deref().ok_or(Error::Operation("missing fix"))?,
        )?)
    }
    fn gate_report<'a>(&self, run: &'a Run) -> Result<&'a GateReport> {
        run.gates.as_ref().ok_or(Error::Operation("missing gates"))
    }
    async fn stop(&mut self, run: &mut Run, cause: Stop) -> Result<Step> {
        let statement = self.key.sign_stop(StopStatement {
            report_id: run.report.report_id,
            bundle_hash: run.bundle_hash,
            stage: run.state.stage(),
            cause: cause.clone(),
            when_ms: Timestamp::now().0,
            artifacts_hash: hash(&serde_json::to_vec(&(
                hash(&run.triage),
                hash(&run.test),
                &run.base,
                &run.fix,
                &run.base_tests,
                &run.fix_tests,
                &run.gates,
                &run.review,
                &run.ci,
                &run.approval,
            ))?),
        })?;
        {
            let tx = self.db.unchecked_transaction()?;
            if let Stop::Signal(signal) = cause {
                let policy = signal.decision();
                if policy.count_against_reporter {
                    tx.execute(STRIKE, [&run.report.key_fingerprint])?;
                }
            }
            tx.execute(
                "INSERT OR REPLACE INTO fixer_provenance VALUES(?1,'stop',?2)",
                params![
                    run.report.report_id.to_string(),
                    serde_json::to_vec(&statement)?
                ],
            )?;
            tx.execute(
                "INSERT INTO fixer_log(report,stage,cause,at) VALUES(?1,?2,?3,?4)",
                params![
                    run.reference,
                    format!("{:?}", run.state),
                    serde_json::to_string(&cause)?,
                    Timestamp::now().0
                ],
            )?;
            run.state = State::Stopped;
            run.busy = false;
            run.stop = Some(cause.clone());
            run.retired = false;
            tx.execute(
                "UPDATE fixer_runs SET state='Stopped',data=?2 WHERE id=?1",
                params![run.report.report_id.to_string(), serde_json::to_vec(run)?],
            )?;
            tx.commit()?;
        }
        self.ban_after_strikes(&run.report.key_fingerprint)?;
        self.retire(run).await?;
        tracing::warn!(reference=%run.reference, cause=?cause, "Report fix stopped");
        Ok(Step::Stopped {
            report: run.report.report_id,
            cause,
        })
    }
    fn ban_after_strikes(&self, reporter: &str) -> Result<()> {
        let strikes: i64 = self
            .db
            .query_row(
                "SELECT count FROM fixer_strikes WHERE reporter=?1",
                [reporter],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(0);
        if strikes >= 3 {
            self.budget
                .ban(reporter, "Repeated hostile pipeline signals")?;
        }
        Ok(())
    }
    async fn retire(&mut self, run: &mut Run) -> Result<()> {
        // Revoke by identity even when a crash happened before the view was saved.
        self.approvals.revoke(run.report.report_id).await?;
        match self
            .intake
            .finish_report(run.report.report_id, &self.worker)
            .await
        {
            Ok(()) | Err(intake::client::Error::Status(403 | 404)) => {}
            Err(error) => return Err(error.into()),
        }
        run.retired = true;
        self.save(run)
    }
    async fn failure(&mut self, run: &mut Run, error: Error) -> Result<Step> {
        if matches!(error, Error::Transient(_)) && !run.busy {
            run.retry_attempts = run.retry_attempts.saturating_add(1);
            if run.retry_attempts <= self.recovery.retry_limit {
                let delay = u64::from(self.recovery.retry_initial_seconds)
                    .saturating_mul(1u64 << (run.retry_attempts - 1).min(63))
                    .min(u64::from(self.recovery.retry_max_seconds));
                run.retry_at_ms = Timestamp::now().0.saturating_add((delay * 1000) as i64);
                self.save(run)?;
                return Ok(Step::Waiting {
                    report: run.report.report_id,
                    state: run.state,
                });
            }
        }
        let cause = match error {
            Error::Signal(s) => Stop::Signal(s),
            // As before timeouts were told apart: spent, never hostile.
            Error::TimedOut => Stop::Signal(Signal::BudgetSpent),
            Error::Safety(ref e) if e.signal().is_some() => {
                Stop::Signal(e.signal().unwrap_or(Signal::GateTampering))
            }
            Error::Operation("lease lost") => Stop::LeaseLost,
            _ => Stop::Infrastructure,
        };
        self.stop(run, cause).await
    }
    /// Advance a single durable stage. Unknown completion is never retried.
    pub async fn step(&mut self) -> Result<Step> {
        // Cleanup runs even while admission is paused, and survives an interrupted stop.
        let cleanup = self.db.query_row(
            "SELECT data FROM fixer_runs WHERE state IN ('Stopped','Shipped','Merged') AND COALESCE(json_extract(data,'$.retired'),0)=0 ORDER BY rowid LIMIT 1",
            [], |r| r.get::<_, Vec<u8>>(0),
        ).optional()?;
        if let Some(bytes) = cleanup {
            let mut run: Run = serde_json::from_slice(&bytes)?;
            self.retire(&mut run).await?;
            return Ok(Step::Waiting {
                report: run.report.report_id,
                state: run.state,
            });
        }
        let mut raw = self.db.query_row("SELECT data FROM fixer_runs WHERE state NOT IN ('Shipped','Stopped','Merged') ORDER BY rowid LIMIT 1", [], |r| r.get::<_, Vec<u8>>(0)).optional()?;
        if raw.is_none()
            && let Some(build) = &self.build
        {
            let merged = self
                .db
                .prepare("SELECT data FROM fixer_runs WHERE state='Merged' ORDER BY rowid")?
                .query_map([], |r| r.get::<_, Vec<u8>>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            for bytes in merged {
                let mut pending: Run = serde_json::from_slice(&bytes)?;
                if pending.retry_at_ms > Timestamp::now().0 {
                    continue;
                }
                if let Some(merge) = &pending.merged {
                    match self.forge.contains(&merge.commit, &build.commit).await {
                        Ok(true) => {
                            raw = Some(bytes);
                            break;
                        }
                        Ok(false) => {
                            pending.retry_attempts = 0;
                            pending.retry_at_ms = 0;
                            self.save(&pending)?;
                        }
                        Err(error) => return self.failure(&mut pending, error).await,
                    }
                }
            }
        }
        let mut run = if let Some(raw) = raw {
            serde_json::from_slice::<Run>(&raw)?
        } else {
            if self.pause().await.unwrap_or(true) {
                return Ok(Step::Paused);
            }
            let Some(report) = self.intake.claim(&self.worker, self.lease_ms).await? else {
                return Ok(Step::Idle);
            };
            if self.state(report.report_id)?.is_some() {
                self.intake
                    .finish_report(report.report_id, &self.worker)
                    .await?;
                return Ok(Step::Idle);
            }
            let bundle = report
                .bundle
                .as_ref()
                .ok_or(Error::Operation("claim without bundle"))?;
            let run = Run {
                bundle_hash: hash(&serde_json::to_vec(bundle)?),
                reference: report_reference(&report.report_id),
                report,
                state: State::Triage,
                busy: false,
                admitted: None,
                base: String::new(),
                fix: None,
                docs: vec![],
                candidates: vec![],
                triage: vec![],
                test: vec![],
                base_tests: None,
                fix_tests: None,
                gates: None,
                review: None,
                pr: None,
                ci: None,
                approval: None,
                merged: None,
                conflict: false,
                stop: None,
                question: None,
                question_deadline_ms: 0,
                answers: vec![],
                retry_attempts: 0,
                retry_at_ms: 0,
                retired: false,
            };
            self.save(&run)?;
            run
        };
        if run.busy {
            return self.stop(&mut run, Stop::Interrupted).await;
        }
        if run.retry_at_ms > Timestamp::now().0 {
            return Ok(Step::Waiting {
                report: run.report.report_id,
                state: run.state,
            });
        }
        match self.pause().await {
            Ok(true) => return Ok(Step::Paused),
            Ok(false) => {}
            Err(error) => return self.failure(&mut run, error).await,
        }
        let lease = match self
            .intake
            .renew(run.report.report_id, &self.worker, self.lease_ms)
            .await
        {
            Err(intake::client::Error::Status(403)) => {
                self.intake
                    .resume_lease(run.report.report_id, &self.worker, self.lease_ms)
                    .await
            }
            other => other,
        };
        if let Err(error) = lease {
            if matches!(error, intake::client::Error::Status(403 | 404)) {
                return self.stop(&mut run, Stop::LeaseLost).await;
            }
            return self.failure(&mut run, Error::polling(error)).await;
        }
        if run.state == State::Triage {
            if let Err(error) = self.prepare_triage(&mut run).await {
                return self.failure(&mut run, error).await;
            }
            if let Some(cause) = run.stop.clone() {
                return self.stop(&mut run, cause).await;
            }
            // A test submission needs no model to say what it is.
            if let Some(test) = run.report.bundle.as_ref().and_then(|b| b.test.clone()) {
                if let Err(error) = self.submitted(&mut run, &test).await {
                    return self.failure(&mut run, error).await;
                }
                if let Some(cause) = run.stop.clone() {
                    return self.stop(&mut run, cause).await;
                }
            }
        }
        let stage = if run.state == State::Release {
            let build = self
                .build
                .as_ref()
                .ok_or(Error::Operation("missing build"))?;
            let merged = run
                .merged
                .as_ref()
                .ok_or(Error::Operation("missing merge"))?;
            match self.forge.contains(&merged.commit, &build.commit).await {
                Ok(true) => {}
                Ok(false) => return self.stop(&mut run, Stop::Infrastructure).await,
                Err(error) => return self.failure(&mut run, error).await,
            }
            if let Some(build) = &self.build
                && match self.releases.published(&build.id).await {
                    Ok(published) => published,
                    Err(error) => return self.failure(&mut run, error).await,
                }
            {
                Stage::Gates
            } else {
                Stage::Release
            }
        } else {
            run.state.stage()
        };
        // Waiting is polling an already admitted stage, not another CI/release attempt.
        let waiting = matches!(
            run.state,
            State::Question | State::Ci | State::Approval | State::Merged
        );
        let ticket = if run.state == State::Question || (waiting && run.admitted == Some(run.state))
        {
            None
        } else {
            match self.budget.check(
                stage,
                Context {
                    run_id: &run.report.report_id.to_string(),
                    reporter_key: &run.report.key_fingerprint,
                    now_ms: Timestamp::now().0,
                },
                &budget::LocalOnly,
            )? {
                Decision::Allowed(t) => Some(t),
                Decision::Stop(
                    budget::StopReason::Paused | budget::StopReason::PauseUnavailable,
                ) => return Ok(Step::Paused),
                Decision::Stop(reason) => return self.stop(&mut run, Stop::Budget(reason)).await,
            }
        };
        let previous = run.state;
        if ticket.is_some() {
            run.admitted = Some(previous);
        }
        // Polling can be repeated. Mutations and agent stages cannot.
        run.busy = !waiting;
        self.save(&run)?;
        let intake = self.intake.clone();
        let id = run.report.report_id;
        let worker = self.worker.clone();
        let lease_ms = self.lease_ms;
        let result = {
            let work = self.advance(&mut run, ticket);
            tokio::pin!(work);
            let mut renewal = tokio::time::interval(Duration::from_millis(lease_ms / 3));
            renewal.tick().await;
            loop {
                tokio::select! {
                    biased;
                    _ = renewal.tick() => { if intake.renew(id, &worker, lease_ms).await.is_err() { break Err(Error::Operation("lease lost")); } }
                    result = &mut work => break result,
                }
            }
        };
        if let Err(error) = result {
            return self.failure(&mut run, error).await;
        }
        run.busy = false;
        run.retry_attempts = 0;
        run.retry_at_ms = 0;
        self.save(&run)?;
        if let Some(cause) = run.stop.clone() {
            return self.stop(&mut run, cause).await;
        }
        if matches!(run.state, State::Merged | State::Shipped) {
            self.retire(&mut run).await?;
        }
        if previous == run.state {
            Ok(Step::Waiting {
                report: id,
                state: run.state,
            })
        } else {
            Ok(Step::Advanced {
                report: id,
                state: run.state,
            })
        }
    }
    async fn agent(
        &mut self,
        run: &Run,
        prompt: Prompt,
        ticket: Ticket,
    ) -> Result<(AgentOutput, tempfile::TempDir, Vec<PathBuf>)> {
        let dir = tempfile::tempdir_in(&self.scratch)?;
        let clone = dir.path().join("clone");
        let home = dir.path().join("home");
        let output = dir.path().join("output/result.json");
        // Review must not load agent instructions or CLI settings from the proposed tree.
        repository::snapshot(&self.repo, &run.base, &clone)?;
        let mut canaries = Honeytokens::generate()?;
        self.agents.watch_credentials(&mut canaries)?;
        let canaries = Arc::new(canaries);
        let planted = canaries.plant(dir.path())?;
        std::fs::create_dir_all(output.parent().ok_or(Error::Operation("agent output"))?)?;
        if prompt.stage() == AgentStage::Fix {
            let repro = self.repro(run)?;
            std::fs::write(clone.join(repro.target().path()), repro.reduced().source())?;
        }
        let job = AgentJob {
            prompt,
            workspace: clone,
            home,
            output,
            canaries,
            input_limit: ticket.input_limit(),
            output_limit: ticket.output_limit(),
            deadline_ms: ticket.deadline_ms(),
        };
        let result = self.agents.run(&job).await?;
        let overspent = self.budget.record(ticket, result.usage())?.is_some();
        if result.signals.contains(&Signal::CanarySeen) {
            return Err(Error::Signal(Signal::CanarySeen));
        }
        if let Some(s) = result
            .signals
            .iter()
            .find(|s| s.decision().quarantine_report)
        {
            return Err(Error::Signal(*s));
        }
        if job.canaries.scan(&result.raw).is_some() {
            return Err(Error::Signal(Signal::CanarySeen));
        }
        repository::scan_workspace(&job.workspace, &job.canaries, &planted.files)?;
        repository::scan_workspace(
            job.output
                .parent()
                .ok_or(Error::Operation("output parent"))?,
            &job.canaries,
            &[],
        )?;
        if overspent {
            return Err(Error::Signal(Signal::BudgetSpent));
        }
        if let Some(s) = result.signals.first() {
            return Err(Error::Signal(*s));
        }
        Ok((result, dir, planted.files))
    }
    async fn prepare_triage(&mut self, run: &mut Run) -> Result<()> {
        if let Some(issue) = self
            .intake
            .known_issues()
            .await
            .map_err(Error::polling)?
            .into_iter()
            .find(|i| i.signature == run.report.signature.key())
        {
            run.busy = true;
            self.save(run)?;
            self.intake
                .set_status(
                    run.report.report_id,
                    ReportProgress::Known {
                        fixed_in: issue.fixed_in,
                    },
                )
                .await?;
            run.stop = Some(Stop::Classified);
            return Ok(());
        }
        run.base = self.forge.prepare().await?;
        repository::oid(&run.base)?;
        run.docs = repository::text(
            &self.repo,
            &["ls-tree", "-r", "--name-only", &run.base, "--", "docs"],
        )?
        .lines()
        .filter(|p| {
            p.ends_with(".md")
                && p.len() <= 240
                && p.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"/_.-".contains(&b))
        })
        .map(String::from)
        .collect();
        run.candidates = self
            .intake
            .list(&intake::maintainer::ReportFilter {
                signature: Some(run.report.signature.key()),
                limit: Some(50),
                ..Default::default()
            })
            .await
            .map_err(Error::polling)?
            .reports
            .into_iter()
            .filter(|r| r.report_id != run.report.report_id)
            .map(|r| Candidate {
                reference: r.report_id,
                signature_hash: hash(r.signature.key().as_bytes()),
            })
            .collect();
        Ok(())
    }
    /// Builds a test submission's verdict in trusted code and takes its test
    /// as the repro, frozen at main's tip. Only the test's reduced form ever
    /// reaches an agent; its suggested fix is never read here.
    async fn submitted(&mut self, run: &mut Run, test: &ReportTest) -> Result<()> {
        if test.check().is_err() {
            return Err(Error::Signal(Signal::OffSchemaOutput));
        }
        let target = schema::TestTarget::new(test.kind, &test.path)
            .ok_or(Error::Signal(Signal::OffSchemaOutput))?;
        run.triage = serde_json::to_vec(&schema::TriageVerdict::submitted(&target))?;
        run.docs.clear();
        run.candidates.clear();
        run.test = test.source.clone().into_bytes();
        // Only the reduced test may reach an agent. Code the lexer can't read
        // safely, or with a literal too long to show, is an honest test no
        // agent may see: stop here, without blame and without a verdict.
        if agent_block(test).is_some() {
            run.stop = Some(Stop::UnshowableTest);
            return Ok(());
        }
        match self.repro(run) {
            Ok(_) => {}
            // The test can't go into main as sent: its path is taken or its
            // folder is gone. That isn't hostile, but nothing can reproduce
            // from it.
            Err(Error::Safety(report_pipeline::Error::Rejected(_))) => {
                run.busy = true;
                self.save(run)?;
                self.intake
                    .set_status(run.report.report_id, ReportProgress::NotReproduced)
                    .await?;
                run.stop = Some(Stop::NotReproduced);
                return Ok(());
            }
            Err(error) => return Err(error),
        }
        run.state = State::Repro;
        self.save(run)
    }
    /// One round of the check stage: claim the oldest unchecked test
    /// submission, run its test contained at the reporter's build and at
    /// main's tip, and record what that showed. No model runs, and its
    /// suggested fix is only carried. Our own trouble never reads as the
    /// submission's: it is put off and retried, then recorded as not run.
    pub async fn check(&mut self) -> Result<CheckStep> {
        if self.pause().await.unwrap_or(true) {
            return Ok(CheckStep::Paused);
        }
        let now = Timestamp::now().0;
        if !self.budget.check_room(now)? {
            return Ok(CheckStep::Spent);
        }
        let Some(report) = self
            .intake
            .claim_check(&self.worker, self.check_lease_ms)
            .await
            .map_err(Error::polling)?
        else {
            return Ok(CheckStep::Idle);
        };
        let id = report.report_id;
        let attempts: u32 = self.db.query_row(
            "INSERT INTO fixer_check_attempts(report,attempts) VALUES(?1,1) ON CONFLICT(report) DO UPDATE SET attempts=attempts+1 RETURNING attempts",
            [id.to_string()],
            |r| r.get(0),
        )?;
        let deadline =
            match self
                .budget
                .reserve_check(&id.to_string(), &report.key_fingerprint, now)?
            {
                Ok(deadline) => deadline,
                Err(reason) => {
                    // Not the reporter's doing: no attempt is spent either.
                    self.db.execute(
                        "UPDATE fixer_check_attempts SET attempts=attempts-1 WHERE report=?1",
                        [id.to_string()],
                    )?;
                    let retry = if reason == budget::StopReason::Paused {
                        u64::from(self.recovery.retry_initial_seconds) * 1000
                    } else {
                        // Today's limits reset at the next UTC day.
                        (budget::DAY_MS - now.rem_euclid(budget::DAY_MS)) as u64
                    };
                    return self.defer_check(id, retry).await;
                }
            };
        let started = Timestamp::now().0;
        let test = report.bundle.as_ref().and_then(|b| b.test.as_ref());
        let outcome = match test {
            Some(test) => self.run_check(test, deadline).await,
            None => Ok((
                self.last_main.clone(),
                Side::Ran(
                    CheckOutcome::Refused {
                        reason: CheckRefusal::InvalidTest,
                    },
                    None,
                ),
                None,
            )),
        };
        let (main, at_build, on_main) = match outcome {
            Ok(sides) => sides,
            Err(error) => {
                tracing::warn!(reference=%report_reference(&id), error=%error, "Test check could not run");
                // Retries are bounded by count and by the work they redo.
                let spent: i64 = self.db.query_row(
                    "UPDATE fixer_check_attempts SET spent_ms=spent_ms+?2 WHERE report=?1 RETURNING spent_ms",
                    params![id.to_string(), Timestamp::now().0.saturating_sub(started).max(0)],
                    |r| r.get(0),
                )?;
                if attempts <= self.recovery.retry_limit && spent < self.check_wall_ms {
                    let delay = u64::from(self.recovery.retry_initial_seconds)
                        .saturating_mul(1u64 << (attempts.saturating_sub(1)).min(20))
                        .min(u64::from(self.recovery.retry_max_seconds));
                    return self.defer_check(id, delay.max(1) * 1000).await;
                }
                let not_run = CheckOutcome::NotRun {
                    reason: NotRunReason::RunnerUnavailable,
                };
                (
                    self.last_main.clone(),
                    Side::Ran(not_run, None),
                    Some(Side::Ran(not_run, None)),
                )
            }
        };
        let hostile = [Some(&at_build), on_main.as_ref()]
            .into_iter()
            .flatten()
            .find_map(|side| match side {
                Side::Hostile(signal) => Some(*signal),
                Side::Ran(..) => None,
            });
        let refused_hostile = CheckOutcome::Refused {
            reason: CheckRefusal::HostileSignal,
        };
        let (at_build, build_site) = match at_build {
            Side::Ran(outcome, site) => (outcome, site),
            Side::Hostile(_) => (refused_hostile, None),
        };
        let (on_main, main_site) = match on_main {
            Some(Side::Ran(outcome, site)) => (outcome, site),
            Some(Side::Hostile(_)) => (refused_hostile, None),
            None => (
                CheckOutcome::NotRun {
                    reason: NotRunReason::SubmissionRefused,
                },
                None,
            ),
        };
        // One strike per submission, however often recording it is retried.
        if let Some(signal) = hostile
            && signal.decision().count_against_reporter
            && self.db.execute(
                "INSERT OR IGNORE INTO fixer_check_strikes VALUES(?1)",
                [id.to_string()],
            )? == 1
        {
            self.db.execute(STRIKE, [&report.key_fingerprint])?;
            self.ban_after_strikes(&report.key_fingerprint)?;
            tracing::warn!(reference=%report_reference(&id), signal=?signal, "Test check raised a hostile signal");
        }
        let Some(main_commit) = main else {
            // Nothing honest to record against yet; try again later.
            return self
                .defer_check(id, u64::from(self.recovery.retry_max_seconds) * 1000)
                .await;
        };
        let failure_site = if hostile.is_some() {
            None
        } else {
            build_site.or(main_site)
        };
        let check = TestCheck {
            checked_at: Timestamp::now(),
            main_commit,
            at_build,
            on_main,
            failure_site,
            agent_block: test.and_then(agent_block),
        };
        match self
            .intake
            .record_check(id, &self.worker, check.clone())
            .await
        {
            // Lost to expiry or withdrawn meanwhile: nothing more to do here.
            Ok(()) | Err(intake::client::Error::Status(403 | 404)) => {}
            Err(error) => return Err(Error::polling(error)),
        }
        self.db.execute(
            "DELETE FROM fixer_check_attempts WHERE report=?1",
            [id.to_string()],
        )?;
        tracing::info!(reference=%report_reference(&id), at_build=?check.at_build, on_main=?check.on_main, "Test submission checked");
        Ok(CheckStep::Checked { report: id, check })
    }
    async fn defer_check(&mut self, report: ReportId, retry_ms: u64) -> Result<CheckStep> {
        match self
            .intake
            .defer_check(report, &self.worker, retry_ms.clamp(1, MAX_CHECK_DEFER_MS))
            .await
        {
            Ok(()) | Err(intake::client::Error::Status(403 | 404)) => {}
            Err(error) => return Err(Error::polling(error)),
        }
        Ok(CheckStep::Deferred { report })
    }
    /// Main's tip and both sides of a check. An error is our own trouble.
    async fn run_check(
        &mut self,
        test: &ReportTest,
        deadline: i64,
    ) -> Result<(Option<String>, Side, Option<Side>)> {
        let main = self.forge.prepare().await?;
        repository::oid(&main)?;
        self.last_main = Some(main.clone());
        let refused = |reason| Side::Ran(CheckOutcome::Refused { reason }, None);
        if test.check().is_err() {
            return Ok((Some(main), Side::Hostile(Signal::OffSchemaOutput), None));
        }
        let commit = &test.source_commit;
        // Only git's plain "no" is the submission's; any other failure is ours.
        if !self.git_answer(&[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{commit}^{{commit}}"),
        ])? {
            return Ok((Some(main), refused(CheckRefusal::UnknownCommit), None));
        }
        if !self.git_answer(&["merge-base", "--is-ancestor", commit, &main])? {
            return Ok((Some(main), refused(CheckRefusal::NotOnMain), None));
        }
        let at_build = self.check_side(test, commit, deadline).await?;
        let on_main = match at_build {
            Side::Ran(CheckOutcome::Refused { .. }, _) | Side::Hostile(_) => None,
            _ => Some(self.check_side(test, &main, deadline).await?),
        };
        Ok((Some(main), at_build, on_main))
    }
    /// Whether a yes-or-no git question answered yes (0) or no (1). Anything
    /// else, from a failed spawn to a locked or broken mirror, is our trouble.
    fn git_answer(&self, args: &[&str]) -> Result<bool> {
        let status = repository::command(&self.repo)
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()?;
        match status.code() {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            _ => Err(Error::Operation("git check")),
        }
    }
    /// Places and runs the submitted test at `commit` with the Repro stage's
    /// runner, containment and tamper check. Nothing here shows the test to
    /// an agent, so its raw code runs without the comment remover.
    async fn check_side(&mut self, test: &ReportTest, commit: &str, deadline: i64) -> Result<Side> {
        let refused = |reason| Ok(Side::Ran(CheckOutcome::Refused { reason }, None));
        let listing = repository::git(&self.repo, &["ls-tree", "-r", "--name-only", "-z", commit])?;
        let paths: std::collections::BTreeSet<&[u8]> = listing
            .split(|b| *b == 0)
            .filter(|p| !p.is_empty())
            .collect();
        let path = test.path.as_str();
        let has = |p: &str| paths.contains(p.as_bytes());
        let (parent, _) = path.rsplit_once('/').unwrap_or_default();
        if has(path)
            || paths
                .iter()
                .any(|p| path.as_bytes().starts_with(&[*p, b"/"].concat()))
        {
            return refused(CheckRefusal::PathTaken);
        }
        if !paths
            .iter()
            .any(|p| p.starts_with(format!("{parent}/").as_bytes()))
        {
            return refused(CheckRefusal::NoSuchFolder);
        }
        let parts: Vec<_> = path.split('/').collect();
        let runnable = match test.kind {
            ReportTestKind::RustIntegration => has(&format!("crates/{}/Cargo.toml", parts[1])),
            ReportTestKind::RustUnit => {
                has(&format!("crates/{}/Cargo.toml", parts[1]))
                    && has(&format!("crates/{}/src/lib.rs", parts[1]))
            }
            ReportTestKind::Vitest => has("client/package.json"),
        };
        if !runnable {
            return refused(CheckRefusal::InvalidTest);
        }
        let Some(target) = schema::TestTarget::new(test.kind, path) else {
            return Ok(Side::Hostile(Signal::OffSchemaOutput));
        };
        let raw = test.source.as_bytes();
        let run = match self
            .tests
            .run(TestJob {
                repository: self.repo.clone(),
                commit: commit.into(),
                target: target.clone(),
                raw: raw.to_vec(),
                deadline_ms: deadline,
                repro_only: true,
            })
            .await
        {
            Ok(run) => run,
            Err(Error::TimedOut) => return Ok(Side::Ran(CheckOutcome::TimedOut, None)),
            Err(Error::Signal(signal)) if signal.decision().count_against_reporter => {
                return Ok(Side::Hostile(signal));
            }
            // Disk or output past the runner's limits is the test's doing.
            Err(Error::Signal(Signal::BudgetSpent) | Error::OutputCap) => {
                return Ok(Side::Ran(CheckOutcome::OverLimits, None));
            }
            Err(error) => return Err(error),
        };
        if run.commit != commit || run.test_hash != hash(raw) {
            return Ok(Side::Hostile(Signal::GateTampering));
        }
        let outcome = match (run.repro, run.detail) {
            (TestResult::Failed, _) => CheckOutcome::Fails,
            (TestResult::Passed, _) => CheckOutcome::Passes,
            (TestResult::TimedOut, _) => CheckOutcome::TimedOut,
            (TestResult::Skipped, _) | (_, Some(RunDetail::NoTestRan)) => CheckOutcome::NoTestRan,
            (_, Some(RunDetail::FailedWithoutAssertion)) => CheckOutcome::FailsWithoutAssertion,
            (_, Some(RunDetail::MissingDependencies)) => CheckOutcome::NotRun {
                reason: NotRunReason::MissingDependencies,
            },
            (_, Some(RunDetail::Crashed)) => CheckOutcome::Crashed,
            (_, Some(RunDetail::OverLimits)) => CheckOutcome::OverLimits,
            (_, Some(RunDetail::UnreadableOutput)) => CheckOutcome::UnreadableOutput,
            (TestResult::CompileError, None) => CheckOutcome::DoesNotBuild,
        };
        // A site is a real file other than the test, wherever the runner read it.
        let site = run
            .failure_site
            .filter(|_| {
                matches!(
                    outcome,
                    CheckOutcome::Fails | CheckOutcome::FailsWithoutAssertion
                )
            })
            .filter(|site| {
                site.len() <= intake::maintainer::MAX_FAILURE_SITE
                    && site.rsplit_once(':').is_some_and(|(file, line)| {
                        file != path
                            && !line.is_empty()
                            && line.bytes().all(|b| b.is_ascii_digit())
                            && repository::safe_relative(file)
                            && file
                                .bytes()
                                .all(|b| b.is_ascii_alphanumeric() || b"/_-.".contains(&b))
                            && has(file)
                    })
            });
        Ok(Side::Ran(outcome, site))
    }
    async fn advance(&mut self, run: &mut Run, ticket: Option<Ticket>) -> Result<()> {
        match run.state {
            State::Triage => {
                let bundle = run
                    .report
                    .bundle
                    .as_ref()
                    .ok_or(Error::Operation("missing bundle"))?;
                let prompt =
                    Prompt::triage_with_answers(bundle, &run.docs, &run.candidates, &run.answers)?;
                let (output, dir, _) = self
                    .agent(
                        run,
                        prompt,
                        ticket.ok_or(Error::Operation("triage reservation"))?,
                    )
                    .await?;
                run.triage = output.raw;
                let v = self.verdict(run)?;
                if v.category() != Category::Bug {
                    if let Some(q) = v.question() {
                        if run.answers.len() >= report_pipeline::render::MAX_TRIAGE_ANSWERS {
                            run.stop = Some(Stop::Question);
                            return Ok(());
                        }
                        run.question = Some(self.intake.ask(run.report.report_id, q).await?);
                        run.question_deadline_ms = Timestamp::now().0.saturating_add(
                            i64::from(self.recovery.question_timeout_seconds) * 1000,
                        );
                        self.budget
                            .begin_wait(&run.report.report_id.to_string(), Timestamp::now().0)?;
                        run.state = State::Question;
                    } else {
                        self.intake
                            .set_status(run.report.report_id, v.progress())
                            .await?;
                        run.stop = Some(Stop::Classified);
                    }
                    return Ok(());
                }
                let test_path = dir.path().join("output/repro");
                let m = std::fs::symlink_metadata(&test_path)?;
                if !m.is_file()
                    || m.file_type().is_symlink()
                    || m.len() > schema::MAX_TEST_BYTES as u64
                {
                    return Err(Error::Signal(Signal::OffSchemaOutput));
                }
                run.test = std::fs::read(test_path)?;
                self.repro(run)?;
                run.state = State::Repro;
            }
            State::Question => {
                let report = self
                    .intake
                    .report(run.report.report_id)
                    .await
                    .map_err(Error::polling)?;
                if report.history.last().is_some_and(|s| s.status.closed()) {
                    run.stop = Some(Stop::Classified);
                    return Ok(());
                }
                let question_id = run
                    .question
                    .as_ref()
                    .ok_or(Error::Operation("missing question"))?
                    .question_id;
                if let Some(answered) = report.questions.into_iter().find(|q| {
                    q.question_id == question_id
                        && q.answer
                            .as_ref()
                            .is_some_and(|a| a.at.0 <= run.question_deadline_ms)
                }) {
                    run.answers.push(answered);
                    run.question = None;
                    self.budget
                        .end_wait(&run.report.report_id.to_string(), Timestamp::now().0)?;
                    run.admitted = None;
                    run.state = State::Triage;
                } else if Timestamp::now().0 >= run.question_deadline_ms {
                    self.intake
                        .set_status(
                            run.report.report_id,
                            ReportProgress::Declined {
                                reason: DeclineReason::NotEnoughInformation,
                            },
                        )
                        .await?;
                    run.stop = Some(Stop::Question);
                }
            }
            State::Repro => {
                let repro = self.repro(run)?;
                let t = ticket.ok_or(Error::Operation("repro reservation"))?;
                let result = self
                    .tests
                    .run(TestJob {
                        repository: self.repo.clone(),
                        commit: run.base.clone(),
                        target: repro.target().clone(),
                        raw: run.test.clone(),
                        deadline_ms: t.deadline_ms(),
                        repro_only: false,
                    })
                    .await?;
                self.budget.record(t, budget::Usage::default())?;
                if result.commit != run.base || result.test_hash != repro.hash() {
                    return Err(Error::Signal(Signal::GateTampering));
                }
                // Missing offline dependencies are ours to fix, not a verdict.
                if result.detail == Some(RunDetail::MissingDependencies) {
                    return Err(Error::Operation("offline dependencies missing"));
                }
                if result.repro != TestResult::Failed {
                    self.intake
                        .set_status(run.report.report_id, ReportProgress::NotReproduced)
                        .await?;
                    run.stop = Some(Stop::NotReproduced);
                    return Ok(());
                }
                run.base_tests = Some(result);
                self.intake
                    .set_status(run.report.report_id, ReportProgress::Reproduced)
                    .await?;
                run.state = State::Fix;
            }
            State::Fix => {
                let repro = self.repro(run)?;
                let v = self.verdict(run)?;
                let prompt = Prompt::fix(&v, repro.reduced())?;
                let (_, dir, ignored) = self
                    .agent(
                        run,
                        prompt,
                        ticket.ok_or(Error::Operation("fix reservation"))?,
                    )
                    .await?;
                run.fix = Some(repository::import(
                    &self.repo,
                    &run.base,
                    &dir.path().join("clone"),
                    repro.target().path(),
                    &run.test,
                    repro.reduced().source().as_bytes(),
                    &ignored,
                )?);
                run.state = State::Gates;
            }
            State::Gates => {
                let repro = self.repro(run)?;
                let t = ticket.ok_or(Error::Operation("gates reservation"))?;
                let fix = run.fix.clone().ok_or(Error::Operation("missing fix"))?;
                // Reject supply-chain edits before executing a single changed build script.
                let diff = self.diff(run)?;
                let base = run
                    .base_tests
                    .as_ref()
                    .ok_or(Error::Operation("base tests"))?;
                let preliminary = TestEvidence {
                    repro_base: base.repro,
                    repro_fix: TestResult::Passed,
                    others_base: base.suites.clone(),
                    others_fix: base.suites.clone(),
                };
                let pre = gates::evaluate(&diff, &repro, &preliminary, GateConfig::default())?;
                if pre.outcome() == Outcome::Fail {
                    if let Some(s) = pre.signals().first() {
                        return Err(Error::Signal(*s));
                    }
                    run.gates = Some(pre);
                    run.stop = Some(Stop::Gates);
                    return Ok(());
                }
                let result = self
                    .tests
                    .run(TestJob {
                        repository: self.repo.clone(),
                        commit: fix.clone(),
                        target: repro.target().clone(),
                        raw: run.test.clone(),
                        deadline_ms: t.deadline_ms(),
                        repro_only: false,
                    })
                    .await?;
                self.budget.record(t, budget::Usage::default())?;
                if result.commit != fix || result.test_hash != repro.hash() {
                    return Err(Error::Signal(Signal::GateTampering));
                }
                let evidence = TestEvidence {
                    repro_base: base.repro,
                    repro_fix: result.repro,
                    others_base: base.suites.clone(),
                    others_fix: result.suites.clone(),
                };
                let gates = gates::evaluate(&diff, &repro, &evidence, GateConfig::default())?;
                run.fix_tests = Some(result);
                if let Some(s) = gates.signals().first() {
                    return Err(Error::Signal(*s));
                }
                if gates.outcome() == Outcome::Fail {
                    run.gates = Some(gates);
                    run.stop = Some(Stop::Gates);
                    return Ok(());
                }
                let oscillation = self.oscillation.check(
                    &run.report.report_id.to_string(),
                    hash(run.report.signature.key().as_bytes()),
                    self.verdict(run)?.area(),
                    Timestamp::now().0,
                    &diff,
                )?;
                run.conflict = oscillation.flagged();
                run.gates = Some(gates);
                if oscillation.stop_for_owner() {
                    run.stop = Some(Stop::Oscillation);
                    return Ok(());
                }
                run.state = State::Review;
            }
            State::Review => {
                // The reviewer sees the repro reduced, as the fix agent did.
                let diff = self.diff(run)?.with_reduced_test(&self.repro(run)?)?;
                let prompt = Prompt::review(&diff, self.gate_report(run)?)?;
                let (output, _, _) = self
                    .agent(
                        run,
                        prompt,
                        ticket.ok_or(Error::Operation("review reservation"))?,
                    )
                    .await?;
                let (review, signal) = schema::parse_review(&output.raw);
                if let Some(s) = signal {
                    return Err(Error::Signal(s));
                }
                run.review = Some(review.clone());
                if review.decision == ReviewDecision::Block {
                    run.stop = Some(Stop::ReviewBlocked);
                    return Ok(());
                }
                run.state = State::Push;
            }
            State::Push => {
                let v = self.verdict(run)?;
                let gates = self.gate_report(run)?;
                let proposal = Proposal {
                    reference: run.reference.clone(),
                    branch: format!("report/{}", run.reference.to_ascii_lowercase()),
                    head: run.fix.clone().ok_or(Error::Operation("proposal fix"))?,
                    title: format!("Fix report {} ({:?})", run.reference, v.area()),
                    body: format!(
                        "Report {}. Category: {:?}. Area: {:?}.\n\n{}\n\nReproduction passed from red to green. Isolated CI and the owner's review are required.",
                        run.reference,
                        v.category(),
                        v.area(),
                        gates.summary()
                    ),
                };
                run.pr = Some(self.forge.push_open(&proposal).await?);
                if run.pr.as_ref().is_none_or(|p| p.head != proposal.head) {
                    return Err(Error::Signal(Signal::GateTampering));
                }
                self.intake
                    .set_status(run.report.report_id, ReportProgress::FixWritten)
                    .await?;
                self.intake
                    .set_status(run.report.report_id, ReportProgress::InReview)
                    .await?;
                if let Some(t) = ticket {
                    self.budget.record(t, budget::Usage::default())?;
                }
                self.budget
                    .begin_wait(&run.report.report_id.to_string(), Timestamp::now().0)?;
                run.admitted = Some(State::Ci);
                run.state = State::Ci;
            }
            State::Ci => {
                let pr = run.pr.as_ref().ok_or(Error::Operation("missing PR"))?;
                match self.forge.ci(pr).await? {
                    CiState::Pending => {}
                    CiState::Failed => run.stop = Some(Stop::CiFailed),
                    CiState::Passed { run: ci } => {
                        self.budget
                            .end_wait(&run.report.report_id.to_string(), Timestamp::now().0)?;
                        run.ci = Some(ci);
                        let diff = self.diff(run)?;
                        let view = ApprovalView {
                            report_id: run.report.report_id,
                            reference: run.reference.clone(),
                            area: self.verdict(run)?.area(),
                            test: self.repro(run)?.target().path().into(),
                            files: diff.files().len(),
                            lines: diff
                                .files()
                                .iter()
                                .map(|f| f.added.len() + f.removed.len())
                                .sum(),
                            gates: self.gate_report(run)?.clone(),
                            conflict: run.conflict,
                            pr: pr.clone(),
                        };
                        self.approvals.raise(&view).await?;
                        run.approval = Some(view);
                        self.budget
                            .begin_wait(&run.report.report_id.to_string(), Timestamp::now().0)?;
                        run.state = State::Approval;
                    }
                }
            }
            State::Approval => {
                let view = run
                    .approval
                    .as_ref()
                    .ok_or(Error::Operation("missing approval"))?;
                match self.approvals.decision(view).await? {
                    OwnerDecision::Waiting => {}
                    OwnerDecision::Closed => {
                        self.intake
                            .set_status(
                                run.report.report_id,
                                ReportProgress::Declined {
                                    reason: DeclineReason::NotEnoughInformation,
                                },
                            )
                            .await?;
                        run.stop = Some(Stop::OwnerClosed);
                    }
                    OwnerDecision::Merged => {
                        self.budget
                            .end_wait(&run.report.report_id.to_string(), Timestamp::now().0)?;
                        let Some(merged) = self.forge.merged(&view.pr).await? else {
                            return Err(Error::Operation("approval without merge"));
                        };
                        repository::oid(&merged.commit)?;
                        if !self
                            .oscillation
                            .remembered(&run.report.report_id.to_string())?
                        {
                            self.oscillation.remember(
                                &run.report.report_id.to_string(),
                                hash(run.report.signature.key().as_bytes()),
                                self.verdict(run)?.area(),
                                merged.when_ms,
                                &self.diff(run)?,
                            )?;
                        }
                        run.merged = Some(merged);
                        self.budget
                            .begin_wait(&run.report.report_id.to_string(), Timestamp::now().0)?;
                        run.state = State::Merged;
                    }
                }
            }
            State::Merged => {
                let Some(build) = &self.build else {
                    return Ok(());
                };
                let merged = run
                    .merged
                    .as_ref()
                    .ok_or(Error::Operation("missing merge"))?;
                if self.forge.contains(&merged.commit, &build.commit).await? {
                    self.budget
                        .end_wait(&run.report.report_id.to_string(), Timestamp::now().0)?;
                    self.intake
                        .set_status(
                            run.report.report_id,
                            ReportProgress::Fixed {
                                build: build.id.clone(),
                            },
                        )
                        .await?;
                    run.state = State::Release;
                }
            }
            State::Release => {
                let build = self
                    .build
                    .as_ref()
                    .ok_or(Error::Operation("missing build"))?;
                let merged = run
                    .merged
                    .as_ref()
                    .ok_or(Error::Operation("missing merge"))?;
                let statement = provenance::Statement::new(provenance::FixEvidence {
                    report_id: run.report.report_id,
                    bundle_hash: run.bundle_hash,
                    triage: &self.verdict(run)?,
                    test: &self.repro(run)?,
                    base_commit: run
                        .base
                        .clone()
                        .try_into()
                        .map_err(|_| Error::Operation("base identity"))?,
                    diff: self.diff(run)?.text().as_bytes(),
                    gates: self.gate_report(run)?,
                    review: run
                        .review
                        .as_ref()
                        .ok_or(Error::Operation("missing review"))?,
                    ci: provenance::CiRun {
                        id: run
                            .ci
                            .clone()
                            .ok_or(Error::Operation("missing CI"))?
                            .try_into()
                            .map_err(|_| Error::Operation("CI identity"))?,
                        result: provenance::CiResult::Passed,
                    },
                    approval: provenance::Approval {
                        who: merged
                            .who
                            .clone()
                            .try_into()
                            .map_err(|_| Error::Operation("owner identity"))?,
                        when_ms: merged.when_ms,
                        merge_commit: merged
                            .commit
                            .clone()
                            .try_into()
                            .map_err(|_| Error::Operation("merge identity"))?,
                    },
                    release_build: build
                        .id
                        .clone()
                        .try_into()
                        .map_err(|_| Error::Operation("build identity"))?,
                })?;
                self.db.execute(
                    "INSERT OR REPLACE INTO fixer_provenance VALUES(?1,'release',?2)",
                    params![
                        run.report.report_id.to_string(),
                        serde_json::to_vec(&self.key.sign(statement)?)?
                    ],
                )?;
                self.releases
                    .publish(ReleaseEntry {
                        build: build.id.clone(),
                        published_at: Timestamp::now(),
                        ring: ReleaseRing::Owner,
                        state: ReleaseState::Rolling,
                        fixes: vec![run.report.signature.key()],
                    })
                    .await?;
                self.intake
                    .set_status(
                        run.report.report_id,
                        ReportProgress::Shipped {
                            build: build.id.clone(),
                        },
                    )
                    .await?;
                if let Some(t) = ticket {
                    self.budget.record(t, budget::Usage::default())?;
                }
                run.state = State::Shipped;
            }
            State::Shipped | State::Stopped => {}
        }
        Ok(())
    }
}
