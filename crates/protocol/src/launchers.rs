// SPDX-License-Identifier: Apache-2.0
//! Agents, skills and scratch pads a task can start from. The computer's own
//! files are the catalog: it is read when asked, never kept on the phone, and
//! a start names the entry and the digest that was shown.
use crate::capability::HarnessKind;
use crate::ids::{LibraryRootId, TaskId};
use crate::model::LaunchPolicy;
use serde::{Deserialize, Serialize};

/// Where an entry was found on the computer.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum CatalogSource {
    /// Made in Ophio, in its own agents folder.
    Ophio,
    /// The person's Claude Code folder.
    ClaudeUser,
    /// The project's `.claude` folder.
    ClaudeProject,
    /// The person's Codex or `.agents` skills.
    CodexUser,
    /// The project's `.agents` or `.codex` skills.
    CodexProject,
    /// Built into Øphio: for setting it up, and for work on GitHub.
    OphioSetup,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum CatalogKind {
    /// A personality and instructions the agent takes on.
    Agent,
    /// Instructions for one kind of job, in a `SKILL.md`.
    Skill,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct CatalogEntry {
    pub kind: CatalogKind,
    pub source: CatalogSource,
    /// An agent's file name without `.md`, or a skill's folder name. Unique
    /// for its kind and source.
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    /// SHA-256 of the file as read, in hex.
    pub digest: String,
    /// Host-native path of the agent's file or the skill's `SKILL.md`, for
    /// display.
    pub host_path: String,
    /// Where the file can be opened in Library, when it is inside a root.
    pub library: Option<CatalogFile>,
    /// Defaults the entry asks for; Review starts from them.
    pub harness: Option<HarnessKind>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub launch_policy: Option<LaunchPolicy>,
    /// For a built-in skill, the person's own copy of it, when they have
    /// made one: tasks start from that, and `library` opens it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub copy: Option<SkillCopy>,
    /// Whether it can be disabled in Ophio, and is. A disabled entry is
    /// listed, so it can be enabled again, but no task starts from it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub disable: Option<crate::disabled::Disable>,
}

/// A person's own copy of a built-in skill, kept in Ophio's skills folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct SkillCopy {
    /// Øphio has a version newer than the one this was copied from. The copy
    /// stays as it is until the person chooses.
    pub newer: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct CatalogFile {
    pub root_id: LibraryRootId,
    /// Root-relative, "/"-separated.
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct Catalog {
    /// Agents first, then skills, each by name.
    pub entries: Vec<CatalogEntry>,
    /// Host-native folder new "On its own" tasks get their folders in.
    pub one_offs: Option<String>,
    /// The Library folder new Ophio agents are made in, when it is in a
    /// writable root.
    pub agents_folder: Option<CatalogFile>,
    /// Folders or files that could not be read, said plainly.
    pub problems: Vec<String>,
    /// More entries exist than are listed.
    pub truncated: bool,
}

/// What a new task starts from. The computer reads the entry again and
/// refuses the start if its digest no longer matches what was shown.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(export)]
pub enum TaskStart {
    Agent {
        source: CatalogSource,
        id: String,
        digest: String,
    },
    Skill {
        source: CatalogSource,
        id: String,
        digest: String,
    },
    /// A bare model with no added instructions.
    Scratch,
    /// Whatever the task `task_id`, in the same project, started from, as
    /// it was read then. A source that started from nothing gives an
    /// ordinary task. Next up uses it to carry its source task's start.
    SameAs { task_id: TaskId },
}

/// Where a new task works when not in an existing project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(export)]
pub enum TaskPlace {
    /// A new folder of its own in the one-offs folder, dated and named after
    /// `name`. The command's `project_id` is new and becomes the one-off's.
    OneOff {
        name: String,
        /// The sender's local date, `yyyy-mm-dd`, for the folder's name.
        /// Omission uses the computer's date in UTC.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        date: Option<String>,
    },
}

/// What a task started from, as it is shown beside its model and flags.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(export)]
pub enum StartedFrom {
    Agent { name: String },
    Skill { name: String },
    Scratch,
}
