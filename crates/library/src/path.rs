// SPDX-License-Identifier: Apache-2.0
use cap_fs_ext::{DirExt, FollowSymlinks, MetadataExt, OpenOptionsFollowExt};
use cap_std::fs::{Dir, OpenOptions};
use ports::library::{LibraryError, RootSpec};
#[cfg(not(windows))]
use same_file::Handle as Identity;
use std::{
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

pub(crate) fn denied() -> LibraryError {
    LibraryError::PathNotAllowed("invalid path or filesystem identity".into())
}
pub(crate) fn io_error(e: io::Error) -> LibraryError {
    match e.kind() {
        io::ErrorKind::NotFound => LibraryError::NotFound,
        io::ErrorKind::AlreadyExists => LibraryError::AlreadyExists,
        _ => e.into(),
    }
}

/// Reject portable aliases as well as native traversal. Empty denotes the root.
pub fn normalize(path: &str) -> Result<String, LibraryError> {
    if path.starts_with('/') || path.contains(['\\', ':', '\0']) {
        return Err(denied());
    }
    let mut parts = Vec::new();
    for part in path.split('/') {
        if part == ".." {
            return Err(denied());
        }
        if part.is_empty() || part == "." {
            continue;
        }
        if part.ends_with(['.', ' ']) || part.chars().any(char::is_control) {
            return Err(denied());
        }
        if ports::library::reserved_on_windows(part) {
            return Err(denied());
        }
        if crate::internal_name(part) {
            return Err(denied());
        }
        parts.push(part);
    }
    Ok(parts.join("/"))
}

#[derive(Debug)]
pub(crate) struct Root {
    pub spec: RootSpec,
    pub canonical: PathBuf,
    #[cfg(not(windows))]
    dir: Dir,
    identity: Identity,
}

/// A directory used by one operation. Windows capability operations use paths
/// internally, so keep their non-delete-sharing root handle alive even after
/// traversal reaches a child. Verified local NTFS/ReFS roots can release that
/// movement restriction between operations; all other volumes retain it.
#[derive(Debug)]
pub(crate) struct Directory {
    dir: Dir,
    #[cfg(windows)]
    _root: Dir,
}

impl std::ops::Deref for Directory {
    type Target = Dir;

    fn deref(&self) -> &Self::Target {
        &self.dir
    }
}

impl Root {
    pub fn new(spec: RootSpec) -> Result<Self, LibraryError> {
        if !spec.path.is_absolute() || spec.path.to_str().is_none() {
            return Err(denied());
        }
        let metadata = fs::symlink_metadata(&spec.path).map_err(io_error)?;
        if !metadata.is_dir() || is_link(&metadata) {
            return Err(denied());
        }
        // Each path a root is limited to is one a request could name.
        if let Some(only) = &spec.only
            && only.iter().any(|entry| {
                !normalize(entry).is_ok_and(|normal| !normal.is_empty() && normal == *entry)
            })
        {
            return Err(denied());
        }
        let canonical = ports::library::canonical(&spec.path).map_err(io_error)?;
        #[cfg(not(windows))]
        let dir =
            Dir::open_ambient_dir(&canonical, cap_std::ambient_authority()).map_err(io_error)?;
        #[cfg(not(windows))]
        let identity = Identity::from_file(dir.try_clone()?.into_std_file())?;
        #[cfg(windows)]
        let identity = pin_directory(&canonical)?;
        let root = Self {
            spec,
            canonical,
            #[cfg(not(windows))]
            dir,
            identity,
        };
        root.check()?;
        Ok(root)
    }

    pub fn check(&self) -> Result<(), LibraryError> {
        let meta = fs::symlink_metadata(&self.spec.path).map_err(|_| denied())?;
        if !meta.is_dir()
            || is_link(&meta)
            || ports::library::canonical(&self.spec.path).map_err(|_| denied())? != self.canonical
        {
            return Err(denied());
        }
        let current = Dir::open_ambient_dir(&self.spec.path, cap_std::ambient_authority())
            .map_err(|_| denied())?;
        if Identity::from_file(current.into_std_file())? != self.identity {
            return Err(denied());
        }
        Ok(())
    }

    /// Mutation algorithms require native hard links and persistent ACLs. A
    /// failed capability query refuses writes, never read-only registration.
    #[cfg(windows)]
    pub fn ensure_mutations_supported(&self) -> Result<(), LibraryError> {
        self.check()?;
        if !windows_mutations_supported(volume_flags(&self.identity.file)) {
            return Err(LibraryError::UnsupportedVolume);
        }
        Ok(())
    }

    /// Where `path` lies inside this root, `/`-separated, and the form of
    /// the root it was found under: the path the root was added by, or the
    /// place that path leads. Only a path the root holds is in it.
    pub fn within(&self, path: &Path) -> Option<(PathBuf, String)> {
        [&self.spec.path, &self.canonical]
            .into_iter()
            .find_map(|prefix| {
                let relative = path.strip_prefix(prefix).ok()?.to_str()?;
                let relative = normalize(&relative.replace(std::path::MAIN_SEPARATOR, "/")).ok()?;
                Some((prefix.clone(), relative))
            })
            .filter(|(_, relative)| self.holds(relative))
    }

    /// Whether `relative` is one of the root's own paths: anything in it,
    /// unless it is limited to some.
    pub fn holds(&self, relative: &str) -> bool {
        protocol::library::holds(self.spec.only.as_deref(), relative)
    }

    /// Whether the folder `relative` is one of the root's paths or on the
    /// way to one.
    pub fn reaches(&self, relative: &str) -> bool {
        protocol::library::reaches(self.spec.only.as_deref(), relative)
    }

    /// Whether this root reaches `path` on the computer, or anything in it.
    pub fn meets(&self, path: &Path) -> bool {
        ports::library::meets(&self.canonical, self.spec.only.as_deref(), path)
    }

    pub fn directory(&self, relative: &str) -> Result<Directory, LibraryError> {
        if !self.reaches(relative) {
            return Err(denied());
        }
        self.check()?;
        #[cfg(not(windows))]
        let mut dir = self.dir.try_clone()?;
        #[cfg(windows)]
        let root = {
            // cap-std deliberately denies delete sharing on directory handles.
            // Acquire that protection for this operation only, and compare the
            // opened handle before using it, not merely its ambient path.
            let root = Dir::open_ambient_dir(&self.spec.path, cap_std::ambient_authority())
                .map_err(|_| denied())?;
            if Identity::from_file(root.try_clone()?.into_std_file())? != self.identity {
                return Err(denied());
            }
            root
        };
        #[cfg(windows)]
        let mut dir = root.try_clone()?;
        for component in relative.split('/').filter(|p| !p.is_empty()) {
            let meta = dir.symlink_metadata(component).map_err(io_error)?;
            if !meta.is_dir() || meta.file_type().is_symlink() {
                return Err(denied());
            }
            dir = dir.open_dir_nofollow(component).map_err(|_| denied())?;
        }
        self.check()?;
        Ok(Directory {
            dir,
            #[cfg(windows)]
            _root: root,
        })
    }

    pub fn parent(&self, relative: &str) -> Result<(Directory, String), LibraryError> {
        let relative = normalize(relative)?;
        if relative.is_empty() || !self.holds(&relative) {
            return Err(denied());
        }
        let (parent, name) = relative.rsplit_once('/').unwrap_or(("", &relative));
        Ok((self.directory(parent)?, name.into()))
    }

    pub fn verify_parent(&self, relative: &str, parent: &Dir) -> Result<(), LibraryError> {
        let (current, _) = self.parent(relative)?;
        if Identity::from_file(current.try_clone()?.into_std_file())?
            != Identity::from_file(parent.try_clone()?.into_std_file())?
        {
            return Err(denied());
        }
        Ok(())
    }
}

/// All 128 identifier bits and the full volume serial participate in Windows
/// identity checks. The legacy 64-bit file index is not unique on ReFS.
#[cfg(any(windows, test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct WindowsFileId {
    volume_serial: u64,
    file_id: [u8; 16],
}

