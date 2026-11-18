// SPDX-License-Identifier: Apache-2.0
//! Next-turn messages own their recipient; they never retarget a running agent.
use super::*;
use queue_completion::AgentChoice;

#[cfg(test)]
#[path = "chat_message_queue_tests.rs"]
mod tests;

const QUEUED: &str = "chat_turn_queue";
const COMMAND: &str = "chat_turn_command";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct QueuedTurn {
    command_id: CommandId,
    task_id: TaskId,
    run_id: RunId,
    seq: u64,
    principal: protocol::auth::Principal,
    choice: AgentChoice,
    conversation: bool,
    mode: TaskMode,
    text: String,
    bundle: Option<Box<AnnotationBundle>>,
}

impl CompanionHub {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn admit_chat_turn(
        &self,
        tx: &mut store::Write<'_>,
        caller: &Caller,
        command_id: CommandId,
        task: &Task,
        text: &str,
        bundle: Option<&AnnotationBundle>,
        choice: AgentChoice,
    ) -> store::Result<()> {
        let run_id = task
            .current_run
            .ok_or_else(|| error(ErrorCode::Conflict, "No active run"))?;
        if stopped(&tx.read(), run_id)? {
            return Err(error(
                ErrorCode::Conflict,
                "This task is stopping; send your message after it stops",
            )
            .into());
        }
        if let Some(bundle) = bundle {
            crate::results::link_bundle(tx, task.task_id, Some(run_id), bundle)?;
        }
        // What the agent has written so far is shown before this message,
        // as it is for a message handed to the live agent.
        let run: Run = tx.read().require("run", run_id)?;
        self.partial_boundary(tx, &run)?;
        let seq = tx.emit(Event::TimelineAppended {
            task_id: task.task_id,
            item: TimelineItem {
                seq: 0,
                at: self.inner.clock.now(),
                run_id: Some(run_id),
                item: TimelineBody::UserMessage {
                    text: text.into(),
                    bundle: bundle.cloned().map(Box::new),
                    delivery: DeliveryState::QueuedForBoundary,
                    carried_plan: None,
                },
            },
        })?;
        crate::turned_from_chat::record_message_origin(
            tx,
            task.task_id,
            seq,
            self.is_from_person(command_id),
        )?;
        tx.put(
            QUEUED,
            command_id,
            &QueuedTurn {
                command_id,
                task_id: task.task_id,
                run_id,
                seq,
                principal: caller.principal,
                choice,
                conversation: task.conversation,
                mode: task.mode,
                text: text.into(),
                bundle: bundle.cloned().map(Box::new),
            },
        )?;
        // Retain this marker after draining: a late send worker must never
        // mistake an already-started queued message for native steering.
        tx.put(COMMAND, command_id, &true)
    }

    pub(crate) fn has_chat_turn(
        &self,
        r: &store::Read<'_>,
        task_id: TaskId,
    ) -> store::Result<bool> {
        Ok(r.list::<QueuedTurn>(QUEUED)?
            .iter()
            .any(|message| message.task_id == task_id))
    }

    pub(crate) fn is_chat_turn_command(
        &self,
        command_id: CommandId,
    ) -> Result<bool, ProtocolError> {
        self.read(|r| Ok(r.get::<bool>(COMMAND, command_id)?.unwrap_or(false)))
    }

    pub(crate) fn fail_chat_turn_command(
        &self,
        tx: &mut store::Write<'_>,
        command_id: CommandId,
    ) -> store::Result<()> {
        fail_undispatched_chat_turn(tx, command_id, self.inner.clock.now())
    }

    pub(crate) fn held_chat_turn(
        &self,
        r: &store::Read<'_>,
        task_id: TaskId,
        seq: u64,
    ) -> store::Result<Option<(RunId, MessageRef)>> {
        Ok(r.list::<QueuedTurn>(QUEUED)?
            .into_iter()
            .find(|m| m.task_id == task_id && m.seq == seq)
            .map(|m| (m.run_id, MessageRef(m.command_id))))
    }

