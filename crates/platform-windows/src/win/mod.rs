// SPDX-License-Identifier: Apache-2.0
//! Small native ownership primitives shared by the Windows adapters.
pub(crate) mod background;
pub(crate) mod dpapi;
pub(crate) mod fs;
pub(crate) mod open_url;
pub(crate) mod pipe;
pub(crate) mod protection;
pub(crate) mod started;
pub(crate) mod startup;
pub(crate) mod system;
use crate::security::{ObjectKind, UserSid};
use std::{io, os::windows::ffi::OsStrExt, path::Path};
use windows::{
    Win32::{
        Foundation::{CloseHandle, HANDLE, HLOCAL, LocalFree},
        Security::{Authorization::*, *},
        System::Threading::*,
    },
    core::{PCWSTR, PWSTR},
};

pub(crate) fn denied() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "Windows owner security policy refused access",
    )
}
pub(crate) fn failure(error: windows::core::Error) -> io::Error {
    let kind = match error.code().0 as u32 & 0xffff {
        2 | 3 => io::ErrorKind::NotFound,
        5 => io::ErrorKind::PermissionDenied,
        80 | 183 => io::ErrorKind::AlreadyExists,
        _ => io::ErrorKind::Other,
    };
    io::Error::new(kind, "Windows operation failed")
}
pub(crate) fn wide(value: &std::ffi::OsStr) -> io::Result<Vec<u16>> {
    let mut out: Vec<_> = value.encode_wide().collect();
    if out.contains(&0) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "embedded NUL"));
    }
    out.push(0);
    Ok(out)
}
pub(crate) fn wide_path(path: &Path) -> io::Result<Vec<u16>> {
    wide(path.as_os_str())
}

