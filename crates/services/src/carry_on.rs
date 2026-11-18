// SPDX-License-Identifier: Apache-2.0
//! No run waits on a person while it holds the project. A question or an
//! approval nobody has answered in [`CARRY_ON_AFTER`] goes back to the agent
//! with word to finish what doesn't depend on it and end its turn, and the
//! project is free once it has. The request stays open: an answer given
//! later reaches the agent as its next message, and starts the task again
//! if its turn has ended. One its agent stops waiting for sooner (its tool
//! call gave up, its turn ended, or its connection to Ophio closed) stays
//! open the same way, unless the person stopped the turn. A private
//! question is never kept open: its answer may only go to the live
//! question, so it expires instead, as a login does.

use crate::{CompanionHub, api::Caller, error, followups::Followup};
use ports::{MessageRef, PromptInput};
use protocol::{
    ApprovalId, CommandId, ErrorCode, Event, ProtocolError,
    capability::HarnessKind,
    command::{ApprovalDecision, MessageDelivery},
    model::{
        Approval, ApprovalAction, ApprovalDelivery, ApprovalKind, ApprovalState, Run, RunState,
        Task, TaskState, TimelineBody, TimelineItem,
    },
    time::Timestamp,
};
use std::time::Duration;

/// Asked this long ago and still unanswered, a request goes back to the
/// agent.
pub(crate) const CARRY_ON_AFTER: Duration = Duration::from_secs(10 * 60);
/// When each request went back to its agent unanswered, by approval.
pub(crate) const CARRIED_ON: &str = "approval_carried_on";
/// Advances only on an actual agent change, so answers created before it
/// cannot be reintroduced after switching back to the earlier agent.
pub(crate) const AGENT_GENERATION: &str = "task_agent_generation";

pub(crate) fn agent_generation(
    r: &store::Read<'_>,
    task_id: protocol::TaskId,
) -> store::Result<u64> {
    Ok(r.get::<u64>(AGENT_GENERATION, task_id)?.unwrap_or(0))
}
pub(crate) const PRIVATE_ANSWER_EXPIRED: &str =
    "This private question is no longer waiting. Ask the agent to request it again.";

/// What the agent is told in place of an answer.
pub(crate) const GUIDANCE: &str = "Nobody has answered yet, and Ophio doesn't let a run wait on a person while it holds the project. Don't ask again in this turn. Finish everything that doesn't depend on the answer, then end your turn with a short summary that says exactly what you still need. The answer will come as your next message.";
/// What the agent is told in place of an answer to a private question,
/// which can't be answered later.
pub(crate) const PRIVATE_GUIDANCE: &str = "Nobody has answered this private question, and it can't be answered after this. Don't ask again in this turn. Finish everything that doesn't depend on it, then end your turn with a short summary that says exactly what you still need, so the person can give it when you ask again.";

/// What may be answered after its agent has moved on, a request for more
/// access included. A login, a code, a post on GitHub and the computer
/// specialist's requests each keep their own time limit.
pub(crate) fn carries_on(kind: ApprovalKind) -> bool {
    matches!(
        kind,
        ApprovalKind::Command
            | ApprovalKind::FileEdit
            | ApprovalKind::Network
            | ApprovalKind::Tool
            | ApprovalKind::Question
            | ApprovalKind::Other
    )
}

/// Whether `a` may be answered after its agent has moved on. A private
/// question may not: its answer goes only to the live question, so it
/// expires as a login does, rather than stay open with nothing to answer.
pub(crate) fn outlives(a: &Approval) -> bool {
    carries_on(a.kind) && !private(a)
}

/// Whether `a` asks for something secret, a password say.
pub(crate) fn private(a: &Approval) -> bool {
    a.questions.iter().any(|q| q.secret)
}

/// Whether `a` still holds its run up: asked, unanswered, and not yet given
/// back to its agent.
pub(crate) fn blocks(r: &store::Read<'_>, a: &Approval) -> store::Result<bool> {
    Ok(a.state == ApprovalState::Pending && !carried_on(r, a.approval_id)?)
}

