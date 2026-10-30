// SPDX-License-Identifier: Apache-2.0
//! wlroots protocols on an explicitly supplied Wayland socket.
use crate::native::{Driver, Event, NativeCapture, NativeInput, Output, failure};
use async_trait::async_trait;
use ports::desktop::{CapturedFrame, DamageRect, PixelFormat, PlatformError};
use protocol::control::TargetGeometry;
use std::{
    collections::BTreeMap,
    fs::File,
    io::{Seek, SeekFrom, Write},
    os::{
        fd::AsFd,
        unix::{fs::FileExt, net::UnixStream},
    },
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use wayland_client::{
    Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum, delegate_noop,
    protocol::{wl_buffer, wl_keyboard, wl_output, wl_registry, wl_seat, wl_shm, wl_shm_pool},
};
use wayland_protocols::ext::{
    image_capture_source::v1::client::{
        ext_image_capture_source_v1::ExtImageCaptureSourceV1,
        ext_output_image_capture_source_manager_v1::ExtOutputImageCaptureSourceManagerV1,
    },
    image_copy_capture::v1::client::{
        ext_image_copy_capture_frame_v1::{self, ExtImageCopyCaptureFrameV1},
        ext_image_copy_capture_manager_v1::{ExtImageCopyCaptureManagerV1, Options},
        ext_image_copy_capture_session_v1::{self, ExtImageCopyCaptureSessionV1},
    },
};
use wayland_protocols::xdg::xdg_output::zv1::client::{
    zxdg_output_manager_v1::ZxdgOutputManagerV1,
    zxdg_output_v1::{self, ZxdgOutputV1},
};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
    zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1,
    zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1,
};
use wayland_protocols_wlr::{
    screencopy::v1::client::{
        zwlr_screencopy_frame_v1::{self, ZwlrScreencopyFrameV1},
        zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1,
    },
    virtual_pointer::v1::client::{
        zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1,
        zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1,
    },
};

#[derive(Clone)]
pub struct WaylandDriver {
    socket: PathBuf,
}
impl std::fmt::Debug for WaylandDriver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WaylandDriver").finish_non_exhaustive()
    }
}
impl WaylandDriver {
    pub fn new(socket: PathBuf) -> Self {
        Self { socket }
    }
    /// Keysyms represented in every keyboard group. Passive Wayland clients
    /// cannot observe another client's active group, so do not guess it.
    pub async fn text_keysyms(&self) -> Result<std::collections::BTreeSet<u32>, PlatformError> {
        let path = self.socket.clone();
        blocking(move || {
            let mut client = Client::connect(&path)?;
            // A headless seat may expose no keyboard at all. Asking for one
            // cannot establish a reachable keymap and breaks some compositors.
            if !client.state.has_keyboard {
                return Ok(Default::default());
            }
            let seat =
                client.state.seat.as_ref().ok_or_else(|| {
                    PlatformError::Unsupported("Keyboard keymap unavailable".into())
                })?;
            let _keyboard = seat.get_keyboard(&client.queue.handle(), ());
            client.roundtrip()?;
            crate::text::common_keysyms(
                client.state.observed_keymap.take().ok_or_else(|| {
                    PlatformError::Unsupported("Keyboard keymap unavailable".into())
                })?,
            )
        })
        .await
    }
    /// Read the globals of this explicit socket without capture or input.
    pub async fn advertised_globals(
        &self,
    ) -> Result<std::collections::BTreeSet<String>, PlatformError> {
        let path = self.socket.clone();
        blocking(move || Ok(Client::connect(&path)?.state.globals.clone())).await
    }
}
#[async_trait]
impl Driver for WaylandDriver {
    async fn outputs(&self) -> Result<Vec<Output>, PlatformError> {
        let path = self.socket.clone();
        blocking(move || Client::connect(&path)?.outputs()).await
    }
    async fn capture(&self, output: &Output) -> Result<CapturedFrame, PlatformError> {
        let path = self.socket.clone();
        let output = output.clone();
        blocking(move || Client::connect(&path)?.capture(&output, false)).await
    }
    async fn stream(
        &self,
        output: &Output,
    ) -> Result<Option<Box<dyn NativeCapture>>, PlatformError> {
        let path = self.socket.clone();
        let client = blocking(move || Client::connect(&path)).await?;
        Ok(Some(Box::new(WaylandCapture {
            client: Arc::new(Mutex::new(client)),
            output: output.clone(),
        })))
    }

