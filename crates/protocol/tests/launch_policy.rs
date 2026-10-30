// SPDX-License-Identifier: Apache-2.0
use protocol::{Command, ProjectId, capability::HarnessKind, model::LaunchPolicy};
use serde_json::json;

#[test]
fn legacy_commands_keep_omission_and_new_choices_round_trip() {
    let legacy = json!({
        "command":"create_task", "project_id":ProjectId::new(), "kind":"coding",
        "title":null, "prompt":"Inspect the parser", "bundle":null,
        "harness":"claude_code", "model":null, "effort":null, "mission":null,
        "target_id":null, "placement":{"mode":"start_now"}
    });
    let command: Command = serde_json::from_value(legacy.clone()).unwrap();
    assert!(matches!(
        command,
        Command::CreateTask {
            launch_policy: None,
            ..
        }
    ));
    assert!(
        serde_json::to_value(command)
            .unwrap()
            .get("launch_policy")
            .is_none()
    );
    for (policy, name) in [
        (LaunchPolicy::Standard, "standard"),
        (LaunchPolicy::BypassPermissions, "bypass_permissions"),
        (LaunchPolicy::AcceptEdits, "accept_edits"),
        (LaunchPolicy::Sandboxed, "sandboxed"),
    ] {
        let mut wire = legacy.clone();
        wire["launch_policy"] = json!(name);
        let command: Command = serde_json::from_value(wire).unwrap();
        assert!(
            matches!(command, Command::CreateTask { launch_policy: Some(p), .. } if p == policy)
        );
        assert_eq!(
            serde_json::to_value(command).unwrap()["launch_policy"],
            name
        );
    }
    let mut unknown = legacy;
    unknown["launch_policy"] = json!("--arbitrary-command");
    assert!(serde_json::from_value::<Command>(unknown).is_err());
}

#[test]
fn launch_policies_are_agent_specific_not_arbitrary_arguments() {
    use HarnessKind::*;
    for kind in [ClaudeCode, Codex, Pi, Hermes, OpenClaw] {
        assert_eq!(
            LaunchPolicy::BypassPermissions.supports(kind),
            matches!(kind, ClaudeCode | Codex)
        );
        assert_eq!(
            LaunchPolicy::Standard.supports(kind),
            matches!(kind, ClaudeCode | Codex)
        );
        assert_eq!(LaunchPolicy::AcceptEdits.supports(kind), kind == ClaudeCode);
        assert_eq!(LaunchPolicy::Sandboxed.supports(kind), kind == Codex);
    }
}
