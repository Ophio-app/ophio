// SPDX-License-Identifier: Apache-2.0
//! Pure layout lookup. Command shortcuts use the layout's Command layer, which
//! may differ from both its typing layer and its physical US key positions.

pub(crate) const FLAG_SHIFT: u64 = 0x0002_0000;
pub(crate) const FLAG_OPTION: u64 = 0x0008_0000;
pub(crate) const FLAG_COMMAND: u64 = 0x0010_0000;
pub(crate) const CHARACTER_MODIFIERS: [u64; 8] = [
    0,
    FLAG_SHIFT,
    FLAG_OPTION,
    FLAG_SHIFT | FLAG_OPTION,
    FLAG_COMMAND,
    FLAG_COMMAND | FLAG_SHIFT,
    FLAG_COMMAND | FLAG_OPTION,
    FLAG_COMMAND | FLAG_SHIFT | FLAG_OPTION,
];

/// UCKeyTranslate takes the high byte of Carbon EventModifiers, not the Carbon
/// masks themselves and not CoreGraphics flags. cmdKey=0x100, shiftKey=0x200,
/// optionKey=0x800. Keep the conversion explicit and independently testable.
pub(crate) fn carbon_modifier_state(flags: u64) -> u32 {
    let masks = (if flags & FLAG_COMMAND != 0 { 0x0100 } else { 0 })
        | (if flags & FLAG_SHIFT != 0 { 0x0200 } else { 0 })
        | (if flags & FLAG_OPTION != 0 { 0x0800 } else { 0 });
    masks >> 8
}

