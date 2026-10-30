// SPDX-License-Identifier: Apache-2.0
use super::*;

async fn configured(
    rig: &Rig,
    path: PathBuf,
    kind: RootKind,
    project_id: Option<ProjectId>,
) -> LibraryRootId {
    let config = rig
        .owner(OwnerRequest::RootsAdd {
            path: path.clone(),
            label: Some("Documents".into()),
            writable: true,
        })
        .await;
    let id = config
        .roots
        .iter()
        .find(|r| r.path == path)
        .unwrap()
        .root_id;
    rig.owner(OwnerRequest::RootsSet {
        root_id: id,
        kind: Some(kind),
        project_id,
        clear_project: false,
        writable: Some(true),
        harness: Some(HarnessKind::Codex),
        clear_harness: false,
    })
    .await;
    let loaded = Config::load(&rig.paths.config_file()).unwrap();
    let root = loaded
        .root_specs()
        .into_iter()
        .find(|r| r.root_id == id)
        .unwrap();
    assert_eq!(root.kind, kind);
    assert_eq!(root.project_id, project_id);
    id
}

#[tokio::test]
async fn configured_roots_reusable_recovery_and_instruction_baseline() {
    let mut rig = Rig::new(false).await;
    rig.pair().await;
    let mut phone = Phone::new(&rig).await;
    let path = rig.project_dir("project");
    std::fs::write(path.join("AGENTS.md"), "Use deterministic tests.\n").unwrap();
    let project = phone.project(&path).await;
    let root = configured(&rig, path.clone(), RootKind::Project, Some(project)).await;
    let drafts = rig.project_dir("drafts");
    let draft_root = configured(&rig, drafts.clone(), RootKind::Drafts, None).await;
    let task = phone
        .task(
            project,
            HarnessKind::Codex,
            "Write a regression test for the retry policy.",
        )
        .await;
    phone.event(|e| matches!(e, Event::RunUpserted { run } if run.task_id == task && run.acknowledged_at.is_some())).await;
    let QueryData::Instructions { sources } = phone
        .query(Query::Instructions {
            project_id: project,
            task_id: Some(task),
        })
        .await
    else {
        panic!()
    };
    let baseline = sources
        .iter()
        .find(|s| s.path.as_deref() == Some("AGENTS.md"))
        .unwrap();
    assert_eq!(baseline.state, InstructionState::Discovered);
    assert_eq!(baseline.current_revision, baseline.revision_at_start);
    assert_eq!(baseline.size, Some(25));
    assert!(!baseline.loaded_reported);
    std::fs::write(path.join("AGENTS.md"), "A changed instruction.\n").unwrap();
    let QueryData::Instructions { sources } = phone
        .query(Query::Instructions {
            project_id: project,
            task_id: Some(task),
        })
        .await
    else {
        panic!()
    };
    let changed = sources
        .iter()
        .find(|s| s.path.as_deref() == Some("AGENTS.md"))
        .unwrap();
    assert_eq!(changed.state, InstructionState::Changed);
    assert_eq!(changed.revision_at_start, baseline.current_revision);
    let QueryData::ReusableDraft { mut text } =
        phone.query(Query::ReusableDraft { task_id: task }).await
    else {
        panic!()
    };
    assert!(text.contains("Write a regression test"));
    assert!(text.contains("## Boundaries"));
    assert!(std::fs::read_dir(&drafts).unwrap().next().is_none());
    text.push_str("\nReviewed destination and checks.\n");
    assert!(
        matches!(phone.effect(Command::CreateReusableDraft { task_id: task, root_id: draft_root, path: "retry.md".into(), text: text.clone() }).await, Some(CommandResult::DraftCreated { root_id, .. }) if root_id == draft_root)
    );
    assert_eq!(
        std::fs::read_to_string(drafts.join("retry.md")).unwrap(),
        text
    );
    let QueryData::LibraryRead { file } = phone
        .query(Query::LibraryRead {
            root_id: draft_root,
            path: "retry.md".into(),
        })
        .await
    else {
        panic!()
    };
    let Some(CommandResult::LibraryDeleted { item }) = phone
        .effect(Command::LibraryDelete {
            root_id: draft_root,
            path: "retry.md".into(),
            base_revision: file.revision,
        })
        .await
    else {
        panic!()
    };
    let QueryData::LibraryTrash { items } = phone.query(Query::LibraryTrash).await else {
        panic!()
    };
    assert_eq!(items[0].trash_id, item.trash_id);
    let Some(CommandResult::LibraryRestored { path: restored, .. }) = phone
        .effect(Command::LibraryRestore {
            trash_id: item.trash_id,
            base_revision: item.revision,
            copy_path: None,
        })
        .await
    else {
        panic!()
    };
    assert_eq!(restored, "retry.md");
    let denied = rig
        .runtime
        .hub
        .query(
            &services::Caller {
                principal: Principal::System,
                grants: Grants::default(),
            },
            Query::LibraryTrash,
        )
        .await;
    assert_eq!(denied.unwrap_err().code, ErrorCode::PermissionDenied);
    assert_eq!(root, changed.root_id.unwrap());
    drop(phone);
    rig.shutdown().await;
}

