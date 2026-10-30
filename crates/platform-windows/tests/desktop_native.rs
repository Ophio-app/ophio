// SPDX-License-Identifier: Apache-2.0
//! Native capture, the local Stop indicator and console typing on a signed-in
//! Windows desktop. Run by hand in the user's own interactive session; a
//! service or SSH session has no desktop.
#![cfg(windows)]
use platform_windows::desktop::WindowsDesktop;
use ports::desktop::{CapturedFrame, DesktopBackend, PlatformError};
use protocol::control::{ControlTarget, InputAction};
use windows::Win32::UI::Input::KeyboardAndMouse::{VIRTUAL_KEY, VK_CONTROL, VK_SHIFT};

fn has_content(target: &ControlTarget, picture: &CapturedFrame) {
    assert_eq!(
        (picture.width, picture.height),
        (target.geometry.width, target.geometry.height)
    );
    let first = &picture.data[..4];
    assert!(
        picture.data.chunks(4).any(|p| p != first),
        "{}: the picture is one flat color",
        target.label
    );
}

/// A still picture, then watching, leaving and watching again: each capture
/// starts and ends its own native thread, and the next must still work.
#[tokio::test]
#[ignore = "needs the signed-in user's desktop, run by hand"]
async fn every_display_gives_a_picture_again_and_again() {
    let desktop = WindowsDesktop::new().unwrap();
    let targets = desktop.targets().await.unwrap();
    assert!(!targets.is_empty(), "no displays");
    for target in targets {
        let still = desktop
            .capture_still(target.target_id)
            .await
            .unwrap_or_else(|e| panic!("{}: still picture failed: {e:?}", target.label));
        has_content(&target, &still);
        for _ in 0..2 {
            let mut stream = desktop.open_stream(target.target_id, 12).await.unwrap();
            for _ in 0..3 {
                let frame = stream
                    .next_frame()
                    .await
                    .unwrap_or_else(|e| panic!("{}: watching failed: {e:?}", target.label))
                    .expect("watching ended by itself");
                has_content(&target, &frame);
            }
        }
    }
    desktop.close().await.unwrap();
}

/// The notification-area Stop shows for a grant, stays without anyone pressing
/// it, hides, and shows again for the next grant.
#[tokio::test]
#[ignore = "needs the signed-in user's notification area, run by hand"]
async fn local_stop_shows_stays_and_shows_again() {
    let mut session = platform_windows::protection::connect().await.unwrap();
    for _ in 0..2 {
        session.indicator.show("Phone input").await.unwrap();
        let pressed =
            tokio::time::timeout(std::time::Duration::from_secs(2), session.stop.recv()).await;
        assert!(pressed.is_err(), "Stop fired with nobody pressing it");
        session.indicator.hide().await.unwrap();
    }
}

/// Losing the taskbar loses the visible Stop, so it stops what it protected.
#[tokio::test]
#[ignore = "ends and restarts the user's Explorer, run by hand on a lab desktop"]
async fn local_stop_fires_when_the_taskbar_goes() {
    let mut session = platform_windows::protection::connect().await.unwrap();
    session.indicator.show("Phone input").await.unwrap();
    let ended = std::process::Command::new("taskkill")
        .args(["/f", "/im", "explorer.exe"])
        .status()
        .unwrap();
    assert!(ended.success());
    tokio::time::timeout(std::time::Duration::from_secs(5), session.stop.recv())
        .await
        .expect("Stop did not fire when the taskbar went")
        .unwrap();
}

/// The few native calls the console test makes.
#[allow(unsafe_code)]
mod native {
    use windows::Win32::{
        Foundation::{CloseHandle, HWND},
        System::Threading::{
            AttachThreadInput, CREATE_NEW_CONSOLE, CreateProcessW, GetCurrentThreadId,
            PROCESS_INFORMATION, STARTUPINFOW, TerminateProcess, WaitForSingleObject,
        },
        UI::{
            Input::KeyboardAndMouse::{
                INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_KEYUP,
                SendInput, VIRTUAL_KEY,
            },
            WindowsAndMessaging::{
                BringWindowToTop, FindWindowW, GetForegroundWindow, GetWindowThreadProcessId,
                SetForegroundWindow,
            },
        },
    };
    use windows::core::{HSTRING, PCWSTR, PWSTR, w};

