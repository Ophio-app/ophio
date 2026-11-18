// SPDX-License-Identifier: Apache-2.0
//! Native Mac process identities and best-effort project-folder discovery.
//! A folder scan cannot prove that a detached process changed folders or exited;
//! the runner must keep its stop result uncertain without tree containment.
#![allow(unsafe_code)]

use std::{
    ffi::OsString,
    io,
    mem::{MaybeUninit, size_of},
    os::unix::ffi::OsStringExt,
    path::{Path, PathBuf},
};

// sys/proc_info.h defines this selector; libc exposes the function but not it.
const PROC_UID_ONLY: u32 = 4;

pub(crate) fn stamp(pid: u32) -> io::Result<Option<u64>> {
    let Some(info) = bsd_info(pid)? else {
        return Ok(None);
    };
    if info.pbi_status == libc::SZOMB {
        return Ok(None);
    }
    // A seconds-only stamp can mistake a reused process number for the check.
    info.pbi_start_tvsec
        .checked_mul(1_000_000)
        .and_then(|seconds| seconds.checked_add(info.pbi_start_tvusec))
        .map(Some)
        .ok_or_else(|| io::Error::other("The Mac returned an invalid process start time"))
}

pub(crate) fn boot_id() -> io::Result<String> {
    let mut bytes = [0u8; 128];
    let mut size = bytes.len();
    // SAFETY: The name is NUL-terminated. The initialized output buffer and its
    // size are valid, and null new-value pointers make this a read-only query.
    let result = unsafe {
        libc::sysctlbyname(
            c"kern.bootsessionuuid".as_ptr(),
            bytes.as_mut_ptr().cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    let bytes = bytes
        .get(..size)
        .ok_or_else(|| io::Error::other("The Mac returned an oversized boot identity"))?;
    let bytes = bytes.strip_suffix(&[0]).unwrap_or(bytes);
    let identity = std::str::from_utf8(bytes)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if identity.is_empty() || identity.contains('\0') {
        return Err(io::Error::other(
            "The Mac returned an empty or invalid boot identity",
        ));
    }
    Ok(identity.to_owned())
}

pub(crate) fn project_processes(project: &Path) -> io::Result<Vec<u32>> {
    let project = project.canonicalize()?;
    let mut found = Vec::new();
    for pid in user_processes()? {
        let Some(before) = stamp(pid)? else {
            continue;
        };
        let Some(cwd) = working_directory(pid)? else {
            continue;
        };
        // Do not carry a path from an old process into a reused process number.
        if cwd.starts_with(&project) && stamp(pid)? == Some(before) {
            found.push(pid);
        }
    }
    found.sort_unstable();
    found.dedup();
    Ok(found)
}

fn bsd_info(pid: u32) -> io::Result<Option<libc::proc_bsdinfo>> {
    let pid = native_pid(pid)?;
    let mut info = MaybeUninit::<libc::proc_bsdinfo>::uninit();
    // SAFETY: The flavor expects exactly proc_bsdinfo. The output buffer has
    // that type's size and alignment; it is read only after a complete result.
    let count = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            size_of::<libc::proc_bsdinfo>() as i32,
        )
    };
    if !complete(count, size_of::<libc::proc_bsdinfo>())? {
        return Ok(None);
    }
    // SAFETY: A complete proc_pidinfo result initialized this integer-only C
    // structure. A short result or failed call has already returned above.
    Ok(Some(unsafe { info.assume_init() }))
}

fn working_directory(pid: u32) -> io::Result<Option<PathBuf>> {
    let pid = native_pid(pid)?;
    let mut info = MaybeUninit::<libc::proc_vnodepathinfo>::uninit();
    // SAFETY: PROC_PIDVNODEPATHINFO writes this exact C structure. The buffer
    // has sufficient size and alignment and is not read before a full result.
    let count = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDVNODEPATHINFO,
            0,
            info.as_mut_ptr().cast(),
            size_of::<libc::proc_vnodepathinfo>() as i32,
        )
    };
    if !complete(count, size_of::<libc::proc_vnodepathinfo>())? {
        return Ok(None);
    }
    // SAFETY: The successful full-sized call initialized this integer-only C
    // structure, including its fixed-size pathname buffer.
    let info = unsafe { info.assume_init() };
    let bytes: Vec<u8> = info
        .pvi_cdir
        .vip_path
        .into_iter()
        .flatten()
        .map(|byte| byte as u8)
        .take_while(|byte| *byte != 0)
        .collect();
    if bytes.is_empty() {
        return Err(io::Error::other(
            "The Mac did not report a process's working folder",
        ));
    }
    Ok(Some(PathBuf::from(OsString::from_vec(bytes))))
}

