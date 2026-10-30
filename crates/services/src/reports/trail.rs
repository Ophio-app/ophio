// SPDX-License-Identifier: Apache-2.0
use protocol::{Event, EventEnvelope, model::ApprovalState, reports::*, time::Timestamp};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

#[derive(Debug)]
pub(crate) struct Recorder {
    path: PathBuf,
    events: Vec<TrailEvent>,
    states: HashMap<String, String>,
    flushed: Instant,
    dirty: bool,
}
impl Recorder {
    pub fn load(data: &Path, now: Timestamp) -> Self {
        let path = data.join("report-trail.json");
        let events = std::fs::read(&path)
            .ok()
            .filter(|b| b.len() <= 512 * 1024)
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        let mut this = Self {
            path,
            events,
            states: HashMap::new(),
            flushed: Instant::now(),
            dirty: true,
        };
        this.evict(now);
        this
    }
    pub fn push(&mut self, at: Timestamp, event: Trail) {
        let event = clean(event);
        self.events.push(TrailEvent { at, event });
        self.dirty = true;
        self.evict(at);
    }
    pub fn evict(&mut self, now: Timestamp) {
        let before = self.events.len();
        self.events
            .retain(|e| e.at.0 >= now.0.saturating_sub(COMPUTER_TRAIL_MS) && e.at <= now);
        let excess = self.events.len().saturating_sub(MAX_TRAIL_EVENTS);
        self.events.drain(..excess);
        self.dirty |= self.events.len() != before;
    }
    pub fn flush(&mut self, force: bool) {
        if !self.dirty || (!force && self.flushed.elapsed() < Duration::from_secs(2)) {
            return;
        }
        if let Ok(bytes) = serde_json::to_vec(&self.events)
            && super::private_write(&self.path, &bytes).is_ok()
        {
            self.dirty = false;
            self.flushed = Instant::now();
        }
    }
    pub fn preview(&mut self, salt: &str, now: Timestamp) -> Vec<TrailEvent> {
        self.evict(now);
        self.events
            .iter()
            .cloned()
            .map(|mut e| {
                match &mut e.event {
                    Trail::TaskState { task, .. } | Trail::SandboxBlock { task, .. } => {
                        *task = pseudonym(salt, PseudonymKind::Task, task)
                    }
                    Trail::RunExit { task, run, .. } => {
                        *task = pseudonym(salt, PseudonymKind::Task, task);
                        *run = pseudonym(salt, PseudonymKind::Run, run);
                    }
                    Trail::Approval { task, approval, .. } => {
                        *task = pseudonym(salt, PseudonymKind::Task, task);
                        *approval = pseudonym(salt, PseudonymKind::Approval, approval);
                    }
                    _ => {}
                }
                e
            })
            .collect()
    }
    pub fn tasks(&self) -> Vec<String> {
        self.events
            .iter()
            .filter_map(|e| match &e.event {
                Trail::TaskState { task, .. }
                | Trail::SandboxBlock { task, .. }
                | Trail::RunExit { task, .. }
                | Trail::Approval { task, .. } => Some(task.clone()),
                _ => None,
            })
            .collect()
    }
    fn changed(&mut self, key: String, value: String) -> bool {
        if self.states.get(&key) == Some(&value) {
            return false;
        }
        // The trail is bounded; the de-duplication cache must be too.
        if self.states.len() >= MAX_TRAIL_EVENTS * 4 {
            self.states.clear();
        }
        self.states.insert(key, value);
        true
    }
    pub fn observe(&mut self, envelope: &EventEnvelope) {
        let at = envelope.at;
        match &envelope.event {
            Event::TaskUpserted { task } => {
                let state = wire(&task.state);
                if self.changed(format!("task:{}", task.task_id), state.clone()) {
                    self.push(
                        at,
                        Trail::TaskState {
                            task: task.task_id.to_string(),
                            state,
                            harness: Some(task.harness),
                            model: task.model.clone(),
                        },
                    );
                }
                if let Some(block) = &task.access_blocked
                    && self.changed(format!("block:{}", task.task_id), block.at.0.to_string())
                {
                    self.push(
                        at,
                        Trail::SandboxBlock {
                            task: task.task_id.to_string(),
                            kind: if block.host.is_some() {
                                BlockKind::Network
                            } else {
                                BlockKind::Path
                            },
                        },
                    );
                }
            }
            Event::RunUpserted { run } => {
                let state = wire(&run.state);
                if self.changed(format!("run:{}", run.run_id), state.clone()) {
                    // The contract's run event carries both transitions and exits.
                    self.push(
                        at,
                        Trail::RunExit {
                            task: run.task_id.to_string(),
                            run: run.run_id.to_string(),
                            exit: state,
                            code: None,
                        },
                    );
                }
            }
            Event::ApprovalUpserted { approval } => {
                let state = wire(&approval.state);
                if self.changed(format!("approval:{}", approval.approval_id), state) {
                    let step = match approval.state {
                        ApprovalState::Pending => ApprovalStep::Asked,
                        ApprovalState::Allowed => ApprovalStep::Allowed,
                        ApprovalState::Denied => ApprovalStep::Denied,
                        ApprovalState::Expired => ApprovalStep::Expired,
                        _ => ApprovalStep::Answered,
                    };
                    self.push(
                        at,
                        Trail::Approval {
                            task: approval.task_id.to_string(),
                            approval: approval.approval_id.to_string(),
                            step,
                        },
                    );
                }
            }
            Event::AttentionUpserted { item }
                if item.resolved_at.is_none() && item.kind.alerts() =>
            {
                if self.changed(format!("alert:{}", item.attention_id), "raised".into()) {
                    self.push(
                        at,
                        Trail::AlertRaised {
                            topic: item.topic(),
                            quiet: item.quiet,
                        },
                    );
                }
            }
            Event::CommandProgress {
                error: Some(error), ..
            } => self.push(
                at,
                Trail::Error {
                    error: ReportedError {
                        code: wire(&error.code),
                        message: None,
                    },
                },
            ),
            _ => {}
        }
    }
}
pub(crate) fn wire(value: &impl serde::Serialize) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}
fn code(value: String) -> String {
    scrub(&value).unwrap_or_default()
}
fn clean(event: Trail) -> Trail {
    match event {
        Trail::TaskState {
            task,
            state,
            harness,
            model,
        } => Trail::TaskState {
            task,
            state: code(state),
            harness,
            model: model
                .filter(|m| {
                    m.len() <= MAX_REPORT_CODE
                        && m.bytes()
                            .all(|c| c.is_ascii_alphanumeric() || b"-._[]".contains(&c))
                })
                .map(code),
        },
        Trail::RunExit {
            task,
            run,
            exit,
            code: c,
        } => Trail::RunExit {
            task,
            run,
            exit: code(exit),
            code: c.map(code),
        },
        Trail::Github { operation, status } => Trail::Github {
            operation: code(operation),
            status,
        },
        Trail::Update { from, to, outcome } => Trail::Update {
            from: build_id(&from).unwrap_or_default(),
            to: build_id(&to).unwrap_or_default(),
            outcome,
        },
        Trail::Error { error } => Trail::Error {
            error: ReportedError {
                code: code(error.code),
                message: error.message.as_deref().and_then(scrub),
            },
        },
        Trail::Crash { code: c } => Trail::Crash { code: code(c) },
        other => other,
    }
}
