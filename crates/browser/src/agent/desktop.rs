// SPDX-License-Identifier: Apache-2.0
//! The agent browser's tabs as Desktop control targets, so the owner can
//! watch them and take over, as on a screen. Pictures come from the browser,
//! filled fields painted over, and input goes to the tab alone.
use super::{AgentBrowser, AgentError, page::OwnerTab, run::VIEWPORT};
use async_trait::async_trait;
use ports::desktop::{
    CapturedFrame, DesktopBackend, DesktopCapabilities, FrameStream, InputSink, PixelFormat,
    PlatformError, UnlockOutcome,
};
use protocol::{
    capability::Availability,
    control::{
        ControlTarget, DesktopLockState, InputAction, PointerButton, TargetGeometry, TargetKind,
        TargetPoint,
    },
    ids::TargetId,
};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tokio::time::Instant;

/// A tab's picture never changes size, so its geometry never does.
const GEOMETRY: TargetGeometry = TargetGeometry {
    width: VIEWPORT.0,
    height: VIEWPORT.1,
    scale: 1.,
    rotation_degrees: 0,
    revision: 1,
};
/// Pictures a second while the owner watches.
const WATCH_FPS: u8 = 4;
/// An unchanged picture is sent again this often, so the owner can tell the
/// tab is still there.
const UNCHANGED_RESEND: Duration = Duration::from_secs(5);

/// A desktop backend whose targets include the agent browser's tabs.
#[derive(Debug)]
pub struct WithAgentBrowser {
    inner: Arc<dyn DesktopBackend>,
    browser: AgentBrowser,
}

impl WithAgentBrowser {
    pub fn new(inner: Arc<dyn DesktopBackend>, browser: AgentBrowser) -> Self {
        Self { inner, browser }
    }
    fn owns(&self, id: TargetId) -> bool {
        self.browser.owner_tabs().1.iter().any(|t| t.id == id)
    }
}

fn target(tab: &OwnerTab) -> ControlTarget {
    let shown = if tab.title.trim().is_empty() {
        &tab.url
    } else {
        &tab.title
    };
    ControlTarget {
        target_id: tab.id,
        kind: TargetKind::Browser,
        label: format!(
            "Agent browser — {}",
            shown.chars().take(80).collect::<String>()
        ),
        native_ref: format!("agent-browser:{}", tab.id),
        geometry: GEOMETRY,
        capture: Availability::Available,
        input: Availability::Available,
    }
}

#[async_trait]
impl DesktopBackend for WithAgentBrowser {
    async fn capabilities(&self) -> DesktopCapabilities {
        self.inner.capabilities().await
    }

    async fn targets(&self) -> Result<Vec<ControlTarget>, PlatformError> {
        let (_, tabs) = self.browser.owner_tabs();
        let mut targets = match self.inner.targets().await {
            Ok(targets) => targets,
            // The agent browser's tabs are there even when the screen isn't.
            Err(_) if !tabs.is_empty() => Vec::new(),
            Err(e) => return Err(e),
        };
        targets.extend(tabs.iter().map(target));
        Ok(targets)
    }

    async fn capture_still(&self, id: TargetId) -> Result<CapturedFrame, PlatformError> {
        if !self.owns(id) {
            return self.inner.capture_still(id).await;
        }
        picture(&self.browser, id).await
    }

    async fn open_stream(
        &self,
        id: TargetId,
        max_fps: u8,
    ) -> Result<Box<dyn FrameStream>, PlatformError> {
        if !self.owns(id) {
            return self.inner.open_stream(id, max_fps).await;
        }
        let fps = max_fps.clamp(1, WATCH_FPS);
        Ok(Box::new(TabStream {
            browser: self.browser.clone(),
            id,
            every: Duration::from_millis(1000 / u64::from(fps)),
            next: Instant::now(),
            last: None,
        }))
    }

    async fn input_sink(&self, id: TargetId) -> Result<Box<dyn InputSink>, PlatformError> {
        if !self.owns(id) {
            return self.inner.input_sink(id).await;
        }
        Ok(Box::new(TabInput {
            browser: self.browser.clone(),
            id,
            buttons: Vec::new(),
            keys: Vec::new(),
            at: (0., 0.),
        }))
    }

    async fn lock_session(&self) -> Result<(), PlatformError> {
        self.inner.lock_session().await
    }

    async fn unlock_session(
        &self,
        password: &protocol::accounts::Secret,
        allowed: &(dyn Fn() -> bool + Send + Sync),
    ) -> Result<UnlockOutcome, PlatformError> {
        self.inner.unlock_session(password, allowed).await
    }

