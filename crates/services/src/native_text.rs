// SPDX-License-Identifier: Apache-2.0
//! Text an agent reports reaches the phone inside one control frame, at most
//! 1 MiB of JSON. A native tool can print far more than that on one line, and
//! a journal event larger than a frame can never be delivered: the phone's
//! connection closes on it at every replay. Agent text is therefore cut, on a
//! character boundary, to a budget of encoded bytes before it is recorded.
use ports::{HarnessEvent, NativeApproval};

/// Agent prose, streamed or whole.
pub(crate) const MESSAGE: usize = 512 * 1024;
/// A tool call's detail or a note.
pub(crate) const DETAIL: usize = 32 * 1024;
/// A helper's label, repeated in its run for every helper.
pub(crate) const LABEL: usize = 1024;

const CUT: &str = "… (too long to show in full)";

/// An approval's record, whole request included, that one control frame can
/// carry once wrapped in an event. The phone decides only on a request it can
/// show whole, since a cut preview offers no exact request to check, so a
/// larger request is refused rather than cut.
const APPROVAL: usize = 768 * 1024;

pub(crate) fn approval_fits(record: &impl serde::Serialize) -> bool {
    serde_json::to_vec(record).is_ok_and(|bytes| bytes.len() <= APPROVAL)
}

pub(crate) fn native_approval_fits(native: &NativeApproval) -> bool {
    approval_fits(&(
        &native.title,
        &native.detail,
        &native.detail_lines,
        &native.questions,
        &native.raw_request,
        &native.scope,
    ))
}

/// `event` with the text an agent controls cut to its budget. Identifiers and
/// native names are kept whole: runs, tools and approvals are found by them.
pub(crate) fn fitted(event: HarnessEvent) -> HarnessEvent {
    match event {
        HarnessEvent::AgentTextDelta { message_key, text } => HarnessEvent::AgentTextDelta {
            message_key,
            text: fit(text, MESSAGE),
        },
        HarnessEvent::AgentMessage { message_key, text } => HarnessEvent::AgentMessage {
            message_key,
            text: fit(text, MESSAGE),
        },
        HarnessEvent::Activity {
            key,
            tool,
            summary,
            status,
            detail,
        } => HarnessEvent::Activity {
            key,
            tool,
            summary,
            status,
            detail: detail.map(|detail| fit(detail, DETAIL)),
        },
        HarnessEvent::Child {
            lifetime,
            native_ref,
            label,
            state,
        } => HarnessEvent::Child {
            lifetime,
            native_ref,
            label: fit(label, LABEL),
            state,
        },
        HarnessEvent::Warning { text } => HarnessEvent::Warning {
            text: fit(text, DETAIL),
        },
        HarnessEvent::ReadinessWarning { reason } => HarnessEvent::ReadinessWarning {
            reason: fit(reason, DETAIL),
        },
        other => other,
    }
}

/// `text` whole if its JSON string content fits `budget` bytes, otherwise its
/// longest prefix that fits with the mark of the cut.
pub(crate) fn fit(mut text: String, budget: usize) -> String {
    let room = budget.saturating_sub(CUT.len());
    let mut used = 0;
    let mut keep = 0;
    for (index, c) in text.char_indices() {
        used += encoded_len(c);
        if used > budget {
            text.truncate(keep);
            text.push_str(CUT);
            return text;
        }
        if used <= room {
            keep = index + c.len_utf8();
        }
    }
    text
}

/// Bytes `c` takes inside a JSON string (RFC 8259 section 7, as serde_json
/// writes it).
fn encoded_len(c: char) -> usize {
    match c {
        '"' | '\\' | '\u{8}' | '\u{c}' | '\n' | '\r' | '\t' => 2,
        '\0'..='\u{1f}' => 6,
        c => c.len_utf8(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encoded(text: &str) -> usize {
        serde_json::to_string(text).unwrap().len() - 2
    }

    #[test]
    fn a_character_costs_what_serde_json_writes() {
        for c in (0..0x80)
            .filter_map(char::from_u32)
            .chain(['é', '€', '😀', '\u{2028}'])
        {
            assert_eq!(encoded_len(c), encoded(c.encode_utf8(&mut [0; 4])), "{c:?}");
        }
    }

    // Generated text mixing every escape class and multibyte characters,
    // cut to every budget around its size.
    #[test]
    fn any_text_fits_its_budget_and_keeps_its_start() {
        const ALPHABET: [char; 10] = ['a', '"', '\\', '\n', '\u{1}', '\u{7f}', 'é', '€', '😀', ' '];
        let mut seed = 0x9e37_79b9_7f4a_7c15_u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for _ in 0..2000 {
            let length = (next() % 80) as usize;
            let text: String = (0..length)
                .map(|_| ALPHABET[(next() % ALPHABET.len() as u64) as usize])
                .collect();
            for budget in [0, 1, CUT.len(), CUT.len() + 1, encoded(&text), 40, 100] {
                let out = fit(text.clone(), budget);
                if encoded(&text) <= budget {
                    assert_eq!(out, text);
                    continue;
                }
                let kept = out.strip_suffix(CUT).expect("a cut is marked");
                assert!(text.starts_with(kept), "{text:?} -> {out:?}");
                assert!(encoded(kept) + CUT.len() <= budget.max(CUT.len()));
                // Nothing more of the text would have fitted.
                if let Some(c) = text[kept.len()..].chars().next() {
                    assert!(encoded(kept) + encoded_len(c) + CUT.len() > budget);
                }
            }
        }
    }
}
