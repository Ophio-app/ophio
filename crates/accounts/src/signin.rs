// SPDX-License-Identifier: Apache-2.0
//! Drives a service's own sign-in command on the computer. The pages it
//! wants opened go to the computer's browser through Ophio, so the phone can
//! follow along; the one-time code it prints is the only output kept.
use crate::desktop::Opener;
use crate::files::private_dir;
use crate::tool::{Contained, OUTPUT_LIMIT, Tools, plain};
use protocol::accounts::SignInStage;
use std::collections::HashSet;
use std::ffi::OsString;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio::io::AsyncReadExt;
use tokio::sync::{mpsc, watch};

/// How long the owner has to finish on the provider's page.
pub const LIMIT: Duration = Duration::from_secs(10 * 60);
/// A tool that prints an address instead of opening it is given this long
/// to open one before Ophio opens the printed one.
const PRINTED_GRACE: Duration = Duration::from_secs(2);
const KEPT: usize = 64 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub enum Ended {
    Exited { success: bool },
    Cancelled,
    TimedOut,
    Failed(String),
}

pub struct Run<'a> {
    pub argv: &'a [String],
    pub extra: Vec<(String, OsString)>,
    pub pty: bool,
    pub tools: &'a Tools,
    pub opener: &'a dyn Opener,
    /// Private and emptied afterwards: the browser stand-in and what it saw.
    pub scratch: PathBuf,
    pub limit: Duration,
}

/// Runs the sign-in command until it exits, is cancelled or runs out of time.
pub async fn run(
    run: Run<'_>,
    stage: &(dyn Fn(SignInStage) + Send + Sync),
    mut cancel: watch::Receiver<bool>,
) -> Ended {
    let _ = std::fs::remove_dir_all(&run.scratch);
    if let Err(e) = private_dir(&run.scratch) {
        return Ended::Failed(format!("Ophio could not prepare the sign-in: {e}"));
    }
    let ended = drive(&run, stage, &mut cancel).await;
    let _ = std::fs::remove_dir_all(&run.scratch);
    ended
}

async fn drive(
    run: &Run<'_>,
    stage: &(dyn Fn(SignInStage) + Send + Sync),
    cancel: &mut watch::Receiver<bool>,
) -> Ended {
    let urls = run.scratch.join("urls");
    let mut extra = run.extra.clone();
    if let Err(e) = browser_stand_in(&run.scratch, &urls, run.tools, &mut extra) {
        return Ended::Failed(format!("Ophio could not prepare the sign-in: {e}"));
    }
    // A few reads deep: a tool that prints faster than it is read waits.
    let (out_tx, mut out) = mpsc::channel::<Vec<u8>>(16);
    let mut proc = match Proc::start(run, &extra, out_tx).await {
        Ok(p) => p,
        Err(e) => return Ended::Failed(e),
    };
    let ended = follow(run, stage, cancel, &mut proc, &mut out).await;
    // Readers waiting on a full queue stop, and so stop holding the output.
    drop(out);
    // Nothing the tool started may outlive the sign-in, to write a login
    // after it was cancelled or signed out: until all of it has ended, the
    // sign-in is still stopping.
    proc.end().await;
    ended
}

async fn follow(
    run: &Run<'_>,
    stage: &(dyn Fn(SignInStage) + Send + Sync),
    cancel: &mut watch::Receiver<bool>,
    proc: &mut Proc,
    out: &mut mpsc::Receiver<Vec<u8>>,
) -> Ended {
    let name = &run.argv[0];
    let urls = run.scratch.join("urls");
    let mut seen = Seen::default();
    let mut printed = 0usize;
    let deadline = tokio::time::sleep(run.limit);
    tokio::pin!(deadline);
    let mut tick = tokio::time::interval(Duration::from_millis(250));
    let mut read_urls = 0usize;
    loop {
        tokio::select! {
            () = &mut deadline => return Ended::TimedOut,
            changed = cancel.changed() => {
                if changed.is_err() || *cancel.borrow() {
                    return Ended::Cancelled;
                }
            }
            Some(chunk) = out.recv() => {
                printed += chunk.len();
                if printed > OUTPUT_LIMIT {
                    return Ended::Failed(format!("{name} printed more than Ophio reads"));
                }
                let text = plain(&String::from_utf8_lossy(&chunk));
                for _ in 0..seen.feed(&text) {
                    proc.enter();
                }
                seen.show(stage);
            }
            _ = tick.tick() => {
                let text = match pages(&urls) {
                    Ok(text) => text,
                    Err(TooMany) => {
                        return Ended::Failed(format!(
                            "{name} asked for more pages than Ophio opens"
                        ));
                    }
                };
                if let Some(text) = text {
                    let lines: Vec<&str> = text.lines().collect();
                    for url in lines.iter().skip(read_urls) {
                        seen.open(url.trim(), run.opener);
                    }
                    read_urls = lines.len();
                }
                seen.open_printed(run.opener);
                seen.show(stage);
                if let Some(success) = proc.exited() {
                    return Ended::Exited { success };
                }
            }
        }
    }
}