    async fn accessibility_point(
        &self,
        target: TargetId,
        revision: u64,
        x: f64,
        y: f64,
    ) -> Result<TargetPoint, PlatformError> {
        if self.owns(target) {
            return Err(PlatformError::Unsupported(
                "The agent browser has no accessibility coordinates".into(),
            ));
        }
        self.inner.accessibility_point(target, revision, x, y).await
    }

    async fn desktop_lock(&self) -> DesktopLockState {
        self.inner.desktop_lock().await
    }

    fn lock_changes(&self) -> Option<tokio::sync::watch::Receiver<DesktopLockState>> {
        self.inner.lock_changes()
    }

    async fn reopen_capture(&self) -> Result<(), PlatformError> {
        self.inner.reopen_capture().await
    }
}

fn platform(browser: &AgentBrowser, id: TargetId, error: AgentError) -> PlatformError {
    if browser.owner_tabs().1.iter().any(|t| t.id == id) {
        PlatformError::Native(error.to_string())
    } else {
        PlatformError::TargetNotFound
    }
}

async fn picture(browser: &AgentBrowser, id: TargetId) -> Result<CapturedFrame, PlatformError> {
    let picture = browser
        .capture(id)
        .await
        .map_err(|e| platform(browser, id, e))?;
    Ok(CapturedFrame {
        width: picture.width,
        height: picture.height,
        stride: picture.width * 4,
        format: PixelFormat::Rgba8,
        data: picture.data,
        geometry: GEOMETRY,
        cursor: None,
        damage: None,
    })
}

struct TabStream {
    browser: AgentBrowser,
    id: TargetId,
    every: Duration,
    next: Instant,
    /// The last picture sent, and when.
    last: Option<(Vec<u8>, Instant)>,
}

#[async_trait]
impl FrameStream for TabStream {
    async fn next_frame(&mut self) -> Result<Option<CapturedFrame>, PlatformError> {
        loop {
            tokio::time::sleep_until(self.next).await;
            self.next = Instant::now() + self.every;
            let frame = match picture(&self.browser, self.id).await {
                Ok(frame) => frame,
                // The tab has closed.
                Err(PlatformError::TargetNotFound) => return Ok(None),
                Err(e) => return Err(e),
            };
            let unchanged = self.last.as_ref().is_some_and(|(data, sent)| {
                *data == frame.data && sent.elapsed() < UNCHANGED_RESEND
            });
            if !unchanged {
                self.last = Some((frame.data.clone(), Instant::now()));
                return Ok(Some(frame));
            }
        }
    }
}

struct TabInput {
    browser: AgentBrowser,
    id: TargetId,
    /// Buttons and keys held down, in the order they went down.
    buttons: Vec<PointerButton>,
    keys: Vec<Key>,
    /// Where the pointer last was, in page pixels.
    at: (f64, f64),
}

/// A key as the browser is told it.
#[derive(Debug, Clone, PartialEq)]
struct Key {
    key: String,
    code: String,
    vk: i64,
    /// The modifier bit it sets, if it is one.
    modifier: i64,
}

fn key(name: &str) -> Result<Key, PlatformError> {
    let named = |key: &str, code: &str, vk: i64, modifier: i64| Key {
        key: key.into(),
        code: code.into(),
        vk,
        modifier,
    };
    Ok(match name {
        "alt" | "Alt" => named("Alt", "AltLeft", 18, 1),
        "ctrl" | "control" | "Control" => named("Control", "ControlLeft", 17, 2),
        "meta" | "Meta" | "super" => named("Meta", "MetaLeft", 91, 4),
        "shift" | "Shift" => named("Shift", "ShiftLeft", 16, 8),
        "Enter" => named("Enter", "Enter", 13, 0),
        "Tab" => named("Tab", "Tab", 9, 0),
        "Escape" => named("Escape", "Escape", 27, 0),
        "Backspace" => named("Backspace", "Backspace", 8, 0),
        "Delete" => named("Delete", "Delete", 46, 0),
        "Insert" => named("Insert", "Insert", 45, 0),
        "ArrowLeft" => named("ArrowLeft", "ArrowLeft", 37, 0),
        "ArrowUp" => named("ArrowUp", "ArrowUp", 38, 0),
        "ArrowRight" => named("ArrowRight", "ArrowRight", 39, 0),
        "ArrowDown" => named("ArrowDown", "ArrowDown", 40, 0),
        "Home" => named("Home", "Home", 36, 0),
        "End" => named("End", "End", 35, 0),
        "PageUp" => named("PageUp", "PageUp", 33, 0),
        "PageDown" => named("PageDown", "PageDown", 34, 0),
        " " | "Space" | "Spacebar" => named(" ", "Space", 32, 0),
        _ => {
            if let Some(n) = name.strip_prefix('F').and_then(|n| n.parse::<i64>().ok())
                && (1..=12).contains(&n)
            {
                return Ok(named(name, name, 111 + n, 0));
            }
            let mut chars = name.chars();
            let (Some(c), None) = (chars.next(), chars.next()) else {
                return Err(PlatformError::Unsupported(format!("Unknown key {name}")));
            };
            let upper = c.to_ascii_uppercase();
            let (code, vk) = if upper.is_ascii_alphabetic() {
                (format!("Key{upper}"), i64::from(upper as u8))
            } else if c.is_ascii_digit() {
                (format!("Digit{c}"), i64::from(c as u8))
            } else {
                (String::new(), 0)
            };
            Key {
                key: c.to_string(),
                code,
                vk,
                modifier: 0,
            }
        }
    })
}

