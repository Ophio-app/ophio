// SPDX-License-Identifier: Apache-2.0
//! The review agent in `tools/review`, run through the real Claude harness and
//! permission bridge on a copy of its demo project.
#![cfg(target_os = "linux")]
use async_trait::async_trait;
use ports::{
    HarnessAdapter, HarnessEvent, PromptInput, RunExit, RunSpec, ToolBridge,
    bridge::*,
    tool_approval::{permission_reply, tool_approval},
};
use protocol::{
    RunId, TaskId,
    command::{ApprovalDecision, QuestionAnswer},
    model::{ApprovalKind, CheckStatus, FileChange, RunOutcome, TaskMode},
};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::oneshot;

const TOKEN: &str = "review-binding";
const BUG: &str = "(1 + Decimal(str(tip_percent)))";
const FIXED: &str = "(1 + Decimal(str(tip_percent)) / 100)";

struct Broker {
    binding: BridgeBinding,
    deny: Option<&'static str>,
    asked: Mutex<Vec<String>>,
    /// The kinds of approval card the hub records from these requests.
    cards: Mutex<Vec<ApprovalKind>>,
}
#[async_trait]
impl BridgeHandler for Broker {
    async fn bind(&self, token: &str) -> Option<BridgeBinding> {
        (token == TOKEN).then(|| self.binding.clone())
    }
    async fn handle(&self, _: &BridgeBinding, request: BridgeRequest) -> BridgeResponse {
        let BridgeRequest::PermissionPrompt {
            tool_name, input, ..
        } = request
        else {
            return BridgeResponse::Refused {
                reason: "Only permission prompts are offered".into(),
            };
        };
        let approval = tool_approval(&tool_name, &input);
        self.cards.lock().unwrap().push(approval.kind);
        let questions = approval.questions;
        // The person answers each question with its last option.
        let decision = match self.deny {
            Some(reason) => ApprovalDecision::Deny {
                reason: Some(reason.into()),
            },
            None if questions.is_empty() => ApprovalDecision::Allow,
            None => ApprovalDecision::Answer {
                text: String::new(),
                answers: questions
                    .into_iter()
                    .map(|question| QuestionAnswer {
                        id: None,
                        choices: vec![question.options.last().unwrap().label.clone()],
                        question: question.question,
                    })
                    .collect(),
            },
        };
        let decision = permission_reply(&tool_name, input, decision);
        self.asked.lock().unwrap().push(tool_name);
        BridgeResponse::Permission { decision }
    }
    async fn unbound(&self, _: &BridgeBinding) {}
}

struct Review {
    dir: tempfile::TempDir,
    project: PathBuf,
    binary: PathBuf,
}

/// A copy of the demo project and a `claude` that runs the review agent at `pace`.
fn review(pace: &str) -> Review {
    let dir = test_support::private_tempdir().unwrap();
    let tools = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/review");
    let project = dir.path().join("project");
    copy(&tools.join("demo"), &project);
    let binary = dir.path().join("claude");
    let agent = tools.join("review_claude.py").canonicalize().unwrap();
    test_support::executable(
        &binary,
        format!(
            "#!/bin/sh\nREVIEW_AGENT_PACE={pace} exec '{}' \"$@\"\n",
            agent.display()
        ),
    )
    .unwrap();
    Review {
        dir,
        project,
        binary,
    }
}

fn copy(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        if entry.file_name() == "__pycache__" {
            continue;
        }
        if entry.file_type().unwrap().is_dir() {
            copy(&entry.path(), &to.join(entry.file_name()));
        } else {
            fs::copy(entry.path(), to.join(entry.file_name())).unwrap();
        }
    }
}

struct Started {
    run: ports::RunStarted,
    broker: Arc<Broker>,
    stop: oneshot::Sender<()>,
    server: tokio::task::JoinHandle<Result<(), computer_mcp::BridgeError>>,
}

