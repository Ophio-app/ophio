// SPDX-License-Identifier: Apache-2.0
use super::*;

/// The next message a task that had stopped is to get, waiting in Next up
/// until its turn there comes: task id → the message.
pub(crate) const CONTINUATION: &str = "queued_continuation";
/// The markup going with a waiting continuation: task id → the bundle. Kept
/// apart so a continuation saved before markup could wait still reads.
pub(crate) const CONTINUATION_BUNDLE: &str = "queued_continuation_bundle";

/// Whether `task` waits in Next up to start: it never ran, or it stopped
/// with its next message waiting there.
pub(crate) fn waits_to_start(r: &store::Read<'_>, task: &Task) -> store::Result<bool> {
    Ok(match task.state {
        TaskState::Accepted => true,
        TaskState::Interrupted | TaskState::Completed | TaskState::Failed => {
            r.get::<String>(CONTINUATION, task.task_id)?.is_some()
        }
        _ => false,
    })
}

impl CompanionHub {
    /// Hydrate saved requests from the task store, including older queue records.
    /// Reading eligibility never admits work or grants start permission.
    pub(crate) fn queue_view(&self, project_id: ProjectId) -> Result<QueueView, ProtocolError> {
        self.write(|tx| {
            let mut queue: QueueView = tx.read().require("queue", project_id)?;
            self.refresh_queue(tx, &mut queue)?;
            Ok(queue)
        })
    }

    pub(crate) fn held_message(
        &self,
        tx: &store::Write<'_>,
        task_id: TaskId,
        seq: u64,
    ) -> store::Result<(RunId, MessageRef)> {
        let task: Task = tx.read().require("task", task_id)?;
        let catalog: Vec<protocol::capability::HarnessInfo> =
            tx.read().get("meta", "harnesses")?.unwrap_or_default();
        let capability = catalog
            .iter()
            .find(|h| h.kind == task.harness)
            .and_then(|h| h.capabilities.cancel_queued_message.as_ref())
            .ok_or_else(|| unsupported("Queued message withdrawal"))?;
        domain::availability(capability)?;
        let run_id = task
            .current_run
            .ok_or_else(|| error(ErrorCode::Conflict, "This task has no held messages"))?;
        let messages: Vec<InFlightMessage> = tx
            .read()
            .get("message_delivery", run_id)?
            .unwrap_or_default();
        let reference = messages
            .iter()
            .find(|m| m.seq == seq && m.state == DeliveryState::QueuedForBoundary)
            .and_then(|m| m.message_ref)
            .ok_or_else(|| {
                error(
                    ErrorCode::Conflict,
                    "This message is no longer held. It may already have reached the agent.",
                )
            })?;
        Ok((run_id, reference))
    }

    /// Withdraws a held message; `revoked` when the chat that sent it lost
    /// its access, rather than someone choosing to cancel it.
    pub(crate) async fn cancel_message(
        &self,
        task_id: TaskId,
        seq: u64,
        revoked: bool,
    ) -> Result<(), ProtocolError> {
        let (run_id, _) = self.write(|tx| self.held_message(tx, task_id, seq))?;
        let lock = self.message_lock(run_id);
        let _guard = lock.lock().await;
        let (_, reference) = self.write(|tx| self.held_message(tx, task_id, seq))?;
        let control = self
            .inner
            .runs
            .lock()
            .await
            .get(&run_id)
            .cloned()
            .ok_or_else(|| {
                error(
                    ErrorCode::Conflict,
                    "The agent is no longer holding this message",
                )
            })?;
        tokio::time::timeout(Duration::from_secs(5), control.cancel_message(reference))
            .await
            .map_err(|_| {
                error(
                    ErrorCode::OutcomeUnknown,
                    "Message withdrawal has not been confirmed",
                )
            })?
            .map_err(|e| error(e.code(), e.to_string()))?;
        self.write(|tx| {
            let mut messages: Vec<InFlightMessage> = tx
                .read()
                .get("message_delivery", run_id)?
                .unwrap_or_default();
            messages.retain(|m| m.message_ref != Some(reference));
            tx.put("message_delivery", run_id, &messages)?;
            tx.emit(Event::TimelineAppended {
                task_id,
                item: TimelineItem {
                    seq: 0,
                    at: self.inner.clock.now(),
                    run_id: Some(run_id),
                    item: TimelineBody::Delivery {
                        message_seq: seq,
                        state: DeliveryState::Cancelled,
                        withdrawn_by_stop: false,
                        withdrawn_by_revocation: revoked,
                    },
                },
            })?;
            Ok(())
        })
    }

