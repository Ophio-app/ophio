// SPDX-License-Identifier: Apache-2.0
//! A chat's suggested work, started as an ordinary task by the person.

use serde::{Deserialize, Serialize};

use crate::{RunId, TaskId};

pub const FEATURE: &str = "chat_work_cards_v1";
pub const HEARS_BACK: &str = "chat_hears_back_v1";
/// Ordered sets of suggestions, a card for Run all, `ophio_propose`, and a
/// project's go-ahead for what its chats' agents propose ([`AutoStart`]).
pub const SETS: &str = "chat_work_sets_v1";
/// A card opened in Review and started from there stays that card's task
/// ([`TaskOrigin::reviewed`]).
pub const REVIEWED: &str = "chat_work_reviewed_v1";

/// The minimum access suggested work needs, not permission to start it.
#[derive(
    Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[ts(export)]
pub struct WorkNeeds {
    /// Names of other projects to read.
    #[serde(default)]
    pub read: Vec<String>,
    /// Name of another project to change; none means this chat's project.
    pub changes: Option<String>,
    #[serde(default)]
    pub internet: bool,
    #[serde(default)]
    pub local_network: bool,
    #[serde(default)]
    pub whole_computer: bool,
    #[serde(default)]
    pub desktop: bool,
}

/// The chat, its turn (run), and the position in that turn's suggested work
/// (`RunReport.new_work`) from which this task was started.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct TaskOrigin {
    pub chat: TaskId,
    pub run: RunId,
    pub index: u32,
    /// Started by the computer on the project's [`AutoStart`], not by a
    /// tap. Only the computer sets it; a phone's start saying so is refused.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[ts(as = "Option<bool>", optional)]
    pub auto: bool,
    /// Started from Review, where the person saw and could change all of
    /// it, rather than from the card's own Start: not compared with the
    /// suggestion, and it may go to another project.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[ts(as = "Option<bool>", optional)]
    pub reviewed: bool,
}

/// A project's standing go-ahead, given by the person and only by them, to
/// start what its chats' agents propose without asking: work in that project
/// alone, within these limits, each start still shown as a card.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[ts(export)]
pub struct AutoStart {
    /// The most tasks one reply of a chat's agent may start, across
    /// everything it proposed; what doesn't fit waits for the person.
    pub per_reply: u32,
    /// The most tasks started this way that may be going at once, running
    /// or waiting in Next up; past it, a proposal waits for the person.
    pub at_once: u32,
}

impl AutoStart {
    /// The highest either limit may be set to.
    pub const MAX: u32 = 8;
    pub const DEFAULT: AutoStart = AutoStart {
        per_reply: 4,
        at_once: 4,
    };
    /// Whether both limits are within 1 and [`Self::MAX`].
    pub fn valid(&self) -> bool {
        (1..=Self::MAX).contains(&self.per_reply) && (1..=Self::MAX).contains(&self.at_once)
    }
}
