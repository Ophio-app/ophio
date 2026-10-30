// SPDX-License-Identifier: Apache-2.0
//! Small Linux syscall boundary. pre_exec closures only use async-signal-safe syscalls.
use std::{
    ffi::{CString, OsString},
    fs::File,
    io,
    net::{SocketAddr, TcpStream},
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::{
            ffi::OsStrExt,
            net::{UnixListener, UnixStream},
            process::CommandExt,
        },
    },
    path::{Component, Path},
    process::Command,
    time::Duration,
};

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
    let entries = std::fs::read_dir(format!("/proc/self/fd/{}", directory.as_raw_fd()))?;
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

pub fn inherit(command: &mut Command, descriptors: Vec<i32>) {
    // SAFETY: only fcntl runs after fork; captured descriptors remain live through spawn.
    unsafe {
        command.pre_exec(move || {
            for fd in &descriptors {
                if libc::fcntl(*fd, libc::F_SETFD, 0) < 0 {
                    return Err(io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
}
#[derive(Debug)]
pub struct Namespaces {
    user: File,
    mount: File,
    root: File,
    /// The sandbox's network, and the user namespace that owns it.
    net: File,
    net_user: File,
}
/// NS_GET_USERNS: a new fd for the user namespace owning a namespace.
const NS_GET_USERNS: libc::c_ulong = 0xb701;
impl Namespaces {
    pub fn root(&self) -> &File {
        &self.root
    }
    pub fn open(pid: u32) -> io::Result<Self> {
        let mount = File::open(format!("/proc/{pid}/ns/mnt"))?;
        // SAFETY: NS_GET_USERNS returns a new fd for this mount namespace's owner.
        // Bubblewrap's final user namespace can be a child of that owner.
        let user = File::from(owned(unsafe {
            libc::ioctl(mount.as_raw_fd(), NS_GET_USERNS)
        })?);
        let net = File::open(format!("/proc/{pid}/ns/net"))?;
        // SAFETY: as above, for the network namespace's owner.
        let net_user = File::from(owned(unsafe {
            libc::ioctl(net.as_raw_fd(), NS_GET_USERNS)
        })?);
        Ok(Self {
            user,
            mount,
            root: File::open(format!("/proc/{pid}/root"))?,
            net,
            net_user,
        })
    }
    pub fn try_clone(&self) -> io::Result<Self> {
        Ok(Self {
            user: self.user.try_clone()?,
            mount: self.mount.try_clone()?,
            root: self.root.try_clone()?,
            net: self.net.try_clone()?,
            net_user: self.net_user.try_clone()?,
        })
    }
    /// A TCP connection made inside the sandbox's network to the first of
    /// `addresses` that answers. A helper joins the network, as only a
    /// single-threaded process can, connects and hands the socket back; the
    /// socket keeps the sandbox's network once it is here.
    pub fn connect(&self, shim: &Path, addresses: &[SocketAddr]) -> io::Result<TcpStream> {
        let (ours, theirs) = UnixStream::pair()?;
        let user = self.net_user.try_clone()?;
        let net = self.net.try_clone()?;
        let mut command = Command::new(shim);
        command.arg("connect");
        command.args(addresses.iter().map(SocketAddr::to_string));
        command
            .stdin(OwnedFd::from(theirs))
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        // SAFETY: only setns syscalls after fork. Files own the namespace fds until exec.
        unsafe {
            command.pre_exec(move || {
                if libc::setns(user.as_raw_fd(), libc::CLONE_NEWUSER) < 0
                    || libc::setns(net.as_raw_fd(), libc::CLONE_NEWNET) < 0
                {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let status = command.status()?;
        if status.code() == Some(CONNECT_REFUSED) {
            return Err(io::Error::from(io::ErrorKind::ConnectionRefused));
        }
        if !status.success() {
            return Err(io::Error::other("namespace connect helper failed"));
        }
        receive_socket(&ours)
    }
    pub fn change(&self, shim: &Path, operation: &str, path: &Path) -> io::Result<()> {
        let user = self.user.try_clone()?;
        let mount = self.mount.try_clone()?;
        let root = self.root.try_clone()?;
        let mut command = Command::new(shim);
        command.args([
            std::ffi::OsStr::new("mount"),
            std::ffi::OsStr::new(operation),
            path.as_os_str(),
        ]);
        // SAFETY: only setns syscalls after fork. Files own the namespace fds until exec.
        unsafe {
            command.pre_exec(move || {
                if libc::setns(user.as_raw_fd(), libc::CLONE_NEWUSER) < 0 {
                    return Err(io::Error::last_os_error());
                }
                if libc::setns(mount.as_raw_fd(), libc::CLONE_NEWNS) < 0 {
                    return Err(io::Error::last_os_error());
                }
                if libc::fchdir(root.as_raw_fd()) < 0
                    || libc::chroot(c".".as_ptr()) < 0
                    || libc::chdir(c"/".as_ptr()) < 0
                {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let status = command.status()?;
        match status.code() {
            Some(0) => Ok(()),
            Some(16) => Err(io::Error::from_raw_os_error(libc::EBUSY)),
            _ => Err(io::Error::other("namespace mount helper failed")),
        }
    }
}

/// The connect helper's exit status when nothing answered.
const CONNECT_REFUSED: i32 = 3;

/// In the sandbox's network: connects to the first of `args`, loopback
/// addresses, that answers, and sends the socket back over stdin.
pub fn connect_helper(args: Vec<OsString>) -> io::Result<i32> {
    let addresses = (args.iter())
        .map(|a| a.to_str().and_then(|a| a.parse::<SocketAddr>().ok()))
        .collect::<Option<Vec<_>>>()
        .filter(|a| !a.is_empty() && a.iter().all(|a| a.ip().is_loopback()))
        .ok_or_else(|| io::Error::other("invalid connect request"))?;
    let connected = addresses
        .iter()
        .find_map(|a| TcpStream::connect_timeout(a, Duration::from_millis(2400)).ok());
    let Some(stream) = connected else {
        return Ok(CONNECT_REFUSED);
    };
    // SAFETY: stdin is the socket the companion handed this helper.
    let back = unsafe { std::os::unix::net::UnixStream::from_raw_fd(0) };
    send_socket(&back, &stream)?;
    Ok(0)
}

/// Sends `stream`'s descriptor over `to` with one byte beside it.
fn send_socket(to: &UnixStream, stream: &TcpStream) -> io::Result<()> {
    let fd = stream.as_raw_fd();
    let mut byte = [0u8; 1];
    let mut iov = libc::iovec {
        iov_base: byte.as_mut_ptr().cast(),
        iov_len: 1,
    };
    // SAFETY: CMSG_SPACE only computes a size.
    let space = unsafe { libc::CMSG_SPACE(std::mem::size_of::<i32>() as u32) } as usize;
    let mut control = vec![0u8; space];
    // SAFETY: a zeroed msghdr is valid; every pointer set below outlives sendmsg.
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = &mut iov;
    message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast();
    message.msg_controllen = space as _;
    // SAFETY: the control buffer holds one header with room for one descriptor.
    unsafe {
        let header = libc::CMSG_FIRSTHDR(&message);
        (*header).cmsg_level = libc::SOL_SOCKET;
        (*header).cmsg_type = libc::SCM_RIGHTS;
        (*header).cmsg_len = libc::CMSG_LEN(std::mem::size_of::<i32>() as u32) as _;
        std::ptr::write_unaligned(libc::CMSG_DATA(header).cast::<i32>(), fd);
    }
    // SAFETY: a valid socket and message, as built above.
    if unsafe { libc::sendmsg(to.as_raw_fd(), &message, 0) } != 1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// The descriptor sent with [`send_socket`], as a stream.
fn receive_socket(from: &UnixStream) -> io::Result<TcpStream> {
    let mut byte = [0u8; 1];
    let mut iov = libc::iovec {
        iov_base: byte.as_mut_ptr().cast(),
        iov_len: 1,
    };
    // SAFETY: CMSG_SPACE only computes a size.
    let space = unsafe { libc::CMSG_SPACE(std::mem::size_of::<i32>() as u32) } as usize;
    let mut control = vec![0u8; space];
    // SAFETY: a zeroed msghdr is valid; every pointer set below outlives recvmsg.
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = &mut iov;
    message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast();
    message.msg_controllen = space as _;
    from.set_read_timeout(Some(Duration::from_secs(5)))?;
    // SAFETY: a valid socket and message, as built above. The helper has
    // exited, so what it sent is already queued.
    if unsafe { libc::recvmsg(from.as_raw_fd(), &mut message, libc::MSG_CMSG_CLOEXEC) } != 1 {
        return Err(io::Error::other("the connect helper sent nothing back"));
    }
    // SAFETY: the kernel filled the control buffer; each header is checked
    // before its descriptor is taken.
    unsafe {
        let header = libc::CMSG_FIRSTHDR(&message);
        if header.is_null()
            || (*header).cmsg_level != libc::SOL_SOCKET
            || (*header).cmsg_type != libc::SCM_RIGHTS
            || message.msg_flags & libc::MSG_CTRUNC != 0
        {
            return Err(io::Error::other("the connect helper sent no socket"));
        }
        let fd = std::ptr::read_unaligned(libc::CMSG_DATA(header).cast::<i32>());
        Ok(TcpStream::from(owned(fd)?))
    }
}

pub fn mount_helper(args: Vec<OsString>) -> io::Result<i32> {
    if args.len() != 2 {
        return Err(io::Error::other("invalid mount request"));
    }
    let path = cpath(Path::new(&args[1]))?;
    let result = if args[0] == "uncover" {
        // SAFETY: a validated nul-terminated path. No lazy unmount: busy covers must fail.
        unsafe { libc::umount2(path.as_ptr(), 0) }
    } else if args[0] == "write" || args[0] == "read" {
        // mount_setattr changes only RDONLY, preserving nosuid/nodev/noexec and locked flags.
        #[repr(C)]
        struct MountAttr {
            set: u64,
            clear: u64,
            propagation: u64,
            userns: u64,
        }
        let attr = MountAttr {
            set: u64::from(args[0] == "read"),
            clear: u64::from(args[0] == "write"),
            propagation: 0,
            userns: 0,
        };
        // SAFETY: correct Linux mount_attr layout, live CString and struct, no pointer escape.
        unsafe {
            libc::syscall(
                libc::SYS_mount_setattr,
                libc::AT_FDCWD,
                path.as_ptr(),
                libc::AT_RECURSIVE,
                &attr,
                std::mem::size_of::<MountAttr>(),
            ) as i32
        }
    } else {
        return Err(io::Error::other("unknown mount operation"));
    };
    if result == 0 {
        Ok(0)
    } else if io::Error::last_os_error().raw_os_error() == Some(libc::EBUSY) {
        Ok(16)
    } else {
        Err(io::Error::last_os_error())
    }
}
