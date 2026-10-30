// SPDX-License-Identifier: Apache-2.0
//! What an agent shows the person through its bridge, and what the person
//! deletes of what they were shown or captured.
use super::*;
use ports::TerminalHost;
use ports::bridge::{BridgeBinding, BridgeHandler, BridgeRequest, BridgeResponse, PersonCall};
use protocol::annotation::{AnnotationBundle, AnnotationSource, SourceOrigin};

pub(super) const PNG: &[u8] = b"\x89PNG\r\n\x1a\nsynthetic";

/// A Work task's run and its bridge.
pub(super) async fn bound(f: &Fixture) -> (TaskId, RunId, BridgeBinding) {
    let project = f.project().await;
    let (task, _) = f
        .task(
            project,
            TaskPlacement::StartNow,
            &Caller::local_owner(),
            false,
        )
        .await;
    let run = run_started(f, task).await;
    let bridge = (f.harness.specs.lock().unwrap().last())
        .and_then(|spec| spec.bridge.clone())
        .expect("a bridge");
    let token = (bridge.env.iter())
        .find(|(key, _)| key == ports::bridge::RUN_TOKEN_ENV)
        .map(|(_, token)| token.clone())
        .unwrap();
    (task, run, f.hub.bind(&token).await.expect("bridge binding"))
}
pub(super) async fn person(
    f: &Fixture,
    binding: &BridgeBinding,
    action: PersonCall,
) -> Result<String, String> {
    match f
        .hub
        .handle(binding, BridgeRequest::Person { action })
        .await
    {
        BridgeResponse::PersonResult { text } => Ok(text),
        BridgeResponse::Refused { reason } => Err(reason),
        other => panic!("{other:?}"),
    }
}
fn show(path: &str, invite: Option<MediaInvite>) -> PersonCall {
    PersonCall::Show {
        path: path.into(),
        caption: Some("The home page".into()),
        invite,
    }
}
fn preview(url: &str) -> PersonCall {
    PersonCall::Preview {
        url: url.into(),
        label: None,
        note: Some("The new header".into()),
        start: None,
        questions: Vec::new(),
    }
}
fn shown(f: &Fixture, task: TaskId) -> Vec<(Option<RunId>, Shown)> {
    let items: Vec<TimelineItem> = f.hub.read(|r| r.list(&format!("timeline:{task}"))).unwrap();
    (items.into_iter())
        .filter_map(|item| match item.item {
            TimelineBody::Shown { shown } => Some((item.run_id, shown)),
            _ => None,
        })
        .collect()
}
fn previews(f: &Fixture) -> Vec<PreviewEndpoint> {
    f.hub.read(|r| r.list("preview")).unwrap()
}

#[tokio::test]
async fn an_agent_shows_the_person_a_picture_from_its_project() {
    let f = Fixture::new().await;
    let (task, run, binding) = bound(&f).await;
    std::fs::create_dir_all(f.project_dir().join("shots")).unwrap();
    std::fs::write(f.project_dir().join("shots/home.png"), PNG).unwrap();
    let said = person(
        &f,
        &binding,
        show("shots/home.png", Some(MediaInvite::Draw)),
    )
    .await
    .unwrap();
    assert!(
        said.starts_with("Shown to the person in Chat: shots/home.png (image/png)."),
        "{said}"
    );
    assert!(said.contains("draw on it"), "{said}");
    // Given as an absolute path inside the project, or through a link to
    // its folder, it is the same file.
    let folder = f.project_dir().canonicalize().unwrap();
    let absolute = folder.join("./shots/home.png");
    person(&f, &binding, show(absolute.to_str().unwrap(), None))
        .await
        .unwrap();
    #[cfg(unix)]
    {
        let alias = f.path.join("alias");
        std::os::unix::fs::symlink(&folder, &alias).unwrap();
        let through = alias.join("shots/home.png");
        person(&f, &binding, show(through.to_str().unwrap(), None))
            .await
            .unwrap();
    }

    let items = shown(&f, task);
    assert_eq!(items.len(), if cfg!(unix) { 3 } else { 2 });
    let (
        item_run,
        Shown::Media {
            artifact_id,
            media_type,
            path,
            caption,
            invite,
        },
    ) = &items[0]
    else {
        panic!("{items:?}")
    };
    assert_eq!(*item_run, Some(run));
    assert_eq!(
        (media_type.as_str(), path.as_str()),
        ("image/png", "shots/home.png")
    );
    assert_eq!(caption.as_deref(), Some("The home page"));
    assert_eq!(*invite, Some(MediaInvite::Draw));
    let meta: ArtifactMeta = f.hub.read(|r| r.require("artifact", *artifact_id)).unwrap();
    assert_eq!(meta.sha256, hash(PNG));
    assert_eq!(meta.label.as_deref(), Some("home.png"));
    assert_eq!(meta.task_id, Some(task));
    assert_eq!(meta.links[0].run_id, Some(run));
    assert_eq!(meta.links[0].role, ArtifactRole::Shown);
    let creator: Option<Principal> = f
        .hub
        .read(|r| r.get("artifact_creator", *artifact_id))
        .unwrap();
    assert_eq!(creator, Some(Principal::System));
    assert!(
        matches!(&items[1].1, Shown::Media { path, invite: None, .. } if path == "shots/home.png")
    );
    f.close().await;
}