/// Complete legacy identity, used only when the native full-ID query is
/// explicitly unsupported. It is safe for root pins only with strict sharing.
#[cfg(any(windows, test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct WindowsLegacyId {
    volume_serial: u32,
    file_index: u64,
}

/// Never compare a full identity to a truncated legacy one. A query changing
/// support cannot silently downgrade an existing full-ID identity check.
#[cfg(any(windows, test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WindowsIdentityKey {
    Full(WindowsFileId),
    Legacy(WindowsLegacyId),
}

#[cfg(any(windows, test))]
fn windows_identity_key(
    full: io::Result<WindowsFileId>,
    legacy: impl FnOnce() -> io::Result<WindowsLegacyId>,
) -> io::Result<WindowsIdentityKey> {
    match full {
        Ok(id) => Ok(WindowsIdentityKey::Full(id)),
        // ERROR_INVALID_FUNCTION, ERROR_NOT_SUPPORTED, ERROR_INVALID_PARAMETER.
        // Permission, device and transport failures must not enable fallback.
        Err(error) if matches!(error.raw_os_error(), Some(1 | 50 | 87)) => {
            legacy().map(WindowsIdentityKey::Legacy)
        }
        Err(error) => Err(error),
    }
}

/// Keep the handle open while its identity is used. Even a full native ID must
/// not be treated as a permanent name after the underlying handle is closed.
#[cfg(windows)]
#[derive(Debug)]
struct Identity {
    file: fs::File,
    id: WindowsIdentityKey,
}

