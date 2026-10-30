// SPDX-License-Identifier: Apache-2.0
//! Watching, not working. An agent asked to keep an eye on something (a
//! pull request's checks, a deploy, a reply) doesn't wait in its turn, where
//! it would hold the project: it asks Ophio to wake it, and ends its turn.
//! Meanwhile the task shows as watching and the project is free. At the time
//! it asked for, Ophio starts its next turn with what it was waiting for.

use crate::{
    CompanionHub, error,
    followups::{FOLLOWUP, Followup, wake_key},
};
use protocol::{
    ErrorCode, Event, ProtocolError, RunId, TaskId,
    model::{Approval, Run, Task, TaskWake, TimelineBody, TimelineItem},
    time::Timestamp,
};

/// How the turn a wake starts opens.
pub(crate) const WAKE: &str = "You asked Ophio to wake you:";

impl CompanionHub {
    /// Wakes `task`'s agent in `minutes`, for `reason`. A newer wake
    /// replaces an older one.
    pub(crate) fn wake_me(
        &self,
        run_id: RunId,
        task: &Task,
        minutes: u32,
        reason: String,
    ) -> Result<String, ProtocolError> {
        if self.run_stopped(run_id)? {
            return Err(error(ErrorCode::Expired, "Run is no longer active"));
        }
        let now = self.inner.clock.now();
        let at = Timestamp(now.0.saturating_add(i64::from(minutes) * 60_000));
        self.write(|tx| {
            let text = format!(
                "{WAKE} {reason}\nCheck on it now. If it still isn't ready, call wake_me again and end your turn; otherwise carry on."
            );
            Followup::wake(task.task_id, text, at, now).put(tx)?;
            let mut t: Task = tx.read().require("task", task.task_id)?;
            t.wake = Some(TaskWake {
                at,
                reason: reason.clone(),
                answer: false,
            });
            t.updated_at = now;
            tx.put("task", t.task_id, &t)?;
            tx.emit(Event::TaskUpserted { task: t })?;
            tx.emit(Event::TimelineAppended {
                task_id: task.task_id,
                item: TimelineItem {
                    seq: 0,
                    at: now,
                    run_id: Some(run_id),
                    item: TimelineBody::Note {
                        text: format!("Watching: {reason}. Ophio wakes the agent in {}.", span(minutes)),
                    },
                },
            })?;
            Ok(())
        })?;
        self.wake_guard();
        Ok(format!(
            "Ophio will wake you in {}. End your turn now with a short summary; don't wait, sleep or poll in this turn. The project is free meanwhile, and the person sees this task as watching.",
            span(minutes)
        ))
    }
}

/// Clears `task_id`'s wake, so the task no longer shows as watching.
pub(crate) fn clear_wake(
    tx: &mut store::Write<'_>,
    task_id: TaskId,
    now: Timestamp,
) -> store::Result<()> {
    let Some(mut task) = tx.read().get::<Task>("task", task_id)? else {
        return Ok(());
    };
    if task.wake.take().is_none() {
        return Ok(());
    }
    task.updated_at = now;
    tx.put("task", task_id, &task)?;
    tx.emit(Event::TaskUpserted { task })?;
    Ok(())
}

/// Drops `task_id`'s wake and clears it. Whether there was one.
pub(crate) fn stop_watching(
    tx: &mut store::Write<'_>,
    task_id: TaskId,
    now: Timestamp,
) -> store::Result<bool> {
    let key = wake_key(task_id);
    let had = tx.read().get::<Followup>(FOLLOWUP, &key)?.is_some();
    tx.delete(FOLLOWUP, &key)?;
    let watching = tx
        .read()
        .get::<Task>("task", task_id)?
        .is_some_and(|t| t.wake.is_some());
    clear_wake(tx, task_id, now)?;
    Ok(had || watching)
}

