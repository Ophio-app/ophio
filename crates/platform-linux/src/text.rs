// SPDX-License-Identifier: Apache-2.0
use ports::PlatformError;
use std::collections::BTreeSet;
use xkbcommon::xkb;

pub(crate) fn common_keysyms(text: String) -> Result<BTreeSet<u32>, PlatformError> {
    let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
    let keymap = xkb::Keymap::new_from_string(
        &context,
        text,
        xkb::KEYMAP_FORMAT_TEXT_V1,
        xkb::KEYMAP_COMPILE_NO_FLAGS,
    )
    .ok_or_else(|| PlatformError::Unsupported("Keyboard keymap could not be read".into()))?;
    let mut common: Option<BTreeSet<u32>> = None;
    for group in 0..keymap.num_layouts() {
        let mut symbols = BTreeSet::new();
        keymap.key_for_each(|map, key| {
            for level in 0..map.num_levels_for_key(key, group) {
                symbols.extend(
                    map.key_get_syms_by_level(key, group, level)
                        .iter()
                        .map(|symbol| symbol.raw()),
                );
            }
        });
        common = Some(match common {
            None => symbols,
            Some(old) => old.intersection(&symbols).copied().collect(),
        });
    }
    common.ok_or_else(|| PlatformError::Unsupported("Keyboard group is unavailable".into()))
}

pub(crate) fn validate(
    text: &str,
    symbols: &BTreeSet<u32>,
    paste_available: bool,
) -> Result<(), PlatformError> {
    let mut characters = String::new();
    for c in text.chars() {
        if !symbols.contains(&crate::input::character_keysym(c)?) && !characters.contains(c) {
            characters.push(c);
        }
    }
    if characters.is_empty() {
        Ok(())
    } else {
        Err(PlatformError::TextUnavailable {
            characters,
            paste_available,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn whole_text_is_refused_with_distinct_missing_characters() {
        let symbols = "Caf \n"
            .chars()
            .map(|c| crate::input::character_keysym(c).unwrap())
            .collect();
        let refusal = validate("Café é Ελληνικά 日本語", &symbols, false).unwrap_err();
        let PlatformError::TextUnavailable {
            characters,
            paste_available,
        } = refusal
        else {
            panic!("text refusal expected")
        };
        assert_eq!(characters, "éΕληνικά日本語");
        assert!(!paste_available);
        assert!(validate("Caf", &symbols, false).is_ok());
    }
}
