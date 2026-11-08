// SPDX-License-Identifier: Apache-2.0
//! X11 capture and XTest input. Only explicit display endpoints are accepted.
use crate::native::{Driver, Event, NativeInput, Output, failure};
use async_trait::async_trait;
use ports::desktop::{CapturedFrame, PixelFormat, PlatformError};
use protocol::control::TargetGeometry;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
};
use x11rb::{
    connection::Connection,
    protocol::{
        randr::ConnectionExt as _,
        xfixes::ConnectionExt as _,
        xproto::{self, ConnectionExt as _, ImageFormat},
        xtest::ConnectionExt as _,
    },
    rust_connection::RustConnection,
};

pub struct X11Driver {
    connection: Arc<RustConnection>,
    screen: usize,
}
impl std::fmt::Debug for X11Driver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("X11Driver").finish_non_exhaustive()
    }
}
impl X11Driver {
    pub async fn connect(display: String) -> Result<Self, PlatformError> {
        if display.is_empty() {
            return Err(PlatformError::Unsupported(
                "an explicit X11 display is required".into(),
            ));
        }
        tokio::task::spawn_blocking(move || {
            let (connection, screen) = x11rb::connect(Some(&display)).map_err(failure)?;
            connection
                .xtest_get_version(2, 2)
                .map_err(failure)?
                .reply()
                .map_err(|_| PlatformError::Unsupported("XTest extension unavailable".into()))?;
            connection
                .randr_query_version(1, 5)
                .map_err(failure)?
                .reply()
                .map_err(failure)?;
            connection
                .xfixes_query_version(5, 0)
                .map_err(failure)?
                .reply()
                .map_err(failure)?;
            Ok(Self {
                connection: Arc::new(connection),
                screen,
            })
        })
        .await
        .map_err(failure)?
    }
}
#[async_trait]
impl Driver for X11Driver {
    async fn outputs(&self) -> Result<Vec<Output>, PlatformError> {
        let c = self.connection.clone();
        let root = c.setup().roots[self.screen].root;
        tokio::task::spawn_blocking(move || {
            let resources = c
                .randr_get_screen_resources_current(root)
                .map_err(failure)?
                .reply()
                .map_err(failure)?;
            let mut result = Vec::new();
            for id in resources.outputs {
                let output = c
                    .randr_get_output_info(id, resources.config_timestamp)
                    .map_err(failure)?
                    .reply()
                    .map_err(failure)?;
                if output.crtc == 0 {
                    continue;
                }
                let crtc = c
                    .randr_get_crtc_info(output.crtc, resources.config_timestamp)
                    .map_err(failure)?
                    .reply()
                    .map_err(failure)?;
                if crtc.width == 0 || crtc.height == 0 {
                    continue;
                }
                result.push(Output {
                    name: String::from_utf8_lossy(&output.name).into_owned(),
                    x: crtc.x.into(),
                    y: crtc.y.into(),
                    width: crtc.width.into(),
                    height: crtc.height.into(),
                    scale: 1.0,
                    rotation: if crtc
                        .rotation
                        .contains(x11rb::protocol::randr::Rotation::ROTATE90)
                    {
                        90
                    } else if crtc
                        .rotation
                        .contains(x11rb::protocol::randr::Rotation::ROTATE180)
                    {
                        180
                    } else if crtc
                        .rotation
                        .contains(x11rb::protocol::randr::Rotation::ROTATE270)
                    {
                        270
                    } else {
                        0
                    },
                });
            }
            Ok(result)
        })
        .await
        .map_err(failure)?
    }
    async fn capture(&self, output: &Output) -> Result<CapturedFrame, PlatformError> {
        if u64::from(output.width) * u64::from(output.height) > 64 * 1024 * 1024
            || output.width == 0
            || output.height == 0
        {
            return Err(PlatformError::Unsupported(
                "X11 capture dimensions exceed limits".into(),
            ));
        }
        let c = self.connection.clone();
        let screen = self.screen;
        let o = output.clone();
        tokio::task::spawn_blocking(move || {
            let setup = c.setup();
            let root = &setup.roots[screen];
            let image = c
                .get_image(
                    ImageFormat::Z_PIXMAP,
                    root.root,
                    o.x as i16,
                    o.y as i16,
                    u16::try_from(o.width).map_err(failure)?,
                    u16::try_from(o.height).map_err(failure)?,
                    u32::MAX,
                )
                .map_err(failure)?
                .reply()
                .map_err(failure)?;
            let format = setup
                .pixmap_formats
                .iter()
                .find(|p| p.depth == image.depth)
                .ok_or_else(|| PlatformError::Unsupported("unknown X11 pixel depth".into()))?;
            if ![24, 32].contains(&format.bits_per_pixel) || format.scanline_pad == 0 {
                return Err(PlatformError::Unsupported(
                    "only 24/32-bit X11 TrueColor is supported".into(),
                ));
            }
            let visual = root
                .allowed_depths
                .iter()
                .flat_map(|d| &d.visuals)
                .find(|v| v.visual_id == image.visual)
                .ok_or_else(|| PlatformError::Unsupported("unknown X11 visual".into()))?;
            if visual.class != xproto::VisualClass::TRUE_COLOR {
                return Err(PlatformError::Unsupported(
                    "X11 visual is not TrueColor".into(),
                ));
            }
            let bytes = format.bits_per_pixel as usize / 8;
            let stride = (o.width as usize * format.bits_per_pixel as usize)
                .div_ceil(format.scanline_pad as usize)
                * format.scanline_pad as usize
                / 8;
            if image.data.len() < stride * o.height as usize {
                return Err(PlatformError::Native("truncated X11 image".into()));
            }
            let mut data = Vec::with_capacity((o.width * o.height * 4) as usize);
            for row in image.data.chunks_exact(stride).take(o.height as usize) {
                for pixel in row[..o.width as usize * bytes].chunks_exact(bytes) {
                    let value = if setup.image_byte_order == xproto::ImageOrder::LSB_FIRST {
                        pixel
                            .iter()
                            .enumerate()
                            .fold(0u32, |v, (i, b)| v | u32::from(*b) << (8 * i))
                    } else {
                        pixel.iter().fold(0u32, |v, b| (v << 8) | u32::from(*b))
                    };
                    data.extend_from_slice(&[
                        component(value, visual.blue_mask),
                        component(value, visual.green_mask),
                        component(value, visual.red_mask),
                        255,
                    ]);
                }
            }
            let cursor = c
                .xfixes_get_cursor_image()
                .map_err(failure)?
                .reply()
                .map_err(failure)?;
            let cx = i32::from(cursor.x) - o.x as i32;
            let cy = i32::from(cursor.y) - o.y as i32;
            for y in 0..u32::from(cursor.height) {
                for x in 0..u32::from(cursor.width) {
                    let px = cx + x as i32 - i32::from(cursor.xhot);
                    let py = cy + y as i32 - i32::from(cursor.yhot);
                    if px < 0 || py < 0 || px >= o.width as i32 || py >= o.height as i32 {
                        continue;
                    }
                    let Some(value) = cursor
                        .cursor_image
                        .get((y * u32::from(cursor.width) + x) as usize)
                    else {
                        continue;
                    };
                    let alpha = value >> 24;
                    let i = (py as usize * o.width as usize + px as usize) * 4;
                    for (offset, shift) in [0, 8, 16].into_iter().enumerate() {
                        let foreground = (value >> shift) & 255;
                        data[i + offset] = (foreground
                            + u32::from(data[i + offset]) * (255 - alpha) / 255)
                            .min(255) as u8;
                    }
                }
            }
            Ok(CapturedFrame {
                width: o.width,
                height: o.height,
                stride: o.width * 4,
                format: PixelFormat::Bgrx8,
                data,
                geometry: TargetGeometry {
                    width: o.width,
                    height: o.height,
                    scale: 1.0,
                    rotation_degrees: o.rotation,
                    revision: 0,
                },
                cursor: (cx >= 0 && cy >= 0 && cx < o.width as i32 && cy < o.height as i32)
                    .then_some((cx.max(0) as u32, cy.max(0) as u32)),
                damage: None,
            })
        })
        .await
        .map_err(failure)?
    }
    async fn input(&self) -> Result<Box<dyn NativeInput>, PlatformError> {
        Ok(Box::new(XInput {
            state: Arc::new(Mutex::new(InputState {
                connection: self.connection.clone(),
                root: self.connection.setup().roots[self.screen].root,
                keys: Keys::default(),
                buttons: BTreeSet::new(),
                axis: (0.0, 0.0),
                fence: None,
            })),
        }))
    }
}
fn component(pixel: u32, mask: u32) -> u8 {
    if mask == 0 {
        return 0;
    }
    let shifted = mask >> mask.trailing_zeros();
    (((pixel & mask) >> mask.trailing_zeros()) * 255 / shifted) as u8
}
struct InputState {
    connection: Arc<RustConnection>,
    root: u32,
    keys: Keys,
    buttons: BTreeSet<u8>,
    axis: (f64, f64),
    fence: Option<crate::native::InputFence>,
}
impl InputState {
    fn fake(&self, event: u8, detail: u8, x: i16, y: i16) -> Result<(), PlatformError> {
        // Recheck after keyboard-map queries and between scroll pulses. Releases
        // remain allowed so cancellation cannot strand a partially held action.
        if let Some(fence) = &self.fence {
            fence.check(matches!(
                event,
                xproto::KEY_RELEASE_EVENT | xproto::BUTTON_RELEASE_EVENT
            ))?;
        }
        self.connection
            .xtest_fake_input(event, detail, 0, self.root, x, y, 0)
            .map_err(crate::native::uncertain)?
            .check()
            .map_err(|error| match error {
                x11rb::errors::ReplyError::X11Error(_) => failure(error),
                _ => crate::native::uncertain(error),
            })
    }
    fn event(&mut self, event: Event) -> Result<(), PlatformError> {
        match event {
            Event::Motion { x, y } => {
                if x < i16::MIN as f64
                    || y < i16::MIN as f64
                    || x > i16::MAX as f64
                    || y > i16::MAX as f64
                {
                    return Err(PlatformError::Unsupported(
                        "X11 coordinate exceeds protocol range".into(),
                    ));
                }
                self.fake(
                    xproto::MOTION_NOTIFY_EVENT,
                    0,
                    x.round() as i16,
                    y.round() as i16,
                )?;
            }
            Event::Button { code, pressed } => {
                let code = match code {
                    0x110 => 1,
                    0x111 => 3,
                    0x112 => 2,
                    _ => return Err(PlatformError::Unsupported("unknown pointer button".into())),
                };
                if pressed {
                    self.buttons.insert(code);
                }
                self.fake(
                    if pressed {
                        xproto::BUTTON_PRESS_EVENT
                    } else {
                        xproto::BUTTON_RELEASE_EVENT
                    },
                    code,
                    0,
                    0,
                )?;
                if !pressed {
                    self.buttons.remove(&code);
                }
            }
            Event::Key {
                keysym,
                pressed,
                text,
            } => {
                let mut keys = std::mem::take(&mut self.keys);
                let result = if pressed {
                    keys.press(self, keysym, text)
                } else {
                    keys.release(self, keysym)
                };
                self.keys = keys;
                result?;
            }
            Event::Axis { dx, dy } => {
                self.axis.0 += dx;
                self.axis.1 += dy;
                for horizontal in [false, true] {
                    loop {
                        let amount = if horizontal { self.axis.0 } else { self.axis.1 };
                        if amount.abs() < 15.0 {
                            break;
                        }
                        let button = if horizontal {
                            if amount > 0.0 { 7 } else { 6 }
                        } else if amount > 0.0 {
                            5
                        } else {
                            4
                        };
                        self.buttons.insert(button);
                        self.fake(xproto::BUTTON_PRESS_EVENT, button, 0, 0)?;
                        self.fake(xproto::BUTTON_RELEASE_EVENT, button, 0, 0)
                            .map_err(crate::native::uncertain)?;
                        self.buttons.remove(&button);
                        if horizontal {
                            self.axis.0 -= amount.signum() * 15.0;
                        } else {
                            self.axis.1 -= amount.signum() * 15.0;
                        }
                    }
                }
            }
        }
        self.connection.flush().map_err(crate::native::uncertain)
    }
}
impl KeyServer for InputState {
    fn lock(&mut self) -> Result<bool, PlatformError> {
        let pointer = self
            .connection
            .query_pointer(self.root)
            .map_err(failure)?
            .reply()
            .map_err(failure)?;
        Ok(u16::from(pointer.mask) & u16::from(xproto::KeyButMask::LOCK) != 0)
    }
    fn mapping(&mut self) -> Result<Mapping, PlatformError> {
        let setup = self.connection.setup();
        let first = setup.min_keycode;
        let count = setup.max_keycode - first + 1;
        let map = self
            .connection
            .get_keyboard_mapping(first, count)
            .map_err(failure)?
            .reply()
            .map_err(failure)?;
        Ok(Mapping {
            first,
            width: map.keysyms_per_keycode as usize,
            keysyms: map.keysyms,
        })
    }
    fn change(&mut self, code: u8, row: &[u32]) -> Result<(), PlatformError> {
        self.connection
            .change_keyboard_mapping(1, code, row.len() as u8, row)
            .map_err(failure)?
            .check()
            .map_err(failure)
    }
    fn key(&mut self, code: u8, pressed: bool) -> Result<(), PlatformError> {
        self.fake(
            if pressed {
                xproto::KEY_PRESS_EVENT
            } else {
                xproto::KEY_RELEASE_EVENT
            },
            code,
            0,
            0,
        )
    }
}
impl Drop for InputState {
    fn drop(&mut self) {
        let mut keys = std::mem::take(&mut self.keys);
        keys.release_all(self);
        for code in &self.buttons {
            let _ = self.fake(xproto::BUTTON_RELEASE_EVENT, *code, 0, 0);
        }
        keys.restore_all(self);
        let _ = self.connection.flush();
    }
}

