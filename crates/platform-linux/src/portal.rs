// SPDX-License-Identifier: Apache-2.0
//! Portal consent and PipeWire capture. The caller supplies the D-Bus connection.
use crate::native::{Driver, Event, NativeCapture, NativeInput, Output, failure};
use ashpd::desktop::{
    PersistMode, Session,
    remote_desktop::{DeviceType, KeyState, RemoteDesktop, SelectDevicesOptions},
    screencast::{CursorMode, Screencast, SelectSourcesOptions, SourceType},
};
use async_trait::async_trait;
use ports::desktop::{CapturedFrame, PlatformError};
use std::{
    os::fd::OwnedFd,
    sync::{Arc, Mutex},
};

/// Tokens belong to the companion data directory. Implementations must not log values.
#[async_trait]
pub trait TokenStore: Send + Sync + 'static {
    async fn load(&self) -> Result<Option<String>, PlatformError>;
    async fn save(&self, token: &str) -> Result<(), PlatformError>;
}
#[derive(Clone)]
pub struct PortalDriver {
    state: Arc<Mutex<Arc<PortalSession>>>,
    connection: zbus::Connection,
    store: Arc<dyn TokenStore>,
    reopening: Arc<tokio::sync::Mutex<()>>,
    description: Option<crate::wayland::WaylandDriver>,
    monitor_names: Arc<Mutex<std::collections::BTreeMap<String, String>>>,
    pointer_space: PointerSpace,
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
enum PointerSpace {
    #[default]
    Logical,
    StreamPixels,
}
impl PointerSpace {
    fn for_desktop(desktop: &str) -> Self {
        if desktop
            .split(':')
            .any(|name| name.eq_ignore_ascii_case("gnome"))
        {
            Self::StreamPixels
        } else {
            Self::Logical
        }
    }
}
impl std::fmt::Debug for PortalDriver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PortalDriver").finish_non_exhaustive()
    }
}
struct PortalSession {
    remote: RemoteDesktop,
    cast: Screencast,
    session: Arc<Session<RemoteDesktop>>,
    closed: Arc<std::sync::atomic::AtomicBool>,
    close_acknowledged: Arc<std::sync::atomic::AtomicBool>,
    closed_watcher: tokio::task::JoinHandle<()>,
    outputs: Mutex<Vec<(u32, Output)>>,
    native_geometry: std::sync::atomic::AtomicBool,
    clipboard: Option<crate::clipboard::PortalClipboard>,
    source_removed: Arc<std::sync::atomic::AtomicBool>,
}
impl PortalDriver {
    /// Mutter's legacy Notify input uses stream pixels, unlike KDE's logical
    /// coordinates. Keep this choice explicit in the supplied session context.
    pub fn with_desktop_name(mut self, desktop: &str) -> Self {
        self.pointer_space = PointerSpace::for_desktop(desktop);
        self
    }
    fn session(&self) -> Arc<PortalSession> {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
    pub fn with_wayland(mut self, description: crate::wayland::WaylandDriver) -> Self {
        self.session()
            .native_geometry
            .store(true, std::sync::atomic::Ordering::SeqCst);
        self.description = Some(description);
        self
    }
    /// This is the explicit consent operation; detection and construction of other backends never invoke it.
    pub async fn request_consent(
        connection: zbus::Connection,
        store: Arc<dyn TokenStore>,
    ) -> Result<Self, PlatformError> {
        let remote = RemoteDesktop::with_connection(connection.clone())
            .await
            .map_err(portal_error)?;
        let clipboard = ashpd::desktop::clipboard::Clipboard::with_connection(connection.clone())
            .await
            .ok()
            .map(Arc::new);
        let cast = Screencast::with_connection(connection.clone())
            .await
            .map_err(portal_error)?;
        let pending = Pending {
            session: Arc::new(
                remote
                    .create_session(Default::default())
                    .await
                    .map_err(portal_error)?,
            ),
            settled: false,
        };
        let session = &*pending.session;
        let clipboard = match clipboard {
            Some(clipboard) if clipboard.request(session, Default::default()).await.is_ok() => {
                Some(clipboard)
            }
            _ => None,
        };
        let result=async {
            let token=store.load().await.map_err(|_|PlatformError::Native("could not load desktop consent token".into()))?;
            remote.select_devices(session,SelectDevicesOptions::default().set_devices(DeviceType::Keyboard|DeviceType::Pointer).set_persist_mode(PersistMode::ExplicitlyRevoked).set_restore_token(token.as_deref()))
                .await.map_err(portal_error)?.response().map_err(portal_error)?;
            cast.select_sources(session,SelectSourcesOptions::default().set_sources(Some(SourceType::Monitor.into())).set_multiple(true).set_cursor_mode(CursorMode::Embedded))
                .await.map_err(portal_error)?.response().map_err(portal_error)?;
            let selected=remote.start(session,None,Default::default()).await.map_err(portal_error)?.response().map_err(portal_error)?;
            if !selected.devices().contains(DeviceType::Keyboard|DeviceType::Pointer) {return Err(PlatformError::PermissionMissing("Screen sharing needs keyboard and pointer permission. Approve both in the desktop dialog.".into()));}
            let mut outputs=Vec::new();let mut next_x=0.0;
            for stream in selected.streams(){
                let (width,height)=stream.size().ok_or_else(||PlatformError::Unsupported("portal did not provide monitor geometry".into()))?;
                if width<=0||height<=0{return Err(PlatformError::Native("invalid portal monitor geometry".into()));}
                let (x,y)=stream.position().map(|(x,y)|(f64::from(x),f64::from(y))).unwrap_or((next_x,0.0));next_x=x+f64::from(width);
                outputs.push((stream.pipe_wire_node_id(),Output{name:stream.id().map(str::to_owned).unwrap_or_else(||format!("portal-{}",stream.pipe_wire_node_id())),x,y,width:width as u32,height:height as u32,scale:1.0,rotation:0}));
            }
            if outputs.is_empty(){return Err(PlatformError::PermissionMissing("No monitor was selected in the desktop sharing dialog.".into()));}
            if let Some(token)=selected.restore_token(){store.save(token).await.map_err(|_|PlatformError::Native("could not save desktop consent token".into()))?;}
            Ok((outputs, selected.is_clipboard_enabled()))
        }.await;
        match result {
            Ok((outputs, clipboard_enabled)) => {
                let clipboard = match clipboard.filter(|_| clipboard_enabled) {
                    Some(clipboard) => {
                        crate::clipboard::PortalClipboard::start(clipboard, pending.session.clone())
                            .await
                            .ok()
                    }
                    None => None,
                };
                // Nothing below waits, so the session is handed straight on.
                let session = pending.keep();
                let closed = Arc::new(std::sync::atomic::AtomicBool::new(false));
                let close_acknowledged = Arc::new(std::sync::atomic::AtomicBool::new(false));
                let acknowledged_flag = close_acknowledged.clone();
                let closed_session = session.clone();
                let closed_flag = closed.clone();
                let closed_watcher = tokio::spawn(async move {
                    use futures_util::StreamExt;
                    if let Ok(mut events) = closed_session.receive_closed().await
                        && events.next().await.is_some()
                    {
                        acknowledged_flag.store(true, std::sync::atomic::Ordering::SeqCst);
                    }
                    closed_flag.store(true, std::sync::atomic::Ordering::Relaxed);
                });
                Ok(Self {
                    connection,
                    store,
                    reopening: Default::default(),
                    description: None,
                    monitor_names: Default::default(),
                    pointer_space: PointerSpace::default(),
                    state: Arc::new(Mutex::new(Arc::new(PortalSession {
                        remote,
                        cast,
                        session,
                        closed,
                        close_acknowledged,
                        closed_watcher,
                        outputs: Mutex::new(outputs),
                        clipboard,
                        native_geometry: std::sync::atomic::AtomicBool::new(false),
                        source_removed: Default::default(),
                    }))),
                })
            }
            Err(error) => {
                pending.close().await;
                Err(error)
            }
        }
    }
    pub async fn close(&self) -> Result<(), PlatformError> {
        let session = self.session();
        close_once(&session.close_acknowledged, &session.closed, async {
            session.session.close().await.map_err(portal_error)
        })
        .await
    }
}

/// A session the desktop has created but consent has not yet handed on.
/// Given up at any wait, even after approval, it is closed rather than left
/// running with nothing to close it.
struct Pending {
    session: Arc<Session<RemoteDesktop>>,
    settled: bool,
}
impl Pending {
    fn keep(mut self) -> Arc<Session<RemoteDesktop>> {
        self.settled = true;
        self.session.clone()
    }
    async fn close(mut self) {
        let _ = self.session.close().await;
        self.settled = true;
    }
}
impl Drop for Pending {
    fn drop(&mut self) {
        if !self.settled
            && let Ok(runtime) = tokio::runtime::Handle::try_current()
        {
            let session = self.session.clone();
            runtime.spawn(async move {
                let _ = session.close().await;
            });
        }
    }
}

async fn close_once(
    acknowledged: &std::sync::atomic::AtomicBool,
    closed: &std::sync::atomic::AtomicBool,
    close: impl std::future::Future<Output = Result<(), PlatformError>>,
) -> Result<(), PlatformError> {
    use std::sync::atomic::Ordering::SeqCst;
    // A received Closed signal or a previous successful Close is already a
    // native fence. The portal may have removed that session's D-Bus object.
    if !acknowledged.load(SeqCst) {
        close.await?;
        acknowledged.store(true, SeqCst);
    }
    closed.store(true, SeqCst);
    Ok(())
}
#[async_trait]
impl Driver for PortalDriver {
    fn input_availability(&self) -> Option<protocol::capability::Availability> {
        self.session()
            .source_removed
            .load(std::sync::atomic::Ordering::SeqCst)
            .then(
                || protocol::capability::Availability::TemporarilyUnavailable {
                    reason: "Display changed, reopen to continue".into(),
                },
            )
    }
    async fn prepare_watch(&self) -> Result<(), PlatformError> {
        let _reopening = self.reopening.lock().await;
        if !self
            .session()
            .source_removed
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            return Ok(());
        }
        self.close().await?;
        // No portal option guarantees a dialog-free restore. A new explicit
        // watch may use saved consent; the portal decides whether to prompt.
        let mut new = Self::request_consent(self.connection.clone(), self.store.clone()).await?;
        if let Some(description) = &self.description {
            new = new.with_wayland(description.clone());
        }
        *self.state.lock().map_err(failure)? = new.session();
        self.monitor_names.lock().map_err(failure)?.clear();
        Ok(())
    }
    fn availability(&self) -> Option<protocol::capability::Availability> {
        if self
            .session()
            .source_removed
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            return Some(protocol::capability::Availability::TemporarilyUnavailable {
                reason: "Display changed, reopen to continue".into(),
            });
        }
        Some(
            if self
                .session()
                .closed
                .load(std::sync::atomic::Ordering::Relaxed)
            {
                protocol::capability::Availability::Revoked
            } else {
                protocol::capability::Availability::Available
            },
        )
    }
    async fn outputs(&self) -> Result<Vec<Output>, PlatformError> {
        let session = self.session();
        if let Some(description) = &self.description {
            let current = description.outputs().await?;
            let mut names = self.monitor_names.lock().map_err(failure)?;
            let mut outputs = session.outputs.lock().map_err(failure)?;
            for (_, output) in outputs.iter_mut() {
                let native = if let Some(name) = names.get(&output.name) {
                    current.iter().find(|candidate| &candidate.name == name)
                } else {
                    let mut matches = current.iter().filter(|candidate| {
                        current.len() == 1
                            || candidate.name == output.name
                            || (candidate.x == output.x && candidate.y == output.y)
                    });
                    let first = matches.next();
                    if matches.next().is_none() {
                        first
                    } else {
                        None
                    }
                }
                .ok_or_else(|| {
                    PlatformError::TemporarilyUnavailable(
                        "Display changed; reopen desktop sharing to continue".into(),
                    )
                })?;
                names.insert(output.name.clone(), native.name.clone());
                let identity = output.name.clone();
                *output = native.clone();
                output.name = identity;
            }
        }
        Ok(self
            .session()
            .outputs
            .lock()
            .map_err(failure)?
            .iter()
            .map(|(_, o)| o.clone())
            .collect())
    }
    async fn capture(&self, output: &Output) -> Result<CapturedFrame, PlatformError> {
        let node = self
            .session()
            .outputs
            .lock()
            .map_err(failure)?
            .iter()
            .find(|(_, o)| o.name == output.name)
            .map(|(id, _)| *id)
            .ok_or(PlatformError::TargetNotFound)?;
        let fd = self
            .session()
            .cast
            .open_pipe_wire_remote(&self.session().session, Default::default())
            .await
            .map_err(portal_error)?;
        let expected = output.clone();
        let frame = tokio::task::spawn_blocking(move || pipewire_capture(fd, node, &expected))
            .await
            .map_err(failure)??;
        if !self
            .session()
            .native_geometry
            .load(std::sync::atomic::Ordering::SeqCst)
            && let Some((_, o)) = self
                .session()
                .outputs
                .lock()
                .map_err(failure)?
                .iter_mut()
                .find(|(id, _)| *id == node)
        {
            let logical_width = o.logical_width();
            o.width = frame.width;
            o.height = frame.height;
            o.scale = (f64::from(frame.width) / logical_width) as f32;
        }
        Ok(frame)
    }
    async fn stream(
        &self,
        output: &Output,
    ) -> Result<Option<Box<dyn NativeCapture>>, PlatformError> {
        let node = self
            .session()
            .outputs
            .lock()
            .map_err(failure)?
            .iter()
            .find(|(_, o)| o.name == output.name)
            .map(|(node, _)| *node)
            .ok_or(PlatformError::TargetNotFound)?;
        let fd = self
            .session()
            .cast
            .open_pipe_wire_remote(&self.session().session, Default::default())
            .await
            .map_err(portal_error)?;
        let (send, receive) = tokio::sync::watch::channel(None);
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let producer_stop = stop.clone();
        let output = output.clone();
        let session = self.session();
        let removed = session.source_removed.clone();
        std::thread::Builder::new()
            .name("desktop-capture".into())
            .spawn(move || {
                let _ = pipewire_run(fd, node, &output, Some((send, producer_stop, session)));
            })
            .map_err(failure)?;
        Ok(Some(Box::new(PipeCapture {
            receive,
            stop,
            sequence: 0,
            removed,
        })))
    }
    async fn input(&self) -> Result<Box<dyn NativeInput>, PlatformError> {
        Ok(Box::new(PortalInput {
            session: self.session().clone(),
            uncertain: false,
            description: self.description.clone(),
            pointer_space: self.pointer_space,
        }))
    }
}
struct PortalInput {
    session: Arc<PortalSession>,
    uncertain: bool,
    description: Option<crate::wayland::WaylandDriver>,
    pointer_space: PointerSpace,
}
#[async_trait]
impl NativeInput for PortalInput {
    async fn validate_text(&mut self, text: &str) -> Result<(), PlatformError> {
        let symbols = if let Some(description) = &self.description {
            description.text_keysyms().await.unwrap_or_default()
        } else {
            Default::default()
        };
        crate::text::validate(text, &symbols, self.session.clipboard.is_some())
    }
    async fn set_clipboard(
        &mut self,
        text: &str,
        fence: crate::native::InputFence,
    ) -> Result<(), PlatformError> {
        fence.check(false)?;
        let clipboard = self.session.clipboard.as_ref().ok_or_else(|| {
            PlatformError::Unsupported("Paste is not available in this desktop session".into())
        })?;
        clipboard.set(&self.session.session, text).await
    }
    async fn barrier(&mut self) -> Result<(), PlatformError> {
        // ManagedInput retains and joins each D-Bus call through cancellation.
        // Successful method replies settle our synchronous Notify calls. A lost
        // reply provides no native fence: do not assume another Notify fixes it.
        if self
            .session
            .close_acknowledged
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            self.uncertain = false;
            return Ok(());
        }
        if self.uncertain
            || self
                .session
                .closed
                .load(std::sync::atomic::Ordering::SeqCst)
        {
            // Close revokes the remote-desktop session, including its held
            // input. Only the acknowledged close can recover a lost Notify.
            self.session.session.close().await.map_err(portal_error)?;
            self.session
                .close_acknowledged
                .store(true, std::sync::atomic::Ordering::SeqCst);
            self.session
                .closed
                .store(true, std::sync::atomic::Ordering::SeqCst);
            self.uncertain = false;
        }
        Ok(())
    }
    async fn event(&mut self, event: Event) -> Result<(), PlatformError> {
        let state = &self.session;
        if state
            .source_removed
            .load(std::sync::atomic::Ordering::SeqCst)
            && !event.is_release()
        {
            return Err(PlatformError::TemporarilyUnavailable(
                "Display changed, reopen to continue".into(),
            ));
        }
        if state.closed.load(std::sync::atomic::Ordering::SeqCst) {
            return if matches!(
                event,
                Event::Key { pressed: false, .. } | Event::Button { pressed: false, .. }
            ) {
                if state
                    .close_acknowledged
                    .load(std::sync::atomic::Ordering::SeqCst)
                {
                    Ok(()) // Acknowledged session close already released these.
                } else {
                    Err(PlatformError::OutcomeUnknown(
                        "Portal disconnected without confirming input release".into(),
                    ))
                }
            } else {
                Err(PlatformError::PermissionMissing(
                    "Desktop sharing ended; approve a new session on the computer".into(),
                ))
            };
        }
        let result = match event {
            Event::Motion { x, y } => {
                let (node, lx, ly) = {
                    let outputs = state.outputs.lock().map_err(failure)?;
                    mapped_portal_point(&outputs, x, y, self.pointer_space)?
                };
                state
                    .remote
                    .notify_pointer_motion_absolute(
                        &state.session,
                        node,
                        lx,
                        ly,
                        Default::default(),
                    )
                    .await
            }
            Event::Button { code, pressed } => {
                state
                    .remote
                    .notify_pointer_button(
                        &state.session,
                        code as i32,
                        key_state(pressed),
                        Default::default(),
                    )
                    .await
            }
            Event::Key { keysym, pressed } => {
                state
                    .remote
                    .notify_keyboard_keysym(
                        &state.session,
                        keysym as i32,
                        key_state(pressed),
                        Default::default(),
                    )
                    .await
            }
            Event::Axis { dx, dy } => {
                state
                    .remote
                    .notify_pointer_axis(&state.session, dx, dy, Default::default())
                    .await
            }
        }
        .map_err(portal_input_error);
        if matches!(result, Err(PlatformError::OutcomeUnknown(_))) {
            self.uncertain = true;
        }
        result
    }
}
fn key_state(pressed: bool) -> KeyState {
    if pressed {
        KeyState::Pressed
    } else {
        KeyState::Released
    }
}
pub fn portal_point(
    outputs: &[(u32, Output)],
    x: f64,
    y: f64,
) -> Result<(u32, f64, f64), PlatformError> {
    mapped_portal_point(outputs, x, y, PointerSpace::Logical)
}
fn mapped_portal_point(
    outputs: &[(u32, Output)],
    x: f64,
    y: f64,
    space: PointerSpace,
) -> Result<(u32, f64, f64), PlatformError> {
    let (node, o) = outputs
        .iter()
        .find(|(_, o)| {
            x >= o.x && y >= o.y && x < o.x + o.logical_width() && y < o.y + o.logical_height()
        })
        .ok_or(PlatformError::TargetNotFound)?;
    let scale = match space {
        PointerSpace::Logical => 1.0,
        PointerSpace::StreamPixels => f64::from(o.scale),
    };
    Ok((*node, (x - o.x) * scale, (y - o.y) * scale))
}
pub(crate) fn portal_input_error(error: ashpd::Error) -> PlatformError {
    match error {
        ashpd::Error::Response(_)
        | ashpd::Error::Portal(
            ashpd::PortalError::InvalidArgument(_)
            | ashpd::PortalError::NotFound(_)
            | ashpd::PortalError::Exist(_)
            | ashpd::PortalError::NotAllowed(_)
            | ashpd::PortalError::Cancelled(_),
        )
        | ashpd::Error::PortalNotFound(_)
        | ashpd::Error::RequiresVersion(_, _) => portal_error(error),
        _ => crate::native::uncertain(error),
    }
}

