// SPDX-License-Identifier: Apache-2.0
//! Run all, told once. It fixes a project's issues one after another, and
//! each fix finishing, its pull request ready or merged, would alert on its
//! own: through a long run the phone would ping all along. Those outcomes
//! are raised quietly instead, listed and counted as any other, and once
//! every fix is through (merged, waiting for the person, failed or stopped)
//! one alert says how the run went. What needs the person on the way still
//! alerts at once: a question, an approval, a failure, a pull request held
//! for them, a run that finished needing a look.
use crate::CompanionHub;
use crate::github::SETTINGS;
use crate::github_run::{RUN, Running};
use crate::github_tools::listed;
use crate::github_watch::{HANDOVER, Handover, headline_item, put_item};
use protocol::{
    ProjectId, ProtocolError,
    github::{GithubMode, GithubSettings, PrStage, PrState, RunFix},
    model::{AttentionItem, AttentionKind, Task, TaskState},
    notifications::AlertTopic,
    time::Timestamp,
};

/// How long the summary waits, once Run all has ended, for pull requests
/// still going before it says how far they got. What comes of them after
/// alerts as it would without Run all.
pub(crate) const WAIT_MS: i64 = 12 * 60 * 60_000;

/// Whether `item` is held for the summary of its task's Run all: what one
/// of its fixes or Sorts came to, raised before the person was told.
pub(crate) fn held(r: &store::Read<'_>, item: &AttentionItem) -> store::Result<bool> {
    let outcome = item.kind == AttentionKind::Completed
        && matches!(
            item.topic(),
            Some(
                AlertTopic::Finished
                    | AlertTopic::PrReady
                    | AlertTopic::Merged
                    | AlertTopic::Issues
            )
        );
    let Some(task_id) = item.task_id.filter(|_| outcome) else {
        return Ok(false);
    };
    match r.get::<Task>("task", task_id)? {
        Some(task) => holds(r, &task),
        None => Ok(false),
    }
}

/// Whether `task` is one of a Run all that is still to tell the person how
/// it went.
pub(crate) fn holds(r: &store::Read<'_>, task: &Task) -> store::Result<bool> {
    Ok(r.get::<Running>(RUN, task.project_id)?
        .is_some_and(|run| run.calm && run.told.is_none() && run.holds(task.task_id)))
}

/// What a fix came to, as the summary counts it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Came {
    NeedsYou,
    Failed,
    Ready,
    Merged,
    Finished,
    Closed,
    Stopped,
    Going,
}

impl Came {
    fn of(task: Option<&Task>, handing: bool, mode: GithubMode) -> Self {
        let Some(task) = task else {
            return Self::Stopped;
        };
        let github = task.github.as_deref();
        let pr = github.and_then(|g| g.pr.as_ref());
        let stage = github.and_then(|g| g.stage);
        if stage == Some(PrStage::Merged) || pr.is_some_and(|p| p.state == PrState::Merged) {
            return Self::Merged;
        }
        match task.state {
            TaskState::Accepted | TaskState::Dispatching | TaskState::Working => {
                return Self::Going;
            }
            _ if handing => return Self::Going,
            TaskState::AwaitingDecision | TaskState::ReconciliationRequired => {
                return Self::NeedsYou;
            }
            TaskState::Failed => return Self::Failed,
            TaskState::Interrupted | TaskState::Archived => return Self::Stopped,
            TaskState::Completed => {}
        }
        match (stage, pr.map(|p| p.state)) {
            (Some(PrStage::Capped | PrStage::NotReproduced), _) => Self::NeedsYou,
            (_, Some(PrState::Closed)) | (Some(PrStage::Closed), _) => Self::Closed,
            (
                Some(PrStage::Babysitting | PrStage::Checking | PrStage::Merging),
                Some(PrState::Open),
            ) if mode != GithubMode::Off => Self::Going,
            (Some(PrStage::Ready), _) => Self::Ready,
            _ => Self::Finished,
        }
    }

    fn label(self, n: usize) -> String {
        match self {
            Self::NeedsYou if n == 1 => "1 needs you".into(),
            Self::NeedsYou => format!("{n} need you"),
            Self::Failed => format!("{n} failed"),
            Self::Ready => format!("{n} ready to merge"),
            Self::Merged => format!("{n} merged"),
            Self::Finished => format!("{n} finished"),
            Self::Closed => format!("{n} closed"),
            Self::Stopped => format!("{n} stopped"),
            Self::Going => format!("{n} still going"),
        }
    }

