// SPDX-License-Identifier: Apache-2.0
use std::ffi::OsString;
use windows::Win32::System::Registry::*;
use windows::core::{PCWSTR, w};

/// Registry values longer than this are not read; Windows itself limits a
/// variable to 32 767 characters.
const LONGEST: u32 = 64 * 1024;

/// The `PATH` a program started now would get (see
/// [`crate::environment::merged`]), with `%NAME%` references in it expanded
/// from this process's environment. `None` when neither the computer's nor
/// the person's `PATH` can be read.
pub fn fresh_path() -> Option<OsString> {
    let machine = path_value(
        HKEY_LOCAL_MACHINE,
        w!("SYSTEM\\CurrentControlSet\\Control\\Session Manager\\Environment"),
    );
    let user = path_value(HKEY_CURRENT_USER, w!("Environment"));
    if machine.is_none() && user.is_none() {
        return None;
    }
    let current = std::env::var("PATH").ok();
    Some(crate::environment::merged(machine.as_deref(), user.as_deref(), current.as_deref()).into())
}

/// The `Path` value under `root\key`, expanded when it is `REG_EXPAND_SZ`.
fn path_value(root: HKEY, key: PCWSTR) -> Option<String> {
    let mut opened = HKEY::default();
    // SAFETY: predefined root, a constant NUL-terminated path and an output
    // handle that is closed below.
    unsafe { RegOpenKeyExW(root, key, Some(0), KEY_READ, &mut opened) }
        .ok()
        .ok()?;
    let read = read(opened);
    // SAFETY: closes the handle opened above, once.
    unsafe {
        let _ = RegCloseKey(opened);
    }
    let (text, kind) = read?;
    Some(if kind == REG_EXPAND_SZ {
        crate::environment::expand(&text, |name| std::env::var(name).ok())
    } else {
        text
    })
}

fn read(key: HKEY) -> Option<(String, REG_VALUE_TYPE)> {
    let name = w!("Path");
    let mut kind = REG_VALUE_TYPE::default();
    let mut size = 0u32;
    // SAFETY: a size query with no data buffer.
    unsafe { RegQueryValueExW(key, name, None, Some(&mut kind), None, Some(&mut size)) }
        .ok()
        .ok()?;
    if !(kind == REG_SZ || kind == REG_EXPAND_SZ) || size > LONGEST {
        return None;
    }
    // Room for the value even if it grew a little meanwhile, in whole UTF-16
    // units.
    let mut units = vec![0u16; (size as usize).div_ceil(2) + 2];
    let mut size = (units.len() * 2) as u32;
    // SAFETY: the buffer is `size` bytes long and outlives the call.
    unsafe {
        RegQueryValueExW(
            key,
            name,
            None,
            Some(&mut kind),
            Some(units.as_mut_ptr().cast()),
            Some(&mut size),
        )
    }
    .ok()
    .ok()?;
    units.truncate(size as usize / 2);
    while units.last() == Some(&0) {
        units.pop();
    }
    Some((String::from_utf16(&units).ok()?, kind))
}
