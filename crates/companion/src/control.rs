// SPDX-License-Identifier: Apache-2.0
use crate::{
    Error, Result,
    error::invalid,
    person_only,
    server::{ServerDeps, current_caller},
    wire::{self, Wire},
};
use axum::extract::ws::Message;
use futures_util::StreamExt;
use protocol::{
    ClientFrame, Command, CommandEnvelope, CommandReceipt, ErrorCode, ProtocolError, QueryEnvelope,
    QueryResult, ReceiptStatus, ServerFrame, time::Timestamp,
};
use services::api::{Backlog, Caller, Outbound};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};
use tokio::{
    sync::{mpsc, watch},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

struct AbortTask(JoinHandle<()>);
impl AbortTask {
    async fn stop(mut self) {
        self.0.abort();
        let _ = (&mut self.0).await;
    }
}
impl Drop for AbortTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}
struct EventFrame {
    generation: u64,
    frame: ServerFrame,
}
type Failure = Option<(u64, ProtocolError)>;

/// Journal entries read at a time while replaying a backlog.
const BACKLOG_PAGE: usize = 256;
/// How often a connected device's last-seen time is refreshed.
pub(crate) const SEEN_EVERY: std::time::Duration = std::time::Duration::from_secs(60);
/// One request executes at a time, with at most this many waiting behind it.
// Leave room for the app's burst of history reads after reconnect.
const REQUEST_QUEUE: usize = 128;

enum Request {
    Command(Arc<CommandEnvelope>, Timestamp),
    Query(QueryEnvelope),
}

// Bounded by the request queue plus its one active worker. Matching retries
// share the original response by command ID, with no extra slots or waiters.
type PendingCommands = Mutex<HashMap<protocol::CommandId, Arc<CommandEnvelope>>>;

async fn pending_retry(
    commands: &PendingCommands,
    priority: &mpsc::Sender<Message>,
    command: &CommandEnvelope,
) -> Result<bool> {
    let original = commands
        .lock()
        .map_err(|_| invalid("pending commands unavailable"))?
        .get(&command.command_id)
        .cloned();
    let Some(original) = original else {
        return Ok(false);
    };
    if !original.matches_request(command) {
        // Do not journal a rejection against the original pending identity.
        reply(
            priority,
            ServerFrame::Receipt(protocol::CommandReceipt {
                command_id: command.command_id,
                status: protocol::ReceiptStatus::Rejected,
                seq: None,
                result: None,
                error: Some(ProtocolError::new(
                    ErrorCode::Conflict,
                    "Command id is already bound to another request",
                )),
            }),
        )
        .await?;
    }
    Ok(true)
}

/// These commands admit an intervention without preparing files. The hub still
/// checks authorization, command identity and revisions, then runs the effect.
fn intervention(command: &Command) -> bool {
    matches!(
        command,
        Command::StopTask { .. }
            | Command::StopChild { .. }
            | Command::StopObservedSession { .. }
            | Command::PauseInput { .. }
            | Command::RequestHumanLease { .. }
            | Command::ReleaseLease { .. }
            | Command::TakeOver { .. }
            | Command::ReturnToAgent { .. }
            | Command::StartPrivateStep { .. }
            | Command::EndPrivateStep { .. }
            | Command::WithdrawObservation { .. }
            | Command::LockDesktop
    )
}

async fn reply(priority: &mpsc::Sender<Message>, response: ServerFrame) -> Result<()> {
    let message = match response {
        ServerFrame::QueryResult(result) => wire::query_result(result)?,
        response => wire::json(&response)?,
    };
    priority.send(message).await.map_err(|_| Error::Closed)
}

