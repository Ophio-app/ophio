// SPDX-License-Identifier: Apache-2.0
//! Packaged AppsFolder apps only. No parsing names, paths, shell commands, or
//! activation arguments are accepted. COM objects remain on their worker thread.
use super::{Error, PackagedApp, Prepared, unavailable};
use std::{collections::BTreeMap, marker::PhantomData, rc::Rc};
use voice_control::apps::AppRole;
use windows::{
    Win32::{
        Foundation::{ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS, PROPERTYKEY, S_OK},
        Storage::Packaging::Appx::{
            _PACKAGE_INFO_REFERENCE, APPLICATION_USER_MODEL_ID_MAX_LENGTH, ClosePackageInfo,
            GetPackageApplicationIds, GetPackagesByPackageFamily, OpenPackageInfoByFullName,
            ParseApplicationUserModelId,
        },
        System::Com::{
            CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE,
            CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize,
        },
        UI::Shell::{
            AO_NONE, ASSOCF_IS_PROTOCOL, ASSOCF_NOTRUNCATE, ASSOCSTR_APPID,
            ApplicationActivationManager, AssocQueryStringW, BHID_EnumItems, FOLDERID_AppsFolder,
            IApplicationActivationManager, IEnumShellItems, IShellItem, IShellItem2,
            KF_FLAG_DEFAULT, PropertiesSystem::PSGetPropertyKeyFromName, SHCreateItemInKnownFolder,
            SIGDN_NORMALDISPLAY,
        },
    },
    core::{Interface, PCWSTR, PWSTR, w},
};

const MAX_ITEMS: usize = 4096;
const MAX_PACKAGES: usize = 64;
const MAX_BUFFER: usize = 1024 * 1024;
const MAX_CATALOG_BYTES: usize = 16 * 1024 * 1024;
const MAX_NAME: usize = 256;
const MAX_ID: usize = APPLICATION_USER_MODEL_ID_MAX_LENGTH as usize;
// Also bounds package full names and the two components of an AUMID.
const MAX_PACKAGE_TEXT: usize = 256;

type Packages = Vec<(String, Vec<String>)>;

struct Apartment(PhantomData<Rc<()>>);
impl Apartment {
    fn new() -> Result<Self, Error> {
        // SAFETY: Initializes this thread only. Both S_OK and S_FALSE require a
        // matching CoUninitialize. A failed initialization creates no guard.
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) }
            .ok()
            .map_err(|_| unavailable("Windows app discovery could not initialize"))?;
        Ok(Self(PhantomData))
    }
}
impl Drop for Apartment {
    fn drop(&mut self) {
        // SAFETY: The non-Send guard balances successful initialization on the
        // same thread. It is dropped after all COM objects owned alongside it.
        unsafe { CoUninitialize() };
    }
}

