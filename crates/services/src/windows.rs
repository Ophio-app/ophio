// SPDX-License-Identifier: Apache-2.0
//! Window commands serialize with the existing target's input and release barrier.
use crate::{Caller, CompanionHub, error, unsupported};
use protocol::{
    ErrorCode, ProtocolError,
    auth::Principal,
    command::CommandResult,
    control::{DesktopLockState, InputState, LeaseOwner, LeaseState},
    windows::*,
};
use std::{sync::atomic::Ordering, time::Instant};
struct Authority {
    hub: CompanionHub,
    caller: Caller,
    deadline: Instant,
}
impl ports::windows::WindowAuthority for Authority {
    fn check(&self) -> Result<(), ports::PlatformError> {
        if self.hub.inner.clock.monotonic() >= self.deadline
            || *self.hub.inner.shutdown.borrow()
            || self.hub.inner.local_stops.load(Ordering::SeqCst) != 0
            || self
                .hub
                .authorize(&self.caller, |g| g.control_input)
                .is_err()
        {
            return Err(ports::PlatformError::PermissionMissing(
                "Control ended, or this device’s access changed, before the action could be sent"
                    .into(),
            ));
        }
        Ok(())
    }
}
impl CompanionHub {
    pub(crate) async fn window_catalog(
        &self,
        search: &str,
    ) -> Result<WindowCatalog, ProtocolError> {
        if search.len() > 512 {
            return Err(error(ErrorCode::LimitExceeded, "App search is too long"));
        }
        let catalog = self
            .inner
            .windows
            .as_ref()
            .ok_or_else(|| unsupported("Window actions"))?
            .catalog(search)
            .await
            .map_err(window_error)?;
        // Window titles and apps show what is on the screen, so they follow
        // capture's rule: refused while the computer is locked, and while a
        // desktop that observes its lock cannot tell whether it is.
        if let Some(desktop) = self.desktop_option() {
            let refused = match desktop.desktop_lock().await {
                DesktopLockState::Locked => Some("Computer is locked"),
                DesktopLockState::Unknown if desktop.lock_changes().is_some() => {
                    Some("Computer lock state is unknown")
                }
                DesktopLockState::Unknown | DesktopLockState::Unlocked => None,
            };
            if let Some(reason) = refused {
                return Err(error(ErrorCode::TemporarilyUnavailable, reason));
            }
        }
        Ok(catalog)
    }
    pub(crate) async fn window_action(
        &self,
        caller: &Caller,
        control: &WindowControl,
        action: &WindowAction,
    ) -> Result<Option<CommandResult>, ProtocolError> {
        let caller = self.authorize(caller, |g| g.control_input)?;
        let Principal::Device { device_id } = caller.principal else {
            return Err(error(
                ErrorCode::PermissionDenied,
                "Only a paired device in control can act on windows",
            ));
        };
        let backend = self
            .inner
            .windows
            .as_ref()
            .ok_or_else(|| unsupported("Window actions"))?;
        let _desktop = self.inner.desktop_operation.read().await;
        let target = self
            .inner
            .controls
            .lock()
            .await
            .get(&control.target_id)
            .cloned()
            .ok_or_else(stale)?;
        let mut target = target.lock().await;
        let lease = target.view.lease.as_ref().ok_or_else(stale)?;
        if target.removed
            || target.view.input_state != InputState::Human
            || lease.state != LeaseState::Active
            || lease.owner != (LeaseOwner::Human { device_id })
            || lease.lease_id != control.lease_id
            || lease.generation != control.generation
            || lease.boot_epoch != control.boot_epoch
            || control.boot_epoch != self.inner.epoch
        {
            return Err(stale());
        }
        let deadline = target.deadline.ok_or_else(stale)?;
        if self.inner.clock.monotonic() >= deadline {
            self.revoke_locked(&mut target, true).await?;
            return Err(stale());
        }
        self.refresh_geometry(&mut target).await?;
        if control.geometry_revision != target.geometry.revision {
            return Err(error(
                ErrorCode::StaleRevision,
                "The control target changed. Review it before acting.",
            ));
        }
        {
            let protection = self.inner.protection.lock().await;
            if protection.required && protection.indicator.is_none() {
                return Err(error(
                    ErrorCode::TemporarilyUnavailable,
                    crate::runtime::INDICATOR_UNAVAILABLE,
                ));
            }
        }
        let id = match action {
            WindowAction::Focus { window_id } | WindowAction::Close { window_id } => window_id,
            WindowAction::OpenApp { app_id } => app_id,
        };
        if id.is_empty() || id.len() > 256 {
            return Err(error(
                ErrorCode::InvalidRequest,
                "Invalid window or app identity",
            ));
        }
        let authority = std::sync::Arc::new(Authority {
            hub: self.clone(),
            caller: caller.clone(),
            deadline,
        });
        ports::windows::WindowAuthority::check(authority.as_ref()).map_err(window_error)?;
        let requested_at = self.inner.clock.now();
        // Adapters bound their native calls and join/stop work before returning.
        // Do not cancel a native call and then release this target lock early.
        let result = backend.act(action, authority).await;
        let (status, summary) = match &result {
            Ok((_, detail)) => (protocol::model::FactStatus::Done, detail.clone()),
            Err(ports::PlatformError::OutcomeUnknown(_)) => (
                protocol::model::FactStatus::Unknown,
                "The desktop did not confirm this window action".into(),
            ),
            Err(_) => (
                protocol::model::FactStatus::Failed,
                "The window action was refused before dispatch was confirmed".into(),
            ),
        };
        if self
            .write(|tx| {
                tx.emit(protocol::Event::InterventionRecorded {
                    task_id: None,
                    target_id: Some(control.target_id),
                    receipt: Box::new(protocol::model::InterventionReceipt {
                        kind: protocol::model::InterventionKind::WindowAction,
                        requested_at,
                        facts: vec![protocol::model::ReceiptFact {
                            label: "Window action dispatch".into(),
                            status,
                            detail: Some(format!(
                                "{} · Control lease {}",
                                summary, control.lease_id
                            )),
                        }],
                        summary: summary.clone(),
                    }),
                })?;
                Ok(())
            })
            .is_err()
        {
            let _ = self.revoke_locked(&mut target, false).await;
            return Err(error(
                ErrorCode::OutcomeUnknown,
                "The desktop action finished but its control receipt could not be saved",
            ));
        }
        match result {
            Ok((outcome, detail)) => Ok(Some(CommandResult::WindowAction {
                receipt: WindowActionReceipt {
                    target_id: control.target_id,
                    lease_id: control.lease_id,
                    requested_at,
                    action: action.clone(),
                    outcome,
                    detail,
                },
            })),
            Err(error) => {
                if matches!(error, ports::PlatformError::OutcomeUnknown(_)) {
                    let _ = self.revoke_locked(&mut target, false).await;
                }
                Err(window_error(error))
            }
        }
    }
}
fn stale() -> ProtocolError {
    error(
        ErrorCode::Expired,
        "Enable control again before using window actions",
    )
}
fn window_error(error: ports::PlatformError) -> ProtocolError {
    let code = error.code();
    // Adapter messages are authored, content-free diagnostics. Native stdout is never included.
    let message = match error {
        ports::PlatformError::Unsupported(s)
        | ports::PlatformError::PermissionMissing(s)
        | ports::PlatformError::TemporarilyUnavailable(s)
        | ports::PlatformError::OutcomeUnknown(s) => s,
        ports::PlatformError::TargetNotFound => {
            "This window or app changed. Refresh the list before trying again.".into()
        }
        _ => "The desktop could not complete this window operation".into(),
    };
    protocol::ProtocolError::new(code, message)
}
#[cfg(test)]
#[path = "windows_tests.rs"]
mod tests;
