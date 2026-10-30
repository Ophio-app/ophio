// SPDX-License-Identifier: Apache-2.0
//! Voice effects share the existing target mutex, lease deadlines and native barrier.
use crate::{Caller, CompanionHub, error};
use async_trait::async_trait;
use base64::Engine;
use ports::{DesktopBackend, InputSink, MediaEncoder};
use protocol::{
    auth::Principal, capability::Availability, command::CommandResult, control::*, voice::*, *,
};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::Mutex as AsyncMutex;
use voice_control::{
    Accessibility, Driver, DriverFactory, Error as VoiceError, Interpreter, Snapshot,
    policy::Decision, session::Session,
};

#[derive(Default)]
pub(crate) struct VoiceRuntime {
    config: Mutex<Option<Arc<VoiceConfig>>>,
    sessions: Mutex<HashMap<String, Arc<LiveSession>>>,
}
#[derive(Debug)]
pub struct VoiceConfig {
    pub browser: Arc<dyn DriverFactory>,
    pub desktop: Arc<dyn Accessibility>,
    pub assessor: Arc<dyn ::jev::questions::VoiceAssessor>,
}
struct LiveSession {
    state: AsyncMutex<Session>,
    driver: Arc<dyn Driver>,
    interpreter: Interpreter,
    device: DeviceId,
    target: TargetId,
    detached: std::sync::atomic::AtomicBool,
    /// Its stop is not yet recorded. It stays until it is, so the phone
    /// learns why it stopped rather than of a restart.
    unrecorded: std::sync::atomic::AtomicBool,
}
fn mapped(e: VoiceError) -> ProtocolError {
    let code = match e {
        VoiceError::Superseded => ErrorCode::AlreadyResolved,
        VoiceError::StaleTarget => ErrorCode::StaleRevision,
        VoiceError::OutcomeUnknown => ErrorCode::OutcomeUnknown,
        VoiceError::Stopped => ErrorCode::Expired,
        _ => ErrorCode::TemporarilyUnavailable,
    };
    error(code, e.to_string())
}
impl CompanionHub {
    pub fn configure_voice(&self, config: VoiceConfig) -> Result<(), ProtocolError> {
        *self.inner.voice.config.lock().map_err(crate::internal)? = Some(Arc::new(config));
        Ok(())
    }
    fn voice_config(&self) -> Result<Arc<VoiceConfig>, ProtocolError> {
        self.inner
            .voice
            .config
            .lock()
            .map_err(crate::internal)?
            .clone()
            .ok_or_else(|| error(ErrorCode::Unsupported, "Voice control is not configured"))
    }
    /// Whether Jev is linked here to answer voice commands, checked now. A
    /// failed answer stands until Jev is linked again.
    pub async fn jev_access(&self) -> Result<::jev::Access, ProtocolError> {
        let mut access: ::jev::Access =
            self.read(|r| Ok(r.get("meta", "jev")?.unwrap_or_default()))?;
        let now = match self.voice_config() {
            Ok(config) => config.assessor.connection().await,
            Err(_) => ::jev::Connection::NotConnected,
        };
        if access.connection != ::jev::Connection::LastCheckFailed
            || now != ::jev::Connection::Connected
        {
            access.connection = now;
        }
        Ok(access)
    }
    /// Owner-only runtime hook after changing the protected key file.
    pub fn jev_linked(&self, linked: bool) -> Result<(), ProtocolError> {
        let access = ::jev::Access {
            connection: if linked {
                ::jev::Connection::Connected
            } else {
                ::jev::Connection::NotConnected
            },
            checked_at: Some(self.inner.clock.now()),
        };
        self.write(|tx| tx.put("meta", "jev", &access))
    }
    fn voice_sessions(&self) -> Result<Vec<Arc<LiveSession>>, ProtocolError> {
        Ok(self
            .inner
            .voice
            .sessions
            .lock()
            .map_err(crate::internal)?
            .values()
            .cloned()
            .collect())
    }
    fn voice_session(&self, caller: &Caller, id: &str) -> Result<Arc<LiveSession>, ProtocolError> {
        let s = self
            .inner
            .voice
            .sessions
            .lock()
            .map_err(crate::internal)?
            .get(id)
            .cloned()
            .ok_or_else(|| error(ErrorCode::NotFound, "Voice session is no longer active"))?;
        if caller.principal
            != (Principal::Device {
                device_id: s.device,
            })
        {
            return Err(error(
                ErrorCode::PermissionDenied,
                "Voice session belongs to another device",
            ));
        }
        Ok(s)
    }
    pub(crate) fn voice_device_active(&self, id: DeviceId) -> bool {
        self.inner.voice.sessions.lock().is_ok_and(|sessions| {
            sessions.values().any(|s| {
                s.device == id
                    && s.state
                        .try_lock()
                        .is_ok_and(|state| state.view.ended.is_none())
            })
        })
    }
    pub(crate) fn set_voice_settings(
        &self,
        tx: &mut store::Write<'_>,
        browser: bool,
        desktop: bool,
        revision: Option<u64>,
    ) -> store::Result<()> {
        let mut settings: VoiceSettings =
            tx.read().get("meta", "voice_settings")?.unwrap_or_default();
        domain::revision(revision, settings.revision)?;
        settings.browser = browser;
        settings.desktop = desktop;
        settings.revision += 1;
        tx.put("meta", "voice_settings", &settings)
    }
    pub(crate) async fn voice_view(
        &self,
        caller: &Caller,
        id: Option<&str>,
    ) -> Result<VoiceView, ProtocolError> {
        let settings: VoiceSettings =
            self.read(|r| Ok(r.get("meta", "voice_settings")?.unwrap_or_default()))?;
        let jev = self.jev_access().await?.connection;
        let unavailable = || Availability::Unsupported {
            reason: "Voice control is not configured".into(),
        };
        let (browser, desktop) = if jev == ::jev::Connection::Connected {
            if let Ok(config) = self.voice_config() {
                let browser = config.browser.availability(VoiceTarget::Browser).await;
                let desktop = match config.desktop.snapshot().await {
                    Ok(_) => Availability::Available,
                    Err(e) => Availability::TemporarilyUnavailable {
                        reason: e.to_string(),
                    },
                };
                (browser, desktop)
            } else {
                (unavailable(), unavailable())
            }
        } else {
            (unavailable(), unavailable())
        };
        let session = if let Some(id) = id {
            if let Ok(s) = self.voice_session(caller, id) {
                Some(s.state.lock().await.view.clone())
            } else {
                self.read(|r|{
                let saved:Option<(DeviceId,VoiceSessionView)>=r.get("voice_session",id)?;
                Ok(saved.filter(|(device,_)|caller.principal==(Principal::Device{device_id:*device})).map(|(_,mut v)|{if v.ended.is_none(){v.ended=Some(VoiceStopCause::Restart);v.phase=VoicePhase::Stopped;v.understood="Øphio restarted on the computer. Review before starting a new voice session".into();}v}))
            })?
            }
        } else {
            None
        };
        Ok(VoiceView {
            voice_browser: voice_control::capability(jev, settings.browser, browser),
            voice_desktop: voice_control::capability(jev, settings.desktop, desktop),
            settings,
            speech: self.speech_availability().await,
            session,
        })
    }
    fn save_voice(&self, device: DeviceId, view: &VoiceSessionView) -> Result<(), ProtocolError> {
        self.write(|tx| tx.put("voice_session", &view.session_id, &(device, view)))
    }
    async fn check_voice_lease(
        &self,
        caller: &Caller,
        live: &LiveSession,
        target: &mut crate::control::Target,
    ) -> Result<(), ProtocolError> {
        self.authorize(caller, |g| g.view && g.control_input)?;
        let state = live.state.lock().await;
        if state.view.ended.is_some()
            || state.idle()
            || target.removed
            || target
                .deadline
                .is_none_or(|d| self.inner.clock.monotonic() >= d)
            || target.view.lease.as_ref().is_none_or(|l| {
                l.lease_id != state.view.lease.lease_id
                    || l.generation != state.view.lease.generation
                    || l.boot_epoch != self.inner.epoch
                    || l.state != LeaseState::Active
            })
        {
            return Err(error(ErrorCode::Expired, "Voice control ended"));
        }
        if *self.inner.shutdown.borrow()
            || self
                .inner
                .local_stops
                .load(std::sync::atomic::Ordering::SeqCst)
                != 0
        {
            return Err(error(ErrorCode::Expired, "Computer control is stopping"));
        }
        Ok(())
    }
    pub(crate) async fn voice_command(
        &self,
        caller: &Caller,
        operation: &VoiceOperation,
        command_id: CommandId,
    ) -> Result<Option<CommandResult>, ProtocolError> {
        if let VoiceOperation::Start {
            target,
            target_id,
            geometry_revision,
            dry_run,
            disclosure_accepted,
        } = operation
        {
            if !disclosure_accepted {
                return Err(error(
                    ErrorCode::PermissionDenied,
                    "Read and accept what voice control sends before starting",
                ));
            }
            let Principal::Device { device_id } = caller.principal else {
                return Err(error(
                    ErrorCode::PermissionDenied,
                    "Voice control requires a paired device",
                ));
            };
            let view = self.voice_view(caller, None).await?;
            domain::availability(if *target == VoiceTarget::Browser {
                &view.voice_browser
            } else {
                &view.voice_desktop
            })?;
            let config = self.voice_config()?;
            // Opening only happens inside the explicit Start command. Standard takeover fences its predecessor.
            let Some(CommandResult::LeaseGranted { lease }) = self
                .control_command(
                    caller,
                    &Command::TakeOver {
                        target_id: *target_id,
                        geometry_revision: *geometry_revision,
                    },
                )
                .await?
            else {
                return Err(error(
                    ErrorCode::Internal,
                    "The computer didn’t hand over control",
                ));
            };
            let driver_result = if *target == VoiceTarget::Browser {
                config.browser.open(*target).await
            } else {
                let backend = self.desktop()?;
                match backend.input_sink(*target_id).await {
                    Ok(sink) => Ok(Arc::new(DesktopDriver {
                        a11y: config.desktop.clone(),
                        backend,
                        sink: AsyncMutex::new(sink),
                        target: *target_id,
                        encoder: self.inner.media.clone(),
                    }) as Arc<dyn Driver>),
                    Err(_) => Err(VoiceError::Unavailable(
                        "Desktop input is unavailable".into(),
                    )),
                }
            };
            let driver = match driver_result {
                Ok(d) => d,
                Err(e) => {
                    let _ = self.revoke_target(*target_id, false).await;
                    return Err(mapped(e));
                }
            };
            let state = Session::new(
                VoiceSessionView {
                    session_id: command_id.to_string(),
                    target: *target,
                    target_id: *target_id,
                    lease,
                    dry_run: *dry_run,
                    phase: VoicePhase::Ready,
                    transcript: String::new(),
                    understood: if *dry_run {
                        "Preview commands before sending input"
                    } else {
                        "Ready for a command"
                    }
                    .into(),
                    prompt_id: 0,
                    choices: vec![],
                    picture: None,
                    steps: vec![],
                    ended: None,
                },
                self.inner.clock.clone(),
            );
            let live = Arc::new(LiveSession {
                state: AsyncMutex::new(state),
                driver: voice_control::FencedDriver::new(driver),
                interpreter: Interpreter::new(config.assessor.clone()),
                device: device_id,
                target: *target_id,
                detached: Default::default(),
                unrecorded: Default::default(),
            });
            let lock = self.target(*target_id).await?;
            let mut target_lock = lock.lock().await;
            if let Err(e) = self
                .check_voice_lease(caller, &live, &mut target_lock)
                .await
            {
                let _ = live.driver.detach().await;
                return Err(e);
            }
            self.inner
                .voice
                .sessions
                .lock()
                .map_err(crate::internal)?
                .insert(command_id.to_string(), live.clone());
            self.inner.expiry.notify_one();
            drop(target_lock);
            self.refresh_local_indicator().await?;
            self.voice_picture(&live).await;
            let state = live.state.lock().await;
            self.save_voice(device_id, &state.view)?;
            return Ok(Some(CommandResult::VoiceSession {
                session: Box::new(state.view.clone()),
            }));
        }
        let id = match operation {
            VoiceOperation::Transcript { session_id, .. }
            | VoiceOperation::Choose { session_id, .. }
            | VoiceOperation::Confirm { session_id, .. }
            | VoiceOperation::Cancel { session_id, .. }
            | VoiceOperation::Stop { session_id } => session_id,
            _ => unreachable!(),
        };
        let live = self.voice_session(caller, id)?;
        if matches!(operation, VoiceOperation::Stop { .. }) {
            self.revoke_target(live.target, false).await?;
            return Ok(None);
        }
        let lock = self.target(live.target).await?;
        {
            let mut target = lock.lock().await;
            self.check_voice_lease(caller, &live, &mut target).await?;
        }
        let available = self.voice_view(caller, None).await?;
        let kind = live.state.lock().await.view.target;
        domain::availability(if kind == VoiceTarget::Browser {
            &available.voice_browser
        } else {
            &available.voice_desktop
        })?;
        let (decision, stamp) = match operation {
            VoiceOperation::Transcript {
                utterance_id,
                revision,
                text,
                final_segment,
                ..
            } => {
                if text.len() > 8192 || utterance_id.len() > 128 || utterance_id.is_empty() {
                    return Err(error(
                        ErrorCode::LimitExceeded,
                        "Voice command is too large",
                    ));
                }
                if *final_segment {
                    let mut state = live.state.lock().await;
                    let prompt = state.view.prompt_id;
                    let spoken = text.trim().to_lowercase();
                    let resolved = if spoken == "cancel"
                        && matches!(
                            state.view.phase,
                            VoicePhase::Confirm | VoicePhase::Disambiguation
                        ) {
                        state.present(&Decision::Cancel);
                        Some(Decision::Cancel)
                    } else if spoken == "confirm" && state.view.phase == VoicePhase::Confirm {
                        Some(state.confirm(prompt).map_err(mapped)?)
                    } else if state.view.phase == VoicePhase::Disambiguation {
                        let number = match spoken.as_str() {
                            "1" | "one" => Some(0),
                            "2" | "two" => Some(1),
                            "3" | "three" => Some(2),
                            _ => None,
                        };
                        if let Some(id) = number
                            .and_then(|n| state.view.choices.get(n))
                            .map(|e| e.id.clone())
                        {
                            Some(state.choose(prompt, &id).map_err(mapped)?)
                        } else {
                            None
                        }
                    } else {
                        None
                    };
                    if let Some(d) = resolved {
                        let stamp = state.stamp();
                        drop(state);
                        return self.voice_decision(caller, &live, d, stamp).await;
                    }
                }
                live.interpreter.cancel();
                let snapshot = live.driver.snapshot().await.map_err(mapped)?;
                {
                    let mut state = live.state.lock().await;
                    state
                        .begin(utterance_id, *revision, text.clone(), snapshot.clone())
                        .map_err(mapped)?;
                    self.save_voice(live.device, &state.view)?;
                }
                let answer = live.interpreter.assess(text.clone(), &snapshot).await;
                let mut state = live.state.lock().await;
                if state.view.ended.is_some()
                    || state.utterance != *utterance_id
                    || state.revision != *revision
                {
                    state.superseded(utterance_id, text.clone(), answer.ok());
                    self.save_voice(live.device, &state.view)?;
                    return Err(mapped(VoiceError::Superseded));
                }
                let answer = match answer {
                    Ok(a) => a,
                    Err(e) => {
                        if matches!(e, VoiceError::Timeout | VoiceError::Assessment) {
                            let access = ::jev::Access {
                                connection: ::jev::Connection::LastCheckFailed,
                                checked_at: Some(self.inner.clock.now()),
                            };
                            self.write(|tx| tx.put("meta", "jev", &access))?;
                        }
                        state.fail(&e);
                        self.save_voice(live.device, &state.view)?;
                        return Err(mapped(e));
                    }
                };
                let mut decision = state.assessed(answer, *final_segment);
                if matches!(decision, Decision::Wait) && !final_segment {
                    let remaining = state.silence_remaining();
                    drop(state);
                    tokio::time::sleep(remaining).await;
                    state = live.state.lock().await;
                    if state.view.ended.is_some()
                        || state.utterance != *utterance_id
                        || state.revision != *revision
                    {
                        return Err(mapped(VoiceError::Superseded));
                    }
                    decision = state.evaluate(false);
                }
                self.save_voice(live.device, &state.view)?;
                (decision, state.stamp())
            }
            VoiceOperation::Choose {
                prompt_id,
                element_id,
                ..
            } => {
                let mut state = live.state.lock().await;
                let decision = state.choose(*prompt_id, element_id).map_err(mapped)?;
                (decision, state.stamp())
            }
            VoiceOperation::Confirm { prompt_id, .. } => {
                let mut state = live.state.lock().await;
                let decision = state.confirm(*prompt_id).map_err(mapped)?;
                (decision, state.stamp())
            }
            VoiceOperation::Cancel { prompt_id, .. } => {
                let mut state = live.state.lock().await;
                if state.view.prompt_id != *prompt_id {
                    return Err(mapped(VoiceError::StaleTarget));
                }
                state.present(&Decision::Cancel);
                (Decision::Cancel, state.stamp())
            }
            _ => {
                return Err(error(
                    ErrorCode::InvalidRequest,
                    "Unsupported voice command",
                ));
            }
        };
        self.voice_decision(caller, &live, decision, stamp).await
    }
    async fn voice_decision(
        &self,
        caller: &Caller,
        live: &Arc<LiveSession>,
        decision: Decision,
        stamp: (String, u64, u64),
    ) -> Result<Option<CommandResult>, ProtocolError> {
        if let Decision::Act(action) = decision {
            let lock = self.target(live.target).await?;
            let mut target = lock.lock().await;
            self.refresh_geometry(&mut target).await?;
            self.check_voice_lease(caller, live, &mut target).await?;
            {
                let state = live.state.lock().await;
                if state.stamp() != stamp
                    || state.pending.as_ref() != Some(&action)
                    || matches!(
                        state.view.phase,
                        VoicePhase::Assessing | VoicePhase::Waiting
                    )
                {
                    return Err(mapped(VoiceError::Superseded));
                }
            }
            self.voice_gate(kind_for(live).await).await?;
            let fresh = live.driver.snapshot().await.map_err(mapped)?;
            let execute = {
                let mut state = live.state.lock().await;
                if state.stamp() != stamp {
                    return Err(mapped(VoiceError::Superseded));
                }
                let claim = state.claim(&action, &fresh);
                self.save_voice(live.device, &state.view)?;
                claim.map_err(mapped)?
            };
            let outcome = if execute {
                tokio::time::timeout(Duration::from_millis(500), live.driver.act(&action, &fresh))
                    .await
                    .unwrap_or(Err(VoiceError::OutcomeUnknown))
            } else {
                Ok(())
            };
            let failed = outcome.is_err();
            let after = if failed || !execute {
                None
            } else {
                tokio::time::timeout(Duration::from_millis(500), live.driver.snapshot())
                    .await
                    .ok()
                    .and_then(Result::ok)
            };
            let ended = {
                let mut state = live.state.lock().await;
                state.complete(&action, outcome, after);
                self.save_voice(live.device, &state.view)?;
                state.view.ended.is_some()
            };
            if failed || ended {
                let _ = self.revoke_locked(&mut target, false).await;
            }
            drop(target);
            self.voice_picture(live).await;
        }
        let state = live.state.lock().await;
        self.save_voice(live.device, &state.view)?;
        Ok(Some(CommandResult::VoiceSession {
            session: Box::new(state.view.clone()),
        }))
    }
    async fn voice_picture(&self, live: &LiveSession) {
        if let Ok(Some(png_base64)) = live.driver.picture().await
            && png_base64.len() <= 384 * 1024
        {
            let mut state = live.state.lock().await;
            state.view.picture = Some(VoicePicture {
                png_base64,
                at: self.inner.clock.now(),
            });
        }
    }
    /// Stops and lets go of every voice session on `target`, whatever cannot
    /// be recorded. The first result says whether input was let go, the second
    /// whether the stops were recorded; a session that could not let go, or
    /// whose stop went unrecorded, is left for the next fence to retry.
    pub(crate) async fn voice_fence_target(
        &self,
        target: TargetId,
        expired: bool,
    ) -> (Result<(), ProtocolError>, Result<(), ProtocolError>) {
        let (mut released, mut recorded) = (Ok(()), Ok(()));
        let sessions = match self.voice_sessions() {
            Ok(sessions) => sessions,
            Err(e) => return (Err(e), Ok(())),
        };
        for live in sessions.into_iter().filter(|s| {
            s.target == target
                && (!s.detached.load(std::sync::atomic::Ordering::SeqCst)
                    || s.unrecorded.load(std::sync::atomic::Ordering::SeqCst))
        }) {
            let detached = live.detached.load(std::sync::atomic::Ordering::SeqCst);
            live.interpreter.cancel();
            {
                let mut state = live.state.lock().await;
                state.stop(if expired {
                    VoiceStopCause::Heartbeat
                } else if *self.inner.shutdown.borrow() {
                    VoiceStopCause::Shutdown
                } else if self
                    .inner
                    .local_stops
                    .load(std::sync::atomic::Ordering::SeqCst)
                    > 0
                {
                    VoiceStopCause::LocalStop
                } else {
                    VoiceStopCause::Stop
                });
                let saved = self.save_voice(live.device, &state.view);
                live.unrecorded
                    .store(saved.is_err(), std::sync::atomic::Ordering::SeqCst);
                recorded = recorded.and(saved);
            }
            if detached {
                continue;
            }
            match tokio::time::timeout(Duration::from_secs(2), async {
                live.driver.barrier().await?;
                live.driver.detach().await
            })
            .await
            {
                Ok(Ok(())) => live
                    .detached
                    .store(true, std::sync::atomic::Ordering::SeqCst),
                Ok(Err(e)) => released = released.and(Err(mapped(e))),
                Err(_) => released = released.and(Err(mapped(VoiceError::OutcomeUnknown))),
            }
        }
        (released, recorded)
    }
    pub(crate) async fn voice_mark_all(&self, cause: VoiceStopCause) {
        if let Ok(sessions) = self.voice_sessions() {
            for live in sessions {
                live.state.lock().await.stop(cause);
                live.interpreter.cancel();
            }
        }
    }
    pub(crate) async fn voice_mark_device(&self, id: DeviceId) {
        if let Ok(sessions) = self.voice_sessions() {
            for live in sessions.into_iter().filter(|s| s.device == id) {
                live.state.lock().await.stop(VoiceStopCause::Revoked);
                live.interpreter.cancel();
            }
        }
    }
    async fn voice_gate(&self, kind: VoiceTarget) -> Result<(), ProtocolError> {
        let settings: VoiceSettings =
            self.read(|r| Ok(r.get("meta", "voice_settings")?.unwrap_or_default()))?;
        domain::availability(&voice_control::capability(
            self.jev_access().await?.connection,
            if kind == VoiceTarget::Browser {
                settings.browser
            } else {
                settings.desktop
            },
            Availability::Available,
        ))
    }
    /// Whether a voice session is left for the expiry tick to check.
    pub(crate) fn voice_pending(&self) -> bool {
        self.inner
            .voice
            .sessions
            .lock()
            .map_or(true, |sessions| !sessions.is_empty())
    }
    pub(crate) async fn expire_voice(&self) -> Result<(), ProtocolError> {
        use std::sync::atomic::Ordering::SeqCst;
        // A stop that went unrecorded is tried again each tick; the fence
        // that stopped it has already reported the failure.
        for live in self.voice_sessions()? {
            if live.detached.load(SeqCst) && live.unrecorded.load(SeqCst) {
                let state = live.state.lock().await;
                if self.save_voice(live.device, &state.view).is_ok() {
                    live.unrecorded.store(false, SeqCst);
                }
            }
        }
        self.inner
            .voice
            .sessions
            .lock()
            .map_err(crate::internal)?
            .retain(|_, live| !live.detached.load(SeqCst) || live.unrecorded.load(SeqCst));
        for live in self.voice_sessions()? {
            if live.detached.load(std::sync::atomic::Ordering::SeqCst) {
                continue;
            }
            let lost = self.voice_gate(kind_for(&live).await).await.is_err();
            let idle = {
                let mut state = live.state.lock().await;
                if state.view.ended.is_none() && (state.idle() || lost) {
                    state.stop(if lost {
                        VoiceStopCause::CapabilityLost
                    } else {
                        VoiceStopCause::Idle
                    });
                    true
                } else {
                    state.view.ended.is_some()
                }
            };
            if idle {
                self.revoke_target(live.target, false).await?;
            }
        }
        Ok(())
    }
}
async fn kind_for(live: &LiveSession) -> VoiceTarget {
    live.state.lock().await.view.target
}
struct DesktopDriver {
    a11y: Arc<dyn Accessibility>,
    backend: Arc<dyn DesktopBackend>,
    sink: AsyncMutex<Box<dyn InputSink>>,
    target: TargetId,
    encoder: Option<Arc<dyn MediaEncoder>>,
}
impl std::fmt::Debug for DesktopDriver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DesktopVoiceDriver")
    }
}
#[async_trait]
impl Driver for DesktopDriver {
    async fn snapshot(&self) -> Result<Snapshot, VoiceError> {
        self.a11y.snapshot().await
    }
    async fn act(&self, action: &VoiceAction, old: &Snapshot) -> Result<(), VoiceError> {
        let fresh = self.a11y.snapshot().await?;
        if !fresh.still_matches(old, action) {
            return Err(VoiceError::StaleTarget);
        }
        let t = self
            .backend
            .targets()
            .await
            .map_err(|_| VoiceError::StaleTarget)?
            .into_iter()
            .find(|t| t.target_id == self.target)
            .ok_or(VoiceError::StaleTarget)?;
        let at = if let Some(e) = &action.target {
            self.backend.accessibility_point(self.target,t.geometry.revision,e.bounds.x+e.bounds.width/2.,e.bounds.y+e.bounds.height/2.).await.map_err(|_|VoiceError::Unavailable("The control’s position cannot be verified on this display. Use the remote keyboard".into()))?
        } else {
            TargetPoint { x: 0.5, y: 0.5 }
        };
        if let Some(e) = &action.target
            && matches!(action.kind, VoiceActionKind::Click | VoiceActionKind::Focus)
            && self
                .a11y
                .activate(e, action.kind == VoiceActionKind::Focus)
                .await?
        {
            return Ok(());
        }
        let input = match action.kind {
            VoiceActionKind::Click | VoiceActionKind::Focus => InputAction::Click {
                button: PointerButton::Left,
                at,
                count: 1,
            },
            VoiceActionKind::Type => {
                let e = action.target.as_ref().ok_or(VoiceError::StaleTarget)?;
                if e.sensitive {
                    return Err(VoiceError::Unavailable(
                        "Use the remote keyboard for sensitive fields".into(),
                    ));
                }
                if !e.focused && !self.a11y.activate(e, true).await? {
                    return Err(VoiceError::StaleTarget);
                }
                InputAction::Text {
                    text: action.literal.clone().ok_or(VoiceError::StaleTarget)?,
                }
            }
            VoiceActionKind::Scroll => InputAction::Scroll {
                at,
                dx: 0.,
                dy: action.scroll_y.into(),
            },
            VoiceActionKind::PressKey => {
                let literal = action
                    .literal
                    .as_deref()
                    .filter(|key| voice_control::policy::allowed_key(key))
                    .ok_or(VoiceError::StaleTarget)?;
                let key = match literal {
                    "enter" => "Enter",
                    "tab" => "Tab",
                    "escape" => "Escape",
                    "delete" => "Delete",
                    "backspace" => "Backspace",
                    other => other,
                };
                InputAction::KeyChord {
                    keys: vec![key.into()],
                }
            }
            _ => {
                return Err(VoiceError::Unavailable(
                    "That command is available in Browser voice control".into(),
                ));
            }
        };
        self.sink
            .lock()
            .await
            .inject(&input, &t.geometry)
            .await
            .map_err(|e| {
                if matches!(e, ports::PlatformError::OutcomeUnknown(_)) {
                    VoiceError::OutcomeUnknown
                } else {
                    VoiceError::Unavailable("Desktop input was refused".into())
                }
            })
    }
    async fn picture(&self) -> Result<Option<String>, VoiceError> {
        let Some(encoder) = &self.encoder else {
            return Ok(None);
        };
        let frame = self
            .backend
            .capture_still(self.target)
            .await
            .map_err(|_| VoiceError::Unavailable("Desktop picture is unavailable".into()))?;
        let image = encoder
            .encode_still(&frame, ports::StillFormat::Png, Some(1000))
            .map_err(|_| VoiceError::Unavailable("Desktop picture is unavailable".into()))?;
        Ok(Some(
            base64::engine::general_purpose::STANDARD.encode(image.bytes),
        ))
    }
    async fn barrier(&self) -> Result<(), VoiceError> {
        self.sink
            .lock()
            .await
            .release_all()
            .await
            .map_err(|_| VoiceError::OutcomeUnknown)
    }
    async fn detach(&self) -> Result<(), VoiceError> {
        self.barrier().await
    }
}

#[cfg(test)]
mod tests;
