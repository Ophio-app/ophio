// SPDX-License-Identifier: Apache-2.0
//! Small system-call boundary. pre_exec closures only use async-signal-safe syscalls.
#[cfg(target_os = "linux")]
#[path = "sys_linux.rs"]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::*;
#[cfg(target_os = "linux")]
use std::os::unix::net::UnixListener;
use std::{
    ffi::CString,
    fs::File,
    io,
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::ffi::{OsStrExt, OsStringExt},
    },
    path::{Component, Path},
};

#[cfg(target_os = "linux")]
fn cpath(path: &Path) -> io::Result<CString> {
    CString::new(path.as_os_str().as_bytes()).map_err(io::Error::other)
}
fn owned(fd: i32) -> io::Result<OwnedFd> {
    if fd < 0 {
        Err(io::Error::last_os_error())
    } else {
        // SAFETY: successful open/openat yields a fresh owned descriptor.
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }
}
fn directory(path: &Path, create: bool) -> io::Result<OwnedFd> {
    if !path.is_absolute() {
        return Err(io::Error::other("path must be absolute"));
    }
    // SAFETY: static nul-terminated path and valid flags.
    let mut fd = owned(unsafe {
        libc::open(
            c"/".as_ptr(),
            libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_RDONLY,
        )
    })?;
    for part in path.components() {
        let Component::Normal(name) = part else {
            if part == Component::RootDir {
                continue;
            }
            return Err(io::Error::other("non-normal path"));
        };
        let name = CString::new(name.as_bytes()).map_err(io::Error::other)?;
        if create {
            // SAFETY: live directory descriptor and nul-terminated component.
            let result = unsafe { libc::mkdirat(fd.as_raw_fd(), name.as_ptr(), 0o700) };
            if result < 0 && io::Error::last_os_error().raw_os_error() != Some(libc::EEXIST) {
                return Err(io::Error::last_os_error());
            }
        }
        // SAFETY: O_NOFOLLOW refuses symlinks at each component, including agent-writable state.
        fd = owned(unsafe {
            libc::openat(
                fd.as_raw_fd(),
                name.as_ptr(),
                libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_RDONLY,
            )
        })?;
    }
    Ok(fd)
}
pub fn directories(path: &Path) -> io::Result<()> {
    directory(path, true).map(drop)
}
/// Holds the folder at `path` open, reached through no link.
pub fn folder(path: &Path) -> io::Result<File> {
    directory(path, false).map(File::from)
}
/// Opens the plain file `name` in the folder held by `parent`, without
/// following a link or waiting on a pipe.
pub fn child(parent: &File, name: &str) -> io::Result<File> {
    let name = CString::new(name).map_err(io::Error::other)?;
    // SAFETY: live directory descriptor, nul-terminated name and valid flags.
    let file = File::from(owned(unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
        )
    })?);
    if !file.metadata()?.is_file() {
        return Err(io::Error::other("not a plain file"));
    }
    Ok(file)
}
/// Listens on `name` in `directory`. A socket address holds at most 107 bytes
/// of path, which state under a long home folder exceeds; such a name is bound
/// through the directory's descriptor instead, so it lands in the same place.
#[cfg(target_os = "linux")]
pub fn bind_unix(directory: &Path, name: &str) -> io::Result<UnixListener> {
    match UnixListener::bind(directory.join(name)) {
        Err(e) if e.kind() == io::ErrorKind::InvalidInput => {
            let handle = File::open(directory)?;
            UnixListener::bind(
                Path::new(&format!("/proc/self/fd/{}", handle.as_raw_fd())).join(name),
            )
        }
        bound => bound,
    }
}
/// Keep the directory open while enumerating it. As with history files,
/// no component may redirect the walk through a symlink.
pub fn history_entries(path: &Path) -> io::Result<(File, std::fs::ReadDir)> {
    let directory = File::from(directory(path, false)?);
    #[cfg(target_os = "linux")]
    let entries = std::fs::read_dir(format!("/proc/self/fd/{}", directory.as_raw_fd()))?;
    // A Mac has no such path for a descriptor. It lists by the path just checked
    // not to pass through any link.
    #[cfg(not(target_os = "linux"))]
    let entries = std::fs::read_dir(path)?;
    Ok((directory, entries))
}
pub fn history_file(path: &Path, write: bool) -> io::Result<File> {
    let parent = directory(
        path.parent().ok_or_else(|| io::Error::other("no parent"))?,
        false,
    )?;
    let name = CString::new(
        path.file_name()
            .ok_or_else(|| io::Error::other("no filename"))?
            .as_bytes(),
    )
    .map_err(io::Error::other)?;
    let flags = libc::O_CLOEXEC
        | libc::O_NOFOLLOW
        | libc::O_NONBLOCK
        | if write {
            libc::O_WRONLY | libc::O_CREAT
        } else {
            libc::O_RDONLY
        };
    // SAFETY: live directory fd, checked name, valid flags and creation mode.
    let fd = owned(unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), flags, 0o600) })?;
    let file = File::from(fd);
    if !file.metadata()?.is_file() || std::os::unix::fs::MetadataExt::nlink(&file.metadata()?) != 1
    {
        return Err(io::Error::other(
            "history must be a regular, unlinked-to-other-files file",
        ));
    }
    Ok(file)
}
/// Replaces `path` with what `write` puts in a new file beside it. Readers see
/// the old file or the whole new one: a copy that fails or is killed never
/// publishes part of a transcript, and a planted link is replaced, not written through.
pub fn replace_history(
    path: &Path,
    write: impl FnOnce(&mut File) -> io::Result<()>,
) -> io::Result<()> {
    static STAGED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let dir = path.parent().ok_or_else(|| io::Error::other("no parent"))?;
    let parent = directory(dir, false)?;
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::other("no filename"))?
        .as_bytes();
    let prefix = [b".", name, b"."].concat();
    sweep(&parent, dir, &prefix);
    let (staging, mut file) = loop {
        let n = STAGED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let staging = [
            &prefix[..],
            format!("{}.{n}{PARTIAL}", std::process::id()).as_bytes(),
        ]
        .concat();
        let staging = CString::new(staging).map_err(io::Error::other)?;
        // SAFETY: live directory fd, checked name, valid flags and creation mode.
        let fd = match owned(unsafe {
            libc::openat(
                parent.as_raw_fd(),
                staging.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600,
            )
        }) {
            Ok(fd) => fd,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        };
        #[cfg(test)]
        if let Some(hook) = BEFORE_LOCK.take() {
            hook();
        }
        let file = File::from(fd);
        // The lock tells `sweep` this staging file is still being written. A
        // sweep that locked it first has unlinked it, so stage another.
        match file.lock().and_then(|()| file.metadata()) {
            Ok(metadata) if std::os::unix::fs::MetadataExt::nlink(&metadata) > 0 => {
                break (staging, file);
            }
            Ok(_) => continue,
            Err(e) => {
                // SAFETY: live directory fd and nul-terminated name.
                unsafe { libc::unlinkat(parent.as_raw_fd(), staging.as_ptr(), 0) };
                return Err(e);
            }
        }
    };
    let published = write(&mut file)
        .and_then(|()| file.sync_all())
        .and_then(|()| {
            let name = CString::new(name).map_err(io::Error::other)?;
            // SAFETY: live directory fd and nul-terminated names in it.
            let result = unsafe {
                libc::renameat(
                    parent.as_raw_fd(),
                    staging.as_ptr(),
                    parent.as_raw_fd(),
                    name.as_ptr(),
                )
            };
            if result < 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    if let Err(e) = published {
        // SAFETY: live directory fd and nul-terminated name.
        unsafe { libc::unlinkat(parent.as_raw_fd(), staging.as_ptr(), 0) };
        return Err(e);
    }
    // The transcript is in place now. Some filesystems can't sync a directory.
    // SAFETY: live directory descriptor.
    if unsafe { libc::fsync(parent.as_raw_fd()) } < 0 {
        let e = io::Error::last_os_error();
        if e.raw_os_error() != Some(libc::EINVAL) {
            return Err(e);
        }
    }
    Ok(())
}
#[cfg(test)]
thread_local! {
    /// Runs once on this thread, after a staging file appears and before it is locked.
    pub(crate) static BEFORE_LOCK: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}
const PARTIAL: &str = ".ophio-partial";
/// Removes staging files that a killed copy left for the same name.
/// One whose lock is still held belongs to a copy in progress.
fn sweep(parent: &OwnedFd, dir: &Path, prefix: &[u8]) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if !name.as_bytes().starts_with(prefix) || !name.as_bytes().ends_with(PARTIAL.as_bytes()) {
            continue;
        }
        let Ok(name) = CString::new(name.as_bytes()) else {
            continue;
        };
        // SAFETY: live directory fd, checked name and valid flags; O_NONBLOCK
        // keeps a planted FIFO from blocking.
        let Ok(fd) = owned(unsafe {
            libc::openat(
                parent.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
            )
        }) else {
            continue;
        };
        // Unlink only a file still linked, and while holding its lock, so a
        // writer that locks it afterwards finds it gone and stages another.
        let file = File::from(fd);
        if file.try_lock().is_ok()
            && file
                .metadata()
                .is_ok_and(|m| std::os::unix::fs::MetadataExt::nlink(&m) > 0)
        {
            // SAFETY: live directory fd and nul-terminated name.
            unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), 0) };
        }
    }
}