pub(super) fn entries() -> Result<Vec<PackagedApp>, Error> {
    let _apartment = Apartment::new()?;
    // SAFETY: The folder identifier is a fixed OS constant; the null relative
    // name selects only AppsFolder, never a caller-supplied namespace or path.
    let folder: IShellItem =
        unsafe { SHCreateItemInKnownFolder(&FOLDERID_AppsFolder, KF_FLAG_DEFAULT, PCWSTR::null()) }
            .map_err(|_| unavailable("Windows apps could not be listed"))?;
    // SAFETY: Requests the documented item enumerator on this apartment.
    let enumerator: IEnumShellItems = unsafe { folder.BindToHandler(None, &BHID_EnumItems) }
        .map_err(|_| unavailable("Windows apps could not be listed"))?;
    let mut key = PROPERTYKEY::default();
    // SAFETY: The property name is static and the output is writable.
    unsafe { PSGetPropertyKeyFromName(w!("System.AppUserModel.ID"), &mut key) }
        .map_err(|_| unavailable("Windows app identities could not be read"))?;
    let mut families = BTreeMap::<String, Packages>::new();
    // None means conflicting AppsFolder entries: do not choose one arbitrarily.
    let mut apps = BTreeMap::<String, Option<PackagedApp>>::new();
    let mut budget = Budget::default();
    for index in 0..=MAX_ITEMS {
        let mut next = [None];
        let mut fetched = 0;
        // SAFETY: Exactly one initialized output slot and its count are valid
        // for this call. The binding maps S_FALSE to Ok, so inspect fetched.
        unsafe { enumerator.Next(&mut next, Some(&mut fetched)) }
            .map_err(|_| unavailable("Windows app enumeration was interrupted"))?;
        if fetched == 0 {
            return Ok(apps.into_values().flatten().collect());
        }
        if fetched != 1 || index == MAX_ITEMS {
            return Err(unavailable("Windows apps exceed the catalog limit"));
        }
        let Some(item) = next[0].take() else {
            return Err(unavailable("Windows returned an incomplete app entry"));
        };
        let Ok(properties) = item.cast::<IShellItem2>() else {
            continue;
        };
        // SAFETY: This is an OS-resolved property key on the enumerated item.
        let Ok(aumid) = (unsafe { properties.GetString(&key) }) else {
            continue;
        };
        // SAFETY: GetString transfers a NUL-terminated CoTaskMem string.
        let Some(aumid) = (unsafe { owned_string(aumid, MAX_ID) }) else {
            continue;
        };
        let Some(family) = package_family(&aumid) else {
            // AppsFolder also exposes unpackaged apps. They are not activatable
            // through this adapter, even when their arbitrary AUMID looks useful.
            continue;
        };
        if !families.contains_key(&family) {
            let packages = family_packages(&family, &mut budget)?;
            families.insert(family.clone(), packages);
        }
        let Some(package) = unique_package(&aumid, &families[&family]) else {
            continue;
        };
        // SAFETY: Uses the non-parsing display-name form only for presentation.
        let Ok(name) = (unsafe { item.GetDisplayName(SIGDN_NORMALDISPLAY) }) else {
            continue;
        };
        // SAFETY: GetDisplayName transfers a NUL-terminated CoTaskMem string.
        let Some(name) = (unsafe { owned_string(name, MAX_NAME) }) else {
            continue;
        };
        let app = PackagedApp {
            aumid: aumid.clone(),
            package: package.to_owned(),
            name,
        };
        budget.charge(app.aumid.len() + app.package.len() + app.name.len())?;
        apps.entry(aumid)
            .and_modify(|old| {
                if old.as_ref() != Some(&app) {
                    *old = None;
                }
            })
            .or_insert(Some(app));
    }
    unreachable!("bounded enumeration returns at its limit")
}

pub(super) fn prepare(app: &PackagedApp) -> Result<Box<dyn Prepared>, Error> {
    let apartment = Apartment::new()?;
    // Match the label, full package identity (including version), and AUMID,
    // not merely an identifier supplied by a caller or a previous catalog.
    if !entries()?.contains(app) {
        return Err(Error::StaleTarget);
    }
    let aumid = wide(&app.aumid, MAX_ID).ok_or(Error::StaleTarget)?;
    // SAFETY: Fixed in-process Windows COM class, no aggregation. Creation and
    // subsequent use stay on this initialized, non-Send worker thread.
    let manager: IApplicationActivationManager =
        unsafe { CoCreateInstance(&ApplicationActivationManager, None, CLSCTX_INPROC_SERVER) }
            .map_err(|_| unavailable("Windows app activation is unavailable"))?;
    Ok(Box::new(Ready {
        manager,
        aumid,
        _apartment: apartment,
    }))
}

struct Ready {
    manager: IApplicationActivationManager,
    aumid: Vec<u16>,
    // Fields drop in declaration order: release the COM object first.
    _apartment: Apartment,
}
impl Prepared for Ready {
    fn open(self: Box<Self>) -> Result<(), Error> {
        // The outer launcher performs the final Stop/authority gate before
        // calling this method. Windows cannot atomically pin a package version
        // between the last observed catalog check and ActivateApplication.
        // SAFETY: The NUL-terminated AUMID came from fresh AppsFolder discovery
        // and exact installed-package membership. No arguments or shell target
        // are supplied, and the COM object remains on its apartment's thread.
        unsafe {
            self.manager
                .ActivateApplication(PCWSTR(self.aumid.as_ptr()), PCWSTR::null(), AO_NONE)
        }
        .map(|_| ())
        .map_err(|_| unavailable("Windows refused to activate the app"))
    }
}

