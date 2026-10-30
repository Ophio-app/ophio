// SPDX-License-Identifier: Apache-2.0
use super::*;
use crate::testing::FakeClock;
use ports::system::{
    DiskReading, InterfaceReading, MemoryReading, ProcessReading, SwapReading, TemperatureReading,
};
use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicUsize, Ordering};

const GIB: u64 = 1 << 30;

fn process(pid: u32, name: &str, cpu: u64, memory: u64) -> ProcessReading {
    ProcessReading {
        pid,
        name: name.into(),
        cpu,
        memory,
    }
}

fn interface(name: &str, received: u64, sent: u64) -> InterfaceReading {
    InterfaceReading {
        name: name.into(),
        received,
        sent,
    }
}

fn temperature(label: &str, celsius: f32, cpu: bool) -> TemperatureReading {
    TemperatureReading {
        label: label.into(),
        celsius,
        cpu,
    }
}

/// Two processors, 16 GiB of memory and 4 GiB of swap.
fn reading(busy: [u64; 2], total: [u64; 2]) -> SystemReading {
    SystemReading {
        cpu: vec![
            CpuTime {
                busy: busy[0] + busy[1],
                total: total[0] + total[1],
            },
            CpuTime {
                busy: busy[0],
                total: total[0],
            },
            CpuTime {
                busy: busy[1],
                total: total[1],
            },
        ],
        cpu_model: Some("Synthetic 2-core".into()),
        memory: Some(MemoryReading {
            total: 16 * GIB,
            available: 10 * GIB,
            free: 4 * GIB,
        }),
        swap: Some(SwapReading {
            total: 4 * GIB,
            free: 3 * GIB,
            swapped_in: Some(0),
            swapped_out: Some(0),
        }),
        load: Some([0.5, 0.25, 0.125]),
        uptime_s: Some(3_600),
        disks: vec![DiskReading {
            mount: "/".into(),
            total: 100 * GIB,
            available: 40 * GIB,
        }],
        network: vec![interface("eth0", 0, 0)],
        ..SystemReading::default()
    }
}

#[test]
fn shares_out_the_processors_and_memory_between_two_readings() {
    let before = reading([0, 0], [0, 0]);
    let after = reading([50, 25], [100, 100]);
    let s = compare(&before, &after, 2.0, Timestamp(7));
    assert_eq!(s.at, Timestamp(7));
    let cpu = s.cpu.unwrap();
    assert_eq!(cpu.usage, 37.5);
    assert_eq!(cpu.cores, vec![50.0, 25.0]);
    assert_eq!(cpu.model.as_deref(), Some("Synthetic 2-core"));
    let memory = s.memory.unwrap();
    assert_eq!(
        (memory.total, memory.used, memory.cached),
        (16 * GIB, 6 * GIB, 6 * GIB)
    );
    assert_eq!(
        memory.total - memory.used - memory.cached,
        4 * GIB,
        "the rest is free"
    );
    assert_eq!(s.load, Some(vec![0.5, 0.25, 0.125]));
    assert_eq!(s.uptime_s, Some(3_600));
    assert_eq!(s.disks[0].mount, "/");
    assert_eq!((s.disks[0].total, s.disks[0].used), (100 * GIB, 60 * GIB));
}

#[test]
fn a_computer_that_did_nothing_is_idle_not_undefined() {
    let same = reading([10, 10], [100, 100]);
    let cpu = compare(&same, &same, 2.0, Timestamp(0)).cpu.unwrap();
    assert_eq!((cpu.usage, cpu.cores), (0.0, vec![0.0, 0.0]));
    // A counter that went backwards, as after a reset, isn't negative.
    let after = reading([0, 0], [200, 200]);
    assert_eq!(
        compare(&same, &after, 2.0, Timestamp(0)).cpu.unwrap().usage,
        0.0
    );
    // Busy counted ahead of the whole, as rounding can, is still at most all of it.
    let ahead = reading([160, 160], [200, 200]);
    let cpu = compare(&same, &ahead, 2.0, Timestamp(0)).cpu.unwrap();
    assert_eq!((cpu.usage, cpu.cores), (100.0, vec![100.0, 100.0]));
    // Nothing to read, nothing shown.
    assert!(
        compare(
            &SystemReading::default(),
            &SystemReading::default(),
            2.0,
            Timestamp(0)
        )
        .cpu
        .is_none()
    );
}

