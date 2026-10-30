// SPDX-License-Identifier: Apache-2.0
//! The window is message-only. Only the notification-area icon is visible.
use crate::protection::Request;
use ports::desktop::PlatformError;
use tokio::sync::{mpsc, oneshot};
use windows::{
    Win32::{
        Foundation::*,
        System::LibraryLoader::GetModuleHandleW,
        UI::{Shell::*, WindowsAndMessaging::*},
    },
    core::{PCWSTR, w},
};

const CALLBACK: u32 = WM_APP + 19;
fn error() -> PlatformError {
    PlatformError::Native("Windows could not show the local Stop indicator".into())
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // SAFETY: GWLP_USERDATA is set to a live Sender by run and cleared before
    // destroying the window. The message loop and this callback share a thread.
    unsafe {
        if message == CALLBACK
            && matches!(
                lparam.0 as u32,
                WM_LBUTTONUP | WM_RBUTTONUP | NIN_BALLOONUSERCLICK
            )
        {
            let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const mpsc::Sender<()>;
            if let Some(stop) = ptr.as_ref() {
                let _ = stop.try_send(());
            }
            return LRESULT(0);
        }
        DefWindowProcW(hwnd, message, wparam, lparam)
    }
}
fn wide<const N: usize>(text: &str) -> [u16; N] {
    let mut output = [0; N];
    let mut index = 0;
    for character in text.chars().filter(|c| *c != '\0') {
        let mut units = [0; 2];
        let units = character.encode_utf16(&mut units);
        if index + units.len() >= N {
            break;
        }
        output[index..index + units.len()].copy_from_slice(units);
        index += units.len();
    }
    output
}
pub(crate) fn run(
    mut requests: mpsc::UnboundedReceiver<Request>,
    stop: mpsc::Sender<()>,
    ready: oneshot::Sender<Result<(), PlatformError>>,
) {
    // SAFETY: all native resources live on this thread; the icon is removed and
    // the message-only window destroyed before its callback data is dropped.
    unsafe {
        let class = w!("OphioLocalStopIndicator");
        let definition = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            lpszClassName: class,
            ..Default::default()
        };
        if RegisterClassW(&definition) == 0 {
            let _ = ready.send(Err(error()));
            return;
        }
        let window = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class,
            w!("Local Stop"),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            None,
            None,
        );
        let Ok(window) = window else {
            let _ = ready.send(Err(error()));
            let _ = UnregisterClassW(class, None);
            return;
        };
        SetWindowLongPtrW(window, GWLP_USERDATA, &stop as *const _ as isize);
        // ophio.exe carries its icon as resource 1; anything else gets a warning sign.
        let icon = GetModuleHandleW(None)
            .and_then(|module| {
                LoadImageW(
                    Some(module.into()),
                    PCWSTR(1 as _),
                    IMAGE_ICON,
                    GetSystemMetrics(SM_CXSMICON),
                    GetSystemMetrics(SM_CYSMICON),
                    LR_DEFAULTCOLOR,
                )
            })
            .map(|image| HICON(image.0))
            .or_else(|_| LoadIconW(None, IDI_WARNING))
            .unwrap_or_default();
        let mut data = NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: window,
            uID: 1,
            uCallbackMessage: CALLBACK,
            hIcon: icon,
            ..Default::default()
        };
        let mut shown = false;
        // The taskbar that holds the icon; a restarted Explorer has a new one.
        let mut tray = HWND::default();
        let _ = ready.send(Ok(()));
        loop {
            match requests.try_recv() {
                Ok(Request::Show(holders, reply)) => {
                    data.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP | NIF_INFO;
                    data.szTip = wide(&format!("Click to Stop · {holders}"));
                    data.szInfoTitle = wide("Computer control is active · Click to Stop");
                    data.szInfo = wide(&holders);
                    data.dwInfoFlags = NIIF_WARNING;
                    let ok = Shell_NotifyIconW(if shown { NIM_MODIFY } else { NIM_ADD }, &data)
                        .as_bool();
                    if ok && !shown {
                        tray = FindWindowW(w!("Shell_TrayWnd"), None).unwrap_or_default();
                    }
                    shown |= ok;
                    let _ = reply.send(if ok { Ok(()) } else { Err(error()) });
                }
                Ok(Request::Hide(reply)) => {
                    let ok = !shown || Shell_NotifyIconW(NIM_DELETE, &data).as_bool();
                    if ok {
                        shown = false;
                    }
                    let _ = reply.send(if ok { Ok(()) } else { Err(error()) });
                }
                Ok(Request::Close) | Err(mpsc::error::TryRecvError::Disconnected) => break,
                Err(mpsc::error::TryRecvError::Empty) => {}
            }
            let mut message = MSG::default();
            while PeekMessageW(&mut message, Some(window), 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
            // Explorer loss removes the protection: request a stop. Future grants
            // must recreate the indicator instead of assuming it survived. The
            // icon's own position is no test: Windows reports none for an icon
            // kept among the hidden ones, which is where new icons start.
            if shown && !IsWindow(Some(tray)).as_bool() {
                let _ = stop.try_send(());
                let _ = Shell_NotifyIconW(NIM_DELETE, &data);
                shown = false;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        if shown {
            let _ = Shell_NotifyIconW(NIM_DELETE, &data);
        }
        SetWindowLongPtrW(window, GWLP_USERDATA, 0);
        let _ = DestroyWindow(window);
        let _ = UnregisterClassW(class, None);
    }
}