/// Only roles backed by an OS default and a currently verified packaged app.
/// The returned values are AUMIDs, not the launcher's opaque catalog IDs.
pub(super) fn defaults() -> Result<BTreeMap<AppRole, String>, Error> {
    let _apartment = Apartment::new()?;
    let apps = entries()?;
    let mut defaults = BTreeMap::new();
    if let (Some(http), Some(https)) = (association("http"), association("https"))
        && http == https
        && apps.iter().any(|app| app.aumid == http)
    {
        defaults.insert(AppRole::Browser, http);
    }
    if let Some(email) = association("mailto")
        && apps.iter().any(|app| app.aumid == email)
    {
        defaults.insert(AppRole::Email, email);
    }
    Ok(defaults)
}

fn association(protocol: &str) -> Option<String> {
    let protocol = wide(protocol, 16)?;
    let mut length = 0;
    // SAFETY: Static protocol choices, no extra string or output buffer. No
    // NOUSERSETTINGS/PER_MACHINE_ONLY flags: retain current-user associations.
    let result = unsafe {
        AssocQueryStringW(
            ASSOCF_IS_PROTOCOL | ASSOCF_NOTRUNCATE,
            ASSOCSTR_APPID,
            PCWSTR(protocol.as_ptr()),
            PCWSTR::null(),
            None,
            &mut length,
        )
    };
    if result.is_err() || length == 0 || length as usize > MAX_ID + 1 {
        return None;
    }
    let mut output = vec![0u16; length as usize];
    // SAFETY: The output buffer is exactly the probed character capacity. A
    // changed association requiring more space is omitted, never truncated.
    let result = unsafe {
        AssocQueryStringW(
            ASSOCF_IS_PROTOCOL | ASSOCF_NOTRUNCATE,
            ASSOCSTR_APPID,
            PCWSTR(protocol.as_ptr()),
            PCWSTR::null(),
            Some(PWSTR(output.as_mut_ptr())),
            &mut length,
        )
    };
    if result != S_OK || length == 0 || length as usize > output.len() {
        return None;
    }
    text(&output[..length as usize], MAX_ID)
}

fn package_family(aumid: &str) -> Option<String> {
    let aumid = wide(aumid, MAX_ID)?;
    // Fixed bounded capacities are larger than the documented identity limits;
    // a changing or unexpectedly large result fails closed without allocation.
    let mut family = [0u16; MAX_PACKAGE_TEXT + 1];
    let mut relative = [0u16; MAX_PACKAGE_TEXT + 1];
    let mut family_length = family.len() as u32;
    let mut relative_length = relative.len() as u32;
    // SAFETY: Both writable buffers have the capacities passed in UTF-16 units.
    let status = unsafe {
        ParseApplicationUserModelId(
            PCWSTR(aumid.as_ptr()),
            &mut family_length,
            Some(PWSTR(family.as_mut_ptr())),
            &mut relative_length,
            Some(PWSTR(relative.as_mut_ptr())),
        )
    };
    if status != ERROR_SUCCESS
        || family_length as usize > family.len()
        || relative_length as usize > relative.len()
    {
        return None;
    }
    text(&relative[..relative_length as usize], MAX_PACKAGE_TEXT)?;
    text(&family[..family_length as usize], MAX_PACKAGE_TEXT)
}

#[derive(Default)]
struct Budget {
    bytes: usize,
    packages: usize,
    ids: usize,
}
impl Budget {
    fn charge(&mut self, bytes: usize) -> Result<(), Error> {
        self.bytes = self.bytes.saturating_add(bytes);
        if self.bytes > MAX_CATALOG_BYTES {
            return Err(unavailable("Windows apps exceed the catalog size limit"));
        }
        Ok(())
    }
}