    pub(crate) fn cancel_chat_turn(
        &self,
        tx: &mut store::Write<'_>,
        task_id: TaskId,
        seq: u64,
        revoked: bool,
    ) -> store::Result<bool> {
        let message = tx
            .read()
            .list::<QueuedTurn>(QUEUED)?
            .into_iter()
            .find(|m| m.task_id == task_id && m.seq == seq);
        if let Some(message) = message {
            self.settle_chat_turn(tx, &message, DeliveryState::Cancelled, false, revoked)?;
            return Ok(true);
        }
        Ok(false)
    }

    pub(crate) fn cancel_chat_turns(
        &self,
        tx: &mut store::Write<'_>,
        task_id: TaskId,
        stopped: bool,
    ) -> store::Result<()> {
        for message in tx
            .read()
            .list::<QueuedTurn>(QUEUED)?
            .into_iter()
            .filter(|m| m.task_id == task_id)
        {
            self.settle_chat_turn(
                tx,
                &message,
                if stopped {
                    DeliveryState::Cancelled
                } else {
                    DeliveryState::Failed
                },
                stopped,
                false,
            )?;
        }
        Ok(())
    }

    pub(crate) fn withdraw_chat_turns(
        &self,
        tx: &mut store::Write<'_>,
        principal: protocol::auth::Principal,
    ) -> store::Result<()> {
        for message in tx
            .read()
            .list::<QueuedTurn>(QUEUED)?
            .into_iter()
            .filter(|m| m.principal == principal)
        {
            self.settle_chat_turn(tx, &message, DeliveryState::Cancelled, false, true)?;
        }
        Ok(())
    }

    fn settle_chat_turn(
        &self,
        tx: &mut store::Write<'_>,
        message: &QueuedTurn,
        state: DeliveryState,
        stopped: bool,
        revoked: bool,
    ) -> store::Result<()> {
        settle_chat_turn(tx, message, state, stopped, revoked, self.inner.clock.now())
    }