fn button_name(button: PointerButton) -> &'static str {
    match button {
        PointerButton::Left => "left",
        PointerButton::Right => "right",
        PointerButton::Middle => "middle",
    }
}

fn button_bit(button: PointerButton) -> i64 {
    match button {
        PointerButton::Left => 1,
        PointerButton::Right => 2,
        PointerButton::Middle => 4,
    }
}

fn place(point: &TargetPoint) -> Result<(f64, f64), PlatformError> {
    if !(0.0..=1.0).contains(&point.x) || !(0.0..=1.0).contains(&point.y) {
        return Err(PlatformError::Native("The point is outside the tab".into()));
    }
    Ok((
        point.x * f64::from(VIEWPORT.0),
        point.y * f64::from(VIEWPORT.1),
    ))
}

impl TabInput {
    fn modifiers(&self) -> i64 {
        self.keys.iter().fold(0, |bits, k| bits | k.modifier)
    }
    fn held_buttons(&self) -> i64 {
        self.buttons.iter().fold(0, |bits, b| bits | button_bit(*b))
    }
    fn mouse(&self, kind: &str, button: Option<PointerButton>, count: u8) -> (&'static str, Value) {
        let (x, y) = self.at;
        (
            "Input.dispatchMouseEvent",
            json!({
                "type": kind,
                "x": x,
                "y": y,
                "button": button.or(self.buttons.last().copied()).map_or("none", button_name),
                "buttons": self.held_buttons(),
                "clickCount": count,
                "modifiers": self.modifiers(),
            }),
        )
    }
    fn key_event(&self, key: &Key, down: bool) -> (&'static str, Value) {
        let modifiers = self.modifiers();
        // Typing makes text only without a command modifier held.
        let text = (down && key.modifier == 0 && modifiers & 7 == 0)
            .then(|| match key.key.as_str() {
                "Enter" => Some("\r".to_owned()),
                k if k.chars().count() == 1 => Some(if modifiers & 8 != 0 {
                    k.to_uppercase()
                } else {
                    k.to_owned()
                }),
                _ => None,
            })
            .flatten();
        let kind = match (down, &text) {
            (false, _) => "keyUp",
            (true, Some(_)) => "keyDown",
            (true, None) => "rawKeyDown",
        };
        let mut params = json!({
            "type": kind,
            "key": key.key,
            "code": key.code,
            "windowsVirtualKeyCode": key.vk,
            "modifiers": modifiers,
        });
        if let Some(text) = text {
            params["text"] = json!(text);
        }
        ("Input.dispatchKeyEvent", params)
    }
    async fn send(&self, events: &[(&str, Value)]) -> Result<(), PlatformError> {
        self.browser
            .owner_input(self.id, events)
            .await
            .map_err(|e| match platform(&self.browser, self.id, e) {
                PlatformError::Native(message) => PlatformError::OutcomeUnknown(message),
                other => other,
            })
    }
    async fn press_key(&mut self, key: Key, pressed: bool) -> Result<(), PlatformError> {
        if pressed {
            let event = self.key_event(&key, true);
            if !self.keys.contains(&key) {
                self.keys.push(key);
            }
            self.send(&[event]).await
        } else {
            self.keys.retain(|k| *k != key);
            let event = self.key_event(&key, false);
            self.send(&[event]).await
        }
    }
}

