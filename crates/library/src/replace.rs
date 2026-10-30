// SPDX-License-Identifier: Apache-2.0
//! Native replacement retaining the displaced file for reconciliation.
use cap_std::fs::Dir;
use ports::library::LibraryError;

#[cfg(target_os = "linux")]
pub(crate) fn replace(
    parent: &Dir,
    temp: &str,
    name: &str,
    backup: &str,
) -> Result<(), LibraryError> {
    rustix::fs::renameat_with(
        parent,
        temp,
        parent,
        name,
        rustix::fs::RenameFlags::EXCHANGE,
    )
    .map_err(std::io::Error::from)?;
    // The exchange succeeded even if moving its displaced half now fails.
    // The recovery record names both temporary names for crash recovery.
    parent.rename(temp, parent, backup)?;
    Ok(())
}

#[cfg(windows)]
pub(crate) use native::replace;

/// Windows has no exchange. Both halves are opened in the checked parent and
/// renamed through their handles, each new name resolved from the parent's
/// handle, so a folder swapped in meanwhile cannot redirect either rename.
#[cfg(windows)]
#[allow(unsafe_code)]
mod native {
    use super::{Dir, LibraryError};
    use cap_std::fs::{OpenOptions, OpenOptionsExt};
    use std::{
        ffi::OsStr,
        fs::File,
        io,
        os::windows::{ffi::OsStrExt, io::AsRawHandle},
        ptr::{null, null_mut},
    };
    use windows_sys::{
        Wdk::Storage::FileSystem::{
            FILE_RENAME_INFORMATION, FileRenameInformation, NtSetInformationFile,
        },
        Win32::{
            Foundation::{LocalFree, RtlNtStatusToDosError},
            Security::{
                Authorization::{GetSecurityInfo, SE_FILE_OBJECT, SetSecurityInfo},
                DACL_SECURITY_INFORMATION, GetSecurityDescriptorControl,
                PROTECTED_DACL_SECURITY_INFORMATION, SE_DACL_PROTECTED,
                UNPROTECTED_DACL_SECURITY_INFORMATION,
            },
            Storage::FileSystem::{
                DELETE, FILE_ATTRIBUTE_ENCRYPTED, FILE_ATTRIBUTE_HIDDEN,
                FILE_ATTRIBUTE_NOT_CONTENT_INDEXED, FILE_ATTRIBUTE_SYSTEM, FILE_BASIC_INFO,
                FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_WRITE_ATTRIBUTES, FileBasicInfo,
                GetFileInformationByHandleEx, READ_CONTROL, SetFileInformationByHandle, WRITE_DAC,
            },
            System::IO::IO_STATUS_BLOCK,
        },
    };

