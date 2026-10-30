// SPDX-License-Identifier: Apache-2.0
//! What a conversation's agent may read, write and reach, enforced by the
//! computer rather than asked of the agent. Advertised as
//! `conversation_access_v1`, with [`crate::capability::HostCapabilities::
//! conversation_access`] saying whether this computer can enforce it.
use crate::ids::ProjectId;
use crate::time::Timestamp;
use serde::{Deserialize, Serialize};

/// The folders and network a conversation's agent is let reach. A project
/// in `write` may also be read, whether or not `read` lists it, and the
/// conversation's own project may always be read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct ConversationAccess {
    /// Projects the agent may read.
    #[serde(default)]
    pub read: Vec<ProjectId>,
    /// Projects the agent may change, and run commands in.
    #[serde(default)]
    pub write: Vec<ProjectId>,
    /// Everything on the computer may be read.
    #[serde(default)]
    pub whole_read: bool,
    /// Everything on the computer may be changed.
    #[serde(default)]
    pub whole_write: bool,
    /// The agent may reach the internet. Off, it reaches only its own
    /// provider, which it needs to answer at all.
    #[serde(default = "on")]
    pub internet: bool,
    /// With the internet, the person's own networks too: addresses on their
    /// home or office network and their tailnet, which the internet alone
    /// never reaches. Never this computer's own services on its loopback.
    #[serde(default)]
    pub local_network: bool,
}

fn on() -> bool {
    true
}

/// What a new conversation starts with: its own project, read-only, and
/// the internet.
impl Default for ConversationAccess {
    fn default() -> Self {
        Self {
            read: Vec::new(),
            write: Vec::new(),
            whole_read: false,
            whole_write: false,
            internet: true,
            local_network: false,
        }
    }
}

impl ConversationAccess {
    /// Whether the agent may change anything at all.
    pub fn writes(&self) -> bool {
        self.whole_write || !self.write.is_empty()
    }

    /// Nothing is held back: the whole computer, read and write, and the
    /// internet. Such a conversation runs as a task does, unsandboxed.
    pub fn unrestricted(&self) -> bool {
        self.whole_read && self.whole_write && self.internet
    }

    /// The same access with `write` included in `read`, each list sorted and
    /// without repeats, and the whole computer's write implying its read.
    pub fn normalized(mut self) -> Self {
        self.read.extend(self.write.iter().copied());
        self.read.sort();
        self.read.dedup();
        self.write.sort();
        self.write.dedup();
        self.whole_read |= self.whole_write;
        self.local_network &= self.internet;
        self
    }
}

/// What the agent of a conversation last tried and was kept from, so the
/// person can let it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct AccessBlocked {
    /// The folder or file, on the computer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub path: Option<String>,
    /// The internet host, when it was the internet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub host: Option<String>,
    /// The host is on the person's own network, such as their tailnet,
    /// which the internet alone doesn't reach.
    #[serde(default)]
    pub local: bool,
    /// Whether it tried to change the path rather than read it.
    #[serde(default)]
    pub write: bool,
    /// The project the path is in, which **Allow** grants. None when it is
    /// in no project, so only the whole computer would let it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub project_id: Option<ProjectId>,
    pub at: Timestamp,
}
