// SPDX-License-Identifier: Apache-2.0
//! Work a chat's agent proposes with `ophio_propose`: one task, an ordered
//! set of them, or fixing every open issue as Run all does. A proposal
//! starts nothing. It is kept for the turn and, when the turn ends, joins
//! what the turn reported (`RunReport.new_work`), so the person sees it as a
//! card and starts it with a tap, exactly as written (`work_card_binding`).
//!
//! A project the person trusts may let what its chats' agents propose start
//! without asking (`Project.auto_start`, theirs alone to set). Then, once a
//! chat's turn ends, the computer starts what the turn proposed itself, as
//! the card's Start would: only work in that project, only what needs no
//! more than the internet, never more in one reply or more at once than the
//! person allowed, and always stopping at the pull request for the person.
//! Only after a turn the person started with their own message, in Work, in
//! a chat with full access, while Next up isn't paused ([`may_start_alone`],
//! the one place that decides it). Anything else waits on its card.

use crate::github::SETTINGS;
use crate::github_watch::as_creator;
use crate::{CompanionHub, error};
use protocol::command::{Command, CommandEnvelope, TaskPlacement};
use protocol::github::{GithubLink, GithubMode, GithubSettings};
use protocol::model::{
    NewWork, Project, ReportStatus, Run, RunOutcome, RunReport, RunState, Task, TaskKind, TaskMode,
    TaskState,
};
use protocol::queue::{QueueAuthorization, QueueView};
use protocol::work_cards::{AutoStart, TaskOrigin};
use protocol::{ErrorCode, ProjectId, ProtocolError, QueueItemId, RunId, TaskId};
use serde_json::Value;

/// What a turn proposed so far, by run.
const PROPOSED: &str = "proposed";
/// A start the computer makes on a project's go-ahead, by command: the only
/// way a `CreateTask` whose origin says `auto` is let through.
pub(crate) const AUTO_COMMAND: &str = "auto_start_command";
/// At most this many suggestions one turn proposes, as a report holds.
const MAX: usize = 8;
/// For each suggestion a turn proposed so far, by run, whether its agent was
/// told it may start by itself. Only those may.
const PROPOSED_ALONE: &str = "proposed_alone";
/// Turns the person started with their own message, by run: typed, a reply
/// from a notification, their message waiting in Next up, or their Steer.
/// Whatever Ophio starts itself (a wake-up, a helper's reply, a checklist's
/// next step) is never here.
pub(crate) const PERSON_TURN: &str = "person_turn";
/// The Next up item a person's own command left waiting for a task's next
/// turn, by task. Its turn is theirs when that very item starts.
pub(crate) const PERSON_QUEUED: &str = "person_queued";

/// Notes run `run_id` as one the person started.
pub(crate) fn person_turn(tx: &mut store::Write<'_>, run_id: RunId) -> store::Result<()> {
    tx.put(PERSON_TURN, run_id, &true)
}

/// Whether the person started run `run_id`.
pub(crate) fn is_person_turn(r: &store::Read<'_>, run_id: RunId) -> store::Result<bool> {
    Ok(r.get::<bool>(PERSON_TURN, run_id)?.unwrap_or(false))
}

/// Notes that what waits in Next up for task `task_id` now is the person's.
pub(crate) fn person_queued(tx: &mut store::Write<'_>, task_id: TaskId) -> store::Result<()> {
    let task: Task = tx.read().require("task", task_id)?;
    let queue: Option<QueueView> = tx.read().get("queue", task.project_id)?;
    let item = queue.and_then(|q| {
        (q.items.iter())
            .find(|i| i.task_id == task_id && i.is_pending())
            .map(|i| i.item_id)
    });
    match item {
        Some(item) => tx.put(PERSON_QUEUED, task_id, &item),
        None => tx.delete(PERSON_QUEUED, task_id),
    }
}

/// Whether Next up item `item` of task `task_id`, starting now, is one the
/// person left waiting. Forgets it either way.
pub(crate) fn take_person_queued(
    tx: &mut store::Write<'_>,
    task_id: TaskId,
    item: QueueItemId,
) -> store::Result<bool> {
    let queued = tx.read().get::<QueueItemId>(PERSON_QUEUED, task_id)?;
    if queued.is_some() {
        tx.delete(PERSON_QUEUED, task_id)?;
    }
    Ok(queued == Some(item))
}

