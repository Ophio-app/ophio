// SPDX-License-Identifier: Apache-2.0
use crate::{
    ChannelConfig, Error, Exposure, Result,
    recovery::{Failure, Scope, Status, skip_refused},
    state::Store,
};
use async_trait::async_trait;
use ports::Clock;
use protocol::{
    auth::{ChannelIdentity, ChannelKind, Grants, Principal},
    command::{ApprovalDecision, CommandResult, CommandStage, MessageDelivery, TaskPlacement},
    model::{
        Approval, ApprovalKind, ApprovalState, AttentionKind, Snapshot, Task, TaskKind, TaskState,
    },
    time::Timestamp,
    *,
};
use serde::{Deserialize, Serialize};
use services::api::{
    Backlog, Caller, ChannelLinkOutcome, ChannelLinkRequest, Hub, LINK_CODE_MINUTES, Outbound,
    Subscription,
};
use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

const APP: &str = "Connections › Chat";
const UNKNOWN: &str = "Outcome unknown — check the app";
const MAX_INPUT: usize = 16_384;
/// Longest sender name kept for the app's list of linked chats.
const MAX_NAME: usize = 64;
/// The hub keeps at most this many bytes of a chat's name.
const MAX_NAME_BYTES: usize = 200;
/// Routes a connector remembers for notifications and link replies.
const MAX_ROUTES: usize = 1024;
/// How long a route nobody is linked through is remembered: as long as the
/// newest code its person was given. After that the code can't be approved,
/// and another message from the chat asks for a new code and remembers the
/// route again.
const UNLINKED_ROUTE_MS: i64 = LINK_CODE_MINUTES as i64 * 60_000;

/// Length-prefixed identities prevent ambiguous concatenation. Platform IDs are
/// authenticated by the bot connection; user-supplied text never chooses this ID.
pub fn command_id(kind: ChannelKind, bot: &str, event: &str) -> CommandId {
    let key = serde_json::to_vec(&(kind, bot, event)).expect("string tuple serialization");
    CommandId(uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, &key))
}
/// Split on Unicode scalar boundaries using UTF-16 units (conservative for both APIs).
pub fn split_message(text: &str, limit: usize) -> Vec<String> {
    let limit = limit.max(2);
    let mut result = Vec::new();
    let mut chunk = String::new();
    let mut units = 0;
    for c in text.chars() {
        if units + c.len_utf16() > limit {
            result.push(std::mem::take(&mut chunk));
            units = 0;
        }
        chunk.push(c);
        units += c.len_utf16();
    }
    if !chunk.is_empty() {
        result.push(chunk);
    }
    if result.is_empty() {
        result.push(" ".into());
    }
    result
}
/// Format characters (Unicode category Cf: direction controls, zero-width
/// spaces and joiners, tags) and the Hangul fillers. They show nothing
/// themselves, so a name made of them looks blank, and direction controls
/// change how the text around them reads.
fn invisible(c: char) -> bool {
    matches!(c,
        '\u{00AD}' | '\u{0600}'..='\u{0605}' | '\u{061C}' | '\u{06DD}' | '\u{070F}'
        | '\u{0890}'..='\u{0891}' | '\u{08E2}' | '\u{180E}' | '\u{200B}'..='\u{200F}'
        | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2064}' | '\u{2066}'..='\u{206F}'
        | '\u{FEFF}' | '\u{FFF9}'..='\u{FFFB}' | '\u{110BD}' | '\u{110CD}'
        | '\u{13430}'..='\u{1343F}' | '\u{1BCA0}'..='\u{1BCA3}' | '\u{1D173}'..='\u{1D17A}'
        | '\u{E0001}' | '\u{E0020}'..='\u{E007F}'
        | '\u{115F}' | '\u{1160}' | '\u{3164}' | '\u{FFA0}')
}
/// A platform-supplied name made fit to show in the app: control characters
/// become spaces, invisible characters are dropped so it reads as typed, and
/// a long name is shortened. A name with nothing left to show is none.
pub(crate) fn person_name(raw: &str) -> Option<String> {
    let cleaned: String = raw
        .chars()
        .filter(|c| !invisible(*c))
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let name = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    if name.chars().count() <= MAX_NAME && name.len() <= MAX_NAME_BYTES {
        return (!name.is_empty()).then_some(name);
    }
    // Whole characters only, and room left for the ellipsis in both limits.
    let mut short = String::new();
    for c in name.chars().take(MAX_NAME - 1) {
        if short.len() + c.len_utf8() + '…'.len_utf8() > MAX_NAME_BYTES {
            break;
        }
        short.push(c);
    }
    short.push('…');
    Some(short)
}
pub fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Route {
    pub bot: String,
    pub user: String,
    pub chat: String,
}
impl Route {
    pub fn key(&self, kind: ChannelKind) -> String {
        command_id(
            kind,
            &self.bot,
            &serde_json::to_string(&(&self.user, &self.chat)).expect("strings"),
        )
        .to_string()
    }
}
/// A remembered route and when it was last remembered, so that routes nobody
/// is linked through expire and the oldest of them make room first.
#[derive(Serialize, Deserialize)]
struct Remembered {
    #[serde(flatten)]
    route: Route,
    /// Absent on routes remembered before this was recorded; they go first.
    #[serde(default)]
    at: Option<Timestamp>,
    /// Someone was linked through the route when it was last checked. Absent
    /// on routes remembered before this was recorded, which are checked again.
    #[serde(default)]
    linked: bool,
}
pub(crate) struct Incoming {
    pub route: Route,
    /// The sender's name as the platform shows it, when it gives one.
    pub name: Option<String>,
    pub event: String,
    pub text: String,
    pub reply: Option<String>,
    pub thread: Option<String>,
    pub action: Option<String>,
    pub explicit_answer: bool,
    /// Forwarded from elsewhere: its text is never a command or an answer.
    pub forwarded: bool,
    /// Retained across retries so a confirmed acknowledgement is sent only once.
    pub acknowledgement: Option<String>,
    pub rate_checked: bool,
}
#[derive(Clone, Default)]
pub(crate) struct Message {
    pub text: String,
    pub buttons: Vec<Button>,
    pub thread: Option<String>,
}
impl Message {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            buttons: Vec::new(),
            thread: None,
        }
    }
}
#[derive(Clone)]
pub(crate) struct Button {
    pub label: &'static str,
    pub data: String,
}
#[async_trait]
pub(crate) trait ChatIo: Send + Sync {
    async fn send(&self, chat: &str, message: Message) -> Result<String>;
    async fn edit(&self, chat: &str, id: &str, message: Message) -> Result<()>;
}
#[derive(Clone, Serialize, Deserialize)]
struct Intent {
    identity: ChannelIdentityId,
    project: ProjectId,
    envelope: CommandEnvelope,
}
#[derive(Clone, Serialize, Deserialize)]
enum Binding {
    Task(TaskId),
    Approval(String),
    Ambiguous,
}
#[derive(Clone, Serialize, Deserialize)]
struct Reference {
    identity: ChannelIdentityId,
    route_key: String,
    project: ProjectId,
    task: TaskId,
    revision: u64,
    expires: Timestamp,
    boot: BootEpoch,
    purpose: Purpose,
    used: Option<CommandId>,
}
#[derive(Clone, Serialize, Deserialize)]
enum Purpose {
    Stop,
    Approval { id: ApprovalId, kind: ApprovalKind },
}
/// A link code this bot handed out, kept until the person who asked has been
/// told whether it was approved or refused, or until it expires.
#[derive(Clone, Serialize, Deserialize)]
struct LinkCode {
    code: String,
    route: Route,
    thread: Option<String>,
    expires: Timestamp,
}
#[derive(Clone, Serialize, Deserialize)]
struct Notice {
    reference: String,
    message: String,
    revision: u32,
    state: ApprovalState,
}

