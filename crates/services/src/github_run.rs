// SPDX-License-Identifier: Apache-2.0
//! Run all: one tap fixes every issue in Agent can take it, one after
//! another, with the project's own choice of who fixes and who reviews.
//! Issues not sorted yet are sorted first, and what the sort finds the agent
//! can take joins the run. It ends when nothing is left, or when the person
//! stops it.
//!
//! While it is on, the person's tap is the go-ahead for each of its fixes:
//! they start by themselves however long they wait in Next up, and an edit
//! to Next up that asks again of everything else doesn't ask again of them.
//! A fix the person tells to wait leaves the run.
use crate::api::Caller;
use crate::github::{SETTINGS, TRIAGE, Triaged, item_status, project_tasks, writes_nothing};
use crate::runs::{
    QUEUE_PERMIT_MS, QueuePermit, bump_queue_revision, forget_finished, sync_queue_attention,
};
use crate::{CompanionHub, error};
use protocol::github::*;
use protocol::{
    CommandId, ErrorCode, Event, ProjectId, ProtocolError, TaskId,
    auth::Principal,
    command::{Command, CommandEnvelope, CommandResult, TaskPlacement},
    model::{Task, TaskKind, TaskMode, TaskState},
    queue::{QueueAuthorization, QueueView},
    time::Timestamp,
};

/// Each project's latest Run all.
pub(crate) const RUN: &str = "github_run";
/// What a fix Pause stopped is told when Resume carries it on.
const CARRY_ON: &str = "Carry on where you stopped.";
/// How long a Start part way through keeps the run from ending as empty.
const STARTING_MS: i64 = 10 * 60_000;
/// Why Next up waits while Run all is paused.
const PAUSED: &str = "Run all is paused. Resume it to carry on.";

/// A project's Run all as kept: what the phone sees, and whose go-ahead its
/// fixes start on.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct Running {
    pub view: IssueRun,
    pub by: Principal,
    /// The fixes Pause stopped, carried on at Resume.
    #[serde(default)]
    pub paused_tasks: Vec<TaskId>,
    /// A Start is adding fixes; until it has, the run isn't over.
    #[serde(default)]
    pub starting_since: Option<Timestamp>,
    /// What its fixes come to is told once, as a whole (`crate::calm`).
    /// Runs kept before that alerted for each.
    #[serde(default)]
    pub calm: bool,
    /// When the person was told how it went.
    #[serde(default)]
    pub told: Option<Timestamp>,
    /// The fixes of a run that ended untold before this one started: told
    /// with this one's.
    #[serde(default)]
    pub earlier: Vec<RunFix>,
    /// The Sorts it started.
    #[serde(default)]
    pub sorts: Vec<TaskId>,
}

impl Running {
    fn has(&self, task_id: TaskId) -> bool {
        self.view.fixes.iter().any(|f| f.task_id == task_id)
    }

    /// Whether `task_id` is one of the run's, to be told with it.
    pub(crate) fn holds(&self, task_id: TaskId) -> bool {
        self.has(task_id)
            || self.earlier.iter().any(|f| f.task_id == task_id)
            || self.sorts.contains(&task_id)
            || self.view.sorting == Some(task_id)
    }
}

/// The project's latest Run all, on or ended.
pub(crate) fn issue_run(
    r: &store::Read<'_>,
    project_id: ProjectId,
) -> store::Result<Option<IssueRun>> {
    Ok(r.get::<Running>(RUN, project_id)?.map(|r| r.view))
}

fn active(r: &store::Read<'_>, project_id: ProjectId) -> store::Result<Option<Running>> {
    Ok(r.get::<Running>(RUN, project_id)?
        .filter(|r| r.view.ended_at.is_none()))
}