pub(crate) fn portal_error(error: ashpd::Error) -> PlatformError {
    match error{
    ashpd::Error::Response(_)|ashpd::Error::Portal(ashpd::PortalError::NotAllowed(_)|ashpd::PortalError::Cancelled(_))=>PlatformError::PermissionMissing("Desktop sharing was denied or cancelled. Approve screen sharing and remote input on the computer.".into()),
    ashpd::Error::PortalNotFound(_)|ashpd::Error::RequiresVersion(_,_)=>PlatformError::Unsupported("The installed desktop portal does not support this operation.".into()),
    _=>PlatformError::TemporarilyUnavailable("The desktop portal is unavailable or its session has closed.".into()),
}
}

struct PipeCapture {
    sequence: u64,
    receive: tokio::sync::watch::Receiver<Option<(u64, Arc<CapturedFrame>)>>,
    stop: Arc<std::sync::atomic::AtomicBool>,
    removed: Arc<std::sync::atomic::AtomicBool>,
}
#[async_trait]
impl NativeCapture for PipeCapture {
    async fn next_frame(&mut self) -> Result<CapturedFrame, PlatformError> {
        loop {
            self.receive.changed().await.map_err(|_| {
                if self.removed.load(std::sync::atomic::Ordering::SeqCst) {
                    PlatformError::Native("Display changed, reopen to continue".into())
                } else {
                    PlatformError::TemporarilyUnavailable("PipeWire capture ended".into())
                }
            })?;
            if let Some((sequence, frame)) = self.receive.borrow_and_update().as_ref() {
                let mut frame = frame.as_ref().clone();
                if self.sequence == 0 || *sequence != self.sequence.wrapping_add(1) {
                    frame.damage = None;
                }
                self.sequence = *sequence;
                return Ok(frame);
            }
        }
    }
}
impl Drop for PipeCapture {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}
type PipeOutput = (
    tokio::sync::watch::Sender<Option<(u64, Arc<CapturedFrame>)>>,
    Arc<std::sync::atomic::AtomicBool>,
    Arc<PortalSession>,
);
// Callbacks may run synchronously during native stream cleanup. A Cell moves
// the result out without keeping a runtime borrow alive across that cleanup.
#[derive(Default)]
struct CaptureMailbox(std::cell::Cell<Option<Result<CapturedFrame, PlatformError>>>);
impl CaptureMailbox {
    fn take(&self) -> Option<Result<CapturedFrame, PlatformError>> {
        self.0.take()
    }
    fn fail(&self, error: PlatformError) {
        self.0.set(Some(Err(error)));
    }
    fn frame(&self, mut frame: CapturedFrame) -> bool {
        let previous = self.0.take();
        if previous.as_ref().is_some_and(Result::is_err) {
            self.0.set(previous);
            return false;
        }
        if previous.is_some() {
            frame.damage = None;
        }
        self.0.set(Some(Ok(frame)));
        true
    }
}
fn pipewire_capture(
    fd: OwnedFd,
    node: u32,
    output: &Output,
) -> Result<CapturedFrame, PlatformError> {
    pipewire_run(fd, node, output, None)
}
fn pipewire_run(
    fd: OwnedFd,
    node: u32,
    output: &Output,
    sink: Option<PipeOutput>,
) -> Result<CapturedFrame, PlatformError> {
    use pipewire::{self as pw, properties::properties, spa};
    use std::{
        rc::Rc,
        time::{Duration, Instant},
    };
    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None).map_err(failure)?;
    let context = pw::context::ContextRc::new(&mainloop, None).map_err(failure)?;
    let core = context.connect_fd_rc(fd, None).map_err(failure)?;
    let stream=pw::stream::StreamBox::new(&core,"desktop capture",properties!{*pw::keys::MEDIA_TYPE=>"Video",*pw::keys::MEDIA_CATEGORY=>"Capture",*pw::keys::MEDIA_ROLE=>"Screen"}).map_err(failure)?;
    let result = Rc::new(CaptureMailbox::default());
    let process = result.clone();
    let error = result.clone();
    let core_error = result.clone();
    let source_seen = Rc::new(std::cell::Cell::new(false));
    let catalog_ready = Rc::new(std::cell::Cell::new(false));
    let sync_sequence = Rc::new(std::cell::Cell::new(None));
    let sync_done = sync_sequence.clone();
    let ready = catalog_ready.clone();
    let _core_listener = core
        .add_listener_local()
        .done(move |_, sequence| {
            if sync_done.get() == Some(sequence.seq()) {
                ready.set(true);
            }
        })
        .error(move |_, _, _, _| {
            core_error.fail(PlatformError::TemporarilyUnavailable(
                "PipeWire connection failed".into(),
            ));
        })
        .register();
    let registry = core.get_registry().map_err(failure)?;
    let removal = result.clone();
    let removed = sink
        .as_ref()
        .map(|(_, _, session)| session.source_removed.clone());
    let missing = removed.clone();
    let seen = source_seen.clone();
    let _registry_listener = registry
        .add_listener_local()
        .global(move |global| {
            if global.id == node {
                seen.set(true);
            }
        })
        .global_remove(move |id| {
            if id == node {
                if let Some(removed) = &removed {
                    removed.store(true, std::sync::atomic::Ordering::SeqCst);
                }
                removal.fail(PlatformError::Native(
                    "Display changed, reopen to continue".into(),
                ));
            }
        })
        .register();
    sync_sequence.set(Some(core.sync(0).map_err(failure)?.seq()));
    let output = output.clone();
    let _listener = stream
        .add_local_listener_with_user_data(spa::param::video::VideoInfoRaw::default())
        .state_changed(move |_, _, _, state| {
            // Paused is normal while the compositor renegotiates an output.
            // Keep observing the registry: only source removal proves that a
            // new portal stream is needed, and may follow this state event.
            if matches!(state, pw::stream::StreamState::Error(_)) {
                error.fail(PlatformError::TemporarilyUnavailable(
                    "PipeWire capture failed".into(),
                ));
            }
        })
        .param_changed(|stream, format, id, pod| {
            if id == spa::param::ParamType::Format.as_raw()
                && let Some(pod) = pod
                && format.parse(pod).is_ok()
            {
                request_damage_metadata(stream);
            }
        })
        .process(move |stream, format| {
            while let Some(frame) = crate::pipewire_buffer::copy(stream, format, &output) {
                if !process.frame(frame) {
                    return;
                }
            }
        })
        .register()
        .map_err(failure)?;
    let object = spa::pod::object!(
        spa::utils::SpaTypes::ObjectParamFormat,
        spa::param::ParamType::EnumFormat,
        spa::pod::property!(
            spa::param::format::FormatProperties::MediaType,
            Id,
            spa::param::format::MediaType::Video
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::MediaSubtype,
            Id,
            spa::param::format::MediaSubtype::Raw
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::VideoFormat,
            Choice,
            Enum,
            Id,
            spa::param::video::VideoFormat::BGRx,
            spa::param::video::VideoFormat::BGRx,
            spa::param::video::VideoFormat::BGRA,
            spa::param::video::VideoFormat::RGBA,
            spa::param::video::VideoFormat::RGBx
        )
    );
    let bytes = spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(object),
    )
    .map_err(failure)?
    .0
    .into_inner();
    let pod = spa::pod::Pod::from_bytes(&bytes)
        .ok_or_else(|| PlatformError::Native("invalid PipeWire format descriptor".into()))?;
    stream
        .connect(
            spa::utils::Direction::Input,
            Some(node),
            pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS,
            &mut [pod],
        )
        .map_err(failure)?;
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut sequence = 0u64;
    loop {
        // A watch opened after a mode switch will never receive the removal
        // event for the old node. The initial registry round trip proves its
        // absence without treating a slow first frame as a geometry change.
        if catalog_ready.get() && !source_seen.get() {
            if let Some(missing) = &missing {
                missing.store(true, std::sync::atomic::Ordering::SeqCst);
            }
            return Err(PlatformError::TemporarilyUnavailable(
                "Display changed, reopen to continue".into(),
            ));
        }
        if let Some(result) = result.take() {
            let frame = result?;
            if let Some((send, _, session)) = &sink {
                if !session
                    .native_geometry
                    .load(std::sync::atomic::Ordering::SeqCst)
                    && let Some((_, output)) = session
                        .outputs
                        .lock()
                        .map_err(failure)?
                        .iter_mut()
                        .find(|(id, _)| *id == node)
                {
                    let logical = output.logical_width();
                    output.width = frame.width;
                    output.height = frame.height;
                    output.scale = (f64::from(frame.width) / logical) as f32;
                }
                sequence = sequence.wrapping_add(1);
                send.send_replace(Some((sequence, Arc::new(frame))));
            } else {
                return Ok(frame);
            }
        }
        if sink.as_ref().is_some_and(|(send, stop, _)| {
            send.is_closed() || stop.load(std::sync::atomic::Ordering::Relaxed)
        }) {
            return Err(PlatformError::TemporarilyUnavailable(
                "capture stopped".into(),
            ));
        }
        if sequence == 0 && Instant::now() >= deadline {
            return Err(PlatformError::TemporarilyUnavailable(
                "PipeWire did not deliver a frame".into(),
            ));
        }
        mainloop.loop_().iterate(Duration::from_millis(50));
    }
}

