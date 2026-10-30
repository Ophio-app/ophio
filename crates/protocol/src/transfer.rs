// SPDX-License-Identifier: Apache-2.0
//! Resumable file transfers between phone and host.
//!
//! A transfer is declared by a command (`create_upload` / `create_download`),
//! then its bytes move on a dedicated authenticated transfer connection:
//! text frames carry [`TransferClientFrame`] / [`TransferServerFrame`], binary
//! frames carry chunk bytes in order starting at the negotiated offset. The
//! host verifies SHA-256 before a file becomes visible at its destination.

use serde::{Deserialize, Serialize};

use crate::ids::{ArtifactId, ProjectId, TransferId};
use crate::time::Timestamp;

/// Largest binary chunk either side sends.
pub const MAX_CHUNK_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct TransferRecord {
    pub transfer_id: TransferId,
    pub direction: TransferDirection,
    /// Internal attachment fetch. Older records have no purpose information.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub shown_in_task: Option<bool>,
    pub name: String,
    #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
    pub size: u64,
    /// Expected (upload) or actual (download) lowercase hex SHA-256.
    pub sha256: Option<String>,
    #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
    pub transferred: u64,
    pub state: TransferState,
    pub destination: Option<TransferDestination>,
    /// Inbox folder chosen at admission; persists across later config changes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub destination_folder: Option<String>,
    /// Publication location recorded before attempting the final move/link.
    /// Display only; its presence does not prove that publication succeeded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub publication: Option<TransferPublication>,
    /// Final host path once completed (display only).
    pub host_path: Option<String>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum TransferDirection {
    PhoneToHost,
    HostToPhone,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[serde(tag = "state", rename_all = "snake_case")]
#[ts(export)]
pub enum TransferState {
    Pending,
    InProgress,
    /// Interrupted; resumable from `transferred`.
    Paused,
    Verifying,
    /// Publication started but completion is unconfirmed. Inspect `publication`
    /// before cancelling to clear this record; this state cannot be resumed.
    ReconciliationRequired {
        reason: String,
    },
    Completed,
    Failed {
        reason: String,
    },
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(export)]
pub enum TransferDestination {
    /// The companion's inbox folder.
    Inbox,
    /// A directory inside a project (project-relative, must stay inside).
    ProjectDir { project_id: ProjectId, dir: String },
    /// Stored as an artifact (e.g. an annotation source image or clip).
    Artifact,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(export)]
pub enum TransferClientFrame {
    /// Begin/resume sending upload bytes. The host answers `resume_at`.
    Upload {
        transfer_id: TransferId,
    },
    /// Begin/resume receiving download bytes from `offset`.
    Download {
        transfer_id: TransferId,
        #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
        offset: u64,
    },
    /// All upload bytes sent.
    Finish {
        transfer_id: TransferId,
    },
    Cancel {
        transfer_id: TransferId,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(export)]
pub enum TransferServerFrame {
    /// Send upload bytes starting at this offset.
    ResumeAt {
        transfer_id: TransferId,
        #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
        offset: u64,
    },
    /// Download bytes follow, starting at `offset`, `size` total.
    Sending {
        transfer_id: TransferId,
        #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
        offset: u64,
        #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
        size: u64,
        sha256: String,
    },
    Completed {
        transfer_id: TransferId,
        sha256: String,
        artifact_id: Option<ArtifactId>,
    },
    Failed {
        transfer_id: TransferId,
        reason: String,
    },
}

/// Where to inspect a possibly published upload. Paths are display-only evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct TransferPublication {
    pub root: String,
    /// Path relative to `root`, including the collision-resolved filename.
    pub path: String,
}

/// A reviewed filename decision. Every publication still refuses an unexpected occupant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[serde(tag = "choice", rename_all = "snake_case")]
#[ts(export)]
pub enum UploadCollision {
    KeepBoth { name: String },
    Replace { revision: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct UploadDestinationCheck {
    pub exists: bool,
    pub keep_both_name: String,
    pub replace_revision: Option<String>,
    pub replace_unavailable: Option<String>,
}
