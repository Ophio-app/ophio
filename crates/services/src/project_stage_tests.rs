// SPDX-License-Identifier: Apache-2.0
use super::*;
use ports::Clock;
use protocol::project::{ProjectStage, ProjectStageHistory};

async fn history(hub: &CompanionHub, project_id: ProjectId) -> ProjectStageHistory {
    match hub
        .query(
            &Caller::local_owner(),
            Query::ProjectStages {
                project_id,
                before_revision: None,
                limit: 100,
            },
        )
        .await
        .unwrap()
    {
        QueryData::ProjectStages { history } => history,
        _ => panic!("stage history expected"),
    }
}
fn change(project_id: ProjectId, stage: ProjectStage, revision: u64) -> CommandEnvelope {
    edit(
        Command::SetProjectStage {
            project_id,
            stage,
            note: Some("Reviewed scope".into()),
        },
        revision,
    )
}

#[tokio::test]
async fn project_stage_records_device_note_event_and_refuses_stale_and_revoked_authority() {
    let f = Fixture::new().await;
    let project = f.project().await;
    assert!(history(&f.hub, project).await.current.is_none());
    let device = f.device().await;
    let Principal::Device { device_id } = device.principal else {
        panic!("device")
    };
    let restricted = f
        .hub
        .submit(
            &Caller::local_owner(),
            CommandEnvelope::new(Command::SetDeviceGrants {
                device_id,
                expected: Some(domain::grants::manager_defaults()),
                grants: domain::grants::device_defaults(),
            }),
        )
        .await;
    settle(&f.hub, restricted.command_id).await;
    let request = change(project, ProjectStage::Planning, 0);
    let refused = f.hub.submit(&device, request.clone()).await;
    assert_eq!(refused.error.unwrap().code, ErrorCode::PermissionDenied);
    let grant = f
        .hub
        .submit(
            &Caller::local_owner(),
            CommandEnvelope::new(Command::SetDeviceGrants {
                device_id,
                expected: Some(domain::grants::device_defaults()),
                grants: Grants::owner(),
            }),
        )
        .await;
    assert_eq!(grant.status, ReceiptStatus::Accepted);
    let accepted = f.hub.submit(&device, request.clone()).await;
    assert_eq!(accepted.status, ReceiptStatus::Accepted);
    let Some(CommandResult::ProjectStageSet { transition, .. }) = accepted.result.clone() else {
        panic!("recorded transition")
    };
    assert_eq!(transition.revision, 1);
    assert_eq!(transition.from, None);
    assert_eq!(transition.to, ProjectStage::Planning);
    assert_eq!(transition.principal, device.principal);
    assert_eq!(transition.actor_name, "Phone");
    assert_eq!(transition.note.as_deref(), Some("Reviewed scope"));
    assert_eq!(transition.at, f.clock.now());
    let duplicate = f.hub.submit(&device, request).await;
    assert_eq!(duplicate.status, ReceiptStatus::Duplicate);
    assert_eq!(duplicate.result, accepted.result);
    let changed = f
        .hub
        .submit(
            &Caller::local_owner(),
            change(project, ProjectStage::Review, 1),
        )
        .await;
    assert_eq!(changed.status, ReceiptStatus::Accepted);
    let stale = f
        .hub
        .submit(&device, change(project, ProjectStage::Implementation, 1))
        .await;
    let failure = stale.error.unwrap();
    assert_eq!(failure.code, ErrorCode::StaleRevision);
    assert_eq!(failure.detail.as_ref().unwrap()["current_stage"], "review");
    assert_eq!(failure.detail.unwrap()["current_revision"], 2);
    assert_eq!(history(&f.hub, project).await.transitions.len(), 2);
    let events = f.hub.read(|r| Ok(r.journal(0)?.events)).unwrap();
    assert!(events.iter().any(|e| matches!(&e.event, Event::ProjectUpserted { project: p } if p.stage.as_ref() == Some(&transition))));
    // Current persisted authority wins over the caller's old cached grants.
    let downgrade = f
        .hub
        .submit(
            &Caller::local_owner(),
            CommandEnvelope::new(Command::SetDeviceGrants {
                device_id,
                expected: Some(Grants::owner()),
                grants: Grants::default(),
            }),
        )
        .await;
    settle(&f.hub, downgrade.command_id).await;
    assert_eq!(
        f.hub
            .submit(
                &Caller {
                    grants: Grants::owner(),
                    ..device
                },
                change(project, ProjectStage::Implementation, 2)
            )
            .await
            .error
            .unwrap()
            .code,
        ErrorCode::PermissionDenied
    );
    f.close().await;
}

