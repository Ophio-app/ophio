// SPDX-License-Identifier: Apache-2.0
//! Durable projections: host, projects, tasks, runs, children, approvals,
//! attention and receipts.

use serde::{Deserialize, Serialize};

use crate::annotation::AnnotationBundle;
use crate::auth::Principal;
use crate::capability::{HarnessKind, HostCapabilities};
use crate::control::{ControlTarget, TargetControlView};
use crate::ids::*;
use crate::library::LibraryRoot;
use crate::time::Timestamp;

/// What the host holds now, within one control frame: every task that is not
/// finished, with its runs, every pending approval that fits and everything
/// waiting for a person, then finished tasks newest first. Runs leave out the
/// diffs their agent reported; `Query::Run` reads a run whole.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct Snapshot {
    /// Journal sequence this snapshot reflects.
    #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
    pub journal_head: u64,
    pub host: HostInfo,
    pub projects: Vec<Project>,
    pub tasks: Vec<Task>,
    pub runs: Vec<Run>,
    pub approvals: Vec<Approval>,
    pub attention: Vec<AttentionItem>,
    pub targets: Vec<ControlTarget>,
    pub control: Vec<TargetControlView>,
    pub library_roots: Vec<LibraryRoot>,
    /// Tasks left out to stay within one frame, all changed before those
    /// carried. Absent when the snapshot carries every task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub older_tasks: Option<OlderTasks>,
    /// Pending approvals left out to stay within one frame, each read alone
    /// with `Query::Approval`. Absent when every pending approval is carried.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(as = "Option<Vec<ApprovalId>>", optional)]
    pub withheld_approvals: Vec<ApprovalId>,
}

/// A place among tasks listed newest first: by last change, then by id.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
    ts_rs::TS,
)]
#[ts(export)]
pub struct TaskCursor {
    pub updated_at: Timestamp,
    pub task_id: TaskId,
}

/// Where the tasks a snapshot leaves out begin.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct OlderTasks {
    /// The newest task left out; `Query::TaskHistory` reads on from here.
    pub from: TaskCursor,
    /// Projects with tasks from `from` on.
    pub projects: Vec<ProjectId>,
}

/// Tasks with their runs, approvals and attention, as a snapshot carries them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct TaskRecords {
    pub tasks: Vec<Task>,
    pub runs: Vec<Run>,
    pub approvals: Vec<Approval>,
    pub attention: Vec<AttentionItem>,
    /// Pending approvals left out to stay within one frame, each read alone
    /// with `Query::Approval`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(as = "Option<Vec<ApprovalId>>", optional)]
    pub withheld_approvals: Vec<ApprovalId>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct HostInfo {
    /// Current destination for files sent to the computer's inbox.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub inbox_path: Option<String>,
    pub host_id: HostId,
    pub name: String,
    pub os_label: String,
    pub companion_version: String,
    pub boot_epoch: BootEpoch,
    pub started_at: Timestamp,
    pub capabilities: HostCapabilities,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct Project {
    pub project_id: ProjectId,
    pub name: String,
    /// Host-native absolute path (display only on the phone).
    pub host_path: String,
    pub default_harness: Option<HarnessKind>,
    pub default_model: Option<String>,
    pub default_effort: Option<String>,
    /// Run currently holding the managed writer slot, if any.
    pub active_writer: Option<RunId>,
    pub vcs: Option<VcsInfo>,
    pub created_at: Timestamp,
    /// Latest explicit stage decision. Older projects have no stage, revision zero.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub stage: Option<crate::project::ProjectStageTransition>,
    /// A folder made for one task started "On its own", listed under
    /// One-offs until it is promoted.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[ts(as = "Option<bool>", optional)]
    pub one_off: bool,
    /// The person's home folder, which every computer has as a place to
    /// work and which is never removed. Files in it are not sent to or
    /// from the phone as a project's are.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[ts(as = "Option<bool>", optional)]
    pub home: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct VcsInfo {
    pub kind: String,
    pub branch: Option<String>,
    pub head: Option<String>,
    pub dirty_files: u32,
}

/// A wake the agent asked for: Ophio starts its next turn at `at`, with
/// what it was waiting for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct TaskWake {
    pub at: Timestamp,
    pub reason: String,
    /// Waiting for another agent's answer, which wakes it when it comes;
    /// `at` is when Ophio wakes it anyway.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[ts(as = "Option<bool>", optional)]
    pub answer: bool,
}

/// The Git branch a task's work is for. The agent is told it; Ophio never
/// creates or checks out a branch itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct TaskBranch {
    pub name: String,
    /// The agent makes it from what is checked out before changing any file.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[ts(as = "Option<bool>", optional)]
    pub new: bool,
}

