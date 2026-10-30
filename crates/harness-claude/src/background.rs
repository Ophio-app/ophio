// SPDX-License-Identifier: Apache-2.0
//! Replace-set tracking for native background work. Acknowledgement is not exit.
use crate::{Control, Reply, Urgent};
use ports::{HarnessError, HarnessEvent, harness::ChildStopResult};
use protocol::{
    capability::Availability,
    model::{ChildLifetime, ChildState},
};
use serde_json::Value;
use std::{collections::BTreeMap, time::Duration};
use tokio::sync::watch;

/// Only native task-type evidence classifies background work.
pub(crate) fn task_lifetime(value: &Value) -> Option<ChildLifetime> {
    match value["task_type"].as_str() {
        Some("local_agent") => Some(ChildLifetime::InProcess),
        Some("local_bash" | "bash") => Some(ChildLifetime::SeparateProcess),
        _ => None,
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Task {
    label: String,
    state: ChildState,
    stopping: bool,
    lifetime: Option<ChildLifetime>,
}
pub(crate) type Snapshot = BTreeMap<String, Task>;
pub(crate) struct Background {
    tasks: Snapshot,
    updates: watch::Sender<Snapshot>,
}
impl Background {
    pub fn new() -> (Self, watch::Receiver<Snapshot>) {
        let (updates, rx) = watch::channel(Snapshot::new());
        (
            Self {
                tasks: Snapshot::new(),
                updates,
            },
            rx,
        )
    }
    pub fn live(&self) -> Vec<String> {
        self.tasks
            .iter()
            .filter(|(_, task)| matches!(task.state, ChildState::Active | ChildState::Unknown))
            .map(|(id, _)| id.clone())
            .collect()
    }
    pub fn unrequested(&self) -> Vec<String> {
        self.tasks
            .iter()
            .filter(|(_, task)| {
                matches!(task.state, ChildState::Active | ChildState::Unknown) && !task.stopping
            })
            .map(|(id, _)| id.clone())
            .collect()
    }
    pub fn request(&mut self, id: &str) -> Result<(), HarnessError> {
        let task = self
            .tasks
            .get_mut(id)
            .filter(|t| matches!(t.state, ChildState::Active | ChildState::Unknown))
            .ok_or(HarnessError::NotActive)?;
        task.stopping = true;
        Ok(())
    }
    /// Validate the complete replacement before removing anything. Malformed
    /// updates cannot certify completion of work that was previously running.
    pub fn observe(&mut self, value: &Value) -> Result<Vec<HarnessEvent>, HarnessError> {
        if value["type"] != "system" {
            return Ok(vec![]);
        }
        let mut next = self.tasks.clone();
        match value["subtype"].as_str() {
            Some("background_tasks_changed") => {
                let tasks = value["tasks"]
                    .as_array()
                    .filter(|tasks| tasks.len() <= 256)
                    .ok_or_else(|| HarnessError::Protocol("Invalid background task set".into()))?;
                let mut replacement = BTreeMap::new();
                for task in tasks {
                    let id = task["task_id"]
                        .as_str()
                        .filter(|id| !id.is_empty() && id.len() <= 256)
                        .ok_or_else(|| {
                            HarnessError::Protocol("Background task identity missing".into())
                        })?;
                    let label = task["description"]
                        .as_str()
                        .unwrap_or("Background task")
                        .chars()
                        .take(512)
                        .collect();
                    if replacement
                        .insert(id.to_owned(), (label, task_lifetime(task)))
                        .is_some()
                    {
                        return Err(HarnessError::Protocol(
                            "Duplicate background task identity".into(),
                        ));
                    }
                }
                for (id, task) in &mut next {
                    if matches!(task.state, ChildState::Active | ChildState::Unknown)
                        && !replacement.contains_key(id)
                    {
                        task.state = if task.stopping {
                            ChildState::Stopped
                        } else {
                            ChildState::Completed
                        };
                    }
                }
                for (id, (label, lifetime)) in replacement {
                    let stopping = next.get(&id).is_some_and(|t| {
                        t.stopping && matches!(t.state, ChildState::Active | ChildState::Unknown)
                    });
                    next.insert(
                        id.clone(),
                        Task {
                            label,
                            lifetime: lifetime.or_else(|| next.get(&id).and_then(|t| t.lifetime)),
                            state: ChildState::Active,
                            stopping,
                        },
                    );
                }
            }
            Some("task_started") => {
                if let Some(id) = value["task_id"]
                    .as_str()
                    .filter(|s| !s.is_empty() && s.len() <= 256)
                {
                    next.insert(
                        id.into(),
                        Task {
                            label: value["description"]
                                .as_str()
                                .unwrap_or("Background task")
                                .chars()
                                .take(512)
                                .collect(),
                            state: ChildState::Active,
                            stopping: false,
                            lifetime: task_lifetime(value),
                        },
                    );
                }
            }
            Some("task_notification") => {
                if let Some(task) = value["task_id"].as_str().and_then(|id| next.get_mut(id)) {
                    task.state = match value["status"].as_str() {
                        Some("completed") => ChildState::Completed,
                        Some("stopped") => ChildState::Stopped,
                        Some("failed") => ChildState::Failed,
                        _ => ChildState::Unknown,
                    };
                }
            }
            _ => return Ok(vec![]),
        }
        if next.len() > 4096 {
            return Err(HarnessError::Protocol(
                "Native task history exceeds the session limit".into(),
            ));
        }
        let events = next
            .iter()
            .filter(|(id, task)| self.tasks.get(*id) != Some(task))
            .map(|(id, task)| HarnessEvent::Child {
                lifetime: task.lifetime,
                native_ref: id.clone(),
                label: task.label.clone(),
                state: task.state,
            })
            .collect();
        self.tasks = next;
        Ok(events)
    }
    pub fn publish(&self) {
        self.updates.send_replace(self.tasks.clone());
    }
}
impl Control {
    pub(crate) fn child_availability(&self, id: &str) -> Availability {
        if *self.done.borrow() {
            Availability::TemporarilyUnavailable { reason: "The agent’s control connection ended. Open this task’s conversation in Claude Code on the computer and check the helper’s task details and output.".into() }
        } else if self.background.borrow().contains_key(id) {
            Availability::Available
        } else {
            Availability::Unsupported { reason: "Claude Code did not supply a stoppable task identity for this helper. Stop it in Claude Code on the computer.".into() }
        }
    }
    pub(crate) async fn stop_native_child(
        &self,
        id: &str,
    ) -> Result<ChildStopResult, HarnessError> {
        if let Availability::Unsupported { reason } = self.child_availability(id) {
            return Err(HarnessError::Unsupported(reason));
        }
        if let Some(state) = self.background.borrow().get(id).map(|task| task.state)
            && matches!(state, ChildState::Completed | ChildState::Stopped)
        {
            return Ok(ChildStopResult {
                state,
                detail: "Claude Code already reported this task ended".into(),
            });
        }
        let mut updates = self.background.clone();
        let (tx, rx): (Reply<()>, _) = tokio::sync::oneshot::channel();
        self.urgent
            .send(Urgent::StopChild(id.into(), tx))
            .await
            .map_err(|_| HarnessError::NotActive)?;
        rx.await.map_err(|_| HarnessError::NotActive)??;
        let observed = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let state = updates.borrow_and_update().get(id).map(|task| task.state);
                if let Some(state) = state.filter(|s| *s != ChildState::Active) {
                    return state;
                }
                if updates.changed().await.is_err() {
                    return ChildState::Unknown;
                }
            }
        })
        .await;
        let state = observed.unwrap_or_else(|_| {
            if *self.done.borrow() {
                ChildState::Unknown
            } else {
                updates
                    .borrow()
                    .get(id)
                    .map_or(ChildState::Unknown, |t| t.state)
            }
        });
        Ok(ChildStopResult { state, detail: match state {
            ChildState::Stopped => "Claude Code removed this task from its live set after Stop",
            ChildState::Completed => "Claude Code reported this task completed",
            ChildState::Active => "Still in Claude Code’s latest live set; Stop was not confirmed within two seconds",
            _ => "Claude Code has not confirmed that this task ended; check it on the computer",
        }.into() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn replacement_completes_removed_tasks_and_preserves_stop_evidence() {
        let (mut state, _) = Background::new();
        let set = |ids: Vec<&str>| json!({"type":"system","subtype":"background_tasks_changed","tasks":ids.into_iter().map(|id| json!({"task_id":id,"description":id,"task_type":"bash"})).collect::<Vec<_>>()});
        assert_eq!(state.observe(&set(vec!["a", "b"])).unwrap().len(), 2);
        assert!(state.observe(&set(vec!["a", "b"])).unwrap().is_empty());
        state.request("a").unwrap();
        let events = state.observe(&set(vec![])).unwrap();
        assert!(events.iter().any(|e| matches!(e, HarnessEvent::Child { native_ref, state: ChildState::Stopped, .. } if native_ref == "a")));
        assert!(events.iter().any(|e| matches!(e, HarnessEvent::Child { native_ref, state: ChildState::Completed, .. } if native_ref == "b")));
        state.observe(&set(vec!["c"])).unwrap();
        assert!(
            state
                .observe(&json!({"type":"system","subtype":"background_tasks_changed"}))
                .is_err()
        );
        assert_eq!(state.live(), ["c"]);
    }
}

#[cfg(test)]
mod lifetime_tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn classifies_only_reported_native_task_types() {
        for (kind, expected) in [
            ("local_agent", Some(ChildLifetime::InProcess)),
            ("local_bash", Some(ChildLifetime::SeparateProcess)),
            ("bash", Some(ChildLifetime::SeparateProcess)),
            ("future_task", None),
        ] {
            let (mut background, _) = Background::new();
            let events = background.observe(&json!({"type":"system", "subtype":"task_started", "task_id":"a", "task_type":kind})).unwrap();
            assert!(
                matches!(events[0], HarnessEvent::Child { lifetime, .. } if lifetime == expected)
            );
        }
        assert_eq!(task_lifetime(&json!({})), None);
    }
}
