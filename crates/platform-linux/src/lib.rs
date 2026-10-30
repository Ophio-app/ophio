// SPDX-License-Identifier: Apache-2.0
//! Linux desktop adapters. Connections always require explicit endpoints.
//! Constructing or detecting a backend never touches the current desktop.
mod detect;
mod input;
#[cfg(target_os = "linux")]
pub mod lock;
pub mod native;
#[cfg(target_os = "linux")]
mod pipewire_buffer;
#[cfg(target_os = "linux")]
pub mod portal;
#[cfg(target_os = "linux")]
pub mod protection;
#[cfg(target_os = "linux")]
pub mod servers;
mod stream;
#[cfg(target_os = "linux")]
pub mod system;
#[cfg(target_os = "linux")]
mod text;
#[cfg(target_os = "linux")]
pub mod unlock;
#[cfg(target_os = "linux")]
pub mod wayland;
#[cfg(target_os = "linux")]
pub mod x11;

use async_trait::async_trait;
pub use detect::{BackendKind, Detection, Environment};
use native::{Driver, Output, SessionLocker, SessionUnlocker};
use ports::{
    clock::{Clock, SystemClock},
    desktop::{
        CapturedFrame, DesktopBackend, DesktopCapabilities, FrameStream, InputSink, PixelFormat,
        PlatformError, UnlockOutcome,
    },
};
use protocol::{
    capability::Availability,
    control::{ControlTarget, TargetGeometry, TargetKind},
    ids::TargetId,
};
use std::{collections::BTreeMap, sync::Arc};
use tokio::sync::Mutex;

