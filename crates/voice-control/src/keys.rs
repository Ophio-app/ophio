// SPDX-License-Identifier: Apache-2.0
//! Spoken key names and shortcuts ("press Super Enter", "press control
//! shift T") as the key names the computer's input already takes: the same
//! names the phone's remote keyboard sends.

/// Any of the spoken words for each modifier.
const MODIFIERS: &[(&[&str], &str)] = &[
    (&["control", "ctrl", "ctl", "ctr"], "ctrl"),
    (&["alt", "option", "alternate"], "alt"),
    (&["shift"], "shift"),
    (
        &[
            "super", "windows", "win", "meta", "command", "cmd", "logo", "mod",
        ],
        "super",
    ),
];
/// Spoken words, in order, for each key name.
const NAMED: &[(&[&str], &str)] = &[
    (&["enter"], "Enter"),
    (&["return"], "Enter"),
    (&["tab"], "Tab"),
    (&["escape"], "Escape"),
    (&["esc"], "Escape"),
    (&["backspace"], "Backspace"),
    (&["back", "space"], "Backspace"),
    (&["delete"], "Delete"),
    (&["del"], "Delete"),
    (&["insert"], "Insert"),
    (&["home"], "Home"),
    (&["end"], "End"),
    (&["page", "up"], "PageUp"),
    (&["pageup"], "PageUp"),
    (&["page", "down"], "PageDown"),
    (&["pagedown"], "PageDown"),
    (&["up", "arrow"], "ArrowUp"),
    (&["arrow", "up"], "ArrowUp"),
    (&["up"], "ArrowUp"),
    (&["down", "arrow"], "ArrowDown"),
    (&["arrow", "down"], "ArrowDown"),
    (&["down"], "ArrowDown"),
    (&["left", "arrow"], "ArrowLeft"),
    (&["arrow", "left"], "ArrowLeft"),
    (&["left"], "ArrowLeft"),
    (&["right", "arrow"], "ArrowRight"),
    (&["arrow", "right"], "ArrowRight"),
    (&["right"], "ArrowRight"),
    (&["space", "bar"], "Space"),
    (&["spacebar"], "Space"),
    (&["space"], "Space"),
];
const NUMBERS: &[&str] = &[
    "zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten",
    "eleven", "twelve",
];
/// Words people say around the keys themselves.
/// None of them can be a key: "a" is the A key ("control a"), never filler.
const FILLER: &[&str] = &[
    "the", "key", "keys", "button", "plus", "and", "then", "together",
];

