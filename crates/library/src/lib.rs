// SPDX-License-Identifier: Apache-2.0
//! Root-confined plain-text Library with recoverable mutations.
//!
//! See `SAFETY.md` for native filesystem concurrency and durability boundaries.
mod instructions;
mod path;
mod recovery;
mod replace;
mod watch;

pub use instructions::{InstructionConfig, SuggestedRoot};
pub use path::normalize;
pub use recovery::{RecoveryEntry, RecoverySnapshot};

use async_trait::async_trait;
use cap_std::fs::Dir;
use path::{Root, denied, io_error, new_file, open_file, read_file, sync_dir};
use ports::{
    clock::Clock,
    library::{LibraryChange, LibraryError, LibraryFs, RootSpec, SaveRequest, TrashEntry},
};
use protocol::{
    capability::HarnessKind,
    ids::LibraryRootId,
    library::{
        DirEntry, DirListing, EntryKind, FileContent, InstructionSource, LibraryRoot, NewlineStyle,
        RootKind, SaveOutcome, SearchHit, TextEncoding,
    },
    time::Timestamp,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard, atomic::AtomicBool},
    time::Duration,
};
use tokio::sync::broadcast;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct LibraryConfig {
    /// Dedicated private directory, outside registered Library roots.
    pub data_dir: PathBuf,
    pub max_file_bytes: u64,
    pub retention: Duration,
    /// Keep confirmed-save displaced files briefly to detect late native writes.
    pub confirmed_save_grace: Duration,
    pub search_timeout: Duration,
    pub search_max_entries: usize,
    pub instructions: InstructionConfig,
}
impl LibraryConfig {
    pub fn new(data_dir: PathBuf, instruction_home: PathBuf) -> Self {
        Self {
            data_dir,
            max_file_bytes: 2 * 1024 * 1024,
            retention: Duration::from_secs(30 * 86400),
            confirmed_save_grace: Duration::from_secs(5 * 60),
            search_timeout: Duration::from_secs(2),
            search_max_entries: 100_000,
            instructions: InstructionConfig::new(instruction_home),
        }
    }
}

#[derive(Debug)]
struct State {
    roots: BTreeMap<LibraryRootId, Arc<Root>>,
    watcher: Option<notify::RecommendedWatcher>,
}
#[derive(Debug)]
struct Core {
    config: LibraryConfig,
    data: Root,
    clock: Arc<dyn Clock>,
    state: Mutex<State>,
    events: broadcast::Sender<LibraryChange>,
    watch_queue: std::sync::mpsc::SyncSender<notify::Result<notify::Event>>,
    rescan_required: Arc<AtomicBool>,
}
#[derive(Debug, Clone)]
pub struct FsLibrary {
    core: Arc<Core>,
}

/// Starts the name of every temporary and recovery file the Library places in
/// a person's folder. Listings and search skip these names; paths cannot use them.
const INTERNAL_PREFIX: &str = ".ophio-";
/// The same files as named before the product was named. They may still sit
/// in people's folders after an upgrade, so they stay internal and recoverable.
const EARLIER_INTERNAL_PREFIX: &str = ".companion-";

/// Letter case is ignored, as Windows and case-insensitive folders elsewhere
/// ignore it: `.OPHIO-save-…` opens the same file there. Only ASCII letters
/// fold to the prefixes' ASCII letters in Windows' comparison.
fn internal_name(name: &str) -> bool {
    [INTERNAL_PREFIX, EARLIER_INTERNAL_PREFIX]
        .iter()
        .any(|prefix| {
            name.as_bytes()
                .get(..prefix.len())
                .is_some_and(|start| start.eq_ignore_ascii_case(prefix.as_bytes()))
        })
}

/// A save's replacement (`new`) or displaced original (`old`), beside the file.
fn save_name(id: &str, half: &str) -> String {
    format!("{INTERNAL_PREFIX}save-{id}-{half}")
}

