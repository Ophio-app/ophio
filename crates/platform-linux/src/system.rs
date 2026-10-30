// SPDX-License-Identifier: Apache-2.0
//! The computer's counters, read from `/proc` and `/sys`, and from
//! `nvidia-smi` for NVIDIA graphics, which keeps its own out of `/sys`.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use ports::system::{
    BatteryReading, CpuTime, Disk, DiskAsking, DiskReading, DiskUsage, GpuReading,
    InterfaceReading, MemoryReading, ProcessReading, SwapReading, SystemMonitor, SystemReading,
    TemperatureReading,
};

/// File systems that hold people's files; the rest are kernel or memory.
const DISK_TYPES: &[&str] = &[
    "ext2", "ext3", "ext4", "btrfs", "xfs", "f2fs", "bcachefs", "zfs", "jfs", "reiserfs", "ntfs",
    "ntfs3", "fuseblk", "exfat", "vfat",
];
/// How long `nvidia-smi` may take before its reading is left out.
const NVIDIA_WAIT: Duration = Duration::from_millis(800);
/// The most of a program's output read.
const OUTPUT_LIMIT: usize = 64 * 1024;

#[derive(Debug)]
pub struct LinuxSystem {
    root: PathBuf,
    page_size: u64,
    nvidia_smi: Option<PathBuf>,
    usage: DiskUsage,
    asking: DiskAsking,
}

impl LinuxSystem {
    /// This computer's counters.
    pub fn new() -> Self {
        Self {
            nvidia_smi: on_path("nvidia-smi"),
            ..Self::at("/")
        }
    }