pub(crate) fn carried_on(r: &store::Read<'_>, id: ApprovalId) -> store::Result<bool> {
    Ok(r.get::<Timestamp>(CARRIED_ON, id)?.is_some())
}

/// Whether anything of `run`'s still holds it up.
pub(crate) fn run_blocked(r: &store::Read<'_>, run: protocol::RunId) -> store::Result<bool> {
    r.run_has_blocking_approval(run)
}

/// Withdraws unanswered requests from a different agent before a chat changes
/// agents. The old agent's questions and access requests must not become the
/// new agent's answers or permissions. A switch to the same agent keeps them.
pub(crate) fn withdraw_for_agent_switch(
    tx: &mut store::Write<'_>,
    task_id: protocol::TaskId,
    new_harness: HarnessKind,
    now: Timestamp,
) -> store::Result<()> {
    let generation = agent_generation(&tx.read(), task_id)?.saturating_add(1);
    tx.put(AGENT_GENERATION, task_id, &generation)?;
    crate::followups::cancel_for_agent_switch(tx, task_id, now)?;
    crate::asking::let_go(tx, task_id)?;
    for mut a in tx.read().list::<Approval>("approval")? {
        if a.task_id != task_id || a.state != ApprovalState::Pending {
            continue;
        }
        let old: Option<Run> = tx.read().get("run", a.run_id)?;
        if old.as_ref().is_some_and(|run| run.harness == new_harness) {
            continue;
        }
        tx.delete(CARRIED_ON, a.approval_id)?;
        a.state = ApprovalState::Cancelled;
        a.revision += 1;
        a.resolved_at = Some(now);
        tx.put("approval", a.approval_id, &a)?;
        tx.emit(Event::ApprovalUpserted {
            approval: a.clone(),
        })?;
        crate::runs::resolve_attention(tx, a.approval_id, now)?;
    }
    // A one-time Allow is scoped to its original run's agent, never to the
    // task id alone. Remove old and legacy passes at the agent boundary.
    crate::switch_work::let_go(tx, task_id)?;
    crate::runs::recount_attention(tx, task_id)
}

/// Whether a carried-over request still belongs to this task's agent.
/// A missing original run fails closed instead of handing its answer to an
/// unrelated agent.
pub(crate) fn same_agent_as_task(r: &store::Read<'_>, a: &Approval) -> store::Result<bool> {
    let task: Task = r.require("task", a.task_id)?;
    let asked: Option<Run> = r.get("run", a.run_id)?;
    Ok(asked.is_some_and(|run| run.harness == task.harness))
}

/// Withdraws `task_id`'s requests that its agent went on without, as the
/// task is archived.
pub(crate) fn let_go(
    tx: &mut store::Write<'_>,
    task_id: protocol::TaskId,
    now: Timestamp,
) -> store::Result<()> {
    for mut a in tx.read().list::<Approval>("approval")? {
        if a.task_id != task_id
            || a.state != ApprovalState::Pending
            || !carried_on(&tx.read(), a.approval_id)?
        {
            continue;
        }
        tx.delete(CARRIED_ON, a.approval_id)?;
        a.state = ApprovalState::Cancelled;
        a.revision += 1;
        a.resolved_at = Some(now);
        tx.put("approval", a.approval_id, &a)?;
        tx.emit(Event::ApprovalUpserted {
            approval: a.clone(),
        })?;
        crate::runs::resolve_attention(tx, a.approval_id, now)?;
    }
    Ok(())
}