fn native_pid(pid: u32) -> io::Result<i32> {
    i32::try_from(pid)
        .ok()
        .filter(|pid| *pid > 0)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Invalid process number"))
}

/// Only a process that vanished is ignored. Permission failures and incomplete
/// kernel responses must not become a successful scan with missing processes.
fn complete(count: i32, expected: usize) -> io::Result<bool> {
    if count == expected as i32 {
        return Ok(true);
    }
    if count <= 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            return Ok(false);
        }
        return Err(error);
    }
    Err(io::Error::other(
        "The Mac returned incomplete process information",
    ))
}

fn user_processes() -> io::Result<Vec<u32>> {
    // SAFETY: geteuid takes no pointers and has no preconditions.
    let uid = unsafe { libc::geteuid() };
    let mut pids = vec![0i32; 1024];
    loop {
        let bytes = pids.len() * size_of::<i32>();
        // SAFETY: The output is an initialized, correctly aligned pid buffer;
        // its exact byte capacity is supplied. The selector takes this UID.
        let count = unsafe {
            libc::proc_listpids(PROC_UID_ONLY, uid, pids.as_mut_ptr().cast(), bytes as i32)
        };
        if count <= 0 {
            return Err(io::Error::last_os_error());
        }
        let count = count as usize;
        if count > bytes || !count.is_multiple_of(size_of::<i32>()) {
            return Err(io::Error::other("The Mac returned an invalid process list"));
        }
        if count < bytes {
            pids.truncate(count / size_of::<i32>());
            return Ok(pids
                .into_iter()
                .filter(|pid| *pid > 0)
                .map(|pid| pid as u32)
                .collect());
        }
        if pids.len() >= 1_048_576 {
            return Err(io::Error::other(
                "The Mac process list kept growing during the scan",
            ));
        }
        pids.resize(pids.len() * 2, 0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};

    #[test]
    fn identities_include_microseconds_and_boot_identity_is_stable() {
        let info = bsd_info(std::process::id()).unwrap().unwrap();
        let expected = info.pbi_start_tvsec * 1_000_000 + info.pbi_start_tvusec;
        assert_eq!(stamp(std::process::id()).unwrap(), Some(expected));
        assert_eq!(boot_id().unwrap(), boot_id().unwrap());
        assert!(!boot_id().unwrap().is_empty());
    }

    #[test]
    fn an_exited_process_has_no_live_stamp() {
        let mut child = Command::new("/bin/sleep")
            .arg("30")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let pid = child.id();
        assert!(stamp(pid).unwrap().is_some());
        child.kill().unwrap();
        child.wait().unwrap();
        assert_eq!(stamp(pid).unwrap(), None);
    }

    #[test]
    fn unreaped_zombie_has_no_kernel_stamp() {
        let mut child = Command::new("/bin/sleep").arg("30").spawn().unwrap();
        let native_pid = rustix::process::Pid::from_raw(child.id() as i32).unwrap();
        let before = stamp(child.id()).unwrap();
        child.kill().unwrap();
        rustix::process::waitid(
            rustix::process::WaitId::Pid(native_pid),
            rustix::process::WaitIdOptions::EXITED | rustix::process::WaitIdOptions::NOWAIT,
        )
        .unwrap();
        let after = stamp(child.id());
        child.wait().unwrap();
        assert!(before.is_some());
        // Ownership, not a second kernel lookup, must authorize normal-exit
        // cleanup while this zombie still pins the process-group number.
        assert_eq!(after.unwrap(), None);
    }

    #[test]
    fn discovers_a_live_process_in_a_nested_project_folder() {
        let project = tempfile::tempdir().unwrap();
        let nested = project.path().join("nested");
        std::fs::create_dir(&nested).unwrap();
        let mut child = Command::new("/bin/sleep")
            .arg("30")
            .current_dir(&nested)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let result = project_processes(project.path());
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(result.unwrap().contains(&child.id()));
    }
}
