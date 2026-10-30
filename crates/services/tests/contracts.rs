// SPDX-License-Identifier: Apache-2.0
use async_trait::async_trait;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ports::{
    CapturedFrame, Clock, DesktopBackend, DesktopCapabilities, FrameStream, InputSink,
    PlatformError,
};
use protocol::{
    Command, CommandEnvelope, ErrorCode, ProtocolVersion, Query, QueryData, TargetId, TransferId,
    auth::{PairRequest, Principal},
    capability::{Availability, InputScope},
    command::{CommandResult, CommandStage, ReceiptStatus},
    control::{
        ControlTarget, InputAction, InputEnvelope, LeaseState, LeaseView, TargetGeometry,
        TargetKind, TargetPoint,
    },
    time::Timestamp,
    transfer::{TransferDestination, TransferDirection, TransferRecord, TransferState},
};
use services::{Caller, CompanionHub, Hub};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Instant,
};
use tokio::sync::Semaphore;

#[derive(Debug)]
struct FixedClock(Instant);
impl Clock for FixedClock {
    fn now(&self) -> Timestamp {
        Timestamp(1000)
    }
    fn monotonic(&self) -> Instant {
        self.0
    }
}

struct TestDirectory(PathBuf);
impl TestDirectory {
    fn new() -> Self {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/review-fixtures")
            .join(uuid::Uuid::new_v4().to_string());
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}
impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[derive(Debug)]
struct NativeQueue {
    applied: AtomicUsize,
    released: AtomicUsize,
    deliver: Semaphore,
    completed: Semaphore,
    delay: bool,
    cancelled: AtomicBool,
    fail_barrier: AtomicBool,
}
impl NativeQueue {
    fn new(delay: bool) -> Self {
        Self {
            applied: AtomicUsize::new(0),
            released: AtomicUsize::new(0),
            deliver: Semaphore::new(0),
            completed: Semaphore::new(0),
            delay,
            cancelled: AtomicBool::new(false),
            fail_barrier: AtomicBool::new(false),
        }
    }
}

struct QueuedSink(Arc<NativeQueue>);
#[async_trait]
impl InputSink for QueuedSink {
    async fn inject(&mut self, _: &InputAction, _: &TargetGeometry) -> Result<(), PlatformError> {
        if self.0.delay {
            let queue = self.0.clone();
            // Model a native request already handed to an asynchronous driver,
            // whose acknowledgement fails before the driver processes it.
            tokio::spawn(async move {
                queue.deliver.acquire().await.unwrap().forget();
                if !queue.cancelled.load(Ordering::SeqCst) {
                    queue.applied.fetch_add(1, Ordering::SeqCst);
                }
                queue.completed.add_permits(1);
            });
        } else {
            self.0.applied.fetch_add(1, Ordering::SeqCst);
        }
        Err(PlatformError::OutcomeUnknown(
            "Synthetic lost acknowledgement".into(),
        ))
    }
    async fn release_all(&mut self) -> Result<(), PlatformError> {
        self.0.released.fetch_add(1, Ordering::SeqCst);
        if self.0.fail_barrier.load(Ordering::SeqCst) {
            return Err(PlatformError::OutcomeUnknown(
                "Synthetic barrier failure".into(),
            ));
        }
        // No await separates the native cancellation check from injection. The
        // fake driver and barrier execute on this test's single-thread runtime.
        self.0.cancelled.store(true, Ordering::SeqCst);
        Ok(())
    }
}

#[derive(Debug)]
struct Desktop {
    target: ControlTarget,
    queue: Arc<NativeQueue>,
}
impl Desktop {
    fn new(delay: bool) -> Self {
        Self {
            target: ControlTarget {
                target_id: TargetId::new(),
                kind: TargetKind::Output,
                label: "Synthetic target".into(),
                native_ref: "synthetic".into(),
                geometry: TargetGeometry {
                    width: 32,
                    height: 24,
                    scale: 1.0,
                    rotation_degrees: 0,
                    revision: 1,
                },
                capture: Availability::Available,
                input: Availability::Available,
            },
            queue: Arc::new(NativeQueue::new(delay)),
        }
    }
}
#[async_trait]
impl DesktopBackend for Desktop {
    async fn capabilities(&self) -> DesktopCapabilities {
        DesktopCapabilities {
            backend: "synthetic".into(),
            capture: Availability::Available,
            input: Availability::Available,
            input_scope: InputScope::OutputBound,
            lock_session: Availability::Unsupported {
                reason: "Fixture".into(),
            },
            unlock_session: Availability::Unsupported {
                reason: "Fixture".into(),
            },
        }
    }
    async fn targets(&self) -> Result<Vec<ControlTarget>, PlatformError> {
        Ok(vec![self.target.clone()])
    }
    async fn capture_still(&self, _: TargetId) -> Result<CapturedFrame, PlatformError> {
        Err(PlatformError::Unsupported(
            "Fixture does not capture".into(),
        ))
    }
    async fn open_stream(&self, _: TargetId, _: u8) -> Result<Box<dyn FrameStream>, PlatformError> {
        Err(PlatformError::Unsupported("Fixture does not stream".into()))
    }
    async fn input_sink(&self, _: TargetId) -> Result<Box<dyn InputSink>, PlatformError> {
        Ok(Box::new(QueuedSink(self.queue.clone())))
    }
    async fn lock_session(&self) -> Result<(), PlatformError> {
        Err(PlatformError::Unsupported("Fixture does not lock".into()))
    }
}

async fn enroll_fixture(hub: &CompanionHub) -> Caller {
    let ticket = hub.open_pairing(&Caller::local_owner()).await.unwrap();
    let claim = hub
        .request_pairing(
            PairRequest {
                pairing_secret: ticket.secret,
                device_public_key: URL_SAFE_NO_PAD.encode([42_u8; 32]),
                device_name: "Fixture".into(),
                device_platform: "synthetic".into(),
                client_version: "fixture".into(),
                protocol: ProtocolVersion::CURRENT,
                shows_code: true,
                replaces: None,
            },
            "123456".into(),
        )
        .await
        .unwrap();
    hub.answer_pairing(&Caller::local_owner(), "123456", true)
        .await
        .unwrap();
    let response = claim.answer.await.unwrap().unwrap();
    Caller {
        principal: Principal::Device {
            device_id: response.device_id,
        },
        grants: response.grants,
    }
}

async fn lease(hub: &CompanionHub, caller: &Caller, target_id: TargetId) -> LeaseView {
    let command = CommandEnvelope::new(Command::RequestHumanLease {
        target_id,
        geometry_revision: 1,
    });
    let id = command.command_id;
    assert_eq!(
        hub.submit(caller, command).await.status,
        ReceiptStatus::Accepted
    );
    for _ in 0..2000 {
        if let QueryData::CommandStatus {
            stage,
            result,
            error,
            ..
        } = hub
            .query(caller, Query::CommandStatus { command_id: id })
            .await
            .unwrap()
        {
            assert!(error.is_none(), "lease request failed: {error:?}");
            if stage == Some(CommandStage::Observed)
                && let Some(result) = result
                && let CommandResult::LeaseGranted { lease } = *result
            {
                return lease;
            }
        }
        tokio::task::yield_now().await;
    }
    panic!("fixture lease did not settle");
}

fn motion(lease: &LeaseView) -> InputEnvelope {
    InputEnvelope {
        lease_id: lease.lease_id,
        generation: lease.generation,
        geometry_revision: 1,
        seq: 1,
        action: InputAction::PointerMove {
            to: TargetPoint { x: 0.5, y: 0.5 },
        },
    }
}

#[tokio::test]
async fn revoke_acknowledgement_fences_queued_native_actions() {
    let directory = TestDirectory::new();
    let desktop = Arc::new(Desktop::new(true));
    let hub = CompanionHub::builder()
        .data_dir(&directory.0)
        .clock(Arc::new(FixedClock(Instant::now())))
        .desktop(desktop.clone())
        .build()
        .await
        .unwrap();
    let caller = enroll_fixture(&hub).await;
    let lease = lease(&hub, &caller, desktop.target.target_id).await;
    hub.input(&caller, motion(&lease)).await;
    let QueryData::Snapshot(snapshot) = hub
        .query(
            &caller,
            Query::Snapshot {
                reads_withheld: None,
            },
        )
        .await
        .unwrap()
    else {
        panic!("expected snapshot");
    };
    let acknowledged = snapshot
        .control
        .iter()
        .find(|v| v.target_id == desktop.target.target_id)
        .and_then(|v| v.lease.as_ref())
        .unwrap()
        .state;
    assert!(desktop.queue.released.load(Ordering::SeqCst) > 0);
    assert_eq!(desktop.queue.applied.load(Ordering::SeqCst), 0);
    desktop.queue.deliver.add_permits(1);
    desktop.queue.completed.acquire().await.unwrap().forget();
    let late_actions = desktop.queue.applied.load(Ordering::SeqCst);
    hub.shutdown().await.unwrap();
    assert!(
        acknowledged == LeaseState::Revoked && late_actions == 0,
        "lease was {acknowledged:?}, but {late_actions} uninjected actions ran afterward"
    );
}

#[tokio::test]
async fn input_with_lost_acknowledgement_is_reported_as_unknown() {
    let directory = TestDirectory::new();
    let desktop = Arc::new(Desktop::new(false));
    let hub = CompanionHub::builder()
        .data_dir(&directory.0)
        .clock(Arc::new(FixedClock(Instant::now())))
        .desktop(desktop.clone())
        .build()
        .await
        .unwrap();
    let caller = enroll_fixture(&hub).await;
    let lease = lease(&hub, &caller, desktop.target.target_id).await;
    let ack = hub.input(&caller, motion(&lease)).await;
    assert_eq!(desktop.queue.applied.load(Ordering::SeqCst), 1);
    let outcome = serde_json::to_value(ack).unwrap()["outcome"].clone();
    hub.shutdown().await.unwrap();
    assert_eq!(
        outcome, "unknown",
        "an applied action cannot be reported as a definite failure"
    );
}

#[tokio::test]
async fn interrupted_upload_publication_has_a_reconciliation_state() {
    let directory = TestDirectory::new();
    let id = TransferId::new();
    let record = TransferRecord {
        transfer_id: id,
        shown_in_task: None,
        direction: TransferDirection::PhoneToHost,
        name: "fixture.txt".into(),
        size: 0,
        sha256: Some("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".into()),
        transferred: 0,
        state: TransferState::Verifying,
        destination: Some(TransferDestination::Inbox),
        destination_folder: None,
        publication: None,
        host_path: None,
        created_at: Timestamp(0),
        updated_at: Timestamp(0),
    };
    // Seed the exact persisted publication crash window: final file linked,
    // publication_started committed, completion transaction not committed.
    let inbox = directory.0.join("inbox");
    std::fs::create_dir_all(&inbox).unwrap();
    std::fs::write(inbox.join("fixture.txt"), []).unwrap();
    let mut store = store::Store::open(
        directory.0.join("companion.sqlite"),
        store::Retention::default(),
    )
    .unwrap();
    store
        .write(Timestamp(0), |tx| {
            tx.put("transfer", id, &record)?;
            tx.put("publication_started", id, &true)?;
            Ok(())
        })
        .unwrap();
    drop(store);
    let hub = CompanionHub::builder()
        .data_dir(&directory.0)
        .clock(Arc::new(FixedClock(Instant::now())))
        .build()
        .await
        .unwrap();
    let QueryData::Transfers { transfers } = hub
        .query(&Caller::local_owner(), Query::Transfers)
        .await
        .unwrap()
    else {
        panic!("expected transfers");
    };
    let resume_error = match hub.upload(&Caller::local_owner(), id).await {
        Ok(_) => panic!("publication must not be replayed"),
        Err(error) => error,
    };
    assert_eq!(resume_error.code, ErrorCode::OutcomeUnknown);
    assert!(inbox.join("fixture.txt").exists());
    let state = serde_json::to_value(&transfers[0].state).unwrap();
    assert_eq!(
        transfers[0].publication.as_ref().unwrap().root,
        inbox.to_string_lossy()
    );
    assert_eq!(
        transfers[0].publication.as_ref().unwrap().path,
        "fixture.txt"
    );
    hub.cancel_transfer(&Caller::local_owner(), id)
        .await
        .unwrap();
    assert_eq!(
        std::fs::read(inbox.join("fixture.txt")).unwrap(),
        Vec::<u8>::new()
    );
    let QueryData::Transfers {
        transfers: cancelled,
    } = hub
        .query(&Caller::local_owner(), Query::Transfers)
        .await
        .unwrap()
    else {
        panic!("transfers");
    };
    assert_eq!(cancelled[0].state, TransferState::Cancelled);
    assert_eq!(cancelled[0].publication, transfers[0].publication);
    assert!(hub.upload(&Caller::local_owner(), id).await.is_err());
    hub.shutdown().await.unwrap();
    assert_eq!(
        state["state"], "reconciliation_required",
        "paused promises resumability, but resume returns outcome_unknown"
    );
}

async fn command_error(
    hub: &CompanionHub,
    caller: &Caller,
    body: Command,
) -> Option<protocol::ProtocolError> {
    let command = CommandEnvelope::new(body);
    let id = command.command_id;
    let receipt = hub.submit(caller, command).await;
    if receipt.status == ReceiptStatus::Rejected {
        return receipt.error;
    }
    for _ in 0..2000 {
        if let QueryData::CommandStatus { stage, error, .. } = hub
            .query(caller, Query::CommandStatus { command_id: id })
            .await
            .unwrap()
        {
            if error.is_some() {
                return error.map(|e| *e);
            }
            if stage == Some(CommandStage::Observed) {
                return None;
            }
        }
        tokio::task::yield_now().await;
    }
    panic!("command did not settle")
}

#[tokio::test]
async fn failed_barrier_refuses_both_human_admissions_until_pause_recovers() {
    let directory = TestDirectory::new();
    let desktop = Arc::new(Desktop::new(false));
    let hub = CompanionHub::builder()
        .data_dir(&directory.0)
        .clock(Arc::new(FixedClock(Instant::now())))
        .desktop(desktop.clone())
        .build()
        .await
        .unwrap();
    let caller = enroll_fixture(&hub).await;
    let target_id = desktop.target.target_id;
    let active = lease(&hub, &caller, target_id).await;
    desktop.queue.fail_barrier.store(true, Ordering::SeqCst);
    hub.input(&caller, motion(&active)).await;
    let QueryData::Snapshot(snapshot) = hub
        .query(
            &caller,
            Query::Snapshot {
                reads_withheld: None,
            },
        )
        .await
        .unwrap()
    else {
        panic!("snapshot");
    };
    assert_eq!(
        snapshot.control[0].input_state,
        protocol::control::InputState::Unknown
    );
    assert_eq!(
        snapshot.control[0].lease.as_ref().unwrap().state,
        LeaseState::Unknown
    );
    for command in [
        Command::RequestHumanLease {
            target_id,
            geometry_revision: 1,
        },
        Command::TakeOver {
            target_id,
            geometry_revision: 1,
        },
    ] {
        let error = command_error(&hub, &caller, command).await.unwrap();
        assert_eq!(error.code, ErrorCode::OutcomeUnknown);
        assert!(error.message.contains("Pause agent"));
    }
    assert_eq!(
        command_error(&hub, &caller, Command::PauseInput { target_id })
            .await
            .unwrap()
            .code,
        ErrorCode::OutcomeUnknown
    );
    desktop.queue.fail_barrier.store(false, Ordering::SeqCst);
    assert!(
        command_error(&hub, &caller, Command::PauseInput { target_id })
            .await
            .is_none()
    );
    let next = lease(&hub, &caller, target_id).await;
    assert!(next.generation > active.generation);
    assert_eq!(next.state, LeaseState::Active);
    hub.shutdown().await.unwrap();
}

#[tokio::test]
async fn interrupted_transfers_without_publication_remain_resumable() {
    let directory = TestDirectory::new();
    let mut store = store::Store::open(
        directory.0.join("companion.sqlite"),
        store::Retention::default(),
    )
    .unwrap();
    for state in [
        TransferState::Pending,
        TransferState::InProgress,
        TransferState::Verifying,
    ] {
        let t = TransferRecord {
            transfer_id: TransferId::new(),
            shown_in_task: None,
            direction: TransferDirection::PhoneToHost,
            name: "fixture.txt".into(),
            size: 0,
            sha256: Some("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".into()),
            transferred: 0,
            state,
            destination: Some(TransferDestination::Inbox),
            destination_folder: None,
            publication: None,
            host_path: None,
            created_at: Timestamp(0),
            updated_at: Timestamp(0),
        };
        store
            .write(Timestamp(0), |tx| tx.put("transfer", t.transfer_id, &t))
            .unwrap();
    }
    drop(store);
    let hub = CompanionHub::builder()
        .data_dir(&directory.0)
        .clock(Arc::new(FixedClock(Instant::now())))
        .build()
        .await
        .unwrap();
    let QueryData::Transfers { transfers } = hub
        .query(&Caller::local_owner(), Query::Transfers)
        .await
        .unwrap()
    else {
        panic!("transfers");
    };
    assert_eq!(transfers.len(), 3);
    for t in transfers {
        assert_eq!(t.state, TransferState::Paused);
        assert!(
            hub.upload(&Caller::local_owner(), t.transfer_id)
                .await
                .is_ok()
        );
    }
    hub.shutdown().await.unwrap();
}
