// SPDX-License-Identifier: Apache-2.0
//! Read-only Windows preview discovery. Unsupported native layouts, inaccessible
//! processes and unverifiable directories are deliberately omitted.
use super::{Handle, denied, owner_sid, process_sid};
use crate::security::UserSid;
use crate::servers::{Socket, Source, Working, find};
use ports::servers::{Listening, LocalServers};
use std::{
    collections::HashMap,
    ffi::c_void,
    io,
    net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV6},
    os::windows::ffi::OsStringExt,
    path::{Path, PathBuf},
};
use windows::Win32::{
    Foundation::{DUPLICATE_SAME_ACCESS, DuplicateHandle, FILETIME, HANDLE, STILL_ACTIVE},
    NetworkManagement::IpHelper::{
        GetExtendedTcpTable, MIB_TCP6ROW_OWNER_MODULE, MIB_TCP6TABLE_OWNER_MODULE,
        MIB_TCPROW_OWNER_MODULE, MIB_TCPTABLE_OWNER_MODULE, TCP_TABLE_OWNER_MODULE_LISTENER,
    },
    Storage::FileSystem::{
        FILE_ID_INFO, FILE_NAME_NORMALIZED, FILE_NAME_OPENED, FileIdInfo,
        GETFINALPATHNAMEBYHANDLE_FLAGS, GetDriveTypeW, GetFileInformationByHandleEx,
        GetFinalPathNameByHandleW, VOLUME_NAME_NT,
    },
    System::{
        Diagnostics::Debug::ReadProcessMemory,
        Threading::{
            GetCurrentProcess, GetExitCodeProcess, GetProcessTimes, OpenProcess,
            PROCESS_DUP_HANDLE, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_VM_READ,
        },
    },
};

#[derive(Debug, Default)]
pub struct WindowsServers;
impl WindowsServers {
    pub fn new() -> Self {
        Self
    }
}

impl LocalServers for WindowsServers {
    fn listening(&self, folders: &[PathBuf]) -> Vec<Listening> {
        // Resolve our token once, never infer ownership from permission to open
        // a process: administrators can open other users' processes too.
        let Ok(owner) = owner_sid() else {
            return Vec::new();
        };
        find(&Native { owner }, std::process::id(), folders)
    }
    fn open(&self) -> HashMap<u64, u16> {
        sockets()
            .into_iter()
            .filter(Socket::local)
            .map(|s| (s.id(), s.port))
            .collect()
    }
    fn boot(&self) -> Option<String> {
        // SystemBootEnvironmentInformation: a boot-session GUID, not a rounded
        // wall-clock-minus-uptime estimate that changes on every scan.
        let mut info = [0u64; 4];
        // SAFETY: aligned, initialized output of the documented native size;
        // failure leaves this reading absent.
        let status = unsafe {
            NtQuerySystemInformation(90, info.as_mut_ptr().cast(), 32, std::ptr::null_mut())
        };
        if status < 0 || info[..2] == [0, 0] {
            return None;
        }
        Some(format!("{:016x}{:016x}", info[0], info[1]))
    }
}

struct Native {
    owner: UserSid,
}
impl Source for Native {
    fn sockets(&self) -> Vec<Socket> {
        sockets()
    }
    fn canonical(&self, path: &Path) -> Option<PathBuf> {
        path.canonicalize().ok()
    }
    fn working(&self, pid: u32, folders: &[PathBuf]) -> Option<Working> {
        // SAFETY: a non-inherited handle, closed by its owner on every path.
        let process = Handle(
            unsafe {
                OpenProcess(
                    PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ | PROCESS_DUP_HANDLE,
                    false,
                    pid,
                )
            }
            .ok()?,
        );
        if process_sid(process.0).ok()? != self.owner {
            return None;
        }
        let created = creation(process.0)?;
        let before = directory(process.0, folders)?;
        // A process can change directory while its parameters are copied. Read
        // again and compare the object identity, not only the folder's name.
        if directory(process.0, folders)? != before || creation(process.0)? != created {
            return None;
        }
        Some(Working {
            created,
            dir: before.0,
            volume: before.1,
            file: before.2,
        })
    }
}