pub(crate) struct Handle(pub HANDLE);
impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: this wrapper owns one valid CloseHandle-compatible handle.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}
pub(crate) struct Local(pub *mut std::ffi::c_void);
impl Drop for Local {
    fn drop(&mut self) {
        // SAFETY: memory returned by a documented LocalAlloc-family API.
        unsafe {
            let _ = LocalFree(Some(HLOCAL(self.0)));
        }
    }
}
pub(crate) fn sid_text(sid: PSID) -> io::Result<String> {
    let mut text = PWSTR::null();
    // SAFETY: callers pass a valid SID backed by a live token/descriptor buffer.
    unsafe {
        ConvertSidToStringSidW(sid, &mut text).map_err(failure)?;
        let _allocation = Local(text.0.cast());
        text.to_string().map_err(|_| denied())
    }
}
pub(crate) fn token(process: HANDLE) -> io::Result<Handle> {
    let mut result = HANDLE::default();
    // SAFETY: process is a live process handle; output lives through the call.
    unsafe {
        OpenProcessToken(process, TOKEN_QUERY, &mut result).map_err(failure)?;
    }
    Ok(Handle(result))
}
pub(crate) fn token_buffer(
    token: HANDLE,
    class: TOKEN_INFORMATION_CLASS,
) -> io::Result<Vec<usize>> {
    let mut needed = 0;
    // SAFETY: null buffer requests required size. Allocation is pointer-aligned.
    unsafe {
        let _ = GetTokenInformation(token, class, None, 0, &mut needed);
    }
    if needed == 0 || needed > 64 * 1024 {
        return Err(denied());
    }
    let mut data = vec![0usize; (needed as usize).div_ceil(size_of::<usize>())];
    // SAFETY: buffer is aligned and has at least `needed` initialized bytes.
    unsafe {
        GetTokenInformation(
            token,
            class,
            Some(data.as_mut_ptr().cast()),
            needed,
            &mut needed,
        )
        .map_err(failure)?;
    }
    Ok(data)
}
pub(crate) fn process_sid(process: HANDLE) -> io::Result<UserSid> {
    let token = token(process)?;
    let data = token_buffer(token.0, TokenUser)?;
    // SAFETY: successful TokenUser query returns TOKEN_USER followed by its SID.
    let user = unsafe { &*data.as_ptr().cast::<TOKEN_USER>() };
    UserSid::parse(&sid_text(user.User.Sid)?).map_err(|_| denied())
}
pub(crate) fn owner_sid() -> io::Result<UserSid> {
    // SAFETY: GetCurrentProcess returns a process pseudo-handle; never closed.
    process_sid(unsafe { GetCurrentProcess() })
}
pub(crate) struct Descriptor {
    memory: Local,
}
impl Descriptor {
    pub(crate) fn new(owner: &UserSid, kind: ObjectKind) -> io::Result<Self> {
        let sddl = wide(std::ffi::OsStr::new(&crate::security::owner_sddl(
            owner, kind,
        )))?;
        let mut sd = PSECURITY_DESCRIPTOR::default();
        // SAFETY: NUL-terminated SDDL and valid out pointer; allocation owned below.
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                PCWSTR(sddl.as_ptr()),
                SDDL_REVISION_1,
                &mut sd,
                None,
            )
            .map_err(failure)?;
        }
        Ok(Self {
            memory: Local(sd.0),
        })
    }
    pub(crate) fn attributes(&self) -> SECURITY_ATTRIBUTES {
        SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: self.memory.0,
            bInheritHandle: false.into(),
        }
    }
}
/// Validate the descriptor read from the opened object, never a path lookup.
pub(crate) fn verify_owner(
    handle: HANDLE,
    owner: &UserSid,
    kind: SE_OBJECT_TYPE,
) -> io::Result<()> {
    let mut sd = PSECURITY_DESCRIPTOR::default();
    let mut sid = PSID::default();
    let mut acl = std::ptr::null_mut();
    // SAFETY: all outputs live through the call. Returned pointers share sd's allocation.
    unsafe {
        GetSecurityInfo(
            handle,
            kind,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            Some(&mut sid),
            None,
            Some(&mut acl),
            None,
            Some(&mut sd),
        )
        .ok()
        .map_err(failure)?;
        let _allocation = Local(sd.0);
        let mut control = 0;
        let mut revision = 0;
        GetSecurityDescriptorControl(sd, &mut control, &mut revision).map_err(failure)?;
        if control & SE_DACL_PROTECTED.0 == 0
            || sid_text(sid)? != owner.as_str()
            || acl.is_null()
            || (*acl).AceCount != 2
        {
            return Err(denied());
        }
        let mut users = Vec::new();
        for index in 0..2 {
            let mut raw = std::ptr::null_mut();
            GetAce(acl, index, &mut raw).map_err(failure)?;
            let header = &*raw.cast::<ACE_HEADER>();
            let sid_offset = std::mem::offset_of!(ACCESS_ALLOWED_ACE, SidStart);
            // Check the header and SID extent before forming a typed ACE or
            // passing the inline SID to another native parser.
            if header.AceType != 0
                || header.AceFlags != 0
                || usize::from(header.AceSize) < sid_offset + 8
            {
                return Err(denied());
            }
            let sid_bytes = raw.cast::<u8>().add(sid_offset);
            let sid_size = 8 + usize::from(*sid_bytes.add(1)) * 4;
            if sid_size > usize::from(header.AceSize) - sid_offset {
                return Err(denied());
            }
            let ace = &*raw.cast::<ACCESS_ALLOWED_ACE>();
            if ace.Mask != 0x1f01ff {
                return Err(denied());
            }
            let sid = PSID(sid_bytes.cast());
            if !IsValidSid(sid).as_bool() {
                return Err(denied());
            }
            users.push(sid_text(sid)?);
        }
        users.sort();
        let mut expected = vec![owner.as_str().to_owned(), "S-1-5-18".into()];
        expected.sort();
        if users != expected {
            return Err(denied());
        }
    }
    Ok(())
}
