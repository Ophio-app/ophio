// SPDX-License-Identifier: Apache-2.0
//! No run waits on a person while it holds the project. A question or an
//! approval nobody has answered in [`CARRY_ON_AFTER`] goes back to the agent
//! with word to finish what doesn't depend on it and end its turn, and the
//! project is free once it has. The request stays open: an answer given
//! later reaches the agent as its next message, and starts the task again
//! if its turn has ended.

use crate::{CompanionHub, api::Caller, error, followups::Followup};
use ports::{MessageRef, PromptInput};
use protocol::{
    ApprovalId, CommandId, ErrorCode, Event, ProtocolError,
    command::{ApprovalDecision, MessageDelivery},
    model::{
        Approval, ApprovalDelivery, ApprovalKind, ApprovalState, Run, RunState, Task, TaskState,
        TimelineBody, TimelineItem,
    },
    time::Timestamp,
};
use std::time::Duration;

/// Asked this long ago and still unanswered, a request goes back to the
/// agent.
pub(crate) const CARRY_ON_AFTER: Duration = Duration::from_secs(10 * 60);
/// When each request went back to its agent unanswered, by approval.
pub(crate) const CARRIED_ON: &str = "approval_carried_on";
pub(crate) const PRIVATE_ANSWER_EXPIRED: &str =
    "This private question is no longer waiting. Ask the agent to request it again.";

/// What the agent is told in place of an answer.
pub(crate) const GUIDANCE: &str = "Nobody has answered yet, and Ophio doesn't let a run wait on a person while it holds the project. Don't ask again in this turn. Finish everything that doesn't depend on the answer, then end your turn with a short summary that says exactly what you still need. The answer will come as your next message.";

/// What may be answered after its agent has moved on. A login, a code, a
/// post on GitHub and the computer specialist's requests each keep their
/// own time limit.
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
    /// than handed to a wait that has just ended.
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
        let deny = ApprovalDecision::Deny {
            reason: Some(GUIDANCE.into()),
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
                self.decline_natively(a.run_id, &a.native_request_id, GUIDANCE)
                    .await
            }
        };
        let carried = self.write(|tx| {
            if !handed {
                tx.delete(CARRIED_ON, id)?;
                return Ok(false);
            }
            let mut run: Run = tx.read().require("run", a.run_id)?;
            if run.state == RunState::AwaitingApproval && !run_blocked(&tx.read(), a.run_id)? {
                let mut task: Task = tx.read().require("task", run.task_id)?;
                run.state = RunState::Working;
                task.state = TaskState::Working;
                task.revision += 1;
                task.updated_at = now;
                tx.put("run", run.run_id, &run)?;
                tx.emit(Event::RunUpserted {
                    run: Box::new(run),
                })?;
                tx.put("task", task.task_id, &task)?;
                tx.emit(Event::TaskUpserted { task })?;
            }
            tx.emit(Event::TimelineAppended {
                task_id: a.task_id,
                item: TimelineItem {
                    seq: 0,
                    at: now,
                    run_id: Some(a.run_id),
                    item: TimelineBody::Note {
                        text: format!(
                            "Nobody answered “{}” in {} minutes, so Ophio told the agent to carry on without it and end its turn. Answering still reaches it.",
                            a.title,
                            CARRY_ON_AFTER.as_secs() / 60
                        ),
                    },
                },
            })?;
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
