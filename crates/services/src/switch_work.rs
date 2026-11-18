// SPDX-License-Identifier: Apache-2.0
//! Switch to Work: a Plan or Ask task carries on in Work, or a conversation
//! becomes a task in Work, in one tap, whether or not its turn has ended.
//! A running turn is stopped first, cleanly, as Stop would; the same native
//! session then carries on in Work with what it learned, told why. While
//! another task changes the project, it waits its turn in Next up.
//!
//! A pending request the task could only allow in Work goes with it: an
//! access request, or, from a read-only task, a write it asked for ("Switch
//! to Work and allow"; from a chat, "Turn into a task"). The request is
//! settled as allowed, and the agent is told it may now do it, so it does it
//! again in Work. Ophio never answers Allow to a run that can't hold it.
use crate::{CompanionHub, api::Caller, error};
use protocol::{
    auth::Principal,
    command::ApprovalDecision,
    model::{
        Approval, ApprovalDelivery, ApprovalKind, ApprovalState, Project, Run, RunState, Task,
        TaskKind, TaskMode, TaskState, TimelineBody, TimelineItem,
    },
    queue::{QueueAuthorization, QueuedRequest},
    time::Timestamp,
    *,
};
use std::time::Duration;

#[cfg(test)]
#[path = "switch_work_agent_tests.rs"]
mod agent_tests;

/// What each admitted `SwitchToWork` (or an Allow that switches) does once
/// it is admitted, by command.
pub(crate) const SWITCH: &str = "switch_to_work";

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct Switch {
    pub task_id: TaskId,
    /// The turn it stops, if it was running.
    pub run: Option<RunId>,
    /// The request it allows by switching.
    pub approval: Option<ApprovalId>,
    /// The person's own words.
    pub text: Option<String>,
    /// It was a conversation.
    pub conversation: bool,
    pub by: Principal,
    /// Execution can happen after the person's admission scope has ended.
    #[serde(default)]
    pub ophio_written: bool,
}

/// The requests a switch (or a late Allow) allowed: asked again by a later
/// turn of the task (or, after a late Allow, by any turn), the same request
/// is allowed without another card, once. Each is kept under its task and
/// the request that was allowed (`{task}/{approval}`), so one never
/// replaces another; records from before kept one per task, under the task
/// alone, and are still read and used.
pub(crate) const PREAPPROVED: &str = "switch_preapproved";
/// How long that lasts after the switch.
const PREAPPROVED_MS: i64 = 30 * 60 * 1000;
/// The requests a pass allowed, by approval: an Allow that came from a pass
/// never makes another one.
pub(crate) const FROM_PASS: &str = "approval_from_pass";

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct Preapproved {
    /// [`request_key`] of the request allowed.
    pub request: String,
    /// The turn that asked it; that turn ended with the switch.
    pub asked_in: RunId,
    pub by: Principal,
    pub until: Timestamp,
    /// Allowed after its agent went on without it: the turn that asked may
    /// ask again too, as the answer tells it to. Older records lack it.
    #[serde(default)]
    pub any_turn: bool,
}

/// An old agent's permission must not be spent by another agent merely
/// because the conversation still has the same task id and request wording.
fn same_agent(r: &store::Read<'_>, asked_in: RunId, answering_in: RunId) -> store::Result<bool> {
    let asked: Option<Run> = r.get("run", asked_in)?;
    let answering: Option<Run> = r.get("run", answering_in)?;
    Ok(
        matches!((asked, answering), (Some(asked), Some(answering)) if asked.harness == answering.harness),
    )
}

/// `task_id`'s passes, with the ids they are kept under.
pub(crate) fn passes(
    r: &store::Read<'_>,
    task_id: TaskId,
) -> store::Result<Vec<(String, Preapproved)>> {
    let task = task_id.to_string();
    let under = format!("{task}/");
    Ok((r.entries::<Preapproved>(PREAPPROVED)?.into_iter())
        .filter(|(id, _)| *id == task || id.starts_with(&under))
        .collect())
}

