// SPDX-License-Identifier: Apache-2.0
#![allow(dead_code)]
use async_trait::async_trait;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use companion::{
    identity::Identity,
    server::{Limits, Server, ServerDeps},
};
use ed25519_dalek::{Signer, SigningKey};
use futures_util::{SinkExt, StreamExt};
use ports::Clock;
use protocol::{
    auth::*,
    capability::*,
    control::{InputAck, InputEnvelope, InputOutcome},
    event::{EventEnvelope, EventSource},
    ids::*,
    media::*,
    time::Timestamp,
    *,
};
use services::api::*;
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::{broadcast, mpsc},
};
use tokio_tungstenite::{WebSocketStream, tungstenite::Message};

/// The task whose reusable draft is large but fits in a control frame.
pub const FITS: &str = "00000000-0000-0000-0000-000000000001";

#[derive(Debug)]
pub struct FakeClock {
    start: Instant,
    millis: AtomicU64,
}
impl Default for FakeClock {
    fn default() -> Self {
        Self {
            start: Instant::now(),
            millis: AtomicU64::new(0),
        }
    }
}
impl FakeClock {
    pub fn advance(&self, duration: Duration) {
        self.millis
            .fetch_add(duration.as_millis() as u64, Ordering::SeqCst);
    }
}
impl Clock for FakeClock {
    fn now(&self) -> Timestamp {
        Timestamp(1_700_000_000_000 + self.millis.load(Ordering::SeqCst) as i64)
    }
    fn monotonic(&self) -> Instant {
        self.start + Duration::from_millis(self.millis.load(Ordering::SeqCst))
    }
}

pub fn unavailable() -> ProtocolError {
    ProtocolError::new(ErrorCode::Unsupported, "test capability not configured")
}
pub fn capabilities() -> HostCapabilities {
    let a = Availability::Available;
    HostCapabilities {
        conversation_access: None,
        os: HostOs::Linux,
        desktop_backend: "test".into(),
        capture: a.clone(),
        input: a.clone(),
        input_scope: InputScope::None,
        terminal: a.clone(),
        lock_session: a.clone(),
        speech: a.clone(),
        library: a.clone(),
        transfers: a.clone(),
        computer_use: a.clone(),
        telegram: a.clone(),
        discord: a,
        features: vec![],
        window_actions: None,
        unlock_session: None,
        app_updates: None,
    }
}
pub fn event(seq: u64) -> Arc<EventEnvelope> {
    Arc::new(EventEnvelope {
        seq,
        at: Timestamp(0),
        source: EventSource::Companion,
        provenance: protocol::model::Provenance::Observed,
        event: Event::HarnessesChanged { harnesses: vec![] },
    })
}

pub struct WaitingPairing {
    pub prompt: PairingPrompt,
    pub key: [u8; 32],
    pub answer: tokio::sync::oneshot::Sender<Result<PairResponse, ProtocolError>>,
}
pub struct FakeHub {
    pub host: HostId,
    pub boot: BootEpoch,
    pub device: DeviceId,
    pub key: SigningKey,
    pub revoked: AtomicBool,
    pub clock: Arc<FakeClock>,
    pub seen: AtomicUsize,
    pub disconnected: AtomicUsize,
    pub callers: Mutex<Vec<Caller>>,
    pub credentials: Mutex<HashMap<DeviceId, DeviceCredential>>,
    pub ticket: Mutex<Option<(String, Timestamp)>>,
    pub waiting: Mutex<Option<WaitingPairing>>,
    pub head: AtomicU64,
    pub history: Mutex<Vec<Arc<EventEnvelope>>>,
    pub live: broadcast::Sender<Outbound>,
    pub subscriptions: AtomicUsize,
    pub backlog_pages: AtomicUsize,
    pub media: Mutex<Option<MediaSession>>,
    pub terminal: Mutex<Option<TerminalAttachment>>,
    pub speech: Mutex<Option<SpeechStream>>,
    pub uploaded: Arc<Mutex<Vec<u8>>>,
    /// Held while an upload connection is attached, as the hub allows one.
    pub attached: Arc<AtomicBool>,
    pub download_bytes: Mutex<Vec<u8>>,
    pub cancelled: AtomicBool,
    pub preview: Mutex<Option<tokio::io::DuplexStream>>,
    pub preview_opens: AtomicUsize,
    /// Every alert the hub knows, open or not; `open_alerts` lists the open ones.
    pub alerts: Mutex<Vec<(protocol::alerts::Alert, bool)>>,
    /// Published just before the next open list is read, so their live
    /// copies are still on the way when it is sent.
    pub before_open: Mutex<Vec<Event>>,
    pub slow_request: Mutex<Option<Arc<RequestGate>>>,
    pub commands: Mutex<Vec<CommandEnvelope>>,
}

