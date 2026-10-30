// SPDX-License-Identifier: Apache-2.0
use super::*;
use std::{
    ffi::OsStr,
    fs::File,
    io::{Read, Write},
    os::windows::io::{AsRawHandle, FromRawHandle},
    path::{Component, PathBuf},
};
use windows::{
    Wdk::Storage::FileSystem::{
        FILE_RENAME_INFORMATION, FileRenameInformation, NtSetInformationFile,
    },
    Win32::{
        Foundation::{GENERIC_READ, GENERIC_WRITE, RtlNtStatusToDosError},
        Storage::FileSystem::*,
        System::IO::IO_STATUS_BLOCK,
    },
    core::HRESULT,
};
use zeroize::Zeroizing;

fn absolute(path: &Path) -> io::Result<PathBuf> {
    let path = std::path::absolute(path)?;
    if !matches!(path.components().next(), Some(Component::Prefix(p)) if matches!(p.kind(), std::path::Prefix::Disk(_) | std::path::Prefix::VerbatimDisk(_)))
        || path.components().any(|c| matches!(c, Component::ParentDir))
    {
        return Err(denied());
    }
    for component in path.components() {
        if let Component::Normal(name) = component
            && !crate::fs::plain_component(&name.to_string_lossy())
        {
            return Err(denied());
        }
    }
    Ok(path)
}
fn open(path: &Path, directory: bool, create: bool, write: bool) -> io::Result<File> {
    let name = wide_path(path)?;
    let owner = owner_sid()?;
    let sd = Descriptor::new(
        &owner,
        if directory {
            ObjectKind::Directory
        } else {
            ObjectKind::File
        },
    )?;
    let sa = sd.attributes();
    // SAFETY: valid NUL-terminated path and descriptor. No delete sharing pins
    // each open component against rename until validation and use complete.
    let handle = unsafe {
        CreateFileW(
            PCWSTR(name.as_ptr()),
            READ_CONTROL.0
                | if write {
                    GENERIC_READ.0 | GENERIC_WRITE.0
                } else {
                    GENERIC_READ.0
                },
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            Some(&sa),
            if create { OPEN_ALWAYS } else { OPEN_EXISTING },
            FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS,
            None,
        )
        .map_err(failure)?
    };
    // SAFETY: ownership of the newly created handle transfers exactly once.
    let file = unsafe { File::from_raw_handle(handle.0) };
    metadata(&file, directory)?;
    Ok(file)
}
fn metadata(file: &File, directory: bool) -> io::Result<BY_HANDLE_FILE_INFORMATION> {
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: file owns a valid handle, output is initialized and writable.
    unsafe {
        GetFileInformationByHandle(HANDLE(file.as_raw_handle()), &mut info).map_err(failure)?;
    }
    if info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0
        || (info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY.0 != 0) != directory
        || (!directory && info.nNumberOfLinks != 1)
    {
        return Err(denied());
    }
    Ok(info)
}
fn verify(file: &File, directory: bool) -> io::Result<()> {
    metadata(file, directory)?;
    verify_owner(HANDLE(file.as_raw_handle()), &owner_sid()?, SE_FILE_OBJECT)
}
/// Open and retain every existing ancestor without delete sharing, preventing
/// directory junction/rename races while the leaf is opened or created.
fn parents(path: &Path, create: bool) -> io::Result<Vec<File>> {
    let parent = path.parent().ok_or_else(denied)?;
    let mut chain: Vec<_> = parent.ancestors().collect();
    chain.reverse();
    let mut held = Vec::new();
    for dir in chain {
        if dir.as_os_str().is_empty() {
            continue;
        }
        let file = match open(dir, true, false, false) {
            Ok(file) => file,
            Err(e) if create && e.kind() == io::ErrorKind::NotFound => {
                create_dir(dir)?;
                let f = open(dir, true, false, false)?;
                verify(&f, true)?;
                f
            }
            Err(e) => return Err(e),
        };
        held.push(file);
    }
    Ok(held)
}
fn create_dir(path: &Path) -> io::Result<()> {
    let name = wide_path(path)?;
    let sd = Descriptor::new(&owner_sid()?, ObjectKind::Directory)?;
    let sa = sd.attributes();
    // SAFETY: descriptor and path remain live through the synchronous call.
    match unsafe { CreateDirectoryW(PCWSTR(name.as_ptr()), Some(&sa)) } {
        Ok(()) => Ok(()),
        Err(e) if e.code().0 as u32 & 0xffff == 183 => Ok(()),
        Err(e) => Err(failure(e)),
    }
}
pub fn private_dir(path: &Path) -> io::Result<()> {
    let path = absolute(path)?;
    let _parents = parents(&path, true)?;
    create_dir(&path)?;
    verify(&open(&path, true, false, false)?, true)
}
pub fn private_open(path: &Path) -> io::Result<File> {
    let path = absolute(path)?;
    let _parents = parents(&path, false)?;
    let file = open(&path, false, true, true)?;
    verify(&file, false)?;
    Ok(file)
}
pub fn protected_read(path: &Path, limit: usize) -> io::Result<Zeroizing<Vec<u8>>> {
    let path = absolute(path)?;
    let _parents = parents(&path, false)?;
    let file = open(&path, false, false, false)?;
    verify(&file, false)?;
    if file.metadata()?.len() > limit as u64 {
        return Err(denied());
    }
    let mut bytes = Zeroizing::new(Vec::new());
    let read_limit = (limit as u64).checked_add(1).ok_or_else(denied)?;
    (&file).take(read_limit).read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(denied());
    }
    verify(&file, false)?;
    Ok(bytes)
}
/// Atomic replacement from an explicitly secured sibling file, with data
/// flushed before rename. Never follows an existing destination link.
pub fn atomic_write(path: &Path, bytes: &[u8], replace: bool) -> io::Result<()> {
    let path = absolute(path)?;
    let parent = path.parent().ok_or_else(denied)?;
    let leaf = path.file_name().ok_or_else(denied)?;
    private_dir(parent)?;
    let _parents = parents(&path, false)?;
    let parent_handle = open(parent, true, false, false)?;
    verify(&parent_handle, true)?;
    if replace {
        match open(&path, false, false, false) {
            Ok(file) => verify(&file, false)?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    // UUID entropy is supplied by the existing protocol identity generator.
    let temp = parent.join(format!(".identity-{}.tmp", protocol::ids::HostId::new()));
    let name = wide_path(&temp)?;
    let sd = Descriptor::new(&owner_sid()?, ObjectKind::File)?;
    let sa = sd.attributes();
    // SAFETY: CREATE_NEW cannot reuse an attacker-supplied temporary file.
    // Write-through makes the rename through this handle durable on return.
    let handle = unsafe {
        CreateFileW(
            PCWSTR(name.as_ptr()),
            GENERIC_READ.0 | GENERIC_WRITE.0 | READ_CONTROL.0 | DELETE.0,
            FILE_SHARE_READ,
            Some(&sa),
            CREATE_NEW,
            FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_WRITE_THROUGH,
            None,
        )
        .map_err(failure)?
    };
    // SAFETY: exclusive ownership transfers into File.
    let file = unsafe { File::from_raw_handle(handle.0) };
    let result = (|| {
        verify(&file, false)?;
        (&file).write_all(bytes)?;
        file.sync_all()?;
        // The checked file itself moves, and into the pinned, checked parent:
        // neither a path swapped meanwhile nor another file at the temporary
        // name can take its place. Replacing only swaps the directory entry,
        // so it never writes through whatever holds the destination name.
        rename(&file, &parent_handle, leaf, replace)
    })();
    drop(file);
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}
/// Renames the open `file` to `name` in the directory `dir`. The name is
/// resolved from the directory's handle, never from a path.
fn rename(file: &File, dir: &File, name: &OsStr, replace: bool) -> io::Result<()> {
    let name = wide(name)?;
    // Without its terminator; the structure's own first unit makes room.
    let length = size_of_val(&name[..name.len() - 1]);
    let size = size_of::<FILE_RENAME_INFORMATION>() + length;
    let mut buffer = vec![0u64; size.div_ceil(size_of::<u64>())];
    let info = buffer.as_mut_ptr().cast::<FILE_RENAME_INFORMATION>();
    // SAFETY: the zeroed buffer is aligned for the structure and holds it and
    // the whole name.
    unsafe {
        (*info).Anonymous.ReplaceIfExists = replace;
        (*info).RootDirectory = HANDLE(dir.as_raw_handle());
        (*info).FileNameLength = u32::try_from(length).map_err(|_| denied())?;
        std::ptr::copy_nonoverlapping(
            name.as_ptr(),
            (&raw mut (*info).FileName).cast::<u16>(),
            name.len() - 1,
        );
    }
    let mut io_status = IO_STATUS_BLOCK::default();
    // SAFETY: both handles are open for the call, and `size` bytes of the
    // buffer are initialized.
    let status = unsafe {
        NtSetInformationFile(
            HANDLE(file.as_raw_handle()),
            &mut io_status,
            info.cast_const().cast(),
            u32::try_from(size).map_err(|_| denied())?,
            FileRenameInformation,
        )
    };
    if status.is_err() {
        // SAFETY: a pure status code conversion.
        let code = unsafe { RtlNtStatusToDosError(status) };
        return Err(failure(windows::core::Error::from_hresult(
            HRESULT::from_win32(code),
        )));
    }
    Ok(())
}
