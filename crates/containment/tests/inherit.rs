// SPDX-License-Identifier: Apache-2.0
//! The lock that keeps private descriptors out of the processes the companion
//! starts. These tests are a program of their own: they hold the one
//! process-wide lock for a while, which would hold up other tests' starts.
#![cfg(unix)]
use containment::inherit::{TcpListener, UnixListener};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn listeners_accept_connections() {
    let listener = TcpListener::bind("127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let address = listener.local_addr().unwrap();
    let (accepted, connected) =
        tokio::join!(listener.accept(), tokio::net::TcpStream::connect(address));
    let ((mut accepted, peer), mut connected) = (accepted.unwrap(), connected.unwrap());
    assert_eq!(peer, connected.local_addr().unwrap());
    connected.write_all(b"t").await.unwrap();
    let mut byte = [0_u8];
    accepted.read_exact(&mut byte).await.unwrap();
    assert_eq!(&byte, b"t");

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s");
    let listener = UnixListener::bind(&path).unwrap();
    let (accepted, connected) =
        tokio::join!(listener.accept(), tokio::net::UnixStream::connect(&path));
    let (mut accepted, mut connected) = (accepted.unwrap(), connected.unwrap());
    connected.write_all(b"u").await.unwrap();
    accepted.read_exact(&mut byte).await.unwrap();
    assert_eq!(&byte, b"u");
}

#[cfg(target_os = "macos")]
mod apple {
    use containment::inherit::{private, spawn, start};
    use std::{
        io::Read,
        process::{Command, Stdio},
        sync::{
            Arc, Barrier, Mutex, MutexGuard, PoisonError,
            atomic::{AtomicBool, Ordering},
            mpsc,
        },
        thread,
        time::Duration,
    };

    /// These tests share the one process-wide lock, and a step waiting behind
    /// another test's must not be what that test waits for.
    fn alone() -> MutexGuard<'static, ()> {
        static SERIAL: Mutex<()> = Mutex::new(());
        SERIAL.lock().unwrap_or_else(PoisonError::into_inner)
    }

    #[test]
    fn a_private_step_waits_for_a_start_under_way() {
        let _alone = alone();
        let entered = Arc::new(Barrier::new(2));
        let done = Arc::new(AtomicBool::new(false));
        let starter = {
            let (entered, done) = (entered.clone(), done.clone());
            thread::spawn(move || {
                start(|| {
                    entered.wait();
                    thread::sleep(Duration::from_millis(200));
                    done.store(true, Ordering::SeqCst);
                })
            })
        };
        entered.wait();
        assert!(private(|| done.load(Ordering::SeqCst)));
        starter.join().unwrap();
    }

    #[test]
    fn starts_run_one_at_a_time() {
        let _alone = alone();
        let entered = Arc::new(Barrier::new(2));
        let done = Arc::new(AtomicBool::new(false));
        let starter = {
            let (entered, done) = (entered.clone(), done.clone());
            thread::spawn(move || {
                start(|| {
                    entered.wait();
                    thread::sleep(Duration::from_millis(200));
                    done.store(true, Ordering::SeqCst);
                })
            })
        };
        entered.wait();
        assert!(
            start(|| done.load(Ordering::SeqCst)),
            "a start ran alongside another"
        );
        starter.join().unwrap();
    }

    /// Accepts on every relay go on together; only a start holds them back.
    #[test]
    fn private_steps_run_together() {
        let _alone = alone();
        let (inside, is_inside) = mpsc::channel();
        let (leave, may_leave) = mpsc::channel::<()>();
        let other = thread::spawn(move || {
            private(|| {
                inside.send(()).unwrap();
                may_leave.recv_timeout(Duration::from_secs(5)).is_ok()
            })
        });
        is_inside.recv().unwrap();
        private(|| {
            let _ = leave.send(());
        });
        assert!(
            other.join().unwrap(),
            "a private step waited for another to end"
        );
    }

    /// A process this test started, ended however the test ends.
    struct Ended(std::process::Child);
    impl Drop for Ended {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    /// The standard library makes a start's pipes (its program's input and
    /// output, and the one that reports a failed start) inside the start, and
    /// on the Mac marks them close-on-exec only after making them. A program
    /// another start launched in between would keep them: another run's agent
    /// could read what goes through them, and their reader would never see
    /// their end, as a keychain read waiting for `security` to finish never
    /// would. This start makes a pipe the same way.
    #[test]
    fn a_start_keeps_no_pipe_another_start_is_making() {
        use rustix::io::{FdFlags, fcntl_setfd, ioctl_fionbio};
        let _alone = alone();
        let (made, is_made) = mpsc::channel();
        let (started, has_started) = mpsc::channel::<()>();
        let maker = thread::spawn(move || {
            start(|| {
                let (reader, writer) = std::io::pipe().unwrap();
                fcntl_setfd(&reader, FdFlags::empty()).unwrap();
                fcntl_setfd(&writer, FdFlags::empty()).unwrap();
                made.send(()).unwrap();
                // Time enough for the other start, were it not held back.
                let overlapped = has_started.recv_timeout(Duration::from_millis(500)).is_ok();
                fcntl_setfd(&reader, FdFlags::CLOEXEC).unwrap();
                fcntl_setfd(&writer, FdFlags::CLOEXEC).unwrap();
                (reader, writer, overlapped)
            })
        });
        is_made.recv().unwrap();
        let other = Ended(
            spawn(
                Command::new("/bin/sleep")
                    .arg("30")
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null()),
            )
            .unwrap(),
        );
        let _ = started.send(());
        let (mut reader, writer, overlapped) = maker.join().unwrap();
        drop(writer);
        ioctl_fionbio(&reader, true).unwrap();
        let end = reader.read(&mut [0_u8]);
        drop(other);
        assert!(
            matches!(end, Ok(0)),
            "the program started meanwhile kept the pipe: {end:?}"
        );
        assert!(!overlapped, "a program started during another start");
    }

    /// What a run's agent is started with keeps out of a private step, so it
    /// can't inherit a relay that is being made or closed.
    #[test]
    fn a_container_start_waits_for_a_private_step() {
        let _alone = alone();
        let (held, holding) = mpsc::channel();
        let (release, released) = mpsc::channel::<()>();
        let holder = thread::spawn(move || {
            private(|| {
                held.send(()).unwrap();
                released.recv().unwrap();
            })
        });
        holding.recv().unwrap();
        let started = Arc::new(AtomicBool::new(false));
        let starter = {
            let started = started.clone();
            thread::spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                runtime.block_on(async {
                    let container = containment::Container::new().unwrap();
                    let mut child = container
                        .spawn(&mut tokio::process::Command::new("/usr/bin/true"))
                        .await
                        .unwrap();
                    started.store(true, Ordering::SeqCst);
                    container.end().await.unwrap();
                    child.wait().await.unwrap();
                })
            })
        };
        thread::sleep(Duration::from_millis(300));
        assert!(
            !started.load(Ordering::SeqCst),
            "started during a private step"
        );
        release.send(()).unwrap();
        holder.join().unwrap();
        starter.join().unwrap();
        assert!(started.load(Ordering::SeqCst));
    }

    #[test]
    fn a_panic_does_not_wedge_the_lock() {
        let _alone = alone();
        let _ = thread::spawn(|| private(|| panic!("poison"))).join();
        let _ = thread::spawn(|| start(|| panic!("poison"))).join();
        start(|| ());
        private(|| ());
    }
}