fn creation(process: HANDLE) -> Option<u64> {
    let (mut created, mut exited, mut kernel, mut user): (FILETIME, FILETIME, FILETIME, FILETIME) =
        Default::default();
    let mut code = 0;
    // SAFETY: live process handle and initialized out parameters.
    unsafe {
        GetExitCodeProcess(process, &mut code).ok()?;
        if code != STILL_ACTIVE.0 as u32 {
            return None;
        }
        GetProcessTimes(process, &mut created, &mut exited, &mut kernel, &mut user).ok()?;
    }
    let ticks = u64::from(created.dwHighDateTime) << 32 | u64::from(created.dwLowDateTime);
    (ticks > 0).then_some(ticks)
}

/// GetExtendedTcpTable may grow between calls; retry boundedly and validate all
/// lengths before accessing its variable-length rows. No IP Helper allocation
/// or foreign process handle is retained across scans.
fn table<T: Copy>(family: u32, offset: usize) -> io::Result<Vec<T>> {
    let mut needed = 0;
    // SAFETY: null buffer requests the required byte count.
    unsafe {
        GetExtendedTcpTable(
            None,
            &mut needed,
            false,
            family,
            TCP_TABLE_OWNER_MODULE_LISTENER,
            0,
        );
    }
    for _ in 0..3 {
        if !(4..=16 * 1024 * 1024).contains(&needed) {
            return Err(io::Error::other("invalid native TCP table size"));
        }
        let allocated = needed as usize;
        let mut bytes = vec![0u64; allocated.div_ceil(8)];
        // SAFETY: 8-byte-aligned buffer at least as large as `needed`.
        let status = unsafe {
            GetExtendedTcpTable(
                Some(bytes.as_mut_ptr().cast()),
                &mut needed,
                false,
                family,
                TCP_TABLE_OWNER_MODULE_LISTENER,
                0,
            )
        };
        if status == 122 {
            continue;
        } // ERROR_INSUFFICIENT_BUFFER
        if status != 0 {
            return Err(io::Error::from_raw_os_error(status as i32));
        }
        if needed as usize > allocated {
            return Err(io::Error::other("native TCP table exceeded buffer"));
        }
        // SAFETY: successful table contains at least its entry count; byte
        // extents checked before each row, rows copied rather than borrowed.
        let count = unsafe { *bytes.as_ptr().cast::<u32>() } as usize;
        if count == 0 {
            return Ok(Vec::new());
        }
        let Some(end) = count
            .checked_mul(size_of::<T>())
            .and_then(|n| offset.checked_add(n))
        else {
            return Err(io::Error::other("native TCP table length overflow"));
        };
        if end > needed as usize {
            return Err(io::Error::other("native TCP table rows exceeded buffer"));
        }
        return Ok((0..count)
            .map(|n| unsafe {
                std::ptr::read_unaligned(
                    bytes
                        .as_ptr()
                        .cast::<u8>()
                        .add(offset + n * size_of::<T>())
                        .cast::<T>(),
                )
            })
            .collect());
    }
    Err(io::Error::other("native TCP table kept growing"))
}
fn rows() -> io::Result<Vec<(Socket, u32)>> {
    let v4 = table::<MIB_TCPROW_OWNER_MODULE>(
        2,
        std::mem::offset_of!(MIB_TCPTABLE_OWNER_MODULE, table),
    )?;
    let v6 = table::<MIB_TCP6ROW_OWNER_MODULE>(
        23,
        std::mem::offset_of!(MIB_TCP6TABLE_OWNER_MODULE, table),
    )?;
    Ok(v4
        .into_iter()
        .filter(|r| r.dwState == 2)
        .map(|r| {
            (
                Socket {
                    pid: r.dwOwningPid,
                    address: Ipv4Addr::from(r.dwLocalAddr.to_ne_bytes()).into(),
                    port: u16::from_be(r.dwLocalPort as u16),
                    created: r.liCreateTimestamp,
                },
                0,
            )
        })
        .chain(v6.into_iter().filter(|r| r.dwState == 2).map(|r| {
            (
                Socket {
                    pid: r.dwOwningPid,
                    address: Ipv6Addr::from(r.ucLocalAddr).into(),
                    port: u16::from_be(r.dwLocalPort as u16),
                    created: r.liCreateTimestamp,
                },
                r.dwLocalScopeId,
            )
        }))
        .collect())
}
fn sockets() -> Vec<Socket> {
    rows()
        .unwrap_or_default()
        .into_iter()
        .filter(|(_, scope)| *scope == 0)
        .map(|(s, _)| s)
        .collect()
}

