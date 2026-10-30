// SPDX-License-Identifier: Apache-2.0
use crate::{Snapshot, thresholds::*};
use protocol::voice::*;
use std::time::Duration;
#[derive(Debug, Clone, PartialEq)]
pub enum Decision {
    Ignore,
    Wait,
    Refuse(String),
    Choose(Vec<VoiceElement>, VoiceAction),
    Confirm(VoiceAction),
    Act(VoiceAction),
    Cancel,
}
pub fn evaluate(
    text: &str,
    snapshot: &Snapshot,
    a: &VoiceAnswers,
    final_segment: bool,
    silence: Duration,
) -> Decision {
    if a.is_command.score < IS_COMMAND {
        return Decision::Ignore;
    }
    if a.intent.confidence < INTENT || a.intent.choice == "none" {
        return Decision::Wait;
    }
    if !final_segment && a.complete.score < COMPLETE && silence < SILENCE {
        return Decision::Wait;
    }
    let kind = match a.intent.choice.as_str() {
        "click" => VoiceActionKind::Click,
        "type" => VoiceActionKind::Type,
        "scroll" => VoiceActionKind::Scroll,
        "navigate" => VoiceActionKind::Navigate,
        "back" => VoiceActionKind::Back,
        "forward" => VoiceActionKind::Forward,
        "switch_tab" => VoiceActionKind::SwitchTab,
        "focus" => VoiceActionKind::Focus,
        "press_key" => VoiceActionKind::PressKey,
        "cancel" => return Decision::Cancel,
        _ => return Decision::Refuse("That action is not supported".into()),
    };
    // Literal payloads cannot be truncated by an early completeness prediction.
    if matches!(
        kind,
        VoiceActionKind::Type | VoiceActionKind::Navigate | VoiceActionKind::PressKey
    ) && !final_segment
        && silence < SILENCE
    {
        return Decision::Wait;
    }
    let literal = match kind {
        VoiceActionKind::Type => span(text, &["type ", "write "]),
        VoiceActionKind::Navigate => span(text, &["go to ", "navigate to ", "open "]).filter(|s| {
            (s.starts_with("https://") || s.starts_with("http://"))
                && !s.chars().any(char::is_whitespace)
                && !s.contains('@')
        }),
        VoiceActionKind::PressKey => span(text, &["press "]).filter(|s| allowed_key(s)),
        _ => None,
    };
    if matches!(
        kind,
        VoiceActionKind::Type | VoiceActionKind::Navigate | VoiceActionKind::PressKey
    ) && literal.is_none()
    {
        return Decision::Refuse(
            "Use ‘type …’, ‘go to https://…’, or ‘press Enter’. The text is copied exactly".into(),
        );
    }
    let target = snapshot
        .elements
        .iter()
        .find(|e| {
            matches!(
                kind,
                VoiceActionKind::Click | VoiceActionKind::Type | VoiceActionKind::Focus
            ) && e.id == a.target.choice
        })
        .cloned();
    let mut action = VoiceAction {
        kind,
        target,
        literal,
        scroll_y: if text.to_lowercase().split_whitespace().any(|w| w == "up") {
            -600
        } else {
            600
        },
        previous_tab: text.to_lowercase().contains("previous"),
    };
    if matches!(
        kind,
        VoiceActionKind::Click | VoiceActionKind::Type | VoiceActionKind::Focus
    ) && (a.target.confidence < TARGET_CONFIDENCE
        || a.target
            .probabilities
            .get(&a.target.choice)
            .copied()
            .unwrap_or(0.0)
            < TARGET_PROBABILITY
        || action.target.is_none())
    {
        let mut choices: Vec<_> = snapshot
            .elements
            .iter()
            .filter(|e| e.enabled && e.visible)
            .cloned()
            .collect();
        choices.sort_by(|x, y| {
            a.target
                .probabilities
                .get(&y.id)
                .copied()
                .unwrap_or(0.0)
                .total_cmp(&a.target.probabilities.get(&x.id).copied().unwrap_or(0.0))
        });
        choices.truncate(3);
        if choices.is_empty() {
            if kind == VoiceActionKind::Type {
                action.target = snapshot.elements.iter().find(|e| e.focused).cloned();
            }
            if action.target.is_none() {
                return Decision::Refuse("No accessible target is available. Scroll, press a key, or use the remote keyboard".into());
            }
        } else {
            return Decision::Choose(choices, action);
        }
    }
    check_action(action, a, false)
}
pub fn check_action(action: VoiceAction, a: &VoiceAnswers, confirmed: bool) -> Decision {
    if let Some(e) = &action.target {
        if !e.enabled || !e.visible {
            return Decision::Refuse(
                "That control is hidden or disabled. Ask again after it is visible".into(),
            );
        }
        if action.kind == VoiceActionKind::Type && e.sensitive {
            return Decision::Refuse("Voice typing is unavailable for password or sensitive fields. Use the remote keyboard".into());
        }
    }
    let key_risk = action.kind == VoiceActionKind::PressKey
        && action.literal.as_ref().is_some_and(|k| {
            matches!(
                k.as_str(),
                "Enter" | "enter" | "Delete" | "delete" | "Backspace" | "backspace"
            )
        });
    let name_risk = action.target.as_ref().is_some_and(|e| {
        e.destructive
            || e.name
                .to_lowercase()
                .split(|c: char| !c.is_alphanumeric())
                .any(|w| {
                    matches!(
                        w,
                        "submit"
                            | "send"
                            | "buy"
                            | "pay"
                            | "delete"
                            | "purchase"
                            | "checkout"
                            | "remove"
                            | "close"
                    )
                })
    });
    if !confirmed
        && (a.destructive.score >= DESTRUCTIVE
            || a.is_correction.score >= CORRECTION
            || key_risk
            || name_risk)
    {
        Decision::Confirm(action)
    } else {
        Decision::Act(action)
    }
}
fn span(text: &str, prefixes: &[&str]) -> Option<String> {
    let text = text.trim();
    let lower = text.to_lowercase();
    let prefix = prefixes.iter().find(|p| lower.starts_with(**p))?;
    let payload = text.get(prefix.len()..)?;
    let payload = if let Some(quoted) = payload.strip_prefix('"') {
        let end = quoted.find('"')?;
        let tail = &quoted[end + 1..];
        if !tail.is_empty() && !tail.starts_with(" into ") {
            return None;
        }
        &quoted[..end]
    } else {
        payload
    };
    (!payload.is_empty()).then(|| payload.to_owned())
}
pub fn allowed_key(s: &str) -> bool {
    matches!(
        s,
        "Enter"
            | "enter"
            | "Tab"
            | "tab"
            | "Escape"
            | "escape"
            | "Backspace"
            | "backspace"
            | "Delete"
            | "delete"
            | "ArrowUp"
            | "ArrowDown"
            | "ArrowLeft"
            | "ArrowRight"
            | "Home"
            | "End"
            | "PageUp"
            | "PageDown"
    )
}
pub fn describe(a: &VoiceAction) -> String {
    let name = a
        .target
        .as_ref()
        .map(|e| format!(" “{}”", e.name))
        .unwrap_or_default();
    match a.kind {
        VoiceActionKind::Click => format!("Click{name}"),
        VoiceActionKind::Type => format!("Type into{name}"),
        VoiceActionKind::Focus => format!("Focus{name}"),
        VoiceActionKind::Navigate => format!("Open {}", a.literal.as_deref().unwrap_or("the URL")),
        VoiceActionKind::PressKey => format!("Press {}", a.literal.as_deref().unwrap_or("the key")),
        VoiceActionKind::Scroll => if a.scroll_y < 0 {
            "Scroll up"
        } else {
            "Scroll down"
        }
        .into(),
        VoiceActionKind::Back => "Go back".into(),
        VoiceActionKind::Forward => "Go forward".into(),
        VoiceActionKind::SwitchTab => "Switch tab".into(),
        _ => "No action".into(),
    }
}
