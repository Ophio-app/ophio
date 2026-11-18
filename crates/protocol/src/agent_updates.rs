// SPDX-License-Identifier: Apache-2.0
//! Owner-controlled checks and updates for Ophio's agent programs.
use crate::{capability::HarnessKind, time::Timestamp};
use serde::{Deserialize, Serialize};

pub const FEATURE: &str = "agent_update_policy_v1";

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
pub enum AgentUpdateMode {
    Automatic,
    #[default]
    Notify,
    Never,
}

/// Structured news about one agent. Kept outside the legacy alert enum so
/// older companions can still read the attention record after a rollback.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct AgentUpdateNotice {
    pub harness: HarnessKind,
    pub kind: AgentUpdateNoticeKind,
    pub version: Option<String>,
    pub models: Vec<String>,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum AgentUpdateNoticeKind {
    Behind,
    UpdateForModels,
    NewModelsAvailable,
}

/// What is known, not inferred, about one agent's programs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct AgentCopy {
    pub harness: HarnessKind,
    /// Compatibility field: the version of Ophio's own copy.
    pub version: Option<String>,
    /// The version tasks actually start; unknown when it cannot be established.
    #[serde(default)]
    pub installed_version: Option<String>,
    #[serde(default)]
    pub latest_version: Option<String>,
    #[serde(default)]
    pub update_supported: bool,
    #[serde(default)]
    pub managed: bool,
    /// Model identifiers explicitly announced by the checked release, not a
    /// promise that the person's account can use them.
    #[serde(default)]
    pub released_models: Vec<String>,
    pub checked_at: Option<Timestamp>,
    pub problem: Option<String>,
}

#[derive(
    Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[ts(export)]
#[serde(default)]
pub struct AgentUpdatesView {
    /// Compatibility projection: true only in Automatic mode.
    pub enabled: bool,
    #[serde(default)]
    pub mode: AgentUpdateMode,
    #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
    pub revision: u64,
    pub supported: bool,
    pub checking: bool,
    pub agents: Vec<AgentCopy>,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn policy_wire_commands_preserve_explicit_choices() {
        for (mode, wire) in [
            (AgentUpdateMode::Automatic, "automatic"),
            (AgentUpdateMode::Notify, "notify"),
            (AgentUpdateMode::Never, "never"),
        ] {
            let command = crate::Command::SetAgentUpdateMode { mode };
            let json = serde_json::to_value(&command).unwrap();
            assert_eq!(json["command"], "set_agent_update_mode");
            assert_eq!(json["mode"], wire);
            assert_eq!(
                serde_json::from_value::<crate::Command>(json).unwrap(),
                command
            );
        }
        assert_eq!(
            serde_json::to_value(crate::Command::CheckAgentUpdates {}).unwrap(),
            serde_json::json!({"command":"check_agent_updates"})
        );
        let update = crate::Command::UpdateAgent {
            harness: HarnessKind::ClaudeCode,
        };
        assert_eq!(
            serde_json::from_value::<crate::Command>(serde_json::to_value(&update).unwrap())
                .unwrap(),
            update
        );
    }
    #[test]
    fn new_status_fields_accept_legacy_documents() {
        let view: AgentUpdatesView = serde_json::from_value(serde_json::json!({"enabled":false, "revision":2, "supported":true, "checking":false, "agents":[{"harness":"claude_code","version":"2.0.0","checked_at":null,"problem":null}]})).unwrap();
        assert_eq!(view.mode, AgentUpdateMode::Notify);
        assert_eq!(view.agents[0].latest_version, None);
        assert!(view.agents[0].released_models.is_empty());
        assert_eq!(
            serde_json::from_value::<AgentUpdatesView>(serde_json::json!({})).unwrap(),
            AgentUpdatesView::default()
        );
    }
}
