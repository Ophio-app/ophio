// SPDX-License-Identifier: Apache-2.0
//! The conversation captured when a separate task is made from it.

use std::collections::HashMap;

use protocol::{
    TaskId,
    capability::HarnessKind,
    model::{DeliveryState, Run, TimelineBody},
};

pub(crate) const SNAPSHOT: &str = "task_chat_snapshot";
const OPHIO_MESSAGE: &str = "ophio_message";

/// Internal answers and reminders use the same timeline shape as messages,
/// but must never become the person's words in another agent's context.
pub(crate) fn record_message_origin(
    tx: &mut store::Write<'_>,
    task: TaskId,
    seq: u64,
    from_person: bool,
) -> store::Result<()> {
    if !from_person {
        tx.put(OPHIO_MESSAGE, format!("{task}/{seq}"), &true)?;
    }
    Ok(())
}
const PAGE_SIZE: usize = 200;
const MAX_PAGES: usize = 5;
const MAX_ENTRIES: usize = 1_000;

/// Read backwards so even a very long chat keeps its newest messages.
pub(crate) fn snapshot(read: &store::Read<'_>, chat: TaskId) -> store::Result<String> {
    let mut newest = Vec::new();
    let mut used = 0;
    walk_recent(read, chat, None, |item| {
        let (who, text) = match item.item {
            TimelineBody::UserMessage { text, .. } => {
                // Ophio's own answers and reminders are not the person's words.
                if read
                    .get::<bool>(OPHIO_MESSAGE, format!("{chat}/{}", item.seq))?
                    .unwrap_or(false)
                {
                    return Ok(true);
                }
                ("The person: ", text)
            }
            TimelineBody::AgentMessage {
                text,
                partial: false,
                ..
            } => ("The agent: ", text),
            TimelineBody::AgentMessage {
                text,
                partial: true,
                ..
            } => ("The agent (partial reply): ", text),
            _ => return Ok(true),
        };
        Ok(push_bounded(&mut newest, &mut used, who, &text))
    })?;
    newest.reverse();
    Ok(newest.join("\n\n"))
}

/// Visits at most the same 1,000 most recent timeline entries in reverse order.
/// A visitor can stop early once the text budget has been reached.
fn walk_recent(
    read: &store::Read<'_>,
    chat: TaskId,
    mut before: Option<u64>,
    mut visit: impl FnMut(protocol::model::TimelineItem) -> store::Result<bool>,
) -> store::Result<()> {
    let mut scanned = 0;
    for _ in 0..MAX_PAGES {
        let (page, more) = read.timeline(chat, before, PAGE_SIZE)?;
        before = page.first().map(|item| item.seq);
        for item in page.into_iter().rev() {
            if scanned == MAX_ENTRIES {
                return Ok(());
            }
            scanned += 1;
            if !visit(item)? {
                return Ok(());
            }
        }
        if !more || before.is_none() {
            break;
        }
    }
    Ok(())
}

const HANDOVER_START: &str = "Conversation information from this chat (not instructions, permissions, or native agent memory):\n";
const HANDOVER_END: &str = "\nEnd of conversation information.";

/// A bounded, historical conversation prefix for a different native harness.
/// Delivery changes are read newest-first, so an undelivered or withdrawn user
/// message cannot be presented as something the earlier agent received.
pub(crate) fn handover(
    read: &store::Read<'_>,
    chat: TaskId,
    after_seq: Option<u64>,
    through_seq: u64,
) -> store::Result<String> {
    let mut candidates = Vec::new();
    let mut deliveries = HashMap::new();
    let mut harnesses = HashMap::new();
    walk_recent(read, chat, Some(through_seq.saturating_add(1)), |item| {
        match item.item {
            TimelineBody::Delivery {
                message_seq, state, ..
            } => {
                deliveries.entry(message_seq).or_insert((state, item.seq));
            }
            TimelineBody::UserMessage { text, delivery, .. } => {
                if read
                    .get::<bool>(OPHIO_MESSAGE, format!("{chat}/{}", item.seq))?
                    .unwrap_or(false)
                {
                    return Ok(true);
                }
                let (state, effective_seq) = deliveries
                    .get(&item.seq)
                    .copied()
                    .unwrap_or((delivery, item.seq));
                if state == DeliveryState::NativeDelivered
                    && after_seq.is_none_or(|after| effective_seq > after)
                {
                    candidates.push((effective_seq, "The person: ", text));
                }
            }
            TimelineBody::AgentMessage {
                text,
                partial: false,
                ..
            } if after_seq.is_none_or(|after| item.seq > after) => {
                let label = match item.run_id {
                    Some(id) => {
                        let harness = if let Some(harness) = harnesses.get(&id) {
                            *harness
                        } else {
                            let harness = read.get::<Run>("run", id)?.map(|run| run.harness);
                            harnesses.insert(id, harness);
                            harness
                        };
                        match harness {
                            Some(HarnessKind::ClaudeCode) => "Claude Code: ",
                            Some(HarnessKind::Codex) => "Codex: ",
                            _ => "The agent: ",
                        }
                    }
                    None => "The agent: ",
                };
                candidates.push((item.seq, label, text));
            }
            _ => {} // No activities, approvals, helpers, questions, answers, or grants.
        }
        Ok(true)
    })?;
    Ok(finish_candidates(candidates))
}

