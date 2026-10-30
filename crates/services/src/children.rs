// SPDX-License-Identifier: Apache-2.0
//! Child-scoped interventions retain the same admission and evidence rules as Stop.
use crate::{CompanionHub, error};
use ports::harness::ChildStopResult;
use protocol::{capability::Availability, model::*, *};
use std::{sync::Arc, time::Duration};

pub(crate) fn stoppable(state: ChildState) -> bool {
    matches!(state, ChildState::Active | ChildState::Unknown)
}

/// Finished helpers a run lists. An agent can start thousands, and every one
/// in the run would make its record larger than a control frame.
const LISTED_FINISHED: usize = 100;

/// Keep every helper that may still be running, which a person may need to
/// stop or check, and the finished ones that ended most recently. The rest
/// are counted instead, and returned.
pub(crate) fn list_recent(run: &mut Run) -> Vec<ChildAgent> {
    let mut finished: Vec<usize> = (0..run.children.len())
        .filter(|&i| !stoppable(run.children[i].state))
        .collect();
    let excess = finished.len().saturating_sub(LISTED_FINISHED);
    if excess == 0 {
        return Vec::new();
    }
    finished.sort_by_key(|&i| (run.children[i].ended_at, i));
    let unlisted: std::collections::HashSet<ChildId> = finished[..excess]
        .iter()
        .map(|&i| run.children[i].child_id)
        .collect();
    let (left, kept) = std::mem::take(&mut run.children)
        .into_iter()
        .partition(|c| unlisted.contains(&c.child_id));
    run.children = kept;
    let earlier = run.earlier_children.unwrap_or(0);
    run.earlier_children = Some(earlier.saturating_add(excess as u32));
    left
}

/// Where a run keeps the helpers it no longer lists, by native identity, with
/// the state each ended in. A later report of one is then not a new helper.
pub(crate) fn unlisted(run_id: RunId) -> String {
    format!("unlisted_helper:{run_id}")
}

/// Still running, and only the turn it belongs to can stop it.
fn ends_with_turn(child: &ChildAgent) -> bool {
    stoppable(child.state) && matches!(child.stop, Some(Availability::Unsupported { .. }))
}

pub(crate) fn child_fact(
    child: &ChildAgent,
    result: Result<ChildStopResult, String>,
) -> ReceiptFact {
    let (status, detail) = match result {
        Ok(result) => (
            match result.state {
                ChildState::Stopped | ChildState::Completed => FactStatus::Done,
                ChildState::Active => FactStatus::StillActive,
                _ => FactStatus::Unknown,
            },
            result.detail,
        ),
        Err(reason) => (FactStatus::Unknown, reason),
    };
    ReceiptFact {
        label: format!("Child: {}", child.label),
        status,
        detail: Some(detail),
    }
}

async fn stop_one(control: Option<Arc<dyn ports::RunControl>>, child: &ChildAgent) -> ReceiptFact {
    let result = match (control, child.native_ref.as_deref()) {
        (Some(control), Some(native_ref)) => {
            match tokio::time::timeout(Duration::from_secs(4), control.stop_child(native_ref)).await {
                Ok(result) => result.map_err(|e| e.to_string()),
                Err(_) => Err("The agent did not confirm this helper stopped within four seconds".into()),
            }
        }
        _ => Err("The helper’s control connection is unavailable. Open the task’s conversation on the computer and check the helper’s task details and output.".into()),
    };
    child_fact(child, result)
}