/// Lets the same request as `a`, asked again in its task, be allowed once
/// without another card, as allowed by `by`. `any_turn`: the turn that
/// asked `a` may be the one to ask again. Allowed again, the same request
/// still goes through once; another request's pass is left as it was.
pub(crate) fn preapprove(
    tx: &mut store::Write<'_>,
    a: &protocol::model::Approval,
    by: Principal,
    any_turn: bool,
    now: Timestamp,
) -> store::Result<()> {
    let request = key_of(a);
    for (id, pass) in tx.read().entries::<Preapproved>(PREAPPROVED)? {
        if now > pass.until {
            tx.delete(PREAPPROVED, id)?;
        }
    }
    for (id, pass) in passes(&tx.read(), a.task_id)? {
        if pass.request == request && same_agent(&tx.read(), pass.asked_in, a.run_id)? {
            tx.delete(PREAPPROVED, id)?;
        }
    }
    tx.put(
        PREAPPROVED,
        format!("{}/{}", a.task_id, a.approval_id),
        &Preapproved {
            request,
            asked_in: a.run_id,
            by,
            until: Timestamp(now.0.saturating_add(PREAPPROVED_MS)),
            any_turn,
        },
    )
}

/// Whether a late Allow of `a` lets the same request through once more: only
/// for what an agent asked to do (run a command, change a file, use the
/// network or a tool), never an access request, a question, Ophio's own
/// gate or its own question about starting an app, and never for an Allow
/// that a pass gave.
pub(crate) fn passes_on(r: &store::Read<'_>, a: &Approval) -> store::Result<bool> {
    // An agent's request through the bridge carries what it asked; Ophio's
    // own questions there carry nothing. Native requests are all an agent's.
    let agent_asked = a.raw_request.is_some()
        || !r
            .get::<bool>("bridge_approval", a.approval_id)?
            .unwrap_or(false);
    Ok(matches!(
        a.kind,
        ApprovalKind::Command
            | ApprovalKind::FileEdit
            | ApprovalKind::Network
            | ApprovalKind::Tool
            | ApprovalKind::Other
    ) && a.access_request.is_none()
        && agent_asked
        && !crate::approval_rule::gate(r, a)?
        && r.get::<bool>(FROM_PASS, a.approval_id)?.is_none())
}

/// The person declined `a`: their last word on that request in its task is
/// no, so a pass an earlier Allow left for it goes.
pub(crate) fn declined(tx: &mut store::Write<'_>, a: &Approval) -> store::Result<()> {
    let request = key_of(a);
    for (id, pass) in passes(&tx.read(), a.task_id)? {
        if pass.request == request && same_agent(&tx.read(), pass.asked_in, a.run_id)? {
            tx.delete(PREAPPROVED, id)?;
        }
    }
    Ok(())
}

/// Withdraws every pass of `task_id`, as the person stops it or archives
/// it: nothing they allowed before goes through after without a card.
pub(crate) fn let_go(tx: &mut store::Write<'_>, task_id: TaskId) -> store::Result<()> {
    for (id, _) in passes(&tx.read(), task_id)? {
        tx.delete(PREAPPROVED, id)?;
    }
    Ok(())
}

fn key_of(a: &Approval) -> String {
    request_key(
        &a.kind,
        a.raw_request.as_deref(),
        &a.title,
        a.detail.as_deref(),
    )
}

/// What makes two requests the same request: their kind, exact words and
/// what they show, never the ids or the agent's own description in them.
pub(crate) fn request_key(
    kind: &ApprovalKind,
    raw: Option<&str>,
    title: &str,
    detail: Option<&str>,
) -> String {
    let raw = match raw.map(serde_json::from_str::<serde_json::Value>) {
        Some(Ok(serde_json::Value::Object(mut fields))) => {
            fields.retain(|key, _| {
                !matches!(
                    key.as_str(),
                    "description"
                        | "itemId"
                        | "threadId"
                        | "turnId"
                        | "callId"
                        | "approvalId"
                        | "id"
                        | "requestId"
                        | "tool_use_id"
                )
            });
            serde_json::Value::Object(fields).to_string()
        }
        _ => raw.unwrap_or_default().to_owned(),
    };
    // The title names the tool asked of: the same input to another tool is
    // another request.
    format!("{kind:?}\n{title}\n{raw}\n{}", detail.unwrap_or_default())
}

