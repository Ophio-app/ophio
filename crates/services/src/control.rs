// SPDX-License-Identifier: Apache-2.0
use crate::{CompanionHub, api::Caller, error, unsupported};
use protocol::{auth::Principal, control::*, model::*, *};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::Mutex;

pub(crate) struct Target {
    pub view: TargetControlView,
    pub removed: bool,
    pub geometry: TargetGeometry,
    pub sink: Box<dyn ports::InputSink>,
    pub generation: u64,
    pub last_seq: u64,
    pub deadline: Option<Instant>,
    pub supervisor: Option<DeviceId>,
    pub observed_generation: Option<u64>,
    /// The last save of `view` failed; the next desktop observation saves it
    /// again, whole.
    pub unsaved: bool,
}
impl CompanionHub {
    /// Publish the catalog and its initial read-only control projections in one
    /// transaction. Merely discovering a target never opens a native input sink.
    /// A target without input is published too, as unavailable, so the
    /// desktop's lock reaches it: a computer locked before Ophio found its
    /// screens, which then take no input, is still shown locked and offered
    /// for unlocking. It is ready once its input is.
    pub(crate) fn publish_targets(&self, targets: Vec<ControlTarget>) -> Result<(), ProtocolError> {
        self.write(|tx| {
            let previous: Vec<ControlTarget> =
                tx.read().get("meta", "targets")?.unwrap_or_default();
            if previous != targets {
                tx.put("meta", "targets", &targets)?;
                tx.emit(Event::TargetsChanged {
                    targets: targets.clone(),
                })?;
            }
            for target in &targets {
                let ready = target.input.is_available();
                let view = match tx
                    .read()
                    .get::<TargetControlView>("control", target.target_id)?
                {
                    None => TargetControlView {
                        target_id: target.target_id,
                        input_state: if ready {
                            InputState::Ready
                        } else {
                            InputState::Unavailable
                        },
                        lease: None,
                        model_observation: None,
                        private_step: None,
                        desktop_lock: None,
                    },
                    // Only this projection is ever unavailable; a screen in use
                    // keeps its own state.
                    Some(view) if ready && view.input_state == InputState::Unavailable => {
                        TargetControlView {
                            input_state: InputState::Ready,
                            ..view
                        }
                    }
                    Some(_) => continue,
                };
                tx.put("control", target.target_id, &view)?;
                tx.emit(Event::ControlChanged { view })?;
            }
            Ok(())
        })
    }

