// SPDX-License-Identifier: Apache-2.0
//! Live words stay in memory and are checkpointed in append-only chunks once
//! a second (or after 8 KiB). Message boundaries checkpoint in their own write.
//! Only committed watermarks consume memory, so failed writes remain retryable.
use crate::CompanionHub;
use protocol::{
    Event, RunId,
    event::EventSource,
    model::{Provenance, Run, TimelineBody, TimelineItem},
    time::Timestamp,
};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, time::Duration};

const PENDING: &str = "partial_reply";
pub(crate) const WATERMARK: &str = "partial_reply_saved";
const CHECKPOINT_BYTES: usize = 8 * 1024;

#[derive(Default)]
pub(crate) struct Runtime {
    // Keep first-seen order for recovery while allowing every helper to stream.
    buffers: HashMap<RunId, Vec<Buffer>>,
}

struct Buffer {
    generation: uuid::Uuid,
    key: String,
    text: String,
    offset: usize,
    at: Timestamp,
}

#[derive(Default, Serialize, Deserialize)]
struct Saved {
    serial: u64,
    #[serde(default)]
    messages: HashMap<String, Progress>,
}

#[derive(Serialize, Deserialize)]
struct Progress {
    // Distinguish a fresh use of a native key from an already completed one.
    generation: uuid::Uuid,
    bytes: usize,
    closed: bool,
}

#[derive(Serialize, Deserialize)]
struct Chunk {
    key: String,
    text: String,
    at: Timestamp,
    #[serde(default)]
    message_key: Option<String>,
}

fn chunks(run: RunId) -> String {
    format!("partial_reply_chunks:{run}")
}

impl Runtime {
    /// Called with the database lock held, only after a successful commit.
    pub(crate) fn committed(&mut self, read: &store::Read<'_>) -> store::Result<()> {
        for (id, buffers) in &mut self.buffers {
            let Some(saved) = read.get::<Saved>(WATERMARK, id)? else {
                continue;
            };
            buffers.retain_mut(|buffer| {
                let Some(progress) = saved.messages.get(&buffer.key) else {
                    return true;
                };
                if progress.generation != buffer.generation {
                    return true;
                }
                let used = progress
                    .bytes
                    .saturating_sub(buffer.offset)
                    .min(buffer.text.len());
                buffer.text.drain(..used);
                buffer.offset += used;
                !(progress.closed && buffer.text.is_empty())
            });
        }
        self.buffers.retain(|_, buffers| !buffers.is_empty());
        Ok(())
    }
}

impl CompanionHub {
    /// No database lock or disk write on the live path for ordinary pieces.
    pub(crate) fn buffer_reply(&self, id: RunId, key: &str, text: &str) -> (bool, String, u64) {
        let mut runtime = self.inner.partial_replies.lock().unwrap();
        let buffers = runtime.buffers.entry(id).or_default();
        let index = match buffers.iter().position(|b| b.key == key) {
            Some(index) => index,
            None => {
                buffers.push(Buffer {
                    generation: uuid::Uuid::now_v7(),
                    key: key.into(),
                    text: String::new(),
                    offset: 0,
                    at: self.inner.clock.now(),
                });
                buffers.len() - 1
            }
        };
        let buffer = &mut buffers[index];
        if buffer.text.is_empty() {
            buffer.at = self.inner.clock.now();
        }
        let offset = (buffer.offset + buffer.text.len()) as u64;
        let message_key = buffer.generation.to_string();
        buffer.text.push_str(text);
        // A helper's key is not a boundary for its parent or other helpers.
        // If a native final never arrives, that key stays pending until a real
        // interruption or run end, rather than being split by another key.
        (
            buffers.iter().map(|b| b.text.len()).sum::<usize>() >= CHECKPOINT_BYTES,
            message_key,
            offset,
        )
    }

    pub(crate) fn checkpoint_reply(
        &self,
        tx: &mut store::Write<'_>,
        run: &Run,
    ) -> store::Result<()> {
        self.checkpoint_reply_key(tx, run, None)
    }

