// SPDX-License-Identifier: Apache-2.0
//! A job per run. No process arguments or credentials are inspected. All
//! native handles are non-inheritable and closed by RAII.
use crate::unconfirmed;
use ports::HarnessError;
use std::{
    io,
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    time::Duration,
};
use tokio::process::{Child, Command};
use windows_sys::Win32::{
    Foundation::{ERROR_NO_MORE_FILES, FILETIME, HANDLE, INVALID_HANDLE_VALUE},
    System::{
        Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
        },
        JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
            JobObjectBasicAccountingInformation, JobObjectExtendedLimitInformation,
            QueryInformationJobObject, SetInformationJobObject, TerminateJobObject,
        },
        Threading::{
            CREATE_NO_WINDOW, CREATE_SUSPENDED, GetProcessIdOfThread, GetProcessTimes, OpenProcess,
            OpenThread, PROCESS_QUERY_LIMITED_INFORMATION, ResumeThread,
            THREAD_QUERY_LIMITED_INFORMATION, THREAD_SUSPEND_RESUME,
        },
    },
};

/// Closing the last handle kills every process in the job.
#[derive(Debug)]
pub(crate) struct Job(OwnedHandle);

impl Job {
    pub(crate) fn new() -> Result<Self, HarnessError> {
        // SAFETY: null security attributes create a non-inheritable, unnamed
        // handle. Successful ownership is transferred exactly once to RAII.
        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if handle.is_null() {
            return Err(native_error("Creating native job"));
        }
        // SAFETY: CreateJobObjectW returned a valid owned handle.
        let job = Self(unsafe { OwnedHandle::from_raw_handle(handle) });
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        // Neither BREAKAWAY_OK nor SILENT_BREAKAWAY_OK is enabled.
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: the live job and correctly sized native structure remain
        // valid for this synchronous call. The OS copies the information.
        if unsafe {
            SetInformationJobObject(
                job.0.as_raw_handle(),
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                std::mem::size_of_val(&limits) as u32,
            )
        } == 0
        {
            return Err(native_error("Setting native job limits"));
        }
        Ok(job)
    }

    /// Keep Tokio's pipe and command-line handling, but do not execute any
    /// native instruction until containment succeeds.
    pub(crate) async fn spawn(&self, command: &mut Command) -> Result<Child, HarnessError> {
        command.creation_flags(CREATE_NO_WINDOW | CREATE_SUSPENDED);
        let mut child = command
            .spawn()
            .map_err(|e| HarnessError::SpawnFailed(e.to_string()))?;
        if let Err(error) = self.assign(&child).and_then(|()| resume(&child)) {
            // Assignment may have failed: kill the owned child as well as the job.
            // It was never resumed on an assignment failure, so it has no children.
            let _ = self.terminate();
            let _ = child.start_kill();
            let _ = tokio::time::timeout(Duration::from_secs(3), child.wait()).await;
            return Err(error);
        }
        Ok(child)
    }

    fn assign(&self, child: &Child) -> Result<(), HarnessError> {
        let process = child
            .raw_handle()
            .ok_or_else(|| unconfirmed("Native process handle is unavailable"))?;
        // SAFETY: both handles are borrowed from live owners; the child is still
        // suspended, so it cannot create descendants before this assignment.
        if unsafe { AssignProcessToJobObject(self.0.as_raw_handle(), process as HANDLE) } == 0 {
            return Err(native_error("Assigning native process to its job"));
        }
        Ok(())
    }

    /// Assigns a process that was not started suspended; the caller says
    /// what it may have started first.
    pub(crate) fn adopt(
        &self,
        process: std::os::windows::io::RawHandle,
    ) -> Result<(), HarnessError> {
        // SAFETY: the job handle is owned, and the caller's process handle
        // is borrowed from its live owner for this call.
        if unsafe { AssignProcessToJobObject(self.0.as_raw_handle(), process as HANDLE) } == 0 {
            return Err(native_error("Assigning native process to its job"));
        }
        Ok(())
    }