/// Read-only listener health for an already verified owner-pipe process. This
/// does not discover previews or grant project ownership. The caller must hold
/// its own verified process handle and check that it did not exit around this
/// observation, so a PID obtained earlier cannot silently name another process.
/// Native table failures are errors, never an apparently healthy empty result.
pub fn listening(pid: u32) -> io::Result<Vec<SocketAddr>> {
    let owner = owner_sid()?;
    // SAFETY: process handle is owned, non-inherited and closed on every path.
    let process = Handle(
        unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }
            .map_err(super::failure)?,
    );
    if process_sid(process.0)? != owner {
        return Err(denied());
    }
    let created = creation(process.0).ok_or_else(denied)?;
    let before = rows()?;
    let after = rows()?;
    if creation(process.0) != Some(created) {
        return Err(denied());
    }
    let mut addresses = Vec::new();
    for (socket, scope) in before.into_iter().filter(|(s, scope)| {
        s.pid == pid
            && s.created > 0
            && s.created as u64 >= created
            && after.contains(&(s.clone(), *scope))
    }) {
        let address = match socket.address {
            std::net::IpAddr::V4(ip) => SocketAddr::from((ip, socket.port)),
            std::net::IpAddr::V6(ip) => {
                SocketAddr::V6(SocketAddrV6::new(ip, socket.port, 0, scope))
            }
        };
        if !addresses.contains(&address) {
            addresses.push(address);
        }
    }
    addresses.sort();
    Ok(addresses)
}

#[repr(C)]
#[derive(Default)]
struct BasicInformation {
    exit_status: usize,
    peb: usize,
    affinity: usize,
    priority: usize,
    pid: usize,
    parent: usize,
}
#[link(name = "ntdll")]
unsafe extern "system" {
    fn NtQueryInformationProcess(
        process: HANDLE,
        class: u32,
        info: *mut c_void,
        length: u32,
        returned: *mut u32,
    ) -> i32;
    fn NtQuerySystemInformation(
        class: u32,
        info: *mut c_void,
        length: u32,
        returned: *mut u32,
    ) -> i32;
}

fn read<const N: usize>(process: HANDLE, address: usize) -> Option<[u8; N]> {
    if address == 0 {
        return None;
    }
    let mut bytes = [0; N];
    let mut copied = 0;
    // SAFETY: the kernel validates the remote address; destination is a live
    // fixed-size local buffer. A short/unreadable copy is never used.
    unsafe {
        ReadProcessMemory(
            process,
            address as *const c_void,
            bytes.as_mut_ptr().cast(),
            N,
            Some(&mut copied),
        )
    }
    .ok()?;
    (copied == N).then_some(bytes)
}
fn pointer(process: HANDLE, address: usize, bits: u32) -> Option<usize> {
    if bits == 32 {
        Some(u32::from_le_bytes(read(process, address)?) as usize)
    } else {
        usize::try_from(u64::from_le_bytes(read(process, address)?)).ok()
    }
}

