// SPDX-License-Identifier: Apache-2.0
use crate::preview;
use crate::{
    api::{Backlog, ChannelLinkOutcome, ChannelLinkRequest, LINK_CODE_MINUTES, Outbound},
    testing::*,
    *,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use protocol::{
    auth::*, capability::HarnessKind, command::*, control::*, model::*, queue::*, time::Timestamp,
    transfer::*, *,
};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, atomic::Ordering},
    time::Duration,
};

/// Every file under `dir`, read whole, for a value that must not be in any.
fn written_anywhere(dir: &std::path::Path, value: &str) -> bool {
    std::fs::read_dir(dir).unwrap().flatten().any(|entry| {
        let path = entry.path();
        if path.is_dir() {
            written_anywhere(&path, value)
        } else {
            std::fs::read(&path)
                .unwrap_or_default()
                .windows(value.len())
                .any(|w| w == value.as_bytes())
        }
    })
}

#[path = "access_tests.rs"]
mod conversation_access;
#[path = "readiness_tests.rs"]
mod readiness;

#[path = "desktop_unlock_tests.rs"]
mod desktop_unlock;

#[path = "upgrade_tests.rs"]
mod upgrade;

#[path = "device_tests.rs"]
mod devices;

#[path = "approval_tests.rs"]
mod approvals;

#[path = "restart_recovery_tests.rs"]
mod restart_recovery;

#[path = "artifact_access_tests.rs"]
mod artifact_access;

#[path = "transfer_limits_tests.rs"]
mod transfer_limits;

#[path = "project_folder_tests.rs"]
mod project_folder;

#[path = "computer_use_grant_tests.rs"]
mod computer_use_grant;

#[path = "history_tests.rs"]
mod history;

#[path = "alerts_tests.rs"]
mod alerts;

#[path = "accounts_tests.rs"]
mod accounts_tests;

#[path = "agent_logins_tests.rs"]
mod agent_logins_tests;

#[path = "launchers_tests.rs"]
mod launchers_tests;

#[path = "github_tests.rs"]
mod github_tests;

#[path = "agent_updates_tests.rs"]
mod agent_updates_tests;

#[path = "system_hub_tests.rs"]
mod system_hub;

#[path = "found_previews_tests.rs"]
mod found_previews;

#[path = "shown_tests.rs"]
mod shown;

#[path = "scratch_tests.rs"]
mod scratch;

#[path = "expiry_tests.rs"]
mod expiry;

#[path = "busy_wake_tests.rs"]
mod busy_wakes;