/// Whether a run's end moves Run all on: one of its fixes, or its Sort.
/// Any end does, not only a finished one: a fix that failed or that the
/// person stopped is theirs now, and the rest carry on.
pub(crate) fn moves_run(r: &store::Read<'_>, task_id: TaskId) -> store::Result<bool> {
    let Some(task) = r.get::<Task>("task", task_id)? else {
        return Ok(false);
    };
    Ok(active(r, task.project_id)?
        .is_some_and(|run| run.has(task_id) || run.view.sorting == Some(task_id)))
}

/// Keeps the go-ahead of each fix Run all is waiting to start, as Next up
/// is refreshed: renewed before it lapses, and given again after an edit to
/// Next up asked again of everything.
pub(crate) fn keep_running(
    tx: &store::Write<'_>,
    q: &mut QueueView,
    now: Timestamp,
) -> store::Result<()> {
    let Some(run) = active(&tx.read(), q.project_id)? else {
        return Ok(());
    };
    let end = Timestamp(now.0 + QUEUE_PERMIT_MS);
    for item in (q.items.iter_mut()).filter(|i| i.is_pending() && run.has(i.task_id)) {
        let Some(task) = tx.read().get::<Task>("task", item.task_id)? else {
            continue;
        };
        let permit = tx.read().get::<QueuePermit>("queue_permit", item.item_id)?;
        let kept = item.authorization == QueueAuthorization::AutoStart
            && (item.authorization_expires_at).is_some_and(|e| e.0 - now.0 > QUEUE_PERMIT_MS / 2)
            && permit.is_some_and(|p| {
                p.remaining == 1
                    && p.task_revision == task.revision
                    && p.queue_revision == q.revision
                    && p.expires_at == item.authorization_expires_at
                    && p.principal == run.by
            });
        if kept {
            continue;
        }
        item.authorization = QueueAuthorization::AutoStart;
        item.authorization_expires_at = Some(end);
        tx.put(
            "queue_permit",
            item.item_id,
            &QueuePermit {
                task_revision: task.revision,
                queue_revision: q.revision,
                remaining: 1,
                principal: run.by,
                expires_at: Some(end),
            },
        )?;
    }
    Ok(())
}

/// Takes a fix out of Run all: the person told it to wait for them.
pub(crate) fn let_go(
    tx: &mut store::Write<'_>,
    project_id: ProjectId,
    task_id: TaskId,
) -> store::Result<()> {
    let Some(mut run) = active(&tx.read(), project_id)? else {
        return Ok(());
    };
    if !run.has(task_id) {
        return Ok(());
    }
    run.view.fixes.retain(|f| f.task_id != task_id);
    tx.put(RUN, project_id, &run)?;
    tx.emit(Event::GithubChanged { project_id })?;
    Ok(())
}

/// Ends Run all once nothing of it is waiting, working or still to be
/// sorted. A paused run waits for the person. One whose project turned
/// GitHub off ends, as nothing it sorts would be taken on.
pub(crate) fn settle(
    tx: &mut store::Write<'_>,
    project_id: ProjectId,
    held: bool,
    now: Timestamp,
) -> store::Result<()> {
    let Some(mut run) = active(&tx.read(), project_id)? else {
        return Ok(());
    };
    if run.view.paused
        || held
        || (run.starting_since).is_some_and(|since| now.0 - since.0 < STARTING_MS)
    {
        return Ok(());
    }
    let settings: GithubSettings = tx.read().get(SETTINGS, project_id)?.unwrap_or_default();
    let task = |id: TaskId| tx.read().get::<Task>("task", id);
    let mut going = false;
    for fix in &run.view.fixes {
        going |= task(fix.task_id)?.is_some_and(|t| {
            matches!(
                t.state,
                TaskState::Accepted | TaskState::Dispatching | TaskState::Working
            )
        });
    }
    if let Some(sorting) = run.view.sorting {
        // A finished Sort is still going until what it found is taken on.
        going |= settings.mode != GithubMode::Off
            && task(sorting)?.is_some_and(|t| {
                matches!(
                    t.state,
                    TaskState::Accepted
                        | TaskState::Dispatching
                        | TaskState::Working
                        | TaskState::Completed
                )
            });
    }
    if going && settings.mode != GithubMode::Off {
        return Ok(());
    }
    run.view.ended_at = Some(now);
    run.view.sorting = None;
    run.starting_since = None;
    tx.put(RUN, project_id, &run)?;
    tx.emit(Event::GithubChanged { project_id })?;
    Ok(())
}

