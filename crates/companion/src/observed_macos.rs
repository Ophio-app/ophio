// SPDX-License-Identifier: Apache-2.0
//! The Mac's process table for finding agents started outside Øphio. Only
//! identities are read: never command lines, environments or memory.

use platform_macos::process::{self, Process};
use ports::{
    HarnessError,
    harness::{AgentProcess, ChildStopResult},
};
use protocol::{capability::HarnessKind, model::ChildState, time::Timestamp};
use std::collections::HashMap;

fn table(all_users: bool) -> Result<HashMap<u32, Process>, HarnessError> {
    let uid = rustix::process::getuid().as_raw();
    Ok(process::all()?
        .into_iter()
        .filter_map(process::read)
        .filter(|process| all_users || process.uid == uid)
        .map(|process| (process.pid, process))
        .collect())
}

/// Which agent a program is, from its file and the name it gave itself.
fn harness_of(executable: &std::path::Path, name: &str) -> Option<HarnessKind> {
    let file = executable
        .file_name()
        .and_then(|file| file.to_str())
        .unwrap_or_default();
    match file {
        "codex" => Some(HarnessKind::Codex),
        "claude" => Some(HarnessKind::ClaudeCode),
        _ if name == "claude"
            && executable
                .components()
                .any(|part| part.as_os_str() == "claude") =>
        {
            Some(HarnessKind::ClaudeCode)
        }
        _ => None,
    }
}

pub(super) fn processes() -> Result<Vec<AgentProcess>, HarnessError> {
    let table = table(false)?;
    let mut result = Vec::new();
    for (&pid, stat) in &table {
        if stat.zombie {
            continue;
        }
        let Ok(raw) = i32::try_from(pid) else {
            continue;
        };
        let Some(path) = process::executable(raw) else {
            continue;
        };
        let Some(harness) = harness_of(&path, &stat.name) else {
            continue;
        };
        let mut ancestors = Vec::new();
        let mut parent = stat.parent;
        for _ in 0..64 {
            if parent == 0 || ancestors.contains(&parent) {
                break;
            }
            ancestors.push(parent);
            let Some(stat) = table.get(&parent) else {
                break;
            };
            parent = stat.parent;
        }
        result.push(AgentProcess {
            pid,
            ancestors,
            process_group: Some(stat.group),
            start_key: format!("{}.{:06}", stat.start_seconds, stat.start_micros),
            harness,
            executable: path,
            cwd: process::working_folder(raw),
            started_at: stat
                .start_seconds
                .checked_mul(1000)
                .and_then(|ms| ms.checked_add(stat.start_micros / 1000))
                .and_then(|ms| i64::try_from(ms).ok())
                .map(Timestamp),
        });
    }
    Ok(result)
}

pub(super) fn stop_group(process: &AgentProcess) -> Result<ChildStopResult, HarnessError> {
    let current = processes()?.into_iter().find(|p| p.pid == process.pid);
    let Some(current) = current else {
        return Ok(ChildStopResult {
            state: ChildState::Unknown,
            detail: "The observed group leader is no longer present; its process group and detached children are not verified".into(),
        });
    };
    if current.start_key != process.start_key
        || current.executable != process.executable
        || current.process_group != Some(process.pid)
        || process.pid <= 1
    {
        return Err(HarnessError::Protocol(
            "Process identity or group changed; refresh and review the outside session again"
                .into(),
        ));
    }
    let pid = i32::try_from(process.pid)
        .ok()
        .and_then(rustix::process::Pid::from_raw)
        .ok_or_else(|| HarnessError::Protocol("Invalid process identity".into()))?;
    // A signal goes only to a freshly verified group led by the reviewed
    // native agent. Never signal a shell's or shared group, or claim detached jobs.
    rustix::process::kill_process_group(pid, rustix::process::Signal::TERM)
        .map_err(std::io::Error::from)?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        if !table(true)?
            .values()
            .any(|p| p.group == process.pid && !p.zombie)
        {
            return Ok(ChildStopResult {
                state: ChildState::Stopped,
                detail: "No live processes remain in the observed process group. Detached work and earlier file changes are not undone.".into(),
            });
        }
        if std::time::Instant::now() >= deadline {
            return Ok(ChildStopResult {
                state: ChildState::Active,
                detail: "The process group is still present after the termination signal; check it in its terminal on the computer".into(),
            });
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agents_are_told_by_their_file_or_their_own_name() {
        use std::path::Path;
        assert_eq!(
            harness_of(Path::new("/opt/homebrew/bin/codex"), "codex"),
            Some(HarnessKind::Codex)
        );
        assert_eq!(
            harness_of(Path::new("/Users/sam/.local/bin/claude"), "node"),
            Some(HarnessKind::ClaudeCode)
        );
        assert_eq!(
            harness_of(
                Path::new("/Users/sam/.local/share/claude/versions/2.1.0"),
                "claude"
            ),
            Some(HarnessKind::ClaudeCode)
        );
        assert_eq!(harness_of(Path::new("/usr/bin/vim"), "claude"), None);
        assert_eq!(harness_of(Path::new("/usr/bin/vim"), "vim"), None);
    }
}
