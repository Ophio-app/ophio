// SPDX-License-Identifier: Apache-2.0
use super::*;
use crate::api::Caller;
use ports::{
    CapturedFrame, DesktopBackend, DesktopCapabilities, FrameStream, InputSink, PlatformError,
    UnlockOutcome,
};
use protocol::accounts::Secret;
use protocol::auth::{Grants, Principal};
use protocol::capability::Availability;
use std::collections::VecDeque;
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize};
use tokio::sync::{Notify, watch};

const PASSWORD: &str = "synthetic computer password";

/// A locked desktop that answers each password as it is told to, and reports
/// that it unlocked by a signal or only when asked.
#[derive(Debug)]
struct Lockscreen {
    native: FakeDesktop,
    lock: watch::Sender<DesktopLockState>,
    signals: bool,
    /// Whether a released lock screen reports that it unlocked.
    confirms: AtomicBool,
    /// Whether it reports that only when asked, its signal lost.
    unsignalled: AtomicBool,
    answers: Mutex<VecDeque<Result<UnlockOutcome, PlatformError>>>,
    asked: AtomicUsize,
    /// Whether checking a password takes a while.
    slow: AtomicBool,
    /// Whether checking a password waits for `go`.
    held: AtomicBool,
    go: Notify,
    /// How many right passwords `allowed` let through.
    passed: AtomicUsize,
    /// Whether releasing the lock, once allowed, waits for `release_go`.
    release_held: AtomicBool,
    release_go: Notify,
    released: AtomicUsize,
}
impl Lockscreen {
    fn new(signals: bool) -> Arc<Self> {
        Arc::new(Self {
            native: FakeDesktop::default(),
            lock: watch::channel(DesktopLockState::Locked).0,
            signals,
            confirms: AtomicBool::new(true),
            unsignalled: AtomicBool::new(false),
            answers: Mutex::default(),
            asked: AtomicUsize::new(0),
            slow: AtomicBool::new(false),
            held: AtomicBool::new(false),
            go: Notify::new(),
            passed: AtomicUsize::new(0),
            release_held: AtomicBool::new(false),
            release_go: Notify::new(),
            released: AtomicUsize::new(0),
        })
    }
    fn answer(&self, answer: Result<UnlockOutcome, PlatformError>) {
        self.answers.lock().unwrap().push_back(answer);
    }
    fn asked(&self) -> usize {
        self.asked.load(Ordering::SeqCst)
    }
}
#[async_trait::async_trait]
impl DesktopBackend for Lockscreen {
    async fn capabilities(&self) -> DesktopCapabilities {
        self.native.capabilities().await
    }
    async fn targets(&self) -> Result<Vec<ControlTarget>, PlatformError> {
        self.native.targets().await
    }
    async fn capture_still(&self, id: TargetId) -> Result<CapturedFrame, PlatformError> {
        self.native.capture_still(id).await
    }
    async fn open_stream(
        &self,
        id: TargetId,
        fps: u8,
    ) -> Result<Box<dyn FrameStream>, PlatformError> {
        self.native.open_stream(id, fps).await
    }
    async fn input_sink(&self, id: TargetId) -> Result<Box<dyn InputSink>, PlatformError> {
        self.native.input_sink(id).await
    }
    async fn lock_session(&self) -> Result<(), PlatformError> {
        self.lock.send_replace(DesktopLockState::Locked);
        Ok(())
    }
    async fn unlock_session(
        &self,
        password: &Secret,
        allowed: &(dyn Fn() -> bool + Send + Sync),
    ) -> Result<UnlockOutcome, PlatformError> {
        self.asked.fetch_add(1, Ordering::SeqCst);
        assert_eq!(password.expose(), PASSWORD);
        if self.slow.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
        if self.held.load(Ordering::SeqCst) {
            self.go.notified().await;
        }
        let mut answer = self
            .answers
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Ok(UnlockOutcome::Wrong));
        if matches!(answer, Ok(UnlockOutcome::Released)) {
            if allowed() {
                self.passed.fetch_add(1, Ordering::SeqCst);
                if self.release_held.load(Ordering::SeqCst) {
                    self.release_go.notified().await;
                }
                self.released.fetch_add(1, Ordering::SeqCst);
            } else {
                answer = Ok(UnlockOutcome::Withheld);
            }
        }
        if matches!(answer, Ok(UnlockOutcome::Released)) && self.confirms.load(Ordering::SeqCst) {
            // The lock screen fades out before it reports.
            let lock = self.lock.clone();
            let unsignalled = self.unsignalled.load(Ordering::SeqCst);
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(200)).await;
                lock.send_if_modified(|state| {
                    *state = DesktopLockState::Unlocked;
                    !unsignalled
                });
            });
        }
        answer
    }
    async fn desktop_lock(&self) -> DesktopLockState {
        *self.lock.borrow()
    }
    fn lock_changes(&self) -> Option<watch::Receiver<DesktopLockState>> {
        self.signals.then(|| self.lock.subscribe())
    }
}

