// SPDX-License-Identifier: Apache-2.0
#[cfg(unix)]
use portable_pty::Child;
#[cfg(not(target_os = "linux"))]
use portable_pty::CommandBuilder;
use ports::terminal::{TerminalError, TerminalSpec};
#[cfg(unix)]
use std::io;
use std::path::PathBuf;

/// The program initially occupying a terminal's PID. A later exec retains
/// that PID, so a shell-only process count is insufficient to establish idle.
#[cfg(target_os = "linux")]
#[derive(Debug)]
pub(crate) struct ProgramIdentity {
    // Keep the original inode pinned even if the executable is replaced.
    executable: std::fs::File,
    arguments: Vec<u8>,
    started: u64,
}

#[cfg(target_os = "linux")]
impl ProgramIdentity {
    pub(crate) fn read(pid: u32) -> io::Result<Self> {
        let dir = PathBuf::from(format!("/proc/{pid}"));
        let executable = std::fs::File::open(dir.join("exe"))?;
        let arguments = std::fs::read(dir.join("cmdline"))?;
        let stat = std::fs::read_to_string(dir.join("stat"))?;
        let started = stat
            .rsplit_once(") ")
            .and_then(|(_, tail)| tail.split_whitespace().nth(19))
            .and_then(|value| value.parse().ok());
        match started {
            Some(started) if !arguments.is_empty() => Ok(Self {
                executable,
                arguments,
                started,
            }),
            _ => Err(io::Error::other("terminal program identity unavailable")),
        }
    }

    pub(crate) fn still_occupies(&self, pid: u32) -> io::Result<bool> {
        use std::os::unix::fs::MetadataExt;
        let now = Self::read(pid)?;
        let before = self.executable.metadata()?;
        let after = now.executable.metadata()?;
        Ok((before.dev(), before.ino()) == (after.dev(), after.ino())
            && self.arguments == now.arguments
            && self.started == now.started)
    }
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn command(spec: &TerminalSpec) -> Result<CommandBuilder, TerminalError> {
    if let Some((program, args)) = &spec.program {
        let mut command = CommandBuilder::new(program);
        command.args(args);
        return Ok(command);
    }
    let shell = default_shell()?;
    let command = CommandBuilder::new(shell);
    #[cfg(unix)]
    let command = {
        let mut command = command;
        command.arg("-l");
        command
    };
    Ok(command)
}

/// Start the terminal's program on the pty as the pty's own `spawn_command`
/// would, from the same program, arguments, directory and environment, except
/// that the new process first joins `group`: between fork and exec, before the
/// program, its loader or anything the environment has the loader run. Should
/// it not join, the program never runs.
#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
pub(crate) fn spawn(
    master: &dyn portable_pty::MasterPty,
    spec: &TerminalSpec,
    group: Option<&containment::TerminalGroup>,
) -> Result<Box<dyn Child + Send + Sync>, TerminalError> {
    use std::{
        ffi::OsString,
        os::unix::{fs::OpenOptionsExt, process::CommandExt},
    };
    let unstartable = || TerminalError::Spawn("program could not be started".into());
    let (program, args): (OsString, Vec<OsString>) = match &spec.program {
        Some((program, args)) => (program.into(), args.iter().map(Into::into).collect()),
        None => (default_shell()?.into(), vec!["-l".into()]),
    };
    let path = spec
        .env
        .iter()
        .rev()
        .find(|(key, _)| key == "PATH")
        .map(|(_, value)| OsString::from(value))
        .or_else(|| std::env::var_os("PATH"));
    let resolved = resolve(&program, path.as_deref(), &spec.cwd)?;
    let terminal = std::fs::File::options()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOCTTY)
        .open(master.tty_name().ok_or_else(unstartable)?)?;
    let mut command = std::process::Command::new(resolved);
    command
        .arg0(&program)
        .args(&args)
        .current_dir(&spec.cwd)
        .stdin(terminal.try_clone()?)
        .stdout(terminal.try_clone()?)
        .stderr(terminal);
    if std::env::var_os("SHELL").is_none() {
        command.env("SHELL", login_shell());
    }
    command.envs(spec.env.iter().map(|(key, value)| (key, value)));
    command.env("TERM", "xterm-256color");
    if let Some(group) = group {
        group.prepare(&mut command)?;
    }
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
                return Err(io::Error::last_os_error());
            }
            // The pty becomes the controlling terminal, so the program gets
            // SIGWINCH when it is resized.
            #[allow(clippy::cast_lossless)]
            if libc::ioctl(0, libc::TIOCSCTTY as _, 0) == -1 {
                return Err(io::Error::last_os_error());
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
    let child = command.spawn().map_err(|_| unstartable())?;
    Ok(Box::new(child))
}

