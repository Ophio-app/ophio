// SPDX-License-Identifier: Apache-2.0
//! Synchronous crash capture, independent of the runtime and database.
use protocol::{reports::*, time::Timestamp};
use std::{
    path::{Path, PathBuf},
    sync::{Once, RwLock},
};

static INSTALL: Once = Once::new();
static TARGET: RwLock<Option<(PathBuf, String)>> = RwLock::new(None);

pub fn install_program_panic_hook() {
    let Ok(paths) = crate::config::Paths::named(crate::config::APP_NAME) else {
        return;
    };
    let data = if !paths.data.exists() {
        crate::config::Paths::named(crate::config::EARLIER_APP_NAME)
            .ok()
            .map(|p| p.data)
            .filter(|p| p.exists())
            .unwrap_or(paths.data)
    } else {
        paths.data
    };
    let build = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| install_info(dir).0))
        .unwrap_or_else(|| env!("CARGO_PKG_VERSION").into());
    install_panic_hook(data, build);
}
pub fn install_panic_hook(data: PathBuf, build: String) {
    if let Ok(mut target) = TARGET.write() {
        *target = Some((data, build));
    }
    INSTALL.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if let Ok(target) = TARGET.try_read()
                && let Some((data, build)) = target.as_ref()
            {
                let location = info.location();
                let file = location
                    .and_then(|l| repo_file(l.file()))
                    .unwrap_or_else(|| "unknown".into());
                let line = location.map_or(0, |l| l.line());
                let message = info
                    .payload()
                    .downcast_ref::<&str>()
                    .copied()
                    .or_else(|| info.payload().downcast_ref::<String>().map(String::as_str))
                    .and_then(scrub);
                let backtrace = std::backtrace::Backtrace::force_capture().to_string();
                let record = CrashRecord {
                    at: Timestamp::now(),
                    side: ReportSide::Computer,
                    kind: CrashKind::Panic,
                    code: format!("panic:{file}:{line}"),
                    build: build.clone(),
                    message,
                    frames: frames(&backtrace),
                };
                let _ = services::reports::try_append_crash(data, &record);
            }
            previous(info);
        }));
    });
}
fn repo_file(file: &str) -> Option<String> {
    let file = file.replace('\\', "/");
    let start = if file.starts_with("crates/") {
        0
    } else {
        file.find("/crates/")? + 1
    };
    let file = &file[start..];
    let parts = file.split('/').collect::<Vec<_>>();
    const CRATES: &[&str] = &[
        "accounts",
        "browser",
        "channels",
        "companion",
        "computer-mcp",
        "containment",
        "domain",
        "harness-claude",
        "harness-codex",
        "jev",
        "library",
        "media",
        "platform-linux",
        "platform-windows",
        "ports",
        "protocol",
        "sandbox",
        "services",
        "speech",
        "store",
        "terminal",
        "voice-control",
    ];
    if parts.len() < 4
        || parts[0] != "crates"
        || !CRATES.contains(&parts[1])
        || !matches!(parts[2], "src" | "tests")
        || parts.iter().any(|p| *p == ".." || *p == ".")
    {
        return None;
    }
    if file.len() > MAX_REPORT_CODE - 24 {
        return None;
    }
    Some(file.into())
}
fn frames(backtrace: &str) -> Vec<StackFrame> {
    let mut function = None;
    let mut frames = Vec::new();
    for line in backtrace.lines() {
        let line = line.trim();
        if let Some(location) = line.strip_prefix("at ") {
            let mut parts = location.rsplitn(3, ':');
            let (Some(column), Some(line), Some(file)) = (parts.next(), parts.next(), parts.next())
            else {
                continue;
            };
            let (Ok(column), Ok(line), Some(file)) =
                (column.parse(), line.parse(), repo_file(file))
            else {
                continue;
            };
            frames.push(StackFrame {
                function: function.take().and_then(|f: String| scrub(&f)),
                file,
                line,
                column: Some(column),
            });
            if frames.len() == MAX_FRAMES {
                break;
            }
        } else if let Some((number, name)) = line.split_once(": ")
            && number.parse::<u32>().is_ok()
        {
            function = Some(name.into());
        }
    }
    frames
}
/// Read only the install adjacent to the executable; tests and unpacked builds
/// never look in the person's real installation.
pub(crate) fn install_info(executable_dir: &Path) -> (String, Option<String>, Option<Trail>) {
    fn read(path: &Path) -> Option<serde_json::Value> {
        let bytes = std::fs::read(path).ok()?;
        if bytes.len() > 64 * 1024 {
            return None;
        }
        serde_json::from_slice(&bytes).ok()
    }
    let root = executable_dir.parent().unwrap_or(executable_dir);
    let current = read(&root.join("installed.json")).or_else(|| read(&root.join("release.json")));
    let build = current
        .as_ref()
        .and_then(|v| v.get("build"))
        .and_then(|v| v.as_str())
        .and_then(build_id)
        .unwrap_or_else(|| env!("CARGO_PKG_VERSION").into());
    let previous = read(&root.join("previous/installed.json"))
        .filter(|_| root.join("previous/bin/ophio").is_file())
        .and_then(|v| v.get("build").and_then(|v| v.as_str()).and_then(build_id));
    let update = read(&root.join("last-update.json")).and_then(|v| {
        let from = v.get("from")?.get("build")?.as_str()?.to_owned();
        let to = v.get("release")?.get("build")?.as_str()?.to_owned();
        let outcome = match v.get("result")?.as_str()? {
            "installed" => UpdateOutcome::Installed,
            "rolled_back" => UpdateOutcome::RolledBack,
            "not_installed" | "cancelled" => UpdateOutcome::Refused,
            _ => return None,
        };
        Some(Trail::Update { from, to, outcome })
    });
    (build, previous, update)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn panic_hook_writes_a_scrubbed_record_and_chains_the_existing_hook() {
        let dir = tempfile::tempdir().unwrap();
        install_panic_hook(dir.path().to_owned(), "fixture-build".into());
        let result = std::panic::catch_unwind(|| panic!("open /workspace/private failed 'secret'"));
        assert!(result.is_err());
        let records = services::reports::read_crashes(dir.path());
        assert_eq!(records.len(), 1);
        let record = &records[0];
        assert_eq!(record.kind, CrashKind::Panic);
        assert!(
            record
                .code
                .starts_with("panic:crates/companion/src/reports.rs:")
        );
        assert_eq!(record.message.as_deref(), Some("open <path> failed \"…\""));
        assert!(record.frames.len() <= MAX_FRAMES);
        assert!(record.frames.iter().all(|f| f.file.starts_with("crates/")));
        if let Ok(mut target) = TARGET.write() {
            *target = None;
        }
    }
    #[test]
    fn frames_only_keep_repository_sources() {
        let frames = frames(
            " 0: companion::example\n at /workspace/crates/companion/src/main.rs:4:2\n 1: std::panic\n at /rust/library/std/src/panic.rs:2:1\n 2: external\n at /registry/other/src/lib.rs:1:1",
        );
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].file, "crates/companion/src/main.rs");
        assert!(repo_file("/workspace/private/src/main.rs").is_none());
    }
}