/// A queued fix that hasn't started taken out of Next up and set aside.
fn take_out(
    tx: &mut store::Write<'_>,
    q: &mut QueueView,
    task_id: TaskId,
    why: &str,
    now: Timestamp,
) -> store::Result<bool> {
    let Some(index) = (q.items.iter()).position(|i| i.task_id == task_id && i.is_pending()) else {
        return Ok(false);
    };
    let Some(mut task) = tx.read().get::<Task>("task", task_id)? else {
        return Ok(false);
    };
    if task.state != TaskState::Accepted || task.current_run.is_some() {
        return Ok(false);
    }
    let item = q.items.remove(index);
    tx.delete("queue_permit", item.item_id)?;
    task.state = TaskState::Archived;
    task.revision += 1;
    task.updated_at = now;
    task.status_line = Some(why.into());
    tx.put("task", task_id, &task)?;
    tx.emit(Event::TaskUpserted { task })?;
    Ok(true)
}

/// Takes fix `task_id` out of Next up and Run all, if it waits there
/// unstarted, archived with `why`. Whether it did.
pub(crate) fn take_out_fix(
    hub: &CompanionHub,
    tx: &mut store::Write<'_>,
    project_id: ProjectId,
    task_id: TaskId,
    why: &str,
    now: Timestamp,
) -> store::Result<bool> {
    let Some(mut q) = tx.read().get::<QueueView>("queue", project_id)? else {
        return Ok(false);
    };
    if !take_out(tx, &mut q, task_id, why, now)? {
        return Ok(false);
    }
    let_go(tx, project_id, task_id)?;
    put_queue(hub, tx, q, now)?;
    Ok(true)
}

/// Puts Next up back after fixes were taken out, keeping what everything
/// else was let start by itself.
fn put_queue(
    hub: &CompanionHub,
    tx: &mut store::Write<'_>,
    mut q: QueueView,
    now: Timestamp,
) -> store::Result<()> {
    bump_queue_revision(tx, &mut q)?;
    for (i, item) in q.items.iter_mut().enumerate() {
        item.position = i as u32;
    }
    forget_finished(tx, &mut q)?;
    hub.refresh_queue(tx, &mut q)?;
    tx.put("queue", q.project_id, &q)?;
    sync_queue_attention(tx, &q, now)?;
    tx.emit(Event::QueueChanged { queue: q })?;
    Ok(())
}

/// Pauses Next up with `why`, or lets it go on with None: Run all's own
/// pause, which the person's stop of a single task also gives.
fn hold_queue(
    hub: &CompanionHub,
    tx: &mut store::Write<'_>,
    project_id: ProjectId,
    why: Option<&str>,
    now: Timestamp,
) -> store::Result<()> {
    let Some(mut q) = tx.read().get::<QueueView>("queue", project_id)? else {
        return Ok(());
    };
    if q.paused == why.is_some() && q.pause_reason.as_deref() == why {
        return Ok(());
    }
    q.paused = why.is_some();
    q.pause_reason = why.map(Into::into);
    bump_queue_revision(tx, &mut q)?;
    hub.refresh_queue(tx, &mut q)?;
    tx.put("queue", project_id, &q)?;
    sync_queue_attention(tx, &q, now)?;
    tx.emit(Event::QueueChanged { queue: q })?;
    Ok(())
}

