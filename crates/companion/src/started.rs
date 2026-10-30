// SPDX-License-Identifier: Apache-2.0
//! Which local programs this companion started, from what the operating system
//! reports about them rather than anything they say. Coding agents, the
//! programs they run and the shells opened for a device all run as the owner's
//! user, so the user id alone cannot tell them from the owner's own terminal.

#[cfg(unix)]
pub use unix::Started;
#[cfg(windows)]
pub use windows::Started;

#[cfg(unix)]
mod unix {
    use std::io;

    /// Longest parent chain followed before a program counts as started here.
    const MAX_ANCESTORS: usize = 1024;

    #[derive(Debug, Clone)]
    pub struct Started {
        companion: u32,
        /// The companion's control group, when only what it started shares
        /// it, as under a service manager. A program keeps its group however it
        /// detaches from its parent, unless it asks the system to move it.
        cgroup: Option<String>,
    }
    impl Started {
        pub fn claim() -> io::Result<Self> {
            Self::of(std::process::id())
        }

        /// What program `companion` will have started, judged before it starts
        /// any.
        pub fn of(companion: u32) -> io::Result<Self> {
            let own = cgroup(companion)?;
            // Started from a shell, the companion shares that shell's group, and
            // with it the owner's own programs; only ancestry tells them apart.
            // The shell may have exited already, leaving the companion to a
            // parent elsewhere, so any other program in the group shares it too.
            let separate = parent(companion)
                .and_then(cgroup)
                .is_ok_and(|parent| !within(&parent, &own))
                && !others(&own, companion).unwrap_or(false);
            Ok(Self {
                companion,
                cgroup: separate.then_some(own),
            })
        }

        /// Whether `pid` is a program the companion started or one of theirs.
        /// A program whose lineage cannot be read counts as started.
        pub fn includes(&self, pid: u32) -> bool {
            if pid == self.companion {
                return false;
            }
            if cgroup(pid).is_ok_and(|peer| self.started_group(&peer)) {
                return true;
            }
            self.descends(pid).unwrap_or(true)
        }

        /// Whether a program in cgroup `peer` was started here, by its group
        /// alone.
        ///
        /// A companion's agent runs and the terminals it opens for devices get
        /// cgroups of their own, beside it in its group. What they start keeps
        /// its cgroup however it detaches, even when the companion shares its
        /// group with the owner's shell or with another program. A terminal's
        /// programs may outlive the companion that opened it, and the group it
        /// left behind still bears that companion's process id, so any
        /// companion's group counts: none of the owner's own programs is in
        /// one.
        fn started_group(&self, peer: &str) -> bool {
            self.cgroup.as_ref().is_some_and(|own| within(peer, own))
                || peer
                    .split('/')
                    .any(|name| ports::run_cgroup_companion(name).is_some())
        }

        fn descends(&self, pid: u32) -> io::Result<bool> {
            let mut current = pid;
            for _ in 0..MAX_ANCESTORS {
                let parent = parent(current)?;
                if parent == self.companion {
                    return Ok(true);
                }
                if parent <= 1 {
                    return Ok(false);
                }
                current = parent;
            }
            Ok(true)
        }
    }

    fn within(path: &str, group: &str) -> bool {
        group == "/"
            || path == group
            || path
                .strip_prefix(group)
                .is_some_and(|rest| rest.starts_with('/'))
    }

    fn cgroup(pid: u32) -> io::Result<String> {
        let text = std::fs::read_to_string(format!("/proc/{pid}/cgroup"))?;
        // The unified hierarchy, or systemd's own where only legacy ones exist.
        text.lines()
            .find_map(|line| line.strip_prefix("0::"))
            .or_else(|| {
                text.lines()
                    .find_map(|line| line.split_once(":name=systemd:").map(|(_, path)| path))
            })
            .map(str::to_owned)
            .ok_or_else(|| io::Error::other("process has no control group"))
    }

    /// Whether any program but `companion` is in `group` or a group below it.
    fn others(group: &str, companion: u32) -> io::Result<bool> {
        let unified = std::path::Path::new("/sys/fs/cgroup");
        let relative = group.trim_start_matches('/');
        let mut pending = vec![unified.join(relative)];
        if !pending[0].join("cgroup.procs").exists() {
            // Only legacy hierarchies, where systemd keeps its own.
            pending[0] = unified.join("systemd").join(relative);
        }
        while let Some(dir) = pending.pop() {
            let procs = std::fs::read_to_string(dir.join("cgroup.procs"))?;
            if procs
                .lines()
                .any(|pid| pid.trim().parse::<u32>().is_ok_and(|pid| pid != companion))
            {
                return Ok(true);
            }
            for entry in std::fs::read_dir(&dir)? {
                let entry = entry?;
                if entry.file_type()?.is_dir() {
                    pending.push(entry.path());
                }
            }
        }
        Ok(false)
    }

