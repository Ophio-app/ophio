// SPDX-License-Identifier: Apache-2.0
//! Synthetic desktop delivery only: no native input, windows, apps or capture.
use super::*;
use crate::testing::FakeDesktop;
use ports::{CapturedFrame, DesktopCapabilities, FrameStream, PlatformError};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};
use voice_control::{Authority, NoLauncher};

#[derive(Debug, Default)]
struct MappedDesktop(FakeDesktop);
#[async_trait]
impl DesktopBackend for MappedDesktop {
    async fn capabilities(&self) -> DesktopCapabilities {
        self.0.capabilities().await
    }
    async fn targets(&self) -> Result<Vec<ControlTarget>, PlatformError> {
        self.0.targets().await
    }
    async fn capture_still(&self, _: TargetId) -> Result<CapturedFrame, PlatformError> {
        panic!("driver tests must not capture a desktop")
    }
    async fn open_stream(&self, _: TargetId, _: u8) -> Result<Box<dyn FrameStream>, PlatformError> {
        panic!("driver tests must not stream a desktop")
    }
    async fn input_sink(&self, _: TargetId) -> Result<Box<dyn InputSink>, PlatformError> {
        panic!("driver tests provide their own synthetic sink")
    }
    async fn lock_session(&self) -> Result<(), PlatformError> {
        panic!("driver tests must not lock a desktop")
    }
    async fn accessibility_point(
        &self,
        target: TargetId,
        revision: u64,
        x: f64,
        y: f64,
    ) -> Result<TargetPoint, PlatformError> {
        let expected = self.0.target.lock().expect("synthetic target");
        assert_eq!(target, expected.target_id);
        assert_eq!(revision, expected.geometry.revision);
        Ok(TargetPoint {
            x: x / f64::from(expected.geometry.width),
            y: y / f64::from(expected.geometry.height),
        })
    }
}

#[derive(Debug)]
struct TestAuthority {
    stopped: AtomicBool,
    checks: AtomicUsize,
    refuse_on_check: AtomicUsize,
}
impl Default for TestAuthority {
    fn default() -> Self {
        Self {
            stopped: AtomicBool::new(false),
            checks: AtomicUsize::new(0),
            refuse_on_check: AtomicUsize::new(usize::MAX),
        }
    }
}
impl Authority for TestAuthority {
    fn check(&self) -> Result<(), VoiceError> {
        let check = self.checks.fetch_add(1, SeqCst) + 1;
        if self.stopped.load(SeqCst) || check >= self.refuse_on_check.load(SeqCst) {
            Err(VoiceError::Stopped)
        } else {
            Ok(())
        }
    }
}

#[derive(Debug)]
struct Controls {
    snapshot: Snapshot,
    authority: Arc<TestAuthority>,
    acknowledge: AtomicBool,
    stop_after_focus: AtomicBool,
    focus_attempts: AtomicUsize,
    focused: AtomicUsize,
}
#[async_trait]
impl Accessibility for Controls {
    async fn snapshot(&self) -> Result<Snapshot, VoiceError> {
        Ok(self.snapshot.clone())
    }
    async fn activate(&self, _: &VoiceElement, focus: bool) -> Result<bool, VoiceError> {
        assert!(focus, "typing must request focus, not press a control");
        self.focus_attempts.fetch_add(1, SeqCst);
        if !self.acknowledge.load(SeqCst) {
            return Ok(false);
        }
        self.focused.fetch_add(1, SeqCst);
        if self.stop_after_focus.load(SeqCst) {
            self.authority.stopped.store(true, SeqCst);
        }
        Ok(true)
    }
}

#[derive(Debug, Default)]
struct Input {
    attempts: AtomicUsize,
    delivered: Mutex<Vec<InputAction>>,
    refusal: Mutex<Option<PlatformError>>,
}
struct Sink(Arc<Input>);
#[async_trait]
impl InputSink for Sink {
    async fn inject(
        &mut self,
        action: &InputAction,
        _: &TargetGeometry,
    ) -> Result<(), PlatformError> {
        self.0.attempts.fetch_add(1, SeqCst);
        if let Some(error) = self.0.refusal.lock().expect("synthetic refusal").take() {
            return Err(error);
        }
        self.0
            .delivered
            .lock()
            .expect("synthetic input")
            .push(action.clone());
        Ok(())
    }
    async fn release_all(&mut self) -> Result<(), PlatformError> {
        Ok(())
    }
}