    /// Counters from a tree laid out like `/`, for tests.
    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            page_size: rustix::param::page_size() as u64,
            nvidia_smi: None,
            usage,
            asking: DiskAsking::default(),
        }
    }

    fn text(&self, path: impl AsRef<Path>) -> Option<String> {
        std::fs::read_to_string(self.root.join(path)).ok()
    }

    fn number(&self, path: impl AsRef<Path>) -> Option<u64> {
        self.text(path)?.trim().parse().ok()
    }

    /// Entries of a directory under the root, sorted by name with numbers in
    /// order (`hwmon2` before `hwmon10`).
    fn entries(&self, dir: &str) -> Vec<(String, PathBuf)> {
        let mut entries: Vec<_> = std::fs::read_dir(self.root.join(dir))
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                Some((
                    e.file_name().into_string().ok()?,
                    Path::new(dir).join(e.file_name()),
                ))
            })
            .collect();
        entries.sort_by_key(|(name, _)| natural(name));
        entries
    }

    fn disks(&self) -> Vec<DiskReading> {
        let Some(mounts) = self.text("proc/self/mounts") else {
            return Vec::new();
        };
        let disks = mounts.lines().filter_map(|line| {
            let mut fields = line.split_whitespace();
            let (device, mount, kind) = (fields.next()?, unescape(fields.next()?), fields.next()?);
            // A boot partition is the system's. A disk is known by its
            // device, so one mounted twice (a btrfs subvolume) is one disk,
            // and one still asked is left out wherever it is mounted now.
            (DISK_TYPES.contains(&kind) && !mount.starts_with("/boot")).then(|| Disk {
                key: device.to_string(),
                path: self.root.join(mount.trim_start_matches('/')),
                mount,
            })
        });
        self.asking.ask(disks, self.usage)
    }

    /// Interfaces with hardware behind them; virtual ones (bridges, tunnels,
    /// containers) carry traffic the hardware already counts.
    fn network(&self) -> Vec<InterfaceReading> {
        self.text("proc/net/dev")
            .map(|text| interfaces(&text))
            .unwrap_or_default()
            .into_iter()
            .filter(|i| {
                self.root
                    .join("sys/class/net")
                    .join(&i.name)
                    .join("device")
                    .exists()
            })
            .collect()
    }

    fn temperatures(&self) -> Vec<TemperatureReading> {
        let mut found = Vec::new();
        for (_, dir) in self.entries("sys/class/hwmon") {
            let Some(chip) = self.text(dir.join("name")) else {
                continue;
            };
            let chip = chip.trim();
            for (file, _) in self.entries(&dir.to_string_lossy()) {
                let Some(sensor) = file
                    .strip_suffix("_input")
                    .filter(|s| s.starts_with("temp"))
                else {
                    continue;
                };
                let label = self.text(dir.join(format!("{sensor}_label")));
                let Some((label, cpu)) = sensor_name(chip, label.as_deref().map(str::trim)) else {
                    continue;
                };
                let Some(celsius) = self
                    .text(dir.join(&file))
                    .and_then(|t| t.trim().parse::<i64>().ok())
                    .map(|milli| milli as f32 / 1000.0)
                    .filter(|c| (-40.0..=150.0).contains(c) && *c != 0.0)
                else {
                    continue;
                };
                found.push(TemperatureReading {
                    label,
                    celsius,
                    cpu,
                });
            }
        }
        number_repeats(&mut found);
        found
    }

    fn gpus(&self) -> Vec<GpuReading> {
        let mut gpus: Vec<GpuReading> = self
            .entries("sys/class/drm")
            .into_iter()
            .filter(|(name, _)| {
                name.strip_prefix("card")
                    .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
            })
            .filter_map(|(_, card)| {
                let device = card.join("device");
                // Only drivers that say how busy they are: amdgpu.
                let busy = self.number(device.join("gpu_busy_percent"))?;
                let celsius = self
                    .entries(&device.join("hwmon").to_string_lossy())
                    .first()
                    .and_then(|(_, h)| self.number(h.join("temp1_input")))
                    .map(|milli| milli as f32 / 1000.0);
                Some(GpuReading {
                    name: "AMD graphics".into(),
                    busy: Some(busy as f32),
                    memory_used: self.number(device.join("mem_info_vram_used")),
                    memory_total: self.number(device.join("mem_info_vram_total")),
                    celsius,
                })
            })
            .collect();
        if let Some(smi) = &self.nvidia_smi {
            gpus.extend(nvidia(smi).as_deref().map(nvidia_gpus).unwrap_or_default());
        }
        gpus
    }

    fn battery(&self) -> Option<BatteryReading> {
        self.entries("sys/class/power_supply")
            .into_iter()
            .find_map(|(_, dir)| {
                // A mouse or headset reports its battery with the "Device" scope.
                let kind = self.text(dir.join("type"))?;
                let scope = self.text(dir.join("scope")).unwrap_or_default();
                if kind.trim() != "Battery" || scope.trim() == "Device" {
                    return None;
                }
                Some(BatteryReading {
                    percent: self.number(dir.join("capacity"))?.min(100) as f32,
                    charging: self
                        .text(dir.join("status"))
                        .is_some_and(|s| s.trim() == "Charging"),
                })
            })
    }

    fn processes(&self) -> Vec<ProcessReading> {
        self.entries("proc")
            .into_iter()
            .filter_map(|(name, dir)| {
                let pid = name.parse().ok()?;
                process(pid, &self.text(dir.join("stat"))?, self.page_size)
            })
            .collect()
    }
}

impl Default for LinuxSystem {
    fn default() -> Self {
        Self::new()
    }
}