/// Every name a save's half may have: the current one, then the earlier one.
fn save_names(id: &str, half: &str) -> [String; 2] {
    [
        save_name(id, half),
        format!("{EARLIER_INTERNAL_PREFIX}save-{id}-{half}"),
    ]
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Record {
    id: String,
    root_id: LibraryRootId,
    root_path: PathBuf,
    path: String,
    revision: String,
    at: Timestamp,
    /// Temporary names are opaque, never interpreted as client paths.
    temp: Option<String>,
    displaced: Option<String>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
fn revision(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn timestamp(metadata: &fs::Metadata) -> Option<Timestamp> {
    metadata
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_millis()).ok())
        .map(Timestamp)
}
fn decode(bytes: &[u8]) -> Result<(&str, bool, NewlineStyle), LibraryError> {
    let bom = bytes.starts_with(b"\xef\xbb\xbf");
    let text = std::str::from_utf8(if bom { &bytes[3..] } else { bytes })
        .map_err(|_| LibraryError::NotEditable)?;
    if text
        .chars()
        .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
    {
        return Err(LibraryError::NotEditable);
    }
    let crlf = text.as_bytes().windows(2).filter(|p| *p == b"\r\n").count();
    let lf = text.bytes().filter(|b| *b == b'\n').count();
    let style = if crlf == 0 {
        NewlineStyle::Lf
    } else if crlf == lf {
        NewlineStyle::CrLf
    } else {
        NewlineStyle::Mixed
    };
    Ok((text, bom, style))
}
fn encode(request: &SaveRequest) -> Vec<u8> {
    let text = match request.newline {
        NewlineStyle::Mixed => request.text.clone(),
        NewlineStyle::Lf => request.text.replace("\r\n", "\n"),
        NewlineStyle::CrLf => request.text.replace("\r\n", "\n").replace('\n', "\r\n"),
    };
    let mut bytes = if request.bom {
        b"\xef\xbb\xbf".to_vec()
    } else {
        Vec::new()
    };
    bytes.extend_from_slice(text.as_bytes());
    bytes
}
fn file_content(
    root: LibraryRootId,
    path: &str,
    bytes: &[u8],
    meta: &fs::Metadata,
) -> Result<FileContent, LibraryError> {
    let (text, has_bom, newline) = decode(bytes)?;
    Ok(FileContent {
        root_id: root,
        path: path.into(),
        text: text.into(),
        revision: revision(bytes),
        encoding: TextEncoding::Utf8,
        newline,
        has_bom,
        modified_at: timestamp(meta),
        size: bytes.len() as u64,
    })
}
fn public_root(root: &Root) -> LibraryRoot {
    LibraryRoot {
        root_id: root.spec.root_id,
        label: root.spec.label.clone(),
        host_path: root.canonical.to_string_lossy().into_owned(),
        kind: root.spec.kind,
        writable: root.spec.writable,
        project_id: root.spec.project_id,
        harness: root.spec.harness,
        only: root.spec.only.clone(),
    }
}

impl FsLibrary {
    pub fn new(config: LibraryConfig, clock: Arc<dyn Clock>) -> Result<Self, LibraryError> {
        if config.max_file_bytes == 0
            || config.max_file_bytes > 64 * 1024 * 1024
            || config.search_max_entries == 0
            || !config.data_dir.is_absolute()
        {
            return Err(denied());
        }
        fs::create_dir_all(&config.data_dir)?;
        let data = Root::new(RootSpec {
            root_id: LibraryRootId::new(),
            label: "Library storage".into(),
            path: config.data_dir.clone(),
            kind: RootKind::Notes,
            writable: true,
            project_id: None,
            harness: None,
            only: None,
        })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            path::directory_file(&data.dir)?.set_permissions(fs::Permissions::from_mode(0o700))?;
        }
        data.dir.create_dir_all("trash")?;
        data.dir.create_dir_all("recovery")?;
        let (watch_queue, receiver) = std::sync::mpsc::sync_channel(4096);
        let core = Arc::new(Core {
            config,
            data,
            clock,
            state: Mutex::new(State {
                roots: BTreeMap::new(),
                watcher: None,
            }),
            events: broadcast::channel(1024).0,
            watch_queue,
            rescan_required: Arc::new(AtomicBool::new(false)),
        });
        watch::start(&core, receiver)?;
        Ok(Self { core })
    }

    async fn run<T: Send + 'static>(
        &self,
        f: impl FnOnce(&Core, &mut State) -> Result<T, LibraryError> + Send + 'static,
    ) -> Result<T, LibraryError> {
        let core = self.core.clone();
        tokio::task::spawn_blocking(move || {
            let mut state = lock(&core.state);
            f(&core, &mut state)
        })
        .await
        .map_err(|_| io::Error::other("Library worker stopped"))?
    }

    /// A watcher overflow or native error requires a fresh list/read snapshot.
    pub fn rescan_required(&self) -> bool {
        self.core
            .rescan_required
            .load(std::sync::atomic::Ordering::Acquire)
    }

    /// Existing suggestions only; this never registers roots or reads settings.
    pub async fn suggested_roots(&self, project: &Path) -> Vec<SuggestedRoot> {
        let project = project.to_owned();
        let config = self.core.config.instructions.clone();
        tokio::task::spawn_blocking(move || instructions::suggested_roots(&config, &project))
            .await
            .unwrap_or_default()
    }

    pub async fn trash_entries(&self) -> Result<Vec<TrashEntry>, LibraryError> {
        self.run(|core, state| {
            let dir = core.data.directory("trash")?;
            let mut entries = Vec::new();
            for item in dir.entries()? {
                let item = item?;
                let Some(id) = item.file_name().to_str().map(str::to_owned) else {
                    continue;
                };
                if Uuid::parse_str(&id).is_err() {
                    continue;
                }
                // A folder whose record did not get written, or no longer
                // reads, is passed over rather than hiding the rest.
                let Ok(record) = core.record("trash", &id) else {
                    continue;
                };
                if state
                    .roots
                    .get(&record.root_id)
                    .is_some_and(|r| r.canonical == record.root_path)
                    && dir.is_file(format!("{id}/content"))
                {
                    entries.push(core.trash_entry(&record));
                }
            }
            Ok(entries)
        })
        .await
    }

    /// Purge expired completed trash and sweep confirmed saves. Uncertain saves
    /// remain until explicitly reconciled. The count includes only trash.
    pub async fn purge_trash(&self) -> Result<usize, LibraryError> {
        self.run(|core, state| {
            core.sweep_confirmed(state)?;
            let dir = core.data.directory("trash")?;
            let mut purged = 0;
            for item in dir.entries()? {
                let item = item?;
                let Some(id) = item.file_name().to_str().map(str::to_owned) else {
                    continue;
                };
                if Uuid::parse_str(&id).is_err() {
                    continue;
                }
                let Ok(record) = core.record("trash", &id) else {
                    continue;
                };
                let age = core.clock.now().millis().saturating_sub(record.at.millis());
                if age >= 0
                    && age as u128 >= core.config.retention.as_millis()
                    && dir.is_file(format!("{id}/complete"))
                {
                    dir.remove_dir_all(&id)?;
                    purged += 1;
                }
            }
            sync_dir(&dir)?;
            Ok(purged)
        })
        .await
    }
}

