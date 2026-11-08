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
        unix::ffi::OsStrExt,
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
