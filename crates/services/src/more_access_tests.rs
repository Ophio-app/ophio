// SPDX-License-Identifier: Apache-2.0
//! An agent asks for more with one card; a read-only task carries on in
//! Work in one tap; declining means it stops asking. These are the gates on
//! what an agent may do: a Plan or Ask run never has a change allowed without
//! the task really switching to Work, and a chat's Allow turns on exactly
//! what was asked for.
use super::conversation_access::{Backend, dispatched};
use super::*;
use ports::bridge::{BridgeBinding, BridgeHandler, BridgeRequest, BridgeResponse, PersonCall};
use protocol::access::ConversationAccess;

fn all_modes(f: &Fixture) {
    f.hub
        .write(|tx| {
            let mut infos: Vec<protocol::capability::HarnessInfo> =
                tx.read().require("meta", "harnesses")?;
            for mode in [TaskMode::Work, TaskMode::Plan, TaskMode::Ask] {
                infos[0].modes.push(protocol::capability::ModeSupport {
                    mode,
                    availability: protocol::capability::Availability::Available,
                    enforcement: "fixture".into(),
                });
            }
            tx.put("meta", "harnesses", &infos)
        })
        .unwrap();
}

fn create(
    project: ProjectId,
    mode: TaskMode,
    access: Option<ConversationAccess>,
) -> CommandEnvelope {
    CommandEnvelope::new(Command::CreateTask {
        github: None,
        agent_chosen: None,
        project_id: project,
        kind: TaskKind::Coding,
        mode: Some(mode),
        title: Some(format!("{mode:?} task")),
        prompt: "Fix the build".into(),
        bundle: None,
        harness: HarnessKind::ClaudeCode,
        model: None,
        effort: None,
        launch_policy: None,
        branch: None,
        mission: None,
        target_id: None,
        placement: TaskPlacement::StartNow,
        start: None,
        place: None,
        browser: None,
        conversation: access.is_some().then_some(true),
        access,
        review: None,
    })
}

async fn started(f: &Fixture, envelope: CommandEnvelope) -> (TaskId, ports::RunSpec) {
    let receipt = f.hub.submit(&Caller::local_owner(), envelope).await;
    let Some(CommandResult::TaskCreated { task_id, .. }) = receipt.result else {
        panic!("{receipt:?}")
    };
    let spec = dispatched(f, task_id).await;
    run_started(f, task_id).await;
    f.harness
        .emit(
            spec.run_id,
            ports::HarnessEvent::SessionStarted {
                native_session: "the-session".into(),
                actual_model: None,
                actual_effort: None,
                account_label: None,
            },
        )
        .await;
    (task_id, spec)
}

async fn bound(f: &Fixture, spec: &ports::RunSpec) -> BridgeBinding {
    let bridge = spec.bridge.clone().expect("a bridge");
    let token = (bridge.env.iter())
        .find(|(key, _)| key == ports::bridge::RUN_TOKEN_ENV)
        .map(|(_, token)| token.clone())
        .unwrap();
    BridgeHandler::bind(&f.hub, &token)
        .await
        .expect("bridge binding")
}

