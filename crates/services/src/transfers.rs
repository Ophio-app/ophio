// SPDX-License-Identifier: Apache-2.0
mod download_stage;
use crate::{
    CompanionHub,
    api::{Caller, DownloadSource, UploadDone, UploadSink},
    error, internal,
    project_folders::open_project_root,
    unsupported,
};
use async_trait::async_trait;
use cap_std::{
    ambient_authority,
    fs::{Dir, OpenOptions},
};
use download_stage::DownloadStage;
use protocol::{auth::Principal, library::LibraryRoot, model::ArtifactMeta, transfer::*, *};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    path::{Component, Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt},
    sync::{Mutex, OwnedMutexGuard, OwnedRwLockReadGuard},
};

/// A phone continues an interrupted transfer when it next connects. One that
/// has not moved for a day was abandoned; its staged bytes, its blob and its
/// share of the upload allowance would otherwise stay until someone cancels it.
const ABANDONED_AFTER: Duration = Duration::from_secs(24 * 60 * 60);
/// Room kept free for the store and the rest of the computer once every
/// declared upload has arrived.
pub(crate) const FREE_SPACE_RESERVE: u64 = 512 * 1024 * 1024;
/// Longest filename a transfer may carry, in bytes.
const MAX_NAME_BYTES: usize = 200;

