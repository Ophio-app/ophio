// SPDX-License-Identifier: Apache-2.0
use super::artifact_access::device_with;
use super::*;

fn upload(size: u64) -> CommandEnvelope {
    upload_to(TransferDestination::Artifact, size)
}

fn upload_to(destination: TransferDestination, size: u64) -> CommandEnvelope {
    CommandEnvelope::new(Command::CreateUpload {
        evidence: None,
        name: "video.mp4".into(),
        size,
        sha256: "0".repeat(64),
        destination,
    })
}

fn declared(receipt: CommandReceipt) -> TransferId {
    let Some(CommandResult::Transfer { transfer }) = receipt.result else {
        panic!("{receipt:?}")
    };
    transfer.transfer_id
}

fn refused(receipt: CommandReceipt) -> String {
    assert_eq!(receipt.status, ReceiptStatus::Rejected, "{receipt:?}");
    let error = receipt.error.unwrap();
    assert_eq!(error.code, ErrorCode::LimitExceeded);
    error.message
}

fn transfers() -> Grants {
    Grants {
        transfers: true,
        ..Grants::default()
    }
}

#[tokio::test]
async fn cancelled_download_preparation_removes_staged_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("project");
    std::fs::create_dir(&project).unwrap();
    let bytes = vec![7; 512 * 1024];
    std::fs::write(project.join("large.bin"), &bytes).unwrap();
    let hub = CompanionHub::builder()
        .data_dir(dir.path().join("data"))
        .build()
        .await
        .unwrap();
    let owner = Caller::local_owner();
    let added = hub
        .submit(
            &owner,
            CommandEnvelope::new(Command::AddProject {
                host_path: project.to_string_lossy().into(),
                name: None,
            }),
        )
        .await;
    let Some(CommandResult::ProjectAdded { project_id }) = added.result else {
        panic!("{added:?}")
    };
    // Hold publication after copying, so cancellation happens with real bytes
    // on disk and before a transfer record can own them.
    let publishing = hub.inner.blobs.clone().write_owned().await;
    let preparing = hub.clone();
    let task = tokio::spawn(async move {
        preparing
            .submit(
                &Caller::local_owner(),
                CommandEnvelope::new(Command::CreateDownload {
                    source: DownloadSource::ProjectFile {
                        project_id,
                        path: "large.bin".into(),
                    },
                    shown_in_task: None,
                }),
            )
            .await
    });
    let staging = dir.path().join("data/staging");
    let stage = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(mut entries) = tokio::fs::read_dir(&staging).await
                && let Some(entry) = entries.next_entry().await.unwrap()
                && entry.metadata().await.unwrap().len() == bytes.len() as u64
            {
                break entry.path();
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    let removed = tokio::time::timeout(Duration::from_secs(2), async {
        while stage.exists() {
            tokio::task::yield_now().await;
        }
    })
    .await;
    drop(publishing);
    let QueryData::Transfers { transfers } = hub.query(&owner, Query::Transfers).await.unwrap()
    else {
        unreachable!()
    };
    assert!(transfers.is_empty());
    hub.shutdown().await.unwrap();
    assert!(
        removed.is_ok(),
        "cancelled preparation left staged bytes behind"
    );
}

#[tokio::test]
async fn unfinished_uploads_are_capped_per_device_and_overall() {
    let f = Fixture::with(|b| b.max_upload_bytes(100)).await;
    let first = device_with(&f, 7, transfers()).await;
    let second = device_with(&f, 8, transfers()).await;
    let third = device_with(&f, 9, transfers()).await;
    let mut ids = Vec::new();
    for caller in [&first, &first, &second, &second] {
        ids.push(declared(f.hub.submit(caller, upload(100)).await));
    }
    let message = refused(f.hub.submit(&first, upload(1)).await);
    assert!(
        message.starts_with("This device already has 200 B of files"),
        "{message}"
    );
    let message = refused(f.hub.submit(&third, upload(1)).await);
    assert!(
        message.starts_with("Your devices already have 400 B of files"),
        "{message}"
    );
    f.hub.cancel_transfer(&first, ids[0]).await.unwrap();
    declared(f.hub.submit(&third, upload(100)).await);
    f.close().await;
}