/// The one answer to whether what run `run` of chat `chat` proposes may
/// start by itself, with the limits it may start within. Yes only when its
/// project allows it, the person started this turn with their own message,
/// the turn could change things (Work, not Ask or Plan), the chat has full
/// access, and Next up isn't paused. `ophio_propose` tells the agent this,
/// and the turn's end starts nothing when it says no.
pub(crate) fn may_start_alone(
    r: &store::Read<'_>,
    run: &Run,
    chat: &Task,
) -> store::Result<Option<AutoStart>> {
    if !chat.conversation || run.task_id != chat.task_id {
        return Ok(None);
    }
    let project: Project = r.require("project", chat.project_id)?;
    let Some(limits) = project.auto_start.filter(|a| a.valid()) else {
        return Ok(None);
    };
    let paused = (r.get::<QueueView>("queue", chat.project_id)?).is_some_and(|q| q.paused);
    let alone = is_person_turn(r, run.run_id)?
        && run.mode == Some(TaskMode::Work)
        && crate::access::held(chat).is_none()
        && !paused;
    Ok(alone.then_some(limits))
}

/// Whether suggestion `w` could start by itself, so far as it alone goes:
/// within the internet, as written, and an issue only where the project's
/// GitHub can take it without the person.
fn could_start_alone(w: &NewWork, settings: &GithubSettings, github: bool) -> bool {
    !needs_the_person(w)
        && (w.issue.is_none()
            || (github
                && settings.mode != GithubMode::Off
                && !(settings.mode == GithubMode::ThroughMerge && !settings.warning_answered)))
}

/// Joins what run `run_id` proposed to what it reported, first, and forgets
/// the proposals. A suggestion the report repeats by title is kept once.
/// For each of the report's suggestions, from the first, that was proposed:
/// whether its agent was told it may start by itself.
pub(crate) fn merge(
    tx: &mut store::Write<'_>,
    run_id: RunId,
    outcome: &mut Option<RunOutcome>,
) -> store::Result<Vec<bool>> {
    let alone: Vec<bool> = tx.read().get(PROPOSED_ALONE, run_id)?.unwrap_or_default();
    tx.delete(PROPOSED_ALONE, run_id)?;
    let Some(proposed) = tx.read().get::<Vec<NewWork>>(PROPOSED, run_id)? else {
        return Ok(Vec::new());
    };
    tx.delete(PROPOSED, run_id)?;
    if proposed.is_empty() {
        return Ok(Vec::new());
    }
    let outcome = outcome.get_or_insert_with(|| RunOutcome {
        summary: None,
        changed_files: Vec::new(),
        checks: Vec::new(),
        still_active: Vec::new(),
        unknowns: Vec::new(),
        usage: None,
        report: None,
        verification: None,
    });
    let report = outcome.report.get_or_insert_with(|| RunReport {
        status: ReportStatus::Done,
        questions: Vec::new(),
        remaining: Vec::new(),
        new_work: Vec::new(),
    });
    let key = |w: &NewWork| w.title.trim().to_lowercase();
    let count = proposed.len();
    let mut work = proposed;
    for w in std::mem::take(&mut report.new_work) {
        // A later item's order refers to the report's own list, which
        // moved; a reported one keeps none.
        if !work.iter().any(|p| key(p) == key(&w)) {
            work.push(NewWork { after: None, ..w });
        }
    }
    work.truncate(MAX);
    report.new_work = work;
    Ok((0..count).map(|i| alone.get(i) == Some(&true)).collect())
}

