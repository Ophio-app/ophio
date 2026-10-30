// SPDX-License-Identifier: Apache-2.0
use async_trait::async_trait;
use platform_linux::{
    BackendKind, Environment, LinuxDesktop,
    native::{Driver, Event, NativeInput, Output},
};
use ports::desktop::{CapturedFrame, DesktopBackend, PixelFormat, PlatformError};
use protocol::{
    capability::InputScope,
    control::{InputAction, TargetGeometry, TargetKind, TargetPoint},
};
use std::sync::{Arc, Mutex};

#[test]
fn detection_matrix_is_pure_and_honest() {
    for desktop in ["GNOME", "KDE", "Hyprland", "sway", "unknown"] {
        let env = Environment {
            session_type: "wayland".into(),
            current_desktop: desktop.into(),
            portal_available: true,
            ..Default::default()
        };
        assert_eq!(LinuxDesktop::detect(&env).kind, BackendKind::Portal);
        assert!(!LinuxDesktop::detect(&env).capabilities.input.is_available());
        let env = Environment {
            wayland_globals: [
                "wl_shm",
                "wl_seat",
                "zwlr_screencopy_manager_v1",
                "zwlr_virtual_pointer_manager_v1",
                "zwp_virtual_keyboard_manager_v1",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect(),
            ..env
        };
        assert_eq!(LinuxDesktop::detect(&env).kind, BackendKind::Wlroots);
        assert_eq!(
            LinuxDesktop::detect(&env).capabilities.input_scope,
            InputScope::FocusedTarget
        );
    }
    assert_eq!(
        LinuxDesktop::detect(&Environment {
            session_type: "x11".into(),
            x11_available: true,
            ..Default::default()
        })
        .kind,
        BackendKind::X11
    );
    for session in ["", "tty", "other"] {
        assert_eq!(
            LinuxDesktop::detect(&Environment {
                session_type: session.into(),
                ..Default::default()
            })
            .kind,
            BackendKind::Unsupported
        );
    }
}
#[derive(Debug)]
struct Fake {
    captures: std::sync::atomic::AtomicU64,
    outputs: Mutex<Vec<Output>>,
    events: Arc<Mutex<Vec<Event>>>,
}
struct Sink(Arc<Mutex<Vec<Event>>>);
#[async_trait]
impl NativeInput for Sink {
    async fn barrier(&mut self) -> Result<(), PlatformError> {
        // Synthetic events are recorded synchronously; there is no queue.
        Ok(())
    }
    async fn event(&mut self, event: Event) -> Result<(), PlatformError> {
        self.0.lock().unwrap().push(event);
        Ok(())
    }
}
#[async_trait]
impl Driver for Fake {
    async fn outputs(&self) -> Result<Vec<Output>, PlatformError> {
        Ok(self.outputs.lock().unwrap().clone())
    }
    async fn input(&self) -> Result<Box<dyn NativeInput>, PlatformError> {
        Ok(Box::new(Sink(self.events.clone())))
    }
    async fn capture(&self, o: &Output) -> Result<CapturedFrame, PlatformError> {
        self.captures
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(CapturedFrame {
            width: o.width,
            height: o.height,
            stride: o.width * 4,
            format: PixelFormat::Rgba8,
            data: [10, 20, 30, 255].repeat((o.width * o.height) as usize),
            geometry: TargetGeometry {
                width: o.width,
                height: o.height,
                scale: o.scale,
                rotation_degrees: 0,
                revision: 0,
            },
            cursor: None,
            damage: None,
        })
    }
}
#[tokio::test]
async fn mixed_scale_combined_layout_and_stale_geometry() {
    let first = Output {
        name: "left".into(),
        x: -100.0,
        y: 0.0,
        width: 200,
        height: 100,
        scale: 2.0,
        rotation: 0,
    };
    let second = Output {
        name: "right".into(),
        x: 0.0,
        y: 0.0,
        width: 100,
        height: 50,
        scale: 1.0,
        rotation: 0,
    };
    let fake = Arc::new(Fake {
        captures: Default::default(),
        outputs: Mutex::new(vec![first, second]),
        events: Default::default(),
    });
    let desktop = LinuxDesktop::new(
        fake.clone(),
        LinuxDesktop::detect(&Environment {
            session_type: "x11".into(),
            x11_available: true,
            ..Default::default()
        })
        .capabilities,
        None,
    );
    let targets = desktop.targets().await.unwrap();
    let combined = targets
        .iter()
        .find(|t| t.kind == TargetKind::Desktop)
        .unwrap();
    assert_eq!(
        (
            combined.geometry.width,
            combined.geometry.height,
            combined.geometry.scale
        ),
        (400, 100, 2.0)
    );
    let frame = desktop.capture_still(combined.target_id).await.unwrap();
    assert!(
        frame
            .data
            .as_chunks::<4>()
            .0
            .iter()
            .all(|p| *p == [10, 20, 30, 255])
    );
    let mut sink = desktop.input_sink(combined.target_id).await.unwrap();
    sink.inject(
        &InputAction::PointerMove {
            to: TargetPoint { x: 0.0, y: 0.0 },
        },
        &combined.geometry,
    )
    .await
    .unwrap();
    assert!(matches!(
        fake.events.lock().unwrap()[0],
        Event::Motion { x: -100.0, y: 0.0 }
    ));
    fake.outputs.lock().unwrap()[0].x = -110.0;
    assert!(matches!(
        sink.inject(
            &InputAction::KeyPress {
                key: "ctrl".into(),
                pressed: true
            },
            &combined.geometry
        )
        .await,
        Err(PlatformError::GeometryChanged)
    ));
    assert_eq!(fake.events.lock().unwrap().len(), 1);
}

#[derive(Debug)]
struct FakeClock {
    start: std::time::Instant,
    elapsed: std::sync::atomic::AtomicU64,
}
impl ports::clock::Clock for FakeClock {
    fn now(&self) -> protocol::time::Timestamp {
        protocol::time::Timestamp(self.elapsed.load(std::sync::atomic::Ordering::Relaxed) as i64)
    }
    fn monotonic(&self) -> std::time::Instant {
        self.start
            + std::time::Duration::from_millis(
                self.elapsed.load(std::sync::atomic::Ordering::Relaxed),
            )
    }
}
#[tokio::test(start_paused = true)]
async fn capture_obeys_max_fps_with_injected_clock() {
    use std::{sync::atomic::Ordering, time::Duration};
    let fake = Arc::new(Fake {
        captures: Default::default(),
        outputs: Mutex::new(vec![Output {
            name: "test".into(),
            x: 0.0,
            y: 0.0,
            width: 10,
            height: 10,
            scale: 1.0,
            rotation: 0,
        }]),
        events: Default::default(),
    });
    let clock = Arc::new(FakeClock {
        start: std::time::Instant::now(),
        elapsed: Default::default(),
    });
    let desktop = LinuxDesktop::new(
        fake.clone(),
        LinuxDesktop::detect(&Environment {
            session_type: "x11".into(),
            x11_available: true,
            ..Default::default()
        })
        .capabilities,
        None,
    )
    .with_clock(clock.clone());
    let target = desktop.targets().await.unwrap()[0].target_id;
    let mut stream = desktop.open_stream(target, 10).await.unwrap();
    let _ = stream.next_frame().await.unwrap();
    assert_eq!(fake.captures.load(Ordering::Relaxed), 1);
    clock.elapsed.store(99, Ordering::Relaxed);
    tokio::time::advance(Duration::from_millis(99)).await;
    tokio::task::yield_now().await;
    assert_eq!(fake.captures.load(Ordering::Relaxed), 1);
    clock.elapsed.store(101, Ordering::Relaxed);
    tokio::time::advance(Duration::from_millis(2)).await;
    tokio::task::yield_now().await;
    assert_eq!(fake.captures.load(Ordering::Relaxed), 2);
}

#[derive(Debug)]
struct Locker(Arc<std::sync::atomic::AtomicBool>);
#[async_trait]
impl platform_linux::native::SessionLocker for Locker {
    async fn lock(&self) -> Result<(), PlatformError> {
        self.0.store(true, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    }
}
#[tokio::test]
async fn lock_uses_injected_service() {
    let called = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let fake = Arc::new(Fake {
        captures: Default::default(),
        outputs: Default::default(),
        events: Default::default(),
    });
    let desktop = LinuxDesktop::new(
        fake,
        LinuxDesktop::detect(&Environment::default()).capabilities,
        Some(Arc::new(Locker(called.clone()))),
    );
    desktop.lock_session().await.unwrap();
    assert!(called.load(std::sync::atomic::Ordering::Relaxed));
}

#[derive(Debug, Clone)]
struct DelayedDriver {
    queued: bool,
    started: Arc<tokio::sync::Semaphore>,
    finish: Arc<tokio::sync::Semaphore>,
    events: Arc<Mutex<Vec<Event>>>,
}
struct DelayedInput(Arc<DelayedDriver>);
#[async_trait]
impl NativeInput for DelayedInput {
    async fn event(&mut self, event: Event) -> Result<(), PlatformError> {
        self.event_cancellable(event, Arc::new(std::sync::atomic::AtomicBool::new(false)))
            .await
    }
    async fn event_cancellable(
        &mut self,
        event: Event,
        cancelled: Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<(), PlatformError> {
        if matches!(event, Event::Key { pressed: true, .. }) {
            self.0.started.add_permits(1);
            self.0.finish.acquire().await.unwrap().forget();
            // Model a native worker waiting to enter the OS, or an OS call
            // already in flight whose acknowledgement cannot be cancelled.
            if self.0.queued && cancelled.load(std::sync::atomic::Ordering::SeqCst) {
                return Ok(());
            }
        }
        self.0.events.lock().unwrap().push(event);
        Ok(())
    }
    async fn barrier(&mut self) -> Result<(), PlatformError> {
        // Native call completion is this synthetic driver's acknowledgement.
        Ok(())
    }
}
#[async_trait]
impl Driver for DelayedDriver {
    async fn outputs(&self) -> Result<Vec<Output>, PlatformError> {
        Ok(vec![Output {
            name: "synthetic".into(),
            x: 0.0,
            y: 0.0,
            width: 10,
            height: 10,
            scale: 1.0,
            rotation: 0,
        }])
    }
    async fn capture(&self, _: &Output) -> Result<CapturedFrame, PlatformError> {
        Err(PlatformError::Unsupported("Synthetic input only".into()))
    }
    async fn input(&self) -> Result<Box<dyn NativeInput>, PlatformError> {
        Ok(Box::new(DelayedInput(Arc::new(self.clone()))))
    }
}
#[tokio::test(start_paused = true)]
async fn cancelled_injection_and_cancelled_barrier_retain_native_work_until_quiescent() {
    for queued in [false, true] {
        cancellation_case(queued).await;
    }
}
async fn cancellation_case(queued: bool) {
    let driver = Arc::new(DelayedDriver {
        queued,
        started: Arc::new(tokio::sync::Semaphore::new(0)),
        finish: Arc::new(tokio::sync::Semaphore::new(0)),
        events: Default::default(),
    });
    let desktop = LinuxDesktop::new(
        driver.clone(),
        LinuxDesktop::detect(&Environment {
            session_type: "x11".into(),
            x11_available: true,
            ..Default::default()
        })
        .capabilities,
        None,
    );
    let target = desktop.targets().await.unwrap().remove(0);
    let mut sink = desktop.input_sink(target.target_id).await.unwrap();
    let action = InputAction::KeyPress {
        key: "ctrl".into(),
        pressed: true,
    };
    {
        let injection = sink.inject(&action, &target.geometry);
        tokio::pin!(injection);
        tokio::select! {
            result = &mut injection => panic!("native gate escaped: {result:?}"),
            permit = driver.started.acquire() => permit.unwrap().forget(),
        }
        // Drop the caller future with a native action still pending.
    }
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(2), sink.release_all())
            .await
            .is_err()
    );
    assert!(driver.events.lock().unwrap().is_empty());
    driver.finish.add_permits(1);
    sink.release_all().await.unwrap();
    {
        let events = driver.events.lock().unwrap();
        assert_eq!(events.len(), if queued { 1 } else { 2 });
        if !queued {
            assert!(matches!(events[0], Event::Key { pressed: true, .. }));
        }
        assert!(matches!(
            events.last().unwrap(),
            Event::Key { pressed: false, .. }
        ));
    }
    // A second successful barrier has nothing left to release or inject.
    sink.release_all().await.unwrap();
    assert_eq!(
        driver.events.lock().unwrap().len(),
        if queued { 1 } else { 2 }
    );
}
