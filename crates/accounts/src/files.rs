// SPDX-License-Identifier: Apache-2.0
//! Folders and files only the owner can open: the accounts' profiles, the
//! private-file vault and each account's own config folder.
use std::io;
use std::path::Path;

/// Creates `path` (and its parents) readable only by the owner, and refuses
/// one that already exists open to others or owned by someone else.
pub fn private_dir(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, MetadataExt};
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)?;
        let metadata = std::fs::symlink_metadata(path)?;
        if !metadata.is_dir()
            || metadata.uid() != rustix::process::geteuid().as_raw()
            || metadata.mode() & 0o077 != 0
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("{} is open to others", path.display()),
            ));
        }
        Ok(())
    }
    #[cfg(windows)]
    {
        platform_windows::fs::private_dir(path)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        Err(io::Error::other("owner-only storage is unavailable"))
    }
}

/// Replaces `path` whole with `bytes`, readable only by the owner. A reader
/// sees the old file or the new one, never half of either.
pub fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let parent = path.parent().ok_or_else(|| io::Error::other("no folder"))?;
        let name = path
            .file_name()
            .ok_or_else(|| io::Error::other("no file name"))?;
        let mut nonce = [0u8; 8];
        getrandom::fill(&mut nonce).map_err(|e| io::Error::other(e.to_string()))?;
        let temp = parent.join(format!(
            ".{}.{}",
            name.to_string_lossy(),
            u64::from_le_bytes(nonce)
        ));
        let written = (|| {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
                .open(&temp)?;
            file.write_all(bytes)?;
            file.sync_all()?;
            std::fs::rename(&temp, path)
        })();
        if written.is_err() {
            let _ = std::fs::remove_file(&temp);
        }
        written
    }
    #[cfg(windows)]
    {
        platform_windows::fs::atomic_write(path, bytes, true)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (path, bytes);
        Err(io::Error::other("owner-only storage is unavailable"))
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn only_the_owner_can_open_what_is_kept() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("a/b");
        private_dir(&dir).unwrap();
        let file = dir.join("key");
        write_private(&file, b"one").unwrap();
        write_private(&file, b"two").unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), b"two");
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&file), 0o600);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);

        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(private_dir(&dir).is_err());
    }
}