#[cfg(unix)]
#[tokio::test]
async fn only_a_picture_or_video_inside_the_project_is_shown() {
    let f = Fixture::new().await;
    let (task, _, binding) = bound(&f).await;
    let project = f.project_dir();
    let outside = f.path.join("outside.png");
    std::fs::write(&outside, PNG).unwrap();
    std::os::unix::fs::symlink(&outside, project.join("link.png")).unwrap();
    std::fs::hard_link(&outside, project.join("linked.png")).unwrap();
    std::fs::create_dir(project.join("folder.png")).unwrap();
    std::fs::write(project.join("notes.png"), b"not a picture").unwrap();
    std::fs::write(project.join("still.png"), PNG).unwrap();
    let mut big = PNG.to_vec();
    big.resize(crate::shown::MAX_SHOWN as usize + 1, 0);
    std::fs::write(project.join("big.png"), big).unwrap();
    for (path, invite, refusal) in [
        ("../outside.png", None, "inside this project"),
        (outside.to_str().unwrap(), None, "inside this project"),
        ("link.png", None, ""),
        ("linked.png", None, "hard-linked"),
        ("folder.png", None, "regular file"),
        ("missing.png", None, ""),
        ("notes.png", None, "Only a PNG"),
        ("big.png", None, "larger than 20 MiB"),
        ("still.png", Some(MediaInvite::Talk), "is for a video"),
    ] {
        let refused = person(&f, &binding, show(path, invite)).await.unwrap_err();
        assert!(refused.contains(refusal), "{path}: {refused}");
    }
    assert!(shown(&f, task).is_empty());
    f.close().await;
}

/// A project folder replaced by a link to another folder isn't the project.
#[cfg(unix)]
#[tokio::test]
async fn nothing_is_shown_from_a_project_folder_replaced_by_a_link() {
    let f = Fixture::new().await;
    let (task, _, binding) = bound(&f).await;
    let private = f.path.with_extension("private");
    std::fs::create_dir(&private).unwrap();
    std::fs::write(private.join("photo.png"), PNG).unwrap();
    let moved = f.project_dir().with_extension("moved");
    std::fs::rename(f.project_dir(), &moved).unwrap();
    std::os::unix::fs::symlink(&private, f.project_dir()).unwrap();
    person(&f, &binding, show("photo.png", None))
        .await
        .unwrap_err();
    assert!(shown(&f, task).is_empty());
    std::fs::remove_file(f.project_dir()).unwrap();
    std::fs::rename(&moved, f.project_dir()).unwrap();
    std::fs::remove_dir_all(private).unwrap();
    f.close().await;
}

