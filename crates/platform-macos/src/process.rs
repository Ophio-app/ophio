// SPDX-License-Identifier: Apache-2.0
//! What the system says about running programs. Identities and counters only:
//! never command lines, environments or memory.
#![allow(unsafe_code)]
// libc keeps these Mach declarations but points people at another crate for them.
#![allow(deprecated)]

use std::{
    ffi::CStr,
    io,
    mem::{size_of, zeroed},
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    path::PathBuf,
};

/// `SZOMB` in <sys/proc.h>.
const ZOMBIE: u32 = 5;
/// `PROC_PIDVNODEPATHINFO` in <sys/proc_info.h>, and the layout of what it
/// fills: the working folder's `vnode_info`, then its path.
const VNODE_PATH_INFO: libc::c_int = 9;
const VNODE_PATH_INFO_SIZE: usize = 2352;
const VNODE_INFO_SIZE: usize = 152;
const PATH_SIZE: usize = 1024;
/// `PROC_PIDFDSOCKETINFO` in <sys/proc_info.h>.
const FD_SOCKET_INFO: libc::c_int = 3;
/// The parts of `struct socket_fdinfo` that are read, by their place in it,
/// which the system's other programs depend on too. The tests here check
/// them against a real listening socket.
const SOCKET_INFO_SIZE: usize = 792;
const SOCKET_ID: usize = 160;
const SOCKET_KIND: usize = 256;
const SOCKET_IN: usize = 264;
const IN_LOCAL_PORT: usize = 4;
const IN_FAMILY: usize = 24;
const IN_LOCAL_ADDRESS: usize = 48;
const TCP_STATE: usize = 80;
const KIND_TCP: i32 = 2;
const TCP_LISTEN: i32 = 1;
const FAMILY_V4: u8 = 1;
const FAMILY_V6: u8 = 2;

/// One running program.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Process {
    pub pid: u32,
    pub parent: u32,
    pub group: u32,
    pub uid: u32,
    pub start_seconds: u64,
    pub start_micros: u64,
    pub zombie: bool,
    /// The short name it goes by.
    pub name: String,
}

/// A socket listening for connections.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ListeningSocket {
    pub address: IpAddr,
    pub port: u16,
    /// Stands for the socket itself while it is open.
    pub id: u64,
}

fn c_size<T>() -> libc::c_int {
    libc::c_int::try_from(size_of::<T>()).unwrap_or(libc::c_int::MAX)
}

/// Every program's id. The list can grow while it is read, so a list that fills
/// the room given is read again with more.
pub fn all() -> io::Result<Vec<i32>> {
    let mut pids = vec![0 as libc::c_int; 2048];
    loop {
        let room = libc::c_int::try_from(pids.len() * size_of::<libc::c_int>())
            .map_err(io::Error::other)?;
        // SAFETY: the pointer and size describe `pids`, which outlives the call.
        let count = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), room) };
        if count < 0 {
            return Err(io::Error::last_os_error());
        }
        let count = usize::try_from(count).map_err(io::Error::other)?;
        if count < pids.len() {
            pids.truncate(count);
            pids.retain(|&pid| pid > 0);
            return Ok(pids);
        }
        if pids.len() >= 1 << 20 {
            return Err(io::Error::other("Too many processes to read"));
        }
        let doubled = pids.len() * 2;
        pids.resize(doubled, 0);
    }
}

