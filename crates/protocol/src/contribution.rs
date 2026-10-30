// SPDX-License-Identifier: Apache-2.0
//! Contribution import over the authenticated control connection. Portable
//! records are inert bytes; local review is a separate Manage-authorized command.
use crate::{HostId, LibraryRootId, ProjectId};
use serde::{Deserialize, Serialize};

pub const CONTRACT: &str = "ch-import-draft-1";
pub const RECORD_BYTES: usize = 65_536;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct ContributionInput {
    /// Exact UTF-8 bytes, including whitespace. Never reserialize before hashing.
    pub packet_json: String,
    pub handoff_json: String,
    /// Receiver-local selection, outside the portable records.
    pub host_id: HostId,
    pub root_id: LibraryRootId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct ContributionApproval {
    /// Independently authenticated publisher, chosen by the local reviewer.
    pub publisher_id: String,
    pub packet_sha256: String,
    pub handoff_sha256: String,
    /// Independently reviewed catalog identity, not copied from untrusted input.
    pub game_id: String,
    /// Independently reviewed catalog label; never taken from portable input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub display_name: Option<String>,
    pub release_id: String,
    pub repository_url: String,
    pub commit: String,
    /// Unix seconds of the reviewer's fresh packet/catalog revocation check.
    #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
    pub checked_at: u64,
    /// At most five minutes after checked_at, and no later than packet expiry.
    #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
    pub valid_until: u64,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ContributionCode {
    AwaitingLocalApproval,
    Acquiring,
    Verifying,
    Prepared,
    Reopened,
    Canceled,
    ReconcileRequired,
    #[serde(rename = "invalid_record")]
    Malformed,
    InvalidJson,
    UnsupportedVersion,
    NotYetValid,
    InvalidLifetime,
    CatalogDigestMismatch,
    LimitExceeded,
    #[serde(rename = "untrusted_packet")]
    PacketUntrusted,
    Expired,
    FreshnessUnknown,
    #[serde(rename = "handoff_digest_mismatch")]
    HandoffMismatch,
    #[serde(rename = "release_mismatch")]
    IdentityMismatch,
    #[serde(rename = "stale_policy")]
    PolicyMismatch,
    CompatibilityUnknown,
    CompatibilityUnsupported,
    HostUnavailable,
    RootChanged,
    TrustRequired,
    IdConflict,
    WorkspaceConflict,
    UnsafeRepository,
    UnsafePath,
    UnsafeObject,
    PrivateDestination,
    AcquisitionFailed,
    DiskFull,
    Busy,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct ContributionView {
    /// Opaque receiver-local operation identity; never a filesystem path.
    pub operation_id: String,
    pub code: ContributionCode,
    pub packet_sha256: String,
    pub handoff_sha256: String,
    /// Validated structure, still untrusted until a separate local approval.
    pub context: Option<ContributionContext>,
    pub project_id: Option<ProjectId>,
    pub dirty: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub conflict: Option<ContributionConflict>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub space: Option<ContributionSpace>,
    /// Validated capability identifiers, at most 18, never raw diagnostics.
    #[serde(default)]
    pub missing_capabilities: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
    pub received_bytes: Option<u64>,
    /// Cancellation is recorded but a worker has not yet confirmed stopping.
    pub interruption_pending: bool,
    /// Always false. Setup and task starts use their existing authorization.
    pub execution_authorized: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct ContributionContext {
    pub game_id: String,
    /// Independently reviewed catalog label; never taken from portable input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub display_name: Option<String>,
    pub release_id: String,
    pub repository_url: String,
    pub commit: String,
    pub policies: Vec<ContributionPolicy>,
    pub task: Option<ContributionTaskReference>,
}

#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
    ts_rs::TS,
)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct ContributionPolicy {
    pub path: String,
    pub sha256: String,
    pub role: String,
}

/// Digest-checked UTF-8 bytes from the pinned commit, never executable content
/// or evidence of publisher authenticity. Returned one file at a time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct ContributionPolicyDocument {
    pub policy: ContributionPolicy,
    pub size_bytes: u32,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct ContributionTaskReference {
    pub id: String,
    pub revision: String,
    pub url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct ContributionCapabilities {
    pub contract: String,
    pub transport: String,
    pub host_id: HostId,
    pub companion_version: String,
    /// Optional build provenance supplied by the build, never inferred from an install.
    pub source_revision: Option<String>,
    pub supported: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub unsupported_reason: Option<ContributionUnsupportedReason>,
    /// Whole, writable Library roots eligible for phone-side selection.
    #[serde(default)]
    pub root_ids: Vec<LibraryRootId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub contributions_root_id: Option<LibraryRootId>,
    pub capabilities: Vec<String>,
    pub record_bytes: u32,
    pub transfer_bytes: u32,
    pub expanded_bytes: u32,
    pub entries: u32,
    pub total_seconds: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct ContributionConflict {
    /// Counts relative to the imported tree. Unknown when inspection is unsafe.
    pub changed: Option<u32>,
    pub untracked: Option<u32>,
    pub branch: Option<String>,
    pub revision: Option<String>,
    pub project_id: Option<ProjectId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct ContributionSpace {
    #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
    pub needed_bytes: u64,
    #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
    pub available_bytes: u64,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ContributionUnsupportedReason {
    UnsupportedPlatform,
    SafeFilesystemUnavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[serde(tag = "choice", rename_all = "snake_case")]
#[ts(export)]
pub enum ContributionWorkspaceChoice {
    /// Open this registered managed project unchanged, without adopting new policy.
    UseExisting { project_id: ProjectId },
    /// Acquire a new sibling under the same allowed root; preserve the old checkout.
    CreateSeparate,
}