#[tokio::test]
async fn an_agent_shows_a_page_it_serves_on_this_computer_once() {
    let f = Fixture::new().await;
    let (task, _, binding) = bound(&f).await;
    let said = person(&f, &binding, preview("http://127.0.0.1:5173/"))
        .await
        .unwrap();
    assert!(said.contains("“Preview”"), "{said}");
    // The same page again is the same preview.
    person(&f, &binding, preview("http://127.0.0.1:5173"))
        .await
        .unwrap();
    let kept = previews(&f);
    assert_eq!(kept.len(), 1);
    assert_eq!(
        (kept[0].label.as_str(), kept[0].url.as_str()),
        ("Preview", "http://127.0.0.1:5173/")
    );
    let items = shown(&f, task);
    assert_eq!(items.len(), 2);
    for (_, item) in &items {
        assert_eq!(
            *item,
            Shown::Preview {
                preview_id: kept[0].preview_id,
                label: "Preview".into(),
                url: "http://127.0.0.1:5173/".into(),
                note: Some("The new header".into()),
                questions: Vec::new(),
            }
        );
    }
    // What it asks about the page is asked beside it, and the agent is told
    // the answers come later.
    let asking = PersonCall::Preview {
        url: "http://127.0.0.1:5173/".into(),
        label: None,
        note: None,
        start: None,
        questions: vec![protocol::model::ApprovalQuestion {
            id: None,
            header: Some("Layout".into()),
            question: "Which header reads better?".into(),
            options: vec![protocol::model::ApprovalQuestionOption {
                label: "The second".into(),
                description: None,
            }],
            multi_select: false,
            secret: false,
        }],
    };
    let said = person(&f, &binding, asking).await.unwrap();
    assert!(said.contains("next message"), "{said}");
    let Some((_, Shown::Preview { questions, .. })) = shown(&f, task).pop() else {
        panic!("shown")
    };
    assert_eq!(questions[0].question, "Which header reads better?");
    // The person's own previews may be anywhere on their network; an
    // agent's is on this computer.
    for url in [
        "http://192.168.1.5:3000/",
        "https://example.com/",
        "file:///etc/passwd",
    ] {
        assert!(person(&f, &binding, preview(url)).await.is_err(), "{url}");
    }
    person(&f, &binding, preview("http://localhost:8080/"))
        .await
        .unwrap();
    // IPv6 on the same port may be another server, so another page.
    person(&f, &binding, preview("http://[::1]:5173/"))
        .await
        .unwrap();
    let kept = previews(&f);
    assert_eq!(kept.len(), 3);
    assert!(kept.iter().any(|p| p.url == "http://[::1]:5173/"));

    // A project keeps a bounded number.
    let project = kept[0].project_id;
    f.hub
        .write(|tx| {
            for port in 0..29 {
                let p = PreviewEndpoint {
                    preview_id: PreviewId::new(),
                    project_id: project,
                    label: "Kept".into(),
                    url: format!("http://127.0.0.1:{}/", 9000 + port),
                    reachable: None,
                    checked_at: None,
                    automatic: false,
                    start: None,
                };
                tx.put("preview", p.preview_id, &p)?;
            }
            Ok(())
        })
        .unwrap();
    let refused = person(&f, &binding, preview("http://127.0.0.1:4000/"))
        .await
        .unwrap_err();
    assert!(refused.contains("as many previews"), "{refused}");
    person(&f, &binding, preview("http://127.0.0.1:9001/"))
        .await
        .unwrap();
    assert_eq!(previews(&f).len(), 32);
    f.close().await;
}

#[tokio::test]
async fn an_address_too_long_or_escaped_too_many_times_over_is_refused_unread() {
    use crate::shown::MAX_ADDRESS;
    let f = Fixture::new().await;
    let (_, _, binding) = bound(&f).await;
    // Read three times over it is plain; four times, it is still escaped.
    person(&f, &binding, preview("http://127.0.0.1:5173/?q=%252541"))
        .await
        .unwrap();
    let refused = person(&f, &binding, preview("http://127.0.0.1:5173/?q=%25252541"))
        .await
        .unwrap_err();
    assert!(refused.contains("too many times over"), "{refused}");
    // Each reading of these escapes takes two letters off a long address.
    let long = format!("http://127.0.0.1:5173/?x=%{}", "25".repeat(MAX_ADDRESS / 2));
    let refused = person(&f, &binding, preview(&long)).await.unwrap_err();
    assert!(refused.contains("too long"), "{refused}");
    let longest = format!("http://127.0.0.1:5174/?x={}", "a".repeat(MAX_ADDRESS - 25));
    assert_eq!(longest.len(), MAX_ADDRESS);
    person(&f, &binding, preview(&longest)).await.unwrap();
    assert_eq!(previews(&f).len(), 2);
    f.close().await;
}

#[tokio::test]
async fn an_invitation_waits_for_nothing() {
    let f = Fixture::new().await;
    let (task, _, binding) = bound(&f).await;
    let said = person(
        &f,
        &binding,
        PersonCall::Invite {
            action: PersonAction::Capture,
            note: "Capture the error dialog".into(),
            goal: None,
        },
    )
    .await
    .unwrap();
    assert!(said.contains("Nothing waits for it"), "{said}");
    person(
        &f,
        &binding,
        PersonCall::Invite {
            action: PersonAction::ComputerUse,
            note: "Try the app as a user would".into(),
            goal: Some("Sign up in the app".into()),
        },
    )
    .await
    .unwrap();
    assert_eq!(
        shown(&f, task),
        [
            (
                Some(binding.run_id),
                Shown::Invitation {
                    action: PersonAction::Capture,
                    note: "Capture the error dialog".into(),
                    goal: None,
                }
            ),
            (
                Some(binding.run_id),
                Shown::Invitation {
                    action: PersonAction::ComputerUse,
                    note: "Try the app as a user would".into(),
                    goal: Some("Sign up in the app".into()),
                }
            )
        ]
    );
    f.close().await;
}

