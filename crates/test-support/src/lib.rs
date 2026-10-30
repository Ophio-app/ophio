// SPDX-License-Identifier: Apache-2.0
//! Test-only helpers. Depend on this crate through `[dev-dependencies]`.

#[cfg(target_os = "linux")]
pub mod detached;
pub mod json;

#[cfg(unix)]
use std::{
    io::{self, Write},
    path::Path,
    process::{Command, Stdio},
};

/// Materialize a new executable fixture without opening it for writing here.
///
/// In a multithreaded test runner, another thread can fork while a fixture is
/// being written. Even a close-on-exec write descriptor survives in that child
/// until exec, so executing the fixture can fail with `ETXTBSY` after the parent
/// has closed its descriptor. Closing, flushing, syncing, or renaming the file
/// in the parent cannot fix this: the kernel tracks writers to the inode.
///
/// Only a separate, short-lived `install` process opens the executable inode
/// for writing. The parent writes a different, non-executable source inode and
/// waits for `install` to exit before returning. Sibling forks cannot inherit
/// the executable's writer. No spawn retries or global test lock are needed.
///
/// Requires the Unix `install` utility. `destination` must be a fresh path in
/// an existing test-owned directory; never rewrite an executable in use.
#[cfg(unix)]
pub fn executable(destination: impl AsRef<Path>, contents: impl AsRef<[u8]>) -> io::Result<()> {
    let destination = destination.as_ref();
    match destination.symlink_metadata() {
        Ok(_) => return Err(io::ErrorKind::AlreadyExists.into()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let parent = destination.parent().ok_or(io::ErrorKind::InvalidInput)?;
    let mut source = tempfile::NamedTempFile::new_in(parent)?;
    source.write_all(contents.as_ref())?;
    let status = Command::new("install")
        .arg("-m")
        .arg("0700")
        .arg("--")
        .arg(source.path())
        .arg(destination)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    if !status.success() {
        return Err(io::Error::other(format!(
            "fixture installer exited with {status}"
        )));
    }
    Ok(())
}

/// A temporary directory only its owner can enter, whatever the umask.
///
/// Sockets and secrets are only created in owner-only directories, and one
/// that others can read is refused. `tempfile::tempdir` follows the umask, so
/// under the common 022 its directory would be refused.
#[cfg(unix)]
pub fn private_tempdir() -> io::Result<tempfile::TempDir> {
    use std::os::unix::fs::PermissionsExt;
    tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::{os::unix::fs::PermissionsExt, sync::Barrier};

    #[test]
    fn fixtures_execute_immediately_during_parallel_creation_and_spawning() {
        let directory = tempfile::tempdir().unwrap();
        let ready = Barrier::new(8);
        std::thread::scope(|threads| {
            for thread in 0..8 {
                let ready = &ready;
                let directory = directory.path();
                threads.spawn(move || {
                    ready.wait();
                    for iteration in 0..16 {
                        let path = directory.join(format!("fixture ' {thread} {iteration}"));
                        let source = b"#!/bin/sh\nprintf '%s' 'fixture output'\n";
                        executable(&path, source).unwrap();
                        assert_eq!(std::fs::read(&path).unwrap(), source);
                        assert_eq!(path.metadata().unwrap().permissions().mode() & 0o777, 0o700);
                        let output = Command::new(&path).output().unwrap();
                        assert!(output.status.success());
                        assert_eq!(output.stdout, b"fixture output");
                    }
                });
            }
        });
    }

    #[test]
    fn refuses_to_overwrite_an_existing_fixture() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("fixture");
        executable(&path, b"#!/bin/sh\nexit 0\n").unwrap();
        assert_eq!(
            executable(&path, b"replacement").unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(std::fs::read(path).unwrap(), b"#!/bin/sh\nexit 0\n");
    }
}
