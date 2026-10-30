// SPDX-License-Identifier: Apache-2.0
//! Skills, agents and agent files the person has disabled in Ophio. No agent
//! Ophio runs is given them: they aren't listed to it, can't be called or
//! started from, and an instruction file isn't loaded. The block is kept in
//! Ophio's own state. The files, and the agents' own settings, stay exactly
//! as they are, so the agents run anywhere else still find them.
//! Advertised as [`FEATURE`].
use crate::capability::HarnessKind;
use crate::ids::ProjectId;
use crate::launchers::CatalogFile;
use crate::library::AgentFileGroup;
use crate::time::Timestamp;
use serde::{Deserialize, Serialize};

/// The feature a computer advertises once it carries this.
pub const FEATURE: &str = "disable_in_ophio_v1";

/// What is disabled: a skill, an agent (a subagent definition, or one of
/// Ophio's own agents) or a file agents are given.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum DisabledKind {
    Skill,
    Agent,
    File,
}

/// Whether Ophio can keep one entry or file from its agents, and whether it
/// does. Absent from a computer without [`FEATURE`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[serde(tag = "state", rename_all = "snake_case")]
#[ts(export)]
pub enum Disable {
    /// It can be disabled, as `kind`; `disabled` says whether it is.
    /// `key` is the path the computer keeps it by, where it leads when it
    /// was listed by a link: a [`DisabledItem`]'s `host_path`.
    Can {
        kind: DisabledKind,
        disabled: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        key: Option<String>,
    },
    /// It can't be, said plainly: Ophio would have to change the file or
    /// the agent's own settings to hide it, or no agent Ophio runs reads it.
    Cannot { reason: String },
}

/// One skill, agent or file disabled in Ophio.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct DisabledItem {
    pub kind: DisabledKind,
    /// Host-native path of the skill's `SKILL.md`, the agent's file or the
    /// file, where it leads: the key it is kept by.
    pub host_path: String,
    pub name: String,
    pub description: Option<String>,
    /// Where it applies, as Library lists it: "Claude Code", a project's
    /// name, a plugin.
    pub scope: String,
    /// The agent that reads it, when only one does.
    pub harness: Option<HarnessKind>,
    pub project_id: Option<ProjectId>,
    /// What kind of agent file it is, when it was listed among them: a
    /// Claude Code command is a skill to Claude Code but listed, and shown,
    /// as a command.
    pub group: Option<AgentFileGroup>,
    /// Where Library opens it now, when a root reaches it.
    pub library: Option<CatalogFile>,
    pub disabled_at: Timestamp,
    /// Still where it was. One no longer there stays disabled, so it is
    /// kept from agents again if it comes back.
    pub found: bool,
}