#[tokio::test]
async fn a_declaration_needs_room_on_the_computer() {
    let f = Fixture::with(|b| b.max_upload_bytes(u64::MAX)).await;
    let message = refused(
        f.hub
            .submit(&Caller::local_owner(), upload(u64::MAX / 8))
            .await,
    );
    assert!(
        message.starts_with("The computer doesn’t have room for this file. Free up "),
        "{message}"
    );
    assert!(message.ends_with(" GB on the computer, then send it again."));
    f.close().await;
}

/// A file sent to a folder is copied there once it has arrived, so on the
/// data volume its declaration needs room for it twice.
#[cfg(unix)]
#[tokio::test]
async fn a_declaration_to_a_folder_on_the_data_volume_needs_room_for_its_copy() {
    let f = Fixture::with(|b| b.max_upload_bytes(u64::MAX)).await;
    let owner = Caller::local_owner();
    let free = crate::transfers::free_space(&f.path).unwrap();
    assert!(free > 2 * crate::transfers::FREE_SPACE_RESERVE, "{free}");
    // Room for the file and the reserve, with a third to spare, but not for a copy too.
    let size = (free - crate::transfers::FREE_SPACE_RESERVE) / 3 * 2;
    let to = |destination| {
        CommandEnvelope::new(Command::CreateUpload {
            evidence: None,
            name: "video.mp4".into(),
            size,
            sha256: "0".repeat(64),
            destination,
        })
    };
    let message = refused(f.hub.submit(&owner, to(TransferDestination::Inbox)).await);
    assert!(
        message.starts_with("The computer doesn’t have room for this file. Free up "),
        "{message}"
    );
    // Kept as an artifact, the same file is never copied.
    declared(
        f.hub
            .submit(&owner, to(TransferDestination::Artifact))
            .await,
    );
    f.close().await;
}

/// A copy still being written has not taken its room on the volume yet, so
/// a declaration counts what copies there have claimed.
#[cfg(unix)]
#[tokio::test]
async fn a_declaration_counts_the_room_copies_have_claimed() {
    let f = Fixture::with(|b| b.max_upload_bytes(u64::MAX)).await;
    let owner = Caller::local_owner();
    let free = crate::transfers::free_space(&f.path).unwrap();
    assert!(free > 2 * crate::transfers::FREE_SPACE_RESERVE, "{free}");
    // Room for one file and the reserve, with a third to spare, but not two.
    let size = (free - crate::transfers::FREE_SPACE_RESERVE) / 3 * 2;
    let claim = f.hub.copy_room(&f.path, size, TransferId::new()).unwrap();
    let message = refused(f.hub.submit(&owner, upload(size)).await);
    assert!(
        message.starts_with("The computer doesn’t have room for this file. Free up "),
        "{message}"
    );
    drop(claim);
    declared(f.hub.submit(&owner, upload(size)).await);
    f.close().await;
}

/// A file sent to a folder is copied there once it has arrived, so a later
/// declaration leaves room for that copy as well as for its own file, and
/// counts it once when the copy has claimed its room.
#[cfg(unix)]
#[tokio::test]
async fn a_declaration_leaves_room_for_the_copies_earlier_uploads_will_make() {
    let f = Fixture::with(|b| b.max_upload_bytes(u64::MAX)).await;
    let owner = Caller::local_owner();
    let reserve = crate::transfers::FREE_SPACE_RESERVE;
    let free = crate::transfers::free_space(&f.path).unwrap();
    assert!(free > 2 * reserve, "{free}");
    // Room for a file sent to the received-files folder, its copy and
    // another file with some to spare, but not for all three and a copy.
    let size = (free - reserve) / 5 * 2;
    let to_inbox = declared(
        f.hub
            .submit(&owner, upload_to(TransferDestination::Inbox, size))
            .await,
    );
    let message = refused(f.hub.submit(&owner, upload(size)).await);
    assert!(
        message.starts_with("The computer doesn’t have room for this file. Free up "),
        "{message}"
    );
    let claim = f.hub.copy_room(&f.path, size, to_inbox).unwrap();
    declared(f.hub.submit(&owner, upload(size / 4)).await);
    drop(claim);
    f.hub.cancel_transfer(&owner, to_inbox).await.unwrap();
    declared(f.hub.submit(&owner, upload(size)).await);
    f.close().await;
}

