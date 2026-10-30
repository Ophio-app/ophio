// SPDX-License-Identifier: Apache-2.0
use ports::terminal::TerminalError;

#[cfg(unix)]
mod native {
    use super::TerminalError;
    use nix::{fcntl, libc};
    use std::{
        fs::File,
        io::{self, Read, Write},
        os::fd::{AsRawFd, BorrowedFd},
    };
    use tokio::{
        io::unix::AsyncFd,
        sync::{mpsc, oneshot, watch},
    };

    pub(crate) struct Input {
        requests: mpsc::Sender<Request>,
        cancelled: watch::Sender<bool>,
    }

    struct Request {
        bytes: Vec<u8>,
        done: oneshot::Sender<Result<(), TerminalError>>,
    }

    impl Input {
        #[allow(unsafe_code)]
        pub(crate) fn new(master: &dyn portable_pty::MasterPty) -> io::Result<Self> {
            let raw = master
                .as_raw_fd()
                .ok_or_else(|| io::Error::other("terminal master descriptor unavailable"))?;
            // SAFETY: master keeps the borrowed fd alive until duplication.
            let fd = unsafe { BorrowedFd::borrow_raw(raw) }.try_clone_to_owned()?;
            let flags = fcntl::OFlag::from_bits_truncate(fcntl::fcntl(raw, fcntl::F_GETFL)?);
            fcntl::fcntl(raw, fcntl::F_SETFL(flags | fcntl::OFlag::O_NONBLOCK))?;
            // Use a plain fd: portable-pty's Unix writer injects newline/VEOF
            // when dropped, which can echo while the reaper drains output.
            // SAFETY: the AsyncFd owns the duplicated fd, which nothing
            // replaces or closes until the AsyncFd is dropped.
            let fd = unsafe {
                AsyncFd::register_with_interest(File::from(fd), tokio::io::Interest::WRITABLE)
            }
            .map_err(io::Error::from)?;
            let (requests, mut pending) = mpsc::channel::<Request>(16);
            let (cancelled, mut closed) = watch::channel(false);
            // One task owns the fd and completes each accepted write in order,
            // even if its caller disconnects. No native worker waits on input.
            tokio::spawn(async move {
                tokio::select! {
                    biased;
                    _ = closed.wait_for(|closed| *closed) => {},
                    _ = async {
                        while let Some(request) = pending.recv().await {
                            let result = write_all(&fd, &request.bytes).await;
                            let _ = request.done.send(result);
                        }
                    } => {},
                }
            });
            Ok(Self {
                requests,
                cancelled,
            })
        }

        pub(crate) async fn write(&self, bytes: &[u8]) -> Result<(), TerminalError> {
            let mut cancelled = self.cancelled.subscribe();
            tokio::select! {
                biased;
                _ = cancelled.wait_for(|closed| *closed) => Err(TerminalError::Exited),
                result = async {
                    let (done, result) = oneshot::channel();
                    self.requests.send(Request { bytes: bytes.to_vec(), done }).await
                        .map_err(|_| TerminalError::Exited)?;
                    result.await.unwrap_or(Err(TerminalError::Exited))
                } => result,
            }
        }

        pub(crate) fn cancel(&self) {
            self.cancelled.send_replace(true);
        }
    }

    async fn write_all(fd: &AsyncFd<File>, mut bytes: &[u8]) -> Result<(), TerminalError> {
        while !bytes.is_empty() {
            let mut ready = fd.writable().await?;
            // A hung-up PTY can keep reporting EAGAIN together with HUP.
            // Retrying that permanent readiness would starve the runtime.
            if ready.ready().is_write_closed() || ready.ready().is_error() {
                return Err(io::Error::from(io::ErrorKind::BrokenPipe).into());
            }
            match ready.try_io(|fd| fd.get_ref().write(bytes)) {
                Ok(Ok(0)) => return Err(io::Error::from(io::ErrorKind::WriteZero).into()),
                Ok(Ok(n)) => bytes = &bytes[n..],
                Ok(Err(e)) if e.kind() == io::ErrorKind::Interrupted => continue,
                Ok(Err(e)) => return Err(e.into()),
                Err(_) => continue,
            }
        }
        Ok(())
    }