    async fn input(&self) -> Result<Box<dyn NativeInput>, PlatformError> {
        let path = self.socket.clone();
        let client = blocking(move || {
            let mut c = Client::connect(&path)?;
            c.create_input()?;
            Ok(c)
        })
        .await?;
        Ok(Box::new(WaylandInput {
            client: Arc::new(Mutex::new(client)),
        }))
    }
}
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, PlatformError> + Send + 'static,
) -> Result<T, PlatformError> {
    tokio::task::spawn_blocking(f).await.map_err(failure)?
}
struct WaylandInput {
    client: Arc<Mutex<Client>>,
}
#[async_trait]
impl NativeInput for WaylandInput {
    async fn barrier(&mut self) -> Result<(), PlatformError> {
        // ManagedInput has joined every blocking event call. A display sync on
        // the same connection acknowledges all preceding virtual input requests.
        let client = self.client.clone();
        blocking(move || client.lock().map_err(failure)?.roundtrip()).await
    }
    async fn event(&mut self, event: Event) -> Result<(), PlatformError> {
        self.event_cancellable(event, Arc::new(std::sync::atomic::AtomicBool::new(false)))
            .await
    }
    async fn event_cancellable(
        &mut self,
        event: Event,
        cancelled: Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<(), PlatformError> {
        self.event_fenced(
            event,
            crate::native::InputFence {
                cancelled,
                lock: None,
            },
        )
        .await
    }
    async fn event_fenced(
        &mut self,
        event: Event,
        fence: crate::native::InputFence,
    ) -> Result<(), PlatformError> {
        let client = self.client.clone();
        blocking(move || {
            let mut client = client.lock().map_err(failure)?;
            fence.check(event.is_release())?;
            client.input(event, &fence)
        })
        .await
    }
}

struct Client {
    connection: Connection,
    queue: EventQueue<State>,
    state: State,
    keys: BTreeMap<u32, u32>,
    held: BTreeMap<u32, u32>,
    keymap: Option<File>,
    previous_capture: Option<Output>,
    buttons: std::collections::BTreeSet<u32>,
}
#[derive(Default)]
struct State {
    globals: std::collections::BTreeSet<String>,
    outputs: BTreeMap<u32, (wl_output::WlOutput, Output)>,
    shm: Option<wl_shm::WlShm>,
    seat: Option<wl_seat::WlSeat>,
    capture: Option<ZwlrScreencopyManagerV1>,
    pointer_manager: Option<ZwlrVirtualPointerManagerV1>,
    keyboard_manager: Option<ZwpVirtualKeyboardManagerV1>,
    pointer: Option<ZwlrVirtualPointerV1>,
    keyboard: Option<ZwpVirtualKeyboardV1>,
    buffer: Option<(wl_shm::Format, u32, u32, u32)>,
    ready: bool,
    failed: bool,
    y_invert: bool,
    damage: Vec<DamageRect>,
    sync_done: bool,
    buffer_done: bool,
    ext_manager: Option<ExtImageCopyCaptureManagerV1>,
    ext_source: Option<ExtOutputImageCaptureSourceManagerV1>,
    ext_size: Option<(u32, u32)>,
    ext_format: Option<wl_shm::Format>,
    ext_done: bool,
    ext_transform: Option<wl_output::Transform>,
    xdg_manager: Option<ZxdgOutputManagerV1>,
    logical: BTreeMap<u32, (i32, i32)>,
    observed_keymap: Option<String>,
    has_keyboard: bool,
}
impl Client {
    fn connect(path: &std::path::Path) -> Result<Self, PlatformError> {
        if !path.is_absolute() {
            return Err(PlatformError::Unsupported(
                "an absolute Wayland socket path is required".into(),
            ));
        }
        let connection = Connection::from_socket(UnixStream::connect(path)?).map_err(failure)?;
        let queue = connection.new_event_queue();
        connection.display().get_registry(&queue.handle(), ());
        let mut client = Self {
            connection,
            queue,
            state: State::default(),
            keys: BTreeMap::new(),
            held: BTreeMap::new(),
            keymap: None,
            previous_capture: None,
            buttons: Default::default(),
        };
        client.roundtrip()?;
        if let Some(manager) = &client.state.xdg_manager {
            for (id, (output, _)) in &client.state.outputs {
                manager.get_xdg_output(output, &client.queue.handle(), *id);
            }
        }
        client.roundtrip()?;
        Ok(client)
    }
    fn dispatch(&mut self) -> Result<(), PlatformError> {
        self.dispatch_within(Duration::from_secs(5))
    }
    fn dispatch_within(&mut self, wait: Duration) -> Result<(), PlatformError> {
        self.queue
            .dispatch_pending(&mut self.state)
            .map_err(failure)?;
        if let Some(read) = self.queue.prepare_read() {
            self.connection.flush().map_err(failure)?;
            let mut fds = [rustix::event::PollFd::new(
                &self.connection,
                rustix::event::PollFlags::IN,
            )];
            let timeout = rustix::event::Timespec {
                tv_sec: wait.as_secs() as _,
                tv_nsec: wait.subsec_nanos() as _,
            };
            if rustix::event::poll(&mut fds, Some(&timeout)).map_err(failure)? == 0 {
                return Err(PlatformError::TemporarilyUnavailable(
                    "Wayland compositor did not respond".into(),
                ));
            }
            match read.read() {
                Ok(_) => {}
                Err(wayland_client::backend::WaylandError::Io(error))
                    if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(error) => return Err(failure(error)),
            }
        }
        self.queue
            .dispatch_pending(&mut self.state)
            .map_err(failure)?;
        Ok(())
    }
    fn roundtrip(&mut self) -> Result<(), PlatformError> {
        self.state.sync_done = false;
        self.connection.display().sync(&self.queue.handle(), ());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !self.state.sync_done {
            if std::time::Instant::now() >= deadline {
                return Err(PlatformError::TemporarilyUnavailable(
                    "Wayland synchronization timed out".into(),
                ));
            }
            self.dispatch()?;
        }
        Ok(())
    }
    fn outputs(&mut self) -> Result<Vec<Output>, PlatformError> {
        self.roundtrip()?;
        for (id, (_, output)) in &mut self.state.outputs {
            if let Some((width, _)) = self.state.logical.get(id)
                && *width > 0
            {
                output.scale = output.width as f32 / *width as f32;
            }
        }
        Ok(self
            .state
            .outputs
            .values()
            .map(|(_, o)| o.clone())
            .filter(|o| o.width > 0 && o.height > 0)
            .collect())
    }
    fn capture(
        &mut self,
        output: &Output,
        use_damage: bool,
    ) -> Result<CapturedFrame, PlatformError> {
        self.roundtrip()?;
        let wl_output = self
            .state
            .outputs
            .values()
            .find(|(_, o)| o.name == output.name)
            .map(|(p, _)| p.clone())
            .ok_or(PlatformError::TargetNotFound)?;
        self.state.buffer = None;
        self.state.buffer_done = false;
        self.state.ready = false;
        self.state.failed = false;
        self.state.damage.clear();
        self.state.y_invert = false;
        self.state.ext_transform = None;
        let frame = if let Some(manager) = &self.state.capture {
            CaptureFrame::Wlr(manager.capture_output(1, &wl_output, &self.queue.handle(), ()))
        } else {
            let source = self
                .state
                .ext_source
                .as_ref()
                .ok_or_else(|| {
                    PlatformError::Unsupported("output capture source protocol unavailable".into())
                })?
                .create_source(&wl_output, &self.queue.handle(), ());
            let manager = self.state.ext_manager.as_ref().ok_or_else(|| {
                PlatformError::Unsupported("image copy capture protocol unavailable".into())
            })?;
            self.state.ext_size = None;
            self.state.ext_format = None;
            self.state.ext_done = false;
            let session =
                manager.create_session(&source, Options::PaintCursors, &self.queue.handle(), ());
            source.destroy();
            while !self.state.ext_done && !self.state.failed {
                self.dispatch()?;
            }
            let frame = session.create_frame(&self.queue.handle(), ());
            CaptureFrame::Ext(frame, session)
        };
        while (self.state.buffer.is_none() || (frame.version() >= 3 && !self.state.buffer_done))
            && !self.state.failed
        {
            self.dispatch()?;
        }
        let (format, w, h, stride) = self
            .state
            .buffer
            .ok_or_else(|| PlatformError::TemporarilyUnavailable("capture was refused".into()))?;
        let len = u64::from(stride) * u64::from(h);
        if w == 0 || h == 0 || u64::from(stride) < u64::from(w) * 4 || len > 256 * 1024 * 1024 {
            frame.destroy();
            return Err(PlatformError::Unsupported(
                "unsupported capture buffer size".into(),
            ));
        }
        let pixel_format = match format {
            wl_shm::Format::Argb8888 => PixelFormat::Bgra8,
            wl_shm::Format::Xrgb8888 => PixelFormat::Bgrx8,
            wl_shm::Format::Abgr8888 => PixelFormat::Rgba8,
            wl_shm::Format::Xbgr8888 => PixelFormat::Rgbx8,
            _ => {
                frame.destroy();
                return Err(PlatformError::Unsupported(
                    "unsupported Wayland pixel format".into(),
                ));
            }
        };
        let file = tempfile::tempfile()?;
        file.set_len(len)?;
        let shm = self.state.shm.as_ref().ok_or_else(|| {
            PlatformError::Unsupported("shared memory capture is unavailable".into())
        })?;
        let pool = shm.create_pool(file.as_fd(), len as i32, &self.queue.handle(), ());
        let buffer = pool.create_buffer(
            0,
            w as i32,
            h as i32,
            stride as i32,
            format,
            &self.queue.handle(),
            (),
        );
        frame.copy(&buffer, w, h, use_damage);
        // A still screen sends no damage, so it is copied anyway before the
        // stream's viewers would take the silence for a stalled picture.
        let still_after =
            (use_damage && frame.version() >= 2).then(|| Instant::now() + DAMAGE_WAIT);
        while !self.state.ready && !self.state.failed {
            let waited = match still_after {
                Some(at) => self.dispatch_within(at.saturating_duration_since(Instant::now())),
                None => self.dispatch(),
            };
            if let Err(error) = waited {
                frame.destroy();
                buffer.destroy();
                pool.destroy();
                if use_damage && matches!(error, PlatformError::TemporarilyUnavailable(_)) {
                    return self.capture(output, false);
                }
                return Err(error);
            }
        }
        buffer.destroy();
        pool.destroy();
        frame.destroy();
        self.connection.flush().map_err(failure)?;
        if self.state.failed {
            return Err(PlatformError::TemporarilyUnavailable(
                "desktop capture is unavailable".into(),
            ));
        }
        let mut data = vec![0; len as usize];
        file.read_exact_at(&mut data, 0)?;
        if self.state.y_invert {
            let mut row = vec![0; stride as usize];
            for y in 0..h as usize / 2 {
                let a = y * stride as usize;
                let b = (h as usize - y - 1) * stride as usize;
                row.copy_from_slice(&data[a..a + stride as usize]);
                data.copy_within(b..b + stride as usize, a);
                data[b..b + stride as usize].copy_from_slice(&row);
            }
        }
        let damage =
            if use_damage && self.previous_capture.as_ref() == Some(output) && frame.version() >= 2
            {
                Some(
                    self.state
                        .damage
                        .iter()
                        .map(|r| {
                            let mut r = *r;
                            if self.state.y_invert {
                                r.y = h.saturating_sub(r.y.saturating_add(r.height));
                            }
                            r
                        })
                        .collect(),
                )
            } else {
                None
            };
        self.previous_capture = use_damage.then(|| output.clone());
        let mut captured = CapturedFrame {
            width: w,
            height: h,
            stride,
            format: pixel_format,
            data,
            geometry: TargetGeometry {
                width: w,
                height: h,
                scale: output.scale,
                rotation_degrees: output.rotation,
                revision: 0,
            },
            cursor: None,
            damage,
        };
        if let Some(transform) = self.state.ext_transform {
            captured = orient(captured, transform);
        }
        Ok(captured)
    }
    fn create_input(&mut self) -> Result<(), PlatformError> {
        let pointer =
            self.state.pointer_manager.as_ref().ok_or_else(|| {
                PlatformError::Unsupported("virtual pointer is unavailable".into())
            })?;
        let keyboard =
            self.state.keyboard_manager.as_ref().ok_or_else(|| {
                PlatformError::Unsupported("virtual keyboard is unavailable".into())
            })?;
        let seat = self
            .state
            .seat
            .as_ref()
            .ok_or_else(|| PlatformError::Unsupported("Wayland seat is unavailable".into()))?;
        self.state.pointer =
            Some(pointer.create_virtual_pointer(Some(seat), &self.queue.handle(), ()));
        self.state.keyboard =
            Some(keyboard.create_virtual_keyboard(seat, &self.queue.handle(), ()));
        self.roundtrip()
    }
    fn input(
        &mut self,
        event: Event,
        fence: &crate::native::InputFence,
    ) -> Result<(), PlatformError> {
        let _ = self.outputs()?;
        fence.check(event.is_release())?;
        let pointer =
            self.state.pointer.as_ref().ok_or_else(|| {
                PlatformError::Unsupported("virtual input not initialized".into())
            })?;
        match event {
            Event::Motion { x, y } => {
                let outputs: Vec<_> = self
                    .state
                    .outputs
                    .values()
                    .map(|(_, o)| o.clone())
                    .collect();
                let desktop = crate::combined(&outputs)?;
                let width = (desktop.logical_width() * 256.0).ceil() as u32;
                let height = (desktop.logical_height() * 256.0).ceil() as u32;
                if x < desktop.x
                    || y < desktop.y
                    || x >= desktop.x + f64::from(width) / 256.0
                    || y >= desktop.y + f64::from(height) / 256.0
                {
                    return Err(PlatformError::GeometryChanged);
                }
                pointer.motion_absolute(
                    0,
                    ((x - desktop.x) * 256.0).round() as u32,
                    ((y - desktop.y) * 256.0).round() as u32,
                    width,
                    height,
                );
                pointer.frame();
            }
            Event::Button { code, pressed } => {
                if pressed {
                    self.buttons.insert(code);
                } else {
                    self.buttons.remove(&code);
                }
                pointer.button(
                    0,
                    code,
                    if pressed {
                        wayland_client::protocol::wl_pointer::ButtonState::Pressed
                    } else {
                        wayland_client::protocol::wl_pointer::ButtonState::Released
                    },
                );
                pointer.frame();
            }
            Event::Axis { dx, dy } => {
                use wayland_client::protocol::wl_pointer::{Axis, AxisSource};
                pointer.axis_source(AxisSource::Continuous);
                pointer.axis(0, Axis::HorizontalScroll, dx);
                pointer.axis(0, Axis::VerticalScroll, dy);
                pointer.frame();
            }
            Event::Key { keysym, pressed } => {
                if !self.keys.contains_key(&keysym) {
                    if self.keys.len() >= 240 {
                        if !self.held.is_empty() {
                            return Err(PlatformError::Unsupported(
                                "virtual keymap is full while keys are held".into(),
                            ));
                        }
                        self.keys.clear();
                    }
                    let code = self.keys.len() as u32 + 8;
                    self.keys.insert(keysym, code);
                    self.update_keymap()?;
                }
                // Keymap synchronization may have blocked while revoke arrived.
                fence.check(!pressed)?;
                let code = self.keys[&keysym];
                let keyboard = self
                    .state
                    .keyboard
                    .as_ref()
                    .ok_or_else(|| PlatformError::Unsupported("keyboard unavailable".into()))?;
                keyboard.key(0, code - 8, u32::from(pressed));
                if pressed {
                    self.held.insert(keysym, code);
                } else {
                    self.held.remove(&keysym);
                }
                let mask = self.held.keys().fold(0, |mask, key| {
                    mask | match key {
                        0xffe1 => 1,
                        0xffe3 => 4,
                        0xffe9 => 8,
                        0xffeb => 64,
                        _ => 0,
                    }
                });
                keyboard.modifiers(mask, 0, 0, 0);
            }
        }
        self.roundtrip().map_err(crate::native::uncertain)
    }
    fn update_keymap(&mut self) -> Result<(), PlatformError> {
        let mut map =
            String::from("xkb_keymap { xkb_keycodes \"remote\" { minimum = 8; maximum = 255;");
        for code in self.keys.values() {
            map.push_str(&format!("<K{code}> = {code};"));
        }
        map.push_str("}; xkb_types \"remote\" { type \"ONE_LEVEL\" { modifiers = none; level_name[Level1] = \"Any\"; }; }; xkb_compatibility \"remote\" {}; xkb_symbols \"remote\" {");
        for (symbol, code) in &self.keys {
            map.push_str(&format!(
                "key <K{code}> {{ type=\"ONE_LEVEL\", [ 0x{symbol:x} ] }};"
            ));
        }
        for (symbol, modifier) in [
            (0xffe1, "Shift"),
            (0xffe3, "Control"),
            (0xffe9, "Mod1"),
            (0xffeb, "Mod4"),
        ] {
            if let Some(code) = self.keys.get(&symbol) {
                map.push_str(&format!("modifier_map {modifier} {{ <K{code}> }};"));
            }
        }
        map.push_str("}; };\0");
        let mut file = tempfile::tempfile()?;
        file.write_all(map.as_bytes())?;
        file.seek(SeekFrom::Start(0))?;
        self.state
            .keyboard
            .as_ref()
            .ok_or_else(|| PlatformError::Unsupported("keyboard unavailable".into()))?
            .keymap(1, file.as_fd(), map.len() as u32);
        self.keymap = Some(file);
        // A replacement keymap can reset the compositor's XKB state. Restore
        // depressed modifiers before dispatching the next chord key.
        let mask = self.held.keys().fold(0, |mask, key| {
            mask | match key {
                0xffe1 => 1,
                0xffe3 => 4,
                0xffe9 => 8,
                0xffeb => 64,
                _ => 0,
            }
        });
        if let Some(keyboard) = &self.state.keyboard {
            keyboard.modifiers(mask, 0, 0, 0);
        }
        Ok(())
    }
}
impl Drop for Client {
    fn drop(&mut self) {
        if let Some(keyboard) = &self.state.keyboard {
            for code in self.held.values() {
                keyboard.key(0, *code - 8, 0);
            }
            keyboard.modifiers(0, 0, 0, 0);
            keyboard.destroy();
        }
        if let Some(pointer) = &self.state.pointer {
            for code in &self.buttons {
                pointer.button(
                    0,
                    *code,
                    wayland_client::protocol::wl_pointer::ButtonState::Released,
                );
            }
            pointer.frame();
            pointer.destroy();
        }
        let _ = self.connection.flush();
    }
}
impl Dispatch<wl_registry::WlRegistry, ()> for State {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            wl_registry::Event::Global {
                name,
                interface,
                version,
            } => {
                state.globals.insert(interface.clone());
                match interface.as_str() {
                    "wl_output" => {
                        let output = registry.bind::<wl_output::WlOutput, _, _>(
                            name,
                            version.min(4),
                            qh,
                            name,
                        );
                        state.outputs.insert(
                            name,
                            (
                                output,
                                Output {
                                    name: format!("output-{name}"),
                                    x: 0.0,
                                    y: 0.0,
                                    width: 0,
                                    height: 0,
                                    scale: 1.0,
                                    rotation: 0,
                                },
                            ),
                        );
                    }
                    "ext_image_copy_capture_manager_v1" => {
                        state.ext_manager = Some(registry.bind(name, 1, qh, ()))
                    }
                    "ext_output_image_capture_source_manager_v1" => {
                        state.ext_source = Some(registry.bind(name, 1, qh, ()))
                    }
                    "zxdg_output_manager_v1" => {
                        state.xdg_manager = Some(registry.bind(name, version.min(3), qh, ()))
                    }
                    "wl_shm" => state.shm = Some(registry.bind(name, 1, qh, ())),
                    "wl_seat" => state.seat = Some(registry.bind(name, version.min(7), qh, ())),
                    "zwlr_screencopy_manager_v1" => {
                        state.capture = Some(registry.bind(name, version.min(3), qh, ()))
                    }
                    "zwlr_virtual_pointer_manager_v1" => {
                        state.pointer_manager = Some(registry.bind(name, version.min(2), qh, ()))
                    }
                    "zwp_virtual_keyboard_manager_v1" => {
                        state.keyboard_manager = Some(registry.bind(name, 1, qh, ()))
                    }
                    _ => {}
                }
            }
            wl_registry::Event::GlobalRemove { name } => {
                state.outputs.remove(&name);
            }
            _ => {}
        }
    }
}
impl Dispatch<wl_output::WlOutput, u32> for State {
    fn event(
        state: &mut Self,
        _: &wl_output::WlOutput,
        event: wl_output::Event,
        id: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some((_, output)) = state.outputs.get_mut(id) else {
            return;
        };
        match event {
            wl_output::Event::Geometry {
                x, y, transform, ..
            } => {
                output.x = x.into();
                output.y = y.into();
                output.rotation = match transform {
                    WEnum::Value(wl_output::Transform::_90 | wl_output::Transform::Flipped90) => 90,
                    WEnum::Value(wl_output::Transform::_180 | wl_output::Transform::Flipped180) => {
                        180
                    }
                    WEnum::Value(wl_output::Transform::_270 | wl_output::Transform::Flipped270) => {
                        270
                    }
                    _ => 0,
                };
            }
            wl_output::Event::Mode {
                flags: WEnum::Value(flags),
                width,
                height,
                ..
            } if flags.contains(wl_output::Mode::Current) => {
                if matches!(output.rotation, 90 | 270) {
                    output.width = height.max(0) as u32;
                    output.height = width.max(0) as u32;
                } else {
                    output.width = width.max(0) as u32;
                    output.height = height.max(0) as u32;
                }
            }
            wl_output::Event::Scale { factor } => output.scale = factor.max(1) as f32,
            wl_output::Event::Name { name } => output.name = name,
            _ => {}
        }
    }
}
impl Dispatch<ZwlrScreencopyFrameV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &ZwlrScreencopyFrameV1,
        event: zwlr_screencopy_frame_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_screencopy_frame_v1::Event::Buffer {
                format: WEnum::Value(format),
                width,
                height,
                stride,
            } => state.buffer = Some((format, width, height, stride)),
            zwlr_screencopy_frame_v1::Event::BufferDone => state.buffer_done = true,
            zwlr_screencopy_frame_v1::Event::Ready { .. } => state.ready = true,
            zwlr_screencopy_frame_v1::Event::Failed => state.failed = true,
            zwlr_screencopy_frame_v1::Event::Flags {
                flags: WEnum::Value(flags),
            } => state.y_invert = flags.contains(zwlr_screencopy_frame_v1::Flags::YInvert),
            zwlr_screencopy_frame_v1::Event::Damage {
                x,
                y,
                width,
                height,
            } => state.damage.push(DamageRect {
                x,
                y,
                width,
                height,
            }),
            _ => {}
        }
    }
}
impl Dispatch<wayland_client::protocol::wl_callback::WlCallback, ()> for State {
    fn event(
        state: &mut Self,
        _: &wayland_client::protocol::wl_callback::WlCallback,
        _: wayland_client::protocol::wl_callback::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        state.sync_done = true;
    }
}
delegate_noop!(State: ignore wl_shm::WlShm);
delegate_noop!(State: ignore wl_shm_pool::WlShmPool);
delegate_noop!(State: ignore wl_buffer::WlBuffer);
delegate_noop!(State: ignore ZwlrScreencopyManagerV1);
delegate_noop!(State: ignore ZwlrVirtualPointerManagerV1);
delegate_noop!(State: ignore ZwlrVirtualPointerV1);
delegate_noop!(State: ignore ZwpVirtualKeyboardManagerV1);
delegate_noop!(State: ignore ZwpVirtualKeyboardV1);

