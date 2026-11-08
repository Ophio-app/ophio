// SPDX-License-Identifier: Apache-2.0
//! Drives the wlroots backend against a real compositor. Ignored by default:
//! it needs a private compositor, never the desktop in use.
//!
//! `OPHIO_LIVE_WAYLAND` is the compositor's absolute socket path and
//! `OPHIO_LIVE_STEPS` a JSON array whose items are either an `InputAction`,
//! `{"sleep_ms": n}`, `"release_all"`, `"target <label>"` or `"drop"` (end the input session and
//! keep the process alive, as Ophio does once control ends).
//! `live_standin_keyboard` stands in for a person's own keyboard.
#![cfg(target_os = "linux")]
use platform_linux::{Environment, LinuxDesktop, wayland::WaylandDriver};
use ports::desktop::DesktopBackend;
use protocol::control::InputAction;
use std::{sync::Arc, time::Duration};

#[derive(serde::Deserialize)]
#[serde(untagged)]
enum Step {
    Action(InputAction),
    Sleep { sleep_ms: u64 },
    Named(String),
}

#[tokio::test]
#[ignore = "needs a private compositor socket in OPHIO_LIVE_WAYLAND"]
async fn live_compositor_steps() {
    let steps: Vec<Step> =
        serde_json::from_str(&std::env::var("OPHIO_LIVE_STEPS").expect("OPHIO_LIVE_STEPS"))
            .expect("steps are JSON");
    // `OPHIO_LIVE_X11` drives a private X server instead.
    let desktop = if let Ok(display) = std::env::var("OPHIO_LIVE_X11") {
        let detected = Environment {
            session_type: "x11".into(),
            x11_available: true,
            ..Default::default()
        };
        LinuxDesktop::new(
            Arc::new(
                platform_linux::x11::X11Driver::connect(display)
                    .await
                    .unwrap(),
            ),
            LinuxDesktop::detect(&detected).capabilities,
            None,
        )
    } else {
        let socket = std::env::var("OPHIO_LIVE_WAYLAND").expect("OPHIO_LIVE_WAYLAND");
        let driver = WaylandDriver::new(socket.into());
        let detected = Environment {
            session_type: "wayland".into(),
            wayland_globals: driver.advertised_globals().await.unwrap(),
            ..Default::default()
        };
        LinuxDesktop::new(
            Arc::new(driver),
            LinuxDesktop::detect(&detected).capabilities,
            None,
        )
    };
    // `OPHIO_LIVE_TARGET` picks a target by label; the first by default.
    let label = std::env::var("OPHIO_LIVE_TARGET").ok();
    let mut target = desktop
        .targets()
        .await
        .unwrap()
        .into_iter()
        .find(|t| label.as_ref().is_none_or(|label| *label == t.label))
        .expect("target");
    println!(
        "target {} {}x{} scale {}",
        target.label, target.geometry.width, target.geometry.height, target.geometry.scale
    );
    let mut sink = Some(desktop.input_sink(target.target_id).await.unwrap());
    for (index, step) in steps.into_iter().enumerate() {
        match step {
            Step::Action(action) => {
                if sink.is_none() {
                    sink = Some(desktop.input_sink(target.target_id).await.unwrap());
                }
                let sink = sink.as_mut().unwrap();
                let started = std::time::Instant::now();
                let result = sink.inject(&action, &target.geometry).await;
                println!(
                    "step {index}: {} in {:?}: {result:?}",
                    serde_json::to_string(&action).unwrap(),
                    started.elapsed()
                );
            }
            Step::Sleep { sleep_ms } => tokio::time::sleep(Duration::from_millis(sleep_ms)).await,
            Step::Named(name) if name == "release_all" => {
                if let Some(sink) = sink.as_mut() {
                    println!("step {index}: release_all {:?}", sink.release_all().await);
                }
            }
            Step::Named(name) if name == "drop" => {
                drop(sink.take());
                // Let the spawned release task finish before the next step.
                tokio::time::sleep(Duration::from_millis(200)).await;
                println!("step {index}: dropped");
            }
            Step::Named(name) if name.starts_with("target ") => {
                // Pick a target afresh, as the phone does after a layout change.
                let label = &name["target ".len()..];
                target = desktop
                    .targets()
                    .await
                    .unwrap()
                    .into_iter()
                    .find(|t| t.label == label)
                    .expect("target");
                println!(
                    "step {index}: target {} {}x{} scale {}",
                    target.label,
                    target.geometry.width,
                    target.geometry.height,
                    target.geometry.scale
                );
            }
            Step::Named(name) => panic!("unknown step {name}"),
        }
    }
}

