// SPDX-License-Identifier: Apache-2.0
//! Owner-controlled adapter lifecycle. Remote transports cannot invoke these hooks.
use crate::{CompanionHub, error, internal, stored};
use protocol::{
    ErrorCode, Event, ProtocolError,
    capability::*,
    control::{InputState, ObservationState},
    model::InterventionKind,
};
use std::sync::Arc;

impl CompanionHub {
    pub(crate) async fn host_capabilities(&self) -> HostCapabilities {
        let absent = || Availability::Unsupported {
            reason: "No adapter is configured".into(),
        };
        let desktop = if let Some(d) = self.desktop_option() {
            Some(d.capabilities().await)
        } else {
            None
        };
        let protected = {
            let p = self.inner.protection.lock().await;
            !p.required || p.indicator.is_some()
        };
        let unavailable = || Availability::TemporarilyUnavailable {
            reason: INDICATOR_UNAVAILABLE.into(),
        };
        HostCapabilities {
            os: if cfg!(windows) {
                HostOs::Windows
            } else {
                HostOs::Linux
            },
            desktop_backend: desktop
                .as_ref()
                .map_or_else(|| "none".into(), |d| d.backend.clone()),
            capture: desktop
                .as_ref()
                .map_or_else(|| self.desktop_absent(), |d| d.capture.clone()),
            input: if protected {
                desktop
                    .as_ref()
                    .map_or_else(|| self.desktop_absent(), |d| d.input.clone())
            } else {
                unavailable()
            },
            input_scope: desktop.as_ref().map_or(InputScope::None, |d| d.input_scope),
            terminal: if self.inner.terminal.is_some() {
                Availability::Available
            } else {
                absent()
            },
            lock_session: desktop
                .as_ref()
                .map_or_else(|| self.desktop_absent(), |d| d.lock_session.clone()),
            unlock_session: Some(
                desktop
                    .as_ref()
                    .map_or_else(|| self.desktop_absent(), |d| d.unlock_session.clone()),
            ),
            speech: self.speech_availability().await,
            library: if self.inner.library.is_some() {
                Availability::Available
            } else {
                absent()
            },
            transfers: Availability::Available,
            computer_use: if !protected {
                unavailable()
            } else if self.inner.bridge.is_none() {
                Availability::Unsupported { reason: "Install computer-mcp beside Øphio on the computer to enable the tool bridge".into() }
            } else if let Some(d) = &desktop {
                if !d.capture.is_available() {
                    d.capture.clone()
                } else if !d.input.is_available() {
                    d.input.clone()
                } else if self.inner.media.is_none() {
                    absent()
                } else {
                    Availability::Available
                }
            } else {
                self.desktop_absent()
            },
            telegram: self.channel_availability(protocol::auth::ChannelKind::Telegram),
            discord: self.channel_availability(protocol::auth::ChannelKind::Discord),
            features: vec![
                "reports_v1".into(),
                "durable_admission".into(),
                "journal_resume".into(),
                "channel_link_review".into(),
                // Snapshots carry recent history; older tasks and whole runs
                // are read on request.
                "task_history".into(),
                "project_folders".into(),
                "launch_policy_v1".into(),
                "task_branch_v1".into(),
                // The phone's background watcher has its own connection.
                "alerts_v1".into(),
                // Tasks can start from an agent, a skill or a scratch pad,
                // in a project or in a one-off folder.
                "launchers_v1".into(),
                // Agents show the person things in Talk, and the person can
                // delete a picture or video nothing has been sent with.
                "collaboration_v1".into(),
                // A Find's fixes start with the launch flags it was given.
                "fix_launch_v1".into(),
                // A stopped or finished task can be asked about its work in
                // one read-only turn of its own session.
                "read_only_turn_v1".into(),
                // A device can download a phone app the computer offers.
                "app_updates_v1".into(),
                // Agents can be told not to ask questions; every run says
                // which system blocks it.
                "agent_preferences_v1".into(),
                // Alerts can be turned off by topic, everywhere or in one
                // project, and folded into one per project.
                "notification_settings_v1".into(),
                // Agents keep a checklist of what the person has to do in a
                // project; ticking an item off starts the agent waiting on it.
                "checklist_v1".into(),
                // An agent's own work on the checklist, which waits on the
                // person's items and starts the agent once they are done.
                "checklist_agent_items_v1".into(),
                // A message waiting in Next up for a task can carry markup.
                "queue_markup_v1".into(),
                // Work started for the person, such as by an agent, starts
                // with the launch flags their phone set as defaults.
                "launch_defaults_v1".into(),
                // What agents showed in a project is listed, from ended tasks
                // too, and kept a week after the person last opened it.
                "project_shown_v1".into(),
                // Talk holds conversations: Ask tasks that change model
                // between turns and can be carried into Work as a task.
                "conversations_v1".into(),
                "conversation_access_v1".into(),
                // An archived task can be brought back, as Undo.
                "unarchive_task_v1".into(),
                // A problem report in the owner's inbox takes their verdict
                // for everyone who sent it.
                "report_inbox_v1".into(),
                "report_inbox_code_v1".into(),
                // Find & fix can work in a project's FINDINGS.md instead of
                // GitHub, and the file is read for the phone.
                "findings_v1".into(),
                // Contribution handoffs can be reviewed and prepared, so the
                // phone never sends their queries to a companion without them.
                "contribution_import_v1".into(),
                "contribution_delivery_v1".into(),
                // Skills, agents and agent files can be disabled in Ophio,
                // which keeps them from every agent it runs.
                protocol::disabled::FEATURE.into(),
                // A task can be made a review: it is given the project's
                // REVIEW.md from the trusted base, and says which it read.
                "review_rules_v1".into(),
            ]
            .into_iter()
            .chain(self.inner.accounts.is_some().then(|| "accounts_v1".into()))
            .chain(
                self.inner
                    .agent_logins
                    .is_some()
                    .then(|| "agent_logins_v1".into()),
            )
            .chain(
                self.inner
                    .agent_updater
                    .is_some()
                    .then(|| "agent_updates_v1".into()),
            )
            .chain(self.inner.github.is_some().then(|| "github_v1".into()))
            // The owner chooses which problem reports the fixer takes.
            .chain(
                self.inner
                    .reports
                    .fixer
                    .load(std::sync::atomic::Ordering::Relaxed)
                    .then(|| "report_look_into_v1".into()),
            )
            // Review profiles, file-only reviews and a review's own limit.
            .chain(
                self.inner
                    .github
                    .is_some()
                    .then(|| "github_review_v1".into()),
            )
            // Filing an issue from the phone, and an issue's own reviewer.
            .chain(
                self.inner
                    .github
                    .is_some()
                    .then(|| "github_issues_v1".into()),
            )
            // Reviewers that use the app on a screen, as computer use.
            .chain(self.inner.github.is_some().then(|| "github_uat_v1".into()))
            // Run all on the Issues tab, and moving an issue between groups.
            .chain(self.inner.github.is_some().then(|| "github_run_v1".into()))
            // Closing an issue from the phone.
            .chain(
                self.inner
                    .github
                    .is_some()
                    .then(|| "github_close_v1".into()),
            )
            // Merge together on the Pull requests tab.
            .chain(
                self.inner
                    .github
                    .is_some()
                    .then(|| "github_together_v1".into()),
            )
            .chain(
                self.inner
                    .system
                    .is_some()
                    .then(|| "system_stats_v1".into()),
            )
            .chain(
                self.inner
                    .finder
                    .is_some()
                    .then(|| "found_previews_v1".into()),
            )
            .collect(),
            window_actions: match &self.inner.windows {
                Some(w) => Some(w.capabilities().await),
                None => None,
            },
            app_updates: (!self.inner.app_updates.is_empty()).then(|| {
                self.inner
                    .app_updates
                    .iter()
                    .map(|(offer, _)| offer.clone())
                    .collect()
            }),
            conversation_access: self.inner.sandboxes.as_ref().map(|s| s.availability()),
        }
    }