/// The same command for the same run and issue, so a second tap on Start
/// while the first is still adding fixes queues each once.
fn fix_command(project_id: ProjectId, started_at: Timestamp, number: u64) -> CommandId {
    use sha2::{Digest, Sha256};
    let mut hash = Sha256::new();
    hash.update(b"ophio run all\0");
    hash.update(project_id.as_uuid().as_bytes());
    hash.update(started_at.0.to_be_bytes());
    hash.update(number.to_be_bytes());
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&hash.finalize()[..16]);
    CommandId(uuid::Builder::from_custom_bytes(bytes).into_uuid())
}

fn create(
    project_id: ProjectId,
    with: &LaunchChoice,
    prompt: String,
    mode: Option<TaskMode>,
    link: GithubLink,
    placement: TaskPlacement,
) -> Command {
    Command::CreateTask {
        project_id,
        kind: TaskKind::Coding,
        mode,
        title: None,
        prompt,
        bundle: None,
        harness: with.harness,
        model: with.model.clone(),
        effort: with.effort.clone(),
        launch_policy: with.launch_policy,
        branch: None,
        start: None,
        place: None,
        browser: None,
        github: Some(Box::new(link)),
        mission: None,
        target_id: None,
        placement,
        agent_chosen: Some(true),
        conversation: None,
        access: None,
        review: None,
    }
}

impl CompanionHub {
    pub(crate) async fn run_issues(
        &self,
        caller: &Caller,
        project_id: ProjectId,
        step: RunStep,
    ) -> Result<(), ProtocolError> {
        match step {
            RunStep::Start => self.start_run_all(caller, project_id).await,
            RunStep::Pause => self.pause_run_all(caller, project_id).await,
            RunStep::Resume => self.resume_run_all(caller, project_id).await,
            RunStep::Stop => self.stop_run_all(caller, project_id).await,
        }
    }

    async fn start_run_all(
        &self,
        caller: &Caller,
        project_id: ProjectId,
    ) -> Result<(), ProtocolError> {
        let settings = self.github_settings(project_id)?;
        if settings.mode == GithubMode::Off {
            return Err(error(
                ErrorCode::InvalidRequest,
                "Turn GitHub on for this project first",
            ));
        }
        let Some(fix_with) = settings.workflow.fix_with.clone() else {
            return Err(error(
                ErrorCode::InvalidRequest,
                "Choose who fixes the issues first",
            ));
        };
        let listed = self
            .github_items(project_id, GithubItemKind::Issues, None)
            .await?;
        let now = self.inner.clock.now();
        // One run a project: a second Start carries on the first.
        self.write(|tx| {
            let mut run = match active(&tx.read(), project_id)? {
                Some(run) => run,
                None => {
                    // One that ended with fixes still going is told with this.
                    let (earlier, sorts) = match tx.read().get::<Running>(RUN, project_id)? {
                        Some(old) if old.calm && old.told.is_none() => {
                            let mut earlier = old.earlier;
                            earlier.extend(old.view.fixes);
                            (earlier, old.sorts)
                        }
                        _ => (Vec::new(), Vec::new()),
                    };
                    Running {
                        view: IssueRun {
                            started_at: now,
                            fixes: Vec::new(),
                            sorting: None,
                            paused: false,
                            ended_at: None,
                            stopped: false,
                        },
                        by: caller.principal,
                        paused_tasks: Vec::new(),
                        starting_since: None,
                        calm: true,
                        told: None,
                        earlier,
                        sorts,
                    }
                }
            };
            run.by = caller.principal;
            run.starting_since = Some(now);
            tx.put(RUN, project_id, &run)
        })?;
        let outcome = self
            .add_to_run(
                caller,
                project_id,
                &settings,
                &fix_with,
                &listed.items,
                true,
            )
            .await;
        let (added, sorting) = match outcome {
            Ok(added) => added,
            Err(e) => {
                self.write(|tx| {
                    if let Some(mut run) = active(&tx.read(), project_id)? {
                        run.starting_since = None;
                        tx.put(RUN, project_id, &run)?;
                    }
                    Ok(())
                })?;
                self.progress_queues();
                return Err(e);
            }
        };
        let empty = self.write(|tx| {
            let Some(mut run) = active(&tx.read(), project_id)? else {
                return Ok(false);
            };
            for fix in added {
                if !run.has(fix.task_id) {
                    run.view.fixes.push(fix);
                }
            }
            if let Some(sort) = sorting {
                run.view.sorting = Some(sort);
                if !run.sorts.contains(&sort) {
                    run.sorts.push(sort);
                }
            }
            run.view.paused = false;
            run.paused_tasks.clear();
            run.starting_since = None;
            let empty = run.view.fixes.is_empty() && run.view.sorting.is_none();
            if empty {
                run.view.ended_at = Some(now);
            }
            tx.put(RUN, project_id, &run)?;
            // Run all is the go-ahead for Next up as well: its fixes would
            // otherwise wait there unseen.
            hold_queue(self, tx, project_id, None, now)?;
            tx.emit(Event::GithubChanged { project_id })?;
            Ok(empty)
        })?;
        if empty {
            return Err(error(
                ErrorCode::InvalidRequest,
                "Nothing to fix: no open issue is in Agent can take it, and none is waiting to be sorted",
            ));
        }
        self.progress_queues();
        Ok(())
    }

