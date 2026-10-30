// SPDX-License-Identifier: Apache-2.0
//! How busy the computer is: processors, memory, swap, disks, network,
//! temperatures, graphics and the busiest processes.
//!
//! A reading holds the operating system's running totals at one moment.
//! Percentages and rates come from the difference between two readings, so
//! an adapter only reports what the system counts.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, mpsc};
use std::time::{Duration, Instant};

/// How long a disk may take to say how full it is before it is left out: a
/// network or user-space file system, or a failing drive, that stops
/// answering would otherwise hold the whole reading.
pub const DISK_WAIT: Duration = Duration::from_millis(400);
/// The most disks asked at once that haven't answered. Past it the rest are
/// left out, so disks that all stop answering hold this many threads at most.
pub const MAX_ASKING: usize = 32;

/// Reads the computer's counters. Each part the platform can't read is left
/// out; a reading as a whole never fails.
pub trait SystemMonitor: Send + Sync + std::fmt::Debug + 'static {
    /// The counters now. Blocking: it reads the system's files or calls.
    /// A reading is timed as it is asked, so what rates are made from is
    /// read first and together, before anything that may wait.
    fn read(&self) -> SystemReading;
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SystemReading {
    /// Every processor together, then each one. The unit is the platform's
    /// own, the same for processes.
    pub cpu: Vec<CpuTime>,
    pub cpu_model: Option<String>,
    pub memory: Option<MemoryReading>,
    pub swap: Option<SwapReading>,
    /// Average runnable processes over 1, 5 and 15 minutes.
    pub load: Option<[f64; 3]>,
    pub uptime_s: Option<u64>,
    pub disks: Vec<DiskReading>,
    pub network: Vec<InterfaceReading>,
    pub temperatures: Vec<TemperatureReading>,
    pub gpus: Vec<GpuReading>,
    pub battery: Option<BatteryReading>,
    pub processes: Vec<ProcessReading>,
}

/// Time spent since boot.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CpuTime {
    pub busy: u64,
    pub total: u64,
}

/// Bytes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MemoryReading {
    pub total: u64,
    /// What programs could have without swapping.
    pub available: u64,
    /// Not in use at all.
    pub free: u64,
}

/// Bytes; `swapped_in`/`swapped_out` count since boot.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SwapReading {
    pub total: u64,
    pub free: u64,
    pub swapped_in: Option<u64>,
    pub swapped_out: Option<u64>,
}

/// A disk to ask how full it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Disk {
    /// What it is known by while it is asked, which stays the same however
    /// it is reached: a device, as where one is mounted may change.
    pub key: String,
    /// Where it is mounted, as shown.
    pub mount: String,
    /// Where it is asked.
    pub path: PathBuf,
}

/// How full a disk is: its total and available bytes.
pub type DiskUsage = fn(&Path) -> Option<(u64, u64)>;

/// Asks disks how full they are, each on a thread of its own, and waits
/// for them no longer than [`DISK_WAIT`]. One that hasn't answered is left
/// out, and not asked again, until it does, so one that never answers holds
/// one thread rather than one for every reading.
#[derive(Debug, Default)]
pub struct DiskAsking {
    asking: Arc<Mutex<HashSet<String>>>,
}

impl DiskAsking {
    /// The disks that answer in time, in the order given. A disk known twice
    /// is asked once, and one that holds nothing is left out.
    pub fn ask(&self, disks: impl IntoIterator<Item = Disk>, usage: DiskUsage) -> Vec<DiskReading> {
        let mut seen = HashSet::new();
        let (answer, answers) = mpsc::channel();
        let mut asked = Vec::new();
        for Disk { key, mount, path } in disks {
            if !seen.insert(key.clone()) {
                continue;
            }
            let held = {
                let mut asking = self.lock();
                asking.len() < MAX_ASKING && asking.insert(key.clone())
            };
            if !held {
                continue;
            }
            let (answer, asking, n, disk) = (
                answer.clone(),
                self.asking.clone(),
                asked.len(),
                key.clone(),
            );
            let spawned = std::thread::Builder::new()
                .name("disk usage".into())
                .spawn(move || {
                    let usage = usage(&path);
                    asking
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .remove(&disk);
                    let _ = answer.send((n, usage));
                });
            match spawned {
                Ok(_) => asked.push((mount, None)),
                Err(_) => {
                    self.lock().remove(&key);
                }
            }
        }
        drop(answer);
        let deadline = Instant::now() + DISK_WAIT;
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            let Ok((n, usage)) = answers.recv_timeout(left) else {
                break;
            };
            asked[n].1 = usage;
        }
        asked
            .into_iter()
            .filter_map(|(mount, usage)| {
                let (total, available) = usage?;
                (total > 0).then_some(DiskReading {
                    mount,
                    total,
                    available,
                })
            })
            .collect()
    }

    /// Disks asked that haven't answered yet.
    pub fn unanswered(&self) -> usize {
        self.lock().len()
    }

    fn lock(&self) -> MutexGuard<'_, HashSet<String>> {
        self.asking.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiskReading {
    /// Where it is mounted: `/`, `/home`, `C:\`.
    pub mount: String,
    pub total: u64,
    pub available: u64,
}

