// SPDX-License-Identifier: Apache-2.0
//! One container per agent run. Stop, the end of a run and the companion's own
//! shutdown end every process in it, including work the agent moved into the
//! background, so a run leaves nothing running behind it.
//!
//! On Linux each run gets a cgroup of its own inside the companion's cgroup,
//! ended with `cgroup.kill`. A process cannot leave it by starting a new
//! session or process group, or by being orphaned. Where the companion cannot
//! make one (no cgroup v2, a kernel older than 5.14, or a cgroup it does not
//! own), the run keeps only its own process group, which `setsid` leaves, and
//! [`Container::containment`] says why. On Windows the agent joins a job that
//! allows no breakaway before any of its code runs.
use ports::{Containment, HarnessError};
use std::sync::Arc;
use tokio::process::{Child, Command};

#[cfg(any(windows, test))]
mod accounting;
#[cfg(target_os = "linux")]
mod cgroup;
#[cfg(unix)]
mod group;
#[cfg(windows)]
#[allow(unsafe_code)]
mod job;
#[cfg(unix)]
mod leader;

#[derive(Debug)]
pub struct Container {
    inner: Inner,
    /// The agent, once started.
    #[cfg(unix)]
    leader: std::sync::OnceLock<leader::Leader>,
    /// Wakes a [`Guard::confirm_end`] waiting to try again.
    retry: tokio::sync::Notify,
    /// How many more ends a test asked to leave unconfirmed.
    #[cfg(any(test, feature = "test-hooks"))]
    unconfirmed: std::sync::atomic::AtomicU32,
    /// A test asked that tries again wait until they are woken.
    #[cfg(any(test, feature = "test-hooks"))]
    held: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct ResourceLimits {
    pub memory_bytes: u64,
    /// CPU milliseconds per second for the whole process tree.
    pub cpu_millis_per_second: u32,
    pub processes: u32,
}

#[derive(Debug)]
enum Inner {
    #[cfg(target_os = "linux")]
    Cgroup(cgroup::Cgroup),
    #[cfg(unix)]
    Group(group::Group, String),
    #[cfg(windows)]
    Job(job::Job),
}

impl Container {
    /// Requires aggregate limits; process-group fallback is never sufficient.
    pub fn limited(limits: ResourceLimits) -> Result<Self, HarnessError> {
        if limits.memory_bytes == 0 || limits.cpu_millis_per_second == 0 || limits.processes == 0 {
            return Err(HarnessError::Unsupported(
                "Resource limits must be positive".into(),
            ));
        }
        #[cfg(target_os = "linux")]
        return cgroup::Cgroup::limited(limits)
            .map(|group| Self::of(Inner::Cgroup(group)))
            .map_err(HarnessError::Unsupported);
        #[cfg(not(target_os = "linux"))]
        Err(HarnessError::Unsupported(
            "Aggregate resource limits require a delegated Linux cgroup".into(),
        ))
    }
    /// An empty container for one run.
    pub fn new() -> Result<Self, HarnessError> {
        #[cfg(all(unix, any(test, feature = "test-hooks")))]
        if hooks::grouping() {
            return Ok(Self::of(Inner::Group(
                group::Group::default(),
                "a test asked for the process group alone".into(),
            )));
        }
        #[cfg(target_os = "linux")]
        return Ok(Self::of(match cgroup::Cgroup::create() {
            Ok(cgroup) => Inner::Cgroup(cgroup),
            Err(reason) => Inner::Group(group::Group::default(), reason),
        }));
        #[cfg(all(unix, not(target_os = "linux")))]
        return Ok(Self::of(Inner::Group(
            group::Group::default(),
            "this system has no cgroups".into(),
        )));
        #[cfg(windows)]
        return Ok(Self::of(Inner::Job(job::Job::new()?)));
    }

    fn of(inner: Inner) -> Self {
        Self {
            inner,
            #[cfg(unix)]
            leader: std::sync::OnceLock::new(),
            retry: tokio::sync::Notify::new(),
            #[cfg(any(test, feature = "test-hooks"))]
            unconfirmed: hooks::asked().into(),
            #[cfg(any(test, feature = "test-hooks"))]
            held: hooks::holding(),
        }
    }

    /// Only the agent's process group, whatever the computer offers.
    #[cfg(all(test, target_os = "linux"))]
    fn process_group(reason: &str) -> Self {
        Self::of(Inner::Group(group::Group::default(), reason.into()))
    }

    pub fn containment(&self) -> Containment {
        match &self.inner {
            #[cfg(target_os = "linux")]
            Inner::Cgroup(_) => Containment::Complete,
            #[cfg(unix)]
            Inner::Group(_, reason) => Containment::ProcessGroup {
                reason: reason.clone(),
            },
            #[cfg(windows)]
            Inner::Job(_) => Containment::Complete,
        }
    }

