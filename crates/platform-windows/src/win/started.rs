// SPDX-License-Identifier: Apache-2.0
use super::*;
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, IsProcessInJob,
};

/// A job this process joins so that every process it starts afterwards, and
/// theirs, belongs to it. It sets no limits, and so allows no breakaway.
pub struct Job(Handle);
impl std::fmt::Debug for Job {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Job").finish_non_exhaustive()
    }
}
// SAFETY: a job handle may be used from any thread.
unsafe impl Send for Job {}
// SAFETY: IsProcessInJob only reads the job; the handle closes once, on drop.
unsafe impl Sync for Job {}
impl Job {
    pub fn claim() -> io::Result<Self> {
        // SAFETY: no attributes or name create an unnamed, non-inheritable job,
        // owned below. The current-process pseudo handle needs no closing.
        unsafe {
            let job = Handle(CreateJobObjectW(None, PCWSTR::null()).map_err(failure)?);
            AssignProcessToJobObject(job.0, GetCurrentProcess()).map_err(failure)?;
            Ok(Self(job))
        }
    }

    pub fn includes(&self, pid: u32) -> io::Result<bool> {
        let mut result = windows::core::BOOL::default();
        // SAFETY: the process handle is owned for this call and the job stays
        // alive with self; result is a valid output location.
        unsafe {
            let process = Handle(
                OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).map_err(failure)?,
            );
            IsProcessInJob(process.0, Some(self.0.0), &mut result).map_err(failure)?;
        }
        Ok(result.as_bool())
    }
}
