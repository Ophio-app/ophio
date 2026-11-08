// SPDX-License-Identifier: Apache-2.0
use super::*;
use crate::{
    Hub,
    testing::{FakeClock, FakeDesktop},
};
use async_trait::async_trait;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ports::{
    PlatformError,
    windows::{WindowActions, WindowAuthority},
};
use protocol::{
    auth::*,
    capability::Availability,
    model::{FactStatus, InterventionKind},
    *,
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize},
    },
    time::Duration,
};
#[derive(Debug, Default)]
struct FakeWindows {
    effects: AtomicUsize,
    delay: AtomicBool,
    entered: tokio::sync::Notify,
    proceed: tokio::sync::Notify,
    /// An app launch hangs and can't be confirmed stopped.
    unstoppable: AtomicBool,
}
#[async_trait]
impl WindowActions for FakeWindows {
    async fn capabilities(&self) -> WindowCapabilities {
        WindowCapabilities {
            backend: "fixture".into(),
            windows: Availability::Available,
            open_apps: Availability::Available,
        }
    }
    async fn catalog(&self, _: &str) -> Result<WindowCatalog, PlatformError> {
        Ok(WindowCatalog {
            capabilities: self.capabilities().await,
            windows: vec![],
            apps: vec![],
            observed_at: protocol::time::Timestamp(0),
            truncated: false,
        })
    }
    async fn act(
        &self,
        _: &WindowAction,
        authority: Arc<dyn WindowAuthority>,
    ) -> Result<(WindowOutcome, String), PlatformError> {
        if self.delay.load(Ordering::SeqCst) {
            self.entered.notify_one();
            self.proceed.notified().await;
        }
        authority.check()?;
        if self.unstoppable.load(Ordering::SeqCst) {
            return Err(PlatformError::LaunchUnstopped);
        }
        self.effects.fetch_add(1, Ordering::SeqCst);
        Ok((
            WindowOutcome::CloseRequested,
            "Close requested; unsaved work may prompt".into(),
        ))
    }
}
async fn settled(
    hub: &CompanionHub,
    command: CommandEnvelope,
    caller: &Caller,
) -> store::StoredCommand {
    let id = command.command_id;
    hub.submit(caller, command).await;
    tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            if let Some(c) = hub.read(|r| r.command(id)).unwrap()
                && !c.pending
            {
                return c;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap()
}
#[tokio::test]
async fn lease_epoch_generation_deadline_local_stop_and_receipt() {
    let path = std::env::temp_dir().join(format!("window-service-test-{}", CommandId::new()));
    let clock = Arc::new(FakeClock::default());
    let desktop = Arc::new(FakeDesktop::default());
    let windows = Arc::new(FakeWindows::default());
    let hub = CompanionHub::builder()
        .data_dir(&path)
        .clock(clock.clone())
        .desktop(desktop.clone())
        .window_actions(windows.clone())
        .build()
        .await
        .unwrap();
    let ticket = hub.open_pairing(&Caller::local_owner()).await.unwrap();
    let paired = crate::testing::pair_confirmed(
        &hub,
        PairRequest {
            pairing_secret: ticket.secret,
            device_public_key: URL_SAFE_NO_PAD.encode([7; 32]),
            device_name: "Fixture phone".into(),
            device_platform: "test".into(),
            client_version: "test".into(),
            protocol: ProtocolVersion::CURRENT,
            shows_code: true,
            replaces: None,
        },
    )
    .await
    .unwrap();
    let caller = Caller {
        principal: Principal::Device {
            device_id: paired.device_id,
        },
        grants: paired.grants,
    };
    let target_id = desktop.target.lock().unwrap().target_id;
    let c = settled(
        &hub,
        CommandEnvelope::new(Command::RequestHumanLease {
            target_id,
            geometry_revision: 1,
        }),
        &caller,
    )
    .await;
    let Some(CommandResult::LeaseGranted { lease }) = c.result else {
        panic!("lease: {:?}", c.error)
    };
    let control = WindowControl {
        target_id,
        lease_id: lease.lease_id,
        boot_epoch: lease.boot_epoch,
        generation: lease.generation,
        geometry_revision: 1,
    };
    let action = WindowAction::Close {
        window_id: "fixture".into(),
    };
    let command = CommandEnvelope::new(Command::WindowAction {
        control: control.clone(),
        action: action.clone(),
    });
    let work = settled(&hub, command.clone(), &caller).await;
    let Some(CommandResult::WindowAction { receipt }) = work.result else {
        panic!("receipt: {:?}", work.error)
    };
    assert_eq!(receipt.lease_id, lease.lease_id);
    assert_eq!(receipt.target_id, target_id);
    assert_eq!(receipt.outcome, WindowOutcome::CloseRequested);
    let events = hub.read(|r| Ok(r.journal(0)?.events)).unwrap();
    assert!(events.iter().any(|event| matches!(&event.event, Event::InterventionRecorded { target_id: Some(id), receipt, .. } if *id == target_id && receipt.kind == InterventionKind::WindowAction && receipt.facts[0].status == FactStatus::Done && receipt.facts[0].detail.as_ref().unwrap().contains(&lease.lease_id.to_string()))));
    assert_eq!(
        hub.submit(&caller, command).await.status,
        ReceiptStatus::Duplicate
    );
    assert_eq!(windows.effects.load(Ordering::SeqCst), 1);
    for mutant in [
        WindowControl {
            generation: control.generation + 1,
            ..control.clone()
        },
        WindowControl {
            boot_epoch: BootEpoch::new(),
            ..control.clone()
        },
        WindowControl {
            geometry_revision: 2,
            ..control.clone()
        },
    ] {
        assert!(hub.window_action(&caller, &mutant, &action).await.is_err());
    }
    hub.inner.local_stops.store(1, Ordering::SeqCst);
    assert!(hub.window_action(&caller, &control, &action).await.is_err());
    hub.inner.local_stops.store(0, Ordering::SeqCst);
    windows.delay.store(true, Ordering::SeqCst);
    let running = {
        let hub = hub.clone();
        let caller = caller.clone();
        let control = control.clone();
        let action = action.clone();
        tokio::spawn(async move { hub.window_action(&caller, &control, &action).await })
    };
    windows.entered.notified().await;
    clock.advance(Duration::from_secs(4));
    windows.proceed.notify_one();
    assert!(running.await.unwrap().is_err());
    assert_eq!(windows.effects.load(Ordering::SeqCst), 1);
    assert!(hub.window_action(&caller, &control, &action).await.is_err());
    hub.shutdown().await.unwrap();
    drop(hub);
    std::fs::remove_dir_all(path).unwrap();
}

/// Window titles and apps show what is on the screen, so a locked computer
/// refuses them as it refuses a capture.
#[tokio::test]
async fn the_window_list_is_refused_while_the_computer_is_locked() {
    let path = std::env::temp_dir().join(format!("window-service-test-{}", CommandId::new()));
    let desktop = Arc::new(FakeDesktop::default());
    let hub = CompanionHub::builder()
        .data_dir(&path)
        .clock(Arc::new(FakeClock::default()))
        .desktop(desktop.clone())
        .window_actions(Arc::new(FakeWindows::default()))
        .build()
        .await
        .unwrap();
    let owner = Caller::local_owner();
    let windows = || Query::Windows {
        search: String::new(),
    };
    assert!(hub.query(&owner, windows()).await.is_ok());
    let locked = settled(&hub, CommandEnvelope::new(Command::LockDesktop), &owner).await;
    assert!(locked.error.is_none(), "{:?}", locked.error);
    let Err(refused) = hub.query(&owner, windows()).await else {
        panic!("the window list of a locked computer")
    };
    assert_eq!(refused.code, ErrorCode::TemporarilyUnavailable);
    assert_eq!(refused.message, "Computer is locked");
    hub.shutdown().await.unwrap();
    drop(hub);
    std::fs::remove_dir_all(path).unwrap();
}

/// A desktop that observes its lock, as Linux's does.
#[derive(Debug)]
struct ObservedDesktop {
    native: FakeDesktop,
    lock: tokio::sync::watch::Sender<protocol::control::DesktopLockState>,
}
#[async_trait]
impl ports::DesktopBackend for ObservedDesktop {
    async fn capabilities(&self) -> ports::DesktopCapabilities {
        self.native.capabilities().await
    }
    async fn targets(&self) -> Result<Vec<control::ControlTarget>, PlatformError> {
        self.native.targets().await
    }
    async fn capture_still(&self, id: TargetId) -> Result<ports::CapturedFrame, PlatformError> {
        self.native.capture_still(id).await
    }
    async fn open_stream(
        &self,
        id: TargetId,
        fps: u8,
    ) -> Result<Box<dyn ports::FrameStream>, PlatformError> {
        self.native.open_stream(id, fps).await
    }
    async fn input_sink(&self, id: TargetId) -> Result<Box<dyn ports::InputSink>, PlatformError> {
        self.native.input_sink(id).await
    }
    async fn lock_session(&self) -> Result<(), PlatformError> {
        self.lock
            .send_replace(protocol::control::DesktopLockState::Locked);
        Ok(())
    }
    async fn desktop_lock(&self) -> protocol::control::DesktopLockState {
        *self.lock.borrow()
    }
    fn lock_changes(
        &self,
    ) -> Option<tokio::sync::watch::Receiver<protocol::control::DesktopLockState>> {
        Some(self.lock.subscribe())
    }
}

/// A desktop that observes its lock but cannot tell whether it is locked
/// refuses the window list, as it refuses a capture.
#[tokio::test]
async fn the_window_list_is_refused_while_an_observed_lock_state_is_unknown() {
    use protocol::control::DesktopLockState;
    let path = std::env::temp_dir().join(format!("window-service-test-{}", CommandId::new()));
    let desktop = Arc::new(ObservedDesktop {
        native: FakeDesktop::default(),
        lock: tokio::sync::watch::channel(DesktopLockState::Unlocked).0,
    });
    let hub = CompanionHub::builder()
        .data_dir(&path)
        .clock(Arc::new(FakeClock::default()))
        .desktop(desktop.clone())
        .window_actions(Arc::new(FakeWindows::default()))
        .build()
        .await
        .unwrap();
    let owner = Caller::local_owner();
    let windows = || Query::Windows {
        search: String::new(),
    };
    assert!(hub.query(&owner, windows()).await.is_ok());
    desktop.lock.send_replace(DesktopLockState::Unknown);
    let Err(refused) = hub.query(&owner, windows()).await else {
        panic!("the window list while the lock state is unknown")
    };
    assert_eq!(refused.code, ErrorCode::TemporarilyUnavailable);
    assert_eq!(refused.message, "Computer lock state is unknown");
    desktop.lock.send_replace(DesktopLockState::Unlocked);
    assert!(hub.query(&owner, windows()).await.is_ok());
    hub.shutdown().await.unwrap();
    drop(hub);
    std::fs::remove_dir_all(path).unwrap();
}

/// Opening an app from the phone whose launch couldn't be confirmed stopped
/// is kept, like the agent's and voice's: the person's control ends, and no
/// later Stop or shutdown claims a clean release while an app may still open.
#[tokio::test]
async fn an_app_whose_launch_could_not_be_stopped_is_never_reported_as_released() {
    let path = std::env::temp_dir().join(format!("window-service-test-{}", CommandId::new()));
    let desktop = Arc::new(FakeDesktop::default());
    let windows = Arc::new(FakeWindows::default());
    windows.unstoppable.store(true, Ordering::SeqCst);
    let hub = CompanionHub::builder()
        .data_dir(&path)
        .clock(Arc::new(FakeClock::default()))
        .desktop(desktop.clone())
        .window_actions(windows.clone())
        .build()
        .await
        .unwrap();
    let ticket = hub.open_pairing(&Caller::local_owner()).await.unwrap();
    let paired = crate::testing::pair_confirmed(
        &hub,
        PairRequest {
            pairing_secret: ticket.secret,
            device_public_key: URL_SAFE_NO_PAD.encode([7; 32]),
            device_name: "Fixture phone".into(),
            device_platform: "test".into(),
            client_version: "test".into(),
            protocol: ProtocolVersion::CURRENT,
            shows_code: true,
            replaces: None,
        },
    )
    .await
    .unwrap();
    let caller = Caller {
        principal: Principal::Device {
            device_id: paired.device_id,
        },
        grants: paired.grants,
    };
    let target_id = desktop.target.lock().unwrap().target_id;
    let c = settled(
        &hub,
        CommandEnvelope::new(Command::RequestHumanLease {
            target_id,
            geometry_revision: 1,
        }),
        &caller,
    )
    .await;
    let Some(CommandResult::LeaseGranted { lease }) = c.result else {
        panic!("lease: {:?}", c.error)
    };
    let control = WindowControl {
        target_id,
        lease_id: lease.lease_id,
        boot_epoch: lease.boot_epoch,
        generation: lease.generation,
        geometry_revision: 1,
    };
    let open = WindowAction::OpenApp {
        app_id: "fixture".into(),
    };
    let failed = hub
        .window_action(&caller, &control, &open)
        .await
        .unwrap_err();
    assert_eq!(failed.code, ErrorCode::OutcomeUnknown);
    assert!(failed.message.contains("may still open"), "{failed:?}");
    for _ in 0..2 {
        let stop = hub.revoke_target(target_id, false).await.unwrap_err();
        assert!(stop.message.contains("may still open"), "{stop:?}");
    }
    // Locking the computer is exactly what the person may want now: it
    // locks, and says an app may still open.
    let locked = hub
        .control_command(&Caller::local_owner(), &Command::LockDesktop)
        .await
        .unwrap_err();
    assert!(locked.message.contains("may still open"), "{locked:?}");
    assert!(
        locked.message.starts_with("The computer is locked"),
        "{locked:?}"
    );
    assert_eq!(desktop.locks.load(Ordering::SeqCst), 1);
    let shutdown = hub.shutdown().await.unwrap_err();
    assert!(shutdown.message.contains("may still open"), "{shutdown:?}");
    drop(hub);
    let _ = std::fs::remove_dir_all(path);
}

/// The same launch while its receipt can't be written still leaves the
/// mark, and says an app may still open rather than that it finished.
#[tokio::test]
async fn an_unstoppable_launch_is_kept_when_its_receipt_cannot_be_written() {
    let path = std::env::temp_dir().join(format!("window-service-test-{}", CommandId::new()));
    let desktop = Arc::new(FakeDesktop::default());
    let windows = Arc::new(FakeWindows::default());
    windows.unstoppable.store(true, Ordering::SeqCst);
    let hub = CompanionHub::builder()
        .data_dir(&path)
        .clock(Arc::new(FakeClock::default()))
        .desktop(desktop.clone())
        .window_actions(windows.clone())
        .build()
        .await
        .unwrap();
    let ticket = hub.open_pairing(&Caller::local_owner()).await.unwrap();
    let paired = crate::testing::pair_confirmed(
        &hub,
        PairRequest {
            pairing_secret: ticket.secret,
            device_public_key: URL_SAFE_NO_PAD.encode([7; 32]),
            device_name: "Fixture phone".into(),
            device_platform: "test".into(),
            client_version: "test".into(),
            protocol: ProtocolVersion::CURRENT,
            shows_code: true,
            replaces: None,
        },
    )
    .await
    .unwrap();
    let caller = Caller {
        principal: Principal::Device {
            device_id: paired.device_id,
        },
        grants: paired.grants,
    };
    let target_id = desktop.target.lock().unwrap().target_id;
    let c = settled(
        &hub,
        CommandEnvelope::new(Command::RequestHumanLease {
            target_id,
            geometry_revision: 1,
        }),
        &caller,
    )
    .await;
    let Some(CommandResult::LeaseGranted { lease }) = c.result else {
        panic!("lease: {:?}", c.error)
    };
    let control = WindowControl {
        target_id,
        lease_id: lease.lease_id,
        boot_epoch: lease.boot_epoch,
        generation: lease.generation,
        geometry_revision: 1,
    };
    let db = rusqlite::Connection::open(path.join("companion.sqlite")).unwrap();
    db.execute_batch(
        "CREATE TRIGGER refuse_journal BEFORE INSERT ON journal BEGIN SELECT RAISE(ABORT, 'disk full'); END;",
    )
    .unwrap();
    let failed = hub
        .window_action(
            &caller,
            &control,
            &WindowAction::OpenApp {
                app_id: "fixture".into(),
            },
        )
        .await
        .unwrap_err();
    assert!(failed.message.contains("may still open"), "{failed:?}");
    db.execute_batch("DROP TRIGGER refuse_journal;").unwrap();
    let stop = hub.revoke_target(target_id, false).await.unwrap_err();
    assert!(stop.message.contains("may still open"), "{stop:?}");
    let _ = hub.shutdown().await;
    drop(hub);
    let _ = std::fs::remove_dir_all(path);
}

/// A phone holding a display, with `windows` as its window adapter.
async fn leased(
    windows: Arc<FakeWindows>,
) -> (
    CompanionHub,
    Caller,
    WindowControl,
    Arc<FakeDesktop>,
    std::path::PathBuf,
) {
    let path = std::env::temp_dir().join(format!("window-service-test-{}", CommandId::new()));
    let desktop = Arc::new(FakeDesktop::default());
    let hub = CompanionHub::builder()
        .data_dir(&path)
        .clock(Arc::new(FakeClock::default()))
        .desktop(desktop.clone())
        .window_actions(windows)
        .build()
        .await
        .unwrap();
    let ticket = hub.open_pairing(&Caller::local_owner()).await.unwrap();
    let paired = crate::testing::pair_confirmed(
        &hub,
        PairRequest {
            pairing_secret: ticket.secret,
            device_public_key: URL_SAFE_NO_PAD.encode([7; 32]),
            device_name: "Fixture phone".into(),
            device_platform: "test".into(),
            client_version: "test".into(),
            protocol: ProtocolVersion::CURRENT,
            shows_code: true,
            replaces: None,
        },
    )
    .await
    .unwrap();
    let caller = Caller {
        principal: Principal::Device {
            device_id: paired.device_id,
        },
        grants: paired.grants,
    };
    let target_id = desktop.target.lock().unwrap().target_id;
    let c = settled(
        &hub,
        CommandEnvelope::new(Command::RequestHumanLease {
            target_id,
            geometry_revision: 1,
        }),
        &caller,
    )
    .await;
    let Some(CommandResult::LeaseGranted { lease }) = c.result else {
        panic!("lease: {:?}", c.error)
    };
    let control = WindowControl {
        target_id,
        lease_id: lease.lease_id,
        boot_epoch: lease.boot_epoch,
        generation: lease.generation,
        geometry_revision: 1,
    };
    (hub, caller, control, desktop, path)
}

/// On a display with an unresolved launch whose input also couldn't be let
/// go, Lock behaves as on any display with input held: it doesn't lock,
/// and says both things plainly.
#[tokio::test]
async fn lock_refuses_when_input_also_failed_to_release_on_a_marked_display() {
    let windows = Arc::new(FakeWindows::default());
    windows.unstoppable.store(true, Ordering::SeqCst);
    let (hub, caller, control, desktop, path) = leased(windows).await;
    desktop.input.fail_release.store(true, Ordering::SeqCst);
    let failed = hub
        .window_action(
            &caller,
            &control,
            &WindowAction::OpenApp {
                app_id: "fixture".into(),
            },
        )
        .await
        .unwrap_err();
    assert!(failed.message.contains("may still open"), "{failed:?}");
    // Its answer says input wasn't let go too.
    assert_eq!(failed, crate::control::release_and_launch_unresolved());
    let refused = hub
        .control_command(&Caller::local_owner(), &Command::LockDesktop)
        .await
        .unwrap_err();
    assert_eq!(desktop.locks.load(Ordering::SeqCst), 0, "{refused:?}");
    assert!(refused.message.contains("may still open"), "{refused:?}");
    assert!(refused.message.contains("input"), "{refused:?}");
    let _ = hub.shutdown().await;
    drop(hub);
    let _ = std::fs::remove_dir_all(path);
}

/// Once a display is marked, letting go of it doesn't wait for launches
/// again; and turning sharing off waits for launches without holding the
/// table of all displays, which renewals on other displays need.
#[tokio::test]
async fn waiting_for_launches_never_stalls_a_marked_display_or_the_other_displays() {
    let windows = Arc::new(FakeWindows::default());
    windows.unstoppable.store(true, Ordering::SeqCst);
    let (hub, caller, control, _desktop, path) = leased(windows).await;
    hub.window_action(
        &caller,
        &control,
        &WindowAction::OpenApp {
            app_id: "fixture".into(),
        },
    )
    .await
    .unwrap_err();
    // A launch that never settles, on the marked display.
    let target = hub.target(control.target_id).await.unwrap();
    let launches = target.lock().await.launches.clone();
    let _running = launches.begin();
    let began = tokio::time::Instant::now();
    assert!(hub.revoke_target(control.target_id, false).await.is_err());
    assert!(
        began.elapsed() < Duration::from_secs(1),
        "{:?}",
        began.elapsed()
    );
    // Unmarked, a sharing change waits for it, but not with the table held.
    target.lock().await.unresolved_launch = false;
    let off = {
        let hub = hub.clone();
        tokio::spawn(async move {
            hub.set_desktop(
                None,
                Availability::TemporarilyUnavailable {
                    reason: "test".into(),
                },
            )
            .await
        })
    };
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(500), hub.inner.controls.lock())
            .await
            .is_ok(),
        "the table of displays is held while launches settle"
    );
    let _ = off.await;
    let _ = hub.shutdown().await;
    drop(hub);
    let _ = std::fs::remove_dir_all(path);
}

