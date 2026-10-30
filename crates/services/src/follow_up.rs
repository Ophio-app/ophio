// SPDX-License-Identifier: Apache-2.0
//! Whether a run that finished as done needed a follow-up: recorded on it
//! when the task's next run ends, from what that run did, never from what
//! either agent said about its work.
//!
//! A Work run that ended normally, and didn't report work left or a need
//! for the person, finished as done. It needed a follow-up when the run
//! after it on the same task changed files, or ran a check that failed
//! before it changed anything. Only the run just before is looked at: one
//! that left work or needed the person, a Plan or an Ask, expects what
//! comes next.

use protocol::{
    Event, RunId, TaskId,
    model::{FollowUp, FollowUpReason, ReportStatus, Run, RunState, TaskMode},
    time::Timestamp,
};

/// Notes on the run before `run` that it needed one, if it did.
pub(crate) fn note(tx: &mut store::Write<'_>, run: &Run, now: Timestamp) -> store::Result<()> {
    let Some(reason) = reason(run) else {
        return Ok(());
    };
    let Some(before) = tx.read().history_run_before(run.task_id, run.run_id)? else {
        return Ok(());
    };
    if !finished_as_done(&before) || before.follow_up.is_some() {
        return Ok(());
    }
    record(
        tx,
        before.run_id,
        run.task_id,
        FollowUp {
            run_id: run.run_id,
            at: now,
            reason,
        },
    )
}

/// What a run that ended normally did that its predecessor should have.
fn reason(run: &Run) -> Option<FollowUpReason> {
    let outcome = run.outcome.as_ref()?;
    // Only a run that ended normally: what one stopped or failed partway
    // through did says little about the run before it.
    if run.state != RunState::Completed || run.human_resolution.is_some() {
        None
    } else if outcome
        .verification
        .as_ref()
        .is_some_and(|v| v.failed_before_changes)
    {
        Some(FollowUpReason::CheckFailed)
    } else if !outcome.changed_files.is_empty() {
        Some(FollowUpReason::ChangedFiles)
    } else {
        None
    }
}

fn finished_as_done(run: &Run) -> bool {
    run.state == RunState::Completed
        && run.ended_at.is_some()
        && run.mode == Some(TaskMode::Work)
        && run.human_resolution.is_none()
        && run
            .outcome
            .as_ref()
            .and_then(|o| o.report.as_ref())
            .is_none_or(|r| r.status == ReportStatus::Done)
}

fn record(
    tx: &mut store::Write<'_>,
    id: RunId,
    task: TaskId,
    follow_up: FollowUp,
) -> store::Result<()> {
    let mut run: Run = tx.read().require("run", id)?;
    if run.task_id != task {
        return Ok(());
    }
    run.follow_up = Some(follow_up);
    tx.put("run", id, &run)?;
    tx.emit(Event::RunUpserted { run: Box::new(run) })?;
    Ok(())
}