pub struct RequestGate {
    pub entered: tokio::sync::Semaphore,
    pub release: tokio::sync::Semaphore,
}
impl Default for RequestGate {
    fn default() -> Self {
        Self {
            entered: tokio::sync::Semaphore::new(0),
            release: tokio::sync::Semaphore::new(0),
        }
    }
}
impl RequestGate {
    async fn wait(&self) {
        self.entered.add_permits(1);
        let _ = self.release.acquire().await;
    }
}
impl FakeHub {
    pub fn new(clock: Arc<FakeClock>) -> Self {
        let device = DeviceId::new();
        let key = SigningKey::from_bytes(&[7u8; 32]);
        let mut credentials = HashMap::new();
        credentials.insert(
            device,
            DeviceCredential {
                device_id: device,
                public_key: key.verifying_key().to_bytes(),
                grants: Grants::owner(),
                revoked: false,
            },
        );
        Self {
            host: HostId::new(),
            boot: BootEpoch::new(),
            device,
            key,
            revoked: AtomicBool::new(false),
            clock,
            seen: AtomicUsize::new(0),
            disconnected: AtomicUsize::new(0),
            callers: Mutex::new(vec![]),
            credentials: Mutex::new(credentials),
            ticket: Mutex::new(None),
            waiting: Mutex::new(None),
            head: AtomicU64::new(0),
            history: Mutex::new(vec![]),
            live: broadcast::channel(16).0,
            subscriptions: AtomicUsize::new(0),
            backlog_pages: AtomicUsize::new(0),
            media: Mutex::new(None),
            terminal: Mutex::new(None),
            speech: Mutex::new(None),
            uploaded: Arc::new(Mutex::new(vec![])),
            attached: Arc::new(AtomicBool::new(false)),
            download_bytes: Mutex::new(vec![]),
            cancelled: AtomicBool::new(false),
            preview: Mutex::new(None),
            preview_opens: AtomicUsize::new(0),
            alerts: Mutex::new(vec![]),
            before_open: Mutex::new(vec![]),
            slow_request: Mutex::new(None),
            commands: Mutex::new(vec![]),
        }
    }
    /// Journals `event` as the next seq and sends it live.
    pub fn publish(&self, event: Event) -> u64 {
        let seq = self.head.load(Ordering::SeqCst) + 1;
        let envelope = Arc::new(EventEnvelope {
            seq,
            at: Timestamp(0),
            source: EventSource::Companion,
            provenance: protocol::model::Provenance::Observed,
            event,
        });
        self.history.lock().unwrap().push(envelope.clone());
        self.head.store(seq, Ordering::SeqCst);
        let _ = self.live.send(Outbound::Event(envelope));
        seq
    }
    pub fn emit(&self, seq: u64) {
        let event = event(seq);
        self.history.lock().unwrap().push(event.clone());
        self.head.store(seq, Ordering::SeqCst);
        let _ = self.live.send(Outbound::Event(event));
    }
}
#[async_trait]
impl Hub for FakeHub {
    fn host_id(&self) -> HostId {
        self.host
    }
    fn host_name(&self) -> String {
        "test-host".into()
    }
    fn boot_epoch(&self) -> BootEpoch {
        self.boot
    }
    fn journal_head(&self) -> u64 {
        self.head.load(Ordering::SeqCst)
    }
    async fn capabilities(&self) -> HostCapabilities {
        capabilities()
    }
    async fn open_pairing(&self, caller: &Caller) -> Result<PairingTicket, ProtocolError> {
        assert_eq!(caller.principal, Principal::LocalOwner);
        let secret = URL_SAFE_NO_PAD.encode([9u8; 32]);
        *self.ticket.lock().unwrap() =
            Some((secret.clone(), Timestamp(self.clock.now().0 + 60_000)));
        Ok(PairingTicket {
            secret,
            expires_at: Timestamp(self.clock.now().0 + 60_000),
        })
    }
    async fn request_pairing(
        &self,
        request: PairRequest,
        code: String,
    ) -> Result<PairingClaim, ProtocolError> {
        let ticket = self.ticket.lock().unwrap();
        let (secret, expires) = ticket.as_ref().ok_or_else(|| {
            ProtocolError::new(ErrorCode::Unauthenticated, "pairing window closed")
        })?;
        if *expires <= self.clock.now() {
            return Err(ProtocolError::new(
                ErrorCode::Expired,
                "pairing window expired",
            ));
        }
        if *secret != request.pairing_secret {
            return Err(ProtocolError::new(
                ErrorCode::Unauthenticated,
                "invalid pairing secret",
            ));
        }
        let mut waiting = self.waiting.lock().unwrap();
        if waiting.as_ref().is_some_and(|w| !w.answer.is_closed()) {
            return Err(ProtocolError::new(
                ErrorCode::Conflict,
                "another device is waiting",
            ));
        }
        let (answer, receiver) = tokio::sync::oneshot::channel();
        *waiting = Some(WaitingPairing {
            prompt: PairingPrompt {
                device_name: request.device_name,
                device_platform: request.device_platform,
                code,
                shows_code: request.shows_code,
                replaces: None,
                expires_at: *expires,
            },
            key: URL_SAFE_NO_PAD
                .decode(request.device_public_key)
                .unwrap()
                .try_into()
                .unwrap(),
            answer,
        });
        Ok(PairingClaim {
            expires_at: *expires,
            answer: receiver,
        })
    }
    async fn waiting_pairing(
        &self,
        caller: &Caller,
    ) -> Result<Option<PairingPrompt>, ProtocolError> {
        assert_eq!(caller.principal, Principal::LocalOwner);
        Ok(self
            .waiting
            .lock()
            .unwrap()
            .as_ref()
            .filter(|w| !w.answer.is_closed())
            .map(|w| w.prompt.clone()))
    }
    async fn answer_pairing(
        &self,
        caller: &Caller,
        code: &str,
        confirm: bool,
    ) -> Result<Option<DeviceRecord>, ProtocolError> {
        assert_eq!(caller.principal, Principal::LocalOwner);
        let waiting = self
            .waiting
            .lock()
            .unwrap()
            .take_if(|w| w.prompt.code == code && !w.answer.is_closed())
            .ok_or_else(|| ProtocolError::new(ErrorCode::NotFound, "no device is waiting"))?;
        *self.ticket.lock().unwrap() = None;
        if !confirm {
            let _ = waiting.answer.send(Err(ProtocolError::new(
                ErrorCode::PermissionDenied,
                "refused",
            )));
            return Ok(None);
        }
        let device_id = DeviceId::new();
        self.credentials.lock().unwrap().insert(
            device_id,
            DeviceCredential {
                device_id,
                public_key: waiting.key,
                grants: Grants::owner(),
                revoked: false,
            },
        );
        let _ = waiting.answer.send(Ok(PairResponse {
            device_id,
            host_id: self.host,
            host_name: self.host_name(),
            grants: Grants::owner(),
        }));
        Ok(Some(DeviceRecord {
            device_id,
            name: waiting.prompt.device_name,
            platform: waiting.prompt.device_platform,
            paired_at: self.clock.now(),
            last_seen_at: None,
            revoked_at: None,
            grants: Grants::owner(),
            is_current: false,
        }))
    }
    async fn device_credential(&self, id: &DeviceId) -> Option<DeviceCredential> {
        self.credentials
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .map(|mut record| {
                record.revoked |= self.revoked.load(Ordering::SeqCst);
                record
            })
    }
    async fn device_seen(&self, _: &DeviceId) {
        self.seen.fetch_add(1, Ordering::SeqCst);
    }
    async fn channel_identity(&self, _: ChannelKind, _: &str, _: &str) -> Option<ChannelIdentity> {
        None
    }
    async fn request_channel_link(&self, _: ChannelLinkRequest) -> Result<String, ProtocolError> {
        Err(unavailable())
    }
    async fn channel_link_outcome(&self, _: ChannelKind, _: &str) -> Option<ChannelLinkOutcome> {
        None
    }
    async fn submit(&self, caller: &Caller, command: CommandEnvelope) -> CommandReceipt {
        if matches!(command.body, Command::CreateDownload { .. }) {
            let gate = self.slow_request.lock().unwrap().clone();
            if let Some(gate) = gate {
                gate.wait().await;
            }
        }
        self.callers.lock().unwrap().push(caller.clone());
        self.commands.lock().unwrap().push(command.clone());
        CommandReceipt {
            command_id: command.command_id,
            status: ReceiptStatus::Accepted,
            seq: None,
            result: None,
            error: None,
        }
    }
    async fn query(&self, _: &Caller, query: Query) -> Result<QueryData, ProtocolError> {
        if matches!(query, Query::LibraryRead { .. }) {
            let gate = self.slow_request.lock().unwrap().clone();
            if let Some(gate) = gate {
                gate.wait().await;
            }
        }
        match query {
            Query::Devices => Ok(QueryData::Devices {
                devices: vec![],
                identities: vec![],
            }),
            // A large answer that fits.
            Query::ReusableDraft { task_id } if task_id.to_string() == FITS => {
                Ok(QueryData::ReusableDraft {
                    text: "a".repeat(200 * 1024),
                })
            }
            // An answer larger than one control frame.
            Query::ReusableDraft { .. } => Ok(QueryData::ReusableDraft {
                text: "\"".repeat(600 * 1024),
            }),
            _ => Err(unavailable()),
        }
    }
    async fn subscribe(
        &self,
        _: &Caller,
        after: Option<u64>,
    ) -> Result<Subscription, ProtocolError> {
        let live = self.live.subscribe();
        let history = self.history.lock().unwrap();
        let head = self.journal_head();
        self.subscriptions.fetch_add(1, Ordering::SeqCst);
        let backlog = Backlog::Events(
            history
                .iter()
                .filter(|e| after.is_some_and(|after| e.seq > after) && e.seq <= head)
                .cloned()
                .collect(),
        );
        Ok(Subscription {
            backlog,
            head,
            live,
        })
    }
    async fn open_alerts(
        &self,
        _: &Caller,
    ) -> Result<(Vec<protocol::alerts::Alert>, u64), ProtocolError> {
        for event in std::mem::take(&mut *self.before_open.lock().unwrap()) {
            self.publish(event);
        }
        let alerts = self.alerts.lock().unwrap();
        let open: Vec<_> = alerts.iter().filter(|(_, open)| *open).collect();
        // As the hub lists them: the newest, oldest first.
        let left_out = open.len().saturating_sub(protocol::alerts::MAX_OPEN_ALERTS);
        let listed = open[left_out..].iter().map(|(a, _)| a.clone());
        Ok((listed.collect(), self.journal_head()))
    }
    async fn alert(
        &self,
        _: &Caller,
        attention_id: AttentionId,
    ) -> Result<Option<protocol::alerts::Alert>, ProtocolError> {
        let alerts = self.alerts.lock().unwrap();
        let found = alerts.iter().find(|(a, _)| a.attention_id == attention_id);
        Ok(found.filter(|(_, open)| *open).map(|(a, _)| a.clone()))
    }
    async fn backlog(
        &self,
        _: &Caller,
        after: u64,
        head: u64,
        limit: usize,
    ) -> Result<(Backlog, u64), ProtocolError> {
        self.backlog_pages.fetch_add(1, Ordering::SeqCst);
        let events: Vec<_> = self
            .history
            .lock()
            .unwrap()
            .iter()
            .filter(|e| e.seq > after && e.seq <= head)
            .take(limit)
            .cloned()
            .collect();
        let through = match events.last() {
            Some(last) if events.len() == limit => last.seq,
            _ => head,
        };
        Ok((Backlog::Events(events), through))
    }
    async fn input(&self, _: &Caller, input: InputEnvelope) -> InputAck {
        InputAck {
            refusal: None,
            lease_id: input.lease_id,
            seq: input.seq,
            outcome: InputOutcome::Injected,
        }
    }
    async fn renew_lease(&self, _: &Caller, _: LeaseId, _: u64) -> Result<(), ProtocolError> {
        Ok(())
    }
    async fn control_disconnected(&self, _: &Caller) {
        self.disconnected.fetch_add(1, Ordering::SeqCst);
    }
    async fn open_media(
        &self,
        _: &Caller,
        _: TargetId,
        _: StreamParams,
    ) -> Result<MediaSession, ProtocolError> {
        self.media.lock().unwrap().take().ok_or_else(unavailable)
    }
    async fn attach_terminal(
        &self,
        _: &Caller,
        _: TerminalId,
        _: Option<u64>,
    ) -> Result<TerminalAttachment, ProtocolError> {
        self.terminal.lock().unwrap().take().ok_or_else(unavailable)
    }
    async fn upload(
        &self,
        _: &Caller,
        _: TransferId,
    ) -> Result<Box<dyn UploadSink>, ProtocolError> {
        if self.attached.swap(true, Ordering::SeqCst) {
            return Err(ProtocolError::new(
                ErrorCode::Conflict,
                "Transfer already has an attached writer",
            ));
        }
        Ok(Box::new(TestUpload {
            bytes: self.uploaded.clone(),
            attached: self.attached.clone(),
        }))
    }
    async fn download(
        &self,
        _: &Caller,
        _: TransferId,
        offset: u64,
    ) -> Result<DownloadSource, ProtocolError> {
        let data = self.download_bytes.lock().unwrap().clone();
        if offset > data.len() as u64 {
            return Err(unavailable());
        }
        let size = data.len() as u64;
        let hash = companion::identity::fingerprint(&data);
        Ok(DownloadSource {
            offset,
            size,
            sha256: hash,
            reader: Box::new(std::io::Cursor::new(data[offset as usize..].to_vec())),
        })
    }
    async fn cancel_transfer(&self, _: &Caller, _: TransferId) -> Result<(), ProtocolError> {
        self.cancelled.store(true, Ordering::SeqCst);
        Ok(())
    }
    async fn open_preview(
        &self,
        caller: &Caller,
        _: protocol::PreviewId,
    ) -> Result<tokio::io::DuplexStream, ProtocolError> {
        assert!(caller.grants.tasks);
        self.preview_opens.fetch_add(1, Ordering::SeqCst);
        self.preview.lock().unwrap().take().ok_or_else(unavailable)
    }
    async fn start_speech(
        &self,
        _: &Caller,
        _: String,
        _: Option<String>,
        _: bool,
    ) -> Result<SpeechStream, ProtocolError> {
        self.speech.lock().unwrap().take().ok_or_else(unavailable)
    }
}
struct TestUpload {
    bytes: Arc<Mutex<Vec<u8>>>,
    attached: Arc<AtomicBool>,
}
impl Drop for TestUpload {
    fn drop(&mut self) {
        self.attached.store(false, Ordering::SeqCst);
    }
}
#[async_trait]
impl UploadSink for TestUpload {
    fn resume_at(&self) -> u64 {
        self.bytes.lock().unwrap().len() as u64
    }
    async fn write(&mut self, chunk: &[u8]) -> Result<(), ProtocolError> {
        self.bytes.lock().unwrap().extend_from_slice(chunk);
        Ok(())
    }
    async fn finish(self: Box<Self>) -> Result<UploadDone, ProtocolError> {
        Ok(UploadDone {
            sha256: companion::identity::fingerprint(&self.bytes.lock().unwrap()),
            artifact_id: None,
        })
    }
}

