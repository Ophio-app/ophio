// SPDX-License-Identifier: Apache-2.0
//! ShellExecute receives a verified shortcut path or a validated web address as
//! its file argument, with no command-line parameters or spoken shell program.
use super::*;
use std::{
    fs::{File, OpenOptions},
    os::windows::{ffi::OsStrExt, fs::OpenOptionsExt},
};
use windows::{
    Win32::{
        System::Com::{
            COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize,
        },
        UI::{Shell::ShellExecuteW, WindowsAndMessaging::SW_SHOWNORMAL},
    },
    core::PCWSTR,
};

#[derive(Debug)]
pub(super) struct Native {
    pub roots: Vec<PathBuf>,
}
struct Ready {
    target: Vec<u16>,
    // Deny deletion/rename of every ancestor; deny writes to the shortcut.
    _locks: Vec<File>,
    // Initialized before authority is checked, and confined to this worker.
    _apartment: Apartment,
}
impl Prepared for Ready {
    fn open(self: Box<Self>) -> Result<(), Error> {
        let verb: Vec<u16> = "open\0".encode_utf16().collect();
        // SAFETY: Strings are NUL-terminated and live throughout the call. No
        // parameters, working directory, or window handle are supplied.
        let result = unsafe {
            ShellExecuteW(
                None,
                PCWSTR(verb.as_ptr()),
                PCWSTR(self.target.as_ptr()),
                PCWSTR::null(),
                PCWSTR::null(),
                SW_SHOWNORMAL,
            )
        };
        if result.0 as isize > 32 {
            Ok(())
        } else {
            Err(unavailable("Windows refused to open the app or address"))
        }
    }
}
struct Apartment(std::marker::PhantomData<std::rc::Rc<()>>);
impl Apartment {
    fn new() -> Result<Self, Error> {
        // SAFETY: This initializes only the current thread; the matching
        // uninitialization is confined to this thread by the non-Send guard.
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) }
            .ok()
            .map_err(|_| unavailable("Windows app launching could not initialize"))?;
        Ok(Self(std::marker::PhantomData))
    }
}
impl Drop for Apartment {
    fn drop(&mut self) {
        // SAFETY: Created only after a successful initialization on this thread.
        unsafe { CoUninitialize() };
    }
}
fn target(path: &std::ffi::OsStr) -> Result<Vec<u16>, Error> {
    let mut wide: Vec<u16> = path.encode_wide().collect();
    if wide.contains(&0) {
        return Err(Error::StaleTarget);
    }
    wide.push(0);
    Ok(wide)
}
impl Platform for Native {
    fn entries(&self) -> Result<Vec<Entry>, Error> {
        discover(&self.roots)
    }
    fn prepare(&self, entry: &Entry) -> Result<Box<dyn Prepared>, Error> {
        if !entry.path.is_absolute() || !self.roots.iter().any(|root| entry.path.starts_with(root))
        {
            return Err(Error::StaleTarget);
        }
        let mut locks = Vec::new();
        // Top-down locking matters: opening only the file leaves its parent
        // replaceable, and a valid byte-for-byte copy could redirect the launch.
        let ancestors: Vec<_> = entry.path.ancestors().collect();
        for path in ancestors.into_iter().rev() {
            let leaf = path == entry.path;
            let mut options = OpenOptions::new();
            options
                .read(true)
                // FILE_SHARE_READ only: also exclude directory writers that
                // could turn an ancestor into a reparse point while locked.
                .share_mode(1)
                // OPEN_REPARSE_POINT and BACKUP_SEMANTICS (directory handles).
                .custom_flags(0x00200000 | 0x02000000);
            let file = options.open(path).map_err(|_| Error::StaleTarget)?;
            use std::os::windows::fs::MetadataExt;
            let metadata = file.metadata().map_err(|_| Error::StaleTarget)?;
            if metadata.file_attributes() & 0x400 != 0 || (leaf && !metadata.is_file()) {
                return Err(Error::StaleTarget);
            }
            locks.push(file);
        }
        if read_shortcut(&entry.path)? != entry.bytes {
            return Err(Error::StaleTarget);
        }
        Ok(Box::new(Ready {
            target: target(entry.path.as_os_str())?,
            _locks: locks,
            _apartment: Apartment::new()?,
        }))
    }
    fn packaged_entries(&self) -> Result<Vec<PackagedApp>, Error> {
        super::apps_folder::entries()
    }
    fn prepare_packaged(&self, app: &PackagedApp) -> Result<Box<dyn Prepared>, Error> {
        super::apps_folder::prepare(app)
    }
    fn packaged_defaults(&self) -> Result<BTreeMap<AppRole, String>, Error> {
        super::apps_folder::defaults()
    }
    fn prepare_url(&self, url: &str) -> Result<Box<dyn Prepared>, Error> {
        Ok(Box::new(Ready {
            target: target(std::ffi::OsStr::new(url))?,
            _locks: vec![],
            _apartment: Apartment::new()?,
        }))
    }
}