    fn parent(pid: u32) -> io::Result<u32> {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"))?;
        stat.rsplit_once(") ")
            .and_then(|(_, fields)| fields.split_whitespace().nth(1)?.parse().ok())
            .ok_or_else(|| io::Error::other("unreadable process status"))
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::process::{Command, Stdio};

        /// A `sleep` started in the background of a shell, killed on drop.
        struct Sleeper(u32);
        impl Sleeper {
            /// The shell exits at once, the way `nohup … &` detaches a program.
            fn detached() -> Self {
                let output = Command::new("/bin/sh")
                    .args(["-c", "sleep 30 </dev/null >/dev/null 2>&1 & echo $!"])
                    .stdout(Stdio::piped())
                    .output()
                    .unwrap();
                Self(
                    String::from_utf8(output.stdout)
                        .unwrap()
                        .trim()
                        .parse()
                        .unwrap(),
                )
            }
        }
        impl Drop for Sleeper {
            fn drop(&mut self) {
                let _ = rustix::process::kill_process(
                    rustix::process::Pid::from_raw(self.0 as i32).unwrap(),
                    rustix::process::Signal::KILL,
                );
            }
        }

        #[test]
        fn children_and_their_descendants_are_started_here_and_the_rest_are_not() {
            let started = Started::claim().unwrap();
            let mut shell = Command::new("/bin/sh")
                .args(["-c", "sleep 30 </dev/null >/dev/null 2>&1 & echo $!; wait"])
                .stdout(Stdio::piped())
                .spawn()
                .unwrap();
            let mut line = String::new();
            std::io::BufRead::read_line(
                &mut std::io::BufReader::new(shell.stdout.take().unwrap()),
                &mut line,
            )
            .unwrap();
            let grandchild = Sleeper(line.trim().parse().unwrap());
            let result = (started.includes(shell.id()), started.includes(grandchild.0));
            drop(grandchild);
            shell.wait().unwrap();
            assert_eq!(result, (true, true));
            assert!(!started.includes(std::process::id()));
            let parent = rustix::process::getppid().unwrap().as_raw_nonzero().get();
            assert!(!started.includes(parent.unsigned_abs()));
            // A program that is gone counts as started.
            assert!(started.includes(u32::MAX));
        }

        #[test]
        fn a_detached_program_keeps_the_companions_own_group() {
            let sleeper = Sleeper::detached();
            // Under a service manager the group is the companion's alone.
            let own = Started {
                companion: std::process::id(),
                cgroup: Some(cgroup(std::process::id()).unwrap()),
            };
            let shared = Started {
                cgroup: None,
                ..own.clone()
            };
            assert!(own.includes(sleeper.0));
            assert!(!shared.includes(sleeper.0));
        }

        #[test]
        fn a_shell_that_has_exited_still_shares_its_group_with_the_owner() {
            // Started with `nohup … &` from a shell that exits at once, the
            // companion has lost the parent that shared its group.
            let companion = Sleeper::detached();
            let started = Started::of(companion.0).unwrap();
            assert!(!started.includes(std::process::id()));
        }

        #[test]
        fn a_companions_group_is_started_here_wherever_the_companion_runs() {
            let shell = Started {
                companion: 123,
                cgroup: None,
            };
            let service = Started {
                cgroup: Some("/app.slice/ophio.service".into()),
                ..shell.clone()
            };
            for started in [&shell, &service] {
                assert!(started.started_group("/user.slice/app.scope/ophio-run-123-0"));
                assert!(started.started_group("/user.slice/app.scope/ophio-run-123-7/nested"));
                // Left behind by an earlier companion, whose terminal's
                // programs outlived it.
                assert!(started.started_group("/user.slice/app.scope/ophio-run-1234-0"));
                assert!(started.started_group("/app.slice/ophio.service/ophio-run-99-2"));
                assert!(!started.started_group("/user.slice/app.scope"));
                assert!(!started.started_group("/user.slice/app.scope/other"));
                assert!(!started.started_group("/user.slice/ophio-run-123-"));
                assert!(!started.started_group("/user.slice/ophio-run-x-0"));
                assert!(!started.started_group("/user.slice/ophio-run-1-2-3"));
                assert!(!started.started_group("/user.slice/my-ophio-run-1-2"));
            }
            assert!(!shell.started_group("/app.slice/ophio.service"));
            assert!(service.started_group("/app.slice/ophio.service"));
        }

        #[test]
        fn a_group_contains_itself_and_what_is_below_it() {
            assert!(within(
                "/app.slice/ophio.service",
                "/app.slice/ophio.service"
            ));
            assert!(within(
                "/app.slice/ophio.service/x",
                "/app.slice/ophio.service"
            ));
            assert!(!within(
                "/app.slice/ophio.service2",
                "/app.slice/ophio.service"
            ));
            assert!(!within("/app.slice", "/app.slice/ophio.service"));
            assert!(within("/anything", "/"));
        }
    }
}

#[cfg(windows)]
mod windows {
    use std::{io, sync::Arc};

    /// Every program the companion starts after [`Started::claim`] inherits its
    /// job and cannot leave it.
    #[derive(Debug, Clone)]
    pub struct Started(Arc<platform_windows::started::Job>);
    impl Started {
        pub fn claim() -> io::Result<Self> {
            Ok(Self(Arc::new(platform_windows::started::Job::claim()?)))
        }

        /// Whether `pid` is a program the companion started or one of theirs.
        /// A program that cannot be inspected counts as started.
        pub fn includes(&self, pid: u32) -> bool {
            pid != std::process::id() && self.0.includes(pid).unwrap_or(true)
        }
    }
}
