// SPDX-License-Identifier: Apache-2.0
//! The agent itself, the process a container starts. What drives a run leaves
//! it unreaped until the container's end is confirmed, and that end is
//! confirmed only once it has exited: it may have left its process group, or
//! been killed but not exited yet, so an empty container doesn't show that.
use crate::{END_WITHIN, unconfirmed};
use ports::HarnessError;
use rustix::{
    io::Errno,
    process::{Pid, WaitId, WaitIdOptions, waitid},
};
use std::time::Duration;
use tokio::process::Child;

#[derive(Debug)]
pub(crate) struct Leader {
    pub(crate) pid: Pid,
    /// Names the leader itself rather than its id, so it still says whether
    /// the leader has been reaped once another process could have that id.
    #[cfg(target_os = "linux")]
    pidfd: Option<std::os::fd::OwnedFd>,
}

impl Leader {
    pub(crate) fn of(child: &Child) -> Option<Self> {
        Self::of_id(child.id()?)
    }

    /// The process `id`, a child of the companion that nothing has waited for.
    pub(crate) fn of_id(id: u32) -> Option<Self> {
        let pid = i32::try_from(id).ok().and_then(Pid::from_raw)?;
        Some(Self {
            pid,
            // Nothing has waited for the new child yet, so this is it.
            #[cfg(target_os = "linux")]
            pidfd: rustix::process::pidfd_open(pid, rustix::process::PidfdFlags::empty()).ok(),
        })
    }

    /// As if the leader had been reaped and its id taken by another group.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn reaped(pid: Pid, pidfd: std::os::fd::OwnedFd) -> Self {
        Self {
            pid,
            pidfd: Some(pidfd),
        }
    }

    /// Whether the leader has exited, without reaping it: `None` once it has
    /// been reaped.
    fn exit(&self) -> rustix::io::Result<Option<bool>> {
        #[cfg(target_os = "linux")]
        let id = match &self.pidfd {
            Some(pidfd) => WaitId::PidFd(std::os::fd::AsFd::as_fd(pidfd)),
            None => WaitId::Pid(self.pid),
        };
        #[cfg(not(target_os = "linux"))]
        let id = WaitId::Pid(self.pid);
        match waitid(
            id,
            WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
        ) {
            Ok(status) => Ok(Some(status.is_some())),
            Err(Errno::CHILD) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// The leader has not been reaped, so its id still names it and its
    /// group. Without a pidfd only the order kept by what drives the run
    /// protects the id.
    pub(crate) fn unreaped(&self) -> bool {
        #[cfg(target_os = "linux")]
        if self.pidfd.is_some() {
            return matches!(self.exit(), Ok(Some(_)));
        }
        true
    }

    /// Wait for the leader to exit, as a confirmed end needs. What drives the
    /// run kills it.
    pub(crate) async fn exited(&self) -> Result<(), HarnessError> {
        let deadline = tokio::time::Instant::now() + END_WITHIN;
        loop {
            match self.exit() {
                Ok(Some(true) | None) => return Ok(()),
                Ok(Some(false)) if tokio::time::Instant::now() >= deadline => {
                    return Err(unconfirmed("The agent itself hasn't exited"));
                }
                Ok(Some(false)) => {}
                Err(e) => {
                    return Err(unconfirmed(format!(
                        "Looking for the agent's exit: {}",
                        std::io::Error::from(e)
                    )));
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}
