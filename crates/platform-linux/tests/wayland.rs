// SPDX-License-Identifier: Apache-2.0
#![cfg(target_os = "linux")]
use platform_linux::{Environment, LinuxDesktop, wayland::WaylandDriver};
use ports::desktop::DesktopBackend;
use protocol::control::{InputAction, PointerButton, TargetPoint};
use std::{
    fs::File,
    io::{Read, Write},
    os::unix::{fs::FileExt, net::UnixListener},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU8, Ordering},
    },
    thread,
    time::Duration,
};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::server::{
    zwp_virtual_keyboard_manager_v1::{self, ZwpVirtualKeyboardManagerV1},
    zwp_virtual_keyboard_v1::{self, ZwpVirtualKeyboardV1},
};
use wayland_protocols_wlr::{
    screencopy::v1::server::{
        zwlr_screencopy_frame_v1::{self, ZwlrScreencopyFrameV1},
        zwlr_screencopy_manager_v1::{self, ZwlrScreencopyManagerV1},
    },
    virtual_pointer::v1::server::{
        zwlr_virtual_pointer_manager_v1::{self, ZwlrVirtualPointerManagerV1},
        zwlr_virtual_pointer_v1::{self, ZwlrVirtualPointerV1},
    },
};
use wayland_server::{
    Client, DataInit, Dispatch, Display, DisplayHandle, GlobalDispatch, New, Resource,
    protocol::{wl_buffer, wl_keyboard, wl_output, wl_seat, wl_shm, wl_shm_pool},
};
#[derive(Default)]
struct Recorded {
    motion: Vec<(u32, u32, u32, u32)>,
    buttons: Vec<(u32, u32)>,
    keys: Vec<(u32, u32)>,
    keymaps: Vec<String>,
    captures: Vec<u8>,
    /// Virtual keyboards and pointers created and destroyed.
    keyboards: (usize, usize),
    pointers: (usize, usize),
    modifiers: Vec<u32>,
    /// Keymaps (as text) and keys (evdev code, state), in arrival order.
    log: Vec<Logged>,
    /// Virtual keyboards that have been given a keymap.
    keymapped: std::collections::HashSet<wayland_server::backend::ObjectId>,
}
#[derive(Debug, Clone)]
enum Logged {
    Keymap(String),
    Key { pressed: bool },
}
struct State {
    has_keyboard: bool,
    /// The seat keyboard's layout, sent to every wl_keyboard.
    layout: String,
    events: Arc<Mutex<Recorded>>,
    tone: Arc<AtomicU8>,
    still: Arc<AtomicBool>,
}
struct Server {
    path: std::path::PathBuf,
    _directory: tempfile::TempDir,
    stop: Arc<AtomicBool>,
    tone: Arc<AtomicU8>,
    /// Nothing on screen changes, so no damage is ever sent.
    still: Arc<AtomicBool>,
    events: Arc<Mutex<Recorded>>,
    hotplug: std::sync::mpsc::Sender<()>,
    thread: Option<thread::JoinHandle<()>>,
}
impl Server {
    fn new() -> Self {
        Self::with_ext(false)
    }
    fn with_ext(ext: bool) -> Self {
        Self::with_options(ext, true)
    }
    fn with_options(ext: bool, has_keyboard: bool) -> Self {
        Self::with_layout(ext, has_keyboard, "us")
    }
    fn with_layout(ext: bool, has_keyboard: bool, layout: &str) -> Self {
        let context = xkbcommon::xkb::Context::new(xkbcommon::xkb::CONTEXT_NO_FLAGS);
        let layout =
            xkbcommon::xkb::Keymap::new_from_names(&context, "evdev", "pc105", layout, "", None, 0)
                .unwrap()
                .get_as_string(xkbcommon::xkb::KEYMAP_FORMAT_TEXT_V1);
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("wayland-test");
        let listener = UnixListener::bind(&path).unwrap();
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let tone = Arc::new(AtomicU8::new(30));
        let still = Arc::new(AtomicBool::new(false));
        let events = Arc::new(Mutex::new(Recorded::default()));
        let done = stop.clone();
        let mut state = State {
            has_keyboard,
            layout,
            events: events.clone(),
            tone: tone.clone(),
            still: still.clone(),
        };
        let (hotplug, rx) = std::sync::mpsc::channel();
        let thread = thread::spawn(move || {
            let mut display = Display::<State>::new().unwrap();
            let handle = display.handle();
            handle.create_global::<State, wl_output::WlOutput, _>(4, 0u32);
            handle.create_global::<State, wl_shm::WlShm, _>(1, ());
            handle.create_global::<State, wl_seat::WlSeat, _>(7, ());
            if ext {
                handle.create_global::<State, ExtOutputImageCaptureSourceManagerV1, _>(1, ());
                handle.create_global::<State, ExtImageCopyCaptureManagerV1, _>(1, ());
            } else {
                handle.create_global::<State, ZwlrScreencopyManagerV1, _>(3, ());
            }
            handle.create_global::<State, ZwlrVirtualPointerManagerV1, _>(2, ());
            handle.create_global::<State, ZwpVirtualKeyboardManagerV1, _>(1, ());
            handle.create_global::<State, ZxdgOutputManagerV1, _>(3, ());
            while !done.load(Ordering::Relaxed) {
                while let Ok((socket, _)) = listener.accept() {
                    display
                        .handle()
                        .insert_client(socket, Arc::new(()))
                        .unwrap();
                }
                if rx.try_recv().is_ok() {
                    handle.create_global::<State, wl_output::WlOutput, _>(4, 1u32);
                }
                display.dispatch_clients(&mut state).unwrap();
                display.flush_clients().unwrap();
                thread::sleep(Duration::from_millis(1));
            }
        });
        Self {
            path,
            _directory: directory,
            stop,
            tone,
            still,
            events,
            hotplug,
            thread: Some(thread),
        }
    }
    fn desktop(&self) -> LinuxDesktop {
        let env = Environment {
            session_type: "wayland".into(),
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
            ..Default::default()
        };
        LinuxDesktop::new(
            Arc::new(WaylandDriver::new(self.path.clone())),
            LinuxDesktop::detect(&env).capabilities,
            None,
        )
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.thread.take().unwrap().join().unwrap();
    }
}
impl GlobalDispatch<wl_output::WlOutput, u32> for State {
    fn bind(
        _: &mut Self,
        _: &DisplayHandle,
        _: &Client,
        resource: New<wl_output::WlOutput>,
        id: &u32,
        data: &mut DataInit<'_, Self>,
    ) {
        let output = data.init(resource, *id);
        output.geometry(
            *id as i32 * 32,
            0,
            300,
            200,
            wl_output::Subpixel::Unknown,
            "Test".into(),
            "Synthetic".into(),
            wl_output::Transform::Normal,
        );
        output.mode(
            wl_output::Mode::Current | wl_output::Mode::Preferred,
            64,
            32,
            60000,
        );
        output.scale(2);
        output.name(format!("TEST-{id}"));
        output.done();
    }
}
impl GlobalDispatch<wl_shm::WlShm, ()> for State {
    fn bind(
        _: &mut Self,
        _: &DisplayHandle,
        _: &Client,
        resource: New<wl_shm::WlShm>,
        _: &(),
        data: &mut DataInit<'_, Self>,
    ) {
        let shm = data.init(resource, ());
        shm.format(wl_shm::Format::Xrgb8888);
    }
}
impl GlobalDispatch<wl_seat::WlSeat, ()> for State {
    fn bind(
        state: &mut Self,
        _: &DisplayHandle,
        _: &Client,
        resource: New<wl_seat::WlSeat>,
        _: &(),
        data: &mut DataInit<'_, Self>,
    ) {
        let seat = data.init(resource, ());
        let mut capabilities = wl_seat::Capability::Pointer;
        if state.has_keyboard {
            capabilities |= wl_seat::Capability::Keyboard;
        }
        seat.capabilities(capabilities);
        seat.name("test-seat".into());
    }
}
macro_rules! global {
    ($ty:ty) => {
        impl GlobalDispatch<$ty, ()> for State {
            fn bind(
                _: &mut Self,
                _: &DisplayHandle,
                _: &Client,
                resource: New<$ty>,
                _: &(),
                data: &mut DataInit<'_, Self>,
            ) {
                data.init(resource, ());
            }
        }
    };
}
global!(ZwlrScreencopyManagerV1);
global!(ZwlrVirtualPointerManagerV1);
global!(ZwpVirtualKeyboardManagerV1);
struct Pool(Arc<File>);
struct Buffer {
    file: Arc<File>,
    offset: u64,
    stride: usize,
    height: usize,
}
impl Dispatch<wl_shm::WlShm, ()> for State {
    fn request(
        _: &mut Self,
        _: &Client,
        _: &wl_shm::WlShm,
        request: wl_shm::Request,
        _: &(),
        _: &DisplayHandle,
        data: &mut DataInit<'_, Self>,
    ) {
        if let wl_shm::Request::CreatePool { id, fd, .. } = request {
            data.init(id, Pool(Arc::new(File::from(fd))));
        }
    }
}
impl Dispatch<wl_shm_pool::WlShmPool, Pool> for State {
    fn request(
        _: &mut Self,
        _: &Client,
        _: &wl_shm_pool::WlShmPool,
        request: wl_shm_pool::Request,
        pool: &Pool,
        _: &DisplayHandle,
        data: &mut DataInit<'_, Self>,
    ) {
        if let wl_shm_pool::Request::CreateBuffer {
            id,
            offset,
            stride,
            height,
            ..
        } = request
        {
            data.init(
                id,
                Buffer {
                    file: pool.0.clone(),
                    offset: offset as u64,
                    stride: stride as usize,
                    height: height as usize,
                },
            );
        }
    }
}
impl Dispatch<ZwlrScreencopyManagerV1, ()> for State {
    fn request(
        _: &mut Self,
        _: &Client,
        _: &ZwlrScreencopyManagerV1,
        request: zwlr_screencopy_manager_v1::Request,
        _: &(),
        _: &DisplayHandle,
        data: &mut DataInit<'_, Self>,
    ) {
        if let zwlr_screencopy_manager_v1::Request::CaptureOutput { frame, .. } = request {
            let frame = data.init(frame, ());
            frame.buffer(wl_shm::Format::Xrgb8888, 64, 32, 256);
            frame.buffer_done();
        }
    }
}
impl Dispatch<ZwlrScreencopyFrameV1, ()> for State {
    fn request(
        state: &mut Self,
        _: &Client,
        frame: &ZwlrScreencopyFrameV1,
        request: zwlr_screencopy_frame_v1::Request,
        _: &(),
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
        let buffer = match request {
            zwlr_screencopy_frame_v1::Request::Copy { buffer } => buffer,
            zwlr_screencopy_frame_v1::Request::CopyWithDamage { buffer }
                if !state.still.load(Ordering::Relaxed) =>
            {
                buffer
            }
            _ => return,
        };
        let data = buffer.data::<Buffer>().unwrap();
        let tone = state.tone.load(Ordering::Relaxed);
        state.events.lock().unwrap().captures.push(tone);
        let pixels = [10, 20, tone, 255].repeat(data.stride * data.height / 4);
        data.file.write_all_at(&pixels, data.offset).unwrap();
        frame.flags(zwlr_screencopy_frame_v1::Flags::empty());
        frame.damage(0, 0, 64, 32);
        frame.ready(0, 1, 0);
        buffer.release();
    }
}
impl Dispatch<ZwlrVirtualPointerManagerV1, ()> for State {
    fn request(
        _state: &mut Self,
        _: &Client,
        _: &ZwlrVirtualPointerManagerV1,
        request: zwlr_virtual_pointer_manager_v1::Request,
        _: &(),
        _: &DisplayHandle,
        data: &mut DataInit<'_, Self>,
    ) {
        if let zwlr_virtual_pointer_manager_v1::Request::CreateVirtualPointer { id, .. } = request {
            data.init(id, ());
            _state.events.lock().unwrap().pointers.0 += 1;
        }
    }
}
impl Dispatch<ZwlrVirtualPointerV1, ()> for State {
    fn request(
        state: &mut Self,
        _: &Client,
        _: &ZwlrVirtualPointerV1,
        request: zwlr_virtual_pointer_v1::Request,
        _: &(),
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
        let mut events = state.events.lock().unwrap();
        match request {
            zwlr_virtual_pointer_v1::Request::MotionAbsolute {
                x,
                y,
                x_extent,
                y_extent,
                ..
            } => events.motion.push((x, y, x_extent, y_extent)),
            zwlr_virtual_pointer_v1::Request::Button { button, state, .. } => {
                events.buttons.push((button, state.into()))
            }
            zwlr_virtual_pointer_v1::Request::Destroy => events.pointers.1 += 1,
            _ => {}
        }
    }
}
impl Dispatch<ZwpVirtualKeyboardManagerV1, ()> for State {
    fn request(
        _state: &mut Self,
        _: &Client,
        _: &ZwpVirtualKeyboardManagerV1,
        request: zwp_virtual_keyboard_manager_v1::Request,
        _: &(),
        _: &DisplayHandle,
        data: &mut DataInit<'_, Self>,
    ) {
        if let zwp_virtual_keyboard_manager_v1::Request::CreateVirtualKeyboard { id, .. } = request
        {
            data.init(id, ());
            _state.events.lock().unwrap().keyboards.0 += 1;
        }
    }
}
impl Dispatch<ZwpVirtualKeyboardV1, ()> for State {
    fn request(
        state: &mut Self,
        _: &Client,
        keyboard: &ZwpVirtualKeyboardV1,
        request: zwp_virtual_keyboard_v1::Request,
        _: &(),
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
        let mut events = state.events.lock().unwrap();
        // As wlroots does: keys or modifiers before a keymap end the client.
        if matches!(
            request,
            zwp_virtual_keyboard_v1::Request::Key { .. }
                | zwp_virtual_keyboard_v1::Request::Modifiers { .. }
        ) && !events.keymapped.contains(&keyboard.id())
        {
            keyboard.post_error(zwp_virtual_keyboard_v1::Error::NoKeymap, "no keymap");
            return;
        }
        match request {
            zwp_virtual_keyboard_v1::Request::Key { key, state, .. } => {
                events.keys.push((key, state));
                events.log.push(Logged::Key {
                    pressed: state == 1,
                });
            }
            zwp_virtual_keyboard_v1::Request::Keymap { fd, size, .. } => {
                let mut text = String::new();
                File::from(fd)
                    .take(size.into())
                    .read_to_string(&mut text)
                    .unwrap();
                events.keymapped.insert(keyboard.id());
                events.log.push(Logged::Keymap(text.clone()));
                events.keymaps.push(text);
            }
            zwp_virtual_keyboard_v1::Request::Modifiers { mods_depressed, .. } => {
                events.modifiers.push(mods_depressed)
            }
            zwp_virtual_keyboard_v1::Request::Destroy => events.keyboards.1 += 1,
            _ => {}
        }
    }
}
macro_rules! noop {
    ($interface:ty,$data:ty) => {
        impl Dispatch<$interface, $data> for State {
            fn request(
                _: &mut Self,
                _: &Client,
                _: &$interface,
                _: <$interface as Resource>::Request,
                _: &$data,
                _: &DisplayHandle,
                _: &mut DataInit<'_, Self>,
            ) {
            }
        }
    };
}
noop!(wl_output::WlOutput, u32);
noop!(wl_keyboard::WlKeyboard, ());
impl Dispatch<wl_seat::WlSeat, ()> for State {
    fn request(
        state: &mut Self,
        _: &Client,
        _: &wl_seat::WlSeat,
        request: wl_seat::Request,
        _: &(),
        _: &DisplayHandle,
        data: &mut DataInit<'_, Self>,
    ) {
        if let wl_seat::Request::GetKeyboard { id } = request {
            assert!(state.has_keyboard, "get_keyboard on a seat without one");
            let keyboard = data.init(id, ());
            let mut file = tempfile::tempfile_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
            file.write_all(state.layout.as_bytes()).unwrap();
            file.write_all(b"\0").unwrap();
            keyboard.keymap(
                wl_keyboard::KeymapFormat::XkbV1,
                std::os::fd::AsFd::as_fd(&file),
                state.layout.len() as u32 + 1,
            );
        }
    }
}
noop!(wl_buffer::WlBuffer, Buffer);