pub struct Fixture {
    pub hub: Arc<FakeHub>,
    pub clock: Arc<FakeClock>,
    pub deps: ServerDeps,
    pub address: std::net::SocketAddr,
    pub shutdown: tokio_util::sync::CancellationToken,
    /// Listeners the running server takes on as well.
    pub later: tokio::sync::mpsc::UnboundedSender<tokio::net::TcpListener>,
    task: tokio::task::JoinHandle<companion::Result<()>>,
}
impl Fixture {
    pub async fn new() -> Self {
        Self::with_limits(Limits::default()).await
    }
    pub async fn with_limits(limits: Limits) -> Self {
        let clock = Arc::new(FakeClock::default());
        let hub = Arc::new(FakeHub::new(clock.clone()));
        let identity = Arc::new(Identity::generate(hub.host, vec!["test-host".into()]).unwrap());
        let mut deps = ServerDeps::new(hub.clone(), identity).unwrap();
        deps.clock = clock.clone();
        deps.limits = limits;
        let mut server = Server::bind(deps.clone(), &["127.0.0.1:0".parse().unwrap()])
            .await
            .unwrap();
        let address = server.local_addresses().unwrap()[0];
        let later = server.later();
        let shutdown = tokio_util::sync::CancellationToken::new();
        let stop = shutdown.clone();
        let task = tokio::spawn(server.run(stop));
        Self {
            hub,
            clock,
            deps,
            address,
            shutdown,
            later,
            task,
        }
    }
    pub async fn stop(self) {
        self.shutdown.cancel();
        tokio::time::timeout(Duration::from_secs(10), self.task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
    pub async fn tls(
        &self,
        pin: &str,
    ) -> Result<tokio_rustls::client::TlsStream<tokio::net::TcpStream>, std::io::Error> {
        self.tls_over(pin, tokio::net::TcpStream::connect(self.address).await?)
            .await
    }
    pub async fn tls_over<S: AsyncRead + AsyncWrite + Unpin>(
        &self,
        pin: &str,
        stream: S,
    ) -> Result<tokio_rustls::client::TlsStream<S>, std::io::Error> {
        let config = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(PinVerifier(pin.into())))
        .with_no_client_auth();
        let connector = tokio_rustls::TlsConnector::from(Arc::new(config));
        connector
            .connect(
                rustls::pki_types::ServerName::try_from("localhost").unwrap(),
                stream,
            )
            .await
    }
    pub async fn connect(&self, kind: ConnectionKind) -> Client {
        let tcp = tokio::net::TcpStream::connect(self.address).await.unwrap();
        self.connect_over(kind, tcp).await
    }
    /// `connect` through `stream`, which the caller opened to this server.
    pub async fn connect_over<S: AsyncRead + AsyncWrite + Unpin>(
        &self,
        kind: ConnectionKind,
        stream: S,
    ) -> Client<tokio_rustls::client::TlsStream<S>> {
        let tls = self
            .tls_over(&self.deps.identity.fingerprint(), stream)
            .await
            .unwrap();
        let exporter = tls
            .get_ref()
            .1
            .export_keying_material([0u8; 32], TLS_EXPORTER_LABEL, None)
            .unwrap();
        let channel = match kind {
            ConnectionKind::Control => "control",
            ConnectionKind::Media => "media",
            ConnectionKind::Terminal => "terminal",
            ConnectionKind::Transfer => "transfer",
            ConnectionKind::Speech => "speech",
            ConnectionKind::Preview => "preview",
            ConnectionKind::Alerts => "alerts",
        };
        let (socket, _) =
            tokio_tungstenite::client_async(format!("wss://localhost/v1/{channel}"), tls)
                .await
                .unwrap();
        Client {
            socket,
            exporter,
            kind,
            nonce: [3; 32],
        }
    }
    pub async fn authenticated(&self, kind: ConnectionKind) -> Client {
        let client = self.connect(kind).await;
        self.authenticate(client).await
    }
    pub async fn authenticate<S: AsyncRead + AsyncWrite + Unpin>(
        &self,
        mut client: Client<S>,
    ) -> Client<S> {
        let challenge = client.challenge(self.hub.device).await;
        let auth = client.proof(&challenge, &self.hub.key, self.hub.device);
        client.send(&ClientFrame::Authenticate(auth)).await;
        assert!(matches!(
            client.receive::<ServerFrame>().await,
            ServerFrame::Authenticated(_)
        ));
        client
    }
    pub async fn pair(&self, request: &PairRequest) -> (u16, serde_json::Value) {
        let pending = self.send_pair(request).await;
        pair_response(pending).await
    }
    /// Sends a pairing request as the phone does and leaves it waiting.
    pub async fn send_pair(
        &self,
        request: &PairRequest,
    ) -> tokio_rustls::client::TlsStream<tokio::net::TcpStream> {
        use tokio::io::AsyncWriteExt;
        let mut tls = self.tls(&self.deps.identity.fingerprint()).await.unwrap();
        let body = serde_json::to_string(request).unwrap();
        let http = format!(
            "POST /v1/pair HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        tls.write_all(http.as_bytes()).await.unwrap();
        tls
    }
    /// The request waiting for the owner, once it has arrived.
    pub async fn waiting(&self) -> PairingPrompt {
        let owner = Caller::local_owner();
        for _ in 0..500 {
            if let Some(prompt) = self.hub.waiting_pairing(&owner).await.unwrap() {
                return prompt;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("no pairing request arrived");
    }
}
pub async fn pair_response(
    mut tls: tokio_rustls::client::TlsStream<tokio::net::TcpStream>,
) -> (u16, serde_json::Value) {
    use tokio::io::AsyncReadExt;
    let mut bytes = vec![];
    tls.read_to_end(&mut bytes).await.unwrap();
    let response = String::from_utf8(bytes).unwrap();
    let (headers, body) = response.split_once("\r\n\r\n").unwrap();
    (
        headers.split_whitespace().nth(1).unwrap().parse().unwrap(),
        serde_json::from_str(body).unwrap(),
    )
}

pub struct Client<S = tokio_rustls::client::TlsStream<tokio::net::TcpStream>> {
    pub socket: WebSocketStream<S>,
    pub exporter: [u8; 32],
    pub kind: ConnectionKind,
    pub nonce: [u8; 32],
}
impl<S: AsyncRead + AsyncWrite + Unpin> Client<S> {
    pub async fn send(&mut self, value: &impl serde::Serialize) {
        self.socket
            .send(Message::Text(serde_json::to_string(value).unwrap().into()))
            .await
            .unwrap();
    }
    pub async fn receive<T: serde::de::DeserializeOwned>(&mut self) -> T {
        match self.message().await {
            Message::Text(text) => serde_json::from_str(&text).unwrap(),
            message => panic!("expected JSON, got {message:?}"),
        }
    }
    pub async fn message(&mut self) -> Message {
        tokio::time::timeout(Duration::from_secs(5), self.socket.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
    }
    pub async fn challenge(&mut self, device_id: DeviceId) -> Challenge {
        self.send(&ClientFrame::Hello(Hello {
            supported: vec![ProtocolVersion::CURRENT],
            device_id,
            client_nonce: URL_SAFE_NO_PAD.encode(self.nonce),
            client_version: "test".into(),
            channel: self.kind,
        }))
        .await;
        match self.receive::<ServerFrame>().await {
            ServerFrame::Challenge(challenge) => challenge,
            other => panic!("expected challenge, got {other:?}"),
        }
    }
    pub fn proof(
        &self,
        challenge: &Challenge,
        key: &SigningKey,
        device_id: DeviceId,
    ) -> Authenticate {
        let transcript = auth_transcript(
            &challenge.host_id,
            &device_id,
            self.kind,
            &self.nonce,
            &URL_SAFE_NO_PAD.decode(&challenge.server_nonce).unwrap(),
            &self.exporter,
        );
        Authenticate {
            signature: URL_SAFE_NO_PAD.encode(key.sign(&transcript).to_bytes()),
        }
    }
    pub async fn binary(&mut self, bytes: &[u8]) {
        self.socket
            .send(Message::Binary(bytes.to_vec().into()))
            .await
            .unwrap();
    }
}

/// A client's TCP stream that counts the TLS records the server sends.
pub struct TlsRecords<S> {
    inner: S,
    header: Vec<u8>,
    skip: usize,
    pub count: Arc<AtomicUsize>,
}
impl<S> TlsRecords<S> {
    pub fn new(inner: S) -> Self {
        Self {
            inner,
            header: Vec::with_capacity(5),
            skip: 0,
            count: Arc::default(),
        }
    }
}
impl<S: AsyncRead + Unpin> AsyncRead for TlsRecords<S> {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let before = buf.filled().len();
        let this = &mut *self;
        let poll = std::pin::Pin::new(&mut this.inner).poll_read(cx, buf);
        for &byte in &buf.filled()[before..] {
            if this.skip > 0 {
                this.skip -= 1;
                continue;
            }
            this.header.push(byte);
            if this.header.len() == 5 {
                this.skip = u16::from_be_bytes([this.header[3], this.header[4]]).into();
                this.header.clear();
                this.count.fetch_add(1, Ordering::SeqCst);
            }
        }
        poll
    }
}
impl<S: AsyncWrite + Unpin> AsyncWrite for TlsRecords<S> {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        std::pin::Pin::new(&mut self.inner).poll_write(cx, buf)
    }
    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

#[derive(Debug)]
pub struct PinVerifier(pub String);
impl rustls::client::danger::ServerCertVerifier for PinVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &rustls::pki_types::CertificateDer<'_>,
        _: &[rustls::pki_types::CertificateDer<'_>],
        _: &rustls::pki_types::ServerName<'_>,
        _: &[u8],
        _: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        if companion::identity::fingerprint(end_entity) != self.0 {
            return Err(rustls::Error::General("certificate pin mismatch".into()));
        }
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        _: &[u8],
        _: &rustls::pki_types::CertificateDer<'_>,
        _: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Err(rustls::Error::General("TLS 1.2 prohibited".into()))
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        certificate: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            certificate,
            dss,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

#[derive(Default)]
pub struct MediaMock {
    pub acks: Mutex<Vec<u64>>,
    pub keyframes: AtomicUsize,
    pub adjusted: AtomicUsize,
    pub closed: AtomicBool,
}
#[async_trait]
impl MediaControl for MediaMock {
    async fn ack(&self, seq: u64) {
        self.acks.lock().unwrap().push(seq);
    }
    async fn adjust(&self, _: StreamParams) {
        self.adjusted.fetch_add(1, Ordering::SeqCst);
    }
    async fn request_keyframe(&self) {
        self.keyframes.fetch_add(1, Ordering::SeqCst);
    }
    async fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
    }
}
#[derive(Default)]
pub struct TerminalMock {
    pub bytes: Mutex<Vec<u8>>,
    pub resized: Mutex<Vec<(u16, u16)>>,
    pub detached: AtomicBool,
}
#[async_trait]
impl TerminalInput for TerminalMock {
    async fn write(&self, bytes: &[u8]) -> Result<(), ProtocolError> {
        self.bytes.lock().unwrap().extend_from_slice(bytes);
        Ok(())
    }
    async fn resize(&self, cols: u16, rows: u16) -> Result<(), ProtocolError> {
        self.resized.lock().unwrap().push((cols, rows));
        Ok(())
    }
    async fn detach(&self) {
        self.detached.store(true, Ordering::SeqCst);
    }
}
pub struct SpeechMock {
    pub bytes: Mutex<Vec<u8>>,
    pub finished: AtomicBool,
    pub cancelled: AtomicBool,
    pub output: mpsc::Sender<SpeechOut>,
}
#[async_trait]
impl SpeechInput for SpeechMock {
    async fn push(&self, pcm: &[u8]) -> Result<(), ProtocolError> {
        self.bytes.lock().unwrap().extend_from_slice(pcm);
        Ok(())
    }
    async fn finish(&self) {
        self.finished.store(true, Ordering::SeqCst);
        self.output
            .send(SpeechOut::Finished {
                audio_artifact: None,
            })
            .await
            .unwrap();
    }
    async fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }
}