#[tokio::test]
async fn a_stopped_run_or_the_computer_specialist_shows_nothing() {
    let f = Fixture::new().await;
    let (task, run, binding) = bound(&f).await;
    std::fs::write(f.project_dir().join("still.png"), PNG).unwrap();
    f.hub.write(|tx| tx.put("stopped", run, &true)).unwrap();
    for call in [show("still.png", None), preview("http://127.0.0.1:5173/")] {
        let refused = person(&f, &binding, call).await.unwrap_err();
        assert!(refused.contains("no longer active"), "{refused}");
    }
    assert!(shown(&f, task).is_empty());
    assert!(previews(&f).is_empty());
    f.close().await;

    let f = Fixture::new().await;
    let c = f.device().await;
    let project = f.project().await;
    let (task, _) = f.task(project, TaskPlacement::StartNow, &c, true).await;
    let run = run_started(&f, task).await;
    let target = f.desktop.target.lock().unwrap().target_id;
    let binding = BridgeBinding {
        run_id: run,
        target_id: Some(target),
        computer_use: true,
        browser: false,
        github: false,
        checklist_answers: false,
    };
    let refused = person(
        &f,
        &binding,
        PersonCall::Invite {
            action: PersonAction::WatchDesktop,
            note: "Watch".into(),
            goal: None,
        },
    )
    .await
    .unwrap_err();
    assert!(refused.contains("computer specialist"), "{refused}");
    assert!(shown(&f, task).is_empty());
    f.close().await;
}

#[test]
fn what_a_file_is_comes_from_its_bytes() {
    use crate::shown::sniff;
    let mut webm = b"\x1a\x45\xdf\xa3\x9f\x42\x86\x81\x01\x42\x82\x84webm".to_vec();
    assert_eq!(sniff(&webm), Some("video/webm"));
    webm.truncate(12);
    webm.extend(b"matroska");
    assert_eq!(sniff(&webm), None);
    for (bytes, kind) in [
        (PNG, Some("image/png")),
        (&b"\xff\xd8\xff\xe0"[..], Some("image/jpeg")),
        (b"GIF89a..", Some("image/gif")),
        (b"RIFF\0\0\0\0WEBPVP8 ", Some("image/webp")),
        (b"\0\0\0\x18ftypisom", Some("video/mp4")),
        (b"\0\0\0\x14ftypqt  ", Some("video/quicktime")),
        (b"\0\0\0\x18ftypheic", None),
        (b"\0\0\0\x18ftypavif", None),
        (b"\0\0\0\x18ftyp", None),
        (b"<svg xmlns=", None),
        (b"", None),
    ] {
        assert_eq!(sniff(bytes), kind, "{bytes:?}");
    }
}

async fn delete(f: &Fixture, caller: &Caller, artifact_id: ArtifactId) -> CommandReceipt {
    let command = CommandEnvelope::new(Command::DeleteArtifact { artifact_id });
    let id = command.command_id;
    let receipt = f.hub.submit(caller, command).await;
    if receipt.status == ReceiptStatus::Accepted {
        settle(&f.hub, id).await;
    }
    receipt
}

async fn uploaded_picture(f: &Fixture) -> ArtifactMeta {
    let owner = Caller::local_owner();
    let receipt = f
        .hub
        .submit(
            &owner,
            CommandEnvelope::new(Command::CreateUpload {
                evidence: None,
                name: "picture.png".into(),
                size: PNG.len() as u64,
                sha256: hash(PNG),
                destination: TransferDestination::Artifact,
            }),
        )
        .await;
    let Some(CommandResult::Transfer { transfer }) = receipt.result else {
        panic!("{receipt:?}")
    };
    let mut sink = f.hub.upload(&owner, transfer.transfer_id).await.unwrap();
    sink.write(PNG).await.unwrap();
    let id = sink.finish().await.unwrap().artifact_id.unwrap();
    f.hub.read(|r| r.require("artifact", id)).unwrap()
}