    fn line(self) -> &'static str {
        match self {
            Self::NeedsYou => "Needs you",
            Self::Failed => "Failed",
            Self::Ready => "Ready to merge",
            Self::Merged => "Merged",
            Self::Finished => "Finished without a pull request to merge",
            Self::Closed => "Closed",
            Self::Stopped => "Stopped",
            Self::Going => "Still going",
        }
    }
}

/// What the person is told of `project_id`'s Run all once it has come
/// through: Some(None) when there is nothing to tell. None while some of it
/// is still going, until `WAIT_MS` after it ended.
fn summary(
    r: &store::Read<'_>,
    project_id: ProjectId,
    run: &Running,
    now: Timestamp,
) -> store::Result<Option<Option<AttentionItem>>> {
    let Some(ended) = run.view.ended_at else {
        return Ok(None);
    };
    let settings: GithubSettings = r.get(SETTINGS, project_id)?.unwrap_or_default();
    let mut fixes: Vec<&RunFix> = Vec::new();
    for fix in run.earlier.iter().chain(&run.view.fixes) {
        if !fixes.iter().any(|f| f.task_id == fix.task_id) {
            fixes.push(fix);
        }
    }
    let mut came: Vec<(Came, &RunFix)> = Vec::new();
    for fix in fixes {
        let task = r.get::<Task>("task", fix.task_id)?;
        let handing = r.get::<Handover>(HANDOVER, fix.task_id)?.is_some();
        came.push((Came::of(task.as_ref(), handing, settings.mode), fix));
    }
    let going = came.iter().any(|(c, _)| *c == Came::Going);
    if going && now.0 - ended.0 < WAIT_MS {
        return Ok(None);
    }
    came.sort_by_key(|(c, _)| *c);
    let mut parts = Vec::new();
    let mut lines = Vec::new();
    for group in came.chunk_by(|a, b| a.0 == b.0) {
        let kind = group[0].0;
        parts.push(kind.label(group.len()));
        let numbers: Vec<String> = group
            .iter()
            .map(|(_, f)| format!("#{}", f.number))
            .collect();
        lines.push(format!("{}: {}.", kind.line(), listed(&numbers)));
    }
    // The fix it opens: one that needs the person, else the last.
    let on = (came.iter())
        .find(|(c, _)| matches!(c, Came::NeedsYou | Came::Failed))
        .map(|(_, f)| f.task_id)
        .or_else(|| {
            run.view
                .fixes
                .last()
                .or(run.earlier.last())
                .map(|f| f.task_id)
        })
        .or_else(|| run.sorts.last().copied());
    let Some(task_id) = on else {
        return Ok(Some(None));
    };
    // Stopped by the person with nothing come of it: nothing to tell.
    if run.view.stopped && came.iter().all(|(c, _)| *c == Came::Stopped) {
        return Ok(Some(None));
    }
    let headline = match (parts.is_empty(), run.view.stopped, going) {
        (true, _, _) => "Run all done: nothing to fix".to_owned(),
        (false, true, _) => format!("Run all stopped: {}", parts.join(", ")),
        (false, false, true) => format!("Run all so far: {}", parts.join(", ")),
        (false, false, false) => format!("Run all done: {}", parts.join(", ")),
    };
    let has = |c: Came| came.iter().any(|(k, _)| *k == c);
    let topic = match () {
        _ if has(Came::Ready) => AlertTopic::PrReady,
        _ if has(Came::Merged) => AlertTopic::Merged,
        _ => AlertTopic::Finished,
    };
    let body = (!lines.is_empty()).then(|| lines.join("\n"));
    Ok(Some(Some(headline_item(
        task_id,
        AttentionKind::Completed,
        topic,
        &headline,
        body,
        now,
    ))))
}

/// Tells the person how `project_id`'s Run all went, once, when it can.
pub(crate) fn tell(
    tx: &mut store::Write<'_>,
    project_id: ProjectId,
    now: Timestamp,
) -> store::Result<()> {
    let Some(mut run) = tx.read().get::<Running>(RUN, project_id)? else {
        return Ok(());
    };
    if !run.calm || run.told.is_some() {
        return Ok(());
    }
    let Some(item) = summary(&tx.read(), project_id, &run, now)? else {
        return Ok(());
    };
    run.told = Some(now);
    tx.put(RUN, project_id, &run)?;
    match item {
        Some(item) => put_item(tx, item),
        None => Ok(()),
    }
}

impl CompanionHub {
    /// Tells the person how `project_id`'s Run all went, once it has ended
    /// and every fix is through.
    pub(crate) fn tell_run_all(&self, project_id: ProjectId) -> Result<(), ProtocolError> {
        let now = self.inner.clock.now();
        self.write(|tx| tell(tx, project_id, now))
    }
}
