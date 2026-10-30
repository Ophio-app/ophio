// SPDX-License-Identifier: Apache-2.0
//! Real PTY sessions with bounded byte-addressed replay.
//!
//! Attach snapshots and subscribes under the same lock used to publish output.
//! After `RecvError::Lagged`, discard that receiver and attach at the first byte
//! not yet rendered. Sessions survive attachments, but not a companion restart.

mod input;
mod process;

use std::{
    collections::{BTreeMap, VecDeque},
    io::{self, Read},
    sync::{Arc, Condvar, Mutex, MutexGuard},
    time::Duration,
};

use async_trait::async_trait;
use bytes::Bytes;
use portable_pty::{Child, MasterPty, PtySize, native_pty_system};
use ports::{
    clock::Clock,
    terminal::{Replay, TerminalError, TerminalEvent, TerminalHost, TerminalSpec},
};
use protocol::{
    ids::TerminalId,
    terminal::{TerminalInfo, TerminalState},
};
use tokio::sync::broadcast;

/// Memory and process limits. Exited sessions remain attachable until evicted.
#[derive(Debug, Clone)]
pub struct PtyConfig {
    pub max_terminals: usize,
    pub scrollback_bytes: usize,
    pub event_capacity: usize,
    pub retained_exited: usize,
}

impl Default for PtyConfig {
    fn default() -> Self {
        Self {
            max_terminals: 16,
            scrollback_bytes: 2 * 1024 * 1024,
            event_capacity: 256,
            retained_exited: 32,
        }
    }
}

#[derive(Debug)]
pub struct PtyHost {
    config: PtyConfig,
    clock: Arc<dyn Clock>,
    sessions: Arc<Mutex<BTreeMap<TerminalId, Arc<Session>>>>,
}

struct Control {
    master: Option<Box<dyn MasterPty + Send>>,
    child: Box<dyn Child + Send + Sync>,
    tree: process::ProcessTree,
    /// Where the program and what it starts are kept, so the companion knows
    /// them as its own however they detach.
    #[cfg(target_os = "linux")]
    group: Option<containment::TerminalGroup>,
    #[cfg(target_os = "linux")]
    initial_program: Option<process::ProgramIdentity>,
    closing: bool,
}

impl std::fmt::Debug for Control {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Control")
            .field("closing", &self.closing)
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
struct Stream {
    info: TerminalInfo,
    ring: VecDeque<u8>,
    capacity: usize,
    events: broadcast::Sender<TerminalEvent>,
}

struct Session {
    clock: Arc<dyn Clock>,
    stream: Mutex<Stream>,
    control: Mutex<Control>,
    finished: Condvar,
    input: input::Input,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("stream", &self.stream)
            .finish_non_exhaustive()
    }
}

/// Output is coalesced into frames at most this far apart.
const FLUSH_TICK: Duration = Duration::from_millis(8);

/// A Windows pseudo-console here inherits its cursor, so the first thing it
/// writes is a request for the cursor's position, `ESC [ 6 n`, and it shows
/// nothing else until it hears back. A new one has nothing to inherit, so the
/// companion answers at once with the top-left corner and keeps the request
/// out of the output: no shell waits for a phone to attach, and a phone that
/// attaches later does not answer it again into the shell.
#[cfg(any(windows, test))]
#[derive(Debug, Default)]
struct CursorRequest {
    seen: Vec<u8>,
    settled: bool,
}

#[cfg(any(windows, test))]
impl CursorRequest {
    const REQUEST: &[u8] = b"\x1b[6n";
    #[cfg(windows)]
    const ANSWER: &[u8] = b"\x1b[1;1R";

    /// The output to keep from `chunk`, and whether to answer now.
    fn take(&mut self, chunk: Vec<u8>) -> (Vec<u8>, bool) {
        if self.settled {
            return (chunk, false);
        }
        self.seen.extend_from_slice(&chunk);
        if self.seen.len() < Self::REQUEST.len() && Self::REQUEST.starts_with(&self.seen) {
            return (Vec::new(), false);
        }
        self.settled = true;
        let seen = std::mem::take(&mut self.seen);
        match seen.strip_prefix(Self::REQUEST) {
            Some(rest) => (rest.to_vec(), true),
            None => (seen, false),
        }
    }

    /// Output held back in case it began the request, when no more follows.
    fn unfinished(self) -> Vec<u8> {
        self.seen
    }
}

