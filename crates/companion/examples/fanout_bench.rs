// SPDX-License-Identifier: Apache-2.0
//! A long agent run fanned out to several connected phones over real TLS.
//!
//! `cargo run --release -p companion --example fanout_bench -- [phones] [activities]`
//!
//! Synthetic ports only: no native tool, desktop or outside network. The
//! companion and the phones run on separately named threads, so the CPU the
//! companion spends per agent event is reported apart from the phones' own.
//! The agent stays a few events ahead of the slowest phone, so the numbers
//! describe fan-out rather than recovery from an overflowing phone queue.
mod common;

use common::{frame, text, until};
use companion::{Server, ServerDeps, identity::Identity};
use futures_util::{SinkExt, StreamExt};
use protocol::*;
use services::{Hub, testing::FakeHarness};
use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tokio_tungstenite::tungstenite::Message;

/// User plus system CPU, in seconds, of this process's threads named `name`.
fn thread_cpu(name: &str) -> f64 {
    std::fs::read_dir("/proc/self/task")
        .unwrap()
        .flatten()
        .filter(|task| {
            std::fs::read_to_string(task.path().join("comm")).is_ok_and(|c| c.trim_end() == name)
        })
        .filter_map(|task| std::fs::read_to_string(task.path().join("stat")).ok())
        .map(|stat| {
            let fields: Vec<&str> = stat.rsplit_once(") ").unwrap().1.split(' ').collect();
            (fields[11].parse::<f64>().unwrap() + fields[12].parse::<f64>().unwrap()) / 100.0
        })
        .sum()
}

/// Agent activities or text deltas emitted ahead of the slowest phone.
const AHEAD: u64 = 8;

/// What each phone has received so far.
#[derive(Default)]
struct Phone {
    events: AtomicU64,
    live: AtomicU64,
}
fn events(phone: &Phone) -> &AtomicU64 {
    &phone.events
}
fn live(phone: &Phone) -> &AtomicU64 {
    &phone.live
}
#[derive(Default)]
struct Received {
    phones: Vec<Phone>,
    caught_up: AtomicU64,
    event_bytes: AtomicU64,
    resubscribed: AtomicU64,
    progress: tokio::sync::Notify,
}
impl Received {
    fn least(&self, count: fn(&Phone) -> &AtomicU64) -> u64 {
        self.phones
            .iter()
            .map(|phone| count(phone).load(Ordering::Relaxed))
            .min()
            .unwrap_or(0)
    }
    /// Waits until every phone's `count` reaches `target`.
    async fn reach(&self, count: fn(&Phone) -> &AtomicU64, target: u64) {
        while self.least(count) < target {
            self.progress.notified().await;
        }
    }
}

fn main() {
    let args: Vec<u64> = std::env::args()
        .skip(1)
        .map(|a| a.parse().expect("numeric argument"))
        .collect();
    let phones = args.first().copied().unwrap_or(4);
    let activities = args.get(1).copied().unwrap_or(2000);
    let runtime = |name: &str| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name(name)
            .enable_all()
            .build()
            .unwrap()
    };
    let (companion, devices) = (runtime("companion"), runtime("phones"));
    companion.block_on(bench(devices.handle().clone(), phones, activities));
}

