// SPDX-License-Identifier: Apache-2.0
use ports::HarnessError;
use serde_json::Value;
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader},
    process::Command,
};

pub(crate) const MAX_FRAME: usize = 32 * 1024 * 1024;
/// A prepared message leaves room in its frame for the identity added on send.
pub(crate) const MAX_MESSAGE: usize = MAX_FRAME - 64 * 1024;
pub(crate) struct Frames<R> {
    reader: BufReader<R>,
    buffer: Vec<u8>,
}
impl<R: AsyncRead + Unpin> Frames<R> {
    pub fn new(reader: R) -> Self {
        Self {
            reader: BufReader::new(reader),
            buffer: Vec::new(),
        }
    }
    /// Discard remaining stdout during EOF-driven cleanup, even if a frame
    /// was malformed or too large. A full pipe must not stall credential writes.
    pub async fn drain(&mut self) {
        let _ = tokio::io::copy(&mut self.reader, &mut tokio::io::sink()).await;
    }
    // Cancellation safe: partial frames remain in self, including select! losers.
    pub async fn next(&mut self) -> Result<Option<Vec<u8>>, HarnessError> {
        loop {
            let buf = self.reader.fill_buf().await?;
            if buf.is_empty() {
                return if self.buffer.is_empty() {
                    Ok(None)
                } else {
                    Err(HarnessError::Protocol("truncated native frame".into()))
                };
            }
            let end = buf.iter().position(|b| *b == b'\n');
            let count = end.map_or(buf.len(), |n| n + 1);
            if self.buffer.len() + count > MAX_FRAME {
                return Err(HarnessError::Protocol("native frame exceeds limit".into()));
            }
            self.buffer.extend_from_slice(&buf[..count]);
            self.reader.consume(count);
            if end.is_some() {
                return Ok(Some(std::mem::take(&mut self.buffer)));
            }
        }
    }
}
pub(crate) async fn write<W: AsyncWrite + Unpin>(
    writer: &mut W,
    value: &Value,
) -> Result<(), HarnessError> {
    let mut bytes = serde_json::to_vec(value)
        .map_err(|_| HarnessError::Protocol("cannot encode native frame".into()))?;
    if bytes.len() >= MAX_FRAME {
        return Err(HarnessError::Protocol("native frame exceeds limit".into()));
    }
    bytes.push(b'\n');
    tokio::time::timeout(Duration::from_secs(2), async {
        writer.write_all(&bytes).await?;
        writer.flush().await
    })
    .await
    .map_err(|_| HarnessError::Protocol("native stdin stalled; delivery unknown".into()))??;
    Ok(())
}
/// The agent's program: the one given by path, or else the computer's own,
/// found where setup and sign-in find it (see [`ports::agent_program`]).
pub(crate) fn locate(explicit: &Option<PathBuf>, name: &str) -> Option<PathBuf> {
    if let Some(path) = explicit {
        return path.is_file().then(|| path.clone());
    }
    #[cfg(not(windows))]
    return ports::agent_program::on_this_computer(name);
    #[cfg(windows)]
    return ports::agent_program::on_this_windows(name);
}
pub(crate) fn command(binary: &Path) -> Command {
    let mut cmd = Command::new(binary);
    // Found outside the service's PATH, an npm-installed agent still finds
    // the `node` installed beside it. Øphio's own copies are programs alone.
    // Windows starts agents as it always has.
    #[cfg(not(windows))]
    if !ports::agent_copy::is_copy(binary)
        && let Some(path) =
            ports::agent_program::path_for(binary, std::env::var_os("PATH").as_deref())
    {
        cmd.env("PATH", path);
    }
    cmd.kill_on_drop(true)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    // Native tools own their sign-in. Do not inject API billing credentials or
    // caller-provided subscription tokens into a managed subprocess.
    for key in [
        "ANTHROPIC_API_KEY",
        "ANTHROPIC_AUTH_TOKEN",
        "ANTHROPIC_BASE_URL",
        "CLAUDE_CODE_OAUTH_TOKEN",
        "CLAUDE_CODE_USE_BEDROCK",
        "CLAUDE_CODE_USE_VERTEX",
        "CLAUDE_CODE_USE_FOUNDRY",
        "OPENAI_API_KEY",
        "OPENAI_BASE_URL",
        "CODEX_API_KEY",
        "CODEX_ACCESS_TOKEN",
        ports::bridge::RUN_TOKEN_ENV,
        ports::bridge::SOCKET_ENV,
    ] {
        cmd.env_remove(key);
    }
    if ports::agent_copy::is_copy(binary) {
        cmd.envs(ports::agent_copy::CLAUDE_ENV);
    }
    #[cfg(unix)]
    cmd.process_group(0);
    #[cfg(windows)]
    cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
    cmd
}
pub(crate) async fn inspect(binary: &Path, args: &[&str]) -> Option<std::process::Output> {
    inspect_in(binary, args, None).await
}
pub(crate) async fn inspect_in(
    binary: &Path,
    args: &[&str],
    cwd: Option<&Path>,
) -> Option<std::process::Output> {
    let mut cmd = command(binary);
    if let Some(cwd) = cwd {
        cmd.current_dir(cwd);
    }
    cmd.args(args).stdin(Stdio::null()).stderr(Stdio::piped());
    let check = ports::claude_checks::run(check_home(binary), async move {
        let mut child = containment::inherit::tokio_spawn(&mut cmd).ok()?;
        let mut stdout = tokio::spawn(check_output(child.stdout.take()?));
        let mut stderr = tokio::spawn(check_output(child.stderr.take()?));
        let status = finish_check(
            &mut child,
            "status or help",
            ports::claude_checks::EXIT_GRACE,
        )
        .await;
        let output = tokio::time::timeout(Duration::from_secs(1), async {
            Some(std::process::Output {
                status: status?,
                stdout: (&mut stdout).await.ok()??,
                stderr: (&mut stderr).await.ok()??,
            })
        })
        .await
        .ok()
        .flatten();
        stdout.abort();
        stderr.abort();
        output
    });
    // Keep startup and readiness responsive. Timing out this caller does not
    // cancel an admitted check's EOF cleanup or release its lock.
    tokio::time::timeout(Duration::from_secs(5), check)
        .await
        .ok()?
        .ok()
        .flatten()
}

