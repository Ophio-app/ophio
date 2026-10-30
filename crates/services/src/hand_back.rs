// SPDX-License-Identifier: Apache-2.0
//! An agent acts on a screen only while a person watches it: its lease
//! lapses three seconds after their phone stops renewing it. Its next
//! action then waits for them to hand the screen back, rather than ending
//! the agent's work with nothing tried, and comes back with the screen as
//! it is then, for the agent to act again from.
use crate::{
    CompanionHub,
    control::Target,
    error,
    github_watch::{headline_item, raise_item},
};
use ports::bridge::BridgeResponse;
use protocol::{control::*, model::*, notifications::AlertTopic, *};
use std::time::Duration;
use tokio::sync::{Mutex, MutexGuard};

/// How long an action waits for the person to hand the screen back.
pub(crate) const HAND_BACK: Duration = Duration::from_secs(600);
const LOOKED_EVERY: Duration = Duration::from_millis(250);
/// The heading of what asks the person to hand it back.
const PAUSED: &str = "Agent paused";

enum Lease {
    Live,
    /// Lapsed, revoked or held by the person, for them to hand back.
    Paused,
    Gone,
}

fn gone() -> ProtocolError {
    error(ErrorCode::Expired, "The agent no longer has control")
}

impl CompanionHub {
    /// `target`, locked, once `run_id`'s lease on it is live, and whether
    /// the person handed it back meanwhile.
    pub(crate) async fn agent_input<'a>(
        &self,
        run_id: RunId,
        task: &Task,
        target: &'a Mutex<Target>,
    ) -> Result<(MutexGuard<'a, Target>, bool), ProtocolError> {
        let until = self.inner.clock.monotonic() + HAND_BACK;
        let mut asked = None;
        let outcome = loop {
            let mut held = target.lock().await;
            let target_id = held.view.target_id;
            match self.agent_lease(run_id, &mut held).await {
                Ok(Lease::Live) if asked.is_none() => return Ok((held, false)),
                Ok(Lease::Live) => break Ok((held, true)),
                Ok(Lease::Gone) => break Err(gone()),
                Err(e) => break Err(e),
                Ok(Lease::Paused) => {}
            }
            drop(held);
            if self.inner.clock.monotonic() >= until {
                break Err(error(
                    ErrorCode::Expired,
                    format!(
                        "Nobody handed the screen back in {} minutes. You can act on the screen only while the person watches it in Ophio. Stop here and say what you couldn't try, so they can run it again while they watch.",
                        HAND_BACK.as_secs() / 60
                    ),
                ));
            }
            if asked.is_none() {
                asked = Some(self.ask_hand_back(task, run_id, target_id)?);
            }
            tokio::time::sleep(LOOKED_EVERY).await;
        };
        if let Some(id) = asked {
            self.settle_hand_back(id)?;
        }
        outcome
    }

    /// What an action that waited for the person is answered with once
    /// they hand the screen back: not taken, since the screen may have
    /// changed while they had it, and the screen as it is now.
    pub(crate) async fn resumed(
        &self,
        run_id: RunId,
        task: &Task,
        target: &mut Target,
    ) -> Result<BridgeResponse, ProtocolError> {
        const NOT_SENT: &str = "Not sent, and nothing is wrong: the person paused you while this action waited, then resumed you. It wasn't sent, because the screen may have changed while you were paused.";
        match self.observe_locked(run_id, task, target, None, None).await {
            Ok(seen) => Ok(BridgeResponse::Resumed {
                text: format!(
                    "{NOT_SENT} This is the screen now: carry on from it, and send the action again if it still makes sense."
                ),
                image_base64: seen.image_base64,
                media_type: seen.media_type,
                width: seen.width,
                height: seen.height,
                geometry_revision: seen.geometry_revision,
            }),
            Err(_) => Err(error(
                ErrorCode::StaleRevision,
                format!(
                    "{NOT_SENT} Call observe_target to see the screen now, then carry on from it."
                ),
            )),
        }
    }

    async fn agent_lease(
        &self,
        run_id: RunId,
        target: &mut Target,
    ) -> Result<Lease, ProtocolError> {
        // A private step ends the run's turn; it doesn't wait.
        if self.run_stopped(run_id)? || target.removed || target.view.private_step.is_some() {
            return Ok(Lease::Gone);
        }
        let ours = target.view.lease.as_ref().filter(|l| {
            l.owner == LeaseOwner::Agent { run_id } && l.boot_epoch == self.inner.epoch
        });
        if ours.is_some_and(|l| l.state == LeaseState::Active) {
            if target
                .deadline
                .is_some_and(|d| self.inner.clock.monotonic() < d)
            {
                return Ok(Lease::Live);
            }
            // Lapsed before the expiry tick got to it.
            self.revoke_locked(target, true).await?;
            return Ok(Lease::Paused);
        }
        // The person took the screen while this run watched it.
        let watched = (target.view.model_observation.as_ref()).is_some_and(|o| o.run_id == run_id);
        Ok(if ours.is_some() || watched {
            Lease::Paused
        } else {
            Lease::Gone
        })
    }

    /// Asks the person to hand `target_id` back to `run_id`, once for the
    /// run however many of its actions wait.
    fn ask_hand_back(
        &self,
        task: &Task,
        run_id: RunId,
        target_id: TargetId,
    ) -> Result<AttentionId, ProtocolError> {
        let targets: Vec<ControlTarget> =
            self.read(|r| r.get("meta", "targets"))?.unwrap_or_default();
        let label = (targets.iter().find(|t| t.target_id == target_id))
            .map_or("the screen", |t| t.label.as_str());
        let body = format!(
            "It can use {label} only while you watch it. Open it and tap Resume agent; it waits {} minutes.",
            HAND_BACK.as_secs() / 60
        );
        let now = self.inner.clock.now();
        self.write(|tx| {
            let waiting = tx.read().task_attention::<AttentionItem>(task.task_id)?;
            if let Some(item) = waiting.iter().find(|i| {
                i.run_id == Some(run_id)
                    && i.resolved_at.is_none()
                    && i.kind == AttentionKind::WaitingForYou
                    && i.title == PAUSED
            }) {
                return Ok(item.attention_id);
            }
            let mut item = headline_item(
                task.task_id,
                AttentionKind::WaitingForYou,
                AlertTopic::NeedsYou,
                PAUSED,
                Some(body),
                now,
            );
            // Ends with the turn, if it ends first.
            item.run_id = Some(run_id);
            item.priority = 70;
            let id = item.attention_id;
            raise_item(tx, item)?;
            Ok(id)
        })
    }

    fn settle_hand_back(&self, id: AttentionId) -> Result<(), ProtocolError> {
        let now = self.inner.clock.now();
        self.write(|tx| {
            let Some(mut item) = tx.read().get::<AttentionItem>("attention", id)? else {
                return Ok(());
            };
            if item.resolved_at.is_some() {
                return Ok(());
            }
            item.resolved_at = Some(now);
            tx.put("attention", id, &item)?;
            tx.emit(Event::AttentionResolved { attention_id: id })?;
            match item.task_id {
                Some(task_id) => crate::runs::recount_attention(tx, task_id),
                None => Ok(()),
            }
        })
    }
}