struct Fixture {
    path: PathBuf,
    hub: CompanionHub,
    clock: Arc<FakeClock>,
    harness: Arc<FakeHarness>,
    desktop: Arc<FakeDesktop>,
}
impl Fixture {
    async fn new() -> Self {
        Self::with(|builder| builder).await
    }
    async fn with(configure: impl FnOnce(HubBuilder) -> HubBuilder) -> Self {
        // Absolute even where the build named this crate's folder for another
        // system, as a Windows test built on Linux does.
        let path = std::path::absolute(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../target/service-tests")
                .join(uuid::Uuid::now_v7().to_string()),
        )
        .unwrap();
        let clock = Arc::new(FakeClock::default());
        let harness = Arc::new(FakeHarness::default());
        let desktop = Arc::new(FakeDesktop::default());
        let hub = Self::build(&path, &clock, &harness, &desktop, configure).await;
        Self {
            path,
            hub,
            clock,
            harness,
            desktop,
        }
    }
    async fn build(
        path: &Path,
        clock: &Arc<FakeClock>,
        harness: &Arc<FakeHarness>,
        desktop: &Arc<FakeDesktop>,
        configure: impl FnOnce(HubBuilder) -> HubBuilder,
    ) -> CompanionHub {
        configure(
            CompanionHub::builder()
                .data_dir(path)
                .inbox_dir(path.with_extension("received"))
                .clock(clock.clone())
                .harness(harness.clone())
                .desktop(desktop.clone())
                .media(Arc::new(FakeMedia))
                .terminal(Arc::new(FakeTerminal::default()))
                .speech(Arc::new(FakeSpeech))
                .bridge(bridge::BridgeConfig::new(
                    "synthetic-bridge",
                    "synthetic-socket",
                )),
        )
        .build()
        .await
        .unwrap()
    }
    /// The companion stopped and started again with what it keeps.
    async fn reopened(self, configure: impl FnOnce(HubBuilder) -> HubBuilder) -> Self {
        let Self {
            path,
            hub,
            clock,
            harness,
            desktop,
        } = self;
        hub.shutdown().await.unwrap();
        drop(hub);
        let hub = Self::build(&path, &clock, &harness, &desktop, configure).await;
        Self {
            path,
            hub,
            clock,
            harness,
            desktop,
        }
    }
    async fn device(&self) -> Caller {
        pair(&self.hub, 1).await
    }
    /// The project's folder, beside the data folder; no project may overlap it.
    fn project_dir(&self) -> PathBuf {
        self.path.with_extension("project")
    }
    async fn project(&self) -> ProjectId {
        std::fs::create_dir_all(self.project_dir()).unwrap();
        let r = self
            .hub
            .submit(
                &Caller::local_owner(),
                CommandEnvelope::new(Command::AddProject {
                    host_path: self.project_dir().to_string_lossy().into_owned(),
                    name: Some("Test".into()),
                }),
            )
            .await;
        match r.result.unwrap() {
            CommandResult::ProjectAdded { project_id } => project_id,
            _ => panic!("project"),
        }
    }
    async fn task(
        &self,
        project_id: ProjectId,
        placement: TaskPlacement,
        caller: &Caller,
        computer: bool,
    ) -> (TaskId, CommandEnvelope) {
        let command = CommandEnvelope::new(Command::CreateTask {
            github: None,
            agent_chosen: None,
            mode: None,
            project_id,
            kind: if computer {
                TaskKind::ComputerUse
            } else {
                TaskKind::Coding
            },
            title: Some("Test task".into()),
            prompt: "test".into(),
            bundle: None,
            harness: HarnessKind::ClaudeCode,
            model: None,
            effort: None,
            launch_policy: None,
            branch: None,
            mission: None,
            target_id: computer.then(|| self.desktop.target.lock().unwrap().target_id),
            placement,
            start: None,
            place: None,
            browser: None,
            conversation: None,
            access: None,
            review: None,
        });
        let r = self.hub.submit(caller, command.clone()).await;
        assert_eq!(r.status, ReceiptStatus::Accepted, "{r:?}");
        match r.result.unwrap() {
            CommandResult::TaskCreated { task_id, .. } => (task_id, command),
            _ => panic!("task"),
        }
    }
    async fn close(self) {
        let project = self.project_dir();
        self.hub.shutdown().await.unwrap();
        drop(self.hub);
        tokio::task::yield_now().await;
        if project.exists() {
            std::fs::remove_dir_all(project).unwrap();
        }
        std::fs::remove_dir_all(self.path).unwrap();
    }
}
async fn pair(hub: &CompanionHub, key: u8) -> Caller {
    let ticket = hub.open_pairing(&Caller::local_owner()).await.unwrap();
    let response = crate::testing::pair_confirmed(
        hub,
        PairRequest {
            pairing_secret: ticket.secret,
            device_public_key: URL_SAFE_NO_PAD.encode([key; 32]),
            device_name: "Phone".into(),
            device_platform: "android".into(),
            client_version: "test".into(),
            protocol: ProtocolVersion::CURRENT,
            shows_code: true,
            replaces: None,
        },
    )
    .await
    .unwrap();
    Caller {
        principal: Principal::Device {
            device_id: response.device_id,
        },
        grants: response.grants,
    }
}
/// Yields until `WAIT` of wall time has passed. A fixed yield count runs out
/// early on a loaded machine, where blocking store work is slower to finish.
fn waiting() -> impl FnMut() -> bool {
    let deadline = std::time::Instant::now() + WAIT;
    move || std::time::Instant::now() < deadline
}
const WAIT: Duration = Duration::from_secs(10);
async fn settle(hub: &CompanionHub, id: CommandId) {
    // Executor yields are not a guarantee that the blocking journal worker ran.
    // Keep the completion assertion, with the same wall-clock guard as run_started.
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if hub
                .read(|r| Ok(r.command(id)?.is_some_and(|c| !c.pending)))
                .unwrap()
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("command did not settle");
}
async fn run_started(f: &Fixture, task_id: TaskId) -> RunId {
    let task: Task = f.hub.read(|r| r.require("task", task_id)).unwrap();
    let id = task.current_run.unwrap();
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if f.hub.inner.runs.lock().await.contains_key(&id) {
                return id;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("run did not spawn")
}

fn edit(command: Command, revision: u64) -> CommandEnvelope {
    let mut c = CommandEnvelope::new(command);
    c.expected_revision = Some(revision);
    c
}
fn hash(bytes: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[tokio::test]
async fn admission_deduplicates_and_recovers_intent_before_dispatch() {
    let f = Fixture::new().await;
    let project = f.project().await;
    // A current-thread executor cannot poll the spawned dispatcher before this
    // test yields: this is the intent-commit / dispatch crash window.
    let (task, c) = f
        .task(
            project,
            TaskPlacement::StartNow,
            &Caller::local_owner(),
            false,
        )
        .await;
    assert_eq!(f.harness.starts.load(Ordering::SeqCst), 0);
    let r = f.hub.submit(&Caller::local_owner(), c.clone()).await;
    assert_eq!(r.status, ReceiptStatus::Duplicate);
    let mut changed = c.clone();
    if let Command::CreateTask { prompt, .. } = &mut changed.body {
        *prompt = "different".into();
    }
    assert_eq!(
        f.hub
            .submit(&Caller::local_owner(), changed)
            .await
            .error
            .unwrap()
            .code,
        ErrorCode::Conflict
    );
    f.hub.shutdown().await.unwrap();
    drop(f.hub);
    let hub = CompanionHub::builder()
        .data_dir(&f.path)
        .clock(f.clock)
        .harness(f.harness.clone())
        .build()
        .await
        .unwrap();
    assert_eq!(
        hub.submit(&Caller::local_owner(), c).await.status,
        ReceiptStatus::Duplicate
    );
    let t: Task = hub.read(|r| r.require("task", task)).unwrap();
    assert_eq!(t.state, TaskState::ReconciliationRequired);
    assert_eq!(f.harness.starts.load(Ordering::SeqCst), 0);
    hub.shutdown().await.unwrap();
    drop(hub);
    std::fs::remove_dir_all(f.path).unwrap();
}
#[tokio::test]
async fn dispatch_without_native_ack_is_never_retried() {
    let f = Fixture::new().await;
    let project = f.project().await;
    let (task, _) = f
        .task(
            project,
            TaskPlacement::StartNow,
            &Caller::local_owner(),
            false,
        )
        .await;
    run_started(&f, task).await;
    f.hub.shutdown().await.unwrap();
    drop(f.hub);
    let hub = CompanionHub::builder()
        .data_dir(&f.path)
        .harness(f.harness.clone())
        .build()
        .await
        .unwrap();
    let t: Task = hub.read(|r| r.require("task", task)).unwrap();
    assert_eq!(t.state, TaskState::ReconciliationRequired);
    assert_eq!(f.harness.starts.load(Ordering::SeqCst), 1);
    hub.shutdown().await.unwrap();
    drop(hub);
    std::fs::remove_dir_all(f.path).unwrap();
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_journal_subscribe_has_no_gap_or_duplicate() {
    let f = Fixture::new().await;
    let before = f.hub.journal_head();
    let hub = f.hub.clone();
    let writer = tokio::spawn(async move {
        for i in 0..200 {
            hub.write(|tx| {
                tx.put("stress", i, &i)?;
                tx.emit(Event::BootEpochChanged {
                    boot_epoch: hub.boot_epoch(),
                })?;
                Ok(())
            })
            .unwrap();
            tokio::task::yield_now().await;
        }
    });
    let mut sub = f
        .hub
        .subscribe(&Caller::local_owner(), Some(before))
        .await
        .unwrap();
    writer.await.unwrap();
    let final_head = f.hub.journal_head();
    let mut seqs = match sub.backlog {
        Backlog::Events(e) => e.into_iter().map(|e| e.seq).collect::<Vec<_>>(),
        _ => panic!("unexpected gap"),
    };
    while let Ok(out) = sub.live.try_recv() {
        if let Outbound::Event(e) = out
            && e.seq > sub.head
        {
            seqs.push(e.seq);
        }
    }
    assert_eq!(seqs, ((before + 1)..=final_head).collect::<Vec<_>>());
    let snapshot = f
        .hub
        .query(
            &Caller::local_owner(),
            Query::Snapshot {
                reads_withheld: None,
            },
        )
        .await
        .unwrap();
    if let QueryData::Snapshot(s) = snapshot {
        assert_eq!(s.journal_head, final_head);
    } else {
        panic!("snapshot");
    }
    f.close().await;
}
fn pair_request(secret: &str, key: u8) -> PairRequest {
    PairRequest {
        pairing_secret: secret.into(),
        device_public_key: URL_SAFE_NO_PAD.encode([key; 32]),
        device_name: "Test".into(),
        device_platform: "android".into(),
        client_version: "test".into(),
        protocol: ProtocolVersion::CURRENT,
        shows_code: true,
        replaces: None,
    }
}
async fn devices(hub: &CompanionHub) -> Vec<DeviceRecord> {
    match hub
        .query(&Caller::local_owner(), Query::Devices)
        .await
        .unwrap()
    {
        QueryData::Devices { devices, .. } => devices,
        _ => panic!("devices"),
    }
}
#[tokio::test]
async fn a_pairing_request_enrolls_only_once_the_owner_confirms_its_code() {
    let f = Fixture::new().await;
    let owner = Caller::local_owner();
    let ticket = f.hub.open_pairing(&owner).await.unwrap();
    // A name or platform that could rewrite the owner's terminal is refused.
    for (name, platform) in [
        ("Phone\u{1b}[1A\u{1b}[2K", "android"),
        ("Phone", "android\r"),
    ] {
        let refused = PairRequest {
            device_name: name.into(),
            device_platform: platform.into(),
            ..pair_request(&ticket.secret, 9)
        };
        assert_eq!(
            f.hub
                .request_pairing(refused, "123456".into())
                .await
                .unwrap_err()
                .code,
            ErrorCode::InvalidRequest
        );
    }
    let wrong = pair_request("not-the-secret", 9);
    assert_eq!(
        f.hub
            .request_pairing(wrong, "123456".into())
            .await
            .unwrap_err()
            .code,
        ErrorCode::Unauthenticated
    );
    let claim = f
        .hub
        .request_pairing(pair_request(&ticket.secret, 9), "123456".into())
        .await
        .unwrap();
    let prompt = f.hub.waiting_pairing(&owner).await.unwrap().unwrap();
    assert_eq!(
        (
            prompt.device_name.as_str(),
            prompt.code.as_str(),
            prompt.shows_code
        ),
        ("Test", "123456", true)
    );
    assert!(
        devices(&f.hub).await.is_empty(),
        "nothing enrolls by itself"
    );
    // Only the local owner answers, and only with the code the device shows.
    let phone = Caller {
        principal: Principal::Device {
            device_id: DeviceId::new(),
        },
        grants: Grants::owner(),
    };
    assert_eq!(
        f.hub
            .answer_pairing(&phone, "123456", true)
            .await
            .unwrap_err()
            .code,
        ErrorCode::PermissionDenied
    );
    assert_eq!(
        f.hub
            .answer_pairing(&owner, "654321", true)
            .await
            .unwrap_err()
            .code,
        ErrorCode::NotFound
    );
    let record = f
        .hub
        .answer_pairing(&owner, "123 456", true)
        .await
        .unwrap()
        .unwrap();
    let response = claim.answer.await.unwrap().unwrap();
    assert_eq!(response.device_id, record.device_id);
    assert!(f.hub.waiting_pairing(&owner).await.unwrap().is_none());
    // The window closed with the enrollment.
    assert_eq!(
        f.hub
            .request_pairing(pair_request(&ticket.secret, 8), "111111".into())
            .await
            .unwrap_err()
            .code,
        ErrorCode::Unauthenticated
    );
    f.close().await;
}
#[tokio::test]
async fn a_refused_request_enrolls_nothing_and_closes_the_window() {
    let f = Fixture::new().await;
    let owner = Caller::local_owner();
    let ticket = f.hub.open_pairing(&owner).await.unwrap();
    let claim = f
        .hub
        .request_pairing(pair_request(&ticket.secret, 9), "123456".into())
        .await
        .unwrap();
    assert!(
        f.hub
            .answer_pairing(&owner, "123456", false)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        claim.answer.await.unwrap().unwrap_err().code,
        ErrorCode::PermissionDenied
    );
    assert!(devices(&f.hub).await.is_empty());
    assert_eq!(
        f.hub
            .request_pairing(pair_request(&ticket.secret, 8), "111111".into())
            .await
            .unwrap_err()
            .code,
        ErrorCode::Unauthenticated
    );
    // A new window ends a request made with the old one.
    let ticket = f.hub.open_pairing(&owner).await.unwrap();
    let claim = f
        .hub
        .request_pairing(pair_request(&ticket.secret, 9), "123456".into())
        .await
        .unwrap();
    f.hub.open_pairing(&owner).await.unwrap();
    assert!(f.hub.waiting_pairing(&owner).await.unwrap().is_none());
    assert_eq!(
        claim.answer.await.unwrap().unwrap_err().code,
        ErrorCode::Expired
    );
    f.close().await;
}
#[tokio::test]
async fn pairing_again_in_place_of_an_earlier_device_moves_its_permissions_once_the_owner_confirms()
{
    let f = Fixture::new().await;
    let owner = Caller::local_owner();
    let id = |caller: &Caller| match caller.principal {
        Principal::Device { device_id } => device_id,
        _ => unreachable!(),
    };
    let active = |list: &[DeviceRecord], device: DeviceId| {
        list.iter()
            .any(|d| d.device_id == device && d.revoked_at.is_none())
    };
    // The first device manages the computer; the second has the defaults.
    let first = pair(&f.hub, 1).await;
    assert!(first.grants.admin);
    let other = pair(&f.hub, 2).await;
    assert!(!other.grants.admin);
    // A request without the marker changes nothing about earlier devices.
    let ticket = f.hub.open_pairing(&owner).await.unwrap();
    let plain = crate::testing::pair_confirmed(&f.hub, pair_request(&ticket.secret, 3))
        .await
        .unwrap();
    assert!(!plain.grants.admin);
    assert!(active(&devices(&f.hub).await, id(&first)));
    // Nor does one naming a device this computer does not have.
    let ticket = f.hub.open_pairing(&owner).await.unwrap();
    let unknown = PairRequest {
        replaces: Some(DeviceId::new()),
        ..pair_request(&ticket.secret, 4)
    };
    let claim = f
        .hub
        .request_pairing(unknown, "111222".into())
        .await
        .unwrap();
    let prompt = f.hub.waiting_pairing(&owner).await.unwrap().unwrap();
    assert_eq!(prompt.replaces, None);
    f.hub.answer_pairing(&owner, "111222", true).await.unwrap();
    assert!(!claim.answer.await.unwrap().unwrap().grants.admin);
    // A phone's claim alone moves nothing: the owner is told which device
    // it replaces, and refusing leaves that device as it was.
    let ticket = f.hub.open_pairing(&owner).await.unwrap();
    let request = PairRequest {
        replaces: Some(id(&first)),
        ..pair_request(&ticket.secret, 5)
    };
    let claim = f
        .hub
        .request_pairing(request.clone(), "222333".into())
        .await
        .unwrap();
    let named = f
        .hub
        .waiting_pairing(&owner)
        .await
        .unwrap()
        .unwrap()
        .replaces
        .expect("the owner is told which device this replaces");
    assert_eq!(
        (named.device_id, named.name.as_str()),
        (id(&first), "Phone")
    );
    assert!(named.grants.admin);
    assert!(active(&devices(&f.hub).await, id(&first)));
    f.hub.answer_pairing(&owner, "222333", false).await.unwrap();
    assert!(claim.answer.await.unwrap().is_err());
    let list = devices(&f.hub).await;
    assert!(active(&list, id(&first)));
    assert_eq!(list.len(), 4);
    // Confirmed, the new device takes over what the earlier one could do,
    // and the earlier one loses access in the same step.
    let ticket = f.hub.open_pairing(&owner).await.unwrap();
    let claim = f
        .hub
        .request_pairing(
            PairRequest {
                pairing_secret: ticket.secret.clone(),
                ..request
            },
            "444555".into(),
        )
        .await
        .unwrap();
    f.hub.answer_pairing(&owner, "444555", true).await.unwrap();
    let response = claim.answer.await.unwrap().unwrap();
    assert_eq!(response.grants, first.grants);
    let list = devices(&f.hub).await;
    assert!(!active(&list, id(&first)));
    assert!(active(&list, response.device_id));
    assert_eq!(
        f.hub.query(&first, Query::Devices).await.unwrap_err().code,
        ErrorCode::Unauthenticated
    );
    // A device already replaced cannot be replaced again.
    let ticket = f.hub.open_pairing(&owner).await.unwrap();
    let again = PairRequest {
        replaces: Some(id(&first)),
        ..pair_request(&ticket.secret, 6)
    };
    let claim = f.hub.request_pairing(again, "666777".into()).await.unwrap();
    assert_eq!(
        f.hub
            .waiting_pairing(&owner)
            .await
            .unwrap()
            .unwrap()
            .replaces,
        None
    );
    f.hub.answer_pairing(&owner, "666777", true).await.unwrap();
    assert!(!claim.answer.await.unwrap().unwrap().grants.admin);
    assert!(active(&devices(&f.hub).await, response.device_id));
    f.close().await;
}
#[tokio::test]
async fn one_request_waits_at_a_time_and_a_withdrawn_one_frees_the_window() {
    let f = Fixture::new().await;
    let owner = Caller::local_owner();
    let ticket = f.hub.open_pairing(&owner).await.unwrap();
    let first = f
        .hub
        .request_pairing(pair_request(&ticket.secret, 9), "123456".into())
        .await
        .unwrap();
    assert_eq!(
        f.hub
            .request_pairing(pair_request(&ticket.secret, 8), "654321".into())
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    assert_eq!(
        f.hub.waiting_pairing(&owner).await.unwrap().unwrap().code,
        "123456"
    );
    // The phone stops waiting: its request is gone and cannot be confirmed.
    drop(first);
    assert!(f.hub.waiting_pairing(&owner).await.unwrap().is_none());
    assert_eq!(
        f.hub
            .answer_pairing(&owner, "123456", true)
            .await
            .unwrap_err()
            .code,
        ErrorCode::NotFound
    );
    assert!(devices(&f.hub).await.is_empty());
    // An app from before codes reaches the owner, but is never paired: its
    // name alone proves nothing. It is told to update, and the window stays
    // open for it to try again.
    let old = PairRequest {
        shows_code: false,
        replaces: None,
        ..pair_request(&ticket.secret, 8)
    };
    let claim = f.hub.request_pairing(old, "654321".into()).await.unwrap();
    assert!(
        !f.hub
            .waiting_pairing(&owner)
            .await
            .unwrap()
            .unwrap()
            .shows_code
    );
    let refused = f
        .hub
        .answer_pairing(&owner, "654321", true)
        .await
        .unwrap_err();
    assert_eq!(refused.code, ErrorCode::IncompatibleVersion);
    assert_eq!(
        refused.message,
        "This device's app is too old to pair safely. Update Øphio on the device and try again."
    );
    let told = claim.answer.await.unwrap().unwrap_err();
    assert_eq!(told.code, ErrorCode::IncompatibleVersion);
    assert_eq!(
        told.message,
        "This app is too old to pair safely. Update Øphio and try again."
    );
    assert!(f.hub.waiting_pairing(&owner).await.unwrap().is_none());
    assert!(devices(&f.hub).await.is_empty());
    let updated = f
        .hub
        .request_pairing(pair_request(&ticket.secret, 8), "654321".into())
        .await
        .unwrap();
    f.hub
        .answer_pairing(&owner, "654321", true)
        .await
        .unwrap()
        .unwrap();
    // With no device yet that can manage the computer, the first one can.
    assert!(updated.answer.await.unwrap().unwrap().grants.admin);
    let second = pair(&f.hub, 7).await;
    assert!(second.grants.tasks);
    assert!(!second.grants.admin);
    assert!(!second.grants.control_input);
    f.close().await;
}
#[tokio::test]
async fn an_unanswered_request_lapses_with_its_window() {
    let f = Fixture::new().await;
    let owner = Caller::local_owner();
    let ticket = f.hub.open_pairing(&owner).await.unwrap();
    let _claim = f
        .hub
        .request_pairing(pair_request(&ticket.secret, 9), "123456".into())
        .await
        .unwrap();
    f.clock.advance(Duration::from_secs(301));
    assert!(f.hub.waiting_pairing(&owner).await.unwrap().is_none());
    assert_eq!(
        f.hub
            .answer_pairing(&owner, "123456", true)
            .await
            .unwrap_err()
            .code,
        ErrorCode::NotFound
    );
    assert_eq!(
        f.hub
            .request_pairing(pair_request(&ticket.secret, 8), "654321".into())
            .await
            .unwrap_err()
            .code,
        ErrorCode::Expired
    );
    assert!(devices(&f.hub).await.is_empty());
    f.close().await;
}
#[tokio::test]
async fn a_long_chat_name_is_shortened_not_refused() {
    let f = Fixture::new().await;
    let request = |display_name: String| ChannelLinkRequest {
        channel: ChannelKind::Telegram,
        external_user: "7".into(),
        external_chat: "42".into(),
        display_name,
    };
    // 202 bytes; the cut for the ellipsis falls inside a four-byte letter.
    let code = f
        .hub
        .request_channel_link(request(format!("ab{}", "𝓢".repeat(50))))
        .await
        .unwrap();
    let link: identity::PendingLink = f.hub.read(|r| r.require("link", &code)).unwrap();
    assert_eq!(link.name, format!("ab{}…", "𝓢".repeat(48)));
    assert_eq!(link.name.len(), 197);
    f.close().await;
}
#[tokio::test]
async fn expired_link_codes_are_pruned_when_a_code_is_requested_or_checked() {
    let f = Fixture::new().await;
    let owner = Caller::local_owner();
    let request = |user: &str| ChannelLinkRequest {
        channel: ChannelKind::Telegram,
        external_user: user.into(),
        external_chat: "chat".into(),
        display_name: "Chat user".into(),
    };
    let pending = || {
        f.hub
            .read(|r| r.entries::<identity::PendingLink>("link"))
            .unwrap()
            .into_iter()
            .map(|(code, _)| code)
            .collect::<Vec<_>>()
    };
    let check = |code: &str| {
        f.hub.submit(
            &owner,
            CommandEnvelope::new(Command::ApproveChannelLink {
                link_code: code.into(),
            }),
        )
    };
    let lifetime = Duration::from_secs(LINK_CODE_MINUTES * 60);

    // A new request clears codes that ran out and keeps the ones that did not.
    f.hub.request_channel_link(request("1")).await.unwrap();
    f.clock.advance(lifetime);
    let second = f.hub.request_channel_link(request("2")).await.unwrap();
    assert_eq!(pending(), [second.as_str()]);

    // Checking an expired code says so once, and the code is gone afterwards.
    f.clock.advance(lifetime);
    let r = check(&second).await;
    assert_eq!(r.error.map(|e| e.code), Some(ErrorCode::Expired));
    assert!(pending().is_empty());
    let r = check(&second).await;
    assert_eq!(r.error.map(|e| e.code), Some(ErrorCode::NotFound));

    // Checking any code clears every expired one and keeps the live ones.
    f.hub.request_channel_link(request("3")).await.unwrap();
    f.clock.advance(lifetime - Duration::from_secs(1));
    let fourth = f.hub.request_channel_link(request("4")).await.unwrap();
    f.clock.advance(Duration::from_secs(1));
    let r = check("UNKNOWN").await;
    assert_eq!(r.error.map(|e| e.code), Some(ErrorCode::NotFound));
    assert_eq!(pending(), [fourth.as_str()]);
    let r = check(&fourth).await;
    assert_eq!(r.status, ReceiptStatus::Accepted);
    f.close().await;
}
#[tokio::test]
async fn the_owner_sees_who_asked_for_a_link_code_and_the_bot_learns_the_decision() {
    let f = Fixture::new().await;
    let owner = Caller::local_owner();
    let project = ProjectId::new();
    f.hub
        .set_chat_projects(ChannelKind::Telegram, [("-42".into(), project)].into())
        .unwrap();
    let request = |user: &str| ChannelLinkRequest {
        channel: ChannelKind::Telegram,
        external_user: user.into(),
        external_chat: "-42".into(),
        display_name: "Owner Name".into(),
    };
    let review = |code: &str| {
        f.hub.query(
            &owner,
            Query::ChannelLink {
                link_code: code.into(),
            },
        )
    };

    // Two people in one group chat can show the same name; the ids tell them apart.
    let asked = f.hub.request_channel_link(request("7")).await.unwrap();
    let QueryData::ChannelLink { link } = review(&asked).await.unwrap() else {
        panic!("link review");
    };
    assert_eq!(
        link,
        PendingChannelLink {
            channel: ChannelKind::Telegram,
            external_user: "7".into(),
            external_chat: "-42".into(),
            display_name: "Owner Name".into(),
            project_id: Some(project),
            expires_at: Timestamp(
                ports::Clock::now(f.clock.as_ref()).0 + LINK_CODE_MINUTES as i64 * 60_000
            ),
        }
    );
    assert_eq!(
        f.hub
            .channel_link_outcome(ChannelKind::Telegram, &asked)
            .await,
        Some(ChannelLinkOutcome::Waiting)
    );
    assert_eq!(
        f.hub
            .channel_link_outcome(ChannelKind::Discord, &asked)
            .await,
        None
    );
    let r = f
        .hub
        .submit(
            &owner,
            CommandEnvelope::new(Command::ApproveChannelLink {
                link_code: asked.clone(),
            }),
        )
        .await;
    assert_eq!(r.status, ReceiptStatus::Accepted);
    assert_eq!(
        f.hub
            .channel_link_outcome(ChannelKind::Telegram, &asked)
            .await,
        Some(ChannelLinkOutcome::Approved)
    );
    assert_eq!(review(&asked).await.unwrap_err().code, ErrorCode::NotFound);

    let refused = f.hub.request_channel_link(request("8")).await.unwrap();
    let r = f
        .hub
        .submit(
            &owner,
            CommandEnvelope::new(Command::RefuseChannelLink {
                link_code: refused.clone(),
            }),
        )
        .await;
    assert_eq!(r.status, ReceiptStatus::Accepted);
    assert_eq!(
        f.hub
            .channel_link_outcome(ChannelKind::Telegram, &refused)
            .await,
        Some(ChannelLinkOutcome::Refused)
    );
    assert!(
        f.hub
            .channel_identity(ChannelKind::Telegram, "8", "-42")
            .await
            .is_none()
    );
    let r = f
        .hub
        .submit(
            &owner,
            CommandEnvelope::new(Command::ApproveChannelLink {
                link_code: refused.clone(),
            }),
        )
        .await;
    assert_eq!(r.error.map(|e| e.code), Some(ErrorCode::NotFound));

    // An expired code can't be reviewed, and its outcome is forgotten with it.
    let late = f.hub.request_channel_link(request("9")).await.unwrap();
    f.clock.advance(Duration::from_secs(LINK_CODE_MINUTES * 60));
    assert_eq!(review(&late).await.unwrap_err().code, ErrorCode::Expired);
    assert_eq!(
        f.hub
            .channel_link_outcome(ChannelKind::Telegram, &late)
            .await,
        None
    );
    assert_eq!(
        f.hub
            .channel_link_outcome(ChannelKind::Telegram, &refused)
            .await,
        None
    );
    f.hub.request_channel_link(request("10")).await.unwrap();
    assert!(
        f.hub
            .read(|r| r.entries::<identity::LinkOutcome>("link_outcome"))
            .unwrap()
            .is_empty()
    );
    f.close().await;
}
#[tokio::test]
async fn link_codes_are_limited_per_chat_and_overall_as_well_as_per_person() {
    let f = Fixture::new().await;
    let ask = |user: &str, chat: &str| {
        f.hub.request_channel_link(ChannelLinkRequest {
            channel: ChannelKind::Telegram,
            external_user: user.into(),
            external_chat: chat.into(),
            display_name: "Chat user".into(),
        })
    };
    let refused = |result: Result<String, ProtocolError>| {
        assert_eq!(result.unwrap_err().code, ErrorCode::LimitExceeded);
    };
    // Different people in one chat share its limit.
    for user in ["1", "2", "3"] {
        ask(user, "-42").await.unwrap();
    }
    refused(ask("4", "-42").await);
    // Other chats go on until all chats together reach theirs.
    for (user, chat) in (5..12).map(|i| (i.to_string(), format!("-{i}"))) {
        ask(&user, &chat).await.unwrap();
    }
    refused(ask("12", "-12").await);
    // A minute later each can ask again, and people who asked earlier are
    // no longer remembered for it.
    f.clock.advance(Duration::from_secs(60));
    ask("4", "-42").await.unwrap();
    ask("12", "-12").await.unwrap();
    let asked = f
        .hub
        .read(|r| r.entries::<Timestamp>("link_rate"))
        .unwrap()
        .into_iter()
        .map(|(person, _)| person)
        .collect::<Vec<_>>();
    assert_eq!(asked, ["Telegram:12", "Telegram:4"]);
    f.close().await;
}
#[tokio::test]
async fn leases_reject_stale_inputs_and_expire_on_fake_clock() {
    let f = Fixture::new().await;
    let c = f.device().await;
    let target = f.desktop.target.lock().unwrap().target_id;
    let command = CommandEnvelope::new(Command::RequestHumanLease {
        target_id: target,
        geometry_revision: 1,
    });
    let id = command.command_id;
    f.hub.submit(&c, command).await;
    settle(&f.hub, id).await;
    let view: TargetControlView = f.hub.read(|r| r.require("control", target)).unwrap();
    let lease = view.lease.unwrap();
    let mut input = InputEnvelope {
        lease_id: lease.lease_id,
        generation: lease.generation,
        geometry_revision: 1,
        seq: 1,
        action: InputAction::ReleaseAll,
    };
    assert_eq!(
        f.hub.input(&c, input.clone()).await.outcome,
        InputOutcome::Injected
    );
    assert_eq!(
        f.hub.input(&c, input.clone()).await.outcome,
        InputOutcome::OutOfOrder
    );
    input.seq = 2;
    input.generation += 1;
    assert_eq!(
        f.hub.input(&c, input.clone()).await.outcome,
        InputOutcome::StaleLease
    );
    input.generation -= 1;
    input.geometry_revision = 2;
    assert_eq!(
        f.hub.input(&c, input.clone()).await.outcome,
        InputOutcome::StaleGeometry
    );
    f.clock.advance(Duration::from_secs(4));
    f.hub.expire_leases().await.unwrap();
    input.geometry_revision = 1;
    assert_eq!(
        f.hub.input(&c, input).await.outcome,
        InputOutcome::StaleLease
    );
    assert_eq!(f.desktop.input.releases.load(Ordering::SeqCst), 1);
    assert!(
        f.hub
            .renew_lease(&c, lease.lease_id, lease.generation)
            .await
            .is_err()
    );
    f.close().await;
}
#[tokio::test]
async fn takeover_waits_for_release_and_private_step_without_native_run_is_ready() {
    let f = Fixture::new().await;
    let c = f.device().await;
    let target = f.desktop.target.lock().unwrap().target_id;
    let cmd = CommandEnvelope::new(Command::RequestHumanLease {
        target_id: target,
        geometry_revision: 1,
    });
    let id = cmd.command_id;
    f.hub.submit(&c, cmd).await;
    settle(&f.hub, id).await;
    f.desktop.input.hold_release.store(true, Ordering::SeqCst);
    let cmd = CommandEnvelope::new(Command::TakeOver {
        target_id: target,
        geometry_revision: 1,
    });
    let id = cmd.command_id;
    f.hub.submit(&c, cmd).await;
    let mut more = waiting();
    while more() {
        if f.desktop.input.releases.load(Ordering::SeqCst) > 0 {
            break;
        }
        tokio::task::yield_now().await;
    }
    let view: TargetControlView = f.hub.read(|r| r.require("control", target)).unwrap();
    assert_eq!(view.input_state, InputState::Revoking);
    f.desktop.input.hold_release.store(false, Ordering::SeqCst);
    f.desktop.input.release_gate.add_permits(1);
    settle(&f.hub, id).await;
    let cmd = CommandEnvelope::new(Command::StartPrivateStep { target_id: target });
    let id = cmd.command_id;
    f.hub.submit(&c, cmd).await;
    settle(&f.hub, id).await;
    let view: TargetControlView = f.hub.read(|r| r.require("control", target)).unwrap();
    let p = view.private_step.unwrap();
    assert_eq!(p.input_stopped, CoverageFact::Acknowledged);
    assert_eq!(p.managed_screenshots_stopped, CoverageFact::Acknowledged);
    assert!(p.uncovered_routes.is_empty());
    assert!(p.ready);
    f.close().await;
}
#[tokio::test]
async fn a_message_needs_text_or_an_annotation() {
    let f = Fixture::new().await;
    let project = f.project().await;
    let (task, _) = f
        .task(
            project,
            TaskPlacement::StartNow,
            &Caller::local_owner(),
            false,
        )
        .await;
    let r = f
        .hub
        .submit(
            &Caller::local_owner(),
            CommandEnvelope::new(Command::SendMessage {
                task_id: task,
                text: " \n".into(),
                bundle: None,
                delivery: MessageDelivery::NextTurn,
            }),
        )
        .await;
    assert_eq!(r.status, ReceiptStatus::Rejected, "{r:?}");
    assert_eq!(r.error.unwrap().code, ErrorCode::InvalidRequest);
    f.close().await;
}
#[tokio::test]
async fn stop_is_idempotent_and_fences_late_bridge_calls() {
    use ports::bridge::{BridgeBinding, BridgeHandler, BridgeRequest, BridgeResponse};
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
    assert!(matches!(
        f.hub
            .handle(&binding, BridgeRequest::ObserveTarget { max_dim: None })
            .await,
        BridgeResponse::Observation { .. }
    ));
    let stop = CommandEnvelope::new(Command::StopTask {
        task_id: task,
        force: false,
    });
    let id = stop.command_id;
    assert_eq!(
        f.hub.submit(&c, stop.clone()).await.status,
        ReceiptStatus::Accepted
    );
    assert!(matches!(
        f.hub
            .handle(
                &binding,
                BridgeRequest::TypeText {
                    text: "late".into()
                }
            )
            .await,
        BridgeResponse::Refused { .. }
    ));
    settle(&f.hub, id).await;
    assert_eq!(
        f.hub.submit(&c, stop).await.status,
        ReceiptStatus::Duplicate
    );
    assert_eq!(f.harness.controls.interrupts.load(Ordering::SeqCst), 1);
    assert_eq!(f.desktop.input.injected.load(Ordering::SeqCst), 0);
    let QueryData::TaskTimeline { items, .. } = f
        .hub
        .query(
            &Caller::local_owner(),
            Query::TaskTimeline {
                task_id: task,
                before_seq: None,
                limit: 100,
            },
        )
        .await
        .unwrap()
    else {
        panic!("timeline")
    };
    assert_eq!(
        items
            .iter()
            .filter(|i| i.run_id == Some(run)
                && matches!(&i.item, TimelineBody::Receipt { receipt } if receipt.kind == InterventionKind::StopTask))
            .count(),
        1,
        "one Stop receipt beside the stopped run"
    );
    f.close().await;
}
async fn upload_record(
    f: &Fixture,
    data: &[u8],
    digest: Option<String>,
    destination: TransferDestination,
) -> TransferId {
    let cmd = CommandEnvelope::new(Command::CreateUpload {
        evidence: None,
        name: "test.txt".into(),
        size: data.len() as u64,
        sha256: digest.unwrap_or_else(|| hash(data)),
        destination,
    });
    let r = f.hub.submit(&Caller::local_owner(), cmd).await;
    match r.result.unwrap() {
        CommandResult::Transfer { transfer } => transfer.transfer_id,
        _ => panic!("transfer"),
    }
}
#[tokio::test]
async fn transfers_resume_reject_overflow_and_verify_hash() {
    let f = Fixture::new().await;
    let owner = Caller::local_owner();
    let id = upload_record(&f, b"abcdef", None, TransferDestination::Artifact).await;
    let mut upload = f.hub.upload(&owner, id).await.unwrap();
    upload.write(b"abc").await.unwrap();
    drop(upload);
    tokio::task::yield_now().await;
    let mut upload = f.hub.upload(&owner, id).await.unwrap();
    assert_eq!(upload.resume_at(), 3);
    assert_eq!(
        upload.write(b"defg").await.unwrap_err().code,
        ErrorCode::LimitExceeded
    );
    upload.write(b"def").await.unwrap();
    let done = upload.finish().await.unwrap();
    assert_eq!(done.sha256, hash(b"abcdef"));
    assert!(done.artifact_id.is_some());
    let id = upload_record(
        &f,
        b"wrong",
        Some(hash(b"right")),
        TransferDestination::Inbox,
    )
    .await;
    let mut upload = f.hub.upload(&owner, id).await.unwrap();
    upload.write(b"wrong").await.unwrap();
    assert_eq!(upload.finish().await.unwrap_err().code, ErrorCode::Conflict);
    tokio::task::yield_now().await;
    f.hub.cancel_transfer(&owner, id).await.unwrap();
    assert!(!f.path.join("staging").join(id.to_string()).exists());
    f.close().await;
}
#[tokio::test]
async fn an_upload_interrupted_while_publishing_resumes_and_leaves_no_partial_copy() {
    let f = Fixture::new().await;
    let project = f.project().await;
    let owner = Caller::local_owner();
    let id = upload_record(
        &f,
        b"complete",
        None,
        TransferDestination::ProjectDir {
            project_id: project,
            dir: "".into(),
        },
    )
    .await;
    let mut upload = f.hub.upload(&owner, id).await.unwrap();
    upload.write(b"complete").await.unwrap();
    drop(upload);
    // The companion stopped while copying into the project: a partial copy
    // is left under its temporary name, and recovery pauses the upload.
    let partial = f.project_dir().join(format!(".incoming-{id}"));
    std::fs::write(&partial, b"comp").unwrap();
    let mut more = waiting();
    let upload = loop {
        match f.hub.upload(&owner, id).await {
            Ok(upload) => break upload,
            Err(_) if more() => tokio::task::yield_now().await,
            Err(e) => panic!("{e:?}"),
        }
    };
    assert_eq!(upload.resume_at(), 8);
    upload.finish().await.unwrap();
    assert_eq!(
        std::fs::read(f.project_dir().join("test.txt")).unwrap(),
        b"complete"
    );
    assert!(!partial.exists());
    f.close().await;
}
#[tokio::test]
async fn a_publication_whose_connection_goes_away_still_finishes() {
    use std::task::Poll;
    let f = Fixture::new().await;
    let owner = Caller::local_owner();
    let data = vec![7u8; 4 * protocol::transfer::MAX_CHUNK_BYTES];
    let id = upload_record(&f, &data, None, TransferDestination::Artifact).await;
    let mut upload = f.hub.upload(&owner, id).await.unwrap();
    for chunk in data.chunks(protocol::transfer::MAX_CHUNK_BYTES) {
        upload.write(chunk).await.unwrap();
    }
    // The connection's grant guard wins its select while the upload is being
    // verified, dropping the publication part-way.
    let mut finish = upload.finish();
    loop {
        let polled = std::future::poll_fn(|cx| Poll::Ready(finish.as_mut().poll(cx))).await;
        assert!(
            polled.is_pending(),
            "publication ended before it could be dropped"
        );
        let t: TransferRecord = f.hub.read(|r| r.require("transfer", id)).unwrap();
        if t.state == TransferState::Verifying {
            break;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    drop(finish);
    let mut more = waiting();
    let t = loop {
        let t: TransferRecord = f.hub.read(|r| r.require("transfer", id)).unwrap();
        if t.state != TransferState::Verifying || !more() {
            break t;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    };
    assert_eq!(t.state, TransferState::Completed);
    f.close().await;
}
#[tokio::test]
async fn transfer_locks_are_kept_only_while_held() {
    let f = Fixture::new().await;
    let owner = Caller::local_owner();
    for _ in 0..64 {
        let unknown = TransferId::new();
        assert!(f.hub.cancel_transfer(&owner, unknown).await.is_err());
        assert!(f.hub.upload(&owner, unknown).await.is_err());
    }
    let id = upload_record(&f, b"data", None, TransferDestination::Artifact).await;
    let mut upload = f.hub.upload(&owner, id).await.unwrap();
    assert_eq!(
        f.hub.upload(&owner, id).await.err().unwrap().code,
        ErrorCode::Conflict,
        "one writer at a time"
    );
    upload.write(b"data").await.unwrap();
    upload.finish().await.unwrap();
    // The finished upload releases its lock from a task of its own. Only the
    // latest lookup's entry may remain, until the next lookup.
    let mut more = waiting();
    loop {
        assert!(f.hub.upload(&owner, TransferId::new()).await.is_err());
        if f.hub.inner.transfer_locks.lock().unwrap().len() <= 1 || !more() {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(f.hub.inner.transfer_locks.lock().unwrap().len() <= 1);
    f.close().await;
}
#[tokio::test]
async fn transfers_do_not_escape_project_and_rename_collisions() {
    let f = Fixture::new().await;
    let project = f.project().await;
    let owner = Caller::local_owner();
    std::fs::write(f.project_dir().join("test.txt"), b"existing").unwrap();
    let receipt = f
        .hub
        .submit(
            &owner,
            CommandEnvelope::new(Command::CreateUploadReviewed {
                task_id: None,
                name: "test.txt".into(),
                size: 3,
                sha256: hash(b"new"),
                destination: TransferDestination::ProjectDir {
                    project_id: project,
                    dir: "".into(),
                },
                collision: UploadCollision::KeepBoth {
                    name: "test (1).txt".into(),
                },
            }),
        )
        .await;
    let Some(CommandResult::Transfer { transfer }) = receipt.result else {
        panic!("transfer declaration missing")
    };
    let id = transfer.transfer_id;
    let mut upload = f.hub.upload(&owner, id).await.unwrap();
    upload.write(b"new").await.unwrap();
    upload.finish().await.unwrap();
    assert_eq!(
        std::fs::read(f.project_dir().join("test.txt")).unwrap(),
        b"existing"
    );
    assert_eq!(
        std::fs::read(f.project_dir().join("test (1).txt")).unwrap(),
        b"new"
    );
    let t: TransferRecord = f.hub.read(|r| r.require("transfer", id)).unwrap();
    assert_eq!(
        t.publication.unwrap(),
        TransferPublication {
            root: ports::library::canonical(&f.project_dir())
                .unwrap()
                .to_string_lossy()
                .into_owned(),
            path: "test (1).txt".into(),
        }
    );
    let r = f
        .hub
        .submit(
            &owner,
            CommandEnvelope::new(Command::CreateUpload {
                evidence: None,
                name: "bad".into(),
                size: 0,
                sha256: hash(b""),
                destination: TransferDestination::ProjectDir {
                    project_id: project,
                    dir: "../outside".into(),
                },
            }),
        )
        .await;
    assert_eq!(r.error.unwrap().code, ErrorCode::PathNotAllowed);
    f.close().await;
}
#[tokio::test]
async fn queue_edits_invalidate_authority_and_reconnect_never_starts() {
    let f = Fixture::new().await;
    let project = f.project().await;
    let owner = Caller::local_owner();
    // A task holding the project keeps what's queued waiting.
    let (busy, _) = f
        .task(project, TaskPlacement::StartNow, &owner, false)
        .await;
    run_started(&f, busy).await;
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
    let q: QueueView = f.hub.read(|r| r.require("queue", project)).unwrap();
    // Adding the second kept the first's; editing the order asks again.
    assert_eq!(q.items[0].authorization, QueueAuthorization::AutoStart);
    let r = f
        .hub
        .submit(
            &owner,
            edit(
                Command::QueueSetDependencies {
                    item_id: q.items[0].item_id,
                    depends_on: vec![q.items[1].item_id],
                },
                q.revision,
            ),
        )
        .await;
    assert_eq!(r.status, ReceiptStatus::Accepted);
    // Only the edited item asks again: what it now waits on is unchanged.
    let q: QueueView = f.hub.read(|r| r.require("queue", project)).unwrap();
    assert_eq!(
        (q.items.iter())
            .map(|i| i.authorization)
            .collect::<Vec<_>>(),
        [
            QueueAuthorization::AskBeforeStart,
            QueueAuthorization::AutoStart
        ]
    );
    let r = f
        .hub
        .submit(
            &owner,
            edit(
                Command::QueueSetDependencies {
                    item_id: q.items[1].item_id,
                    depends_on: vec![q.items[0].item_id],
                },
                q.revision,
            ),
        )
        .await;
    assert_eq!(r.error.unwrap().code, ErrorCode::Conflict);
    let _ = f.hub.subscribe(&owner, None).await.unwrap();
    let _ = f
        .hub
        .query(
            &owner,
            Query::Snapshot {
                reads_withheld: None,
            },
        )
        .await
        .unwrap();
    f.hub.control_disconnected(&owner).await;
    assert_eq!(f.harness.starts.load(Ordering::SeqCst), 1);
    f.close().await;
}
#[tokio::test]
async fn channels_never_receive_control_and_revocation_blocks_existing_caller() {
    let f = Fixture::new().await;
    let device = f.device().await;
    let id = ChannelIdentityId::new();
    f.hub
        .write(|tx| {
            tx.put(
                "channel",
                id,
                &ChannelIdentity {
                    id,
                    channel: ChannelKind::Telegram,
                    external_user: "1".into(),
                    external_chat: "2".into(),
                    display_name: "Synthetic channel".into(),
                    linked_at: Timestamp(0),
                    revoked_at: None,
                    grants: Grants::owner(),
                    project_id: None,
                },
            )
        })
        .unwrap();
    let caller = Caller {
        principal: Principal::Channel { identity_id: id },
        grants: Grants::owner(),
    };
    let mut sub = f.hub.subscribe(&caller, None).await.unwrap();
    let target = f.desktop.target.lock().unwrap().target_id;
    let command = CommandEnvelope::new(Command::RequestHumanLease {
        target_id: target,
        geometry_revision: 1,
    });
    let cid = command.command_id;
    f.hub.submit(&device, command).await;
    settle(&f.hub, cid).await;
    while let Ok(out) = sub.live.try_recv() {
        if let Outbound::Event(e) = out {
            assert!(!matches!(
                e.event,
                Event::ControlChanged { .. } | Event::TargetsChanged { .. }
            ));
        }
    }
    assert_eq!(
        f.hub
            .query(&caller, Query::Terminals)
            .await
            .unwrap_err()
            .code,
        ErrorCode::PermissionDenied
    );
    let Principal::Device { device_id } = device.principal else {
        unreachable!()
    };
    let revoke = CommandEnvelope::new(Command::RevokeDevice { device_id });
    let cid = revoke.command_id;
    f.hub.submit(&Caller::local_owner(), revoke).await;
    settle(&f.hub, cid).await;
    assert_eq!(
        f.hub
            .query(
                &device,
                Query::Snapshot {
                    reads_withheld: None
                }
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::Unauthenticated
    );
    f.close().await;
}

#[tokio::test]
async fn command_effect_result_is_correlated_and_original_receipt_is_immutable() {
    let f = Fixture::new().await;
    let caller = f.device().await;
    let target = f.desktop.target.lock().unwrap().target_id;
    let command = CommandEnvelope::new(Command::RequestHumanLease {
        target_id: target,
        geometry_revision: 1,
    });
    let id = command.command_id;
    let admission = f.hub.submit(&caller, command.clone()).await;
    assert!(admission.result.is_none());
    settle(&f.hub, id).await;
    let status = f
        .hub
        .query(&caller, Query::CommandStatus { command_id: id })
        .await
        .unwrap();
    match status {
        QueryData::CommandStatus {
            receipt,
            stage,
            result,
            error,
        } => {
            assert_eq!(receipt, Some(admission.clone()));
            assert_eq!(stage, Some(CommandStage::Observed));
            assert!(matches!(
                result.as_deref(),
                Some(CommandResult::LeaseGranted { .. })
            ));
            assert!(error.is_none());
        }
        _ => panic!("command status"),
    }
    let duplicate = f.hub.submit(&caller, command).await;
    assert_eq!(duplicate.status, ReceiptStatus::Duplicate);
    assert_eq!(duplicate.result, admission.result);
    f.close().await;
}
#[tokio::test]
async fn replacement_terminal_uses_the_requested_folder_and_refuses_missing_folders() {
    let f = Fixture::new().await;
    let folder = f.path.join("subfolder");
    tokio::fs::create_dir_all(&folder).await.unwrap();
    let owner = Caller::local_owner();
    for (cwd, valid) in [
        (folder.display().to_string(), true),
        ("relative-folder".into(), false),
        (f.path.join("missing").display().to_string(), false),
    ] {
        let command = CommandEnvelope::new(Command::OpenTerminal {
            project_id: None,
            cwd: Some(cwd.clone()),
            harness: None,
            cols: 80,
            rows: 24,
        });
        let id = command.command_id;
        f.hub.submit(&owner, command).await;
        settle(&f.hub, id).await;
        let record = f.hub.read(|r| r.command(id)).unwrap().unwrap();
        if valid {
            let Some(CommandResult::TerminalOpened { terminal }) = record.result else {
                panic!("terminal result")
            };
            assert_eq!(terminal.cwd, cwd);
        } else {
            assert_eq!(record.error.unwrap().code, ErrorCode::InvalidRequest);
        }
    }
    f.close().await;
}

#[tokio::test]
async fn a_terminal_belongs_to_the_device_that_opened_it_and_ends_with_its_revocation() {
    let f = Fixture::new().await;
    // The first device manages; the second opens the terminal.
    let manager = pair(&f.hub, 1).await;
    let opener = pair(&f.hub, 2).await;
    let Principal::Device { device_id } = opener.principal else {
        unreachable!()
    };
    f.hub
        .write(|tx| {
            let mut device: crate::identity::Device = tx.read().require("device", device_id)?;
            device.record.grants.terminal = true;
            tx.put("device", device_id, &device)
        })
        .unwrap();
    let opener = Caller {
        grants: Grants {
            terminal: true,
            ..opener.grants
        },
        ..opener
    };
    let mut watching = f.hub.subscribe(&manager, None).await.unwrap();
    let open = CommandEnvelope::new(Command::OpenTerminal {
        cwd: None,
        project_id: None,
        harness: None,
        cols: 80,
        rows: 24,
    });
    let id = open.command_id;
    f.hub.submit(&opener, open).await;
    settle(&f.hub, id).await;
    let Some(CommandResult::TerminalOpened { terminal }) =
        f.hub.read(|r| r.command(id)).unwrap().unwrap().result
    else {
        panic!("terminal result")
    };
    let listed = |caller: Caller| {
        let hub = f.hub.clone();
        async move {
            let QueryData::Terminals { terminals } =
                hub.query(&caller, Query::Terminals).await.unwrap()
            else {
                panic!("terminals")
            };
            terminals.into_iter().map(|t| t.state).collect::<Vec<_>>()
        }
    };
    let running = [protocol::terminal::TerminalState::Running];
    assert_eq!(listed(opener.clone()).await, running);
    assert_eq!(listed(Caller::local_owner()).await, running);
    assert!(listed(manager.clone()).await.is_empty());
    let refused = f
        .hub
        .attach_terminal(&manager, terminal.terminal_id, Some(0))
        .await
        .err()
        .unwrap();
    assert_eq!(refused.code, ErrorCode::PermissionDenied);
    let close = CommandEnvelope::new(Command::CloseTerminal {
        terminal_id: terminal.terminal_id,
    });
    let id = close.command_id;
    f.hub.submit(&manager, close).await;
    settle(&f.hub, id).await;
    assert_eq!(
        f.hub
            .read(|r| r.command(id))
            .unwrap()
            .unwrap()
            .error
            .unwrap()
            .code,
        ErrorCode::PermissionDenied
    );
    assert!(
        f.hub
            .attach_terminal(&opener, terminal.terminal_id, Some(0))
            .await
            .is_ok()
    );
    assert!(
        f.hub
            .attach_terminal(&Caller::local_owner(), terminal.terminal_id, Some(0))
            .await
            .is_ok()
    );
    // Another device learns of the change, but not of the terminal.
    let mut changes = Vec::new();
    while let Ok(out) = watching.live.try_recv() {
        if let Outbound::Event(e) = out
            && let Event::TerminalsChanged { terminals } = &e.event
        {
            changes.push(terminals.len());
        }
    }
    assert_eq!(changes, [0]);
    let revoke = CommandEnvelope::new(Command::RevokeDevice { device_id });
    let id = revoke.command_id;
    f.hub.submit(&Caller::local_owner(), revoke).await;
    settle(&f.hub, id).await;
    assert_eq!(
        listed(Caller::local_owner()).await,
        [protocol::terminal::TerminalState::Exited { code: Some(0) }]
    );
    f.close().await;
}

#[tokio::test]
async fn a_terminal_that_did_not_outlive_a_restart_is_not_found_rather_than_someone_elses() {
    let f = Fixture::new().await;
    let device = pair(&f.hub, 1).await;
    let device = Caller {
        grants: Grants {
            terminal: true,
            ..device.grants
        },
        ..device
    };
    // Startup forgets every opener, because no terminal outlives the process.
    let old = protocol::ids::TerminalId::new();
    let refused = f
        .hub
        .attach_terminal(&device, old, Some(0))
        .await
        .err()
        .unwrap();
    assert_eq!(refused.code, ErrorCode::NotFound);
    let close = CommandEnvelope::new(Command::CloseTerminal { terminal_id: old });
    let id = close.command_id;
    f.hub.submit(&device, close).await;
    settle(&f.hub, id).await;
    assert_eq!(
        f.hub
            .read(|r| r.command(id))
            .unwrap()
            .unwrap()
            .error
            .unwrap()
            .code,
        ErrorCode::NotFound
    );
    f.close().await;
}

#[tokio::test]
async fn a_terminal_ends_when_its_device_is_replaced_by_pairing_again() {
    let f = Fixture::new().await;
    let owner = Caller::local_owner();
    let opener = pair(&f.hub, 1).await;
    let Principal::Device { device_id } = opener.principal else {
        unreachable!()
    };
    let opener = Caller {
        grants: Grants {
            terminal: true,
            ..opener.grants
        },
        ..opener
    };
    let open = CommandEnvelope::new(Command::OpenTerminal {
        cwd: None,
        project_id: None,
        harness: None,
        cols: 80,
        rows: 24,
    });
    let opened = open.command_id;
    f.hub.submit(&opener, open).await;
    settle(&f.hub, opened).await;
    assert!(matches!(
        f.hub.read(|r| r.command(opened)).unwrap().unwrap().result,
        Some(CommandResult::TerminalOpened { .. })
    ));
    let ticket = f.hub.open_pairing(&owner).await.unwrap();
    let request = PairRequest {
        replaces: Some(device_id),
        ..pair_request(&ticket.secret, 2)
    };
    let claim = f
        .hub
        .request_pairing(request, "222333".into())
        .await
        .unwrap();
    f.hub.answer_pairing(&owner, "222333", true).await.unwrap();
    claim.answer.await.unwrap().unwrap();
    let QueryData::Terminals { terminals } = f.hub.query(&owner, Query::Terminals).await.unwrap()
    else {
        panic!("terminals")
    };
    assert_eq!(
        terminals.into_iter().map(|t| t.state).collect::<Vec<_>>(),
        [protocol::terminal::TerminalState::Exited { code: Some(0) }]
    );
    f.close().await;
}

/// A terminal host whose shells start only once the test lets them.
#[derive(Debug, Default)]
struct HeldTerminal {
    terminals: FakeTerminal,
    opening: tokio::sync::Notify,
    start: tokio::sync::Notify,
}
#[async_trait::async_trait]
impl ports::TerminalHost for HeldTerminal {
    async fn open(
        &self,
        spec: ports::TerminalSpec,
    ) -> Result<protocol::terminal::TerminalInfo, ports::TerminalError> {
        self.opening.notify_one();
        self.start.notified().await;
        self.terminals.open(spec).await
    }
    async fn list(&self) -> Vec<protocol::terminal::TerminalInfo> {
        self.terminals.list().await
    }
    async fn info(&self, id: TerminalId) -> Option<protocol::terminal::TerminalInfo> {
        self.terminals.info(id).await
    }
    async fn attach(
        &self,
        id: TerminalId,
        from: Option<u64>,
    ) -> Result<
        (
            ports::Replay,
            tokio::sync::broadcast::Receiver<ports::TerminalEvent>,
        ),
        ports::TerminalError,
    > {
        self.terminals.attach(id, from).await
    }
    async fn write(&self, id: TerminalId, bytes: &[u8]) -> Result<(), ports::TerminalError> {
        self.terminals.write(id, bytes).await
    }
    async fn resize(
        &self,
        id: TerminalId,
        cols: u16,
        rows: u16,
    ) -> Result<(), ports::TerminalError> {
        self.terminals.resize(id, cols, rows).await
    }
    async fn close(&self, id: TerminalId) -> Result<(), ports::TerminalError> {
        self.terminals.close(id).await
    }
}

#[tokio::test]
async fn a_terminal_still_starting_when_its_device_is_revoked_ends_with_it() {
    let host = Arc::new(HeldTerminal::default());
    let f = Fixture::with(|builder| builder.terminal(host.clone())).await;
    let opener = pair(&f.hub, 1).await;
    let Principal::Device { device_id } = opener.principal else {
        unreachable!()
    };
    f.hub
        .write(|tx| {
            let mut device: crate::identity::Device = tx.read().require("device", device_id)?;
            device.record.grants.terminal = true;
            tx.put("device", device_id, &device)
        })
        .unwrap();
    let opener = Caller {
        grants: Grants {
            terminal: true,
            ..opener.grants
        },
        ..opener
    };
    let open = CommandEnvelope::new(Command::OpenTerminal {
        cwd: None,
        project_id: None,
        harness: None,
        cols: 80,
        rows: 24,
    });
    let opened = open.command_id;
    f.hub.submit(&opener, open).await;
    host.opening.notified().await;

    let revoke = CommandEnvelope::new(Command::RevokeDevice { device_id });
    let id = revoke.command_id;
    f.hub.submit(&Caller::local_owner(), revoke).await;
    settle(&f.hub, id).await;
    host.start.notify_one();
    settle(&f.hub, opened).await;

    let command = f.hub.read(|r| r.command(opened)).unwrap().unwrap();
    assert!(command.result.is_none());
    assert_eq!(command.error.unwrap().code, ErrorCode::Unauthenticated);
    let terminals = ports::TerminalHost::list(host.as_ref()).await;
    assert_eq!(terminals.len(), 1);
    assert!(
        terminals
            .iter()
            .all(|t| t.state != protocol::terminal::TerminalState::Running)
    );
    f.close().await;
}

#[tokio::test]
async fn terminal_replay_live_media_open_and_speech_retention() {
    let f = Fixture::new().await;
    let owner = Caller::local_owner();
    let cmd = CommandEnvelope::new(Command::OpenTerminal {
        cwd: None,
        project_id: None,
        harness: None,
        cols: 80,
        rows: 24,
    });
    let id = cmd.command_id;
    f.hub.submit(&owner, cmd).await;
    settle(&f.hub, id).await;
    let record = f.hub.read(|r| r.command(id)).unwrap().unwrap();
    let Some(CommandResult::TerminalOpened { terminal }) = record.result else {
        panic!("terminal result")
    };
    let mut attachment = f
        .hub
        .attach_terminal(&owner, terminal.terminal_id, Some(0))
        .await
        .unwrap();
    attachment.input.resize(100, 30).await.unwrap();
    assert!(matches!(
        attachment.output.recv().await,
        Some(api::TerminalOut::Resized {
            cols: 100,
            rows: 30
        })
    ));
    attachment.input.detach().await;
    assert!(attachment.input.write(b"x").await.is_err());
    let target = f.desktop.target.lock().unwrap().target_id;
    let mut media = f
        .hub
        .open_media(&owner, target, protocol::media::StreamParams::default())
        .await
        .unwrap();
    assert!(matches!(
        media.output.recv().await,
        Some(api::MediaOut::Control(
            protocol::media::MediaServerFrame::Opened { .. }
        ))
    ));
    assert!(matches!(
        media.output.recv().await,
        Some(api::MediaOut::Frame(_))
    ));
    media.control.close().await;
    for keep in [false, true] {
        let mut speech = f
            .hub
            .start_speech(&owner, "test".into(), None, keep)
            .await
            .unwrap();
        assert!(matches!(
            speech.output.recv().await,
            Some(api::SpeechOut::Ready { .. })
        ));
        speech.input.push(&[0, 0, 1, 0]).await.unwrap();
        speech.input.finish().await;
        assert!(matches!(
            speech.output.recv().await,
            Some(api::SpeechOut::Segment(_))
        ));
        match speech.output.recv().await.unwrap() {
            api::SpeechOut::Finished { audio_artifact } => {
                assert_eq!(audio_artifact.is_some(), keep)
            }
            other => panic!("{other:?}"),
        }
    }
    drop(attachment);
    drop(media);
    f.close().await;
}
#[tokio::test]
async fn queue_pause_serializes_with_admission_and_failed_dependencies_block() {
    let f = Fixture::new().await;
    let owner = Caller::local_owner();
    let project = f.project().await;
    let (task, _) = f
        .task(
            project,
            TaskPlacement::Queue {
                position: None,
                authorization: QueueAuthorization::AutoStart,
            },
            &owner,
            false,
        )
        .await;
    let q: QueueView = f.hub.read(|r| r.require("queue", project)).unwrap();
    let resume = edit(
        Command::QueueResume {
            project_id: project,
        },
        q.revision,
    );
    let resume_id = resume.command_id;
    f.hub.submit(&owner, resume).await;
    settle(&f.hub, resume_id).await;
    let run = run_started(&f, task).await;
    let q: QueueView = f.hub.read(|r| r.require("queue", project)).unwrap();
    assert_eq!(q.writer, Some(run));
    assert_eq!(
        f.hub
            .read(|r| r.require::<crate::runs::QueuePermit>("queue_permit", q.items[0].item_id))
            .unwrap()
            .remaining,
        0
    );
    let pause = edit(
        Command::QueuePause {
            project_id: project,
        },
        q.revision,
    );
    assert_eq!(
        f.hub.submit(&owner, pause).await.status,
        ReceiptStatus::Accepted
    );
    let events = f.hub.read(|r| r.journal(0)).unwrap().events;
    assert!(events.iter().any(|e|matches!(&e.event,Event::InterventionRecorded{receipt,..}if receipt.facts.iter().any(|fact|fact.detail.as_deref()==Some(&run.to_string())))));
    let (next, _) = f
        .task(
            project,
            TaskPlacement::Queue {
                position: None,
                authorization: QueueAuthorization::AskBeforeStart,
            },
            &owner,
            false,
        )
        .await;
    let q: QueueView = f.hub.read(|r| r.require("queue", project)).unwrap();
    let item = q.items.iter().find(|i| i.task_id == next).unwrap();
    let dependency = q.items.iter().find(|i| i.task_id == task).unwrap();
    let dep = edit(
        Command::QueueSetDependencies {
            item_id: item.item_id,
            depends_on: vec![dependency.item_id],
        },
        q.revision,
    );
    f.hub.submit(&owner, dep).await;
    f.harness
        .emit(
            run,
            ports::HarnessEvent::Exited {
                exit: ports::RunExit::Failed {
                    reason: "synthetic failure".into(),
                },
                outcome: None,
            },
        )
        .await;
    let mut more = waiting();
    while more() {
        let q: QueueView = f.hub.read(|r| r.require("queue", project)).unwrap();
        if q.writer.is_none() {
            assert!(matches!(
                q.items.iter().find(|i| i.task_id == next).unwrap().state,
                QueueItemState::Blocked { .. }
            ));
            break;
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(f.harness.starts.load(Ordering::SeqCst), 1);
    f.close().await;
}
#[tokio::test]
async fn native_approval_compares_revision_and_routes_once() {
    let f = Fixture::new().await;
    let owner = Caller::local_owner();
    let project = f.project().await;
    let (task, _) = f
        .task(project, TaskPlacement::StartNow, &owner, false)
        .await;
    let run = run_started(&f, task).await;
    f.harness
        .emit(
            run,
            ports::HarnessEvent::ApprovalRequested(ports::NativeApproval {
                native_request_id: "native-1".into(),
                kind: ApprovalKind::Tool,
                title: "Synthetic approval".into(),
                detail: None,
                detail_lines: Vec::new(),
                questions: Vec::new(),
                raw_request: None,
                scope: "once".into(),
                shortened: false,
                tool_call: None,
            }),
        )
        .await;
    let approval = loop {
        let approvals: Vec<Approval> = f.hub.read(|r| r.list("approval")).unwrap();
        if let Some(a) = approvals.into_iter().next() {
            break a;
        }
        tokio::task::yield_now().await;
    };
    let stale = f
        .hub
        .submit(
            &owner,
            edit(
                Command::ResolveApproval {
                    approval_id: approval.approval_id,
                    decision: ApprovalDecision::Allow,
                },
                0,
            ),
        )
        .await;
    assert_eq!(stale.error.unwrap().code, ErrorCode::StaleRevision);
    let command = edit(
        Command::ResolveApproval {
            approval_id: approval.approval_id,
            decision: ApprovalDecision::Allow,
        },
        1,
    );
    let id = command.command_id;
    f.hub.submit(&owner, command.clone()).await;
    settle(&f.hub, id).await;
    assert_eq!(f.harness.controls.resolutions.load(Ordering::SeqCst), 1);
    let current: Task = f.hub.read(|r| r.require("task", task)).unwrap();
    assert_eq!(current.state, TaskState::Working);
    assert_eq!(current.attention_count, 0);
    let current: Approval = f
        .hub
        .read(|r| r.require("approval", approval.approval_id))
        .unwrap();
    assert_eq!(current.state, ApprovalState::Allowed);
    assert!(current.resolved_at.is_some());
    let Backlog::Events(events) = f.hub.subscribe(&owner, Some(0)).await.unwrap().backlog else {
        panic!("fresh journal expected");
    };
    assert!(
        events.iter().any(
            |e| matches!(&e.event, Event::ApprovalUpserted { approval } if approval == &current)
        )
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(&e.event, Event::AttentionResolved { .. }))
    );
    assert_eq!(
        f.hub.submit(&owner, command).await.status,
        ReceiptStatus::Duplicate
    );
    let another = f
        .hub
        .submit(
            &owner,
            edit(
                Command::ResolveApproval {
                    approval_id: approval.approval_id,
                    decision: ApprovalDecision::Allow,
                },
                1,
            ),
        )
        .await;
    assert_eq!(another.error.unwrap().code, ErrorCode::AlreadyResolved);
    f.close().await;
}
#[tokio::test]
async fn interrupted_publication_requires_review_instead_of_republish() {
    let f = Fixture::new().await;
    let owner = Caller::local_owner();
    let id = upload_record(&f, b"x", None, TransferDestination::Inbox).await;
    f.hub
        .write(|tx| {
            tx.put("publication_started", id, &true)?;
            Ok(())
        })
        .unwrap();
    assert_eq!(
        f.hub.upload(&owner, id).await.err().unwrap().code,
        ErrorCode::OutcomeUnknown
    );
    f.close().await;
}
#[cfg(unix)]
#[tokio::test]
async fn project_symlink_escape_is_refused_by_filesystem_capability() {
    let f = Fixture::new().await;
    let owner = Caller::local_owner();
    let project = f.project().await;
    let outside = f
        .path
        .parent()
        .unwrap()
        .join(format!("outside-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&outside).unwrap();
    let destination = TransferDestination::ProjectDir {
        project_id: project,
        dir: "link".into(),
    };
    // The folder is swapped for a link out of the project after the upload
    // was declared into it.
    std::fs::create_dir(f.project_dir().join("link")).unwrap();
    let id = upload_record(&f, b"x", None, destination.clone()).await;
    std::fs::remove_dir(f.project_dir().join("link")).unwrap();
    std::os::unix::fs::symlink(&outside, f.project_dir().join("link")).unwrap();
    let mut upload = f.hub.upload(&owner, id).await.unwrap();
    upload.write(b"x").await.unwrap();
    assert_eq!(
        upload.finish().await.unwrap_err().code,
        ErrorCode::PathNotAllowed
    );
    assert!(!outside.join("test.txt").exists());
    // Declared into the link, it is refused before any bytes are sent.
    let receipt = f
        .hub
        .submit(
            &owner,
            CommandEnvelope::new(Command::CreateUpload {
                evidence: None,
                name: "test.txt".into(),
                size: 1,
                sha256: hash(b"x"),
                destination,
            }),
        )
        .await;
    assert_eq!(receipt.error.unwrap().code, ErrorCode::PathNotAllowed);
    f.close().await;
    std::fs::remove_dir_all(outside).unwrap();
}

#[tokio::test]
async fn library_saves_return_current_content_on_conflict() {
    use ports::LibraryFs;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path();
    let library = Arc::new(FakeLibrary::default());
    let root = LibraryRootId::new();
    library
        .set_roots(vec![ports::RootSpec {
            root_id: root,
            label: "Drafts".into(),
            path: "synthetic".into(),
            kind: protocol::library::RootKind::Drafts,
            writable: true,
            project_id: None,
            harness: None,
            only: None,
        }])
        .await
        .unwrap();
    let hub = CompanionHub::builder()
        .data_dir(path)
        .library(library.clone())
        .build()
        .await
        .unwrap();
    let owner = Caller::local_owner();
    let create = CommandEnvelope::new(Command::LibraryCreate {
        root_id: root,
        path: "note.md".into(),
        text: "original".into(),
    });
    let id = create.command_id;
    hub.submit(&owner, create).await;
    settle(&hub, id).await;
    assert!(matches!(
        hub.read(|r| r.command(id)).unwrap().unwrap().result,
        Some(CommandResult::Created { .. })
    ));
    // The first paired phone is a manager; the second has ordinary device grants.
    let _manager = pair(&hub, 1).await;
    async fn set_library(hub: &CompanionHub, caller: &Caller, read: bool, write: bool) {
        let Principal::Device { device_id } = caller.principal else {
            unreachable!()
        };
        let stored: crate::identity::Device = hub.read(|r| r.require("device", device_id)).unwrap();
        let update = CommandEnvelope::new(Command::SetDeviceGrants {
            device_id,
            expected: Some(stored.record.grants.clone()),
            grants: Grants {
                library_read: read,
                library_write: write,
                ..stored.record.grants
            },
        });
        let id = update.command_id;
        let receipt = hub.submit(&Caller::local_owner(), update).await;
        assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
        settle(hub, id).await;
    }
    let mut phone = pair(&hub, 2).await;
    let mut other = pair(&hub, 3).await;
    for caller in [&mut phone, &mut other] {
        set_library(&hub, caller, true, true).await;
        caller.grants.library_write = true;
    }
    assert!(!phone.grants.admin);
    let save = CommandEnvelope::new(Command::LibrarySave {
        root_id: root,
        path: "note.md".into(),
        text: "new".into(),
        base_revision: "stale".into(),
        newline: protocol::library::NewlineStyle::Lf,
        bom: false,
    });
    let id = save.command_id;
    let receipt = hub.submit(&phone, save).await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    settle(&hub, id).await;
    assert!(matches!(
        hub.read(|r| r.command(id)).unwrap().unwrap().result,
        Some(CommandResult::Saved {
            outcome: protocol::library::SaveOutcome::Conflict { current: Some(current) }
        }) if current.text == "original"
    ));
    assert_eq!(
        library.read(root, "note.md").await.unwrap().text,
        "original"
    );
    enum Disclosure {
        Visible,
        Withheld,
        Missing,
    }
    use Disclosure::*;
    async fn disclosed(hub: &CompanionHub, caller: &Caller, id: CommandId, expected: Disclosure) {
        let Backlog::Events(events) = hub.subscribe(caller, Some(0)).await.unwrap().backlog else {
            panic!("fresh journal expected");
        };
        assert_eq!(
            events.iter().any(|e| matches!(&e.event,
                Event::CommandProgress { command_id, result: Some(_), .. } if *command_id == id
            )),
            matches!(expected, Visible)
        );
        let status = hub
            .query(caller, Query::CommandStatus { command_id: id })
            .await;
        if matches!(expected, Withheld) {
            assert_eq!(
                status.unwrap_err(),
                error(
                    ErrorCode::PermissionDenied,
                    "Access to this command's status is not permitted"
                )
            );
            return;
        }
        let status = status.unwrap();
        if matches!(expected, Visible) {
            assert!(matches!(status, QueryData::CommandStatus {
                result: Some(result), receipt: Some(_), ..
            } if matches!(result.as_ref(), CommandResult::Saved {
                outcome: protocol::library::SaveOutcome::Conflict { current: Some(current) }
            } if current.text == "original")));
        } else {
            assert!(
                matches!(
                    status,
                    QueryData::CommandStatus {
                        stage: None,
                        result: None,
                        error: None,
                        receipt: None,
                    }
                ),
                "protected command status leaked: {status:?}"
            );
        }
    }
    disclosed(&hub, &phone, id, Visible).await;
    disclosed(&hub, &owner, id, Visible).await;
    disclosed(&hub, &other, id, Missing).await;
    // Receipts can carry immediate results too: protect the whole stored record.
    hub.write(|tx| {
        let mut stored = tx.read().command(id)?.unwrap();
        stored.receipt.result = stored.result.clone();
        tx.command(&stored)
    })
    .unwrap();
    // Exercise either grant being removed and both removed, retaining the stale Caller.
    for (library_read, library_write) in
        [(true, false), (false, true), (false, false), (true, true)]
    {
        set_library(&hub, &phone, library_read, library_write).await;
        if !library_read {
            assert_eq!(
                hub.query(
                    &phone,
                    Query::LibraryRead {
                        root_id: root,
                        path: "note.md".into(),
                    }
                )
                .await
                .unwrap_err()
                .code,
                ErrorCode::PermissionDenied
            );
        }
        disclosed(
            &hub,
            &phone,
            id,
            if library_read && library_write {
                Visible
            } else {
                Withheld
            },
        )
        .await;
        disclosed(&hub, &phone, CommandId::new(), Missing).await;
    }
    hub.shutdown().await.unwrap();
    drop(hub);
}
/// Serve one scripted answer per request, recording each request line.
async fn preview_server(
    answers: Vec<&'static str>,
) -> (url::Url, tokio::task::JoinHandle<Vec<String>>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = url::Url::parse(&format!("http://{}/test", listener.local_addr().unwrap())).unwrap();
    let server = tokio::spawn(async move {
        let mut requests = Vec::new();
        for answer in answers {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0; 2048];
            let n = stream.read(&mut request).await.unwrap();
            let head = String::from_utf8_lossy(&request[..n]).to_ascii_lowercase();
            requests.push(format!(
                "{}{}",
                head.lines().next().unwrap_or_default(),
                if head.contains("\r\nrange: bytes=0-0\r\n") {
                    " ranged"
                } else {
                    ""
                }
            ));
            let status = answer.split(' ').next().unwrap_or_default();
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 {answer}\r\nContent-Length: 0\r\nConnection: close\r\n{}\r\n",
                        if status == "302" {
                            "Location: http://8.8.8.8/\r\n"
                        } else {
                            ""
                        }
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        }
        requests
    });
    (url, server)
}

#[tokio::test]
async fn a_conflict_over_a_large_note_leaves_its_text_to_be_read() {
    use ports::LibraryFs;
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/service-tests")
        .join(uuid::Uuid::now_v7().to_string());
    let library = Arc::new(FakeLibrary::default());
    let root = LibraryRootId::new();
    library
        .set_roots(vec![ports::RootSpec {
            root_id: root,
            label: "Drafts".into(),
            path: "synthetic".into(),
            kind: protocol::library::RootKind::Drafts,
            writable: true,
            project_id: None,
            harness: None,
            only: None,
        }])
        .await
        .unwrap();
    // Under the Library's file limit, but twice a frame once quoted.
    let text = "\"".repeat(900 * 1024);
    library.create(root, "note.md", &text).await.unwrap();
    let hub = CompanionHub::builder()
        .data_dir(&path)
        .library(library.clone())
        .build()
        .await
        .unwrap();
    let save = CommandEnvelope::new(Command::LibrarySave {
        root_id: root,
        path: "note.md".into(),
        text: "new".into(),
        base_revision: "stale".into(),
        newline: protocol::library::NewlineStyle::Lf,
        bom: false,
    });
    let id = save.command_id;
    hub.submit(&Caller::local_owner(), save).await;
    settle(&hub, id).await;
    let progress = hub
        .read(|r| r.journal(0))
        .unwrap()
        .events
        .into_iter()
        .rfind(
            |e| matches!(&e.event, Event::CommandProgress { command_id, .. } if *command_id == id),
        )
        .unwrap();
    assert!(serde_json::to_vec(&progress).unwrap().len() < 1024 * 1024);
    assert!(matches!(
        progress.event,
        Event::CommandProgress {
            result: Some(CommandResult::Saved {
                outcome: protocol::library::SaveOutcome::Conflict { current: None }
            }),
            ..
        }
    ));
    assert_eq!(library.read(root, "note.md").await.unwrap().text, text);
    hub.shutdown().await.unwrap();
    drop(hub);
    std::fs::remove_dir_all(path).unwrap();
}
#[tokio::test]
async fn preview_http_does_not_follow_redirects() {
    use crate::preview::PreviewChecker;
    let (url, server) = preview_server(vec!["302 Found"]).await;
    let checker = preview::HttpPreviewChecker::new().unwrap();
    assert!(checker.check(&url).await.unwrap());
    assert_eq!(server.await.unwrap(), ["head /test http/1.1"]);
}

#[tokio::test]
async fn preview_http_asks_with_get_when_head_is_refused() {
    use crate::preview::PreviewChecker;
    let checker = preview::HttpPreviewChecker::new().unwrap();
    // Python's http.server with only do_GET refuses HEAD with 501; any
    // answer to the GET, even an application's 500, is a running server.
    for (head, get) in [
        ("501 Unsupported method", "200 OK"),
        ("405 Method Not Allowed", "206 Partial Content"),
        ("404 Not Found", "500 Internal Server Error"),
    ] {
        let (url, server) = preview_server(vec![head, get]).await;
        assert!(checker.check(&url).await.unwrap(), "{head} then {get}");
        assert_eq!(
            server.await.unwrap(),
            ["head /test http/1.1", "get /test http/1.1 ranged"]
        );
    }
    let (url, server) = preview_server(vec!["200 OK"]).await;
    assert!(checker.check(&url).await.unwrap());
    assert_eq!(server.await.unwrap(), ["head /test http/1.1"]);
}

#[tokio::test]
async fn preview_http_reports_a_port_nothing_answers_as_unreachable() {
    use crate::preview::PreviewChecker;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = url::Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
    drop(listener);
    let checker = preview::HttpPreviewChecker::new().unwrap();
    assert!(!checker.check(&url).await.unwrap());
}

#[tokio::test]
async fn rolled_back_events_are_never_published() {
    let f = Fixture::new().await;
    let mut sub = f.hub.subscribe(&Caller::local_owner(), None).await.unwrap();
    let head = f.hub.journal_head();
    let failed: Result<(), ProtocolError> = f.hub.write(|tx| {
        tx.put("rollback", "value", &42)?;
        tx.emit(Event::BootEpochChanged {
            boot_epoch: BootEpoch::new(),
        })?;
        Err(crate::error(ErrorCode::Conflict, "synthetic rollback").into())
    });
    assert!(failed.is_err());
    assert_eq!(f.hub.journal_head(), head);
    assert!(
        f.hub
            .read(|r| r.get::<u32>("rollback", "value"))
            .unwrap()
            .is_none()
    );
    assert!(matches!(
        sub.live.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));
    f.close().await;
}
#[tokio::test]
async fn retention_gap_requires_a_fresh_snapshot() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/service-tests")
        .join(uuid::Uuid::now_v7().to_string());
    let hub = CompanionHub::builder()
        .data_dir(&path)
        .retention(store::Retention {
            max_events: 3,
            max_age_ms: i64::MAX,
        })
        .build()
        .await
        .unwrap();
    for _ in 0..10 {
        hub.write(|tx| {
            tx.emit(Event::BootEpochChanged {
                boot_epoch: hub.boot_epoch(),
            })?;
            Ok(())
        })
        .unwrap();
    }
    let sub = hub
        .subscribe(&Caller::local_owner(), Some(0))
        .await
        .unwrap();
    match sub.backlog {
        Backlog::Gap {
            oldest_retained, ..
        } => assert_eq!(oldest_retained, hub.journal_head() - 2),
        _ => panic!("expected retention gap"),
    }
    let head = hub.journal_head();
    assert!(matches!(
        hub.backlog(&Caller::local_owner(), 0, head, 10).await.unwrap().0,
        Backlog::Gap { requested_from: 0, oldest_retained } if oldest_retained == head - 2
    ));
    let boundary = hub.journal_head() - 3;
    let sub = hub
        .subscribe(&Caller::local_owner(), Some(boundary))
        .await
        .unwrap();
    match sub.backlog {
        Backlog::Events(events) => assert_eq!(events.len(), 3),
        _ => panic!("boundary is retained"),
    }
    hub.shutdown().await.unwrap();
    drop(hub);
    std::fs::remove_dir_all(path).unwrap();
}

#[tokio::test]
async fn supplied_host_identity_survives_restart_and_mismatch_fails_closed() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/service-tests")
        .join(HostId::new().to_string());
    let host = HostId::new();
    let hub = CompanionHub::builder()
        .data_dir(&path)
        .host_id(host)
        .build()
        .await
        .unwrap();
    assert_eq!(hub.host_id(), host);
    hub.shutdown().await.unwrap();
    drop(hub);
    let error = CompanionHub::builder()
        .data_dir(&path)
        .host_id(HostId::new())
        .build()
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::Conflict);
    assert!(error.message.contains("host id differs"));
    let hub = CompanionHub::builder()
        .data_dir(&path)
        .host_id(host)
        .build()
        .await
        .unwrap();
    assert_eq!(hub.host_id(), host);
    hub.shutdown().await.unwrap();
    drop(hub);
    std::fs::remove_dir_all(path).unwrap();
}

#[tokio::test]
async fn desktop_removal_revokes_lease_and_ends_stream_with_reason() {
    let f = Fixture::new().await;
    let caller = f.device().await;
    let id = f.desktop.target.lock().unwrap().target_id;
    let command = f
        .hub
        .submit(
            &caller,
            CommandEnvelope::new(Command::RequestHumanLease {
                target_id: id,
                geometry_revision: 1,
            }),
        )
        .await;
    settle(&f.hub, command.command_id).await;
    let mut media = f
        .hub
        .open_media(&caller, id, protocol::media::StreamParams::default())
        .await
        .unwrap();
    let mut events = f.hub.subscribe(&caller, None).await.unwrap();
    f.hub
        .set_desktop(
            None,
            protocol::capability::Availability::PermissionMissing {
                remedy: "Run ophio desktop enable".into(),
            },
        )
        .await
        .unwrap();
    assert!(matches!(
        f.hub.capabilities().await.capture,
        protocol::capability::Availability::PermissionMissing { .. }
    ));
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Some(api::MediaOut::Control(protocol::media::MediaServerFrame::Ended {
                reason,
            })) = media.output.recv().await
            {
                assert!(reason.contains("Desktop sharing ended"));
                break;
            }
        }
    })
    .await
    .unwrap();
    let mut revoked = false;
    let mut reason = false;
    while let Ok(Outbound::Event(e)) = events.live.try_recv() {
        match &e.event {
            Event::ControlChanged { view } => {
                if view
                    .lease
                    .as_ref()
                    .is_some_and(|l| l.state == LeaseState::Revoked)
                {
                    revoked = true;
                }
            }
            Event::InterventionRecorded { receipt, .. } => {
                reason |= receipt.summary.contains("Desktop sharing ended")
            }
            _ => {}
        }
    }
    assert!(revoked && reason);
    f.hub
        .set_desktop(
            Some(f.desktop.clone()),
            protocol::capability::Availability::Available,
        )
        .await
        .unwrap();
    assert!(f.hub.capabilities().await.capture.is_available());
    drop(media);
    f.close().await;
}

#[tokio::test]
async fn retention_change_compacts_the_running_store() {
    let f = Fixture::new().await;
    for _ in 0..10 {
        f.hub.refresh_readiness().await.unwrap();
    }
    f.hub
        .set_retention(store::Retention {
            max_events: 2,
            max_age_ms: i64::MAX,
        })
        .unwrap();
    let sub = f
        .hub
        .subscribe(&Caller::local_owner(), Some(0))
        .await
        .unwrap();
    assert!(matches!(sub.backlog, Backlog::Gap { .. }));
    f.close().await;
}

#[tokio::test]
async fn bridge_connection_is_single_use_and_disconnect_expires_pending_approval() {
    use ports::bridge::{BridgeHandler, BridgeRequest, BridgeResponse, RUN_TOKEN_ENV};
    let f = Fixture::new().await;
    let project = f.project().await;
    let (task, _) = f
        .task(
            project,
            TaskPlacement::StartNow,
            &Caller::local_owner(),
            false,
        )
        .await;
    let _run = run_started(&f, task).await;
    let token = f.harness.specs.lock().unwrap()[0]
        .bridge
        .as_ref()
        .unwrap()
        .env
        .iter()
        .find(|(k, _)| k == RUN_TOKEN_ENV)
        .unwrap()
        .1
        .clone();
    let binding = f.hub.bind(&token).await.unwrap();
    assert!(f.hub.bind(&token).await.is_none());
    let hub = f.hub.clone();
    let bound = binding.clone();
    let pending = tokio::spawn(async move {
        hub.handle(
            &bound,
            BridgeRequest::PermissionPrompt {
                tool_name: "Bash".into(),
                input: serde_json::json!({"command": "fixture"}),
                tool_use_id: None,
                native_request_id: Some("2".into()),
            },
        )
        .await
    });
    let approval = loop {
        let approvals: Vec<Approval> = f.hub.read(|r| r.list("approval")).unwrap();
        if let Some(a) = approvals.into_iter().next() {
            break a;
        }
        tokio::task::yield_now().await;
    };
    pending.abort();
    let _ = pending.await;
    f.hub.unbound(&binding).await;
    assert!(f.hub.bind(&token).await.is_none());
    let approval: Approval = f
        .hub
        .read(|r| r.require("approval", approval.approval_id))
        .unwrap();
    assert_eq!(approval.state, ApprovalState::Expired);
    assert!(f.hub.inner.waits.lock().await.is_empty());
    assert!(matches!(
        f.hub
            .handle(&binding, BridgeRequest::ObserveTarget { max_dim: None })
            .await,
        BridgeResponse::Refused { .. }
    ));
    f.close().await;
}

#[tokio::test]
async fn bridge_permissions_use_typed_mcp_ids_instead_of_optional_tool_ids() {
    use ports::bridge::{BridgeBinding, BridgeHandler, BridgeRequest, BridgeResponse};
    let f = Fixture::new().await;
    let project = f.project().await;
    let (task, _) = f
        .task(
            project,
            TaskPlacement::StartNow,
            &Caller::local_owner(),
            false,
        )
        .await;
    let run = run_started(&f, task).await;
    // Exercise the broker boundary directly with the trusted run binding.
    // Transport authentication and token handling are covered separately.
    let binding = BridgeBinding {
        run_id: run,
        target_id: None,
        computer_use: false,
        browser: false,
        github: false,
        checklist_answers: false,
    };
    let mut ids = Vec::new();
    for native_id in ["7", "\"7\""] {
        let request = BridgeRequest::PermissionPrompt {
            tool_name: "FixtureTool".into(),
            input: serde_json::json!({"fixture": true}),
            tool_use_id: Some("same-optional-tool-id".into()),
            native_request_id: Some(native_id.into()),
        };
        let hub = f.hub.clone();
        let bound = binding.clone();
        let pending_request = request.clone();
        let pending = tokio::spawn(async move { hub.handle(&bound, pending_request).await });
        let mut approval = None;
        let mut more = waiting();
        while more() {
            approval = f
                .hub
                .read(|r| {
                    Ok(r.list::<Approval>("approval")?
                        .into_iter()
                        .find(|a| a.native_request_id == native_id))
                })
                .unwrap();
            if approval.is_some() {
                break;
            }
            tokio::task::yield_now().await;
        }
        let approval = approval.expect("MCP identity should identify a fresh approval");
        ids.push(approval.approval_id);
        let decision = edit(
            Command::ResolveApproval {
                approval_id: approval.approval_id,
                decision: ApprovalDecision::Allow,
            },
            approval.revision.into(),
        );
        let command_id = decision.command_id;
        assert_eq!(
            f.hub.submit(&Caller::local_owner(), decision).await.status,
            ReceiptStatus::Accepted
        );
        settle(&f.hub, command_id).await;
        assert!(
            matches!(pending.await.unwrap(), BridgeResponse::Permission { decision }
            if decision["behavior"] == "allow")
        );
        assert!(matches!(
            f.hub.handle(&binding, request).await,
            BridgeResponse::Refused { .. }
        ));
    }
    assert_ne!(
        ids[0], ids[1],
        "JSON numeric 7 and string 7 are distinct requests"
    );
    f.close().await;
}

#[tokio::test]
async fn initial_start_admission_preserves_availability_remedy_without_dispatch() {
    use protocol::capability::Availability;
    let f = Fixture::new().await;
    let project_id = f.project().await;
    for (availability, code) in [
        (
            Availability::TemporarilyUnavailable {
                reason: "Retry later".into(),
            },
            ErrorCode::TemporarilyUnavailable,
        ),
        (
            Availability::PermissionMissing {
                remedy: "Grant permission on the computer".into(),
            },
            ErrorCode::OsPermissionMissing,
        ),
    ] {
        let mut info = ports::HarnessAdapter::probe(f.harness.as_ref()).await;
        info.capabilities.start_run = availability.clone();
        f.hub
            .write(|tx| tx.put("meta", "harnesses", &vec![info]))
            .unwrap();
        let receipt = f
            .hub
            .submit(
                &Caller::local_owner(),
                CommandEnvelope::new(Command::CreateTask {
                    github: None,
                    agent_chosen: None,
                    project_id,
                    kind: TaskKind::Coding,
                    mode: Some(TaskMode::Work),
                    title: None,
                    prompt: "fixture".into(),
                    bundle: None,
                    harness: HarnessKind::ClaudeCode,
                    model: None,
                    effort: None,
                    launch_policy: None,
                    branch: None,
                    mission: None,
                    target_id: None,
                    placement: TaskPlacement::StartNow,
                    start: None,
                    place: None,
                    browser: None,
                    conversation: None,
                    access: None,
                    review: None,
                }),
            )
            .await;
        assert_eq!(receipt.status, ReceiptStatus::Rejected);
        let error = receipt.error.unwrap();
        assert_eq!(error.code, code);
        assert_eq!(error, domain::availability(&availability).unwrap_err());
    }
    assert_eq!(f.harness.starts.load(Ordering::SeqCst), 0);
    f.close().await;
}

#[path = "hand_back_tests.rs"]
mod hand_back;
#[path = "queue_completion_tests.rs"]
mod queue_completion_tests;
#[path = "results_tests.rs"]
mod result_evidence;
#[path = "control_tests.rs"]
mod safety;

#[path = "review_findings_tests.rs"]
mod review_findings_tests;
#[tokio::test]
async fn device_grants_change_end_input_and_repairing_after_revoking_all_restores_owner() {
    let f = Fixture::new().await;
    let first = f.device().await;
    let second = pair(&f.hub, 8).await;
    assert!(!second.grants.control_input);
    let Principal::Device { device_id } = second.principal else {
        unreachable!()
    };
    let set = |expected: Grants, grants: Grants| {
        CommandEnvelope::new(Command::SetDeviceGrants {
            device_id,
            expected: Some(expected),
            grants,
        })
    };
    // Only a device that may manage devices can change them.
    let refused = f
        .hub
        .submit(
            &second,
            set(
                domain::grants::device_defaults(),
                domain::grants::manager_defaults(),
            ),
        )
        .await;
    assert_eq!(refused.error.unwrap().code, ErrorCode::PermissionDenied);
    let r = f
        .hub
        .submit(
            &first,
            set(
                domain::grants::device_defaults(),
                domain::grants::manager_defaults(),
            ),
        )
        .await;
    assert_eq!(r.status, ReceiptStatus::Accepted, "{r:?}");
    let stored: crate::identity::Device = f.hub.read(|r| r.require("device", device_id)).unwrap();
    assert!(stored.record.grants.control_input && stored.record.grants.admin);
    let second = Caller {
        principal: second.principal,
        grants: Grants::owner(),
    };
    let target = f.desktop.target.lock().unwrap().target_id;
    let lease = CommandEnvelope::new(Command::RequestHumanLease {
        target_id: target,
        geometry_revision: 1,
    });
    let id = lease.command_id;
    f.hub.submit(&second, lease).await;
    settle(&f.hub, id).await;
    let view: TargetControlView = f.hub.read(|r| r.require("control", target)).unwrap();
    assert!(view.lease.is_some());
    // Taking input away ends the input it holds.
    let narrow = set(
        domain::grants::manager_defaults(),
        domain::grants::device_defaults(),
    );
    let id = narrow.command_id;
    f.hub.submit(&first, narrow).await;
    settle(&f.hub, id).await;
    let view: TargetControlView = f.hub.read(|r| r.require("control", target)).unwrap();
    assert_eq!(view.lease.unwrap().state, LeaseState::Revoked);
    // A revoked device keeps its revocation.
    let Principal::Device {
        device_id: first_id,
    } = first.principal
    else {
        unreachable!()
    };
    for id in [first_id, device_id] {
        let revoke = CommandEnvelope::new(Command::RevokeDevice { device_id: id });
        let cid = revoke.command_id;
        f.hub.submit(&Caller::local_owner(), revoke).await;
        settle(&f.hub, cid).await;
    }
    let r = f
        .hub
        .submit(
            &Caller::local_owner(),
            set(domain::grants::device_defaults(), Grants::owner()),
        )
        .await;
    assert_eq!(r.error.unwrap().code, ErrorCode::Conflict);
    // With every device revoked, the next one paired can manage the rest.
    let third = pair(&f.hub, 7).await;
    assert!(third.grants.admin && third.grants.control_input && !third.grants.accounts);
    f.close().await;
}
/// Every agent step moves a task's revision on, so a rename is fenced by the
/// title it replaces instead: it goes through while the agent works, and one
/// made from a title that has changed since is refused with the current one.
#[tokio::test]
async fn a_rename_is_fenced_by_the_title_it_replaces_not_by_agent_steps() {
    let f = Fixture::new().await;
    let owner = Caller::local_owner();
    let project = f.project().await;
    let (task, _) = f
        .task(project, TaskPlacement::StartNow, &owner, false)
        .await;
    let run = run_started(&f, task).await;
    let current = || f.hub.read(|r| r.require::<Task>("task", task)).unwrap();
    let reviewed = current().revision;
    for step in ["Read the index", "Ran the tests"] {
        f.harness
            .emit(
                run,
                ports::HarnessEvent::Activity {
                    key: step.into(),
                    tool: "Bash".into(),
                    summary: step.into(),
                    status: ActivityStatus::Succeeded,
                    detail: None,
                },
            )
            .await;
    }
    let mut more = waiting();
    while current().revision < reviewed + 2 && more() {
        tokio::task::yield_now().await;
    }
    assert!(current().revision >= reviewed + 2);
    let rename = |from: &str, to: &str| {
        edit(
            Command::RenameTask {
                task_id: task,
                title: to.into(),
                expected_title: Some(from.into()),
            },
            reviewed,
        )
    };
    let r = f
        .hub
        .submit(&owner, rename("Test task", "Faster search"))
        .await;
    assert_eq!(r.status, ReceiptStatus::Accepted, "{r:?}");
    assert_eq!(current().title, "Faster search");
    // A second rename from the same starting title would silently undo it.
    let r = f.hub.submit(&owner, rename("Test task", "Search")).await;
    let refused = r.error.unwrap();
    assert_eq!(refused.code, ErrorCode::StaleRevision);
    assert_eq!(
        refused.message,
        "Not renamed: this task was renamed to “Faster search” in the meantime."
    );
    assert_eq!(current().title, "Faster search");
    // A phone that predates the title fence is still fenced by the revision.
    let older = |revision| {
        edit(
            Command::RenameTask {
                task_id: task,
                title: "Search".into(),
                expected_title: None,
            },
            revision,
        )
    };
    let r = f.hub.submit(&owner, older(reviewed)).await;
    assert_eq!(r.error.unwrap().code, ErrorCode::StaleRevision);
    let r = f.hub.submit(&owner, older(current().revision)).await;
    assert_eq!(r.status, ReceiptStatus::Accepted, "{r:?}");
    assert_eq!(current().title, "Search");
    f.close().await;
}
#[tokio::test]
async fn backlog_pages_repeat_the_subscription_backlog() {
    let f = Fixture::new().await;
    let phone = f.device().await;
    for i in 0..3 {
        // Owner commands add progress events that a device may not see.
        let dir = f.project_dir().join(format!("project-{i}"));
        std::fs::create_dir_all(&dir).unwrap();
        let receipt = f
            .hub
            .submit(
                &Caller::local_owner(),
                CommandEnvelope::new(Command::AddProject {
                    host_path: dir.to_string_lossy().into_owned(),
                    name: None,
                }),
            )
            .await;
        assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
        f.hub
            .write(|tx| {
                tx.emit(Event::BootEpochChanged {
                    boot_epoch: f.hub.boot_epoch(),
                })
            })
            .unwrap();
    }
    let head = f.hub.journal_head();
    for caller in [Caller::local_owner(), phone.clone()] {
        for from in [0, 5, head - 3, head] {
            let Backlog::Events(expected) =
                f.hub.subscribe(&caller, Some(from)).await.unwrap().backlog
            else {
                panic!("retained");
            };
            let expected: Vec<u64> = expected.iter().map(|e| e.seq).collect();
            for limit in [1, 7, 1000] {
                let (mut cursor, mut paged) = (from, Vec::new());
                loop {
                    let (Backlog::Events(page), through) =
                        f.hub.backlog(&caller, cursor, head, limit).await.unwrap()
                    else {
                        panic!("retained");
                    };
                    assert!(page.len() <= limit && through > cursor || through == head);
                    paged.extend(page.iter().map(|e| e.seq));
                    if through >= head {
                        break;
                    }
                    cursor = through;
                }
                assert_eq!(paged, expected, "from {from}, {limit} per page");
            }
        }
    }
    let ahead = f.hub.backlog(&phone, head + 1, head, 10).await.unwrap_err();
    assert_eq!(ahead.code, ErrorCode::InvalidRequest);
    f.close().await;
}
#[tokio::test]
async fn subscribers_share_one_copy_of_each_event() {
    let f = Fixture::new().await;
    let phone = f.device().await;
    let mut owner = f.hub.subscribe(&Caller::local_owner(), None).await.unwrap();
    let mut device = f.hub.subscribe(&phone, None).await.unwrap();
    f.hub
        .write(|tx| {
            tx.emit(Event::BootEpochChanged {
                boot_epoch: f.hub.boot_epoch(),
            })
        })
        .unwrap();
    f.hub.live(protocol::event::LiveEvent::AgentTextDelta {
        task_id: TaskId::new(),
        run_id: RunId::new(),
        message_key: "m".into(),
        text: "text".into(),
    });
    let (Ok(Outbound::Event(a)), Ok(Outbound::Event(b))) =
        (owner.live.try_recv(), device.live.try_recv())
    else {
        panic!("both subscribers receive the event");
    };
    assert!(Arc::ptr_eq(&a, &b));
    let (Ok(Outbound::Live(a)), Ok(Outbound::Live(b))) =
        (owner.live.try_recv(), device.live.try_recv())
    else {
        panic!("both subscribers receive the text");
    };
    assert!(Arc::ptr_eq(&a, &b));
    f.close().await;
}
#[tokio::test]
async fn a_subscription_follows_its_device_grants_and_revocation() {
    // A task event and streamed text, then what the phone received of them.
    fn exchange(hub: &CompanionHub, subscription: &mut api::Subscription) -> Vec<&'static str> {
        hub.write(|tx| {
            tx.emit(Event::ProjectRemoved {
                project_id: ProjectId::new(),
            })
        })
        .unwrap();
        hub.live(protocol::event::LiveEvent::AgentTextDelta {
            task_id: TaskId::new(),
            run_id: RunId::new(),
            message_key: "m".into(),
            text: "text".into(),
        });
        let mut received = Vec::new();
        while let Ok(out) = subscription.live.try_recv() {
            match out {
                Outbound::Event(e) if matches!(e.event, Event::ProjectRemoved { .. }) => {
                    received.push("event")
                }
                Outbound::Live(_) => received.push("text"),
                _ => {}
            }
        }
        received
    }
    async fn owner_command(hub: &CompanionHub, command: Command) {
        let command = CommandEnvelope::new(command);
        let id = command.command_id;
        let receipt = hub.submit(&Caller::local_owner(), command).await;
        assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
        settle(hub, id).await;
    }
    async fn set_tasks(hub: &CompanionHub, device_id: DeviceId, tasks: bool) {
        let stored: crate::identity::Device = hub.read(|r| r.require("device", device_id)).unwrap();
        let grants = stored.record.grants;
        owner_command(
            hub,
            Command::SetDeviceGrants {
                device_id,
                expected: Some(grants.clone()),
                grants: Grants { tasks, ..grants },
            },
        )
        .await;
    }
    let f = Fixture::new().await;
    let phone = f.device().await;
    let Principal::Device { device_id } = phone.principal else {
        unreachable!()
    };
    let mut subscription = f.hub.subscribe(&phone, None).await.unwrap();
    assert_eq!(exchange(&f.hub, &mut subscription), ["event", "text"]);
    assert_eq!(exchange(&f.hub, &mut subscription), ["event", "text"]);
    set_tasks(&f.hub, device_id, false).await;
    assert!(exchange(&f.hub, &mut subscription).is_empty());
    set_tasks(&f.hub, device_id, true).await;
    assert_eq!(exchange(&f.hub, &mut subscription), ["event", "text"]);
    owner_command(&f.hub, Command::RevokeDevice { device_id }).await;
    assert!(exchange(&f.hub, &mut subscription).is_empty());
    f.close().await;
}
#[tokio::test]
async fn activity_keeps_the_native_tool_call_key() {
    let f = Fixture::new().await;
    let owner = Caller::local_owner();
    let project = f.project().await;
    let (task, _) = f
        .task(project, TaskPlacement::StartNow, &owner, false)
        .await;
    let run = run_started(&f, task).await;
    for key in ["tool-1", ""] {
        f.harness
            .emit(
                run,
                ports::HarnessEvent::Activity {
                    key: key.into(),
                    tool: "Bash".into(),
                    summary: "Bash finished".into(),
                    status: ActivityStatus::Succeeded,
                    detail: None,
                },
            )
            .await;
    }
    let keys = loop {
        let Backlog::Events(events) = f.hub.subscribe(&owner, Some(0)).await.unwrap().backlog
        else {
            panic!("fresh journal expected");
        };
        let keys: Vec<Option<String>> = events
            .into_iter()
            .filter_map(|e| match &e.event {
                Event::TimelineAppended { item, .. } => match &item.item {
                    TimelineBody::Activity { key, .. } => Some(key.clone()),
                    _ => None,
                },
                _ => None,
            })
            .collect();
        if keys.len() == 2 {
            break keys;
        }
        tokio::task::yield_now().await;
    };
    assert_eq!(keys, [Some("tool-1".into()), None]);
    f.close().await;
}
#[path = "observed_tests.rs"]
mod observed;

#[path = "project_stage_tests.rs"]
mod project_stage_tests;

#[path = "device_activity_tests.rs"]
mod device_activity_tests;

#[path = "ask_tests.rs"]
mod ask_tests;
#[path = "checklist_tests.rs"]
mod checklist_tests;
#[path = "native_text_tests.rs"]
mod native_text_tests;
#[path = "reported_tests.rs"]
mod reported_tests;
#[path = "unarchive_tests.rs"]
mod unarchive_tests;
#[path = "usage_limit_tests.rs"]
mod usage_limit_tests;
#[path = "waiting_tests.rs"]
mod waiting_tests;
#[path = "watch_tests.rs"]
mod watch_tests;

/// Answer one request with `answer`, a whole HTTP response.
async fn page_server(answer: &'static str) -> url::Url {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = url::Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = [0; 2048];
        let _ = stream.read(&mut request).await.unwrap();
        stream.write_all(answer.as_bytes()).await.unwrap();
    });
    url
}

#[tokio::test]
async fn preview_http_names_a_site_only_by_a_title_within_what_is_read() {
    use crate::preview::{PageAnswer, PreviewChecker, TITLE_WITHIN};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let checker = preview::HttpPreviewChecker::new().unwrap();
    let head = "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nConnection: close\r\n\r\n";
    // A title is named once `</title` is read, so its last byte decides.
    let title = "<title>Chosen</title";
    // The page in two pieces, the second sent a moment later, so it comes
    // to be read on its own.
    for (first, then, expected) in [
        // Ending exactly where reading stops.
        (
            TITLE_WITHIN - title.len(),
            title.to_string(),
            Some("Chosen"),
        ),
        // Straddling it, by a byte.
        (TITLE_WITHIN - title.len() + 1, title.to_string(), None),
        // Beyond it.
        (TITLE_WITHIN - 1, format!("x{title}"), None),
    ] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0; 2048];
            let _ = stream.read(&mut request).await.unwrap();
            let first = format!("{head}{}", " ".repeat(first));
            stream.write_all(first.as_bytes()).await.unwrap();
            stream.flush().await.unwrap();
            tokio::time::sleep(Duration::from_millis(100)).await;
            let _ = stream.write_all(then.as_bytes()).await;
        });
        let answer = checker.page(&url::Url::parse(&url).unwrap()).await.unwrap();
        let expected = PageAnswer::Page(expected.map(String::from));
        assert_eq!(answer, expected, "{first}");
    }
}

