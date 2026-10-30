// SPDX-License-Identifier: Apache-2.0
//! How the person likes agents to work with them, in every task on the
//! computer.
use crate::{capability::HarnessKind, model::LaunchPolicy};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct AgentPreferences {
    /// Agents may ask them questions. Off, agents make the best choice they
    /// can, say what they chose, and ask only when they truly can't choose.
    pub questions: bool,
    /// The launch flags work starts with when nobody chose them, such as
    /// work an agent starts: at most one for each agent. The person's phone
    /// sets them from its launch defaults. An agent without one starts
    /// without flags, as before. Advertised as `launch_defaults_v1`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(as = "Option<Vec<LaunchDefault>>", optional)]
    pub launch_defaults: Vec<LaunchDefault>,
    #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
    pub revision: u64,
}

/// The launch flags an agent starts with by default.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[ts(export)]
pub struct LaunchDefault {
    pub harness: HarnessKind,
    pub policy: LaunchPolicy,
}

impl Default for AgentPreferences {
    fn default() -> Self {
        Self {
            questions: true,
            launch_defaults: Vec::new(),
            revision: 0,
        }
    }
}
