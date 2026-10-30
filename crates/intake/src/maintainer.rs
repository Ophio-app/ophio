// SPDX-License-Identifier: Apache-2.0
//! Private API shared with pipeline workers. Reporter-facing values come from protocol.
use protocol::{ReportId, ReportQuestionId, reports::*, time::Timestamp};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const PATH_QUERY: &str = "/maintainer/v1/reports/query";
pub const PATH_COMMAND: &str = "/maintainer/v1/command";
pub const PATH_STATE: &str = "/maintainer/v1/state";
pub const PATH_COUNTERS: &str = "/maintainer/v1/counters";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReportFilter {
    pub status: Option<ReportProgress>,
    pub signature: Option<String>,
    pub since: Option<Timestamp>,
    /// Exclusive cursor into the (received time, id) ordering, oldest first.
    pub after: Option<ReportCursor>,
    pub limit: Option<usize>,
    /// Only reports whose recorded check failed at this site.
    #[serde(default)]
    pub failure_site: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportCursor {
    pub received_at: Timestamp,
    pub report_id: ReportId,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportPage {
    pub reports: Vec<ReportRecord>,
    pub next: Option<ReportCursor>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportRecord {
    pub report_id: ReportId,
    /// SHA-256 of the public key; also used for bans and counters.
    pub key_fingerprint: String,
    pub signature: ReportSignature,
    pub received_at: Timestamp,
    pub updated_at: Timestamp,
    pub history: Vec<StatusChange>,
    pub questions: Vec<ReportQuestion>,
    pub duplicate_of: Option<ReportId>,
    pub group_id: ReportId,
    pub group_count: u64,
    pub lease: Option<Lease>,
    /// Only present in the single-report response and successful claims.
    pub bundle: Option<ReportBundle>,
    /// What running a test submission showed, once it has been checked.
    /// For the owner only: never part of what a reporter sees.
    #[serde(default)]
    pub check: Option<TestCheck>,
}

/// The longest check lease and the longest a check may be put off.
pub const MAX_CHECK_LEASE_MS: u64 = 3_600_000;
pub const MAX_CHECK_DEFER_MS: u64 = 86_400_000;
/// The longest failure site a check records.
pub const MAX_FAILURE_SITE: usize = 300;

/// What running a test submission's test showed, contained and offline, at
/// the reporter's build and at main's tip. Written by trusted code from
/// the runs alone; no model is asked. Owner-only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TestCheck {
    pub checked_at: Timestamp,
    /// Main's tip the test ran at, as 40 lowercase hex digits.
    pub main_commit: String,
    pub at_build: CheckOutcome,
    pub on_main: CheckOutcome,
    /// Where the test failed inside our tree, other than in the test file
    /// itself, as `path:line`: a file that exists in the tree. A hint only,
    /// since the test's own output could name another real file.
    pub failure_site: Option<String>,
    /// Why Look into it couldn't use this test, when its code can't be
    /// shown to an agent. None when it can, or when that wasn't reached.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_block: Option<AgentBlock>,
}

/// Why a submitted test can't go to the fix agent, even reduced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentBlock {
    /// The comment remover can't read its code safely, such as JSX or a
    /// regular expression in a Vitest test.
    Unreducible,
    /// It has a string or literal longer than 256 bytes, which no agent is
    /// ever shown.
    LongLiteral,
}

/// What one run of a submitted test showed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
pub enum CheckOutcome {
    /// It built and its run reported an assertion failure in the shape the
    /// test harness gives one, by the same evidence rules the fixer's
    /// red-to-green gate uses. The test controls its own output, so this is
    /// what the run reported, not proof of a bug.
    Fails,
    /// It built and ran, and failed without an assertion: a panic or an
    /// error. Not evidence by the gate's rules, but not a build failure.
    FailsWithoutAssertion,
    Passes,
    /// It, or the tree with it, didn't compile.
    DoesNotBuild,
    /// It built, but no test ran.
    NoTestRan,
    TimedOut,
    /// It ran past the runner's disk, memory, CPU, file-size or output
    /// limits.
    OverLimits,
    /// It built, then stopped abnormally before reporting a result: a
    /// crash, abort, stack overflow or early exit.
    Crashed,
    /// It ran, but its output wasn't a test report the runner could read.
    UnreadableOutput,
    /// It wasn't run, because the submission can't be checked as sent.
    Refused {
        reason: CheckRefusal,
    },
    /// It wasn't run, through no fault of the submission.
    NotRun {
        reason: NotRunReason,
    },
}

/// Why a submission can't be checked as sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckRefusal {
    /// The commit isn't in the fixer's mirror.
    UnknownCommit,
    /// The commit is there, but isn't part of main.
    NotOnMain,
    /// A file already has the test's path at that commit.
    PathTaken,
    /// The test's folder doesn't exist at that commit.
    NoSuchFolder,
    /// The test breaks the rules for test files, or its code can't be read
    /// safely enough to run.
    InvalidTest,
    /// Running it raised a hostile signal, which counts against the reporter.
    HostileSignal,
}

