// SPDX-License-Identifier: Apache-2.0
use crate::{
    LinuxDesktop,
    native::{Event, NativeInput, Output},
};
use async_trait::async_trait;
use ports::desktop::{InputSink, PlatformError};
use protocol::{
    control::{InputAction, PointerButton, TargetGeometry, TargetPoint},
    ids::TargetId,
};
use std::{
    collections::BTreeSet,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
type NativeResult = (Box<dyn NativeInput>, Result<(), PlatformError>);

pub(crate) struct ManagedInput {
    desktop: LinuxDesktop,
    target: TargetId,
    native: Option<Box<dyn NativeInput>>,
    pending: Option<tokio::task::JoinHandle<NativeResult>>,
    cancel: Arc<AtomicBool>,
    effects_started: bool,
    keys: BTreeSet<u32>,
    buttons: BTreeSet<u32>,
}
impl ManagedInput {
    pub fn new(desktop: LinuxDesktop, target: TargetId, native: Box<dyn NativeInput>) -> Self {
        Self {
            desktop,
            target,
            native: Some(native),
            pending: None,
            cancel: Arc::new(AtomicBool::new(false)),
            effects_started: false,
            keys: BTreeSet::new(),
            buttons: BTreeSet::new(),
        }
    }
    // Keep ownership of native work across cancellation of the Hub's timeout.
    // In particular, spawn_blocking work is not cancelled when its waiter drops.
    async fn settle(&mut self) -> Result<(), PlatformError> {
        let Some(pending) = self.pending.as_mut() else {
            return Ok(());
        };
        let result = pending.await;
        self.pending = None;
        let (native, result) = result.map_err(|_| {
            PlatformError::OutcomeUnknown(
                "The input helper ended before confirming the action".into(),
            )
        })?;
        self.native = Some(native);
        result
    }
    async fn native_call(&mut self, event: Option<Event>) -> Result<(), PlatformError> {
        self.settle().await?;
        let mut native = self.native.take().ok_or_else(|| {
            PlatformError::OutcomeUnknown("Native input worker is unavailable".into())
        })?;
        self.cancel.store(false, Ordering::SeqCst);
        let cancel = self.cancel.clone();
        let desktop = self.desktop.clone();
        let is_event = event.is_some();
        self.pending = Some(tokio::spawn(async move {
            let result = if cancel.load(Ordering::SeqCst) {
                Ok(())
            } else if let Some(event) = event {
                let result = if event.is_release() {
                    Ok(())
                } else {
                    desktop.check_lock().await
                };
                match result {
                    Ok(()) => {
                        native
                            .event_fenced(
                                event,
                                crate::native::InputFence {
                                    cancelled: cancel,
                                    lock: desktop.lock.clone(),
                                },
                            )
                            .await
                    }
                    Err(error) => Err(error),
                }
            } else {
                native.barrier().await
            };
            (native, result)
        }));
        let result = self.settle().await;
        if is_event && result.is_ok() {
            self.effects_started = true;
        }
        result
    }
    async fn clipboard(&mut self, text: String) -> Result<(), PlatformError> {
        self.settle().await?;
        let mut native = self.native.take().ok_or_else(|| {
            PlatformError::OutcomeUnknown("Native input worker is unavailable".into())
        })?;
        let desktop = self.desktop.clone();
        self.cancel.store(false, Ordering::SeqCst);
        let fence = crate::native::InputFence {
            cancelled: self.cancel.clone(),
            lock: desktop.lock.clone(),
        };
        self.pending = Some(tokio::spawn(async move {
            let result = match desktop.check_lock().await {
                Ok(()) => native.set_clipboard(&text, fence).await,
                Err(error) => Err(error),
            };
            (native, result)
        }));
        self.settle().await?;
        self.effects_started = true;
        Ok(())
    }
    async fn motion(&mut self, output: &Output, point: TargetPoint) -> Result<(), PlatformError> {
        let (x, y) = coordinates(output, point)?;
        self.native_call(Some(Event::Motion { x, y })).await
    }
    async fn button(&mut self, code: u32, pressed: bool) -> Result<(), PlatformError> {
        if pressed {
            self.buttons.insert(code);
        }
        self.native_call(Some(Event::Button { code, pressed }))
            .await?;
        if !pressed {
            self.buttons.remove(&code);
        }
        Ok(())
    }
    async fn key(&mut self, keysym: u32, pressed: bool) -> Result<(), PlatformError> {
        if pressed {
            self.keys.insert(keysym);
        }
        self.native_call(Some(Event::Key { keysym, pressed }))
            .await?;
        if !pressed {
            self.keys.remove(&keysym);
        }
        Ok(())
    }
}
#[async_trait]
impl InputSink for ManagedInput {
    async fn inject(
        &mut self,
        action: &InputAction,
        geometry: &TargetGeometry,
    ) -> Result<(), PlatformError> {
        if matches!(action, InputAction::ReleaseAll) {
            return self.release_all().await;
        }
        self.desktop.check_lock().await?;
        let (output, current) = self.desktop.resolve(self.target).await?;
        if *geometry != current {
            return Err(PlatformError::GeometryChanged);
        }
        self.effects_started = false;
        let result = async {
            match action {
                InputAction::PointerMove { to } => self.motion(&output, *to).await,
                InputAction::PointerButton {
                    button,
                    pressed,
                    at,
                } => {
                    self.motion(&output, *at).await?;
                    self.button(button_code(*button), *pressed).await
                }
                InputAction::Click { button, at, count } => {
                    if !(1..=3).contains(count) {
                        return Err(invalid("click count must be 1..3"));
                    }
                    let code = button_code(*button);
                    if self.buttons.contains(&code) {
                        return Err(invalid("cannot click an already held button"));
                    }
                    self.motion(&output, *at).await?;
                    for _ in 0..*count {
                        self.button(code, true).await?;
                        self.button(code, false).await?;
                    }
                    Ok(())
                }
                InputAction::Scroll { at, dx, dy } => {
                    if !dx.is_finite()
                        || !dy.is_finite()
                        || dx.abs() > 10000.0
                        || dy.abs() > 10000.0
                    {
                        return Err(invalid("invalid scroll distance"));
                    }
                    self.motion(&output, *at).await?;
                    self.native_call(Some(Event::Axis { dx: *dx, dy: *dy }))
                        .await
                }
                InputAction::KeyPress { key, pressed } => self.key(keysym(key)?, *pressed).await,
                InputAction::KeyChord { keys } => {
                    if keys.is_empty() || keys.len() > 16 {
                        return Err(invalid("key chord length must be 1..16"));
                    }
                    let symbols = keys
                        .iter()
                        .map(|key| keysym(key))
                        .collect::<Result<Vec<_>, _>>()?;
                    if symbols.iter().copied().collect::<BTreeSet<_>>().len() != symbols.len() {
                        return Err(invalid("key chord contains duplicate keys"));
                    }
                    if symbols.iter().any(|key| self.keys.contains(key)) {
                        return Err(invalid("chord contains an already held key"));
                    }
                    for key in &symbols {
                        self.key(*key, true).await?;
                    }
                    for key in symbols.iter().rev() {
                        self.key(*key, false).await?;
                    }
                    Ok(())
                }
                InputAction::Text { text } => {
                    if text.chars().count() > 4096 {
                        return Err(invalid("text action exceeds 4096 characters"));
                    }
                    if !self.keys.is_empty() {
                        return Err(invalid("release held keys before typing text"));
                    }
                    let symbols = text
                        .chars()
                        .map(character_keysym)
                        .collect::<Result<Vec<_>, _>>()?;
                    self.settle().await?;
                    self.native
                        .as_mut()
                        .ok_or_else(|| {
                            PlatformError::OutcomeUnknown(
                                "Native input worker is unavailable".into(),
                            )
                        })?
                        .validate_text(text)
                        .await?;
                    for key in symbols {
                        self.key(key, true).await?;
                        self.key(key, false).await?;
                    }
                    Ok(())
                }
                InputAction::ReleaseAll => unreachable!(),
                InputAction::Paste { text } => {
                    if !self.keys.is_empty() || text.len() > 65_536 {
                        return Err(invalid(
                            "Release held keys before pasting at most 65536 bytes",
                        ));
                    }
                    self.clipboard(text.clone()).await?;
                    self.key(0xffe3, true).await?;
                    self.key(u32::from('v'), true).await?;
                    self.key(u32::from('v'), false).await?;
                    self.key(0xffe3, false).await
                }
            }
        }
        .await;
        let result = if self.effects_started {
            result.map_err(|_| {
                PlatformError::OutcomeUnknown("Only part of the input action was confirmed".into())
            })
        } else {
            result
        };
        if result.is_err() {
            let _ = self.release_all().await;
        }
        result
    }
    async fn release_all(&mut self) -> Result<(), PlatformError> {
        self.cancel.store(true, Ordering::SeqCst);
        // An earlier delivery error is separate from this barrier. Join it, then
        // ask the native connection to prove that no outstanding request remains.
        let _ = self.settle().await;
        self.native_call(None).await?;
        let mut failure = None;
        for key in self.keys.clone() {
            if let Err(error) = self.key(key, false).await {
                failure = Some(error);
            }
        }
        for code in self.buttons.clone() {
            if let Err(error) = self.button(code, false).await {
                failure = Some(error);
            }
        }
        self.native_call(None).await?;
        failure.map_or(Ok(()), Err)
    }
}
fn invalid(message: &str) -> PlatformError {
    PlatformError::Unsupported(message.into())
}
pub fn coordinates(output: &Output, point: TargetPoint) -> Result<(f64, f64), PlatformError> {
    if !point.x.is_finite()
        || !point.y.is_finite()
        || !(0.0..=1.0).contains(&point.x)
        || !(0.0..=1.0).contains(&point.y)
    {
        return Err(invalid("point outside target"));
    }
    Ok((
        output.x + point.x * f64::from(output.width - 1) / f64::from(output.scale),
        output.y + point.y * f64::from(output.height - 1) / f64::from(output.scale),
    ))
}
fn button_code(button: PointerButton) -> u32 {
    match button {
        PointerButton::Left => 0x110,
        PointerButton::Right => 0x111,
        PointerButton::Middle => 0x112,
    }
}
pub fn character_keysym(c: char) -> Result<u32, PlatformError> {
    Ok(match c {
        '\n' | '\r' => 0xff0d,
        '\t' => 0xff09,
        c if c.is_control() => return Err(invalid("unsupported control character")),
        c if u32::from(c) <= 0xff => u32::from(c),
        c => 0x0100_0000 | u32::from(c),
    })
}
pub fn keysym(key: &str) -> Result<u32, PlatformError> {
    if key.chars().count() == 1 {
        return character_keysym(key.chars().next().ok_or_else(|| invalid("empty key"))?);
    }
    Ok(match key {
        "ctrl" | "control" => 0xffe3,
        "shift" => 0xffe1,
        "alt" => 0xffe9,
        "meta" | "super" => 0xffeb,
        "Enter" => 0xff0d,
        "Tab" => 0xff09,
        "Escape" => 0xff1b,
        "Backspace" => 0xff08,
        "Delete" => 0xffff,
        "ArrowLeft" => 0xff51,
        "ArrowUp" => 0xff52,
        "ArrowRight" => 0xff53,
        "ArrowDown" => 0xff54,
        "Home" => 0xff50,
        "End" => 0xff57,
        "PageUp" => 0xff55,
        "PageDown" => 0xff56,
        "Insert" => 0xff63,
        " " | "Space" => 0x20,
        "CapsLock" => 0xffe5,
        name if name.starts_with('F') => {
            let n = name[1..]
                .parse::<u32>()
                .map_err(|_| invalid("unknown key name"))?;
            if !(1..=24).contains(&n) {
                return Err(invalid("unknown function key"));
            }
            0xffbd + n
        }
        _ => return Err(invalid("unknown key name")),
    })
}

impl Drop for ManagedInput {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::SeqCst);
        let pending = self.pending.take();
        let native = self.native.take();
        let keys = std::mem::take(&mut self.keys);
        let buttons = std::mem::take(&mut self.buttons);
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                let native = if let Some(pending) = pending {
                    pending.await.ok().map(|(native, _)| native)
                } else {
                    native
                };
                if let Some(mut native) = native {
                    let _ = native.barrier().await;
                    for keysym in keys {
                        let _ = native
                            .event(Event::Key {
                                keysym,
                                pressed: false,
                            })
                            .await;
                    }
                    for code in buttons {
                        let _ = native
                            .event(Event::Button {
                                code,
                                pressed: false,
                            })
                            .await;
                    }
                    let _ = native.barrier().await;
                }
            });
        }
    }
}