impl SystemMonitor for LinuxSystem {
    fn read(&self) -> SystemReading {
        // What rates are made from is read first and together: a disk or
        // a program slow to answer would otherwise stretch the time between
        // two readings for some counters and not others.
        let cpu = self
            .text("proc/stat")
            .map(|t| cpu_times(&t))
            .unwrap_or_default();
        let processes = self.processes();
        let vmstat = self
            .text("proc/vmstat")
            .map(|t| fields(&t))
            .unwrap_or_default();
        let network = self.network();
        let meminfo = self
            .text("proc/meminfo")
            .map(|t| fields(&t))
            .unwrap_or_default();
        let kib = |key: &str| meminfo.get(key).map(|v| v.saturating_mul(1024));
        let pages = |key: &str| vmstat.get(key).map(|v| v.saturating_mul(self.page_size));
        SystemReading {
            cpu,
            cpu_model: self.text("proc/cpuinfo").and_then(|t| cpu_model(&t)),
            memory: kib("MemTotal").map(|total| {
                let free = kib("MemFree").unwrap_or(0);
                MemoryReading {
                    total,
                    available: kib("MemAvailable")
                        .unwrap_or_else(|| {
                            free + kib("Cached").unwrap_or(0) + kib("Buffers").unwrap_or(0)
                        })
                        .min(total),
                    free: free.min(total),
                }
            }),
            swap: kib("SwapTotal")
                .filter(|t| *t > 0)
                .map(|total| SwapReading {
                    total,
                    free: kib("SwapFree").unwrap_or(total).min(total),
                    swapped_in: pages("pswpin"),
                    swapped_out: pages("pswpout"),
                }),
            load: self.text("proc/loadavg").and_then(|t| {
                let mut n = t.split_whitespace().map(|v| v.parse().ok());
                Some([n.next()??, n.next()??, n.next()??])
            }),
            uptime_s: self
                .text("proc/uptime")
                .and_then(|t| t.split_whitespace().next()?.parse::<f64>().ok())
                .map(|s| s as u64),
            disks: self.disks(),
            network,
            temperatures: self.temperatures(),
            gpus: self.gpus(),
            battery: self.battery(),
            processes,
        }
    }
}

fn usage(disk: &Path) -> Option<(u64, u64)> {
    let stat = rustix::fs::statvfs(disk).ok()?;
    Some((
        stat.f_blocks.saturating_mul(stat.f_frsize),
        stat.f_bavail.saturating_mul(stat.f_frsize),
    ))
}

/// `/proc/stat`'s `cpu` lines: all processors, then each one. Waiting for
/// input and output counts as idle, as `top` has it.
fn cpu_times(stat: &str) -> Vec<CpuTime> {
    stat.lines()
        .filter(|line| line.starts_with("cpu"))
        .filter_map(|line| {
            let times: Vec<u64> = line
                .split_whitespace()
                .skip(1)
                .map_while(|v| v.parse().ok())
                .collect();
            // user nice system idle iowait irq softirq steal; guest time is
            // already in user.
            let total: u64 = times.iter().take(8).sum();
            let idle = times.get(3)? + times.get(4).unwrap_or(&0);
            Some(CpuTime {
                busy: total - idle,
                total,
            })
        })
        .collect()
}

fn cpu_model(cpuinfo: &str) -> Option<String> {
    cpuinfo
        .lines()
        .find_map(|line| line.strip_prefix("model name")?.split_once(':'))
        .map(|(_, model)| model.trim().to_string())
        .filter(|model| !model.is_empty())
}

/// `Key: value` lines (`/proc/meminfo`) or `key value` lines (`/proc/vmstat`).
fn fields(text: &str) -> HashMap<String, u64> {
    text.lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let key = parts.next()?.trim_end_matches(':');
            Some((key.to_string(), parts.next()?.parse().ok()?))
        })
        .collect()
}

/// `/proc/net/dev`: received bytes are the first count, sent the ninth.
fn interfaces(dev: &str) -> Vec<InterfaceReading> {
    dev.lines()
        .skip(2)
        .filter_map(|line| {
            let (name, counts) = line.split_once(':')?;
            let counts: Vec<u64> = counts
                .split_whitespace()
                .map_while(|v| v.parse().ok())
                .collect();
            Some(InterfaceReading {
                name: name.trim().to_string(),
                received: *counts.first()?,
                sent: *counts.get(8)?,
            })
        })
        .filter(|i| i.name != "lo")
        .collect()
}

/// `/proc/<pid>/stat`. The name is in parentheses and may hold spaces and
/// parentheses itself, so the fields are counted from its last `)`.
fn process(pid: u32, stat: &str, page_size: u64) -> Option<ProcessReading> {
    let open = stat.find('(')?;
    let close = stat.rfind(')')?;
    let name = stat.get(open + 1..close)?.to_string();
    // Fields from the third (state) on; utime is the 14th, stime the 15th,
    // rss the 24th.
    let rest: Vec<&str> = stat.get(close + 1..)?.split_whitespace().collect();
    let field = |n: usize| rest.get(n - 3)?.parse::<u64>().ok();
    Some(ProcessReading {
        pid,
        name,
        cpu: field(14)? + field(15)?,
        memory: field(24)?.saturating_mul(page_size),
    })
}