    /// Queues a fix for each issue in Agent can take it that has none, takes
    /// on each fix already waiting in Next up, and, if `sort`, starts a Sort
    /// when issues wait to be sorted. Says what it added, and the Sort the
    /// run waits on.
    async fn add_to_run(
        &self,
        caller: &Caller,
        project_id: ProjectId,
        settings: &GithubSettings,
        fix_with: &LaunchChoice,
        listed: &[GithubItem],
        sort: bool,
    ) -> Result<(Vec<RunFix>, Option<TaskId>), ProtocolError> {
        let (tasks, queue, run) = self.read(|r| {
            Ok((
                project_tasks(r, project_id)?,
                r.get::<QueueView>("queue", project_id)?,
                active(r, project_id)?,
            ))
        })?;
        let Some(run) = run else {
            return Ok((Vec::new(), None));
        };
        let task = |id: Option<TaskId>| id.and_then(|id| tasks.iter().find(|t| t.task_id == id));
        let waiting = |id: TaskId| {
            (queue.iter())
                .flat_map(|q| q.items.iter())
                .any(|i| i.task_id == id && i.is_pending())
        };
        let mut added = Vec::new();
        let mut failed = None;
        for item in listed.iter().filter(|i| !i.pr) {
            let fix = task(item.task_id).filter(|t| !writes_nothing(t));
            match fix {
                // Already waiting in Next up: the run starts it in its turn.
                Some(t) if t.state == TaskState::Accepted && waiting(t.task_id) => {
                    added.push(RunFix {
                        number: item.number,
                        task_id: t.task_id,
                    });
                }
                Some(_) => {}
                None if item.group == Some(TriageGroup::AgentCanTake) => {
                    let link = GithubLink::Issue {
                        number: item.number,
                        review_with: settings.workflow.review_with.clone(),
                    };
                    let mut envelope = CommandEnvelope::new(create(
                        project_id,
                        fix_with,
                        format!("Fix issue #{}.", item.number),
                        None,
                        link,
                        TaskPlacement::Queue {
                            position: None,
                            authorization: QueueAuthorization::AutoStart,
                        },
                    ));
                    envelope.command_id = fix_command(project_id, run.view.started_at, item.number);
                    let receipt = self.admit_inside(caller, envelope).await;
                    match (receipt.error, receipt.result) {
                        (None, Some(CommandResult::TaskCreated { task_id, .. })) => {
                            added.push(RunFix {
                                number: item.number,
                                task_id,
                            });
                        }
                        (Some(e), _) => failed = Some(e),
                        _ => failed = Some(error(ErrorCode::Internal, "A fix wasn't queued")),
                    }
                }
                None => {}
            }
        }
        let unsorted = listed
            .iter()
            .any(|i| !i.pr && i.group.is_none() && i.task_id.is_none());
        let sorting_now = (tasks.iter()).find(|t| {
            (t.github.as_deref()).is_some_and(|g| g.link == GithubLink::Triage)
                && matches!(
                    t.state,
                    TaskState::Accepted | TaskState::Dispatching | TaskState::Working
                )
        });
        let sorting = match sorting_now {
            // A Sort already under way: what it finds joins the run.
            Some(t) => Some(t.task_id),
            None if unsorted && sort => {
                let sort_with = (settings.workflow.review_with.clone())
                    .map(|r| LaunchChoice {
                        launch_policy: None,
                        ..r
                    })
                    .unwrap_or_else(|| LaunchChoice {
                        launch_policy: None,
                        ..fix_with.clone()
                    });
                let envelope = CommandEnvelope::new(create(
                    project_id,
                    &sort_with,
                    "Sort the open issues and pull requests".into(),
                    Some(TaskMode::Plan),
                    GithubLink::Triage,
                    TaskPlacement::StartNow,
                ));
                let receipt = self.admit_inside(caller, envelope).await;
                match (receipt.error, receipt.result) {
                    (None, Some(CommandResult::TaskCreated { task_id, .. })) => Some(task_id),
                    (Some(e), _) => {
                        failed = Some(e);
                        None
                    }
                    _ => None,
                }
            }
            None => None,
        };
        // What was added stays added; a failure is said once nothing else
        // came of the tap.
        match failed {
            Some(e) if added.is_empty() && sorting.is_none() => Err(e),
            _ => Ok((added, sorting)),
        }
    }

