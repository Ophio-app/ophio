// SPDX-License-Identifier: Apache-2.0
//! Øphio's own copies of the agents' programs, kept up to date when the
//! owner turns that on.
use crate::{capability::HarnessKind, time::Timestamp};
use serde::{Deserialize, Serialize};

/// Where Øphio's copy of one agent stands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct AgentCopy {
    pub harness: HarnessKind,
    /// The version tasks start with, `None` before the first copy.
    pub version: Option<String>,
    /// When Øphio last asked the agent's maker for its newest version.
    pub checked_at: Option<Timestamp>,
    /// Why the last check or download didn't finish, in words for the owner.
    pub problem: Option<String>,
}

#[derive(
    Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[ts(export)]
pub struct AgentUpdatesView {
    /// Whether Øphio keeps its own up-to-date copies; off, tasks use the
    /// programs installed on the computer.
    pub enabled: bool,
    #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
    pub revision: u64,
    /// Whether this computer can have copies: its system is one the agents'
    /// makers publish programs for.
    pub supported: bool,
    /// A check is under way.
    pub checking: bool,
    /// Each agent Øphio keeps a copy of or last tried to; agents not
    /// installed on the computer aren't added.
    pub agents: Vec<AgentCopy>,
}
