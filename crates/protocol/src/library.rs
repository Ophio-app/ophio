// SPDX-License-Identifier: Apache-2.0
//! Markdown Library: authorized roots on the host, folder navigation,
//! read/edit with source fidelity, and conflict-aware saves.

use serde::{Deserialize, Serialize};

use crate::capability::HarnessKind;
use crate::ids::{LibraryRootId, ProjectId};
use crate::time::Timestamp;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct LibraryRoot {
    pub root_id: LibraryRootId,
    pub label: String,
    /// Host-native absolute path, for display. The phone never constructs
    /// host paths; it only sends root-relative paths.
    pub host_path: String,
    pub kind: RootKind,
    pub writable: bool,
    /// Project this root belongs to, if any.
    pub project_id: Option<ProjectId>,
    /// Harness whose native semantics apply (skills/agents folders).
    pub harness: Option<HarnessKind>,
    /// When set, the only paths in it the root reaches, root-relative: each
    /// a file, or a folder with everything in it. The folders on the way to
    /// one are listed, holding nothing else.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub only: Option<Vec<String>>,
}

impl LibraryRoot {
    /// Whether the root reaches the file or folder at `relative`, with
    /// everything in it.
    pub fn holds(&self, relative: &str) -> bool {
        holds(self.only.as_deref(), relative)
    }
}

/// Whether a root limited to `only`, or to nothing when it is unset, reaches
/// the file or folder at `relative` with everything in it.
pub fn holds(only: Option<&[String]>, relative: &str) -> bool {
    let path = parts(relative);
    only.is_none_or(|only| only.iter().any(|entry| path.starts_with(&parts(entry))))
}

/// Whether the folder at `relative` is one a root limited to `only` reaches,
/// or one on the way to its paths.
pub fn reaches(only: Option<&[String]>, relative: &str) -> bool {
    let path = parts(relative);
    only.is_none_or(|only| {
        only.iter().any(|entry| {
            let entry = parts(entry);
            path.starts_with(&entry) || entry.starts_with(&path)
        })
    })
}