/// Where the pty's own spawn would find `program`, given the `PATH` it would
/// search and the directory it would start in: one naming a directory in
/// every case, from `cwd` when it starts with `.` or `..`, and otherwise from
/// each `PATH` entry in turn, taken from `cwd` when relative. The first that
/// is not a folder and that this user may run is the one.
#[cfg(target_os = "linux")]
pub(crate) fn resolve(
    program: &std::ffi::OsStr,
    path: Option<&std::ffi::OsStr>,
    cwd: &std::path::Path,
) -> Result<PathBuf, TerminalError> {
    use nix::unistd::{AccessFlags, access};
    use std::path::{Component, Path};
    let runnable =
        |candidate: &PathBuf| !candidate.is_dir() && access(candidate, AccessFlags::X_OK).is_ok();
    let program = Path::new(program);
    let found = if program.is_absolute() {
        Some(program.to_owned()).filter(runnable)
    } else if matches!(
        program.components().next(),
        Some(Component::CurDir | Component::ParentDir)
    ) {
        Some(cwd.join(program)).filter(runnable)
    } else {
        path.and_then(|paths| {
            std::env::split_paths(paths)
                .map(|dir| cwd.join(dir).join(program))
                .find(runnable)
        })
    };
    found.ok_or_else(|| TerminalError::Spawn("program could not be started".into()))
}

/// The shell the pty's own spawn names in `SHELL` when nothing else does.
#[cfg(target_os = "linux")]
fn login_shell() -> PathBuf {
    use nix::unistd::{AccessFlags, Uid, User, access};
    User::from_uid(Uid::current())
        .ok()
        .flatten()
        .map(|user| user.shell)
        .filter(|shell| access(shell, AccessFlags::X_OK).is_ok())
        .unwrap_or_else(|| "/bin/sh".into())
}

#[cfg(unix)]
fn default_shell() -> Result<PathBuf, TerminalError> {
    if let Some(shell) = std::env::var_os("SHELL").filter(|s| !s.is_empty()) {
        return Ok(shell.into());
    }
    let user = nix::unistd::User::from_uid(nix::unistd::Uid::current()).map_err(io::Error::from)?;
    Ok(user
        .map(|u| u.shell)
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| "/bin/sh".into()))
}

#[cfg(windows)]
fn default_shell() -> Result<PathBuf, TerminalError> {
    for executable in ["pwsh.exe", "powershell.exe", "cmd.exe"] {
        if let Some(path) = std::env::var_os("PATH").and_then(|paths| {
            std::env::split_paths(&paths)
                .map(|p| p.join(executable))
                .find(|p| p.is_file())
        }) {
            return Ok(path);
        }
    }
    Err(TerminalError::Spawn("no supported shell found".into()))
}

/// Blocks until the process has exited. WNOWAIT leaves it unreaped, so its
/// id cannot be reused before the caller's `try_wait` reaps it. Any failure
/// returns at once and leaves the caller to poll.
#[cfg(target_os = "linux")]
pub(crate) fn wait_exited(pid: u32) {
    use nix::{
        errno::Errno,
        sys::wait::{Id, WaitPidFlag, waitid},
        unistd::Pid,
    };
    let Ok(pid) = i32::try_from(pid) else {
        return;
    };
    while waitid(
        Id::Pid(Pid::from_raw(pid)),
        WaitPidFlag::WEXITED | WaitPidFlag::WNOWAIT,
    ) == Err(Errno::EINTR)
    {}
}

#[cfg(unix)]
#[derive(Debug)]
pub(crate) struct ProcessTree {
    pid: i32,
}

#[cfg(unix)]
impl ProcessTree {
    pub(crate) fn new(child: &dyn Child) -> io::Result<Self> {
        Ok(Self {
            pid: child
                .process_id()
                .and_then(|p| i32::try_from(p).ok())
                .ok_or_else(|| io::Error::other("missing terminal process identity"))?,
        })
    }

    pub(crate) fn kill(&self) -> io::Result<()> {
        use nix::{
            errno::Errno,
            sys::signal::{Signal, kill, killpg},
            unistd::Pid,
        };
        #[cfg(target_os = "linux")]
        {
            // Job-control shells create separate process groups in one session.
            // Include descendants that have created their own session as well.
            let mut processes = Vec::new();
            for entry in std::fs::read_dir("/proc")?.flatten() {
                let Ok(pid) = entry.file_name().to_string_lossy().parse::<i32>() else {
                    continue;
                };
                let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
                    continue;
                };
                let Some((_, tail)) = stat.rsplit_once(") ") else {
                    continue;
                };
                let fields: Vec<_> = tail.split_whitespace().take(4).collect();
                if fields.len() == 4
                    && let (Ok(parent), Ok(session)) =
                        (fields[1].parse::<i32>(), fields[3].parse::<i32>())
                {
                    processes.push((pid, parent, session));
                }
            }
            let mut owned = std::collections::BTreeSet::from([self.pid]);
            loop {
                let count = owned.len();
                for &(pid, parent, session) in &processes {
                    if session == self.pid || owned.contains(&parent) {
                        owned.insert(pid);
                    }
                }
                if count == owned.len() {
                    break;
                }
            }
            for pid in owned.into_iter().filter(|pid| *pid != self.pid) {
                match kill(Pid::from_raw(pid), Signal::SIGKILL) {
                    Ok(()) | Err(Errno::ESRCH) => {}
                    Err(e) => return Err(e.into()),
                }
            }
        }
        match killpg(Pid::from_raw(self.pid), Signal::SIGKILL) {
            Ok(()) | Err(Errno::ESRCH) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}

#[cfg(windows)]
#[path = "windows.rs"]
mod windows;
#[cfg(windows)]
pub(crate) use windows::ProcessTree;

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::io::Read;