#[derive(Debug, Default)]
struct Catalog {
    outputs: Vec<Output>,
    revision: u64,
    ids: BTreeMap<String, TargetId>,
}
#[derive(Debug, Clone)]
pub struct LinuxDesktop {
    driver: Arc<dyn Driver>,
    capabilities: DesktopCapabilities,
    catalog: Arc<Mutex<Catalog>>,
    locker: Option<Arc<dyn SessionLocker>>,
    unlocker: Option<Arc<dyn SessionUnlocker>>,
    clock: Arc<dyn Clock>,
    lock: Option<Arc<dyn native::LockObserver>>,
}
impl LinuxDesktop {
    pub fn detect(env: &Environment) -> Detection {
        detect::detect(env)
    }
    /// The driver must have established its session (including portal consent) before construction.
    pub fn new(
        driver: Arc<dyn Driver>,
        mut capabilities: DesktopCapabilities,
        locker: Option<Arc<dyn SessionLocker>>,
    ) -> Self {
        capabilities.lock_session = if locker.is_some() {
            Availability::Available
        } else {
            Availability::Unsupported {
                reason: "No session locker configured.".into(),
            }
        };
        capabilities.unlock_session = Availability::Unsupported {
            reason: "No session unlocker configured.".into(),
        };
        Self {
            driver,
            capabilities,
            catalog: Arc::new(Mutex::new(Catalog::default())),
            locker,
            unlocker: None,
            clock: Arc::new(SystemClock),
            lock: None,
        }
    }
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }
    pub fn with_lock_observer(mut self, observer: Arc<dyn native::LockObserver>) -> Self {
        self.lock = Some(observer);
        self
    }
    pub fn with_unlocker(mut self, unlocker: Arc<dyn SessionUnlocker>) -> Self {
        self.capabilities.unlock_session = Availability::Available;
        self.unlocker = Some(unlocker);
        self
    }

    async fn check_lock(&self) -> Result<(), PlatformError> {
        if let Some(lock) = &self.lock {
            match lock.observe().await {
                protocol::control::DesktopLockState::Unlocked => {}
                protocol::control::DesktopLockState::Locked => return Err(PlatformError::Locked),
                protocol::control::DesktopLockState::Unknown => {
                    return Err(PlatformError::TemporarilyUnavailable(
                        "Computer lock state is unknown".into(),
                    ));
                }
            }
        }
        Ok(())
    }

    async fn refresh(&self) -> Result<(Vec<Output>, Vec<ControlTarget>), PlatformError> {
        let capabilities = self.capabilities().await;
        // Serialize native snapshots so an older concurrent reply cannot replace a newer layout.
        let mut catalog = self.catalog.lock().await;
        let mut outputs = self.driver.outputs().await?;
        outputs.sort_by(|a, b| a.name.cmp(&b.name));
        for output in &outputs {
            if output.name.is_empty()
                || output.name == COMBINED
                || output.width == 0
                || output.height == 0
                || u64::from(output.width) * u64::from(output.height) > 64 * 1024 * 1024
                || !output.x.is_finite()
                || !output.y.is_finite()
                || !output.scale.is_finite()
                || output.scale <= 0.0
            {
                return Err(PlatformError::Native("invalid output geometry".into()));
            }
        }
        if outputs.windows(2).any(|w| w[0].name == w[1].name) {
            return Err(PlatformError::Native("duplicate output identity".into()));
        }
        if catalog.outputs != outputs {
            catalog.revision = catalog
                .revision
                .checked_add(1)
                .ok_or_else(|| PlatformError::Native("geometry revision exhausted".into()))?;
            catalog.outputs = outputs.clone();
        }
        let mut targets = Vec::new();
        for output in &outputs {
            let id = *catalog.ids.entry(output.name.clone()).or_default();
            targets.push(ControlTarget {
                target_id: id,
                kind: TargetKind::Output,
                label: output.name.clone(),
                native_ref: output.name.clone(),
                geometry: geometry(output, catalog.revision),
                capture: capabilities.capture.clone(),
                input: capabilities.input.clone(),
            });
        }
        if outputs.len() > 1 {
            let output = combined(&outputs)?;
            let id = *catalog.ids.entry(COMBINED.into()).or_default();
            targets.push(ControlTarget {
                target_id: id,
                kind: TargetKind::Desktop,
                label: "Entire desktop".into(),
                native_ref: COMBINED.into(),
                geometry: geometry(&output, catalog.revision),
                capture: capabilities.capture.clone(),
                input: capabilities.input.clone(),
            });
        }
        Ok((outputs, targets))
    }

    pub(crate) async fn resolve(
        &self,
        id: TargetId,
    ) -> Result<(Output, TargetGeometry), PlatformError> {
        let (outputs, targets) = self.refresh().await?;
        let target = targets
            .into_iter()
            .find(|t| t.target_id == id)
            .ok_or(PlatformError::TargetNotFound)?;
        let output = if target.native_ref == COMBINED {
            combined(&outputs)?
        } else {
            outputs
                .into_iter()
                .find(|o| o.name == target.native_ref)
                .ok_or(PlatformError::TargetNotFound)?
        };
        Ok((output, target.geometry))
    }
}
const COMBINED: &str = "desktop:combined";
fn geometry(output: &Output, revision: u64) -> TargetGeometry {
    TargetGeometry {
        width: output.width,
        height: output.height,
        scale: output.scale,
        rotation_degrees: output.rotation,
        revision,
    }
}
fn combined(outputs: &[Output]) -> Result<Output, PlatformError> {
    let x = outputs.iter().map(|o| o.x).fold(f64::INFINITY, f64::min);
    let y = outputs.iter().map(|o| o.y).fold(f64::INFINITY, f64::min);
    let right = outputs
        .iter()
        .map(|o| o.x + o.logical_width())
        .fold(f64::NEG_INFINITY, f64::max);
    let bottom = outputs
        .iter()
        .map(|o| o.y + o.logical_height())
        .fold(f64::NEG_INFINITY, f64::max);
    let scale = outputs.iter().map(|o| o.scale).fold(1.0, f32::max);
    let width = ((right - x) * f64::from(scale)).ceil() as u32;
    let height = ((bottom - y) * f64::from(scale)).ceil() as u32;
    if width == 0 || height == 0 || u64::from(width) * u64::from(height) > 64 * 1024 * 1024 {
        return Err(PlatformError::Unsupported(
            "combined desktop exceeds capture size limit".into(),
        ));
    }
    Ok(Output {
        name: COMBINED.into(),
        x,
        y,
        width,
        height,
        scale,
        rotation: 0,
    })
}
#[async_trait]
impl DesktopBackend for LinuxDesktop {
    async fn accessibility_point(
        &self,
        target: TargetId,
        revision: u64,
        x: f64,
        y: f64,
    ) -> Result<protocol::control::TargetPoint, PlatformError> {
        let (output, current) = self.resolve(target).await?;
        crate::a11y::map_point(
            x,
            y,
            output.x,
            output.y,
            f64::from(output.width.saturating_sub(1)) / f64::from(output.scale),
            f64::from(output.height.saturating_sub(1)) / f64::from(output.scale),
            revision == current.revision,
        )
    }
    async fn capabilities(&self) -> DesktopCapabilities {
        let mut capabilities = self.capabilities.clone();
        if let Some(availability) = self.driver.availability() {
            capabilities.capture = availability.clone();
            capabilities.input = availability;
        }
        if let Some(input) = self.driver.input_availability() {
            capabilities.input = input;
        }
        if let Some(lock) = &self.lock {
            match lock.current() {
                protocol::control::DesktopLockState::Locked => {
                    capabilities.input = Availability::TemporarilyUnavailable {
                        reason: "Computer is locked".into(),
                    };
                }
                protocol::control::DesktopLockState::Unknown => {
                    capabilities.input = Availability::Unknown { reason: "Computer lock state is unknown; control is unavailable until lock observation recovers".into() };
                }
                protocol::control::DesktopLockState::Unlocked => {}
            }
        }
        capabilities
    }
    async fn targets(&self) -> Result<Vec<ControlTarget>, PlatformError> {
        self.refresh().await.map(|(_, targets)| targets)
    }
    async fn capture_still(&self, target: TargetId) -> Result<CapturedFrame, PlatformError> {
        self.check_lock().await?;
        let (output, current) = self.resolve(target).await?;
        let mut frame = if output.name == COMBINED {
            let (outputs, _) = self.refresh().await?;
            let mut data = vec![0; (output.width * output.height * 4) as usize];
            for item in outputs {
                let f = self.driver.capture(&item).await?;
                validate_frame(&f)?;
                let x = ((item.x - output.x) * f64::from(output.scale)).round() as u32;
                let y = ((item.y - output.y) * f64::from(output.scale)).round() as u32;
                let w = (item.logical_width() * f64::from(output.scale)).round() as u32;
                let h = (item.logical_height() * f64::from(output.scale)).round() as u32;
                for dy in 0..h.min(output.height.saturating_sub(y)) {
                    for dx in 0..w.min(output.width.saturating_sub(x)) {
                        let sx = u64::from(dx) * u64::from(f.width) / u64::from(w);
                        let sy = u64::from(dy) * u64::from(f.height) / u64::from(h);
                        let source = (sy * u64::from(f.stride) + sx * 4) as usize;
                        let dest = (((y + dy) * output.width + x + dx) * 4) as usize;
                        let p = &f.data[source..source + 4];
                        if matches!(f.format, PixelFormat::Bgra8 | PixelFormat::Bgrx8) {
                            data[dest..dest + 4].copy_from_slice(&[p[2], p[1], p[0], 255]);
                        } else {
                            data[dest..dest + 4].copy_from_slice(&[p[0], p[1], p[2], 255]);
                        }
                    }
                }
            }
            CapturedFrame {
                width: output.width,
                height: output.height,
                stride: output.width * 4,
                format: PixelFormat::Rgba8,
                data,
                geometry: current,
                cursor: None,
                damage: None,
            }
        } else {
            self.driver.capture(&output).await?
        };
        validate_frame(&frame)?;
        let (_, after) = self.resolve(target).await?;
        if current != after || frame.width != current.width || frame.height != current.height {
            return Err(PlatformError::GeometryChanged);
        }
        frame.geometry = current;
        self.check_lock().await?;
        Ok(frame)
    }
    async fn open_stream(
        &self,
        target: TargetId,
        max_fps: u8,
    ) -> Result<Box<dyn FrameStream>, PlatformError> {
        self.resolve(target).await?;
        if !(1..=120).contains(&max_fps) {
            return Err(PlatformError::Unsupported(
                "frame rate must be 1..120".into(),
            ));
        }
        Ok(Box::new(stream::CaptureStream::start(
            self.clone(),
            target,
            max_fps,
        )))
    }
    async fn input_sink(&self, target: TargetId) -> Result<Box<dyn InputSink>, PlatformError> {
        self.resolve(target).await?;
        Ok(Box::new(input::ManagedInput::new(
            self.clone(),
            target,
            self.driver.input().await?,
        )))
    }
    async fn reopen_capture(&self) -> Result<(), PlatformError> {
        self.driver.prepare_watch().await
    }
    async fn lock_session(&self) -> Result<(), PlatformError> {
        self.locker
            .as_ref()
            .ok_or_else(|| PlatformError::Unsupported("session locking is not configured".into()))?
            .lock()
            .await
    }
    async fn unlock_session(
        &self,
        password: &protocol::accounts::Secret,
        allowed: &(dyn Fn() -> bool + Send + Sync),
    ) -> Result<UnlockOutcome, PlatformError> {
        self.unlocker
            .as_ref()
            .ok_or_else(|| {
                PlatformError::Unsupported("session unlocking is not configured".into())
            })?
            .unlock(password, allowed)
            .await
    }
    async fn desktop_lock(&self) -> protocol::control::DesktopLockState {
        if let Some(lock) = &self.lock {
            lock.observe().await
        } else {
            protocol::control::DesktopLockState::Unknown
        }
    }
    fn lock_changes(
        &self,
    ) -> Option<tokio::sync::watch::Receiver<protocol::control::DesktopLockState>> {
        self.lock.as_ref().map(|lock| lock.changes())
    }
}
pub(crate) fn validate_frame(frame: &CapturedFrame) -> Result<(), PlatformError> {
    let row = u64::from(frame.width) * 4;
    let bytes = u64::from(frame.stride) * u64::from(frame.height);
    if row == 0
        || frame.height == 0
        || u64::from(frame.stride) < row
        || bytes > 256 * 1024 * 1024
        || (frame.data.len() as u64) < bytes
    {
        Err(PlatformError::Native("invalid capture buffer".into()))
    } else {
        Ok(())
    }
}

#[cfg(target_os = "linux")]
pub mod a11y;
mod clipboard;
#[cfg(test)]
mod fake_portal;
pub mod windows;
