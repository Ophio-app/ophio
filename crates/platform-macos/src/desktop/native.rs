// SPDX-License-Identifier: Apache-2.0
//! The system's own calls for screens, permissions and the lock.
#![allow(unsafe_code)]

use platform_windows::desktop::geometry::Monitor;
use ports::desktop::PlatformError;
use protocol::control::DesktopLockState;
use std::ffi::{CStr, c_char, c_int, c_void};

#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct CGPoint {
    pub x: f64,
    pub y: f64,
}
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct CGSize {
    pub width: f64,
    pub height: f64,
}
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct CGRect {
    pub origin: CGPoint,
    pub size: CGSize,
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGGetActiveDisplayList(max: u32, displays: *mut u32, count: *mut u32) -> i32;
    fn CGGetOnlineDisplayList(max: u32, displays: *mut u32, count: *mut u32) -> i32;
    fn CGDisplayIsAsleep(display: u32) -> u32;
    fn CGDisplayBounds(display: u32) -> CGRect;
    fn CGDisplayIsBuiltin(display: u32) -> c_int;
    fn CGDisplayUnitNumber(display: u32) -> u32;
    fn CGPreflightScreenCaptureAccess() -> bool;
    fn CGRequestScreenCaptureAccess() -> bool;
    fn CGSessionCopyCurrentDictionary() -> *const c_void;
}
#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrusted() -> bool;
    fn AXIsProcessTrustedWithOptions(options: *const c_void) -> bool;
    static kAXTrustedCheckOptionPrompt: *const c_void;
}
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(value: *const c_void);
    fn CFStringCreateWithCString(
        allocator: *const c_void,
        text: *const c_char,
        encoding: u32,
    ) -> *const c_void;
    fn CFDictionaryGetValue(dictionary: *const c_void, key: *const c_void) -> *const c_void;
    fn CFBooleanGetValue(value: *const c_void) -> bool;
    fn CFDictionaryCreate(
        allocator: *const c_void,
        keys: *const *const c_void,
        values: *const *const c_void,
        count: isize,
        key_callbacks: *const c_void,
        value_callbacks: *const c_void,
    ) -> *const c_void;
    static kCFBooleanTrue: *const c_void;
    static kCFTypeDictionaryKeyCallBacks: c_void;
    static kCFTypeDictionaryValueCallBacks: c_void;
}

const UTF8: u32 = 0x0800_0100;
const MAX_DISPLAYS: usize = 64;

/// The screens now, by id.
pub(super) fn displays() -> Result<Vec<u32>, PlatformError> {
    let mut ids = [0u32; MAX_DISPLAYS];
    let mut count = 0u32;
    // SAFETY: the pointer and maximum describe `ids`; `count` outlives the call.
    let failed =
        unsafe { CGGetActiveDisplayList(MAX_DISPLAYS as u32, ids.as_mut_ptr(), &raw mut count) };
    if failed != 0 {
        return Err(PlatformError::TemporarilyUnavailable(
            "This Mac's screens can't be listed from this session".into(),
        ));
    }
    Ok(ids[..(count as usize).min(MAX_DISPLAYS)].to_vec())
}

/// What the phone is told while the Mac's screen sleeps.
pub(super) const ASLEEP: &str = "The Mac’s screen is asleep. Wake it to see it.";

/// Whether the Mac's screens are connected but asleep, so none is active: a
/// Mac whose display slept lists no screen at all until it wakes.
pub(super) fn asleep() -> bool {
    let mut ids = [0u32; MAX_DISPLAYS];
    let mut count = 0u32;
    let mut active = 0u32;
    // SAFETY: the pointers and maximum describe `ids`, `count` and `active`,
    // which outlive the calls; a null list with a maximum of 0 only counts.
    unsafe {
        if CGGetActiveDisplayList(0, std::ptr::null_mut(), &raw mut active) != 0 || active > 0 {
            return false;
        }
        if CGGetOnlineDisplayList(MAX_DISPLAYS as u32, ids.as_mut_ptr(), &raw mut count) != 0 {
            return false;
        }
        let online = &ids[..(count as usize).min(MAX_DISPLAYS)];
        !online.is_empty()
            && online
                .iter()
                .all(|&display| CGDisplayIsAsleep(display) != 0)
    }
}

/// What a screen is called, the same for as long as it stays connected.
pub(super) fn display_name(display: u32, builtin: bool, unit: u32) -> String {
    if builtin {
        "Built-in screen".into()
    } else {
        format!("Screen {} ({display})", unit + 1)
    }
}

pub(super) fn named(display: u32) -> String {
    // SAFETY: plain queries by id, which answer nothing for one that has gone.
    let (builtin, unit) = unsafe {
        (
            CGDisplayIsBuiltin(display) != 0,
            CGDisplayUnitNumber(display),
        )
    };
    display_name(display, builtin, unit)
}

