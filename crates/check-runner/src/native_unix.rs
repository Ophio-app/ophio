// SPDX-License-Identifier: Apache-2.0
use super::{PartialProcessIdentity, ProcessIdentity, ProcessState, StartError, StopStatus};
use rustix::process::{Pid, Signal, WaitId, WaitIdOptions};
use std::{io, path::PathBuf, time::Duration};
#[cfg(target_os = "linux")]
use std::{path::Path, process::Stdio};
use tokio::process::{Child, Command};

#[derive(Debug)]
pub(crate) struct Handle {
    pid: u32,
    start: u64,
    project: PathBuf,
    unit: Option<String>,
    boot: String,
}
impl Handle {
    pub(crate) fn unit(&self) -> Option<&str> {
        self.unit.as_deref()
    }
    pub(crate) fn start_time(&self) -> u64 {
        self.start
    }
    pub(crate) async fn stop(&self) -> StopStatus {
        // Only the live runner owns this handle, and it has not reaped its
        // child yet. That ownership pins the process-group id even when
        // macOS no longer exposes a start stamp for the zombie leader.
        // Stop the launcher even for systemd: a startup timeout may happen
        // before it creates its unit, and it must not create that unit later.
        // Recovery through a persisted identity still requires its stamp.
        let group = match pid(self.pid) {
            Ok(group) => group,
            Err(error) => return unconfirmed(error),
        };
        if let Err(error) = rustix::process::kill_process_group(group, Signal::KILL)
            && error != rustix::io::Errno::SRCH
        {
            return unconfirmed(io::Error::from(error));
        }
        stop(&ProcessIdentity {
            pid: self.pid,
            start_time: self.start,
            sandbox_unit: self.unit.clone(),
            project: self.project.clone(),
            boot_id: self.boot.clone(),
        })
        .await
    }
}
pub(crate) async fn spawn(
    command: &mut Command,
    name: &str,
    without_systemd: bool,
    systemd_launcher: Option<&str>,
    deadline: tokio::time::Instant,
    began: std::time::Instant,
    systemd_probe: Option<&str>,
) -> Result<(Child, Handle), StartError> {
    let boot = boot_id()?;
    let project = command
        .as_std()
        .get_current_dir()
        .ok_or_else(|| io::Error::other("A check needs a project folder"))?
        .to_owned();
    #[cfg(target_os = "linux")]
    let unit = if !without_systemd
        && tokio::time::timeout_at(
            deadline,
            systemd_available(systemd_probe.unwrap_or("/usr/bin/systemctl")),
        )
        .await
        .unwrap_or(false)
    {
        Some(name.to_owned())
    } else {
        None
    };
    #[cfg(not(target_os = "linux"))]
    let unit: Option<String> = {
        let _ = (
            name,
            without_systemd,
            systemd_launcher,
            deadline,
            systemd_probe,
        );
        None
    };
    if tokio::time::Instant::now() >= deadline {
        return Err(StartError::TimedOutBeforeStart {
            duration_ms: began.elapsed().as_millis().min(u64::MAX as u128) as u64,
        });
    }
    #[cfg(target_os = "linux")]
    let child = if unit.is_some() {
        // The user manager has its own environment. env -i is intentional:
        // clearing only systemd-run's environment would not clear the service's.
        let original = command.as_std();
        let mut wrapped = control(systemd_launcher.unwrap_or("/usr/bin/systemd-run"));
        wrapped
            .args([
                "--user",
                "--quiet",
                "--wait",
                "--pipe",
                "--collect",
                "--service-type=exec",
            ])
            .arg(format!("--unit={name}"))
            .args([
                // The default main-process exit policy also stops remaining
                // group members, and works on systemd releases before 250.
                "--property=KillMode=control-group",
                "--property=TimeoutStopSec=3s",
                "--property=SendSIGKILL=yes",
            ])
            .arg("--working-directory")
            .arg(&project)
            .arg("--")
            .arg("/usr/bin/env")
            .arg("-i");
        for (key, value) in original.get_envs() {
            if let Some(value) = value {
                let mut item = key.to_os_string();
                item.push("=");
                item.push(value);
                wrapped.arg(item);
            }
        }
        wrapped
            .arg(original.get_program())
            .args(original.get_args())
            .current_dir(&project)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .process_group(0);
        containment::inherit::start(|| wrapped.spawn())?
    } else {
        command.process_group(0);
        containment::inherit::start(|| command.spawn())?
    };
    #[cfg(not(target_os = "linux"))]
    let child = {
        command.process_group(0);
        containment::inherit::start(|| command.spawn())?
    };
    let mut child = child;
    let pid = match child.id() {
        Some(pid) => pid,
        None => {
            return Err(StartError::Uncertain {
                reason: "A started check did not return a process id".into(),
                identity: Box::new(PartialProcessIdentity {
                    pid: None,
                    start_time: None,
                    sandbox_unit: unit,
                    project,
                    boot_id: Some(boot),
                }),
            });
        }
    };
    let start = match stamp(pid) {
        Ok(Some(start)) => start,
        result => {
            // This owned, unreaped child's id cannot yet have been reused.
            // Kill its group even when reading the persistent stamp failed.
            if let Ok(group) = self::pid(pid) {
                let _ = rustix::process::kill_process_group(group, Signal::KILL);
            }
            let _ = child.start_kill();
            let partial = PartialProcessIdentity {
                pid: Some(pid),
                start_time: None,
                sandbox_unit: unit.clone(),
                project: project.clone(),
                boot_id: Some(boot.clone()),
            };
            let fallback = ProcessIdentity {
                pid,
                start_time: 0,
                sandbox_unit: unit,
                project,
                boot_id: boot,
            };
            let mut status = stop(&fallback).await;
            if !matches!(
                tokio::time::timeout(Duration::from_secs(4), child.wait()).await,
                Ok(Ok(_))
            ) {
                status = StopStatus::Uncertain {
                    reason: "The check's launcher did not confirm its exit after failed identity recording".into(),
                };
            }
            return Err(StartError::after_cleanup(
                format!("The check started but its start time could not be recorded ({result:?})"),
                partial,
                status,
            ));
        }
    };
    let handle = Handle {
        pid,
        start,
        project,
        unit,
        boot,
    };
    #[cfg(target_os = "linux")]
    if let Some(unit) = &handle.unit {
        loop {
            // Readiness and the private startup marker share the check's one
            // deadline. On expiry return the owned child for normal timeout
            // cleanup instead of turning a slow launch into a start error.
            let state = match tokio::time::timeout_at(deadline, unit_state(unit)).await {
                Ok(state) => state,
                Err(_) => break,
            };
            match state {
                Ok(UnitState::Running) => break,
                Ok(UnitState::Gone) => match has_exited(pid) {
                    Ok(true) => break,
                    Ok(false) => {}
                    Err(error) => return Err(abort_start(child, &handle, error.to_string()).await),
                },
                Err(_) => break,
            }
            if tokio::time::Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
    Ok((child, handle))
}
#[cfg(target_os = "linux")]
async fn abort_start(mut child: Child, handle: &Handle, reason: String) -> StartError {
    let mut status = handle.stop().await;
    if !matches!(
        tokio::time::timeout(Duration::from_secs(4), child.wait()).await,
        Ok(Ok(_))
    ) {
        status = StopStatus::Uncertain {
            reason: "The failed check's launcher did not exit".into(),
        };
    }
    StartError::after_cleanup(
        reason,
        PartialProcessIdentity {
            pid: Some(handle.pid),
            start_time: Some(handle.start),
            sandbox_unit: handle.unit.clone(),
            project: handle.project.clone(),
            boot_id: Some(handle.boot.clone()),
        },
        status,
    )
}
fn pid(value: u32) -> io::Result<Pid> {
    i32::try_from(value)
        .ok()
        .and_then(Pid::from_raw)
        .ok_or_else(|| io::Error::other("Invalid check process id"))
}
fn has_exited(value: u32) -> io::Result<bool> {
    rustix::process::waitid(
        WaitId::Pid(pid(value)?),
        WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
    )
    .map(|s| s.is_some())
    .map_err(io::Error::from)
}
pub(crate) async fn exited(child: &mut Child) -> io::Result<()> {
    let pid = child
        .id()
        .ok_or_else(|| io::Error::other("The check has no process id"))?;
    loop {
        if has_exited(pid)? {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}
#[cfg(target_os = "linux")]
pub(crate) fn stamp(pid: u32) -> io::Result<Option<u64>> {
    let text = match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(s) => s,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let (_, rest) = text
        .rsplit_once(')')
        .ok_or_else(|| io::Error::other("Invalid process start record"))?;
    rest.split_whitespace()
        .nth(19)
        .ok_or_else(|| io::Error::other("Missing process start record"))?
        .parse()
        .map(Some)
        .map_err(io::Error::other)
}
#[cfg(target_os = "linux")]
pub(crate) fn boot_id() -> io::Result<String> {
    Ok(std::fs::read_to_string("/proc/sys/kernel/random/boot_id")?
        .trim()
        .into())
}
#[cfg(target_os = "macos")]
pub(crate) use crate::macos_process::{boot_id, stamp};

pub(crate) async fn inspect(identity: &ProcessIdentity) -> ProcessState {
    match boot_id() {
        Ok(boot) if boot != identity.boot_id => return ProcessState::Stopped,
        Err(e) => return uncertain(e),
        _ => {}
    }
    #[cfg(target_os = "linux")]
    if let Some(unit) = &identity.sandbox_unit {
        return match unit_state(unit).await {
            Ok(UnitState::Running) => ProcessState::Running,
            Ok(UnitState::Gone) => ProcessState::Stopped,
            Err(e) => uncertain(e),
        };
    }
    match stamp(identity.pid) {
        Ok(Some(start)) if start == identity.start_time => ProcessState::Running,
        Err(e) => uncertain(e),
        _ => match project_processes(&identity.project) {
            Ok(left) => ProcessState::Uncertain {
                reason: format!(
                    "The check's leader is gone, but process groups cannot account for detached programs. {} program(s) still have this project as their working folder.",
                    left.len()
                ),
            },
            Err(e) => uncertain(e),
        },
    }
}
fn uncertain(error: impl std::fmt::Display) -> ProcessState {
    ProcessState::Uncertain {
        reason: format!("Ophio could not account for the check's processes: {error}"),
    }
}
fn unconfirmed(error: impl std::fmt::Display) -> StopStatus {
    StopStatus::Uncertain {
        reason: error.to_string(),
    }
}
pub(crate) async fn stop(identity: &ProcessIdentity) -> StopStatus {
    match boot_id() {
        Ok(boot) if boot != identity.boot_id => return StopStatus::Confirmed,
        Err(e) => return unconfirmed(e),
        _ => {}
    }
    #[cfg(target_os = "linux")]
    if let Some(unit) = &identity.sandbox_unit {
        if !valid_unit(unit) {
            return unconfirmed("The stored check unit name is invalid");
        }
        let status = tokio::time::timeout(
            Duration::from_secs(6),
            control("/usr/bin/systemctl")
                .args(["--user", "stop", unit])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status(),
        )
        .await;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(4);
        loop {
            match unit_state(unit).await {
                Ok(UnitState::Gone) => return StopStatus::Confirmed,
                Err(e) => return unconfirmed(e),
                _ => {}
            }
            if tokio::time::Instant::now() >= deadline {
                return unconfirmed(format!(
                    "The check's systemd unit has not stopped ({status:?})"
                ));
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }
    match stamp(identity.pid) {
        Ok(Some(start)) if start == identity.start_time => {
            let group = match pid(identity.pid) {
                Ok(pid) => pid,
                Err(e) => return unconfirmed(e),
            };
            if let Err(e) = rustix::process::kill_process_group(group, Signal::KILL)
                && e != rustix::io::Errno::SRCH
            {
                return unconfirmed(io::Error::from(e));
            }
        }
        Err(e) => return unconfirmed(e),
        _ => {}
    }
    // A detached process may change its cwd too. An empty scan is supporting
    // evidence, never proof that the process group contained the whole tree.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while tokio::time::Instant::now() < deadline {
        if !live_matching(identity).unwrap_or(true) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    match project_processes(&identity.project) {
        Ok(left) => unconfirmed(format!(
            "Ophio stopped the check's process group, but this system cannot confirm that detached programs ended. {} program(s) still have the project as their working folder.",
            left.len()
        )),
        Err(e) => unconfirmed(format!(
            "Ophio stopped the check's process group but could not inspect the project for detached programs: {e}"
        )),
    }
}
fn live_matching(identity: &ProcessIdentity) -> io::Result<bool> {
    if stamp(identity.pid)? != Some(identity.start_time) {
        return Ok(false);
    }
    #[cfg(target_os = "linux")]
    {
        let stat = std::fs::read_to_string(format!("/proc/{}/stat", identity.pid))?;
        Ok(!stat
            .rsplit_once(')')
            .is_some_and(|(_, rest)| rest.trim_start().starts_with('Z')))
    }
    #[cfg(not(target_os = "linux"))]
    Ok(true)
}
#[cfg(target_os = "linux")]
fn project_processes(project: &Path) -> io::Result<Vec<u32>> {
    use std::os::unix::fs::MetadataExt;
    let uid = std::fs::metadata("/proc/self")?.uid();
    let mut left = vec![];
    for entry in std::fs::read_dir("/proc")? {
        let entry = entry?;
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|s| s.parse::<u32>().ok())
        else {
            continue;
        };
        if pid == std::process::id() {
            continue;
        }
        let metadata = match entry.metadata() {
            Ok(m) => m,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        };
        if metadata.uid() != uid {
            continue;
        }
        match std::fs::read_link(entry.path().join("cwd")) {
            Ok(cwd) if cwd.starts_with(project) => left.push(pid),
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    Ok(left)
}
#[cfg(target_os = "macos")]
use crate::macos_process::project_processes;

#[cfg(target_os = "linux")]
fn control(program: &str) -> Command {
    let mut command = Command::new(program);
    command.env_clear().env("PATH", "/usr/bin:/bin");
    if let Ok(meta) = std::fs::metadata("/proc/self") {
        use std::os::unix::fs::MetadataExt;
        command.env("XDG_RUNTIME_DIR", format!("/run/user/{}", meta.uid()));
    }
    command.kill_on_drop(true);
    command
}
#[cfg(target_os = "linux")]
async fn systemd_available(program: &str) -> bool {
    matches!(tokio::time::timeout(Duration::from_secs(2),control(program).args(["--user","show-environment"]).stdout(Stdio::null()).stderr(Stdio::null()).status()).await,Ok(Ok(status)) if status.success())
}
#[cfg(target_os = "linux")]
fn valid_unit(unit: &str) -> bool {
    let unit = unit.strip_suffix(".service").unwrap_or(unit);
    unit.strip_prefix("ophio-check-")
        .is_some_and(|tail| tail.len() == 32 && tail.bytes().all(|b| b.is_ascii_hexdigit()))
}
#[cfg(target_os = "linux")]
#[derive(Debug)]
enum UnitState {
    Running,
    Gone,
}
#[cfg(target_os = "linux")]
async fn unit_state(unit: &str) -> io::Result<UnitState> {
    if !valid_unit(unit) {
        return Err(io::Error::other("Invalid stored check unit name"));
    }
    let output = tokio::time::timeout(
        Duration::from_secs(2),
        control("/usr/bin/systemctl")
            .args([
                "--user",
                "show",
                unit,
                "--property=LoadState",
                "--property=ActiveState",
                "--property=ControlGroup",
            ])
            .output(),
    )
    .await
    .map_err(|_| io::Error::other("The systemd user manager did not answer"))??;
    let text = String::from_utf8_lossy(&output.stdout);
    if text.lines().any(|s| s == "LoadState=not-found") {
        return Ok(UnitState::Gone);
    }
    if !output.status.success() {
        return Err(io::Error::other(
            "The systemd user manager could not inspect this check",
        ));
    }
    let cgroup = text.lines().find_map(|s| s.strip_prefix("ControlGroup="));
    let active = text.lines().find_map(|s| s.strip_prefix("ActiveState="));
    if matches!(active, Some("inactive" | "failed")) {
        // A failed/inactive unit alone is insufficient: check the kernel's
        // population count when a cgroup is still attached.
        if let Some(group) = cgroup.filter(|s| !s.is_empty()) {
            if !group.starts_with('/') || group.split('/').any(|p| p == "..") {
                return Err(io::Error::other("Invalid check cgroup path"));
            }
            match std::fs::read_to_string(
                Path::new("/sys/fs/cgroup")
                    .join(group.trim_start_matches('/'))
                    .join("cgroup.events"),
            ) {
                Ok(events) if events.lines().any(|s| s == "populated 0") => {
                    return Ok(UnitState::Gone);
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(UnitState::Gone),
                Ok(_) => return Ok(UnitState::Running),
                Err(e) => return Err(e),
            }
        }
        return Ok(UnitState::Gone);
    }
    Ok(UnitState::Running)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn self_stamp_is_stable() {
        let pid = std::process::id();
        assert!(stamp(pid).unwrap().is_some());
        assert_eq!(stamp(pid).unwrap(), stamp(pid).unwrap());
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn unit_names_cannot_target_other_services() {
        assert!(valid_unit("ophio-check-0123456789abcdef0123456789abcdef"));
        for bad in [
            "other.service",
            "ophio-check-*",
            "ophio-check-../other",
            "--all",
        ] {
            assert!(!valid_unit(bad));
        }
    }
}
