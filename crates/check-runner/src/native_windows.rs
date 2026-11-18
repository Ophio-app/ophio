// SPDX-License-Identifier: Apache-2.0
//! Windows check containment, not a limited-access sandbox. The persisted random
//! Job name is the tree identity; a process id alone never authorizes a stop.
#![allow(unsafe_code)]

use crate::runner::{
    PartialProcessIdentity, ProcessIdentity, ProcessState, StartError, StopStatus,
};
use containment::NamedJob;
use std::{
    io,
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
};
use tokio::process::{Child, Command};
use windows_sys::Win32::{
    Foundation::{ERROR_INVALID_PARAMETER, FILETIME, WAIT_OBJECT_0, WAIT_TIMEOUT},
    System::Threading::{
        GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
        WaitForSingleObject,
    },
};

#[derive(Debug)]
pub(crate) struct Handle(NamedJob);

pub(crate) async fn spawn(
    command: &mut Command,
    name: &str,
) -> Result<(Child, Handle), StartError> {
    let not_started = |error: String| StartError::NotStarted { reason: error };
    validate_name(name).map_err(|error| not_started(error.to_string()))?;
    let job = NamedJob::new(name).map_err(|error| not_started(error.to_string()))?;
    let child = match job.spawn(command).await {
        Ok(child) => child,
        Err(error) => {
            let reason = error.to_string();
            return match job.end().await {
                Ok(()) => Err(not_started(reason)),
                Err(cleanup) => Err(StartError::Uncertain {
                    reason: format!("{reason}. Cleanup could not be confirmed: {cleanup}"),
                    identity: PartialProcessIdentity {
                        pid: None,
                        start_time: None,
                        sandbox_unit: Some(name.into()),
                        project: command
                            .as_std()
                            .get_current_dir()
                            .unwrap_or_else(|| std::path::Path::new(""))
                            .into(),
                        boot_id: boot_id().ok(),
                    }
                    .into(),
                }),
            };
        }
    };
    Ok((child, Handle(job)))
}

impl Handle {
    pub(crate) async fn stop(&self) -> StopStatus {
        match self.0.end().await {
            Ok(()) => StopStatus::Confirmed,
            Err(error) => uncertain(error),
        }
    }
}

/// Absolute FILETIME is independent of process id reuse and computer restarts.
pub(crate) fn stamp(pid: u32) -> io::Result<Option<u64>> {
    let Some(process) = open_process(pid)? else {
        return Ok(None);
    };
    creation_time(&process).map(Some)
}

pub(crate) async fn exited(child: &mut Child) -> io::Result<()> {
    let handle = child
        .raw_handle()
        .ok_or_else(|| io::Error::other("The native check process handle is unavailable."))?;
    // Keep only an integer across the await. The caller's mutable borrow keeps
    // the Child, and therefore its native process handle, alive until return.
    let handle = handle as usize;
    loop {
        // SAFETY: the borrowed Child still owns this handle with synchronization
        // rights. Polling waits for actual exit without reaping the Child.
        match unsafe { WaitForSingleObject(handle as _, 0) } {
            WAIT_OBJECT_0 => return Ok(()),
            WAIT_TIMEOUT => tokio::time::sleep(std::time::Duration::from_millis(10)).await,
            _ => return Err(io::Error::last_os_error()),
        }
    }
}

fn live_stamp(pid: u32) -> io::Result<Option<u64>> {
    let Some(process) = open_process(pid)? else {
        return Ok(None);
    };
    // SAFETY: the owned query/synchronization handle stays live for this poll.
    match unsafe { WaitForSingleObject(process.as_raw_handle(), 0) } {
        WAIT_OBJECT_0 => return Ok(None),
        WAIT_TIMEOUT => {}
        _ => return Err(io::Error::last_os_error()),
    }
    creation_time(&process).map(Some)
}