fn family_packages(family: &str, budget: &mut Budget) -> Result<Packages, Error> {
    let family = wide(family, MAX_PACKAGE_TEXT).ok_or(Error::StaleTarget)?;
    let mut count = 0;
    let mut length = 0;
    // SAFETY: Size-only query for packages registered to the current user.
    let status = unsafe {
        GetPackagesByPackageFamily(PCWSTR(family.as_ptr()), &mut count, None, &mut length, None)
    };
    if status == ERROR_SUCCESS && count == 0 {
        return Ok(Vec::new());
    }
    if status != ERROR_INSUFFICIENT_BUFFER
        || count == 0
        || count as usize > MAX_PACKAGES
        || length == 0
        || length as usize > MAX_BUFFER / 2
    {
        return Err(unavailable("Windows package identities could not be read"));
    }
    let mut names = vec![PWSTR::null(); count as usize];
    let mut buffer = vec![0u16; length as usize];
    budget.charge(buffer.len() * 2)?;
    // SAFETY: Both arrays have the probed capacities and remain alive while the
    // returned pointers into buffer are inspected. No cross-user query is used.
    let status = unsafe {
        GetPackagesByPackageFamily(
            PCWSTR(family.as_ptr()),
            &mut count,
            Some(names.as_mut_ptr()),
            &mut length,
            Some(PWSTR(buffer.as_mut_ptr())),
        )
    };
    if status != ERROR_SUCCESS || count as usize > names.len() || length as usize > buffer.len() {
        // A package update can change the required size. Do not reuse partial
        // results or guess a version; a later catalog refresh can retry.
        return Err(unavailable(
            "Windows package identities changed during discovery",
        ));
    }
    let mut full_names = Vec::new();
    for name in &names[..count as usize] {
        full_names.push(
            buffer_string(*name, &buffer[..length as usize], MAX_PACKAGE_TEXT)
                .ok_or_else(|| unavailable("Windows returned an invalid package identity"))?,
        );
    }
    full_names.sort();
    full_names.dedup();
    let mut packages = Vec::new();
    for name in full_names {
        budget.packages += 1;
        if budget.packages > MAX_ITEMS {
            return Err(unavailable("Windows packages exceed the catalog limit"));
        }
        let ids = package_ids(&name, budget)?;
        packages.push((name, ids));
    }
    Ok(packages)
}

fn unique_package<'a>(aumid: &str, packages: &'a Packages) -> Option<&'a str> {
    let mut matches = packages
        .iter()
        .filter(|(_, ids)| ids.iter().any(|id| id == aumid));
    let package = &matches.next()?.0;
    // Two installed versions exposing this same app are ambiguous. Never select
    // the newest by string sorting or silently activate a different version.
    matches.next().is_none().then_some(package.as_str())
}

struct PackageInfo(*mut _PACKAGE_INFO_REFERENCE);
impl Drop for PackageInfo {
    fn drop(&mut self) {
        // SAFETY: The handle is owned from one successful open, and is closed
        // exactly once after all calls that borrow its package information.
        let _ = unsafe { ClosePackageInfo(self.0) };
    }
}
fn package_ids(package: &str, budget: &mut Budget) -> Result<Vec<String>, Error> {
    let package = wide(package, MAX_PACKAGE_TEXT).ok_or(Error::StaleTarget)?;
    let mut reference = std::ptr::null_mut();
    // SAFETY: NUL-terminated full name returned for this user, reserved is zero,
    // and the reference output is writable. Open alone does not prove user
    // registration; GetPackagesByPackageFamily above supplies that evidence.
    let status =
        unsafe { OpenPackageInfoByFullName(PCWSTR(package.as_ptr()), None, &mut reference) };
    if status != ERROR_SUCCESS || reference.is_null() {
        return Err(unavailable(
            "Windows package information could not be opened",
        ));
    }
    let reference = PackageInfo(reference);
    let mut length = 0;
    let mut count = 0;
    // SAFETY: Size-only query against the live owned package reference.
    let status =
        unsafe { GetPackageApplicationIds(reference.0, &mut length, None, Some(&mut count)) };
    if status == ERROR_SUCCESS && count == 0 {
        return Ok(Vec::new());
    }
    if status != ERROR_INSUFFICIENT_BUFFER
        || length == 0
        || length as usize > MAX_BUFFER
        || count as usize > MAX_ITEMS
    {
        return Err(unavailable(
            "Windows package applications could not be read",
        ));
    }
    let capacity = length as usize;
    // The API returns a pointer table followed by UTF-16 strings. usize storage
    // supplies pointer alignment; Vec<u8> would not guarantee that alignment.
    let mut buffer = vec![0usize; capacity.div_ceil(size_of::<usize>())];
    budget.charge(buffer.len() * size_of::<usize>())?;
    // SAFETY: The initialized, pointer-aligned allocation covers length bytes.
    // All returned pointers are validated against this allocation before use.
    let status = unsafe {
        GetPackageApplicationIds(
            reference.0,
            &mut length,
            Some(buffer.as_mut_ptr().cast()),
            Some(&mut count),
        )
    };
    if status != ERROR_SUCCESS || length as usize > capacity {
        return Err(unavailable(
            "Windows package applications changed during discovery",
        ));
    }
    budget.ids = budget.ids.saturating_add(count as usize);
    if budget.ids > MAX_ITEMS * 4 {
        return Err(unavailable(
            "Windows package applications exceed the catalog limit",
        ));
    }
    ids_from_buffer(&buffer, length as usize, count as usize)
        .ok_or_else(|| unavailable("Windows returned invalid package applications"))
}

