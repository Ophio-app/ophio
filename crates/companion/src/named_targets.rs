// SPDX-License-Identifier: Apache-2.0
//! Installation evidence for named tools without a managed integration.
use async_trait::async_trait;
use ports::{HarnessAdapter, HarnessError, RunSpec, RunStarted};
use protocol::capability::*;
use std::{ffi::OsString, path::PathBuf, process::Stdio, time::Duration};

#[derive(Debug)]
pub(crate) struct NamedTarget {
    kind: HarnessKind,
    search_path: OsString,
}
impl NamedTarget {
    pub fn new(kind: HarnessKind, search_path: OsString) -> Self {
        Self { kind, search_path }
    }
    fn name(&self) -> (&str, &str) {
        match self.kind {
            HarnessKind::Hermes => ("hermes", "Hermes"),
            HarnessKind::OpenClaw => ("openclaw", "OpenClaw"),
            HarnessKind::Pi => ("pi", "Pi"),
            _ => ("", "Unknown agent"),
        }
    }
    fn locate(&self) -> Option<PathBuf> {
        let (name, _) = self.name();
        if name.is_empty() {
            return None;
        }
        let executables = if cfg!(windows) {
            ["exe", "com", "cmd", "bat"]
                .map(|ext| format!("{name}.{ext}"))
                .to_vec()
        } else {
            vec![name.to_owned()]
        };
        std::env::split_paths(&self.search_path)
            .filter(|p| p.is_absolute())
            .flat_map(|dir| executables.iter().map(move |name| dir.join(name)))
            .find(|path| {
                path.metadata().is_ok_and(|m| {
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        m.is_file() && m.permissions().mode() & 0o111 != 0
                    }
                    #[cfg(not(unix))]
                    {
                        m.is_file()
                    }
                })
            })
    }
}
#[async_trait]
impl HarnessAdapter for NamedTarget {
    fn kind(&self) -> HarnessKind {
        self.kind
    }
    async fn probe(&self) -> HarnessInfo {
        let path = self.locate();
        let version = if let Some(path) = &path {
            let mut cmd = tokio::process::Command::new(path);
            cmd.arg("--version")
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .kill_on_drop(true);
            #[cfg(windows)]
            cmd.creation_flags(0x08000000);
            version(&mut cmd).await
        } else {
            None
        };
        let unavailable = Availability::Unsupported {
            reason: if path.is_some() {
                "Installed, not supported by this version of the app yet"
            } else {
                "Not found on this computer"
            }
            .into(),
        };
        HarnessInfo {
            usage_limit: None,
            kind: self.kind,
            display_name: self.name().1.into(),
            native_version: version,
            integration: IntegrationLevel::NotIntegrated,
            installed: if path.is_some() {
                Availability::Available
            } else {
                unavailable.clone()
            },
            account: AccountReadiness::Unknown {
                reason: "Sign-in is not checked for an unsupported agent".into(),
            },
            models: Vec::new(),
            efforts: Vec::new(),
            modes: Vec::new(),
            catalog: None,
            capabilities: HarnessCapabilities {
                start_run: unavailable.clone(),
                steer_running: unavailable.clone(),
                interrupt: unavailable.clone(),
                native_approvals: unavailable.clone(),
                resume_session: unavailable.clone(),
                child_agents: unavailable.clone(),
                computer_use_delegation: unavailable.clone(),
                model_selection: unavailable.clone(),
                cancel_queued_message: Some(unavailable.clone()),
                effort_selection: unavailable,
            },
        }
    }
    async fn start_run(&self, _: RunSpec) -> Result<RunStarted, HarnessError> {
        Err(HarnessError::Unsupported(
            "This agent has no managed integration".into(),
        ))
    }
}

async fn version(cmd: &mut tokio::process::Command) -> Option<String> {
    use tokio::io::AsyncReadExt;
    let mut child = cmd.spawn().ok()?;
    let result = tokio::time::timeout(Duration::from_secs(3), async {
        let mut bytes = Vec::new();
        child
            .stdout
            .take()?
            .take(1025)
            .read_to_end(&mut bytes)
            .await
            .ok()?;
        if bytes.len() > 1024 || !child.wait().await.ok()?.success() {
            return None;
        }
        String::from_utf8(bytes)
            .ok()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
    })
    .await
    .ok()
    .flatten();
    let _ = child.kill().await;
    let _ = child.wait().await;
    result
}
#[cfg(all(test, unix))]
mod tests {
    use super::*;
    #[tokio::test]
    async fn every_named_target_is_unselectable_found_or_missing() {
        let dir = tempfile::tempdir().unwrap();
        for (kind, binary) in [
            (HarnessKind::Hermes, "hermes"),
            (HarnessKind::OpenClaw, "openclaw"),
            (HarnessKind::Pi, "pi"),
        ] {
            let target = NamedTarget::new(kind, dir.path().into());
            let absent = target.probe().await;
            assert!(!absent.installed.is_available());
            assert_eq!(absent.integration, IntegrationLevel::NotIntegrated);
            test_support::executable(dir.path().join(binary), "#!/bin/sh\n[ \"$#\" = 1 ] && [ \"$1\" = --version ] || exit 1\nprintf 'fixture 1.0'\n").unwrap();
            let present = target.probe().await;
            assert!(present.installed.is_available());
            assert_eq!(present.native_version.as_deref(), Some("fixture 1.0"));
            assert_eq!(present.integration, IntegrationLevel::NotIntegrated);
            assert!(!present.capabilities.start_run.is_available());
            assert!(present.models.is_empty());
        }
    }
}
