// SPDX-License-Identifier: Apache-2.0
use crate::{Caller, CompanionHub, Hub};
use protocol::{command::*, transfer::*};

#[tokio::test]
async fn configured_inbox_preserves_collisions_and_admitted_destination_across_restart() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("data");
    let folder = temp.path().join("Downloads").join("companion");
    // Only the existing parent is canonical: the inbox is not created yet.
    let expected_folder = temp
        .path()
        .canonicalize()
        .unwrap()
        .join("Downloads/companion");
    let other = temp.path().join("Other");
    let hub = CompanionHub::builder()
        .data_dir(&data)
        .inbox_dir(&folder)
        .build()
        .await
        .unwrap();
    assert!(!folder.exists(), "Create the folder only on first use");
    let owner = Caller::local_owner();
    let declare = || {
        CommandEnvelope::new(Command::CreateUpload {
            name: "note.txt".into(),
            size: 1,
            sha256: format!("{:x}", sha2::Sha256::digest(b"x")),
            destination: TransferDestination::Inbox,
            evidence: None,
        })
    };
    use sha2::Digest;
    let receipt = hub.submit(&owner, declare()).await;
    let Some(CommandResult::Transfer { transfer }) = receipt.result else {
        panic!("{receipt:?}")
    };
    assert_eq!(
        transfer.destination_folder.as_deref(),
        expected_folder.to_str()
    );
    hub.set_inbox_folder(&other).await.unwrap();
    let mut upload = hub.upload(&owner, transfer.transfer_id).await.unwrap();
    upload.write(b"x").await.unwrap();
    upload.finish().await.unwrap();
    let done: TransferRecord = hub
        .read(|r| r.require("transfer", transfer.transfer_id))
        .unwrap();
    assert_eq!(
        done.host_path.as_deref(),
        expected_folder.join("note.txt").to_str()
    );
    assert_eq!(std::fs::read(folder.join("note.txt")).unwrap(), b"x");
    hub.set_inbox_folder(&folder).await.unwrap();
    let check = hub
        .check_upload_destination(&owner, "note.txt", &TransferDestination::Inbox)
        .await
        .unwrap();
    assert!(check.exists);
    assert_eq!(check.keep_both_name, "note (1).txt");
    let receipt = hub
        .submit(
            &owner,
            CommandEnvelope::new(Command::CreateUpload {
                name: "note (1).txt".into(),
                size: 1,
                sha256: format!("{:x}", sha2::Sha256::digest(b"x")),
                destination: TransferDestination::Inbox,
                evidence: None,
            }),
        )
        .await;
    let Some(CommandResult::Transfer { transfer }) = receipt.result else {
        panic!("{receipt:?}")
    };
    let mut upload = hub.upload(&owner, transfer.transfer_id).await.unwrap();
    upload.write(b"x").await.unwrap();
    upload.finish().await.unwrap();
    assert_eq!(std::fs::read(folder.join("note.txt")).unwrap(), b"x");
    assert_eq!(std::fs::read(folder.join("note (1).txt")).unwrap(), b"x");
    // An interrupted publication uses its admitted folder even after config changes.
    hub.write(|tx| {
        let mut interrupted = transfer.clone();
        interrupted.state = TransferState::Verifying;
        interrupted.publication = None;
        tx.put("transfer", transfer.transfer_id, &interrupted)?;
        tx.put("publication_started", transfer.transfer_id, &true)?;
        Ok(())
    })
    .unwrap();
    hub.shutdown().await.unwrap();
    drop(hub);
    let hub = CompanionHub::builder()
        .data_dir(&data)
        .inbox_dir(&other)
        .build()
        .await
        .unwrap();
    let recovered: TransferRecord = hub
        .read(|r| r.require("transfer", transfer.transfer_id))
        .unwrap();
    assert!(matches!(
        recovered.state,
        TransferState::ReconciliationRequired { .. }
    ));
    assert_eq!(
        recovered.publication.unwrap().root,
        expected_folder.to_string_lossy()
    );
    assert!(!other.exists());
    hub.shutdown().await.unwrap();
}

