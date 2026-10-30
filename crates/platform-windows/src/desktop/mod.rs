// SPDX-License-Identifier: Apache-2.0
//! Monitor and whole-desktop capture with geometry-fenced, focused-session input.
//! Construct only after desktop sharing has been explicitly enabled by the owner.
#[cfg(windows)]
#[allow(unsafe_code)]
mod capture;
pub mod cursor;
pub mod geometry;
pub mod input;
#[cfg(windows)]
#[allow(unsafe_code)]
mod native;
use async_trait::async_trait;
use geometry::{Layout, Monitor};
use input::{Injector, ManagedInput};
use ports::{Clock, SystemClock, desktop::*};
use protocol::capability::{Availability, InputScope};
use protocol::{
    control::{ControlTarget, DesktopLockState, InputAction, TargetGeometry, TargetKind},
    ids::TargetId,
};
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::{Mutex, RwLock, RwLockReadGuard};
pub(crate) const COMPOSITE: &str = "desktop:combined";

/// One independent capture timeline. Damage must refer to the previous frame
/// returned by this session, never another viewer's timeline.
#[async_trait]
pub trait Capture: Send {
    async fn frame(&mut self, monitor: &Monitor) -> Result<CapturedFrame, PlatformError>;
    /// Finish native work before acknowledging desktop shutdown.
    async fn close(&mut self) -> Result<(), PlatformError> {
        Ok(())
    }
}
/// Platform seam for deterministic tests and explicit native composition.
pub trait Driver: Send + Sync + std::fmt::Debug + 'static {
    fn monitors(&self) -> Result<Vec<Monitor>, PlatformError>;
    fn capabilities(&self) -> DesktopCapabilities;
    fn capture(&self) -> Result<Box<dyn Capture>, PlatformError>;
    fn injector(&self) -> Result<Box<dyn Injector>, PlatformError>;
    fn lock(&self) -> Result<(), PlatformError>;
    /// Windows' own report of whether the signed-in session is locked.
    fn lock_state(&self) -> DesktopLockState {
        DesktopLockState::Unknown
    }
}
#[derive(Debug, Default)]
struct Catalog {
    layout: Option<Layout>,
    revision: u64,
    ids: BTreeMap<String, TargetId>,
}
type CaptureResource = Mutex<Option<Box<dyn Capture>>>;
type InputResource = Mutex<Option<ManagedInput>>;