    fn checkpoint_reply_key(
        &self,
        tx: &mut store::Write<'_>,
        run: &Run,
        key: Option<&str>,
    ) -> store::Result<()> {
        let runtime = self.inner.partial_replies.lock().unwrap();
        let Some(buffers) = runtime.buffers.get(&run.run_id) else {
            return Ok(());
        };
        let mut saved: Saved = tx.read().get(WATERMARK, run.run_id)?.unwrap_or_default();
        let mut changed = false;
        for buffer in buffers
            .iter()
            .filter(|b| !b.text.is_empty() && key.is_none_or(|key| key == b.key))
        {
            let used = saved
                .messages
                .get(&buffer.key)
                .filter(|p| p.generation == buffer.generation)
                .map_or(0, |p| {
                    p.bytes.saturating_sub(buffer.offset).min(buffer.text.len())
                });
            changed = true;
            if used == buffer.text.len() {
                // Retry acknowledgement after an already successful commit,
                // without appending the same bytes again.
                continue;
            }
            saved.serial += 1;
            tx.put(
                &chunks(run.run_id),
                format!("{:020}", saved.serial),
                &Chunk {
                    key: buffer.key.clone(),
                    text: buffer.text[used..].into(),
                    at: buffer.at,
                    message_key: Some(buffer.generation.to_string()),
                },
            )?;
            saved.messages.insert(
                buffer.key.clone(),
                Progress {
                    generation: buffer.generation,
                    bytes: buffer.offset + buffer.text.len(),
                    closed: false,
                },
            );
        }
        if changed {
            tx.put(WATERMARK, run.run_id, &saved)?;
        }
        Ok(())
    }

    pub(crate) fn partial_boundary(
        &self,
        tx: &mut store::Write<'_>,
        run: &Run,
    ) -> store::Result<()> {
        self.checkpoint_reply(tx, run)?;
        boundary(tx, run)
    }

    pub(crate) fn complete_reply(
        &self,
        tx: &mut store::Write<'_>,
        run: &Run,
        key: &str,
        text: String,
    ) -> store::Result<Completed> {
        self.checkpoint_reply_key(tx, run, Some(key))?;
        complete(tx, run, key, text)
    }

    pub(crate) fn finish_reply(&self, tx: &mut store::Write<'_>, run: &Run) -> store::Result<()> {
        self.checkpoint_reply(tx, run)?;
        finish(tx, run)
    }

    /// Blocking filesystem work runs off the async executor. Each run gets at
    /// most one scheduled checkpoint per second; empty buffers take no write.
    pub(crate) async fn save_replies(&self) -> Result<(), protocol::ProtocolError> {
        let ids: Vec<_> = self
            .inner
            .partial_replies
            .lock()
            .unwrap()
            .buffers
            .iter()
            .filter(|(_, buffers)| buffers.iter().any(|b| !b.text.is_empty()))
            .map(|(id, _)| *id)
            .collect();
        self.save_reply_ids(ids).await
    }

    pub(crate) async fn save_reply(&self, id: RunId) -> Result<(), protocol::ProtocolError> {
        self.save_reply_ids(vec![id]).await
    }

    pub(crate) async fn save_reply_ids(
        &self,
        ids: Vec<RunId>,
    ) -> Result<(), protocol::ProtocolError> {
        if ids.is_empty() {
            return Ok(());
        }
        let hub = self.clone();
        tokio::task::spawn_blocking(move || {
            let mut first_error = None;
            for id in ids {
                if let Err(error) = hub.write(|tx| {
                    let run = tx.read().require("run", id)?;
                    hub.checkpoint_reply(tx, &run)
                }) {
                    first_error.get_or_insert(error);
                }
            }
            first_error.map_or(Ok(()), Err)
        })
        .await
        .map_err(|_| crate::internal("reply checkpoint worker stopped"))?
    }

    pub(crate) fn start_reply_checkpoints(&self) {
        let weak = std::sync::Arc::downgrade(&self.inner);
        let mut shutdown = self.inner.shutdown.subscribe();
        self.spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(1));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            tick.tick().await;
            loop {
                tokio::select! {
                    _ = shutdown.changed() => break,
                    _ = tick.tick() => {
                        let Some(inner) = weak.upgrade() else { break };
                        if let Err(e) = (CompanionHub { inner }).save_replies().await {
                            tracing::error!(code=?e.code, "reply checkpoint failed; keeping unsaved words");
                        }
                    }
                }
            }
        });
    }
}

#[derive(Default, Serialize, Deserialize)]
struct Reply {
    messages: Vec<Message>,
}

#[derive(Serialize, Deserialize)]
struct Message {
    key: String,
    text: String,
    shown: String,
    at: Timestamp,
    #[serde(default)]
    message_key: Option<String>,
}

