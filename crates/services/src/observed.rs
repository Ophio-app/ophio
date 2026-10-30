// SPDX-License-Identifier: Apache-2.0
//! Native observations that do not imply managed task authority.
use crate::CompanionHub;
use protocol::{capability::*, model::*, *};

impl CompanionHub {
    pub(crate) fn native_readiness_warning(
        &self,
        run_id: RunId,
        reason: &str,
    ) -> Result<(), ProtocolError> {
        self.write(|tx| {
            let run: Run = tx.read().require("run", run_id)?;
            let mut harnesses: Vec<HarnessInfo> =
                tx.read().get("meta", "harnesses")?.unwrap_or_default();
            let (mut label, mut summary) = (
                "Agent sandbox".to_owned(),
                "Agents can’t start tasks right now".to_owned(),
            );
            if let Some(harness) = harnesses.iter_mut().find(|h| h.kind == run.harness) {
                label = format!("{} start run", harness.display_name);
                summary = format!("{} can’t start tasks right now", harness.display_name);
                harness.capabilities.start_run = Availability::TemporarilyUnavailable {
                    reason: reason.into(),
                };
            }
            tx.put("meta", "harnesses", &harnesses)?;
            tx.emit(Event::HarnessesChanged { harnesses })?;
            let mut report = tx
                .read()
                .get::<ReadinessReport>("meta", "readiness")?
                .unwrap_or(ReadinessReport {
                    checked_at: self.inner.clock.now(),
                    checks: vec![],
                });
            report.checked_at = self.inner.clock.now();
            report.checks.retain(|c| c.label != label);
            report.checks.push(ReadinessCheck {
                area: ReadinessArea::Accounts,
                label,
                status: CheckStatus::Failed,
                detail: Some(reason.into()),
                remedy: Some(
                    "Fix the sandbox on the computer, then restart Øphio and check readiness."
                        .into(),
                ),
                summary: Some(summary),
            });
            tx.put("meta", "readiness", &report)?;
            tx.emit(Event::ReadinessReported { report })?;
            Ok(())
        })
    }
}

use ports::harness::{AgentProcess, AgentSessionSource};
use protocol::observed::{ObservedSession, ObservedSessions};
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

#[derive(Default)]
pub(crate) struct Observer {
    state: Mutex<ObservationState>,
    scan: tokio::sync::Mutex<()>,
}
#[derive(Default)]
struct ObservationState {
    source: Option<Arc<dyn AgentSessionSource>>,
    last_scan: Option<Instant>,
    revision: u64,
    view: Option<ObservedSessions>,
    processes: HashMap<String, AgentProcess>,
}
const COVERAGE: &str = "Claude Code and Codex processes and session metadata are checked. Hermes, OpenClaw and Pi are not checked.";

fn project_for(cwd: Option<&std::path::Path>, projects: &[Project]) -> Option<ProjectId> {
    let cwd = cwd?;
    projects
        .iter()
        .filter(|p| cwd.starts_with(&p.host_path))
        .max_by_key(|p| p.host_path.len())
        .map(|p| p.project_id)
}
impl CompanionHub {
    pub fn set_session_observer(&self, source: Arc<dyn AgentSessionSource>) {
        self.inner
            .observed
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .source = Some(source);
    }

