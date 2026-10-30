// SPDX-License-Identifier: Apache-2.0
//! Non-inference shell capability discovery and executable diagnostics.
use crate::io;
use ports::HarnessError;
use protocol::capability::Availability;
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

pub(crate) fn sandbox_warning(params: &Value) -> Option<String> {
    let summary = params["summary"].as_str()?;
    let detail = params["details"].as_str().unwrap_or_default();
    let text = if detail.is_empty() {
        summary.to_owned()
    } else {
        format!("{summary}: {detail}")
    };
    let lower = text.to_lowercase();
    (lower.contains("sandbox")
        && [
            "fail",
            "cannot",
            "can't",
            "could not",
            "unavailable",
            "unable",
            "not available",
        ]
        .iter()
        .any(|word| lower.contains(word)))
    .then(|| {
        text.chars()
            .filter(|c| !c.is_control())
            .take(1000)
            .collect()
    })
}
/// The oldest Codex this adapter is qualified with.
pub(crate) const MINIMUM: (u64, u64, u64) = (0, 153, 2);

fn supported() -> String {
    let (major, minor, patch) = MINIMUM;
    format!("Øphio supports Codex {major}.{minor}.{patch} and later.")
}

fn version_number(text: &str) -> Option<(u64, u64, u64)> {
    text.split_whitespace().find_map(|word| {
        let mut numbers = word.trim_start_matches('v').split('.');
        Some((
            numbers.next()?.parse().ok()?,
            numbers.next()?.parse().ok()?,
            numbers.next()?.split('-').next()?.parse().ok()?,
        ))
    })
}
pub(crate) fn candidates() -> Vec<PathBuf> {
    let executable = if cfg!(windows) { "codex.exe" } else { "codex" };
    let mut paths = std::env::var_os("PATH")
        .map(|p| {
            std::env::split_paths(&p)
                .map(|p| p.join(executable))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    for root in [std::env::var_os("HOME"), std::env::var_os("USERPROFILE")]
        .into_iter()
        .flatten()
    {
        for suffix in [
            ".local/bin",
            ".cargo/bin",
            ".local/opt/codex",
            "AppData/Local/Programs",
            "AppData/Roaming/npm",
        ] {
            paths.push(PathBuf::from(&root).join(suffix).join(executable));
        }
    }
    let mut unique = Vec::new();
    let mut canonical = Vec::new();
    for path in paths {
        if !path.is_file() {
            continue;
        }
        if let Ok(resolved) = path.canonicalize()
            && !canonical.contains(&resolved)
        {
            canonical.push(resolved);
            unique.push(path);
            if unique.len() == 16 {
                break;
            }
        }
    }
    unique
}

pub(crate) async fn report(
    binary: &Path,
    version: Option<&str>,
    capability: Result<Capability, HarnessError>,
    alternatives: Vec<PathBuf>,
    remembered_warning: Option<String>,
) -> Availability {
    let current = version.and_then(version_number);
    let too_old = current.is_some_and(|v| v < MINIMUM);
    let reason = match (remembered_warning.or_else(|| capability.as_ref().ok().and_then(|c| c.warning.clone())), capability) {
        _ if too_old => format!("{} Update Codex on the computer.", supported()),
        (Some(warning), _) => format!("Codex reports: {warning}. Fix the sandbox on the computer, then restart Øphio and check readiness."),
        (None, Ok(Capability { shell: Some(true), code_mode_host, .. })) => {
            match code_mode_host.filter(|enabled| *enabled).and_then(|_| missing_host(binary)) {
                None => return Availability::Available,
                Some(host) => format!("Its command tool can’t start, because {} is missing. Reinstall Codex so the helper sits beside it, or choose a complete installation. {}", host.display(), supported()),
            }
        }
        (None, Ok(Capability { shell: Some(false), .. })) => "The app-server reports shell tools are disabled. Check the selected Codex installation and its shell_tool setting.".into(),
        _ => format!("The app-server did not confirm shell-tool support. Update Codex or choose an installation that reports it. {}", supported()),
    };
    let mut detail = format!(
        "Found {} ({version}): {reason}",
        binary.display(),
        version = version.unwrap_or("version unknown")
    );
    if let Some(current) = current {
        let selected = binary.canonicalize().ok();
        let others: Vec<_> = alternatives
            .into_iter()
            .take(16)
            .filter(|path| path.canonicalize().ok() != selected)
            .collect();
        // Every other installation is asked at once; the first newer one in
        // search order is named.
        let mut asked = tokio::task::JoinSet::new();
        for (order, path) in others.iter().cloned().enumerate() {
            asked.spawn(async move { (order, io::inspect(&path, &["--version"]).await) });
        }
        let mut newer: Option<(usize, String)> = None;
        while let Some(Ok((order, output))) = asked.join_next().await {
            let Some(output) = output.filter(|o| o.status.success()) else {
                continue;
            };
            let other = String::from_utf8_lossy(&output.stdout).trim().to_owned();
            if version_number(&other).is_some_and(|v| v > current)
                && newer.as_ref().is_none_or(|(first, _)| order < *first)
            {
                newer = Some((order, other));
            }
        }
        if let Some((order, other)) = newer {
            detail.push_str(&format!(
                " A newer Codex exists at {} ({other}). Select that executable or correct PATH.",
                others[order].display()
            ));
        }
    }
    if too_old {
        Availability::Incompatible { reason: detail }
    } else {
        Availability::TemporarilyUnavailable { reason: detail }
    }
}

#[derive(Default)]
pub(crate) struct Capability {
    shell: Option<bool>,
    /// Commands of code-mode models run through a helper executable.
    code_mode_host: Option<bool>,
    warning: Option<String>,
}

/// The code-mode helper that belongs beside this Codex executable, if it isn't
/// there. Codex looks for it beside its own executable, so only a native
/// executable is checked; a launcher script finds the real one itself.
fn missing_host(binary: &Path) -> Option<PathBuf> {
    use std::io::Read;
    let executable = binary.canonicalize().ok()?;
    let mut magic = [0; 4];
    std::fs::File::open(&executable)
        .ok()?
        .read_exact(&mut magic)
        .ok()?;
    let native = magic == *b"\x7fELF"
        || magic.starts_with(b"MZ")
        || [[0xcf, 0xfa, 0xed, 0xfe], [0xca, 0xfe, 0xba, 0xbe]].contains(&magic);
    let host = executable.with_file_name(format!(
        "codex-code-mode-host{}",
        std::env::consts::EXE_SUFFIX
    ));
    (native && !host.is_file()).then_some(host)
}
fn failure() -> HarnessError {
    HarnessError::Protocol("Shell capability could not be checked".into())
}
pub(crate) async fn inspect(binary: &Path) -> Result<Capability, HarnessError> {
    let directory = tempfile::tempdir()?;
    let mut child = io::command(binary)
        .arg("app-server")
        .current_dir(directory.path())
        .spawn()?;
    let mut stdin = child.stdin.take().ok_or_else(failure)?;
    let mut frames = io::Frames::new(child.stdout.take().ok_or_else(failure)?);
    let result = tokio::time::timeout(Duration::from_secs(5), async {
        let mut capability = Capability::default();
        for (id, method, params) in [
            (1, "initialize", json!({"clientInfo":{"name":ports::PROGRAM_NAME,"version":env!("CARGO_PKG_VERSION")},"capabilities":{"experimentalApi":true}})),
            (2, "experimentalFeature/list", json!({"limit":100})),
        ] {
            io::write(&mut stdin, &json!({"id":id,"method":method,"params":params})).await?;
            let response = loop {
                let bytes = frames.next().await?.ok_or_else(failure)?;
                let value: Value = serde_json::from_slice(&bytes).map_err(|_| failure())?;
                if value["method"] == "configWarning" {
                    capability.warning = sandbox_warning(&value["params"]).or(capability.warning);
                }
                if value["id"] == id { break value; }
                if value.get("id").is_some() { return Err(failure()); }
            };
            if response.get("error").is_some() { return Ok(capability); }
            if id == 1 { io::write(&mut stdin, &json!({"method":"initialized"})).await?; }
            else if let Some(features) = response["result"]["data"].as_array().filter(|a| a.len() <= 256) {
                // shell_tool gates shell execution; unified_exec alone describes
                // one execution backend, not proof that shell tools are enabled.
                let enabled = |name: &str| features.iter().find(|f| f["name"] == name).and_then(|f| f["enabled"].as_bool());
                capability.shell = enabled("shell_tool");
                capability.code_mode_host = enabled("code_mode_host");
            }
        }
        Ok(capability)
    }).await.map_err(|_| failure()).and_then(|r| r);
    drop(stdin);
    io::kill(&mut child);
    let _ = child.wait().await;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn warning_filter_preserves_native_sandbox_cause() {
        assert_eq!(
            sandbox_warning(&json!({"summary":"Sandbox cannot start: user namespaces disabled"})),
            Some("Sandbox cannot start: user namespaces disabled".into())
        );
        assert_eq!(
            sandbox_warning(&json!({"summary":"unknown key: fixture"})),
            None
        );
        assert_eq!(version_number("codex-cli 0.153.2"), Some((0, 153, 2)));
        assert!(version_number("unknown").is_none());
    }
    #[test]
    fn a_lone_codex_executable_lacks_its_command_helper() {
        let dir = tempfile::tempdir().unwrap();
        let codex = dir.path().join("codex");
        std::fs::write(&codex, b"\x7fELF fixture").unwrap();
        let host = codex.canonicalize().unwrap().with_file_name(format!(
            "codex-code-mode-host{}",
            std::env::consts::EXE_SUFFIX
        ));
        assert_eq!(missing_host(&codex), Some(host.clone()));
        std::fs::write(&host, b"").unwrap();
        assert_eq!(missing_host(&codex), None);
        // A launcher script runs an executable found elsewhere.
        let launcher = dir.path().join("launcher");
        std::fs::write(&launcher, b"#!/usr/bin/env node\n").unwrap();
        std::fs::remove_file(&host).unwrap();
        assert_eq!(missing_host(&launcher), None);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn shadowed_binary_and_native_warning_are_actionable_without_a_turn() {
        let dir = tempfile::tempdir().unwrap();
        let old_dir = dir.path().join("old");
        let new_dir = dir.path().join("new");
        std::fs::create_dir_all(&old_dir).unwrap();
        std::fs::create_dir_all(&new_dir).unwrap();
        let old = old_dir.join("codex");
        let new = new_dir.join("codex");
        let fixture = include_str!("../tests/fake_codex.py");
        test_support::executable(&old, fixture).unwrap();
        test_support::executable(&new, fixture).unwrap();
        std::fs::write(old_dir.join("version"), "codex-cli 0.100.0").unwrap();
        std::fs::write(old_dir.join("no-shell"), "").unwrap();
        let result = report(
            &old,
            Some("codex-cli 0.100.0"),
            inspect(&old).await,
            vec![old.clone(), new.clone()],
            None,
        )
        .await;
        let Availability::Incompatible { reason } = result else {
            panic!("false readiness")
        };
        assert!(
            reason.contains(old.to_str().unwrap())
                && reason.contains("0.100.0")
                && reason.contains("Øphio supports Codex 0.153.2 and later")
                && reason.contains(new.to_str().unwrap())
                && reason.contains("newer")
        );
        std::fs::remove_file(old_dir.join("no-shell")).unwrap();
        std::fs::write(
            old_dir.join("sandbox-warning"),
            "Sandbox cannot start: fixture namespaces disabled",
        )
        .unwrap();
        let result = report(
            &old,
            Some("codex-cli 0.153.2"),
            inspect(&old).await,
            vec![],
            None,
        )
        .await;
        assert!(
            matches!(result, Availability::TemporarilyUnavailable { reason } if reason.contains("fixture namespaces disabled"))
        );
        std::fs::remove_file(old_dir.join("sandbox-warning")).unwrap();
        assert!(
            report(
                &new,
                Some("codex-cli 0.153.2"),
                inspect(&new).await,
                vec![],
                None
            )
            .await
            .is_available()
        );
    }
}