/// The queue item the suggestion at `index` of a set waits for: the one
/// before it, `after`, titled `title`, started from the same turn. None when
/// that one has finished, or it is in another project than `project_id`,
/// where this one starts. Refused when it hasn't started, or when this one
/// would start now while it is still going.
pub(crate) fn follows(
    r: &store::Read<'_>,
    origin: &TaskOrigin,
    after: u32,
    title: &str,
    project_id: ProjectId,
    placement: &TaskPlacement,
) -> store::Result<Option<QueueItemId>> {
    let key = format!("{}/{}/{}", origin.chat, origin.run, after);
    let Some(before) = r.get::<TaskId>("task_origin", &key)? else {
        return Err(error(
            ErrorCode::Conflict,
            format!("This follows “{}”. Start that first.", title.trim()),
        )
        .into());
    };
    let task: Task = r.require("task", before)?;
    // Moved to another project in Review, it can't wait in that project's
    // Next up, so it starts only once the one before has finished.
    let queue: Option<QueueView> = if task.project_id == project_id {
        r.get("queue", project_id)?
    } else {
        None
    };
    let item = queue.as_ref().and_then(|q| {
        (q.items.iter().rev())
            .find(|i| i.task_id == before)
            .map(|i| i.item_id)
    });
    let going = matches!(
        task.state,
        TaskState::Accepted
            | TaskState::Dispatching
            | TaskState::Working
            | TaskState::AwaitingDecision
    );
    let now = matches!(placement, TaskPlacement::StartNow);
    // What follows a step that didn't succeed doesn't start after it on its
    // own; started now, it is the person's own choice.
    if !succeeded(r, &task)? {
        if now {
            return Ok(None);
        }
        return Err(error(
            ErrorCode::Conflict,
            format!(
                "“{}” didn't succeed, so this doesn't start after it. Start it on its own if you still want it.",
                task.title.trim()
            ),
        )
        .into());
    }
    match (item, going) {
        (Some(_), true) | (None, true) if now => Err(error(
            ErrorCode::Conflict,
            format!(
                "This follows “{}”, which hasn't finished. Put it in Next up to start after it.",
                task.title.trim()
            ),
        )
        .into()),
        (Some(item), _) => Ok(Some(item)),
        (None, true) => Err(error(
            ErrorCode::Conflict,
            format!(
                "This follows “{}”, which is still going outside Next up. Start this once it has finished.",
                task.title.trim()
            ),
        )
        .into()),
        (None, false) => Ok(None),
    }
}

/// Whether task `task`, the step before another, has not ended without
/// success: it failed, was stopped, or was taken out of Next up before it
/// started.
pub(crate) fn succeeded(r: &store::Read<'_>, task: &Task) -> store::Result<bool> {
    if matches!(task.state, TaskState::Failed | TaskState::Interrupted) {
        return Ok(false);
    }
    if task.state == TaskState::Archived {
        let Some(run) = task.current_run else {
            return Ok(false);
        };
        let run: Option<Run> = r.get("run", run)?;
        return Ok(
            run.is_none_or(|run| !matches!(run.state, RunState::Failed | RunState::Interrupted))
        );
    }
    Ok(true)
}

/// Why a step of a set from a chat's card was put away before it started.
const PUT_AWAY: &str = "proposal_put_away";

/// Takes out of `q` the steps of sets from a chat's card that wait on a step
/// that didn't succeed, or that the person took out of Next up: in order,
/// they could never start, and waiting they would hold up everything after
/// them and count as going. Each is put away unstarted, saying which step
/// didn't succeed, and its card lets the person start it by hand. This is
/// what taking it out of Next up does, for every such set, whether it was
/// started from its card or without asking. Returns whether any went.
pub(crate) fn let_go_after_failure(
    tx: &mut store::Write<'_>,
    q: &mut QueueView,
    now: protocol::time::Timestamp,
) -> store::Result<bool> {
    let mut changed = false;
    loop {
        let mut gone = None;
        for item in q.items.iter().filter(|i| i.is_pending()) {
            if item.depends_on.is_empty() {
                continue;
            }
            let Some(task) = tx.read().get::<Task>("task", item.task_id)? else {
                continue;
            };
            let Some(origin) = task.origin.clone() else {
                continue;
            };
            let Some(run) = tx.read().get::<Run>("run", origin.run)? else {
                continue;
            };
            let after = (run.outcome.and_then(|o| o.report))
                .and_then(|r| r.new_work.get(origin.index as usize).and_then(|w| w.after));
            let Some(after) = after else {
                continue;
            };
            let key = format!("{}/{}/{after}", origin.chat, origin.run);
            let Some(before) = tx.read().get::<TaskId>("task_origin", &key)? else {
                continue;
            };
            let before: Task = tx.read().require("task", before)?;
            if succeeded(&tx.read(), &before)? {
                continue;
            }
            // Why, from the first step that didn't succeed.
            let why = match tx.read().get::<String>(PUT_AWAY, before.task_id)? {
                Some(why) => why,
                None if before.state == TaskState::Archived && before.current_run.is_none() => {
                    format!("“{}” was taken out of Next up", before.title.trim())
                }
                None => format!("“{}” didn't succeed", before.title.trim()),
            };
            gone = Some((item.item_id, task, why));
            break;
        }
        let Some((item_id, mut task, why)) = gone else {
            break;
        };
        q.items.retain(|i| i.item_id != item_id);
        tx.delete("queue_permit", item_id)?;
        for waiting in domain::queue::withdraw(q, &[item_id]) {
            tx.delete("queue_permit", waiting)?;
        }
        task.state = TaskState::Archived;
        task.revision += 1;
        task.updated_at = now;
        task.status_line = Some(format!("Not started: {why}"));
        crate::github_run::let_go(tx, q.project_id, task.task_id)?;
        tx.put(PUT_AWAY, task.task_id, &why)?;
        tx.put("task", task.task_id, &task)?;
        tx.emit(protocol::Event::TaskUpserted { task })?;
        changed = true;
    }
    if changed {
        crate::runs::bump_queue_revision(tx, q)?;
        crate::runs::forget_finished(tx, q)?;
    }
    Ok(changed)
}

