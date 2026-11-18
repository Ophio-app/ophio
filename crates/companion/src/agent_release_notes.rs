// SPDX-License-Identifier: Apache-2.0
//! Conservative extraction of newly announced *model IDs* from official agent releases.
//! These are hints, not a catalog: missing or ambiguous announcements yield no IDs.

pub(crate) const LIMIT: usize = 2 << 20;
const LINE_LIMIT: usize = 2048;
const MAX_IDS: usize = 16;

#[derive(Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
struct Version(u64, u64, u64);

fn version(s: &str) -> Option<Version> {
    let mut parts = s.split('.');
    fn component(s: &str) -> Option<u64> {
        if s.is_empty()
            || s.len() > 12
            || (s.len() > 1 && s.starts_with('0'))
            || !s.bytes().all(|b| b.is_ascii_digit())
        {
            return None;
        }
        s.parse().ok()
    }
    let result = Version(
        component(parts.next()?)?,
        component(parts.next()?)?,
        component(parts.next()?)?,
    );
    parts.next().is_none().then_some(result)
}

fn range(installed: &str, latest: &str, codex: bool) -> Option<(Version, Version)> {
    fn normalize(v: &str, codex: bool) -> &str {
        if codex {
            v.strip_prefix("rust-v").unwrap_or(v)
        } else {
            v
        }
    }
    let start = version(normalize(installed, codex))?;
    let end = version(normalize(latest, codex))?;
    (end > start).then_some((start, end))
}

fn clean_line(line: &str) -> Option<&str> {
    let line = line.strip_suffix('\r').unwrap_or(line);
    (line.len() <= LINE_LIMIT
        && !line.chars().any(|c| c.is_control())
        && !line.bytes().any(|b| matches!(b, b'<' | b'>')))
    .then_some(line)
}

/// Require an affirmative release announcement at the beginning of the bullet.
/// Mentions in fixes, removals, examples, or unrelated prose are not announcements.
fn announcement(line: &str) -> bool {
    let line = line
        .trim_start()
        .trim_start_matches(['-', '*', ' '])
        .trim_start();
    let line = line.strip_prefix("**").unwrap_or(line);
    let lower = line.to_ascii_lowercase();
    [
        "added support for ",
        "add support for ",
        "now supports ",
        "introducing ",
        "introduced ",
        "new model: ",
        "new models: ",
        "newly available model: ",
        "newly available models: ",
    ]
    .iter()
    .any(|prefix| lower.starts_with(prefix))
        || lower.starts_with("added claude-")
        || lower.starts_with("added `claude-")
        || lower.starts_with("added gpt-")
        || lower.starts_with("added `gpt-")
        || (lower.starts_with("model ") && lower.contains(" is now available"))
        || (lower.starts_with("models ") && lower.contains(" are now available"))
}

fn model_id(id: &str, codex: bool) -> bool {
    if id.len() > 64 || !id.is_ascii() {
        return false;
    }
    if codex {
        let Some(rest) = id.strip_prefix("gpt-") else {
            return false;
        };
        let mut parts = rest.split('-');
        let Some(number) = parts.next() else {
            return false;
        };
        let numbers: Vec<_> = number.split('.').collect();
        if !(1..=2).contains(&numbers.len()) || !numbers.iter().all(|n| numeric(n, 2)) {
            return false;
        }
        let suffix: Vec<_> = parts.collect();
        matches!(
            suffix.as_slice(),
            [] | ["codex" | "astra" | "luna" | "sol"] | ["codex", "mini" | "max"]
        )
    } else {
        let Some(rest) = id.strip_prefix("claude-") else {
            return false;
        };
        let mut parts = rest.split('-');
        if !matches!(parts.next(), Some("opus" | "sonnet" | "haiku" | "fable")) {
            return false;
        }
        if !parts.next().is_some_and(|n| numeric(n, 2)) {
            return false;
        }
        match parts.next() {
            None => true,
            Some(minor) if numeric(minor, 2) => match parts.next() {
                None => true,
                Some(date) => {
                    date.len() == 8
                        && date.bytes().all(|b| b.is_ascii_digit())
                        && parts.next().is_none()
                }
            },
            _ => false,
        }
    }
}

fn numeric(s: &str, max: usize) -> bool {
    !s.is_empty()
        && s.len() <= max
        && (s.len() == 1 || !s.starts_with('0'))
        && s.bytes().all(|b| b.is_ascii_digit())
}

