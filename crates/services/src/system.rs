// SPDX-License-Identifier: Apache-2.0
//! Control's System section: how busy the computer is. The computer is read
//! only while someone asks, and the last few minutes of asking are kept for
//! the graphs.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ports::Clock;
use ports::system::{CpuTime, SystemMonitor, SystemReading};
use protocol::system::{
    Battery, CpuStats, DiskStats, GpuStats, InterfaceStats, MemoryStats, NetworkStats,
    ProcessStats, SYSTEM_HISTORY, SwapStats, SystemSample, SystemStats, Temperature,
};
use protocol::time::Timestamp;
use tokio::sync::Mutex;

/// Asked again within this, the last answer stands: several devices looking
/// at once read the computer once.
const FRESH: Duration = Duration::from_millis(1_000);
/// Longer than this without anyone asking, the history starts again.
const GAP: Duration = Duration::from_secs(30);
/// The first answer compares two readings this far apart.
const FIRST_WAIT: Duration = Duration::from_millis(250);
/// The busiest processes by processor, and by memory, each.
const PROCESSES: usize = 6;

pub(crate) struct SystemSampler {
    monitor: Arc<dyn SystemMonitor>,
    state: Mutex<Sampled>,
}

#[derive(Default)]
struct Sampled {
    last: Option<(Instant, SystemReading)>,
    stats: Option<SystemStats>,
    history: VecDeque<SystemSample>,
}

impl SystemSampler {
    pub(crate) fn new(monitor: Arc<dyn SystemMonitor>) -> Self {
        Self {
            monitor,
            state: Mutex::default(),
        }
    }

    pub(crate) async fn stats(&self, clock: &dyn Clock) -> SystemStats {
        let mut s = self.state.lock().await;
        let now = clock.monotonic();
        if let (Some(stats), Some((at, _))) = (&s.stats, &s.last)
            && now.saturating_duration_since(*at) < FRESH
        {
            return stats.clone();
        }
        let (before_at, before) = match s.last.take() {
            Some((at, reading)) if now.saturating_duration_since(at) < GAP => (at, reading),
            _ => {
                s.history.clear();
                let first = self.read().await;
                tokio::time::sleep(FIRST_WAIT).await;
                (now, first)
            }
        };
        let at = clock.monotonic();
        let reading = self.read().await;
        let seconds = at.saturating_duration_since(before_at).as_secs_f64();
        let mut stats = compare(&before, &reading, seconds, clock.now());
        s.history.push_back(sample(&stats));
        while s.history.len() > SYSTEM_HISTORY {
            s.history.pop_front();
        }
        stats.history = s.history.iter().cloned().collect();
        s.last = Some((at, reading));
        s.stats = Some(stats.clone());
        stats
    }

    async fn read(&self) -> SystemReading {
        let monitor = self.monitor.clone();
        tokio::task::spawn_blocking(move || monitor.read())
            .await
            .unwrap_or_default()
    }
}

