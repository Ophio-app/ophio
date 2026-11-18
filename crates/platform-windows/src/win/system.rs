// SPDX-License-Identifier: Apache-2.0
use super::Handle;
use crate::system::{
    battery_reading, drive_roots, memory_reading, native_text, pagefile_reading, process_name,
};
use ports::system::{
    BatteryReading, CpuTime, Disk, DiskAsking, DiskReading, GpuReading, InterfaceReading,
    MemoryReading, ProcessReading, SwapReading, SystemMonitor, SystemReading,
};
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use windows::{
    Win32::{
        Foundation::FILETIME,
        Graphics::Dxgi::{CreateDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE, IDXGIFactory1},
        NetworkManagement::IpHelper::{
            FreeMibTable, GetIfTable2, MIB_IF_TABLE2, MIB_IF_TYPE_LOOPBACK,
        },
        Storage::FileSystem::{GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDrives},
        System::{
            Diagnostics::ToolHelp::{
                CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
                TH32CS_SNAPPROCESS,
            },
            Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS},
            ProcessStatus::{
                ENUM_PAGE_FILE_INFORMATION, EnumPageFilesW, K32GetProcessMemoryInfo,
                PROCESS_MEMORY_COUNTERS,
            },
            Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW},
            SystemInformation::{
                GetSystemInfo, GetTickCount64, GlobalMemoryStatusEx, MEMORYSTATUSEX, SYSTEM_INFO,
            },
            Threading::{
                GetProcessTimes, GetSystemTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
            },
        },
    },
    core::{BOOL, PCWSTR, w},
};

/// `GetDriveTypeW` for a drive that stays in the computer; network and
/// removable drives are left out, as they can be slow to answer.
const DRIVE_FIXED: u32 = 3;

#[derive(Debug, Default)]
pub struct WindowsSystem {
    disks: DiskAsking,
    graphics: super::gpu::GpuCounters,
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
        let network = network();
        let processes = processes();
        SystemReading {
            cpu,
            network,
            cpu_model: cpu_model(),
            swap: swap(),
            battery: battery(),
            gpus: graphics(&self.graphics.read()),
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

#[link(name = "ntdll")]
unsafe extern "system" {
    fn NtQuerySystemInformation(
        class: u32,
        info: *mut core::ffi::c_void,
        length: u32,
        returned: *mut u32,
    ) -> i32;
}

/// SystemMemoryListInformation separates free/zeroed RAM from standby cache.
/// The shared memory contract requires both figures, so an unavailable native
/// query leaves this optional reading absent rather than fabricating cache=0.
fn memory() -> Option<MemoryReading> {
    // SYSTEM_MEMORY_LIST_INFORMATION: five page-list counts, eight standby
    // priorities, eight repurposed priorities, modified-pagefile count.
    let mut lists = [0usize; 22];
    let mut page = SYSTEM_INFO::default();
    let mut returned = 0;
    // SAFETY: pointer-aligned initialized native output with the expected
    // size, and an independent initialized SYSTEM_INFO output.
    let result = unsafe {
        GetSystemInfo(&mut page);
        NtQuerySystemInformation(
            80,
            lists.as_mut_ptr().cast(),
            size_of_val(&lists) as u32,
            &mut returned,
        )
    };
    if result < 0 || returned < (2 * size_of::<usize>()) as u32 {
        return None;
    }
    let mut status = MEMORYSTATUSEX {
        dwLength: size_of::<MEMORYSTATUSEX>() as u32,
        ..Default::default()
    };
    // SAFETY: the structure's size is set as the call requires.
    unsafe { GlobalMemoryStatusEx(&mut status) }.ok()?;
    memory_reading(
        status.ullTotalPhys,
        status.ullAvailPhys,
        lists[0] as u64,
        lists[1] as u64,
        u64::from(page.dwPageSize),
    )
}

/// IP Helper supplies 64-bit byte counters, including virtual interfaces.
fn network() -> Vec<InterfaceReading> {
    struct Table(*mut MIB_IF_TABLE2);
    impl Drop for Table {
        fn drop(&mut self) {
            // SAFETY: the table was allocated by GetIfTable2 and is freed once.
            unsafe { FreeMibTable(self.0.cast()) };
        }
    }
    let mut table = std::ptr::null_mut();
    // SAFETY: writable out-pointer; successful allocation is owned below.
    if unsafe { GetIfTable2(&mut table) }.is_err() || table.is_null() {
        return Vec::new();
    }
    let table = Table(table);
    // SAFETY: GetIfTable2 returns NumEntries initialized rows in this allocation.
    let rows = unsafe {
        std::slice::from_raw_parts((*table.0).Table.as_ptr(), (*table.0).NumEntries as usize)
    };
    rows.iter()
        .filter(|row| row.Type != MIB_IF_TYPE_LOOPBACK)
        .map(|row| {
            let alias = native_text(&row.Alias)
                .or_else(|| native_text(&row.Description))
                .unwrap_or_else(|| "Network interface".into());
            InterfaceReading {
                // The index disambiguates adapters with identical labels.
                name: format!("{alias} ({})", row.InterfaceIndex),
                received: row.InOctets,
                sent: row.OutOctets,
            }
        })
        .collect()
}

fn cpu_model() -> Option<String> {
    let mut text = [0u16; 512];
    let mut bytes = size_of_val(&text) as u32;
    // SAFETY: read-only registry query into the supplied, sized buffer. The
    // predefined key is borrowed and must not be closed.
    unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            w!("HARDWARE\\DESCRIPTION\\System\\CentralProcessor\\0"),
            w!("ProcessorNameString"),
            RRF_RT_REG_SZ,
            None,
            Some(text.as_mut_ptr().cast()),
            Some(&raw mut bytes),
        )
    }
    .ok()
    .ok()?;
    native_text(&text)
}