    impl Drop for Input {
        fn drop(&mut self) {
            self.cancel();
        }
    }

    // O_NONBLOCK is shared by the duplicated master descriptors. Keep the
    // native reader asleep on readiness rather than treating EAGAIN as EOF.
    pub(crate) struct Reader {
        fd: File,
    }

    impl Reader {
        #[allow(unsafe_code)]
        pub(crate) fn new(master: &dyn portable_pty::MasterPty) -> io::Result<Self> {
            let raw = master
                .as_raw_fd()
                .ok_or_else(|| io::Error::other("terminal master descriptor unavailable"))?;
            // SAFETY: master keeps the borrowed fd alive until duplication.
            let fd = unsafe { BorrowedFd::borrow_raw(raw) }.try_clone_to_owned()?;
            Ok(Self { fd: File::from(fd) })
        }
    }

    impl Read for Reader {
        #[allow(unsafe_code)]
        fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
            loop {
                match self.fd.read(bytes) {
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                        let mut fd = libc::pollfd {
                            fd: self.fd.as_raw_fd(),
                            events: libc::POLLIN,
                            revents: 0,
                        };
                        // SAFETY: poll borrows one live descriptor and a valid
                        // pollfd for the duration of the call.
                        if unsafe { libc::poll(&mut fd, 1, -1) } == -1 {
                            return Err(io::Error::last_os_error());
                        }
                    }
                    result => return result,
                }
            }
        }
    }
}

#[cfg(windows)]
mod native {
    use super::TerminalError;
    use crate::lock;
    use std::{
        io::{self, Write},
        sync::{
            Arc, Mutex,
            atomic::{AtomicBool, Ordering},
        },
    };

    pub(crate) struct Input {
        writer: Arc<Mutex<Option<Box<dyn Write + Send>>>>,
        cancelled: Arc<AtomicBool>,
        order: tokio::sync::Mutex<()>,
    }

    impl Input {
        pub(crate) fn new(master: &dyn portable_pty::MasterPty) -> io::Result<Self> {
            Ok(Self {
                writer: Arc::new(Mutex::new(Some(
                    master
                        .take_writer()
                        .map_err(|_| io::Error::other("terminal writer unavailable"))?,
                ))),
                cancelled: Arc::default(),
                order: tokio::sync::Mutex::new(()),
            })
        }

        pub(crate) async fn write(&self, bytes: &[u8]) -> Result<(), TerminalError> {
            let _order = self.order.lock().await;
            let writer = self.writer.clone();
            let cancelled = self.cancelled.clone();
            let bytes = bytes.to_vec();
            crate::blocking(move || Self::write_native(&writer, &cancelled, &bytes)).await
        }

        fn write_native(
            writer: &Mutex<Option<Box<dyn Write + Send>>>,
            cancelled: &AtomicBool,
            bytes: &[u8],
        ) -> Result<(), TerminalError> {
            let mut slot = lock(writer);
            if cancelled.load(Ordering::Acquire) {
                return Err(TerminalError::Exited);
            }
            let writer = slot.as_mut().ok_or(TerminalError::Exited)?;
            let result = writer.write_all(bytes).and_then(|()| writer.flush());
            if cancelled.load(Ordering::Acquire) {
                slot.take();
                return Err(TerminalError::Exited);
            }
            result.map_err(Into::into)
        }

        pub(crate) fn answer_cursor(&self, bytes: &[u8]) {
            let _ = Self::write_native(&self.writer, &self.cancelled, bytes);
        }

        pub(crate) fn cancel(&self) {
            self.cancelled.store(true, Ordering::Release);
            // Closing ConPTY releases native input. The reaper must never
            // wait for a writer holding this lock during that shutdown.
            if let Ok(mut writer) = self.writer.try_lock() {
                writer.take();
            }
        }
    }
}

pub(crate) use native::Input;
#[cfg(unix)]
pub(crate) use native::Reader;
