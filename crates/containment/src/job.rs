// SPDX-License-Identifier: Apache-2.0
//! A job per run. No process arguments or credentials are inspected. All
//! native handles are non-inheritable and closed by RAII.
use crate::unconfirmed;
use ports::HarnessError;
use std::{
    io,
    os::windows::io::{AsHandle, AsRawHandle, BorrowedHandle, FromRawHandle, OwnedHandle},
    sync::Mutex,
    time::Duration,
};
use tokio::process::{Child, Command};
#[cfg(test)]
use windows_sys::Win32::System::{
    JobObjects::{
        JOB_OBJECT_CPU_RATE_CONTROL_ENABLE, JOB_OBJECT_CPU_RATE_CONTROL_HARD_CAP,
        JOB_OBJECT_LIMIT_ACTIVE_PROCESS, JOB_OBJECT_LIMIT_JOB_MEMORY,
        JOBOBJECT_CPU_RATE_CONTROL_INFORMATION, JOBOBJECT_CPU_RATE_CONTROL_INFORMATION_0,
        JobObjectCpuRateControlInformation,
    },
    Threading::{ALL_PROCESSOR_GROUPS, GetActiveProcessorCount},
};
use windows_sys::Win32::{
    Foundation::{
        ERROR_ALREADY_EXISTS, ERROR_FILE_NOT_FOUND, ERROR_INVALID_PARAMETER, ERROR_MORE_DATA,
        ERROR_NO_MORE_FILES, FILETIME, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0, WAIT_TIMEOUT,
    },
    System::{
        Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
        },
        IO::{CreateIoCompletionPort, GetQueuedCompletionStatus},
        JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, IsProcessInJob,
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_ASSOCIATE_COMPLETION_PORT,
            JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_BASIC_PROCESS_ID_LIST,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectAssociateCompletionPortInformation,
            JobObjectBasicAccountingInformation, JobObjectBasicProcessIdList,
            JobObjectExtendedLimitInformation, OpenJobObjectW, QueryInformationJobObject,
            SetInformationJobObject, TerminateJobObject,
        },
        SystemServices::{JOB_OBJECT_MSG_NEW_PROCESS, JOB_OBJECT_QUERY, JOB_OBJECT_TERMINATE},
        Threading::{
            CREATE_NO_WINDOW, CREATE_SUSPENDED, GetProcessIdOfThread, GetProcessTimes, OpenProcess,
            OpenThread, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, ResumeThread,
            THREAD_QUERY_LIMITED_INFORMATION, THREAD_SUSPEND_RESUME, WaitForSingleObject,
        },
    },
};

/// Closing the last handle kills every process in the job.
#[derive(Debug)]
pub(crate) struct Job(OwnedHandle, bool, OwnedHandle, Mutex<Members>);

/// Notifications are installed before the first assignment, so matching their
/// count to cumulative accounting proves that no descendant was missed. Handles
/// stay pinned across retries; inactive Job accounting alone is not an exit.
#[derive(Debug, Default)]
struct Members {
    reported: u32,
    pending: Vec<u32>,
    handles: Vec<OwnedHandle>,
}

impl Job {
    pub(crate) fn new() -> Result<Self, HarnessError> {
        Self::create(None)
    }

    fn create(name: Option<&[u16]>) -> Result<Self, HarnessError> {
        // SAFETY: null security attributes create a non-inheritable handle.
        // The optional terminated name remains live throughout this call.
        let handle = unsafe {
            CreateJobObjectW(
                std::ptr::null(),
                name.map_or(std::ptr::null(), <[u16]>::as_ptr),
            )
        };
        if handle.is_null() {
            return Err(native_error("Creating native job"));
        }
        // Read last error before any other native call can change it.
        let existed = name.is_some()
            && io::Error::last_os_error().raw_os_error() == Some(ERROR_ALREADY_EXISTS as i32);
        // SAFETY: CreateJobObjectW returned a valid owned handle.
        let handle = unsafe { OwnedHandle::from_raw_handle(handle) };
        if existed {
            // Never alter limits, association or processes in an existing Job.
            return Err(unconfirmed("The native job name is already in use"));
        }
        // SAFETY: INVALID_HANDLE_VALUE creates an unnamed, non-inheritable port,
        // independent of any file or external application.
        let port =
            unsafe { CreateIoCompletionPort(INVALID_HANDLE_VALUE, std::ptr::null_mut(), 0, 1) };
        if port.is_null() {
            return Err(native_error("Creating native job completion port"));
        }
        // SAFETY: the successful call returned a unique owned port handle.
        let port = unsafe { OwnedHandle::from_raw_handle(port) };
        let job = Self(handle, false, port, Mutex::new(Members::default()));
        let completion = JOBOBJECT_ASSOCIATE_COMPLETION_PORT {
            // The constant is an opaque key, never dereferenced. Each Job has
            // its own port; no caller can inject another Job's notifications.
            CompletionKey: std::ptr::without_provenance_mut(1),
            CompletionPort: job.2.as_raw_handle(),
        };
        // SAFETY: all handles and the correctly sized buffer remain live; the
        // kernel copies this association before the empty Job can be returned.
        if unsafe {
            SetInformationJobObject(
                job.0.as_raw_handle(),
                JobObjectAssociateCompletionPortInformation,
                (&completion as *const JOBOBJECT_ASSOCIATE_COMPLETION_PORT).cast(),
                std::mem::size_of_val(&completion) as u32,
            )
        } == 0
        {
            return Err(native_error("Associating native job completion port"));
        }
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        // Neither BREAKAWAY_OK nor SILENT_BREAKAWAY_OK is enabled.
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        job.set_extended(&limits)?;
        Ok(job)
    }