/// What people call a sensor, and whether it is the processor's own; `None`
/// leaves it out (each core of an Intel processor, a disk's extra sensors,
/// graphics, which are shown with the graphics).
fn sensor_name(chip: &str, label: Option<&str>) -> Option<(String, bool)> {
    let name = |n: &str| Some((n.to_string(), false));
    match (chip, label) {
        ("k10temp" | "zenpower", Some("Tctl" | "Tdie")) | ("coretemp", Some("Package id 0")) => {
            Some(("CPU".into(), true))
        }
        ("cpu_thermal" | "cpu-thermal" | "soc_thermal", _) => Some(("CPU".into(), true)),
        ("k10temp" | "zenpower", Some(l)) if l.starts_with("Tccd") => {
            Some((format!("CPU die {}", &l[4..]), false))
        }
        ("coretemp" | "k10temp" | "zenpower" | "amdgpu" | "nouveau" | "radeon", _) => None,
        ("nvme", Some("Composite") | None) => name("NVMe"),
        ("nvme", _) => None,
        ("spd5118" | "jc42", _) => name("Memory"),
        ("acpitz", _) => name("System"),
        (chip, _)
            if chip.starts_with("r8169")
                || chip.starts_with("igc")
                || chip.starts_with("e1000") =>
        {
            name("Ethernet")
        }
        (chip, _)
            if chip.starts_with("ath")
                || chip.starts_with("iwlwifi")
                || chip.starts_with("mt79") =>
        {
            name("Wi-Fi")
        }
        (chip, _) if chip.contains("xhci") => name("USB"),
        (chip, Some(l)) => Some((format!("{chip} {l}"), false)),
        (chip, None) => name(chip),
    }
}

/// Sensors with the same name (two disks, two memory sticks) are numbered.
fn number_repeats(found: &mut [TemperatureReading]) {
    let mut count: HashMap<String, usize> = HashMap::new();
    for t in found.iter() {
        *count.entry(t.label.clone()).or_default() += 1;
    }
    let mut seen: HashMap<String, usize> = HashMap::new();
    for t in found.iter_mut() {
        if count[&t.label] > 1 {
            let n = seen.entry(t.label.clone()).or_default();
            *n += 1;
            t.label = format!("{} {n}", t.label);
        }
    }
}

/// `nvidia-smi`'s answer, or nothing if it takes too long.
fn nvidia(smi: &Path) -> Option<String> {
    let mut command = Command::new(smi);
    command.args([
        "--query-gpu=name,utilization.gpu,memory.used,memory.total,temperature.gpu",
        "--format=csv,noheader,nounits",
    ]);
    output_within(command, NVIDIA_WAIT)
}

/// What a program prints, if it prints all of it, ends well and does both
/// within `wait`. Its output is read as it comes, so a program that leaves
/// something holding its output open still can't hold the reading up.
fn output_within(mut command: Command, wait: Duration) -> Option<String> {
    use rustix::event::{PollFd, PollFlags, Timespec, poll};
    let deadline = Instant::now() + wait;
    let left = || {
        let left = deadline.checked_duration_since(Instant::now())?;
        Some(Timespec {
            tv_sec: left.as_secs() as i64,
            tv_nsec: left.subsec_nanos() as i64,
        })
    };
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let mut out = Vec::new();
    let read = (|| {
        let mut chunk = [0; 4096];
        loop {
            let mut ready = [PollFd::new(&stdout, PollFlags::IN)];
            match poll(&mut ready, Some(&left()?)) {
                Ok(0) => return None,
                Ok(_) => {}
                Err(rustix::io::Errno::INTR) => continue,
                Err(_) => return None,
            }
            match stdout.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) if out.len() + n <= OUTPUT_LIMIT => out.extend_from_slice(&chunk[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                _ => return None,
            }
        }
        loop {
            match child.try_wait() {
                Ok(Some(status)) => return status.success().then_some(()),
                Ok(None) => {
                    left()?;
                    std::thread::sleep(Duration::from_millis(5))
                }
                Err(_) => return None,
            }
        }
    })();
    drop(stdout);
    if read.is_none() {
        let _ = child.kill();
    }
    let _ = child.wait();
    read.and_then(|()| String::from_utf8(out).ok())
}