/// Two files finishing together are copied to their folders at once, so
/// each copy claims its room until it is written.
#[cfg(unix)]
#[tokio::test]
async fn copies_to_one_volume_each_claim_their_room() {
    let f = Fixture::new().await;
    let folder = f.path.clone();
    let free = crate::transfers::free_space(&folder).unwrap();
    assert!(free > 2 * crate::transfers::FREE_SPACE_RESERVE, "{free}");
    // Room for one copy and the reserve, with a third to spare, but not two.
    let size = (free - crate::transfers::FREE_SPACE_RESERVE) / 3 * 2;
    let first = f.hub.copy_room(&folder, size, TransferId::new()).unwrap();
    let Err(refused) = f.hub.copy_room(&folder, size, TransferId::new()) else {
        panic!("a second copy fitted in the first one's room");
    };
    assert!(
        (refused.message).starts_with("The computer doesn’t have room for this file."),
        "{}",
        refused.message
    );
    drop(first);
    drop(f.hub.copy_room(&folder, size, TransferId::new()).unwrap());
    f.close().await;
}

/// A copy reads what is arriving, checks its room and claims it in one step,
/// as a declaration does, so an upload declared at that moment is counted by
/// the copy or sees its claim: the two are never both let in when only one
/// fits.
#[cfg(unix)]
#[tokio::test]
async fn a_copy_and_an_upload_declared_while_it_checks_do_not_both_fit() {
    let f = Fixture::with(|b| b.max_upload_bytes(u64::MAX)).await;
    let folder = f.path.clone();
    let free = crate::transfers::free_space(&folder).unwrap();
    assert!(free > 2 * crate::transfers::FREE_SPACE_RESERVE, "{free}");
    // Room for one file and the reserve, with a third to spare, but not two.
    let size = (free - crate::transfers::FREE_SPACE_RESERVE) / 3 * 2;
    let (hub, folder) = (&f.hub, &folder);
    let declare = || {
        hub.write(|tx| {
            let sha = "0".repeat(64);
            let owner = Caller::local_owner().principal;
            hub.declare_upload(
                tx,
                "video.mp4",
                size,
                &sha,
                &TransferDestination::Artifact,
                owner,
            )
        })
    };
    // While the claims are held, the copy stops just before it claims.
    let claims = hub.inner.copying.lock().unwrap();
    std::thread::scope(|s| {
        let (claiming, at_claim) = std::sync::mpsc::channel();
        let copy = s.spawn(move || {
            crate::transfers::CLAIMING.set(Some(claiming));
            hub.copy_room(folder, size, TransferId::new())
        });
        at_claim.recv().unwrap();
        // It has read what is arriving. Until it has claimed, a declaration
        // cannot check its own room.
        assert!(
            matches!(
                hub.inner.db.try_lock(),
                Err(std::sync::TryLockError::WouldBlock)
            ),
            "an upload could be declared between the copy's check and its claim"
        );
        let declared = s.spawn(declare);
        drop(claims);
        let copied = copy.join().unwrap().unwrap();
        let refused = declared.join().unwrap().unwrap_err();
        assert_eq!(refused.code, ErrorCode::LimitExceeded, "{refused:?}");
        assert!(refused.message.contains("room"), "{refused:?}");
        // Refused for the room the copy holds, which it gives back once written.
        drop(copied);
    });
    declare().unwrap();
    f.close().await;
}

