// SPDX-License-Identifier: Apache-2.0
use super::Handle;
use crate::system::{drive_roots, process_name};
use ports::system::{
    CpuTime, Disk, DiskAsking, DiskReading, MemoryReading, ProcessReading, SystemMonitor,
    SystemReading,
};
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use windows::{
    Win32::{
        Foundation::FILETIME,
        Storage::FileSystem::{GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDrives},
        System::{
            Diagnostics::ToolHelp::{
                CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
                TH32CS_SNAPPROCESS,
            },
            ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS},
            SystemInformation::{GetTickCount64, GlobalMemoryStatusEx, MEMORYSTATUSEX},
            Threading::{
                GetProcessTimes, GetSystemTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
            },
        },
    },
    core::PCWSTR,
};

/// `GetDriveTypeW` for a drive that stays in the computer; network and
/// removable drives are left out, as they can be slow to answer.
const DRIVE_FIXED: u32 = 3;

#[derive(Debug, Default)]
pub struct WindowsSystem {
    disks: DiskAsking,
}

impl WindowsSystem {
    pub fn new() -> Self {
        Self::default()
    }

    fn disks(&self) -> Vec<DiskReading> {
        // SAFETY: no arguments.
        let mask = unsafe { GetLogicalDrives() };
        let drives = drive_roots(mask).into_iter().map(|root| Disk {
            key: root.clone(),
            path: PathBuf::from(&root),
            mount: root,
        });
        self.disks.ask(drives, usage)
    }
}

impl SystemMonitor for WindowsSystem {
    fn read(&self) -> SystemReading {
        // What rates are made from is read first and together: a drive may
        // be slow to answer.
        let cpu = cpu().into_iter().collect();
        let processes = processes();
        SystemReading {
            cpu,
            memory: memory(),
            // SAFETY: no arguments; reads a counter.
            uptime_s: Some(unsafe { GetTickCount64() } / 1_000),
            disks: self.disks(),
            processes,
            ..SystemReading::default()
        }
    }
}

/// Hundreds of nanoseconds.
fn ticks(time: FILETIME) -> u64 {
    u64::from(time.dwHighDateTime) << 32 | u64::from(time.dwLowDateTime)
}

/// Every processor together; Windows doesn't count each one here.
fn cpu() -> Option<CpuTime> {
    let (mut idle, mut kernel, mut user) = Default::default();
    // SAFETY: three out-parameters that live through the call.
    unsafe {
        GetSystemTimes(
            Some(&raw mut idle),
            Some(&raw mut kernel),
            Some(&raw mut user),
        )
    }
    .ok()?;
    // Kernel time includes the idle time.
    let total = ticks(kernel) + ticks(user);
    Some(CpuTime {
        busy: total.saturating_sub(ticks(idle)),
        total,
    })
}

/// Windows counts what it holds as cache among what is available, so
/// nothing shows as cached.
fn memory() -> Option<MemoryReading> {
    let mut status = MEMORYSTATUSEX {
        dwLength: size_of::<MEMORYSTATUSEX>() as u32,
        ..Default::default()
    };
    // SAFETY: the structure's size is set as the call requires.
    unsafe { GlobalMemoryStatusEx(&mut status) }.ok()?;
    Some(MemoryReading {
        total: status.ullTotalPhys,
        available: status.ullAvailPhys,
        free: status.ullAvailPhys,
    })
}

/// How full a drive is, if it stays in the computer. Even asking what kind
/// of drive it is can wait on one that has stopped answering, so both are
/// asked on the drive's own thread.
fn usage(root: &Path) -> Option<(u64, u64)> {
    let root: Vec<u16> = root.as_os_str().encode_wide().chain(Some(0)).collect();
    let root = PCWSTR(root.as_ptr());
    // SAFETY: `root` is nul-terminated and outlives both calls, which write
    // only to the locals given.
    if unsafe { GetDriveTypeW(root) } != DRIVE_FIXED {
        return None;
    }
    let (mut available, mut total) = (0u64, 0u64);
    unsafe { GetDiskFreeSpaceExW(root, Some(&raw mut available), Some(&raw mut total), None) }
        .ok()?;
    Some((total, available))
}

fn processes() -> Vec<ProcessReading> {
    // SAFETY: a snapshot of processes, owned and closed by `Handle`.
    let Ok(snapshot) = (unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }) else {
        return Vec::new();
    };
    let snapshot = Handle(snapshot);
    let mut entry = PROCESSENTRY32W {
        dwSize: size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    let mut out = Vec::new();
    // SAFETY: `entry` has its size set and lives through each call.
    let mut more = unsafe { Process32FirstW(snapshot.0, &mut entry) }.is_ok();
    while more {
        // Id 0 is the idle process: the processors' free time, not a program.
        if entry.th32ProcessID != 0
            && let Some(p) = process(entry.th32ProcessID, &entry.szExeFile)
        {
            out.push(p);
        }
        // SAFETY: as above.
        more = unsafe { Process32NextW(snapshot.0, &mut entry) }.is_ok();
    }
    out
}

/// The system's own processes and other users' refuse to be looked at;
/// they are left out.
fn process(pid: u32, exe: &[u16]) -> Option<ProcessReading> {
    // SAFETY: the handle is owned and closed by `Handle`.
    let handle =
        Handle(unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?);
    let (mut created, mut exited, mut kernel, mut user) = Default::default();
    // SAFETY: four out-parameters that live through the call.
    unsafe {
        GetProcessTimes(
            handle.0,
            &raw mut created,
            &raw mut exited,
            &raw mut kernel,
            &raw mut user,
        )
    }
    .ok()?;
    let mut counters = PROCESS_MEMORY_COUNTERS {
        cb: size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
        ..Default::default()
    };
    // SAFETY: the structure's size is given and it lives through the call.
    let read = unsafe { K32GetProcessMemoryInfo(handle.0, &mut counters, counters.cb) };
    let memory = if read.as_bool() {
        counters.WorkingSetSize as u64
    } else {
        0
    };
    Some(ProcessReading {
        pid,
        name: process_name(exe),
        cpu: ticks(kernel) + ticks(user),
        memory,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_this_computer_and_its_fixed_drives() {
        let reading = WindowsSystem::new().read();
        assert!(reading.cpu.first().is_some_and(|c| c.total > 0));
        assert!(!reading.processes.is_empty());
        let system = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into());
        assert!(
            reading
                .disks
                .iter()
                .any(|d| d.mount == format!("{system}\\") && d.available <= d.total),
            "{:?}",
            reading.disks
        );
    }

    #[test]
    fn a_drive_that_is_not_there_is_left_out() {
        // SAFETY: no arguments.
        let mask = unsafe { GetLogicalDrives() };
        let missing = (0..26u8).rev().find(|i| mask & (1 << i) == 0).unwrap();
        let root = format!("{}:\\", char::from(b'A' + missing));
        assert_eq!(usage(Path::new(&root)), None);
        assert!(usage(Path::new("C:\\")).is_some_and(|(total, _)| total > 0));
    }
}
