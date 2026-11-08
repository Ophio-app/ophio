// SPDX-License-Identifier: Apache-2.0
//! Screen capture through the system's ScreenCaptureKit. Its objects stay on the
//! threads it calls and those started here; only owned bytes cross into Tokio.
#![allow(unsafe_code)]

use super::native;
use async_trait::async_trait;
use block2::RcBlock;
use dispatch2::DispatchQueue;
use objc2::{
    AllocAnyThread, DefinedClass, define_class, msg_send,
    rc::Retained,
    runtime::{NSObject, NSObjectProtocol, ProtocolObject},
};
use objc2_core_media::{CMSampleBuffer, CMTime};
use objc2_core_video::{
    CVPixelBufferGetBaseAddress, CVPixelBufferGetBytesPerRow, CVPixelBufferGetHeight,
    CVPixelBufferGetWidth, CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags,
    CVPixelBufferUnlockBaseAddress, kCVPixelFormatType_32BGRA,
};
use objc2_foundation::NSError;
use objc2_screen_capture_kit::{
    SCContentFilter, SCShareableContent, SCStream, SCStreamConfiguration, SCStreamOutput,
    SCStreamOutputType,
};
use platform_windows::desktop::{Capture, geometry::Monitor};
use ports::desktop::{CapturedFrame, PixelFormat, PlatformError};
use protocol::control::TargetGeometry;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, mpsc},
    time::Duration,
};
use tokio::sync::Notify;

/// How long a frame may take to come when the screen is changing or a stream has
/// just started. A screen that isn't changing sends nothing, and the frame before
/// stands.
const WAIT_FOR_CHANGE: Duration = Duration::from_millis(500);
const WAIT_TO_START: Duration = Duration::from_secs(10);
const FRAMES_PER_SECOND: i32 = 30;

#[derive(Debug)]
struct Frame {
    width: u32,
    height: u32,
    stride: u32,
    data: Vec<u8>,
}

#[derive(Debug, Default)]
struct Shared {
    latest: Mutex<(u64, Option<Arc<Frame>>)>,
    arrived: Notify,
}

fn copied(buffer: &CMSampleBuffer) -> Option<Frame> {
    // SAFETY: the buffer is valid for this callback; the base address is read
    // only between lock and unlock, within `stride * height` bytes.
    unsafe {
        if !buffer.is_valid() {
            return None;
        }
        // A sample that says only that nothing changed carries no picture.
        let image = buffer.image_buffer()?;
        let read_only = CVPixelBufferLockFlags::ReadOnly;
        if CVPixelBufferLockBaseAddress(&image, read_only) != 0 {
            return None;
        }
        let width = CVPixelBufferGetWidth(&image);
        let height = CVPixelBufferGetHeight(&image);
        let stride = CVPixelBufferGetBytesPerRow(&image);
        let base = CVPixelBufferGetBaseAddress(&image).cast::<u8>();
        let frame = (!base.is_null()).then(|| Frame {
            width: width as u32,
            height: height as u32,
            stride: stride as u32,
            data: std::slice::from_raw_parts(base, stride * height).to_vec(),
        });
        CVPixelBufferUnlockBaseAddress(&image, read_only);
        frame
    }
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "OphioScreenFrames"]
    #[ivars = Arc<Shared>]
    struct FrameSink;

    unsafe impl NSObjectProtocol for FrameSink {}

    unsafe impl SCStreamOutput for FrameSink {
        #[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
        unsafe fn did_output(
            &self,
            _stream: &SCStream,
            buffer: &CMSampleBuffer,
            kind: SCStreamOutputType,
        ) {
            if kind != SCStreamOutputType::Screen {
                return;
            }
            if let Some(frame) = copied(buffer) {
                let shared = self.ivars();
                {
                    let mut latest = shared
                        .latest
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    latest.0 += 1;
                    latest.1 = Some(Arc::new(frame));
                }
                shared.arrived.notify_waiters();
            }
        }
    }
);

impl FrameSink {
    fn new(shared: Arc<Shared>) -> Retained<Self> {
        let this = Self::alloc().set_ivars(shared);
        // SAFETY: NSObject's init, on a freshly allocated object.
        unsafe { msg_send![super(this), init] }
    }
}