// Never dereference pointers returned inside an OS-filled buffer. Convert them
// to checked offsets, then read only through the original Rust allocation.
fn buffer_string(pointer: PWSTR, buffer: &[u16], limit: usize) -> Option<String> {
    let offset = (pointer.0 as usize).checked_sub(buffer.as_ptr() as usize)?;
    if !offset.is_multiple_of(size_of::<u16>()) {
        return None;
    }
    text(buffer.get(offset / size_of::<u16>()..)?, limit)
}
fn ids_from_buffer(buffer: &[usize], length: usize, count: usize) -> Option<Vec<String>> {
    if count > MAX_ITEMS
        || length > size_of_val(buffer)
        || count.checked_mul(size_of::<usize>())? > length
    {
        return None;
    }
    // SAFETY: usize has no padding, the allocation is fully initialized, and
    // length was checked against its byte size. This view never outlives it.
    let bytes = unsafe { std::slice::from_raw_parts(buffer.as_ptr().cast::<u8>(), length) };
    let table_end = count * size_of::<usize>();
    let mut ids = Vec::new();
    for pointer in &buffer[..count] {
        let offset = pointer.checked_sub(buffer.as_ptr() as usize)?;
        if offset < table_end || !offset.is_multiple_of(2) || offset >= length {
            return None;
        }
        let units: Vec<_> = bytes[offset..]
            .as_chunks::<2>()
            .0
            .iter()
            .take(MAX_ID + 1)
            .map(|pair| u16::from_ne_bytes(*pair))
            .collect();
        ids.push(text(&units, MAX_ID)?);
    }
    Some(ids)
}
fn text(buffer: &[u16], limit: usize) -> Option<String> {
    let end = buffer.iter().take(limit + 1).position(|unit| *unit == 0)?;
    if end == 0 {
        return None;
    }
    let text = String::from_utf16(&buffer[..end]).ok()?;
    (!text.chars().any(char::is_control)).then_some(text)
}
fn wide(text: &str, limit: usize) -> Option<Vec<u16>> {
    if text.is_empty() || text.len() > limit * 4 || text.chars().any(char::is_control) {
        return None;
    }
    let mut units: Vec<_> = text.encode_utf16().collect();
    if units.len() > limit {
        return None;
    }
    units.push(0);
    Some(units)
}

