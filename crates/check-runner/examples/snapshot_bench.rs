// SPDX-License-Identifier: Apache-2.0
//! Run with `cargo run -p check-runner --example snapshot_bench -- /path/to/project`.
//! No project files are created or changed. Temporary snapshot data is removed.
use check_runner::snapshot::SnapshotStore;
use std::{error::Error, path::PathBuf, time::Instant};

fn main() -> Result<(), Box<dyn Error>> {
    let project = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or(std::env::current_dir()?);
    let data = tempfile::tempdir()?;
    let store = SnapshotStore::new(data.path())?;
    let started = Instant::now();
    let mut previous = store.capture(&project, None)?;
    println!(
        "Cold capture: {:.3} seconds; {} files; {} issues.",
        started.elapsed().as_secs_f64(),
        previous.files.len(),
        previous.issues.len()
    );
    let mut samples = Vec::new();
    for sample in 1..=7 {
        let started = Instant::now();
        let next = store.capture(&project, Some(&previous))?;
        let elapsed = started.elapsed().as_secs_f64();
        println!(
            "Warm capture {sample}: {elapsed:.3} seconds; {} reused hashes; {} issues; changed: {}.",
            next.reused_hashes,
            next.issues.len(),
            !previous.diff(&next)?.is_empty()
        );
        samples.push(elapsed);
        previous = next;
    }
    samples.sort_by(f64::total_cmp);
    println!(
        "Warm median: {:.3} seconds; slowest: {:.3} seconds.",
        samples[samples.len() / 2],
        samples[samples.len() - 1]
    );
    if !previous.issues.is_empty() {
        eprintln!("Capture issues must be investigated before treating these records as complete.");
        for issue in &previous.issues {
            eprintln!(
                "{}: {}",
                issue.path.as_deref().unwrap_or("Folder walk"),
                issue.message
            );
        }
    }
    Ok(())
}
