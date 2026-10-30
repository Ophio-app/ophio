// SPDX-License-Identifier: Apache-2.0
use super::*;

/// A computer-use run acts on the desktop. Directing it needs the same
/// `computer_use` grant as starting or resuming it; stopping and denying do not.
#[tokio::test]
async fn only_computer_use_holders_steer_or_answer_a_computer_use_run() {
    let f = Fixture::new().await;
    let owner = Caller::local_owner();
    let project_id = f.project().await;
    let (task_id, _) = f
        .task(project_id, TaskPlacement::StartNow, &owner, true)
        .await;
    let run_id = run_started(&f, task_id).await;
    f.harness
        .emit(
            run_id,
            ports::HarnessEvent::SessionStarted {
                native_session: "session".into(),
                actual_model: None,
                actual_effort: None,
                account_label: None,
            },
        )
        .await;
    assert!(
        tokio::time::timeout(WAIT, async {
            loop {
                let task: Task = f.hub.read(|r| r.require("task", task_id)).unwrap();
                if task.state == TaskState::Working {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .is_ok()
    );
    let approval_id = ApprovalId::new();
    f.hub
        .write(|tx| {
            tx.put("bridge_approval", approval_id, &true)?;
            f.hub.put_approval(
                tx,
                &Approval {
                    approval_id,
                    task_id,
                    run_id,
                    project_id,
                    native_request_id: format!("bridge:{approval_id}"),
                    kind: ApprovalKind::Question,
                    title: "Which account should I use?".into(),
                    detail: None,
                    detail_lines: Vec::new(),
                    questions: Vec::new(),
                    raw_request: None,
                    scope: "This request once".into(),
                    shortened: false,
                    revision: 1,
                    state: ApprovalState::Pending,
                    delivery: None,
                    outcome: None,
                    requested_at: Timestamp(1_700_000_000_000),
                    expires_at: None,
                    resolved_by: None,
                    resolved_at: None,
                },
            )
        })
        .unwrap();
    let chat = Caller {
        principal: pair(&f.hub, 9).await.principal,
        grants: Grants {
            tasks: true,
            approvals: true,
            ..Grants::default()
        },
    };
    let Principal::Device { device_id } = chat.principal else {
        panic!("device")
    };
    let current: Grants = f
        .hub
        .read(|r| {
            Ok(r.require::<identity::Device>("device", device_id)?
                .record
                .grants)
        })
        .unwrap();
    let receipt = f
        .hub
        .submit(
            &owner,
            CommandEnvelope::new(Command::SetDeviceGrants {
                device_id,
                expected: Some(current),
                grants: chat.grants.clone(),
            }),
        )
        .await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    for envelope in [
        CommandEnvelope::new(Command::SendMessage {
            task_id,
            text: "Open the banking site".into(),
            bundle: None,
            delivery: MessageDelivery::Steer,
        }),
        edit(
            Command::ResolveApproval {
                approval_id,
                decision: ApprovalDecision::Answer {
                    text: "Use the business account".into(),
                    answers: Vec::new(),
                },
            },
            1,
        ),
    ] {
        let receipt = f.hub.submit(&chat, envelope).await;
        assert_eq!(receipt.status, ReceiptStatus::Rejected, "{receipt:?}");
        assert_eq!(receipt.error.unwrap().code, ErrorCode::PermissionDenied);
    }
    let deny = f
        .hub
        .submit(
            &chat,
            edit(
                Command::ResolveApproval {
                    approval_id,
                    decision: ApprovalDecision::Deny { reason: None },
                },
                1,
            ),
        )
        .await;
    assert_eq!(deny.status, ReceiptStatus::Accepted, "{deny:?}");
    f.close().await;
}
