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
pub(crate) fn locate(explicit: &Option<PathBuf>, name: &str) -> Option<PathBuf> {
    if let Some(path) = explicit {
        return path.is_file().then(|| path.clone());
    }
    let executable = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.into()
    };
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            let path = dir.join(&executable);
            if path.is_file() {
                return Some(path);
            }
        }
    }
    for root in [std::env::var_os("HOME"), std::env::var_os("USERPROFILE")]
        .into_iter()
        .flatten()
    {
        for suffix in [
            ".local/bin",
            ".cargo/bin",
            "AppData/Local/Programs",
            "AppData/Roaming/npm",
        ] {
            let path = PathBuf::from(&root).join(suffix).join(&executable);
            if path.is_file() {
                return Some(path);
            }
        }
    }
    None
}
pub(crate) fn command(binary: &Path) -> Command {
    let mut cmd = Command::new(binary);
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
    tokio::time::timeout(Duration::from_secs(5), cmd.output())
        .await
        .ok()?
        .ok()
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
    #[cfg(unix)]
    exit: tokio::io::unix::AsyncFd<std::os::fd::OwnedFd>,
}
impl Leader {
    #[allow(unsafe_code)]
    pub(crate) fn new(child: &tokio::process::Child) -> Result<Self, HarnessError> {
        let _ = child;
        Ok(Self {
            #[cfg(unix)]
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
            use std::os::{fd::AsFd, unix::process::ExitStatusExt};
            let _ = child;
            let options = WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT;
            loop {
                if let Some(status) = waitid(WaitId::PidFd(self.exit.get_ref().as_fd()), options)
                    .map_err(std::io::Error::from)?
                {
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
                self.exit.readable().await?.clear_ready();
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
