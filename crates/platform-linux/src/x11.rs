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
                held: BTreeMap::new(),
                buttons: BTreeSet::new(),
                remaps: BTreeMap::new(),
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
    held: BTreeMap<u32, u8>,
    buttons: BTreeSet<u8>,
    remaps: BTreeMap<u8, Vec<u32>>,
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
    fn keycode(&mut self, symbol: u32) -> Result<u8, PlatformError> {
        if let Some(code) = self.held.get(&symbol) {
            return Ok(*code);
        }
        let setup = self.connection.setup();
        let first = setup.min_keycode;
        let count = setup.max_keycode - first + 1;
        let map = self
            .connection
            .get_keyboard_mapping(first, count)
            .map_err(failure)?
            .reply()
            .map_err(failure)?;
        let width = map.keysyms_per_keycode as usize;
        if width == 0 {
            return Err(PlatformError::Native("empty X11 keyboard map".into()));
        }
        let (code, remap) = select_keycode(first, &map.keysyms, width, &self.remaps, symbol)?;
        if !remap {
            return Ok(code);
        }
        let start = usize::from(code - first) * width;
        let row = &map.keysyms[start..start + width];
        let mut replacement = vec![symbol; width];
        replacement[0] = symbol;
        self.remaps.insert(code, row.to_vec());
        self.connection
            .change_keyboard_mapping(1, code, map.keysyms_per_keycode, &replacement)
            .map_err(failure)?
            .check()
            .map_err(failure)?;
        Ok(code)
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
            Event::Key { keysym, pressed } => {
                let code = self.keycode(keysym)?;
                if pressed {
                    self.held.insert(keysym, code);
                }
                self.fake(
                    if pressed {
                        xproto::KEY_PRESS_EVENT
                    } else {
                        xproto::KEY_RELEASE_EVENT
                    },
                    code,
                    0,
                    0,
                )?;
                if !pressed {
                    self.held.remove(&keysym);
                    self.restore(code).map_err(crate::native::uncertain)?;
                }
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
    fn restore(&mut self, code: u8) -> Result<(), PlatformError> {
        if let Some(row) = self.remaps.get(&code) {
            self.connection
                .change_keyboard_mapping(1, code, row.len() as u8, row)
                .map_err(failure)?
                .check()
                .map_err(failure)?;
            self.remaps.remove(&code);
        }
        Ok(())
    }
}
impl Drop for InputState {
    fn drop(&mut self) {
        for code in self.held.values() {
            let _ = self.fake(xproto::KEY_RELEASE_EVENT, *code, 0, 0);
        }
        for code in &self.buttons {
            let _ = self.fake(xproto::BUTTON_RELEASE_EVENT, *code, 0, 0);
        }
        let codes: Vec<_> = self.remaps.keys().copied().collect();
        for code in codes {
            let _ = self.restore(code);
        }
        let _ = self.connection.flush();
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

fn select_keycode(
    first: u8,
    map: &[u32],
    width: usize,
    reserved: &BTreeMap<u8, Vec<u32>>,
    symbol: u32,
) -> Result<(u8, bool), PlatformError> {
    if width == 0
        || !map.len().is_multiple_of(width)
        || map.len() / width > usize::from(u8::MAX - first) + 1
    {
        return Err(PlatformError::Native("invalid X11 keyboard map".into()));
    }
    for (i, row) in map.chunks_exact(width).enumerate() {
        let code = first + i as u8;
        if row[0] == symbol && !reserved.contains_key(&code) {
            return Ok((code, false));
        }
    }
    for (i, row) in map.chunks_exact(width).enumerate().rev() {
        let code = first + i as u8;
        if row.iter().all(|value| *value == 0) && !reserved.contains_key(&code) {
            return Ok((code, true));
        }
    }
    Err(PlatformError::Unsupported(
        "X11 has no free keycode for this character".into(),
    ))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn uppercase_and_unicode_use_unused_keycodes_without_changing_modifiers() {
        let map = [0x61, 0x41, 0, 0];
        let mut reserved = BTreeMap::new();
        assert_eq!(
            select_keycode(8, &map, 2, &reserved, 0x61).unwrap(),
            (8, false)
        );
        assert_eq!(
            select_keycode(8, &map, 2, &reserved, 0x41).unwrap(),
            (9, true)
        );
        assert_eq!(
            select_keycode(8, &map, 2, &reserved, 0x0101f980).unwrap(),
            (9, true)
        );
        reserved.insert(9, vec![0, 0]);
        assert!(select_keycode(8, &map, 2, &reserved, 0x41).is_err());
    }
    #[test]
    fn truecolour_masks_scale_to_eight_bits() {
        assert_eq!(component(0xf800, 0xf800), 255);
        assert_eq!(component(0x07e0, 0x07e0), 255);
        assert_eq!(component(0, 0x00ff0000), 0);
    }
}
