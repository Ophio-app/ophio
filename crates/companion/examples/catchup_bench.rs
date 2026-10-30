// SPDX-License-Identifier: Apache-2.0
//! A phone on a slow link catches up on a long history over real TLS.
//!
//! `cargo run --release -p companion --example catchup_bench -- [activities] [bytes_per_second]`
//!
//! Synthetic ports only: no native tool, desktop or outside network. Reports
//! resubscriptions, wall and process CPU time, and how long other commands
//! waited while the backlog was being served.
mod common;

use common::{cpu, frame, peak_mb, text, until};
use companion::{Server, ServerDeps, identity::Identity};
use futures_util::{SinkExt, StreamExt};
use protocol::*;
use services::{Hub, api::Caller, testing::FakeHarness};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

#[tokio::main]
async fn main() {
    let args: Vec<u64> = std::env::args()
        .skip(1)
        .map(|a| a.parse().expect("numeric argument"))
        .collect();
    let activities = args.first().copied().unwrap_or(10_000);
    let rate = args.get(1).copied().unwrap_or(1_000_000) as f64;
    let path = std::env::temp_dir().join(format!("catchup-bench-{}", std::process::id()));
    let harness = Arc::new(FakeHarness::default());
    let (hub, run) = common::hub_with_run(&path, &harness).await;
    let head = hub.journal_head();
    for i in 0..activities {
        harness.emit(run, common::activity(i)).await;
    }
    until(|| hub.journal_head() >= head + 3 * activities).await;

    let (key, device_id) = common::pair(&hub, 7).await;
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
    let mut ws = common::connect(address, &pin, &key, device_id, Some(64 * 1024)).await;

    // Meanwhile the person keeps using the computer from another device.
    let busy = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let probe = {
        let (hub, busy) = (hub.clone(), busy.clone());
        tokio::spawn(async move {
            let mut waits = Vec::new();
            while busy.load(std::sync::atomic::Ordering::Relaxed) {
                let started = Instant::now();
                hub.query(&Caller::local_owner(), Query::Harnesses)
                    .await
                    .unwrap();
                waits.push(started.elapsed());
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            waits
        })
    };
    let peak_before = peak_mb();
    let (started, cpu_started) = (Instant::now(), cpu());
    let (mut applied, mut events, mut closed, mut bytes) = (0u64, 0u64, 0u64, 0f64);
    ws.send(text(&ClientFrame::Subscribe(Subscribe {
        after_seq: Some(0),
    })))
    .await
    .unwrap();
    loop {
        let message = ws.next().await.unwrap().unwrap();
        bytes += message.len() as f64;
        let due = Duration::from_secs_f64(bytes / rate);
        if let Some(ahead) = due.checked_sub(started.elapsed())
            && ahead > Duration::from_millis(2)
        {
            tokio::time::sleep(ahead).await;
        }
        match frame(message) {
            ServerFrame::Event(event) => {
                events += 1;
                applied = event.seq;
            }
            ServerFrame::SubscriptionClosed { after_seq, .. } => {
                closed += 1;
                ws.send(text(&ClientFrame::Subscribe(Subscribe {
                    after_seq: Some(after_seq.min(applied)),
                })))
                .await
                .unwrap();
            }
            ServerFrame::CaughtUp { .. } => break,
            _ => {}
        }
    }
    let (elapsed, cpu_used) = (started.elapsed(), cpu() - cpu_started);
    busy.store(false, std::sync::atomic::Ordering::Relaxed);
    let mut waits = probe.await.unwrap();
    waits.sort();
    println!(
        "events,{events},resubscriptions,{closed},megabytes,{:.1},wall,{:.2} s,cpu,{:.2} s,peak rss growth,{:.0} MB,other query wait p50,{:.2} ms,max,{:.2} ms",
        bytes / 1e6,
        elapsed.as_secs_f64(),
        cpu_used,
        peak_mb() - peak_before,
        waits[waits.len() / 2].as_secs_f64() * 1e3,
        waits.last().unwrap().as_secs_f64() * 1e3,
    );
    drop(ws);
    stop.cancel();
    let _ = serving.await;
    hub.shutdown().await.unwrap();
    drop(hub);
    std::fs::remove_dir_all(path).unwrap();
}
