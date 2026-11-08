// SPDX-License-Identifier: Apache-2.0
//! Voice effects share the existing target mutex, lease deadlines and native barrier.
use crate::{Caller, CompanionHub, error};
use async_trait::async_trait;
use base64::Engine;
use ports::{DesktopBackend, InputSink, MediaEncoder, windows::WindowActions};
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
    Accessibility, Driver, DriverFactory, Error as VoiceError, Interpreter, Launcher, Snapshot,
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
    /// Opens apps and addresses for desktop voice control.
    pub launcher: Arc<dyn Launcher>,
    pub assessor: Arc<dyn ::jev::questions::VoiceAssessor>,
}
struct LiveSession {
    state: AsyncMutex<Session>,
    driver: Arc<dyn Driver>,
    interpreter: Interpreter,
    device: DeviceId,
    target: TargetId,
    detached: std::sync::atomic::AtomicBool,
    /// Set the moment anything begins to stop the session, before it waits
    /// for an action in progress: that action's authority ends here.
    stopping: std::sync::atomic::AtomicBool,
    /// Its stop is not yet recorded. It stays until it is, so the phone
    /// learns why it stopped rather than of a restart.
    unrecorded: std::sync::atomic::AtomicBool,
}
fn mapped(e: VoiceError) -> ProtocolError {
    let code = match e {
        VoiceError::Superseded => ErrorCode::AlreadyResolved,
        VoiceError::StaleTarget => ErrorCode::StaleRevision,
        VoiceError::OutcomeUnknown | VoiceError::LaunchUnstopped => ErrorCode::OutcomeUnknown,
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
    pub(crate) fn voice_config(&self) -> Result<Arc<VoiceConfig>, ProtocolError> {
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
                // Desktop voice control needs the desktop's input. Readable
                // controls only add clicking by name: opening apps, keys,
                // typing and scrolling work without them.
                let desktop = match self.desktop_option() {
                    Some(desktop) => desktop.capabilities().await.input,
                    None => self.desktop_absent(),
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
                let launches = self.target(*target_id).await?.lock().await.launches.clone();
                match backend.input_sink(*target_id).await {
                    Ok(sink) => Ok(Arc::new(DesktopDriver {
                        unfenced: std::sync::atomic::AtomicU64::new(u64::MAX),
                        launches,
                        a11y: config.desktop.clone(),
                        launcher: config.launcher.clone(),
                        windows: self.inner.windows.clone(),
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
                    offer: None,
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
                stopping: Default::default(),
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
            self.revoke_own(&live).await?;
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
                if !*final_segment {
                    // Words still being spoken are shown, never acted on, and
                    // cost nothing: Jev hears the finished command only.
                    let mut state = live.state.lock().await;
                    if state.view.ended.is_some() {
                        return Err(mapped(VoiceError::Stopped));
                    }
                    // Nor do they replace a command being interpreted or
                    // acted on, or one waiting for a reply.
                    if !matches!(
                        state.view.phase,
                        VoicePhase::Assessing
                            | VoicePhase::Acting
                            | VoicePhase::Confirm
                            | VoicePhase::Disambiguation
                    ) {
                        state.view.transcript = text.clone();
                        state.view.phase = VoicePhase::Waiting;
                        state.view.understood = "Listening…".into();
                    }
                    return Ok(Some(CommandResult::VoiceSession {
                        session: Box::new(state.view.clone()),
                    }));
                }
                {
                    let mut state = live.state.lock().await;
                    let prompt = state.view.prompt_id;
                    let spoken = reply(text);
                    let resolved = if matches!(
                        spoken.as_str(),
                        "cancel" | "cancel that" | "no" | "never mind" | "nevermind"
                    ) && matches!(
                        state.view.phase,
                        VoicePhase::Confirm | VoicePhase::Disambiguation
                    ) {
                        state.present(&Decision::Cancel);
                        Some(Decision::Cancel)
                    } else if matches!(
                        spoken.as_str(),
                        "confirm" | "confirmed" | "yes confirm" | "confirm it"
                    ) && state.view.phase == VoicePhase::Confirm
                    {
                        Some(state.confirm(prompt).map_err(mapped)?)
                    } else if state.view.phase == VoicePhase::Disambiguation {
                        let number = match spoken
                            .trim_start_matches("number ")
                            .trim_start_matches("the ")
                            .trim_end_matches(" one")
                        {
                            "1" | "one" | "first" => Some(0),
                            "2" | "two" | "second" => Some(1),
                            "3" | "three" | "third" => Some(2),
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
                let snapshot = tokio::time::timeout(SNAPSHOT_LIMIT, live.driver.snapshot())
                    .await
                    .map_err(|_| mapped(VoiceError::Unavailable(SLOW_SCREEN.into())))?
                    .map_err(mapped)?;
                {
                    let mut state = live.state.lock().await;
                    state
                        .begin(utterance_id, *revision, text.clone(), snapshot.clone())
                        .map_err(mapped)?;
                    self.save_voice(live.device, &state.view)?;
                }
                let answer = live.interpreter.assess(text.clone(), &snapshot).await;
                // An agent is offered only where one could start: this
                // device may start computer use and the computer can run it.
                let agents = self
                    .authorize(caller, |g| g.tasks && g.computer_use)
                    .is_ok()
                    && self.host_capabilities().await.computer_use.is_available();
                let mut state = live.state.lock().await;
                state.agents = agents;
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
                        // Only a refused key needs the person to link Jev
                        // again. A slow, busy or garbled answer fails this
                        // command alone; the next one asks again.
                        if matches!(e, VoiceError::KeyRefused) {
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
                let decision = state.assessed(answer, true);
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
            // The display's lock is held only to check the lease, never while
            // the screen is read or the action runs: renewals and a lock's
            // revocation never wait behind voice control.
            let lock = self.target(live.target).await?;
            {
                let mut target = lock.lock().await;
                self.refresh_geometry(&mut target).await?;
                self.check_voice_lease(caller, live, &mut target).await?;
            }
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
            let fresh = tokio::time::timeout(SNAPSHOT_LIMIT, live.driver.snapshot())
                .await
                .map_err(|_| mapped(VoiceError::Unavailable(SLOW_SCREEN.into())))?
                .map_err(mapped)?;
            let execute = {
                let mut state = live.state.lock().await;
                if state.stamp() != stamp {
                    return Err(mapped(VoiceError::Superseded));
                }
                let claim = state.claim(&action, &fresh);
                self.save_voice(live.device, &state.view)?;
                claim.map_err(mapped)?
            };
            // Authority again now that the screen has been read, and once
            // more inside the driver just before it sends or opens anything.
            let mut refused = None;
            let outcome = if !execute {
                Ok(())
            } else {
                let authority = {
                    let mut target = lock.lock().await;
                    match self.check_voice_lease(caller, live, &mut target).await {
                        Ok(()) => {
                            let lease = live.state.lock().await.view.lease.clone();
                            Some(Arc::new(VoiceAuthority {
                                hub: self.clone(),
                                caller: caller.clone(),
                                lease: target.live.clone(),
                                lease_id: lease.lease_id,
                                generation: lease.generation,
                                live: live.clone(),
                            })
                                as Arc<dyn voice_control::Authority>)
                        }
                        Err(e) => {
                            refused = Some(e);
                            None
                        }
                    }
                };
                match authority {
                    Some(authority) => tokio::time::timeout(
                        act_limit(&action),
                        live.driver.act(&action, &fresh, authority),
                    )
                    .await
                    .unwrap_or(Err(VoiceError::OutcomeUnknown)),
                    None => Err(VoiceError::Stopped),
                }
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
                let _ = self.revoke_own(live).await;
            }
            if let Some(e) = refused {
                return Err(e);
            }
            self.voice_picture(live).await;
        }
        let state = live.state.lock().await;
        self.save_voice(live.device, &state.view)?;
        Ok(Some(CommandResult::VoiceSession {
            session: Box::new(state.view.clone()),
        }))
    }
    /// Lets go of the display only while `live`'s own lease still holds it.
    /// A session stopped meanwhile was fenced by whatever stopped it, and
    /// the display may since belong to another session or device.
    async fn revoke_own(&self, live: &LiveSession) -> Result<(), ProtocolError> {
        let lease = live.state.lock().await.view.lease.clone();
        let lock = self.target(live.target).await?;
        let mut target = lock.lock().await;
        let own = target.view.lease.as_ref().is_some_and(|l| {
            l.lease_id == lease.lease_id
                && l.generation == lease.generation
                && l.boot_epoch == lease.boot_epoch
        });
        if !own {
            // Its lease is gone already; this session still ends.
            drop(target);
            live.stopping
                .store(true, std::sync::atomic::Ordering::SeqCst);
            let mut state = live.state.lock().await;
            state.stop(VoiceStopCause::Stop);
            return self.save_voice(live.device, &state.view);
        }
        self.revoke_locked(&mut target, false).await
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
    /// whether the stops were recorded; a session that could not let go of
    /// input, or whose stop went unrecorded, is left for the next fence to
    /// retry. One left only with a launch it couldn't stop has let go: it
    /// is done, and its launch is the display's mark from here on.
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
            live.stopping
                .store(true, std::sync::atomic::Ordering::SeqCst);
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
                // Its driver let go of input and takes no more actions; only
                // a launch it couldn't stop is left, and the release that
                // called this marks the display with it, which stays until
                // the launch is confirmed stopped. The session is done: releasing it again could
                // only say the same, on every expiry tick.
                Ok(Err(VoiceError::LaunchUnstopped)) => {
                    live.detached
                        .store(true, std::sync::atomic::Ordering::SeqCst);
                    released = released.and(Err(mapped(VoiceError::LaunchUnstopped)));
                }
                Ok(Err(e)) => released = released.and(Err(mapped(e))),
                Err(_) => released = released.and(Err(mapped(VoiceError::OutcomeUnknown))),
            }
        }
        (released, recorded)
    }
    pub(crate) async fn voice_mark_all(&self, cause: VoiceStopCause) {
        if let Ok(sessions) = self.voice_sessions() {
            for live in sessions {
                live.stopping
                    .store(true, std::sync::atomic::Ordering::SeqCst);
                live.state.lock().await.stop(cause);
                live.interpreter.cancel();
            }
        }
    }
    pub(crate) async fn voice_mark_device(&self, id: DeviceId) {
        if let Ok(sessions) = self.voice_sessions() {
            for live in sessions.into_iter().filter(|s| s.device == id) {
                live.stopping
                    .store(true, std::sync::atomic::Ordering::SeqCst);
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
        let mut failure = None;
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
            // One session that can't be let go of never keeps the others
            // from being checked.
            if idle {
                crate::control::keep_worse(&mut failure, self.revoke_own(&live).await);
            }
        }
        failure.map_or(Ok(()), Err)
    }
}
/// How long reading the screen may take, and what is said when it takes
/// longer. Nothing is sent then.
const SNAPSHOT_LIMIT: Duration = Duration::from_millis(2500);
const SLOW_SCREEN: &str =
    "The computer didn’t read the screen in time. Nothing was sent; ask again";
/// Asked by the driver just before it sends or opens anything: the
/// session's lease is the display's active one now, on its deadline as
/// renewed so far, nothing has begun to stop or revoke it, nothing is
/// stopping control, and the device still may send input.
struct VoiceAuthority {
    hub: CompanionHub,
    caller: Caller,
    lease: Arc<crate::control::LiveLease>,
    lease_id: LeaseId,
    generation: u64,
    live: Arc<LiveSession>,
}
impl voice_control::Authority for VoiceAuthority {
    fn check(&self) -> Result<(), VoiceError> {
        let inner = &self.hub.inner;
        if !self
            .lease
            .holds(self.lease_id, self.generation, inner.clock.monotonic())
            || self.live.stopping.load(std::sync::atomic::Ordering::SeqCst)
            || *inner.shutdown.borrow()
            || inner.local_stops.load(std::sync::atomic::Ordering::SeqCst) != 0
            || self.live.detached.load(std::sync::atomic::Ordering::SeqCst)
            || self
                .hub
                .authorize(&self.caller, |g| g.view && g.control_input)
                .is_err()
        {
            return Err(VoiceError::Stopped);
        }
        Ok(())
    }
}
/// A spoken reply to a prompt, as words: "Confirm." is "confirm".
fn reply(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}
/// How long an action may take before its outcome counts as unknown. No
/// display lock is held meanwhile, so renewals never wait on it. Opening an
/// app or address waits for the launcher's own limit, which includes
/// stopping a hung launch: the answer is then what the launch found, never
/// an unknown given while it was still stopping what it started.
///
/// The margin is at least a second, so a launch at its own worst case still
/// answers first; one cut off would count as dropped, and its display would
/// stay marked until restart.
fn act_limit(action: &VoiceAction) -> Duration {
    match action.kind {
        VoiceActionKind::Launch | VoiceActionKind::Navigate => {
            voice_control::LAUNCH_LIMIT.saturating_add(LAUNCH_MARGIN)
        }
        _ => Duration::from_millis(2000),
    }
}
/// How much longer voice control waits for a launch than the launcher may
/// take.
const LAUNCH_MARGIN: Duration = Duration::from_millis(1000);
// With the launcher's own worst case, which platform crates check is
// within `LAUNCH_LIMIT`, a second or more to spare.
const _: () = assert!(LAUNCH_MARGIN.as_millis() >= 1000);
async fn kind_for(live: &LiveSession) -> VoiceTarget {
    live.state.lock().await.view.target
}
struct DesktopDriver {
    /// A launch whose stop couldn't be confirmed: an app may still open, so
    /// the barrier never reports that nothing is left, until the display's
    /// mark is next cleared. Which clear of it came last when the launch
    /// ended, or `u64::MAX` for none.
    unfenced: std::sync::atomic::AtomicU64,
    /// The display's launches in flight, which every release of it waits
    /// for, as it does for an agent's.
    launches: Arc<crate::control::Launches>,
    a11y: Arc<dyn Accessibility>,
    launcher: Arc<dyn Launcher>,
    windows: Option<Arc<dyn WindowActions>>,
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
fn reason(e: VoiceError) -> String {
    match e {
        VoiceError::Unavailable(reason) => reason,
        other => other.to_string(),
    }
}
impl DesktopDriver {
    /// What is on the desktop now. Unreadable controls are a fact about the
    /// app in front, not a failure: commands that need none still work.
    /// The focused window, when the compositor names it, is the context an
    /// action is checked against, so a key meant for one window never goes
    /// to another.
    async fn observe(&self, controls: bool, apps: bool) -> Snapshot {
        let listed = async {
            if apps {
                Some(self.launcher.apps().await)
            } else {
                None
            }
        };
        let seen = async {
            let focused = match &self.windows {
                Some(windows) => windows.focused().await,
                None => None,
            };
            // Where the compositor doesn't say which window has focus
            // (GNOME, KDE, X11, Windows), accessibility's focused window is
            // the context, so its controls are read even for an action
            // aimed at none.
            let read = if controls || focused.is_none() {
                self.a11y.snapshot().await
            } else {
                Err(VoiceError::Unavailable(String::new()))
            };
            (focused, read)
        };
        let (listed, (focused, read)) = tokio::join!(listed, seen);
        let mut snapshot = match read {
            Ok(snapshot) => snapshot,
            Err(e) => Snapshot::unreadable(String::new(), reason(e)),
        };
        if let Some(window) = focused {
            snapshot.context = format!("window:{}:{}", window.id, window.app);
            snapshot.window = Some(VoiceBounds {
                x: window.x,
                y: window.y,
                width: window.width,
                height: window.height,
            });
        }
        if let Some(listed) = listed {
            snapshot.apps = listed.map_err(reason);
        }
        snapshot.desktop = true;
        snapshot
    }
}
impl DesktopDriver {
    /// Opens `app`, or else `url`, counted among the display's launches
    /// from before its final authority check until it has ended, and
    /// remembers one that couldn't be stopped, for the barrier, and the
    /// display with how its launcher can check it again.
    ///
    /// Counted first, a launch is either refused at that check by a release
    /// that has fenced the display, or waited for by it: the release never
    /// says it let go while an app may still open. It runs inside the fenced
    /// driver's own task, so voice control giving up on its answer never
    /// cuts its cleanup short; one that never ends (its task dropped) counts
    /// as unstopped.
    async fn launching(
        &self,
        authority: &dyn voice_control::Authority,
        app: Option<&VoiceApp>,
        url: Option<&str>,
    ) -> Result<(), VoiceError> {
        let guard = self.launches.begin();
        let authority = crate::launch_recheck::Recording::new(authority);
        let outcome = match (app, url) {
            (Some(app), _) => self.launcher.launch(app, &authority).await,
            (None, Some(url)) => self.launcher.open_url(url, &authority).await,
            (None, None) => Err(VoiceError::StaleTarget),
        };
        let unstopped = matches!(outcome, Err(VoiceError::LaunchUnstopped));
        // Read while the launch still counts as running, so no clear of the
        // display's mark can pass between this and its being kept there.
        if unstopped {
            self.unfenced.store(
                self.launches
                    .cleared
                    .load(std::sync::atomic::Ordering::SeqCst),
                std::sync::atomic::Ordering::SeqCst,
            );
        }
        guard.ended(!unstopped, authority.take());
        outcome
    }
}
fn keys(keys: &[&str]) -> InputAction {
    InputAction::KeyChord {
        keys: keys.iter().map(|k| (*k).to_owned()).collect(),
    }
}
#[async_trait]
impl Driver for DesktopDriver {
    async fn snapshot(&self) -> Result<Snapshot, VoiceError> {
        Ok(self.observe(true, true).await)
    }
    async fn act(
        &self,
        action: &VoiceAction,
        old: &Snapshot,
        authority: Arc<dyn voice_control::Authority>,
    ) -> Result<(), VoiceError> {
        match action.kind {
            VoiceActionKind::Launch => {
                let app = action.app.as_ref().ok_or(VoiceError::StaleTarget)?;
                authority.check()?;
                return self.launching(&*authority, Some(app), None).await;
            }
            VoiceActionKind::Navigate => {
                let url = action.literal.as_deref().ok_or(VoiceError::StaleTarget)?;
                authority.check()?;
                return self.launching(&*authority, None, Some(url)).await;
            }
            _ => {}
        }
        // Controls are read again only for an action aimed at one.
        let fresh = self.observe(action.target.is_some(), false).await;
        if !fresh.still_matches(old, action) {
            return Err(VoiceError::StaleTarget);
        }
        // An unknown focus fails closed: two empty contexts are not a match.
        if fresh.context.is_empty() && action.kind != VoiceActionKind::Scroll {
            return Err(VoiceError::Unavailable(
                voice_control::policy::UNKNOWN_FOCUS.into(),
            ));
        }
        let t = self
            .backend
            .targets()
            .await
            .map_err(|_| VoiceError::StaleTarget)?
            .into_iter()
            .find(|t| t.target_id == self.target)
            .ok_or(VoiceError::StaleTarget)?;
        let centre = |b: &VoiceBounds| (b.x + b.width / 2., b.y + b.height / 2.);
        let at = if let Some(e) = &action.target {
            let (x, y) = centre(&e.bounds);
            self.backend.accessibility_point(self.target,t.geometry.revision,x,y).await.map_err(|_|VoiceError::Unavailable("The control’s position cannot be verified on this display. Use the remote keyboard".into()))?
        } else if let Some(window) = &fresh.window {
            // Scrolling reaches the window under the pointer, so aim at the
            // focused one; one on another display falls back to this one's
            // centre.
            let (x, y) = centre(window);
            self.backend
                .accessibility_point(self.target, t.geometry.revision, x, y)
                .await
                .unwrap_or(TargetPoint { x: 0.5, y: 0.5 })
        } else {
            TargetPoint { x: 0.5, y: 0.5 }
        };
        authority.check()?;
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
                // With no readable controls, the text goes to the focused
                // app, as the remote keyboard's would.
                if let Some(e) = &action.target {
                    if e.sensitive {
                        return Err(VoiceError::Unavailable(
                            "Use the remote keyboard for sensitive fields".into(),
                        ));
                    }
                    if !e.focused && !self.a11y.activate(e, true).await? {
                        return Err(VoiceError::StaleTarget);
                    }
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
            VoiceActionKind::PressKey => InputAction::KeyChord {
                // The space bar's one name every desktop's input takes.
                keys: action
                    .literal
                    .as_deref()
                    .and_then(voice_control::keys::keys)
                    .ok_or(VoiceError::StaleTarget)?
                    .into_iter()
                    .map(|k| if k == "Space" { " ".into() } else { k })
                    .collect(),
            },
            // The shortcuts browsers, file managers and most apps share.
            VoiceActionKind::Back => keys(&["alt", "ArrowLeft"]),
            VoiceActionKind::Forward => keys(&["alt", "ArrowRight"]),
            VoiceActionKind::SwitchTab if action.previous_tab => keys(&["ctrl", "shift", "Tab"]),
            VoiceActionKind::SwitchTab => keys(&["ctrl", "Tab"]),
            _ => {
                return Err(VoiceError::Unavailable(
                    "That command is available in Browser voice control".into(),
                ));
            }
        };
        let mut sink = self.sink.lock().await;
        authority.check()?;
        sink.inject(&input, &t.geometry)
            .await
            .map_err(|e| match e {
                ports::PlatformError::OutcomeUnknown(_) => VoiceError::OutcomeUnknown,
                ports::PlatformError::TextUnavailable { .. } => VoiceError::Unavailable(
                    "Some of these characters can’t be typed with the computer’s keyboard layout. Nothing was typed. Use the remote keyboard’s Paste".into(),
                ),
                _ => VoiceError::Unavailable("Desktop input was refused".into()),
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
            .map_err(|_| VoiceError::OutcomeUnknown)?;
        // Unless the display's mark has been cleared since, which proved
        // that launch stopped too.
        let unfenced = self.unfenced.load(std::sync::atomic::Ordering::SeqCst);
        if unfenced != u64::MAX
            && unfenced
                == self
                    .launches
                    .cleared
                    .load(std::sync::atomic::Ordering::SeqCst)
        {
            return Err(VoiceError::LaunchUnstopped);
        }
        Ok(())
    }
    async fn detach(&self) -> Result<(), VoiceError> {
        self.barrier().await
    }
}

#[cfg(test)]
mod tests;
