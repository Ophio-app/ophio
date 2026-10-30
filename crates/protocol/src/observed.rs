// SPDX-License-Identifier: Apache-2.0
//! Read-only observations of native sessions outside managed runs.
use crate::{
    ProjectId,
    capability::{Availability, HarnessKind},
    time::Timestamp,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct ObservedSession {
    /// Opaque, boot-bound identity. Never a PID supplied by the phone.
    pub observed_id: String,
    #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
    pub revision: u64,
    pub harness: HarnessKind,
    pub native_session: Option<String>,
    pub process_id: Option<u32>,
    pub executable: Option<String>,
    pub cwd: Option<String>,
    pub project_id: Option<ProjectId>,
    pub started_at: Option<Timestamp>,
    pub last_activity: Option<Timestamp>,
    pub observed_at: Timestamp,
    /// A session record alone never proves that its process is running.
    pub running: bool,
    pub stop: Availability,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct ObservedSessions {
    pub checked_at: Option<Timestamp>,
    pub sessions: Vec<ObservedSession>,
    pub coverage: Vec<String>,
    pub error: Option<String>,
}
