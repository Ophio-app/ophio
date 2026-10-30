// SPDX-License-Identifier: Apache-2.0
//! One agent asking another: Claude Code asking Codex to review its work,
//! say, or Codex asking Claude Code. An agent's own subagent tool runs only
//! its own models, so Ophio starts the agent asked as a task of its own in
//! the same project, read-only, and passes its last message back as the
//! asking agent's next message. Meanwhile the asking task shows as waiting
//! for the answer, and the task asked raises nothing for the person.

use crate::followups::{FOLLOWUP, Followup, FollowupKind};
use crate::github_tools::via;
use crate::github_watch::as_creator;
use crate::work_tools::Allowance;
use crate::{CompanionHub, error};
use ports::bridge::AgentChoice;
use protocol::command::{Command, CommandEnvelope, CommandResult, TaskPlacement};
use protocol::model::{
    Run, RunState, Task, TaskKind, TaskMode, TaskState, TaskWake, TimelineBody, TimelineItem,
};
use protocol::time::Timestamp;
use protocol::{ErrorCode, Event, ProtocolError, RunId, TaskId};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// Who asked, by the task asked.
const CONSULT: &str = "consult";
/// How long Ophio waits for an answer before it wakes the asking agent
/// anyway.
const WAIT_MS: i64 = 24 * 3_600_000;
/// The most of an answer passed on; the task asked keeps all of it.
const MAX_ANSWER: usize = 20_000;
/// How far back an answer is looked for on the task asked.
const LOOK_BACK: usize = 50;

#[derive(Serialize, Deserialize)]
struct Consult {
    /// The task whose agent asked.
    caller: TaskId,
    title: String,
    /// The agent asked, as the asking agent reads it.
    by: String,
}

/// Tasks whose helper has not answered yet, or whose answer is still to
/// be delivered. The consultation becomes a reply in the same transaction,
/// so a finished helper never leaves a gap in this hold on automatic merging.
pub(crate) fn pending_callers(r: &store::Read<'_>) -> store::Result<HashSet<TaskId>> {
    let mut callers: HashSet<_> = r
        .list::<Consult>(CONSULT)?
        .into_iter()
        .map(|c| c.caller)
        .collect();
    callers.extend(
        r.list::<Followup>(FOLLOWUP)?
            .into_iter()
            .filter(|f| f.kind == FollowupKind::Reply)
            .map(|f| f.task_id),
    );
    Ok(callers)
}

/// `text` cut to `max` characters.
fn cut(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", text[..i].trim_end()),
        None => text.to_owned(),
    }
}

impl CompanionHub {
    /// Starts `agent` on `prompt`, read-only, in `task`'s project, and has
    /// its answer passed back to `task`'s agent once it ends.
    pub(crate) async fn ask_agent(
        &self,
        run_id: RunId,
        task: &Task,
        agent: AgentChoice,
        prompt: String,
        title: Option<String>,
    ) -> Result<String, ProtocolError> {
        if self.run_stopped(run_id)? {
            return Err(error(ErrorCode::Expired, "Run is no longer active"));
        }
        let choice = self.chosen_agent(task, Some(agent))?;
        let by = via(choice.harness, choice.model.as_deref());
        let asker = self.this_run(run_id, task)?;
        let title = title.unwrap_or_else(|| cut(&format!("Question from “{}”", task.title), 79));
        let prompt = format!(
            "{asker} is working on “{}” in this project and asks for your help. It can't see this conversation and you can't see its; Ophio passes your last message back to it as your answer.\n\n{prompt}\n\nYou're read-only here: read the project, don't change it. End with your whole answer in your last message.",
            task.title
        );
        let envelope = CommandEnvelope::new(Command::CreateTask {
            project_id: task.project_id,
            kind: TaskKind::Coding,
            mode: Some(TaskMode::Ask),
            title: Some(title.clone()),
            prompt,
            bundle: None,
            harness: choice.harness,
            model: choice.model.clone(),
            effort: choice.effort.clone(),
            // The person's launch flags could undo read-only.
            launch_policy: None,
            branch: None,
            start: None,
            place: None,
            browser: Some(false),
            github: None,
            mission: None,
            target_id: None,
            placement: TaskPlacement::StartNow,
            agent_chosen: None,
            conversation: None,
            // Asked by a conversation held to its access, it reads no more.
            access: crate::access::held_to(crate::access::held(task), true),
            review: None,
        });
        self.take_planned(run_id, Allowance::Ask)?;
        let receipt = self.admit(&as_creator(task), envelope).await;
        let helper = match (receipt.error, receipt.result) {
            (None, Some(CommandResult::TaskCreated { task_id, .. })) => task_id,
            (e, _) => {
                self.give_back_planned(run_id, Allowance::Ask);
                return Err(e.unwrap_or_else(|| error(ErrorCode::Internal, "Nobody was asked")));
            }
        };
        let now = self.inner.clock.now();
        self.write(|tx| {
            let consult = Consult {
                caller: task.task_id,
                title: title.clone(),
                by: by.clone(),
            };
            tx.put(CONSULT, helper, &consult)?;
            if let Some(mut asked) = tx.read().get::<Task>("task", helper)? {
                asked.asked_by = Some(task.task_id);
                tx.put("task", helper, &asked)?;
                tx.emit(Event::TaskUpserted { task: asked })?;
            }
            let mut caller: Task = tx.read().require("task", task.task_id)?;
            // A wake it asked for stays: that wakes it anyway.
            if caller.wake.is_none() {
                let at = Timestamp(now.0.saturating_add(WAIT_MS));
                let text = format!(
                    "At least a day ago you asked {by} “{title}”, and Ophio has had no answer. Look at its task in Tasks, or carry on without it."
                );
                Followup::wake(task.task_id, text, at, now).put(tx)?;
                caller.wake = Some(TaskWake {
                    at,
                    reason: format!("{by}’s answer to “{title}”"),
                    answer: true,
                });
                caller.updated_at = now;
                tx.put("task", caller.task_id, &caller)?;
                tx.emit(Event::TaskUpserted { task: caller })?;
            }
            tx.emit(Event::TimelineAppended {
                task_id: task.task_id,
                item: TimelineItem {
                    seq: 0,
                    at: now,
                    run_id: Some(run_id),
                    item: TimelineBody::Note {
                        text: format!("Asked {by}: “{title}”. Its answer comes back here."),
                    },
                },
            })?;
            Ok(())
        })?;
        self.wake_guard();
        Ok(format!(
            "Asked {by} in “{title}”. It reads the project and changes nothing. Its answer comes to you as a message from Ophio once it's done: keep working meanwhile, or end your turn if you have nothing else to do, and Ophio starts your next turn with it. Don't wait, sleep or poll for it."
        ))
    }
}

