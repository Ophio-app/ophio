// SPDX-License-Identifier: Apache-2.0
#![cfg(unix)]
use super::*;
mod background;
mod boundary;
mod containment;
mod launch;
mod models;
mod sandbox;
use ports::bridge::*;
use protocol::{
    ids::{RunId, TaskId},
    model::{ActivityStatus, ChildState, UsageSummary},
};

/// What a run is told, ending with how it talks to the person through Ophio
/// and answers to them.
fn told(spec: &RunSpec, before: Option<&str>) -> String {
    let person = format!(
        "{}\n\n{}",
        ports::harness::person_instruction(spec).unwrap(),
        ports::harness::conduct_instruction(spec).unwrap()
    );
    match before {
        Some(text) => format!("{text}\n\n{person}"),
        None => person,
    }
}
/// What a Work run is told about talking to the person and answering to
/// them rides on every request it makes, so what grows here is paid again
/// on every turn of every task. Questions off is the longest it gets. The
/// line sending what the person has to do to the checklist took it past
/// 5,000, and the one on running checks so their exit status says whether
/// they passed past 5,200.
#[test]
fn what_a_run_is_told_stays_within_its_budget() {
    let (_dir, _adapter, mut spec) = fixture();
    spec.questions = false;
    let told = told(&spec, None);
    assert!(told.len() <= 5_300, "{} characters", told.len());
}
pub(super) fn fixture() -> (tempfile::TempDir, ClaudeAdapter, RunSpec) {
    let dir = tempfile::tempdir().unwrap();
    let binary = dir.path().join("fake-claude");
    test_support::executable(&binary, include_str!("../tests/fake_claude.py")).unwrap();
    let spec = RunSpec {
        mode: protocol::model::TaskMode::Work,
        run_id: RunId::new(),
        task_id: TaskId::new(),
        cwd: dir.path().to_owned(),
        prompt: PromptInput {
            text: "start".into(),
            images: vec![],
        },
        model: Some("requested-model".into()),
        effort: Some("high".into()),
        launch_policy: None,
        resume_session: Some("old-session".into()),
        resume_from_mode: None,
        mission: None,
        branch: None,
        start: None,
        bridge: Some(ToolBridge {
            server_name: "computer".into(),
            command: "/fixture/computer-mcp".into(),
            args: vec![],
            env: vec![
                (RUN_TOKEN_ENV.into(), "fixture-secret".into()),
                (SOCKET_ENV.into(), "/fixture/socket".into()),
            ],
            permission_tool: Some("permission_prompt".into()),
            computer_use: false,
            browser: false,
            github: false,
            checklist_answers: false,
        }),
        project_settings: false,
        env: Default::default(),
        github: None,
        scratch: None,
        questions: true,
        sandbox: None,
        web: false,
        disabled: Default::default(),
    };
    (dir, ClaudeAdapter::with_binary(binary), spec)
}
async fn event(rx: &mut mpsc::Receiver<HarnessEvent>) -> HarnessEvent {
    tokio::time::timeout(Duration::from_secs(3), rx.recv())
        .await
        .unwrap()
        .unwrap()
}
#[tokio::test]
async fn replay_stream_followup_usage_and_resume() {
    let (dir, adapter, spec) = fixture();
    let probe = adapter.probe().await;
    assert!(matches!(probe.account, AccountReadiness::SignedIn { .. }));
    assert!(probe.native_version.unwrap().contains("2.1.280"));
    let mut run = adapter.start_run(spec).await.unwrap();
    assert!(
        matches!(event(&mut run.events).await,HarnessEvent::SessionStarted { actual_model:Some(ref m),.. } if m=="native-model")
    );
    let mut streamed = false;
    let mut activity = false;
    let mut child = false;
    loop {
        match event(&mut run.events).await {
            HarnessEvent::AgentTextDelta { text, .. } => {
                assert_eq!(text, "Hello");
                streamed = true;
            }
            HarnessEvent::Activity {
                status: ActivityStatus::Succeeded,
                ..
            } => activity = true,
            HarnessEvent::Child {
                state: ChildState::Active,
                ..
            } => child = true,
            HarnessEvent::Warning { .. } => break,
            _ => {}
        }
    }
    assert!(streamed && activity && child);
    let state = run
        .control
        .send_message(
            MessageRef(protocol::CommandId::new()),
            PromptInput {
                text: "follow-up".into(),
                images: vec![],
            },
            MessageDelivery::Steer,
        )
        .await
        .unwrap();
    assert_eq!(state, DeliveryState::HostAccepted);
    assert!(matches!(
        event(&mut run.events).await,
        HarnessEvent::MessageDelivery {
            state: DeliveryState::HostAccepted,
            ..
        }
    ));
    assert!(matches!(
        event(&mut run.events).await,
        HarnessEvent::MessageDelivery {
            state: DeliveryState::NativeDelivered,
            ..
        }
    ));
    let mut results_seen = false;
    loop {
        match event(&mut run.events).await {
            HarnessEvent::Results { .. } => results_seen = true,
            HarnessEvent::TurnCompleted {
                usage:
                    Some(UsageSummary {
                        input_tokens: Some(12),
                        output_tokens: Some(4),
                        ..
                    }),
            } => break,
            event => panic!("unexpected event before completion: {event:?}"),
        }
    }
    assert!(results_seen);
    // The fixture's Task call never returned before its turn ended.
    assert!(matches!(
        event(&mut run.events).await,
        HarnessEvent::Child { native_ref, label, state: ChildState::Unknown, .. }
            if native_ref == "c1" && label == "Fixture child"
    ));

    assert!(matches!(
        event(&mut run.events).await,
        HarnessEvent::Exited {
            exit: RunExit::Completed,
            ..
        }
    ));
    assert!(run.events.recv().await.is_none());
    let args: Vec<String> =
        serde_json::from_slice(&std::fs::read(dir.path().join("argv.json")).unwrap()).unwrap();
    assert!(args.windows(2).any(|w| w == ["--resume", "old-session"]));
    assert!(args.windows(2).any(|w| w == ["--effort", "high"]));
    assert!(args.windows(2).any(|w| w == ["--model", "requested-model"]));
    assert!(args.windows(2).any(|w| w
        == [
            "--permission-prompt-tool",
            "mcp__computer__permission_prompt"
        ]));
    // The model never sees the tool that answers permission prompts.
    assert!(
        args.windows(2)
            .any(|w| w == ["--disallowedTools", "mcp__computer__permission_prompt"])
    );
    assert!(!args.join(" ").contains("fixture-secret"));
    // Only the bridge gets its credentials; the agent's commands never see them.
    let config: Value =
        serde_json::from_slice(&std::fs::read(dir.path().join("mcp.json")).unwrap()).unwrap();
    assert_eq!(
        config["mcpServers"]["computer"]["env"],
        json!({RUN_TOKEN_ENV: "fixture-secret", SOCKET_ENV: "/fixture/socket"})
    );
    let environment: Vec<String> =
        serde_json::from_slice(&std::fs::read(dir.path().join("environ.json")).unwrap()).unwrap();
    assert!(
        !environment
            .iter()
            .any(|name| name == RUN_TOKEN_ENV || name == SOCKET_ENV)
    );
    // The project's own settings wait for the owner's choice; its CLAUDE.md does not.
    assert!(args.windows(2).any(|w| w == ["--setting-sources", "user"]));
    assert!(
        environment
            .iter()
            .any(|name| name == "CLAUDE_CODE_ADDITIONAL_DIRECTORIES_CLAUDE_MD")
    );
    let stdin = std::fs::read_to_string(dir.path().join("stdin.jsonl")).unwrap();
    let last: Value = serde_json::from_str(stdin.lines().last().unwrap()).unwrap();
    assert_eq!(last["message"]["content"][0]["text"], "follow-up");
}
#[tokio::test]
async fn stop_discards_a_held_followup() {
    let (_dir, adapter, spec) = fixture();
    let mut run = adapter.start_run(spec).await.unwrap();
    while !matches!(event(&mut run.events).await, HarnessEvent::Warning { .. }) {}
    assert_eq!(
        run.control
            .send_message(
                MessageRef(protocol::CommandId::new()),
                PromptInput {
                    text: "later".into(),
                    images: vec![]
                },
                MessageDelivery::NextTurn
            )
            .await
            .unwrap(),
        DeliveryState::QueuedForBoundary
    );
    run.control.interrupt().await.unwrap();
    let mut turns = 0;
    let mut helper = None;
    loop {
        match event(&mut run.events).await {
            HarnessEvent::TurnCompleted { .. } => turns += 1,
            HarnessEvent::Child {
                native_ref, state, ..
            } if native_ref == "c1" => helper = Some(state),
            HarnessEvent::MessageDelivery { state, .. } => {
                assert_eq!(state, DeliveryState::Cancelled, "Stop replayed queued work")
            }
            HarnessEvent::Exited {
                exit: RunExit::Interrupted,
                ..
            } => break,
            _ => {}
        }
    }
    assert_eq!(turns, 1);
    // The Task helper open when Stop arrived ended with its turn.
    assert_eq!(helper, Some(ChildState::Stopped));
}
#[tokio::test]
async fn images_too_large_for_one_message_are_refused_without_ending_the_run() {
    let (dir, adapter, spec) = fixture();
    let mut run = adapter.start_run(spec).await.unwrap();
    while !matches!(event(&mut run.events).await, HarnessEvent::Warning { .. }) {}
    // Each image is within the per-image limit; together they are not.
    let mut images = Vec::new();
    for name in ["one.png", "two.png"] {
        std::fs::write(dir.path().join(name), vec![0u8; 13 * 1024 * 1024]).unwrap();
        images.push(ImageInput {
            path: name.into(),
            media_type: "image/png".into(),
        });
    }
    let refused = run
        .control
        .send_message(
            MessageRef(protocol::CommandId::new()),
            PromptInput {
                text: "look at these".into(),
                images,
            },
            MessageDelivery::Steer,
        )
        .await
        .unwrap_err();
    assert!(
        matches!(refused, HarnessError::Unsupported(ref why) if why.contains("too large")),
        "{refused:?}"
    );
    run.control.interrupt().await.unwrap();
    loop {
        match event(&mut run.events).await {
            HarnessEvent::MessageDelivery { .. } => panic!("the refused message was never written"),
            HarnessEvent::Exited { exit, .. } => {
                assert_eq!(exit, RunExit::Interrupted);
                break;
            }
            _ => {}
        }
    }
}
#[tokio::test]
async fn steer_echo_precedes_queued_followup_with_its_own_reference() {
    use tokio::{io::AsyncWriteExt, net::UnixListener};

    let (dir, adapter, spec) = fixture();
    let socket = dir.path().join("turn.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let mut run = adapter.start_run(spec).await.unwrap();
    loop {
        match event(&mut run.events).await {
            HarnessEvent::Warning { .. } => break,
            HarnessEvent::MessageDelivery { .. } => {
                panic!("initial/resume prompt has no reference")
            }
            _ => {}
        }
    }
    let a = MessageRef(protocol::CommandId::new());
    let b = MessageRef(protocol::CommandId::new());
    for (message_ref, text, delivery, expected) in [
        (
            a,
            "queued A",
            MessageDelivery::NextTurn,
            DeliveryState::QueuedForBoundary,
        ),
        (
            b,
            "steer B",
            MessageDelivery::Steer,
            DeliveryState::HostAccepted,
        ),
    ] {
        assert_eq!(
            run.control
                .send_message(
                    message_ref,
                    PromptInput {
                        text: text.into(),
                        images: vec![]
                    },
                    delivery
                )
                .await
                .unwrap(),
            expected
        );
    }
    // The fixture echoes B, then holds its result so A cannot be written yet.
    let mut boundary = boundary::gate(&listener).await;
    for state in [DeliveryState::HostAccepted, DeliveryState::NativeDelivered] {
        assert_eq!(
            event(&mut run.events).await,
            HarnessEvent::MessageDelivery {
                message_ref: b,
                state
            }
        );
    }
    std::fs::remove_file(socket).unwrap();
    boundary.write_all(b"x").await.unwrap();
    let mut updates = Vec::new();
    loop {
        match event(&mut run.events).await {
            HarnessEvent::MessageDelivery { message_ref, state } => {
                updates.push((message_ref, state))
            }
            HarnessEvent::Exited { exit, .. } => {
                assert_eq!(exit, RunExit::Completed);
                break;
            }
            _ => {}
        }
    }
    assert_eq!(
        updates,
        vec![
            (a, DeliveryState::HostAccepted),
            (a, DeliveryState::NativeDelivered)
        ]
    );
    let frames: Vec<Value> = std::fs::read_to_string(dir.path().join("stdin.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let texts: Vec<_> = frames
        .iter()
        .filter(|v| v["type"] == "user")
        .map(|v| v["message"]["content"][0]["text"].as_str().unwrap())
        .collect();
    assert_eq!(texts, vec!["start", "steer B", "queued A"]);
}

#[tokio::test]
async fn nonzero_exit_is_failed() {
    let (_dir, adapter, mut spec) = fixture();
    spec.prompt.text = "crash".into();
    let mut run = adapter.start_run(spec).await.unwrap();
    assert!(matches!(
        event(&mut run.events).await,
        HarnessEvent::Exited {
            exit: RunExit::Failed { .. },
            ..
        }
    ));
}
#[tokio::test]
async fn image_blocks_contain_frozen_bytes() {
    let (dir, _, spec) = fixture();
    std::fs::write(dir.path().join("image.png"), [1, 2, 3]).unwrap();
    let input = PromptInput {
        text: "image".into(),
        images: vec![ImageInput {
            path: "image.png".into(),
            media_type: "image/png".into(),
        }],
    };
    let value = prompt(&input, &spec.cwd, "id").await.unwrap();
    assert_eq!(value["message"]["content"][1]["source"]["data"], "AQID");
}
#[tokio::test]
async fn api_account_never_dispatches_inference() {
    let (dir, adapter, spec) = fixture();
    std::fs::write(dir.path().join("api-auth"), "fixture").unwrap();
    assert!(matches!(
        adapter.start_run(spec).await,
        Err(HarnessError::AccountNotReady(_))
    ));
    assert!(!dir.path().join("argv.json").exists());
}

#[tokio::test]
async fn modes_probe_and_launch_enforce_read_tools_on_start_and_resume() {
    for mode in [TaskMode::Ask, TaskMode::Plan, TaskMode::Work] {
        for resume in [None, Some("continued-session".to_owned())] {
            let (dir, adapter, mut spec) = fixture();
            spec.mode = mode;
            if mode != TaskMode::Work {
                spec.launch_policy = Some(protocol::model::LaunchPolicy::BypassPermissions);
            }
            spec.resume_session = resume.clone();
            if mode != TaskMode::Work {
                spec.bridge = None;
            }
            let info = adapter.probe().await;
            assert_eq!(info.modes.len(), 3);
            assert!(
                info.modes
                    .iter()
                    .all(|s| s.availability.is_available() && !s.enforcement.is_empty())
            );
            let told = told(&spec, ports::harness::task_mode_instruction(mode));
            let mut run = adapter.start_run(spec).await.unwrap();
            while !matches!(
                event(&mut run.events).await,
                HarnessEvent::SessionStarted { .. }
            ) {}
            run.control
                .send_message(
                    MessageRef(protocol::CommandId::new()),
                    PromptInput {
                        text: "follow-up".into(),
                        images: vec![],
                    },
                    MessageDelivery::Steer,
                )
                .await
                .unwrap();
            while !matches!(
                event(&mut run.events).await,
                HarnessEvent::TurnCompleted { .. }
            ) {}
            run.control.shutdown().await.unwrap();
            let frames: Vec<Value> = std::fs::read_to_string(dir.path().join("stdin.jsonl"))
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
            assert_eq!(
                frames.last().unwrap()["message"]["content"][0]["text"],
                "follow-up"
            );
            let args: Vec<String> =
                serde_json::from_slice(&std::fs::read(dir.path().join("argv.json")).unwrap())
                    .unwrap();
            let mut expected: Vec<String> = [
                "--print",
                "--output-format",
                "stream-json",
                "--input-format",
                "stream-json",
                "--verbose",
                "--include-partial-messages",
                "--replay-user-messages",
                "--permission-mode",
                if mode == TaskMode::Work {
                    "manual"
                } else {
                    "plan"
                },
                "--model",
                "requested-model",
                "--effort",
                "high",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect();
            if let Some(session) = &resume {
                expected.extend(["--resume".into(), session.clone()]);
            }
            if mode == TaskMode::Work {
                expected.extend([
                    "--setting-sources".into(),
                    "user".into(),
                    "--add-dir".into(),
                    dir.path().to_str().unwrap().into(),
                ]);
                expected.extend(
                    [
                        "--mcp-config",
                        "<config>",
                        "--permission-prompt-tool",
                        "mcp__computer__permission_prompt",
                        "--disallowedTools",
                        "mcp__computer__permission_prompt",
                        "--allowedTools",
                    ]
                    .into_iter()
                    .map(str::to_owned),
                );
                let allowed: Vec<_> = (ports::bridge::PERSON_TOOLS.iter())
                    .chain(ports::bridge::WORK_TOOLS)
                    .map(|tool| format!("mcp__computer__{tool}"))
                    .collect();
                expected.push(allowed.join(","));
            } else {
                expected.extend(
                    [
                        "--system-prompt-snapshot",
                        "off",
                        "--tools",
                        "Read,Glob,Grep",
                        "--safe-mode",
                        "--strict-mcp-config",
                        "--mcp-config",
                        "<config>",
                        "--permission-prompts",
                        "none",
                    ]
                    .into_iter()
                    .map(str::to_owned),
                );
                if mode == TaskMode::Ask {
                    expected.extend(["--disallowedTools".into(), "ExitPlanMode".into()]);
                }
            }
            expected.extend(["--append-system-prompt".into(), told.clone()]);
            let normalized: Vec<_> = args
                .iter()
                .enumerate()
                .map(|(i, arg)| {
                    if i > 0 && args[i - 1] == "--mcp-config" {
                        "<config>".to_owned()
                    } else {
                        arg.clone()
                    }
                })
                .collect();
            assert_eq!(normalized, expected);
            let framing: Vec<_> = args
                .windows(2)
                .filter(|pair| pair[0] == "--append-system-prompt")
                .map(|pair| pair[1].as_str())
                .collect();
            assert_eq!(framing, [told.as_str()]);
            assert_eq!(
                args.windows(2)
                    .any(|pair| pair == ["--disallowedTools", "ExitPlanMode"]),
                mode == TaskMode::Ask
            );
            assert_eq!(
                args.windows(2)
                    .any(|pair| pair == ["--system-prompt-snapshot", "off"]),
                mode != TaskMode::Work
            );
            let permission = if mode == TaskMode::Work {
                "manual"
            } else {
                "plan"
            };
            assert!(
                args.windows(2)
                    .any(|pair| pair == ["--permission-mode", permission])
            );
            assert_eq!(args.iter().any(|arg| arg == "--resume"), resume.is_some());
            if mode != TaskMode::Work {
                assert!(
                    args.windows(2)
                        .any(|pair| pair == ["--tools", "Read,Glob,Grep"])
                );
                assert!(
                    args.windows(2)
                        .any(|pair| pair == ["--permission-prompts", "none"])
                );
                assert!(args.contains(&"--safe-mode".into()));
                assert!(args.contains(&"--strict-mcp-config".into()));
                assert!(!args.contains(&"--permission-prompt-tool".into()));
                let config: Value =
                    serde_json::from_slice(&std::fs::read(dir.path().join("mcp.json")).unwrap())
                        .unwrap();
                assert_eq!(config, json!({"mcpServers":{}}));
            } else {
                assert!(!args.contains(&"--tools".into()));
                assert!(args.contains(&"--permission-prompt-tool".into()));
            }
        }
    }
}

#[tokio::test]
async fn a_probe_reads_the_help_once_for_modes_models_and_delegation() {
    let (dir, adapter, _) = fixture();
    let info = adapter.probe().await;
    assert_eq!(info.modes.len(), 3);
    assert!(!info.models.is_empty());
    assert!(info.capabilities.computer_use_delegation.is_available());
    let calls = std::fs::read_to_string(dir.path().join("help-calls")).unwrap();
    assert_eq!(calls.lines().count(), 1, "{calls}");
}

#[tokio::test]
async fn models_are_the_ones_claude_code_lists_and_its_help_without_a_list() {
    let (dir, adapter, _) = fixture();
    let info = adapter.probe().await;
    let models: Vec<_> = info
        .models
        .iter()
        .take(4)
        .map(|m| (m.id.as_str(), m.label.as_str(), m.is_default))
        .collect();
    assert_eq!(
        models,
        [
            ("opus", "Claude Opus 5.5", true),
            (
                "claude-fable-5-1[1m]",
                "Claude Fable 5.1 (1M context)",
                false
            ),
            ("sonnet", "Claude Sonnet 5.5", false),
            ("haiku", "Claude Haiku 5.5", false),
        ]
    );
    let haiku = info.models[3].details.as_ref().unwrap();
    assert!(haiku.alias && haiku.efforts.is_empty());
    assert_eq!(info.models[0].details.as_ref().unwrap().efforts.len(), 5);
    let catalog = info.catalog.unwrap();
    assert!(catalog.source.contains("model list"));
    assert_eq!(catalog.default_model.as_deref(), Some("opus"));
    // Asked with nothing that runs: no hooks, servers or saved session.
    let calls = std::fs::read_to_string(dir.path().join("list-calls")).unwrap();
    let argv: Vec<String> = serde_json::from_str(calls.lines().next().unwrap()).unwrap();
    for flag in ["--strict-mcp-config", "--no-session-persistence", "-p"] {
        assert!(argv.contains(&flag.to_owned()), "{argv:?}");
    }
    std::fs::write(dir.path().join("no-model-list"), "").unwrap();
    let info = adapter.probe().await;
    assert!(info.models.iter().any(|m| m.id == "claude-fable-5"));
    assert!(!info.models.iter().any(|m| m.id == "haiku"));
    assert!(info.catalog.unwrap().source.contains("help"));
}

#[tokio::test]
async fn native_catalog_exposes_verified_exact_model_without_changing_default() {
    let (dir, adapter, _) = fixture();
    std::fs::write(
        dir.path().join("models.json"),
        json!([
            {"value":"opus", "resolvedModel":"claude-opus-fixture", "supportsEffort":true, "supportedEffortLevels":["high"]},
            {"value":"default", "resolvedModel":"claude-opus-fixture"}
        ]).to_string(),
    ).unwrap();
    let info = adapter.probe().await;
    let exact = info
        .models
        .iter()
        .find(|m| m.id == "claude-opus-fixture")
        .expect("the installed CLI's resolved ID must be selectable");
    assert!(!exact.details.as_ref().unwrap().alias);
    assert_eq!(exact.details.as_ref().unwrap().efforts[0].id, "high");
    assert!(!exact.is_default);
    assert!(info.models.iter().any(|m| m.id == "opus" && m.is_default));
}

#[tokio::test]
async fn missing_plan_flag_is_unsupported_and_never_launches_inference() {
    let (dir, adapter, mut spec) = fixture();
    std::fs::write(dir.path().join("no-plan"), "").unwrap();
    let info = adapter.probe().await;
    for support in info.modes.iter().filter(|s| s.mode != TaskMode::Work) {
        assert!(
            matches!(&support.availability, Availability::Unsupported { reason } if reason.contains("leave files unchanged"))
        );
        spec.mode = support.mode;
        assert!(matches!(
            adapter.start_run(spec.clone()).await,
            Err(HarnessError::Unsupported(_))
        ));
    }
    assert!(!dir.path().join("argv.json").exists());
}

#[tokio::test]
async fn plan_tool_content_is_reply_text_and_cannot_approve_writing() {
    let (dir, adapter, mut spec) = fixture();
    spec.mode = TaskMode::Plan;
    spec.bridge = None;
    spec.prompt.text = "plan-reply".into();
    let mut run = adapter.start_run(spec).await.unwrap();
    let mut plan = None;
    let mut proposal = None;
    loop {
        match event(&mut run.events).await {
            HarnessEvent::AgentMessage { text, .. } => plan = Some(text),
            HarnessEvent::Activity {
                summary,
                status,
                detail,
                ..
            } => {
                assert_eq!(summary, "Plan proposed");
                assert_eq!(status, ActivityStatus::Succeeded);
                proposal = detail;
            }
            HarnessEvent::TurnCompleted { .. } => break,
            HarnessEvent::ApprovalRequested(_) => panic!("plan cannot request a writing grant"),
            HarnessEvent::Warning { text } => panic!("unexpected plan warning: {text}"),
            _ => {}
        }
    }
    assert_eq!(
        plan.as_deref(),
        Some(
            "1. Inspect the parser.\n2. Add regression coverage.\n3. Implement and verify in Work."
        )
    );
    assert_eq!(proposal, plan);
    run.control.shutdown().await.unwrap();
    let response: Value =
        serde_json::from_slice(&std::fs::read(dir.path().join("plan-permission.json")).unwrap())
            .unwrap();
    assert_eq!(response["response"]["subtype"], "error");
}

#[test]
fn mission_and_mode_use_one_system_prompt_and_work_argv_is_unchanged() {
    let (_dir, _adapter, mut spec) = fixture();
    spec.bridge = None;
    spec.project_settings = true;
    spec.mission = Some(protocol::model::Mission {
        goal: "Inspect parser".into(),
        bounds: vec![],
        completion_checks: vec!["Review tests".into()],
        stop_point: Some("Report".into()),
    });
    let mission = "Mission: Inspect parser\nCompletion check: Review tests\nStop point: Report";
    let mission_told = told(&spec, Some(mission));
    let work = Launch::build(&spec).unwrap().args;
    assert_eq!(
        work,
        [
            "--print",
            "--output-format",
            "stream-json",
            "--input-format",
            "stream-json",
            "--verbose",
            "--include-partial-messages",
            "--replay-user-messages",
            "--permission-mode",
            "manual",
            "--model",
            "requested-model",
            "--effort",
            "high",
            "--resume",
            "old-session",
            "--append-system-prompt",
            &mission_told
        ]
    );
    for mode in [TaskMode::Ask, TaskMode::Plan] {
        spec.mode = mode;
        let launch = Launch::build(&spec).unwrap();
        let prompts: Vec<_> = launch
            .args
            .windows(2)
            .filter(|p| p[0] == "--append-system-prompt")
            .collect();
        assert_eq!(prompts.len(), 1);
        assert_eq!(
            prompts[0][1],
            told(
                &spec,
                Some(&format!(
                    "{mission}\n\n{}",
                    ports::harness::task_mode_instruction(mode).unwrap()
                ))
            )
        );
    }
}

#[test]
fn a_chosen_branch_joins_the_system_prompt_for_every_mode() {
    let (_dir, _adapter, mut spec) = fixture();
    spec.bridge = None;
    let prompt = |spec: &RunSpec| {
        Launch::build(spec)
            .unwrap()
            .args
            .windows(2)
            .filter(|p| p[0] == "--append-system-prompt")
            .map(|p| p[1].clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(prompt(&spec), [told(&spec, None)]);
    let mut chosen = protocol::model::TaskBranch {
        name: "feature/search".into(),
        new: true,
    };
    spec.branch = Some(chosen.clone());
    let made = ports::harness::branch_instruction(&chosen);
    assert!(made.contains("new Git branch `feature/search`"));
    assert!(made.contains("Before changing any file"));
    assert!(made.contains("git switch -c feature/search"));
    assert_eq!(prompt(&spec), [told(&spec, Some(&made))]);
    chosen.new = false;
    spec.branch = Some(chosen.clone());
    let branch = ports::harness::branch_instruction(&chosen);
    assert!(branch.contains("the Git branch `feature/search`"));
    assert!(!branch.contains("git switch -c"));
    assert_eq!(prompt(&spec), [told(&spec, Some(&branch))]);
    spec.mission = Some(protocol::model::Mission {
        goal: "Inspect parser".into(),
        bounds: vec![],
        completion_checks: vec![],
        stop_point: None,
    });
    assert_eq!(
        prompt(&spec),
        [told(
            &spec,
            Some(&format!("Mission: Inspect parser\n\n{branch}"))
        )]
    );
    spec.mode = TaskMode::Plan;
    assert_eq!(
        prompt(&spec),
        [told(
            &spec,
            Some(&format!(
                "Mission: Inspect parser\n\n{}\n\n{branch}",
                ports::harness::task_mode_instruction(TaskMode::Plan).unwrap()
            ))
        )]
    );
}

#[test]
fn work_applies_a_projects_own_settings_only_when_the_owner_chose_them() {
    let (_dir, _adapter, mut spec) = fixture();
    let cwd = spec.cwd.to_str().unwrap().to_owned();
    let launch = Launch::build(&spec).unwrap();
    assert!(
        launch
            .args
            .windows(2)
            .any(|w| w == ["--setting-sources", "user"])
    );
    assert!(launch.args.windows(2).any(|w| w == ["--add-dir", &cwd]));
    assert_eq!(
        launch.env,
        [("CLAUDE_CODE_ADDITIONAL_DIRECTORIES_CLAUDE_MD", "1")]
    );
    spec.project_settings = true;
    let launch = Launch::build(&spec).unwrap();
    assert!(
        !launch
            .args
            .iter()
            .any(|a| a == "--setting-sources" || a == "--add-dir")
    );
    assert!(launch.env.is_empty());
    // Ask and Plan run in safe mode, and computer use reads no settings files.
    spec.project_settings = false;
    for mode in [TaskMode::Ask, TaskMode::Plan] {
        spec.mode = mode;
        let launch = Launch::build(&spec).unwrap();
        assert!(!launch.args.iter().any(|a| a == "--add-dir"));
        assert!(launch.args.iter().any(|a| a == "--safe-mode"));
    }
    spec.mode = TaskMode::Work;
    spec.bridge.as_mut().unwrap().computer_use = true;
    let launch = Launch::build(&spec).unwrap();
    assert!(
        launch
            .args
            .windows(2)
            .any(|w| w == ["--setting-sources", ""])
    );
    assert!(!launch.args.iter().any(|a| a == "--add-dir"));
    // The supervisor reports what it came to, with none of the bridge's
    // waiting or asking.
    let told = (launch.args.windows(2))
        .find(|w| w[0] == "--append-system-prompt")
        .map(|w| w[1].clone())
        .unwrap();
    assert!(told.contains("ophio-outcome"));
    assert!(!told.contains("wake_me") && !told.contains("can_ask"));
}

#[test]
fn work_writes_to_the_tasks_scratch_folder_and_is_told_what_it_is_for() {
    let (dir, _adapter, mut spec) = fixture();
    let flag = |spec: &RunSpec, flag: &str| {
        Launch::build(spec)
            .unwrap()
            .args
            .windows(2)
            .filter(|p| p[0] == flag)
            .map(|p| p[1].clone())
            .collect::<Vec<_>>()
    };
    let scratch = dir.path().join("scratch");
    let path = scratch.to_str().unwrap().to_owned();
    spec.scratch = Some(scratch);
    assert!(flag(&spec, "--add-dir").contains(&path));
    let scratch_told = ports::harness::scratch_instruction(&spec).unwrap();
    assert!(scratch_told.contains(&format!("`{path}` (also $OPHIO_SCRATCH)")));
    assert_eq!(
        flag(&spec, "--append-system-prompt"),
        [format!("{}\n\n{scratch_told}", told(&spec, None))]
    );
    // Ask and Plan change nothing, so have nothing to put there.
    for mode in [TaskMode::Ask, TaskMode::Plan] {
        spec.mode = mode;
        assert!(!flag(&spec, "--add-dir").contains(&path));
        assert!(!flag(&spec, "--append-system-prompt")[0].contains(&path));
    }
}

#[tokio::test]
async fn withdraw_held_message_but_never_recall_a_native_write() {
    let (dir, adapter, spec) = fixture();
    let mut run = adapter.start_run(spec).await.unwrap();
    while !matches!(event(&mut run.events).await, HarnessEvent::Warning { .. }) {}
    let cancelled = MessageRef(protocol::CommandId::new());
    let dispatched = MessageRef(protocol::CommandId::new());
    let send = |reference, text: &str, delivery| {
        run.control.send_message(
            reference,
            PromptInput {
                text: text.into(),
                images: vec![],
            },
            delivery,
        )
    };
    assert_eq!(
        send(cancelled, "withdraw this", MessageDelivery::NextTurn)
            .await
            .unwrap(),
        DeliveryState::QueuedForBoundary
    );
    run.control.cancel_message(cancelled).await.unwrap();
    loop {
        if let HarnessEvent::MessageDelivery { message_ref, state } = event(&mut run.events).await {
            assert_eq!(message_ref, cancelled);
            assert_eq!(state, DeliveryState::Cancelled);
            break;
        }
    }
    assert!(matches!(
        run.control.cancel_message(cancelled).await,
        Err(HarnessError::MessageNotQueued)
    ));
    // Stop drops held follow-ups, so this one reaches the boundary by steering.
    send(dispatched, "keep this", MessageDelivery::InterruptAndSend)
        .await
        .unwrap();
    loop {
        if let HarnessEvent::MessageDelivery {
            message_ref,
            state: DeliveryState::HostAccepted | DeliveryState::NativeDelivered,
        } = event(&mut run.events).await
        {
            assert_eq!(message_ref, dispatched);
            break;
        }
    }
    assert!(matches!(
        run.control.cancel_message(dispatched).await,
        Err(HarnessError::MessageNotQueued)
    ));
    // Shutdown may now terminate an owned process immediately. Establish the
    // fixture read before ending it; a native stdin write alone is not a read.
    tokio::time::timeout(Duration::from_secs(3), async {
        while !std::fs::read_to_string(dir.path().join("stdin.jsonl"))
            .unwrap()
            .contains("keep this")
        {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
    run.control.shutdown().await.unwrap();
    while !matches!(event(&mut run.events).await, HarnessEvent::Exited { .. }) {}
    let input = std::fs::read_to_string(dir.path().join("stdin.jsonl")).unwrap();
    assert!(!input.contains("withdraw this"));
    assert!(input.contains("keep this"));
}
async fn held_follow_ups(run: &RunStarted) -> Vec<MessageRef> {
    let mut held = Vec::new();
    for text in ["first after stop", "second after stop"] {
        let message_ref = MessageRef(protocol::CommandId::new());
        assert_eq!(
            run.control
                .send_message(
                    message_ref,
                    PromptInput {
                        text: text.into(),
                        images: vec![],
                    },
                    MessageDelivery::NextTurn,
                )
                .await
                .unwrap(),
            DeliveryState::QueuedForBoundary
        );
        held.push(message_ref);
    }
    held
}

/// Every held follow-up is reported withdrawn before the run's end.
async fn withdrawn_before_exit(run: &mut RunStarted) -> Vec<MessageRef> {
    let mut withdrawn = Vec::new();
    loop {
        match event(&mut run.events).await {
            HarnessEvent::MessageDelivery { message_ref, state } => {
                assert_eq!(state, DeliveryState::Cancelled);
                withdrawn.push(message_ref);
            }
            HarnessEvent::Exited { .. } => return withdrawn,
            _ => {}
        }
    }
}

#[tokio::test]
async fn stop_withdraws_every_held_follow_up() {
    let (dir, adapter, spec) = fixture();
    let mut run = adapter.start_run(spec).await.unwrap();
    while !matches!(event(&mut run.events).await, HarnessEvent::Warning { .. }) {}
    let held = held_follow_ups(&run).await;
    run.control.interrupt().await.unwrap();
    assert_eq!(withdrawn_before_exit(&mut run).await, held);
    let input = std::fs::read_to_string(dir.path().join("stdin.jsonl")).unwrap();
    assert!(!input.contains("after stop"));
    run.control.shutdown().await.unwrap();
}

#[tokio::test]
async fn force_stop_withdraws_every_held_follow_up() {
    let (dir, adapter, spec) = fixture();
    let mut run = adapter.start_run(spec).await.unwrap();
    while !matches!(event(&mut run.events).await, HarnessEvent::Warning { .. }) {}
    let held = held_follow_ups(&run).await;
    run.control.terminate().await.unwrap();
    assert_eq!(withdrawn_before_exit(&mut run).await, held);
    let input = std::fs::read_to_string(dir.path().join("stdin.jsonl")).unwrap();
    assert!(!input.contains("after stop"));
}

#[tokio::test]
async fn unexpected_callback_is_refused_in_plain_language() {
    let (dir, adapter, mut spec) = fixture();
    spec.mode = TaskMode::Work;
    spec.bridge = None;
    spec.prompt.text = "plan-reply".into();
    let mut run = adapter.start_run(spec).await.unwrap();
    let mut warning = None;
    loop {
        match event(&mut run.events).await {
            HarnessEvent::Warning { text } => warning = Some(text),
            HarnessEvent::TurnCompleted { .. } => break,
            HarnessEvent::ApprovalRequested(_) => panic!("unsupported callback cannot be approved"),
            _ => {}
        }
    }
    assert_eq!(
        warning.as_deref(),
        Some("The agent asked for something this app can't answer yet · refused")
    );
    let response: Value =
        serde_json::from_slice(&std::fs::read(dir.path().join("plan-permission.json")).unwrap())
            .unwrap();
    assert_eq!(response["response"]["subtype"], "error");
    assert_eq!(
        response["response"]["error"],
        "unsupported companion control callback"
    );
    run.control.shutdown().await.unwrap();
}

#[tokio::test]
async fn reviewed_plan_resumes_with_manual_work_permissions() {
    let (dir, adapter, mut spec) = fixture();
    spec.mode = TaskMode::Plan;
    spec.resume_session = None;
    spec.bridge = None;
    spec.prompt.text = "plan-reply".into();
    let mut plan = adapter.start_run(spec.clone()).await.unwrap();
    let mut session = None;
    loop {
        match event(&mut plan.events).await {
            HarnessEvent::SessionStarted { native_session, .. } => session = Some(native_session),
            HarnessEvent::TurnCompleted { .. } => break,
            _ => {}
        }
    }
    plan.control.shutdown().await.unwrap();
    spec.mode = TaskMode::Work;
    spec.resume_from_mode = Some(TaskMode::Plan);
    spec.resume_session = session;
    spec.prompt.text = "complete".into();
    let mut work = adapter.start_run(spec).await.unwrap();
    while !matches!(
        event(&mut work.events).await,
        HarnessEvent::TurnCompleted { .. }
    ) {}
    let args: Vec<String> =
        serde_json::from_slice(&std::fs::read(dir.path().join("argv.json")).unwrap()).unwrap();
    assert!(args.windows(2).any(|p| p == ["--resume", "native-session"]));
    assert!(
        args.windows(2)
            .any(|p| p == ["--permission-mode", "manual"])
    );
    assert!(!args.iter().any(|a| a == "--tools" || a == "--safe-mode"));
    work.control.shutdown().await.unwrap();
}

/// Present, and neither a zombie nor dead.
#[cfg(target_os = "linux")]
fn running(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
        !matches!(
            stat.rsplit_once(") ")
                .and_then(|(_, rest)| rest.split(' ').next()),
            Some("Z" | "X" | "x")
        )
    })
}

#[cfg(target_os = "linux")]
async fn until(what: &str, done: impl Fn() -> bool) {
    let started = tokio::time::Instant::now();
    while !done() {
        assert!(started.elapsed() < Duration::from_secs(3), "{what}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// An agent that exits leaving a helper in its process group, holding its
/// output open. Returns once the agent has exited, with the helper's id.
#[cfg(target_os = "linux")]
async fn outlived_by_a_helper() -> (tempfile::TempDir, RunStarted, u32) {
    let (dir, adapter, mut spec) = fixture();
    spec.prompt.text = "outlive".into();
    let run = adapter.start_run(spec).await.unwrap();
    let id = |name: &str| {
        std::fs::read_to_string(dir.path().join(name))
            .ok()
            .and_then(|id| id.parse::<u32>().ok())
    };
    until("the agent should exit", || {
        id("helper.pid").is_some() && id("fixture.pid").is_some_and(|agent| !running(agent))
    })
    .await;
    // An adapter that reaps the agent now can no longer name its group.
    // Give it time to, though this adapter must not.
    let agent = id("fixture.pid").unwrap();
    let started = tokio::time::Instant::now();
    while std::path::Path::new(&format!("/proc/{agent}")).exists()
        && started.elapsed() < Duration::from_millis(500)
    {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let helper = id("helper.pid").unwrap();
    (dir, run, helper)
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn force_stop_reaches_a_helper_that_outlives_the_agent() {
    let (_dir, mut run, helper) = outlived_by_a_helper().await;
    assert!(running(helper));
    run.control.terminate().await.unwrap();
    until("Force stop should end the helper", || !running(helper)).await;
    loop {
        if let HarnessEvent::Exited { exit, .. } = event(&mut run.events).await {
            assert_eq!(exit, RunExit::Interrupted);
            break;
        }
    }
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_helper_that_outlives_the_agent_ends_with_its_run() {
    let (_dir, mut run, helper) = outlived_by_a_helper().await;
    // The run ends once the grace for the agent's output has passed.
    loop {
        let event = tokio::time::timeout(Duration::from_secs(10), run.events.recv())
            .await
            .unwrap()
            .unwrap();
        if matches!(event, HarnessEvent::Exited { .. }) {
            break;
        }
    }
    until("the helper should end with the run", || !running(helper)).await;
}

#[test]
fn an_agent_or_skill_is_loaded_natively_only_where_claude_finds_it_itself() {
    use ports::{NativeEntry, RunStart};
    let (_dir, _adapter, mut spec) = fixture();
    spec.bridge = None;
    spec.resume_session = None;
    let flag = |spec: &RunSpec, flag: &str| {
        Launch::build(spec)
            .unwrap()
            .args
            .windows(2)
            .filter(|p| p[0] == flag)
            .map(|p| p[1].clone())
            .collect::<Vec<_>>()
    };
    let native = |harness, project| {
        Some(NativeEntry {
            harness,
            name: "video-producer".into(),
            project,
        })
    };
    let agent = |native| RunStart::Agent {
        name: "Video producer".into(),
        instructions: "Make short videos.".into(),
        native,
        file: "/agents/video-producer.md".into(),
        digest: "d".into(),
    };

    // Claude's own agent, in Work: by name, with nothing added.
    spec.start = Some(agent(native(HarnessKind::ClaudeCode, false)));
    assert_eq!(flag(&spec, "--agent"), ["video-producer"]);
    assert_eq!(flag(&spec, "--append-system-prompt"), [told(&spec, None)]);
    assert_eq!(invoked_skill(&spec), None);
    // Ask and Plan keep their own tools: the agent is told, not loaded.
    for mode in [TaskMode::Ask, TaskMode::Plan] {
        spec.mode = mode;
        assert!(flag(&spec, "--agent").is_empty());
        let prompt = flag(&spec, "--append-system-prompt");
        assert!(prompt[0].starts_with("For this task, act as the agent “Video producer”"));
        assert!(prompt[0].contains("Make short videos."));
    }
    spec.mode = TaskMode::Work;
    // A project's own agent only where the project's settings apply.
    spec.start = Some(agent(native(HarnessKind::ClaudeCode, true)));
    assert!(flag(&spec, "--agent").is_empty());
    assert_eq!(flag(&spec, "--append-system-prompt").len(), 1);
    spec.project_settings = true;
    assert_eq!(flag(&spec, "--agent"), ["video-producer"]);
    spec.project_settings = false;
    // Ophio's own, or another harness's, joins the system prompt.
    for native in [None, native(HarnessKind::Codex, false)] {
        spec.start = Some(agent(native));
        assert!(flag(&spec, "--agent").is_empty());
        let prompt = flag(&spec, "--append-system-prompt");
        assert!(prompt[0].ends_with(&told(&spec, Some("Make short videos."))));
    }
    spec.branch = Some(protocol::model::TaskBranch {
        name: "videos".into(),
        new: false,
    });
    let prompt = flag(&spec, "--append-system-prompt");
    assert!(prompt[0].contains("Make short videos.\n\n"));
    assert!(prompt[0].ends_with(&told(
        &spec,
        Some(&ports::harness::branch_instruction(
            spec.branch.as_ref().unwrap()
        ))
    )));
    spec.branch = None;

    // Claude's own skill is invoked by the first prompt of a new session.
    let folder = spec.cwd.join("skills").join("launch-video-clone");
    let skill = |native: Option<NativeEntry>| RunStart::Skill {
        name: "launch-video-clone".into(),
        skill_file: folder.join("SKILL.md"),
        native: native.map(|n| NativeEntry {
            name: "launch-video-clone".into(),
            ..n
        }),
        instructions: Some("Render with the clone pipeline.".into()),
        digest: "d".into(),
    };
    spec.start = Some(skill(native(HarnessKind::ClaudeCode, false)));
    assert_eq!(invoked_skill(&spec), Some("launch-video-clone"));
    assert_eq!(flag(&spec, "--append-system-prompt"), [told(&spec, None)]);
    let folder_arg = folder.to_str().unwrap().to_owned();
    assert!(!flag(&spec, "--add-dir").contains(&folder_arg));
    // A resumed session may have begun where the skill wasn't invoked, as a
    // plan Work carries out, so it is given the instructions instead.
    spec.resume_session = Some("old-session".into());
    for from in [TaskMode::Plan, TaskMode::Work] {
        spec.resume_from_mode = Some(from);
        assert_eq!(invoked_skill(&spec), None);
        assert!(flag(&spec, "--add-dir").contains(&folder_arg));
        let prompt = flag(&spec, "--append-system-prompt");
        assert!(prompt[0].contains(".\n\nRender with the clone pipeline."));
    }
    spec.resume_session = None;
    spec.resume_from_mode = None;
    // Elsewhere it is given as it was read, with its folder for the files
    // it mentions.
    for (mode, native) in [
        (TaskMode::Plan, native(HarnessKind::ClaudeCode, false)),
        (TaskMode::Work, native(HarnessKind::Codex, false)),
        (TaskMode::Work, None),
    ] {
        spec.mode = mode;
        spec.start = Some(skill(native));
        assert_eq!(invoked_skill(&spec), None);
        assert!(flag(&spec, "--add-dir").contains(&folder_arg));
        let prompt = flag(&spec, "--append-system-prompt");
        assert!(prompt[0].starts_with("This task uses the skill “launch-video-clone”"));
        assert!(prompt[0].contains(&format!("relative to {folder_arg}.")));
        assert!(prompt[0].contains(".\n\nRender with the clone pipeline."));
        assert!(!prompt[0].contains("SKILL.md"));
    }
    // A start saved before its text was kept points to its file.
    if let Some(RunStart::Skill { instructions, .. }) = &mut spec.start {
        *instructions = None;
    }
    let prompt = flag(&spec, "--append-system-prompt");
    assert!(prompt[0].contains(&format!("{folder_arg}/SKILL.md")));
    // Scratch adds nothing.
    spec.mode = TaskMode::Work;
    spec.start = Some(RunStart::Scratch);
    assert_eq!(flag(&spec, "--append-system-prompt"), [told(&spec, None)]);
    assert!(flag(&spec, "--agent").is_empty());
}

#[tokio::test]
async fn a_native_skill_is_invoked_by_the_first_prompt_of_a_new_session() {
    use ports::{NativeEntry, RunStart};
    for resume in [None, Some("old-session".to_owned())] {
        let (dir, adapter, mut spec) = fixture();
        spec.bridge = None;
        spec.resume_session = resume.clone();
        spec.start = Some(RunStart::Skill {
            name: "Launch video".into(),
            skill_file: dir.path().join("launch-video-clone").join("SKILL.md"),
            native: Some(NativeEntry {
                harness: HarnessKind::ClaudeCode,
                name: "launch-video-clone".into(),
                project: false,
            }),
            instructions: Some("Render it.".into()),
            digest: "d".into(),
        });
        let run = adapter.start_run(spec).await.unwrap();
        let log = dir.path().join("stdin.jsonl");
        for _ in 0..300 {
            if std::fs::read_to_string(&log).is_ok_and(|s| s.ends_with('\n')) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        run.control.shutdown().await.unwrap();
        let first: Value = serde_json::from_str(
            std::fs::read_to_string(&log)
                .unwrap()
                .lines()
                .next()
                .unwrap(),
        )
        .unwrap();
        let expected = if resume.is_none() {
            "/launch-video-clone start"
        } else {
            "start"
        };
        assert_eq!(first["message"]["content"][0]["text"], expected);
    }
}

#[test]
fn a_browsing_work_run_may_use_the_browser_tools_without_asking() {
    let (_dir, _adapter, mut spec) = fixture();
    let allowed = |spec: &RunSpec| {
        let args = Launch::build(spec).unwrap().args;
        let at = args.iter().position(|a| a == "--allowedTools")?;
        Some(args[at + 1].clone())
    };
    // A Work run may always show the person things, and plan their work.
    let person: Vec<_> = (ports::bridge::PERSON_TOOLS.iter())
        .chain(ports::bridge::WORK_TOOLS)
        .map(|tool| format!("mcp__computer__{tool}"))
        .collect();
    assert_eq!(allowed(&spec), Some(person.join(",")));
    spec.bridge.as_mut().unwrap().browser = true;
    let tools = allowed(&spec).unwrap();
    let tools: Vec<&str> = tools.split(',').collect();
    assert!(tools.contains(&"mcp__computer__browser_open"));
    assert!(tools.contains(&"mcp__computer__fill_login"));
    assert!(tools.contains(&"mcp__computer__request_human"));
    assert_eq!(
        tools.len(),
        ports::bridge::PERSON_TOOLS.len()
            + ports::bridge::WORK_TOOLS.len()
            + ports::bridge::BROWSER_TOOLS.len()
            + 1
    );
    // The tool answering permission prompts stays out of the model's reach.
    assert!(!tools.contains(&"mcp__computer__permission_prompt"));
}

#[test]
fn a_run_linked_to_github_gets_its_tools_and_instructions_once() {
    let (_dir, _adapter, mut spec) = fixture();
    let flag_values = |spec: &RunSpec, name: &str| -> Vec<String> {
        let args = Launch::build(spec).unwrap().args;
        args.windows(2)
            .filter(|pair| pair[0] == name)
            .map(|pair| pair[1].clone())
            .collect()
    };
    spec.bridge.as_mut().unwrap().github = true;
    spec.github = Some("## GitHub\nThis task is linked to GitHub.".into());
    let allowed = flag_values(&spec, "--allowedTools");
    assert_eq!(allowed.len(), 1);
    let tools: Vec<&str> = allowed[0].split(',').collect();
    assert_eq!(
        tools.len(),
        ports::bridge::PERSON_TOOLS.len()
            + ports::bridge::GITHUB_TOOLS.len()
            + ports::bridge::ASK_TOOLS.len()
    );
    assert!(tools.contains(&"mcp__computer__github_pr"));
    assert!(tools.contains(&"mcp__computer__ask_agent"));
    assert!(!tools.contains(&"mcp__computer__github_start_issue"));
    assert!(!tools.contains(&"mcp__computer__browser_open"));
    let prompt = flag_values(&spec, "--append-system-prompt");
    assert_eq!(prompt.len(), 1);
    assert!(prompt[0].contains("This task is linked to GitHub."));

    // With the browser too, one list holds both.
    spec.bridge.as_mut().unwrap().browser = true;
    let allowed = flag_values(&spec, "--allowedTools");
    assert_eq!(allowed.len(), 1);
    let tools: Vec<&str> = allowed[0].split(',').collect();
    assert_eq!(
        tools.len(),
        ports::bridge::PERSON_TOOLS.len()
            + ports::bridge::GITHUB_TOOLS.len()
            + ports::bridge::ASK_TOOLS.len()
            + ports::bridge::BROWSER_TOOLS.len()
            + 1
    );
    assert!(tools.contains(&"mcp__computer__github_merge"));
    assert!(tools.contains(&"mcp__computer__request_human"));
}

#[test]
fn disabled_resources_merge_into_one_final_settings_flag() {
    let (_dir, _adapter, mut spec) = fixture();
    let file = spec.cwd.join("CLAUDE.md");
    spec.disabled.skills.push(ports::DisabledEntry {
        path: spec.cwd.join("SKILL.md"),
        name: "hidden-skill".into(),
        harness: None,
    });
    spec.disabled.skills.push(ports::DisabledEntry {
        path: spec.cwd.join("other"),
        name: "codex-only".into(),
        harness: Some(HarnessKind::Codex),
    });
    spec.disabled.agents.push(ports::DisabledEntry {
        path: spec.cwd.join("agent.md"),
        name: "plugin:hidden".into(),
        harness: None,
    });
    std::fs::write(&file, "test").unwrap();
    spec.disabled.files.push(file.clone());
    spec.disabled.memory = true;
    spec.disabled.output_style = true;
    let launch = Launch::build(&spec).unwrap();
    let args = &launch.args;
    let matches: Vec<_> = args
        .windows(2)
        .filter(|pair| pair[0] == "--settings")
        .collect();
    assert_eq!(matches.len(), 1);
    assert_eq!(args[args.len() - 2], "--settings");
    let settings: Value =
        serde_json::from_slice(&std::fs::read(&args[args.len() - 1]).unwrap()).unwrap();
    assert_eq!(settings["skillOverrides"], json!({"hidden-skill":"off"}));
    assert_eq!(
        settings["permissions"]["deny"],
        json!(["Agent(plugin:hidden)"])
    );
    assert_eq!(settings["claudeMdExcludes"], json!([file]));
    assert_eq!(settings["autoMemoryEnabled"], false);
    assert_eq!(settings["outputStyle"], "default");
    spec.model = Some("claude-sonnet-4-5".into());
    let launch = Launch::build(&spec).unwrap();
    let args = &launch.args;
    let settings: Value =
        serde_json::from_slice(&std::fs::read(args.last().unwrap()).unwrap()).unwrap();
    assert_eq!(settings["switchModelsOnFlag"], false);
    spec.bridge.as_mut().unwrap().computer_use = true;
    spec.model = None;
    let launch = Launch::build(&spec).unwrap();
    let args = &launch.args;
    assert_eq!(args.iter().filter(|arg| *arg == "--settings").count(), 1);
    let settings: Value =
        serde_json::from_slice(&std::fs::read(args.last().unwrap()).unwrap()).unwrap();
    assert_eq!(settings["disableAllHooks"], true);
    assert_eq!(settings["skillOverrides"]["hidden-skill"], "off");
}

#[test]
fn empty_disabled_resources_add_no_settings_flag() {
    let (_dir, _adapter, mut spec) = fixture();
    spec.model = None;
    assert!(
        !Launch::build(&spec)
            .unwrap()
            .args
            .contains(&"--settings".into())
    );
}

#[test]
fn disabled_globs_escape_metacharacters_and_dedupe_paths() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("a[x] {y} (z)!+@*?.md");
    std::fs::write(&file, "canary").unwrap();
    let disabled = ports::Disabled {
        files: vec![file.clone(), file.clone(), dir.path().join("missing.md")],
        ..Default::default()
    };
    let settings = disabled_settings(&disabled).unwrap();
    assert_eq!(
        settings["claudeMdExcludes"],
        json!([format!(
            "{}/a[[]x[]] [{{]y[}}] [(]z[)][!][+][@][*][?].md",
            dir.path().display()
        )])
    );
}

#[test]
fn terminal_settings_are_reused_and_owner_only() {
    let dir = tempfile::tempdir().unwrap();
    assert!(
        terminal_arguments(dir.path(), &ports::Disabled::default())
            .unwrap()
            .is_empty()
    );
    let disabled = ports::Disabled {
        memory: true,
        ..Default::default()
    };
    let first = terminal_arguments(dir.path(), &disabled).unwrap();
    let second = terminal_arguments(dir.path(), &disabled).unwrap();
    assert_eq!(first, second);
    assert_eq!(first[0], "--settings");
    assert_eq!(
        serde_json::from_slice::<Value>(&std::fs::read(&first[1]).unwrap()).unwrap()["autoMemoryEnabled"],
        false
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&first[1]).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[test]
fn a_turn_answering_on_the_checklist_gets_its_read_and_reply_tools_alone() {
    let (_dir, _adapter, mut spec) = fixture();
    spec.bridge.as_mut().unwrap().checklist_answers = true;
    let only = "mcp__computer__checklist_read,mcp__computer__checklist_reply";
    for mode in [TaskMode::Ask, TaskMode::Work] {
        spec.mode = mode;
        let launch = Launch::build(&spec).unwrap();
        let args = &launch.args;
        let values = |name: &str| -> Vec<&str> {
            (args.windows(2))
                .filter(|pair| pair[0] == name)
                .map(|pair| pair[1].as_str())
                .collect()
        };
        assert_eq!(values("--allowedTools"), [only], "{mode:?}");
        // It is never asked for permission, and nothing tells it to wait or
        // to ask through Ophio's tools.
        assert!(values("--permission-prompt-tool").is_empty(), "{mode:?}");
        let told = values("--append-system-prompt").join("\n");
        assert!(
            !told.contains("wake_me") && !told.contains("can_ask"),
            "{told}"
        );
        // Its MCP config names the bridge.
        let config = std::fs::read_to_string(values("--mcp-config")[0]).unwrap();
        assert!(config.contains("/fixture/computer-mcp"), "{config}");
        if mode == TaskMode::Ask {
            // Plan mode would refuse the two tools, and safe mode would turn
            // the bridge off; it reads no settings files instead.
            assert_eq!(values("--tools"), ["Read,Glob,Grep"]);
            assert_eq!(values("--permission-mode"), ["manual"]);
            assert_eq!(values("--permission-prompts"), ["none"]);
            assert_eq!(values("--setting-sources"), [""]);
            assert!(!args.contains(&"--safe-mode".to_owned()));
            assert!(args.contains(&"--strict-mcp-config".to_owned()));
        }
    }
    // Any other Ask turn still has no server at all.
    spec.mode = TaskMode::Ask;
    spec.bridge.as_mut().unwrap().checklist_answers = false;
    let launch = Launch::build(&spec).unwrap();
    let at = launch
        .args
        .iter()
        .position(|a| a == "--mcp-config")
        .unwrap();
    let config = std::fs::read_to_string(&launch.args[at + 1]).unwrap();
    assert_eq!(config, "{\"mcpServers\":{}}");
    assert!(!launch.args.contains(&"--allowedTools".to_owned()));
    assert!(launch.args.contains(&"--safe-mode".to_owned()));
    let at = launch
        .args
        .iter()
        .position(|a| a == "--permission-mode")
        .unwrap();
    assert_eq!(launch.args[at + 1], "plan");
}

#[test]
fn a_read_only_turn_answering_on_the_checklist_gets_the_disabled_block() {
    let (_dir, _adapter, mut spec) = fixture();
    spec.mode = TaskMode::Ask;
    spec.model = None;
    spec.bridge.as_mut().unwrap().checklist_answers = true;
    spec.disabled.skills.push(ports::DisabledEntry {
        path: spec.cwd.join("SKILL.md"),
        name: "hidden-skill".into(),
        harness: None,
    });
    let launch = Launch::build(&spec).unwrap();
    let args = &launch.args;
    assert_eq!(args.iter().filter(|arg| *arg == "--settings").count(), 1);
    let settings: Value =
        serde_json::from_slice(&std::fs::read(args.last().unwrap()).unwrap()).unwrap();
    assert_eq!(settings["skillOverrides"]["hidden-skill"], "off");
}
