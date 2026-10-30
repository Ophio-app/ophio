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
#[derive(Debug, Default)]
struct Fake {
    actions: AtomicU64,
    detaches: AtomicU64,
    connected: AtomicBool,
    unfenced: AtomicBool,
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
        Ok(observation())
    }
    async fn act(&self, _: &VoiceAction, _: &Snapshot) -> Result<(), VoiceError> {
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
                intent: choice("click"),
                target: choice("e1"),
                complete: score(1.),
                is_command: score(1.),
                destructive: score(0.),
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
}
impl Fixture {
    async fn new() -> Self {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/voice-service-tests")
            .join(CommandId::new().to_string());
        let fake = Arc::new(Fake::default());
        fake.connected.store(true, Ordering::SeqCst);
        let clock = Arc::new(FakeClock::default());
        let desktop = Arc::new(FakeDesktop::default());
        let target = desktop.target.lock().unwrap().target_id;
        let hub = CompanionHub::builder()
            .data_dir(&path)
            .clock(clock.clone())
            .desktop(desktop)
            .build()
            .await
            .unwrap();
        let indicator = Arc::new(Indicator::default());
        hub.set_local_indicator(Some(indicator.clone())).await;
        hub.configure_voice(VoiceConfig {
            browser: Arc::new(Factory(fake.clone())),
            desktop: fake.clone(),
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
        }
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
