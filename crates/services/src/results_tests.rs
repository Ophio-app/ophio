// SPDX-License-Identifier: Apache-2.0
use super::*;
use protocol::annotation::*;

fn bundle(artifact: &ArtifactMeta, mark_id: MarkId) -> Box<AnnotationBundle> {
    Box::new(AnnotationBundle {
        bundle_id: AnnotationBundleId::new(),
        revision: 1,
        sources: vec![AnnotationSource {
            key: "source".into(),
            artifact_id: artifact.artifact_id,
            sha256: artifact.sha256.clone(),
            media_type: artifact.media_type.clone(),
            width: 32,
            height: 24,
            origin: SourceOrigin::PhoneImport {
                original_name: None,
            },
            captured_at: artifact.created_at,
            derived_from: None,
            derivation: None,
        }],
        marks: vec![Mark {
            mark_id,
            label: "A".into(),
            source_key: "source".into(),
            shape: MarkShape::Pin {
                at: StrokePoint {
                    x: 0.5,
                    y: 0.5,
                    t: None,
                },
            },
            color: 0,
            created_at: artifact.created_at,
            deleted: false,
        }],
        comments: vec![],
        steps: vec![],
        general_note: None,
        speech: None,
    })
}

#[tokio::test]
async fn capture_and_upload_links_survive_restart_and_paginate_without_labels() {
    let f = Fixture::new().await;
    let owner = Caller::local_owner();
    let project = f.project().await;
    let source = f
        .hub
        .put_artifact(b"before", "image/png", None, owner.principal)
        .await
        .unwrap();
    let mark = MarkId::new();
    let bundle = bundle(&source, mark);
    let retained_bundle = bundle.clone();
    let annotation = ArtifactAnnotation {
        bundle_id: bundle.bundle_id,
        revision: 1,
        mark_ids: vec![mark],
    };
    let request = CommandEnvelope::new(Command::CreateTask {
        github: None,
        project_id: project,
        kind: TaskKind::Coding,
        mode: None,
        title: Some("Review layout".into()),
        prompt: "Review the drawing".into(),
        bundle: Some(bundle),
        harness: HarnessKind::ClaudeCode,
        model: None,
        effort: None,
        launch_policy: None,
        branch: None,
        mission: None,
        target_id: None,
        placement: TaskPlacement::StartNow,
        agent_chosen: None,
        start: None,
        place: None,
        browser: None,
        conversation: None,
        access: None,
        review: None,
    });
    let mut queued = request.body.clone();
    if let Command::CreateTask { placement, .. } = &mut queued {
        *placement = TaskPlacement::Queue {
            position: None,
            authorization: QueueAuthorization::AskBeforeStart,
        };
    }
    let receipt = f.hub.submit(&owner, request).await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    let Some(CommandResult::TaskCreated { task_id, .. }) = receipt.result else {
        panic!("task")
    };
    let task: Task = f.hub.read(|r| r.require("task", task_id)).unwrap();
    let queued = f.hub.submit(&owner, CommandEnvelope::new(queued)).await;
    assert_eq!(queued.status, ReceiptStatus::Accepted);
    let Some(CommandResult::TaskCreated {
        task_id: queued_task,
        ..
    }) = queued.result
    else {
        panic!("queued task")
    };
    let queued_artifacts = f.hub.task_artifacts(queued_task, None, 100).unwrap();
    assert!(queued_artifacts.artifacts.iter().any(|a| {
        a.artifact_id == source.artifact_id
            && a.links
                .iter()
                .any(|l| l.task_id == queued_task && l.run_id.is_none())
    }));
    let mut changed = retained_bundle.clone();
    changed.marks[0].label = "Changed".into();
    assert!(
        f.hub
            .write(|tx| crate::results::link_bundle(tx, task_id, task.current_run, &changed))
            .is_err()
    );
    run_started(&f, task_id).await;
    let rejected = f
        .hub
        .submit(
            &Caller::local_owner(),
            CommandEnvelope::new(Command::SendMessage {
                task_id,
                text: "Changed drawing".into(),
                bundle: Some(changed.clone()),
                delivery: MessageDelivery::NextTurn,
            }),
        )
        .await
        .error
        .unwrap();
    assert_eq!(rejected.code, ErrorCode::Conflict);
    assert!(f.harness.controls.message_refs.lock().unwrap().is_empty());

    let link = ArtifactLink {
        task_id,
        run_id: task.current_run,
        annotation: Some(annotation),
        role: ArtifactRole::After,
    };
    // A later run explicitly inherits reviewed evidence even though the original
    // message is attached to the first run (as with queued/resumed work).
    f.hub
        .write(|tx| {
            let mut later: Run = tx.read().require("run", task.current_run.unwrap())?;
            later.run_id = RunId::new();
            tx.put("run", later.run_id, &later)?;
            crate::results::link_bundle(tx, task_id, Some(later.run_id), &retained_bundle)?;
            let mut later_link = link.clone();
            later_link.run_id = Some(later.run_id);
            crate::results::validate_link(&tx.read(), &later_link)?;
            later_link.annotation.as_mut().unwrap().revision += 1;
            assert!(crate::results::validate_link(&tx.read(), &later_link).is_err());
            Ok(())
        })
        .unwrap();
    let target_id = f.desktop.target.lock().unwrap().target_id;
    let capture = f
        .hub
        .control_command(
            &owner,
            &Command::CaptureTarget {
                target_id,
                evidence: Some(link.clone()),
            },
        )
        .await
        .unwrap();
    let Some(CommandResult::ArtifactCaptured { artifact }) = capture else {
        panic!("capture")
    };
    assert_eq!(artifact.links, vec![link.clone()]);

    let receipt = f
        .hub
        .submit(
            &owner,
            CommandEnvelope::new(Command::CreateUpload {
                name: "after.png".into(),
                size: 5,
                sha256: hash(b"after"),
                destination: TransferDestination::Artifact,
                evidence: Some(link.clone()),
            }),
        )
        .await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted);
    let Some(CommandResult::Transfer { transfer }) = receipt.result else {
        panic!("upload")
    };
    let mut upload = f.hub.upload(&owner, transfer.transfer_id).await.unwrap();
    upload.write(b"after").await.unwrap();
    let uploaded = upload.finish().await.unwrap().artifact_id.unwrap();
    let mut wrong = link.clone();
    wrong.annotation.as_mut().unwrap().mark_ids = vec![MarkId::new()];
    assert_eq!(
        f.hub
            .put_linked_artifact(b"bad", "image/png", Some(wrong), owner.principal, None)
            .await
            .unwrap_err()
            .code,
        ErrorCode::InvalidRequest
    );
    let mut no_files = owner.clone();
    no_files.principal = Principal::Channel {
        identity_id: ChannelIdentityId::new(),
    };
    assert!(!domain::grants::query(
        &domain::grants::effective(no_files.grants, no_files.principal),
        &Query::TaskArtifacts {
            task_id,
            before: None,
            limit: 10
        }
    ));
    f.hub.shutdown().await.unwrap();
    drop(f.hub);

    let hub = CompanionHub::builder()
        .data_dir(&f.path)
        .clock(f.clock)
        .build()
        .await
        .unwrap();
    let mut before = None;
    let mut all = Vec::new();
    loop {
        let QueryData::TaskArtifacts { history } = hub
            .query(
                &owner,
                Query::TaskArtifacts {
                    task_id,
                    before,
                    limit: 1,
                },
            )
            .await
            .unwrap()
        else {
            panic!("history")
        };
        assert_eq!(history.artifacts.len(), 1);
        all.extend(history.artifacts);
        before = history.next_before;
        if before.is_none() {
            break;
        }
    }
    assert_eq!(all.len(), 3);
    assert!(
        all.iter()
            .any(|a| a.artifact_id == uploaded && a.links == vec![link.clone()])
    );
    assert!(all.iter().any(|a| {
        a.artifact_id == source.artifact_id
            && a.links
                .iter()
                .any(|l| l.role == ArtifactRole::Source && l.run_id == task.current_run)
    }));
    hub.shutdown().await.unwrap();
    drop(hub);
    std::fs::remove_dir_all(f.path).unwrap();
}