impl CompanionHub {
    pub(crate) fn child_control_closed(&self, run_id: RunId) -> Result<(), ProtocolError> {
        self.write(|tx| {
            let mut run: Run = tx.read().require("run", run_id)?;
            let now = self.inner.clock.now();
            let exited = tx
                .read()
                .get::<bool>("run_process_ended", run_id)?
                .unwrap_or(false);
            for child in run.children.iter_mut().filter(|c| stoppable(c.state)) {
                if exited && child.lifetime == Some(ChildLifetime::InProcess) {
                    child.state = ChildState::Stopped;
                    child.ended_at = Some(now);
                    child.provenance = Provenance::Observed;
                    child.stop = Some(Availability::Unsupported {
                        reason: "Ended with the agent process".into(),
                    });
                } else {
                    child.state = ChildState::Unknown;
                    child.ended_at = None;
                    child.stop = Some(Availability::TemporarilyUnavailable {
                        reason: helper_check_location(run.harness).into(),
                    });
                }
            }
            let mut task: Task = tx.read().require("task", run.task_id)?;
            let attention = tx.read().list::<AttentionItem>("attention")?;
            for mut item in attention
                .iter()
                .filter(|a| {
                    a.run_id == Some(run_id) && a.child_id.is_some() && a.resolved_at.is_none()
                })
                .cloned()
            {
                if !run
                    .children
                    .iter()
                    .any(|c| Some(c.child_id) == item.child_id && stoppable(c.state))
                {
                    item.resolved_at = Some(now);
                    tx.put("attention", item.attention_id, &item)?;
                    tx.emit(Event::AttentionResolved {
                        attention_id: item.attention_id,
                    })?;
                }
            }
            for child in run
                .children
                .iter()
                .filter(|c| c.state == ChildState::Unknown)
            {
                if run.helpers_checked.is_some()
                    || run.human_resolution.is_some()
                    || attention.iter().any(|a| {
                        a.run_id == Some(run_id)
                            && a.child_id == Some(child.child_id)
                            && a.resolved_at.is_none()
                    })
                {
                    continue;
                }
                let item = AttentionItem {
                    attention_id: AttentionId::new(),
                    child_id: Some(child.child_id),
                    kind: AttentionKind::ReconciliationRequired,
                    title: format!("{} · Helper '{}' — state unknown", task.title, child.label),
                    body: Some(helper_check_location(run.harness).into()),
                    task_id: Some(run.task_id),
                    run_id: Some(run_id),
                    approval_id: None,
                    created_at: now,
                    resolved_at: None,
                    priority: 5,
                    topic: None,
                    quiet: false,
                    report_id: None,
                };
                tx.put("attention", item.attention_id, &item)?;
                tx.emit(Event::AttentionUpserted { item })?;
            }
            task.revision += 1;
            task.updated_at = now;
            task.attention_count = crate::runs::blocking_attention_count(&tx.read(), task.task_id)?;
            // Exit recovery can revisit this bookkeeping after the person
            // queued a continuation. Its message and go-ahead still stand.
            crate::runs::carry_permits(tx, &task, task.revision - 1)?;
            tx.put("task", task.task_id, &task)?;
            tx.emit(Event::TaskUpserted { task })?;
            tx.put("run", run_id, &run)?;
            tx.emit(Event::RunUpserted { run: Box::new(run) })?;
            Ok(())
        })
    }
    pub(crate) fn check_helpers(
        &self,
        tx: &mut store::Write<'_>,
        run_id: RunId,
        expected: Option<u64>,
        by: protocol::auth::Principal,
    ) -> store::Result<()> {
        let mut run: Run = tx.read().require("run", run_id)?;
        let mut task: Task = tx.read().require("task", run.task_id)?;
        domain::revision(expected, task.revision)?;
        if run.ended_at.is_none()
            || run.helpers_checked.is_some()
            || !run.children.iter().any(|c| c.state == ChildState::Unknown)
        {
            return Err(error(
                ErrorCode::Conflict,
                "There are no unchecked helper unknowns in this ended run",
            )
            .into());
        }
        let now = self.inner.clock.now();
        run.helpers_checked = Some(HumanCheck { at: now, by });
        for mut item in tx.read().list::<AttentionItem>("attention")? {
            if item.run_id == Some(run_id) && item.child_id.is_some() && item.resolved_at.is_none()
            {
                item.resolved_at = Some(now);
                tx.put("attention", item.attention_id, &item)?;
                tx.emit(Event::AttentionResolved {
                    attention_id: item.attention_id,
                })?;
            }
        }
        task.revision += 1;
        task.updated_at = now;
        task.attention_count = crate::runs::blocking_attention_count(&tx.read(), task.task_id)?;
        tx.put("task", task.task_id, &task)?;
        tx.emit(Event::TaskUpserted { task })?;
        tx.put("run", run_id, &run)?;
        tx.emit(Event::RunUpserted {
            run: Box::new(run.clone()),
        })?;
        Ok(())
    }