#[derive(Debug)]
pub(crate) struct DownloadPrepared {
    name: String,
    size: u64,
    sha256: String,
    required_grant: Option<String>,
    /// Keeps the blob from collection until the transfer that names it is stored.
    _publishing: OwnedRwLockReadGuard<()>,
}
struct Upload {
    hub: CompanionHub,
    caller: Caller,
    id: TransferId,
    file: tokio::fs::File,
    offset: u64,
    last_progress: std::time::Instant,
    guard: Option<OwnedMutexGuard<()>>,
}
impl std::fmt::Debug for Upload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Upload")
            .field("id", &self.id)
            .field("offset", &self.offset)
            .finish_non_exhaustive()
    }
}
impl Drop for Upload {
    fn drop(&mut self) {
        let hub = self.hub.clone();
        let id = self.id;
        let guard = self.guard.take();
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                let _guard = guard;
                if let Ok(record) = hub.read(|r| r.require::<TransferRecord>("transfer", id))
                    && record.state == TransferState::Cancelled
                {
                    let _ = tokio::fs::remove_file(hub.stage_path(id)).await;
                }
            });
        }
    }
}
impl CompanionHub {
    pub(crate) fn artifact_path(&self, sha: &str) -> Result<PathBuf, ProtocolError> {
        valid_hash(sha)?;
        Ok(self.inner.data.join("artifacts").join(&sha[..2]).join(sha))
    }
    fn stage_path(&self, id: TransferId) -> PathBuf {
        self.inner.data.join("staging").join(id.to_string())
    }
    fn transfer_lock(&self, id: TransferId) -> Result<Arc<Mutex<()>>, ProtocolError> {
        // Keep an entry only while someone holds its lock, so ids that were
        // looked up or finished long ago leave nothing behind.
        let mut locks = self.inner.transfer_locks.lock().map_err(internal)?;
        locks.retain(|_, lock| lock.strong_count() > 0);
        if let Some(lock) = locks.get(&id).and_then(std::sync::Weak::upgrade) {
            return Ok(lock);
        }
        let lock = Arc::new(Mutex::new(()));
        locks.insert(id, Arc::downgrade(&lock));
        Ok(lock)
    }
    pub(crate) fn declare_upload(
        &self,
        tx: &mut store::Write<'_>,
        name: &str,
        size: u64,
        sha: &str,
        destination: &TransferDestination,
        owner: Principal,
    ) -> store::Result<TransferRecord> {
        valid_name(name)?;
        valid_hash(sha)?;
        if size > self.inner.max_upload {
            return Err(error(
                ErrorCode::LimitExceeded,
                "Upload exceeds the configured size limit",
            )
            .into());
        }
        let folder = match destination {
            TransferDestination::Artifact => None,
            TransferDestination::Inbox => Some(self.inbox_folder()?),
            TransferDestination::ProjectDir { project_id, dir } => {
                relative(dir, true)?;
                let project: protocol::model::Project = tx.read().require("project", project_id)?;
                project_folder(&project, dir)?;
                Some(PathBuf::from(project.host_path).join(dir))
            }
        };
        if let Some(folder) = &folder {
            let roots: Vec<LibraryRoot> =
                tx.read().get("meta", "library_roots")?.unwrap_or_default();
            outside_read_only_roots(&roots, folder)?;
        }
        self.upload_room(&tx.read(), size, owner, folder.as_deref())?;
        let now = self.inner.clock.now();
        let record = TransferRecord {
            transfer_id: TransferId::new(),
            shown_in_task: None,
            direction: TransferDirection::PhoneToHost,
            name: name.into(),
            size,
            sha256: Some(sha.into()),
            transferred: 0,
            state: TransferState::Pending,
            destination: Some(destination.clone()),
            destination_folder: if matches!(destination, TransferDestination::Inbox) {
                Some(self.inbox_folder()?.to_string_lossy().into_owned())
            } else {
                None
            },
            publication: None,
            host_path: None,
            created_at: now,
            updated_at: now,
        };
        tx.put("transfer", record.transfer_id, &record)?;
        tx.put("transfer_owner", record.transfer_id, &owner)?;
        tx.emit(Event::TransferUpserted {
            transfer: record.clone(),
        })?;
        Ok(record)
    }
    /// Uploads a device declared but has not finished hold space on the
    /// computer until they finish, are cancelled or are abandoned. Each device
    /// may hold two files at the size limit, all devices together four, and
    /// never more than the data volume can take. One sent to a folder also
    /// holds room there for its copy until the copy claims it.
    fn upload_room(
        &self,
        r: &store::Read<'_>,
        size: u64,
        owner: Principal,
        folder: Option<&Path>,
    ) -> Result<(), ProtocolError> {
        let (mut device, mut everyone, mut arriving) = (0u64, 0u64, 0u64);
        let mut promised = HashMap::<u64, u64>::new();
        let mut copies =
            (self.inner.copies.lock()).unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut unfinished = HashSet::new();
        for t in r
            .list::<TransferRecord>("transfer")
            .map_err(crate::stored)?
        {
            if t.direction != TransferDirection::PhoneToHost
                || matches!(t.state, TransferState::Completed | TransferState::Cancelled)
            {
                continue;
            }
            unfinished.insert(t.transfer_id);
            if !copies.contains(&t.transfer_id)
                && let Some(volume) = self.copy_to_come(r, &t)
            {
                let room = promised.entry(volume).or_default();
                *room = room.saturating_add(t.size);
            }
            everyone = everyone.saturating_add(t.size);
            arriving = arriving.saturating_add(t.size.saturating_sub(t.transferred));
            if r.get::<Principal>("transfer_owner", t.transfer_id)
                .map_err(crate::stored)?
                == Some(owner)
            {
                device = device.saturating_add(t.size);
            }
        }
        copies.retain(|id| unfinished.contains(id));
        drop(copies);
        let promised = |volume| promised.get(&volume).copied().unwrap_or(0);
        let limit = self.inner.max_upload;
        if device.saturating_add(size) > limit.saturating_mul(2) {
            return Err(error(
                ErrorCode::LimitExceeded,
                format!(
                    "This device already has {} of files on their way to the computer. Let them finish or cancel some, then send this file again.",
                    amount(device)
                ),
            ));
        }
        if everyone.saturating_add(size) > limit.saturating_mul(4) {
            return Err(error(
                ErrorCode::LimitExceeded,
                format!(
                    "Your devices already have {} of files on their way to the computer. Let them finish or cancel some, then send this file again.",
                    amount(everyone)
                ),
            ));
        }
        // Copies still being written, or still to come, take room their
        // volume does not show yet.
        let data = volume(&self.inner.data)?;
        let needed = arriving
            .saturating_add(size)
            .saturating_add(self.claimed(data))
            .saturating_add(promised(data))
            .saturating_add(FREE_SPACE_RESERVE);
        // A file sent to a folder is published by copying it there, so the
        // person is told now, not after every byte arrived, when the copy
        // cannot fit. A folder that does not exist yet will be on its
        // nearest existing parent's volume.
        let Some(folder) = folder.and_then(|f| f.ancestors().find(|a| a.exists())) else {
            return room(&self.inner.data, needed);
        };
        let target = volume(folder)?;
        if target == data {
            return room(&self.inner.data, needed.saturating_add(size));
        }
        room(&self.inner.data, needed)?;
        room(
            folder,
            size.saturating_add(self.claimed(target))
                .saturating_add(promised(target))
                .saturating_add(FREE_SPACE_RESERVE),
        )
    }
    /// The volume an unfinished upload will still be copied to when it has
    /// arrived, if it is sent to a folder and can still arrive.
    fn copy_to_come(&self, r: &store::Read<'_>, t: &TransferRecord) -> Option<u64> {
        if !matches!(
            t.state,
            TransferState::Pending
                | TransferState::InProgress
                | TransferState::Paused
                | TransferState::Verifying
        ) {
            return None;
        }
        let folder = match t.destination.as_ref()? {
            TransferDestination::Artifact => return None,
            TransferDestination::Inbox => (t.destination_folder.as_ref())
                .map_or_else(|| self.inner.data.join("inbox"), PathBuf::from),
            TransferDestination::ProjectDir { project_id, dir } => {
                let project: protocol::model::Project =
                    r.get("project", project_id).ok().flatten()?;
                PathBuf::from(project.host_path).join(dir)
            }
        };
        volume(folder.ancestors().find(|a| a.exists())?).ok()
    }
    /// Room copies to `volume` have claimed and not yet written.
    fn claimed(&self, volume: u64) -> u64 {
        let copying =
            (self.inner.copying.lock()).unwrap_or_else(std::sync::PoisonError::into_inner);
        copying.get(&volume).copied().unwrap_or(0)
    }
    /// Publishing to a folder copies the staged file there, so the file takes
    /// its room twice until the staged copy is removed. The copy needs that
    /// room on the folder's volume, beside what other copies there have
    /// claimed and the reserve, and on the data volume beside what uploads
    /// still have to send, so publishing cannot fill the disk either. The
    /// room is claimed for `transfer` until the claim is dropped, once the
    /// copy is written; from then its upload no longer holds room for a copy
    /// to come.
    pub(crate) fn copy_room(
        &self,
        folder: &Path,
        size: u64,
        transfer: TransferId,
    ) -> Result<Claim<'_>, ProtocolError> {
        let data = volume(&self.inner.data)?;
        let volume = volume(folder)?;
        // Read, checked and claimed under the database lock, which a
        // declaration holds while it checks, so an upload declared meanwhile
        // is counted here or sees this claim.
        let mut db = self.inner.db.lock().map_err(internal)?;
        let arriving = if volume == data {
            db.read(|r| {
                Ok(r.list::<TransferRecord>("transfer")?
                    .into_iter()
                    .filter(|t| {
                        t.direction == TransferDirection::PhoneToHost
                            && !matches!(
                                t.state,
                                TransferState::Completed | TransferState::Cancelled
                            )
                    })
                    .fold(0u64, |arriving, t| {
                        arriving.saturating_add(t.size.saturating_sub(t.transferred))
                    }))
            })
            .map_err(crate::stored)?
        } else {
            0
        };
        #[cfg(test)]
        CLAIMING.with_borrow(|claiming| claiming.as_ref().map(|c| c.send(())));
        let mut copying =
            (self.inner.copying.lock()).unwrap_or_else(std::sync::PoisonError::into_inner);
        let claimed = copying.get(&volume).copied().unwrap_or(0);
        room(
            folder,
            arriving
                .saturating_add(claimed)
                .saturating_add(size)
                .saturating_add(FREE_SPACE_RESERVE),
        )?;
        *copying.entry(volume).or_default() += size;
        drop(copying);
        (self.inner.copies.lock())
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(transfer);
        Ok(Claim {
            hub: self,
            transfer,
            volume,
            size,
            written: false,
        })
    }
    pub(crate) fn declare_download(
        &self,
        tx: &mut store::Write<'_>,
        prepared: DownloadPrepared,
        shown_in_task: Option<bool>,
        owner: Principal,
    ) -> store::Result<TransferRecord> {
        let now = self.inner.clock.now();
        let record = TransferRecord {
            transfer_id: TransferId::new(),
            shown_in_task,
            direction: TransferDirection::HostToPhone,
            name: prepared.name,
            size: prepared.size,
            sha256: Some(prepared.sha256),
            transferred: 0,
            state: TransferState::Pending,
            destination: None,
            destination_folder: None,
            publication: None,
            host_path: None,
            created_at: now,
            updated_at: now,
        };
        tx.put("transfer", record.transfer_id, &record)?;
        tx.put("transfer_owner", record.transfer_id, &owner)?;
        tx.put(
            "download_grant",
            record.transfer_id,
            &prepared.required_grant,
        )?;
        tx.emit(Event::TransferUpserted {
            transfer: record.clone(),
        })?;
        Ok(record)
    }
    pub(crate) async fn prepare_download(
        &self,
        caller: &Caller,
        source: &protocol::command::DownloadSource,
    ) -> Result<DownloadPrepared, ProtocolError> {
        self.authorize(caller, |g| g.transfers)?;
        use protocol::command::DownloadSource::*;
        let mut required_grant = match source {
            LibraryFile { .. } => Some("library_read".into()),
            ProjectFile { .. } | ChecklistFile { .. } => Some("tasks".into()),
            Artifact { .. } | AppUpdate { .. } => None,
        };
        let (name, mut reader): (String, Box<dyn tokio::io::AsyncRead + Send + Unpin>) =
            match source {
                Artifact { artifact_id } => {
                    let meta = self.read(|r| crate::expiry::require_artifact(r, *artifact_id))?;
                    required_grant = self.artifact_grant(caller, &meta)?;
                    let file = tokio::fs::File::open(self.artifact_path(&meta.sha256)?)
                        .await
                        .map_err(internal)?;
                    (
                        meta.label.unwrap_or_else(|| "Attachment".into()),
                        Box::new(file),
                    )
                }
                ProjectFile { project_id, path } => {
                    self.authorize(caller, |g| g.tasks)?;
                    relative(path, false)?;
                    let project: protocol::model::Project =
                        self.read(|r| r.require("project", project_id))?;
                    let file = open_project_file(&project, path)?;
                    (
                        Path::new(path)
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .into_owned(),
                        Box::new(tokio::fs::File::from_std(file)),
                    )
                }
                ChecklistFile { project_id, sha256 } => {
                    self.authorize(caller, |g| g.tasks)?;
                    let gone = || {
                        error(
                            ErrorCode::NotFound,
                            "That file is no longer on the checklist",
                        )
                    };
                    let file = self
                        .read(|r| crate::checklist::file(r, *project_id, sha256))?
                        .ok_or_else(gone)?;
                    let blob = tokio::fs::File::open(self.artifact_path(&file.sha256)?)
                        .await
                        .map_err(|_| gone())?;
                    (file.name, Box::new(blob))
                }
                AppUpdate { sha256 } => {
                    let gone = || {
                        error(
                            ErrorCode::NotFound,
                            "The computer no longer offers that app. Check for an update again.",
                        )
                    };
                    let (_, path) = self
                        .inner
                        .app_updates
                        .iter()
                        .find(|(offer, _)| offer.sha256 == *sha256)
                        .ok_or_else(gone)?;
                    let file = tokio::fs::File::open(path).await.map_err(|_| gone())?;
                    ("Ophio.apk".into(), Box::new(file))
                }
                LibraryFile { root_id, path } => {
                    self.authorize(caller, |g| g.library_read)?;
                    relative(path, false)?;
                    let file = self
                        .inner
                        .library
                        .as_ref()
                        .ok_or_else(|| unsupported("Library"))?
                        .open(*root_id, path)
                        .await
                        .map_err(|e| error(e.code(), "Library download failed"))?;
                    (
                        Path::new(path)
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .into_owned(),
                        Box::new(tokio::fs::File::from_std(file)),
                    )
                }
            };
        let mut stage = DownloadStage::create(self.stage_path(TransferId::new())).await?;
        let mut size = 0;
        let mut hash = Sha256::new();
        let mut buffer = vec![0; 256 * 1024];
        loop {
            let n = reader.read(&mut buffer).await.map_err(internal)?;
            if n == 0 {
                break;
            }
            size += n as u64;
            if size > self.inner.max_upload {
                return Err(error(
                    ErrorCode::LimitExceeded,
                    "Download exceeds the configured limit",
                ));
            }
            hash.update(&buffer[..n]);
            (stage, buffer) = stage.write(buffer, n).await?;
        }
        let sha256 = hex(&hash.finalize());
        // An update replaced the app since this computer offered it.
        if let AppUpdate { sha256: offered } = source
            && *offered != sha256
        {
            return Err(error(
                ErrorCode::Conflict,
                "The app on the computer changed. Check for an update again.",
            ));
        }
        let publishing = self.inner.blobs.clone().read_owned().await;
        let publishing = stage
            .publish(self.artifact_path(&sha256)?, publishing)
            .await?;
        Ok(DownloadPrepared {
            name: download_name(&name),
            size,
            sha256,
            required_grant,
            _publishing: publishing,
        })
    }
    /// Transfer ids are no capability. A transfer, its name and its bytes are
    /// for the device that declared it, or a manager, who may fetch any
    /// artifact and still needs each file's own grant.
    pub(crate) fn sees_transfer(
        r: &store::Read<'_>,
        caller: &Caller,
        id: TransferId,
    ) -> store::Result<bool> {
        Ok(caller.grants.admin
            || r.get::<Principal>("transfer_owner", id)? == Some(caller.principal))
    }
    fn transfer_owner(&self, caller: &Caller, id: TransferId) -> Result<(), ProtocolError> {
        if !self.read(|r| Self::sees_transfer(r, caller, id))? {
            return Err(error(
                ErrorCode::PermissionDenied,
                "Another device started this transfer. Start it again from this device.",
            ));
        }
        Ok(())
    }
    /// Artifacts hold screen captures, agent observations and retained
    /// dictation. `transfers` moves bytes; it is not permission to see them.
    /// A caller sees its own, a task's with `tasks`, or any as a manager.
    pub(crate) fn sees_artifact(
        r: &store::Read<'_>,
        caller: &Caller,
        meta: &ArtifactMeta,
    ) -> store::Result<bool> {
        Ok(Self::owns_artifact(r, caller, meta)? || (caller.grants.tasks && task_artifact(meta)))
    }
    /// Forgets an artifact the caller can see, as Recent pictures' Delete
    /// does. Its bytes go once nothing else names them (`collect_blobs`).
    /// What a person sent stays with their message, so a sent or waiting
    /// prompt's picture is refused rather than broken.
    pub(crate) fn delete_artifact(
        tx: &mut store::Write<'_>,
        caller: &Caller,
        artifact_id: ArtifactId,
    ) -> store::Result<()> {
        let meta: ArtifactMeta = tx.read().require("artifact", artifact_id)?;
        if !Self::sees_artifact(&tx.read(), caller, &meta)? {
            return Err(error(
                ErrorCode::PermissionDenied,
                "This file belongs to another device or to a task this device can’t see",
            )
            .into());
        }
        if sent(&tx.read(), &meta)? {
            return Err(error(
                ErrorCode::Conflict,
                "This picture is part of a message to an agent, so it stays with that message",
            )
            .into());
        }
        tx.delete("artifact", artifact_id)?;
        tx.delete("artifact_creator", artifact_id)?;
        tx.delete("artifact_provenance", artifact_id)?;
        tx.emit(Event::ArtifactDeleted { artifact_id })?;
        Ok(())
    }
    fn owns_artifact(
        r: &store::Read<'_>,
        caller: &Caller,
        meta: &ArtifactMeta,
    ) -> store::Result<bool> {
        Ok(caller.grants.admin
            || r.get::<Principal>("artifact_creator", meta.artifact_id)? == Some(caller.principal))
    }
    fn artifact_grant(
        &self,
        caller: &Caller,
        meta: &ArtifactMeta,
    ) -> Result<Option<String>, ProtocolError> {
        let caller = self.authorize(caller, |_| true)?;
        if self.read(|r| Self::owns_artifact(r, &caller, meta))? {
            return Ok(None);
        }
        if caller.grants.tasks && task_artifact(meta) {
            return Ok(Some("tasks".into()));
        }
        Err(error(
            ErrorCode::PermissionDenied,
            "This file belongs to another device or to a task this device can’t see",
        ))
    }
    async fn publish_blob(&self, stage: &Path, sha: &str) -> Result<(), ProtocolError> {
        let path = self.artifact_path(sha)?;
        tokio::fs::create_dir_all(path.parent().ok_or_else(|| internal("artifact"))?)
            .await
            .map_err(internal)?;
        match tokio::fs::hard_link(stage, &path).await {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(internal(e)),
        }
        sync_directory(path.parent().ok_or_else(|| internal("artifact"))?).await
    }
    pub(crate) async fn put_artifact(
        &self,
        bytes: &[u8],
        media_type: &str,
        task_id: Option<TaskId>,
        created_by: Principal,
    ) -> Result<ArtifactMeta, ProtocolError> {
        self.put_artifact_with_links(
            bytes,
            media_type,
            task_id.map(|task_id| protocol::model::ArtifactLink {
                task_id,
                run_id: None,
                annotation: None,
                role: protocol::model::ArtifactRole::Observation,
            }),
            created_by,
            None,
        )
        .await
    }
    pub(crate) async fn put_artifact_with_links(
        &self,
        bytes: &[u8],
        media_type: &str,
        link: Option<protocol::model::ArtifactLink>,
        created_by: Principal,
        capture: Option<Box<protocol::model::CaptureGeometry>>,
    ) -> Result<ArtifactMeta, ProtocolError> {
        self.store_artifact(bytes, media_type, link, created_by, capture, None)
            .await
    }
    /// `label` is the name the file is saved by on the phone.
    pub(crate) async fn store_artifact(
        &self,
        bytes: &[u8],
        media_type: &str,
        link: Option<protocol::model::ArtifactLink>,
        created_by: Principal,
        capture: Option<Box<protocol::model::CaptureGeometry>>,
        label: Option<String>,
    ) -> Result<ArtifactMeta, ProtocolError> {
        if bytes.len() as u64 > self.inner.max_upload {
            return Err(error(ErrorCode::LimitExceeded, "Artifact too large"));
        }
        let sha = hex(&Sha256::digest(bytes));
        let _publishing = self.inner.blobs.read().await;
        self.put_blob(bytes, &sha).await?;
        let meta = ArtifactMeta {
            artifact_id: ArtifactId::new(),
            media_type: media_type.into(),
            size: bytes.len() as u64,
            sha256: sha,
            created_at: self.inner.clock.now(),
            label,
            task_id: link.as_ref().map(|l| l.task_id),
            links: link.into_iter().collect(),
            capture,
        };
        self.write(|tx| {
            tx.put("artifact", meta.artifact_id, &meta)?;
            tx.put("artifact_creator", meta.artifact_id, &created_by)?;
            tx.put(
                "artifact_provenance",
                meta.artifact_id,
                &protocol::model::Provenance::Observed,
            )?;
            tx.emit(Event::ArtifactCreated {
                artifact: meta.clone(),
            })?;
            Ok(())
        })?;
        Ok(meta)
    }
    /// Keeps `bytes` as the blob `sha`. The caller holds `blobs` for reading
    /// until a record names it, so collection can't take it in between.
    pub(crate) async fn put_blob(&self, bytes: &[u8], sha: &str) -> Result<(), ProtocolError> {
        let stage = self.stage_path(TransferId::new());
        tokio::fs::create_dir_all(stage.parent().ok_or_else(|| internal("stage"))?)
            .await
            .map_err(internal)?;
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&stage)
            .await
            .map_err(internal)?;
        file.write_all(bytes).await.map_err(internal)?;
        file.sync_all().await.map_err(internal)?;
        drop(file);
        self.publish_blob(&stage, sha).await?;
        tokio::fs::remove_file(stage).await.map_err(internal)?;
        Ok(())
    }
    pub(crate) async fn begin_upload(
        &self,
        caller: &Caller,
        id: TransferId,
    ) -> Result<Box<dyn UploadSink>, ProtocolError> {
        let caller = self.authorize(caller, |g| g.transfers)?;
        self.transfer_owner(&caller, id)?;
        let guard = self.transfer_lock(id)?.try_lock_owned().map_err(|_| {
            error(
                ErrorCode::Conflict,
                "Transfer already has an attached writer",
            )
        })?;
        let record: TransferRecord = self.read(|r| r.require("transfer", id))?;
        if matches!(record.state, TransferState::ReconciliationRequired { .. })
            || (record.state != TransferState::Cancelled
                && self.read(|r| Ok(r.get::<bool>("publication_started", id)?.unwrap_or(false)))?)
        {
            return Err(error(
                ErrorCode::OutcomeUnknown,
                "Upload publication was interrupted; inspect the destination before cancelling or creating a new transfer",
            ));
        }

        if record.direction != TransferDirection::PhoneToHost
            || !matches!(
                record.state,
                TransferState::Pending | TransferState::Paused | TransferState::InProgress
            )
        {
            return Err(error(ErrorCode::Conflict, "Transfer cannot accept bytes"));
        }
        if matches!(
            record.destination,
            Some(TransferDestination::ProjectDir { .. })
        ) {
            self.authorize(&caller, |g| g.tasks)?;
        }
        let path = self.stage_path(id);
        tokio::fs::create_dir_all(path.parent().ok_or_else(|| internal("stage"))?)
            .await
            .map_err(internal)?;
        let file = tokio::fs::OpenOptions::new()
            .read(true)
            .append(true)
            .create(true)
            .open(path)
            .await
            .map_err(internal)?;
        let offset = file.metadata().await.map_err(internal)?.len();
        if offset > record.size {
            return Err(error(
                ErrorCode::Conflict,
                "Staged bytes exceed the declaration",
            ));
        }
        self.transfer_progress(id, offset, TransferState::InProgress)?;
        Ok(Box::new(Upload {
            hub: self.clone(),
            caller,
            id,
            file,
            offset,
            last_progress: self.inner.clock.monotonic(),
            guard: Some(guard),
        }))
    }
    fn transfer_progress(
        &self,
        id: TransferId,
        offset: u64,
        state: TransferState,
    ) -> Result<(), ProtocolError> {
        self.write(|tx| {
            let mut t: TransferRecord = tx.read().require("transfer", id)?;
            if t.state == TransferState::Cancelled {
                return Err(error(ErrorCode::Conflict, "Transfer cancelled").into());
            }
            t.transferred = offset;
            t.state = state;
            t.updated_at = self.inner.clock.now();
            tx.put("transfer", id, &t)?;
            tx.emit(Event::TransferUpserted { transfer: t })?;
            Ok(())
        })
    }
    pub(crate) async fn begin_download(
        &self,
        caller: &Caller,
        id: TransferId,
        offset: u64,
    ) -> Result<DownloadSource, ProtocolError> {
        let caller = self.authorize(caller, |g| g.transfers)?;
        self.transfer_owner(&caller, id)?;
        let required: Option<String> =
            self.read(|r| Ok(r.get("download_grant", id)?.unwrap_or(None)))?;
        self.authorize(&caller, |g| match required.as_deref() {
            Some("library_read") => g.library_read,
            Some("tasks") => g.tasks,
            _ => true,
        })?;
        let t: TransferRecord = self.read(|r| r.require("transfer", id))?;
        if t.direction != TransferDirection::HostToPhone
            || matches!(
                t.state,
                TransferState::Cancelled | TransferState::Failed { .. }
            )
            || offset > t.size
        {
            return Err(error(
                ErrorCode::InvalidRequest,
                "Invalid download or offset",
            ));
        }
        let sha256 = t.sha256.ok_or_else(|| internal("missing digest"))?;
        let mut file = match tokio::fs::File::open(self.artifact_path(&sha256)?).await {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(error(
                    ErrorCode::Expired,
                    "This download is no longer on the computer. Start it again.",
                ));
            }
            Err(e) => return Err(internal(e)),
        };
        file.seek(std::io::SeekFrom::Start(offset))
            .await
            .map_err(internal)?;
        // With nothing left to send, the reader never reaches the end that
        // records completion; the download is complete as it starts.
        if offset == t.size {
            self.transfer_progress(id, offset, TransferState::Completed)?;
        }
        Ok(DownloadSource {
            offset,
            size: t.size,
            sha256,
            reader: Box::new(GuardedDownload {
                file,
                hub: self.clone(),
                caller,
                id,
                offset,
                size: t.size,
                required_grant: required,
                last_progress: self.inner.clock.monotonic(),
            }),
        })
    }
    pub(crate) async fn cancel_upload(
        &self,
        caller: &Caller,
        id: TransferId,
    ) -> Result<(), ProtocolError> {
        let caller = self.authorize(caller, |g| g.transfers)?;
        let _: TransferRecord = self.read(|r| r.require("transfer", id))?;
        self.transfer_owner(&caller, id)?;
        // Serialize cancellation with publication. A live writer observes the
        // cancellation on its next write and drops its attachment.
        let lock = self.transfer_lock(id)?;
        let guard = lock.try_lock_owned().ok();
        self.write(|tx| {
            let mut t: TransferRecord = tx.read().require("transfer", id)?;
            if t.state == TransferState::Completed || t.state == TransferState::Verifying {
                return Err(error(
                    ErrorCode::AlreadyResolved,
                    "Transfer is already publishing or completed",
                )
                .into());
            }
            t.state = TransferState::Cancelled;
            t.updated_at = self.inner.clock.now();
            tx.put("transfer", id, &t)?;
            tx.emit(Event::TransferUpserted { transfer: t })?;
            Ok(())
        })?;
        if guard.is_some() {
            remove_if_present(&self.stage_path(id)).await?;
        }
        Ok(())
    }
    /// Cancels abandoned transfers, then removes staged bytes no upload is
    /// waiting on and artifact blobs nothing refers to any more.
    pub(crate) async fn collect_transfers(&self) -> Result<(), ProtocolError> {
        let now = self.inner.clock.now();
        let stale: Vec<TransferId> = self.read(|r| {
            Ok(r.list::<TransferRecord>("transfer")?
                .into_iter()
                .filter(|t| abandoned(t, now))
                .map(|t| t.transfer_id)
                .collect())
        })?;
        for id in stale {
            // An attached writer is still at work.
            let Ok(_guard) = self.transfer_lock(id)?.try_lock_owned() else {
                continue;
            };
            let cancelled = self.write(|tx| {
                let mut t: TransferRecord = tx.read().require("transfer", id)?;
                if !abandoned(&t, now) {
                    return Ok(false);
                }
                t.state = TransferState::Cancelled;
                t.updated_at = now;
                tx.put("transfer", id, &t)?;
                tx.emit(Event::TransferUpserted { transfer: t })?;
                Ok(true)
            })?;
            if cancelled {
                remove_if_present(&self.stage_path(id)).await?;
            }
        }
        self.collect_staging().await?;
        self.collect_blobs().await
    }
    /// Staged bytes outlive their transfer only when Ophio stopped before
    /// removing them. Anything no upload waits on and untouched for as long
    /// as an abandoned transfer is left over.
    async fn collect_staging(&self) -> Result<(), ProtocolError> {
        let waiting: HashSet<String> = self.read(|r| {
            Ok(r.list::<TransferRecord>("transfer")?
                .into_iter()
                .filter(|t| {
                    t.direction == TransferDirection::PhoneToHost
                        && !matches!(t.state, TransferState::Completed | TransferState::Cancelled)
                })
                .map(|t| t.transfer_id.to_string())
                .collect())
        })?;
        let mut entries = match tokio::fs::read_dir(self.inner.data.join("staging")).await {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(internal(e)),
        };
        while let Some(entry) = entries.next_entry().await.map_err(internal)? {
            if waiting.contains(&*entry.file_name().to_string_lossy()) {
                continue;
            }
            let modified = entry
                .metadata()
                .await
                .and_then(|m| m.modified())
                .map_err(internal)?;
            if modified.elapsed().is_ok_and(|idle| idle >= ABANDONED_AFTER) {
                remove_if_present(&entry.path()).await?;
            }
        }
        Ok(())
    }
    /// A blob is kept while an artifact names it, a download may still read
    /// it, or an upload is publishing it. Publication holds `blobs` for
    /// reading until its record is stored, so a blob is never judged between
    /// being written and being named.
    pub(crate) async fn collect_blobs(&self) -> Result<(), ProtocolError> {
        let _collecting = self.inner.blobs.write().await;
        let now = self.inner.clock.now();
        let named: HashSet<String> = self.read(|r| {
            let mut named: HashSet<String> = r
                .list::<ArtifactMeta>("artifact")?
                .into_iter()
                .map(|a| a.sha256)
                .collect();
            for t in r.list::<TransferRecord>("transfer")? {
                let reads = match t.direction {
                    TransferDirection::HostToPhone => {
                        !matches!(
                            t.state,
                            TransferState::Cancelled | TransferState::Failed { .. }
                        ) && !idle(&t, now)
                    }
                    TransferDirection::PhoneToHost => {
                        // Completion and artifact metadata are committed
                        // together. From then on the artifact owns the blob;
                        // transfer history must not pin it after deletion.
                        t.destination == Some(TransferDestination::Artifact)
                            && !matches!(
                                t.state,
                                TransferState::Completed | TransferState::Cancelled
                            )
                    }
                };
                if reads {
                    named.extend(t.sha256);
                }
            }
            named.extend(crate::checklist::blobs(r)?);
            Ok(named)
        })?;
        let mut shards = match tokio::fs::read_dir(self.inner.data.join("artifacts")).await {
            Ok(shards) => shards,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(internal(e)),
        };
        while let Some(shard) = shards.next_entry().await.map_err(internal)? {
            if !shard.file_type().await.map_err(internal)?.is_dir() {
                continue;
            }
            let mut blobs = tokio::fs::read_dir(shard.path()).await.map_err(internal)?;
            while let Some(blob) = blobs.next_entry().await.map_err(internal)? {
                if !named.contains(&*blob.file_name().to_string_lossy()) {
                    remove_if_present(&blob.path()).await?;
                }
            }
        }
        Ok(())
    }
}
/// A file on the computer, or an artifact's label, can carry direction
/// controls that make its name read as another one on the phone, where it is
/// shown and saved. They are removed; everything else is kept as it is.
fn download_name(name: &str) -> String {
    let kept: String = name
        .chars()
        .filter(|&c| !ports::library::direction_control(c))
        .collect();
    if kept.trim().is_empty() {
        "File".into()
    } else {
        kept
    }
}
fn idle(t: &TransferRecord, now: protocol::time::Timestamp) -> bool {
    now.millis().saturating_sub(t.updated_at.millis()) >= ABANDONED_AFTER.as_millis() as i64
}
/// Unfinished and untouched for a day. Staged bytes of a failed upload wait
/// for a cancellation that an abandoned transfer never gets.
fn abandoned(t: &TransferRecord, now: protocol::time::Timestamp) -> bool {
    let unfinished = match t.direction {
        TransferDirection::PhoneToHost => matches!(
            t.state,
            TransferState::Pending
                | TransferState::InProgress
                | TransferState::Paused
                | TransferState::Failed { .. }
        ),
        TransferDirection::HostToPhone => matches!(
            t.state,
            TransferState::Pending | TransferState::InProgress | TransferState::Paused
        ),
    };
    unfinished && idle(t, now)
}
/// Inbox and project-folder uploads land outside the Library's own checks.
/// A folder the owner shared read-only stays read-only to them too.
fn outside_read_only_roots(roots: &[LibraryRoot], folder: &Path) -> Result<(), ProtocolError> {
    let folder = ports::library::canonical(folder).unwrap_or_else(|_| folder.to_owned());
    match roots
        .iter()
        .find(|root| !root.writable && folder.starts_with(&root.host_path))
    {
        Some(root) => Err(error(
            ErrorCode::PathNotAllowed,
            format!(
                "“{}” is a read-only Library folder on the computer. Send the file to another folder.",
                root.label
            ),
        )),
        None => Ok(()),
    }
}
fn task_artifact(meta: &ArtifactMeta) -> bool {
    meta.task_id.is_some() || !meta.links.is_empty()
}
/// Whether a message to an agent names the artifact as a reviewed source:
/// one sent, a task's first prompt (resumes send it again), or a Next up
/// request still to start.
pub(crate) fn sent(r: &store::Read<'_>, meta: &ArtifactMeta) -> store::Result<bool> {
    use protocol::{annotation::AnnotationBundle, queue::QueueView};
    let names = |bundle: &Option<Box<AnnotationBundle>>| {
        bundle
            .as_ref()
            .is_some_and(|b| b.sources.iter().any(|s| s.artifact_id == meta.artifact_id))
    };
    if meta
        .links
        .iter()
        .any(|l| l.role == protocol::model::ArtifactRole::Source)
    {
        return Ok(true);
    }
    let prompts = r.list::<(String, Option<Box<AnnotationBundle>>)>("prompt")?;
    if prompts.iter().any(|(_, bundle)| names(bundle)) {
        return Ok(true);
    }
    Ok(r.list::<QueueView>("queue")?.iter().any(|q| {
        q.items
            .iter()
            .any(|i| i.request.as_ref().is_some_and(|req| names(&req.bundle)))
    }))
}
async fn remove_if_present(path: &Path) -> Result<(), ProtocolError> {
    match tokio::fs::remove_file(path).await {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(internal(e)),
    }
}
/// Refuses a file when the volume holding `path` has less than `needed` free.
/// Room a publication's copy has claimed on its volume.
#[derive(Debug)]
pub(crate) struct Claim<'a> {
    hub: &'a CompanionHub,
    transfer: TransferId,
    volume: u64,
    size: u64,
    written: bool,
}
impl Claim<'_> {
    /// The copy is written, and its room shows on the volume itself.
    pub(crate) fn written(mut self) {
        self.written = true;
    }
}
impl Drop for Claim<'_> {
    fn drop(&mut self) {
        let mut copying =
            (self.hub.inner.copying.lock()).unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(claimed) = copying.get_mut(&self.volume) {
            *claimed = claimed.saturating_sub(self.size);
            if *claimed == 0 {
                copying.remove(&self.volume);
            }
        }
        drop(copying);
        // A copy given up before it was written is made again, and its upload
        // holds room for it until then.
        if !self.written {
            (self.hub.inner.copies.lock())
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(&self.transfer);
        }
    }
}
#[cfg(test)]
thread_local! {
    /// Told when a copy on this thread has read what is still arriving and is
    /// about to check and claim its room.
    pub(crate) static CLAIMING: std::cell::RefCell<Option<std::sync::mpsc::Sender<()>>> =
        const { std::cell::RefCell::new(None) };
}
fn room(path: &Path, needed: u64) -> Result<(), ProtocolError> {
    let free = free_space(path)?;
    if free < needed {
        return Err(error(
            ErrorCode::LimitExceeded,
            format!(
                "The computer doesn’t have room for this file. Free up {} on the computer, then send it again.",
                amount(needed - free)
            ),
        ));
    }
    Ok(())
}
/// Which volume holds `path`.
#[cfg(unix)]
fn volume(path: &Path) -> Result<u64, ProtocolError> {
    use std::os::unix::fs::MetadataExt;
    Ok(std::fs::metadata(path).map_err(internal)?.dev())
}
/// Which volume holds `path`: its serial number.
#[cfg(windows)]
#[allow(unsafe_code)]
fn volume(path: &Path) -> Result<u64, ProtocolError> {
    use std::os::windows::{fs::OpenOptionsExt, io::AsRawHandle};
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, FILE_FLAG_BACKUP_SEMANTICS, GetFileInformationByHandle,
    };
    // Directories open only with backup semantics; no access is requested.
    let file = std::fs::OpenOptions::new()
        .access_mode(0)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
        .map_err(internal)?;
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: the handle is open for the call and `info` is a valid output.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 {
        return Err(internal(std::io::Error::last_os_error()));
    }
    Ok(u64::from(info.dwVolumeSerialNumber))
}
/// Bytes this process may still write on the volume holding `path`.
#[cfg(unix)]
pub(crate) fn free_space(path: &Path) -> Result<u64, ProtocolError> {
    let stat = rustix::fs::statvfs(path).map_err(|e| internal(std::io::Error::from(e)))?;
    Ok(stat.f_bavail.saturating_mul(stat.f_frsize))
}
/// Bytes this process may still write on the volume holding `path`.
#[cfg(windows)]
#[allow(unsafe_code)]
pub(crate) fn free_space(path: &Path) -> Result<u64, ProtocolError> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut available = 0u64;
    // SAFETY: `wide` is a NUL-terminated owned buffer alive for the call and
    // `available` a valid output; the optional totals are null.
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &mut available,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(internal(std::io::Error::last_os_error()));
    }
    Ok(available)
}
/// A byte count as a person reads it, in the units the phone uses.
fn amount(bytes: u64) -> String {
    const KB: u64 = 1024;
    match bytes {
        b if b < KB => format!("{b} B"),
        b if b < KB * KB => format!("{:.1} KB", b as f64 / KB as f64),
        b if b < KB * KB * KB => format!("{:.1} MB", b as f64 / (KB * KB) as f64),
        b => format!("{:.1} GB", b as f64 / (KB * KB * KB) as f64),
    }
}
#[async_trait]
impl UploadSink for Upload {
    fn resume_at(&self) -> u64 {
        self.offset
    }
    async fn write(&mut self, chunk: &[u8]) -> Result<(), ProtocolError> {
        self.hub.authorize(&self.caller, |g| g.transfers)?;
        let t: TransferRecord = self.hub.read(|r| r.require("transfer", self.id))?;
        if t.state != TransferState::InProgress {
            return Err(error(
                ErrorCode::Conflict,
                "Transfer is not accepting bytes",
            ));
        }
        if chunk.len() > MAX_CHUNK_BYTES
            || self
                .offset
                .checked_add(chunk.len() as u64)
                .is_none_or(|size| size > t.size)
        {
            return Err(error(
                ErrorCode::LimitExceeded,
                "Chunk exceeds the declared upload size or chunk limit",
            ));
        }
        self.file.write_all(chunk).await.map_err(internal)?;
        // Tokio completes a file write in the background; wait for it, so a
        // writer that reattaches after this one is dropped resumes at the
        // staged file's real length rather than one still being written.
        self.file.flush().await.map_err(internal)?;
        self.offset += chunk.len() as u64;
        if self
            .hub
            .inner
            .clock
            .monotonic()
            .duration_since(self.last_progress)
            >= std::time::Duration::from_millis(250)
        {
            self.file.sync_data().await.map_err(internal)?;
            self.hub
                .transfer_progress(self.id, self.offset, TransferState::InProgress)?;
            self.last_progress = self.hub.inner.clock.monotonic();
        }
        Ok(())
    }
    async fn finish(self: Box<Self>) -> Result<UploadDone, ProtocolError> {
        // Publication runs to its end even if the connection that asked for
        // it goes away part-way. Dropped mid-way, it would leave the transfer
        // Verifying, which neither a resume nor a cancel accepts.
        tokio::spawn(self.conclude())
            .await
            .map_err(|_| internal("upload publication task ended abnormally"))?
    }
}
impl Upload {
    async fn conclude(self: Box<Self>) -> Result<UploadDone, ProtocolError> {
        let hub = self.hub.clone();
        let id = self.id;
        let outcome = self.publish().await;
        if let Err(failure) = &outcome {
            let _ = hub.write(|tx| {
                let mut t: TransferRecord = tx.read().require("transfer", id)?;
                if t.state == TransferState::Verifying {
                    t.state = if tx.read().get::<bool>("publication_started", id)?.unwrap_or(false) {
                        TransferState::ReconciliationRequired {
                            reason: "Publication is unconfirmed; inspect the recorded destination, then cancel to clear".into(),
                        }
                    } else {
                        TransferState::Failed { reason: failure.message.clone() }
                    };
                    t.updated_at = hub.inner.clock.now();
                    tx.put("transfer", id, &t)?;
                    tx.emit(Event::TransferUpserted { transfer: t })?;
                }
                Ok(())
            });
        }
        if outcome.is_err()
            && hub.read(|r| Ok(r.get::<bool>("publication_started", id)?.unwrap_or(false)))?
        {
            return Err(error(
                ErrorCode::OutcomeUnknown,
                "Upload publication is unconfirmed; inspect the recorded destination, then cancel to clear",
            ));
        }
        outcome
    }
    /// Records that a publication definitely didn't happen: nothing was made
    /// under the recorded name, so the transfer fails with `failure` rather
    /// than asking for its destination to be checked. If even that can't be
    /// recorded, the outcome stays unconfirmed.
    fn unpublished(&self, failure: ProtocolError) -> ProtocolError {
        let _ = self.hub.write(|tx| {
            let mut t: TransferRecord = tx.read().require("transfer", self.id)?;
            t.publication = None;
            tx.put("transfer", self.id, &t)?;
            tx.delete("publication_started", self.id)?;
            Ok(())
        });
        failure
    }
    fn record_publication(&self, publication: TransferPublication) -> Result<(), ProtocolError> {
        self.hub.write(|tx| {
            let mut t: TransferRecord = tx.read().require("transfer", self.id)?;
            t.publication = Some(publication);
            tx.put("transfer", self.id, &t)?;
            tx.put("publication_started", self.id, &true)?;
            Ok(())
        })
    }
    async fn publish(mut self: Box<Self>) -> Result<UploadDone, ProtocolError> {
        self.hub.authorize(&self.caller, |g| g.transfers)?;
        let t: TransferRecord = self.hub.read(|r| r.require("transfer", self.id))?;
        if t.state != TransferState::InProgress {
            return Err(error(ErrorCode::Conflict, "Transfer is no longer active"));
        }
        if self.offset != t.size {
            self.hub
                .transfer_progress(self.id, self.offset, TransferState::Paused)?;
            return Err(error(
                ErrorCode::InvalidRequest,
                "Upload length does not match its declaration",
            ));
        }
        self.file.sync_all().await.map_err(internal)?;
        self.hub
            .transfer_progress(self.id, self.offset, TransferState::Verifying)?;
        self.file
            .seek(std::io::SeekFrom::Start(0))
            .await
            .map_err(internal)?;
        let mut hasher = Sha256::new();
        let mut buffer = vec![0; MAX_CHUNK_BYTES];
        loop {
            let n = self.file.read(&mut buffer).await.map_err(internal)?;
            if n == 0 {
                break;
            }
            hasher.update(&buffer[..n]);
        }
        let sha256 = hex(&hasher.finalize());
        if t.sha256.as_deref() != Some(&sha256) {
            self.hub.transfer_progress(
                self.id,
                self.offset,
                TransferState::Failed {
                    reason: "SHA-256 mismatch; cancel this transfer and declare a new upload"
                        .into(),
                },
            )?;
            return Err(error(
                ErrorCode::Conflict,
                "Upload hash mismatch; staged bytes retained until cancellation",
            ));
        }
        let stage = self.hub.stage_path(self.id);
        let mut artifact_id = None;
        let mut host_path = None;
        let mut publishing = None;
        match t
            .destination
            .as_ref()
            .ok_or_else(|| internal("destination missing"))?
        {
            TransferDestination::Artifact => {
                publishing = Some(self.hub.inner.blobs.clone().read_owned().await);
                self.record_publication(TransferPublication {
                    root: self
                        .hub
                        .inner
                        .data
                        .join("artifacts")
                        .to_string_lossy()
                        .into_owned(),
                    path: format!("{}/{}", &sha256[..2], sha256),
                })?;
                // Until the artifact is recorded below, a blob stored or not
                // is no file anyone sees; one left over is collected later.
                if let Err(failure) = self.hub.publish_blob(&stage, &sha256).await {
                    return Err(self.unpublished(error(
                        failure.code,
                        "Not saved: the computer couldn’t store the file. Send it again.",
                    )));
                }
                artifact_id = Some(ArtifactId::new());
            }
            destination => {
                let (root_path, relative_dir) = match destination {
                    TransferDestination::Inbox => {
                        let p = t
                            .destination_folder
                            .as_ref()
                            .map(PathBuf::from)
                            .unwrap_or_else(|| self.hub.inner.data.join("inbox"));
                        tokio::fs::create_dir_all(&p).await.map_err(internal)?;
                        (p, String::new())
                    }
                    TransferDestination::ProjectDir { project_id, dir } => {
                        self.hub.authorize(&self.caller, |g| g.tasks)?;
                        relative(dir, true)?;
                        let p: protocol::model::Project =
                            self.hub.read(|r| r.require("project", project_id))?;
                        crate::commands::not_home(&p)?;
                        (PathBuf::from(p.host_path), dir.clone())
                    }
                    TransferDestination::Artifact => unreachable!(),
                };
                let roots = match &self.hub.inner.library {
                    Some(library) => library.roots().await,
                    None => Vec::new(),
                };
                outside_read_only_roots(&roots, &root_path.join(&relative_dir))?;
                let root = match destination {
                    TransferDestination::ProjectDir { .. } => open_project_root(&root_path),
                    _ => Dir::open_ambient_dir(&root_path, ambient_authority()),
                }
                .map_err(path_error)?;
                let dir = if relative_dir.is_empty() {
                    root
                } else {
                    root.open_dir(&relative_dir).map_err(path_error)?
                };
                let staged_name = format!(".incoming-{}", self.id);
                // Only this transfer uses that name. Anything under it is a
                // partial copy left by its own interrupted publication.
                match dir.remove_file(&staged_name) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(path_error(e)),
                }
                let claim = self
                    .hub
                    .copy_room(&root_path.join(&relative_dir), t.size, self.id)?;
                let mut options = OpenOptions::new();
                options.write(true).create_new(true);
                let temp = dir.open_with(&staged_name, &options).map_err(path_error)?;
                let _staged = Staged {
                    dir: &dir,
                    name: &staged_name,
                };
                let mut output = tokio::fs::File::from_std(temp.into_std());
                self.file
                    .seek(std::io::SeekFrom::Start(0))
                    .await
                    .map_err(internal)?;
                tokio::io::copy(&mut self.file, &mut output)
                    .await
                    .map_err(internal)?;
                output.sync_all().await.map_err(internal)?;
                drop(output);
                claim.written();
                let name = t.name.clone();
                let collision: Option<UploadCollision> =
                    self.hub.read(|r| r.get("upload_collision", self.id))?;
                let replacing = matches!(collision, Some(UploadCollision::Replace { .. }));
                if let Some(UploadCollision::Replace { revision }) = collision {
                    self.hub
                        .authorize(&self.caller, |g| g.library_read && g.library_write)?;
                    let root = self.hub.replacement_root(destination).await?;
                    let path = if relative_dir.is_empty() {
                        name.clone()
                    } else {
                        format!("{relative_dir}/{name}")
                    };
                    self.record_publication(TransferPublication {
                        root: root_path.to_string_lossy().into_owned(),
                        path: path.clone(),
                    })?;
                    // Trash first, then no-replace publication. A concurrent creator
                    // wins without losing its bytes; the reviewed old copy remains recoverable.
                    if let Err(e) = self
                        .hub
                        .library()?
                        .trash(root.root_id, &path, &revision)
                        .await
                    {
                        return Err(crate::plumbing::library_error(e));
                    }
                    self.hub.library_changed(root.root_id, &path, None)?;
                } else {
                    match dir.symlink_metadata(&name) {
                        Ok(_) => {
                            return Err(error(
                                ErrorCode::Conflict,
                                "Destination exists. Review replace, keep both, or cancel before sending again",
                            ));
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                        Err(e) => return Err(path_error(e)),
                    }
                }
                self.record_publication(TransferPublication {
                    root: root_path.to_string_lossy().into_owned(),
                    path: Path::new(&relative_dir)
                        .join(&name)
                        .to_string_lossy()
                        .into_owned(),
                })?;
                if let Err(mut failure) = publish_new(&dir, &staged_name, &name, |staged, name| {
                    dir.hard_link(staged, &dir, name)
                }) {
                    if replacing {
                        failure
                            .message
                            .push_str(" The copy it was replacing is in Recently deleted.");
                    }
                    return Err(self.unpublished(failure));
                }
                #[cfg(unix)]
                dir.open(".")
                    .map_err(internal)?
                    .sync_all()
                    .map_err(internal)?;
                // Directory capability prevents symlink/reparse escapes during
                // final publication, including concurrent directory changes.
                host_path = Some(
                    root_path
                        .join(relative_dir)
                        .join(name)
                        .to_string_lossy()
                        .into_owned(),
                );
            }
        }
        self.hub.write(|tx| {
            if let Some(id) = artifact_id {
                let artifact = ArtifactMeta {
                    artifact_id: id,
                    media_type: media_type(&t.name).into(),
                    size: t.size,
                    sha256: sha256.clone(),
                    created_at: self.hub.inner.clock.now(),
                    label: Some(t.name.clone()),
                    task_id: tx
                        .read()
                        .get::<protocol::model::ArtifactLink>("transfer_evidence", self.id)?
                        .map(|l| l.task_id),
                    links: tx
                        .read()
                        .get::<protocol::model::ArtifactLink>("transfer_evidence", self.id)?
                        .into_iter()
                        .collect(),
                    capture: None,
                };
                tx.put("artifact", id, &artifact)?;
                tx.put("artifact_creator", id, &self.caller.principal)?;
                tx.put(
                    "artifact_provenance",
                    id,
                    &protocol::model::Provenance::Observed,
                )?;
                tx.emit(Event::ArtifactCreated { artifact })?;
            }
            let mut t: TransferRecord = tx.read().require("transfer", self.id)?;
            tx.delete("publication_started", self.id)?;
            t.state = TransferState::Completed;
            t.transferred = self.offset;
            t.host_path = host_path;
            t.updated_at = self.hub.inner.clock.now();
            tx.put("transfer", self.id, &t)?;
            tx.emit(Event::TransferUpserted { transfer: t })?;
            Ok(())
        })?;
        drop(publishing);
        let done = UploadDone {
            sha256,
            artifact_id,
        };
        drop(self);
        let _ = tokio::fs::remove_file(stage).await;
        Ok(done)
    }
}
fn media_type(name: &str) -> &'static str {
    match Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "md" => "text/markdown",
        "txt" => "text/plain",
        _ => "application/octet-stream",
    }
}
fn valid_hash(sha: &str) -> Result<(), ProtocolError> {
    if sha.len() == 64
        && sha
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        Ok(())
    } else {
        Err(error(
            ErrorCode::InvalidRequest,
            "Expected a lowercase SHA-256 digest",
        ))
    }
}
/// A transferred file's name is one a file can have on any computer, by the
/// Library's rules (no reserved characters or Windows device names, no
/// trailing dot or space), with no direction controls, which could make it
/// read as another name (an `.exe` shown as a `.pdf`).
pub(crate) fn valid_name(name: &str) -> Result<(), ProtocolError> {
    if name.is_empty()
        || name.len() > MAX_NAME_BYTES
        || name == "."
        || name == ".."
        || name.chars().any(|c| {
            c.is_control() || "/\\:<>\"|?*".contains(c) || ports::library::direction_control(c)
        })
        || name.ends_with([' ', '.'])
        || ports::library::reserved_on_windows(name)
    {
        Err(error(
            ErrorCode::PathNotAllowed,
            "Invalid transfer filename",
        ))
    } else {
        Ok(())
    }
}
pub(crate) fn relative(path: &str, empty: bool) -> Result<(), ProtocolError> {
    if (!empty && path.is_empty())
        || path.contains(['\\', ':', '\0'])
        || Path::new(path)
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        return Err(error(
            ErrorCode::PathNotAllowed,
            "Path must remain relative to its root",
        ));
    }
    Ok(())
}
/// Opens a regular file inside the project's folder for reading. The path is
/// relative and can't lead out of the folder, by `..` or by a link.
pub(crate) fn open_project_file(
    project: &protocol::model::Project,
    path: &str,
) -> Result<std::fs::File, ProtocolError> {
    relative(path, false)?;
    crate::commands::not_home(project)?;
    let dir = open_project_root(Path::new(&project.host_path)).map_err(path_error)?;
    open_file_in(&dir, path)
}
/// Opens a regular file inside the task's scratch folder for reading, the
/// way [`open_project_file`] opens one in the project.
pub(crate) fn open_scratch_file(
    scratch: &Path,
    path: &str,
) -> Result<std::fs::File, ProtocolError> {
    relative(path, false)?;
    let dir = Dir::open_ambient_dir(scratch, cap_std::ambient_authority()).map_err(path_error)?;
    open_file_in(&dir, path)
}
fn open_file_in(dir: &Dir, path: &str) -> Result<std::fs::File, ProtocolError> {
    // A FIFO with no writer would block this open, and the
    // runtime thread with it. Open without waiting, then refuse it.
    let mut options = cap_std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use cap_std::fs::OpenOptionsExt;
        options.custom_flags(rustix::fs::OFlags::NONBLOCK.bits() as i32);
    }
    let file = dir
        .open_with(path, &options)
        .map_err(path_error)?
        .into_std();
    let metadata = file.metadata().map_err(internal)?;
    if !metadata.is_file() {
        return Err(error(
            ErrorCode::PathNotAllowed,
            "Download requires a regular file",
        ));
    }
    // A hard link planted in the project can name any file
    // its owner can read, anywhere on the computer.
    if links(&file, &metadata).map_err(internal)? != 1 {
        return Err(error(
            ErrorCode::PathNotAllowed,
            "This file is hard-linked to another name on the computer, so it can’t be sent from the project.",
        ));
    }
    Ok(file)
}
/// Opens the project folder an upload goes to the way publication will, so
/// a folder that is missing, isn't a folder, leads out of the project or
/// can't take new files is refused before any bytes are sent.
pub(crate) fn project_folder(
    project: &protocol::model::Project,
    dir: &str,
) -> Result<Dir, ProtocolError> {
    crate::commands::not_home(project)?;
    let place = if dir.is_empty() {
        format!("“{}”", project.name)
    } else {
        format!("“{dir}” in {}", project.name)
    };
    let unusable = || {
        error(
            ErrorCode::PathNotAllowed,
            format!(
                "{place} isn’t a folder files can be sent to. Choose another folder, then send the file again."
            ),
        )
    };
    let folder = match open_project_root(Path::new(&project.host_path)) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(error(
                ErrorCode::NotFound,
                format!(
                    "The project folder {} can’t be found. Reconnect its drive or move it back, then send the file again.",
                    project.host_path
                ),
            ));
        }
        Err(_) => return Err(unusable()),
        Ok(root) if dir.is_empty() => root,
        Ok(root) => match root.open_dir(dir) {
            Ok(folder) => folder,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(error(
                    ErrorCode::NotFound,
                    format!(
                        "There’s no folder “{dir}” in {}. Create it on the computer or choose another folder, then send the file again.",
                        project.name
                    ),
                ));
            }
            Err(_) => return Err(unusable()),
        },
    };
    #[cfg(unix)]
    {
        use rustix::fs::{Access, AtFlags, accessat};
        accessat(&folder, ".", Access::WRITE_OK | Access::EXEC_OK, AtFlags::empty()).map_err(
            |_| {
                error(
                    ErrorCode::PermissionDenied,
                    format!("This computer can’t add files to {place}. Choose another folder, or allow changes to it on the computer, then send the file again."),
                )
            },
        )?;
    }
    Ok(folder)
}
/// Puts staged bytes in `dir` under their final name without replacing
/// anything there, so a file someone created meanwhile keeps its bytes. A
/// hard link does that atomically. A filesystem without hard links (exFAT,
/// some network mounts) refuses one, and a rename that won't replace an
/// existing name does it instead. `path` is `dir`'s own path.
///
/// Either way the name is made or it isn't, so an error means the file was
/// not published. Once it is, the staged name is only tidied away; if that
/// fails, the caller's guard removes it.
fn publish_new(
    dir: &Dir,
    staged: &str,
    name: &str,
    link: impl FnOnce(&str, &str) -> std::io::Result<()>,
) -> Result<(), ProtocolError> {
    let published = match link(staged, name) {
        Ok(()) => {
            let _ = dir.remove_file(staged);
            Ok(())
        }
        Err(e) if links_unsupported(&e) => rename_new(dir, staged, name),
        Err(e) => Err(e),
    };
    published.map_err(|e| {
        if e.kind() == std::io::ErrorKind::AlreadyExists {
            error(
                ErrorCode::Conflict,
                "Not saved: another file took this name while it was being saved. Send it again to replace it or keep both.",
            )
        } else {
            error(
                ErrorCode::PathNotAllowed,
                "Not saved: the computer couldn’t create the file in its folder.",
            )
        }
    })
}
/// FAT and exFAT refuse a hard link as not permitted; some network and
/// FUSE filesystems say they don't support one.
fn links_unsupported(e: &std::io::Error) -> bool {
    #[cfg(unix)]
    {
        use rustix::io::Errno;
        matches!(
            Errno::from_io_error(e),
            Some(Errno::PERM | Errno::OPNOTSUPP | Errno::NOSYS)
        )
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{ERROR_INVALID_FUNCTION, ERROR_NOT_SUPPORTED};
        e.raw_os_error().is_some_and(|code| {
            [ERROR_INVALID_FUNCTION, ERROR_NOT_SUPPORTED].contains(&(code as u32))
        })
    }
}
/// How many names the open file has.
#[cfg(unix)]
pub(crate) fn links(_file: &std::fs::File, metadata: &std::fs::Metadata) -> std::io::Result<u64> {
    use std::os::unix::fs::MetadataExt;
    Ok(metadata.nlink())
}
/// How many names the open file has, read through its handle: std's
/// metadata does not carry the count on Windows.
#[cfg(windows)]
#[allow(unsafe_code)]
pub(crate) fn links(file: &std::fs::File, _metadata: &std::fs::Metadata) -> std::io::Result<u64> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
    };
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: `file` keeps its handle open for the call and `info` is writable.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(info.nNumberOfLinks.into())
}
/// Renames within `dir`, failing if `to` already exists.
#[cfg(target_os = "linux")]
fn rename_new(dir: &Dir, from: &str, to: &str) -> std::io::Result<()> {
    use rustix::fs::{RenameFlags, renameat_with};
    renameat_with(dir, from, dir, to, RenameFlags::NOREPLACE).map_err(Into::into)
}
/// Renames within `dir`, failing if `to` already exists. The file is opened
/// through `dir` and given its new name in `dir` by handle, so the folder
/// being moved or replaced meanwhile can't change which file or where.
///
/// The name is set through ntdll: kernelbase's SetFileInformationByHandle
/// refuses a rename relative to a folder handle as an invalid parameter.
#[cfg(windows)]
#[allow(unsafe_code)]
fn rename_new(dir: &Dir, from: &str, to: &str) -> std::io::Result<()> {
    use cap_std::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Wdk::Storage::FileSystem::{
        FILE_RENAME_INFORMATION, FileRenameInformation, NtSetInformationFile,
    };
    use windows_sys::Win32::Foundation::RtlNtStatusToDosError;
    use windows_sys::Win32::Storage::FileSystem::{DELETE, FILE_READ_ATTRIBUTES, SYNCHRONIZE};
    use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;
    let mut options = cap_std::fs::OpenOptions::new();
    options.access_mode(DELETE | FILE_READ_ATTRIBUTES | SYNCHRONIZE);
    let file = dir.open_with(from, &options)?;
    let name: Vec<u16> = to.encode_utf16().collect();
    let size = std::mem::size_of::<FILE_RENAME_INFORMATION>() + std::mem::size_of_val(&name[..]);
    // Zeroed, so `ReplaceIfExists` is false; u64s keep it aligned.
    let mut buffer = vec![0u64; size.div_ceil(8)];
    let info = buffer.as_mut_ptr().cast::<FILE_RENAME_INFORMATION>();
    // SAFETY: `buffer` is aligned for FILE_RENAME_INFORMATION and holds it
    // with the name after its header; `file` and `dir` keep their handles
    // open for the call, which reads `size` bytes from `info` and writes
    // only `status`.
    let renamed = unsafe {
        let mut status: IO_STATUS_BLOCK = std::mem::zeroed();
        (*info).RootDirectory = dir.as_raw_handle();
        (*info).FileNameLength = std::mem::size_of_val(&name[..]) as u32;
        std::ptr::copy_nonoverlapping(
            name.as_ptr(),
            std::ptr::addr_of_mut!((*info).FileName).cast::<u16>(),
            name.len(),
        );
        NtSetInformationFile(
            file.as_raw_handle(),
            &mut status,
            info.cast(),
            size as u32,
            FileRenameInformation,
        )
    };
    if renamed < 0 {
        // SAFETY: only maps a status code to its Win32 error.
        let code = unsafe { RtlNtStatusToDosError(renamed) };
        return Err(std::io::Error::from_raw_os_error(code as i32));
    }
    Ok(())
}
/// Other systems have no rename here that refuses an existing name.
#[cfg(not(any(target_os = "linux", windows)))]
fn rename_new(_dir: &Dir, _from: &str, _to: &str) -> std::io::Result<()> {
    Err(std::io::ErrorKind::Unsupported.into())
}
fn path_error(_: std::io::Error) -> ProtocolError {
    error(
        ErrorCode::PathNotAllowed,
        "File is missing, inaccessible, or outside the authorized root",
    )
}
pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
async fn sync_directory(path: &Path) -> Result<(), ProtocolError> {
    #[cfg(unix)]
    {
        tokio::fs::File::open(path)
            .await
            .map_err(internal)?
            .sync_all()
            .await
            .map_err(internal)?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

/// An upload's bytes staged in its destination folder. Every return after
/// staging, published or not, removes the staged name.
struct Staged<'a> {
    dir: &'a Dir,
    name: &'a str,
}
impl Drop for Staged<'_> {
    fn drop(&mut self) {
        let _ = self.dir.remove_file(self.name);
    }
}
struct GuardedDownload {
    file: tokio::fs::File,
    hub: CompanionHub,
    caller: Caller,
    id: TransferId,
    offset: u64,
    size: u64,
    required_grant: Option<String>,
    last_progress: std::time::Instant,
}
impl tokio::io::AsyncRead for GuardedDownload {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let this = self.get_mut();
        if this
            .hub
            .authorize(&this.caller, |g| {
                g.transfers
                    && match this.required_grant.as_deref() {
                        Some("library_read") => g.library_read,
                        Some("tasks") => g.tasks,
                        _ => true,
                    }
            })
            .is_err()
            || !this
                .hub
                .read(|r| {
                    Ok(r.require::<TransferRecord>("transfer", this.id)?.state
                        != TransferState::Cancelled)
                })
                .unwrap_or(false)
        {
            return std::task::Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "Download no longer authorized",
            )));
        }
        let before = buf.filled().len();
        match std::pin::Pin::new(&mut this.file).poll_read(cx, buf) {
            std::task::Poll::Ready(Ok(())) => {
                this.offset += (buf.filled().len() - before) as u64;
                if this.offset == this.size
                    || this
                        .hub
                        .inner
                        .clock
                        .monotonic()
                        .duration_since(this.last_progress)
                        >= std::time::Duration::from_millis(250)
                {
                    let state = if this.offset == this.size {
                        TransferState::Completed
                    } else {
                        TransferState::InProgress
                    };
                    if this
                        .hub
                        .transfer_progress(this.id, this.offset, state)
                        .is_err()
                    {
                        return std::task::Poll::Ready(Err(std::io::Error::other(
                            "Download progress could not be recorded",
                        )));
                    }
                    this.last_progress = this.hub.inner.clock.monotonic();
                }
                std::task::Poll::Ready(Ok(()))
            }
            other => other,
        }
    }
}