#[tokio::test]
async fn deleting_a_picture_forgets_it_and_its_bytes_once_nothing_else_has_them() {
    let f = Fixture::new().await;
    let owner = Caller::local_owner();
    let first = uploaded_picture(&f).await;
    let second = (f.hub)
        .put_artifact(PNG, "image/png", None, Principal::LocalOwner)
        .await
        .unwrap();
    let blob = f.hub.artifact_path(&first.sha256).unwrap();
    assert_eq!(
        delete(&f, &owner, first.artifact_id).await.status,
        ReceiptStatus::Accepted
    );
    let gone: Option<ArtifactMeta> = f
        .hub
        .read(|r| r.get("artifact", first.artifact_id))
        .unwrap();
    assert!(gone.is_none());
    let creator: Option<Principal> = f
        .hub
        .read(|r| r.get("artifact_creator", first.artifact_id))
        .unwrap();
    assert!(creator.is_none());
    // The other artifact with the same bytes keeps them.
    assert!(blob.exists());
    assert_eq!(
        delete(&f, &owner, second.artifact_id).await.status,
        ReceiptStatus::Accepted
    );
    assert!(!blob.exists());
    // Deleted already, it is not found.
    let again = delete(&f, &owner, second.artifact_id).await;
    assert_eq!(again.error.unwrap().code, ErrorCode::NotFound);
    f.close().await;
}

#[tokio::test]
async fn ordinary_collection_removes_bytes_stranded_by_a_completed_upload() {
    let f = Fixture::new().await;
    let meta = uploaded_picture(&f).await;
    let blob = f.hub.artifact_path(&meta.sha256).unwrap();
    // Reproduce the on-disk state left by the old deletion path: metadata
    // gone, with both the blob and the completed transfer still present.
    f.hub
        .write(|tx| CompanionHub::delete_artifact(tx, &Caller::local_owner(), meta.artifact_id))
        .unwrap();
    assert!(blob.exists());
    f.clock.advance(Duration::from_secs(2 * 24 * 60 * 60));
    f.hub.collect_transfers().await.unwrap();
    assert!(!blob.exists());
    let transfers = f
        .hub
        .read(|r| r.list::<TransferRecord>("transfer"))
        .unwrap();
    assert_eq!(transfers.len(), 1);
    assert_eq!(transfers[0].state, TransferState::Completed);
    f.close().await;
}

#[tokio::test]
async fn a_deleted_upload_stays_readable_until_its_download_expires() {
    let f = Fixture::new().await;
    let owner = Caller::local_owner();
    let meta = uploaded_picture(&f).await;
    let blob = f.hub.artifact_path(&meta.sha256).unwrap();
    let receipt = f
        .hub
        .submit(
            &owner,
            CommandEnvelope::new(Command::CreateDownload {
                source: DownloadSource::Artifact {
                    artifact_id: meta.artifact_id,
                },
                shown_in_task: None,
            }),
        )
        .await;
    let Some(CommandResult::Transfer { transfer }) = receipt.result else {
        panic!("{receipt:?}")
    };
    assert_eq!(
        delete(&f, &owner, meta.artifact_id).await.status,
        ReceiptStatus::Accepted
    );
    assert!(blob.exists());
    let mut source = f
        .hub
        .download(&owner, transfer.transfer_id, 0)
        .await
        .unwrap();
    let mut bytes = Vec::new();
    tokio::io::AsyncReadExt::read_to_end(&mut source.reader, &mut bytes)
        .await
        .unwrap();
    assert_eq!(bytes, PNG);
    drop(source);
    f.hub.collect_transfers().await.unwrap();
    assert!(blob.exists(), "a completed download may still be retried");
    f.clock.advance(Duration::from_secs(24 * 60 * 60));
    f.hub.collect_transfers().await.unwrap();
    assert!(!blob.exists());
    f.close().await;
}