/// Withdraws `task_id`'s requests that its agent went on without and that
/// offer to switch it to Work (or turn it into a task), but `except`, as the
/// task switches: nothing could take them forward after. The agent asks
/// again in Work if it still needs them.
pub(crate) fn let_go_switched(
    tx: &mut store::Write<'_>,
    task_id: protocol::TaskId,
    except: Option<ApprovalId>,
    now: Timestamp,
) -> store::Result<()> {
    for mut a in tx.read().list::<Approval>("approval")? {
        if a.task_id != task_id
            || Some(a.approval_id) == except
            || a.state != ApprovalState::Pending
            || !carried_on(&tx.read(), a.approval_id)?
        {
            continue;
        }
        let actions = match &a.actions {
            Some(actions) => actions.clone(),
            None => crate::approval_rule::Asked::of(&tx.read(), &a)?.actions(),
        };
        if !actions.iter().any(|action| {
            matches!(
                action,
                ApprovalAction::SwitchToWork | ApprovalAction::TurnIntoTask
            )
        }) {
            continue;
        }
        tx.delete(CARRIED_ON, a.approval_id)?;
        a.state = ApprovalState::Cancelled;
        a.revision += 1;
        a.resolved_at = Some(now);
        tx.put("approval", a.approval_id, &a)?;
        tx.emit(Event::ApprovalUpserted {
            approval: a.clone(),
        })?;
        crate::runs::resolve_attention(tx, a.approval_id, now)?;
        note(
            tx,
            &a,
            now,
            format!(
                "“{}” was withdrawn as the task switched to Work: the agent asks again there if it still needs it.",
                a.title
            ),
        )?;
    }
    Ok(())
}

/// Keeps open what `run_id`'s agent asked through Ophio's bridge and
/// stopped waiting for, as if it had gone back to the agent: its tool call
/// gave up, its turn ended, or its connection to Ophio closed. Only requests
/// that carry on, and only from a turn nobody stopped. Its native requests
/// are the agent's own to end. The ids kept open.
pub(crate) fn outlive_wait(
    tx: &mut store::Write<'_>,
    run_id: protocol::RunId,
    now: Timestamp,
) -> store::Result<Vec<ApprovalId>> {
    let mut kept = Vec::new();
    for a in tx.read().run_pending_approvals(run_id)? {
        if keep(tx, &a, now)? {
            kept.push(a.approval_id);
        }
    }
    if !kept.is_empty() {
        back_to_work(tx, run_id, now)?;
    }
    Ok(kept)
}

/// Keeps `a` open as [`outlive_wait`] does, if it may be. An answer given
/// as its agent stopped waiting is settled as it is handed over: one that
/// no longer reaches the wait becomes the agent's next message
/// ([`CompanionHub::resolve_approval`]).
pub(crate) fn keep(tx: &mut store::Write<'_>, a: &Approval, now: Timestamp) -> store::Result<bool> {
    // A person's Stop withdrew what it asked; Ophio's own gate before a
    // turn starts asks again each turn.
    if (tx.read())
        .get::<bool>("stopped", a.run_id)?
        .unwrap_or(false)
        || !outlives(a)
        || !same_agent_as_task(&tx.read(), a)?
        || !blocks(&tx.read(), a)?
        || crate::approval_rule::gate(&tx.read(), a)?
        || !(tx.read())
            .get::<bool>("bridge_approval", a.approval_id)?
            .unwrap_or(false)
    {
        return Ok(false);
    }
    tx.put(CARRIED_ON, a.approval_id, &now)?;
    let answering = (tx.read())
        .get::<protocol::CommandId>("approval_claim", a.approval_id)?
        .is_some();
    let text = match answering {
        true => format!(
            "The agent stopped waiting just as “{}” was answered. If the answer didn't reach it, it goes to it as its next message.",
            a.title
        ),
        false => format!(
            "The agent stopped waiting before anyone answered, so the request stays open. Answering still reaches it: “{}”",
            a.title
        ),
    };
    note(tx, a, now, text)?;
    Ok(true)
}

