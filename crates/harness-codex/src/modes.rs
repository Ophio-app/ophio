// SPDX-License-Identifier: Apache-2.0
//! Qualify the installed app-server's sandbox API without starting a thread.
use crate::{io, *};

pub(crate) const MISSING: &str = "This version of Codex cannot confirm that Ask and Plan will leave files unchanged. Update Codex on the computer to use these modes.";

pub(crate) async fn probe(binary: &Path) -> Vec<ModeSupport> {
    let supported = inspect(binary).await.read_only;
    [TaskMode::Ask, TaskMode::Plan, TaskMode::Work]
        .into_iter()
        .map(|mode| ModeSupport {
            mode,
            availability: if mode == TaskMode::Work || supported {
                Availability::Available
            } else {
                Availability::Unsupported {
                    reason: MISSING.into(),
                }
            },
            enforcement: if mode == TaskMode::Work {
                "Work uses the task’s saved native launch permissions. Review its launch flags before starting.".into()
            } else if supported {
                "Files are read-only and network access is off. Requests to allow writing are refused."
                    .into()
            } else {
                "This version has not confirmed that files will stay unchanged.".into()
            },
        })
        .collect()
}

/// The setting that gives Codex's Default mode, which every Ophio mode runs
/// in, the question tool Codex otherwise keeps to its own Plan mode.
pub(crate) const QUESTIONS: &str = "default_mode_request_user_input";

/// Whether this Codex knows [`QUESTIONS`]. Setting a feature it doesn't
/// know would only be warned about, but the instruction to use the tool
/// would then name one the run doesn't have.
pub(crate) async fn asks(binary: &Path) -> bool {
    io::inspect(binary, &["features", "list"])
        .await
        .is_some_and(|output| {
            output.status.success()
                && String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .any(|line| line.split_whitespace().next() == Some(QUESTIONS))
        })
}

#[derive(Default)]
pub(crate) struct Policy {
    pub read_only: bool,
    pub developer_instructions: bool,
    pub work_resume: bool,
    /// A turn can name a skill to use, by its name and `SKILL.md`.
    pub skill_input: bool,
}

pub(crate) async fn inspect(binary: &Path) -> Policy {
    schema_support(binary).await.unwrap_or_default()
}