/// The most the addresses the tool asked to open may come to.
const PAGES_LIMIT: u64 = 64 * 1024;

/// The tool asked to open more than [`PAGES_LIMIT`] of addresses.
struct TooMany;

/// The addresses the tool has asked to open so far, `None` while there are
/// none to read.
fn pages(urls: &Path) -> Result<Option<String>, TooMany> {
    let Ok(file) = std::fs::File::open(urls) else {
        return Ok(None);
    };
    let mut text = String::new();
    if std::io::Read::read_to_string(&mut std::io::Read::take(file, PAGES_LIMIT + 1), &mut text)
        .is_err()
    {
        return Ok(None);
    }
    if text.len() as u64 > PAGES_LIMIT {
        return Err(TooMany);
    }
    Ok(Some(text))
}

/// `BROWSER` and an `xdg-open` that only write down the address, so the
/// tool's page is opened by Ophio, once, and the phone learns where.
fn browser_stand_in(
    scratch: &Path,
    urls: &Path,
    tools: &Tools,
    extra: &mut Vec<(String, OsString)>,
) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use crate::files::write_private;
        use std::os::unix::fs::PermissionsExt;
        let script = format!(
            "#!/bin/sh\nfor a in \"$@\"; do printf '%s\\n' \"$a\" >> {}; done\n",
            shell_words::quote(&urls.to_string_lossy())
        );
        let shim = scratch.join("xdg-open");
        write_private(&shim, script.as_bytes())?;
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o700))?;
        let mut path = vec![scratch.to_path_buf()];
        path.extend(std::env::split_paths(
            &tools.var("PATH").unwrap_or_default(),
        ));
        extra.push((
            "PATH".into(),
            std::env::join_paths(path).map_err(std::io::Error::other)?,
        ));
        extra.push(("BROWSER".into(), shim.clone().into()));
        extra.push(("GH_BROWSER".into(), shim.into()));
    }
    #[cfg(not(unix))]
    let _ = (scratch, urls, tools, extra);
    Ok(())
}

/// What the tool has said and opened so far.
#[derive(Default)]
struct Seen {
    text: String,
    /// Where the next "Press Enter" and address are looked for.
    enter_from: usize,
    url_from: usize,
    code: Option<String>,
    host: Option<String>,
    opened: HashSet<String>,
    printed: Option<(String, Instant)>,
    shown: Option<SignInStage>,
}

impl Seen {
    /// Takes in more output; returns how many times the tool asked for Enter.
    fn feed(&mut self, text: &str) -> usize {
        self.text.push_str(text);
        if self.text.len() > KEPT {
            let mut cut = self.text.len() - KEPT / 2;
            while !self.text.is_char_boundary(cut) {
                cut += 1;
            }
            self.text.drain(..cut);
            self.enter_from = self.enter_from.saturating_sub(cut);
            self.url_from = self.url_from.saturating_sub(cut);
        }
        let mut enters = 0;
        while let Some(at) = self.text[self.enter_from..].find("Press Enter") {
            self.enter_from += at + "Press Enter".len();
            enters += 1;
        }
        if self.code.is_none() {
            self.code = one_time_code(&self.text);
        }
        // An address counts once whitespace shows where it ends.
        while let Some(at) = self.text[self.url_from..].find("https://") {
            let start = self.url_from + at;
            let rest = &self.text[start..];
            let Some(len) = rest.find(|c: char| c.is_whitespace()) else {
                break;
            };
            let url = rest[..len]
                .trim_end_matches(['.', ',', ')', '"', '\''])
                .to_owned();
            self.url_from = start + len;
            if self.printed.is_none() && allowed(&url) {
                self.printed = Some((url, Instant::now()));
            }
        }
        enters
    }

    fn open(&mut self, url: &str, opener: &dyn Opener) {
        if !allowed(url) || !self.opened.insert(url.to_owned()) {
            return;
        }
        match opener.open(url) {
            Ok(()) => self.host = host(url),
            Err(e) => tracing::warn!("accounts: {e}"),
        }
    }