enum CaptureFrame {
    Wlr(ZwlrScreencopyFrameV1),
    Ext(ExtImageCopyCaptureFrameV1, ExtImageCopyCaptureSessionV1),
}
impl CaptureFrame {
    fn destroy(&self) {
        match self {
            Self::Wlr(frame) => frame.destroy(),
            Self::Ext(frame, session) => {
                frame.destroy();
                session.destroy();
            }
        }
    }
    fn version(&self) -> u32 {
        match self {
            Self::Wlr(frame) => frame.version(),
            Self::Ext(..) => 0,
        }
    }
    fn copy(&self, buffer: &wl_buffer::WlBuffer, w: u32, h: u32, damage: bool) {
        match self {
            // Polling copies also satisfy still captures on an unchanged desktop.
            Self::Wlr(frame) => {
                if damage && frame.version() >= 2 {
                    frame.copy_with_damage(buffer);
                } else {
                    frame.copy(buffer);
                }
            }
            Self::Ext(frame, _) => {
                frame.attach_buffer(buffer);
                frame.damage_buffer(0, 0, w as i32, h as i32);
                frame.capture();
            }
        }
    }
}
impl Dispatch<ExtImageCopyCaptureSessionV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &ExtImageCopyCaptureSessionV1,
        event: ext_image_copy_capture_session_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            ext_image_copy_capture_session_v1::Event::BufferSize { width, height } => {
                state.ext_size = Some((width, height))
            }
            ext_image_copy_capture_session_v1::Event::ShmFormat {
                format: WEnum::Value(format),
            } if matches!(
                format,
                wl_shm::Format::Argb8888
                    | wl_shm::Format::Xrgb8888
                    | wl_shm::Format::Abgr8888
                    | wl_shm::Format::Xbgr8888
            ) =>
            {
                state.ext_format = Some(format)
            }
            ext_image_copy_capture_session_v1::Event::Done => {
                state.ext_done = true;
                if let (Some((w, h)), Some(format)) = (state.ext_size, state.ext_format) {
                    state.buffer = w.checked_mul(4).map(|stride| (format, w, h, stride));
                }
                if state.buffer.is_none() {
                    state.failed = true;
                }
            }
            ext_image_copy_capture_session_v1::Event::Stopped => state.failed = true,
            _ => {}
        }
    }
}
impl Dispatch<ExtImageCopyCaptureFrameV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &ExtImageCopyCaptureFrameV1,
        event: ext_image_copy_capture_frame_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            ext_image_copy_capture_frame_v1::Event::Ready => state.ready = true,
            ext_image_copy_capture_frame_v1::Event::Failed { .. } => state.failed = true,
            ext_image_copy_capture_frame_v1::Event::Transform {
                transform: WEnum::Value(transform),
            } => state.ext_transform = Some(transform),
            _ => {}
        }
    }
}
impl Dispatch<ZxdgOutputV1, u32> for State {
    fn event(
        state: &mut Self,
        _: &ZxdgOutputV1,
        event: zxdg_output_v1::Event,
        id: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zxdg_output_v1::Event::LogicalPosition { x, y } => {
                if let Some((_, output)) = state.outputs.get_mut(id) {
                    output.x = x.into();
                    output.y = y.into();
                }
            }
            zxdg_output_v1::Event::LogicalSize { width, height } => {
                state.logical.insert(*id, (width, height));
            }
            zxdg_output_v1::Event::Name { name } => {
                if let Some((_, output)) = state.outputs.get_mut(id) {
                    output.name = name;
                }
            }
            _ => {}
        }
    }
}
fn orient(mut frame: CapturedFrame, transform: wl_output::Transform) -> CapturedFrame {
    let value = transform as u32;
    if value == 0 {
        return frame;
    }
    let (w, h) = (frame.width, frame.height);
    let rotated = value & 1 != 0;
    let (width, height) = if rotated { (h, w) } else { (w, h) };
    let mut pixels = vec![0; (width * height * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let (dx, dy) = match value & 3 {
                0 => (x, y),
                1 => (h - 1 - y, x),
                2 => (w - 1 - x, h - 1 - y),
                _ => (y, w - 1 - x),
            };
            let dx = if value & 4 != 0 { width - 1 - dx } else { dx };
            let src = (y * frame.stride + x * 4) as usize;
            let dst = ((dy * width + dx) * 4) as usize;
            pixels[dst..dst + 4].copy_from_slice(&frame.data[src..src + 4]);
        }
    }
    frame.width = width;
    frame.height = height;
    frame.stride = width * 4;
    frame.data = pixels;
    frame.damage = None;
    frame
}
delegate_noop!(State: ignore ExtImageCopyCaptureManagerV1);
delegate_noop!(State: ignore ExtOutputImageCaptureSourceManagerV1);
delegate_noop!(State: ignore ExtImageCaptureSourceV1);
delegate_noop!(State: ignore ZxdgOutputManagerV1);

