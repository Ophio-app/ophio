// SPDX-License-Identifier: Apache-2.0
//! UI Automation metadata. No ValuePattern or text content is read.
#[cfg(windows)]
pub mod browser_pipe;
use async_trait::async_trait;
use protocol::voice::VoiceElement;
use voice_control::{Accessibility, Error, Snapshot};

#[derive(Debug, Default)]
pub struct Uia;
#[async_trait]
impl Accessibility for Uia {
    async fn snapshot(&self) -> Result<Snapshot, Error> {
        #[cfg(windows)]
        {
            tokio::task::spawn_blocking(native::snapshot)
                .await
                .map_err(|_| Error::Stopped)?
        }
        #[cfg(not(windows))]
        {
            Err(Error::Unavailable(
                "Windows UI Automation requires an unlocked Windows desktop".into(),
            ))
        }
    }
    async fn activate(&self, element: &VoiceElement, focus: bool) -> Result<bool, Error> {
        #[cfg(windows)]
        {
            let e = element.clone();
            tokio::task::spawn_blocking(move || native::activate(&e, focus))
                .await
                .map_err(|_| Error::OutcomeUnknown)?
        }
        #[cfg(not(windows))]
        {
            let _ = (element, focus);
            Err(Error::Unavailable(
                "Windows UI Automation is unavailable on this computer".into(),
            ))
        }
    }
}
#[cfg(windows)]
#[allow(unsafe_code)]
mod native {
    use super::*;
    use protocol::voice::VoiceBounds;
    use windows::Win32::{
        System::{Com::*, Ole::*},
        UI::{Accessibility::*, WindowsAndMessaging::GetForegroundWindow},
    };
    fn failure(_: windows::core::Error) -> Error {
        Error::Unavailable(
            "Enable accessibility for the focused Windows app and use an unlocked desktop".into(),
        )
    }
    struct Apartment;
    impl Drop for Apartment {
        fn drop(&mut self) {
            // SAFETY: paired with successful initialization on this thread.
            unsafe {
                CoUninitialize();
            }
        }
    }
    fn open() -> Result<(Apartment, IUIAutomation), Error> {
        // SAFETY: COM objects remain on this dedicated blocking thread and are dropped before uninitializing.
        unsafe {
            CoInitializeEx(None, COINIT_MULTITHREADED)
                .ok()
                .map_err(failure)?;
            let apartment = Apartment;
            let automation =
                CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER).map_err(failure)?;
            Ok((apartment, automation))
        }
    }
    fn runtime_id(element: &IUIAutomationElement) -> Result<String, Error> {
        // SAFETY: UIA returns an owned one-dimensional SAFEARRAY of integers. Bounds/type are checked and every successful allocation is destroyed.
        unsafe {
            let array = element.GetRuntimeId().map_err(failure)?;
            struct Array(*mut SAFEARRAY);
            impl Drop for Array {
                fn drop(&mut self) {
                    // SAFETY: owns the SAFEARRAY returned by GetRuntimeId.
                    unsafe {
                        let _ = SafeArrayDestroy(self.0);
                    }
                }
            }
            if array.is_null() {
                return Err(Error::StaleTarget);
            }
            let _owned = Array(array);
            if SafeArrayGetDim(array) != 1 || SafeArrayGetElemsize(array) != 4 {
                return Err(Error::StaleTarget);
            }
            let first = SafeArrayGetLBound(array, 1).map_err(failure)?;
            let last = SafeArrayGetUBound(array, 1).map_err(failure)?;
            if !(0..=24).contains(&(last - first)) {
                return Err(Error::StaleTarget);
            }
            let mut id = String::from("u");
            for index in first..=last {
                let mut part = 0i32;
                SafeArrayGetElement(array, &index, (&mut part as *mut i32).cast())
                    .map_err(failure)?;
                use std::fmt::Write;
                let _ = write!(id, "-{part:x}");
            }
            Ok(id)
        }
    }
    fn collect(
        a: &IUIAutomation,
    ) -> Result<(Snapshot, Vec<(VoiceElement, IUIAutomationElement)>), Error> {
        // SAFETY: all COM pointers are owned by windows-rs and used only in this initialized apartment. Properties read here contain no field values.
        unsafe {
            let window = GetForegroundWindow();
            if window.0.is_null() {
                return Err(Error::Unavailable("No foreground Windows app".into()));
            }
            let root = a.ElementFromHandle(window).map_err(failure)?;
            let condition = a.CreateTrueCondition().map_err(failure)?;
            let found = root
                .FindAll(TreeScope_Subtree, &condition)
                .map_err(failure)?;
            let mut items = Vec::new();
            for i in 0..found.Length().map_err(failure)?.min(1000) {
                let e = found.GetElement(i).map_err(failure)?;
                if !e.CurrentIsKeyboardFocusable().map_err(failure)?.as_bool() {
                    continue;
                }
                let bounds = e.CurrentBoundingRectangle().map_err(failure)?;
                let role = e.CurrentControlType().map_err(failure)?;
                let name = e.CurrentName().map_err(failure)?.to_string();
                let row = VoiceElement {
                    id: runtime_id(&e)?,
                    role: role_name(role.0).into(),
                    name: name.clone(),
                    bounds: VoiceBounds {
                        x: bounds.left.into(),
                        y: bounds.top.into(),
                        width: (bounds.right - bounds.left).into(),
                        height: (bounds.bottom - bounds.top).into(),
                    },
                    visible: !e.CurrentIsOffscreen().map_err(failure)?.as_bool(),
                    enabled: e.CurrentIsEnabled().map_err(failure)?.as_bool(),
                    focused: e.CurrentHasKeyboardFocus().map_err(failure)?.as_bool(),
                    sensitive: e.CurrentIsPassword().map_err(failure)?.as_bool()
                        || private_label(&name),
                    destructive: false,
                };
                items.push((row, e));
            }
            let snapshot = Snapshot::new(
                items.iter().map(|(e, _)| e.clone()).collect(),
                format!("{:?}", window.0),
            );
            if snapshot.elements.is_empty() {
                return Err(Error::Unavailable(
                    "The focused Windows app exposes no accessible controls".into(),
                ));
            }
            Ok((snapshot, items))
        }
    }
    pub fn snapshot() -> Result<Snapshot, Error> {
        let (_apartment, a) = open()?;
        Ok(collect(&a)?.0)
    }
    pub fn activate(expected: &VoiceElement, focus: bool) -> Result<bool, Error> {
        let (_apartment, a) = open()?;
        let (_, items) = collect(&a)?;
        let e = revalidated(&items, expected)?;
        // SAFETY: the revalidated element and its pattern remain in the same initialized COM apartment.
        unsafe {
            if focus {
                e.SetFocus().map_err(failure)?;
                return Ok(true);
            }
            match e.GetCurrentPatternAs::<IUIAutomationInvokePattern>(UIA_InvokePatternId) {
                Ok(pattern) => {
                    pattern.Invoke().map_err(|_| Error::OutcomeUnknown)?;
                    Ok(true)
                }
                Err(_) => Ok(false),
            }
        }
    }
}