    pub(crate) fn terminate(&self) -> Result<(), HarnessError> {
        // SAFETY: the job handle is owned for the duration of this call.
        if unsafe { TerminateJobObject(self.0.as_raw_handle(), 1) } == 0 {
            return Err(native_error("Terminating native process tree"));
        }
        Ok(())
    }

    fn accounting(&self) -> Result<(u32, u32), HarnessError> {
        let mut info = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
        // SAFETY: info is writable, correctly sized for the information class,
        // and the job stays alive. No buffer is retained by the OS.
        if unsafe {
            QueryInformationJobObject(
                self.0.as_raw_handle(),
                JobObjectBasicAccountingInformation,
                (&mut info as *mut JOBOBJECT_BASIC_ACCOUNTING_INFORMATION).cast(),
                std::mem::size_of_val(&info) as u32,
                std::ptr::null_mut(),
            )
        } == 0
        {
            return Err(native_error("Reading native job accounting"));
        }
        Ok((info.TotalProcesses, info.ActiveProcesses))
    }

    pub(crate) async fn confirm(&self) -> Result<(), HarnessError> {
        crate::accounting::confirm(&ports::SystemClock, || self.accounting()).await
    }
}

fn native_error(operation: &str) -> HarnessError {
    unconfirmed(format!("{operation}: {}", io::Error::last_os_error()))
}

fn creation_time(process: HANDLE) -> Result<u64, HarnessError> {
    let mut born = FILETIME::default();
    let mut exited = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    // SAFETY: the caller holds the process handle; all outputs are initialized
    // native FILETIME buffers and remain valid until this call returns.
    if unsafe { GetProcessTimes(process, &mut born, &mut exited, &mut kernel, &mut user) } == 0 {
        return Err(native_error("Reading native process identity"));
    }
    Ok((u64::from(born.dwHighDateTime) << 32) | u64::from(born.dwLowDateTime))
}