async fn enqueue(
    deps: &ServerDeps,
    caller: &Caller,
    requests: &mpsc::Sender<Request>,
    commands: &PendingCommands,
    priority: &mpsc::Sender<Message>,
    request: Request,
) -> Result<()> {
    let command = match &request {
        Request::Command(command, _) => Some(command.clone()),
        Request::Query(_) => None,
    };
    if let Err(error) = requests.try_send(request) {
        // Never stop reading interventions or heartbeats to wait for queue space.
        let error_response = ProtocolError::new(
            ErrorCode::LimitExceeded,
            "Control request queue is full; retry when pending requests finish",
        );
        let response = match error.into_inner() {
            Request::Command(command, _) => ServerFrame::Receipt(
                deps.hub
                    .reject_command(caller, (*command).clone(), error_response)
                    .await,
            ),
            Request::Query(query) => ServerFrame::QueryResult(QueryResult {
                query_id: query.query_id,
                result: Err(error_response),
            }),
        };
        reply(priority, response).await?;
    } else if let Some(command) = command {
        commands
            .lock()
            .map_err(|_| invalid("pending commands unavailable"))?
            .insert(command.command_id, command);
    }
    Ok(())
}
/// A command over this connection. One asked from inside a run (Ophio's own
/// command line, `started`) is refused when it would read or change what the
/// person disabled in Ophio.
pub(crate) async fn routed_command(
    hub: &dyn services::Hub,
    caller: &Caller,
    command: protocol::command::CommandEnvelope,
    received: protocol::time::Timestamp,
    started: bool,
) -> protocol::command::CommandReceipt {
    let refused = match started {
        true => hub.agent_refusal(&command.body).await,
        false => None,
    };
    match refused {
        Some(error) => hub.reject_command(caller, command, error).await,
        None => hub.submit_received(caller, command, received).await,
    }
}

/// A query over this connection. One asked from inside a run is answered
/// with what the person disabled in Ophio left out.
pub(crate) async fn routed_query(
    hub: &dyn services::Hub,
    caller: &Caller,
    query: protocol::Query,
    started: bool,
) -> std::result::Result<protocol::QueryData, ProtocolError> {
    match started {
        true => hub.agent_query(caller, query).await,
        false => hub.query(caller, query).await,
    }
}