    /// Both kernel policies are installed while the job is empty. If either
    /// call fails, RAII closes it and no launchable Job escapes this function.
    #[cfg(test)]
    pub(crate) fn limited(limits: crate::ResourceLimits) -> Result<Self, HarnessError> {
        // SAFETY: this reads a system-wide processor count and changes nothing.
        let processors = unsafe { GetActiveProcessorCount(ALL_PROCESSOR_GROUPS) };
        let limits = crate::job_limits::JobLimits::new(limits, processors)?;
        let mut job = Self::new()?;
        let mut extended = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        extended.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
            | JOB_OBJECT_LIMIT_ACTIVE_PROCESS
            | JOB_OBJECT_LIMIT_JOB_MEMORY;
        extended.BasicLimitInformation.ActiveProcessLimit = limits.processes;
        extended.JobMemoryLimit = limits.memory_bytes;
        job.set_extended(&extended)?;
        let cpu = JOBOBJECT_CPU_RATE_CONTROL_INFORMATION {
            ControlFlags: JOB_OBJECT_CPU_RATE_CONTROL_ENABLE | JOB_OBJECT_CPU_RATE_CONTROL_HARD_CAP,
            Anonymous: JOBOBJECT_CPU_RATE_CONTROL_INFORMATION_0 {
                CpuRate: limits.cpu_rate,
            },
        };
        // SAFETY: the job is owned, empty and non-inheritable. The correctly
        // sized input and its union member remain live throughout the call.
        if unsafe {
            SetInformationJobObject(
                job.0.as_raw_handle(),
                JobObjectCpuRateControlInformation,
                (&cpu as *const JOBOBJECT_CPU_RATE_CONTROL_INFORMATION).cast(),
                std::mem::size_of_val(&cpu) as u32,
            )
        } == 0
        {
            // CPU rate control can be unavailable under system scheduling
            // policies or outer jobs. Never silently launch without this cap.
            return Err(native_error("Setting native job CPU hard cap"));
        }
        job.1 = true;
        Ok(job)
    }