pub(crate) fn check_home(binary: &Path) -> Option<PathBuf> {
    // Synthetic executables and their homes share a fixture folder. Downstream
    // tests opt in through a dev-dependency: cfg(test) alone does not reach
    // their copy of this crate. Normal builds still use one lock per real home.
    #[cfg(any(test, feature = "test-fixtures"))]
    return binary.parent().map(Path::to_owned);
    #[cfg(not(any(test, feature = "test-fixtures")))]
    {
        let _ = binary;
        None
    }
}

/// Keep draining even after the output limit, so the CLI can finish a login
/// write instead of blocking on a full pipe. No native output is logged.
async fn check_output(mut reader: impl AsyncRead + Unpin) -> Option<Vec<u8>> {
    use tokio::io::AsyncReadExt;
    const LIMIT: usize = 1024 * 1024;
    let mut output = Vec::new();
    let mut chunk = [0; 8192];
    let mut oversized = false;
    loop {
        let count = reader.read(&mut chunk).await.ok()?;
        if count == 0 {
            return (!oversized).then_some(output);
        }
        if output.len() + count <= LIMIT && !oversized {
            output.extend_from_slice(&chunk[..count]);
        } else {
            oversized = true;
        }
    }
}

/// EOF first; force termination only after the full persistence grace. The
/// caller owns this future independently of whoever requested the check.
pub(crate) async fn finish_check(
    child: &mut tokio::process::Child,
    operation: &'static str,
    grace: Duration,
) -> Option<std::process::ExitStatus> {
    drop(child.stdin.take());
    // Keep the leader unreaped on Unix until its process group has been
    // cleaned up; otherwise its id could be reused before we signal helpers.
    let mut leader = Leader::new(child).ok();
    let status = tokio::time::timeout(grace, async {
        match &mut leader {
            Some(leader) => leader.exited(child).await,
            None => child.wait().await.map_err(HarnessError::from),
        }
    })
    .await;
    if status.is_err() {
        tracing::warn!(
            operation,
            pid = child.id(),
            grace_seconds = grace.as_secs(),
            "Claude background check did not exit after EOF; forcing termination; sign-in may need checking"
        );
    }
    kill(child);
    let _ = child.wait().await;
    status.ok().and_then(Result::ok)
}
pub(crate) fn bridge_name(name: &str) -> Result<(), HarnessError> {
    if name.is_empty()
        || name.len() > 64
        || !name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
    {
        return Err(HarnessError::Protocol(
            "invalid bridge server or tool name".into(),
        ));
    }
    Ok(())
}
pub(crate) fn validate_env(env: &[(String, String)]) -> Result<(), HarnessError> {
    // Bridge credentials are the only environment overrides accepted from this
    // contract. Do not allow loader/provider overrides to change the harness.
    for (key, _) in env {
        if !matches!(
            key.as_str(),
            ports::bridge::RUN_TOKEN_ENV | ports::bridge::SOCKET_ENV
        ) {
            return Err(HarnessError::Unsupported(
                "bridge environment key is not supported".into(),
            ));
        }
    }
    for required in [ports::bridge::RUN_TOKEN_ENV, ports::bridge::SOCKET_ENV] {
        let matching: Vec<_> = env.iter().filter(|(key, _)| key == required).collect();
        if matching.len() != 1 || matching[0].1.is_empty() {
            return Err(HarnessError::Protocol(
                "bridge requires one nonempty token and socket environment value".into(),
            ));
        }
    }
    Ok(())
}