#[tokio::test]
async fn real_protocol_pixels_damage_input_and_hotplug() {
    let server = Server::new();
    let desktop = server.desktop();
    let targets = desktop.targets().await.unwrap();
    assert_eq!(targets.len(), 1);
    let target = &targets[0];
    assert_eq!(
        (
            target.geometry.width,
            target.geometry.height,
            target.geometry.scale
        ),
        (64, 32, 2.0)
    );
    let frame = desktop.capture_still(target.target_id).await.unwrap();
    assert!(
        frame
            .data
            .as_chunks::<4>()
            .0
            .iter()
            .all(|p| *p == [10, 20, 30, 255])
    );
    let mut capture = desktop.open_stream(target.target_id, 120).await.unwrap();
    let _ = capture.next_frame().await.unwrap();
    let frame = capture.next_frame().await.unwrap().unwrap();
    assert_eq!(frame.damage.unwrap()[0].width, 64);
    drop(capture);
    let mut input = desktop.input_sink(target.target_id).await.unwrap();
    let point = TargetPoint { x: 1.0, y: 1.0 };
    input
        .inject(
            &InputAction::PointerButton {
                button: PointerButton::Left,
                pressed: true,
                at: point,
            },
            &target.geometry,
        )
        .await
        .unwrap();
    input
        .inject(
            &InputAction::KeyPress {
                key: "ctrl".into(),
                pressed: true,
            },
            &target.geometry,
        )
        .await
        .unwrap();
    input.release_all().await.unwrap();
    input
        .inject(
            &InputAction::Text {
                text: "λ🦀".into()
            },
            &target.geometry,
        )
        .await
        .unwrap();
    {
        let recorded = server.events.lock().unwrap();
        // The last pixel, 1/256 px inside it.
        assert_eq!(recorded.motion[0], (8065, 3969, 8192, 4096));
        assert_eq!(recorded.buttons, [(0x110, 1), (0x110, 0)]);
        assert_eq!(recorded.keys.len(), 6);
        let context = xkbcommon::xkb::Context::new(xkbcommon::xkb::CONTEXT_NO_FLAGS);
        for text in &recorded.keymaps {
            let keymap = xkbcommon::xkb::Keymap::new_from_string(
                &context,
                text.trim_end_matches('\0').to_owned(),
                xkbcommon::xkb::KEYMAP_FORMAT_TEXT_V1,
                xkbcommon::xkb::KEYMAP_COMPILE_NO_FLAGS,
            );
            assert!(keymap.is_some(), "generated keymap must compile");
        }

        assert!(recorded.keymaps.iter().any(|map| map.contains("0x10003bb")));
        assert!(recorded.keymaps.iter().any(|map| map.contains("0x101f980")));
    }
    server.hotplug.send(()).unwrap();
    let updated = desktop.targets().await.unwrap();
    assert_eq!(updated.len(), 3);
    assert!(updated[0].geometry.revision > target.geometry.revision);
    assert!(matches!(
        input
            .inject(&InputAction::PointerMove { to: point }, &target.geometry)
            .await,
        Err(ports::desktop::PlatformError::GeometryChanged)
    ));
}