    pub(crate) fn replace(
        parent: &Dir,
        temp: &str,
        name: &str,
        backup: &str,
    ) -> Result<(), LibraryError> {
        // Without delete sharing, nothing else can move either file until
        // both renames are done.
        let open = |name: &str, access: u32| {
            let mut options = OpenOptions::new();
            options
                .access_mode(access)
                .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE);
            crate::path::open_checked(parent, name, &mut options)
        };
        let original = open(name, DELETE | READ_CONTROL)?;
        // Setting the access list reads the current one through the handle.
        let replacement = open(
            temp,
            DELETE | READ_CONTROL | WRITE_DAC | FILE_WRITE_ATTRIBUTES,
        )?;
        carry_over(&original, &replacement)?;
        rename(&original, parent, backup)?;
        // Stopping here leaves the original at `backup` and the save at
        // `temp`; the recovery record names both.
        rename(&replacement, parent, name)?;
        Ok(())
    }

    /// Gives the replacement what the replaced file had and a new file in the
    /// same folder would not: its access list, the attributes a person sets,
    /// and its creation time. The save's new file was created encrypted if
    /// the original is; one that could not be never replaces it.
    fn carry_over(original: &File, replacement: &File) -> io::Result<()> {
        let old = basic(original)?;
        let new = basic(replacement)?;
        if old.FileAttributes & FILE_ATTRIBUTE_ENCRYPTED != 0
            && new.FileAttributes & FILE_ATTRIBUTE_ENCRYPTED == 0
        {
            return Err(io::Error::other("the saved file would not be encrypted"));
        }
        let carried = FILE_BASIC_INFO {
            CreationTime: old.CreationTime,
            FileAttributes: new.FileAttributes
                | (old.FileAttributes
                    & (FILE_ATTRIBUTE_HIDDEN
                        | FILE_ATTRIBUTE_SYSTEM
                        | FILE_ATTRIBUTE_NOT_CONTENT_INDEXED)),
            // Zero leaves the other times as they are.
            ..FILE_BASIC_INFO::default()
        };
        // SAFETY: the handle is open and the structure lives for the call.
        if unsafe {
            SetFileInformationByHandle(
                replacement.as_raw_handle(),
                FileBasicInfo,
                (&raw const carried).cast(),
                size_of::<FILE_BASIC_INFO>() as u32,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        let mut dacl = null_mut();
        let mut descriptor = null_mut();
        // SAFETY: the handle is open; the outputs are writable. The access
        // list points into the descriptor, which is freed after its last use.
        unsafe {
            let status = GetSecurityInfo(
                original.as_raw_handle(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                null_mut(),
                null_mut(),
                &mut dacl,
                null_mut(),
                &mut descriptor,
            );
            if status != 0 {
                return Err(io::Error::from_raw_os_error(status as i32));
            }
            let mut control = 0;
            let mut revision = 0;
            let result =
                if GetSecurityDescriptorControl(descriptor, &mut control, &mut revision) == 0 {
                    Err(io::Error::last_os_error())
                } else {
                    // Inherited entries are the parent's, which both files share.
                    let inheritance = if control & SE_DACL_PROTECTED != 0 {
                        PROTECTED_DACL_SECURITY_INFORMATION
                    } else {
                        UNPROTECTED_DACL_SECURITY_INFORMATION
                    };
                    match SetSecurityInfo(
                        replacement.as_raw_handle(),
                        SE_FILE_OBJECT,
                        DACL_SECURITY_INFORMATION | inheritance,
                        null_mut(),
                        null_mut(),
                        dacl,
                        null(),
                    ) {
                        0 => Ok(()),
                        status => Err(io::Error::from_raw_os_error(status as i32)),
                    }
                };
            LocalFree(descriptor);
            result
        }
    }

    fn basic(file: &File) -> io::Result<FILE_BASIC_INFO> {
        let mut info = FILE_BASIC_INFO::default();
        // SAFETY: the handle is open and `info` is writable for its size.
        if unsafe {
            GetFileInformationByHandleEx(
                file.as_raw_handle(),
                FileBasicInfo,
                (&raw mut info).cast(),
                size_of::<FILE_BASIC_INFO>() as u32,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(info)
    }

    /// Renames the open `file` to `name` in `dir`, keeping any file already
    /// there. `name` is resolved from the directory's handle, not a path.
    fn rename(file: &File, dir: &Dir, name: &str) -> io::Result<()> {
        let name: Vec<u16> = OsStr::new(name).encode_wide().collect();
        // The structure ends in the first unit of a name that runs past it.
        let size = size_of::<FILE_RENAME_INFORMATION>() + size_of_val(name.as_slice());
        let too_long = || io::Error::from(io::ErrorKind::InvalidFilename);
        let length = u32::try_from(size_of_val(name.as_slice())).map_err(|_| too_long())?;
        let size_u32 = u32::try_from(size).map_err(|_| too_long())?;
        let mut buffer = vec![0u64; size.div_ceil(size_of::<u64>())];
        let info = buffer.as_mut_ptr().cast::<FILE_RENAME_INFORMATION>();
        // SAFETY: the zeroed buffer is aligned for the structure and holds it
        // and the whole name.
        unsafe {
            (*info).Anonymous.ReplaceIfExists = false;
            (*info).RootDirectory = dir.as_raw_handle();
            (*info).FileNameLength = length;
            std::ptr::copy_nonoverlapping(
                name.as_ptr(),
                (&raw mut (*info).FileName).cast::<u16>(),
                name.len(),
            );
        }
        let mut io_status = IO_STATUS_BLOCK::default();
        // SAFETY: both handles are open for the call, and `size_u32` bytes
        // of the buffer are initialized.
        let status = unsafe {
            NtSetInformationFile(
                file.as_raw_handle(),
                &mut io_status,
                info.cast_const().cast(),
                size_u32,
                FileRenameInformation,
            )
        };
        if status < 0 {
            // SAFETY: a pure status code conversion.
            let code = unsafe { RtlNtStatusToDosError(status) };
            return Err(io::Error::from_raw_os_error(code as i32));
        }
        Ok(())
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
pub(crate) fn replace(
    _parent: &Dir,
    _temp: &str,
    _name: &str,
    _backup: &str,
) -> Result<(), LibraryError> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "safe replacement is unavailable on this platform",
    )
    .into())
}