#[tokio::test]
async fn collection_waits_for_publication_to_record_the_artifact() {
    use std::{future::Future, task::Poll};

    let f = Fixture::new().await;
    let meta = uploaded_picture(&f).await;
    let blob = f.hub.artifact_path(&meta.sha256).unwrap();
    // Hold the same guard as upload publication and expose its critical
    // interval: the blob exists, but artifact metadata is not committed yet.
    let publishing = f.hub.inner.blobs.read().await;
    f.hub
        .write(|tx| tx.delete("artifact", meta.artifact_id))
        .unwrap();
    let mut collecting = Box::pin(f.hub.collect_blobs());
    std::future::poll_fn(|cx| {
        assert!(collecting.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    assert!(blob.exists());
    f.hub
        .write(|tx| tx.put("artifact", meta.artifact_id, &meta))
        .unwrap();
    drop(publishing);
    collecting.await.unwrap();
    assert!(blob.exists());
    assert_eq!(
        delete(&f, &Caller::local_owner(), meta.artifact_id)
            .await
            .status,
        ReceiptStatus::Accepted
    );
    assert!(!blob.exists());
    f.close().await;
}

#[tokio::test]
async fn a_device_deletes_only_what_it_sees_and_nothing_that_was_sent() {
    let f = Fixture::new().await;
    let other = super::artifact_access::device_with(
        &f,
        7,
        Grants {
            transfers: true,
            ..Grants::default()
        },
    )
    .await;
    let owners = (f.hub)
        .put_artifact(
            b"\xff\xd8\xffowner",
            "image/jpeg",
            None,
            Principal::LocalOwner,
        )
        .await
        .unwrap();
    let refused = delete(&f, &other, owners.artifact_id).await;
    assert_eq!(refused.error.unwrap().code, ErrorCode::PermissionDenied);
    let without = super::artifact_access::device_with(&f, 8, Grants::default()).await;
    let own = (f.hub)
        .put_artifact(PNG, "image/png", None, without.principal)
        .await
        .unwrap();
    assert_eq!(
        delete(&f, &without, own.artifact_id).await.status,
        ReceiptStatus::Rejected
    );

    // A picture that went with a message stays with it.
    let (task, _, _) = bound(&f).await;
    let sent = (f.hub)
        .put_artifact_with_links(
            PNG,
            "image/png",
            Some(ArtifactLink {
                task_id: task,
                run_id: None,
                annotation: None,
                role: ArtifactRole::Source,
            }),
            other.principal,
            None,
        )
        .await
        .unwrap();
    let refused = delete(&f, &other, sent.artifact_id).await;
    assert_eq!(refused.error.unwrap().code, ErrorCode::Conflict);
    let kept: Option<ArtifactMeta> = f.hub.read(|r| r.get("artifact", sent.artifact_id)).unwrap();
    assert!(kept.is_some());
    f.close().await;
}

#[tokio::test]
async fn a_device_cant_say_the_agent_showed_what_it_sends_or_captures() {
    let f = Fixture::new().await;
    let (task, run, _binding) = bound(&f).await;
    let owner = Caller::local_owner();
    let link = |role| ArtifactLink {
        task_id: task,
        run_id: Some(run),
        annotation: None,
        role,
    };
    let target_id = f.desktop.target.lock().unwrap().target_id;
    let capture = |role| {
        let command = Command::CaptureTarget {
            target_id,
            evidence: Some(link(role)),
        };
        let (hub, owner) = (&f.hub, &owner);
        async move { hub.control_command(owner, &command).await }
    };
    let upload = |role| {
        f.hub.submit(
            &owner,
            CommandEnvelope::new(Command::CreateUpload {
                name: "shot.png".into(),
                size: PNG.len() as u64,
                sha256: hash(PNG),
                destination: TransferDestination::Artifact,
                evidence: Some(link(role)),
            }),
        )
    };

    let refused = capture(ArtifactRole::Shown).await.unwrap_err();
    assert_eq!(refused.code, ErrorCode::InvalidRequest);
    let refused = upload(ArtifactRole::Shown).await;
    assert_eq!(refused.status, ReceiptStatus::Rejected, "{refused:?}");
    assert_eq!(refused.error.unwrap().code, ErrorCode::InvalidRequest);

    // As what they are, they are kept with the task as the device said.
    let Some(CommandResult::ArtifactCaptured { artifact }) =
        capture(ArtifactRole::Observation).await.unwrap()
    else {
        panic!("capture")
    };
    assert_eq!(artifact.links, vec![link(ArtifactRole::Observation)]);
    let receipt = upload(ArtifactRole::Observation).await;
    let Some(CommandResult::Transfer { transfer }) = receipt.result else {
        panic!("{receipt:?}")
    };
    let mut sending = f.hub.upload(&owner, transfer.transfer_id).await.unwrap();
    sending.write(PNG).await.unwrap();
    let sent = sending.finish().await.unwrap().artifact_id.unwrap();
    let meta: ArtifactMeta = f.hub.read(|r| r.require("artifact", sent)).unwrap();
    assert_eq!(meta.links, vec![link(ArtifactRole::Observation)]);
    f.close().await;
}

#[tokio::test]
async fn a_picture_an_agent_showed_can_be_drawn_on_and_sent_back() {
    let f = Fixture::new().await;
    let (task, _, binding) = bound(&f).await;
    std::fs::write(f.project_dir().join("home.png"), PNG).unwrap();
    person(&f, &binding, show("home.png", Some(MediaInvite::Draw)))
        .await
        .unwrap();
    let Some((_, Shown::Media { artifact_id, .. })) = shown(&f, task).pop() else {
        panic!("nothing shown")
    };
    let meta: ArtifactMeta = f.hub.read(|r| r.require("artifact", artifact_id)).unwrap();
    let bundle = |origin: SourceOrigin| AnnotationBundle {
        bundle_id: AnnotationBundleId::new(),
        revision: 1,
        sources: vec![AnnotationSource {
            key: "S1".into(),
            artifact_id,
            sha256: meta.sha256.clone(),
            media_type: meta.media_type.clone(),
            width: 1,
            height: 1,
            origin,
            captured_at: protocol::time::Timestamp(123),
            derived_from: None,
            derivation: None,
        }],
        marks: vec![],
        comments: vec![],
        steps: vec![],
        general_note: Some("This header".into()),
        speech: None,
    };
    let reader = super::artifact_access::device_with(
        &f,
        9,
        Grants {
            tasks: true,
            ..Grants::default()
        },
    )
    .await;
    f.hub
        .validate_bundle_for(&reader, &bundle(SourceOrigin::Shown { artifact_id }))
        .unwrap();
    // Only as what was shown: under another origin it is someone else's file.
    let code = |caller: &Caller, origin| {
        f.hub
            .validate_bundle_for(caller, &bundle(origin))
            .unwrap_err()
            .code
    };
    let imported = SourceOrigin::PhoneImport {
        original_name: None,
    };
    assert_eq!(code(&reader, imported), ErrorCode::PermissionDenied);
    // A device that can't read tasks can't send it either.
    let blind = super::artifact_access::device_with(&f, 10, Grants::default()).await;
    assert_eq!(
        code(&blind, SourceOrigin::Shown { artifact_id }),
        ErrorCode::PermissionDenied
    );
    // Nor is a file another device made a shown one by being called so.
    let theirs = (f.hub)
        .put_artifact(PNG, "image/png", Some(task), Principal::LocalOwner)
        .await
        .unwrap();
    let mut other = bundle(SourceOrigin::Shown {
        artifact_id: theirs.artifact_id,
    });
    other.sources[0].artifact_id = theirs.artifact_id;
    assert_eq!(
        f.hub.validate_bundle_for(&reader, &other).unwrap_err().code,
        ErrorCode::PermissionDenied
    );
    f.close().await;
}

/// A page whose server answers once something was typed into a terminal,
/// unless it has been stopped since.
#[derive(Debug)]
struct Started {
    terminal: Arc<FakeTerminal>,
    stopped: std::sync::atomic::AtomicBool,
}
#[async_trait::async_trait]
impl crate::preview::PreviewChecker for Started {
    async fn check(&self, _: &url::Url) -> Result<bool, ProtocolError> {
        let typed = (self.terminal.list().await.iter())
            .any(|t| !self.terminal.input(t.terminal_id).is_empty());
        Ok(typed && !self.stopped.load(std::sync::atomic::Ordering::SeqCst))
    }
}

#[tokio::test]
async fn a_previews_server_stopped_with_its_run_is_started_again_with_its_command() {
    let terminal = Arc::new(FakeTerminal::default());
    let page = Arc::new(Started {
        terminal: terminal.clone(),
        stopped: false.into(),
    });
    let f = Fixture::with(|b| b.terminal(terminal.clone()).preview_checker(page.clone())).await;
    let (_, _, binding) = bound(&f).await;
    let start = "npx vite --port 5179";
    let said = person(
        &f,
        &binding,
        PersonCall::Preview {
            url: "http://127.0.0.1:5179/".into(),
            label: Some("CRT".into()),
            note: None,
            start: Some(format!(" {start} ")),
            questions: Vec::new(),
        },
    )
    .await
    .unwrap();
    assert!(said.contains(start), "{said}");
    let [p] = &previews(&f)[..] else { panic!() };
    assert_eq!(p.start.as_deref(), Some(start));
    // Shown without one, a preview keeps the command it was given.
    person(&f, &binding, preview("http://127.0.0.1:5179/"))
        .await
        .unwrap();
    assert_eq!(previews(&f)[0].start.as_deref(), Some(start));
    let fixture = &f;
    let check = |start| async move {
        let command = CommandEnvelope::new(Command::CheckPreview {
            preview_id: previews(fixture)[0].preview_id,
            start,
        });
        let id = command.command_id;
        fixture.hub.submit(&Caller::local_owner(), command).await;
        settle(&fixture.hub, id).await;
        fixture.hub.read(|r| r.command(id)).unwrap().unwrap()
    };
    // Only checked, nothing answers and nothing is started.
    assert!(check(false).await.error.is_none());
    assert!(terminal.list().await.is_empty());
    assert_eq!(previews(&f)[0].reachable, Some(false));
    // Started, in a terminal in the project's folder, the person's to close.
    assert!(check(true).await.error.is_none());
    let [t] = &terminal.list().await[..] else {
        panic!()
    };
    assert_eq!(t.title, "Preview: CRT");
    assert_eq!(
        std::fs::canonicalize(&t.cwd).unwrap(),
        std::fs::canonicalize(f.project_dir()).unwrap()
    );
    assert_eq!(
        terminal.input(t.terminal_id),
        format!("{start}\r").as_bytes()
    );
    assert_eq!(previews(&f)[0].reachable, Some(true));
    let owner: Option<Principal> = (f.hub)
        .read(|r| r.get("terminal_owner", t.terminal_id))
        .unwrap();
    assert_eq!(owner, Some(Principal::LocalOwner));
    // While that terminal is open the server is waited for, not started twice.
    page.stopped
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let page_again = page.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(1)).await;
        page_again
            .stopped
            .store(false, std::sync::atomic::Ordering::SeqCst);
    });
    assert!(check(true).await.error.is_none());
    assert_eq!(terminal.list().await.len(), 1);
    assert_eq!(
        terminal.input(t.terminal_id),
        format!("{start}\r").as_bytes()
    );
    // Once it is closed, the server is started anew.
    terminal.close(t.terminal_id).await.unwrap();
    page.stopped
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let page_again = page.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(1)).await;
        page_again
            .stopped
            .store(false, std::sync::atomic::Ordering::SeqCst);
    });
    assert!(check(true).await.error.is_none());
    let running: Vec<_> = (terminal.list().await.into_iter())
        .filter(|t| t.state == protocol::terminal::TerminalState::Running)
        .collect();
    assert_eq!(running.len(), 1);
    assert_ne!(running[0].terminal_id, t.terminal_id);
    f.close().await;
}

