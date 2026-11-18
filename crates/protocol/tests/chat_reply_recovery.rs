// SPDX-License-Identifier: Apache-2.0
use protocol::{
    Command, RunId, TaskId, command::ObservedReply, event::LiveEvent, model::TimelineBody,
};
use serde::Deserialize;
use serde_json::json;

#[test]
fn old_messages_and_live_frames_do_not_invent_stream_positions() {
    let body: TimelineBody = serde_json::from_value(json!({
        "kind": "agent_message", "text": "é🌿 unfinished", "partial": true
    }))
    .unwrap();
    assert!(matches!(
        &body,
        TimelineBody::AgentMessage {
            message_key: None,
            text_offset: None,
            ..
        }
    ));
    let wire = serde_json::to_value(body).unwrap();
    assert!(wire.get("message_key").is_none() && wire.get("text_offset").is_none());
    let frame: LiveEvent = serde_json::from_value(json!({
        "kind": "agent_text_delta", "task_id": TaskId::new(), "run_id": RunId::new(),
        "message_key": "legacy-native-key", "text": "é🌿"
    }))
    .unwrap();
    assert!(matches!(
        frame,
        LiveEvent::AgentTextDelta {
            text_offset: None,
            ..
        }
    ));
}

#[test]
fn old_phone_message_shapes_ignore_new_optional_stream_metadata() {
    #[derive(Deserialize)]
    struct OldMessage {
        text: String,
        partial: bool,
    }
    #[derive(Deserialize)]
    struct OldDelta {
        message_key: String,
        text: String,
    }
    let body = TimelineBody::AgentMessage {
        text: "🌿".into(),
        partial: true,
        message_key: Some("generation-1".into()),
        text_offset: Some(2),
    };
    let old: OldMessage = serde_json::from_value(serde_json::to_value(&body).unwrap()).unwrap();
    assert_eq!(old.text, "🌿");
    assert!(old.partial);
    assert_eq!(
        serde_json::from_value::<TimelineBody>(serde_json::to_value(body).unwrap()).unwrap(),
        TimelineBody::AgentMessage {
            text: "🌿".into(),
            partial: true,
            message_key: Some("generation-1".into()),
            text_offset: Some(2)
        }
    );
    let delta = LiveEvent::AgentTextDelta {
        task_id: TaskId::new(),
        run_id: RunId::new(),
        message_key: "generation-1".into(),
        text: "🌿".into(),
        text_offset: Some(2),
    };
    let old: OldDelta = serde_json::from_value(serde_json::to_value(delta).unwrap()).unwrap();
    assert_eq!(old.message_key, "generation-1");
    assert_eq!(old.text, "🌿");
}

#[test]
fn old_resume_commands_omit_observation_and_new_observations_round_trip_separately() {
    let wire = json!({ "command": "resume_task", "task_id": TaskId::new(), "text": "Carry on" });
    let mut command: Command = serde_json::from_value(wire).unwrap();
    assert!(matches!(
        &command,
        Command::ResumeTask {
            observed_reply: None,
            ..
        }
    ));
    assert!(
        serde_json::to_value(&command)
            .unwrap()
            .get("observed_reply")
            .is_none()
    );
    if let Command::ResumeTask { observed_reply, .. } = &mut command {
        *observed_reply = Some(ObservedReply {
            run_id: RunId::new(),
            message_key: "generation-1".into(),
            text_offset: 2,
            text: "🌿 unfinished".into(),
        });
    }
    let wire = serde_json::to_value(&command).unwrap();
    assert_eq!(
        wire["text"], "Carry on",
        "observation is not silently appended to the person's words"
    );
    assert_eq!(wire["observed_reply"]["text_offset"], 2);
    assert_eq!(serde_json::from_value::<Command>(wire).unwrap(), command);
}

#[test]
fn offsets_are_unsigned_byte_positions_and_not_lossy_floats() {
    for invalid in [json!(-1), json!(1.5), json!("2")] {
        let wire = json!({ "kind": "agent_message", "text": "x", "partial": true,
            "message_key": "generation-1", "text_offset": invalid });
        assert!(serde_json::from_value::<TimelineBody>(wire).is_err());
    }
}
