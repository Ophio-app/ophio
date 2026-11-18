// SPDX-License-Identifier: Apache-2.0
//! The small, read-only CoreFoundation boundary. Both binary and XML bundle
//! plists go through Apple's parser; no utility, script or app is executed.
// SAFETY: Canonical CF bindings own Create/Copy results. Nullable raw results
// pass through NonNull before ownership is adopted. Plist children are retained
// immediately, and type-checked before access.
#![allow(unsafe_code)]
use super::Info;
use objc2_core_foundation::{
    CFArray, CFBoolean, CFData, CFDictionary, CFError, CFNumber, CFPropertyListCreateWithData,
    CFPropertyListFormat, CFRetained, CFString, CFType, Type,
};
use std::{
    ffi::c_void,
    ptr::{self, NonNull},
};

// Launch Services has no binding in the existing dependency set. This one
// narrow declaration uses canonical CF types and the documented Copy rule.
#[link(name = "CoreServices", kind = "framework")]
unsafe extern "C-unwind" {
    fn LSCopyDefaultHandlerForURLScheme(scheme: &CFString) -> *mut CFString;
}

/// Adopt a nullable Create/Copy result, without ever constructing an owner for
/// NULL. `Option::map` evaluates its closure only for a non-null pointer.
///
/// # Safety
/// A non-null pointer must be a valid `T` with a transferred +1 retain count.
unsafe fn take_created<T: Type>(value: *mut T) -> Option<CFRetained<T>> {
    NonNull::new(value).map(|value| unsafe { CFRetained::from_raw(value) })
}

/// All dictionary and array values come from an immutable Apple property list,
/// which contains only CF objects. A Get result is borrowed, so retain it rather
/// than adopting its existing reference count.
unsafe fn retain_child(value: *const c_void) -> Option<CFRetained<CFType>> {
    NonNull::new(value.cast_mut().cast::<CFType>())
        .map(|value| unsafe { CFRetained::retain(value) })
}

fn string(value: Option<&CFType>) -> Option<String> {
    let value = value?.downcast_ref::<CFString>()?;
    if !(0..=4096).contains(&value.length()) {
        return None;
    }
    let result = value.to_string();
    // Do not silently truncate a plist string at an embedded C terminator.
    if result.contains('\0') {
        return None;
    }
    Some(result)
}

fn get(dictionary: &CFType, key: &str) -> Option<CFRetained<CFType>> {
    let dictionary = dictionary.downcast_ref::<CFDictionary>()?;
    let key = CFString::from_str(key);
    // The type check above verifies the container. Apple plist dictionary keys
    // and values are CF objects; the temporary lookup key remains alive here.
    let value = unsafe { dictionary.value((&*key as *const CFString).cast()) };
    unsafe { retain_child(value) }
}

fn field(dictionary: &CFType, key: &str) -> String {
    string(get(dictionary, key).as_deref()).unwrap_or_default()
}

fn array(value: Option<&CFType>) -> Vec<CFRetained<CFType>> {
    let Some(array) = value.and_then(|v| v.downcast_ref::<CFArray>()) else {
        return vec![];
    };
    let count = array.count();
    if !(0..=128).contains(&count) {
        return vec![];
    }
    // Each index is in bounds and all plist array elements are CF objects.
    (0..count)
        .filter_map(|i| unsafe { retain_child(array.value_at_index(i)) })
        .collect()
}

fn flag(value: Option<&CFType>) -> bool {
    let Some(value) = value else {
        return false;
    };
    if let Some(boolean) = value.downcast_ref::<CFBoolean>() {
        return boolean.value();
    }
    if let Some(number) = value.downcast_ref::<CFNumber>() {
        return number.as_i64().is_some_and(|n| n != 0);
    }
    string(Some(value))
        .is_some_and(|v| matches!(v.to_ascii_lowercase().as_str(), "yes" | "true" | "1"))
}

fn parse(bytes: &[u8]) -> (Option<CFRetained<CFType>>, Option<CFRetained<CFError>>) {
    let data = CFData::from_bytes(bytes);
    let mut format = CFPropertyListFormat::XMLFormat_v1_0;
    let mut error = ptr::null_mut();
    // None selects the default allocator; immutable parsing means no borrowed
    // container can be changed while its children are being retained.
    let plist =
        unsafe { CFPropertyListCreateWithData(None, Some(&data), 0, &mut format, &mut error) };
    // On success Apple leaves error null. Never build a retaining owner for it.
    let error = unsafe { take_created(error) };
    (plist, error)
}

pub(super) fn info(bytes: &[u8]) -> Option<Info> {
    let (plist, error) = parse(bytes);
    if error.is_some() {
        return None;
    }
    let plist = plist?;
    plist.downcast_ref::<CFDictionary>()?;
    let mut schemes = Vec::new();
    for item in array(get(&plist, "CFBundleURLTypes").as_deref()) {
        schemes.extend(
            array(get(&item, "CFBundleURLSchemes").as_deref())
                .into_iter()
                .filter_map(|v| string(Some(&v))),
        );
    }
    let mut content_types = Vec::new();
    for item in array(get(&plist, "CFBundleDocumentTypes").as_deref()) {
        content_types.extend(
            array(get(&item, "LSItemContentTypes").as_deref())
                .into_iter()
                .filter_map(|v| string(Some(&v))),
        );
    }
    Some(Info {
        identifier: field(&plist, "CFBundleIdentifier"),
        name: field(&plist, "CFBundleName"),
        display_name: field(&plist, "CFBundleDisplayName"),
        executable: field(&plist, "CFBundleExecutable"),
        package: field(&plist, "CFBundlePackageType"),
        category: field(&plist, "LSApplicationCategoryType"),
        background: flag(get(&plist, "LSBackgroundOnly").as_deref())
            || flag(get(&plist, "LSUIElement").as_deref()),
        schemes,
        content_types,
    })
}

pub(super) fn default_browser() -> Option<String> {
    let scheme = CFString::from_str("https");
    let handler = unsafe { take_created(LSCopyDefaultHandlerForURLScheme(&scheme)) }?;
    string(Some(&handler))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mac_launch_native_nullable_created_result_never_owns_null() {
        let absent = unsafe { take_created::<CFType>(ptr::null_mut()) };
        assert!(absent.is_none());
        drop(absent);
        let absent_error = unsafe { take_created::<CFError>(ptr::null_mut()) };
        assert!(absent_error.is_none());
        drop(absent_error);
    }

    #[test]
    fn mac_launch_native_successful_parse_has_no_error_owner() {
        let (plist, error) = parse(br#"<plist version="1.0"><dict/></plist>"#);
        assert!(plist.is_some());
        assert!(error.is_none());
        drop((plist, error));
    }

    #[test]
    fn mac_launch_native_failed_parse_releases_returned_error() {
        let (plist, error) = parse(b"not a property list");
        assert!(plist.is_none());
        assert!(error.is_some());
        drop((plist, error));
    }
}
