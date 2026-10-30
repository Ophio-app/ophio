// SPDX-License-Identifier: Apache-2.0
//! The children `fixtures/detach.sh` leaves behind, one for each way a
//! program can move work into the background.
use std::{
    path::Path,
    time::{Duration, Instant},
};

pub const SCRIPT: &str = include_str!("../fixtures/detach.sh");

/// `setsid` and a daemon leave the process group; the others stay in it.
pub const WAYS: [&str; 4] = ["background", "nohup", "setsid", "daemon"];

pub fn left_group(way: &str) -> bool {
    matches!(way, "setsid" | "daemon")
}

/// Every child's process id, once each has recorded it in `dir`.
pub fn children(dir: &Path) -> Vec<(&'static str, u32)> {
    let deadline = Instant::now() + Duration::from_secs(10);
    WAYS.iter()
        .map(|way| {
            loop {
                if let Ok(pid) = std::fs::read_to_string(dir.join(format!("{way}.pid"))) {
                    break (*way, pid.trim().parse().unwrap());
                }
                assert!(Instant::now() < deadline, "{way} child never started");
                std::thread::sleep(Duration::from_millis(10));
            }
        })
        .collect()
}

/// A live process, not one that has exited and waits to be reaped.
pub fn running(pid: u32) -> bool {
    // The name, in parentheses, may hold any bytes; the state follows it.
    std::fs::read(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
        stat.iter().rposition(|b| *b == b')').is_some_and(|end| {
            !matches!(
                stat[end + 1..]
                    .split(u8::is_ascii_whitespace)
                    .find(|f| !f.is_empty()),
                Some(b"Z" | b"X")
            )
        })
    })
}

/// Whether `pid` ends within three seconds.
pub fn ends(pid: u32) -> bool {
    let deadline = Instant::now() + Duration::from_secs(3);
    while running(pid) {
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    true
}

/// With a complete container every child ended with the run. With only a
/// process group, exactly those that stayed in it did, and the others, still
/// running as the run's containment reported, are ended here.
pub fn assert_ended(complete: bool, children: &[(&str, u32)]) {
    for (way, pid) in children {
        if complete || !left_group(way) {
            assert!(ends(*pid), "{way} child survived");
        } else {
            assert!(running(*pid), "{way} child was reached");
            let _ = std::process::Command::new("kill")
                .args(["-KILL", &pid.to_string()])
                .status();
        }
    }
}