    pub(crate) async fn target(&self, id: TargetId) -> Result<Arc<Mutex<Target>>, ProtocolError> {
        let mut controls = self.inner.controls.lock().await;
        if let Some(target) = controls.get(&id) {
            return Ok(target.clone());
        }
        let backend = self.desktop()?;
        let target = backend
            .targets()
            .await
            .map_err(platform)?
            .into_iter()
            .find(|t| t.target_id == id)
            .ok_or_else(|| error(ErrorCode::NotFound, "Desktop target not found"))?;
        if !target.input.is_available() {
            return Err(error(
                ErrorCode::TemporarilyUnavailable,
                "Input is unavailable for this target",
            ));
        }
        let sink = backend.input_sink(id).await.map_err(platform)?;
        let view = TargetControlView {
            target_id: id,
            input_state: InputState::Ready,
            lease: None,
            model_observation: None,
            private_step: None,
            desktop_lock: None,
        };
        self.save_control(&view)?;
        let state = Arc::new(Mutex::new(Target {
            removed: false,
            view,
            geometry: target.geometry,
            sink,
            generation: 0,
            last_seq: 0,
            deadline: None,
            supervisor: None,
            observed_generation: None,
            unsaved: false,
        }));
        controls.insert(id, state.clone());
        Ok(state)
    }
    /// Saves a live screen's whole projection, remembering a save that failed
    /// so the phone is not left with state the screen has moved past.
    pub(crate) fn save_target(&self, target: &mut Target) -> Result<(), ProtocolError> {
        let saved = self.save_control(&target.view);
        target.unsaved = saved.is_err();
        saved
    }
    pub(crate) fn save_control(&self, view: &TargetControlView) -> Result<(), ProtocolError> {
        // An agent browser tab is the owner's while their lease holds it: the
        // agent's tools wait, as they would for one input owner.
        if let Some(browser) = &self.inner.agent_browser {
            browser.set_taken_over(
                view.target_id,
                view.lease.as_ref().is_some_and(|l| {
                    matches!(l.owner, LeaseOwner::Human { .. })
                        && matches!(l.state, LeaseState::Active | LeaseState::Revoking)
                }),
            );
        }
        self.write(|tx| {
            tx.put("control", view.target_id, view)?;
            tx.emit(Event::ControlChanged { view: view.clone() })?;
            Ok(())
        })
    }
    pub(crate) async fn refresh_geometry(&self, target: &mut Target) -> Result<(), ProtocolError> {
        self.check_desktop_lock(target).await?;
        if target.removed {
            return Err(error(
                ErrorCode::OsPermissionMissing,
                "Desktop sharing ended on the computer",
            ));
        }
        let backend = self.desktop()?;
        let targets = backend.targets().await.map_err(platform)?;
        self.publish_targets(targets.clone())?;
        let current = targets
            .into_iter()
            .find(|t| t.target_id == target.view.target_id)
            .ok_or_else(|| error(ErrorCode::NotFound, "Target disappeared"))?;
        if current.geometry != target.geometry {
            // Release held input at the old geometry before accepting actions at
            // the new revision. The action envelope is the phone's explicit
            // acknowledgement of the geometry used to compute its coordinates.
            if !matches!(
                tokio::time::timeout(Duration::from_secs(2), target.sink.release_all()).await,
                Ok(Ok(()))
            ) {
                let _ = self.revoke_locked(target, false).await;
                return Err(barrier_required());
            }
            target.geometry = current.geometry;
            target.observed_generation = None;
        }
        if !current.input.is_available() {
            self.revoke_locked(target, false).await?;
            return Err(error(
                ErrorCode::TemporarilyUnavailable,
                "Target input is unavailable",
            ));
        }
        Ok(())
    }
    async fn grant_locked(
        &self,
        target: &mut Target,
        owner: LeaseOwner,
        supervisor: Option<DeviceId>,
    ) -> Result<LeaseView, ProtocolError> {
        if target.view.input_state == InputState::Unknown {
            return Err(barrier_required());
        }
        if target.removed
            || matches!(
                target.view.input_state,
                InputState::Revoking | InputState::Unknown
            )
            || target
                .view
                .lease
                .as_ref()
                .is_some_and(|l| l.state == LeaseState::Active)
        {
            return Err(error(
                ErrorCode::Conflict,
                "Control hasn’t been let go yet. Try again in a moment",
            ));
        }
        if self
            .inner
            .local_stops
            .load(std::sync::atomic::Ordering::SeqCst)
            != 0
        {
            return Err(error(
                ErrorCode::Conflict,
                "Local Stop is fencing computer control. Review the target again before requesting input.",
            ));
        }
        let protection = self.inner.protection.lock().await;
        if let Some(indicator) = &protection.indicator {
            let holders = self.protection_holders(Some((target.view.target_id, owner)))?;
            tokio::time::timeout(Duration::from_secs(2), indicator.show(&holders))
                .await
                .map_err(|_| {
                    error(
                        ErrorCode::TemporarilyUnavailable,
                        "Local Stop indicator did not answer",
                    )
                })?
                .map_err(platform)?;
        } else if protection.required {
            return Err(error(
                ErrorCode::TemporarilyUnavailable,
                crate::runtime::INDICATOR_UNAVAILABLE,
            ));
        }
        target.generation = target
            .generation
            .checked_add(1)
            .filter(|n| *n <= protocol::MAX_SAFE_INTEGER)
            .ok_or_else(|| error(ErrorCode::LimitExceeded, "Lease generation exhausted"))?;
        let lease = LeaseView {
            lease_id: LeaseId::new(),
            target_id: target.view.target_id,
            owner,
            generation: target.generation,
            boot_epoch: self.inner.epoch,
            state: LeaseState::Active,
            granted_at: self.inner.clock.now(),
            renewal_window_ms: self.inner.renewal.as_millis() as u32,
        };
        target.view.input_state = match owner {
            LeaseOwner::Human { .. } => InputState::Human,
            LeaseOwner::Agent { .. } => InputState::Agent,
        };
        target.view.lease = Some(lease.clone());
        target.deadline = Some(self.inner.clock.monotonic() + self.inner.renewal);
        self.inner.expiry.notify_one();
        target.last_seq = 0;
        target.supervisor = supervisor;
        target.observed_generation = None;
        if let Err(e) = self.save_target(target) {
            target.view.input_state = InputState::Unknown;
            if let Some(l) = &mut target.view.lease {
                l.state = LeaseState::Unknown;
            }
            return Err(e);
        }
        Ok(lease)
    }
    pub(crate) async fn revoke_locked(
        &self,
        target: &mut Target,
        expired: bool,
    ) -> Result<(), ProtocolError> {
        target.view.input_state = InputState::Revoking;
        target.deadline = None;
        target.observed_generation = None;
        if let Some(lease) = &mut target.view.lease {
            lease.state = LeaseState::Revoking;
        }
        // Fencing in memory precedes persistence and OS work. A storage failure
        // must never leave the sink open to fresh actions.
        let persisted = self.save_target(target);
        let released =
            tokio::time::timeout(Duration::from_secs(2), target.sink.release_all()).await;
        let (voice_released, voice_recorded) = self
            .voice_fence_target(target.view.target_id, expired)
            .await;
        let acknowledged = matches!(released, Ok(Ok(()))) && voice_released.is_ok();
        target.view.input_state = if acknowledged {
            InputState::Paused
        } else {
            InputState::Unknown
        };
        if let Some(lease) = &mut target.view.lease {
            lease.state = if acknowledged {
                if expired {
                    LeaseState::Expired
                } else {
                    LeaseState::Revoked
                }
            } else {
                LeaseState::Unknown
            };
        }
        let saved = self.save_target(target);
        let indicated = self.refresh_local_indicator().await;
        // Input that may still be held matters more than what went unrecorded.
        if !acknowledged {
            return Err(release_unconfirmed());
        }
        saved.and(persisted).and(voice_recorded).and(indicated)
    }
    pub(crate) async fn revoke_target(
        &self,
        id: TargetId,
        expired: bool,
    ) -> Result<(), ProtocolError> {
        let target = self.target(id).await?;
        self.revoke_locked(&mut *target.lock().await, expired).await
    }
    pub(crate) async fn expire_leases(&self) -> Result<(), ProtocolError> {
        let targets: Vec<_> = self.inner.controls.lock().await.values().cloned().collect();
        let mut failure = None;
        for target in targets {
            let mut target = target.lock().await;
            if target
                .deadline
                .is_some_and(|d| self.inner.clock.monotonic() >= d)
            {
                keep_worse(&mut failure, self.revoke_locked(&mut target, true).await);
            }
        }
        failure.map_or(Ok(()), Err)
    }
    /// Whether a lease deadline or voice session is left for the expiry tick.
    pub(crate) async fn expiry_pending(&self) -> bool {
        if self.voice_pending() {
            return true;
        }
        let targets: Vec<_> = self.inner.controls.lock().await.values().cloned().collect();
        for target in targets {
            if target.lock().await.deadline.is_some() {
                return true;
            }
        }
        false
    }
    pub(crate) async fn grant_agent(
        &self,
        run_id: RunId,
        target_id: TargetId,
    ) -> Result<(), ProtocolError> {
        let (task, supervisor) = self.read(|r| {
            let run: Run = r.require("run", run_id)?;
            let task: Task = r.require("task", run.task_id)?;
            let supervisor = if let Principal::Device { device_id } = task.created_by {
                Some(device_id)
            } else {
                None
            };
            // The agent browser's tabs are driven through its own tools.
            let targets: Vec<ControlTarget> = r.get("meta", "targets")?.unwrap_or_default();
            if targets
                .iter()
                .any(|t| t.target_id == target_id && t.kind == TargetKind::Browser)
            {
                return Err(error(
                    ErrorCode::PermissionDenied,
                    "Computer use can't drive the agent browser",
                )
                .into());
            }
            Ok((task, supervisor))
        })?;
        if task.target_id != Some(target_id) || self.run_stopped(run_id)? {
            return Err(error(
                ErrorCode::PermissionDenied,
                "Run is not authorized for this target",
            ));
        }
        let target = self.target(target_id).await?;
        let mut target = target.lock().await;
        self.refresh_geometry(&mut target).await?;
        if target.view.private_step.is_some() {
            return Err(error(
                ErrorCode::Conflict,
                "Private step must be ended explicitly",
            ));
        }
        self.grant_locked(&mut target, LeaseOwner::Agent { run_id }, supervisor)
            .await?;
        let prior = target
            .view
            .model_observation
            .as_ref()
            .filter(|o| o.run_id == run_id);
        let delivered_count = prior.map_or(0, |o| o.delivered_count);
        let observation_id = prior.map_or_else(ObservationId::new, |o| o.observation_id);
        target.view.model_observation = Some(ModelObservationView {
            observation_id,
            run_id,
            target_id,
            state: ObservationState::Allowed,
            delivered_count,
        });
        self.save_target(&mut target)
    }
    pub(crate) async fn revoke_run(&self, run_id: RunId) -> Result<(), ProtocolError> {
        let mut failure = None;
        // A credential that cannot be withdrawn is no reason to leave the
        // run's screens open to it.
        keep_worse(
            &mut failure,
            self.write(|tx| {
                tx.delete("run_token", run_id)?;
                Ok(())
            }),
        );
        // What the run was let use in the agent browser goes too, however
        // late its bridge lets go.
        self.end_browsing(run_id).await;
        let targets: Vec<_> = self.inner.controls.lock().await.values().cloned().collect();
        for target in targets {
            let mut target = target.lock().await;
            if target
                .view
                .model_observation
                .as_ref()
                .is_some_and(|o| o.run_id == run_id)
            {
                if let Some(o) = &mut target.view.model_observation {
                    o.state = ObservationState::Withdrawn;
                }
                // Unrecorded, the withdrawal still holds; the run's input
                // must still be fenced.
                keep_worse(&mut failure, self.save_target(&mut target));
            }
            if target
                .view
                .lease
                .as_ref()
                .is_some_and(|l| l.owner == LeaseOwner::Agent { run_id })
            {
                keep_worse(&mut failure, self.revoke_locked(&mut target, false).await);
            }
            // Once its input is surely let go, nobody waits for the screen
            // any more: it is free, not an agent paused.
            if target.view.input_state == InputState::Paused
                && target
                    .view
                    .lease
                    .as_ref()
                    .is_some_and(|l| l.owner == LeaseOwner::Agent { run_id })
            {
                target.view.input_state = InputState::Ready;
                target.view.lease = None;
                keep_worse(&mut failure, self.save_target(&mut target));
            }
        }
        let approvals: Vec<Approval> = match self.read(|r| r.list("approval")) {
            Ok(approvals) => approvals,
            Err(e) => {
                keep_worse(&mut failure, Err(e));
                Vec::new()
            }
        };
        let mut waits = self.inner.waits.lock().await;
        for a in approvals {
            if a.run_id == run_id
                && let Some(wait) = waits.remove(&a.approval_id)
            {
                let _ = wait.send(protocol::command::ApprovalDecision::Deny {
                    reason: Some("Run stopped".into()),
                });
            }
        }
        failure.map_or(Ok(()), Err)
    }
    pub(crate) async fn revoke_device_leases(
        &self,
        device_id: DeviceId,
    ) -> Result<(), ProtocolError> {
        self.voice_mark_device(device_id).await;
        let targets: Vec<_> = self.inner.controls.lock().await.values().cloned().collect();
        let mut failure = None;
        for target in targets {
            let mut target = target.lock().await;
            if target.supervisor == Some(device_id)
                || target
                    .view
                    .lease
                    .as_ref()
                    .is_some_and(|l| l.owner == LeaseOwner::Human { device_id })
            {
                keep_worse(&mut failure, self.revoke_locked(&mut target, false).await);
            }
        }
        failure.map_or(Ok(()), Err)
    }
    pub(crate) async fn human_input(&self, caller: &Caller, input: InputEnvelope) -> InputAck {
        let (outcome, refusal) = self.inject_human(caller, &input).await.unwrap_or_else(|e| {
            if e.message == "Computer is locked" {
                return (InputOutcome::Failed, Some(InputRefusal::Locked));
            }
            (
                match e.code {
                    ErrorCode::StaleRevision => InputOutcome::StaleGeometry,
                    ErrorCode::OutcomeUnknown => InputOutcome::Unknown,
                    _ => InputOutcome::Failed,
                },
                None,
            )
        });
        InputAck {
            lease_id: input.lease_id,
            seq: input.seq,
            outcome,
            refusal,
        }
    }
    async fn inject_human(
        &self,
        caller: &Caller,
        input: &InputEnvelope,
    ) -> Result<(InputOutcome, Option<InputRefusal>), ProtocolError> {
        let caller = match self.authorize(caller, |g| g.control_input) {
            Ok(c) => c,
            Err(_) => return Ok((InputOutcome::StaleLease, None)),
        };
        let Principal::Device { device_id } = caller.principal else {
            return Ok((InputOutcome::StaleLease, None));
        };
        let targets: Vec<_> = self.inner.controls.lock().await.values().cloned().collect();
        for target in targets {
            let mut target = target.lock().await;
            let Some(lease) = target.view.lease.clone() else {
                continue;
            };
            if lease.lease_id != input.lease_id {
                continue;
            }
            self.check_desktop_lock(&mut target).await?;
            if lease.owner != (LeaseOwner::Human { device_id })
                || lease.generation != input.generation
                || lease.boot_epoch != self.inner.epoch
                || lease.state != LeaseState::Active
            {
                return Ok((InputOutcome::StaleLease, None));
            }
            if target
                .deadline
                .is_none_or(|d| self.inner.clock.monotonic() >= d)
            {
                self.revoke_locked(&mut target, true).await?;
                return Ok((InputOutcome::StaleLease, None));
            }
            self.refresh_geometry(&mut target).await?;
            if input.geometry_revision != target.geometry.revision {
                return Ok((InputOutcome::StaleGeometry, None));
            }
            if input.seq <= target.last_seq {
                return Ok((InputOutcome::OutOfOrder, None));
            }
            if !domain::lease::valid_action(&input.action) {
                return Ok((InputOutcome::Failed, None));
            }
            target.last_seq = input.seq;
            let geometry = target.geometry;
            // A short native operation is bounded; failed/timeout delivery is
            // never replayed. Revoke is serialized behind this single action.
            let injected = tokio::time::timeout(
                Duration::from_millis(500),
                target.sink.inject(&input.action, &geometry),
            )
            .await;
            if let Ok(Err(ports::PlatformError::TextUnavailable {
                characters,
                paste_available,
            })) = injected
            {
                return Ok((
                    InputOutcome::Failed,
                    Some(InputRefusal::TextUnavailable {
                        characters,
                        paste_available,
                    }),
                ));
            }
            if matches!(injected, Ok(Err(ports::PlatformError::Locked))) {
                self.apply_desktop_lock(&mut target, DesktopLockState::Locked)
                    .await?;
                return Ok((InputOutcome::Failed, Some(InputRefusal::Locked)));
            }
            if !matches!(injected, Ok(Ok(()))) {
                let outcome = match injected {
                    Err(_) | Ok(Err(ports::PlatformError::OutcomeUnknown(_))) => {
                        InputOutcome::Unknown
                    }
                    _ => InputOutcome::Failed,
                };
                // Revocation records its own uncertainty. It cannot turn an
                // uncertain action into a refusal, or a refusal into an action.
                let _ = self.revoke_locked(&mut target, false).await;
                return Ok((outcome, None));
            }
            return Ok((InputOutcome::Injected, None));
        }
        Ok((InputOutcome::StaleLease, None))
    }
    pub(crate) async fn renew(
        &self,
        caller: &Caller,
        id: LeaseId,
        generation: u64,
    ) -> Result<(), ProtocolError> {
        let caller = self.authorize(caller, |g| g.control_input)?;
        let Principal::Device { device_id } = caller.principal else {
            return Err(error(
                ErrorCode::PermissionDenied,
                "Only a paired device can keep control",
            ));
        };
        let targets: Vec<_> = self.inner.controls.lock().await.values().cloned().collect();
        for target in targets {
            let mut target = target.lock().await;
            if target.view.lease.as_ref().is_some_and(|l| {
                l.lease_id == id
                    && l.generation == generation
                    && l.boot_epoch == self.inner.epoch
                    && l.state == LeaseState::Active
                    && (l.owner == LeaseOwner::Human { device_id }
                        || target.supervisor == Some(device_id))
            }) {
                if target
                    .deadline
                    .is_none_or(|d| self.inner.clock.monotonic() >= d)
                {
                    self.revoke_locked(&mut target, true).await?;
                    return Err(error(ErrorCode::Expired, "Lease expired"));
                }
                self.refresh_geometry(&mut target).await?;
                target.deadline = Some(self.inner.clock.monotonic() + self.inner.renewal);
                self.inner.expiry.notify_one();
                return Ok(());
            }
        }
        Err(error(ErrorCode::Expired, "Lease is no longer active"))
    }
    pub(crate) async fn control_command(
        &self,
        caller: &Caller,
        command: &Command,
    ) -> Result<Option<protocol::command::CommandResult>, ProtocolError> {
        use Command::*;
        let mut result = None;
        match command {
            RequestHumanLease {
                target_id,
                geometry_revision,
            }
            | TakeOver {
                target_id,
                geometry_revision,
            } => {
                let Principal::Device { device_id } = caller.principal else {
                    return Err(error(
                        ErrorCode::PermissionDenied,
                        "Human input requires a paired device",
                    ));
                };
                let target = self.target(*target_id).await?;
                let mut target = target.lock().await;
                if target.view.input_state == InputState::Unknown {
                    return Err(barrier_required());
                }
                self.refresh_geometry(&mut target).await?;
                if *geometry_revision != target.geometry.revision {
                    return Err(error(ErrorCode::StaleRevision, "Target geometry changed"));
                }
                if target.view.private_step.as_ref().is_some_and(|p| !p.ready) {
                    return Err(error(
                        ErrorCode::OutcomeUnknown,
                        "The private step isn’t covered yet, so input stays closed",
                    ));
                }
                if matches!(command, TakeOver { .. }) {
                    self.revoke_locked(&mut target, false).await?;
                }
                let lease = self
                    .grant_locked(
                        &mut target,
                        LeaseOwner::Human { device_id },
                        Some(device_id),
                    )
                    .await?;
                result = Some(protocol::command::CommandResult::LeaseGranted { lease });
                if matches!(command, TakeOver { .. }) {
                    self.intervention(
                        *target_id,
                        InterventionKind::TakeOver,
                        "The agent let go of the input, and you have control",
                        true,
                    )?;
                }
            }
            ReleaseLease { lease_id } => {
                let targets: Vec<_> = self.inner.controls.lock().await.values().cloned().collect();
                let mut found = false;
                for target in targets {
                    let mut target = target.lock().await;
                    if target
                        .view
                        .lease
                        .as_ref()
                        .is_some_and(|l| l.lease_id == *lease_id)
                    {
                        let owner = target.view.lease.as_ref().map(|l| l.owner);
                        if !caller.grants.admin
                            && !matches!((caller.principal, owner), (Principal::Device { device_id }, Some(LeaseOwner::Human { device_id: owner })) if device_id == owner)
                        {
                            return Err(error(
                                ErrorCode::PermissionDenied,
                                "Lease belongs to another device",
                            ));
                        }
                        self.revoke_locked(&mut target, false).await?;
                        found = true;
                    }
                }
                if !found {
                    return Err(error(ErrorCode::Expired, "Lease not found"));
                }
            }
            PauseInput { target_id } => {
                let released = self.revoke_target(*target_id, false).await;
                let recorded = self.intervention(
                    *target_id,
                    InterventionKind::PauseInput,
                    "Managed input release requested",
                    released.is_ok(),
                );
                released?;
                recorded?;
            }
            ReturnToAgent { target_id, run_id } => {
                self.revoke_target(*target_id, false).await?;
                self.grant_agent(*run_id, *target_id).await?;
                if let Principal::Device { device_id } = caller.principal {
                    self.target(*target_id).await?.lock().await.supervisor = Some(device_id);
                }
            }
            StartPrivateStep { target_id } => {
                self.start_private_step(*target_id).await?;
            }
            EndPrivateStep { target_id } => {
                let target = self.target(*target_id).await?;
                let mut target = target.lock().await;
                target.view.private_step = None;
                self.save_target(&mut target)?;
            }
            WithdrawObservation { run_id, target_id } => {
                let target = self.target(*target_id).await?;
                let mut target = target.lock().await;
                let o = target
                    .view
                    .model_observation
                    .as_mut()
                    .filter(|o| o.run_id == *run_id)
                    .ok_or_else(|| error(ErrorCode::NotFound, "Observation session not found"))?;
                o.state = ObservationState::Withdrawn;
                self.save_target(&mut target)?;
            }
            LockDesktop => {
                self.voice_mark_all(protocol::voice::VoiceStopCause::ScreenLock)
                    .await;
                let targets: Vec<_> = self.inner.controls.lock().await.keys().copied().collect();
                let mut failure = None;
                for id in targets {
                    keep_worse(&mut failure, self.revoke_target(id, false).await);
                }
                if let Some(failure) = failure {
                    return Err(failure);
                }
                let desktop = self.desktop()?;
                let changes = desktop.lock_changes();
                desktop.lock_session().await.map_err(platform)?;
                self.confirm_locked(&desktop, changes).await?;
            }
            UnlockDesktop { password } => self.unlock_desktop(caller, password).await?,
            CaptureTarget {
                target_id,
                evidence,
            } => {
                if let Some(link) = evidence {
                    self.read(|r| crate::results::validate_device_link(r, link))?;
                }
                let _operation = self.inner.desktop_operation.read().await;
                let backend = self.desktop()?;
                let frame = backend.capture_still(*target_id).await.map_err(platform)?;
                let capture = protocol::model::CaptureGeometry {
                    target_id: *target_id,
                    geometry: frame.geometry,
                    captured_at: self.inner.clock.now(),
                };
                let encoded = self
                    .inner
                    .media
                    .as_ref()
                    .ok_or_else(|| unsupported("Media encoder"))?
                    .encode_still(&frame, ports::StillFormat::Png, None)
                    .map_err(platform)?;
                let artifact = self
                    .put_linked_artifact(
                        &encoded.bytes,
                        encoded.media_type,
                        evidence.clone(),
                        caller.principal,
                        Some(Box::new(capture)),
                    )
                    .await?;
                result = Some(protocol::command::CommandResult::ArtifactCaptured { artifact });
            }
            _ => return Err(error(ErrorCode::InvalidRequest, "Not a control command")),
        }
        Ok(result)
    }
    async fn start_private_step(&self, target_id: TargetId) -> Result<(), ProtocolError> {
        let target = self.target(target_id).await?;
        // Whatever cannot be recorded, the agent is still fenced and paused;
        // input that may still be held is reported ahead of the rest.
        let mut failure = None;
        let (run_id, requested_at) = {
            let mut target = target.lock().await;
            if target.view.private_step.as_ref().is_some_and(|p| p.ready) {
                return Ok(());
            }
            let run_id = target.view.model_observation.as_ref().map(|o| o.run_id);
            // Fence first. No native interrupt or potentially slow cleanup may
            // precede managed input/observation admission closing.
            let input = self.revoke_locked(&mut target, false).await;
            let count = target
                .view
                .model_observation
                .as_ref()
                .map_or(0, |o| o.delivered_count);
            if let Some(o) = &mut target.view.model_observation {
                o.state = ObservationState::Withdrawn;
            }
            let requested_at = self.inner.clock.now();
            target.view.private_step = Some(PrivateStepView {
                requested_at,
                input_stopped: if input.is_ok() {
                    CoverageFact::Acknowledged
                } else {
                    CoverageFact::Unknown
                },
                managed_screenshots_stopped: CoverageFact::Acknowledged,
                uncovered_routes: vec!["The agent’s session pause is not yet confirmed".into()],
                prior_observations_delivered: count,
                ready: false,
            });
            keep_worse(&mut failure, input);
            keep_worse(&mut failure, self.save_target(&mut target));
            (run_id, requested_at)
        };
        let mut elsewhere = false;
        let paused = if let Some(run_id) = run_id {
            let control = self.inner.runs.lock().await.get(&run_id).cloned();
            keep_worse(
                &mut failure,
                self.write(|tx| {
                    tx.put("stopped", run_id, &true)?;
                    tx.delete("run_token", run_id)?;
                    Ok(())
                }),
            );
            if let Some(control) = &control {
                control.close_admission();
            }
            let revoked = self.revoke_run(run_id).await;
            elsewhere = revoked
                .as_ref()
                .is_err_and(|e| e.code == ErrorCode::OutcomeUnknown);
            keep_worse(&mut failure, revoked);
            let confirmed = if let Some(control) = control {
                matches!(
                    tokio::time::timeout(Duration::from_secs(20), control.pause_for_private())
                        .await,
                    Ok(Ok(()))
                )
            } else {
                false
            };
            if confirmed {
                keep_worse(
                    &mut failure,
                    self.write(|tx| {
                        let mut run: Run = tx.read().require("run", run_id)?;
                        for child in &mut run.children {
                            if matches!(child.state, ChildState::Active | ChildState::Unknown) {
                                child.state = ChildState::Stopped;
                                child.ended_at = Some(self.inner.clock.now());
                            }
                        }
                        tx.put("run", run_id, &run)?;
                        tx.emit(Event::RunUpserted { run: Box::new(run) })?;
                        Ok(())
                    }),
                );
                keep_worse(
                    &mut failure,
                    self.finish_run(run_id, RunState::Interrupted, None),
                );
            }
            confirmed
        } else {
            true
        };
        let mut target = target.lock().await;
        // A later EndPrivateStep/Stop cannot be undone by a late acknowledgement.
        let Some(private) = target
            .view
            .private_step
            .as_mut()
            .filter(|p| p.requested_at == requested_at)
        else {
            return failure.map_or(Ok(()), Err);
        };
        private.uncovered_routes = self
            .inner
            .bridge
            .as_ref()
            .map_or_else(Vec::new, |c| c.uncovered_routes.clone());
        if !paused {
            private.uncovered_routes.push("The agent’s native session or a descendant may still be running. Privacy is unverified".into());
        }
        if elsewhere {
            private.uncovered_routes.push("The agent’s input on another screen may not have been released. Privacy is unverified".into());
        }
        private.ready = domain::lease::private_ready(private);
        let ready = private.ready;
        keep_worse(&mut failure, self.save_target(&mut target));
        keep_worse(&mut failure, self.intervention(target_id, InterventionKind::PrivateStep, if ready {
            "Agent input stopped. Screenshots to the agent stopped. The agent's session is paused. Earlier pictures stay in the conversation."
        } else { "Private step requested. Native session pause or input release is unverified." }, ready));
        failure.map_or(Ok(()), Err)
    }