impl Core {
    fn root(state: &State, id: LibraryRootId, write: bool) -> Result<Arc<Root>, LibraryError> {
        let root = state
            .roots
            .get(&id)
            .ok_or(LibraryError::UnknownRoot)?
            .clone();
        root.check()?;
        if write && !root.spec.writable {
            return Err(LibraryError::ReadOnly);
        }
        Ok(root)
    }
    fn read(&self, root: &Root, path: &str) -> Result<FileContent, LibraryError> {
        let (dir, name) = root.parent(path)?;
        let (bytes, metadata) = read_file(&dir, &name, self.config.max_file_bytes)?;
        root.verify_parent(path, &dir)?;
        file_content(root.spec.root_id, path, &bytes, &metadata)
    }
    fn emit(&self, root: LibraryRootId, path: &str, revision: Option<String>) {
        let _ = self.events.send(LibraryChange {
            root_id: root,
            path: path.into(),
            revision,
        });
    }
    fn make_record(&self, root: &Root, path: &str, revision: &str) -> Record {
        Record {
            id: Uuid::new_v4().to_string(),
            root_id: root.spec.root_id,
            root_path: root.canonical.clone(),
            path: path.into(),
            revision: revision.into(),
            at: self.clock.now(),
            temp: None,
            displaced: None,
        }
    }
    fn store_record(&self, kind: &str, record: &Record) -> Result<Dir, LibraryError> {
        let base = self.data.directory(kind)?;
        base.create_dir(&record.id)?;
        let dir = base.open_dir(&record.id)?;
        let bytes = serde_json::to_vec(record).map_err(io::Error::other)?;
        new_file(&dir, "record.json", &bytes, None)?;
        sync_dir(&dir)?;
        sync_dir(&base)?;
        Ok(dir)
    }
    fn record(&self, kind: &str, id: &str) -> Result<Record, LibraryError> {
        if Uuid::parse_str(id).is_err() {
            return Err(LibraryError::NotFound);
        }
        let dir = self.data.directory(&format!("{kind}/{id}"))?;
        let (bytes, _) = read_file(&dir, "record.json", 32 * 1024)?;
        let record: Record = serde_json::from_slice(&bytes)
            .map_err(|_| io::Error::other("invalid Library recovery record"))?;
        if record.id != id || normalize(&record.path)? != record.path {
            return Err(denied());
        }
        Ok(record)
    }
    fn trash_entry(&self, record: &Record) -> TrashEntry {
        TrashEntry {
            trash_id: record.id.clone(),
            root_id: record.root_id,
            original_path: record.path.clone(),
            revision: record.revision.clone(),
            stored_at: self
                .data
                .canonical
                .join("trash")
                .join(&record.id)
                .join("content"),
        }
    }
    fn check_bytes(&self, bytes: &[u8]) -> Result<(), LibraryError> {
        if bytes.len() as u64 > self.config.max_file_bytes {
            return Err(LibraryError::TooLarge(bytes.len() as u64));
        }
        decode(bytes)?;
        Ok(())
    }
}

