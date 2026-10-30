// SPDX-License-Identifier: Apache-2.0
use ports::HarnessEvent;
use protocol::model::{ActivityStatus, ChildState, TaskMode, UsageSummary};
use serde_json::Value;
use std::collections::HashMap;

#[derive(Default)]
pub(crate) struct StreamState {
    pub completed: bool,
    pub failed: bool,
    /// Stop was sent this turn, so its unsuccessful end is the one asked for.
    pub interrupted: bool,
    plan_reply: bool,
    message_keys: HashMap<String, String>,
    tools: HashMap<String, String>,
    /// Helpers started by a Task/Agent call still waiting for its result.
    helpers: HashMap<String, String>,
    unknown: std::collections::HashSet<String>,
    limited: Option<String>,
    results: crate::results::Results,
}
fn string(value: &Value, key: &str) -> String {
    value[key].as_str().unwrap_or_default().to_owned()
}
fn warning(text: &str) -> HarnessEvent {
    HarnessEvent::Warning { text: text.into() }
}
/// Why Claude Code ended a turn that did not succeed, in words a person can
/// act on rather than its status code.
fn unsuccessful(value: &Value) -> &'static str {
    match value["subtype"].as_str() {
        Some("error_max_turns") => "Claude Code stopped this turn at its turn limit",
        Some("error_max_budget_usd") => "Claude Code stopped this turn at its spending limit",
        _ => "Claude Code ended this turn with an error",
    }
}
/// A count the phone can represent; any other reads as not reported.
fn count(value: &Value) -> Option<u64> {
    value.as_u64().filter(|n| *n <= protocol::MAX_SAFE_INTEGER)
}
impl StreamState {
    pub fn expected_plan_callback(&self, value: &Value) -> bool {
        self.plan_reply
            && value["request"]["subtype"] == "can_use_tool"
            && value["request"]["tool_name"] == "ExitPlanMode"
    }
    pub fn new(mode: TaskMode) -> Self {
        Self {
            plan_reply: mode == TaskMode::Plan,
            ..Self::default()
        }
    }
    pub fn parse(&mut self, value: &Value) -> Vec<HarnessEvent> {
        self.results.observe(value);
        let mut out = Vec::new();
        if let Some(parent) = value["parent_tool_use_id"]
            .as_str()
            .filter(|id| !id.is_empty())
        {
            // A helper's own lines name the Task/Agent call still waiting for
            // it. A long call's progress names that call, and a backgrounded
            // helper writes on after its call returned: neither is a helper
            // still running.
            if let Some(label) = self.helpers.get(parent).cloned() {
                out.push(HarnessEvent::Child {
                    lifetime: Some(protocol::model::ChildLifetime::InProcess),
                    native_ref: parent.into(),
                    label: label.clone(),
                    state: ChildState::Active,
                });
                if value["type"] == "result" {
                    out.push(HarnessEvent::Child {
                        lifetime: Some(protocol::model::ChildLifetime::InProcess),
                        native_ref: parent.into(),
                        label,
                        state: if value["is_error"] == false && value["subtype"] == "success" {
                            ChildState::Completed
                        } else {
                            ChildState::Unknown
                        },
                    });
                }
            }
            if value["type"] == "result" {
                return out;
            }
            // A child's session id must never replace the parent run identity.
            if value["type"] == "system" && value["subtype"] == "init" {
                return out;
            }
        }
        match value["type"].as_str().unwrap_or_default() {
            "system" if value["subtype"] == "init" => {
                if let Some(id) = value["session_id"].as_str() {
                    out.push(HarnessEvent::SessionStarted {
                        native_session: id.into(),
                        actual_model: value["model"].as_str().map(str::to_owned),
                        actual_effort: value["effort"].as_str().map(str::to_owned),
                        account_label: None,
                    });
                } else {
                    out.push(warning("Claude Code started without reporting its session"));
                }
            }
            "system"
                if value["subtype"] == "task_started"
                    || value["subtype"] == "task_notification" =>
            {
                let state = match value["status"].as_str() {
                    Some("completed") => ChildState::Completed,
                    Some("failed") => ChildState::Failed,
                    Some("stopped") => ChildState::Stopped,
                    _ if value["subtype"] == "task_started" => ChildState::Active,
                    _ => ChildState::Unknown,
                };
                out.push(HarnessEvent::Child {
                    lifetime: crate::background::task_lifetime(value),
                    native_ref: string(value, "task_id"),
                    label: value["description"]
                        .as_str()
                        .unwrap_or("Background task")
                        .into(),
                    state,
                });
            }
            "stream_event" => {
                let event = &value["event"];
                match event["type"].as_str().unwrap_or_default() {
                    "message_start" => {
                        let route = value["parent_tool_use_id"].as_str().unwrap_or_default();
                        self.message_keys
                            .insert(route.into(), string(&event["message"], "id"));
                        out.push(HarnessEvent::TurnStarted);
                    }
                    "content_block_delta" if event["delta"]["type"] == "text_delta" => {
                        out.push(HarnessEvent::AgentTextDelta {
                            message_key: self
                                .message_keys
                                .get(value["parent_tool_use_id"].as_str().unwrap_or_default())
                                .cloned()
                                .unwrap_or_default(),
                            text: string(&event["delta"], "text"),
                        })
                    }
                    "content_block_start"
                    | "content_block_delta"
                    | "content_block_stop"
                    | "message_delta"
                    | "message_stop"
                    | "ping" => {}
                    _ => out.push(warning("Claude Code sent an update this app does not recognize, so it is not shown")),
                }
            }
            "assistant" => {
                let message = &value["message"];
                let key = message["id"]
                    .as_str()
                    .or_else(|| value["uuid"].as_str())
                    .unwrap_or_default()
                    .to_owned();
                if let Some(content) = message["content"].as_array() {
                    let text = content
                        .iter()
                        .filter(|b| b["type"] == "text")
                        .filter_map(|b| b["text"].as_str())
                        .collect::<Vec<_>>()
                        .join("");
                    // Claude Code wraps an API error, a usage refusal among them,
                    // as an assistant message. It is not the agent's reply and
                    // proves nothing about usage, so it stays a note.
                    if value["is_api_error_message"] == true && !text.is_empty() {
                        out.push(HarnessEvent::Warning { text });
                    } else if !text.is_empty() {
                        out.push(HarnessEvent::AgentMessage {
                            message_key: key,
                            text,
                        });
                    }
                    for block in content.iter().filter(|b| b["type"] == "tool_use") {
                        let key = string(block, "id");
                        let tool = string(block, "name");
                        if self.plan_reply
                            && tool == "ExitPlanMode"
                            && let Some(plan) = block["input"]["plan"]
                                .as_str()
                                .filter(|s| !s.trim().is_empty())
                        {
                            // Preserve a plan supplied as a tool argument as reply
                            // prose too. This never grants the tool permission.
                            out.push(HarnessEvent::AgentMessage {
                                message_key: key.clone(),
                                text: plan.to_owned(),
                            });
                        }
                        self.tools.insert(key.clone(), tool.clone());
                        out.push(HarnessEvent::Activity {
                            key: key.clone(),
                            tool: tool.clone(),
                            summary: if self.plan_reply && tool == "ExitPlanMode" {
                                "Plan proposed".into()
                            } else {
                                tool.clone()
                            },
                            status: if self.plan_reply && tool == "ExitPlanMode" {
                                ActivityStatus::Succeeded
                            } else {
                                ActivityStatus::Running
                            },
                            detail: if self.plan_reply && tool == "ExitPlanMode" {
                                block["input"]["plan"].as_str().map(str::to_owned)
                            } else {
                                Some(block["input"].to_string())
                            },
                        });
                        if matches!(tool.as_str(), "Task" | "Agent") {
                            let label: String = block["input"]["description"]
                                .as_str()
                                .unwrap_or("Helper task")
                                .into();
                            self.helpers.insert(key.clone(), label.clone());
                            out.push(HarnessEvent::Child {
                                lifetime: Some(protocol::model::ChildLifetime::InProcess),
                                native_ref: key,
                                label,
                                state: ChildState::Active,
                            });
                        }
                    }
                } else {
                    out.push(warning("Claude Code sent a reply with nothing in it"));
                }
            }
            "user" => {
                if let Some(content) = value["message"]["content"].as_array() {
                    for block in content.iter().filter(|b| b["type"] == "tool_result") {
                        let key = string(block, "tool_use_id");
                        let tool = self.tools.remove(&key).unwrap_or_else(|| "Tool".into());
                        if self.plan_reply && tool == "ExitPlanMode" {
                            // The proposal already contains its readable plan. Refusing
                            // this callback does not turn that proposal into a failed step.
                            continue;
                        }
                        let failed = block["is_error"] == true;
                        out.push(HarnessEvent::Activity {
                            key: key.clone(),
                            tool: tool.clone(),
                            summary: format!("{tool} finished"),
                            status: if failed {
                                ActivityStatus::Failed
                            } else {
                                ActivityStatus::Succeeded
                            },
                            detail: Some(block["content"].to_string()),
                        });
                        if matches!(tool.as_str(), "Task" | "Agent") {
                            let label = self
                                .helpers
                                .remove(&key)
                                .unwrap_or_else(|| "Helper task".into());
                            out.push(HarnessEvent::Child {
                                lifetime: Some(protocol::model::ChildLifetime::InProcess),
                                native_ref: key,
                                label,
                                state: if failed {
                                    ChildState::Failed
                                } else {
                                    ChildState::Completed
                                },
                            });
                        }
                    }
                }
            }
            "result" => {
                self.completed = true;
                self.failed = value["is_error"] == true
                    || value["subtype"].as_str().is_some_and(|s| s != "success");
                if self.failed && !self.interrupted {
                    out.push(warning(unsuccessful(value)));
                }
                out.push(HarnessEvent::Results {
                    outcome: self.results.outcome(),
                });
                out.push(HarnessEvent::TurnCompleted {
                    usage: Some(UsageSummary {
                        input_tokens: count(&value["usage"]["input_tokens"]),
                        output_tokens: count(&value["usage"]["output_tokens"]),
                        duration_ms: count(&value["duration_ms"]),
                        reported_cost: value
                            .get("total_cost_usd")
                            .filter(|v| v.is_number())
                            .map(Value::to_string),
                    }),
                });
            }
            "tool_progress" => {
                // A heartbeat has an id of its own, and names the call it
                // reports on as its parent.
                let mut key = string(value, "tool_use_id");
                if !self.tools.contains_key(&key)
                    && let Some(call) = value["parent_tool_use_id"]
                        .as_str()
                        .filter(|id| self.tools.contains_key(*id))
                {
                    key = call.into();
                }
                let tool = value["tool_name"]
                    .as_str()
                    .filter(|t| !t.is_empty())
                    .map(str::to_owned)
                    .or_else(|| self.tools.get(&key).cloned())
                    .unwrap_or_else(|| "Tool".into());
                out.push(HarnessEvent::Activity {
                    key,
                    tool: tool.clone(),
                    summary: tool,
                    status: ActivityStatus::Running,
                    detail: None,
                })
            }
            "rate_limit_event" => self.rate_limit(&value["rate_limit_info"], &mut out),
            // Session hooks and request status belong to the person's own
            // Claude Code setup; only a failing hook is worth a line.
            "system" if value["subtype"] == "hook_response" => {
                if value["outcome"] != "success" || value["exit_code"].as_i64() != Some(0) {
                    out.push(HarnessEvent::Warning {
                        text: format!(
                            "Claude Code hook {} did not succeed",
                            value["hook_name"].as_str().unwrap_or("(unnamed)")
                        ),
                    });
                }
            }
            "system"
                if matches!(
                    value["subtype"].as_str(),
                    Some(
                        "hook_started" | "status" | "compact_boundary" | "background_tasks_changed"
                    )
                ) => {}
            // An estimate of how much the agent has thought so far in this
            // turn. Nothing in the app shows thinking progress; the turn's
            // usage counts it once the turn ends.
            "system" if value["subtype"] == "thinking_tokens" => {}
            // Running commentary on a background task. Its start and end come
            // as task_started and task_notification, which are what Talk shows.
            "system"
                if matches!(
                    value["subtype"].as_str(),
                    Some("task_updated" | "task_progress")
                ) => {}
            // Claude Code reports the fate of each message it was sent. Delivery
            // is confirmed by the replayed message; only a message it will never
            // act on is worth a line.
            "command_lifecycle" => {
                if let Some(state @ ("discarded" | "refused")) = value["state"].as_str() {
                    out.push(HarnessEvent::Warning {
                        text: format!("Claude Code {state} a message sent to this task"),
                    });
                }
            }
            other => {
                // Newer Claude Code releases add line types. Say so once per
                // kind rather than once per line.
                let kind = match value["subtype"].as_str() {
                    Some(sub) => format!("{other}/{sub}"),
                    None => other.to_owned(),
                };
                if self.unknown.insert(kind.clone()) {
                    out.push(HarnessEvent::Warning {
                        text: format!(
                            "Claude Code sent an update this version does not read ({kind})"
                        ),
                    });
                }
            }
        }
        if value["type"] == "user" || value["type"] == "assistant" {
            out.push(HarnessEvent::Results {
                outcome: self.results.outcome(),
            });
        }
        out
    }
    /// Plan limits are reported on every turn; only a change away from
    /// "allowed" (and the return to it) reaches the transcript. A refusal
    /// that paid overage does not cover also marks the account limited, with
    /// the reset time Claude Code gives in epoch seconds.
    fn rate_limit(&mut self, info: &Value, out: &mut Vec<HarnessEvent>) {
        let status = info["status"].as_str().unwrap_or("allowed");
        let window = match info["rateLimitType"].as_str() {
            Some("five_hour") => "five-hour",
            Some("seven_day") => "weekly",
            Some("seven_day_opus") => "weekly model",
            _ => "plan",
        };
        let state = (status != "allowed").then(|| format!("{status}/{window}"));
        if state == self.limited {
            return;
        }
        let previous = std::mem::replace(&mut self.limited, state);
        let text = match status {
            "allowed" if previous.is_some() => "Claude usage is available again".to_owned(),
            "allowed" => return,
            "allowed_warning" => {
                format!("Claude Code reports you are close to the {window} usage limit")
            }
            _ => format!("Claude Code reports the {window} usage limit is reached"),
        };
        out.push(HarnessEvent::Warning { text });
        let overage = info["isUsingOverage"] == true
            || matches!(
                info["overageStatus"].as_str(),
                Some("allowed" | "allowed_warning")
            );
        if status == "rejected" && !overage {
            out.push(HarnessEvent::UsageLimited {
                resets_at: info["resetsAt"]
                    .as_i64()
                    .and_then(protocol::time::Timestamp::from_unix_seconds),
            });
        }
    }
}

