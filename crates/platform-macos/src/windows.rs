// SPDX-License-Identifier: Apache-2.0
//! Opaque Mac window identities. Grants and confirmation remain in services.
use async_trait::async_trait;
use ports::{
    Clock, PlatformError,
    windows::{FocusedWindow, WindowActions, WindowAuthority},
};
use protocol::{capability::Availability, voice::VoiceApp, windows::*};
use std::{collections::BTreeMap, sync::Arc};
use voice_control::{Launcher, apps::LaunchApp};

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
mod native;
#[cfg(test)]
mod tests;

#[derive(Debug, Clone, PartialEq)]
pub struct NativeWindow {
    /// Backend-owned identity for a retained native window, never a title or index.
    pub identity: String,
    pub pid: i32,
    pub app: String,
    pub title: String,
    pub focused: bool,
    pub bounds: protocol::voice::VoiceBounds,
}
/// Native dispatch must revalidate identity and permission, then check authority
/// immediately before its first effect. Returning leaves no deferred adapter job.
pub trait Driver: Send + Sync + std::fmt::Debug + 'static {
    fn availability(&self) -> Availability;
    fn list(&self) -> Result<Vec<NativeWindow>, PlatformError>;
    fn act(
        &self,
        expected: &NativeWindow,
        close: bool,
        authority: &dyn WindowAuthority,
    ) -> Result<(), PlatformError>;
}
#[derive(Debug)]
pub struct MacWindows {
    driver: Arc<dyn Driver>,
    launcher: Arc<dyn Launcher>,
    windows: tokio::sync::Mutex<BTreeMap<String, NativeWindow>>,
    apps: tokio::sync::Mutex<BTreeMap<String, VoiceApp>>,
    clock: Arc<dyn Clock>,
}
impl MacWindows {
    pub fn new(
        driver: Arc<dyn Driver>,
        launcher: Arc<dyn Launcher>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            driver,
            launcher,
            clock,
            windows: Default::default(),
            apps: Default::default(),
        }
    }
    #[cfg(target_os = "macos")]
    pub fn from_environment(environment: &BTreeMap<String, String>, clock: Arc<dyn Clock>) -> Self {
        Self::new(
            Arc::new(native::Native::default()),
            Arc::new(crate::launch::MacLauncher::from_environment(environment)),
            clock,
        )
    }
    async fn list(&self) -> Result<Vec<NativeWindow>, PlatformError> {
        let driver = self.driver.clone();
        tokio::task::spawn_blocking(move || driver.list())
            .await
            .map_err(|_| {
                PlatformError::TemporarilyUnavailable(
                    "The Mac window list stopped responding".into(),
                )
            })?
    }
}
fn unavailable(error: voice_control::Error) -> PlatformError {
    match error {
        voice_control::Error::StaleTarget => PlatformError::TargetNotFound,
        voice_control::Error::Stopped => {
            PlatformError::PermissionMissing("Control ended before the app opened".into())
        }
        voice_control::Error::OutcomeUnknown => {
            PlatformError::OutcomeUnknown("The Mac has not confirmed the app launch".into())
        }
        voice_control::Error::LaunchUnstopped => PlatformError::LaunchUnstopped,
        other => PlatformError::TemporarilyUnavailable(other.to_string()),
    }
}
struct LaunchAuthority(Arc<dyn WindowAuthority>);
impl voice_control::Authority for LaunchAuthority {
    fn check(&self) -> Result<(), voice_control::Error> {
        self.0.check().map_err(|_| voice_control::Error::Stopped)
    }
    fn unstopped(&self, launch: Arc<dyn ports::LaunchCheck>) {
        self.0.unstopped(launch);
    }
}
#[async_trait]
impl WindowActions for MacWindows {
    async fn capabilities(&self) -> WindowCapabilities {
        WindowCapabilities {
            backend: "macos".into(),
            windows: self.driver.availability(),
            open_apps: if cfg!(target_os = "macos") || cfg!(test) {
                Availability::Available
            } else {
                Availability::Unsupported {
                    reason: "Mac app launching requires macOS".into(),
                }
            },
        }
    }
    async fn catalog(&self, search: &str) -> Result<WindowCatalog, PlatformError> {
        let mut capabilities = self.capabilities().await;
        let native = if capabilities.windows.is_available() {
            match self.list().await {
                Ok(windows) => windows,
                Err(PlatformError::PermissionMissing(remedy)) => {
                    capabilities.windows = Availability::PermissionMissing { remedy };
                    vec![]
                }
                Err(PlatformError::TemporarilyUnavailable(reason)) => {
                    capabilities.windows = Availability::TemporarilyUnavailable { reason };
                    vec![]
                }
                Err(error) => return Err(error),
            }
        } else {
            vec![]
        };
        let query = search.to_lowercase();
        let mut windows = self.windows.lock().await;
        *windows = native
            .into_iter()
            .map(|w| (w.identity.clone(), w))
            .collect();
        let list: Vec<_> = windows
            .iter()
            .filter(|(_, w)| {
                format!("{} {}", w.app, w.title)
                    .to_lowercase()
                    .contains(&query)
            })
            .map(|(id, w)| WindowInfo {
                window_id: id.clone(),
                title: w.title.clone(),
                app: w.app.clone(),
                focused: w.focused,
                workspace: None,
                monitor: None,
            })
            .collect();
        drop(windows);
        let discovered = match self.launcher.apps().await {
            Ok(apps) => apps,
            Err(error) => {
                capabilities.open_apps = Availability::TemporarilyUnavailable {
                    reason: error.to_string(),
                };
                vec![]
            }
        };
        let mut apps = self.apps.lock().await;
        *apps = discovered
            .into_iter()
            .filter(|a| a.listed)
            .map(|LaunchApp { id, name, .. }| (id.clone(), VoiceApp { id, name }))
            .collect();
        let apps: Vec<_> = apps
            .iter()
            .filter(|(_, a)| a.name.to_lowercase().contains(&query))
            .map(|(id, a)| InstalledApp {
                app_id: id.clone(),
                name: a.name.clone(),
            })
            .collect();
        Ok(WindowCatalog {
            capabilities,
            observed_at: self.clock.now(),
            truncated: list.len() > 200 || apps.len() > 200,
            windows: list.into_iter().take(200).collect(),
            apps: apps.into_iter().take(200).collect(),
        })
    }
    async fn act(
        &self,
        action: &WindowAction,
        authority: Arc<dyn WindowAuthority>,
    ) -> Result<(WindowOutcome, String), PlatformError> {
        if let WindowAction::OpenApp { app_id } = action {
            let app = self
                .apps
                .lock()
                .await
                .get(app_id)
                .cloned()
                .ok_or(PlatformError::TargetNotFound)?;
            self.launcher
                .launch(&app, &LaunchAuthority(authority))
                .await
                .map_err(unavailable)?;
            return Ok((
                WindowOutcome::AppLaunchRequested,
                "Asked macOS to open the app. A window may take time to appear.".into(),
            ));
        }
        let (id, close) = match action {
            WindowAction::Focus { window_id } => (window_id, false),
            WindowAction::Close { window_id } => (window_id, true),
            _ => unreachable!(),
        };
        let expected = self
            .windows
            .lock()
            .await
            .get(id)
            .cloned()
            .ok_or(PlatformError::TargetNotFound)?;
        let driver = self.driver.clone();
        tokio::task::spawn_blocking(move || driver.act(&expected, close, &*authority))
            .await
            .map_err(|_| {
                PlatformError::OutcomeUnknown("The Mac did not confirm the window action".into())
            })??;
        Ok(if close {
            (
                WindowOutcome::CloseRequested,
                "Close requested. Unsaved work may prompt on the computer.".into(),
            )
        } else {
            (
                WindowOutcome::Focused,
                "macOS confirmed that the window received focus.".into(),
            )
        })
    }
    async fn focused(&self) -> Option<FocusedWindow> {
        let window = self.list().await.ok()?.into_iter().find(|w| w.focused)?;
        Some(FocusedWindow {
            id: window.identity,
            app: window.app,
            x: window.bounds.x,
            y: window.bounds.y,
            width: window.bounds.width,
            height: window.bounds.height,
        })
    }
}
