// SPDX-License-Identifier: Apache-2.0
use super::*;

/// A device with the given grants, set by the owner.
pub(super) async fn device_with(f: &Fixture, key: u8, grants: Grants) -> Caller {
    let device = pair(&f.hub, key).await;
    let Principal::Device { device_id } = device.principal else {
        panic!("device")
    };
    let receipt = f
        .hub
        .submit(
            &Caller::local_owner(),
            CommandEnvelope::new(Command::SetDeviceGrants {
                device_id,
                expected: Some(device.grants.clone()),
                grants: grants.clone(),
            }),
        )
        .await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    Caller {
        principal: device.principal,
        grants,
    }
}

async fn download(f: &Fixture, caller: &Caller, artifact_id: ArtifactId) -> CommandReceipt {
    f.hub
        .submit(
            caller,
            CommandEnvelope::new(Command::CreateDownload {
                shown_in_task: None,
                source: DownloadSource::Artifact { artifact_id },
            }),
        )
        .await
}

#[tokio::test]
async fn transfers_alone_reach_only_the_callers_own_artifacts() {
    let f = Fixture::new().await;
    let transfers = Grants {
        transfers: true,
        ..Grants::default()
    };
    let other = device_with(&f, 7, transfers.clone()).await;
    let owner_capture = f
        .hub
        .put_artifact(b"screen", "image/png", None, Principal::LocalOwner)
        .await
        .unwrap();
    let project = f.project().await;
    let (task, _) = f
        .task(
            project,
            TaskPlacement::Queue {
                position: None,
                authorization: QueueAuthorization::AskBeforeStart,
            },
            &Caller::local_owner(),
            false,
        )
        .await;
    let observation = f
        .hub
        .put_artifact(b"observed", "image/png", Some(task), Principal::System)
        .await
        .unwrap();
    for artifact in [&owner_capture, &observation] {
        let receipt = download(&f, &other, artifact.artifact_id).await;
        assert_eq!(receipt.status, ReceiptStatus::Rejected, "{receipt:?}");
        assert_eq!(receipt.error.unwrap().code, ErrorCode::PermissionDenied);
    }
    let own = f
        .hub
        .put_artifact(b"mine", "image/png", None, other.principal)
        .await
        .unwrap();
    let receipt = download(&f, &other, own.artifact_id).await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    let reviewer = device_with(
        &f,
        8,
        Grants {
            tasks: true,
            ..transfers
        },
    )
    .await;
    let receipt = download(&f, &reviewer, observation.artifact_id).await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    let receipt = download(&f, &reviewer, owner_capture.artifact_id).await;
    assert_eq!(receipt.status, ReceiptStatus::Rejected, "{receipt:?}");
    let receipt = download(&f, &Caller::local_owner(), owner_capture.artifact_id).await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    f.close().await;
}

#[tokio::test]
async fn a_transfer_moves_only_for_the_device_that_asked_for_it() {
    let f = Fixture::new().await;
    let transfers = Grants {
        transfers: true,
        ..Grants::default()
    };
    let asking = device_with(&f, 7, transfers.clone()).await;
    let other = device_with(&f, 8, transfers).await;
    let own = f
        .hub
        .put_artifact(b"dictation", "audio/wav", None, asking.principal)
        .await
        .unwrap();
    let receipt = download(&f, &asking, own.artifact_id).await;
    let Some(CommandResult::Transfer { transfer }) = receipt.result else {
        panic!("{receipt:?}")
    };
    let Err(refused) = f.hub.download(&other, transfer.transfer_id, 0).await else {
        panic!("another device's download")
    };
    assert_eq!(refused.code, ErrorCode::PermissionDenied);
    assert!(
        f.hub
            .download(&asking, transfer.transfer_id, 0)
            .await
            .is_ok()
    );
    // A manager may continue it, as a manager may fetch any artifact.
    assert!(
        f.hub
            .download(&Caller::local_owner(), transfer.transfer_id, 0)
            .await
            .is_ok()
    );
    let upload = f
        .hub
        .submit(
            &asking,
            CommandEnvelope::new(Command::CreateUpload {
                evidence: None,
                name: "notes.txt".into(),
                size: 4,
                sha256: "0".repeat(64),
                destination: TransferDestination::Artifact,
            }),
        )
        .await;
    let Some(CommandResult::Transfer { transfer }) = upload.result else {
        panic!("{upload:?}")
    };
    let Err(refused) = f.hub.upload(&other, transfer.transfer_id).await else {
        panic!("another device's upload")
    };
    assert_eq!(refused.code, ErrorCode::PermissionDenied);
    assert!(f.hub.upload(&asking, transfer.transfer_id).await.is_ok());
    f.close().await;
}

