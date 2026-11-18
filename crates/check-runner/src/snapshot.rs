// SPDX-License-Identifier: Apache-2.0
//! Before-and-after records owned by Ophio, not by the project's git history.
//!
//! Filesystem work is synchronous: async callers should use `spawn_blocking`.
//! A successful capture can still contain issues. Such a record never proves
//! that nothing changed: its diff selects checks conservatively.

use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt};
use cap_std::fs::{Dir, Metadata, OpenOptions};
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use ignore::WalkBuilder;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use thiserror::Error;
use uuid::Uuid;

#[cfg(test)]
mod tests;

pub const MAX_FILE_BYTES: u64 = 5 * 1024 * 1024;
pub const MAX_TEXT_BYTES: u64 = 1024 * 1024;
pub const DEFAULT_RETENTION_DAYS: u64 = 14;
const FORCED_SKIP: &[&str] = &[
    "node_modules",
    "target",
    ".venv",
    "build",
    "dist",
    ".gradle",
    ".git",
];

#[derive(Debug, Error)]
pub enum SnapshotError {
    #[error("Could not {action}: {source}")]
    Io {
        action: &'static str,
        #[source]
        source: io::Error,
    },
    #[error("Could not read the saved file record: {0}")]
    Json(#[from] serde_json::Error),
    #[error("The snapshot data folder must be outside the project folder")]
    OverlappingFolders,
    #[error("The two file records belong to different project folders")]
    DifferentProjects,
    #[error("This is not a safe project-relative path: {0}")]
    InvalidPath(String),
    #[error("This file pattern is not valid: {0}")]
    InvalidGlob(String),
    #[error("The saved file record is not valid: {0}")]
    InvalidManifest(String),
    #[error("The saved file content does not match its fingerprint")]
    CorruptBlob,
    #[error("The saved file content is unavailable: {reason}")]
    ContentUnavailable { hash: String, reason: String },
}

fn io_error(action: &'static str, source: io::Error) -> SnapshotError {
    SnapshotError::Io { action, source }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileTime {
    pub seconds: i64,
    pub nanos: u32,
}

impl FileTime {
    fn of(time: SystemTime) -> Self {
        match time.duration_since(UNIX_EPOCH) {
            Ok(delta) => Self {
                seconds: delta.as_secs() as i64,
                nanos: delta.subsec_nanos(),
            },
            Err(error) => {
                let delta = error.duration();
                let nanos = delta.subsec_nanos();
                Self {
                    seconds: -(delta.as_secs() as i64) - i64::from(nanos != 0),
                    nanos: if nanos == 0 { 0 } else { 1_000_000_000 - nanos },
                }
            }
        }
    }
}

/// Additional cache identity stops replacing a file or resetting its mtime
/// from reusing the old content hash on Unix filesystems.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheIdentity {
    pub device: Option<u64>,
    pub inode: Option<u64>,
    pub changed: Option<FileTime>,
    pub created: Option<FileTime>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    File,
    Symlink,
    Fifo,
    Socket,
    BlockDevice,
    CharacterDevice,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileEntry {
    pub path: String,
    pub size: u64,
    /// Unix permission bits, or the portable read-only/writable equivalent.
    pub mode: u32,
    pub hash: Option<String>,
    /// True only when the text itself is saved in the blob store.
    pub blob: bool,
    pub modified: Option<FileTime>,
    pub identity: CacheIdentity,
    pub kind: EntryKind,
    pub unreadable: Option<String>,
    /// Link text only, never the contents of the link's destination.
    pub link_target: Option<String>,
}

impl FileEntry {
    /// These entries are compared by metadata, without opening their contents.
    fn content_omitted(&self) -> bool {
        match self.kind {
            EntryKind::File => self.size > MAX_FILE_BYTES,
            EntryKind::Symlink => false,
            _ => true,
        }
    }

    fn is_unreadable(&self) -> bool {
        self.unreadable.is_some() || (self.hash.is_none() && !self.content_omitted())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureIssue {
    /// None means the walk could not identify which files were affected.
    pub path: Option<String>,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub id: String,
    pub root: PathBuf,
    pub captured_at_secs: u64,
    pub files: BTreeMap<String, FileEntry>,
    pub issues: Vec<CaptureIssue>,
    pub reused_hashes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileChange {
    Added,
    Modified,
    Removed,
    ModeChanged,
    Unreadable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotDiff {
    pub changes: BTreeMap<String, FileChange>,
    /// Incomplete records select all checks, rather than quietly skipping any.
    pub incomplete: bool,
}

impl Manifest {
    pub fn diff(&self, after: &Self) -> Result<SnapshotDiff, SnapshotError> {
        if self.root != after.root {
            return Err(SnapshotError::DifferentProjects);
        }
        let mut changes = BTreeMap::new();
        for (path, before) in &self.files {
            let change = match after.files.get(path) {
                None => Some(FileChange::Removed),
                Some(next) if before.is_unreadable() || next.is_unreadable() => {
                    Some(FileChange::Unreadable)
                }
                Some(next)
                    if before.hash != next.hash
                        || before.kind != next.kind
                        || (matches!(before.kind, EntryKind::File | EntryKind::Symlink)
                            && before.size != next.size)
                        || (before.content_omitted()
                            && (before.identity != next.identity
                                || (before.kind == EntryKind::File
                                    && before.modified != next.modified))) =>
                {
                    Some(FileChange::Modified)
                }
                Some(next) if before.mode != next.mode => Some(FileChange::ModeChanged),
                _ => None,
            };
            if let Some(change) = change {
                changes.insert(path.clone(), change);
            }
        }
        for (path, entry) in &after.files {
            if !self.files.contains_key(path) {
                changes.insert(
                    path.clone(),
                    if entry.is_unreadable() {
                        FileChange::Unreadable
                    } else {
                        FileChange::Added
                    },
                );
            }
        }
        Ok(SnapshotDiff {
            changes,
            incomplete: !self.issues.is_empty() || !after.issues.is_empty(),
        })
    }
}

impl SnapshotDiff {
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty() && !self.incomplete
    }

    pub fn changed_paths(&self) -> impl Iterator<Item = &str> {
        self.changes.keys().map(String::as_str)
    }

    /// Empty patterns mean every change, including uncertain captures.
    pub fn matches_globs(&self, patterns: &[String]) -> Result<bool, SnapshotError> {
        let globs = compile_globs(patterns)?;
        if self.incomplete {
            return Ok(true);
        }
        if patterns.is_empty() {
            return Ok(!self.is_empty());
        }
        Ok(self.changed_paths().any(|path| globs.is_match(path)))
    }

    /// Returns normalized, unique project-relative paths in sorted order.
    pub fn changed_inputs(&self, inputs: &[String]) -> Result<Vec<String>, SnapshotError> {
        let mut changed = BTreeSet::new();
        for input in inputs {
            let normalized = normalize_relative(input)?;
            if self.incomplete || self.changes.contains_key(&normalized) {
                changed.insert(normalized);
            }
        }
        Ok(changed.into_iter().collect())
    }
}

pub fn validate_input(input: &str) -> Result<(), SnapshotError> {
    normalize_relative(input).map(|_| ())
}

pub fn validate_globs(patterns: &[String]) -> Result<(), SnapshotError> {
    compile_globs(patterns).map(|_| ())
}

fn normalize_relative(input: &str) -> Result<String, SnapshotError> {
    let normalized = input.replace('\\', "/");
    if normalized.is_empty() || normalized.starts_with('/') || normalized.contains(['\0', ':']) {
        return Err(SnapshotError::InvalidPath(input.into()));
    }
    let mut parts = Vec::new();
    for part in normalized.split('/') {
        match part {
            ".." => return Err(SnapshotError::InvalidPath(input.into())),
            "." | "" => {}
            _ => parts.push(part),
        }
    }
    if parts.is_empty() {
        return Err(SnapshotError::InvalidPath(input.into()));
    }
    Ok(parts.join("/"))
}

fn compile_globs(patterns: &[String]) -> Result<GlobSet, SnapshotError> {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        let normalized = normalize_relative(pattern)?;
        let glob = GlobBuilder::new(&normalized)
            .literal_separator(true)
            .backslash_escape(false)
            .build()
            .map_err(|_| SnapshotError::InvalidGlob(pattern.clone()))?;
        builder.add(glob);
    }
    builder
        .build()
        .map_err(|error| SnapshotError::InvalidGlob(error.to_string()))
}

/// Pass a dedicated directory under Ophio's data folder. Constructing this
/// handle does not create anything: capture first checks it is external.
#[derive(Debug, Clone)]
pub struct SnapshotStore {
    data: PathBuf,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct GcReport {
    pub manifests_removed: u64,
    pub blobs_removed: u64,
    pub bytes_removed: u64,
}

impl SnapshotStore {
    pub fn new(data_dir: impl AsRef<Path>) -> Result<Self, SnapshotError> {
        Ok(Self {
            data: resolve_missing(data_dir.as_ref())?,
        })
    }

    pub fn data_dir(&self) -> &Path {
        &self.data
    }

    pub fn capture(
        &self,
        project: impl AsRef<Path>,
        previous: Option<&Manifest>,
    ) -> Result<Manifest, SnapshotError> {
        let root = fs::canonicalize(project)
            .map_err(|error| io_error("open the project folder", error))?;
        let data = resolve_missing(&self.data)?;
        if data.starts_with(&root) || root.starts_with(&data) {
            return Err(SnapshotError::OverlappingFolders);
        }
        if let Some(previous) = previous {
            validate_manifest(previous)?;
            if previous.root != root {
                return Err(SnapshotError::DifferentProjects);
            }
        }
        let project_dir = Dir::open_ambient_dir(&root, cap_std::ambient_authority())
            .map_err(|error| io_error("open the project folder", error))?;
        // Root failures are fatal; errors farther down are recorded in issues.
        project_dir
            .entries()
            .map_err(|error| io_error("read the project folder", error))?;
        let _lock = self.lock()?;
        let mut manifest = Manifest {
            id: Uuid::new_v4().to_string(),
            root: root.clone(),
            captured_at_secs: unix_seconds(SystemTime::now()),
            files: BTreeMap::new(),
            issues: Vec::new(),
            reused_hashes: 0,
        };
        let mut touched_blobs = false;
        let mut builder = WalkBuilder::new(&root);
        builder
            .hidden(false)
            .follow_links(false)
            .require_git(false)
            .parents(false)
            .git_global(false)
            .filter_entry(|entry| {
                if entry.depth() == 0 {
                    return true;
                }
                // A worktree's .git is a file, not a directory. Skip both forms,
                // but do not mistake an ordinary source file named "build" for
                // a build-output directory.
                if entry.file_name() == ".git" {
                    return false;
                }
                !entry.file_type().is_some_and(|kind| kind.is_dir())
                    || !FORCED_SKIP.iter().any(|name| entry.file_name() == *name)
            });
        for walked in builder.build() {
            let entry = match walked {
                Ok(entry) => entry,
                Err(error) => {
                    manifest.issues.push(CaptureIssue {
                        path: None,
                        message: error.to_string(),
                    });
                    continue;
                }
            };
            if let Some(error) = entry.error() {
                manifest.issues.push(CaptureIssue {
                    path: relative_name(&root, entry.path()).ok(),
                    message: error.to_string(),
                });
            }
            if entry.depth() == 0 {
                continue;
            }
            let path = match relative_name(&root, entry.path()) {
                Ok(path) => path,
                Err(error) => {
                    manifest.issues.push(CaptureIssue {
                        path: None,
                        message: error.to_string(),
                    });
                    continue;
                }
            };
            let metadata = match project_dir.symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(error) => {
                    let file = unreadable_entry(path.clone(), None, error.to_string());
                    manifest.issues.push(CaptureIssue {
                        path: Some(path.clone()),
                        message: error.to_string(),
                    });
                    manifest.files.insert(path, file);
                    continue;
                }
            };
            if metadata.is_dir() {
                continue;
            }
            let old = previous.and_then(|old| old.files.get(&path));
            match self.capture_file(&project_dir, &path, &metadata, old) {
                Ok((file, reused)) => {
                    touched_blobs |= file.blob && !reused;
                    manifest.reused_hashes += u64::from(reused);
                    manifest.files.insert(path, file);
                }
                Err(error) => {
                    manifest.issues.push(CaptureIssue {
                        path: Some(path.clone()),
                        message: error.to_string(),
                    });
                    manifest.files.insert(
                        path.clone(),
                        unreadable_entry(path, Some(&metadata), error.to_string()),
                    );
                }
            }
        }
        // One directory flush batches the publication of new blob names. It is
        // inexpensive compared with syncing every tiny content file separately.
        // Content is still hash-checked on read; this is not a power-loss promise.
        #[cfg(unix)]
        if touched_blobs
            && let Err(error) =
                fs::File::open(self.data.join("blobs")).and_then(|dir| dir.sync_all())
        {
            manifest.issues.push(CaptureIssue {
                path: None,
                message: format!(
                    "Some saved file content may be unavailable after a restart: {error}"
                ),
            });
        }
        #[cfg(not(unix))]
        let _ = touched_blobs;
        let bytes = serde_json::to_vec(&manifest)?;
        self.write_new(
            &self
                .data
                .join("manifests")
                .join(format!("{}.json", manifest.id)),
            &bytes,
            true,
        )?;
        Ok(manifest)
    }

    pub fn load_manifest(&self, id: &str) -> Result<Manifest, SnapshotError> {
        validate_id(id)?;
        let _lock = self.lock()?;
        self.load_manifest_inner(id)
    }

    pub fn read_blob(&self, hash: &str) -> Result<Vec<u8>, SnapshotError> {
        validate_hash(hash)?;
        let read = || {
            let _lock = self.lock()?;
            let path = self.data.join("blobs").join(hash);
            require_regular(&path)?;
            let bytes = read_bounded(&path, MAX_TEXT_BYTES)?;
            if digest(&bytes) != hash {
                return Err(SnapshotError::CorruptBlob);
            }
            Ok(bytes)
        };
        read().map_err(|error: SnapshotError| SnapshotError::ContentUnavailable {
            hash: hash.into(),
            reason: error.to_string(),
        })
    }

    pub fn gc(&self, retain_days: u64) -> Result<GcReport, SnapshotError> {
        self.gc_at(retain_days, SystemTime::now())
    }

    /// The explicit clock makes retention deterministic in tests and recovery.
    /// Every manifest is validated before any deletion, so a corrupt record
    /// cannot make collection delete content that may still be referenced.
    pub fn gc_at(&self, retain_days: u64, now: SystemTime) -> Result<GcReport, SnapshotError> {
        let _lock = self.lock()?;
        let cutoff = unix_seconds(now).saturating_sub(retain_days.saturating_mul(86_400));
        let mut references = BTreeSet::new();
        let mut expired = Vec::new();
        for entry in fs::read_dir(self.data.join("manifests"))
            .map_err(|error| io_error("list saved file records", error))?
        {
            let entry = entry.map_err(|error| io_error("read a saved file record", error))?;
            let path = entry.path();
            if path.extension().is_none_or(|extension| extension != "json") {
                continue;
            }
            let id = path
                .file_stem()
                .and_then(|name| name.to_str())
                .ok_or_else(|| {
                    SnapshotError::InvalidManifest("The record has no valid identifier".into())
                })?;
            validate_id(id)?;
            let manifest = self.load_manifest_inner(id)?;
            if manifest.captured_at_secs < cutoff {
                expired.push(path);
            } else {
                for file in manifest.files.values().filter(|file| file.blob) {
                    if let Some(hash) = &file.hash {
                        references.insert(hash.clone());
                    }
                }
            }
        }
        // Inspect the entire blob directory before deleting anything as well.
        let mut garbage = Vec::new();
        for entry in fs::read_dir(self.data.join("blobs"))
            .map_err(|error| io_error("list saved file contents", error))?
        {
            let entry =
                entry.map_err(|error| io_error("read a saved file content entry", error))?;
            let name = entry.file_name().to_string_lossy().into_owned();
            // Interrupted atomic writes have no references and can be removed.
            if name.starts_with(".tmp") {
                continue;
            }
            validate_hash(&name)?;
            require_regular(&entry.path())?;
            if !references.contains(&name) {
                garbage.push((
                    entry.path(),
                    entry
                        .metadata()
                        .map_err(|error| io_error("read saved content size", error))?
                        .len(),
                ));
            }
        }
        let mut report = GcReport::default();
        for path in expired {
            fs::remove_file(path).map_err(|error| io_error("remove an old file record", error))?;
            report.manifests_removed += 1;
        }
        for (path, size) in garbage {
            fs::remove_file(path).map_err(|error| io_error("remove old saved content", error))?;
            report.blobs_removed += 1;
            report.bytes_removed += size;
        }
        Ok(report)
    }

    fn capture_file(
        &self,
        root: &Dir,
        path: &str,
        metadata: &Metadata,
        old: Option<&FileEntry>,
    ) -> Result<(FileEntry, bool), SnapshotError> {
        let mut entry = entry_from_metadata(path.into(), metadata);
        if metadata.is_symlink() {
            let target = root
                .read_link_contents(path)
                .map_err(|error| io_error("read a file link", error))?;
            let bytes = target.as_os_str().as_encoded_bytes();
            entry.size = bytes.len() as u64;
            entry.hash = Some(digest(bytes));
            entry.link_target = Some(target.to_string_lossy().into_owned());
            let after = root
                .symlink_metadata(path)
                .map_err(|error| io_error("check a file link", error))?;
            if !same_metadata(metadata, &after) {
                return Err(changed_during_capture());
            }
            return Ok((entry, false));
        }
        if entry.content_omitted() {
            // Large files retain size, mtime and identity; special files retain
            // their concrete type and identity. Neither needs a content read.
            return Ok((entry, false));
        }
        if let Some(old) = old.filter(|old| can_reuse(old, &entry)) {
            let content_exists = !old.blob
                || old.hash.as_ref().is_some_and(|hash| {
                    fs::symlink_metadata(self.data.join("blobs").join(hash))
                        .is_ok_and(|meta| meta.is_file() && meta.len() == old.size)
                });
            if content_exists {
                entry.hash = old.hash.clone();
                entry.blob = old.blob;
                return Ok((entry, true));
            }
        }
        // Capability-relative opening prevents a concurrent symlink replacement
        // from escaping the pinned project directory on any supported platform.
        let mut options = OpenOptions::new();
        options.read(true).follow(FollowSymlinks::No);
        #[cfg(unix)]
        {
            use cap_std::fs::OpenOptionsExt;
            // A regular file replaced by a pipe must not block this capture.
            options.custom_flags(rustix::fs::OFlags::NONBLOCK.bits() as i32);
        }
        let file = root
            .open_with(path, &options)
            .map_err(|error| io_error("read a project file", error))?;
        let opened = file
            .metadata()
            .map_err(|error| io_error("check a project file", error))?;
        if !opened.is_file() || !same_metadata(metadata, &opened) {
            return Err(changed_during_capture());
        }
        let mut bytes = Vec::with_capacity(metadata.len() as usize);
        let mut limited = (&file).take(MAX_FILE_BYTES + 1);
        limited
            .read_to_end(&mut bytes)
            .map_err(|error| io_error("read a project file", error))?;
        let after = file
            .metadata()
            .map_err(|error| io_error("check a project file", error))?;
        let path_after = root
            .symlink_metadata(path)
            .map_err(|error| io_error("check a project file", error))?;
        if bytes.len() as u64 != metadata.len()
            || !same_metadata(metadata, &after)
            || !same_metadata(metadata, &path_after)
        {
            return Err(changed_during_capture());
        }
        let hash = digest(&bytes);
        if bytes.len() as u64 <= MAX_TEXT_BYTES
            && std::str::from_utf8(&bytes).is_ok_and(|text| {
                text.chars().all(|character| {
                    !character.is_control() || matches!(character, '\t' | '\n' | '\r' | '\u{000c}')
                })
            })
        {
            self.save_blob(&hash, &bytes)?;
            entry.blob = true;
        }
        entry.hash = Some(hash);
        Ok((entry, false))
    }

    fn save_blob(&self, hash: &str, bytes: &[u8]) -> Result<(), SnapshotError> {
        let path = self.data.join("blobs").join(hash);
        match fs::symlink_metadata(&path) {
            Ok(metadata) => {
                if !metadata.is_file() {
                    return Err(SnapshotError::CorruptBlob);
                }
                match read_bounded(&path, MAX_TEXT_BYTES) {
                    Ok(saved) if saved == bytes => Ok(()),
                    // Rebuild a damaged cache entry only from bytes just read
                    // and verified in the project, never from the damaged blob.
                    _ => self.write_new(&path, bytes, false),
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                self.write_new(&path, bytes, false)
            }
            Err(error) => Err(io_error("check saved file contents", error)),
        }
    }

    fn load_manifest_inner(&self, id: &str) -> Result<Manifest, SnapshotError> {
        let path = self.data.join("manifests").join(format!("{id}.json"));
        require_regular(&path)?;
        let manifest: Manifest = serde_json::from_reader(
            fs::File::open(path).map_err(|error| io_error("open a saved file record", error))?,
        )?;
        validate_manifest(&manifest)?;
        if manifest.id != id {
            return Err(SnapshotError::InvalidManifest(
                "The record has a different identifier".into(),
            ));
        }
        Ok(manifest)
    }

    fn write_new(&self, path: &Path, bytes: &[u8], sync: bool) -> Result<(), SnapshotError> {
        let parent = path
            .parent()
            .ok_or_else(|| SnapshotError::InvalidPath(path.display().to_string()))?;
        let mut temp = tempfile::NamedTempFile::new_in(parent)
            .map_err(|error| io_error("save a file record", error))?;
        temp.write_all(bytes)
            .map_err(|error| io_error("save a file record", error))?;
        // Sync the completed manifest, not thousands of individual cache blobs.
        // Atomic publication protects against process interruption. After power
        // loss a missing/corrupt blob is detected by read_blob, never returned as
        // valid content; a later capture rebuilds missing blobs from the project.
        if sync {
            temp.as_file()
                .sync_all()
                .map_err(|error| io_error("save a file record", error))?;
        }
        if sync {
            temp.persist_noclobber(path)
                .map_err(|error| io_error("finish saving a file record", error.error))?;
        } else {
            temp.persist(path)
                .map_err(|error| io_error("finish saving file contents", error.error))?;
        }
        Ok(())
    }

    fn lock(&self) -> Result<fs::File, SnapshotError> {
        ensure_directory(&self.data)?;
        let lock_path = self.data.join("snapshot.lock");
        if let Ok(metadata) = fs::symlink_metadata(&lock_path)
            && !metadata.is_file()
        {
            return Err(SnapshotError::InvalidManifest(
                "The file record lock is not a regular file".into(),
            ));
        }
        let lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(lock_path)
            .map_err(|error| io_error("open the file record lock", error))?;
        lock.lock()
            .map_err(|error| io_error("lock the saved file records", error))?;
        ensure_directory(&self.data.join("blobs"))?;
        ensure_directory(&self.data.join("manifests"))?;
        Ok(lock)
    }
}

fn validate_manifest(manifest: &Manifest) -> Result<(), SnapshotError> {
    validate_id(&manifest.id)?;
    if !manifest.root.is_absolute() {
        return Err(SnapshotError::InvalidManifest(
            "The project folder is not absolute".into(),
        ));
    }
    for (path, file) in &manifest.files {
        if normalize_relative(path)? != *path || file.path != *path {
            return Err(SnapshotError::InvalidManifest(
                "A file path does not match its entry".into(),
            ));
        }
        if let Some(hash) = &file.hash {
            validate_hash(hash)?;
        }
        if file.blob
            && (file.hash.is_none()
                || file.size > MAX_TEXT_BYTES
                || file.kind != EntryKind::File
                || file.unreadable.is_some())
        {
            return Err(SnapshotError::InvalidManifest(
                "A saved content reference is not valid".into(),
            ));
        }
    }
    Ok(())
}

fn validate_id(id: &str) -> Result<(), SnapshotError> {
    if Uuid::parse_str(id).is_err() || id.len() != 36 {
        return Err(SnapshotError::InvalidManifest(
            "The record identifier is not valid".into(),
        ));
    }
    Ok(())
}

fn validate_hash(hash: &str) -> Result<(), SnapshotError> {
    if hash.len() != 64
        || !hash
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(SnapshotError::InvalidManifest(
            "The content fingerprint is not valid".into(),
        ));
    }
    Ok(())
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn unix_seconds(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs()
}

fn relative_name(root: &Path, path: &Path) -> Result<String, SnapshotError> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| SnapshotError::InvalidPath(path.display().to_string()))?;
    let mut parts = Vec::new();
    for part in relative.components() {
        match part {
            Component::Normal(name) => {
                let name = name.to_str().ok_or_else(|| {
                    SnapshotError::InvalidPath("A filename is not valid Unicode".into())
                })?;
                // A literal Unix backslash must not be confused with a portable
                // Windows separator in saved records or check input queries.
                if name.contains(['\\', ':']) {
                    return Err(SnapshotError::InvalidPath(name.into()));
                }
                parts.push(name);
            }
            _ => return Err(SnapshotError::InvalidPath(relative.display().to_string())),
        }
    }
    Ok(parts.join("/"))
}

fn entry_from_metadata(path: String, metadata: &Metadata) -> FileEntry {
    FileEntry {
        path,
        size: metadata.len(),
        mode: file_mode(metadata),
        hash: None,
        blob: false,
        modified: metadata
            .modified()
            .ok()
            .map(|time| FileTime::of(time.into_std())),
        identity: cache_identity(metadata),
        kind: entry_kind(metadata),
        unreadable: None,
        link_target: None,
    }
}

fn entry_kind(metadata: &Metadata) -> EntryKind {
    if metadata.is_symlink() {
        return EntryKind::Symlink;
    }
    if metadata.is_file() {
        return EntryKind::File;
    }
    #[cfg(unix)]
    {
        use cap_std::fs::FileTypeExt;
        let kind = metadata.file_type();
        if kind.is_fifo() {
            return EntryKind::Fifo;
        }
        if kind.is_socket() {
            return EntryKind::Socket;
        }
        if kind.is_block_device() {
            return EntryKind::BlockDevice;
        }
        if kind.is_char_device() {
            return EntryKind::CharacterDevice;
        }
    }
    EntryKind::Other
}

fn unreadable_entry(path: String, metadata: Option<&Metadata>, message: String) -> FileEntry {
    let mut entry = match metadata {
        Some(metadata) => entry_from_metadata(path, metadata),
        None => FileEntry {
            path,
            size: 0,
            mode: 0,
            hash: None,
            blob: false,
            modified: None,
            identity: CacheIdentity {
                device: None,
                inode: None,
                changed: None,
                created: None,
            },
            kind: EntryKind::File,
            unreadable: None,
            link_target: None,
        },
    };
    entry.unreadable = Some(message);
    entry
}

fn can_reuse(old: &FileEntry, next: &FileEntry) -> bool {
    old.kind == EntryKind::File
        && next.kind == EntryKind::File
        && old.unreadable.is_none()
        && old.hash.is_some()
        && old.modified.is_some()
        && old.size == next.size
        && old.modified == next.modified
        && old.identity == next.identity
}

fn same_metadata(before: &Metadata, after: &Metadata) -> bool {
    before.is_file() == after.is_file()
        && before.is_symlink() == after.is_symlink()
        && before.len() == after.len()
        && before.modified().ok() == after.modified().ok()
        && file_mode(before) == file_mode(after)
        && cache_identity(before) == cache_identity(after)
}

#[cfg(unix)]
fn file_mode(metadata: &Metadata) -> u32 {
    use cap_std::fs::MetadataExt;
    metadata.mode() & 0o7777
}

#[cfg(not(unix))]
fn file_mode(metadata: &Metadata) -> u32 {
    if metadata.permissions().readonly() {
        0o444
    } else {
        0o666
    }
}

fn cache_identity(metadata: &Metadata) -> CacheIdentity {
    #[cfg(unix)]
    {
        use cap_std::fs::MetadataExt;
        CacheIdentity {
            device: Some(metadata.dev()),
            inode: Some(metadata.ino()),
            changed: Some(FileTime {
                seconds: metadata.ctime(),
                nanos: metadata.ctime_nsec() as u32,
            }),
            created: metadata
                .created()
                .ok()
                .map(|time| FileTime::of(time.into_std())),
        }
    }
    #[cfg(windows)]
    {
        use cap_fs_ext::MetadataExt;
        CacheIdentity {
            device: Some(metadata.dev()),
            inode: Some(metadata.ino()),
            changed: None,
            created: metadata
                .created()
                .ok()
                .map(|time| FileTime::of(time.into_std())),
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        CacheIdentity {
            device: None,
            inode: None,
            changed: None,
            created: metadata
                .created()
                .ok()
                .map(|time| FileTime::of(time.into_std())),
        }
    }
}

fn changed_during_capture() -> SnapshotError {
    io_error(
        "record a file that changed while it was being read",
        io::Error::other("The file changed during capture"),
    )
}

fn ensure_directory(path: &Path) -> Result<(), SnapshotError> {
    fs::create_dir_all(path).map_err(|error| io_error("create the file record folder", error))?;
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| io_error("check the file record folder", error))?;
    if !metadata.is_dir() {
        return Err(SnapshotError::InvalidManifest(
            "A file record folder is not a real directory".into(),
        ));
    }
    Ok(())
}

fn require_regular(path: &Path) -> Result<(), SnapshotError> {
    let metadata =
        fs::symlink_metadata(path).map_err(|error| io_error("open saved file data", error))?;
    if !metadata.is_file() {
        return Err(SnapshotError::InvalidManifest(
            "Saved file data is not a regular file".into(),
        ));
    }
    Ok(())
}

fn read_bounded(path: &Path, limit: u64) -> Result<Vec<u8>, SnapshotError> {
    let file = fs::File::open(path).map_err(|error| io_error("read saved file contents", error))?;
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| io_error("read saved file contents", error))?;
    if bytes.len() as u64 > limit {
        return Err(SnapshotError::CorruptBlob);
    }
    Ok(bytes)
}

/// Resolve an existing ancestor before appending nonexistent components. This
/// detects a data-folder alias into the project without creating that folder.
fn resolve_missing(path: &Path) -> Result<PathBuf, SnapshotError> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| io_error("find the data folder", error))?
            .join(path)
    };
    let mut ancestor = absolute.as_path();
    let mut missing = Vec::new();
    loop {
        match fs::canonicalize(ancestor) {
            Ok(mut resolved) => {
                for part in missing.into_iter().rev() {
                    resolved.push(part);
                }
                // normalize_lexically is not stable; components are enough after
                // the existing ancestor has already resolved all its symlinks.
                let mut clean = PathBuf::new();
                for component in resolved.components() {
                    match component {
                        Component::CurDir => {}
                        Component::ParentDir => {
                            clean.pop();
                        }
                        _ => clean.push(component.as_os_str()),
                    }
                }
                return Ok(clean);
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let name = ancestor
                    .file_name()
                    .ok_or_else(|| io_error("resolve the data folder", error))?;
                missing.push(name.to_os_string());
                ancestor = ancestor
                    .parent()
                    .ok_or_else(|| SnapshotError::InvalidPath(absolute.display().to_string()))?;
            }
            Err(error) => return Err(io_error("resolve the data folder", error)),
        }
    }
}
