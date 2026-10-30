// SPDX-License-Identifier: Apache-2.0
//! Capability model. Availability is always one of a small set of truthful
//! states; installation is never a permanent grant, and an absent backend is
//! reported as `unsupported`, not silently faked.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[serde(tag = "state", rename_all = "snake_case")]
#[ts(export)]
pub enum Availability {
    Available,
    /// This host/backend does not implement it.
    Unsupported {
        reason: String,
    },
    /// Implemented, but something the person has to set up is missing: an OS
    /// permission, an owner setting, or a local download such as a speech
    /// model. `remedy` tells the person what to do on the computer.
    PermissionMissing {
        remedy: String,
    },
    /// Implemented and permitted but not usable right now (locked session…).
    TemporarilyUnavailable {
        reason: String,
    },
    /// Previously granted and then revoked.
    Revoked,
    /// Peer versions cannot use this together.
    Incompatible {
        reason: String,
    },
    Unknown {
        reason: String,
    },
}

impl Availability {
    pub fn is_available(&self) -> bool {
        matches!(self, Availability::Available)
    }
}

/// Host-level capability summary negotiated at authentication and refreshed by
/// `capabilities_changed` events.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct HostCapabilities {
    pub os: HostOs,
    /// e.g. "hyprland", "gnome", "kde", "x11", "windows".
    pub desktop_backend: String,
    pub capture: Availability,
    pub input: Availability,
    /// Whether input can be delivered to a specific window or only to the
    /// focused/foreground target. Drives the "not isolated" disclosure.
    pub input_scope: InputScope,
    pub terminal: Availability,
    pub lock_session: Availability,
    pub speech: Availability,
    pub library: Availability,
    pub transfers: Availability,
    pub computer_use: Availability,
    pub telegram: Availability,
    pub discord: Availability,
    /// Optional minor features advertised by name for forward compatibility.
    pub features: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub window_actions: Option<crate::windows::WindowCapabilities>,
    /// Whether the computer's password, typed on the phone, can unlock it.
    /// A computer that predates unlocking leaves it out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub unlock_session: Option<Availability>,
    /// The phone apps this computer's install carries, which a device can
    /// download and update itself to. A computer that predates this, or
    /// carries none, leaves it out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub app_updates: Option<Vec<AppOffer>>,
    /// Whether the computer can hold a conversation's agent to the folders
    /// and internet the person chose. A computer that predates this leaves
    /// it out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub conversation_access: Option<Availability>,
}

/// A phone app a computer offers its devices, as its install carries it:
/// one for each key the app is signed with, since a device can update itself
/// only to an app signed as the one it runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct AppOffer {
    /// The app file's SHA-256, in hex, which also names it in a download.
    pub sha256: String,
    #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
    pub size: u64,
    /// The SHA-256 of each certificate the app is signed with, in hex.
    pub signers: Vec<String>,
    /// The version of Øphio on the computer, which the app was built with.
    pub version: String,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum HostOs {
    Linux,
    Windows,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum InputScope {
    /// Input is injected into the session's focused target; delivery to a
    /// particular window is revalidated but cannot be guaranteed.
    FocusedTarget,
    /// Input is confined to the selected output's coordinate space.
    OutputBound,
    None,
}

/// Which native harness an adapter drives.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum HarnessKind {
    ClaudeCode,
    Codex,
    Hermes,
    OpenClaw,
    Pi,
}

/// Catalog entry for one harness on this host.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct HarnessInfo {
    pub kind: HarnessKind,
    pub display_name: String,
    /// Detected native tool version, if installed.
    pub native_version: Option<String>,
    /// Integration depth actually implemented by this build.
    pub integration: IntegrationLevel,
    pub installed: Availability,
    /// Signed in with the person's own account (never inferred from a token
    /// file's presence alone when the native tool can report it).
    pub account: AccountReadiness,
    pub models: Vec<ModelOption>,
    pub efforts: Vec<String>,
    pub capabilities: HarnessCapabilities,
    /// Empty on older hosts: only Work may be assumed, if start_run is available.
    #[serde(default)]
    #[ts(as = "Option<Vec<ModeSupport>>", optional)]
    pub modes: Vec<ModeSupport>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub catalog: Option<CatalogObservation>,
    /// A run hit the account's usage limit. Starting stays allowed: the limit
    /// may lift before anyone says so. Cleared when a later run gets a reply
    /// or the reset time passes; absent from older hosts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub usage_limit: Option<UsageLimit>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct UsageLimit {
    pub reached_at: crate::time::Timestamp,
    /// When the agent tool said usage resets, if it said.
    pub resets_at: Option<crate::time::Timestamp>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct ModeSupport {
    pub mode: crate::model::TaskMode,
    pub availability: Availability,
    /// The actual native/tool restriction; unavailable modes state the missing guarantee.
    pub enforcement: String,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum IntegrationLevel {
    /// Managed runs, events, interrupts, approvals as listed in capabilities.
    Managed,
    /// Reachable only through a terminal session; no managed events.
    TerminalOnly,
    /// Known target; not integrated in this build.
    NotIntegrated,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[serde(tag = "state", rename_all = "snake_case")]
#[ts(export)]
pub enum AccountReadiness {
    SignedIn {
        account_label: Option<String>,
        plan: Option<String>,
    },
    SignedOut {
        remedy: String,
    },
    Unknown {
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct ModelOption {
    /// Identifier passed to the native tool.
    pub id: String,
    pub label: String,
    /// Suggested for GUI computer-use specialists.
    pub suggested_for_computer_use: bool,
    pub is_default: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub details: Option<ModelDetails>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct CatalogObservation {
    pub checked_at: crate::time::Timestamp,
    pub source: String,
    pub availability: Availability,
    pub default_model: Option<String>,
    pub default_effort: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct ModelDetails {
    /// Aliases are resolved by the native tool at dispatch.
    pub alias: bool,
    /// Empty means no effort choices were documented for this model.
    pub efforts: Vec<EffortOption>,
    pub default_effort: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct EffortOption {
    pub id: String,
    pub label: String,
}

/// Per-harness truthful capability rows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct HarnessCapabilities {
    pub start_run: Availability,
    pub steer_running: Availability,
    pub interrupt: Availability,
    pub native_approvals: Availability,
    pub resume_session: Availability,
    pub child_agents: Availability,
    pub computer_use_delegation: Availability,
    pub model_selection: Availability,
    pub effort_selection: Availability,
    /// Omission means withdrawal of queued messages is unavailable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub cancel_queued_message: Option<Availability>,
}
