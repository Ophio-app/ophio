// SPDX-License-Identifier: Apache-2.0
//! Puts events in the system's queue. The shared input code decides what to send
//! and releases what it holds; this says it in the system's own terms.
#![allow(unsafe_code)]

use super::native::{self, CGPoint};
use platform_windows::desktop::{
    geometry::Layout,
    input::{Button, Event, Injector, Key},
};
use ports::desktop::PlatformError;
use protocol::control::DesktopLockState;
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::c_void,
    time::{Duration, Instant},
};

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGEventSourceCreate(state: i32) -> *const c_void;
    fn CGEventCreateMouseEvent(
        source: *const c_void,
        kind: u32,
        at: CGPoint,
        button: u32,
    ) -> *const c_void;
    fn CGEventCreateKeyboardEvent(source: *const c_void, key: u16, down: bool) -> *const c_void;
    fn CGEventCreateScrollWheelEvent2(
        source: *const c_void,
        units: u32,
        wheels: u32,
        first: i32,
        second: i32,
        third: i32,
    ) -> *const c_void;
    fn CGEventSetFlags(event: *const c_void, flags: u64);
    fn CGEventSetIntegerValueField(event: *const c_void, field: u32, value: i64);
    fn CGEventKeyboardSetUnicodeString(event: *const c_void, length: usize, text: *const u16);
    fn CGEventPost(tap: u32, event: *const c_void);
}
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(value: *const c_void);
}

const HID_SYSTEM_STATE: i32 = 1;
const HID_TAP: u32 = 0;
const SCROLL_LINES: u32 = 1;
const CLICK_STATE_FIELD: u32 = 1;
/// How soon, and how near, a second press makes a double click.
const CLICK_WITHIN: Duration = Duration::from_millis(500);
const CLICK_NEAR: f64 = 4.0;
const WHEEL_NOTCH: i32 = 120;

const FLAG_SHIFT: u64 = 0x0002_0000;
const FLAG_CONTROL: u64 = 0x0004_0000;
const FLAG_OPTION: u64 = 0x0008_0000;
const FLAG_COMMAND: u64 = 0x0010_0000;

const KEYCODE_LEFT_SHIFT: u16 = 0x38;
const KEYCODE_LEFT_OPTION: u16 = 0x3a;
const MODIFIER_FLAGS: [u64; 2] = [FLAG_SHIFT, FLAG_OPTION];
const MODIFIER_KEYCODES: [u16; 2] = [KEYCODE_LEFT_SHIFT, KEYCODE_LEFT_OPTION];
const CHARACTER_MODIFIERS: [u64; 4] = [0, FLAG_SHIFT, FLAG_OPTION, FLAG_SHIFT | FLAG_OPTION];
/// F1 through F20 on a Mac keyboard. F21 through F24 have no Mac equivalent.
const FUNCTIONS: [u16; 20] = [
    0x7a, 0x78, 0x63, 0x76, 0x60, 0x61, 0x62, 0x64, 0x65, 0x6d, 0x67, 0x6f, 0x69, 0x6b, 0x71, 0x6a,
    0x40, 0x4f, 0x50, 0x5a,
];

