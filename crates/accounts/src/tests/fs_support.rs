// SPDX-License-Identifier: Apache-2.0
//! Real filesystem failures, held only until the test drops the guard.
use std::path::Path;

#[cfg(windows)]
use std::{fs::File, os::windows::fs::OpenOptionsExt};
#[cfg(unix)]
use std::{fs::Permissions, os::unix::fs::PermissionsExt, path::PathBuf};

pub(super) struct Denied {
    #[cfg(unix)]
    path: PathBuf,
    #[cfg(unix)]
    permissions: Permissions,
    #[cfg(windows)]
    _held: File,
}

#[cfg(unix)]
impl Denied {
    fn permissions(path: &Path, mode: u32) -> Self {
        let permissions = std::fs::metadata(path).unwrap().permissions();
        let guard = Self {
            path: path.to_owned(),
            permissions,
        };
        std::fs::set_permissions(path, Permissions::from_mode(mode)).unwrap();
        guard
    }
}

#[cfg(unix)]
impl Drop for Denied {
    fn drop(&mut self) {
        // Restore even when an assertion unwinds, so temporary directories can
        // be cleaned up. Do not cause a second panic during that unwinding.
        let restored = std::fs::set_permissions(&self.path, self.permissions.clone());
        if !std::thread::panicking() {
            restored.unwrap();
        }
    }
}

#[cfg(windows)]
impl Denied {
    fn sharing(path: &Path, sharing: u32) -> Self {
        // FILE_FLAG_BACKUP_SEMANTICS permits opening a directory too. The
        // handle's lifetime, not the read-only attribute, enforces denial.
        let held = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(sharing)
            .custom_flags(0x0200_0000)
            .open(path)
            .unwrap();
        Self { _held: held }
    }
}

/// Deny private storage writes, including creation of a recovery copy.
/// Windows private storage validates the parent by opening it; an exclusive
/// directory handle prevents that open without making existing files unreadable.
pub(super) fn deny_directory_writes(path: &Path) -> Denied {
    #[cfg(unix)]
    {
        Denied::permissions(path, 0o500)
    }
    #[cfg(windows)]
    {
        Denied::sharing(path, 0)
    }
}

/// Keep a directory from being removed, while allowing ordinary reads/writes.
pub(super) fn deny_removal(path: &Path) -> Denied {
    #[cfg(unix)]
    {
        Denied::permissions(path, 0o500)
    }
    #[cfg(windows)]
    {
        // FILE_SHARE_READ | FILE_SHARE_WRITE, deliberately no FILE_SHARE_DELETE.
        Denied::sharing(path, 3)
    }
}

pub(super) fn deny_reads(path: &Path) -> Denied {
    #[cfg(unix)]
    {
        Denied::permissions(path, 0)
    }
    #[cfg(windows)]
    {
        Denied::sharing(path, 0)
    }
}

/// Check the actual platform protection, not Windows' read-only attribute.
pub(super) fn assert_owner_only(path: &Path) {
    #[cfg(unix)]
    assert_eq!(
        std::fs::metadata(path).unwrap().permissions().mode() & 0o077,
        0,
        "only the owner can open {path:?}"
    );
    #[cfg(windows)]
    if path.is_dir() {
        // Existing directories are verified, not repaired, by this operation.
        platform_windows::fs::private_dir(path).expect("owner-only directory");
    } else {
        // Checks owner, protected DACL and allowed principals on the open file.
        platform_windows::fs::protected_read(path, usize::MAX - 1).expect("owner-only file");
    }
}