/// The name "Keep both" offers: `name (suffix)`, the number before any
/// extension. A stem too long for the number to fit is shortened on a
/// character boundary; the extension stays whole. An extension that leaves
/// no room for any of the stem is not one, and the whole name is shortened.
pub(crate) fn collision_name(name: &str, suffix: u32) -> String {
    if suffix == 0 {
        return name.into();
    }
    let split = [".tar.gz", ".tar.bz2", ".tar.xz", ".tar.zst"]
        .iter()
        .find_map(|extension| {
            name.strip_suffix(extension)
                .filter(|stem| !stem.is_empty())
                .map(|stem| stem.len())
        })
        .or_else(|| name.rfind('.').filter(|i| *i > 0 && *i + 1 < name.len()));
    let number = format!(" ({suffix})");
    let room = |extension: &str| MAX_NAME_BYTES.saturating_sub(number.len() + extension.len());
    let (stem, extension) = match name.split_at(split.unwrap_or(name.len())) {
        (stem, extension) if stem.floor_char_boundary(room(extension)) > 0 => (stem, extension),
        _ => (name, ""),
    };
    let stem = &stem[..stem.floor_char_boundary(room(extension))];
    format!("{stem}{number}{extension}")
}
#[cfg(all(test, any(target_os = "linux", windows)))]
mod publication_tests {
    use cap_std::{ambient_authority, fs::Dir};

