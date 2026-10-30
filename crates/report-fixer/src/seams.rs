// SPDX-License-Identifier: Apache-2.0
use crate::Result;
use async_trait::async_trait;
use ports::sandbox::RelayTotals;
use protocol::{ReportId, reports::ReleaseEntry};
use report_pipeline::{
    budget::Usage,
    gates::{CommitDiff, GateReport, TestResult},
    honeytoken::Honeytokens,
    render,
    schema::{ReducedTest, TestTarget, TriageVerdict},
    signals::Signal,
};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

/// Constructed exclusively by the library renderers. Fix cannot accept a bundle.
#[derive(Debug)]
pub struct Prompt {
    pub(crate) stage: AgentStage,
    text: String,
}
impl Prompt {
    pub fn triage(
        bundle: &protocol::reports::ReportBundle,
        docs: &[String],
        candidates: &[render::Candidate],
    ) -> Result<Self> {
        Ok(Self {
            stage: AgentStage::Triage,
            text: render::triage(bundle, docs, candidates)?,
        })
    }
    pub fn fix(verdict: &TriageVerdict, test: &ReducedTest) -> Result<Self> {
        Ok(Self {
            stage: AgentStage::Fix,
            text: render::fix(verdict, test)?,
        })
    }
    pub fn triage_with_answers(
        bundle: &protocol::reports::ReportBundle,
        docs: &[String],
        candidates: &[render::Candidate],
        answers: &[protocol::reports::ReportQuestion],
    ) -> Result<Self> {
        Ok(Self {
            stage: AgentStage::Triage,
            text: render::triage_with_answers(bundle, docs, candidates, answers)?,
        })
    }
    pub fn review(diff: &CommitDiff, gates: &GateReport) -> Result<Self> {
        Ok(Self {
            stage: AgentStage::Review,
            text: render::review(diff, gates)?,
        })
    }
    pub fn text(&self) -> &str {
        &self.text
    }
    pub fn stage(&self) -> AgentStage {
        self.stage
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentStage {
    Triage,
    Fix,
    Review,
}
#[derive(Debug)]
pub struct AgentJob {
    pub prompt: Prompt,
    pub workspace: PathBuf,
    pub home: PathBuf,
    pub output: PathBuf,
    pub canaries: Arc<Honeytokens>,
    pub input_limit: u64,
    pub output_limit: u64,
    pub deadline_ms: i64,
}
#[derive(Debug, Default)]
pub struct AgentOutput {
    pub raw: Vec<u8>,
    pub totals: RelayTotals,
    pub signals: Vec<Signal>,
}
impl AgentOutput {
    pub fn usage(&self) -> Usage {
        Usage {
            input_tokens: self.totals.input_tokens,
            output_tokens: self.totals.output_tokens,
        }
    }
}
#[async_trait]
pub trait AgentRunner: Send + Sync {
    fn watch_credentials(&self, _canaries: &mut Honeytokens) -> Result<()> {
        Ok(())
    }
    async fn run(&self, job: &AgentJob) -> Result<AgentOutput>;
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestRun {
    pub commit: String,
    pub test_hash: [u8; 32],
    pub repro: TestResult,
    pub suites: BTreeMap<String, TestResult>,
    /// What the runner could tell about a repro that didn't pass or fail
    /// with an assertion. `repro` keeps the gate's evidence rules either way.
    #[serde(default)]
    pub detail: Option<RunDetail>,
    /// Where a failing repro first failed inside the tree, other than in the
    /// test file, as `path:line`, read from the runner's own output. The
    /// test can print anything, so this is a hint the caller checks against
    /// the tree, never evidence.
    #[serde(default)]
    pub failure_site: Option<String>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunDetail {
    /// It built, but no test ran.
    NoTestRan,
    /// It built and ran, and failed without an assertion: a panic or error.
    FailedWithoutAssertion,
    /// The offline caches lack dependencies that commit locks.
    MissingDependencies,
    /// It built, then stopped before reporting a result: a crash, abort,
    /// stack overflow or early exit.
    Crashed,
    /// It was killed for memory, CPU time or file size.
    OverLimits,
    /// It ran, but printed no test report the runner could read.
    UnreadableOutput,
}
#[derive(Debug)]
pub struct TestJob {
    pub repository: PathBuf,
    pub commit: String,
    pub target: TestTarget,
    pub raw: Vec<u8>,
    pub deadline_ms: i64,
    /// Run only the repro, not the other suites: a test submission's check.
    pub repro_only: bool,
}
#[async_trait]
pub trait TestRunner: Send + Sync {
    async fn run(&self, job: TestJob) -> Result<TestRun>;
}
/// Contains no model-supplied wording, path, test source or diagnostic text.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Proposal {
    pub reference: String,
    pub branch: String,
    pub head: String,
    pub title: String,
    pub body: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PullRequest {
    pub number: u64,
    pub head: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum CiState {
    Pending,
    Passed { run: String },
    Failed,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Merged {
    pub commit: String,
    pub who: String,
    pub when_ms: i64,
}
#[async_trait]
pub trait Forge: Send + Sync {
    /// Fetch only into the pipeline's own bare mirror. Return a full base OID.
    async fn prepare(&self) -> Result<String>;
    async fn push_open(&self, proposal: &Proposal) -> Result<PullRequest>;
    /// Must verify the exact head and the isolated workflow, including runner labels.
    async fn ci(&self, pr: &PullRequest) -> Result<CiState>;
    /// Observe an owner merge. This interface deliberately cannot authorize one.
    async fn merged(&self, pr: &PullRequest) -> Result<Option<Merged>>;
    async fn contains(&self, commit: &str, build_commit: &str) -> Result<bool>;
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalView {
    pub report_id: ReportId,
    pub reference: String,
    pub area: report_pipeline::schema::Area,
    pub test: String,
    pub files: usize,
    pub lines: usize,
    pub gates: GateReport,
    pub conflict: bool,
    pub pr: PullRequest,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnerDecision {
    Waiting,
    Merged,
    Closed,
}
#[async_trait]
pub trait Approvals: Send + Sync {
    /// Idempotent by report id. Actions use the existing person-only GitHub path.
    async fn raise(&self, view: &ApprovalView) -> Result<()>;
    async fn decision(&self, view: &ApprovalView) -> Result<OwnerDecision>;
    /// Idempotently revoke every pending action, even if raising it was interrupted.
    async fn revoke(&self, report_id: ReportId) -> Result<()>;
}
#[async_trait]
pub trait Releases: Send + Sync {
    async fn published(&self, _build: &str) -> Result<bool> {
        Ok(false)
    }
    async fn publish(&self, entry: ReleaseEntry) -> Result<()>;
}
#[async_trait]
impl Releases for intake::client::Client {
    async fn published(&self, build: &str) -> Result<bool> {
        Ok(self
            .releases()
            .await
            .map_err(crate::Error::polling)?
            .releases
            .iter()
            .any(|r| r.build == build))
    }
    async fn publish(&self, entry: ReleaseEntry) -> Result<()> {
        if let Some(existing) = self
            .releases()
            .await?
            .releases
            .iter()
            .find(|r| r.build == entry.build)
            && entry.fixes.iter().all(|f| existing.fixes.contains(f))
        {
            return Ok(());
        }
        Ok(self.publish(entry).await?)
    }
}
#[derive(Debug, Clone)]
pub struct RunningBuild {
    pub id: String,
    pub commit: String,
}