    /// Withdraws every message a revoked chat left held for an agent's next
    /// turn, so nothing it sent reaches an agent after its access ends.
    pub(crate) async fn withdraw_revoked(
        &self,
        identity_id: ChannelIdentityId,
    ) -> Result<(), ProtocolError> {
        let sender = protocol::auth::Principal::Channel { identity_id };
        // A message the chat is sending now is recorded before its run's lock
        // is released, and one it sends later is refused. Wait for those.
        let runs: Vec<RunId> = self.inner.runs.lock().await.keys().copied().collect();
        for run in runs {
            drop(self.message_lock(run).lock().await);
        }
        let (held, mut kept) = self.read(|r| {
            let (mut held, mut kept) = (Vec::new(), false);
            for (run_id, messages) in r.entries::<Vec<InFlightMessage>>("message_delivery")? {
                let run: Run = r.require("run", &run_id)?;
                for message in messages {
                    let Some(reference) = message.message_ref else {
                        continue;
                    };
                    if !r
                        .command(reference.0)?
                        .is_some_and(|c| c.principal == sender)
                    {
                        continue;
                    }
                    if message.undispatched {
                        // Admission saved the words, but nothing reached the
                        // adapter. Access is already revoked, so the send worker
                        // will fail authorization and settle its saved message.
                        continue;
                    }
                    if message.state == DeliveryState::QueuedForBoundary {
                        held.push((run.task_id, message.seq));
                    } else {
                        // Already handed to the agent, or not known either way.
                        kept = true;
                    }
                }
            }
            Ok((held, kept))
        })?;
        for (task_id, seq) in held {
            if let Err(e) = self.cancel_message(task_id, seq, true).await {
                tracing::warn!(error = %e.message, "held chat message was not withdrawn");
                kept = true;
            }
        }
        if kept {
            return Err(error(
                ErrorCode::Conflict,
                "This chat’s access was revoked, but a message it queued couldn’t be withdrawn and may still reach the agent. Check the task in Chat.",
            ));
        }
        Ok(())
    }

    /// Adds `task` to its project's Next up at `position`, the end when
    /// none, to ask `request` of it.
    pub(crate) fn enqueue(
        &self,
        tx: &mut store::Write<'_>,
        caller: &Caller,
        task: &Task,
        position: Option<u32>,
        authorization: QueueAuthorization,
        request: QueuedRequest,
    ) -> store::Result<QueueItemId> {
        let now = self.inner.clock.now();
        let mut q: QueueView = tx.read().require("queue", task.project_id)?;
        let position = position.unwrap_or(q.items.len() as u32) as usize;
        if position > q.items.len() {
            return Err(error(
                ErrorCode::InvalidRequest,
                "Queue position is outside the queue",
            )
            .into());
        }
        // Adding work keeps what was already let start by itself, and one to
        // start by itself resumes a paused Next up: queued work starts as
        // soon as it's ready. A review's verifier is Ophio carrying on by
        // itself, so it waits for a paused Next up like the rest.
        crate::runs::bump_queue_revision(tx, &mut q)?;
        let verifier =
            (task.review.as_ref()).is_some_and(|r| r.role == protocol::review::ReviewRole::Verify);
        let item_id = QueueItemId::new();
        let expires_at = (authorization == QueueAuthorization::AutoStart).then_some(
            protocol::time::Timestamp(now.0 + crate::runs::QUEUE_PERMIT_MS),
        );
        if authorization == QueueAuthorization::AutoStart {
            if !verifier {
                q.paused = false;
                q.pause_reason = None;
            }
            tx.put(
                "queue_permit",
                item_id,
                &crate::runs::QueuePermit {
                    task_revision: task.revision,
                    queue_revision: q.revision,
                    remaining: 1,
                    principal: caller.principal,
                    expires_at,
                },
            )?;
        }
        q.items.insert(
            position,
            QueueItem {
                item_id,
                task_id: task.task_id,
                position: position as u32,
                depends_on: Vec::new(),
                authorization,
                state: QueueItemState::Waiting,
                added_by: caller.principal,
                added_at: now,
                request: Some(request),
                authorization_expires_at: expires_at,
            },
        );
        for (i, item) in q.items.iter_mut().enumerate() {
            item.position = i as u32;
        }
        self.refresh_queue(tx, &mut q)?;
        tx.put("queue", task.project_id, &q)?;
        crate::runs::sync_queue_attention(tx, &q, now)?;
        tx.emit(Event::QueueChanged { queue: q })?;
        Ok(item_id)
    }