/// Windows stores the current directory in RTL_USER_PROCESS_PARAMETERS, not a
/// kernel process query. Read ONLY that field, never the adjacent environment
/// or command line. Check its held directory against its canonical named path.
/// 64-bit companions can inspect native and WOW64 processes; a 32-bit companion
/// fails closed for a 64-bit target instead of truncating addresses.
fn directory(process: HANDLE, folders: &[PathBuf]) -> Option<(PathBuf, u64, [u8; 16])> {
    let mut basic = BasicInformation::default();
    // SAFETY: correctly sized native PROCESS_BASIC_INFORMATION output.
    if unsafe {
        NtQueryInformationProcess(
            process,
            0,
            (&raw mut basic).cast(),
            size_of::<BasicInformation>() as u32,
            std::ptr::null_mut(),
        )
    } < 0
    {
        return None;
    }
    let mut wow64 = 0usize;
    // ProcessWow64Information returns the 32-bit PEB address when present.
    if unsafe {
        NtQueryInformationProcess(
            process,
            26,
            (&raw mut wow64).cast(),
            size_of::<usize>() as u32,
            std::ptr::null_mut(),
        )
    } < 0
    {
        return None;
    }
    #[cfg(target_pointer_width = "32")]
    {
        // Native 32-bit structure queries cannot represent a 64-bit PEB.
        use windows::Win32::System::Threading::IsWow64Process;
        let mut ours = windows::core::BOOL::default();
        let mut theirs = windows::core::BOOL::default();
        unsafe {
            IsWow64Process(GetCurrentProcess(), &mut ours).ok()?;
            IsWow64Process(process, &mut theirs).ok()?;
        }
        if ours.as_bool() && !theirs.as_bool() {
            return None;
        }
    }
    let bits = if wow64 != 0 { 32 } else { usize::BITS };
    let peb = if wow64 != 0 { wow64 } else { basic.peb };
    let parameters = pointer(
        process,
        peb.checked_add(if bits == 64 { 0x20 } else { 0x10 })?,
        bits,
    )?;
    // CurrentDirectory is at 0x38 (64-bit), 0x24 (32-bit). Its DOS_PATH is a
    // UNICODE_STRING followed by the directory handle, with native alignment.
    let curdir = parameters.checked_add(if bits == 64 { 0x38 } else { 0x24 })?;
    let header = read::<4>(process, curdir)?;
    let length = usize::from(u16::from_le_bytes([header[0], header[1]]));
    let capacity = usize::from(u16::from_le_bytes([header[2], header[3]]));
    if length == 0 || length % 2 != 0 || length > capacity || length > 65532 {
        return None;
    }
    let address = pointer(
        process,
        curdir.checked_add(if bits == 64 { 8 } else { 4 })?,
        bits,
    )?;
    let handle = pointer(
        process,
        curdir.checked_add(if bits == 64 { 16 } else { 8 })?,
        bits,
    )?;
    if address == 0 || handle & !3 == 0 {
        return None;
    }
    let mut path = vec![0u16; length / 2];
    let mut copied = 0;
    // SAFETY: bounded Unicode destination; no command line/environment is read.
    unsafe {
        ReadProcessMemory(
            process,
            address as *const c_void,
            path.as_mut_ptr().cast(),
            length,
            Some(&mut copied),
        )
    }
    .ok()?;
    if copied != length || path.contains(&0) {
        return None;
    }
    let named = PathBuf::from(std::ffi::OsString::from_wide(&path));
    // Restrict to ordinary local absolute drive paths. UNC/device paths could
    // wait on network providers and are not suitable automatic project roots.
    if path.len() < 3
        || !((path[0] as u8).is_ascii_alphabetic() && path[0] <= 127)
        || path[1] != b':' as u16
        || path[2] != b'\\' as u16
    {
        return None;
    }
    let root = [path[0], b':' as u16, b'\\' as u16, 0];
    // A drive letter can be a mapped network share. GetDriveType only examines
    // the drive root; remote/removable/unknown drives are not probed further.
    if unsafe { GetDriveTypeW(windows::core::PCWSTR(root.as_ptr())) } != 3 {
        return None;
    }
    let mut duplicate = HANDLE::default();
    // SAFETY: duplicate into this process without inheritance or source close.
    // Low two RTL current-directory flag bits are not part of the handle.
    unsafe {
        DuplicateHandle(
            process,
            HANDLE((handle & !3) as *mut c_void),
            GetCurrentProcess(),
            &mut duplicate,
            0,
            false,
            DUPLICATE_SAME_ACCESS,
        )
    }
    .ok()?;
    let duplicate = Handle(duplicate);
    // FILE_NAME_OPENED avoids SMB's normalized-component traversal. Reject a
    // local drive path that actually resolves through a reparse point to a
    // remote filesystem before asking for normalized paths or file identities.
    let device = final_path_flags(
        duplicate.0,
        GETFINALPATHNAMEBYHANDLE_FLAGS(FILE_NAME_OPENED.0 | VOLUME_NAME_NT.0),
    )?;
    if !device.to_str()?.starts_with(r"\Device\HarddiskVolume") {
        return None;
    }
    let actual = final_path(duplicate.0)?;
    // Use the held object's canonical path, not the raw DOS spelling: a
    // junction/alias or non-ASCII case difference must not hide a project.
    // Unrelated processes never cause a named-path traversal or reopen.
    if !folders.iter().any(|root| actual.starts_with(root)) {
        return None;
    }
    let held = file_id(duplicate.0)?;
    let canonical = named.canonicalize().ok()?;
    if canonical != actual {
        return None;
    }
    let opened = std::fs::File::options()
        .read(true)
        .custom_flags_for_directory()
        .open(&canonical)
        .ok()?;
    use std::os::windows::io::AsRawHandle;
    if !opened.metadata().ok()?.is_dir() || file_id(HANDLE(opened.as_raw_handle()))? != held {
        return None;
    }
    Some((canonical, held.0, held.1))
}