/// A project's local Git branches, read when asked. `VcsInfo` is captured when
/// the project is added and goes stale.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct BranchListing {
    /// Checked out now; None when HEAD is detached.
    pub current: Option<String>,
    /// Most recently committed first, the current branch included.
    pub branches: Vec<String>,
    /// Tracked files that differ from HEAD.
    pub dirty_files: u32,
    /// More branches exist than are listed.
    pub truncated: bool,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum TaskKind {
    /// Coding / general harness work in a project.
    Coding,
    /// Delegated GUI work by a computer-use specialist.
    ComputerUse,
}

/// Requested intent, restricted by the selected harness's declared policy.
#[derive(
    Debug,
    Default,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
    ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum TaskMode {
    Ask,
    Plan,
    #[default]
    Work,
}

/// Native launch permissions, never Ophio device or desktop authority.
/// Omission on older tasks preserves their earlier launch behavior.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum LaunchPolicy {
    Standard,
    BypassPermissions,
    AcceptEdits,
    Sandboxed,
}

impl LaunchPolicy {
    pub fn supports(self, harness: HarnessKind) -> bool {
        use HarnessKind::{ClaudeCode, Codex};
        match self {
            Self::Standard | Self::BypassPermissions => matches!(harness, ClaudeCode | Codex),
            Self::AcceptEdits => harness == ClaudeCode,
            Self::Sandboxed => harness == Codex,
        }
    }
}

/// Task lifecycle as the person sees it. Stream/view state is separate.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum TaskState {
    /// Admitted by the host; no run dispatched yet (e.g. in Next up).
    Accepted,
    Dispatching,
    Working,
    /// A question/approval/"waiting for you" blocks progress.
    AwaitingDecision,
    /// Stopped/interrupted by a person or failure; resumable only explicitly.
    Interrupted,
    Completed,
    Failed,
    /// Outcome of a dispatch or effect is uncertain.
    ReconciliationRequired,
    Archived,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct Task {
    pub task_id: TaskId,
    pub project_id: ProjectId,
    pub kind: TaskKind,
    /// Kept by resumed runs. Carrying out a finished Plan is the one change,
    /// to Work; each run records its own in `Run::mode`.
    #[serde(default)]
    #[ts(as = "Option<TaskMode>", optional)]
    pub mode: TaskMode,
    pub title: String,
    pub state: TaskState,
    /// Revision of task-level mutable fields (title, queue position…).
    #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
    pub revision: u64,
    pub harness: HarnessKind,
    pub model: Option<String>,
    pub effort: Option<String>,
    /// Saved choice for new runs, including explicit continuations. Ask and
    /// Plan keep their read-only policy; this choice applies when doing Work.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub launch_policy: Option<LaunchPolicy>,
    /// Git branch the person wants the work done on. None leaves the branch
    /// to the agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub branch: Option<TaskBranch>,
    /// The agent, skill or scratch pad the task started from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub start: Option<crate::launchers::StartedFrom>,
    /// Work runs may use Ophio's agent browser and the logins the owner lets
    /// the task use.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[ts(as = "Option<bool>", optional)]
    pub browser: bool,
    /// A conversation held in Talk over many turns rather than a piece of
    /// work. Carrying it on in Work makes it a task, which keeps its access.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[ts(as = "Option<bool>", optional)]
    pub conversation: bool,
    /// What a conversation's agent may read, change and reach, as the person
    /// last chose it. None is what a conversation starts with, or, for a
    /// task, no limit beyond its mode. A task carried on from a
    /// conversation keeps the conversation's, with its own project writable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub access: Option<crate::access::ConversationAccess>,
    /// What the agent of a conversation, or of a task carried on from one,
    /// last tried and was kept from, until the person lets it or the access
    /// changes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub access_blocked: Option<Box<crate::access::AccessBlocked>>,
    /// The GitHub work the task is for, and its pull request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub github: Option<Box<crate::github::TaskGithub>>,
    /// The task's part in a review: the rules it was given and, for a
    /// finder or verifier, how each finding was judged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub review: Option<Box<crate::review::TaskReview>>,
    /// When the agent asked to be woken, between turns: the task is
    /// watching, not working, and the project is free meanwhile.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub wake: Option<TaskWake>,
    /// The task whose agent asked this one, read-only, for its help: its
    /// answer goes back there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub asked_by: Option<TaskId>,
    pub created_by: Principal,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    /// Most recent run, if any.
    pub current_run: Option<RunId>,
    /// Target for computer-use tasks.
    pub target_id: Option<TargetId>,
    /// Mission bounds, when set.
    pub mission: Option<Mission>,
    /// Unresolved attention count for badges.
    pub attention_count: u32,
    /// Most recent human-readable status line from the harness.
    pub status_line: Option<String>,
}

