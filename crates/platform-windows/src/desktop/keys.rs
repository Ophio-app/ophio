// SPDX-License-Identifier: Apache-2.0
//! How Windows' own keyboard calls are read and answered when a key or
//! character is typed. Kept apart from the native code so it is testable on
//! every host.

/// VkKeyScanExW's modifier mask, reduced to the keys this injector must
/// add. Explicitly held left or right modifiers, and modifiers already added
/// for another character, satisfy the requirement without another key-down.
#[cfg_attr(not(windows), allow(dead_code))]
pub(super) fn character_modifiers(mask: u16, held: impl Fn(u16) -> bool) -> Option<Vec<u16>> {
    if mask & !7 != 0 {
        return None;
    }
    Some(
        [(2, 0xa2, 0xa3), (4, 0xa4, 0xa5), (1, 0xa0, 0xa1)]
            .into_iter()
            .filter(|(bit, _, _)| mask & bit != 0)
            .filter(|(_, left, right)| !held(*left) && !held(*right))
            .map(|(_, left, _)| left)
            .collect(),
    )
}

/// Keyboard state for verifying how a console layout types a character.
#[cfg_attr(not(windows), allow(dead_code))]
pub(super) fn console_keyboard_state(mask: u16, caps_toggle: bool) -> [u8; 256] {
    let mut state = [0u8; 256];
    state[0x14] = u8::from(caps_toggle);
    for (bit, generic, left) in [(2, 0x11, 0xa2), (4, 0x12, 0xa4), (1, 0x10, 0xa0)] {
        let pressed = if mask & bit == 0 { 0 } else { 0x80 };
        state[generic] = pressed;
        state[left] = pressed;
    }
    state
}

/// Convert MapVirtualKeyExW's E0/E1-prefixed scan result to SendInput fields.
#[cfg_attr(not(windows), allow(dead_code))]
pub(super) fn mapped_scan(vk: u16, mapped: u32, extended: bool) -> (u16, bool) {
    if vk == 0x13 {
        // Pause is E1 1D 45 in hardware; E1 1D is not a standalone
        // SendInput scan (0x1D is Control). Use VK_PAUSE and scan 0x45
        // without KEYEVENTF_SCANCODE or KEYEVENTF_EXTENDEDKEY.
        return (0x45, false);
    }
    (
        (mapped & 0xff) as u16,
        extended || mapped & 0xff00 == 0xe000,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pause_uses_its_sendinput_scan_not_the_e1_ctrl_scan() {
        assert_eq!(mapped_scan(0x13, 0xe11d, false), (0x45, false));
        assert_eq!(mapped_scan(0x13, 0xe11d, true), (0x45, false));
        assert_eq!(mapped_scan(0x2e, 0xe053, true), (0x53, true));
        assert_eq!(mapped_scan(0x41, 0x001e, false), (0x1e, false));
    }
    #[test]
    fn console_state_sets_generic_and_side_modifiers_and_caps_toggle() {
        for mask in 0..=7 {
            for caps in [false, true] {
                let state = console_keyboard_state(mask, caps);
                assert_eq!(state[0x14], u8::from(caps));
                for (bit, generic, left, right) in [
                    (1, 0x10, 0xa0, 0xa1),
                    (2, 0x11, 0xa2, 0xa3),
                    (4, 0x12, 0xa4, 0xa5),
                ] {
                    let expected = if mask & bit == 0 { 0 } else { 0x80 };
                    assert_eq!(state[generic], expected, "generic mask {mask}");
                    assert_eq!(state[left], expected, "left mask {mask}");
                    assert_eq!(state[right], 0, "right mask {mask}");
                }
            }
        }
    }
    #[test]
    fn character_modifiers_respect_both_sides_and_existing_shift() {
        assert_eq!(
            character_modifiers(7, |_| false),
            Some(vec![0xa2, 0xa4, 0xa0])
        );
        assert_eq!(
            character_modifiers(7, |vk| [0xa3, 0xa0].contains(&vk)),
            Some(vec![0xa4])
        );
        assert_eq!(character_modifiers(1, |vk| vk == 0xa1), Some(vec![]));
        assert_eq!(character_modifiers(0, |_| false), Some(vec![]));
        assert_eq!(character_modifiers(8, |_| false), None);
    }
}