async fn bench(devices: tokio::runtime::Handle, phones: u64, activities: u64) {
    let path = std::env::temp_dir().join(format!("fanout-bench-{}", std::process::id()));
    let harness = Arc::new(FakeHarness::default());
    let (hub, run) = common::hub_with_run(&path, &harness).await;
    let mut paired = Vec::new();
    for seed in 0..phones {
        paired.push(common::pair(&hub, 10 + seed as u8).await);
    }
    let hub = Arc::new(hub);
    let identity = Arc::new(Identity::generate(hub.host_id(), vec!["localhost".into()]).unwrap());
    let pin = identity.fingerprint();
    let deps = ServerDeps::new(hub.clone(), identity).unwrap();
    let server = Server::bind(deps, &["127.0.0.1:0".parse().unwrap()])
        .await
        .unwrap();
    let address = server.local_addresses().unwrap()[0];
    let stop = tokio_util::sync::CancellationToken::new();
    let serving = tokio::spawn(server.run(stop.clone()));

    let received = Arc::new(Received {
        phones: (0..phones).map(|_| Phone::default()).collect(),
        ..Received::default()
    });
    let head = hub.journal_head();
    let mut readers = Vec::new();
    for (index, (key, device_id)) in paired.into_iter().enumerate() {
        let (pin, received) = (pin.clone(), received.clone());
        readers.push(devices.spawn(async move {
            let mut ws = common::connect(address, &pin, &key, device_id, None).await;
            ws.send(text(&ClientFrame::Subscribe(Subscribe {
                after_seq: Some(head),
            })))
            .await
            .unwrap();
            let mut applied = head;
            while let Some(Ok(message)) = ws.next().await {
                let Message::Text(body) = &message else {
                    continue;
                };
                if body.starts_with(r#"{"type":"event""#) {
                    // Only the sequence is read, as far as it goes.
                    let seq = body
                        .split_once(r#""seq":"#)
                        .and_then(|(_, rest)| rest.split(|c: char| !c.is_ascii_digit()).next())
                        .and_then(|digits| digits.parse().ok())
                        .unwrap_or(applied);
                    applied = applied.max(seq);
                    received.phones[index]
                        .events
                        .fetch_add(1, Ordering::Relaxed);
                    received
                        .event_bytes
                        .fetch_add(body.len() as u64, Ordering::Relaxed);
                } else if body.starts_with(r#"{"type":"live""#) {
                    received.phones[index].live.fetch_add(1, Ordering::Relaxed);
                } else if body.starts_with(r#"{"type":"caught_up""#) {
                    received.caught_up.fetch_add(1, Ordering::Relaxed);
                } else if let ServerFrame::SubscriptionClosed { .. } = frame(message) {
                    received.resubscribed.fetch_add(1, Ordering::Relaxed);
                    ws.send(text(&ClientFrame::Subscribe(Subscribe {
                        after_seq: Some(applied),
                    })))
                    .await
                    .unwrap();
                }
                received.progress.notify_one();
            }
        }));
    }
    until(|| received.caught_up.load(Ordering::Relaxed) == phones).await;
    // Let connection bookkeeping (last seen, device list) settle first.
    tokio::time::sleep(Duration::from_millis(300)).await;

    println!(
        "workload,phones,count,wall per op,companion cpu per op,phones cpu per op,bytes per event,resubscriptions"
    );
    let measure = |name: &str, count: u64, started: (Instant, f64, f64), bytes: f64| {
        let per = |value: f64| value * 1e6 / count as f64;
        println!(
            "{name},{phones},{count},{:.1} us,{:.1} us,{:.1} us,{bytes:.0},{}",
            per(started.0.elapsed().as_secs_f64()),
            per(thread_cpu("companion") - started.1),
            per(thread_cpu("phones") - started.2),
            received.resubscribed.load(Ordering::Relaxed),
        );
    };
    let now = || {
        (
            Instant::now(),
            thread_cpu("companion"),
            thread_cpu("phones"),
        )
    };

    // Each agent activity journals three events that every phone receives.
    let (before, bytes) = (
        received.least(events),
        received.event_bytes.load(Ordering::Relaxed),
    );
    let started = now();
    for i in 0..activities {
        let behind = (i + 1).saturating_sub(AHEAD);
        received.reach(events, before + 3 * behind).await;
        harness.emit(run, common::activity(i)).await;
    }
    received.reach(events, before + 3 * activities).await;
    let average = (received.event_bytes.load(Ordering::Relaxed) - bytes) as f64
        / (3 * activities * phones) as f64;
    measure("activity_events", activities, started, average);

    // Streamed text is live only and reaches every phone.
    let before = received.least(live);
    let started = now();
    for i in 0..activities {
        let behind = (i + 1).saturating_sub(AHEAD);
        received.reach(live, before + behind).await;
        harness
            .emit(
                run,
                ports::HarnessEvent::AgentTextDelta {
                    message_key: "m1".into(),
                    text: format!("token {i} "),
                },
            )
            .await;
    }
    received.reach(live, before + activities).await;
    measure("text_deltas", activities, started, 0.0);

    stop.cancel();
    let _ = serving.await;
    for reader in readers {
        reader.abort();
    }
    hub.shutdown().await.unwrap();
    drop(hub);
    std::fs::remove_dir_all(path).unwrap();
}