/// Bounded mission: goal, authorized scope, completion checks and stop point.
/// Enforcement coverage is reported per bound; unsupported bounds are shown as
/// requests, never as enforced.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct Mission {
    pub goal: String,
    pub bounds: Vec<MissionBound>,
    pub completion_checks: Vec<String>,
    pub stop_point: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct MissionBound {
    pub text: String,
    pub enforcement: Enforcement,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum Enforcement {
    /// Enforced by a native permission/tool policy the adapter configured.
    Enforced,
    /// Passed to the agent as an instruction only.
    RequestedOnly,
    Unsupported,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum RunState {
    /// Admitted; dispatch not started.
    Admitted,
    /// Dispatch started; native acknowledgement pending.
    Dispatching,
    /// Native harness acknowledged and is working.
    Working,
    AwaitingApproval,
    AwaitingInput,
    /// Stop requested; native interrupt pending.
    Stopping,
    Interrupted,
    Completed,
    Failed,
    ReconciliationRequired,
}

impl RunState {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            RunState::Interrupted | RunState::Completed | RunState::Failed
        )
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct Run {
    /// A person's check of helper unknowns, never computer verification.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub helpers_checked: Option<HumanCheck>,
    /// A human-recorded outcome is not computer verification.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub human_resolution: Option<HumanResolution>,
    /// This companion owns the agent process and supports Force stop.
    #[serde(default)]
    #[ts(as = "Option<bool>", optional)]
    pub force_stoppable: bool,
    /// The turn ended, but this companion hasn't confirmed that everything
    /// the agent started has ended. Until it has, Stop still reaches the run.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[ts(as = "Option<bool>", optional)]
    pub cleanup_pending: bool,
    pub run_id: RunId,
    pub task_id: TaskId,
    pub harness: HarnessKind,
    /// Actual model/effort as reported by the native harness (may differ from
    /// what was requested; both are shown).
    pub requested_model: Option<String>,
    pub actual_model: Option<String>,
    pub requested_effort: Option<String>,
    /// Requested native Work policy, not proof of native acknowledgement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub launch_policy: Option<LaunchPolicy>,
    pub actual_effort: Option<String>,
    pub account_label: Option<String>,
    /// Native session identity for resume/reconciliation.
    pub native_session: Option<String>,
    pub state: RunState,
    pub admitted_at: Timestamp,
    pub dispatched_at: Option<Timestamp>,
    pub acknowledged_at: Option<Timestamp>,
    pub ended_at: Option<Timestamp>,
    /// Helpers in the order first reported: every one that may still be
    /// running, and the finished ones that ended most recently.
    pub children: Vec<ChildAgent>,
    /// Finished helpers `children` no longer lists, so that a run with any
    /// number of helpers fits one control frame. Absent when all are listed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub earlier_children: Option<u32>,
    pub outcome: Option<RunOutcome>,
    /// Whether this run holds the project's managed writer slot.
    pub holds_writer: bool,
    /// Mode the run was admitted in. Carrying out a Plan switches the task to
    /// Work, so earlier runs keep their own mode here. Absent on runs recorded
    /// before it was kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub mode: Option<TaskMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub revisions: Option<RunRevisions>,
    /// A later run on the task had to change things after this one finished
    /// as done.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub follow_up: Option<FollowUp>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct HumanResolution {
    pub resolution: crate::command::Reconciliation,
    pub at: Timestamp,
    pub by: Principal,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct HumanCheck {
    pub at: Timestamp,
    pub by: Principal,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ChildLifetime {
    InProcess,
    SeparateProcess,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct ChildAgent {
    /// Missing when the harness cannot establish the helper's lifetime.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub lifetime: Option<ChildLifetime>,
    pub child_id: ChildId,
    /// Native identity (e.g. subagent/task id) where supplied.
    pub native_ref: Option<String>,
    pub label: String,
    pub state: ChildState,
    pub started_at: Timestamp,
    pub ended_at: Option<Timestamp>,
    pub provenance: Provenance,
    /// Missing on older companions; never assume a child is controllable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub stop: Option<crate::capability::Availability>,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ChildState {
    Active,
    Completed,
    Failed,
    Stopped,
    Unknown,
}

/// Where a fact came from, and how strong the evidence is.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum Provenance {
    /// Reported by the native harness protocol.
    Native,
    /// Observed by the companion (process exit, file change).
    Observed,
    /// Inferred (heuristic). Never overrides native/observed facts.
    Inferred,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct RunOutcome {
    pub summary: Option<String>,
    pub changed_files: Vec<ChangedFile>,
    pub checks: Vec<CheckResult>,
    /// Work still running or unknown when the run ended.
    pub still_active: Vec<String>,
    pub unknowns: Vec<String>,
    pub usage: Option<UsageSummary>,
    /// What the agent said its turn came to. Absent when it didn't say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub report: Option<RunReport>,
    /// Whether its work was checked after its last change. Absent on runs
    /// recorded before this was kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub verification: Option<RunVerification>,
}

/// What an agent says its turn came to, from the `ophio-outcome` block it
/// ends its last message with. The agent's own word: it decides what Ophio
/// offers next, never what is verified.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct RunReport {
    pub status: ReportStatus,
    /// What the agent needs the person to answer or do before it can go on.
    #[serde(default)]
    pub questions: Vec<String>,
    /// What it left undone.
    #[serde(default)]
    pub remaining: Vec<String>,
    /// Separate work the turn found or proposed, each to start as a task of
    /// its own.
    #[serde(default)]
    pub new_work: Vec<NewWork>,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ReportStatus {
    /// What was asked is done.
    Done,
    /// It can't go on without the person's answer or action.
    NeedsYou,
    /// It stopped with work left that it could do itself.
    Unfinished,
}

/// A piece of work an agent proposes starting as a new task.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct NewWork {
    pub title: String,
    /// What to do, written for an agent that never saw the conversation.
    pub brief: String,
    pub mode: TaskMode,
    /// The GitHub issue the agent filed for it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
    pub issue: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct ChangedFile {
    pub path: String,
    pub change: FileChange,
    pub added: Option<u32>,
    pub removed: Option<u32>,
    /// Bounded patch reported by a successful native file tool, not a fresh disk diff.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub diff: Option<ReportedDiff>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub provenance: Option<Provenance>,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum FileChange {
    Added,
    Modified,
    Deleted,
    Renamed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct CheckResult {
    pub name: String,
    pub command: Option<String>,
    pub status: CheckStatus,
    pub detail: Option<String>,
    pub provenance: Provenance,
    /// Only populated when the native tool supplies a numeric exit status.
    /// It is the whole command line's: `status` says whether it is the
    /// check's own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub exit_code: Option<i32>,
    /// The end of what it printed, as the native tool reported it: a few
    /// lines, cut to a bound and masked like the rest of the outcome.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub output: Option<String>,
    /// Whether it ran after the run's last recorded file change. Absent on
    /// checks recorded before this was kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub after_last_change: Option<bool>,
}

/// Whether a run's work was checked after its last change, read from the
/// checks Ophio saw the agent's tools run, never from what the agent said.
/// `RunState::Completed` only says the agent stopped normally.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct RunVerification {
    pub status: VerificationStatus,
    /// The checks that ran after the last recorded file change, each
    /// command line counted once by its latest result.
    pub passed: u32,
    pub failed: u32,
    pub unknown: u32,
    /// Checks whose latest result failed before the last change and that
    /// weren't run again: still failing as far as anyone saw.
    #[serde(default, skip_serializing_if = "is_zero")]
    #[ts(as = "Option<u32>", optional)]
    pub failed_not_rerun: u32,
    /// A check failed before the run changed any file: the work it started
    /// from was already failing.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[ts(as = "Option<bool>", optional)]
    pub failed_before_changes: bool,
    /// Why it can't be verified though no check failed: evidence of a change
    /// or a check that Ophio couldn't keep.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub incomplete: Option<String>,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum VerificationStatus {
    /// At least one check ran after the last change, and every one passed.
    Verified,
    /// A check that ran after the last change failed.
    Failed,
    /// None failed, but at least one has no result of its own, a check that
    /// failed earlier wasn't run again, or evidence was lost.
    Unknown,
    /// No check ran after the last change.
    NotChecked,
}

/// That a later run on the same task had to change things after this run
/// finished as done. Recorded on this run when the later one ends.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct FollowUp {
    pub run_id: RunId,
    pub at: Timestamp,
    pub reason: FollowUpReason,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum FollowUpReason {
    /// A check failed in the later run before it changed anything.
    CheckFailed,
    /// The later run changed files.
    ChangedFiles,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct ReportedDiff {
    pub text: String,
    pub truncated: bool,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum CheckStatus {
    Passed,
    Failed,
    NotRun,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct UsageSummary {
    #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
    pub input_tokens: Option<u64>,
    #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
    pub output_tokens: Option<u64>,
    #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
    pub duration_ms: Option<u64>,
    /// Native cost figure if the harness reports one; subscription usage is
    /// not converted into money.
    pub reported_cost: Option<String>,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ApprovalState {
    Pending,
    Allowed,
    Denied,
    Expired,
    /// Resolved elsewhere (terminal, another device, chat) — shows the actual
    /// recorded outcome.
    ResolvedElsewhere,
    Cancelled,
}

/// A decision explicitly reported by the native tool; never inferred from progress.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ApprovalOutcome {
    Allowed,
    Denied,
}

/// One line of an approval's `detail`. Code (a command, path, pattern or URL)
/// is shown exactly as written; the rest reads as prose.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct ApprovalDetailLine {
    /// What the text is, e.g. "Pattern".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub label: Option<String>,
    pub text: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[ts(as = "Option<bool>", optional)]
    pub code: bool,
    /// Set on the lines of a proposed file change, which read as a diff.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub change: Option<ApprovalLineChange>,
}

/// How a line of a proposed file change differs from the file as it is.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ApprovalLineChange {
    Unchanged,
    Removed,
    Added,
    /// Lines left out of the preview; `text` says how many and which.
    Omitted,
}

/// A multiple-choice question the agent asked. The person may also answer
/// in their own words.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct ApprovalQuestion {
    /// Opaque identity within this request, assigned by the adapter, never native text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub id: Option<String>,
    /// Short label, e.g. "Rounding".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub header: Option<String>,
    pub question: String,
    pub options: Vec<ApprovalQuestionOption>,
    /// Several options may be chosen together.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[ts(as = "Option<bool>", optional)]
    pub multi_select: bool,
    /// The answer is secret, like a password: typed hidden, and never
    /// asked for from a notification.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[ts(as = "Option<bool>", optional)]
    pub secret: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct ApprovalQuestionOption {
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct Approval {
    pub approval_id: ApprovalId,
    pub run_id: RunId,
    pub task_id: TaskId,
    pub project_id: ProjectId,
    /// Native request identity (tool-use id, JSON-RPC id).
    pub native_request_id: String,
    pub kind: ApprovalKind,
    /// Human-readable description of the requested effect.
    pub title: String,
    pub detail: Option<String>,
    /// `detail` line by line, when its source says which parts are code.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(as = "Option<Vec<ApprovalDetailLine>>", optional)]
    pub detail_lines: Vec<ApprovalDetailLine>,
    /// The agent's questions, when it asked multiple-choice ones; `detail`
    /// then lists them as text.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(as = "Option<Vec<ApprovalQuestion>>", optional)]
    pub questions: Vec<ApprovalQuestion>,
    /// Exact command/tool input, when the native harness supplies it.
    pub raw_request: Option<String>,
    /// What granting would actually permit (e.g. "this one command").
    pub scope: String,
    /// `title` or `detail` was cut to a readable length; `raw_request` is whole.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[ts(as = "Option<bool>", optional)]
    pub shortened: bool,
    pub revision: u32,
    pub state: ApprovalState,
    /// Last decision hand-off. A failed delivery leaves the request pending.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub delivery: Option<ApprovalDelivery>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub outcome: Option<ApprovalOutcome>,
    pub requested_at: Timestamp,
    pub expires_at: Option<Timestamp>,
    pub resolved_by: Option<Principal>,
    pub resolved_at: Option<Timestamp>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct ApprovalDelivery {
    pub decision: crate::command::ApprovalDecision,
    pub at: Timestamp,
    /// None means handed to the native route, not an agent acknowledgement.
    pub error: Option<crate::ProtocolError>,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ApprovalKind {
    Command,
    FileEdit,
    Network,
    Tool,
    /// A question from the agent that needs an answer rather than yes/no.
    Question,
    /// Supervised GUI access requested by a background task.
    ComputerUse,
    /// A task asks to use an agent login set to "Ask me first".
    LoginUse,
    /// A task asks the owner for a code a site sent them. It is answered
    /// with `AgentLoginAction::AnswerCode`, never in the approval itself.
    LoginCode,
    /// A task asks to post on a GitHub repository the account doesn't own,
    /// or to open a pull request past the project's limit.
    GithubPost,
    Other,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum AttentionKind {
    Approval,
    Question,
    Failure,
    Completed,
    ReconciliationRequired,
    WaitingForYou,
    Conflict,
    ConnectionIssue,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
/// Something that needs a person, or tells them of an outcome. Once
/// resolved it stays resolved: anything raised again is a new item with a new
/// id, so a phone never has to tell a second opening from the first.
pub struct AttentionItem {
    /// A helper-specific unknown, distinct from an unknown parent outcome.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub child_id: Option<ChildId>,
    pub attention_id: AttentionId,
    pub kind: AttentionKind,
    pub title: String,
    pub body: Option<String>,
    pub task_id: Option<TaskId>,
    pub run_id: Option<RunId>,
    pub approval_id: Option<ApprovalId>,
    pub created_at: Timestamp,
    pub resolved_at: Option<Timestamp>,
    /// Higher is more urgent. Approvals/questions outrank completions.
    pub priority: u8,
    /// What the alert is about, when its kind doesn't say: a merged pull
    /// request, say, rather than a finished task. See [`Self::topic`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub topic: Option<crate::notifications::AlertTopic>,
    /// Raised without an alert of its own: it is part of something that
    /// alerts once, as a whole, when it is through (Run all, say), or the
    /// person was already told of it. Listed and counted as any other.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[ts(as = "Option<bool>", optional)]
    pub quiet: bool,
    /// The problem report the item is about. With no task, it is either
    /// for the reporter, a question to answer with `AnswerReportQuestion`
    /// or news of its progress, or, as `waiting_for_you`, one the owner's
    /// intake received, for them to read. With a task, a fix is ready.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub report_id: Option<ReportId>,
}

impl AttentionItem {
    /// What the item's alert is about; none for one that raises no alert.
    pub fn topic(&self) -> Option<crate::notifications::AlertTopic> {
        self.topic
            .or_else(|| crate::notifications::AlertTopic::of(self.kind))
    }
}

/// A reviewed plan carried from an earlier run of the same task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct CarriedPlan {
    /// The run that proposed the plan.
    pub run_id: RunId,
    /// The plan exactly as quoted at the end of the message.
    pub text: String,
}

/// One entry in a task's Talk timeline.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct TimelineItem {
    #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
    pub seq: u64,
    pub at: Timestamp,
    pub run_id: Option<RunId>,
    pub item: TimelineBody,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(export)]
pub enum TimelineBody {
    /// What the person sent (text and/or annotation bundle).
    UserMessage {
        text: String,
        bundle: Option<Box<AnnotationBundle>>,
        delivery: DeliveryState,
        /// The earlier run's plan that `text` ends with, recorded only when
        /// the companion carried that plan into Work. Clients show a plan
        /// from this record, never from markers in the text alone.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        carried_plan: Option<CarriedPlan>,
    },
    /// A later delivery state for the person's message at `message_seq` in
    /// the same task. Clients apply it to that message and never show it as
    /// a row of its own.
    Delivery {
        #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
        message_seq: u64,
        state: DeliveryState,
        /// A `Cancelled` message that was still held when Stop ended its run.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        #[ts(as = "Option<bool>", optional)]
        withdrawn_by_stop: bool,
        /// A `Cancelled` message that was still held when the chat that sent
        /// it had its access revoked.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        #[ts(as = "Option<bool>", optional)]
        withdrawn_by_revocation: bool,
    },
    /// Agent prose (Markdown). `partial` while streaming.
    AgentMessage {
        text: String,
        partial: bool,
    },
    /// Tool/activity summary.
    Activity {
        tool: String,
        summary: String,
        status: ActivityStatus,
        detail: Option<String>,
        /// The native tool call this reports on; later reports with the same
        /// key update the same call.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        key: Option<String>,
        /// A failed call that never ran, and why: the failure is then the
        /// person's decision, not the tool's.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        not_run: Option<NotRun>,
    },
    Approval {
        approval_id: ApprovalId,
    },
    RunStarted {
        run_id: RunId,
    },
    RunEnded {
        run_id: RunId,
        state: RunState,
    },
    Receipt {
        receipt: Box<InterventionReceipt>,
    },
    Note {
        text: String,
    },
    /// Something the agent put in front of the person through Ophio.
    Shown {
        shown: Shown,
    },
}

/// What an agent showed the person, or asked them to do, through Ophio.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[serde(tag = "what", rename_all = "snake_case")]
#[ts(export)]
pub enum Shown {
    /// A picture or video from the project folder, kept as an artifact.
    Media {
        artifact_id: ArtifactId,
        media_type: String,
        /// Where the file was, relative to the project folder.
        path: String,
        caption: Option<String>,
        /// What the agent asked the person to do with it.
        invite: Option<MediaInvite>,
    },
    /// A web page the agent serves, now among the project's previews.
    Preview {
        preview_id: PreviewId,
        label: String,
        url: String,
        note: Option<String>,
        /// What the agent asks about the page. The person answers them in
        /// the preview, choosing options or writing their own words, and
        /// sends the answers on as a message.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        #[ts(as = "Option<Vec<ApprovalQuestion>>", optional)]
        questions: Vec<ApprovalQuestion>,
    },
    /// Something the agent suggested the person do in Ophio.
    Invitation {
        action: PersonAction,
        note: String,
        /// For computer use, the goal the agent suggests, filled in when the
        /// person opens it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        goal: Option<String>,
    },
    /// Items the agent put on the project's checklist, which opens from it.
    Checklist {
        title: String,
        /// The items added or changed, by title.
        items: Vec<String>,
        /// How many of them the agent waits on.
        waiting: u32,
    },
}

/// How the agent asked the person to answer a picture or video.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum MediaInvite {
    /// Draw on it and comment.
    Draw,
    /// Watch the video and talk over it; what is said keeps its moment.
    Talk,
}

/// Something in Ophio an agent can ask the person to do.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum PersonAction {
    /// Watch the computer's screen live.
    WatchDesktop,
    /// Take a screenshot of the computer and draw on it.
    Capture,
    /// Send files from the phone.
    SendFiles,
    /// Have an agent use one of the computer's screens while they watch.
    ComputerUse,
    /// Control the computer by voice, with Jev.
    VoiceControl,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ActivityStatus {
    Running,
    Succeeded,
    Failed,
    Unknown,
}

/// Why a tool call reported as failed never ran.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum NotRun {
    /// The person denied the request to run it.
    Denied,
    /// Stop cancelled the request to run it before anyone answered.
    Stopped,
}

/// Delivery of a person's message/steering to the native harness.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum DeliveryState {
    HostAccepted,
    /// Held until the native harness reaches a safe boundary.
    QueuedForBoundary,
    NativeDelivered,
    Failed,
    Unknown,
    /// Removed from the adapter queue before any native write.
    Cancelled,
}

/// What an intervention (Stop, Take over, Pause, Private step) actually
/// achieved, fact by fact.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct InterventionReceipt {
    pub kind: InterventionKind,
    pub requested_at: Timestamp,
    pub facts: Vec<ReceiptFact>,
    /// Plain-language summary composed only from `facts`.
    pub summary: String,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum InterventionKind {
    StopTask,
    PauseInput,
    TakeOver,
    LeaveControl,
    PrivateStep,
    PauseQueue,
    LockDesktop,
    WindowAction,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct ReceiptFact {
    pub label: String,
    pub status: FactStatus,
    pub detail: Option<String>,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum FactStatus {
    Done,
    Pending,
    StillActive,
    NotApplicable,
    Failed,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct PreviewEndpoint {
    pub preview_id: PreviewId,
    pub project_id: ProjectId,
    pub label: String,
    /// URL reachable from the phone (e.g. tailnet address). Opened in the
    /// phone's ordinary browser, never in a privileged webview.
    pub url: String,
    pub reachable: Option<bool>,
    pub checked_at: Option<Timestamp>,
    /// Found rather than added: a site a program working in the project's
    /// folder serves on the host. It is marked stopped when its server stops
    /// and removed once it has stayed stopped a while.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[ts(as = "Option<bool>", optional)]
    pub automatic: bool,
    /// The command line that starts the page's server in the project's
    /// folder, as the agent that showed the page gave it. A server an agent
    /// starts stops with its run, so the person can have Ophio start it again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub start: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct ReadinessReport {
    pub checked_at: Timestamp,
    pub checks: Vec<ReadinessCheck>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct ReadinessCheck {
    pub area: ReadinessArea,
    pub label: String,
    pub status: CheckStatus,
    /// The exact reason, as the tool or platform gave it.
    pub detail: Option<String>,
    pub remedy: Option<String>,
    /// What the person can or cannot do, in plain words; `detail` then stays
    /// on request. Absent from older hosts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub summary: Option<String>,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ReadinessArea {
    Network,
    Terminal,
    Graphics,
    Accounts,
    Storage,
    Speech,
    Channels,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct ArtifactMeta {
    pub artifact_id: ArtifactId,
    pub media_type: String,
    #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
    pub size: u64,
    pub sha256: String,
    pub created_at: Timestamp,
    pub label: Option<String>,
    pub task_id: Option<TaskId>,
    /// Geometry returned by the capture backend, absent for imported artifacts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub capture: Option<Box<CaptureGeometry>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(as = "Option<Vec<ArtifactLink>>", optional)]
    pub links: Vec<ArtifactLink>,
}

/// A durable reference, independent of visible labels or picture timestamps.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct ArtifactLink {
    pub task_id: TaskId,
    pub run_id: Option<RunId>,
    pub annotation: Option<ArtifactAnnotation>,
    pub role: ArtifactRole,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct ArtifactAnnotation {
    pub bundle_id: AnnotationBundleId,
    pub revision: u32,
    pub mark_ids: Vec<MarkId>,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ArtifactRole {
    Source,
    Observation,
    /// Supplied as after-evidence; this is not verification of the request.
    After,
    /// Put in front of the person by the run's agent, to look at, draw on or
    /// talk over.
    Shown,
}

/// Paged, newest-first artifact history; survives journal retention and restart.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct TaskArtifactHistory {
    pub artifacts: Vec<ArtifactMeta>,
    pub next_before: Option<ArtifactId>,
}

#[cfg(test)]
mod compatibility_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn legacy_task_and_approval_rows_default_without_escalation_or_invented_outcomes() {
        let id = uuid::Uuid::nil().to_string();
        let task: Task = serde_json::from_value(json!({
            "task_id":id,"project_id":id,"kind":"coding","title":"Existing task",
            "state":"completed","revision":1,"harness":"codex","model":null,
            "effort":null,"created_by":{"kind":"local_owner"},"created_at":0,"updated_at":0,
            "current_run":null,"target_id":null,"mission":null,"attention_count":0,"status_line":null
        })).unwrap();
        assert_eq!(task.mode, TaskMode::Work);
        let approval: Approval = serde_json::from_value(json!({
            "approval_id":id,"run_id":id,"task_id":id,"project_id":id,"native_request_id":"7",
            "kind":"command","title":"Command","detail":null,"raw_request":null,"scope":"once",
            "revision":1,"state":"resolved_elsewhere","requested_at":0,"expires_at":null,
            "resolved_by":null,"resolved_at":0
        }))
        .unwrap();
        assert_eq!(approval.outcome, None);
        assert!(!approval.shortened);
    }

    #[test]
    fn a_carried_plan_is_stored_on_the_message_and_old_messages_load_without_one() {
        let old: TimelineBody = serde_json::from_value(json!({
            "kind":"user_message","text":"Go\n\n--- Plan from run 1, proposed by the agent ---\nStep\n--- End of plan ---",
            "bundle":null,"delivery":"host_accepted"
        }))
        .unwrap();
        assert!(matches!(
            old,
            TimelineBody::UserMessage {
                carried_plan: None,
                ..
            }
        ));
        assert_eq!(
            serde_json::to_value(&old).unwrap().get("carried_plan"),
            None
        );
        let carried = TimelineBody::UserMessage {
            text: "Go".into(),
            bundle: None,
            delivery: DeliveryState::HostAccepted,
            carried_plan: Some(CarriedPlan {
                run_id: RunId::new(),
                text: "Step".into(),
            }),
        };
        let stored = serde_json::to_value(&carried).unwrap();
        assert_eq!(stored["carried_plan"]["text"], "Step");
        assert_eq!(
            serde_json::from_value::<TimelineBody>(stored).unwrap(),
            carried
        );
    }
}

/// Frame provenance recorded before encoding or storing a capture.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct CaptureGeometry {
    pub target_id: TargetId,
    pub geometry: crate::control::TargetGeometry,
    /// Host time immediately after the backend returned the frame.
    pub captured_at: Timestamp,
}
/// Repository observations made around dispatch and exit, not file verification.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct RunRevisions {
    pub before: Option<RepositoryRevision>,
    pub after: Option<RepositoryRevision>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct RepositoryRevision {
    pub commit: String,
    pub observed_at: Timestamp,
    /// None when the working tree could not be inspected.
    pub dirty: Option<bool>,
}
