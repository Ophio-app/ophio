// SPDX-License-Identifier: Apache-2.0
use crate::{
    Snapshot,
    apps::{self, Resolution},
    keys,
    thresholds::*,
};
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
    /// Voice control can't do this itself; a computer-use agent could. The
    /// text is a plain refusal, shown as it is wherever no agent can be
    /// offered.
    HandOff(String),
}
/// What to say when nothing can tell which window keys or text would reach.
pub const UNKNOWN_FOCUS: &str = "Øphio can’t tell which window has focus here, so it won’t send keys or text by voice. Use the remote keyboard, or open an app by voice";
/// What to say when a finished command needs several steps.
pub const SEVERAL_STEPS: &str =
    "This needs several steps. Say one step at a time, such as “open the browser”";
/// What to say when a finished command matched no action.
pub const UNCLEAR: &str = "Not sure what to do. Try “open the browser”, “press Super Enter”, “type hello”, “scroll down” or “click Sign in”";
const SAY_TEXT: &str =
    "Say the words after “type”, such as “type hello”. The text is copied exactly";
const SAY_ADDRESS: &str = "Say the address, such as “go to example.com”";
const SAY_KEYS: &str =
    "Say the key or shortcut, such as “press Enter”, “press Escape” or “press Super Enter”";
/// Choices for apps carry this before the app's id, so they can't be taken
/// for a control.
pub const APP_CHOICE: &str = "app:";
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
        // A finished command waits for nothing more: say so, not "waiting".
        return if final_segment {
            Decision::Refuse(UNCLEAR.into())
        } else {
            Decision::Wait
        };
    }
    if !final_segment && a.complete.score < COMPLETE && silence < SILENCE {
        return Decision::Wait;
    }
    let mut kind = match a.intent.choice.as_str() {
        "click" => VoiceActionKind::Click,
        "type" => VoiceActionKind::Type,
        "scroll" => VoiceActionKind::Scroll,
        "navigate" => VoiceActionKind::Navigate,
        "back" => VoiceActionKind::Back,
        "forward" => VoiceActionKind::Forward,
        "switch_tab" => VoiceActionKind::SwitchTab,
        "focus" => VoiceActionKind::Focus,
        "press_key" => VoiceActionKind::PressKey,
        "launch" => VoiceActionKind::Launch,
        "cancel" => return Decision::Cancel,
        "task" if final_segment => return Decision::HandOff(SEVERAL_STEPS.into()),
        "task" => return Decision::Wait,
        _ => return Decision::Refuse("That action is not supported".into()),
    };
    // Literal payloads come from the finished transcript only: a pause in
    // the middle of "type hello world" must never type "hello".
    if matches!(
        kind,
        VoiceActionKind::Type
            | VoiceActionKind::Navigate
            | VoiceActionKind::PressKey
            | VoiceActionKind::Launch
    ) && !final_segment
    {
        return Decision::Wait;
    }
    // "Open youtube.com" is an address, and "open Telegram" an app,
    // whichever of the two Jev picked.
    let address = address(text);
    if kind == VoiceActionKind::Launch && address.is_some() {
        kind = VoiceActionKind::Navigate;
    }
    if kind == VoiceActionKind::Navigate
        && address.is_none()
        && snapshot.apps.is_ok()
        && apps::launch_phrase(text).is_some()
    {
        kind = VoiceActionKind::Launch;
    }
    let literal = match kind {
        VoiceActionKind::Type => match span(text, &["type ", "write "]) {
            Some(text) => Some(text),
            None => return Decision::Refuse(SAY_TEXT.into()),
        },
        VoiceActionKind::Navigate => match address {
            Some(url) => Some(url),
            None => return Decision::Refuse(SAY_ADDRESS.into()),
        },
        VoiceActionKind::PressKey => {
            match span(text, &["press ", "hit ", "push ", "tap ", "key "])
                .and_then(|s| keys::parse(&s))
            {
                Some(keys) => Some(keys::literal(&keys)),
                None => return Decision::Refuse(SAY_KEYS.into()),
            }
        }
        VoiceActionKind::Launch => match apps::launch_phrase(text) {
            Some(phrase) => Some(phrase),
            None => {
                return Decision::Refuse(
                    "Say which app to open, such as “open the browser” or “open a terminal”".into(),
                );
            }
        },
        _ => None,
    };
    // Keys and text go to the focused window. Unless the compositor or
    // accessibility can say which that is, and check it again just before
    // sending, they are not sent at all.
    if snapshot.desktop
        && snapshot.context.is_empty()
        && matches!(
            kind,
            VoiceActionKind::Type
                | VoiceActionKind::PressKey
                | VoiceActionKind::Back
                | VoiceActionKind::Forward
                | VoiceActionKind::SwitchTab
        )
    {
        return Decision::Refuse(UNKNOWN_FOCUS.into());
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
        scroll_y: if text
            .to_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .any(|w| matches!(w, "up" | "upward" | "upwards"))
        {
            -600
        } else {
            600
        },
        previous_tab: text.to_lowercase().contains("previous"),
        app: None,
    };
    if kind == VoiceActionKind::Launch {
        let apps = match &snapshot.apps {
            Ok(apps) => apps,
            Err(reason) => return Decision::Refuse(reason.clone()),
        };
        return match apps::resolve(action.literal.as_deref().unwrap_or(""), apps) {
            Resolution::One(app) => {
                action.app = Some(VoiceApp {
                    id: app.id,
                    name: app.name,
                });
                check_action(action, a, false, snapshot)
            }
            Resolution::Several(apps) => Decision::Choose(
                apps.iter()
                    .map(|app| app_choice(app, &crate::apps::label(app, &apps)))
                    .collect(),
                action,
            ),
            Resolution::None(reason) => Decision::Refuse(reason),
        };
    }
    // Without readable controls a typed command goes to the focused app, as
    // the remote keyboard's would; a click needs a control to aim at.
    if snapshot.desktop && snapshot.elements.is_empty() {
        match kind {
            VoiceActionKind::Type => return check_action(action, a, false, snapshot),
            // An agent sees the screen, so it can click what can't be read.
            VoiceActionKind::Click => return Decision::HandOff(unreadable(snapshot)),
            VoiceActionKind::Focus => return Decision::Refuse(unreadable(snapshot)),
            _ => {}
        }
    }
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
        if kind == VoiceActionKind::Type
            && let Some(focused) = snapshot.elements.iter().find(|e| e.focused)
        {
            action.target = Some(focused.clone());
            return check_action(action, a, false, snapshot);
        }
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
            return Decision::Refuse("No accessible target is available. Scroll, press a key, or use the remote keyboard".into());
        }
        return Decision::Choose(choices, action);
    }
    check_action(action, a, false, snapshot)
}
/// Why a click can't be aimed, and what still works.
fn unreadable(snapshot: &Snapshot) -> String {
    let why = snapshot
        .unreadable
        .as_deref()
        .unwrap_or("The app in front exposes no controls");
    format!(
        "Can’t click by name here. {}. Opening apps, pressing keys, typing and scrolling still work",
        why.trim_end_matches('.')
    )
}
/// An app as a numbered choice. The id names the app, never a control.
pub fn app_choice(app: &apps::LaunchApp, label: &str) -> VoiceElement {
    VoiceElement {
        id: format!("{APP_CHOICE}{}", app.id),
        role: "app".into(),
        name: label.into(),
        bounds: VoiceBounds {
            x: 0.,
            y: 0.,
            width: 1.,
            height: 1.,
        },
        visible: true,
        enabled: true,
        focused: false,
        sensitive: false,
        destructive: false,
    }
}
pub fn check_action(
    action: VoiceAction,
    a: &VoiceAnswers,
    confirmed: bool,
    snapshot: &Snapshot,
) -> Decision {
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
    let press = action.kind == VoiceActionKind::PressKey;
    let key_risk = press
        && action
            .literal
            .as_deref()
            .and_then(keys::keys)
            .is_none_or(|k| keys::risky(&k));
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
    let app_risk = action.app.as_ref().is_some_and(|chosen| {
        snapshot
            .apps
            .as_ref()
            .ok()
            .and_then(|apps| apps.iter().find(|app| app.id == chosen.id))
            .is_none_or(apps::risky)
    });
    // Jev can't know what a desktop's own shortcuts do, and calls most of
    // them consequential when unsure; keys it is sure of still confirm.
    let destructive = a.destructive.score >= if press { DESTRUCTIVE_KEY } else { DESTRUCTIVE };
    if !confirmed
        && (destructive || a.is_correction.score >= CORRECTION || key_risk || name_risk || app_risk)
    {
        Decision::Confirm(action)
    } else {
        Decision::Act(action)
    }
}
/// Polite words before the command itself, removed before a payload is
/// copied: "please type hello" types "hello".
fn lead(text: &str) -> &str {
    let mut text = text.trim();
    loop {
        let lower = text.to_lowercase();
        let Some(word) = [
            "please ",
            "can you ",
            "could you ",
            "would you ",
            "will you ",
            "now ",
            "and ",
            "then ",
            "ok ",
            "okay ",
            "hey jev ",
            "jev ",
            "jev, ",
        ]
        .iter()
        .find(|w| lower.starts_with(**w)) else {
            return text;
        };
        text = text[word.len()..].trim_start();
    }
}
fn span(text: &str, prefixes: &[&str]) -> Option<String> {
    let text = lead(text);
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
        // Speech recognition ends every sentence with a full stop the
        // person didn't say. Quote the text to keep one.
        payload.strip_suffix('.').unwrap_or(payload)
    };
    (!payload.is_empty()).then(|| payload.to_owned())
}
/// Top-level domains an address may end in when it is said without
/// "https://". Anything else, such as "notes.txt", isn't taken for one.
const DOMAINS: &[&str] = &[
    "com", "org", "net", "io", "dev", "app", "ai", "co", "edu", "gov", "info", "me", "tv", "xyz",
    "gg", "so", "sh", "ly", "fm", "news", "blog", "page", "site", "online", "tech", "store",
    "shop", "wiki", "uk", "us", "ca", "de", "fr", "eu", "au", "nz", "in", "jp", "nl", "se", "no",
    "es", "it", "pl", "br", "mx", "ch", "at", "be", "dk", "fi", "ie", "kr", "cn", "studio",
];
/// The web address a command names, copied from it: an http or https URL,
/// or a domain such as "example.com" or "example dot com", opened over https.
pub fn address(text: &str) -> Option<String> {
    let spoken = text.replace(" dot ", ".").replace(" Dot ", ".");
    for word in spoken.split_whitespace() {
        let word = word
            .trim_start_matches(['"', '“', '(', '<'])
            .trim_end_matches(['.', ',', '!', '?', ';', ':', '"', '”', ')', '>']);
        if word.contains('@') || word.is_empty() {
            continue;
        }
        let lower = word.to_lowercase();
        if lower.starts_with("https://") || lower.starts_with("http://") {
            if word.len() > "https://".len() {
                return Some(word.to_owned());
            }
            continue;
        }
        let (host, path) = lower.split_once('/').unwrap_or((&lower, ""));
        let labels: Vec<&str> = host.split('.').collect();
        let good = labels.len() >= 2
            && labels.iter().all(|l| {
                !l.is_empty()
                    && l.len() <= 63
                    && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
            })
            && (labels[0] == "www" || DOMAINS.contains(labels.last().unwrap_or(&"")));
        if good {
            return Some(if path.is_empty() {
                format!("https://{host}")
            } else {
                format!("https://{host}/{}", &word[host.len() + 1..])
            });
        }
    }
    None
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
        VoiceActionKind::Type if a.target.is_none() => "Type into the focused app".into(),
        VoiceActionKind::Type => format!("Type into{name}"),
        VoiceActionKind::Focus => format!("Focus{name}"),
        VoiceActionKind::Navigate => format!("Open {}", a.literal.as_deref().unwrap_or("the URL")),
        VoiceActionKind::PressKey => format!(
            "Press {}",
            a.literal
                .as_deref()
                .map_or("the key".into(), keys::describe)
        ),
        VoiceActionKind::Launch => match &a.app {
            Some(app) => format!("Open {}", app.name),
            None => format!("Open “{}”", a.literal.as_deref().unwrap_or("the app")),
        },
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