/// `run_id`, if it waited on the person and nothing else holds it, is
/// working again.
pub(crate) fn back_to_work(
    tx: &mut store::Write<'_>,
    run_id: protocol::RunId,
    now: Timestamp,
) -> store::Result<()> {
    let mut run: Run = tx.read().require("run", run_id)?;
    if run.state != RunState::AwaitingApproval || run_blocked(&tx.read(), run_id)? {
        return Ok(());
    }
    let mut task: Task = tx.read().require("task", run.task_id)?;
    run.state = RunState::Working;
    task.state = TaskState::Working;
    task.revision += 1;
    task.updated_at = now;
    tx.put("run", run.run_id, &run)?;
    tx.emit(Event::RunUpserted { run: Box::new(run) })?;
    tx.put("task", task.task_id, &task)?;
    tx.emit(Event::TaskUpserted { task })?;
    Ok(())
}

pub(crate) fn note(
    tx: &mut store::Write<'_>,
    a: &Approval,
    now: Timestamp,
    text: String,
) -> store::Result<()> {
    tx.emit(Event::TimelineAppended {
        task_id: a.task_id,
        item: TimelineItem {
            seq: 0,
            at: now,
            run_id: Some(a.run_id),
            item: TimelineBody::Note { text },
        },
    })?;
    Ok(())
}

impl CompanionHub {
    /// Gives back to their agents the requests of running turns that nobody
    /// has answered in [`CARRY_ON_AFTER`].
    pub(crate) async fn carry_on_unanswered(&self) -> Result<usize, ProtocolError> {
        let live: Vec<protocol::RunId> = self.inner.runs.lock().await.keys().copied().collect();
        if live.is_empty() {
            return Ok(0);
        }
        let now = self.inner.clock.now();
        let after = CARRY_ON_AFTER.as_millis() as i64;
        let due = self.read(|r| {
            let mut due = Vec::new();
            for run in &live {
                for a in r.run_pending_approvals(*run)? {
                    if carries_on(a.kind) && now.0.saturating_sub(a.requested_at.0) >= after {
                        due.push(a);
                    }
                }
            }
            // Preserve the old global request order across the per-run reads.
            due.sort_unstable_by_key(|a| a.approval_id);
            Ok(due)
        })?;
        let mut count = 0;
        for a in due {
            match self.carry_on(&a).await {
                Ok(true) => count += 1,
                Ok(false) => {}
                Err(e) => tracing::warn!(code = ?e.code, "giving a request back failed"),
            }
        }
        Ok(count)
    }

    /// Tells the agent to go on without an answer to `a`. Marked first, so
    /// an answer given meanwhile is kept for the agent's next message rather
    /// than handed to a wait that has just ended. A private question isn't
    /// kept: it expires once its agent has been told.
    async fn carry_on(&self, a: &Approval) -> Result<bool, ProtocolError> {
        if self.run_stopped(a.run_id)? {
            return Ok(false);
        }
        let id = a.approval_id;
        let now = self.inner.clock.now();
        let marked = self.write(|tx| {
            let stored: Approval = tx.read().require("approval", id)?;
            if !blocks(&tx.read(), &stored)? {
                return Ok(false);
            }
            tx.put(CARRIED_ON, id, &now)?;
            Ok(true)
        })?;
        if !marked {
            return Ok(false);
        }
        let private = private(a);
        let guidance = if private { PRIVATE_GUIDANCE } else { GUIDANCE };
        let deny = ApprovalDecision::Deny {
            reason: Some(guidance.into()),
        };
        let wait = self.inner.waits.lock().await.remove(&id);
        let handed = match wait {
            Some(wait) => wait.send(deny).is_ok(),
            // A bridge request whose wait has gone is closed by the bridge.
            None if self
                .read(|r| Ok(r.get::<bool>("bridge_approval", id)?.unwrap_or(false)))? =>
            {
                false
            }
            None => {
                self.decline_natively(a.run_id, &a.native_request_id, guidance)
                    .await
            }
        };
        let carried = self.write(|tx| {
            if !handed {
                tx.delete(CARRIED_ON, id)?;
                return Ok(false);
            }
            if private {
                tx.delete(CARRIED_ON, id)?;
                let mut stored: Approval = tx.read().require("approval", id)?;
                if stored.state == ApprovalState::Pending {
                    stored.state = ApprovalState::Expired;
                    stored.revision += 1;
                    tx.put("approval", id, &stored)?;
                    tx.emit(Event::ApprovalUpserted { approval: stored })?;
                    crate::runs::resolve_attention(tx, id, now)?;
                    crate::runs::recount_attention(tx, a.task_id)?;
                }
                back_to_work(tx, a.run_id, now)?;
                note(
                    tx,
                    a,
                    now,
                    format!(
                        "Nobody answered the private question “{}” in {} minutes, so it expired and Ophio told the agent to carry on without it. It asks again if it still needs the answer.",
                        a.title,
                        CARRY_ON_AFTER.as_secs() / 60
                    ),
                )?;
                return Ok(true);
            }
            back_to_work(tx, a.run_id, now)?;
            note(
                tx,
                a,
                now,
                format!(
                    "Nobody answered “{}” in {} minutes, so Ophio told the agent to carry on without it and end its turn. Answering still reaches it.",
                    a.title,
                    CARRY_ON_AFTER.as_secs() / 60
                ),
            )?;
            Ok(true)
        })?;
        Ok(carried)
    }