#[tokio::test]
async fn slow_consumer_gets_latest_frame() {
    let server = Server::new();
    let desktop = server.desktop();
    let target = desktop.targets().await.unwrap()[0].target_id;
    let mut stream = desktop.open_stream(target, 30).await.unwrap();
    let _ = stream.next_frame().await.unwrap().unwrap();
    server.tone.store(100, Ordering::Relaxed);
    wait_for_captures(&server, 100, 2).await;
    server.tone.store(200, Ordering::Relaxed);
    wait_for_captures(&server, 200, 2).await;
    let frame = stream.next_frame().await.unwrap().unwrap();
    assert_eq!(frame.data[2], 200);
    assert!(frame.damage.is_none());
}

#[tokio::test]
async fn a_still_screen_is_still_captured() {
    let server = Server::new();
    server.still.store(true, Ordering::Relaxed);
    let desktop = server.desktop();
    let target = desktop.targets().await.unwrap()[0].target_id;
    let mut stream = desktop.open_stream(target, 30).await.unwrap();
    let _ = stream.next_frame().await.unwrap().unwrap();
    // A viewer takes three seconds without a picture for a stalled one and
    // stops an agent's input, so a still screen keeps arriving well inside it.
    for _ in 0..2 {
        let frame = tokio::time::timeout(Duration::from_millis(1500), stream.next_frame())
            .await
            .expect("a still screen stopped arriving")
            .unwrap()
            .unwrap();
        assert!(frame.damage.is_none());
    }
}

