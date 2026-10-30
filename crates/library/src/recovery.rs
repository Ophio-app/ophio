// SPDX-License-Identifier: Apache-2.0
//! Owner-visible recovery snapshots. Callers must enforce principal grants.
use crate::{
    Core, FsLibrary, Record, State,
    path::{denied, new_file, read_file, sync_dir},
    revision, save_names,
};
use ports::library::LibraryError;
use protocol::{
    ids::LibraryRootId,
    library::{LibraryRecovery, RecoveryVersion},
    time::Timestamp,
};
use std::path::Path;

pub use protocol::library::LibraryRecoveryItem as RecoveryEntry;

impl Core {
    pub(crate) fn mark_confirmed(&self, storage: &cap_std::fs::Dir) -> Result<(), LibraryError> {
        let bytes = serde_json::to_vec(&self.clock.now()).map_err(std::io::Error::other)?;
        new_file(storage, "confirmed", &bytes, None)?;
        sync_dir(storage)
    }

    fn confirmed_at(&self, storage: &cap_std::fs::Dir) -> Result<Option<Timestamp>, LibraryError> {
        match read_file(storage, "confirmed", 128) {
            Ok((bytes, _)) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|e| std::io::Error::other(e).into()),
            Err(LibraryError::NotFound) => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn grace_elapsed(&self, at: Timestamp) -> bool {
        let age = self.clock.now().millis().saturating_sub(at.millis());
        age >= 0 && age as u128 >= self.config.confirmed_save_grace.as_millis()
    }

    pub(crate) fn sweep_confirmed(&self, state: &State) -> Result<(), LibraryError> {
        let recovery = self.data.directory("recovery")?;
        for entry in recovery.entries()? {
            let entry = entry?;
            let Some(id) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if uuid::Uuid::parse_str(&id).is_err() {
                continue;
            }
            // One inaccessible record/root must not prevent other cleanup.
            if self.sweep_confirmed_record(state, &id).is_err() {
                tracing::warn!("Could not sweep a Library recovery record; retaining evidence");
            }
        }
        Ok(())
    }

    fn sweep_confirmed_record(&self, state: &State, id: &str) -> Result<(), LibraryError> {
        let record = self.record("recovery", id)?;
        let storage = self.data.directory(&format!("recovery/{id}"))?;
        let Some(at) = self.confirmed_at(&storage)? else {
            // Legacy records are indistinguishable from Uncertain saves.
            return Ok(());
        };
        if !self.grace_elapsed(at) || storage.is_file("changed") {
            return Ok(());
        }
        let Some(root) = state.roots.get(&record.root_id) else {
            return Ok(());
        };
        if root.canonical != record.root_path || !root.spec.writable {
            return Ok(());
        }
        let (parent, _) = root.parent(&record.path)?;
        match read_saved(&parent, id, "old", self.config.max_file_bytes) {
            Ok((backup, bytes)) if revision(&bytes) == record.revision => {
                root.verify_parent(&record.path, &parent)?;
                // Resume safely if the process stops between unlink and record
                // deletion. An existing backup is always re-read on restart.
                if !storage.is_file("cleanup") {
                    new_file(&storage, "cleanup", b"", None)?;
                    sync_dir(&storage)?;
                }
                parent.remove_file(&backup)?;
            }
            Err(LibraryError::NotFound) if storage.is_file("cleanup") => {}
            Ok(_) | Err(LibraryError::TooLarge(_) | LibraryError::Conflict) => {
                // A detected late write stays unresolved even if later bytes
                // happen to return to the original base.
                new_file(&storage, "changed", b"", None)?;
                sync_dir(&storage)?;
                return Ok(());
            }
            Err(e) => return Err(e),
        }
        sync_dir(&parent)?;
        // Windows cannot remove a folder while a handle to it is open.
        drop(storage);
        let recovery = self.data.directory("recovery")?;
        recovery.remove_dir_all(id)?;
        sync_dir(&recovery)
    }
}

/// Exact bytes; Debug intentionally omits document contents.
pub struct RecoverySnapshot {
    pub entry: RecoveryEntry,
    pub base: Vec<u8>,
    pub proposed: Vec<u8>,
    /// The displaced inode may continue changing through an external open handle.
    pub displaced: Option<Vec<u8>>,
}
impl std::fmt::Debug for RecoverySnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RecoverySnapshot")
            .field("entry", &self.entry)
            .finish_non_exhaustive()
    }
}
impl From<&Record> for RecoveryEntry {
    fn from(record: &Record) -> Self {
        Self {
            recovery_id: record.id.clone(),
            root_id: record.root_id,
            path: record.path.clone(),
            created_at: record.at,
        }
    }
}
impl FsLibrary {
    pub async fn recovery_entries(&self) -> Result<Vec<RecoveryEntry>, LibraryError> {
        self.run(|core, state| {
            core.sweep_confirmed(state)?;
            let mut entries = Vec::new();
            for item in core.data.directory("recovery")?.entries()? {
                let item = item?;
                let Some(id) = item.file_name().to_str().map(str::to_owned) else {
                    continue;
                };
                if uuid::Uuid::parse_str(&id).is_err() {
                    continue;
                }
                let Ok(record) = core.record("recovery", &id) else {
                    continue;
                };
                let storage = core.data.directory(&format!("recovery/{id}"))?;
                if core
                    .confirmed_at(&storage)?
                    .is_some_and(|at| !core.grace_elapsed(at))
                {
                    continue;
                }
                if state
                    .roots
                    .get(&record.root_id)
                    .is_some_and(|r| r.canonical == record.root_path && r.check().is_ok())
                {
                    entries.push((&record).into());
                }
            }
            entries.sort_by_key(|entry: &RecoveryEntry| std::cmp::Reverse(entry.created_at));
            Ok(entries)
        })
        .await
    }