fn resume(child: &Child) -> Result<(), HarnessError> {
    let pid = child
        .id()
        .ok_or_else(|| unconfirmed("Native process exited before containment"))?;
    let process = child
        .raw_handle()
        .ok_or_else(|| unconfirmed("Native process handle is unavailable"))?;
    let born = creation_time(process as HANDLE)?;
    // Tokio does not expose the primary thread handle. A newly created,
    // suspended process has one thread; refuse any ambiguous census.
    // SAFETY: this only snapshots thread metadata; no pointers are passed.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(native_error("Snapshotting suspended native threads"));
    }
    // SAFETY: the snapshot is a valid, uniquely owned handle.
    let snapshot = unsafe { OwnedHandle::from_raw_handle(snapshot) };
    let mut entry = THREADENTRY32 {
        dwSize: std::mem::size_of::<THREADENTRY32>() as u32,
        ..Default::default()
    };
    let mut thread_id = None;
    // SAFETY: entry has the required size and outlives each snapshot read.
    let mut found = unsafe { Thread32First(snapshot.as_raw_handle(), &mut entry) };
    while found != 0 {
        if entry.th32OwnerProcessID == pid && thread_id.replace(entry.th32ThreadID).is_some() {
            return Err(unconfirmed("Suspended native process has multiple threads"));
        }
        entry.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;
        // SAFETY: snapshot is live and entry is the correctly sized output.
        found = unsafe { Thread32Next(snapshot.as_raw_handle(), &mut entry) };
    }
    if io::Error::last_os_error().raw_os_error() != Some(ERROR_NO_MORE_FILES as i32) {
        return Err(native_error("Reading suspended native threads"));
    }
    let thread_id = thread_id.ok_or_else(|| unconfirmed("Suspended native thread is missing"))?;
    // SAFETY: opens a non-inheritable handle by the enumerated id. Identity is
    // independently revalidated below before resuming it.
    let thread = unsafe {
        OpenThread(
            THREAD_SUSPEND_RESUME | THREAD_QUERY_LIMITED_INFORMATION,
            0,
            thread_id,
        )
    };
    if thread.is_null() {
        return Err(native_error("Opening suspended native thread"));
    }
    // SAFETY: OpenThread returned a valid owned handle.
    let thread = unsafe { OwnedHandle::from_raw_handle(thread) };
    // SAFETY: thread is a live handle with query rights.
    let owner_id = unsafe { GetProcessIdOfThread(thread.as_raw_handle()) };
    // SAFETY: only queries the owner identity; no mutation, no inheritance.
    let owner = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, owner_id) };
    if owner.is_null() {
        return Err(native_error("Opening native thread owner"));
    }
    // SAFETY: OpenProcess returned a valid owned handle.
    let owner = unsafe { OwnedHandle::from_raw_handle(owner) };
    // Birth time keeps a reused id from standing in for this process.
    if owner_id != pid || creation_time(owner.as_raw_handle())? != born {
        return Err(unconfirmed("Native process identity changed before resume"));
    }
    // SAFETY: the owned thread's process identity matches the child assigned to
    // our job. Only the single suspension added during creation is removed.
    if unsafe { ResumeThread(thread.as_raw_handle()) } != 1 {
        return Err(unconfirmed(
            "Native thread did not have exactly one launch suspension",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Stdio;
    use windows_sys::Win32::{
        Foundation::{WAIT_OBJECT_0, WAIT_TIMEOUT},
        System::{
            JobObjects::IsProcessInJob,
            Threading::{PROCESS_SYNCHRONIZE, WaitForSingleObject},
        },
    };

    /// An inert subprocess of this test executable, never a real agent session.
    #[test]
    fn job_fixture() {
        let Ok(role) = std::env::var("OPHIO_JOB_TEST_ROLE") else {
            return;
        };
        if role == "child" {
            loop {
                std::thread::park();
            }
        }
        use std::os::windows::process::CommandExt;
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "job::tests::job_fixture", "--nocapture"])
            .env("OPHIO_JOB_TEST_ROLE", "child")
            .creation_flags(CREATE_NO_WINDOW)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        std::fs::write("descendant.pid.part", child.id().to_string()).unwrap();
        std::fs::rename("descendant.pid.part", "descendant.pid").unwrap();
        let mut line = String::new();
        while std::io::stdin().read_line(&mut line).unwrap() != 0 {
            line.clear();
        }
        // Deliberately leave the descendant alive on root exit. The enclosing
        // job owns cleanup; keep its handle until this fixture exits.
        let _ = child.try_wait().unwrap();
    }

    fn command(path: &std::path::Path) -> Command {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .current_dir(path)
            .args(["--exact", "job::tests::job_fixture", "--nocapture"])
            .env("OPHIO_JOB_TEST_ROLE", "parent")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        command
    }

    /// Each console process in the job brings its own console host, so the
    /// descendant is identified by its reported id and its job membership,
    /// never by a count of the job's processes.
    async fn descendant(path: &std::path::Path, job: &Job) -> OwnedHandle {
        let pid = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Ok(text) = std::fs::read_to_string(path.join("descendant.pid"))
                    && let Ok(pid) = text.parse::<u32>()
                {
                    return pid;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        // SAFETY: opens only synchronization and query rights to the inert
        // fixture descendant, which lives until its job ends.
        let handle = unsafe {
            OpenProcess(
                PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION,
                0,
                pid,
            )
        };
        assert!(!handle.is_null());
        // SAFETY: OpenProcess returned a unique owned handle.
        let handle = unsafe { OwnedHandle::from_raw_handle(handle) };
        let mut contained = 0;
        // SAFETY: both handles are live and the output is a writable BOOL. A
        // reused id would name a process outside this job.
        assert_ne!(
            unsafe {
                IsProcessInJob(
                    handle.as_raw_handle(),
                    job.0.as_raw_handle(),
                    &mut contained,
                )
            },
            0
        );
        assert_ne!(contained, 0);
        handle
    }

    fn assert_exited(process: &OwnedHandle) {
        // SAFETY: the handle has synchronization rights and is held through the
        // bounded wait. This touches no desktop or session services.
        assert_eq!(
            unsafe { WaitForSingleObject(process.as_raw_handle(), 3000) },
            WAIT_OBJECT_0
        );
    }

    #[tokio::test]
    async fn root_exit_is_not_tree_exit_and_termination_covers_descendants() {
        let dir = tempfile::tempdir().unwrap();
        let job = Job::new().unwrap();
        let mut child = job.spawn(&mut command(dir.path())).await.unwrap();
        let descendant = descendant(dir.path(), &job).await;
        child.stdin.take();
        tokio::time::timeout(Duration::from_secs(10), child.wait())
            .await
            .unwrap()
            .unwrap();
        // SAFETY: the handle has synchronization rights; this only polls it.
        assert_eq!(
            unsafe { WaitForSingleObject(descendant.as_raw_handle(), 0) },
            WAIT_TIMEOUT
        );
        assert_ne!(job.accounting().unwrap().1, 0);
        job.terminate().unwrap();
        job.confirm().await.unwrap();
        assert_eq!(job.accounting().unwrap().1, 0);
        assert_exited(&descendant);
        job.terminate().unwrap();
        job.confirm().await.unwrap();
    }

    #[tokio::test]
    async fn closing_last_job_handle_kills_root_and_descendant() {
        let dir = tempfile::tempdir().unwrap();
        let job = Job::new().unwrap();
        let mut child = job.spawn(&mut command(dir.path())).await.unwrap();
        let descendant = descendant(dir.path(), &job).await;
        drop(job);
        assert_exited(&descendant);
        tokio::time::timeout(Duration::from_secs(10), child.wait())
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn cancelled_run_kills_tree_even_with_retained_confirmation_handle() {
        let dir = tempfile::tempdir().unwrap();
        let container = std::sync::Arc::new(crate::Container::new().unwrap());
        let mut child = container.spawn(&mut command(dir.path())).await.unwrap();
        let crate::Inner::Job(job) = &container.inner;
        let descendant = descendant(dir.path(), job).await;
        let guard = crate::Guard::new(container.clone());
        let task = tokio::spawn(async move {
            let _guard = guard;
            std::future::pending::<()>().await;
        });
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        container.end().await.unwrap();
        assert_exited(&descendant);
        child.wait().await.unwrap();
    }

    #[tokio::test]
    async fn job_limits_disallow_both_forms_of_breakaway() {
        let job = Job::new().unwrap();
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        // SAFETY: live job handle and correctly sized, writable output buffer.
        assert_ne!(
            unsafe {
                QueryInformationJobObject(
                    job.0.as_raw_handle(),
                    JobObjectExtendedLimitInformation,
                    (&mut limits as *mut JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                    std::mem::size_of_val(&limits) as u32,
                    std::ptr::null_mut(),
                )
            },
            0
        );
        assert_eq!(
            limits.BasicLimitInformation.LimitFlags,
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
        );
        assert!(job.confirm().await.is_err()); // Never assigned a process.
    }

    #[tokio::test]
    async fn failed_assignment_never_executes_the_child_and_reports_uncertainty() {
        use windows_sys::Win32::System::JobObjects::JOB_OBJECT_LIMIT_ACTIVE_PROCESS;
        let dir = tempfile::tempdir().unwrap();
        let job = Job::new().unwrap();
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags =
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_ACTIVE_PROCESS;
        limits.BasicLimitInformation.ActiveProcessLimit = 1;
        // SAFETY: the owned job and correctly sized limits are live throughout
        // this synchronous call. This deliberately limits only our test job.
        assert_ne!(
            unsafe {
                SetInformationJobObject(
                    job.0.as_raw_handle(),
                    JobObjectExtendedLimitInformation,
                    (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                    std::mem::size_of_val(&limits) as u32,
                )
            },
            0
        );
        let mut first = command(dir.path());
        first.env("OPHIO_JOB_TEST_ROLE", "child");
        let mut child = job.spawn(&mut first).await.unwrap();
        let rejected = job.spawn(&mut command(dir.path())).await;
        assert!(
            matches!(rejected, Err(HarnessError::Protocol(ref reason)) if reason.contains("Assigning") && reason.contains("not confirmed"))
        );
        assert!(!dir.path().join("descendant.pid").exists());
        // Failure cleanup also terminates the containing job.
        tokio::time::timeout(Duration::from_secs(10), child.wait())
            .await
            .unwrap()
            .unwrap();
        job.confirm().await.unwrap();
    }
}
