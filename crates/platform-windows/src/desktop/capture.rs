// SPDX-License-Identifier: Apache-2.0
//! D3D/WinRT objects stay on a dedicated MTA thread. Only owned packed bytes
//! cross into Tokio; dropping a capture stream closes its worker channel.
use super::cursor::{Shape, ShapeKind};
use super::native::{Dpi, failure, session_ready, unavailable};
use super::*;
use tokio::sync::{mpsc, oneshot};
use windows::{
    Foundation::TypedEventHandler,
    Graphics::{
        Capture::*,
        DirectX::{Direct3D11::IDirect3DDevice, DirectXPixelFormat},
    },
    Win32::{
        Foundation::*,
        Graphics::{
            Direct3D::*,
            Direct3D11::*,
            Dxgi::{Common::*, *},
            Gdi::*,
        },
        System::WinRT::{Direct3D11::*, Graphics::Capture::IGraphicsCaptureItemInterop, *},
    },
    core::Interface,
};
struct Request {
    monitor: Monitor,
    reply: oneshot::Sender<Result<CapturedFrame, PlatformError>>,
}
pub(super) struct Worker {
    requests: Option<mpsc::Sender<Request>>,
    thread: Option<std::thread::JoinHandle<()>>,
    closing: Option<tokio::task::JoinHandle<std::thread::Result<()>>>,
}
impl Worker {
    pub(super) fn new() -> Result<Self, PlatformError> {
        let (requests, mut incoming) = mpsc::channel::<Request>(1);
        let thread = std::thread::Builder::new()
            .name("desktop-capture".into())
            .spawn(move || {
                let initialized = Apartment::new();
                let mut sources: BTreeMap<String, (Monitor, Source)> = BTreeMap::new();
                while let Some(request) = incoming.blocking_recv() {
                    if request.reply.is_closed() {
                        continue;
                    }
                    let result = (|| {
                        if initialized.is_err() {
                            return Err(PlatformError::Native(
                                "Windows capture apartment initialization failed".into(),
                            ));
                        }
                        let _dpi = Dpi::enter()?;
                        session_ready()?;
                        if sources
                            .get(&request.monitor.name)
                            .is_some_and(|(old, _)| old != &request.monitor)
                        {
                            sources.remove(&request.monitor.name);
                        }
                        if !sources.contains_key(&request.monitor.name) {
                            sources.insert(
                                request.monitor.name.clone(),
                                (request.monitor.clone(), Source::new(&request.monitor)?),
                            );
                        }
                        let frame = sources
                            .get_mut(&request.monitor.name)
                            .ok_or_else(unavailable)?
                            .1
                            .frame(&request.monitor)?;
                        session_ready()?;
                        Ok(frame)
                    })();
                    if result.is_err() {
                        sources.clear();
                    }
                    if request.reply.send(result).is_err() {
                        // A cancelled consumer did not see this damage baseline.
                        sources.clear();
                    }
                }
                // Keep apartment alive until all capture objects and callbacks close.
                drop(sources);
                drop(initialized);
            })
            .map_err(PlatformError::Io)?;
        Ok(Self {
            requests: Some(requests),
            thread: Some(thread),
            closing: None,
        })
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.requests.take();
        // Cancellation can skip async close. Join even then so a dropped stream
        // cannot retain capture indicators or native frames beyond its lifetime.
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
#[async_trait]
impl Capture for Worker {
    async fn frame(&mut self, monitor: &Monitor) -> Result<CapturedFrame, PlatformError> {
        let (reply, response) = oneshot::channel();
        self.requests
            .as_ref()
            .ok_or_else(unavailable)?
            .send(Request {
                monitor: monitor.clone(),
                reply,
            })
            .await
            .map_err(|_| unavailable())?;
        response.await.map_err(|_| unavailable())?
    }
    async fn close(&mut self) -> Result<(), PlatformError> {
        self.requests.take();
        if let Some(thread) = self.thread.take() {
            self.closing = Some(tokio::task::spawn_blocking(move || thread.join()));
        }
        if let Some(closing) = self.closing.as_mut() {
            // Retain the join across cancellation so a later close still waits
            // for native resources instead of prematurely acknowledging it.
            let result = closing.await;
            self.closing.take();
            result
                .map_err(|_| unavailable())?
                .map_err(|_| unavailable())?;
        }
        Ok(())
    }
}
struct Apartment;
impl Apartment {
    fn new() -> Result<Self, PlatformError> {
        // The bindings cache WinRT factories for the life of the process, so
        // the apartment they came from must live as long. Otherwise the last
        // capture thread to leave tears it down and the next capture calls a
        // released factory. The usage is never released.
        static KEPT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if !KEPT.load(std::sync::atomic::Ordering::Acquire) {
            // SAFETY: no preconditions; the returned cookie is deliberately kept forever.
            unsafe {
                windows::Win32::System::Com::CoIncrementMTAUsage().map_err(failure)?;
            }
            KEPT.store(true, std::sync::atomic::Ordering::Release);
        }
        // SAFETY: fresh dedicated thread; paired RoUninitialize after all COM objects drop.
        unsafe {
            RoInitialize(RO_INIT_MULTITHREADED).map_err(failure)?;
        }
        Ok(Self)
    }
}
impl Drop for Apartment {
    fn drop(&mut self) {
        // SAFETY: successful RoInitialize on this same worker thread.
        unsafe {
            RoUninitialize();
        }
    }
}
/// Fails once the GPU device behind a capture has been removed or reset, after
/// which no capture through it can show a new picture.
fn device_alive(device: &ID3D11Device) -> Result<(), PlatformError> {
    // SAFETY: live device owned by the capture; the call only reads its state.
    unsafe { device.GetDeviceRemovedReason() }.map_err(failure)
}
struct Device {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    staging: Option<(u32, u32, ID3D11Texture2D)>,
}
impl Device {
    fn new(adapter: Option<&IDXGIAdapter>) -> windows::core::Result<Self> {
        let mut device = None;
        let mut context = None;
        // SAFETY: output pointers are initialized. Explicit adapter requires UNKNOWN.
        unsafe {
            D3D11CreateDevice(
                adapter,
                if adapter.is_some() {
                    D3D_DRIVER_TYPE_UNKNOWN
                } else {
                    D3D_DRIVER_TYPE_HARDWARE
                },
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )?;
        }
        Ok(Self {
            device: device.ok_or_else(|| windows::core::Error::from(E_FAIL))?,
            context: context.ok_or_else(|| windows::core::Error::from(E_FAIL))?,
            staging: None,
        })
    }
    fn read(
        &mut self,
        texture: &ID3D11Texture2D,
        geometry: TargetGeometry,
    ) -> Result<CapturedFrame, PlatformError> {
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: live texture and initialized output descriptor.
        unsafe {
            texture.GetDesc(&mut desc);
        }
        if desc.Width == 0
            || desc.Height == 0
            || u64::from(desc.Width) * u64::from(desc.Height) > 64 * 1024 * 1024
            || desc.Format != DXGI_FORMAT_B8G8R8A8_UNORM
            || desc.ArraySize != 1
            || desc.MipLevels != 1
            || desc.SampleDesc.Count != 1
        {
            return Err(geometry::invalid());
        }
        if !self
            .staging
            .as_ref()
            .is_some_and(|(w, h, _)| *w == desc.Width && *h == desc.Height)
        {
            let mut staging = None;
            let staging_desc = D3D11_TEXTURE2D_DESC {
                Usage: D3D11_USAGE_STAGING,
                BindFlags: 0,
                CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                MiscFlags: 0,
                MipLevels: 1,
                ArraySize: 1,
                ..desc
            };
            // SAFETY: staging descriptor has no bindings and permits CPU reads.
            unsafe {
                self.device
                    .CreateTexture2D(&staging_desc, None, Some(&mut staging))
                    .map_err(failure)?;
            }
            self.staging = Some((desc.Width, desc.Height, staging.ok_or_else(unavailable)?));
        }
        let staging = &self.staging.as_ref().ok_or_else(unavailable)?.2;
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        // SAFETY: resources have matching format/dimensions; Map synchronizes CopyResource.
        unsafe {
            self.context.CopyResource(staging, texture);
            self.context
                .Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
                .map_err(failure)?;
        }
        let result = (|| {
            let stride = desc.Width * 4;
            if mapped.pData.is_null()
                || mapped.RowPitch < stride
                || u64::from(mapped.RowPitch) * u64::from(desc.Height) > 512 * 1024 * 1024
            {
                return Err(geometry::invalid());
            }
            let mut data = vec![0; stride as usize * desc.Height as usize];
            for y in 0..desc.Height as usize {
                // SAFETY: Map guarantees RowPitch bytes per texture row. Only
                // initialized pixel bytes are copied; driver padding is excluded.
                let row = unsafe {
                    std::slice::from_raw_parts(
                        mapped.pData.cast::<u8>().add(y * mapped.RowPitch as usize),
                        stride as usize,
                    )
                };
                data[y * stride as usize..(y + 1) * stride as usize].copy_from_slice(row);
            }
            Ok(CapturedFrame {
                width: desc.Width,
                height: desc.Height,
                stride,
                format: PixelFormat::Bgra8,
                data,
                geometry,
                cursor: None,
                damage: None,
            })
        })();
        // SAFETY: exactly one Unmap for the successful Map above, including errors.
        unsafe {
            self.context.Unmap(staging, 0);
        }
        result
    }
}
enum Source {
    Wgc(Wgc),
    Duplication(Duplication),
}
impl Source {
    fn new(monitor: &Monitor) -> Result<Self, PlatformError> {
        match GraphicsCaptureSession::IsSupported() {
            Ok(true) => match Wgc::new(monitor) {
                Ok(source) => Ok(Self::Wgc(source)),
                Err(e) if matches!(e.code().0 as u32, 0x80004001 | 0x80040154 | 0x80004002) => {
                    Duplication::new(monitor).map(Self::Duplication)
                }
                Err(e) => Err(failure(e)),
            },
            Ok(false) => Duplication::new(monitor).map(Self::Duplication),
            Err(e) if matches!(e.code().0 as u32, 0x80040154 | 0x80004002) => {
                Duplication::new(monitor).map(Self::Duplication)
            }
            Err(e) => Err(failure(e)),
        }
    }
    fn frame(&mut self, monitor: &Monitor) -> Result<CapturedFrame, PlatformError> {
        match self {
            Self::Wgc(source) => source.frame(monitor),
            Self::Duplication(source) => source.frame(monitor),
        }
    }
}
struct Wgc {
    device: Device,
    pool: Direct3D11CaptureFramePool,
    session: GraphicsCaptureSession,
    token: i64,
    item: GraphicsCaptureItem,
    closed_token: i64,
    closed: Arc<std::sync::atomic::AtomicBool>,
    arrived: std::sync::mpsc::Receiver<()>,
    last: Option<CapturedFrame>,
}
impl Wgc {
    fn new(monitor: &Monitor) -> windows::core::Result<Self> {
        let device = Device::new(None)?;
        let dxgi: IDXGIDevice = device.device.cast()?;
        // SAFETY: correct DXGI interface from the live D3D11 device.
        let runtime: IDirect3DDevice =
            unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi)? }.cast()?;
        let interop: IGraphicsCaptureItemInterop =
            windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()?;
        let point = POINT {
            x: monitor.x,
            y: monitor.y,
        };
        // SAFETY: physical coordinate belongs to the enumerated output; creation
        // does not request window isolation or disable the OS capture indicator.
        let item: GraphicsCaptureItem =
            unsafe { interop.CreateForMonitor(MonitorFromPoint(point, MONITOR_DEFAULTTONULL))? };
        let size = item.Size()?;
        if size.Width != monitor.width as i32 || size.Height != monitor.height as i32 {
            return Err(windows::core::Error::from(E_FAIL));
        }
        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
            &runtime,
            DirectXPixelFormat::B8G8R8A8UIntNormalized,
            2,
            size,
        )?;
        let (sender, arrived) = std::sync::mpsc::sync_channel(1);
        let wake_closed = sender.clone();
        let token = pool.FrameArrived(&TypedEventHandler::new(move |_, _| {
            let _ = sender.try_send(());
            Ok(())
        }))?;
        let session = pool.CreateCaptureSession(&item)?;
        let closed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let signal = closed.clone();
        let closed_token = item.Closed(&TypedEventHandler::new(move |_, _| {
            signal.store(true, std::sync::atomic::Ordering::Release);
            let _ = wake_closed.try_send(());
            Ok(())
        }))?;
        let source = Self {
            device,
            pool,
            session,
            token,
            item,
            closed_token,
            closed,
            arrived,
            last: None,
        };
        source.session.SetIsCursorCaptureEnabled(true)?;
        source.session.StartCapture()?;
        Ok(source)
    }
    fn frame(&mut self, monitor: &Monitor) -> Result<CapturedFrame, PlatformError> {
        if self.closed.load(std::sync::atomic::Ordering::Acquire) {
            return Err(unavailable());
        }
        // Free-threaded events wake this worker without a desktop message loop.
        let timeout = if self.last.is_some() {
            Duration::from_millis(250)
        } else {
            Duration::from_secs(2)
        };
        match self.arrived.recv_timeout(timeout) {
            Ok(()) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if self.closed.load(std::sync::atomic::Ordering::Acquire) {
                    return Err(unavailable());
                }
                return self.repeat(monitor);
            }
            Err(_) => return Err(unavailable()),
        }
        if self.closed.load(std::sync::atomic::Ordering::Acquire) {
            return Err(unavailable());
        }
        let mut newest = None;
        // Pool holds at most two frames; drain to the newest without unbounded
        // work if an output produces frames faster than the requested rate.
        for _ in 0..2 {
            match self.pool.TryGetNextFrame() {
                Ok(frame) => {
                    if let Some(old) = newest.replace(frame) {
                        let _: windows::core::Result<()> = old.Close();
                    }
                }
                // An empty pool returns no frame, which these bindings report
                // as an error without a failure code; older ones used E_POINTER.
                Err(e) if e.code().is_ok() || e.code() == E_POINTER => break,
                Err(e) => return Err(failure(e)),
            }
        }
        let Some(frame) = newest else {
            return self.repeat(monitor);
        };
        let result = (|| {
            let size = frame.ContentSize().map_err(failure)?;
            if size.Width != monitor.width as i32 || size.Height != monitor.height as i32 {
                return Err(PlatformError::GeometryChanged);
            }
            let access: IDirect3DDxgiInterfaceAccess =
                frame.Surface().map_err(failure)?.cast().map_err(failure)?;
            // SAFETY: WinRT surface exposes the documented D3D11 texture interface.
            let texture: ID3D11Texture2D = unsafe { access.GetInterface().map_err(failure)? };
            self.device.read(&texture, monitor.geometry(0))
        })();
        frame.Close().map_err(failure)?;
        let result = result?;
        self.last = Some(result.clone());
        Ok(result)
    }
    /// Windows sends nothing for a screen that stays the same: the last
    /// picture again, marked unchanged so it costs the encoder nothing. It is
    /// only handed back while the capture could still deliver a new one, since
    /// it tells the phone the picture is current.
    fn repeat(&self, monitor: &Monitor) -> Result<CapturedFrame, PlatformError> {
        let mut frame = self.last.clone().ok_or_else(unavailable)?;
        device_alive(&self.device.device)?;
        let size = self.item.Size().map_err(failure)?;
        if size.Width != monitor.width as i32 || size.Height != monitor.height as i32 {
            return Err(PlatformError::GeometryChanged);
        }
        frame.damage = Some(Vec::new());
        Ok(frame)
    }
}
impl Drop for Wgc {
    fn drop(&mut self) {
        let _ = self.item.RemoveClosed(self.closed_token);
        let _ = self.pool.RemoveFrameArrived(self.token);
        let _ = self.session.Close();
        let _ = self.pool.Close();
    }
}
struct Duplication {
    device: Device,
    dup: IDXGIOutputDuplication,
    shape: Option<Shape>,
    position: (i32, i32),
    visible: bool,
    old_cursor: Option<DamageRect>,
    last: Option<CapturedFrame>,
}
struct Acquired<'a>(&'a IDXGIOutputDuplication);
impl Drop for Acquired<'_> {
    fn drop(&mut self) {
        // SAFETY: exactly one ReleaseFrame for the successful AcquireNextFrame.
        unsafe {
            let _ = self.0.ReleaseFrame();
        }
    }
}
impl Duplication {
    fn new(monitor: &Monitor) -> Result<Self, PlatformError> {
        // SAFETY: factory enumeration and interface casts validate adapter/output
        // lifetimes. The device is created on the output's actual adapter.
        unsafe {
            let factory: IDXGIFactory1 = CreateDXGIFactory1().map_err(failure)?;
            for a in 0..32 {
                let adapter = match factory.EnumAdapters1(a) {
                    Ok(a) => a,
                    Err(e) if e.code() == DXGI_ERROR_NOT_FOUND => break,
                    Err(e) => return Err(failure(e)),
                };
                for o in 0..64 {
                    let output = match adapter.EnumOutputs(o) {
                        Ok(o) => o,
                        Err(e) if e.code() == DXGI_ERROR_NOT_FOUND => break,
                        Err(e) => return Err(failure(e)),
                    };
                    let desc = output.GetDesc().map_err(failure)?;
                    let name = String::from_utf16_lossy(
                        &desc.DeviceName[..desc
                            .DeviceName
                            .iter()
                            .position(|c| *c == 0)
                            .unwrap_or(desc.DeviceName.len())],
                    );
                    if name != monitor.name {
                        continue;
                    }
                    let device =
                        Device::new(Some(&adapter.cast().map_err(failure)?)).map_err(failure)?;
                    let output: IDXGIOutput1 = output.cast().map_err(failure)?;
                    let dup = output.DuplicateOutput(&device.device).map_err(failure)?;
                    return Ok(Self {
                        device,
                        dup,
                        shape: None,
                        position: (0, 0),
                        visible: false,
                        old_cursor: None,
                        last: None,
                    });
                }
            }
        }
        Err(PlatformError::TargetNotFound)
    }
    fn frame(&mut self, monitor: &Monitor) -> Result<CapturedFrame, PlatformError> {
        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource = None;
        // SAFETY: live duplication session and valid outputs; timeout is bounded.
        match unsafe { self.dup.AcquireNextFrame(250, &mut info, &mut resource) } {
            Ok(()) => {}
            Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => {
                // Duplication reports a lost desktop itself; the device it reads
                // through is checked as for WGC before vouching for the picture.
                let mut frame = self.last.clone().ok_or_else(unavailable)?;
                device_alive(&self.device.device)?;
                frame.damage = Some(Vec::new());
                return Ok(frame);
            }
            Err(e) => return Err(failure(e)),
        }
        let _acquired = Acquired(&self.dup);
        let texture: ID3D11Texture2D = resource.ok_or_else(unavailable)?.cast().map_err(failure)?;
        let mut frame = self.device.read(&texture, monitor.geometry(0))?;
        let mut damage = dirty_rects(
            &self.dup,
            info.TotalMetadataBufferSize,
            frame.width,
            frame.height,
        )?;
        if monitor.rotation != 0 {
            let (data, w, h) = geometry::rotate_bgra(
                &frame.data,
                frame.width,
                frame.height,
                frame.stride,
                monitor.rotation,
            )?;
            frame.data = data;
            frame.width = w;
            frame.height = h;
            frame.stride = w * 4;
            // Dirty coordinates are in the unrotated texture; conservative full
            // damage avoids a partially transformed hint at rotated boundaries.
            damage = vec![DamageRect {
                x: 0,
                y: 0,
                width: w,
                height: h,
            }];
        }
        if frame.width != monitor.width || frame.height != monitor.height {
            return Err(PlatformError::GeometryChanged);
        }
        if info.PointerShapeBufferSize > 0 {
            if info.PointerShapeBufferSize > 4 * 1024 * 1024 {
                return Err(geometry::invalid());
            }
            let mut bytes = vec![0; info.PointerShapeBufferSize as usize];
            let mut required = 0;
            let mut shape = DXGI_OUTDUPL_POINTER_SHAPE_INFO::default();
            // SAFETY: driver-reported buffer size, initialized metadata outputs.
            unsafe {
                self.dup
                    .GetFramePointerShape(
                        bytes.len() as u32,
                        bytes.as_mut_ptr().cast(),
                        &mut required,
                        &mut shape,
                    )
                    .map_err(failure)?;
            }
            if required > info.PointerShapeBufferSize {
                return Err(geometry::invalid());
            }
            bytes.truncate(required as usize);
            let kind = match shape.Type {
                1 => ShapeKind::Monochrome,
                2 => ShapeKind::Color,
                4 => ShapeKind::MaskedColor,
                _ => return Err(geometry::invalid()),
            };
            self.shape = Some(Shape {
                kind,
                width: shape.Width,
                height: if kind == ShapeKind::Monochrome {
                    shape.Height / 2
                } else {
                    shape.Height
                },
                pitch: shape.Pitch,
                bytes,
            });
        }
        if info.LastMouseUpdateTime != 0 {
            self.position = (
                info.PointerPosition.Position.x,
                info.PointerPosition.Position.y,
            );
            self.visible = info.PointerPosition.Visible.as_bool();
        }
        if let Some(old) = self.old_cursor.take() {
            damage.push(old);
        }
        if self.visible {
            let shape = self.shape.as_ref().ok_or_else(unavailable)?;
            self.old_cursor = shape.overlay(
                &mut frame.data,
                frame.width,
                frame.height,
                self.position.0,
                self.position.1,
            )?;
            if let Some(rect) = self.old_cursor {
                damage.push(rect);
            }
        }
        frame.damage = if self.last.is_none() {
            None
        } else {
            Some(geometry::merge_damage(&damage, frame.width, frame.height))
        };
        self.last = Some(frame.clone());
        Ok(frame)
    }
}
fn dirty_rects(
    dup: &IDXGIOutputDuplication,
    bytes: u32,
    width: u32,
    height: u32,
) -> Result<Vec<DamageRect>, PlatformError> {
    if bytes > 16 * 1024 * 1024 {
        return Err(geometry::invalid());
    }
    if bytes == 0 {
        return Ok(Vec::new());
    }
    let mut dirty = vec![RECT::default(); (bytes as usize).div_ceil(size_of::<RECT>())];
    let mut moved = vec![
        DXGI_OUTDUPL_MOVE_RECT::default();
        (bytes as usize).div_ceil(size_of::<DXGI_OUTDUPL_MOVE_RECT>())
    ];
    let mut required = 0;
    // SAFETY: both typed buffers are aligned and cover the total metadata bound.
    unsafe {
        dup.GetFrameDirtyRects(bytes, dirty.as_mut_ptr(), &mut required)
            .map_err(failure)?;
    }
    if required > bytes {
        return Err(geometry::invalid());
    }
    dirty.truncate(required as usize / size_of::<RECT>());
    // SAFETY: move buffer covers the same documented combined metadata bound.
    unsafe {
        dup.GetFrameMoveRects(bytes, moved.as_mut_ptr(), &mut required)
            .map_err(failure)?;
    }
    if required > bytes {
        return Err(geometry::invalid());
    }
    moved.truncate(required as usize / size_of::<DXGI_OUTDUPL_MOVE_RECT>());
    for moved in moved {
        dirty.push(moved.DestinationRect);
        dirty.push(RECT {
            left: moved.SourcePoint.x,
            top: moved.SourcePoint.y,
            right: moved.SourcePoint.x.saturating_add(
                moved
                    .DestinationRect
                    .right
                    .saturating_sub(moved.DestinationRect.left),
            ),
            bottom: moved.SourcePoint.y.saturating_add(
                moved
                    .DestinationRect
                    .bottom
                    .saturating_sub(moved.DestinationRect.top),
            ),
        });
    }
    let rects: Vec<_> = dirty
        .iter()
        .filter_map(|r| {
            let left = r.left.max(0).min(width as i32) as u32;
            let top = r.top.max(0).min(height as i32) as u32;
            let right = r.right.max(0).min(width as i32) as u32;
            let bottom = r.bottom.max(0).min(height as i32) as u32;
            (right > left && bottom > top).then_some(DamageRect {
                x: left,
                y: top,
                width: right.saturating_sub(left),
                height: bottom.saturating_sub(top),
            })
        })
        .collect();
    Ok(geometry::merge_damage(&rects, width, height))
}
