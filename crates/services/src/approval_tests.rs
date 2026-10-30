// SPDX-License-Identifier: Apache-2.0
use super::*;

#[tokio::test]
async fn closed_permission_bridge_records_failed_delivery_and_keeps_attention_actionable() {
    let f = Fixture::new().await;
    let owner = Caller::local_owner();
    let project_id = f.project().await;
    let (task_id, _) = f
        .task(project_id, TaskPlacement::StartNow, &owner, false)
        .await;
    let run_id = run_started(&f, task_id).await;
    let approval_id = ApprovalId::new();
    let approval = Approval {
        approval_id,
        task_id,
        run_id,
        project_id,
        native_request_id: format!("bridge:{approval_id}"),
        kind: ApprovalKind::Tool,
        title: "Permit one tool".into(),
        detail: None,
        detail_lines: Vec::new(),
        questions: Vec::new(),
        raw_request: None,
        scope: "Once".into(),
        shortened: false,
        revision: 1,
        state: ApprovalState::Pending,
        delivery: None,
        outcome: None,
        requested_at: Timestamp(1_700_000_000_000),
        expires_at: None,
        resolved_by: None,
        resolved_at: None,
    };
    // As the permission bridge admits it: flagged as a bridge approval, with a wait.
    f.hub
        .write(|tx| {
            tx.put("bridge_approval", approval_id, &true)?;
            f.hub.put_approval(tx, &approval)
        })
        .unwrap();
    let (sender, receiver) = tokio::sync::oneshot::channel();
    f.hub.inner.waits.lock().await.insert(approval_id, sender);
    // The bridge receiver has gone away before Allow reaches it; no native
    // decision can be delivered. This exercises resolve_approval's real wait route.
    drop(receiver);
    let request = edit(
        Command::ResolveApproval {
            approval_id,
            decision: ApprovalDecision::Allow,
        },
        1,
    );
    let id = request.command_id;
    assert_eq!(
        f.hub.submit(&owner, request).await.status,
        ReceiptStatus::Accepted
    );
    settle(&f.hub, id).await;
    let work = f.hub.read(|r| Ok(r.command(id)?.unwrap())).unwrap();
    assert_eq!(work.stage, CommandStage::Failed);
    assert_eq!(work.error.unwrap().code, ErrorCode::NativeFailure);
    let stored: Approval = f.hub.read(|r| r.require("approval", approval_id)).unwrap();
    assert_eq!(stored.state, ApprovalState::Pending);
    let delivery = stored.delivery.as_ref().unwrap();
    assert_eq!(delivery.decision, ApprovalDecision::Allow);
    assert_eq!(
        delivery.error.as_ref().unwrap().message,
        "The agent did not receive this. It may still be waiting."
    );
    assert!(stored.resolved_at.is_none());
    let attention = f
        .hub
        .read(|r| r.list::<AttentionItem>("attention"))
        .unwrap();
    assert!(
        attention
            .iter()
            .any(|a| a.approval_id == Some(approval_id) && a.resolved_at.is_none())
    );
    assert!(
        f.hub
            .read(|r| r.get::<CommandId>("approval_claim", approval_id))
            .unwrap()
            .is_none()
    );
    // A later decision is admitted again. Without a live bridge it still fails,
    // rather than falling back to an unrelated native approval route.
    let denied = f
        .hub
        .resolve_approval(
            &owner,
            approval_id,
            &ApprovalDecision::Deny { reason: None },
        )
        .await;
    assert_eq!(denied.unwrap_err().code, ErrorCode::NativeFailure);
    let stored: Approval = f.hub.read(|r| r.require("approval", approval_id)).unwrap();
    assert_eq!(stored.state, ApprovalState::Pending);
    assert!(matches!(
        stored.delivery.unwrap().decision,
        ApprovalDecision::Deny { .. }
    ));
    // When a native route is waiting again, the same attention item can be answered.
    let (sender, receiver) = tokio::sync::oneshot::channel();
    f.hub.inner.waits.lock().await.insert(approval_id, sender);
    let request = edit(
        Command::ResolveApproval {
            approval_id,
            decision: ApprovalDecision::Allow,
        },
        stored.revision.into(),
    );
    let id = request.command_id;
    assert_eq!(
        f.hub.submit(&owner, request).await.status,
        ReceiptStatus::Accepted
    );
    settle(&f.hub, id).await;
    assert_eq!(receiver.await.unwrap(), ApprovalDecision::Allow);
    let stored: Approval = f.hub.read(|r| r.require("approval", approval_id)).unwrap();
    assert_eq!(stored.state, ApprovalState::Allowed);
    assert!(stored.delivery.unwrap().error.is_none());
    assert!(
        f.hub
            .read(|r| r.list::<AttentionItem>("attention"))
            .unwrap()
            .iter()
            .filter(|a| a.approval_id == Some(approval_id))
            .all(|a| a.resolved_at.is_some())
    );
    f.close().await;
}

#[tokio::test]
async fn attention_dismissed_before_a_failed_hand_off_is_raised_again_as_new() {
    let f = Fixture::new().await;
    let owner = Caller::local_owner();
    let device = f.device().await;
    let project_id = f.project().await;
    let (task_id, _) = f
        .task(project_id, TaskPlacement::StartNow, &owner, false)
        .await;
    let run_id = run_started(&f, task_id).await;
    let approval_id = ApprovalId::new();
    let approval = Approval {
        approval_id,
        task_id,
        run_id,
        project_id,
        native_request_id: format!("bridge:{approval_id}"),
        kind: ApprovalKind::Tool,
        title: "Permit one tool".into(),
        detail: None,
        detail_lines: Vec::new(),
        questions: Vec::new(),
        raw_request: None,
        scope: "Once".into(),
        shortened: false,
        revision: 1,
        state: ApprovalState::Pending,
        delivery: None,
        outcome: None,
        requested_at: f.hub.inner.clock.now(),
        expires_at: None,
        resolved_by: None,
        resolved_at: None,
    };
    f.hub
        .write(|tx| {
            tx.put("bridge_approval", approval_id, &true)?;
            f.hub.put_approval(tx, &approval)
        })
        .unwrap();
    let items = || {
        f.hub
            .read(|r| r.list::<AttentionItem>("attention"))
            .unwrap()
            .into_iter()
            .filter(|a| a.approval_id == Some(approval_id))
            .collect::<Vec<_>>()
    };
    let first = items()[0].attention_id;
    // A person dismisses it, then their Allow cannot be handed to the agent.
    let dismiss = CommandEnvelope::new(Command::AcknowledgeAttention {
        attention_id: first,
    });
    assert_eq!(
        f.hub.submit(&owner, dismiss).await.status,
        ReceiptStatus::Accepted
    );
    let (sender, receiver) = tokio::sync::oneshot::channel();
    f.hub.inner.waits.lock().await.insert(approval_id, sender);
    drop(receiver);
    let failed = f
        .hub
        .resolve_approval(&owner, approval_id, &ApprovalDecision::Allow)
        .await;
    assert_eq!(failed.unwrap_err().code, ErrorCode::NativeFailure);
    // The dismissed item stays resolved; one new item, under a new id, is open.
    let after = items();
    assert_eq!(after.len(), 2, "{after:?}");
    assert!(
        after
            .iter()
            .any(|a| a.attention_id == first && a.resolved_at.is_some())
    );
    let open: Vec<_> = after.iter().filter(|a| a.resolved_at.is_none()).collect();
    assert_eq!(open.len(), 1);
    assert_ne!(open[0].attention_id, first);
    assert_eq!(open[0].kind, AttentionKind::Approval);
    assert_eq!(open[0].title, "Permit one tool");
    // A watcher that showed the first, however it reconnects, sees a new alert.
    let (alerts, _) = f.hub.open_alerts(&device).await.unwrap();
    let ids: Vec<_> = alerts.iter().map(|a| a.attention_id).collect();
    assert_eq!(ids, vec![open[0].attention_id]);
    // Failing again while it is still open raises nothing more.
    let (sender, receiver) = tokio::sync::oneshot::channel();
    f.hub.inner.waits.lock().await.insert(approval_id, sender);
    drop(receiver);
    let again = f
        .hub
        .resolve_approval(&owner, approval_id, &ApprovalDecision::Allow)
        .await;
    assert_eq!(again.unwrap_err().code, ErrorCode::NativeFailure);
    assert_eq!(items().len(), 2);
    f.close().await;
}

