// SPDX-License-Identifier: Apache-2.0
use protocol::{
    ClientFrame, Command, CommandEnvelope, Event, EventEnvelope, ProtocolVersion, QueryData, RunId,
    ServerFrame, TaskId,
    command::CommandStage,
    event::{EventSource, LiveEvent},
    model::Provenance,
    time::Timestamp,
};
use serde_json::json;
use ts_rs::TS;

#[test]
fn nested_frame_tags_and_optional_results_match_the_wire() {
    let live = ServerFrame::Live(LiveEvent::AgentTextDelta {
        task_id: TaskId::new(),
        run_id: RunId::new(),
        message_key: "fixture-message".into(),
        text: "fixture".into(),
    });
    let value = serde_json::to_value(&live).unwrap();
    assert_eq!(value["type"], "live");
    assert_eq!(value["kind"], "agent_text_delta");
    assert_eq!(serde_json::from_value::<ServerFrame>(value).unwrap(), live);
    assert!(LiveEvent::decl(&ts_rs::Config::from_env()).contains("\"kind\": \"agent_text_delta\""));

    let command = CommandEnvelope::new(Command::RunReadinessCheck);
    let event = ServerFrame::Event(EventEnvelope {
        seq: 7,
        at: Timestamp(0),
        source: EventSource::Companion,
        provenance: Provenance::Observed,
        event: Event::CommandProgress {
            command_id: command.command_id,
            stage: CommandStage::ReconciliationRequired,
            detail: None,
            result: None,
            error: None,
        },
    });
    let mut value = serde_json::to_value(&event).unwrap();
    assert_eq!(value["type"], "event");
    assert_eq!(value["event"]["type"], "command_progress");
    assert!(value["event"].get("result").is_none());
    assert!(value["event"]["detail"].is_null());
    value["event"]["future_optional_field"] = json!("ignored");
    assert_eq!(serde_json::from_value::<ServerFrame>(value).unwrap(), event);
    assert!(Event::decl(&ts_rs::Config::from_env()).contains("result?: CommandResult"));
    assert!(Event::decl(&ts_rs::Config::from_env()).contains("detail: string | null"));

    let status = QueryData::CommandStatus {
        receipt: None,
        stage: None,
        result: None,
        error: None,
    };
    assert_eq!(
        serde_json::to_value(&status).unwrap(),
        json!({"kind": "command_status", "receipt": null})
    );
    let schema = serde_json::to_value(schemars::schema_for!(QueryData)).unwrap();
    let status_schema = schema["oneOf"]
        .as_array()
        .unwrap()
        .iter()
        .find(|variant| variant["properties"]["kind"]["const"] == "command_status")
        .unwrap();
    assert!(
        !status_schema["required"]
            .as_array()
            .unwrap()
            .contains(&json!("stage"))
    );

    let frame = ClientFrame::Command(Box::new(command));
    let wire = serde_json::to_value(&frame).unwrap();
    assert_eq!(wire["type"], "command");
    assert_eq!(wire["body"]["command"], "run_readiness_check");
    assert_eq!(serde_json::from_value::<ClientFrame>(wire).unwrap(), frame);
}

#[test]
fn version_and_unknown_command_checks_fail_closed() {
    assert!(ProtocolVersion::CURRENT.compatible_with(&ProtocolVersion {
        major: 1,
        minor: 99
    }));
    assert!(!ProtocolVersion::CURRENT.compatible_with(&ProtocolVersion { major: 2, minor: 0 }));
    assert!(serde_json::from_value::<Command>(json!({"command": "future_effect"})).is_err());
    assert!(
        serde_json::from_value::<Command>(json!({
            "command": "create_task", "mode": "future_mode"
        }))
        .is_err()
    );
}