// A download that starts at its end, an empty file or a resume after the
// last byte, sends no bytes and so never reads its source to the end. The
// phone is told it completed; the computer's record says so too.
#[tokio::test]
async fn a_download_with_nothing_left_to_send_is_complete() {
    let f = Fixture::new().await;
    let owner = Caller::local_owner();
    for (bytes, offset) in [(b"".as_slice(), 0), (b"dictation".as_slice(), 9)] {
        let artifact = f
            .hub
            .put_artifact(bytes, "audio/wav", None, owner.principal)
            .await
            .unwrap();
        let receipt = download(&f, &owner, artifact.artifact_id).await;
        let Some(CommandResult::Transfer { transfer }) = receipt.result else {
            panic!("{receipt:?}")
        };
        f.hub
            .download(&owner, transfer.transfer_id, offset)
            .await
            .unwrap();
        let record: TransferRecord = f
            .hub
            .read(|r| r.require("transfer", transfer.transfer_id))
            .unwrap();
        assert_eq!(record.state, TransferState::Completed, "{bytes:?}");
        assert_eq!(record.transferred, bytes.len() as u64);
    }
    f.close().await;
}

// Cancelling ends the transfer and deletes its staged bytes. Like moving the
// bytes, it is for the device that asked for the transfer, or a manager.
#[tokio::test]
async fn only_the_device_that_asked_for_a_transfer_cancels_it() {
    let f = Fixture::new().await;
    let transfers = Grants {
        transfers: true,
        ..Grants::default()
    };
    let asking = device_with(&f, 7, transfers.clone()).await;
    let other = device_with(&f, 8, transfers).await;
    let mut ids = Vec::new();
    for _ in 0..2 {
        let upload = f
            .hub
            .submit(
                &asking,
                CommandEnvelope::new(Command::CreateUpload {
                    evidence: None,
                    name: "notes.txt".into(),
                    size: 4,
                    sha256: "0".repeat(64),
                    destination: TransferDestination::Artifact,
                }),
            )
            .await;
        let Some(CommandResult::Transfer { transfer }) = upload.result else {
            panic!("{upload:?}")
        };
        ids.push(transfer.transfer_id);
    }
    let state = |id| {
        f.hub
            .read(|r| r.require::<TransferRecord>("transfer", id))
            .unwrap()
            .state
    };
    let before = state(ids[0]);
    let Err(refused) = f.hub.cancel_transfer(&other, ids[0]).await else {
        panic!("another device cancelled the transfer")
    };
    assert_eq!(refused.code, ErrorCode::PermissionDenied);
    assert_eq!(state(ids[0]), before);
    f.hub.cancel_transfer(&asking, ids[0]).await.unwrap();
    assert_eq!(state(ids[0]), TransferState::Cancelled);
    f.hub
        .cancel_transfer(&Caller::local_owner(), ids[1])
        .await
        .unwrap();
    assert_eq!(state(ids[1]), TransferState::Cancelled);
    f.close().await;
}

/// A FIFO that nothing writes to must be refused at once. Opening one for
/// reading would otherwise block admission, and the runtime thread with it.
#[cfg(unix)]
#[tokio::test]
async fn a_project_fifo_is_refused_without_blocking() {
    let f = Fixture::new().await;
    let project_id = f.project().await;
    let fifo = f.project_dir().join("report.csv");
    assert!(
        std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success()
    );
    // Stands in for a writer that eventually arrives, so an unfixed build
    // fails this test instead of hanging it. O_RDWR never blocks on a FIFO.
    let release = std::thread::spawn({
        let fifo = fifo.clone();
        move || {
            std::thread::sleep(Duration::from_secs(3));
            drop(
                std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(fifo),
            );
        }
    });
    let started = std::time::Instant::now();
    let receipt = f
        .hub
        .submit(
            &Caller::local_owner(),
            CommandEnvelope::new(Command::CreateDownload {
                shown_in_task: None,
                source: DownloadSource::ProjectFile {
                    project_id,
                    path: "report.csv".into(),
                },
            }),
        )
        .await;
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "admission blocked"
    );
    assert_eq!(receipt.status, ReceiptStatus::Rejected, "{receipt:?}");
    assert_eq!(receipt.error.unwrap().code, ErrorCode::PathNotAllowed);
    release.join().unwrap();
    f.close().await;
}

/// A publication that fails after its bytes were staged in the destination
/// folder must not leave them there under a hidden name.
#[tokio::test]
async fn a_failed_publication_leaves_no_staged_copy_behind() {
    let f = Fixture::new().await;
    let owner = Caller::local_owner();
    let data = b"replacement";
    let receipt = f
        .hub
        .submit(
            &owner,
            CommandEnvelope::new(Command::CreateUploadReviewed {
                task_id: None,
                name: "notes.txt".into(),
                size: data.len() as u64,
                sha256: hash(data),
                destination: TransferDestination::Inbox,
                collision: UploadCollision::Replace {
                    revision: hash(b"old"),
                },
            }),
        )
        .await;
    let Some(CommandResult::Transfer { transfer }) = receipt.result else {
        panic!("{receipt:?}")
    };
    let mut upload = f.hub.upload(&owner, transfer.transfer_id).await.unwrap();
    upload.write(data).await.unwrap();
    assert!(upload.finish().await.is_err());
    let inbox = f.hub.inbox_folder().unwrap();
    let left: Vec<_> = std::fs::read_dir(&inbox)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert!(left.is_empty(), "{left:?}");
    f.close().await;
}
