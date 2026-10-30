// SPDX-License-Identifier: Apache-2.0
//! What waits on the person stops waiting once the task has moved on,
//! however it moved: archived, started again, put in Next up, its pull
//! request merged or closed, the question behind it answered, cancelled or
//! expired, or the Next up it waited on resumed. Settled in the same write
//! as the move, for every task the write touched, so no path that moves a
//! task can leave it waiting too.
use protocol::{
    AttentionId, ProjectId, TaskId,
    event::Event,
    github::PrStage,
    model::{Approval, ApprovalState, AttentionItem, AttentionKind, Run, Task, TaskState},
    notifications::AlertTopic,
    queue::QueueView,
    time::Timestamp,
};
use serde::{Deserialize, Serialize};

/// What waits on the person only until their project's Next up is resumed.
const UNTIL_RESUMED: &str = "waits_until_resumed";

#[derive(Serialize, Deserialize)]
struct UntilResumed {
    attention_id: AttentionId,
    project_id: ProjectId,
}

/// Has `attention_id` wait only until `project_id`'s Next up is resumed,
/// which needn't move its own task.
pub(crate) fn until_resumed(
    tx: &mut store::Write<'_>,
    attention_id: AttentionId,
    project_id: ProjectId,
    now: Timestamp,
) -> store::Result<()> {
    let wait = UntilResumed {
        attention_id,
        project_id,
    };
    tx.put(UNTIL_RESUMED, attention_id, &wait)?;
    settle_resumed(tx, |p| p == project_id, now)
}

/// Settles each task this write has touched so far.
pub(crate) fn settle(tx: &mut store::Write<'_>, now: Timestamp) -> store::Result<()> {
    let mut touched = Vec::new();
    let mut resumed = Vec::new();
    for envelope in tx.emitted() {
        match &envelope.event {
            Event::TaskUpserted { task } => touched.push(task.task_id),
            Event::RunUpserted { run } => touched.push(run.task_id),
            Event::ApprovalUpserted { approval } => touched.push(approval.task_id),
            Event::AttentionUpserted { item } => touched.extend(item.task_id),
            Event::QueueChanged { queue } => {
                touched.extend(queue.items.iter().map(|item| item.task_id));
                if !queue.paused {
                    resumed.push(queue.project_id);
                }
            }
            _ => {}
        }
    }
    if !resumed.is_empty() {
        settle_resumed(tx, |p| resumed.contains(&p), now)?;
    }
    settle_tasks(tx, touched, now)
}

/// Settles every task with something open, as when Ophio starts: versions
/// before this left items open after their task moved on.
pub(crate) fn settle_all(tx: &mut store::Write<'_>, now: Timestamp) -> store::Result<()> {
    let open = (tx.read().list::<AttentionItem>("attention")?.into_iter())
        .filter(|item| item.resolved_at.is_none() && blocks(item.kind))
        .filter_map(|item| item.task_id)
        .collect();
    settle_resumed(tx, |_| true, now)?;
    settle_tasks(tx, open, now)
}

/// Settles what waited for Next up in the projects `of`, where it isn't
/// paused now.
fn settle_resumed(
    tx: &mut store::Write<'_>,
    of: impl Fn(ProjectId) -> bool,
    now: Timestamp,
) -> store::Result<()> {
    for wait in tx.read().list::<UntilResumed>(UNTIL_RESUMED)? {
        if !of(wait.project_id) {
            continue;
        }
        let read = tx.read();
        let queue: Option<QueueView> = read.get("queue", wait.project_id)?;
        let item: Option<AttentionItem> = read.get("attention", wait.attention_id)?;
        let open = item.filter(|item| item.resolved_at.is_none());
        if open.is_some() && queue.is_some_and(|q| q.paused) {
            continue;
        }
        tx.delete(UNTIL_RESUMED, wait.attention_id)?;
        if let Some(item) = open {
            let task_id = item.task_id;
            resolve(tx, item, now)?;
            if let Some(task_id) = task_id
                && tx.read().get::<Task>("task", task_id)?.is_some()
            {
                crate::runs::recount_attention(tx, task_id)?;
            }
        }
    }
    Ok(())
}

