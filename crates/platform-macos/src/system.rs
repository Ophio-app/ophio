// SPDX-License-Identifier: Apache-2.0
//! How busy the computer is, as a Mac counts it: processors, memory, swap,
//! local disks, network, battery and the processes this user may look at.
//! Temperatures and graphics need private interfaces, so Control leaves them
//! out here.
#![allow(unsafe_code)]
// libc keeps these Mach declarations but points people at another crate for them.
#![allow(deprecated)]

use crate::process;
use ports::system::{
    BatteryReading, CpuTime, Disk, DiskAsking, InterfaceReading, MemoryReading, ProcessReading,
    SwapReading, SystemMonitor, SystemReading,
};
use std::{
    ffi::{CStr, CString},
    mem::{size_of, zeroed},
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

/// The processor tick, in nanoseconds: the unit [`CpuTime`] is in here.
const NANOS_PER_TICK: u64 = 10_000_000;

#[derive(Debug, Default)]
pub struct MacSystem {
    disks: DiskAsking,
}

impl MacSystem {
    pub fn new() -> Self {
        Self::default()
    }
}

impl SystemMonitor for MacSystem {
    fn read(&self) -> SystemReading {
        // What rates are made from is read first and together: a disk may be
        // slow to answer.
        let cpu = cpu();
        let network = network();
        let processes = processes();
        let statistics = statistics();
        SystemReading {
            cpu,
            cpu_model: process::sysctl_text(c"machdep.cpu.brand_string"),
            memory: statistics.as_ref().and_then(memory),
            swap: swap(statistics.as_ref()),
            load: load(),
            uptime_s: uptime(),
            disks: self.disks.ask(local_disks(), usage),
            network,
            battery: battery(),
            processes,
            ..SystemReading::default()
        }
    }
}

/// Every processor together, then each one.
fn cpu() -> Vec<CpuTime> {
    let mut count: libc::natural_t = 0;
    let mut info: libc::processor_info_array_t = std::ptr::null_mut();
    let mut size: libc::mach_msg_type_number_t = 0;
    // SAFETY: the out-parameters live through the call, and the array it makes
    // is released below.
    let result = unsafe {
        libc::host_processor_info(
            libc::mach_host_self(),
            libc::PROCESSOR_CPU_LOAD_INFO,
            &raw mut count,
            &raw mut info,
            &raw mut size,
        )
    };
    if result != libc::KERN_SUCCESS || info.is_null() {
        return Vec::new();
    }
    let states = libc::CPU_STATE_MAX as usize;
    // SAFETY: the system gives `count` records of CPU_STATE_MAX integers.
    let ticks = unsafe {
        std::slice::from_raw_parts(info.cast::<libc::integer_t>(), count as usize * states)
    };
    let mut each: Vec<CpuTime> = ticks
        .chunks_exact(states)
        .map(|processor| {
            let at = |state: libc::c_int| {
                u64::from(processor[usize::try_from(state).unwrap_or(0)].cast_unsigned())
            };
            let idle = at(libc::CPU_STATE_IDLE);
            let total = at(libc::CPU_STATE_USER)
                + at(libc::CPU_STATE_SYSTEM)
                + at(libc::CPU_STATE_NICE)
                + idle;
            CpuTime {
                busy: total.saturating_sub(idle),
                total,
            }
        })
        .collect();
    // SAFETY: this is the array the call above made, of `size` integers.
    unsafe {
        libc::vm_deallocate(
            libc::mach_task_self(),
            info as libc::vm_address_t,
            size as usize * size_of::<libc::integer_t>(),
        );
    }
    if each.is_empty() {
        return each;
    }
    let all = each.iter().fold(CpuTime::default(), |sum, one| CpuTime {
        busy: sum.busy + one.busy,
        total: sum.total + one.total,
    });
    each.insert(0, all);
    each
}

fn statistics() -> Option<libc::vm_statistics64> {
    // SAFETY: an all-zero vm_statistics64 is valid: it holds only integers.
    let mut stats: libc::vm_statistics64 = unsafe { zeroed() };
    let mut count = libc::HOST_VM_INFO64_COUNT;
    // SAFETY: the pointer and count describe `stats`, which outlives the call.
    let result = unsafe {
        libc::host_statistics64(
            libc::mach_host_self(),
            libc::HOST_VM_INFO64,
            std::ptr::from_mut(&mut stats).cast(),
            &raw mut count,
        )
    };
    (result == libc::KERN_SUCCESS).then_some(stats)
}

fn page_size() -> u64 {
    // SAFETY: sysconf takes a constant and can't misuse memory.
    let size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    u64::try_from(size).unwrap_or(4096)
}

/// What programs could have without swapping: pages free, those the system
/// keeps only in case they are wanted again, and those programs said it may
/// take back.
fn memory(stats: &libc::vm_statistics64) -> Option<MemoryReading> {
    let total = process::sysctl_value(c"hw.memsize", 0u64)?;
    let page = page_size();
    let free = u64::from(stats.free_count).saturating_mul(page);
    let reclaimable =
        u64::from(stats.inactive_count).saturating_add(u64::from(stats.purgeable_count));
    Some(MemoryReading {
        total,
        available: free
            .saturating_add(reclaimable.saturating_mul(page))
            .min(total),
        free: free.min(total),
    })
}

fn swap(stats: Option<&libc::vm_statistics64>) -> Option<SwapReading> {
    // SAFETY: an all-zero xsw_usage is valid: it holds only integers.
    let usage: libc::xsw_usage = process::sysctl_value(c"vm.swapusage", unsafe { zeroed() })?;
    let page = page_size();
    Some(SwapReading {
        total: usage.xsu_total,
        free: usage.xsu_avail,
        swapped_in: stats.map(|stats| stats.swapins.saturating_mul(page)),
        swapped_out: stats.map(|stats| stats.swapouts.saturating_mul(page)),
    })
}

fn load() -> Option<[f64; 3]> {
    let mut averages = [0f64; 3];
    // SAFETY: the pointer and count describe `averages`, which outlives the call.
    (unsafe { libc::getloadavg(averages.as_mut_ptr(), 3) } == 3).then_some(averages)
}

fn uptime() -> Option<u64> {
    let boot = process::sysctl_value(
        c"kern.boottime",
        libc::timeval {
            tv_sec: 0,
            tv_usec: 0,
        },
    )?;
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
    now.checked_sub(u64::try_from(boot.tv_sec).ok()?)
}

/// Whether a mounted volume is one a person would call a disk: not the parts
/// of the system's own volume, which share its space, or a simulator's image.
fn shown(mount: &str, file_system: &str) -> bool {
    file_system != "devfs"
        && !mount.starts_with("/System/Volumes/")
        && !mount.starts_with("/Library/Developer/CoreSimulator/")
        && !mount.starts_with("/private/var/")
}

fn c_text(bytes: &[libc::c_char]) -> String {
    let bytes: Vec<u8> = bytes
        .iter()
        .take_while(|&&byte| byte != 0)
        .map(|&byte| byte.cast_unsigned())
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

fn local_disks() -> Vec<Disk> {
    let mut mounts: *mut libc::statfs = std::ptr::null_mut();
    // SAFETY: the call points `mounts` at its own list and doesn't wait on any
    // disk with this flag.
    let count = unsafe { libc::getmntinfo(&raw mut mounts, libc::MNT_NOWAIT) };
    if count <= 0 || mounts.is_null() {
        return Vec::new();
    }
    // SAFETY: the list holds `count` records and stays until this thread's next
    // call, which comes after they are copied here.
    let mounts = unsafe { std::slice::from_raw_parts(mounts, usize::try_from(count).unwrap_or(0)) };
    mounts
        .iter()
        .filter(|mount| {
            mount.f_flags & libc::MNT_LOCAL.cast_unsigned() != 0
                && shown(&c_text(&mount.f_mntonname), &c_text(&mount.f_fstypename))
        })
        .map(|mount| {
            let path = c_text(&mount.f_mntonname);
            Disk {
                key: c_text(&mount.f_mntfromname),
                path: PathBuf::from(&path),
                mount: path,
            }
        })
        .collect()
}

/// A disk's total and available bytes.
fn usage(path: &Path) -> Option<(u64, u64)> {
    let path = CString::new(path.as_os_str().as_bytes()).ok()?;
    // SAFETY: an all-zero statfs is valid: it holds only integers and byte arrays.
    let mut stats: libc::statfs = unsafe { zeroed() };
    // SAFETY: both pointers outlive the call.
    if unsafe { libc::statfs(path.as_ptr(), &raw mut stats) } != 0 {
        return None;
    }
    let block = u64::from(stats.f_bsize);
    Some((
        stats.f_blocks.saturating_mul(block),
        stats.f_bavail.saturating_mul(block),
    ))
}

/// Bytes through each network interface that has carried any. These counters
/// are 32 bits, so a rate made across their wrap reads as nothing for a moment.
fn network() -> Vec<InterfaceReading> {
    let mut list: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: the call points `list` at a list released below.
    if unsafe { libc::getifaddrs(&raw mut list) } != 0 {
        return Vec::new();
    }
    let mut interfaces = Vec::new();
    let mut entry = list;
    while !entry.is_null() {
        // SAFETY: `entry` is a node of the list, which stays until freed below.
        let this = unsafe { &*entry };
        entry = this.ifa_next;
        if this.ifa_addr.is_null() || this.ifa_data.is_null() || this.ifa_name.is_null() {
            continue;
        }
        // SAFETY: checked not null; the system fills it for every address.
        let family = i32::from(unsafe { (*this.ifa_addr).sa_family });
        if family != libc::AF_LINK || this.ifa_flags & libc::IFF_LOOPBACK.cast_unsigned() != 0 {
            continue;
        }
        // SAFETY: for a link address, `ifa_data` is the interface's if_data.
        let data = unsafe { &*this.ifa_data.cast::<libc::if_data>() };
        if data.ifi_ibytes == 0 && data.ifi_obytes == 0 {
            continue;
        }
        interfaces.push(InterfaceReading {
            // SAFETY: checked not null; a nul-terminated name.
            name: unsafe { CStr::from_ptr(this.ifa_name) }
                .to_string_lossy()
                .into_owned(),
            received: u64::from(data.ifi_ibytes),
            sent: u64::from(data.ifi_obytes),
        });
    }
    // SAFETY: this is the list getifaddrs made.
    unsafe { libc::freeifaddrs(list) };
    interfaces
}

/// A battery's charge from what `pmset -g batt` prints, as in
/// `-InternalBattery-0 (id=1)\t79%; AC attached; not charging present: true`.
fn parse_battery(text: &str) -> Option<BatteryReading> {
    let line = text
        .lines()
        .find(|line| line.trim_start().starts_with("-InternalBattery"))?;
    let (_, details) = line.split_once('\t')?;
    let mut fields = details.split(';').map(str::trim);
    let percent = fields.next()?.strip_suffix('%')?.parse().ok()?;
    let state = fields.next()?;
    Some(BatteryReading {
        percent,
        // Plugged in, whether or not it is taking charge just now.
        charging: state != "discharging",
    })
}

fn battery() -> Option<BatteryReading> {
    let output = containment::inherit::output(
        std::process::Command::new("/usr/bin/pmset").args(["-g", "batt"]),
    )
    .ok()?;
    parse_battery(std::str::from_utf8(&output.stdout).ok()?)
}

fn processes() -> Vec<ProcessReading> {
    let timebase = process::nanoseconds_per_unit();
    process::all()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|pid| {
            let (cpu, memory) = process::usage(pid, timebase)?;
            let process = process::read(pid)?;
            Some(ProcessReading {
                pid: process.pid,
                name: process.name,
                cpu: cpu / NANOS_PER_TICK,
                memory,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reading_holds_what_a_mac_counts() {
        let reading = MacSystem::new().read();
        assert!(reading.cpu.len() >= 2, "together, then each one");
        assert_eq!(
            reading.cpu[0].total,
            reading.cpu[1..].iter().map(|cpu| cpu.total).sum::<u64>()
        );
        assert!(reading.cpu[0].busy <= reading.cpu[0].total);
        assert!(reading.cpu_model.is_some());
        let memory = reading.memory.unwrap();
        assert!(memory.total > 0 && memory.available <= memory.total);
        assert!(memory.free <= memory.available);
        assert!(reading.swap.is_some());
        assert!(reading.load.is_some());
        assert!(reading.uptime_s.is_some_and(|uptime| uptime > 0));
        let root = reading
            .disks
            .iter()
            .find(|disk| disk.mount == "/")
            .expect("the system's own disk is shown");
        assert!(root.total > 0 && root.available <= root.total);
        assert!(
            reading
                .disks
                .iter()
                .all(|disk| !disk.mount.starts_with("/System/Volumes/"))
        );
        let me = reading
            .processes
            .iter()
            .find(|process| process.pid == std::process::id())
            .expect("this process is listed");
        assert!(me.memory > 0);
    }

    #[test]
    fn a_process_uses_time_in_the_processors_unit() {
        // A second of work must not read as hours, which it would if the
        // system's time units were taken for nanoseconds or the reverse.
        let start = std::time::Instant::now();
        let mut spin = 0u64;
        while start.elapsed().as_millis() < 300 {
            spin = std::hint::black_box(spin.wrapping_add(1));
        }
        let me = MacSystem::new()
            .read()
            .processes
            .into_iter()
            .find(|process| process.pid == std::process::id())
            .unwrap();
        assert!(me.cpu >= 10, "{} ticks", me.cpu);
        assert!(me.cpu < 60_000, "{} ticks", me.cpu);
    }

    #[test]
    fn a_batterys_charge_is_read_from_what_pmset_prints() {
        let read = |state: &str| {
            parse_battery(&format!(
                "Now drawing from 'AC Power'\n -InternalBattery-0 (id=27197539)\t79%; {state}; 0:00 remaining present: true\n"
            ))
        };
        assert_eq!(
            read("charging"),
            Some(BatteryReading {
                percent: 79.0,
                charging: true
            })
        );
        assert!(!read("discharging").unwrap().charging);
        assert!(read("AC attached").unwrap().charging);
        assert!(read("charged").unwrap().charging);
        assert_eq!(parse_battery("Now drawing from 'AC Power'\n"), None);
    }

    #[test]
    fn only_a_persons_disks_are_shown() {
        assert!(shown("/", "apfs"));
        assert!(shown("/Volumes/Backup", "hfs"));
        assert!(!shown("/dev", "devfs"));
        assert!(!shown("/System/Volumes/Data", "apfs"));
        assert!(!shown(
            "/Library/Developer/CoreSimulator/Volumes/iOS_24B8117",
            "apfs"
        ));
    }
}
