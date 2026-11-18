// SPDX-License-Identifier: Apache-2.0
//! Start-menu and packaged app voice launching. Spoken text is never a command.
use async_trait::async_trait;
use protocol::voice::VoiceApp;
#[cfg(any(windows, test))]
use std::path::Path;
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use voice_control::{
    Authority, Error, Launcher,
    apps::{AppRole, LaunchApp},
};

#[cfg(windows)]
#[allow(unsafe_code)]
mod apps_folder;

#[cfg(windows)]
#[allow(unsafe_code)]
mod native;
#[cfg(test)]
mod tests;

const PREPARE: Duration = Duration::from_millis(1500);
const OPEN: Duration = Duration::from_millis(2000);
const _: () = {
    assert!(2 * PREPARE.as_millis() + OPEN.as_millis() <= voice_control::LAUNCH_LIMIT.as_millis());
    assert!(OPEN.as_millis() <= voice_control::LAUNCH_AFTER_CHECK.as_millis());
};

/// The entire shortcut is retained, not a non-cryptographic content hash.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub path: PathBuf,
    pub name: String,
    pub bytes: Vec<u8>,
}
/// An OS-enumerated packaged application, including full package version.
/// These descriptors never come from the phone or spoken command text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackagedApp {
    pub aumid: String,
    pub package: String,
    pub name: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
enum CatalogEntry {
    Shortcut(Entry),
    Packaged(PackagedApp),
}
impl CatalogEntry {
    fn name(&self) -> &str {
        match self {
            Self::Shortcut(app) => &app.name,
            Self::Packaged(app) => &app.name,
        }
    }
    fn aumid(&self) -> Option<&str> {
        match self {
            Self::Packaged(app) => Some(&app.aumid),
            _ => None,
        }
    }
}
struct CatalogSnapshot {
    entries: Vec<CatalogEntry>,
    defaults: BTreeMap<AppRole, String>,
}

