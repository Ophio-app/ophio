// SPDX-License-Identifier: Apache-2.0
//! Coordination for short, unattended Claude Code processes. These processes
//! share a login, even when they belong to different adapters or companions.
use std::{future::Future, io, path::PathBuf, time::Duration};

/// Token renewal includes a server request followed by a credential-store
/// write. EOF is not permission to interrupt either operation.
pub const EXIT_GRACE: Duration = Duration::from_secs(120);
const LOCK_WAIT: Duration = Duration::from_secs(180);

/// Run a check in an owned task, serialized with every other check for this
/// computer login. Cancelling the caller does not drop the child or release
/// its lock while it is still finishing. User-started agent runs are separate.
///
/// The empty coordination file is not a credential. Never remove it: replacing
/// a locked file would let another process lock a different inode. A separate
/// open file description per check also serializes tasks in the same process.
pub async fn run<T, F>(home: Option<PathBuf>, operation: F) -> io::Result<T>
where
    T: Send + 'static,
    F: Future<Output = T> + Send + 'static,
{
    let home = home
        .or_else(|| {
            std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from)
        })
        .filter(|home| home.is_absolute())
        .ok_or_else(|| {
            io::Error::other("Claude check coordination needs an absolute home directory")
        })?;
    // A separate runtime also survives the companion runtime shutting down.
    // Merely detaching tokio::spawn would still drop its child on shutdown.
    // Blocking workers are joined during normal runtime shutdown, so the
    // process and lock remain owned until EOF cleanup has actually finished.
    let (answer, received) = tokio::sync::oneshot::channel();
    tokio::task::spawn_blocking(move || {
        let result = (|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?
                .block_on(async {
                    // Deliberately independent of executable, config directory and Ophio
                    // installation: all background checks for the same person share it.
                    if !home.is_dir() {
                        return Err(io::Error::new(
                            io::ErrorKind::NotFound,
                            "Claude check home directory is unavailable",
                        ));
                    }
                    let folder = home.join(".ophio");
                    std::fs::create_dir_all(&folder)?;
                    let mut options = std::fs::OpenOptions::new();
                    options.read(true).write(true).create(true).truncate(false);
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::OpenOptionsExt;
                        options.mode(0o600);
                    }
                    let lock = options.open(folder.join("claude-check.lock"))?;
                    tokio::time::timeout(LOCK_WAIT, async {
                        loop {
                            if answer.is_closed() {
                                return Err(io::Error::new(
                                    io::ErrorKind::Interrupted,
                                    "Claude check was cancelled before starting",
                                ));
                            }
                            match lock.try_lock() {
                                Ok(()) => return Ok(()),
                                Err(std::fs::TryLockError::WouldBlock) => {
                                    tokio::time::sleep(Duration::from_millis(25)).await;
                                }
                                Err(std::fs::TryLockError::Error(error)) => return Err(error),
                            }
                        }
                    })
                    .await
                    .map_err(|_| {
                        io::Error::new(
                            io::ErrorKind::TimedOut,
                            "another Claude check is still finishing",
                        )
                    })??;
                    let result = operation.await;
                    drop(lock);
                    Ok(result)
                })
        })();
        let _ = answer.send(result);
    });
    received.await.map_err(io::Error::other)?
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    fn home() -> PathBuf {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let home = std::env::temp_dir().join(format!(
            "ophio-check-lock-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&home).unwrap();
        home
    }

    #[tokio::test]
    async fn independent_checks_never_overlap_and_cancelled_callers_keep_the_lock() {
        let home = home();
        let active = Arc::new(AtomicUsize::new(0));
        let (started, ready) = tokio::sync::oneshot::channel();
        let (finish, finishing) = tokio::sync::oneshot::channel();
        let first = tokio::spawn(run(Some(home.clone()), {
            let active = active.clone();
            async move {
                assert_eq!(active.fetch_add(1, Ordering::SeqCst), 0);
                started.send(()).unwrap();
                finishing.await.unwrap();
                assert_eq!(active.fetch_sub(1, Ordering::SeqCst), 1);
            }
        }));
        ready.await.unwrap();
        first.abort();
        let second = tokio::spawn(run(Some(home.clone()), {
            let active = active.clone();
            async move {
                assert_eq!(active.load(Ordering::SeqCst), 0);
            }
        }));
        tokio::time::sleep(Duration::from_millis(75)).await;
        assert!(!second.is_finished());
        finish.send(()).unwrap();
        second.await.unwrap().unwrap();
        std::fs::remove_dir_all(home).unwrap();
    }

    #[tokio::test]
    async fn an_existing_operating_system_lock_prevents_a_check_from_starting() {
        let home = home();
        let folder = home.join(".ophio");
        std::fs::create_dir_all(&folder).unwrap();
        let lock = std::fs::File::create(folder.join("claude-check.lock")).unwrap();
        lock.lock().unwrap();
        let check = tokio::spawn(run(Some(home.clone()), async { 42 }));
        tokio::time::sleep(Duration::from_millis(75)).await;
        assert!(!check.is_finished());
        drop(lock);
        assert_eq!(check.await.unwrap().unwrap(), 42);
        std::fs::remove_dir_all(home).unwrap();
    }
    #[test]
    fn runtime_shutdown_waits_for_an_admitted_check_to_finish() {
        let home = home();
        let saved = home.join("saved");
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (started, ready) = tokio::sync::oneshot::channel();
        runtime.block_on(async {
            let saved = saved.clone();
            tokio::spawn(run(Some(home.clone()), async move {
                started.send(()).unwrap();
                tokio::time::sleep(Duration::from_millis(75)).await;
                std::fs::write(saved, "").unwrap();
            }));
            ready.await.unwrap();
        });
        drop(runtime);
        assert!(saved.exists());
        std::fs::remove_dir_all(home).unwrap();
    }

    // Invoked only by the subprocess test below, with a synthetic home.
    #[test]
    #[ignore = "subprocess helper"]
    fn other_companion_holds_the_lock() {
        let Some(home) = std::env::var_os("OPHIO_CHECK_TEST_HOME").map(PathBuf::from) else {
            return;
        };
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(home.join(".ophio/claude-check.lock"))
            .unwrap();
        lock.lock().unwrap();
        std::fs::write(home.join("ready"), "").unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !home.join("release").exists() {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[tokio::test]
    async fn a_different_companion_process_uses_the_same_lock() {
        let home = home();
        std::fs::create_dir_all(home.join(".ophio")).unwrap();
        std::fs::File::create(home.join(".ophio/claude-check.lock")).unwrap();
        let mut other = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "claude_checks::tests::other_companion_holds_the_lock",
            ])
            .env("OPHIO_CHECK_TEST_HOME", &home)
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            while !home.join("ready").exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let check = tokio::spawn(run(Some(home.clone()), async { 42 }));
        tokio::time::sleep(Duration::from_millis(75)).await;
        assert!(!check.is_finished());
        std::fs::write(home.join("release"), "").unwrap();
        assert_eq!(check.await.unwrap().unwrap(), 42);
        assert!(other.wait().unwrap().success());
        std::fs::remove_dir_all(home).unwrap();
    }
}