/// Only boundaries materialize all chunks. Routine checkpoints append their
/// unsaved suffix and a small watermark, never rewrite the preceding text.
fn take(tx: &mut store::Write<'_>, run: &Run, key: Option<&str>) -> store::Result<Reply> {
    let mut reply: Reply = tx.read().get(PENDING, run.run_id)?.unwrap_or_default();
    let kind = chunks(run.run_id);
    for (id, chunk) in tx.read().entries::<Chunk>(&kind)? {
        if key.is_some_and(|key| key != chunk.key) {
            continue;
        }
        if let Some(message) = reply.messages.iter_mut().find(|m| m.key == chunk.key) {
            if message.text.is_empty() {
                message.at = chunk.at;
            }
            message.text.push_str(&chunk.text);
        } else {
            reply.messages.push(Message {
                key: chunk.key,
                text: chunk.text,
                shown: String::new(),
                at: chunk.at,
                message_key: chunk.message_key,
            });
        }
        tx.delete(&kind, id)?;
    }
    Ok(reply)
}

fn boundary(tx: &mut store::Write<'_>, run: &Run) -> store::Result<()> {
    let mut reply = take(tx, run, None)?;
    if reply.messages.is_empty() {
        return Ok(());
    }
    flush(tx, run, &mut reply)?;
    tx.put(PENDING, run.run_id, &reply)
}

fn close(tx: &mut store::Write<'_>, id: RunId, key: Option<&str>) -> store::Result<()> {
    if let Some(mut saved) = tx.read().get::<Saved>(WATERMARK, id)? {
        for (message_key, progress) in &mut saved.messages {
            if key.is_none_or(|key| key == message_key) {
                progress.closed = true;
            }
        }
        tx.put(WATERMARK, id, &saved)?;
    }
    Ok(())
}

pub(crate) struct Completed {
    pub text: String,
    pub message_key: Option<String>,
    pub text_offset: u64,
}

fn complete(
    tx: &mut store::Write<'_>,
    run: &Run,
    key: &str,
    text: String,
) -> store::Result<Completed> {
    let mut reply = take(tx, run, Some(key))?;
    let mut message_key = None;
    let mut text_offset = 0;
    let text = if let Some(index) = reply.messages.iter().position(|m| m.key == key) {
        let message = reply.messages.remove(index);
        message_key = message.message_key;
        if let Some(rest) = text.strip_prefix(&message.shown) {
            text_offset = message.shown.len() as u64;
            rest.to_owned()
        } else if message.shown.starts_with(&text) {
            text_offset = message.shown.len() as u64;
            String::new()
        } else {
            text
        }
    } else {
        text
    };
    // A helper's final message must not split another still-streaming message.
    if reply.messages.is_empty() {
        tx.delete(PENDING, run.run_id)?;
    } else {
        tx.put(PENDING, run.run_id, &reply)?;
    }
    close(tx, run.run_id, Some(key))?;
    Ok(Completed {
        text,
        message_key,
        text_offset,
    })
}

/// Startup has no in-memory tail. Runtime callers use `finish_reply` above.
pub(crate) fn finish(tx: &mut store::Write<'_>, run: &Run) -> store::Result<()> {
    let mut reply = take(tx, run, None)?;
    flush(tx, run, &mut reply)?;
    tx.delete(PENDING, run.run_id)?;
    close(tx, run.run_id, None)
}

fn flush(tx: &mut store::Write<'_>, run: &Run, reply: &mut Reply) -> store::Result<()> {
    for message in &mut reply.messages {
        if message.text.is_empty() {
            continue;
        }
        // Every UTF-8 byte needs at most six JSON bytes, even control characters.
        let mut rest = message.text.as_str();
        let mut offset = message.shown.len() as u64;
        while !rest.is_empty() {
            let mut end = rest.len().min(crate::native_text::MESSAGE / 6);
            while !rest.is_char_boundary(end) {
                end -= 1;
            }
            tx.emit_from(
                Event::TimelineAppended {
                    task_id: run.task_id,
                    item: TimelineItem {
                        seq: 0,
                        at: message.at,
                        run_id: Some(run.run_id),
                        item: TimelineBody::AgentMessage {
                            text: rest[..end].into(),
                            partial: true,
                            message_key: message.message_key.clone(),
                            text_offset: message.message_key.as_ref().map(|_| offset),
                        },
                    },
                },
                EventSource::Harness {
                    harness: run.harness,
                },
                Provenance::Native,
            )?;
            offset += end as u64;
            rest = &rest[end..];
        }
        message.shown.push_str(&message.text);
        message.text.clear();
    }
    Ok(())
}