/// A prepared request retains its native resources until `open` ends. Shortcut
/// requests hold identity locks; packaged activation cannot pin a package version.
/// It stays on the preparing thread, allowing thread-affine shell resources.
pub trait Prepared {
    fn open(self: Box<Self>) -> Result<(), Error>;
}
pub trait Platform: Send + Sync + std::fmt::Debug + 'static {
    fn entries(&self) -> Result<Vec<Entry>, Error>;
    /// Revalidates the shortcut, locking it and its ancestors against replacement.
    fn prepare(&self, entry: &Entry) -> Result<Box<dyn Prepared>, Error>;
    fn prepare_url(&self, url: &str) -> Result<Box<dyn Prepared>, Error>;
    fn packaged_entries(&self) -> Result<Vec<PackagedApp>, Error> {
        Ok(vec![])
    }
    fn prepare_packaged(&self, _app: &PackagedApp) -> Result<Box<dyn Prepared>, Error> {
        Err(Error::StaleTarget)
    }
    /// Only OS-selected defaults, keyed by exact validated application identity.
    /// Unavailable associations remain absent, never guessed from app labels.
    fn packaged_defaults(&self) -> Result<BTreeMap<AppRole, String>, Error> {
        Ok(BTreeMap::new())
    }
}
#[derive(Debug)]
pub struct WindowsLauncher {
    platform: Arc<dyn Platform>,
    catalog: tokio::sync::Mutex<BTreeMap<String, CatalogEntry>>,
}
impl WindowsLauncher {
    pub fn new(platform: Arc<dyn Platform>) -> Self {
        Self {
            platform,
            catalog: Default::default(),
        }
    }
    #[cfg(windows)]
    pub fn from_environment(environment: &BTreeMap<String, String>) -> Self {
        Self::new(Arc::new(native::Native {
            roots: start_menu_roots(environment),
        }))
    }
    async fn entries(&self, with_defaults: bool) -> Result<CatalogSnapshot, Error> {
        let platform = self.platform.clone();
        tokio::time::timeout(
            PREPARE,
            tokio::task::spawn_blocking(move || {
                let mut entries: Vec<_> = platform
                    .entries()?
                    .into_iter()
                    .map(CatalogEntry::Shortcut)
                    .collect();
                entries.extend(
                    platform
                        .packaged_entries()?
                        .into_iter()
                        .map(CatalogEntry::Packaged),
                );
                // A missing association must not break explicit app launch.
                let defaults = if with_defaults {
                    platform.packaged_defaults().unwrap_or_default()
                } else {
                    BTreeMap::new()
                };
                Ok(CatalogSnapshot { entries, defaults })
            }),
        )
        .await
        .map_err(|_| unavailable("The Windows app catalog did not answer in time"))?
        .map_err(|_| unavailable("The Windows app catalog could not be read"))?
    }
    async fn open(&self, request: Request, authority: Arc<dyn Authority>) -> Result<(), Error> {
        authority.check()?;
        let platform = self.platform.clone();
        let canceled = Arc::new(AtomicBool::new(false));
        let guard = Cancel(canceled.clone());
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let (go_tx, go_rx) = std::sync::mpsc::sync_channel(1);
        let deadline = Instant::now() + PREPARE + OPEN;
        let dispatch_authority = authority.clone();
        // Only preparation may continue after cancellation. The delivery gate is
        // closed on future drop, Stop, timeout, or a lost receiver.
        let mut worker = tokio::task::spawn_blocking(move || {
            let prepared = match request {
                Request::App(CatalogEntry::Shortcut(entry)) => platform.prepare(&entry),
                Request::App(CatalogEntry::Packaged(app)) => platform.prepare_packaged(&app),
                Request::Url(url) => platform.prepare_url(&url),
            };
            let prepared = match prepared {
                Ok(prepared) => prepared,
                Err(error) => {
                    let _ = ready_tx.send(Err(error));
                    return Ok(());
                }
            };
            if ready_tx.send(Ok(())).is_err() {
                return Ok(());
            }
            if go_rx.recv_timeout(PREPARE + OPEN).is_err()
                || canceled.load(Ordering::SeqCst)
                || Instant::now() >= deadline
            {
                return Err(Error::Stopped);
            }
            // The caller's gate is not authority: Stop or lease expiry may
            // occur while this worker is descheduled after receiving delivery.
            // All potentially blocking identity/package preparation is finished.
            // Native open must dispatch directly, without another queued task.
            dispatch_authority.check()?;
            if canceled.load(Ordering::SeqCst) || Instant::now() >= deadline {
                return Err(Error::Stopped);
            }
            prepared.open()
        });
        tokio::time::timeout(PREPARE, ready_rx)
            .await
            .map_err(|_| unavailable("Windows could not prepare the app in time"))?
            .map_err(|_| unavailable("Windows app preparation stopped"))??;
        // The host checks its permission, Stop and lease deadline only after
        // native identity validation, immediately before releasing delivery.
        authority.check()?;
        go_tx
            .send(())
            .map_err(|_| unavailable("Windows app preparation stopped"))?;
        let result = match tokio::time::timeout(OPEN, &mut worker).await {
            Ok(Ok(result)) => result,
            // A shell request may already have escaped to another process. Do
            // not claim it was canceled or that the app cannot appear later.
            _ => Err(Error::LaunchUnstopped),
        };
        drop(guard);
        result
    }
}
#[cfg(any(windows, test))]
fn start_menu_roots(environment: &BTreeMap<String, String>) -> Vec<PathBuf> {
    ["APPDATA", "PROGRAMDATA"]
        .into_iter()
        .filter_map(|key| {
            // Windows environment variable names are case-insensitive; inherited
            // ProgramData commonly keeps mixed case in the captured environment.
            environment
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case(key))
                .map(|(_, value)| {
                    PathBuf::from(value).join("Microsoft/Windows/Start Menu/Programs")
                })
        })
        .collect()
}

struct Cancel(Arc<AtomicBool>);
impl Drop for Cancel {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}
enum Request {
    App(CatalogEntry),
    Url(String),
}
fn unavailable(reason: &str) -> Error {
    Error::Unavailable(reason.into())
}