/// Renewing the phone's lease on one display never waits on another
/// display's lock, which a release or a launch may hold for seconds.
#[tokio::test]
async fn a_renewal_never_waits_on_another_displays_lock() {
    let (hub, caller, control, desktop, path) = leased(Arc::new(FakeWindows::default())).await;
    let template = desktop.target.lock().unwrap().clone();
    let others: Vec<TargetId> = (0..32)
        .map(|_| {
            let mut other = template.clone();
            other.target_id = TargetId::new();
            desktop.extra.lock().unwrap().push(other.clone());
            other.target_id
        })
        .collect();
    // Every other display is busy: its lock is held.
    let mut held = Vec::new();
    let mut guards = Vec::new();
    for id in &others {
        held.push(hub.target(*id).await.unwrap());
    }
    for target in &held {
        guards.push(target.lock().await);
    }
    let renewed = tokio::time::timeout(
        Duration::from_millis(500),
        hub.renew(&caller, control.lease_id, control.generation),
    )
    .await;
    assert!(matches!(renewed, Ok(Ok(()))), "{renewed:?}");
    drop(guards);
    let _ = hub.shutdown().await;
    drop(hub);
    let _ = std::fs::remove_dir_all(path);
}

/// Before waiting for launches, a display about to be released is fenced:
/// no lease on it can act or start a launch any more, so none can begin
/// after the wait has passed it.
#[tokio::test]
async fn displays_are_fenced_before_their_launches_are_waited_for() {
    let (hub, caller, control, _desktop, path) = leased(Arc::new(FakeWindows::default())).await;
    let target = hub.target(control.target_id).await.unwrap();
    hub.settle_launches(vec![target.clone()]).await;
    let live = target.lock().await.live.clone();
    assert!(!live.holds(
        control.lease_id,
        control.generation,
        hub.inner.clock.monotonic()
    ));
    assert!(
        hub.renew(&caller, control.lease_id, control.generation)
            .await
            .is_err()
    );
    let _ = hub.shutdown().await;
    drop(hub);
    let _ = std::fs::remove_dir_all(path);
}