/// Atomic owner-only token persistence under an explicitly supplied data directory.
#[derive(Clone)]
pub struct FileTokenStore {
    directory: std::path::PathBuf,
}
impl std::fmt::Debug for FileTokenStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("FileTokenStore { .. }")
    }
}
impl FileTokenStore {
    pub fn new(directory: std::path::PathBuf) -> Self {
        Self { directory }
    }
}
#[async_trait]
impl TokenStore for FileTokenStore {
    async fn load(&self) -> Result<Option<String>, PlatformError> {
        let directory = self.directory.clone();
        tokio::task::spawn_blocking(move || {
            use std::io::Read;
            use std::os::unix::fs::OpenOptionsExt;
            let file = std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
                .open(directory.join("desktop-restore-token"));
            let file = match file {
                Ok(file) => file,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(_) => {
                    return Err(PlatformError::Native(
                        "could not open desktop consent token".into(),
                    ));
                }
            };
            use std::os::unix::fs::PermissionsExt;
            let metadata = file.metadata().map_err(failure)?;
            if !metadata.is_file() || metadata.permissions().mode() & 0o077 != 0 {
                return Err(PlatformError::PermissionMissing(
                    "Desktop consent token must be an owner-only regular file.".into(),
                ));
            }
            let mut text = String::new();
            file.take(4097).read_to_string(&mut text).map_err(|_| {
                PlatformError::Native("could not read desktop consent token".into())
            })?;
            if text.len() > 4096 {
                return Err(PlatformError::Native(
                    "desktop consent token exceeds size limit".into(),
                ));
            }
            Ok(Some(text))
        })
        .await
        .map_err(failure)?
    }
    async fn save(&self, token: &str) -> Result<(), PlatformError> {
        if token.len() > 4096 {
            return Err(PlatformError::Native(
                "desktop consent token exceeds size limit".into(),
            ));
        }
        // Written in place rather than on a blocking worker: a cancelled
        // enable drops this future, and a worker it left running could put
        // the token back after turning sharing off had forgotten it. The
        // file is a few bytes, written once per grant.
        use std::io::Write;
        let directory = &self.directory;
        let mut file = tempfile::NamedTempFile::new_in(directory).map_err(failure)?;
        file.write_all(token.as_bytes())
            .map_err(|_| PlatformError::Native("could not write desktop consent token".into()))?;
        file.as_file().sync_all().map_err(failure)?;
        file.persist(directory.join("desktop-restore-token"))
            .map_err(|_| PlatformError::Native("could not persist desktop consent token".into()))?;
        std::fs::File::open(directory)
            .and_then(|dir| dir.sync_all())
            .map_err(failure)?;
        Ok(())
    }
}

