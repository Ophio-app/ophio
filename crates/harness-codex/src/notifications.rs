// SPDX-License-Identifier: Apache-2.0
use crate::{
    Actor,
    wire::{Id, Turn, Usage},
};
use ports::{HarnessError, HarnessEvent, RunExit};
use protocol::model::{ActivityStatus, ChildState, UsageSummary};
use serde_json::Value;

fn text(value: &Value, key: &str) -> String {
    value[key].as_str().unwrap_or_default().into()
}
/// A Codex warning as the person reads it. The one about the code-mode
/// helper names its consequence and what to do.
fn plain_warning(message: &str) -> String {
    if message.contains("codex-code-mode-host") {
        "Codex can’t run commands in this task: codex-code-mode-host, the helper it runs them through, is missing beside Codex on the computer. Reinstall Codex, or choose a complete installation.".into()
    } else {
        format!("Codex warns: {message}")
    }
}
/// Part of something still under way, which Codex sends again whole once
/// it is done: a delta, or progress.
fn streamed(method: &str) -> bool {
    method.ends_with("Delta") || method.ends_with("/delta") || method.ends_with("/progress")
}
impl Actor {
    pub(crate) fn notification(
        &mut self,
        method: &str,
        params: &Value,
    ) -> Result<bool, HarnessError> {
        // The app server also reports child threads. Their status is handled
        // through native collab items; never let them replace the parent turn.
        if let Some(thread) = params["threadId"].as_str()
            && self.thread.as_deref() != Some(thread)
        {
            return Ok(false);
        }
        self.results.observe(method, params);
        if method == "item/completed" {
            self.emit(HarnessEvent::Results {
                outcome: self.results.outcome(),
            })?;
        }
        match method {
            "turn/started" => {
                let turn: Turn = serde_json::from_value(params["turn"].clone())
                    .map_err(|_| HarnessError::Protocol("invalid turn notification".into()))?;
                self.turn = Some(turn.id);
                self.turn_exit = None;
                self.usage_limited = false;
                self.emit(HarnessEvent::TurnStarted)?;
            }
            "turn/completed" => {
                let turn: Turn = serde_json::from_value(params["turn"].clone())
                    .map_err(|_| HarnessError::Protocol("invalid turn notification".into()))?;
                if self.turn.as_deref().is_some_and(|id| id != turn.id) {
                    return Err(HarnessError::Protocol(
                        "completion belongs to another turn".into(),
                    ));
                }
                if self.finished_turns.contains(&turn.id) {
                    return Ok(false);
                }
                self.finished_turns.push_back(turn.id.clone());
                if self.finished_turns.len() > 64 {
                    self.finished_turns.pop_front();
                }
                self.turn = None;
                self.file_changes.clear();
                if turn.status == "failed" {
                    self.usage_limit(&params["turn"]["error"])?;
                }
                // Keep the in-flight start fenced until its response arrives.
                self.starting = self
                    .pending
                    .values()
                    .any(|(_, p)| matches!(p, crate::Pending::Turn(..)));
                self.turn_exit = Some(match turn.status.as_str() {
                    "completed" => RunExit::Completed,
                    "interrupted" => RunExit::Interrupted,
                    "failed" => RunExit::Failed {
                        reason: params["turn"]["error"]["message"]
                            .as_str()
                            .unwrap_or("Codex ended this turn with an error")
                            .to_owned(),
                    },
                    _ => RunExit::Unknown {
                        reason: "Codex ended this turn without saying whether it succeeded".into(),
                    },
                });
                for (_, approval) in self.approvals.drain() {
                    self.events
                        .try_send(HarnessEvent::ApprovalClosed {
                            native_request_id: approval.id.key(),
                            outcome: None,
                        })
                        .map_err(|_| {
                            HarnessError::Protocol("approval receiver unavailable".into())
                        })?;
                }
                self.emit(HarnessEvent::TurnCompleted {
                    usage: self.usage.clone(),
                })?;
                let unsuccessful = match self.turn_exit {
                    Some(RunExit::Failed { .. }) => Some("Codex ended this turn with an error"),
                    Some(RunExit::Unknown { .. }) => {
                        Some("Codex ended this turn without saying whether it succeeded")
                    }
                    _ => None,
                };
                if let Some(text) = unsuccessful {
                    self.emit(HarnessEvent::Warning { text: text.into() })?;
                }
                return Ok(true);
            }
            "thread/tokenUsage/updated" => {
                let usage: Usage = serde_json::from_value(params["tokenUsage"]["last"].clone())
                    .map_err(|_| {
                        HarnessError::Protocol("invalid token usage notification".into())
                    })?;
                // A count the phone cannot represent is not reported.
                let count = |n: u64| Some(n).filter(|n| *n <= protocol::MAX_SAFE_INTEGER);
                self.usage = Some(UsageSummary {
                    input_tokens: count(usage.input_tokens),
                    output_tokens: count(usage.output_tokens),
                    duration_ms: None,
                    reported_cost: None,
                });
            }
            "item/agentMessage/delta" => self.emit(HarnessEvent::AgentTextDelta {
                message_key: text(params, "itemId"),
                text: text(params, "delta"),
            })?,
            "item/started" | "item/completed" => {
                let item = &params["item"];
                let kind = item["type"].as_str().unwrap_or_default();
                let completed = method == "item/completed";
                if self.computer.is_some()
                    && let Some(bridge) = &self.spec.bridge
                    && !crate::computer::allowed(item, &bridge.server_name)
                {
                    // Whatever let it through, the run ends here.
                    let reason = crate::computer::stopped(item);
                    self.turn_exit = Some(RunExit::Failed {
                        reason: reason.clone(),
                    });
                    return Err(HarnessError::Unsupported(reason));
                }
                if kind == "fileChange" {
                    let id = text(item, "id");
                    if completed {
                        self.file_changes.remove(&id);
                    } else if self.file_changes.len() < 64 {
                        self.file_changes
                            .insert(id, crate::approval::planned_changes(item));
                    }
                }
                match kind {
                    "agentMessage" if completed => self.emit(HarnessEvent::AgentMessage {
                        message_key: text(item, "id"),
                        text: text(item, "text"),
                    })?,
                    "agentMessage" | "userMessage" | "reasoning" | "plan" => {}
                    "collabAgentToolCall" => {
                        if let Some(agents) = item["agentsStates"].as_object() {
                            for (id, state) in agents {
                                let state = match state["status"].as_str() {
                                    Some("running") | Some("pendingInit") => ChildState::Active,
                                    Some("completed") => ChildState::Completed,
                                    Some("errored") => ChildState::Failed,
                                    Some("shutdown") => ChildState::Stopped,
                                    _ => ChildState::Unknown,
                                };
                                self.emit(HarnessEvent::Child {
                                    lifetime: None,
                                    native_ref: id.clone(),
                                    label: text(item, "tool"),
                                    state,
                                })?;
                            }
                        }
                        self.activity(item, kind, completed)?;
                    }
                    "subAgentActivity" => self.emit(HarnessEvent::Child {
                        lifetime: None,
                        native_ref: text(item, "agentThreadId"),
                        label: text(item, "agentPath"),
                        state: ChildState::Unknown,
                    })?,
                    "commandExecution" | "fileChange" | "mcpToolCall" | "dynamicToolCall"
                    | "webSearch" | "imageView" | "imageGeneration" | "functionCallOutput" => {
                        self.activity(item, kind, completed)?
                    }
                    other => {
                        if self.unknown.insert(format!("item {other}")) {
                            self.emit(HarnessEvent::Warning {
                                text: format!(
                                    "Codex reported a step this version does not read ({other})"
                                ),
                            })?;
                        }
                    }
                }
            }
            "serverRequest/resolved" => {
                let id: Id = serde_json::from_value(params["requestId"].clone())
                    .map_err(|_| HarnessError::Protocol("invalid resolved request id".into()))?;
                if self.approvals.remove(&id.key()).is_some() {
                    // The verified v2 notification carries no decision. Our own
                    // responses already removed the request; only unsolicited
                    // resolution/cleanup should be reported as closed elsewhere.
                    self.emit(HarnessEvent::ApprovalClosed {
                        native_request_id: id.key(),
                        outcome: None,
                    })?;
                }
            }
            "error" => {
                if !self.usage_limit(&params["error"])? {
                    // Keep Codex's own words; they usually say what went wrong.
                    let said = params["error"]["message"]
                        .as_str()
                        .map(str::trim)
                        .filter(|m| !m.is_empty());
                    let again = params["willRetry"] == true;
                    self.emit(HarnessEvent::Warning {
                        text: match (said, again) {
                            (Some(m), true) => {
                                format!("Codex hit an error and is trying again: {m}")
                            }
                            (Some(m), false) => format!("Codex reported an error: {m}"),
                            (None, true) => "Codex hit an error and is trying again".into(),
                            (None, false) => "Codex reported an error".into(),
                        },
                    })?;
                }
            }
            "account/rateLimits/updated" => self.rate_limits.update(&params["rateLimits"]),
            "configWarning" => {
                if let Some(reason) = crate::readiness::sandbox_warning(params) {
                    *self
                        .readiness_warning
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(reason.clone());
                    self.emit(HarnessEvent::ReadinessWarning { reason })?;
                }
                let summary = text(params, "summary");
                if self.unknown.insert(format!("config {summary}")) {
                    self.emit(HarnessEvent::Warning {
                        text: format!("Codex reports a configuration problem: {summary}"),
                    })?;
                }
            }
            "warning" => {
                let message: String = text(params, "message")
                    .chars()
                    .filter(|c| !c.is_control())
                    .take(1000)
                    .collect();
                if !message.is_empty() && self.unknown.insert(format!("warning {message}")) {
                    self.emit(HarnessEvent::Warning {
                        text: plain_warning(&message),
                    })?;
                }
            }
            "deprecationNotice" => {
                let notice: String = [text(params, "summary"), text(params, "details")]
                    .join(" ")
                    .chars()
                    .filter(|c| !c.is_control())
                    .take(1000)
                    .collect();
                let notice = notice.trim();
                if !notice.is_empty() && self.unknown.insert(format!("deprecation {notice}")) {
                    self.emit(HarnessEvent::Warning {
                        text: format!("Codex notes: {notice}"),
                    })?;
                }
            }
            // Nothing in the app follows Codex's thread goals.
            "thread/goal/updated" | "thread/goal/cleared" => {}
            "thread/started"
            | "thread/status/changed"
            | "thread/name/updated"
            | "thread/settings/updated"
            | "thread/compacted"
            | "remoteControl/status/changed"
            | "item/reasoning/summaryTextDelta"
            | "item/reasoning/textDelta"
            | "item/reasoning/summaryPartAdded"
            | "item/fileChange/patchUpdated"
            | "item/commandExecution/terminalInteraction"
            | "turn/diff/updated"
            | "turn/plan/updated"
            | "hook/started"
            | "hook/completed"
            | "mcpServer/startupStatus/updated"
            | "account/updated" => {}
            // A running step's output as it streams, such as a command's
            // (`item/commandExecution/outputDelta`, from Codex 0.16x). The
            // step's finished item carries all of it.
            other if streamed(other) => {}
            other => {
                // Newer app servers add notifications; say so once per kind
                // rather than on every occurrence.
                if self.unknown.insert(other.to_owned()) {
                    self.emit(HarnessEvent::Warning {
                        text: format!(
                            "Codex sent an update Ophio doesn't know yet ({other}). The task carries on; an Ophio update will read it."
                        ),
                    })?;
                }
            }
        }
        Ok(false)
    }
    /// Report a refusal for usage once per turn, with the reset time of the
    /// exhausted rate-limit windows. Returns whether `error` was one.
    fn usage_limit(&mut self, error: &Value) -> Result<bool, HarnessError> {
        if !crate::limits::usage_limit_exceeded(error) {
            return Ok(false);
        }
        if !std::mem::replace(&mut self.usage_limited, true) {
            self.emit(HarnessEvent::Warning {
                text: "Codex reports the usage limit is reached".into(),
            })?;
            self.emit(HarnessEvent::UsageLimited {
                resets_at: self.rate_limits.resets_at(),
            })?;
        }
        Ok(true)
    }
    fn activity(&self, item: &Value, kind: &str, completed: bool) -> Result<(), HarnessError> {
        let status = match item["status"].as_str() {
            Some("failed") | Some("declined") => ActivityStatus::Failed,
            Some("completed") => ActivityStatus::Succeeded,
            Some("inProgress") => ActivityStatus::Running,
            _ if !completed => ActivityStatus::Running,
            _ => ActivityStatus::Unknown,
        };
        self.emit(HarnessEvent::Activity {
            key: text(item, "id"),
            tool: item["tool"].as_str().unwrap_or(kind).into(),
            summary: kind.into(),
            status,
            detail: item["command"]
                .as_str()
                .map(
                    |command| match item["aggregatedOutput"].as_str().filter(|s| !s.is_empty()) {
                        Some(output) => format!("{command}\n{output}"),
                        None => command.to_owned(),
                    },
                )
                .or_else(|| {
                    if kind == "fileChange" || kind == "collabAgentToolCall" {
                        Some(item.to_string())
                    } else {
                        item.get("arguments").map(Value::to_string)
                    }
                }),
        })
    }
}