#[async_trait]
impl Launcher for WindowsLauncher {
    async fn apps(&self) -> Result<Vec<LaunchApp>, Error> {
        let snapshot = self.entries(true).await?;
        let mut catalog = self.catalog.lock().await;
        let mut next = BTreeMap::new();
        for entry in snapshot.entries {
            let id = catalog
                .iter()
                .find(|(_, old)| **old == entry)
                .map(|(id, _)| id.clone())
                .unwrap_or_else(|| protocol::CommandId::new().to_string());
            next.insert(id, entry);
        }
        *catalog = next;
        let mut identity_counts = BTreeMap::new();
        for aumid in catalog.values().filter_map(CatalogEntry::aumid) {
            *identity_counts.entry(aumid).or_insert(0usize) += 1;
        }
        Ok(catalog
            .iter()
            .map(|(id, entry)| {
                let defaults: Vec<_> = snapshot
                    .defaults
                    .iter()
                    .filter_map(|(role, aumid)| {
                        // Ambiguous identities are not a trustworthy default.
                        (entry.aumid() == Some(aumid.as_str())
                            && identity_counts.get(aumid.as_str()) == Some(&1))
                        .then_some(*role)
                    })
                    .collect();
                LaunchApp {
                    id: id.clone(),
                    name: entry.name().into(),
                    aliases: vec![],
                    roles: defaults.clone(),
                    defaults,
                    listed: true,
                    detail: Some(
                        match entry {
                            CatalogEntry::Shortcut(_) => "Windows Start menu",
                            CatalogEntry::Packaged(_) => "Windows Apps folder",
                        }
                        .into(),
                    ),
                }
            })
            .collect())
    }
    // A borrowed authority cannot cross the native worker handoff safely. Do
    // not substitute a snapshot of permission or fall back to an unchecked gate.
    async fn launch(&self, _: &VoiceApp, authority: &dyn Authority) -> Result<(), Error> {
        authority.check()?;
        Err(unavailable(
            "Windows app launch requires owned execution authority",
        ))
    }
    async fn open_url(&self, _: &str, authority: &dyn Authority) -> Result<(), Error> {
        authority.check()?;
        Err(unavailable(
            "Windows address launch requires owned execution authority",
        ))
    }
    async fn launch_authorized(
        &self,
        app: &VoiceApp,
        authority: Arc<dyn Authority>,
    ) -> Result<(), Error> {
        authority.check()?;
        let entry = self
            .catalog
            .lock()
            .await
            .get(&app.id)
            .cloned()
            .filter(|entry| entry.name() == app.name)
            .ok_or(Error::StaleTarget)?;
        // A fresh catalog also verifies membership, not just a caller-supplied path.
        if !self.entries(false).await?.entries.contains(&entry) {
            return Err(Error::StaleTarget);
        }
        self.open(Request::App(entry), authority).await
    }
    async fn open_url_authorized(
        &self,
        url: &str,
        authority: Arc<dyn Authority>,
    ) -> Result<(), Error> {
        let lower = url.to_ascii_lowercase();
        if !(lower.starts_with("https://") || lower.starts_with("http://"))
            || url.len() > 2048
            || url.chars().any(|c| c.is_whitespace() || c.is_control())
            || url
                .split_once("://")
                .is_none_or(|(_, host)| host.is_empty() || host.starts_with('/'))
        {
            return Err(Error::StaleTarget);
        }
        self.open(Request::Url(url.to_owned()), authority).await
    }
}

/// Bounded discovery. Reparse points are excluded, including ancestor folders.
/// Native preparation additionally holds Windows handles against replacement.
#[cfg(any(windows, test))]
fn discover(roots: &[PathBuf]) -> Result<Vec<Entry>, Error> {
    let mut entries = Vec::new();
    let mut count = 0;
    let mut total_bytes = 0;
    for root in roots {
        if !root.is_absolute() || root.ancestors().any(reparse) {
            continue;
        }
        let mut dirs = vec![(root.clone(), 0)];
        while let Some((dir, depth)) = dirs.pop() {
            if depth > 8 || reparse(&dir) {
                continue;
            }
            let children = match std::fs::read_dir(dir) {
                Ok(children) => children,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(_) => return Err(unavailable("The Start menu could not be read")),
            };
            for child in children {
                let child = child.map_err(|_| unavailable("The Start menu could not be read"))?;
                count += 1;
                if count > 4096 {
                    return Err(unavailable("The Start menu exceeds the app catalog limit"));
                }
                let path = child.path();
                if reparse(&path) {
                    continue;
                }
                if child.file_type().is_ok_and(|kind| kind.is_dir()) {
                    dirs.push((path, depth + 1));
                    continue;
                }
                if !path
                    .extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("lnk"))
                {
                    continue;
                }
                let Some(name) = path.file_stem().and_then(|name| name.to_str()) else {
                    continue;
                };
                if name.is_empty() || name.len() > 256 || name.chars().any(char::is_control) {
                    continue;
                }
                if let Ok(bytes) = read_shortcut(&path) {
                    total_bytes += bytes.len();
                    if total_bytes > 16 * 1024 * 1024 {
                        return Err(unavailable(
                            "The Start menu exceeds the app catalog size limit",
                        ));
                    }
                    entries.push(Entry {
                        name: name.into(),
                        path,
                        bytes,
                    });
                }
            }
        }
    }
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    entries.dedup_by(|a, b| a.path == b.path);
    Ok(entries)
}
#[cfg(any(windows, test))]
fn reparse(path: &Path) -> bool {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return false;
    };
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}
#[cfg(any(windows, test))]
fn read_shortcut(path: &Path) -> Result<Vec<u8>, Error> {
    use std::io::Read;
    let file = std::fs::File::open(path).map_err(|_| Error::StaleTarget)?;
    if !file.metadata().is_ok_and(|metadata| metadata.is_file()) {
        return Err(Error::StaleTarget);
    }
    let mut bytes = Vec::new();
    file.take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| Error::StaleTarget)?;
    if bytes.len() > 1024 * 1024 {
        return Err(Error::StaleTarget);
    }
    Ok(bytes)
}
