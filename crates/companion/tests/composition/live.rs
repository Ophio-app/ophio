// SPDX-License-Identifier: Apache-2.0
//! Owner-run qualification against the installed, signed-in Claude Code and
//! Codex. Ignored by default and inert unless `OPHIO_LIVE=1`; it uses the
//! owner's subscription quota and never an API key.
//!
//! `OPHIO_LIVE=1 cargo test -p companion --test composition live_ -- --ignored --nocapture --test-threads=1`
use super::*;

const STEP: Duration = Duration::from_secs(240);

/// Stands in for a long build that an agent runs in the foreground: it prints
/// its progress for about a minute and a half, then leaves a marker.
const LONG_STEP: &str = "#!/bin/sh
i=0
while [ $i -lt 90 ]; do
  i=$((i + 1))
  echo \"step $i of 90\"
  sleep 1
done
touch stop-marker
";

/// The long step's processes, found from the computer's side: an agent's
/// sandbox may give them process ids of its own.
fn long_step() -> Vec<u32> {
    std::fs::read_dir("/proc")
        .unwrap()
        .filter_map(|entry| entry.ok()?.file_name().to_str()?.parse().ok())
        .filter(|pid| {
            std::fs::read(format!("/proc/{pid}/cmdline"))
                .is_ok_and(|line| line.windows(12).any(|w| w == b"long-step.sh"))
                && test_support::detached::running(*pid)
        })
        .collect()
}

fn enabled() -> bool {
    std::env::var("OPHIO_LIVE").as_deref() == Ok("1")
}

fn on_path(name: &str, override_var: &str) -> PathBuf {
    if let Ok(path) = std::env::var(override_var) {
        return PathBuf::from(path);
    }
    std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|dir| dir.join(name))
        .find(|path| path.is_file())
        .unwrap_or_else(|| panic!("{name} is not on PATH"))
}

/// The fixture rig with the installed binaries, the real clock and the
/// owner's HOME for instruction discovery. The desktop stays synthetic.
async fn live_rig() -> Rig {
    let temp = tempfile::tempdir().unwrap();
    let paths = Paths {
        config: temp.path().join("config"),
        data: temp.path().join("data"),
        runtime: temp.path().join("run"),
    };
    paths.initialize().unwrap();
    let lock = paths.lock().unwrap();
    let config = Config::default();
    config.save(&paths.config_file()).unwrap();
    let identity = Arc::new(
        Identity::load_or_create(
            &FileSecretStore::new(paths.identity()),
            vec!["localhost".into()],
        )
        .unwrap(),
    );
    let executable_dir = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_owned();
    assert!(
        executable_dir.join(computer_mcp::BINARY_NAME).exists(),
        "Build computer-mcp before this test: cargo build -p computer-mcp"
    );
    let options = ComposeOptions {
        environment: [("HOME".into(), std::env::var("HOME").unwrap())]
            .into_iter()
            .collect(),
        executable_dir,
        clock: Arc::new(ports::SystemClock),
        claude_binary: Some(on_path("claude", "OPHIO_LIVE_CLAUDE")),
        codex_binary: Some(on_path("codex", "OPHIO_LIVE_CODEX")),
        desktop_factory: Some(Arc::new(SyntheticDesktop)),
        speech_factory: Arc::new(companion::frontdoors::LocalSpeech),
        accounts: false,
    };
    let runtime = Runtime::build(paths.clone(), config, &identity, options.clone())
        .await
        .unwrap();
    let mut deps = ServerDeps::new(runtime.hub.clone(), identity.clone()).unwrap();
    deps.clock = options.clock.clone();
    deps.owner = Some(runtime.clone());
    let server = Server::bind(deps.clone(), &["127.0.0.1:0".parse().unwrap()])
        .await
        .unwrap();
    let address = server.local_addresses().unwrap()[0];
    let local = LocalServer::bind(
        &paths.socket(),
        companion::started::Started::claim().unwrap(),
    )
    .await
    .unwrap();
    let stop = CancellationToken::new();
    let tasks = vec![
        tokio::spawn(server.run(stop.clone())),
        tokio::spawn(local.run(deps, stop.clone())),
    ];
    Rig {
        temp,
        paths,
        runtime,
        identity,
        options,
        address,
        stop,
        tasks,
        lock,
        key: ed25519_dalek::SigningKey::from_bytes(&[7; 32]),
        device: None,
    }
}

