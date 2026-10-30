// SPDX-License-Identifier: Apache-2.0
//! Reviews: the project rules a review followed, what it found, how a fresh
//! verifier judged each finding, how far corrections have been rechecked, and
//! what only the person can still decide.
//!
//! A review's own words never decide anything here. Rules come from the
//! trusted base revision, verdicts from a separate verifier, and a change
//! that touches something sensitive waits for the person to approve its
//! exact revision whatever any agent says.
use crate::github::LaunchChoice;
use crate::ids::TaskId;
use crate::time::Timestamp;
use serde::{Deserialize, Serialize};

/// What a review task is for. Set by whoever creates it, never by an agent
/// or by repository text. Advertised as `review_rules_v1`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ReviewRole {
    /// Look for problems: a finder run, a check before merging, or Find &
    /// fix without GitHub.
    Find,
    /// In a fresh context, try to disprove what a review found before
    /// anything is filed, fixed or posted.
    Verify,
    /// Recheck the corrections made for earlier findings, and review what
    /// else changed since the last review.
    Recheck,
}

/// The project review rules a review was given, so the person can see what
/// it followed.
#[derive(
    Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[ts(export)]
pub struct ReviewRulesUsed {
    /// The commit the rules were read from: the base the change goes into,
    /// never the change under review.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub revision: Option<String>,
    /// Where that commit came from, such as `origin/main`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub source: Option<String>,
    /// The rule files read, as repository paths, the root's first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(as = "Option<Vec<String>>", optional)]
    pub files: Vec<String>,
    /// The change under review edits review rules. Its edits were not
    /// followed; the base's rules were.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[ts(as = "Option<bool>", optional)]
    pub ignored_changes: bool,
    /// Some rule text was left out to stay within the size limit.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[ts(as = "Option<bool>", optional)]
    pub truncated: bool,
    /// Why no rules were used, when none were: the project has no
    /// `REVIEW.md`, or it couldn't be read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub missing: Option<String>,
}

/// How serious a finding is, as the reviewer and verifier judged it.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum FindingSeverity {
    High,
    Medium,
    Low,
}

/// What the verifier made of a finding.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum FindingVerdict {
    /// A real problem in this change: filed, fixed or posted.
    Confirmed,
    /// Not a problem: intended, already there before the change, or the
    /// path can't produce the harm. Kept privately with the reason.
    Rejected,
    /// Couldn't be settled either way. Kept privately with the reason; not
    /// filed or fixed by itself.
    Uncertain,
}

/// What a confirmed finding rests on. Reading code is never called a test.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum EvidenceKind {
    /// The verifier made it happen: a test, a command or a run showed it.
    Reproduced,
    /// The verifier followed the code and reasoned it through, without
    /// running anything that showed it.
    SourceReasoning,
}

/// What became of a finding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[serde(tag = "state", rename_all = "snake_case")]
#[ts(export)]
pub enum FindingDisposition {
    /// Confirmed and still to be dealt with.
    Open,
    /// Filed as issue `number`.
    Filed {
        #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
        number: u64,
    },
    /// A fixer is correcting it.
    Fixing,
    /// A later review saw it corrected.
    Corrected,
    /// Not acted on: rejected, uncertain, a duplicate, or over the limit.
    SetAside,
}

/// One finding with everything needed to judge it later, kept whatever the
/// verdict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct ReviewFinding {
    pub title: String,
    /// Repository path the problem is in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub line: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub severity: Option<FindingSeverity>,
    /// The concrete inputs and state that lead to the wrong outcome.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub scenario: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub expected: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub actual: Option<String>,
    /// None until the verifier has judged it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub verdict: Option<FindingVerdict>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub evidence: Option<EvidenceKind>,
    /// The verifier's reason, always given for rejected and uncertain ones.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub reason: Option<String>,
    /// The commit it was found at.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub revision: Option<String>,
    pub disposition: FindingDisposition,
    /// The round of review it was found in, from 1.
    #[serde(default)]
    pub round: u32,
}

/// A kind of change that waits for the person to approve its exact
/// revision before it merges.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[serde(tag = "area", rename_all = "snake_case")]
#[ts(export)]
pub enum SensitiveArea {
    Permissions,
    PairingOrLogin,
    Credentials,
    Sandboxing,
    Updates,
    Signing,
    ReleaseOrBuild,
    Installers,
    /// Changes to stored data that can't be undone, such as migrations.
    DataChanges,
    /// The review rules or this list themselves.
    ReviewPolicy,
    /// Named by the project's own review rules.
    ProjectRule {
        label: String,
    },
    /// The reviewer judged it sensitive, in its words. A reviewer can add a
    /// reason, never take one away.
    Reviewer {
        reason: String,
    },
    /// Ophio couldn't tell what the change touches, such as when its files
    /// couldn't be listed or there were too many, so it waits to be safe.
    Unchecked {
        reason: String,
    },
}

/// Why a change waits for the person's approval of its exact revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct ApprovalNeed {
    pub areas: Vec<SensitiveArea>,
    /// The changed paths that made it sensitive, a few at most.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(as = "Option<Vec<String>>", optional)]
    pub paths: Vec<String>,
    /// The commit these reasons describe. A newer commit needs a fresh
    /// look, and any approval given is for this commit alone.
    pub head: String,
}

/// Where a pull request's review stands, across rounds and restarts. It
/// replaces the old once-only check.
#[derive(
    Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[ts(export)]
pub struct PrReview {
    /// Rounds of review run so far: the first reviews the whole change, later
    /// ones the corrections and anything else changed since.
    #[serde(default)]
    pub round: u32,
    /// The commit the latest finished round covered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub reviewed_head: Option<String>,
    /// The commit a round under way is reviewing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub reviewing_head: Option<String>,
    /// Every finding across rounds, one per underlying problem.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(as = "Option<Vec<ReviewFinding>>", optional)]
    pub findings: Vec<ReviewFinding>,
    /// The rules the latest round followed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub rules: Option<ReviewRulesUsed>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub reviewer: Option<LaunchChoice>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub verifier: Option<LaunchChoice>,
    /// The task that reviewed the latest round, or is reviewing it now.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub task: Option<TaskId>,
    /// Why the review stopped and needs the person: a problem came back
    /// after its correction, or the rounds or time ran out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub escalated: Option<String>,
    /// Checks the reviewer or fixer said weren't run, in plain words.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(as = "Option<Vec<String>>", optional)]
    pub not_run: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub started_at: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub updated_at: Option<Timestamp>,
}

/// A task's part in a review, saved with it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct TaskReview {
    pub role: ReviewRole,
    /// The rules this task was given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub rules: Option<ReviewRulesUsed>,
    /// For a verifier or recheck, the review task whose findings it judges.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub of_task: Option<TaskId>,
    /// For a review, the verifier started for it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub verified_by: Option<TaskId>,
    /// What a finder or verifier run found and how each was judged,
    /// rejected and uncertain ones included.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(as = "Option<Vec<ReviewFinding>>", optional)]
    pub findings: Vec<ReviewFinding>,
    /// Checks the agent said it didn't run, in plain words.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(as = "Option<Vec<String>>", optional)]
    pub not_run: Vec<String>,
}