fn creation_time(process: &OwnedHandle) -> io::Result<u64> {
    let mut born = FILETIME::default();
    let mut exited = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    // SAFETY: the handle and all four initialized writable buffers stay live.
    if unsafe {
        GetProcessTimes(
            process.as_raw_handle(),
            &mut born,
            &mut exited,
            &mut kernel,
            &mut user,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok((u64::from(born.dwHighDateTime) << 32) | u64::from(born.dwLowDateTime))
}

pub(crate) fn boot_id() -> io::Result<String> {
    // Unlike Linux's start ticks, Windows FILETIME is an absolute timestamp.
    Ok("windows-filetime-v1".into())
}

pub(crate) async fn inspect(identity: &ProcessIdentity) -> ProcessState {
    match recover(identity) {
        Ok(Some(job)) => match job.has_processes() {
            Ok(true) => ProcessState::Running,
            Ok(false) => ProcessState::Uncertain {
                reason: "The recovered check has no active processes, but their complete exit could not be confirmed.".into(),
            },
            Err(error) => ProcessState::Uncertain { reason: error.to_string() },
        },
        Ok(None) => ProcessState::Stopped,
        Err(error) => ProcessState::Uncertain { reason: error.to_string() },
    }
}

pub(crate) async fn stop(identity: &ProcessIdentity) -> StopStatus {
    match recover(identity) {
        Ok(Some(job)) => match job.end().await {
            Ok(()) => StopStatus::Confirmed,
            Err(error) => uncertain(error),
        },
        Ok(None) => StopStatus::Confirmed,
        Err(error) => uncertain(error),
    }
}

fn recover(identity: &ProcessIdentity) -> io::Result<Option<NamedJob>> {
    if identity.boot_id != boot_id()? || identity.start_time == 0 || identity.pid == 0 {
        return Err(io::Error::other(
            "The saved check process identity is invalid.",
        ));
    }
    let name = identity
        .sandbox_unit
        .as_deref()
        .ok_or_else(|| io::Error::other("The saved check has no native process group identity."))?;
    validate_name(name)?;
    let job = NamedJob::open(name)?;
    let current = live_stamp(identity.pid)?;
    match (&job, current) {
        (Some(job), Some(time)) => {
            if time != identity.start_time
                || !job
                    .contains_identity(identity.pid, identity.start_time)
                    .map_err(io::Error::other)?
            {
                return Err(io::Error::other(
                    "The saved check process identity no longer matches its native process group.",
                ));
            }
        }
        (None, Some(time)) if time == identity.start_time => {
            return Err(io::Error::other(
                "The check process is still running, but its native process group could not be found.",
            ));
        }
        // The random Job identity owns descendants even after the root exits.
        // If the Job is absent and the id belongs to a different process, that
        // unrelated process is never signaled or considered part of the check.
        _ => {}
    }
    Ok(job)
}

fn open_process(pid: u32) -> io::Result<Option<OwnedHandle>> {
    if pid == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "The process id is invalid.",
        ));
    }
    // SAFETY: opens only read/synchronization rights, never termination rights,
    // and does not allow this handle to be inherited by a child.
    let process = unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
            0,
            pid,
        )
    };
    if process.is_null() {
        let error = io::Error::last_os_error();
        return if error.raw_os_error() == Some(ERROR_INVALID_PARAMETER as i32) {
            Ok(None)
        } else {
            Err(error)
        };
    }
    // SAFETY: OpenProcess returned a valid handle owned exactly once here.
    Ok(Some(unsafe { OwnedHandle::from_raw_handle(process) }))
}

fn validate_name(name: &str) -> io::Result<()> {
    let valid = name
        .strip_prefix("ophio-check-")
        .and_then(|id| uuid::Uuid::parse_str(id).ok())
        .is_some_and(|id| !id.is_nil());
    if !valid {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "The native check process group name is invalid.",
        ));
    }
    Ok(())
}

fn uncertain(error: impl std::fmt::Display) -> StopStatus {
    StopStatus::Uncertain {
        reason: error.to_string(),
    }
}