/// When `run_id`, `task`'s turn, ended `state` and another agent asked
/// it, passes its answer back to that agent. Whether it was asked: what it
/// answered is the asking agent's, not the person's, to read.
pub(crate) fn answered(
    tx: &mut store::Write<'_>,
    task: &Task,
    run_id: RunId,
    state: RunState,
    now: Timestamp,
) -> store::Result<bool> {
    let Some(c) = tx.read().get::<Consult>(CONSULT, task.task_id)? else {
        return Ok(false);
    };
    tx.delete(CONSULT, task.task_id)?;
    let text = match state {
        RunState::Completed => match last_message(&tx.read(), task.task_id, run_id)? {
            Some(answer) => format!(
                "{} answered what you asked in “{}”:\n\n{}",
                c.by,
                c.title,
                cut(&answer, MAX_ANSWER)
            ),
            None => format!(
                "{} finished what you asked in “{}” without an answer. Carry on without it, or ask again.",
                c.by, c.title
            ),
        },
        RunState::Interrupted => format!(
            "{} couldn't answer what you asked in “{}”: it was stopped. Carry on without it, or ask again.",
            c.by, c.title
        ),
        _ => format!(
            "{} couldn't answer what you asked in “{}”: it failed. Carry on without it, or ask again.",
            c.by, c.title
        ),
    };
    let waiting =
        (tx.read().get::<Task>("task", c.caller)?).is_some_and(|t| t.state != TaskState::Archived);
    if waiting {
        Followup::reply(c.caller, task.task_id, text, now).put(tx)?;
    }
    Ok(true)
}

/// The last thing `run_id` said, without its report for Ophio.
pub(crate) fn last_message(
    r: &store::Read<'_>,
    task_id: TaskId,
    run_id: RunId,
) -> store::Result<Option<String>> {
    let (page, _) = r.timeline(task_id, None, LOOK_BACK)?;
    let said = page.into_iter().rev().find_map(|item| match item.item {
        TimelineBody::AgentMessage { text, .. } if item.run_id == Some(run_id) => Some(text),
        _ => None,
    });
    let said = said.or_else(|| {
        (r.get::<Run>("run", run_id).ok().flatten())
            .and_then(|run| run.outcome)
            .and_then(|o| o.summary)
    });
    Ok(said
        .map(|text| crate::reported::take(&text).0.trim().to_owned())
        .filter(|text| !text.is_empty()))
}

/// Once an answer has gone to `caller`, or couldn't, it waits for no other
/// it asked for: it stops showing as waiting.
pub(crate) fn passed_on(
    tx: &mut store::Write<'_>,
    caller: TaskId,
    now: Timestamp,
) -> store::Result<()> {
    let asked = (tx.read().entries::<Consult>(CONSULT)?.iter()).any(|(_, c)| c.caller == caller);
    let waiting = (tx.read().get::<Task>("task", caller)?)
        .and_then(|t| t.wake)
        .is_some_and(|w| w.answer);
    if waiting && !asked {
        crate::watching::stop_watching(tx, caller, now)?;
        // What it waits on in the checklist it still waits on.
        crate::checklist::refresh_wait(tx, caller, now)?;
    }
    Ok(())
}

/// The person stopped or archived `caller`: answers to what it asked go to
/// the person, in the tasks asked, and none starts it again.
pub(crate) fn let_go(tx: &mut store::Write<'_>, caller: TaskId) -> store::Result<()> {
    for (key, f) in tx.read().entries::<Followup>(FOLLOWUP)? {
        if f.task_id == caller && f.kind == FollowupKind::Reply {
            tx.delete(FOLLOWUP, &key)?;
        }
    }
    for (key, c) in tx.read().entries::<Consult>(CONSULT)? {
        if c.caller == caller {
            tx.delete(CONSULT, &key)?;
        }
    }
    Ok(())
}