/// A release can announce a Claude model by its exact family and numeric
/// version rather than spelling out its ID. Never infer an ID from a nickname.
fn announced_claude_name(line: &str) -> Option<String> {
    let line = line
        .trim_start()
        .trim_start_matches(['-', '*', ' '])
        .trim_start();
    let added = line.starts_with("Added Claude ");
    let rest = if added {
        line.strip_prefix("Added Claude ")?
    } else {
        line.strip_prefix("Claude ")?
    };
    let (family, rest) = rest.split_once(' ')?;
    let family = family.to_ascii_lowercase();
    if !matches!(family.as_str(), "opus" | "sonnet" | "haiku" | "fable") {
        return None;
    }
    let (number, after) = rest.split_once(' ').unwrap_or((rest, ""));
    let numbers: Vec<_> = number.split('.').collect();
    if !(1..=2).contains(&numbers.len()) || !numbers.iter().all(|n| numeric(n, 2)) {
        return None;
    }
    let id = format!("claude-{family}-{}", number.replace('.', "-"));
    if !added {
        if !after.starts_with("is now available") {
            return None;
        }
    } else if !after.is_empty()
        && !after.starts_with('(')
        && !after.starts_with(',')
        && !after.starts_with('—')
    {
        return None;
    }
    // A conflicting parenthesized ID is not evidence for the derived model.
    if let Some(parenthesis) = after.find("(`claude-")
        && !after[parenthesis..].starts_with(&format!("(`{id}`)"))
    {
        return None;
    }
    Some(id)
}

fn collect(line: &str, codex: bool, out: &mut Vec<String>) {
    let Some(line) = clean_line(line) else { return };
    if !codex && let Some(id) = announced_claude_name(line) {
        if !out.contains(&id) {
            out.push(id);
        }
        return;
    }
    if !announcement(line) {
        return;
    }
    // Never take pieces of a URL, a longer identifier or markup as model IDs.
    for token in line.split_ascii_whitespace() {
        let id = token.trim_matches(|c: char| {
            matches!(c, '`' | '*' | ',' | '.' | ':' | ';' | '(' | ')' | '[' | ']')
        });
        if model_id(id, codex) && !out.iter().any(|existing| existing == id) {
            out.push(id.to_owned());
            if out.len() == MAX_IDS {
                return;
            }
        }
    }
}

/// Read Claude Code changelog headings (`## 2.1.0`) and only newer stable sections.
pub(crate) fn claude(body: &[u8], installed: &str, latest: &str) -> Vec<String> {
    let Some((start, end)) = range(installed, latest, false) else {
        return Vec::new();
    };
    if body.len() > LIMIT {
        return Vec::new();
    }
    let Ok(text) = std::str::from_utf8(body) else {
        return Vec::new();
    };
    let mut active = false;
    let mut result = Vec::new();
    for raw in text.lines() {
        if raw.starts_with("## ") {
            active = clean_line(raw)
                .and_then(|line| version(line[3..].trim()))
                .is_some_and(|v| v > start && v <= end);
        } else if active && result.len() < MAX_IDS {
            collect(raw, false, &mut result);
        }
    }
    result
}