/// The event pump never waits for the socket or the command receiver. A full
/// queue closes with a typed resubscription cursor rather than losing events.
/// Replies wait their turn instead: a phone that asks many things at once while
/// it reads slowly is answered in order, and one that stops reading meets the
/// write timeout.
pub(crate) async fn run(
    wire: &mut Wire,
    deps: &ServerDeps,
    caller: Caller,
    shutdown: &CancellationToken,
    local: bool,
    started: bool,
) -> Result<()> {
    let (priority, mut urgent) = mpsc::channel::<Message>(32);
    let (requests, mut pending) = mpsc::channel::<Request>(REQUEST_QUEUE);
    let commands = PendingCommands::default();
    let (events, mut event_rx) = mpsc::channel::<EventFrame>(deps.limits.event_queue);
    let (failure, mut failed) = watch::channel::<Failure>(None);
    let generation = Arc::new(AtomicU64::new(0));
    let sent_seq = Arc::new(AtomicU64::new(0));
    let reader = &mut wire.reader;
    let writer = &mut wire.writer;
    let mut subscription: Option<AbortTask> = None;
    let mut quit = false;
    let result = {
        let read = async {
            let mut last_seen = deps.clock.monotonic();
            let mut marked_seen = last_seen;
            let mut ticker = tokio::time::interval(deps.limits.revocation_poll);
            loop {
                let message = tokio::select! {
                    biased;
                    _ = shutdown.cancelled() => return Ok(()),
                    _ = ticker.tick() => {
                        let now = deps.clock.monotonic();
                        if !local && now.duration_since(last_seen) >= deps.limits.heartbeat { return Err(Error::Timeout); }
                        let current = current_caller(deps, &caller).await?;
                        if current.grants != caller.grants { return Err(ProtocolError::new(ErrorCode::PermissionDenied, "device grants changed; reconnect").into()); }
                        // A long-lived connection keeps its device's "last seen" current.
                        if let protocol::auth::Principal::Device { device_id } = &caller.principal
                            && now.duration_since(marked_seen) >= SEEN_EVERY
                        {
                            marked_seen = now;
                            deps.hub.device_seen(device_id).await;
                        }
                        continue;
                    },
                    incoming = reader.next() => incoming.ok_or(Error::Closed)??,
                };
                last_seen = deps.clock.monotonic();
                match message {
                    Message::Close(_) => return Ok(()),
                    Message::Pong(_) => continue,
                    Message::Ping(bytes) => {
                        priority
                            .send(Message::Pong(bytes))
                            .await
                            .map_err(|_| Error::Closed)?;
                        continue;
                    }
                    Message::Binary(_) => return Err(invalid("control frames must be JSON")),
                    _ => {}
                }
                if local
                    && let Message::Text(text) = &message
                    && let Ok(request) = serde_json::from_str::<crate::local::OwnerRequest>(text)
                {
                    if started && let Some(what) = person_only::owner_request(&request) {
                        return Err(person_only::refusal(what).into());
                    }
                    let response = match request {
                        crate::local::OwnerRequest::OpenPairing => {
                            crate::local::OwnerResponse::PairingOffer(deps.pairing_offer().await?)
                        }
                        crate::local::OwnerRequest::WaitingPairing => {
                            crate::local::OwnerResponse::PairingWaiting(
                                deps.hub.waiting_pairing(&caller).await?,
                            )
                        }
                        crate::local::OwnerRequest::AnswerPairing { code, confirm } => {
                            crate::local::OwnerResponse::PairingAnswered(
                                deps.hub.answer_pairing(&caller, &code, confirm).await?,
                            )
                        }
                        crate::local::OwnerRequest::Quit => {
                            quit = true;
                            crate::local::OwnerResponse::Quitting
                        }
                        request => {
                            let owner = deps
                                .owner
                                .clone()
                                .ok_or(Error::Unsupported("Owner runtime is not configured"))?;
                            let (progress, mut updates) = mpsc::channel(16);
                            let cancel = CancellationToken::new();
                            let _cancel_on_disconnect = cancel.clone().drop_guard();
                            let token = cancel.clone();
                            let mut operation = tokio::spawn(async move {
                                owner.request_progress(request, progress, token).await
                            });
                            let mut progress_open = true;
                            loop {
                                tokio::select! {
                                    result = &mut operation => {
                                        // Drain queued progress before the terminal response. Keep
                                        // one reply slot reserved even if the owner reads slowly.
                                        while let Ok(update) = updates.try_recv() {
                                            if priority.capacity() > 1 { let _ = priority.try_send(wire::json(&update)?); }
                                        }
                                        break result.map_err(|_| invalid("Owner operation failed"))??;
                                    },
                                    update = updates.recv(), if progress_open => if let Some(update) = update {
                                        // Progress may be coalesced when the terminal is slow.
                                        if priority.capacity() > 1 { let _ = priority.try_send(wire::json(&update)?); }
                                    } else { progress_open = false; },
                                    _ = shutdown.cancelled() => { cancel.cancel(); let _ = operation.await; return Ok(()); },
                                    _ = reader.next() => { cancel.cancel(); let _ = operation.await; return Err(Error::Closed); },
                                }
                            }
                        }
                    };
                    priority
                        .send(wire::json(&response)?)
                        .await
                        .map_err(|_| Error::Closed)?;
                    if quit {
                        return Ok(());
                    }
                    continue;
                }
                let caller = current_caller(deps, &caller).await?;
                let frame: ClientFrame = wire::parse(message)?;
                let response = match frame {
                    ClientFrame::Command(command)
                        if started && person_only::command(&command.body).is_some() =>
                    {
                        Some(ServerFrame::Receipt(CommandReceipt {
                            command_id: command.command_id,
                            status: ReceiptStatus::Rejected,
                            seq: None,
                            result: None,
                            error: Some(person_only::refusal(
                                person_only::command(&command.body).expect("checked command"),
                            )),
                        }))
                    }
                    ClientFrame::Query(query)
                        if started && person_only::query(&query.query).is_some() =>
                    {
                        Some(ServerFrame::QueryResult(QueryResult {
                            query_id: query.query_id,
                            result: Err(person_only::refusal(
                                person_only::query(&query.query).expect("checked query"),
                            )),
                        }))
                    }
                    ClientFrame::Command(command)
                        if pending_retry(&commands, &priority, &command).await? =>
                    {
                        None
                    }
                    ClientFrame::Command(command) if intervention(&command.body) => Some(
                        ServerFrame::Receipt(deps.hub.submit(&caller, *command).await),
                    ),
                    ClientFrame::Command(command) => {
                        enqueue(
                            deps,
                            &caller,
                            &requests,
                            &commands,
                            &priority,
                            Request::Command(Arc::from(command), deps.clock.now()),
                        )
                        .await?;
                        None
                    }
                    ClientFrame::Query(query) => {
                        enqueue(
                            deps,
                            &caller,
                            &requests,
                            &commands,
                            &priority,
                            Request::Query(query),
                        )
                        .await?;
                        None
                    }
                    ClientFrame::Ping { nonce } => Some(ServerFrame::Pong { nonce }),
                    ClientFrame::Input(input) => {
                        Some(ServerFrame::InputAck(deps.hub.input(&caller, input).await))
                    }
                    ClientFrame::RenewLease {
                        lease_id,
                        generation,
                    } => {
                        deps.hub.renew_lease(&caller, lease_id, generation).await?;
                        None
                    }
                    ClientFrame::Subscribe(request) => {
                        if let Some(task) = subscription.take() {
                            task.stop().await;
                        }
                        let current = generation.fetch_add(1, Ordering::SeqCst) + 1;
                        sent_seq.store(request.after_seq.unwrap_or(0), Ordering::SeqCst);
                        let deps = deps.clone();
                        let events = events.clone();
                        let failure = failure.clone();
                        subscription = Some(AbortTask(tokio::spawn(async move {
                            if let Err(error) = event_pump(
                                &deps,
                                &caller,
                                request.after_seq,
                                current,
                                events,
                                started,
                            )
                            .await
                            {
                                failure.send_replace(Some((current, error.protocol())));
                            }
                        })));
                        None
                    }
                    ClientFrame::Hello(_) | ClientFrame::Authenticate(_) => {
                        return Err(invalid("connection already authenticated"));
                    }
                };
                if let Some(response) = response {
                    reply(&priority, response).await?;
                }
            }
        };
        let work = async {
            while let Some(request) = pending.recv().await {
                // Recheck at execution: this request may have waited behind I/O.
                let caller = current_caller(deps, &caller).await?;
                let response = match request {
                    Request::Command(command, received) => {
                        let receipt = routed_command(
                            &*deps.hub,
                            &caller,
                            (*command).clone(),
                            received,
                            started,
                        )
                        .await;
                        commands
                            .lock()
                            .map_err(|_| invalid("pending commands unavailable"))?
                            .remove(&command.command_id);
                        ServerFrame::Receipt(receipt)
                    }
                    Request::Query(query) => ServerFrame::QueryResult(QueryResult {
                        query_id: query.query_id,
                        result: routed_query(&*deps.hub, &caller, query.query, started).await,
                    }),
                };
                reply(&priority, response).await?;
            }
            Ok(())
        };
        let write = async {
            let mut closed_generation = None;
            loop {
                tokio::select! {
                    biased;
                    _ = shutdown.cancelled() => return Ok(()),
                    message = urgent.recv() => wire::send(writer, message.ok_or(Error::Closed)?, deps.limits.write).await?,
                    result = failed.changed() => {
                        result.map_err(|_| Error::Closed)?;
                        let failure = failed.borrow_and_update().clone();
                        if let Some((epoch, error)) = failure && epoch == generation.load(Ordering::SeqCst) && closed_generation != Some(epoch) {
                            closed_generation = Some(epoch);
                            wire::send_json(writer, &ServerFrame::SubscriptionClosed { after_seq: sent_seq.load(Ordering::SeqCst), reason: error }, deps.limits.write).await?;
                        }
                    },
                    frame = event_rx.recv() => {
                        // Frames already queued go out in the same write, each
                        // still its own message, until something this loop takes
                        // first is waiting.
                        let mut next = frame.ok_or(Error::Closed)?;
                        let mut delivered = None;
                        loop {
                            let EventFrame { generation: epoch, frame } = next;
                            if epoch == generation.load(Ordering::SeqCst) && closed_generation != Some(epoch) {
                                let seq = match &frame { ServerFrame::Event(event) => Some(event.seq), ServerFrame::CaughtUp {head} => Some(*head), _ => None };
                                let message = match &frame {
                                    // A journal can hold an event recorded before its
                                    // text was bounded. It can never be sent, so the
                                    // phone reloads a snapshot past it rather than
                                    // reconnecting into it until retention drops it.
                                    ServerFrame::Event(event) => match wire::fitted(&frame)? {
                                        Some(message) => message,
                                        None => {
                                            closed_generation = Some(epoch);
                                            let requested_from = delivered.filter(|(e, _)| *e == epoch).map_or_else(|| sent_seq.load(Ordering::SeqCst), |(_, seq)| seq);
                                            wire::json(&ServerFrame::JournalGap { requested_from, oldest_retained: event.seq + 1 })?
                                        }
                                    },
                                    frame => wire::json(frame)?,
                                };
                                wire::feed(writer, message, deps.limits.write).await?;
                                if let Some(seq) = seq && closed_generation != Some(epoch) { delivered = Some((epoch, seq)); }
                            }
                            if shutdown.is_cancelled() || !urgent.is_empty() || failed.has_changed().unwrap_or(true) { break; }
                            let Ok(queued) = event_rx.try_recv() else { break };
                            next = queued;
                        }
                        wire::flush(writer, deps.limits.write).await?;
                        if let Some((epoch, seq)) = delivered && epoch == generation.load(Ordering::SeqCst) { sent_seq.store(seq, Ordering::SeqCst); }
                    },
                }
            }
        };
        // Keep request execution scoped to the connection. Disconnect, revocation,
        // shutdown or a failed writer drops both queued and unfinished requests.
        tokio::select! { result = read => result, result = write => result, result = work => result }
    };
    if let Some(task) = subscription.take() {
        task.stop().await;
    }
    if quit {
        // The answer goes out first: stopping closes this connection.
        while let Ok(message) = urgent.try_recv() {
            if wire::send(writer, message, deps.limits.write)
                .await
                .is_err()
            {
                break;
            }
        }
        shutdown.cancel();
    }
    result
}

