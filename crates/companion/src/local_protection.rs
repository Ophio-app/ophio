// SPDX-License-Identifier: Apache-2.0
//! Local protection has no network authority. It calls the same control arbiter.
use services::CompanionHub;
use std::{collections::BTreeMap, sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;

pub(crate) struct ProtectionTask {
    cancel: CancellationToken,
    task: Option<tokio::task::JoinHandle<()>>,
}
impl ProtectionTask {
    pub(crate) async fn close(mut self) {
        self.cancel.cancel();
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}
/// Control stays closed until the indicator is reachable. The notification
/// service may start after the companion or restart under it, so connection is
/// retried in the background instead of disabling control until restart.
pub(crate) async fn start(
    hub: Arc<CompanionHub>,
    environment: &BTreeMap<String, String>,
) -> ProtectionTask {
    // Missing desktop/session services never fall back to another user's bus.
    hub.set_local_indicator(None).await;
    let environment = environment.clone();
    let cancel = CancellationToken::new();
    let stopping = cancel.clone();
    let task = tokio::spawn(async move {
        let mut delay = Duration::from_secs(1);
        let mut reported = false;
        loop {
            let session = tokio::select! {
                biased;
                _ = stopping.cancelled() => break,
                session = tokio::time::timeout(Duration::from_secs(5), native(&environment)) => session,
            };
            let session = match session {
                Ok(Ok(session)) => session,
                failed => {
                    let permanent =
                        matches!(failed, Ok(Err(ports::PlatformError::PermissionMissing(_))));
                    if !reported {
                        reported = true;
                        match &failed {
                            Ok(Err(error)) => {
                                tracing::warn!(%error, "Local Stop indicator unavailable; computer control stays off")
                            }
                            _ => tracing::warn!(
                                "Local Stop indicator did not answer; computer control stays off"
                            ),
                        }
                    }
                    if permanent {
                        break;
                    }
                    tokio::select! {
                        biased;
                        _ = stopping.cancelled() => break,
                        _ = tokio::time::sleep(delay) => {}
                    }
                    delay = (delay * 2).min(Duration::from_secs(30));
                    continue;
                }
            };
            delay = Duration::from_secs(1);
            reported = false;
            hub.set_local_indicator(Some(session.indicator.clone()))
                .await;
            tracing::info!("Local Stop indicator connected");
            let mut stops = session.stop;
            let lost = loop {
                tokio::select! {
                    biased;
                    _ = stopping.cancelled() => break false,
                    event = stops.recv() => {
                        if let Err(error) = hub.stop_all_control().await {
                            tracing::warn!(code=?error.code, "Local Stop could not confirm every control barrier");
                        }
                        if event.is_none() { break true; }
                    }
                }
            };
            let _ = session.indicator.hide().await;
            if !lost {
                break;
            }
            hub.set_local_indicator(None).await;
            tracing::warn!("Local Stop indicator lost; reconnecting");
        }
    });
    ProtectionTask {
        cancel,
        task: Some(task),
    }
}
async fn native(
    environment: &BTreeMap<String, String>,
) -> Result<ports::desktop::IndicatorSession, ports::PlatformError> {
    #[cfg(target_os = "linux")]
    {
        let address = environment.get("DBUS_SESSION_BUS_ADDRESS").ok_or_else(|| {
            ports::PlatformError::PermissionMissing(
                "A graphical session notification service is required for local Stop".into(),
            )
        })?;
        let connection = zbus::connection::Builder::address(address.as_str())
            .map_err(|_| ports::PlatformError::Native("Invalid notification bus address".into()))?
            .build()
            .await
            .map_err(|_| {
                ports::PlatformError::Native("Cannot reach the notification service".into())
            })?;
        platform_linux::protection::connect(connection).await
    }
    #[cfg(windows)]
    {
        let _ = environment;
        platform_windows::protection::connect().await
    }
}

impl Drop for ProtectionTask {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}