async fn hub_with(desktop: Arc<Lockscreen>, clock: Arc<FakeClock>) -> (CompanionHub, PathBuf) {
    let path = std::env::temp_dir().join(format!("unlock-test-{}", CommandId::new()));
    (hub_at(&path, desktop, clock).await, path)
}

async fn hub_at(path: &Path, desktop: Arc<Lockscreen>, clock: Arc<FakeClock>) -> CompanionHub {
    CompanionHub::builder()
        .data_dir(path)
        .clock(clock)
        .desktop(desktop)
        .build()
        .await
        .unwrap()
}

async fn unlock(hub: &CompanionHub, password: &str) -> Result<(), ProtocolError> {
    hub.control_command(
        &Caller::local_owner(),
        &Command::UnlockDesktop {
            password: Secret::new(password),
        },
    )
    .await
    .map(|_| ())
}

async fn finish(hub: CompanionHub, path: PathBuf) {
    hub.shutdown().await.unwrap();
    drop(hub);
    std::fs::remove_dir_all(path).unwrap();
}

/// Unlock is done only once the desktop reports it unlocked, by a signal or
/// only when asked, and says so when a right password never shows.
#[tokio::test]
async fn unlock_is_done_only_once_the_desktop_reports_it() {
    for signals in [true, false] {
        let desktop = Lockscreen::new(signals);
        desktop.answer(Ok(UnlockOutcome::Released));
        let (hub, path) = hub_with(desktop.clone(), Arc::default()).await;
        unlock(&hub, PASSWORD).await.unwrap();
        assert_eq!(desktop.desktop_lock().await, DesktopLockState::Unlocked);
        assert_eq!(desktop.asked(), 1);
        finish(hub, path).await;
    }

    // An unlock whose signal never came is still found by asking, not told
    // as a right password that didn't unlock.
    let desktop = Lockscreen::new(true);
    desktop.unsignalled.store(true, Ordering::SeqCst);
    desktop.answer(Ok(UnlockOutcome::Released));
    let (hub, path) = hub_with(desktop.clone(), Arc::default()).await;
    unlock(&hub, PASSWORD).await.unwrap();
    assert_eq!(desktop.desktop_lock().await, DesktopLockState::Unlocked);
    finish(hub, path).await;

    let desktop = Lockscreen::new(true);
    desktop.confirms.store(false, Ordering::SeqCst);
    desktop.answer(Ok(UnlockOutcome::Wrong));
    desktop.answer(Ok(UnlockOutcome::Released));
    let (hub, path) = hub_with(desktop.clone(), Arc::default()).await;
    assert_eq!(
        unlock(&hub, PASSWORD).await.unwrap_err().code,
        ErrorCode::PermissionDenied
    );
    let Err(unconfirmed) = unlock(&hub, PASSWORD).await else {
        panic!("an unlock the desktop never reported")
    };
    assert_eq!(unconfirmed.code, ErrorCode::OutcomeUnknown);
    assert_eq!(
        unconfirmed.message,
        "The password was right, but the computer didn’t confirm that it unlocked. Check its screen."
    );
    // A right password is no try against the pause, but the wrong one
    // before it still is.
    for _ in 0..2 {
        assert_eq!(
            unlock(&hub, PASSWORD).await.unwrap_err().code,
            ErrorCode::PermissionDenied
        );
    }
    assert_eq!(
        unlock(&hub, PASSWORD).await.unwrap_err().code,
        ErrorCode::LimitExceeded
    );
    assert_eq!(desktop.asked(), 4);
    finish(hub, path).await;
}