    /// Takes on what a Sort found the agent can take while Run all is on,
    /// once its end has been acted on, however that went: the one Run all
    /// started, or one the person did.
    pub(crate) async fn sorted(&self, task_id: TaskId) {
        let Ok(Some(task)) = self.read(|r| r.get::<Task>("task", task_id)) else {
            return;
        };
        if (task.github.as_deref()).is_none_or(|g| g.link != GithubLink::Triage) {
            return;
        }
        let project_id = task.project_id;
        let Ok(Some(run)) = self.read(|r| active(r, project_id)) else {
            return;
        };
        let settings = self.github_settings(project_id).ok();
        let fix_with = (settings.as_ref()).and_then(|s| s.workflow.fix_with.clone());
        let mut added = Vec::new();
        if let (Some(settings), Some(fix_with)) = (settings.as_ref(), fix_with.as_ref())
            && settings.mode != GithubMode::Off
            && !run.view.paused
        {
            let listed = self
                .github_items(project_id, GithubItemKind::Issues, None)
                .await;
            match listed {
                Ok(listed) => {
                    let caller = Caller {
                        principal: run.by,
                        grants: protocol::auth::Grants::owner(),
                    };
                    let allowed = self
                        .read(
                            |r| Ok(Self::current_caller(r, &caller).is_ok_and(|c| c.grants.tasks)),
                        )
                        .unwrap_or(false);
                    if allowed {
                        match self
                            .add_to_run(
                                &caller,
                                project_id,
                                settings,
                                fix_with,
                                &listed.items,
                                false,
                            )
                            .await
                        {
                            Ok((fixes, _)) => added = fixes,
                            Err(e) => {
                                tracing::error!(code=?e.code, "Run all didn't queue what the sort found")
                            }
                        }
                    }
                }
                Err(e) => tracing::error!(code=?e.code, "Run all couldn't list the sorted issues"),
            }
        }
        let wrote = self.write(|tx| {
            let Some(mut run) = active(&tx.read(), project_id)? else {
                return Ok(());
            };
            for fix in added {
                if !run.has(fix.task_id) {
                    run.view.fixes.push(fix);
                }
            }
            if run.view.sorting == Some(task_id) {
                run.view.sorting = None;
            }
            tx.put(RUN, project_id, &run)?;
            let mut held = false;
            if let Some(mut q) = tx.read().get::<QueueView>("queue", project_id)? {
                held = q.paused;
                self.refresh_queue(tx, &mut q)?;
                tx.put("queue", project_id, &q)?;
                tx.emit(Event::QueueChanged { queue: q })?;
            }
            tx.emit(Event::GithubChanged { project_id })?;
            // A Sort that found nothing to take on ends the run here: with
            // no fix queued, Next up has nothing to settle it on.
            settle(tx, project_id, held, self.inner.clock.now())
        });
        if let Err(e) = wrote {
            tracing::error!(code=?e.code, "Run all didn't take on the sort");
        }
        self.progress_queues();
    }

