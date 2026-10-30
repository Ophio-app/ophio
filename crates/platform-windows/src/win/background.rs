// SPDX-License-Identifier: Apache-2.0
use super::*;
use windows::Win32::{
    Foundation::{HANDLE_FLAG_INHERIT, HANDLE_FLAGS, SetHandleInformation, WAIT_OBJECT_0},
    System::Console::{
        GetConsoleProcessList, GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
    },
};

/// True when this process is the only one attached to its console, so the
/// window exists for it alone and closes when it ends. False in a terminal,
/// where the shell is attached too, and for a process with no console.
pub fn own_console() -> bool {
    let mut attached = [0u32; 2];
    // SAFETY: the call writes at most the buffer's length of process ids.
    unsafe { GetConsoleProcessList(&mut attached) == 1 }
}

/// Keep this process's standard input, output and error from passing to the
/// programs it starts. Windows gives a started program every inheritable
/// handle, not only the ones it is told to use, so a background Øphio would
/// otherwise hold open a pipe that a script reads this command's output from,
/// and the script would wait for as long as Øphio runs.
pub fn keep_std_handles() {
    for id in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
        // SAFETY: a standard handle is owned by this process and stays open;
        // only its inherit flag changes. A missing handle is skipped.
        unsafe {
            if let Ok(handle) = GetStdHandle(id)
                && !handle.is_invalid()
            {
                let _ = SetHandleInformation(handle, HANDLE_FLAG_INHERIT.0, HANDLE_FLAGS(0));
            }
        }
    }
}

/// True when this process runs with an administrator's elevated token.
pub fn elevated() -> io::Result<bool> {
    // SAFETY: GetCurrentProcess returns a pseudo-handle that is never closed.
    let token = token(unsafe { GetCurrentProcess() })?;
    let data = token_buffer(token.0, TokenElevation)?;
    // SAFETY: a successful TokenElevation query returns a TOKEN_ELEVATION.
    Ok(unsafe { &*data.as_ptr().cast::<TOKEN_ELEVATION>() }.TokenIsElevated != 0)
}

/// A running process, held open so that its end can be seen even after its
/// id is reused.
pub struct Process(Handle);
impl std::fmt::Debug for Process {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Process").finish_non_exhaustive()
    }
}
impl Process {
    pub fn open(id: u32) -> io::Result<Self> {
        // SAFETY: the returned handle is owned and closed by `Handle`.
        unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, id) }
            .map(|handle| Self(Handle(handle)))
            .map_err(failure)
    }
    /// True once the process has ended.
    pub fn ended(&self) -> bool {
        // SAFETY: a live process handle opened with SYNCHRONIZE; no wait.
        unsafe { WaitForSingleObject(self.0.0, 0) == WAIT_OBJECT_0 }
    }
}