/// Bytes since boot, through one network interface.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InterfaceReading {
    pub name: String,
    pub received: u64,
    pub sent: u64,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct TemperatureReading {
    /// The sensor as people know it: "CPU", "NVMe", "GPU edge".
    pub label: String,
    pub celsius: f32,
    /// The processor's own sensor, the one shown first.
    pub cpu: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct GpuReading {
    pub name: String,
    /// Percent of the time it was busy, as the driver reports it.
    pub busy: Option<f32>,
    pub memory_used: Option<u64>,
    pub memory_total: Option<u64>,
    pub celsius: Option<f32>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct BatteryReading {
    pub percent: f32,
    pub charging: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProcessReading {
    pub pid: u32,
    /// The program's short name, never its command line, which can hold
    /// secrets.
    pub name: String,
    /// Processor time since it started, in the unit of [`CpuTime`].
    pub cpu: u64,
    /// Resident memory, bytes.
    pub memory: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};

    fn disk(key: &str, mount: &str) -> Disk {
        Disk {
            key: key.into(),
            mount: mount.into(),
            path: PathBuf::from(mount),
        }
    }

    fn mounts(disks: Vec<DiskReading>) -> Vec<String> {
        disks.into_iter().map(|d| d.mount).collect()
    }

    fn until_answered(asking: &DiskAsking) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while asking.unanswered() > 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn a_disk_that_does_not_answer_is_left_out_and_asked_once_until_it_does() {
        static STUCK: AtomicBool = AtomicBool::new(true);
        static ASKED: AtomicUsize = AtomicUsize::new(0);
        fn usage(disk: &Path) -> Option<(u64, u64)> {
            if disk.ends_with("stuck") {
                ASKED.fetch_add(1, SeqCst);
                while STUCK.load(SeqCst) {
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
            Some((100, 40))
        }
        let asking = DiskAsking::default();
        let ask = |stuck: &str| {
            let started = Instant::now();
            let disks = asking.ask([disk("a", "/"), disk("b", stuck)], usage);
            assert!(started.elapsed() < DISK_WAIT + Duration::from_secs(1));
            mounts(disks)
        };
        assert_eq!(ask("/mnt/stuck"), ["/"]);
        assert_eq!(asking.unanswered(), 1);
        // Nor where it is reached next.
        assert_eq!(ask("/mnt/moved/stuck"), ["/"]);
        assert_eq!(ASKED.load(SeqCst), 1);
        STUCK.store(false, SeqCst);
        until_answered(&asking);
        assert_eq!(ask("/mnt/moved/stuck"), ["/", "/mnt/moved/stuck"]);
        assert_eq!(ASKED.load(SeqCst), 2);
    }

    #[test]
    fn disks_that_do_not_answer_hold_no_more_than_so_many_threads() {
        static STUCK: AtomicBool = AtomicBool::new(true);
        static ASKED: AtomicUsize = AtomicUsize::new(0);
        fn usage(_: &Path) -> Option<(u64, u64)> {
            ASKED.fetch_add(1, SeqCst);
            while STUCK.load(SeqCst) {
                std::thread::sleep(Duration::from_millis(10));
            }
            Some((100, 40))
        }
        let disks = |from: usize| {
            (from..from + MAX_ASKING + 4)
                .map(|n| disk(&n.to_string(), &format!("/mnt/{n}")))
                .collect::<Vec<_>>()
        };
        let asking = DiskAsking::default();
        assert!(asking.ask(disks(0), usage).is_empty());
        assert_eq!(asking.unanswered(), MAX_ASKING);
        assert_eq!(ASKED.load(SeqCst), MAX_ASKING);
        // Others found while those are asked wait their turn.
        assert!(asking.ask(disks(100), usage).is_empty());
        assert_eq!(asking.unanswered(), MAX_ASKING);
        assert_eq!(ASKED.load(SeqCst), MAX_ASKING);
        STUCK.store(false, SeqCst);
        until_answered(&asking);
        assert_eq!(asking.unanswered(), 0);
        // A larger batch may skip disks before any worker gets scheduled.
        // Reuse every slot without depending on workers finishing mid-spawn.
        for batch in disks(100).chunks(MAX_ASKING) {
            let expected = batch.iter().map(|d| d.mount.clone()).collect::<Vec<_>>();
            assert_eq!(mounts(asking.ask(batch.iter().cloned(), usage)), expected);
            assert_eq!(asking.unanswered(), 0);
        }
        assert_eq!(ASKED.load(SeqCst), 2 * MAX_ASKING + 4);
    }

    #[test]
    fn a_disk_known_twice_is_asked_once_and_an_empty_one_is_left_out() {
        static ASKED: AtomicUsize = AtomicUsize::new(0);
        fn usage(disk: &Path) -> Option<(u64, u64)> {
            ASKED.fetch_add(1, SeqCst);
            match disk.to_str() {
                Some("/empty") => Some((0, 0)),
                Some("/gone") => None,
                _ => Some((100, 40)),
            }
        }
        let asking = DiskAsking::default();
        let found = [
            disk("a", "/"),
            disk("a", "/home"),
            disk("b", "/empty"),
            disk("c", "/gone"),
            disk("d", "/data"),
        ];
        // Found slowly, so the first has answered before it is found again.
        let disks = asking.ask(
            found.into_iter().inspect(|d| {
                if d.mount == "/home" {
                    until_answered(&asking);
                }
            }),
            usage,
        );
        assert_eq!(
            disks,
            [
                DiskReading {
                    mount: "/".into(),
                    total: 100,
                    available: 40,
                },
                DiskReading {
                    mount: "/data".into(),
                    total: 100,
                    available: 40,
                },
            ]
        );
        assert_eq!(ASKED.load(SeqCst), 4);
    }
}
