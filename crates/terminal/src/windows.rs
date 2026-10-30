// SPDX-License-Identifier: Apache-2.0
//! Minimal owned Windows job handle. Unsafe is limited to typed native calls.
use portable_pty::Child;
use std::io;
use windows_sys::Win32::{
    Foundation::{CloseHandle, HANDLE},
    System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JobObjectBasicAccountingInformation, JobObjectExtendedLimitInformation,
        QueryInformationJobObject, SetInformationJobObject, TerminateJobObject,
    },
};

#[derive(Debug)]
pub(crate) struct ProcessTree {
    handle: usize,
}
impl ProcessTree {
    #[allow(unsafe_code)]
    pub(crate) fn has_running_children(&self) -> io::Result<bool> {
        let mut accounting = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
        // SAFETY: the owned job handle stays alive and the output buffer has
        // exactly the size and layout required by this information class.
        if unsafe {
            QueryInformationJobObject(
                self.handle as HANDLE,
                JobObjectBasicAccountingInformation,
                (&mut accounting as *mut JOBOBJECT_BASIC_ACCOUNTING_INFORMATION).cast(),
                std::mem::size_of_val(&accounting) as u32,
                std::ptr::null_mut(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(accounting.ActiveProcesses > 1)
    }
    #[allow(unsafe_code)]
    pub(crate) fn new(child: &dyn Child) -> io::Result<Self> {
        // SAFETY: null means default security and an unnamed job. No borrowed
        // pointers escape the call; the resulting handle is owned below.
        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        let job = Self {
            handle: handle as usize,
        };
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: buffer has exactly the layout and size of the specified class;
        // the job handle remains alive during the call.
        if unsafe {
            SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                std::mem::size_of_val(&limits) as u32,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        let process = child
            .as_raw_handle()
            .ok_or_else(|| io::Error::other("missing terminal process handle"))?;
        // SAFETY: portable-pty owns the process handle and it stays alive during
        // assignment. Assignment failure is surfaced and the child is killed.
        if unsafe { AssignProcessToJobObject(handle, process) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(job)
    }
    #[allow(unsafe_code)]
    pub(crate) fn kill(&self) -> io::Result<()> {
        // SAFETY: this object owns the live job handle for the duration of call.
        if unsafe { TerminateJobObject(self.handle as HANDLE, 1) } == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}
impl Drop for ProcessTree {
    #[allow(unsafe_code)]
    fn drop(&mut self) {
        // SAFETY: the handle is owned uniquely and closed exactly once here.
        unsafe {
            CloseHandle(self.handle as HANDLE);
        }
    }
}