fn text(bytes: &[libc::c_char]) -> String {
    let bytes: Vec<u8> = bytes
        .iter()
        .take_while(|&&byte| byte != 0)
        .map(|&byte| byte.cast_unsigned())
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

/// A program's identity, while it can be read: gone, or another user's that
/// this one may not look at, reads as nothing.
pub fn read(pid: i32) -> Option<Process> {
    // SAFETY: an all-zero proc_bsdinfo is valid: it holds only integers and
    // byte arrays.
    let mut info: libc::proc_bsdinfo = unsafe { zeroed() };
    // SAFETY: the pointer and size describe `info`, which outlives the call.
    let read = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            std::ptr::from_mut(&mut info).cast(),
            c_size::<libc::proc_bsdinfo>(),
        )
    };
    if read != c_size::<libc::proc_bsdinfo>() {
        return None;
    }
    let long = text(&info.pbi_name);
    Some(Process {
        pid: pid.cast_unsigned(),
        parent: info.pbi_ppid,
        group: info.pbi_pgid,
        uid: info.pbi_uid,
        start_seconds: info.pbi_start_tvsec,
        start_micros: info.pbi_start_tvusec,
        zombie: info.pbi_status == ZOMBIE,
        name: if long.is_empty() {
            text(&info.pbi_comm)
        } else {
            long
        },
    })
}

/// A program's parent, which can be read for any program, as the rest of what
/// [`read`] says can't be for another user's.
pub fn parent(pid: i32) -> Option<u32> {
    // SAFETY: an all-zero proc_bsdshortinfo is valid: it holds only integers and
    // byte arrays.
    let mut info: libc::proc_bsdshortinfo = unsafe { zeroed() };
    // SAFETY: the pointer and size describe `info`, which outlives the call.
    let read = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDT_SHORTBSDINFO,
            0,
            std::ptr::from_mut(&mut info).cast(),
            c_size::<libc::proc_bsdshortinfo>(),
        )
    };
    (read == c_size::<libc::proc_bsdshortinfo>()).then_some(info.pbsi_ppid)
}

/// The file a program runs from.
pub fn executable(pid: i32) -> Option<PathBuf> {
    let mut buffer = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    let room = u32::try_from(buffer.len()).ok()?;
    // SAFETY: the pointer and size describe `buffer`, which outlives the call.
    let written = unsafe { libc::proc_pidpath(pid, buffer.as_mut_ptr().cast(), room) };
    if written <= 0 {
        return None;
    }
    buffer.truncate(usize::try_from(written).ok()?);
    Some(PathBuf::from(String::from_utf8(buffer).ok()?))
}

/// The folder a program works in.
pub fn working_folder(pid: i32) -> Option<PathBuf> {
    let mut buffer = [0u8; VNODE_PATH_INFO_SIZE];
    let room = i32::try_from(buffer.len()).ok()?;
    // SAFETY: the pointer and size describe `buffer`, which outlives the call.
    let read =
        unsafe { libc::proc_pidinfo(pid, VNODE_PATH_INFO, 0, buffer.as_mut_ptr().cast(), room) };
    if read != room {
        return None;
    }
    let path = buffer.get(VNODE_INFO_SIZE..VNODE_INFO_SIZE + PATH_SIZE)?;
    let path = CStr::from_bytes_until_nul(path).ok()?.to_str().ok()?;
    (!path.is_empty()).then(|| PathBuf::from(path))
}

/// Nanoseconds for each of the system's time units: not one on Apple silicon.
pub(crate) fn nanoseconds_per_unit() -> (u64, u64) {
    let mut base = libc::mach_timebase_info { numer: 1, denom: 1 };
    // SAFETY: the pointer describes `base`, which outlives the call.
    unsafe { libc::mach_timebase_info(&raw mut base) };
    if base.denom == 0 {
        return (1, 1);
    }
    (u64::from(base.numer), u64::from(base.denom))
}

/// Processor nanoseconds since a program started, and its resident bytes.
pub(crate) fn usage(pid: i32, (numer, denom): (u64, u64)) -> Option<(u64, u64)> {
    // SAFETY: an all-zero proc_taskinfo is valid: it holds only integers.
    let mut info: libc::proc_taskinfo = unsafe { zeroed() };
    // SAFETY: the pointer and size describe `info`, which outlives the call.
    let read = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTASKINFO,
            0,
            std::ptr::from_mut(&mut info).cast(),
            c_size::<libc::proc_taskinfo>(),
        )
    };
    if read != c_size::<libc::proc_taskinfo>() {
        return None;
    }
    let units = u128::from(info.pti_total_user) + u128::from(info.pti_total_system);
    let nanos = units * u128::from(numer) / u128::from(denom.max(1));
    Some((u64::try_from(nanos).ok()?, info.pti_resident_size))
}