/// Uploads still arriving are written to the data volume, so a copy to a
/// folder on another volume needs room there only for itself.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_copy_to_another_volume_leaves_out_uploads_arriving_on_the_data_volume() {
    use std::os::unix::fs::MetadataExt;
    let f = Fixture::with(|b| b.max_upload_bytes(u64::MAX)).await;
    let other = std::path::Path::new("/dev/shm");
    let reserve = crate::transfers::FREE_SPACE_RESERVE;
    let data_free = crate::transfers::free_space(&f.path).unwrap();
    let other_free = crate::transfers::free_space(other).unwrap_or(0);
    let separate = std::fs::metadata(other).map(|m| m.dev()).ok()
        != Some(std::fs::metadata(&f.path).unwrap().dev());
    // Needs a second volume with room for the reserve but less than the data volume.
    if !separate || other_free <= 2 * reserve || other_free + 2 * reserve >= data_free {
        eprintln!("skipped: no second volume smaller than the data volume");
        f.close().await;
        return;
    }
    let folder = tempfile::tempdir_in(other).unwrap();
    // An upload the other volume could not hold, arriving on the data volume.
    declared(
        f.hub
            .submit(&Caller::local_owner(), upload(other_free))
            .await,
    );
    drop(
        f.hub
            .copy_room(folder.path(), 1, TransferId::new())
            .unwrap(),
    );
    f.close().await;
}

#[tokio::test]
async fn abandoned_transfers_expire_and_bytes_nothing_names_are_removed() {
    let f = Fixture::new().await;
    let owner = Caller::local_owner();
    let project_id = f.project().await;
    let folder = f.project_dir();
    let download = |path: &str| {
        CommandEnvelope::new(Command::CreateDownload {
            shown_in_task: None,
            source: DownloadSource::ProjectFile {
                project_id,
                path: path.into(),
            },
        })
    };
    let blob = |bytes: &[u8]| {
        let sha = hash(bytes);
        f.path.join("artifacts").join(&sha[..2]).join(sha)
    };
    let staging = f.path.join("staging");

    let idle_upload = declared(f.hub.submit(&owner, upload(6)).await);
    let mut sink = f.hub.upload(&owner, idle_upload).await.unwrap();
    sink.write(b"abc").await.unwrap();
    drop(sink);
    tokio::task::yield_now().await;
    std::fs::write(folder.join("waiting.txt"), b"never fetched").unwrap();
    let idle_download = declared(f.hub.submit(&owner, download("waiting.txt")).await);
    std::fs::write(folder.join("fetched.txt"), b"fetched").unwrap();
    let fetched = declared(f.hub.submit(&owner, download("fetched.txt")).await);
    let mut source = f.hub.download(&owner, fetched, 0).await.unwrap();
    tokio::io::copy(&mut source.reader, &mut tokio::io::sink())
        .await
        .unwrap();
    f.hub
        .put_artifact(b"kept", "image/png", None, owner.principal)
        .await
        .unwrap();
    // Bytes left by a copy Ophio never finished, and one still being written.
    std::fs::write(staging.join("left-over"), b"old").unwrap();
    std::fs::File::options()
        .write(true)
        .open(staging.join("left-over"))
        .unwrap()
        .set_modified(std::time::SystemTime::now() - Duration::from_secs(2 * 24 * 60 * 60))
        .unwrap();
    std::fs::write(staging.join("being-copied"), b"new").unwrap();

    f.clock.advance(Duration::from_secs(24 * 60 * 60));
    let active_upload = declared(f.hub.submit(&owner, upload(6)).await);
    let mut sink = f.hub.upload(&owner, active_upload).await.unwrap();
    sink.write(b"abc").await.unwrap();
    drop(sink);
    tokio::task::yield_now().await;

    f.hub.collect_transfers().await.unwrap();
    let state = |id: TransferId| {
        f.hub
            .read(|r| r.require::<TransferRecord>("transfer", id))
            .unwrap()
            .state
    };
    assert_eq!(state(idle_upload), TransferState::Cancelled);
    assert!(!staging.join(idle_upload.to_string()).exists());
    assert_eq!(state(idle_download), TransferState::Cancelled);
    assert!(!blob(b"never fetched").exists());
    assert_eq!(state(fetched), TransferState::Completed);
    assert!(!blob(b"fetched").exists());
    let Err(gone) = f.hub.download(&owner, fetched, 0).await else {
        panic!("a collected download")
    };
    assert_eq!(gone.code, ErrorCode::Expired);
    assert!(blob(b"kept").exists());
    assert_eq!(state(active_upload), TransferState::InProgress);
    assert!(staging.join(active_upload.to_string()).exists());
    assert!(!staging.join("left-over").exists());
    assert!(staging.join("being-copied").exists());
    drop(source);
    f.close().await;
}