    pub(crate) fn intervention(
        &self,
        target_id: TargetId,
        kind: InterventionKind,
        summary: &str,
        acknowledged: bool,
    ) -> Result<(), ProtocolError> {
        self.write(|tx| {
            tx.emit(Event::InterventionRecorded {
                task_id: None,
                target_id: Some(target_id),
                receipt: Box::new(InterventionReceipt {
                    kind,
                    requested_at: self.inner.clock.now(),
                    facts: vec![ReceiptFact {
                        label: "Input release".into(),
                        status: if acknowledged {
                            FactStatus::Done
                        } else {
                            FactStatus::Unknown
                        },
                        detail: None,
                    }],
                    summary: summary.into(),
                }),
            })?;
            Ok(())
        })
    }
}
pub(crate) fn platform(e: ports::PlatformError) -> ProtocolError {
    error(e.code(), "Desktop backend operation failed")
}

/// Keeps whichever failure matters more: one whose outcome is unknown, such
/// as input that may still be held, ahead of anything that merely went
/// unrecorded; otherwise the first.
pub(crate) fn keep_worse(kept: &mut Option<ProtocolError>, result: Result<(), ProtocolError>) {
    let Err(error) = result else { return };
    let worse = |e: &ProtocolError| e.code == ErrorCode::OutcomeUnknown;
    if kept.as_ref().is_none_or(|k| worse(&error) && !worse(k)) {
        *kept = Some(error);
    }
}

pub(crate) fn release_unconfirmed() -> ProtocolError {
    error(
        ErrorCode::OutcomeUnknown,
        "The computer hasn’t confirmed the agent is paused. Tap Pause agent again before taking control",
    )
}

fn barrier_required() -> ProtocolError {
    error(
        ErrorCode::OutcomeUnknown,
        "Input state is unknown; retry Pause agent to establish the native barrier before requesting control",
    )
}

#[cfg(test)]
#[path = "capture_tests.rs"]
mod capture_tests;
