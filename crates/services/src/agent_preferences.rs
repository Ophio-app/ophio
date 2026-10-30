// SPDX-License-Identifier: Apache-2.0
//! How the person likes agents to work with them, given to every run.
use crate::{CompanionHub, error};
use protocol::{
    agent_preferences::{AgentPreferences, LaunchDefault},
    *,
};

impl CompanionHub {
    pub(crate) fn agent_preferences(r: &store::Read<'_>) -> store::Result<AgentPreferences> {
        Ok(r.get("meta", "agent_preferences")?.unwrap_or_default())
    }
    pub(crate) fn agent_preferences_view(&self) -> Result<AgentPreferences, ProtocolError> {
        self.read(Self::agent_preferences)
    }
    pub(crate) fn set_agent_preferences(
        &self,
        tx: &mut store::Write<'_>,
        questions: bool,
        revision: Option<u64>,
    ) -> store::Result<()> {
        let mut preferences = Self::agent_preferences(&tx.read())?;
        domain::revision(revision, preferences.revision)?;
        preferences.questions = questions;
        preferences.revision += 1;
        tx.put("meta", "agent_preferences", &preferences)
    }
    pub(crate) fn set_launch_defaults(
        &self,
        tx: &mut store::Write<'_>,
        defaults: &[LaunchDefault],
        revision: Option<u64>,
    ) -> store::Result<()> {
        let mut preferences = Self::agent_preferences(&tx.read())?;
        domain::revision(revision, preferences.revision)?;
        for (i, d) in defaults.iter().enumerate() {
            if !d.policy.supports(d.harness) {
                return Err(error(
                    ErrorCode::Unsupported,
                    "Those launch flags aren't available for that agent",
                )
                .into());
            }
            if defaults[..i].iter().any(|e| e.harness == d.harness) {
                return Err(error(
                    ErrorCode::InvalidRequest,
                    "An agent has one default for its launch flags",
                )
                .into());
            }
        }
        preferences.launch_defaults = defaults.to_vec();
        preferences.revision += 1;
        tx.put("meta", "agent_preferences", &preferences)
    }
}
