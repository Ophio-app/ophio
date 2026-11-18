// SPDX-License-Identifier: Apache-2.0
//! Agent release checks never run an agent. Catalog changes are learned from
//! the normal catalog observation, after a confirmed update has landed.
use crate::{CompanionHub, error};
use protocol::{
    agent_updates::*,
    capability::{HarnessInfo, HarnessKind},
    model::{AttentionItem, AttentionKind},
    time::Timestamp,
    *,
};
use serde::{Deserialize, Serialize};
use std::{sync::atomic::Ordering, time::Duration};

pub(crate) const EVERY: Duration = Duration::from_secs(6 * 60 * 60);
const DAY: i64 = 24 * 60 * 60 * 1000;
const KEY: &str = "agent_updates";
// A separate transactional record shares the plain reminder budget across agents.
const NUDGE_KEY: &str = "agent_update_nudge";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Stored {
    #[serde(flatten, serialize_with = "rollback_view")]
    view: AgentUpdatesView,
    #[serde(default)]
    tried_at: Option<Timestamp>,
    #[serde(default)]
    notices: Vec<Notices>,
}
// Storage and the phone's compatibility projection have different meanings:
// 0.2.0's stored false removes managed copies, while every current mode keeps
// them. Leave the wire view unchanged (true only for Automatic). A rollback
// necessarily resumes automatic updates because 0.2.0 has no keep-only mode.
fn rollback_view<S: serde::Serializer>(
    view: &AgentUpdatesView,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    let mut legacy = view.clone();
    legacy.enabled = true;
    legacy.serialize(serializer)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Notices {
    harness: HarnessKind,
    behind_since: Option<Timestamp>,
    last_nudge: Option<Timestamp>,
    installed: Option<String>,
    #[serde(default)]
    active: Vec<AttentionId>,
    #[serde(default)]
    told: Vec<String>,
    pending: Option<PendingModels>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PendingModels {
    version: String,
    before: Vec<String>,
}

/// Explicit modes win. Only an old persisted true opts into automation;
/// an old false (or no record) migrates to the new Notify default.
fn load(r: &store::Read<'_>) -> store::Result<Stored> {
    let Some(mut value) = r.get::<serde_json::Value>("meta", KEY)? else {
        return Ok(Stored::default());
    };
    if !value.is_object() {
        return Ok(Stored::default());
    }
    if value.get("mode").is_none() {
        value["mode"] = serde_json::json!(if value.get("enabled").and_then(|v| v.as_bool())
            == Some(true)
        {
            "automatic"
        } else {
            "notify"
        });
    }
    let mut stored: Stored = serde_json::from_value(value).unwrap_or_else(|_| {
        tracing::warn!(
            "Agent update settings cannot be read; using Notify without automatic installs"
        );
        Stored::default()
    });
    stored.view.enabled = stored.view.mode == AgentUpdateMode::Automatic;
    for copy in &mut stored.view.agents {
        if copy.version.is_some() {
            copy.managed = true;
            copy.installed_version = copy.version.clone();
        }
    }
    Ok(stored)
}

fn scheduled_due(stored: &Stored, now: Timestamp) -> bool {
    stored.view.mode != AgentUpdateMode::Never
        && stored
            .tried_at
            .is_none_or(|at| at > now || now.0.saturating_sub(at.0) >= EVERY.as_millis() as i64)
}

#[derive(Clone, Copy)]
pub(crate) enum Request {
    Scheduled,
    Check,
    Update(HarnessKind),
}
struct Checking(CompanionHub);
impl Drop for Checking {
    fn drop(&mut self) {
        self.0.inner.agent_checks.fetch_sub(1, Ordering::SeqCst);
    }
}

impl CompanionHub {
    pub(crate) fn agent_updates_view(&self) -> Result<AgentUpdatesView, ProtocolError> {
        let mut view = self.read(load)?.view;
        view.supported = self.agent_copies_supported();
        view.checking = self.inner.agent_checks.load(Ordering::SeqCst) > 0;
        let infos: Vec<HarnessInfo> = self
            .read(|r| r.get("meta", "harnesses"))?
            .unwrap_or_default();
        for info in infos
            .iter()
            .filter(|i| matches!(i.kind, HarnessKind::ClaudeCode | HarnessKind::Codex))
        {
            if !view.agents.iter().any(|a| a.harness == info.kind) {
                view.agents.push(AgentCopy {
                    harness: info.kind,
                    version: None,
                    installed_version: observed_version(info),
                    latest_version: None,
                    // The per-agent updater, not a general host capability,
                    // establishes whether this installation can be updated.
                    update_supported: false,
                    managed: false,
                    released_models: vec![],
                    checked_at: None,
                    problem: None,
                });
            }
        }
        for agent in &mut view.agents {
            if agent.version.is_none() {
                agent.installed_version = infos
                    .iter()
                    .find(|i| i.kind == agent.harness)
                    .and_then(observed_version);
            }
        }
        Ok(view)
    }
    pub(crate) fn set_agent_updates(
        &self,
        tx: &mut store::Write<'_>,
        enabled: bool,
        revision: Option<u64>,
    ) -> store::Result<()> {
        self.set_agent_update_mode(
            tx,
            if enabled {
                AgentUpdateMode::Automatic
            } else {
                AgentUpdateMode::Never
            },
            revision,
        )
    }
    pub(crate) fn set_agent_update_mode(
        &self,
        tx: &mut store::Write<'_>,
        mode: AgentUpdateMode,
        revision: Option<u64>,
    ) -> store::Result<()> {
        let mut stored = load(&tx.read())?;
        domain::revision(revision, stored.view.revision)?;
        if mode == AgentUpdateMode::Automatic && !self.agent_copies_supported() {
            return Err(error(
                ErrorCode::Unsupported,
                "Agent updates are not supported on this computer.",
            )
            .into());
        }
        if stored.view.mode != mode {
            stored.tried_at = None;
        }
        stored.view.mode = mode;
        stored.view.enabled = mode == AgentUpdateMode::Automatic;
        stored.view.revision += 1;
        if mode == AgentUpdateMode::Never {
            for notices in &mut stored.notices {
                clear(tx, notices, self.inner.clock.now())?;
            }
        }
        tx.put("meta", KEY, &stored)
    }
    pub(crate) fn validate_agent_request(
        &self,
        tx: &mut store::Write<'_>,
        harness: Option<HarnessKind>,
    ) -> store::Result<()> {
        if self.inner.agent_updater.is_none()
            || (harness.is_some() && !self.agent_copies_supported())
        {
            return Err(error(
                ErrorCode::Unsupported,
                "Agent updates are not supported on this computer.",
            )
            .into());
        }
        if let Some(kind) = harness {
            if !matches!(kind, HarnessKind::ClaudeCode | HarnessKind::Codex) {
                return Err(
                    error(ErrorCode::Unsupported, "Ophio cannot update this agent.").into(),
                );
            }
            let stored = load(&tx.read())?;
            let infos: Vec<HarnessInfo> = tx.read().get("meta", "harnesses")?.unwrap_or_default();
            if !stored
                .view
                .agents
                .iter()
                .any(|a| a.harness == kind && a.update_supported)
                && !infos
                    .iter()
                    .any(|i| i.kind == kind && i.installed.is_available())
            {
                return Err(error(
                    ErrorCode::Unsupported,
                    "Install this agent on the computer before updating it in Ophio.",
                )
                .into());
            }
        }
        Ok(())
    }
    fn agent_copies_supported(&self) -> bool {
        self.inner
            .agent_updater
            .as_ref()
            .is_some_and(|u| u.supported())
    }
    pub(crate) fn update_agents(&self) {
        self.request_agent_updates(Request::Scheduled);
    }
    pub(crate) fn request_agent_updates(&self, request: Request) {
        let Some(updater) = self.inner.agent_updater.clone() else {
            return;
        };
        self.inner.agent_checks.fetch_add(1, Ordering::SeqCst);
        let checking = Checking(self.clone());
        let mut shutdown = self.inner.shutdown.subscribe();
        self.spawn(async move {
            let hub = checking.0.clone();
            let work = async {
                let _one = hub.inner.agent_updates.lock().await;
                let now = hub.inner.clock.now();
                let stored = hub.read(load)?;
                let before_infos: Vec<HarnessInfo> = hub.read(|r| r.get("meta", "harnesses"))?.unwrap_or_default();
                if matches!(request, Request::Scheduled) && !scheduled_due(&stored, now) { return Ok(()); }
                // Persist before asking, so restarting does not create a check loop.
                hub.write(|tx| { let mut s = load(&tx.read())?; s.tried_at = Some(now); tx.put("meta", KEY, &s) })?;
                let versions: Vec<_> = before_infos.iter().filter_map(|info| {
                    observed_version(info).map(|version| (info.kind, version))
                }).collect();
                updater.observe_versions(&versions);
                let states = match request {
                    Request::Update(kind) => updater.update(kind).await,
                    Request::Scheduled if stored.view.mode == AgentUpdateMode::Automatic => updater.apply(true).await,
                    _ => updater.check().await,
                };
                let now = hub.inner.clock.now();
                hub.write(|tx| {
                    let mut saved = load(&tx.read())?;
                    let infos: Vec<HarnessInfo> = tx.read().get("meta", "harnesses")?.unwrap_or_default();
                    for state in states {
                        let old = saved.view.agents.iter().find(|a| a.harness == state.harness).cloned();
                        let info = infos.iter().find(|i| i.kind == state.harness);
                        let before = before_infos.iter().find(|i| i.kind == state.harness);
                        let installed = state.version.clone().or_else(|| info.and_then(observed_version));
                        let copy = AgentCopy { harness: state.harness, managed: state.version.is_some(), installed_version: installed, version: state.version, latest_version: state.latest_version.or_else(|| old.as_ref().and_then(|a| a.latest_version.clone())), update_supported: state.update_supported, released_models: if state.checked { state.released_models } else { old.as_ref().map(|a| a.released_models.clone()).unwrap_or_default() }, checked_at: if state.checked { Some(now) } else { old.as_ref().and_then(|a| a.checked_at) }, problem: state.problem };
                        if !saved.notices.iter().any(|n| n.harness == copy.harness) {
                            saved.notices.push(Notices { harness: copy.harness, behind_since: None, last_nudge: None, installed: old.as_ref().and_then(|a| a.installed_version.clone()).or_else(|| before.and_then(observed_version)), active: vec![], told: vec![], pending: None });
                        }
                        let notices = saved.notices.iter_mut().find(|n| n.harness == copy.harness).unwrap();
                        found(tx, notices, &copy, before, saved.view.mode, now)?;
                        saved.view.agents.retain(|a| a.harness != copy.harness);
                        saved.view.agents.push(copy);
                    }
                    tx.put("meta", KEY, &saved)?;
                    // A normal observation may have finished during the download.
                    // Consume it now if it already names the new program, without
                    // launching a second observation.
                    catalog_seen(tx, &infos, now)
                })
            };
            tokio::select! { _ = shutdown.changed() => {}, result = work => if let Err(e) = result { tracing::error!(code = ?e.code, "checking or updating agents failed"); } }
            drop(checking);
        });
    }
}

fn found(
    tx: &mut store::Write<'_>,
    notices: &mut Notices,
    copy: &AgentCopy,
    info: Option<&HarnessInfo>,
    mode: AgentUpdateMode,
    now: Timestamp,
) -> store::Result<()> {
    // A failed probe is not an uninstall or an update. The public copy may
    // honestly say "unknown", but reminder history keeps its last known version.
    // Both release checks and ordinary catalog observations come through here.
    if copy.installed_version.is_none() {
        if mode == AgentUpdateMode::Never {
            clear(tx, notices, now)?;
        }
        return Ok(());
    }
    let landed = notices.installed != copy.installed_version;
    if landed {
        if notices.installed.is_some() {
            tx.put("meta", NUDGE_KEY, &Option::<Timestamp>::None)?;
        }
        clear(tx, notices, now)?;
        notices.behind_since = None;
        notices.last_nudge = None;
        // Several updates can land before the normal catalog next answers.
        // Keep the earliest real baseline rather than replacing it with an
        // intermediate or unknown observation.
        let previous = notices.pending.take().map(|pending| pending.before);
        notices.pending = copy.version.as_ref().and_then(|version| {
            let before = previous.or_else(|| {
                let info = info.filter(|i| {
                    i.catalog
                        .as_ref()
                        .is_some_and(|c| c.availability.is_available())
                        && !i.models.is_empty()
                        && observed_version(i) != copy.version
                })?;
                Some(info.models.iter().map(|m| m.id.clone()).collect())
            })?;
            Some(PendingModels {
                version: version.clone(),
                before,
            })
        });
        // An intermediate patch does not make an already-announced model news
        // again. Keep model IDs even when the installed agent version changes.
    }
    notices.installed = copy.installed_version.clone();
    let behind = copy
        .latest_version
        .as_ref()
        .zip(copy.installed_version.as_ref())
        .is_some_and(|(latest, installed)| {
            protocol::releases::newer_than(latest, installed).is_some()
        });
    if !behind || mode == AgentUpdateMode::Never {
        if mode == AgentUpdateMode::Never {
            clear(tx, notices, now)?;
        } else {
            clear_notices(tx, notices, now, true)?;
        }
        notices.behind_since = None;
        return Ok(());
    }
    let since = *notices.behind_since.get_or_insert(now);
    // Wall-clock corrections cannot postpone a reminder indefinitely.
    if since > now {
        notices.behind_since = Some(now);
    }
    if notices.last_nudge.is_some_and(|at| at > now) {
        notices.last_nudge = Some(now);
    }
    let mut models: Vec<_> = copy
        .released_models
        .iter()
        .filter(|id| info.is_none_or(|i| !i.models.iter().any(|m| &m.id == *id)))
        .cloned()
        .collect();
    models.sort();
    models.dedup();
    if mode == AgentUpdateMode::Notify && !models.is_empty() {
        let untold: Vec<_> = models
            .into_iter()
            .filter(|id| !notices.told.contains(&format!("model:{id}")))
            .collect();
        if !untold.is_empty() {
            clear(tx, notices, now)?;
            let id = notice(
                tx,
                AgentUpdateNotice {
                    harness: copy.harness,
                    kind: AgentUpdateNoticeKind::UpdateForModels,
                    version: copy.latest_version.clone(),
                    models: untold.clone(),
                },
                format!("Update {} for {}", name(copy.harness), model_names(&untold)),
                format!(
                    "The new release announces {}. Update the agent to check which models your account can use.",
                    model_names(&untold)
                ),
                now,
            )?;
            notices.active.push(id);
            notices
                .told
                .extend(untold.iter().map(|id| format!("model:{id}")));
            notices.last_nudge = Some(now);
        }
    } else if mode == AgentUpdateMode::Notify
        && now.0.saturating_sub(notices.behind_since.unwrap_or(now).0) >= 14 * DAY
        && notices
            .last_nudge
            .is_none_or(|at| now.0.saturating_sub(at.0) >= 30 * DAY)
        && computer_nudge_due(tx, now)?
    {
        clear(tx, notices, now)?;
        let id = notice(tx, AgentUpdateNotice { harness: copy.harness, kind: AgentUpdateNoticeKind::Behind, version: copy.latest_version.clone(), models: vec![] }, format!("{} has an update", name(copy.harness)), "This agent has been behind for at least two weeks. Review its update details in Settings.".into(), now)?;
        notices.active.push(id);
        notices.last_nudge = Some(now);
        tx.put("meta", NUDGE_KEY, &Some(now))?;
    }
    Ok(())
}

fn computer_nudge_due(tx: &mut store::Write<'_>, now: Timestamp) -> store::Result<bool> {
    let last = tx
        .read()
        .get::<Option<Timestamp>>("meta", NUDGE_KEY)?
        .flatten();
    if last.is_some_and(|at| at > now) {
        tx.put("meta", NUDGE_KEY, &Some(now))?;
        return Ok(false);
    }
    Ok(last.is_none_or(|at| now.0.saturating_sub(at.0) >= 30 * DAY))
}

/// Called only by the existing catalog refresh, never by a release check.
pub(crate) fn catalog_seen(
    tx: &mut store::Write<'_>,
    infos: &[HarnessInfo],
    now: Timestamp,
) -> store::Result<()> {
    let mut stored = load(&tx.read())?;
    let mut changed = false;
    for notices in &mut stored.notices {
        // A system installation can change outside Ophio. Its ordinary
        // observation clears obsolete update requests too.
        if let Some(copy) = stored
            .view
            .agents
            .iter_mut()
            .find(|c| c.harness == notices.harness && !c.managed)
            && let Some(info) = infos.iter().find(|i| i.kind == notices.harness)
            && copy.installed_version != observed_version(info)
        {
            copy.installed_version = observed_version(info);
            found(tx, notices, copy, Some(info), stored.view.mode, now)?;
            changed = true;
        }
        let Some(pending) = notices.pending.as_ref() else {
            continue;
        };
        let Some(info) = infos.iter().find(|i| {
            i.kind == notices.harness
                && observed_version(i).as_deref() == Some(pending.version.as_str())
                && i.catalog
                    .as_ref()
                    .is_some_and(|c| c.availability.is_available())
        }) else {
            continue;
        };
        let mut added: Vec<_> = info
            .models
            .iter()
            .map(|m| m.id.clone())
            .filter(|id| !pending.before.contains(id))
            .collect();
        added.sort();
        added.dedup();
        let key = format!("available:{}:{}", pending.version, added.join(","));
        if stored.view.mode != AgentUpdateMode::Never
            && !added.is_empty()
            && !notices.told.contains(&key)
        {
            let id = notice(
                tx,
                AgentUpdateNotice {
                    harness: notices.harness,
                    kind: AgentUpdateNoticeKind::NewModelsAvailable,
                    version: Some(pending.version.clone()),
                    models: added.clone(),
                },
                format!("New in {}: {}", name(notices.harness), model_names(&added)),
                format!(
                    "After the update, this agent now lists {} for your account.",
                    model_names(&added)
                ),
                now,
            )?;
            notices.active.push(id);
            notices.told.push(key);
        }
        notices.pending = None;
        changed = true;
    }
    if changed {
        tx.put("meta", KEY, &stored)?;
    }
    Ok(())
}
fn observed_version(info: &HarnessInfo) -> Option<String> {
    ports::agent_copy::normalized_version(info.kind, info.native_version.as_deref()?)
}
fn model_names(ids: &[String]) -> String {
    ids.iter()
        .map(|id| ports::model_name::model_name(id))
        .collect::<Vec<_>>()
        .join(", ")
}
fn name(harness: HarnessKind) -> &'static str {
    match harness {
        HarnessKind::ClaudeCode => "Claude Code",
        HarnessKind::Codex => "Codex",
        _ => "this agent",
    }
}
fn clear(tx: &mut store::Write<'_>, notices: &mut Notices, now: Timestamp) -> store::Result<()> {
    clear_notices(tx, notices, now, false)
}
fn clear_notices(
    tx: &mut store::Write<'_>,
    notices: &mut Notices,
    now: Timestamp,
    keep_available: bool,
) -> store::Result<()> {
    let mut keep = Vec::new();
    for id in notices.active.drain(..) {
        if let Some(mut item) = tx.read().get::<AttentionItem>("attention", id)?
            && item.resolved_at.is_none()
        {
            if keep_available
                && item
                    .agent_update
                    .as_ref()
                    .is_some_and(|n| n.kind == AgentUpdateNoticeKind::NewModelsAvailable)
            {
                keep.push(id);
                continue;
            }
            item.resolved_at = Some(now);
            tx.put("attention", id, &item)?;
            tx.emit(Event::AttentionResolved { attention_id: id })?;
        }
    }
    notices.active = keep;
    Ok(())
}
fn notice(
    tx: &mut store::Write<'_>,
    agent_update: AgentUpdateNotice,
    title: String,
    body: String,
    now: Timestamp,
) -> store::Result<AttentionId> {
    let computer = tx
        .read()
        .get::<protocol::model::HostInfo>("meta", "host")?
        .map(|host| host.name)
        .unwrap_or_else(|| "this computer".into());
    let title = format!("{title} on {computer}");
    let item = AttentionItem {
        child_id: None,
        attention_id: AttentionId::new(),
        kind: AttentionKind::Completed,
        title,
        body: Some(body),
        task_id: None,
        run_id: None,
        approval_id: None,
        created_at: now,
        resolved_at: None,
        priority: 1,
        topic: None,
        quiet: false,
        report_id: None,
        account_id: None,
        alert: None,
        alert_handled_at: None,
        new_version: None,
        agent_update: Some(agent_update),
    };
    let id = item.attention_id;
    tx.put("attention", id, &item)?;
    tx.emit(Event::AttentionUpserted { item })?;
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ports::HarnessAdapter;
    struct Db {
        store: store::Store,
        _dir: tempfile::TempDir,
    }
    impl std::ops::Deref for Db {
        type Target = store::Store;
        fn deref(&self) -> &Self::Target {
            &self.store
        }
    }
    impl std::ops::DerefMut for Db {
        fn deref_mut(&mut self) -> &mut Self::Target {
            &mut self.store
        }
    }
    fn db() -> Db {
        let dir = tempfile::tempdir().unwrap();
        let store = store::Store::open(dir.path().join("test.sqlite"), store::Retention::default())
            .unwrap();
        Db { store, _dir: dir }
    }
    fn copy() -> AgentCopy {
        AgentCopy {
            harness: HarnessKind::ClaudeCode,
            version: None,
            installed_version: Some("1.0.0".into()),
            latest_version: Some("2.0.0".into()),
            update_supported: true,
            managed: false,
            released_models: vec![],
            checked_at: Some(Timestamp(0)),
            problem: None,
        }
    }
    fn notices() -> Notices {
        Notices {
            harness: HarnessKind::ClaudeCode,
            behind_since: None,
            last_nudge: None,
            installed: Some("1.0.0".into()),
            active: vec![],
            told: vec![],
            pending: None,
        }
    }
    fn items(db: &mut store::Store) -> Vec<AttentionItem> {
        db.read(|r| r.list("attention")).unwrap()
    }
    // Faithfully reproduced storage readers from 755d609d:
    // crates/protocol/src/agent_updates.rs (AgentUpdatesView and AgentCopy).
    // Unknown fields are ignored, and every view field is required.
    #[derive(Default, Serialize, Deserialize)]
    struct Release020View {
        enabled: bool,
        revision: u64,
        supported: bool,
        checking: bool,
        agents: Vec<Release020Copy>,
    }
    #[derive(Serialize, Deserialize)]
    struct Release020Copy {
        harness: HarnessKind,
        version: Option<String>,
        checked_at: Option<Timestamp>,
        problem: Option<String>,
    }
    fn stored_bytes(db: &Db) -> Vec<u8> {
        // Read the exact JSON text written by tx.put, without reserializing it.
        let connection = rusqlite::Connection::open(db._dir.path().join("test.sqlite")).unwrap();
        let json: String = connection
            .query_row(
                "SELECT json FROM documents WHERE kind = 'meta' AND id = 'agent_updates'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        json.into_bytes()
    }
    #[test]
    fn rollback_020_keeps_copies_for_every_current_choice() {
        for mode in [
            AgentUpdateMode::Automatic,
            AgentUpdateMode::Notify,
            AgentUpdateMode::Never,
        ] {
            let mut db = db();
            let mut agent = copy();
            agent.version = Some("1.0.0".into());
            let saved = Stored {
                view: AgentUpdatesView {
                    mode,
                    enabled: mode == AgentUpdateMode::Automatic,
                    agents: vec![agent],
                    ..AgentUpdatesView::default()
                },
                ..Stored::default()
            };
            db.write(Timestamp(0), |tx| tx.put("meta", KEY, &saved))
                .unwrap();
            let bytes = stored_bytes(&db);
            let old: Release020View = serde_json::from_slice(&bytes).unwrap();
            // 755d609d services/src/agent_updates.rs passes precisely this bool
            // to updater.apply; companion/src/agent_updates.rs::agent removes
            // CURRENT and prunes the copies when it is false.
            assert!(old.enabled, "{mode:?} must not ask 0.2.0 to remove copies");
            assert_eq!(old.agents[0].version.as_deref(), Some("1.0.0"));
            assert_eq!(db.read(load).unwrap().view.mode, mode);
            // A 0.2.0 periodic check rewrites its parsed view, dropping mode.
            db.write(Timestamp(1), |tx| tx.put("meta", KEY, &old))
                .unwrap();
            assert!(
                !serde_json::from_slice::<serde_json::Value>(&stored_bytes(&db))
                    .unwrap()
                    .as_object()
                    .unwrap()
                    .contains_key("mode")
            );
            assert_eq!(db.read(load).unwrap().view.mode, AgentUpdateMode::Automatic);
            // Its actual off command still means off there; main's existing
            // migration uses Notify, never a new deletion request.
            let old = Release020View {
                enabled: false,
                ..old
            };
            db.write(Timestamp(2), |tx| tx.put("meta", KEY, &old))
                .unwrap();
            assert_eq!(db.read(load).unwrap().view.mode, AgentUpdateMode::Notify);
        }
    }
    #[test]
    fn rollback_020_default_and_legacy_migrations_write_safe_bytes() {
        let mut db = db();
        for legacy in [None, Some(true), Some(false)] {
            if let Some(enabled) = legacy {
                db.write(Timestamp(0), |tx| {
                    tx.put(
                        "meta",
                        KEY,
                        &Release020View {
                            enabled,
                            ..Release020View::default()
                        },
                    )
                })
                .unwrap();
            }
            let saved = db.read(load).unwrap();
            let mode = if legacy == Some(true) {
                AgentUpdateMode::Automatic
            } else {
                AgentUpdateMode::Notify
            };
            assert_eq!(saved.view.mode, mode);
            db.write(Timestamp(1), |tx| tx.put("meta", KEY, &saved))
                .unwrap();
            let old: Release020View = serde_json::from_slice(&stored_bytes(&db)).unwrap();
            assert!(
                old.enabled,
                "migration must not request removal on rollback"
            );
            assert_eq!(db.read(load).unwrap().view.mode, mode);
        }
    }
    #[test]
    fn legacy_policy_migration_and_projection() {
        let mut db = db();
        assert_eq!(db.read(load).unwrap().view.mode, AgentUpdateMode::Notify);
        for (value, mode) in [
            (
                serde_json::json!({"enabled":true}),
                AgentUpdateMode::Automatic,
            ),
            (
                serde_json::json!({"enabled":false}),
                AgentUpdateMode::Notify,
            ),
            (serde_json::json!({}), AgentUpdateMode::Notify),
            (
                serde_json::json!({"enabled":true,"mode":"never"}),
                AgentUpdateMode::Never,
            ),
        ] {
            db.write(Timestamp(0), |tx| tx.put("meta", KEY, &value))
                .unwrap();
            let saved = db.read(load).unwrap();
            assert_eq!(saved.view.mode, mode);
            assert_eq!(saved.view.enabled, mode == AgentUpdateMode::Automatic);
        }
    }
    #[test]
    fn scheduled_checks_follow_wall_clock_and_never_policy() {
        let mut stored = Stored::default();
        assert!(scheduled_due(&stored, Timestamp(0)));
        stored.tried_at = Some(Timestamp(100));
        let every = EVERY.as_millis() as i64;
        assert!(!scheduled_due(&stored, Timestamp(100 + every - 1)));
        assert!(scheduled_due(&stored, Timestamp(100 + every)));
        assert!(
            scheduled_due(&stored, Timestamp(0)),
            "a corrected wall clock must not postpone checks forever"
        );
        stored.view.mode = AgentUpdateMode::Never;
        assert!(!scheduled_due(&stored, Timestamp(100 * DAY)));
    }

    #[test]
    fn no_daily_nags_and_dismissal_survives_reloading() {
        let mut db = db();
        let mut n = notices();
        let c = copy();
        for day in 0..14 {
            db.write(Timestamp(day * DAY), |tx| {
                found(
                    tx,
                    &mut n,
                    &c,
                    None,
                    AgentUpdateMode::Notify,
                    Timestamp(day * DAY),
                )
            })
            .unwrap();
            assert!(items(&mut db).is_empty());
        }
        db.write(Timestamp(14 * DAY), |tx| {
            found(
                tx,
                &mut n,
                &c,
                None,
                AgentUpdateMode::Notify,
                Timestamp(14 * DAY),
            )
        })
        .unwrap();
        assert_eq!(items(&mut db).len(), 1);
        let id = n.active[0];
        db.write(Timestamp(15 * DAY), |tx| {
            let mut item: AttentionItem = tx.read().require("attention", id)?;
            item.resolved_at = Some(Timestamp(15 * DAY));
            tx.put("attention", id, &item)?;
            tx.put("meta", "test-notices", &n)
        })
        .unwrap();
        n = db.read(|r| r.require("meta", "test-notices")).unwrap();
        for day in 15..44 {
            db.write(Timestamp(day * DAY), |tx| {
                found(
                    tx,
                    &mut n,
                    &c,
                    None,
                    AgentUpdateMode::Notify,
                    Timestamp(day * DAY),
                )
            })
            .unwrap();
            assert_eq!(items(&mut db).len(), 1);
        }
        db.write(Timestamp(44 * DAY), |tx| {
            found(
                tx,
                &mut n,
                &c,
                None,
                AgentUpdateMode::Notify,
                Timestamp(44 * DAY),
            )
        })
        .unwrap();
        assert_eq!(items(&mut db).len(), 2);
        let mut landed = c;
        landed.installed_version = Some("2.0.0".into());
        db.write(Timestamp(45 * DAY), |tx| {
            found(
                tx,
                &mut n,
                &landed,
                None,
                AgentUpdateMode::Notify,
                Timestamp(45 * DAY),
            )
        })
        .unwrap();
        assert!(items(&mut db).iter().all(|i| i.resolved_at.is_some()));
        assert!(n.behind_since.is_none());
        assert!(n.last_nudge.is_none());
    }
    #[test]
    fn release_model_notice_is_once_and_never_mode_is_silent() {
        let mut db = db();
        let mut n = notices();
        let mut c = copy();
        c.released_models = vec!["announced-model".into()];
        for day in [0, 1, 14, 44, 90] {
            db.write(Timestamp(day * DAY), |tx| {
                found(
                    tx,
                    &mut n,
                    &c,
                    None,
                    AgentUpdateMode::Notify,
                    Timestamp(day * DAY),
                )
            })
            .unwrap();
        }
        assert_eq!(items(&mut db).len(), 1);
        assert!(
            items(&mut db)[0]
                .body
                .as_ref()
                .unwrap()
                .contains("check which models your account can use")
        );
        db.write(Timestamp(91 * DAY), |tx| {
            found(
                tx,
                &mut n,
                &c,
                None,
                AgentUpdateMode::Never,
                Timestamp(91 * DAY),
            )
        })
        .unwrap();
        assert!(items(&mut db)[0].resolved_at.is_some());
    }
    async fn info(version: &str, models: &[&str]) -> HarnessInfo {
        let mut info = crate::testing::FakeHarness::default().probe().await;
        info.native_version = Some(version.into());
        info.models = models
            .iter()
            .map(|id| protocol::capability::ModelOption {
                id: (*id).into(),
                label: (*id).into(),
                suggested_for_computer_use: false,
                is_default: false,
                details: None,
            })
            .collect();
        info.catalog = Some(protocol::capability::CatalogObservation {
            checked_at: Timestamp(0),
            source: "native test catalog".into(),
            availability: protocol::capability::Availability::Available,
            default_model: None,
            default_effort: None,
        });
        info
    }
    #[tokio::test]
    async fn model_news_requires_real_post_update_catalog_diff() {
        let mut db = db();
        let mut n = notices();
        let before = info("1.0.0 (Claude Code)", &["already-there"]).await;
        let mut c = copy();
        c.version = Some("2.0.0".into());
        c.installed_version = c.version.clone();
        c.released_models = vec!["speculative-model".into()];
        db.write(Timestamp(0), |tx| {
            found(
                tx,
                &mut n,
                &c,
                Some(&before),
                AgentUpdateMode::Notify,
                Timestamp(0),
            )?;
            tx.put(
                "meta",
                KEY,
                &Stored {
                    notices: vec![n.clone()],
                    ..Stored::default()
                },
            )
        })
        .unwrap();
        db.write(Timestamp(1), |tx| catalog_seen(tx, &[before], Timestamp(1)))
            .unwrap();
        assert!(items(&mut db).is_empty());
        let after = info("2.0.0 (Claude Code)", &["already-there", "actually-added"]).await;
        db.write(Timestamp(2), |tx| {
            catalog_seen(tx, std::slice::from_ref(&after), Timestamp(2))
        })
        .unwrap();
        let got = items(&mut db);
        assert_eq!(got.len(), 1);
        let body = got[0].body.as_ref().unwrap();
        assert!(body.contains("actually-added"));
        assert!(!body.contains("already-there"));
        assert!(!body.contains("speculative-model"));
        db.write(Timestamp(3), |tx| catalog_seen(tx, &[after], Timestamp(3)))
            .unwrap();
        assert_eq!(items(&mut db).len(), 1);
    }
    #[tokio::test]
    async fn system_update_observation_clears_stale_update_notices() {
        let mut db = db();
        let mut n = notices();
        let mut c = copy();
        c.released_models = vec!["announced-model".into()];
        db.write(Timestamp(0), |tx| {
            found(tx, &mut n, &c, None, AgentUpdateMode::Notify, Timestamp(0))?;
            let stored = Stored {
                view: AgentUpdatesView {
                    agents: vec![c],
                    ..AgentUpdatesView::default()
                },
                notices: vec![n.clone()],
                ..Stored::default()
            };
            tx.put("meta", KEY, &stored)
        })
        .unwrap();
        assert_eq!(items(&mut db).len(), 1);
        assert!(items(&mut db)[0].resolved_at.is_none());
        let current = info("2.0.0", &["announced-model"]).await;
        db.write(Timestamp(1), |tx| {
            catalog_seen(tx, &[current], Timestamp(1))
        })
        .unwrap();
        assert!(items(&mut db)[0].resolved_at.is_some());
    }

    #[test]
    fn checking_current_version_keeps_real_model_news_until_dismissed() {
        let mut db = db();
        let mut n = notices();
        n.installed = Some("2.0.0".into());
        let mut c = copy();
        c.installed_version = Some("2.0.0".into());
        db.write(Timestamp(0), |tx| {
            let id = notice(
                tx,
                AgentUpdateNotice {
                    harness: c.harness,
                    kind: AgentUpdateNoticeKind::NewModelsAvailable,
                    version: c.installed_version.clone(),
                    models: vec!["observed-model".into()],
                },
                "New models".into(),
                "Observed after updating".into(),
                Timestamp(0),
            )?;
            n.active.push(id);
            found(
                tx,
                &mut n,
                &c,
                None,
                AgentUpdateMode::Notify,
                Timestamp(DAY),
            )
        })
        .unwrap();
        assert!(items(&mut db)[0].resolved_at.is_none());
    }

    #[test]
    fn same_announced_model_does_not_nag_on_each_patch_release() {
        let mut db = db();
        let mut n = notices();
        let mut c = copy();
        c.released_models = vec!["claude-sonnet-5-5".into()];
        for patch in 0..45 {
            c.latest_version = Some(format!("2.0.{patch}"));
            db.write(Timestamp(patch * DAY), |tx| {
                found(
                    tx,
                    &mut n,
                    &c,
                    None,
                    AgentUpdateMode::Notify,
                    Timestamp(patch * DAY),
                )
            })
            .unwrap();
        }
        let got = items(&mut db);
        assert_eq!(got.len(), 1);
        assert!(got[0].title.contains("Claude Sonnet 5.5"));
        assert!(got[0].title.contains("this computer"));
        assert!(!got[0].title.contains("claude-sonnet-5-5"));
        assert!(got[0].body.as_ref().unwrap().contains("Claude Sonnet 5.5"));
    }

    #[test]
    fn automatic_mode_never_raises_plain_behind_reminders() {
        let mut db = db();
        let mut n = notices();
        let c = copy();
        for day in [0, 14, 30, 44, 100] {
            db.write(Timestamp(day * DAY), |tx| {
                found(
                    tx,
                    &mut n,
                    &c,
                    None,
                    AgentUpdateMode::Automatic,
                    Timestamp(day * DAY),
                )
            })
            .unwrap();
        }
        assert!(items(&mut db).is_empty());
    }

    #[tokio::test]
    async fn rapid_updates_keep_the_earliest_unconsumed_real_catalog() {
        let mut db = db();
        let mut n = notices();
        let before = info("1.0.0", &["already-there"]).await;
        let mut c = copy();
        c.version = Some("2.0.0".into());
        c.installed_version = c.version.clone();
        db.write(Timestamp(0), |tx| {
            found(
                tx,
                &mut n,
                &c,
                Some(&before),
                AgentUpdateMode::Automatic,
                Timestamp(0),
            )
        })
        .unwrap();
        let intermediate = info("2.0.0", &["already-there", "first-addition"]).await;
        c.version = Some("3.0.0".into());
        c.installed_version = c.version.clone();
        db.write(Timestamp(1), |tx| {
            found(
                tx,
                &mut n,
                &c,
                Some(&intermediate),
                AgentUpdateMode::Automatic,
                Timestamp(1),
            )?;
            tx.put(
                "meta",
                KEY,
                &Stored {
                    notices: vec![n.clone()],
                    ..Stored::default()
                },
            )
        })
        .unwrap();
        assert_eq!(n.pending.as_ref().unwrap().before, ["already-there"]);
        let final_catalog = info(
            "3.0.0",
            &["already-there", "first-addition", "second-addition"],
        )
        .await;
        db.write(Timestamp(2), |tx| {
            catalog_seen(tx, &[final_catalog], Timestamp(2))
        })
        .unwrap();
        let got = items(&mut db);
        assert_eq!(got.len(), 1);
        assert_eq!(
            got[0].agent_update.as_ref().unwrap().models,
            ["first-addition", "second-addition"]
        );
    }

    #[tokio::test]
    async fn missing_versions_preserve_dismissals_and_reminder_clocks_in_both_paths() {
        for from_catalog in [false, true] {
            for model_news in [false, true] {
                let mut db = db();
                let mut n = notices();
                let mut c = copy();
                if model_news {
                    c.released_models = vec!["announced-model".into()];
                }
                for day in [0, 14] {
                    db.write(Timestamp(day * DAY), |tx| {
                        found(
                            tx,
                            &mut n,
                            &c,
                            None,
                            AgentUpdateMode::Notify,
                            Timestamp(day * DAY),
                        )
                    })
                    .unwrap();
                }
                assert_eq!(items(&mut db).len(), 1);
                let id = n.active[0];
                db.write(Timestamp(14 * DAY), |tx| {
                    let mut item: AttentionItem = tx.read().require("attention", id)?;
                    item.alert_handled_at = Some(Timestamp(14 * DAY));
                    item.resolved_at = Some(Timestamp(14 * DAY));
                    tx.put("attention", id, &item)?;
                    tx.put(
                        "meta",
                        KEY,
                        &Stored {
                            view: AgentUpdatesView {
                                agents: vec![c.clone()],
                                ..AgentUpdatesView::default()
                            },
                            notices: vec![n.clone()],
                            ..Stored::default()
                        },
                    )
                })
                .unwrap();
                let before = serde_json::to_value(&n).unwrap();
                let budget = db
                    .read(|r| r.get::<Option<Timestamp>>("meta", NUDGE_KEY))
                    .unwrap();
                let mut unknown = info("unavailable", &[]).await;
                unknown.native_version = None;
                let known = info("1.0.0 (Claude Code)", &[]).await;
                for (day, observed) in [(15, &unknown), (16, &known)] {
                    db.write(Timestamp(day * DAY), |tx| {
                        if from_catalog {
                            catalog_seen(tx, std::slice::from_ref(observed), Timestamp(day * DAY))
                        } else {
                            // The same merge used after an updater check: unknown
                            // status remains visible, but must not erase history.
                            let mut saved = load(&tx.read())?;
                            let mut checked = c.clone();
                            checked.installed_version = observed_version(observed);
                            found(
                                tx,
                                &mut saved.notices[0],
                                &checked,
                                Some(observed),
                                saved.view.mode,
                                Timestamp(day * DAY),
                            )?;
                            saved.view.agents[0] = checked;
                            tx.put("meta", KEY, &saved)
                        }
                    })
                    .unwrap();
                    let saved = db.read(load).unwrap();
                    assert_eq!(
                        serde_json::to_value(&saved.notices[0]).unwrap(),
                        before,
                        "history changed: catalog={from_catalog}, models={model_news}, day={day}"
                    );
                    assert_eq!(
                        saved.view.agents[0].installed_version,
                        observed_version(observed)
                    );
                    assert_eq!(
                        db.read(|r| r.get::<Option<Timestamp>>("meta", NUDGE_KEY))
                            .unwrap(),
                        budget
                    );
                    let got = items(&mut db);
                    assert_eq!(got.len(), 1);
                    assert!(got[0].alert_handled_at.is_some());
                    assert!(got[0].resolved_at.is_some());
                }
                let mut saved = db.read(load).unwrap();
                db.write(Timestamp(44 * DAY), |tx| {
                    found(
                        tx,
                        &mut saved.notices[0],
                        &c,
                        Some(&known),
                        saved.view.mode,
                        Timestamp(44 * DAY),
                    )
                })
                .unwrap();
                assert_eq!(items(&mut db).len(), if model_news { 1 } else { 2 });
            }
        }
    }

    #[test]
    fn intermediate_installed_patches_do_not_repeat_announced_models() {
        let mut db = db();
        let mut n = notices();
        let mut c = copy();
        c.released_models = vec!["announced-model".into()];
        db.write(Timestamp(0), |tx| {
            found(tx, &mut n, &c, None, AgentUpdateMode::Notify, Timestamp(0))
        })
        .unwrap();
        let id = n.active[0];
        db.write(Timestamp(1), |tx| {
            let mut item: AttentionItem = tx.read().require("attention", id)?;
            item.alert_handled_at = Some(Timestamp(1));
            item.resolved_at = Some(Timestamp(1));
            tx.put("attention", id, &item)
        })
        .unwrap();
        for patch in 1..4 {
            c.installed_version = Some(format!("1.0.{patch}"));
            c.latest_version = Some(format!("2.0.{patch}"));
            db.write(Timestamp(patch * DAY), |tx| {
                found(
                    tx,
                    &mut n,
                    &c,
                    None,
                    AgentUpdateMode::Notify,
                    Timestamp(patch * DAY),
                )
            })
            .unwrap();
            assert_eq!(items(&mut db).len(), 1);
            assert!(n.told.contains(&"model:announced-model".into()));
        }
        c.released_models.push("genuinely-new-model".into());
        db.write(Timestamp(4 * DAY), |tx| {
            found(
                tx,
                &mut n,
                &c,
                None,
                AgentUpdateMode::Notify,
                Timestamp(4 * DAY),
            )
        })
        .unwrap();
        let got = items(&mut db);
        assert_eq!(got.len(), 2);
        let new = got.iter().find(|item| item.resolved_at.is_none()).unwrap();
        assert_eq!(
            new.agent_update.as_ref().unwrap().models,
            ["genuinely-new-model"]
        );
    }

    #[test]
    fn unsupported_agent_reminder_does_not_promise_an_in_app_update() {
        let mut db = db();
        let mut n = notices();
        let mut c = copy();
        c.update_supported = false;
        for day in [0, 14] {
            db.write(Timestamp(day * DAY), |tx| {
                found(
                    tx,
                    &mut n,
                    &c,
                    None,
                    AgentUpdateMode::Notify,
                    Timestamp(day * DAY),
                )
            })
            .unwrap();
        }
        assert_eq!(
            items(&mut db)[0].body.as_deref(),
            Some(
                "This agent has been behind for at least two weeks. Review its update details in Settings."
            )
        );
    }

    #[tokio::test]
    async fn observed_native_branding_is_normalized_without_substring_guessing() {
        let claude = info("2.1.321 (Claude Code)", &[]).await;
        assert_eq!(observed_version(&claude).as_deref(), Some("2.1.321"));
        let mut codex = info("codex-cli 0.119.0", &[]).await;
        codex.kind = HarnessKind::Codex;
        assert_eq!(observed_version(&codex).as_deref(), Some("0.119.0"));
        codex.native_version = Some("warning: run 0.119.0 from this other path".into());
        assert_eq!(observed_version(&codex), None);
    }
}
