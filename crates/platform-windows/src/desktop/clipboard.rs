// SPDX-License-Identifier: Apache-2.0
//! Write-only, explicitly requested replacement. No clipboard reads, backups,
//! delayed retries or restoration: a later local copy always stays untouched.
#![allow(unsafe_code)]
use ports::desktop::PlatformError;
use std::ffi::c_void;
use zeroize::Zeroizing;

#[cfg(test)]
mod tests;

type Handle = *mut c_void;
#[link(name = "user32")]
unsafe extern "system" {
    fn OpenClipboard(owner: Handle) -> i32;
    fn CloseClipboard() -> i32;
    fn EmptyClipboard() -> i32;
    fn SetClipboardData(format: u32, memory: Handle) -> Handle;
    fn GetClipboardSequenceNumber() -> u32;
    fn CreateWindowExW(
        ex: u32,
        class: *const u16,
        title: *const u16,
        style: u32,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        parent: Handle,
        menu: Handle,
        instance: Handle,
        param: Handle,
    ) -> Handle;
    fn DestroyWindow(window: Handle) -> i32;
}
#[link(name = "kernel32")]
unsafe extern "system" {
    fn GlobalAlloc(flags: u32, bytes: usize) -> Handle;
    fn GlobalLock(memory: Handle) -> Handle;
    fn GlobalUnlock(memory: Handle) -> i32;
    fn GlobalFree(memory: Handle) -> Handle;
}
struct Window(Handle);
impl Drop for Window {
    fn drop(&mut self) {
        // SAFETY: this thread owns this hidden, message-only window.
        unsafe {
            DestroyWindow(self.0);
        }
    }
}
struct Open;
impl Drop for Open {
    fn drop(&mut self) {
        // SAFETY: constructed only after successful OpenClipboard.
        unsafe {
            CloseClipboard();
        }
    }
}
struct Memory {
    handle: Handle,
    bytes: usize,
}
impl Drop for Memory {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            // SAFETY: this is still our allocation, not clipboard-owned memory.
            // Wipe only the replacement text; no prior clipboard was read.
            unsafe {
                let pointer = GlobalLock(self.handle);
                if !pointer.is_null() {
                    for index in 0..self.bytes {
                        std::ptr::write_volatile(pointer.cast::<u8>().add(index), 0);
                    }
                    GlobalUnlock(self.handle);
                }
                GlobalFree(self.handle);
            }
        }
    }
}
fn unavailable() -> PlatformError {
    PlatformError::TemporarilyUnavailable("Windows clipboard is busy or unavailable".into())
}
fn changed() -> PlatformError {
    PlatformError::OutcomeUnknown(
        "Clipboard replacement was started; the paste shortcut was not confirmed".into(),
    )
}
/// Prepares only supplied text, without opening a clipboard or creating a window.
fn text_memory(text: &str) -> Result<Memory, PlatformError> {
    let mut text = Zeroizing::new(text.encode_utf16().collect::<Vec<_>>());
    text.push(0);
    let bytes = text.len() * size_of::<u16>();
    // SAFETY: allocate movable clipboard-compatible storage; checked below.
    let memory = Memory {
        handle: unsafe { GlobalAlloc(2, bytes) },
        bytes,
    };
    if memory.handle.is_null() {
        return Err(unavailable());
    }
    // SAFETY: live HGLOBAL; the returned pointer is checked before copying.
    let pointer = unsafe { GlobalLock(memory.handle) };
    if pointer.is_null() {
        return Err(unavailable());
    }
    // SAFETY: allocation holds exactly `bytes`; the source lives through copy.
    unsafe {
        std::ptr::copy_nonoverlapping(text.as_ptr(), pointer.cast::<u16>(), text.len());
        GlobalUnlock(memory.handle);
    }
    Ok(memory)
}
/// This is the only production entry point that changes the global clipboard.
pub(super) fn replace(text: &str, deadline: std::time::Instant) -> Result<u32, PlatformError> {
    let mut memory = text_memory(text)?;
    let class: Vec<u16> = "STATIC\0".encode_utf16().collect();
    // SAFETY: built-in class, no text, message-only HWND_MESSAGE parent. No
    // visible window, activation, message pump, prompt or external input.
    let owner = Window(unsafe {
        CreateWindowExW(
            0,
            class.as_ptr(),
            std::ptr::null(),
            0,
            0,
            0,
            0,
            0,
            -3isize as Handle,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    });
    if owner.0.is_null() {
        return Err(unavailable());
    }
    // SAFETY: live window; no clipboard contents are inspected.
    if unsafe { OpenClipboard(owner.0) } == 0 {
        return Err(unavailable());
    }
    let _open = Open;
    if std::time::Instant::now() >= deadline {
        return Err(unavailable());
    }
    // SAFETY: caller exclusively owns the open clipboard. Explicit Paste alone
    // authorizes this destructive replacement. From here refusal is partial.
    if unsafe { EmptyClipboard() } == 0 {
        return Err(changed());
    }
    // SAFETY: CF_UNICODETEXT (13), owned movable NUL-terminated UTF-16 storage.
    if unsafe { SetClipboardData(13, memory.handle) }.is_null() {
        return Err(changed());
    }
    memory.handle = std::ptr::null_mut(); // ownership transferred to the OS
    // SAFETY: read-only generation, captured while the clipboard is locked.
    let sequence = unsafe { GetClipboardSequenceNumber() };
    if sequence == 0 {
        return Err(changed());
    }
    Ok(sequence)
}
pub(super) fn check(sequence: Option<u32>) -> Result<(), PlatformError> {
    // SAFETY: reads only a generation counter, not clipboard contents.
    if sequence.is_some_and(|expected| {
        expected != 0 && unsafe { GetClipboardSequenceNumber() } == expected
    }) {
        Ok(())
    } else {
        Err(PlatformError::TemporarilyUnavailable(
            "The clipboard changed before the paste shortcut; copy and paste again explicitly"
                .into(),
        ))
    }
}
