// SPDX-License-Identifier: Apache-2.0
//! Windows window actions with a testable native boundary and opaque catalog ids.
use async_trait::async_trait;
use ports::{
    Clock, PlatformError,
    windows::{WindowActions, WindowAuthority},
};
use protocol::{capability::Availability, windows::*};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};
#[cfg(windows)]
#[allow(unsafe_code)]
mod native;

#[derive(Clone, Debug, PartialEq)]
pub struct NativeWindow {
    pub handle: usize,
    pub pid: u32,
    pub title: String,
    pub app: String,
    pub monitor: Option<String>,
    pub focused: bool,
}
pub trait Driver: Send + Sync + std::fmt::Debug + 'static {
    fn list(&self) -> Result<Vec<NativeWindow>, PlatformError>;
    fn focus(&self, window: &NativeWindow) -> Result<(), PlatformError>;
    fn close(&self, window: &NativeWindow) -> Result<(), PlatformError>;
    fn launch(&self, shortcut: &Path) -> Result<(), PlatformError>;
}
#[derive(Clone, Debug)]
struct App {
    path: PathBuf,
    name: String,
    fingerprint: u64,
}
#[derive(Debug)]
pub struct WindowsActions {
    driver: Arc<dyn Driver>,
    roots: Vec<PathBuf>,
    windows: tokio::sync::Mutex<BTreeMap<String, NativeWindow>>,
    apps: tokio::sync::Mutex<BTreeMap<String, App>>,
    clock: Arc<dyn Clock>,
}
impl WindowsActions {
    pub fn new(driver: Arc<dyn Driver>, roots: Vec<PathBuf>, clock: Arc<dyn Clock>) -> Self {
        Self {
            driver,
            roots,
            clock,
            windows: Default::default(),
            apps: Default::default(),
        }
    }
    #[cfg(windows)]
    pub fn from_environment(env: &BTreeMap<String, String>, clock: Arc<dyn Clock>) -> Self {
        let roots = ["APPDATA", "PROGRAMDATA"]
            .into_iter()
            .filter_map(|key| env.get(key))
            .map(|p| PathBuf::from(p).join("Microsoft/Windows/Start Menu/Programs"))
            .collect();
        Self::new(Arc::new(native::Native), roots, clock)
    }
}
#[async_trait]
impl WindowActions for WindowsActions {
    async fn capabilities(&self) -> WindowCapabilities {
        WindowCapabilities {
            backend: "windows".into(),
            windows: Availability::Available,
            open_apps: Availability::Available,
        }
    }
    async fn catalog(&self, search: &str) -> Result<WindowCatalog, PlatformError> {
        let driver = self.driver.clone();
        let native = tokio::task::spawn_blocking(move || driver.list())
            .await
            .map_err(|_| {
                PlatformError::TemporarilyUnavailable("Windows window enumeration stopped".into())
            })??;
        let mut windows = self.windows.lock().await;
        let mut next = BTreeMap::new();
        for w in native {
            let id = windows
                .iter()
                .find(|(_, old)| old.handle == w.handle && old.pid == w.pid && old.app == w.app)
                .map(|(id, _)| id.clone())
                .unwrap_or_else(|| protocol::CommandId::new().to_string());
            next.insert(id, w);
        }
        *windows = next;
        let query = search.to_lowercase();
        let list: Vec<_> = windows
            .iter()
            .filter(|(_, w)| {
                format!("{} {}", w.title, w.app)
                    .to_lowercase()
                    .contains(&query)
            })
            .map(|(id, w)| WindowInfo {
                window_id: id.clone(),
                title: w.title.clone(),
                app: w.app.clone(),
                workspace: None,
                monitor: w.monitor.clone(),
                focused: w.focused,
            })
            .collect();
        let roots = self.roots.clone();
        let discovered = tokio::task::spawn_blocking(move || discover(&roots))
            .await
            .map_err(|_| PlatformError::Native("Start menu discovery stopped".into()))??;
        let mut apps = self.apps.lock().await;
        let mut next = BTreeMap::new();
        for app in discovered {
            let id = apps
                .iter()
                .find(|(_, old)| old.path == app.path && old.fingerprint == app.fingerprint)
                .map(|(id, _)| id.clone())
                .unwrap_or_else(|| protocol::CommandId::new().to_string());
            next.insert(id, app);
        }
        *apps = next;
        let apps: Vec<_> = apps
            .iter()
            .filter(|(_, a)| a.name.to_lowercase().contains(&query))
            .map(|(id, a)| InstalledApp {
                app_id: id.clone(),
                name: a.name.clone(),
            })
            .collect();
        let truncated = list.len() > 200 || apps.len() > 200;
        Ok(WindowCatalog {
            capabilities: self.capabilities().await,
            windows: list.into_iter().take(200).collect(),
            apps: apps.into_iter().take(200).collect(),
            observed_at: self.clock.now(),
            truncated,
        })
    }
    async fn act(
        &self,
        action: &WindowAction,
        authority: Arc<dyn WindowAuthority>,
    ) -> Result<(WindowOutcome, String), PlatformError> {
        let driver = self.driver.clone();
        if let WindowAction::OpenApp { app_id } = action {
            let app = self
                .apps
                .lock()
                .await
                .get(app_id)
                .cloned()
                .ok_or(PlatformError::TargetNotFound)?;
            return tokio::task::spawn_blocking(move || {
                if fingerprint(&app.path)? != app.fingerprint {
                    return Err(PlatformError::TargetNotFound);
                }
                authority.check()?;
                driver.launch(&app.path)?;
                Ok((
                    WindowOutcome::AppLaunchRequested,
                    "Asked Windows to open the app. A window may take time to appear.".into(),
                ))
            })
            .await
            .map_err(|_| {
                PlatformError::OutcomeUnknown("Windows did not confirm this app launch".into())
            })?;
        }
        let (id, close) = match action {
            WindowAction::Focus { window_id } => (window_id, false),
            WindowAction::Close { window_id } => (window_id, true),
            _ => unreachable!(),
        };
        let previous = self
            .windows
            .lock()
            .await
            .get(id)
            .cloned()
            .ok_or(PlatformError::TargetNotFound)?;
        tokio::task::spawn_blocking(move || {
            let window = driver
                .list()?
                .into_iter()
                .find(|w| {
                    w.handle == previous.handle && w.pid == previous.pid && w.app == previous.app
                })
                .ok_or(PlatformError::TargetNotFound)?;
            authority.check()?;
            if close {
                driver.close(&window)?;
                Ok((
                    WindowOutcome::CloseRequested,
                    "Close requested. Unsaved work may prompt on the computer.".into(),
                ))
            } else {
                driver.focus(&window)?;
                Ok((
                    WindowOutcome::Focused,
                    "Windows confirmed that the window received focus.".into(),
                ))
            }
        })
        .await
        .map_err(|_| {
            PlatformError::OutcomeUnknown("Windows did not confirm this window action".into())
        })?
    }
}
fn fingerprint(path: &Path) -> Result<u64, PlatformError> {
    use std::{
        hash::{Hash, Hasher},
        io::Read,
    };
    let file = std::fs::File::open(path)?;
    if !file.metadata()?.is_file() {
        return Err(PlatformError::TargetNotFound);
    }
    let mut bytes = Vec::new();
    file.take(1024 * 1024 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > 1024 * 1024 {
        return Err(PlatformError::Unsupported(
            "Start menu shortcut is too large".into(),
        ));
    }
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hash);
    Ok(hash.finish())
}
fn discover(roots: &[PathBuf]) -> Result<Vec<App>, PlatformError> {
    let mut apps = Vec::new();
    let mut count = 0;
    for root in roots {
        let mut dirs = vec![(root.clone(), 0)];
        while let Some((dir, depth)) = dirs.pop() {
            if depth > 8 {
                continue;
            }
            let children = match std::fs::read_dir(dir) {
                Ok(c) => c,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e.into()),
            };
            for child in children {
                let child = child?;
                count += 1;
                if count > 4096 {
                    return Err(PlatformError::TemporarilyUnavailable(
                        "The Start menu exceeds the app catalog limit".into(),
                    ));
                }
                let path = child.path();
                if child.file_type()?.is_dir() {
                    dirs.push((path, depth + 1));
                    continue;
                }
                if path
                    .extension()
                    .and_then(|s| s.to_str())
                    .is_none_or(|s| !s.eq_ignore_ascii_case("lnk"))
                {
                    continue;
                }
                let Some(name) = path.file_stem().and_then(|s| s.to_str()).map(str::to_owned)
                else {
                    continue;
                };
                if let Ok(fingerprint) = fingerprint(&path) {
                    apps.push(App {
                        path,
                        name,
                        fingerprint,
                    });
                }
            }
        }
    }
    apps.sort_by_key(|a| a.name.to_lowercase());
    Ok(apps)
}
#[cfg(test)]
mod tests;
