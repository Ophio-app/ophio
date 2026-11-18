// SPDX-License-Identifier: Apache-2.0
//! Conversation isolation on Linux and on a Mac. Windows has none yet, and
//! says so. See the crate README for deployment and limits.
use ports::HarnessError;
#[cfg(target_os = "linux")]
use ports::sandbox::{Sandbox, SandboxSpec, Sandboxes};
#[cfg(target_os = "linux")]
use protocol::capability::Availability;
use std::path::PathBuf;
#[cfg(target_os = "linux")]
use std::sync::{Arc, OnceLock};

#[cfg(target_os = "linux")]
mod disk;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod history;
#[cfg(target_os = "linux")]
pub use disk::DiskBudget;
#[cfg(target_os = "linux")]
mod linux;
/// Host runtime trees mounted by explicit-grant Linux sandboxes.
#[cfg(target_os = "linux")]
pub use linux::runtime_paths;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::{Seatbelt, runtime_paths};
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn runtime_paths() -> &'static [&'static str] {
    &[]
}
/// Files mounted for an executable, including resolved aliases and wrapper targets.
pub fn program_paths(
    program: &std::path::Path,
    home: &std::path::Path,
) -> std::io::Result<Vec<PathBuf>> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    return program::files(program, home);
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = home;
        Ok(vec![program.canonicalize()?])
    }
}
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod own;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod program;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub mod proxy;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub mod relay;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod setup;
#[cfg(target_os = "linux")]
pub mod shim;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod sign_in;
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[allow(unsafe_code)]
mod sys;

pub mod checks;

mod on_windows;
pub use on_windows::NoWindowsSandbox;

/// Install the shim alongside the ophio program, or supply its absolute path.
#[cfg(target_os = "linux")]
#[derive(Debug)]
pub struct Bubblewrap {
    shim: PathBuf,
    probe: OnceLock<Availability>,
    disk: Option<Arc<DiskBudget>>,
}
#[cfg(target_os = "linux")]
impl Bubblewrap {
    pub fn new(shim: impl Into<PathBuf>) -> Self {
        Self {
            shim: shim.into(),
            probe: OnceLock::new(),
            disk: None,
        }
    }
    pub fn with_disk_budget(mut self, budget: Arc<DiskBudget>) -> Self {
        self.disk = Some(budget);
        self
    }
}
#[cfg(target_os = "linux")]
impl Default for Bubblewrap {
    fn default() -> Self {
        let path = std::env::current_exe()
            .unwrap_or_default()
            .with_file_name("ophio-sandbox-shim");
        Self::new(path)
    }
}
#[cfg(target_os = "linux")]
#[async_trait::async_trait]
impl Sandboxes for Bubblewrap {
    fn availability(&self) -> Availability {
        self.probe.get_or_init(|| probe(&self.shim)).clone()
    }
    fn create(&self, spec: SandboxSpec) -> Result<Arc<dyn Sandbox>, HarnessError> {
        if spec.reach.unrestricted() {
            return Err(error("unrestricted runs must not create a sandbox"));
        }
        if !self.availability().is_available() {
            return Err(error(format!(
                "sandbox unavailable: {:?}",
                self.availability()
            )));
        }
        Ok(Arc::new(linux::LinuxSandbox::with_disk_budget(
            spec,
            self.shim.clone(),
            self.disk.clone(),
        )?))
    }
}
fn error(message: impl ToString) -> HarnessError {
    HarnessError::SpawnFailed(message.to_string())
}
/// What to do when bubblewrap is installed but cannot start a sandbox. The
/// AppArmor switch is a troubleshooting hint, not proof of what blocked it.
#[cfg(target_os = "linux")]
fn blocked_remedy(said: &str, apparmor_restricts: bool) -> String {
    let hint = if apparmor_restricts {
        " On Ubuntu, its AppArmor security setting is the usual cause."
    } else {
        ""
    };
    format!(
        "Something on this computer stops bubblewrap, which Øphio runs limited chats in, from starting (it said: {said}).{hint} See \"Chat access controls\" in the INSTALL.md that came with Øphio, then restart Øphio as described there. Until then, chats can't be limited to a project or kept off the internet."
    )
}
#[cfg(target_os = "linux")]
fn probe(shim: &std::path::Path) -> Availability {
    use std::os::unix::fs::PermissionsExt;
    if !shim
        .metadata()
        .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    {
        return Availability::Unsupported {
            reason: "Install ophio-sandbox-shim alongside the ophio program".into(),
        };
    }
    match std::process::Command::new("/usr/bin/bwrap")
        .args([
            "--unshare-user",
            "--unshare-pid",
            "--unshare-net",
            "--ro-bind",
            "/",
            "/",
            "/usr/bin/true",
        ])
        .output()
    {
        Ok(output) if output.status.success() => Availability::Available,
        Ok(output) => Availability::PermissionMissing {
            remedy: blocked_remedy(
                String::from_utf8_lossy(&output.stderr).trim(),
                std::fs::read_to_string("/proc/sys/kernel/apparmor_restrict_unprivileged_userns")
                    .is_ok_and(|v| v.trim() == "1"),
            ),
        },
        Err(e) => Availability::Unsupported {
            reason: format!(
                "Install bubblewrap 0.9 or newer from your distribution's bubblewrap package; Øphio runs it from /usr/bin/bwrap ({e})"
            ),
        },
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::blocked_remedy;

    #[test]
    fn blocked_remedy_keeps_the_error_with_or_without_apparmor() {
        let said = "bwrap: Creating new namespace failed: No space left on device";
        for restricted in [false, true] {
            let remedy = blocked_remedy(said, restricted);
            assert!(remedy.contains(said));
            assert!(remedy.contains("restart Øphio as described there"));
            assert!(!remedy.contains("systemctl"));
            assert_eq!(
                remedy.contains("On Ubuntu, its AppArmor security setting is the usual cause."),
                restricted
            );
        }
    }

    #[test]
    fn blocked_remedy_treats_apparmor_as_a_possible_cause() {
        let remedy = blocked_remedy("Operation not permitted", true);
        assert!(remedy.contains("is the usual cause"));
        assert!(!remedy.contains("Ubuntu's AppArmor stops"));
    }
}
