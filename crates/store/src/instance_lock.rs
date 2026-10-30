// SPDX-License-Identifier: Apache-2.0
//! Lifetime-scoped ownership of a companion directory or listener.

use std::fs::{File, TryLockError};

/// An exclusive lock, explicitly unlocked before its owning file is closed.
///
/// A close-on-exec descriptor can still be inherited by a concurrent fork. On
/// Unix, closing only the parent's descriptor leaves the shared open-file
/// description locked until that child execs or exits. Explicit unlock releases
/// ownership immediately, even while such a child still has its descriptor.
/// Keep this guard alive until all work protected by the lock has been drained.
#[derive(Debug)]
pub struct InstanceLock(File);

impl InstanceLock {
    /// Acquire a lock on an already opened file. Opening permissions and error
    /// translation remain the caller's responsibility. A failed acquisition
    /// never constructs a guard and therefore never unlocks somebody else's lock.
    pub fn acquire(file: File) -> Result<Self, TryLockError> {
        file.try_lock()?;
        Ok(Self(file))
    }
}

impl Drop for InstanceLock {
    fn drop(&mut self) {
        // Closing remains a fallback if the OS reports an unlock error. The file
        // is private and never exposed, so no caller can close it prematurely.
        let _ = self.0.unlock();
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::fs::OpenOptions;

    #[test]
    fn releases_ownership_while_a_duplicate_descriptor_is_still_open() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("instance.lock");
        let open = || {
            OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .open(&path)
                .unwrap()
        };
        let file = open();
        // dup and fork share the same open-file description on Unix. Retain it
        // deterministically instead of depending on a child pausing before exec.
        let inherited = file.try_clone().unwrap();
        let owner = InstanceLock::acquire(file).unwrap();
        assert!(matches!(
            InstanceLock::acquire(open()),
            Err(TryLockError::WouldBlock)
        ));
        drop(owner);
        let successor = InstanceLock::acquire(open()).unwrap();
        drop(inherited);
        assert!(matches!(
            InstanceLock::acquire(open()),
            Err(TryLockError::WouldBlock)
        ));
        drop(successor);
        InstanceLock::acquire(open()).unwrap();
    }
}
