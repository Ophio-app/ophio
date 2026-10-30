// SPDX-License-Identifier: Apache-2.0
use crate::LinuxDesktop;
use async_trait::async_trait;
use ports::desktop::{CapturedFrame, DesktopBackend, FrameStream, PlatformError};
use protocol::ids::TargetId;
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::Notify;
type Slot = Arc<Mutex<Option<Result<CapturedFrame, PlatformError>>>>;
pub(crate) struct CaptureStream {
    slot: Slot,
    wake: Arc<Notify>,
    task: tokio::task::JoinHandle<()>,
}
impl CaptureStream {
    pub fn start(desktop: LinuxDesktop, target: TargetId, fps: u8) -> Self {
        let slot: Slot = Arc::new(Mutex::new(None));
        let producer = slot.clone();
        let wake = Arc::new(Notify::new());
        let notify = wake.clone();
        let task = tokio::spawn(async move {
            let interval = Duration::from_secs_f64(1.0 / f64::from(fps));
            let mut capture = None;
            let mut capture_geometry = None;
            loop {
                let start = desktop.clock.monotonic();
                let mut frame = async {
                    desktop.check_lock().await.map_err(|e| match e {
                        PlatformError::Locked => {
                            PlatformError::TemporarilyUnavailable("Computer is locked".into())
                        }
                        other => other,
                    })?;
                    let (output, geometry) = desktop.resolve(target).await?;
                    if capture_geometry != Some(geometry) {
                        capture = if output.name == crate::COMBINED {
                            None
                        } else {
                            desktop.driver.stream(&output).await?
                        };
                        capture_geometry = Some(geometry);
                    }
                    if let Some(capture) = &mut capture {
                        let mut frame = capture.next_frame().await?;
                        crate::validate_frame(&frame)?;
                        if desktop.resolve(target).await?.1 != geometry
                            || (frame.width, frame.height) != (geometry.width, geometry.height)
                        {
                            return Err(PlatformError::GeometryChanged);
                        }
                        frame.geometry = geometry;
                        desktop.check_lock().await.map_err(|e| match e {
                            PlatformError::Locked => {
                                PlatformError::TemporarilyUnavailable("Computer is locked".into())
                            }
                            other => other,
                        })?;
                        Ok(frame)
                    } else {
                        desktop.capture_still(target).await
                    }
                }
                .await;
                if matches!(frame, Err(PlatformError::GeometryChanged)) {
                    capture_geometry = None;
                    tokio::task::yield_now().await;
                    continue;
                }
                if matches!(
                    &frame,
                    Err(PlatformError::TemporarilyUnavailable(_)
                        | PlatformError::PermissionMissing(_))
                ) {
                    capture_geometry = None;
                }
                let terminal = matches!(
                    frame,
                    Err(PlatformError::TargetNotFound
                        | PlatformError::Unsupported(_)
                        | PlatformError::Native(_)
                        | PlatformError::Io(_))
                );
                {
                    let mut latest = producer.lock().unwrap_or_else(|e| e.into_inner());
                    // A replaced frame's damage cannot be discarded independently of its pixels.
                    if latest.is_some()
                        && let Ok(f) = &mut frame
                    {
                        f.damage = None;
                    }
                    *latest = Some(frame);
                }
                notify.notify_one();
                if terminal {
                    break;
                }
                let elapsed = desktop.clock.monotonic().saturating_duration_since(start);
                tokio::time::sleep(interval.saturating_sub(elapsed)).await;
            }
        });
        Self { slot, wake, task }
    }
}
impl Drop for CaptureStream {
    fn drop(&mut self) {
        self.task.abort();
    }
}
#[async_trait]
impl FrameStream for CaptureStream {
    async fn next_frame(&mut self) -> Result<Option<CapturedFrame>, PlatformError> {
        loop {
            let wake = self.wake.notified();
            if let Some(frame) = self.slot.lock().unwrap_or_else(|e| e.into_inner()).take() {
                return frame.map(Some);
            }
            if self.task.is_finished() {
                return Ok(None);
            }
            wake.await;
        }
    }
}