fn refused(error: Option<&NSError>) -> PlatformError {
    match error.map(|error| error.localizedDescription().to_string()) {
        Some(said) => PlatformError::TemporarilyUnavailable(format!(
            "This Mac's screen can't be captured: {said}"
        )),
        None => PlatformError::TemporarilyUnavailable("This Mac's screen can't be captured".into()),
    }
}

/// A running stream of one screen.
struct Running {
    stream: Retained<SCStream>,
    _sink: Retained<FrameSink>,
    _queue: dispatch2::DispatchRetained<DispatchQueue>,
    shared: Arc<Shared>,
    size: (u32, u32),
}
// SAFETY: ScreenCaptureKit's objects may be used from any thread; each is touched
// here only through `&mut` or after being moved to a blocking thread to stop it.
unsafe impl Send for Running {}

/// Calls a system function that answers through a completion handler, and waits.
fn answered(
    call: impl FnOnce(&block2::DynBlock<dyn Fn(*mut NSError)>),
) -> Result<(), PlatformError> {
    let (done, finished) = mpsc::channel();
    let block = RcBlock::new(move |error: *mut NSError| {
        // SAFETY: the system passes null or a live error for this call.
        let said = unsafe { error.as_ref() }.map(|error| error.localizedDescription().to_string());
        let _ = done.send(said);
    });
    call(&block);
    match finished.recv_timeout(WAIT_TO_START) {
        Ok(None) => Ok(()),
        Ok(Some(said)) => Err(PlatformError::TemporarilyUnavailable(format!(
            "This Mac's screen can't be captured: {said}"
        ))),
        Err(_) => Err(PlatformError::TemporarilyUnavailable(
            "This Mac's screen didn't answer in time".into(),
        )),
    }
}

/// Starts capturing the screen a monitor stands for. Blocking.
fn start(monitor: &Monitor) -> Result<Running, PlatformError> {
    let display_id = native::displays()?
        .into_iter()
        .find(|&display| native::named(display) == monitor.name)
        .ok_or(PlatformError::TargetNotFound)?;
    let (found, content) = mpsc::channel();
    let block = RcBlock::new(
        move |content: *mut SCShareableContent, error: *mut NSError| {
            // SAFETY: the system passes null or live objects for this call, which are
            // retained here before it returns.
            let answer = unsafe {
                match Retained::retain(content) {
                    Some(content) => Ok(SendContent(content)),
                    None => Err(refused(error.as_ref())),
                }
            };
            let _ = found.send(answer);
        },
    );
    // SAFETY: the block is called once by the system and outlives the call.
    unsafe { SCShareableContent::getShareableContentWithCompletionHandler(&block) };
    let content = content
        .recv_timeout(WAIT_TO_START)
        .map_err(|_| refused(None))??
        .0;
    // SAFETY: plain getters on a live object.
    let display = unsafe { content.displays() }
        .iter()
        .find(|display| unsafe { display.displayID() } == display_id)
        .ok_or(PlatformError::TargetNotFound)?;

    let shared = Arc::new(Shared::default());
    let sink = FrameSink::new(shared.clone());
    let queue = DispatchQueue::new("app.ophio.screen", None);
    // SAFETY: live objects, valid settings; the filter and configuration are kept
    // by the stream, and the output and queue are kept by `Running`.
    let stream = unsafe {
        let no_windows = objc2_foundation::NSArray::new();
        let filter = SCContentFilter::initWithDisplay_excludingWindows(
            SCContentFilter::alloc(),
            &display,
            &no_windows,
        );
        let settings = SCStreamConfiguration::new();
        settings.setWidth(monitor.width as usize);
        settings.setHeight(monitor.height as usize);
        settings.setPixelFormat(kCVPixelFormatType_32BGRA);
        settings.setShowsCursor(true);
        settings.setQueueDepth(3);
        settings.setMinimumFrameInterval(CMTime::new(1, FRAMES_PER_SECOND));
        let stream = SCStream::initWithFilter_configuration_delegate(
            SCStream::alloc(),
            &filter,
            &settings,
            None,
        );
        stream
            .addStreamOutput_type_sampleHandlerQueue_error(
                ProtocolObject::from_ref(&*sink),
                SCStreamOutputType::Screen,
                Some(&queue),
            )
            .map_err(|error| refused(Some(&error)))?;
        stream
    };
    // SAFETY: a live stream; `answered` keeps the block until it has been called.
    answered(|done| unsafe { stream.startCaptureWithCompletionHandler(Some(done)) })?;
    Ok(Running {
        stream,
        _sink: sink,
        _queue: queue,
        shared,
        size: (monitor.width, monitor.height),
    })
}