// Allow is on its way to the agent's route when the run that asked ends.
// `refused`: the route turns it down before the agent sees it. Returns what the
// computer recorded for the command and the request, once both have settled.
async fn decision_overtaken_by_the_runs_end(
    refused: bool,
) -> (Fixture, store::StoredCommand, Approval) {
    let f = Fixture::new().await;
    let owner = Caller::local_owner();
    let project = f.project().await;
    let (task, _) = f
        .task(project, TaskPlacement::StartNow, &owner, false)
        .await;
    let run = run_started(&f, task).await;
    f.harness
        .emit(
            run,
            ports::HarnessEvent::ApprovalRequested(ports::NativeApproval {
                native_request_id: "late-tool".into(),
                kind: ApprovalKind::Command,
                title: "Run test".into(),
                detail: None,
                detail_lines: Vec::new(),
                questions: Vec::new(),
                raw_request: None,
                scope: "Once".into(),
                shortened: false,
                tool_call: None,
            }),
        )
        .await;
    let approval = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(a) = f
                .hub
                .read(|r| r.list::<Approval>("approval"))
                .unwrap()
                .into_iter()
                .next()
            {
                break a;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let controls = &f.harness.controls;
    controls.hold_resolution.store(true, Ordering::SeqCst);
    controls.refuse_resolution.store(refused, Ordering::SeqCst);
    let request = edit(
        Command::ResolveApproval {
            approval_id: approval.approval_id,
            decision: ApprovalDecision::Allow,
        },
        approval.revision.into(),
    );
    let id = request.command_id;
    assert_eq!(
        f.hub.submit(&owner, request).await.status,
        ReceiptStatus::Accepted
    );
    controls
        .resolution_entered
        .acquire()
        .await
        .unwrap()
        .forget();
    f.harness
        .emit(
            run,
            ports::HarnessEvent::Exited {
                exit: ports::RunExit::Completed,
                outcome: None,
            },
        )
        .await;
    let stored = |f: &Fixture| {
        f.hub
            .read(|r| r.require::<Approval>("approval", approval.approval_id))
            .unwrap()
    };
    tokio::time::timeout(Duration::from_secs(5), async {
        while stored(&f).state != ApprovalState::Cancelled {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    controls.resolution_gate.add_permits(1);
    settle(&f.hub, id).await;
    let work = f.hub.read(|r| Ok(r.command(id)?.unwrap())).unwrap();
    let approval = stored(&f);
    (f, work, approval)
}

// A decision the run's end kept from its agent is refused as late rather than
// reported as accepted, and so is one sent once the run has ended.
#[tokio::test]
async fn a_decision_that_arrives_as_the_run_ends_is_refused_as_late() {
    let (f, work, cancelled) = decision_overtaken_by_the_runs_end(true).await;
    assert_eq!(work.stage, CommandStage::Failed);
    let refusal = work.error.unwrap();
    assert_eq!(refusal.code, ErrorCode::Expired);
    assert_eq!(refusal.message, "Arrived after the run ended; not applied");
    assert_eq!(cancelled.state, ApprovalState::Cancelled);
    assert!(cancelled.delivery.is_none());
    let receipt = f
        .hub
        .submit(
            &Caller::local_owner(),
            edit(
                Command::ResolveApproval {
                    approval_id: cancelled.approval_id,
                    decision: ApprovalDecision::Deny { reason: None },
                },
                cancelled.revision.into(),
            ),
        )
        .await;
    assert_eq!(receipt.status, ReceiptStatus::Rejected);
    assert_eq!(receipt.error.unwrap().message, refusal.message);
    f.close().await;
}

// The agent may have acted on a decision it took just as its run ended, so the
// computer keeps what was sent and calls the effect unknown, not "not applied".
#[tokio::test]
async fn a_decision_the_agent_takes_as_its_run_ends_is_kept_and_called_unknown() {
    let (f, work, cancelled) = decision_overtaken_by_the_runs_end(false).await;
    assert_eq!(work.stage, CommandStage::ReconciliationRequired);
    let unknown = work.error.unwrap();
    assert_eq!(unknown.code, ErrorCode::OutcomeUnknown);
    assert_eq!(
        unknown.message,
        "Reached the agent as its run ended; check what it did"
    );
    assert_eq!(cancelled.state, ApprovalState::Cancelled);
    let delivery = cancelled.delivery.unwrap();
    assert_eq!(delivery.decision, ApprovalDecision::Allow);
    assert!(delivery.error.is_none());
    f.close().await;
}

// A tool call the person denied, or that Stop cancelled while it waited for
// an answer, never ran: its failure is recorded as theirs, not the tool's.
#[tokio::test]
async fn a_denied_or_stopped_tool_call_is_recorded_as_not_run() {
    use ports::bridge::{BridgeBinding, BridgeHandler, BridgeRequest};
    let f = Fixture::new().await;
    let owner = Caller::local_owner();
    let project = f.project().await;
    let (task, _) = f
        .task(project, TaskPlacement::StartNow, &owner, false)
        .await;
    let run = run_started(&f, task).await;
    let ask = |call: &str, request: &str| {
        let hub = f.hub.clone();
        let request = BridgeRequest::PermissionPrompt {
            tool_name: "Bash".into(),
            input: serde_json::json!({"command":"rm -r build"}),
            tool_use_id: Some(call.into()),
            native_request_id: Some(request.into()),
        };
        tokio::spawn(async move {
            let binding = BridgeBinding {
                run_id: run,
                target_id: None,
                computer_use: false,
                browser: false,
                github: false,
                checklist_answers: false,
            };
            hub.handle(&binding, request).await
        })
    };
    let pending = || async {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(a) = f
                    .hub
                    .read(|r| r.list::<Approval>("approval"))
                    .unwrap()
                    .into_iter()
                    .find(|a| a.state == ApprovalState::Pending)
                {
                    break a;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap()
    };
    let failed = |call: &str| ports::HarnessEvent::Activity {
        key: call.into(),
        tool: "Bash".into(),
        summary: "Bash finished".into(),
        status: ActivityStatus::Failed,
        detail: None,
    };

    let denied = ask("toolu_denied", "1");
    let approval = pending().await;
    f.hub
        .resolve_approval(
            &owner,
            approval.approval_id,
            &ApprovalDecision::Deny { reason: None },
        )
        .await
        .unwrap();
    denied.await.unwrap();
    f.harness.emit(run, failed("toolu_denied")).await;
    // A call that ran and failed stays a failure.
    f.harness.emit(run, failed("toolu_ran")).await;
    // A harness that names the call behind its own request, as Codex does.
    f.harness
        .emit(
            run,
            ports::HarnessEvent::ApprovalRequested(ports::NativeApproval {
                native_request_id: "3".into(),
                kind: ApprovalKind::Command,
                title: "Run a command".into(),
                detail: None,
                detail_lines: Vec::new(),
                questions: Vec::new(),
                raw_request: None,
                scope: "Once".into(),
                shortened: false,
                tool_call: Some("item_denied".into()),
            }),
        )
        .await;
    let approval = pending().await;
    f.hub
        .resolve_approval(
            &owner,
            approval.approval_id,
            &ApprovalDecision::Deny { reason: None },
        )
        .await
        .unwrap();
    f.harness.emit(run, failed("item_denied")).await;
    let stopped = ask("toolu_stopped", "2");
    pending().await;
    let seen: Task = f.hub.read(|r| r.require("task", task)).unwrap();
    assert_eq!(
        f.hub
            .submit(
                &owner,
                edit(
                    Command::StopTask {
                        task_id: task,
                        force: false,
                    },
                    seen.revision,
                ),
            )
            .await
            .status,
        ReceiptStatus::Accepted
    );
    f.harness.emit(run, failed("toolu_stopped")).await;
    let steps = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let (items, _) = f.hub.read(|r| r.timeline(task, None, 100)).unwrap();
            let steps: Vec<_> = items
                .into_iter()
                .filter_map(|i| match i.item {
                    TimelineBody::Activity { key, not_run, .. } => Some((key.unwrap(), not_run)),
                    _ => None,
                })
                .collect();
            if steps.len() == 4 {
                break steps;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        steps,
        [
            ("toolu_denied".into(), Some(NotRun::Denied)),
            ("toolu_ran".into(), None),
            ("item_denied".into(), Some(NotRun::Denied)),
            ("toolu_stopped".into(), Some(NotRun::Stopped)),
        ]
    );
    stopped.abort();
    f.close().await;
}

// Stop can cancel a request whose Allow is already on its way to the agent. Its
// tool may then run, so a failure is the tool's, not a step Stop kept from
// running. Once the route turns the Allow away, the step was stopped after all.
#[tokio::test]
async fn a_step_whose_allow_may_have_reached_the_agent_is_not_called_stopped() {
    for refused in [false, true] {
        let f = Fixture::new().await;
        let owner = Caller::local_owner();
        let project = f.project().await;
        let (task, _) = f
            .task(project, TaskPlacement::StartNow, &owner, false)
            .await;
        let run = run_started(&f, task).await;
        f.harness
            .emit(
                run,
                ports::HarnessEvent::ApprovalRequested(ports::NativeApproval {
                    native_request_id: "7".into(),
                    kind: ApprovalKind::Command,
                    title: "Run a command".into(),
                    detail: None,
                    detail_lines: Vec::new(),
                    questions: Vec::new(),
                    raw_request: None,
                    scope: "Once".into(),
                    shortened: false,
                    tool_call: Some("item_allowed".into()),
                }),
            )
            .await;
        let approval = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(a) = f
                    .hub
                    .read(|r| r.list::<Approval>("approval"))
                    .unwrap()
                    .into_iter()
                    .next()
                {
                    break a;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let controls = &f.harness.controls;
        controls.hold_resolution.store(true, Ordering::SeqCst);
        controls.refuse_resolution.store(refused, Ordering::SeqCst);
        let allow = edit(
            Command::ResolveApproval {
                approval_id: approval.approval_id,
                decision: ApprovalDecision::Allow,
            },
            approval.revision.into(),
        );
        let allow_id = allow.command_id;
        assert_eq!(
            f.hub.submit(&owner, allow).await.status,
            ReceiptStatus::Accepted
        );
        controls
            .resolution_entered
            .acquire()
            .await
            .unwrap()
            .forget();
        let seen: Task = f.hub.read(|r| r.require("task", task)).unwrap();
        assert_eq!(
            f.hub
                .submit(
                    &owner,
                    edit(
                        Command::StopTask {
                            task_id: task,
                            force: false,
                        },
                        seen.revision,
                    ),
                )
                .await
                .status,
            ReceiptStatus::Accepted
        );
        let failed = ports::HarnessEvent::Activity {
            key: "item_allowed".into(),
            tool: "Bash".into(),
            summary: "Bash finished".into(),
            status: ActivityStatus::Failed,
            detail: None,
        };
        let steps = |count: usize| {
            let hub = f.hub.clone();
            async move {
                tokio::time::timeout(Duration::from_secs(5), async {
                    loop {
                        let (items, _) = hub.read(|r| r.timeline(task, None, 100)).unwrap();
                        let steps: Vec<_> = items
                            .into_iter()
                            .filter_map(|i| match i.item {
                                TimelineBody::Activity { not_run, .. } => Some(not_run),
                                _ => None,
                            })
                            .collect();
                        if steps.len() == count {
                            break steps;
                        }
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .unwrap()
            }
        };
        // The agent's route has the Allow, and nothing yet says what became of it.
        f.harness.emit(run, failed.clone()).await;
        assert_eq!(steps(1).await, [None]);
        controls.resolution_gate.add_permits(1);
        settle(&f.hub, allow_id).await;
        f.harness.emit(run, failed).await;
        let after = if refused { Some(NotRun::Stopped) } else { None };
        assert_eq!(steps(2).await, [None, after], "refused: {refused}");
        f.close().await;
    }
}

// Both delivery routes resume native work after Deny. A later native end is a
// result to review, not an invented question waiting for a second answer.
#[tokio::test]
async fn denied_native_and_bridge_permissions_resume_then_end_with_review_attention() {
    use ports::bridge::{BridgeBinding, BridgeHandler, BridgeRequest, BridgeResponse};
    for bridge in [false, true] {
        let f = Fixture::new().await;
        let owner = Caller::local_owner();
        let project = f.project().await;
        let (task, _) = f
            .task(project, TaskPlacement::StartNow, &owner, false)
            .await;
        let run = run_started(&f, task).await;
        let pending = if bridge {
            let hub = f.hub.clone();
            Some(tokio::spawn(async move {
                hub.handle(
                    &BridgeBinding {
                        run_id: run,
                        target_id: None,
                        computer_use: false,
                        browser: false,
                        github: false,
                        checklist_answers: false,
                    },
                    BridgeRequest::PermissionPrompt {
                        tool_name: "Bash".into(),
                        input: serde_json::json!({"command":"test"}),
                        tool_use_id: None,
                        native_request_id: Some("denied-tool".into()),
                    },
                )
                .await
            }))
        } else {
            f.harness
                .emit(
                    run,
                    ports::HarnessEvent::ApprovalRequested(ports::NativeApproval {
                        native_request_id: "denied-tool".into(),
                        kind: ApprovalKind::Command,
                        title: "Run test".into(),
                        detail: None,
                        detail_lines: Vec::new(),
                        questions: Vec::new(),
                        raw_request: None,
                        scope: "Once".into(),
                        shortened: false,
                        tool_call: None,
                    }),
                )
                .await;
            None
        };
        let approval = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(a) = f
                    .hub
                    .read(|r| r.list::<Approval>("approval"))
                    .unwrap()
                    .into_iter()
                    .next()
                {
                    break a;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            f.hub
                .read(|r| r.require::<Task>("task", task))
                .unwrap()
                .state,
            TaskState::AwaitingDecision
        );
        assert_eq!(
            f.hub
                .read(|r| r.list::<AttentionItem>("attention"))
                .unwrap()
                .iter()
                .filter(|a| a.resolved_at.is_none())
                .count(),
            1
        );
        f.hub
            .resolve_approval(
                &owner,
                approval.approval_id,
                &ApprovalDecision::Deny {
                    reason: Some("Do not run it".into()),
                },
            )
            .await
            .unwrap();
        let stored: Approval = f
            .hub
            .read(|r| r.require("approval", approval.approval_id))
            .unwrap();
        assert_eq!(
            stored.delivery.unwrap().decision,
            ApprovalDecision::Deny {
                reason: Some("Do not run it".into())
            }
        );
        if let Some(pending) = pending {
            let BridgeResponse::Permission { decision } = pending.await.unwrap() else {
                panic!("permission response")
            };
            assert_eq!(decision["behavior"], "deny");
            assert_eq!(decision["message"], "Do not run it");
        } else {
            assert_eq!(f.harness.controls.resolutions.load(Ordering::SeqCst), 1);
        }
        assert_eq!(
            f.hub
                .read(|r| r.require::<Task>("task", task))
                .unwrap()
                .state,
            TaskState::Working
        );
        assert_eq!(
            f.hub.read(|r| r.require::<Run>("run", run)).unwrap().state,
            RunState::Working
        );
        assert!(
            f.hub
                .read(|r| r.list::<AttentionItem>("attention"))
                .unwrap()
                .iter()
                .all(|a| a.resolved_at.is_some())
        );
        f.harness
            .emit(
                run,
                ports::HarnessEvent::Exited {
                    exit: ports::RunExit::Completed,
                    outcome: None,
                },
            )
            .await;
        tokio::time::timeout(Duration::from_secs(5), async {
            while f
                .hub
                .read(|r| r.require::<Task>("task", task))
                .unwrap()
                .state
                != TaskState::Completed
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            f.hub
                .read(|r| r.require::<Task>("task", task))
                .unwrap()
                .state,
            TaskState::Completed
        );
        let open = f
            .hub
            .read(|r| r.list::<AttentionItem>("attention"))
            .unwrap()
            .into_iter()
            .filter(|a| a.resolved_at.is_none())
            .collect::<Vec<_>>();
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].kind, AttentionKind::Completed);
        f.close().await;
    }
}

#[tokio::test]
async fn bridge_tool_approval_facts_match_fixtures_and_bound_oversized_input() {
    use ports::bridge::{BridgeBinding, BridgeHandler, BridgeRequest, BridgeResponse};
    let f = Fixture::new().await;
    let project = f.project().await;
    let (task, _) = f
        .task(
            project,
            TaskPlacement::StartNow,
            &Caller::local_owner(),
            false,
        )
        .await;
    let run = run_started(&f, task).await;
    let binding = BridgeBinding {
        run_id: run,
        target_id: None,
        computer_use: false,
        browser: false,
        github: false,
        checklist_answers: false,
    };
    let cases: serde_json::Value = serde_json::from_str(include_str!(
        "../../ports/test-fixtures/tool-approvals.json"
    ))
    .unwrap();
    let mut cases = cases.as_array().unwrap().clone();
    cases.push(serde_json::json!({"tool":"Bash","input":{"command":"界".repeat(4000)}}));
    for (index, case) in cases.iter().enumerate() {
        let native_id = format!("fixture-{index}");
        let request = BridgeRequest::PermissionPrompt {
            tool_name: case["tool"].as_str().unwrap().into(),
            input: case["input"].clone(),
            tool_use_id: None,
            native_request_id: Some(native_id.clone()),
        };
        let hub = f.hub.clone();
        let bound = binding.clone();
        let pending = tokio::spawn(async move { hub.handle(&bound, request).await });
        let approval = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(a) = f
                    .hub
                    .read(|r| r.list::<Approval>("approval"))
                    .unwrap()
                    .into_iter()
                    .find(|a| a.native_request_id == native_id)
                {
                    break a;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        if !case["approval"].is_null() {
            let mut facts = serde_json::json!({"kind":approval.kind,"title":approval.title,"detail":approval.detail,"detail_lines":approval.detail_lines,"scope":approval.scope,"shortened":approval.shortened});
            if !approval.questions.is_empty() {
                facts["questions"] = serde_json::json!(approval.questions);
            }
            assert_eq!(facts, case["approval"]);
        } else {
            let detail = approval.detail.as_ref().unwrap();
            assert!(detail.chars().count() <= ports::tool_approval::DETAIL_LIMIT);
            assert!(detail.ends_with('…'));
            assert!(approval.shortened);
        }
        assert_eq!(approval.raw_request, Some(case["input"].to_string()));
        let answer = |answers: Vec<QuestionAnswer>| ApprovalDecision::Answer {
            text: "Always up; Invoices, Statements".into(),
            answers,
        };
        let asked = |question: &ApprovalQuestion, choices: &[&str]| QuestionAnswer {
            id: None,
            question: question.question.clone(),
            choices: choices.iter().map(|&choice| choice.into()).collect(),
        };
        let decision = match &approval.questions[..] {
            [] => ApprovalDecision::Allow,
            [rounding, reports] => {
                for wrong in [
                    vec![asked(rounding, &["Always up"])],
                    vec![
                        asked(rounding, &["Always up", "Nearest cent"]),
                        asked(reports, &["Invoices"]),
                    ],
                    vec![
                        asked(rounding, &["Always up"]),
                        asked(rounding, &["Always up"]),
                    ],
                    vec![asked(rounding, &["Always up"]), asked(reports, &[" "])],
                ] {
                    let receipt = f
                        .hub
                        .submit(
                            &Caller::local_owner(),
                            edit(
                                Command::ResolveApproval {
                                    approval_id: approval.approval_id,
                                    decision: answer(wrong),
                                },
                                approval.revision.into(),
                            ),
                        )
                        .await;
                    assert_eq!(receipt.status, ReceiptStatus::Rejected, "{receipt:?}");
                    assert_eq!(receipt.error.unwrap().code, ErrorCode::InvalidRequest);
                }
                answer(vec![
                    asked(rounding, &["Always up"]),
                    asked(reports, &["Invoices", "Statements"]),
                ])
            }
            _ => panic!("unexpected questions in {}", case["tool"]),
        };
        let decision = edit(
            Command::ResolveApproval {
                approval_id: approval.approval_id,
                decision,
            },
            approval.revision.into(),
        );
        let id = decision.command_id;
        assert_eq!(
            f.hub.submit(&Caller::local_owner(), decision).await.status,
            ReceiptStatus::Accepted
        );
        settle(&f.hub, id).await;
        let BridgeResponse::Permission { decision } = pending.await.unwrap() else {
            panic!("expected a permission reply")
        };
        if approval.questions.is_empty() {
            assert_eq!(
                decision,
                serde_json::json!({"behavior":"allow","updatedInput":case["input"]})
            );
        } else {
            assert_eq!(
                decision,
                serde_json::json!({"behavior":"allow","updatedInput":{
                    "questions":case["input"]["questions"],
                    "answers":{
                        "How should totals be rounded?":"Always up",
                        "Which reports need the change?":"Invoices, Statements"
                    }
                }})
            );
        }
    }
    let response = f
        .hub
        .handle(
            &binding,
            BridgeRequest::PermissionPrompt {
                tool_name: "Bash".into(),
                input: serde_json::json!({"command":"x".repeat(1024 * 1024)}),
                tool_use_id: None,
                native_request_id: Some("too-large".into()),
            },
        )
        .await;
    assert!(matches!(response, BridgeResponse::Refused { reason } if reason.contains("too large")));
    f.close().await;
}

/// The permission bridge alone records a Claude Code permission request. A
/// copy of the same request in the run's event stream, before or after the
/// bridge admits it, adds no record and never costs Claude Code the person's
/// decision.
#[tokio::test]
async fn a_bridged_permission_has_one_record_whichever_copy_arrives_first() {
    use ports::bridge::{BridgeHandler, BridgeRequest, BridgeResponse};
    for copy_first in [true, false] {
        let f = Fixture::new().await;
        let owner = Caller::local_owner();
        let project = f.project().await;
        let (task, _) = f
            .task(project, TaskPlacement::StartNow, &owner, false)
            .await;
        let run = run_started(&f, task).await;
        let token = f
            .harness
            .specs
            .lock()
            .unwrap()
            .last()
            .and_then(|spec| spec.bridge.as_ref())
            .and_then(|bridge| {
                bridge
                    .env
                    .iter()
                    .find(|(key, _)| key == ports::bridge::RUN_TOKEN_ENV)
                    .map(|(_, token)| token.clone())
            })
            .expect("bridge token");
        let binding = f.hub.bind(&token).await.expect("bridge binding");
        // Events for one run apply in order; the note shows the copy applied.
        let copy = |note: &'static str| {
            let f = &f;
            async move {
                f.harness
                    .emit(
                        run,
                        ports::HarnessEvent::ApprovalRequested(ports::NativeApproval {
                            native_request_id: "7".into(),
                            kind: ApprovalKind::Command,
                            title: "Run a shell command".into(),
                            detail: None,
                            detail_lines: Vec::new(),
                            questions: Vec::new(),
                            raw_request: None,
                            scope: "This request once".into(),
                            shortened: false,
                            tool_call: None,
                        }),
                    )
                    .await;
                f.harness
                    .emit(run, ports::HarnessEvent::Warning { text: note.into() })
                    .await;
                tokio::time::timeout(Duration::from_secs(5), async {
                    loop {
                        let items: Vec<TimelineItem> =
                            f.hub.read(|r| r.list(&format!("timeline:{task}"))).unwrap();
                        if items
                            .iter()
                            .any(|i| matches!(&i.item, TimelineBody::Note { text } if text == note))
                        {
                            break;
                        }
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .expect("the copy was applied");
            }
        };
        if copy_first {
            copy("copy before").await;
        }
        let pending = tokio::spawn({
            let hub = f.hub.clone();
            async move {
                hub.handle(
                    &binding,
                    BridgeRequest::PermissionPrompt {
                        tool_name: "Bash".into(),
                        input: serde_json::json!({"command":"test"}),
                        tool_use_id: None,
                        native_request_id: Some("7".into()),
                    },
                )
                .await
            }
        });
        let recorded = || {
            f.hub
                .read(|r| r.list::<Approval>("approval"))
                .unwrap()
                .into_iter()
                .filter(|a| a.native_request_id == "7")
                .collect::<Vec<_>>()
        };
        tokio::time::timeout(Duration::from_secs(5), async {
            while recorded().is_empty() && !pending.is_finished() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        if !copy_first {
            copy("copy after").await;
        }
        let approvals = recorded();
        assert_eq!(approvals.len(), 1, "copy first: {copy_first}");
        let approval = &approvals[0];
        assert_eq!(approval.state, ApprovalState::Pending);
        assert!(
            f.hub
                .read(|r| r.get::<bool>("bridge_approval", approval.approval_id))
                .unwrap()
                .unwrap_or(false),
            "the bridge owns the record"
        );
        f.hub
            .resolve_approval(&owner, approval.approval_id, &ApprovalDecision::Allow)
            .await
            .unwrap();
        let BridgeResponse::Permission { decision } = pending.await.unwrap() else {
            panic!("the person's decision reaches Claude Code (copy first: {copy_first})")
        };
        assert_eq!(decision["behavior"], "allow");
        f.close().await;
    }
}

/// Codex asks over its own app-server even when the computer tools are bound,
/// so its question is the only copy: it is recorded, the task waits on the
/// person, and their answer reaches Codex rather than the bridge.
#[tokio::test]
async fn a_codex_question_is_recorded_when_its_computer_tools_are_bound() {
    use ports::bridge::BridgeHandler;
    let f = Fixture::new().await;
    let owner = Caller::local_owner();
    let project = f.project().await;
    let (task, _) = f
        .task(project, TaskPlacement::StartNow, &owner, false)
        .await;
    let run = run_started(&f, task).await;
    f.hub
        .write(|tx| {
            let mut run: Run = tx.read().require("run", run)?;
            run.harness = HarnessKind::Codex;
            tx.put("run", run.run_id, &run)
        })
        .unwrap();
    let token = f
        .harness
        .specs
        .lock()
        .unwrap()
        .last()
        .and_then(|spec| spec.bridge.as_ref())
        .and_then(|bridge| {
            bridge
                .env
                .iter()
                .find(|(key, _)| key == ports::bridge::RUN_TOKEN_ENV)
                .map(|(_, token)| token.clone())
        })
        .expect("bridge token");
    f.hub.bind(&token).await.expect("bridge binding");
    f.harness
        .emit(
            run,
            ports::HarnessEvent::ApprovalRequested(ports::NativeApproval {
                native_request_id: "codex-question".into(),
                kind: ApprovalKind::Question,
                title: "The agent asked a question".into(),
                detail: Some("Which branch should the fix go on?".into()),
                detail_lines: Vec::new(),
                questions: Vec::new(),
                raw_request: None,
                scope: "This question".into(),
                shortened: false,
                tool_call: None,
            }),
        )
        .await;
    let recorded = || {
        f.hub
            .read(|r| r.list::<Approval>("approval"))
            .unwrap()
            .into_iter()
            .filter(|a| a.native_request_id == "codex-question")
            .collect::<Vec<_>>()
    };
    tokio::time::timeout(Duration::from_secs(5), async {
        while recorded().is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the question was recorded");
    let approvals = recorded();
    assert_eq!(approvals.len(), 1);
    assert_eq!(approvals[0].state, ApprovalState::Pending);
    let waiting: Task = f.hub.read(|r| r.require("task", task)).unwrap();
    assert_eq!(waiting.state, TaskState::AwaitingDecision);
    f.hub
        .resolve_approval(
            &owner,
            approvals[0].approval_id,
            &ApprovalDecision::Answer {
                text: "main".into(),
                answers: Vec::new(),
            },
        )
        .await
        .unwrap();
    assert_eq!(f.harness.controls.resolutions.load(Ordering::SeqCst), 1);
    f.close().await;
}

#[tokio::test]
async fn secret_answer_is_not_retained_after_failed_delivery() {
    let f = Fixture::new().await;
    let owner = Caller::local_owner();
    let project_id = f.project().await;
    let (task_id, _) = f
        .task(project_id, TaskPlacement::StartNow, &owner, false)
        .await;
    let run_id = run_started(&f, task_id).await;
    let approval_id = ApprovalId::new();
    let approval = Approval {
        approval_id,
        task_id,
        run_id,
        project_id,
        native_request_id: format!("bridge:{approval_id}"),
        kind: ApprovalKind::Tool,
        title: "Permit one tool".into(),
        detail: None,
        detail_lines: Vec::new(),
        questions: vec![ApprovalQuestion {
            id: None,
            header: None,
            question: "Password?".into(),
            options: Vec::new(),
            multi_select: false,
            secret: true,
        }],
        raw_request: None,
        scope: "Once".into(),
        shortened: false,
        revision: 1,
        state: ApprovalState::Pending,
        delivery: None,
        outcome: None,
        requested_at: Timestamp(1_700_000_000_000),
        expires_at: None,
        resolved_by: None,
        resolved_at: None,
    };
    // As the permission bridge admits it: flagged as a bridge approval, with a wait.
    f.hub
        .write(|tx| {
            tx.put("bridge_approval", approval_id, &true)?;
            f.hub.put_approval(tx, &approval)
        })
        .unwrap();
    let (sender, receiver) = tokio::sync::oneshot::channel();
    f.hub.inner.waits.lock().await.insert(approval_id, sender);
    // The bridge receiver has gone away before Allow reaches it; no native
    // decision can be delivered. This exercises resolve_approval's real wait route.
    drop(receiver);
    let request = edit(
        Command::ResolveApproval {
            approval_id,
            decision: ApprovalDecision::Answer {
                text: "issue91-password-sentinel".into(),
                answers: vec![QuestionAnswer {
                    id: None,
                    question: "Password?".into(),
                    choices: vec!["issue91-password-sentinel".into()],
                }],
            },
        },
        1,
    );
    let id = request.command_id;
    assert_eq!(
        f.hub.submit(&owner, request).await.status,
        ReceiptStatus::Accepted
    );
    settle(&f.hub, id).await;
    let work = f.hub.read(|r| Ok(r.command(id)?.unwrap())).unwrap();
    assert!(
        !serde_json::to_string(&work)
            .unwrap()
            .contains("issue91-password-sentinel")
    );
    assert_eq!(work.stage, CommandStage::Failed);
    assert_eq!(work.error.unwrap().code, ErrorCode::NativeFailure);
    let stored: Approval = f.hub.read(|r| r.require("approval", approval_id)).unwrap();
    assert_eq!(stored.state, ApprovalState::Pending);
    let delivery = stored.delivery.as_ref().unwrap();
    assert!(
        !serde_json::to_string(delivery)
            .unwrap()
            .contains("issue91-password-sentinel")
    );
    assert_eq!(
        delivery.error.as_ref().unwrap().message,
        "The agent did not receive this. It may still be waiting."
    );
    assert!(stored.resolved_at.is_none());
    let attention = f
        .hub
        .read(|r| r.list::<AttentionItem>("attention"))
        .unwrap();
    assert!(
        attention
            .iter()
            .any(|a| a.approval_id == Some(approval_id) && a.resolved_at.is_none())
    );
    assert!(
        f.hub
            .read(|r| r.get::<CommandId>("approval_claim", approval_id))
            .unwrap()
            .is_none()
    );
    f.close().await;
}

#[tokio::test]
async fn secret_answers_deliver_natively_but_never_replay_to_another_device() {
    for route in ["success", "uncertain", "late"] {
        let f = Fixture::new().await;
        let owner = Caller::local_owner();
        let project_id = f.project().await;
        let (task_id, _) = f
            .task(project_id, TaskPlacement::StartNow, &owner, false)
            .await;
        let run_id = run_started(&f, task_id).await;
        let approval_id = ApprovalId::new();
        let approval = Approval {
            approval_id,
            task_id,
            run_id,
            project_id,
            native_request_id: format!("bridge:{approval_id}"),
            kind: ApprovalKind::Question,
            title: "Private question".into(),
            detail: None,
            detail_lines: Vec::new(),
            questions: vec![ApprovalQuestion {
                id: None,
                header: None,
                question: "Password?".into(),
                options: Vec::new(),
                multi_select: false,
                secret: true,
            }],
            raw_request: None,
            scope: "Once".into(),
            shortened: false,
            revision: 1,
            state: ApprovalState::Pending,
            delivery: None,
            outcome: None,
            requested_at: Timestamp(1_700_000_000_000),
            expires_at: None,
            resolved_by: None,
            resolved_at: None,
        };

        f.hub.write(|tx| f.hub.put_approval(tx, &approval)).unwrap();
        let phone = f.device().await;
        let Principal::Device { device_id } = phone.principal else {
            unreachable!()
        };
        f.hub
            .write(|tx| {
                let mut device: crate::identity::Device = tx.read().require("device", device_id)?;
                device.record.grants.accounts = false;
                tx.put("device", device_id, &device)
            })
            .unwrap();
        if route == "uncertain" {
            f.hub.inner.runs.lock().await.remove(&run_id);
        } else if route == "late" {
            f.hub
                .write(|tx| tx.put(crate::carry_on::CARRIED_ON, approval_id, &Timestamp(1)))
                .unwrap();
        }
        let decision = ApprovalDecision::Answer {
            text: "issue91-password-sentinel".into(),
            answers: vec![QuestionAnswer {
                id: None,
                question: "Password?".into(),
                choices: vec!["issue91-password-sentinel".into()],
            }],
        };
        let request = edit(
            Command::ResolveApproval {
                approval_id,
                decision: decision.clone(),
            },
            1,
        );
        let id = request.command_id;
        assert_eq!(
            f.hub.submit(&owner, request.clone()).await.status,
            if route == "late" {
                ReceiptStatus::Rejected
            } else {
                ReceiptStatus::Accepted
            }
        );
        settle(&f.hub, id).await;
        assert_eq!(
            f.hub.submit(&owner, request).await.status,
            ReceiptStatus::Duplicate
        );
        let stored = f.hub.read(|r| Ok(r.command(id)?.unwrap())).unwrap();
        assert!(
            !serde_json::to_string(&stored)
                .unwrap()
                .contains("issue91-password-sentinel")
        );
        let queried = f
            .hub
            .query(&phone, Query::Approval { approval_id })
            .await
            .unwrap();
        assert!(
            !serde_json::to_string(&queried)
                .unwrap()
                .contains("issue91-password-sentinel")
        );
        let events = f.hub.read(|r| r.journal(0)).unwrap().events;
        assert!(
            !serde_json::to_string(&events)
                .unwrap()
                .contains("issue91-password-sentinel")
        );
        let decisions = f.harness.controls.decisions.lock().unwrap().clone();
        match route {
            "success" => {
                assert_eq!(decisions, vec![decision.clone()]);
                assert_eq!(stored.stage, CommandStage::Observed);
            }
            "uncertain" => {
                assert!(decisions.is_empty());
                assert_eq!(stored.error.unwrap().code, ErrorCode::OutcomeUnknown);
            }
            _ => {
                assert!(decisions.is_empty());
                assert_eq!(
                    stored.receipt.error.as_ref().unwrap().code,
                    ErrorCode::Expired
                );
                assert!(
                    f.hub
                        .read(|r| r.entries::<serde_json::Value>(crate::followups::FOLLOWUP))
                        .unwrap()
                        .is_empty()
                );
            }
        }
        if route == "late" {
            assert!(
                f.hub
                    .read(|r| r.get::<CommandId>("approval_claim", approval_id))
                    .unwrap()
                    .is_none()
            );
            // Deterministic carry-on race: admission has claimed the approval
            // before execution discovers that the native question moved on.
            f.hub
                .write(|tx| tx.put("approval_claim", approval_id, &CommandId::new()))
                .unwrap();
            let failure = f
                .hub
                .resolve_approval(&owner, approval_id, &decision)
                .await
                .unwrap_err();
            assert_eq!(failure.code, ErrorCode::Expired);
            assert!(
                f.hub
                    .read(|r| r.get::<CommandId>("approval_claim", approval_id))
                    .unwrap()
                    .is_none()
            );
            let denied = edit(
                Command::ResolveApproval {
                    approval_id,
                    decision: ApprovalDecision::Deny { reason: None },
                },
                1,
            );
            let denied_id = denied.command_id;
            assert_eq!(
                f.hub.submit(&owner, denied).await.status,
                ReceiptStatus::Accepted
            );
            settle(&f.hub, denied_id).await;
            let approval: Approval = f.hub.read(|r| r.require("approval", approval_id)).unwrap();
            assert_eq!(approval.state, ApprovalState::Denied);
            assert!(
                f.hub
                    .read(|r| r.list::<AttentionItem>("attention"))
                    .unwrap()
                    .iter()
                    .filter(|item| item.approval_id == Some(approval_id))
                    .all(|item| item.resolved_at.is_some())
            );
        }
        // Rejected answers are history too, including requests for unknown ids.
        for rejected_id in [approval_id, ApprovalId::new()] {
            let rejected = edit(
                Command::ResolveApproval {
                    approval_id: rejected_id,
                    decision: ApprovalDecision::Answer {
                        text: "issue91-password-sentinel".into(),
                        answers: Vec::new(),
                    },
                },
                999,
            );
            let rejected_command = rejected.command_id;
            assert_eq!(
                f.hub.submit(&owner, rejected).await.status,
                ReceiptStatus::Rejected
            );
            let kept = f
                .hub
                .read(|r| Ok(r.command(rejected_command)?.unwrap()))
                .unwrap();
            assert!(
                !serde_json::to_string(&kept)
                    .unwrap()
                    .contains("issue91-password-sentinel")
            );
        }
        let f = f.reopened(|builder| builder).await;
        let stored = f.hub.read(|r| Ok(r.command(id)?.unwrap())).unwrap();
        assert!(
            !serde_json::to_string(&stored)
                .unwrap()
                .contains("issue91-password-sentinel")
        );
        f.close().await;
    }
}

#[tokio::test]
async fn legacy_rejected_private_followup_commands_are_scrubbed_on_reopen() {
    let mut f = Fixture::new().await;
    let owner = Caller::local_owner();
    let project_id = f.project().await;
    let (task_id, _) = f
        .task(project_id, TaskPlacement::StartNow, &owner, false)
        .await;
    let run_id = run_started(&f, task_id).await;
    let approval_id = ApprovalId::new();
    let approval = Approval {
        approval_id,
        task_id,
        run_id,
        project_id,
        native_request_id: format!("bridge:{approval_id}"),
        kind: ApprovalKind::Question,
        title: "Private question".into(),
        detail: None,
        detail_lines: Vec::new(),
        questions: vec![ApprovalQuestion {
            id: None,
            header: None,
            question: "Password?".into(),
            options: Vec::new(),
            multi_select: false,
            secret: true,
        }],
        raw_request: None,
        scope: "Once".into(),
        shortened: false,
        revision: 1,
        state: ApprovalState::Pending,
        delivery: None,
        outcome: None,
        requested_at: Timestamp(1_700_000_000_000),
        expires_at: None,
        resolved_by: None,
        resolved_at: None,
    };

    f.hub.write(|tx| f.hub.put_approval(tx, &approval)).unwrap();

    for queued in [false, true] {
        let mut followup = crate::followups::Followup::answer(
            task_id,
            approval_id,
            "issue91-password-sentinel".into(),
            Timestamp(1_900_000_000_000),
        );
        // Keep the legacy follow-up pending while admission rejects its stale
        // request. This is the old application's retained state at upgrade.
        followup.due_at = Timestamp(1_900_000_000_000);
        let body = if queued {
            Command::QueueContinuation {
                task_id,
                text: followup.text.clone(),
                bundle: None,
                authorization: QueueAuthorization::AutoStart,
            }
        } else {
            Command::ResumeTask {
                task_id,
                text: Some(followup.text.clone()),
                bundle: None,
                mode: None,
                carried_plan: None,
                read_only: false,
                model: None,
                effort: None,
            }
        };
        let mut request = edit(body, 999);
        request.command_id = followup.command_id;
        f.hub.write(|tx| followup.put(tx)).unwrap();
        assert_eq!(
            f.hub.submit(&owner, request).await.status,
            ReceiptStatus::Rejected
        );
        let before = f
            .hub
            .read(|r| Ok(r.command(followup.command_id)?.unwrap()))
            .unwrap();
        assert!(
            serde_json::to_string(&before)
                .unwrap()
                .contains("issue91-password-sentinel")
        );
        f = f.reopened(|builder| builder).await;
        let after = f
            .hub
            .read(|r| Ok(r.command(followup.command_id)?.unwrap()))
            .unwrap();
        let kept: crate::followups::Followup = f
            .hub
            .read(|r| r.require(crate::followups::FOLLOWUP, &followup.key))
            .unwrap();
        assert!(
            !serde_json::to_string(&after)
                .unwrap()
                .contains("issue91-password-sentinel")
        );
        assert!(!kept.text.contains("issue91-password-sentinel"));
        let mut expected = serde_json::to_value(&before).unwrap();
        expected["envelope"]["body"]["text"] = serde_json::json!(kept.text);
        assert_eq!(serde_json::to_value(&after).unwrap(), expected);
        assert_eq!(kept.command_id, followup.command_id);
    }
    f.close().await;
}

#[tokio::test]
async fn codex_question_fixtures_pass_through_approval_resolution() {
    let f = Fixture::new().await;
    let owner = Caller::local_owner();
    let project = f.project().await;
    let (task, _) = f
        .task(project, TaskPlacement::StartNow, &owner, false)
        .await;
    let run = run_started(&f, task).await;
    let cases: serde_json::Value = serde_json::from_str(include_str!(
        "../../harness-codex/test-fixtures/questions.json"
    ))
    .unwrap();
    for case in cases.as_array().unwrap() {
        let native_id = case["name"].as_str().unwrap();
        f.harness
            .emit(
                run,
                ports::HarnessEvent::ApprovalRequested(ports::NativeApproval {
                    native_request_id: native_id.into(),
                    kind: ApprovalKind::Question,
                    title: case["approval"]["title"].as_str().unwrap().into(),
                    detail: None,
                    detail_lines: vec![],
                    questions: serde_json::from_value(case["approval"]["questions"].clone())
                        .unwrap(),
                    raw_request: Some(case["input"].to_string()),
                    tool_call: None,
                    scope: "Only your answer".into(),
                    shortened: false,
                }),
            )
            .await;
        let approval = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(approval) = f
                    .hub
                    .read(|r| r.list::<Approval>("approval"))
                    .unwrap()
                    .into_iter()
                    .find(|a| a.native_request_id == native_id)
                {
                    break approval;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        for bad in ["unknown", "no_id", "duplicate", "missing"] {
            let mut decision = case["decision"].clone();
            match bad {
                "unknown" => decision["answers"][0]["id"] = serde_json::json!("unknown"),
                "no_id" => {
                    if case["approval"]["questions"][0]["question"]
                        != case["approval"]["questions"][1]["question"]
                    {
                        continue;
                    }
                    decision["answers"][0]["id"] = serde_json::Value::Null;
                }
                "duplicate" => {
                    let first = decision["answers"][0].clone();
                    decision["answers"].as_array_mut().unwrap().push(first);
                }
                "missing" => {
                    if decision["answers"].as_array().unwrap().len() == 1 {
                        continue;
                    }
                    decision["answers"].as_array_mut().unwrap().pop();
                }
                _ => unreachable!(),
            }
            let receipt = f
                .hub
                .submit(
                    &owner,
                    edit(
                        Command::ResolveApproval {
                            approval_id: approval.approval_id,
                            decision: serde_json::from_value(decision).unwrap(),
                        },
                        approval.revision.into(),
                    ),
                )
                .await;
            assert_eq!(
                receipt.error.unwrap().code,
                ErrorCode::InvalidRequest,
                "{bad}"
            );
        }
        let decision: ApprovalDecision = serde_json::from_value(case["decision"].clone()).unwrap();
        let command = edit(
            Command::ResolveApproval {
                approval_id: approval.approval_id,
                decision: decision.clone(),
            },
            approval.revision.into(),
        );
        let id = command.command_id;
        assert_eq!(
            f.hub.submit(&owner, command).await.status,
            ReceiptStatus::Accepted
        );
        settle(&f.hub, id).await;
        let stored: Approval = f
            .hub
            .read(|r| r.require("approval", approval.approval_id))
            .unwrap();
        assert_eq!(stored.state, ApprovalState::Allowed);
        assert_eq!(stored.delivery.unwrap().decision, decision);
    }
    assert_eq!(
        f.harness.controls.resolutions.load(Ordering::SeqCst),
        cases.as_array().unwrap().len() as u64
    );
}
