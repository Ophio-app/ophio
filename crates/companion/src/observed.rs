// SPDX-License-Identifier: Apache-2.0
//! Bounded native process/session metadata producer. No transcript iteration.
use async_trait::async_trait;
use ports::{
    HarnessError,
    harness::{
        AgentProcess, AgentProcessTable, AgentSessionRecord, AgentSessionScan, AgentSessionSource,
        ChildStopResult,
    },
};
#[cfg(target_os = "linux")]
use protocol::model::ChildState;
use protocol::{capability::HarnessKind, time::Timestamp};
use serde::Deserialize;
use std::{
    io::Read,
    path::{Path, PathBuf},
    sync::Arc,
};

const MAX_RECORDS: usize = 128;
const MAX_ENTRIES: usize = 2048;
const MAX_METADATA: u64 = 256 * 1024;

#[derive(Debug)]
pub(crate) struct NativeSessions {
    home: PathBuf,
    processes: Arc<dyn AgentProcessTable>,
    scanning: Arc<tokio::sync::Mutex<()>>,
}
impl NativeSessions {
    pub(crate) fn new(home: PathBuf) -> Self {
        Self {
            home,
            processes: Arc::new(NativeProcesses),
            scanning: Arc::new(tokio::sync::Mutex::new(())),
        }
    }
}
#[async_trait]
impl AgentSessionSource for NativeSessions {
    async fn scan(&self) -> Result<AgentSessionScan, HarnessError> {
        let guard =
            self.scanning.clone().try_lock_owned().map_err(|_| {
                HarnessError::Protocol("An observation is still in progress".into())
            })?;
        let home = self.home.clone();
        let processes = self.processes.clone();
        tokio::task::spawn_blocking(move || {
            let _guard = guard;
            let mut scan = AgentSessionScan { processes: processes.processes()?, ..AgentSessionScan::default() };
            scan.coverage.push("Session indexes and the first Codex metadata record only; conversation entries are not read. Unmatched records do not prove a session is running.".into());
            for (kind, root) in [(HarnessKind::ClaudeCode, home.join(".claude/projects")), (HarnessKind::Codex, home.join(".codex/sessions"))] {
                match metadata_records(kind, &root) {
                    Ok((records, limited)) => {
                        scan.records.extend(records);
                        if limited { scan.coverage.push(format!("{kind:?} metadata scan reached its bounded limit; older sessions may be absent")); }
                    }
                    Err(_) => scan.coverage.push(format!("{kind:?} session metadata could not be read; process observations remain available")),
                }
            }
            #[cfg(windows)]
            scan.coverage.push("Windows process working folders and outside process groups cannot be verified; session-record folders remain available.".into());
            Ok(scan)
        }).await.map_err(|_| HarnessError::Protocol("Session observer worker failed".into()))?
    }
    async fn stop(&self, process: &AgentProcess) -> Result<ChildStopResult, HarnessError> {
        let processes = self.processes.clone();
        let process = process.clone();
        tokio::task::spawn_blocking(move || processes.stop_group(&process))
            .await
            .map_err(|_| HarnessError::Protocol("Outside process Stop worker failed".into()))?
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClaudeIndex {
    entries: Vec<ClaudeEntry>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClaudeEntry {
    session_id: String,
    project_path: String,
    created: Option<String>,
    modified: Option<String>,
}
#[derive(Deserialize)]
struct CodexHeader {
    #[serde(rename = "type")]
    kind: String,
    payload: CodexMetadata,
}
#[derive(Deserialize)]
struct CodexMetadata {
    id: String,
    cwd: String,
    timestamp: Option<String>,
}
fn timestamp(value: Option<&str>) -> Option<Timestamp> {
    let parsed =
        time::OffsetDateTime::parse(value?, &time::format_description::well_known::Rfc3339).ok()?;
    i64::try_from(parsed.unix_timestamp_nanos() / 1_000_000)
        .ok()
        .map(Timestamp)
}
fn regular_file(path: &Path) -> bool {
    path.symlink_metadata()
        .is_ok_and(|m| m.is_file() && !m.file_type().is_symlink())
}
/// Native metadata types deliberately omit prompts, titles, summaries and tool
/// results. Serde skips unselected index fields rather than retaining them.
fn metadata_records(
    kind: HarnessKind,
    root: &Path,
) -> std::io::Result<(Vec<AgentSessionRecord>, bool)> {
    let mut records = Vec::new();
    let mut pending = vec![(root.to_owned(), 0)];
    let mut visited = 0;
    let mut limited = false;
    while let Some((directory, depth)) = pending.pop() {
        if directory
            .symlink_metadata()
            .is_ok_and(|m| m.file_type().is_symlink())
        {
            continue;
        }
        let entries = match std::fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        for entry in entries {
            visited += 1;
            if visited > MAX_ENTRIES || records.len() >= MAX_RECORDS {
                limited = true;
                break;
            }
            let entry = entry?;
            let path = entry.path();
            let kind_of_file = entry.file_type()?;
            if kind_of_file.is_symlink() {
                continue;
            }
            if kind_of_file.is_dir() && depth < 4 {
                pending.push((path, depth + 1));
                continue;
            }
            if !kind_of_file.is_file() {
                continue;
            }
            if kind == HarnessKind::ClaudeCode && entry.file_name() == "sessions-index.json" {
                if entry.metadata()?.len() > MAX_METADATA {
                    limited = true;
                    continue;
                }
                let mut file = std::fs::File::open(&path)?.take(MAX_METADATA + 1);
                // This is the native metadata index, never a Claude conversation file.
                let Ok(index) = serde_json::from_reader::<_, ClaudeIndex>(&mut file) else {
                    continue;
                };
                for entry in index.entries.into_iter().take(MAX_RECORDS - records.len()) {
                    if entry.session_id.is_empty()
                        || entry.session_id.len() > 256
                        || !Path::new(&entry.project_path).is_absolute()
                    {
                        continue;
                    }
                    records.push(AgentSessionRecord {
                        harness: kind,
                        session_id: entry.session_id,
                        cwd: entry.project_path.into(),
                        started_at: timestamp(entry.created.as_deref()),
                        last_activity: timestamp(entry.modified.as_deref()),
                    });
                }
            } else if kind == HarnessKind::Codex
                && path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .is_some_and(|s| s.starts_with("rollout") && s.ends_with(".jsonl"))
                && regular_file(&path)
            {
                // Read one byte at a time so even read-ahead cannot consume the
                // conversation after the metadata header. Large headers are skipped.
                let mut line = Vec::new();
                let file = std::fs::File::open(&path)?;
                let mut file = file.take(MAX_METADATA + 1);
                let mut byte = [0];
                while file.read(&mut byte)? == 1 {
                    if byte[0] == b'\n' {
                        break;
                    }
                    line.push(byte[0]);
                }
                if line.len() as u64 > MAX_METADATA {
                    limited = true;
                    continue;
                }
                let Ok(header) = serde_json::from_slice::<CodexHeader>(&line) else {
                    continue;
                };
                if header.kind != "session_meta"
                    || header.payload.id.is_empty()
                    || header.payload.id.len() > 256
                    || !Path::new(&header.payload.cwd).is_absolute()
                {
                    continue;
                }
                let modified = entry
                    .metadata()?
                    .modified()
                    .ok()
                    .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok())
                    .and_then(|at| i64::try_from(at.as_millis()).ok())
                    .map(Timestamp);
                records.push(AgentSessionRecord {
                    harness: kind,
                    session_id: header.payload.id,
                    cwd: header.payload.cwd.into(),
                    started_at: timestamp(header.payload.timestamp.as_deref()),
                    last_activity: modified,
                });
            }
        }
        if visited > MAX_ENTRIES || records.len() >= MAX_RECORDS {
            limited = true;
            break;
        }
    }
    Ok((records, limited))
}

#[derive(Debug)]
struct NativeProcesses;
#[cfg(target_os = "linux")]
#[derive(Clone)]
struct ProcessStat {
    parent: u32,
    group: u32,
    start: u64,
    zombie: bool,
}
#[cfg(target_os = "linux")]
fn process_stat(text: &str) -> Option<ProcessStat> {
    let (_, tail) = text.rsplit_once(") ")?;
    let fields: Vec<_> = tail.split_whitespace().take(20).collect();
    Some(ProcessStat {
        parent: fields.get(1)?.parse().ok()?,
        group: fields.get(2)?.parse().ok()?,
        start: fields.get(19)?.parse().ok()?,
        zombie: fields.first()? == &"Z",
    })
}
#[cfg(target_os = "linux")]
fn linux_table(
    all_users: bool,
) -> Result<std::collections::HashMap<u32, ProcessStat>, HarnessError> {
    use std::os::unix::fs::MetadataExt;
    let uid = rustix::process::getuid().as_raw();
    let mut table = std::collections::HashMap::new();
    for (index, entry) in std::fs::read_dir("/proc")?.enumerate() {
        if index >= 16384 {
            return Err(HarnessError::Protocol(
                "Process table exceeds the observation limit".into(),
            ));
        }
        let entry = entry?;
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        if !all_users && !entry.metadata().is_ok_and(|m| m.uid() == uid) {
            continue;
        }
        match std::fs::read_to_string(entry.path().join("stat")) {
            Ok(text) => {
                if let Some(stat) = process_stat(&text) {
                    table.insert(pid, stat);
                } else if all_users {
                    return Err(HarnessError::Protocol(
                        "A process-group member could not be checked".into(),
                    ));
                }
            }
            Err(error) if all_users && error.kind() != std::io::ErrorKind::NotFound => {
                return Err(error.into());
            }
            Err(_) => {}
        }
    }
    Ok(table)
}
#[cfg(target_os = "linux")]
impl AgentProcessTable for NativeProcesses {
    fn processes(&self) -> Result<Vec<AgentProcess>, HarnessError> {
        let table = linux_table(false)?;
        let boot = std::fs::read_to_string("/proc/stat").ok().and_then(|text| {
            text.lines()
                .find_map(|line| line.strip_prefix("btime ")?.parse::<u64>().ok())
        });
        let ticks = rustix::param::clock_ticks_per_second().max(1);
        let mut result = Vec::new();
        for (&pid, stat) in &table {
            if stat.zombie {
                continue;
            }
            let directory = PathBuf::from(format!("/proc/{pid}"));
            let Ok(executable) = std::fs::read_link(directory.join("exe")) else {
                continue;
            };
            let comm = std::fs::read_to_string(directory.join("comm")).unwrap_or_default();
            let name = executable
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default();
            let harness = match name {
                "codex" => HarnessKind::Codex,
                "claude" => HarnessKind::ClaudeCode,
                _ if comm.trim() == "claude"
                    && executable
                        .components()
                        .any(|part| part.as_os_str() == "claude") =>
                {
                    HarnessKind::ClaudeCode
                }
                _ => continue,
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
                start_key: stat.start.to_string(),
                harness,
                executable,
                cwd: std::fs::read_link(directory.join("cwd")).ok(),
                started_at: boot
                    .and_then(|boot| {
                        boot.checked_mul(1000)?
                            .checked_add(stat.start.saturating_mul(1000) / ticks)
                    })
                    .and_then(|ms| i64::try_from(ms).ok())
                    .map(Timestamp),
            });
        }
        Ok(result)
    }
    fn stop_group(&self, process: &AgentProcess) -> Result<ChildStopResult, HarnessError> {
        let current = self.processes()?.into_iter().find(|p| p.pid == process.pid);
        let Some(current) = current else {
            return Ok(ChildStopResult { state: ChildState::Unknown, detail: "The observed group leader is no longer present; its process group and detached children are not verified".into() });
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
        // native agent. Never signal a shell/shared group or claim detached jobs.
        rustix::process::kill_process_group(pid, rustix::process::Signal::TERM)
            .map_err(std::io::Error::from)?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            if !linux_table(true)?
                .values()
                .any(|p| p.group == process.pid && !p.zombie)
            {
                return Ok(ChildStopResult { state: ChildState::Stopped, detail: "No live processes remain in the observed process group. Detached work and earlier file changes are not undone.".into() });
            }
            if std::time::Instant::now() >= deadline {
                return Ok(ChildStopResult { state: ChildState::Active, detail: "The process group is still present after the termination signal; check it in its terminal on the computer".into() });
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
    }
}

#[cfg(any(windows, test))]
#[path = "observed_windows.rs"]
mod windows;
#[cfg(windows)]
impl AgentProcessTable for NativeProcesses {
    fn processes(&self) -> Result<Vec<AgentProcess>, HarnessError> {
        windows::processes()
    }
    fn stop_group(&self, _: &AgentProcess) -> Result<ChildStopResult, HarnessError> {
        Err(HarnessError::Unsupported("Windows cannot verify the outside agent’s process group. Stop it in its terminal on the computer.".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Debug)]
    struct FakeTable;
    impl AgentProcessTable for FakeTable {
        fn processes(&self) -> Result<Vec<AgentProcess>, HarnessError> {
            Ok(vec![])
        }
        fn stop_group(&self, _: &AgentProcess) -> Result<ChildStopResult, HarnessError> {
            Err(HarnessError::NotActive)
        }
    }
    #[tokio::test]
    async fn reads_only_authored_metadata_and_skips_symlinks_and_conversation_rows() {
        let home = tempfile::tempdir().unwrap();
        let claude = home.path().join(".claude/projects/fixture");
        let codex = home.path().join(".codex/sessions/2026/09/27");
        std::fs::create_dir_all(&claude).unwrap();
        std::fs::create_dir_all(&codex).unwrap();
        let cwd = home.path().to_string_lossy();
        std::fs::write(claude.join("sessions-index.json"), serde_json::json!({"entries":[{"sessionId":"claude-session","projectPath":cwd,"created":"2026-09-27T12:00:00Z","modified":"2026-09-27T12:01:00Z","firstPrompt":"discarded field","summary":"discarded field"}]}).to_string()).unwrap();
        std::fs::write(codex.join("rollout.jsonl"), format!("{}\nnot valid JSON: this conversation must not be parsed\n", serde_json::json!({"type":"session_meta","payload":{"id":"codex-session","cwd":cwd,"timestamp":"2026-09-27T12:00:00Z"}}))).unwrap();
        std::fs::write(claude.join("conversation.jsonl"), "this must not be read").unwrap();
        let source = NativeSessions {
            home: home.path().to_owned(),
            processes: Arc::new(FakeTable),
            scanning: Arc::new(tokio::sync::Mutex::new(())),
        };
        let scan = source.scan().await.unwrap();
        assert_eq!(scan.records.len(), 2);
        assert!(
            scan.records
                .iter()
                .all(|r| r.started_at.is_some() && r.last_activity.is_some())
        );
        assert!(scan.processes.is_empty());
        #[cfg(unix)]
        {
            let linked = codex.join("linked.jsonl");
            std::os::unix::fs::symlink(codex.join("rollout.jsonl"), &linked).unwrap();
            assert_eq!(source.scan().await.unwrap().records.len(), 2);
        }
    }
    #[test]
    fn metadata_work_is_bounded() {
        let home = tempfile::tempdir().unwrap();
        for index in 0..(MAX_RECORDS + 2) {
            std::fs::write(home.path().join(format!("rollout-{index}.jsonl")), serde_json::json!({"type":"session_meta","payload":{"id":index.to_string(),"cwd":home.path(),"timestamp":null}}).to_string()).unwrap();
        }
        let (records, limited) = metadata_records(HarnessKind::Codex, home.path()).unwrap();
        assert_eq!(records.len(), MAX_RECORDS);
        assert!(limited);
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn process_identity_parser_handles_spaces_and_parentheses() {
        let stat =
            process_stat("42 (agent (native)) S 7 42 42 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 12345 0")
                .unwrap();
        assert_eq!((stat.parent, stat.group, stat.start), (7, 42, 12345));
        assert!(!stat.zombie);
    }
}
