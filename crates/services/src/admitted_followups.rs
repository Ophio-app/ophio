// SPDX-License-Identifier: Apache-2.0
use super::*;

/// The run and conversation row accepted for this command never change, even
/// if the task starts another turn before its send worker runs.
#[derive(Debug, Serialize, Deserialize)]
pub(super) struct Followup {
    pub run_id: RunId,
    pub seq: u64,
}

impl CompanionHub {
    pub(crate) fn admit_followup(
        &self,
        tx: &mut store::Write<'_>,
        command_id: CommandId,
        task: &Task,
        text: &str,
        bundle: Option<&AnnotationBundle>,
    ) -> store::Result<()> {
        let id = task
            .current_run
            .ok_or_else(|| error(ErrorCode::Conflict, "No active run"))?;
        if let Some(bundle) = bundle {
            crate::results::link_bundle(tx, task.task_id, Some(id), bundle)?;
        }
        let seq = tx.emit(Event::TimelineAppended {
            task_id: task.task_id,
            item: TimelineItem {
                seq: 0,
                at: self.inner.clock.now(),
                run_id: Some(id),
                item: TimelineBody::UserMessage {
                    text: text.into(),
                    bundle: bundle.cloned().map(Box::new),
                    delivery: DeliveryState::HostAccepted,
                    carried_plan: None,
                },
            },
        })?;
        tx.put("followup", command_id, &Followup { run_id: id, seq })?;
        let mut messages: Vec<InFlightMessage> =
            tx.read().get("message_delivery", id)?.unwrap_or_default();
        messages.push(InFlightMessage {
            message_ref: Some(MessageRef(command_id)),
            seq,
            state: DeliveryState::HostAccepted,
            undispatched: true,
        });
        tx.put("message_delivery", id, &messages)
    }
}

/// Only an undispatched message is known not to have reached the adapter.
/// Removing its tracking entry makes execution, run-end and restart
/// settlement idempotent; native uncertainty must stay uncertain.
pub(crate) fn fail_undispatched_followup(
    tx: &mut store::Write<'_>,
    command_id: CommandId,
    now: protocol::time::Timestamp,
) -> store::Result<()> {
    let Some(message) = tx.read().get::<Followup>("followup", command_id)? else {
        return Ok(());
    };
    let id = message.run_id;
    let mut messages: Vec<InFlightMessage> =
        tx.read().get("message_delivery", id)?.unwrap_or_default();
    let Some(index) = messages
        .iter()
        .position(|m| m.message_ref == Some(MessageRef(command_id)) && m.undispatched)
    else {
        return Ok(());
    };
    messages.remove(index);
    tx.put("message_delivery", id, &messages)?;
    let run: Run = tx.read().require("run", id)?;
    tx.emit(Event::TimelineAppended {
        task_id: run.task_id,
        item: TimelineItem {
            seq: 0,
            at: now,
            run_id: Some(id),
            item: unsent(tx, id, message.seq)?,
        },
    })?;
    Ok(())
}