    pub fn key(vk: VIRTUAL_KEY, down: bool) {
        let input = INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: vk,
                    dwFlags: if down {
                        KEYBD_EVENT_FLAGS(0)
                    } else {
                        KEYEVENTF_KEYUP
                    },
                    ..Default::default()
                },
            },
        };
        // SAFETY: one fully initialized INPUT.
        assert_eq!(unsafe { SendInput(&[input], size_of::<INPUT>() as i32) }, 1);
    }

    /// A command prompt in a console of its own, as Start would open it, closed
    /// when dropped even if the test fails. Unlike `Command`, nothing of the
    /// test's own input or output is handed to it, so it reads what is typed
    /// into its window.
    pub struct Console(PROCESS_INFORMATION);
    impl Console {
        pub fn open(title: &str) -> Self {
            let mut command: Vec<u16> = format!("cmd.exe /k title {title}")
                .encode_utf16()
                .chain(Some(0))
                .collect();
            let startup = STARTUPINFOW {
                cb: size_of::<STARTUPINFOW>() as u32,
                ..Default::default()
            };
            let mut process = PROCESS_INFORMATION::default();
            // SAFETY: a writable, terminated command line and initialized structures.
            unsafe {
                CreateProcessW(
                    PCWSTR::null(),
                    Some(PWSTR(command.as_mut_ptr())),
                    None,
                    None,
                    false,
                    CREATE_NEW_CONSOLE,
                    None,
                    PCWSTR::null(),
                    &startup,
                    &mut process,
                )
            }
            .unwrap();
            Self(process)
        }
    }
    impl Drop for Console {
        fn drop(&mut self) {
            // SAFETY: handles this value owns, closed once.
            unsafe {
                let _ = TerminateProcess(self.0.hProcess, 1);
                let _ = WaitForSingleObject(self.0.hProcess, 5000);
                let _ = CloseHandle(self.0.hThread);
                let _ = CloseHandle(self.0.hProcess);
            }
        }
    }

    /// The classic console window with this title.
    pub fn console(title: &str) -> Option<HWND> {
        // SAFETY: a lookup by class and title.
        unsafe { FindWindowW(w!("ConsoleWindowClass"), &HSTRING::from(title)) }.ok()
    }

    /// Brings the window to the front, and says whether it is there. A
    /// background process may not take the foreground, so this one joins the
    /// input of the window in front while it asks, and asks again if refused.
    pub fn bring_forward(window: HWND) -> bool {
        for _ in 0..10 {
            // SAFETY: a top-level window; a stale one only fails the calls.
            unsafe {
                let ours = GetCurrentThreadId();
                let theirs = GetWindowThreadProcessId(GetForegroundWindow(), None);
                let joined = theirs != 0
                    && theirs != ours
                    && AttachThreadInput(ours, theirs, true).as_bool();
                let _ = BringWindowToTop(window);
                let _ = SetForegroundWindow(window);
                if joined {
                    let _ = AttachThreadInput(ours, theirs, false);
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
            // SAFETY: reads which window is in front.
            if unsafe { GetForegroundWindow() } == window {
                return true;
            }
        }
        false
    }
}

/// A key held down as if by the person at the computer, let go when dropped.
struct Held(VIRTUAL_KEY);
impl Held {
    fn new(vk: VIRTUAL_KEY) -> Self {
        native::key(vk, true);
        Self(vk)
    }
}
impl Drop for Held {
    fn drop(&mut self) {
        native::key(self.0, false);
    }
}

/// Typing into a classic console presses the layout's keys. While Shift or
/// Ctrl is held at the computer that would type another character or a
/// shortcut, so it waits; once let go, mixed case and shifted symbols arrive
/// as sent.
#[tokio::test]
#[ignore = "types into a console it opens on the signed-in user's desktop, run by hand"]
async fn console_text_waits_while_a_key_is_held_at_the_computer() {
    // Named for this run, so a window left by another is never the one used.
    let title = format!("ophio-held-key-{}", std::process::id());
    let saved = std::env::temp_dir().join(format!("{title}.txt"));
    let _ = std::fs::remove_file(&saved);
    let console = native::Console::open(&title);
    let mut window = None;
    for _ in 0..50 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        window = native::console(&title);
        if window.is_some() {
            break;
        }
    }
    let window = window.expect("no classic console opened; is another terminal the default?");
    assert!(native::bring_forward(window), "the console is not in front");

    let desktop = WindowsDesktop::new().unwrap();
    let target = desktop.targets().await.unwrap().remove(0);
    for vk in [VK_SHIFT, VK_CONTROL] {
        let held = Held::new(vk);
        let mut sink = desktop.input_sink(target.target_id).await.unwrap();
        let typed = sink
            .inject(&InputAction::Text { text: "ab".into() }, &target.geometry)
            .await;
        drop(held);
        assert!(
            matches!(typed, Err(PlatformError::TemporarilyUnavailable(_))),
            "typed with {vk:?} held: {typed:?}"
        );
    }
    let mut sink = desktop.input_sink(target.target_id).await.unwrap();
    for text in ["echo AbCdEfGh!>".to_owned(), format!("%TEMP%\\{title}.txt")] {
        sink.inject(&InputAction::Text { text }, &target.geometry)
            .await
            .unwrap();
    }
    sink.inject(
        &InputAction::KeyChord {
            keys: vec!["Enter".into()],
        },
        &target.geometry,
    )
    .await
    .unwrap();
    let mut written = None;
    for _ in 0..30 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        if let Ok(text) = std::fs::read_to_string(&saved) {
            written = Some(text);
            break;
        }
    }
    drop(console);
    let _ = std::fs::remove_file(&saved);
    desktop.close().await.unwrap();
    assert_eq!(written.as_deref(), Some("AbCdEfGh!\r\n"));
}