/// Kill the owned process group and its leader, without waiting for either or
/// reaping the leader (see [`Leader`]). What left the group ends with the run's
/// container; the broker must still fence desktop authority independently
/// before interrupting or shutting down.
pub(crate) fn kill(child: &mut tokio::process::Child) {
    #[cfg(unix)]
    if let Some(pid) = leader_id(child) {
        let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
    }
    let _ = child.start_kill();
}

#[cfg(unix)]
fn leader_id(child: &tokio::process::Child) -> Option<rustix::process::Pid> {
    child
        .id()
        .and_then(|id| i32::try_from(id).ok())
        .and_then(rustix::process::Pid::from_raw)
}

/// Watches for the leader's exit. On Unix the leader is left unreaped until the
/// run's container is confirmed ended: while it remains a zombie its id, which
/// names the owned group, cannot be reused, so the group's survivors can still
/// be signalled.
pub(crate) struct Leader {
    /// Readable once the leader exits. Unlike SIGCHLD, no other process's exit
    /// wakes it.
    #[cfg(target_os = "linux")]
    exit: tokio::io::unix::AsyncFd<std::os::fd::OwnedFd>,
    /// Without pidfds any child's exit wakes this, and the leader is checked.
    #[cfg(all(unix, not(target_os = "linux")))]
    exit: (rustix::process::Pid, tokio::signal::unix::Signal),
}
impl Leader {
    #[allow(unsafe_code)]
    pub(crate) fn new(child: &tokio::process::Child) -> Result<Self, HarnessError> {
        let _ = child;
        Ok(Self {
            #[cfg(all(unix, not(target_os = "linux")))]
            exit: (
                leader_id(child).ok_or(HarnessError::NotActive)?,
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::child())?,
            ),
            #[cfg(target_os = "linux")]
            exit: {
                use rustix::process::{PidfdFlags, pidfd_open};
                let pid = leader_id(child).ok_or(HarnessError::NotActive)?;
                let pidfd = pidfd_open(pid, PidfdFlags::empty()).map_err(std::io::Error::from)?;
                // SAFETY: the AsyncFd owns the pidfd, which nothing replaces or
                // closes until the AsyncFd is dropped.
                unsafe {
                    tokio::io::unix::AsyncFd::register_with_interest(
                        pidfd,
                        tokio::io::Interest::READABLE,
                    )
                }
                .map_err(std::io::Error::from)?
            },
        })
    }
    /// Cancellation safe.
    pub(crate) async fn exited(
        &mut self,
        child: &mut tokio::process::Child,
    ) -> Result<std::process::ExitStatus, HarnessError> {
        #[cfg(unix)]
        {
            use rustix::process::{WaitId, WaitIdOptions, waitid};
            #[cfg(target_os = "linux")]
            use std::os::fd::AsFd;
            use std::os::unix::process::ExitStatusExt;
            let _ = child;
            let options = WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT;
            loop {
                #[cfg(target_os = "linux")]
                let id = WaitId::PidFd(self.exit.get_ref().as_fd());
                #[cfg(not(target_os = "linux"))]
                let id = WaitId::Pid(self.exit.0);
                if let Some(status) = waitid(id, options).map_err(std::io::Error::from)? {
                    let raw = match (status.exit_status(), status.terminating_signal()) {
                        (Some(code), _) => (code & 0xff) << 8,
                        (None, Some(signal)) => signal & 0x7f,
                        (None, None) => {
                            return Err(HarnessError::Protocol(
                                "native exit status unavailable".into(),
                            ));
                        }
                    };
                    return Ok(std::process::ExitStatus::from_raw(raw));
                }
                #[cfg(target_os = "linux")]
                self.exit.readable().await?.clear_ready();
                // A signal can be folded into another, so look again anyway.
                #[cfg(not(target_os = "linux"))]
                let _ = tokio::time::timeout(std::time::Duration::from_secs(1), self.exit.1.recv())
                    .await;
            }
        }
        #[cfg(windows)]
        Ok(child.wait().await?)
    }
}