/// What changed between two readings `seconds` apart. Rates need time
/// between them; without it they are left out.
fn compare(
    before: &SystemReading,
    after: &SystemReading,
    seconds: f64,
    at: Timestamp,
) -> SystemStats {
    let rate =
        |from: u64, to: u64| (seconds > 0.0).then(|| to.saturating_sub(from) as f64 / seconds);
    let all = |r: &SystemReading| r.cpu.first().copied().unwrap_or_default();
    let elapsed = all(after).total.saturating_sub(all(before).total);
    let previous: HashMap<_, _> = before.processes.iter().map(|p| (p.pid, p)).collect();
    let mut processes: Vec<ProcessStats> = after
        .processes
        .iter()
        .map(|p| ProcessStats {
            pid: p.pid,
            name: p.name.clone(),
            // A process id used again by another program starts from nothing.
            cpu: previous
                .get(&p.pid)
                .filter(|was| was.name == p.name && elapsed > 0)
                .map_or(0.0, |was| {
                    (p.cpu.saturating_sub(was.cpu) as f64 / elapsed as f64 * 100.0).min(100.0)
                        as f32
                }),
            memory: p.memory,
        })
        .collect();
    processes.sort_by(|a, b| b.cpu.total_cmp(&a.cpu).then(b.memory.cmp(&a.memory)));
    let mut busiest: Vec<ProcessStats> = processes.iter().take(PROCESSES).cloned().collect();
    processes.sort_by_key(|p| std::cmp::Reverse(p.memory));
    for p in processes.into_iter().take(PROCESSES) {
        if !busiest.iter().any(|b| b.pid == p.pid) {
            busiest.push(p);
        }
    }
    let mut temperatures = after.temperatures.clone();
    temperatures.sort_by_key(|t| !t.cpu);
    SystemStats {
        at,
        cpu: (!after.cpu.is_empty()).then(|| CpuStats {
            usage: usage(all(before), all(after)),
            cores: after
                .cpu
                .iter()
                .skip(1)
                .enumerate()
                .map(|(i, now)| usage(before.cpu.get(i + 1).copied().unwrap_or_default(), *now))
                .collect(),
            model: after.cpu_model.clone(),
        }),
        memory: after.memory.map(|m| MemoryStats {
            total: m.total,
            used: m.total.saturating_sub(m.available),
            cached: m.available.saturating_sub(m.free),
        }),
        swap: after.swap.map(|now| {
            let was = before.swap.unwrap_or_default();
            SwapStats {
                total: now.total,
                used: now.total.saturating_sub(now.free),
                in_per_s: was
                    .swapped_in
                    .zip(now.swapped_in)
                    .and_then(|(a, b)| rate(a, b)),
                out_per_s: was
                    .swapped_out
                    .zip(now.swapped_out)
                    .and_then(|(a, b)| rate(a, b)),
            }
        }),
        load: after.load.map(Vec::from),
        uptime_s: after.uptime_s,
        disks: after
            .disks
            .iter()
            .map(|d| DiskStats {
                mount: d.mount.clone(),
                total: d.total,
                used: d.total.saturating_sub(d.available),
            })
            .collect(),
        network: (seconds > 0.0 && !after.network.is_empty()).then(|| {
            let interfaces: Vec<InterfaceStats> = after
                .network
                .iter()
                .filter_map(|now| {
                    let was = before.network.iter().find(|w| w.name == now.name)?;
                    Some(InterfaceStats {
                        name: now.name.clone(),
                        received_per_s: rate(was.received, now.received)?,
                        sent_per_s: rate(was.sent, now.sent)?,
                    })
                })
                .collect();
            NetworkStats {
                received_per_s: interfaces.iter().map(|i| i.received_per_s).sum(),
                sent_per_s: interfaces.iter().map(|i| i.sent_per_s).sum(),
                interfaces,
            }
        }),
        temperatures: temperatures
            .into_iter()
            .map(|t| Temperature {
                label: t.label,
                celsius: t.celsius,
            })
            .collect(),
        gpus: after
            .gpus
            .iter()
            .map(|g| GpuStats {
                name: g.name.clone(),
                usage: g.busy,
                memory_used: g.memory_used,
                memory_total: g.memory_total,
                celsius: g.celsius,
            })
            .collect(),
        battery: after.battery.map(|b| Battery {
            percent: b.percent,
            charging: b.charging,
        }),
        processes: busiest,
        history: Vec::new(),
    }
}

fn usage(before: CpuTime, after: CpuTime) -> f32 {
    let total = after.total.saturating_sub(before.total);
    if total == 0 {
        return 0.0;
    }
    (after.busy.saturating_sub(before.busy) as f64 / total as f64 * 100.0).clamp(0.0, 100.0) as f32
}

fn sample(stats: &SystemStats) -> SystemSample {
    let percent =
        |part: u64, whole: u64| (whole > 0).then(|| (part as f64 / whole as f64 * 100.0) as f32);
    SystemSample {
        at: stats.at,
        cpu: stats.cpu.as_ref().map(|c| c.usage),
        memory: stats.memory.as_ref().and_then(|m| percent(m.used, m.total)),
        swap: stats.swap.as_ref().and_then(|s| percent(s.used, s.total)),
        received_per_s: stats.network.as_ref().map(|n| n.received_per_s),
        sent_per_s: stats.network.as_ref().map(|n| n.sent_per_s),
    }
}

#[cfg(test)]
#[path = "system_tests.rs"]
mod tests;
