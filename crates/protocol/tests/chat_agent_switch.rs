// SPDX-License-Identifier: Apache-2.0
use protocol::{Command, TaskId, capability::HarnessKind};

#[test]
fn old_chat_messages_keep_their_choices_and_new_agent_choices_round_trip() {
    let id = TaskId::new();
    for mut value in [
        serde_json::json!({"command":"resume_task", "task_id":id, "text":"hello"}),
        serde_json::json!({"command":"send_message", "task_id":id, "text":"hello", "bundle":null, "delivery":"next_turn"}),
    ] {
        let old: Command = serde_json::from_value(value.clone()).unwrap();
        match old {
            Command::ResumeTask {
                harness,
                model,
                effort,
                ..
            }
            | Command::SendMessage {
                harness,
                model,
                effort,
                ..
            } => assert_eq!((harness, model, effort), (None, None, None)),
            _ => unreachable!(),
        }
        value["harness"] = serde_json::json!("codex");
        value["model"] = serde_json::json!("synthetic-model");
        let chosen: Command = serde_json::from_value(value).unwrap();
        match &chosen {
            Command::ResumeTask {
                harness,
                model,
                effort,
                ..
            }
            | Command::SendMessage {
                harness,
                model,
                effort,
                ..
            } => {
                assert_eq!(*harness, Some(HarnessKind::Codex));
                assert_eq!(model.as_deref(), Some("synthetic-model"));
                assert!(effort.is_none());
            }
            _ => unreachable!(),
        }
        let wire = serde_json::to_value(&chosen).unwrap();
        assert_eq!(serde_json::from_value::<Command>(wire).unwrap(), chosen);
    }
}