/// Whether the request `a` of its task is one a switch or a late Allow
/// allowed; if so, that pass is used up (and only that one), `a` is marked
/// as allowed by a pass, and who allowed it is returned.
pub(crate) fn take_preapproved(
    tx: &mut store::Write<'_>,
    a: &protocol::model::Approval,
    now: Timestamp,
) -> store::Result<Option<Principal>> {
    let key = key_of(a);
    let mut found = None;
    for (id, pass) in passes(&tx.read(), a.task_id)? {
        if now > pass.until {
            tx.delete(PREAPPROVED, id)?;
        } else if found.is_none()
            && pass.request == key
            && same_agent(&tx.read(), pass.asked_in, a.run_id)?
            && (a.run_id != pass.asked_in || pass.any_turn)
        {
            found = Some((id, pass.by));
        }
    }
    let Some((id, by)) = found else {
        return Ok(None);
    };
    // Never an access request, and never Ophio's own gate, which asks again
    // each turn by design; and only where Allow is the person's to give.
    if a.access_request.is_some()
        || crate::approval_rule::gate(&tx.read(), a)?
        || (crate::approval_rule::Asked::of(&tx.read(), a)?)
            .refusal(&ApprovalDecision::Allow, false)
            .is_some()
    {
        return Ok(None);
    }
    tx.delete(PREAPPROVED, id)?;
    tx.put(FROM_PASS, a.approval_id, &true)?;
    Ok(Some(by))
}

/// Settles `id` as allowed by `by`, as a delivered Allow would, without
/// handing it to the turn that asked: that turn is ending.
pub(crate) fn settle_allowed(
    tx: &mut store::Write<'_>,
    id: ApprovalId,
    by: Principal,
    now: Timestamp,
) -> store::Result<()> {
    let mut a: Approval = tx.read().require("approval", id)?;
    if a.state != ApprovalState::Pending {
        return Ok(());
    }
    a.state = ApprovalState::Allowed;
    a.resolved_by = Some(by);
    a.resolved_at = Some(now);
    a.delivery = Some(ApprovalDelivery {
        decision: ApprovalDecision::Allow,
        at: now,
        error: None,
    });
    a.revision += 1;
    tx.delete(crate::carry_on::CARRIED_ON, id)?;
    tx.put("approval", id, &a)?;
    tx.emit(Event::ApprovalUpserted {
        approval: a.clone(),
    })?;
    crate::runs::resolve_attention(tx, id, now)?;
    crate::runs::recount_attention(tx, a.task_id)
}

