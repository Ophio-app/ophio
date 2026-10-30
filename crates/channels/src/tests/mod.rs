// SPDX-License-Identifier: Apache-2.0
mod behavior;
mod recovery;
mod transports;

use super::*;
use crate::{
    core::{ChatIo, Core, Incoming, Route},
    discord::Discord,
    http::{Http, Pace},
    state::Store,
    telegram::Telegram,
};
use axum::{
    Router,
    body::to_bytes,
    extract::{Request, State},
    response::IntoResponse,
    routing::any,
};
use ports::Clock;
use protocol::{auth::*, capability::*, command::*, control::*, media::StreamParams, model::*, *};
use serde_json::{Value, json};
use services::{api::*, testing::FakeClock};
use std::{
    collections::{HashMap, VecDeque},
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::sync::broadcast;

fn caps() -> HostCapabilities {
    HostCapabilities {
        conversation_access: None,
        os: HostOs::Linux,
        desktop_backend: "synthetic".into(),
        capture: Availability::Available,
        input: Availability::Available,
        input_scope: InputScope::None,
        terminal: Availability::Available,
        lock_session: Availability::Available,
        speech: Availability::Available,
        library: Availability::Available,
        transfers: Availability::Available,
        computer_use: Availability::Available,
        telegram: Availability::Available,
        discord: Availability::Available,
        features: vec![],
        window_actions: None,
        unlock_session: None,
        app_updates: None,
    }
}
struct FakeHub {
    snapshot: Mutex<Snapshot>,
    snapshot_queries: AtomicU64,
    identities: Mutex<Vec<ChannelIdentity>>,
    links: Mutex<Vec<ChannelLinkRequest>>,
    link_errors: Mutex<VecDeque<ErrorCode>>,
    link_outcomes: Mutex<HashMap<String, ChannelLinkOutcome>>,
    commands: Mutex<HashMap<CommandId, CommandReceipt>>,
    submitted: Mutex<Vec<(Caller, CommandEnvelope)>>,
    events: broadcast::Sender<Outbound>,
    clock: Arc<FakeClock>,
    stage: Mutex<Option<CommandStage>>,
    history: Mutex<Vec<Arc<EventEnvelope>>>,
    subscriptions: Mutex<Vec<(ChannelIdentityId, Option<u64>)>>,
    gap: std::sync::atomic::AtomicBool,
    snapshot_errors: Mutex<VecDeque<ErrorCode>>,
}
impl FakeHub {
    fn new(clock: Arc<FakeClock>) -> Self {
        let project = Project {
            project_id: ProjectId::new(),
            name: "Private project".into(),
            host_path: "/private/repo".into(),
            default_harness: Some(HarnessKind::Codex),
            default_model: Some("chosen-model".into()),
            default_effort: Some("chosen-effort".into()),
            active_writer: None,
            vcs: None,
            created_at: clock.now(),
            stage: None,
            one_off: false,
            home: false,
        };
        Self {
            snapshot_queries: AtomicU64::new(0),
            snapshot: Mutex::new(Snapshot {
                journal_head: 0,
                host: HostInfo {
                    inbox_path: None,
                    host_id: HostId::new(),
                    name: "Synthetic computer".into(),
                    os_label: "synthetic".into(),
                    companion_version: "test".into(),
                    boot_epoch: BootEpoch::new(),
                    started_at: clock.now(),
                    capabilities: caps(),
                },
                projects: vec![project],
                tasks: vec![],
                runs: vec![],
                approvals: vec![],
                attention: vec![],
                targets: vec![],
                control: vec![],
                library_roots: vec![],
                older_tasks: None,
                withheld_approvals: Vec::new(),
            }),
            identities: Mutex::new(vec![]),
            links: Mutex::new(vec![]),
            commands: Mutex::new(HashMap::new()),
            submitted: Mutex::new(vec![]),
            events: broadcast::channel(64).0,
            clock,
            stage: Mutex::new(None),
            history: Mutex::new(vec![]),
            subscriptions: Mutex::new(vec![]),
            gap: std::sync::atomic::AtomicBool::new(false),
            snapshot_errors: Mutex::new(VecDeque::new()),
            link_errors: Mutex::new(VecDeque::new()),
            link_outcomes: Mutex::new(HashMap::new()),
        }
    }
    fn project(&self) -> ProjectId {
        self.snapshot.lock().unwrap().projects[0].project_id
    }
    fn link(&self, kind: ChannelKind, user: &str, chat: &str) -> ChannelIdentity {
        // Deliberately broad grants test the adapter's independent intersection.
        let identity = ChannelIdentity {
            id: ChannelIdentityId::new(),
            channel: kind,
            external_user: user.into(),
            external_chat: chat.into(),
            display_name: "Synthetic user".into(),
            linked_at: self.clock.now(),
            revoked_at: None,
            grants: Grants::owner(),
            project_id: None,
        };
        self.identities.lock().unwrap().push(identity.clone());
        identity
    }
    fn authorized(&self, c: &Caller) -> bool {
        matches!(c.principal,Principal::Channel {identity_id} if self.identities.lock().unwrap().iter().any(|i|i.id==identity_id && i.revoked_at.is_none()))
    }
    fn task(&self) -> Task {
        self.snapshot.lock().unwrap().tasks[0].clone()
    }
    fn approval(&self, kind: ApprovalKind) -> Approval {
        let task = self.task();
        let a = Approval {
            outcome: None,
            approval_id: ApprovalId::new(),
            run_id: RunId::new(),
            task_id: task.task_id,
            project_id: task.project_id,
            native_request_id: "synthetic-request".into(),
            kind,
            title: "Run a specific command".into(),
            detail: Some("Exact effect description".into()),
            detail_lines: Vec::new(),
            questions: Vec::new(),
            raw_request: Some("cargo test --locked".into()),
            scope: "One invocation".into(),
            shortened: false,
            revision: 1,
            state: ApprovalState::Pending,
            delivery: None,
            requested_at: self.clock.now(),
            expires_at: None,
            resolved_by: None,
            resolved_at: None,
        };
        self.snapshot.lock().unwrap().approvals.push(a.clone());
        a
    }
    fn event(&self, event: Event) -> EventEnvelope {
        let seq = {
            let mut s = self.snapshot.lock().unwrap();
            s.journal_head += 1;
            s.journal_head
        };
        let envelope = EventEnvelope {
            seq,
            at: self.clock.now(),
            source: protocol::event::EventSource::Companion,
            provenance: Provenance::Observed,
            event,
        };
        self.history
            .lock()
            .unwrap()
            .push(Arc::new(envelope.clone()));
        envelope
    }
}
#[async_trait]
impl Hub for FakeHub {
    fn host_id(&self) -> HostId {
        self.snapshot.lock().unwrap().host.host_id
    }
    fn host_name(&self) -> String {
        "test".into()
    }
    fn boot_epoch(&self) -> BootEpoch {
        self.snapshot.lock().unwrap().host.boot_epoch
    }
    fn journal_head(&self) -> u64 {
        self.snapshot.lock().unwrap().journal_head
    }
    async fn capabilities(&self) -> HostCapabilities {
        caps()
    }
    async fn channel_identity(
        &self,
        kind: ChannelKind,
        user: &str,
        chat: &str,
    ) -> Option<ChannelIdentity> {
        self.identities
            .lock()
            .unwrap()
            .iter()
            .find(|i| i.channel == kind && i.external_user == user && i.external_chat == chat)
            .cloned()
    }
    async fn request_channel_link(
        &self,
        request: ChannelLinkRequest,
    ) -> std::result::Result<String, ProtocolError> {
        if let Some(code) = self.link_errors.lock().unwrap().pop_front() {
            return Err(ProtocolError::new(code, "synthetic link refusal"));
        }
        self.links.lock().unwrap().push(request);
        self.link_outcomes
            .lock()
            .unwrap()
            .insert("TEST42".into(), ChannelLinkOutcome::Waiting);
        Ok("TEST42".into())
    }
    async fn channel_link_outcome(&self, _: ChannelKind, code: &str) -> Option<ChannelLinkOutcome> {
        self.link_outcomes.lock().unwrap().get(code).copied()
    }
    async fn submit(&self, caller: &Caller, command: CommandEnvelope) -> CommandReceipt {
        self.submitted
            .lock()
            .unwrap()
            .push((caller.clone(), command.clone()));
        if let Some(mut receipt) = self
            .commands
            .lock()
            .unwrap()
            .get(&command.command_id)
            .cloned()
        {
            receipt.status = ReceiptStatus::Duplicate;
            return receipt;
        }
        assert!(self.authorized(caller));
        assert!(
            !caller.grants.admin
                && !caller.grants.view
                && !caller.grants.terminal
                && !caller.grants.computer_use
                && !caller.grants.transfers
                && !caller.grants.library_read
        );
        let mut snapshot = self.snapshot.lock().unwrap();
        let mut error = None;
        let result = match &command.body {
            Command::CreateTask {
                project_id,
                harness,
                model,
                effort,
                ..
            } => {
                let id = TaskId::new();
                snapshot.tasks.push(Task {
                    github: None,
                    mode: protocol::model::TaskMode::Work,
                    task_id: id,
                    project_id: *project_id,
                    kind: TaskKind::Coding,
                    title: "Sensitive task title".into(),
                    state: TaskState::Working,
                    revision: 1,
                    harness: *harness,
                    model: model.clone(),
                    effort: effort.clone(),
                    launch_policy: None,
                    branch: None,
                    created_by: caller.principal,
                    created_at: self.clock.now(),
                    updated_at: self.clock.now(),
                    current_run: None,
                    target_id: None,
                    access: None,
                    access_blocked: None,
                    mission: None,
                    attention_count: 0,
                    status_line: Some("PRIVATE STATUS".into()),
                    wake: None,
                    asked_by: None,
                    start: None,
                    browser: false,
                    conversation: false,
                    review: None,
                });
                Some(CommandResult::TaskCreated {
                    task_id: id,
                    queue_item_id: None,
                })
            }
            Command::SendMessage { .. } => None,
            Command::StopTask { task_id, .. } => {
                let t = snapshot
                    .tasks
                    .iter_mut()
                    .find(|t| t.task_id == *task_id)
                    .unwrap();
                if command.expected_revision != Some(t.revision) {
                    error = Some(ProtocolError::new(
                        ErrorCode::StaleRevision,
                        "private error body",
                    ));
                } else {
                    t.state = TaskState::Interrupted;
                }
                None
            }
            Command::ResolveApproval {
                approval_id,
                decision,
            } => {
                let a = snapshot
                    .approvals
                    .iter_mut()
                    .find(|a| a.approval_id == *approval_id)
                    .unwrap();
                if command.expected_revision != Some(u64::from(a.revision))
                    || a.state != ApprovalState::Pending
                {
                    error = Some(ProtocolError::new(
                        ErrorCode::AlreadyResolved,
                        "private error body",
                    ));
                } else {
                    a.state = if matches!(decision, ApprovalDecision::Deny { .. }) {
                        ApprovalState::Denied
                    } else {
                        ApprovalState::Allowed
                    };
                    a.revision += 1;
                    a.resolved_by = Some(caller.principal);
                }
                None
            }
            other => panic!("forbidden command {other:?}"),
        };
        let receipt = CommandReceipt {
            command_id: command.command_id,
            status: if error.is_some() {
                ReceiptStatus::Rejected
            } else {
                ReceiptStatus::Accepted
            },
            seq: Some(1),
            result,
            error,
        };
        self.commands
            .lock()
            .unwrap()
            .insert(command.command_id, receipt.clone());
        receipt
    }
    async fn query(
        &self,
        caller: &Caller,
        query: Query,
    ) -> std::result::Result<QueryData, ProtocolError> {
        if !self.authorized(caller) {
            return Err(ProtocolError::new(ErrorCode::Unauthenticated, "revoked"));
        }
        match query {
            Query::Snapshot { .. } => {
                self.snapshot_queries.fetch_add(1, Ordering::SeqCst);
                if let Some(code) = self.snapshot_errors.lock().unwrap().pop_front() {
                    return Err(ProtocolError::new(code, "synthetic snapshot refusal"));
                }
                Ok(QueryData::Snapshot(Box::new(
                    self.snapshot.lock().unwrap().clone(),
                )))
            }
            Query::CommandStatus { command_id } => Ok(QueryData::CommandStatus {
                receipt: self.commands.lock().unwrap().get(&command_id).cloned(),
                stage: *self.stage.lock().unwrap(),
                result: None,
                error: None,
            }),
            _ => panic!("forbidden query"),
        }
    }
    async fn subscribe(
        &self,
        caller: &Caller,
        after: Option<u64>,
    ) -> std::result::Result<Subscription, ProtocolError> {
        assert!(self.authorized(caller));
        let Principal::Channel { identity_id } = caller.principal else {
            panic!("channel caller");
        };
        self.subscriptions
            .lock()
            .unwrap()
            .push((identity_id, after));
        let backlog = if self.gap.load(Ordering::SeqCst) && after.unwrap_or(0) < self.journal_head()
        {
            Backlog::Gap {
                requested_from: after.unwrap_or(0),
                oldest_retained: self.journal_head(),
            }
        } else {
            Backlog::Events(
                self.history
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|e| after.is_some_and(|seq| e.seq > seq))
                    .cloned()
                    .collect(),
            )
        };
        Ok(Subscription {
            backlog,
            head: self.journal_head(),
            live: self.events.subscribe(),
        })
    }
    async fn backlog(
        &self,
        _: &Caller,
        _: u64,
        _: u64,
        _: usize,
    ) -> std::result::Result<(Backlog, u64), ProtocolError> {
        panic!("forbidden hub route: backlog")
    }
    async fn open_pairing(&self, _: &Caller) -> std::result::Result<PairingTicket, ProtocolError> {
        panic!("forbidden hub route: open_pairing")
    }
    async fn request_pairing(
        &self,
        _: PairRequest,
        _: String,
    ) -> std::result::Result<PairingClaim, ProtocolError> {
        panic!("forbidden hub route: request_pairing")
    }
    async fn waiting_pairing(
        &self,
        _: &Caller,
    ) -> std::result::Result<Option<PairingPrompt>, ProtocolError> {
        panic!("forbidden hub route: waiting_pairing")
    }
    async fn answer_pairing(
        &self,
        _: &Caller,
        _: &str,
        _: bool,
    ) -> std::result::Result<Option<DeviceRecord>, ProtocolError> {
        panic!("forbidden hub route: answer_pairing")
    }
    async fn device_credential(&self, _: &DeviceId) -> Option<DeviceCredential> {
        panic!("forbidden hub route: device_credential")
    }
    async fn device_seen(&self, _: &DeviceId) {
        panic!("forbidden hub route: device_seen")
    }
    async fn input(&self, _: &Caller, _: InputEnvelope) -> InputAck {
        panic!("forbidden hub route: input")
    }
    async fn renew_lease(
        &self,
        _: &Caller,
        _: LeaseId,
        _: u64,
    ) -> std::result::Result<(), ProtocolError> {
        panic!("forbidden hub route: renew_lease")
    }
    async fn control_disconnected(&self, _: &Caller) {
        panic!("forbidden hub route: control_disconnected")
    }
    async fn open_media(
        &self,
        _: &Caller,
        _: TargetId,
        _: StreamParams,
    ) -> std::result::Result<MediaSession, ProtocolError> {
        panic!("forbidden hub route: open_media")
    }
    async fn attach_terminal(
        &self,
        _: &Caller,
        _: TerminalId,
        _: Option<u64>,
    ) -> std::result::Result<TerminalAttachment, ProtocolError> {
        panic!("forbidden hub route: attach_terminal")
    }
    async fn upload(
        &self,
        _: &Caller,
        _: TransferId,
    ) -> std::result::Result<Box<dyn UploadSink>, ProtocolError> {
        panic!("forbidden hub route: upload")
    }
    async fn download(
        &self,
        _: &Caller,
        _: TransferId,
        _: u64,
    ) -> std::result::Result<services::api::DownloadSource, ProtocolError> {
        panic!("forbidden hub route: download")
    }
    async fn cancel_transfer(
        &self,
        _: &Caller,
        _: TransferId,
    ) -> std::result::Result<(), ProtocolError> {
        panic!("forbidden hub route: cancel_transfer")
    }
    async fn start_speech(
        &self,
        _: &Caller,
        _: String,
        _: Option<String>,
        _: bool,
    ) -> std::result::Result<SpeechStream, ProtocolError> {
        panic!("forbidden hub route: start_speech")
    }
}