/// Confirm the owned group received termination and the leader exited, leaving
/// the leader unreaped (see [`Leader`]). Detached/remote children are outside
/// this fact.
pub(crate) async fn kill_owned(
    child: &mut tokio::process::Child,
    leader: &mut Leader,
) -> Result<(), HarnessError> {
    #[cfg(unix)]
    {
        let pid = leader_id(child).ok_or(HarnessError::NotActive)?;
        match rustix::process::kill_process_group(pid, rustix::process::Signal::KILL) {
            Ok(()) | Err(rustix::io::Errno::SRCH) => {}
            // A Mac refuses to signal a group of nothing but zombies.
            #[cfg(target_os = "macos")]
            Err(rustix::io::Errno::PERM) => {}
            Err(error) => return Err(std::io::Error::from(error).into()),
        }
        child.start_kill()?;
        leader.exited(child).await?;
    }
    // On Windows the job owns the tree, and is ended and confirmed once the
    // actor returns; a leader that already exited needs nothing more here.
    #[cfg(windows)]
    {
        let _ = leader;
        if child.try_wait()?.is_none() {
            child.kill().await?;
        }
    }
    Ok(())
}

pub(crate) fn no_token_in_arguments(
    args: &[String],
    env: &[(String, String)],
) -> Result<(), HarnessError> {
    if let Some((_, token)) = env
        .iter()
        .find(|(name, _)| name == ports::bridge::RUN_TOKEN_ENV)
        && !token.is_empty()
        && args.iter().any(|arg| arg.contains(token))
    {
        return Err(HarnessError::Protocol(
            "bridge token must travel only through environment".into(),
        ));
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod locate_tests {
    use super::*;

    /// An agent found outside the service's PATH (say npm's under nvm, a
    /// script that starts `node` from PATH) starts with its folder on PATH;
    /// one on PATH starts with PATH as it is.
    #[test]
    fn an_agent_found_off_path_starts_with_its_folder_on_path() {
        let path = |cmd: &Command| {
            cmd.as_std()
                .get_envs()
                .find(|(k, _)| *k == "PATH")
                .and_then(|(_, v)| v.map(std::ffi::OsStr::to_owned))
        };
        let folder = std::env::temp_dir().join(format!("ophio-locate-{}", std::process::id()));
        let added = path(&command(&folder.join("agent"))).expect("PATH set");
        assert_eq!(
            std::env::split_paths(&added).last().as_deref(),
            Some(folder.as_path())
        );
        if let Some(first) = std::env::var_os("PATH")
            .as_deref()
            .and_then(|p| std::env::split_paths(p).next())
        {
            assert_eq!(path(&command(&first.join("agent"))), None);
        }
    }
}