#[async_trait]
impl InputSink for TabInput {
    async fn inject(
        &mut self,
        action: &InputAction,
        _geometry: &TargetGeometry,
    ) -> Result<(), PlatformError> {
        match action {
            InputAction::PointerMove { to } => {
                self.at = place(to)?;
                let event = self.mouse("mouseMoved", None, 0);
                self.send(&[event]).await
            }
            InputAction::PointerButton {
                button,
                pressed,
                at,
            } => {
                self.at = place(at)?;
                let moved = self.mouse("mouseMoved", None, 0);
                let event = if *pressed {
                    if !self.buttons.contains(button) {
                        self.buttons.push(*button);
                    }
                    self.mouse("mousePressed", Some(*button), 1)
                } else {
                    self.buttons.retain(|b| b != button);
                    self.mouse("mouseReleased", Some(*button), 1)
                };
                self.send(&[moved, event]).await
            }
            InputAction::Click { button, at, count } => {
                self.at = place(at)?;
                let mut events = vec![self.mouse("mouseMoved", None, 0)];
                for n in 1..=(*count).clamp(1, 3) {
                    events.push(self.mouse("mousePressed", Some(*button), n));
                    events.push(self.mouse("mouseReleased", Some(*button), n));
                }
                self.send(&events).await
            }
            InputAction::Scroll { at, dx, dy } => {
                if !dx.is_finite() || !dy.is_finite() || dx.abs() > 10_000. || dy.abs() > 10_000. {
                    return Err(PlatformError::Native("invalid scroll distance".into()));
                }
                self.at = place(at)?;
                let (x, y) = self.at;
                self.send(&[(
                    "Input.dispatchMouseEvent",
                    json!({"type":"mouseWheel","x":x,"y":y,"deltaX":dx,"deltaY":dy,
                        "modifiers":self.modifiers()}),
                )])
                .await
            }
            InputAction::KeyChord { keys } => {
                if keys.is_empty() || keys.len() > 16 {
                    return Err(PlatformError::Native("invalid key chord".into()));
                }
                let keys = keys.iter().map(|k| key(k)).collect::<Result<Vec<_>, _>>()?;
                let mut outcome = Ok(());
                let mut down = Vec::new();
                for key in keys {
                    outcome = self.press_key(key.clone(), true).await;
                    down.push(key);
                    if outcome.is_err() {
                        break;
                    }
                }
                // Everything pressed is let go, even after a failure.
                for key in down.into_iter().rev() {
                    let released = self.press_key(key, false).await;
                    if outcome.is_ok() {
                        outcome = released;
                    }
                }
                outcome
            }
            InputAction::KeyPress { key: name, pressed } => {
                let key = key(name)?;
                self.press_key(key, *pressed).await
            }
            // The tab takes text directly; there is no clipboard to replace.
            InputAction::Text { text } | InputAction::Paste { text } => {
                self.send(&[("Input.insertText", json!({"text":text}))])
                    .await
            }
            InputAction::ReleaseAll => self.release_all().await,
        }
    }

    async fn release_all(&mut self) -> Result<(), PlatformError> {
        let mut events = Vec::new();
        while let Some(key) = self.keys.pop() {
            events.push(self.key_event(&key, false));
        }
        while let Some(button) = self.buttons.pop() {
            events.push(self.mouse("mouseReleased", Some(button), 1));
        }
        if events.is_empty() {
            return Ok(());
        }
        match self.send(&events).await {
            // A tab that has gone holds nothing.
            Err(PlatformError::TargetNotFound) | Ok(()) => Ok(()),
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_carry_their_codes_and_modifier_bits() {
        assert_eq!(key("ctrl").unwrap().modifier, 2);
        assert_eq!(key("shift").unwrap().vk, 16);
        let a = key("a").unwrap();
        assert_eq!((a.code.as_str(), a.vk), ("KeyA", 65));
        assert_eq!(key("F5").unwrap().vk, 116);
        assert_eq!(key("7").unwrap().code, "Digit7");
        assert!(key("NotAKey").is_err());
    }

    #[test]
    fn typing_makes_text_only_without_a_command_modifier() {
        let browser =
            AgentBrowser::new("/nonexistent".into(), std::path::Path::new("/nonexistent"));
        let mut input = TabInput {
            browser,
            id: TargetId::new(),
            buttons: Vec::new(),
            keys: Vec::new(),
            at: (0., 0.),
        };
        let a = key("a").unwrap();
        assert_eq!(input.key_event(&a, true).1["text"], "a");
        input.keys.push(key("shift").unwrap());
        assert_eq!(input.key_event(&a, true).1["text"], "A");
        input.keys.push(key("ctrl").unwrap());
        let event = input.key_event(&a, true).1;
        assert!(event.get("text").is_none());
        assert_eq!(event["type"], "rawKeyDown");
        assert_eq!(event["modifiers"], 10);
    }
}