    pub(crate) fn admit_child_stop(
        &self,
        tx: &mut store::Write<'_>,
        run_id: RunId,
        child_id: ChildId,
        expected: Option<u64>,
    ) -> store::Result<()> {
        let run: Run = tx.read().require("run", run_id)?;
        let mut task: Task = tx.read().require("task", run.task_id)?;
        match expected {
            Some(expected) if crate::runs::names_run(&tx.read(), &task, run_id, expected)? => {}
            _ => domain::revision(expected, task.revision)?,
        }
        let child = run
            .children
            .iter()
            .find(|c| c.child_id == child_id)
            .ok_or_else(|| error(ErrorCode::NotFound, "Helper not found in this run"))?;
        if !stoppable(child.state) {
            return Err(error(ErrorCode::AlreadyResolved, "This helper has already ended").into());
        }
        domain::availability(child.stop.as_ref().unwrap_or(&Availability::Unsupported {
            reason: "This agent cannot stop this helper separately. Open the task’s conversation on the computer and check the helper’s task details and output.".into(),
        }))?;
        task.revision += 1;
        task.updated_at = self.inner.clock.now();
        tx.put("task", task.task_id, &task)?;
        tx.emit(Event::TaskUpserted { task })?;
        Ok(())
    }

    pub(crate) async fn stop_child(
        &self,
        run_id: RunId,
        child_id: ChildId,
    ) -> Result<protocol::command::CommandResult, ProtocolError> {
        let requested_at = self.inner.clock.now();
        let run: Run = self.read(|r| r.require("run", run_id))?;
        let child = run
            .children
            .iter()
            .find(|c| c.child_id == child_id)
            .ok_or_else(|| error(ErrorCode::NotFound, "Helper not found in this run"))?;
        let fact = if stoppable(child.state) {
            let control = self.inner.runs.lock().await.get(&run_id).cloned();
            stop_one(control, child).await
        } else {
            child_fact(
                child,
                Ok(ChildStopResult {
                    state: child.state,
                    detail: "The agent reported this helper ended before Stop reached it".into(),
                }),
            )
        };
        let confirmed = fact.status == FactStatus::Done;
        let receipt = InterventionReceipt {
            kind: InterventionKind::StopTask,
            requested_at,
            summary: if confirmed {
                "The helper has ended"
            } else {
                "The computer has not confirmed that the helper stopped"
            }
            .into(),
            facts: vec![fact],
        };
        self.write(|tx| {
            tx.emit(Event::InterventionRecorded {
                task_id: Some(run.task_id),
                target_id: None,
                receipt: Box::new(receipt.clone()),
            })?;
            Ok(())
        })?;
        Ok(protocol::command::CommandResult::Intervention {
            receipt: Box::new(receipt),
        })
    }