#[tokio::test]
async fn cancelling_an_unknown_transfer_leaves_no_lock_behind() {
    let f = Fixture::new().await;
    let Err(missing) = f
        .hub
        .cancel_transfer(&Caller::local_owner(), TransferId::new())
        .await
    else {
        panic!("an unknown transfer")
    };
    assert_eq!(missing.code, ErrorCode::NotFound);
    assert!(f.hub.inner.transfer_locks.lock().unwrap().is_empty());
    f.close().await;
}

#[tokio::test]
async fn transfers_and_artifacts_reach_only_devices_that_may_fetch_them() {
    let f = Fixture::new().await;
    let asking = device_with(&f, 7, transfers()).await;
    let other = device_with(&f, 8, transfers()).await;
    let reviewer = device_with(
        &f,
        9,
        Grants {
            tasks: true,
            ..transfers()
        },
    )
    .await;
    let project_id = f.project().await;
    let (task, _) = f
        .task(
            project_id,
            TaskPlacement::Queue {
                position: None,
                authorization: QueueAuthorization::AskBeforeStart,
            },
            &Caller::local_owner(),
            false,
        )
        .await;
    let callers = [&asking, &other, &reviewer, &Caller::local_owner()];
    let mut subscriptions = Vec::new();
    for caller in callers {
        subscriptions.push(f.hub.subscribe(caller, None).await.unwrap());
    }
    let upload = declared(f.hub.submit(&asking, upload(6)).await);
    let observation = f
        .hub
        .put_artifact(b"observed", "image/png", Some(task), Principal::System)
        .await
        .unwrap();
    let dictation = f
        .hub
        .put_artifact(b"dictation", "audio/wav", None, asking.principal)
        .await
        .unwrap();
    let seen: Vec<(bool, Vec<ArtifactId>)> = subscriptions
        .iter_mut()
        .map(|s| {
            let (mut transfer, mut artifacts) = (false, Vec::new());
            while let Ok(Outbound::Event(e)) = s.live.try_recv() {
                match &e.event {
                    Event::TransferUpserted { transfer: t } => {
                        transfer |= t.transfer_id == upload;
                    }
                    Event::ArtifactCreated { artifact } => artifacts.push(artifact.artifact_id),
                    _ => {}
                }
            }
            (transfer, artifacts)
        })
        .collect();
    let (observed, dictated) = (observation.artifact_id, dictation.artifact_id);
    assert_eq!(seen[0], (true, vec![dictated]), "the device that asked");
    assert_eq!(seen[1], (false, vec![]), "another transfers device");
    assert_eq!(seen[2], (false, vec![observed]), "a tasks device");
    assert_eq!(seen[3], (true, vec![observed, dictated]), "a manager");

    let listed = |caller: &Caller| {
        let hub = f.hub.clone();
        let caller = caller.clone();
        async move {
            let Ok(QueryData::Transfers { transfers }) = hub.query(&caller, Query::Transfers).await
            else {
                panic!("transfers")
            };
            transfers.len()
        }
    };
    assert_eq!(listed(&asking).await, 1);
    assert_eq!(listed(&other).await, 0);
    let Err(hidden) = f
        .hub
        .query(
            &other,
            Query::Artifact {
                artifact_id: observed,
            },
        )
        .await
    else {
        panic!("another device's artifact")
    };
    assert_eq!(hidden.code, ErrorCode::NotFound);
    assert!(
        f.hub
            .query(
                &reviewer,
                Query::Artifact {
                    artifact_id: observed
                }
            )
            .await
            .is_ok()
    );
    f.close().await;
}