    #[tokio::test]
    async fn dropping_input_does_not_send_a_newline_or_eof() {
        let pair = portable_pty::native_pty_system()
            .openpty(portable_pty::PtySize::default())
            .unwrap();
        let mut command = portable_pty::CommandBuilder::new("/bin/sh");
        command.args([
            "-c",
            "stty -echo; printf ready; read line; printf 'received:%s' \"$line\"",
        ]);
        let mut child = pair.slave.spawn_command(command).unwrap();
        drop(pair.slave);
        let mut reader = crate::input::Reader::new(pair.master.as_ref()).unwrap();
        let mut ready = [0; 5];
        reader.read_exact(&mut ready).unwrap();
        assert_eq!(&ready, b"ready");
        let input = crate::input::Input::new(pair.master.as_ref()).unwrap();
        drop(input);
        let input = crate::input::Input::new(pair.master.as_ref()).unwrap();
        input.write(b"hello\n").await.unwrap();
        let mut response = [0; 14];
        reader.read_exact(&mut response).unwrap();
        assert_eq!(&response, b"received:hello");
        assert!(child.wait().unwrap().success());
    }

    /// What the pty's own spawn and then [`spawn`] each ran as `program`,
    /// searching `path` from `cwd`: what the script they found wrote.
    #[cfg(target_os = "linux")]
    fn ran(cwd: &std::path::Path, program: &str, path: &str) -> [Option<String>; 2] {
        let out = cwd.join("out");
        let finished = |child: Option<Box<dyn Child + Send + Sync>>| {
            let written = child.and_then(|mut child| {
                child.wait().unwrap();
                std::fs::read_to_string(&out).ok()
            });
            let _ = std::fs::remove_file(&out);
            written
        };
        let pty = || {
            portable_pty::native_pty_system()
                .openpty(portable_pty::PtySize::default())
                .unwrap()
        };
        let theirs = pty();
        let mut command = portable_pty::CommandBuilder::new(program);
        command.cwd(cwd);
        command.env("PATH", path);
        command.env("OUT", &out);
        let theirs = finished(theirs.slave.spawn_command(command).ok());
        let ours = pty();
        let spec = TerminalSpec {
            title: String::new(),
            cwd: cwd.to_owned(),
            program: Some((program.into(), vec![])),
            env: vec![
                ("PATH".into(), path.into()),
                ("OUT".into(), out.to_string_lossy().into_owned()),
            ],
            project_id: None,
            harness: None,
            cols: 80,
            rows: 24,
        };
        let ours = finished(spawn(ours.master.as_ref(), &spec, None).ok());
        [theirs, ours]
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_program_is_found_where_the_ptys_own_spawn_finds_it() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path();
        let script = |path: &str, word: &str, mode: u32| {
            let path = cwd.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, format!("#!/bin/sh\necho {word} \"$0\" >\"$OUT\"\n")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        };
        // Others may run it but this user may not, before one this user may.
        script("locked/tool", "locked", 0o011);
        script("open/tool", "open", 0o755);
        std::fs::create_dir_all(cwd.join("folder/tool")).unwrap();
        // A name with a separator in it is looked for along PATH, even from a
        // relative entry, and not in the directory.
        script("bin/tool", "here", 0o755);
        script("relative/bin/tool", "along", 0o755);
        script("dot", "dot", 0o755);
        let at = |path: &str| cwd.join(path).to_string_lossy().into_owned();
        // Root may run what anyone may.
        let locked = if nix::unistd::Uid::effective().is_root() {
            "locked"
        } else {
            "open"
        };
        let cases = [
            (
                "tool",
                format!("{}:{}", at("locked"), at("open")),
                Some(locked),
            ),
            (
                "tool",
                format!("{}:{}", at("folder"), at("open")),
                Some("open"),
            ),
            (
                "bin/tool",
                format!("{}:relative", at("nowhere")),
                Some("along"),
            ),
            ("./dot", String::new(), Some("dot")),
            ("../missing/dot", at("open"), None),
            ("./bin", at("open"), None),
            ("missing", at("open"), None),
            (&at("folder/tool"), at("open"), None),
            (&at("open/tool"), String::new(), Some("open")),
        ];
        for (program, path, word) in cases {
            let [theirs, ours] = ran(cwd, program, &path);
            assert_eq!(ours, theirs, "{program} along {path}");
            assert_eq!(
                ours.as_deref().and_then(|out| out.split(' ').next()),
                word,
                "{program} along {path}"
            );
        }
    }
}