struct SendContent(Retained<SCShareableContent>);
// SAFETY: moved once from the system's callback to the thread that waits for it.
unsafe impl Send for SendContent {}

/// One timeline: what `frame` last returned for each screen.
pub(super) struct MacCapture {
    running: HashMap<String, Running>,
    seen: HashMap<String, u64>,
}
impl MacCapture {
    pub(super) fn new() -> Self {
        Self {
            running: HashMap::new(),
            seen: HashMap::new(),
        }
    }
}

fn captured(frame: &Frame, monitor: &Monitor, changed: bool) -> CapturedFrame {
    CapturedFrame {
        width: frame.width,
        height: frame.height,
        stride: frame.stride,
        format: PixelFormat::Bgra8,
        data: frame.data.clone(),
        // The shared code fills in the current revision.
        geometry: TargetGeometry {
            width: monitor.width,
            height: monitor.height,
            scale: 1.0,
            rotation_degrees: 0,
            revision: 0,
        },
        cursor: None,
        damage: if changed { None } else { Some(Vec::new()) },
    }
}

fn stop(running: Running) -> Result<(), PlatformError> {
    // SAFETY: a live stream; `answered` keeps the block until it has been called.
    answered(|done| unsafe { running.stream.stopCaptureWithCompletionHandler(Some(done)) })
}

#[async_trait]
impl Capture for MacCapture {
    async fn frame(&mut self, monitor: &Monitor) -> Result<CapturedFrame, PlatformError> {
        let current = self
            .running
            .get(&monitor.name)
            .is_some_and(|running| running.size == (monitor.width, monitor.height));
        if !current {
            if let Some(old) = self.running.remove(&monitor.name) {
                let _ = tokio::task::spawn_blocking(move || stop(old)).await;
            }
            self.seen.remove(&monitor.name);
            let wanted = monitor.clone();
            let started = tokio::task::spawn_blocking(move || start(&wanted))
                .await
                .map_err(|_| refused(None))??;
            self.running.insert(monitor.name.clone(), started);
        }
        let running = self
            .running
            .get(&monitor.name)
            .ok_or(PlatformError::TargetNotFound)?;
        let shared = running.shared.clone();
        let seen = self.seen.get(&monitor.name).copied();
        // The first frame may take a moment; after that an unchanged screen
        // returns the frame before.
        let limit = if seen.is_some() {
            WAIT_FOR_CHANGE
        } else {
            WAIT_TO_START
        };
        let deadline = tokio::time::Instant::now() + limit;
        loop {
            let arrived = shared.arrived.notified();
            let (number, frame) = {
                let latest = shared
                    .latest
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                (latest.0, latest.1.clone())
            };
            if let Some(frame) = &frame
                && seen != Some(number)
            {
                self.seen.insert(monitor.name.clone(), number);
                return Ok(captured(frame, monitor, true));
            }
            if tokio::time::timeout_at(deadline, arrived).await.is_err() {
                return match frame {
                    Some(frame) => Ok(captured(&frame, monitor, false)),
                    None => Err(PlatformError::TemporarilyUnavailable(
                        "This Mac's screen sent no picture. It may be asleep.".into(),
                    )),
                };
            }
        }
    }

    async fn close(&mut self) -> Result<(), PlatformError> {
        let running: Vec<Running> = self.running.drain().map(|(_, running)| running).collect();
        tokio::task::spawn_blocking(move || {
            let mut failure = None;
            for one in running {
                if let Err(error) = stop(one) {
                    failure = Some(error);
                }
            }
            failure.map_or(Ok(()), Err)
        })
        .await
        .map_err(|_| refused(None))?
    }
}