#[tokio::test]
async fn implementation_release_and_reverse_transitions_leave_execution_authority_untouched() {
    let f = Fixture::new().await;
    let owner = Caller::local_owner();
    let project = f.project().await;
    let _device = f.device().await;
    f.task(
        project,
        TaskPlacement::Queue {
            position: None,
            authorization: QueueAuthorization::AutoStart,
        },
        &owner,
        false,
    )
    .await;
    let before = f
        .hub
        .query(
            &owner,
            Query::Snapshot {
                reads_withheld: None,
            },
        )
        .await
        .unwrap();
    let queue = f
        .hub
        .query(
            &owner,
            Query::Queue {
                project_id: project,
            },
        )
        .await
        .unwrap();
    let devices = f.hub.query(&owner, Query::Devices).await.unwrap();
    for (revision, stage) in [
        ProjectStage::Implementation,
        ProjectStage::Released,
        ProjectStage::Planning,
    ]
    .into_iter()
    .enumerate()
    {
        assert_eq!(
            f.hub
                .submit(&owner, change(project, stage, revision as u64))
                .await
                .status,
            ReceiptStatus::Accepted
        );
        let mut after = f
            .hub
            .query(
                &owner,
                Query::Snapshot {
                    reads_withheld: None,
                },
            )
            .await
            .unwrap();
        if let (QueryData::Snapshot(after), QueryData::Snapshot(before)) = (&mut after, &before) {
            after.journal_head = before.journal_head;
            after.projects.iter_mut().for_each(|p| p.stage = None);
        }
        assert_eq!(after, before);
        assert_eq!(
            f.hub
                .query(
                    &owner,
                    Query::Queue {
                        project_id: project
                    }
                )
                .await
                .unwrap(),
            queue
        );
        assert_eq!(f.hub.query(&owner, Query::Devices).await.unwrap(), devices);
        assert!(f.hub.inner.runs.lock().await.is_empty());
    }
    let h = history(&f.hub, project).await;
    assert_eq!(h.transitions[0].from, Some(ProjectStage::Released));
    assert_eq!(h.transitions[0].to, ProjectStage::Planning);
    f.close().await;
}

#[tokio::test]
async fn agent_completion_claim_cannot_promote_a_project() {
    let f = Fixture::new().await;
    let project = f.project().await;
    let owner = Caller::local_owner();
    f.hub
        .submit(&owner, change(project, ProjectStage::Prototype, 0))
        .await;
    let before = history(&f.hub, project).await;
    let (task, _) = f
        .task(project, TaskPlacement::StartNow, &owner, false)
        .await;
    let run = run_started(&f, task).await;
    f.hub
        .finish_run(
            run,
            RunState::Completed,
            Some(RunOutcome {
                summary: Some("ready for production".into()),
                changed_files: vec![],
                checks: vec![],
                still_active: vec![],
                unknowns: vec![],
                usage: None,
                report: None,
                verification: None,
            }),
        )
        .unwrap();
    assert_eq!(
        f.hub
            .read(|r| r.require::<Task>("task", task))
            .unwrap()
            .state,
        TaskState::Completed
    );
    assert_eq!(history(&f.hub, project).await, before);
    f.close().await;
}