#[tokio::test]
async fn portable_brief_is_a_new_task_and_writer_admission_is_unchanged() {
    let f = Fixture::new().await;
    let project = f.project().await;
    let owner = Caller::local_owner();
    let (source, original) = f
        .task(project, TaskPlacement::StartNow, &owner, false)
        .await;
    let Command::CreateTask { mut prompt, .. } = original.body.clone() else {
        panic!("create")
    };
    prompt.push_str("\n\n# Task brief\n\n## Open gaps\n\nReview the missing check.");
    let mut new = original.body;
    if let Command::CreateTask {
        prompt: text,
        title,
        ..
    } = &mut new
    {
        *text = prompt.clone();
        *title = Some("New task from brief".into());
    }
    let refused = f
        .hub
        .submit(&owner, CommandEnvelope::new(new.clone()))
        .await;
    assert_eq!(refused.status, ReceiptStatus::Rejected);
    assert_eq!(refused.error.unwrap().code, ErrorCode::Conflict);
    let t: Task = f.hub.read(|r| r.require("task", source)).unwrap();
    f.hub
        .finish_run(t.current_run.unwrap(), RunState::Completed, None)
        .unwrap();
    let accepted = f.hub.submit(&owner, CommandEnvelope::new(new)).await;
    assert_eq!(accepted.status, ReceiptStatus::Accepted);
    let Some(CommandResult::TaskCreated { task_id, .. }) = accepted.result else {
        panic!("task")
    };
    assert_ne!(source, task_id);
    let (stored, bundle): (String, Option<Box<AnnotationBundle>>) =
        f.hub.read(|r| r.require("prompt", task_id)).unwrap();
    assert_eq!(stored, prompt);
    assert!(bundle.is_none());
    let t: Task = f.hub.read(|r| r.require("task", task_id)).unwrap();
    let intent: serde_json::Value = f
        .hub
        .read(|r| r.require("run_intent", t.current_run.unwrap()))
        .unwrap();
    assert!(intent["resume"].is_null());
    f.close().await;
}

