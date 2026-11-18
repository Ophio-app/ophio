// SPDX-License-Identifier: Apache-2.0
//! Bounded retries for a freshly written executable, never for a failed child.
use std::{io, time::Duration};

const ATTEMPTS: usize = 10;
const RETRY_DELAY: Duration = Duration::from_millis(50);

fn busy(error: &io::Error) -> bool {
    #[cfg(unix)]
    return error.raw_os_error() == Some(rustix::io::Errno::TXTBSY.raw_os_error());
    #[cfg(not(unix))]
    {
        let _ = error;
        false
    }
}

/// Retry only executable-busy errors, at most ten attempts (450 ms of waits).
///
/// A sibling thread can fork while an executable is being copied and inherit
/// its writable descriptor until exec, even after our writer has closed. Sync,
/// rename and hard links cannot release that other process's inode writer.
/// Call this around launching, not around a child's exit-status validation.
pub fn retry<T>(attempt: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    retry_with_wait(attempt, || std::thread::sleep(RETRY_DELAY))
}

fn retry_with_wait<T>(
    mut attempt: impl FnMut() -> io::Result<T>,
    mut wait: impl FnMut(),
) -> io::Result<T> {
    for index in 0..ATTEMPTS {
        match attempt() {
            Err(error) if busy(&error) && index + 1 < ATTEMPTS => wait(),
            result => return result,
        }
    }
    unreachable!("the final attempt always returns")
}

/// Asynchronous counterpart to `retry`, without blocking a runtime worker.
pub async fn spawn(command: &mut tokio::process::Command) -> io::Result<tokio::process::Child> {
    // Each attempt starts as other starts do, while nothing private is being
    // made; the wait between attempts holds nothing up.
    retry_async_with_wait(
        || containment::inherit::tokio_spawn(command),
        || tokio::time::sleep(RETRY_DELAY),
    )
    .await
}

async fn retry_async_with_wait<T, F: std::future::Future<Output = ()>>(
    mut attempt: impl FnMut() -> io::Result<T>,
    mut wait: impl FnMut() -> F,
) -> io::Result<T> {
    for index in 0..ATTEMPTS {
        match attempt() {
            Err(error) if busy(&error) && index + 1 < ATTEMPTS => wait().await,
            result => return result,
        }
    }
    unreachable!("the final attempt always returns")
}