/// Turning sharing off reports the worst of what it couldn't let go, in any
/// order of the displays: held input before an app that may still open.
#[tokio::test]
async fn sharing_off_reports_held_input_before_an_unresolved_launch_in_any_order() {
    let (hub, _caller, control, desktop, path) = leased(Arc::new(FakeWindows::default())).await;
    let template = desktop.target.lock().unwrap().clone();
    let mut ids = vec![control.target_id];
    for _ in 0..16 {
        let mut other = template.clone();
        other.target_id = TargetId::new();
        desktop.extra.lock().unwrap().push(other.clone());
        ids.push(other.target_id);
    }
    // One display can't let go of its input; every other has only an
    // unresolved launch.
    let held = ids[1];
    desktop.input.fail_release_on.lock().unwrap().push(held);
    for id in &ids {
        let target = hub.target(*id).await.unwrap();
        if *id != held {
            target.lock().await.unresolved_launch = true;
        }
    }
    let off = hub
        .set_desktop(
            None,
            Availability::TemporarilyUnavailable {
                reason: "test".into(),
            },
        )
        .await
        .unwrap_err();
    assert_eq!(off, crate::control::release_unconfirmed(), "{off:?}");
    let _ = hub.shutdown().await;
    drop(hub);
    let _ = std::fs::remove_dir_all(path);
}