async fn approval_where(f: &Fixture, found: impl Fn(&Approval) -> bool) -> Approval {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let approvals: Vec<Approval> = f.hub.read(|r| r.list("approval")).unwrap();
            if let Some(a) = approvals.into_iter().find(|a| found(a)) {
                return a;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .expect("no such approval")
}

/// Raises a command approval from `run`, as Claude Code or Codex would.
async fn raised(f: &Fixture, run: RunId, raw: &str) -> Approval {
    let native = uuid::Uuid::new_v4().to_string();
    f.harness
        .emit(
            run,
            ports::HarnessEvent::ApprovalRequested(ports::NativeApproval {
                native_request_id: native.clone(),
                kind: ApprovalKind::Command,
                title: "Run the build".into(),
                headline: None,
                subject: Default::default(),
                detail: None,
                detail_lines: Vec::new(),
                questions: Vec::new(),
                raw_request: Some(raw.into()),
                scope: "once".into(),
                shortened: false,
                tool_call: None,
            }),
        )
        .await;
    approval_where(f, |a| a.native_request_id == native).await
}

async fn decide(f: &Fixture, a: &Approval, decision: ApprovalDecision) -> CommandReceipt {
    let mut envelope = CommandEnvelope::new(Command::ResolveApproval {
        approval_id: a.approval_id,
        decision,
    });
    envelope.expected_revision = Some(a.revision.into());
    f.hub.submit(&Caller::local_owner(), envelope).await
}

/// Submits `command`, ending `run` as its agent would once Ophio stops it.
async fn switching(f: &Fixture, run: RunId, command: Command) -> CommandReceipt {
    let before = f.harness.controls.interrupts.load(Ordering::SeqCst);
    let ends = async {
        tokio::time::timeout(Duration::from_secs(15), async {
            while f.harness.controls.interrupts.load(Ordering::SeqCst) == before {
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
        .await
        .expect("the turn was never stopped");
        f.harness
            .emit(
                run,
                ports::HarnessEvent::Exited {
                    exit: ports::RunExit::Interrupted,
                    outcome: None,
                },
            )
            .await;
    };
    let owner = Caller::local_owner();
    let (receipt, ()) = tokio::join!(f.hub.submit(&owner, CommandEnvelope::new(command)), ends);
    receipt
}

async fn next_spec(f: &Fixture, task: TaskId, after: RunId) -> ports::RunSpec {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let found = (f.harness.specs.lock().unwrap().iter())
                .find(|s| s.task_id == task && s.run_id != after)
                .cloned();
            if let Some(spec) = found {
                return spec;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("no next turn")
}

fn allowed_ever(f: &Fixture) -> bool {
    (f.harness.controls.decisions.lock().unwrap().iter())
        .any(|d| matches!(d, ApprovalDecision::Allow))
}

/// A Plan run's request to change something can't be allowed while it is
/// read-only. The card offers Switch to Work instead, which ends that turn,
/// settles the request as allowed and carries the same session on in Work,
/// where the agent is told to do it again. The read-only run itself is never
/// handed an Allow.
#[tokio::test]
async fn a_read_only_run_never_has_a_write_allowed_without_switching() {
    let f = Fixture::new().await;
    all_modes(&f);
    let project = f.project().await;
    let (task_id, spec) = started(&f, create(project, TaskMode::Plan, None)).await;
    assert_eq!(spec.mode, TaskMode::Plan);
    // It has Ophio's bridge, offering request_access alone.
    let bridge = spec.bridge.clone().expect("a read-only bridge");
    assert!(bridge.read_only && !bridge.browser && !bridge.github && !bridge.computer_use);
    assert_eq!(bridge.permission_tool, None);
    let a = raised(&f, spec.run_id, "make build").await;
    assert_eq!(
        a.actions.as_deref(),
        Some(&[ApprovalAction::SwitchToWork, ApprovalAction::Deny][..])
    );
    let refused = decide(&f, &a, ApprovalDecision::Allow).await;
    assert_eq!(refused.error.map(|e| e.code), Some(ErrorCode::Unsupported));
    assert!(!allowed_ever(&f));
    // An answer is no way round it either.
    let refused = decide(
        &f,
        &a,
        ApprovalDecision::Answer {
            text: "yes".into(),
            answers: vec![],
        },
    )
    .await;
    assert_eq!(refused.status, ReceiptStatus::Rejected);
    let receipt = switching(
        &f,
        spec.run_id,
        Command::SwitchToWork {
            task_id,
            approval_id: Some(a.approval_id),
            text: None,
        },
    )
    .await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    let next = next_spec(&f, task_id, spec.run_id).await;
    assert_eq!(next.mode, TaskMode::Work);
    assert_eq!(next.resume_session.as_deref(), Some("the-session"));
    assert_eq!(next.resume_from_mode, Some(TaskMode::Plan));
    assert!(
        next.prompt
            .text
            .contains("allowed what you asked for: Run the build"),
        "{}",
        next.prompt.text
    );
    assert!(!next.bridge.as_ref().is_some_and(|b| b.read_only));
    let (task, settled): (Task, Approval) = f
        .hub
        .read(|r| {
            Ok((
                r.require("task", task_id)?,
                r.require("approval", a.approval_id)?,
            ))
        })
        .unwrap();
    assert_eq!(task.mode, TaskMode::Work);
    assert_eq!(settled.state, ApprovalState::Allowed);
    // Nothing was ever handed an Allow: the read-only turn ended without it.
    assert!(!allowed_ever(&f));
    // In Work, the same request is allowed at once, as the person allowed it
    // by switching, and only once; another request still asks.
    run_started(&f, task_id).await;
    let again = raised(&f, next.run_id, "make build").await;
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let a: Approval = f
                .hub
                .read(|r| r.require("approval", again.approval_id))
                .unwrap();
            if a.state == ApprovalState::Allowed {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the switched request was not allowed again");
    assert_eq!(
        (f.harness.controls.decisions.lock().unwrap().iter())
            .filter(|d| matches!(d, ApprovalDecision::Allow))
            .count(),
        1
    );
    let other = raised(&f, next.run_id, "rm -rf build").await;
    let third = raised(&f, next.run_id, "make build").await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    for a in [other, third] {
        let a: Approval = f
            .hub
            .read(|r| r.require("approval", a.approval_id))
            .unwrap();
        assert_eq!(a.state, ApprovalState::Pending, "{}", a.title);
    }
    f.close().await;
}

/// Switching needs a task that can carry on in Work: never one already
/// there, and a request goes along only when Allow can't take it.
#[tokio::test]
async fn only_a_read_only_task_switches_and_only_with_what_needs_it() {
    let f = Fixture::new().await;
    all_modes(&f);
    let project = f.project().await;
    let (work, _) = started(&f, create(project, TaskMode::Work, None)).await;
    let receipt = f
        .hub
        .submit(
            &Caller::local_owner(),
            CommandEnvelope::new(Command::SwitchToWork {
                task_id: work,
                approval_id: None,
                text: None,
            }),
        )
        .await;
    assert_eq!(
        receipt.error.map(|e| e.code),
        Some(ErrorCode::InvalidRequest)
    );
    f.close().await;
}

/// A chat's agent asks for more with request_access; Allow turns on exactly
/// that, within the turn when the sandbox can widen, and the agent hears it
/// may carry on. Nothing else changes: no other project becomes writable.
#[tokio::test]
async fn a_chats_allow_turns_on_exactly_what_was_asked() {
    let backend = Arc::new(Backend::default());
    let f = Fixture::with(|b| b.sandboxes(backend.clone())).await;
    all_modes(&f);
    let project = f.project().await;
    let other = ProjectId::new();
    let before = ConversationAccess {
        read: vec![project],
        internet: false,
        ..Default::default()
    };
    let (task_id, spec) = started(&f, create(project, TaskMode::Ask, Some(before.clone()))).await;
    assert_eq!(spec.mode, TaskMode::Work);
    let binding = bound(&f, &spec).await;
    assert!(!binding.read_only);
    let hub = f.hub.clone();
    let asking = tokio::spawn(async move {
        hub.handle(
            &binding,
            BridgeRequest::RequestAccess {
                needs: vec![AccessNeed::LocalNetwork, AccessNeed::ChangeFiles],
                reason: "to build the Mac app on the MacBook".into(),
            },
        )
        .await
    });
    let a = approval_where(&f, |a| a.access_request.is_some()).await;
    assert_eq!(
        a.actions.as_deref(),
        Some(&[ApprovalAction::Allow, ApprovalAction::Deny][..])
    );
    let asked = a.access_request.clone().unwrap();
    // Its own network is reached only with the internet, which is off: the
    // card asks for both, and says so.
    assert_eq!(
        asked.needs,
        [
            AccessNeed::ChangeFiles,
            AccessNeed::Internet,
            AccessNeed::LocalNetwork
        ]
    );
    assert!(
        a.title
            .starts_with("Synthetic harness needs to change files in")
            || a.title.starts_with("Claude Code needs to change files in"),
        "{}",
        a.title
    );
    assert!(
        a.title.ends_with(
            "and reach the internet and your own network to build the Mac app on the MacBook."
        ),
        "{}",
        a.title
    );
    let receipt = decide(&f, &a, ApprovalDecision::Allow).await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    let said = asking.await.unwrap();
    let BridgeResponse::AccessResult { text } = said else {
        panic!("{said:?}")
    };
    assert!(text.starts_with("The person allowed it"), "{text}");
    let task: Task = f.hub.read(|r| r.require("task", task_id)).unwrap();
    let after = task.access.unwrap();
    assert_eq!(
        after,
        ConversationAccess {
            read: vec![project],
            write: vec![project],
            internet: true,
            local_network: true,
            whole_read: false,
            whole_write: false,
        }
        .normalized()
    );
    assert!(!after.write.contains(&other));
    // The running sandbox was widened before the agent heard it.
    let changes = backend.sandbox_changes();
    assert!(
        changes
            .last()
            .is_some_and(|r| r.internet && r.local_network && r.write.len() == 1),
        "{changes:?}"
    );
    f.close().await;
}

/// Declined, it is told to finish without it, and asking again in the same
/// turn is declined at once, with no second card.
#[tokio::test]
async fn declining_means_it_stops_asking() {
    let backend = Arc::new(Backend::default());
    let f = Fixture::with(|b| b.sandboxes(backend.clone())).await;
    all_modes(&f);
    let project = f.project().await;
    let access = ConversationAccess {
        read: vec![project],
        internet: false,
        ..Default::default()
    };
    let (task_id, spec) = started(&f, create(project, TaskMode::Ask, Some(access.clone()))).await;
    let binding = bound(&f, &spec).await;
    let ask = |binding: BridgeBinding, needs: Vec<AccessNeed>| {
        let hub = f.hub.clone();
        tokio::spawn(async move {
            hub.handle(
                &binding,
                BridgeRequest::RequestAccess {
                    needs,
                    reason: "to fetch the docs".into(),
                },
            )
            .await
        })
    };
    let first = ask(binding.clone(), vec![AccessNeed::Internet]);
    let a = approval_where(&f, |a| a.access_request.is_some()).await;
    let receipt = decide(
        &f,
        &a,
        ApprovalDecision::Deny {
            reason: Some("Use the copy in the repo".into()),
        },
    )
    .await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    let BridgeResponse::AccessResult { text } = first.await.unwrap() else {
        panic!()
    };
    assert!(
        text.contains("not now: Use the copy in the repo")
            && text.contains("Don't ask for this again in this turn"),
        "{text}"
    );
    // The card keeps the person's own words, not what the agent was told.
    let settled: Approval = f
        .hub
        .read(|r| r.require("approval", a.approval_id))
        .unwrap();
    assert_eq!(settled.state, ApprovalState::Denied);
    // Asking again, for the same or less, gets no new card.
    let BridgeResponse::AccessResult { text } = ask(binding.clone(), vec![AccessNeed::Internet])
        .await
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(text, crate::more_access::ASKED_AGAIN);
    let cards = (f
        .hub
        .read(|r| r.list::<Approval>("approval"))
        .unwrap()
        .into_iter())
    .filter(|a| a.access_request.is_some())
    .count();
    assert_eq!(cards, 1);
    let task: Task = f.hub.read(|r| r.require("task", task_id)).unwrap();
    assert_eq!(task.access, Some(access));
    // A native request the person declined is declined again at once too.
    let permission = |binding: BridgeBinding| {
        let hub = f.hub.clone();
        tokio::spawn(async move {
            hub.handle(
                &binding,
                BridgeRequest::PermissionPrompt {
                    tool_name: "Bash".into(),
                    input: serde_json::json!({"command": "curl https://example.com"}),
                    tool_use_id: None,
                    native_request_id: Some(uuid::Uuid::new_v4().to_string()),
                },
            )
            .await
        })
    };
    let first = permission(binding.clone());
    let a = approval_where(&f, |a| {
        a.kind == ApprovalKind::Command && a.state == ApprovalState::Pending
    })
    .await;
    let receipt = decide(&f, &a, ApprovalDecision::Deny { reason: None }).await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    let BridgeResponse::Permission { decision } = first.await.unwrap() else {
        panic!()
    };
    assert_eq!(decision["behavior"], "deny");
    let told = decision["message"].as_str().unwrap();
    assert!(
        told.contains("Don't ask for this again in this turn"),
        "{told}"
    );
    let BridgeResponse::Permission { decision } = permission(binding).await.unwrap() else {
        panic!()
    };
    assert_eq!(decision["message"], crate::more_access::ASKED_AGAIN);
    let commands = (f
        .hub
        .read(|r| r.list::<Approval>("approval"))
        .unwrap()
        .into_iter())
    .filter(|a| a.kind == ApprovalKind::Command)
    .count();
    assert_eq!(commands, 1);
    f.close().await;
}

/// A Plan task's agent asks to work with request_access, the one tool its
/// read-only bridge takes; the card's Switch to Work carries the same
/// session on in Work.
#[tokio::test]
async fn a_plan_task_asks_once_and_carries_on_in_work() {
    let f = Fixture::new().await;
    all_modes(&f);
    let project = f.project().await;
    let (task_id, spec) = started(&f, create(project, TaskMode::Plan, None)).await;
    let binding = bound(&f, &spec).await;
    assert!(binding.read_only && !binding.browser && !binding.github);
    // Nothing else of Ophio's is open to it.
    let refused = f
        .hub
        .handle(
            &binding,
            BridgeRequest::Person {
                action: PersonCall::CanAsk,
            },
        )
        .await;
    assert!(
        matches!(refused, BridgeResponse::Refused { .. }),
        "{refused:?}"
    );
    let hub = f.hub.clone();
    let asking = tokio::spawn(async move {
        hub.handle(
            &binding,
            BridgeRequest::RequestAccess {
                needs: vec![AccessNeed::ChangeFiles],
                reason: "to fix the build".into(),
            },
        )
        .await
    });
    let a = approval_where(&f, |a| a.access_request.is_some()).await;
    assert_eq!(
        a.actions.as_deref(),
        Some(&[ApprovalAction::SwitchToWork, ApprovalAction::Deny][..])
    );
    assert!(
        a.scope.starts_with("Switches this task to Work mode"),
        "{}",
        a.scope
    );
    let receipt = switching(
        &f,
        spec.run_id,
        Command::SwitchToWork {
            task_id,
            approval_id: Some(a.approval_id),
            text: Some("Go ahead".into()),
        },
    )
    .await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    let next = next_spec(&f, task_id, spec.run_id).await;
    assert_eq!(next.mode, TaskMode::Work);
    assert!(
        next.prompt
            .text
            .contains("so you can do what you asked for (to fix the build)"),
        "{}",
        next.prompt.text
    );
    assert!(
        next.prompt.text.ends_with("Go ahead"),
        "{}",
        next.prompt.text
    );
    // The tool call in the ended turn was never answered Allow.
    asking.abort();
    let settled: Approval = f
        .hub
        .read(|r| r.require("approval", a.approval_id))
        .unwrap();
    assert_eq!(settled.state, ApprovalState::Allowed);
    f.close().await;
}

/// While another task changes the project, a switch waits its turn in Next
/// up and starts by itself once the project is free.
#[tokio::test]
async fn switching_waits_its_turn_while_another_task_changes_the_project() {
    let f = Fixture::new().await;
    all_modes(&f);
    let project = f.project().await;
    let (_writer, _) = started(&f, create(project, TaskMode::Work, None)).await;
    let (task_id, spec) = started(&f, create(project, TaskMode::Plan, None)).await;
    let receipt = switching(
        &f,
        spec.run_id,
        Command::SwitchToWork {
            task_id,
            approval_id: None,
            text: None,
        },
    )
    .await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    let task: Task = f.hub.read(|r| r.require("task", task_id)).unwrap();
    assert_eq!(task.mode, TaskMode::Work);
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let queue: QueueView = f.hub.read(|r| r.require("queue", project)).unwrap();
            if queue
                .items
                .iter()
                .any(|i| i.task_id == task_id && i.is_pending())
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("it never waited in Next up");
    assert!(
        !(f.harness.specs.lock().unwrap().iter())
            .any(|s| s.task_id == task_id && s.run_id != spec.run_id)
    );
    f.close().await;
}

/// A running chat turns into a task in one tap, without waiting for its
/// turn to end, keeping its access and adding its own project to change.
#[tokio::test]
async fn a_running_chat_turns_into_a_task_in_one_tap() {
    let backend = Arc::new(Backend::default());
    let f = Fixture::with(|b| b.sandboxes(backend.clone())).await;
    all_modes(&f);
    let project = f.project().await;
    let access = ConversationAccess {
        read: vec![project],
        ..Default::default()
    };
    let (task_id, spec) = started(&f, create(project, TaskMode::Ask, Some(access))).await;
    let receipt = switching(
        &f,
        spec.run_id,
        Command::SwitchToWork {
            task_id,
            approval_id: None,
            text: None,
        },
    )
    .await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    let task: Task = f.hub.read(|r| r.require("task", task_id)).unwrap();
    assert!(!task.conversation);
    assert_eq!(task.mode, TaskMode::Work);
    assert_eq!(task.access.as_ref().unwrap().write, [project]);
    let next = next_spec(&f, task_id, spec.run_id).await;
    assert_eq!(next.mode, TaskMode::Work);
    assert!(
        next.prompt
            .text
            .contains("turned this conversation into a task in Work mode"),
        "{}",
        next.prompt.text
    );
    f.close().await;
}

/// What a running sandbox can't take on as it runs (the whole computer,
/// say) ends the turn, settled as allowed, and a fresh turn of the same
/// session carries on, told what it now has.
#[tokio::test]
async fn what_the_sandbox_cant_widen_carries_on_in_a_fresh_turn() {
    let backend = Arc::new(Backend::default());
    let f = Fixture::with(|b| b.sandboxes(backend.clone())).await;
    all_modes(&f);
    let project = f.project().await;
    let access = ConversationAccess {
        read: vec![project],
        ..Default::default()
    };
    let (task_id, spec) = started(&f, create(project, TaskMode::Ask, Some(access))).await;
    let binding = bound(&f, &spec).await;
    let hub = f.hub.clone();
    let _asking = tokio::spawn(async move {
        hub.handle(
            &binding,
            BridgeRequest::RequestAccess {
                needs: vec![AccessNeed::WholeComputer],
                reason: "to read the system logs".into(),
            },
        )
        .await
    });
    let a = approval_where(&f, |a| a.access_request.is_some()).await;
    backend.needs_restart();
    f.harness
        .controls
        .confirm_terminate
        .store(true, Ordering::SeqCst);
    let before = f.harness.controls.terminates.load(Ordering::SeqCst);
    let ends = async {
        tokio::time::timeout(Duration::from_secs(15), async {
            while f.harness.controls.terminates.load(Ordering::SeqCst) == before {
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
        .await
        .expect("the turn was never ended");
        f.harness
            .emit(
                spec.run_id,
                ports::HarnessEvent::Exited {
                    exit: ports::RunExit::Interrupted,
                    outcome: None,
                },
            )
            .await;
    };
    let (receipt, ()) = tokio::join!(decide(&f, &a, ApprovalDecision::Allow), ends);
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    let next = next_spec(&f, task_id, spec.run_id).await;
    assert_eq!(next.resume_session.as_deref(), Some("the-session"));
    assert!(
        next.prompt.text.contains(
            "to give you what you asked for: you may now read and change anything on this computer"
        ),
        "{}",
        next.prompt.text
    );
    let (task, settled): (Task, Approval) = f
        .hub
        .read(|r| {
            Ok((
                r.require("task", task_id)?,
                r.require("approval", a.approval_id)?,
            ))
        })
        .unwrap();
    assert_eq!(settled.state, ApprovalState::Allowed);
    let after = task.access.unwrap();
    assert!(after.whole_read && after.whole_write && after.write.is_empty());
    // The turn that asked was never handed the Allow: it had ended.
    assert!(!allowed_ever(&f));
    f.close().await;
}

/// Allow on a request for more access changes the task itself, so a phone
/// that may answer requests but not change tasks can't give it.
#[tokio::test]
async fn giving_more_access_needs_a_phone_that_may_change_tasks() {
    let backend = Arc::new(Backend::default());
    let f = Fixture::with(|b| b.sandboxes(backend.clone())).await;
    all_modes(&f);
    let project = f.project().await;
    let access = ConversationAccess {
        read: vec![project],
        ..Default::default()
    };
    let (task_id, spec) = started(&f, create(project, TaskMode::Ask, Some(access.clone()))).await;
    let binding = bound(&f, &spec).await;
    let hub = f.hub.clone();
    let _asking = tokio::spawn(async move {
        hub.handle(
            &binding,
            BridgeRequest::RequestAccess {
                needs: vec![AccessNeed::ChangeFiles],
                reason: "to fix it".into(),
            },
        )
        .await
    });
    let a = approval_where(&f, |a| a.access_request.is_some()).await;
    let phone = f.device().await;
    let Principal::Device { device_id } = phone.principal else {
        unreachable!()
    };
    let stored: crate::identity::Device = f.hub.read(|r| r.require("device", device_id)).unwrap();
    let grants = stored.record.grants;
    let set = CommandEnvelope::new(Command::SetDeviceGrants {
        device_id,
        expected: Some(grants.clone()),
        grants: Grants {
            tasks: false,
            approvals: true,
            ..grants
        },
    });
    let receipt = f.hub.submit(&Caller::local_owner(), set).await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    let mut envelope = CommandEnvelope::new(Command::ResolveApproval {
        approval_id: a.approval_id,
        decision: ApprovalDecision::Allow,
    });
    envelope.expected_revision = Some(a.revision.into());
    let refused = f.hub.submit(&phone, envelope).await;
    assert_eq!(
        refused.error.map(|e| e.code),
        Some(ErrorCode::PermissionDenied)
    );
    let task: Task = f.hub.read(|r| r.require("task", task_id)).unwrap();
    assert_eq!(task.access, Some(access));
    f.close().await;
}

/// Switching as the task is still starting, the moment its mode turns out
/// wrong, waits for the starting turn to end, then carries on in Work.
#[tokio::test]
async fn switching_a_task_still_starting_carries_on_in_work() {
    let f = Fixture::new().await;
    all_modes(&f);
    let project = f.project().await;
    f.harness.acknowledge_spawn.store(false, Ordering::SeqCst);
    let receipt = f
        .hub
        .submit(
            &Caller::local_owner(),
            create(project, TaskMode::Plan, None),
        )
        .await;
    let Some(CommandResult::TaskCreated { task_id, .. }) = receipt.result else {
        panic!("{receipt:?}")
    };
    // Its agent is starting: asked to, and not yet confirmed.
    let spec = dispatched(&f, task_id).await;
    assert!(!f.hub.inner.runs.lock().await.contains_key(&spec.run_id));
    let owner = Caller::local_owner();
    let starts = async {
        tokio::time::sleep(Duration::from_millis(200)).await;
        f.harness.acknowledge_spawn.store(true, Ordering::SeqCst);
        f.harness.spawn_gate.add_permits(1);
        // Stopped as it started, it ends.
        tokio::time::timeout(Duration::from_secs(15), async {
            while f.harness.controls.interrupts.load(Ordering::SeqCst) == 0 {
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
        .await
        .expect("the starting turn was never stopped");
        f.harness
            .emit(
                spec.run_id,
                ports::HarnessEvent::Exited {
                    exit: ports::RunExit::Interrupted,
                    outcome: None,
                },
            )
            .await;
    };
    let (receipt, ()) = tokio::join!(
        f.hub.submit(
            &owner,
            CommandEnvelope::new(Command::SwitchToWork {
                task_id,
                approval_id: None,
                text: None
            })
        ),
        starts
    );
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    let next = next_spec(&f, task_id, spec.run_id).await;
    assert_eq!(next.mode, TaskMode::Work);
    f.close().await;
}

/// The build before this one, as `ophio update --rollback` brings it back,
/// reads what asking for more stores: its approvals and its journal. That
/// build knew these kinds and states, and ignores fields it doesn't know.
#[tokio::test]
async fn the_build_before_reads_what_asking_for_more_stores() {
    #[derive(Debug, serde::Deserialize)]
    #[serde(rename_all = "snake_case")]
    #[allow(dead_code)]
    enum Kind {
        Command,
        FileEdit,
        Network,
        Tool,
        Question,
        ComputerUse,
        LoginUse,
        LoginCode,
        GithubPost,
        Other,
    }
    #[derive(Debug, serde::Deserialize)]
    #[serde(rename_all = "snake_case")]
    #[allow(dead_code)]
    enum State {
        Pending,
        Allowed,
        Denied,
        Expired,
        ResolvedElsewhere,
        Cancelled,
    }
    #[derive(Debug, serde::Deserialize)]
    #[allow(dead_code)]
    struct Before {
        kind: Kind,
        state: State,
    }
    let backend = Arc::new(Backend::default());
    let f = Fixture::with(|b| b.sandboxes(backend.clone())).await;
    all_modes(&f);
    let project = f.project().await;
    let access = ConversationAccess {
        read: vec![project],
        ..Default::default()
    };
    let (_, spec) = started(&f, create(project, TaskMode::Ask, Some(access))).await;
    let binding = bound(&f, &spec).await;
    let hub = f.hub.clone();
    let asking = tokio::spawn(async move {
        hub.handle(
            &binding,
            BridgeRequest::RequestAccess {
                needs: vec![AccessNeed::ChangeFiles],
                reason: "to fix it".into(),
            },
        )
        .await
    });
    let a = approval_where(&f, |a| a.access_request.is_some()).await;
    assert_eq!(a.kind, ApprovalKind::Other);
    let receipt = decide(&f, &a, ApprovalDecision::Allow).await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    asking.await.unwrap();
    // A Plan task's request, with its ways forward, switched past.
    let (plan, plan_spec) = started(&f, create(project, TaskMode::Plan, None)).await;
    let asked = raised(&f, plan_spec.run_id, "make build").await;
    let receipt = switching(
        &f,
        plan_spec.run_id,
        Command::SwitchToWork {
            task_id: plan,
            approval_id: Some(asked.approval_id),
            text: None,
        },
    )
    .await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    let stored: Vec<String> = f
        .hub
        .read(|r| {
            let ids: Vec<Approval> = r.list("approval")?;
            ids.iter()
                .map(|a| Ok(r.text("approval", a.approval_id)?.unwrap()))
                .collect()
        })
        .unwrap();
    assert!(stored.len() >= 2);
    for json in &stored {
        serde_json::from_str::<Before>(json).unwrap_or_else(|e| panic!("{e}: {json}"));
    }
    let journal = f.hub.read(|r| r.journal(0)).unwrap();
    let mut seen = 0;
    for envelope in journal.events {
        let json = serde_json::to_value(&envelope).unwrap();
        if json["event"]["type"] == "approval_upserted" {
            serde_json::from_value::<Before>(json["event"]["approval"].clone())
                .unwrap_or_else(|e| panic!("{e}: {json}"));
            seen += 1;
        }
    }
    assert!(seen >= 4, "{seen}");
    f.close().await;
}

/// Ophio's own gate before an outsider's task runs without project limits
/// is answered as it is: never switched past and never allowed again
/// unasked. Stopping the turn lets go of it at once.
#[tokio::test]
async fn ophio_s_own_gate_is_never_switched_past_and_stop_lets_it_go() {
    let f = Fixture::new().await;
    all_modes(&f);
    let project = f.project().await;
    let (task_id, spec) = started(&f, create(project, TaskMode::Plan, None)).await;
    let task: Task = f.hub.read(|r| r.require("task", task_id)).unwrap();
    let id = ApprovalId::new();
    f.hub
        .write(|tx| tx.put(crate::github_access::START_APPROVAL, spec.run_id, &id))
        .unwrap();
    let hub = f.hub.clone();
    let run = spec.run_id;
    let gate = tokio::spawn(async move {
        hub.bridge_wait_as(
            id,
            run,
            &task,
            ports::tool_approval::ToolApproval {
                kind: ApprovalKind::Other,
                title: "Start without project limits?".into(),
                headline: None,
                subject: Default::default(),
                detail: None,
                detail_lines: Vec::new(),
                questions: Vec::new(),
                scope: "This turn only".into(),
                shortened: false,
            },
            None,
            None,
            None,
        )
        .await
    });
    let a = approval_where(&f, |a| a.approval_id == id).await;
    assert_eq!(a.actions.as_deref(), Some(&[ApprovalAction::Deny][..]));
    let refused = f
        .hub
        .submit(
            &Caller::local_owner(),
            CommandEnvelope::new(Command::SwitchToWork {
                task_id,
                approval_id: Some(id),
                text: None,
            }),
        )
        .await;
    assert_eq!(
        refused.error.map(|e| e.code),
        Some(ErrorCode::InvalidRequest)
    );
    assert!(
        f.hub
            .read(|r| r
                .get::<crate::switch_work::Preapproved>(crate::switch_work::PREAPPROVED, task_id))
            .unwrap()
            .is_none()
    );
    // Stopped, what waits on the gate is let go at once, not at its deadline.
    let seen: Task = f.hub.read(|r| r.require("task", task_id)).unwrap();
    let stop = f
        .hub
        .submit(
            &Caller::local_owner(),
            edit(
                Command::StopTask {
                    task_id,
                    force: false,
                },
                seen.revision,
            ),
        )
        .await;
    assert_eq!(stop.status, ReceiptStatus::Accepted, "{stop:?}");
    let answer = tokio::time::timeout(Duration::from_secs(10), gate)
        .await
        .expect("the gate still waits")
        .unwrap();
    assert!(answer.is_err(), "{answer:?}");
    f.close().await;
}

/// A task that read an outsider's content carries on in Work only from its
/// reviewed plan, never by Switch to Work.
#[tokio::test]
async fn an_outsider_s_task_never_switches_to_work() {
    let f = Fixture::new().await;
    all_modes(&f);
    let project = f.project().await;
    let (task_id, spec) = started(&f, create(project, TaskMode::Plan, None)).await;
    f.hub
        .write(|tx| tx.put(crate::github_access::UNTRUSTED, task_id, &true))
        .unwrap();
    let a = raised(&f, spec.run_id, "make build").await;
    assert_eq!(a.actions.as_deref(), Some(&[ApprovalAction::Deny][..]));
    for approval_id in [None, Some(a.approval_id)] {
        let refused = f
            .hub
            .submit(
                &Caller::local_owner(),
                CommandEnvelope::new(Command::SwitchToWork {
                    task_id,
                    approval_id,
                    text: None,
                }),
            )
            .await;
        assert_eq!(
            refused.error.map(|e| e.code),
            Some(ErrorCode::InvalidRequest)
        );
    }
    let task: Task = f.hub.read(|r| r.require("task", task_id)).unwrap();
    assert_eq!(task.mode, TaskMode::Plan);
    // Its read-only turn has no request_access to ask with either.
    let binding = bound(&f, &spec).await;
    let said = f
        .hub
        .handle(
            &binding,
            BridgeRequest::RequestAccess {
                needs: vec![AccessNeed::ChangeFiles],
                reason: "to fix it".into(),
            },
        )
        .await;
    let BridgeResponse::AccessResult { text } = said else {
        panic!("{said:?}")
    };
    assert!(text.contains("can't be given more"), "{text}");
    f.close().await;
}

/// Switching while another task changes the project leaves a paused Next
/// up paused: the switched task waits there for the person's go-ahead.
#[tokio::test]
async fn switching_leaves_a_paused_next_up_paused() {
    let f = Fixture::new().await;
    all_modes(&f);
    let project = f.project().await;
    let (_writer, _) = started(&f, create(project, TaskMode::Work, None)).await;
    let (task_id, spec) = started(&f, create(project, TaskMode::Plan, None)).await;
    f.hub
        .write(|tx| {
            let mut q: QueueView = tx.read().require("queue", project)?;
            q.paused = true;
            tx.put("queue", project, &q)
        })
        .unwrap();
    let receipt = switching(
        &f,
        spec.run_id,
        Command::SwitchToWork {
            task_id,
            approval_id: None,
            text: None,
        },
    )
    .await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    let queue = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let queue: QueueView = f.hub.read(|r| r.require("queue", project)).unwrap();
            if queue
                .items
                .iter()
                .any(|i| i.task_id == task_id && i.is_pending())
            {
                return queue;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("it never waited in Next up");
    assert!(queue.paused);
    let item = queue.items.iter().find(|i| i.task_id == task_id).unwrap();
    assert_eq!(item.authorization, QueueAuthorization::AskBeforeStart);
    f.close().await;
}

/// Should working out what a request does ever fail, as a parser bug might
/// on some Codex request, the card says Ophio can't tell, highlighted, and
/// the store keeps working: later reads, other requests and Stop.
#[tokio::test]
async fn a_request_the_facts_line_fails_on_leaves_the_store_working() {
    let f = Fixture::new().await;
    all_modes(&f);
    let project = f.project().await;
    let (task_id, spec) = started(&f, create(project, TaskMode::Work, None)).await;
    let native = uuid::Uuid::new_v4().to_string();
    f.harness
        .emit(
            spec.run_id,
            ports::HarnessEvent::ApprovalRequested(ports::NativeApproval {
                native_request_id: native.clone(),
                kind: ApprovalKind::Command,
                title: "Run a command".into(),
                headline: None,
                subject: ports::request_facts::Subject::Command {
                    command: crate::glance::FAILS.into(),
                    cwd: None,
                },
                detail: None,
                detail_lines: Vec::new(),
                questions: Vec::new(),
                raw_request: None,
                scope: "once".into(),
                shortened: false,
                tool_call: None,
            }),
        )
        .await;
    let a = approval_where(&f, |a| a.native_request_id == native).await;
    assert_eq!(a.facts.len(), 1);
    assert_eq!(a.facts[0].text, crate::glance::CANT_TELL);
    assert!(a.facts[0].risky);
    // The store still answers, and another request still gets its card.
    let next = raised(&f, spec.run_id, "make build").await;
    assert_eq!(next.state, ApprovalState::Pending);
    let seen: Task = f.hub.read(|r| r.require("task", task_id)).unwrap();
    let stop = f
        .hub
        .submit(
            &Caller::local_owner(),
            edit(
                Command::StopTask {
                    task_id,
                    force: false,
                },
                seen.revision,
            ),
        )
        .await;
    assert_eq!(stop.status, ReceiptStatus::Accepted, "{stop:?}");
    f.close().await;
}
