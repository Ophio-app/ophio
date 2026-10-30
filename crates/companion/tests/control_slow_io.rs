// SPDX-License-Identifier: Apache-2.0
#![cfg(unix)]
use companion::{identity::Identity, local::LocalServer, server::ServerDeps};
use futures_util::{SinkExt, StreamExt};
use ports::{HarnessEvent, LibraryFs};
use protocol::{capability::HarnessKind, command::*, library::RootKind, model::*, *};
use services::{
    Caller, CompanionHub, Hub,
    testing::{FakeClock, FakeHarness, FakeLibrary},
};
use std::{
    sync::{Arc, atomic::Ordering},
    time::Duration,
};
use tokio_util::{
    codec::{Framed, LinesCodec},
    sync::CancellationToken,
};

type Client = Framed<tokio::net::UnixStream, LinesCodec>;
async fn send(client: &mut Client, frame: ClientFrame) {
    client
        .send(serde_json::to_string(&frame).unwrap())
        .await
        .unwrap();
}
async fn receive(client: &mut Client) -> ServerFrame {
    let line = tokio::time::timeout(Duration::from_secs(2), client.next())
        .await
        .expect("control response blocked")
        .unwrap()
        .unwrap();
    serde_json::from_str(&line).unwrap()
}

fn download_library() -> Arc<FakeLibrary> {
    use std::io::{Seek, Write};
    let library = Arc::new(FakeLibrary::default());
    let mut file = tempfile::tempfile().unwrap();
    file.write_all(b"download bytes\n").unwrap();
    file.rewind().unwrap();
    *library.download_file.lock().unwrap() = Some(file);
    library
}

