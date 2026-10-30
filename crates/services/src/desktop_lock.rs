// SPDX-License-Identifier: Apache-2.0
//! Session observation and catalog publication are independent of lease expiry.
use crate::{
    CompanionHub,
    api::Caller,
    control::{Target, keep_worse, platform},
    error,
};
use ports::UnlockOutcome;
use protocol::{
    ErrorCode, ProtocolError, accounts::Secret, auth::Grants, control::*, time::Timestamp,
};
use std::{
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

/// How long Lock waits for the desktop to report that it locked. Screen
/// lockers report within a second; longer would keep the phone waiting.
const LOCK_CONFIRMATION: Duration = Duration::from_secs(3);
/// How long Unlock waits for the desktop to report that it unlocked, a
/// little longer since the lock screen fades out first.
const UNLOCK_CONFIRMATION: Duration = Duration::from_secs(5);
/// Tries from Ophio that didn't unlock it before it stops passing more on
/// for a while, so guessing from a device gets nowhere. They never count
/// toward the computer's own lockout.
const UNLOCK_TRIES: usize = 3;
/// How long a try that didn't unlock it counts.
const UNLOCK_WINDOW: Duration = Duration::from_secs(15 * 60);
/// Longer than any lock screen takes.
const UNLOCK_PASSWORD_MAX: usize = 512;
/// Where the times of recent tries are kept, never what was typed.
const UNLOCK_TRIES_KEY: &str = "unlock_tries";

/// Unlocking takes both Control and Manage, since the password opens the
/// whole computer.
fn may_unlock(grants: &Grants) -> bool {
    grants.control_input && grants.admin
}

impl CompanionHub {
    pub(crate) fn watch_desktop(&self) {
        let Some(backend) = self.desktop_option() else {
            return;
        };
        let weak = Arc::downgrade(&self.inner);
        let mut replaced = self.inner.desktop_changed.subscribe();
        let mut shutdown = self.inner.shutdown.subscribe();
        let mut changes = backend.lock_changes();
        self.spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_millis(250));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    biased;
                    _ = shutdown.changed() => break,
                    _ = replaced.changed() => break,
                    result = async { match &mut changes { Some(changes) => changes.changed().await, None => std::future::pending().await } } => {
                        // A closed signal source must not become a busy loop.
                        // Resume polling so a replacement observation can recover.
                        if result.is_err() { changes = None; }
                    },
                    // Catalog changes (rescale, source loss, hotplug) are
                    // independent of lock signals. Keep their refresh alive.
                    _ = tick.tick() => {},
                }
                let Some(inner) = weak.upgrade() else { break; };
                let hub = Self { inner };
                let _operation = hub.inner.desktop_operation.read().await;
                if replaced.has_changed().unwrap_or(true) { break; }
                let lock = match &mut changes {
                    Some(changes) => *changes.borrow_and_update(),
                    None => backend.desktop_lock().await,
                };
                if let Err(error) = hub.observe_desktop_lock(&backend, lock).await {
                    tracing::warn!(code=?error.code, "Desktop observation unavailable");
                }
            }
        });
    }

    pub(crate) async fn observe_desktop_lock(
        &self,
        backend: &Arc<dyn ports::DesktopBackend>,
        lock: DesktopLockState,
    ) -> Result<(), ProtocolError> {
        let mut failure = None;
        // Unreadable, the projection is refreshed as if it had changed.
        let observed_change = self
            .read(|tx| {
                Ok(tx
                    .list::<TargetControlView>("control")?
                    .iter()
                    .any(|view| view.desktop_lock != Some(lock)))
            })
            .unwrap_or_else(|e| {
                keep_worse(&mut failure, Err(e));
                true
            });
        // Every screen is fenced, whatever became of the one before it.
        let controls: Vec<_> = self.inner.controls.lock().await.values().cloned().collect();
        let mut repaired = false;
        for target in controls {
            let mut target = target.lock().await;
            repaired |= target.unsaved;
            keep_worse(
                &mut failure,
                self.apply_desktop_lock(&mut target, lock).await,
            );
        }
        let published = async {
            self.publish_targets(backend.targets().await.map_err(platform)?)?;
            // Ready projections need lock state even before a native sink is opened.
            let changed = self.write(|tx| {
                let mut changed = false;
                for mut view in tx.read().list::<TargetControlView>("control")? {
                    if view.desktop_lock != Some(lock) {
                        view.desktop_lock = Some(lock);
                        changed = true;
                        tx.put("control", view.target_id, &view)?;
                        tx.emit(protocol::Event::ControlChanged { view })?;
                    }
                }
                Ok(changed)
            })?;
            if changed || observed_change || repaired {
                self.refresh_host().await?;
                self.refresh_readiness().await?;
            }
            Ok(())
        }
        .await;
        keep_worse(&mut failure, published);
        failure.map_or(Ok(()), Err)
    }

    pub(crate) async fn apply_desktop_lock(
        &self,
        target: &mut Target,
        lock: DesktopLockState,
    ) -> Result<(), ProtocolError> {
        let mut changed = target.view.desktop_lock != Some(lock);
        target.view.desktop_lock = Some(lock);
        // Legacy adapters with no lock observer keep their existing policy.
        // Linux's native sink additionally refuses unknown lock observations.
        let unknown_observer =
            lock == DesktopLockState::Unknown && self.desktop()?.lock_changes().is_some();
        if lock == DesktopLockState::Locked || unknown_observer {
            if let Some(observation) = &mut target.view.model_observation {
                changed |= observation.state != ObservationState::Withdrawn;
                observation.state = ObservationState::Withdrawn;
            }
            if target.deadline.is_some() {
                let released = self.revoke_locked(target, false).await;
                let recorded = self.intervention(
                    target.view.target_id,
                    protocol::model::InterventionKind::PauseInput,
                    if unknown_observer {
                        "Computer lock state is unknown"
                    } else {
                        "Computer is locked"
                    },
                    released.is_ok(),
                );
                released?;
                recorded?;
                return Ok(());
            }
        }
        // A projection an earlier save could not keep is saved whole, even
        // when nothing has changed since.
        if changed || target.unsaved {
            self.save_target(target)?;
        }
        Ok(())
    }

    /// Lock is done only once the desktop reports it locked. `changes` is
    /// subscribed before the lock was requested, so its report is not missed.
    pub(crate) async fn confirm_locked(
        &self,
        backend: &Arc<dyn ports::DesktopBackend>,
        changes: Option<tokio::sync::watch::Receiver<DesktopLockState>>,
    ) -> Result<(), ProtocolError> {
        if reports(
            backend,
            changes,
            DesktopLockState::Locked,
            LOCK_CONFIRMATION,
        )
        .await
        {
            return Ok(());
        }
        Err(error(
            ErrorCode::OutcomeUnknown,
            "The computer was asked to lock but didn’t confirm that it locked. Check its screen, or lock it there.",
        ))
    }

    /// Unlock with the computer's password for its account, which the
    /// computer checks. After `UNLOCK_TRIES` that didn't unlock it Ophio passes
    /// no more on for a while. Done only once the desktop reports it unlocked.
    pub(crate) async fn unlock_desktop(
        &self,
        caller: &Caller,
        password: &Secret,
    ) -> Result<(), ProtocolError> {
        if password.expose().is_empty() {
            return Err(error(
                ErrorCode::InvalidRequest,
                "Type the computer’s password",
            ));
        }
        if password.expose().len() > UNLOCK_PASSWORD_MAX {
            return Err(error(
                ErrorCode::InvalidRequest,
                "That is longer than any computer’s password",
            ));
        }
        // Taken together, so the sharing it is checked against later is the
        // one this backend was shared by.
        let (desktop, sharing) = {
            let _operation = self.inner.desktop_operation.read().await;
            (self.desktop()?, *self.inner.desktop_changed.borrow())
        };
        // Held for the whole attempt, so two at once can't both pass the
        // count; a second waits for nobody, since its device may lose access
        // while it would.
        let Ok(mut held) = self.inner.unlock_tries.try_lock() else {
            return Err(error(
                ErrorCode::Conflict,
                "The computer is still checking the last password",
            ));
        };
        let tries = match &mut *held {
            Some(tries) => tries,
            loaded => loaded.insert(self.load_unlock_tries()?),
        };
        let now = self.inner.clock.monotonic();
        tries.retain(|at| now.saturating_duration_since(*at) < UNLOCK_WINDOW);
        if let Some(first) = tries.first().filter(|_| tries.len() >= UNLOCK_TRIES) {
            let left = UNLOCK_WINDOW.saturating_sub(now.saturating_duration_since(*first));
            let minutes = left.as_secs().div_ceil(60).max(1);
            return Err(error(
                ErrorCode::LimitExceeded,
                format!(
                    "Unlocking from Ophio is paused for {minutes} min after three tries that didn’t unlock it. Unlock it at the computer."
                ),
            ));
        }
        if desktop.desktop_lock().await == DesktopLockState::Unlocked {
            return Err(error(
                ErrorCode::Conflict,
                "The computer is already unlocked",
            ));
        }
        self.authorize(caller, may_unlock)?;
        // Counted before the computer sees it, so a restart part way through
        // still remembers it.
        tries.push(now);
        self.save_unlock_tries(tries)?;
        let changes = desktop.lock_changes();
        // Asked again once the password is found right, so a device that
        // lost Control or Manage while it was checked releases nothing, nor
        // does one whose computer stopped or changed sharing its desktop
        // meanwhile. What it holds keeps sharing on until the lock is released.
        let releasing = OnceLock::new();
        let withdrawn = AtomicBool::new(false);
        let allowed = || {
            let shared = (self.inner.unlock_release.try_read().ok())
                .filter(|_| *self.inner.desktop_changed.borrow() == sharing);
            let Some(shared) = shared else {
                withdrawn.store(true, Ordering::SeqCst);
                return false;
            };
            if self.authorize(caller, may_unlock).is_err() {
                return false;
            }
            let _ = releasing.set(shared);
            true
        };
        let outcome = desktop.unlock_session(password, &allowed).await;
        drop(releasing);
        if let Ok(UnlockOutcome::Released) = outcome {
            // The right password isn't counted, but the wrong ones before it
            // still are, so a right one never makes room for more guesses.
            tries.pop();
            self.keep_unlock_tries(tries);
            drop(held);
            if reports(
                &desktop,
                changes,
                DesktopLockState::Unlocked,
                UNLOCK_CONFIRMATION,
            )
            .await
            {
                return Ok(());
            }
            return Err(error(
                ErrorCode::OutcomeUnknown,
                "The password was right, but the computer didn’t confirm that it unlocked. Check its screen.",
            ));
        }
        // Whatever kept it locked, the try counts from when the computer
        // answered, so a slow answer doesn't shorten the pause.
        if let Some(last) = tries.last_mut() {
            *last = self.inner.clock.monotonic();
        }
        self.keep_unlock_tries(tries);
        Err(match outcome {
            Ok(UnlockOutcome::Wrong) => error(
                ErrorCode::PermissionDenied,
                "That isn’t the computer’s password",
            ),
            // Told as it would have been had sharing ended first.
            Ok(UnlockOutcome::Withheld) if withdrawn.load(Ordering::SeqCst) => {
                self.desktop().err().unwrap_or_else(|| {
                    error(
                        ErrorCode::TemporarilyUnavailable,
                        "Desktop sharing changed on the computer while the password was checked",
                    )
                })
            }
            // Told as it would have been had it lost access first.
            Ok(UnlockOutcome::Withheld) => {
                self.authorize(caller, may_unlock).err().unwrap_or_else(|| {
                    error(
                        ErrorCode::PermissionDenied,
                        "This device or chat wasn’t given permission for that",
                    )
                })
            }
            Ok(UnlockOutcome::Refused | UnlockOutcome::Released) => error(
                ErrorCode::PermissionDenied,
                "The computer turned the password away; after wrong ones it can wait a while before taking more. Unlock it at the computer.",
            ),
            // The backends' own words here are fixed, never the system's.
            Err(ports::PlatformError::Unsupported(reason)) if !reason.is_empty() => {
                error(ErrorCode::Unsupported, reason)
            }
            Err(ports::PlatformError::TemporarilyUnavailable(reason)) if !reason.is_empty() => {
                error(ErrorCode::TemporarilyUnavailable, reason)
            }
            Err(e) => platform(e),
        })
    }

    /// The tries saved by an earlier run, placed on this run's clock.
    fn load_unlock_tries(&self) -> Result<Vec<Instant>, ProtocolError> {
        let saved: Vec<Timestamp> =
            self.read(|r| Ok(r.get("meta", UNLOCK_TRIES_KEY)?.unwrap_or_default()))?;
        let (now, wall) = (self.inner.clock.monotonic(), self.inner.clock.now());
        Ok(saved
            .into_iter()
            .filter_map(|at| {
                // One from a clock set back since counts as just made.
                let ago = u64::try_from(wall.millis().saturating_sub(at.millis())).unwrap_or(0);
                // One before this clock began was before the computer last
                // started, and its own count with it.
                now.checked_sub(Duration::from_millis(ago))
            })
            .collect())
    }

    /// Saves the tries without anything typed, so a restart keeps the pause.
    fn save_unlock_tries(&self, tries: &[Instant]) -> Result<(), ProtocolError> {
        let (now, wall) = (self.inner.clock.monotonic(), self.inner.clock.now());
        let saved: Vec<Timestamp> = tries
            .iter()
            .map(|at| {
                let ago = i64::try_from(now.saturating_duration_since(*at).as_millis())
                    .unwrap_or(i64::MAX);
                Timestamp(wall.millis().saturating_sub(ago))
            })
            .collect();
        self.write(|tx| tx.put("meta", UNLOCK_TRIES_KEY, &saved))
    }

    /// Saves the tries once the computer has answered, whatever it answered;
    /// a failure leaves the earlier, fuller count in place.
    fn keep_unlock_tries(&self, tries: &[Instant]) {
        if let Err(e) = self.save_unlock_tries(tries) {
            tracing::warn!(code = ?e.code, "Couldn't save the unlock tries");
        }
    }

    pub(crate) async fn check_desktop_lock(
        &self,
        target: &mut Target,
    ) -> Result<(), ProtocolError> {
        let lock = self.desktop()?.desktop_lock().await;
        self.apply_desktop_lock(target, lock).await?;
        if lock == DesktopLockState::Locked {
            return Err(error(
                ErrorCode::TemporarilyUnavailable,
                "Computer is locked",
            ));
        }
        Ok(())
    }
}