fn request_damage_metadata(stream: &pipewire::stream::Stream) {
    use pipewire::spa;
    let object = spa::pod::Object {
        type_: spa::utils::SpaTypes::ObjectParamMeta.as_raw(),
        id: spa::param::ParamType::Meta.as_raw(),
        properties: vec![
            spa::pod::Property::new(
                spa::sys::SPA_PARAM_META_type,
                spa::pod::Value::Id(spa::utils::Id(spa::sys::SPA_META_VideoDamage)),
            ),
            spa::pod::Property::new(
                spa::sys::SPA_PARAM_META_size,
                spa::pod::Value::Int(
                    (16 * std::mem::size_of::<spa::sys::spa_meta_region>()) as i32,
                ),
            ),
        ],
    };
    if let Ok((bytes, _)) = spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(object),
    ) {
        let bytes = bytes.into_inner();
        if let Some(pod) = spa::pod::Pod::from_bytes(&bytes) {
            let _ = stream.update_params(&mut [pod]);
        }
    }
}

impl Drop for PortalSession {
    fn drop(&mut self) {
        self.closed_watcher.abort();
        if !self.closed.load(std::sync::atomic::Ordering::Relaxed)
            && let Ok(runtime) = tokio::runtime::Handle::try_current()
        {
            let session = self.session.clone();
            runtime.spawn(async move {
                let _ = session.close().await;
            });
        }
    }
}