#[derive(Debug)]
struct Outcome {
    state: RunState,
    approvals: Vec<Approval>,
}

/// Follows one run of `task` that is not in `settled` to a terminal state,
/// answering its approvals with `decide`. Stop is sent once `stop_when` holds.
async fn drive(
    phone: &mut Phone,
    task: TaskId,
    settled: &mut Vec<RunId>,
    decide: impl Fn(&Approval) -> ApprovalDecision,
    mut stop_when: Option<&dyn Fn() -> bool>,
) -> Outcome {
    let mut approvals: Vec<Approval> = vec![];
    let started = tokio::time::Instant::now();
    // Real time passes here, so the phone keeps its session alive as the app does.
    let mut pinged = started;
    let mut nonce = 0;
    loop {
        if pinged.elapsed() >= Duration::from_secs(10) {
            pinged = tokio::time::Instant::now();
            nonce += 1;
            phone.client.send(&ClientFrame::Ping { nonce }).await;
        }
        assert!(
            started.elapsed() < STEP,
            "no terminal run within {STEP:?}; approvals so far: {approvals:?}"
        );
        if stop_when.is_some_and(|holds| holds()) {
            stop_when = None;
            println!("  sending Stop");
            phone
                .command(
                    Command::StopTask {
                        task_id: task,
                        force: false,
                    },
                    None,
                )
                .await;
        }
        let frame = tokio::time::timeout(Duration::from_secs(2), phone.frame()).await;
        let Ok(ServerFrame::Event(envelope)) = frame else {
            continue;
        };
        match envelope.event {
            Event::ApprovalUpserted { approval }
                if approval.task_id == task
                    && approval.state == ApprovalState::Pending
                    && !approvals
                        .iter()
                        .any(|a| a.approval_id == approval.approval_id) =>
            {
                let decision = decide(&approval);
                println!(
                    "  approval {:?}: {} | scope: {} | answering {:?}",
                    approval.kind, approval.title, approval.scope, decision
                );
                phone
                    .command(
                        Command::ResolveApproval {
                            approval_id: approval.approval_id,
                            decision,
                        },
                        Some(u64::from(approval.revision)),
                    )
                    .await;
                approvals.push(approval);
            }
            Event::RunUpserted { run }
                if run.task_id == task
                    && !settled.contains(&run.run_id)
                    && matches!(
                        run.state,
                        RunState::Completed
                            | RunState::Failed
                            | RunState::Interrupted
                            | RunState::ReconciliationRequired
                    ) =>
            {
                settled.push(run.run_id);
                println!(
                    "  run ended {:?} after {:.1}s (native session {})",
                    run.state,
                    started.elapsed().as_secs_f32(),
                    if run.native_session.is_some() {
                        "reported"
                    } else {
                        "missing"
                    }
                );
                return Outcome {
                    state: run.state,
                    approvals,
                };
            }
            _ => {}
        }
    }
}

async fn agent_text(phone: &mut Phone, task: TaskId) -> Vec<String> {
    let QueryData::TaskTimeline { items, .. } = phone
        .query(Query::TaskTimeline {
            task_id: task,
            before_seq: None,
            limit: 200,
        })
        .await
    else {
        panic!()
    };
    items
        .into_iter()
        .filter_map(|i| match i.item {
            TimelineBody::AgentMessage {
                text,
                partial: false,
            } => Some(text),
            TimelineBody::Note { text } => Some(format!("[note] {text}")),
            _ => None,
        })
        .collect()
}