#[cfg(windows)]
impl Identity {
    fn from_file(file: fs::File) -> io::Result<Self> {
        let id = windows_identity_key(windows_file_id(&file), || windows_legacy_id(&file))?;
        Ok(Self { file, id })
    }
}

#[cfg(windows)]
impl PartialEq for Identity {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

#[cfg(windows)]
impl Eq for Identity {}

#[cfg(windows)]
#[allow(unsafe_code)]
fn windows_file_id(file: &fs::File) -> io::Result<WindowsFileId> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ID_INFO, FileIdInfo, GetFileInformationByHandleEx,
    };

    let mut info = FILE_ID_INFO::default();
    // SAFETY: the file remains open and `info` is writable for its exact size.
    if unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileIdInfo,
            (&raw mut info).cast(),
            size_of::<FILE_ID_INFO>() as u32,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(WindowsFileId {
        volume_serial: info.VolumeSerialNumber,
        file_id: info.FileId.Identifier,
    })
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn windows_legacy_id(file: &fs::File) -> io::Result<WindowsLegacyId> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
    };

    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: the file remains open and `info` is a correctly sized output.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &raw mut info) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(WindowsLegacyId {
        volume_serial: info.dwVolumeSerialNumber,
        file_index: (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
    })
}

/// A query failure, unknown filesystem, or remote device must retain the
/// strict capability pin. FAT-family identifiers can be reused after a move.
#[cfg(any(windows, test))]
fn rename_friendly_volume(filesystem: Option<&str>, local_disk: bool) -> bool {
    local_disk
        && filesystem.is_some_and(|name| {
            name.eq_ignore_ascii_case("NTFS") || name.eq_ignore_ascii_case("ReFS")
        })
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn volume_filesystem(file: &fs::File) -> Option<String> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::GetVolumeInformationByHandleW;

    let mut name = [0u16; 256];
    // SAFETY: the file remains open; omitted outputs are optional, and `name`
    // is writable for the supplied number of UTF-16 units.
    if unsafe {
        GetVolumeInformationByHandleW(
            file.as_raw_handle(),
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            name.as_mut_ptr(),
            name.len() as u32,
        )
    } == 0
    {
        return None;
    }
    let end = name.iter().position(|&unit| unit == 0)?;
    String::from_utf16(&name[..end]).ok()
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn volume_flags(file: &fs::File) -> Option<u32> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::GetVolumeInformationByHandleW;

    let mut flags = 0;
    // SAFETY: the handle remains open, `flags` is writable, and all omitted
    // information fields are documented optional outputs.
    let result = unsafe {
        GetVolumeInformationByHandleW(
            file.as_raw_handle(),
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &raw mut flags,
            std::ptr::null_mut(),
            0,
        )
    };
    (result != 0).then_some(flags)
}