#[derive(Debug, Clone)]
pub(crate) struct CharacterTable {
    output: [[Option<char>; 8]; 128],
}
impl Default for CharacterTable {
    fn default() -> Self {
        Self {
            output: [[None; 8]; 128],
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CharacterChoice {
    pub(crate) keycode: u16,
    pub(crate) modifiers: u64,
}
impl CharacterTable {
    pub(crate) fn set(&mut self, keycode: u16, modifiers: u64, character: char) {
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

    /// Never synthesize Command to type a character, and never resolve a Command
    /// shortcut through the normal typing layer or add modifiers that would
    /// change its meaning. Shortcut letters may differ in case on the exact
    /// held layer, because shared shortcut handling lowercases them. Prefer an
    /// exact character before that fallback. Ordinary character lookup stays
    /// case-exact and prefers fewer modifiers, then reuses held modifiers,
    /// then chooses the lowest key code.
    pub(crate) fn choose(&self, character: char, held: u64) -> Option<CharacterChoice> {
        type MatchRank = (bool, u32, u32, u16, usize);
        let mut best: Option<(MatchRank, CharacterChoice)> = None;
        for (keycode, row) in self.output.iter().enumerate() {
            for (index, &modifiers) in CHARACTER_MODIFIERS.iter().enumerate() {
                let Some(output) = row[index] else {
                    continue;
                };
                let inexact = output != character;
                if modifiers & FLAG_COMMAND != held & FLAG_COMMAND
                    || (held & FLAG_COMMAND != 0
                        && modifiers != held & (FLAG_COMMAND | FLAG_SHIFT | FLAG_OPTION))
                    || (inexact
                        && !(held & FLAG_COMMAND != 0
                            && character.is_ascii_alphabetic()
                            && output.eq_ignore_ascii_case(&character)))
                {
                    continue;
                }
                let choice = CharacterChoice {
                    keycode: keycode as u16,
                    modifiers,
                };
                let rank = (
                    inexact,
                    modifiers.count_ones(),
                    (modifiers & !held).count_ones(),
                    choice.keycode,
                    index,
                );
                if best.is_none_or(|(best_rank, _)| rank < best_rank) {
                    best = Some((rank, choice));
                }
            }
        }
        best.map(|(_, choice)| choice)
    }

    #[cfg(test)]
    pub(crate) fn has_characters(&self) -> bool {
        self.output
            .iter()
            .any(|row| row.iter().any(Option::is_some))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn carbon_translation_uses_the_documented_modifier_byte() {
        assert_eq!(carbon_modifier_state(0), 0);
        assert_eq!(carbon_modifier_state(FLAG_COMMAND), 1);
        assert_eq!(carbon_modifier_state(FLAG_SHIFT), 2);
        assert_eq!(carbon_modifier_state(FLAG_OPTION), 8);
        assert_eq!(
            carbon_modifier_state(FLAG_COMMAND | FLAG_SHIFT | FLAG_OPTION),
            11
        );
        for flags in CHARACTER_MODIFIERS {
            assert!(carbon_modifier_state(flags) <= 0xff);
        }
    }

    #[test]
    fn russian_typing_layer_does_not_hide_command_v() {
        let mut table = CharacterTable::default();
        table.set(9, 0, 'м');
        table.set(9, FLAG_COMMAND, 'v');
        assert_eq!(
            table.choose('v', FLAG_COMMAND),
            Some(CharacterChoice {
                keycode: 9,
                modifiers: FLAG_COMMAND
            })
        );
        assert_eq!(
            table.choose('v', 0),
            None,
            "normal character input never borrows Command"
        );
        assert_eq!(table.choose('м', 0).unwrap().keycode, 9);
        assert!(table.has_characters());
    }

    #[test]
    fn alternative_latin_layout_uses_its_actual_command_position() {
        let mut table = CharacterTable::default();
        table.set(9, 0, 'k');
        table.set(9, FLAG_COMMAND, 'k');
        table.set(47, 0, 'v');
        table.set(47, FLAG_COMMAND, 'v');
        assert_eq!(
            table.choose('v', FLAG_COMMAND).unwrap().keycode,
            47,
            "never hard-code the US V position"
        );
    }

    #[test]
    fn dvorak_command_qwerty_uses_command_not_typing_position() {
        let mut table = CharacterTable::default();
        table.set(47, 0, 'v');
        table.set(47, FLAG_COMMAND, '/');
        table.set(9, 0, 'k');
        table.set(9, FLAG_COMMAND, 'v');
        assert_eq!(table.choose('v', 0).unwrap().keycode, 47);
        assert_eq!(table.choose('v', FLAG_COMMAND).unwrap().keycode, 9);
    }

    #[test]
    fn unavailable_command_layer_refuses_instead_of_guessing_from_typing() {
        let mut table = CharacterTable::default();
        table.set(9, 0, 'v');
        assert!(table.choose('v', FLAG_COMMAND).is_none());
        assert!(table.choose('v', 0).is_some());
    }

    #[test]
    fn command_shortcuts_never_add_option_or_shift_to_find_a_letter() {
        let mut table = CharacterTable::default();
        table.set(9, FLAG_COMMAND | FLAG_OPTION, 'v');
        table.set(47, FLAG_COMMAND | FLAG_SHIFT, 'v');
        assert!(
            table.choose('v', FLAG_COMMAND).is_none(),
            "Command-Option-V or Command-Shift-V is not the requested paste chord"
        );
        assert_eq!(
            table
                .choose('v', FLAG_COMMAND | FLAG_OPTION)
                .unwrap()
                .keycode,
            9
        );
        assert_eq!(
            table
                .choose('v', FLAG_COMMAND | FLAG_SHIFT)
                .unwrap()
                .keycode,
            47
        );
    }

    #[test]
    fn command_shift_letters_resolve_without_changing_the_held_layer() {
        let mut table = CharacterTable::default();
        table.set(6, 0, 'z');
        table.set(6, FLAG_SHIFT, 'Z');
        table.set(6, FLAG_COMMAND, 'z');
        table.set(6, FLAG_COMMAND | FLAG_SHIFT, 'Z');
        for character in ['z', 'Z'] {
            assert_eq!(
                table.choose(character, FLAG_COMMAND | FLAG_SHIFT),
                Some(CharacterChoice {
                    keycode: 6,
                    modifiers: FLAG_COMMAND | FLAG_SHIFT,
                })
            );
        }
        assert_eq!(
            table.choose('z', FLAG_COMMAND).unwrap().modifiers,
            FLAG_COMMAND
        );
    }

    #[test]
    fn exact_command_characters_rank_ahead_of_case_fallbacks() {
        let mut table = CharacterTable::default();
        let held = FLAG_COMMAND | FLAG_SHIFT;
        table.set(1, held, 'Z');
        table.set(40, held, 'z');
        assert_eq!(table.choose('z', held).unwrap().keycode, 40);
        assert_eq!(table.choose('Z', held).unwrap().keycode, 1);
        assert!(table.choose('z', FLAG_COMMAND).is_none());
        assert!(table.choose('z', held | FLAG_OPTION).is_none());
    }

    #[test]
    fn ordinary_character_lookup_remains_case_exact() {
        let mut table = CharacterTable::default();
        table.set(1, 0, 'Z');
        assert!(table.choose('z', 0).is_none());
        table.set(40, 0, 'z');
        assert_eq!(table.choose('Z', 0).unwrap().keycode, 1);
        assert_eq!(table.choose('z', 0).unwrap().keycode, 40);
    }

    #[test]
    fn character_preferences_and_held_modifier_reuse_are_unchanged() {
        let mut table = CharacterTable::default();
        table.set(2, FLAG_SHIFT, 'a');
        table.set(30, 0, 'a');
        table.set(15, FLAG_SHIFT, '!');
        table.set(20, FLAG_OPTION, '!');
        assert_eq!(table.choose('a', 0).unwrap().keycode, 30);
        assert_eq!(
            table.choose('!', FLAG_OPTION).unwrap(),
            CharacterChoice {
                keycode: 20,
                modifiers: FLAG_OPTION
            }
        );
        assert!(table.choose('x', 0).is_none());
    }
}
