// SPDX-License-Identifier: Apache-2.0
//! Synthetic only. No host clipboard or input APIs are invoked.
use super::*;
use std::sync::Mutex;

#[derive(Default)]
struct State {
    writes: Vec<String>,
    events: Vec<Event>,
    checks: usize,
    changed_at: Option<usize>,
    refuse_layout: bool,
    refuse_clipboard: bool,
    partial_clipboard: bool,
    fail_event: Option<usize>,
    mac: bool,
}
struct Fake(Arc<Mutex<State>>);
impl Injector for Fake {
    fn check_layout(&mut self, _: &Layout, _: Instant) -> Result<(), PlatformError> {
        if self.0.lock().unwrap().refuse_layout {
            Err(PlatformError::PermissionMissing(
                "synthetic permission refusal".into(),
            ))
        } else {
            Ok(())
        }
    }
    fn emit(&mut self, event: Event) -> Result<(), PlatformError> {
        let mut state = self.0.lock().unwrap();
        state.events.push(event);
        if state.fail_event == Some(state.events.len()) {
            Err(PlatformError::PermissionMissing(
                "synthetic input refusal".into(),
            ))
        } else {
            Ok(())
        }
    }
    fn set_clipboard(&mut self, text: &str) -> Result<(), PlatformError> {
        let mut state = self.0.lock().unwrap();
        if state.refuse_clipboard {
            return Err(PlatformError::TemporarilyUnavailable(
                "synthetic clipboard busy".into(),
            ));
        }
        state.writes.push(text.into());
        if state.partial_clipboard {
            return Err(PlatformError::OutcomeUnknown(
                "synthetic failure after clearing clipboard".into(),
            ));
        }
        Ok(())
    }
    fn check_clipboard(&mut self) -> Result<(), PlatformError> {
        let mut state = self.0.lock().unwrap();
        state.checks += 1;
        if state.changed_at == Some(state.checks) {
            Err(PlatformError::TemporarilyUnavailable(
                "synthetic concurrent copy".into(),
            ))
        } else {
            Ok(())
        }
    }
    fn paste_modifier(&self) -> Key {
        if self.0.lock().unwrap().mac {
            Key::Virtual {
                code: 0x5b,
                extended: true,
            }
        } else {
            Key::Virtual {
                code: 0xa2,
                extended: false,
            }
        }
    }
    fn paste_key(&self) -> Key {
        if self.0.lock().unwrap().mac {
            Key::Character('v')
        } else {
            Key::Virtual {
                code: 0x56,
                extended: false,
            }
        }
    }
}
fn layout() -> Layout {
    Layout::new(vec![Monitor {
        name: "fixture".into(),
        x: 0,
        y: 0,
        width: 1920,
        height: 1080,
        dpi: 96,
        rotation: 0,
    }])
    .unwrap()
}
fn paste() -> InputAction {
    InputAction::Paste {
        text: "Café 日本語\nsecond 😀\r\nthird".into(),
    }
}
#[test]
fn explicit_paste_preserves_unicode_and_multiline_and_never_restores() {
    let layout = layout();
    for mac in [false, true] {
        let state = Arc::new(Mutex::new(State {
            mac,
            ..Default::default()
        }));
        let mut input = ManagedInput::new(Box::new(Fake(state.clone())));
        input
            .inject(&paste(), &layout, &layout.monitors[0])
            .unwrap();
        input.barrier().unwrap();
        drop(input);
        let state = state.lock().unwrap();
        assert_eq!(state.writes.len(), 1);
        let InputAction::Paste { text } = paste() else {
            unreachable!()
        };
        assert!(state.writes[0] == text);
        assert_eq!(state.events.len(), 4);
        assert_eq!(state.checks, 2);
        let modifier = Key::Virtual {
            code: if mac { 0x5b } else { 0xa2 },
            extended: mac,
        };
        assert_eq!(
            state.events.first(),
            Some(&Event::Key {
                key: modifier,
                down: true
            })
        );
        assert_eq!(
            state.events.last(),
            Some(&Event::Key {
                key: modifier,
                down: false
            })
        );
    }
}
#[test]
fn ordinary_text_never_uses_clipboard_even_when_native_text_fails() {
    let layout = layout();
    let state = Arc::new(Mutex::new(State {
        fail_event: Some(1),
        ..Default::default()
    }));
    let mut input = ManagedInput::new(Box::new(Fake(state.clone())));
    assert!(
        input
            .inject(
                &InputAction::Text {
                    text: "日本語".into()
                },
                &layout,
                &layout.monitors[0]
            )
            .is_err()
    );
    assert!(state.lock().unwrap().writes.is_empty());
}
#[test]
fn permission_and_clipboard_refusals_have_no_effect_and_barrier_allows_retry() {
    let layout = layout();
    for permission in [true, false] {
        let state = Arc::new(Mutex::new(State {
            refuse_layout: permission,
            refuse_clipboard: !permission,
            ..Default::default()
        }));
        let mut input = ManagedInput::new(Box::new(Fake(state.clone())));
        assert!(!matches!(
            input.inject(&paste(), &layout, &layout.monitors[0]),
            Ok(()) | Err(PlatformError::OutcomeUnknown(_))
        ));
        assert!(state.lock().unwrap().writes.is_empty());
        assert!(state.lock().unwrap().events.is_empty());
        {
            let mut state = state.lock().unwrap();
            state.refuse_layout = false;
            state.refuse_clipboard = false;
        }
        assert!(
            input
                .inject(&paste(), &layout, &layout.monitors[0])
                .is_err()
        );
        input.barrier().unwrap();
        input
            .inject(&paste(), &layout, &layout.monitors[0])
            .unwrap();
        assert_eq!(state.lock().unwrap().writes.len(), 1);
    }
}
#[test]
fn concurrent_copy_stops_chord_without_republish_and_releases_held_modifier() {
    let layout = layout();
    for changed_at in [1, 2] {
        let state = Arc::new(Mutex::new(State {
            changed_at: Some(changed_at),
            ..Default::default()
        }));
        let mut input = ManagedInput::new(Box::new(Fake(state.clone())));
        assert!(matches!(
            input.inject(&paste(), &layout, &layout.monitors[0]),
            Err(PlatformError::OutcomeUnknown(_))
        ));
        input.barrier().unwrap();
        drop(input);
        let state = state.lock().unwrap();
        assert_eq!(state.writes.len(), 1);
        assert!(state.events.iter().all(|event| !matches!(
            event,
            Event::Key {
                key: Key::Virtual { code: 0x56, .. },
                ..
            }
        )));
        assert_eq!(state.events.len(), if changed_at == 1 { 0 } else { 2 });
    }
}
#[test]
fn failed_chord_after_clipboard_effect_is_unknown_and_cleanup_never_rewrites() {
    let layout = layout();
    for failure in 1..=4 {
        let state = Arc::new(Mutex::new(State {
            fail_event: Some(failure),
            ..Default::default()
        }));
        let mut input = ManagedInput::new(Box::new(Fake(state.clone())));
        assert!(matches!(
            input.inject(&paste(), &layout, &layout.monitors[0]),
            Err(PlatformError::OutcomeUnknown(_))
        ));
        assert!(input.keys.is_empty());
        input.barrier().unwrap();
        drop(input);
        assert_eq!(state.lock().unwrap().writes.len(), 1);
    }
}
#[test]
fn invalid_and_expired_paste_never_changes_clipboard() {
    let layout = layout();
    for action in [
        InputAction::Paste {
            text: "a\0b".into(),
        },
        InputAction::Paste {
            text: "x".repeat(65537),
        },
    ] {
        let state = Arc::new(Mutex::new(State::default()));
        let mut input = ManagedInput::new(Box::new(Fake(state.clone())));
        assert!(input.inject(&action, &layout, &layout.monitors[0]).is_err());
        assert!(state.lock().unwrap().writes.is_empty());
    }
    let state = Arc::new(Mutex::new(State::default()));
    let mut input = ManagedInput::new(Box::new(Fake(state.clone())));
    assert!(
        input
            .inject_before(
                &paste(),
                &layout,
                &layout.monitors[0],
                Instant::now() - Duration::from_secs(1)
            )
            .is_err()
    );
    assert!(state.lock().unwrap().writes.is_empty());
}
#[test]
fn partial_clipboard_failure_is_unknown_and_never_sends_keys_or_retries() {
    let layout = layout();
    let state = Arc::new(Mutex::new(State {
        partial_clipboard: true,
        ..Default::default()
    }));
    let mut input = ManagedInput::new(Box::new(Fake(state.clone())));
    assert!(matches!(
        input.inject(&paste(), &layout, &layout.monitors[0]),
        Err(PlatformError::OutcomeUnknown(_))
    ));
    input.barrier().unwrap();
    drop(input);
    let state = state.lock().unwrap();
    assert_eq!(state.writes.len(), 1);
    assert!(state.events.is_empty());
}
struct ExpiresAfterWrite {
    state: Arc<Mutex<State>>,
    start: Instant,
}
impl std::fmt::Debug for ExpiresAfterWrite {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SyntheticClock")
    }
}
impl Clock for ExpiresAfterWrite {
    fn now(&self) -> protocol::time::Timestamp {
        protocol::time::Timestamp(0)
    }
    fn monotonic(&self) -> Instant {
        self.start
            + if self.state.lock().unwrap().writes.is_empty() {
                Duration::ZERO
            } else {
                Duration::from_secs(1)
            }
    }
}
#[test]
fn expiry_after_replacement_is_unknown_and_emits_no_late_paste_keys() {
    let layout = layout();
    let state = Arc::new(Mutex::new(State::default()));
    let clock = Arc::new(ExpiresAfterWrite {
        state: state.clone(),
        start: Instant::now(),
    });
    let mut input = ManagedInput::new(Box::new(Fake(state.clone()))).with_clock(clock);
    assert!(matches!(
        input.inject(&paste(), &layout, &layout.monitors[0]),
        Err(PlatformError::OutcomeUnknown(_))
    ));
    input.barrier().unwrap();
    drop(input);
    let state = state.lock().unwrap();
    assert_eq!(state.writes.len(), 1);
    // A down attempted after expiry is tracked for cleanup, but never emitted.
    assert!(
        state
            .events
            .iter()
            .all(|event| matches!(event, Event::Key { down: false, .. }))
    );
}

#[test]
fn held_input_refuses_paste_before_any_clipboard_effect() {
    let layout = layout();
    let state = Arc::new(Mutex::new(State::default()));
    let mut input = ManagedInput::new(Box::new(Fake(state.clone())));
    input
        .inject(
            &InputAction::KeyPress {
                key: "shift".into(),
                pressed: true,
            },
            &layout,
            &layout.monitors[0],
        )
        .unwrap();
    assert!(
        input
            .inject(&paste(), &layout, &layout.monitors[0])
            .is_err()
    );
    assert!(state.lock().unwrap().writes.is_empty());
    assert!(input.keys.is_empty());
}