    fn open_printed(&mut self, opener: &dyn Opener) {
        if !cfg!(unix) || !self.opened.is_empty() {
            return;
        }
        if let Some((url, at)) = self.printed.clone()
            && at.elapsed() >= PRINTED_GRACE
        {
            self.open(&url, opener);
        }
    }

    fn show(&mut self, stage: &(dyn Fn(SignInStage) + Send + Sync)) {
        let now = match (&self.code, &self.host) {
            (Some(code), host) => SignInStage::Code {
                code: code.clone(),
                url_host: host.clone().unwrap_or_default(),
            },
            (None, Some(host)) => SignInStage::Browser {
                url_host: host.clone(),
            },
            (None, None) => return,
        };
        if self.shown.as_ref() != Some(&now) {
            self.shown = Some(now.clone());
            stage(now);
        }
    }
}

/// Only the provider's pages, never a file or another kind of address.
fn allowed(url: &str) -> bool {
    match url::Url::parse(url) {
        Ok(u) if u.scheme() == "https" => u.host_str().is_some() && u.password().is_none(),
        Ok(u) if u.scheme() == "http" => matches!(u.host_str(), Some("localhost" | "127.0.0.1")),
        _ => false,
    }
}

fn host(url: &str) -> Option<String> {
    url::Url::parse(url).ok()?.host_str().map(str::to_owned)
}

/// The `XXXX-XXXX` code a tool asks the owner to enter on the provider's page.
fn one_time_code(text: &str) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    let after = lower.find("one-time code")? + "one-time code".len();
    let bytes = text.as_bytes();
    let part = |b: &[u8]| {
        b.iter()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
    };
    (after..bytes.len().saturating_sub(8)).find_map(|i| {
        let w = &bytes[i..i + 9];
        let boundary = i == 0 || !bytes[i - 1].is_ascii_alphanumeric();
        let end = bytes.get(i + 9).is_none_or(|c| !c.is_ascii_alphanumeric());
        (boundary && end && w[4] == b'-' && part(&w[..4]) && part(&w[5..]))
            .then(|| text[i..i + 9].to_owned())
    })
}

/// The sign-in command, in a container of its own with all it starts.
struct Proc {
    contained: Contained,
    pid: Option<u32>,
    io: Io,
}

// One for each sign-in, so the size of either matters little.
#[allow(clippy::large_enum_variant)]
enum Io {
    Plain {
        child: tokio::process::Child,
        stdin: Option<tokio::process::ChildStdin>,
    },
    Pty {
        child: Box<dyn portable_pty::Child + Send + Sync>,
        press: std::sync::mpsc::SyncSender<()>,
        _master: Box<dyn portable_pty::MasterPty + Send>,
    },
}

/// Enter presses waiting to be written. A tool that is not reading isn't
/// waiting for Enter, so presses past these are dropped.
const PRESSES: usize = 4;

/// Writes Enter on `writer` when asked, on a thread of its own: on a
/// terminal whose tool isn't reading the write waits, and the sign-in must
/// still see its cancellation, time limit and output limit meanwhile. The
/// thread ends once the sender is dropped, or once a write fails as the
/// tool's terminal closes.
fn presser(mut writer: Box<dyn std::io::Write + Send>) -> std::sync::mpsc::SyncSender<()> {
    let (press, pressed) = std::sync::mpsc::sync_channel::<()>(PRESSES);
    std::thread::spawn(move || {
        while pressed.recv().is_ok() {
            if writer
                .write_all(b"\r")
                .and_then(|()| writer.flush())
                .is_err()
            {
                break;
            }
        }
    });
    press
}