    pub(crate) fn desktop_option(&self) -> Option<Arc<dyn ports::DesktopBackend>> {
        self.inner.desktop.lock().ok().and_then(|d| d.clone())
    }
    pub(crate) fn desktop_absent(&self) -> Availability {
        self.inner
            .desktop_absent
            .lock()
            .map(|a| a.clone())
            .unwrap_or(Availability::Unknown {
                reason: "Desktop state unavailable".into(),
            })
    }
    pub(crate) fn desktop(&self) -> Result<Arc<dyn ports::DesktopBackend>, ProtocolError> {
        self.desktop_option()
            .ok_or_else(|| match self.desktop_absent() {
                Availability::PermissionMissing { remedy } => {
                    error(ErrorCode::OsPermissionMissing, remedy)
                }
                other => error(
                    ErrorCode::TemporarilyUnavailable,
                    format!("Desktop unavailable: {other:?}"),
                ),
            })
    }
    /// Fence admission, withdraw observations and release every old sink before replacement.
    /// A failed release is recorded as unknown; the replacement is not installed.
    /// Every sink is released even when nothing can be recorded, and an input
    /// release that was not confirmed is reported ahead of any other failure.
    pub async fn set_desktop(
        &self,
        backend: Option<Arc<dyn ports::DesktopBackend>>,
        unavailable: Availability,
    ) -> Result<(), ProtocolError> {
        let _change = self.inner.runtime_change.lock().await;
        let _operation = self.inner.desktop_operation.write().await;
        if backend.is_some() && *self.inner.shutdown.borrow() {
            return Err(error(
                ErrorCode::Expired,
                "Øphio is shutting down on the computer",
            ));
        }
        if *self.inner.shutdown.borrow() {
            let active = std::mem::take(&mut self.inner.speech.lock().map_err(internal)?.active);
            for session in active.into_iter().filter_map(|s| s.upgrade()) {
                session.cancel().await;
            }
        }
        *self.inner.desktop_absent.lock().map_err(internal)? = unavailable;
        self.inner.desktop.lock().map_err(internal)?.take();
        self.inner
            .desktop_changed
            .send_modify(|n| *n = n.wrapping_add(1));
        // An unlock already releasing the computer's lock finishes first; one
        // not yet releasing sees sharing changed and releases nothing.
        drop(self.inner.unlock_release.write().await);
        let streams = std::mem::take(&mut *self.inner.active_media.lock().map_err(internal)?);
        for stream in streams.into_iter().filter_map(|s| s.upgrade()) {
            stream.close().await;
        }
        let mut controls = self.inner.controls.lock().await;
        let mut unreleased = None;
        let mut failure = None;
        for (id, control) in controls.iter() {
            let mut target = control.lock().await;
            target.removed = true;
            if let Some(o) = &mut target.view.model_observation {
                o.state = ObservationState::Withdrawn;
            }
            let result = self.revoke_locked(&mut target, false).await;
            let released = target.view.input_state == InputState::Paused;
            let recorded = self.intervention(
                *id,
                InterventionKind::PauseInput,
                "Desktop sharing ended on the computer",
                released,
            );
            if !released {
                unreleased.get_or_insert_with(crate::control::release_unconfirmed);
            }
            failure = failure.or(result.err()).or(recorded.err());
        }
        if failure.is_none() {
            controls.clear();
            *self.inner.desktop.lock().map_err(internal)? = backend;
        }
        drop(controls);
        let host = self.refresh_host().await;
        self.watch_desktop();
        let readiness = self.refresh_readiness().await;
        unreleased.or(failure).map_or(host.and(readiness), Err)
    }
    /// Replace the adapter's authorization set and publish it to all clients.
    /// The folders of Ophio's own agents and of one-offs join as roots of
    /// their own while they exist and no root of the person's holds them, and
    /// so do the folders agent files are kept in, each reaching only those.
    pub async fn set_library_roots(
        &self,
        roots: Vec<ports::RootSpec>,
    ) -> Result<(), ProtocolError> {
        let folders = self.find_agent_files().await;
        let _change = self.inner.runtime_change.lock().await;
        self.apply_library_roots(roots, folders).await
    }
    /// Set the person's `roots` with the ones Ophio adds, while the runtime
    /// change is held.
    pub(crate) async fn apply_library_roots(
        &self,
        roots: Vec<ports::RootSpec>,
        folders: crate::agent_files::AgentFolders,
    ) -> Result<(), ProtocolError> {
        let library = self
            .inner
            .library
            .as_ref()
            .ok_or_else(|| crate::unsupported("Library"))?;
        let automatic =
            crate::launchers::automatic_roots(&self.inner.launchers, &roots, &self.inner.private);
        let agent = self.agent_file_roots(&roots, &folders);
        let taken: Vec<ports::RootSpec> = roots.iter().cloned().chain(automatic.clone()).collect();
        let (any_agent, any_automatic) = (!agent.is_empty(), !automatic.is_empty());
        let all = taken.iter().cloned().chain(agent.clone()).collect();
        // What Ophio adds never stands in the way of the person's own.
        let mut applied = library.set_roots(all).await.map(|()| agent);
        if applied.is_err() && any_agent {
            tracing::warn!("Library refused the folders agent files are kept in");
            applied = library.set_roots(taken).await.map(|()| Vec::new());
        }
        if applied.is_err() && any_automatic {
            tracing::warn!("Library refused the folders Ophio adds");
            applied = library.set_roots(roots.clone()).await.map(|()| Vec::new());
        }
        let agent = applied.map_err(|e| error(e.code(), "Cannot apply Library roots"))?;
        *self.inner.configured_roots.lock().map_err(internal)? = Some(roots);
        self.found_agent_files(folders, agent)?;
        let roots = library.roots().await;
        self.write(|tx| {
            tx.put("meta", "library_roots", &roots)?;
            tx.emit(Event::LibraryRootsChanged { roots })?;
            Ok(())
        })
    }
    /// Apply the person's roots again, so a one-offs folder made since
    /// joins Library.
    pub(crate) async fn refresh_automatic_roots(&self) -> Result<(), ProtocolError> {
        let roots = self
            .inner
            .configured_roots
            .lock()
            .map_err(internal)?
            .clone();
        match roots {
            Some(roots) => self.set_library_roots(roots).await,
            None => Ok(()),
        }
    }
    /// Whether Work runs in a project apply the settings committed in its
    /// folder. Only the owner at the computer reaches this; no device command
    /// changes it.
    pub fn set_project_settings(
        &self,
        project_id: protocol::ProjectId,
        apply: bool,
    ) -> Result<protocol::model::Project, ProtocolError> {
        self.write(|tx| {
            let project = tx.read().require("project", project_id)?;
            tx.put("project_settings", project_id, &apply)?;
            Ok(project)
        })
    }