#[cfg(test)]
mod tests {
    fn captured() -> ports::CapturedFrame {
        ports::CapturedFrame {
            width: 1,
            height: 1,
            stride: 4,
            format: ports::desktop::PixelFormat::Bgra8,
            data: vec![0; 4],
            geometry: protocol::control::TargetGeometry {
                width: 1,
                height: 1,
                scale: 1.0,
                rotation_degrees: 0,
                revision: 1,
            },
            cursor: None,
            damage: Some(vec![]),
        }
    }
    #[test]
    fn capture_cleanup_can_reenter_after_a_frame_or_error_is_taken() {
        struct NativeCleanup<'a>(&'a super::CaptureMailbox);
        impl Drop for NativeCleanup<'_> {
            fn drop(&mut self) {
                self.0
                    .fail(ports::PlatformError::Native("cleanup callback".into()));
            }
        }
        fn finish(mailbox: &super::CaptureMailbox) -> Result<(), ports::PlatformError> {
            let _native = NativeCleanup(mailbox);
            if let Some(result) = mailbox.take() {
                let _frame = result?;
                return Ok(());
            }
            panic!("expected a capture result");
        }
        let mailbox = super::CaptureMailbox::default();
        mailbox.frame(captured());
        assert!(finish(&mailbox).is_ok());
        assert!(finish(&mailbox).is_err());
        assert!(matches!(
            mailbox.take(),
            Some(Err(ports::PlatformError::Native(_)))
        ));
    }
    #[test]
    fn capture_mailbox_retains_errors_and_discards_damage_after_skipped_frames() {
        let mailbox = super::CaptureMailbox::default();
        mailbox.frame(captured());
        assert!(mailbox.take().unwrap().unwrap().damage.is_some());
        mailbox.frame(captured());
        mailbox.frame(captured());
        assert!(mailbox.take().unwrap().unwrap().damage.is_none());
        mailbox.fail(ports::PlatformError::Native("source removed".into()));
        mailbox.frame(captured());
        assert!(matches!(
            mailbox.take(),
            Some(Err(ports::PlatformError::Native(_)))
        ));
    }
    #[tokio::test]
    async fn acknowledged_portal_closure_is_idempotent_but_an_error_is_not_a_fence() {
        use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
        let acknowledged = AtomicBool::new(false);
        let closed = AtomicBool::new(false);
        assert!(
            super::close_once(&acknowledged, &closed, async {
                Err(ports::PlatformError::OutcomeUnknown(
                    "Close reply missing".into(),
                ))
            })
            .await
            .is_err()
        );
        assert!(!acknowledged.load(SeqCst));
        assert!(!closed.load(SeqCst));
        super::close_once(&acknowledged, &closed, async { Ok(()) })
            .await
            .unwrap();
        super::close_once(&acknowledged, &closed, async {
            panic!("Already closed object must not be called again")
        })
        .await
        .unwrap();
        assert!(acknowledged.load(SeqCst));
        assert!(closed.load(SeqCst));
    }
    #[test]
    fn input_refusals_and_lost_native_acknowledgements_remain_distinct() {
        assert!(matches!(
            super::portal_input_error(ashpd::Error::Portal(ashpd::PortalError::NotAllowed(
                "denied".into()
            ))),
            ports::PlatformError::PermissionMissing(_)
        ));
        for error in [
            ashpd::Error::NoResponse,
            ashpd::Error::Portal(ashpd::PortalError::Failed("possibly applied".into())),
        ] {
            assert!(matches!(
                super::portal_input_error(error),
                ports::PlatformError::OutcomeUnknown(_)
            ));
        }
    }

    use super::*;
    #[tokio::test]
    async fn token_store_is_atomic_private_and_redacted() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        let store = FileTokenStore::new(directory.path().into());
        assert!(store.load().await.unwrap().is_none());
        store.save("synthetic-consent-value").await.unwrap();
        assert_eq!(
            store.load().await.unwrap().as_deref(),
            Some("synthetic-consent-value")
        );
        assert_eq!(
            std::fs::metadata(directory.path().join("desktop-restore-token"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert!(!format!("{store:?}").contains("synthetic"));
        store.save("rotated-value").await.unwrap();
        assert_eq!(
            store.load().await.unwrap().as_deref(),
            Some("rotated-value")
        );
    }
    #[test]
    fn an_abandoned_token_save_never_outlives_a_later_forget() {
        use futures_util::FutureExt;
        // One blocking worker, held busy, so any write handed to it waits in
        // line where the test can see it.
        let runtime = tokio::runtime::Builder::new_current_thread()
            .max_blocking_threads(1)
            .build()
            .unwrap();
        runtime.block_on(async {
            let directory = tempfile::tempdir().unwrap();
            let token = directory.path().join("desktop-restore-token");
            let store = FileTokenStore::new(directory.path().into());
            let (busy, held) = std::sync::mpsc::channel();
            let (release, released) = std::sync::mpsc::channel::<()>();
            let blocker = tokio::task::spawn_blocking(move || {
                busy.send(()).unwrap();
                let _ = released.recv();
            });
            held.recv().unwrap();
            // A cancelled enable drops its save wherever it has got to...
            let _ = store.save("synthetic-consent-value").now_or_never();
            // ...and turning sharing off then forgets consent.
            match std::fs::remove_file(&token) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => panic!("{error}"),
            }
            // Anything the save left queued runs before this does.
            release.send(()).unwrap();
            blocker.await.unwrap();
            tokio::task::spawn_blocking(|| ()).await.unwrap();
            assert!(
                !token.exists(),
                "an abandoned save restored forgotten consent"
            );
        });
    }
    #[tokio::test]
    async fn consent_given_up_while_its_dialog_is_open_closes_the_session() {
        use crate::fake_portal::Seen;
        struct NoToken;
        #[async_trait]
        impl TokenStore for NoToken {
            async fn load(&self) -> Result<Option<String>, PlatformError> {
                Ok(None)
            }
            async fn save(&self, _: &str) -> Result<(), PlatformError> {
                Ok(())
            }
        }
        let bus = crate::fake_portal::Bus::start();
        let (seen, mut saw) = tokio::sync::mpsc::unbounded_channel();
        let _portal = crate::fake_portal::serve(&bus, seen).await;
        // Held here too, so only an explicit Close can end the session.
        let client = bus.connect().await;
        tokio::select! {
            _ = PortalDriver::request_consent(client.clone(), Arc::new(NoToken)) => {
                panic!("consent finished without an answer")
            }
            shown = saw.recv() => assert_eq!(shown, Some(Seen::Dialog)),
        }
        let closed = tokio::time::timeout(std::time::Duration::from_secs(5), saw.recv())
            .await
            .expect("a consent given up mid-dialog closes its session");
        assert!(matches!(closed, Some(Seen::Closed(_))), "{closed:?}");
    }
    #[test]
    fn stream_coordinates_use_logical_pixels() {
        let output = Output {
            name: "monitor".into(),
            x: -100.0,
            y: 50.0,
            width: 400,
            height: 200,
            scale: 2.0,
            rotation: 0,
        };
        assert_eq!(
            portal_point(&[(7, output)], 0.0, 100.0).unwrap(),
            (7, 100.0, 50.0)
        );
    }
    #[test]
    fn mutter_stream_coordinates_scale_once_after_removing_the_monitor_origin() {
        let output = Output {
            name: "monitor".into(),
            x: -1024.0,
            y: 100.0,
            width: 1280,
            height: 800,
            scale: 1.25,
            rotation: 0,
        };
        for (desktop, expected) in [("ubuntu:GNOME", (640.0, 400.0)), ("KDE", (512.0, 320.0))] {
            let point = mapped_portal_point(
                &[(7, output.clone())],
                -512.0,
                420.0,
                PointerSpace::for_desktop(desktop),
            )
            .unwrap();
            assert_eq!(point, (7, expected.0, expected.1));
        }
        assert!(
            mapped_portal_point(&[(7, output)], 0.0, 420.0, PointerSpace::StreamPixels).is_err()
        );
    }
}