    pub(crate) async fn observed_sessions(&self) -> Result<ObservedSessions, ProtocolError> {
        // The scan is demand-driven by the Agents view. A detached caller cannot
        // keep filesystem/process observation alive by polling a local query.
        let watching = self
            .inner
            .subscribers
            .lock()
            .map_err(crate::internal)?
            .iter()
            .any(|s| s.caller.grants.tasks && s.sender.receiver_count() > 0);
        if watching {
            let _scan = self.inner.observed.scan.lock().await;
            let now = self.inner.clock.monotonic();
            let source = {
                let mut state = self.inner.observed.state.lock().map_err(crate::internal)?;
                if state.last_scan.is_some_and(|last| {
                    now.saturating_duration_since(last) < Duration::from_secs(10)
                }) {
                    None
                } else {
                    state.last_scan = Some(now);
                    state.source.clone()
                }
            };
            if let Some(source) = source {
                let result = tokio::time::timeout(Duration::from_secs(5), source.scan()).await;
                match result {
                    Ok(Ok(mut scan)) => {
                        // Only identities we may display need reconciliation. A
                        // scan with no records never looks up managed sessions.
                        scan.records.truncate(256);
                        let (projects, sessions) = self.read(|r| {
                            let mut sessions = HashSet::new();
                            for record in &scan.records {
                                if r.has_native_session(record.harness, &record.session_id)? {
                                    sessions.insert((record.harness, record.session_id.clone()));
                                }
                            }
                            Ok((r.list::<Project>("project")?, sessions))
                        })?;
                        let managed: Vec<_> = self
                            .inner
                            .runs
                            .lock()
                            .await
                            .values()
                            .filter_map(|c| c.process_id())
                            .collect();
                        let at = self.inner.clock.now();
                        let mut state =
                            self.inner.observed.state.lock().map_err(crate::internal)?;
                        state.revision = state
                            .revision
                            .saturating_add(1)
                            .min(protocol::MAX_SAFE_INTEGER);
                        let revision = state.revision;
                        let mut rows = Vec::new();
                        let mut processes = HashMap::new();
                        for process in scan.processes.into_iter().take(256) {
                            if managed.contains(&process.pid)
                                || process.ancestors.iter().any(|pid| managed.contains(pid))
                            {
                                continue;
                            }
                            let id = state
                                .processes
                                .iter()
                                .find(|(_, p)| {
                                    p.pid == process.pid
                                        && p.start_key == process.start_key
                                        && p.executable == process.executable
                                })
                                .map(|(id, _)| id.clone())
                                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
                            let stop = if process.process_group == Some(process.pid)
                                && process.pid > 1
                            {
                                Availability::Available
                            } else {
                                Availability::Unsupported { reason: "This agent shares a process group or its group cannot be verified. Stop it in its terminal on the computer.".into() }
                            };
                            rows.push(ObservedSession {
                                observed_id: id.clone(),
                                revision,
                                harness: process.harness,
                                native_session: None,
                                process_id: Some(process.pid),
                                executable: Some(process.executable.to_string_lossy().into_owned()),
                                cwd: process
                                    .cwd
                                    .as_ref()
                                    .map(|p| p.to_string_lossy().into_owned()),
                                project_id: project_for(process.cwd.as_deref(), &projects),
                                started_at: process.started_at,
                                last_activity: None,
                                observed_at: at,
                                running: true,
                                stop,
                            });
                            processes.insert(id, process);
                        }
                        for record in scan.records {
                            if sessions.contains(&(record.harness, record.session_id.clone())) {
                                continue;
                            }
                            // Path/time similarity cannot establish process identity.
                            // Keep records distinct and never call an unmatched record Running.
                            rows.push(ObservedSession { observed_id: format!("record:{:?}:{}", record.harness, record.session_id), revision, harness: record.harness, native_session: Some(record.session_id), process_id: None, executable: None, project_id: project_for(Some(&record.cwd), &projects), cwd: Some(record.cwd.to_string_lossy().into_owned()), started_at: record.started_at, last_activity: record.last_activity, observed_at: at, running: false, stop: Availability::Unsupported { reason: "A session record does not identify a running process to stop".into() } });
                        }
                        let mut coverage = vec![COVERAGE.into()];
                        coverage.extend(scan.coverage);
                        state.processes = processes;
                        state.view = Some(ObservedSessions {
                            checked_at: Some(at),
                            sessions: rows,
                            coverage,
                            error: None,
                        });
                    }
                    _ => {
                        let mut state =
                            self.inner.observed.state.lock().map_err(crate::internal)?;
                        let mut view = state.view.take().unwrap_or(ObservedSessions {
                            checked_at: None,
                            sessions: vec![],
                            coverage: vec![COVERAGE.into()],
                            error: None,
                        });
                        view.error = Some("The computer could not refresh outside sessions. These are the last observations.".into());
                        for row in &mut view.sessions {
                            row.stop = Availability::TemporarilyUnavailable {
                                reason: "Refresh outside sessions before stopping a process".into(),
                            };
                        }
                        state.view = Some(view);
                        state.processes.clear();
                    }
                }
            }
        }
        let state = self.inner.observed.state.lock().map_err(crate::internal)?;
        Ok(state.view.clone().unwrap_or(ObservedSessions {
            checked_at: None,
            sessions: vec![],
            coverage: vec![COVERAGE.into()],
            error: Some(
                if state.source.is_some() {
                    "Waiting for a connected client to request an observation"
                } else {
                    "Outside session observation is unavailable on this computer"
                }
                .into(),
            ),
        }))
    }