/// `deadline`, or the first later tick on its grid once it has passed.
fn next_tick(deadline: std::time::Instant) -> std::time::Instant {
    let now = std::time::Instant::now();
    match now.checked_duration_since(deadline) {
        None => deadline,
        Some(late) => {
            let into_tick = late.as_nanos() % FLUSH_TICK.as_nanos();
            now + FLUSH_TICK - Duration::from_nanos(into_tick as u64)
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn native_error(e: impl std::fmt::Display) -> TerminalError {
    // Native diagnostics can contain command lines or environment values.
    let _ = e;
    TerminalError::Io(io::Error::other("native terminal operation failed"))
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, TerminalError> + Send + 'static,
) -> Result<T, TerminalError> {
    tokio::task::spawn_blocking(f).await.map_err(native_error)?
}

impl PtyHost {
    pub fn new(config: PtyConfig, clock: Arc<dyn Clock>) -> Result<Self, TerminalError> {
        if config.max_terminals == 0 || config.scrollback_bytes == 0 || config.event_capacity == 0 {
            return Err(TerminalError::Io(io::Error::new(
                io::ErrorKind::InvalidInput,
                "terminal limits must be nonzero",
            )));
        }
        Ok(Self {
            config,
            clock,
            sessions: Arc::default(),
        })
    }

    fn session(&self, id: TerminalId) -> Result<Arc<Session>, TerminalError> {
        lock(&self.sessions)
            .get(&id)
            .cloned()
            .ok_or(TerminalError::NotFound)
    }
}

impl Default for PtyHost {
    fn default() -> Self {
        Self::new(PtyConfig::default(), Arc::new(ports::clock::SystemClock))
            .expect("valid default terminal limits")
    }
}

impl Drop for PtyHost {
    fn drop(&mut self) {
        for session in lock(&self.sessions).values() {
            let mut control = lock(&session.control);
            if matches!(lock(&session.stream).info.state, TerminalState::Running) {
                session.input.cancel();
                let _ = control.terminate();
            }
        }
    }
}

impl Control {
    fn terminate(&mut self) -> Result<(), TerminalError> {
        if self.closing {
            return Ok(());
        }
        self.tree.kill().map_err(TerminalError::Io)?;
        if self.child.try_wait()?.is_none() {
            self.child.kill()?;
        }
        self.closing = true;
        self.master.take();
        Ok(())
    }
}

impl Session {
    fn output(&self, bytes: &[u8]) {
        let mut stream = lock(&self.stream);
        if bytes.is_empty() || !matches!(stream.info.state, TerminalState::Running) {
            return;
        }
        stream.info.last_output_at = Some(self.clock.now());
        let offset = stream.info.output_offset;
        stream.info.output_offset += bytes.len() as u64;
        let keep = bytes.len().min(stream.capacity);
        let remove = (stream.ring.len() + bytes.len())
            .saturating_sub(stream.capacity)
            .min(stream.ring.len());
        stream.ring.drain(..remove);
        stream.ring.extend(&bytes[bytes.len() - keep..]);
        let _ = stream.events.send(TerminalEvent::Output {
            offset,
            bytes: Bytes::copy_from_slice(bytes),
        });
    }

    fn finish(&self, code: Option<i32>) {
        let mut stream = lock(&self.stream);
        stream.info.state = TerminalState::Exited { code };
        let _ = stream.events.send(TerminalEvent::Exited { code });
        self.finished.notify_all();
    }
}

#[async_trait]
impl TerminalHost for PtyHost {
    async fn open(&self, spec: TerminalSpec) -> Result<TerminalInfo, TerminalError> {
        if spec.cols == 0
            || spec.rows == 0
            || !spec.cwd.is_absolute()
            || !spec.cwd.is_dir()
            || spec.cwd.to_str().is_none()
            || spec
                .env
                .iter()
                .any(|(k, v)| k.is_empty() || k.contains(['=', '\0']) || v.contains('\0'))
        {
            return Err(TerminalError::Spawn(
                "invalid terminal size, directory or environment".into(),
            ));
        }
        let sessions = self.sessions.clone();
        let config = self.config.clone();
        let created_at = self.clock.now();
        let clock = self.clock.clone();
        blocking(move || {
            // Serialize admission and spawn so simultaneous opens cannot exceed the limit.
            let mut sessions = lock(&sessions);
            if sessions
                .values()
                .filter(|s| matches!(lock(&s.stream).info.state, TerminalState::Running))
                .count()
                >= config.max_terminals
            {
                return Err(TerminalError::Limit);
            }
            let exited: Vec<_> = sessions
                .iter()
                .filter_map(|(id, s)| {
                    matches!(lock(&s.stream).info.state, TerminalState::Exited { .. })
                        .then_some(*id)
                })
                .collect();
            for id in exited
                .iter()
                .take(exited.len().saturating_sub(config.retained_exited))
            {
                sessions.remove(id);
            }
            let pair = native_pty_system()
                .openpty(PtySize {
                    rows: spec.rows,
                    cols: spec.cols,
                    pixel_width: 0,
                    pixel_height: 0,
                })
                .map_err(native_error)?;
            #[cfg(unix)]
            let mut reader = input::Reader::new(pair.master.as_ref())?;
            #[cfg(windows)]
            let mut reader = pair.master.try_clone_reader().map_err(native_error)?;
            let input = input::Input::new(pair.master.as_ref())?;
            // The program's process joins a cgroup of its own before the
            // program runs, so nothing it starts, however early, is outside.
            // Where the computer offers no cgroup, only the program's ancestry
            // shows that the companion started it.
            #[cfg(target_os = "linux")]
            let group = containment::TerminalGroup::new().ok();
            #[cfg(target_os = "linux")]
            let mut child = process::spawn(pair.master.as_ref(), &spec, group.as_ref())?;
            #[cfg(not(target_os = "linux"))]
            let mut child = {
                let mut command = process::command(&spec)?;
                command.cwd(&spec.cwd);
                for (key, value) in &spec.env {
                    command.env(key, value);
                }
                command.env("TERM", "xterm-256color");
                pair.slave
                    .spawn_command(command)
                    .map_err(|_| TerminalError::Spawn("program could not be started".into()))?
            };
            drop(pair.slave);
            let tree = match process::ProcessTree::new(child.as_ref()) {
                Ok(tree) => tree,
                Err(e) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(e.into());
                }
            };
            #[cfg(target_os = "linux")]
            let initial_program = child
                .process_id()
                .and_then(|pid| process::ProgramIdentity::read(pid).ok());
            let info = TerminalInfo {
                terminal_id: TerminalId::new(),
                title: spec.title,
                cwd: spec.cwd.to_string_lossy().into_owned(),
                project_id: spec.project_id,
                harness: spec.harness,
                cols: spec.cols,
                rows: spec.rows,
                state: TerminalState::Running,
                created_at,
                output_offset: 0,
                last_output_at: None,
            };
            let session = Arc::new(Session {
                clock,
                stream: Mutex::new(Stream {
                    info: info.clone(),
                    ring: VecDeque::new(),
                    capacity: config.scrollback_bytes,
                    events: broadcast::channel(config.event_capacity).0,
                }),
                control: Mutex::new(Control {
                    master: Some(pair.master),
                    child,
                    tree,
                    #[cfg(target_os = "linux")]
                    group,
                    #[cfg(target_os = "linux")]
                    initial_program,
                    closing: false,
                }),
                finished: Condvar::new(),
                input,
            });
            let (done_tx, done_rx) = std::sync::mpsc::sync_channel(1);
            let (chunks_tx, chunks_rx) = std::sync::mpsc::sync_channel::<Result<Vec<u8>, bool>>(16);
            let output = session.clone();
            if let Err(e) = std::thread::Builder::new()
                .name("terminal-output".into())
                .spawn(move || {
                    let mut pending = Vec::with_capacity(16 * 1024);
                    let mut deadline = std::time::Instant::now() + FLUSH_TICK;
                    let clean = loop {
                        // Sleep while nothing is pending instead of waking on
                        // every tick; later output keeps the same flush grid.
                        let received = if pending.is_empty() {
                            let received = chunks_rx
                                .recv()
                                .map_err(|_| std::sync::mpsc::RecvTimeoutError::Disconnected);
                            deadline = next_tick(deadline);
                            received
                        } else {
                            chunks_rx.recv_timeout(
                                deadline.saturating_duration_since(std::time::Instant::now()),
                            )
                        };
                        match received {
                            Ok(Ok(bytes)) => {
                                if pending.len() + bytes.len() > 16 * 1024 {
                                    output.output(&pending);
                                    pending.clear();
                                }
                                pending.extend_from_slice(&bytes);
                            }
                            Ok(Err(clean)) => break clean,
                            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break false,
                            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                        }
                        if pending.len() >= 16 * 1024 || std::time::Instant::now() >= deadline {
                            if !pending.is_empty() {
                                output.output(&pending);
                                pending.clear();
                            }
                            deadline = std::time::Instant::now() + FLUSH_TICK;
                        }
                    };
                    if !pending.is_empty() {
                        output.output(&pending);
                    }
                    let _ = done_tx.send(clean);
                })
            {
                let _ = lock(&session.control).terminate();
                let _ = lock(&session.control).child.wait();
                return Err(e.into());
            }
            #[cfg(windows)]
            let answer = session.clone();
            if let Err(e) = std::thread::Builder::new()
                .name("terminal-reader".into())
                .spawn(move || {
                    let mut bytes = [0u8; 16 * 1024];
                    #[cfg(windows)]
                    let mut request = CursorRequest::default();
                    let clean = loop {
                        match reader.read(&mut bytes) {
                            Ok(0) => break true,
                            Ok(n) => {
                                let chunk = bytes[..n].to_vec();
                                #[cfg(windows)]
                                let chunk = {
                                    let (chunk, now) = request.take(chunk);
                                    if now {
                                        answer.input.answer_cursor(CursorRequest::ANSWER);
                                    }
                                    if chunk.is_empty() {
                                        continue;
                                    }
                                    chunk
                                };
                                if chunks_tx.send(Ok(chunk)).is_err() {
                                    return;
                                }
                            }
                            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                            // Unix PTY EOF is commonly EIO; other failures make the
                            // final outcome unknown instead of confirming a clean exit.
                            Err(e) => break cfg!(unix) && e.raw_os_error() == Some(5),
                        }
                    };
                    #[cfg(windows)]
                    {
                        let rest = request.unfinished();
                        if !rest.is_empty() {
                            let _ = chunks_tx.send(Ok(rest));
                        }
                    }
                    let _ = chunks_tx.send(Err(clean));
                })
            {
                let _ = lock(&session.control).terminate();
                let _ = lock(&session.control).child.wait();
                return Err(e.into());
            }
            let waiter = session.clone();
            #[cfg(target_os = "linux")]
            let pid = lock(&session.control).child.process_id();
            if let Err(e) = std::thread::Builder::new()
                .name("terminal-reaper".into())
                .spawn(move || {
                    let code = loop {
                        // Sleep until the program exits. It is left for
                        // try_wait to reap under the lock, as before.
                        #[cfg(target_os = "linux")]
                        if let Some(pid) = pid {
                            process::wait_exited(pid);
                        }
                        let status = lock(&waiter.control).child.try_wait();
                        match status {
                            Ok(Some(status)) => {
                                break if status.signal().is_some() {
                                    None
                                } else {
                                    i32::try_from(status.exit_code()).ok()
                                };
                            }
                            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
                            Err(_) => break None,
                        }
                    };
                    {
                        let mut control = lock(&waiter.control);
                        // A shell can exit while background jobs still hold its slave.
                        let _ = control.tree.kill();
                        control.master.take();
                        control.closing = true;
                    }
                    waiter.input.cancel();
                    // Drain before Exited; a broken native reader cannot stall forever.
                    let drained = done_rx
                        .recv_timeout(Duration::from_secs(2))
                        .unwrap_or(false);
                    waiter.finish(if drained { code } else { None });
                })
            {
                let _ = lock(&session.control).terminate();
                let _ = lock(&session.control).child.wait();
                return Err(e.into());
            }
            sessions.insert(info.terminal_id, session);
            Ok(info)
        })
        .await
    }

    async fn list(&self) -> Vec<TerminalInfo> {
        lock(&self.sessions)
            .values()
            .map(|s| lock(&s.stream).info.clone())
            .collect()
    }

    async fn info(&self, id: TerminalId) -> Option<TerminalInfo> {
        self.session(id).ok().map(|s| lock(&s.stream).info.clone())
    }

    async fn has_running_children(&self, id: TerminalId) -> Result<Option<bool>, TerminalError> {
        let session = self.session(id)?;
        blocking(move || {
            let control = lock(&session.control);
            #[cfg(target_os = "linux")]
            if let (Some(group), Some(pid)) = (&control.group, control.child.process_id()) {
                if group.has_other_processes(pid)? {
                    return Ok(Some(true));
                }
                // exec can turn the only remaining PID into the app itself.
                // Inspect identity after membership; failure is unknown.
                return Ok(control
                    .initial_program
                    .as_ref()
                    .and_then(|before| before.still_occupies(pid).ok())
                    .filter(|same| *same)
                    .map(|_| false));
            }
            #[cfg(windows)]
            {
                control
                    .tree
                    .has_running_children()
                    .map(Some)
                    .map_err(Into::into)
            }
            #[cfg(not(windows))]
            {
                drop(control);
                Ok(None)
            }
        })
        .await
    }

    async fn attach(
        &self,
        id: TerminalId,
        from_offset: Option<u64>,
    ) -> Result<(Replay, broadcast::Receiver<TerminalEvent>), TerminalError> {
        let session = self.session(id)?;
        let stream = lock(&session.stream);
        let end = stream.info.output_offset;
        let oldest = end - stream.ring.len() as u64;
        let requested = from_offset.unwrap_or(oldest);
        if requested > end {
            return Err(TerminalError::Io(io::Error::new(
                io::ErrorKind::InvalidInput,
                "replay offset is ahead of terminal output",
            )));
        }
        let from = requested.max(oldest);
        let bytes: Vec<_> = stream
            .ring
            .iter()
            .skip((from - oldest) as usize)
            .copied()
            .collect();
        let receiver = if let TerminalState::Exited { code } = stream.info.state {
            let (sender, receiver) = broadcast::channel(1);
            let _ = sender.send(TerminalEvent::Exited { code });
            receiver
        } else {
            stream.events.subscribe()
        };
        Ok((
            Replay {
                from,
                truncated: requested < oldest,
                bytes: Bytes::from(bytes),
            },
            receiver,
        ))
    }

    async fn write(&self, id: TerminalId, bytes: &[u8]) -> Result<(), TerminalError> {
        if bytes.len() > 1024 * 1024 {
            return Err(TerminalError::Limit);
        }
        let session = self.session(id)?;
        session.input.write(bytes).await
    }

    async fn resize(&self, id: TerminalId, cols: u16, rows: u16) -> Result<(), TerminalError> {
        if cols == 0 || rows == 0 {
            return Err(TerminalError::Io(io::Error::new(
                io::ErrorKind::InvalidInput,
                "terminal dimensions must be nonzero",
            )));
        }
        let session = self.session(id)?;
        blocking(move || {
            let control = lock(&session.control);
            control
                .master
                .as_ref()
                .ok_or(TerminalError::Exited)?
                .resize(PtySize {
                    cols,
                    rows,
                    pixel_width: 0,
                    pixel_height: 0,
                })
                .map_err(native_error)?;
            let mut stream = lock(&session.stream);
            stream.info.cols = cols;
            stream.info.rows = rows;
            let _ = stream.events.send(TerminalEvent::Resized { cols, rows });
            Ok(())
        })
        .await
    }

    async fn close(&self, id: TerminalId) -> Result<(), TerminalError> {
        let session = self.session(id)?;
        session.input.cancel();
        blocking(move || {
            {
                let mut control = lock(&session.control);
                if matches!(
                    lock(&session.stream).info.state,
                    TerminalState::Exited { .. }
                ) {
                    return Ok(());
                }
                control.terminate()?;
            }
            let (state, timeout) = session
                .finished
                .wait_timeout_while(lock(&session.stream), Duration::from_secs(5), |s| {
                    matches!(s.info.state, TerminalState::Running)
                })
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if timeout.timed_out() && matches!(state.info.state, TerminalState::Running) {
                return Err(TerminalError::Io(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "terminal exit not yet confirmed",
                )));
            }
            Ok(())
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::CursorRequest;

    #[test]
    fn the_leading_cursor_request_is_answered_and_kept_out() {
        let mut request = CursorRequest::default();
        assert_eq!(request.take(b"\x1b[6".to_vec()), (vec![], false));
        assert_eq!(request.take(b"nPS> ".to_vec()), (b"PS> ".to_vec(), true));
        // Later requests are a program's own, for the phone to answer.
        assert_eq!(
            request.take(b"\x1b[6n".to_vec()),
            (b"\x1b[6n".to_vec(), false)
        );
        assert!(request.unfinished().is_empty());
    }

    #[test]
    fn other_output_passes_through_whole() {
        let mut request = CursorRequest::default();
        assert_eq!(request.take(b"\x1b".to_vec()), (vec![], false));
        assert_eq!(
            request.take(b"[2Jhi".to_vec()),
            (b"\x1b[2Jhi".to_vec(), false)
        );
        let mut request = CursorRequest::default();
        assert_eq!(request.take(b"\x1b[".to_vec()), (vec![], false));
        // Held bytes still reach the output if nothing follows them.
        assert_eq!(request.unfinished(), b"\x1b[");
    }
}
