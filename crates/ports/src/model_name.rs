// SPDX-License-Identifier: Apache-2.0
//! How a model is named to a person. Harnesses report ids; an id reads the way
//! the catalogs write names (a Claude id as Anthropic writes it, a GPT id as
//! Codex publishes it), and any other id stays verbatim,
//! because a guessed name misleads where the id does not. A harness catalog
//! that publishes its own names (Codex does) is used as published instead of
//! this. The client follows the same rules, held to the shared
//! `model-names.json` fixture.

const FAMILIES: [&str; 4] = ["opus", "sonnet", "haiku", "fable"];

/// "claude-opus-5-5" → "Claude Opus 5.5"; "sonnet" → "Claude Sonnet (latest)";
/// "gpt-6-astra" → "GPT-6-Astra".
pub fn model_name(id: &str) -> String {
    claude(id)
        .or_else(|| gpt(id))
        .unwrap_or_else(|| id.to_owned())
}

/// A model named beside its agent drops the vendor the agent's name already
/// gives: "Claude Opus 5.5" beside "Claude Code" is "Opus 5.5".
pub fn beside_agent<'a>(agent: &str, name: &'a str) -> &'a str {
    match name.split_once(' ') {
        // "Claude 3.5 Sonnet" keeps its vendor: a version alone is not a name.
        Some((vendor, rest))
            if !vendor.is_empty()
                && rest.starts_with(|c: char| c.is_ascii_alphabetic())
                && (agent == vendor
                    || agent
                        .strip_prefix(vendor)
                        .is_some_and(|a| a.starts_with(' '))) =>
        {
            rest
        }
        _ => name,
    }
}

fn claude(id: &str) -> Option<String> {
    let mut notes = Vec::new();
    let id = match id.strip_suffix("[1m]") {
        Some(id) => {
            notes.push("1M context");
            id
        }
        None => id,
    };
    let name = if let Some(family) = FAMILIES.iter().find(|f| **f == id) {
        notes.insert(0, "latest");
        format!("Claude {}", title(family))
    } else {
        let rest = id.strip_prefix("claude-")?;
        let rest = match rest.rsplit_once(['-', '@']) {
            Some((rest, date)) if date.len() == 8 && date.bytes().all(|b| b.is_ascii_digit()) => {
                rest
            }
            _ => rest,
        };
        let parts: Vec<_> = rest.split('-').collect();
        match parts.as_slice() {
            [family, version @ ..] if FAMILIES.contains(family) => {
                format!("Claude {} {}", title(family), version_of(version)?)
            }
            [version @ .., family] if FAMILIES.contains(family) => {
                format!("Claude {} {}", version_of(version)?, title(family))
            }
            _ => return None,
        }
    };
    Some(if notes.is_empty() {
        name
    } else {
        format!("{name} ({})", notes.join(", "))
    })
}

/// "gpt-5.6-sol" → "GPT-5.6-Sol".
fn gpt(id: &str) -> Option<String> {
    let mut parts = id.strip_prefix("gpt-")?.split('-');
    let version = parts.next()?;
    let numbers: Vec<_> = version.split('.').collect();
    if numbers.len() > 2
        || !numbers
            .iter()
            .all(|n| (1..=2).contains(&n.len()) && n.bytes().all(|b| b.is_ascii_digit()))
    {
        return None;
    }
    let mut name = format!("GPT-{version}");
    for word in parts {
        if word.is_empty() || !word.bytes().all(|b| b.is_ascii_lowercase()) {
            return None;
        }
        name.push('-');
        name.push_str(&title(word));
    }
    Some(name)
}

/// ["4", "5"] → "4.5"; a major and at most one minor number, never a date.
fn version_of(parts: &[&str]) -> Option<String> {
    let number = |p: &&str| {
        (1..=2).contains(&p.len())
            && p.bytes().all(|b| b.is_ascii_digit())
            && !(p.len() == 2 && p.starts_with('0'))
    };
    (matches!(parts.len(), 1 | 2) && parts.iter().all(number)).then(|| parts.join("."))
}

fn title(word: &str) -> String {
    word[..1].to_uppercase() + &word[1..]
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Deserialize)]
    struct Fixture {
        names: Vec<Name>,
        beside: Vec<Beside>,
    }
    #[derive(Deserialize)]
    struct Name {
        id: String,
        name: String,
    }
    #[derive(Deserialize)]
    struct Beside {
        agent: String,
        id: String,
        shown: String,
    }

    #[test]
    fn names_match_the_shared_fixture() {
        let fixture: Fixture =
            serde_json::from_str(include_str!("../test-fixtures/model-names.json")).unwrap();
        for case in fixture.names {
            assert_eq!(model_name(&case.id), case.name, "{}", case.id);
        }
        for case in fixture.beside {
            let name = model_name(&case.id);
            assert_eq!(beside_agent(&case.agent, &name), case.shown, "{}", case.id);
        }
    }
}