use wayland_protocols::ext::{
    image_capture_source::v1::server::{
        ext_image_capture_source_v1::ExtImageCaptureSourceV1,
        ext_output_image_capture_source_manager_v1::{self, ExtOutputImageCaptureSourceManagerV1},
    },
    image_copy_capture::v1::server::{
        ext_image_copy_capture_frame_v1::{self, ExtImageCopyCaptureFrameV1},
        ext_image_copy_capture_manager_v1::{self, ExtImageCopyCaptureManagerV1},
        ext_image_copy_capture_session_v1::{self, ExtImageCopyCaptureSessionV1},
    },
};
global!(ExtImageCopyCaptureManagerV1);
global!(ExtOutputImageCaptureSourceManagerV1);
noop!(ExtImageCaptureSourceV1, ());
impl Dispatch<ExtOutputImageCaptureSourceManagerV1, ()> for State {
    fn request(
        _: &mut Self,
        _: &Client,
        _: &ExtOutputImageCaptureSourceManagerV1,
        request: ext_output_image_capture_source_manager_v1::Request,
        _: &(),
        _: &DisplayHandle,
        data: &mut DataInit<'_, Self>,
    ) {
        if let ext_output_image_capture_source_manager_v1::Request::CreateSource {
            source, ..
        } = request
        {
            data.init(source, ());
        }
    }
}
impl Dispatch<ExtImageCopyCaptureManagerV1, ()> for State {
    fn request(
        _: &mut Self,
        _: &Client,
        _: &ExtImageCopyCaptureManagerV1,
        request: ext_image_copy_capture_manager_v1::Request,
        _: &(),
        _: &DisplayHandle,
        data: &mut DataInit<'_, Self>,
    ) {
        if let ext_image_copy_capture_manager_v1::Request::CreateSession { session, .. } = request {
            let session = data.init(session, ());
            session.buffer_size(64, 32);
            session.shm_format(wl_shm::Format::Xrgb8888);
            session.done();
        }
    }
}
impl Dispatch<ExtImageCopyCaptureSessionV1, ()> for State {
    fn request(
        _: &mut Self,
        _: &Client,
        _: &ExtImageCopyCaptureSessionV1,
        request: ext_image_copy_capture_session_v1::Request,
        _: &(),
        _: &DisplayHandle,
        data: &mut DataInit<'_, Self>,
    ) {
        if let ext_image_copy_capture_session_v1::Request::CreateFrame { frame } = request {
            data.init(frame, Mutex::<Option<wl_buffer::WlBuffer>>::new(None));
        }
    }
}
impl Dispatch<ExtImageCopyCaptureFrameV1, Mutex<Option<wl_buffer::WlBuffer>>> for State {
    fn request(
        state: &mut Self,
        _: &Client,
        frame: &ExtImageCopyCaptureFrameV1,
        request: ext_image_copy_capture_frame_v1::Request,
        buffer: &Mutex<Option<wl_buffer::WlBuffer>>,
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
        match request {
            ext_image_copy_capture_frame_v1::Request::AttachBuffer { buffer: new } => {
                *buffer.lock().unwrap() = Some(new);
            }
            ext_image_copy_capture_frame_v1::Request::Capture => {
                let buffer = buffer.lock().unwrap();
                let buffer = buffer.as_ref().unwrap();
                let data = buffer.data::<Buffer>().unwrap();
                data.file
                    .write_all_at(
                        &[10, 20, state.tone.load(Ordering::Relaxed), 255]
                            .repeat(data.stride * data.height / 4),
                        data.offset,
                    )
                    .unwrap();
                frame.transform(wl_output::Transform::Normal);
                frame.damage(0, 0, 64, 32);
                frame.ready();
            }
            _ => {}
        }
    }
}
#[tokio::test]
async fn ext_image_copy_only_compositor() {
    let server = Server::with_ext(true);
    let desktop = server.desktop();
    let target = desktop.targets().await.unwrap()[0].target_id;
    let frame = desktop.capture_still(target).await.unwrap();
    assert!(
        frame
            .data
            .as_chunks::<4>()
            .0
            .iter()
            .all(|p| *p == [10, 20, 30, 255])
    );
    let mut stream = desktop.open_stream(target, 20).await.unwrap();
    let first = stream.next_frame().await.unwrap().unwrap();
    assert_eq!(first.width, 64);
}

