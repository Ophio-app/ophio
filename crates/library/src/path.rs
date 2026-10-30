// SPDX-License-Identifier: Apache-2.0
use cap_fs_ext::{DirExt, FollowSymlinks, MetadataExt, OpenOptionsFollowExt};
use cap_std::fs::{Dir, OpenOptions};
use ports::library::{LibraryError, RootSpec};
use same_file::Handle;
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
    pub dir: Dir,
    identity: Handle,
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
        let dir =
            Dir::open_ambient_dir(&canonical, cap_std::ambient_authority()).map_err(io_error)?;
        let identity = Handle::from_file(dir.try_clone()?.into_std_file())?;
        let root = Self {
            spec,
            canonical,
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
        if Handle::from_file(current.into_std_file())? != self.identity {
            return Err(denied());
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

    pub fn directory(&self, relative: &str) -> Result<Dir, LibraryError> {
        if !self.reaches(relative) {
            return Err(denied());
        }
        self.check()?;
        let mut dir = self.dir.try_clone()?;
        for component in relative.split('/').filter(|p| !p.is_empty()) {
            let meta = dir.symlink_metadata(component).map_err(io_error)?;
            if !meta.is_dir() || meta.file_type().is_symlink() {
                return Err(denied());
            }
            dir = dir.open_dir_nofollow(component).map_err(|_| denied())?;
        }
        self.check()?;
        Ok(dir)
    }

    pub fn parent(&self, relative: &str) -> Result<(Dir, String), LibraryError> {
        let relative = normalize(relative)?;
        if relative.is_empty() || !self.holds(&relative) {
            return Err(denied());
        }
        let (parent, name) = relative.rsplit_once('/').unwrap_or(("", &relative));
        Ok((self.directory(parent)?, name.into()))
    }

    pub fn verify_parent(&self, relative: &str, parent: &Dir) -> Result<(), LibraryError> {
        let (current, _) = self.parent(relative)?;
        if Handle::from_file(current.into_std_file())?
            != Handle::from_file(parent.try_clone()?.into_std_file())?
        {
            return Err(denied());
        }
        Ok(())
    }
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
        || Handle::from_file(file)? != Handle::from_file(named)?
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