#[tokio::test]
async fn uploads_leave_read_only_library_folders_alone() {
    let library = Arc::new(FakeLibrary::default());
    let f = Fixture::with({
        let library = library.clone();
        move |b| b.library(library)
    })
    .await;
    let owner = Caller::local_owner();
    let project_id = f.project().await;
    let folder = f.project_dir();
    let records = folder.join("records");
    std::fs::create_dir_all(&records).unwrap();
    let inbox = f.hub.inbox_folder().unwrap();
    std::fs::create_dir_all(&inbox).unwrap();
    let read_only = |label: &str, path: &std::path::Path| ports::RootSpec {
        root_id: LibraryRootId::new(),
        label: label.into(),
        path: ports::library::canonical(path).unwrap(),
        kind: protocol::library::RootKind::Notes,
        writable: false,
        project_id: None,
        harness: None,
        only: None,
    };
    let roots = vec![
        read_only("Records", &records),
        read_only("Received", &inbox),
    ];
    let into = |destination: TransferDestination| {
        CommandEnvelope::new(Command::CreateUpload {
            evidence: None,
            name: "note.txt".into(),
            size: 4,
            sha256: hash(b"note"),
            destination,
        })
    };
    let in_records = TransferDestination::ProjectDir {
        project_id,
        dir: "records".into(),
    };
    f.hub.set_library_roots(roots.clone()).await.unwrap();
    for (destination, label) in [
        (in_records.clone(), "Records"),
        (TransferDestination::Inbox, "Received"),
    ] {
        let receipt = f.hub.submit(&owner, into(destination)).await;
        let error = receipt.error.unwrap();
        assert_eq!(error.code, ErrorCode::PathNotAllowed);
        assert_eq!(
            error.message,
            format!(
                "“{label}” is a read-only Library folder on the computer. Send the file to another folder."
            )
        );
    }
    declared(
        f.hub
            .submit(
                &owner,
                into(TransferDestination::ProjectDir {
                    project_id,
                    dir: String::new(),
                }),
            )
            .await,
    );

    // A folder shared read-only after the upload was declared.
    f.hub.set_library_roots(Vec::new()).await.unwrap();
    let id = declared(f.hub.submit(&owner, into(in_records)).await);
    let mut sink = f.hub.upload(&owner, id).await.unwrap();
    sink.write(b"note").await.unwrap();
    f.hub.set_library_roots(roots).await.unwrap();
    let Err(refused) = sink.finish().await else {
        panic!("an upload into a read-only folder")
    };
    assert_eq!(refused.code, ErrorCode::PathNotAllowed);
    assert!(!records.join("note.txt").exists());
    f.close().await;
    std::fs::remove_dir_all(inbox).unwrap();
}

#[tokio::test]
async fn transfer_names_follow_the_library_rules_and_have_no_direction_controls() {
    let f = Fixture::new().await;
    let owner = Caller::local_owner();
    let upload = |name: &str| {
        f.hub.submit(
            &owner,
            CommandEnvelope::new(Command::CreateUpload {
                evidence: None,
                name: name.into(),
                size: 4,
                sha256: hash(b"note"),
                destination: TransferDestination::Inbox,
            }),
        )
    };
    for name in [
        "CON",
        "nul.txt",
        "Com1.tar.gz",
        "LPT\u{b2}.md",
        "conin$",
        "photo\u{202E}gnp.exe",
        "report\u{2067}.pdf",
        "a\u{200F}b.txt",
    ] {
        let receipt = upload(name).await;
        assert_eq!(receipt.status, ReceiptStatus::Rejected, "{name:?}");
        assert_eq!(receipt.error.unwrap().code, ErrorCode::PathNotAllowed);
    }
    for name in ["console.txt", "COM10.md", "Büro notes.md", "نص.txt"] {
        declared(upload(name).await);
    }
    f.close().await;
}