#[derive(Default)]
struct Resources {
    captures: Vec<Weak<CaptureResource>>,
    inputs: Vec<Weak<InputResource>>,
}
#[derive(Default)]
struct Lifecycle {
    closed: AtomicBool,
    operations: RwLock<()>,
    resources: Mutex<Resources>,
}
impl std::fmt::Debug for Lifecycle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Lifecycle")
            .field("closed", &self.closed.load(Ordering::Acquire))
            .finish_non_exhaustive()
    }
}
fn session_closed() -> PlatformError {
    PlatformError::TemporarilyUnavailable("Desktop sharing has ended".into())
}
#[derive(Debug, Clone)]
pub struct WindowsDesktop {
    driver: Arc<dyn Driver>,
    catalog: Arc<Mutex<Catalog>>,
    clock: Arc<dyn Clock>,
    lifecycle: Arc<Lifecycle>,
}
impl WindowsDesktop {
    #[cfg(windows)]
    pub fn new() -> Result<Self, PlatformError> {
        Ok(Self::with_driver(Arc::new(native::NativeDriver::new()?)))
    }
    pub fn with_driver(driver: Arc<dyn Driver>) -> Self {
        Self {
            driver,
            catalog: Arc::new(Mutex::new(Catalog::default())),
            clock: Arc::new(SystemClock),
            lifecycle: Arc::new(Lifecycle::default()),
        }
    }
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }
    async fn enter(&self) -> Result<RwLockReadGuard<'_, ()>, PlatformError> {
        let guard = self.lifecycle.operations.read().await;
        if self.lifecycle.closed.load(Ordering::Acquire) {
            return Err(session_closed());
        }
        Ok(guard)
    }
    /// Permanently fence all clones, close capture workers and release held input.
    /// The Hub must revoke leases first. Failed releases remain retryable; a
    /// subsequent close retries them and no new input can be admitted meanwhile.
    pub async fn close(&self) -> Result<(), PlatformError> {
        self.lifecycle.closed.store(true, Ordering::Release);
        let _operations = self.lifecycle.operations.write().await;
        let mut resources = self.lifecycle.resources.lock().await;
        let mut failure = None;
        for input in resources.inputs.iter().filter_map(Weak::upgrade) {
            let mut input = input.lock().await;
            if let Some(active) = input.as_mut() {
                match active.release_all() {
                    Ok(()) => {
                        input.take();
                    }
                    Err(error) => failure = Some(error),
                }
            }
        }
        for capture in resources.captures.iter().filter_map(Weak::upgrade) {
            let mut capture = capture.lock().await;
            if let Some(active) = capture.as_mut() {
                match active.close().await {
                    Ok(()) => {
                        capture.take();
                    }
                    Err(error) => failure = Some(error),
                }
            }
        }
        resources.inputs.retain(|r| r.strong_count() > 0);
        resources.captures.retain(|r| r.strong_count() > 0);
        *self.catalog.lock().await = Catalog::default();
        failure.map_or(Ok(()), Err)
    }
    async fn refresh(&self) -> Result<(Layout, Vec<ControlTarget>), PlatformError> {
        let mut catalog = self.catalog.lock().await;
        let layout = Layout::new(self.driver.monitors()?)?;
        if catalog.layout.as_ref() != Some(&layout) {
            catalog.revision = catalog
                .revision
                .checked_add(1)
                .ok_or_else(geometry::invalid)?;
            catalog.layout = Some(layout.clone());
        }
        let cap = self.driver.capabilities();
        let revision = catalog.revision;
        let mut outputs = layout.monitors.clone();
        if outputs.len() > 1 {
            outputs.push(layout.desktop.clone());
        }
        let targets = outputs
            .iter()
            .map(|m| ControlTarget {
                target_id: *catalog.ids.entry(m.name.clone()).or_default(),
                kind: if m.name == COMPOSITE {
                    TargetKind::Desktop
                } else {
                    TargetKind::Output
                },
                label: if m.name == COMPOSITE {
                    "Entire desktop".into()
                } else {
                    m.name.clone()
                },
                native_ref: m.name.clone(),
                geometry: m.geometry(revision),
                capture: cap.capture.clone(),
                input: cap.input.clone(),
            })
            .collect();
        Ok((layout, targets))
    }
    async fn resolve(
        &self,
        id: TargetId,
    ) -> Result<(Layout, Monitor, TargetGeometry), PlatformError> {
        let (layout, targets) = self.refresh().await?;
        let target = targets
            .iter()
            .find(|t| t.target_id == id)
            .ok_or(PlatformError::TargetNotFound)?;
        let monitor = if target.native_ref == COMPOSITE {
            layout.desktop.clone()
        } else {
            layout
                .monitors
                .iter()
                .find(|m| m.name == target.native_ref)
                .cloned()
                .ok_or(PlatformError::TargetNotFound)?
        };
        Ok((layout, monitor, target.geometry))
    }
    async fn frame(
        &self,
        id: TargetId,
        capture: &mut dyn Capture,
    ) -> Result<CapturedFrame, PlatformError> {
        let (layout, monitor, geometry) = self.resolve(id).await?;
        let mut frame = if monitor.name == COMPOSITE {
            let mut frames = Vec::new();
            for monitor in &layout.monitors {
                frames.push((monitor.clone(), capture.frame(monitor).await?));
            }
            geometry::composite(&layout, &frames, geometry)?
        } else {
            capture.frame(&monitor).await?
        };
        geometry::validate_frame(&frame)?;
        let (_, _, after) = self.resolve(id).await?;
        geometry::check_geometry(&geometry, &after)?;
        if frame.width != geometry.width || frame.height != geometry.height {
            return Err(PlatformError::GeometryChanged);
        }
        frame.geometry = geometry;
        Ok(frame)
    }
}
#[async_trait]
impl DesktopBackend for WindowsDesktop {
    async fn accessibility_point(
        &self,
        target: TargetId,
        revision: u64,
        x: f64,
        y: f64,
    ) -> Result<protocol::control::TargetPoint, PlatformError> {
        let _operation = self.enter().await?;
        let (_, monitor, current) = self.resolve(target).await?;
        crate::a11y::map_point(
            x,
            y,
            monitor.x,
            monitor.y,
            monitor.width,
            monitor.height,
            revision == current.revision,
        )
    }
    async fn capabilities(&self) -> DesktopCapabilities {
        if self.lifecycle.closed.load(Ordering::Acquire) {
            return capabilities(Availability::TemporarilyUnavailable {
                reason: "Desktop sharing has ended".into(),
            });
        }
        self.driver.capabilities()
    }
    async fn targets(&self) -> Result<Vec<ControlTarget>, PlatformError> {
        let _operation = self.enter().await?;
        self.refresh().await.map(|(_, targets)| targets)
    }
    async fn capture_still(&self, target: TargetId) -> Result<CapturedFrame, PlatformError> {
        let _operation = self.enter().await?;
        let mut capture = self.driver.capture()?;
        let result = self.frame(target, &mut *capture).await;
        capture.close().await?;
        result
    }
    async fn open_stream(
        &self,
        target: TargetId,
        max_fps: u8,
    ) -> Result<Box<dyn FrameStream>, PlatformError> {
        let _operation = self.enter().await?;
        if !(1..=120).contains(&max_fps) {
            return Err(PlatformError::Unsupported(
                "frame rate must be 1..120".into(),
            ));
        }
        self.resolve(target).await?;
        let capture = Arc::new(Mutex::new(Some(self.driver.capture()?)));
        let mut resources = self.lifecycle.resources.lock().await;
        resources.captures.retain(|r| r.strong_count() > 0);
        resources.captures.push(Arc::downgrade(&capture));
        Ok(Box::new(Stream {
            backend: self.clone(),
            target,
            capture,
            period: Duration::from_secs_f64(1.0 / f64::from(max_fps)),
            next: self.clock.monotonic(),
            last_revision: None,
        }))
    }
    async fn input_sink(&self, target: TargetId) -> Result<Box<dyn InputSink>, PlatformError> {
        let _operation = self.enter().await?;
        self.resolve(target).await?;
        let input = Arc::new(Mutex::new(Some(
            ManagedInput::new(self.driver.injector()?).with_clock(self.clock.clone()),
        )));
        let mut resources = self.lifecycle.resources.lock().await;
        resources.inputs.retain(|r| r.strong_count() > 0);
        resources.inputs.push(Arc::downgrade(&input));
        Ok(Box::new(Sink {
            backend: self.clone(),
            target,
            input,
        }))
    }
    async fn lock_session(&self) -> Result<(), PlatformError> {
        let _operation = self.enter().await?;
        self.driver.lock()
    }
    async fn desktop_lock(&self) -> DesktopLockState {
        if self.lifecycle.closed.load(Ordering::Acquire) {
            return DesktopLockState::Unknown;
        }
        self.driver.lock_state()
    }
}
struct Stream {
    backend: WindowsDesktop,
    target: TargetId,
    capture: Arc<CaptureResource>,
    period: Duration,
    next: Instant,
    last_revision: Option<u64>,
}
#[async_trait]
impl FrameStream for Stream {
    async fn next_frame(&mut self) -> Result<Option<CapturedFrame>, PlatformError> {
        // Taking the baseline before any await also invalidates it if the
        // caller cancels this future after native acquisition but before delivery.
        let previous_revision = self.last_revision.take();
        loop {
            if self.backend.lifecycle.closed.load(Ordering::Acquire) {
                return Ok(None);
            }
            let remaining = self
                .next
                .saturating_duration_since(self.backend.clock.monotonic());
            if remaining.is_zero() {
                break;
            }
            tokio::time::sleep(remaining.min(Duration::from_millis(25))).await;
        }
        let _operation = self.backend.enter().await?;
        let mut capture = self.capture.lock().await;
        let capture = capture.as_mut().ok_or_else(session_closed)?;
        let mut frame = match self.backend.frame(self.target, &mut **capture).await {
            Ok(frame) => frame,
            Err(error) => {
                self.last_revision = None;
                return Err(error);
            }
        };
        if previous_revision != Some(frame.geometry.revision) {
            frame.damage = None;
        }
        self.last_revision = Some(frame.geometry.revision);
        self.next = self.backend.clock.monotonic() + self.period;
        Ok(Some(frame))
    }
}
struct Sink {
    backend: WindowsDesktop,
    target: TargetId,
    input: Arc<InputResource>,
}
#[async_trait]
impl InputSink for Sink {
    async fn inject(
        &mut self,
        action: &InputAction,
        geometry: &TargetGeometry,
    ) -> Result<(), PlatformError> {
        if matches!(action, InputAction::ReleaseAll) {
            return self.release_all().await;
        }
        let _operation = self.backend.enter().await?;
        let mut input = self.input.lock().await;
        let input = input.as_mut().ok_or_else(session_closed)?;
        let deadline = self.backend.clock.monotonic() + Duration::from_millis(250);
        let result = async {
            let (layout, monitor, current) = self.backend.resolve(self.target).await?;
            geometry::check_geometry(geometry, &current)?;
            input.inject_before(action, &layout, &monitor, deadline)
        }
        .await;
        if result.is_err() {
            input.fail();
        }
        result
    }
    async fn release_all(&mut self) -> Result<(), PlatformError> {
        // The same mutex serializes injection and release. ManagedInput calls
        // SendInput synchronously (one event per call), with no detached worker
        // or native queue awaiting insertion, so lock + release is the barrier.
        self.input
            .lock()
            .await
            .as_mut()
            .map_or(Ok(()), ManagedInput::barrier)
    }
}
pub(crate) fn capabilities(availability: Availability) -> DesktopCapabilities {
    DesktopCapabilities {
        backend: "windows".into(),
        capture: availability.clone(),
        input: availability.clone(),
        input_scope: InputScope::FocusedTarget,
        lock_session: availability,
        unlock_session: Availability::Unsupported {
            reason: "Windows lets only its own sign-in screen unlock it".into(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;
    #[derive(Debug)]
    struct Fake {
        outputs: Arc<StdMutex<Vec<Monitor>>>,
        emitted: Arc<StdMutex<usize>>,
        refuse: Arc<AtomicBool>,
        captures: Arc<std::sync::atomic::AtomicUsize>,
        capture_closes: Arc<std::sync::atomic::AtomicUsize>,
    }
    struct FakeInput {
        emitted: Arc<StdMutex<usize>>,
        refuse: Arc<AtomicBool>,
    }
    impl Injector for FakeInput {
        fn check_layout(&mut self, _: &Layout, _: Instant) -> Result<(), PlatformError> {
            Ok(())
        }
        fn emit(&mut self, _: input::Event) -> Result<(), PlatformError> {
            *self.emitted.lock().unwrap() += 1;
            if self.refuse.load(Ordering::Acquire) {
                Err(PlatformError::TemporarilyUnavailable(
                    "secure desktop".into(),
                ))
            } else {
                Ok(())
            }
        }
    }
    struct Pixels;
    struct TrackedPixels {
        captures: Arc<std::sync::atomic::AtomicUsize>,
        closes: Arc<std::sync::atomic::AtomicUsize>,
    }
    impl Drop for TrackedPixels {
        fn drop(&mut self) {
            self.captures.fetch_sub(1, Ordering::AcqRel);
        }
    }
    #[async_trait]
    impl Capture for TrackedPixels {
        async fn frame(&mut self, monitor: &Monitor) -> Result<CapturedFrame, PlatformError> {
            Pixels.frame(monitor).await
        }
        async fn close(&mut self) -> Result<(), PlatformError> {
            self.closes.fetch_add(1, Ordering::AcqRel);
            Ok(())
        }
    }
    #[async_trait]
    impl Capture for Pixels {
        async fn frame(&mut self, m: &Monitor) -> Result<CapturedFrame, PlatformError> {
            let color = if m.x < 0 {
                [255, 0, 0, 255]
            } else {
                [0, 255, 0, 255]
            };
            Ok(CapturedFrame {
                width: m.width,
                height: m.height,
                stride: m.width * 4,
                format: PixelFormat::Bgra8,
                data: color.repeat(m.width as usize * m.height as usize),
                geometry: m.geometry(0),
                cursor: None,
                damage: None,
            })
        }
    }
    impl Driver for Fake {
        fn monitors(&self) -> Result<Vec<Monitor>, PlatformError> {
            Ok(self.outputs.lock().unwrap().clone())
        }
        fn capabilities(&self) -> DesktopCapabilities {
            capabilities(Availability::Available)
        }
        fn capture(&self) -> Result<Box<dyn Capture>, PlatformError> {
            self.captures.fetch_add(1, Ordering::AcqRel);
            Ok(Box::new(TrackedPixels {
                captures: self.captures.clone(),
                closes: self.capture_closes.clone(),
            }))
        }
        fn injector(&self) -> Result<Box<dyn Injector>, PlatformError> {
            Ok(Box::new(FakeInput {
                emitted: self.emitted.clone(),
                refuse: self.refuse.clone(),
            }))
        }
        fn lock(&self) -> Result<(), PlatformError> {
            Err(PlatformError::Unsupported("fake locker".into()))
        }
        fn lock_state(&self) -> DesktopLockState {
            DesktopLockState::Locked
        }
    }
    fn fixture(refuse: bool) -> (WindowsDesktop, Arc<Fake>) {
        let fake = Arc::new(Fake {
            outputs: Arc::new(StdMutex::new(vec![
                Monitor {
                    name: "primary".into(),
                    x: 0,
                    y: 0,
                    width: 3,
                    height: 2,
                    dpi: 144,
                    rotation: 0,
                },
                Monitor {
                    name: "secondary".into(),
                    x: -2,
                    y: 0,
                    width: 2,
                    height: 3,
                    dpi: 96,
                    rotation: 90,
                },
            ])),
            emitted: Arc::new(StdMutex::new(0)),
            refuse: Arc::new(AtomicBool::new(refuse)),
            captures: Arc::default(),
            capture_closes: Arc::default(),
        });
        (
            WindowsDesktop::with_driver(fake.clone()).with_clock(Arc::new(FakeClock {
                base: Instant::now(),
                millis: std::sync::atomic::AtomicU64::new(0),
            })),
            fake,
        )
    }
    #[tokio::test]
    async fn catalog_ids_stay_stable_and_every_layout_change_fences_input() {
        let (desktop, fake) = fixture(false);
        let first = desktop.targets().await.unwrap();
        let target = &first[0];
        let mut sink = desktop.input_sink(target.target_id).await.unwrap();
        fake.outputs.lock().unwrap()[0].x += 1;
        let second = desktop.targets().await.unwrap();
        assert_eq!(first[0].target_id, second[0].target_id);
        assert!(second[0].geometry.revision > first[0].geometry.revision);
        let error = sink
            .inject(
                &InputAction::KeyPress {
                    key: "a".into(),
                    pressed: true,
                },
                &target.geometry,
            )
            .await
            .unwrap_err();
        assert!(matches!(error, PlatformError::GeometryChanged));
        assert_eq!(*fake.emitted.lock().unwrap(), 0);
        fake.outputs.lock().unwrap()[0].dpi = 192;
        let third = desktop.targets().await.unwrap();
        assert!(third[0].geometry.revision > second[0].geometry.revision);
        fake.outputs.lock().unwrap().pop();
        let fourth = desktop.targets().await.unwrap();
        assert!(fourth[0].geometry.revision > third[0].geometry.revision);
    }
    #[tokio::test]
    async fn close_releases_resources_even_when_consumers_retain_streams_and_sinks() {
        let (desktop, fake) = fixture(false);
        let target = desktop.targets().await.unwrap()[0].clone();
        let mut stream = desktop.open_stream(target.target_id, 30).await.unwrap();
        let mut sink = desktop.input_sink(target.target_id).await.unwrap();
        sink.inject(
            &InputAction::KeyPress {
                key: "ctrl".into(),
                pressed: true,
            },
            &target.geometry,
        )
        .await
        .unwrap();
        stream.next_frame().await.unwrap().unwrap();
        assert_eq!(fake.captures.load(Ordering::Acquire), 1);
        desktop.close().await.unwrap();
        assert_eq!(fake.captures.load(Ordering::Acquire), 0);
        assert_eq!(fake.capture_closes.load(Ordering::Acquire), 1);
        assert_eq!(*fake.emitted.lock().unwrap(), 2);
        assert!(stream.next_frame().await.unwrap().is_none());
        assert!(
            sink.inject(
                &InputAction::Text {
                    text: "refused".into()
                },
                &target.geometry
            )
            .await
            .is_err()
        );
        assert!(desktop.clone().targets().await.is_err());
        assert!(desktop.capture_still(target.target_id).await.is_err());
        assert!(desktop.open_stream(target.target_id, 30).await.is_err());
        assert!(desktop.input_sink(target.target_id).await.is_err());
        assert!(desktop.lock_session().await.is_err());
        assert!(!desktop.capabilities().await.capture.is_available());
        desktop.close().await.unwrap();
        sink.release_all().await.unwrap();
        drop(sink);
        drop(stream);
        assert_eq!(*fake.emitted.lock().unwrap(), 2);
        assert_eq!(fake.capture_closes.load(Ordering::Acquire), 1);
    }
    #[tokio::test]
    async fn close_reports_failed_releases_and_retries_without_reopening_admission() {
        let (desktop, fake) = fixture(false);
        let target = desktop.targets().await.unwrap()[0].clone();
        let mut sink = desktop.input_sink(target.target_id).await.unwrap();
        sink.inject(
            &InputAction::KeyPress {
                key: "ctrl".into(),
                pressed: true,
            },
            &target.geometry,
        )
        .await
        .unwrap();
        fake.refuse.store(true, Ordering::Release);
        assert!(desktop.close().await.is_err());
        fake.refuse.store(false, Ordering::Release);
        desktop.close().await.unwrap();
        assert_eq!(*fake.emitted.lock().unwrap(), 3);
        assert!(
            sink.inject(
                &InputAction::Text {
                    text: "refused".into()
                },
                &target.geometry
            )
            .await
            .is_err()
        );
        assert!(desktop.targets().await.is_err());
        drop(sink);
        assert_eq!(*fake.emitted.lock().unwrap(), 3);
    }
    #[tokio::test]
    async fn cancelling_close_retains_capture_until_cleanup_finishes() {
        struct WaitingCapture(Arc<AtomicBool>);
        #[async_trait]
        impl Capture for WaitingCapture {
            async fn frame(&mut self, _: &Monitor) -> Result<CapturedFrame, PlatformError> {
                Err(session_closed())
            }
            async fn close(&mut self) -> Result<(), PlatformError> {
                if !self.0.load(Ordering::Acquire) {
                    std::future::pending::<()>().await;
                }
                Ok(())
            }
        }
        let (desktop, _) = fixture(false);
        let finish = Arc::new(AtomicBool::new(false));
        let capture: Arc<CaptureResource> =
            Arc::new(Mutex::new(Some(Box::new(WaitingCapture(finish.clone())))));
        desktop
            .lifecycle
            .resources
            .lock()
            .await
            .captures
            .push(Arc::downgrade(&capture));
        let mut closing = Box::pin(desktop.close());
        let mut context = std::task::Context::from_waker(std::task::Waker::noop());
        assert!(std::future::Future::poll(closing.as_mut(), &mut context).is_pending());
        drop(closing);
        assert!(capture.lock().await.is_some());
        finish.store(true, Ordering::Release);
        desktop.close().await.unwrap();
        assert!(capture.lock().await.is_none());
        assert!(desktop.targets().await.is_err());
    }
    #[tokio::test]
    async fn composite_preserves_physical_layout_and_black_gaps() {
        let (desktop, _) = fixture(false);
        let targets = desktop.targets().await.unwrap();
        let combined = targets
            .iter()
            .find(|t| t.kind == TargetKind::Desktop)
            .unwrap();
        let frame = desktop.capture_still(combined.target_id).await.unwrap();
        assert_eq!((frame.width, frame.height, frame.stride), (5, 3, 20));
        assert_eq!(&frame.data[..4], &[255, 0, 0, 255]);
        assert_eq!(&frame.data[8..12], &[0, 255, 0, 255]);
        assert_eq!(&frame.data[56..60], &[0, 0, 0, 255]);
        let layout = Layout::new(vec![
            Monitor {
                name: "one".into(),
                x: 0,
                y: 0,
                width: 1,
                height: 1,
                dpi: 96,
                rotation: 0,
            },
            Monitor {
                name: "two".into(),
                x: 3,
                y: 0,
                width: 1,
                height: 1,
                dpi: 96,
                rotation: 0,
            },
        ])
        .unwrap();
        assert!(
            layout
                .physical(
                    &layout.desktop,
                    protocol::control::TargetPoint { x: 0.5, y: 0.0 }
                )
                .is_err()
        );
    }
    #[tokio::test]
    async fn lock_state_is_windows_own_until_sharing_ends() {
        let (desktop, _) = fixture(false);
        assert_eq!(desktop.desktop_lock().await, DesktopLockState::Locked);
        desktop.close().await.unwrap();
        assert_eq!(desktop.desktop_lock().await, DesktopLockState::Unknown);
    }
    #[tokio::test]
    async fn refusal_reaches_port_without_success_and_stream_returns_fenced_frame() {
        let (desktop, _) = fixture(true);
        let targets = desktop.targets().await.unwrap();
        let target = &targets[0];
        let mut sink = desktop.input_sink(target.target_id).await.unwrap();
        let result = sink
            .inject(&InputAction::Text { text: "x".into() }, &target.geometry)
            .await;
        assert!(matches!(
            result,
            Err(PlatformError::TemporarilyUnavailable(_))
        ));
        let mut stream = desktop.open_stream(target.target_id, 30).await.unwrap();
        assert_eq!(
            stream.next_frame().await.unwrap().unwrap().geometry,
            target.geometry
        );
        assert!(desktop.open_stream(target.target_id, 0).await.is_err());
    }
    #[derive(Debug)]
    struct FakeClock {
        base: Instant,
        millis: std::sync::atomic::AtomicU64,
    }
    impl Clock for FakeClock {
        fn now(&self) -> protocol::time::Timestamp {
            protocol::time::Timestamp(0)
        }
        fn monotonic(&self) -> Instant {
            self.base
                + Duration::from_millis(self.millis.load(std::sync::atomic::Ordering::Relaxed))
        }
    }
    #[tokio::test]
    async fn cancelling_a_frame_invalidates_damage_history() {
        let (desktop, _) = fixture(false);
        let clock = Arc::new(FakeClock {
            base: Instant::now(),
            millis: std::sync::atomic::AtomicU64::new(0),
        });
        let desktop = desktop.with_clock(clock.clone());
        let target = desktop.targets().await.unwrap()[0].target_id;
        let mut stream = Stream {
            backend: desktop.clone(),
            target,
            capture: Arc::new(Mutex::new(Some(Box::new(Pixels)))),
            period: Duration::from_millis(100),
            next: clock.monotonic(),
            last_revision: None,
        };
        stream.next_frame().await.unwrap();
        assert!(stream.last_revision.is_some());
        let mut pending = stream.next_frame();
        let mut context = std::task::Context::from_waker(std::task::Waker::noop());
        assert!(std::future::Future::poll(pending.as_mut(), &mut context).is_pending());
        drop(pending);
        assert!(stream.last_revision.is_none());
        clock
            .millis
            .store(200, std::sync::atomic::Ordering::Relaxed);
        assert!(stream.next_frame().await.unwrap().unwrap().damage.is_none());
    }
    #[tokio::test]
    async fn removing_a_target_releases_held_input() {
        let (desktop, fake) = fixture(false);
        let target = desktop.targets().await.unwrap()[0].clone();
        let mut sink = desktop.input_sink(target.target_id).await.unwrap();
        sink.inject(
            &InputAction::KeyPress {
                key: "ctrl".into(),
                pressed: true,
            },
            &target.geometry,
        )
        .await
        .unwrap();
        fake.outputs.lock().unwrap().remove(0);
        assert!(matches!(
            sink.inject(
                &InputAction::Text {
                    text: "blocked".into()
                },
                &target.geometry
            )
            .await,
            Err(PlatformError::TargetNotFound)
        ));
        assert_eq!(*fake.emitted.lock().unwrap(), 2);
        drop(sink);
        assert_eq!(*fake.emitted.lock().unwrap(), 2);
    }
}