#[cfg(windows)]
fn windows_mutations_supported(flags: Option<u32>) -> bool {
    use windows_sys::Win32::System::SystemServices::{
        FILE_PERSISTENT_ACLS, FILE_SUPPORTS_HARD_LINKS,
    };
    let required = FILE_PERSISTENT_ACLS | FILE_SUPPORTS_HARD_LINKS;
    flags.is_some_and(|flags| flags & required == required)
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn local_disk_volume(file: &fs::File) -> bool {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::{
        Wdk::{
            Storage::FileSystem::{FileFsDeviceInformation, NtQueryVolumeInformationFile},
            System::SystemServices::FILE_FS_DEVICE_INFORMATION,
        },
        Win32::System::IO::IO_STATUS_BLOCK,
    };

    let mut info = FILE_FS_DEVICE_INFORMATION::default();
    let mut status = IO_STATUS_BLOCK::default();
    // SAFETY: the synchronous file handle stays open and both output objects
    // remain writable for the duration of this handle-based query.
    let result = unsafe {
        NtQueryVolumeInformationFile(
            file.as_raw_handle(),
            &raw mut status,
            (&raw mut info).cast(),
            size_of::<FILE_FS_DEVICE_INFORMATION>() as u32,
            FileFsDeviceInformation,
        )
    };
    result == 0
        && status.Information == size_of::<FILE_FS_DEVICE_INFORMATION>()
        && local_disk_device(info.DeviceType, info.Characteristics)
}

#[cfg(windows)]
fn local_disk_device(device_type: u32, characteristics: u32) -> bool {
    use windows_sys::{
        Wdk::System::SystemServices::{FILE_REMOTE_DEVICE, FILE_REMOVABLE_MEDIA},
        Win32::Storage::FileSystem::FILE_DEVICE_DISK,
    };
    // Native FileFsDeviceInformation reports the underlying disk device (7),
    // not the disk-filesystem driver device (8).
    device_type == FILE_DEVICE_DISK
        && characteristics & (FILE_REMOTE_DEVICE | FILE_REMOVABLE_MEDIA) == 0
}

/// Classify while a strict handle already protects the directory. Unknown or
/// unsupported volumes never acquire an unprotected, rename-friendly pin.
#[cfg(windows)]
fn pin_directory(path: &Path) -> Result<Identity, LibraryError> {
    let strict = Dir::open_ambient_dir(path, cap_std::ambient_authority()).map_err(io_error)?;
    let strict = Identity::from_file(strict.into_std_file())?;
    let filesystem = volume_filesystem(&strict.file);
    let allow_rename =
        rename_friendly_volume(filesystem.as_deref(), local_disk_volume(&strict.file));
    select_directory_pin(path, strict, allow_rename)
}

/// Never use a rename-friendly identity handle for capability operations:
/// those still require the temporary protection in `Root::directory`.
#[cfg(windows)]
fn select_directory_pin(
    path: &Path,
    strict: Identity,
    allow_rename: bool,
) -> Result<Identity, LibraryError> {
    // Legacy FAT-family indices can be reused after movement. Never exchange
    // their original strict handle, even if the volume reports as NTFS/ReFS.
    if !allow_rename || !matches!(strict.id, WindowsIdentityKey::Full(_)) {
        return Ok(strict);
    }
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE,
        FILE_SHARE_READ, FILE_SHARE_WRITE,
    };

    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .open(path)
        .map_err(io_error)?;
    let meta = file.metadata()?;
    if !meta.is_dir() || is_link(&meta) {
        return Err(denied());
    }
    let friendly = Identity::from_file(file)?;
    if friendly != strict {
        return Err(denied());
    }
    Ok(friendly)
}

pub(crate) fn is_link(meta: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        meta.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        meta.file_type().is_symlink()
    }
}

pub(crate) fn open_file(dir: &Dir, name: &str) -> Result<fs::File, LibraryError> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use cap_std::fs::OpenOptionsExt;
        options.custom_flags(rustix::fs::OFlags::NONBLOCK.bits() as i32);
    }
    open_checked(dir, name, &mut options)
}

/// Opens `name` in `dir` without following a link, refusing anything but a
/// regular file with a single name.
pub(crate) fn open_checked(
    dir: &Dir,
    name: &str,
    options: &mut OpenOptions,
) -> Result<fs::File, LibraryError> {
    let meta = dir.symlink_metadata(name).map_err(io_error)?;
    if !meta.is_file() || meta.file_type().is_symlink() {
        return Err(denied());
    }
    let file = dir
        .open_with(name, options.follow(FollowSymlinks::No))
        .map_err(io_error)?
        .into_std();
    let meta = file.metadata()?;
    // A hard link can name a file outside the root. Its count is read through
    // the handle, which std's metadata cannot do on Windows.
    if !meta.is_file() || is_link(&meta) || cap_std::fs::Metadata::from_file(&file)?.nlink() != 1 {
        return Err(denied());
    }
    // Windows also opens a file by its generated 8.3 short name, such as
    // `OPHIO-~1`. A name that isn't the Library's own must not reach its files.
    #[cfg(windows)]
    if !crate::internal_name(name) && crate::internal_name(&own_name(&file)?.to_string_lossy()) {
        return Err(denied());
    }
    Ok(file)
}

