// SPDX-License-Identifier: Apache-2.0
//! Keeps the companion's private descriptors out of the processes it starts.
//!
//! A process inherits every descriptor its parent has open that is not marked
//! close-on-exec. On Linux a socket, an accepted connection and a pipe are
//! made with that mark in one step. On Apple systems there is no such step:
//! each is made first and marked afterwards, so a process started between the
//! two keeps it for as long as it runs. A program started at that moment, a
//! run's agent among them, could then hold another run's model relay listener
//! and take its connections, or hold the pipe another start is making for its
//! program's input or output, or for the `security` program that reads and
//! writes the agent's sign-in in the keychain. A kept pipe exposes what goes
//! through it and keeps its reader from ever seeing the end of it.
//!
//! The Mac also copies every descriptor, marked or not, into a process while
//! it is being started, and drops the marked ones only once its program has
//! loaded. A socket closed meanwhile keeps listening until then.
//!
//! So, as Go does with its `ForkLock`, starting a process excludes everything
//! else that makes or closes a private descriptor. [`start`] wraps the start of
//! a process (only the start, never a wait for it) and takes the lock behind
//! them alone, so starts run one at a time and none sees the pipes another is
//! making. [`private`] wraps the making, accepting or closing of a private
//! descriptor; those steps run together, and wait for a start under way.
//! [`Private`] closes what it holds that way, and [`Accepting`],
//! [`TcpListener`] and [`UnixListener`] accept that way.
//!
//! A start with the standard library takes milliseconds when it uses
//! `posix_spawn`, which it does unless the command has a step to run before its
//! program (`pre_exec`), a user or group to change to, or a bare program name
//! looked up on a `PATH` it sets. Otherwise it forks, and its start ends only
//! once the new program has loaded or failed to: a fork-path start holds
//! every other start and private step until then, so keep commands started
//! under [`start`] on the `posix_spawn` path where there is any choice.
//!
//! Only what goes through here is covered. A process started without
//! [`start`] can still keep a descriptor made under [`private`] while it is
//! being made, and a start can still keep one made elsewhere without
//! [`private`], such as the sockets an HTTP client opens to a model provider.
//!
//! Neither may be called inside the other or inside itself on the same thread,
//! and a step under either must not wait for another thread's step: the lock
//! behind them is not reentrant, and a private step waits behind a start that
//! is waiting. Elsewhere than on Apple systems both just run their step.

/// Run `step`, which starts a process, while no other process is being
/// started and no private descriptor is being made or closed. Hold it for the
/// start alone, never for a wait for the process: every start and private step
/// waits for it.
pub fn start<T>(step: impl FnOnce() -> T) -> T {
    #[cfg(target_vendor = "apple")]
    let _alone = lock::LOCK
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    step()
}

/// Run `step`, which makes, accepts or closes a descriptor no other process
/// may hold, while no process is being started. Private steps run together.
/// Keep a step short and never let it wait on the network: starts wait for it.
pub fn private<T>(step: impl FnOnce() -> T) -> T {
    #[cfg(target_vendor = "apple")]
    let _shared = lock::LOCK
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    step()
}

/// Start `command` under [`start`].
pub fn spawn(command: &mut std::process::Command) -> std::io::Result<std::process::Child> {
    start(|| command.spawn())
}

/// Start `command` under [`start`] and wait for it outside it, collecting its
/// output as [`std::process::Command::output`] does. Its input is empty and
/// its output and errors are captured, whatever `command` said before.
pub fn output(command: &mut std::process::Command) -> std::io::Result<std::process::Output> {
    use std::process::Stdio;
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    spawn(command)?.wait_with_output()
}

/// Start the runtime's `command` under [`start`].
pub fn tokio_spawn(
    command: &mut tokio::process::Command,
) -> std::io::Result<tokio::process::Child> {
    start(|| command.spawn())
}