impl CompanionHub {
    /// Admits a switch of `task_id` to Work, taking `approval` along: the
    /// task shows Work at once, its running turn is stopped, and the switch
    /// is kept for [`Self::switch_effect`].
    pub(crate) fn admit_switch(
        &self,
        tx: &mut store::Write<'_>,
        caller: &Caller,
        command: CommandId,
        task_id: TaskId,
        approval: Option<ApprovalId>,
        text: Option<String>,
    ) -> store::Result<()> {
        let now = self.inner.clock.now();
        let mut task: Task = tx.read().require("task", task_id)?;
        if !task.conversation && task.mode == TaskMode::Work {
            return Err(error(
                ErrorCode::InvalidRequest,
                "This task is already in Work mode",
            )
            .into());
        }
        if task.kind != TaskKind::Coding || crate::github::writes_nothing(&task) {
            return Err(error(
                ErrorCode::InvalidRequest,
                "This task only reads and reports, so it isn't carried out in Work mode",
            )
            .into());
        }
        // An outsider's task began in Plan so its plan is reviewed before
        // anything changes; it carries on in Work that way only.
        if crate::approval_rule::outsider(&tx.read(), &task)? {
            return Err(error(
                ErrorCode::InvalidRequest,
                "This task came from someone outside the project, so it carries on in Work only from its reviewed plan.",
            )
            .into());
        }
        let text = text.map(|t| t.trim().to_owned()).filter(|t| !t.is_empty());
        if text.as_ref().is_some_and(|t| t.len() > 32 * 1024) {
            return Err(error(ErrorCode::LimitExceeded, "The message is too long").into());
        }
        let running = match task.current_run {
            Some(id) => {
                let run: Run = tx.read().require("run", id)?;
                (!run.state.is_terminal()).then_some(run)
            }
            None => None,
        };
        match (&running, task.state) {
            (Some(run), _) => {
                // A read-only side question of the task finishes first.
                if crate::runs::run_mode(run, &task) != task.mode
                    || tx
                        .read()
                        .get::<TaskState>(crate::runs::SIDE_TURN, run.run_id)?
                        .is_some()
                {
                    return Err(error(
                        ErrorCode::Conflict,
                        "Wait for the side question to finish, then switch to Work.",
                    )
                    .into());
                }
                if run.state == RunState::Stopping {
                    return Err(error(
                        ErrorCode::Conflict,
                        "This task is already stopping. Switch to Work once it has.",
                    )
                    .into());
                }
            }
            (None, TaskState::Interrupted | TaskState::Completed | TaskState::Failed) => {}
            (None, TaskState::Archived) => {
                return Err(error(
                    ErrorCode::Conflict,
                    "Bring the task back from the archive first",
                )
                .into());
            }
            (None, _) => {
                return Err(error(
                    ErrorCode::Conflict,
                    "This task hasn't started yet, so it can't carry on in Work.",
                )
                .into());
            }
        }
        if (crate::runs::queue_completion::waits_to_start(&tx.read(), &task))? {
            return Err(error(
                ErrorCode::Conflict,
                "This task is already waiting in Next up. Start or remove it there first.",
            )
            .into());
        }
        if let Some(id) = approval {
            let a: Approval = tx.read().require("approval", id)?;
            // A request its agent stopped waiting for stays open: switching
            // for it carries on from whichever turn is now the task's, or
            // none, by the same rules. Anything else is the running turn's.
            let later = crate::carry_on::carried_on(&tx.read(), id)?;
            let asked: Run = tx.read().require("run", a.run_id)?;
            if asked.harness != task.harness {
                return Err(error(
                    ErrorCode::Expired,
                    "That request came from a different agent. Ask the current agent again.",
                )
                .into());
            }
            if a.task_id != task_id
                || (!later && running.as_ref().is_none_or(|run| run.run_id != a.run_id))
            {
                return Err(error(
                    ErrorCode::InvalidRequest,
                    "That request isn't this task's turn's",
                )
                .into());
            }
            if a.state != ApprovalState::Pending || a.expires_at.is_some_and(|t| t <= now) {
                return Err(error(
                    ErrorCode::AlreadyResolved,
                    "That request is no longer waiting",
                )
                .into());
            }
            if let Some(refusal) = crate::approval_rule::Asked::of(&tx.read(), &a)?.switch_refusal()
            {
                return Err(refusal.into());
            }
            match tx.read().get::<CommandId>("approval_claim", id)? {
                Some(claim) if claim != command => {
                    return Err(error(
                        ErrorCode::AlreadyResolved,
                        "Approval decision already admitted",
                    )
                    .into());
                }
                _ => tx.put("approval_claim", id, &command)?,
            }
            settle_allowed(tx, id, caller.principal, now)?;
            // Asked again in Work, the same request is allowed at once: the
            // person allowed it by switching.
            if a.access_request.is_none() && !crate::approval_rule::gate(&tx.read(), &a)? {
                preapprove(tx, &a, caller.principal, false, now)?;
            }
        }
        // What else it went on without and offered to switch for has no
        // way forward once it has: it goes, and is asked again in Work.
        crate::carry_on::let_go_switched(tx, task_id, approval, now)?;
        let conversation = task.conversation;
        task.mode = TaskMode::Work;
        if task.conversation {
            // It stays held to what the conversation was, and may now change
            // its own project, as Turn into a task does.
            if let Some(mut access) = task.access.take() {
                access.write.push(task.project_id);
                task.access = Some(access.normalized());
            }
            task.access_blocked = None;
            task.conversation = false;
        }
        if let Some(run) = &running {
            self.admit_stopping(tx, &mut task, run.run_id, command, now)?;
        }
        tx.put(
            SWITCH,
            command,
            &Switch {
                task_id,
                run: running.map(|r| r.run_id),
                approval,
                text,
                conversation,
                by: caller.principal,
                ophio_written: !self.is_from_person(command),
            },
        )?;
        task.revision += 1;
        task.updated_at = now;
        task.attention_count = crate::runs::blocking_attention_count(&tx.read(), task_id)?;
        tx.put("task", task_id, &task)?;
        tx.emit(Event::TaskUpserted { task })?;
        Ok(())
    }