#[test]
fn journal_cursor_survives_the_generated_javascript_number_type() {
    let too_large = json!({"type": "subscribe", "after_seq": protocol::MAX_SAFE_INTEGER + 1});
    assert_eq!(
        protocol::integer::validate(&too_large).unwrap_err().code,
        protocol::ErrorCode::InvalidRequest
    );
    let maximum = json!({"type": "subscribe", "after_seq": protocol::MAX_SAFE_INTEGER});
    protocol::integer::validate(&maximum).unwrap();
    assert!(serde_json::from_value::<ClientFrame>(maximum).is_ok());
    assert!(ServerFrame::decl(&ts_rs::Config::from_env()).contains("head: number"));
    let original = ServerFrame::CaughtUp {
        head: protocol::MAX_SAFE_INTEGER,
    };
    let wire = serde_json::to_string(&original).unwrap();
    let decoded: serde_json::Value = serde_json::from_str(&wire).unwrap();
    // JavaScript JSON.parse stores numbers as IEEE-754 binary64. Re-encode
    // that value as a JSON number, as JSON.stringify does on the client.
    let javascript_head = decoded["head"].as_f64().unwrap();
    let echoed = format!(r#"{{"type":"caught_up","head":{javascript_head}}}"#);
    let back: ServerFrame = serde_json::from_str(&echoed).unwrap();
    assert_eq!(
        back, original,
        "the advertised number type loses the cursor"
    );
}

#[test]
fn schemas_bound_every_json_u64_including_annotation_tuples() {
    fn visit(value: &serde_json::Value, count: &mut usize) {
        match value {
            serde_json::Value::Object(fields) => {
                if fields.get("format").and_then(|v| v.as_str()) == Some("uint64") {
                    assert_eq!(
                        fields.get("maximum"),
                        Some(&json!(protocol::MAX_SAFE_INTEGER))
                    );
                    *count += 1;
                }
                for value in fields.values() {
                    visit(value, count);
                }
            }
            serde_json::Value::Array(values) => {
                for value in values {
                    visit(value, count);
                }
            }
            _ => {}
        }
    }
    let mut count = 0;
    for schema in [
        schemars::schema_for!(ClientFrame),
        schemars::schema_for!(ServerFrame),
        schemars::schema_for!(protocol::media::MediaClientFrame),
        schemars::schema_for!(protocol::terminal::TerminalClientFrame),
        schemars::schema_for!(protocol::transfer::TransferClientFrame),
    ] {
        visit(&serde_json::to_value(schema).unwrap(), &mut count);
    }
    assert!(count > 20, "schema coverage unexpectedly missing: {count}");
}

#[test]
fn delivery_targets_a_message_with_a_safe_numeric_sequence() {
    use protocol::model::{DeliveryState, TimelineBody};
    let delivery = TimelineBody::Delivery {
        message_seq: protocol::MAX_SAFE_INTEGER,
        state: DeliveryState::NativeDelivered,
        withdrawn_by_stop: false,
        withdrawn_by_revocation: false,
    };
    let wire = json!({"kind": "delivery", "message_seq": protocol::MAX_SAFE_INTEGER, "state": "native_delivered"});
    assert_eq!(serde_json::to_value(&delivery).unwrap(), wire);
    assert_eq!(
        serde_json::from_value::<TimelineBody>(wire).unwrap(),
        delivery
    );
    assert!(TimelineBody::decl(&ts_rs::Config::from_env()).contains("message_seq: number"));
}

#[test]
fn terminal_directory_is_additive_to_legacy_open() {
    let legacy = serde_json::json!({"command":"open_terminal", "project_id":null, "harness":null, "cols":80, "rows":24});
    let command: protocol::Command = serde_json::from_value(legacy.clone()).unwrap();
    assert_eq!(serde_json::to_value(command).unwrap(), legacy);
    let mut with_folder = legacy;
    with_folder["cwd"] = serde_json::json!("/work/project/subfolder");
    let command: protocol::Command = serde_json::from_value(with_folder.clone()).unwrap();
    assert_eq!(serde_json::to_value(command).unwrap(), with_folder);
}

#[test]
fn internal_picture_download_marker_is_additive() {
    let old = json!({"command":"create_download", "source":{"kind":"artifact", "artifact_id":protocol::ArtifactId::new()}});
    let command: Command = serde_json::from_value(old.clone()).unwrap();
    assert_eq!(serde_json::to_value(command).unwrap(), old);
    let mut marked = old;
    marked["shown_in_task"] = json!(true);
    let command: Command = serde_json::from_value(marked.clone()).unwrap();
    assert_eq!(serde_json::to_value(command).unwrap(), marked);
}

#[test]
fn inbox_folder_is_additive_and_legacy_transfers_keep_their_path() {
    use protocol::transfer::TransferRecord;
    let value: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/inbox-transfer.json")).unwrap();
    let record: TransferRecord = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(serde_json::to_value(record).unwrap(), value);
    let mut old = value;
    old.as_object_mut().unwrap().remove("destination_folder");
    old["host_path"] = json!("/private/db/inbox/note.txt");
    let record: TransferRecord = serde_json::from_value(old.clone()).unwrap();
    assert!(record.destination_folder.is_none());
    assert_eq!(serde_json::to_value(record).unwrap(), old);
    assert!(
        protocol::model::HostInfo::decl(&ts_rs::Config::default()).contains("inbox_path?: string")
    );
}

#[test]
fn old_children_do_not_claim_a_process_lifetime() {
    let child: protocol::model::ChildAgent = serde_json::from_value(json!({
        "child_id": "00000000-0000-4000-8000-000000000001", "native_ref": null,
        "label": "Older helper", "state": "active", "started_at": 1,
        "ended_at": null, "provenance": "native"
    }))
    .unwrap();
    assert_eq!(child.lifetime, None);
    assert!(
        serde_json::to_value(child)
            .unwrap()
            .get("lifetime")
            .is_none()
    );
}

#[test]
fn contribution_recovery_fields_accept_older_receipts_and_commands() {
    use protocol::contribution::*;
    let old_view = json!({
        "operation_id":"operation", "code":"workspace_conflict",
        "packet_sha256":"a".repeat(64), "handoff_sha256":"b".repeat(64),
        "context":null, "project_id":null, "dirty":null,
        "interruption_pending":false, "execution_authorized":false
    });
    let view: ContributionView = serde_json::from_value(old_view).unwrap();
    assert!(view.conflict.is_none());
    assert!(view.space.is_none());
    assert!(view.received_bytes.is_none());
    assert!(view.missing_capabilities.is_empty());
    let approval = json!({"publisher_id":"fixture", "packet_sha256":"a".repeat(64),
        "handoff_sha256":"b".repeat(64), "game_id":"fixture", "release_id":"one",
        "repository_url":"https://source.example/owned/fixture", "commit":"c".repeat(40),
        "checked_at":1, "valid_until":2});
    let command: protocol::Command =
        serde_json::from_value(json!({"command":"prepare_contribution",
        "operation_id":"operation", "approval":approval}))
        .unwrap();
    let protocol::Command::PrepareContribution {
        approval,
        workspace_choice,
        ..
    } = command
    else {
        panic!()
    };
    assert!(workspace_choice.is_none());
    assert!(approval.display_name.is_none());
}