/// Takes ownership of an OS-returned NUL-terminated CoTaskMem string.
///
/// # Safety
/// `pointer` must be null or the owned string returned by a documented shell
/// string API. The allocation must contain its terminator and be freed once.
unsafe fn owned_string(pointer: PWSTR, limit: usize) -> Option<String> {
    struct Free(PWSTR);
    impl Drop for Free {
        fn drop(&mut self) {
            // SAFETY: Ownership was transferred to this guard by the caller.
            unsafe { CoTaskMemFree(Some(self.0.0.cast())) };
        }
    }
    let owned = Free(pointer);
    if owned.0.is_null() {
        return None;
    }
    let mut units = Vec::new();
    for index in 0..=limit {
        // SAFETY: The API guarantees a terminated string. Read one unit at a
        // time, stopping at its terminator, never constructing an oversized
        // slice from a maximum guessed allocation length.
        let unit = unsafe { *owned.0.0.add(index) };
        units.push(unit);
        if unit == 0 {
            return text(&units, limit);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    // These tests use synthetic memory only. No COM initialization, AppsFolder
    // enumeration, association lookup, desktop access, or activation occurs.
    #[test]
    fn synthetic_strings_are_bounded_and_strict() {
        assert_eq!(text(&[b'a' as u16, 0], 1).as_deref(), Some("a"));
        assert!(text(&[b'a' as u16, b'b' as u16, 0], 1).is_none());
        assert!(text(&[b'a' as u16], 1).is_none());
        assert!(text(&[0], 1).is_none());
        assert!(text(&[0xd800, 0], 1).is_none());
        assert!(text(&[b'\n' as u16, 0], 1).is_none());
        assert!(wide("app\0command", MAX_ID).is_none());
        assert!(wide("😀", 1).is_none());
        assert_eq!(wide("😀", 2).unwrap(), vec![0xd83d, 0xde00, 0]);
    }

    #[test]
    fn synthetic_package_pointer_must_stay_inside_original_buffer() {
        let buffer = [b'a' as u16, 0];
        let base = buffer.as_ptr() as usize;
        assert_eq!(
            buffer_string(PWSTR(base as *mut u16), &buffer, 1).as_deref(),
            Some("a")
        );
        for address in [0, base - 2, base + 1, base + size_of_val(&buffer)] {
            assert!(buffer_string(PWSTR(address as *mut u16), &buffer, 1).is_none());
        }
    }

    #[test]
    fn synthetic_app_id_pointer_table_rejects_invalid_offsets_and_counts() {
        let mut buffer = [0usize; 4];
        let base = buffer.as_ptr() as usize;
        let offset = size_of::<usize>();
        buffer[0] = base + offset;
        // SAFETY: The initialized buffer is exclusively borrowed and its byte
        // view covers exactly its own allocation; only synthetic data is used.
        let bytes = unsafe {
            std::slice::from_raw_parts_mut(buffer.as_mut_ptr().cast::<u8>(), size_of_val(&buffer))
        };
        bytes[offset..offset + 2].copy_from_slice(&(b'a' as u16).to_ne_bytes());
        let length = offset + 4;
        assert_eq!(ids_from_buffer(&buffer, length, 1).unwrap(), vec!["a"]);
        assert!(ids_from_buffer(&buffer, size_of_val(&buffer) + 1, 1).is_none());
        assert!(ids_from_buffer(&buffer, length, MAX_ITEMS + 1).is_none());
        for address in [0, base, base + offset + 1, base + length] {
            buffer[0] = address;
            assert!(ids_from_buffer(&buffer, length, 1).is_none());
        }
        buffer[0] = base + offset;
        assert!(ids_from_buffer(&buffer, offset + 2, 1).is_none());
        assert!(ids_from_buffer(&buffer, length, 2).is_none());
    }

    #[test]
    fn synthetic_membership_requires_one_exact_package_version() {
        let mut packages = vec![("package_1.0".into(), vec!["family!app".into()])];
        assert_eq!(unique_package("family!app", &packages), Some("package_1.0"));
        assert!(unique_package("family!other", &packages).is_none());
        assert!(unique_package("family!APP", &packages).is_none());
        packages.push(("package_2.0".into(), vec!["family!app".into()]));
        assert!(unique_package("family!app", &packages).is_none());
    }

    #[test]
    fn synthetic_catalog_size_limit_is_fail_closed() {
        let mut budget = Budget::default();
        assert!(budget.charge(MAX_CATALOG_BYTES).is_ok());
        assert!(budget.charge(1).is_err());
        assert!(budget.charge(usize::MAX).is_err());
    }
}
