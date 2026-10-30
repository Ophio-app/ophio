// SPDX-License-Identifier: Apache-2.0
//! Input sequencing is OS-independent so failure cleanup can be exhaustively tested.
use super::geometry::{Layout, Monitor, invalid};
use ports::desktop::PlatformError;
use ports::{Clock, SystemClock};
use protocol::control::{InputAction, PointerButton, TargetPoint};
use std::{
    collections::BTreeSet,
    sync::Arc,
    time::{Duration, Instant},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Key {
    Virtual { code: u16, extended: bool },
    Unicode(u16),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Button {
    Left,
    Middle,
    Right,
}
impl From<PointerButton> for Button {
    fn from(value: PointerButton) -> Self {
        match value {
            PointerButton::Left => Self::Left,
            PointerButton::Middle => Self::Middle,
            PointerButton::Right => Self::Right,
        }
    }
}
// Debug is redacted: even Unicode code units can reveal typed text.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Event {
    Move {
        absolute: (i32, i32),
        physical: (i32, i32),
    },
    Button {
        button: Button,
        down: bool,
    },
    Key {
        key: Key,
        down: bool,
    },
    Wheel {
        horizontal: bool,
        delta: i32,
    },
}
impl std::fmt::Debug for Event {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("InputEvent([redacted])")
    }
}
/// An injector acknowledges only OS queue insertion. It must refuse secure
/// desktops/elevated targets and return an error for every uninserted event.
pub trait Injector: Send {
    fn check_layout(&mut self, layout: &Layout, deadline: Instant) -> Result<(), PlatformError>;
    fn emit(&mut self, event: Event) -> Result<(), PlatformError>;
}
#[derive(Debug, Default)]
pub struct Wheel {
    x: f64,
    y: f64,
}
impl Wheel {
    /// Protocol scrolling matches the Linux 15-unit detent; Windows uses 120.
    pub fn accumulate(&mut self, dx: f64, dy: f64) -> Result<(i32, i32), PlatformError> {
        if !dx.is_finite() || !dy.is_finite() || dx.abs() > 1500.0 || dy.abs() > 1500.0 {
            return Err(invalid());
        }
        self.x += dx * 8.0;
        self.y -= dy * 8.0;
        let result = (self.x.trunc() as i32, self.y.trunc() as i32);
        self.x -= f64::from(result.0);
        self.y -= f64::from(result.1);
        Ok(result)
    }
}
/// Tracks uncertain down attempts before calling the injector. Cleanup always
/// tries every held item; failures remain held for a later retry and for Drop.
pub struct ManagedInput {
    injector: Box<dyn Injector>,
    keys: BTreeSet<Key>,
    buttons: BTreeSet<Button>,
    wheel: Wheel,
    failed: bool,
    effects_started: bool,
    clock: Arc<dyn Clock>,
    deadline: Option<Instant>,
}
impl std::fmt::Debug for ManagedInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ManagedInput")
            .field("failed", &self.failed)
            .finish_non_exhaustive()
    }
}
impl ManagedInput {
    pub fn new(injector: Box<dyn Injector>) -> Self {
        Self {
            injector,
            keys: BTreeSet::new(),
            buttons: BTreeSet::new(),
            wheel: Wheel::default(),
            failed: false,
            effects_started: false,
            clock: Arc::new(SystemClock),
            deadline: None,
        }
    }
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }
    fn emit(&mut self, event: Event) -> Result<(), PlatformError> {
        let release = matches!(
            event,
            Event::Key { down: false, .. } | Event::Button { down: false, .. }
        );
        if !release
            && self
                .deadline
                .is_some_and(|end| self.clock.monotonic() >= end)
        {
            return Err(PlatformError::TemporarilyUnavailable(
                "input action exceeded its execution window".into(),
            ));
        }
        self.injector.emit(event)?;
        self.effects_started = true;
        Ok(())
    }
    fn key(&mut self, key: Key, down: bool) -> Result<(), PlatformError> {
        if down {
            if !self.keys.insert(key) {
                return Ok(());
            }
        } else if !self.keys.contains(&key) {
            return Ok(());
        }
        self.emit(Event::Key { key, down })?;
        if !down {
            self.keys.remove(&key);
        }
        Ok(())
    }
    fn button(&mut self, button: Button, down: bool) -> Result<(), PlatformError> {
        if down {
            if !self.buttons.insert(button) {
                return Ok(());
            }
        } else if !self.buttons.contains(&button) {
            return Ok(());
        }
        self.emit(Event::Button { button, down })?;
        if !down {
            self.buttons.remove(&button);
        }
        Ok(())
    }
    fn point(
        &mut self,
        layout: &Layout,
        target: &Monitor,
        point: TargetPoint,
    ) -> Result<(), PlatformError> {
        let physical = layout.physical(target, point)?;
        self.emit(Event::Move {
            absolute: layout.absolute(physical.0, physical.1)?,
            physical,
        })
    }
    pub fn inject(
        &mut self,
        action: &InputAction,
        layout: &Layout,
        target: &Monitor,
    ) -> Result<(), PlatformError> {
        self.inject_before(
            action,
            layout,
            target,
            self.clock.monotonic() + Duration::from_millis(250),
        )
    }
    pub(crate) fn inject_before(
        &mut self,
        action: &InputAction,
        layout: &Layout,
        target: &Monitor,
        deadline: Instant,
    ) -> Result<(), PlatformError> {
        if matches!(action, InputAction::ReleaseAll) {
            return self.release_all();
        }
        self.deadline = Some(deadline);
        self.effects_started = false;
        let result = (|| {
            if self.failed {
                return Err(PlatformError::TemporarilyUnavailable(
                    "input sink stopped after a failure; acquire a new sink".into(),
                ));
            }
            self.injector.check_layout(layout, deadline)?;
            match action {
                InputAction::PointerMove { to } => self.point(layout, target, *to)?,
                InputAction::PointerButton {
                    button,
                    pressed,
                    at,
                } => {
                    self.point(layout, target, *at)?;
                    self.button((*button).into(), *pressed)?;
                }
                InputAction::Click { button, at, count } => {
                    if !(1..=3).contains(count) || self.buttons.contains(&(*button).into()) {
                        return Err(invalid());
                    }
                    self.point(layout, target, *at)?;
                    for _ in 0..*count {
                        self.button((*button).into(), true)?;
                        self.button((*button).into(), false)?;
                    }
                }
                InputAction::Scroll { at, dx, dy } => {
                    let (x, y) = self.wheel.accumulate(*dx, *dy)?;
                    self.point(layout, target, *at)?;
                    for (horizontal, delta) in [(true, x), (false, y)] {
                        if delta != 0 {
                            self.emit(Event::Wheel { horizontal, delta })?;
                        }
                    }
                }
                InputAction::KeyPress { key, pressed } => self.key(key_code(key)?, *pressed)?,
                InputAction::KeyChord { keys } => {
                    if keys.is_empty() || keys.len() > 16 {
                        return Err(invalid());
                    }
                    let keys: Vec<_> =
                        keys.iter().map(|k| key_code(k)).collect::<Result<_, _>>()?;
                    if keys.iter().collect::<BTreeSet<_>>().len() != keys.len() {
                        return Err(invalid());
                    }
                    let newly_pressed: Vec<_> = keys
                        .into_iter()
                        .filter(|k| !self.keys.contains(k))
                        .collect();
                    for key in &newly_pressed {
                        self.key(*key, true)?;
                    }
                    for key in newly_pressed.into_iter().rev() {
                        self.key(key, false)?;
                    }
                }
                InputAction::Text { text } => {
                    if !self.keys.is_empty() || text.encode_utf16().count() > 4096 {
                        return Err(invalid());
                    }
                    for unit in text.encode_utf16() {
                        self.key(Key::Unicode(unit), true)?;
                        self.key(Key::Unicode(unit), false)?;
                    }
                }
                InputAction::ReleaseAll => unreachable!(),
                InputAction::Paste { .. } => {
                    return Err(PlatformError::Unsupported(
                        "Clipboard paste is not available on this route".into(),
                    ));
                }
            }
            Ok(())
        })();
        self.deadline = None;
        let result = if self.effects_started {
            result.map_err(|_: PlatformError| {
                PlatformError::OutcomeUnknown("Only part of the input action was confirmed".into())
            })
        } else {
            result
        };
        if result.is_err() {
            self.failed = true;
            let _ = self.release_all();
        }
        result
    }
    /// Stop on a backend/geometry failure that happened before sequencing.
    pub fn fail(&mut self) {
        self.failed = true;
        let _ = self.release_all();
    }
    /// Explicit native barrier; a fresh lease may use the sink after success.
    pub fn barrier(&mut self) -> Result<(), PlatformError> {
        self.release_all()?;
        self.failed = false;
        Ok(())
    }
    pub fn release_all(&mut self) -> Result<(), PlatformError> {
        let mut first = None;
        for key in self.keys.clone() {
            if let Err(e) = self.key(key, false)
                && first.is_none()
            {
                first = Some(e);
            }
        }
        for button in self.buttons.clone() {
            if let Err(e) = self.button(button, false)
                && first.is_none()
            {
                first = Some(e);
            }
        }
        self.wheel = Wheel::default();
        if let Some(e) = first {
            self.failed = true;
            Err(e)
        } else {
            Ok(())
        }
    }
}
impl Drop for ManagedInput {
    fn drop(&mut self) {
        let _ = self.release_all();
    }
}
fn key_code(name: &str) -> Result<Key, PlatformError> {
    let (code, extended) = match name {
        "ctrl" | "control" => (0x11, false),
        "shift" => (0x10, false),
        "alt" => (0x12, false),
        "meta" | "super" => (0x5b, true),
        "Enter" | "enter" => (0x0d, false),
        "Tab" | "tab" => (9, false),
        "Escape" | "escape" => (0x1b, false),
        "Backspace" | "backspace" => (8, false),
        "Delete" | "delete" => (0x2e, true),
        "Insert" | "insert" => (0x2d, true),
        "Home" | "home" => (0x24, true),
        "End" | "end" => (0x23, true),
        "PageUp" | "pageup" => (0x21, true),
        "PageDown" | "pagedown" => (0x22, true),
        "ArrowLeft" | "arrowleft" => (0x25, true),
        "ArrowUp" | "arrowup" => (0x26, true),
        "ArrowRight" | "arrowright" => (0x27, true),
        "ArrowDown" | "arrowdown" => (0x28, true),
        " " | "space" => (0x20, false),
        "CapsLock" | "capslock" => (0x14, false),
        "NumLock" | "numlock" => (0x90, true),
        "ScrollLock" | "scrolllock" => (0x91, false),
        "PrintScreen" | "printscreen" => (0x2c, true),
        "Pause" | "pause" => (0x13, false),
        _ if name.len() == 1 && name.as_bytes()[0].is_ascii_alphanumeric() => {
            (u16::from(name.as_bytes()[0].to_ascii_uppercase()), false)
        }
        _ => {
            let f = name
                .strip_prefix('F')
                .or_else(|| name.strip_prefix('f'))
                .and_then(|v| v.parse::<u16>().ok())
                .filter(|n| (1..=24).contains(n));
            if let Some(n) = f {
                (0x70 + n - 1, false)
            } else {
                return Err(PlatformError::Unsupported(
                    "unsupported key name; use text input for characters".into(),
                ));
            }
        }
    };
    Ok(Key::Virtual { code, extended })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    #[derive(Debug)]
    struct FrozenClock(Instant);
    impl Clock for FrozenClock {
        fn now(&self) -> protocol::time::Timestamp {
            protocol::time::Timestamp(0)
        }
        fn monotonic(&self) -> Instant {
            self.0
        }
    }
    #[derive(Default)]
    struct Log {
        events: Vec<Event>,
        fail_at: Option<usize>,
        refuse_releases: bool,
    }
    struct Fake(Arc<Mutex<Log>>);
    impl Injector for Fake {
        fn check_layout(&mut self, _: &Layout, _: Instant) -> Result<(), PlatformError> {
            Ok(())
        }
        fn emit(&mut self, event: Event) -> Result<(), PlatformError> {
            let mut log = self.0.lock().unwrap();
            log.events.push(event);
            if log.fail_at == Some(log.events.len())
                || (log.refuse_releases
                    && matches!(
                        event,
                        Event::Key { down: false, .. } | Event::Button { down: false, .. }
                    ))
            {
                return Err(PlatformError::PermissionMissing(
                    "input refused by UIPI".into(),
                ));
            }
            Ok(())
        }
    }
    fn layout() -> Layout {
        Layout::new(vec![Monitor {
            name: "one".into(),
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
            dpi: 96,
            rotation: 0,
        }])
        .unwrap()
    }
    #[test]
    fn releases_every_key_and_button_on_drop() {
        let log = Arc::new(Mutex::new(Log::default()));
        let l = layout();
        let mut sink = ManagedInput::new(Box::new(Fake(log.clone())))
            .with_clock(Arc::new(FrozenClock(Instant::now())));
        for key in ["ctrl", "shift", "a"] {
            sink.inject(
                &InputAction::KeyPress {
                    key: key.into(),
                    pressed: true,
                },
                &l,
                &l.monitors[0],
            )
            .unwrap();
        }
        for button in [
            PointerButton::Left,
            PointerButton::Middle,
            PointerButton::Right,
        ] {
            sink.inject(
                &InputAction::PointerButton {
                    button,
                    pressed: true,
                    at: TargetPoint { x: 0.5, y: 0.5 },
                },
                &l,
                &l.monitors[0],
            )
            .unwrap();
        }
        drop(sink);
        let log = log.lock().unwrap();
        assert_eq!(
            log.events
                .iter()
                .filter(|e| matches!(e, Event::Key { down: false, .. }))
                .count(),
            3
        );
        assert_eq!(
            log.events
                .iter()
                .filter(|e| matches!(e, Event::Button { down: false, .. }))
                .count(),
            3
        );
    }
    #[test]
    fn failure_mid_chord_releases_keys_and_preserves_partial_outcome() {
        for failure in 1..=6 {
            let log = Arc::new(Mutex::new(Log {
                fail_at: Some(failure),
                ..Log::default()
            }));
            let l = layout();
            let mut sink = ManagedInput::new(Box::new(Fake(log.clone())))
                .with_clock(Arc::new(FrozenClock(Instant::now())));
            let error = sink
                .inject(
                    &InputAction::KeyChord {
                        keys: vec!["ctrl".into(), "shift".into(), "a".into()],
                    },
                    &l,
                    &l.monitors[0],
                )
                .unwrap_err();
            if failure == 1 {
                assert!(matches!(error, PlatformError::PermissionMissing(_)));
            } else {
                assert!(matches!(error, PlatformError::OutcomeUnknown(_)));
            }
            assert!(sink.keys.is_empty());
            assert!(
                sink.inject(&InputAction::Text { text: "x".into() }, &l, &l.monitors[0])
                    .is_err()
            );
            let log = log.lock().unwrap();
            for e in &log.events {
                if let Event::Key { key, down: true } = e {
                    assert!(log.events.contains(&Event::Key {
                        key: *key,
                        down: false
                    }));
                }
            }
        }
    }
    #[test]
    fn pointer_failure_releases_keys_existing_buttons_and_uncertain_down() {
        let log = Arc::new(Mutex::new(Log::default()));
        let layout = layout();
        let target = &layout.monitors[0];
        let mut sink = ManagedInput::new(Box::new(Fake(log.clone())))
            .with_clock(Arc::new(FrozenClock(Instant::now())));
        sink.inject(
            &InputAction::KeyPress {
                key: "ctrl".into(),
                pressed: true,
            },
            &layout,
            target,
        )
        .unwrap();
        let at = TargetPoint { x: 0.5, y: 0.5 };
        sink.inject(
            &InputAction::PointerButton {
                button: PointerButton::Middle,
                pressed: true,
                at,
            },
            &layout,
            target,
        )
        .unwrap();
        {
            let mut log = log.lock().unwrap();
            log.fail_at = Some(log.events.len() + 2);
        }
        assert!(matches!(
            sink.inject(
                &InputAction::Click {
                    button: PointerButton::Left,
                    at,
                    count: 1
                },
                &layout,
                target
            ),
            Err(PlatformError::OutcomeUnknown(_))
        ));
        assert!(sink.keys.is_empty());
        assert!(sink.buttons.is_empty());
        let log = log.lock().unwrap();
        assert!(log.events.contains(&Event::Key {
            key: key_code("ctrl").unwrap(),
            down: false
        }));
        for button in [Button::Left, Button::Middle] {
            assert!(log.events.contains(&Event::Button {
                button,
                down: false
            }));
        }
    }
    #[test]
    fn cleanup_attempts_all_despite_refused_releases_and_retries() {
        let log = Arc::new(Mutex::new(Log::default()));
        let l = layout();
        let mut sink = ManagedInput::new(Box::new(Fake(log.clone())))
            .with_clock(Arc::new(FrozenClock(Instant::now())));
        for key in ["ctrl", "shift"] {
            sink.inject(
                &InputAction::KeyPress {
                    key: key.into(),
                    pressed: true,
                },
                &l,
                &l.monitors[0],
            )
            .unwrap();
        }
        log.lock().unwrap().refuse_releases = true;
        assert!(sink.release_all().is_err());
        assert_eq!(sink.keys.len(), 2);
        assert_eq!(log.lock().unwrap().events.len(), 4);
        log.lock().unwrap().refuse_releases = false;
        sink.barrier().unwrap();
        assert!(sink.keys.is_empty());
        assert!(!sink.failed);
        sink.inject(
            &InputAction::Text {
                text: "recovered".into(),
            },
            &l,
            &l.monitors[0],
        )
        .unwrap();
    }
    #[test]
    fn wheel_accumulates_and_unicode_surrogates_get_matching_releases() {
        let mut wheel = Wheel::default();
        assert_eq!(wheel.accumulate(0.05, 0.05).unwrap(), (0, 0));
        assert_eq!(wheel.accumulate(0.1, 0.1).unwrap(), (1, -1));
        assert!(wheel.accumulate(f64::NAN, 0.0).is_err());
        let log = Arc::new(Mutex::new(Log::default()));
        let l = layout();
        let mut sink = ManagedInput::new(Box::new(Fake(log.clone())))
            .with_clock(Arc::new(FrozenClock(Instant::now())));
        sink.inject(
            &InputAction::Text {
                text: "😀".into()
            },
            &l,
            &l.monitors[0],
        )
        .unwrap();
        assert_eq!(log.lock().unwrap().events.len(), 4);
        assert!(sink.keys.is_empty());
    }
}