#[async_trait]
impl LibraryFs for FsLibrary {
    async fn set_roots(&self, roots: Vec<RootSpec>) -> Result<(), LibraryError> {
        self.run(move |core, state| {
            let mut next = BTreeMap::new();
            for spec in roots {
                let root = Arc::new(Root::new(spec)?);
                if root.meets(&core.data.canonical)
                    || next.insert(root.spec.root_id, root).is_some()
                {
                    return Err(denied());
                }
            }
            // A read-only root inside a writable one would be writable through it.
            if next.values().any(|outer| {
                next.values().any(|inner| {
                    inner.spec.root_id != outer.spec.root_id
                        && inner.canonical.starts_with(&outer.canonical)
                        && outer.meets(&inner.canonical)
                        && inner.spec.writable != outer.spec.writable
                })
            }) {
                return Err(denied());
            }
            let watcher = watch::watcher(core, &next)?;
            state.roots = next;
            state.watcher = Some(watcher);
            // Startup recovery waits for configured roots, never trusting paths
            // from old records as authority to reopen an unregistered root.
            core.sweep_confirmed(state)?;
            core.rescan_required
                .store(false, std::sync::atomic::Ordering::Release);
            Ok(())
        })
        .await
    }

    async fn roots(&self) -> Vec<LibraryRoot> {
        lock(&self.core.state)
            .roots
            .values()
            .map(|r| public_root(r))
            .collect()
    }

    async fn list(&self, root: LibraryRootId, dir: &str) -> Result<DirListing, LibraryError> {
        let dir = normalize(dir)?;
        self.run(move |_, state| {
            let root = Core::root(state, root, false)?;
            let handle = root.directory(&dir)?;
            let mut entries = Vec::new();
            for entry in handle.entries()? {
                let entry = entry?;
                // Only names a path can use are listed, as search reads them;
                // internal files are among those refused. One other name must
                // not hide the folder.
                let Ok(name) = entry.file_name().into_string() else {
                    continue;
                };
                if !normalize(&name).is_ok_and(|normal| normal == name) {
                    continue;
                }
                let path = if dir.is_empty() {
                    name.clone()
                } else {
                    format!("{dir}/{name}")
                };
                let metadata = match handle.symlink_metadata(&name) {
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                    metadata => metadata?,
                };
                let kind = if metadata.file_type().is_symlink() {
                    EntryKind::Link
                } else if metadata.is_dir() {
                    EntryKind::Directory
                } else if metadata.is_file() {
                    if name.to_ascii_lowercase().ends_with(".md")
                        || name.to_ascii_lowercase().ends_with(".markdown")
                    {
                        EntryKind::Markdown
                    } else {
                        EntryKind::Other
                    }
                } else {
                    continue;
                };
                // A root limited to some paths lists those and the folders
                // on the way to them, and nothing else.
                let shown = match kind {
                    EntryKind::Directory => root.reaches(&path),
                    _ => root.holds(&path),
                };
                if !shown {
                    continue;
                }
                let mut hidden = name.starts_with('.');
                #[cfg(windows)]
                {
                    use cap_std::fs::MetadataExt;
                    hidden |= metadata.file_attributes() & 2 != 0;
                }
                #[cfg(not(windows))]
                let _ = &mut hidden;
                entries.push(DirEntry {
                    name,
                    hidden,
                    path,
                    kind,
                    size: metadata.len(),
                    modified_at: metadata
                        .modified()
                        .ok()
                        .and_then(|t| t.into_std().duration_since(std::time::UNIX_EPOCH).ok())
                        .and_then(|d| i64::try_from(d.as_millis()).ok())
                        .map(Timestamp),
                    title: None,
                    description: None,
                });
            }
            entries.sort_by(|a, b| {
                (
                    a.kind != EntryKind::Directory,
                    a.name.to_lowercase(),
                    &a.name,
                )
                    .cmp(&(
                        b.kind != EntryKind::Directory,
                        b.name.to_lowercase(),
                        &b.name,
                    ))
            });
            root.check()?;
            Ok(DirListing {
                root_id: root.spec.root_id,
                dir,
                entries,
            })
        })
        .await
    }

