// SPDX-License-Identifier: Apache-2.0
use crate::{
    Error, Result,
    identity::Identity,
    wire::{self, Wire},
};
use axum::{
    Extension, Json, Router,
    extract::{DefaultBodyLimit, State, WebSocketUpgrade},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ed25519_dalek::{Signature, VerifyingKey};
use futures_util::StreamExt;
use hyper_util::{
    rt::{TokioIo, TokioTimer},
    service::TowerToHyperService,
};
use protocol::{
    ClientFrame, ErrorCode, ProtocolError, ProtocolVersion, ServerFrame,
    auth::{self, Authenticated, Challenge, ConnectionKind, PairRequest, Principal},
};
use ring::rand::{SecureRandom, SystemRandom};
use services::api::{Caller, Hub};
use std::{
    collections::HashMap,
    hash::Hash,
    net::{IpAddr, SocketAddr},
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{
    net::TcpListener,
    sync::{Semaphore, mpsc, watch},
    task::JoinSet,
};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

/// How long an upload may go without a message from the phone before its
/// connection ends and lets go of the upload. A phone that changes networks
/// mid-upload leaves its old connection open but silent, and until it is
/// let go every reconnect is refused as a second writer. The phone sends its
/// 256 KiB chunks back to back and gives up on one it can't hand to the
/// network within 5 s, so a working upload delivers a chunk at least every
/// few seconds. The phone pings only its control connection, never this
/// one. The phone's next connection continues from the last chunk written.
pub const UPLOAD_SILENCE: Duration = Duration::from_secs(30);
/// Keepalive probes notice a peer that vanished without closing, such as a
/// phone that changed networks, on a connection that is otherwise idle.
const KEEPALIVE: socket2::TcpKeepalive = socket2::TcpKeepalive::new()
    .with_time(Duration::from_secs(30))
    .with_interval(Duration::from_secs(10));

#[derive(Debug, Clone)]
pub struct Limits {
    pub authentication: Duration,
    pub heartbeat: Duration,
    pub write: Duration,
    pub revocation_poll: Duration,
    /// See [`UPLOAD_SILENCE`].
    pub upload_silence: Duration,
    pub event_queue: usize,
    pub connections: usize,
    /// Connections one network address may hold before they authenticate, so
    /// one host cannot take every slot from a paired device.
    pub unauthenticated_per_peer: usize,
    /// Authenticated connections one device may hold. Previews open one per
    /// browser connection, so this is generous.
    pub authenticated_per_device: usize,
    /// How often an alerts connection is pinged. The phone only answers, so
    /// this sets how often its radio wakes for Ophio while the app is closed.
    pub alert_keepalive: Duration,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            authentication: Duration::from_secs(10),
            heartbeat: Duration::from_secs(30),
            write: Duration::from_secs(5),
            revocation_poll: Duration::from_millis(250),
            upload_silence: UPLOAD_SILENCE,
            event_queue: 64,
            connections: 256,
            unauthenticated_per_peer: 32,
            authenticated_per_device: 64,
            alert_keepalive: Duration::from_secs(240),
        }
    }
}