fn resolve(
    tx: &mut store::Write<'_>,
    mut item: AttentionItem,
    now: Timestamp,
) -> store::Result<()> {
    item.resolved_at = Some(now);
    tx.put("attention", item.attention_id, &item)?;
    tx.emit(Event::AttentionResolved {
        attention_id: item.attention_id,
    })?;
    Ok(())
}

fn settle_tasks(
    tx: &mut store::Write<'_>,
    mut tasks: Vec<TaskId>,
    now: Timestamp,
) -> store::Result<()> {
    tasks.sort_unstable();
    tasks.dedup();
    for task_id in tasks {
        settle_task(tx, task_id, now)?;
    }
    Ok(())
}

/// Review results are kept to look at; everything else waits on the person.
fn blocks(kind: AttentionKind) -> bool {
    !matches!(kind, AttentionKind::Completed | AttentionKind::Failure)
}

/// Where a task is now, as what it once waited on is checked against.
struct Now {
    task: Option<Task>,
    /// Its latest run.
    run: Option<Run>,
    /// When it was put in Next up, while it waits there.
    queued_at: Option<Timestamp>,
    /// What asks for the go-ahead at the front of its project's Next up,
    /// which Next up raises and settles itself.
    go_ahead: Option<AttentionId>,
}

fn settle_task(tx: &mut store::Write<'_>, task_id: TaskId, now: Timestamp) -> store::Result<()> {
    let open: Vec<AttentionItem> = (tx.read().task_attention::<AttentionItem>(task_id)?)
        .into_iter()
        .filter(|item| {
            item.task_id == Some(task_id) && item.resolved_at.is_none() && blocks(item.kind)
        })
        .collect();
    if open.is_empty() {
        return Ok(());
    }
    let read = tx.read();
    let task: Option<Task> = read.get("task", task_id)?;
    let run: Option<Run> = match task.as_ref().and_then(|t| t.current_run) {
        Some(run_id) => read.get("run", run_id)?,
        None => None,
    };
    let (queued_at, go_ahead) = match &task {
        Some(t) => (
            (read.get::<QueueView>("queue", t.project_id)?).and_then(|q| {
                (q.items.iter())
                    .filter(|item| item.task_id == task_id && item.is_pending())
                    .map(|item| item.added_at)
                    .max()
            }),
            read.get::<AttentionId>("queue_attention", t.project_id)?,
        ),
        None => (None, None),
    };
    let at = Now {
        task,
        run,
        queued_at,
        go_ahead,
    };
    let mut settled = false;
    for item in open {
        let approval: Option<Approval> = match item.approval_id {
            Some(id) => tx.read().get("approval", id)?,
            None => None,
        };
        if !moved_on(&item, &at, approval.as_ref()) {
            continue;
        }
        resolve(tx, item, now)?;
        settled = true;
    }
    if settled && at.task.is_some() {
        crate::runs::recount_attention(tx, task_id)?;
    }
    Ok(())
}

/// Whether `item` no longer waits on the person.
fn moved_on(item: &AttentionItem, at: &Now, approval: Option<&Approval>) -> bool {
    // A question or approval waits exactly as long as it is undecided.
    if matches!(item.kind, AttentionKind::Approval | AttentionKind::Question) {
        return item.approval_id.is_some()
            && approval.is_none_or(|a| a.state != ApprovalState::Pending);
    }
    let Some(task) = &at.task else {
        return true;
    };
    if task.state == TaskState::Archived {
        return true;
    }
    if item.kind != AttentionKind::WaitingForYou || at.go_ahead == Some(item.attention_id) {
        return false;
    }
    // Started again since, by anyone: what the run asks, it asks itself.
    let restarted = at
        .run
        .as_ref()
        .is_some_and(|run| run.admitted_at > item.created_at && item.run_id != Some(run.run_id));
    let queued = at.queued_at.is_some_and(|added| added > item.created_at);
    let pr_done = item.topic == Some(AlertTopic::PrNeedsYou)
        && (task.github.as_deref())
            .is_some_and(|g| matches!(g.stage, Some(PrStage::Merged | PrStage::Closed)));
    restarted || queued || pr_done
}
