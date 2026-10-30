// SPDX-License-Identifier: Apache-2.0
use super::*;
use crate::win::{Handle, token, token_buffer};
use input::{Button, Event, Key};
use windows::{
    Win32::{
        Foundation::*,
        Graphics::Gdi::*,
        Security::*,
        System::{
            RemoteDesktop::*, Shutdown::LockWorkStation, StationsAndDesktops::*, Threading::*,
        },
        UI::{
            HiDpi::*,
            Input::{Ime::ImmGetDefaultIMEWnd, KeyboardAndMouse::*},
            WindowsAndMessaging::*,
        },
    },
    core::{BOOL, PCWSTR, PWSTR},
};

pub(super) fn failure(error: windows::core::Error) -> PlatformError {
    match error.code().0 as u32 {
        0x80070005 => PlatformError::PermissionMissing("Windows denied desktop access".into()),
        0x887a0026 | 0x887a0027 | 0x887a0005 | 0x887a0006 => unavailable(),
        _ => PlatformError::Native("Windows desktop operation failed".into()),
    }
}
pub(super) fn unavailable() -> PlatformError {
    PlatformError::TemporarilyUnavailable(
        "the interactive desktop is locked, disconnected or unavailable".into(),
    )
}
pub(super) struct Dpi(DPI_AWARENESS_CONTEXT);
impl Dpi {
    pub(super) fn enter() -> Result<Self, PlatformError> {
        // SAFETY: thread-local setting restored by Drop on this same thread.
        let previous =
            unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
        if previous.0.is_null() {
            return Err(unavailable());
        }
        Ok(Self(previous))
    }
}
impl Drop for Dpi {
    fn drop(&mut self) {
        // SAFETY: restore the thread context captured by enter; never held across await.
        unsafe {
            SetThreadDpiAwarenessContext(self.0);
        }
    }
}
struct Desktop(HDESK);
impl Drop for Desktop {
    fn drop(&mut self) {
        // SAFETY: owns an OpenInputDesktop handle, never a borrowed thread desktop.
        unsafe {
            let _ = CloseDesktop(self.0);
        }
    }
}
struct Wts(*mut std::ffi::c_void);
impl Drop for Wts {
    fn drop(&mut self) {
        // SAFETY: WTSQuerySessionInformation allocated this buffer.
        unsafe {
            WTSFreeMemory(self.0);
        }
    }
}
fn desktop_name(handle: HDESK) -> Result<String, PlatformError> {
    let mut text = [0u16; 256];
    let mut needed = 0;
    // SAFETY: writable UTF-16 buffer and live desktop handle.
    unsafe {
        GetUserObjectInformationW(
            HANDLE(handle.0),
            UOI_NAME,
            Some(text.as_mut_ptr().cast()),
            size_of_val(&text) as u32,
            Some(&mut needed),
        )
        .map_err(|_| unavailable())?;
    }
    let end = text.iter().position(|c| *c == 0).ok_or_else(unavailable)?;
    String::from_utf16(&text[..end]).map_err(|_| unavailable())
}
/// This process's session: whether it is the active one, and its lock flags.
fn session() -> Result<(bool, u32), PlatformError> {
    // SAFETY: read-only OS session queries; the buffer is owned and freed.
    unsafe {
        let mut session = 0;
        ProcessIdToSessionId(GetCurrentProcessId(), &mut session).map_err(|_| unavailable())?;
        if session == 0 {
            return Err(unavailable());
        }
        let mut buffer = PWSTR::null();
        let mut len = 0;
        WTSQuerySessionInformationW(None, session, WTSSessionInfoEx, &mut buffer, &mut len)
            .map_err(|_| unavailable())?;
        let _buffer = Wts(buffer.0.cast());
        if len < size_of::<WTSINFOEXW>() as u32 {
            return Err(unavailable());
        }
        let info = &*buffer.0.cast::<WTSINFOEXW>();
        if info.Level != 1 {
            return Err(unavailable());
        }
        let level = &info.Data.WTSInfoExLevel1;
        Ok((level.SessionState == WTSActive, level.SessionFlags as u32))
    }
}
/// What Windows says of the session lock; whatever it cannot say is unknown.
pub(super) fn lock_state() -> DesktopLockState {
    match session() {
        Ok((_, WTS_SESSIONSTATE_LOCK)) => DesktopLockState::Locked,
        Ok((_, WTS_SESSIONSTATE_UNLOCK)) => DesktopLockState::Unlocked,
        _ => DesktopLockState::Unknown,
    }
}
/// Never switches desktops, authenticates, unlocks or dismisses secure UI.
pub(super) fn session_ready() -> Result<(), PlatformError> {
    if session()? != (true, WTS_SESSIONSTATE_UNLOCK) {
        return Err(unavailable());
    }
    // SAFETY: read-only desktop queries; the desktop handle is owned.
    unsafe {
        let desktop = Desktop(
            OpenInputDesktop(DESKTOP_CONTROL_FLAGS(0), false, DESKTOP_READOBJECTS)
                .map_err(|_| unavailable())?,
        );
        let input_name = desktop_name(desktop.0)?;
        let own_name =
            desktop_name(GetThreadDesktop(GetCurrentThreadId()).map_err(|_| unavailable())?)?;
        if !input_name.eq_ignore_ascii_case("Default")
            || !own_name.eq_ignore_ascii_case(&input_name)
        {
            return Err(PlatformError::PermissionMissing(
                "input is refused on the secure desktop".into(),
            ));
        }
    }
    Ok(())
}
pub(super) fn monitors() -> Result<Vec<Monitor>, PlatformError> {
    let _dpi = Dpi::enter()?;
    session_ready()?;
    let mut result: Result<Vec<Monitor>, PlatformError> = Ok(Vec::new());
    // SAFETY: EnumDisplayMonitors calls synchronously. The callback receives the
    // exclusive pointer to `result` and never retains it or unwinds across FFI.
    unsafe {
        EnumDisplayMonitors(
            None,
            None,
            Some(enumerate),
            LPARAM((&mut result as *mut Result<Vec<Monitor>, PlatformError>) as isize),
        )
        .ok()
        .map_err(failure)?;
    }
    result
}
unsafe extern "system" fn enumerate(handle: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> BOOL {
    // SAFETY: data is the exclusive pointer supplied by monitors above.
    let result = unsafe { &mut *(data.0 as *mut Result<Vec<Monitor>, PlatformError>) };
    if result.is_err() {
        return true.into();
    }
    let item = (|| {
        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
        let mut mode = DEVMODEW {
            dmSize: size_of::<DEVMODEW>() as u16,
            ..Default::default()
        };
        let mut dx = 0;
        let mut dy = 0;
        // SAFETY: initialized outputs, live monitor, properly sized DEVMODE.
        let rotation = unsafe {
            GetMonitorInfoW(handle, (&mut info as *mut MONITORINFOEXW).cast())
                .ok()
                .map_err(failure)?;
            EnumDisplaySettingsW(
                PCWSTR(info.szDevice.as_ptr()),
                ENUM_CURRENT_SETTINGS,
                &mut mode,
            )
            .ok()
            .map_err(failure)?;
            GetDpiForMonitor(handle, MDT_EFFECTIVE_DPI, &mut dx, &mut dy).map_err(failure)?;
            match mode.Anonymous1.Anonymous2.dmDisplayOrientation.0 {
                0 => 0,
                1 => 90,
                2 => 180,
                3 => 270,
                _ => return Err(geometry::invalid()),
            }
        };
        let end = info
            .szDevice
            .iter()
            .position(|c| *c == 0)
            .ok_or_else(geometry::invalid)?;
        let name = String::from_utf16(&info.szDevice[..end]).map_err(|_| geometry::invalid())?;
        let rect = info.monitorInfo.rcMonitor;
        Ok(Monitor {
            name,
            x: rect.left,
            y: rect.top,
            width: u32::try_from(i64::from(rect.right) - i64::from(rect.left))
                .map_err(|_| geometry::invalid())?,
            height: u32::try_from(i64::from(rect.bottom) - i64::from(rect.top))
                .map_err(|_| geometry::invalid())?,
            dpi: dx,
            rotation,
        })
    })();
    match item {
        Ok(item) => {
            if let Ok(items) = result {
                items.push(item);
            }
        }
        Err(error) => *result = Err(error),
    }
    true.into()
}
fn integrity(process: HANDLE) -> Result<u32, PlatformError> {
    let token = token(process).map_err(|_| {
        PlatformError::PermissionMissing("cannot inspect target integrity level".into())
    })?;
    let buffer = token_buffer(token.0, TokenIntegrityLevel).map_err(|_| unavailable())?;
    // SAFETY: TokenIntegrityLevel returns TOKEN_MANDATORY_LABEL with a live SID.
    unsafe {
        let label = &*buffer.as_ptr().cast::<TOKEN_MANDATORY_LABEL>();
        let count = *GetSidSubAuthorityCount(label.Label.Sid);
        if count == 0 {
            return Err(unavailable());
        }
        Ok(*GetSidSubAuthority(label.Label.Sid, u32::from(count - 1)))
    }
}
fn window_allowed(window: HWND, ours: u32) -> Result<(), PlatformError> {
    if window.0.is_null() {
        return Err(unavailable());
    }
    // SAFETY: Windows validates a possibly transient HWND; no raw memory access.
    unsafe {
        let mut pid = 0;
        GetWindowThreadProcessId(window, Some(&mut pid));
        if pid == 0 {
            return Err(unavailable());
        }
        let process = Handle(
            OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).map_err(|_| {
                PlatformError::PermissionMissing("target process cannot be inspected".into())
            })?,
        );
        if integrity(process.0)? > ours {
            return Err(PlatformError::PermissionMissing(
                "input refused: target has higher integrity (UIPI)".into(),
            ));
        }
    }
    Ok(())
}
#[derive(Debug)]
pub(super) struct NativeDriver;
impl NativeDriver {
    pub(super) fn new() -> Result<Self, PlatformError> {
        // SAFETY: changes only this process's DPI policy. Already-aware callers
        // are accepted; an incompatible process policy is rejected explicitly.
        unsafe {
            let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
            if !AreDpiAwarenessContextsEqual(
                GetDpiAwarenessContextForProcess(GetCurrentProcess()),
                DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
            )
            .as_bool()
            {
                return Err(PlatformError::Unsupported(
                    "Øphio requires per-monitor v2 DPI awareness".into(),
                ));
            }
            if integrity(GetCurrentProcess())? >= 0x3000 {
                return Err(PlatformError::PermissionMissing(
                    "run Øphio without elevation".into(),
                ));
            }
        }
        session_ready()?;
        Ok(Self)
    }
}
impl Driver for NativeDriver {
    fn monitors(&self) -> Result<Vec<Monitor>, PlatformError> {
        monitors()
    }
    fn capabilities(&self) -> DesktopCapabilities {
        let availability = match session_ready() {
            Ok(()) => Availability::Available,
            Err(PlatformError::PermissionMissing(_)) => Availability::PermissionMissing {
                remedy: "Return to the signed-in user's normal desktop.".into(),
            },
            Err(_) => Availability::TemporarilyUnavailable {
                reason: "The interactive desktop is unavailable.".into(),
            },
        };
        capabilities(availability)
    }
    fn capture(&self) -> Result<Box<dyn Capture>, PlatformError> {
        Ok(Box::new(super::capture::Worker::new()?))
    }
    fn injector(&self) -> Result<Box<dyn Injector>, PlatformError> {
        session_ready()?;
        // SAFETY: current-process pseudo handle is queried, not closed.
        Ok(Box::new(NativeInjector {
            integrity: integrity(unsafe { GetCurrentProcess() })?,
            layout: None,
            deadline: None,
            typing: Default::default(),
        }))
    }
    fn lock(&self) -> Result<(), PlatformError> {
        session_ready()?;
        // SAFETY: explicitly requested lock of our own interactive workstation.
        unsafe { LockWorkStation().map_err(failure) }
    }
    fn lock_state(&self) -> DesktopLockState {
        lock_state()
    }
}
struct NativeInjector {
    integrity: u32,
    layout: Option<Layout>,
    deadline: Option<Instant>,
    /// Keys still down for a character typed as keys, in press order.
    typing: std::collections::BTreeMap<u16, Vec<(u16, u16)>>,
}
impl Injector for NativeInjector {
    fn check_layout(&mut self, layout: &Layout, deadline: Instant) -> Result<(), PlatformError> {
        self.deadline = Some(deadline);
        if &Layout::new(monitors()?)? != layout {
            return Err(PlatformError::GeometryChanged);
        }
        self.layout = Some(layout.clone());
        Ok(())
    }
    fn emit(&mut self, event: Event) -> Result<(), PlatformError> {
        let _dpi = Dpi::enter()?;
        session_ready()?;
        let release = matches!(
            event,
            Event::Key { down: false, .. } | Event::Button { down: false, .. }
        );
        if !release && self.layout.as_ref() != Some(&Layout::new(monitors()?)?) {
            return Err(PlatformError::GeometryChanged);
        }
        // SAFETY: read-only HWND queries; every event revalidates UIPI before insertion.
        unsafe {
            window_allowed(GetForegroundWindow(), self.integrity)?;
            if !matches!(event, Event::Key { .. }) {
                let mut point = POINT::default();
                if let Event::Move { physical, .. } = event {
                    point.x = physical.0;
                    point.y = physical.1;
                } else {
                    GetCursorPos(&mut point).map_err(failure)?;
                }
                window_allowed(WindowFromPoint(point), self.integrity)?;
            }
        }
        let native = match event {
            Event::Key {
                key: Key::Unicode(unit),
                down,
            } => return self.character(unit, down),
            Event::Key {
                key: Key::Virtual { code, extended },
                down,
            } => keyboard(
                (code, 0),
                if extended {
                    KEYEVENTF_EXTENDEDKEY
                } else {
                    KEYBD_EVENT_FLAGS(0)
                },
                down,
            ),
            _ => {
                let (dx, dy, data, flags) = match event {
                    Event::Move { absolute, .. } => (
                        absolute.0,
                        absolute.1,
                        0,
                        MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
                    ),
                    Event::Button { button, down } => (
                        0,
                        0,
                        0,
                        match (button, down) {
                            (Button::Left, true) => MOUSEEVENTF_LEFTDOWN,
                            (Button::Left, false) => MOUSEEVENTF_LEFTUP,
                            (Button::Right, true) => MOUSEEVENTF_RIGHTDOWN,
                            (Button::Right, false) => MOUSEEVENTF_RIGHTUP,
                            (Button::Middle, true) => MOUSEEVENTF_MIDDLEDOWN,
                            (Button::Middle, false) => MOUSEEVENTF_MIDDLEUP,
                        },
                    ),
                    Event::Wheel { horizontal, delta } => (
                        0,
                        0,
                        delta as u32,
                        if horizontal {
                            MOUSEEVENTF_HWHEEL
                        } else {
                            MOUSEEVENTF_WHEEL
                        },
                    ),
                    Event::Key { .. } => unreachable!(),
                };
                INPUT {
                    r#type: INPUT_MOUSE,
                    Anonymous: INPUT_0 {
                        mi: MOUSEINPUT {
                            dx,
                            dy,
                            mouseData: data,
                            dwFlags: flags,
                            ..Default::default()
                        },
                    },
                }
            }
        };
        insert(native, release, self.deadline)
    }
}
impl NativeInjector {
    /// Text goes in as characters, except where they would be lost: a console
    /// drops a character that arrives without its key, and PowerShell's line
    /// editor reads keys. There a character the layout types is pressed as
    /// its keys, and they are released with it.
    fn character(&mut self, unit: u16, down: bool) -> Result<(), PlatformError> {
        if down {
            if let Some((keys, dead)) = console_keys(unit) {
                // The keys were looked up with nothing else down. A key held at
                // the computer would change the character or make it a shortcut,
                // and letting go of it would take it from the person, so wait.
                if held_at_the_computer(&self.typing) {
                    return Err(PlatformError::TemporarilyUnavailable(
                        "a key is held down at the computer".into(),
                    ));
                }
                self.typing.entry(unit).or_default();
                for key in keys {
                    self.press(unit, key)?;
                }
                if dead {
                    // A dead key waits to mark the next letter; a space makes
                    // it type itself instead.
                    self.release(unit)?;
                    // SAFETY: a pure lookup in the current layout.
                    let scan = unsafe { MapVirtualKeyW(VK_SPACE.0.into(), MAPVK_VK_TO_VSC) };
                    self.press(unit, (VK_SPACE.0, scan as u16))?;
                    self.release(unit)?;
                }
                return Ok(());
            }
        } else if self.typing.contains_key(&unit) {
            self.release(unit)?;
            self.typing.remove(&unit);
            return Ok(());
        }
        let native = keyboard((0, unit), KEYEVENTF_UNICODE, down);
        insert(native, !down, self.deadline)
    }
    fn press(&mut self, unit: u16, key: (u16, u16)) -> Result<(), PlatformError> {
        insert(
            keyboard(key, KEYBD_EVENT_FLAGS(0), true),
            false,
            self.deadline,
        )?;
        self.typing.entry(unit).or_default().push(key);
        Ok(())
    }
    /// Lets go of a character's keys, last pressed first.
    fn release(&mut self, unit: u16) -> Result<(), PlatformError> {
        let held = self.typing.entry(unit).or_default();
        while let Some(&key) = held.last() {
            insert(
                keyboard(key, KEYBD_EVENT_FLAGS(0), false),
                true,
                self.deadline,
            )?;
            held.pop();
        }
        Ok(())
    }
}
fn keyboard((vk, scan): (u16, u16), flags: KEYBD_EVENT_FLAGS, down: bool) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(vk),
                wScan: scan,
                dwFlags: flags
                    | if down {
                        KEYBD_EVENT_FLAGS(0)
                    } else {
                        KEYEVENTF_KEYUP
                    },
                ..Default::default()
            },
        },
    }
}
fn insert(native: INPUT, release: bool, deadline: Option<Instant>) -> Result<(), PlatformError> {
    if !release && deadline.is_none_or(|end| SystemClock.monotonic() >= end) {
        return Err(PlatformError::TemporarilyUnavailable(
            "input action exceeded its execution window".into(),
        ));
    }
    // SAFETY: exactly one fully initialized INPUT. A short insertion is
    // always refusal; GetLastError cannot reliably identify UIPI blocking.
    if unsafe { SendInput(&[native], size_of::<INPUT>() as i32) } != 1 {
        return Err(PlatformError::PermissionMissing(
            "Windows refused input insertion".into(),
        ));
    }
    // SendInput confirmed insertion. Preserve that acknowledgement; every
    // following event rechecks session readiness before touching the OS.
    Ok(())
}
/// The modifiers a layout key may need, in press order, by VkKeyScan bit.
const MODIFIERS: [(u16, VIRTUAL_KEY); 3] = [(2, VK_CONTROL), (4, VK_MENU), (1, VK_SHIFT)];
/// Whether a key that changes what a layout key types, or turns it into a
/// shortcut, is down other than as one of `typing`'s own.
fn held_at_the_computer(typing: &std::collections::BTreeMap<u16, Vec<(u16, u16)>>) -> bool {
    [VK_SHIFT, VK_CONTROL, VK_MENU, VK_LWIN, VK_RWIN]
        .into_iter()
        // SAFETY: a read-only query of the keyboard's current state.
        .filter(|vk| unsafe { GetAsyncKeyState(vk.0.into()) } < 0)
        .any(|vk| !typing.values().flatten().any(|&(own, _)| own == vk.0))
}
/// The keys, with their scan codes, that type `unit` in the foreground
/// console on that console's layout and CapsLock: plain, shifted, or AltGr
/// (Ctrl+Alt) as Windows takes it, and whether they make a dead key. None
/// elsewhere, for control characters and for characters the layout has no
/// key for; those stay characters.
fn console_keys(unit: u16) -> Option<(Vec<(u16, u16)>, bool)> {
    if char::from_u32(u32::from(unit)).is_none_or(char::is_control) {
        return None;
    }
    // SAFETY: read-only queries; a stale HWND only fails them.
    unsafe {
        let window = GetForegroundWindow();
        let mut class = [0; 32];
        let length = GetClassNameW(window, &mut class).max(0) as usize;
        if !class[..length]
            .iter()
            .copied()
            .eq("ConsoleWindowClass".encode_utf16())
        {
            return None;
        }
        // A console window names its program's thread as its own, but its keys
        // are read by the console host's, in that thread's layout. The host's
        // thread also owns the window's input context, so that is how to find it.
        let reader = ImmGetDefaultIMEWnd(window);
        if reader.is_invalid() {
            return None;
        }
        let layout = GetKeyboardLayout(GetWindowThreadProcessId(reader, None));
        let found = VkKeyScanExW(unit, layout);
        let (vk, mask) = (VIRTUAL_KEY(found as u16 & 0xff), (found as u16) >> 8);
        // Ctrl or Alt alone types nothing; together they are AltGr.
        if found == -1 || mask > 7 || !matches!(mask & 6, 0 | 6) {
            return None;
        }
        let key = |vk: VIRTUAL_KEY| {
            let scan = MapVirtualKeyExW(vk.0.into(), MAPVK_VK_TO_VSC, Some(layout));
            (vk.0, scan as u16)
        };
        let mut state = [0u8; 256];
        state[usize::from(VK_CAPITAL.0)] = (GetKeyState(VK_CAPITAL.0.into()) & 1) as u8;
        // CapsLock turns some keys' case over, and Shift turns it back.
        for mask in [mask, mask ^ 1] {
            for (bit, modifier) in MODIFIERS {
                state[usize::from(modifier.0)] = if mask & bit == 0 { 0 } else { 0x80 };
            }
            let mut typed = [0; 4];
            // Flag 4 leaves the keyboard state, a pending dead key included, alone.
            let count = ToUnicodeEx(
                vk.0.into(),
                key(vk).1.into(),
                &state,
                &mut typed,
                4,
                Some(layout),
            );
            // A dead key says -1 and gives the character it types by itself.
            if matches!(count, 1 | -1) && typed[0] == unit {
                let mut keys: Vec<_> = MODIFIERS
                    .iter()
                    .filter(|(bit, _)| mask & bit != 0)
                    .map(|(_, modifier)| key(*modifier))
                    .collect();
                keys.push(key(vk));
                return Some((keys, count == -1));
            }
        }
        None
    }
}