#[test]
fn moves_through_swap_and_network_as_rates_over_the_time_between() {
    let mut before = reading([0, 0], [0, 0]);
    before.network = vec![interface("eth0", 1_000, 500), interface("wlan0", 0, 0)];
    let mut after = reading([0, 0], [100, 100]);
    after.swap = Some(SwapReading {
        total: 4 * GIB,
        free: GIB,
        swapped_in: Some(8_000),
        swapped_out: Some(2_000),
    });
    after.network = vec![
        interface("eth0", 5_000, 2_500),
        interface("wlan0", 400, 0),
        // Not there the first time: no rate yet.
        interface("usb0", 9_000, 9_000),
    ];
    let s = compare(&before, &after, 4.0, Timestamp(0));
    let swap = s.swap.unwrap();
    assert_eq!((swap.total, swap.used), (4 * GIB, 3 * GIB));
    assert_eq!(
        (swap.in_per_s, swap.out_per_s),
        (Some(2_000.0), Some(500.0))
    );
    let network = s.network.unwrap();
    assert_eq!(
        network
            .interfaces
            .iter()
            .map(|i| (i.name.as_str(), i.received_per_s, i.sent_per_s))
            .collect::<Vec<_>>(),
        vec![("eth0", 1_000.0, 500.0), ("wlan0", 100.0, 0.0)]
    );
    assert_eq!(
        (network.received_per_s, network.sent_per_s),
        (1_100.0, 500.0)
    );

    // Without time between, there is no rate.
    let s = compare(&before, &after, 0.0, Timestamp(0));
    assert!(s.network.is_none());
    let swap = s.swap.unwrap();
    assert_eq!(
        (swap.used, swap.in_per_s, swap.out_per_s),
        (3 * GIB, None, None)
    );
    // A system that doesn't count swapping has no swap rates.
    after.swap = Some(SwapReading {
        total: 4 * GIB,
        free: GIB,
        swapped_in: None,
        swapped_out: None,
    });
    let swap = compare(&before, &after, 4.0, Timestamp(0)).swap.unwrap();
    assert_eq!((swap.in_per_s, swap.out_per_s), (None, None));
    // No interfaces, no network.
    after.network.clear();
    assert!(
        compare(&before, &after, 4.0, Timestamp(0))
            .network
            .is_none()
    );
}

#[test]
fn a_process_id_used_again_by_another_program_starts_from_nothing() {
    let mut before = reading([0, 0], [0, 0]);
    before.processes = vec![
        process(10, "builder", 0, GIB),
        process(11, "editor", 0, GIB),
    ];
    let mut after = reading([0, 0], [100, 100]);
    after.processes = vec![
        process(10, "builder", 150, GIB),
        process(11, "player", 40, GIB),
        // New since the last reading: nothing to compare with.
        process(12, "fresh", 90, GIB),
        // Capped at the whole computer.
        process(13, "runaway", 900, GIB),
    ];
    before.processes.push(process(13, "runaway", 0, GIB));
    let s = compare(&before, &after, 2.0, Timestamp(0));
    let cpu: HashMap<_, _> = s
        .processes
        .iter()
        .map(|p| (p.name.as_str(), p.cpu))
        .collect();
    assert_eq!(cpu["builder"], 75.0);
    assert_eq!(cpu["player"], 0.0);
    assert_eq!(cpu["fresh"], 0.0);
    assert_eq!(cpu["runaway"], 100.0);
}