async fn start(
    review: &Review,
    mode: TaskMode,
    prompt: &str,
    deny: Option<&'static str>,
) -> Started {
    let adapter = harness_claude::ClaudeAdapter::with_binary(review.binary.clone());
    let broker = Arc::new(Broker {
        binding: BridgeBinding {
            run_id: RunId::new(),
            target_id: None,
            computer_use: false,
            browser: false,
            github: false,
            checklist_answers: false,
        },
        deny,
        asked: Mutex::new(Vec::new()),
        cards: Mutex::new(Vec::new()),
    });
    let socket = review.dir.path().join("bridge.sock");
    let handler = broker.clone();
    let (stop, ended) = oneshot::channel();
    let path = socket.clone();
    let server = tokio::spawn(async move {
        computer_mcp::serve::serve_bridge(path, handler, async {
            let _ = ended.await;
        })
        .await
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        while !socket.exists() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let bridge = (mode == TaskMode::Work).then(|| ToolBridge {
        server_name: "computer".into(),
        command: env!("CARGO_BIN_EXE_computer-mcp").into(),
        args: Vec::new(),
        env: vec![
            (RUN_TOKEN_ENV.into(), TOKEN.into()),
            (SOCKET_ENV.into(), socket.to_str().unwrap().into()),
        ],
        permission_tool: Some("permission_prompt".into()),
        computer_use: false,
        browser: false,
        github: false,
        checklist_answers: false,
    });
    let spec = RunSpec {
        run_id: broker.binding.run_id,
        task_id: TaskId::new(),
        cwd: review.project.clone(),
        mode,
        prompt: PromptInput {
            text: prompt.into(),
            images: Vec::new(),
        },
        model: None,
        effort: None,
        launch_policy: None,
        resume_session: None,
        resume_from_mode: None,
        mission: None,
        branch: None,
        start: None,
        bridge,
        project_settings: false,
        env: Default::default(),
        github: None,
        scratch: None,
        questions: true,
        sandbox: None,
        web: false,
        disabled: Default::default(),
    };
    let run = adapter.start_run(spec).await.unwrap();
    Started {
        run,
        broker,
        stop,
        server,
    }
}

struct Finished {
    events: Vec<HarnessEvent>,
    exit: RunExit,
    outcome: RunOutcome,
    asked: Vec<String>,
    approvals: Vec<ApprovalKind>,
}

async fn finish(mut started: Started, mut events: Vec<HarnessEvent>) -> Finished {
    let (exit, outcome) = loop {
        let event = tokio::time::timeout(Duration::from_secs(30), started.run.events.recv())
            .await
            .expect("the review agent went quiet")
            .expect("the run ended without an exit");
        if let HarnessEvent::Exited { exit, .. } = &event {
            let outcome = events.iter().rev().find_map(|event| match event {
                HarnessEvent::Results { outcome } => Some(outcome.clone()),
                _ => None,
            });
            break (exit.clone(), outcome.expect("the turn reported no results"));
        }
        events.push(event);
    };
    let _ = started.stop.send(());
    started.server.await.unwrap().unwrap();
    // The bridge alone records Claude Code's approvals; the stream raises none.
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, HarnessEvent::ApprovalRequested(_))),
        "{events:?}"
    );
    let asked = started.broker.asked.lock().unwrap().clone();
    let approvals = started.broker.cards.lock().unwrap().clone();
    Finished {
        events,
        exit,
        outcome,
        asked,
        approvals,
    }
}