/// How long a stream waits for the screen to change before copying it
/// unchanged. The media stream reports a still screen once a second, and only
/// when it is given a capture.
const DAMAGE_WAIT: Duration = Duration::from_millis(750);

struct WaylandCapture {
    client: Arc<Mutex<Client>>,
    output: Output,
}
#[async_trait]
impl NativeCapture for WaylandCapture {
    async fn next_frame(&mut self) -> Result<CapturedFrame, PlatformError> {
        let client = self.client.clone();
        let output = self.output.clone();
        blocking(move || client.lock().map_err(failure)?.capture(&output, true)).await
    }
}

impl Dispatch<wl_keyboard::WlKeyboard, ()> for State {
    fn event(
        state: &mut Self,
        _: &wl_keyboard::WlKeyboard,
        event: wl_keyboard::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_keyboard::Event::Keymap {
            format: WEnum::Value(wl_keyboard::KeymapFormat::XkbV1),
            fd,
            size,
        } = event
        {
            state.observed_keymap = read_keymap(&File::from(fd), size).ok();
        }
    }
}

fn read_keymap(file: &File, size: u32) -> std::io::Result<String> {
    if size == 0 || size > 4 * 1024 * 1024 {
        return Err(std::io::Error::from(std::io::ErrorKind::InvalidData));
    }
    let mut bytes = vec![0; size as usize];
    // Compositors may reuse an open file description across keyboard clients.
    // Positional reads neither depend on nor change its shared cursor.
    file.read_exact_at(&mut bytes, 0)?;
    let text = String::from_utf8(bytes)
        .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidData))?;
    Ok(text.trim_end_matches('\0').to_owned())
}