/// Read GitHub releases API JSON (one release or an array of releases).
pub(crate) fn codex(body: &[u8], installed: &str, latest: &str) -> Vec<String> {
    let Some((start, end)) = range(installed, latest, true) else {
        return Vec::new();
    };
    if body.len() > LIMIT {
        return Vec::new();
    }
    let Ok(json) = serde_json::from_slice::<serde_json::Value>(body) else {
        return Vec::new();
    };
    let mut result = Vec::new();
    let releases = match &json {
        serde_json::Value::Array(releases) => releases.as_slice(),
        serde_json::Value::Object(_) => std::slice::from_ref(&json),
        _ => return result,
    };
    for release in releases {
        if result.len() == MAX_IDS {
            break;
        }
        let Some(object) = release.as_object() else {
            continue;
        };
        if object.get("draft") != Some(&serde_json::Value::Bool(false))
            || object.get("prerelease") != Some(&serde_json::Value::Bool(false))
        {
            continue;
        }
        let Some(tag) = object.get("tag_name").and_then(|v| v.as_str()) else {
            continue;
        };
        if !tag
            .strip_prefix("rust-v")
            .and_then(version)
            .is_some_and(|v| v > start && v <= end)
        {
            continue;
        }
        let Some(notes) = object.get("body").and_then(|v| v.as_str()) else {
            continue;
        };
        for line in notes.lines() {
            if result.len() == MAX_IDS {
                break;
            }
            collect(line, true, &mut result);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_only_new_stable_sections_and_announcements() {
        let notes = b"## 2.3.0\n- Added support for `claude-opus-5-5` and `claude-sonnet-5-0-20261109`\n- Fixed support for claude-haiku-4-5\n## 2.2.0-beta.1\n- Added support for claude-haiku-4-5\n## 2.1.0\n- Added support for claude-haiku-4-5\n## 2.0.0\n- Added support for claude-opus-3-0";
        assert_eq!(
            claude(notes, "2.1.0", "2.3.0"),
            ["claude-opus-5-5", "claude-sonnet-5-0-20261109"]
        );
        assert!(claude(notes, "unknown", "2.3.0").is_empty());
        assert!(claude(notes, "2.1.0-rc.1", "2.3.0").is_empty());
        assert!(claude(notes, "2.3.0", "2.1.0").is_empty());
    }

    #[test]
    fn official_claude_name_announcement_and_unicode_prose() {
        let notes = "## 2.1.321\n- Added Claude Fable 5.2 (`claude-fable-5-2`), now the default Fable model — 1M context, $8/$40 per Mtok with $0.20/Mtok cache reads\n- Claude Opus 5 is now available\n- Added `claude-haiku-5`\n- Fixed Claude Sonnet 5.0 (`claude-sonnet-5-0`)\n";
        assert_eq!(
            claude(notes.as_bytes(), "2.1.320", "2.1.321"),
            ["claude-fable-5-2", "claude-opus-5", "claude-haiku-5"]
        );
        assert_eq!(
            claude(
                "## 2.0.0\n- Added Claude Fable 5.2 (`claude-opus-5-2`)".as_bytes(),
                "1.0.0",
                "2.0.0"
            ),
            Vec::<String>::new()
        );
    }

    #[test]
    fn codex_only_stable_in_range_releases() {
        let notes = br#"[
            {"tag_name":"rust-v0.101.0","draft":false,"prerelease":false,"body":"- Added support for `gpt-6-codex-mini` and gpt-6.1-codex-max\n- Fixed gpt-5-codex"},
            {"tag_name":"rust-v0.102.0","draft":true,"prerelease":false,"body":"- Added support for gpt-7-codex"},
            {"tag_name":"rust-v0.103.0","draft":false,"prerelease":true,"body":"- Added support for gpt-7-codex"},
            {"tag_name":"rust-v0.104.0-rc.1","draft":false,"prerelease":false,"body":"- Added support for gpt-7-codex"},
            {"tag_name":"rust-v0.99.0","draft":false,"prerelease":false,"body":"- Added support for gpt-5-codex"}
        ]"#;
        assert_eq!(
            codex(notes, "0.100.0", "rust-v0.103.0"),
            ["gpt-6-codex-mini", "gpt-6.1-codex-max"]
        );
        assert!(codex(notes, "unknown", "0.103.0").is_empty());
    }

    #[test]
    fn hostile_ids_and_non_announcements_are_ignored() {
        let notes = "## 3.0.0\n- Fixed support for claude-opus-5-5\n- Removed claude-opus-5-5\n- Added support for claude-opus-5-5<script>\n- Added support for claude-opus-５-５\n- Added support for claude-opus-5-5\u{0001}\n- Added support for https://bad/claude-opus-5-5\n- Added support for claude-opus-5-5-garbage\n";
        assert!(claude(notes.as_bytes(), "2.0.0", "3.0.0").is_empty());
        let json = r#"{"tag_name":"rust-v1.0.0","draft":false,"prerelease":false,"body":"- Fixed support for gpt-6-codex\n- Added support for gpt-6-arbitrary\n- Added support for gpt-６-codex\n- Added support for gpt-6-codex-extra"}"#;
        assert!(codex(json.as_bytes(), "0.99.0", "1.0.0").is_empty());
    }

    #[test]
    fn input_and_output_are_bounded() {
        assert!(claude(&vec![b' '; LIMIT + 1], "1.0.0", "2.0.0").is_empty());
        assert!(codex(&vec![b' '; LIMIT + 1], "1.0.0", "2.0.0").is_empty());
        let long = format!(
            "## 2.0.0\n- Added support for {} claude-opus-5-5",
            "x".repeat(LINE_LIMIT)
        );
        assert!(claude(long.as_bytes(), "1.0.0", "2.0.0").is_empty());
        assert!(
            claude(
                b"## 2.0.0\n- Added support for claude-opus-5-5\rignored",
                "1.0.0",
                "2.0.0"
            )
            .is_empty()
        );
        let many = format!(
            "## 2.0.0\n{}",
            (1..=20)
                .map(|n| format!("- Added support for claude-opus-5-{n}\n"))
                .collect::<String>()
        );
        assert_eq!(claude(many.as_bytes(), "1.0.0", "2.0.0").len(), MAX_IDS);
    }
}
