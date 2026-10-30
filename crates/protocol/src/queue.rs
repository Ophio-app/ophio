// SPDX-License-Identifier: Apache-2.0
//! Next up: a per-project ordered queue of admitted tasks waiting for the
//! project's managed writer slot.

use serde::{Deserialize, Serialize};

use crate::auth::Principal;
use crate::ids::{ProjectId, QueueItemId, RunId, TaskId};
use crate::time::Timestamp;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct QueueView {
    pub project_id: ProjectId,
    /// Bumped on every reorder/add/remove; queue edits quote it.
    #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
    pub revision: u64,
    /// Paused queues never auto-start the next item.
    pub paused: bool,
    /// Why Next up is paused. Absent on older companions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub pause_reason: Option<String>,
    /// Run currently holding the writer slot.
    pub writer: Option<RunId>,
    pub items: Vec<QueueItem>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct QueueItem {
    pub item_id: QueueItemId,
    pub task_id: TaskId,
    pub position: u32,
    pub depends_on: Vec<QueueItemId>,
    pub authorization: QueueAuthorization,
    pub state: QueueItemState,
    pub added_by: Principal,
    pub added_at: Timestamp,
    /// Persisted request to review before starting. None on older companions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub request: Option<QueuedRequest>,
    /// A one-use automatic start permission expires at this host time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub authorization_expires_at: Option<Timestamp>,
}

/// Whether an item may start without a fresh confirmation when it reaches the
/// head of the queue.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum QueueAuthorization {
    /// Start automatically when the writer slot frees and dependencies passed.
    AutoStart,
    /// Raise an attention item and wait for the person to start it.
    AskBeforeStart,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[serde(tag = "state", rename_all = "snake_case")]
#[ts(export)]
pub enum QueueItemState {
    Waiting,
    /// A dependency failed/was removed, or the previous run needs
    /// reconciliation; the queue will not skip past it silently.
    Blocked {
        reason: String,
    },
    /// At the head and waiting for the person (ask-before-start).
    AwaitingStart,
    Started {
        run_id: RunId,
    },
}

impl QueueItem {
    pub fn is_pending(&self) -> bool {
        !matches!(self.state, QueueItemState::Started { .. })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct QueuedRequest {
    pub prompt: String,
    pub bundle: Option<Box<crate::annotation::AnnotationBundle>>,
}