/// The open file's own long name, whatever name opened it.
#[cfg(windows)]
#[allow(unsafe_code)]
fn own_name(file: &fs::File) -> io::Result<std::ffi::OsString> {
    use std::os::windows::{ffi::OsStringExt, io::AsRawHandle};
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_NAME_NORMALIZED, GetFinalPathNameByHandleW, VOLUME_NAME_NONE,
    };
    let mut path = vec![0u16; 1024];
    loop {
        let capacity = u32::try_from(path.len())
            .map_err(|_| io::Error::from(io::ErrorKind::InvalidFilename))?;
        // SAFETY: the handle is open and `path` is writable for `capacity` units.
        let written = unsafe {
            GetFinalPathNameByHandleW(
                file.as_raw_handle(),
                path.as_mut_ptr(),
                capacity,
                FILE_NAME_NORMALIZED | VOLUME_NAME_NONE,
            )
        } as usize;
        match written {
            0 => return Err(io::Error::last_os_error()),
            // Too small: `written` is the size it needs, terminator included.
            needed if needed >= path.len() => path.resize(needed, 0),
            length => {
                let separator = u16::from(b'\\');
                let name = path[..length].rsplit(|&unit| unit == separator).next();
                return Ok(std::ffi::OsString::from_wide(name.unwrap_or_default()));
            }
        }
    }
}

