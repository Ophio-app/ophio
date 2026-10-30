// SPDX-License-Identifier: Apache-2.0
#![cfg(unix)]
use super::*;
mod boundary;
mod computer;
mod containment;
mod launch;
mod sandbox;
use protocol::{
    ids::{RunId, TaskId},
    model::{ActivityStatus, ApprovalKind},
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
/// What a run with Codex's question tool is told: `text`, then how to ask.
fn asking(text: &str) -> String {
    format!("{text}\n\n{ASKING}")
}
fn fixture() -> (tempfile::TempDir, CodexAdapter, RunSpec) {
    let dir = tempfile::tempdir().unwrap();
    let binary = dir.path().join("fake-codex");
    test_support::executable(&binary, include_str!("../tests/fake_codex.py")).unwrap();
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
        resume_session: Some("resume-fixture".into()),
        resume_from_mode: None,
        mission: None,
        branch: None,
        start: None,
        bridge: Some(ToolBridge {
            server_name: "computer".into(),
            command: "/fixture/computer-mcp".into(),
            args: vec!["--fixture".into()],
            env: vec![
                (ports::bridge::RUN_TOKEN_ENV.into(), "fixture-secret".into()),
                (ports::bridge::SOCKET_ENV.into(), "/fixture/socket".into()),
            ],
            permission_tool: None,
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
    (dir, CodexAdapter::with_binary(binary), spec)
}
async fn event(rx: &mut mpsc::Receiver<HarnessEvent>) -> HarnessEvent {
    let event = tokio::time::timeout(Duration::from_secs(3), rx.recv())
        .await
        .unwrap()
        .unwrap();
    // Start summaries may survive a stopped turn without an item/completed.
    if let HarnessEvent::Activity { summary, .. } = &event {
        assert!(!summary.to_lowercase().contains("running"), "{summary}");
    }
    event
}
#[tokio::test]
async fn replay_approvals_exact_bytes_steering_resume_and_usage() {
    let (dir, adapter, spec) = fixture();
    let probe = adapter.probe().await;
    assert!(matches!(probe.account, AccountReadiness::SignedIn { .. }));
    assert_eq!(probe.models[0].id, "gpt-6-astra");
    assert_eq!(probe.models[1].details.as_ref().unwrap().efforts.len(), 1);
    assert_eq!(
        probe.catalog.as_ref().unwrap().default_effort.as_deref(),
        Some("high")
    );
    let mut run = adapter.start_run(spec).await.unwrap();
    assert!(
        matches!(event(&mut run.events).await,HarnessEvent::SessionStarted {actual_model:Some(ref m),actual_effort:Some(ref e),..} if m=="actual-native-model" && e=="medium")
    );
    let mut ids = vec![];
    let mut streaming = false;
    let mut activity = false;
    let mut warnings = vec![];
    let mut file_detail = None;
    while ids.len() < 2 {
        match event(&mut run.events).await {
            HarnessEvent::ApprovalRequested(a) => {
                if a.kind == ApprovalKind::FileEdit {
                    file_detail = a.detail.clone();
                }
                ids.push((a.native_request_id, a.kind))
            }
            HarnessEvent::AgentTextDelta { text, .. } => {
                assert_eq!(text, "hello");
                streaming = true;
            }
            HarnessEvent::Activity {
                status: ActivityStatus::Running,
                ..
            } => activity = true,
            HarnessEvent::Warning { text } => warnings.push(text),
            _ => {}
        }
    }
    assert!(streaming && activity);
    // The approval names the file its change was announced for.
    assert!(
        file_detail
            .as_deref()
            .is_some_and(|detail| detail.contains("/etc/profile.d/agent.sh")),
        "{file_detail:?}"
    );
    // An unread kind is noted once, a configuration warning keeps its words,
    // and known status chatter and streamed output say nothing.
    assert_eq!(
        warnings,
        vec![
            "Codex sent an update Ophio doesn't know yet (future/method). The task carries on; an Ophio update will read it.",
            "Codex reports a configuration problem: unknown key `fixture`",
        ]
    );
    assert_eq!(
        ids,
        vec![
            ("71".into(), ApprovalKind::Command),
            ("\"71\"".into(), ApprovalKind::FileEdit)
        ]
    );
    run.control
        .resolve_approval(&ids[0].0, &ApprovalDecision::Allow)
        .await
        .unwrap();
    run.control
        .resolve_approval(&ids[1].0, &ApprovalDecision::Deny { reason: None })
        .await
        .unwrap();
    assert!(
        run.control
            .resolve_approval(&ids[0].0, &ApprovalDecision::Allow)
            .await
            .is_err()
    );
    assert_eq!(
        run.control
            .send_message(
                MessageRef(protocol::CommandId::new()),
                PromptInput {
                    text: "finish".into(),
                    images: vec![]
                },
                MessageDelivery::Steer
            )
            .await
            .unwrap(),
        DeliveryState::NativeDelivered
    );
    loop {
        if let HarnessEvent::TurnCompleted { usage } = event(&mut run.events).await {
            assert_eq!(usage.unwrap().input_tokens, Some(12));
            break;
        }
    }
    assert!(matches!(
        event(&mut run.events).await,
        HarnessEvent::Exited {
            exit: RunExit::Completed,
            ..
        }
    ));
    assert!(run.events.recv().await.is_none());
    let raw = std::fs::read_to_string(dir.path().join("stdin.jsonl")).unwrap();
    assert!(
        raw.lines()
            .any(|line| line == "{\"id\":71,\"result\":{\"decision\":\"accept\"}}")
    );
    assert!(
        raw.lines()
            .any(|line| line == "{\"id\":\"71\",\"result\":{\"decision\":\"decline\"}}")
    );
    let frames: Vec<Value> = raw
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect();
    let resume = frames
        .iter()
        .find(|v| v["method"] == "thread/resume")
        .unwrap();
    assert_eq!(resume["params"]["threadId"], "resume-fixture");
    assert_eq!(resume["params"]["excludeTurns"], true);
    assert_eq!(resume["params"]["model"], "requested-model");
    assert_eq!(resume["params"]["config"]["model_reasoning_effort"], "high");
    let turn = frames.iter().find(|v| v["method"] == "turn/start").unwrap();
    assert_eq!(turn["params"]["effort"], "high");
    let args = std::fs::read_to_string(dir.path().join("argv.json")).unwrap();
    assert!(!args.contains("fixture-secret"));
    assert!(!args.contains("permission_prompt"));
}
#[tokio::test]
async fn interrupt_and_send_waits_for_interrupt_completion() {
    let (dir, adapter, spec) = fixture();
    let mut run = adapter.start_run(spec).await.unwrap();
    while !matches!(
        event(&mut run.events).await,
        HarnessEvent::ApprovalRequested(_)
    ) {}
    assert_eq!(
        run.control
            .send_message(
                MessageRef(protocol::CommandId::new()),
                PromptInput {
                    text: "later".into(),
                    images: vec![]
                },
                MessageDelivery::InterruptAndSend
            )
            .await
            .unwrap(),
        DeliveryState::QueuedForBoundary
    );
    let mut turns = 0;
    while turns < 2 {
        if matches!(
            event(&mut run.events).await,
            HarnessEvent::TurnCompleted { .. }
        ) {
            turns += 1;
        }
    }
    assert!(matches!(
        event(&mut run.events).await,
        HarnessEvent::Exited {
            exit: RunExit::Completed,
            ..
        }
    ));
    let raw = std::fs::read_to_string(dir.path().join("stdin.jsonl")).unwrap();
    assert!(raw.lines().any(|line| line == "{\"id\":\"companion-5\",\"method\":\"turn/interrupt\",\"params\":{\"threadId\":\"thread-fixture\",\"turnId\":\"turn-1\"}}"));
}
#[tokio::test]
async fn interrupt_and_send_confirms_its_reference_before_the_older_queued_turn() {
    let (dir, adapter, spec) = fixture();
    let mut run = adapter.start_run(spec).await.unwrap();
    loop {
        match event(&mut run.events).await {
            HarnessEvent::ApprovalRequested(_) => break,
            HarnessEvent::MessageDelivery { .. } => {
                panic!("initial/resume prompt has no reference")
            }
            _ => {}
        }
    }
    let a = MessageRef(protocol::CommandId::new());
    let b = MessageRef(protocol::CommandId::new());
    for (message_ref, text, delivery) in [
        (a, "later", MessageDelivery::NextTurn),
        (b, "complete", MessageDelivery::InterruptAndSend),
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
            DeliveryState::QueuedForBoundary
        );
    }
    let mut updates = Vec::new();
    let mut turns = 0;
    loop {
        match event(&mut run.events).await {
            HarnessEvent::MessageDelivery { message_ref, state } => {
                updates.push((message_ref, state))
            }
            HarnessEvent::TurnCompleted { .. } => turns += 1,
            HarnessEvent::Exited { exit, .. } => {
                assert_eq!(exit, RunExit::Completed);
                break;
            }
            _ => {}
        }
    }
    assert_eq!(turns, 3);
    assert_eq!(
        updates,
        vec![
            (b, DeliveryState::HostAccepted),
            (b, DeliveryState::NativeDelivered),
            (a, DeliveryState::HostAccepted),
            (a, DeliveryState::NativeDelivered),
        ]
    );
    let frames: Vec<Value> = std::fs::read_to_string(dir.path().join("stdin.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let texts: Vec<_> = frames
        .iter()
        .filter(|v| v["method"] == "turn/start")
        .map(|v| v["params"]["input"][0]["text"].as_str().unwrap())
        .collect();
    assert_eq!(texts, vec!["start", "complete", "later"]);
}

#[tokio::test]
async fn cancelled_steer_waiter_still_emits_its_correlated_native_confirmation() {
    use tokio::{io::AsyncWriteExt, net::UnixListener};

    let (dir, _, spec) = fixture();
    let binary = dir.path().join("delayed-codex");
    let script = include_str!("../tests/fake_codex.py").replace(
        "elif method == \"turn/steer\":",
        "elif method == \"turn/steer\":\n        gate(\"steer.sock\")",
    );
    test_support::executable(&binary, script).unwrap();
    let listener = UnixListener::bind(dir.path().join("steer.sock")).unwrap();
    let mut run = CodexAdapter::with_binary(binary)
        .start_run(spec)
        .await
        .unwrap();
    while !matches!(
        event(&mut run.events).await,
        HarnessEvent::ApprovalRequested(_)
    ) {}
    let a = MessageRef(protocol::CommandId::new());
    let b = MessageRef(protocol::CommandId::new());
    assert_eq!(
        run.control
            .send_message(
                a,
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
    let control = run.control.clone();
    let send = tokio::spawn(async move {
        control
            .send_message(
                b,
                PromptInput {
                    text: "steer".into(),
                    images: vec![],
                },
                MessageDelivery::Steer,
            )
            .await
    });
    // Model the hub dropping its timed-out send after the native write, before ack.
    let mut gate = boundary::gate(&listener).await;
    send.abort();
    assert!(send.await.unwrap_err().is_cancelled());
    gate.write_all(b"x").await.unwrap();
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
            (b, DeliveryState::NativeDelivered),
            (a, DeliveryState::HostAccepted),
            (a, DeliveryState::NativeDelivered),
        ]
    );
}

#[tokio::test]
async fn crash_is_failed() {
    let (_dir, adapter, mut spec) = fixture();
    spec.prompt.text = "crash".into();
    let mut run = adapter.start_run(spec).await.unwrap();
    assert!(matches!(
        event(&mut run.events).await,
        HarnessEvent::SessionStarted { .. }
    ));
    assert!(matches!(
        event(&mut run.events).await,
        HarnessEvent::Exited {
            exit: RunExit::Failed { .. },
            ..
        }
    ));
}
#[tokio::test]
async fn a_rejected_request_keeps_the_app_server_reason() {
    let (dir, adapter, spec) = fixture();
    std::fs::write(dir.path().join("resume-missing"), "").unwrap();
    let mut run = adapter.start_run(spec).await.unwrap();
    loop {
        if let HarnessEvent::Exited { exit, .. } = event(&mut run.events).await {
            let RunExit::Failed { reason } = exit else {
                panic!("{exit:?}")
            };
            assert!(
                reason.contains("no rollout found for thread id resume-fixture"),
                "{reason}"
            );
            break;
        }
    }
}
#[tokio::test]
async fn images_are_encoded_without_mutable_path_race() {
    let (dir, _, spec) = fixture();
    std::fs::write(dir.path().join("image.png"), [1, 2, 3]).unwrap();
    let input = prepare(
        &PromptInput {
            text: "look".into(),
            images: vec![ImageInput {
                path: "image.png".into(),
                media_type: "image/png".into(),
            }],
        },
        &spec.cwd,
    )
    .await
    .unwrap();
    assert_eq!(
        serde_json::to_value(input).unwrap()[1]["url"],
        "data:image/png;base64,AQID"
    );
}
#[test]
fn decisions_do_not_widen_permission_scope() {
    let approval = PendingApproval {
        id: Id::Number(1),
        method: "item/permissions/requestApproval".into(),
        params: json!({"permissions":{"network":{"enabled":true}}}),
    };
    assert!(approval.response(&ApprovalDecision::Allow).is_err());
    assert_eq!(
        approval
            .response(&ApprovalDecision::Deny { reason: None })
            .unwrap(),
        json!({"permissions":{},"scope":"turn"})
    );
    let question = PendingApproval {
        id: Id::Text("q".into()),
        method: "item/tool/requestUserInput".into(),
        params: json!({"questions":[{"id":"choice"}]}),
    };
    assert_eq!(
        question
            .response(&ApprovalDecision::Answer {
                text: "answer".into(),
                answers: Vec::new()
            })
            .unwrap(),
        json!({"answers":{"choice":{"answers":["answer"]}}})
    );
    assert!(question.response(&ApprovalDecision::Allow).is_err());
}

#[tokio::test]
async fn late_turn_start_response_does_not_resurrect_completed_turn() {
    let (_dir, adapter, mut spec) = fixture();
    spec.prompt.text = "late-ack".into();
    let mut run = adapter.start_run(spec).await.unwrap();
    while !matches!(
        event(&mut run.events).await,
        HarnessEvent::TurnCompleted { .. }
    ) {}
    // The initial turn has no follow-up delivery reference.

    assert!(matches!(
        event(&mut run.events).await,
        HarnessEvent::Exited {
            exit: RunExit::Completed,
            ..
        }
    ));
}
#[tokio::test]
async fn stop_before_native_dispatch_prevents_initial_turn() {
    let (dir, adapter, spec) = fixture();
    let mut run = adapter.start_run(spec).await.unwrap();
    assert!(matches!(
        run.control.interrupt().await,
        Err(HarnessError::NotActive)
    ));
    loop {
        if let HarnessEvent::Exited { exit, .. } = event(&mut run.events).await {
            assert_eq!(exit, RunExit::Interrupted);
            break;
        }
    }
    let raw = std::fs::read_to_string(dir.path().join("stdin.jsonl")).unwrap();
    assert!(!raw.contains("turn/start"));
}
#[tokio::test]
async fn api_account_never_dispatches_inference() {
    let (dir, adapter, spec) = fixture();
    std::fs::write(dir.path().join("api-auth"), "fixture").unwrap();
    let mut run = adapter.start_run(spec).await.unwrap();
    assert!(matches!(
        event(&mut run.events).await,
        HarnessEvent::Exited {
            exit: RunExit::Failed { .. },
            ..
        }
    ));
    let raw = std::fs::read_to_string(dir.path().join("stdin.jsonl")).unwrap();
    assert!(!raw.contains("thread/start") && !raw.contains("turn/start"));
}

#[tokio::test]
async fn the_question_tool_is_turned_on_only_where_codex_has_it() {
    for has in [true, false] {
        let (dir, adapter, spec) = fixture();
        if !has {
            std::fs::write(dir.path().join("no-questions"), "").unwrap();
        }
        let plain = told(&spec, None);
        let mut run = adapter.start_run(spec).await.unwrap();
        while !matches!(
            event(&mut run.events).await,
            HarnessEvent::ApprovalRequested(_)
        ) {}
        run.control.shutdown().await.unwrap();
        let args: Vec<String> =
            serde_json::from_slice(&std::fs::read(dir.path().join("argv.json")).unwrap()).unwrap();
        assert_eq!(
            args.iter()
                .any(|a| a == "features.default_mode_request_user_input=true"),
            has
        );
        let thread = std::fs::read_to_string(dir.path().join("stdin.jsonl"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .find(|f| f["method"] == "thread/start" || f["method"] == "thread/resume")
            .unwrap();
        let expected = if has { asking(&plain) } else { plain };
        assert_eq!(thread["params"]["developerInstructions"], expected);
    }
}

#[tokio::test]
async fn a_person_who_turned_questions_off_is_asked_only_when_it_truly_matters() {
    let (dir, adapter, mut spec) = fixture();
    spec.questions = false;
    let plain = told(&spec, None);
    assert!(plain.contains("turned questions off"), "{plain}");
    let mut run = adapter.start_run(spec).await.unwrap();
    while !matches!(
        event(&mut run.events).await,
        HarnessEvent::ApprovalRequested(_)
    ) {}
    run.control.shutdown().await.unwrap();
    let thread = std::fs::read_to_string(dir.path().join("stdin.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .find(|f| f["method"] == "thread/start" || f["method"] == "thread/resume")
        .unwrap();
    // The tool stays, for what only they can answer, without the guidance
    // to ask whenever their decision is needed.
    assert_eq!(
        thread["params"]["developerInstructions"],
        format!("{plain}\n\n{ASKING_SELDOM}")
    );
}

#[tokio::test]
async fn a_read_only_task_still_asks_its_questions() {
    let (dir, adapter, mut spec) = fixture();
    spec.mode = TaskMode::Plan;
    spec.bridge = None;
    spec.prompt.text = "questions".into();
    let mut run = adapter.start_run(spec).await.unwrap();
    let mut declined = vec![];
    let question = loop {
        match event(&mut run.events).await {
            HarnessEvent::ApprovalRequested(event) => break event,
            HarnessEvent::Warning { text } if text.starts_with("This task is read-only") => {
                declined.push(text)
            }
            _ => {}
        }
    };
    // A tool's request could authorize an effect, so only the agent's own
    // question waits for an answer.
    assert_eq!(
        declined,
        vec!["This task is read-only, so Codex's request to use a tool was declined"]
    );
    assert_eq!(question.native_request_id, "72");
    assert_eq!(question.kind, ApprovalKind::Question);
    run.control
        .resolve_approval(
            &question.native_request_id,
            &ApprovalDecision::Answer {
                text: "this one".into(),
                answers: Vec::new(),
            },
        )
        .await
        .unwrap();
    run.control
        .send_message(
            MessageRef(protocol::CommandId::new()),
            PromptInput {
                text: "finish".into(),
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
    let raw = std::fs::read_to_string(dir.path().join("stdin.jsonl")).unwrap();
    let frames: Vec<Value> = raw
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(
        frames
            .iter()
            .any(|v| v["id"] == 73 && v["result"]["action"] == "decline")
    );
    assert!(
        frames.iter().any(|v| v["id"] == 72
            && v["result"] == json!({"answers":{"choice":{"answers":["this one"]}}}))
    );
}

#[tokio::test]
async fn modes_keep_read_only_sandbox_and_user_approvals_on_start_resume_and_followup() {
    for mode in [TaskMode::Ask, TaskMode::Plan, TaskMode::Work] {
        for resume in [None, Some("continued-thread".to_owned())] {
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
            let told = asking(&told(&spec, ports::harness::task_mode_instruction(mode)));
            let mut run = adapter.start_run(spec).await.unwrap();
            if mode == TaskMode::Work {
                while !matches!(
                    event(&mut run.events).await,
                    HarnessEvent::ApprovalRequested(_)
                ) {}
                run.control
                    .resolve_approval("71", &ApprovalDecision::Deny { reason: None })
                    .await
                    .unwrap();
            } else {
                // Requests only Deny could answer are declined without asking.
                let mut declined = vec![];
                while declined.len() < 2 {
                    match event(&mut run.events).await {
                        HarnessEvent::ApprovalRequested(event) => {
                            panic!("a read-only task asked for approval: {event:?}")
                        }
                        HarnessEvent::Warning { text }
                            if text.starts_with("This task is read-only") =>
                        {
                            declined.push(text)
                        }
                        _ => {}
                    }
                }
                assert_eq!(
                    declined,
                    vec![
                        "This task is read-only, so Codex's request to run a command was declined",
                        "This task is read-only, so Codex's request to change files was declined",
                    ]
                );
                assert!(matches!(
                    run.control
                        .resolve_approval("71", &ApprovalDecision::Allow)
                        .await,
                    Err(HarnessError::Protocol(_))
                ));
            }
            run.control
                .send_message(
                    MessageRef(protocol::CommandId::new()),
                    PromptInput {
                        text: "later".into(),
                        images: vec![],
                    },
                    MessageDelivery::InterruptAndSend,
                )
                .await
                .unwrap();
            let mut completed = 0;
            while completed < 2 {
                if matches!(
                    event(&mut run.events).await,
                    HarnessEvent::TurnCompleted { .. }
                ) {
                    completed += 1;
                }
            }
            run.control.shutdown().await.unwrap();
            let raw = std::fs::read_to_string(dir.path().join("stdin.jsonl")).unwrap();
            let frames: Vec<Value> = raw
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
            let thread = frames
                .iter()
                .find(|v| {
                    v["method"]
                        == if resume.is_some() {
                            "thread/resume"
                        } else {
                            "thread/start"
                        }
                })
                .unwrap();
            let mut expected_thread = json!({"cwd":dir.path(), "model":"requested-model", "modelProvider":"openai", "approvalPolicy":"on-request", "approvalsReviewer":"user", "config":{"model_reasoning_effort":"high"}});
            if let Some(session) = &resume {
                expected_thread["threadId"] = json!(session);
                expected_thread["excludeTurns"] = json!(true);
            }
            if mode != TaskMode::Work {
                expected_thread["sandbox"] = json!("read-only");
            }
            expected_thread["developerInstructions"] = json!(told);
            assert_eq!(thread["params"], expected_thread);
            let args: Vec<String> =
                serde_json::from_slice(&std::fs::read(dir.path().join("argv.json")).unwrap())
                    .unwrap();
            let mut expected_args: Vec<String> = [
                "app-server",
                "--listen",
                "stdio://",
                "-c",
                "forced_login_method=\"chatgpt\"",
                "-c",
                "model_provider=\"openai\"",
                "-c",
                "features.default_mode_request_user_input=true",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect();
            // A Work run's bridge server has no permission tool for the
            // model to raise approval cards with.
            if mode == TaskMode::Work {
                let server = args.last().unwrap();
                assert!(server.starts_with("mcp_servers.computer="), "{server}");
                assert!(!server.contains("permission_prompt"), "{server}");
                expected_args.extend(["-c".into(), server.clone()]);
            } else {
                for setting in [
                    "apps={_default={enabled=false}}",
                    "features.plugins=false",
                    "features.hooks=false",
                ] {
                    expected_args.extend(["-c".into(), setting.into()]);
                }
            }
            assert_eq!(args, expected_args);
            assert_eq!(thread["params"]["approvalPolicy"], "on-request");
            assert_eq!(thread["params"]["approvalsReviewer"], "user");
            assert_eq!(
                thread["params"]["sandbox"],
                if mode == TaskMode::Work {
                    Value::Null
                } else {
                    json!("read-only")
                }
            );
            let turns: Vec<_> = frames
                .iter()
                .filter(|v| v["method"] == "turn/start")
                .collect();
            assert_eq!(turns.len(), 2);
            for (turn, text) in turns.into_iter().zip(["start", "later"]) {
                let mut expected_turn = json!({"threadId":"thread-fixture", "input":[{"type":"text", "text":text,"text_elements":[]}], "effort":"high"});
                if mode != TaskMode::Work {
                    expected_turn["sandboxPolicy"] =
                        json!({"type":"readOnly","networkAccess":false});
                    expected_turn["approvalPolicy"] = json!("on-request");
                    expected_turn["approvalsReviewer"] = json!("user");
                }
                assert_eq!(turn["params"], expected_turn);
                if mode == TaskMode::Work {
                    assert!(turn["params"]["sandboxPolicy"].is_null());
                } else {
                    assert_eq!(
                        turn["params"]["sandboxPolicy"],
                        json!({"type":"readOnly","networkAccess":false})
                    );
                    assert_eq!(turn["params"]["approvalPolicy"], "on-request");
                    assert_eq!(turn["params"]["approvalsReviewer"], "user");
                }
            }
            if mode != TaskMode::Work {
                assert!(!frames.iter().any(|v| v["result"]["decision"] == "accept"));
                for id in [json!(71), json!("71")] {
                    assert!(
                        frames
                            .iter()
                            .any(|v| v["id"] == id && v["result"]["decision"] == "decline"),
                        "{id} was not declined"
                    );
                }
                let args: Vec<String> =
                    serde_json::from_slice(&std::fs::read(dir.path().join("argv.json")).unwrap())
                        .unwrap();
                for setting in [
                    "apps={_default={enabled=false}}",
                    "features.plugins=false",
                    "features.hooks=false",
                ] {
                    assert!(args.contains(&setting.to_owned()));
                }
                assert!(!args.iter().any(|arg| arg.contains(".command=")));
            }
        }
    }
}

#[tokio::test]
async fn missing_read_only_schema_is_unsupported_and_never_launches_inference() {
    let (dir, adapter, mut spec) = fixture();
    std::fs::write(dir.path().join("no-read-only"), "").unwrap();
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
async fn unconfirmed_read_only_thread_never_receives_a_turn() {
    let (dir, adapter, mut spec) = fixture();
    spec.mode = TaskMode::Ask;
    spec.bridge = None;
    std::fs::write(dir.path().join("unsafe-response"), "").unwrap();
    let mut run = adapter.start_run(spec).await.unwrap();
    assert!(matches!(
        event(&mut run.events).await,
        HarnessEvent::Exited {
            exit: RunExit::Failed { .. },
            ..
        }
    ));
    let raw = std::fs::read_to_string(dir.path().join("stdin.jsonl")).unwrap();
    assert!(!raw.contains("turn/start"));
}

#[tokio::test]
async fn work_without_a_native_sandbox_never_receives_a_turn() {
    let (dir, adapter, spec) = fixture();
    std::fs::write(dir.path().join("full-access-response"), "").unwrap();
    let mut run = adapter.start_run(spec).await.unwrap();
    loop {
        match event(&mut run.events).await {
            HarnessEvent::TurnStarted => panic!("Work started without a confirmed sandbox"),
            HarnessEvent::Exited { exit, .. } => {
                assert!(
                    matches!(&exit, RunExit::Failed { reason } if reason.contains("sandbox")),
                    "{exit:?}"
                );
                break;
            }
            _ => {}
        }
    }
    let raw = std::fs::read_to_string(dir.path().join("stdin.jsonl")).unwrap();
    assert!(!raw.contains("turn/start"));
}

#[tokio::test]
async fn server_resolution_without_a_decision_keeps_outcome_unknown() {
    let (_dir, adapter, mut spec) = fixture();
    spec.prompt.text = "resolved-elsewhere".into();
    let mut run = adapter.start_run(spec).await.unwrap();
    loop {
        if let HarnessEvent::ApprovalClosed {
            native_request_id,
            outcome,
        } = event(&mut run.events).await
        {
            assert_eq!(native_request_id, "71");
            assert_eq!(outcome, None);
            break;
        }
    }
    run.control.shutdown().await.unwrap();
}

#[tokio::test]
async fn missing_native_instruction_field_frames_every_input_including_resume_and_steering() {
    for mode in [TaskMode::Ask, TaskMode::Plan] {
        for resume in [None, Some("continued-thread".to_owned())] {
            let (dir, adapter, mut spec) = fixture();
            std::fs::write(dir.path().join("no-instructions"), "").unwrap();
            spec.mode = mode;
            spec.bridge = None;
            spec.resume_session = resume;
            let original = spec.prompt.text.clone();
            let told = asking(&told(&spec, ports::harness::task_mode_instruction(mode)));
            let mut run = adapter.start_run(spec).await.unwrap();
            // The turn is underway once its first request has been declined.
            while !matches!(
                event(&mut run.events).await,
                HarnessEvent::Warning { text } if text.starts_with("This task is read-only")
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
            run.control.shutdown().await.unwrap();
            let frames: Vec<Value> = std::fs::read_to_string(dir.path().join("stdin.jsonl"))
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
            let thread = frames
                .iter()
                .find(|f| f["method"] == "thread/start" || f["method"] == "thread/resume")
                .unwrap();
            assert!(thread["params"]["developerInstructions"].is_null());
            let inputs: Vec<_> = frames
                .iter()
                .filter(|f| f["method"] == "turn/start" || f["method"] == "turn/steer")
                .collect();
            assert_eq!(inputs.len(), 2);
            for (input, text) in inputs.iter().zip([original.as_str(), "follow-up"]) {
                assert_eq!(
                    input["params"]["input"][0]["text"],
                    format!("<task-mode-intent>\n{told}\n</task-mode-intent>\n\n{text}")
                );
            }
        }
    }
}

#[test]
fn work_is_told_about_the_tasks_scratch_folder() {
    let (dir, _adapter, mut spec) = fixture();
    spec.scratch = Some(dir.path().join("scratch"));
    let scratch = ports::harness::scratch_instruction(&spec).unwrap();
    assert_eq!(
        task_instructions(&spec).unwrap(),
        Some(format!("{}\n\n{scratch}", told(&spec, None)))
    );
    spec.mode = TaskMode::Plan;
    assert!(
        !task_instructions(&spec)
            .unwrap()
            .unwrap()
            .contains(&scratch)
    );
}

#[test]
fn mission_and_mode_share_developer_instructions_without_changing_work() {
    let (_dir, _adapter, mut spec) = fixture();
    assert_eq!(task_instructions(&spec).unwrap(), Some(told(&spec, None)));
    let mission = protocol::model::Mission {
        goal: "Inspect".into(),
        bounds: vec![],
        completion_checks: vec![],
        stop_point: None,
    };
    let json = serde_json::to_string(&mission).unwrap();
    spec.mission = Some(mission);
    assert_eq!(
        task_instructions(&spec).unwrap().unwrap(),
        told(&spec, Some(&json))
    );
    for mode in [TaskMode::Ask, TaskMode::Plan] {
        spec.mode = mode;
        assert_eq!(
            task_instructions(&spec).unwrap().unwrap(),
            told(
                &spec,
                Some(&format!(
                    "{json}\n\n{}",
                    ports::harness::task_mode_instruction(mode).unwrap()
                ))
            )
        );
    }
}

#[test]
fn a_chosen_branch_joins_the_developer_instructions() {
    let (_dir, _adapter, mut spec) = fixture();
    let chosen = protocol::model::TaskBranch {
        name: "feature/search".into(),
        new: true,
    };
    spec.branch = Some(chosen.clone());
    let branch = ports::harness::branch_instruction(&chosen);
    assert!(branch.contains("git switch -c feature/search"));
    assert_eq!(
        task_instructions(&spec).unwrap(),
        Some(told(&spec, Some(&branch)))
    );
    spec.mode = TaskMode::Ask;
    assert_eq!(
        task_instructions(&spec).unwrap().unwrap(),
        told(
            &spec,
            Some(&format!(
                "{}\n\n{branch}",
                ports::harness::task_mode_instruction(TaskMode::Ask).unwrap()
            ))
        )
    );
    spec.mode = TaskMode::Work;
    spec.resume_from_mode = Some(TaskMode::Plan);
    assert_eq!(
        task_instructions(&spec).unwrap().unwrap(),
        format!(
            "{}\n\nWork mode: carry out the plan reviewed by the user. The earlier read-only planning instruction has ended. Follow the native permissions selected for this task.",
            told(&spec, Some(&branch))
        )
    );
    // Work again after the person asked a Work task about what it did.
    spec.resume_from_mode = Some(TaskMode::Ask);
    assert_eq!(
        task_instructions(&spec).unwrap().unwrap(),
        format!(
            "{}\n\nWork mode: the earlier read-only instruction has ended. Follow the native permissions selected for this task.",
            told(&spec, Some(&branch))
        )
    );
}

#[tokio::test]
async fn withdraw_held_message_but_never_recall_a_native_write() {
    let (dir, adapter, spec) = fixture();
    let mut run = adapter.start_run(spec).await.unwrap();
    while !matches!(
        event(&mut run.events).await,
        HarnessEvent::ApprovalRequested(_)
    ) {}
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
    // Stop withdraws held follow-ups, so this one reaches the boundary by interrupting.
    send(dispatched, "later", MessageDelivery::InterruptAndSend)
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
    while !matches!(event(&mut run.events).await, HarnessEvent::Exited { .. }) {}
    let input = std::fs::read_to_string(dir.path().join("stdin.jsonl")).unwrap();
    assert!(!input.contains("withdraw this"));
    assert!(input.contains("later"));
    run.control.shutdown().await.unwrap();
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
                        images: vec![]
                    },
                    MessageDelivery::NextTurn
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
async fn withdrawn_before_exit(run: &mut RunStarted) -> (Vec<MessageRef>, RunExit) {
    let mut withdrawn = Vec::new();
    loop {
        match event(&mut run.events).await {
            HarnessEvent::MessageDelivery { message_ref, state } => {
                assert_eq!(state, DeliveryState::Cancelled);
                withdrawn.push(message_ref);
            }
            HarnessEvent::Exited { exit, .. } => return (withdrawn, exit),
            _ => {}
        }
    }
}

#[tokio::test]
async fn stop_withdraws_every_held_follow_up() {
    let (dir, adapter, spec) = fixture();
    let mut run = adapter.start_run(spec).await.unwrap();
    while !matches!(
        event(&mut run.events).await,
        HarnessEvent::ApprovalRequested(_)
    ) {}
    let held = held_follow_ups(&run).await;
    run.control.interrupt().await.unwrap();
    assert!(matches!(
        run.control
            .send_message(
                MessageRef(protocol::CommandId::new()),
                PromptInput {
                    text: "sent after stop".into(),
                    images: vec![]
                },
                MessageDelivery::NextTurn
            )
            .await,
        Err(HarnessError::NotActive)
    ));
    let (withdrawn, exit) = withdrawn_before_exit(&mut run).await;
    assert_eq!(withdrawn, held);
    assert_eq!(exit, RunExit::Interrupted);
    let raw = std::fs::read_to_string(dir.path().join("stdin.jsonl")).unwrap();
    assert!(!raw.contains("after stop"));
    assert_eq!(raw.matches("turn/start").count(), 1);
}

#[tokio::test]
async fn images_too_large_for_one_message_are_refused_without_ending_the_run() {
    let (dir, adapter, spec) = fixture();
    let mut run = adapter.start_run(spec).await.unwrap();
    while !matches!(
        event(&mut run.events).await,
        HarnessEvent::ApprovalRequested(_)
    ) {}
    // Each image is within the per-image limit; together they are not.
    let mut images = Vec::new();
    for name in ["one.png", "two.png"] {
        std::fs::write(dir.path().join(name), vec![0u8; 13 * 1024 * 1024]).unwrap();
        images.push(ImageInput {
            path: name.into(),
            media_type: "image/png".into(),
        });
    }
    let message_ref = MessageRef(protocol::CommandId::new());
    let refused = run
        .control
        .send_message(
            message_ref,
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
            HarnessEvent::MessageDelivery {
                message_ref: sent, ..
            } => {
                assert_ne!(sent, message_ref, "the refused message was never written")
            }
            HarnessEvent::Exited { exit, .. } => {
                assert_eq!(exit, RunExit::Interrupted);
                break;
            }
            _ => {}
        }
    }
}

#[tokio::test]
async fn force_stop_withdraws_every_held_follow_up() {
    let (dir, adapter, spec) = fixture();
    let mut run = adapter.start_run(spec).await.unwrap();
    while !matches!(
        event(&mut run.events).await,
        HarnessEvent::ApprovalRequested(_)
    ) {}
    let held = held_follow_ups(&run).await;
    run.control.terminate().await.unwrap();
    let (withdrawn, exit) = withdrawn_before_exit(&mut run).await;
    assert_eq!(withdrawn, held);
    assert_eq!(exit, RunExit::Interrupted);
    let raw = std::fs::read_to_string(dir.path().join("stdin.jsonl")).unwrap();
    assert!(!raw.contains("after stop"));
}

#[tokio::test]
async fn a_turn_refused_for_usage_reports_the_limit_once_with_its_reset_time() {
    let (_dir, adapter, mut spec) = fixture();
    spec.prompt.text = "usage-limit".into();
    spec.resume_session = None;
    let mut run = adapter.start_run(spec).await.unwrap();
    let mut limits = Vec::new();
    loop {
        match event(&mut run.events).await {
            HarnessEvent::UsageLimited { resets_at } => limits.push(resets_at),
            HarnessEvent::Warning { text } => {
                assert!(!text.contains("error"), "{text}")
            }
            HarnessEvent::TurnCompleted { .. } => break,
            _ => {}
        }
    }
    // The five-hour window is exhausted; the weekly one is not.
    assert_eq!(limits, [Some(protocol::time::Timestamp(1_790_000_000_000))]);
    run.control.shutdown().await.unwrap();
}

// These notes appear in the task's transcript, where "app-server" and
// "native" mean nothing; Codex's own reason is kept because it says more.
#[tokio::test]
async fn an_error_during_a_turn_is_reported_in_plain_words() {
    let (_dir, adapter, mut spec) = fixture();
    spec.prompt.text = "turn-error".into();
    spec.resume_session = None;
    let mut run = adapter.start_run(spec).await.unwrap();
    let mut notes = Vec::new();
    let mut completed = false;
    loop {
        match event(&mut run.events).await {
            HarnessEvent::Warning { text } => {
                notes.push(text);
                if completed {
                    break;
                }
            }
            HarnessEvent::TurnCompleted { .. } => completed = true,
            _ => {}
        }
    }
    assert_eq!(
        notes,
        [
            "Codex hit an error and is trying again: stream disconnected before completion",
            "Codex reported an error",
            "Codex ended this turn with an error",
        ]
    );
    run.control.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_token_count_beyond_the_wire_bound_is_not_reported() {
    let (dir, adapter, mut spec) = fixture();
    spec.prompt.text = "complete".into();
    spec.resume_session = None;
    std::fs::write(
        dir.path().join("token-usage.json"),
        format!(
            r#"{{"inputTokens":{},"outputTokens":4}}"#,
            protocol::MAX_SAFE_INTEGER + 1
        ),
    )
    .unwrap();
    let mut run = adapter.start_run(spec).await.unwrap();
    loop {
        if let HarnessEvent::TurnCompleted { usage } = event(&mut run.events).await {
            let usage = usage.unwrap();
            assert_eq!((usage.input_tokens, usage.output_tokens), (None, Some(4)));
            break;
        }
    }
    run.control.shutdown().await.unwrap();
}

#[tokio::test]
async fn sandbox_notification_updates_readiness_beyond_the_transcript() {
    let (dir, adapter, spec) = fixture();
    std::fs::write(
        dir.path().join("sandbox-warning"),
        "Sandbox cannot start: fixture user namespaces are disabled",
    )
    .unwrap();
    let mut run = adapter.start_run(spec).await.unwrap();
    loop {
        if let HarnessEvent::ReadinessWarning { reason } = event(&mut run.events).await {
            assert!(reason.contains("fixture user namespaces"));
            break;
        }
    }
    run.control.shutdown().await.unwrap();
    std::fs::remove_file(dir.path().join("sandbox-warning")).unwrap();
    let info = adapter.probe().await;
    assert!(
        matches!(info.capabilities.start_run, Availability::TemporarilyUnavailable { reason } if reason.contains("fixture user namespaces"))
    );
}

#[tokio::test]
async fn warnings_and_notices_reach_the_person_in_plain_words_once() {
    let (dir, adapter, mut spec) = fixture();
    // Codex 0.153.2 without its helper beside it (path replaced), sent at
    // every thread start, then a warning in Codex's own words.
    let helper = r#"{"method":"warning","params":{"threadId":"thread-fixture","message":"Code Mode is unavailable because failed to spawn code-mode host /opt/codex/codex-code-mode-host: host executable was not found. Code mode will fail closed; enable `features.code_mode_host` and install `codex-code-mode-host`."}}"#;
    let own = r#"{"method":"warning","params":{"threadId":null,"message":"Model metadata for `fixture-model` not found. Defaulting to fallback metadata; this can degrade performance and cause issues."}}"#;
    // Codex 0.157.1 resuming a thread with its whole history, and the goal
    // it cleared.
    let deprecated = r#"{"method":"deprecationNotice","params":{"summary":"Full-history hydration is deprecated for paginated threads; use `excludeTurns: true`, then page with `thread/turns/list` and `thread/items/list`.","details":null}}"#;
    let goal = r#"{"method":"thread/goal/cleared","params":{"threadId":"thread-fixture"}}"#;
    std::fs::write(
        dir.path().join("after-thread.jsonl"),
        [helper, helper, own, deprecated, goal].join("\n"),
    )
    .unwrap();
    spec.prompt.text = "complete".into();
    let mut run = adapter.start_run(spec).await.unwrap();
    let mut notes = vec![];
    loop {
        match event(&mut run.events).await {
            HarnessEvent::Warning { text } => notes.push(text),
            HarnessEvent::Exited { .. } => break,
            _ => {}
        }
    }
    let warnings: Vec<_> = notes
        .iter()
        .filter(|n| {
            n.contains("codex-code-mode-host")
                || n.starts_with("Codex warns")
                || n.starts_with("Codex notes")
        })
        .collect();
    assert_eq!(
        warnings,
        [
            "Codex can’t run commands in this task: codex-code-mode-host, the helper it runs them through, is missing beside Codex on the computer. Reinstall Codex, or choose a complete installation.",
            "Codex warns: Model metadata for `fixture-model` not found. Defaulting to fallback metadata; this can degrade performance and cause issues.",
            "Codex notes: Full-history hydration is deprecated for paginated threads; use `excludeTurns: true`, then page with `thread/turns/list` and `thread/items/list`.",
        ]
    );
    assert!(
        !notes.iter().any(
            |n| ["(warning)", "(deprecationNotice)", "(thread/goal/cleared)"]
                .iter()
                .any(|kind| n.contains(kind))
        ),
        "{notes:?}"
    );
}

#[tokio::test]
async fn reviewed_plan_resumes_with_confirmed_work_permissions() {
    let (dir, adapter, mut spec) = fixture();
    spec.resume_from_mode = Some(TaskMode::Plan);
    spec.prompt.text = "Carry out the reviewed parser plan".into();
    let mut run = adapter.start_run(spec).await.unwrap();
    while !matches!(
        event(&mut run.events).await,
        HarnessEvent::ApprovalRequested(_)
    ) {}
    let lines: Vec<Value> = std::fs::read_to_string(dir.path().join("stdin.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let resume = lines
        .iter()
        .find(|line| line["method"] == "thread/resume")
        .unwrap();
    assert_eq!(resume["params"]["threadId"], "resume-fixture");
    assert_eq!(resume["params"]["sandbox"], "workspace-write");
    assert_eq!(resume["params"]["approvalPolicy"], "on-request");
    assert_eq!(resume["params"]["approvalsReviewer"], "user");
    assert!(
        resume["params"]["developerInstructions"]
            .as_str()
            .unwrap()
            .contains("earlier read-only planning instruction has ended")
    );
    assert!(lines.iter().any(|line| line["method"] == "turn/start"
        && line["params"]["input"][0]["text"] == "Carry out the reviewed parser plan"));
    run.control.shutdown().await.unwrap();
}

#[tokio::test]
async fn work_after_a_read_only_turn_resumes_with_confirmed_work_permissions() {
    let (dir, adapter, mut spec) = fixture();
    spec.resume_from_mode = Some(TaskMode::Ask);
    spec.prompt.text = "Now fix the parser".into();
    let mut run = adapter.start_run(spec).await.unwrap();
    while !matches!(
        event(&mut run.events).await,
        HarnessEvent::ApprovalRequested(_)
    ) {}
    let lines: Vec<Value> = std::fs::read_to_string(dir.path().join("stdin.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let resume = lines
        .iter()
        .find(|line| line["method"] == "thread/resume")
        .unwrap();
    // The thread's last turn was read-only, which Codex would otherwise keep.
    assert_eq!(resume["params"]["sandbox"], "workspace-write");
    assert!(
        resume["params"]["developerInstructions"]
            .as_str()
            .unwrap()
            .contains("earlier read-only instruction has ended")
    );
    run.control.shutdown().await.unwrap();
    // A turn that only answers keeps the thread read-only.
    let (dir, adapter, mut spec) = fixture();
    spec.mode = TaskMode::Ask;
    spec.resume_from_mode = Some(TaskMode::Work);
    let mut run = adapter.start_run(spec).await.unwrap();
    loop {
        if let Some(resume) = std::fs::read_to_string(dir.path().join("stdin.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .find(|line| line["method"] == "thread/resume")
        {
            assert_eq!(resume["params"]["sandbox"], "read-only");
            break;
        }
        event(&mut run.events).await;
    }
    run.control.shutdown().await.unwrap();
}

#[tokio::test]
async fn carrying_plan_requires_a_supported_mode_change_before_sending() {
    let (dir, adapter, mut spec) = fixture();
    std::fs::write(dir.path().join("no-instructions"), "").unwrap();
    spec.resume_from_mode = Some(TaskMode::Plan);
    assert!(matches!(
        adapter.start_run(spec).await,
        Err(HarnessError::Unsupported(_))
    ));
    assert!(!dir.path().join("argv.json").exists());
}

#[tokio::test]
async fn carrying_plan_waits_for_native_work_acknowledgement() {
    let (dir, adapter, mut spec) = fixture();
    std::fs::write(dir.path().join("read-only-work-response"), "").unwrap();
    spec.resume_from_mode = Some(TaskMode::Plan);
    let mut run = adapter.start_run(spec).await.unwrap();
    loop {
        match event(&mut run.events).await {
            HarnessEvent::TurnStarted => panic!("plan sent before Work was confirmed"),
            HarnessEvent::Exited { exit, .. } => {
                assert!(matches!(exit, RunExit::Failed { .. }));
                break;
            }
            _ => {}
        }
    }
    let lines = std::fs::read_to_string(dir.path().join("stdin.jsonl")).unwrap();
    assert!(!lines.contains("turn/start"));
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

/// An app-server that exits leaving a helper in its process group, holding its
/// output open. Returns once the server has exited, with the helper's id.
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
    until("the app-server should exit", || {
        id("helper.pid").is_some() && id("fixture.pid").is_some_and(|server| !running(server))
    })
    .await;
    // An adapter that reaps the server now can no longer name its group.
    // Give it time to, though this adapter must not.
    let server = id("fixture.pid").unwrap();
    let started = tokio::time::Instant::now();
    while std::path::Path::new(&format!("/proc/{server}")).exists()
        && started.elapsed() < Duration::from_millis(500)
    {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let helper = id("helper.pid").unwrap();
    (dir, run, helper)
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn force_stop_reaches_a_helper_that_outlives_the_app_server() {
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
async fn a_helper_that_outlives_the_app_server_ends_with_its_run() {
    let (_dir, mut run, helper) = outlived_by_a_helper().await;
    // The run ends once the grace for the server's output has passed.
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
fn a_chosen_agent_or_skill_leads_the_developer_instructions() {
    use ports::{NativeEntry, RunStart};
    let (dir, _adapter, mut spec) = fixture();
    spec.resume_session = None;
    let entry = |harness, name: &str, project| {
        Some(NativeEntry {
            harness,
            name: name.into(),
            project,
        })
    };
    // Codex keeps no agents of its own, so any agent's body is given.
    for native in [
        None,
        entry(HarnessKind::ClaudeCode, "video-producer", false),
    ] {
        spec.start = Some(RunStart::Agent {
            name: "Video producer".into(),
            instructions: "Make short videos.".into(),
            native,
            file: "/agents/video-producer.md".into(),
            digest: "d".into(),
        });
        let text = task_instructions(&spec).unwrap().unwrap();
        assert!(text.starts_with("For this task, act as the agent “Video producer”"));
        assert!(text.ends_with(&told(&spec, Some("Make short videos."))));
        assert_eq!(invoked_skill(&spec), None);
    }
    spec.mode = TaskMode::Plan;
    assert_eq!(
        task_instructions(&spec)
            .unwrap()
            .unwrap()
            .split_once("Make short videos.\n\n")
            .unwrap()
            .1,
        told(&spec, ports::harness::task_mode_instruction(TaskMode::Plan))
    );
    spec.mode = TaskMode::Work;

    let skill_file = dir.path().join("launch-video-clone").join("SKILL.md");
    let skill = |native| RunStart::Skill {
        name: "Launch video".into(),
        skill_file: skill_file.clone(),
        native,
        instructions: Some("Render with the clone pipeline.".into()),
        digest: "d".into(),
    };
    let told = format!(
        "This task uses the skill “Launch video”: follow its instructions below as well as these. Paths they mention are relative to {}.\n\nRender with the clone pipeline.",
        skill_file.parent().unwrap().display()
    );
    // Codex's own skill, in Work, goes with the first prompt and adds nothing here.
    spec.start = Some(skill(entry(
        HarnessKind::Codex,
        "launch-video-clone",
        false,
    )));
    assert_eq!(
        task_instructions(&spec).unwrap(),
        Some(crate::tests::told(&spec, None))
    );
    assert_eq!(
        invoked_skill(&spec),
        Some(("launch-video-clone", skill_file.as_path()))
    );
    // A resumed thread may have begun where the skill wasn't used, as a plan
    // Work carries out, so it is given the instructions instead.
    spec.resume_session = Some("continued-thread".into());
    for from in [TaskMode::Plan, TaskMode::Work] {
        spec.resume_from_mode = Some(from);
        assert_eq!(invoked_skill(&spec), None);
        assert!(
            task_instructions(&spec)
                .unwrap()
                .unwrap()
                .starts_with(&told)
        );
    }
    spec.resume_session = None;
    spec.resume_from_mode = None;
    // Ask and Plan keep their own tools: the skill is pointed to, not used.
    for mode in [TaskMode::Ask, TaskMode::Plan] {
        spec.mode = mode;
        assert_eq!(invoked_skill(&spec), None);
        assert!(
            task_instructions(&spec)
                .unwrap()
                .unwrap()
                .starts_with(&told)
        );
    }
    spec.mode = TaskMode::Work;
    // A project's own skill only where the project's settings apply.
    spec.start = Some(skill(entry(HarnessKind::Codex, "launch-video-clone", true)));
    assert_eq!(invoked_skill(&spec), None);
    assert!(
        task_instructions(&spec)
            .unwrap()
            .unwrap()
            .starts_with(&told)
    );
    spec.project_settings = true;
    assert!(invoked_skill(&spec).is_some());
    // Ophio's own, or Claude's, is pointed to.
    for native in [
        None,
        entry(HarnessKind::ClaudeCode, "launch-video-clone", false),
    ] {
        spec.start = Some(skill(native));
        assert_eq!(invoked_skill(&spec), None);
        assert!(
            task_instructions(&spec)
                .unwrap()
                .unwrap()
                .starts_with(&told)
        );
    }
    spec.start = Some(RunStart::Scratch);
    assert_eq!(
        task_instructions(&spec).unwrap(),
        Some(crate::tests::told(&spec, None))
    );
}

#[tokio::test]
async fn a_native_skill_is_sent_with_the_first_prompt_only_where_codex_can_take_it() {
    use ports::{NativeEntry, RunStart};
    for (supported, resume) in [
        (true, None),
        (false, None),
        (true, Some("continued-thread")),
        (false, Some("continued-thread")),
    ] {
        let (dir, adapter, mut spec) = fixture();
        if supported {
            std::fs::write(dir.path().join("skill-input"), "").unwrap();
        }
        spec.bridge = None;
        spec.resume_session = resume.map(str::to_owned);
        let skill_file = dir.path().join("launch-video-clone").join("SKILL.md");
        spec.start = Some(RunStart::Skill {
            name: "Launch video".into(),
            skill_file: skill_file.clone(),
            native: Some(NativeEntry {
                harness: HarnessKind::Codex,
                name: "launch-video-clone".into(),
                project: false,
            }),
            instructions: Some("Render it.".into()),
            digest: "d".into(),
        });
        let person = &told(&spec, None);
        let mut run = adapter.start_run(spec).await.unwrap();
        while !matches!(
            event(&mut run.events).await,
            HarnessEvent::ApprovalRequested(_)
        ) {}
        run.control.shutdown().await.unwrap();
        let frames: Vec<Value> = std::fs::read_to_string(dir.path().join("stdin.jsonl"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let thread = frames
            .iter()
            .find(|f| f["method"] == "thread/start" || f["method"] == "thread/resume")
            .unwrap();
        let turn = frames.iter().find(|f| f["method"] == "turn/start").unwrap();
        let input = turn["params"]["input"].as_array().unwrap();
        if supported && resume.is_none() {
            assert_eq!(thread["params"]["developerInstructions"], asking(person));
            assert_eq!(input.len(), 2);
            assert_eq!(input[0]["text"], "$launch-video-clone start");
            assert_eq!(
                input[1],
                json!({"type": "skill", "name": "launch-video-clone", "path": skill_file})
            );
        } else {
            // An older Codex is given the skill instead, in every run, and a
            // resumed thread is too, as it may never have invoked it.
            let told = thread["params"]["developerInstructions"].as_str().unwrap();
            assert!(
                told.starts_with("This task uses the skill “Launch video”"),
                "{told}"
            );
            assert!(
                told.ends_with(&format!("Render it.\n\n{}", asking(person))),
                "{told}"
            );
            assert_eq!(input.len(), 1);
            assert_eq!(input[0]["text"], "start");
        }
    }
}

#[test]
fn disabled_skills_use_one_toml_override_and_filter_claude() {
    let (_dir, _adapter, mut spec) = fixture();
    let skill = spec.cwd.join("a SKILL.md");
    std::fs::write(&skill, "skill").unwrap();
    spec.disabled.skills.push(ports::DisabledEntry {
        path: skill.clone(),
        name: "one".into(),
        harness: None,
    });
    spec.disabled.skills.push(ports::DisabledEntry {
        path: "/tmp/other.md".into(),
        name: "other".into(),
        harness: Some(HarnessKind::ClaudeCode),
    });
    let args = arguments(&spec, None, false, None, &[]).unwrap();
    assert!(args.windows(2).any(|pair| pair[0] == "-c"
        && pair[1]
            == format!(
                "skills.config=[{{path={},enabled=false}}]",
                toml_string(&skill.to_string_lossy())
            )));
    assert_eq!(
        args.iter()
            .filter(|value| value.starts_with("skills.config="))
            .count(),
        1
    );
}

#[test]
fn disabled_project_docs_restore_kept_chain_and_override_fallback() {
    let (_dir, _adapter, mut spec) = fixture();
    spec.resume_session = None;
    std::fs::create_dir(spec.cwd.join(".git")).unwrap();
    let root = spec.cwd.clone();
    let nested = root.join("nested");
    std::fs::create_dir(&nested).unwrap();
    std::fs::write(root.join("AGENTS.md"), "root instructions").unwrap();
    std::fs::write(nested.join("AGENTS.md"), "nested instructions").unwrap();
    spec.cwd = nested.clone();
    spec.disabled.files.push(root.join("AGENTS.md"));
    let args = arguments(&spec, None, false, None, &[]).unwrap();
    assert!(
        args.windows(2)
            .any(|pair| pair == ["-c", "project_doc_max_bytes=0"])
    );
    let text = task_instructions(&spec).unwrap().unwrap();
    assert!(text.starts_with(&format!(
        "# AGENTS.md instructions for {}",
        nested.display()
    )));
    assert!(text.contains("nested instructions"));
    assert!(!text.contains("root instructions"));
    spec.disabled.files.clear();
    let override_file = nested.join("AGENTS.override.md");
    std::fs::write(&override_file, "override instructions").unwrap();
    spec.disabled.files.push(override_file);
    let text = task_instructions(&spec).unwrap().unwrap();
    assert!(text.contains("root instructions"));
    assert!(text.contains("nested instructions"));
    assert!(!text.contains("override instructions"));
    spec.resume_session = Some("prior".into());
    assert!(
        task_instructions(&spec)
            .unwrap()
            .unwrap()
            .contains("nested instructions")
    );
    assert!(
        arguments(&spec, None, false, None, &[])
            .unwrap()
            .contains(&"project_doc_max_bytes=0".into())
    );
    spec.disabled.files = vec![nested.join("below/AGENTS.md")];
    assert!(
        !arguments(&spec, None, false, None, &[])
            .unwrap()
            .contains(&"project_doc_max_bytes=0".into())
    );
}

#[test]
fn a_read_only_turn_answering_on_the_checklist_gets_the_disabled_block() {
    let (_dir, _adapter, mut spec) = fixture();
    spec.mode = TaskMode::Ask;
    spec.bridge.as_mut().unwrap().checklist_answers = true;
    let skill = spec.cwd.join("SKILL.md");
    std::fs::write(&skill, "skill").unwrap();
    spec.disabled.skills.push(ports::DisabledEntry {
        path: skill.clone(),
        name: "hidden".into(),
        harness: None,
    });
    let args = arguments(&spec, Some("computer"), false, None, &[]).unwrap();
    assert!(args.iter().any(|value| value
        == &format!(
            "skills.config=[{{path={},enabled=false}}]",
            toml_string(&skill.to_string_lossy())
        )));
}