/// The keys a spoken shortcut names, modifiers first, or None if any word is
/// not a key. `"super enter"` is `["super", "Enter"]`.
pub fn parse(spoken: &str) -> Option<Vec<String>> {
    let words: Vec<String> = spoken
        .to_lowercase()
        .replace(['+', '-', ',', '.', '!', '?'], " ")
        .split_whitespace()
        .filter(|w| !FILLER.contains(w))
        .map(str::to_owned)
        .collect();
    if words.is_empty() || words.len() > 8 {
        return None;
    }
    let mut modifiers: Vec<&str> = Vec::new();
    let mut key: Option<String> = None;
    let mut at = 0;
    while at < words.len() {
        let word = words[at].as_str();
        if let Some((_, name)) = MODIFIERS.iter().find(|(spoken, _)| spoken.contains(&word)) {
            if !modifiers.contains(name) {
                modifiers.push(name);
            }
            at += 1;
            continue;
        }
        if key.is_some() {
            return None;
        }
        if let Some((spoken, name)) = NAMED.iter().find(|(spoken, _)| {
            words.len() >= at + spoken.len() && spoken.iter().zip(&words[at..]).all(|(a, b)| a == b)
        }) {
            key = Some((*name).into());
            at += spoken.len();
            continue;
        }
        // "F4", "F 4", "F four".
        if word == "f"
            && let Some(n) = words.get(at + 1).and_then(|w| number(w))
            && (1..=12).contains(&n)
        {
            key = Some(format!("F{n}"));
            at += 2;
            continue;
        }
        if let Some(n) = word
            .strip_prefix('f')
            .and_then(|n| n.parse::<u32>().ok())
            .filter(|n| (1..=12).contains(n))
        {
            key = Some(format!("F{n}"));
            at += 1;
            continue;
        }
        if word.chars().count() == 1 && word.chars().all(|c| c.is_ascii_alphanumeric()) {
            key = Some(word.into());
            at += 1;
            continue;
        }
        if let Some(n) = number(word).filter(|n| *n <= 9) {
            key = Some(n.to_string());
            at += 1;
            continue;
        }
        return None;
    }
    // "press super" alone is a key of its own.
    let mut keys: Vec<String> = modifiers.iter().map(|m| (*m).to_owned()).collect();
    if let Some(key) = key {
        keys.push(key);
    } else if keys.len() != 1 {
        return None;
    }
    Some(keys)
}
fn number(word: &str) -> Option<u32> {
    word.parse()
        .ok()
        .or_else(|| NUMBERS.iter().position(|n| *n == word).map(|n| n as u32))
}
/// The literal a receipt shows and the driver sends: keys joined by `+`.
pub fn literal(keys: &[String]) -> String {
    keys.join("+")
}
/// Keys back from a literal, when every part is a key name this module makes.
pub fn keys(literal: &str) -> Option<Vec<String>> {
    let keys: Vec<String> = literal.split('+').map(str::to_owned).collect();
    let back = parse(&keys.iter().map(|k| spoken(k)).collect::<Vec<_>>().join(" "))?;
    (back == keys).then_some(keys)
}
fn spoken(key: &str) -> String {
    match key {
        "PageUp" => "pageup".into(),
        "PageDown" => "pagedown".into(),
        "ArrowUp" => "arrow up".into(),
        "ArrowDown" => "arrow down".into(),
        "ArrowLeft" => "arrow left".into(),
        "ArrowRight" => "arrow right".into(),
        other => other.to_lowercase(),
    }
}
/// How a shortcut reads to a person: "Super+Enter", "Ctrl+Shift+T".
pub fn describe(literal: &str) -> String {
    literal
        .split('+')
        .map(|k| match k {
            "ctrl" => "Ctrl".to_owned(),
            "alt" => "Alt".to_owned(),
            "shift" => "Shift".to_owned(),
            "super" => "Super".to_owned(),
            "ArrowUp" => "Up".to_owned(),
            "ArrowDown" => "Down".to_owned(),
            "ArrowLeft" => "Left".to_owned(),
            "ArrowRight" => "Right".to_owned(),
            k if k.chars().count() == 1 => k.to_uppercase(),
            k => k.to_owned(),
        })
        .collect::<Vec<_>>()
        .join("+")
}
/// Whether these keys are likely to submit, close, quit or delete. They are
/// confirmed before they are pressed. A desktop's own shortcuts can't be
/// known, so this lists the common ones; Jev's judgement adds to it.
pub fn risky(keys: &[String]) -> bool {
    let has = |m: &str| keys.iter().any(|k| k == m);
    let base = keys
        .iter()
        .find(|k| !matches!(k.as_str(), "ctrl" | "alt" | "shift" | "super"))
        .map(String::as_str);
    let modified = has("ctrl") || has("alt") || has("super");
    match base {
        // Enter submits and runs; with Super it is a desktop shortcut.
        Some("Enter" | "Delete" | "Backspace") => !has("super"),
        Some("F4") => has("alt") || has("ctrl"),
        // Close or quit a window, tab or app; end a shell; log out.
        Some("w" | "q") => modified,
        Some("d") => has("ctrl"),
        Some("m" | "e") => has("super") && (has("shift") || base == Some("m")),
        _ => false,
    }
}