    async fn read(&self, root: LibraryRootId, path: &str) -> Result<FileContent, LibraryError> {
        let path = normalize(path)?;
        self.run(move |core, state| core.read(Core::root(state, root, false)?.as_ref(), &path))
            .await
    }

    async fn open(&self, root: LibraryRootId, path: &str) -> Result<std::fs::File, LibraryError> {
        let path = normalize(path)?;
        self.run(move |_, state| {
            let root = Core::root(state, root, false)?;
            let (dir, name) = root.parent(&path)?;
            let file = open_file(&dir, &name)?;
            root.verify_parent(&path, &dir)?;
            Ok(file)
        })
        .await
    }

    async fn save(
        &self,
        root: LibraryRootId,
        path: &str,
        request: SaveRequest,
    ) -> Result<SaveOutcome, LibraryError> {
        let path = normalize(path)?;
        self.run(move |core, state| {
            let root = Core::root(state, root, true)?;
            core.save(&root, &path, request, || Ok(()), || Ok(()))
        })
        .await
    }

    async fn create(
        &self,
        root: LibraryRootId,
        path: &str,
        text: &str,
    ) -> Result<String, LibraryError> {
        let path = normalize(path)?;
        let bytes = text.as_bytes().to_vec();
        self.run(move |core, state| {
            core.check_bytes(&bytes)?;
            let root = Core::root(state, root, true)?;
            let (dir, name) = root.parent(&path)?;
            let temp = format!("{INTERNAL_PREFIX}create-{}", Uuid::new_v4());
            new_file(&dir, &temp, &bytes, None)?;
            let result = root
                .verify_parent(&path, &dir)
                .and_then(|()| path::publish(&dir, &temp, &name));
            if result.is_err() {
                let _ = dir.remove_file(&temp);
            }
            result?;
            let revision = revision(&bytes);
            core.emit(root.spec.root_id, &path, Some(revision.clone()));
            Ok(revision)
        })
        .await
    }

    async fn rename(
        &self,
        root: LibraryRootId,
        from: &str,
        to: &str,
        base_revision: &str,
    ) -> Result<String, LibraryError> {
        let from = normalize(from)?;
        let to = normalize(to)?;
        let base = base_revision.to_owned();
        self.run(move |core, state| {
            let root = Core::root(state, root, true)?;
            let (source, name) = root.parent(&from)?;
            let (dest, dest_name) = root.parent(&to)?;
            let (bytes, _) = read_file(&source, &name, core.config.max_file_bytes)?;
            let rev = revision(&bytes);
            if rev != base {
                return Err(LibraryError::Conflict);
            }
            root.verify_parent(&from, &source)?;
            root.verify_parent(&to, &dest)?;
            // A no-replace rename preserves a writer's inode instead of copying
            // a stale snapshot. The post-move hash detects replacement races.
            move_noreplace(&source, &name, &dest, &dest_name)?;
            sync_dir(&source)
                .and_then(|()| sync_dir(&dest))
                .map_err(|_| LibraryError::OutcomeUnknown)?;
            let (moved, _) = read_file(&dest, &dest_name, core.config.max_file_bytes)
                .map_err(|_| LibraryError::OutcomeUnknown)?;
            core.emit(root.spec.root_id, &from, None);
            core.emit(root.spec.root_id, &to, Some(revision(&moved)));
            if revision(&moved) != base {
                return Err(LibraryError::OutcomeUnknown);
            }
            Ok(rev)
        })
        .await
    }