    /// exFAT and FAT answer a hard link as not permitted.
    #[cfg(target_os = "linux")]
    fn no_links(_: &str, _: &str) -> std::io::Result<()> {
        Err(rustix::io::Errno::PERM.into())
    }
    /// A filesystem without hard links answers that it doesn't support one.
    #[cfg(windows)]
    fn no_links(_: &str, _: &str) -> std::io::Result<()> {
        Err(std::io::Error::from_raw_os_error(
            windows_sys::Win32::Foundation::ERROR_NOT_SUPPORTED as i32,
        ))
    }

    #[test]
    fn a_rename_publishes_in_the_folder_held_even_if_it_was_moved() {
        let temp = tempfile::tempdir().unwrap();
        let (held, moved) = (temp.path().join("Inbox"), temp.path().join("Moved"));
        std::fs::create_dir(&held).unwrap();
        let dir = Dir::open_ambient_dir(&held, ambient_authority()).unwrap();
        std::fs::write(held.join(".incoming-1"), "sent").unwrap();
        // Another program moves the folder and puts its own in its place.
        // Windows doesn't let a folder held open be moved at all.
        let swapped = std::fs::rename(&held, &moved).is_ok();
        assert_eq!(swapped, cfg!(target_os = "linux"));
        let published = if swapped {
            std::fs::create_dir(&held).unwrap();
            std::fs::write(held.join(".incoming-1"), "planted").unwrap();
            &moved
        } else {
            &held
        };
        super::publish_new(&dir, ".incoming-1", "note.txt", no_links).unwrap();
        assert_eq!(std::fs::read(published.join("note.txt")).unwrap(), b"sent");
        assert!(!published.join(".incoming-1").exists());
        if swapped {
            assert!(!held.join("note.txt").exists());
            assert_eq!(std::fs::read(held.join(".incoming-1")).unwrap(), b"planted");
        }
    }

