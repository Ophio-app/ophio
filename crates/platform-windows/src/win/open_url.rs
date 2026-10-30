// SPDX-License-Identifier: Apache-2.0
use std::io;
use windows::{
    Win32::UI::{Shell::ShellExecuteW, WindowsAndMessaging::SW_SHOWNORMAL},
    core::PCWSTR,
};

/// Asks the shell to open `url` with its registered handler.
pub fn open_url(url: &str) -> io::Result<()> {
    if url.contains('\0') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "address has a nul",
        ));
    }
    let url: Vec<u16> = url.encode_utf16().chain(Some(0)).collect();
    let verb: Vec<u16> = "open".encode_utf16().chain(Some(0)).collect();
    // SAFETY: both strings are nul-terminated and live through this call.
    let result = unsafe {
        ShellExecuteW(
            None,
            PCWSTR(verb.as_ptr()),
            PCWSTR(url.as_ptr()),
            None,
            None,
            SW_SHOWNORMAL,
        )
    };
    // Values above 32 are success; the rest are error codes.
    if result.0 as isize > 32 {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "Windows could not open the address (code {})",
            result.0 as isize
        )))
    }
}