    async fn trash(
        &self,
        root: LibraryRootId,
        path: &str,
        base_revision: &str,
    ) -> Result<TrashEntry, LibraryError> {
        let path = normalize(path)?;
        let base = base_revision.to_owned();
        self.run(move |core, state| {
            let root = Core::root(state, root, true)?;
            let (parent, name) = root.parent(&path)?;
            let (bytes, _) = read_file(&parent, &name, core.config.max_file_bytes)?;
            if revision(&bytes) != base {
                return Err(LibraryError::Conflict);
            }
            let mut record = core.make_record(&root, &path, &base);
            let stage = format!("{INTERNAL_PREFIX}trash-{}", record.id);
            record.temp = Some(stage.clone());
            let storage = core.store_record("trash", &record)?;
            root.verify_parent(&path, &parent)?;
            move_noreplace(&parent, &name, &parent, &stage)?;
            let finish = (|| {
                sync_dir(&parent)?;
                match move_noreplace(&parent, &stage, &storage, "content") {
                    Ok(()) => {}
                    Err(LibraryError::Io(error))
                        if error.kind() == io::ErrorKind::CrossesDevices =>
                    {
                        let (moved, metadata) =
                            read_file(&parent, &stage, core.config.max_file_bytes)?;
                        new_file(&storage, "content", &moved, Some(&metadata))?;
                        sync_dir(&storage)?;
                        let (confirmed, _) =
                            read_file(&parent, &stage, core.config.max_file_bytes)?;
                        if confirmed != moved || revision(&moved) != base {
                            return Err(LibraryError::Conflict);
                        }
                        parent.remove_file(&stage)?;
                    }
                    Err(error) => return Err(error),
                }
                sync_dir(&storage)?;
                sync_dir(&parent)?;
                let (moved, _) = read_file(&storage, "content", core.config.max_file_bytes)?;
                if revision(&moved) != base {
                    return Err(LibraryError::Conflict);
                }
                new_file(&storage, "complete", b"", None)?;
                sync_dir(&storage)
            })();
            finish.map_err(|_| LibraryError::OutcomeUnknown)?;
            core.emit(root.spec.root_id, &path, None);
            Ok(core.trash_entry(&record))
        })
        .await
    }

    async fn restore(&self, trash_id: &str) -> Result<LibraryChange, LibraryError> {
        let item = self
            .recoverable()
            .await?
            .into_iter()
            .find(|i| i.trash_id == trash_id)
            .ok_or(LibraryError::NotFound)?;
        self.restore_checked(trash_id, &item.revision, None).await
    }

    async fn recoverable(&self) -> Result<Vec<protocol::library::LibraryTrashItem>, LibraryError> {
        self.list_recoverable().await
    }
    async fn list_recovery(
        &self,
    ) -> Result<Vec<protocol::library::LibraryRecoveryItem>, LibraryError> {
        self.recovery_entries().await
    }
    async fn inspect_recovery(
        &self,
        root: LibraryRootId,
        id: &str,
    ) -> Result<protocol::library::LibraryRecovery, LibraryError> {
        self.inspect_snapshot(root, id).await
    }
    async fn discard_recovery_checked(
        &self,
        root: LibraryRootId,
        id: &str,
    ) -> Result<(), LibraryError> {
        self.discard_bound(Some(root), id).await
    }
    async fn purge(&self) -> Result<(), LibraryError> {
        self.purge_trash().await.map(drop)
    }
    async fn read_trash(&self, id: &str) -> Result<FileContent, LibraryError> {
        self.inspect_trash(id).await
    }
    async fn restore_checked(
        &self,
        id: &str,
        revision: &str,
        copy_path: Option<&str>,
    ) -> Result<LibraryChange, LibraryError> {
        self.restore_trash(id, revision, copy_path).await
    }

    async fn search(
        &self,
        root: Option<LibraryRootId>,
        text: &str,
        limit: u32,
    ) -> Result<Vec<SearchHit>, LibraryError> {
        let query = text.to_lowercase();
        self.run(move |core, state| core.search(state, root, &query, limit))
            .await
    }

    async fn instruction_sources(
        &self,
        project_dir: &Path,
        harness: HarnessKind,
        loaded: &[PathBuf],
        project_settings: bool,
    ) -> Vec<InstructionSource> {
        let project = project_dir.to_owned();
        let loaded = loaded.to_vec();
        self.run(move |core, state| {
            Ok(instructions::sources(
                core,
                state,
                &project,
                harness,
                &loaded,
                project_settings,
            ))
        })
        .await
        .unwrap_or_default()
    }

    fn changes(&self) -> broadcast::Receiver<LibraryChange> {
        self.core.events.subscribe()
    }
}

fn move_noreplace(source: &Dir, name: &str, dest: &Dir, target: &str) -> Result<(), LibraryError> {
    #[cfg(target_os = "linux")]
    {
        rustix::fs::renameat_with(
            source,
            name,
            dest,
            target,
            rustix::fs::RenameFlags::NOREPLACE,
        )
        .map_err(std::io::Error::from)
        .map_err(io_error)?;
    }
    #[cfg(not(target_os = "linux"))]
    {
        // Hard-link publication is atomic and fails on any existing destination.
        // A crash before unlink leaves two recoverable names for the same file.
        source.hard_link(name, dest, target).map_err(io_error)?;
        source
            .remove_file(name)
            .map_err(|_| LibraryError::OutcomeUnknown)?;
    }
    Ok(())
}