#[derive(Clone)]
pub struct ServerDeps {
    pub hub: Arc<dyn Hub>,
    pub clock: Arc<dyn ports::Clock>,
    pub identity: Arc<Identity>,
    pub addresses: Addresses,
    pub limits: Limits,
    pub owner: Option<Arc<dyn crate::local::OwnerRuntime>>,
}
impl std::fmt::Debug for ServerDeps {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServerDeps")
            .field("identity", &self.identity)
            .field("limits", &self.limits)
            .finish_non_exhaustive()
    }
}
/// The addresses a pairing offer gives, shared by every copy of the
/// [`ServerDeps`]. They change while Øphio runs when Tailscale comes up
/// after it.
#[derive(Debug, Clone, Default)]
pub struct Addresses(Arc<Mutex<Vec<String>>>);
impl Addresses {
    pub fn get(&self) -> Vec<String> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
    pub fn set(&self, addresses: Vec<String>) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = addresses;
    }
}
impl From<Vec<String>> for Addresses {
    fn from(addresses: Vec<String>) -> Self {
        Self(Arc::new(Mutex::new(addresses)))
    }
}
impl ServerDeps {
    pub fn new(hub: Arc<dyn Hub>, identity: Arc<Identity>) -> Result<Self> {
        if hub.host_id() != identity.host_id {
            return Err(Error::Identity);
        }
        Ok(Self {
            hub,
            identity,
            clock: Arc::new(ports::clock::SystemClock),
            addresses: Addresses::default(),
            limits: Limits::default(),
            owner: None,
        })
    }
    pub async fn pairing_offer(&self) -> Result<auth::PairingOffer> {
        let ticket = self.hub.open_pairing(&Caller::local_owner()).await?;
        Ok(auth::PairingOffer {
            host_id: self.hub.host_id(),
            host_name: self.hub.host_name(),
            addresses: self.addresses.get(),
            cert_sha256: self.identity.fingerprint(),
            pairing_secret: ticket.secret,
            expires_at: ticket.expires_at,
            protocol: ProtocolVersion::CURRENT,
        })
    }
}

#[derive(Clone)]
struct App {
    deps: ServerDeps,
    shutdown: CancellationToken,
    sessions: TaskTracker,
    controls: crate::preview::Controls,
    peers: Tally<IpAddr>,
    devices: Tally<protocol::DeviceId>,
}
#[derive(Clone)]
struct Binding {
    exporter: [u8; 32],
    _permit: Arc<tokio::sync::OwnedSemaphorePermit>,
    /// Released once the connection authenticates.
    peer: Arc<Counted<IpAddr>>,
    /// When the connection must have upgraded or finished its requests.
    deadline: Arc<watch::Sender<tokio::time::Instant>>,
    /// The connection's own socket, so a connection can change its keepalive
    /// once it knows what it is for.
    socket: Arc<socket2::Socket>,
} // Never Debug/log the TLS exporter.

/// Waits for a connection's deadline, which a pairing request moves while
/// the owner answers at the computer.
async fn until(mut deadline: watch::Receiver<tokio::time::Instant>) {
    loop {
        let at = *deadline.borrow_and_update();
        tokio::select! {
            () = tokio::time::sleep_until(at) => return,
            changed = deadline.changed() => if changed.is_err() {
                return tokio::time::sleep_until(at).await;
            },
        }
    }
}

/// Open connections counted by peer address or device.
struct Tally<K>(Arc<Mutex<HashMap<K, usize>>>);
impl<K> Clone for Tally<K> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}
impl<K> Default for Tally<K> {
    fn default() -> Self {
        Self(Arc::default())
    }
}
impl<K: Copy + Eq + Hash> Tally<K> {
    fn count(&self, key: K, limit: usize) -> Option<Counted<K>> {
        let mut counts = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let count = counts.entry(key).or_default();
        if *count >= limit {
            return None;
        }
        *count += 1;
        Some(Counted {
            tally: self.clone(),
            key,
            released: AtomicBool::new(false),
        })
    }
}
/// One counted connection, released when dropped or explicitly, only once.
struct Counted<K: Copy + Eq + Hash> {
    tally: Tally<K>,
    key: K,
    released: AtomicBool,
}
impl<K: Copy + Eq + Hash> Counted<K> {
    fn release(&self) {
        if self.released.swap(true, Ordering::SeqCst) {
            return;
        }
        let mut counts = self.tally.0.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(count) = counts.get_mut(&self.key) {
            *count -= 1;
            if *count == 0 {
                counts.remove(&self.key);
            }
        }
    }
}
impl<K: Copy + Eq + Hash> Drop for Counted<K> {
    fn drop(&mut self) {
        self.release();
    }
}

/// Listens at each address there is on this computer. A local network
/// address the computer no longer has, as when a laptop leaves home, is
/// passed over: phones reach it the other ways.
async fn listen(addresses: &[SocketAddr]) -> Result<Vec<TcpListener>> {
    let mut listeners = Vec::new();
    for address in addresses {
        match TcpListener::bind(address).await {
            Ok(listener) => listeners.push(listener),
            Err(error)
                if error.kind() == std::io::ErrorKind::AddrNotAvailable
                    && !address.ip().is_loopback() =>
            {
                tracing::warn!(%address, "listen address is not on this computer; not listening there");
            }
            Err(error) => return Err(error.into()),
        }
    }
    if listeners.is_empty() {
        return Err(Error::Config(
            "no listen address is on this computer".into(),
        ));
    }
    Ok(listeners)
}