    /// Helpers the agent can't stop one at a time end with an interrupted
    /// turn; give its report of that a moment to arrive.
    async fn settle_turn_helpers(&self, run_id: RunId) -> Result<Run, ProtocolError> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        loop {
            let run: Run = self.read(|r| r.require("run", run_id))?;
            if !run.children.iter().any(ends_with_turn) || tokio::time::Instant::now() >= deadline {
                return Ok(run);
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    pub(crate) async fn stop_run_children(
        &self,
        run_id: RunId,
        before: Vec<ChildAgent>,
        interrupted: bool,
    ) -> Result<Vec<ReceiptFact>, ProtocolError> {
        let run: Run = if interrupted {
            self.settle_turn_helpers(run_id).await?
        } else {
            self.read(|r| r.require("run", run_id))?
        };
        let mut facts = Vec::new();
        // A helper that ended during Stop may have left the list since.
        let listed: std::collections::HashSet<ChildId> =
            run.children.iter().map(|c| c.child_id).collect();
        for child in (before.iter()).filter(|c| stoppable(c.state) && !listed.contains(&c.child_id))
        {
            let ended = match &child.native_ref {
                Some(native) => self.read(|r| r.get::<ChildState>(&unlisted(run_id), native))?,
                None => None,
            };
            let result = ended
                .map(|state| ChildStopResult {
                    state,
                    detail: "The agent reported this helper ended during Stop".into(),
                })
                .ok_or_else(|| "The agent’s last report of this helper can’t be found".into());
            facts.push((usize::MAX, child_fact(child, result)));
        }
        let control = self.inner.runs.lock().await.get(&run_id).cloned();
        let mut work = tokio::task::JoinSet::new();
        for (index, child) in run.children.into_iter().enumerate().filter(|(_, c)| {
            stoppable(c.state)
                || before
                    .iter()
                    .any(|old| old.child_id == c.child_id && stoppable(old.state))
        }) {
            let control = control.clone();
            work.spawn(async move {
                let fact = if let (true, Some(Availability::Unsupported { reason })) =
                    (ends_with_turn(&child), &child.stop)
                {
                    child_fact(
                        &child,
                        Err(if interrupted {
                            "The agent didn’t report this helper ending with the stopped turn. Check it on the computer.".into()
                        } else {
                            reason.clone()
                        }),
                    )
                } else if stoppable(child.state) {
                    stop_one(control, &child).await
                } else {
                    child_fact(
                        &child,
                        Ok(ChildStopResult {
                            state: child.state,
                            detail: "The agent reported this helper ended during Stop".into(),
                        }),
                    )
                };
                (index, fact)
            });
        }
        while let Some(result) = work.join_next().await {
            match result {
                Ok(fact) => facts.push(fact),
                Err(_) => facts.push((
                    usize::MAX,
                    ReceiptFact {
                        label: "Helpers".into(),
                        status: FactStatus::Unknown,
                        detail: Some("A helper’s result could not be collected".into()),
                    },
                )),
            }
        }
        facts.sort_by_key(|(index, _)| *index);
        Ok(facts.into_iter().map(|(_, fact)| fact).collect())
    }
}

fn helper_check_location(harness: protocol::capability::HarnessKind) -> &'static str {
    if harness == protocol::capability::HarnessKind::ClaudeCode {
        "Open this task’s conversation in Claude Code on the computer and check the helper’s task details and output."
    } else {
        "Open this task’s conversation in the agent app on the computer and check the helper’s task details and output."
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thousands_of_helpers_fit_one_frame() {
        let helper = |n: u32, state| ChildAgent {
            lifetime: None,
            child_id: ChildId::new(),
            native_ref: Some(format!("helper-{n}")),
            label: format!("{n} {}", "h".repeat(crate::native_text::LABEL)),
            state,
            started_at: protocol::time::Timestamp(0),
            ended_at: (state != ChildState::Active).then_some(protocol::time::Timestamp(n.into())),
            provenance: Provenance::Native,
            stop: None,
        };
        let mut run: Run = serde_json::from_value(serde_json::json!({
            "run_id": RunId::new(), "task_id": TaskId::new(), "harness": "codex",
            "requested_model": null, "actual_model": null, "requested_effort": null,
            "actual_effort": null, "account_label": null, "native_session": null,
            "state": "working", "admitted_at": 0, "dispatched_at": 0, "acknowledged_at": 0,
            "ended_at": null, "children": [], "outcome": null, "holds_writer": true,
        }))
        .unwrap();
        // Finished in reverse order of report, so the listed ones are the
        // last to end rather than the last reported.
        run.children = (0..5000)
            .map(|n| {
                if n % 100 == 0 {
                    helper(n, ChildState::Active)
                } else {
                    helper(5000 - n, ChildState::Completed)
                }
            })
            .collect();
        let frame = 1024 * 1024;
        assert!(
            serde_json::to_vec(&Event::RunUpserted {
                run: Box::new(run.clone())
            })
            .unwrap()
            .len()
                > frame
        );
        list_recent(&mut run);
        assert!(
            serde_json::to_vec(&Event::RunUpserted {
                run: Box::new(run.clone())
            })
            .unwrap()
            .len()
                < frame
        );
        assert_eq!(
            run.children
                .iter()
                .filter(|c| c.state == ChildState::Active)
                .count(),
            50
        );
        let mut ended: Vec<_> = run
            .children
            .iter()
            .filter(|c| c.state == ChildState::Completed)
            .map(|c| c.ended_at.unwrap().0)
            .collect();
        ended.sort();
        let latest: Vec<i64> = (1..5000)
            .filter(|n| n % 100 != 0)
            .skip(4950 - 100)
            .collect();
        assert_eq!(ended, latest);
        assert_eq!(run.earlier_children, Some(4950 - 100));
    }
}