fn i32_at(bytes: &[u8], at: usize) -> Option<i32> {
    Some(i32::from_ne_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

fn listening_from(info: &[u8]) -> Option<ListeningSocket> {
    if i32_at(info, SOCKET_KIND)? != KIND_TCP || i32_at(info, SOCKET_IN + TCP_STATE)? != TCP_LISTEN
    {
        return None;
    }
    let inner = info.get(SOCKET_IN..)?;
    let address: [u8; 16] = inner
        .get(IN_LOCAL_ADDRESS..IN_LOCAL_ADDRESS + 16)?
        .try_into()
        .ok()?;
    let address = match *inner.get(IN_FAMILY)? {
        // The last four bytes of the sixteen, as in an address mapped to IPv6.
        family if family & FAMILY_V6 != 0 => IpAddr::V6(Ipv6Addr::from(address)),
        family if family & FAMILY_V4 != 0 => IpAddr::V4(Ipv4Addr::new(
            address[12],
            address[13],
            address[14],
            address[15],
        )),
        _ => return None,
    };
    // Kept in network order, in the low half of an int.
    let port = u16::from_be(u16::try_from(i32_at(inner, IN_LOCAL_PORT)? & 0xffff).ok()?);
    let id = u64::from_ne_bytes(info.get(SOCKET_ID..SOCKET_ID + 8)?.try_into().ok()?);
    Some(ListeningSocket { address, port, id })
}

/// The TCP sockets a program listens on. A program that goes meanwhile, or
/// that this user may not look at, has none.
pub fn listening(pid: i32) -> Vec<ListeningSocket> {
    let mut files = vec![libc::proc_fdinfo {
        proc_fd: 0,
        proc_fdtype: 0,
    }];
    let count = loop {
        let Ok(room) = libc::c_int::try_from(files.len() * size_of::<libc::proc_fdinfo>()) else {
            return Vec::new();
        };
        // SAFETY: the pointer and size describe `files`, which outlives the call.
        let bytes = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDLISTFDS,
                0,
                files.as_mut_ptr().cast(),
                room,
            )
        };
        if bytes <= 0 {
            return Vec::new();
        }
        if bytes < room || files.len() >= 1 << 20 {
            break usize::try_from(bytes).unwrap_or(0) / size_of::<libc::proc_fdinfo>();
        }
        let doubled = files.len() * 2;
        files.resize(
            doubled.max(64),
            libc::proc_fdinfo {
                proc_fd: 0,
                proc_fdtype: 0,
            },
        );
    };
    let mut sockets = Vec::new();
    for file in files.iter().take(count) {
        if file.proc_fdtype.cast_signed() != libc::PROX_FDTYPE_SOCKET {
            continue;
        }
        let mut info = [0u8; SOCKET_INFO_SIZE];
        let Ok(room) = libc::c_int::try_from(info.len()) else {
            continue;
        };
        // SAFETY: the pointer and size describe `info`, which outlives the call.
        let read = unsafe {
            libc::proc_pidfdinfo(
                pid,
                file.proc_fd,
                FD_SOCKET_INFO,
                info.as_mut_ptr().cast(),
                room,
            )
        };
        if read == room
            && let Some(socket) = listening_from(&info)
        {
            sockets.push(socket);
        }
    }
    sockets
}

/// A value the system keeps by name, as text.
pub(crate) fn sysctl_text(name: &CStr) -> Option<String> {
    let mut size = 0usize;
    // SAFETY: asks only for the size, into `size`, which outlives the call.
    let asked = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            std::ptr::null_mut(),
            &raw mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if asked != 0 || size == 0 {
        return None;
    }
    let mut buffer = vec![0u8; size];
    // SAFETY: the pointer and size describe `buffer`, which outlives the call.
    let read = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            buffer.as_mut_ptr().cast(),
            &raw mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if read != 0 {
        return None;
    }
    buffer.truncate(size);
    let text = CStr::from_bytes_until_nul(&buffer).ok()?.to_str().ok()?;
    Some(text.to_string()).filter(|text| !text.is_empty())
}

