// SPDX-License-Identifier: Apache-2.0
//! A chat transfers only its bounded transcript. Native sessions stay with
//! their own agent, and the exact text and boundary are frozen at admission.

use super::{RunIntent, still_finishing};
use protocol::model::*;

#[cfg(test)]
#[path = "chat_handover_tests.rs"]
mod tests;

pub(super) const DELIVERED: &str = "chat_handover_delivered";

pub(super) struct Prepared {
    pub resume: Option<String>,
    pub resume_from_mode: Option<TaskMode>,
    pub boundary: ChatHandover,
    pub text: String,
}

pub(super) fn prepare(read: &store::Read<'_>, task: &Task) -> store::Result<Option<Prepared>> {
    if !task.conversation {
        return Ok(None);
    }
    let Some(current_id) = task.current_run else {
        return Ok(None);
    };
    let current: Run = read.require("run", current_id)?;
    if current.harness == task.harness {
        // A launch that failed before opening a native session keeps the exact
        // handover. An opened session may have read it without acknowledging it.
        if current.state == RunState::Failed
            && current.native_session.is_none()
            && let Some(boundary) = current.chat_handover.clone()
            && !read.get::<bool>(DELIVERED, current_id)?.unwrap_or(false)
            && let Some(intent) = read.get::<RunIntent>("run_intent", current_id)?
            && let Some(text) = intent.chat_handover_text
        {
            return Ok(Some(Prepared {
                resume: intent.resume,
                resume_from_mode: intent.resume_from_mode,
                boundary,
                text,
            }));
        }
        // An uncertain launch may have consumed the handover without an
        // acknowledgement. Never append it twice to that native session:
        // carrying on starts a fresh one with recent confirmed conversation.
        if current.chat_handover.is_some()
            && !read.get::<bool>(DELIVERED, current_id)?.unwrap_or(false)
        {
            let through_seq = read.head()?;
            return Ok(Some(Prepared {
                resume: None,
                resume_from_mode: None,
                boundary: ChatHandover {
                    after_seq: None,
                    through_seq,
                    resumed: false,
                },
                text: crate::turned_from_chat::handover(read, task.task_id, None, through_seq)?,
            }));
        }
        return Ok(None);
    }

    let through_seq = read.head()?;
    let catalog: Vec<protocol::capability::HarnessInfo> =
        read.get("meta", "harnesses")?.unwrap_or_default();
    let can_resume = catalog
        .iter()
        .find(|info| info.kind == task.harness)
        .is_some_and(|info| info.capabilities.resume_session.is_available());
    let mut resume = None;
    let mut resume_from_mode = None;
    let mut after_seq = None;
    // Search the newest ended turn of the destination agent, never a different
    // agent's session. If its end is outside this bounded search, start fresh.
    let mut before = None;
    'history: for _ in 0..5 {
        let (page, more) = read.timeline(task.task_id, before, 200)?;
        before = page.first().map(|item| item.seq);
        for item in page.into_iter().rev() {
            let TimelineBody::RunEnded { run_id, .. } = item.item else {
                continue;
            };
            let Some(previous) = read.get::<Run>("run", run_id)? else {
                continue;
            };
            if previous.harness != task.harness {
                continue;
            }
            // An uncertain native turn is not safe to resume implicitly. Nor
            // is a turn whose helpers may still be running.
            if can_resume && previous.state.is_terminal() && !still_finishing(&previous) {
                let delivered = previous.chat_handover.is_none()
                    || read.get::<bool>(DELIVERED, run_id)?.unwrap_or(false);
                if delivered && previous.native_session.is_some() {
                    resume = previous.native_session;
                    resume_from_mode = previous.mode;
                    after_seq = Some(item.seq);
                } else if previous.state == RunState::Failed
                    && previous.native_session.is_none()
                    && let Some(intent) = read.get::<RunIntent>("run_intent", run_id)?
                    && intent.resume.is_some()
                    && let Some(boundary) = previous.chat_handover
                {
                    // It never received the earlier handover. Resume its known
                    // earlier session and include everything it still missed.
                    resume = intent.resume;
                    resume_from_mode = intent.resume_from_mode;
                    after_seq = boundary.after_seq;
                }
            }
            break 'history;
        }
        if !more {
            break;
        }
    }
    let boundary = ChatHandover {
        after_seq,
        through_seq,
        resumed: resume.is_some(),
    };
    let text = crate::turned_from_chat::handover(read, task.task_id, after_seq, through_seq)?;
    Ok(Some(Prepared {
        resume,
        resume_from_mode,
        boundary,
        text,
    }))
}
