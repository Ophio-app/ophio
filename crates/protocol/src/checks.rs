// SPDX-License-Identifier: Apache-2.0
//! Person-approved project checks. Agent proposals never replace approved definitions.

use crate::{ids::*, time::Timestamp};
use serde::{Deserialize, Serialize};

pub const CHECKS_FEATURE: &str = "checks_v1";
fn default_timeout() -> u32 {
    600
}
fn default_enabled() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct Check {
    pub check_id: CheckId,
    pub name: String,
    pub about: String,
    pub run: String,
    #[serde(default)]
    pub setup: Option<String>,
    #[serde(default)]
    pub paths: Vec<String>,
    #[serde(default)]
    pub when: CheckWhen,
    #[serde(default = "default_timeout")]
    pub timeout_secs: u32,
    #[serde(default)]
    pub network: bool,
    #[serde(default)]
    pub inputs: Vec<String>,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// Measured by Ophio, never accepted from an agent proposal.
    #[serde(default)]
    pub typical_secs: Option<f64>,
}

#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
    ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum CheckWhen {
    #[default]
    EveryTask,
    BeforeMerge,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct CheckInputFingerprint {
    pub check_id: CheckId,
    pub path: String,
    /// None records a missing input; it is not the hash of empty content.
    pub sha256: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(export)]
pub enum ChecksChangedBy {
    Person {
        device_id: Option<DeviceId>,
    },
    Draft {
        draft_id: CheckDraftId,
        device_id: Option<DeviceId>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct ProjectChecks {
    pub project_id: ProjectId,
    #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
    pub version: u64,
    pub checks: Vec<Check>,
    pub input_fingerprints: Vec<CheckInputFingerprint>,
    pub changed_at: Timestamp,
    pub changed_by: ChecksChangedBy,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(export)]
pub enum CheckDraftItem {
    Add { check: Check, note: String },
    Change { check: Check, note: String },
    Remove { check_id: CheckId, reason: String },
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ChecksDraftState {
    Proving,
    Ready,
    Applied,
    Discarded,
    Stale,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct ChecksDraft {
    pub id: CheckDraftId,
    pub project_id: ProjectId,
    #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
    pub base_version: u64,
    pub task_id: Option<TaskId>,
    pub request: Option<String>,
    pub items: Vec<CheckDraftItem>,
    /// Proofs are written only by Ophio. An agent's account is not proof.
    pub proofs: Vec<CheckResult>,
    pub state: ChecksDraftState,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(export)]
pub enum CheckEdit {
    SetEnabled { check_id: CheckId, enabled: bool },
    Delete { check_id: CheckId },
    Save { check: Check },
    Undo,
    KeepInputs { check_id: CheckId },
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum CheckJobState {
    Pending,
    Running,
    Cancelling,
    Finished,
    Uncertain,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(export)]
pub enum CheckJobPurpose {
    AfterRun,
    RunNow { check_ids: Vec<CheckId> },
    DraftProof { draft_id: CheckDraftId },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct CheckProcessIdentity {
    pub pid: u32,
    /// Operating-system process creation identity, not the job's wall clock.
    pub start_time: String,
    pub sandbox_unit: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(export)]
pub enum CheckSandboxLevel {
    Contained,
    ContainedWithoutNetworkLimit { reason: String },
    Unsandboxed { reason: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(export)]
pub enum CheckOutcome {
    Passed,
    Failed,
    TimedOut,
    CouldNotRun { reason: String },
    SkippedPaths,
    Off,
    Stopped,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export, rename = "OphioCheckResult")]
pub struct CheckResult {
    pub check_id: CheckId,
    pub name: String,
    pub outcome: CheckOutcome,
    pub duration_secs: f64,
    pub exit_code: Option<i32>,
    /// Combined output tail, at most 8192 bytes, cut at a UTF-8 boundary.
    pub output: String,
    pub inputs_changed: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct CheckJob {
    pub id: CheckJobId,
    pub project_id: ProjectId,
    pub task_id: Option<TaskId>,
    pub run_id: Option<RunId>,
    pub purpose: CheckJobPurpose,
    #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
    pub checks_version: u64,
    /// Frozen definitions ensure later approvals cannot change an admitted job.
    pub checks: Vec<Check>,
    pub state: CheckJobState,
    pub process: Option<CheckProcessIdentity>,
    pub created_at: Timestamp,
    pub started_at: Option<Timestamp>,
    pub finished_at: Option<Timestamp>,
    pub sandbox: Option<CheckSandboxLevel>,
    pub results: Vec<CheckResult>,
    /// Explains stopping, restart uncertainty or a failure to start.
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct TaskChecking {
    pub job_id: CheckJobId,
    pub started_at: Timestamp,
    pub done: u32,
    pub total: u32,
}

/// Factual text computed by Ophio, not agent-authored prose.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct CheckFacts {
    pub when: String,
    pub internet: String,
    pub time_limit: String,
    pub typical_time: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct CheckView {
    pub check: Check,
    pub facts: CheckFacts,
    pub last_result: Option<CheckResult>,
    pub last_task_id: Option<TaskId>,
    pub last_task_title: Option<String>,
    pub last_finished_at: Option<Timestamp>,
    pub inputs_changed: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct ChecksDraftView {
    pub draft: ChecksDraft,
    /// One list per item, computed by Ophio against the base approved version.
    pub differences: Vec<Vec<String>>,
    /// One factual description per item; removals have None.
    pub facts: Vec<Option<CheckFacts>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct ProjectChecksView {
    pub project_id: ProjectId,
    pub project_name: String,
    pub approved: ProjectChecks,
    pub checks: Vec<CheckView>,
    pub drafts: Vec<ChecksDraftView>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub unfinished_job: Option<CheckJob>,
}