/// Stands in for a person's own keyboard on a private compositor: a virtual
/// keyboard with the layout `OPHIO_LIVE_STANDIN_LAYOUT` that types one key,
/// so it becomes the seat's keyboard, then stays for `OPHIO_LIVE_STANDIN_MS`.
#[test]
#[ignore = "needs a private compositor socket in OPHIO_LIVE_WAYLAND"]
fn live_standin_keyboard() {
    use std::io::{Seek, Write};
    use std::os::fd::AsFd;
    use wayland_client::{
        Connection, Dispatch, QueueHandle, delegate_noop,
        protocol::{wl_registry, wl_seat},
    };
    use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
        zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1,
        zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1,
    };
    #[derive(Default)]
    struct Globals {
        seat: Option<wl_seat::WlSeat>,
        manager: Option<ZwpVirtualKeyboardManagerV1>,
    }
    impl Dispatch<wl_registry::WlRegistry, ()> for Globals {
        fn event(
            state: &mut Self,
            registry: &wl_registry::WlRegistry,
            event: wl_registry::Event,
            _: &(),
            _: &Connection,
            qh: &QueueHandle<Self>,
        ) {
            if let wl_registry::Event::Global {
                name, interface, ..
            } = event
            {
                match interface.as_str() {
                    "wl_seat" => state.seat = Some(registry.bind(name, 1, qh, ())),
                    "zwp_virtual_keyboard_manager_v1" => {
                        state.manager = Some(registry.bind(name, 1, qh, ()))
                    }
                    _ => {}
                }
            }
        }
    }
    delegate_noop!(Globals: ignore wl_seat::WlSeat);
    delegate_noop!(Globals: ignore ZwpVirtualKeyboardManagerV1);
    delegate_noop!(Globals: ignore ZwpVirtualKeyboardV1);
    let socket = std::env::var("OPHIO_LIVE_WAYLAND").expect("OPHIO_LIVE_WAYLAND");
    let layout = std::env::var("OPHIO_LIVE_STANDIN_LAYOUT").unwrap_or_else(|_| "us".into());
    let hold = std::env::var("OPHIO_LIVE_STANDIN_MS")
        .ok()
        .and_then(|ms| ms.parse().ok())
        .unwrap_or(5000);
    let connection =
        Connection::from_socket(std::os::unix::net::UnixStream::connect(socket).unwrap()).unwrap();
    let mut queue = connection.new_event_queue();
    connection.display().get_registry(&queue.handle(), ());
    let mut globals = Globals::default();
    queue.roundtrip(&mut globals).unwrap();
    let context = xkbcommon::xkb::Context::new(xkbcommon::xkb::CONTEXT_NO_FLAGS);
    let mut text =
        xkbcommon::xkb::Keymap::new_from_names(&context, "evdev", "pc105", &layout, "", None, 0)
            .unwrap()
            .get_as_string(xkbcommon::xkb::KEYMAP_FORMAT_TEXT_V1)
            .into_bytes();
    text.push(0);
    let mut file = tempfile::tempfile().unwrap();
    file.write_all(&text).unwrap();
    file.rewind().unwrap();
    let keyboard = globals.manager.as_ref().unwrap().create_virtual_keyboard(
        globals.seat.as_ref().unwrap(),
        &queue.handle(),
        (),
    );
    keyboard.keymap(1, file.as_fd(), text.len() as u32);
    // Shift: no shortcut, no text.
    keyboard.key(1, 42, 1);
    keyboard.key(2, 42, 0);
    // `OPHIO_LIVE_STANDIN_CAPS` turns its Caps Lock on, as a person would.
    if std::env::var_os("OPHIO_LIVE_STANDIN_CAPS").is_some() {
        keyboard.key(3, 58, 1);
        keyboard.modifiers(0, 0, 2, 0);
        keyboard.key(4, 58, 0);
    }
    queue.roundtrip(&mut globals).unwrap();
    println!("standin {layout} ready");
    std::thread::sleep(Duration::from_millis(hold));
    keyboard.destroy();
    queue.roundtrip(&mut globals).unwrap();
}