/// Only an explicit successful result is success; missing/new statuses stay unknown.
impl StreamState {
    /// A Task/Agent call runs inside its parent's turn (a backgrounded one
    /// returns its result at once), so a helper with no result when the
    /// turn ends did not outlive it: stopped by an interrupt, otherwise its
    /// outcome is unknown.
    pub fn end_turn_helpers(&mut self, interrupted: bool) -> Vec<HarnessEvent> {
        let mut ended: Vec<_> = self.helpers.drain().collect();
        ended.sort();
        ended
            .into_iter()
            .map(|(native_ref, label)| {
                self.tools.remove(&native_ref);
                HarnessEvent::Child {
                    lifetime: Some(protocol::model::ChildLifetime::InProcess),
                    native_ref,
                    label,
                    state: if interrupted {
                        ChildState::Stopped
                    } else {
                        ChildState::Unknown
                    },
                }
            })
            .collect()
    }
}
pub(crate) fn result_exit(value: &Value, interrupted: bool) -> ports::RunExit {
    use ports::RunExit;
    if interrupted {
        RunExit::Interrupted
    } else if value["subtype"] == "success" && value["is_error"] == false {
        RunExit::Completed
    } else if value["is_error"] == true
        || value["subtype"]
            .as_str()
            .is_some_and(|s| s.starts_with("error"))
    {
        let reason = value["errors"]
            .as_array()
            .map(|errors| {
                errors
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join("; ")
            })
            .filter(|s| !s.is_empty())
            .or_else(|| value["result"].as_str().map(str::to_owned))
            .unwrap_or_else(|| unsuccessful(value).to_owned());
        RunExit::Failed { reason }
    } else {
        RunExit::Unknown {
            reason: "Claude Code ended this turn without saying whether it succeeded".into(),
        }
    }
}

