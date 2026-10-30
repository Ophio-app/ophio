// SPDX-License-Identifier: Apache-2.0
use super::*;
use protocol::model::LaunchPolicy;

#[tokio::test]
async fn launch_flags_configure_process_thread_and_every_turn_including_resume() {
    for (policy, approval, sandbox, native_type) in [
        (
            LaunchPolicy::Standard,
            "on-request",
            "workspace-write",
            "workspaceWrite",
        ),
        (
            LaunchPolicy::BypassPermissions,
            "never",
            "danger-full-access",
            "dangerFullAccess",
        ),
        (
            LaunchPolicy::Sandboxed,
            "never",
            "workspace-write",
            "workspaceWrite",
        ),
    ] {
        for resume in [false, true] {
            let (dir, adapter, mut spec) = fixture();
            spec.launch_policy = Some(policy);
            spec.resume_session = resume.then(|| "saved-session".into());
            spec.resume_from_mode = resume.then_some(TaskMode::Plan);
            spec.prompt.text = "wait".into();
            let args = arguments(&spec, None, false, None, &[]).unwrap();
            assert!(args.contains(&format!("approval_policy=\"{approval}\"")));
            assert!(args.contains(&format!("sandbox_mode=\"{sandbox}\"")));
            let mut run = adapter.start_run(spec).await.unwrap();
            loop {
                if matches!(event(&mut run.events).await, HarnessEvent::TurnStarted) {
                    break;
                }
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
            let input = std::fs::read_to_string(dir.path().join("stdin.jsonl")).unwrap();
            let values: Vec<Value> = input
                .lines()
                .map(|l| serde_json::from_str(l).unwrap())
                .collect();
            let thread = values
                .iter()
                .find(|v| {
                    v["method"]
                        == if resume {
                            "thread/resume"
                        } else {
                            "thread/start"
                        }
                })
                .unwrap();
            assert_eq!(thread["params"]["approvalPolicy"], approval);
            assert_eq!(thread["params"]["sandbox"], sandbox);
            let turns: Vec<_> = values
                .iter()
                .filter(|v| v["method"] == "turn/start")
                .collect();
            assert_eq!(turns.len(), 2);
            for turn in turns {
                assert_eq!(turn["params"]["approvalPolicy"], approval);
                assert_eq!(turn["params"]["approvalsReviewer"], "user");
                assert_eq!(turn["params"]["sandboxPolicy"]["type"], native_type);
            }
        }
    }
}

#[tokio::test]
async fn bypass_is_not_silently_replaced_with_a_different_native_policy() {
    let (dir, adapter, mut spec) = fixture();
    std::fs::write(dir.path().join("unsafe-response"), "").unwrap();
    spec.launch_policy = Some(LaunchPolicy::BypassPermissions);
    let mut run = adapter.start_run(spec).await.unwrap();
    loop {
        match event(&mut run.events).await {
            HarnessEvent::SessionStarted { .. } => panic!("unconfirmed policy was accepted"),
            HarnessEvent::Exited { exit, .. } => {
                assert!(matches!(exit, RunExit::Failed { .. }));
                break;
            }
            _ => {}
        }
    }
    let input = std::fs::read_to_string(dir.path().join("stdin.jsonl")).unwrap();
    assert!(!input.contains("turn/start"));
}

#[test]
fn native_acknowledgement_must_match_all_selected_controls() {
    let (_dir, _adapter, mut spec) = fixture();
    spec.launch_policy = Some(LaunchPolicy::BypassPermissions);
    let response = json!({"approvalPolicy":"never", "approvalsReviewer":"user", "sandbox":{"type":"dangerFullAccess"}});
    assert!(crate::launch::confirms(&spec, &response));
    for (field, value) in [
        ("approvalPolicy", json!("on-request")),
        ("approvalsReviewer", json!("auto_review")),
        ("sandbox", json!({"type":"workspaceWrite"})),
    ] {
        let mut changed = response.clone();
        changed[field] = value;
        assert!(!crate::launch::confirms(&spec, &changed));
        changed.as_object_mut().unwrap().remove(field);
        assert!(!crate::launch::confirms(&spec, &changed));
    }
}

#[test]
fn read_only_is_not_escalated_and_cross_agent_flags_are_refused() {
    let (_dir, _adapter, mut spec) = fixture();
    for mode in [TaskMode::Ask, TaskMode::Plan] {
        spec.mode = mode;
        spec.launch_policy = Some(LaunchPolicy::BypassPermissions);
        assert!(crate::launch::policy(&spec).is_none());
        let own = ["mine".to_owned()];
        let args = arguments(&spec, None, false, None, &own).unwrap();
        assert!(!args.iter().any(|a| a.contains("danger-full-access")));
        // The person's servers, plugins and hooks are off for the run.
        for setting in [
            r#"mcp_servers={"mine"={enabled=false}}"#,
            "features.plugins=false",
            "features.hooks=false",
            "apps={_default={enabled=false}}",
        ] {
            assert!(args.iter().any(|a| a == setting), "{setting}");
        }
    }
    spec.mode = TaskMode::Work;
    spec.launch_policy = Some(LaunchPolicy::AcceptEdits);
    assert!(arguments(&spec, None, false, None, &[]).is_err());
}

#[test]
fn a_browsing_work_run_gets_the_browser_tools_with_the_token_kept_off_the_command_line() {
    let (_dir, _adapter, mut spec) = fixture();
    spec.mode = TaskMode::Work;
    spec.bridge = Some(ports::ToolBridge {
        server_name: "computer".into(),
        command: "/opt/ophio/computer \"mcp\"".into(),
        args: vec![],
        env: vec![
            (ports::bridge::RUN_TOKEN_ENV.into(), "run-secret".into()),
            (ports::bridge::SOCKET_ENV.into(), "/run/ophio/bridge".into()),
        ],
        permission_tool: Some("permission_prompt".into()),
        computer_use: false,
        browser: true,
        github: false,
        checklist_answers: false,
    });
    let (file, server) = bridge_server(&spec).unwrap().unwrap();
    let args = arguments(&spec, Some(&server), false, None, &[]).unwrap();
    assert!(!args.iter().any(|a| a.contains("run-secret")));
    assert!(!args.iter().any(|a| a.starts_with("mcp_servers={")));
    let server = args
        .iter()
        .find(|a| a.starts_with("mcp_servers.computer="))
        .unwrap();
    assert!(server.contains(r#"command="/opt/ophio/computer \"mcp\"""#));
    assert!(server.contains(&format!(
        r#"args=["--bridge-file","{}"]"#,
        file.path().display()
    )));
    assert!(server.contains(r#"default_tools_approval_mode="approve""#));
    for tool in ports::bridge::BROWSER_TOOLS
        .iter()
        .chain([&"request_human"])
        .chain(ports::bridge::PERSON_TOOLS)
    {
        assert!(server.contains(&format!("\"{tool}\"")), "{tool}");
    }
    assert!(!server.contains("permission_prompt"));
    let details: ports::bridge::BridgeFile =
        serde_json::from_slice(&std::fs::read(file.path()).unwrap()).unwrap();
    assert_eq!(
        (details.token.as_str(), details.socket.as_str()),
        ("run-secret", "/run/ophio/bridge")
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(file.path()).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    // Planning doesn't browse. A run not asked to browse still shows the
    // person things, and only that.
    spec.mode = TaskMode::Plan;
    assert!(bridge_server(&spec).unwrap().is_none());
    spec.mode = TaskMode::Work;
    spec.bridge.as_mut().unwrap().browser = false;
    let (_file, server) = bridge_server(&spec).unwrap().unwrap();
    let tools: Vec<String> = (ports::bridge::PERSON_TOOLS.iter())
        .chain(ports::bridge::WORK_TOOLS)
        .map(|t| format!("\"{t}\""))
        .collect();
    assert!(server.contains(&format!("enabled_tools=[{}]", tools.join(","))));
    // Computer use gets the computer tools and nothing else.
    spec.bridge.as_mut().unwrap().computer_use = true;
    let (_file, server) = bridge_server(&spec).unwrap().unwrap();
    let tools: Vec<String> = (ports::bridge::COMPUTER_TOOLS.iter())
        .map(|t| format!("\"{t}\""))
        .collect();
    assert!(server.contains(&format!("enabled_tools=[{}]", tools.join(","))));
    spec.bridge.as_mut().unwrap().computer_use = false;
    spec.bridge.as_mut().unwrap().browser = true;
    spec.bridge.as_mut().unwrap().server_name = "a.b".into();
    assert!(bridge_server(&spec).is_err());
}

#[test]
fn a_run_linked_to_github_gets_the_github_tools_and_is_told_how_to_use_them() {
    let (_dir, _adapter, mut spec) = fixture();
    spec.mode = TaskMode::Work;
    spec.bridge = Some(ports::ToolBridge {
        server_name: "computer".into(),
        command: "/opt/ophio/computer".into(),
        args: vec![],
        env: vec![
            (ports::bridge::RUN_TOKEN_ENV.into(), "run-secret".into()),
            (ports::bridge::SOCKET_ENV.into(), "/run/ophio/bridge".into()),
        ],
        permission_tool: Some("permission_prompt".into()),
        computer_use: false,
        browser: false,
        github: true,
        checklist_answers: false,
    });
    spec.github = Some("## GitHub\nThis task is linked to GitHub.".into());
    let (_file, server) = bridge_server(&spec).unwrap().unwrap();
    for tool in ports::bridge::GITHUB_TOOLS
        .iter()
        .chain(ports::bridge::ASK_TOOLS)
    {
        assert!(server.contains(&format!("\"{tool}\"")), "{tool}");
    }
    assert!(!server.contains("github_start_issue"));
    assert!(!server.contains("browser_open"));
    assert!(!server.contains("request_human"));
    let args = arguments(&spec, Some(&server), false, None, &[]).unwrap();
    assert!(!args.iter().any(|a| a.contains("run-secret")));
    assert!(
        crate::task_instructions(&spec)
            .unwrap()
            .unwrap()
            .contains("This task is linked to GitHub.")
    );
    // Both: one server, with both sets of tools.
    spec.bridge.as_mut().unwrap().browser = true;
    let (_file, server) = bridge_server(&spec).unwrap().unwrap();
    assert!(server.contains("\"github_merge\""));
    assert!(server.contains("\"request_human\""));
    // A plan doesn't post.
    spec.mode = TaskMode::Plan;
    assert!(bridge_server(&spec).unwrap().is_none());
}

#[test]
fn toml_strings_escape_what_toml_requires() {
    assert_eq!(toml_string("a\"b\\c"), r#""a\"b\\c""#);
    assert_eq!(toml_string("x\ny\u{7f}"), r#""x\u000Ay\u007F""#);
    assert_eq!(toml_string("día"), "\"día\"");
}

#[tokio::test]
async fn a_read_only_run_switches_the_persons_own_servers_off_or_does_not_start() {
    let (dir, adapter, mut spec) = fixture();
    spec.mode = TaskMode::Ask;
    spec.bridge = None;
    spec.prompt.text = "complete".into();
    std::fs::write(dir.path().join("mcp-servers.json"), r#"[{"name":"mine"}]"#).unwrap();
    let mut run = adapter.start_run(spec.clone()).await.unwrap();
    while !matches!(event(&mut run.events).await, HarnessEvent::Exited { .. }) {}
    let args: Vec<String> =
        serde_json::from_str(&std::fs::read_to_string(dir.path().join("argv.json")).unwrap())
            .unwrap();
    assert!(
        args.iter()
            .any(|a| a == r#"mcp_servers={"mine"={enabled=false}}"#)
    );
    std::fs::remove_file(dir.path().join("argv.json")).unwrap();
    std::fs::write(dir.path().join("mcp-list-fails"), "").unwrap();
    assert!(matches!(
        adapter.start_run(spec).await,
        Err(HarnessError::Unsupported(reason)) if reason.contains("codex mcp list")
    ));
    assert!(!dir.path().join("argv.json").exists());
}

#[test]
fn a_turn_answering_on_the_checklist_gets_its_read_and_reply_tools_alone() {
    let (_dir, _adapter, mut spec) = fixture();
    spec.mode = TaskMode::Ask;
    spec.bridge = Some(ports::ToolBridge {
        server_name: "computer".into(),
        command: "/opt/ophio/computer".into(),
        args: vec![],
        env: vec![
            (ports::bridge::RUN_TOKEN_ENV.into(), "run-secret".into()),
            (ports::bridge::SOCKET_ENV.into(), "/run/ophio/bridge".into()),
        ],
        permission_tool: Some("permission_prompt".into()),
        computer_use: false,
        browser: false,
        github: false,
        checklist_answers: true,
    });
    for mode in [TaskMode::Ask, TaskMode::Work] {
        spec.mode = mode;
        let (_file, server) = bridge_server(&spec).unwrap().unwrap();
        assert!(
            server.contains(r#"enabled_tools=["checklist_read","checklist_reply"]"#),
            "{server}"
        );
        let args = arguments(&spec, Some(&server), false, None, &[]).unwrap();
        assert!(!args.iter().any(|a| a.contains("run-secret")));
    }
    // Any other Ask turn still has none.
    spec.mode = TaskMode::Ask;
    spec.bridge.as_mut().unwrap().checklist_answers = false;
    assert!(bridge_server(&spec).unwrap().is_none());
}
