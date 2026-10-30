// SPDX-License-Identifier: Apache-2.0
//! Alone in its own test binary, so no other library's watch thread is counted.
#![cfg(target_os = "linux")]
use library::{FsLibrary, LibraryConfig};
use ports::library::{LibraryFs, RootSpec};
use protocol::{ids::LibraryRootId, library::RootKind};
use std::{sync::Arc, time::Duration};

/// Scheduler switches so far, and count, of this process's threads named `name`.
fn threads(name: &str) -> (u64, usize) {
    let statuses: Vec<String> = std::fs::read_dir("/proc/self/task")
        .unwrap()
        .flatten()
        .filter(|task| {
            std::fs::read_to_string(task.path().join("comm")).is_ok_and(|c| c.trim_end() == name)
        })
        .filter_map(|task| std::fs::read_to_string(task.path().join("status")).ok())
        .collect();
    let switches = statuses
        .iter()
        .flat_map(|status| status.lines().filter(|l| l.contains("ctxt_switches:")))
        .filter_map(|l| l.split_whitespace().nth(1)?.parse::<u64>().ok())
        .sum();
    (switches, statuses.len())
}

#[tokio::test]
async fn an_idle_library_watch_sleeps_and_ends_with_the_library() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("root");
    std::fs::create_dir(&root).unwrap();
    let config = LibraryConfig::new(temp.path().join("data"), temp.path().join("home"));
    let fs = FsLibrary::new(config, Arc::new(ports::clock::SystemClock)).unwrap();
    fs.set_roots(vec![RootSpec {
        root_id: LibraryRootId::new(),
        path: root,
        label: "Idle".into(),
        kind: RootKind::Project,
        writable: true,
        project_id: None,
        harness: None,
        only: None,
    }])
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    let (before, count) = threads("library-watch");
    assert_eq!(count, 1);
    tokio::time::sleep(Duration::from_secs(1)).await;
    let woken = threads("library-watch").0 - before;
    assert!(woken <= 2, "idle library watch woke {woken} times in 1 s");
    drop(fs);
    tokio::time::timeout(Duration::from_secs(2), async {
        while threads("library-watch").1 > 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the watch thread ends when the library is dropped");
}
