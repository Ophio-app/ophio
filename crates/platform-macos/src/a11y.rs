// SPDX-License-Identifier: Apache-2.0
//! Named-control metadata and authorized actions. Native references never leave
//! the serial worker thread. Field values and text interfaces are never read.
use async_trait::async_trait;
use protocol::voice::VoiceElement;
use std::sync::{Arc, mpsc};
use voice_control::{Accessibility, Authority, Error, Snapshot};

#[cfg(test)]
mod identity_tests;
#[cfg(target_os = "macos")]
mod native;
#[cfg(test)]
mod tests;

pub const PERMISSION: &str =
    "Allow Accessibility for this app in System Settings, Privacy & Security, then try again";
#[cfg(any(target_os = "macos", test))]
const SESSION: &str = "Mac named controls require an unlocked, active desktop session";
const UNSUPPORTED: &str = "Mac accessibility is unavailable on this computer";
#[cfg(any(target_os = "macos", test))]
const NO_CONTROLS: &str = "The focused Mac window exposes no accessible controls";
#[cfg(target_os = "macos")]
const SLOW: &str = "The focused Mac app did not answer accessibility requests in time";

#[cfg(any(target_os = "macos", test))]
struct Retained<T> {
    id: String,
    object: T,
}
/// Identity follows the retained native object, never its tree position or
/// metadata. Native passes CFEqual; synthetic fixtures use retained Arc identity.
#[cfg(any(target_os = "macos", test))]
fn retained_id<T>(
    object: &T,
    current: &[Retained<T>],
    previous: &[Retained<T>],
    equal: impl Fn(&T, &T) -> bool,
) -> Option<String> {
    current
        .iter()
        .chain(previous)
        .find(|e| equal(&e.object, object))
        .map(|e| e.id.clone())
}