    /// Puts the person's next message for a task that has stopped in Next
    /// up, to go to its agent as a new turn when its turn there comes.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn queue_continuation(
        &self,
        tx: &mut store::Write<'_>,
        caller: &Caller,
        task_id: TaskId,
        text: &str,
        bundle: Option<&AnnotationBundle>,
        authorization: QueueAuthorization,
        expected: Option<u64>,
    ) -> store::Result<()> {
        let text = text.trim();
        // Markup alone is a complete message, as it is sent to a task.
        if text.is_empty() && bundle.is_none() {
            return Err(error(ErrorCode::InvalidRequest, "Write the message first").into());
        }
        let mut task: Task = tx.read().require("task", task_id)?;
        domain::revision(expected, task.revision)?;
        if !matches!(
            task.state,
            TaskState::Interrupted | TaskState::Completed | TaskState::Failed
        ) {
            return Err(error(
                ErrorCode::Conflict,
                "Only a task that has stopped waits in Next up for its next message. Send it to the task instead.",
            )
            .into());
        }
        if task.kind == TaskKind::ComputerUse && !caller.grants.computer_use {
            return Err(error(
                ErrorCode::PermissionDenied,
                "This device needs the AI computer use permission",
            )
            .into());
        }
        let queue: QueueView = tx.read().require("queue", task.project_id)?;
        if queue
            .items
            .iter()
            .any(|i| i.task_id == task_id && i.is_pending())
        {
            return Err(error(
                ErrorCode::Conflict,
                "This task is already waiting in Next up. Edit its message there.",
            )
            .into());
        }
        let now = self.inner.clock.now();
        task.revision += 1;
        task.updated_at = now;
        tx.put("task", task_id, &task)?;
        tx.emit(Event::TaskUpserted { task: task.clone() })?;
        tx.put(CONTINUATION, task_id, &text.to_owned())?;
        let bundle = bundle.map(|b| Box::new(b.clone()));
        if let Some(reviewed) = bundle.as_deref() {
            tx.put(CONTINUATION_BUNDLE, task_id, reviewed)?;
            crate::results::link_bundle(tx, task_id, None, reviewed)?;
        }
        self.enqueue(
            tx,
            caller,
            &task,
            None,
            authorization,
            QueuedRequest {
                prompt: text.to_owned(),
                bundle: bundle.clone(),
            },
        )?;
        let seq = tx.emit(Event::TimelineAppended {
            task_id,
            item: TimelineItem {
                seq: 0,
                at: now,
                run_id: None,
                item: TimelineBody::UserMessage {
                    text: text.to_owned(),
                    bundle,
                    delivery: DeliveryState::HostAccepted,
                    carried_plan: None,
                },
            },
        })?;
        self.track_start_message(tx, task_id, None, seq)
    }

    pub(crate) fn edit_queue_prompt(
        &self,
        tx: &mut store::Write<'_>,
        item_id: QueueItemId,
        prompt: &str,
        expected: Option<u64>,
    ) -> store::Result<()> {
        if prompt.trim().is_empty() {
            return Err(error(
                ErrorCode::InvalidRequest,
                "Write the request before saving it",
            )
            .into());
        }
        let mut queue = queue_for(tx, item_id)?;
        domain::revision(expected, queue.revision)?;
        let item = queue
            .items
            .iter()
            .find(|i| i.item_id == item_id)
            .expect("located item");
        if !item.is_pending() {
            return Err(error(ErrorCode::Conflict, "This task has already started").into());
        }
        let mut task: Task = tx.read().require("task", item.task_id)?;
        if !waits_to_start(&tx.read(), &task)? {
            return Err(error(
                ErrorCode::Conflict,
                "Only a task waiting to start can be edited",
            )
            .into());
        }
        // A task that has run keeps the request it started with; what waits
        // is only its next message.
        let continues = task.state != TaskState::Accepted;
        let bundle = if continues {
            tx.put(CONTINUATION, task.task_id, &prompt.to_owned())?;
            tx.read().get(CONTINUATION_BUNDLE, task.task_id)?
        } else {
            let (_, bundle): (String, Option<Box<AnnotationBundle>>) =
                tx.read().require("prompt", task.task_id)?;
            tx.put("prompt", task.task_id, &(prompt.to_owned(), bundle.clone()))?;
            bundle
        };
        for item in [
            TimelineBody::Note {
                text: "Queued prompt updated".into(),
            },
            TimelineBody::UserMessage {
                text: prompt.to_owned(),
                bundle,
                delivery: DeliveryState::HostAccepted,
                carried_plan: None,
            },
        ] {
            let is_message = matches!(item, TimelineBody::UserMessage { .. });
            let seq = tx.emit(Event::TimelineAppended {
                task_id: task.task_id,
                item: TimelineItem {
                    seq: 0,
                    at: self.inner.clock.now(),
                    run_id: None,
                    item,
                },
            })?;
            if is_message {
                self.track_start_message(tx, task.task_id, None, seq)?;
            }
        }
        task.revision += 1;
        task.updated_at = self.inner.clock.now();
        if let Some(mission) = task.mission.as_mut().filter(|_| !continues) {
            mission.goal = prompt.to_owned();
        }
        tx.put("task", task.task_id, &task)?;
        tx.emit(Event::TaskUpserted { task })?;
        // It, and what waits on it, ask again; the rest keep their go-ahead.
        for item_id in domain::queue::withdraw(&mut queue, &[item_id]) {
            tx.delete("queue_permit", item_id)?;
        }
        crate::runs::bump_queue_revision(tx, &mut queue)?;
        self.refresh_queue(tx, &mut queue)?;
        tx.put("queue", queue.project_id, &queue)?;
        crate::runs::sync_queue_attention(tx, &queue, self.inner.clock.now())?;
        tx.emit(Event::QueueChanged { queue })?;
        Ok(())
    }

    /// A legacy resume command must not bypass a queued task's dependencies.
    pub(crate) fn check_pending_start(
        &self,
        tx: &store::Write<'_>,
        task: &Task,
        explicit_start: bool,
    ) -> store::Result<()> {
        let mut queue: QueueView = tx.read().require("queue", task.project_id)?;
        let Some(item) = queue
            .items
            .iter()
            .find(|i| i.task_id == task.task_id && i.is_pending())
            .cloned()
        else {
            return Ok(());
        };
        self.refresh_queue(tx, &mut queue)?;
        if queue.paused && !explicit_start {
            return Err(error(
                ErrorCode::Conflict,
                "Next up is paused. Resume it before starting a task.",
            )
            .into());
        }
        if queue
            .items
            .iter()
            .find(|i| i.is_pending())
            .map(|i| i.item_id)
            != Some(item.item_id)
        {
            return Err(error(
                ErrorCode::Conflict,
                "An earlier task is still waiting in Next up",
            )
            .into());
        }
        if let Some(QueueItem {
            state: QueueItemState::Blocked { reason },
            ..
        }) = queue.items.iter().find(|i| i.item_id == item.item_id)
        {
            return Err(error(ErrorCode::Conflict, reason).into());
        }
        Ok(())
    }

    pub(crate) fn start_queue_item(
        &self,
        tx: &mut store::Write<'_>,
        caller: &Caller,
        envelope: &CommandEnvelope,
        item_id: QueueItemId,
        task_revision: u64,
    ) -> store::Result<()> {
        let queue = queue_for(tx, item_id)?;
        domain::revision(envelope.expected_revision, queue.revision)?;
        let item = queue
            .items
            .iter()
            .find(|i| i.item_id == item_id)
            .expect("located item");
        if !item.is_pending() {
            return Err(error(ErrorCode::Conflict, "This task has already started").into());
        }
        let mut task: Task = tx.read().require("task", item.task_id)?;
        domain::revision(Some(task_revision), task.revision)?;
        if !waits_to_start(&tx.read(), &task)? {
            return Err(error(
                ErrorCode::Conflict,
                "This task is no longer waiting to start",
            )
            .into());
        }
        if task.kind == TaskKind::ComputerUse && !caller.grants.computer_use {
            return Err(error(
                ErrorCode::PermissionDenied,
                "This device needs the AI computer use permission",
            )
            .into());
        }
        self.check_pending_start(tx, &task, true)?;
        self.new_run(tx, &mut task, envelope.command_id, None, None, false)?;
        tx.put("task", task.task_id, &task)?;
        tx.emit(Event::TaskUpserted { task })?;
        Ok(())
    }
}

fn queue_for(tx: &store::Write<'_>, item_id: QueueItemId) -> store::Result<QueueView> {
    tx.read()
        .list::<QueueView>("queue")?
        .into_iter()
        .find(|q| q.items.iter().any(|i| i.item_id == item_id))
        .ok_or_else(|| error(ErrorCode::NotFound, "Queue item not found").into())
}
