// SPDX-License-Identifier: Apache-2.0
//! Bounded, profile-free Windows metadata query. No command lines or memory reads.
use super::*;

#[derive(Deserialize)]
struct ProcessMetadata {
    pid: u32,
    parent: u32,
    executable: PathBuf,
    created: String,
}
fn decode(bytes: &[u8]) -> Result<Vec<AgentProcess>, HarnessError> {
    let rows: Vec<ProcessMetadata> = serde_json::from_slice(bytes)
        .map_err(|_| HarnessError::Protocol("Windows process metadata was not readable".into()))?;
    if rows.len() > 256 {
        return Err(HarnessError::Protocol(
            "Windows agent process limit reached".into(),
        ));
    }
    Ok(rows
        .into_iter()
        .filter_map(|row| {
            let name = row
                .executable
                .to_string_lossy()
                .replace('\\', "/")
                .rsplit('/')
                .next()?
                .to_ascii_lowercase();
            let harness = match name.as_str() {
                "claude.exe" => HarnessKind::ClaudeCode,
                "codex.exe" => HarnessKind::Codex,
                _ => return None,
            };
            let started_at = timestamp(Some(&row.created))?;
            Some(AgentProcess {
                pid: row.pid,
                ancestors: vec![row.parent],
                process_group: None,
                start_key: row.created,
                harness,
                executable: row.executable,
                cwd: None,
                started_at: Some(started_at),
            })
        })
        .collect())
}

#[cfg(windows)]
pub(super) fn processes() -> Result<Vec<AgentProcess>, HarnessError> {
    use std::{
        os::windows::process::CommandExt,
        process::{Command, Stdio},
        time::{Duration, Instant},
    };
    let root = std::env::var_os("SystemRoot")
        .ok_or_else(|| HarnessError::Protocol("Windows system directory is unavailable".into()))?;
    // A static query asks only for process identity. It does not load profiles,
    // enumerate environment variables, or select Win32_Process.CommandLine.
    let script = r#"$ErrorActionPreference='Stop'; [Console]::OutputEncoding=[Text.UTF8Encoding]::new($false); $sid=[Security.Principal.WindowsIdentity]::GetCurrent().User.Value; $rows=@(Get-CimInstance Win32_Process -Filter "Name='claude.exe' OR Name='codex.exe'" | Select-Object -First 257 | ForEach-Object { $owner=Invoke-CimMethod -InputObject $_ -MethodName GetOwnerSid; if($owner.Sid -eq $sid -and $_.ExecutablePath){ @{pid=[uint32]$_.ProcessId;parent=[uint32]$_.ParentProcessId;executable=$_.ExecutablePath;created=$_.CreationDate.ToUniversalTime().ToString('o')} } }); ConvertTo-Json -InputObject $rows -Compress"#;
    let mut child =
        Command::new(PathBuf::from(&root).join("System32/WindowsPowerShell/v1.0/powershell.exe"))
            .args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                script,
            ])
            .env_clear()
            .env("SystemRoot", &root)
            .creation_flags(0x0800_0000) // CREATE_NO_WINDOW: scans never open a console.
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .stdout(Stdio::piped())
            .spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| HarnessError::Protocol("Process metadata pipe unavailable".into()))?;
    let (send, receive) = std::sync::mpsc::sync_channel(1);
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = stdout
            .take(1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map(|_| bytes);
        let _ = send.send(result);
    });
    let deadline = Instant::now() + Duration::from_secs(3);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            result => {
                let _ = child.kill();
                let _ = child.wait();
                break Err(result.err().unwrap_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "Windows metadata query timed out",
                    )
                }));
            }
        }
    };
    let read = receive.recv_timeout(Duration::from_millis(100));
    // The static query launches no child processes that inherit this pipe.
    if reader.is_finished() {
        let _ = reader.join();
    }
    let status = status?;
    let bytes = read.map_err(|_| {
        HarnessError::Protocol("Process metadata worker ended without a result".into())
    })??;
    if !status.success() || bytes.len() > 1024 * 1024 {
        return Err(HarnessError::Protocol(
            "Windows process metadata check did not complete".into(),
        ));
    }
    decode(&bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn windows_metadata_is_typed_and_never_claims_a_working_folder_or_group() {
        let rows = decode(br#"[{"pid":42,"parent":7,"executable":"C:\\Tools\\codex.exe","created":"2026-09-27T12:00:00Z"}]"#).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].harness, HarnessKind::Codex);
        assert_eq!(rows[0].pid, 42);
        assert!(rows[0].process_group.is_none() && rows[0].cwd.is_none());
        assert!(decode(b"not metadata").is_err());
    }
}
