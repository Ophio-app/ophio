// SPDX-License-Identifier: Apache-2.0
//! Desktop control: targets, observation and input leases.
//!
//! Default manual Desktop is read-only watching. A human input lease is an
//! explicit request bound to one target. An agent lease belongs to one
//! computer-use run. Exactly one managed input owner exists per target; local
//! physical input is outside this arbitration and always remains possible.

use serde::{Deserialize, Serialize};

use crate::capability::Availability;
use crate::ids::{BootEpoch, DeviceId, LeaseId, ObservationId, RunId, TargetId};
use crate::time::Timestamp;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct ControlTarget {
    pub target_id: TargetId,
    pub kind: TargetKind,
    /// Human label, e.g. "DP-1 (2560×1440)" or a window title. Display only.
    pub label: String,
    /// Backend-native identity (output name, window address). Opaque.
    pub native_ref: String,
    pub geometry: TargetGeometry,
    pub capture: Availability,
    pub input: Availability,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum TargetKind {
    /// All outputs in one desktop coordinate space.
    Desktop,
    Output,
    Window,
    /// A tab of Ophio's agent browser.
    Browser,
}

/// Geometry of a target in its own pixel space at a specific revision. Any
/// resize/rotation/scale change bumps `revision`; coordinates tied to an older
/// revision are rejected rather than reinterpreted.
#[derive(
    Debug, Clone, Copy, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[ts(export)]
pub struct TargetGeometry {
    pub width: u32,
    pub height: u32,
    pub scale: f32,
    pub rotation_degrees: u16,
    #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
    pub revision: u64,
}

/// Normalized point in a target at a geometry revision (0.0..=1.0 each axis).
#[derive(
    Debug, Clone, Copy, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[ts(export)]
pub struct TargetPoint {
    pub x: f64,
    pub y: f64,
}

/// Managed input state of a target, as shown in the control strip.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum InputState {
    Unavailable,
    Ready,
    Human,
    Agent,
    Revoking,
    Paused,
    Unknown,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(export)]
pub enum LeaseOwner {
    Human { device_id: DeviceId },
    Agent { run_id: RunId },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct LeaseView {
    pub lease_id: LeaseId,
    pub target_id: TargetId,
    pub owner: LeaseOwner,
    /// Monotonic per target across the worker's boot epoch. Late actions with
    /// an older generation are rejected.
    #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
    pub generation: u64,
    pub boot_epoch: BootEpoch,
    pub state: LeaseState,
    pub granted_at: Timestamp,
    /// Configured renewal window, for display only. The worker enforces the
    /// real deadline on its own monotonic clock.
    pub renewal_window_ms: u32,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum LeaseState {
    Active,
    /// Revocation requested; not yet acknowledged by the input worker.
    Revoking,
    /// Worker acknowledged: new actions rejected, uninjected actions dropped,
    /// held keys/buttons released.
    Revoked,
    /// Deadline passed without renewal; worker closed input on its own clock.
    Expired,
    /// Worker state cannot be established (unreachable after boot change…).
    Unknown,
}

/// Model observation of a target is a separate lifecycle from input and from
/// the phone's own watching.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ObservationState {
    Allowed,
    WithdrawalPending,
    Withdrawn,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct ModelObservationView {
    pub observation_id: ObservationId,
    pub run_id: RunId,
    pub target_id: TargetId,
    pub state: ObservationState,
    /// Count of observations already delivered to the model; these cannot be
    /// recalled and are disclosed in private-handoff coverage.
    pub delivered_count: u32,
}

/// Per-target control projection used by the control strip.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct TargetControlView {
    pub target_id: TargetId,
    pub input_state: InputState,
    pub lease: Option<LeaseView>,
    pub model_observation: Option<ModelObservationView>,
    /// Private human step coverage, when one is active.
    pub private_step: Option<PrivateStepView>,
    /// Omitted by older companions; never infer unlocked from omission.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub desktop_lock: Option<DesktopLockState>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct PrivateStepView {
    pub requested_at: Timestamp,
    pub input_stopped: CoverageFact,
    pub managed_screenshots_stopped: CoverageFact,
    /// Other capture routes that may still observe (e.g. a harness with its
    /// own screenshot tool). If non-empty, privacy is not claimed.
    pub uncovered_routes: Vec<String>,
    /// Prior observations already delivered (cannot be retracted).
    pub prior_observations_delivered: u32,
    pub ready: bool,
}

/// One enforcement fact with its evidence state.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum CoverageFact {
    Requested,
    Acknowledged,
    NotApplicable,
    Failed,
    Unknown,
}

/// Human input actions sent over the control connection while a human lease
/// is held. Coordinates are normalized to the lease's target geometry
/// revision; a stale revision is rejected.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[serde(tag = "action", rename_all = "snake_case")]
#[ts(export)]
pub enum InputAction {
    PointerMove {
        to: TargetPoint,
    },
    PointerButton {
        button: PointerButton,
        pressed: bool,
        at: TargetPoint,
    },
    Click {
        button: PointerButton,
        at: TargetPoint,
        count: u8,
    },
    Scroll {
        at: TargetPoint,
        dx: f64,
        dy: f64,
    },
    /// A key chord such as ["ctrl","shift","t"]. Names follow the W3C
    /// `KeyboardEvent.key` vocabulary, lowercased for modifiers.
    KeyChord {
        keys: Vec<String>,
    },
    KeyPress {
        key: String,
        pressed: bool,
    },
    /// Literal text (IME-composed). Delivered as text, not as key codes when
    /// the backend supports it.
    Text {
        text: String,
    },
    /// Release every key/button the managed path currently holds.
    ReleaseAll,
    /// Explicitly replace the remote clipboard and send the paste chord. Never
    /// substituted for Text. Offered only when this session supports clipboard.
    Paste {
        text: String,
    },
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum PointerButton {
    Left,
    Right,
    Middle,
}

/// One human input action bound to a lease generation and geometry revision.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct InputEnvelope {
    pub lease_id: LeaseId,
    #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
    pub generation: u64,
    #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
    pub geometry_revision: u64,
    /// Per-lease monotonic sequence; the worker drops out-of-order actions.
    #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
    pub seq: u64,
    pub action: InputAction,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct InputAck {
    pub lease_id: LeaseId,
    #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
    pub seq: u64,
    pub outcome: InputOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub refusal: Option<InputRefusal>,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum InputOutcome {
    /// Handed to the OS input path.
    Injected,
    /// Lease not active / generation or boot epoch stale.
    StaleLease,
    /// Geometry changed since the phone computed the coordinates.
    StaleGeometry,
    /// Out-of-order or duplicate sequence.
    OutOfOrder,
    /// The action may or may not have reached the OS; the phone must not assume
    /// either outcome or automatically resend it.
    Unknown,
    /// The OS path definitely refused the action before any effect.
    Failed,
}

/// Current native session observation, independent of lease/barrier state.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum DesktopLockState {
    Unlocked,
    Locked,
    Unknown,
}

/// A definite refusal before any part of the action was injected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(export)]
pub enum InputRefusal {
    Locked,
    TextUnavailable {
        /// Distinct unsupported characters; returned only to the sending device.
        characters: String,
        paste_available: bool,
    },
}
