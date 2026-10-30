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
