// SPDX-License-Identifier: Apache-2.0
//! Start one approved command and retain enough identity to reconcile it later.
use sandbox::checks::CheckSandboxSpec;
pub use sandbox::checks::SandboxLevel;
use serde::{Deserialize, Serialize};
use std::{
    collections::VecDeque,
    io,
    path::PathBuf,
    process::Stdio,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    sync::watch,
    task::JoinHandle,
};

#[cfg(unix)]
#[path = "native_unix.rs"]
mod native;
#[cfg(windows)]
#[path = "native_windows.rs"]
mod native;

/// Store this whole value with the check job before waiting for its result.
/// The start stamp and boot identity prevent signalling a reused process id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessIdentity {
    pub pid: u32,
    pub start_time: u64,
    pub sandbox_unit: Option<String>,
    pub project: PathBuf,
    pub boot_id: String,
}
/// Whatever was known when startup failed. Never signal a PID from this
/// record alone; incomplete identities must go through reconciliation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartialProcessIdentity {
    pub pid: Option<u32>,
    pub start_time: Option<u64>,
    pub sandbox_unit: Option<String>,
    pub project: PathBuf,
    pub boot_id: Option<String>,
}

/// Whether a failed start can safely release the project's reservation.
#[derive(Debug, Clone, Serialize, Deserialize, thiserror::Error)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum StartError {
    /// The deadline expired before any sandbox launcher or project command
    /// was spawned. A host capability probe may have run; no check identity
    /// exists and there are no check processes to reconcile or stop.
    #[error("The check timed out before its sandbox could start")]
    TimedOutBeforeStart { duration_ms: u64 },
    /// Nothing was spawned, or every spawned process was confirmed stopped.
    #[error("{reason}")]
    NotStarted { reason: String },
    /// Startup spawned something whose complete end could not be confirmed.
    #[error("{reason}")]
    Uncertain {
        reason: String,
        identity: Box<PartialProcessIdentity>,
    },
}
impl From<io::Error> for StartError {
    fn from(error: io::Error) -> Self {
        Self::NotStarted {
            reason: error.to_string(),
        }
    }
}
impl StartError {
    pub(crate) fn after_cleanup(
        reason: impl std::fmt::Display,
        identity: PartialProcessIdentity,
        stop: StopStatus,
    ) -> Self {
        match stop {
            StopStatus::Confirmed => Self::NotStarted {
                reason: reason.to_string(),
            },
            StopStatus::Uncertain { reason: cleanup } => Self::Uncertain {
                reason: format!("{reason}. Ophio could not confirm cleanup: {cleanup}"),
                identity: Box::new(identity),
            },
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ProcessState {
    Running,
    Stopped,
    Uncertain { reason: String },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum StopStatus {
    Confirmed,
    Uncertain { reason: String },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum CheckOutcome {
    Passed,
    Failed,
    TimedOut,
    Cancelled,
    CouldNotRun { reason: String },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckOutput {
    pub outcome: CheckOutcome,
    pub duration_ms: u64,
    pub exit_code: Option<i32>,
    pub output_tail: String,
    pub output_bytes: u64,
    pub stop: StopStatus,
    pub sandbox_level: SandboxLevel,
}
#[derive(Debug, Clone)]
pub struct CheckRequest {
    pub project: PathBuf,
    pub data: PathBuf,
    pub command: String,
    pub timeout: Duration,
    pub read_toolchains: Vec<PathBuf>,
    /// Must be the intersection of the approved check's and task's network grants.
    pub network: bool,
    /// True unless every task grant allowed the agent to run unsandboxed.
    pub task_network_restricted: bool,
    /// Restricted tasks may additionally allow private networks. On macOS,
    /// direct loopback follows the task's internet/local permission and is
    /// disabled for offline checks. The proxy still rejects host loopback.
    pub local_network: bool,
}
impl CheckRequest {
    pub fn new(
        project: impl Into<PathBuf>,
        data: impl Into<PathBuf>,
        command: impl Into<String>,
        timeout: Duration,
    ) -> Self {
        Self {
            project: project.into(),
            data: data.into(),
            command: command.into(),
            timeout,
            read_toolchains: vec![],
            network: false,
            task_network_restricted: true,
            local_network: false,
        }
    }
}
#[derive(Debug, Clone, Copy, Default)]
pub struct RunnerOptions {
    /// Used to exercise the weaker fallback, or when the host has no user manager.
    pub without_systemd: bool,
}
#[derive(Debug, Clone)]
pub struct CheckCancellation(watch::Sender<bool>);
impl CheckCancellation {
    pub fn cancel(&self) {
        self.0.send_replace(true);
    }
}

/// Dropping the handle requests cancellation. Waiting consumes the handle;
/// the monitoring task owns the sandbox until cleanup has been attempted.
#[derive(Debug)]
pub struct RunningCheck {
    identity: ProcessIdentity,
    level: SandboxLevel,
    cancellation: CheckCancellation,
    completion: Option<JoinHandle<io::Result<CheckOutput>>>,
}
impl RunningCheck {
    pub fn identity(&self) -> &ProcessIdentity {
        &self.identity
    }
    pub fn sandbox_level(&self) -> &SandboxLevel {
        &self.level
    }
    pub fn cancellation(&self) -> CheckCancellation {
        self.cancellation.clone()
    }
    pub fn cancel(&self) {
        self.cancellation.cancel();
    }
    pub async fn wait(mut self) -> io::Result<CheckOutput> {
        self.completion
            .take()
            .expect("check completion exists")
            .await
            .map_err(io::Error::other)?
    }
    pub async fn cancel_and_wait(self) -> io::Result<CheckOutput> {
        self.cancel();
        self.wait().await
    }
}
impl Drop for RunningCheck {
    fn drop(&mut self) {
        if self.completion.is_some() {
            self.cancel();
        }
    }
}

pub async fn start(request: CheckRequest) -> Result<RunningCheck, StartError> {
    start_with_options(request, RunnerOptions::default()).await
}
pub async fn start_with_options(
    request: CheckRequest,
    options: RunnerOptions,
) -> Result<RunningCheck, StartError> {
    start_inner(request, options, false, None).await
}

async fn start_inner(
    request: CheckRequest,
    options: RunnerOptions,
    fail_after_spawn: bool,
    systemd_launcher: Option<&str>,
) -> Result<RunningCheck, StartError> {
    let began = Instant::now();
    let boot = native::boot_id()?;
    if request.timeout.is_zero() || request.timeout > Duration::from_secs(3600) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "A check's time limit must be greater than zero and no longer than one hour",
        )
        .into());
    }
    let deadline = tokio::time::Instant::from_std(began) + request.timeout;
    let mut sandbox = sandbox::checks::prepare(
        CheckSandboxSpec {
            project: request.project,
            state: request.data,
            read: request.read_toolchains,
            network: request.network,
            network_restricted: request.task_network_restricted,
            local_network: request.local_network,
        },
        &request.command,
    )?;
    let level = sandbox.level().clone();
    let startup_marker = sandbox.startup_marker().map(str::to_owned);
    let std_command = sandbox.take_command()?;
    let project = std_command
        .get_current_dir()
        .ok_or_else(|| io::Error::other("A check needs a project folder"))?
        .to_owned();
    let mut command: tokio::process::Command = std_command.into();
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let name = format!("ophio-check-{}", uuid::Uuid::new_v4().simple());
    #[cfg(unix)]
    let (mut child, guard) = native::spawn(
        &mut command,
        &name,
        options.without_systemd,
        systemd_launcher,
        deadline,
        began,
        None,
    )
    .await?;
    #[cfg(windows)]
    let (mut child, guard) = {
        let _ = (options, systemd_launcher);
        native::spawn(&mut command, &name).await?
    };
    let mut partial = PartialProcessIdentity {
        pid: child.id(),
        start_time: None,
        #[cfg(unix)]
        sandbox_unit: guard.unit().map(str::to_owned),
        #[cfg(windows)]
        sandbox_unit: Some(name),
        project,
        boot_id: Some(boot),
    };
    // Unix spawn already recorded this while the child was live. A second
    // lookup can lose a normally exited Mac leader's otherwise valid identity.
    #[cfg(unix)]
    let recorded = Ok(guard.start_time());
    #[cfg(windows)]
    let recorded = match partial.pid {
        Some(pid) => native::stamp(pid).and_then(|stamp| {
            stamp.ok_or_else(|| io::Error::other("The check's process identity is unavailable"))
        }),
        None => Err(io::Error::other("The check did not return a process id")),
    };
    if let Ok(start) = recorded {
        partial.start_time = Some(start);
    }
    let recorded = if fail_after_spawn {
        Err(io::Error::other(
            "Simulated failure while recording a started check",
        ))
    } else {
        recorded
    };
    let start = match recorded {
        Ok(start) => start,
        Err(error) => {
            let mut stop = guard.stop().await;
            if !matches!(
                tokio::time::timeout(Duration::from_secs(4), child.wait()).await,
                Ok(Ok(_))
            ) {
                stop = StopStatus::Uncertain {
                    reason: "The check's launcher did not exit after failed startup".into(),
                };
            }
            return Err(StartError::after_cleanup(error, partial, stop));
        }
    };
    partial.start_time = Some(start);
    let identity = ProcessIdentity {
        pid: partial.pid.expect("recorded child id"),
        start_time: start,
        sandbox_unit: partial.sandbox_unit,
        project: partial.project,
        boot_id: partial.boot_id.expect("recorded boot identity"),
    };
    let (cancel, mut cancelled) = watch::channel(false);
    let cancellation = CheckCancellation(cancel);
    let tail = Arc::new(Mutex::new(Tail::default()));
    // On contained platforms stdout belongs exclusively to the trusted startup
    // wrapper. Before the check runs, that wrapper redirects its stdout to
    // stderr, so project-controlled output can never forge a startup signal.
    let mut stdout_pipe = child.stdout.take().expect("piped stdout");
    let stderr = tokio::spawn(drain(
        child.stderr.take().expect("piped stderr"),
        tail.clone(),
    ));
    let mut startup_timed_out = false;
    let stdout = if let Some(marker) = startup_marker {
        let started = if tokio::time::Instant::now() >= deadline {
            None
        } else {
            tokio::time::timeout_at(
                deadline,
                read_startup_marker(&mut stdout_pipe, marker.as_bytes()),
            )
            .await
            .ok()
        };
        // An expired budget is a check timeout, not a failed startup. Leave
        // cleanup to the same owned monitor that handles execution timeouts.
        startup_timed_out = started.is_none();
        if let Some(Err(error)) = started {
            let mut stop = guard.stop().await;
            if !matches!(
                tokio::time::timeout(Duration::from_secs(4), child.wait()).await,
                Ok(Ok(_))
            ) {
                stop = StopStatus::Uncertain {
                    reason: "The failed check's launcher did not confirm its exit".into(),
                };
            }
            finish_reader(stderr).await;
            let diagnostics = tail.lock().unwrap().text();
            let reason = if diagnostics.trim().is_empty() {
                format!("The computer could not start the check's sandbox: {error}")
            } else {
                format!(
                    "The computer could not start the check's sandbox: {}",
                    diagnostics.trim()
                )
            };
            return Err(StartError::after_cleanup(
                reason,
                partial_from(&identity),
                stop,
            ));
        }
        // No project command inherits this pipe. Keep draining so the trusted
        // launcher cannot be blocked by any later status information.
        tokio::spawn(drain(stdout_pipe, Arc::new(Mutex::new(Tail::default()))))
    } else {
        tokio::spawn(drain(stdout_pipe, tail.clone()))
    };
    let result_level = level.clone();
    let completion = tokio::spawn(async move {
        let _sandbox = sandbox;
        enum End {
            Exit(io::Result<()>),
            Timeout,
            Cancel,
        }
        let end = if startup_timed_out || tokio::time::Instant::now() >= deadline {
            End::Timeout
        } else {
            tokio::select! {
                result=native::exited(&mut child)=>End::Exit(result),
                _=tokio::time::sleep_until(deadline)=>End::Timeout,
                _=async { loop { if *cancelled.borrow_and_update() || cancelled.changed().await.is_err() {break;} } }=>End::Cancel,
            }
        };
        let mut stop = guard.stop().await;
        // Reaping follows tree cleanup, keeping the process group's identity
        // pinned on Unix until it is no longer signalled.
        let status = tokio::time::timeout(Duration::from_secs(4), child.wait()).await;
        if !matches!(&status, Ok(Ok(_))) {
            stop = StopStatus::Uncertain {
                reason: "The check's launcher did not confirm its exit after cleanup".into(),
            };
        }
        for reader in [stdout, stderr] {
            finish_reader(reader).await;
        }
        let code = match status {
            Ok(Ok(status)) => status.code(),
            _ => None,
        };
        let outcome = match end {
            End::Timeout => CheckOutcome::TimedOut,
            End::Cancel => CheckOutcome::Cancelled,
            End::Exit(Err(e)) => CheckOutcome::CouldNotRun {
                reason: e.to_string(),
            },
            End::Exit(Ok(())) if code == Some(0) => CheckOutcome::Passed,
            End::Exit(Ok(())) => CheckOutcome::Failed,
        };
        let tail = tail.lock().unwrap();
        let mut text = tail.text();
        if text.len() > OUTPUT_LIMIT {
            let mut at = text.len() - OUTPUT_LIMIT;
            while !text.is_char_boundary(at) {
                at += 1;
            }
            text = text[at..].into();
        }
        if !request.network {
            let note = if cfg!(windows) {
                "[Ophio] Network access was requested off, but Windows cannot enforce a network limit.\n"
            } else {
                "[Ophio] Network access is off for this check.\n"
            };
            let available = OUTPUT_LIMIT.saturating_sub(note.len());
            if text.len() > available {
                let mut at = text.len() - available;
                while !text.is_char_boundary(at) {
                    at += 1;
                }
                text = text[at..].into();
            }
            text.insert_str(0, note);
        }
        Ok(CheckOutput {
            outcome,
            duration_ms: began.elapsed().as_millis().min(u64::MAX as u128) as u64,
            exit_code: code,
            output_tail: text,
            output_bytes: tail.total,
            stop,
            sandbox_level: result_level,
        })
    });
    Ok(RunningCheck {
        identity,
        level,
        cancellation,
        completion: Some(completion),
    })
}

/// A missing leader alone is never proof that a detached descendant stopped.
pub async fn inspect(identity: &ProcessIdentity) -> ProcessState {
    native::inspect(identity).await
}
pub async fn stop(identity: &ProcessIdentity) -> StopStatus {
    native::stop(identity).await
}

fn partial_from(identity: &ProcessIdentity) -> PartialProcessIdentity {
    PartialProcessIdentity {
        pid: Some(identity.pid),
        start_time: Some(identity.start_time),
        sandbox_unit: identity.sandbox_unit.clone(),
        project: identity.project.clone(),
        boot_id: Some(identity.boot_id.clone()),
    }
}

async fn read_startup_marker(
    reader: &mut (impl AsyncRead + Unpin),
    marker: &[u8],
) -> io::Result<()> {
    let mut actual = vec![0; marker.len()];
    reader.read_exact(&mut actual).await?;
    if actual != marker {
        return Err(io::Error::other("The sandbox did not confirm startup"));
    }
    Ok(())
}

async fn finish_reader(mut reader: JoinHandle<()>) {
    if tokio::time::timeout(Duration::from_millis(250), &mut reader)
        .await
        .is_err()
    {
        reader.abort();
        let _ = reader.await;
    }
}

pub const OUTPUT_LIMIT: usize = 8192;
#[derive(Debug, Default)]
struct Tail {
    bytes: VecDeque<u8>,
    total: u64,
}
impl Tail {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.bytes.iter().copied().collect::<Vec<_>>()).into_owned()
    }
    fn push(&mut self, bytes: &[u8]) {
        self.total = self.total.saturating_add(bytes.len() as u64);
        if bytes.len() >= OUTPUT_LIMIT {
            self.bytes.clear();
            self.bytes.extend(&bytes[bytes.len() - OUTPUT_LIMIT..]);
            return;
        }
        let remove = (self.bytes.len() + bytes.len()).saturating_sub(OUTPUT_LIMIT);
        self.bytes.drain(..remove);
        self.bytes.extend(bytes);
    }
}
async fn drain(mut reader: impl AsyncRead + Unpin, tail: Arc<Mutex<Tail>>) {
    let mut bytes = [0; 4096];
    loop {
        match reader.read(&mut bytes).await {
            Ok(0) | Err(_) => break,
            Ok(n) => tail.lock().unwrap().push(&bytes[..n]),
        }
    }
}
#[cfg(all(test, target_os = "linux"))]
#[path = "runner_startup_tests.rs"]
mod startup_tests;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn output_memory_is_bounded_and_keeps_tail() {
        let mut tail = Tail::default();
        for _ in 0..10000 {
            tail.push(&[b'x'; 4096]);
        }
        tail.push(b"last line");
        assert_eq!(tail.bytes.len(), OUTPUT_LIMIT);
        assert_eq!(tail.total, 40_960_009);
        assert!(
            tail.bytes
                .iter()
                .copied()
                .collect::<Vec<_>>()
                .ends_with(b"last line")
        );
    }
    #[tokio::test]
    async fn startup_requires_the_complete_private_channel_marker() {
        let marker = b"OPHIO_CHECK_STARTED\n";
        let mut complete = &marker[..];
        read_startup_marker(&mut complete, marker).await.unwrap();
        for bytes in [
            b"OPHIO_CHECK_STARTED".as_slice(),
            b"bwrap: fixture output".as_slice(),
            b"sandbox-exec: fixture output".as_slice(),
            b"".as_slice(),
        ] {
            let mut reader = bytes;
            assert!(read_startup_marker(&mut reader, marker).await.is_err());
        }
    }

