// SPDX-License-Identifier: Apache-2.0
use super::*;

const EXACT: &str = "claude-opus-fixture";

#[test]
fn exact_computer_selection_preserves_hook_restrictions() {
    let (_, _, mut spec) = fixture();
    spec.model = Some(EXACT.into());
    spec.bridge.as_mut().unwrap().computer_use = true;
    let launch = Launch::build(&spec).unwrap();
    let settings: Vec<_> = launch
        .args
        .windows(2)
        .filter(|p| p[0] == "--settings")
        .collect();
    assert_eq!(settings.len(), 1);
    assert_eq!(
        serde_json::from_slice::<Value>(&std::fs::read(&settings[0][1]).unwrap()).unwrap(),
        json!({"disableAllHooks":true, "switchModelsOnFlag":false})
    );
}

#[test]
fn model_changes_are_checked_on_main_assistant_frames_only() {
    for frame in [
        json!({"type":"assistant", "message":{"model":"claude-other"}}),
        json!({"type":"stream_event", "event":{"type":"message_start", "message":{"model":"claude-other"}}}),
        json!({"type":"system", "subtype":"init"}),
        json!({"type":"system", "subtype":"model_consent_fallback", "fallback_model":"claude-other"}),
        json!({"type":"system", "subtype":"model_fallback"}),
    ] {
        assert!(model::check_event(Some(EXACT), &frame).is_err());
        assert!(model::check_event(None, &frame).is_ok());
        let mut child = frame;
        child["parent_tool_use_id"] = json!("child");
        assert!(model::check_event(Some(EXACT), &child).is_ok());
    }
}

fn exact_fixture() -> (tempfile::TempDir, ClaudeAdapter, RunSpec) {
    let (dir, adapter, mut spec) = fixture();
    spec.model = Some(EXACT.into());
    spec.prompt.text = "complete".into();
    std::fs::write(dir.path().join("models.json"), json!([
        {"value":"opus", "resolvedModel":EXACT, "supportsEffort":true, "supportedEffortLevels":["high"]}
    ]).to_string()).unwrap();
    std::fs::write(
        dir.path().join("applied-model.json"),
        json!(EXACT).to_string(),
    )
    .unwrap();
    (dir, adapter, spec)
}

async fn exit(run: &mut RunStarted) -> RunExit {
    loop {
        if let HarnessEvent::Exited { exit, .. } = event(&mut run.events).await {
            return exit;
        }
    }
}

