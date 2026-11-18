// SPDX-License-Identifier: Apache-2.0
//! A display marked with an app launch that couldn't be confirmed stopped
//! is checked again in the background, and cleared once the computer proves
//! nothing of the launch is left: systemd says its scope is inactive or
//! gone. A launch nothing can check again (one without a scope, or whose
//! task panicked) keeps the display marked until Ophio restarts.
//!
//! The checks hold no display's lock; only clearing takes it, briefly, once
//! the proof is in. The release that said an app may still open stays as
//! it was said.
use crate::{
    CompanionHub,
    control::{Launches, Target, Unconfirmed},
};
use protocol::{
    Event,
    control::InputState,
    ids::TargetId,
    model::{FactStatus, InterventionKind, InterventionReceipt, ReceiptFact},
};
use std::{
    sync::{Arc, atomic::Ordering::SeqCst},
    time::Duration,
};

/// When a mark is checked again, after it was set: at 2, 5, 10, 20 and 40
/// seconds, then every minute. Tests run the same schedule 20 times faster.
fn after(attempt: u32) -> Duration {
    const FIRST: [u64; 5] = [2, 5, 10, 20, 40];
    let seconds = FIRST
        .get(attempt as usize)
        .copied()
        .unwrap_or_else(|| 40 + 60 * u64::from(attempt - 4));
    Duration::from_millis(seconds * if cfg!(test) { 50 } else { 1000 })
}
/// The checks within 10 seconds of the mark stop again what is left of the
/// launch, as its own cleanup tried to; later ones only look, so an app the
/// person may have taken up meanwhile is never closed under them.
const STOPPING: u32 = 3;
/// How long one launch's check may take: two calls to systemd, with room.
const CHECK_LIMIT: Duration = Duration::from_millis(2500);

/// An authority that keeps what its launcher tells of a launch it couldn't
/// stop, for the display's later checks.
pub(crate) struct Recording {
    inner: Arc<dyn voice_control::Authority>,
    launch: std::sync::Mutex<Unconfirmed>,
}
impl Recording {
    /// Keeps authority alive through delayed native launch dispatch.
    pub(crate) fn owned(inner: Arc<dyn voice_control::Authority>) -> Self {
        Self {
            inner,
            launch: Default::default(),
        }
    }
    /// How the launch can be checked again, if its launcher said.
    pub(crate) fn take(&self) -> Unconfirmed {
        self.launch.lock().ok().and_then(|mut launch| launch.take())
    }
}
impl voice_control::Authority for Recording {
    fn check(&self) -> Result<(), voice_control::Error> {
        self.inner.check()
    }
    fn unstopped(&self, launch: Arc<dyn ports::LaunchCheck>) {
        if let Ok(mut kept) = self.launch.lock() {
            *kept = Some(launch);
        }
    }
}