/// A character's small and capital keysyms, or the keysym twice when it has
/// no case.
fn case_pair(symbol: u32) -> (u32, u32) {
    let Some(c) = char::from_u32(xkbcommon::xkb::keysym_to_utf32(
        xkbcommon::xkb::Keysym::new(symbol),
    )) else {
        return (symbol, symbol);
    };
    let one = |chars: Vec<char>| (chars.len() == 1).then(|| chars[0]);
    match (
        one(c.to_lowercase().collect()),
        one(c.to_uppercase().collect()),
    ) {
        // Only true partners, including the typed symbol itself. Titlecase
        // digraphs have a small and capital form but are neither of them.
        // The Kelvin sign lowercases to k, but k's capital is K, not Kelvin.
        (Some(small), Some(capital))
            if small != capital
                && (c == small || c == capital)
                && one(small.to_uppercase().collect()) == Some(capital)
                && one(capital.to_lowercase().collect()) == Some(small) =>
        {
            (
                crate::input::character_keysym(small).unwrap_or(symbol),
                crate::input::character_keysym(capital).unwrap_or(symbol),
            )
        }
        _ => (symbol, symbol),
    }
}
/// Whether a key's two levels are a small letter and its capital.
fn cased(row: &[u32]) -> bool {
    let character = |sym: u32| {
        char::from_u32(xkbcommon::xkb::keysym_to_utf32(
            xkbcommon::xkb::Keysym::new(sym),
        ))
    };
    match (
        row.first().and_then(|s| character(*s)),
        row.get(1).and_then(|s| character(*s)),
    ) {
        (Some(small), Some(capital)) => {
            small.is_lowercase() && small.to_uppercase().eq(std::iter::once(capital))
        }
        _ => false,
    }
}
/// The X server as the keyboard logic sees it, so that logic can be tested
/// without a server.
trait KeyServer {
    fn mapping(&mut self) -> Result<Mapping, PlatformError>;
    fn change(&mut self, code: u8, row: &[u32]) -> Result<(), PlatformError>;
    fn key(&mut self, code: u8, pressed: bool) -> Result<(), PlatformError>;
    /// Whether Caps Lock is on.
    fn lock(&mut self) -> Result<bool, PlatformError>;
}
struct Mapping {
    first: u8,
    width: usize,
    keysyms: Vec<u32>,
}
impl Mapping {
    fn row(&self, code: u8) -> Option<&[u32]> {
        let start = usize::from(code.checked_sub(self.first)?) * self.width;
        self.keysyms.get(start..start + self.width)
    }
}
/// Keys held through XTest, and keycodes remapped for characters.
#[derive(Default)]
struct Keys {
    /// Held keysyms: their keycode, and whether Shift was pressed for them.
    held: BTreeMap<u32, (u8, Option<u8>)>,
    /// Original rows of keycodes remapped to characters the map lacks.
    remaps: BTreeMap<u8, Vec<u32>>,
    /// Those keycodes and their characters, least recently used first. They
    /// stay remapped until reused or dropped: restoring right after a key
    /// let a client that read the map late see the key as nothing.
    /// Each with the row as the server reads it back: XKB normalises what
    /// was written (seven copies come back as four and three empty), so
    /// that, not the row written, is what shows the remap is still Ophio's.
    /// A cased character is remapped with its other case as the second
    /// level, so XKB gives the key a letter's type and Caps Lock swaps its
    /// levels instead of capitalising it: the pair is (small, capital).
    remapped: Vec<(u8, (u32, u32), Vec<u32>)>,
    /// Held keys sharing the Shift this pressed for them, by Shift keycode.
    shifts: BTreeMap<u8, usize>,
}
impl Keys {
    /// The keycode for `symbol`, and the Shift keycode to hold with it.
    fn keycode(
        &mut self,
        server: &mut dyn KeyServer,
        symbol: u32,
        text: bool,
    ) -> Result<(u8, Option<u8>), PlatformError> {
        if let Some(code) = self.held.get(&symbol) {
            return Ok(*code);
        }
        let map = server.mapping()?;
        let width = map.width;
        if width == 0 {
            return Err(PlatformError::Native("empty X11 keyboard map".into()));
        }
        self.forget_lost(&map);
        let held_codes: BTreeSet<u8> = self.held.values().map(|(code, _)| *code).collect();
        let shift_held = self.held.contains_key(&0xffe1) || self.held.contains_key(&0xffe2);
        let shift_key = map
            .keysyms
            .chunks_exact(width)
            .position(|row| row[0] == 0xffe1)
            .map(|i| map.first + i as u8);
        // Caps Lock swaps a letter key's two levels, so typed text needs
        // Shift for a small letter and none for a capital. Shortcuts name
        // keys, which Caps Lock does not change.
        let mut level =
            |code: u8, second: bool, cased: bool| -> Result<(u8, Option<u8>), PlatformError> {
                let swapped = text && cased && server.lock()?;
                let shift = second != swapped;
                Ok((
                    code,
                    if shift && !shift_held {
                        shift_key
                    } else {
                        None
                    },
                ))
            };
        let choice = select_keycode(map.first, &map.keysyms, width, &self.remaps, symbol)?;
        // A character the layout lacks: a remap made for it before, or a
        // fresh one. Never for a character with a key of its own.
        if matches!(choice, Choice::Free(_) | Choice::Full)
            && let Some(at) = self
                .remapped
                .iter()
                .position(|(_, (small, capital), _)| *small == symbol || *capital == symbol)
        {
            let entry = self.remapped.remove(at);
            let (code, (small, capital)) = (entry.0, entry.1);
            self.remapped.push(entry);
            return level(
                code,
                symbol == capital && small != capital,
                small != capital,
            );
        }
        let code = match choice {
            Choice::Plain(code) => {
                let cased = map.row(code).is_some_and(cased);
                return level(code, false, cased);
            }
            Choice::Shifted { code, .. } => {
                let cased = map.row(code).is_some_and(cased);
                return level(code, true, cased);
            }
            Choice::Free(code) => code,
            Choice::Full => {
                // Reuse the least recently used remapped key not held now.
                let at = self
                    .remapped
                    .iter()
                    .position(|(code, _, _)| !held_codes.contains(code))
                    .ok_or_else(|| {
                        PlatformError::Unsupported(
                            "X11 has no free keycode for this character".into(),
                        )
                    })?;
                self.remapped.remove(at).0
            }
        };
        let original = map
            .row(code)
            .ok_or_else(|| PlatformError::Native("invalid X11 keyboard map".into()))?
            .to_vec();
        self.remaps.entry(code).or_insert(original);
        let (small, capital) = case_pair(symbol);
        let row: Vec<u32> = [small, capital].into_iter().cycle().take(width).collect();
        server.change(code, &row)?;
        let written = server
            .mapping()?
            .row(code)
            .ok_or_else(|| PlatformError::Native("invalid X11 keyboard map".into()))?
            .to_vec();
        self.remapped.push((code, (small, capital), written));
        let cased = small != capital;
        let mut level = |code: u8, second: bool| -> Result<(u8, Option<u8>), PlatformError> {
            let swapped = text && cased && server.lock()?;
            let shift = second != swapped;
            Ok((
                code,
                if shift && !shift_held {
                    shift_key
                } else {
                    None
                },
            ))
        };
        level(code, symbol == capital && cased)
    }
    /// Press the key for `symbol`; `text` asks for exactly that character.
    fn press(
        &mut self,
        server: &mut dyn KeyServer,
        symbol: u32,
        text: bool,
    ) -> Result<(), PlatformError> {
        // A second press of a held key holds nothing more: one release
        // must let it, and the Shift it took, go.
        if self.held.contains_key(&symbol) {
            return Ok(());
        }
        let (code, shift) = self.keycode(server, symbol, text)?;
        self.held.insert(symbol, (code, shift));
        if let Some(shift) = shift {
            let users = self.shifts.entry(shift).or_default();
            *users += 1;
            if *users == 1 {
                server.key(shift, true)?;
            }
        }
        server.key(code, true)
    }
    fn release(&mut self, server: &mut dyn KeyServer, symbol: u32) -> Result<(), PlatformError> {
        let Some((code, shift)) = self.held.get(&symbol).copied() else {
            return Ok(());
        };
        server.key(code, false)?;
        self.held.remove(&symbol);
        if let Some(shift) = shift
            && let Some(users) = self.shifts.get_mut(&shift)
        {
            *users = users.saturating_sub(1);
            if *users == 0 {
                // Kept until the release is sent, so release_all retries it.
                server.key(shift, false)?;
                self.shifts.remove(&shift);
            }
        }
        Ok(())
    }
    fn release_all(&mut self, server: &mut dyn KeyServer) {
        for symbol in self.held.keys().copied().collect::<Vec<_>>() {
            let _ = self.release(server, symbol);
        }
        for shift in std::mem::take(&mut self.shifts).into_keys() {
            let _ = server.key(shift, false);
        }
    }
    /// Forget remaps the keyboard map no longer has, as after setxkbmap or a
    /// new keyboard: their keys are the keyboard's own again, so they are
    /// neither used nor ever written back.
    fn forget_lost(&mut self, map: &Mapping) {
        let lost: Vec<u8> = self
            .remapped
            .iter()
            .filter(|(code, _, written)| map.row(*code) != Some(written.as_slice()))
            .map(|(code, _, _)| *code)
            .collect();
        for code in lost {
            self.remapped.retain(|(c, _, _)| *c != code);
            self.remaps.remove(&code);
        }
    }
    /// Put back the own row of every keycode still remapped.
    fn restore_all(&mut self, server: &mut dyn KeyServer) {
        if let Ok(map) = server.mapping() {
            self.forget_lost(&map);
            for (code, row) in std::mem::take(&mut self.remaps) {
                let _ = server.change(code, &row);
            }
        }
        self.remaps.clear();
        self.remapped.clear();
    }
}
struct XInput {
    state: Arc<Mutex<InputState>>,
}
#[async_trait]
impl NativeInput for XInput {
    async fn barrier(&mut self) -> Result<(), PlatformError> {
        // Joined event calls cannot enqueue later work. A reply on this exact
        // connection proves the server processed all preceding XTest requests.
        let state = self.state.clone();
        tokio::task::spawn_blocking(move || {
            let mut state = state.lock().map_err(failure)?;
            state
                .connection
                .get_input_focus()
                .map_err(failure)?
                .reply()
                .map_err(failure)?;
            // Wheel pulses also hold native buttons if a release was lost.
            for button in state
                .buttons
                .clone()
                .into_iter()
                .filter(|b| (4..=7).contains(b))
            {
                state.fake(xproto::BUTTON_RELEASE_EVENT, button, 0, 0)?;
                state.buttons.remove(&button);
            }
            state.axis = (0.0, 0.0);
            Ok(())
        })
        .await
        .map_err(failure)?
    }
    async fn event(&mut self, event: Event) -> Result<(), PlatformError> {
        self.event_cancellable(event, Arc::new(std::sync::atomic::AtomicBool::new(false)))
            .await
    }
    async fn event_cancellable(
        &mut self,
        event: Event,
        cancelled: Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<(), PlatformError> {
        self.event_fenced(
            event,
            crate::native::InputFence {
                cancelled,
                lock: None,
            },
        )
        .await
    }
    async fn event_fenced(
        &mut self,
        event: Event,
        fence: crate::native::InputFence,
    ) -> Result<(), PlatformError> {
        let state = self.state.clone();
        tokio::task::spawn_blocking(move || {
            let mut state = state.lock().map_err(failure)?;
            fence.check(event.is_release())?;
            state.fence = Some(fence);
            state.event(event)
        })
        .await
        .map_err(failure)?
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Choice {
    /// The symbol's own key.
    Plain(u8),
    /// Its key with Shift, as a person would type it.
    Shifted { code: u8, shift: u8 },
    /// A keycode with no symbols, to remap.
    Free(u8),
    /// Every free keycode is remapped already.
    Full,
}
fn select_keycode(
    first: u8,
    map: &[u32],
    width: usize,
    reserved: &BTreeMap<u8, Vec<u32>>,
    symbol: u32,
) -> Result<Choice, PlatformError> {
    if width == 0
        || !map.len().is_multiple_of(width)
        || map.len() / width > usize::from(u8::MAX - first) + 1
    {
        return Err(PlatformError::Native("invalid X11 keyboard map".into()));
    }
    let rows = || {
        map.chunks_exact(width)
            .enumerate()
            .map(|(i, row)| (first + i as u8, row))
            .filter(|(code, _)| !reserved.contains_key(code))
    };
    if let Some((code, _)) = rows().find(|(_, row)| row[0] == symbol) {
        return Ok(Choice::Plain(code));
    }
    let shift = rows()
        .find(|(_, row)| row[0] == 0xffe1)
        .map(|(code, _)| code);
    if let Some(shift) = shift
        && width > 1
        && let Some((code, _)) = rows().find(|(_, row)| row[1] == symbol)
    {
        return Ok(Choice::Shifted { code, shift });
    }
    Ok(rows()
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .find(|(_, row)| row.iter().all(|value| *value == 0))
        .map_or(Choice::Full, |(code, _)| Choice::Free(code)))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shifted_symbols_use_their_key_with_shift_and_unicode_a_free_keycode() {
        // a/A, Shift_L, then a free keycode.
        let map = [0x61, 0x41, 0xffe1, 0, 0, 0];
        let mut reserved = BTreeMap::new();
        assert_eq!(
            select_keycode(8, &map, 2, &reserved, 0x61).unwrap(),
            Choice::Plain(8)
        );
        assert_eq!(
            select_keycode(8, &map, 2, &reserved, 0x41).unwrap(),
            Choice::Shifted { code: 8, shift: 9 }
        );
        assert_eq!(
            select_keycode(8, &map, 2, &reserved, 0x0101f980).unwrap(),
            Choice::Free(10)
        );
        reserved.insert(10, vec![0, 0]);
        assert_eq!(
            select_keycode(8, &map, 2, &reserved, 0x0101f980).unwrap(),
            Choice::Full
        );
        // Without a Shift key, a capital needs a free keycode too.
        let map = [0x61, 0x41, 0, 0];
        assert_eq!(
            select_keycode(8, &map, 2, &BTreeMap::new(), 0x41).unwrap(),
            Choice::Free(9)
        );
    }
    /// A keyboard map of a, Shift_L, b, Caps_Lock and two free keycodes,
    /// seven keysyms a key, as Xvfb's XKB has it.
    #[derive(Default)]
    struct Server {
        rows: Vec<[u32; 7]>,
        keys: Vec<(u8, bool)>,
        changes: Vec<(u8, Vec<u32>)>,
        caps_lock: bool,
    }
    impl Server {
        fn new() -> Self {
            Self {
                rows: vec![
                    row(0x61, 0x41),
                    row(0xffe1, 0),
                    row(0x62, 0x42),
                    row(0xffe5, 0),
                    row(0, 0),
                    row(0, 0),
                    row(0x6b, 0x4b),
                ],
                ..Default::default()
            }
        }
    }
    impl KeyServer for Server {
        fn mapping(&mut self) -> Result<Mapping, PlatformError> {
            Ok(Mapping {
                first: 8,
                width: 7,
                keysyms: self.rows.iter().flatten().copied().collect(),
            })
        }
        fn change(&mut self, code: u8, written: &[u32]) -> Result<(), PlatformError> {
            // XKB keeps two groups of two levels of a core row and reads the
            // rest back empty: [é; 7] comes back as [é, é, é, é, 0, 0, 0].
            let mut kept = [0; 7];
            kept[..4].copy_from_slice(&written[..4]);
            self.rows[usize::from(code - 8)] = kept;
            self.changes.push((code, written.to_vec()));
            Ok(())
        }
        fn key(&mut self, code: u8, pressed: bool) -> Result<(), PlatformError> {
            self.keys.push((code, pressed));
            Ok(())
        }
        fn lock(&mut self) -> Result<bool, PlatformError> {
            Ok(self.caps_lock)
        }
    }
    fn row(base: u32, shifted: u32) -> [u32; 7] {
        [base, shifted, 0, 0, 0, 0, 0]
    }
    #[test]
    fn a_remap_xkb_rewrote_is_still_ophios_and_is_restored() {
        let mut server = Server::new();
        let mut keys = Keys::default();
        for _ in 0..3 {
            keys.press(&mut server, 0xe9, true).unwrap();
            keys.release(&mut server, 0xe9).unwrap();
        }
        // One remap, used three times, not a new keycode each press.
        assert_eq!(server.changes.len(), 1, "{:?}", server.changes);
        keys.restore_all(&mut server);
        assert_eq!(server.rows[5], row(0, 0), "the keycode types nothing again");
    }
    #[test]
    fn capitals_use_shift_and_characters_the_map_lacks_a_free_keycode() {
        let mut server = Server::new();
        let mut keys = Keys::default();
        for symbol in [0x41, 0xe9] {
            keys.press(&mut server, symbol, true).unwrap();
            keys.release(&mut server, symbol).unwrap();
        }
        // Shift (9) with a (8); é remapped onto the last free keycode, 13.
        assert_eq!(
            server.keys,
            [
                (9, true),
                (8, true),
                (8, false),
                (9, false),
                (13, true),
                (13, false)
            ]
        );
        // With its capital as the second level, so Caps Lock acts as on a letter.
        assert_eq!(
            server.changes,
            [(13, vec![0xe9, 0xc9, 0xe9, 0xc9, 0xe9, 0xc9, 0xe9])]
        );
        keys.restore_all(&mut server);
        assert_eq!(server.rows[5], row(0, 0));
    }
    #[test]
    fn shift_shared_by_two_held_capitals_stays_down_until_both_are_up() {
        let mut server = Server::new();
        let mut keys = Keys::default();
        keys.press(&mut server, 0x41, true).unwrap();
        keys.press(&mut server, 0x42, true).unwrap();
        keys.release(&mut server, 0x41).unwrap();
        assert!(!server.keys.contains(&(9, false)), "{:?}", server.keys);
        keys.release(&mut server, 0x42).unwrap();
        assert_eq!(server.keys.iter().filter(|k| **k == (9, true)).count(), 1);
        assert_eq!(server.keys.last(), Some(&(9, false)));
    }
    #[test]
    fn caps_lock_does_not_invert_typed_capitals() {
        let mut server = Server::new();
        server.caps_lock = true;
        let mut keys = Keys::default();
        // "Ab": with Caps Lock on, A is the a key alone and b needs Shift.
        for symbol in [0x41, 0x62] {
            keys.press(&mut server, symbol, true).unwrap();
            keys.release(&mut server, symbol).unwrap();
        }
        assert_eq!(
            server.keys,
            [
                (8, true),
                (8, false),
                (9, true),
                (10, true),
                (10, false),
                (9, false)
            ]
        );
        // A shortcut names a key, not a character: Caps Lock changes nothing.
        server.keys.clear();
        keys.press(&mut server, 0x61, false).unwrap();
        keys.release(&mut server, 0x61).unwrap();
        assert_eq!(server.keys, [(8, true), (8, false)]);
    }
    #[test]
    fn a_remap_the_keyboard_map_lost_is_made_again_and_not_restored() {
        let mut server = Server::new();
        let mut keys = Keys::default();
        keys.press(&mut server, 0xe9, true).unwrap();
        keys.release(&mut server, 0xe9).unwrap();
        // The map changes underneath, as setxkbmap or a new keyboard does:
        // keycode 13 types o now.
        server.rows[5] = row(0x6f, 0x4f);
        server.keys.clear();
        keys.press(&mut server, 0xe9, true).unwrap();
        let (code, _) = server.keys[0];
        assert_eq!(
            server.rows[usize::from(code - 8)][0],
            0xe9,
            "é has a key again"
        );
        keys.release(&mut server, 0xe9).unwrap();
        // And the keyboard's own new row is not overwritten with the old one.
        server.rows[usize::from(code - 8)] = row(0x70, 0x50);
        keys.restore_all(&mut server);
        assert_eq!(server.rows[5], row(0x6f, 0x4f));
        assert_eq!(server.rows[usize::from(code - 8)], row(0x70, 0x50));
    }
    #[test]
    fn a_key_pressed_twice_and_released_once_leaves_nothing_held() {
        let mut server = Server::new();
        let mut keys = Keys::default();
        keys.press(&mut server, 0x41, false).unwrap();
        keys.press(&mut server, 0x41, false).unwrap();
        keys.release(&mut server, 0x41).unwrap();
        // Shift (9) and the a key (8) each went down once and came up.
        for code in [8, 9] {
            let downs = server.keys.iter().filter(|k| **k == (code, true)).count();
            let ups = server.keys.iter().filter(|k| **k == (code, false)).count();
            assert_eq!((downs, ups), (1, 1), "keycode {code}: {:?}", server.keys);
        }
    }
    /// What an XKB client reads from the keys sent: Caps Lock swaps a letter
    /// key's levels, and otherwise capitalises a small letter it finds.
    fn typed(server: &Server) -> String {
        let character = |sym: u32| {
            char::from_u32(xkbcommon::xkb::keysym_to_utf32(
                xkbcommon::xkb::Keysym::new(sym),
            ))
            .unwrap_or('?')
        };
        let mut shift = false;
        let mut out = String::new();
        for (code, pressed) in &server.keys {
            let row = server.rows[usize::from(code - 8)];
            if row[0] == 0xffe1 {
                shift = *pressed;
                continue;
            }
            if !pressed {
                continue;
            }
            let c = if cased(&row) {
                character(if shift != server.caps_lock {
                    row[1]
                } else {
                    row[0]
                })
            } else {
                let c = character(if shift && row[1] != 0 { row[1] } else { row[0] });
                if server.caps_lock {
                    c.to_uppercase().next().unwrap()
                } else {
                    c
                }
            };
            out.push(c);
        }
        out
    }
    #[test]
    fn caps_lock_does_not_capitalise_remapped_characters() {
        let mut server = Server::new();
        server.caps_lock = true;
        let mut keys = Keys::default();
        for c in "aéÉ".chars() {
            let symbol = crate::input::character_keysym(c).unwrap();
            keys.press(&mut server, symbol, true).unwrap();
            keys.release(&mut server, symbol).unwrap();
        }
        assert_eq!(typed(&server), "aéÉ");
    }
    #[test]
    fn a_remap_never_stands_in_for_a_key_the_layout_has() {
        // The Kelvin sign's small form is k, but it is not k's capital.
        for (odd, plain) in [(0x0100_212a, 0x6b), (0x0100_212a, 0x4b)] {
            let mut server = Server::new();
            let mut keys = Keys::default();
            keys.press(&mut server, odd, true).unwrap();
            keys.release(&mut server, odd).unwrap();
            server.keys.clear();
            // Ctrl+k must be the k key, which shortcuts grab by keycode.
            keys.press(&mut server, plain, false).unwrap();
            keys.release(&mut server, plain).unwrap();
            assert!(
                server.keys.contains(&(14, true)),
                "{plain:x}: {:?}",
                server.keys
            );
        }
        // ı, ſ and the Ångström sign get no partner either; é does.
        for odd in ['ı', 'ſ', '\u{212b}', '\u{212a}'] {
            let symbol = crate::input::character_keysym(odd).unwrap();
            assert_eq!(case_pair(symbol), (symbol, symbol), "{odd}");
        }
        assert_eq!(case_pair(0xe9), (0xe9, 0xc9));
    }
    #[test]
    fn titlecase_digraphs_remap_as_themselves_and_reuse_the_remap() {
        for titlecase in ['ǅ', 'ǈ', 'ǋ', 'ǲ'] {
            let symbol = crate::input::character_keysym(titlecase).unwrap();
            assert_eq!(case_pair(symbol), (symbol, symbol), "{titlecase}");
            let mut server = Server::new();
            let mut keys = Keys::default();
            for _ in 0..2 {
                keys.press(&mut server, symbol, true).unwrap();
                keys.release(&mut server, symbol).unwrap();
            }
            assert_eq!(typed(&server), titlecase.to_string().repeat(2));
            assert_eq!(server.changes.len(), 1, "{titlecase}");
        }
    }
    #[test]
    fn truecolour_masks_scale_to_eight_bits() {
        assert_eq!(component(0xf800, 0xf800), 255);
        assert_eq!(component(0x07e0, 0x07e0), 255);
        assert_eq!(component(0, 0x00ff0000), 0);
    }
}