    pub async fn recovery(&self, recovery_id: &str) -> Result<RecoverySnapshot, LibraryError> {
        let id = recovery_id.to_owned();
        self.run(move |core, state| {
            let record = core.record("recovery", &id)?;
            let root = Core::root(state, record.root_id, false)?;
            if root.canonical != record.root_path {
                return Err(denied());
            }
            let storage = core.data.directory(&format!("recovery/{id}"))?;
            let cap = core.config.max_file_bytes;
            let base = read_file(&storage, "base", cap)?.0;
            let proposed = read_file(&storage, "proposed", cap)?.0;
            let (parent, _) = root.parent(&record.path)?;
            let displaced = match read_saved(&parent, &id, "old", cap) {
                Ok((_, bytes)) => Some(bytes),
                // An exchange may have completed before its second rename.
                Err(LibraryError::NotFound) => match read_saved(&parent, &id, "new", cap) {
                    Ok((_, bytes)) => Some(bytes),
                    Err(LibraryError::NotFound) => None,
                    Err(e) => return Err(e),
                },
                Err(e) => return Err(e),
            };
            Ok(RecoverySnapshot {
                entry: (&record).into(),
                base,
                proposed,
                displaced,
            })
        })
        .await
    }

    /// Explicit acknowledgement after reconciliation; never called by save or
    /// by automatic retention. Deletes only names derived from a validated id.
    pub async fn discard_recovery(&self, recovery_id: &str) -> Result<(), LibraryError> {
        self.discard_bound(None, recovery_id).await
    }

    pub(crate) async fn discard_bound(
        &self,
        expected_root: Option<LibraryRootId>,
        recovery_id: &str,
    ) -> Result<(), LibraryError> {
        let id = recovery_id.to_owned();
        self.run(move |core, state| {
            let record = core.record("recovery", &id)?;
            if expected_root.is_some_and(|id| id != record.root_id) {
                return Err(denied());
            }
            let root = Core::root(state, record.root_id, true)?;
            if root.canonical != record.root_path {
                return Err(denied());
            }
            let parent_path = Path::new(&record.path)
                .parent()
                .and_then(Path::to_str)
                .ok_or_else(denied)?;
            match root.directory(parent_path) {
                Ok(parent) => {
                    root.verify_parent(&record.path, &parent)?;
                    for name in ["old", "new"]
                        .into_iter()
                        .flat_map(|half| save_names(&id, half))
                    {
                        match parent.remove_file(&name) {
                            Ok(()) => {}
                            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                            Err(e) => return Err(e.into()),
                        }
                    }
                    sync_dir(&parent)?;
                }
                Err(LibraryError::NotFound) => {}
                Err(e) => return Err(e),
            }
            let storage = core.data.directory("recovery")?;
            storage.remove_dir_all(&id)?;
            sync_dir(&storage)
        })
        .await
    }
}