#[tokio::test]
async fn a_previews_start_command_is_one_plain_line_without_the_runs_secrets() {
    let f = Fixture::new().await;
    let (_, _, binding) = bound(&f).await;
    for start in [
        "npm i\nnpx vite".to_owned(),
        "npx vite\u{7}".into(),
        "npx vite #\u{202E}etis".into(),
        "x".repeat(1001),
    ] {
        let refused = person(
            &f,
            &binding,
            PersonCall::Preview {
                url: "http://127.0.0.1:5179/".into(),
                label: None,
                note: None,
                start: Some(start.clone()),
                questions: Vec::new(),
            },
        )
        .await
        .unwrap_err();
        assert!(
            refused.contains("one plain command line"),
            "{start:?}: {refused}"
        );
    }
    assert!(previews(&f).is_empty());
    f.close().await;
}

#[tokio::test]
async fn a_preview_without_a_start_command_is_not_started() {
    let terminal = Arc::new(FakeTerminal::default());
    let page = Arc::new(Started {
        terminal: terminal.clone(),
        stopped: false.into(),
    });
    let f = Fixture::with(|b| b.terminal(terminal.clone()).preview_checker(page)).await;
    let (_, _, binding) = bound(&f).await;
    let said = person(&f, &binding, preview("http://127.0.0.1:5179/"))
        .await
        .unwrap();
    // The agent is told how to have it started again.
    assert!(said.contains("show it again with start"), "{said}");
    let command = CommandEnvelope::new(Command::CheckPreview {
        preview_id: previews(&f)[0].preview_id,
        start: true,
    });
    let id = command.command_id;
    f.hub.submit(&Caller::local_owner(), command).await;
    settle(&f.hub, id).await;
    let record = f.hub.read(|r| r.command(id)).unwrap().unwrap();
    assert_eq!(record.error.unwrap().code, ErrorCode::InvalidRequest);
    assert!(terminal.list().await.is_empty());
    assert_eq!(previews(&f)[0].reachable, Some(false));
    f.close().await;
}
