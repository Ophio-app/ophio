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
        self.native_step(event, false).await
    }
    /// One native step: an event, or a barrier that with `idle` also ends the
    /// native side's control, removing any devices it added.
    async fn native_step(&mut self, event: Option<Event>, idle: bool) -> Result<(), PlatformError> {
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
                match native.barrier().await {
                    Ok(()) if idle => native.idle().await,
                    result => result,
                }
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
        self.key_as(keysym, pressed, false).await
    }
    /// `text` types exactly the character rather than pressing the key it
    /// names; see [`Event::Key`].
    async fn key_as(
        &mut self,
        keysym: u32,
        pressed: bool,
        text: bool,
    ) -> Result<(), PlatformError> {
        if pressed {
            self.keys.insert(keysym);
        }
        self.native_call(Some(Event::Key {
            keysym,
            pressed,
            text,
        }))
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
                InputAction::KeyPress { key, pressed } => {
                    let symbol = keysym(key)?;
                    let symbol = if *pressed {
                        if self.keys.iter().any(|k| SHORTCUT_MODIFIERS.contains(k)) {
                            shortcut_letter(symbol)
                        } else {
                            symbol
                        }
                    } else if !self.keys.contains(&symbol)
                        && self.keys.contains(&shortcut_letter(symbol))
                    {
                        // Pressed while Ctrl was held: the small letter is held.
                        shortcut_letter(symbol)
                    } else {
                        symbol
                    };
                    self.key(symbol, *pressed).await
                }
                InputAction::KeyChord { keys } => {
                    if keys.is_empty() || keys.len() > 16 {
                        return Err(invalid("key chord length must be 1..16"));
                    }
                    let mut symbols = keys
                        .iter()
                        .map(|key| keysym(key))
                        .collect::<Result<Vec<_>, _>>()?;
                    // A modifier held through key_press counts as in the chord.
                    if symbols
                        .iter()
                        .chain(self.keys.iter())
                        .any(|k| SHORTCUT_MODIFIERS.contains(k))
                    {
                        symbols = symbols.into_iter().map(shortcut_letter).collect();
                    }
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
                        self.key_as(key, true, true).await?;
                        self.key_as(key, false, true).await?;
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
                    self.settle().await?;
                    let keys = self
                        .native
                        .as_mut()
                        .ok_or_else(|| {
                            PlatformError::OutcomeUnknown(
                                "Native input worker is unavailable".into(),
                            )
                        })?
                        .paste_keys()
                        .await;
                    for key in &keys {
                        self.key(*key, true).await?;
                    }
                    for key in keys.iter().rev() {
                        self.key(*key, false).await?;
                    }
                    Ok(())
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
        if let Some(error) = failure {
            self.native_call(None).await?;
            return Err(error);
        }
        // Nothing is held: the native side may remove its devices until the
        // next action, so they stop standing beside the person's own.
        self.native_step(None, true).await
    }
}
/// Modifiers that make a key a shortcut: with one of them, a letter names
/// its key whatever its case, and Shift is added only when it is named. So
/// "ctrl+W" is Ctrl+W, not Ctrl+Shift+W, which closes a browser window.
const SHORTCUT_MODIFIERS: [u32; 11] = [
    0xffe3, 0xffe4, // Control
    0xffe9, 0xffea, // Alt
    0xffeb, 0xffec, // Super
    0xffe7, 0xffe8, // Meta
    0xffed, 0xffee, // Hyper
    0xfe03, // AltGr
];
/// The small letter for a capital letter's keysym; any other keysym as is.
fn shortcut_letter(symbol: u32) -> u32 {
    let character = match symbol {
        0..=0xff => char::from_u32(symbol),
        0x0100_0100..=0x0110_ffff => char::from_u32(symbol - 0x0100_0000),
        _ => None,
    };
    let Some(c) = character.filter(|c| c.is_alphabetic() && c.is_uppercase()) else {
        return symbol;
    };
    let mut small = c.to_lowercase();
    match (small.next(), small.next()) {
        (Some(small), None) => character_keysym(small).unwrap_or(symbol),
        _ => symbol,
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
/// The keysym for a key name. Names follow the W3C `KeyboardEvent.key`
/// vocabulary the phone uses, in any case, with the common aliases agents
/// use (Return, Esc, cmd, win, PgDn...), and fall back to X keysym names
/// such as `Super_L` or `XF86AudioMute`. A single character names itself, so
/// "T" is Shift+T.
pub fn keysym(key: &str) -> Result<u32, PlatformError> {
    let mut chars = key.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        return character_keysym(c);
    }
    let name = key.to_ascii_lowercase();
    Ok(match name.as_str() {
        "ctrl" | "control" | "ctl" | "control_l" | "ctrl_l" | "lctrl" => 0xffe3,
        "control_r" | "ctrl_r" | "rctrl" => 0xffe4,
        "shift" | "shift_l" | "lshift" => 0xffe1,
        "shift_r" | "rshift" => 0xffe2,
        "alt" | "alt_l" | "lalt" | "option" | "opt" => 0xffe9,
        "alt_r" | "ralt" => 0xffea,
        "altgr" | "altgraph" | "iso_level3_shift" => 0xfe03,
        "meta" | "meta_l" | "super" | "super_l" | "lsuper" | "win" | "windows" | "lwin" | "cmd"
        | "command" | "os" | "logo" => 0xffeb,
        "super_r" | "meta_r" | "rsuper" | "rwin" => 0xffec,
        "enter" | "return" => 0xff0d,
        "tab" => 0xff09,
        "escape" | "esc" => 0xff1b,
        "backspace" | "back_space" => 0xff08,
        "delete" | "del" => 0xffff,
        "insert" | "ins" => 0xff63,
        "arrowleft" | "left" => 0xff51,
        "arrowup" | "up" => 0xff52,
        "arrowright" | "right" => 0xff53,
        "arrowdown" | "down" => 0xff54,
        "home" => 0xff50,
        "end" => 0xff57,
        "pageup" | "page_up" | "pgup" | "prior" => 0xff55,
        "pagedown" | "page_down" | "pgdn" | "next" => 0xff56,
        "space" | "spacebar" => 0x20,
        "capslock" | "caps_lock" => 0xffe5,
        "numlock" | "num_lock" => 0xff7f,
        "scrolllock" | "scroll_lock" => 0xff14,
        "printscreen" | "print" | "prtsc" => 0xff61,
        "pause" => 0xff13,
        "contextmenu" | "menu" | "apps" => 0xff67,
        "audiovolumeup" | "volumeup" => 0x1008_ff13,
        "audiovolumedown" | "volumedown" => 0x1008_ff11,
        "audiovolumemute" | "volumemute" | "mute" => 0x1008_ff12,
        "mediaplaypause" | "playpause" => 0x1008_ff14,
        "mediastop" => 0x1008_ff15,
        "mediatrackprevious" => 0x1008_ff16,
        "mediatracknext" => 0x1008_ff17,
        name if name.starts_with('f') && name[1..].chars().all(|c| c.is_ascii_digit()) => {
            let n = name[1..]
                .parse::<u32>()
                .map_err(|_| invalid("unknown key name"))?;
            if !(1..=24).contains(&n) {
                return Err(invalid("unknown function key"));
            }
            0xffbd + n
        }
        _ => return named_keysym(key),
    })
}
#[cfg(target_os = "linux")]
fn named_keysym(key: &str) -> Result<u32, PlatformError> {
    use xkbcommon::xkb;
    [xkb::KEYSYM_NO_FLAGS, xkb::KEYSYM_CASE_INSENSITIVE]
        .into_iter()
        .map(|flags| xkb::keysym_from_name(key, flags).raw())
        .find(|sym| *sym != 0)
        .ok_or_else(|| invalid("unknown key name"))
}
#[cfg(not(target_os = "linux"))]
fn named_keysym(_: &str) -> Result<u32, PlatformError> {
    Err(invalid("unknown key name"))
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
                                text: false,
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
mod name_tests {
    use super::keysym;
    #[test]
    fn phone_voice_and_agent_key_names_resolve() {
        // The phone's names (keys.ts), voice's, and what agents commonly send.
        for (names, expected) in [
            (
                &["super", "Super", "meta", "Meta", "cmd", "win", "Super_L"][..],
                0xffeb,
            ),
            (&["ctrl", "Control", "CTRL", "Control_L"][..], 0xffe3),
            (&["alt", "Alt", "option"][..], 0xffe9),
            (&["shift", "Shift"][..], 0xffe1),
            (&["Enter", "enter", "Return", "RETURN"][..], 0xff0d),
            (&["Escape", "Esc", "escape"][..], 0xff1b),
            (&["Tab", "tab"][..], 0xff09),
            (&["Backspace", "BackSpace"][..], 0xff08),
            (&["Delete", "Del"][..], 0xffff),
            (&["ArrowLeft", "Left"][..], 0xff51),
            (&["ArrowUp", "Up"][..], 0xff52),
            (&["ArrowRight", "Right"][..], 0xff53),
            (&["ArrowDown", "Down"][..], 0xff54),
            (&["PageUp", "Page_Up", "Prior"][..], 0xff55),
            (&["PageDown", "Page_Down", "PgDn"][..], 0xff56),
            (&["Space", "space", " "][..], 0x20),
            (&["F1", "f1"][..], 0xffbe),
            (&["F12"][..], 0xffc9),
            (&["F24"][..], 0xffd5),
            (&["PrintScreen", "Print"][..], 0xff61),
            (&["AudioVolumeMute", "XF86AudioMute"][..], 0x1008_ff12),
            (&["c"][..], u32::from('c')),
            (&["T"][..], u32::from('T')),
            (&["slash", "/"][..], u32::from('/')),
        ] {
            for name in names {
                assert_eq!(keysym(name).unwrap(), expected, "{name}");
            }
        }
        for name in ["", "F0", "F25", "Fx", "nonsense key", "ctrl+c"] {
            assert!(keysym(name).is_err(), "{name}");
        }
    }
}

#[cfg(test)]
mod shortcut_case_tests {
    use super::*;
    use crate::native::{Driver, NativeInput};
    use std::sync::Mutex;

    type Log = Arc<Mutex<Vec<(u32, bool)>>>;
    struct Keys(Log);
    #[async_trait]
    impl NativeInput for Keys {
        async fn event(&mut self, event: Event) -> Result<(), PlatformError> {
            if let Event::Key {
                keysym, pressed, ..
            } = event
            {
                self.0.lock().unwrap().push((keysym, pressed));
            }
            Ok(())
        }
        async fn barrier(&mut self) -> Result<(), PlatformError> {
            Ok(())
        }
    }
    #[derive(Debug, Default)]
    struct Desk(Log);
    #[async_trait]
    impl Driver for Desk {
        async fn outputs(&self) -> Result<Vec<Output>, PlatformError> {
            Ok(vec![Output {
                name: "monitor".into(),
                x: 0.,
                y: 0.,
                width: 100,
                height: 100,
                scale: 1.0,
                rotation: 0,
            }])
        }
        async fn capture(&self, _: &Output) -> Result<ports::CapturedFrame, PlatformError> {
            Err(PlatformError::Unsupported("no capture".into()))
        }
        async fn input(&self) -> Result<Box<dyn NativeInput>, PlatformError> {
            Ok(Box::new(Keys(self.0.clone())))
        }
    }
    async fn sent(actions: &[InputAction]) -> Vec<(u32, bool)> {
        use ports::DesktopBackend;
        let desk = Arc::new(Desk::default());
        let available = protocol::capability::Availability::Available;
        let desktop = LinuxDesktop::new(
            desk.clone(),
            ports::DesktopCapabilities {
                backend: "test".into(),
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
        for action in actions {
            sink.inject(action, &target.geometry).await.unwrap();
        }
        desk.0.lock().unwrap().clone()
    }
    fn chord(keys: &[&str]) -> InputAction {
        InputAction::KeyChord {
            keys: keys.iter().map(|k| k.to_string()).collect(),
        }
    }
    fn press(key: &str, pressed: bool) -> InputAction {
        InputAction::KeyPress {
            key: key.into(),
            pressed,
        }
    }
    #[tokio::test]
    async fn a_capital_in_a_shortcut_is_its_key_and_shift_only_when_named() {
        let (ctrl, shift, w) = (0xffe3, 0xffe1, u32::from('w'));
        // Ctrl+W, not Ctrl+Shift+W, which closes the whole browser window.
        assert_eq!(
            sent(&[chord(&["ctrl", "W"])]).await,
            [(ctrl, true), (w, true), (w, false), (ctrl, false)]
        );
        assert_eq!(
            sent(&[chord(&["Super", "Q"])]).await[1],
            (u32::from('q'), true)
        );
        // Shift named is Shift; alone, or with only Shift, a capital stays.
        assert_eq!(
            sent(&[chord(&["ctrl", "shift", "W"])]).await[..3],
            [(ctrl, true), (shift, true), (w, true)]
        );
        assert_eq!(sent(&[chord(&["W"])]).await[0], (u32::from('W'), true));
        assert_eq!(
            sent(&[chord(&["shift", "W"])]).await[1],
            (u32::from('W'), true)
        );
        // Symbols keep what the layout needs for them.
        assert_eq!(
            sent(&[chord(&["ctrl", "+"])]).await[1],
            (u32::from('+'), true)
        );
        // Ctrl held with key_press, then a chord of a capital: still Ctrl+W.
        assert_eq!(
            sent(&[press("ctrl", true), chord(&["W"]), press("ctrl", false)]).await,
            [(ctrl, true), (w, true), (w, false), (ctrl, false)]
        );
        // Held with key_press, released after Ctrl: the same key goes up.
        assert_eq!(
            sent(&[
                press("ctrl", true),
                press("W", true),
                press("ctrl", false),
                press("W", false)
            ])
            .await,
            [(ctrl, true), (w, true), (ctrl, false), (w, false)]
        );
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
