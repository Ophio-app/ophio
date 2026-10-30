// SPDX-License-Identifier: Apache-2.0
//! Runs the services' own command-line tools, without a shell, with the
//! environment Ophio gives them and a time limit. Each runs in a container of
//! its own, so ending it ends all it started, however that detaches.
use containment::{Container, Guard};
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::sync::Notify;

/// The most a tool may print, both streams together. What prints more is
/// ended rather than read on.
pub(crate) const OUTPUT_LIMIT: usize = 1 << 20;

/// How tools are found and what every one of them is given. Tests point
/// `PATH` at fakes; the computer leaves it as the companion's own.
#[derive(Debug, Clone, Default)]
pub struct Tools {
    pub env: Vec<(OsString, OsString)>,
}
impl Tools {
    /// The value a tool would see for `name`.
    pub fn var(&self, name: &str) -> Option<OsString> {
        self.env
            .iter()
            .rev()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
            .or_else(|| std::env::var_os(name))
    }
    pub fn home(&self) -> Option<PathBuf> {
        self.var(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
            .map(PathBuf::from)
    }

    /// A command for `argv`, found on the tools' `PATH`, to start in a
    /// [`Contained`].
    pub fn command(
        &self,
        argv: &[String],
        extra: &[(String, OsString)],
    ) -> tokio::process::Command {
        let mut command = tokio::process::Command::from(self.std_command(argv, extra));
        command.kill_on_drop(true);
        command
    }

    /// [`Tools::command`] for a tool that makes a session of its own.
    pub fn std_command(
        &self,
        argv: &[String],
        extra: &[(String, OsString)],
    ) -> std::process::Command {
        let mut command = std::process::Command::new(self.find(&argv[0]));
        command
            .args(&argv[1..])
            .envs(self.env.iter().map(|(k, v)| (k, v)))
            .envs(extra.iter().map(|(k, v)| (k, v)));
        command
    }

    /// The program on the tools' `PATH`, or the name as given.
    pub fn find(&self, name: &str) -> PathBuf {
        if name.contains(std::path::MAIN_SEPARATOR) {
            return PathBuf::from(name);
        }
        let path = self.var("PATH").unwrap_or_default();
        for dir in std::env::split_paths(&path) {
            for candidate in candidates(name) {
                let full = dir.join(&candidate);
                if full.is_file() {
                    return full;
                }
            }
        }
        PathBuf::from(name)
    }

    /// Runs `argv` to the end, or until `limit`. What it leaves running is
    /// ended with it.
    pub async fn run(
        &self,
        argv: &[String],
        extra: &[(String, OsString)],
        limit: Duration,
    ) -> Result<Output, String> {
        let name = &argv[0];
        let contained = Contained::new(name)?;
        let mut command = self.command(argv, extra);
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = contained
            .container()
            .spawn(&mut command)
            .await
            .map_err(|e| format!("{name} could not start: {e}"))?;
        let pid = child.id();
        let deadline = tokio::time::Instant::now() + limit;
        let budget = Arc::new(Budget::default());
        let stdout = Reader(tokio::spawn(read_all(child.stdout.take(), budget.clone())));
        let stderr = Reader(tokio::spawn(read_all(child.stderr.take(), budget.clone())));
        let exited = tokio::time::timeout_at(deadline, async {
            tokio::select! {
                success = exit(&mut child, pid) => Some(success),
                () = budget.over.notified() => None,
            }
        })
        .await;
        // Ending what it left running also ends its output.
        let ended = contained.end_once(pid).await;
        let _ = child.try_wait();
        if let Err(e) = ended {
            tracing::warn!("accounts: {name}: {e}");
            return Err(format!(
                "{name} left something running that Ophio could not end"
            ));
        }
        let late = || format!("{name} did not answer in time");
        let too_much = |_| format!("{name} printed more than Ophio reads");
        let success = exited
            .map_err(|_| late())?
            .ok_or(TooMuch)
            .map_err(too_much)?;
        // What escaped a process group can still hold its output open, so
        // reading it keeps to the same limit.
        let (stdout, stderr) =
            tokio::time::timeout_at(deadline, async { (stdout.all().await, stderr.all().await) })
                .await
                .map_err(|_| late())?;
        Ok(Output {
            success,
            stdout: stdout.map_err(too_much)?,
            stderr: stderr.map_err(too_much)?,
        })
    }
}

/// Reads a tool's output, and stops when dropped, so a limit, an error or a
/// cancelled run leaves nothing reading.
struct Reader(tokio::task::JoinHandle<Result<String, TooMuch>>);
impl Reader {
    async fn all(mut self) -> Result<String, TooMuch> {
        (&mut self.0).await.unwrap_or(Ok(String::new()))
    }
}
impl Drop for Reader {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// What a tool's two streams have printed so far, and the signal that
/// together they went past [`OUTPUT_LIMIT`].
#[derive(Default)]
struct Budget {
    read: AtomicUsize,
    over: Notify,
}

/// A tool printed more than [`OUTPUT_LIMIT`].
struct TooMuch;

async fn read_all(
    stream: Option<impl tokio::io::AsyncRead + Unpin>,
    budget: Arc<Budget>,
) -> Result<String, TooMuch> {
    let mut out = Vec::new();
    if let Some(mut stream) = stream {
        let mut chunk = [0u8; 8192];
        while let Ok(n @ 1..) = stream.read(&mut chunk).await {
            if budget.read.fetch_add(n, Ordering::Relaxed) + n > OUTPUT_LIMIT {
                // Kept for the run to find, however late it looks.
                budget.over.notify_one();
                return Err(TooMuch);
            }
            out.extend_from_slice(&chunk[..n]);
        }
    }
    Ok(String::from_utf8_lossy(&out).into_owned())
}

/// Waits for `child`, process `pid`, to exit, and says whether it
/// succeeded, leaving it unreaped until its container's end is confirmed.
async fn exit(child: &mut tokio::process::Child, pid: Option<u32>) -> bool {
    #[cfg(unix)]
    {
        let _ = child;
        let Some(pid) = pid else {
            return false;
        };
        loop {
            if let Some(success) = exited(pid) {
                return success;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        child.wait().await.is_ok_and(|s| s.success())
    }
}

/// Whether process `pid`, a child nothing has reaped, has exited, and
/// whether it succeeded. It stays unreaped.
#[cfg(unix)]
pub fn exited(pid: u32) -> Option<bool> {
    use rustix::process::{Pid, WaitId, WaitIdOptions, waitid};
    let pid = i32::try_from(pid).ok().and_then(Pid::from_raw)?;
    match waitid(
        WaitId::Pid(pid),
        WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
    ) {
        Ok(Some(status)) => Some(status.exit_status() == Some(0)),
        Ok(None) => None,
        // Reaped already: there's nothing left to wait for.
        Err(_) => Some(false),
    }
}

/// A tool and all it starts, in a container of their own that is ended
/// whatever happens to what holds it.
#[derive(Debug)]
pub struct Contained {
    container: Arc<Container>,
    guard: Guard,
}

impl Contained {
    pub fn new(name: &str) -> Result<Self, String> {
        let container =
            Arc::new(Container::new().map_err(|e| format!("{name} could not start: {e}"))?);
        Ok(Self {
            guard: Guard::new(container.clone()),
            container,
        })
    }

    pub fn container(&self) -> &Container {
        &self.container
    }

    /// Ends the tool, process `leader`, and all it started, and confirms
    /// they have ended. Leave the tool unreaped until then.
    pub async fn end_once(&self, leader: Option<u32>) -> Result<(), String> {
        // The tool may have left the container's process group; the
        // container still holds it, but killing it is up to what started it.
        #[cfg(unix)]
        if let Some(pid) = leader.and_then(|p| i32::try_from(p).ok())
            && let Some(pid) = rustix::process::Pid::from_raw(pid)
        {
            let _ = rustix::process::kill_process(pid, rustix::process::Signal::KILL);
        }
        #[cfg(not(unix))]
        let _ = leader;
        self.guard.end().await.map_err(|e| e.to_string())
    }

    /// [`Contained::end_once`], and again and again until the end is
    /// confirmed: until then what the tool started may still be running.
    pub async fn end(&self, leader: Option<u32>) {
        if let Err(e) = self.end_once(leader).await {
            tracing::warn!("accounts: {e}");
            self.guard.confirm_end().await;
        }
    }
}

fn candidates(name: &str) -> Vec<String> {
    if cfg!(windows) {
        vec![
            format!("{name}.exe"),
            format!("{name}.cmd"),
            name.to_owned(),
        ]
    } else {
        vec![name.to_owned()]
    }
}

#[derive(Debug, Clone)]
pub struct Output {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

/// Drops terminal colour and cursor codes from a tool's output.
pub fn plain(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            match chars.peek() {
                Some('[') => {
                    chars.next();
                    for c in chars.by_ref() {
                        if ('@'..='~').contains(&c) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    chars.next();
                    while let Some(c) = chars.next() {
                        if c == '\u{7}' || (c == '\u{1b}' && chars.next_if_eq(&'\\').is_some()) {
                            break;
                        }
                    }
                }
                _ => {
                    chars.next();
                }
            }
        } else if c == '\r' {
            out.push('\n');
        } else if !c.is_control() || c == '\n' || c == '\t' {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn plain_drops_colours_and_links() {
        assert_eq!(
            super::plain(
                "\u{1b}[1;32mok\u{1b}[0m \u{1b}]8;;https://x\u{7}link\u{1b}]8;;\u{1b}\\\r\n"
            ),
            "ok link\n\n"
        );
    }
}