async fn stop_during_file_work(download: bool) {
    let dir = tempfile::tempdir().unwrap();
    let project_path = dir.path().join("project");
    std::fs::create_dir(&project_path).unwrap();
    let library = download_library();
    let harness = Arc::new(FakeHarness::default());
    let clock = Arc::new(FakeClock::default());
    let hub = CompanionHub::builder()
        .data_dir(dir.path().join("data"))
        .library(library.clone())
        .harness(harness.clone())
        .clock(clock.clone())
        .bridge(services::bridge::BridgeConfig::new(
            "synthetic-bridge",
            "synthetic-socket",
        ))
        .build()
        .await
        .unwrap();
    let owner = Caller::local_owner();
    let root_id = LibraryRootId::new();
    hub.set_library_roots(vec![ports::RootSpec {
        root_id,
        label: "Test".into(),
        path: project_path.clone(),
        kind: RootKind::Notes,
        writable: true,
        project_id: None,
        harness: None,
        only: None,
    }])
    .await
    .unwrap();
    library
        .create(root_id, "slow.md", "download bytes\n")
        .await
        .unwrap();
    let add = CommandEnvelope::new(Command::AddProject {
        host_path: project_path.to_string_lossy().into(),
        name: None,
    });
    let added = hub.submit(&owner, add.clone()).await;
    let Some(CommandResult::ProjectAdded { project_id }) = added.result else {
        panic!("{added:?}")
    };
    let created = hub
        .submit(
            &owner,
            CommandEnvelope::new(Command::CreateTask {
                project_id,
                kind: TaskKind::Coding,
                mode: None,
                title: None,
                prompt: "Work".into(),
                bundle: None,
                harness: HarnessKind::ClaudeCode,
                model: None,
                effort: None,
                mission: None,
                target_id: None,
                placement: TaskPlacement::StartNow,
                launch_policy: None,
                branch: None,
                start: None,
                place: None,
                browser: None,
                conversation: None,
                access: None,
                github: None,
                agent_chosen: None,
                review: None,
            }),
        )
        .await;
    let Some(CommandResult::TaskCreated { task_id, .. }) = created.result else {
        panic!("{created:?}")
    };
    let mut events = hub.subscribe(&owner, None).await.unwrap().live;
    let run_id = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Some(id) = harness.runs().first() {
                break *id;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    harness
        .emit(
            run_id,
            HarnessEvent::SessionStarted {
                native_session: "test-run".into(),
                actual_model: None,
                actual_effort: None,
                account_label: None,
            },
        )
        .await;
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let services::api::Outbound::Event(event) = events.recv().await.unwrap()
                && matches!(&event.event, Event::RunUpserted { run } if run.native_session.is_some()) {
                break;
            }
        }
    }).await.unwrap();

    let identity = Arc::new(Identity::generate(hub.host_id(), vec!["test".into()]).unwrap());
    let mut deps = ServerDeps::new(Arc::new(hub.clone()), identity).unwrap();
    deps.clock = clock.clone();
    let path = dir.path().join("run/owner.sock");
    let server = LocalServer::bind(&path, companion::started::Started::claim().unwrap())
        .await
        .unwrap();
    let shutdown = CancellationToken::new();
    let task = tokio::spawn(server.run(deps, shutdown.clone()));
    let mut client = Framed::new(
        tokio::net::UnixStream::connect(path).await.unwrap(),
        LinesCodec::new(),
    );
    library.hold_read.store(true, Ordering::SeqCst);
    let slow = if download {
        ClientFrame::Command(Box::new(CommandEnvelope::new(Command::CreateDownload {
            source: DownloadSource::LibraryFile {
                root_id,
                path: "slow.md".into(),
            },
            shown_in_task: None,
        })))
    } else {
        ClientFrame::Query(QueryEnvelope {
            query_id: QueryId::new(),
            query: Query::LibraryRead {
                root_id,
                path: "slow.md".into(),
            },
        })
    };
    send(&mut client, slow).await;
    tokio::time::timeout(Duration::from_secs(2), library.read_entered.acquire())
        .await
        .unwrap()
        .unwrap()
        .forget();

    // Admission must retain revision checks even on the intervention path.
    let mut stale = CommandEnvelope::new(Command::StopTask {
        task_id,
        force: false,
    });
    stale.expected_revision = Some(999);
    send(&mut client, ClientFrame::Command(Box::new(stale))).await;
    assert!(
        matches!(receive(&mut client).await, ServerFrame::Receipt(receipt)
        if receipt.error.as_ref().unwrap().code == ErrorCode::StaleRevision)
    );
    assert_eq!(harness.controls.interrupts.load(Ordering::SeqCst), 0);
    let stop = CommandEnvelope::new(Command::StopTask {
        task_id,
        force: false,
    });
    send(&mut client, ClientFrame::Command(Box::new(stop.clone()))).await;
    assert!(
        matches!(receive(&mut client).await, ServerFrame::Receipt(receipt)
        if receipt.command_id == stop.command_id && receipt.status == ReceiptStatus::Accepted)
    );
    tokio::time::timeout(Duration::from_secs(2), async {
        while harness.controls.interrupts.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("native interrupt waited for file work");
    send(&mut client, ClientFrame::Command(Box::new(stop))).await;
    assert!(
        matches!(receive(&mut client).await, ServerFrame::Receipt(receipt)
        if receipt.status == ReceiptStatus::Duplicate)
    );
    assert_eq!(harness.controls.interrupts.load(Ordering::SeqCst), 1);

    // An ordinary command's TTL starts at socket ingestion, not after file I/O.
    let mut expiring = CommandEnvelope::new(Command::RunReadinessCheck);
    expiring.expires_in_ms = Some(10);
    send(
        &mut client,
        ClientFrame::Command(Box::new(expiring.clone())),
    )
    .await;
    send(&mut client, ClientFrame::Ping { nonce: 1 }).await;
    assert_eq!(receive(&mut client).await, ServerFrame::Pong { nonce: 1 });
    // Saturation must not falsely reject an already accepted command ID.
    for _ in 0..127 {
        send(
            &mut client,
            ClientFrame::Query(QueryEnvelope {
                query_id: QueryId::new(),
                query: Query::Devices,
            }),
        )
        .await;
    }
    send(&mut client, ClientFrame::Command(Box::new(add))).await;
    assert!(
        matches!(receive(&mut client).await, ServerFrame::Receipt(receipt)
        if receipt.status == ReceiptStatus::Duplicate && receipt.error.is_none())
    );
    let excess = CommandEnvelope::new(Command::RunReadinessCheck);
    send(&mut client, ClientFrame::Command(Box::new(excess.clone()))).await;
    assert!(
        matches!(receive(&mut client).await, ServerFrame::Receipt(receipt)
        if receipt.command_id == excess.command_id && receipt.status == ReceiptStatus::Rejected
            && receipt.error.as_ref().unwrap().code == ErrorCode::LimitExceeded)
    );
    clock.advance(Duration::from_millis(20));
    library.read_gate.close();
    match receive(&mut client).await {
        ServerFrame::Receipt(receipt) if download => {
            assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}")
        }
        ServerFrame::QueryResult(result) if !download => assert!(result.result.is_ok()),
        frame => panic!("unexpected file response: {frame:?}"),
    }
    assert!(
        matches!(receive(&mut client).await, ServerFrame::Receipt(receipt)
        if receipt.command_id == expiring.command_id && receipt.error.as_ref().unwrap().code == ErrorCode::Expired)
    );
    for _ in 0..127 {
        assert!(
            matches!(receive(&mut client).await, ServerFrame::QueryResult(result) if result.result.is_ok())
        );
    }
    send(&mut client, ClientFrame::Command(Box::new(excess))).await;
    assert!(
        matches!(receive(&mut client).await, ServerFrame::Receipt(receipt)
        if receipt.status == ReceiptStatus::Duplicate
            && receipt.error.as_ref().unwrap().code == ErrorCode::LimitExceeded)
    );
    // Receipt time never rewrites the envelope, so retries still deduplicate.
    send(&mut client, ClientFrame::Command(Box::new(expiring))).await;
    assert!(
        matches!(receive(&mut client).await, ServerFrame::Receipt(receipt)
        if receipt.status == ReceiptStatus::Duplicate && receipt.error.as_ref().unwrap().code == ErrorCode::Expired)
    );
    drop(client);
    shutdown.cancel();
    task.await.unwrap().unwrap();
    hub.shutdown().await.unwrap();
}

