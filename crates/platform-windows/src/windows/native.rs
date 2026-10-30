// SPDX-License-Identifier: Apache-2.0
use super::*;
use windows::{
    Win32::{
        Foundation::{CloseHandle, HWND, LPARAM, WPARAM},
        Graphics::Gdi::{
            GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFOEXW, MonitorFromWindow,
        },
        System::Threading::{
            OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
            QueryFullProcessImageNameW,
        },
        UI::{Shell::ShellExecuteW, WindowsAndMessaging::*},
    },
    core::{BOOL, PCWSTR, PWSTR},
};
#[derive(Debug)]
pub(super) struct Native;
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}
fn same(window: &NativeWindow) -> bool {
    let hwnd = HWND(window.handle as *mut _);
    let mut pid = 0;
    // SAFETY: no memory from the window is dereferenced; USER32 validates the handle.
    unsafe {
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        IsWindow(Some(hwnd)).as_bool() && pid == window.pid
    }
}
impl Driver for Native {
    fn list(&self) -> Result<Vec<NativeWindow>, PlatformError> {
        let mut windows = Vec::<NativeWindow>::new();
        // SAFETY: the callback is synchronous and the pointer remains valid for EnumWindows.
        unsafe {
            EnumWindows(
                Some(enumerate),
                LPARAM((&mut windows as *mut Vec<NativeWindow>) as isize),
            )
        }
        .map_err(|_| {
            PlatformError::TemporarilyUnavailable(
                "Windows could not enumerate this desktop session".into(),
            )
        })?;
        Ok(windows)
    }
    fn focus(&self, window: &NativeWindow) -> Result<(), PlatformError> {
        if !same(window) {
            return Err(PlatformError::TargetNotFound);
        }
        let hwnd = HWND(window.handle as *mut _);
        // SAFETY: USER32 validates the handle. We honor foreground restrictions and never attach input threads.
        let focused =
            unsafe { SetForegroundWindow(hwnd).as_bool() && GetForegroundWindow() == hwnd };
        if focused {
            Ok(())
        } else {
            Err(PlatformError::PermissionMissing("Windows refused focus. It can keep the current window active while someone is using it or a menu is open. Select the window on the computer, then refresh.".into()))
        }
    }
    fn close(&self, window: &NativeWindow) -> Result<(), PlatformError> {
        if !same(window) {
            return Err(PlatformError::TargetNotFound);
        }
        // SAFETY: WM_CLOSE carries no pointers. This posts a normal close request, preserving save prompts.
        unsafe {
            PostMessageW(
                Some(HWND(window.handle as *mut _)),
                WM_CLOSE,
                WPARAM(0),
                LPARAM(0),
            )
        }
        .map_err(|_| {
            PlatformError::PermissionMissing(
                "Windows refused this close request; the app may have higher privileges".into(),
            )
        })
    }
    fn launch(&self, shortcut: &Path) -> Result<(), PlatformError> {
        use std::os::windows::ffi::OsStrExt;
        let path: Vec<u16> = shortcut.as_os_str().encode_wide().chain(Some(0)).collect();
        let verb = wide("open");
        // SAFETY: all strings are nul-terminated and live through this call. Only a discovered .lnk is accepted upstream.
        let result = unsafe {
            ShellExecuteW(
                None,
                PCWSTR(verb.as_ptr()),
                PCWSTR(path.as_ptr()),
                None,
                None,
                SW_SHOWNORMAL,
            )
        };
        if result.0 as isize > 32 {
            Ok(())
        } else {
            Err(PlatformError::Native(
                "Windows could not open the Start menu shortcut".into(),
            ))
        }
    }
}
unsafe extern "system" fn enumerate(hwnd: HWND, data: LPARAM) -> BOOL {
    // SAFETY: data points to the live Vec passed by list; EnumWindows invokes this callback synchronously.
    let list = unsafe { &mut *(data.0 as *mut Vec<NativeWindow>) };
    if list.len() >= 2048 {
        return BOOL(1);
    }
    // SAFETY: USER32 validates the enumerated handle; supplied output buffers are bounded and live.
    unsafe {
        if !IsWindowVisible(hwnd).as_bool()
            || GetWindowLongW(hwnd, GWL_EXSTYLE) as u32 & WS_EX_TOOLWINDOW.0 != 0
        {
            return BOOL(1);
        }
        let mut title = [0u16; 1024];
        let n = GetWindowTextW(hwnd, &mut title);
        if n <= 0 {
            return BOOL(1);
        }
        let mut pid = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        let mut class = [0u16; 256];
        let count = GetClassNameW(hwnd, &mut class);
        let mut app = String::from_utf16_lossy(&class[..count.max(0) as usize]);
        if let Ok(process) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
            let mut name = vec![0u16; 32768];
            let mut len = name.len() as u32;
            if QueryFullProcessImageNameW(
                process,
                PROCESS_NAME_WIN32,
                PWSTR(name.as_mut_ptr()),
                &mut len,
            )
            .is_ok()
            {
                app = PathBuf::from(String::from_utf16_lossy(&name[..len as usize]))
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or(app);
            }
            let _ = CloseHandle(process);
        }
        let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        let name = if GetMonitorInfoW(monitor, &mut info.monitorInfo).as_bool() {
            Some(String::from_utf16_lossy(
                &info.szDevice[..info
                    .szDevice
                    .iter()
                    .position(|c| *c == 0)
                    .unwrap_or(info.szDevice.len())],
            ))
        } else {
            None
        };
        list.push(NativeWindow {
            handle: hwnd.0 as usize,
            pid,
            title: String::from_utf16_lossy(&title[..n as usize]),
            app,
            monitor: name,
            focused: GetForegroundWindow() == hwnd,
        });
    }
    BOOL(1)
}