#[derive(Clone)]
struct Mock {
    inner: Arc<MockInner>,
}
struct MockInner {
    requests: Mutex<Vec<(String, Value, tokio::time::Instant)>>,
    replies: Mutex<HashMap<String, VecDeque<(u16, Value)>>>,
    updates: Mutex<VecDeque<Value>>,
    chat_replies: Mutex<HashMap<String, VecDeque<(u16, Value)>>>,
    next: AtomicU64,
}
impl Mock {
    fn new() -> Self {
        Self {
            inner: Arc::new(MockInner {
                requests: Mutex::new(vec![]),
                replies: Mutex::new(HashMap::new()),
                updates: Mutex::new(VecDeque::new()),
                chat_replies: Mutex::new(HashMap::new()),
                next: AtomicU64::new(100),
            }),
        }
    }
    fn script(&self, path: &str, replies: Vec<(u16, Value)>) {
        self.inner
            .replies
            .lock()
            .unwrap()
            .insert(path.into(), replies.into());
    }
    fn requests(&self) -> Vec<(String, Value, tokio::time::Instant)> {
        self.inner.requests.lock().unwrap().clone()
    }
    fn texts(&self) -> String {
        self.requests()
            .iter()
            .filter_map(|(_, v, _)| {
                v["text"]
                    .as_str()
                    .or(v["content"].as_str())
                    .map(str::to_owned)
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
    fn last_buttons(&self) -> Vec<String> {
        self.requests()
            .iter()
            .rev()
            .find_map(|(_, v, _)| {
                if let Some(a) = v["reply_markup"]["inline_keyboard"][0]
                    .as_array()
                    .filter(|a| !a.is_empty())
                {
                    return Some(
                        a.iter()
                            .map(|b| b["callback_data"].as_str().unwrap().into())
                            .collect(),
                    );
                }
                v["components"][0]["components"]
                    .as_array()
                    .filter(|a| !a.is_empty())
                    .map(|a| {
                        a.iter()
                            .map(|b| b["custom_id"].as_str().unwrap().into())
                            .collect()
                    })
            })
            .unwrap_or_default()
    }
}
async fn mock_handler(State(mock): State<Mock>, request: Request) -> impl IntoResponse {
    let path = request.uri().path().to_owned();
    let body = to_bytes(request.into_body(), 1024 * 1024).await.unwrap();
    let value: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    mock.inner.requests.lock().unwrap().push((
        path.clone(),
        value.clone(),
        tokio::time::Instant::now(),
    ));
    if path.ends_with("sendMessage")
        && let Some((code, body)) = mock
            .inner
            .chat_replies
            .lock()
            .unwrap()
            .get_mut(value["chat_id"].as_str().unwrap_or_default())
            .and_then(VecDeque::pop_front)
    {
        return (
            axum::http::StatusCode::from_u16(code).unwrap(),
            axum::Json(body),
        );
    }
    if let Some((code, body)) = mock
        .inner
        .replies
        .lock()
        .unwrap()
        .get_mut(&path)
        .and_then(VecDeque::pop_front)
    {
        return (
            axum::http::StatusCode::from_u16(code).unwrap(),
            axum::Json(body),
        );
    }
    let id = mock.inner.next.fetch_add(1, Ordering::SeqCst);
    let result = if path.contains("/bot") {
        let result = if path.ends_with("getMe") {
            json!({"id":123,"is_bot":true})
        } else if path.ends_with("getWebhookInfo") {
            json!({"url":""})
        } else if path.ends_with("getUpdates") {
            mock.inner
                .updates
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(json!([]))
        } else {
            json!({"message_id":id})
        };
        json!({"ok":true,"result":result})
    } else if path.ends_with("users/@me") || path.ends_with("applications/@me") {
        json!({"id":"123"})
    } else {
        json!({"id":id.to_string()})
    };
    (axum::http::StatusCode::OK, axum::Json(result))
}
struct Server {
    base: String,
    mock: Mock,
    worker: tokio::task::JoinHandle<()>,
}
impl Server {
    async fn new() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let mock = Mock::new();
        let app = Router::new()
            .fallback(any(mock_handler))
            .with_state(mock.clone());
        let worker = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self { base, mock, worker }
    }
    fn io(&self, kind: ChannelKind) -> Arc<dyn ChatIo> {
        let http = Http::new(
            self.base.clone(),
            BotToken::new("123:synthetic".into()).unwrap(),
            kind == ChannelKind::Telegram,
            CancellationToken::new(),
        )
        .unwrap();
        match kind {
            ChannelKind::Telegram => Arc::new(Telegram::new(http)),
            ChannelKind::Discord => Arc::new(Discord::new(http)),
        }
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.worker.abort();
    }
}
struct Fixture {
    hub: Arc<FakeHub>,
    clock: Arc<FakeClock>,
    dir: tempfile::TempDir,
    core: Arc<Core>,
    server: Server,
    io: Arc<dyn ChatIo>,
    kind: ChannelKind,
}
impl Fixture {
    async fn new(kind: ChannelKind, exposure: Exposure) -> Self {
        let clock = Arc::new(FakeClock::default());
        let hub = Arc::new(FakeHub::new(clock.clone()));
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("state")).await.unwrap();
        let cfg = ChannelConfig {
            projects: [("42".into(), hub.project())].into(),
            exposure,
            ..ChannelConfig::default()
        };
        let core = Arc::new(Core::new(hub.clone(), cfg, store, clock.clone(), kind));
        let server = Server::new().await;
        let io = server.io(kind);
        Self {
            hub,
            clock,
            dir,
            core,
            server,
            io,
            kind,
        }
    }
    fn input(&self, event: &str, text: &str) -> Incoming {
        Incoming {
            route: self.route("7"),
            name: None,
            event: event.into(),
            text: text.into(),
            reply: None,
            thread: None,
            action: None,
            explicit_answer: false,
            forwarded: false,
            acknowledgement: None,
            rate_checked: false,
        }
    }
    fn route(&self, user: &str) -> Route {
        Route {
            bot: "123".into(),
            user: user.into(),
            chat: "42".into(),
        }
    }
    async fn handle(&self, mut input: Incoming) {
        self.core
            .handle(self.io.as_ref(), &mut input)
            .await
            .unwrap();
    }
    async fn create(&self) -> ChannelIdentity {
        let identity = self.hub.link(self.kind, "7", "42");
        self.handle(self.input("new:1", "/new Build the requested feature"))
            .await;
        identity
    }
    fn message_id(&self) -> String {
        self.server
            .mock
            .requests()
            .iter()
            .rev()
            .find_map(|(path, v, _)| {
                if path.ends_with("editMessageText") {
                    v["message_id"].as_str().map(str::to_owned)
                } else if path.contains("/messages/") {
                    path.rsplit('/').next().map(str::to_owned)
                } else {
                    None
                }
            })
            .unwrap()
    }
    async fn notice(&self, identity: &ChannelIdentity, approval: Approval) {
        let event = self.hub.event(Event::ApprovalUpserted { approval });
        self.core
            .notify(self.io.as_ref(), &self.route("7"), identity, &event)
            .await
            .unwrap();
    }
    async fn ignored_notification(&self, identity: &ChannelIdentity, event: Event) {
        let queries = self.hub.snapshot_queries.load(Ordering::SeqCst);
        let messages = self.server.mock.requests().len();
        self.core
            .notify(
                self.io.as_ref(),
                &self.route("7"),
                identity,
                &self.hub.event(event.clone()),
            )
            .await
            .unwrap();
        assert_eq!(
            self.hub.snapshot_queries.load(Ordering::SeqCst),
            queries,
            "{:?}: {event:?}",
            self.kind
        );
        assert_eq!(self.server.mock.requests().len(), messages);
    }
}
async fn until(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}