/// Which event feed a connection reads. One opened from inside a run reads
/// the agent feed, which withholds the owner's report inbox, even when it
/// holds the owner's credentials.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EventFeed {
    Owner,
    Agent,
}
fn event_feed(started: bool) -> EventFeed {
    if started {
        EventFeed::Agent
    } else {
        EventFeed::Owner
    }
}

async fn event_pump(
    deps: &ServerDeps,
    caller: &Caller,
    mut after: Option<u64>,
    generation: u64,
    output: mpsc::Sender<EventFrame>,
    started: bool,
) -> Result<()> {
    let emit = |frame| {
        output
            .try_send(EventFrame { generation, frame })
            .map_err(|_| {
                Error::from(ProtocolError::new(
                    ErrorCode::LimitExceeded,
                    "event consumer fell behind; resubscribe from the delivered cursor",
                ))
            })
    };
    loop {
        // Attach to live events at the head, then replay the backlog up to it
        // a page at a time. A resubscription after an overflow reads only the
        // page it can send, never the whole remaining history.
        let feed = event_feed(started);
        let mut subscription = match feed {
            EventFeed::Agent => deps.hub.agent_subscribe(caller, None).await?,
            EventFeed::Owner => deps.hub.subscribe(caller, None).await?,
        };
        if let Some(mut cursor) = after {
            let mut last = cursor;
            loop {
                let (backlog, through) = match feed {
                    EventFeed::Agent => {
                        deps.hub
                            .agent_backlog(caller, cursor, subscription.head, BACKLOG_PAGE)
                            .await?
                    }
                    EventFeed::Owner => {
                        deps.hub
                            .backlog(caller, cursor, subscription.head, BACKLOG_PAGE)
                            .await?
                    }
                };
                match backlog {
                    Backlog::Gap {
                        requested_from,
                        oldest_retained,
                    } => {
                        emit(ServerFrame::JournalGap {
                            requested_from,
                            oldest_retained,
                        })?;
                        return Ok(());
                    }
                    Backlog::Events(backlog) => {
                        for event in backlog {
                            if event.seq > subscription.head || event.seq <= last {
                                continue;
                            }
                            last = event.seq;
                            emit(ServerFrame::Event((*event).clone()))?;
                            // Let command receipt work run even during a large replay.
                            tokio::task::yield_now().await;
                        }
                    }
                }
                if through >= subscription.head {
                    break;
                }
                cursor = through;
            }
        }
        after = Some(subscription.head.max(after.unwrap_or(0)));
        emit(ServerFrame::CaughtUp {
            head: subscription.head,
        })?;
        loop {
            match subscription.live.recv().await {
                Ok(Outbound::Event(event)) if event.seq > after.unwrap_or(0) => {
                    after = Some(event.seq);
                    emit(ServerFrame::Event((*event).clone()))?;
                }
                Ok(Outbound::Live(event)) => emit(ServerFrame::Live((*event).clone()))?,
                Ok(_) => {}
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => break,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return Ok(()),
            }
        }
    }
}