    pub(crate) fn project_settings(
        r: &store::Read<'_>,
        project_id: protocol::ProjectId,
    ) -> store::Result<bool> {
        Ok(r.get("project_settings", project_id)?.unwrap_or(false))
    }

    /// Reopen under the existing exclusive Hub lock to replace the store's retention policy.
    /// No transaction or subscriber can interleave with the connection swap.
    pub fn set_retention(&self, retention: store::Retention) -> Result<(), ProtocolError> {
        if retention.max_age_ms <= 0 || retention.max_events == 0 {
            return Err(error(
                ErrorCode::InvalidRequest,
                "Retention limits must be positive",
            ));
        }
        let mut db = self.inner.db.lock().map_err(internal)?;
        let replacement = store::Store::open(self.inner.data.join("companion.sqlite"), retention)
            .map_err(stored)?;
        *db = replacement;
        db.write(self.inner.clock.now(), |_| Ok(()))
            .map_err(stored)?;
        Ok(())
    }
}

/// A session clones the selected engine before awaiting; replacing this slot
/// never invalidates sessions that have already started.
#[derive(Debug)]
pub(crate) struct SpeechSlot {
    engine: Option<Arc<dyn ports::SpeechEngine>>,
    absent: Availability,
    active: Vec<std::sync::Weak<dyn crate::api::SpeechInput>>,
}
impl SpeechSlot {
    pub(crate) fn new(engine: Option<Arc<dyn ports::SpeechEngine>>) -> Self {
        Self {
            engine,
            absent: Availability::Unsupported {
                reason: "Local speech is not configured".into(),
            },
            active: Vec::new(),
        }
    }
}
impl CompanionHub {
    pub(crate) fn register_speech(
        &self,
        input: &Arc<dyn crate::api::SpeechInput>,
    ) -> Result<(), ProtocolError> {
        let mut slot = self.inner.speech.lock().map_err(internal)?;
        if *self.inner.shutdown.borrow() {
            return Err(error(
                ErrorCode::Expired,
                "Øphio is shutting down on the computer",
            ));
        }
        slot.active.retain(|session| session.strong_count() > 0);
        slot.active.push(Arc::downgrade(input));
        Ok(())
    }
    pub(crate) fn speech_engine(&self) -> Result<Arc<dyn ports::SpeechEngine>, ProtocolError> {
        let slot = self.inner.speech.lock().map_err(internal)?;
        slot.engine
            .clone()
            .ok_or_else(|| availability_error(&slot.absent))
    }
    pub(crate) async fn speech_availability(&self) -> Availability {
        let (engine, absent) = match self.inner.speech.lock() {
            Ok(slot) => (slot.engine.clone(), slot.absent.clone()),
            Err(_) => {
                return Availability::Unknown {
                    reason: "Speech state unavailable".into(),
                };
            }
        };
        match engine {
            Some(engine) => engine.availability().await,
            None => absent,
        }
    }
    pub async fn set_speech(
        &self,
        engine: Option<Arc<dyn ports::SpeechEngine>>,
        unavailable: Availability,
    ) -> Result<(), ProtocolError> {
        let _change = self.inner.runtime_change.lock().await;
        if *self.inner.shutdown.borrow() {
            return Err(error(
                ErrorCode::Expired,
                "Øphio is shutting down on the computer",
            ));
        }
        {
            let mut slot = self.inner.speech.lock().map_err(internal)?;
            slot.engine = engine;
            slot.absent = unavailable;
        }
        self.publish_capabilities().await
    }
    fn channel_availability(&self, channel: protocol::auth::ChannelKind) -> Availability {
        // Boot-scoped: persisted availability from an earlier process is never trusted.
        self.read(|r| {
            r.get::<(protocol::BootEpoch, Availability)>(
                "channel_availability",
                channel_key(channel),
            )
        })
        .ok()
        .flatten()
        .filter(|(epoch, _)| *epoch == self.inner.epoch)
        .map(|(_, a)| a)
        .unwrap_or(Availability::Unknown {
            reason: "Chat connector has not reported its state".into(),
        })
    }
    pub async fn set_channel_availability(
        &self,
        channel: protocol::auth::ChannelKind,
        availability: Availability,
    ) -> Result<(), ProtocolError> {
        let _change = self.inner.runtime_change.lock().await;
        if *self.inner.shutdown.borrow() {
            return Err(error(
                ErrorCode::Expired,
                "Øphio is shutting down on the computer",
            ));
        }
        if self.channel_availability(channel) == availability {
            return Ok(());
        }
        self.write(|tx| {
            tx.put(
                "channel_availability",
                channel_key(channel),
                &(self.inner.epoch, availability),
            )
        })?;
        self.publish_capabilities().await
    }
    async fn publish_capabilities(&self) -> Result<(), ProtocolError> {
        let capabilities = self.host_capabilities().await;
        self.write(|tx| {
            let mut host: protocol::model::HostInfo = tx.read().require("meta", "host")?;
            host.capabilities = capabilities.clone();
            tx.put("meta", "host", &host)?;
            tx.emit(Event::CapabilitiesChanged {
                capabilities: Box::new(capabilities),
            })?;
            Ok(())
        })?;
        self.refresh_readiness().await
    }
}
fn channel_key(channel: protocol::auth::ChannelKind) -> &'static str {
    match channel {
        protocol::auth::ChannelKind::Telegram => "telegram",
        protocol::auth::ChannelKind::Discord => "discord",
    }
}
pub(crate) fn availability_error(a: &Availability) -> ProtocolError {
    match a {
        Availability::PermissionMissing { remedy } => error(ErrorCode::OsPermissionMissing, remedy),
        Availability::Unsupported { reason } => error(ErrorCode::Unsupported, reason),
        Availability::Unknown { reason }
        | Availability::TemporarilyUnavailable { reason }
        | Availability::Incompatible { reason } => error(ErrorCode::TemporarilyUnavailable, reason),
        _ => error(ErrorCode::TemporarilyUnavailable, "Speech unavailable"),
    }
}
/// Adapter diagnostics are not transcript output. Only known, content-free
/// diagnostics are forwarded; unrecognized native bodies stay private.
pub(crate) fn speech_error(e: ports::SpeechError) -> ProtocolError {
    let (unavailable, message) = match e {
        ports::SpeechError::Unavailable(s) => (true, s),
        ports::SpeechError::Failed(s) => (false, s),
    };
    let code = match message.as_str() {
        "busy: two speech sessions are already active" => ErrorCode::TemporarilyUnavailable,
        "installed model supports English only" | "speech model is not installed" => {
            ErrorCode::Unsupported
        }
        "audio chunk exceeds one second; split it before pushing"
        | "speech session exceeds ten minutes; finish and start a new session" => {
            ErrorCode::LimitExceeded
        }
        "speech session finished"
        | "speech session cancelled"
        | "speech session ended or an operation was cancelled" => ErrorCode::AlreadyResolved,
        "speech worker closed"
        | "speech worker stopped unexpectedly"
        | "local speech model computation failed"
        | "local speech model configuration is invalid"
        | "local speech tokenizer failed" => ErrorCode::NativeFailure,
        _ => {
            return error(
                if unavailable {
                    ErrorCode::TemporarilyUnavailable
                } else {
                    ErrorCode::NativeFailure
                },
                "Local speech failed; check voice status on the computer",
            );
        }
    };
    error(code, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Hub,
        api::{Caller, Outbound, SpeechOut},
    };
    use async_trait::async_trait;
    use ports::{SpeechEngine, SpeechError, SpeechSession};
    #[derive(Debug)]
    struct Engine {
        start: Option<&'static str>,
        failure: Option<&'static str>,
    }
    #[async_trait]
    impl SpeechEngine for Engine {
        fn name(&self) -> String {
            "fixture".into()
        }
        async fn availability(&self) -> Availability {
            Availability::Available
        }
        async fn start(&self, _: Option<String>) -> Result<Box<dyn SpeechSession>, SpeechError> {
            if let Some(message) = self.start {
                return Err(SpeechError::Unavailable(message.into()));
            }
            Ok(Box::new(Session(self.failure)))
        }
    }
    struct Session(Option<&'static str>);
    #[async_trait]
    impl SpeechSession for Session {
        async fn push_pcm(
            &mut self,
            _: &[i16],
        ) -> Result<Vec<protocol::annotation::SpeechSegment>, SpeechError> {
            if let Some(message) = self.0 {
                return Err(SpeechError::Failed(message.into()));
            }
            Ok(vec![])
        }
        async fn finish(
            &mut self,
        ) -> Result<Vec<protocol::annotation::SpeechSegment>, SpeechError> {
            self.push_pcm(&[]).await
        }
    }
    async fn hub() -> (CompanionHub, std::path::PathBuf) {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/runtime-tests")
            .join(protocol::HostId::new().to_string());
        let hub = CompanionHub::builder()
            .data_dir(&path)
            .clock(Arc::new(crate::testing::FakeClock::default()))
            .build()
            .await
            .unwrap();
        (hub, path)
    }
    async fn cleanup(hub: CompanionHub, path: std::path::PathBuf) {
        hub.shutdown().await.unwrap();
        drop(hub);
        std::fs::remove_dir_all(path).unwrap();
    }
    #[tokio::test]
    async fn speech_replace_preserves_sessions_and_publishes_capabilities() {
        let (hub, path) = hub().await;
        let mut subscription = hub
            .subscribe(&Caller::local_owner(), Some(hub.journal_head()))
            .await
            .unwrap();
        let engine: Arc<dyn SpeechEngine> = Arc::new(Engine {
            start: None,
            failure: None,
        });
        let weak = Arc::downgrade(&engine);
        hub.set_speech(Some(engine), Availability::Available)
            .await
            .unwrap();
        let mut stream = hub
            .start_speech(&Caller::local_owner(), "fixture".into(), None, false)
            .await
            .unwrap();
        let missing = Availability::PermissionMissing {
            remedy: "install fixture".into(),
        };
        hub.set_speech(None, missing.clone()).await.unwrap();
        assert_eq!(hub.capabilities().await.speech, missing);
        assert!(weak.upgrade().is_some());
        let error = hub
            .start_speech(&Caller::local_owner(), "fixture".into(), None, false)
            .await
            .unwrap_err();
        assert_eq!(error.message, "install fixture");
        stream.input.push(&[0, 0]).await.unwrap();
        stream.input.finish().await;
        assert!(matches!(
            stream.output.recv().await,
            Some(SpeechOut::Ready { .. })
        ));
        assert!(matches!(
            stream.output.recv().await,
            Some(SpeechOut::Finished { .. })
        ));
        assert!(weak.upgrade().is_none());
        let mut events = 0;
        while let Ok(Outbound::Event(event)) = subscription.live.try_recv() {
            if matches!(event.event, Event::CapabilitiesChanged { .. }) {
                events += 1;
            }
        }
        assert_eq!(events, 2);
        drop(stream);
        cleanup(hub, path).await;
    }
    #[tokio::test]
    async fn channel_availability_events_survive_host_refresh_but_not_restart() {
        let (hub, path) = hub().await;
        let mut events = hub
            .subscribe(&Caller::local_owner(), Some(hub.journal_head()))
            .await
            .unwrap();
        hub.set_channel_availability(
            protocol::auth::ChannelKind::Telegram,
            Availability::Available,
        )
        .await
        .unwrap();
        hub.refresh_host().await.unwrap();
        assert!(hub.capabilities().await.telegram.is_available());
        assert!(
            matches!(events.live.recv().await.unwrap(), Outbound::Event(e) if matches!(e.event, Event::CapabilitiesChanged { .. }))
        );
        hub.shutdown().await.unwrap();
        drop(hub);
        let reopened = CompanionHub::builder()
            .data_dir(&path)
            .build()
            .await
            .unwrap();
        assert!(!reopened.capabilities().await.telegram.is_available());
        cleanup(reopened, path).await;
    }
    #[tokio::test]
    async fn speech_errors_preserve_busy_language_install_limits_and_redact_unknown_text() {
        let (hub, path) = hub().await;
        for (message, code) in [
            (
                "busy: two speech sessions are already active",
                ErrorCode::TemporarilyUnavailable,
            ),
            (
                "installed model supports English only",
                ErrorCode::Unsupported,
            ),
            ("speech model is not installed", ErrorCode::Unsupported),
        ] {
            hub.set_speech(
                Some(Arc::new(Engine {
                    start: Some(message),
                    failure: None,
                })),
                Availability::Available,
            )
            .await
            .unwrap();
            let error = hub
                .start_speech(&Caller::local_owner(), "fixture".into(), None, false)
                .await
                .unwrap_err();
            assert_eq!(error.code, code);
            assert_eq!(error.message, message);
        }
        let limit = "speech session exceeds ten minutes; finish and start a new session";
        hub.set_speech(
            Some(Arc::new(Engine {
                start: None,
                failure: Some(limit),
            })),
            Availability::Available,
        )
        .await
        .unwrap();
        for finish in [false, true] {
            let mut stream = hub
                .start_speech(&Caller::local_owner(), "fixture".into(), None, false)
                .await
                .unwrap();
            stream.output.recv().await.unwrap();
            if finish {
                stream.input.finish().await;
            } else {
                assert_eq!(
                    stream.input.push(&[0, 0]).await.unwrap_err().code,
                    ErrorCode::LimitExceeded
                );
            }
            assert_eq!(
                stream.output.recv().await.unwrap(),
                SpeechOut::Error {
                    message: limit.into()
                }
            );
        }
        assert!(
            !speech_error(SpeechError::Failed("private transcript bytes".into()))
                .message
                .contains("private transcript")
        );
        cleanup(hub, path).await;
    }
    #[tokio::test]
    async fn speech_shutdown_drops_active_session_and_engine() {
        let (hub, path) = hub().await;
        let engine: Arc<dyn SpeechEngine> = Arc::new(Engine {
            start: None,
            failure: None,
        });
        let weak = Arc::downgrade(&engine);
        hub.set_speech(Some(engine), Availability::Available)
            .await
            .unwrap();
        let stream = hub
            .start_speech(&Caller::local_owner(), "fixture".into(), None, false)
            .await
            .unwrap();
        hub.set_speech(
            None,
            Availability::Unknown {
                reason: "replaced".into(),
            },
        )
        .await
        .unwrap();
        hub.shutdown().await.unwrap();
        assert!(weak.upgrade().is_none());
        assert!(stream.input.push(&[0, 0]).await.is_err());
        drop(stream);
        cleanup(hub, path).await;
    }
}

/// What the owner must fix on the computer before control can open.
pub(crate) const INDICATOR_UNAVAILABLE: &str = if cfg!(windows) {
    "Computer control needs the on-computer Stop, and the Windows notification area is not available. Sign in to the computer’s desktop."
} else {
    "Computer control needs the on-computer Stop. Run a desktop notification service that supports actions, such as mako or dunst."
};

#[derive(Default)]
pub(crate) struct ProtectionSlot {
    pub required: bool,
    pub indicator: Option<Arc<dyn ports::desktop::LocalIndicator>>,
}
impl CompanionHub {
    /// Production composition calls this before enabling desktop input. An
    /// absent or failed indicator closes future input grants.
    pub async fn set_local_indicator(
        &self,
        indicator: Option<Arc<dyn ports::desktop::LocalIndicator>>,
    ) {
        *self.inner.protection.lock().await = ProtectionSlot {
            required: true,
            indicator,
        };
        if let Err(error) = self.publish_capabilities().await {
            tracing::warn!(code=?error.code, "Local protection capability update failed");
        }
    }

    pub(crate) fn protection_holders(
        &self,
        replacement: Option<(protocol::TargetId, protocol::control::LeaseOwner)>,
    ) -> Result<String, ProtocolError> {
        use protocol::{control::*, model::*};
        self.read(|r| {
            let mut owners: Vec<_> = r
                .list::<TargetControlView>("control")?
                .into_iter()
                .filter(|v| replacement.is_none_or(|(id, _)| id != v.target_id))
                .filter_map(|v| {
                    v.lease
                        .filter(|l| {
                            matches!(
                                l.state,
                                LeaseState::Active | LeaseState::Revoking | LeaseState::Unknown
                            )
                        })
                        .map(|l| l.owner)
                })
                .collect();
            if let Some((_, owner)) = replacement {
                owners.push(owner);
            }
            let mut names = Vec::new();
            for owner in owners {
                let name = match owner {
                    LeaseOwner::Human { device_id } => {
                        let name = r
                            .get::<crate::identity::Device>("device", device_id)?
                            .map(|d| d.record.name)
                            .unwrap_or_else(|| "Paired device".into());
                        if self.voice_device_active(device_id) {
                            format!("Voice control from {name}")
                        } else {
                            name
                        }
                    }
                    LeaseOwner::Agent { run_id } => {
                        let run: Run = r.require("run", run_id)?;
                        let task: Task = r.require("task", run.task_id)?;
                        format!("Agent · {}", task.title)
                    }
                };
                if !names.contains(&name) {
                    names.push(name);
                }
            }
            Ok(names.join("\n"))
        })
    }
    pub(crate) async fn refresh_local_indicator(&self) -> Result<(), ProtocolError> {
        let protection = self.inner.protection.lock().await;
        if let Some(indicator) = &protection.indicator {
            let holders = self.protection_holders(None)?;
            tokio::time::timeout(std::time::Duration::from_secs(2), async {
                if holders.is_empty() {
                    indicator.hide().await
                } else {
                    indicator.show(&holders).await
                }
            })
            .await
            .map_err(|_| {
                error(
                    ErrorCode::TemporarilyUnavailable,
                    "Local Stop indicator did not answer",
                )
            })?
            .map_err(crate::control::platform)?;
        }
        Ok(())
    }

    /// Owner-only composition entry point. Fence every control target before
    /// submitting native task interrupts. Does not stop unrelated coding work.
    pub async fn stop_all_control(&self) -> Result<usize, ProtocolError> {
        use protocol::{control::ObservationState, model::*};
        self.inner
            .local_stops
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let _stopping = LocalStopGuard(&self.inner.local_stops);
        let targets: Vec<_> = self.inner.controls.lock().await.values().cloned().collect();
        let mut failure = None;
        for target in targets {
            let mut target = target.lock().await;
            if let Some(observation) = &mut target.view.model_observation {
                observation.state = ObservationState::Withdrawn;
            }
            let released = self.revoke_locked(&mut target, false).await;
            crate::control::keep_worse(&mut failure, released);
        }
        let tasks = self.read(|r| {
            let runs = r.list::<Run>("run")?;
            Ok(r.list::<Task>("task")?
                .into_iter()
                .filter(|t| {
                    t.kind == TaskKind::ComputerUse
                        && runs.iter().any(|run| {
                            Some(run.run_id) == t.current_run && !run.state.is_terminal()
                        })
                })
                .collect::<Vec<_>>())
        });
        // Every screen is already fenced; report what could not be found out
        // without letting it hide a screen that may still hold input.
        let tasks = match tasks {
            Ok(tasks) => tasks,
            Err(error) => {
                crate::control::keep_worse(&mut failure, Err(error));
                Vec::new()
            }
        };
        let count = tasks.len();
        for task in tasks {
            let receipt = self
                .admit(
                    &crate::api::Caller::local_owner(),
                    protocol::command::CommandEnvelope::new(protocol::Command::StopTask {
                        force: false,
                        task_id: task.task_id,
                    }),
                )
                .await;
            if let Some(error) = receipt.error {
                crate::control::keep_worse(&mut failure, Err(error));
            }
        }
        failure.map_or(Ok(count), Err)
    }
}

struct LocalStopGuard<'a>(&'a std::sync::atomic::AtomicUsize);
impl Drop for LocalStopGuard<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}