fn battery() -> Option<BatteryReading> {
    let mut status = SYSTEM_POWER_STATUS::default();
    // SAFETY: writable structure; only reads the current power status.
    unsafe { GetSystemPowerStatus(&mut status) }.ok()?;
    battery_reading(status.BatteryFlag, status.BatteryLifePercent)
}

/// Enumerate actual pagefiles: GlobalMemoryStatusEx's "pagefile" fields
/// describe the commit limit, not pagefile capacity or free pagefile bytes.
fn swap() -> Option<SwapReading> {
    unsafe extern "system" fn add(
        context: *mut core::ffi::c_void,
        info: *mut ENUM_PAGE_FILE_INFORMATION,
        _: PCWSTR,
    ) -> BOOL {
        // SAFETY: EnumPageFilesW calls synchronously with our live accumulator
        // and its initialized pagefile information. No filename is inspected.
        let (total, used) = unsafe { (&mut *context.cast::<Option<(u64, u64)>>(), &*info) };
        *total = total.and_then(|(pages, in_use)| {
            Some((
                pages.checked_add(used.TotalSize as u64)?,
                in_use.checked_add(used.TotalInUse as u64)?,
            ))
        });
        BOOL::from(total.is_some())
    }
    let mut totals = Some((0u64, 0u64));
    let mut info = SYSTEM_INFO::default();
    // SAFETY: GetSystemInfo initializes the structure. EnumPageFilesW borrows
    // the accumulator only for this synchronous invocation.
    unsafe {
        GetSystemInfo(&mut info);
        EnumPageFilesW(Some(add), (&raw mut totals).cast()).ok()?;
    }
    let (total, used) = totals?;
    pagefile_reading(total, used, u64::from(info.dwPageSize))
}

/// DXGI supplies hardware identity and dedicated capacity; the optional PDH
/// driver counters supply whole-adapter usage. DXGI's per-process memory
/// budget is deliberately not mistaken for total system graphics usage.
fn graphics(counters: &crate::gpu::GpuReadings) -> Vec<GpuReading> {
    // SAFETY: creates a local DXGI factory; no desktop or output is opened.
    let Ok(factory): Result<IDXGIFactory1, _> = (unsafe { CreateDXGIFactory1() }) else {
        return Vec::new();
    };
    let mut readings = Vec::new();
    let mut index = 0;
    // SAFETY: enumeration and adapter descriptions are read-only. Interfaces
    // are released automatically; software rendering adapters are excluded.
    while let Ok(adapter) = unsafe { factory.EnumAdapters1(index) } {
        index += 1;
        let Ok(desc) = (unsafe { adapter.GetDesc1() }) else {
            continue;
        };
        if desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0 {
            continue;
        }
        let Some(name) = native_text(&desc.Description) else {
            continue;
        };
        let (busy, memory_used) = counters
            .get(&(desc.AdapterLuid.LowPart, desc.AdapterLuid.HighPart))
            .copied()
            .unwrap_or_default();
        readings.push(GpuReading {
            name,
            busy,
            memory_used,
            memory_total: (desc.DedicatedVideoMemory > 0)
                .then_some(desc.DedicatedVideoMemory as u64),
            ..GpuReading::default()
        });
    }
    readings
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
    if !read.as_bool() {
        // This field is not optional in the shared contract. Omit an unreadable
        // process instead of inventing an empty working set.
        return None;
    }
    let memory = counters.WorkingSetSize as u64;
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
    fn native_optional_readings_are_valid_without_requiring_hardware() {
        // Read-only: no network traffic, desktop access, power changes or
        // graphics workload. Missing driver/hardware readings are legitimate.
        let interfaces = network();
        let names: std::collections::HashSet<_> = interfaces.iter().map(|i| &i.name).collect();
        assert_eq!(names.len(), interfaces.len());
        assert!(interfaces.iter().all(|i| !i.name.trim().is_empty()));
        if let Some(memory) = memory() {
            assert!(memory.free <= memory.available && memory.available <= memory.total);
        }
        if let Some(model) = cpu_model() {
            assert!(!model.trim().is_empty());
        }
        if let Some(swap) = swap() {
            assert!(swap.free <= swap.total);
            assert_eq!(swap.swapped_in, None);
            assert_eq!(swap.swapped_out, None);
        }
        if let Some(battery) = battery() {
            assert!((0.0..=100.0).contains(&battery.percent));
        }
        for gpu in graphics(&crate::gpu::GpuReadings::new()) {
            assert!(!gpu.name.trim().is_empty());
            assert!(gpu.memory_total.is_none_or(|total| total > 0));
            assert_eq!(gpu.memory_used, None);
            assert_eq!(gpu.busy, None);
            assert_eq!(gpu.celsius, None);
        }
    }

    #[test]
    fn pagefile_callback_accumulates_real_page_counts() {
        // Exercise the native enumeration separately; an installed pagefile
        // is optional, but a successful read must satisfy byte invariants.
        if let Some(reading) = swap() {
            let mut info = SYSTEM_INFO::default();
            // SAFETY: reads system page size into our local structure.
            unsafe { GetSystemInfo(&mut info) };
            assert!(info.dwPageSize > 0);
            assert_eq!(reading.total % u64::from(info.dwPageSize), 0);
            assert_eq!(reading.free % u64::from(info.dwPageSize), 0);
        }
    }

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