/// Local Stop reports the worst of what it couldn't let go, in any order
/// of the displays: input that may still be held on one display is never
/// hidden behind another's app that may still open.
#[tokio::test]
async fn local_stop_reports_held_input_before_an_unresolved_launch_in_any_order() {
    let (hub, _caller, control, desktop, path) = leased(Arc::new(FakeWindows::default())).await;
    let template = desktop.target.lock().unwrap().clone();
    let mut ids = vec![control.target_id];
    for _ in 0..16 {
        let mut other = template.clone();
        other.target_id = TargetId::new();
        desktop.extra.lock().unwrap().push(other.clone());
        ids.push(other.target_id);
    }
    // One display can't let go of its input; every other has only an
    // unresolved launch.
    let held = ids[1];
    desktop.input.fail_release_on.lock().unwrap().push(held);
    for id in &ids {
        let target = hub.target(*id).await.unwrap();
        if *id != held {
            target.lock().await.unresolved_launch = true;
        }
    }
    let stopped = hub.stop_all_control().await.unwrap_err();
    assert_eq!(
        stopped,
        crate::control::release_unconfirmed(),
        "{stopped:?}"
    );
    let _ = hub.shutdown().await;
    drop(hub);
    let _ = std::fs::remove_dir_all(path);
}

/// Every path that lets go of several displays keeps the worst release,
/// whichever order the failures come in, and the first of equals.
#[test]
fn the_worst_release_is_kept_in_any_order() {
    use crate::control::{
        keep_worse, launch_unresolved, release_and_launch_unresolved, release_unconfirmed,
    };
    let unrecorded = crate::error(ErrorCode::Internal, "unrecorded");
    let failures = [
        unrecorded.clone(),
        launch_unresolved(),
        release_unconfirmed(),
        release_and_launch_unresolved(),
    ];
    for worst in 0..failures.len() {
        // Every order of these four.
        let mut order: Vec<usize> = (0..failures.len()).collect();
        for rotation in 0..failures.len() {
            order.rotate_left(1);
            for reversed in [false, true] {
                let mut seen = order.clone();
                if reversed {
                    seen.reverse();
                }
                let mut kept = None;
                for i in seen.iter().filter(|i| **i <= worst) {
                    keep_worse(&mut kept, Err(failures[*i].clone()));
                    keep_worse(&mut kept, Ok(()));
                }
                assert_eq!(kept.as_ref(), Some(&failures[worst]), "{rotation} {seen:?}");
            }
        }
    }
    // Equals keep the first.
    let mut kept = None;
    keep_worse(&mut kept, Err(crate::error(ErrorCode::Internal, "first")));
    keep_worse(&mut kept, Err(unrecorded));
    assert_eq!(kept.unwrap().message, "first");
}