impl Core {
    fn save(
        &self,
        root: &Root,
        path: &str,
        request: SaveRequest,
        before_commit: impl FnOnce() -> Result<(), LibraryError>,
        at_replace: impl FnOnce() -> Result<(), LibraryError>,
    ) -> Result<SaveOutcome, LibraryError> {
        let current = match self.read(root, path) {
            Ok(file) => file,
            Err(LibraryError::NotFound) => return Ok(SaveOutcome::Missing),
            Err(e) => return Err(e),
        };
        if current.revision != request.base_revision {
            return Ok(SaveOutcome::Conflict {
                current: Some(Box::new(current)),
            });
        }
        let bytes = encode(&request);
        self.check_bytes(&bytes)?;
        let (parent, name) = root.parent(path)?;
        let (base, metadata) = read_file(&parent, &name, self.config.max_file_bytes)?;
        if revision(&base) != request.base_revision {
            return Ok(SaveOutcome::Conflict {
                current: Some(Box::new(file_content(
                    root.spec.root_id,
                    path,
                    &base,
                    &metadata,
                )?)),
            });
        }
        let mut record = self.make_record(root, path, &request.base_revision);
        let temp = save_name(&record.id, "new");
        let backup = save_name(&record.id, "old");
        record.temp = Some(temp.clone());
        record.displaced = Some(backup.clone());
        let storage = self.store_record("recovery", &record)?;
        new_file(&storage, "proposed", &bytes, None)?;
        new_file(&storage, "base", &base, None)?;
        sync_dir(&storage)?;
        new_file(&parent, &temp, &bytes, Some(&metadata))?;
        let preflight = (|| {
            before_commit()?;
            let current = self.read(root, path)?;
            root.verify_parent(path, &parent)?;
            Ok::<_, LibraryError>(current)
        })();
        let current = match preflight {
            Ok(value) => value,
            Err(e) => {
                let _ = parent.remove_file(&temp);
                return Err(e);
            }
        };
        if current.revision != request.base_revision {
            parent.remove_file(&temp)?;
            return Ok(SaveOutcome::Conflict {
                current: Some(Box::new(current)),
            });
        }
        if at_replace().is_err() {
            let _ = parent.remove_file(&temp);
            return Err(io::Error::other("replacement interrupted").into());
        }
        if replace::replace(&parent, &temp, &name, &backup).is_err() {
            return Ok(SaveOutcome::Uncertain {
                recovery_id: record.id,
            });
        }
        let confirmed = (|| {
            sync_dir(&parent)?;
            root.verify_parent(path, &parent)?;
            let (displaced, _) = read_file(&parent, &backup, self.config.max_file_bytes)?;
            let (saved, _) = read_file(&parent, &name, self.config.max_file_bytes)?;
            if revision(&displaced) != request.base_revision || saved != bytes {
                return Err(LibraryError::Conflict);
            }
            Ok::<_, LibraryError>(())
        })();
        if confirmed.is_err() {
            return Ok(SaveOutcome::Uncertain {
                recovery_id: record.id,
            });
        }
        // Only a Saved outcome may have this marker. Failure to record it leaves
        // conservative recovery evidence; it cannot turn this into Uncertain
        // after publishing a marker that permits automatic cleanup.
        if self.mark_confirmed(&storage).is_err() {
            tracing::warn!("Could not persist Library save confirmation; retaining recovery");
        }
        let revision = revision(&bytes);
        self.emit(root.spec.root_id, path, Some(revision.clone()));
        Ok(SaveOutcome::Saved { revision })
    }

    /// Whether a search of every root finds the file at `absolute`, in `root`,
    /// through another root instead: one that reaches all of its folder, or
    /// one limited like `root` but deeper. A file found once is listed once.
    fn found_elsewhere(state: &State, root: &Root, absolute: &Path) -> bool {
        root.spec.only.is_some()
            && state.roots.values().any(|other| {
                other.spec.root_id != root.spec.root_id
                    && (other.spec.only.is_none()
                        || other.canonical.starts_with(&root.canonical)
                            && other.canonical != root.canonical)
                    && absolute
                        .strip_prefix(&other.canonical)
                        .ok()
                        .and_then(|rest| rest.to_str())
                        .is_some_and(|rest| {
                            other.holds(&rest.replace(std::path::MAIN_SEPARATOR, "/"))
                        })
            })
    }

