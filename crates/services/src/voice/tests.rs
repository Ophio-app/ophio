// SPDX-License-Identifier: Apache-2.0
use super::*;
use crate::{
    Hub,
    testing::{FakeClock, FakeDesktop},
};
use ports::{Clock, desktop::LocalIndicator};
use protocol::auth::PairRequest;
use std::{
    collections::BTreeMap,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
};
/// A gate with no permits until a test adds them.
#[derive(Debug)]
struct Hold(tokio::sync::Semaphore);
impl Default for Hold {
    fn default() -> Self {
        Self(tokio::sync::Semaphore::new(0))
    }
}
#[derive(Debug, Default)]
struct Fake {
    actions: AtomicU64,
    detaches: AtomicU64,
    connected: AtomicBool,
    unfenced: AtomicBool,
    /// The app in front exposes no controls, as on Hyprland.
    unreadable: AtomicBool,
    /// What Jev answers: the intent and how consequential. Click otherwise.
    script: Mutex<Option<(String, f64)>>,
    /// Jev fails this way instead of answering.
    jev_error: Mutex<Option<::jev::Error>>,
    assessed: AtomicU64,
    /// Jev's answer waits for a permit here while `holding` is set.
    holding: AtomicBool,
    hold: Hold,
    /// The browser driver's action waits here while `acting` is set.
    acting: AtomicBool,
    act_gate: Hold,
    entered: AtomicU64,
    /// Driver snapshots taken, and the one at which the lease lapses.
    snapshots: AtomicU64,
    lapse_at: AtomicU64,
    clock: Mutex<Option<Arc<FakeClock>>>,
    /// Milliseconds the launcher takes to get ready before it starts an app.
    slow_launch: AtomicU64,
    /// The launcher can't confirm a hung launch was stopped.
    unstoppable: AtomicBool,
    /// How it says such a launch can be checked again, if it can.
    check: Mutex<Option<Arc<FakeCheck>>>,
    /// Milliseconds a started launch hangs, its helper running, before it
    /// has stopped it (or found it can't).
    hang_ms: AtomicU64,
    /// Launches whose helper started, and those that went on to finish
    /// their own cleanup.
    helpers: AtomicU64,
    cleaned: AtomicU64,
    launched: Mutex<Vec<String>>,
    opened: Mutex<Vec<String>>,
}
impl Fake {
    fn answer(&self, intent: &str, destructive: f64) {
        *self.script.lock().unwrap() = Some((intent.into(), destructive));
    }
}
#[async_trait]
impl Launcher for Fake {
    async fn apps(&self) -> Result<Vec<voice_control::apps::LaunchApp>, VoiceError> {
        use voice_control::apps::{AppRole, LaunchApp};
        let app = |id: &str, name: &str, role| LaunchApp {
            id: id.into(),
            name: name.into(),
            aliases: vec![],
            roles: vec![role],
            defaults: vec![role],
            listed: true,
            detail: None,
        };
        Ok(vec![
            app("chromium.desktop", "Chromium", AppRole::Browser),
            app("Alacritty.desktop", "Alacritty", AppRole::Terminal),
        ])
    }
    async fn launch(
        &self,
        app: &VoiceApp,
        authority: &dyn voice_control::Authority,
    ) -> Result<(), VoiceError> {
        // Getting ready to start the app (reading entries, the session's
        // environment) can take a while.
        let slow = self.slow_launch.load(Ordering::SeqCst);
        if slow > 0
            && let Some(clock) = self.clock.lock().unwrap().as_ref()
        {
            clock.advance(Duration::from_millis(slow));
        }
        // As the real launcher does: last, just before the app starts.
        authority.check()?;
        self.helpers.fetch_add(1, Ordering::SeqCst);
        let hang = self.hang_ms.load(Ordering::SeqCst);
        if hang > 0 {
            tokio::time::sleep(Duration::from_millis(hang)).await;
            self.cleaned.fetch_add(1, Ordering::SeqCst);
        }
        if self.unstoppable.load(Ordering::SeqCst) {
            if let Some(check) = self.check.lock().unwrap().clone() {
                authority.unstopped(check);
            }
            return Err(VoiceError::LaunchUnstopped);
        }
        self.launched.lock().unwrap().push(app.id.clone());
        Ok(())
    }
    async fn open_url(
        &self,
        url: &str,
        authority: &dyn voice_control::Authority,
    ) -> Result<(), VoiceError> {
        authority.check()?;
        self.opened.lock().unwrap().push(url.into());
        Ok(())
    }
}
/// A launch's scope as a stand-in systemd answers for it.
#[derive(Default)]
struct FakeCheck {
    /// systemd says the scope is inactive; otherwise it fails, or says the
    /// app is running.
    stopped: AtomicBool,
    calls: AtomicU64,
    stops: AtomicU64,
    /// The display it marked, and how often its lock was held during a
    /// check.
    display: Mutex<Option<Arc<AsyncMutex<crate::control::Target>>>>,
    locked: AtomicU64,
}
impl std::fmt::Debug for FakeCheck {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("FakeCheck")
    }
}
#[async_trait]
impl ports::LaunchCheck for FakeCheck {
    async fn stopped(&self, stop: bool) -> bool {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if stop {
            self.stops.fetch_add(1, Ordering::SeqCst);
        }
        if let Some(display) = self.display.lock().unwrap().as_ref()
            && display.try_lock().is_err()
        {
            self.locked.fetch_add(1, Ordering::SeqCst);
        }
        self.stopped.load(Ordering::SeqCst)
    }
}
/// The compositor's focused window, as Hyprland reports it.
#[derive(Debug)]
struct Focus;
#[async_trait]
impl ports::windows::WindowActions for Focus {
    async fn capabilities(&self) -> protocol::windows::WindowCapabilities {
        protocol::windows::WindowCapabilities {
            backend: "synthetic".into(),
            windows: Availability::Available,
            open_apps: Availability::Available,
        }
    }
    async fn catalog(
        &self,
        _: &str,
    ) -> Result<protocol::windows::WindowCatalog, ports::PlatformError> {
        Err(ports::PlatformError::Unsupported("synthetic".into()))
    }
    async fn act(
        &self,
        _: &protocol::windows::WindowAction,
        _: Arc<dyn ports::windows::WindowAuthority>,
    ) -> Result<(protocol::windows::WindowOutcome, String), ports::PlatformError> {
        Err(ports::PlatformError::Unsupported("synthetic".into()))
    }
    async fn focused(&self) -> Option<ports::windows::FocusedWindow> {
        Some(ports::windows::FocusedWindow {
            id: "0x1234".into(),
            app: "Alacritty".into(),
            x: 0.,
            y: 0.,
            width: 16.,
            height: 24.,
        })
    }
}
fn observation() -> Snapshot {
    Snapshot::new(
        vec![VoiceElement {
            id: "e1".into(),
            role: "button".into(),
            name: "Open".into(),
            bounds: VoiceBounds {
                x: 1.,
                y: 1.,
                width: 10.,
                height: 10.,
            },
            visible: true,
            enabled: true,
            focused: true,
            sensitive: false,
            destructive: false,
        }],
        "window".into(),
    )
}
#[async_trait]
impl Driver for Fake {
    async fn snapshot(&self) -> Result<Snapshot, VoiceError> {
        let n = self.snapshots.fetch_add(1, Ordering::SeqCst) + 1;
        if n == self.lapse_at.load(Ordering::SeqCst)
            && let Some(clock) = self.clock.lock().unwrap().as_ref()
        {
            // Reading the screen took longer than the lease lasts.
            clock.advance(Duration::from_secs(4));
        }
        Ok(observation())
    }
    async fn act(
        &self,
        _: &VoiceAction,
        _: &Snapshot,
        authority: Arc<dyn voice_control::Authority>,
    ) -> Result<(), VoiceError> {
        self.entered.fetch_add(1, Ordering::SeqCst);
        if self.acting.load(Ordering::SeqCst) {
            self.act_gate.0.acquire().await.unwrap().forget();
        }
        // A driver asks last of all, just before it sends anything.
        authority.check()?;
        self.actions.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    async fn picture(&self) -> Result<Option<String>, VoiceError> {
        Ok(None)
    }
    async fn barrier(&self) -> Result<(), VoiceError> {
        if self.unfenced.load(Ordering::SeqCst) {
            return Err(VoiceError::OutcomeUnknown);
        }
        Ok(())
    }
    async fn detach(&self) -> Result<(), VoiceError> {
        self.detaches.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}
#[async_trait]
impl Accessibility for Fake {
    async fn snapshot(&self) -> Result<Snapshot, VoiceError> {
        if self.unreadable.load(Ordering::SeqCst) {
            return Err(VoiceError::Unavailable(
                "No app on the computer that exposes its controls has focus".into(),
            ));
        }
        Ok(observation())
    }
    async fn activate(&self, _: &VoiceElement, _: bool) -> Result<bool, VoiceError> {
        Ok(true)
    }
}
#[derive(Debug)]
struct Factory(Arc<Fake>);
#[async_trait]
impl DriverFactory for Factory {
    async fn availability(&self, _: VoiceTarget) -> Availability {
        Availability::Available
    }
    async fn open(&self, _: VoiceTarget) -> Result<Arc<dyn Driver>, VoiceError> {
        Ok(self.0.clone())
    }
}
#[async_trait]
impl ::jev::questions::VoiceAssessor for Fake {
    async fn connection(&self) -> ::jev::Connection {
        if self.connected.load(Ordering::SeqCst) {
            ::jev::Connection::Connected
        } else {
            ::jev::Connection::NotConnected
        }
    }
    async fn assess_voice(
        &self,
        _: &::jev::questions::VoiceRequest,
    ) -> Result<::jev::questions::VoiceAnswer, ::jev::Error> {
        self.assessed.fetch_add(1, Ordering::SeqCst);
        if self.holding.load(Ordering::SeqCst) {
            self.hold.0.acquire().await.unwrap().forget();
        }
        if let Some(e) = *self.jev_error.lock().unwrap() {
            return Err(e);
        }
        let (intent, destructive) = self
            .script
            .lock()
            .unwrap()
            .clone()
            .unwrap_or(("click".into(), 0.));
        let choice = |label: &str| VoiceChoice {
            choice: label.into(),
            confidence: 1.,
            probabilities: BTreeMap::from([(label.into(), 1.)]),
        };
        let score = |n| VoiceScore {
            score: n,
            confidence: 1.,
            probabilities: BTreeMap::from([("0".into(), 1. - n), ("1".into(), n)]),
        };
        Ok(::jev::questions::VoiceAnswer {
            answers: VoiceAnswers {
                intent: choice(&intent),
                target: choice(if intent == "click" { "e1" } else { "none" }),
                complete: score(1.),
                is_command: score(1.),
                destructive: score(destructive),
                is_correction: score(0.),
            },
            usage: JevUsage {
                input_tokens: Some(2),
                output_tokens: Some(1),
            },
        })
    }
}
#[derive(Default)]
struct Indicator(Mutex<String>);
#[async_trait]
impl LocalIndicator for Indicator {
    async fn show(&self, text: &str) -> Result<(), ports::PlatformError> {
        *self.0.lock().unwrap() = text.into();
        Ok(())
    }
    async fn hide(&self) -> Result<(), ports::PlatformError> {
        self.0.lock().unwrap().clear();
        Ok(())
    }
}
struct Fixture {
    path: std::path::PathBuf,
    hub: CompanionHub,
    caller: Caller,
    clock: Arc<FakeClock>,
    fake: Arc<Fake>,
    target: TargetId,
    indicator: Arc<Indicator>,
    desktop: Arc<FakeDesktop>,
}
impl Fixture {
    async fn new() -> Self {
        Self::build(false, true).await
    }
    /// A desktop whose compositor doesn't say which window has focus:
    /// GNOME, KDE, X11 and Windows.
    async fn without_focus() -> Self {
        Self::build(false, false).await
    }
    /// With computer use available: the tool bridge, pictures and the local
    /// Stop indicator.
    async fn with_computer_use() -> Self {
        Self::build(true, true).await
    }
    async fn build(computer_use: bool, focus: bool) -> Self {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/voice-service-tests")
            .join(CommandId::new().to_string());
        let fake = Arc::new(Fake::default());
        fake.connected.store(true, Ordering::SeqCst);
        let clock = Arc::new(FakeClock::default());
        *fake.clock.lock().unwrap() = Some(clock.clone());
        let desktop = Arc::new(FakeDesktop::default());
        let target = desktop.target.lock().unwrap().target_id;
        let mut builder = CompanionHub::builder()
            .data_dir(&path)
            .clock(clock.clone())
            .desktop(desktop.clone());
        if focus {
            builder = builder.window_actions(Arc::new(Focus));
        }
        if computer_use {
            builder = builder.media(Arc::new(crate::testing::FakeMedia)).bridge(
                crate::bridge::BridgeConfig::new("synthetic-bridge", "synthetic-socket"),
            );
        }
        let hub = builder.build().await.unwrap();
        let indicator = Arc::new(Indicator::default());
        hub.set_local_indicator(Some(indicator.clone())).await;
        hub.configure_voice(VoiceConfig {
            browser: Arc::new(Factory(fake.clone())),
            desktop: fake.clone(),
            launcher: fake.clone(),
            assessor: fake.clone(),
        })
        .unwrap();
        hub.jev_linked(true).unwrap();
        let offer = hub.open_pairing(&Caller::local_owner()).await.unwrap();
        let phone = crate::testing::pair_confirmed(
            &hub,
            PairRequest {
                pairing_secret: offer.secret,
                device_public_key: base64::engine::general_purpose::URL_SAFE_NO_PAD.encode([7; 32]),
                device_name: "Test phone".into(),
                device_platform: "android".into(),
                client_version: "test".into(),
                protocol: ProtocolVersion::CURRENT,
                shows_code: true,
                replaces: None,
            },
        )
        .await
        .unwrap();
        let caller = Caller {
            principal: Principal::Device {
                device_id: phone.device_id,
            },
            grants: phone.grants,
        };
        Self {
            path,
            hub,
            caller,
            clock,
            fake,
            target,
            indicator,
            desktop,
        }
    }
    async fn start_desktop(&self) -> Arc<LiveSession> {
        let id = CommandId::new();
        self.hub
            .voice_command(
                &self.caller,
                &VoiceOperation::Start {
                    target: VoiceTarget::Desktop,
                    target_id: self.target,
                    geometry_revision: 1,
                    dry_run: false,
                    disclosure_accepted: true,
                },
                id,
            )
            .await
            .unwrap();
        self.hub
            .voice_session(&self.caller, &id.to_string())
            .unwrap()
    }
    /// Says `text` as one finished command, a moment after the last.
    async fn say(
        &self,
        live: &LiveSession,
        text: &str,
        final_segment: bool,
    ) -> Result<VoiceSessionView, ProtocolError> {
        self.clock.advance(Duration::from_millis(600));
        // The phone keeps renewing the lease while voice control runs.
        let (id, lease) = {
            let state = live.state.lock().await;
            (state.view.session_id.clone(), state.view.lease.clone())
        };
        self.hub
            .renew(&self.caller, lease.lease_id, lease.generation)
            .await
            .unwrap();
        match self
            .hub
            .voice_command(
                &self.caller,
                &VoiceOperation::Transcript {
                    session_id: id,
                    utterance_id: CommandId::new().to_string(),
                    revision: 1,
                    text: text.into(),
                    final_segment,
                },
                CommandId::new(),
            )
            .await?
        {
            Some(CommandResult::VoiceSession { session }) => Ok(*session),
            other => panic!("{other:?}"),
        }
    }
    fn injected(&self) -> Vec<InputAction> {
        std::mem::take(&mut *self.desktop.input.actions.lock().unwrap())
    }
    async fn start(&self, dry_run: bool) -> Arc<LiveSession> {
        let id = CommandId::new();
        let r = self
            .hub
            .voice_command(
                &self.caller,
                &VoiceOperation::Start {
                    target: VoiceTarget::Browser,
                    target_id: self.target,
                    geometry_revision: 1,
                    dry_run,
                    disclosure_accepted: true,
                },
                id,
            )
            .await
            .unwrap();
        assert!(matches!(r, Some(CommandResult::VoiceSession { .. })));
        self.hub
            .voice_session(&self.caller, &id.to_string())
            .unwrap()
    }
    async fn command(&self, live: &LiveSession) -> Result<Option<CommandResult>, ProtocolError> {
        let id = live.state.lock().await.view.session_id.clone();
        self.hub
            .voice_command(
                &self.caller,
                &VoiceOperation::Transcript {
                    session_id: id,
                    utterance_id: CommandId::new().to_string(),
                    revision: 1,
                    text: "click Open".into(),
                    final_segment: true,
                },
                CommandId::new(),
            )
            .await
    }
    /// Send `operation` as a phone whose clock is `skew` ms off the
    /// computer's, with the phone's 10 s deadline, and let the computer reach
    /// its effect `delay` after receiving it.
    async fn send_from_phone(
        &self,
        operation: VoiceOperation,
        skew: i64,
        delay: Duration,
    ) -> Result<Option<CommandResult>, ProtocolError> {
        let phone = self.clock.now().0 + skew;
        let command_id = CommandId::new();
        let receipt = self
            .hub
            .submit(
                &self.caller,
                CommandEnvelope {
                    command_id,
                    issued_at: protocol::time::Timestamp(phone),
                    expected_revision: None,
                    expires_at: Some(protocol::time::Timestamp(phone + 10_000)),
                    expires_in_ms: Some(10_000),
                    body: Command::Voice { operation },
                },
            )
            .await;
        if let Some(error) = receipt.error {
            return Err(error);
        }
        // An admitted command reaches its effect once this test yields.
        self.clock.advance(delay);
        loop {
            let QueryData::CommandStatus {
                stage,
                result,
                error,
                ..
            } = self
                .hub
                .query(&self.caller, Query::CommandStatus { command_id })
                .await
                .unwrap()
            else {
                panic!("command status")
            };
            match stage {
                Some(command::CommandStage::Observed) => return Ok(result.map(|r| *r)),
                Some(command::CommandStage::Failed) => return Err(*error.unwrap()),
                _ => tokio::task::yield_now().await,
            }
        }
    }
    async fn close(self) {
        self.hub.shutdown().await.unwrap();
        drop(self.hub);
        std::fs::remove_dir_all(self.path).unwrap();
    }
}
#[tokio::test]
async fn receipt_and_indicator_follow_native_ack_and_dry_run() {
    for preview in [true, false] {
        let f = Fixture::new().await;
        let live = f.start(preview).await;
        assert!(
            f.indicator
                .0
                .lock()
                .unwrap()
                .contains("Voice control from Test phone")
        );
        f.command(&live).await.unwrap();
        let state = live.state.lock().await;
        assert_eq!(state.view.steps.len(), 1);
        let step = &state.view.steps[0];
        assert_eq!(
            step.outcome,
            if preview {
                VoiceOutcome::DryRun
            } else {
                VoiceOutcome::Applied
            }
        );
        assert_eq!(step.transcript, "click Open");
        assert_eq!(step.jev_usage.as_ref().unwrap().input_tokens, Some(2));
        assert_eq!(f.fake.actions.load(Ordering::SeqCst), u64::from(!preview));
        drop(state);
        f.close().await;
    }
}
#[tokio::test]
async fn service_fences_every_external_stop_cause() {
    for cause in [
        VoiceStopCause::Stop,
        VoiceStopCause::LocalStop,
        VoiceStopCause::ScreenLock,
        VoiceStopCause::Revoked,
        VoiceStopCause::Heartbeat,
        VoiceStopCause::Idle,
        VoiceStopCause::CapabilityLost,
        VoiceStopCause::Shutdown,
    ] {
        let f = Fixture::new().await;
        let live = f.start(false).await;
        match cause {
            VoiceStopCause::Stop => {
                f.hub.revoke_target(f.target, false).await.unwrap();
            }
            VoiceStopCause::LocalStop => {
                f.hub.stop_all_control().await.unwrap();
            }
            VoiceStopCause::ScreenLock => {
                f.hub
                    .control_command(&f.caller, &Command::LockDesktop)
                    .await
                    .unwrap();
            }
            VoiceStopCause::Revoked => {
                f.hub.revoke_device_leases(live.device).await.unwrap();
            }
            VoiceStopCause::Heartbeat => {
                f.clock.advance(Duration::from_secs(4));
                f.hub.expire_leases().await.unwrap();
            }
            VoiceStopCause::Idle => {
                f.clock.advance(Duration::from_secs(600));
                f.hub.expire_voice().await.unwrap();
            }
            VoiceStopCause::CapabilityLost => {
                f.fake.connected.store(false, Ordering::SeqCst);
                f.hub.expire_voice().await.unwrap();
            }
            VoiceStopCause::Shutdown => {
                f.hub.shutdown().await.unwrap();
            }
            _ => unreachable!(),
        }
        assert_eq!(live.state.lock().await.view.ended, Some(cause));
        assert!(f.command(&live).await.is_err());
        assert_eq!(f.fake.actions.load(Ordering::SeqCst), 0);
        assert_eq!(f.fake.detaches.load(Ordering::SeqCst), 1);
        f.close().await;
    }
}
#[tokio::test]
async fn settings_revision_gate_and_revocation_stop_existing_voice() {
    let f = Fixture::new().await;
    let live = f.start(false).await;
    f.hub
        .write(|tx| f.hub.set_voice_settings(tx, false, true, Some(0)))
        .unwrap();
    assert!(
        f.hub
            .write(|tx| f.hub.set_voice_settings(tx, true, true, Some(0)))
            .is_err()
    );
    f.hub.expire_voice().await.unwrap();
    assert_eq!(
        live.state.lock().await.view.ended,
        Some(VoiceStopCause::CapabilityLost)
    );
    assert!(
        !f.hub
            .voice_view(&f.caller, None)
            .await
            .unwrap()
            .voice_browser
            .is_available()
    );
    f.close().await;
}
/// Refuses, or while `refused` is false allows again, every record write.
fn refuse_records(f: &Fixture, refused: bool) {
    let db = rusqlite::Connection::open(f.path.join("companion.sqlite")).unwrap();
    db.execute_batch(if refused {
        "CREATE TRIGGER refuse_insert BEFORE INSERT ON documents BEGIN SELECT RAISE(ABORT, 'disk full'); END;
         CREATE TRIGGER refuse_update BEFORE UPDATE ON documents BEGIN SELECT RAISE(ABORT, 'disk full'); END;"
    } else {
        "DROP TRIGGER refuse_insert; DROP TRIGGER refuse_update;"
    })
    .unwrap();
}
#[tokio::test]
async fn turning_sharing_off_detaches_voice_even_when_nothing_can_be_recorded() {
    let f = Fixture::new().await;
    let live = f.start(false).await;
    let off = || {
        f.hub.set_desktop(
            None,
            protocol::capability::Availability::PermissionMissing {
                remedy: "Run ophio desktop enable".into(),
            },
        )
    };
    // Voice cannot confirm it let go, which matters more than the records.
    refuse_records(&f, true);
    f.fake.unfenced.store(true, Ordering::SeqCst);
    let error = off().await.unwrap_err();
    assert_eq!(error.code, ErrorCode::OutcomeUnknown, "{error:?}");
    assert_eq!(f.fake.detaches.load(Ordering::SeqCst), 0);
    // It lets go once it can, though nothing can be recorded yet.
    f.fake.unfenced.store(false, Ordering::SeqCst);
    let error = off().await.unwrap_err();
    assert_ne!(error.code, ErrorCode::OutcomeUnknown, "{error:?}");
    assert_eq!(
        live.state.lock().await.view.ended,
        Some(VoiceStopCause::Stop)
    );
    assert_eq!(f.fake.detaches.load(Ordering::SeqCst), 1);
    assert!(f.command(&live).await.is_err());
    assert_eq!(f.fake.actions.load(Ordering::SeqCst), 0);
    refuse_records(&f, false);
    off().await.unwrap();
    assert_eq!(f.fake.detaches.load(Ordering::SeqCst), 1);
    // The stop it could not record before is recorded now, and outlives the
    // session, so the phone learns why it stopped rather than of a restart.
    let id = live.state.lock().await.view.session_id.clone();
    let saved: Option<(DeviceId, VoiceSessionView)> =
        f.hub.read(|r| r.get("voice_session", &id)).unwrap();
    assert_eq!(saved.unwrap().1.ended, Some(VoiceStopCause::Stop));
    f.hub.expire_voice().await.unwrap();
    assert!(f.hub.voice_session(&f.caller, &id).is_err());
    let view = f.hub.voice_view(&f.caller, Some(&id)).await.unwrap();
    assert_eq!(view.session.unwrap().ended, Some(VoiceStopCause::Stop));
    f.close().await;
}
#[tokio::test]
async fn an_unrecorded_voice_stop_is_kept_until_the_expiry_tick_records_it() {
    let f = Fixture::new().await;
    let live = f.start(false).await;
    let id = live.state.lock().await.view.session_id.clone();
    refuse_records(&f, true);
    f.hub
        .set_desktop(
            None,
            protocol::capability::Availability::PermissionMissing {
                remedy: "Run ophio desktop enable".into(),
            },
        )
        .await
        .unwrap_err();
    assert_eq!(f.fake.detaches.load(Ordering::SeqCst), 1);
    // Nothing can be recorded yet, so the tick keeps the stop it knows of.
    f.hub.expire_voice().await.unwrap();
    let view = f.hub.voice_view(&f.caller, Some(&id)).await.unwrap();
    assert_eq!(view.session.unwrap().ended, Some(VoiceStopCause::Stop));
    refuse_records(&f, false);
    f.hub.expire_voice().await.unwrap();
    assert!(f.hub.voice_session(&f.caller, &id).is_err());
    let view = f.hub.voice_view(&f.caller, Some(&id)).await.unwrap();
    assert_eq!(view.session.unwrap().ended, Some(VoiceStopCause::Stop));
    assert_eq!(f.fake.detaches.load(Ordering::SeqCst), 1);
    f.close().await;
}
#[tokio::test]
async fn stale_decision_cannot_claim_newer_utterance() {
    let f = Fixture::new().await;
    let live = f.start(false).await;
    f.command(&live).await.unwrap();
    let stamp = live.state.lock().await.stamp();
    f.clock.advance(Duration::from_millis(500));
    let mut state = live.state.lock().await;
    state
        .begin("new", 1, "click Open".into(), observation())
        .unwrap();
    drop(state);
    let action = VoiceAction {
        kind: VoiceActionKind::Click,
        target: Some(observation().elements[0].clone()),
        literal: None,
        scroll_y: 0,
        previous_tab: false,
        app: None,
    };
    assert!(
        f.hub
            .voice_decision(&f.caller, &live, Decision::Act(action), stamp)
            .await
            .is_err()
    );
    assert_eq!(f.fake.actions.load(Ordering::SeqCst), 1);
    f.close().await;
}
/// The phone's deadline counts from when the computer receives the command,
/// so a phone clock that runs behind the computer's doesn't refuse a command
/// that arrives in time, and one that runs ahead doesn't stretch the fence
/// against a late one.
#[tokio::test]
async fn a_deadline_means_the_same_when_the_phones_clock_is_off() {
    for skew in [-10_000, 10_000] {
        let f = Fixture::new().await;
        let start = VoiceOperation::Start {
            target: VoiceTarget::Browser,
            target_id: f.target,
            geometry_revision: 1,
            dry_run: false,
            disclosure_accepted: true,
        };
        let late = Duration::from_secs(11);
        let refused = f
            .send_from_phone(start.clone(), skew, late)
            .await
            .unwrap_err();
        assert_eq!(
            refused.message, "Command expired before dispatch",
            "a late start with the clock {skew} ms off"
        );
        let Ok(Some(CommandResult::VoiceSession { session })) =
            f.send_from_phone(start, skew, Duration::ZERO).await
        else {
            panic!("voice control did not start with the clock {skew} ms off")
        };
        let session_id = session.session_id;
        let transcript = || VoiceOperation::Transcript {
            session_id: session_id.clone(),
            utterance_id: CommandId::new().to_string(),
            revision: 1,
            text: "click Open".into(),
            final_segment: true,
        };
        f.send_from_phone(transcript(), skew, Duration::ZERO)
            .await
            .unwrap();
        assert_eq!(f.fake.actions.load(Ordering::SeqCst), 1);
        let refused = f
            .send_from_phone(transcript(), skew, late)
            .await
            .unwrap_err();
        assert_eq!(
            refused.message, "Command expired before dispatch",
            "a late command with the clock {skew} ms off"
        );
        assert_eq!(f.fake.actions.load(Ordering::SeqCst), 1);
        f.close().await;
    }
}

/// Hyprland, as on the owner's computer: accessibility runs, but no app on
/// it exposes its controls. Voice control still starts, opens apps, presses
/// shortcuts, types, scrolls and goes back, and says why it can't click by
/// name.
#[tokio::test]
async fn desktop_voice_works_when_no_app_exposes_its_controls() {
    let f = Fixture::new().await;
    f.fake.unreadable.store(true, Ordering::SeqCst);
    let view = f.hub.voice_view(&f.caller, None).await.unwrap();
    assert!(
        view.voice_desktop.is_available(),
        "{:?}",
        view.voice_desktop
    );
    let live = f.start_desktop().await;

    f.fake.answer("launch", 0.01);
    let v = f.say(&live, "Open the browser.", true).await.unwrap();
    assert_eq!(
        v.understood,
        "Open Chromium · The computer confirmed the input"
    );
    f.say(&live, "Could you open a terminal for me?", true)
        .await
        .unwrap();
    assert_eq!(
        *f.fake.launched.lock().unwrap(),
        ["chromium.desktop", "Alacritty.desktop"]
    );
    assert!(f.injected().is_empty());

    // Jev calls most shortcuts consequential when unsure (0.78 for this
    // one); only the ones it is sure of, or known to close, are confirmed.
    f.fake.answer("press_key", 0.78);
    let v = f.say(&live, "Press Super Enter.", true).await.unwrap();
    assert_eq!(v.phase, VoicePhase::Ready, "{}", v.understood);
    assert_eq!(
        f.injected(),
        [InputAction::KeyChord {
            keys: vec!["super".into(), "Enter".into()]
        }]
    );
    f.fake.answer("press_key", 0.99);
    let v = f.say(&live, "Press alt F4", true).await.unwrap();
    assert_eq!(v.phase, VoicePhase::Confirm);
    assert_eq!(v.understood, "Press Alt+F4");
    assert!(f.injected().is_empty());
    f.say(&live, "Confirm.", true).await.unwrap();
    assert_eq!(
        f.injected(),
        [InputAction::KeyChord {
            keys: vec!["alt".into(), "F4".into()]
        }]
    );

    f.fake.answer("type", 0.04);
    let v = f.say(&live, "Type hello world.", true).await.unwrap();
    assert_eq!(
        v.understood,
        "Type into the focused app · The computer confirmed the input"
    );
    f.fake.answer("scroll", 0.);
    f.say(&live, "Scroll down.", true).await.unwrap();
    f.fake.answer("back", 0.01);
    f.say(&live, "Go back.", true).await.unwrap();
    let injected = f.injected();
    assert_eq!(
        injected[0],
        InputAction::Text {
            text: "hello world".into()
        }
    );
    assert!(matches!(injected[1], InputAction::Scroll { dy, .. } if dy == 600.));
    assert_eq!(
        injected[2],
        InputAction::KeyChord {
            keys: vec!["alt".into(), "ArrowLeft".into()]
        }
    );

    f.fake.answer("navigate", 0.);
    f.say(&live, "Go to youtube.com.", true).await.unwrap();
    assert_eq!(*f.fake.opened.lock().unwrap(), ["https://youtube.com"]);

    f.fake.answer("click", 0.);
    let v = f.say(&live, "Click Sign in.", true).await.unwrap();
    assert_eq!(v.phase, VoicePhase::Refused);
    assert!(
        v.understood.starts_with("Can’t click by name here."),
        "{}",
        v.understood
    );

    // A finished command Jev can't place says so instead of waiting.
    f.fake.answer("none", 0.);
    let v = f.say(&live, "What's the weather?", true).await.unwrap();
    assert_eq!(v.phase, VoicePhase::Refused);
    assert_eq!(v.understood, voice_control::policy::UNCLEAR);

    // Nine actions that change no readable control are not a stall.
    let state = live.state.lock().await;
    assert!(state.view.ended.is_none(), "{:?}", state.view.ended);
    drop(state);
    f.close().await;
}
#[tokio::test]
async fn words_still_being_spoken_are_shown_but_never_sent_to_jev() {
    let f = Fixture::new().await;
    let live = f.start_desktop().await;
    f.fake.answer("type", 0.);
    let v = f.say(&live, "Type hello", false).await.unwrap();
    assert_eq!(v.transcript, "Type hello");
    assert_eq!(v.phase, VoicePhase::Waiting);
    assert_eq!(f.fake.assessed.load(Ordering::SeqCst), 0);
    assert!(f.injected().is_empty());
    f.fake.unreadable.store(true, Ordering::SeqCst);
    f.say(&live, "Type hello world", true).await.unwrap();
    assert_eq!(f.fake.assessed.load(Ordering::SeqCst), 1);
    assert_eq!(
        f.injected(),
        [InputAction::Text {
            text: "hello world".into()
        }]
    );
    f.close().await;
}
/// One slow or garbled answer fails that command only. Before, it marked
/// Jev's check failed, which turned voice control off until Jev was linked
/// again.
#[tokio::test]
async fn only_a_refused_key_turns_voice_control_off() {
    let f = Fixture::new().await;
    let live = f.start_desktop().await;
    for e in [
        ::jev::Error::Timeout,
        ::jev::Error::Malformed,
        ::jev::Error::Unavailable,
    ] {
        *f.fake.jev_error.lock().unwrap() = Some(e);
        assert!(f.say(&live, "Open the browser", true).await.is_err());
        let view = f.hub.voice_view(&f.caller, None).await.unwrap();
        assert!(view.voice_desktop.is_available(), "after {e:?}");
    }
    *f.fake.jev_error.lock().unwrap() = Some(::jev::Error::Refused);
    let refused = f.say(&live, "Open the browser", true).await.unwrap_err();
    assert!(refused.message.contains("ophio jev connect"), "{refused:?}");
    let view = f.hub.voice_view(&f.caller, None).await.unwrap();
    assert!(!view.voice_desktop.is_available());
    assert_eq!(
        f.hub.jev_access().await.unwrap().connection,
        ::jev::Connection::LastCheckFailed
    );
    f.close().await;
}
/// A misheard name still finds the app; a kind of app the computer lacks
/// is refused in plain words, with nothing opened.
#[tokio::test]
async fn a_misheard_app_opens_and_a_missing_one_is_named() {
    let f = Fixture::new().await;
    f.fake.unreadable.store(true, Ordering::SeqCst);
    let live = f.start_desktop().await;
    f.fake.answer("launch", 0.);
    let v = f.say(&live, "Open Chromiun", true).await.unwrap();
    assert_eq!(
        v.understood,
        "Open Chromium · The computer confirmed the input"
    );
    let v = f.say(&live, "Open the calculator", true).await.unwrap();
    assert_eq!(v.phase, VoicePhase::Refused);
    assert_eq!(v.understood, "No calculator is installed on the computer");
    assert_eq!(*f.fake.launched.lock().unwrap(), ["chromium.desktop"]);
    f.close().await;
}

/// A command voice control can't do is offered to an agent, in the
/// person's words, only where one could start; the plain refusal stays for
/// everyone else. Nothing starts until the phone asks.
#[tokio::test]
async fn several_steps_are_offered_to_an_agent_only_where_one_could_start() {
    let f = Fixture::with_computer_use().await;
    f.fake.unreadable.store(true, Ordering::SeqCst);
    let live = f.start_desktop().await;
    f.fake.answer("task", 0.14);
    let v = f
        .say(&live, "Open Firefox and then search for cats.", true)
        .await
        .unwrap();
    assert_eq!(v.phase, VoicePhase::Refused);
    assert_eq!(v.understood, voice_control::policy::SEVERAL_STEPS);
    assert_eq!(
        v.offer.unwrap().instruction,
        "Open Firefox and then search for cats."
    );
    // A click on what can't be read is offered too.
    f.fake.answer("click", 0.69);
    let v = f
        .say(&live, "Click the blue button at the top right.", true)
        .await
        .unwrap();
    assert!(v.understood.starts_with("Can’t click by name here."));
    assert!(v.offer.is_some());
    // Nothing was sent, launched or started.
    assert!(f.injected().is_empty());
    assert!(f.fake.launched.lock().unwrap().is_empty());
    let tasks: Vec<protocol::model::Task> = f.hub.read(|r| r.list("task")).unwrap();
    assert!(tasks.is_empty());
    // A device without permission to start computer use isn't offered one.
    let Principal::Device { device_id } = f.caller.principal else {
        panic!()
    };
    let current: protocol::auth::Grants = f
        .hub
        .read(|r| {
            Ok(r.require::<crate::identity::Device>("device", device_id)?
                .record
                .grants)
        })
        .unwrap();
    let receipt = f
        .hub
        .submit(
            &Caller::local_owner(),
            CommandEnvelope::new(Command::SetDeviceGrants {
                device_id,
                expected: Some(current.clone()),
                grants: protocol::auth::Grants {
                    computer_use: false,
                    ..current
                },
            }),
        )
        .await;
    assert!(receipt.error.is_none(), "{receipt:?}");
    f.fake.answer("task", 0.14);
    let v = f
        .say(&live, "Open Firefox and then search for cats.", true)
        .await
        .unwrap();
    assert_eq!(v.understood, voice_control::policy::SEVERAL_STEPS);
    assert!(v.offer.is_none());
    f.close().await;
    // Nor is one where the computer can't run computer use.
    let f = Fixture::new().await;
    let live = f.start_desktop().await;
    f.fake.answer("task", 0.14);
    let v = f
        .say(&live, "Find my latest invoice and open it.", true)
        .await
        .unwrap();
    assert_eq!(v.understood, voice_control::policy::SEVERAL_STEPS);
    assert!(v.offer.is_none());
    f.close().await;
}

/// Without the compositor's word on focus, accessibility's window is the
/// context: keys and scrolls go through rather than ending the session.
#[tokio::test]
async fn keys_work_where_only_accessibility_knows_the_focused_window() {
    let f = Fixture::without_focus().await;
    let live = f.start_desktop().await;
    f.fake.answer("press_key", 0.2);
    let v = f.say(&live, "Press Super Enter.", true).await.unwrap();
    assert_eq!(v.phase, VoicePhase::Ready, "{}", v.understood);
    f.fake.answer("scroll", 0.);
    f.say(&live, "Scroll down.", true).await.unwrap();
    let injected = f.injected();
    assert_eq!(
        injected[0],
        InputAction::KeyChord {
            keys: vec!["super".into(), "Enter".into()]
        }
    );
    assert!(matches!(injected[1], InputAction::Scroll { .. }));
    assert!(live.state.lock().await.view.ended.is_none());
    f.close().await;
}
/// When neither the compositor nor accessibility can say which window has
/// focus, keys and text are refused rather than sent somewhere unknown.
/// Opening apps doesn't depend on focus and still works.
#[tokio::test]
async fn keys_and_text_are_refused_when_no_one_can_say_which_window_has_focus() {
    let f = Fixture::without_focus().await;
    f.fake.unreadable.store(true, Ordering::SeqCst);
    let live = f.start_desktop().await;
    for (intent, text) in [
        ("type", "Type hello."),
        ("press_key", "Press Enter."),
        ("back", "Go back."),
    ] {
        f.fake.answer(intent, 0.);
        let v = f.say(&live, text, true).await.unwrap();
        assert_eq!(v.phase, VoicePhase::Refused, "{text}");
        assert_eq!(v.understood, voice_control::policy::UNKNOWN_FOCUS, "{text}");
    }
    assert!(f.injected().is_empty());
    f.fake.answer("launch", 0.);
    f.say(&live, "Open the browser.", true).await.unwrap();
    assert_eq!(*f.fake.launched.lock().unwrap(), ["chromium.desktop"]);
    assert!(live.state.lock().await.view.ended.is_none());
    f.close().await;
}

/// An older phone sends a finished command, then words of the next while
/// Jev is still answering the first: the answer acts on the first
/// command's own words.
#[tokio::test]
async fn words_spoken_while_jev_answers_never_change_what_it_answers() {
    let f = Fixture::new().await;
    f.fake.unreadable.store(true, Ordering::SeqCst);
    let live = f.start_desktop().await;
    f.fake.answer("type", 0.);
    f.fake.holding.store(true, Ordering::SeqCst);
    let first = f.say(&live, "Type hello world.", true);
    let next = async {
        while f.fake.assessed.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
        f.say(&live, "Scroll", false).await.unwrap();
        f.fake.hold.0.add_permits(1);
    };
    let (first, ()) = tokio::join!(first, next);
    let v = first.unwrap();
    assert_eq!(
        f.injected(),
        [InputAction::Text {
            text: "hello world".into()
        }],
        "{}",
        v.understood
    );
    assert_eq!(v.steps.last().unwrap().transcript, "Type hello world.");
    f.close().await;
}

/// An action in progress holds no lock that keeps the phone's lease
/// renewals waiting: a healthy phone keeps its session however long the
/// computer takes.
#[tokio::test]
async fn renewals_never_wait_behind_a_voice_action() {
    let f = Fixture::new().await;
    let live = f.start(false).await;
    f.fake.acting.store(true, Ordering::SeqCst);
    let lease = live.state.lock().await.view.lease.clone();
    let command = f.command(&live);
    let renew = async {
        while f.fake.entered.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
        let renewed = tokio::time::timeout(
            Duration::from_millis(500),
            f.hub.renew(&f.caller, lease.lease_id, lease.generation),
        )
        .await;
        f.fake.act_gate.0.add_permits(1);
        renewed
    };
    let (done, renewed) = tokio::join!(command, renew);
    assert!(matches!(renewed, Ok(Ok(()))), "{renewed:?}");
    done.unwrap();
    assert_eq!(f.fake.actions.load(Ordering::SeqCst), 1);
    f.close().await;
}
/// The lease is checked again just before the action: one that lapsed
/// while the screen was read sends nothing.
#[tokio::test]
async fn a_lease_that_lapses_while_the_screen_is_read_sends_nothing() {
    let f = Fixture::new().await;
    let live = f.start(false).await;
    // The first snapshot is the command's own; the second, the one taken
    // just before acting.
    f.fake.snapshots.store(0, Ordering::SeqCst);
    f.fake.lapse_at.store(2, Ordering::SeqCst);
    assert!(f.command(&live).await.is_err());
    assert_eq!(f.fake.entered.load(Ordering::SeqCst), 0);
    assert_eq!(f.fake.actions.load(Ordering::SeqCst), 0);
    f.close().await;
}

/// The phone renews the lease while an action waits to be sent: the action
/// goes out on the renewed lease rather than being refused, and ending, a
/// healthy session.
#[tokio::test]
async fn an_action_sent_after_a_renewal_goes_out_on_the_renewed_lease() {
    let f = Fixture::new().await;
    let live = f.start(false).await;
    f.fake.acting.store(true, Ordering::SeqCst);
    let lease = live.state.lock().await.view.lease.clone();
    let command = f.command(&live);
    let renew = async {
        while f.fake.entered.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
        // Renewed at 2.5 s, to 5.5 s; the action is sent at 3.1 s.
        f.clock.advance(Duration::from_millis(2500));
        f.hub
            .renew(&f.caller, lease.lease_id, lease.generation)
            .await
            .unwrap();
        f.clock.advance(Duration::from_millis(600));
        f.fake.act_gate.0.add_permits(1);
    };
    let (done, ()) = tokio::join!(command, renew);
    done.unwrap();
    assert_eq!(f.fake.actions.load(Ordering::SeqCst), 1);
    assert!(live.state.lock().await.view.ended.is_none());
    f.close().await;
}
/// Stop withdraws an action's authority the moment it starts, before it
/// waits for that action: a driver about to send sends nothing.
#[tokio::test]
async fn stopping_withdraws_authority_from_an_action_about_to_be_sent() {
    let f = Fixture::new().await;
    let live = f.start(false).await;
    f.fake.acting.store(true, Ordering::SeqCst);
    let command = f.command(&live);
    let stop = async {
        while f.fake.entered.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
        let stopping = f.hub.revoke_target(f.target, false);
        let release = async {
            tokio::time::sleep(Duration::from_millis(50)).await;
            f.fake.act_gate.0.add_permits(1);
        };
        let (stopped, ()) = tokio::join!(stopping, release);
        stopped
    };
    let (done, stopped) = tokio::join!(command, stop);
    let _ = (done, stopped);
    assert_eq!(f.fake.actions.load(Ordering::SeqCst), 0);
    f.close().await;
}

/// Getting ready to start an app can outlast the lease; the app must not
/// start then. Authority is the last thing checked before it starts.
#[tokio::test]
async fn an_app_whose_launch_outlasts_the_lease_never_starts() {
    let f = Fixture::new().await;
    f.fake.unreadable.store(true, Ordering::SeqCst);
    let live = f.start_desktop().await;
    f.fake.answer("launch", 0.);
    f.fake.slow_launch.store(4000, Ordering::SeqCst);
    let _ = f.say(&live, "Open the browser.", true).await;
    assert!(f.fake.launched.lock().unwrap().is_empty());
    f.close().await;
}

/// A launch whose stop couldn't be confirmed leaves the voice driver's
/// barrier failing: voice control never claims it let go of the display.
#[tokio::test]
async fn a_launch_that_could_not_be_stopped_keeps_the_barrier_from_succeeding() {
    let f = Fixture::new().await;
    f.fake.unreadable.store(true, Ordering::SeqCst);
    let live = f.start_desktop().await;
    f.fake.answer("launch", 0.);
    f.fake.unstoppable.store(true, Ordering::SeqCst);
    let _ = f.say(&live, "Open the browser.", true).await;
    assert_eq!(
        live.state.lock().await.view.phase,
        VoicePhase::OutcomeUnknown
    );
    assert!(live.driver.barrier().await.is_err());
    // The session is done, but the display it was on keeps the launch.
    assert!(live.detached.load(Ordering::SeqCst));
    assert!(
        f.hub
            .target(f.target)
            .await
            .unwrap()
            .lock()
            .await
            .unresolved_launch
    );
    // Stop says plainly that an app may still open, every time.
    for _ in 0..2 {
        let stop = f.hub.revoke_target(f.target, false).await.unwrap_err();
        assert!(stop.message.contains("may still open"), "{stop:?}");
    }
    // Locking still locks, and says an app may still open.
    let locked = f
        .hub
        .control_command(&f.caller, &Command::LockDesktop)
        .await
        .unwrap_err();
    assert!(
        locked.message.starts_with("The computer is locked"),
        "{locked:?}"
    );
    assert_eq!(f.desktop.locks.load(Ordering::SeqCst), 1);
    // Shutting down can't confirm it let go either, and says so.
    assert!(f.hub.shutdown().await.is_err());
    drop(live);
    let path = f.path.clone();
    drop(f);
    let _ = std::fs::remove_dir_all(path);
}

/// An old command whose action fails after its session was stopped never
/// revokes the session that has since taken the display.
#[tokio::test]
async fn an_old_command_never_stops_the_session_that_replaced_it() {
    let f = Fixture::new().await;
    let old = f.start(false).await;
    f.fake.acting.store(true, Ordering::SeqCst);
    let command = {
        let old = old.clone();
        let hub = f.hub.clone();
        let caller = f.caller.clone();
        tokio::spawn(async move {
            let id = old.state.lock().await.view.session_id.clone();
            hub.voice_command(
                &caller,
                &VoiceOperation::Transcript {
                    session_id: id,
                    utterance_id: CommandId::new().to_string(),
                    revision: 1,
                    text: "click Open".into(),
                    final_segment: true,
                },
                CommandId::new(),
            )
            .await
        })
    };
    while f.fake.entered.load(Ordering::SeqCst) == 0 {
        tokio::task::yield_now().await;
    }
    // Stop the old session; once it is marked stopped, hold its state so
    // its command pauses right after its action fails.
    let stop = {
        let hub = f.hub.clone();
        let target = f.target;
        tokio::spawn(async move { hub.revoke_target(target, false).await })
    };
    let held = loop {
        if let Ok(state) = old.state.try_lock()
            && state.view.ended.is_some()
        {
            break state;
        }
        tokio::task::yield_now().await;
    };
    f.fake.act_gate.0.add_permits(1);
    let _ = stop.await.unwrap();
    // A new session takes the display while the old command waits.
    f.fake.acting.store(false, Ordering::SeqCst);
    let new = f.start(false).await;
    let lease = new.state.lock().await.view.lease.clone();
    drop(held);
    let _ = command.await.unwrap();
    let current = f
        .hub
        .target(f.target)
        .await
        .unwrap()
        .lock()
        .await
        .view
        .clone();
    let held_now = current.lease.unwrap();
    assert_eq!(held_now.lease_id, lease.lease_id);
    assert_eq!(held_now.state, LeaseState::Active);
    assert!(new.state.lock().await.view.ended.is_none());
    assert_eq!(f.fake.actions.load(Ordering::SeqCst), 0);
    f.close().await;
}

/// Leaving control while a voice launch hangs waits for that launch, as it
/// would for an agent's: one that can't be stopped keeps the display
/// marked, and nobody else is given it while an app may still open.
#[tokio::test]
async fn leaving_control_waits_for_a_hung_voice_launch_and_keeps_its_mark() {
    let f = Fixture::new().await;
    f.fake.unreadable.store(true, Ordering::SeqCst);
    let live = f.start_desktop().await;
    f.fake.answer("launch", 0.);
    f.fake.unstoppable.store(true, Ordering::SeqCst);
    // Longer than voice control's own barrier waits for its driver.
    f.fake.hang_ms.store(2600, Ordering::SeqCst);
    let say = f.say(&live, "Open the browser.", true);
    let leave = async {
        while f.fake.helpers.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        f.hub.revoke_target(f.target, false).await
    };
    let (_, left) = tokio::join!(say, leave);
    assert_eq!(left, Err(crate::control::launch_unresolved()));
    assert_eq!(f.fake.cleaned.load(Ordering::SeqCst), 1);
    let target = f.hub.target(f.target).await.unwrap();
    assert!(target.lock().await.unresolved_launch);
    // Nobody is given the display while the app may still open.
    let taken = f
        .hub
        .control_command(
            &f.caller,
            &Command::RequestHumanLease {
                target_id: f.target,
                geometry_revision: 1,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(taken, crate::control::launch_unresolved());
    drop(live);
    let path = f.path.clone();
    let _ = f.hub.shutdown().await;
    drop(f);
    let _ = std::fs::remove_dir_all(path);
}

/// A voice launch that hangs past what voice control used to wait for an
/// action, but within the launcher's own limit, finishes stopping what it
/// started and is answered as it ended: an app may still open, and the
/// display is marked.
#[tokio::test]
async fn a_slow_unstoppable_voice_launch_is_answered_as_it_ended_and_marks() {
    let f = Fixture::new().await;
    f.fake.unreadable.store(true, Ordering::SeqCst);
    let live = f.start_desktop().await;
    f.fake.answer("launch", 0.);
    f.fake.unstoppable.store(true, Ordering::SeqCst);
    f.fake.hang_ms.store(3000, Ordering::SeqCst);
    assert!(Duration::from_millis(3000) < voice_control::LAUNCH_LIMIT);
    let said = f.say(&live, "Open the browser.", true).await.unwrap();
    assert_eq!(said.phase, VoicePhase::OutcomeUnknown);
    assert!(
        said.understood.contains("may still open"),
        "{}",
        said.understood
    );
    assert_eq!(f.fake.cleaned.load(Ordering::SeqCst), 1);
    let target = f.hub.target(f.target).await.unwrap();
    assert!(target.lock().await.unresolved_launch);
    let again = f.hub.revoke_target(f.target, false).await;
    assert_eq!(again, Err(crate::control::launch_unresolved()));
    drop(live);
    let path = f.path.clone();
    let _ = f.hub.shutdown().await;
    drop(f);
    let _ = std::fs::remove_dir_all(path);
}

/// Once a voice session's release has found a launch it couldn't stop and
/// marked the display, the session is done: the expiry tick never lets go
/// of the display again, the mark stays, and another session is still
/// checked and stopped when it should be.
#[tokio::test]
async fn an_unstoppable_voice_launch_is_released_once_and_others_still_expire() {
    let f = Fixture::new().await;
    let other = {
        let mut other = f.desktop.target.lock().unwrap().clone();
        other.target_id = TargetId::new();
        other.label = "Second display".into();
        f.desktop.extra.lock().unwrap().push(other.clone());
        other.target_id
    };
    f.fake.unreadable.store(true, Ordering::SeqCst);
    let live = f.start_desktop().await;
    f.fake.answer("launch", 0.);
    f.fake.unstoppable.store(true, Ordering::SeqCst);
    let _ = f.say(&live, "Open the browser.", true).await;
    // A second session, on the other display.
    let id = CommandId::new();
    f.hub
        .voice_command(
            &f.caller,
            &VoiceOperation::Start {
                target: VoiceTarget::Desktop,
                target_id: other,
                geometry_revision: 1,
                dry_run: false,
                disclosure_accepted: true,
            },
            id,
        )
        .await
        .unwrap();
    let second = f.hub.voice_session(&f.caller, &id.to_string()).unwrap();
    // Several expiry ticks pass; nothing lets go of either display again.
    let releases = f.desktop.input.releases.load(Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(f.desktop.input.releases.load(Ordering::SeqCst), releases);
    assert!(live.detached.load(Ordering::SeqCst));
    let first = live.state.lock().await.view.session_id.clone();
    assert!(
        !f.hub
            .inner
            .voice
            .sessions
            .lock()
            .unwrap()
            .contains_key(&first)
    );
    // The other session is still checked: voice control turned off for the
    // desktop stops it at the next tick.
    f.hub
        .write(|tx| {
            let mut settings: VoiceSettings =
                tx.read().get("meta", "voice_settings")?.unwrap_or_default();
            settings.desktop = false;
            tx.put("meta", "voice_settings", &settings)
        })
        .unwrap();
    let began = std::time::Instant::now();
    while second.state.lock().await.view.ended.is_none() {
        assert!(began.elapsed() < Duration::from_secs(2), "never expired");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        second.state.lock().await.view.ended,
        Some(VoiceStopCause::CapabilityLost)
    );
    while f.hub.voice_pending() {
        assert!(began.elapsed() < Duration::from_secs(2), "still pending");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let releases = f.desktop.input.releases.load(Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(f.desktop.input.releases.load(Ordering::SeqCst), releases);
    // The first display keeps its mark, and says so.
    let target = f.hub.target(f.target).await.unwrap();
    assert!(target.lock().await.unresolved_launch);
    assert_eq!(
        f.hub.revoke_target(f.target, false).await,
        Err(crate::control::launch_unresolved())
    );
    drop((live, second));
    let path = f.path.clone();
    let _ = f.hub.shutdown().await;
    drop(f);
    let _ = std::fs::remove_dir_all(path);
}

/// Two displays in the order the table lists them: voice control on the
/// first, about to open an app, and the phone holding the second. Paths
/// that let go of every display go through them in this order.
async fn two_displays() -> (Fixture, Arc<LiveSession>, TargetId, LeaseView) {
    let f = Fixture::new().await;
    let extra = {
        let mut extra = f.desktop.target.lock().unwrap().clone();
        extra.target_id = TargetId::new();
        extra.label = "Second display".into();
        f.desktop.extra.lock().unwrap().push(extra.clone());
        extra.target_id
    };
    f.hub.target(f.target).await.unwrap();
    f.hub.target(extra).await.unwrap();
    let order: Vec<TargetId> = f.hub.inner.controls.lock().await.keys().copied().collect();
    let (first, second) = (order[0], order[1]);
    f.fake.unreadable.store(true, Ordering::SeqCst);
    let id = CommandId::new();
    f.hub
        .voice_command(
            &f.caller,
            &VoiceOperation::Start {
                target: VoiceTarget::Desktop,
                target_id: first,
                geometry_revision: 1,
                dry_run: false,
                disclosure_accepted: true,
            },
            id,
        )
        .await
        .unwrap();
    let live = f.hub.voice_session(&f.caller, &id.to_string()).unwrap();
    let Some(CommandResult::LeaseGranted { lease }) = f
        .hub
        .control_command(
            &f.caller,
            &Command::RequestHumanLease {
                target_id: second,
                geometry_revision: 1,
            },
        )
        .await
        .unwrap()
    else {
        panic!("lease expected")
    };
    f.fake.answer("launch", 0.);
    f.fake.hang_ms.store(1500, Ordering::SeqCst);
    (f, live, second, lease)
}

/// Local Stop stops input on every display at once: while it waits for a
/// voice launch hung on one display, the phone's typing on another is
/// refused at once and nothing of it is typed.
#[tokio::test]
async fn local_stop_refuses_input_on_every_display_while_one_waits_for_a_launch() {
    let (f, live, _, lease) = two_displays().await;
    let say = f.say(&live, "Open the browser.", true);
    let stop_and_type = async {
        while f.fake.helpers.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let hub = f.hub.clone();
        let stop = tokio::spawn(async move { hub.stop_all_control().await });
        while f.hub.inner.local_stops.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
        f.injected();
        let began = std::time::Instant::now();
        let ack = f
            .hub
            .human_input(
                &f.caller,
                InputEnvelope {
                    lease_id: lease.lease_id,
                    generation: lease.generation,
                    geometry_revision: 1,
                    seq: 1,
                    action: InputAction::Text { text: "x".into() },
                },
            )
            .await;
        let waited = began.elapsed();
        assert!(!stop.is_finished(), "Local Stop ended before the launch");
        (ack, waited, stop.await.unwrap())
    };
    let (_, (ack, waited, stopped)) = tokio::join!(say, stop_and_type);
    assert_eq!(ack.outcome, InputOutcome::StaleLease, "{ack:?}");
    assert!(waited < Duration::from_millis(200), "{waited:?}");
    assert!(f.injected().is_empty());
    stopped.unwrap();
    drop(live);
    f.close().await;
}

/// The computer locking lets go of every display at once in the same way:
/// while it waits for a launch on one, another's lease no longer holds, and
/// that display's own lock is free for whatever asks next.
#[tokio::test]
async fn locking_fences_every_display_before_waiting_for_a_launch() {
    let (f, live, second, lease) = two_displays().await;
    let say = f.say(&live, "Open the browser.", true);
    let lock_and_look = async {
        while f.fake.helpers.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let hub = f.hub.clone();
        let backend = f.hub.desktop().unwrap();
        let locking = tokio::spawn(async move {
            hub.observe_desktop_lock(&backend, DesktopLockState::Locked)
                .await
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        let target = f.hub.target(second).await.unwrap();
        let held = target.try_lock().map(|t| {
            t.live
                .holds(lease.lease_id, lease.generation, f.clock.monotonic())
        });
        assert!(!locking.is_finished(), "the lock ended before the launch");
        let _ = locking.await.unwrap();
        held
    };
    let (_, held) = tokio::join!(say, lock_and_look);
    assert_eq!(held.ok(), Some(false));
    drop(live);
    f.close().await;
}

/// A display marked with a launch that couldn't be stopped stays marked,
/// and refuses control, while the computer can't confirm the launch
/// stopped; it is checked again with no display's lock held, and once
/// systemd says the launch's scope is inactive the mark clears, the display
/// is let go of, the confirmation is recorded and control can be given on
/// it again.
#[tokio::test]
async fn a_marked_display_clears_once_the_computer_confirms_the_launch_stopped() {
    let f = Fixture::new().await;
    f.fake.unreadable.store(true, Ordering::SeqCst);
    let live = f.start_desktop().await;
    let target = f.hub.target(f.target).await.unwrap();
    let check = Arc::new(FakeCheck::default());
    *check.display.lock().unwrap() = Some(target.clone());
    *f.fake.check.lock().unwrap() = Some(check.clone());
    f.fake.answer("launch", 0.);
    f.fake.unstoppable.store(true, Ordering::SeqCst);
    let _ = f.say(&live, "Open the browser.", true).await;
    assert!(target.lock().await.unresolved_launch);
    let request = Command::RequestHumanLease {
        target_id: f.target,
        geometry_revision: 1,
    };
    // While systemd can't confirm it, the display stays marked.
    let began = std::time::Instant::now();
    while check.calls.load(Ordering::SeqCst) < 3 {
        assert!(began.elapsed() < Duration::from_secs(3), "never checked");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(target.lock().await.unresolved_launch);
    assert_eq!(
        f.hub
            .control_command(&f.caller, &request)
            .await
            .unwrap_err(),
        crate::control::launch_unresolved()
    );
    // The first checks stop what is left again, as the launch's cleanup did.
    assert!(check.stops.load(Ordering::SeqCst) >= 1);
    // systemd now says the scope is inactive.
    check.stopped.store(true, Ordering::SeqCst);
    while target.lock().await.unresolved_launch {
        assert!(began.elapsed() < Duration::from_secs(5), "never cleared");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(check.locked.load(Ordering::SeqCst), 0);
    // What the phone shows: input let go, no longer unknown.
    let view: TargetControlView = f.hub.read(|r| r.get("control", f.target)).unwrap().unwrap();
    assert_eq!(view.input_state, InputState::Paused);
    let events = f.hub.read(|r| Ok(r.journal(0)?.events)).unwrap();
    assert!(events.iter().any(|e| matches!(
        &e.event,
        protocol::Event::InterventionRecorded { target_id: Some(id), receipt, .. }
            if *id == f.target && receipt.facts.iter().any(|fact| fact.label == "App launch")
    )));
    // Control can be given on it again.
    assert!(matches!(
        f.hub.control_command(&f.caller, &request).await.unwrap(),
        Some(CommandResult::LeaseGranted { .. })
    ));
    drop(live);
    f.close().await;
}

/// A voice session whose release couldn't let go of input when its launch
/// went unstopped is let go of once input can be, after the launch is
/// confirmed stopped, and the display isn't marked again for that same
/// launch. Input that still couldn't be let go of is reported as such.
#[tokio::test]
async fn clearing_a_mark_never_marks_the_display_again_for_the_same_launch() {
    let f = Fixture::new().await;
    f.fake.unreadable.store(true, Ordering::SeqCst);
    let live = f.start_desktop().await;
    let target = f.hub.target(f.target).await.unwrap();
    let check = Arc::new(FakeCheck::default());
    *f.fake.check.lock().unwrap() = Some(check.clone());
    f.fake.answer("launch", 0.);
    f.fake.unstoppable.store(true, Ordering::SeqCst);
    // Input can't be let go of while the launch goes unstopped.
    f.desktop.input.fail_release.store(true, Ordering::SeqCst);
    let _ = f.say(&live, "Open the browser.", true).await;
    assert_eq!(
        f.hub.revoke_target(f.target, false).await,
        Err(crate::control::release_and_launch_unresolved())
    );
    assert!(!live.detached.load(Ordering::SeqCst));
    // The launch is confirmed stopped while input still can't be let go of.
    check.stopped.store(true, Ordering::SeqCst);
    let began = std::time::Instant::now();
    while target.lock().await.unresolved_launch {
        assert!(began.elapsed() < Duration::from_secs(5), "never cleared");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(target.lock().await.view.input_state, InputState::Unknown);
    let summaries = |f: &Fixture| -> Vec<String> {
        let events = f.hub.read(|r| Ok(r.journal(0)?.events)).unwrap();
        events
            .iter()
            .filter_map(|e| match &e.event {
                protocol::Event::InterventionRecorded { receipt, .. }
                    if receipt.facts.iter().any(|fact| fact.label == "App launch") =>
                {
                    Some(receipt.summary.clone())
                }
                _ => None,
            })
            .collect()
    };
    let said = summaries(&f);
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(said[0].contains("hasn’t confirmed it let go"), "{said:?}");
    // Input can be let go of now; the session's next release does, and
    // says nothing of the launch already proven stopped.
    f.desktop.input.fail_release.store(false, Ordering::SeqCst);
    while !live.detached.load(Ordering::SeqCst) {
        assert!(began.elapsed() < Duration::from_secs(5), "never let go");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    for _ in 0..30 {
        assert!(!target.lock().await.unresolved_launch);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(target.lock().await.view.input_state, InputState::Paused);
    assert_eq!(summaries(&f).len(), 1);
    drop(live);
    f.close().await;
}