    /// Start `command` inside the container. No instruction of the new
    /// process runs before it is inside, so nothing it starts is outside.
    ///
    /// Wait for the child's exit without reaping it, and reap it only once
    /// [`Container::end`] has been confirmed, which it is only once the child
    /// has exited: a process group is named by its leader's id, which could
    /// otherwise be reused by another group.
    pub async fn spawn(&self, command: &mut Command) -> Result<Child, HarnessError> {
        #[cfg(unix)]
        let spawn = |command: &mut Command| {
            command
                .spawn()
                .map_err(|e| HarnessError::SpawnFailed(e.to_string()))
        };
        match &self.inner {
            #[cfg(target_os = "linux")]
            Inner::Cgroup(cgroup) => {
                command.process_group(0);
                cgroup
                    .prepare(command.as_std_mut())
                    .map_err(|e| HarnessError::SpawnFailed(e.to_string()))?;
                self.started(spawn(command)?)
            }
            #[cfg(unix)]
            Inner::Group(..) => {
                command.process_group(0);
                self.started(spawn(command)?)
            }
            #[cfg(windows)]
            Inner::Job(job) => job.spawn(command).await,
        }
    }

    /// Have the process `command` starts join the container before its
    /// program runs, for a program that makes a session of its own, as one
    /// on a terminal does: the process group [`Container::spawn`] makes would
    /// keep it from making one. Call this before the command's other steps
    /// between fork and exec, and [`Container::adopt`] once it has started.
    /// Where the container is only a process group, the program's session
    /// leads the group that is ended.
    #[cfg(unix)]
    pub fn prepare_session(&self, command: &mut std::process::Command) -> Result<(), HarnessError> {
        match &self.inner {
            #[cfg(target_os = "linux")]
            Inner::Cgroup(cgroup) => cgroup
                .prepare(command)
                .map_err(|e| HarnessError::SpawnFailed(e.to_string())),
            Inner::Group(..) => Ok(()),
        }
    }

    /// The program a command prepared with [`Container::prepare_session`]
    /// started, as process `id`, which must make a session of its own before
    /// its program runs. As with [`Container::spawn`], leave it unreaped
    /// until the container's end is confirmed.
    #[cfg(unix)]
    pub fn adopt(&self, id: u32) {
        if let Some(leader) = leader::Leader::of_id(id) {
            let _ = self.leader.set(leader);
        }
    }

    /// Have a process started outside [`Container::spawn`], such as one on a
    /// pseudo console, join the container. Only what it starts after joining
    /// is inside, so start nothing else before this.
    #[cfg(windows)]
    pub fn adopt(&self, process: std::os::windows::io::RawHandle) -> Result<(), HarnessError> {
        match &self.inner {
            Inner::Job(job) => job.adopt(process),
        }
    }

    #[cfg(unix)]
    fn started(&self, child: Child) -> Result<Child, HarnessError> {
        if let Some(leader) = leader::Leader::of(&child) {
            let _ = self.leader.set(leader);
        }
        Ok(child)
    }

    /// As if the agent had been reaped and its id taken by another group.
    #[cfg(all(test, target_os = "linux"))]
    fn as_if_reused(&self, pid: rustix::process::Pid, reaped: std::os::fd::OwnedFd) {
        let _ = self.leader.set(leader::Leader::reaped(pid, reaped));
    }

    /// Kill every process in the container without waiting for them to end.
    pub fn kill(&self) -> Result<(), HarnessError> {
        match &self.inner {
            #[cfg(target_os = "linux")]
            Inner::Cgroup(cgroup) => cgroup.kill(),
            #[cfg(unix)]
            Inner::Group(group, _) => group.kill(self.leader.get()),
            #[cfg(windows)]
            Inner::Job(job) => job.terminate(),
        }
    }

    /// Kill every process in the container and confirm that none is left,
    /// and that the agent itself has exited. What drives the run kills the
    /// agent. Ending an ended container confirms it again.
    #[allow(deprecated)] // fetch_update supports the workspace MSRV; renamed in newer Rust.
    pub async fn end(&self) -> Result<(), HarnessError> {
        let ended = self.end_inner().await;
        #[cfg(any(test, feature = "test-hooks"))]
        if ended.is_ok()
            && self
                .unconfirmed
                .fetch_update(
                    std::sync::atomic::Ordering::SeqCst,
                    std::sync::atomic::Ordering::SeqCst,
                    |left| left.checked_sub(1),
                )
                .is_ok()
        {
            return Err(unconfirmed("A test left this end unconfirmed"));
        }
        ended
    }

    /// Have a [`Guard::confirm_end`] try again now rather than when its
    /// wait is over: someone asked for the run to stop.
    pub fn retry_now(&self) {
        self.retry.notify_one();
    }

    fn retry_after(&self, wait: std::time::Duration) -> std::time::Duration {
        #[cfg(any(test, feature = "test-hooks"))]
        if self.held {
            return std::time::Duration::from_secs(3600);
        }
        wait
    }