/// Why a run didn't happen that is nobody's fault in the submission.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotRunReason {
    /// The offline caches don't hold the dependencies that commit locks.
    MissingDependencies,
    /// The contained runner wasn't available after several tries.
    RunnerUnavailable,
    /// The other side was refused, so this one wasn't tried.
    SubmissionRefused,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Lease {
    pub worker: String,
    pub expires_at: Timestamp,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnownIssue {
    pub signature: String,
    #[serde(default)]
    pub affected_builds: Vec<String>,
    pub fixed_in: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Switches {
    pub pipeline_paused: bool,
    pub intake_closed: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Counters {
    pub reports_per_day: BTreeMap<String, u64>,
    pub reports_per_key: BTreeMap<String, u64>,
    pub rejections: BTreeMap<String, u64>,
    pub audit_rows: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    Claim {
        worker: String,
        lease_ms: u64,
    },
    ClaimNext {
        worker: String,
        lease_ms: u64,
        #[serde(default)]
        excluding: Vec<ReportId>,
    },
    FinishReport {
        report_id: ReportId,
        worker: String,
    },
    Renew {
        report_id: ReportId,
        worker: String,
        lease_ms: u64,
    },
    ResumeLease {
        report_id: ReportId,
        worker: String,
        lease_ms: u64,
    },
    /// Takes the oldest open test submission that has no recorded check and
    /// no live check lease, whether or not the owner admitted it. Returns
    /// nothing while the pipeline is paused.
    ClaimCheck {
        worker: String,
        lease_ms: u64,
    },
    /// Records a check, only by the worker holding its live check lease.
    /// Recording the same check again changes nothing.
    RecordCheck {
        report_id: ReportId,
        worker: String,
        check: TestCheck,
    },
    /// The check lease's holder puts the check off: no worker is offered it
    /// again until `retry_ms` from now.
    DeferCheck {
        report_id: ReportId,
        worker: String,
        retry_ms: u64,
    },
    /// The owner chose this report for the fixer. Claims take nothing else.
    Admit {
        report_id: ReportId,
    },
    SetStatus {
        report_id: ReportId,
        status: ReportProgress,
    },
    Ask {
        report_id: ReportId,
        template: QuestionTemplate,
    },
    CloseQuestion {
        report_id: ReportId,
        question_id: ReportQuestionId,
    },
    AnswerQuestion {
        report_id: ReportId,
        question_id: ReportQuestionId,
        answer: ReportAnswer,
    },
    PutKnown {
        issue: KnownIssue,
    },
    DeleteKnown {
        signature: String,
    },
    Publish {
        release: ReleaseEntry,
    },
    Widen {
        build: String,
        ring: ReleaseRing,
    },
    Halt {
        build: String,
    },
    Complete {
        build: String,
    },
    PipelinePaused {
        paused: bool,
    },
    IntakeClosed {
        closed: bool,
    },
    Ban {
        key_fingerprint: String,
        banned: bool,
    },
    Sweep {
        retention_days: u32,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum CommandResult {
    Done,
    Claimed { report: Option<Box<ReportRecord>> },
    Question { question: ReportQuestion },
    Swept { reports: usize },
}