#[tokio::test]
async fn transfer_replacement_keeps_old_bytes_recoverable_and_refuses_stale_review() {
    let mut rig = Rig::new(false).await;
    rig.pair().await;
    let mut phone = Phone::new(&rig).await;
    let path = rig.project_dir("project");
    std::fs::write(path.join("notes.md"), "old").unwrap();
    let project = phone.project(&path).await;
    configured(&rig, path.clone(), RootKind::Project, Some(project)).await;
    let destination = TransferDestination::ProjectDir {
        project_id: project,
        dir: "".into(),
    };
    let QueryData::UploadDestination { check } = phone
        .query(Query::UploadDestination {
            name: "notes.md".into(),
            destination: destination.clone(),
        })
        .await
    else {
        panic!()
    };
    assert!(check.exists);
    assert_eq!(check.keep_both_name, "notes (1).md");
    let revision = check.replace_revision.unwrap();
    let owner = services::Caller::local_owner();
    for (review, succeeds) in [(revision.clone(), true), (revision, false)] {
        let Some(CommandResult::Transfer { transfer }) = phone
            .effect(Command::CreateUploadReviewed {
                task_id: None,
                name: "notes.md".into(),
                size: 3,
                sha256: companion::identity::fingerprint(b"new"),
                destination: destination.clone(),
                collision: UploadCollision::Replace { revision: review },
            })
            .await
        else {
            panic!()
        };
        let mut sink = rig
            .runtime
            .hub
            .upload(&owner, transfer.transfer_id)
            .await
            .unwrap();
        sink.write(b"new").await.unwrap();
        assert_eq!(sink.finish().await.is_ok(), succeeds);
    }
    assert_eq!(std::fs::read(path.join("notes.md")).unwrap(), b"new");
    let QueryData::LibraryTrash { items } = phone.query(Query::LibraryTrash).await else {
        panic!()
    };
    assert_eq!(items.len(), 1);
    let QueryData::LibraryTrashRead { file } = phone
        .query(Query::LibraryTrashRead {
            trash_id: items[0].trash_id.clone(),
        })
        .await
    else {
        panic!()
    };
    assert_eq!(file.text, "old");
    drop(phone);
    rig.shutdown().await;
}

#[tokio::test]
async fn a_library_file_that_is_not_text_downloads_as_its_exact_bytes() {
    let mut rig = Rig::new(false).await;
    rig.pair().await;
    let mut phone = Phone::new(&rig).await;
    let path = rig.project_dir("documents");
    // A scan or a picture: not UTF-8, and larger than a note may be to edit.
    let bytes: Vec<u8> = (0..3 * 1024 * 1024u32)
        .map(|i| (i % 251) as u8 ^ 0x80)
        .collect();
    std::fs::write(path.join("scan.pdf"), &bytes).unwrap();
    let root = configured(&rig, path, RootKind::Notes, None).await;
    let Some(CommandResult::Transfer { transfer }) = phone
        .effect(Command::CreateDownload {
            shown_in_task: None,
            source: DownloadSource::LibraryFile {
                root_id: root,
                path: "scan.pdf".into(),
            },
        })
        .await
    else {
        panic!()
    };
    assert_eq!(transfer.size, bytes.len() as u64);
    let mut receiving = rig.client(ConnectionKind::Transfer).await;
    receiving
        .send(&TransferClientFrame::Download {
            transfer_id: transfer.transfer_id,
            offset: 0,
        })
        .await;
    assert!(matches!(
        receiving.receive::<TransferServerFrame>().await,
        TransferServerFrame::Sending { .. }
    ));
    let mut received = Vec::new();
    loop {
        match receiving.message().await {
            tokio_tungstenite::tungstenite::Message::Binary(chunk) => {
                received.extend_from_slice(&chunk)
            }
            tokio_tungstenite::tungstenite::Message::Text(text) => {
                assert!(matches!(
                    serde_json::from_str::<TransferServerFrame>(&text).unwrap(),
                    TransferServerFrame::Completed { .. }
                ));
                break;
            }
            other => panic!("{other:?}"),
        }
    }
    assert!(received == bytes);
    drop(receiving);
    drop(phone);
    rig.shutdown().await;
}