    /// Declines `run_id`'s own request `native_request_id`, which drops any
    /// reason, and tells the agent `why` in its turn. Whether it may have
    /// reached the agent.
    pub(crate) async fn decline_natively(
        &self,
        run_id: protocol::RunId,
        native_request_id: &str,
        why: &str,
    ) -> bool {
        let Some(control) = self.inner.runs.lock().await.get(&run_id).cloned() else {
            return false;
        };
        let deny = ApprovalDecision::Deny {
            reason: Some(why.into()),
        };
        let declined = tokio::time::timeout(
            Duration::from_secs(5),
            control.resolve_approval(native_request_id, &deny),
        )
        .await;
        match declined {
            Ok(Ok(())) => {}
            // It may have reached the agent; it is not asked again.
            Err(_) => return true,
            Ok(Err(_)) => return false,
        }
        let _ = tokio::time::timeout(
            Duration::from_secs(5),
            control.send_message(
                MessageRef(CommandId::new()),
                PromptInput {
                    text: why.into(),
                    images: Vec::new(),
                },
                MessageDelivery::Steer,
            ),
        )
        .await;
        true
    }

    /// Records the answer to a request its agent went on without, and sends
    /// it to the task as its next message.
    pub(crate) fn answer_later(
        &self,
        caller: &Caller,
        id: ApprovalId,
        decision: &ApprovalDecision,
    ) -> Result<(), ProtocolError> {
        let now = self.inner.clock.now();
        let private_answer = self.write(|tx| {
            let mut a: Approval = tx.read().require("approval", id)?;
            if a.state != ApprovalState::Pending {
                return Err(error(ErrorCode::AlreadyResolved, "Approval already resolved").into());
            }
            if !same_agent_as_task(&tx.read(), &a)? {
                return Err(error(
                    ErrorCode::Expired,
                    "This request came from a different agent. Ask the current agent again.",
                )
                .into());
            }
            // A follow-up becomes durable conversation history. A secret may
            // only go through its original, still-live native question.
            if a.questions.iter().any(|q| q.secret)
                && matches!(decision, ApprovalDecision::Answer { .. })
            {
                // Carry-on may have won after admission claimed this request.
                // Commit the release before returning the execution error, so
                // the person can still dismiss the obsolete question.
                tx.delete("approval_claim", id)?;
                return Ok(true);
            }
            a.state = if matches!(decision, ApprovalDecision::Deny { .. }) {
                ApprovalState::Denied
            } else {
                ApprovalState::Allowed
            };
            a.delivery = Some(ApprovalDelivery {
                decision: decision.clone(),
                at: now,
                error: None,
            });
            a.resolved_at = Some(now);
            a.resolved_by = Some(caller.principal);
            a.revision += 1;
            tx.put("approval", id, &a)?;
            tx.emit(Event::ApprovalUpserted {
                approval: a.clone(),
            })?;
            crate::runs::resolve_attention(tx, id, now)?;
            tx.delete(CARRIED_ON, id)?;
            // Told it may, the agent asks again: the same request is
            // allowed then without a second card, once. Told no, it isn't.
            match decision {
                ApprovalDecision::Allow if crate::switch_work::passes_on(&tx.read(), &a)? => {
                    crate::switch_work::preapprove(tx, &a, caller.principal, true, now)?;
                }
                ApprovalDecision::Deny { .. } => crate::switch_work::declined(tx, &a)?,
                _ => {}
            }
            Followup::answer(a.task_id, id, later_text(&a, decision), now).put(tx)?;
            Ok(false)
        })?;
        if private_answer {
            return Err(error(ErrorCode::Expired, PRIVATE_ANSWER_EXPIRED));
        }
        self.wake_guard();
        Ok(())
    }
}