async fn schema_support(binary: &Path) -> Option<Policy> {
    let directory = tempfile::tempdir().ok()?;
    let output = io::inspect(
        binary,
        &[
            "app-server",
            "generate-json-schema",
            "--out",
            directory.path().to_str()?,
        ],
    )
    .await?;
    if !output.status.success() {
        return Some(Policy::default());
    }
    let mut schemas = Vec::new();
    for name in ["ThreadStartParams", "ThreadResumeParams", "TurnStartParams"] {
        let path = directory.path().join("v2").join(format!("{name}.json"));
        use tokio::io::AsyncReadExt;
        let file = tokio::fs::File::open(path).await.ok()?;
        let mut bytes = Vec::new();
        file.take(2 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .await
            .ok()?;
        if bytes.len() > 2 * 1024 * 1024 {
            return Some(Policy::default());
        }
        schemas.push((name, serde_json::from_slice::<Value>(&bytes).ok()?));
    }
    let skill_input = schemas
        .iter()
        .any(|(name, schema)| *name == "TurnStartParams" && skill_input(schema));
    let mut developer_instructions = true;
    let mut work_resume = true;
    for (name, schema) in schemas {
        if name != "TurnStartParams" {
            let kind = &schema["properties"]["developerInstructions"]["type"];
            developer_instructions &= kind == "string"
                || kind
                    .as_array()
                    .is_some_and(|types| types.iter().any(|t| t == "string"));
        }
        if name != "TurnStartParams" {
            work_resume &= enum_contains(&schema["definitions"]["SandboxMode"], "workspace-write");
        }
        if !supports(&schema, name == "TurnStartParams") {
            return Some(Policy {
                skill_input,
                ..Policy::default()
            });
        }
    }
    Some(Policy {
        read_only: true,
        developer_instructions,
        work_resume: work_resume && developer_instructions,
        skill_input,
    })
}

/// Whether a turn's input can be a skill, given by name and path.
fn skill_input(schema: &Value) -> bool {
    schema["definitions"]["UserInput"]["oneOf"]
        .as_array()
        .is_some_and(|variants| {
            variants.iter().any(|v| {
                enum_contains(&v["properties"]["type"], "skill")
                    && v["properties"]["name"]["type"] == "string"
                    && v["properties"]["path"]["type"] == "string"
            })
        })
}

fn enum_contains(value: &Value, expected: &str) -> bool {
    value["enum"]
        .as_array()
        .is_some_and(|values| values.iter().any(|v| v == expected))
}

fn refers_to(property: &Value, definition: &str) -> bool {
    let reference = format!("#/definitions/{definition}");
    property["anyOf"]
        .as_array()
        .is_some_and(|values| values.iter().any(|v| v["$ref"] == reference))
        || property["$ref"] == reference
}

fn supports(schema: &Value, turn: bool) -> bool {
    let properties = &schema["properties"];
    let definitions = &schema["definitions"];
    let sandbox = if turn {
        refers_to(&properties["sandboxPolicy"], "SandboxPolicy")
            && definitions["SandboxPolicy"]["oneOf"]
                .as_array()
                .is_some_and(|variants| {
                    variants.iter().any(|variant| {
                        enum_contains(&variant["properties"]["type"], "readOnly")
                            && variant["properties"]["networkAccess"]["type"] == "boolean"
                    })
                })
    } else {
        refers_to(&properties["sandbox"], "SandboxMode")
            && enum_contains(&definitions["SandboxMode"], "read-only")
    };
    sandbox
        && refers_to(&properties["approvalPolicy"], "AskForApproval")
        && definitions["AskForApproval"]["oneOf"]
            .as_array()
            .is_some_and(|variants| variants.iter().any(|v| enum_contains(v, "on-request")))
        && refers_to(&properties["approvalsReviewer"], "ApprovalsReviewer")
        && enum_contains(&definitions["ApprovalsReviewer"], "user")
}

/// Work asks the person before anything outside Codex's own sandbox. That
/// holds only while a sandbox is in force and approvals come to the person.
pub(crate) fn asking_response(result: &Value) -> bool {
    matches!(
        result["sandbox"]["type"].as_str(),
        Some("readOnly" | "workspaceWrite")
    ) && result["approvalPolicy"] == "on-request"
        && (result["approvalsReviewer"].is_null() || result["approvalsReviewer"] == "user")
}

/// Whether a computer-use thread is read-only, offline and never asks.
pub(crate) fn computer_response(result: &Value) -> bool {
    result["sandbox"]["type"] == "readOnly"
        && result["sandbox"]
            .get("networkAccess")
            .is_none_or(|value| value == false)
        && result["approvalPolicy"] == "never"
}

pub(crate) fn restricted_response(result: &Value) -> bool {
    result["sandbox"]["type"] == "readOnly"
        && result["sandbox"]
            .get("networkAccess")
            .is_none_or(|value| value == false)
        && result["approvalPolicy"] == "on-request"
        && result["approvalsReviewer"] == "user"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_read_only_acknowledgement_is_required() {
        let confirmed = json!({"sandbox":{"type":"readOnly","networkAccess":false},"approvalPolicy":"on-request","approvalsReviewer":"user"});
        assert!(restricted_response(&confirmed));
        for (key, value) in [
            ("sandbox", json!({"type":"workspaceWrite"})),
            ("sandbox", json!({"type":"readOnly","networkAccess":true})),
            ("approvalPolicy", json!("never")),
            ("approvalsReviewer", json!("auto_review")),
        ] {
            let mut response = confirmed.clone();
            response[key] = value;
            assert!(!restricted_response(&response));
        }
        assert!(!restricted_response(&json!({})));
    }

    #[test]
    fn work_requires_a_native_sandbox_and_the_person_as_reviewer() {
        let confirmed = json!({"sandbox":{"type":"workspaceWrite"},"approvalPolicy":"on-request","approvalsReviewer":"user"});
        assert!(asking_response(&confirmed));
        let mut read_only = confirmed.clone();
        read_only["sandbox"] = json!({"type":"readOnly"});
        assert!(asking_response(&read_only));
        let mut unreported = confirmed.clone();
        unreported
            .as_object_mut()
            .unwrap()
            .remove("approvalsReviewer");
        assert!(asking_response(&unreported));
        for (key, value) in [
            ("sandbox", json!({"type":"dangerFullAccess"})),
            (
                "sandbox",
                json!({"type":"externalSandbox","networkAccess":true}),
            ),
            ("sandbox", json!(null)),
            ("approvalPolicy", json!("never")),
            ("approvalsReviewer", json!("auto_review")),
        ] {
            let mut response = confirmed.clone();
            response[key] = value;
            assert!(!asking_response(&response), "{key}");
        }
    }
}