#[tokio::test]
async fn download_preparation_allows_native_interrupt_and_keeps_admission_guards() {
    stop_during_file_work(true).await;
}

#[tokio::test]
async fn library_read_allows_native_interrupt_and_keeps_admission_guards() {
    stop_during_file_work(false).await;
}

async fn retry_before_admission(active: bool, expired: bool) {
    let dir = tempfile::tempdir().unwrap();
    let library = download_library();
    let clock = Arc::new(FakeClock::default());
    let hub = CompanionHub::builder()
        .data_dir(dir.path().join("data"))
        .library(library.clone())
        .clock(clock.clone())
        .build()
        .await
        .unwrap();
    let owner = Caller::local_owner();
    let root_id = LibraryRootId::new();
    hub.set_library_roots(vec![ports::RootSpec {
        root_id,
        label: "Test".into(),
        path: dir.path().join("files"),
        kind: RootKind::Notes,
        writable: true,
        project_id: None,
        harness: None,
        only: None,
    }])
    .await
    .unwrap();
    library
        .create(root_id, "slow.md", "download bytes\n")
        .await
        .unwrap();
    let identity = Arc::new(Identity::generate(hub.host_id(), vec!["test".into()]).unwrap());
    let mut deps = ServerDeps::new(Arc::new(hub.clone()), identity).unwrap();
    deps.clock = clock.clone();
    let path = dir.path().join("run/owner.sock");
    let server = LocalServer::bind(&path, companion::started::Started::claim().unwrap())
        .await
        .unwrap();
    let shutdown = CancellationToken::new();
    let server_task = tokio::spawn(server.run(deps, shutdown.clone()));
    let mut client = Framed::new(
        tokio::net::UnixStream::connect(path).await.unwrap(),
        LinesCodec::new(),
    );
    let mut command = CommandEnvelope::new(Command::CreateDownload {
        source: DownloadSource::LibraryFile {
            root_id,
            path: "slow.md".into(),
        },
        shown_in_task: None,
    });
    command.expires_in_ms = Some(if expired { 10 } else { 10_000 });
    library.hold_read.store(true, Ordering::SeqCst);
    send(
        &mut client,
        if active {
            ClientFrame::Command(Box::new(command.clone()))
        } else {
            ClientFrame::Query(QueryEnvelope {
                query_id: QueryId::new(),
                query: Query::LibraryRead {
                    root_id,
                    path: "slow.md".into(),
                },
            })
        },
    )
    .await;
    tokio::time::timeout(Duration::from_secs(2), library.read_entered.acquire())
        .await
        .unwrap()
        .unwrap()
        .forget();
    if !active {
        send(&mut client, ClientFrame::Command(Box::new(command.clone()))).await;
    }
    for _ in 0..if active { 128 } else { 127 } {
        send(
            &mut client,
            ClientFrame::Query(QueryEnvelope {
                query_id: QueryId::new(),
                query: Query::Devices,
            }),
        )
        .await;
    }
    send(&mut client, ClientFrame::Ping { nonce: 1 }).await;
    assert_eq!(receive(&mut client).await, ServerFrame::Pong { nonce: 1 });
    clock.advance(Duration::from_millis(20));
    // Matching retries consume no queue slots and await the original receipt.
    for _ in 0..3 {
        let mut retry = command.clone();
        retry.issued_at.0 += 1; // Informational, not part of command identity.
        send(&mut client, ClientFrame::Command(Box::new(retry))).await;
    }
    send(&mut client, ClientFrame::Ping { nonce: 2 }).await;
    let before_release = receive(&mut client).await;
    assert_eq!(
        before_release,
        ServerFrame::Pong { nonce: 2 },
        "a retry poisoned the pending command"
    );
    // An altered request using the same ID must not replace or reject the
    // original, including when the altered body would use the urgent path.
    for change in 0..4 {
        let mut changed = command.clone();
        match change {
            0 => {
                changed.body = Command::StopTask {
                    task_id: TaskId::new(),
                    force: false,
                }
            }
            1 => changed.expected_revision = Some(1),
            2 => changed.expires_at = Some(protocol::time::Timestamp(1)),
            _ => changed.expires_in_ms = Some(99_000),
        }
        send(&mut client, ClientFrame::Command(Box::new(changed))).await;
        assert!(
            matches!(receive(&mut client).await, ServerFrame::Receipt(receipt)
            if receipt.status == ReceiptStatus::Rejected && receipt.error.as_ref().unwrap().code == ErrorCode::Conflict)
        );
    }
    library.read_gate.close();
    if !active {
        assert!(matches!(
            receive(&mut client).await,
            ServerFrame::QueryResult(_)
        ));
    }
    let ServerFrame::Receipt(receipt) = receive(&mut client).await else {
        panic!("receipt expected")
    };
    assert_eq!(receipt.command_id, command.command_id);
    if expired {
        assert_eq!(receipt.status, ReceiptStatus::Rejected);
        assert_eq!(receipt.error.unwrap().code, ErrorCode::Expired);
    } else {
        assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    }
    for _ in 0..if active { 128 } else { 127 } {
        assert!(matches!(
            receive(&mut client).await,
            ServerFrame::QueryResult(_)
        ));
    }
    send(&mut client, ClientFrame::Command(Box::new(command))).await;
    assert!(
        matches!(receive(&mut client).await, ServerFrame::Receipt(receipt) if receipt.status == ReceiptStatus::Duplicate)
    );
    let QueryData::Transfers { transfers } = hub.query(&owner, Query::Transfers).await.unwrap()
    else {
        unreachable!()
    };
    assert_eq!(transfers.len(), usize::from(!expired));
    drop(client);
    shutdown.cancel();
    server_task.await.unwrap().unwrap();
    hub.shutdown().await.unwrap();
}

#[tokio::test]
async fn queued_retry_survives_a_full_queue() {
    retry_before_admission(false, false).await;
}
#[tokio::test]
async fn preparing_retry_survives_a_full_queue() {
    retry_before_admission(true, false).await;
}
#[tokio::test]
async fn queued_retry_preserves_original_expiry() {
    retry_before_admission(false, true).await;
}
#[tokio::test]
async fn preparing_retry_preserves_original_expiry() {
    retry_before_admission(true, true).await;
}