/// Why suggestion `w` can't start without the person: past the internet,
/// fixing every issue, or not as written.
fn needs_the_person(w: &NewWork) -> bool {
    let needs = w.needs.clone().unwrap_or_default();
    w.refused.is_some()
        || w.all_issues
        || !needs.read.is_empty()
        || needs.changes.is_some()
        || needs.whole_computer
        || needs.local_network
        || needs.desktop
}

/// The suggestions of a report, `count` from the first, in proposals: each
/// a run of indexes where every one after the first follows the one before.
fn proposals(work: &[NewWork], count: usize) -> Vec<Vec<usize>> {
    let mut sets: Vec<Vec<usize>> = Vec::new();
    for (i, w) in work.iter().enumerate().take(count) {
        match w.after {
            Some(j) => match sets.iter_mut().find(|s| s.last() == Some(&(j as usize))) {
                Some(set) => set.push(i),
                // Out of order: its set can't start as one.
                None => sets.push(vec![usize::MAX, i]),
            },
            None => sets.push(vec![i]),
        }
    }
    sets
}

impl CompanionHub {
    /// `ophio_propose`: keeps what run `run_id` of chat `task` proposes,
    /// for its card when the turn ends.
    pub(crate) fn propose(
        &self,
        run_id: RunId,
        task: &Task,
        tasks: &[Value],
        in_order: bool,
        all_issues: bool,
    ) -> Result<String, ProtocolError> {
        if !task.conversation {
            return Err(error(
                ErrorCode::PermissionDenied,
                "Only a chat proposes work as cards. Say what you'd start in your summary instead.",
            ));
        }
        let mut work = Vec::new();
        if all_issues {
            if !tasks.is_empty() {
                return Err(error(
                    ErrorCode::InvalidRequest,
                    "Propose fixing every issue on its own, with no tasks.",
                ));
            }
            let settings: GithubSettings = self
                .read(|r| r.get(SETTINGS, task.project_id))?
                .unwrap_or_default();
            if settings.mode == GithubMode::Off || self.inner.github.is_none() {
                return Err(error(
                    ErrorCode::InvalidRequest,
                    "GitHub is off for this project, so its issues can't be fixed from here. Ask the person to turn it on in Tasks › GitHub.",
                ));
            }
            work.push(NewWork {
                title: "Fix the open issues".into(),
                brief: "Fix every open issue Agent can take, one by one, as Run all does.".into(),
                mode: TaskMode::Work,
                issue: None,
                needs: None,
                refused: None,
                after: None,
                all_issues: true,
            });
        } else {
            if tasks.is_empty() || tasks.len() > MAX {
                return Err(error(
                    ErrorCode::InvalidRequest,
                    format!("Propose between 1 and {MAX} tasks at a time."),
                ));
            }
            for (n, v) in tasks.iter().enumerate() {
                let w = crate::reported::work(v).ok_or_else(|| {
                    error(
                        ErrorCode::InvalidRequest,
                        format!("Task {} needs a title and a brief.", n + 1),
                    )
                })?;
                work.push(w);
            }
        }
        let count = work.len();
        let refused: Vec<String> = (work.iter().enumerate())
            .filter_map(|(n, w)| (w.refused.as_ref()).map(|why| format!("Task {}: {why}", n + 1)))
            .collect();
        let github = self.inner.github.is_some();
        let alone: Vec<bool> = self.read(|r| {
            let run: Run = r.require("run", run_id)?;
            let settings: GithubSettings = r.get(SETTINGS, task.project_id)?.unwrap_or_default();
            let yes = may_start_alone(r, &run, task)?.is_some();
            Ok((work.iter())
                .map(|w| yes && could_start_alone(w, &settings, github))
                .collect())
        })?;
        let auto = alone.iter().any(|a| *a);
        self.write(|tx| {
            let mut kept: Vec<NewWork> = tx.read().get(PROPOSED, run_id)?.unwrap_or_default();
            let mut told: Vec<bool> = tx.read().get(PROPOSED_ALONE, run_id)?.unwrap_or_default();
            told.resize(kept.len(), false);
            told.extend(alone.iter().copied());
            tx.put(PROPOSED_ALONE, run_id, &told)?;
            // One turn offers to fix every issue once: the chat hears how
            // that Run all went as one report for the turn.
            if all_issues && kept.iter().any(|w| w.all_issues) {
                return Err(error(
                    ErrorCode::InvalidRequest,
                    "You already proposed fixing every issue this turn.",
                )
                .into());
            }
            if kept.len() + work.len() > MAX {
                return Err(error(
                    ErrorCode::InvalidRequest,
                    format!(
                        "One turn proposes at most {MAX} tasks; {} are already proposed.",
                        kept.len()
                    ),
                )
                .into());
            }
            let base = kept.len() as u32;
            for (k, mut w) in work.into_iter().enumerate() {
                if in_order && k > 0 {
                    w.after = Some(base + k as u32 - 1);
                }
                kept.push(w);
            }
            tx.put(PROPOSED, run_id, &kept)
        })?;
        let mut said = vec![match (all_issues, count, in_order) {
            (true, _, _) => "Proposed fixing the open issues one by one with the person's own fixer and reviewer, as Run all does.".to_owned(),
            (false, 1, _) => "Proposed 1 task.".to_owned(),
            (false, n, true) => format!("Proposed {n} tasks in order: each starts once the one before has finished."),
            (false, n, false) => format!("Proposed {n} tasks."),
        }];
        if auto {
            said.push("Nothing has started yet. The person lets this project start what its chats propose without asking, within their limits, so what needs only the internet may start by itself when your turn ends, in the order proposed, and stops at its pull request for the person; the card says what started. Anything else waits on the card for the person's tap.".into());
        } else {
            said.push("Nothing has started, and nothing will by itself: the person sees this as a card when your turn ends, and starts it with a tap.".into());
        }
        if !refused.is_empty() {
            said.push(format!(
                "These can't start as written, and the card says why: {}",
                refused.join(" ")
            ));
        }
        Ok(said.join(" "))
    }

