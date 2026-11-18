// SPDX-License-Identifier: Apache-2.0
//! Explicit write-only replacement of the general pasteboard. Never read,
//! retain or restore the person's previous clipboard contents.
#![allow(unsafe_code)]
use ports::desktop::PlatformError;
use std::{
    ffi::{c_char, c_void},
    time::Instant,
};

#[cfg(test)]
mod tests;

type Object = *mut c_void;
#[link(name = "AppKit", kind = "framework")]
unsafe extern "C" {}
#[link(name = "objc")]
unsafe extern "C" {
    fn objc_getClass(name: *const c_char) -> Object;
    fn sel_registerName(name: *const c_char) -> Object;
    fn objc_msgSend();
}
// Each message signature below matches the named Objective-C selector exactly.
unsafe fn object(receiver: Object, selector: &std::ffi::CStr) -> Object {
    let send: unsafe extern "C" fn(Object, Object) -> Object =
        unsafe { std::mem::transmute(objc_msgSend as *const ()) };
    unsafe { send(receiver, sel_registerName(selector.as_ptr())) }
}
unsafe fn count(board: Object) -> isize {
    let send: unsafe extern "C" fn(Object, Object) -> isize =
        unsafe { std::mem::transmute(objc_msgSend as *const ()) };
    unsafe { send(board, sel_registerName(c"changeCount".as_ptr())) }
}
struct Pool(Object);
impl Pool {
    fn new() -> Result<Self, PlatformError> {
        // SAFETY: system class and no-argument constructors, on this thread.
        let pool = unsafe {
            object(
                object(objc_getClass(c"NSAutoreleasePool".as_ptr()), c"alloc"),
                c"init",
            )
        };
        if pool.is_null() {
            return Err(unavailable());
        }
        Ok(Self(pool))
    }
}
impl Drop for Pool {
    fn drop(&mut self) {
        // SAFETY: thread-local pool; drain returns void.
        unsafe {
            let send: unsafe extern "C" fn(Object, Object) =
                std::mem::transmute(objc_msgSend as *const ());
            send(self.0, sel_registerName(c"drain".as_ptr()));
        }
    }
}
struct StringObject(Object);
impl StringObject {
    fn new(text: &str) -> Result<Self, PlatformError> {
        // No previous clipboard text is ever read. NSString owns a copy of only
        // the explicit replacement, and the temporary units are wiped on drop.
        let mut units: Vec<u16> = text.encode_utf16().collect();
        // SAFETY: live UTF-16 buffer and exact initWithCharacters:length: ABI.
        let value = unsafe {
            let allocated = object(objc_getClass(c"NSString".as_ptr()), c"alloc");
            let send: unsafe extern "C" fn(Object, Object, *const u16, usize) -> Object =
                std::mem::transmute(objc_msgSend as *const ());
            send(
                allocated,
                sel_registerName(c"initWithCharacters:length:".as_ptr()),
                units.as_ptr(),
                units.len(),
            )
        };
        // Volatile clearing, so the optimizer cannot discard the cleanup.
        for unit in &mut units {
            unsafe {
                std::ptr::write_volatile(unit, 0);
            }
        }
        if value.is_null() {
            return Err(unavailable());
        }
        Ok(Self(value))
    }
}
impl Drop for StringObject {
    fn drop(&mut self) {
        // SAFETY: owns the retained NSString from init, released once.
        unsafe {
            let send: unsafe extern "C" fn(Object, Object) =
                std::mem::transmute(objc_msgSend as *const ());
            send(self.0, sel_registerName(c"release".as_ptr()));
        }
    }
}
fn unavailable() -> PlatformError {
    PlatformError::TemporarilyUnavailable("The Mac clipboard is unavailable".into())
}
fn partial() -> PlatformError {
    PlatformError::OutcomeUnknown(
        "Clipboard replacement was started; the paste shortcut was not confirmed".into(),
    )
}
unsafe fn board() -> Object {
    unsafe {
        object(
            objc_getClass(c"NSPasteboard".as_ptr()),
            c"generalPasteboard",
        )
    }
}
pub(super) fn replace(text: &str, deadline: Instant) -> Result<isize, PlatformError> {
    let _pool = Pool::new()?;
    // SAFETY: system singleton, scoped by the autorelease pool.
    let board = unsafe { board() };
    replace_on(board, text, deadline)
}
fn replace_on(board: Object, text: &str, deadline: Instant) -> Result<isize, PlatformError> {
    let text = StringObject::new(text)?;
    let kind = StringObject::new("public.utf8-plain-text")?;
    // SAFETY: NSArray retains the supplied NSString; the surrounding pool
    // covers the autoreleased array throughout this synchronous operation.
    let types = unsafe {
        let send: unsafe extern "C" fn(Object, Object, Object) -> Object =
            std::mem::transmute(objc_msgSend as *const ());
        send(
            objc_getClass(c"NSArray".as_ptr()),
            sel_registerName(c"arrayWithObject:".as_ptr()),
            kind.0,
        )
    };
    if board.is_null() || types.is_null() || Instant::now() >= deadline {
        return Err(unavailable());
    }
    // SAFETY: declareTypes:owner: atomically replaces contents and declares our
    // string flavor. Unlike implicitly adding a type later, setString below
    // does not change ownership/generation. Return value is the new generation.
    // From this call onward failure cannot be a pre-effect refusal.
    let expected = unsafe {
        let send: unsafe extern "C" fn(Object, Object, Object, Object) -> isize =
            std::mem::transmute(objc_msgSend as *const ());
        send(
            board,
            sel_registerName(c"declareTypes:owner:".as_ptr()),
            types,
            std::ptr::null_mut(),
        )
    };
    // A detected local copy never causes republishing or retry.
    if Instant::now() >= deadline || unsafe { count(board) } != expected {
        return Err(partial());
    }
    // SAFETY: live NSString arguments; BOOL is a signed byte on Mac, including
    // Apple silicon. Only these supplied strings are copied to the pasteboard.
    let success = unsafe {
        let send: unsafe extern "C" fn(Object, Object, Object, Object) -> i8 =
            std::mem::transmute(objc_msgSend as *const ());
        send(
            board,
            sel_registerName(c"setString:forType:".as_ptr()),
            text.0,
            kind.0,
        ) != 0
    };
    if !success || unsafe { count(board) } != expected {
        return Err(partial());
    }
    Ok(expected)
}
pub(super) fn check(expected: Option<isize>) -> Result<(), PlatformError> {
    let _pool = Pool::new()?;
    // SAFETY: reads only the system generation, never clipboard content.
    let board = unsafe { board() };
    check_on(board, expected)
}
fn check_on(board: Object, expected: Option<isize>) -> Result<(), PlatformError> {
    if !board.is_null() && expected.is_some_and(|expected| unsafe { count(board) } == expected) {
        Ok(())
    } else {
        Err(PlatformError::TemporarilyUnavailable(
            "The clipboard changed before the paste shortcut; copy and paste again explicitly"
                .into(),
        ))
    }
}