#[cfg(test)]
mod disabled_routing {
    use super::{routed_command, routed_query};
    use protocol::{
        Command, Query, QueryData, ReceiptStatus,
        command::{CommandEnvelope, DownloadSource},
        disabled::DisabledKind,
        launchers::CatalogKind,
    };
    use services::{Caller, CompanionHub, Hub, LauncherDirs, testing::FakeClock};
    use std::sync::Arc;

    /// What an agent asks through Ophio's own command line leaves out what
    /// the person disabled; the person's own phone still sees it.
    #[tokio::test]
    async fn a_run_s_own_command_line_is_kept_from_what_is_disabled() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("project");
        let skill = project.join(".claude/skills/probe/SKILL.md");
        std::fs::create_dir_all(skill.parent().unwrap()).unwrap();
        std::fs::write(
            &skill,
            "---\nname: probe\ndescription: A probe.\n---\n\nReply OK.\n",
        )
        .unwrap();
        let hub = CompanionHub::builder()
            .data_dir(dir.path().join("data"))
            .clock(Arc::new(FakeClock::default()))
            .launchers(LauncherDirs {
                claude_home: Some(project.join(".claude")),
                ..LauncherDirs::default()
            })
            .build()
            .await
            .unwrap();
        let owner = Caller::local_owner();
        let host_path = std::fs::canonicalize(&skill).unwrap();
        let receipt = hub
            .submit(
                &owner,
                CommandEnvelope::new(Command::SetDisabledInOphio {
                    kind: DisabledKind::Skill,
                    host_path: host_path.to_string_lossy().into_owned(),
                    disabled: true,
                }),
            )
            .await;
        assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
        let probe = |data: QueryData| match data {
            QueryData::Catalog { catalog } => catalog
                .entries
                .iter()
                .any(|e| e.kind == CatalogKind::Skill && e.id == "probe"),
            other => panic!("{other:?}"),
        };
        let catalog = || Query::Catalog { project_id: None };
        assert!(probe(
            routed_query(&hub, &owner, catalog(), false).await.unwrap()
        ));
        assert!(!probe(
            routed_query(&hub, &owner, catalog(), true).await.unwrap()
        ));