/// Whether the desktop reports `wanted` within `within`. `changes` is
/// subscribed before the change was asked for, so its report is not missed.
async fn reports(
    backend: &Arc<dyn ports::DesktopBackend>,
    changes: Option<tokio::sync::watch::Receiver<DesktopLockState>>,
    wanted: DesktopLockState,
    within: Duration,
) -> bool {
    let reported = async {
        let mut changes = changes;
        loop {
            // A signal answers at once. Asking again now and then still finds
            // a change whose signal never came, so a lock screen that went
            // away unannounced isn't told as one that stayed.
            if let Some(watch) = &mut changes {
                let signalled = tokio::time::timeout(
                    Duration::from_millis(500),
                    watch.wait_for(|lock| *lock == wanted),
                )
                .await
                .map(|seen| seen.is_ok());
                match signalled {
                    Ok(true) => break true,
                    Ok(false) => changes = None,
                    Err(_) => {}
                }
            }
            if backend.desktop_lock().await == wanted {
                break true;
            }
            if changes.is_none() {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    };
    tokio::time::timeout(within, reported)
        .await
        .unwrap_or(false)
}

impl CompanionHub {
    pub(crate) async fn reopen_desktop_capture(&self) -> Result<(), ProtocolError> {
        let _operation = self.inner.desktop_operation.write().await;
        let desktop = self.desktop()?;
        let mut controls = self.inner.controls.lock().await;
        let mut failure = None;
        for target in controls.values() {
            let mut target = target.lock().await;
            if let Some(observation) = &mut target.view.model_observation {
                observation.state = ObservationState::Withdrawn;
            }
            let released = self.revoke_locked(&mut target, false).await;
            target.removed = released.is_ok();
            crate::control::keep_worse(&mut failure, released);
        }
        if let Some(failure) = failure {
            return Err(failure);
        }
        controls.clear();
        drop(controls);
        desktop.reopen_capture().await.map_err(platform)?;
        self.refresh_host().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{FakeClock, FakeDesktop};
    use ports::{
        CapturedFrame, DesktopBackend, DesktopCapabilities, FrameStream, InputSink, PlatformError,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Debug)]
    struct ObservedDesktop {
        native: FakeDesktop,
        changes: Option<tokio::sync::watch::Sender<DesktopLockState>>,
        reads: AtomicUsize,
        unavailable: std::sync::atomic::AtomicBool,
    }
    #[async_trait::async_trait]
    impl DesktopBackend for ObservedDesktop {
        async fn capabilities(&self) -> DesktopCapabilities {
            self.native.capabilities().await
        }
        async fn targets(&self) -> Result<Vec<ControlTarget>, PlatformError> {
            let mut targets = self.native.targets().await?;
            if self.unavailable.load(Ordering::SeqCst) {
                for target in &mut targets {
                    target.capture = protocol::capability::Availability::TemporarilyUnavailable {
                        reason: "Display changed, reopen to continue".into(),
                    };
                }
            }
            Ok(targets)
        }
        async fn capture_still(
            &self,
            id: protocol::TargetId,
        ) -> Result<CapturedFrame, PlatformError> {
            self.native.capture_still(id).await
        }
        async fn open_stream(
            &self,
            id: protocol::TargetId,
            fps: u8,
        ) -> Result<Box<dyn FrameStream>, PlatformError> {
            self.native.open_stream(id, fps).await
        }
        async fn input_sink(
            &self,
            id: protocol::TargetId,
        ) -> Result<Box<dyn InputSink>, PlatformError> {
            self.native.input_sink(id).await
        }
        async fn lock_session(&self) -> Result<(), PlatformError> {
            self.native.lock_session().await
        }
        async fn desktop_lock(&self) -> DesktopLockState {
            self.reads.fetch_add(1, Ordering::SeqCst);
            DesktopLockState::Unlocked
        }
        fn lock_changes(&self) -> Option<tokio::sync::watch::Receiver<DesktopLockState>> {
            self.changes.as_ref().map(|s| s.subscribe())
        }
    }
    #[tokio::test]
    async fn signals_replace_service_polling_and_preserve_locked_evidence() {
        check_watcher(true).await;
    }
    #[tokio::test]
    async fn backends_without_signals_keep_a_fallback_poll() {
        check_watcher(false).await;
    }
    async fn check_watcher(signals: bool) {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/lock-tests")
            .join(uuid::Uuid::now_v7().to_string());
        let backend = Arc::new(ObservedDesktop {
            native: FakeDesktop::default(),
            changes: signals.then(|| tokio::sync::watch::channel(DesktopLockState::Unlocked).0),
            reads: AtomicUsize::new(0),
            unavailable: Default::default(),
        });
        let hub = CompanionHub::builder()
            .data_dir(&path)
            .clock(Arc::new(FakeClock::default()))
            .desktop(backend.clone())
            .build()
            .await
            .unwrap();
        hub.watch_desktop();
        for _ in 0..50 {
            tokio::task::yield_now().await;
        }
        let before = backend.reads.load(Ordering::SeqCst);
        backend.unavailable.store(true, Ordering::SeqCst);
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(2)).await;
        for _ in 0..50 {
            tokio::task::yield_now().await;
        }
        if let Some(changes) = &backend.changes {
            assert_eq!(backend.reads.load(Ordering::SeqCst), before);
            let targets: Vec<ControlTarget> = hub
                .read(|tx| Ok(tx.get("meta", "targets")?.unwrap_or_default()))
                .unwrap();
            assert!(!targets.is_empty());
            assert!(targets.iter().all(|target| !target.capture.is_available()));
            changes.send_replace(DesktopLockState::Locked);
            for _ in 0..50 {
                tokio::task::yield_now().await;
            }
            let views = hub
                .read(|tx| tx.list::<TargetControlView>("control"))
                .unwrap();
            assert!(!views.is_empty());
            assert!(
                views
                    .iter()
                    .all(|v| v.desktop_lock == Some(DesktopLockState::Locked))
            );
            assert_eq!(backend.reads.load(Ordering::SeqCst), before);
        } else {
            assert!(backend.reads.load(Ordering::SeqCst) > before);
        }
        tokio::time::resume();
        hub.shutdown().await.unwrap();
        drop(hub);
        std::fs::remove_dir_all(path).unwrap();
    }

    /// Lock is done only once the desktop says it locked, whether it reports
    /// that by a signal or only when asked, and says so when it never does.
    #[tokio::test]
    async fn lock_is_done_only_once_the_desktop_reports_it() {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/lock-tests")
            .join(uuid::Uuid::now_v7().to_string());
        let lock = |hub: &CompanionHub| {
            let hub = hub.clone();
            async move {
                hub.control_command(
                    &crate::api::Caller::local_owner(),
                    &protocol::Command::LockDesktop,
                )
                .await
            }
        };
        // A signal that reports the lock a moment after it is requested.
        let signalled = Arc::new(ObservedDesktop {
            native: FakeDesktop::default(),
            changes: Some(tokio::sync::watch::channel(DesktopLockState::Unlocked).0),
            reads: AtomicUsize::new(0),
            unavailable: Default::default(),
        });
        let hub = CompanionHub::builder()
            .data_dir(path.join("signalled"))
            .clock(Arc::new(FakeClock::default()))
            .desktop(signalled.clone())
            .build()
            .await
            .unwrap();
        let reporter = tokio::spawn({
            let signalled = signalled.clone();
            async move {
                while signalled.native.locks.load(Ordering::SeqCst) == 0 {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
                if let Some(changes) = &signalled.changes {
                    changes.send_replace(DesktopLockState::Locked);
                }
            }
        });
        lock(&hub).await.unwrap();
        reporter.await.unwrap();
        hub.shutdown().await.unwrap();
        drop(hub);

        // A backend that reports its state only when asked.
        let hub = CompanionHub::builder()
            .data_dir(path.join("polled"))
            .clock(Arc::new(FakeClock::default()))
            .desktop(Arc::new(FakeDesktop::default()))
            .build()
            .await
            .unwrap();
        lock(&hub).await.unwrap();
        hub.shutdown().await.unwrap();
        drop(hub);

        // A desktop that stays unlocked.
        let unlocked = Arc::new(ObservedDesktop {
            native: FakeDesktop::default(),
            changes: None,
            reads: AtomicUsize::new(0),
            unavailable: Default::default(),
        });
        let hub = CompanionHub::builder()
            .data_dir(path.join("unlocked"))
            .clock(Arc::new(FakeClock::default()))
            .desktop(unlocked.clone())
            .build()
            .await
            .unwrap();
        let Err(unconfirmed) = lock(&hub).await else {
            panic!("a lock the desktop never reported")
        };
        assert_eq!(unconfirmed.code, ErrorCode::OutcomeUnknown);
        assert_eq!(
            unconfirmed.message,
            "The computer was asked to lock but didn’t confirm that it locked. Check its screen, or lock it there."
        );
        assert_eq!(unlocked.native.locks.load(Ordering::SeqCst), 1);
        hub.shutdown().await.unwrap();
        drop(hub);
        std::fs::remove_dir_all(path).unwrap();
    }
}