impl Proc {
    async fn start(
        run: &Run<'_>,
        extra: &[(String, OsString)],
        out: mpsc::Sender<Vec<u8>>,
    ) -> Result<Self, String> {
        let name = &run.argv[0];
        let not_started = |e: &dyn std::fmt::Display| format!("{name} could not start: {e}");
        let contained = Contained::new(name)?;
        if run.pty {
            let pair = portable_pty::native_pty_system()
                .openpty(portable_pty::PtySize {
                    rows: 40,
                    cols: 200,
                    ..Default::default()
                })
                .map_err(|e| not_started(&e))?;
            let mut reader = pair
                .master
                .try_clone_reader()
                .map_err(|e| not_started(&e))?;
            let writer = pair.master.take_writer().map_err(|e| not_started(&e))?;
            #[cfg(target_os = "linux")]
            let child: Box<dyn portable_pty::Child + Send + Sync> = {
                let mut command = run.tools.std_command(run.argv, extra);
                command.current_dir(&run.scratch);
                Box::new(
                    on_terminal(&*pair.master, command, contained.container())
                        .map_err(|e| not_started(&e))?,
                )
            };
            #[cfg(not(target_os = "linux"))]
            let child = {
                let mut command = portable_pty::CommandBuilder::new(run.tools.find(name));
                command.args(&run.argv[1..]);
                command.cwd(&run.scratch);
                for (k, v) in run.tools.env.iter() {
                    command.env(k, v);
                }
                for (k, v) in extra {
                    command.env(k, v);
                }
                let child = pair
                    .slave
                    .spawn_command(command)
                    .map_err(|e| not_started(&e))?;
                // Where the container is the tool's process group, the
                // session the pty's spawn makes leads it. On Windows the
                // tool joins its job only once it has started: what it
                // starts before then is outside.
                #[cfg(unix)]
                if let Some(pid) = child.process_id() {
                    contained.container().adopt(pid);
                }
                #[cfg(windows)]
                if let Err(e) = child
                    .as_raw_handle()
                    .ok_or_else(|| "no handle".to_owned())
                    .and_then(|h| contained.container().adopt(h).map_err(|e| e.to_string()))
                {
                    let mut child = child;
                    let _ = child.kill();
                    contained.end(None).await;
                    return Err(not_started(&e));
                }
                child
            };
            drop(pair.slave);
            std::thread::spawn(move || {
                let mut buf = [0u8; 4096];
                while let Ok(n) = std::io::Read::read(&mut reader, &mut buf) {
                    if n == 0 || out.blocking_send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
            });
            return Ok(Self {
                pid: child.process_id(),
                contained,
                io: Io::Pty {
                    child,
                    press: presser(writer),
                    _master: pair.master,
                },
            });
        }
        let mut command = run.tools.command(run.argv, extra);
        command
            .current_dir(&run.scratch)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut child = contained
            .container()
            .spawn(&mut command)
            .await
            .map_err(|e| not_started(&e))?;
        for stream in [
            child
                .stdout
                .take()
                .map(|s| Box::new(s) as Box<dyn tokio::io::AsyncRead + Send + Unpin>),
            child
                .stderr
                .take()
                .map(|s| Box::new(s) as Box<dyn tokio::io::AsyncRead + Send + Unpin>),
        ]
        .into_iter()
        .flatten()
        {
            let out = out.clone();
            tokio::spawn(async move {
                let mut stream = stream;
                let mut buf = [0u8; 4096];
                while let Ok(n) = stream.read(&mut buf).await {
                    if n == 0 || out.send(buf[..n].to_vec()).await.is_err() {
                        break;
                    }
                }
            });
        }
        let stdin = child.stdin.take();
        Ok(Self {
            pid: child.id(),
            contained,
            io: Io::Plain { child, stdin },
        })
    }

    fn enter(&mut self) {
        match &mut self.io {
            Io::Plain { stdin, .. } => {
                if let Some(stdin) = stdin.as_mut() {
                    // A full pipe means the tool is not reading; it is not waiting for Enter.
                    let _ = tokio::io::AsyncWriteExt::write_all(stdin, b"\n").now_or_never();
                }
            }
            Io::Pty { press, .. } => {
                // A full queue means the tool is not reading, as above.
                let _ = press.try_send(());
            }
        }
    }

    /// Whether the tool has exited, leaving it unreaped until all it
    /// started has ended.
    fn exited(&mut self) -> Option<bool> {
        #[cfg(unix)]
        return self.pid.and_then(crate::tool::exited);
        #[cfg(not(unix))]
        match &mut self.io {
            Io::Plain { child, .. } => child.try_wait().ok().flatten().map(|s| s.success()),
            Io::Pty { child, .. } => child.try_wait().ok().flatten().map(|s| s.success()),
        }
    }

    /// Ends the tool and everything it started, and waits until that is
    /// confirmed.
    async fn end(&mut self) {
        self.contained.end(self.pid).await;
        match &mut self.io {
            Io::Plain { child, .. } => {
                let _ = child.try_wait();
            }
            Io::Pty { child, .. } => {
                let _ = child.try_wait();
            }
        }
    }
}

/// Starts `command` on the terminal `master` leads, in a session of its own,
/// as the pty's own spawn does, but inside `container` before its program
/// runs, so nothing it starts is outside.
#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
fn on_terminal(
    master: &dyn portable_pty::MasterPty,
    mut command: std::process::Command,
    container: &containment::Container,
) -> std::io::Result<std::process::Child> {
    use std::os::unix::{fs::OpenOptionsExt, process::CommandExt};
    let terminal = std::fs::File::options()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOCTTY)
        .open(
            master
                .tty_name()
                .ok_or_else(|| std::io::Error::other("the terminal has no name"))?,
        )?;
    command
        .stdin(terminal.try_clone()?)
        .stdout(terminal.try_clone()?)
        .stderr(terminal);
    container
        .prepare_session(&mut command)
        .map_err(std::io::Error::other)?;
    // SAFETY: between fork and exec the new process only makes the
    // async-signal-safe calls signal(2), sigprocmask(2), setsid(2), ioctl(2)
    // and close_range(2), as the pty's own spawn does. Only a kernel without
    // close_range falls back to that spawn's way of closing descriptors.
    unsafe {
        command.pre_exec(|| {
            for signal in [
                libc::SIGCHLD,
                libc::SIGHUP,
                libc::SIGINT,
                libc::SIGQUIT,
                libc::SIGTERM,
                libc::SIGALRM,
            ] {
                libc::signal(signal, libc::SIG_DFL);
            }
            let empty: libc::sigset_t = std::mem::zeroed();
            libc::sigprocmask(libc::SIG_SETMASK, &empty, std::ptr::null_mut());
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            #[allow(clippy::cast_lossless)]
            if libc::ioctl(0, libc::TIOCSCTTY as _, 0) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::syscall(
                libc::SYS_close_range,
                3 as libc::c_uint,
                libc::c_uint::MAX,
                libc::CLOSE_RANGE_CLOEXEC,
            ) == -1
            {
                portable_pty::unix::close_random_fds();
            }
            Ok(())
        });
    }
    let child = command.spawn()?;
    container.adopt(child.id());
    Ok(child)
}