#[tokio::test]
async fn a_download_is_named_without_direction_controls_and_keeps_its_emoji() {
    let f = Fixture::new().await;
    let owner = Caller::local_owner();
    let project_id = f.project().await;
    let folder = f.project_dir();
    let family = "\u{1F469}\u{200D}\u{1F469}\u{200D}\u{1F467} trip \u{2764}\u{FE0F}.txt";
    // Not a spoofed `.exe`: Microsoft Defender refuses to open one at all.
    for (file, shown) in [
        ("photo\u{202E}gnp.txt", "photognp.txt"),
        ("\u{2066}report\u{2069}\u{200F}.pdf", "report.pdf"),
        (family, family),
        ("\u{202E}\u{2067}", "File"),
    ] {
        std::fs::write(folder.join(file), b"bytes").unwrap();
        let receipt = f
            .hub
            .submit(
                &owner,
                CommandEnvelope::new(Command::CreateDownload {
                    shown_in_task: None,
                    source: DownloadSource::ProjectFile {
                        project_id,
                        path: file.into(),
                    },
                }),
            )
            .await;
        let Some(CommandResult::Transfer { transfer }) = receipt.result else {
            panic!("{file:?}: {receipt:?}")
        };
        assert_eq!(transfer.name, shown, "{file:?}");
    }
    f.close().await;
}

/// The reviewed copy goes to the trash and then another file takes the name
/// before the new one is published. That publication is refused, and the
/// transfer says it wasn't saved rather than asking for it to be checked.
#[tokio::test]
async fn a_publication_refused_after_it_was_recorded_is_reported_as_not_saved() {
    use ports::LibraryFs;
    let library = Arc::new(FakeLibrary::default());
    let f = Fixture::with({
        let library = library.clone();
        move |b| b.library(library)
    })
    .await;
    let owner = Caller::local_owner();
    let project_id = f.project().await;
    let project: protocol::model::Project =
        f.hub.read(|r| r.require("project", project_id)).unwrap();
    let root_id = LibraryRootId::new();
    f.hub
        .set_library_roots(vec![ports::RootSpec {
            root_id,
            label: "Test".into(),
            path: project.host_path.clone().into(),
            kind: protocol::library::RootKind::Project,
            writable: true,
            project_id: Some(project_id),
            harness: None,
            only: None,
        }])
        .await
        .unwrap();
    library.create(root_id, "notes.txt", "old").await.unwrap();
    // The fake Library's trash leaves the folder alone, so the name is
    // taken on disk when the upload publishes.
    let folder = f.project_dir();
    std::fs::write(folder.join("notes.txt"), "theirs").unwrap();
    let data = b"replacement";
    let id = declared(
        f.hub
            .submit(
                &owner,
                CommandEnvelope::new(Command::CreateUploadReviewed {
                    task_id: None,
                    name: "notes.txt".into(),
                    size: data.len() as u64,
                    sha256: hash(data),
                    destination: TransferDestination::ProjectDir {
                        project_id,
                        dir: String::new(),
                    },
                    collision: UploadCollision::Replace {
                        revision: hash(b"old"),
                    },
                }),
            )
            .await,
    );
    let mut sink = f.hub.upload(&owner, id).await.unwrap();
    sink.write(data).await.unwrap();
    let Err(refused) = sink.finish().await else {
        panic!("a publication onto a name another file took")
    };
    assert_eq!(refused.code, ErrorCode::Conflict);
    let transfer: TransferRecord = f.hub.read(|r| r.require("transfer", id)).unwrap();
    let TransferState::Failed { reason } = &transfer.state else {
        panic!("{:?}", transfer.state)
    };
    assert_eq!(
        reason,
        "Not saved: another file took this name while it was being saved. Send it again to replace it or keep both. The copy it was replacing is in Recently deleted."
    );
    assert_eq!(transfer.publication, None);
    assert_eq!(std::fs::read(folder.join("notes.txt")).unwrap(), b"theirs");
    f.close().await;
}

#[tokio::test]
async fn the_hourly_collection_also_purges_deleted_library_documents() {
    let library = Arc::new(FakeLibrary::default());
    let f = Fixture::with({
        let library = library.clone();
        move |b| b.library(library)
    })
    .await;
    // The collection runs once as Ophio starts, then every hour.
    tokio::time::timeout(WAIT, async {
        while library.purges() == 0 {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("deleted documents were never purged");
    f.close().await;
}