#[derive(Debug)]
pub struct Server {
    deps: ServerDeps,
    listeners: Vec<TcpListener>,
    later: Option<mpsc::UnboundedReceiver<TcpListener>>,
}
impl Server {
    pub async fn bind(deps: ServerDeps, addresses: &[SocketAddr]) -> Result<Self> {
        if addresses.is_empty()
            || deps.limits.connections == 0
            || deps.limits.unauthenticated_per_peer == 0
            || deps.limits.authenticated_per_device == 0
            || deps.limits.event_queue == 0
            || deps.limits.authentication.is_zero()
            || deps.limits.heartbeat.is_zero()
            || deps.limits.write.is_zero()
            || deps.limits.revocation_poll.is_zero()
            || deps.limits.upload_silence.is_zero()
            || deps.limits.alert_keepalive.is_zero()
        {
            return Err(Error::Config(
                "listeners and queue limits must be nonzero".into(),
            ));
        }
        let listeners = listen(addresses).await?;
        Ok(Self {
            deps,
            listeners,
            later: None,
        })
    }
    /// Takes listeners to serve as well once running, for addresses that
    /// appear afterwards.
    pub fn later(&mut self) -> mpsc::UnboundedSender<TcpListener> {
        let (sender, receiver) = mpsc::unbounded_channel();
        self.later = Some(receiver);
        sender
    }
    pub fn local_addresses(&self) -> Result<Vec<SocketAddr>> {
        self.listeners
            .iter()
            .map(|l| l.local_addr().map_err(Error::from))
            .collect()
    }
    pub async fn run(self, shutdown: CancellationToken) -> Result<()> {
        let Self {
            deps,
            listeners,
            mut later,
        } = self;
        let acceptor = tokio_rustls::TlsAcceptor::from(deps.identity.tls_config()?);
        let permits = Arc::new(Semaphore::new(deps.limits.connections));
        let mut tasks = JoinSet::new();
        let sessions = TaskTracker::new();
        let controls = crate::preview::Controls::default();
        let (peers, devices) = (Tally::default(), Tally::default());
        let serve = |listener: TcpListener, tasks: &mut JoinSet<Result<()>>| {
            let app = App {
                deps: deps.clone(),
                shutdown: shutdown.clone(),
                sessions: sessions.clone(),
                controls: controls.clone(),
                peers: peers.clone(),
                devices: devices.clone(),
            };
            let acceptor = acceptor.clone();
            let permits = permits.clone();
            tasks.spawn(async move {
                let mut connections = JoinSet::new();
                let result = loop {
                    tokio::select! {
                        biased;
                        _ = app.shutdown.cancelled() => break Ok(()),
                        Some(_) = connections.join_next(), if !connections.is_empty() => {},
                        accepted = accept_with_retry(|| listener.accept(), &app.shutdown) => {
                            let (tcp, peer) = match accepted {
                                Ok(Some(stream)) => stream,
                                Ok(None) => break Ok(()),
                                Err(error) => { app.shutdown.cancel(); break Err(Error::Io(error)); }
                            };
                            let Some(peer) = app.peers.count(peer.ip().to_canonical(), app.deps.limits.unauthenticated_per_peer) else { continue; };
                            let Ok(permit) = permits.clone().try_acquire_owned() else { continue; };
                            if tcp.set_nodelay(true).is_err() || socket2::SockRef::from(&tcp).set_tcp_keepalive(&KEEPALIVE).is_err() { continue; }
                            let Ok(socket) = socket2::SockRef::from(&tcp).try_clone() else { continue; };
                            let acceptor = acceptor.clone(); let app = app.clone();
                            connections.spawn(async move {
                                let permit = Arc::new(permit);
                                // One deadline bounds everything before a WebSocket upgrade: the TLS
                                // handshake and every HTTP request, including a body that never arrives.
                                // Only a pairing request holding the window's secret moves it.
                                let start = tokio::time::Instant::now() + app.deps.limits.authentication;
                                let (deadline, until_deadline) = watch::channel(start);
                                let tls = match tokio::time::timeout_at(start, acceptor.accept(tcp)).await { Ok(Ok(tls)) => tls, _ => return };
                                let Ok(exporter) = tls.get_ref().1.export_keying_material([0u8; 32], auth::TLS_EXPORTER_LABEL, None) else { return; };
                                let router = router(app.clone()).layer(Extension(Binding { exporter, _permit: permit, peer: Arc::new(peer), deadline: Arc::new(deadline), socket: Arc::new(socket) }));
                                let mut http = hyper::server::conn::http1::Builder::new();
                                http.timer(TokioTimer::new()).header_read_timeout(Duration::from_secs(10)).max_buf_size(32 * 1024);
                                let connection = http.serve_connection(TokioIo::new(tls), TowerToHyperService::new(router)).with_upgrades();
                                // An upgrade completes this future and hands the stream to the WebSocket task.
                                tokio::select! { _ = app.shutdown.cancelled() => {}, _ = until(until_deadline) => {}, _ = connection => {} }
                            });
                        }
                    }
                };
                connections.abort_all();
                while connections.join_next().await.is_some() {}
                result
            });
        };
        for listener in listeners {
            serve(listener, &mut tasks);
        }
        let mut result = Ok(());
        loop {
            tokio::select! {
                listener = async {
                    match &mut later {
                        Some(later) => later.recv().await,
                        None => std::future::pending().await,
                    }
                } => match listener {
                    Some(listener) => serve(listener, &mut tasks),
                    None => later = None,
                },
                task = tasks.join_next() => match task {
                    None => break,
                    Some(Ok(Ok(()))) => {}
                    Some(Ok(Err(e))) => {
                        result = Err(e);
                        shutdown.cancel();
                    }
                    Some(Err(_)) => {
                        result = Err(Error::Closed);
                        shutdown.cancel();
                    }
                },
            }
        }
        sessions.close();
        sessions.wait().await;
        result
    }
}
fn router(app: App) -> Router {
    Router::new()
        .route("/v1/info", get(info))
        .route("/v1/pair", post(pair))
        .route(
            "/v1/control",
            get(|ws, state, binding, headers| {
                upgrade(ws, state, binding, headers, ConnectionKind::Control)
            }),
        )
        .route(
            "/v1/media",
            get(|ws, state, binding, headers| {
                upgrade(ws, state, binding, headers, ConnectionKind::Media)
            }),
        )
        .route(
            "/v1/terminal",
            get(|ws, state, binding, headers| {
                upgrade(ws, state, binding, headers, ConnectionKind::Terminal)
            }),
        )
        .route(
            "/v1/transfer",
            get(|ws, state, binding, headers| {
                upgrade(ws, state, binding, headers, ConnectionKind::Transfer)
            }),
        )
        .route(
            "/v1/speech",
            get(|ws, state, binding, headers| {
                upgrade(ws, state, binding, headers, ConnectionKind::Speech)
            }),
        )
        .route(
            "/v1/preview",
            get(|ws, state, binding, headers| {
                upgrade(ws, state, binding, headers, ConnectionKind::Preview)
            }),
        )
        .route(
            "/v1/alerts",
            get(|ws, state, binding, headers| {
                upgrade(ws, state, binding, headers, ConnectionKind::Alerts)
            }),
        )
        .layer(DefaultBodyLimit::max(16 * 1024))
        .with_state(app)
}
async fn info(State(app): State<App>) -> Json<serde_json::Value> {
    Json(
        serde_json::json!({"host_name": app.deps.hub.host_name(), "protocol": ProtocolVersion::CURRENT, "cert_sha256": app.deps.identity.fingerprint()}),
    )
}
/// Holds a pairing request until the local owner answers it at the computer,
/// or its window closes. The phone keeps the request open while it waits, and
/// closing it withdraws the request. Phones from before the owner confirmed
/// pairing wait the same way; they give up sooner.
async fn pair(
    State(app): State<App>,
    Extension(binding): Extension<Binding>,
    headers: HeaderMap,
    request: std::result::Result<Json<PairRequest>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let result = async {
        if headers.contains_key("origin") {
            return Err(ProtocolError::new(
                ErrorCode::PermissionDenied,
                "native client required",
            ));
        }
        let Json(request) = request.map_err(|_| {
            ProtocolError::new(ErrorCode::InvalidRequest, "invalid pairing request")
        })?;
        if !ProtocolVersion::CURRENT.compatible_with(&request.protocol) {
            return Err(ProtocolError::new(
                ErrorCode::IncompatibleVersion,
                "unsupported protocol major version",
            )
            .with_detail(serde_json::json!({"supported": [ProtocolVersion::CURRENT]})));
        }
        decode::<32>(&request.pairing_secret).map_err(|e| e.protocol())?;
        let public = decode::<32>(&request.device_public_key).map_err(|e| e.protocol())?;
        let public_key = VerifyingKey::from_bytes(&public)
            .map_err(|_| ProtocolError::new(ErrorCode::InvalidRequest, "invalid device key"))?;
        if public_key.is_weak() {
            return Err(ProtocolError::new(
                ErrorCode::InvalidRequest,
                "invalid device key",
            ));
        }
        if request.device_name.is_empty()
            || request.device_name.len() > 256
            || request.device_platform.len() > 64
            || request.client_version.len() > 64
        {
            return Err(ProtocolError::new(
                ErrorCode::InvalidRequest,
                "invalid device metadata",
            ));
        }
        let code = auth::pairing_code(
            &app.deps.identity.fingerprint(),
            &request.pairing_secret,
            &request.device_public_key,
        );
        let mut claim = tokio::time::timeout(
            app.deps.limits.authentication,
            app.deps.hub.request_pairing(request, code),
        )
        .await
        .map_err(|_| ProtocolError::new(ErrorCode::Expired, "pairing request timed out"))??;
        let open = Duration::from_millis(
            u64::try_from(claim.expires_at.0.saturating_sub(app.deps.clock.now().0)).unwrap_or(0),
        );
        binding
            .deadline
            .send_replace(tokio::time::Instant::now() + open + app.deps.limits.write);
        tokio::select! {
            answer = &mut claim.answer => answer.unwrap_or_else(|_| Err(ProtocolError::new(
                ErrorCode::TemporarilyUnavailable,
                "The computer stopped before this request was answered",
            ))),
            () = wire::deadline(app.deps.clock.clone(), app.deps.clock.monotonic() + open) => Err(ProtocolError::new(
                ErrorCode::Expired,
                "The pairing code expired before it was confirmed on the computer",
            )),
        }
    }
    .await;
    match result {
        Ok(response) => Json(response).into_response(),
        Err(error) => {
            let code = match error.code {
                ErrorCode::IncompatibleVersion => StatusCode::UPGRADE_REQUIRED,
                ErrorCode::Unauthenticated | ErrorCode::Expired | ErrorCode::AlreadyResolved => {
                    StatusCode::UNAUTHORIZED
                }
                ErrorCode::PermissionDenied => StatusCode::FORBIDDEN,
                ErrorCode::Conflict => StatusCode::CONFLICT,
                _ => StatusCode::BAD_REQUEST,
            };
            (code, Json(error)).into_response()
        }
    }
}
async fn upgrade(
    ws: WebSocketUpgrade,
    State(app): State<App>,
    Extension(binding): Extension<Binding>,
    headers: HeaderMap,
    kind: ConnectionKind,
) -> Response {
    if headers.contains_key("origin") {
        return StatusCode::FORBIDDEN.into_response();
    }
    let session_token = app.sessions.token();
    // A control connection's queued event frames leave in one write when it
    // flushes; other channels write each message through as it is sent.
    let write_buffer = match kind {
        ConnectionKind::Control => wire::CONTROL_WRITE_BUFFER,
        _ => 0,
    };
    ws.max_message_size(wire::MAX_JSON_BYTES)
        .max_frame_size(wire::MAX_JSON_BYTES)
        .write_buffer_size(write_buffer)
        .max_write_buffer_size(16 * 1024 * 1024)
        .on_upgrade(move |socket| async move {
            let _session_token = session_token;
            connection(Wire::websocket(socket), app, binding, kind).await;
        })
}
async fn connection(mut wire: Wire, app: App, binding: Binding, kind: ConnectionKind) {
    let end = app.deps.clock.monotonic() + app.deps.limits.authentication;
    let authenticated = tokio::select! {
        _ = app.shutdown.cancelled() => return,
        _ = wire::deadline(app.deps.clock.clone(), end) => Err(Error::Timeout),
        result = authenticate(&mut wire, &app.deps, &binding.exporter, kind, &app.controls, &app.devices) => result,
    };
    let (caller, control, _device) = match authenticated {
        Ok(caller) => caller,
        Err(error) => {
            fatal(&mut wire.writer, &error, &app.deps).await;
            return;
        }
    };
    binding.peer.release();
    let result = match kind {
        ConnectionKind::Control => {
            let result = crate::control::run(
                &mut wire,
                &app.deps,
                caller.clone(),
                &app.shutdown,
                false,
                false,
            )
            .await;
            drop(control);
            app.deps.hub.control_disconnected(&caller).await;
            result
        }
        ConnectionKind::Preview => {
            crate::preview::run(&mut wire, &app.deps, &caller, &app.shutdown, &app.controls).await
        }
        ConnectionKind::Alerts => {
            // The computer's pings keep this connection alive; TCP's own
            // probes would wake the phone every 30 s as well.
            let _ = binding.socket.set_keepalive(false);
            crate::alerts::run(&mut wire, &app.deps, &caller, &app.shutdown).await
        }
        _ => crate::side::run(&mut wire, &app.deps, &caller, &app.shutdown, kind).await,
    };
    if let Err(error) = result {
        fatal(&mut wire.writer, &error, &app.deps).await;
    } else {
        let _ = wire::send(
            &mut wire.writer,
            axum::extract::ws::Message::Close(None),
            app.deps.limits.write,
        )
        .await;
    }
}
pub(crate) async fn fatal(writer: &mut wire::Writer, error: &Error, deps: &ServerDeps) {
    let _ = wire::send_json(
        writer,
        &ServerFrame::Fatal(error.protocol()),
        deps.limits.write,
    )
    .await;
    let _ = wire::send(
        writer,
        axum::extract::ws::Message::Close(None),
        deps.limits.write,
    )
    .await;
}
pub(crate) async fn current_caller(deps: &ServerDeps, caller: &Caller) -> Result<Caller> {
    match caller.principal {
        Principal::Device { device_id } => {
            let record = deps
                .hub
                .device_credential(&device_id)
                .await
                .ok_or_else(unauthenticated)?;
            if record.revoked || record.device_id != device_id {
                return Err(unauthenticated());
            }
            Ok(Caller {
                principal: caller.principal,
                grants: record.grants,
            })
        }
        Principal::LocalOwner => Ok(Caller::local_owner()),
        _ => Err(unauthenticated()),
    }
}
fn unauthenticated() -> Error {
    ProtocolError::new(ErrorCode::Unauthenticated, "device authentication failed").into()
}
fn decode<const N: usize>(text: &str) -> Result<[u8; N]> {
    if text.len() > N * 2 {
        return Err(unauthenticated());
    }
    URL_SAFE_NO_PAD
        .decode(text)
        .map_err(|_| unauthenticated())?
        .try_into()
        .map_err(|_| unauthenticated())
}
fn negotiate(supported: &[ProtocolVersion], server: ProtocolVersion) -> Option<ProtocolVersion> {
    supported
        .iter()
        .filter(|p| p.major == server.major)
        .map(|p| ProtocolVersion {
            major: p.major,
            minor: p.minor.min(server.minor),
        })
        .max()
}