    /// Holds Next up, then stops the fixes under way, so the one stopping
    /// isn't followed by the next.
    async fn pause_run_all(
        &self,
        caller: &Caller,
        project_id: ProjectId,
    ) -> Result<(), ProtocolError> {
        let now = self.inner.clock.now();
        let working = self.write(|tx| {
            let Some(mut run) = active(&tx.read(), project_id)? else {
                return Ok(Vec::new());
            };
            hold_queue(self, tx, project_id, Some(PAUSED), now)?;
            let mut working = Vec::new();
            for fix in &run.view.fixes {
                if let Some(t) = tx.read().get::<Task>("task", fix.task_id)?
                    && matches!(t.state, TaskState::Dispatching | TaskState::Working)
                {
                    working.push(t.task_id);
                }
            }
            run.view.paused = true;
            run.paused_tasks = working.clone();
            tx.put(RUN, project_id, &run)?;
            tx.emit(Event::GithubChanged { project_id })?;
            Ok(working)
        })?;
        self.stop_each(caller, &working).await
    }

    /// Carries on the fixes Pause stopped, then Next up.
    async fn resume_run_all(
        &self,
        caller: &Caller,
        project_id: ProjectId,
    ) -> Result<(), ProtocolError> {
        let stopped = self.read(|r| {
            let Some(run) = active(r, project_id)? else {
                return Ok(Vec::new());
            };
            let mut stopped = Vec::new();
            for &id in &run.paused_tasks {
                if let Some(t) = r.get::<Task>("task", id)?
                    && t.state == TaskState::Interrupted
                {
                    stopped.push((id, t.revision));
                }
            }
            Ok(stopped)
        })?;
        // The fixes it stopped go on first, so the next in Next up doesn't
        // start ahead of them.
        let mut failed = None;
        for (task_id, revision) in stopped {
            let mut envelope = CommandEnvelope::new(Command::ResumeTask {
                task_id,
                mode: None,
                text: Some(CARRY_ON.into()),
                bundle: None,
                carried_plan: None,
                read_only: false,
                model: None,
                effort: None,
            });
            envelope.expected_revision = Some(revision);
            if let Some(e) = self.admit_inside(caller, envelope).await.error {
                failed.get_or_insert(e);
            }
        }
        let now = self.inner.clock.now();
        self.write(|tx| {
            let Some(mut run) = active(&tx.read(), project_id)? else {
                return Ok(());
            };
            run.view.paused = false;
            run.paused_tasks.clear();
            tx.put(RUN, project_id, &run)?;
            hold_queue(self, tx, project_id, None, now)?;
            tx.emit(Event::GithubChanged { project_id }).map(drop)
        })?;
        self.progress_queues();
        failed.map_or(Ok(()), Err)
    }

