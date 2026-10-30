// SPDX-License-Identifier: Apache-2.0
//! Durable receiver journal. Preparation reserves an identity transactionally;
//! acquisition runs outside the database and registration commits atomically.
mod delivery;
#[cfg(target_os = "linux")]
mod index;
mod network;
mod pack;
mod preview;
#[cfg(test)]
mod tests;
mod validate;
mod workspace;

use crate::{Caller, CompanionHub};
use protocol::{
    auth::Principal,
    command::CommandResult,
    contribution::{ContributionCode as Code, *},
    library::LibraryRoot,
    model::{Project, VcsInfo},
    queue::QueueView,
    *,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

#[derive(Debug)]
pub(crate) struct Runtime {
    source: Mutex<Arc<dyn network::Fetch>>,
    space: Mutex<Arc<dyn workspace::Space>>,
    slots: Arc<tokio::sync::Semaphore>,
    progress: Mutex<BTreeMap<String, Arc<AtomicU64>>>,
}
impl Default for Runtime {
    fn default() -> Self {
        Self {
            source: Mutex::new(Arc::new(network::Network)),
            space: Mutex::new(Arc::new(workspace::SystemSpace)),
            slots: Arc::new(tokio::sync::Semaphore::new(2)),
            progress: Mutex::new(BTreeMap::new()),
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Record {
    view: ContributionView,
    owner: Principal,
    input: ContributionInput,
    root: Option<workspace::Binding>,
    approval: Option<ContributionApproval>,
    manifest: Option<workspace::Manifest>,
    workspace: Option<workspace::Binding>,
    active: bool,
    command: Option<CommandId>,
    attempts: u8,
    #[serde(default)]
    separate: bool,
    #[serde(default)]
    reopen_as_is: bool,
}
impl Record {
    fn parsed(&self) -> Result<(validate::Packet, validate::Handoff), Code> {
        validate::records(&self.input.packet_json, &self.input.handoff_json)
    }
}
fn result(view: ContributionView) -> CommandResult {
    CommandResult::Contribution { operation: view }
}
fn failure(code: Code) -> ProtocolError {
    crate::error(
        ErrorCode::InvalidRequest,
        serde_json::to_string(&code)
            .unwrap_or_default()
            .trim_matches('"'),
    )
}
fn local(c: &Caller) -> Result<(), ProtocolError> {
    if c.grants.admin
        && matches!(
            c.principal,
            Principal::LocalOwner | Principal::Device { .. }
        )
    {
        Ok(())
    } else {
        Err(crate::error(
            ErrorCode::PermissionDenied,
            "contribution_manage_required",
        ))
    }
}
fn owned(r: &store::Read<'_>, c: &Caller, id: &str) -> store::Result<Record> {
    local(c)?;
    let rec: Record = r.require("contribution", id)?;
    if rec.owner != c.principal {
        return Err(
            crate::error(ErrorCode::PermissionDenied, "contribution_owner_mismatch").into(),
        );
    }
    Ok(rec)
}
fn seconds(hub: &CompanionHub) -> u64 {
    u64::try_from(hub.inner.clock.now().0 / 1000).unwrap_or(0)
}
fn root_path(r: &store::Read<'_>, input: &ContributionInput) -> Result<String, Code> {
    let roots: Vec<LibraryRoot> = r
        .get("meta", "library_roots")
        .map_err(|_| Code::RootChanged)?
        .unwrap_or_default();
    roots
        .into_iter()
        .find(|r| r.root_id == input.root_id && r.writable && r.holds(""))
        .map(|r| r.host_path)
        .ok_or(Code::RootChanged)
}
fn check_saved_root(r: &store::Read<'_>, rec: &Record) -> Result<(), Code> {
    let root = rec.root.as_ref().ok_or(Code::RootChanged)?;
    if root_path(r, &rec.input)? != root.path || workspace::bind(&root.path).as_ref() != Ok(root) {
        return Err(Code::RootChanged);
    }
    Ok(())
}
// Small transactional fence: checkpoints must not deserialize a potentially
// 20,000-file manifest for every network/decompression/write chunk.
#[derive(Debug, Serialize, Deserialize)]
struct Guard {
    owner: Principal,
    root_id: LibraryRootId,
    root: Option<workspace::Binding>,
    active: bool,
    command: Option<CommandId>,
    code: Code,
    issued_at: Option<u64>,
    expires_at: Option<u64>,
    checked_at: Option<u64>,
    valid_until: Option<u64>,
}
fn save(tx: &mut store::Write<'_>, rec: &Record) -> store::Result<()> {
    tx.put(
        "contribution_guard",
        &rec.view.operation_id,
        &Guard {
            owner: rec.owner,
            root_id: rec.input.root_id,
            root: rec.root.clone(),
            active: rec.active,
            command: rec.command,
            code: rec.view.code,
            issued_at: rec.parsed().ok().map(|(p, _)| p.issued_at),
            expires_at: rec.parsed().ok().map(|(p, _)| p.expires_at),
            checked_at: rec.approval.as_ref().map(|a| a.checked_at),
            valid_until: rec.approval.as_ref().map(|a| a.valid_until),
        },
    )?;
    tx.put("contribution", &rec.view.operation_id, rec)
}

pub(crate) fn recover(tx: &mut store::Write<'_>) -> store::Result<()> {
    for (id, guard) in tx.read().entries::<Guard>("contribution_guard")? {
        if guard.active {
            let mut rec: Record = tx.read().require("contribution", id)?;
            rec.active = false;
            rec.view.interruption_pending = false;
            rec.approval = None;
            rec.view.code = Code::ReconcileRequired;
            rec.view.received_bytes = None;
            save(tx, &rec)?;
        }
    }
    Ok(())
}
impl CompanionHub {
    fn contribution_space(&self, root: &workspace::Binding) -> Result<ContributionSpace, Code> {
        let space = self
            .inner
            .contributions
            .space
            .lock()
            .map_err(|_| Code::ReconcileRequired)?
            .clone();
        Ok(ContributionSpace {
            needed_bytes: 2 * 1024 * 1024 * 1024,
            available_bytes: space.available_bytes(root)?.min(MAX_SAFE_INTEGER as u128) as u64,
        })
    }
    fn contribution_room(&self, root: &workspace::Binding) -> Result<(), Code> {
        let space = self.contribution_space(root)?;
        if space.available_bytes < space.needed_bytes {
            Err(Code::DiskFull)
        } else {
            Ok(())
        }
    }
    pub(crate) fn set_contribution_root(
        &self,
        tx: &mut store::Write<'_>,
        caller: &Caller,
        root_id: LibraryRootId,
    ) -> store::Result<()> {
        local(caller)?;
        let roots: Vec<LibraryRoot> = tx.read().get("meta", "library_roots")?.unwrap_or_default();
        let root = roots
            .iter()
            .find(|r| r.root_id == root_id && r.writable && r.holds(""))
            .ok_or_else(|| failure(Code::RootChanged))?;
        self.project_folder(std::path::Path::new(&root.host_path))?;
        let binding = workspace::bind(&root.host_path).map_err(failure)?;
        tx.put("meta", "contributions_root", &(root_id, binding))
    }
    fn contribution_details(&self, rec: &mut Record) {
        if rec.view.code != Code::WorkspaceConflict {
            rec.view.conflict = None;
        }
        if rec.view.code == Code::DiskFull {
            rec.view.space = rec
                .root
                .as_ref()
                .and_then(|r| self.contribution_space(r).ok());
        } else {
            rec.view.space = None;
        }
        rec.view.missing_capabilities = if rec.view.code == Code::CompatibilityUnsupported {
            let mut missing = rec
                .parsed()
                .map(|(_, h)| validate::missing_capabilities(&h))
                .unwrap_or_default();
            if missing.is_empty() {
                missing.push(
                    if workspace::available() {
                        "git-smart-http"
                    } else {
                        "safe-workspace"
                    }
                    .into(),
                );
            }
            missing
        } else {
            vec![]
        };
        if !matches!(rec.view.code, Code::Acquiring | Code::Verifying) {
            rec.view.received_bytes = None;
        }
    }
    fn contribution_conflict(&self, r: &store::Read<'_>, rec: &Record) -> ContributionConflict {
        let mut detail = ContributionConflict {
            changed: None,
            untracked: None,
            branch: None,
            revision: None,
            project_id: None,
        };
        // A new review can bind the same Library root ID at a different path
        // or inode. Do not inspect a workspace authorized by its old binding.
        if check_saved_root(r, rec).is_err() {
            return detail;
        }
        if let Some(manifest) = &rec.manifest
            && workspace::bind(&manifest.binding.path).as_ref() == Ok(&manifest.binding)
        {
            let start = Instant::now();
            detail = workspace::details(manifest, &|| {
                if start.elapsed() > Duration::from_secs(3) {
                    Err(Code::LimitExceeded)
                } else {
                    Ok(())
                }
            })
            .unwrap_or(detail);
            detail.project_id = rec.view.project_id.filter(|id| {
                r.get::<Project>("project", id)
                    .ok()
                    .flatten()
                    .is_some_and(|p| p.host_path == manifest.binding.path)
            });
        }
        detail
    }
    pub(crate) fn contribution_capabilities(&self) -> ContributionCapabilities {
        let supported = workspace::available();
        let (root_ids, contributions_root_id) = self
            .read(|r| {
                let roots: Vec<LibraryRoot> = r.get("meta", "library_roots")?.unwrap_or_default();
                let eligible: Vec<_> = roots.iter().filter(|r| r.writable && r.holds("")).collect();
                let selected = r
                    .get::<(LibraryRootId, workspace::Binding)>("meta", "contributions_root")?
                    .filter(|(id, b)| {
                        eligible
                            .iter()
                            .any(|r| r.root_id == *id && r.host_path == b.path)
                            && workspace::bind(&b.path).as_ref() == Ok(b)
                    })
                    .map(|(id, _)| id);
                Ok((eligible.iter().map(|r| r.root_id).collect(), selected))
            })
            .unwrap_or_default();
        ContributionCapabilities {
            contract: CONTRACT.into(),
            transport: "authenticated-control-v1".into(),
            host_id: self.inner.host_id,
            companion_version: env!("CARGO_PKG_VERSION").into(),
            source_revision: option_env!("OPHIO_SOURCE_REVISION")
                .filter(|s| validate::hex(s, 40) || validate::hex(s, 64))
                .map(str::to_owned),
            supported,
            unsupported_reason: (!supported).then_some(if cfg!(target_os = "linux") {
                ContributionUnsupportedReason::SafeFilesystemUnavailable
            } else {
                ContributionUnsupportedReason::UnsupportedPlatform
            }),
            root_ids,
            contributions_root_id,
            capabilities: if workspace::available() {
                validate::CAPABILITIES.iter().map(|c| (*c).into()).collect()
            } else {
                vec![]
            },
            record_bytes: RECORD_BYTES as u32,
            transfer_bytes: network::TRANSFER as u32,
            expanded_bytes: pack::EXPANDED as u32,
            entries: pack::ENTRIES as u32,
            total_seconds: 120,
        }
    }
    pub(crate) fn contribution_view(
        &self,
        caller: &Caller,
        id: &str,
    ) -> Result<ContributionView, ProtocolError> {
        self.read(|r| {
            let mut rec = owned(r, caller, id)?;
            if matches!(rec.view.code, Code::Prepared | Code::Reopened)
                && rec
                    .approval
                    .as_ref()
                    .is_none_or(|a| a.valid_until <= seconds(self) || a.checked_at > seconds(self))
            {
                rec.view.code = Code::FreshnessUnknown;
            }
            if matches!(rec.view.code, Code::Acquiring | Code::Verifying)
                && let Some(progress) = self
                    .inner
                    .contributions
                    .progress
                    .lock()
                    .ok()
                    .and_then(|p| p.get(id).cloned())
            {
                rec.view.received_bytes = Some(
                    progress
                        .load(Ordering::Relaxed)
                        .min(network::TRANSFER as u64),
                );
            }
            Ok(rec.view)
        })
    }
    pub(crate) fn review_contribution(
        &self,
        tx: &mut store::Write<'_>,
        caller: &Caller,
        input: &ContributionInput,
    ) -> store::Result<CommandResult> {
        local(caller)?;
        if input.packet_json.len() > RECORD_BYTES || input.handoff_json.len() > RECORD_BYTES {
            return Err(failure(Code::LimitExceeded).into());
        }
        let key = validate::digest(
            serde_json::to_vec(&(caller.principal, input)).map_err(crate::internal)?,
        );
        if let Some(id) = tx.read().get::<String>("contribution_review", &key)? {
            let mut rec = owned(&tx.read(), caller, &id)?;
            // A canceled operation keeps its fence: neither an acquisition nor
            // a preview already in flight may be revived by reviewing again.
            // Once its acquisition has stopped, create a fresh review below.
            if rec.input == *input && rec.view.code == Code::Canceled && rec.active {
                return Ok(result(rec.view));
            }
            // A fresh review never presents cached preparation as fresh trust.
            if rec.input == *input && rec.view.code != Code::Canceled {
                let check = (|| {
                    let (packet, handoff) = rec.parsed()?;
                    validate::lifetime(&packet, seconds(self))?;
                    validate::compatibility(&handoff)?;
                    if input.host_id != self.inner.host_id {
                        return Err(Code::HostUnavailable);
                    }
                    let path = root_path(&tx.read(), input)?;
                    if let Some(root) = &rec.root {
                        if path != root.path || workspace::bind(&path)? != *root {
                            return Err(Code::RootChanged);
                        }
                        Ok(None)
                    } else {
                        // A transient refusal before root validation has no
                        // binding to preserve. Establish it on the first valid
                        // review; an existing binding is never replaced.
                        self.project_folder(std::path::Path::new(&path))
                            .map_err(|_| Code::RootChanged)?;
                        workspace::bind(&path).map(Some)
                    }
                })();
                if let Ok(Some(root)) = &check {
                    rec.root = Some(root.clone());
                    rec.view.code = Code::AwaitingLocalApproval;
                    save(tx, &rec)?;
                }
                // Reviewing a live or completed operation returns a review
                // result without changing its worker/preparation state.
                rec.view.code = check.err().unwrap_or(Code::AwaitingLocalApproval);
                self.contribution_details(&mut rec);
                return Ok(result(rec.view));
            }
        }
        if tx.read().list::<Guard>("contribution_guard")?.len() >= 1000 {
            return Err(failure(Code::LimitExceeded).into());
        }
        let mut rec = Record {
            view: ContributionView {
                operation_id: uuid::Uuid::now_v7().to_string(),
                code: Code::AwaitingLocalApproval,
                packet_sha256: validate::digest(&input.packet_json),
                handoff_sha256: validate::digest(&input.handoff_json),
                context: None,
                project_id: None,
                dirty: None,
                conflict: None,
                space: None,
                missing_capabilities: vec![],
                received_bytes: None,
                execution_authorized: false,
                interruption_pending: false,
            },
            owner: caller.principal,
            input: input.clone(),
            root: None,
            approval: None,
            manifest: None,
            workspace: None,
            active: false,
            command: None,
            attempts: 0,
            separate: false,
            reopen_as_is: false,
        };
        let check = (|| {
            let (p, h) = rec.parsed()?;
            rec.view.context = Some(ContributionContext {
                game_id: h.game_id.clone(),
                display_name: None,
                release_id: h.release_id.clone(),
                repository_url: h.source.repository_url.clone(),
                commit: h.source.commit.clone(),
                policies: h.policies.clone(),
                task: h.task.clone(),
            });
            validate::lifetime(&p, seconds(self))?;
            validate::compatibility(&h)?;
            if input.host_id != self.inner.host_id {
                return Err(Code::HostUnavailable);
            }
            let path = root_path(&tx.read(), input)?;
            self.project_folder(std::path::Path::new(&path))
                .map_err(|_| Code::RootChanged)?;
            rec.root = Some(workspace::bind(&path)?);
            Ok(())
        })();
        if let Err(code) = check {
            rec.view.code = code;
        }
        self.contribution_details(&mut rec);
        save(tx, &rec)?;
        tx.put("contribution_review", key, &rec.view.operation_id)?;
        Ok(result(rec.view))
    }
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn prepare_contribution(
        &self,
        tx: &mut store::Write<'_>,
        caller: &Caller,
        id: &str,
        approval: &ContributionApproval,
        choice: Option<&ContributionWorkspaceChoice>,
        command: CommandId,
    ) -> store::Result<(CommandResult, bool)> {
        let mut rec = owned(&tx.read(), caller, id)?;
        let prepared = (|| {
            let (packet, mut handoff) = rec.parsed()?;
            validate::approval(
                &packet,
                &handoff,
                &rec.view.packet_sha256,
                approval,
                seconds(self),
            )?;
            validate::compatibility(&handoff)?;
            if rec.input.host_id != self.inner.host_id {
                return Err(Code::HostUnavailable);
            }
            check_saved_root(&tx.read(), &rec)?;
            if rec.active {
                return Ok(Some(rec.view.clone()));
            }
            if matches!(choice, Some(ContributionWorkspaceChoice::CreateSeparate)) && !rec.separate
            {
                let parent = rec.view.operation_id.clone();
                if let Some(child) = tx
                    .read()
                    .get::<String>("contribution_separate", &parent)
                    .map_err(|_| Code::ReconcileRequired)?
                {
                    let input = rec.input.clone();
                    let packet_sha256 = rec.view.packet_sha256.clone();
                    let handoff_sha256 = rec.view.handoff_sha256.clone();
                    rec = owned(&tx.read(), caller, &child).map_err(|_| Code::ReconcileRequired)?;
                    check_saved_root(&tx.read(), &rec)?;
                    if rec.active {
                        return Ok(Some(rec.view.clone()));
                    }
                    rec.input = input;
                    rec.view.packet_sha256 = packet_sha256;
                    rec.view.handoff_sha256 = handoff_sha256;
                } else {
                    if tx
                        .read()
                        .list::<Guard>("contribution_guard")
                        .map_err(|_| Code::ReconcileRequired)?
                        .len()
                        >= 1000
                    {
                        return Err(Code::LimitExceeded);
                    }
                    rec.view.operation_id = uuid::Uuid::now_v7().to_string();
                    rec.view.project_id = None;
                    rec.view.dirty = None;
                    rec.manifest = None;
                    rec.workspace = None;
                    rec.approval = None;
                    rec.attempts = 0;
                    rec.separate = true;
                    rec.reopen_as_is = false;
                    tx.put("contribution_separate", &parent, &rec.view.operation_id)
                        .map_err(|_| Code::ReconcileRequired)?;
                }
            }
            rec.view.context = Some(ContributionContext {
                game_id: handoff.game_id.clone(),
                display_name: approval.display_name.clone(),
                release_id: handoff.release_id.clone(),
                repository_url: handoff.source.repository_url.clone(),
                commit: handoff.source.commit.clone(),
                policies: handoff.policies.clone(),
                task: handoff.task.clone(),
            });
            let packet_key = validate::digest(
                serde_json::to_vec(&(&approval.publisher_id, &packet.packet_id))
                    .map_err(|_| Code::Malformed)?,
            );
            if tx
                .read()
                .get::<String>("contribution_packet", &packet_key)
                .map_err(|_| Code::ReconcileRequired)?
                .is_some_and(|hash| hash != rec.view.packet_sha256)
            {
                return Err(Code::IdConflict);
            }
            // Bind identity before any network/FS effect, including failed attempts.
            tx.put("contribution_packet", &packet_key, &rec.view.packet_sha256)
                .map_err(|_| Code::ReconcileRequired)?;
            handoff.policies.sort();
            let fingerprint =
                validate::digest(serde_json::to_vec(&handoff).map_err(|_| Code::Malformed)?);
            let workspace_key = validate::digest(
                serde_json::to_vec(&(
                    caller.principal,
                    rec.input.host_id,
                    rec.input.root_id,
                    &handoff.source.repository_url,
                ))
                .map_err(|_| Code::Malformed)?,
            );
            let workspace_key = if rec.separate {
                format!("{workspace_key}:{}", rec.view.operation_id)
            } else {
                workspace_key
            };
            if let Some((other, prior_fingerprint)) = tx
                .read()
                .get::<(String, String)>("contribution_workspace", &workspace_key)
                .map_err(|_| Code::ReconcileRequired)?
            {
                let previous: Record = tx
                    .read()
                    .require("contribution", &other)
                    .map_err(|_| Code::ReconcileRequired)?;
                check_saved_root(&tx.read(), &previous)?;
                if let Some(ContributionWorkspaceChoice::UseExisting { project_id }) = choice {
                    if previous.active {
                        return Err(Code::Busy);
                    }
                    let detail = self.contribution_conflict(&tx.read(), &previous);
                    if detail.project_id != Some(*project_id) {
                        return Err(Code::WorkspaceConflict);
                    }
                    rec.manifest = previous.manifest;
                    rec.workspace = previous.workspace;
                    rec.view.project_id = Some(*project_id);
                    rec.view.context = previous.view.context;
                    rec.reopen_as_is = true;
                } else {
                    if prior_fingerprint != fingerprint {
                        rec.view.conflict = Some(self.contribution_conflict(&tx.read(), &previous));
                        return Err(Code::WorkspaceConflict);
                    }
                    if previous.active {
                        return Ok(Some(previous.view));
                    }
                    // Renew trust without changing the existing project or queue.
                    if other != rec.view.operation_id {
                        let input = rec.input.clone();
                        let packet_sha256 = rec.view.packet_sha256.clone();
                        let handoff_sha256 = rec.view.handoff_sha256.clone();
                        rec = previous;
                        rec.input = input;
                        rec.view.packet_sha256 = packet_sha256;
                        rec.view.handoff_sha256 = handoff_sha256;
                    }
                    rec.reopen_as_is = false;
                    if let Some(context) = &mut rec.view.context {
                        context.display_name = approval.display_name.clone();
                    }
                }
            } else {
                if matches!(
                    choice,
                    Some(ContributionWorkspaceChoice::UseExisting { .. })
                ) {
                    return Err(Code::WorkspaceConflict);
                }
                tx.put(
                    "contribution_workspace",
                    workspace_key,
                    &(&rec.view.operation_id, fingerprint),
                )
                .map_err(|_| Code::ReconcileRequired)?;
            }
            if rec.active {
                return Ok(Some(rec.view.clone()));
            }
            if tx
                .read()
                .list::<Guard>("contribution_guard")
                .map_err(|_| Code::ReconcileRequired)?
                .iter()
                .filter(|r| r.active)
                .count()
                >= 2
            {
                return Err(Code::Busy);
            }
            if rec.manifest.is_none() {
                if rec.attempts >= 3 {
                    return Err(Code::ReconcileRequired);
                }
                self.contribution_room(rec.root.as_ref().ok_or(Code::RootChanged)?)?;
                rec.attempts += 1;
                rec.workspace = None; // Earlier incomplete resources are retained, never reused.
            }
            rec.approval = Some(approval.clone());
            rec.view.code = if rec.manifest.is_some() {
                Code::Verifying
            } else {
                Code::Acquiring
            };
            self.contribution_details(&mut rec);
            rec.view.received_bytes = (rec.view.code == Code::Acquiring).then_some(0);
            rec.active = true;
            rec.command = Some(command);
            Ok(None)
        })();
        match prepared {
            Ok(Some(view)) => Ok((result(view), false)),
            Ok(None) => {
                save(tx, &rec)?;
                Ok((result(rec.view), true))
            }
            Err(code) => {
                // Do not invalidate a live worker because a duplicate supplied
                // bad approval. Return this request's refusal independently.
                let active = rec.active;
                rec.view.code = code;
                if code == Code::WorkspaceConflict && rec.view.conflict.is_none() {
                    rec.view.conflict = Some(self.contribution_conflict(&tx.read(), &rec));
                }
                self.contribution_details(&mut rec);
                if !active {
                    save(tx, &rec)?;
                }
                Ok((result(rec.view), false))
            }
        }
    }
    pub(crate) fn cancel_contribution(
        &self,
        tx: &mut store::Write<'_>,
        caller: &Caller,
        id: &str,
    ) -> store::Result<CommandResult> {
        let mut rec = owned(&tx.read(), caller, id)?;
        rec.approval = None;
        rec.view.code = Code::Canceled;
        rec.view.interruption_pending = rec.active;
        self.contribution_details(&mut rec);
        save(tx, &rec)?;
        Ok(result(rec.view))
    }
    fn check_contribution_guard(
        &self,
        r: &store::Read<'_>,
        caller: &Caller,
        id: &str,
        command: CommandId,
    ) -> Result<(), Code> {
        let caller = Self::current_caller(r, caller).map_err(|_| Code::TrustRequired)?;
        local(&caller).map_err(|_| Code::TrustRequired)?;
        if !caller.grants.library_write {
            return Err(Code::TrustRequired);
        }
        let guard: Guard = r
            .require("contribution_guard", id)
            .map_err(|_| Code::ReconcileRequired)?;
        if guard.owner != caller.principal {
            return Err(Code::TrustRequired);
        }
        if guard.code == Code::Canceled {
            return Err(Code::Canceled);
        }
        if !guard.active || guard.command != Some(command) || *self.inner.shutdown.borrow() {
            return Err(Code::ReconcileRequired);
        }
        let now = seconds(self);
        if guard.issued_at.is_none_or(|at| at > now) {
            return Err(Code::NotYetValid);
        }
        if guard.expires_at.is_none_or(|at| at <= now) {
            return Err(Code::Expired);
        }
        if guard.checked_at.is_none_or(|at| at > now)
            || guard.valid_until.is_none_or(|at| at <= now)
        {
            return Err(Code::FreshnessUnknown);
        }
        let root = guard.root.as_ref().ok_or(Code::RootChanged)?;
        let roots: Vec<LibraryRoot> = r
            .get("meta", "library_roots")
            .map_err(|_| Code::RootChanged)?
            .unwrap_or_default();
        if !roots.iter().any(|r| {
            r.root_id == guard.root_id && r.writable && r.holds("") && r.host_path == root.path
        }) || workspace::bind(&root.path)? != *root
        {
            return Err(Code::RootChanged);
        }
        Ok(())
    }
    fn check_contribution(
        &self,
        r: &store::Read<'_>,
        caller: &Caller,
        id: &str,
        command: CommandId,
    ) -> Result<Record, Code> {
        self.check_contribution_guard(r, caller, id, command)?;
        owned(r, caller, id).map_err(|_| Code::ReconcileRequired)
    }
    pub(crate) async fn execute_contribution(
        &self,
        caller: &Caller,
        id: &str,
        command: CommandId,
        deadline: Option<protocol::time::Timestamp>,
    ) -> Result<CommandResult, ProtocolError> {
        let start = Instant::now();
        let check = || -> Result<(), Code> {
            if deadline.is_some_and(|t| self.inner.clock.now() >= t) {
                return Err(Code::Expired);
            }
            if start.elapsed() >= Duration::from_secs(120) {
                return Err(Code::AcquisitionFailed);
            }
            self.read(|r| Ok(self.check_contribution_guard(r, caller, id, command)))
                .map_err(|_| Code::ReconcileRequired)?
        };
        let outcome = async {
            check()?;
            let _slot = self
                .inner
                .contributions
                .slots
                .clone()
                .try_acquire_owned()
                .map_err(|_| Code::Busy)?;
            let rec: Record = self
                .read(|r| r.require("contribution", id))
                .map_err(|_| Code::ReconcileRequired)?;
            let (_, handoff) = rec.parsed()?;
            let manifest = if let Some(manifest) = rec.manifest {
                manifest
            } else {
                let source = self
                    .inner
                    .contributions
                    .source
                    .lock()
                    .map_err(|_| Code::ReconcileRequired)?
                    .clone();
                let received = Arc::new(AtomicU64::new(0));
                self.inner
                    .contributions
                    .progress
                    .lock()
                    .map_err(|_| Code::ReconcileRequired)?
                    .insert(id.into(), received.clone());
                let future = source.pack(
                    &handoff.source.repository_url,
                    &handoff.source.commit,
                    &received,
                );
                tokio::pin!(future);
                let mut interval = tokio::time::interval(Duration::from_millis(100));
                let bytes = loop {
                    tokio::select! {
                        response = &mut future => break response?,
                        _ = interval.tick() => check()?,
                    }
                };
                check()?;
                let hub = self.clone();
                let c = caller.clone();
                let operation = id.to_owned();
                let h = handoff.clone();
                tokio::task::spawn_blocking(move || {
                    let check = || {
                        if deadline.is_some_and(|t| hub.inner.clock.now() >= t) {
                            return Err(Code::Expired);
                        }
                        if start.elapsed() >= Duration::from_secs(120) {
                            return Err(Code::AcquisitionFailed);
                        }
                        hub.read(|r| Ok(hub.check_contribution_guard(r, &c, &operation, command)))
                            .map_err(|_| Code::ReconcileRequired)?
                    };
                    let tree = pack::decode(&bytes, &h.source.commit, &check)?;
                    check()?;
                    let root = rec.root.as_ref().ok_or(Code::RootChanged)?;
                    hub.contribution_room(root)?;
                    let name = format!(".ophio-contribution-{operation}-{}", rec.attempts);
                    let binding = workspace::create(root, &name)?;
                    hub.write(|tx| {
                        let mut current = hub
                            .check_contribution(&tx.read(), &c, &operation, command)
                            .map_err(failure)?;
                        current.workspace = Some(binding.clone());
                        current.view.code = Code::Verifying;
                        save(tx, &current)
                    })
                    .map_err(|_| Code::ReconcileRequired)?;
                    let write_check = || {
                        check()?;
                        hub.contribution_room(root)
                    };
                    let manifest = workspace::materialize(&binding, &tree, &h, &write_check)?;
                    hub.write(|tx| {
                        let mut current = hub
                            .check_contribution(&tx.read(), &c, &operation, command)
                            .map_err(failure)?;
                        current.manifest = Some(manifest.clone());
                        save(tx, &current)
                    })
                    .map_err(|_| Code::ReconcileRequired)?;
                    Ok::<_, Code>(manifest)
                })
                .await
                .map_err(|_| Code::ReconcileRequired)??
            };
            check()?;
            let hub = self.clone();
            let c = caller.clone();
            let operation = id.to_owned();
            let h = handoff.clone();
            let verified = manifest.clone();
            let dirty = tokio::task::spawn_blocking(move || {
                let check = || {
                    if deadline.is_some_and(|t| hub.inner.clock.now() >= t) {
                        return Err(Code::Expired);
                    }
                    if start.elapsed() >= Duration::from_secs(120) {
                        return Err(Code::AcquisitionFailed);
                    }
                    hub.read(|r| Ok(hub.check_contribution_guard(r, &c, &operation, command)))
                        .map_err(|_| Code::ReconcileRequired)?
                };
                if rec.reopen_as_is {
                    // Explicitly reopen the registered project as it is. New
                    // packet policies never replace or authorize its old files.
                    check()?;
                    if workspace::bind(&verified.binding.path)? != verified.binding {
                        return Err(Code::RootChanged);
                    }
                    Ok(workspace::details(&verified, &check)
                        .map(|d| d.changed != Some(0) || d.untracked != Some(0))
                        .unwrap_or(true))
                } else {
                    workspace::verify(&verified, &h, &check)
                }
            })
            .await
            .map_err(|_| Code::ReconcileRequired)??;
            check()?;
            self.write(|tx| {
                if deadline.is_some_and(|t| self.inner.clock.now() >= t) {
                    return Err(failure(Code::Expired).into());
                }
                if start.elapsed() >= Duration::from_secs(120) {
                    return Err(failure(Code::AcquisitionFailed).into());
                }
                let mut rec = self
                    .check_contribution(&tx.read(), caller, id, command)
                    .map_err(failure)?;
                if workspace::bind(&manifest.binding.path).map_err(failure)? != manifest.binding {
                    return Err(failure(Code::WorkspaceConflict).into());
                }
                if let Some(project_id) = rec.view.project_id {
                    let project: Project = tx
                        .read()
                        .get("project", project_id)?
                        .ok_or_else(|| failure(Code::ReconcileRequired))?;
                    if project.host_path != manifest.binding.path {
                        return Err(failure(Code::WorkspaceConflict).into());
                    }
                    // A duplicate open never writes an existing project's queue.
                    rec.view.code = Code::Reopened;
                } else {
                    if dirty
                        || tx
                            .read()
                            .list::<Project>("project")?
                            .iter()
                            .any(|p| p.host_path == manifest.binding.path)
                    {
                        return Err(failure(Code::WorkspaceConflict).into());
                    }
                    let project_id = ProjectId::new();
                    let project = Project {
                        project_id,
                        name: rec
                            .approval
                            .as_ref()
                            .and_then(|a| a.display_name.clone())
                            .unwrap_or_else(|| handoff.game_id.clone()),
                        host_path: manifest.binding.path.clone(),
                        default_harness: None,
                        default_model: None,
                        default_effort: None,
                        active_writer: None,
                        stage: None,
                        one_off: false,
                        home: false,
                        vcs: Some(VcsInfo {
                            kind: "git".into(),
                            branch: Some("contribution".into()),
                            head: Some(handoff.source.commit.clone()),
                            dirty_files: 0,
                        }),
                        created_at: self.inner.clock.now(),
                    };
                    let queue = QueueView {
                        project_id,
                        revision: 1,
                        paused: true,
                        pause_reason: None,
                        writer: None,
                        items: vec![],
                    };
                    tx.put("project", project_id, &project)?;
                    crate::history::admit_projects(&tx.read())?;
                    tx.emit(Event::ProjectUpserted { project })?;
                    tx.put("queue", project_id, &queue)?;
                    tx.emit(Event::QueueChanged { queue })?;
                    rec.view.project_id = Some(project_id);
                    rec.view.code = Code::Prepared;
                }
                rec.view.dirty = Some(dirty);
                self.contribution_details(&mut rec);
                rec.active = false;
                rec.view.interruption_pending = false;
                save(tx, &rec)?;
                Ok(rec.view)
            })
            .map_err(|_| Code::ReconcileRequired)
        }
        .await;
        let view = match outcome {
            Ok(view) => view,
            Err(code) => self.write(|tx| {
                let mut rec: Record = tx.read().require("contribution", id)?;
                if rec.command == Some(command) {
                    if rec.view.code != Code::Canceled {
                        rec.view.code = code;
                    }
                    rec.active = false;
                    rec.view.interruption_pending = false;
                    rec.approval = None;
                    if rec.view.code == Code::WorkspaceConflict {
                        rec.view.conflict = Some(self.contribution_conflict(&tx.read(), &rec));
                    }
                    self.contribution_details(&mut rec);
                    save(tx, &rec)?;
                }
                Ok(rec.view)
            })?,
        };
        if let Ok(mut progress) = self.inner.contributions.progress.lock() {
            progress.remove(id);
        }
        Ok(result(view))
    }
}