    /// Stop's admission for the turn a switch ends: its other requests are
    /// withdrawn and it takes nothing new. Unlike Stop, Next up isn't paused
    /// and the checklist keeps what waits on the task: it carries on.
    fn admit_stopping(
        &self,
        tx: &mut store::Write<'_>,
        task: &mut Task,
        run_id: RunId,
        command: CommandId,
        now: Timestamp,
    ) -> store::Result<()> {
        let mut run: Run = tx.read().require("run", run_id)?;
        let count = tx.read().get::<u32>("stop_count", run_id)?.unwrap_or(0);
        tx.put("stop_count", run_id, &count.saturating_add(1))?;
        tx.put("stopped", run_id, &true)?;
        tx.put("stop_run", command, &run_id)?;
        for mut a in tx.read().list::<Approval>("approval")? {
            if a.run_id == run_id && a.state == ApprovalState::Pending {
                tx.delete(crate::carry_on::CARRIED_ON, a.approval_id)?;
                a.state = ApprovalState::Cancelled;
                a.revision += 1;
                a.resolved_at = Some(now);
                tx.put("approval", a.approval_id, &a)?;
                tx.emit(Event::ApprovalUpserted {
                    approval: a.clone(),
                })?;
                crate::runs::resolve_attention(tx, a.approval_id, now)?;
            }
        }
        tx.delete("run_token", run_id)?;
        if !crate::runs::still_finishing(&run) {
            run.state = RunState::Stopping;
            tx.put("run", run_id, &run)?;
            tx.emit(Event::RunUpserted { run: Box::new(run) })?;
        }
        if task.wake.take().is_some() {
            tx.delete(
                crate::followups::FOLLOWUP,
                crate::followups::wake_key(task.task_id),
            )?;
        }
        crate::asking::let_go(tx, task.task_id)
    }