/// Three passwords that didn't unlock it pause unlocking from Ophio for
/// fifteen minutes, before a fourth reaches the computer; a right one isn't
/// counted, but leaves the wrong ones before it counted.
#[tokio::test]
async fn three_tries_that_fail_pause_unlocking_from_ophio() {
    let desktop = Lockscreen::new(true);
    let clock = Arc::new(FakeClock::default());
    let (hub, path) = hub_with(desktop.clone(), clock.clone()).await;

    desktop.answer(Ok(UnlockOutcome::Wrong));
    let wrong = unlock(&hub, PASSWORD).await.unwrap_err();
    assert_eq!(wrong.code, ErrorCode::PermissionDenied);
    assert_eq!(wrong.message, "That isn’t the computer’s password");

    clock.advance(Duration::from_secs(60));
    desktop.answer(Ok(UnlockOutcome::Refused));
    let refused = unlock(&hub, PASSWORD).await.unwrap_err();
    assert_eq!(refused.code, ErrorCode::PermissionDenied);
    assert_eq!(
        refused.message,
        "The computer turned the password away; after wrong ones it can wait a while before taking more. Unlock it at the computer."
    );
    assert_eq!(
        unlock(&hub, PASSWORD).await.unwrap_err().code,
        ErrorCode::PermissionDenied
    );
    assert_eq!(desktop.asked(), 3);

    // Paused until the first try is fifteen minutes old.
    let paused = unlock(&hub, PASSWORD).await.unwrap_err();
    assert_eq!(paused.code, ErrorCode::LimitExceeded);
    assert_eq!(
        paused.message,
        "Unlocking from Ophio is paused for 14 min after three tries that didn’t unlock it. Unlock it at the computer."
    );
    clock.advance(Duration::from_secs(14 * 60 - 1));
    let paused = unlock(&hub, PASSWORD).await.unwrap_err();
    assert_eq!(paused.code, ErrorCode::LimitExceeded);
    assert!(
        paused.message.contains("paused for 1 min"),
        "{}",
        paused.message
    );
    assert_eq!(desktop.asked(), 3, "a paused try reaches the computer");

    // The first try has aged out; the other two still count.
    clock.advance(Duration::from_secs(1));
    desktop.answer(Ok(UnlockOutcome::Wrong));
    assert_eq!(
        unlock(&hub, PASSWORD).await.unwrap_err().code,
        ErrorCode::PermissionDenied
    );
    assert_eq!(
        unlock(&hub, PASSWORD).await.unwrap_err().code,
        ErrorCode::LimitExceeded
    );
    assert_eq!(desktop.asked(), 4);

    clock.advance(Duration::from_secs(15 * 60));
    assert_eq!(
        unlock(&hub, PASSWORD).await.unwrap_err().code,
        ErrorCode::PermissionDenied
    );
    desktop.answer(Ok(UnlockOutcome::Released));
    unlock(&hub, PASSWORD).await.unwrap();
    desktop.lock_session().await.unwrap();
    for _ in 0..2 {
        assert_eq!(
            unlock(&hub, PASSWORD).await.unwrap_err().code,
            ErrorCode::PermissionDenied
        );
    }
    assert_eq!(
        unlock(&hub, PASSWORD).await.unwrap_err().code,
        ErrorCode::LimitExceeded
    );
    assert_eq!(desktop.asked(), 8);
    finish(hub, path).await;
}