/// One line per card: name, busy percent, memory used and total (MiB),
/// temperature. A field the card doesn't report reads `[N/A]`.
fn nvidia_gpus(csv: &str) -> Vec<GpuReading> {
    csv.lines()
        .filter_map(|line| {
            let f: Vec<&str> = line.split(',').map(str::trim).collect();
            let number = |i: usize| f.get(i).and_then(|v| v.parse::<f64>().ok());
            let mib = |i: usize| number(i).map(|v| (v * 1024.0 * 1024.0) as u64);
            Some(GpuReading {
                name: f.first().filter(|n| !n.is_empty())?.to_string(),
                busy: number(1).map(|v| v as f32),
                memory_used: mib(2),
                memory_total: mib(3),
                celsius: number(4).map(|v| v as f32),
            })
        })
        .collect()
}

/// `/proc/self/mounts` writes a space in a mount point as `\040`.
fn unescape(field: &str) -> String {
    let mut out = String::with_capacity(field.len());
    let mut rest = field;
    while let Some(i) = rest.find('\\') {
        out.push_str(&rest[..i]);
        let code = rest
            .get(i + 1..i + 4)
            .and_then(|o| u8::from_str_radix(o, 8).ok());
        match code {
            Some(byte) => {
                out.push(byte as char);
                rest = &rest[i + 4..];
            }
            None => {
                out.push('\\');
                rest = &rest[i + 1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn natural(name: &str) -> (String, u64) {
    let digits = name.len() - name.trim_end_matches(|c: char| c.is_ascii_digit()).len();
    let (stem, n) = name.split_at(name.len() - digits);
    (stem.to_string(), n.parse().unwrap_or(0))
}

fn on_path(program: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(program))
        .find(|path| path.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(files: &[(&str, &str)]) -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        for (path, text) in files {
            let path = root.path().join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        root
    }

    #[test]
    fn reads_processors_memory_swap_and_load() {
        let root = tree(&[
            (
                "proc/stat",
                "cpu  100 0 50 800 50 0 0 0 30 0\ncpu0 60 0 20 400 20 0 0 0 0 0\ncpu1 40 0 30 400 30 0 0 0 0 0\nintr 1 2 3\n",
            ),
            (
                "proc/cpuinfo",
                "processor\t: 0\nmodel name\t: Example 9 9950X\n",
            ),
            (
                "proc/meminfo",
                "MemTotal:       1000 kB\nMemFree:         100 kB\nMemAvailable:    600 kB\nSwapTotal:       400 kB\nSwapFree:        300 kB\n",
            ),
            ("proc/vmstat", "nr_free_pages 9\npswpin 3\npswpout 5\n"),
            ("proc/loadavg", "1.50 0.75 0.25 2/900 1234\n"),
            ("proc/uptime", "3600.42 7000.00\n"),
        ]);
        let system = LinuxSystem::at(root.path());
        let page = system.page_size;
        let r = system.read();
        assert_eq!(
            r.cpu,
            [
                CpuTime {
                    busy: 150,
                    total: 1000
                },
                CpuTime {
                    busy: 80,
                    total: 500
                },
                CpuTime {
                    busy: 70,
                    total: 500
                },
            ]
        );
        assert_eq!(r.cpu_model.as_deref(), Some("Example 9 9950X"));
        assert_eq!(
            r.memory,
            Some(MemoryReading {
                total: 1_024_000,
                available: 614_400,
                free: 102_400
            })
        );
        assert_eq!(
            r.swap,
            Some(SwapReading {
                total: 409_600,
                free: 307_200,
                swapped_in: Some(3 * page),
                swapped_out: Some(5 * page),
            })
        );
        assert_eq!(r.load, Some([1.5, 0.75, 0.25]));
        assert_eq!(r.uptime_s, Some(3600));
    }

    #[test]
    fn a_computer_without_swap_has_none_and_a_missing_file_leaves_its_part_out() {
        let root = tree(&[(
            "proc/meminfo",
            "MemTotal: 1000 kB\nMemFree: 100 kB\nSwapTotal: 0 kB\n",
        )]);
        let r = LinuxSystem::at(root.path()).read();
        assert_eq!(r.swap, None);
        // Without MemAvailable, free and cache are what programs could have.
        assert_eq!(r.memory.map(|m| m.available), Some(102_400));
        assert!(r.cpu.is_empty() && r.load.is_none() && r.processes.is_empty());
    }

    #[test]
    fn counts_only_the_network_with_hardware_behind_it() {
        let root = tree(&[
            (
                "proc/net/dev",
                "Inter-|   Receive\n face |bytes    packets\n    lo: 999 1 0 0 0 0 0 0 999 1 0 0 0 0 0 0\nenp8s0: 1000 10 0 0 0 0 0 0 2000 20 0 0 0 0 0 0\ntailscale0: 50 1 0 0 0 0 0 0 60 1 0 0 0 0 0 0\n",
            ),
            ("sys/class/net/enp8s0/device/vendor", "0x10ec"),
            ("sys/class/net/tailscale0/type", "65534"),
        ]);
        let r = LinuxSystem::at(root.path()).read();
        assert_eq!(
            r.network,
            [InterfaceReading {
                name: "enp8s0".into(),
                received: 1000,
                sent: 2000
            }]
        );
    }

    #[test]
    fn counts_for_rates_are_read_before_a_disk_is_asked() {
        // The disk is asked as the counters move on.
        fn usage(root: &Path) -> Option<(u64, u64)> {
            for (path, text) in [
                ("proc/stat", "cpu  900 0 0 900 0 0 0 0 0 0\n"),
                ("proc/vmstat", "pswpin 900\npswpout 900\n"),
                ("proc/net/dev", "h\nh\nenp8s0: 900 0 0 0 0 0 0 0 900 0\n"),
                ("proc/42/stat", &format!("42 (a) {}\n", ["9"; 22].join(" "))),
            ] {
                std::fs::write(root.join(path), text).unwrap();
            }
            Some((100, 40))
        }
        let root = tree(&[
            ("proc/self/mounts", "/dev/sda1 / ext4 rw 0 0\n"),
            ("proc/stat", "cpu  100 0 0 100 0 0 0 0 0 0\n"),
            ("proc/meminfo", "MemTotal: 1000 kB\nSwapTotal: 400 kB\n"),
            ("proc/vmstat", "pswpin 1\npswpout 2\n"),
            ("proc/net/dev", "h\nh\nenp8s0: 10 0 0 0 0 0 0 0 20 0\n"),
            ("sys/class/net/enp8s0/device/vendor", "0x10ec"),
            ("proc/42/stat", &format!("42 (a) {}\n", ["1"; 22].join(" "))),
        ]);
        let system = LinuxSystem {
            usage,
            ..LinuxSystem::at(root.path())
        };
        let page = system.page_size;
        let r = system.read();
        assert_eq!(r.disks.len(), 1);
        assert_eq!(r.cpu.first().map(|c| c.total), Some(200));
        let swap = r.swap.unwrap();
        assert_eq!(
            (swap.swapped_in, swap.swapped_out),
            (Some(page), Some(2 * page))
        );
        assert_eq!((r.network[0].received, r.network[0].sent), (10, 20));
        assert_eq!(r.processes[0].cpu, 2);
    }

    #[test]
    fn names_temperatures_as_people_know_them() {
        let root = tree(&[
            ("sys/class/hwmon/hwmon0/name", "nvme\n"),
            ("sys/class/hwmon/hwmon0/temp1_input", "41850\n"),
            ("sys/class/hwmon/hwmon0/temp1_label", "Composite\n"),
            ("sys/class/hwmon/hwmon0/temp2_input", "50000\n"),
            ("sys/class/hwmon/hwmon0/temp2_label", "Sensor 1\n"),
            ("sys/class/hwmon/hwmon1/name", "nvme\n"),
            ("sys/class/hwmon/hwmon1/temp1_input", "38000\n"),
            ("sys/class/hwmon/hwmon2/name", "amdgpu\n"),
            ("sys/class/hwmon/hwmon2/temp1_input", "45000\n"),
            ("sys/class/hwmon/hwmon10/name", "k10temp\n"),
            ("sys/class/hwmon/hwmon10/temp1_input", "62125\n"),
            ("sys/class/hwmon/hwmon10/temp1_label", "Tctl\n"),
            ("sys/class/hwmon/hwmon10/temp3_input", "55000\n"),
            ("sys/class/hwmon/hwmon10/temp3_label", "Tccd1\n"),
            ("sys/class/hwmon/hwmon3/name", "r8169_0_800:00\n"),
            ("sys/class/hwmon/hwmon3/temp1_input", "0\n"),
        ]);
        let r = LinuxSystem::at(root.path()).read();
        let names: Vec<_> = r
            .temperatures
            .iter()
            .map(|t| (t.label.as_str(), t.celsius, t.cpu))
            .collect();
        assert_eq!(
            names,
            [
                ("NVMe 1", 41.85, false),
                ("NVMe 2", 38.0, false),
                ("CPU", 62.125, true),
                ("CPU die 1", 55.0, false),
            ]
        );
    }

    #[test]
    fn reads_graphics_and_battery() {
        let root = tree(&[
            ("sys/class/drm/card1/device/gpu_busy_percent", "37\n"),
            ("sys/class/drm/card1/device/mem_info_vram_used", "1048576\n"),
            (
                "sys/class/drm/card1/device/mem_info_vram_total",
                "4194304\n",
            ),
            (
                "sys/class/drm/card1/device/hwmon/hwmon4/temp1_input",
                "51000\n",
            ),
            ("sys/class/drm/card1-DP-1/status", "connected\n"),
            ("sys/class/drm/card0/device/vendor", "0x10de\n"),
            ("sys/class/power_supply/hidpp_battery_0/type", "Battery\n"),
            ("sys/class/power_supply/hidpp_battery_0/scope", "Device\n"),
            ("sys/class/power_supply/hidpp_battery_0/capacity", "5\n"),
            ("sys/class/power_supply/macsmc-battery/type", "Battery\n"),
            ("sys/class/power_supply/macsmc-battery/capacity", "81\n"),
            ("sys/class/power_supply/macsmc-battery/status", "Charging\n"),
        ]);
        let r = LinuxSystem::at(root.path()).read();
        assert_eq!(
            r.gpus,
            [GpuReading {
                name: "AMD graphics".into(),
                busy: Some(37.0),
                memory_used: Some(1_048_576),
                memory_total: Some(4_194_304),
                celsius: Some(51.0),
            }]
        );
        assert_eq!(
            r.battery,
            Some(BatteryReading {
                percent: 81.0,
                charging: true
            })
        );
        assert_eq!(
            nvidia_gpus(
                "NVIDIA GeForce RTX 5070, 12, 2, 12227, 32\nOld card, [N/A], 10, 20, [N/A]\n"
            ),
            [
                GpuReading {
                    name: "NVIDIA GeForce RTX 5070".into(),
                    busy: Some(12.0),
                    memory_used: Some(2 * 1024 * 1024),
                    memory_total: Some(12227 * 1024 * 1024),
                    celsius: Some(32.0),
                },
                GpuReading {
                    name: "Old card".into(),
                    busy: None,
                    memory_used: Some(10 * 1024 * 1024),
                    memory_total: Some(20 * 1024 * 1024),
                    celsius: None,
                },
            ]
        );
    }

    #[test]
    fn reads_each_process_by_its_name_whatever_the_name_holds() {
        let root = tree(&[
            (
                "proc/42/stat",
                "42 (Web (Content) x) S 1 42 42 0 -1 4194560 100 0 0 0 250 50 0 0 20 0 30 0 5000 123456 300 18446744073709551615\n",
            ),
            (
                "proc/self/stat",
                "1 (self) S 0 0 0 0 0 0 0 0 0 0 1 1 0 0 0 0 0 0 0 0 1\n",
            ),
            ("proc/7/status", "Name: no stat\n"),
        ]);
        let system = LinuxSystem::at(root.path());
        assert_eq!(
            system.read().processes,
            [ProcessReading {
                pid: 42,
                name: "Web (Content) x".into(),
                cpu: 300,
                memory: 300 * system.page_size,
            }]
        );
    }

    #[test]
    fn lists_each_disk_once_by_where_it_is_mounted() {
        let root = tree(&[
            (
                "proc/self/mounts",
                "/dev/nvme0n1p2 / btrfs rw 0 0\nproc /proc proc rw 0 0\n/dev/nvme0n1p2 /home btrfs rw 0 0\n/dev/nvme0n1p1 /boot vfat rw 0 0\n/dev/sda1 /mnt/My\\040Disk ext4 rw 0 0\ntmpfs /tmp tmpfs rw 0 0\n",
            ),
            ("mnt/My Disk/.keep", ""),
            // There to be asked, were it not the system's.
            ("boot/.keep", ""),
        ]);
        let disks = LinuxSystem::at(root.path()).read().disks;
        let mounts: Vec<_> = disks.iter().map(|d| d.mount.as_str()).collect();
        assert_eq!(mounts, ["/", "/mnt/My Disk"]);
        assert!(disks.iter().all(|d| d.total > 0 && d.available <= d.total));
    }

    #[test]
    fn a_disk_that_does_not_answer_is_left_out_and_asked_once() {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};
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
        let root = tree(&[(
            "proc/self/mounts",
            "/dev/sda1 / ext4 rw 0 0\n/dev/sdb1 /mnt/stuck fuseblk rw 0 0\n",
        )]);
        let system = LinuxSystem {
            usage,
            ..LinuxSystem::at(root.path())
        };
        let mounts = || {
            let started = Instant::now();
            let disks = system.read().disks;
            assert!(started.elapsed() < Duration::from_secs(2));
            disks.into_iter().map(|d| d.mount).collect::<Vec<_>>()
        };
        assert_eq!(mounts(), ["/"]);
        assert_eq!(mounts(), ["/"]);
        assert_eq!(ASKED.load(SeqCst), 1);
        // Nor where it is found next, as a folder above it is renamed.
        std::fs::write(
            root.path().join("proc/self/mounts"),
            "/dev/sda1 / ext4 rw 0 0\n/dev/sdb1 /mnt/moved/stuck fuseblk rw 0 0\n",
        )
        .unwrap();
        assert_eq!(mounts(), ["/"]);
        assert_eq!(ASKED.load(SeqCst), 1);
        // Once it answers, it is asked again.
        STUCK.store(false, SeqCst);
        let deadline = Instant::now() + Duration::from_secs(5);
        while system.asking.unanswered() > 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(mounts(), ["/", "/mnt/moved/stuck"]);
        assert_eq!(ASKED.load(SeqCst), 2);
    }

    #[test]
    fn a_program_is_read_only_within_its_time() {
        let sh = |script: &str| {
            let mut command = Command::new("sh");
            command.args(["-c", script]);
            output_within(command, Duration::from_millis(500))
        };
        assert_eq!(
            sh("echo 'GPU, 5, 100, 200, 40'").as_deref(),
            Some("GPU, 5, 100, 200, 40\n")
        );
        assert_eq!(sh("echo half; exit 3"), None);
        assert_eq!(sh("head -c 70000 /dev/zero"), None);
        // Silent, or ended but leaving its output held open by another.
        for script in ["sleep 5", "echo 'GPU, 5'; sleep 5 &", "exec 1>&-; sleep 5"] {
            let started = Instant::now();
            assert_eq!(sh(script), None, "{script}");
            assert!(started.elapsed() < Duration::from_secs(2), "{script}");
        }
    }
}