    #[tokio::test]
    async fn invalid_preparation_is_guaranteed_not_started() {
        let project = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let absent = data.path().join("must-not-exist");
        let request = CheckRequest::new(
            project.path().canonicalize().unwrap(),
            &absent,
            "echo must-not-run",
            Duration::ZERO,
        );
        assert!(matches!(
            start(request).await,
            Err(StartError::NotStarted { .. })
        ));
        assert!(!absent.exists());
    }

    #[test]
    fn cleanup_certainty_controls_start_error() {
        let partial = PartialProcessIdentity {
            pid: Some(42),
            start_time: None,
            sandbox_unit: Some("job".into()),
            project: "project".into(),
            boot_id: Some("boot".into()),
        };
        assert!(matches!(
            StartError::after_cleanup("failed", partial.clone(), StopStatus::Confirmed),
            StartError::NotStarted { .. }
        ));
        let error = StartError::after_cleanup(
            "failed",
            partial.clone(),
            StopStatus::Uncertain {
                reason: "could not count descendants".into(),
            },
        );
        let error: StartError =
            serde_json::from_str(&serde_json::to_string(&error).unwrap()).unwrap();
        let StartError::Uncertain { identity, reason } = error else {
            panic!("uncertainty was lost")
        };
        assert_eq!(*identity, partial);
        assert!(reason.contains("could not count descendants"));
    }