#[cfg(any(windows, test))]
fn role_name(id: i32) -> &'static str {
    match id {
        50000 => "button",
        50002 => "checkbox",
        50003 => "combobox",
        50004 => "textbox",
        50005 => "link",
        50007 => "list item",
        50009 => "menu",
        50011 => "menu item",
        50013 => "radio button",
        50014 => "scroll bar",
        50015 => "slider",
        50018 => "tab",
        50019 => "tab item",
        50023 => "tree",
        50024 => "tree item",
        50030 => "document",
        50032 => "window",
        _ => "control",
    }
}
#[cfg(any(windows, test))]
fn private_label(name: &str) -> bool {
    let name = name.to_lowercase();
    [
        "password",
        "passcode",
        "secret",
        "token",
        "card",
        "cvv",
        "cvc",
        "one-time code",
        "key",
        "credential",
        "authorization",
        "ssn",
        "otp",
        "seed",
        "recovery",
    ]
    .iter()
    .any(|word| name.contains(word))
}
#[cfg(any(windows, test))]
fn revalidated<'a, T>(
    items: &'a [(VoiceElement, T)],
    expected: &VoiceElement,
) -> Result<&'a T, Error> {
    items
        .iter()
        .find(|(e, _)| e == expected)
        .map(|(_, native)| native)
        .ok_or(Error::StaleTarget)
}
pub(crate) fn map_point(
    x: f64,
    y: f64,
    origin_x: i32,
    origin_y: i32,
    width: u32,
    height: u32,
    current: bool,
) -> Result<protocol::control::TargetPoint, ports::PlatformError> {
    let point = protocol::control::TargetPoint {
        x: (x - f64::from(origin_x)) / f64::from(width.saturating_sub(1)),
        y: (y - f64::from(origin_y)) / f64::from(height.saturating_sub(1)),
    };
    if !current || !(0.0..=1.0).contains(&point.x) || !(0.0..=1.0).contains(&point.y) {
        return Err(ports::PlatformError::GeometryChanged);
    }
    Ok(point)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fake_uia_controls_use_exact_runtime_identity_and_metadata() {
        let element = VoiceElement {
            id: "u-4-2-17".into(),
            role: role_name(50000).into(),
            name: "Open".into(),
            bounds: protocol::voice::VoiceBounds {
                x: 4.,
                y: 6.,
                width: 80.,
                height: 30.,
            },
            enabled: true,
            visible: true,
            focused: false,
            sensitive: false,
            destructive: false,
        };
        let handles = vec![(element.clone(), 42u32)];
        assert_eq!(*revalidated(&handles, &element).unwrap(), 42);
        for changed in ["role", "name", "bounds", "id"] {
            let mut expected = element.clone();
            match changed {
                "role" => expected.role = "link".into(),
                "name" => expected.name = "Send".into(),
                "bounds" => expected.bounds.x += 1.,
                _ => expected.id = "u-4-2-18".into(),
            };
            assert!(revalidated(&handles, &expected).is_err());
        }
        assert_eq!(role_name(50004), "textbox");
        assert!(private_label("Payment card"));
        assert!(!private_label("Search"));
    }
}
