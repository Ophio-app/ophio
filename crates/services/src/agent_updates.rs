// SPDX-License-Identifier: Apache-2.0
//! Øphio's own copies of the agents' programs, kept up to date while the
//! owner has that on (see `ports::agent_copy`).
use crate::{CompanionHub, error};
use protocol::{agent_updates::*, *};
use std::{sync::atomic::Ordering, time::Duration};

/// How often the copies are checked against their makers' newest releases.
pub(crate) const EVERY: Duration = Duration::from_secs(6 * 60 * 60);

/// Counts a check from when it is asked for until it ends or is dropped.
struct Checking(CompanionHub);
impl Drop for Checking {
    fn drop(&mut self) {
        self.0.inner.agent_checks.fetch_sub(1, Ordering::SeqCst);
    }
}

impl CompanionHub {
    pub(crate) fn agent_updates_view(&self) -> Result<AgentUpdatesView, ProtocolError> {
        let mut view: AgentUpdatesView = self
            .read(|r| r.get("meta", "agent_updates"))?
            .unwrap_or_default();
        view.supported = self.agent_copies_supported();
        view.checking = self.inner.agent_checks.load(Ordering::SeqCst) > 0;
        Ok(view)
    }
    pub(crate) fn set_agent_updates(
        &self,
        tx: &mut store::Write<'_>,
        enabled: bool,
        revision: Option<u64>,
    ) -> store::Result<()> {
        let mut view: AgentUpdatesView =
            tx.read().get("meta", "agent_updates")?.unwrap_or_default();
        domain::revision(revision, view.revision)?;
        if enabled && !self.agent_copies_supported() {
            return Err(error(
                ErrorCode::Unsupported,
                "Claude Code and Codex don't publish programs for this computer's system.",
            )
            .into());
        }
        view.enabled = enabled;
        view.revision += 1;
        tx.put("meta", "agent_updates", &view)
    }
    fn agent_copies_supported(&self) -> bool {
        self.inner
            .agent_updater
            .as_ref()
            .is_some_and(|updater| updater.supported())
    }
    /// Brings the copies in line with the setting, in the background: on,
    /// each gets its maker's newest release; off, they go.
    pub(crate) fn update_agents(&self) {
        let Some(updater) = self.inner.agent_updater.clone() else {
            return;
        };
        self.inner.agent_checks.fetch_add(1, Ordering::SeqCst);
        let checking = Checking(self.clone());
        let mut shutdown = self.inner.shutdown.subscribe();
        self.spawn(async move {
            let hub = checking.0.clone();
            let work = async {
                let _one = hub.inner.agent_updates.lock().await;
                let view: AgentUpdatesView = hub
                    .read(|r| r.get("meta", "agent_updates"))?
                    .unwrap_or_default();
                let states = updater.apply(view.enabled).await;
                let now = hub.inner.clock.now();
                hub.write(|tx| {
                    let mut stored: AgentUpdatesView =
                        tx.read().get("meta", "agent_updates")?.unwrap_or_default();
                    // Turned the other way meanwhile: the check queued
                    // behind this one records that.
                    if stored.enabled != view.enabled {
                        return Ok(());
                    }
                    stored.agents = states
                        .into_iter()
                        .map(|state| AgentCopy {
                            checked_at: if state.checked {
                                Some(now)
                            } else {
                                stored
                                    .agents
                                    .iter()
                                    .find(|copy| copy.harness == state.harness)
                                    .and_then(|copy| copy.checked_at)
                            },
                            harness: state.harness,
                            version: state.version,
                            problem: state.problem,
                        })
                        .collect();
                    tx.put("meta", "agent_updates", &stored)
                })?;
                hub.refresh_catalog().await
            };
            tokio::select! {
                _ = shutdown.changed() => {}
                result = work => if let Err(e) = result {
                    tracing::error!(code = ?e.code, "updating the agents' copies failed");
                },
            }
            drop(checking);
        });
    }
}