trait NowOrNever: std::future::Future + Sized {
    fn now_or_never(self) -> Option<Self::Output> {
        let waker = std::task::Waker::noop();
        let mut cx = std::task::Context::from_waker(waker);
        let mut this = std::pin::pin!(self);
        match this.as_mut().poll(&mut cx) {
            std::task::Poll::Ready(out) => Some(out),
            std::task::Poll::Pending => None,
        }
    }
}
impl<F: std::future::Future> NowOrNever for F {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_one_time_code_and_each_request_for_enter() {
        let mut seen = Seen::default();
        assert_eq!(seen.feed("! First copy your one-time code: "), 0);
        assert_eq!(seen.code, None);
        assert_eq!(
            seen.feed(
                "1A2B-3C4D\nPress Enter to open https://github.com/login/device in your browser... "
            ),
            1
        );
        assert_eq!(seen.code.as_deref(), Some("1A2B-3C4D"));
        assert_eq!(
            seen.printed.as_ref().map(|p| p.0.as_str()),
            Some("https://github.com/login/device")
        );
        assert_eq!(seen.feed("still waiting"), 0);
        assert_eq!(
            one_time_code("code ABCD-EFGH"),
            None,
            "only after the words one-time code"
        );
    }

    #[test]
    fn enter_is_never_waited_for() {
        // A terminal whose tool reads nothing: every write waits.
        struct Stuck(std::sync::mpsc::Receiver<()>);
        impl std::io::Write for Stuck {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                let _ = self.0.recv();
                Err(std::io::ErrorKind::BrokenPipe.into())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let (closed, closes) = std::sync::mpsc::channel();
        let press = presser(Box::new(Stuck(closes)));
        let began = Instant::now();
        let queued = (0..10_000).filter(|_| press.try_send(()).is_ok()).count();
        assert!(began.elapsed() < Duration::from_secs(1));
        // One taken by the waiting write, and as many as wait behind it.
        assert!(queued <= PRESSES + 1, "{queued}");
        // Once the terminal closes the write fails, and the thread ends.
        drop(closed);
        let ended = Instant::now();
        while !matches!(
            press.try_send(()),
            Err(std::sync::mpsc::TrySendError::Disconnected(()))
        ) {
            assert!(
                ended.elapsed() < Duration::from_secs(5),
                "the thread still runs"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn only_provider_pages_are_opened() {
        assert!(allowed("https://github.com/login/device"));
        assert!(allowed("http://localhost:1455/auth/callback"));
        assert!(!allowed("http://example.com/"));
        assert!(!allowed("file:///etc/passwd"));
        assert!(!allowed("javascript:alert(1)"));
    }
}