// Directory opens must allow deletion/renames and must not request write access.
trait DirectoryOptions {
    fn custom_flags_for_directory(&mut self) -> &mut Self;
}
impl DirectoryOptions for std::fs::OpenOptions {
    fn custom_flags_for_directory(&mut self) -> &mut Self {
        use std::os::windows::fs::OpenOptionsExt;
        self.custom_flags(0x02000000).share_mode(1 | 2 | 4)
    }
}
fn file_id(handle: HANDLE) -> Option<(u64, [u8; 16])> {
    let mut info = FILE_ID_INFO::default();
    // SAFETY: fixed-size initialized output and live directory/file handle.
    unsafe {
        GetFileInformationByHandleEx(
            handle,
            FileIdInfo,
            (&raw mut info).cast(),
            size_of::<FILE_ID_INFO>() as u32,
        )
    }
    .ok()?;
    Some((info.VolumeSerialNumber, info.FileId.Identifier))
}
fn final_path(handle: HANDLE) -> Option<PathBuf> {
    final_path_flags(handle, FILE_NAME_NORMALIZED)
}
fn final_path_flags(
    handle: HANDLE,
    flags: windows::Win32::Storage::FileSystem::GETFINALPATHNAMEBYHANDLE_FLAGS,
) -> Option<PathBuf> {
    let mut wide = vec![0; 32768];
    // SAFETY: initialized writable UTF-16 buffer, live handle.
    let length = unsafe { GetFinalPathNameByHandleW(handle, &mut wide, flags) } as usize;
    if length == 0 || length >= wide.len() {
        return None;
    }
    Some(PathBuf::from(std::ffi::OsString::from_wide(
        &wide[..length],
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn another_token_user_is_refused_even_if_the_process_handle_is_accessible() {
        let source = Native {
            owner: UserSid::parse("S-1-5-21-1-2-3-99999").unwrap(),
        };
        assert!(
            source
                .working(
                    std::process::id(),
                    &[std::env::current_dir().unwrap().canonicalize().unwrap()]
                )
                .is_none()
        );
    }

    struct Child(std::process::Child);
    impl Drop for Child {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    fn until(mut condition: impl FnMut() -> bool) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !condition() {
            assert!(
                std::time::Instant::now() < deadline,
                "native fixture timed out"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }
    fn serve(dir: &Path, state: &Path, address: &str) -> (Child, u16) {
        let child = Child(
            std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--ignored",
                    "--exact",
                    "win::servers::tests::fixture_listener",
                    "--nocapture",
                ])
                .current_dir(dir)
                .env("OPHIO_TEST_SERVER_STATE", state)
                .env("OPHIO_TEST_SERVER_ADDRESS", address)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .unwrap(),
        );
        let ready = state.join("ready");
        until(|| {
            std::fs::read_to_string(&ready)
                .ok()
                .and_then(|s| s.parse::<u16>().ok())
                .is_some()
        });
        let port = std::fs::read_to_string(ready).unwrap().parse().unwrap();
        (child, port)
    }

    /// Explicit child fixture: only an authored test executable, no shell,
    /// interpreter, user profile, installed agent or real project is involved.
    #[test]
    #[ignore = "spawned only by the native discovery lifecycle test"]
    fn fixture_listener() {
        let Some(state) = std::env::var_os("OPHIO_TEST_SERVER_STATE") else {
            return;
        };
        let state = PathBuf::from(state);
        let address = std::env::var("OPHIO_TEST_SERVER_ADDRESS").unwrap();
        let listener = std::net::TcpListener::bind(format!("{address}:0")).unwrap();
        std::fs::write(
            state.join("ready"),
            listener.local_addr().unwrap().port().to_string(),
        )
        .unwrap();
        loop {
            if let Ok(path) = std::fs::read_to_string(state.join("move")) {
                std::env::set_current_dir(path).unwrap();
                std::fs::remove_file(state.join("move")).unwrap();
                std::fs::write(state.join("moved"), "ready").unwrap();
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    #[test]
    fn native_project_discovery_rejects_unrelated_servers_and_tracks_move_exit_restart() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project");
        let inside = project.join("site");
        let outside = temp.path().join("project-other");
        std::fs::create_dir_all(&inside).unwrap();
        std::fs::create_dir(&outside).unwrap();
        let state: Vec<_> = (0..4)
            .map(|n| {
                let path = temp.path().join(format!("state{n}"));
                std::fs::create_dir(&path).unwrap();
                path
            })
            .collect();
        let (mut here, port) = serve(&inside, &state[0], "127.0.0.1");
        let (_elsewhere, other_port) = serve(&outside, &state[1], "127.0.0.1");
        let (_wildcard, wildcard_port) = serve(&project, &state[2], "0.0.0.0");
        let servers = WindowsServers::new();
        let roots = [project.clone()];
        let mut discovered = Vec::new();
        until(|| {
            discovered = servers.listening(&roots);
            discovered.iter().any(|l| l.port == port)
        });
        assert!(discovered.iter().any(|l| l.port == wildcard_port));
        assert!(!discovered.iter().any(|l| l.port == other_port));
        assert_eq!(
            discovered.iter().find(|l| l.port == port).unwrap().dir,
            ports::library::canonical(&inside).unwrap()
        );
        let old_id = discovered.iter().find(|l| l.port == port).unwrap().socket;
        assert_eq!(servers.open().get(&old_id), Some(&port));
        // The same listening process changes folder: project attribution must
        // stop immediately, though its socket remains known to be open.
        std::fs::write(state[0].join("move"), outside.to_str().unwrap()).unwrap();
        until(|| state[0].join("moved").exists());
        until(|| !servers.listening(&roots).iter().any(|l| l.port == port));
        assert_eq!(servers.open().get(&old_id), Some(&port));
        here.0.kill().unwrap();
        here.0.wait().unwrap();
        until(|| !servers.open().contains_key(&old_id));
        let (_restarted, new_port) = serve(&inside, &state[3], "127.0.0.1");
        until(|| {
            servers
                .listening(&roots)
                .iter()
                .any(|l| l.port == new_port && l.socket != old_id)
        });
    }

    /// A directory junction needs neither a shell nor symbolic-link privilege.
    fn junction(link: &Path, target: &Path) {
        use std::os::windows::{ffi::OsStrExt, fs::OpenOptionsExt, io::AsRawHandle};
        std::fs::create_dir(link).unwrap();
        let target = ports::library::canonical(target).unwrap();
        let substitute: Vec<u16> = format!(r"\??\{}", target.display())
            .encode_utf16()
            .collect();
        let print: Vec<u16> = target.as_os_str().encode_wide().collect();
        let path_bytes = (substitute.len() + print.len() + 2) * 2;
        let mut buffer = Vec::new();
        // REPARSE_DATA_BUFFER, IO_REPARSE_TAG_MOUNT_POINT, with both bounded
        // UTF-16 names and the mount-point header preceding the path buffer.
        buffer.extend_from_slice(&0xa0000003u32.to_le_bytes());
        buffer.extend_from_slice(&((8 + path_bytes) as u16).to_le_bytes());
        for value in [
            0u16,
            0,
            (substitute.len() * 2) as u16,
            ((substitute.len() + 1) * 2) as u16,
            (print.len() * 2) as u16,
        ] {
            buffer.extend_from_slice(&value.to_le_bytes());
        }
        for unit in substitute.into_iter().chain([0]).chain(print).chain([0]) {
            buffer.extend_from_slice(&unit.to_le_bytes());
        }
        let directory = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(0x02000000 | 0x00200000)
            .share_mode(1 | 2 | 4)
            .open(link)
            .unwrap();
        let mut returned = 0;
        // SAFETY: the test owns this empty directory; input is a fully bounded
        // mount-point reparse buffer, no output or asynchronous operation.
        unsafe {
            windows::Win32::System::IO::DeviceIoControl(
                HANDLE(directory.as_raw_handle()),
                0x000900a4,
                Some(buffer.as_ptr().cast()),
                buffer.len() as u32,
                None,
                0,
                Some(&mut returned),
                None,
            )
            .unwrap();
        }
    }

    #[test]
    fn native_junction_and_unicode_case_paths_find_the_registered_project() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("Pröject");
        let inside = project.join("site");
        std::fs::create_dir_all(&inside).unwrap();
        let alias = temp.path().join("alias");
        junction(&alias, &project);
        let states: Vec<_> = (0..2)
            .map(|n| {
                let state = temp.path().join(format!("state{n}"));
                std::fs::create_dir(&state).unwrap();
                state
            })
            .collect();
        let (_alias_server, alias_port) = serve(&alias.join("site"), &states[0], "127.0.0.1");
        let (_case_server, case_port) =
            serve(&temp.path().join("PRÖJECT/site"), &states[1], "127.0.0.1");
        let servers = WindowsServers::new();
        until(|| {
            let found = servers.listening(std::slice::from_ref(&project));
            [alias_port, case_port].iter().all(|port| {
                found.iter().any(|l| {
                    l.port == *port && l.dir == ports::library::canonical(&inside).unwrap()
                })
            })
        });
    }

    #[test]
    fn own_process_directory_is_proven_but_own_listener_is_not_a_preview() {
        let owner = owner_sid().unwrap();
        let source = Native { owner };
        let working = source
            .working(
                std::process::id(),
                &[std::env::current_dir().unwrap().canonicalize().unwrap()],
            )
            .expect("same-user process and directory identity");
        assert_eq!(
            working.dir,
            std::env::current_dir().unwrap().canonicalize().unwrap()
        );
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let servers = WindowsServers::new();
        assert!(
            !servers
                .listening(&[working.dir])
                .iter()
                .any(|l| l.port == port)
        );
        assert!(servers.open().values().any(|p| *p == port));
        assert!(
            listening(std::process::id())
                .unwrap()
                .iter()
                .any(|a| a.port() == port)
        );
        assert!(listening(u32::MAX).is_err());
        assert!(servers.boot().is_some());
        assert_eq!(servers.boot(), servers.boot());
        assert!(servers.listening(&[]).is_empty());
        assert!(source.working(u32::MAX, &[]).is_none());
    }
}