#[tokio::test]
async fn preview_http_tells_a_site_from_other_servers() {
    use crate::preview::{PageAnswer, PreviewChecker};
    let checker = preview::HttpPreviewChecker::new().unwrap();
    let html = "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nConnection: close\r\n\r\n\
                <!doctype html><html><head><TITLE>\n  Shop &amp; Cart\n</TITLE></head></html>";
    for (answer, expected) in [
        (html, PageAnswer::Page(Some("\n  Shop & Cart\n".into()))),
        (
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
            PageAnswer::Other,
        ),
        // Express's own answer when nothing serves `/`.
        (
            "HTTP/1.1 404 Not Found\r\nContent-Type: text/html\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            PageAnswer::Other,
        ),
        (
            "HTTP/1.1 302 Found\r\nLocation: /login\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            PageAnswer::Page(None),
        ),
        (
            "HTTP/1.1 302 Found\r\nLocation: http://8.8.8.8/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            PageAnswer::Other,
        ),
        (
            "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:1/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            PageAnswer::Other,
        ),
        (
            "HTTP/1.1 500 Internal Server Error\r\nContent-Type: text/html\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            PageAnswer::Silent,
        ),
    ] {
        let url = page_server(answer).await;
        assert_eq!(checker.page(&url).await.unwrap(), expected, "{answer}");
    }
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = url::Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
    drop(listener);
    assert_eq!(checker.page(&url).await.unwrap(), PageAnswer::Silent);
}