    /// Called inside queue advancement's transaction. One turn per task is
    /// admitted, in message order, with current authority and readiness.
    pub(crate) fn advance_chat_turns(
        &self,
        tx: &mut store::Write<'_>,
    ) -> store::Result<Vec<(TaskId, RunId)>> {
        let mut messages = tx.read().list::<QueuedTurn>(QUEUED)?;
        messages.sort_by_key(|m| m.seq);
        let mut visited = HashSet::new();
        let mut starts = Vec::new();
        for message in messages {
            if tx
                .read()
                .get::<QueuedTurn>(QUEUED, message.command_id)?
                .is_none()
            {
                continue;
            }
            if !visited.insert(message.task_id) {
                continue;
            }
            let mut task: Task = tx.read().require("task", message.task_id)?;
            let original = tx.read().command(message.command_id)?;
            let allowed = Self::current_caller(
                &tx.read(),
                &Caller {
                    principal: message.principal,
                    grants: protocol::auth::Grants::owner(),
                },
            )
            .is_ok_and(|caller| caller.grants.tasks);
            if !allowed {
                self.withdraw_chat_turns(tx, message.principal)?;
                visited.remove(&message.task_id);
                continue;
            }
            if original.as_ref().is_none_or(|c| {
                matches!(
                    c.stage,
                    CommandStage::Failed
                        | CommandStage::Uncertain
                        | CommandStage::ReconciliationRequired
                )
            }) || task.conversation != message.conversation
                || task.mode != message.mode
                || matches!(
                    task.state,
                    TaskState::Archived
                        | TaskState::Failed
                        | TaskState::Interrupted
                        | TaskState::ReconciliationRequired
                )
            {
                self.cancel_chat_turns(tx, message.task_id, false)?;
                continue;
            }
            if task.state != TaskState::Completed {
                continue;
            }
            let previous: Run = tx.read().require(
                "run",
                task.current_run
                    .ok_or_else(|| error(ErrorCode::Conflict, "No previous chat turn"))?,
            )?;
            if still_finishing(&previous) {
                continue;
            }
            if tx
                .read()
                .get::<bool>("stopped", previous.run_id)?
                .unwrap_or(false)
            {
                self.cancel_chat_turns(tx, message.task_id, true)?;
                continue;
            }
            // Admission is not permission to bypass the send worker's
            // current authority and deadline checks.
            if original
                .as_ref()
                .is_none_or(|c| c.stage != CommandStage::Observed)
            {
                continue;
            }
            let id = CommandId::new();
            let started = tx.attempt(|tx| {
                self.validate_agent_choice(&tx.read(), &task, &message.choice)?;
                if task.harness != message.choice.harness {
                    crate::carry_on::withdraw_for_agent_switch(
                        tx,
                        task.task_id,
                        message.choice.harness,
                        self.inner.clock.now(),
                    )?;
                    task = tx.read().require("task", message.task_id)?;
                }
                message.choice.apply(&mut task);
                let run = self.new_run(
                    tx,
                    &mut task,
                    id,
                    Some(message.text.clone()),
                    message.bundle.clone(),
                    false,
                )?;
                self.track_start_message(tx, task.task_id, Some(run), message.seq)?;
                crate::proposals::person_turn(tx, run)?;
                let envelope = CommandEnvelope::new(Command::ResumeTask {
                    observed_reply: None,
                    task_id: task.task_id,
                    text: Some(message.text.clone()),
                    bundle: message.bundle.clone(),
                    mode: None,
                    harness: None,
                    model: None,
                    effort: None,
                    carried_plan: None,
                    read_only: false,
                });
                let envelope = CommandEnvelope {
                    command_id: id,
                    ..envelope
                };
                let seq = tx.emit(Event::CommandProgress {
                    command_id: id,
                    stage: CommandStage::HostAccepted,
                    result: None,
                    error: None,
                    detail: None,
                })?;
                tx.command(&store::StoredCommand {
                    envelope,
                    principal: message.principal,
                    receipt: CommandReceipt {
                        command_id: id,
                        status: ReceiptStatus::Accepted,
                        seq: Some(seq),
                        result: None,
                        error: None,
                    },
                    stage: CommandStage::HostAccepted,
                    pending: true,
                    result: None,
                    error: None,
                })?;
                tx.put("task", task.task_id, &task)?;
                tx.emit(Event::TaskUpserted { task: task.clone() })?;
                tx.delete(QUEUED, message.command_id)?;
                crate::history::admit(&tx.read())?;
                Ok(run)
            });
            match started {
                Ok(run) => starts.push((message.task_id, run)),
                Err(store::Error::Rule(reason)) => {
                    self.cancel_chat_turns(tx, message.task_id, false)?;
                    tx.emit(Event::TimelineAppended {
                        task_id: message.task_id,
                        item: TimelineItem {
                            seq: 0,
                            at: self.inner.clock.now(),
                            run_id: Some(message.run_id),
                            item: TimelineBody::Note {
                                text: format!(
                                    "The queued message could not start: {}",
                                    reason.message
                                ),
                            },
                        },
                    })?;
                }
                Err(e) => return Err(e),
            }
        }
        Ok(starts)
    }
}

/// Restart and terminal command settlement must release the queued message,
/// leaving its visible delivery failed so the person can send it again.
pub(crate) fn fail_undispatched_chat_turn(
    tx: &mut store::Write<'_>,
    command_id: CommandId,
    now: protocol::time::Timestamp,
) -> store::Result<()> {
    if let Some(message) = tx.read().get::<QueuedTurn>(QUEUED, command_id)? {
        settle_chat_turn(tx, &message, DeliveryState::Failed, false, false, now)?;
    }
    Ok(())
}

fn settle_chat_turn(
    tx: &mut store::Write<'_>,
    message: &QueuedTurn,
    state: DeliveryState,
    stopped: bool,
    revoked: bool,
    now: protocol::time::Timestamp,
) -> store::Result<()> {
    tx.delete(QUEUED, message.command_id)?;
    tx.emit(Event::TimelineAppended {
        task_id: message.task_id,
        item: TimelineItem {
            seq: 0,
            at: now,
            run_id: Some(message.run_id),
            item: TimelineBody::Delivery {
                message_seq: message.seq,
                state,
                withdrawn_by_stop: stopped,
                withdrawn_by_revocation: revoked,
            },
        },
    })?;
    Ok(())
}