    fn set_extended(
        &self,
        limits: &JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    ) -> Result<(), HarnessError> {
        // SAFETY: the live job and correctly sized native structure remain
        // valid for this synchronous call. The OS copies the information.
        if unsafe {
            SetInformationJobObject(
                self.0.as_raw_handle(),
                JobObjectExtendedLimitInformation,
                (limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                std::mem::size_of_val(limits) as u32,
            )
        } == 0
        {
            return Err(native_error("Setting native job limits"));
        }
        Ok(())
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

    pub(crate) fn creation_handle(&self) -> BorrowedHandle<'_> {
        self.0.as_handle()
    }

    /// Assigns a process that was not started suspended; the caller says
    /// what it may have started first. Limited Jobs cannot retroactively
    /// charge that process's existing allocations and therefore refuse it.
    pub(crate) fn adopt(
        &self,
        process: std::os::windows::io::RawHandle,
    ) -> Result<(), HarnessError> {
        crate::job_limits::external_adoption(self.1)?;
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
        Self::accounting_for(self.0.as_raw_handle())
    }

    fn accounting_for(handle: HANDLE) -> Result<(u32, u32), HarnessError> {
        let mut info = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
        // SAFETY: info is writable, correctly sized for the information class,
        // and the job stays alive. No buffer is retained by the OS.
        if unsafe {
            QueryInformationJobObject(
                handle,
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

    /// Pin one reported identity without ever signaling a numeric PID. A PID
    /// that has disappeared, or now belongs outside this no-breakaway Job,
    /// proves the original object has already ended. Every other query failure
    /// leaves confirmation uncertain, rather than being mistaken for an exit.
    fn pin_member(&self, pid: u32) -> Result<Option<OwnedHandle>, HarnessError> {
        Self::pin_member_for(self.0.as_raw_handle(), pid)
    }

    fn pin_member_for(job: HANDLE, pid: u32) -> Result<Option<OwnedHandle>, HarnessError> {
        // SAFETY: opens only non-inheritable query/synchronization rights. No
        // destructive operation is performed on the process identified by PID.
        let process = unsafe {
            OpenProcess(
                PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION,
                0,
                pid,
            )
        };
        if process.is_null() {
            let error = io::Error::last_os_error();
            return if error.raw_os_error() == Some(ERROR_INVALID_PARAMETER as i32) {
                Ok(None)
            } else {
                Err(unconfirmed(format!("Opening native job member: {error}")))
            };
        }
        // SAFETY: OpenProcess returned a valid, uniquely owned process handle.
        let process = unsafe { OwnedHandle::from_raw_handle(process) };
        let mut member = 0;
        // SAFETY: both handles and the writable BOOL remain live for this call.
        if unsafe { IsProcessInJob(process.as_raw_handle(), job, &mut member) } == 0 {
            return Err(native_error("Checking native job member identity"));
        }
        Ok((member != 0).then_some(process))
    }

    fn poll_members(&self) -> Result<(u32, bool), HarnessError> {
        let mut members = self
            .3
            .lock()
            .map_err(|_| unconfirmed("Native job membership lock was poisoned"))?;
        loop {
            let mut message = 0;
            let mut key = 0;
            let mut identity = std::ptr::null_mut();
            // SAFETY: this owned port belongs only to this Job. The output
            // buffers are initialized and timeout zero never blocks Tokio.
            let received = unsafe {
                GetQueuedCompletionStatus(
                    self.2.as_raw_handle(),
                    &mut message,
                    &mut key,
                    &mut identity,
                    0,
                )
            };
            if received == 0 {
                if io::Error::last_os_error().raw_os_error() == Some(WAIT_TIMEOUT as i32) {
                    break;
                }
                return Err(native_error("Reading native job member notifications"));
            }
            if key != 1 {
                return Err(unconfirmed("Native job notification identity changed"));
            }
            if message == JOB_OBJECT_MSG_NEW_PROCESS {
                let pid = u32::try_from(identity as usize)
                    .ok()
                    .filter(|pid| *pid != 0)
                    .ok_or_else(|| {
                        unconfirmed("Native job notification has no process identity")
                    })?;
                members.reported = members
                    .reported
                    .checked_add(1)
                    .ok_or_else(|| unconfirmed("Native job member count overflowed"))?;
                members.pending.push(pid);
            }
        }
        // Keep a failed lookup pending across retries. Completion-port messages
        // are not guaranteed delivery: only an exact match with cumulative Job
        // accounting below can prove this census complete.
        while let Some(&pid) = members.pending.last() {
            if let Some(process) = self.pin_member(pid)? {
                members.handles.push(process);
            }
            members.pending.pop();
        }
        let mut index = 0;
        while index < members.handles.len() {
            // SAFETY: the stored handle pins the validated process object and
            // has synchronization rights. This polls its actual exit signal.
            match unsafe { WaitForSingleObject(members.handles[index].as_raw_handle(), 0) } {
                WAIT_OBJECT_0 => {
                    members.handles.swap_remove(index);
                }
                WAIT_TIMEOUT => {
                    index += 1;
                }
                _ => return Err(native_error("Waiting for native job member exit")),
            }
        }
        Ok((members.reported, members.handles.is_empty()))
    }

    pub(crate) async fn confirm(&self) -> Result<(), HarnessError> {
        let deadline = tokio::time::Instant::now() + crate::END_WITHIN;
        loop {
            let (reported, signaled) = self.poll_members()?;
            // Read accounting last: a descendant born between notification
            // reads and termination must increase total and prevent success.
            let (total, active) = self.accounting()?;
            if total == 0 || active > total || reported > total {
                return Err(unconfirmed(
                    "Native job membership could not be established",
                ));
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(unconfirmed(
                    "Native process-tree exit confirmation timed out",
                ));
            }
            if active == 0 && reported == total && signaled {
                return Ok(());
            }
            // This is scheduling between native polls, not an exit guarantee:
            // only each process object's signal plus a complete census can end
            // confirmation. Missing notifications and access errors fail closed.
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

/// A named check job with the same suspended launch and no-breakaway policy as
/// an agent job. Its last handle closing kills the tree, including on a crash.
/// Reopening never adds processes or weakens any existing limits.
#[derive(Debug)]
pub struct NamedJob(NamedKind);

#[derive(Debug)]
enum NamedKind {
    Fresh(Job),
    Reopened(OwnedHandle),
}

impl NamedJob {
    /// The caller supplies a fresh, unpredictable name and persists it before
    /// starting the check. An existing name is refused, never reused for spawn.
    pub fn new(name: &str) -> Result<Self, HarnessError> {
        let name = job_name(name).map_err(|error| unconfirmed(error.to_string()))?;
        Job::create(Some(&name)).map(|job| Self(NamedKind::Fresh(job)))
    }

    /// An absent job normally means the previous owner's last handle closed.
    /// Access errors are not absence. The reopened handle cannot assign a child.
    pub fn open(name: &str) -> io::Result<Option<Self>> {
        let name = job_name(name)?;
        // SAFETY: name is terminated, and the returned handle is non-inheritable.
        let handle =
            unsafe { OpenJobObjectW(JOB_OBJECT_QUERY | JOB_OBJECT_TERMINATE, 0, name.as_ptr()) };
        if handle.is_null() {
            let error = io::Error::last_os_error();
            return if error.raw_os_error() == Some(ERROR_FILE_NOT_FOUND as i32) {
                Ok(None)
            } else {
                Err(error)
            };
        }
        // SAFETY: the successful call returned one owned handle.
        let handle = unsafe { OwnedHandle::from_raw_handle(handle) };
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        // SAFETY: the query-only input handle and correctly sized output stay live.
        if unsafe {
            QueryInformationJobObject(
                handle.as_raw_handle(),
                JobObjectExtendedLimitInformation,
                (&mut limits as *mut JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                std::mem::size_of_val(&limits) as u32,
                std::ptr::null_mut(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        // The check's named jobs have exactly this policy. In particular,
        // neither kind of breakaway may have been enabled by another owner.
        if limits.BasicLimitInformation.LimitFlags != JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE {
            return Err(io::Error::other("The recovered native job policy changed"));
        }
        Ok(Some(Self(NamedKind::Reopened(handle))))
    }

    fn handle(&self) -> HANDLE {
        match &self.0 {
            NamedKind::Fresh(job) => job.0.as_raw_handle(),
            NamedKind::Reopened(handle) => handle.as_raw_handle(),
        }
    }

    /// Uses the existing creation-suspended, assign, identity-check, resume path.
    pub async fn spawn(&self, command: &mut Command) -> Result<Child, HarnessError> {
        match &self.0 {
            NamedKind::Fresh(job) => job.spawn(command).await,
            NamedKind::Reopened(_) => {
                Err(unconfirmed("A recovered native job cannot start a check"))
            }
        }
    }

    /// Verifies membership and the absolute process creation time on the same
    /// pinned native handle. A numeric process id is never used for signaling.
    pub fn contains_identity(&self, pid: u32, start_time: u64) -> Result<bool, HarnessError> {
        let Some(process) = Job::pin_member_for(self.handle(), pid)? else {
            return Ok(false);
        };
        Ok(creation_time(process.as_raw_handle())? == start_time)
    }

    pub fn has_processes(&self) -> Result<bool, HarnessError> {
        Job::accounting_for(self.handle()).map(|(_, active)| active != 0)
    }

    /// Fresh jobs retain the full notification census. A reopened job has lost
    /// that history, so pin its current members before termination and refuse a
    /// complete-exit claim if accounting shows any unobserved new process.
    pub async fn end(&self) -> Result<(), HarnessError> {
        if let NamedKind::Fresh(job) = &self.0 {
            job.terminate()?;
            return job.confirm().await;
        }
        let census = self.recovery_census();
        // Even a failed census should stop the owned tree, but cannot confirm it.
        // SAFETY: this is the validated, owned job, never a process id.
        if unsafe { TerminateJobObject(self.handle(), 1) } == 0 {
            return Err(native_error("Terminating recovered native process tree"));
        }
        let (total, handles) = census?;
        let deadline = tokio::time::Instant::now() + crate::END_WITHIN;
        loop {
            let mut signaled = true;
            for process in &handles {
                // SAFETY: each owned handle was validated against this exact Job
                // before termination and has synchronization rights.
                match unsafe { WaitForSingleObject(process.as_raw_handle(), 0) } {
                    WAIT_OBJECT_0 => {}
                    WAIT_TIMEOUT => signaled = false,
                    _ => return Err(native_error("Waiting for recovered native process exit")),
                }
            }
            let (now_total, active) = Job::accounting_for(self.handle())?;
            if now_total != total {
                return Err(unconfirmed(
                    "Recovered native job gained an unobserved process",
                ));
            }
            if active == 0 && signaled {
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(unconfirmed(
                    "Recovered native process-tree exit confirmation timed out",
                ));
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    fn recovery_census(&self) -> Result<(u32, Vec<OwnedHandle>), HarnessError> {
        let (total, active) = Job::accounting_for(self.handle())?;
        if active == 0 || active != total {
            // A recovered handle has no notification history. If any historical
            // member already left accounting, its native exit signal cannot be
            // reconstructed from a process id list. Stop, but stay uncertain.
            return Err(unconfirmed(
                "The recovered native job has no complete process census",
            ));
        }
        let mut capacity = usize::try_from(active).unwrap_or(usize::MAX).max(16);
        loop {
            if capacity > 1_048_576 {
                return Err(unconfirmed(
                    "The recovered native job has too many process identities",
                ));
            }
            // usize alignment is sufficient for the two DWORDs and pointer list.
            let mut buffer = vec![0usize; capacity + 2];
            // SAFETY: the live handle and aligned, generously sized output are
            // valid throughout the query. The OS never retains the buffer.
            if unsafe {
                QueryInformationJobObject(
                    self.handle(),
                    JobObjectBasicProcessIdList,
                    buffer.as_mut_ptr().cast(),
                    std::mem::size_of_val(buffer.as_slice()) as u32,
                    std::ptr::null_mut(),
                )
            } == 0
            {
                if io::Error::last_os_error().raw_os_error() == Some(ERROR_MORE_DATA as i32) {
                    capacity *= 2;
                    continue;
                }
                return Err(native_error("Reading recovered native job members"));
            }
            // SAFETY: the successful query initialized the fixed header. The
            // count is checked against the allocated list space before reading.
            let info = unsafe { &*buffer.as_ptr().cast::<JOBOBJECT_BASIC_PROCESS_ID_LIST>() };
            let count = info.NumberOfProcessIdsInList as usize;
            if count > capacity
                || count != total as usize
                || info.NumberOfAssignedProcesses != info.NumberOfProcessIdsInList
            {
                return Err(unconfirmed(
                    "The recovered native job census was incomplete",
                ));
            }
            // SAFETY: count entries fit in this successful query's output buffer.
            let pids = unsafe { std::slice::from_raw_parts(info.ProcessIdList.as_ptr(), count) };
            let mut handles = Vec::with_capacity(count);
            for &pid in pids {
                let pid = u32::try_from(pid).map_err(|_| {
                    unconfirmed("The recovered native job has an invalid process id")
                })?;
                if let Some(process) = Job::pin_member_for(self.handle(), pid)? {
                    handles.push(process);
                }
            }
            if handles.len() != count || Job::accounting_for(self.handle())?.0 != total {
                return Err(unconfirmed(
                    "The recovered native job changed during its census",
                ));
            }
            return Ok((total, handles));
        }
    }
}

fn job_name(name: &str) -> io::Result<Vec<u16>> {
    if name.is_empty() || name.contains(['\0', '\\', '/']) || name.len() > 240 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "The native job name is invalid",
        ));
    }
    Ok(name.encode_utf16().chain(Some(0)).collect())
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
#[path = "job_limits_native.rs"]
mod limits_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Stdio;
    use windows_sys::Win32::{
        Foundation::{WAIT_OBJECT_0, WAIT_TIMEOUT},
        System::{
            IO::PostQueuedCompletionStatus,
            JobObjects::{
                IsProcessInJob, JOBOBJECT_BASIC_PROCESS_ID_LIST, JobObjectBasicProcessIdList,
            },
            Threading::{
                GetCurrentProcess, GetExitCodeProcess, GetProcessId, PROCESS_SYNCHRONIZE,
                WaitForSingleObject,
            },
        },
    };

    /// An inner Job with no completion port, as used by native process
    /// libraries. Only this authored fixture's own process handles are adopted.
    fn fixture_nested_job(process: HANDLE) -> OwnedHandle {
        // SAFETY: creates a private, non-inheritable, unnamed fixture Job.
        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        assert!(!handle.is_null());
        // SAFETY: successful creation transfers unique ownership here.
        let job = unsafe { OwnedHandle::from_raw_handle(handle) };
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: the input buffer and both owned/borrowed fixture handles
        // remain live. No external process is ever assigned to this inner Job.
        unsafe {
            assert_ne!(
                SetInformationJobObject(
                    job.as_raw_handle(),
                    JobObjectExtendedLimitInformation,
                    (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                    std::mem::size_of_val(&limits) as u32,
                ),
                0
            );
            assert_ne!(AssignProcessToJobObject(job.as_raw_handle(), process), 0);
            let mut member = 0;
            assert_ne!(IsProcessInJob(process, job.as_raw_handle(), &mut member), 0);
            assert_ne!(member, 0);
        }
        job
    }

    /// An inert subprocess of this test executable, never a real agent session.
    #[test]
    fn job_fixture() {
        let Ok(role) = std::env::var("OPHIO_JOB_TEST_ROLE") else {
            return;
        };
        std::thread::spawn(|| {
            std::thread::sleep(Duration::from_secs(60));
            std::process::exit(0);
        });
        if role == "spawning-parent" {
            use std::os::windows::process::CommandExt;
            let mut children = Vec::new();
            for _ in 0..16 {
                let child = std::process::Command::new(std::env::current_exe().unwrap())
                    .args(["--exact", "job::tests::job_fixture", "--nocapture"])
                    .env("OPHIO_JOB_TEST_ROLE", "child")
                    .creation_flags(CREATE_NO_WINDOW)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .unwrap();
                children.push(child);
                std::fs::write("spawning", b"").unwrap();
            }
            // Keep every child handle alive until the enclosing Job stops us.
            loop {
                std::thread::park();
            }
        }
        if role == "child" {
            if std::env::var_os("OPHIO_JOB_TEST_PENDING_IO").is_some() {
                use std::io::Write;
                std::fs::write("descendant-ready", b"").unwrap();
                let bytes = [b'x'; 8192];
                let mut stderr = std::io::stderr().lock();
                while stderr.write_all(&bytes).is_ok() {}
            }
            loop {
                std::thread::park();
            }
        }
        use std::os::windows::process::CommandExt;
        let mut nested_jobs = Vec::new();
        if matches!(role.as_str(), "nested-self" | "nested-both") {
            // SAFETY: GetCurrentProcess returns this fixture's borrowed pseudo
            // handle. The inner Job is retained until the fixture exits.
            nested_jobs.push(fixture_nested_job(unsafe { GetCurrentProcess() }));
        }
        let pending_io = std::env::var_os("OPHIO_JOB_TEST_PENDING_IO").is_some();
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "job::tests::job_fixture", "--nocapture"])
            .env("OPHIO_JOB_TEST_ROLE", "child")
            .creation_flags(CREATE_NO_WINDOW)
            .stdin(Stdio::null())
            .stdout(if pending_io {
                Stdio::inherit()
            } else {
                Stdio::null()
            })
            .stderr(if pending_io {
                Stdio::inherit()
            } else {
                Stdio::null()
            })
            .spawn()
            .unwrap();
        if matches!(role.as_str(), "nested-child" | "nested-both") {
            nested_jobs.push(fixture_nested_job(child.as_raw_handle()));
        }
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

    #[derive(Debug)]
    struct ProcessSnapshot {
        pid: u32,
        born: u64,
        in_job: bool,
        accounting: (u32, u32),
        process_ids: Vec<usize>,
        wait: u32,
        exit_code: u32,
    }

    fn process_snapshot(process: &OwnedHandle, job: &Job) -> ProcessSnapshot {
        let mut in_job = 0;
        let mut exit_code = 0;
        // SAFETY: both handles stay owned throughout these read-only calls.
        // The process was pinned before termination, so PID reuse cannot change
        // the identity these observations describe.
        unsafe {
            assert_ne!(
                IsProcessInJob(process.as_raw_handle(), job.0.as_raw_handle(), &mut in_job),
                0,
                "could not query fixture membership: {}",
                io::Error::last_os_error()
            );
            assert_ne!(
                GetExitCodeProcess(process.as_raw_handle(), &mut exit_code),
                0,
                "could not query fixture exit: {}",
                io::Error::last_os_error()
            );
            // This fixture has only a root, one descendant and their console
            // hosts. An oversized fixed buffer makes truncation a test error.
            let mut buffer = [0usize; 66];
            assert_ne!(
                QueryInformationJobObject(
                    job.0.as_raw_handle(),
                    JobObjectBasicProcessIdList,
                    buffer.as_mut_ptr().cast(),
                    std::mem::size_of_val(&buffer) as u32,
                    std::ptr::null_mut(),
                ),
                0,
                "could not enumerate fixture job: {}",
                io::Error::last_os_error()
            );
            let ids = &*buffer.as_ptr().cast::<JOBOBJECT_BASIC_PROCESS_ID_LIST>();
            assert_eq!(ids.NumberOfAssignedProcesses, ids.NumberOfProcessIdsInList);
            let process_ids = std::slice::from_raw_parts(
                ids.ProcessIdList.as_ptr(),
                ids.NumberOfProcessIdsInList as usize,
            )
            .to_vec();
            ProcessSnapshot {
                pid: GetProcessId(process.as_raw_handle()),
                born: creation_time(process.as_raw_handle()).unwrap(),
                in_job: in_job != 0,
                accounting: job.accounting().unwrap(),
                process_ids,
                wait: WaitForSingleObject(process.as_raw_handle(), 0),
                exit_code,
            }
        }
    }

    /// Keep native identities pinned before ending the job. Accounting and
    /// process-object signaling are deliberately observed separately, without
    /// adding a wait to the completion assertion we are investigating.
    #[tokio::test]
    async fn confirmed_end_has_signaled_owned_process_handles() {
        for root_exits_first in [false, true] {
            for trial in 0..16 {
                let dir = tempfile::tempdir().unwrap();
                let container = crate::Container::new().unwrap();
                let crate::Inner::Job(job) = &container.inner;
                let mut launch = command(dir.path());
                if trial % 2 == 1 {
                    launch
                        .env("OPHIO_JOB_TEST_PENDING_IO", "1")
                        .stdout(Stdio::piped())
                        .stderr(Stdio::piped());
                }
                let mut child = container.spawn(&mut launch).await.unwrap();
                let descendant = descendant(dir.path(), job).await;
                if trial % 2 == 1 {
                    tokio::time::timeout(Duration::from_secs(10), async {
                        while !dir.path().join("descendant-ready").exists() {
                            tokio::time::sleep(Duration::from_millis(10)).await;
                        }
                    })
                    .await
                    .unwrap();
                    std::fs::remove_file(dir.path().join("descendant-ready")).unwrap();
                }
                // SAFETY: the root is our owned, still-running child. This
                // duplicate remains owned even after Child::wait reaps it.
                let root = unsafe {
                    BorrowedHandle::borrow_raw(child.raw_handle().unwrap())
                        .try_clone_to_owned()
                        .unwrap()
                };
                let root_before = process_snapshot(&root, job);
                let descendant_before = process_snapshot(&descendant, job);
                eprintln!(
                    "native exit trial={trial} root_exits_first={root_exits_first} before root={root_before:?} descendant={descendant_before:?}"
                );
                assert!(root_before.in_job && descendant_before.in_job);
                assert!(
                    root_before
                        .process_ids
                        .contains(&(root_before.pid as usize))
                );
                assert!(
                    descendant_before
                        .process_ids
                        .contains(&(descendant_before.pid as usize))
                );
                assert_eq!(root_before.wait, WAIT_TIMEOUT);
                assert_eq!(descendant_before.wait, WAIT_TIMEOUT);
                if root_exits_first {
                    child.stdin.take();
                    tokio::time::timeout(Duration::from_secs(10), child.wait())
                        .await
                        .unwrap()
                        .unwrap();
                }
                std::fs::remove_file(dir.path().join("descendant.pid")).unwrap();
                let ended = container.end().await;
                // Capture both snapshots immediately, before printing, querying
                // the filesystem or waiting for either process again.
                let root_after = process_snapshot(&root, job);
                let descendant_after = process_snapshot(&descendant, job);
                let removed = std::fs::remove_dir(dir.path());
                eprintln!(
                    "native exit trial={trial} root_exits_first={root_exits_first} end={ended:?} after root={root_after:?} descendant={descendant_after:?} remove_working_directory={removed:?}"
                );
                ended.unwrap();
                assert_eq!(root_before.pid, root_after.pid);
                assert_eq!(root_before.born, root_after.born);
                assert_eq!(descendant_before.pid, descendant_after.pid);
                assert_eq!(descendant_before.born, descendant_after.born);
                assert!(root_after.in_job && descendant_after.in_job);
                assert_eq!(root_after.accounting.1, 0);
                assert_eq!(descendant_after.accounting.1, 0);
                assert_eq!(
                    root_after.wait, WAIT_OBJECT_0,
                    "root not signaled after end"
                );
                assert_eq!(
                    descendant_after.wait, WAIT_OBJECT_0,
                    "descendant not signaled after end; exit code was {}",
                    descendant_after.exit_code
                );
                removed.unwrap();
                child.wait().await.unwrap();
            }
        }
    }

    fn lose_process_notification(job: &Job, pid: u32) {
        let mut preserved = Vec::new();
        loop {
            let mut message = 0;
            let mut key = 0;
            let mut identity = std::ptr::null_mut();
            // SAFETY: this bounded read touches only our private fixture port.
            assert_ne!(
                unsafe {
                    GetQueuedCompletionStatus(
                        job.2.as_raw_handle(),
                        &mut message,
                        &mut key,
                        &mut identity,
                        10_000,
                    )
                },
                0
            );
            assert_eq!(key, 1);
            if message == JOB_OBJECT_MSG_NEW_PROCESS && identity as usize == pid as usize {
                break;
            }
            preserved.push((message, key, identity));
        }
        for (message, key, identity) in preserved {
            // SAFETY: replay each unmodified, non-target packet exactly once on
            // the same owned port. Only one matching identity is deliberately
            // lost; duplicates are preserved to expose any masked census gap.
            assert_ne!(
                unsafe {
                    PostQueuedCompletionStatus(job.2.as_raw_handle(), message, key, identity)
                },
                0
            );
        }
    }

    #[tokio::test]
    async fn nested_job_adoption_preserves_exit_and_census_proof() {
        for role in ["nested-self", "nested-child", "nested-both"] {
            for lose_notification in [false, true] {
                let dir = tempfile::tempdir().unwrap();
                let container = crate::Container::new().unwrap();
                let crate::Inner::Job(job) = &container.inner;
                let mut launch = command(dir.path());
                launch.env("OPHIO_JOB_TEST_ROLE", role);
                let mut child = container.spawn(&mut launch).await.unwrap();
                let descendant = descendant(dir.path(), job).await;
                // SAFETY: this duplicates our owned root's live process handle.
                let root = unsafe {
                    BorrowedHandle::borrow_raw(child.raw_handle().unwrap())
                        .try_clone_to_owned()
                        .unwrap()
                };
                let root_before = process_snapshot(&root, job);
                let descendant_before = process_snapshot(&descendant, job);
                assert!(root_before.in_job && descendant_before.in_job);
                assert_eq!(root_before.wait, WAIT_TIMEOUT);
                assert_eq!(descendant_before.wait, WAIT_TIMEOUT);
                if lose_notification {
                    lose_process_notification(job, descendant_before.pid);
                }
                std::fs::remove_file(dir.path().join("descendant.pid")).unwrap();
                let ended = container.end().await;
                let root_after = process_snapshot(&root, job);
                let descendant_after = process_snapshot(&descendant, job);
                let removed = std::fs::remove_dir(dir.path());
                let reported = job.3.lock().unwrap().reported;
                eprintln!(
                    "nested role={role} lost_notification={lose_notification} before root={root_before:?} descendant={descendant_before:?}; end={ended:?} after root={root_after:?} descendant={descendant_after:?} reported={reported} remove_working_directory={removed:?}"
                );
                if lose_notification {
                    assert!(
                        matches!(ended, Err(HarnessError::Protocol(ref reason))
                        if reason.contains("not confirmed")),
                        "missing identity was hidden: {ended:?}"
                    );
                } else {
                    ended.unwrap();
                    assert_eq!(reported, root_after.accounting.0);
                }
                assert_eq!(root_before.pid, root_after.pid);
                assert_eq!(root_before.born, root_after.born);
                assert_eq!(descendant_before.pid, descendant_after.pid);
                assert_eq!(descendant_before.born, descendant_after.born);
                assert!(root_after.in_job && descendant_after.in_job);
                assert_eq!(root_after.accounting.1, 0);
                assert_eq!(descendant_after.accounting.1, 0);
                assert_eq!(root_after.wait, WAIT_OBJECT_0);
                assert_eq!(descendant_after.wait, WAIT_OBJECT_0);
                removed.unwrap();
                child.wait().await.unwrap();
            }
        }
    }

    #[tokio::test]
    async fn a_missing_process_notification_never_confirms_an_empty_job() {
        let dir = tempfile::tempdir().unwrap();
        let job = Job::new().unwrap();
        let mut launch = command(dir.path());
        launch.env("OPHIO_JOB_TEST_ROLE", "child");
        let mut child = job.spawn(&mut launch).await.unwrap();
        // Deliberately lose one NEW_PROCESS packet before confirmation can
        // consume it. Job notifications are documented as best effort, so this
        // must remain uncertain even after every process actually exits.
        loop {
            let mut message = 0;
            let mut key = 0;
            let mut identity = std::ptr::null_mut();
            // SAFETY: only our private fixture port is read, with valid outputs
            // and a bounded wait. No PID is signaled or otherwise modified.
            assert_ne!(
                unsafe {
                    GetQueuedCompletionStatus(
                        job.2.as_raw_handle(),
                        &mut message,
                        &mut key,
                        &mut identity,
                        10_000,
                    )
                },
                0
            );
            assert_eq!(key, 1);
            if message == JOB_OBJECT_MSG_NEW_PROCESS {
                break;
            }
        }
        job.terminate().unwrap();
        child.wait().await.unwrap();
        assert_eq!(job.accounting().unwrap().1, 0);
        let error = job.confirm().await.unwrap_err();
        assert!(matches!(error, HarnessError::Protocol(ref reason)
            if reason.contains("timed out") && reason.contains("not confirmed")));
    }

    #[tokio::test]
    async fn confirmation_covers_descendants_created_while_the_job_stops() {
        for _ in 0..4 {
            let dir = tempfile::tempdir().unwrap();
            let job = Job::new().unwrap();
            let mut launch = command(dir.path());
            launch.env("OPHIO_JOB_TEST_ROLE", "spawning-parent");
            let mut child = job.spawn(&mut launch).await.unwrap();
            tokio::time::timeout(Duration::from_secs(10), async {
                while !dir.path().join("spawning").exists() {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            })
            .await
            .unwrap();
            job.terminate().unwrap();
            job.confirm().await.unwrap();
            let (total, active) = job.accounting().unwrap();
            {
                let members = job.3.lock().unwrap();
                assert_eq!(members.reported, total);
                assert!(members.pending.is_empty());
                assert!(members.handles.is_empty());
                assert_eq!(active, 0);
            }
            // Root signaling and filesystem handles must be complete too, not
            // just the cumulative accounting/notification counts.
            assert_eq!(
                unsafe { WaitForSingleObject(child.raw_handle().unwrap(), 0) },
                WAIT_OBJECT_0
            );
            child.wait().await.unwrap();
            std::fs::remove_file(dir.path().join("spawning")).unwrap();
            std::fs::remove_dir(dir.path()).unwrap();
        }
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