    #[test]
    fn a_filesystem_without_hard_links_publishes_by_a_rename_that_never_replaces() {
        let temp = tempfile::tempdir().unwrap();
        let dir = Dir::open_ambient_dir(temp.path(), ambient_authority()).unwrap();
        std::fs::write(temp.path().join(".incoming-1"), "sent").unwrap();
        super::publish_new(&dir, ".incoming-1", "note.txt", no_links).unwrap();
        assert_eq!(
            std::fs::read(temp.path().join("note.txt")).unwrap(),
            b"sent"
        );
        assert!(!temp.path().join(".incoming-1").exists());
        // Someone else created the name first: theirs stays as it was.
        std::fs::write(temp.path().join(".incoming-2"), "late").unwrap();
        super::publish_new(&dir, ".incoming-2", "note.txt", no_links).unwrap_err();
        assert_eq!(
            std::fs::read(temp.path().join("note.txt")).unwrap(),
            b"sent"
        );
        // Any other refusal to link stands; nothing is renamed.
        super::publish_new(&dir, ".incoming-2", "other.txt", |_, _| {
            Err(std::io::ErrorKind::PermissionDenied.into())
        })
        .unwrap_err();
        assert!(!temp.path().join("other.txt").exists());
    }
}
#[cfg(test)]
mod collision_tests {
    #[test]
    fn collision_suffix_precedes_extensions() {
        for (name, expected) in [
            ("notes.md", "notes (1).md"),
            ("archive.tar.gz", "archive (1).tar.gz"),
            ("a.tar.bz2", "a (1).tar.bz2"),
            ("a.tar.xz", "a (1).tar.xz"),
            ("a.tar.zst", "a (1).tar.zst"),
            (".env", ".env (1)"),
            ("README", "README (1)"),
        ] {
            assert_eq!(super::collision_name(name, 0), name);
            assert_eq!(super::collision_name(name, 1), expected);
        }
    }

