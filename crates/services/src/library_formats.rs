// SPDX-License-Identifier: Apache-2.0
//! Deliberately version-bound templates, never an activation operation.
use protocol::{capability::*, library::*};

// These minimal schemas were checked against the native versions and their
// vendor documentation. New releases require another compatibility review;
// an unfamiliar version continues to support the generic task draft.
const CLAUDE_VERSION: &str = "2.1.280";
const CODEX_VERSION: &str = "0.153.2";

pub(super) fn templates(info: Option<&HarnessInfo>, body: &str) -> Vec<ReusableFormat> {
    let version = info
        .and_then(|i| i.native_version.as_deref())
        .and_then(|v| {
            v.split_whitespace()
                .find(|p| p.as_bytes().first().is_some_and(u8::is_ascii_digit))
        });
    let known = info.is_some_and(|i| {
        i.installed.is_available()
            && match i.kind {
                HarnessKind::ClaudeCode => version == Some(CLAUDE_VERSION),
                HarnessKind::Codex => version == Some(CODEX_VERSION),
                _ => false,
            }
    });
    let mut formats = vec![ReusableFormat {
        kind: ReusableFormatKind::Task,
        availability: Availability::Available,
        suggested_name: "reusable-task.md".into(),
        text: Some(body.into()),
        instructions: "A task draft for manual review. Saving does not start an agent.".into(),
        documentation: None,
    }];
    for kind in [
        ReusableFormatKind::Skill,
        ReusableFormatKind::Subagent,
        ReusableFormatKind::Prompt,
    ] {
        let mut f = ReusableFormat {
            kind, availability: Availability::Unsupported { reason: "This agent’s installed version has no verified template for this kind. Use a task draft.".into() },
            suggested_name: String::new(), text: None, instructions: String::new(), documentation: None,
        };
        if !known {
            f.availability = Availability::Unknown { reason: if version.is_none() {
                "The installed agent version is unknown. Only the generic task draft is available."
            } else { "Native formats have not been verified for this installed version. Only the generic task draft is available." }.into() };
        } else {
            match (info.map(|i| i.kind), kind) {
                (Some(HarnessKind::ClaudeCode), ReusableFormatKind::Skill) => {
                    f.text = Some(format!(
                        "---\nname: reviewed-task\ndescription: Describe when this skill should be used.\ndisable-model-invocation: true\n---\n\n{body}"
                    ));
                    f.suggested_name = "skill-draft.md".into();
                    f.instructions = "After review, move into a skill folder as SKILL.md to make it discoverable. Automatic model invocation is disabled in this draft.".into();
                    f.documentation = Some("https://code.claude.com/docs/en/skills".into());
                }
                (Some(HarnessKind::ClaudeCode), ReusableFormatKind::Subagent) => {
                    f.text = Some(format!(
                        "---\nname: reviewed-task\ndescription: Describe when to delegate to this agent.\ntools: Read, Grep, Glob\n---\n\n{body}"
                    ));
                    f.suggested_name = "subagent-draft.md".into();
                    f.instructions = "Review instructions and tools before moving this Markdown file into a native agents folder. The template allows only reading tools.".into();
                    f.documentation = Some("https://code.claude.com/docs/en/sub-agents".into());
                }
                (Some(HarnessKind::Codex), ReusableFormatKind::Prompt) => {
                    f.text = Some(format!(
                        "---\ndescription: Describe when to use this prompt.\n---\n\n{body}"
                    ));
                    f.suggested_name = "prompt-draft.md".into();
                    f.instructions = "Custom prompts are a legacy format. After review, move the Markdown file to the native prompts folder; restart the agent to discover it.".into();
                    f.documentation = Some("https://learn.chatgpt.com/docs/custom-prompts".into());
                }
                (Some(HarnessKind::Codex), ReusableFormatKind::Subagent) => {
                    let quoted = toml_instructions(body);
                    f.text = Some(format!(
                        "name = \"reviewed_task\"\ndescription = \"Describe when to delegate to this agent.\"\nsandbox_mode = \"read-only\"\ndeveloper_instructions = {quoted}\n"
                    ));
                    f.suggested_name = "agent-draft.toml.md".into();
                    f.instructions = "This preview contains native TOML. After review, rename the draft to .toml and move it into a native agents folder. It requests a read-only sandbox.".into();
                    f.documentation =
                        Some("https://learn.chatgpt.com/docs/agent-configuration/subagents".into());
                }
                _ => {}
            }
            if f.text.is_some() {
                f.availability = Availability::Available;
            }
        }
        formats.push(f);
    }
    formats
}

/// Keep paragraphs editable while escaping delimiters and control characters.
fn toml_instructions(body: &str) -> String {
    let mut literal = String::from("\"\"\"\n");
    for ch in body.chars() {
        match ch {
            '\n' => literal.push('\n'),
            '\\' => literal.push_str("\\\\"),
            '\"' => literal.push_str("\\\""),
            ch if ch < ' ' || ch == '\u{7f}' => {
                use std::fmt::Write;
                write!(literal, "\\u{:04x}", ch as u32).expect("writing to a string cannot fail");
            }
            ch => literal.push(ch),
        }
    }
    literal.push_str("\"\"\"");
    literal
}

#[cfg(test)]
mod tests {
    use super::*;
    async fn info(kind: HarnessKind, version: Option<&str>) -> HarnessInfo {
        use ports::HarnessAdapter;
        let mut i = crate::testing::FakeHarness::default().probe().await;
        i.kind = kind;
        i.native_version = version.map(str::to_owned);
        i
    }
    #[tokio::test]
    async fn native_formats_are_version_bound_and_inert_drafts() {
        for (kind, version) in [
            (HarnessKind::ClaudeCode, "2.1.280 (Claude Code)"),
            (HarnessKind::Codex, "codex-cli 0.153.2"),
        ] {
            let forms = templates(
                Some(&info(kind, Some(version)).await),
                "# A task\n\n\"quoted\"\\text\u{7f}",
            );
            assert_eq!(forms.iter().filter(|f| f.text.is_some()).count(), 3);
            assert!(
                forms
                    .iter()
                    .all(|f| f.text.is_none() || f.suggested_name.ends_with(".md"))
            );
            let native = &forms[2];
            if kind == HarnessKind::Codex {
                assert!(
                    native
                        .text
                        .as_ref()
                        .unwrap()
                        .contains("developer_instructions = \"\"\"\n# A task\n")
                );
                assert!(!native.text.as_ref().unwrap().contains('\u{7f}'));
                assert!(native.text.as_ref().unwrap().contains("\\u007f"));
            }
            assert!(
                native
                    .text
                    .as_ref()
                    .unwrap()
                    .contains(if kind == HarnessKind::ClaudeCode {
                        "tools: Read, Grep, Glob"
                    } else {
                        "sandbox_mode = \"read-only\""
                    })
            );
        }
        assert_eq!(
            toml_instructions("a\n\"\"\"\\end\u{7f}"),
            "\"\"\"\na\n\\\"\\\"\\\"\\\\end\\u007f\"\"\""
        );
        for version in [None, Some("9.9.9"), Some("2.1.280-custom"), Some("2.1.281")] {
            let forms = templates(Some(&info(HarnessKind::ClaudeCode, version).await), "task");
            assert_eq!(forms.iter().filter(|f| f.text.is_some()).count(), 1);
            assert!(forms[1..].iter().all(|f| !f.availability.is_available()));
        }
    }
}
