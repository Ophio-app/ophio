// SPDX-License-Identifier: Apache-2.0
use super::*;
use windows::Win32::System::Registry::*;
use windows::core::w;
const RUN: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
struct Key(HKEY);
impl Drop for Key {
    fn drop(&mut self) {
        // SAFETY: owns a registry handle opened below, not a predefined key.
        unsafe {
            let _ = RegCloseKey(self.0);
        }
    }
}
fn key(path: PCWSTR, create: bool) -> io::Result<Option<Key>> {
    let mut out = HKEY::default();
    // SAFETY: predefined current-user root, valid NUL-terminated path and output.
    let status = unsafe {
        if create {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                path,
                Some(0),
                None,
                REG_OPTION_NON_VOLATILE,
                KEY_READ | KEY_WRITE,
                None,
                &mut out,
                None,
            )
        } else {
            RegOpenKeyExW(HKEY_CURRENT_USER, path, Some(0), KEY_READ, &mut out)
        }
    };
    if status.0 == 2 {
        return Ok(None);
    }
    status.ok().map_err(failure)?;
    Ok(Some(Key(out)))
}
fn value(key: &Key, name: PCWSTR) -> io::Result<Option<Vec<u8>>> {
    let mut size = 0;
    let mut kind = REG_VALUE_TYPE::default();
    // SAFETY: size query with no data buffer.
    let status =
        unsafe { RegQueryValueExW(key.0, name, None, Some(&mut kind), None, Some(&mut size)) };
    if status.0 == 2 {
        return Ok(None);
    }
    status.ok().map_err(failure)?;
    if kind != REG_SZ || size > 4096 || size % 2 != 0 {
        return Err(denied());
    }
    let mut bytes = vec![0; size as usize];
    // SAFETY: allocated buffer covers the reported size; concurrent growth fails.
    unsafe {
        RegQueryValueExW(
            key.0,
            name,
            None,
            Some(&mut kind),
            Some(bytes.as_mut_ptr()),
            Some(&mut size),
        )
        .ok()
        .map_err(failure)?;
    }
    if kind != REG_SZ {
        return Err(denied());
    }
    bytes.truncate(size as usize);
    Ok(Some(bytes))
}
/// The Run value and ownership marker key of a program's registration.
fn names(program: &str) -> io::Result<(Vec<u16>, Vec<u16>)> {
    Ok((
        wide(std::ffi::OsStr::new(program))?,
        wide(std::ffi::OsStr::new(&format!(
            "Software\\{program}\\Startup"
        )))?,
    ))
}
fn encode(text: &str) -> Vec<u8> {
    text.encode_utf16()
        .chain([0])
        .flat_map(u16::to_le_bytes)
        .collect()
}
/// Status describes registration only, not whether a process is running or
/// Windows Startup Apps has enabled it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Absent,
    Registered,
    Unmanaged,
}
pub fn status() -> io::Result<Status> {
    status_of(ports::PROGRAM_NAME)
}
fn status_of(program: &str) -> io::Result<Status> {
    let (name, marker) = names(program)?;
    let run = key(RUN, false)?
        .map(|k| value(&k, PCWSTR(name.as_ptr())))
        .transpose()?
        .flatten();
    let owned = key(PCWSTR(marker.as_ptr()), false)?
        .map(|k| value(&k, w!("Command")))
        .transpose()?
        .flatten();
    Ok(match run {
        None => Status::Absent,
        Some(ref run) if Some(run) == owned.as_ref() => Status::Registered,
        _ => Status::Unmanaged,
    })
}
pub fn install(executable: &Path) -> io::Result<()> {
    let command = crate::startup::command_line(executable.to_str().ok_or_else(denied)?)?;
    let data = encode(&command);
    let (name, marker) = names(ports::PROGRAM_NAME)?;
    if status()? == Status::Unmanaged {
        return Err(denied());
    }
    // This registration replaces the earlier program's; anyone else's value
    // of that name is left alone.
    if status_of(ports::EARLIER_PROGRAM_NAME)? == Status::Registered {
        remove(ports::EARLIER_PROGRAM_NAME)?;
    }
    let run = key(RUN, true)?.ok_or_else(denied)?;
    let marker = key(PCWSTR(marker.as_ptr()), true)?.ok_or_else(denied)?;
    // SAFETY: live registry handles and immutable buffers. Save ownership first;
    // a crash can leave an orphan marker but cannot delete an unrelated value.
    unsafe {
        RegSetValueExW(marker.0, w!("Command"), Some(0), REG_SZ, Some(&data))
            .ok()
            .map_err(failure)?;
        RegSetValueExW(run.0, PCWSTR(name.as_ptr()), Some(0), REG_SZ, Some(&data))
            .ok()
            .map_err(failure)?;
    }
    Ok(())
}
pub fn uninstall() -> io::Result<()> {
    if status_of(ports::EARLIER_PROGRAM_NAME)? == Status::Registered {
        remove(ports::EARLIER_PROGRAM_NAME)?;
    }
    match status()? {
        Status::Absent => Ok(()),
        Status::Unmanaged => Err(denied()),
        Status::Registered => remove(ports::PROGRAM_NAME),
    }
}
fn remove(program: &str) -> io::Result<()> {
    let (name, marker_name) = names(program)?;
    let run = key(RUN, true)?.ok_or_else(denied)?;
    let marker = key(PCWSTR(marker_name.as_ptr()), true)?.ok_or_else(denied)?;
    if value(&run, PCWSTR(name.as_ptr()))? != value(&marker, w!("Command"))? {
        return Err(denied());
    }
    // SAFETY: delete only the two values whose ownership has been verified.
    unsafe {
        RegDeleteValueW(run.0, PCWSTR(name.as_ptr()))
            .ok()
            .map_err(failure)?;
        RegDeleteValueW(marker.0, w!("Command"))
            .ok()
            .map_err(failure)?;
    }
    Ok(())
}