impl FsLibrary {
    pub(crate) async fn list_recoverable(
        &self,
    ) -> Result<Vec<protocol::library::LibraryTrashItem>, LibraryError> {
        self.run(|core, state| {
            let dir = core.data.directory("trash")?;
            let mut items = Vec::new();
            for entry in dir.entries()? {
                let entry = entry?;
                let Some(id) = entry.file_name().to_str().map(str::to_owned) else {
                    continue;
                };
                if uuid::Uuid::parse_str(&id).is_err() {
                    continue;
                }
                let Ok(record) = core.record("trash", &id) else {
                    continue;
                };
                if !state
                    .roots
                    .get(&record.root_id)
                    .is_some_and(|r| r.canonical == record.root_path && r.check().is_ok())
                {
                    continue;
                }
                let storage = core.data.directory(&format!("trash/{id}"))?;
                if !storage.is_file("complete") || !storage.is_file("content") {
                    continue;
                }
                // Content grown past the size limit through a handle still
                // open on it cannot be shown or restored here.
                let Ok((bytes, _)) = read_file(&storage, "content", core.config.max_file_bytes)
                else {
                    continue;
                };
                items.push(protocol::library::LibraryTrashItem {
                    trash_id: id,
                    root_id: record.root_id,
                    path: record.path,
                    revision: crate::revision(&bytes),
                    deleted_at: record.at,
                    size: bytes.len() as u64,
                });
            }
            items.sort_by_key(|i| std::cmp::Reverse(i.deleted_at));
            Ok(items)
        })
        .await
    }

    pub(crate) async fn inspect_trash(
        &self,
        id: &str,
    ) -> Result<protocol::library::FileContent, LibraryError> {
        let id = id.to_owned();
        self.run(move |core, state| {
            let record = core.record("trash", &id)?;
            let root = Core::root(state, record.root_id, false)?;
            if root.canonical != record.root_path {
                return Err(denied());
            }
            let storage = core.data.directory(&format!("trash/{id}"))?;
            if !storage.is_file("complete") {
                return Err(LibraryError::OutcomeUnknown);
            }
            let (bytes, metadata) = read_file(&storage, "content", core.config.max_file_bytes)?;
            crate::file_content(record.root_id, &record.path, &bytes, &metadata)
        })
        .await
    }

    pub(crate) async fn restore_trash(
        &self,
        id: &str,
        expected: &str,
        copy_path: Option<&str>,
    ) -> Result<ports::LibraryChange, LibraryError> {
        let id = id.to_owned();
        let expected = expected.to_owned();
        let copy_path = copy_path.map(crate::normalize).transpose()?;
        self.run(move |core, state| {
            let record = core.record("trash", &id)?;
            let root = Core::root(state, record.root_id, true)?;
            if root.canonical != record.root_path {
                return Err(denied());
            }
            let storage = core.data.directory(&format!("trash/{id}"))?;
            if !storage.is_file("complete") {
                return Err(LibraryError::OutcomeUnknown);
            }
            let (bytes, metadata) = read_file(&storage, "content", core.config.max_file_bytes)?;
            if crate::revision(&bytes) != expected {
                return Err(LibraryError::Conflict);
            }
            let destination = copy_path.as_deref().unwrap_or(&record.path);
            let (parent, name) = root.parent(destination)?;
            root.verify_parent(destination, &parent)?;
            let needs_copy = if copy_path.is_none() {
                match crate::move_noreplace(&storage, "content", &parent, &name) {
                    Ok(()) => false,
                    Err(LibraryError::Io(e)) if e.kind() == std::io::ErrorKind::CrossesDevices => {
                        true
                    }
                    Err(e) => return Err(e),
                }
            } else {
                true
            };
            if needs_copy {
                let temp = format!("{}restore-{}", crate::INTERNAL_PREFIX, uuid::Uuid::new_v4());
                crate::path::new_file(&parent, &temp, &bytes, Some(&metadata))?;
                let result = root
                    .verify_parent(destination, &parent)
                    .and_then(|()| crate::path::publish(&parent, &temp, &name));
                if result.is_err() {
                    let _ = parent.remove_file(&temp);
                }
                result?;
            }
            // Windows cannot remove the record's folder below while it is open.
            drop(storage);
            let confirmed = (|| {
                sync_dir(&parent)?;
                root.verify_parent(destination, &parent)?;
                let (current, _) = read_file(&parent, &name, core.config.max_file_bytes)?;
                if crate::revision(&current) != expected {
                    return Err(LibraryError::Conflict);
                }
                // Restore-as-copy retains the original recovery record.
                if copy_path.is_none() {
                    let trash = core.data.directory("trash")?;
                    trash.remove_dir_all(&id)?;
                    sync_dir(&trash)?;
                }
                Ok::<_, LibraryError>(())
            })();
            confirmed.map_err(|_| LibraryError::OutcomeUnknown)?;
            let change = ports::LibraryChange {
                root_id: record.root_id,
                path: destination.into(),
                revision: Some(expected),
            };
            let _ = core.events.send(change.clone());
            Ok(change)
        })
        .await
    }
}