    pub(crate) fn admit_observed_stop(
        &self,
        tx: &mut store::Write<'_>,
        command_id: CommandId,
        id: &str,
        confirmed: bool,
        expected: Option<u64>,
    ) -> store::Result<()> {
        if !confirmed {
            return Err(crate::error(
                ErrorCode::InvalidRequest,
                "Confirm stopping the outside agent’s process group first",
            )
            .into());
        }
        let mut state = self.inner.observed.state.lock().map_err(crate::internal)?;
        let row = state
            .view
            .as_ref()
            .and_then(|v| v.sessions.iter().find(|s| s.observed_id == id))
            .ok_or_else(|| {
                crate::error(ErrorCode::NotFound, "Outside session is no longer observed")
            })?;
        domain::revision(expected, row.revision)?;
        domain::availability(&row.stop)?;
        if state.last_scan.is_none_or(|last| {
            self.inner.clock.monotonic().saturating_duration_since(last) > Duration::from_secs(30)
        }) {
            return Err(crate::error(
                ErrorCode::StaleRevision,
                "Refresh outside sessions before stopping this process",
            )
            .into());
        }
        let process = state.processes.get(id).ok_or_else(|| {
            crate::error(
                ErrorCode::NotFound,
                "No process identity for this observation",
            )
        })?;
        tx.put("observed_stop", command_id, process)?;
        // Reserve this observation while the accepted intervention executes.
        // Replaying the command id uses its durable receipt; a new id must refresh.
        state.revision = state
            .revision
            .saturating_add(1)
            .min(protocol::MAX_SAFE_INTEGER);
        let revision = state.revision;
        if let Some(row) = state
            .view
            .as_mut()
            .and_then(|v| v.sessions.iter_mut().find(|s| s.observed_id == id))
        {
            row.revision = revision;
            row.stop = Availability::TemporarilyUnavailable {
                reason: "A Stop request is awaiting its process-group result".into(),
            };
        }
        Ok(())
    }

    pub(crate) async fn stop_observed(
        &self,
        command_id: CommandId,
    ) -> Result<protocol::command::CommandResult, ProtocolError> {
        let process: AgentProcess = self.read(|r| r.require("observed_stop", command_id))?;
        let source = self
            .inner
            .observed
            .state
            .lock()
            .map_err(crate::internal)?
            .source
            .clone()
            .ok_or_else(|| crate::unsupported("Outside session observation"))?;
        let requested_at = self.inner.clock.now();
        let result = tokio::time::timeout(Duration::from_secs(4), source.stop(&process)).await;
        let (status, detail) = match result {
            Ok(Ok(result)) => (
                match result.state {
                    ChildState::Stopped | ChildState::Completed => FactStatus::Done,
                    ChildState::Active => FactStatus::StillActive,
                    _ => FactStatus::Unknown,
                },
                result.detail,
            ),
            Ok(Err(error)) => (FactStatus::Unknown, error.to_string()),
            Err(_) => (
                FactStatus::Unknown,
                "The computer has not confirmed process-group exit".into(),
            ),
        };
        let receipt = InterventionReceipt {
            kind: InterventionKind::StopTask,
            requested_at,
            facts: vec![ReceiptFact {
                label: format!("Outside agent process group {}", process.pid),
                status,
                detail: Some(detail),
            }],
            summary: if status == FactStatus::Done {
                "The observed process group has ended"
            } else {
                "The outside agent’s Stop outcome is not confirmed"
            }
            .into(),
        };
        self.write(|tx| {
            tx.emit(Event::InterventionRecorded {
                task_id: None,
                target_id: None,
                receipt: Box::new(receipt.clone()),
            })?;
            Ok(())
        })?;
        if status == FactStatus::Done {
            let mut state = self.inner.observed.state.lock().map_err(crate::internal)?;
            if let Some(view) = &mut state.view {
                for row in &mut view.sessions {
                    if row.process_id == Some(process.pid) {
                        row.running = false;
                        row.stop = Availability::Unsupported {
                            reason: "The observed process group has ended".into(),
                        };
                    }
                }
            }
        }
        Ok(protocol::command::CommandResult::Intervention {
            receipt: Box::new(receipt),
        })
    }
}