async fn wait_for_captures(server: &Server, tone: u8, count: usize) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if server
                .events
                .lock()
                .unwrap()
                .captures
                .iter()
                .filter(|value| **value == tone)
                .count()
                >= count
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("fake compositor did not capture the requested content");
}

#[tokio::test]
async fn a_headless_seat_without_a_keyboard_never_receives_get_keyboard() {
    let server = Server::with_options(false, false);
    let driver = WaylandDriver::new(server.path.clone());
    assert!(driver.text_keysyms().await.unwrap().is_empty());
    assert!(server.events.lock().unwrap().keys.is_empty());
}

async fn inject(
    sink: &mut Box<dyn ports::desktop::InputSink>,
    target: &protocol::control::ControlTarget,
    action: InputAction,
) {
    sink.inject(&action, &target.geometry).await.unwrap();
}

#[tokio::test]
async fn keys_arrive_on_the_seat_layouts_own_keycodes_and_devices_leave_with_control() {
    // The seat's keyboard is German: z and y trade places there.
    let server = Server::with_layout(false, true, "de");
    let desktop = server.desktop();
    let target = desktop.targets().await.unwrap().remove(0);
    let mut sink = desktop.input_sink(target.target_id).await.unwrap();
    // Opening the sink adds nothing to the desktop yet.
    assert_eq!(server.events.lock().unwrap().keyboards, (0, 0));
    let chord = |keys: &[&str]| InputAction::KeyChord {
        keys: keys.iter().map(|k| k.to_string()).collect(),
    };
    inject(&mut sink, &target, chord(&["super", "Enter"])).await;
    inject(&mut sink, &target, chord(&["ctrl", "z"])).await;
    inject(&mut sink, &target, InputAction::Text { text: "y".into() }).await;
    {
        let recorded = server.events.lock().unwrap();
        // evdev codes: Super 125 and Enter 28, never 0 or Escape's 1.
        assert_eq!(recorded.keys[..4], [(125, 1), (28, 1), (28, 0), (125, 0)]);
        // Ctrl 29, and z on the German layout's own key, evdev 21.
        assert_eq!(recorded.keys[4..8], [(29, 1), (21, 1), (21, 0), (29, 0)]);
        assert_eq!(recorded.keys[8..], [(44, 1), (44, 0)]);
        // Super held reaches the compositor as Mod4 before Enter is pressed.
        assert_eq!(recorded.modifiers[1], 1 << 6);
        assert_eq!(*recorded.modifiers.last().unwrap(), 0);
        assert_eq!(recorded.keymaps.len(), 1, "one keymap for the session");
        assert!(
            recorded.keymaps[0].contains("German"),
            "the seat layout is uploaded"
        );
        assert_eq!(recorded.keyboards, (1, 0));
    }
    // Control ends: the virtual keyboard leaves the seat.
    sink.release_all().await.unwrap();
    assert_eq!(server.events.lock().unwrap().keyboards, (1, 1));
    inject(
        &mut sink,
        &target,
        InputAction::PointerMove {
            to: TargetPoint { x: 0.5, y: 0.5 },
        },
    )
    .await;
    inject(&mut sink, &target, chord(&["Escape"])).await;
    {
        let recorded = server.events.lock().unwrap();
        assert_eq!(recorded.keyboards, (2, 1));
        assert_eq!(recorded.pointers, (1, 0));
        assert_eq!(recorded.keys[recorded.keys.len() - 2..], [(1, 1), (1, 0)]);
    }
    drop(sink);
    for _ in 0..200 {
        if server.events.lock().unwrap().pointers.1 == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let recorded = server.events.lock().unwrap();
    assert_eq!((recorded.keyboards, recorded.pointers), ((2, 2), (1, 1)));
}

/// A keymap that could not be written must leave no keyboard without one,
/// and no model that thinks the compositor has a keymap it never got. Runs
/// in a child process, which alone may change where temporary files go.
#[test]
fn a_keymap_that_cannot_be_written_leaves_input_working() {
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "keymap_write_failure_child",
            "--ignored",
            "--test-threads=1",
        ])
        .env("OPHIO_KEYMAP_FAILURE_CHILD", "1")
        .stdout(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(status.success());
}
#[tokio::test]
#[ignore = "run by a_keymap_that_cannot_be_written_leaves_input_working"]
async fn keymap_write_failure_child() {
    if std::env::var_os("OPHIO_KEYMAP_FAILURE_CHILD").is_none() {
        return;
    }
    #[allow(unsafe_code)]
    fn temporary_files(working: bool) {
        let dir = if working {
            env!("CARGO_TARGET_TMPDIR")
        } else {
            "/nonexistent/ophio-test"
        };
        // SAFETY: this process runs this one test on one thread.
        unsafe { std::env::set_var("TMPDIR", dir) };
    }
    let server = Server::new();
    let desktop = server.desktop();
    let target = desktop.targets().await.unwrap().remove(0);
    let mut sink = desktop.input_sink(target.target_id).await.unwrap();
    let chord = InputAction::KeyChord {
        keys: vec!["ctrl".into()],
    };
    // The first keyboard's keymap cannot be written.
    temporary_files(false);
    assert!(sink.inject(&chord, &target.geometry).await.is_err());
    temporary_files(true);
    inject(&mut sink, &target, chord).await;
    // A new character's keymap cannot be written either.
    temporary_files(false);
    let text = InputAction::Text { text: "é".into() };
    assert!(sink.inject(&text, &target.geometry).await.is_err());
    temporary_files(true);
    inject(&mut sink, &target, text).await;
    let log = server.events.lock().unwrap().log.clone();
    // The é key went down after a keymap that has é.
    let pressed = log
        .iter()
        .rposition(|e| matches!(e, Logged::Key { pressed: true }))
        .unwrap();
    let keymap = log[..pressed]
        .iter()
        .rev()
        .find_map(|e| match e {
            Logged::Keymap(text) => Some(text),
            _ => None,
        })
        .unwrap();
    assert!(
        keymap.contains("0xe9"),
        "é's key was pressed without its keymap"
    );
}

