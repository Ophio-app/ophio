// SPDX-License-Identifier: Apache-2.0
//! Read-only folder discovery for explicit project registration, not file access.
use crate::ProjectId;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct ProjectFolder {
    pub name: String,
    /// Absolute path on the computer, never on the phone.
    pub path: String,
    pub project_id: Option<ProjectId>,
    /// May still be browsed, but cannot itself be registered (home, disk, etc.).
    pub unavailable: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct ProjectFolderListing {
    /// None lists starting places. A directory response names its canonical path.
    pub current: Option<ProjectFolder>,
    pub ancestors: Vec<ProjectFolder>,
    pub folders: Vec<ProjectFolder>,
    /// Continue this listing with the same path/filter and this exact name.
    pub next_after: Option<String>,
}