/// A value the system keeps by name, into a plain structure or number.
pub(crate) fn sysctl_value<T: Copy>(name: &CStr, mut value: T) -> Option<T> {
    let mut size = size_of::<T>();
    // SAFETY: the pointer and size describe `value`, which outlives the call,
    // and callers use only types valid for any bytes the system writes.
    let read = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            std::ptr::from_mut(&mut value).cast(),
            &raw mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    (read == 0 && size == size_of::<T>()).then_some(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn me() -> i32 {
        i32::try_from(std::process::id()).unwrap()
    }

    #[test]
    fn this_process_reads_back_as_it_is() {
        assert!(all().unwrap().contains(&me()));
        let process = read(me()).unwrap();
        // SAFETY: getuid takes nothing and can't fail.
        assert_eq!(process.uid, unsafe { libc::getuid() });
        assert!(!process.zombie);
        assert!(process.start_seconds > 1_600_000_000);
        assert_eq!(
            executable(me()).unwrap().canonicalize().unwrap(),
            std::env::current_exe().unwrap().canonicalize().unwrap()
        );
        assert_eq!(
            working_folder(me()).unwrap().canonicalize().unwrap(),
            std::env::current_dir().unwrap().canonicalize().unwrap()
        );
        let (cpu, memory) = usage(me(), nanoseconds_per_unit()).unwrap();
        assert!(cpu > 0 && memory > 0);
    }

    #[test]
    fn a_process_that_has_gone_reads_as_nothing() {
        let mut child = std::process::Command::new("/usr/bin/true").spawn().unwrap();
        let pid = i32::try_from(child.id()).unwrap();
        child.wait().unwrap();
        assert!(read(pid).is_none());
        assert!(listening(pid).is_empty());
    }

    #[test]
    fn a_zombie_reads_as_one() {
        let mut child = std::process::Command::new("/usr/bin/true").spawn().unwrap();
        let pid = i32::try_from(child.id()).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        // Either the system says it is a zombie or it says nothing about it.
        while read(pid).is_some_and(|process| !process.zombie) {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        child.wait().unwrap();
    }

    #[test]
    fn listening_sockets_are_found_with_their_addresses() {
        let v4 = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let any = std::net::TcpListener::bind("0.0.0.0:0").unwrap();
        let v6 = std::net::TcpListener::bind("[::1]:0").unwrap();
        let sockets = listening(me());
        let found = |listener: &std::net::TcpListener| {
            let local = listener.local_addr().unwrap();
            sockets
                .iter()
                .find(|socket| socket.port == local.port())
                .map(|socket| socket.address)
        };
        assert_eq!(found(&v4), Some(IpAddr::V4(Ipv4Addr::LOCALHOST)));
        assert_eq!(found(&any), Some(IpAddr::V4(Ipv4Addr::UNSPECIFIED)));
        assert_eq!(found(&v6), Some(IpAddr::V6(Ipv6Addr::LOCALHOST)));
        assert!(sockets.iter().all(|socket| socket.id != 0));
    }

    #[test]
    fn a_socket_that_only_connected_is_not_listening() {
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let client = std::net::TcpStream::connect(server.local_addr().unwrap()).unwrap();
        let own_port = client.local_addr().unwrap().port();
        assert!(listening(me()).iter().all(|socket| socket.port != own_port));
    }

    #[test]
    fn values_kept_by_name_read() {
        assert!(sysctl_text(c"kern.ostype").is_some_and(|name| name == "Darwin"));
        assert!(sysctl_value(c"hw.memsize", 0u64).is_some_and(|bytes| bytes > 0));
        assert!(sysctl_text(c"no.such.value").is_none());
    }
}
