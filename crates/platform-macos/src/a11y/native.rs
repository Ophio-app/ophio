// SPDX-License-Identifier: Apache-2.0
//! Minimal AX/CF boundary. Every CF value is type-checked before use; every
//! Create/Copy/retain has one owner. No AXValue (field value), text, selected
//! text, or password-value attribute is requested.
#![allow(unsafe_code)]
use super::*;
use objc2_core_foundation::{
    CFArray, CFBoolean, CFDictionary, CFEqual, CFGetTypeID, CFRetained, CFString, CFType,
    ConcreteType, kCFBooleanTrue,
};
use protocol::voice::VoiceBounds;
use std::{
    collections::VecDeque,
    ffi::c_void,
    ptr::{self, NonNull},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};
type Ref = *const c_void;
const LIMIT: usize = 256;
const DEPTH: usize = 24;
const BUDGET: Duration = Duration::from_millis(1500);
const MESSAGE_TIMEOUT: f32 = 0.04;
const UTF8: u32 = 0x0800_0100;
static INSTANCE: AtomicU64 = AtomicU64::new(1);
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
    fn AXUIElementGetTypeID() -> usize;
    fn AXUIElementCreateSystemWide() -> Ref;
    fn AXUIElementSetMessagingTimeout(element: Ref, timeout: f32) -> i32;
    fn AXUIElementGetPid(element: Ref, pid: *mut i32) -> i32;
    fn AXUIElementCopyAttributeValue(element: Ref, attribute: Ref, value: *mut Ref) -> i32;
    fn AXUIElementCopyAttributeValues(
        element: Ref,
        attribute: Ref,
        index: isize,
        max: isize,
        values: *mut Ref,
    ) -> i32;
    fn AXUIElementCopyActionNames(element: Ref, actions: *mut Ref) -> i32;
    fn AXUIElementIsAttributeSettable(element: Ref, attribute: Ref, settable: *mut u8) -> i32;
    fn AXUIElementPerformAction(element: Ref, action: Ref) -> i32;
    fn AXUIElementSetAttributeValue(element: Ref, attribute: Ref, value: Ref) -> i32;
    fn AXValueGetTypeID() -> usize;
    fn AXValueGetType(value: Ref) -> u32;
    fn AXValueGetValue(value: Ref, kind: u32, output: *mut c_void) -> bool;
}
#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGSessionCopyCurrentDictionary() -> Ref;
    fn CGGetActiveDisplayList(max: u32, displays: *mut u32, count: *mut u32) -> i32;
}
#[derive(Clone)]
struct Owned(CFRetained<CFType>);
impl Owned {
    fn take(value: Ref) -> Result<Self, Error> {
        // Check before adopting ownership: never construct/drop a null CF owner.
        let value = NonNull::new(value.cast_mut().cast::<CFType>()).ok_or_else(unavailable)?;
        // SAFETY: callers pass the +1 result of a CF/AX Create/Copy function.
        Ok(Self(unsafe { CFRetained::from_raw(value) }))
    }
    fn raw(&self) -> Ref {
        CFRetained::as_ptr(&self.0).as_ptr().cast()
    }
    fn typed(&self, kind: usize) -> Result<(), Error> {
        if CFGetTypeID(Some(&self.0)) == kind {
            Ok(())
        } else {
            Err(unavailable())
        }
    }
    fn downcast<T: ConcreteType>(&self) -> Result<&T, Error> {
        self.0.downcast_ref().ok_or_else(unavailable)
    }
    fn equal(&self, other: &Self) -> bool {
        CFEqual(Some(&self.0), Some(&other.0))
    }
    fn retained(value: Ref) -> Result<Self, Error> {
        let value = NonNull::new(value.cast_mut().cast::<CFType>()).ok_or_else(unavailable)?;
        // SAFETY: caller passes a live borrowed CF value; retain owns one count.
        Ok(Self(unsafe { CFRetained::retain(value) }))
    }
}
fn unavailable() -> Error {
    Error::Unavailable("The focused Mac app did not expose valid accessibility metadata".into())
}
fn status(code: i32) -> Result<(), Error> {
    match code {
        0 => Ok(()),
        -25211 => Err(Error::Unavailable(PERMISSION.into())),
        -25202 => Err(Error::StaleTarget),
        _ => Err(unavailable()),
    }
}
fn key(name: &str) -> Result<Owned, Error> {
    let text = std::ffi::CString::new(name).map_err(|_| unavailable())?;
    // SAFETY: a valid NUL-terminated UTF-8 string, copied with the default allocator.
    let value =
        unsafe { CFString::with_c_string(None, text.as_ptr(), UTF8) }.ok_or_else(unavailable)?;
    Ok(Owned(value.into()))
}
fn string(value: &Owned) -> Result<String, Error> {
    let value = value.downcast::<CFString>()?;
    // Bound untrusted application metadata allocation after the checked downcast.
    let length = value.length();
    if !(0..=4096).contains(&length) {
        return Err(unavailable());
    }
    let mut bytes = vec![0u8; length as usize * 4 + 1];
    // SAFETY: the buffer covers the advertised size and remains live for the call.
    if !unsafe { value.c_string(bytes.as_mut_ptr().cast(), bytes.len() as isize, UTF8) } {
        return Err(unavailable());
    }
    let end = bytes.iter().position(|c| *c == 0).ok_or_else(unavailable)?;
    String::from_utf8(bytes[..end].to_vec()).map_err(|_| unavailable())
}
fn boolean(value: &Owned) -> Result<bool, Error> {
    Ok(value.downcast::<CFBoolean>()?.value())
}
fn array(value: &Owned, limit: usize) -> Result<Vec<Owned>, Error> {
    let value = value.downcast::<CFArray>()?;
    let count = value.count();
    if count < 0 {
        return Err(unavailable());
    }
    (0..(count as usize).min(limit))
        .map(|i| {
            // SAFETY: index is bounded by the checked CFArray count. The AX/CF
            // APIs used here return arrays of CF objects, retained before return.
            Owned::retained(unsafe { value.value_at_index(i as isize) })
        })
        .collect()
}
fn bounded(mut s: String, max: usize) -> String {
    let mut end = s.len().min(max);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s.truncate(end);
    s
}
type Entry = Retained<Owned>;
struct Front {
    pid: i32,
    window: Owned,
}
struct Prepared {
    id: String,
    object: Owned,
    press: bool,
    focus: bool,
}
pub(super) struct Native {
    instance: u64,
    serial: u64,
    entries: Vec<Entry>,
    front: Option<Front>,
    context: String,
    window: Option<VoiceBounds>,
    prepared: Option<Prepared>,
    deadline: Instant,
    press: Owned,
    focused: Owned,
    #[cfg(test)]
    only_pid: Option<i32>,
}
impl Native {
    pub(super) fn new() -> Result<Self, Error> {
        // Constants only: construction never observes the desktop or prompts.
        Ok(Self {
            instance: INSTANCE.fetch_add(1, Ordering::Relaxed),
            serial: 0,
            entries: vec![],
            front: None,
            context: String::new(),
            window: None,
            prepared: None,
            deadline: Instant::now(),
            press: key("AXPress")?,
            focused: key("AXFocused")?,
            #[cfg(test)]
            only_pid: None,
        })
    }
    fn budget(&self) -> Result<(), Error> {
        if Instant::now() >= self.deadline {
            Err(Error::Unavailable(SLOW.into()))
        } else {
            Ok(())
        }
    }
    fn session(&self) -> Result<(), Error> {
        self.budget()?;
        // SAFETY: non-prompting trust query; copied session dictionary is owned
        // and all dictionary values are retained and checked before use.
        unsafe {
            if !AXIsProcessTrusted() {
                return Err(Error::Unavailable(PERMISSION.into()));
            }
            let session = Owned::take(CGSessionCopyCurrentDictionary())
                .map_err(|_| Error::Unavailable(SESSION.into()))?;
            let session = session.downcast::<CFDictionary>()?;
            let get = |name: &str| -> Result<Option<bool>, Error> {
                let k = key(name)?;
                let value = session.value(k.raw());
                if value.is_null() {
                    Ok(None)
                } else {
                    boolean(&Owned::retained(value)?).map(Some)
                }
            };
            // The lock marker is absent when unlocked. Missing/ill-typed positive
            // evidence for an active logged-in console session is refused.
            if get("kCGSessionOnConsoleKey")? != Some(true)
                || get("kCGSessionLoginDoneKey")? != Some(true)
                || get("CGSSessionScreenIsLocked")?.unwrap_or(false)
            {
                return Err(Error::Unavailable(SESSION.into()));
            }
            let mut active = 0;
            if CGGetActiveDisplayList(0, ptr::null_mut(), &mut active) != 0 || active == 0 {
                return Err(Error::Unavailable(SESSION.into()));
            }
        }
        Ok(())
    }
    fn ax(&self, object: &Owned) -> Result<(), Error> {
        self.budget()?;
        // SAFETY: type check precedes all AX messaging. Bound each AX IPC call.
        unsafe {
            object.typed(AXUIElementGetTypeID())?;
            status(AXUIElementSetMessagingTimeout(
                object.raw(),
                MESSAGE_TIMEOUT,
            ))
        }
    }
    fn attr(&self, object: &Owned, name: &str) -> Result<Option<Owned>, Error> {
        self.ax(object)?;
        let attribute = key(name)?;
        let mut out = ptr::null();
        // SAFETY: object is AX; output receives a +1 CF value on success.
        let code =
            unsafe { AXUIElementCopyAttributeValue(object.raw(), attribute.raw(), &mut out) };
        // Own even an unexpected non-null error output, so it cannot leak.
        let owned = if out.is_null() {
            None
        } else {
            Some(Owned::take(out)?)
        };
        if matches!(code, -25205 | -25212) {
            return Ok(None);
        }
        status(code)?;
        owned.ok_or_else(unavailable).map(Some)
    }
    fn bool_attr(&self, object: &Owned, name: &str) -> Result<Option<bool>, Error> {
        self.attr(object, name)?.as_ref().map(boolean).transpose()
    }
    fn front(&self) -> Result<Front, Error> {
        self.session()?;
        // SAFETY: Create returns an owned AX object or null.
        let system = Owned::take(unsafe { AXUIElementCreateSystemWide() })?;
        let app = self
            .attr(&system, "AXFocusedApplication")?
            .ok_or_else(unavailable)?;
        self.ax(&app)?;
        let mut pid = 0;
        // SAFETY: app is AX and pid is writable.
        status(unsafe { AXUIElementGetPid(app.raw(), &mut pid) })?;
        let window = self
            .attr(&app, "AXFocusedWindow")?
            .ok_or_else(|| Error::Unavailable("No focused accessible Mac window".into()))?;
        self.ax(&window)?;
        let mut window_pid = 0;
        // SAFETY: window is AX and pid is writable.
        status(unsafe { AXUIElementGetPid(window.raw(), &mut window_pid) })?;
        if pid <= 0 || window_pid != pid {
            return Err(Error::StaleTarget);
        }
        Ok(Front { pid, window })
    }
    fn bounds(&self, object: &Owned) -> Result<VoiceBounds, Error> {
        let position = self.attr(object, "AXPosition")?.ok_or_else(unavailable)?;
        let size = self.attr(object, "AXSize")?.ok_or_else(unavailable)?;
        let mut p = Point::default();
        let mut s = Size::default();
        // SAFETY: check both CF type and AXValue subtype before writing into
        // correctly sized/repr(C) CGPoint and CGSize buffers.
        unsafe {
            position.typed(AXValueGetTypeID())?;
            size.typed(AXValueGetTypeID())?;
            if AXValueGetType(position.raw()) != 1
                || AXValueGetType(size.raw()) != 2
                || !AXValueGetValue(position.raw(), 1, (&mut p as *mut Point).cast())
                || !AXValueGetValue(size.raw(), 2, (&mut s as *mut Size).cast())
            {
                return Err(unavailable());
            }
        }
        if ![p.x, p.y, s.width, s.height].iter().all(|n| n.is_finite()) {
            return Err(unavailable());
        }
        Ok(VoiceBounds {
            x: p.x,
            y: p.y,
            width: s.width,
            height: s.height,
        })
    }
    fn children(&self, object: &Owned) -> Result<Vec<Owned>, Error> {
        self.ax(object)?;
        let children = key("AXChildren")?;
        let mut out = ptr::null();
        // SAFETY: object is AX, output is owned, and copy is bounded at source.
        let code = unsafe {
            AXUIElementCopyAttributeValues(
                object.raw(),
                children.raw(),
                0,
                LIMIT as isize,
                &mut out,
            )
        };
        let owned = if out.is_null() {
            None
        } else {
            Some(Owned::take(out)?)
        };
        if matches!(code, -25205 | -25212) {
            return Ok(vec![]);
        }
        status(code)?;
        let values = array(&owned.ok_or_else(unavailable)?, LIMIT)?;
        for value in &values {
            self.ax(value)?;
        }
        Ok(values)
    }
    fn capabilities(&self, object: &Owned) -> Result<(bool, bool), Error> {
        self.ax(object)?;
        let mut names = ptr::null();
        // SAFETY: checked AX object; copied CFArray is owned and type checked.
        let code = unsafe { AXUIElementCopyActionNames(object.raw(), &mut names) };
        let names = if names.is_null() {
            None
        } else {
            Some(Owned::take(names)?)
        };
        let mut press = false;
        if code == 0 {
            for name in array(&names.ok_or_else(unavailable)?, 32)? {
                if string(&name)? == "AXPress" {
                    press = true;
                }
            }
        } else if code != -25206 {
            status(code)?;
        }
        self.ax(object)?;
        let mut focus = 0;
        // SAFETY: checked AX object, valid constant attribute and writable Boolean.
        let code =
            unsafe { AXUIElementIsAttributeSettable(object.raw(), self.focused.raw(), &mut focus) };
        if code != 0 && code != -25205 {
            status(code)?;
        }
        Ok((press, code == 0 && focus != 0))
    }
    fn metadata(
        &self,
        object: &Owned,
        id: &str,
        ancestor_visible: bool,
    ) -> Result<Option<(VoiceElement, bool, bool)>, Error> {
        let role = self
            .attr(object, "AXRole")?
            .as_ref()
            .map(string)
            .transpose()?
            .unwrap_or_default();
        // Static text/document content is not a control label and is never read.
        if matches!(role.as_str(), "AXStaticText" | "AXWebArea" | "AXUnknown") {
            return Ok(None);
        }
        let (press, focus) = self.capabilities(object)?;
        if !press && !focus {
            return Ok(None);
        }
        let subrole = self
            .attr(object, "AXSubrole")?
            .as_ref()
            .map(string)
            .transpose()?
            .unwrap_or_default();
        let name = match self.attr(object, "AXTitle")? {
            Some(title) => string(&title)?,
            None => self
                .attr(object, "AXDescription")?
                .as_ref()
                .map(string)
                .transpose()?
                .unwrap_or_default(),
        };
        let bounds = self.bounds(object)?;
        let intersects = self.window.as_ref().is_some_and(|w| {
            bounds.x < w.x + w.width
                && bounds.x + bounds.width > w.x
                && bounds.y < w.y + w.height
                && bounds.y + bounds.height > w.y
        });
        let visible = ancestor_visible
            && !self.bool_attr(object, "AXHidden")?.unwrap_or(false)
            && intersects
            && bounds.width > 0.
            && bounds.height > 0.;
        let sensitive =
            role.contains("Secure") || subrole.contains("Secure") || private_label(&name);
        let destructive = destructive_label(&name);
        let role = match role.as_str() {
            "AXButton" => "button",
            "AXCheckBox" => "checkbox",
            "AXRadioButton" => "radio button",
            "AXTextField" | "AXTextArea" | "AXSecureTextField" => "textbox",
            "AXLink" => "link",
            "AXPopUpButton" | "AXComboBox" => "combobox",
            "AXMenuItem" => "menu item",
            "AXSlider" => "slider",
            "AXTabGroup" => "tab",
            _ => role.as_str(),
        };
        Ok(Some((
            VoiceElement {
                id: id.into(),
                role: bounded(role.into(), 64),
                name: bounded(name, 256),
                bounds,
                visible,
                enabled: self.bool_attr(object, "AXEnabled")?.unwrap_or(false),
                focused: self.bool_attr(object, "AXFocused")?.unwrap_or(false),
                sensitive,
                destructive,
            },
            press,
            focus,
        )))
    }
    fn identity(&mut self, object: &Owned, new: &[Entry]) -> String {
        if let Some(id) = retained_id(object, new, &self.entries, Owned::equal) {
            return id;
        }
        self.serial += 1;
        format!("ax{:x}-{:x}", self.instance, self.serial)
    }
    fn collect(&mut self) -> Result<Frame, Error> {
        self.deadline = Instant::now() + BUDGET;
        self.prepared = None;
        let front = self.front()?;
        #[cfg(test)]
        if self.only_pid.is_some_and(|pid| pid != front.pid) {
            return Err(Error::StaleTarget);
        }
        self.window = Some(self.bounds(&front.window)?);
        let window_id = self.identity(&front.window, &[]);
        let context = format!("{}:{}", front.pid, window_id);
        let mut queue = VecDeque::from([(front.window.clone(), 0usize, true)]);
        let mut entries: Vec<Entry> = vec![];
        let mut elements = vec![];
        while let Some((object, depth, parent_visible)) = queue.pop_front() {
            self.budget()?;
            if entries.iter().any(|e| e.object.equal(&object)) {
                continue;
            }
            if entries.len() >= LIMIT {
                break;
            }
            let id = if depth == 0 {
                window_id.clone()
            } else {
                self.identity(&object, &entries)
            };
            let visible = parent_visible && !self.bool_attr(&object, "AXHidden")?.unwrap_or(false);
            if let Some((row, _, _)) = self.metadata(&object, &id, visible)? {
                elements.push(row);
            }
            if depth < DEPTH {
                // Queue allocation, node count and depth are all bounded, even
                // for cycles or apps exposing enormous child lists.
                for child in self
                    .children(&object)?
                    .into_iter()
                    .take(LIMIT.saturating_sub(queue.len() + entries.len()))
                {
                    queue.push_back((child, depth + 1, visible));
                }
            }
            entries.push(Entry { id, object });
        }
        // Do not publish a tree observed while focus or lock state changed.
        let end = self.front()?;
        if end.pid != front.pid || !end.window.equal(&front.window) {
            return Err(Error::StaleTarget);
        }
        self.entries = entries;
        self.front = Some(front);
        self.context = context.clone();
        Ok(Frame {
            context,
            elements,
            window: self.window,
        })
    }
}
impl Backend for Native {
    fn frame(&mut self) -> Result<Frame, Error> {
        let result = self.collect();
        if result.is_err() {
            // Permission recovery cannot resurrect references from before denial.
            self.entries.clear();
            self.front = None;
            self.prepared = None;
        }
        result
    }
    fn resolve(&mut self, expected: &VoiceElement, context: &str) -> Result<(), Error> {
        self.prepared = None;
        if context != self.context {
            return Err(Error::StaleTarget);
        }
        let object = self
            .entries
            .iter()
            .find(|e| e.id == expected.id)
            .ok_or(Error::StaleTarget)?
            .object
            .clone();
        let (fresh, press, focus) = self
            .metadata(&object, &expected.id, true)?
            .ok_or(Error::StaleTarget)?;
        if fresh != *expected {
            return Err(Error::StaleTarget);
        }
        let front = self.front()?;
        let old = self.front.as_ref().ok_or(Error::StaleTarget)?;
        if front.pid != old.pid || !front.window.equal(&old.window) {
            return Err(Error::StaleTarget);
        }
        self.budget()?;
        self.prepared = Some(Prepared {
            id: expected.id.clone(),
            object,
            press,
            focus,
        });
        Ok(())
    }
    fn perform(&mut self, id: &str, focus: bool, authority: &dyn Authority) -> Result<bool, Error> {
        let prepared = self.prepared.take().ok_or(Error::StaleTarget)?;
        if prepared.id != id {
            return Err(Error::StaleTarget);
        }
        if (focus && !prepared.focus) || (!focus && !prepared.press) {
            return Ok(false);
        }
        // Everything, including CF constants and timeout setup, is prepared.
        // SAFETY: refs stay owned on this thread; only a verified focused control
        // can reach dispatch. Check the host's current authority last of all.
        let code = unsafe {
            let action = self.press.raw();
            let attr = self.focused.raw();
            let yes = kCFBooleanTrue.ok_or_else(unavailable)? as *const CFBoolean as Ref;
            authority.check()?;
            if focus {
                AXUIElementSetAttributeValue(prepared.object.raw(), attr, yes)
            } else {
                AXUIElementPerformAction(prepared.object.raw(), action)
            }
        };
        match code {
            0 => Ok(true),
            -25211 => Err(Error::Unavailable(PERMISSION.into())),
            -25202 => Err(Error::StaleTarget),
            _ => Err(Error::OutcomeUnknown),
        }
    }
}