/// Capture output with the same null input and piped output defaults as output().
pub async fn output(command: &mut tokio::process::Command) -> io::Result<std::process::Output> {
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    spawn(command).await?.wait_with_output().await
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::{
        cell::Cell,
        io::{BufRead, Write},
        process::{Child, Command, Stdio},
    };

    struct Writer(Child);

    impl Writer {
        fn hold(program: &std::path::Path) -> Self {
            // Open the writable descriptor only in a separate process, so
            // unrelated test threads cannot inherit it when they fork. The
            // readiness pipe proves it is open before either launch attempt.
            let mut child = Command::new("/bin/sh")
                .args([
                    "-c",
                    "exec 3<> \"$1\" || exit; printf 'ready\\n'; IFS= read -r release",
                    "writer",
                ])
                .arg(program)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()
                .unwrap();
            let mut ready = String::new();
            std::io::BufReader::new(child.stdout.take().unwrap())
                .read_line(&mut ready)
                .unwrap();
            assert_eq!(ready, "ready\n");
            Self(child)
        }
    }

    impl Drop for Writer {
        fn drop(&mut self) {
            let _ = self.0.stdin.take().unwrap().write_all(b"release\n");
            let _ = self.0.wait();
        }
    }

    fn program(dir: &std::path::Path) -> std::path::PathBuf {
        let path = dir.join("program");
        test_support::executable(&path, b"#!/bin/sh\nprintf 'launched'\n").unwrap();
        path
    }

    #[test]
    fn a_held_writer_fails_without_retry_and_launches_after_release() {
        let dir = tempfile::tempdir().unwrap();
        let program = program(dir.path());
        let mut writer = Some(Writer::hold(&program));
        // Recovery fixtures execute a hard link to the copied binary. The
        // writer is attached to the inode, not to the name used at launch.
        let entry = dir.path().join("installed-program");
        std::fs::hard_link(&program, &entry).unwrap();
        let mut command = Command::new(&entry);
        let original_error = command.output().unwrap_err();
        assert!(busy(&original_error), "{original_error}");
        let attempts = Cell::new(0);
        let output = retry(|| {
            attempts.set(attempts.get() + 1);
            if attempts.get() == 2 {
                // Release only after the production retry has observed the
                // first ETXTBSY. Its backoff does not create the test race.
                drop(writer.take());
            }
            command.output()
        })
        .unwrap();
        assert_eq!(attempts.get(), 2);
        assert!(writer.is_none());
        assert!(output.status.success());
        assert_eq!(output.stdout, b"launched");
    }

    #[tokio::test]
    async fn async_launch_retries_a_held_writer_until_coordinated_release() {
        let dir = tempfile::tempdir().unwrap();
        let program = program(dir.path());
        let mut writer = Some(Writer::hold(&program));
        let mut command = tokio::process::Command::new(&program);
        command.stdout(std::process::Stdio::piped());
        let original_error = command.spawn().unwrap_err();
        assert!(busy(&original_error), "{original_error}");
        let attempts = Cell::new(0);
        let child = retry_async_with_wait(
            || {
                attempts.set(attempts.get() + 1);
                command.spawn()
            },
            || {
                drop(writer.take());
                std::future::ready(())
            },
        )
        .await
        .unwrap();
        let output = child.wait_with_output().await.unwrap();
        assert_eq!(attempts.get(), 2);
        assert!(writer.is_none());
        assert!(output.status.success());
        assert_eq!(output.stdout, b"launched");
    }

    #[test]
    fn persistent_busy_is_bounded_and_other_errors_are_not_retried() {
        for errno in [
            rustix::io::Errno::TXTBSY,
            rustix::io::Errno::ACCESS,
            rustix::io::Errno::NOENT,
        ] {
            let attempts = Cell::new(0);
            let waits = Cell::new(0);
            let error = retry_with_wait(
                || {
                    attempts.set(attempts.get() + 1);
                    Err::<(), _>(io::Error::from_raw_os_error(errno.raw_os_error()))
                },
                || waits.set(waits.get() + 1),
            )
            .unwrap_err();
            let expected = if errno == rustix::io::Errno::TXTBSY {
                ATTEMPTS
            } else {
                1
            };
            assert_eq!(attempts.get(), expected);
            assert_eq!(waits.get(), expected - 1);
            assert_eq!(error.raw_os_error(), Some(errno.raw_os_error()));
        }
    }

    #[tokio::test]
    async fn async_retry_is_bounded_and_preserves_other_errors() {
        for errno in [
            rustix::io::Errno::TXTBSY,
            rustix::io::Errno::ACCESS,
            rustix::io::Errno::NOENT,
        ] {
            let attempts = Cell::new(0);
            let waits = Cell::new(0);
            let error = retry_async_with_wait(
                || {
                    attempts.set(attempts.get() + 1);
                    Err::<(), _>(io::Error::from_raw_os_error(errno.raw_os_error()))
                },
                || {
                    waits.set(waits.get() + 1);
                    std::future::ready(())
                },
            )
            .await
            .unwrap_err();
            let expected = if errno == rustix::io::Errno::TXTBSY {
                ATTEMPTS
            } else {
                1
            };
            assert_eq!(attempts.get(), expected);
            assert_eq!(waits.get(), expected - 1);
            assert_eq!(error.raw_os_error(), Some(errno.raw_os_error()));
        }
    }

    #[tokio::test]
    async fn a_started_child_is_not_retried_after_an_unsuccessful_exit() {
        let dir = tempfile::tempdir().unwrap();
        let program = dir.path().join("fails");
        let marker = dir.path().join("starts");
        test_support::executable(&program, b"#!/bin/sh\necho started >> \"$1\"\nexit 7\n").unwrap();
        let output = output(tokio::process::Command::new(&program).arg(&marker))
            .await
            .unwrap();
        assert_eq!(output.status.code(), Some(7));
        assert_eq!(std::fs::read_to_string(marker).unwrap(), "started\n");
    }
}
