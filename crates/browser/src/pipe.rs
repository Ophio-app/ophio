// SPDX-License-Identifier: Apache-2.0
#[cfg(windows)]
use platform_windows::a11y::browser_pipe as windows_pipe;
use serde_json::{Value, json};
#[cfg(unix)]
use std::process::Stdio;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};
#[cfg(unix)]
use tokio::process::{Child, Command};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    sync::{Mutex as AsyncMutex, mpsc, oneshot},
};
use voice_control::Error;
#[cfg(windows)]
type Child = windows_pipe::Process;
type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, Error>>>>>;
type Events = Arc<Mutex<Follower>>;
/// Who follows the browser's events, and whether there will be any more.
#[derive(Default)]
struct Follower {
    sender: Option<mpsc::UnboundedSender<Value>>,
    ended: bool,
}
/// How long one call waits for the browser's answer.
const CALL_WAIT: Duration = Duration::from_secs(3);
/// Capturing waits for the renderer/compositor and PNG encoding, not just
/// the browser process. Give that work the same budget as loading a page:
/// a busy renderer can still return a valid picture after three seconds.
/// Other calls keep their short deadline; a capture is never retried.
const SCREENSHOT_WAIT: Duration = Duration::from_secs(15);
pub struct Pipe {
    input: mpsc::Sender<Vec<u8>>,
    pending: Pending,
    events: Events,
    next: AtomicU64,
    child: AsyncMutex<Child>,
    closed: Arc<AtomicBool>,
}
impl std::fmt::Debug for Pipe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CdpPipe")
    }
}
impl Pipe {
    pub async fn launch(
        executable: &Path,
        profile: &Path,
        headless: bool,
        keep: bool,
        extra: &[&str],
    ) -> Result<Arc<Self>, Error> {
        let mut arguments: Vec<std::ffi::OsString> = [
            "--remote-debugging-pipe",
            "--no-first-run",
            "--no-default-browser-check",
            "--disable-background-networking",
            "--disable-component-update",
            "--disable-sync",
            "--disable-default-apps",
            "--disable-extensions",
            "--metrics-recording-only",
            "--disable-features=MediaRouter",
            "--password-store=basic",
        ]
        .into_iter()
        .map(Into::into)
        .collect();
        let mut profile_arg = std::ffi::OsString::from("--user-data-dir=");
        profile_arg.push(profile);
        arguments.push(profile_arg);
        if headless {
            arguments.extend(
                ["--headless=new", "--disable-gpu", "--window-size=1000,700"].map(Into::into),
            );
        }
        arguments.extend(extra.iter().map(Into::into));
        #[cfg(test)]
        if headless {
            arguments.push("--no-sandbox".into());
        }
        arguments.push("about:blank".into());
        #[cfg(unix)]
        let (input, output, child) = {
            let mut command = Command::new("/bin/sh");
            // Fixed shell syntax; executable and every argument are separate argv items.
            command
                .arg("-c")
                .arg("exec \"$@\" 3<&0 4>&1")
                .arg("browser-pipe")
                .arg(executable)
                .args(arguments)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .kill_on_drop(false);
            if headless {
                command
                    .env_remove("DISPLAY")
                    .env_remove("WAYLAND_DISPLAY")
                    .env_remove("DBUS_SESSION_BUS_ADDRESS");
            }
            // It starts while nothing private is being made, as other starts
            // do. It runs as long as the browser, so a pipe it kept from
            // another start would never close: a keychain read waiting for
            // `security` to finish would wait for the browser to end.
            let mut child = containment::inherit::tokio_spawn(&mut command)
                .map_err(|_| Error::Unavailable("The separate browser could not start".into()))?;
            (
                child.stdin.take().ok_or(Error::Stopped)?,
                child.stdout.take().ok_or(Error::Stopped)?,
                child,
            )
        };
        #[cfg(windows)]
        let (input, output, child) = windows_pipe::launch(executable, &arguments)?;
        let pending: Pending = Arc::default();
        let events: Events = Arc::default();
        let (sender, mut receiver) = mpsc::channel::<Vec<u8>>(16);
        tokio::spawn(async move {
            let mut input = input;
            while let Some(bytes) = receiver.recv().await {
                if input.write_all(&bytes).await.is_err() {
                    break;
                }
            }
        });
        let pipe = Arc::new(Self {
            input: sender,
            pending: pending.clone(),
            events: events.clone(),
            next: AtomicU64::new(1),
            child: AsyncMutex::new(child),
            closed: Arc::default(),
        });
        let profile = PathBuf::from(profile);
        let closed = Arc::downgrade(&pipe.closed);
        tokio::spawn(async move {
            let mut reader = BufReader::new(output);
            let mut frame = Vec::new();
            loop {
                frame.clear();
                // A peer cannot make the pipe reader allocate an unbounded message.
                let result = async {
                    loop {
                        let available = reader.fill_buf().await?;
                        if available.is_empty() {
                            return Ok::<bool, std::io::Error>(false);
                        }
                        let n = available
                            .iter()
                            .position(|b| *b == 0)
                            .map(|n| n + 1)
                            .unwrap_or(available.len());
                        if frame.len() + n > 8 * 1024 * 1024 {
                            return Ok(false);
                        }
                        frame.extend_from_slice(&available[..n]);
                        reader.consume(n);
                        if frame.last() == Some(&0) {
                            return Ok(true);
                        }
                    }
                }
                .await;
                if !matches!(result, Ok(true)) {
                    break;
                }
                frame.pop();
                let Ok(value) = serde_json::from_slice::<Value>(&frame) else {
                    continue;
                };
                if value.get("id").is_none() {
                    // Events go to whoever follows them; nobody may, and then
                    // they are dropped.
                    if let Ok(events) = events.lock()
                        && let Some(sender) = &events.sender
                    {
                        let _ = sender.send(value);
                    }
                    continue;
                }
                if let Some(id) = value["id"].as_u64()
                    && let Ok(mut waiters) = pending.lock()
                    && let Some(tx) = waiters.remove(&id)
                {
                    let _ = tx.send(if value.get("error").is_some() {
                        #[cfg(test)]
                        eprintln!(
                            "browser CDP error {}: {}",
                            value["error"]["code"], value["error"]["message"]
                        );
                        Err(Error::Unavailable(
                            "The browser refused the operation".into(),
                        ))
                    } else {
                        Ok(value["result"].clone())
                    });
                }
            }
            if let Ok(mut waiters) = pending.lock() {
                for (_, tx) in waiters.drain() {
                    let _ = tx.send(Err(Error::OutcomeUnknown));
                }
            }
            // Followers learn the browser is gone when their stream ends.
            if let Ok(mut events) = events.lock() {
                events.sender = None;
                events.ended = true;
            }
            // The pipe closes when the browser exits, though its helpers may
            // still be finishing. A profile that isn't kept goes once nothing
            // uses it.
            crate::profile::close_when_unused(profile, keep, closed).await;
        });
        Ok(pipe)
    }
    /// Whether the browser has exited and none of its processes remain.
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }
    /// The browser's events from now on, such as paused requests and new
    /// tabs. There is one follower; a new one replaces the last.
    pub fn events(&self) -> mpsc::UnboundedReceiver<Value> {
        let (sender, receiver) = mpsc::unbounded_channel();
        // Once the browser is gone, the stream ends at once.
        if let Ok(mut events) = self.events.lock()
            && !events.ended
        {
            events.sender = Some(sender);
        }
        receiver
    }
    pub async fn call(
        &self,
        session: Option<&str>,
        method: &str,
        params: Value,
    ) -> Result<Value, Error> {
        let wait = if method == "Page.captureScreenshot" {
            SCREENSHOT_WAIT
        } else {
            CALL_WAIT
        };
        self.call_within(session, method, params, wait).await
    }
    /// [`Self::call`], for operations that can take longer than a moment,
    /// such as opening an address.
    pub async fn call_within(
        &self,
        session: Option<&str>,
        method: &str,
        params: Value,
        wait: Duration,
    ) -> Result<Value, Error> {
        let id = self.next.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.pending
            .lock()
            .map_err(|_| Error::Stopped)?
            .insert(id, tx);
        struct Remove<'a>(&'a Pending, u64);
        impl Drop for Remove<'_> {
            fn drop(&mut self) {
                if let Ok(mut p) = self.0.lock() {
                    p.remove(&self.1);
                }
            }
        }
        let _remove = Remove(&self.pending, id);
        let mut value = json!({"id":id,"method":method,"params":params});
        if let Some(s) = session {
            value["sessionId"] = json!(s);
        }
        let mut bytes = serde_json::to_vec(&value).map_err(|_| Error::Stopped)?;
        bytes.push(0);
        #[cfg(test)]
        let started = std::time::Instant::now();
        let result = tokio::time::timeout(wait, async {
            // A cancelled caller must not leave half a JSON frame in the private pipe.
            self.input
                .send(bytes)
                .await
                .map_err(|_| Error::OutcomeUnknown)?;
            rx.await.map_err(|_| Error::OutcomeUnknown)?
        })
        .await
        .map_err(|_| Error::OutcomeUnknown)
        .and_then(|result| result);
        // Test failures must identify the operation, without logging its
        // arguments (which may contain a password or other page content).
        // How long it took tells a wait that ran out from a browser that went.
        #[cfg(test)]
        if let Err(error) = &result {
            eprintln!(
                "browser CDP {method} failed after {} ms: {error:?}",
                started.elapsed().as_millis()
            );
        }
        result
    }
    pub async fn close_for_test(&self) {
        self.close().await;
    }
    /// Closes the browser and waits for its process to exit.
    pub async fn close(&self) {
        let _ = self.call(None, "Browser.close", json!({})).await;
        #[cfg(windows)]
        self.child.lock().await.finish().await;
        #[cfg(unix)]
        {
            let mut child = self.child.lock().await;
            if tokio::time::timeout(Duration::from_secs(5), child.wait())
                .await
                .is_err()
            {
                let _ = child.kill().await;
                let _ = child.wait().await;
            }
        }
    }
}

