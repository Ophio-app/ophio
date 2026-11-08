// SPDX-License-Identifier: Apache-2.0
//! The keyboard a Wayland virtual keyboard presents to the compositor.
//!
//! Compositors resolve shortcuts from raw keycodes through their own layout:
//! Hyprland, for one, looks a keybind's key up by keycode in its configured
//! layout, not in the virtual keyboard's keymap. So keys must arrive on the
//! keycodes the desktop's layout gives them, or Super+Return reaches the
//! compositor as Super+Escape. The virtual keyboard therefore uploads the
//! desktop's own layout and presses its real keys, with Shift where the layout
//! needs it. Only characters the layout cannot type get spare keycodes, which
//! no layout key and so no shortcut uses.
//!
//! This module is pure: it turns key requests into the requests to send, so it
//! can be tested without a compositor.
use ports::PlatformError;
use std::collections::{BTreeMap, BTreeSet};
use xkbcommon::xkb;

/// Names the keycodes section of every keymap Ophio uploads, so a keymap read
/// back from the seat can be recognised as Ophio's own and not the desktop's.
const MARKER: &str = "ophio-remote";
/// X clients, through Xwayland, cannot see higher keycodes.
const HIGHEST: u32 = 255;
const SHIFT_L: u32 = 0xffe1;
/// The key type of characters on spare and borrowed keys: one level that
/// takes Lock, so Caps Lock cannot capitalise them (é stays é), which an
/// ordinary one-level key would.
const EXACT: &str = "OPHIO_EXACT";
/// The same type for borrowed layout keys, under its own name, so a keymap
/// read back from the seat shows whether real keys were borrowed.
const BORROWED: &str = "OPHIO_BORROWED";
/// One level that takes Lock, under `name`.
fn exact_type(name: &str) -> String {
    format!(
        "\ttype \"{name}\" {{\n\t\tmodifiers= Lock;\n\t\tmap[Lock]= Level1;\n\t\tlevel_name[Level1]= \"Any\";\n\t}};\n"
    )
}

/// The last desktop layout read from a seat, for when the seat's current
/// keyboard is one of Ophio's own.
static DESKTOP_LAYOUT: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

pub(crate) fn is_ours(text: &str) -> bool {
    text.contains(&format!("xkb_keycodes \"{MARKER}\""))
}

/// The layout to start from, given what the seat reported, if anything.
pub(crate) fn desktop_layout(seat: Option<String>) -> Option<String> {
    let mut remembered = DESKTOP_LAYOUT
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(text) = seat.as_ref().filter(|text| !is_ours(text)) {
        *remembered = Some(text.clone());
    }
    choose_layout(seat, remembered.clone())
}
/// The seat's keymap unless it is Ophio's own; then the layout remembered
/// from before. With nothing remembered, as in a fresh process, Ophio's
/// keymap is still the desktop's layout only if it borrowed no real key (spare
/// keys only add): a borrowed key no longer types what the layout puts
/// there, so the system's default layout is better than that.
fn choose_layout(seat: Option<String>, remembered: Option<String>) -> Option<String> {
    match seat {
        Some(text) if !is_ours(&text) => Some(text),
        Some(text) if remembered.is_none() && !text.contains(BORROWED) => Some(text),
        _ => remembered,
    }
}