/// The answer as the agent reads it, with what it asked.
pub(crate) fn later_text(a: &Approval, decision: &ApprovalDecision) -> String {
    let asked = match a.questions.as_slice() {
        [] => match a.detail.as_deref().map(str::trim) {
            Some(detail) if a.kind == ApprovalKind::Question && !detail.is_empty() => detail,
            _ => a.title.trim(),
        },
        [only] => only.question.trim(),
        _ => a.title.trim(),
    };
    let asked: String = asked.chars().take(400).collect();
    let question = a.kind == ApprovalKind::Question;
    let what = if question { "question" } else { "request" };
    match decision {
        ApprovalDecision::Answer { answers, .. } if !answers.is_empty() => {
            // Use the recorded questions, not answer-array order or the
            // free-form summary: repeated wording must keep its association.
            let mut lines: Vec<_> = answers
                .iter()
                .map(|answer| {
                    let asked = a.questions.iter().enumerate().find(|(_, q)| {
                        q.question == answer.question && (answer.id.is_none() || answer.id == q.id)
                    });
                    let choices = answer.choices.join(", ");
                    match asked {
                        Some((index, q)) => {
                            let id =
                                q.id.as_ref()
                                    .map(|id| format!(" [{id}]"))
                                    .unwrap_or_default();
                            let header = q
                                .header
                                .as_ref()
                                .map(|h| format!(" ({h})"))
                                .unwrap_or_default();
                            (
                                index,
                                format!(
                                    "Question {}{id}{header} {}: {choices}",
                                    index + 1,
                                    q.question
                                ),
                            )
                        }
                        None => (usize::MAX, format!("{}: {choices}", answer.question)),
                    }
                })
                .collect();
            lines.sort_by_key(|(index, _)| *index);
            let lines: Vec<_> = lines.into_iter().map(|(_, text)| text).collect();
            format!(
                "Answering your earlier {what}, “{asked}”:\n{}",
                lines.join("\n")
            )
        }
        ApprovalDecision::Answer { text, .. } => {
            format!("Answering your earlier {what}, “{asked}”: {text}")
        }
        ApprovalDecision::Allow if question => {
            format!("About your earlier question, “{asked}”: yes.")
        }
        ApprovalDecision::Allow => format!(
            "About your earlier request, “{asked}”: yes, it's allowed now. Do it if it's still needed."
        ),
        ApprovalDecision::Deny { reason } => {
            let reason = reason.as_deref().map(str::trim).filter(|r| !r.is_empty());
            match reason {
                Some(reason) => format!("About your earlier {what}, “{asked}”: no. {reason}"),
                None => format!("About your earlier {what}, “{asked}”: no. Carry on without it."),
            }
        }
    }
}