/// The screens as the shared desktop code works with them. Points stand in for
/// its pixels, at the scale that says one is one.
pub(super) fn monitors() -> Result<Vec<Monitor>, PlatformError> {
    let list = displays()?;
    if list.is_empty() {
        return Err(if asleep() {
            PlatformError::ScreenAsleep(ASLEEP.into())
        } else {
            PlatformError::TemporarilyUnavailable("This Mac has no screen turned on".into())
        });
    }
    Ok(list
        .into_iter()
        .map(|display| {
            // SAFETY: a plain query by id.
            let bounds = unsafe { CGDisplayBounds(display) };
            Monitor {
                name: named(display),
                x: bounds.origin.x.round() as i32,
                y: bounds.origin.y.round() as i32,
                width: bounds.size.width.round() as u32,
                height: bounds.size.height.round() as u32,
                dpi: 96,
                // The system gives frames and positions already turned.
                rotation: 0,
            }
        })
        .collect())
}

/// Whether this program may record the screen, and may control the computer.
pub fn permissions() -> (bool, bool) {
    // SAFETY: both only read this program's standing.
    unsafe { (CGPreflightScreenCaptureAccess(), AXIsProcessTrusted()) }
}

/// Has the system ask the person for each permission not given yet. Only an
/// explicit command calls this: it puts a dialog on the Mac's screen.
pub fn ask_for_permissions() -> (bool, bool) {
    // SAFETY: the dictionary is made from constants with their own callbacks and
    // released after the call that reads it.
    unsafe {
        let screen = CGPreflightScreenCaptureAccess() || CGRequestScreenCaptureAccess();
        let keys = [kAXTrustedCheckOptionPrompt];
        let values = [kCFBooleanTrue];
        let options = CFDictionaryCreate(
            std::ptr::null(),
            keys.as_ptr(),
            values.as_ptr(),
            1,
            (&raw const kCFTypeDictionaryKeyCallBacks).cast(),
            (&raw const kCFTypeDictionaryValueCallBacks).cast(),
        );
        let input = AXIsProcessTrustedWithOptions(options);
        if !options.is_null() {
            CFRelease(options);
        }
        (screen, input)
    }
}

/// Whether the screen is locked, as the window system says for this session. No
/// session at all, as from a connection with no one logged in, says nothing.
pub(super) fn lock_state() -> DesktopLockState {
    const LOCKED: &CStr = c"CGSSessionScreenIsLocked";
    // SAFETY: the dictionary is ours to release; the key string is released after
    // the lookup, and the value is read while the dictionary holds it.
    unsafe {
        let session = CGSessionCopyCurrentDictionary();
        if session.is_null() {
            return DesktopLockState::Unknown;
        }
        let key = CFStringCreateWithCString(std::ptr::null(), LOCKED.as_ptr(), UTF8);
        let state = if key.is_null() {
            DesktopLockState::Unknown
        } else {
            let value = CFDictionaryGetValue(session, key);
            CFRelease(key);
            // The key is there only while it is locked.
            if !value.is_null() && CFBooleanGetValue(value) {
                DesktopLockState::Locked
            } else {
                DesktopLockState::Unlocked
            }
        };
        CFRelease(session);
        state
    }
}

/// Locks the screen. The system's own call for it isn't public, so it is looked
/// up, and if it has gone the display is put to sleep, which locks a Mac set to
/// ask for its password at once. Only what the system then reports counts.
pub(super) fn lock() -> Result<(), PlatformError> {
    const LOGIN: &CStr =
        c"/System/Library/PrivateFrameworks/login.framework/Versions/Current/login";
    const LOCK: &CStr = c"SACLockScreenImmediate";
    // SAFETY: both names are nul-terminated; a symbol found under this name takes
    // no arguments and returns an int in every system that has it.
    unsafe {
        let library = libc::dlopen(LOGIN.as_ptr(), libc::RTLD_LAZY);
        if !library.is_null() {
            let symbol = libc::dlsym(library, LOCK.as_ptr());
            if !symbol.is_null() {
                let lock: extern "C" fn() -> c_int = std::mem::transmute(symbol);
                lock();
            }
        }
    }
    let locked = || lock_state() == DesktopLockState::Locked;
    for _ in 0..20 {
        if locked() {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let slept = std::process::Command::new("/usr/bin/pmset")
        .arg("displaysleepnow")
        .status()
        .is_ok_and(|status| status.success());
    if slept {
        for _ in 0..20 {
            if locked() {
                return Ok(());
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }
    Err(PlatformError::Native(
        "This Mac didn't lock. Lock it at the computer.".into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_screen_is_named_for_people_and_told_from_the_others() {
        assert_eq!(display_name(1, true, 0), "Built-in screen");
        assert_eq!(display_name(7, false, 1), "Screen 2 (7)");
        assert_ne!(display_name(7, false, 1), display_name(8, false, 1));
    }
}
