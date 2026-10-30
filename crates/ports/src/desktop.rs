// SPDX-License-Identifier: Apache-2.0
//! Desktop capture and input backends.
//!
//! A backend enumerates targets (outputs, windows), captures frames and
//! provides input sinks. Input sinks do not arbitrate: the services' control
//! arbiter decides who may inject and passes only validated actions.

use async_trait::async_trait;

use protocol::capability::{Availability, InputScope};
use protocol::control::{ControlTarget, InputAction, TargetGeometry};
use protocol::ids::TargetId;

#[derive(Debug, Clone, PartialEq)]
pub struct DesktopCapabilities {
    /// e.g. "hyprland", "gnome", "kde", "x11", "windows".
    pub backend: String,
    pub capture: Availability,
    pub input: Availability,
    pub input_scope: InputScope,
    pub lock_session: Availability,
    /// Whether the account's password, typed on the phone, can unlock it.
    pub unlock_session: Availability,
}

/// What became of a password given to unlock the computer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnlockOutcome {
    /// It was right, and the screen locker was told to let go.
    Released,
    /// It was right, but by then whoever gave it could no longer unlock the
    /// computer, so nothing was released.
    Withheld,
    /// It was wrong.
    Wrong,
    /// The computer would not check it now, as after too many wrong ones.
    Refused,
}