fn replies(events: &[HarnessEvent]) -> String {
    events
        .iter()
        .filter_map(|event| match event {
            HarnessEvent::AgentMessage { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test]
async fn allowed_fix_edits_the_project_and_reports_the_failing_then_passing_check() {
    let review = review("0");
    let started = start(&review, TaskMode::Work, "Fix the failing test", None).await;
    let done = finish(started, Vec::new()).await;

    assert_eq!(done.exit, RunExit::Completed);
    assert_eq!(done.asked, ["Bash", "Edit", "Bash"]);
    assert_eq!(
        done.approvals,
        [
            ApprovalKind::Command,
            ApprovalKind::FileEdit,
            ApprovalKind::Command
        ]
    );
    let tips = fs::read_to_string(review.project.join("tips.py")).unwrap();
    assert!(tips.contains(FIXED), "{tips}");
    let [file] = done.outcome.changed_files.as_slice() else {
        panic!("{:?}", done.outcome.changed_files);
    };
    assert!(file.path.ends_with("tips.py"), "{}", file.path);
    assert_eq!(file.change, FileChange::Modified);
    assert_eq!((file.added, file.removed), (Some(1), Some(1)));
    let checks: Vec<_> = done
        .outcome
        .checks
        .iter()
        .map(|check| (check.command.as_deref(), check.status, check.exit_code))
        .collect();
    assert_eq!(
        checks,
        [
            (Some("python3 -m unittest"), CheckStatus::Failed, Some(1)),
            (Some("python3 -m unittest"), CheckStatus::Passed, Some(0)),
        ]
    );
    assert!(
        done.outcome
            .summary
            .as_deref()
            .is_some_and(|s| s.starts_with("Fixed the tip calculation")),
        "{:?}",
        done.outcome.summary
    );
}

#[tokio::test]
async fn denied_command_carries_the_reason_and_changes_nothing() {
    let review = review("0");
    let started = start(
        &review,
        TaskMode::Work,
        "Fix the failing test",
        Some("Not on the shared runner"),
    )
    .await;
    let done = finish(started, Vec::new()).await;

    assert_eq!(done.exit, RunExit::Completed);
    assert_eq!(done.asked, ["Bash"]);
    assert!(
        replies(&done.events).contains("You declined running the tests (Not on the shared runner)"),
        "{}",
        replies(&done.events)
    );
    assert!(done.outcome.changed_files.is_empty());
    let [check] = &done.outcome.checks[..] else {
        panic!("{:?}", done.outcome.checks)
    };
    assert_eq!(check.status, CheckStatus::NotRun);
    assert_eq!(check.exit_code, None);
    assert_eq!(
        check.detail.as_deref(),
        Some("Denied before it ran: Not on the shared runner")
    );
    let tips = fs::read_to_string(review.project.join("tips.py")).unwrap();
    assert!(tips.contains(BUG), "{tips}");
}

#[tokio::test]
async fn answered_question_reaches_the_agent_as_the_chosen_option() {
    let review = review("0");
    let started = start(
        &review,
        TaskMode::Work,
        "Ask me how to round each share",
        None,
    )
    .await;
    let done = finish(started, Vec::new()).await;

    assert_eq!(done.exit, RunExit::Completed);
    assert_eq!(done.asked, ["AskUserQuestion"]);
    assert_eq!(done.approvals, [ApprovalKind::Question]);
    assert!(
        replies(&done.events).contains("Thanks. You chose: Always up."),
        "{}",
        replies(&done.events)
    );
    assert!(done.outcome.changed_files.is_empty());
    let tips = fs::read_to_string(review.project.join("tips.py")).unwrap();
    assert!(tips.contains(BUG), "{tips}");
}

#[tokio::test]
async fn plan_reads_the_project_and_proposes_steps_without_changing_it() {
    let review = review("0");
    let started = start(
        &review,
        TaskMode::Plan,
        "How would you fix the failing test?",
        None,
    )
    .await;
    let done = finish(started, Vec::new()).await;

    assert_eq!(done.exit, RunExit::Completed);
    assert!(done.asked.is_empty());
    let plan = done
        .events
        .iter()
        .find_map(|event| match event {
            HarnessEvent::Activity {
                summary, detail, ..
            } if summary == "Plan proposed" => detail.clone(),
            _ => None,
        })
        .expect("no plan was proposed");
    assert!(
        plan.starts_with("## Fix the tip calculation\n\n1. "),
        "{plan}"
    );
    assert!(plan.contains("divide `tip_percent` by 100"), "{plan}");
    let tools: Vec<_> = done
        .events
        .iter()
        .filter_map(|event| match event {
            HarnessEvent::Activity { tool, .. } => Some(tool.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        tools
            .iter()
            .all(|tool| ["Glob", "Read", "Grep", "ExitPlanMode"].contains(tool)),
        "{tools:?}"
    );
    assert!(done.outcome.changed_files.is_empty());
    let tips = fs::read_to_string(review.project.join("tips.py")).unwrap();
    assert!(tips.contains(BUG), "{tips}");
}

#[tokio::test]
async fn stop_mid_turn_ends_the_turn_as_interrupted() {
    let review = review("1");
    let mut started = start(
        &review,
        TaskMode::Work,
        "Take your time and read every file",
        None,
    )
    .await;
    let mut events = Vec::new();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(30), started.run.events.recv())
            .await
            .unwrap()
            .unwrap();
        let reading = matches!(&event, HarnessEvent::Activity { tool, .. } if tool == "Read");
        events.push(event);
        if reading {
            break;
        }
    }
    tokio::time::timeout(Duration::from_secs(5), started.run.control.interrupt())
        .await
        .unwrap()
        .unwrap();
    let done = finish(started, events).await;

    assert_eq!(done.exit, RunExit::Interrupted);
    assert_eq!(
        done.outcome.summary.as_deref(),
        Some("Stopped at your request.")
    );
    assert!(done.outcome.changed_files.is_empty());
    let reads = done
        .events
        .iter()
        .filter(|event| matches!(event, HarnessEvent::Activity { tool, .. } if tool == "Read"))
        .count();
    assert!(
        reads < 3,
        "the agent kept reading after Stop: {reads} reads"
    );
}