    async fn forced_start_failure(without_systemd: bool) -> StartError {
        let project = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let command = if cfg!(windows) {
            "ping -n 30 127.0.0.1 >nul"
        } else {
            "sleep 30"
        };
        let request = CheckRequest::new(
            project.path().canonicalize().unwrap(),
            data.path().canonicalize().unwrap(),
            command,
            Duration::from_secs(30),
        );
        start_inner(request, RunnerOptions { without_systemd }, true, None)
            .await
            .unwrap_err()
    }

    #[tokio::test]
    #[cfg_attr(target_os = "linux", ignore = "live namespaces require a user service")]
    async fn forced_post_spawn_failure_preserves_uncertainty() {
        let error = forced_start_failure(true).await;
        #[cfg(unix)]
        {
            let StartError::Uncertain { identity, reason } = error else {
                panic!("group fallback claimed full cleanup")
            };
            assert!(identity.pid.is_some());
            assert!(identity.start_time.is_some());
            assert!(identity.boot_id.is_some());
            assert!(identity.sandbox_unit.is_none());
            assert!(reason.contains("Simulated failure"));
            assert_eq!(native::stamp(identity.pid.unwrap()).unwrap(), None);
        }
        #[cfg(windows)]
        assert!(matches!(error, StartError::NotStarted { .. }));
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    #[ignore = "live namespaces require a user service and systemd"]
    async fn forced_post_spawn_failure_is_not_started_after_confirmed_tree_exit() {
        assert!(matches!(
            forced_start_failure(false).await,
            StartError::NotStarted { .. }
        ));
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    #[ignore = "Requires a systemd user manager"]
    async fn systemd_launcher_rejection_is_not_started_with_its_real_message() {
        use std::os::unix::fs::PermissionsExt;
        let project = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let launcher = data.path().join("reject-systemd-run");
        std::fs::write(
            &launcher,
            "#!/bin/sh\nprintf 'Unknown assignment: deliberate launcher refusal\\n' >&2\nexit 1\n",
        )
        .unwrap();
        std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o700)).unwrap();
        let request = CheckRequest::new(
            project.path(),
            data.path(),
            "touch must-not-run",
            Duration::from_secs(20),
        );
        let result = start_inner(
            request,
            RunnerOptions::default(),
            false,
            Some(launcher.to_str().unwrap()),
        )
        .await;
        let error = match result {
            Err(error) => error,
            Ok(run) => panic!(
                "Launcher rejection was reported as a check: {:?}",
                run.wait().await
            ),
        };
        let StartError::NotStarted { reason } = error else {
            panic!("A rejected unit should have confirmed cleanup: {error:?}")
        };
        assert!(
            reason.contains("Unknown assignment: deliberate launcher refusal"),
            "{reason}"
        );
        assert!(!project.path().join("must-not-run").exists());
    }

    #[test]
    fn identity_roundtrips_without_precision_loss() {
        let id = ProcessIdentity {
            pid: 42,
            start_time: u64::MAX,
            sandbox_unit: Some("ophio-check-123".into()),
            project: "project".into(),
            boot_id: "boot".into(),
        };
        assert_eq!(
            serde_json::from_str::<ProcessIdentity>(&serde_json::to_string(&id).unwrap()).unwrap(),
            id
        );
    }
}
