// SPDX-License-Identifier: Apache-2.0
use super::*;

#[tokio::test]
async fn pending_bridge_approval_expires_on_restart_with_its_flag_intact() {
    use ports::bridge::{BridgeBinding, BridgeHandler, BridgeRequest};
    let f = Fixture::new().await;
    let owner = Caller::local_owner();
    let project = f.project().await;
    let (task, _) = f
        .task(project, TaskPlacement::StartNow, &owner, false)
        .await;
    let run = run_started(&f, task).await;
    let hub = f.hub.clone();
    let pending = tokio::spawn(async move {
        hub.handle(
            &BridgeBinding {
                checks: false,
                run_id: run,
                target_id: None,
                computer_use: false,
                browser: false,
                github: false,
                checklist_answers: false,
                read_only: false,
            },
            BridgeRequest::RequestHuman {
                reason: "Pending at process exit".into(),
            },
        )
        .await
    });
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
    pending.abort();
    let _ = pending.await;
    // Model process death before background workers persist graceful run
    // reconciliation. No database records or bridge flags are manufactured.
    let jobs = std::mem::take(&mut *f.hub.inner.jobs.lock().unwrap());
    for job in &jobs {
        job.abort();
    }
    for job in jobs {
        let _ = job.await;
    }
    f.hub.shutdown().await.unwrap();
    let before: Approval = f
        .hub
        .read(|r| r.require("approval", approval.approval_id))
        .unwrap();
    assert_eq!(before.state, ApprovalState::Pending);
    crate::testing::drop_hub_and_wait(f.hub, WAIT).await;
    let hub = CompanionHub::builder()
        .data_dir(&f.path)
        .harness(f.harness.clone())
        .build()
        .await
        .unwrap();
    let after: Approval = hub
        .read(|r| r.require("approval", approval.approval_id))
        .unwrap();
    assert_eq!(after.state, ApprovalState::Expired);
    assert!(hub.inner.waits.lock().await.is_empty());
    assert_eq!(
        hub.read(|r| r.get::<bool>("bridge_approval", approval.approval_id))
            .unwrap(),
        Some(true)
    );
    assert_eq!(
        hub.resolve_approval(&owner, approval.approval_id, &ApprovalDecision::Allow)
            .await
            .unwrap_err()
            .code,
        ErrorCode::AlreadyResolved
    );
    assert_eq!(f.harness.controls.resolutions.load(Ordering::SeqCst), 0);
    let f = Fixture { hub, ..f };
    f.close().await;
}