struct Fixture {
    driver: DesktopDriver,
    controls: Arc<Controls>,
    authority: Arc<TestAuthority>,
    input: Arc<Input>,
    snapshot: Snapshot,
    action: VoiceAction,
}
impl Fixture {
    fn new(already_focused: bool) -> Self {
        let element = VoiceElement {
            id: "synthetic-field".into(),
            role: "text field".into(),
            name: "Notes".into(),
            bounds: VoiceBounds {
                x: 1.0,
                y: 1.0,
                width: 10.0,
                height: 10.0,
            },
            visible: true,
            enabled: true,
            focused: already_focused,
            sensitive: false,
            destructive: false,
        };
        let mut snapshot = Snapshot::new(vec![element.clone()], "synthetic-window".into());
        snapshot.desktop = true;
        let authority = Arc::new(TestAuthority::default());
        let controls = Arc::new(Controls {
            snapshot: snapshot.clone(),
            authority: authority.clone(),
            acknowledge: AtomicBool::new(true),
            stop_after_focus: AtomicBool::new(false),
            focus_attempts: AtomicUsize::new(0),
            focused: AtomicUsize::new(0),
        });
        let backend = Arc::new(MappedDesktop::default());
        let target = backend.0.target.lock().expect("synthetic target").target_id;
        let input = Arc::new(Input::default());
        let driver = DesktopDriver {
            unfenced: std::sync::atomic::AtomicU64::new(u64::MAX),
            launches: Arc::new(crate::control::Launches::default()),
            a11y: controls.clone(),
            launcher: Arc::new(NoLauncher("Synthetic launcher is not used")),
            windows: None,
            backend,
            sink: AsyncMutex::new(Box::new(Sink(input.clone()))),
            target,
            encoder: None,
        };
        Self {
            driver,
            controls,
            authority,
            input,
            snapshot,
            action: VoiceAction {
                kind: VoiceActionKind::Type,
                target: Some(element),
                literal: Some("hello".into()),
                scroll_y: 0,
                previous_tab: false,
                app: None,
            },
        }
    }
    async fn act(&self) -> Result<(), VoiceError> {
        self.driver
            .act(&self.action, &self.snapshot, self.authority.clone())
            .await
    }
    fn refuse(&self, error: PlatformError) {
        *self.input.refusal.lock().expect("synthetic refusal") = Some(error);
    }
    fn assert_no_delivery(&self) {
        assert!(
            self.input
                .delivered
                .lock()
                .expect("synthetic input")
                .is_empty()
        );
    }
}

fn chord(keys: &[&str]) -> InputAction {
    InputAction::KeyChord {
        keys: keys.iter().map(|key| (*key).to_owned()).collect(),
    }
}

#[test]
fn mac_history_uses_command_and_brackets_not_option_arrows() {
    assert_eq!(history_keys(true, false), chord(&["super", "["]));
    assert_eq!(history_keys(true, true), chord(&["super", "]"]));
}

#[test]
fn other_desktops_keep_alt_and_history_arrows() {
    assert_eq!(history_keys(false, false), chord(&["alt", "ArrowLeft"]));
    assert_eq!(history_keys(false, true), chord(&["alt", "ArrowRight"]));
}

#[tokio::test]
async fn desktop_history_delivers_the_current_platform_chords() {
    let mut f = Fixture::new(true);
    f.action.target = None;
    f.action.literal = None;
    f.action.kind = VoiceActionKind::Back;
    f.act().await.expect("back");
    f.action.kind = VoiceActionKind::Forward;
    f.act().await.expect("forward");
    let expected = if cfg!(target_os = "macos") {
        vec![chord(&["super", "["]), chord(&["super", "]"])]
    } else {
        vec![chord(&["alt", "ArrowLeft"]), chord(&["alt", "ArrowRight"])]
    };
    assert_eq!(
        *f.input.delivered.lock().expect("synthetic input"),
        expected
    );
    assert_eq!(f.controls.focus_attempts.load(SeqCst), 0);
}

#[tokio::test]
async fn acknowledged_focus_then_stop_is_unknown_and_never_types() {
    let f = Fixture::new(false);
    f.controls.stop_after_focus.store(true, SeqCst);
    assert!(matches!(f.act().await, Err(VoiceError::OutcomeUnknown)));
    assert_eq!(f.controls.focused.load(SeqCst), 1);
    assert_eq!(f.input.attempts.load(SeqCst), 0);
    f.assert_no_delivery();
}