    /// Carries out an admitted switch: stops the turn it ends, then starts
    /// the task's next turn in Work, in the same session, or puts it in
    /// Next up while another task changes the project.
    pub(crate) async fn switch_effect(&self, command: CommandId) -> Result<(), ProtocolError> {
        let switch: Switch = self.read(|r| r.require(SWITCH, command))?;
        let task_id = switch.task_id;
        if let Some(run) = switch.run {
            let stopped = self.stop_task(task_id, Some(run), false).await;
            // Until it has ended, whether its agent was running or still
            // starting: one still starting isn't among the live runs yet.
            let ended = tokio::time::timeout(Duration::from_secs(30), async {
                loop {
                    let live = self.inner.runs.lock().await.contains_key(&run);
                    let stored: Option<Run> = self.read(|r| r.get("run", run)).unwrap_or(None);
                    if !live && stored.is_none_or(|r| r.state.is_terminal()) {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await;
            if ended.is_err() {
                return Err(stopped.err().unwrap_or_else(|| {
                    error(
                        ErrorCode::OutcomeUnknown,
                        "The agent hasn’t stopped yet. The task is in Work mode now: send it a message once it has stopped.",
                    )
                }));
            }
        }
        if let Some(id) = switch.approval {
            // The turn that waited on it has ended; nothing answers it there.
            self.inner.waits.lock().await.remove(&id);
        }
        let next = self.write(|tx| {
            let mut task: Task = tx.read().require("task", task_id)?;
            if task.mode != TaskMode::Work {
                return Ok(None);
            }
            if let Some(id) = task.current_run {
                let current: Run = tx.read().require("run", id)?;
                if !current.state.is_terminal() || crate::runs::still_finishing(&current) {
                    return Err(error(
                        ErrorCode::Conflict,
                        "Something this task started earlier hasn’t ended yet. The task is in Work mode now: send it a message once that has ended.",
                    )
                    .into());
                }
            }
            let project: Project = tx.read().require("project", task.project_id)?;
            let approval: Option<Approval> = match switch.approval {
                Some(id) => tx.read().get("approval", id)?,
                None => None,
            };
            let text = agent_text(&switch, approval.as_ref(), &project.name);
            let now = self.inner.clock.now();
            let agent = crate::agent_browser::harness_name(task.harness);
            let busy = project.active_writer.and_then(|writer| {
                let run: Option<Run> = tx.read().get("run", writer).ok().flatten();
                run.filter(|r| r.task_id != task_id)
            });
            let note = |tx: &mut store::Write<'_>, run_id: Option<RunId>, text: String| {
                tx.emit(Event::TimelineAppended {
                    task_id,
                    item: TimelineItem {
                        seq: 0,
                        at: now,
                        run_id,
                        item: TimelineBody::Note { text },
                    },
                })
            };
            if let Some(holder) = busy {
                let holder: Task = tx.read().require("task", holder.task_id)?;
                // It waits its turn, and starts by itself once the project
                // is free.
                tx.put(crate::runs::queue_completion::CONTINUATION, task_id, &text)?;
                // Its start permit names the task as it now is.
                task.revision += 1;
                task.updated_at = now;
                tx.put("task", task_id, &task)?;
                let caller = Caller {
                    principal: switch.by,
                    grants: protocol::auth::Grants::default(),
                };
                // Next up the person paused stays paused: there it waits for
                // their go-ahead, as anything added to it then does.
                let paused = tx
                    .read()
                    .get::<protocol::queue::QueueView>("queue", task.project_id)?
                    .is_some_and(|q| q.paused);
                self.enqueue(
                    tx,
                    &caller,
                    &task,
                    None,
                    match paused {
                        true => QueueAuthorization::AskBeforeStart,
                        false => QueueAuthorization::AutoStart,
                    },
                    QueuedRequest {
                        prompt: switch.text.clone().unwrap_or_else(|| "Carry on in Work mode.".into()),
                        bundle: None,
                    },
                )?;
                tx.emit(Event::TaskUpserted { task })?;
                if let Some(text) = &switch.text {
                    let seq = user_message(tx, task_id, None, text, now)?;
                    crate::turned_from_chat::record_message_origin(tx, task_id, seq, !switch.ophio_written)?;
                    self.track_start_message(tx, task_id, None, seq)?;
                }
                note(
                    tx,
                    None,
                    match paused {
                        true => format!(
                            "Switched to Work. Waits in Next up while “{}” changes {}. Next up is paused, so start it there when you're ready; {agent} carries on in the same session.",
                            holder.title, project.name
                        ),
                        false => format!(
                            "Switched to Work. Waits in Next up while “{}” changes {}, then {agent} carries on in the same session.",
                            holder.title, project.name
                        ),
                    },
                )?;
                return Ok(None);
            }
            let run_id = self.new_run(tx, &mut task, command, Some(text), None, false)?;
            task.attention_count = crate::runs::blocking_attention_count(&tx.read(), task_id)?;
            tx.put("task", task_id, &task)?;
            tx.emit(Event::TaskUpserted { task })?;
            note(
                tx,
                Some(run_id),
                match switch.conversation {
                    true => format!("Turned into a task in Work mode. {agent} carries on in the same session and may now change {}.", project.name),
                    false => format!("Switched to Work mode. {agent} carries on in the same session and may now change {}.", project.name),
                },
            )?;
            if let Some(text) = &switch.text {
                let seq = user_message(tx, task_id, Some(run_id), text, now)?;
                crate::turned_from_chat::record_message_origin(tx, task_id, seq, !switch.ophio_written)?;
                self.track_start_message(tx, task_id, Some(run_id), seq)?;
            }
            Ok(Some(run_id))
        })?;
        match next {
            Some(run) => self.dispatch_task(task_id, run).await,
            None => Ok(()),
        }
    }
}

/// The person's own words, in the task's timeline; its sequence number.
fn user_message(
    tx: &mut store::Write<'_>,
    task_id: TaskId,
    run_id: Option<RunId>,
    text: &str,
    at: Timestamp,
) -> store::Result<u64> {
    tx.emit(Event::TimelineAppended {
        task_id,
        item: TimelineItem {
            seq: 0,
            at,
            run_id,
            item: TimelineBody::UserMessage {
                text: text.to_owned(),
                bundle: None,
                delivery: protocol::model::DeliveryState::HostAccepted,
                carried_plan: None,
            },
        },
    })
}

/// What the agent is told as its turn in Work starts, ahead of the person's
/// own words.
fn agent_text(switch: &Switch, approval: Option<&Approval>, project: &str) -> String {
    let what = match switch.conversation {
        true => "turned this conversation into a task in Work mode",
        false => "switched this task to Work mode",
    };
    let stopped = match switch.run {
        Some(_) => {
            " Ophio stopped your previous turn to do it: check what finished before starting anything again."
        }
        None => "",
    };
    let mut text = match approval {
        Some(a) if a.access_request.is_some() => {
            let reason = (a.access_request.as_ref())
                .map(|r| format!(" ({})", r.reason))
                .unwrap_or_default();
            format!(
                "Ophio: the person {what} so you can do what you asked for{reason}. You may now change files and run commands in {project}.{stopped} Then carry on."
            )
        }
        Some(a) => format!(
            "Ophio: the person {what} and allowed what you asked for: {}. It didn't run in your previous turn, which Ophio stopped to switch, so do it again now, then carry on.",
            a.title.trim_end_matches('.')
        ),
        None => format!(
            "Ophio: the person {what}. You may now change files and run commands in {project}.{stopped} Then carry on with what they asked."
        ),
    };
    if let Some(words) = &switch.text {
        text.push_str("\n\n");
        text.push_str(words);
    }
    text
}