async fn qualify(harness: HarnessKind) {
    if !enabled() {
        eprintln!("skipped: set OPHIO_LIVE=1 to use the signed-in {harness:?}");
        return;
    }
    let mut rig = live_rig().await;
    rig.pair().await;
    let mut phone = Phone::new(&rig).await;
    let readiness = phone.query(Query::Readiness).await;
    println!(
        "{harness:?} readiness: {}",
        serde_json::to_string(&readiness).unwrap()
    );
    let mut settled = vec![];
    let allow = |_: &Approval| ApprovalDecision::Allow;

    // 1. A write that should ask first, allowed from the phone.
    let dir = rig.project_dir("write");
    let project = phone.project(&dir).await;
    let task = phone
        .task(
            project,
            harness,
            "Create a file named live-check.txt in the current directory whose entire content is the two letters ok. Then reply with the single word done.",
        )
        .await;
    println!("{harness:?} write task:");
    let outcome = drive(&mut phone, task, &mut settled, allow, None).await;
    let written = std::fs::read_to_string(dir.join("live-check.txt")).ok();
    println!(
        "  approvals asked: {}; file: {written:?}; agent: {:?}",
        outcome.approvals.len(),
        agent_text(&mut phone, task).await
    );
    assert_eq!(outcome.state, RunState::Completed);
    assert_eq!(written.as_deref().map(str::trim), Some("ok"));

    // 2. The same task continues in the native session.
    let QueryData::Snapshot(snapshot) = phone
        .query(Query::Snapshot {
            reads_withheld: None,
        })
        .await
    else {
        panic!()
    };
    let revision = snapshot
        .tasks
        .iter()
        .find(|t| t.task_id == task)
        .unwrap()
        .revision;
    phone
        .command(
            Command::ResumeTask {
                mode: None,
                task_id: task,
                text: Some(
                    "What is the exact content of live-check.txt? Reply with only that content."
                        .into(),
                ),
                bundle: None,
                carried_plan: None,
                read_only: false,
                model: None,
                effort: None,
            },
            Some(revision),
        )
        .await;
    println!("{harness:?} continue:");
    let outcome = drive(&mut phone, task, &mut settled, allow, None).await;
    let text = agent_text(&mut phone, task).await;
    println!("  agent: {text:?}");
    assert_eq!(outcome.state, RunState::Completed);
    // A note, such as a usage warning, can follow the reply.
    let reply = text.iter().rev().find(|t| !t.starts_with("[note] "));
    assert!(
        reply.is_some_and(|t| t.to_lowercase().contains("ok")),
        "{text:?}"
    );

    // 3. A denied command is not run.
    let dir = rig.project_dir("deny");
    let project = phone.project(&dir).await;
    // Codex runs commands inside the project without asking; one that asks
    // for escalated permissions asks first.
    let ask = match harness {
        HarnessKind::Codex => " with escalated permissions, so that I'm asked first",
        _ => "",
    };
    let task = phone
        .task(
            project,
            harness,
            &format!("Use the shell to run exactly this command{ask}: touch denied-marker. If you are not allowed, reply with the single word refused and do nothing else."),
        )
        .await;
    println!("{harness:?} denied command:");
    let outcome = drive(
        &mut phone,
        task,
        &mut settled,
        |_| ApprovalDecision::Deny {
            reason: Some("Live qualification denial".into()),
        },
        None,
    )
    .await;
    println!("  agent: {:?}", agent_text(&mut phone, task).await);
    assert!(!dir.join("denied-marker").exists(), "denied command ran");
    assert!(
        !outcome.approvals.is_empty() || outcome.state == RunState::Completed,
        "{outcome:?}"
    );

    // 4. Stop ends a long step the agent runs in the foreground, and the
    // step's own process with it.
    let dir = rig.project_dir("stop");
    test_support::executable(dir.join("long-step.sh"), LONG_STEP).unwrap();
    let project = phone.project(&dir).await;
    let task = phone
        .task(
            project,
            harness,
            "Run ./long-step.sh in the foreground and wait for it to finish; it prints its progress for about a minute and a half. Don't move it to the background. Then reply done.",
        )
        .await;
    println!("{harness:?} stop:");
    let running = || !long_step().is_empty();
    let outcome = drive(&mut phone, task, &mut settled, allow, Some(&running)).await;
    println!("  agent: {:?}", agent_text(&mut phone, task).await);
    let Event::InterventionRecorded { receipt, .. } = phone
        .event(
            |e| matches!(e, Event::InterventionRecorded { task_id: Some(id), .. } if *id == task),
        )
        .await
    else {
        unreachable!()
    };
    println!("  receipt: {}", serde_json::to_string(&receipt).unwrap());
    assert_eq!(outcome.state, RunState::Interrupted, "{outcome:?}");
    let left = long_step();
    assert!(
        left.is_empty(),
        "the long step's processes {left:?} outlived Stop"
    );
    assert!(
        !dir.join("stop-marker").exists(),
        "stopped command finished"
    );
    assert!(
        receipt
            .facts
            .iter()
            .any(|f| f.label == "Everything the agent started" && f.status == FactStatus::Done),
        "{receipt:?}"
    );

    drop(phone);
    rig.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "live: uses the owner's signed-in Claude Code"]
async fn live_claude_code() {
    qualify(HarnessKind::ClaudeCode).await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "live: uses the owner's signed-in Codex"]
async fn live_codex() {
    qualify(HarnessKind::Codex).await;
}