        let added = hub
            .submit(
                &owner,
                CommandEnvelope::new(Command::AddProject {
                    host_path: project.to_string_lossy().into_owned(),
                    name: Some("probe".into()),
                }),
            )
            .await;
        let project_id = match added.result {
            Some(protocol::command::CommandResult::ProjectAdded { project_id, .. }) => project_id,
            other => panic!("{other:?}"),
        };
        let download = || {
            CommandEnvelope::new(Command::CreateDownload {
                source: DownloadSource::ProjectFile {
                    project_id,
                    path: ".claude/skills/probe/SKILL.md".into(),
                },
                shown_in_task: None,
            })
        };
        let now = protocol::time::Timestamp(0);
        let refused = routed_command(&hub, &owner, download(), now, true).await;
        assert_eq!(refused.status, ReceiptStatus::Rejected);
        assert!(
            refused
                .error
                .as_ref()
                .is_some_and(|e| e.message.contains("disabled this in Øphio")),
            "{refused:?}"
        );
        let person = routed_command(&hub, &owner, download(), now, false).await;
        assert!(
            !person
                .error
                .as_ref()
                .is_some_and(|e| e.message.contains("disabled this in Øphio")),
            "{person:?}"
        );
    }

    #[test]
    fn a_connection_started_in_a_run_reads_the_agent_feed() {
        // The agent feed withholds the owner's report inbox; a run's
        // connection must never land on the owner's.
        use super::{EventFeed, event_feed};
        assert_eq!(event_feed(true), EventFeed::Agent);
        assert_eq!(event_feed(false), EventFeed::Owner);
    }
}