#[async_trait]
pub trait DesktopBackend: Send + Sync + std::fmt::Debug + 'static {
    async fn capabilities(&self) -> DesktopCapabilities;

    /// Current targets. Target ids are stable for the same native target for
    /// the life of the backend; geometry revisions bump on any change.
    async fn targets(&self) -> Result<Vec<ControlTarget>, PlatformError>;

    async fn capture_still(&self, target: TargetId) -> Result<CapturedFrame, PlatformError>;

    async fn open_stream(
        &self,
        target: TargetId,
        max_fps: u8,
    ) -> Result<Box<dyn FrameStream>, PlatformError>;

    async fn input_sink(&self, target: TargetId) -> Result<Box<dyn InputSink>, PlatformError>;

    async fn lock_session(&self) -> Result<(), PlatformError>;

    /// Check the account's password and, only if it is right and `allowed`
    /// still says so once it is, release the lock before returning. `allowed`
    /// is asked just before releasing, since what it allows holds only until
    /// this returns. Implementations keep no copy of the password.
    async fn unlock_session(
        &self,
        _password: &protocol::accounts::Secret,
        _allowed: &(dyn Fn() -> bool + Send + Sync),
    ) -> Result<UnlockOutcome, PlatformError> {
        Err(PlatformError::Unsupported(
            "This computer can’t be unlocked from Ophio".into(),
        ))
    }

    /// Convert an accessibility screen coordinate into this target’s input space.
    /// Coordinates use the platform accessibility API’s units (logical on Linux,
    /// physical under per-monitor DPI awareness on Windows). Unknown mappings refuse input.
    async fn accessibility_point(
        &self,
        _target: TargetId,
        _revision: u64,
        _x: f64,
        _y: f64,
    ) -> Result<protocol::control::TargetPoint, PlatformError> {
        Err(PlatformError::Unsupported(
            "Accessibility coordinates cannot be mapped to this display".into(),
        ))
    }

    /// Native observation, distinct from requesting that a session lock.
    async fn desktop_lock(&self) -> protocol::control::DesktopLockState {
        protocol::control::DesktopLockState::Unknown
    }

    /// Signals invalidate the observation immediately. Consumers also recheck
    /// at injection time; a cached unlocked value is never sufficient there.
    fn lock_changes(
        &self,
    ) -> Option<tokio::sync::watch::Receiver<protocol::control::DesktopLockState>> {
        None
    }
    /// Explicit recovery may ask the local portal for a new consented session.
    /// Normal opens and automatic reconnects must never invoke this operation.
    async fn reopen_capture(&self) -> Result<(), PlatformError> {
        Err(PlatformError::Unsupported(
            "Reopening desktop sharing is not supported by this route".into(),
        ))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelFormat {
    Bgra8,
    Bgrx8,
    Rgba8,
    Rgbx8,
}

#[derive(Clone, PartialEq)]
pub struct CapturedFrame {
    pub width: u32,
    pub height: u32,
    /// Bytes per row.
    pub stride: u32,
    pub format: PixelFormat,
    pub data: Vec<u8>,
    pub geometry: TargetGeometry,
    /// Cursor position in frame pixels, if the backend knows it.
    pub cursor: Option<(u32, u32)>,
    /// Damaged rectangles since the previous frame of this stream, when the
    /// backend reports them (`None` = unknown, treat as full damage).
    pub damage: Option<Vec<DamageRect>>,
}

impl std::fmt::Debug for CapturedFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CapturedFrame")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("format", &self.format)
            .field("geometry", &self.geometry)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DamageRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

#[async_trait]
pub trait FrameStream: Send {
    /// Next frame, waiting for new content. `Ok(None)` = stream ended.
    async fn next_frame(&mut self) -> Result<Option<CapturedFrame>, PlatformError>;
}

#[async_trait]
pub trait InputSink: Send {
    /// Inject one validated action. Coordinates in `action` are normalized to
    /// `geometry`, which the arbiter has already checked is current.
    async fn inject(
        &mut self,
        action: &InputAction,
        geometry: &TargetGeometry,
    ) -> Result<(), PlatformError>;

    /// Native input barrier: cancel every queued or in-flight action and release
    /// every held key and button. Return `Ok` only once no action accepted before
    /// this call can subsequently reach the OS through this sink. Input already
    /// inserted into the OS cannot be retracted. New explicit `inject` calls may
    /// be accepted after the barrier.
    ///
    /// This must serialize with native injection, including work that outlives
    /// a cancelled `inject` future. Cancellation of this future must retain the
    /// evidence needed to retry. If quiescence or release cannot be established,
    /// return an error, never an optimistic acknowledgement.
    async fn release_all(&mut self) -> Result<(), PlatformError>;
}

#[derive(Debug, thiserror::Error)]
pub enum PlatformError {
    #[error("unsupported: {0}")]
    Unsupported(String),
    #[error("permission missing: {0}")]
    PermissionMissing(String),
    #[error("temporarily unavailable: {0}")]
    TemporarilyUnavailable(String),
    #[error("target not found")]
    TargetNotFound,
    #[error("target geometry changed")]
    GeometryChanged,
    /// An action may have reached the OS; callers must not replay it.
    #[error("input outcome unknown: {0}")]
    OutcomeUnknown(String),
    #[error("native failure: {0}")]
    Native(String),
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Computer is locked")]
    Locked,
    #[error("Text contains characters unavailable to the active keyboard")]
    TextUnavailable {
        characters: String,
        paste_available: bool,
    },
}

impl PlatformError {
    pub fn code(&self) -> protocol::ErrorCode {
        use protocol::ErrorCode;
        match self {
            PlatformError::Unsupported(_) => ErrorCode::Unsupported,
            PlatformError::PermissionMissing(_) => ErrorCode::OsPermissionMissing,
            PlatformError::TemporarilyUnavailable(_) => ErrorCode::TemporarilyUnavailable,
            PlatformError::TargetNotFound => ErrorCode::NotFound,
            PlatformError::GeometryChanged => ErrorCode::StaleRevision,
            PlatformError::OutcomeUnknown(_) => ErrorCode::OutcomeUnknown,
            PlatformError::Native(_) | PlatformError::Io(_) => ErrorCode::NativeFailure,
            PlatformError::Locked => ErrorCode::TemporarilyUnavailable,
            PlatformError::TextUnavailable { .. } => ErrorCode::Unsupported,
        }
    }
}

/// A persistent, host-local control indicator. Implementations emit a Stop
/// request when the person activates it or its visible protection disappears.
/// Showing it never grants input; the companion owns the control lifecycle.
#[async_trait]
pub trait LocalIndicator: Send + Sync {
    async fn show(&self, holders: &str) -> Result<(), PlatformError>;
    async fn hide(&self) -> Result<(), PlatformError>;
}

pub struct IndicatorSession {
    pub indicator: std::sync::Arc<dyn LocalIndicator>,
    pub stop: tokio::sync::mpsc::Receiver<()>,
}

impl std::fmt::Debug for IndicatorSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IndicatorSession").finish_non_exhaustive()
    }
}