pub(crate) fn read_file(
    dir: &Dir,
    name: &str,
    cap: u64,
) -> Result<(Vec<u8>, fs::Metadata), LibraryError> {
    let file = open_file(dir, name)?;
    let before = file.metadata()?;
    if before.len() > cap {
        return Err(LibraryError::TooLarge(before.len()));
    }
    let mut bytes = Vec::new();
    (&file)
        .take(cap.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > cap {
        return Err(LibraryError::TooLarge(bytes.len() as u64));
    }
    let after = file.metadata()?;
    let named = open_file(dir, name)?;
    if before.len() != after.len()
        || before.modified().ok() != after.modified().ok()
        || Identity::from_file(file)? != Identity::from_file(named)?
    {
        return Err(LibraryError::Conflict);
    }
    Ok((bytes, after))
}

/// Writes a new file. `like`, the file it stands in for, gives it that file's
/// permissions and, on Windows, its encryption, so the content of an
/// encrypted file is never written in plain text.
pub(crate) fn new_file(
    dir: &Dir,
    name: &str,
    bytes: &[u8],
    like: Option<&fs::Metadata>,
) -> Result<(), LibraryError> {
    let mut options = OpenOptions::new();
    options
        .write(true)
        .create_new(true)
        .follow(FollowSymlinks::No);
    #[cfg(unix)]
    {
        use cap_std::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    #[cfg(windows)]
    if let Some(like) = like {
        use cap_std::fs::OpenOptionsExt;
        use std::os::windows::fs::MetadataExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_ENCRYPTED;
        options.attributes(like.file_attributes() & FILE_ATTRIBUTE_ENCRYPTED);
    }
    let mut file = dir.open_with(name, &options).map_err(io_error)?.into_std();
    let result = (|| {
        file.write_all(bytes)?;
        if let Some(like) = like {
            file.set_permissions(like.permissions())?;
        }
        file.sync_all()
    })();
    if result.is_err() {
        let _ = dir.remove_file(name);
    }
    result.map_err(io_error)
}

pub(crate) fn sync_dir(dir: &Dir) -> Result<(), LibraryError> {
    #[cfg(unix)]
    directory_file(dir)?.sync_all()?;
    #[cfg(windows)]
    let _ = dir; // Windows does not expose a supported directory fsync.
    Ok(())
}

/// Publish without clobbering even when another writer wins the name race.
pub(crate) fn publish(dir: &Dir, temp: &str, name: &str) -> Result<(), LibraryError> {
    dir.hard_link(temp, dir, name).map_err(io_error)?;
    dir.remove_file(temp)
        .map_err(|_| LibraryError::OutcomeUnknown)?;
    sync_dir(dir).map_err(|_| LibraryError::OutcomeUnknown)
}

#[cfg(unix)]
pub(crate) fn directory_file(dir: &Dir) -> io::Result<fs::File> {
    // cap-std can hold O_PATH handles; reopen the directory itself for fsync.
    Ok(rustix::fs::openat(
        dir,
        ".",
        rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::DIRECTORY | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )?
    .into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::{ids::LibraryRootId, library::RootKind};

    fn root(path: PathBuf) -> Root {
        Root::new(RootSpec {
            root_id: LibraryRootId::new(),
            path,
            label: "Test".into(),
            kind: RootKind::Project,
            writable: true,
            project_id: None,
            harness: None,
            only: None,
        })
        .unwrap()
    }

    #[test]
    fn windows_identity_compares_every_file_id_and_volume_serial_bit() {
        let id = WindowsFileId {
            volume_serial: 0,
            file_id: [0; 16],
        };
        assert_eq!(id, id);
        for bit in 0..128 {
            let mut other = id;
            other.file_id[bit / 8] = 1 << (bit % 8);
            assert_ne!(id, other, "file ID bit {bit} must participate");
        }
        for bit in 0..64 {
            let mut other = id;
            other.volume_serial = 1 << bit;
            assert_ne!(id, other, "volume serial bit {bit} must participate");
        }
    }

    #[test]
    fn windows_legacy_fallback_is_limited_to_native_unsupported_errors() {
        let legacy = WindowsLegacyId {
            volume_serial: 0x1234_5678,
            file_index: 0x1234_5678_9abc_def0,
        };
        for error in [1, 50, 87] {
            assert_eq!(
                windows_identity_key(Err(io::Error::from_raw_os_error(error)), || Ok(legacy))
                    .unwrap(),
                WindowsIdentityKey::Legacy(legacy)
            );
        }
        for error in [0, 2, 5, 6, 32, 53, 64, 1117] {
            let result = windows_identity_key(Err(io::Error::from_raw_os_error(error)), || {
                panic!("native error {error} must not enable a legacy query")
            });
            assert_eq!(result.unwrap_err().raw_os_error(), Some(error));
        }
        assert!(
            windows_identity_key(Err(io::Error::other("unknown")), || {
                panic!("an error without a native code must not enable fallback")
            })
            .is_err()
        );
        let failed = windows_identity_key(Err(io::Error::from_raw_os_error(87)), || {
            Err(io::Error::from_raw_os_error(5))
        });
        assert_eq!(failed.unwrap_err().raw_os_error(), Some(5));
        let full = WindowsFileId {
            volume_serial: u64::from(legacy.volume_serial),
            file_id: [0; 16],
        };
        assert_eq!(
            windows_identity_key(Ok(full), || panic!("full identity must not be truncated"))
                .unwrap(),
            WindowsIdentityKey::Full(full)
        );
        assert_ne!(
            WindowsIdentityKey::Full(full),
            WindowsIdentityKey::Legacy(legacy)
        );
    }

    #[test]
    fn windows_legacy_identity_compares_every_index_and_volume_bit() {
        let id = WindowsLegacyId {
            volume_serial: 0,
            file_index: 0,
        };
        for bit in 0..64 {
            let mut other = id;
            other.file_index = 1 << bit;
            assert_ne!(id, other, "legacy file index bit {bit} must participate");
        }
        for bit in 0..32 {
            let mut other = id;
            other.volume_serial = 1 << bit;
            assert_ne!(id, other, "legacy volume serial bit {bit} must participate");
        }
    }

    #[test]
    fn windows_volume_policy_only_relaxes_verified_local_ntfs_and_refs() {
        for name in ["NTFS", "ntfs", "ReFS", "REFS"] {
            assert!(rename_friendly_volume(Some(name), true), "{name}");
            assert!(!rename_friendly_volume(Some(name), false), "{name}");
        }
        for name in [
            "FAT", "FAT32", "exFAT", "CDFS", "SMB", "", "unknown", "NTFS ",
        ] {
            assert!(!rename_friendly_volume(Some(name), true), "{name}");
            assert!(!rename_friendly_volume(Some(name), false), "{name}");
        }
        assert!(!rename_friendly_volume(None, true));
        assert!(!rename_friendly_volume(None, false));
    }

    #[cfg(windows)]
    #[test]
    fn windows_mutation_policy_requires_hard_links_and_persistent_acls() {
        use windows_sys::Win32::System::SystemServices::{
            FILE_PERSISTENT_ACLS, FILE_SUPPORTS_HARD_LINKS,
        };
        assert!(!windows_mutations_supported(None));
        assert!(!windows_mutations_supported(Some(0)));
        assert!(!windows_mutations_supported(Some(FILE_PERSISTENT_ACLS)));
        assert!(!windows_mutations_supported(Some(FILE_SUPPORTS_HARD_LINKS)));
        assert!(windows_mutations_supported(Some(
            FILE_PERSISTENT_ACLS | FILE_SUPPORTS_HARD_LINKS
        )));
        assert!(windows_mutations_supported(Some(u32::MAX)));
    }

    #[cfg(windows)]
    #[test]
    fn windows_device_policy_rejects_remote_and_unknown_devices() {
        use windows_sys::{
            Wdk::System::SystemServices::{FILE_REMOTE_DEVICE, FILE_REMOVABLE_MEDIA},
            Win32::{
                Storage::FileSystem::FILE_DEVICE_DISK,
                System::Ioctl::{FILE_DEVICE_DISK_FILE_SYSTEM, FILE_DEVICE_NETWORK_FILE_SYSTEM},
            },
        };
        assert!(local_disk_device(FILE_DEVICE_DISK, 0));
        // These are the characteristics measured on the native NTFS fixture;
        // unrelated characteristic bits do not make a local disk remote.
        assert!(local_disk_device(FILE_DEVICE_DISK, 0x0002_0020));
        assert!(!local_disk_device(FILE_DEVICE_DISK, FILE_REMOTE_DEVICE));
        assert!(!local_disk_device(FILE_DEVICE_DISK, FILE_REMOVABLE_MEDIA));
        assert!(!local_disk_device(
            FILE_DEVICE_DISK,
            FILE_REMOTE_DEVICE | FILE_REMOVABLE_MEDIA
        ));
        assert!(!local_disk_device(
            FILE_DEVICE_DISK,
            0x0002_0020 | FILE_REMOVABLE_MEDIA
        ));
        assert!(!local_disk_device(
            FILE_DEVICE_DISK,
            0x0002_0020 | FILE_REMOTE_DEVICE
        ));
        assert!(!local_disk_device(FILE_DEVICE_DISK_FILE_SYSTEM, 0));
        assert!(!local_disk_device(FILE_DEVICE_NETWORK_FILE_SYSTEM, 0));
        assert!(!local_disk_device(
            FILE_DEVICE_NETWORK_FILE_SYSTEM,
            FILE_REMOTE_DEVICE
        ));
        assert!(!local_disk_device(0, 0));
        assert!(!local_disk_device(u32::MAX, 0));
    }

    #[cfg(windows)]
    #[test]
    fn native_windows_identity_and_volume_queries_use_the_open_handle() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("root");
        let other = temp.path().join("other");
        fs::create_dir(&path).unwrap();
        fs::create_dir(&other).unwrap();
        let directory = Dir::open_ambient_dir(&path, cap_std::ambient_authority()).unwrap();
        let first = Identity::from_file(directory.try_clone().unwrap().into_std_file()).unwrap();
        let same = Identity::from_file(directory.into_std_file()).unwrap();
        let different = Identity::from_file(
            Dir::open_ambient_dir(&other, cap_std::ambient_authority())
                .unwrap()
                .into_std_file(),
        )
        .unwrap();
        assert_eq!(first, same);
        assert_ne!(first, different);
        assert_eq!(
            first.id,
            WindowsIdentityKey::Full(windows_file_id(&same.file).unwrap())
        );
        assert_eq!(
            windows_legacy_id(&first.file).unwrap(),
            windows_legacy_id(&same.file).unwrap()
        );
        assert_ne!(
            windows_legacy_id(&first.file).unwrap(),
            windows_legacy_id(&different.file).unwrap()
        );
        let filesystem = volume_filesystem(&first.file).expect("filesystem query must succeed");
        assert!(
            local_disk_volume(&first.file),
            "fixture must be on a local disk"
        );
        assert!(
            rename_friendly_volume(Some(&filesystem), true),
            "rename tests require a local NTFS/ReFS fixture, found {filesystem}"
        );

        let file_path = path.join("note.md");
        fs::write(&file_path, "note").unwrap();
        let file = fs::File::open(&file_path).unwrap();
        let same = Identity::from_file(file.try_clone().unwrap()).unwrap();
        assert_eq!(Identity::from_file(file).unwrap(), same);
        assert_ne!(first, same);
    }

    #[cfg(windows)]
    #[test]
    fn legacy_windows_identity_never_exchanges_its_original_strict_pin() {
        let temp = tempfile::tempdir().unwrap();
        for error in [1, 50, 87] {
            let path = temp.path().join(format!("root-{error}"));
            let moved = temp.path().join(format!("moved-{error}"));
            fs::create_dir(&path).unwrap();
            let file = Dir::open_ambient_dir(&path, cap_std::ambient_authority())
                .unwrap()
                .into_std_file();
            let id = windows_identity_key(Err(io::Error::from_raw_os_error(error)), || {
                windows_legacy_id(&file)
            })
            .unwrap();
            assert!(matches!(id, WindowsIdentityKey::Legacy(_)));
            let pin = select_directory_pin(&path, Identity { file, id }, true).unwrap();
            assert_eq!(pin.id, id);
            assert_eq!(
                fs::rename(&path, &moved).unwrap_err().raw_os_error(),
                Some(32)
            );
            drop(pin);
            fs::rename(&path, &moved).unwrap();
        }
    }

    #[cfg(windows)]
    #[test]
    fn strict_windows_pin_is_retained_for_unverified_or_unsupported_volumes() {
        let temp = tempfile::tempdir().unwrap();
        for (index, (filesystem, local_disk)) in [
            (None, true),
            (Some("FAT"), true),
            (Some("FAT32"), true),
            (Some("exFAT"), true),
            (Some("unknown"), true),
            (Some("NTFS"), false),
            (Some("ReFS"), false),
        ]
        .into_iter()
        .enumerate()
        {
            let path = temp.path().join(format!("root-{index}"));
            let moved = temp.path().join(format!("moved-{index}"));
            fs::create_dir(&path).unwrap();
            let strict = Identity::from_file(
                Dir::open_ambient_dir(&path, cap_std::ambient_authority())
                    .unwrap()
                    .into_std_file(),
            )
            .unwrap();
            let expected = strict.id;
            let allow_rename = rename_friendly_volume(filesystem, local_disk);
            assert!(!allow_rename);
            let pin = select_directory_pin(&path, strict, allow_rename).unwrap();
            assert_eq!(pin.id, expected);
            assert_eq!(
                fs::rename(&path, &moved).unwrap_err().raw_os_error(),
                Some(32),
                "{filesystem:?}, local disk: {local_disk} must retain the strict handle"
            );
            drop(pin);
            fs::rename(&path, &moved).unwrap();
        }
    }

    #[test]
    fn identity_pin_allows_moves_but_rejects_replacement_until_original_returns() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("root");
        let moved = temp.path().join("moved");
        fs::create_dir_all(path.join("child")).unwrap();
        let root = root(path.clone());

        fs::rename(&path, &moved).unwrap();
        assert!(root.check().is_err());
        assert!(root.directory("").is_err());
        fs::create_dir_all(path.join("child")).unwrap();
        assert!(matches!(root.check(), Err(LibraryError::PathNotAllowed(_))));
        for relative in ["", "child"] {
            assert!(matches!(
                root.directory(relative),
                Err(LibraryError::PathNotAllowed(_))
            ));
        }

        fs::remove_dir_all(&path).unwrap();
        fs::rename(&moved, &path).unwrap();
        root.check().unwrap();
        root.directory("child").unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn nested_identity_pin_is_not_dropped_to_allow_ancestor_moves() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("root");
        let moved = temp.path().join("moved");
        fs::create_dir_all(path.join("child")).unwrap();
        let parent = root(path.clone());
        let child = root(path.join("child"));

        // Windows also refuses ancestor moves for delete-sharing descendant
        // handles. Keep this identity pin rather than silently weakening it.
        assert_eq!(
            fs::rename(&path, &moved).unwrap_err().raw_os_error(),
            Some(5)
        );
        parent.check().unwrap();
        child.check().unwrap();
        drop(child);
        fs::rename(&path, &moved).unwrap();
        assert!(parent.check().is_err());
        fs::rename(&moved, &path).unwrap();
        parent.check().unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn operation_guards_block_root_and_child_moves_only_while_in_use() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("root");
        let moved = temp.path().join("moved");
        fs::create_dir_all(path.join("child")).unwrap();
        let root = root(path.clone());
        let directory = root.directory("child").unwrap();

        assert_eq!(
            fs::rename(&path, &moved).unwrap_err().raw_os_error(),
            Some(32)
        );
        assert_eq!(
            fs::rename(path.join("child"), path.join("moved-child"))
                .unwrap_err()
                .raw_os_error(),
            Some(32)
        );
        drop(directory);
        fs::rename(path.join("child"), path.join("moved-child")).unwrap();
        fs::rename(&path, &moved).unwrap();
        fs::rename(&moved, &path).unwrap();
        root.check().unwrap();
    }
}