async fn authenticate(
    wire: &mut Wire,
    deps: &ServerDeps,
    exporter: &[u8; 32],
    kind: ConnectionKind,
    controls: &crate::preview::Controls,
    devices: &Tally<protocol::DeviceId>,
) -> Result<(
    Caller,
    Option<crate::preview::Control>,
    Counted<protocol::DeviceId>,
)> {
    let ClientFrame::Hello(hello) = sign_in_frame(&mut wire.reader).await? else {
        return Err(unauthenticated());
    };
    if hello.channel != kind || hello.supported.len() > 16 || hello.client_version.len() > 64 {
        return Err(unauthenticated());
    }
    let selected = negotiate(&hello.supported, ProtocolVersion::CURRENT).ok_or_else(|| {
        ProtocolError::new(
            ErrorCode::IncompatibleVersion,
            "unsupported protocol major version",
        )
        .with_detail(serde_json::json!({"supported": [ProtocolVersion::CURRENT]}))
    })?;
    let client_nonce = decode::<32>(&hello.client_nonce)?;
    let credential = deps
        .hub
        .device_credential(&hello.device_id)
        .await
        .ok_or_else(unauthenticated)?;
    if credential.revoked || credential.device_id != hello.device_id {
        return Err(unauthenticated());
    }
    let mut nonce = [0u8; 32];
    SystemRandom::new()
        .fill(&mut nonce)
        .map_err(|_| Error::Tls)?;
    wire::send_json(
        &mut wire.writer,
        &ServerFrame::Challenge(Challenge {
            selected,
            host_id: deps.hub.host_id(),
            boot_epoch: deps.hub.boot_epoch(),
            server_nonce: URL_SAFE_NO_PAD.encode(nonce),
        }),
        deps.limits.write,
    )
    .await?;
    let ClientFrame::Authenticate(proof) = sign_in_frame(&mut wire.reader).await? else {
        return Err(unauthenticated());
    };
    let transcript = auth::auth_transcript(
        &deps.hub.host_id(),
        &hello.device_id,
        kind,
        &client_nonce,
        &nonce,
        exporter,
    );
    let key = VerifyingKey::from_bytes(&credential.public_key).map_err(|_| unauthenticated())?;
    key.verify_strict(
        &transcript,
        &Signature::from_bytes(&decode::<64>(&proof.signature)?),
    )
    .map_err(|_| unauthenticated())?;
    let caller = current_caller(
        deps,
        &Caller {
            principal: Principal::Device {
                device_id: hello.device_id,
            },
            grants: credential.grants,
        },
    )
    .await?;
    let device = devices
        .count(hello.device_id, deps.limits.authenticated_per_device)
        .ok_or_else(|| {
            ProtocolError::new(
                ErrorCode::LimitExceeded,
                "This device has reached its limit of open connections. It can connect again when one closes.",
            )
        })?;
    let control = if kind == ConnectionKind::Control {
        Some(controls.register(&caller)?)
    } else {
        None
    };
    wire::send_json(
        &mut wire.writer,
        &ServerFrame::Authenticated(Authenticated {
            host_id: deps.hub.host_id(),
            device_id: hello.device_id,
            grants: caller.grants.clone(),
            capabilities: deps.hub.capabilities().await,
            companion_version: env!("CARGO_PKG_VERSION").into(),
            journal_head: deps.hub.journal_head(),
            control_connection_id: control.as_ref().map(|c| c.id.clone()),
        }),
        deps.limits.write,
    )
    .await?;
    deps.hub.device_seen(&hello.device_id).await;
    Ok((caller, control, device))
}