impl CompanionHub {
    /// Marks `target` with a launch that couldn't be confirmed stopped, and
    /// starts checking it again, unless that is under way already.
    pub(crate) fn mark_launch(&self, target: &mut Target) {
        target.unresolved_launch = true;
        let launches = target.launches.clone();
        if launches.rechecking.swap(true, SeqCst) {
            return;
        }
        let id = target.view.target_id;
        let weak = Arc::downgrade(&self.inner);
        let mut shutdown = self.inner.shutdown.subscribe();
        self.spawn(async move {
            let began = tokio::time::Instant::now();
            let mut attempt = 0;
            loop {
                tokio::select! {
                    _ = shutdown.changed() => return,
                    _ = tokio::time::sleep_until(began + after(attempt)) => {}
                }
                let Some(inner) = weak.upgrade() else { return };
                if !(Self { inner }).recheck(id, &launches, attempt).await {
                    return;
                }
                attempt = attempt.saturating_add(1);
            }
        });
    }
    /// Checks the launches that marked display `id` once more: false once
    /// there is no more to check, cleared or for good.
    async fn recheck(&self, id: TargetId, launches: &Arc<Launches>, attempt: u32) -> bool {
        // A launch still running ends first, and keeps its own outcome.
        if launches.pending() != 0 {
            return true;
        }
        let Ok(unconfirmed) = launches.unconfirmed.lock().map(|u| u.clone()) else {
            return false;
        };
        // One nothing can check again keeps the display marked until restart.
        if unconfirmed.iter().any(Option::is_none) {
            return false;
        }
        let stop = attempt < STOPPING;
        for launch in unconfirmed.iter().flatten() {
            let stopped = tokio::time::timeout(CHECK_LIMIT, launch.stopped(stop)).await;
            if !matches!(stopped, Ok(true)) {
                return true;
            }
        }
        // Proven. Only now is the display's lock taken.
        let Some(target) = self.inner.controls.lock().await.get(&id).cloned() else {
            return false;
        };
        let mut target = target.lock().await;
        // A display reopened since is a new one, with launches of its own.
        if !Arc::ptr_eq(&target.launches, launches) {
            return false;
        }
        {
            let Ok(mut now) = launches.unconfirmed.lock() else {
                return false;
            };
            // Another launch ended meanwhile: checked again, with it.
            if launches.pending() != 0 || now.len() != unconfirmed.len() {
                return true;
            }
            now.clear();
            // Every launch counted before now is proven stopped: a voice
            // driver still holding one as unstopped stops saying so.
            launches.cleared.fetch_add(1, SeqCst);
        }
        // Cleared under the display's lock, as every mark is set, so a new
        // mark either comes after this and is checked again, or was counted.
        target.unresolved_launch = false;
        launches.rechecking.store(false, SeqCst);
        self.launch_confirmed(&mut target).await;
        false
    }
    /// Lets go of `target` once more, now that it can be, so what the phone
    /// shows says whether its input was let go, and records why.
    async fn launch_confirmed(&self, target: &mut Target) {
        let id = target.view.target_id;
        tracing::info!(target_id = %id, "an app launch that couldn't be stopped is confirmed stopped");
        let shared = !(target.removed || target.leaving);
        let released = if !shared {
            Ok(())
        } else if target.view.input_state == InputState::Unknown {
            self.revoke_locked(target, false).await
        } else {
            self.save_target(target)
        };
        let input = released.is_ok();
        let recorded = self.write(|tx| {
            tx.emit(Event::InterventionRecorded {
                task_id: None,
                target_id: Some(id),
                receipt: Box::new(InterventionReceipt {
                    kind: InterventionKind::PauseInput,
                    requested_at: self.inner.clock.now(),
                    facts: vec![
                        ReceiptFact {
                            label: "App launch".into(),
                            status: FactStatus::Done,
                            // Only the launch's own scope is proven empty: an
                            // app the desktop started elsewhere (through D-Bus,
                            // or Flatpak) isn't in it.
                            detail: Some("The computer confirmed the launch itself has stopped".into()),
                        },
                        ReceiptFact {
                            label: "Input release".into(),
                            status: if input {
                                FactStatus::Done
                            } else {
                                FactStatus::Unknown
                            },
                            detail: None,
                        },
                    ],
                    summary: if !shared {
                        "The computer confirmed the app’s launch itself stopped. This display is no longer shared"
                    } else if input {
                        "The computer confirmed the app’s launch itself stopped. Control can be given on this display again"
                    } else {
                        "The computer confirmed the app’s launch itself stopped, but hasn’t confirmed it let go of this display’s input"
                    }
                    .into(),
                }),
            })?;
            Ok(())
        });
        if let Err(e) = recorded {
            tracing::warn!(code = ?e.code, "a confirmed launch stop went unrecorded");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;
    use voice_control::{Authority, Error};

    struct LiveAuthority(Arc<AtomicBool>);
    impl Authority for LiveAuthority {
        fn check(&self) -> Result<(), Error> {
            if self.0.load(SeqCst) {
                Ok(())
            } else {
                Err(Error::Stopped)
            }
        }
    }
    #[derive(Debug)]
    struct UnknownLaunch;
    #[async_trait::async_trait]
    impl ports::LaunchCheck for UnknownLaunch {
        async fn stopped(&self, _: bool) -> bool {
            false
        }
    }

    #[test]
    fn owned_recording_keeps_live_authority_and_uncertain_launch_tracking() {
        let allowed = Arc::new(AtomicBool::new(true));
        let inner = Arc::new(LiveAuthority(allowed.clone()));
        let weak = Arc::downgrade(&inner);
        let recording = Arc::new(Recording::owned(inner));
        assert!(
            weak.upgrade().is_some(),
            "the worker must retain the actual authority"
        );
        assert!(recording.check().is_ok());
        allowed.store(false, SeqCst);
        let worker = recording.clone();
        assert!(matches!(worker.check(), Err(Error::Stopped)));
        let launch: Arc<dyn ports::LaunchCheck> = Arc::new(UnknownLaunch);
        worker.unstopped(launch.clone());
        assert!(Arc::ptr_eq(&recording.take().unwrap(), &launch));
        assert!(recording.take().is_none());
        drop(worker);
        drop(recording);
        assert!(weak.upgrade().is_none());
    }
}