/// The Mac key code for a shared Windows virtual key. Modifier state is tracked
/// separately because left and right variants share a CoreGraphics flag.
pub(crate) fn mac_key(virtual_key: u16) -> Result<(u16, u64), PlatformError> {
    let plain = |code| Ok((code, 0));
    match virtual_key {
        0xa0 => Ok((0x38, FLAG_SHIFT)),   // Left Shift
        0xa1 => Ok((0x3c, FLAG_SHIFT)),   // Right Shift
        0xa2 => Ok((0x3b, FLAG_CONTROL)), // Left Control
        0xa3 => Ok((0x3e, FLAG_CONTROL)), // Right Control
        0xa4 => Ok((0x3a, FLAG_OPTION)),  // Left Option
        0xa5 => Ok((0x3d, FLAG_OPTION)),  // Right Option / AltGr
        0x5b => Ok((0x37, FLAG_COMMAND)), // Left Command
        0x5c => Ok((0x36, FLAG_COMMAND)), // Right Command
        0x0d => plain(0x24),
        0x09 => plain(0x30),
        0x1b => plain(0x35),
        0x08 => plain(0x33),
        0x2e => plain(0x75),
        0x2d => plain(0x72),
        0x24 => plain(0x73),
        0x23 => plain(0x77),
        0x21 => plain(0x74),
        0x22 => plain(0x79),
        0x25 => plain(0x7b),
        0x26 => plain(0x7e),
        0x27 => plain(0x7c),
        0x28 => plain(0x7d),
        0x20 => plain(0x31),
        0x14 => plain(0x39),
        0x90 => plain(0x47), // Numeric keypad Clear is the closest Mac equivalent.
        0xad => plain(0x4a), // Mute
        0xae => plain(0x49), // Volume down
        0xaf => plain(0x48), // Volume up
        0xb0..=0xb3 => Err(PlatformError::Unsupported(
            "This Mac cannot send media next, previous, stop, or play/pause keys".into(),
        )),
        0x2c => Err(PlatformError::Unsupported(
            "This Mac has no Print Screen key".into(),
        )),
        0x91 => Err(PlatformError::Unsupported(
            "This Mac has no Scroll Lock key".into(),
        )),
        0x13 => Err(PlatformError::Unsupported(
            "This Mac has no Pause key".into(),
        )),
        0x5d => Err(PlatformError::Unsupported(
            "This Mac has no Context Menu key".into(),
        )),
        0x70..=0x83 => plain(FUNCTIONS[usize::from(virtual_key - 0x70)]),
        0x84..=0x87 => Err(PlatformError::Unsupported(
            "F21 through F24 are not available on a Mac".into(),
        )),
        _ => Err(PlatformError::Unsupported(format!(
            "This Mac has no mapping for virtual key 0x{virtual_key:02X}"
        ))),
    }
}