/// Prints each key an X client receives on the private display
/// `OPHIO_LIVE_X11`, translated through the keyboard map as the client reads
/// it at that moment, for `OPHIO_LIVE_X11_MS`.
#[test]
#[ignore = "needs a private X server in OPHIO_LIVE_X11"]
fn live_x11_observer() {
    use x11rb::{
        connection::Connection,
        protocol::{Event, xproto::*},
        wrapper::ConnectionExt as _,
    };
    let display = std::env::var("OPHIO_LIVE_X11").expect("OPHIO_LIVE_X11");
    let ms: u64 = std::env::var("OPHIO_LIVE_X11_MS")
        .ok()
        .and_then(|ms| ms.parse().ok())
        .unwrap_or(5000);
    let (connection, screen) = x11rb::connect(Some(&display)).unwrap();
    let root = connection.setup().roots[screen].root;
    let window = connection.generate_id().unwrap();
    let mask = EventMask::KEY_PRESS | EventMask::KEY_RELEASE | EventMask::BUTTON_PRESS;
    connection
        .create_window(
            0,
            window,
            root,
            0,
            0,
            400,
            300,
            0,
            WindowClass::INPUT_OUTPUT,
            0,
            &CreateWindowAux::new().event_mask(mask),
        )
        .unwrap();
    connection.map_window(window).unwrap();
    connection.sync().unwrap();
    connection
        .set_input_focus(InputFocus::PARENT, window, x11rb::CURRENT_TIME)
        .unwrap();
    connection.sync().unwrap();
    // Keycodes whose first keysym is a non-ASCII character: remaps left over.
    {
        let (first, last) = (
            connection.setup().min_keycode,
            connection.setup().max_keycode,
        );
        let map = connection
            .get_keyboard_mapping(first, last - first + 1)
            .unwrap()
            .reply()
            .unwrap();
        let width = usize::from(map.keysyms_per_keycode);
        for (i, row) in map.keysyms.chunks_exact(width).enumerate() {
            if (0xa0..=0xff).contains(&row[0]) || (0x0100_0100..=0x0110_ffff).contains(&row[0]) {
                println!("x11 remapped {} {:x?}", usize::from(first) + i, row);
            }
        }
    }
    println!("observer ready");
    let deadline = std::time::Instant::now() + Duration::from_millis(ms);
    let (first, last) = (
        connection.setup().min_keycode,
        connection.setup().max_keycode,
    );
    while std::time::Instant::now() < deadline {
        let Some(event) = connection.poll_for_event().unwrap() else {
            std::thread::sleep(Duration::from_millis(2));
            continue;
        };
        if let Event::KeyPress(key) = event {
            let map = connection
                .get_keyboard_mapping(first, last - first + 1)
                .unwrap()
                .reply()
                .unwrap();
            let width = usize::from(map.keysyms_per_keycode);
            let row = &map.keysyms[usize::from(key.detail - first) * width..][..width];
            let shifted = u16::from(key.state) & u16::from(KeyButMask::SHIFT) != 0;
            let sym = if shifted && width > 1 && row[1] != 0 {
                row[1]
            } else {
                row[0]
            };
            println!(
                "x11 key {} state {:#x} sym {sym:#x} {} row {row:x?}",
                key.detail,
                u16::from(key.state),
                char::from_u32(sym)
                    .filter(|c| !c.is_control())
                    .unwrap_or(' ')
            );
        }
    }
}