fn frames(dir: &std::path::Path) -> Vec<Value> {
    std::fs::read_to_string(dir.join("stdin.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[tokio::test]
async fn exact_start_and_resume_verify_before_delivering_any_prompt() {
    for resume in [None, Some("prior-session".into())] {
        let (dir, adapter, mut spec) = exact_fixture();
        spec.resume_session = resume;
        let mut run = adapter.start_run(spec).await.unwrap();
        assert!(matches!(event(&mut run.events).await,
            HarnessEvent::SessionStarted { actual_model: Some(model), .. } if model == EXACT));
        assert_eq!(exit(&mut run).await, RunExit::Completed);
        let input = frames(dir.path());
        assert_eq!(input.len(), 3);
        assert_eq!(input[0]["request"]["subtype"], "initialize");
        assert_eq!(input[1]["request"]["subtype"], "get_settings");
        assert_eq!(input[2]["type"], "user");
        let args: Vec<String> =
            serde_json::from_slice(&std::fs::read(dir.path().join("argv.json")).unwrap()).unwrap();
        assert!(args.windows(2).any(|p| p == ["--fallback-model", EXACT]));
        assert!(
            args.windows(2)
                .any(|p| p[0] == "--settings" && p[1].ends_with(".json"))
        );
        assert_eq!(
            serde_json::from_slice::<Value>(
                &std::fs::read(dir.path().join("settings-content.json")).unwrap()
            )
            .unwrap()["switchModelsOnFlag"],
            false
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("no-model-fallback.json")).unwrap(),
            r#""1""#
        );
    }
}

#[tokio::test]
async fn exact_selection_blocks_consent_substitution_in_the_native_process() {
    let (dir, adapter, mut spec) = exact_fixture();
    spec.prompt.text = "consent-declined".into();
    let mut run = adapter.start_run(spec).await.unwrap();
    assert_eq!(
        exit(&mut run).await,
        RunExit::Failed {
            reason: "CLAUDE_CODE_NO_MODEL_FALLBACK forbids model substitution".into()
        }
    );
    assert!(!dir.path().join("substituted-model-executed").exists());
    assert_eq!(frames(dir.path())[2]["type"], "user");
}

#[tokio::test]
async fn exact_selection_rejects_fallback_notices_before_substituted_output() {
    for subtype in ["model_consent_fallback", "model_fallback"] {
        let (dir, adapter, spec) = exact_fixture();
        std::fs::write(dir.path().join("after-init.json"), json!([
            {"type":"system", "subtype":subtype, "choice":"cancelled",
             "original_model":EXACT, "fallback_model":"claude-other"},
            {"type":"assistant", "message":{"id":"substituted",
             "content":[{"type":"tool_use", "id":"wrong-model-tool", "name":"Bash", "input":{}}]}}
        ]).to_string()).unwrap();
        let mut run = adapter.start_run(spec).await.unwrap();
        loop {
            match event(&mut run.events).await {
                HarnessEvent::Activity { .. }
                | HarnessEvent::AgentMessage { .. }
                | HarnessEvent::AgentTextDelta { .. } => panic!("accepted substituted output"),
                HarnessEvent::Exited { exit, .. } => {
                    assert!(
                        matches!(exit, RunExit::Failed { reason } if reason.contains("model changed"))
                    );
                    break;
                }
                _ => {}
            }
        }
        assert_eq!(frames(dir.path())[2]["type"], "user");
    }
}

#[tokio::test]
async fn exact_selection_preserves_synthetic_refusal_warning_and_native_failure() {
    let (dir, adapter, spec) = exact_fixture();
    // The same native API-error envelope exercised by stream.rs's refusal fixture.
    let reason = "You've hit your session limit · resets 3pm";
    std::fs::write(dir.path().join("after-init.json"), json!([
        {"type":"assistant", "is_api_error_message":true,
         "message":{"id":"e", "model":"<synthetic>", "content":[{"type":"text", "text":reason}]}}
    ]).to_string()).unwrap();
    std::fs::write(
        dir.path().join("result.json"),
        json!({
            "type":"result", "subtype":"error_during_execution", "is_error":true, "errors":[reason]
        })
        .to_string(),
    )
    .unwrap();
    let mut run = adapter.start_run(spec).await.unwrap();
    let mut warned = false;
    loop {
        match event(&mut run.events).await {
            HarnessEvent::Warning { text } => {
                warned |= text == reason;
            }
            HarnessEvent::AgentMessage { .. } => panic!("native error became an agent reply"),
            HarnessEvent::Exited { exit, .. } => {
                assert_eq!(
                    exit,
                    RunExit::Failed {
                        reason: reason.into()
                    }
                );
                assert!(warned);
                break;
            }
            _ => {}
        }
    }
}

#[test]
fn synthetic_exception_does_not_hide_real_or_unknown_model_changes() {
    for (model, api_error, allowed) in [
        ("<synthetic>", true, true),
        ("<synthetic>", false, false),
        ("claude-other", true, false),
        ("<unknown>", true, false),
        (EXACT, false, true),
    ] {
        let frame = json!({"type":"assistant", "is_api_error_message":api_error,
            "message":{"model":model}});
        assert_eq!(
            model::check_event(Some(EXACT), &frame).is_ok(),
            allowed,
            "{frame}"
        );
    }
}

#[tokio::test]
async fn unavailable_mismatched_or_unverifiable_exact_models_never_receive_a_prompt() {
    for failure in [
        "unavailable",
        "unsupported",
        "missing",
        "mismatch",
        "fallback",
    ] {
        let (dir, adapter, spec) = exact_fixture();
        match failure {
            "unavailable" => std::fs::write(dir.path().join("models.json"), "[]").unwrap(),
            "unsupported" => std::fs::remove_file(dir.path().join("models.json")).unwrap(),
            "missing" => std::fs::remove_file(dir.path().join("applied-model.json")).unwrap(),
            "mismatch" => std::fs::write(
                dir.path().join("applied-model.json"),
                r#""claude-opus-other""#,
            )
            .unwrap(),
            "fallback" => std::fs::write(dir.path().join("force-fallback"), "").unwrap(),
            _ => unreachable!(),
        }
        let mut run = adapter.start_run(spec).await.unwrap();
        assert!(
            matches!(exit(&mut run).await, RunExit::Failed { .. }),
            "{failure}"
        );
        assert!(
            frames(dir.path())
                .iter()
                .all(|f| f["type"] == "control_request"),
            "{failure}"
        );
        assert!(!dir.path().join("fixture.pid").exists(), "{failure}");
    }
}

#[tokio::test]
async fn a_late_model_change_is_a_failed_run() {
    let (dir, adapter, spec) = exact_fixture();
    std::fs::write(
        dir.path().join("reported-model.json"),
        r#""claude-opus-other""#,
    )
    .unwrap();
    let mut run = adapter.start_run(spec).await.unwrap();
    assert!(
        matches!(exit(&mut run).await, RunExit::Failed { reason } if reason.contains("model changed"))
    );
}

#[tokio::test]
async fn force_stop_during_verification_does_not_deliver_initial_or_steered_input() {
    let (dir, adapter, spec) = exact_fixture();
    let listener = tokio::net::UnixListener::bind(dir.path().join("model-check.sock")).unwrap();
    let mut run = adapter.start_run(spec).await.unwrap();
    let _connection = boundary::gate(&listener).await;
    let control = run.control.clone();
    let send = control.send_message(
        MessageRef(protocol::CommandId::new()),
        PromptInput {
            text: "must stay held".into(),
            images: vec![],
        },
        MessageDelivery::Steer,
    );
    tokio::pin!(send);
    // Poll through channel admission, before Stop closes it.
    std::future::poll_fn(|cx| {
        assert!(send.as_mut().poll(cx).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
    run.control.close_admission();
    run.control.terminate().await.unwrap();
    assert_eq!(exit(&mut run).await, RunExit::Interrupted);
    assert!(matches!(send.await, Err(HarnessError::NotActive)));
    assert!(
        frames(dir.path())
            .iter()
            .all(|f| f["type"] == "control_request")
    );
}

#[tokio::test]
async fn ordinary_stop_during_verification_needs_no_native_turn() {
    let (dir, adapter, spec) = exact_fixture();
    let listener = tokio::net::UnixListener::bind(dir.path().join("model-check.sock")).unwrap();
    let mut run = adapter.start_run(spec).await.unwrap();
    let _connection = boundary::gate(&listener).await;
    run.control.close_admission();
    run.control.interrupt().await.unwrap();
    assert_eq!(exit(&mut run).await, RunExit::Interrupted);
    assert!(
        frames(dir.path())
            .iter()
            .all(|f| f["type"] == "control_request")
    );
}

#[tokio::test]
async fn stalled_verification_times_out_without_prompt_delivery() {
    let (dir, adapter, spec) = exact_fixture();
    let listener = tokio::net::UnixListener::bind(dir.path().join("model-check.sock")).unwrap();
    let mut run = adapter.start_run(spec).await.unwrap();
    let _connection = boundary::gate(&listener).await;
    tokio::time::pause();
    tokio::time::advance(model::CHECK_TIMEOUT).await;
    tokio::time::resume();
    assert!(matches!(exit(&mut run).await, RunExit::Failed { .. }));
    assert!(
        frames(dir.path())
            .iter()
            .all(|f| f["type"] == "control_request")
    );
}