#[cfg(any(target_os = "macos", test))]
#[derive(Debug)]
struct Frame {
    context: String,
    elements: Vec<VoiceElement>,
    window: Option<protocol::voice::VoiceBounds>,
}
/// Synchronous on purpose: construction, reference ownership, resolution and
/// dispatch all happen on the same thread, including destruction of references.
#[cfg(any(target_os = "macos", test))]
trait Backend {
    fn frame(&mut self) -> Result<Frame, Error>;
    fn resolve(&mut self, expected: &VoiceElement, context: &str) -> Result<(), Error>;
    /// Check authority after all native resolution and immediately before input.
    fn perform(&mut self, id: &str, focus: bool, authority: &dyn Authority) -> Result<bool, Error>;
}
#[cfg(any(target_os = "macos", test))]
struct Engine {
    backend: Box<dyn Backend>,
    last: Option<Snapshot>,
}
#[cfg(any(target_os = "macos", test))]
impl Engine {
    fn snapshot(&mut self) -> Result<Snapshot, Error> {
        // An unsuccessful observation invalidates every previously issued target.
        let previous = self.last.take();
        drop(previous);
        let frame = self.backend.frame()?;
        let mut snapshot = Snapshot::new(frame.elements, frame.context);
        snapshot.window = frame.window;
        snapshot.desktop = true;
        if snapshot.elements.is_empty() {
            return Err(Error::Unavailable(NO_CONTROLS.into()));
        }
        self.last = Some(snapshot.clone());
        Ok(snapshot)
    }
    fn activate(
        &mut self,
        expected: &VoiceElement,
        focus: bool,
        authority: &dyn Authority,
    ) -> Result<bool, Error> {
        authority.check()?;
        let old = self.last.as_ref().ok_or(Error::StaleTarget)?;
        if !old.elements.contains(expected) || !expected.visible || !expected.enabled {
            return Err(Error::StaleTarget);
        }
        let context = old.context.clone();
        let fresh = self.snapshot()?;
        if fresh.context != context || !fresh.elements.contains(expected) {
            return Err(Error::StaleTarget);
        }
        if let Err(error) = self.backend.resolve(expected, &context) {
            self.last = None;
            return Err(error);
        }
        let result = self.backend.perform(&expected.id, focus, authority);
        if result.is_err() {
            self.last = None;
        }
        result
    }
}
#[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
enum Request {
    Snapshot(tokio::sync::oneshot::Sender<Result<Snapshot, Error>>),
    Activate(
        VoiceElement,
        bool,
        Arc<dyn Authority>,
        tokio::sync::oneshot::Sender<Result<bool, Error>>,
    ),
}
/// Serial native AX adapter. Construction does not query the desktop or prompt.
#[derive(Debug)]
pub struct Ax {
    sender: Option<mpsc::SyncSender<Request>>,
}
impl Default for Ax {
    fn default() -> Self {
        Self::new()
    }
}
impl Ax {
    pub fn new() -> Self {
        #[cfg(target_os = "macos")]
        {
            Self::start(|| native::Native::new().map(|b| Box::new(b) as Box<dyn Backend>))
        }
        #[cfg(not(target_os = "macos"))]
        {
            Self { sender: None }
        }
    }
    #[cfg(any(target_os = "macos", test))]
    fn start(factory: impl FnOnce() -> Result<Box<dyn Backend>, Error> + Send + 'static) -> Self {
        // Bounded queue: do not accumulate observations/actions behind a slow app.
        let (sender, receiver) = mpsc::sync_channel(8);
        let started = std::thread::Builder::new()
            .name("mac-ax".into())
            .spawn(move || {
                let mut engine = factory().map(|backend| Engine {
                    backend,
                    last: None,
                });
                while let Ok(request) = receiver.recv() {
                    match request {
                        Request::Snapshot(reply) => {
                            // A cancelled caller must not perform an unnecessary read.
                            if reply.is_closed() {
                                continue;
                            }
                            let result = match &mut engine {
                                Ok(engine) => engine.snapshot(),
                                Err(_) => Err(Error::Unavailable(
                                    "Mac accessibility could not initialize".into(),
                                )),
                            };
                            let _ = reply.send(result);
                        }
                        Request::Activate(element, focus, authority, reply) => {
                            if reply.is_closed() {
                                continue;
                            }
                            let result = match &mut engine {
                                Ok(engine) => engine.activate(&element, focus, authority.as_ref()),
                                Err(_) => Err(Error::Unavailable(
                                    "Mac accessibility could not initialize".into(),
                                )),
                            };
                            let _ = reply.send(result);
                        }
                    }
                }
            })
            .is_ok();
        Self {
            sender: started.then_some(sender),
        }
    }
    fn send(&self, request: Request) -> Result<(), Error> {
        self.sender
            .as_ref()
            .ok_or_else(|| Error::Unavailable(UNSUPPORTED.into()))?
            .try_send(request)
            .map_err(|_| Error::Unavailable("Mac accessibility is busy; try again".into()))
    }
}
#[async_trait]
impl Accessibility for Ax {
    async fn snapshot(&self) -> Result<Snapshot, Error> {
        let (reply, result) = tokio::sync::oneshot::channel();
        self.send(Request::Snapshot(reply))?;
        result.await.map_err(|_| Error::Stopped)?
    }
    async fn activate(&self, _: &VoiceElement, _: bool) -> Result<bool, Error> {
        // Never send native input without an execution-time authority object.
        // Coordinate fallback, if any, remains the caller's fenced responsibility.
        Ok(false)
    }
    async fn activate_authorized(
        &self,
        element: &VoiceElement,
        focus: bool,
        authority: Arc<dyn Authority>,
    ) -> Result<bool, Error> {
        let (reply, result) = tokio::sync::oneshot::channel();
        self.send(Request::Activate(element.clone(), focus, authority, reply))?;
        result.await.map_err(|_| Error::OutcomeUnknown)?
    }
}

#[cfg(any(target_os = "macos", test))]
fn private_label(label: &str) -> bool {
    let label = label.to_lowercase();
    [
        "password",
        "passcode",
        "secret",
        "token",
        "card",
        "cvv",
        "cvc",
        "credential",
        "authorization",
        "ssn",
        "otp",
        "seed",
        "recovery",
        "one-time code",
        "private key",
    ]
    .iter()
    .any(|word| label.contains(word))
}
#[cfg(any(target_os = "macos", test))]
fn destructive_label(label: &str) -> bool {
    label
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .any(|word| {
            matches!(
                word,
                "delete"
                    | "erase"
                    | "remove"
                    | "destroy"
                    | "discard"
                    | "uninstall"
                    | "send"
                    | "submit"
                    | "purchase"
                    | "buy"
                    | "pay"
            )
        })
}