impl FsLibrary {
    pub(crate) async fn inspect_snapshot(
        &self,
        root_id: LibraryRootId,
        recovery_id: &str,
    ) -> Result<LibraryRecovery, LibraryError> {
        let id = recovery_id.to_owned();
        self.run(move |core, state| {
            let record = core.record("recovery", &id)?;
            if record.root_id != root_id {
                return Err(denied());
            }
            let root = Core::root(state, root_id, false)?;
            if root.canonical != record.root_path {
                return Err(denied());
            }
            let storage = core.data.directory(&format!("recovery/{id}"))?;
            // Four versions share a 1 MiB control frame. Reserve room for metadata
            // and the worst-case six-byte JSON escaping of each text byte.
            let cap = core.config.max_file_bytes.min(32 * 1024);
            let base = observe_version(&storage, "base", &record, cap)?;
            let proposed = observe_version(&storage, "proposed", &record, cap)?;
            let (current, displaced) = match root.parent(&record.path) {
                Ok((parent, name)) => {
                    let current = observe_version(&parent, &name, &record, cap)?;
                    let old = observe_saved(&parent, &id, "old", &record, cap)?;
                    let displaced = if matches!(old, RecoveryVersion::Missing) {
                        observe_saved(&parent, &id, "new", &record, cap)?
                    } else {
                        old
                    };
                    root.verify_parent(&record.path, &parent)?;
                    (current, displaced)
                }
                Err(LibraryError::NotFound) => (RecoveryVersion::Missing, RecoveryVersion::Missing),
                Err(e) => return Err(e),
            };
            Ok(LibraryRecovery {
                item: (&record).into(),
                base,
                proposed,
                displaced,
                current,
                observed_at: core.clock.now(),
            })
        })
        .await
    }
}

/// A save's half under the first of its names that exists, with that name.
fn read_saved(
    dir: &cap_std::fs::Dir,
    id: &str,
    half: &str,
    cap: u64,
) -> Result<(String, Vec<u8>), LibraryError> {
    for name in save_names(id, half) {
        match read_file(dir, &name, cap) {
            Ok((bytes, _)) => return Ok((name, bytes)),
            Err(LibraryError::NotFound) => {}
            Err(e) => return Err(e),
        }
    }
    Err(LibraryError::NotFound)
}

fn observe_saved(
    dir: &cap_std::fs::Dir,
    id: &str,
    half: &str,
    record: &Record,
    cap: u64,
) -> Result<RecoveryVersion, LibraryError> {
    for name in save_names(id, half) {
        match observe_version(dir, &name, record, cap)? {
            RecoveryVersion::Missing => {}
            found => return Ok(found),
        }
    }
    Ok(RecoveryVersion::Missing)
}

fn observe_version(
    dir: &cap_std::fs::Dir,
    name: &str,
    record: &Record,
    cap: u64,
) -> Result<RecoveryVersion, LibraryError> {
    match read_file(dir, name, cap) {
        Ok((bytes, metadata)) => {
            match crate::file_content(record.root_id, &record.path, &bytes, &metadata) {
                Ok(file) => Ok(RecoveryVersion::Text { file }),
                Err(LibraryError::NotEditable) => Ok(RecoveryVersion::Unavailable {
                    size: Some(bytes.len() as u64),
                    reason: "Binary or unsupported text encoding; the original bytes are retained."
                        .into(),
                }),
                Err(e) => Err(e),
            }
        }
        Err(LibraryError::TooLarge(size)) => Ok(RecoveryVersion::Unavailable {
            size: Some(size.min(protocol::MAX_SAFE_INTEGER)),
            reason: "Too large to compare here; the original bytes are retained.".into(),
        }),
        Err(LibraryError::NotFound) => Ok(RecoveryVersion::Missing),
        Err(LibraryError::Conflict) => Ok(RecoveryVersion::Unavailable {
            size: None,
            reason: "This version changed while being observed. Refresh to compare again.".into(),
        }),
        Err(LibraryError::Io(_)) => Ok(RecoveryVersion::Unavailable {
            size: None,
            reason: "The computer could not read this version.".into(),
        }),
        Err(e) => Err(e),
    }
}