/// Start the runtime's `command` under [`start`] at once and wait for it
/// outside it, as [`tokio::process::Command::output`] does: its output and
/// errors are captured, and it is killed if the wait is dropped and the
/// command says so.
pub fn tokio_output(
    command: &mut tokio::process::Command,
) -> impl Future<Output = std::io::Result<std::process::Output>> + use<> {
    use std::process::Stdio;
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let child = tokio_spawn(command);
    async { child?.wait_with_output().await }
}

/// Start the runtime's `command` under [`start`] at once and wait for its end
/// outside it, as [`tokio::process::Command::status`] does.
pub fn tokio_status(
    command: &mut tokio::process::Command,
) -> impl Future<Output = std::io::Result<std::process::ExitStatus>> + use<> {
    let child = tokio_spawn(command);
    async {
        let mut child = child?;
        drop((child.stdin.take(), child.stdout.take(), child.stderr.take()));
        child.wait().await
    }
}

#[cfg(target_vendor = "apple")]
mod lock {
    pub(super) static LOCK: std::sync::RwLock<()> = std::sync::RwLock::new(());
}

/// A private descriptor (or anything owning one) that is closed under
/// [`private`], so no process being started keeps it a moment longer.
#[derive(Debug)]
pub struct Private<T>(Option<T>);

impl<T> Private<T> {
    pub fn new(value: T) -> Self {
        Self(Some(value))
    }
}

impl<T> std::ops::Deref for Private<T> {
    type Target = T;
    fn deref(&self) -> &T {
        self.0.as_ref().expect("held until dropped")
    }
}

impl<T> std::ops::DerefMut for Private<T> {
    fn deref_mut(&mut self) -> &mut T {
        self.0.as_mut().expect("held until dropped")
    }
}

#[cfg(unix)]
impl<T: std::os::fd::AsRawFd> std::os::fd::AsRawFd for Private<T> {
    fn as_raw_fd(&self) -> std::os::fd::RawFd {
        (**self).as_raw_fd()
    }
}

impl<T> Drop for Private<T> {
    fn drop(&mut self) {
        if let Some(value) = self.0.take() {
            private(|| drop(value));
        }
    }
}

/// A listener of the standard library whose connections [`Accepting`] takes
/// under [`private`].
#[cfg(target_vendor = "apple")]
pub trait Listen: std::os::fd::AsRawFd {
    /// The runtime's own stream for an accepted connection.
    type Stream;
    type Address;
    /// Make the listener's accepts return at once when none is waiting.
    fn make_nonblocking(&self) -> std::io::Result<()>;
    /// Accept a waiting connection under [`private`], and hand it to the
    /// runtime.
    fn accept_private(&self) -> std::io::Result<(Self::Stream, Self::Address)>;
}

#[cfg(target_vendor = "apple")]
impl Listen for std::net::TcpListener {
    type Stream = tokio::net::TcpStream;
    type Address = std::net::SocketAddr;
    fn make_nonblocking(&self) -> std::io::Result<()> {
        std::net::TcpListener::set_nonblocking(self, true)
    }
    fn accept_private(&self) -> std::io::Result<(Self::Stream, Self::Address)> {
        let (stream, address) = private(|| self.accept())?;
        stream.set_nonblocking(true)?;
        Ok((tokio::net::TcpStream::from_std(stream)?, address))
    }
}

#[cfg(target_vendor = "apple")]
impl Listen for std::os::unix::net::UnixListener {
    type Stream = tokio::net::UnixStream;
    type Address = std::os::unix::net::SocketAddr;
    fn make_nonblocking(&self) -> std::io::Result<()> {
        std::os::unix::net::UnixListener::set_nonblocking(self, true)
    }
    fn accept_private(&self) -> std::io::Result<(Self::Stream, Self::Address)> {
        let (stream, address) = private(|| self.accept())?;
        stream.set_nonblocking(true)?;
        Ok((tokio::net::UnixStream::from_std(stream)?, address))
    }
}

/// A listener whose connections are accepted, and which is closed, under
/// [`private`], so no process being started keeps the listener or a
/// connection to it. Make it inside the runtime that accepts.
#[cfg(target_vendor = "apple")]
#[derive(Debug)]
pub struct Accepting<L: Listen>(tokio::io::unix::AsyncFd<Private<L>>);