#[cfg(test)]
mod timing_tests {
    use super::*;
    use std::sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    };
    #[derive(Debug)]
    struct FakeClock {
        base: Instant,
        millis: AtomicU64,
    }
    impl Clock for FakeClock {
        fn now(&self) -> protocol::time::Timestamp {
            protocol::time::Timestamp(0)
        }
        fn monotonic(&self) -> Instant {
            self.base + Duration::from_millis(self.millis.load(Ordering::Relaxed))
        }
    }
    struct Timed {
        clock: Arc<FakeClock>,
        events: Arc<Mutex<Vec<Event>>>,
    }
    impl Injector for Timed {
        fn check_layout(&mut self, _: &Layout, _: Instant) -> Result<(), PlatformError> {
            Ok(())
        }
        fn emit(&mut self, event: Event) -> Result<(), PlatformError> {
            self.events.lock().unwrap().push(event);
            self.clock.millis.store(300, Ordering::Relaxed);
            Ok(())
        }
    }
    #[test]
    fn slow_action_stops_new_downs_but_finishes_releases() {
        let clock = Arc::new(FakeClock {
            base: Instant::now(),
            millis: AtomicU64::new(0),
        });
        let events = Arc::new(Mutex::new(Vec::new()));
        let mut sink = ManagedInput::new(Box::new(Timed {
            clock: clock.clone(),
            events: events.clone(),
        }))
        .with_clock(clock);
        let layout = Layout::new(vec![Monitor {
            name: "one".into(),
            x: 0,
            y: 0,
            width: 10,
            height: 10,
            dpi: 96,
            rotation: 0,
        }])
        .unwrap();
        let result = sink.inject(
            &InputAction::Text { text: "ab".into() },
            &layout,
            &layout.monitors[0],
        );
        assert!(matches!(result, Err(PlatformError::OutcomeUnknown(_))));
        let events = events.lock().unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, Event::Key { down: true, .. }))
                .count(),
            1
        );
        assert!(events.contains(&Event::Key {
            key: Key::Unicode(u16::from(b'a')),
            down: false
        }));
        assert!(sink.keys.is_empty());
    }
}