#[cfg(test)]
mod proof {
    use super::*;
    #[test]
    fn cf_ownership_checks_null_and_types_without_desktop_access() {
        assert!(Owned::take(ptr::null()).is_err());
        assert!(Owned::retained(ptr::null()).is_err());
        let value = key("Fixture metadata λ").unwrap();
        let cloned = value.clone();
        assert!(value.equal(&cloned));
        assert_eq!(string(&cloned).unwrap(), "Fixture metadata λ");
        assert!(boolean(&value).is_err());
        assert!(array(&value, 1).is_err());
        drop(value);
        assert_eq!(string(&cloned).unwrap(), "Fixture metadata λ");
    }
    fn fixture() -> (std::path::PathBuf, i32) {
        assert_eq!(
            std::env::var("OPHIO_AX_FIXTURE_APPROVED").as_deref(),
            Ok("1")
        );
        let directory = std::path::PathBuf::from(
            std::env::var("OPHIO_MAC_NATIVE_FIXTURE_MARKERS")
                .expect("set disposable fixture markers"),
        );
        assert!(directory.is_absolute() && directory.is_dir());
        assert_eq!(
            std::fs::read_to_string(directory.join("ready.txt")).unwrap(),
            "Ophio native fixture\n"
        );
        let pid: i32 = std::fs::read_to_string(directory.join("pid.txt"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert!(pid > 0);
        (directory, pid)
    }
    fn fixture_ax(pid: i32) -> Ax {
        Ax::start(move || {
            let mut native = Native::new()?;
            native.only_pid = Some(pid);
            Ok(Box::new(native))
        })
    }
    fn fixture_button(snapshot: &Snapshot, pid: i32) -> VoiceElement {
        assert!(
            snapshot.context.starts_with(&format!("{pid}:")),
            "only the disposable fixture may be inspected"
        );
        let button = snapshot
            .elements
            .iter()
            .find(|e| e.name == "Increment fixture counter")
            .expect("fixture button")
            .clone();
        assert!(button.visible && button.enabled && !button.sensitive && !button.destructive);
        button
    }
    /// Requires an explicitly approved disposable fixture window already focused.
    /// This does not create/focus an app or request permission. Never run by default.
    #[tokio::test]
    #[ignore = "requires approved disposable Mac AX fixture and unlocked desktop"]
    async fn native_fixture_metadata_only() {
        let (_, pid) = fixture();
        let ax = fixture_ax(pid);
        let first = ax.snapshot().await.unwrap();
        let button = fixture_button(&first, pid);
        let second = ax.snapshot().await.unwrap();
        assert_eq!(first.context, second.context);
        assert!(second.elements.contains(&button));
    }
    /// Separate explicit input opt-in. Only the disposable app's marker is read;
    /// no text/value accessibility attribute or real desktop capture is used.
    #[tokio::test]
    #[ignore = "requires explicit input approval for disposable Mac AX fixture"]
    async fn native_fixture_authorized_press() {
        assert_eq!(
            std::env::var("OPHIO_AX_FIXTURE_INPUT_APPROVED").as_deref(),
            Ok("1")
        );
        let (directory, pid) = fixture();
        let count = || -> u64 {
            std::fs::read_to_string(directory.join("button-count.txt"))
                .unwrap()
                .trim()
                .parse()
                .unwrap()
        };
        let before = count();
        let ax = fixture_ax(pid);
        let snapshot = ax.snapshot().await.unwrap();
        let button = fixture_button(&snapshot, pid);
        struct Approved;
        impl Authority for Approved {
            fn check(&self) -> Result<(), Error> {
                Ok(())
            }
        }
        assert!(
            ax.activate_authorized(&button, false, Arc::new(Approved))
                .await
                .unwrap()
        );
        for _ in 0..20 {
            if count() == before + 1 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert_eq!(
            count(),
            before + 1,
            "fixture must acknowledge exactly one press"
        );
    }
}