#[cfg(target_vendor = "apple")]
impl<L: Listen> Accepting<L> {
    /// Accept on `listener`, which this makes non-blocking.
    #[allow(unsafe_code)]
    pub fn new(listener: Private<L>) -> std::io::Result<Self> {
        listener.make_nonblocking()?;
        // SAFETY: the listener is owned here and moves into the AsyncFd, which
        // closes it only once it has been dropped; its descriptor is open and
        // stays the same until then.
        let registered = unsafe {
            tokio::io::unix::AsyncFd::register_with_interest(
                listener,
                tokio::io::Interest::READABLE,
            )
        };
        Ok(Self(registered?))
    }

    pub async fn accept(&self) -> std::io::Result<(L::Stream, L::Address)> {
        loop {
            let mut ready = self.0.readable().await?;
            if let Ok(accepted) = ready.try_io(|listener| listener.get_ref().accept_private()) {
                return accepted;
            }
        }
    }

    pub fn get_ref(&self) -> &L {
        self.0.get_ref()
    }
}

/// A TCP listener that, on Apple systems, is made, accepts and is closed under
/// [`private`]; elsewhere it is the runtime's own. Make it inside the runtime
/// that accepts.
#[derive(Debug)]
pub struct TcpListener {
    #[cfg(target_vendor = "apple")]
    inner: Accepting<std::net::TcpListener>,
    #[cfg(not(target_vendor = "apple"))]
    inner: tokio::net::TcpListener,
}

impl TcpListener {
    /// Listen at `address` as [`tokio::net::TcpListener::bind`] does.
    pub async fn bind(address: std::net::SocketAddr) -> std::io::Result<Self> {
        #[cfg(target_vendor = "apple")]
        {
            let listener = private(|| {
                let socket = match address {
                    std::net::SocketAddr::V4(_) => tokio::net::TcpSocket::new_v4()?,
                    std::net::SocketAddr::V6(_) => tokio::net::TcpSocket::new_v6()?,
                };
                socket.set_reuseaddr(true)?;
                socket.bind(address)?;
                socket.listen(1024)?.into_std()
            });
            let listener = Private::new(listener?);
            Ok(Self {
                inner: Accepting::new(listener)?,
            })
        }
        #[cfg(not(target_vendor = "apple"))]
        Ok(Self {
            inner: tokio::net::TcpListener::bind(address).await?,
        })
    }

    pub async fn accept(&self) -> std::io::Result<(tokio::net::TcpStream, std::net::SocketAddr)> {
        self.inner.accept().await
    }

    pub fn local_addr(&self) -> std::io::Result<std::net::SocketAddr> {
        #[cfg(target_vendor = "apple")]
        return self.inner.get_ref().local_addr();
        #[cfg(not(target_vendor = "apple"))]
        self.inner.local_addr()
    }
}

/// A Unix socket listener that, on Apple systems, is made, accepts and is
/// closed under [`private`]; elsewhere it is the runtime's own. Make it inside
/// the runtime that accepts.
#[cfg(unix)]
#[derive(Debug)]
pub struct UnixListener {
    #[cfg(target_vendor = "apple")]
    inner: Accepting<std::os::unix::net::UnixListener>,
    #[cfg(not(target_vendor = "apple"))]
    inner: tokio::net::UnixListener,
}

#[cfg(unix)]
impl UnixListener {
    /// Listen at `path` as [`tokio::net::UnixListener::bind`] does.
    pub fn bind(path: &std::path::Path) -> std::io::Result<Self> {
        #[cfg(target_vendor = "apple")]
        {
            let listener = private(|| std::os::unix::net::UnixListener::bind(path));
            Ok(Self {
                inner: Accepting::new(Private::new(listener?))?,
            })
        }
        #[cfg(not(target_vendor = "apple"))]
        Ok(Self {
            inner: tokio::net::UnixListener::bind(path)?,
        })
    }

    /// Accept a connection; its address, always unnamed, is left out.
    pub async fn accept(&self) -> std::io::Result<tokio::net::UnixStream> {
        Ok(self.inner.accept().await?.0)
    }
}
