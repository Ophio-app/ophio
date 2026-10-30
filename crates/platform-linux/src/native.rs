// SPDX-License-Identifier: Apache-2.0
use async_trait::async_trait;
use ports::desktop::{CapturedFrame, PlatformError};

/// Output geometry in physical pixels, positioned in the native logical desktop.
#[derive(Debug, Clone, PartialEq)]
pub struct Output {
    pub name: String,
    pub x: f64,
    pub y: f64,
    pub width: u32,
    pub height: u32,
    pub scale: f32,
    pub rotation: u16,
}
impl Output {
    pub fn logical_width(&self) -> f64 {
        self.width as f64 / f64::from(self.scale)
    }
    pub fn logical_height(&self) -> f64 {
        self.height as f64 / f64::from(self.scale)
    }
}

/// Native events deliberately have no Debug implementation containing typed text.
pub enum Event {
    Motion { x: f64, y: f64 },
    Button { code: u32, pressed: bool },
    Key { keysym: u32, pressed: bool },
    Axis { dx: f64, dy: f64 },
}
impl std::fmt::Debug for Event {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("NativeInputEvent { .. }")
    }
}

#[async_trait]
pub trait NativeInput: Send {
    async fn event(&mut self, event: Event) -> Result<(), PlatformError>;
    /// Cancel work that has not entered the native call yet. Drivers that
    /// dispatch to blocking workers must recheck after acquiring their native
    /// lock, so a queued worker cannot inject after revocation was requested.
    /// Already-started native calls still have to settle before `barrier`.
    async fn event_cancellable(
        &mut self,
        event: Event,
        cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<(), PlatformError> {
        if cancelled.load(std::sync::atomic::Ordering::SeqCst) {
            return Ok(());
        }
        self.event(event).await
    }
    /// Synchronize the native connection after all event calls have settled.
    /// `Ok` proves that previously sent requests cannot later enter the OS.
    async fn barrier(&mut self) -> Result<(), PlatformError>;

    async fn event_fenced(&mut self, event: Event, fence: InputFence) -> Result<(), PlatformError> {
        fence.check(event.is_release())?;
        self.event_cancellable(event, fence.cancelled).await
    }
    async fn set_clipboard(
        &mut self,
        _text: &str,
        _fence: InputFence,
    ) -> Result<(), PlatformError> {
        Err(PlatformError::Unsupported(
            "Clipboard paste is not available on this route".into(),
        ))
    }
    /// Validate the complete text before the first native event. Drivers that
    /// supply their own keymap can represent every validated Unicode keysym.
    async fn validate_text(&mut self, _text: &str) -> Result<(), PlatformError> {
        Ok(())
    }
}

impl Event {
    pub(crate) fn is_release(&self) -> bool {
        matches!(
            self,
            Self::Key { pressed: false, .. } | Self::Button { pressed: false, .. }
        )
    }
}

#[async_trait]
pub trait LockObserver: Send + Sync + std::fmt::Debug + 'static {
    async fn observe(&self) -> protocol::control::DesktopLockState;
    fn current(&self) -> protocol::control::DesktopLockState;
    fn changes(&self) -> tokio::sync::watch::Receiver<protocol::control::DesktopLockState>;
}

/// Passed into native workers so a lock noticed while they were queued also
/// fences the operation after the worker acquires its native connection.
#[derive(Debug, Clone)]
pub struct InputFence {
    pub cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
    pub lock: Option<std::sync::Arc<dyn LockObserver>>,
}
impl InputFence {
    pub fn check(&self, release: bool) -> Result<(), PlatformError> {
        if release {
            return Ok(());
        }
        if let Some(lock) = &self.lock {
            match lock.current() {
                protocol::control::DesktopLockState::Unlocked => {}
                protocol::control::DesktopLockState::Locked => return Err(PlatformError::Locked),
                protocol::control::DesktopLockState::Unknown => {
                    return Err(PlatformError::TemporarilyUnavailable(
                        "Computer lock state is unknown".into(),
                    ));
                }
            }
        }
        if self.cancelled.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(PlatformError::TemporarilyUnavailable(
                "Input cancelled before native insertion".into(),
            ));
        }
        Ok(())
    }
}
#[async_trait]
pub trait NativeCapture: Send {
    async fn next_frame(&mut self) -> Result<CapturedFrame, PlatformError>;
}
#[async_trait]
pub trait Driver: Send + Sync + std::fmt::Debug + 'static {
    fn availability(&self) -> Option<protocol::capability::Availability> {
        None
    }
    fn input_availability(&self) -> Option<protocol::capability::Availability> {
        None
    }
    /// Called only for a newly opened watch, never by a capture retry loop.
    async fn prepare_watch(&self) -> Result<(), PlatformError> {
        Ok(())
    }
    async fn outputs(&self) -> Result<Vec<Output>, PlatformError>;
    async fn capture(&self, output: &Output) -> Result<CapturedFrame, PlatformError>;
    async fn input(&self) -> Result<Box<dyn NativeInput>, PlatformError>;
    /// A dedicated capture context keeps damage relative to this stream only.
    async fn stream(
        &self,
        _output: &Output,
    ) -> Result<Option<Box<dyn NativeCapture>>, PlatformError> {
        Ok(None)
    }
}

#[async_trait]
pub trait SessionLocker: Send + Sync + std::fmt::Debug + 'static {
    async fn lock(&self) -> Result<(), PlatformError>;
}
#[derive(Debug)]
pub struct LogindLocker {
    session: String,
}
impl LogindLocker {
    pub fn new(session: String) -> Result<Self, PlatformError> {
        if !valid_session(&session) {
            return Err(PlatformError::Unsupported(
                "invalid login session identifier".into(),
            ));
        }
        Ok(Self { session })
    }
}
/// Whether `session` can only be read by `loginctl` as a session identifier.
pub(crate) fn valid_session(session: &str) -> bool {
    !session.is_empty()
        && session
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-')
        && !session.starts_with('-')
}
#[async_trait]
impl SessionLocker for LogindLocker {
    async fn lock(&self) -> Result<(), PlatformError> {
        let status = tokio::process::Command::new("loginctl")
            .arg("lock-session")
            .arg(&self.session)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .status();
        let status = tokio::time::timeout(std::time::Duration::from_secs(5), status)
            .await
            .map_err(|_| {
                PlatformError::TemporarilyUnavailable("session lock timed out".into())
            })??;
        if status.success() {
            Ok(())
        } else {
            Err(PlatformError::PermissionMissing(
                "The desktop session could not be locked through logind.".into(),
            ))
        }
    }
}

/// Checks the account's password and, only when it is right and `allowed`
/// still says so once it is, releases the screen lock. Implementations keep
/// no copy of the password.
#[async_trait]
pub trait SessionUnlocker: Send + Sync + std::fmt::Debug + 'static {
    async fn unlock(
        &self,
        password: &protocol::accounts::Secret,
        allowed: &(dyn Fn() -> bool + Send + Sync),
    ) -> Result<ports::desktop::UnlockOutcome, PlatformError>;
}

pub(crate) fn failure(_: impl std::fmt::Display) -> PlatformError {
    // Native errors can contain server-supplied data; keep boundary messages constant.
    PlatformError::Native("desktop protocol operation failed".into())
}

pub(crate) fn uncertain(_: impl std::fmt::Display) -> PlatformError {
    PlatformError::OutcomeUnknown("The input helper’s confirmation was lost".into())
}