#[cfg(test)]
mod refusal_tests {
    use super::*;
    use crate::native::{Driver, NativeInput};
    use std::sync::Mutex;

    #[derive(Debug, Default)]
    struct Witness(Arc<Mutex<Vec<&'static str>>>);
    struct Portal(Arc<Mutex<Vec<&'static str>>>);
    #[async_trait]
    impl NativeInput for Portal {
        async fn event(&mut self, _: Event) -> Result<(), PlatformError> {
            self.0.lock().unwrap().push("key");
            Ok(())
        }
        async fn barrier(&mut self) -> Result<(), PlatformError> {
            Ok(())
        }
        async fn validate_text(&mut self, text: &str) -> Result<(), PlatformError> {
            let symbols = "Caf "
                .chars()
                .map(|c| character_keysym(c).unwrap())
                .collect();
            crate::text::validate(text, &symbols, true)
        }
        async fn set_clipboard(
            &mut self,
            text: &str,
            fence: crate::native::InputFence,
        ) -> Result<(), PlatformError> {
            fence.check(false)?;
            assert_eq!(text, "Café 日本語");
            self.0.lock().unwrap().push("clipboard");
            Ok(())
        }
    }
    #[async_trait]
    impl Driver for Witness {
        async fn outputs(&self) -> Result<Vec<Output>, PlatformError> {
            Ok(vec![Output {
                name: "monitor".into(),
                x: 0.,
                y: 0.,
                width: 1920,
                height: 1080,
                scale: 1.25,
                rotation: 0,
            }])
        }
        async fn capture(&self, _: &Output) -> Result<ports::CapturedFrame, PlatformError> {
            Err(PlatformError::Unsupported(
                "No capture in input test".into(),
            ))
        }
        async fn input(&self) -> Result<Box<dyn NativeInput>, PlatformError> {
            Ok(Box::new(Portal(self.0.clone())))
        }
    }
    #[tokio::test]
    async fn text_preflight_sends_nothing_and_only_explicit_paste_changes_clipboard() {
        use ports::DesktopBackend;
        let witness = Arc::new(Witness::default());
        let available = protocol::capability::Availability::Available;
        let desktop = LinuxDesktop::new(
            witness.clone(),
            ports::DesktopCapabilities {
                backend: "test portal".into(),
                capture: available.clone(),
                input: available.clone(),
                lock_session: available.clone(),
                unlock_session: available,
                input_scope: protocol::capability::InputScope::FocusedTarget,
            },
            None,
        );
        let target = desktop.targets().await.unwrap().remove(0);
        let mut sink = desktop.input_sink(target.target_id).await.unwrap();
        let text = "Café 日本語".to_owned();
        assert!(matches!(
            sink.inject(&InputAction::Text { text: text.clone() }, &target.geometry)
                .await,
            Err(PlatformError::TextUnavailable {
                paste_available: true,
                ..
            })
        ));
        assert!(witness.0.lock().unwrap().is_empty());
        sink.inject(&InputAction::Paste { text }, &target.geometry)
            .await
            .unwrap();
        assert_eq!(
            *witness.0.lock().unwrap(),
            ["clipboard", "key", "key", "key", "key"]
        );
    }
}