#[cfg(test)]
mod quiet_lines {
    use super::*;
    use serde_json::json;

    fn notes(state: &mut StreamState, line: Value) -> Vec<String> {
        state
            .parse(&line)
            .into_iter()
            .filter_map(|e| match e {
                HarnessEvent::Warning { text } => Some(text),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn hooks_status_and_allowed_limits_stay_out_of_the_transcript() {
        let mut state = StreamState::new(TaskMode::Work);
        for line in [
            json!({"type":"system","subtype":"hook_started","hook_name":"SessionStart:startup"}),
            json!({"type":"system","subtype":"hook_response","hook_name":"SessionStart:startup","outcome":"success","exit_code":0}),
            json!({"type":"system","subtype":"status","status":"requesting"}),
            json!({"type":"rate_limit_event","rate_limit_info":{"status":"allowed","rateLimitType":"five_hour"}}),
            json!({"type":"command_lifecycle","command_uuid":"m1","state":"queued"}),
            json!({"type":"command_lifecycle","command_uuid":"m1","state":"completed"}),
        ] {
            assert_eq!(notes(&mut state, line), Vec::<String>::new());
        }
        assert_eq!(
            notes(
                &mut state,
                json!({"type":"command_lifecycle","command_uuid":"m2","state":"refused"})
            ),
            ["Claude Code refused a message sent to this task"]
        );
        assert_eq!(
            notes(
                &mut state,
                json!({"type":"system","subtype":"hook_response","hook_name":"Stop","outcome":"error","exit_code":2})
            ),
            ["Claude Code hook Stop did not succeed"]
        );
    }

    #[test]
    fn limit_changes_are_reported_once_each() {
        let mut state = StreamState::new(TaskMode::Work);
        let limit = |status: &str| json!({"type":"rate_limit_event","rate_limit_info":{"status":status,"rateLimitType":"five_hour"}});
        assert_eq!(
            notes(&mut state, limit("allowed_warning")),
            ["Claude Code reports you are close to the five-hour usage limit"]
        );
        assert!(notes(&mut state, limit("allowed_warning")).is_empty());
        assert_eq!(
            notes(&mut state, limit("rejected")),
            ["Claude Code reports the five-hour usage limit is reached"]
        );
        assert_eq!(
            notes(&mut state, limit("allowed")),
            ["Claude usage is available again"]
        );
        assert!(notes(&mut state, limit("allowed")).is_empty());
    }

    #[test]
    fn a_refused_request_marks_the_account_limited_until_its_reset() {
        let limited = |line: Value| {
            StreamState::new(TaskMode::Work)
                .parse(&line)
                .into_iter()
                .filter_map(|e| match e {
                    HarnessEvent::UsageLimited { resets_at } => Some(resets_at),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(
            limited(
                json!({"type":"rate_limit_event","rate_limit_info":{"status":"rejected","rateLimitType":"five_hour","resetsAt":1_790_000_000}})
            ),
            [Some(protocol::time::Timestamp(1_790_000_000_000))]
        );
        assert_eq!(
            limited(
                json!({"type":"rate_limit_event","rate_limit_info":{"status":"rejected","rateLimitType":"seven_day"}})
            ),
            [None]
        );
        // Paid overage still serves the request; a warning is not a limit.
        // The refusal's own message is Claude Code's, not a reply from the agent.
        let refusal = StreamState::new(TaskMode::Work).parse(&json!({"type":"assistant","is_api_error_message":true,
            "message":{"id":"e","model":"<synthetic>","content":[{"type":"text","text":"You've hit your session limit · resets 3pm"}]}}));
        assert!(refusal.iter().any(
            |e| matches!(e, HarnessEvent::Warning { text } if text.starts_with("You've hit"))
        ));
        assert!(
            !refusal
                .iter()
                .any(|e| matches!(e, HarnessEvent::AgentMessage { .. }))
        );
        assert!(limited(json!({"type":"rate_limit_event","rate_limit_info":{"status":"rejected","rateLimitType":"five_hour","overageStatus":"allowed"}})).is_empty());
        assert!(limited(json!({"type":"rate_limit_event","rate_limit_info":{"status":"allowed_warning","rateLimitType":"five_hour"}})).is_empty());
    }

    #[test]
    fn tool_summaries_remain_readable_after_a_turn_stops() {
        let mut state = StreamState::new(TaskMode::Work);
        for line in [
            json!({"type":"assistant","message":{"content":[{"type":"tool_use","id":"step","name":"Task","input":{}}]}}),
            json!({"type":"tool_progress","tool_use_id":"step"}),
            json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"step","content":"Stopped","is_error":true}]}}),
        ] {
            let events = state.parse(&line);
            assert!(
                events
                    .iter()
                    .any(|e| matches!(e, HarnessEvent::Activity { .. }))
            );
            for event in events {
                if let HarnessEvent::Activity { summary, .. } = event {
                    assert!(!summary.to_lowercase().contains("running"), "{summary}");
                }
            }
        }
    }

    #[test]
    fn thinking_progress_is_read_without_a_note() {
        let mut state = StreamState::new(TaskMode::Work);
        // Shaped as Claude Code 2.1.284 writes it.
        let line = json!({"type":"system","subtype":"thinking_tokens","estimated_tokens":412,"estimated_tokens_delta":96,"user_message_uuid":"5d2c3a1e-0f4b-4c8e-9a51-2b7d6e8f9a10","uuid":"8a1f4e2b-6c3d-4b5a-9e7f-1d2c3b4a5e6f","session_id":"native-session"});
        assert!(state.parse(&line).is_empty());
    }

    #[test]
    fn background_task_commentary_is_read_without_a_note() {
        let mut state = StreamState::new(TaskMode::Work);
        // Shaped as Claude Code 2.1.284 declares them.
        for line in [
            json!({"type":"system","subtype":"task_updated","task_id":"t","patch":{"status":"running","is_backgrounded":true},"uuid":"u","session_id":"native-session"}),
            json!({"type":"system","subtype":"task_progress","task_id":"t","description":"Index","uuid":"u","session_id":"native-session"}),
        ] {
            assert!(state.parse(&line).is_empty());
        }
    }

    #[test]
    fn an_unknown_kind_is_named_once() {
        let mut state = StreamState::new(TaskMode::Work);
        let line = json!({"type":"system","subtype":"future_thing"});
        assert_eq!(
            notes(&mut state, line.clone()),
            ["Claude Code sent an update this version does not read (system/future_thing)"]
        );
        assert!(notes(&mut state, line).is_empty());
        assert_eq!(notes(&mut state, json!({"type":"brand_new"})).len(), 1);
    }

    // These notes and reasons appear in the task's transcript and receipt;
    // Claude Code's status codes and this app's internals mean nothing there.
    #[test]
    fn an_unsuccessful_turn_says_why_in_plain_words() {
        use ports::RunExit;
        for (subtype, said) in [
            (
                "error_max_turns",
                "Claude Code stopped this turn at its turn limit",
            ),
            (
                "error_max_budget_usd",
                "Claude Code stopped this turn at its spending limit",
            ),
            (
                "error_during_execution",
                "Claude Code ended this turn with an error",
            ),
        ] {
            let line = json!({"type":"result","subtype":subtype,"is_error":true});
            assert_eq!(
                notes(&mut StreamState::new(TaskMode::Work), line.clone()),
                [said]
            );
            assert_eq!(
                result_exit(&line, false),
                RunExit::Failed {
                    reason: said.into()
                }
            );
        }
        assert_eq!(
            result_exit(&json!({"type":"result"}), false),
            RunExit::Unknown {
                reason: "Claude Code ended this turn without saying whether it succeeded".into()
            }
        );
    }

    #[test]
    fn a_stopped_turn_is_not_called_an_error() {
        let line = json!({"type":"result","subtype":"error_during_execution","is_error":true});
        let mut state = StreamState::new(TaskMode::Work);
        state.interrupted = true;
        assert!(notes(&mut state, line.clone()).is_empty());
        assert!(state.failed);
        assert_eq!(result_exit(&line, true), ports::RunExit::Interrupted);
    }

    #[test]
    fn a_line_left_out_of_the_transcript_says_so_plainly() {
        let mut state = StreamState::new(TaskMode::Work);
        for (line, said) in [
            (
                json!({"type":"system","subtype":"init"}),
                "Claude Code started without reporting its session",
            ),
            (
                json!({"type":"stream_event","event":{"type":"future_delta"}}),
                "Claude Code sent an update this app does not recognize, so it is not shown",
            ),
            (
                json!({"type":"assistant","message":{}}),
                "Claude Code sent a reply with nothing in it",
            ),
        ] {
            assert_eq!(notes(&mut state, line), [said]);
        }
    }

    #[test]
    fn steps_without_their_own_name_are_named_for_what_they_are() {
        let mut state = StreamState::new(TaskMode::Work);
        let summaries = |events: Vec<HarnessEvent>| {
            events
                .into_iter()
                .filter_map(|e| match e {
                    HarnessEvent::Activity { summary, .. } => Some(summary),
                    HarnessEvent::Child { label, .. } => Some(label),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(
            summaries(
                state.parse(&json!({"type":"assistant","message":{"content":[
                {"type":"tool_use","id":"helper","name":"Task","input":{}},
                {"type":"tool_use","id":"shell","name":"Bash","input":{}}]}}))
            ),
            ["Task", "Helper task", "Bash"]
        );
        assert_eq!(
            summaries(
                state.parse(
                    &json!({"type":"tool_progress","tool_use_id":"shell","tool_name":"Bash"})
                )
            ),
            ["Bash"]
        );
        assert_eq!(
            summaries(state.parse(&json!({"type":"tool_progress","tool_use_id":"shell"}))),
            ["Bash"]
        );
        assert_eq!(
            summaries(state.parse(&json!({"type":"tool_progress","tool_use_id":"unseen"}))),
            ["Tool"]
        );
        assert_eq!(
            summaries(state.parse(&json!({"type":"user","message":{"content":[
                {"type":"tool_result","tool_use_id":"unseen","content":"ok"}]}}))),
            ["Tool finished"]
        );
        assert_eq!(
            summaries(
                state.parse(&json!({"type":"system","subtype":"task_started","task_id":"t"}))
            ),
            ["Background task"]
        );
    }
}

#[cfg(test)]
mod lifetime_tests {
    use super::*;
    use protocol::model::ChildLifetime;
    use serde_json::json;

    #[test]
    fn task_and_agent_calls_report_in_process_lifetime() {
        for tool in ["Task", "Agent"] {
            let mut stream = StreamState::default();
            let events = stream.parse(&json!({"type":"assistant", "message":{"content":[{"type":"tool_use", "id":"helper", "name":tool, "input":{"description":"Summarize the README"}}]}}));
            assert!(events.iter().any(|event| matches!(
                event,
                HarnessEvent::Child {
                    lifetime: Some(ChildLifetime::InProcess),
                    state: ChildState::Active,
                    ..
                }
            )));
            assert!(matches!(
                stream.end_turn_helpers(false)[0],
                HarnessEvent::Child {
                    lifetime: Some(ChildLifetime::InProcess),
                    state: ChildState::Unknown,
                    ..
                }
            ));
        }
    }

    fn children(events: &[HarnessEvent]) -> Vec<(String, String, ChildState)> {
        events
            .iter()
            .filter_map(|event| match event {
                HarnessEvent::Child {
                    native_ref,
                    label,
                    state,
                    ..
                } => Some((native_ref.clone(), label.clone(), *state)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_long_calls_heartbeat_is_its_progress_not_a_helper() {
        let mut stream = StreamState::default();
        stream.parse(&json!({"type":"assistant","message":{"content":[{"type":"tool_use","id":"toolu_1","name":"Bash","input":{"command":"npm test"}}]}}));
        // Shaped as Claude Code 2.1.292 writes it every 30 seconds a call runs.
        let beat = stream.parse(&json!({"type":"tool_progress","tool_use_id":"toolu_1-heartbeat-0","tool_name":"Bash","parent_tool_use_id":"toolu_1","elapsed_time_seconds":30,"heartbeat":true,"session_id":"s","uuid":"u"}));
        assert!(children(&beat).is_empty(), "{beat:?}");
        assert!(matches!(
            &beat[..],
            [HarnessEvent::Activity { key, status: ActivityStatus::Running, .. }] if key == "toolu_1"
        ));
        stream.parse(&json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_1","content":"ok"}]}}));
        assert!(stream.end_turn_helpers(false).is_empty());
    }

    #[test]
    fn a_helper_is_running_only_until_its_call_returns() {
        let mut stream = StreamState::default();
        stream.parse(&json!({"type":"assistant","message":{"content":[{"type":"tool_use","id":"agent","name":"Agent","input":{"description":"Check the page"}}]}}));
        let own = stream.parse(&json!({"type":"assistant","parent_tool_use_id":"agent","message":{"id":"x","content":[{"type":"text","text":"Looking"}]}}));
        assert_eq!(
            children(&own),
            [("agent".into(), "Check the page".into(), ChildState::Active)]
        );
        let ended = stream.parse(&json!({"type":"result","subtype":"success","is_error":false,"parent_tool_use_id":"agent"}));
        assert_eq!(
            children(&ended).last(),
            Some(&(
                "agent".into(),
                "Check the page".into(),
                ChildState::Completed
            ))
        );
        // Backgrounded, its call returns at once; what it writes after
        // belongs to its background task, which reports its own end.
        stream.parse(&json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"agent","content":"Started in the background"}]}}));
        let later = stream.parse(&json!({"type":"assistant","parent_tool_use_id":"agent","message":{"id":"y","content":[{"type":"text","text":"Still looking"}]}}));
        assert!(children(&later).is_empty(), "{later:?}");
        assert!(
            stream
                .parse(&json!({"type":"result","subtype":"success","is_error":false,"parent_tool_use_id":"agent"}))
                .is_empty()
        );
        assert!(stream.end_turn_helpers(false).is_empty());
    }
}

#[cfg(test)]
mod wire_bounds {
    use super::*;
    use serde_json::json;

    fn usage(line: Value) -> Option<UsageSummary> {
        StreamState::new(TaskMode::Work)
            .parse(&line)
            .into_iter()
            .find_map(|e| match e {
                HarnessEvent::TurnCompleted { usage } => usage,
                _ => None,
            })
    }

    fn resets_at(seconds: Value) -> Vec<Option<protocol::time::Timestamp>> {
        StreamState::new(TaskMode::Work)
            .parse(&json!({"type":"rate_limit_event","rate_limit_info":{
                "status":"rejected","rateLimitType":"five_hour","resetsAt":seconds}}))
            .into_iter()
            .filter_map(|e| match e {
                HarnessEvent::UsageLimited { resets_at } => Some(resets_at),
                _ => None,
            })
            .collect()
    }

    // The journal refuses any integer the phone cannot represent, and a
    // refused event interrupts the run. A number Claude Code reports beyond
    // that bound is unknown, not a reason to stop the agent.
    #[test]
    fn numbers_beyond_the_wire_bound_read_as_unknown() {
        let beyond = protocol::MAX_SAFE_INTEGER + 1;
        assert_eq!(
            usage(json!({"type":"result","subtype":"success","is_error":false,
                "usage":{"input_tokens":beyond,"output_tokens":protocol::MAX_SAFE_INTEGER},
                "duration_ms":u64::MAX})),
            Some(UsageSummary {
                input_tokens: None,
                output_tokens: Some(protocol::MAX_SAFE_INTEGER),
                duration_ms: None,
                reported_cost: None,
            })
        );
        for seconds in [
            json!(i64::MAX),
            json!(i64::MIN),
            json!(protocol::MAX_SAFE_INTEGER / 1000 + 1),
        ] {
            assert_eq!(resets_at(seconds), [None]);
        }
        assert_eq!(
            resets_at(json!(protocol::MAX_SAFE_INTEGER / 1000)),
            [Some(protocol::time::Timestamp(
                (protocol::MAX_SAFE_INTEGER / 1000 * 1000) as i64
            ))]
        );
    }
}

#[cfg(test)]
mod generated_lines {
    use super::*;
    use crate::background::Background;
    use serde_json::json;

    /// One line of each kind this parser reads, as Claude Code writes them.
    fn lines() -> Vec<Value> {
        vec![
            json!({"type":"system","subtype":"init","session_id":"s","model":"m","effort":"high"}),
            json!({"type":"system","subtype":"task_started","task_id":"t","description":"Index","task_type":"local_agent"}),
            json!({"type":"system","subtype":"task_notification","task_id":"t","status":"completed"}),
            json!({"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"t","description":"Index"}]}),
            json!({"type":"system","subtype":"hook_response","hook_name":"Stop","outcome":"error","exit_code":2}),
            json!({"type":"stream_event","parent_tool_use_id":null,"event":{"type":"message_start","message":{"id":"m1"}}}),
            json!({"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"Hel"}}}),
            json!({"type":"assistant","message":{"id":"m1","content":[
                {"type":"text","text":"Hello"},
                {"type":"tool_use","id":"t1","name":"Bash","input":{"command":"cargo test"}},
                {"type":"tool_use","id":"c1","name":"Task","input":{"description":"Helper"}},
                {"type":"tool_use","id":"e1","name":"Edit","input":{"file_path":"a.rs","old_string":"a","new_string":"b"}},
                {"type":"tool_use","id":"w1","name":"Write","input":{"file_path":"b.rs","content":"b"}},
                {"type":"tool_use","id":"p1","name":"ExitPlanMode","input":{"plan":"1. Read"}}]}}),
            json!({"type":"assistant","is_api_error_message":true,"message":{"id":"e","content":[{"type":"text","text":"Limit"}]}}),
            json!({"type":"assistant","parent_tool_use_id":"c1","message":{"id":"x","content":[{"type":"text","text":"child"}]}}),
            json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"ok","is_error":true}]},"tool_use_result":{"exit_code":1}}),
            json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"e1","content":[{"type":"text","text":"done"}]}]},
                "tool_use_result":{"type":"create","structuredPatch":[{"lines":["-a","+b"]}]}}),
            json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"c1","content":"Helped"}]}}),
            json!({"type":"tool_progress","tool_use_id":"t1","tool_name":"Bash"}),
            json!({"type":"rate_limit_event","rate_limit_info":{"status":"rejected","rateLimitType":"five_hour","resetsAt":1_790_000_000,"overageStatus":"rejected","isUsingOverage":false}}),
            json!({"type":"command_lifecycle","command_uuid":"m","state":"refused"}),
            json!({"type":"control_request","request_id":"r","request":{"subtype":"can_use_tool","tool_name":"ExitPlanMode","input":{"plan":"x"}}}),
            json!({"type":"result","subtype":"success","is_error":false,"result":"Done","usage":{"input_tokens":12,"output_tokens":4},
                "duration_ms":10,"total_cost_usd":0.1,"errors":["x"],"permission_denials":[{"tool_use_id":"t1"}]}),
            json!({"type":"result","parent_tool_use_id":"c1","subtype":"error_during_execution","is_error":true}),
        ]
    }

    /// What reaches the journal stays within the wire's integer range.
    fn within_bounds(event: &HarnessEvent) -> bool {
        let safe = |n: &Option<u64>| n.is_none_or(|n| n <= protocol::MAX_SAFE_INTEGER);
        match event {
            HarnessEvent::TurnCompleted { usage: Some(usage) } => {
                safe(&usage.input_tokens) && safe(&usage.output_tokens) && safe(&usage.duration_ms)
            }
            HarnessEvent::UsageLimited {
                resets_at: Some(time),
            } => time.0.unsigned_abs() <= protocol::MAX_SAFE_INTEGER,
            HarnessEvent::Results { outcome } => {
                protocol::integer::validate(&serde_json::to_value(outcome).unwrap()).is_ok()
            }
            _ => true,
        }
    }

    // Claude Code is another program; any line it writes, including ones this
    // version has never seen, must be read without a panic and without an
    // out-of-range value.
    #[test]
    fn any_variant_of_a_native_line_is_read_within_bounds() {
        let lines = lines();
        let mut variants = test_support::json::Variants::new(0x5eed_c1a0);
        for mode in [TaskMode::Work, TaskMode::Plan] {
            let mut state = StreamState::new(mode);
            let (mut background, _) = Background::new();
            for n in 0..6000 {
                let line = variants.of(&lines[n % lines.len()]);
                let read = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let _ = background.observe(&line);
                    state.expected_plan_callback(&line);
                    let _ = result_exit(&line, n % 2 == 0);
                    state.parse(&line)
                }));
                let Ok(events) = read else {
                    panic!("panicked on {line}");
                };
                for event in events {
                    assert!(within_bounds(&event), "{event:?} from {line}");
                }
            }
            state.end_turn_helpers(false);
        }
    }
}