/// What the person reads when `run_id`, `task`'s turn, has finished: that
/// it is watching something, or still needs an answer it went on without.
pub(crate) fn ended_body(
    r: &store::Read<'_>,
    task: &Task,
    run_id: RunId,
    now: Timestamp,
) -> store::Result<Option<String>> {
    let waiting = went_on_without(r, run_id)?;
    let wake = wake_of(r, task)?;
    let listed = crate::checklist::waited_on(r, task.project_id, task.task_id)?;
    Ok(match (wake, waiting) {
        (_, _) if !listed.is_empty() => Some(match listed.as_slice() {
            [one] => format!(
                "Waiting for you to finish “{one}” on the checklist. The agent carries on as soon as you tick it off; the project is free meanwhile."
            ),
            many => format!(
                "Waiting for you to finish {} items on the checklist. The agent carries on as soon as you tick one off; the project is free meanwhile.",
                many.len()
            ),
        }),
        (Some(wake), _) if wake.answer => Some(format!(
            "Waiting for {}. Ophio wakes the agent when it comes; the project is free meanwhile.",
            wake.reason.trim_end_matches('.')
        )),
        (Some(wake), _) => {
            let minutes = (wake.at.0.saturating_sub(now.0).max(0) + 59_999) / 60_000;
            Some(format!(
                "Watching: {}. Ophio wakes the agent in {}; the project is free meanwhile.",
                wake.reason.trim_end_matches('.'),
                span(minutes.max(1) as u32)
            ))
        }
        (None, 0) => r
            .get::<Run>("run", run_id)?
            .and_then(|run| crate::reported::ended(&run)),
        (None, 1) => Some("Finished what it could. It still needs your answer.".into()),
        (None, _) => Some("Finished what it could. It still needs your answers.".into()),
    })
}

/// Whether `task`'s turn ended watching: Ophio wakes it, so there is
/// nothing to raise for the person. What it went on without was raised as
/// it was asked, and stays open. Waiting on the person's checklist is
/// theirs to do, so it is raised.
pub(crate) fn quietly_watching(r: &store::Read<'_>, task: &Task) -> store::Result<bool> {
    let listed = crate::checklist::waited_on(r, task.project_id, task.task_id)?;
    Ok(listed.is_empty() && wake_of(r, task)?.is_some())
}

/// What still holds the turn is withdrawn as it ends; what it went on
/// without stays open. How many of those.
fn went_on_without(r: &store::Read<'_>, run_id: RunId) -> store::Result<usize> {
    let mut waiting = 0;
    for a in r.list::<Approval>("approval")? {
        if a.run_id == run_id
            && a.state == protocol::model::ApprovalState::Pending
            && crate::carry_on::carried_on(r, a.approval_id)?
        {
            waiting += 1;
        }
    }
    Ok(waiting)
}

fn wake_of(r: &store::Read<'_>, task: &Task) -> store::Result<Option<protocol::model::TaskWake>> {
    Ok(r.get::<Task>("task", task.task_id)?
        .and_then(|t| t.wake)
        .or_else(|| task.wake.clone()))
}

/// `minutes` as people say it.
fn span(minutes: u32) -> String {
    match minutes {
        1 => "1 minute".into(),
        m if m < 120 => format!("{m} minutes"),
        m if m % 60 == 0 => format!("{} hours", m / 60),
        m => format!("{} h {} min", m / 60, m % 60),
    }
}

/// Whether `task_id` is watching with no turn running, so Stop stops the
/// watching rather than a turn.
pub(crate) fn between_turns(r: &store::Read<'_>, task_id: TaskId) -> store::Result<bool> {
    let Some(task) = r.get::<Task>("task", task_id)? else {
        return Ok(false);
    };
    let watching =
        task.wake.is_some() || r.get::<Followup>(FOLLOWUP, &wake_key(task_id))?.is_some();
    if !watching {
        return Ok(false);
    }
    Ok(match task.current_run {
        None => true,
        Some(id) => r
            .get::<protocol::model::Run>("run", id)?
            .is_none_or(|run| run.state.is_terminal() && !crate::runs::still_finishing(&run)),
    })
}