    /// Starts what run `run_id` of a chat proposed, on its project's
    /// go-ahead, if [`may_start_alone`] still says yes as the turn ends.
    /// `told` says, for each of the report's suggestions from the first that
    /// was proposed, whether its agent was told it may start by itself; no
    /// other starts. Proposals go in the order made: one starts only whole,
    /// within what is left of both limits, and once one doesn't fit, nothing
    /// after it does. One that only needs the person's tap is passed over.
    pub(crate) async fn start_proposed(
        &self,
        run_id: RunId,
        told: Vec<bool>,
    ) -> Result<(), ProtocolError> {
        let _adding = self.inner.planning.lock().await;
        let read = self.read(|r| {
            let run: Run = r.require("run", run_id)?;
            let chat: Task = r.require("task", run.task_id)?;
            let Some(limits) = may_start_alone(r, &run, &chat)? else {
                return Ok(None);
            };
            let work = (run.outcome.and_then(|o| o.report))
                .map(|r| r.new_work)
                .unwrap_or_default();
            let settings = r
                .get::<GithubSettings>(SETTINGS, chat.project_id)?
                .unwrap_or_default();
            Ok(Some((
                chat,
                limits,
                work,
                r.list::<Task>("task")?,
                settings,
            )))
        })?;
        let Some((chat, limits, work, tasks, settings)) = read else {
            return Ok(());
        };
        let github = settings.mode != GithubMode::Off && self.inner.github.is_some();
        let through_merge_unanswered =
            settings.mode == GithubMode::ThroughMerge && !settings.warning_answered;
        let mut going = (tasks.iter())
            .filter(|t| {
                t.project_id == chat.project_id
                    && t.origin.as_ref().is_some_and(|o| o.auto)
                    && matches!(
                        t.state,
                        TaskState::Accepted
                            | TaskState::Dispatching
                            | TaskState::Working
                            | TaskState::AwaitingDecision
                    )
            })
            .count() as u32;
        let agent = match self.chosen_agent(&chat, None) {
            Ok(agent) => agent,
            // No agent can start now: the cards wait for the person.
            Err(_) => return Ok(()),
        };
        let projects = self.read(|r| r.list::<Project>("project"))?;
        let mut left = limits.per_reply;
        let mut started = false;
        'sets: for set in proposals(&work, told.len()) {
            let size = set.len() as u32;
            let startable = set.iter().all(|&i| {
                told.get(i) == Some(&true)
                    && work
                        .get(i)
                        .is_some_and(|w| could_start_alone(w, &settings, github))
            });
            if !startable {
                // It waits for the person's tap; what follows may still go.
                continue;
            }
            if size > left || going + size > limits.at_once {
                // Nothing proposed after it jumps ahead of it.
                break;
            }
            for &i in &set {
                let w = &work[i];
                let mode = crate::work_card_binding::mode_of(w);
                // Started without asking, it stops at its pull request for
                // the person, whatever the project does with others.
                let link = match (w.issue, mode) {
                    (Some(number), _) => Some(GithubLink::Issue {
                        number,
                        review_with: settings.workflow.review_with.clone(),
                        when_green: None,
                    }),
                    (None, TaskMode::Work) if github && !through_merge_unanswered => {
                        Some(GithubLink::NewPr)
                    }
                    _ => None,
                };
                let access = match mode {
                    TaskMode::Ask => {
                        match crate::work_card_binding::least_access(w, chat.project_id, &projects)
                        {
                            Ok(a) => Some(a),
                            Err(_) => break 'sets,
                        }
                    }
                    _ => None,
                };
                let envelope = CommandEnvelope::new(Command::CreateTask {
                    origin: Some(TaskOrigin {
                        chat: chat.task_id,
                        run: run_id,
                        index: i as u32,
                        auto: true,
                        reviewed: false,
                    }),
                    from_chat: None,
                    project_id: chat.project_id,
                    kind: TaskKind::Coding,
                    mode: Some(mode),
                    title: Some(w.title.clone()),
                    prompt: w.brief.clone(),
                    bundle: None,
                    harness: agent.harness,
                    model: agent.model.clone(),
                    effort: agent.effort.clone(),
                    launch_policy: agent.launch_policy,
                    branch: None,
                    start: None,
                    place: None,
                    browser: None,
                    github: link.map(Box::new),
                    mission: None,
                    target_id: None,
                    placement: TaskPlacement::Queue {
                        position: None,
                        authorization: QueueAuthorization::AutoStart,
                    },
                    agent_chosen: Some(true),
                    conversation: None,
                    access,
                    review: None,
                });
                let command_id = envelope.command_id;
                self.write(|tx| tx.put(AUTO_COMMAND, command_id, &true))?;
                let receipt = self.admit(&as_creator(&chat), envelope).await;
                if let Some(e) = receipt.error {
                    self.write(|tx| tx.delete(AUTO_COMMAND, command_id))?;
                    tracing::info!(code = ?e.code, "a proposal waits for the person");
                    // What follows it, in its set or after, waits on its
                    // card too.
                    break 'sets;
                }
                going += 1;
                left -= 1;
                started = true;
            }
        }
        if started {
            self.progress_queues();
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(after: Option<u32>) -> NewWork {
        NewWork {
            title: "t".into(),
            brief: "b".into(),
            mode: TaskMode::Work,
            issue: None,
            needs: None,
            refused: None,
            after,
            all_issues: false,
        }
    }

    /// An ordered set is one proposal; standalone suggestions are their own.
    #[test]
    fn sets_are_one_proposal_each() {
        let work = vec![w(None), w(Some(0)), w(Some(1)), w(None), w(None)];
        assert_eq!(proposals(&work, 5), vec![vec![0, 1, 2], vec![3], vec![4]]);
        // Only what was proposed counts; the report's own come after.
        assert_eq!(proposals(&work, 3), vec![vec![0, 1, 2]]);
        // A follower whose leader isn't the last of a set can't start as one.
        let odd = vec![w(None), w(Some(0)), w(Some(0))];
        assert_eq!(proposals(&odd, 3), vec![vec![0, 1], vec![usize::MAX, 2]]);
    }
}