#[tokio::test]
async fn keep_both_offers_a_name_that_sends_even_for_the_longest_names() {
    use sha2::Digest;
    let temp = tempfile::tempdir().unwrap();
    let folder = temp.path().join("Inbox");
    std::fs::create_dir(&folder).unwrap();
    let hub = CompanionHub::builder()
        .data_dir(temp.path().join("data"))
        .inbox_dir(&folder)
        .build()
        .await
        .unwrap();
    let owner = Caller::local_owner();
    for length in 197..=200 {
        let name = format!("{}é.txt", "n".repeat(length - 6));
        assert_eq!(name.len(), length);
        std::fs::write(folder.join(&name), b"old").unwrap();
        let check = hub
            .check_upload_destination(&owner, &name, &TransferDestination::Inbox)
            .await
            .unwrap();
        assert!(check.exists);
        // Shortened stems can meet an earlier one's name; the offer skips it.
        assert!(check.keep_both_name.ends_with(").txt"), "{check:?}");
        assert!(!folder.join(&check.keep_both_name).exists(), "{check:?}");
        let receipt = hub
            .submit(
                &owner,
                CommandEnvelope::new(Command::CreateUploadReviewed {
                    task_id: None,
                    name: name.clone(),
                    size: 3,
                    sha256: format!("{:x}", sha2::Sha256::digest(b"new")),
                    destination: TransferDestination::Inbox,
                    collision: UploadCollision::KeepBoth {
                        name: check.keep_both_name.clone(),
                    },
                }),
            )
            .await;
        let Some(CommandResult::Transfer { transfer }) = receipt.result else {
            panic!("{length}: {receipt:?}")
        };
        let mut upload = hub.upload(&owner, transfer.transfer_id).await.unwrap();
        upload.write(b"new").await.unwrap();
        upload.finish().await.unwrap();
        assert_eq!(std::fs::read(folder.join(&name)).unwrap(), b"old");
        assert_eq!(
            std::fs::read(folder.join(&check.keep_both_name)).unwrap(),
            b"new"
        );
    }
    hub.shutdown().await.unwrap();
}

#[test]
fn inbox_refuses_data_paths_including_dot_components_and_symlinks() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("data");
    std::fs::create_dir(&data).unwrap();
    for path in [
        data.clone(),
        data.join("new"),
        temp.path().join("new/../data/nested"),
    ] {
        assert!(crate::inbox::outside_data(&path, &data).is_err());
    }
    #[cfg(unix)]
    {
        let link = temp.path().join("disguised");
        std::os::unix::fs::symlink(&data, &link).unwrap();
        assert!(crate::inbox::outside_data(&link.join("new"), &data).is_err());
    }
    assert_eq!(
        crate::inbox::outside_data(&temp.path().join("Downloads/companion"), &data).unwrap(),
        temp.path()
            .canonicalize()
            .unwrap()
            .join("Downloads/companion")
    );
}

// A file already named like a transfer's staging file, left by a publication
// that was interrupted or put there by anyone who can write to the folder,
// must not keep that transfer from being published.
#[tokio::test]
async fn a_leftover_under_the_staging_name_does_not_block_publication() {
    use sha2::Digest;
    let temp = tempfile::tempdir().unwrap();
    let folder = temp.path().join("Inbox");
    let hub = CompanionHub::builder()
        .data_dir(temp.path().join("data"))
        .inbox_dir(&folder)
        .build()
        .await
        .unwrap();
    let owner = Caller::local_owner();
    let receipt = hub
        .submit(
            &owner,
            CommandEnvelope::new(Command::CreateUpload {
                name: "report.txt".into(),
                size: 1,
                sha256: format!("{:x}", sha2::Sha256::digest(b"x")),
                destination: TransferDestination::Inbox,
                evidence: None,
            }),
        )
        .await;
    let Some(CommandResult::Transfer { transfer }) = receipt.result else {
        panic!("{receipt:?}")
    };
    std::fs::create_dir_all(&folder).unwrap();
    let planted = folder.join(format!(".incoming-{}", transfer.transfer_id));
    std::fs::write(&planted, b"planted").unwrap();
    let mut upload = hub.upload(&owner, transfer.transfer_id).await.unwrap();
    upload.write(b"x").await.unwrap();
    upload.finish().await.unwrap();
    assert_eq!(std::fs::read(folder.join("report.txt")).unwrap(), b"x");
    // Only this transfer uses that name, so what was there is cleared.
    assert!(!planted.exists());
}
