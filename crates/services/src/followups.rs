// SPDX-License-Identifier: Apache-2.0
//! What Ophio still has to pass on to a task's agent: an answer given after
//! the agent went on without it, the wake an agent asked for, or another
//! agent's answer to what it asked (`crate::asking`), or what the person did
//! on the project's checklist (`crate::checklist`). Answers go into an
//! existing turn or start the next one; wakes wait for a free turn. Each is
//! kept until it has gone (answers have a one-day delivery limit).

use crate::{CompanionHub, github_watch::as_creator};
use protocol::{
    ApprovalId, CommandId, ErrorCode, Event, ProtocolError, TaskId,
    command::{Command, CommandEnvelope, MessageDelivery, ReceiptStatus},
    model::{Project, Run, Task, TaskMode, TaskState, TimelineBody, TimelineItem},
    queue::{QueueAuthorization, QueueView},
    time::Timestamp,
};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Still to pass on, by key: one answer per request, one wake per task.
pub(crate) const FOLLOWUP: &str = "task_followup";
/// Answers not passed on in this long after they were due are dropped, and
/// the task says so. A wake stays pending until delivered or cancelled.
const KEPT_FOR: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum FollowupKind {
    Answer,
    Wake,
    Reply,
    Checklist,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Followup {
    pub key: String,
    /// What starts the task again, so a restart doesn't start it twice.
    pub command_id: CommandId,
    pub task_id: TaskId,
    pub text: String,
    pub due_at: Timestamp,
    pub made_at: Timestamp,
    pub kind: FollowupKind,
}

impl Followup {
    /// The answer to `approval`, to pass on now.
    pub(crate) fn answer(
        task_id: TaskId,
        approval: ApprovalId,
        text: String,
        now: Timestamp,
    ) -> Self {
        Self {
            key: format!("answer:{approval}"),
            command_id: CommandId::new(),
            task_id,
            text,
            due_at: now,
            made_at: now,
            kind: FollowupKind::Answer,
        }
    }

    /// The wake `task_id`'s agent asked for at `due_at`. A task has one: a
    /// newer wake replaces it.
    pub(crate) fn wake(task_id: TaskId, text: String, due_at: Timestamp, now: Timestamp) -> Self {
        Self {
            key: wake_key(task_id),
            command_id: CommandId::new(),
            task_id,
            text,
            due_at,
            made_at: now,
            kind: FollowupKind::Wake,
        }
    }

    /// What the agent of `asked`, a task `task_id`'s agent asked for help,
    /// answered, to pass on now.
    pub(crate) fn reply(task_id: TaskId, asked: TaskId, text: String, now: Timestamp) -> Self {
        Self {
            key: format!("reply:{asked}"),
            command_id: CommandId::new(),
            task_id,
            text,
            due_at: now,
            made_at: now,
            kind: FollowupKind::Reply,
        }
    }

    /// What the person did on the checklist that `task_id`'s agent wrote,
    /// to pass on now: one per item ticked or answered, or question asked.
    pub(crate) fn checklist(key: String, task_id: TaskId, text: String, now: Timestamp) -> Self {
        Self {
            key,
            command_id: CommandId::new(),
            task_id,
            text,
            due_at: now,
            made_at: now,
            kind: FollowupKind::Checklist,
        }
    }

    /// A batch copy is only usable while its exact delivery is still stored.
    pub(crate) fn is_current(&self, r: &store::Read<'_>) -> store::Result<bool> {
        Ok(r.get::<Self>(FOLLOWUP, &self.key)?
            .is_some_and(|saved| saved.command_id == self.command_id))
    }

    pub(crate) fn require_current(&self, r: &store::Read<'_>) -> store::Result<()> {
        if !self.is_current(r)? {
            return Err(
                crate::error(ErrorCode::Expired, "Follow-up was cancelled or replaced").into(),
            );
        }
        Ok(())
    }

    pub(crate) fn put(&self, tx: &mut store::Write<'_>) -> store::Result<()> {
        tx.put(FOLLOWUP, &self.key, self)
    }
}

/// Check locally before issuing a command, rather than recording a refused
/// resume every time the guard looks. The admission transaction still enforces
/// the writer and cleanup gates if anything changes after this check.
pub(crate) fn waiting_for_turn(
    r: &store::Read<'_>,
    task: &Task,
    wake: bool,
) -> store::Result<bool> {
    // A wake never bypasses the person's pending turn. An answer may start
    // it with the queued message once the project is free, as before.
    let queued = r
        .get::<QueueView>("queue", task.project_id)?
        .is_some_and(|q| {
            q.items
                .iter()
                .any(|item| item.task_id == task.task_id && item.is_pending())
        });
    if wake && queued {
        return Ok(true);
    }
    if let Some(id) = task.current_run {
        let Some(run) = r.get::<Run>("run", id)? else {
            // Missing evidence is not permission to start another turn.
            return Ok(true);
        };
        if crate::runs::still_finishing(&run)
            || (!run.state.is_terminal()
                && (wake
                    || !matches!(task.state, TaskState::Working | TaskState::AwaitingDecision)))
        {
            return Ok(true);
        }
    }
    // Keep wakes in their existing, durable per-task slot, not in Next up:
    // a newer wake can replace it there without piling another message onto
    // a queued turn. Even a writer whose state is unknown holds this gate.
    if (wake || task.mode == TaskMode::Work)
        && let Some(writer) = r
            .get::<Project>("project", task.project_id)?
            .and_then(|project| project.active_writer)
    {
        if wake || queued || task.state == TaskState::Accepted {
            return Ok(true);
        }
        if !matches!(task.state, TaskState::Working | TaskState::AwaitingDecision)
            && r.get::<Run>("run", writer)?
                .is_none_or(|holder| holder.task_id == task.task_id)
        {
            return Ok(true);
        }
    }
    // A retained wake has no age limit. Known temporary harness outages must
    // therefore wait locally too, not grow the rejected-command log forever.
    if wake {
        let catalog: Vec<protocol::capability::HarnessInfo> =
            r.get("meta", "harnesses")?.unwrap_or_default();
        if let Some(info) = catalog.iter().find(|info| info.kind == task.harness)
            && (domain::task_mode(info, task.mode)
                .is_err_and(|e| e.code == ErrorCode::TemporarilyUnavailable)
                || (task.current_run.is_some()
                    && domain::availability(&info.capabilities.resume_session)
                        .is_err_and(|e| e.code == ErrorCode::TemporarilyUnavailable)))
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Whether a work task's project is held by another task's turn.
fn held_by_another(r: &store::Read<'_>, task: &Task) -> store::Result<bool> {
    if task.mode != TaskMode::Work {
        return Ok(false);
    }
    let Some(project) = r.get::<Project>("project", task.project_id)? else {
        return Ok(false);
    };
    Ok(match project.active_writer {
        Some(run) => r
            .get::<Run>("run", run)?
            .is_some_and(|run| run.task_id != task.task_id),
        None => false,
    })
}

pub(crate) fn wake_key(task_id: TaskId) -> String {
    format!("wake:{task_id}")
}

/// Whether anything is still to pass on, so the guard keeps looking.
pub(crate) fn any(r: &store::Read<'_>) -> store::Result<bool> {
    Ok(!r.entries::<Followup>(FOLLOWUP)?.is_empty())
}

impl CompanionHub {
    /// Passes on what is due. Returns how many went.
    pub(crate) async fn pass_on_followups(&self) -> Result<usize, ProtocolError> {
        let _one = self.inner.followups.lock().await;
        let now = self.inner.clock.now();
        let mut due: Vec<Followup> = self.read(|r| {
            Ok((r.entries::<Followup>(FOLLOWUP)?.into_iter())
                .map(|(_, f)| f)
                .filter(|f| f.due_at <= now)
                .collect())
        })?;
        due.sort_by_key(|f| (f.due_at, f.made_at));
        let mut went = 0;
        for f in due {
            match self.pass_on(f, now).await {
                Ok(true) => went += 1,
                Ok(false) => {}
                Err(e) => tracing::warn!(code = ?e.code, "passing a follow-up on failed"),
            }
        }
        Ok(went)
    }

    async fn pass_on(&self, mut f: Followup, now: Timestamp) -> Result<bool, ProtocolError> {
        if !self.read(|r| f.is_current(r))? {
            return Ok(false);
        }
        // Started before a restart, say: it went.
        if let Some(command) = self.read(|r| r.command(f.command_id))? {
            if command.receipt.status != ReceiptStatus::Rejected {
                self.drop_followup(&f, None)?;
                return Ok(true);
            }
            let changed = self.write(|tx| {
                if !f.is_current(&tx.read())? {
                    return Ok(false);
                }
                f.command_id = CommandId::new();
                f.put(tx)?;
                Ok(true)
            })?;
            if !changed {
                return Ok(false);
            }
        }
        let task: Option<Task> = self.read(|r| r.get("task", f.task_id))?;
        let Some(task) = task.clone().filter(|t| t.state != TaskState::Archived) else {
            // A question about a checklist whose agent was put away goes to
            // another, so the person still gets an answer.
            if let Some(archived) = task.filter(|_| crate::checklist::is_question(&f.key))
                && let Err(e) = self.ask_about_checklist(&archived, &f).await
            {
                tracing::warn!(code = ?e.code, "asking another agent about the checklist failed");
            }
            self.drop_followup(&f, None)?;
            return Ok(false);
        };
        if f.kind != FollowupKind::Wake
            && now.0.saturating_sub(f.due_at.0) > KEPT_FOR.as_millis() as i64
        {
            let why = match f.kind {
                FollowupKind::Answer => {
                    "Ophio couldn't pass the answer on to the agent within a day, so it stopped trying."
                }
                FollowupKind::Wake => {
                    "Ophio couldn't wake the agent within a day, so it stopped trying."
                }
                FollowupKind::Reply => {
                    "Ophio couldn't pass the other agent's answer on within a day, so it stopped trying."
                }
                FollowupKind::Checklist => {
                    "Ophio couldn't pass what you did on the checklist on to the agent within a day, so it stopped trying. Ask it in Chat, or tick the item again."
                }
            };
            self.drop_followup(&f, Some(why))?;
            return Ok(false);
        }
        if self.read(|r| waiting_for_turn(r, &task, f.kind == FollowupKind::Wake))? {
            return Ok(false);
        }
        match (task.state, task.current_run) {
            (TaskState::Working | TaskState::AwaitingDecision, Some(run)) => {
                let sent = self
                    .send_message_with_followup(
                        &as_creator(&task),
                        CommandId::new(),
                        run,
                        &f.text,
                        None,
                        MessageDelivery::Steer,
                        Some(&f),
                    )
                    .await;
                match sent {
                    Ok(()) => {}
                    // It may have reached the agent: it isn't sent twice.
                    Err(e) if e.code == ErrorCode::OutcomeUnknown => {}
                    // The turn is ending: it starts the next one.
                    Err(_) => return Ok(false),
                }
                self.drop_followup(&f, None)?;
                Ok(true)
            }
            (
                TaskState::Accepted
                | TaskState::Interrupted
                | TaskState::Completed
                | TaskState::Failed,
                _,
            ) => {
                // Answers can wait visibly in Next up. Wakes keep their
                // replaceable per-task slot even if a writer arrived since
                // the local check; admission rechecks before starting the turn.
                let command = match f.kind != FollowupKind::Wake
                    && task.state != TaskState::Accepted
                    && self.read(|r| held_by_another(r, &task))?
                {
                    true => Command::QueueContinuation {
                        task_id: task.task_id,
                        text: f.text.clone(),
                        bundle: None,
                        authorization: QueueAuthorization::AutoStart,
                    },
                    false => Command::ResumeTask {
                        task_id: task.task_id,
                        text: Some(f.text.clone()),
                        bundle: None,
                        mode: None,
                        carried_plan: None,
                        read_only: false,
                        model: None,
                        effort: None,
                    },
                };
                let mut envelope = CommandEnvelope::new(command);
                envelope.command_id = f.command_id;
                envelope.expected_revision = Some(task.revision);
                let receipt = self
                    .admit_with_followup(&as_creator(&task), envelope, now, None, Some(&f))
                    .await;
                let Some(e) = receipt.error else {
                    self.drop_followup(&f, None)?;
                    return Ok(true);
                };
                match e.code {
                    // The task moved on meanwhile: try again next time.
                    ErrorCode::StaleRevision
                    | ErrorCode::Conflict
                    | ErrorCode::TemporarilyUnavailable => Ok(false),
                    _ => {
                        let why =
                            format!("Ophio couldn't pass this on to the agent: {}", e.message);
                        self.drop_followup(&f, Some(&why))?;
                        Ok(false)
                    }
                }
            }
            // Starting, or waiting on a check of what it did: next time.
            _ => Ok(false),
        }
    }

    /// Starts an agent in `archived`'s project, read-only, on the question
    /// `f` carries, which `archived`'s agent can no longer answer. It has
    /// the checklist's read and reply tools and nothing else of Ophio's, so
    /// its answer goes under the question.
    async fn ask_about_checklist(
        &self,
        archived: &Task,
        f: &Followup,
    ) -> Result<(), ProtocolError> {
        let choice = self.chosen_agent(archived, None)?;
        let prompt = format!(
            "The agent that wrote this project's checklist has finished, so Ophio asks you instead. You're read-only here: read the project and the checklist, with checklist_read first, and answer with checklist_reply. You can't change either.\n\n{}",
            crate::checklist::for_another(&f.text)
        );
        let envelope = CommandEnvelope::new(Command::CreateTask {
            project_id: archived.project_id,
            kind: protocol::model::TaskKind::Coding,
            mode: Some(TaskMode::Ask),
            title: Some("Question about the checklist".into()),
            prompt,
            bundle: None,
            harness: choice.harness,
            model: choice.model,
            effort: choice.effort,
            launch_policy: None,
            branch: None,
            start: None,
            place: None,
            browser: Some(false),
            github: None,
            mission: None,
            target_id: None,
            placement: protocol::command::TaskPlacement::StartNow,
            agent_chosen: None,
            conversation: None,
            access: crate::access::held_to(crate::access::held(archived), true),
            review: None,
        });
        let command = envelope.command_id;
        self.write(|tx| crate::checklist::will_answer(tx, command))?;
        let receipt = self.admit(&as_creator(archived), envelope).await;
        self.write(|tx| crate::checklist::wont_answer(tx, command))?;
        match receipt.error {
            None => Ok(()),
            Some(e) => Err(e),
        }
    }

    fn drop_followup(&self, f: &Followup, why: Option<&str>) -> Result<(), ProtocolError> {
        let now = self.inner.clock.now();
        self.write(|tx| {
            // A late worker must not delete a newer wake or undo Stop.
            if !f.is_current(&tx.read())? {
                return Ok(());
            }
            if f.kind == FollowupKind::Wake {
                crate::watching::clear_wake(tx, f.task_id, now)?;
            }
            tx.delete(FOLLOWUP, &f.key)?;
            if f.kind == FollowupKind::Reply {
                crate::asking::passed_on(tx, f.task_id, now)?;
                // Losing an answer doesn't authorize merging past its findings.
                if why.is_some()
                    && let Some(pr) = tx
                        .read()
                        .get::<Task>("task", f.task_id)?
                        .and_then(|t| t.github)
                        .and_then(|g| g.pr)
                        .filter(|p| p.state == protocol::github::PrState::Open)
                {
                    crate::github_watch::hold(
                        tx,
                        f.task_id,
                        pr.number,
                        "The helper's answer couldn't reach the agent. Read it in the helper's task before merging.".into(),
                        now,
                        |_| {},
                    )?;
                }
            }
            if let Some(text) = why {
                tx.emit(Event::TimelineAppended {
                    task_id: f.task_id,
                    item: TimelineItem {
                        seq: 0,
                        at: now,
                        run_id: None,
                        item: TimelineBody::Note { text: text.into() },
                    },
                })?;
            }
            Ok(())
        })
    }

    /// Looks again now, for what was just stored.
    pub(crate) fn wake_guard(&self) {
        self.inner.guard.notify_one();
    }
}