pub(crate) struct Core {
    pub hub: Arc<dyn Hub>,
    pub cfg: ChannelConfig,
    pub store: Store,
    pub kind: ChannelKind,
    clock: Arc<dyn Clock>,
    limits: Mutex<HashMap<String, (Instant, u32)>>,
    /// Serializes admission/reference consumption, not the gateway heartbeat.
    admission: Mutex<()>,
}
impl Core {
    pub fn new(
        hub: Arc<dyn Hub>,
        cfg: ChannelConfig,
        store: Store,
        clock: Arc<dyn Clock>,
        kind: ChannelKind,
    ) -> Self {
        Self {
            hub,
            cfg,
            store,
            kind,
            clock,
            limits: Mutex::new(HashMap::new()),
            admission: Mutex::new(()),
        }
    }
    async fn identity(&self, route: &Route) -> Option<ChannelIdentity> {
        self.hub
            .channel_identity(self.kind, &route.user, &route.chat)
            .await
            .filter(|i| {
                i.revoked_at.is_none()
                    && i.channel == self.kind
                    && i.external_user == route.user
                    && i.external_chat == route.chat
            })
    }
    fn caller(identity: &ChannelIdentity) -> Caller {
        Caller {
            principal: Principal::Channel {
                identity_id: identity.id,
            },
            grants: Grants {
                tasks: identity.grants.tasks,
                approvals: identity.grants.approvals,
                ..Grants::default()
            },
        }
    }
    async fn snapshot(&self, caller: &Caller) -> Result<Box<Snapshot>> {
        match self
            .hub
            .query(
                caller,
                Query::Snapshot {
                    reads_withheld: None,
                },
            )
            .await
            .map_err(|e| Error::Hub(e.code))?
        {
            QueryData::Snapshot(snapshot) => Ok(snapshot),
            _ => Err(Error::Hub(ErrorCode::Internal)),
        }
    }
    async fn rate_allowed(&self, key: &str) -> bool {
        let now = self.clock.monotonic();
        let mut limits = self.limits.lock().await;
        limits.retain(|_, (at, _)| now.saturating_duration_since(*at) < Duration::from_secs(60));
        if limits.len() >= 4096 && !limits.contains_key(key) {
            return false;
        }
        let entry = limits.entry(key.to_owned()).or_insert((now, 0));
        entry.1 += 1;
        entry.1 <= 20
    }
    pub async fn handle(&self, io: &dyn ChatIo, input: &mut Incoming) -> Result<()> {
        let _serial = self.admission.lock().await;
        let route = &input.route;
        if !input.rate_checked && !self.rate_allowed(&route.key(self.kind)).await {
            if let Some(ack) = &input.acknowledgement {
                io.edit(
                    &route.chat,
                    ack,
                    Message::text("Request refused (LimitExceeded). Try again in a minute."),
                )
                .await?;
            }
            return Ok(());
        }
        input.rate_checked = true;
        let ack = match input.acknowledgement.clone() {
            Some(id) => id,
            None => {
                io.send(
                    &route.chat,
                    Message {
                        thread: input.thread.clone(),
                        ..Message::text("Received")
                    },
                )
                .await?
            }
        };
        input.acknowledgement = Some(ack.clone());
        let result = self.process(io, input, &ack).await;
        if result.is_ok()
            && let Some(thread) = &input.thread
            && let Some(identity) = self.identity(route).await
            && let Some(Binding::Task(task)) = self
                .store
                .get::<Binding>("bindings", &self.binding_key(route, identity.id, &ack))
                .await?
        {
            let key = self.binding_key(route, identity.id, thread);
            let binding = match self.store.get::<Binding>("threads", &key).await? {
                None => Binding::Task(task),
                Some(Binding::Task(old)) if old == task => Binding::Task(task),
                _ => Binding::Ambiguous,
            };
            self.store.put("threads", &key, &binding).await?;
        }
        if let Err(error) = result {
            let text = match error {
                Error::Hub(ErrorCode::OutcomeUnknown) => UNKNOWN.to_owned(),
                Error::Hub(code) => format!("Request refused ({code:?}). Check the app."),
                _ => return Err(error),
            };
            io.edit(&route.chat, &ack, Message::text(text)).await?;
        }
        Ok(())
    }
    async fn process(&self, io: &dyn ChatIo, input: &Incoming, ack: &str) -> Result<()> {
        let route = &input.route;
        let route_key = route.key(self.kind);
        if input.text.len() > MAX_INPUT {
            return Err(Error::Hub(ErrorCode::LimitExceeded));
        }
        let Some(identity) = self.identity(route).await else {
            let code = match self
                .hub
                .request_channel_link(ChannelLinkRequest {
                    channel: self.kind,
                    external_user: route.user.clone(),
                    external_chat: route.chat.clone(),
                    display_name: input.name.clone().unwrap_or_else(|| {
                        match self.kind {
                            ChannelKind::Telegram => "Telegram chat",
                            ChannelKind::Discord => "Discord chat",
                        }
                        .into()
                    }),
                })
                .await
            {
                Ok(code) => code,
                // The code message promises a new code for the next message.
                Err(e) if e.code == ErrorCode::LimitExceeded => {
                    return io
                        .edit(
                            &route.chat,
                            ack,
                            Message::text(
                                "Wait a minute, then send any message here again for a new code.",
                            ),
                        )
                        .await;
                }
                Err(e) => return Err(Error::Hub(e.code)),
            };
            // Only successful link requests are remembered, and never contain a token.
            self.remember(route, false).await?;
            self.store
                .put(
                    "link_codes",
                    &code,
                    &LinkCode {
                        code: code.clone(),
                        route: route.clone(),
                        thread: input.thread.clone(),
                        expires: Timestamp(self.clock.now().0 + LINK_CODE_MINUTES as i64 * 60_000),
                    },
                )
                .await?;
            return io
                .edit(
                    &route.chat,
                    ack,
                    Message::text(format!(
                        "Code: {code}\nApprove this code in the app: {APP}\nIt expires in {LINK_CODE_MINUTES} minutes. For a new code, send any message here again."
                    )),
                )
                .await;
        };
        self.remember(route, true).await?;
        let Some(&project) = self.cfg.projects.get(&route.chat) else {
            return io
                .edit(
                    &route.chat,
                    ack,
                    Message::text(format!("Choose a project for this chat in the app: {APP}")),
                )
                .await;
        };
        let caller = Self::caller(&identity);
        let id = command_id(self.kind, &route.bot, &input.event);
        // Persisted payloads preserve revisions/defaults across restart and replay.
        if let Some(intent) = self
            .store
            .get::<Intent>("commands", &id.to_string())
            .await?
        {
            if intent.identity != identity.id || intent.project != project {
                return Err(Error::Hub(ErrorCode::PermissionDenied));
            }
            return self.submit(io, route, ack, &caller, intent.envelope).await;
        }
        // The host journal remains the authority if connector state was restored
        // from an older backup. Never rebuild an already admitted command.
        if let Ok(QueryData::CommandStatus {
            receipt: Some(receipt),
            stage,
            result,
            error,
        }) = self
            .hub
            .query(&caller, Query::CommandStatus { command_id: id })
            .await
        {
            let result = result.map(|r| *r).or(receipt.result.clone());
            return self
                .finish(
                    io,
                    route,
                    ack,
                    &identity,
                    receipt,
                    stage,
                    result,
                    error.map(|e| e.code),
                )
                .await;
        }
        let snapshot = self.snapshot(&caller).await?;
        let mut reference = None;
        let mut expected_revision = None;
        let mut expires_at = None;
        let command = if let Some(action) = &input.action {
            let (key, choice) = action
                .split_once(':')
                .ok_or(Error::Hub(ErrorCode::InvalidRequest))?;
            let mut r = self
                .store
                .get::<Reference>("references", key)
                .await?
                .ok_or(Error::Hub(ErrorCode::Expired))?;
            self.check_reference(&r, &identity, &route_key, project, id)?;
            let body = match r.purpose {
                Purpose::Stop if choice == "stop" && caller.grants.tasks => {
                    visible_task(&snapshot, r.task, project)?;
                    Command::StopTask {
                        task_id: r.task,
                        force: false,
                    }
                }
                Purpose::Approval {
                    id: approval_id,
                    kind,
                } if caller.grants.approvals
                    && matches!(
                        kind,
                        ApprovalKind::Command
                            | ApprovalKind::FileEdit
                            | ApprovalKind::Network
                            | ApprovalKind::Tool
                    ) =>
                {
                    check_approval(&snapshot, approval_id, project, r.revision)?;
                    let decision = match choice {
                        "allow" => ApprovalDecision::Allow,
                        "deny" => ApprovalDecision::Deny { reason: None },
                        _ => return Err(Error::Hub(ErrorCode::InvalidRequest)),
                    };
                    Command::ResolveApproval {
                        approval_id,
                        decision,
                    }
                }
                _ => return Err(Error::Hub(ErrorCode::PermissionDenied)),
            };
            expected_revision = Some(r.revision);
            expires_at = Some(r.expires);
            r.used = Some(id);
            reference = Some((key.to_owned(), r));
            body
        } else {
            let text = input.text.trim();
            let (verb, args) = if input.explicit_answer || input.forwarded {
                ("", text)
            } else {
                text.split_once(char::is_whitespace).unwrap_or((text, ""))
            };
            match verb {
                "/new" if caller.grants.tasks => {
                    if args.trim().is_empty() {
                        return Err(Error::Hub(ErrorCode::InvalidRequest));
                    }
                    let p = snapshot
                        .projects
                        .iter()
                        .find(|p| p.project_id == project)
                        .ok_or(Error::Hub(ErrorCode::NotFound))?;
                    let Some(harness) = p.default_harness else {
                        return io
                            .edit(
                                &route.chat,
                                ack,
                                Message::text(
                                    "Choose a default harness for this project in the app first.",
                                ),
                            )
                            .await;
                    };
                    Command::CreateTask {
                        agent_chosen: None,
                        mode: None,
                        project_id: project,
                        kind: TaskKind::Coding,
                        title: None,
                        prompt: args.trim().into(),
                        bundle: None,
                        harness,
                        model: p.default_model.clone(),
                        effort: p.default_effort.clone(),
                        launch_policy: None,
                        branch: None,
                        mission: None,
                        target_id: None,
                        placement: TaskPlacement::StartNow,
                        start: None,
                        place: None,
                        browser: None,
                        github: None,
                        conversation: None,
                        access: None,
                        review: None,
                    }
                }
                "/status" | "/tasks" if caller.grants.tasks => {
                    let tasks = snapshot.tasks.iter().filter(|t| {
                        t.project_id == project
                            && (verb == "/tasks"
                                || !matches!(
                                    t.state,
                                    TaskState::Completed
                                        | TaskState::Archived
                                        | TaskState::Interrupted
                                ))
                    });
                    let lines: Vec<_> = tasks
                        .take(50)
                        .map(|t| {
                            let mut text = task_line(t, self.cfg.exposure);
                            crate::context_links::append(
                                &mut text,
                                &crate::context_links::task(self.hub.host_id(), t.task_id),
                            );
                            text
                        })
                        .collect();
                    return io
                        .edit(
                            &route.chat,
                            ack,
                            Message::text(if lines.is_empty() {
                                "No matching tasks.".into()
                            } else {
                                lines.join("\n")
                            }),
                        )
                        .await;
                }
                "/stop" if caller.grants.tasks => {
                    let task = self
                        .target(input, &identity, &snapshot, project, args.trim())
                        .await?;
                    let key = uuid::Uuid::new_v4().simple().to_string();
                    let r = Reference {
                        identity: identity.id,
                        route_key,
                        project,
                        task: task.task_id,
                        revision: task.revision,
                        expires: Timestamp(self.clock.now().0 + 600_000),
                        boot: self.hub.boot_epoch(),
                        purpose: Purpose::Stop,
                        used: None,
                    };
                    self.store.put("references", &key, &r).await?;
                    return io
                        .edit(
                            &route.chat,
                            ack,
                            Message {
                                text: format!("Stop task {}?", task.task_id),
                                thread: input.thread.clone(),
                                buttons: vec![Button {
                                    label: "Confirm stop",
                                    data: format!("{key}:stop"),
                                }],
                            },
                        )
                        .await;
                }
                _ => {
                    let binding = self.binding(input, &identity).await?;
                    if let Some(Binding::Approval(key)) = binding {
                        if (!input.explicit_answer && text.starts_with('/'))
                            || input.forwarded
                            || !caller.grants.approvals
                        {
                            return Err(Error::Hub(ErrorCode::InvalidRequest));
                        }
                        let mut r = self
                            .store
                            .get::<Reference>("references", &key)
                            .await?
                            .ok_or(Error::Hub(ErrorCode::Expired))?;
                        self.check_reference(&r, &identity, &route_key, project, id)?;
                        let Purpose::Approval {
                            id: approval_id,
                            kind: ApprovalKind::Question,
                        } = r.purpose
                        else {
                            return Err(Error::Hub(ErrorCode::Unsupported));
                        };
                        check_approval(&snapshot, approval_id, project, r.revision)?;
                        expected_revision = Some(r.revision);
                        expires_at = Some(r.expires);
                        r.used = Some(id);
                        reference = Some((key, r));
                        Command::ResolveApproval {
                            approval_id,
                            decision: ApprovalDecision::Answer {
                                text: text.into(),
                                answers: Vec::new(),
                            },
                        }
                    } else {
                        if input.explicit_answer {
                            return Err(Error::Hub(ErrorCode::InvalidRequest));
                        }
                        if !caller.grants.tasks
                            || (!input.forwarded
                                && text.starts_with('/')
                                && verb != "/steer"
                                && verb != "/reply")
                        {
                            return io.edit(&route.chat,ack,Message::text("Use /new, /tasks, /status, /stop or /steer. Reply to a task message to continue it.")).await;
                        }
                        // Discord slash commands provide an explicit UUID as their task option.
                        let (explicit, prompt) = if matches!(verb, "/steer" | "/reply") {
                            match args.trim().split_once(char::is_whitespace) {
                                Some((id, body)) if id.parse::<TaskId>().is_ok() => (id, body),
                                _ => ("", args.trim()),
                            }
                        } else {
                            ("", text)
                        };
                        if prompt.is_empty() {
                            return Err(Error::Hub(ErrorCode::InvalidRequest));
                        }
                        let task = self
                            .target(input, &identity, &snapshot, project, explicit)
                            .await?;
                        Command::SendMessage {
                            task_id: task.task_id,
                            text: prompt.into(),
                            bundle: None,
                            delivery: if verb == "/steer" {
                                MessageDelivery::Steer
                            } else {
                                MessageDelivery::NextTurn
                            },
                        }
                    }
                }
            }
        };
        let envelope = CommandEnvelope {
            command_id: id,
            issued_at: self.clock.now(),
            expected_revision,
            expires_at,
            expires_in_ms: None,
            body: command,
        };
        let mut writes = vec![(
            "commands",
            id.to_string(),
            serde_json::to_value(Intent {
                identity: identity.id,
                project,
                envelope: envelope.clone(),
            })
            .map_err(|_| Error::State)?,
        )];
        if let Some((key, r)) = reference {
            writes.push((
                "references",
                key,
                serde_json::to_value(r).map_err(|_| Error::State)?,
            ));
        }
        self.store.put_batch(writes).await?;
        self.submit(io, route, ack, &caller, envelope).await
    }
    fn routes_bucket(&self) -> &'static str {
        match self.kind {
            ChannelKind::Telegram => "telegram_routes",
            ChannelKind::Discord => "discord_routes",
        }
    }
    /// Remembers `route`, `linked` when someone is linked through it. A route
    /// nobody is linked through is kept only while its code can be approved,
    /// so strangers who message the bot don't pile up; a linked one stays.
    async fn remember(&self, route: &Route, linked: bool) -> Result<()> {
        let bucket = self.routes_bucket();
        let key = route.key(self.kind);
        let now = self.clock.now();
        let existing = self.store.get::<Remembered>(bucket, &key).await?;
        if linked && existing.as_ref().is_some_and(|r| r.linked) {
            return Ok(());
        }
        if existing.is_none() {
            self.make_room(bucket, now).await?;
        }
        self.store
            .put(
                bucket,
                &key,
                &Remembered {
                    route: route.clone(),
                    at: Some(now),
                    linked,
                },
            )
            .await
    }
    /// Forgets routes whose codes ran out without a link, then, if the table
    /// is still full, the oldest route nobody is linked through.
    async fn make_room(&self, bucket: &str, now: Timestamp) -> Result<()> {
        let mut routes = Vec::new();
        for mut r in self.store.list::<Remembered>(bucket).await? {
            let expired =
                r.at.is_none_or(|at| now.0.saturating_sub(at.0) >= UNLINKED_ROUTE_MS);
            if !r.linked && expired {
                let key = r.route.key(self.kind);
                if self.identity(&r.route).await.is_none() {
                    self.store.delete(bucket, &key).await?;
                    continue;
                }
                // The code was approved; the route is kept from now on.
                r.linked = true;
                self.store.put(bucket, &key, &r).await?;
            }
            routes.push(r);
        }
        if routes.len() >= MAX_ROUTES {
            // Strangers who message the bot must not crowd out new links. Make
            // room from the oldest routes; one with an active identity stays.
            routes.sort_by_key(|r| r.at);
            let mut stale = None;
            for r in routes {
                if self.identity(&r.route).await.is_none() {
                    stale = Some(r.route.key(self.kind));
                    break;
                }
            }
            let stale = stale.ok_or(Error::Hub(ErrorCode::LimitExceeded))?;
            self.store.delete(bucket, &stale).await?;
        }
        Ok(())
    }
    /// Approval controls are offered only while titles are shown. A connector
    /// that starts with minimal exposure withdraws those already in chats, so
    /// switching back to titles later doesn't bring them back either.
    pub async fn withdraw_approval_controls(&self) -> Result<()> {
        if self.cfg.exposure != Exposure::Minimal {
            return Ok(());
        }
        for (key, reference) in self.store.entries::<Reference>("references").await? {
            if matches!(reference.purpose, Purpose::Approval { .. }) {
                self.store.delete("references", &key).await?;
            }
        }
        Ok(())
    }
    fn check_reference(
        &self,
        r: &Reference,
        i: &ChannelIdentity,
        key: &str,
        project: ProjectId,
        id: CommandId,
    ) -> Result<()> {
        if r.identity != i.id || r.route_key != key || r.project != project {
            return Err(Error::Hub(ErrorCode::PermissionDenied));
        }
        if r.expires <= self.clock.now() || r.boot != self.hub.boot_epoch() {
            return Err(Error::Hub(ErrorCode::Expired));
        }
        if r.used.is_some_and(|used| used != id) {
            return Err(Error::Hub(ErrorCode::AlreadyResolved));
        }
        Ok(())
    }
    fn binding_key(&self, route: &Route, identity: ChannelIdentityId, message: &str) -> String {
        format!("{}:{identity}:{message}", route.key(self.kind))
    }
    async fn binding(&self, input: &Incoming, i: &ChannelIdentity) -> Result<Option<Binding>> {
        if let Some(message) = &input.reply {
            let binding = self
                .store
                .get("bindings", &self.binding_key(&input.route, i.id, message))
                .await?;
            if binding.is_some() {
                return Ok(binding);
            }
        }
        if let Some(thread) = &input.thread {
            return self
                .store
                .get("threads", &self.binding_key(&input.route, i.id, thread))
                .await;
        }
        Ok(None)
    }
    async fn target<'a>(
        &self,
        input: &Incoming,
        i: &ChannelIdentity,
        snapshot: &'a Snapshot,
        project: ProjectId,
        explicit: &str,
    ) -> Result<&'a Task> {
        let task = if !explicit.is_empty() {
            explicit
                .parse()
                .map_err(|_| Error::Hub(ErrorCode::InvalidRequest))?
        } else {
            match self.binding(input, i).await? {
                Some(Binding::Task(id)) => id,
                _ => return Err(Error::Hub(ErrorCode::InvalidRequest)),
            }
        };
        visible_task(snapshot, task, project)
    }
    async fn submit(
        &self,
        io: &dyn ChatIo,
        route: &Route,
        ack: &str,
        caller: &Caller,
        envelope: CommandEnvelope,
    ) -> Result<()> {
        let identity = self
            .identity(route)
            .await
            .ok_or(Error::Hub(ErrorCode::Unauthenticated))?;
        if caller.principal != Self::caller(&identity).principal {
            return Err(Error::Hub(ErrorCode::Unauthenticated));
        }
        let caller = Self::caller(&identity);
        let permitted = match &envelope.body {
            Command::CreateTask { .. } | Command::SendMessage { .. } | Command::StopTask { .. } => {
                caller.grants.tasks
            }
            Command::ResolveApproval { .. } => caller.grants.approvals,
            _ => false,
        };
        if !permitted {
            return Err(Error::Hub(ErrorCode::PermissionDenied));
        }
        let task = match &envelope.body {
            Command::SendMessage { task_id, .. } | Command::StopTask { task_id, .. } => {
                Some(*task_id)
            }
            _ => None,
        };
        let id = envelope.command_id;
        let receipt =
            tokio::time::timeout(Duration::from_secs(10), self.hub.submit(&caller, envelope))
                .await
                .ok();
        let status = tokio::time::timeout(
            Duration::from_secs(5),
            self.hub
                .query(&caller, Query::CommandStatus { command_id: id }),
        )
        .await
        .ok()
        .and_then(std::result::Result::ok);
        let (receipt, stage, result, error) = match status {
            Some(QueryData::CommandStatus {
                receipt: r,
                stage,
                result,
                error,
            }) => (
                r.or(receipt),
                stage,
                result.map(|r| *r),
                error.map(|e| e.code),
            ),
            _ => (receipt, None, None, None),
        };
        if let Some(receipt) = receipt {
            let result = result.or(receipt.result.clone());
            if let Some(task) = task {
                self.store
                    .put(
                        "bindings",
                        &self.binding_key(route, identity.id, ack),
                        &Binding::Task(task),
                    )
                    .await?;
            }
            self.finish(io, route, ack, &identity, receipt, stage, result, error)
                .await
        } else {
            io.edit(&route.chat, ack, Message::text(UNKNOWN)).await
        }
    }
    #[allow(clippy::too_many_arguments)]
    async fn finish(
        &self,
        io: &dyn ChatIo,
        route: &Route,
        ack: &str,
        identity: &ChannelIdentity,
        receipt: CommandReceipt,
        stage: Option<CommandStage>,
        result: Option<CommandResult>,
        error: Option<ErrorCode>,
    ) -> Result<()> {
        let error = error.or(receipt.error.as_ref().map(|e| e.code));
        let mut text = if error == Some(ErrorCode::OutcomeUnknown)
            || matches!(
                stage,
                Some(CommandStage::Uncertain | CommandStage::ReconciliationRequired)
            ) {
            UNKNOWN.into()
        } else if let Some(code) = error {
            format!("Request refused ({code:?}). Check the app.")
        } else if receipt.status == ReceiptStatus::Rejected || stage == Some(CommandStage::Failed) {
            "Request failed — check the app".into()
        } else {
            "Accepted by the computer".to_owned()
        };
        if let Some(CommandResult::TaskCreated { task_id, .. }) = result {
            self.store
                .put(
                    "bindings",
                    &self.binding_key(route, identity.id, ack),
                    &Binding::Task(task_id),
                )
                .await?;
            text.push_str(&format!(
                "\nTask {task_id}\nReply to this message to continue."
            ));
        }
        if let Some(CommandResult::TaskCreated { task_id, .. }) = result {
            crate::context_links::append(
                &mut text,
                &crate::context_links::task(self.hub.host_id(), task_id),
            );
        }
        io.edit(&route.chat, ack, Message::text(text)).await
    }

    /// Owns one grant-filtered subscription per known linked identity. Live text
    /// is discarded. Reattach on lag; gaps prompt a snapshot and app review.
    pub async fn notifications(
        self: Arc<Self>,
        io: Arc<dyn ChatIo>,
        bot: String,
        shutdown: CancellationToken,
        status: &Status<'_>,
    ) -> Result<()> {
        let mut active: HashMap<String, (ChannelIdentityId, Subscription, u64)> = HashMap::new();
        let mut retries: HashMap<String, (Instant, u64)> = HashMap::new();
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! { _=shutdown.cancelled()=>return Ok(()),_=tick.tick()=>{} }
            self.link_replies(io.as_ref(), &bot, status).await?;
            let routes = self.store.list::<Route>(self.routes_bucket()).await?;
            for route in routes
                .into_iter()
                .filter(|r| r.bot == bot && self.cfg.projects.contains_key(&r.chat))
            {
                let key = route.key(self.kind);
                let Some(identity) = self.identity(&route).await.filter(|i| i.grants.tasks) else {
                    active.remove(&key);
                    retries.remove(&key);
                    status.recovered(&key);
                    continue;
                };
                if active
                    .get(&key)
                    .is_some_and(|(id, _, _)| *id != identity.id)
                {
                    active.remove(&key);
                }
                if retries
                    .get(&key)
                    .is_some_and(|(at, _)| self.clock.monotonic() < *at)
                {
                    continue;
                }
                let cursor_key = format!("{key}:{}", identity.id);
                let result: Result<()> = async {
                    if !active.contains_key(&key) {
                        let sub = self
                            .subscribe_notices(io.as_ref(), &route, &identity, &cursor_key)
                            .await?;
                        let head = sub.head;
                        active.insert(key.clone(), (identity.id, sub, head));
                    }
                    let mut lagged = false;
                    if let Some((_, sub, cursor)) = active.get_mut(&key) {
                        for _ in 0..128 {
                            match sub.live.try_recv() {
                                Ok(Outbound::Event(event)) if event.seq > *cursor => {
                                    skip_refused(
                                        self.notify(io.as_ref(), &route, &identity, &event).await,
                                    )?;
                                    self.store.put("cursors", &cursor_key, &event.seq).await?;
                                    *cursor = event.seq;
                                }
                                Ok(_) => {}
                                Err(tokio::sync::broadcast::error::TryRecvError::Empty) => break,
                                Err(_) => {
                                    lagged = true;
                                    break;
                                }
                            }
                        }
                    }
                    if lagged {
                        active.remove(&key);
                    }
                    Ok(())
                }
                .await;
                match result {
                    Ok(()) => {
                        retries.remove(&key);
                        status.recovered(&key);
                    }
                    Err(error) => {
                        active.remove(&key);
                        let limited = match error {
                            Error::RateLimited(wait) => Some(wait),
                            _ => None,
                        };
                        match error.failure(Scope::Item) {
                            Failure::Fatal => return Err(error),
                            Failure::Shutdown => return Ok(()),
                            // A chat the platform is limiting isn't failing; it's
                            // tried again when its limit ends.
                            Failure::Transient if limited.is_some() => {}
                            Failure::Transient => status.retrying(&key),
                            Failure::Refused => {
                                // A refused subscription has no event to consume. Save the
                                // head and defer reattachment, as with a failed route.
                                skip_refused(Err(error))?;
                                self.store
                                    .put("cursors", &cursor_key, &self.hub.journal_head())
                                    .await?;
                                status.recovered(&key);
                            }
                        }
                        let delay = retries
                            .get(&key)
                            .map_or(1, |(_, delay)| (delay * 2).min(60));
                        let wait = limited.unwrap_or(Duration::from_secs(delay));
                        retries.insert(key, (self.clock.monotonic() + wait, delay));
                    }
                }
            }
        }
    }
    /// Tells each person who asked this bot for a link code whether the owner
    /// approved or refused it.
    async fn link_replies(&self, io: &dyn ChatIo, bot: &str, status: &Status<'_>) -> Result<()> {
        for link in self.store.list::<LinkCode>("link_codes").await? {
            if link.route.bot != bot {
                continue;
            }
            let key = format!("link:{}", link.code);
            let text = match self.hub.channel_link_outcome(self.kind, &link.code).await {
                Some(ChannelLinkOutcome::Approved) => format!(
                    "Code {} was approved in the app. This chat is now linked to the computer.",
                    link.code
                ),
                Some(ChannelLinkOutcome::Refused) => format!(
                    "Code {} was refused in the app, so this chat isn’t linked.",
                    link.code
                ),
                Some(ChannelLinkOutcome::Waiting) => continue,
                // A code the computer doesn't hold is kept until it would have expired.
                None if link.expires > self.clock.now() => continue,
                None => {
                    self.store.delete("link_codes", &link.code).await?;
                    // A reply that failed before the code expired is no longer retried.
                    status.recovered(&key);
                    continue;
                }
            };
            let sent = io
                .send(
                    &link.route.chat,
                    Message {
                        thread: link.thread.clone(),
                        ..Message::text(text)
                    },
                )
                .await
                .map(|_| ());
            match skip_refused(sent) {
                Ok(()) => {
                    self.store.delete("link_codes", &link.code).await?;
                    status.recovered(&key);
                }
                Err(error) => match error.failure(Scope::Item) {
                    // Tried again on a later tick, once the chat's limit ends.
                    Failure::Transient if matches!(error, Error::RateLimited(_)) => {}
                    Failure::Transient => status.retrying(&key),
                    Failure::Shutdown => return Ok(()),
                    _ => return Err(error),
                },
            }
        }
        Ok(())
    }
    async fn subscribe_notices(
        &self,
        io: &dyn ChatIo,
        route: &Route,
        identity: &ChannelIdentity,
        cursor_key: &str,
    ) -> Result<Subscription> {
        let after = self.store.get::<u64>("cursors", cursor_key).await?;
        let mut sub = self
            .hub
            .subscribe(&Self::caller(identity), after)
            .await
            .map_err(|e| Error::Hub(e.code))?;
        match std::mem::replace(&mut sub.backlog, Backlog::Events(Vec::new())) {
            Backlog::Events(events) => {
                for event in events {
                    if after.is_some_and(|seq| event.seq <= seq) {
                        continue;
                    }
                    skip_refused(self.notify(io, route, identity, &event).await)?;
                    self.store.put("cursors", cursor_key, &event.seq).await?;
                }
            }
            Backlog::Gap { .. } => {
                skip_refused(
                    io.send(
                        &route.chat,
                        Message::text("Notification history has a gap — check the app."),
                    )
                    .await
                    .map(|_| ()),
                )?;
            }
        }
        // Snapshot repairs pending/resolved approvals after restart or retention gaps.
        match self.snapshot(&Self::caller(identity)).await {
            Ok(snapshot) => {
                for approval in &snapshot.approvals {
                    skip_refused(
                        self.approval_notice(io, route, identity, &snapshot, approval)
                            .await,
                    )?;
                }
            }
            Err(error) => skip_refused(Err(error))?,
        }
        self.store.put("cursors", cursor_key, &sub.head).await?;
        Ok(sub)
    }
    pub async fn notify(
        &self,
        io: &dyn ChatIo,
        route: &Route,
        identity: &ChannelIdentity,
        event: &EventEnvelope,
    ) -> Result<()> {
        let Some(current) = self.identity(route).await else {
            return Ok(());
        };
        if current.id != identity.id || !current.grants.tasks {
            return Ok(());
        }
        let Some(&project) = self.cfg.projects.get(&route.chat) else {
            return Ok(());
        };
        // Filter with the event and current grants before loading retained history.
        match &event.event {
            Event::ApprovalUpserted { approval }
                if current.grants.approvals && approval.project_id == project =>
            {
                let snapshot = self.snapshot(&Self::caller(&current)).await?;
                if let Some(latest) = snapshot
                    .approvals
                    .iter()
                    .find(|a| a.approval_id == approval.approval_id)
                {
                    self.approval_notice(io, route, &current, &snapshot, latest)
                        .await?;
                }
            }
            // A task another agent asked answers that agent, not the person.
            Event::TaskUpserted { task }
                if task.project_id == project
                    && task.asked_by.is_none()
                    && matches!(
                        task.state,
                        TaskState::Completed
                            | TaskState::Failed
                            | TaskState::ReconciliationRequired
                    ) =>
            {
                let snapshot = self.snapshot(&Self::caller(&current)).await?;
                if visible_task(&snapshot, task.task_id, project).is_err() {
                    return Ok(());
                }
                let key = format!(
                    "{}:{}:{}:{:?}:{:?}",
                    route.key(self.kind),
                    identity.id,
                    task.task_id,
                    task.current_run,
                    task.state
                );
                if self.store.get::<bool>("notified", &key).await?.is_none() {
                    let mut text = task_line(task, self.cfg.exposure);
                    if self.cfg.exposure == Exposure::Titles
                        && task.state == TaskState::Completed
                        && let Some(summary) = snapshot
                            .runs
                            .iter()
                            .find(|r| Some(r.run_id) == task.current_run)
                            .and_then(|r| r.outcome.as_ref())
                            .and_then(|o| o.summary.as_ref())
                    {
                        text.push('\n');
                        text.extend(summary.chars().take(500));
                    }
                    crate::context_links::append(
                        &mut text,
                        &crate::context_links::task(self.hub.host_id(), task.task_id),
                    );
                    let id = io.send(&route.chat, Message::text(text)).await?;
                    self.store
                        .put(
                            "bindings",
                            &self.binding_key(route, identity.id, &id),
                            &Binding::Task(task.task_id),
                        )
                        .await?;
                    self.store.put("notified", &key, &true).await?;
                }
            }
            Event::AttentionUpserted { item }
                if item.approval_id.is_none()
                    && item.task_id.is_some()
                    && item.resolved_at.is_none()
                    && !item.quiet
                    && !matches!(item.kind, AttentionKind::Completed | AttentionKind::Failure) =>
            {
                let snapshot = self.snapshot(&Self::caller(&current)).await?;
                if let Some(latest) = snapshot
                    .attention
                    .iter()
                    .find(|a| a.attention_id == item.attention_id)
                    && latest.resolved_at.is_none()
                    && let Some(task_id) = latest.task_id
                    && visible_task(&snapshot, task_id, project).is_ok()
                {
                    let key = format!(
                        "{}:{}:attention:{}",
                        route.key(self.kind),
                        identity.id,
                        latest.attention_id
                    );
                    if self.store.get::<bool>("notified", &key).await?.is_none() {
                        let mut text = if self.cfg.exposure == Exposure::Titles {
                            format!(
                                "Needs attention: {}",
                                latest.title.chars().take(200).collect::<String>()
                            )
                        } else {
                            format!("Task {task_id}: needs attention.")
                        };
                        crate::context_links::append(
                            &mut text,
                            &crate::context_links::attention(
                                self.hub.host_id(),
                                latest.attention_id,
                            ),
                        );
                        io.send(&route.chat, Message::text(text)).await?;
                        self.store.put("notified", &key, &true).await?;
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }
    async fn approval_notice(
        &self,
        io: &dyn ChatIo,
        route: &Route,
        i: &ChannelIdentity,
        snapshot: &Snapshot,
        a: &Approval,
    ) -> Result<()> {
        if !i.grants.approvals || self.cfg.projects.get(&route.chat) != Some(&a.project_id) {
            return Ok(());
        }
        let task = visible_task(snapshot, a.task_id, a.project_id)?;
        let key = format!("{}:{}:{}", route.key(self.kind), i.id, a.approval_id);
        let existing = self.store.get::<Notice>("notices", &key).await?;
        if existing
            .as_ref()
            .is_some_and(|n| n.revision == a.revision && n.state == a.state)
        {
            return Ok(());
        }
        let reference = existing
            .as_ref()
            .map(|n| n.reference.clone())
            .unwrap_or_else(|| uuid::Uuid::new_v4().simple().to_string());
        let mut message =
            Message::text(format!("Task {}: needs approval. Open the app.", a.task_id));
        if a.state != ApprovalState::Pending {
            if existing.is_none() {
                return Ok(());
            }
            let who = match a.resolved_by {
                Some(Principal::Device { device_id }) => {
                    format!("device {device_id} in the app")
                }
                Some(Principal::Channel { identity_id }) => {
                    format!("chat identity {identity_id}")
                }
                Some(Principal::LocalOwner) => "owner on the computer".into(),
                Some(Principal::System) => "computer".into(),
                None => "recorded source unavailable".into(),
            };
            message.text = format!(
                "Task {}: approval {}. Recorded by {who}.",
                a.task_id,
                approval_state(a.state, a.outcome)
            );
        } else if self.cfg.exposure == Exposure::Titles {
            message.text = format!(
                "Task {} — {}\nRequested effect: {}\nScope: {}",
                a.task_id, task.title, a.title, a.scope
            );
            if let Some(detail) = &a.detail {
                message.text.push('\n');
                message.text.push_str(detail);
            }
            if let Some(raw) = &a.raw_request {
                message.text.push('\n');
                message.text.push_str(raw);
            }
            // Do not invite a decision against truncated/unknown/supervised effects.
            let rendered_units = match self.kind {
                ChannelKind::Telegram => message.text.encode_utf16().count(),
                ChannelKind::Discord => crate::discord::discord_text(&message.text)
                    .encode_utf16()
                    .count(),
            };
            if rendered_units > 1800
                || matches!(
                    a.kind,
                    ApprovalKind::Other
                        | ApprovalKind::ComputerUse
                        | ApprovalKind::LoginUse
                        | ApprovalKind::LoginCode
                        | ApprovalKind::GithubPost
                )
            {
                message = Message::text(format!(
                    "Task {}: this approval requires the app.",
                    a.task_id
                ));
            } else {
                let r = Reference {
                    identity: i.id,
                    route_key: route.key(self.kind),
                    project: a.project_id,
                    task: a.task_id,
                    revision: u64::from(a.revision),
                    expires: a
                        .expires_at
                        .unwrap_or(Timestamp(self.clock.now().0 + 600_000)),
                    boot: self.hub.boot_epoch(),
                    purpose: Purpose::Approval {
                        id: a.approval_id,
                        kind: a.kind,
                    },
                    used: None,
                };
                self.store.put("references", &reference, &r).await?;
                if a.kind == ApprovalKind::Question {
                    message.text.push_str("\nReply to this message with your answer (Discord guild: /answer with this message ID).");
                } else {
                    message.buttons = vec![
                        Button {
                            label: "Allow",
                            data: format!("{reference}:allow"),
                        },
                        Button {
                            label: "Deny",
                            data: format!("{reference}:deny"),
                        },
                    ];
                }
            }
        }
        crate::context_links::append(
            &mut message.text,
            &crate::context_links::approval(self.hub.host_id(), a.approval_id),
        );
        let id = if let Some(notice) = existing {
            io.edit(&route.chat, &notice.message, message).await?;
            notice.message
        } else {
            io.send(&route.chat, message).await?
        };
        self.store
            .put(
                "bindings",
                &self.binding_key(route, i.id, &id),
                &Binding::Approval(reference.clone()),
            )
            .await?;
        self.store
            .put(
                "notices",
                &key,
                &Notice {
                    reference,
                    message: id,
                    revision: a.revision,
                    state: a.state,
                },
            )
            .await
    }
}
fn check_approval(
    snapshot: &Snapshot,
    id: ApprovalId,
    project: ProjectId,
    revision: u64,
) -> Result<()> {
    let a = snapshot
        .approvals
        .iter()
        .find(|a| a.approval_id == id && a.project_id == project)
        .ok_or(Error::Hub(ErrorCode::NotFound))?;
    if a.state != ApprovalState::Pending {
        return Err(Error::Hub(ErrorCode::AlreadyResolved));
    }
    if u64::from(a.revision) != revision {
        return Err(Error::Hub(ErrorCode::StaleRevision));
    }
    Ok(())
}
fn visible_task(snapshot: &Snapshot, id: TaskId, project: ProjectId) -> Result<&Task> {
    snapshot
        .tasks
        .iter()
        .find(|t| t.task_id == id && t.project_id == project)
        .ok_or(Error::Hub(ErrorCode::NotFound))
}
fn task_line(task: &Task, exposure: Exposure) -> String {
    let state = match task.state {
        TaskState::Accepted => "accepted",
        TaskState::Dispatching => "dispatching",
        TaskState::Working => "working",
        TaskState::AwaitingDecision => "needs attention",
        TaskState::Interrupted => "interrupted",
        TaskState::Completed => "finished",
        TaskState::Failed => "failed",
        TaskState::ReconciliationRequired => "needs reconciliation",
        TaskState::Archived => "archived",
    };
    let title = if exposure == Exposure::Titles {
        format!(" — {}", task.title.chars().take(120).collect::<String>())
    } else {
        String::new()
    };
    format!(
        "Task {}: {state}{title}{}",
        task.task_id,
        if task.attention_count > 0 {
            " (needs attention)"
        } else {
            ""
        }
    )
}
fn approval_state(
    state: ApprovalState,
    outcome: Option<protocol::model::ApprovalOutcome>,
) -> &'static str {
    match state {
        ApprovalState::Pending => "pending",
        ApprovalState::Allowed => "allowed",
        ApprovalState::Denied => "denied",
        ApprovalState::Expired => "expired",
        ApprovalState::ResolvedElsewhere => match outcome {
            Some(protocol::model::ApprovalOutcome::Allowed) => "resolved elsewhere: allowed",
            Some(protocol::model::ApprovalOutcome::Denied) => "resolved elsewhere: denied",
            None => "resolved elsewhere (decision not supplied by the computer)",
        },
        ApprovalState::Cancelled => "cancelled",
    }
}

#[cfg(test)]
mod approval_outcome_tests {
    use super::*;
    use protocol::model::ApprovalOutcome;

    #[test]
    fn resolved_elsewhere_displays_reported_outcome_or_honest_unknown() {
        assert_eq!(
            approval_state(
                ApprovalState::ResolvedElsewhere,
                Some(ApprovalOutcome::Allowed)
            ),
            "resolved elsewhere: allowed"
        );
        assert_eq!(
            approval_state(
                ApprovalState::ResolvedElsewhere,
                Some(ApprovalOutcome::Denied)
            ),
            "resolved elsewhere: denied"
        );
        assert_eq!(
            approval_state(ApprovalState::ResolvedElsewhere, None),
            "resolved elsewhere (decision not supplied by the computer)"
        );
        assert_eq!(approval_state(ApprovalState::Allowed, None), "allowed");
        assert_eq!(approval_state(ApprovalState::Denied, None), "denied");
    }
}