    fn search(
        &self,
        state: &State,
        root_id: Option<LibraryRootId>,
        query: &str,
        limit: u32,
    ) -> Result<Vec<SearchHit>, LibraryError> {
        let mut hits = Vec::new();
        if query.is_empty() || limit == 0 {
            return Ok(hits);
        }
        if let Some(id) = root_id {
            Self::root(state, id, false)?;
        }
        let started = self.clock.monotonic();
        let expired = || {
            self.clock.monotonic().saturating_duration_since(started) >= self.config.search_timeout
        };
        let limit = limit.min(1000) as usize;
        let mut scanned = 0;
        for root in state
            .roots
            .values()
            .filter(|r| root_id.is_none_or(|id| r.spec.root_id == id))
        {
            // A root that has gone away, or a folder below that cannot be
            // read, leaves the rest searchable. A search of that root alone
            // reports it above.
            if root.check().is_err() {
                continue;
            }
            let mut pending = vec![(
                String::new(),
                Vec::<Arc<ignore::gitignore::Gitignore>>::new(),
            )];
            while let Some((relative_dir, mut rules)) = pending.pop() {
                if expired() {
                    return Ok(hits);
                }
                let Ok(dir) = root.directory(&relative_dir) else {
                    continue;
                };
                // A root limited to some paths searches all of them, as
                // the agents given them read them whatever is ignored.
                for ignore_name in [".gitignore", ".ignore"]
                    .into_iter()
                    .filter(|_| root.spec.only.is_none())
                {
                    // Read ignore files through the same no-follow handles as
                    // documents; the walker never reads ambient user config.
                    if let Ok((bytes, _)) = read_file(&dir, ignore_name, self.config.max_file_bytes)
                        && let Ok((text, _, _)) = decode(&bytes)
                    {
                        let mut builder = ignore::gitignore::GitignoreBuilder::new(
                            root.canonical.join(&relative_dir),
                        );
                        for line in text.lines() {
                            let _ = builder.add_line(None, line);
                        }
                        if let Ok(matcher) = builder.build() {
                            rules.push(Arc::new(matcher));
                        }
                    }
                }
                let Ok(listing) = dir.entries() else {
                    continue;
                };
                let mut entries = Vec::new();
                for entry in listing {
                    scanned += 1;
                    if scanned > self.config.search_max_entries || expired() {
                        return Ok(hits);
                    }
                    if let Ok(entry) = entry {
                        entries.push(entry);
                    }
                }
                entries.sort_by_key(|e| e.file_name());
                for entry in entries {
                    // Each file read can take up to its size limit.
                    if expired() {
                        return Ok(hits);
                    }
                    let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                        continue;
                    };
                    if matches!(name.as_str(), ".git" | "node_modules" | "target")
                        || internal_name(&name)
                    {
                        continue;
                    }
                    let path = if relative_dir.is_empty() {
                        name.clone()
                    } else {
                        format!("{relative_dir}/{name}")
                    };
                    let Ok(path) = normalize(&path) else { continue };
                    let Ok(meta) = dir.symlink_metadata(&name) else {
                        continue;
                    };
                    if meta.file_type().is_symlink() {
                        continue;
                    }
                    let absolute = root.canonical.join(&path);
                    let ignored = rules
                        .iter()
                        .rev()
                        .map(|r| r.matched(&absolute, meta.is_dir()))
                        .find(|m| !m.is_none())
                        .is_some_and(|m| m.is_ignore());
                    if ignored {
                        continue;
                    }
                    let reached = if meta.is_dir() {
                        root.reaches(&path)
                    } else {
                        root.holds(&path)
                    };
                    if !reached {
                        continue;
                    }
                    if meta.is_dir() {
                        if path.split('/').count() < 128 {
                            pending.push((path, rules.clone()));
                        }
                        continue;
                    }
                    if !meta.is_file()
                        || meta.len() > self.config.max_file_bytes
                        || (root_id.is_none() && Self::found_elsewhere(state, root, &absolute))
                    {
                        continue;
                    }
                    let Ok(file) = self.read(root, &path) else {
                        continue;
                    };
                    // A file is found by its name, empty or not, before
                    // any lines in it that match.
                    if name.to_lowercase().contains(query) {
                        hits.push(SearchHit {
                            root_id: root.spec.root_id,
                            path: path.clone(),
                            line: None,
                            snippet: String::new(),
                        });
                        if hits.len() >= limit {
                            return Ok(hits);
                        }
                    }
                    for (line, text) in file.text.lines().enumerate() {
                        if text.to_lowercase().contains(query) {
                            hits.push(SearchHit {
                                root_id: root.spec.root_id,
                                path: path.clone(),
                                line: Some(line as u32 + 1),
                                snippet: text.chars().take(300).collect(),
                            });
                            if hits.len() >= limit {
                                return Ok(hits);
                            }
                        }
                    }
                }
            }
        }
        Ok(hits)
    }
}

#[cfg(test)]
mod tests;