#[test]
fn lists_the_busiest_by_processor_and_the_largest_by_memory() {
    let mut before = reading([0, 0], [0, 0]);
    let mut after = reading([0, 0], [1_000, 1_000]);
    // Eight busy and small, eight idle and large.
    for i in 0..8u32 {
        before
            .processes
            .push(process(i, &format!("busy{i}"), 0, u64::from(i)));
        after.processes.push(process(
            i,
            &format!("busy{i}"),
            u64::from(i + 1) * 10,
            u64::from(i),
        ));
        before.processes.push(process(
            100 + i,
            &format!("large{i}"),
            0,
            GIB * u64::from(i + 1),
        ));
        after.processes.push(process(
            100 + i,
            &format!("large{i}"),
            0,
            GIB * u64::from(i + 1),
        ));
    }
    let s = compare(&before, &after, 2.0, Timestamp(0));
    let names: Vec<_> = s.processes.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(
        names,
        vec![
            "busy7", "busy6", "busy5", "busy4", "busy3", "busy2", "large7", "large6", "large5",
            "large4", "large3", "large2"
        ]
    );
    assert_eq!(s.processes[0].cpu, 4.0);

    // One that is both busy and large is listed once.
    after.processes[15].cpu = 10_000;
    let s = compare(&before, &after, 2.0, Timestamp(0));
    let names: Vec<_> = s.processes.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names.iter().filter(|n| **n == "large7").count(), 1);
    assert_eq!(names[0], "large7");
    assert_eq!(names.len(), 11);
    // Equal processor time: the larger first.
    let mut tie = reading([0, 0], [0, 0]);
    tie.processes = vec![process(1, "small", 0, 1), process(2, "big", 0, 2)];
    let s = compare(&tie, &tie, 2.0, Timestamp(0));
    assert_eq!(s.processes[0].name, "big");
}

#[test]
fn the_processor_temperature_comes_first() {
    let mut after = reading([0, 0], [0, 0]);
    after.temperatures = vec![
        temperature("NVMe", 41.0, false),
        temperature("CPU", 55.0, true),
        temperature("Memory", 38.0, false),
    ];
    let s = compare(&SystemReading::default(), &after, 2.0, Timestamp(0));
    let labels: Vec<_> = s
        .temperatures
        .iter()
        .map(|t| (t.label.as_str(), t.celsius))
        .collect();
    assert_eq!(
        labels,
        vec![("CPU", 55.0), ("NVMe", 41.0), ("Memory", 38.0)]
    );
}

#[test]
fn passes_graphics_and_battery_through() {
    let mut after = reading([0, 0], [0, 0]);
    after.gpus = vec![ports::system::GpuReading {
        name: "Synthetic GPU".into(),
        busy: Some(12.0),
        memory_used: Some(GIB),
        memory_total: Some(8 * GIB),
        celsius: None,
    }];
    after.battery = Some(ports::system::BatteryReading {
        percent: 81.0,
        charging: true,
    });
    let s = compare(&after, &after, 2.0, Timestamp(0));
    assert_eq!(
        s.gpus,
        vec![GpuStats {
            name: "Synthetic GPU".into(),
            usage: Some(12.0),
            memory_used: Some(GIB),
            memory_total: Some(8 * GIB),
            celsius: None,
        }]
    );
    assert_eq!(
        s.battery,
        Some(Battery {
            percent: 81.0,
            charging: true
        })
    );
}

#[test]
fn a_sample_holds_the_shares_the_graphs_draw() {
    let mut after = reading([30, 30], [100, 100]);
    after.network = vec![interface("eth0", 200, 100)];
    let s = compare(&reading([0, 0], [0, 0]), &after, 2.0, Timestamp(9));
    let sample = sample(&s);
    assert_eq!(sample.at, Timestamp(9));
    assert_eq!(sample.cpu, Some(30.0));
    assert_eq!(sample.memory, Some(37.5));
    assert_eq!(sample.swap, Some(25.0));
    assert_eq!(
        (sample.received_per_s, sample.sent_per_s),
        (Some(100.0), Some(50.0))
    );
    // Nothing to share out of, nothing drawn.
    let empty = SystemReading {
        swap: Some(SwapReading::default()),
        ..SystemReading::default()
    };
    let sample = super::sample(&compare(&empty, &empty, 2.0, Timestamp(0)));
    assert_eq!(
        (
            sample.cpu,
            sample.memory,
            sample.swap,
            sample.received_per_s
        ),
        (None, None, None, None)
    );
}