/// The pause outlasts a restart of Ophio, since the computer's own count
/// does.
#[tokio::test]
async fn the_pause_outlasts_a_restart() {
    let desktop = Lockscreen::new(true);
    let clock = Arc::new(FakeClock::default());
    let (hub, path) = hub_with(desktop.clone(), clock.clone()).await;
    for _ in 0..3 {
        assert_eq!(
            unlock(&hub, PASSWORD).await.unwrap_err().code,
            ErrorCode::PermissionDenied
        );
    }
    clock.advance(Duration::from_secs(60));
    hub.shutdown().await.unwrap();
    drop(hub);

    let hub = hub_at(&path, desktop.clone(), clock.clone()).await;
    let paused = unlock(&hub, PASSWORD).await.unwrap_err();
    assert_eq!(paused.code, ErrorCode::LimitExceeded);
    assert!(
        paused.message.contains("paused for 14 min"),
        "{}",
        paused.message
    );
    assert_eq!(desktop.asked(), 3);
    clock.advance(Duration::from_secs(14 * 60));
    assert_eq!(
        unlock(&hub, PASSWORD).await.unwrap_err().code,
        ErrorCode::PermissionDenied
    );
    desktop.answer(Ok(UnlockOutcome::Released));
    unlock(&hub, PASSWORD).await.unwrap();

    // The wrong try before a right one outlasts one too.
    hub.shutdown().await.unwrap();
    drop(hub);
    let hub = hub_at(&path, desktop.clone(), clock).await;
    desktop.lock_session().await.unwrap();
    for _ in 0..2 {
        assert_eq!(
            unlock(&hub, PASSWORD).await.unwrap_err().code,
            ErrorCode::PermissionDenied
        );
    }
    assert_eq!(
        unlock(&hub, PASSWORD).await.unwrap_err().code,
        ErrorCode::LimitExceeded
    );
    assert_eq!(desktop.asked(), 7);
    finish(hub, path).await;
}

/// An unlock asked for while a password is being checked waits for nothing,
/// so access its device loses meanwhile can't pass, and a caller without
/// Control and Manage reaches nothing however it came.
#[tokio::test]
async fn an_unlock_waits_for_no_other_and_needs_manage_when_it_reaches_the_computer() {
    let desktop = Lockscreen::new(true);
    desktop.slow.store(true, Ordering::SeqCst);
    let (hub, path) = hub_with(desktop.clone(), Arc::default()).await;
    let first = tokio::spawn({
        let hub = hub.clone();
        async move { unlock(&hub, PASSWORD).await }
    });
    while desktop.asked() == 0 {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let second = unlock(&hub, PASSWORD).await.unwrap_err();
    assert_eq!(second.code, ErrorCode::Conflict);
    assert_eq!(
        second.message,
        "The computer is still checking the last password"
    );
    assert_eq!(
        first.await.unwrap().unwrap_err().code,
        ErrorCode::PermissionDenied
    );
    assert_eq!(desktop.asked(), 1);

    let owner = Grants::owner();
    for grants in [
        Grants {
            admin: false,
            ..owner
        },
        Grants {
            control_input: false,
            ..owner
        },
    ] {
        let caller = Caller {
            principal: Principal::LocalOwner,
            grants,
        };
        let refused = hub
            .control_command(
                &caller,
                &Command::UnlockDesktop {
                    password: Secret::new(PASSWORD),
                },
            )
            .await
            .unwrap_err();
        assert_eq!(refused.code, ErrorCode::PermissionDenied);
    }
    // Nor did those count against the pause.
    assert_eq!(
        unlock(&hub, PASSWORD).await.unwrap_err().code,
        ErrorCode::PermissionDenied
    );
    assert_eq!(desktop.asked(), 2);
    finish(hub, path).await;
}

async fn set_grants(hub: &CompanionHub, device_id: DeviceId, from: &Grants, to: &Grants) {
    let receipt = hub
        .submit(
            &Caller::local_owner(),
            CommandEnvelope::new(Command::SetDeviceGrants {
                device_id,
                expected: Some(from.clone()),
                grants: to.clone(),
            }),
        )
        .await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted);
}