// Both real bridge entry points, an answer whose wait has closed, unbound
// keeping one open across a restart, and a native approval sharing the run. No hand-written
// bridge flag or artificial wait entry.
#[tokio::test]
async fn bridge_flag_routes_both_requests_and_does_not_capture_native_approvals() {
    use ports::bridge::{BridgeBinding, BridgeHandler, BridgeRequest};
    let f = Fixture::new().await;
    let owner = Caller::local_owner();
    let project = f.project().await;
    let (task, _) = f
        .task(project, TaskPlacement::StartNow, &owner, false)
        .await;
    let run = run_started(&f, task).await;
    let binding = BridgeBinding {
        checks: false,
        run_id: run,
        target_id: None,
        computer_use: false,
        browser: false,
        github: false,
        checklist_answers: false,
        read_only: false,
    };
    let mut bridge_ids = Vec::new();
    for request in [
        BridgeRequest::PermissionPrompt {
            tool_name: "Bridge permission".into(),
            input: serde_json::json!({}),
            tool_use_id: None,
            native_request_id: Some("bridge-permission".into()),
        },
        BridgeRequest::RequestHuman {
            reason: "Bridge question".into(),
        },
    ] {
        let hub = f.hub.clone();
        let bound = binding.clone();
        let pending = tokio::spawn(async move { hub.handle(&bound, request).await });
        let approval = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(approval) = f
                    .hub
                    .read(|r| r.list::<Approval>("approval"))
                    .unwrap()
                    .into_iter()
                    .find(|a| !bridge_ids.contains(&a.approval_id))
                {
                    break approval;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let id = approval.approval_id;
        bridge_ids.push(id);
        assert_eq!(
            f.hub
                .read(|r| r.get::<bool>("bridge_approval", id))
                .unwrap(),
            Some(true)
        );
        assert!(f.hub.inner.waits.lock().await.contains_key(&id));
        pending.abort();
        let _ = pending.await;
    }
    // Answered after its agent stopped waiting (its receiver closed), the
    // first stays open as it would have, and the answer becomes the task's
    // next message: never a native resolution.
    let first = bridge_ids[0];
    f.hub
        .resolve_approval(&owner, first, &ApprovalDecision::Allow)
        .await
        .unwrap();
    let stored: Approval = f.hub.read(|r| r.require("approval", first)).unwrap();
    assert_eq!(stored.state, ApprovalState::Allowed);
    assert!(
        !f.hub
            .read(|r| crate::carry_on::carried_on(r, first))
            .unwrap()
    );
    assert_eq!(f.harness.controls.resolutions.load(Ordering::SeqCst), 0);
    f.harness
        .emit(
            run,
            ports::HarnessEvent::ApprovalRequested(ports::NativeApproval {
                headline: None,
                subject: Default::default(),
                native_request_id: "native-approval".into(),
                kind: ApprovalKind::Tool,
                title: "Native".into(),
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
    let native = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(approval) = f
                .hub
                .read(|r| r.list::<Approval>("approval"))
                .unwrap()
                .into_iter()
                .find(|a| a.native_request_id == "native-approval")
            {
                break approval;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        f.hub
            .read(|r| r.get::<bool>("bridge_approval", native.approval_id))
            .unwrap(),
        None
    );
    f.hub.unbound(&binding).await;
    // Its agent stopped waiting: the second stays open for a later answer,
    // still shown as needing the person.
    let second = bridge_ids[1];
    let a: Approval = f.hub.read(|r| r.require("approval", second)).unwrap();
    assert_eq!(a.state, ApprovalState::Pending);
    assert!(
        f.hub
            .read(|r| crate::carry_on::carried_on(r, second))
            .unwrap()
    );
    assert!(
        f.hub
            .read(|r| r.list::<AttentionItem>("attention"))
            .unwrap()
            .iter()
            .any(|a| a.approval_id == Some(second) && a.resolved_at.is_none())
    );
    let a: Approval = f
        .hub
        .read(|r| r.require("approval", native.approval_id))
        .unwrap();
    assert_eq!(a.state, ApprovalState::Pending);
    f.hub
        .resolve_approval(&owner, native.approval_id, &ApprovalDecision::Allow)
        .await
        .unwrap();
    assert_eq!(f.harness.controls.resolutions.load(Ordering::SeqCst), 1);

    f.hub.shutdown().await.unwrap();
    crate::testing::drop_hub_and_wait(f.hub, WAIT).await;
    let hub = CompanionHub::builder()
        .data_dir(&f.path)
        .harness(f.harness.clone())
        .build()
        .await
        .unwrap();
    assert!(hub.inner.waits.lock().await.is_empty());
    assert_eq!(
        hub.read(|r| r.get::<bool>("bridge_approval", second))
            .unwrap(),
        Some(true)
    );
    // Still open after the restart, and answered from the phone as any
    // card is: the answer is the task's next message, not handed to any
    // agent's wait.
    let a: Approval = hub.read(|r| r.require("approval", second)).unwrap();
    let mut answer = CommandEnvelope::new(Command::ResolveApproval {
        approval_id: second,
        decision: ApprovalDecision::Answer {
            text: "Yes, go ahead".into(),
            answers: Vec::new(),
        },
    });
    answer.expected_revision = Some(a.revision.into());
    let receipt = hub.submit(&owner, answer).await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let a: Approval = hub.read(|r| r.require("approval", second)).unwrap();
            if a.state == ApprovalState::Allowed {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the answer was recorded");
    assert_eq!(f.harness.controls.resolutions.load(Ordering::SeqCst), 1);
    let f = Fixture { hub, ..f };
    f.close().await;
}

// Restoring a backup, computer side. No simulated deletion of one record:
// restore a complete, closed SQLite backup which already contains the phone.
// The client regressions in journal-reset.test.ts guard against trusting a
// missing receipt with exactly this combination of head, epoch and status.
#[tokio::test]
async fn backup_restore_can_preserve_identity_and_catch_up_after_losing_receipts() {
    let f = Fixture::new().await;
    let phone = f.device().await;
    let Principal::Device { device_id } = phone.principal else {
        panic!("paired device expected");
    };
    let project = f.project().await;
    let host_id = f.hub.host_id();
    f.hub.shutdown().await.unwrap();
    crate::testing::drop_hub_and_wait(f.hub, WAIT).await;
    let database = f.path.join("companion.sqlite");
    let backup = f.path.join("before-command.sqlite");
    std::fs::copy(&database, &backup).unwrap();

    let hub = CompanionHub::builder()
        .data_dir(&f.path)
        .clock(f.clock.clone())
        .harness(f.harness.clone())
        .bridge(bridge::BridgeConfig::new(
            "synthetic-bridge",
            "synthetic-socket",
        ))
        .build()
        .await
        .unwrap();
    let f = Fixture { hub, ..f };
    let (task, command) = f
        .task(project, TaskPlacement::StartNow, &phone, false)
        .await;
    run_started(&f, task).await;
    assert_eq!(f.harness.starts.load(Ordering::SeqCst), 1);
    let phone_head = f.hub.journal_head();
    let epoch = f.hub.boot_epoch();
    f.hub.shutdown().await.unwrap();
    crate::testing::drop_hub_and_wait(f.hub, WAIT).await;

    // A normal reopen retains the command, including its status and dedupe.
    let hub = CompanionHub::builder()
        .data_dir(&f.path)
        .clock(f.clock.clone())
        .harness(f.harness.clone())
        .bridge(bridge::BridgeConfig::new(
            "synthetic-bridge",
            "synthetic-socket",
        ))
        .build()
        .await
        .unwrap();
    assert!(matches!(
        hub.query(
            &phone,
            Query::CommandStatus {
                command_id: command.command_id
            }
        )
        .await
        .unwrap(),
        QueryData::CommandStatus {
            receipt: Some(_),
            ..
        }
    ));
    assert_eq!(
        hub.submit(&phone, command.clone()).await.status,
        ReceiptStatus::Duplicate
    );
    assert_eq!(f.harness.starts.load(Ordering::SeqCst), 1);
    hub.shutdown().await.unwrap();
    crate::testing::drop_hub_and_wait(hub, WAIT).await;

    std::fs::copy(&backup, &database).unwrap();
    let hub = CompanionHub::builder()
        .data_dir(&f.path)
        .clock(f.clock.clone())
        .harness(f.harness.clone())
        .bridge(bridge::BridgeConfig::new(
            "synthetic-bridge",
            "synthetic-socket",
        ))
        .build()
        .await
        .unwrap();
    assert_eq!(hub.host_id(), host_id);
    assert_ne!(hub.boot_epoch(), epoch);
    assert!(!hub.device_credential(&device_id).await.unwrap().revoked);
    // Routine activity while the phone is absent can consume all the reused
    // sequence numbers. The numeric cursor now passes the client's guard.
    while hub.journal_head() <= phone_head {
        hub.refresh_readiness().await.unwrap();
    }
    assert!(matches!(
        hub.query(
            &phone,
            Query::CommandStatus {
                command_id: command.command_id
            }
        )
        .await
        .unwrap(),
        QueryData::CommandStatus { receipt: None, .. }
    ));
    let receipt = hub.submit(&phone, command).await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted);
    let Some(CommandResult::TaskCreated { task_id, .. }) = receipt.result else {
        panic!("new task expected");
    };
    let f = Fixture { hub, ..f };
    run_started(&f, task_id).await;
    assert_eq!(f.harness.starts.load(Ordering::SeqCst), 2);
    f.close().await;
}