/// What is at a name in a folder, without following a link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Link,
    Folder,
    /// A plain file, with its length.
    File(u64),
    Other,
}
/// A single plain name: not empty, `.` or `..`, and without a slash, so an
/// operation through a folder's handle stays in that folder.
fn entry(name: &std::ffi::OsStr) -> io::Result<CString> {
    let bytes = name.as_bytes();
    if bytes.is_empty() || bytes == b"." || bytes == b".." || bytes.contains(&b'/') {
        return Err(io::Error::other("not a single plain name"));
    }
    CString::new(bytes).map_err(io::Error::other)
}
fn done(result: i32) -> io::Result<()> {
    if result < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}
/// What `name` in the folder held by `parent` is, if anything.
pub fn kind_at(parent: &File, name: &std::ffi::OsStr) -> io::Result<Option<Kind>> {
    let name = entry(name)?;
    // SAFETY: zeroed stat is a valid out-buffer for fstatat.
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: live directory fd, nul-terminated name and a valid buffer.
    let result = unsafe {
        libc::fstatat(
            parent.as_raw_fd(),
            name.as_ptr(),
            &mut stat,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if let Err(e) = done(result) {
        return match e.kind() {
            io::ErrorKind::NotFound => Ok(None),
            _ => Err(e),
        };
    }
    let format = stat.st_mode & libc::S_IFMT;
    Ok(Some(if format == libc::S_IFLNK {
        Kind::Link
    } else if format == libc::S_IFDIR {
        Kind::Folder
    } else if format == libc::S_IFREG {
        Kind::File(u64::try_from(stat.st_size).unwrap_or(u64::MAX))
    } else {
        Kind::Other
    }))
}
/// Removes `name` from the folder held by `parent`: with `folder`, only an
/// empty folder. Never anything inside it.
pub fn remove_at(parent: &File, name: &std::ffi::OsStr, folder: bool) -> io::Result<()> {
    let name = entry(name)?;
    let flags = if folder { libc::AT_REMOVEDIR } else { 0 };
    // SAFETY: live directory fd and nul-terminated name.
    done(unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), flags) })
}
/// Opens the folder `name` in the folder held by `parent`, refusing a link;
/// with `create`, makes it first if it isn't there.
pub fn folder_at(parent: &File, name: &std::ffi::OsStr, create: bool) -> io::Result<File> {
    let name = entry(name)?;
    if create {
        // SAFETY: live directory fd and nul-terminated name.
        let made = done(unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o700) });
        if let Err(e) = made
            && e.kind() != io::ErrorKind::AlreadyExists
        {
            return Err(e);
        }
    }
    // SAFETY: live directory fd, nul-terminated name and valid flags.
    owned(unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_RDONLY,
        )
    })
    .map(File::from)
}
/// Makes a new empty file `name` in the folder held by `parent`.
pub fn new_file_at(parent: &File, name: &std::ffi::OsStr) -> io::Result<()> {
    let name = entry(name)?;
    // SAFETY: live directory fd, nul-terminated name, valid flags and mode.
    owned(unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600,
        )
    })
    .map(drop)
}
/// Makes `name` in the folder held by `parent` a link to `target`.
pub fn link_at(parent: &File, name: &std::ffi::OsStr, target: &Path) -> io::Result<()> {
    let name = entry(name)?;
    let target = CString::new(target.as_os_str().as_bytes()).map_err(io::Error::other)?;
    // SAFETY: nul-terminated target and name, live directory fd.
    done(unsafe { libc::symlinkat(target.as_ptr(), parent.as_raw_fd(), name.as_ptr()) })
}
/// Where the link `name` in the folder held by `parent` leads.
pub fn read_link_at(parent: &File, name: &std::ffi::OsStr) -> io::Result<std::path::PathBuf> {
    let name = entry(name)?;
    let mut buffer = vec![0u8; 4096];
    // SAFETY: live directory fd, nul-terminated name, buffer of the given size.
    let length = unsafe {
        libc::readlinkat(
            parent.as_raw_fd(),
            name.as_ptr(),
            buffer.as_mut_ptr().cast(),
            buffer.len(),
        )
    };
    let length = usize::try_from(length).map_err(|_| io::Error::last_os_error())?;
    if length >= buffer.len() {
        return Err(io::Error::other("link too long"));
    }
    buffer.truncate(length);
    Ok(std::ffi::OsString::from_vec(buffer).into())
}
/// Renames `from` to `to`, both in the folder held by `parent`.
pub fn rename_at(parent: &File, from: &std::ffi::OsStr, to: &std::ffi::OsStr) -> io::Result<()> {
    let (from, to) = (entry(from)?, entry(to)?);
    let fd = parent.as_raw_fd();
    // SAFETY: live directory fd and nul-terminated names.
    done(unsafe { libc::renameat(fd, from.as_ptr(), fd, to.as_ptr()) })
}