/// Control and Manage are asked again once the password is found right, so a
/// device that loses either, or is revoked, while its right password is
/// checked releases nothing, and is told as it would have been before.
#[tokio::test]
async fn a_device_that_loses_access_while_its_password_is_checked_releases_nothing() {
    let desktop = Lockscreen::new(true);
    desktop.held.store(true, Ordering::SeqCst);
    let (hub, path) = hub_with(desktop.clone(), Arc::default()).await;
    let paired = pair(&hub, 1).await;
    let Principal::Device { device_id } = paired.principal else {
        panic!("device")
    };
    let may = Grants {
        control_input: true,
        admin: true,
        ..paired.grants.clone()
    };
    set_grants(&hub, device_id, &paired.grants, &may).await;
    let device = Caller {
        principal: paired.principal,
        grants: may.clone(),
    };
    let narrow = Grants {
        admin: false,
        ..may.clone()
    };
    for round in 1..=2 {
        desktop.answer(Ok(UnlockOutcome::Released));
        let trying = tokio::spawn({
            let (hub, device) = (hub.clone(), device.clone());
            async move {
                hub.control_command(
                    &device,
                    &Command::UnlockDesktop {
                        password: Secret::new(PASSWORD),
                    },
                )
                .await
            }
        });
        while desktop.asked() < round {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let told = if round == 1 {
            set_grants(&hub, device_id, &may, &narrow).await;
            (
                ErrorCode::PermissionDenied,
                "This device or chat wasn’t given permission for that",
            )
        } else {
            let receipt = hub
                .submit(
                    &Caller::local_owner(),
                    CommandEnvelope::new(Command::RevokeDevice { device_id }),
                )
                .await;
            assert_eq!(receipt.status, ReceiptStatus::Accepted);
            (ErrorCode::Unauthenticated, "Device revoked")
        };
        desktop.go.notify_one();
        let refused = tokio::time::timeout(Duration::from_secs(5), trying)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert_eq!((refused.code, refused.message.as_str()), told);
        assert_eq!(desktop.released.load(Ordering::SeqCst), 0);
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(desktop.desktop_lock().await, DesktopLockState::Locked);
        if round == 1 {
            set_grants(&hub, device_id, &narrow, &may).await;
        }
    }
    // Both tries count, as any that didn't unlock it, so a wrong one makes
    // three.
    desktop.held.store(false, Ordering::SeqCst);
    assert_eq!(
        unlock(&hub, PASSWORD).await.unwrap_err().code,
        ErrorCode::PermissionDenied
    );
    assert_eq!(
        unlock(&hub, PASSWORD).await.unwrap_err().code,
        ErrorCode::LimitExceeded
    );
    assert_eq!(desktop.asked(), 3);
    finish(hub, path).await;
}

/// Desktop sharing that ends, or is replaced, while a right password is
/// checked waits for no check, and the desktop it shared releases nothing
/// once it has.
#[tokio::test]
async fn sharing_that_ends_while_a_password_is_checked_releases_nothing() {
    let missing = Availability::PermissionMissing {
        remedy: "Run ophio desktop enable".into(),
    };
    for replaced in [false, true] {
        let desktop = Lockscreen::new(true);
        desktop.held.store(true, Ordering::SeqCst);
        desktop.answer(Ok(UnlockOutcome::Released));
        let (hub, path) = hub_with(desktop.clone(), Arc::default()).await;
        let trying = tokio::spawn({
            let hub = hub.clone();
            async move { unlock(&hub, PASSWORD).await }
        });
        while desktop.asked() == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let next = Lockscreen::new(true);
        let (backend, availability, told) = if replaced {
            (
                Some(next.clone() as Arc<dyn DesktopBackend>),
                Availability::Available,
                (
                    ErrorCode::TemporarilyUnavailable,
                    "Desktop sharing changed on the computer while the password was checked",
                ),
            )
        } else {
            (
                None,
                missing.clone(),
                (ErrorCode::OsPermissionMissing, "Run ophio desktop enable"),
            )
        };
        tokio::time::timeout(
            Duration::from_secs(5),
            hub.set_desktop(backend, availability),
        )
        .await
        .unwrap()
        .unwrap();
        desktop.go.notify_one();
        let refused = tokio::time::timeout(Duration::from_secs(5), trying)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert_eq!((refused.code, refused.message.as_str()), told);
        assert_eq!(desktop.released.load(Ordering::SeqCst), 0);
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(desktop.desktop_lock().await, DesktopLockState::Locked);
        assert_eq!(next.asked(), 0);
        finish(hub, path).await;
    }
}

/// Sharing that ends while the computer is releasing its lock for a right
/// password ends only once it has, never part way through.
#[tokio::test]
async fn sharing_ends_only_after_a_release_under_way() {
    let desktop = Lockscreen::new(true);
    desktop.release_held.store(true, Ordering::SeqCst);
    desktop.answer(Ok(UnlockOutcome::Released));
    let (hub, path) = hub_with(desktop.clone(), Arc::default()).await;
    let trying = tokio::spawn({
        let hub = hub.clone();
        async move { unlock(&hub, PASSWORD).await }
    });
    while desktop.passed.load(Ordering::SeqCst) == 0 {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let ending = tokio::spawn({
        let hub = hub.clone();
        async move {
            hub.set_desktop(
                None,
                Availability::PermissionMissing {
                    remedy: "Run ophio desktop enable".into(),
                },
            )
            .await
        }
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!ending.is_finished());
    assert_eq!(desktop.released.load(Ordering::SeqCst), 0);
    desktop.release_go.notify_one();
    tokio::time::timeout(Duration::from_secs(5), ending)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(desktop.released.load(Ordering::SeqCst), 1);
    tokio::time::timeout(Duration::from_secs(5), trying)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    finish(hub, path).await;
}

/// A computer that can't take a password this way says so in its own fixed
/// words, and that try counts too, since the computer may have.
#[tokio::test]
async fn a_computer_that_cannot_check_says_why() {
    let desktop = Lockscreen::new(true);
    let (hub, path) = hub_with(desktop.clone(), Arc::default()).await;
    let reason = "This computer’s lock screen asks for more than a password";
    desktop.answer(Err(PlatformError::Unsupported(reason.into())));
    let unsupported = unlock(&hub, PASSWORD).await.unwrap_err();
    assert_eq!(unsupported.code, ErrorCode::Unsupported);
    assert_eq!(unsupported.message, reason);
    let reason = "The computer couldn’t check the password";
    desktop.answer(Err(PlatformError::TemporarilyUnavailable(reason.into())));
    let unavailable = unlock(&hub, PASSWORD).await.unwrap_err();
    assert_eq!(unavailable.code, ErrorCode::TemporarilyUnavailable);
    assert_eq!(unavailable.message, reason);
    assert_eq!(
        unlock(&hub, PASSWORD).await.unwrap_err().code,
        ErrorCode::PermissionDenied
    );
    assert_eq!(
        unlock(&hub, PASSWORD).await.unwrap_err().code,
        ErrorCode::LimitExceeded
    );
    assert_eq!(desktop.asked(), 3);
    finish(hub, path).await;
}

/// Nothing reaches the computer without a password, with one longer than any
/// lock screen takes, or while it is already unlocked.
#[tokio::test]
async fn unlock_needs_a_password_and_a_locked_computer() {
    let desktop = Lockscreen::new(true);
    let (hub, path) = hub_with(desktop.clone(), Arc::default()).await;
    let empty = unlock(&hub, "").await.unwrap_err();
    assert_eq!(empty.code, ErrorCode::InvalidRequest);
    assert_eq!(empty.message, "Type the computer’s password");
    let long = unlock(&hub, &"x".repeat(513)).await.unwrap_err();
    assert_eq!(long.code, ErrorCode::InvalidRequest);
    desktop.lock.send_replace(DesktopLockState::Unlocked);
    let unlocked = unlock(&hub, PASSWORD).await.unwrap_err();
    assert_eq!(unlocked.code, ErrorCode::Conflict);
    assert_eq!(unlocked.message, "The computer is already unlocked");
    assert_eq!(desktop.asked(), 0);
    // None of those counted as a try.
    desktop.lock.send_replace(DesktopLockState::Locked);
    for _ in 0..2 {
        assert_eq!(
            unlock(&hub, PASSWORD).await.unwrap_err().code,
            ErrorCode::PermissionDenied
        );
    }
    finish(hub, path).await;
}

/// A phone that unlocks the computer sends its password once: the command
/// Ophio keeps has it withheld, and it is in no file Ophio writes.
#[tokio::test]
async fn an_unlock_password_reaches_the_computer_and_nowhere_else() {
    let path = std::env::temp_dir().join(format!("unlock-test-{}", CommandId::new()));
    let desktop = Arc::new(FakeDesktop::default());
    let hub = CompanionHub::builder()
        .data_dir(&path)
        .clock(Arc::new(FakeClock::default()))
        .desktop(desktop.clone())
        .build()
        .await
        .unwrap();
    let owner = Caller::local_owner();
    let settled = |envelope: CommandEnvelope| {
        let hub = hub.clone();
        let owner = owner.clone();
        async move {
            let id = envelope.command_id;
            hub.submit(&owner, envelope).await;
            tokio::time::timeout(Duration::from_secs(8), async {
                loop {
                    if let Some(c) = hub.read(|r| r.command(id)).unwrap()
                        && !c.pending
                    {
                        return c;
                    }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .unwrap()
        }
    };
    let locked = settled(CommandEnvelope::new(Command::LockDesktop)).await;
    assert!(locked.error.is_none(), "{:?}", locked.error);
    let wrong = settled(CommandEnvelope::new(Command::UnlockDesktop {
        password: Secret::new(PASSWORD),
    }))
    .await;
    assert_eq!(
        wrong.error.as_ref().unwrap().code,
        ErrorCode::PermissionDenied
    );
    let unlocked = settled(CommandEnvelope::new(Command::UnlockDesktop {
        password: Secret::new(FakeDesktop::PASSWORD),
    }))
    .await;
    assert!(unlocked.error.is_none(), "{:?}", unlocked.error);
    assert_eq!(desktop.unlocks.load(Ordering::SeqCst), 1);
    for stored in [&wrong, &unlocked] {
        let Command::UnlockDesktop { password } = &stored.envelope.body else {
            panic!("an unlock kept as {:?}", stored.envelope.body)
        };
        assert!(password.expose().is_empty());
    }
    hub.shutdown().await.unwrap();
    drop(hub);
    for password in [PASSWORD, FakeDesktop::PASSWORD] {
        assert!(!written_anywhere(&path, password), "{password} was written");
    }
    std::fs::remove_dir_all(path).unwrap();
}

/// A computer locked before Ophio starts, whose screens take no input while
/// it is, still shows them locked, so the app offers to unlock it; once it is
/// unlocked and input is back, they are ready to control.
#[tokio::test]
async fn a_computer_locked_before_ophio_starts_shows_its_screens_locked() {
    let desktop = Lockscreen::new(true);
    let target_id = {
        let mut target = desktop.native.target.lock().unwrap();
        target.input = Availability::TemporarilyUnavailable {
            reason: "Computer is locked".into(),
        };
        target.target_id
    };
    let (hub, path) = hub_with(desktop.clone(), Arc::default()).await;
    // As the computer starts watching a desktop it shares.
    hub.watch_desktop();
    let view = |hub: &CompanionHub| {
        hub.read(|r| r.get::<TargetControlView>("control", target_id))
            .unwrap()
    };
    let mut waited = 0;
    while view(&hub).and_then(|v| v.desktop_lock) != Some(DesktopLockState::Locked) {
        assert!(waited < 100, "the screen was never shown locked");
        tokio::time::sleep(Duration::from_millis(50)).await;
        waited += 1;
    }
    let locked = view(&hub).unwrap();
    assert_eq!(locked.input_state, InputState::Unavailable);
    assert!(locked.lease.is_none());

    desktop.native.target.lock().unwrap().input = Availability::Available;
    desktop.answer(Ok(UnlockOutcome::Released));
    unlock(&hub, PASSWORD).await.unwrap();
    let mut waited = 0;
    while view(&hub).map(|v| (v.desktop_lock, v.input_state))
        != Some((Some(DesktopLockState::Unlocked), InputState::Ready))
    {
        assert!(waited < 100, "the unlocked screen never became ready");
        tokio::time::sleep(Duration::from_millis(50)).await;
        waited += 1;
    }
    finish(hub, path).await;
}