fn parts(path: &str) -> Vec<&str> {
    path.split('/').filter(|p| !p.is_empty()).collect()
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum RootKind {
    Skills,
    Agents,
    Instructions,
    Notes,
    Project,
    /// Drafts created by "make reusable" — never an active skill folder.
    Drafts,
    /// An agent's own folder, or a project's, reaching only the files agents
    /// are given there: instructions, agents, skills, commands and memory.
    AgentFiles,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct DirListing {
    pub root_id: LibraryRootId,
    /// Root-relative directory, "/"-separated, "" for the root.
    pub dir: String,
    pub entries: Vec<DirEntry>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct DirEntry {
    pub name: String,
    /// Dotfile or native hidden attribute.
    #[serde(default)]
    pub hidden: bool,
    pub path: String,
    pub kind: EntryKind,
    #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
    pub size: u64,
    pub modified_at: Option<Timestamp>,
    /// Frontmatter `name`/`description` if a Markdown file declares them.
    pub title: Option<String>,
    pub description: Option<String>,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum EntryKind {
    Markdown,
    Directory,
    /// Other files are listed (so folders make sense) but not editable here.
    Other,
    /// A symlink/reparse point. Followed only if it stays inside the root.
    Link,
}

/// File content as the host read it. `revision` is the SHA-256 of the exact
/// bytes; saves must quote it as `base_revision`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct FileContent {
    pub root_id: LibraryRootId,
    pub path: String,
    pub text: String,
    pub revision: String,
    pub encoding: TextEncoding,
    pub newline: NewlineStyle,
    pub has_bom: bool,
    pub modified_at: Option<Timestamp>,
    #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
    pub size: u64,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum TextEncoding {
    Utf8,
    /// Not valid UTF-8; shown read-only.
    Unsupported,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum NewlineStyle {
    Lf,
    CrLf,
    Mixed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct SearchHit {
    pub root_id: LibraryRootId,
    pub path: String,
    /// The matching line, from 1. Absent when the file's name matched, with
    /// `snippet` empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub line: Option<u32>,
    pub snippet: String,
}

/// Outcome of a save attempt.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[serde(tag = "outcome", rename_all = "snake_case")]
#[ts(export)]
pub enum SaveOutcome {
    Saved {
        revision: String,
    },
    /// The file changed since `base_revision`; nothing was written. The
    /// current content is returned so the client can offer side-by-side
    /// recovery, or `None` when it is too large to send with this result and
    /// the client reads the file itself. The draft stays on the phone.
    Conflict {
        current: Option<Box<FileContent>>,
    },
    /// The file was deleted or moved since `base_revision`.
    Missing,
    /// Replacement may have occurred, but a concurrent writer or durability
    /// failure prevented confirmation. Keep the draft and reconcile. Both the
    /// proposed bytes and displaced file are retained under this recovery id.
    Uncertain {
        recovery_id: String,
    },
}

/// Known instruction sources for the instruction inspector. States are
/// honest: `unknown` is a valid answer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct InstructionSource {
    pub label: String,
    pub root_id: Option<LibraryRootId>,
    pub path: Option<String>,
    pub host_path: Option<String>,
    pub harness: HarnessKind,
    pub scope: InstructionScope,
    pub state: InstructionState,
    /// Revision (SHA-256) observed when the run started, if known.
    pub revision_at_start: Option<String>,
    pub current_revision: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
    pub size: Option<u64>,
    #[serde(default)]
    pub loaded_reported: bool,
    /// Disabled in Ophio: runs aren't given it, so its state is
    /// `NotApplied`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[ts(as = "Option<bool>", optional)]
    pub disabled: bool,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum InstructionScope {
    User,
    Project,
    Directory,
    Skill,
    Agent,
    /// A settings file committed in the project folder, which can allow tools
    /// and run commands. Its content is not read.
    Settings,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum InstructionState {
    /// An expected instruction file does not exist.
    Missing,
    /// The native harness reported loading it for this run.
    Loaded,
    /// Present where the harness discovers instructions; load not reported.
    Discovered,
    /// Changed on disk after the run started.
    Changed,
    /// Present, but runs of this project do not use it.
    NotApplied,
    Unknown,
}

/// A recoverable deletion. Private storage paths never cross the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct LibraryTrashItem {
    pub trash_id: String,
    pub root_id: LibraryRootId,
    pub path: String,
    pub revision: String,
    pub deleted_at: Timestamp,
    #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
    pub size: u64,
}

/// Retained after a save could not be confirmed. Storage paths are private.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct LibraryRecoveryItem {
    pub recovery_id: String,
    pub root_id: LibraryRootId,
    pub path: String,
    pub created_at: Timestamp,
}

/// A bounded observation, including an explicit reason when text cannot be shown.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[serde(tag = "state", rename_all = "snake_case")]
#[ts(export)]
pub enum RecoveryVersion {
    Text {
        file: FileContent,
    },
    Missing,
    Unavailable {
        #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
        size: Option<u64>,
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct LibraryRecovery {
    pub item: LibraryRecoveryItem,
    pub base: RecoveryVersion,
    pub proposed: RecoveryVersion,
    /// An external open handle can still change the displaced file.
    pub displaced: RecoveryVersion,
    pub current: RecoveryVersion,
    pub observed_at: Timestamp,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ReusableFormatKind {
    Task,
    Skill,
    Subagent,
    Prompt,
}

/// A documented format for the host's detected version, or an honest refusal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct ReusableFormat {
    pub kind: ReusableFormatKind,
    pub availability: crate::capability::Availability,
    pub suggested_name: String,
    pub text: Option<String>,
    pub instructions: String,
    pub documentation: Option<String>,
}

/// What a file agents are given is for, as Library sorts them.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
    ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum AgentFileGroup {
    /// Read at the start of every task: CLAUDE.md, AGENTS.md and the like.
    Instructions,
    /// Agents a task can hand work to, or start from.
    Agents,
    Skills,
    /// Prompts kept to be sent by name.
    Commands,
    /// Rules and output styles.
    Rules,
    /// What agents wrote down to remember.
    Memory,
}

/// A Markdown file agents are given, wherever it is kept, and where Library
/// opens it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct AgentFile {
    pub root_id: LibraryRootId,
    /// Root-relative, as Library opens it.
    pub path: String,
    /// The path as the agent finds it, within its group's folder.
    pub shown: String,
    /// Frontmatter `name`/`description`, when it declares them.
    pub title: Option<String>,
    pub description: Option<String>,
    pub group: AgentFileGroup,
    /// Where it applies: an agent, wherever it works, or one project.
    pub scope: String,
    /// Whether it applies wherever the agent works, not in one project.
    pub everywhere: bool,
    pub project_id: Option<ProjectId>,
    /// The agent that reads it, when only one does.
    pub harness: Option<HarnessKind>,
    #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
    pub size: u64,
    pub modified_at: Option<Timestamp>,
    /// Host-native path, for disabling it in Ophio.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    #[ts(as = "Option<String>", optional)]
    pub host_path: String,
    /// Whether it can be disabled in Ophio, and is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub disable: Option<crate::disabled::Disable>,
}