/// Each key's output in the current layout, at no modifier, Shift, Option, and
/// Shift+Option. Kept independent of Carbon so lookup and choice can be tested.
#[derive(Debug, Clone)]
struct CharacterTable {
    output: [[Option<char>; 4]; 128],
}
impl Default for CharacterTable {
    fn default() -> Self {
        Self {
            output: [[None; 4]; 128],
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CharacterChoice {
    keycode: u16,
    modifiers: u64,
}
impl CharacterTable {
    fn set(&mut self, keycode: u16, modifiers: u64, character: char) {
        let Some(row) = self.output.get_mut(usize::from(keycode)) else {
            return;
        };
        if let Some(index) = CHARACTER_MODIFIERS
            .iter()
            .position(|&candidate| candidate == modifiers)
        {
            row[index] = Some(character);
        }
    }

    /// Prefer fewer layout modifiers, then reuse held modifiers, then choose the
    /// lowest key code for a stable result when a character has several sources.
    fn choose(&self, character: char, held: u64) -> Option<CharacterChoice> {
        let mut best: Option<((u32, u32, u16, usize), CharacterChoice)> = None;
        for (keycode, row) in self.output.iter().enumerate() {
            for (index, &modifiers) in CHARACTER_MODIFIERS.iter().enumerate() {
                if row[index] != Some(character) {
                    continue;
                }
                let choice = CharacterChoice {
                    keycode: keycode as u16,
                    modifiers,
                };
                let rank = (
                    modifiers.count_ones(),
                    (modifiers & !held).count_ones(),
                    choice.keycode,
                    index,
                );
                if best.map_or(true, |(best_rank, _)| rank < best_rank) {
                    best = Some((rank, choice));
                }
            }
        }
        best.map(|(_, choice)| choice)
    }
}

#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    fn TISCopyCurrentKeyboardLayoutInputSource() -> *const c_void;
    fn TISGetInputSourceProperty(source: *const c_void, property: *const c_void) -> *const c_void;
    static kTISPropertyUnicodeKeyLayoutData: *const c_void;
    fn LMGetKbdType() -> u8;
    fn UCKeyTranslate(
        layout: *const c_void,
        virtual_key_code: u16,
        key_action: u16,
        modifier_key_state: u32,
        keyboard_type: u32,
        options: u32,
        dead_key_state: *mut u32,
        max_length: u32,
        actual_length: *mut u32,
        output: *mut u16,
    ) -> i32;
}
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFDataGetBytePtr(data: *const c_void) -> *const u8;
    fn CFDataGetLength(data: *const c_void) -> isize;
}

const KEY_ACTION_DISPLAY: u16 = 3;
const TRANSLATE_NO_DEAD_KEYS: u32 = 1;
const CARBON_SHIFT_KEY: u32 = 0x0200;
const CARBON_OPTION_KEY: u32 = 0x0800;

/// Text Input Sources is not safe to call from multiple threads at once.
static KEYBOARD_LAYOUT_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct RetainedKeyboardLayoutSource(*const c_void);
impl Drop for RetainedKeyboardLayoutSource {
    fn drop(&mut self) {
        // SAFETY: this owns the retained source returned by TISCopy, exactly once.
        unsafe { CFRelease(self.0) };
    }
}

/// Read the current input source and translate every Mac key in the four
/// requested modifier states. The retained input source keeps its layout bytes
/// alive throughout the calls to UCKeyTranslate.
fn current_character_table() -> Result<CharacterTable, PlatformError> {
    // Recover a poisoned mutex so a previous panic cannot let concurrent TIS
    // calls through. The guard covers source lookup, all layout use, and release.
    let _tis_guard = KEYBOARD_LAYOUT_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // SAFETY: this system call takes no arguments and returns a retained source.
    let source = unsafe { TISCopyCurrentKeyboardLayoutInputSource() };
    if source.is_null() {
        return Err(PlatformError::TemporarilyUnavailable(
            "The Mac's current keyboard layout is unavailable".into(),
        ));
    }
    let source = RetainedKeyboardLayoutSource(source);
    let result = (|| {
        // SAFETY: the property symbol is a system CFStringRef, and source is the
        // retained input-source object returned above.
        let property = unsafe { kTISPropertyUnicodeKeyLayoutData };
        // SAFETY: TISGetInputSourceProperty borrows both live CF objects.
        let data = unsafe { TISGetInputSourceProperty(source.0, property) };
        if data.is_null() {
            return Err(PlatformError::TemporarilyUnavailable(
                "The Mac's current keyboard layout has no Unicode key data".into(),
            ));
        }
        // SAFETY: data is the CFData property of the still-live input source.
        let (length, bytes) = unsafe { (CFDataGetLength(data), CFDataGetBytePtr(data)) };
        if length <= 0 || bytes.is_null() {
            return Err(PlatformError::TemporarilyUnavailable(
                "The Mac's current keyboard layout data is empty".into(),
            ));
        }
        // SAFETY: this no-argument system call reads the current keyboard type.
        let keyboard_type = u32::from(unsafe { LMGetKbdType() });
        let layout = bytes.cast::<c_void>();
        let mut table = CharacterTable::default();
        for keycode in 0u16..128 {
            for modifiers in CHARACTER_MODIFIERS {
                let carbon_modifiers = (if modifiers & FLAG_SHIFT != 0 {
                    CARBON_SHIFT_KEY
                } else {
                    0
                }) | (if modifiers & FLAG_OPTION != 0 {
                    CARBON_OPTION_KEY
                } else {
                    0
                });
                let mut dead_key_state = 0u32;
                let mut actual_length = 0u32;
                let mut output = [0u16; 8];
                // SAFETY: layout points into the live CFData above; all input and
                // output buffers are valid for this call and sized to their counts.
                let status = unsafe {
                    UCKeyTranslate(
                        layout,
                        keycode,
                        KEY_ACTION_DISPLAY,
                        carbon_modifiers,
                        keyboard_type,
                        TRANSLATE_NO_DEAD_KEYS,
                        &raw mut dead_key_state,
                        output.len() as u32,
                        &raw mut actual_length,
                        output.as_mut_ptr(),
                    )
                };
                let length = actual_length as usize;
                if status != 0 || length == 0 || length > output.len() {
                    continue;
                }
                let Ok(text) = String::from_utf16(&output[..length]) else {
                    continue;
                };
                let mut chars = text.chars();
                let Some(character) = chars.next() else {
                    continue;
                };
                if chars.next().is_none() {
                    table.set(keycode, modifiers, character);
                }
            }
        }
        Ok(table)
    })();
    // Release the input source before the mutex guard leaves scope.
    drop(source);
    result
}

/// How many lines a wheel movement scrolls: at least one, since the shared code
/// only says when a whole step has built up.
pub(crate) fn wheel_lines(delta: i32) -> i32 {
    match delta / WHEEL_NOTCH {
        0 => delta.signum(),
        lines => lines,
    }
}

/// The system's event kind for a button going down or up, and the button's number.
pub(crate) fn button_event(button: Button, down: bool) -> (u32, u32) {
    match (button, down) {
        (Button::Left, true) => (1, 0),
        (Button::Left, false) => (2, 0),
        (Button::Right, true) => (3, 1),
        (Button::Right, false) => (4, 1),
        (Button::Middle, true) => (25, 2),
        (Button::Middle, false) => (26, 2),
    }
}

/// The system's event kind for moving, which differs while a button is held.
pub(crate) fn move_event(held: Option<Button>) -> (u32, u32) {
    match held {
        None => (5, 0),
        Some(Button::Left) => (6, 0),
        Some(Button::Right) => (7, 1),
        Some(Button::Middle) => (27, 2),
    }
}

/// Which click a press is: a second soon after and near the first makes a double.
#[derive(Debug, Default)]
pub(crate) struct Clicks {
    last: Option<(Button, Instant, CGPoint, i64)>,
}
impl Clicks {
    pub(crate) fn press(&mut self, button: Button, now: Instant, at: CGPoint) -> i64 {
        let count = match self.last {
            Some((before, then, there, count))
                if before == button
                    && now.saturating_duration_since(then) <= CLICK_WITHIN
                    && (there.x - at.x).abs() <= CLICK_NEAR
                    && (there.y - at.y).abs() <= CLICK_NEAR =>
            {
                count + 1
            }
            _ => 1,
        };
        self.last = Some((button, now, at, count));
        count
    }
}

/// Joins a character outside the 16-bit range, which arrives in two halves, so
/// the system gets it whole.
#[derive(Debug, Default)]
pub(crate) struct Surrogates {
    high: Option<u16>,
    pair: Option<[u16; 2]>,
}
impl Surrogates {
    /// What to send for a unit going down or up: nothing for the first half.
    pub(crate) fn unit(&mut self, unit: u16, down: bool) -> Option<Vec<u16>> {
        match unit {
            0xd800..=0xdbff => {
                if down {
                    self.high = Some(unit);
                }
                None
            }
            0xdc00..=0xdfff => {
                if down {
                    self.pair = self.high.take().map(|high| [high, unit]);
                }
                self.pair.map(|pair| pair.to_vec())
            }
            _ => Some(vec![unit]),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct HeldCharacter {
    keycode: u16,
    temporary_modifiers: u64,
    key_down: bool,
}

pub(crate) struct MacInjector {
    source: *const c_void,
    at: CGPoint,
    flags: u64,
    temporary_modifiers: [u8; 2],
    held_modifiers: BTreeSet<u16>,
    held_characters: BTreeMap<char, HeldCharacter>,
    held: Option<Button>,
    clicks: Clicks,
    surrogates: Surrogates,
}
// SAFETY: the event source is a thread-safe system object that only this owner
// releases, and every use is through `&mut self`.
unsafe impl Send for MacInjector {}

impl MacInjector {
    pub(crate) fn new() -> Result<Self, PlatformError> {
        // SAFETY: takes a constant, and the result is checked.
        let source = unsafe { CGEventSourceCreate(HID_SYSTEM_STATE) };
        if source.is_null() {
            return Err(PlatformError::TemporarilyUnavailable(
                "This session can't send input to the Mac's screen".into(),
            ));
        }
        Ok(Self {
            source,
            at: CGPoint::default(),
            flags: 0,
            temporary_modifiers: [0; 2],
            held_modifiers: BTreeSet::new(),
            held_characters: BTreeMap::new(),
            held: None,
            clicks: Clicks::default(),
            surrogates: Surrogates::default(),
        })
    }

    fn flags_for(held_modifiers: &BTreeSet<u16>, temporary_modifiers: &[u8; 2]) -> u64 {
        let mut flags = 0;
        for &virtual_key in held_modifiers {
            flags |= match virtual_key {
                0xa0 | 0xa1 => FLAG_SHIFT,
                0xa2 | 0xa3 => FLAG_CONTROL,
                0xa4 | 0xa5 => FLAG_OPTION,
                0x5b | 0x5c => FLAG_COMMAND,
                _ => 0,
            };
        }
        for (index, &flag) in MODIFIER_FLAGS.iter().enumerate() {
            if temporary_modifiers[index] != 0 {
                flags |= flag;
            }
        }
        flags
    }

    fn acquire_temporary_modifier(&mut self, flag: u64) -> Result<bool, PlatformError> {
        let index = MODIFIER_FLAGS
            .iter()
            .position(|&candidate| candidate == flag)
            .ok_or_else(|| PlatformError::Native("Invalid temporary Mac modifier".into()))?;
        let mut counts = self.temporary_modifiers;
        if counts[index] == u8::MAX {
            return Err(PlatformError::TemporarilyUnavailable(
                "Too many character keys are holding a Mac modifier".into(),
            ));
        }
        if counts[index] == 0 && Self::flags_for(&self.held_modifiers, &counts) & flag != 0 {
            return Ok(false);
        }
        counts[index] += 1;
        let flags = Self::flags_for(&self.held_modifiers, &counts);
        if self.temporary_modifiers[index] == 0 {
            let source = self.source;
            let keycode = MODIFIER_KEYCODES[index];
            self.post_with_flags(
                flags,
                // SAFETY: the source is live, and the key code and state are valid.
                || unsafe { CGEventCreateKeyboardEvent(source, keycode, true) },
                |_| {},
            )?;
        }
        self.temporary_modifiers = counts;
        self.flags = flags;
        Ok(true)
    }

    fn release_temporary_modifier(&mut self, flag: u64) -> Result<(), PlatformError> {
        let index = MODIFIER_FLAGS
            .iter()
            .position(|&candidate| candidate == flag)
            .ok_or_else(|| PlatformError::Native("Invalid temporary Mac modifier".into()))?;
        if self.temporary_modifiers[index] == 0 {
            return Err(PlatformError::Native(
                "A temporary Mac modifier was released twice".into(),
            ));
        }
        let mut counts = self.temporary_modifiers;
        counts[index] -= 1;
        let flags = Self::flags_for(&self.held_modifiers, &counts);
        if counts[index] == 0 {
            let source = self.source;
            let keycode = MODIFIER_KEYCODES[index];
            self.post_with_flags(
                flags,
                // SAFETY: the source is live, and this releases our synthetic key.
                || unsafe { CGEventCreateKeyboardEvent(source, keycode, false) },
                |_| {},
            )?;
        }
        self.temporary_modifiers = counts;
        self.flags = flags;
        Ok(())
    }

    fn press_character(&mut self, character: char) -> Result<(), PlatformError> {
        let table = current_character_table()?;
        let choice = table.choose(character, self.flags).ok_or_else(|| {
            PlatformError::Unsupported(format!(
                "The current Mac keyboard layout cannot type {character:?} with one key"
            ))
        })?;
        if self.held_characters.contains_key(&character) {
            return Err(PlatformError::Native(
                "This character key is already held by the Mac input session".into(),
            ));
        }
        self.held_characters.insert(
            character,
            HeldCharacter {
                keycode: choice.keycode,
                temporary_modifiers: 0,
                key_down: false,
            },
        );
        let needed = choice.modifiers & !Self::flags_for(&self.held_modifiers, &[0; 2]);
        for flag in MODIFIER_FLAGS {
            if needed & flag != 0 && self.acquire_temporary_modifier(flag)? {
                self.held_characters
                    .get_mut(&character)
                    .expect("character state was inserted above")
                    .temporary_modifiers |= flag;
            }
        }
        let source = self.source;
        self.post(
            // SAFETY: the source is live and the selected key code is in range.
            || unsafe { CGEventCreateKeyboardEvent(source, choice.keycode, true) },
            |_| {},
        )?;
        self.held_characters
            .get_mut(&character)
            .expect("character state was inserted above")
            .key_down = true;
        Ok(())
    }

    fn release_character(&mut self, character: char) -> Result<(), PlatformError> {
        let Some(mut held) = self.held_characters.get(&character).copied() else {
            return Ok(());
        };
        let mut first_error = None;
        if held.key_down {
            let source = self.source;
            match self.post(
                // SAFETY: this releases the key code recorded for the matching down.
                || unsafe { CGEventCreateKeyboardEvent(source, held.keycode, false) },
                |_| {},
            ) {
                Ok(()) => held.key_down = false,
                Err(error) => first_error = Some(error),
            }
        }
        for flag in MODIFIER_FLAGS.into_iter().rev() {
            if held.temporary_modifiers & flag == 0 {
                continue;
            }
            match self.release_temporary_modifier(flag) {
                Ok(()) => held.temporary_modifiers &= !flag,
                Err(error) if first_error.is_none() => first_error = Some(error),
                Err(_) => {}
            }
        }
        if held.key_down || held.temporary_modifiers != 0 {
            self.held_characters.insert(character, held);
        } else {
            self.held_characters.remove(&character);
        }
        first_error.map_or(Ok(()), Err)
    }

    fn post(
        &self,
        make: impl FnOnce() -> *const c_void,
        then: impl FnOnce(*const c_void),
    ) -> Result<(), PlatformError> {
        self.post_with_flags(self.flags, make, then)
    }

    /// Posts an event made by `make`, with the specified modifier flags.
    fn post_with_flags(
        &self,
        flags: u64,
        make: impl FnOnce() -> *const c_void,
        then: impl FnOnce(*const c_void),
    ) -> Result<(), PlatformError> {
        let event = make();
        if event.is_null() {
            return Err(PlatformError::Native(
                "The Mac refused an input event".into(),
            ));
        }
        then(event);
        // SAFETY: `event` is checked and ours to release; posting copies it into
        // the system's queue and it isn't used afterwards.
        unsafe {
            CGEventSetFlags(event, flags);
            CGEventPost(HID_TAP, event);
            CFRelease(event);
        }
        Ok(())
    }
}

impl Drop for MacInjector {
    fn drop(&mut self) {
        // SAFETY: ours, released once.
        unsafe { CFRelease(self.source) };
    }
}

impl Injector for MacInjector {
    fn check_layout(&mut self, layout: &Layout, deadline: Instant) -> Result<(), PlatformError> {
        if Instant::now() >= deadline {
            return Err(PlatformError::TemporarilyUnavailable(
                "The Mac is taking too long to accept input".into(),
            ));
        }
        // Never type into a login screen, or into what wakes behind it.
        if native::lock_state() != DesktopLockState::Unlocked {
            return Err(PlatformError::Locked);
        }
        let mut now = native::monitors()?;
        now.sort_by(|a, b| a.name.cmp(&b.name));
        if now != layout.monitors() {
            return Err(PlatformError::GeometryChanged);
        }
        Ok(())
    }

    fn emit(&mut self, event: Event) -> Result<(), PlatformError> {
        let source = self.source;
        match event {
            Event::Move { physical, .. } => {
                self.at = CGPoint {
                    x: f64::from(physical.0),
                    y: f64::from(physical.1),
                };
                let (kind, number) = move_event(self.held);
                let at = self.at;
                self.post(
                    // SAFETY: a live source and plain values.
                    || unsafe { CGEventCreateMouseEvent(source, kind, at, number) },
                    |_| {},
                )
            }
            Event::Button { button, down } => {
                let count = if down {
                    self.clicks.press(button, Instant::now(), self.at)
                } else {
                    self.clicks.last.map_or(1, |(_, _, _, count)| count)
                };
                self.held = if down { Some(button) } else { None };
                let (kind, number) = button_event(button, down);
                let at = self.at;
                self.post(
                    // SAFETY: a live source and plain values.
                    || unsafe { CGEventCreateMouseEvent(source, kind, at, number) },
                    // SAFETY: called with the live event `post` made.
                    |event| unsafe { CGEventSetIntegerValueField(event, CLICK_STATE_FIELD, count) },
                )
            }
            Event::Key {
                key: Key::Virtual { code, .. },
                down,
            } => {
                let (keycode, flag) = mac_key(code)?;
                let source = self.source;
                if flag == 0 {
                    self.post(
                        // SAFETY: the source is live and the key code is supported.
                        || unsafe { CGEventCreateKeyboardEvent(source, keycode, down) },
                        |_| {},
                    )
                } else {
                    let mut held = self.held_modifiers.clone();
                    if down {
                        held.insert(code);
                    } else {
                        held.remove(&code);
                    }
                    let flags = Self::flags_for(&held, &self.temporary_modifiers);
                    self.post_with_flags(
                        flags,
                        // SAFETY: the source is live and the key code is supported.
                        || unsafe { CGEventCreateKeyboardEvent(source, keycode, down) },
                        |_| {},
                    )?;
                    self.held_modifiers = held;
                    self.flags = flags;
                    Ok(())
                }
            }
            Event::Key {
                key: Key::Character(character),
                down: true,
            } => self.press_character(character),
            Event::Key {
                key: Key::Character(character),
                down: false,
            } => self.release_character(character),
            Event::Key {
                key: Key::Unicode(unit),
                down,
            } => {
                let Some(text) = self.surrogates.unit(unit, down) else {
                    return Ok(());
                };
                self.post(
                    // SAFETY: a live source and plain values.
                    || unsafe { CGEventCreateKeyboardEvent(source, 0, down) },
                    // SAFETY: called with the live event `post` made; `text`
                    // outlives the call, which copies it.
                    |event| unsafe {
                        CGEventKeyboardSetUnicodeString(event, text.len(), text.as_ptr());
                    },
                )
            }
            Event::Wheel { horizontal, delta } => {
                let lines = wheel_lines(delta);
                let (first, second) = if horizontal { (0, lines) } else { (lines, 0) };
                self.post(
                    // SAFETY: a live source and plain values.
                    || unsafe {
                        CGEventCreateScrollWheelEvent2(source, SCROLL_LINES, 2, first, second, 0)
                    },
                    |_| {},
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn common_virtual_keys_and_mac_function_keys_map_correctly() {
        assert_eq!(mac_key(0x0d).unwrap(), (0x24, 0));
        assert_eq!(mac_key(0x70).unwrap(), (0x7a, 0), "F1");
        assert_eq!(mac_key(0x83).unwrap(), (0x5a, 0), "F20");
        assert_eq!(mac_key(0x7c).unwrap(), (0x69, 0), "F13");
        assert_eq!(mac_key(0x7d).unwrap(), (0x6b, 0), "F14");
        assert_eq!(mac_key(0x7e).unwrap(), (0x71, 0), "F15");
    }

    #[test]
    fn left_and_right_modifiers_use_distinct_mac_keys_and_shared_flags() {
        for (virtual_key, mac_code, flag) in [
            (0xa0, 0x38, FLAG_SHIFT),
            (0xa1, 0x3c, FLAG_SHIFT),
            (0xa2, 0x3b, FLAG_CONTROL),
            (0xa3, 0x3e, FLAG_CONTROL),
            (0xa4, 0x3a, FLAG_OPTION),
            (0xa5, 0x3d, FLAG_OPTION),
            (0x5b, 0x37, FLAG_COMMAND),
            (0x5c, 0x36, FLAG_COMMAND),
        ] {
            assert_eq!(mac_key(virtual_key).unwrap(), (mac_code, flag));
        }
    }

    #[test]
    fn media_keys_map_only_where_a_mac_has_a_direct_equivalent() {
        assert_eq!(mac_key(0xad).unwrap(), (0x4a, 0), "mute");
        assert_eq!(mac_key(0xae).unwrap(), (0x49, 0), "volume down");
        assert_eq!(mac_key(0xaf).unwrap(), (0x48, 0), "volume up");
        for virtual_key in 0xb0..=0xb3 {
            assert!(matches!(
                mac_key(virtual_key),
                Err(PlatformError::Unsupported(_))
            ));
        }
    }

    #[test]
    fn keys_without_mac_equivalents_are_refused_instead_of_aliasing_function_keys() {
        for virtual_key in [0x2c, 0x91, 0x13] {
            assert!(matches!(
                mac_key(virtual_key),
                Err(PlatformError::Unsupported(_))
            ));
        }
        for virtual_key in 0x84..=0x87 {
            let Err(PlatformError::Unsupported(reason)) = mac_key(virtual_key) else {
                panic!("F21-F24 must be refused");
            };
            assert!(reason.contains("F21 through F24"), "{reason}");
        }
    }

    #[test]
    fn character_table_prefers_unmodified_outputs_and_uses_held_modifiers() {
        let mut table = CharacterTable::default();
        table.set(2, FLAG_SHIFT, 'a');
        table.set(30, 0, 'a');
        table.set(15, FLAG_SHIFT, '!');
        table.set(20, FLAG_OPTION, '!');
        table.set(5, FLAG_SHIFT | FLAG_OPTION, '€');

        assert_eq!(
            table.choose('a', 0),
            Some(CharacterChoice {
                keycode: 30,
                modifiers: 0,
            }),
            "unmodified mapping wins even when another key has a lower code"
        );
        assert_eq!(
            table.choose('!', FLAG_OPTION),
            Some(CharacterChoice {
                keycode: 20,
                modifiers: FLAG_OPTION,
            }),
            "when modifier counts tie, reuse a modifier already held"
        );
        let euro = table.choose('€', FLAG_SHIFT).unwrap();
        assert_eq!(euro.keycode, 5);
        assert_eq!(euro.modifiers & !FLAG_SHIFT, FLAG_OPTION);
        assert_eq!(table.choose('x', 0), None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn concurrent_keyboard_layout_reads_are_safe() {
        use std::sync::{Arc, Barrier};

        const THREADS: usize = 8;
        const ITERATIONS: usize = 10;

        let start = Arc::new(Barrier::new(THREADS));
        let workers: Vec<_> = (0..THREADS)
            .map(|_| {
                let start = Arc::clone(&start);
                std::thread::spawn(move || {
                    start.wait();
                    for _ in 0..ITERATIONS {
                        current_character_table().expect("Mac keyboard layout should be readable");
                    }
                })
            })
            .collect();

        for worker in workers {
            worker
                .join()
                .expect("keyboard layout reader should not abort");
        }
    }

    #[test]
    fn current_keyboard_layout_can_be_read_without_sending_input() {
        let table = current_character_table().expect("Mac keyboard layout should be readable");
        assert!(
            table
                .output
                .iter()
                .any(|row| row.iter().any(Option::is_some)),
            "the current layout should provide at least one character"
        );
    }

    #[test]
    fn releasing_one_side_does_not_clear_a_modifier_still_held_on_the_other_side() {
        let held = BTreeSet::from([0xa0, 0xa1, 0xa2, 0xa4, 0x5b]);
        assert_eq!(
            MacInjector::flags_for(&held, &[0; 2]),
            FLAG_SHIFT | FLAG_CONTROL | FLAG_OPTION | FLAG_COMMAND
        );
        let held = BTreeSet::from([0xa1, 0xa2, 0xa4, 0x5b]);
        assert_eq!(
            MacInjector::flags_for(&held, &[0; 2]),
            FLAG_SHIFT | FLAG_CONTROL | FLAG_OPTION | FLAG_COMMAND
        );
    }

    #[test]
    fn a_wheel_step_scrolls_at_least_a_line_in_its_direction() {
        assert_eq!(wheel_lines(120), 1);
        assert_eq!(wheel_lines(-120), -1);
        assert_eq!(wheel_lines(360), 3);
        assert_eq!(wheel_lines(40), 1);
        assert_eq!(wheel_lines(-40), -1);
        assert_eq!(wheel_lines(0), 0);
    }

    #[test]
    fn a_second_press_soon_and_near_makes_a_double_click() {
        let mut clicks = Clicks::default();
        let start = Instant::now();
        let here = CGPoint { x: 10.0, y: 10.0 };
        assert_eq!(clicks.press(Button::Left, start, here), 1);
        let soon = start + Duration::from_millis(100);
        assert_eq!(clicks.press(Button::Left, soon, here), 2);
        // Another button starts again.
        assert_eq!(clicks.press(Button::Right, soon, here), 1);
        // So does a press too late, or too far away.
        let later = soon + Duration::from_secs(2);
        assert_eq!(clicks.press(Button::Right, later, here), 1);
        let away = CGPoint { x: 100.0, y: 10.0 };
        assert_eq!(clicks.press(Button::Right, later, away), 1);
    }

    #[test]
    fn a_character_in_two_halves_is_sent_whole() {
        let mut surrogates = Surrogates::default();
        let units: Vec<u16> = "é😀".encode_utf16().collect();
        assert_eq!(units.len(), 3);
        assert_eq!(surrogates.unit(units[0], true), Some(vec![units[0]]));
        assert_eq!(surrogates.unit(units[0], false), Some(vec![units[0]]));
        assert_eq!(surrogates.unit(units[1], true), None);
        assert_eq!(surrogates.unit(units[1], false), None);
        let whole = Some(vec![units[1], units[2]]);
        assert_eq!(surrogates.unit(units[2], true), whole);
        assert_eq!(surrogates.unit(units[2], false), whole);
    }

    #[test]
    fn moving_with_a_button_held_is_dragging() {
        assert_eq!(move_event(None), (5, 0));
        assert_eq!(move_event(Some(Button::Left)), (6, 0));
        assert_eq!(move_event(Some(Button::Right)), (7, 1));
        assert_eq!(button_event(Button::Left, true), (1, 0));
        assert_eq!(button_event(Button::Middle, false), (26, 2));
    }
}