    async fn end_inner(&self) -> Result<(), HarnessError> {
        match &self.inner {
            #[cfg(target_os = "linux")]
            Inner::Cgroup(cgroup) => cgroup.end().await,
            #[cfg(unix)]
            Inner::Group(group, _) => group.end(self.leader.get()).await,
            // No process can leave a job, the agent included.
            #[cfg(windows)]
            Inner::Job(job) => {
                job.terminate()?;
                job.confirm().await
            }
        }?;
        #[cfg(unix)]
        if let Some(leader) = self.leader.get() {
            leader.exited().await?;
        }
        Ok(())
    }
}

/// Held by the task that drives a run. It kills the container if that task
/// is cancelled or unwinds, even while a controller still holds the container
/// to confirm its end later.
#[derive(Debug)]
pub struct Guard(Arc<Container>);
impl Guard {
    pub fn new(container: Arc<Container>) -> Self {
        Self(container)
    }
    pub async fn end(&self) -> Result<(), HarnessError> {
        self.0.end().await
    }
    /// End the container again and again until its end is confirmed, waiting
    /// longer between tries, or less when [`Container::retry_now`] wakes it.
    /// Call it once an end could not be confirmed: until one is, the run may
    /// still have processes on the computer.
    pub async fn confirm_end(&self) {
        let mut wait = RETRY_FIRST;
        loop {
            tokio::select! {
                () = tokio::time::sleep(self.0.retry_after(wait)) => {}
                () = self.0.retry.notified() => {}
            }
            if self.0.end().await.is_ok() {
                return;
            }
            wait = (wait * 2).min(RETRY_AT_MOST);
        }
    }
}
impl Drop for Guard {
    fn drop(&mut self) {
        let _ = self.0.kill();
    }
}

/// A cgroup of its own for a terminal opened for a device, made beside the
/// runs' in the companion's cgroup and named like them, so the companion knows
/// what the terminal starts as its own however that detaches. The terminal's
/// process joins it before its program runs. It ends nothing: what the
/// terminal leaves running keeps the group, which is removed once that has
/// ended, before the companion makes another.
#[cfg(target_os = "linux")]
#[derive(Debug)]
pub struct TerminalGroup(cgroup::Cgroup);

#[cfg(target_os = "linux")]
impl TerminalGroup {
    /// The reason is for the person, when the computer cannot make one.
    pub fn new() -> Result<Self, String> {
        cgroup::Cgroup::create().map(Self)
    }

    /// Have the process `command` starts join the group before its program
    /// runs, before even the program's loader, so all it starts runs inside.
    /// Should joining fail, the spawn fails and the program never runs. Add
    /// this before the command's other steps between fork and exec.
    pub fn prepare(&self, command: &mut std::process::Command) -> std::io::Result<()> {
        self.0.prepare(command)
    }

    /// Whether anything besides the terminal shell remains, including
    /// descendants that detached or created a nested cgroup.
    pub fn has_other_processes(&self, shell: u32) -> std::io::Result<bool> {
        self.0.has_other_processes(shell)
    }
}

#[cfg(target_os = "linux")]
impl Drop for TerminalGroup {
    fn drop(&mut self) {
        self.0.release();
    }
}

/// Shown in the task when a run ended but its container could not be
/// confirmed empty.
pub const STILL_RUNNING: &str =
    "Some of what the agent started may still be running on the computer.";

/// How long ending a container may take before it is reported unconfirmed.
const END_WITHIN: std::time::Duration = std::time::Duration::from_secs(3);
/// How long [`Guard::confirm_end`] waits before trying again, at first and at most.
const RETRY_FIRST: std::time::Duration = std::time::Duration::from_secs(1);
const RETRY_AT_MOST: std::time::Duration = std::time::Duration::from_secs(60);

/// For tests of what drives a run: a container made inside
/// [`hooks::unconfirmed`] ends everything it holds, but reports its first
/// few ends as unconfirmed, as a process that will not die would leave them.
/// One made inside [`hooks::held`] waits an hour before each try again,
/// unless [`Container::retry_now`] wakes it. One made inside
/// [`hooks::process_group`] keeps only the agent's process group, as where
/// the computer offers nothing wider.
#[cfg(any(test, feature = "test-hooks"))]
pub mod hooks {
    tokio::task_local! {
        static UNCONFIRMED: u32;
        static HELD: ();
        static GROUP: ();
    }
    /// Run `f`, and any container made within it reports its first `ends`
    /// ends as unconfirmed.
    pub async fn unconfirmed<F: Future>(ends: u32, f: F) -> F::Output {
        UNCONFIRMED.scope(ends, f).await
    }
    /// Run `f`, and any container made within it tries again only when woken.
    pub async fn held<F: Future>(f: F) -> F::Output {
        HELD.scope((), f).await
    }
    /// Run `f`, and any container made within it keeps only the agent's
    /// process group.
    pub async fn process_group<F: Future>(f: F) -> F::Output {
        GROUP.scope((), f).await
    }
    #[cfg(unix)]
    pub(crate) fn grouping() -> bool {
        GROUP.try_with(|()| ()).is_ok()
    }
    pub(crate) fn asked() -> u32 {
        UNCONFIRMED.try_with(|ends| *ends).unwrap_or(0)
    }
    pub(crate) fn holding() -> bool {
        HELD.try_with(|()| ()).is_ok()
    }
}

fn unconfirmed(reason: impl std::fmt::Display) -> HarnessError {
    HarnessError::Protocol(format!(
        "{reason}; it is not confirmed that the run's processes ended"
    ))
}

#[cfg(test)]
mod tests;
