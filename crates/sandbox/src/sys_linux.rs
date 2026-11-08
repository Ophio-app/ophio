// SPDX-License-Identifier: Apache-2.0
//! The Linux side of the system-call boundary: namespaces and the helpers that
//! enter them.
use super::*;
use std::{
    ffi::OsString,
    net::{SocketAddr, TcpStream},
    os::unix::{net::UnixStream, process::CommandExt},
    process::Command,
    time::Duration,
};

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