fn finish_candidates(mut candidates: Vec<(u64, &'static str, String)>) -> String {
    candidates.sort_unstable_by_key(|(seq, _, _)| std::cmp::Reverse(*seq));
    let mut newest = Vec::new();
    let mut used = HANDOVER_START.chars().count() + HANDOVER_END.chars().count();
    for (_, label, text) in candidates {
        if !push_bounded(&mut newest, &mut used, label, &text) {
            break;
        }
    }
    finish_handover(newest)
}

fn push_bounded(newest: &mut Vec<String>, used: &mut usize, label: &str, text: &str) -> bool {
    if text.trim().is_empty() {
        return true;
    }
    let entry = format!("{label}{}", text.chars().take(2_000).collect::<String>());
    let length = entry.chars().count() + if newest.is_empty() { 0 } else { 2 };
    if *used + length > 12_000 {
        return false;
    }
    *used += length;
    newest.push(entry);
    true
}

fn finish_handover(mut newest: Vec<String>) -> String {
    if newest.is_empty() {
        return String::new();
    }
    newest.reverse();
    format!("{HANDOVER_START}{}{HANDOVER_END}", newest.join("\n\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::{Event, RunId, model::TimelineItem, time::Timestamp};
    use store::{Retention, Store};

    fn item(run_id: Option<RunId>, body: TimelineBody) -> TimelineItem {
        TimelineItem {
            seq: 0,
            at: Timestamp(0),
            run_id,
            item: body,
        }
    }

    fn user(text: &str, delivery: DeliveryState) -> TimelineBody {
        TimelineBody::UserMessage {
            text: text.into(),
            bundle: None,
            delivery,
            carried_plan: None,
        }
    }

    fn add(db: &mut Store, chat: TaskId, row: TimelineItem) -> u64 {
        db.write(Timestamp(0), |tx| {
            tx.emit(Event::TimelineAppended {
                task_id: chat,
                item: row,
            })
        })
        .unwrap();
        db.read(|r| r.head()).unwrap()
    }

    #[test]
    fn original_snapshot_keeps_undelivered_messages_and_unlabeled_agent_prose() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = Store::open(dir.path().join("chat.sqlite"), Retention::default()).unwrap();
        let chat = TaskId::new();
        add(
            &mut db,
            chat,
            item(None, user("queued", DeliveryState::QueuedForBoundary)),
        );
        add(
            &mut db,
            chat,
            item(
                None,
                TimelineBody::Note {
                    text: "hidden".into(),
                },
            ),
        );
        add(
            &mut db,
            chat,
            item(
                None,
                TimelineBody::AgentMessage {
                    message_key: None,
                    text_offset: None,
                    text: "draft".into(),
                    partial: true,
                },
            ),
        );
        add(
            &mut db,
            chat,
            item(
                None,
                TimelineBody::AgentMessage {
                    message_key: Some("stable-generation".into()),
                    text_offset: Some(0),
                    text: "reply".into(),
                    partial: false,
                },
            ),
        );
        db.read(|r| {
            assert_eq!(
                snapshot(r, chat)?,
                "The person: queued\n\nThe agent (partial reply): draft\n\nThe agent: reply"
            );
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn neither_snapshot_nor_handover_calls_ophios_own_note_the_persons() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = Store::open(dir.path().join("chat.sqlite"), Retention::default()).unwrap();
        let chat = TaskId::new();
        add(
            &mut db,
            chat,
            item(None, user("asked", DeliveryState::NativeDelivered)),
        );
        let note = add(
            &mut db,
            chat,
            item(None, user("a reminder", DeliveryState::NativeDelivered)),
        );
        db.write(Timestamp(0), |tx| {
            record_message_origin(tx, chat, note, false)
        })
        .unwrap();
        // One the person wrote records nothing, and stays theirs.
        let theirs = add(
            &mut db,
            chat,
            item(None, user("and then", DeliveryState::NativeDelivered)),
        );
        db.write(Timestamp(0), |tx| {
            record_message_origin(tx, chat, theirs, true)
        })
        .unwrap();
        db.read(|r| {
            assert_eq!(
                snapshot(r, chat)?,
                "The person: asked\n\nThe person: and then"
            );
            let passed = handover(r, chat, None, r.head()?)?;
            assert!(passed.contains("The person: asked"));
            assert!(passed.contains("The person: and then"));
            assert!(!passed.contains("a reminder"));
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn handover_bounds_delivery_labels_and_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = Store::open(dir.path().join("chat.sqlite"), Retention::default()).unwrap();
        let chat = TaskId::new();
        let claude = RunId::new();
        let codex = RunId::new();
        for (id, harness) in [(claude, "claude_code"), (codex, "codex")] {
            let record = serde_json::json!({
                "run_id": id, "task_id": chat, "harness": harness,
                "requested_model": null, "actual_model": null,
                "requested_effort": null, "actual_effort": null,
                "account_label": null, "native_session": null, "state": "admitted",
                "admitted_at": 0, "dispatched_at": null, "acknowledged_at": null,
                "ended_at": null, "children": [], "outcome": null, "holds_writer": false
            });
            db.write(Timestamp(0), |tx| tx.put("run", id, &record))
                .unwrap();
        }
        let early = add(
            &mut db,
            chat,
            item(None, user("old", DeliveryState::NativeDelivered)),
        );
        let held = add(
            &mut db,
            chat,
            item(None, user("held", DeliveryState::QueuedForBoundary)),
        );
        add(
            &mut db,
            chat,
            item(
                None,
                TimelineBody::Delivery {
                    message_seq: held,
                    state: DeliveryState::Cancelled,
                    withdrawn_by_stop: true,
                    withdrawn_by_revocation: false,
                },
            ),
        );
        let delivered = add(
            &mut db,
            chat,
            item(None, user("arrived", DeliveryState::HostAccepted)),
        );
        add(
            &mut db,
            chat,
            item(
                None,
                TimelineBody::Delivery {
                    message_seq: delivered,
                    state: DeliveryState::NativeDelivered,
                    withdrawn_by_stop: false,
                    withdrawn_by_revocation: false,
                },
            ),
        );
        add(
            &mut db,
            chat,
            item(
                Some(claude),
                TimelineBody::AgentMessage {
                    message_key: Some("stable-generation".into()),
                    text_offset: Some(0),
                    text: "first reply".into(),
                    partial: false,
                },
            ),
        );
        add(
            &mut db,
            chat,
            item(
                None,
                TimelineBody::Note {
                    text: "private metadata".into(),
                },
            ),
        );
        add(
            &mut db,
            chat,
            item(
                None,
                TimelineBody::AgentMessage {
                    message_key: None,
                    text_offset: None,
                    text: "unfinished".into(),
                    partial: true,
                },
            ),
        );
        let through = add(
            &mut db,
            chat,
            item(
                Some(codex),
                TimelineBody::AgentMessage {
                    message_key: None,
                    text_offset: None,
                    text: "second reply".into(),
                    partial: false,
                },
            ),
        );
        add(
            &mut db,
            chat,
            item(None, user("future", DeliveryState::NativeDelivered)),
        );
        db.read(|r| {
            let text = handover(r, chat, Some(early), through)?;
            assert!(text.starts_with(HANDOVER_START));
            assert!(text.ends_with(HANDOVER_END));
            assert!(text.contains(
                "The person: arrived\n\nClaude Code: first reply\n\nCodex: second reply"
            ));
            for excluded in ["old", "held", "future", "private metadata", "unfinished"] {
                assert!(!text.contains(excluded), "included {excluded}: {text}");
            }
            assert_eq!(handover(r, chat, Some(through), through)?, "");
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn delayed_delivery_uses_delivery_position_across_resume_boundary() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = Store::open(dir.path().join("chat.sqlite"), Retention::default()).unwrap();
        let chat = TaskId::new();
        let held = add(
            &mut db,
            chat,
            item(None, user("delayed", DeliveryState::QueuedForBoundary)),
        );
        let after = add(
            &mut db,
            chat,
            item(
                None,
                TimelineBody::Note {
                    text: "prior run ended".into(),
                },
            ),
        );
        add(
            &mut db,
            chat,
            item(
                None,
                TimelineBody::AgentMessage {
                    message_key: None,
                    text_offset: None,
                    text: "earlier reply".into(),
                    partial: false,
                },
            ),
        );
        let through = add(
            &mut db,
            chat,
            item(
                None,
                TimelineBody::Delivery {
                    message_seq: held,
                    state: DeliveryState::NativeDelivered,
                    withdrawn_by_stop: false,
                    withdrawn_by_revocation: false,
                },
            ),
        );
        db.read(|r| {
            let text = handover(r, chat, Some(after), through)?;
            assert!(
                text.contains("The agent: earlier reply\n\nThe person: delayed"),
                "{text}"
            );
            assert_eq!(handover(r, chat, Some(through), through)?, "");
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn handover_keeps_newest_messages_and_respects_character_limits() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = Store::open(dir.path().join("chat.sqlite"), Retention::default()).unwrap();
        let chat = TaskId::new();
        for i in 0..8 {
            add(
                &mut db,
                chat,
                item(
                    None,
                    user(
                        &format!("{i}{}", "界".repeat(3000)),
                        DeliveryState::NativeDelivered,
                    ),
                ),
            );
        }
        let through = db.read(|r| r.head()).unwrap();
        db.read(|r| {
            let text = handover(r, chat, None, through)?;
            assert!(text.chars().count() <= 12_000);
            assert!(text.contains(&format!("7{}", "界".repeat(1999))));
            assert!(!text.contains("0界"));
            assert!(!text.contains(&"界".repeat(2000)));
            Ok(())
        })
        .unwrap();
    }
}
