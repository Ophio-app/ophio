// SPDX-License-Identifier: Apache-2.0
//! Window and app operations use the same fenced human lease as desktop input.
use crate::{BootEpoch, LeaseId, TargetId, capability::Availability, time::Timestamp};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct WindowCapabilities {
    pub backend: String,
    pub windows: Availability,
    pub open_apps: Availability,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct WindowInfo {
    /// Opaque, backend-owned identity. Never interpret a title as an identifier.
    pub window_id: String,
    pub title: String,
    pub app: String,
    pub workspace: Option<String>,
    pub monitor: Option<String>,
    pub focused: bool,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct InstalledApp {
    pub app_id: String,
    pub name: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct WindowCatalog {
    pub capabilities: WindowCapabilities,
    pub windows: Vec<WindowInfo>,
    pub apps: Vec<InstalledApp>,
    pub observed_at: Timestamp,
    pub truncated: bool,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct WindowControl {
    pub target_id: TargetId,
    pub lease_id: LeaseId,
    pub boot_epoch: BootEpoch,
    #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
    pub generation: u64,
    #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
    pub geometry_revision: u64,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
#[ts(export)]
pub enum WindowAction {
    Focus { window_id: String },
    Close { window_id: String },
    OpenApp { app_id: String },
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum WindowOutcome {
    Focused,
    FocusRequested,
    CloseRequested,
    AppLaunchRequested,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct WindowActionReceipt {
    pub target_id: TargetId,
    pub lease_id: LeaseId,
    pub requested_at: Timestamp,
    pub action: WindowAction,
    pub outcome: WindowOutcome,
    /// Acknowledgement of dispatch is not evidence of closed windows or finished launches.
    pub detail: String,
}