    /// Ends the run first, so no fix follows the one stopping; takes the
    /// fixes that haven't started out of Next up; then stops those under
    /// way. Next up itself is left as it was.
    async fn stop_run_all(
        &self,
        caller: &Caller,
        project_id: ProjectId,
    ) -> Result<(), ProtocolError> {
        let now = self.inner.clock.now();
        let working = self.write(|tx| {
            let Some(mut run) = active(&tx.read(), project_id)? else {
                return Ok(Vec::new());
            };
            run.view.ended_at = Some(now);
            run.view.stopped = true;
            run.view.paused = false;
            run.view.sorting = None;
            run.starting_since = None;
            let mut working = Vec::new();
            if let Some(mut q) = tx.read().get::<QueueView>("queue", project_id)? {
                for fix in &run.view.fixes {
                    take_out(
                        tx,
                        &mut q,
                        fix.task_id,
                        "Taken out of Next up when Run all stopped",
                        now,
                    )?;
                }
                put_queue(self, tx, q, now)?;
            }
            for fix in &run.view.fixes {
                if let Some(t) = tx.read().get::<Task>("task", fix.task_id)?
                    && matches!(t.state, TaskState::Dispatching | TaskState::Working)
                {
                    working.push(t.task_id);
                }
            }
            run.paused_tasks.clear();
            tx.put(RUN, project_id, &run)?;
            tx.emit(Event::GithubChanged { project_id })?;
            Ok(working)
        })?;
        self.stop_each(caller, &working).await
    }

    /// A command admitted while one is being carried out. Boxed, as the
    /// one being carried out may be this one's own kind.
    fn admit_inside<'a>(
        &'a self,
        caller: &'a Caller,
        envelope: CommandEnvelope,
    ) -> std::pin::Pin<Box<dyn Future<Output = protocol::CommandReceipt> + Send + 'a>> {
        Box::pin(self.admit(caller, envelope))
    }

    async fn stop_each(&self, caller: &Caller, tasks: &[TaskId]) -> Result<(), ProtocolError> {
        let mut failed = None;
        for &task_id in tasks {
            let mut envelope = CommandEnvelope::new(Command::StopTask {
                task_id,
                force: false,
            });
            envelope.expected_revision = None;
            let receipt = self.admit_inside(caller, envelope).await;
            if let Some(e) = receipt.error
                && e.code != ErrorCode::AlreadyResolved
            {
                failed.get_or_insert(e);
            }
        }
        failed.map_or(Ok(()), Err)
    }

    /// The person moves an item to a group, or back to Not sorted. A later
    /// Sort keeps where they put it. Moved to Needs you, an issue's fix that
    /// hasn't started leaves Next up and Run all: it waits for them.
    pub(crate) async fn set_group(
        &self,
        project_id: ProjectId,
        number: u64,
        pr: bool,
        group: Option<TriageGroup>,
    ) -> Result<(), ProtocolError> {
        let (owner, name) = self.on_github(project_id).await?;
        let here = crate::github::repo_key(&owner, &name);
        let now = self.inner.clock.now();
        self.write(|tx| {
            let mut triage: Vec<Triaged> = tx.read().get(TRIAGE, project_id)?.unwrap_or_default();
            triage.retain(|t| !(t.pr == pr && t.number == number));
            if let Some(group) = group {
                let note = match group {
                    TriageGroup::NeedsYou => "You asked it to wait for you.",
                    TriageGroup::AgentCanTake => "You moved it here.",
                };
                triage.push(Triaged {
                    pr,
                    number,
                    group,
                    note: note.into(),
                    pick: None,
                    by_you: true,
                });
            }
            tx.put(TRIAGE, project_id, &triage)?;
            if group == Some(TriageGroup::NeedsYou) && !pr {
                let tasks = project_tasks(&tx.read(), project_id)?;
                let queued = crate::github::linked_task(&tasks, &here, number, false)
                    .filter(|t| item_status(t) == Some(ItemStatus::Queued))
                    .map(|t| t.task_id);
                if let Some(task_id) = queued
                    && let Some(mut q) = tx.read().get::<QueueView>("queue", project_id)?
                {
                    let_go(tx, project_id, task_id)?;
                    if take_out(
                        tx,
                        &mut q,
                        task_id,
                        "Taken out of Next up: it waits for you",
                        now,
                    )? {
                        put_queue(self, tx, q, now)?;
                    }
                }
            }
            tx.emit(Event::GithubChanged { project_id })?;
            Ok(())
        })
    }
}
