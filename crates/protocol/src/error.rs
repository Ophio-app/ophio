// SPDX-License-Identifier: Apache-2.0
//! Typed errors. Codes preserve actionable cause: offline versus stale view,
//! missing permission versus unsupported backend, native failure versus an
//! uncertain outcome. Messages are human-readable and must never contain
//! secrets.

use serde::{Deserialize, Serialize};

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ErrorCode {
    /// Malformed frame or schema violation.
    InvalidRequest,
    /// Peer protocol major version is not supported.
    IncompatibleVersion,
    /// Not authenticated, or the device/channel credential was revoked.
    Unauthenticated,
    /// Authenticated but the grant for this operation is missing.
    PermissionDenied,
    /// Referenced entity does not exist (or is not visible to this principal).
    NotFound,
    /// Expected revision/generation no longer current.
    StaleRevision,
    /// Device permissions changed since the editor loaded its baseline.
    DeviceGrantsChanged,
    /// Removing this manager would strand other paired devices.
    LastManager,
    /// Approval/lease/request already resolved or expired.
    AlreadyResolved,
    Expired,
    /// The host backend does not implement this capability.
    Unsupported,
    /// Capability exists but OS permission is missing or was denied.
    OsPermissionMissing,
    /// Capability temporarily unavailable (session locked, backend busy…).
    TemporarilyUnavailable,
    /// Another owner currently holds the resource (writer, input lease).
    Conflict,
    /// A native harness/tool reported failure.
    NativeFailure,
    /// The effect may or may not have happened; reconciliation required.
    OutcomeUnknown,
    /// Harness account not signed in / quota state blocks the request.
    AccountNotReady,
    /// Path escapes an authorized root, is a symlink/reparse point to outside,
    /// or is otherwise not permitted.
    PathNotAllowed,
    /// Size or rate limit exceeded.
    LimitExceeded,
    /// Internal failure in Ophio on the computer; details are in the host log.
    Internal,
}

#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
    ts_rs::TS,
    thiserror::Error,
)]
#[error("{code:?}: {message}")]
#[ts(export)]
pub struct ProtocolError {
    pub code: ErrorCode,
    pub message: String,
    /// Optional structured detail for the UI (e.g. the current revision).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "unknown")]
    pub detail: Option<serde_json::Value>,
}

impl ProtocolError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            detail: None,
        }
    }

    pub fn with_detail(mut self, detail: serde_json::Value) -> Self {
        self.detail = Some(detail);
        self
    }
}