#[tokio::test]
async fn preview_capture_provenance_is_retained_with_the_reviewed_bundle() {
    let f = Fixture::new().await;
    let owner = Caller::local_owner();
    let project = f.project().await;
    let source = f
        .hub
        .put_artifact(b"preview", "image/png", None, owner.principal)
        .await
        .unwrap();
    let mut reviewed = bundle(&source, MarkId::new());
    reviewed.sources[0].origin = SourceOrigin::PreviewCapture {
        capture_id: "retained-picture".into(),
        host_id: HostId::new(),
        computer_name: "studio".into(),
        preview_id: PreviewId::new(),
        preview_label: "Notes".into(),
        captured_at: Some(source.created_at),
    };
    let receipt = f
        .hub
        .submit(
            &owner,
            CommandEnvelope::new(Command::CreateTask {
                github: None,
                project_id: project,
                kind: TaskKind::Coding,
                mode: None,
                title: Some("Review preview".into()),
                prompt: "Review this picture".into(),
                bundle: Some(reviewed.clone()),
                harness: HarnessKind::ClaudeCode,
                model: None,
                effort: None,
                launch_policy: None,
                branch: None,
                mission: None,
                target_id: None,
                placement: TaskPlacement::StartNow,
                agent_chosen: None,
                start: None,
                place: None,
                browser: None,
                conversation: None,
                access: None,
                review: None,
            }),
        )
        .await;
    let Some(CommandResult::TaskCreated { task_id, .. }) = receipt.result else {
        panic!("{receipt:?}")
    };
    let stored: AnnotationBundle = f
        .hub
        .read(|r| {
            r.require(
                &format!("task_annotation:{task_id}"),
                format!("{}:{}", reviewed.bundle_id, reviewed.revision),
            )
        })
        .unwrap();
    assert_eq!(stored.sources[0].origin, reviewed.sources[0].origin);
    assert_eq!(
        stored.sources[0].captured_at,
        reviewed.sources[0].captured_at
    );
}