/// Hands out readings in turn, the last one again once they run out, and
/// counts how often it was read.
#[derive(Debug, Default)]
struct Scripted {
    readings: StdMutex<VecDeque<SystemReading>>,
    reads: AtomicUsize,
}
impl Scripted {
    fn with(readings: impl IntoIterator<Item = SystemReading>) -> Arc<Self> {
        Arc::new(Self {
            readings: StdMutex::new(readings.into_iter().collect()),
            reads: AtomicUsize::new(0),
        })
    }
    fn reads(&self) -> usize {
        self.reads.load(Ordering::SeqCst)
    }
}
impl SystemMonitor for Scripted {
    fn read(&self) -> SystemReading {
        self.reads.fetch_add(1, Ordering::SeqCst);
        let mut readings = self.readings.lock().unwrap();
        if readings.len() > 1 {
            readings.pop_front().unwrap()
        } else {
            readings.front().cloned().unwrap_or_default()
        }
    }
}

/// The processors kept getting busier: each reading 100 more of each, half
/// of it busy.
fn ticking(n: u64) -> impl Iterator<Item = SystemReading> {
    (0..n).map(|i| reading([i * 50, i * 50], [i * 100, i * 100]))
}

#[tokio::test]
async fn the_first_answer_reads_twice_and_later_ones_once() {
    let monitor = Scripted::with(ticking(10));
    let sampler = SystemSampler::new(monitor.clone());
    let clock = FakeClock::default();
    let first = sampler.stats(&clock).await;
    assert_eq!(monitor.reads(), 2);
    assert_eq!(first.cpu.unwrap().usage, 50.0);
    assert_eq!(first.history.len(), 1);
    clock.advance(Duration::from_secs(2));
    let second = sampler.stats(&clock).await;
    assert_eq!(monitor.reads(), 3);
    assert_eq!(second.history.len(), 2);
    assert_eq!(second.at, clock.now());
    // Two seconds between, so the network had time to move.
    assert_eq!(second.network.unwrap().received_per_s, 0.0);
}

#[tokio::test]
async fn asked_again_at_once_gives_the_same_answer_without_reading() {
    let monitor = Scripted::with(ticking(10));
    let sampler = SystemSampler::new(monitor.clone());
    let clock = FakeClock::default();
    let first = sampler.stats(&clock).await;
    clock.advance(FRESH - Duration::from_millis(1));
    assert_eq!(sampler.stats(&clock).await, first);
    assert_eq!(monitor.reads(), 2);
    clock.advance(Duration::from_millis(1));
    assert_ne!(sampler.stats(&clock).await, first);
    assert_eq!(monitor.reads(), 3);
}

#[tokio::test]
async fn keeps_only_the_last_few_minutes_and_starts_again_after_a_gap() {
    let monitor = Scripted::with(ticking(200));
    let sampler = SystemSampler::new(monitor.clone());
    let clock = FakeClock::default();
    let mut last = sampler.stats(&clock).await;
    for _ in 0..SYSTEM_HISTORY + 5 {
        clock.advance(Duration::from_secs(2));
        last = sampler.stats(&clock).await;
    }
    assert_eq!(last.history.len(), SYSTEM_HISTORY);
    assert_eq!(last.history.last().unwrap().at, last.at);
    assert!(
        last.history.windows(2).all(|w| w[0].at < w[1].at),
        "oldest first"
    );
    let reads = monitor.reads();
    // Just under the gap, the history goes on.
    clock.advance(GAP - Duration::from_millis(1));
    let kept = sampler.stats(&clock).await;
    assert_eq!(kept.history.len(), SYSTEM_HISTORY);
    assert_eq!(monitor.reads(), reads + 1);
    // Longer without anyone asking: it starts again, from two fresh readings.
    clock.advance(GAP);
    let restarted = sampler.stats(&clock).await;
    assert_eq!(restarted.history.len(), 1);
    assert_eq!(monitor.reads(), reads + 3);
}