#[tokio::test]
async fn acknowledged_focus_then_input_refusal_is_unknown() {
    let f = Fixture::new(false);
    f.refuse(PlatformError::PermissionMissing("Synthetic refusal".into()));
    assert!(matches!(f.act().await, Err(VoiceError::OutcomeUnknown)));
    assert_eq!(f.controls.focused.load(SeqCst), 1);
    assert_eq!(f.input.attempts.load(SeqCst), 1);
    f.assert_no_delivery();
}

#[tokio::test]
async fn acknowledged_focus_then_untypable_text_is_unknown() {
    let f = Fixture::new(false);
    f.refuse(PlatformError::TextUnavailable {
        characters: "synthetic".into(),
        paste_available: false,
    });
    assert!(matches!(f.act().await, Err(VoiceError::OutcomeUnknown)));
    assert_eq!(f.controls.focused.load(SeqCst), 1);
    f.assert_no_delivery();
}

#[tokio::test]
async fn refusal_without_focus_keeps_its_definite_failure() {
    for targeted in [true, false] {
        let mut f = Fixture::new(true);
        if !targeted {
            f.action.target = None;
        }
        f.refuse(PlatformError::PermissionMissing("Synthetic refusal".into()));
        assert!(matches!(f.act().await, Err(VoiceError::Unavailable(reason))
            if reason == "Desktop input was refused"));
        assert_eq!(f.controls.focus_attempts.load(SeqCst), 0);
        assert_eq!(f.input.attempts.load(SeqCst), 1);
        f.assert_no_delivery();
    }
}

#[tokio::test]
async fn untypable_text_without_focus_keeps_its_specific_refusal() {
    let f = Fixture::new(true);
    f.refuse(PlatformError::TextUnavailable {
        characters: "synthetic".into(),
        paste_available: false,
    });
    assert!(matches!(f.act().await, Err(VoiceError::Unavailable(reason))
        if reason.contains("Nothing was typed") && reason.contains("keyboard layout")));
    assert_eq!(f.controls.focus_attempts.load(SeqCst), 0);
    f.assert_no_delivery();
}

#[tokio::test]
async fn native_unknown_without_focus_still_stays_unknown() {
    let f = Fixture::new(true);
    f.refuse(PlatformError::OutcomeUnknown(
        "Synthetic lost acknowledgement".into(),
    ));
    assert!(matches!(f.act().await, Err(VoiceError::OutcomeUnknown)));
    assert_eq!(f.controls.focus_attempts.load(SeqCst), 0);
    f.assert_no_delivery();
}

#[tokio::test]
async fn stop_without_focus_keeps_stopped_and_never_injects() {
    let f = Fixture::new(true);
    // Allow the first driver check, then revoke at its final delivery check.
    f.authority.refuse_on_check.store(2, SeqCst);
    assert!(matches!(f.act().await, Err(VoiceError::Stopped)));
    assert_eq!(f.controls.focus_attempts.load(SeqCst), 0);
    assert_eq!(f.input.attempts.load(SeqCst), 0);
    f.assert_no_delivery();
}

#[tokio::test]
async fn missing_literal_is_refused_before_any_focus_effect() {
    let mut f = Fixture::new(false);
    f.action.literal = None;
    assert!(matches!(f.act().await, Err(VoiceError::StaleTarget)));
    assert_eq!(f.controls.focus_attempts.load(SeqCst), 0);
    assert_eq!(f.input.attempts.load(SeqCst), 0);
    f.assert_no_delivery();
}

#[tokio::test]
async fn unacknowledged_focus_stays_stale_and_never_injects() {
    let f = Fixture::new(false);
    f.controls.acknowledge.store(false, SeqCst);
    assert!(matches!(f.act().await, Err(VoiceError::StaleTarget)));
    assert_eq!(f.controls.focus_attempts.load(SeqCst), 1);
    assert_eq!(f.controls.focused.load(SeqCst), 0);
    assert_eq!(f.input.attempts.load(SeqCst), 0);
    f.assert_no_delivery();
}

#[tokio::test]
async fn acknowledged_focus_and_typing_still_succeed() {
    let f = Fixture::new(false);
    f.act().await.expect("focus and type");
    assert_eq!(f.controls.focused.load(SeqCst), 1);
    assert_eq!(f.input.attempts.load(SeqCst), 1);
    assert_eq!(
        *f.input.delivered.lock().expect("synthetic input"),
        vec![InputAction::Text {
            text: "hello".into()
        }]
    );
}