#[cfg(all(test, target_os = "macos"))]
mod mac {
    use std::{
        os::unix::fs::PermissionsExt,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
            mpsc,
        },
        thread,
        time::Duration,
    };

    /// The browser's shell starts only while nothing private is being made,
    /// so it keeps none of the pipes another start is making.
    #[tokio::test]
    async fn the_browser_starts_only_between_private_steps() {
        let dir = tempfile::tempdir().unwrap();
        let browser = dir.path().join("browser");
        std::fs::write(&browser, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&browser, std::fs::Permissions::from_mode(0o755)).unwrap();
        let ended = Arc::new(AtomicBool::new(false));
        let (inside, is_inside) = mpsc::channel();
        let step = {
            let ended = ended.clone();
            thread::spawn(move || {
                containment::inherit::private(|| {
                    inside.send(()).unwrap();
                    thread::sleep(Duration::from_millis(300));
                    ended.store(true, Ordering::SeqCst);
                })
            })
        };
        is_inside.recv().unwrap();
        let launched = super::Pipe::launch(&browser, dir.path(), false, true, &[]).await;
        let started_after = ended.load(Ordering::SeqCst);
        step.join().unwrap();
        let pipe = launched.unwrap();
        pipe.child.lock().await.wait().await.unwrap();
        assert!(started_after, "the browser started during a private step");
    }
}
