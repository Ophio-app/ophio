// SPDX-License-Identifier: Apache-2.0
//! Human decisions about project stages, independent of execution authority.
use crate::{ProjectId, auth::Principal, time::Timestamp};
use serde::{Deserialize, Serialize};

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ProjectStage {
    Planning,
    Design,
    Prototype,
    Review,
    Implementation,
    Released,
    Paused,
}

impl ProjectStage {
    /// Ordered choices returned by the companion; clients need no stage catalog.
    pub const ALL: [Self; 7] = [
        Self::Planning,
        Self::Design,
        Self::Prototype,
        Self::Review,
        Self::Implementation,
        Self::Released,
        Self::Paused,
    ];
}

pub const STAGE_NOTE_LIMIT: u32 = 500;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct ProjectStageTransition {
    #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
    pub revision: u64,
    pub from: Option<ProjectStage>,
    pub to: ProjectStage,
    pub note: Option<String>,
    pub at: Timestamp,
    /// Authenticated identity, never supplied by the command payload.
    pub principal: Principal,
    /// Name as recorded at the time, retained if the device is renamed/revoked.
    pub actor_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct ProjectStageHistory {
    pub project_id: ProjectId,
    /// None means no person has set a stage; its revision is zero.
    pub current: Option<ProjectStageTransition>,
    /// Newest first. Continue with the last entry's revision as before_revision.
    pub transitions: Vec<ProjectStageTransition>,
    pub has_more: bool,
    pub stages: Vec<ProjectStage>,
    pub note_limit: u32,
}