use wayland_protocols::xdg::xdg_output::zv1::server::{
    zxdg_output_manager_v1::{self, ZxdgOutputManagerV1},
    zxdg_output_v1::ZxdgOutputV1,
};
global!(ZxdgOutputManagerV1);
noop!(ZxdgOutputV1, ());
impl Dispatch<ZxdgOutputManagerV1, ()> for State {
    fn request(
        _: &mut Self,
        _: &Client,
        _: &ZxdgOutputManagerV1,
        request: zxdg_output_manager_v1::Request,
        _: &(),
        _: &DisplayHandle,
        data: &mut DataInit<'_, Self>,
    ) {
        if let zxdg_output_manager_v1::Request::GetXdgOutput { id, output } = request {
            let id_of_output = *output.data::<u32>().unwrap();
            let xdg = data.init(id, ());
            // Output 0 is 64x32 at scale 2; the hotplugged output 1 has a
            // fractional scale of 1.6, which only xdg-output describes.
            let (x, width, height) = if id_of_output == 0 {
                (0, 32, 16)
            } else {
                (32, 40, 20)
            };
            xdg.logical_position(x, 0);
            xdg.logical_size(width, height);
        }
    }
}

#[tokio::test]
async fn pointer_motion_follows_a_fractionally_scaled_monitor_plugged_in_during_control() {
    let server = Server::new();
    let desktop = server.desktop();
    let target = desktop.targets().await.unwrap().remove(0);
    let mut sink = desktop.input_sink(target.target_id).await.unwrap();
    let corner = TargetPoint { x: 1.0, y: 1.0 };
    inject(&mut sink, &target, InputAction::PointerMove { to: corner }).await;
    server.hotplug.send(()).unwrap();
    // The phone picks the target again once its geometry revision moves on.
    let target = loop {
        let targets = desktop.targets().await.unwrap();
        if targets.len() == 3 {
            break targets.into_iter().find(|t| t.label == "TEST-0").unwrap();
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    };
    inject(&mut sink, &target, InputAction::PointerMove { to: corner }).await;
    let motion = server.events.lock().unwrap().motion.clone();
    // Alone, the desktop was 32x16 logical pixels; with the new monitor's
    // 40x20 beside it, 72x20. TEST-0's last pixel is at 31.5, 15.5.
    assert_eq!(motion[0], (8065, 3969, 8192, 4096));
    assert_eq!(motion[1], (8065, 3969, 72 * 256, 20 * 256));
}
