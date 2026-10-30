// SPDX-License-Identifier: Apache-2.0
//! Per-commit cost as the retained journal grows, with the default retention.
//!
//! `cargo run --release -p store --example store_bench`
use protocol::{
    Event,
    model::{ActivityStatus, TimelineBody, TimelineItem},
    time::Timestamp,
};
use std::time::Instant;
use store::{Retention, Store};

fn activity(task_id: protocol::TaskId, i: u64) -> Event {
    Event::TimelineAppended {
        task_id,
        item: TimelineItem {
            seq: 0,
            at: Timestamp(0),
            run_id: None,
            item: TimelineBody::Activity {
                tool: "Bash".into(),
                summary: format!("Ran step {i} of the build and tests"),
                status: ActivityStatus::Succeeded,
                detail: Some("cargo test -p services --lib -- --nocapture ".repeat(4)),
                key: Some(format!("tool-{i}")),
                not_run: None,
            },
        },
    }
}

fn main() {
    let dir = std::env::temp_dir().join(format!("store-bench-{}", uuid::Uuid::now_v7()));
    let mut db = Store::open(dir.join("db.sqlite"), Retention::default()).unwrap();
    let task = protocol::TaskId::new();
    let now = Timestamp(1_700_000_000_000);
    let mut written = 0u64;
    println!("retained_events,commits,per commit");
    for target in [1_000u64, 10_000, 50_000, 100_000] {
        while written < target {
            db.write(now, |tx| {
                for _ in 0..1000 {
                    tx.emit(activity(task, written))?;
                    written += 1;
                }
                Ok(())
            })
            .unwrap();
        }
        let commits = 200;
        let started = Instant::now();
        for i in 0..commits {
            db.write(now, |tx| tx.emit(activity(task, written + i)))
                .unwrap();
        }
        written += commits;
        println!(
            "{target},{commits},{:.3} ms",
            started.elapsed().as_secs_f64() * 1e3 / commits as f64
        );
    }
    // Statement work without the commit's fsync: one read transaction per
    // lookup, as the hub does, and many writes in one transaction.
    db.write(now, |tx| {
        tx.put("device", "phone", &serde_json::json!({"name": "Phone"}))
    })
    .unwrap();
    let reads = 20_000;
    let started = Instant::now();
    for _ in 0..reads {
        db.read(|r| r.get::<serde_json::Value>("device", "phone"))
            .unwrap()
            .unwrap();
    }
    println!(
        "point reads,{reads},{:.2} us",
        started.elapsed().as_secs_f64() * 1e6 / reads as f64
    );
    let writes = 5_000u64;
    let started = Instant::now();
    db.write(now, |tx| {
        for i in 0..writes {
            tx.put(
                "task",
                i,
                &serde_json::json!({"title": "Task", "revision": i}),
            )?;
            tx.emit(activity(task, written + i))?;
        }
        Ok(())
    })
    .unwrap();
    println!(
        "put and emit in one commit,{writes},{:.2} us",
        started.elapsed().as_secs_f64() * 1e6 / writes as f64
    );
    drop(db);
    std::fs::remove_dir_all(dir).unwrap();
}