/// Largest frame read before a connection authenticates. `Hello` and
/// `Authenticate` are 210 and 124 bytes from the app, and the largest the
/// server accepts is about 1.1 KiB. After authentication the limit is
/// [`wire::MAX_JSON_BYTES`].
const MAX_SIGN_IN_BYTES: usize = 4 * 1024;

/// The next sign-in frame, refused by its length before any of it is parsed.
/// The WebSocket's own limit cannot change after the upgrade, so it stays at
/// the authenticated size and this check comes first.
async fn sign_in_frame(reader: &mut wire::Reader) -> Result<ClientFrame> {
    let message = reader.next().await.ok_or(Error::Closed)??;
    if let axum::extract::ws::Message::Text(text) = &message
        && text.len() > MAX_SIGN_IN_BYTES
    {
        return Err(
            ProtocolError::new(ErrorCode::LimitExceeded, "sign-in frame is too large").into(),
        );
    }
    wire::parse(message)
}

/// Retry resource pressure and aborted connections without discarding the listener.
/// Only invalid/closed listener errors are fatal. Diagnostics never include peers.
pub(crate) async fn accept_with_retry<T, F, Fut>(
    mut accept: F,
    shutdown: &CancellationToken,
) -> std::io::Result<Option<T>>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = std::io::Result<T>>,
{
    let mut delay = Duration::from_millis(50);
    loop {
        let result = tokio::select! { biased; _ = shutdown.cancelled() => return Ok(None), result = accept() => result };
        match result {
            Ok(stream) => return Ok(Some(stream)),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::InvalidInput | std::io::ErrorKind::NotConnected
                ) || matches!(error.raw_os_error(), Some(9 | 88 | 10038)) =>
            {
                return Err(error);
            }
            Err(error) => {
                tracing::warn!(kind = ?error.kind(), retry_ms = delay.as_millis(), "listener accept failed; retrying");
                tokio::select! { biased; _ = shutdown.cancelled() => return Ok(None), _ = tokio::time::sleep(delay) => {} }
                delay = (delay * 2).min(Duration::from_secs(1));
            }
        }
    }
}
#[cfg(test)]
mod accept_tests {
    use super::*;
    #[test]
    fn the_largest_sign_in_frames_fit_well_within_their_limit() {
        use protocol::auth::{Authenticate, Hello};
        // Every field at the most the server accepts: sixteen versions, a
        // 64-byte client version of characters JSON must escape, the longest
        // nonce and signature text `decode` reads.
        let hello = ClientFrame::Hello(Hello {
            supported: vec![
                ProtocolVersion {
                    major: u16::MAX,
                    minor: u16::MAX
                };
                16
            ],
            device_id: protocol::DeviceId::new(),
            client_nonce: "n".repeat(64),
            client_version: "\u{1}".repeat(64),
            channel: ConnectionKind::Preview,
        });
        let proof = ClientFrame::Authenticate(Authenticate {
            signature: "s".repeat(128),
        });
        for frame in [hello, proof] {
            let size = serde_json::to_string(&frame).unwrap().len();
            assert!(size * 3 < MAX_SIGN_IN_BYTES, "{size} bytes");
        }
    }
    #[tokio::test(start_paused = true)]
    async fn transient_accept_errors_keep_listener_alive() {
        let mut errors = [24, 23, 103, 104].into_iter();
        let result = accept_with_retry(
            || {
                std::future::ready(
                    errors
                        .next()
                        .map_or(Ok(7), |code| Err(std::io::Error::from_raw_os_error(code))),
                )
            },
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(result, Some(7));
    }
    #[tokio::test(start_paused = true)]
    async fn fatal_accept_error_and_cancelled_backoff_terminate() {
        let stop = CancellationToken::new();
        assert!(
            accept_with_retry::<(), _, _>(
                || std::future::ready(Err(std::io::Error::from(std::io::ErrorKind::InvalidInput))),
                &stop
            )
            .await
            .is_err()
        );
        let stopping = stop.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(75)).await;
            stopping.cancel();
        });
        assert!(
            accept_with_retry::<(), _, _>(
                || std::future::ready(Err(std::io::Error::from_raw_os_error(24))),
                &stop
            )
            .await
            .unwrap()
            .is_none()
        );
    }

    #[tokio::test]
    async fn an_address_the_computer_lacks_is_passed_over() {
        // 192.0.2.1 is for documentation, so no computer has it.
        let gone = "192.0.2.1:0".parse().unwrap();
        let here = "127.0.0.1:0".parse().unwrap();
        let listeners = listen(&[gone, here]).await.unwrap();
        assert_eq!(listeners.len(), 1);
        assert!(listeners[0].local_addr().unwrap().ip().is_loopback());
        assert!(matches!(
            listen(&[gone]).await,
            Err(Error::Config(message)) if message.contains("no listen address")
        ));
    }
}