/// A request for the virtual keyboard, in order. Keycodes are XKB keycodes,
/// eight above the evdev codes the protocol carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Wire {
    Keymap(String),
    Key {
        code: u32,
        pressed: bool,
    },
    Modifiers {
        depressed: u32,
        latched: u32,
        locked: u32,
        group: u32,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Place {
    code: u32,
    /// Reached with Shift held.
    shift: bool,
    /// The layout's keysym there, which for a character may be a legacy
    /// keysym rather than the requested Unicode one.
    sym: u32,
}

#[derive(Debug, Clone, Copy)]
struct Held {
    code: u32,
    /// Shift was pressed for this key alone.
    shift: bool,
}

pub(crate) struct Keyboard {
    context: xkb::Context,
    base: String,
    keymap: xkb::Keymap,
    /// What the compositor holds for this keyboard, mirrored.
    state: xkb::State,
    keys: BTreeMap<u32, Vec<Place>>,
    chars: BTreeMap<char, Vec<Place>>,
    shift: Option<u32>,
    shift_mask: u32,
    /// Keycodes no layout key uses, best first, and the names some of them
    /// already have in the layout.
    spare: Vec<u32>,
    /// Layout keys that type a plain character, which typed text may borrow
    /// for a character the layout lacks: some apps, Chromium among them,
    /// ignore keycodes no real key has, and with no modifier held such a key
    /// can trigger no more shortcuts than typing its own character does.
    borrowable: Vec<u32>,
    named: BTreeMap<u32, String>,
    max_keycode: u32,
    /// Characters on spare keycodes, least recently used first.
    extras: Vec<(u32, u32)>,
    held: BTreeMap<u32, Held>,
    /// Keycodes down, with how many held keys share each, in press order.
    down: Vec<(u32, u32)>,
}
// SAFETY: every libxkbcommon object here (the context, the keymap and the
// state referring to it) is owned by this value alone and never handed out,
// so their non-atomic reference counts are only touched by its owner.
// libxkbcommon keeps no thread-local state. It is not Sync, so no two
// threads can reach these objects at once.
#[allow(unsafe_code)]
unsafe impl Send for Keyboard {}

fn unavailable() -> PlatformError {
    PlatformError::Unsupported("The desktop keyboard layout could not be read".into())
}

fn compile(context: &xkb::Context, text: &str) -> Result<xkb::Keymap, PlatformError> {
    xkb::Keymap::new_from_string(
        context,
        text.to_owned(),
        xkb::KEYMAP_FORMAT_TEXT_V1,
        xkb::KEYMAP_COMPILE_NO_FLAGS,
    )
    .ok_or_else(unavailable)
}

/// Where a section starting with `header` begins, at the start of a line.
fn section(text: &str, header: &str) -> Option<usize> {
    if text.starts_with(header) {
        return Some(0);
    }
    text.find(&format!("\n{header}")).map(|at| at + 1)
}

/// Insert `lines` just before the closing line of the `header` section.
fn insert_before_end(text: &mut String, header: &str, lines: &str) -> Result<(), PlatformError> {
    if lines.is_empty() {
        return Ok(());
    }
    let start = section(text, header).ok_or_else(unavailable)?;
    let end = text[start..]
        .find("\n};")
        .map(|at| start + at + 1)
        .ok_or_else(unavailable)?;
    text.insert_str(end, lines);
    Ok(())
}

fn character(sym: u32) -> Option<char> {
    let keysym = xkb::Keysym::new(sym);
    if keysym.is_keypad_key() {
        return None;
    }
    char::from_u32(xkb::keysym_to_utf32(keysym)).filter(|c| !c.is_control())
}

impl Keyboard {
    /// Start from the desktop's layout text, or the system default layout
    /// when there is none or it does not compile.
    pub fn new(source: Option<&str>) -> Result<(Self, Vec<Wire>), PlatformError> {
        let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        let desktop = source
            .and_then(|text| compile(&context, text.trim_end_matches('\0')).ok())
            .or_else(|| {
                xkb::Keymap::new_from_names(
                    &context,
                    "",
                    "",
                    "",
                    "",
                    None,
                    xkb::KEYMAP_COMPILE_NO_FLAGS,
                )
            })
            .ok_or_else(unavailable)?;
        let mut base = desktop.get_as_string(xkb::KEYMAP_FORMAT_TEXT_V1);
        let start = section(&base, "xkb_keycodes").ok_or_else(unavailable)?;
        let end = base[start..]
            .find('\n')
            .map(|at| start + at)
            .ok_or_else(unavailable)?;
        base.replace_range(start..end, &format!("xkb_keycodes \"{MARKER}\" {{"));
        let keymap = compile(&context, &base)?;
        let state = xkb::State::new(&keymap);
        let mut keyboard = Self {
            context,
            base,
            keymap,
            state,
            keys: BTreeMap::new(),
            chars: BTreeMap::new(),
            shift: None,
            shift_mask: 0,
            spare: Vec::new(),
            borrowable: Vec::new(),
            named: BTreeMap::new(),
            max_keycode: 0,
            extras: Vec::new(),
            held: BTreeMap::new(),
            down: Vec::new(),
        };
        keyboard.index();
        let text = keyboard.base.clone();
        let modifiers = keyboard.modifiers();
        Ok((keyboard, vec![Wire::Keymap(text), modifiers]))
    }

    fn index(&mut self) {
        let keymap = &self.keymap;
        let shift_index = keymap.mod_get_index(xkb::MOD_NAME_SHIFT);
        self.shift_mask = if shift_index < 32 {
            1 << shift_index
        } else {
            0
        };
        let plain = xkb::State::new(keymap);
        let mut shifted = xkb::State::new(keymap);
        shifted.update_mask(self.shift_mask, 0, 0, 0, 0, 0);
        let (min, max) = (keymap.min_keycode().raw(), keymap.max_keycode().raw());
        self.max_keycode = max;
        for code in min.max(9)..=max.min(HIGHEST) {
            let key = xkb::Keycode::new(code);
            if keymap.num_layouts_for_key(key) == 0 {
                continue;
            }
            let base = plain.key_get_one_sym(key).raw();
            let upper = shifted.key_get_one_sym(key).raw();
            if character(base).is_some_and(|c| c != ' ')
                && let Some(name) = keymap.key_get_name(key)
            {
                self.borrowable.push(code);
                self.named.insert(code, name.to_owned());
            }
            for (sym, shift) in [(base, false), (upper, true)] {
                if sym == 0 || (shift && sym == base) {
                    continue;
                }
                let place = Place { code, shift, sym };
                self.keys.entry(sym).or_default().push(place);
                if let Some(c) = character(sym) {
                    self.chars.entry(c).or_default().push(place);
                }
            }
        }
        // Unshifted first, then the lowest keycode: Super_L on <LWIN>, not
        // on the shifted level of a spare key.
        for places in self.keys.values_mut().chain(self.chars.values_mut()) {
            places.sort_by_key(|p| (p.shift, p.code));
        }
        self.shift = self
            .keys
            .get(&SHIFT_L)
            .and_then(|places| places.iter().find(|p| !p.shift))
            .map(|p| p.code);
        self.borrowable.reverse();
        for code in (9..=HIGHEST).rev() {
            let in_range = (min..=max).contains(&code);
            if in_range && keymap.num_layouts_for_key(xkb::Keycode::new(code)) > 0 {
                continue;
            }
            self.spare.push(code);
            if in_range && let Some(name) = keymap.key_get_name(xkb::Keycode::new(code)) {
                self.named.insert(code, name.to_owned());
            }
        }
    }

    pub fn modifiers(&self) -> Wire {
        Wire::Modifiers {
            depressed: self.state.serialize_mods(xkb::STATE_MODS_DEPRESSED),
            latched: self.state.serialize_mods(xkb::STATE_MODS_LATCHED),
            locked: self.state.serialize_mods(xkb::STATE_MODS_LOCKED),
            group: self.state.serialize_layout(xkb::STATE_LAYOUT_EFFECTIVE),
        }
    }

    fn places(&self, keysym: u32) -> Vec<Place> {
        let mut places = self.keys.get(&keysym).cloned().unwrap_or_default();
        if let Some(c) = character(keysym)
            && let Some(more) = self.chars.get(&c)
        {
            let more: Vec<Place> = more
                .iter()
                .filter(|p| !places.contains(p))
                .copied()
                .collect();
            places.extend(more);
        }
        places
    }

    fn shift_active(&self) -> bool {
        self.state.serialize_mods(xkb::STATE_MODS_EFFECTIVE) & self.shift_mask != 0
    }

    /// The layout key for `keysym`, and whether Shift must be added for it.
    ///
    /// Text must produce exactly its character in the current state. A named
    /// key is the layout's key for that symbol, as on a real keyboard: with
    /// Shift held, "1" is still the 1 key and a shortcut sees Shift+1.
    fn route(&self, keysym: u32, text: bool) -> Option<(u32, bool)> {
        let places = self.places(keysym);
        let shift_active = self.shift_active();
        if !text {
            let place = places.first()?;
            if place.shift && !shift_active {
                return self.shift.map(|_| (place.code, true));
            }
            return Some((place.code, false));
        }
        if let Some(place) = places
            .iter()
            .find(|p| self.state.key_get_one_sym(xkb::Keycode::new(p.code)).raw() == p.sym)
        {
            return Some((place.code, false));
        }
        if shift_active || self.shift.is_none() {
            return None;
        }
        let mut probe = xkb::State::new(&self.keymap);
        probe.update_mask(
            self.state.serialize_mods(xkb::STATE_MODS_DEPRESSED) | self.shift_mask,
            self.state.serialize_mods(xkb::STATE_MODS_LATCHED),
            self.state.serialize_mods(xkb::STATE_MODS_LOCKED),
            self.state.serialize_layout(xkb::STATE_LAYOUT_DEPRESSED),
            self.state.serialize_layout(xkb::STATE_LAYOUT_LATCHED),
            self.state.serialize_layout(xkb::STATE_LAYOUT_LOCKED),
        );
        places
            .iter()
            .find(|p| probe.key_get_one_sym(xkb::Keycode::new(p.code)).raw() == p.sym)
            .map(|p| (p.code, true))
    }

    /// A keycode for `keysym`, which the layout lacks, and whether the
    /// keymap must change. Text borrows plain character keys not in
    /// `keep`, then spare keycodes; a shortcut only uses spare keycodes, since
    /// a borrowed key with a modifier held would be that key's shortcut.
    fn extra(
        &mut self,
        keysym: u32,
        text: bool,
        keep: &BTreeSet<u32>,
    ) -> Result<(u32, bool), PlatformError> {
        if let Some(at) = self.extras.iter().position(|(sym, _)| *sym == keysym)
            && (text || !self.borrowable.contains(&self.extras[at].1))
        {
            let extra = self.extras.remove(at);
            self.extras.push(extra);
            return Ok((extra.1, false));
        }
        let pool: Vec<u32> = if text {
            self.borrowable.iter().chain(&self.spare).copied().collect()
        } else {
            self.spare.clone()
        };
        let usable = |code: &u32| !keep.contains(code) && !self.is_down(*code);
        let used: BTreeSet<u32> = self.extras.iter().map(|(_, code)| *code).collect();
        let code = match pool
            .iter()
            .find(|code| !used.contains(code) && usable(code))
        {
            Some(code) => *code,
            None => {
                let at = self
                    .extras
                    .iter()
                    .position(|(_, code)| pool.contains(code) && usable(code))
                    .ok_or_else(|| {
                        PlatformError::Unsupported(
                            "Too many different special characters are held at once".into(),
                        )
                    })?;
                self.extras.remove(at).1
            }
        };
        self.extras.retain(|(sym, _)| *sym != keysym);
        self.extras.push((keysym, code));
        Ok((code, true))
    }

    /// Give back borrowed keys that type `keysym` themselves. Whether the
    /// keymap changed.
    fn reclaim(&mut self, keysym: u32) -> bool {
        let codes: BTreeSet<u32> = self.places(keysym).iter().map(|p| p.code).collect();
        let before = self.extras.len();
        let down: BTreeSet<u32> = self.down.iter().map(|(code, _)| *code).collect();
        self.extras
            .retain(|(_, code)| !codes.contains(code) || down.contains(code));
        self.extras.len() != before
    }

    fn is_down(&self, code: u32) -> bool {
        self.down.iter().any(|(c, _)| *c == code)
    }

    /// The layout with every current extra, compiled and made current. The
    /// mirrored state is rebuilt with the same keys down and locks set, as the
    /// compositor's will be once the modifiers that follow arrive.
    fn render(&mut self) -> Result<Wire, PlatformError> {
        let mut text = self.base.clone();
        let mut names = String::new();
        let mut symbols = String::new();
        let mut highest = 0;
        for (sym, code) in &self.extras {
            let name = match self.named.get(code) {
                Some(name) => name.clone(),
                None => {
                    let name = format!("O{code:03}");
                    names.push_str(&format!("\t<{name}> = {code};\n"));
                    name
                }
            };
            // A borrowed key is replaced whole, so Shift cannot change it.
            let (merge, kind) = if self.borrowable.contains(code) {
                ("replace ", BORROWED)
            } else {
                ("", EXACT)
            };
            symbols.push_str(&format!(
                "\t{merge}key <{name}> {{ type=\"{kind}\", [ 0x{sym:x} ] }};\n"
            ));
            highest = highest.max(*code);
        }
        if highest > self.max_keycode {
            let start = section(&text, "xkb_keycodes").ok_or_else(unavailable)?;
            let at = text[start..]
                .find("maximum = ")
                .map(|at| start + at + "maximum = ".len())
                .ok_or_else(unavailable)?;
            let end = text[at..]
                .find(';')
                .map(|end| at + end)
                .ok_or_else(unavailable)?;
            text.replace_range(at..end, &HIGHEST.to_string());
        }
        insert_before_end(&mut text, "xkb_keycodes", &names)?;
        // Only the types in use, so a keymap names borrowing only if it
        // borrowed.
        let mut types = String::new();
        for kind in [EXACT, BORROWED] {
            if symbols.contains(&format!("type=\"{kind}\"")) {
                types.push_str(&exact_type(kind));
            }
        }
        insert_before_end(&mut text, "xkb_types", &types)?;
        insert_before_end(&mut text, "xkb_symbols", &symbols)?;
        let keymap = compile(&self.context, &text)?;
        let mut state = xkb::State::new(&keymap);
        for (code, _) in &self.down {
            state.update_key(xkb::Keycode::new(*code), xkb::KeyDirection::Down);
        }
        state.update_mask(
            state.serialize_mods(xkb::STATE_MODS_DEPRESSED),
            state.serialize_mods(xkb::STATE_MODS_LATCHED),
            self.state.serialize_mods(xkb::STATE_MODS_LOCKED),
            state.serialize_layout(xkb::STATE_LAYOUT_DEPRESSED),
            state.serialize_layout(xkb::STATE_LAYOUT_LATCHED),
            self.state.serialize_layout(xkb::STATE_LAYOUT_LOCKED),
        );
        self.keymap = keymap;
        self.state = state;
        Ok(Wire::Keymap(text))
    }

    /// Press or release one keycode, sharing it between held keys.
    fn code(&mut self, code: u32, pressed: bool, wire: &mut Vec<Wire>) {
        let at = self.down.iter().position(|(c, _)| *c == code);
        match (pressed, at) {
            (true, Some(at)) => {
                self.down[at].1 += 1;
                return;
            }
            (true, None) => self.down.push((code, 1)),
            (false, None) => return,
            (false, Some(at)) => {
                self.down[at].1 -= 1;
                if self.down[at].1 > 0 {
                    return;
                }
                self.down.remove(at);
            }
        }
        let before = self.modifiers();
        wire.push(Wire::Key { code, pressed });
        self.state.update_key(
            xkb::Keycode::new(code),
            if pressed {
                xkb::KeyDirection::Down
            } else {
                xkb::KeyDirection::Up
            },
        );
        let after = self.modifiers();
        if after != before {
            wire.push(after);
        }
    }

    /// Press or release the key for `keysym`. `text` asks for exactly that
    /// character rather than the key named by it.
    pub fn key(
        &mut self,
        keysym: u32,
        pressed: bool,
        text: bool,
    ) -> Result<Vec<Wire>, PlatformError> {
        let mut wire = Vec::new();
        if !pressed {
            if let Some(held) = self.held.remove(&keysym) {
                self.code(held.code, false, &mut wire);
                if held.shift
                    && let Some(shift) = self.shift
                {
                    self.code(shift, false, &mut wire);
                }
            }
            return Ok(wire);
        }
        if self.held.contains_key(&keysym) {
            return Ok(wire);
        }
        if self.reclaim(keysym) {
            wire.push(self.render()?);
            wire.push(self.modifiers());
        }
        let (code, shift) = match self.route(keysym, text) {
            Some(route) => route,
            None => {
                let (code, changed) = self.extra(keysym, text, &BTreeSet::new())?;
                if changed {
                    wire.push(self.render()?);
                    wire.push(self.modifiers());
                }
                (code, false)
            }
        };
        if shift && let Some(shift) = self.shift {
            self.code(shift, true, &mut wire);
        }
        self.code(code, true, &mut wire);
        self.held.insert(keysym, Held { code, shift });
        Ok(wire)
    }

    /// Give every character of a text that the layout cannot type a key up
    /// front, so the keymap changes at most once for it. Keys the text types
    /// itself are given back and never borrowed for it.
    pub fn prepare_text(&mut self, keysyms: &[u32]) -> Result<Vec<Wire>, PlatformError> {
        let mut changed = false;
        for sym in keysyms {
            changed |= self.reclaim(*sym);
        }
        if changed {
            // Only the final keymap is sent; this one just updates the state.
            self.render()?;
        }
        let keep: BTreeSet<u32> = keysyms
            .iter()
            .flat_map(|sym| self.places(*sym))
            .map(|p| p.code)
            .collect();
        let mut needed = Vec::new();
        for sym in keysyms {
            if !needed.contains(sym) && self.route(*sym, true).is_none() {
                needed.push(*sym);
            }
        }
        let room = self
            .borrowable
            .iter()
            .chain(&self.spare)
            .filter(|code| !keep.contains(code))
            .count();
        for sym in needed.into_iter().take(room) {
            changed |= self.extra(sym, true, &keep)?.1;
        }
        Ok(if changed {
            vec![self.render()?, self.modifiers()]
        } else {
            Vec::new()
        })
    }

    /// Release everything held, last pressed first.
    pub fn release_all(&mut self) -> Vec<Wire> {
        let mut wire = Vec::new();
        self.held.clear();
        while let Some((code, _)) = self.down.last().copied() {
            if let Some(last) = self.down.last_mut() {
                last.1 = 1;
            }
            self.code(code, false, &mut wire);
        }
        wire
    }

    /// XKB keycodes currently down.
    #[cfg(test)]
    pub fn down(&self) -> Vec<u32> {
        self.down.iter().map(|(code, _)| *code).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ESC: u32 = 9;
    const RETURN: u32 = 36;
    const LWIN: u32 = 133;
    const LFSH: u32 = 50;
    const LCTL: u32 = 37;
    const LALT: u32 = 64;
    const AC01: u32 = 38; // a
    const AE01: u32 = 10; // 1
    const TAB: u32 = 23;

    fn us() -> String {
        let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        xkb::Keymap::new_from_names(&context, "evdev", "pc105", "us", "", None, 0)
            .expect("xkeyboard-config provides us")
            .get_as_string(xkb::KEYMAP_FORMAT_TEXT_V1)
    }
    fn layout(name: &str) -> String {
        let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        xkb::Keymap::new_from_names(&context, "evdev", "pc105", name, "", None, 0)
            .expect("xkeyboard-config provides the layout")
            .get_as_string(xkb::KEYMAP_FORMAT_TEXT_V1)
    }
    fn keys(wire: &[Wire]) -> Vec<(u32, bool)> {
        wire.iter()
            .filter_map(|w| match w {
                Wire::Key { code, pressed } => Some((*code, *pressed)),
                _ => None,
            })
            .collect()
    }
    fn depressed(wire: &[Wire]) -> Option<u32> {
        wire.iter().rev().find_map(|w| match w {
            Wire::Modifiers { depressed, .. } => Some(*depressed),
            _ => None,
        })
    }
    fn chord(keyboard: &mut Keyboard, syms: &[u32]) -> Vec<Wire> {
        let mut wire = Vec::new();
        for sym in syms {
            wire.extend(keyboard.key(*sym, true, false).unwrap());
        }
        for sym in syms.iter().rev() {
            wire.extend(keyboard.key(*sym, false, false).unwrap());
        }
        wire
    }
    /// What a client would read from these requests: the keysym and text of
    /// each press, through the keymap and modifiers it was sent.
    fn typed(wire: &[Wire]) -> String {
        let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        let mut keymap = None;
        let mut state: Option<xkb::State> = None;
        let mut out = String::new();
        for w in wire {
            match w {
                Wire::Keymap(text) => {
                    let map = compile(&context, text).unwrap();
                    state = Some(xkb::State::new(&map));
                    keymap = Some(map);
                }
                Wire::Modifiers {
                    depressed,
                    latched,
                    locked,
                    group,
                } => {
                    state
                        .as_mut()
                        .unwrap()
                        .update_mask(*depressed, *latched, *locked, 0, 0, *group);
                }
                Wire::Key {
                    code,
                    pressed: true,
                } => {
                    out.push_str(
                        &state
                            .as_ref()
                            .unwrap()
                            .key_get_utf8(xkb::Keycode::new(*code)),
                    );
                }
                Wire::Key { .. } => {}
            }
        }
        let _ = keymap;
        out
    }

    #[test]
    fn named_keys_use_the_layouts_own_keycodes_and_never_keycode_eight() {
        let (mut keyboard, start) = Keyboard::new(Some(&us())).unwrap();
        assert!(matches!(&start[0], Wire::Keymap(text) if is_ours(text)));
        let wire = chord(&mut keyboard, &[0xffeb, 0xff0d]);
        // Super_L on <LWIN>, Return on <RTRN>: Super+Return, not Super+Escape.
        assert_eq!(
            keys(&wire),
            [(LWIN, true), (RETURN, true), (RETURN, false), (LWIN, false)]
        );
        assert!(
            !keys(&wire)
                .iter()
                .any(|(code, _)| *code == ESC || *code == 8)
        );
        let wire = chord(&mut keyboard, &[0xff1b]);
        assert_eq!(keys(&wire), [(ESC, true), (ESC, false)]);
        for (sym, code) in [
            (0xffe3, LCTL),
            (0xffe9, LALT),
            (0xffe1, LFSH),
            (0xff09, TAB),
        ] {
            assert_eq!(keys(&chord(&mut keyboard, &[sym]))[0], (code, true));
        }
        // F1..F12 are 67..76, 95, 96 in evdev.
        assert_eq!(keys(&chord(&mut keyboard, &[0xffbe]))[0], (67, true));
        assert_eq!(keys(&chord(&mut keyboard, &[0xffc9]))[0], (96, true));
    }

    #[test]
    fn modifiers_follow_the_keys_and_clear_on_release() {
        let (mut keyboard, _) = Keyboard::new(Some(&us())).unwrap();
        let wire = keyboard.key(0xffeb, true, false).unwrap();
        // Mod4 is the sixth real modifier.
        assert_eq!(depressed(&wire), Some(1 << 6));
        let wire = keyboard.key(0xffe1, true, false).unwrap();
        assert_eq!(depressed(&wire), Some(1 << 6 | 1));
        let wire = keyboard.key(u32::from('t'), true, false).unwrap();
        // Super+Shift+T is the T key with both held, no extra Shift.
        assert_eq!(keys(&wire), [(28, true)]);
        let mut wire = keyboard.key(u32::from('t'), false, false).unwrap();
        wire.extend(keyboard.key(0xffe1, false, false).unwrap());
        wire.extend(keyboard.key(0xffeb, false, false).unwrap());
        assert_eq!(depressed(&wire), Some(0));
        assert!(keyboard.down().is_empty());
    }

    #[test]
    fn shifted_symbols_in_shortcuts_add_shift_only_when_it_is_not_held() {
        let (mut keyboard, _) = Keyboard::new(Some(&us())).unwrap();
        // "ctrl+T" means Ctrl+Shift+T, as xdotool has it.
        let wire = chord(&mut keyboard, &[0xffe3, u32::from('T')]);
        assert_eq!(
            keys(&wire),
            [
                (LCTL, true),
                (LFSH, true),
                (28, true),
                (28, false),
                (LFSH, false),
                (LCTL, false)
            ]
        );
        // With Shift held, "1" is still the 1 key: Super+Shift+1.
        let wire = chord(&mut keyboard, &[0xffeb, 0xffe1, u32::from('1')]);
        assert_eq!(keys(&wire)[2], (AE01, true));
        assert_eq!(keys(&wire).len(), 6);
        assert!(keyboard.down().is_empty());
    }

    #[test]
    fn text_arrives_exactly_including_uppercase_punctuation_and_unicode() {
        let (mut keyboard, start) = Keyboard::new(Some(&us())).unwrap();
        let text = "Hello, World! ~{}|\"<>? éλ🦀 aA\tz\n";
        let syms: Vec<u32> = text
            .chars()
            .map(|c| crate::input::character_keysym(c).unwrap())
            .collect();
        let mut wire = start;
        wire.extend(keyboard.prepare_text(&syms).unwrap());
        let uploads = wire.iter().filter(|w| matches!(w, Wire::Keymap(_))).count();
        for sym in &syms {
            wire.extend(keyboard.key(*sym, true, true).unwrap());
            wire.extend(keyboard.key(*sym, false, true).unwrap());
        }
        // One upload to start and one for the three extras, none per key.
        assert_eq!(
            uploads,
            wire.iter().filter(|w| matches!(w, Wire::Keymap(_))).count()
        );
        assert_eq!(uploads, 2);
        assert_eq!(typed(&wire), text.replace('\n', "\r"));
        assert!(keyboard.down().is_empty());
        // Letters use their own keys, Shift for capitals.
        let wire = keyboard.key(u32::from('A'), true, true).unwrap();
        assert_eq!(keys(&wire), [(LFSH, true), (AC01, true)]);
        let wire = keyboard.key(u32::from('A'), false, true).unwrap();
        assert_eq!(keys(&wire), [(AC01, false), (LFSH, false)]);
        assert_eq!(depressed(&wire), Some(0));
    }

    #[test]
    fn spare_keycodes_have_no_layout_symbol_and_stay_within_x11_range() {
        let (mut keyboard, _) = Keyboard::new(Some(&us())).unwrap();
        let base = compile(&keyboard.context, &keyboard.base).unwrap();
        // A US pc105 layout leaves fourteen.
        assert!(keyboard.spare.len() >= 8, "{} spare", keyboard.spare.len());
        for code in &keyboard.spare {
            assert!((9..=255).contains(code));
            assert_eq!(base.num_layouts_for_key(xkb::Keycode::new(*code)), 0);
        }
        // In a shortcut: a spare keycode.
        let wire = keyboard.key(0x0100_263a, true, false).unwrap();
        let code = keys(&wire)[0].0;
        assert!(keyboard.spare.contains(&code));
        // The desktop layout itself gives that keycode no symbol, so no
        // shortcut can be bound to it.
        let desktop = xkb::State::new(&base);
        assert_eq!(desktop.key_get_one_sym(xkb::Keycode::new(code)).raw(), 0);
    }

    #[test]
    fn text_borrows_plain_keys_it_does_not_type_and_gives_them_back() {
        let (mut keyboard, start) = Keyboard::new(Some(&us())).unwrap();
        let mut wire = start;
        let text = "é🦀 cafe";
        let syms: Vec<u32> = text
            .chars()
            .map(|c| crate::input::character_keysym(c).unwrap())
            .collect();
        wire.extend(keyboard.prepare_text(&syms).unwrap());
        let mut typed_codes = Vec::new();
        for sym in &syms {
            let pressed = keyboard.key(*sym, true, true).unwrap();
            typed_codes.extend(keys(&pressed));
            wire.extend(pressed);
            wire.extend(keyboard.key(*sym, false, true).unwrap());
        }
        assert_eq!(typed(&wire), text);
        // é and 🦀 went on keys that type a plain character in the layout, so
        // every app sees a real key, and not on c, a, f or e, which the text
        // types itself.
        let (acute, crab) = (typed_codes[0].0, typed_codes[1].0);
        for code in [acute, crab] {
            assert!(keyboard.borrowable.contains(&code));
            assert!(![54, 38, 41, 26].contains(&code));
        }
        // Typing the borrowed key's own character gives it back first.
        let base = compile(&keyboard.context, &keyboard.base).unwrap();
        let own = xkb::State::new(&base)
            .key_get_one_sym(xkb::Keycode::new(acute))
            .raw();
        let mut more = keyboard.key(own, true, true).unwrap();
        more.extend(keyboard.key(own, false, true).unwrap());
        assert!(matches!(more[0], Wire::Keymap(_)));
        assert_eq!(keys(&more), [(acute, true), (acute, false)]);
        wire.extend(more);
        assert!(typed(&wire).ends_with(char::from_u32(own).unwrap()));
        // A shortcut never uses a borrowed key: with Ctrl held it would be
        // that key's own shortcut.
        let shortcut = chord(&mut keyboard, &[0xffe3, 0x0100_263a]);
        assert!(keyboard.spare.contains(&keys(&shortcut)[1].0));
    }

    #[test]
    fn many_distinct_characters_reuse_spare_keys_without_dropping_held_ones() {
        let (mut keyboard, _) = Keyboard::new(Some(&us())).unwrap();
        let capacity = keyboard.spare.len();
        let text: String = (0..capacity as u32 * 2 + 3)
            .map(|i| char::from_u32(0x4e00 + i).unwrap())
            .collect();
        let syms: Vec<u32> = text
            .chars()
            .map(|c| crate::input::character_keysym(c).unwrap())
            .collect();
        let mut wire = keyboard.prepare_text(&syms).unwrap();
        for sym in &syms {
            wire.extend(keyboard.key(*sym, true, true).unwrap());
            wire.extend(keyboard.key(*sym, false, true).unwrap());
        }
        let (_, start) = Keyboard::new(Some(&us())).unwrap();
        let mut all = start;
        all.extend(wire);
        assert_eq!(typed(&all), text);
        // Every spare key held at once: one more cannot be typed.
        let (mut keyboard, _) = Keyboard::new(Some(&us())).unwrap();
        for sym in &syms[..capacity] {
            keyboard.key(*sym, true, false).unwrap();
        }
        assert!(keyboard.key(syms[capacity], true, false).is_err());
        let released = keyboard.release_all();
        assert_eq!(keys(&released).len(), capacity);
        assert!(keyboard.down().is_empty());
    }

    #[test]
    fn other_layouts_keep_their_own_keys() {
        // On a German layout z and y swap places and @ needs AltGr.
        let (mut keyboard, start) = Keyboard::new(Some(&layout("de"))).unwrap();
        assert_eq!(
            keys(&chord(&mut keyboard, &[u32::from('z')]))[0],
            (29, true)
        );
        assert_eq!(
            keys(&chord(&mut keyboard, &[u32::from('y')]))[0],
            (52, true)
        );
        let mut wire = start;
        for c in "zy@ÄÖ".chars() {
            let sym = crate::input::character_keysym(c).unwrap();
            wire.extend(keyboard.key(sym, true, true).unwrap());
            wire.extend(keyboard.key(sym, false, true).unwrap());
        }
        assert_eq!(typed(&wire), "zy@ÄÖ");
        // A Greek layout types λ from its legacy keysym, on the L key.
        let (mut keyboard, _) = Keyboard::new(Some(&layout("gr"))).unwrap();
        let wire = keyboard.key(0x0100_03bb, true, true).unwrap();
        assert_eq!(keys(&wire), [(46, true)]);
    }

    #[test]
    fn caps_lock_does_not_change_typed_text() {
        let (mut keyboard, start) = Keyboard::new(Some(&us())).unwrap();
        let mut wire = start;
        wire.extend(chord(&mut keyboard, &[0xffe5]));
        for c in "aB".chars() {
            let sym = u32::from(c);
            wire.extend(keyboard.key(sym, true, true).unwrap());
            wire.extend(keyboard.key(sym, false, true).unwrap());
        }
        assert_eq!(typed(&wire), "aB");
    }

    #[test]
    fn caps_lock_does_not_change_characters_on_spare_or_borrowed_keys() {
        let (mut keyboard, start) = Keyboard::new(Some(&us())).unwrap();
        let mut wire = start;
        wire.extend(chord(&mut keyboard, &[0xffe5]));
        // Typed text borrows a layout key; a shortcut uses a spare keycode.
        for c in "éλ".chars() {
            let sym = crate::input::character_keysym(c).unwrap();
            wire.extend(keyboard.key(sym, true, true).unwrap());
            wire.extend(keyboard.key(sym, false, true).unwrap());
        }
        let sym = crate::input::character_keysym('ß').unwrap();
        wire.extend(keyboard.key(sym, true, false).unwrap());
        wire.extend(keyboard.key(sym, false, false).unwrap());
        assert_eq!(typed(&wire), "éλß");
    }

    #[test]
    fn a_fresh_process_never_takes_borrowed_keys_for_the_layout() {
        // An Ophio keyboard that borrowed the q key for é is on the seat, and
        // this process remembers no desktop layout.
        let (mut keyboard, _) = Keyboard::new(Some(&us())).unwrap();
        let wire = keyboard.prepare_text(&[0xe9]).unwrap();
        let Some(Wire::Keymap(borrowed)) = wire.first().cloned() else {
            panic!("a keymap with é")
        };
        let code = keyboard.extras[0].1;
        let base = compile(&keyboard.context, &keyboard.base).unwrap();
        let own = xkb::State::new(&base)
            .key_get_one_sym(xkb::Keycode::new(code))
            .raw();
        let chosen = choose_layout(Some(borrowed), None);
        let (mut fresh, _) = Keyboard::new(chosen.as_deref()).unwrap();
        assert!(
            !fresh.base.contains(BORROWED),
            "a borrowed key became the layout"
        );
        // The borrowed key types its own character again, with no extra key.
        let wire = fresh.key(own, true, true).unwrap();
        assert_eq!(
            wire,
            [Wire::Key {
                code,
                pressed: true
            }]
        );
        // One that only gained a spare key, for a shortcut, is too.
        let (mut spare, _) = Keyboard::new(Some(&layout("de"))).unwrap();
        let wire = spare.key(0x0101_f980, true, false).unwrap();
        let Some(Wire::Keymap(extra)) = wire.first().cloned() else {
            panic!("a keymap with the crab")
        };
        assert_eq!(choose_layout(Some(extra.clone()), None), Some(extra));
        // An Ophio keymap that borrowed nothing is still the desktop's.
        let (_, start) = Keyboard::new(Some(&layout("de"))).unwrap();
        let Wire::Keymap(plain) = &start[0] else {
            panic!()
        };
        assert_eq!(
            choose_layout(Some(plain.clone()), None).as_deref(),
            Some(plain.as_str())
        );
    }

    #[test]
    fn the_seats_own_ophio_keymap_is_not_taken_for_the_desktops() {
        let desktop = us();
        assert!(!is_ours(&desktop));
        assert_eq!(desktop_layout(Some(desktop.clone())), Some(desktop.clone()));
        let (_, start) = Keyboard::new(Some(&desktop)).unwrap();
        let Wire::Keymap(ours) = &start[0] else {
            panic!("keymap first")
        };
        assert!(is_ours(ours));
        assert_eq!(desktop_layout(Some(ours.clone())), Some(desktop));
        // Nothing usable at all still gives the system's default layout.
        let (mut keyboard, _) = Keyboard::new(Some("not a keymap")).unwrap();
        assert_eq!(keys(&chord(&mut keyboard, &[0xff0d]))[0], (RETURN, true));
    }

    #[test]
    fn a_key_held_twice_or_released_unheld_sends_nothing_extra() {
        let (mut keyboard, _) = Keyboard::new(Some(&us())).unwrap();
        assert_eq!(keys(&keyboard.key(0xffe3, true, false).unwrap()).len(), 1);
        assert!(keyboard.key(0xffe3, true, false).unwrap().is_empty());
        assert!(keyboard.key(0xff0d, false, false).unwrap().is_empty());
        // 'a' and 'A' share the A key; it goes up once both are released.
        keyboard.key(u32::from('a'), true, false).unwrap();
        let wire = keyboard.key(u32::from('A'), true, false).unwrap();
        assert_eq!(keys(&wire), [(LFSH, true)]);
        assert!(keys(&keyboard.key(u32::from('a'), false, false).unwrap()).is_empty());
        let wire = keyboard.key(u32::from('A'), false, false).unwrap();
        assert_eq!(keys(&wire), [(AC01, false), (LFSH, false)]);
        let wire = keyboard.release_all();
        assert_eq!(keys(&wire), [(LCTL, false)]);
        assert_eq!(depressed(&wire), Some(0));
    }
}