    #[test]
    fn keep_both_names_always_fit_and_keep_the_extension() {
        for limit in 190..=super::MAX_NAME_BYTES {
            for extension in ["", ".md", ".tar.gz"] {
                for c in ['a', 'é', '日', '🙂'] {
                    let mut name = String::new();
                    while name.len() + c.len_utf8() + extension.len() <= limit {
                        name.push(c);
                    }
                    name.push_str(extension);
                    super::valid_name(&name).unwrap();
                    let stem = name.strip_suffix(extension).unwrap();
                    for suffix in [1, 9, 10, 9999] {
                        let offered = super::collision_name(&name, suffix);
                        assert!(super::valid_name(&offered).is_ok(), "{offered}");
                        let kept = offered
                            .strip_suffix(&format!(" ({suffix}){extension}"))
                            .unwrap_or_else(|| panic!("{offered}"));
                        assert!(!kept.is_empty() && stem.starts_with(kept), "{offered}");
                    }
                }
            }
        }
        // A dot near the start doesn't make the rest an extension to keep.
        let name = format!("a.{}", "b".repeat(super::MAX_NAME_BYTES - 2));
        let offered = super::collision_name(&name, 1);
        assert!(super::valid_name(&offered).is_ok());
        assert!(offered.starts_with("a.bbb") && offered.ends_with("b (1)"));
    }

    // The name offered to keep both files must itself be one an upload may
    // use, including when the original is already as long as allowed.
    #[test]
    fn a_collision_name_stays_within_the_name_limit() {
        for name in [
            format!("{}.md", "x".repeat(197)),
            format!("{}.md", "é".repeat(98)),
            format!("{}.tar.gz", "😀".repeat(48)),
            "y".repeat(200),
            format!("a.{}", "z".repeat(198)),
        ] {
            super::valid_name(&name).unwrap();
            for suffix in [1, 42, 9999] {
                let other = super::collision_name(&name, suffix);
                assert!(super::valid_name(&other).is_ok(), "{other:?}");
                assert!(other.contains(&format!(" ({suffix})")), "{other:?}");
            }
        }
    }
}