impl Dispatch<wl_seat::WlSeat, ()> for State {
    fn event(
        state: &mut Self,
        _: &wl_seat::WlSeat,
        event: wl_seat::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities { capabilities } = event {
            state.has_keyboard = matches!(capabilities, WEnum::Value(value) if value.contains(wl_seat::Capability::Keyboard));
        }
    }
}

#[cfg(test)]
mod keymap_tests {
    use super::read_keymap;
    use std::io::{Seek, Write};

    #[test]
    fn repeated_keymap_reads_ignore_and_preserve_the_shared_cursor() {
        let mut file = tempfile::tempfile().unwrap();
        let bytes = b"xkb_keymap { fixture };\0";
        file.write_all(bytes).unwrap();
        let duplicate = file.try_clone().unwrap();
        for _ in 0..3 {
            assert_eq!(
                read_keymap(&duplicate, bytes.len() as u32).unwrap(),
                "xkb_keymap { fixture };"
            );
            assert_eq!(file.stream_position().unwrap(), bytes.len() as u64);
        }
    }

    #[test]
    fn incomplete_invalid_and_unbounded_keymaps_are_rejected() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(&[0xff]).unwrap();
        for size in [0, 1, 2, 4 * 1024 * 1024 + 1] {
            assert!(read_keymap(&file, size).is_err());
        }
    }
}
