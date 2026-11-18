// SPDX-License-Identifier: Apache-2.0
//! AX window references, not window titles, are retained as catalog identities.
use super::*;
use objc2::{
    class, msg_send,
    rc::{Retained, autoreleasepool},
    runtime::AnyObject,
};
use objc2_core_foundation::{CFArray, CFGetTypeID, CFRetained, CFString, CFType, Type};
use std::{
    ffi::{CStr, c_char, c_void},
    ptr::NonNull,
    sync::Mutex,
    time::{Duration, Instant},
};

type Ref = *const c_void;
#[repr(C)]
#[derive(Default)]
struct Point {
    x: f64,
    y: f64,
}
#[repr(C)]
#[derive(Default)]
struct Size {
    width: f64,
    height: f64,
}
#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrusted() -> bool;
    fn AXUIElementCreateApplication(pid: i32) -> Ref;
    fn AXUIElementCreateSystemWide() -> Ref;
    fn AXUIElementGetPid(element: Ref, pid: *mut i32) -> i32;
    fn AXUIElementSetMessagingTimeout(element: Ref, timeout: f32) -> i32;
    fn AXUIElementCopyAttributeValue(element: Ref, attribute: Ref, value: *mut Ref) -> i32;
    fn AXUIElementPerformAction(element: Ref, action: Ref) -> i32;
    fn AXValueGetType(value: Ref) -> u32;
    fn AXValueGetTypeID() -> usize;
    fn AXValueGetValue(value: Ref, kind: u32, output: *mut c_void) -> bool;
    fn AXUIElementGetTypeID() -> usize;
}
/// A retained native value can never contain NULL. CoreFoundation's canonical
/// owner provides its retain/release operations and their exact ABI.
#[derive(Debug)]
struct Owned(CFRetained<CFType>);
impl Owned {
    unsafe fn from_raw(value: Ref) -> Option<Self> {
        NonNull::new(value.cast_mut().cast::<CFType>())
            .map(|value| Self(unsafe { CFRetained::from_raw(value) }))
    }
    fn raw(&self) -> Ref {
        CFRetained::as_ptr(&self.0).as_ptr().cast()
    }
    fn type_id(&self) -> usize {
        CFGetTypeID(Some(&self.0))
    }
    fn equal(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}
// SAFETY: AX/CF references may be retained across threads; all accesses and
// releases in this adapter are serialized by Native::state.
unsafe impl Send for Owned {}
#[derive(Debug)]
struct Application(Retained<AnyObject>);
// SAFETY: this wrapper contains only an NSRunningApplication, which AppKit
// documents as thread safe. The adapter serializes access and destruction.
unsafe impl Send for Application {}
#[derive(Debug)]
struct Entry {
    row: NativeWindow,
    window: Owned,
    pid: i32,
    application: Application,
}
#[derive(Debug, Default)]
pub(super) struct Native {
    state: Mutex<Vec<Entry>>,
}
const REMEDY: &str =
    "Give Ophio permission in System Settings > Privacy & Security > Accessibility, then try again";
fn ready() -> Result<(), PlatformError> {
    // SAFETY: reads trust only, never requests it or opens a prompt.
    if !unsafe { AXIsProcessTrusted() } {
        return Err(PlatformError::PermissionMissing(REMEDY.into()));
    }
    if crate::desktop::native::lock_state() != protocol::control::DesktopLockState::Unlocked {
        return Err(PlatformError::TemporarilyUnavailable(
            "Unlock this Mac before using its windows".into(),
        ));
    }
    if crate::desktop::native::asleep() {
        return Err(PlatformError::TemporarilyUnavailable(
            "Wake this Mac before using its windows".into(),
        ));
    }
    Ok(())
}
fn key(name: &CStr) -> Option<Owned> {
    // SAFETY: name is nul terminated, with valid static UTF-8. Allocation failure
    // stays an absent value; no owner or release is ever constructed for NULL.
    let value = unsafe { CFString::with_c_string(None, name.as_ptr(), 0x0800_0100) }?;
    // The canonical conversion preserves the string's +1 ownership as CFType.
    Some(Owned(value.into()))
}
fn attribute(element: Ref, name: &CStr) -> Result<Owned, i32> {
    let name = key(name).ok_or(-25204)?;
    let mut value = std::ptr::null();
    // SAFETY: element is a retained live AX object, key is a CF string and
    // value is an initialized output slot. Copy transfers a +1 reference.
    let code = unsafe { AXUIElementCopyAttributeValue(element, name.raw(), &raw mut value) };
    if code != 0 {
        return Err(code);
    }
    // SAFETY: AX Copy transfers +1 ownership on success. Null never constructs
    // an owning value, so even a malformed successful response is recoverable.
    unsafe { Owned::from_raw(value) }.ok_or(-25202)
}
fn ax_error(code: i32) -> PlatformError {
    match code {
        -25211 => PlatformError::PermissionMissing(REMEDY.into()),
        -25202 => PlatformError::TargetNotFound,
        _ => PlatformError::TemporarilyUnavailable(
            "The Mac app did not answer its accessibility request; try again".into(),
        ),
    }
}
fn string(value: Ref) -> Option<String> {
    // SAFETY: every value comes from a typed AX/CF copy; type checked before
    // interpreting it. Buffer length bounds metadata read and allocation.
    unsafe {
        let value = value
            .cast::<CFType>()
            .as_ref()?
            .downcast_ref::<CFString>()?;
        let mut bytes = [0u8; 8192];
        if !value.c_string(bytes.as_mut_ptr().cast(), bytes.len() as isize, 0x0800_0100) {
            return None;
        }
        Some(
            CStr::from_ptr(bytes.as_ptr().cast())
                .to_string_lossy()
                .chars()
                .take(256)
                .collect(),
        )
    }
}
fn bounds(element: Ref) -> Option<protocol::voice::VoiceBounds> {
    let position = attribute(element, c"AXPosition").ok()?;
    let size = attribute(element, c"AXSize").ok()?;
    let mut p = Point::default();
    let mut s = Size::default();
    // SAFETY: exact AXValue types checked before writing into matching C
    // layouts. AX uses top-left logical desktop coordinates.
    unsafe {
        if position.type_id() != AXValueGetTypeID()
            || size.type_id() != AXValueGetTypeID()
            || AXValueGetType(position.raw()) != 1
            || AXValueGetType(size.raw()) != 2
            || !AXValueGetValue(position.raw(), 1, (&raw mut p).cast())
            || !AXValueGetValue(size.raw(), 2, (&raw mut s).cast())
        {
            return None;
        }
    }
    (p.x.is_finite()
        && p.y.is_finite()
        && s.width.is_finite()
        && s.height.is_finite()
        && s.width > 0.
        && s.height > 0.)
        .then_some(protocol::voice::VoiceBounds {
            x: p.x,
            y: p.y,
            width: s.width,
            height: s.height,
        })
}
fn windows(element: Ref) -> Result<Vec<Owned>, PlatformError> {
    let array = attribute(element, c"AXWindows").map_err(ax_error)?;
    // SAFETY: arrays and children are type checked; children retained before
    // the owning array is released. An excessive app is refused, not guessed.
    unsafe {
        let array = array.0.downcast_ref::<CFArray>().ok_or_else(|| {
            PlatformError::Native("The app returned an invalid window list".into())
        })?;
        let count = array.count();
        if !(0..=256).contains(&count) {
            return Err(PlatformError::TemporarilyUnavailable(
                "The Mac app has too many windows to list safely".into(),
            ));
        }
        let mut result = Vec::new();
        for i in 0..count {
            let value = array.value_at_index(i);
            if let Some(value) = value.cast::<CFType>().as_ref()
                && CFGetTypeID(Some(value)) == AXUIElementGetTypeID()
            {
                result.push(Owned(value.retain()));
            }
        }
        Ok(result)
    }
}
fn focused_application() -> Option<(i32, Owned)> {
    // SAFETY: the system-wide AX object and copied focused application are
    // owned references. No content, images or input are accessed.
    unsafe {
        let system = Owned::from_raw(AXUIElementCreateSystemWide())?;
        AXUIElementSetMessagingTimeout(system.raw(), 0.15);
        let focused = attribute(system.raw(), c"AXFocusedApplication").ok()?;
        if focused.type_id() != AXUIElementGetTypeID() {
            return None;
        }
        let mut pid = 0;
        if AXUIElementGetPid(focused.raw(), &raw mut pid) != 0 || pid <= 0 {
            return None;
        }
        Some((pid, focused))
    }
}
impl Native {
    fn collect(&self, old: &[Entry]) -> Result<Vec<Entry>, PlatformError> {
        ready()?;
        autoreleasepool(|_| {
            // SAFETY: NSWorkspace and its returned objects live in this pool;
            // only documented, read-only running-app properties are queried.
            unsafe {
                let workspace: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
                if workspace.is_null() {
                    return Err(PlatformError::TemporarilyUnavailable(
                        "No Mac application session is available".into(),
                    ));
                }
                let apps: *mut AnyObject = msg_send![workspace, runningApplications];
                if apps.is_null() {
                    return Err(PlatformError::TemporarilyUnavailable(
                        "The Mac app list is unavailable".into(),
                    ));
                }
                let count: usize = msg_send![apps, count];
                if count > 1024 {
                    return Err(PlatformError::TemporarilyUnavailable(
                        "Too many running Mac apps to list safely".into(),
                    ));
                }
                let deadline = Instant::now() + Duration::from_millis(1500);
                let foreground = focused_application().map(|(pid, _)| pid);
                let mut result = Vec::new();
                for i in 0..count {
                    if Instant::now() >= deadline {
                        return Err(PlatformError::TemporarilyUnavailable(
                            "The Mac window list took too long; try again".into(),
                        ));
                    }
                    let app: *mut AnyObject = msg_send![apps,objectAtIndex:i];
                    let policy: isize = msg_send![app, activationPolicy];
                    if policy != 0 {
                        continue;
                    } // regular apps only, not agents
                    let pid: i32 = msg_send![app, processIdentifier];
                    if pid <= 0 {
                        continue;
                    }
                    let name: *mut AnyObject = msg_send![app, localizedName];
                    let app_name = if name.is_null() {
                        continue;
                    } else {
                        let bytes: *const c_char = msg_send![name, UTF8String];
                        if bytes.is_null() {
                            continue;
                        };
                        CStr::from_ptr(bytes)
                            .to_string_lossy()
                            .chars()
                            .take(256)
                            .collect::<String>()
                    };
                    let active = foreground == Some(pid);
                    let Some(root) = Owned::from_raw(AXUIElementCreateApplication(pid)) else {
                        continue;
                    };
                    if AXUIElementSetMessagingTimeout(root.raw(), 0.15) != 0 {
                        continue;
                    };
                    let focused = attribute(root.raw(), c"AXFocusedWindow").ok();
                    let children = match windows(root.raw()) {
                        Ok(windows) => windows,
                        Err(PlatformError::PermissionMissing(reason)) => {
                            return Err(PlatformError::PermissionMissing(reason));
                        }
                        Err(_) => continue, // an exiting/unresponsive app needn't hide other apps
                    };
                    for window in children {
                        if result.len() >= 1024 || Instant::now() >= deadline {
                            return Err(PlatformError::TemporarilyUnavailable(
                                "The Mac window list exceeds its safe limit; try again".into(),
                            ));
                        }
                        AXUIElementSetMessagingTimeout(window.raw(), 0.15);
                        let Some(bounds) = bounds(window.raw()) else {
                            continue;
                        };
                        let title = attribute(window.raw(), c"AXTitle")
                            .ok()
                            .and_then(|v| string(v.raw()))
                            .unwrap_or_default();
                        let identity = old
                            .iter()
                            .find(|e| {
                                let same_process: bool = msg_send![&*e.application.0, isEqual:app];
                                e.pid == pid && same_process && e.window.equal(&window)
                            })
                            .map(|e| e.row.identity.clone())
                            .unwrap_or_else(|| protocol::CommandId::new().to_string());
                        let focused = active && focused.as_ref().is_some_and(|f| f.equal(&window));
                        result.push(Entry {
                            application: Application(
                                Retained::retain(app).ok_or(PlatformError::TargetNotFound)?,
                            ),
                            row: NativeWindow {
                                identity,
                                pid,
                                app: app_name.clone(),
                                title,
                                focused,
                                bounds,
                            },
                            window,
                            pid,
                        });
                    }
                }
                ready()?;
                Ok(result)
            }
        })
    }
}
impl Driver for Native {
    fn availability(&self) -> Availability {
        match ready() {
            Ok(()) => Availability::Available,
            Err(PlatformError::PermissionMissing(remedy)) => {
                Availability::PermissionMissing { remedy }
            }
            Err(error) => Availability::TemporarilyUnavailable {
                reason: error.to_string(),
            },
        }
    }
    fn list(&self) -> Result<Vec<NativeWindow>, PlatformError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| PlatformError::Native("The Mac window catalog stopped".into()))?;
        let next = match self.collect(&state) {
            Ok(next) => next,
            Err(error) => {
                state.clear();
                return Err(error);
            }
        };
        *state = next;
        Ok(state.iter().map(|e| e.row.clone()).collect())
    }
    fn act(
        &self,
        expected: &NativeWindow,
        close: bool,
        authority: &dyn WindowAuthority,
    ) -> Result<(), PlatformError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| PlatformError::TargetNotFound)?;
        let next = self.collect(&state)?;
        *state = next;
        let entry = state
            .iter()
            .find(|e| e.row.identity == expected.identity && e.row.app == expected.app)
            .ok_or(PlatformError::TargetNotFound)?;
        autoreleasepool(|_| {
            // SAFETY: the window is retained and freshly resolved in this
            // serialized catalog. No pointer/selector comes from a request.
            unsafe {
                let application: *mut AnyObject = msg_send![class!(NSRunningApplication),runningApplicationWithProcessIdentifier:entry.pid];
                if application.is_null() {
                    return Err(PlatformError::TargetNotFound);
                }
                let same_process: bool = msg_send![&*entry.application.0, isEqual:application];
                if !same_process {
                    return Err(PlatformError::TargetNotFound);
                }
                if close {
                    let button =
                        attribute(entry.window.raw(), c"AXCloseButton").map_err(ax_error)?;
                    if button.type_id() != AXUIElementGetTypeID() {
                        return Err(PlatformError::TargetNotFound);
                    }
                    let press = key(c"AXPress").ok_or_else(|| {
                        PlatformError::Native("Could not prepare the close request".into())
                    })?;
                    ready()?;
                    authority.check()?;
                    let result = AXUIElementPerformAction(button.raw(), press.raw());
                    if result == -25211 {
                        return Err(PlatformError::PermissionMissing(REMEDY.into()));
                    }
                    if result == -25202 {
                        return Err(PlatformError::TargetNotFound);
                    }
                    if result != 0 {
                        return Err(PlatformError::OutcomeUnknown(
                            "The Mac app has not confirmed closing this window".into(),
                        ));
                    }
                    return Ok(());
                }
                let raise = key(c"AXRaise").ok_or_else(|| {
                    PlatformError::Native("Could not prepare the focus request".into())
                })?;
                ready()?;
                authority.check()?;
                let activated: bool = msg_send![application,activateWithOptions:0usize];
                if !activated {
                    return Err(PlatformError::OutcomeUnknown(
                        "macOS has not confirmed activating this app".into(),
                    ));
                }
                // Activation is already an effect: a Stop from here must never
                // be reported as though nothing happened.
                ready().and_then(|_| authority.check()).map_err(|_| {
                    PlatformError::OutcomeUnknown(
                        "Control ended while macOS was focusing the window".into(),
                    )
                })?;
                if AXUIElementPerformAction(entry.window.raw(), raise.raw()) != 0 {
                    return Err(PlatformError::OutcomeUnknown(
                        "macOS has not confirmed raising this window".into(),
                    ));
                }
                let deadline = Instant::now() + Duration::from_millis(500);
                loop {
                    if let Some((pid, focused_app)) = focused_application()
                        && pid == entry.pid
                        && attribute(focused_app.raw(), c"AXFocusedWindow")
                            .ok()
                            .is_some_and(|f| f.equal(&entry.window))
                    {
                        return Ok(());
                    }
                    if Instant::now() >= deadline {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(PlatformError::OutcomeUnknown(
                    "macOS has not confirmed that this window received focus".into(),
                ))
            }
        })
    }
}

#[cfg(test)]
mod ownership_tests {
    use super::*;

    #[test]
    fn mac_window_native_null_create_never_constructs_an_owner() {
        // This exercises only NULL rejection, never a desktop or AX query.
        assert!(unsafe { Owned::from_raw(std::ptr::null()) }.is_none());
    }

    #[test]
    fn mac_window_native_cf_metadata_has_owned_typed_identity() {
        let first = key(c"native fixture label").expect("CF string");
        let second = key(c"native fixture label").expect("CF string");
        assert!(first.equal(&second));
        assert_eq!(string(first.raw()).as_deref(), Some("native fixture label"));
        assert!(string(std::ptr::null()).is_none());
        let boolean = objc2_core_foundation::CFBoolean::new(false);
        assert!(string((boolean as *const objc2_core_foundation::CFBoolean).cast()).is_none());
        drop(first);
        assert_eq!(
            string(second.raw()).as_deref(),
            Some("native fixture label")
        );
    }
}
