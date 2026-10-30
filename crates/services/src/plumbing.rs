// SPDX-License-Identifier: Apache-2.0
use crate::{
    CompanionHub, api::*, control::platform, error, internal, transfers::relative, unsupported,
};
use async_trait::async_trait;
use protocol::command::CommandResult;
use protocol::{auth::Principal, capability::*, library::RootKind, media::*, model::*, *};
use std::{path::PathBuf, sync::Arc};
use tokio::sync::{Mutex, mpsc};

impl CompanionHub {
    pub(crate) async fn refresh_host(&self) -> Result<(), ProtocolError> {
        let capabilities = self.host_capabilities().await;
        let host = HostInfo {
            inbox_path: self
                .inbox_folder()
                .ok()
                .map(|p| p.to_string_lossy().into_owned()),
            host_id: self.inner.host_id,
            name: self.inner.name.clone(),
            os_label: std::env::consts::OS.into(),
            companion_version: env!("CARGO_PKG_VERSION").into(),
            boot_epoch: self.inner.epoch,
            started_at: self.inner.started,
            capabilities,
        };
        let targets = if let Some(d) = self.desktop_option() {
            d.targets().await.unwrap_or_default()
        } else {
            Vec::new()
        };
        let roots = if let Some(l) = &self.inner.library {
            l.roots().await
        } else {
            Vec::new()
        };
        self.write(|tx| {
            // What the computer can do is announced as such whenever it
            // changes, as when desktop sharing is turned on or off.
            let changed = tx
                .read()
                .get::<HostInfo>("meta", "host")?
                .is_some_and(|before| before.capabilities != host.capabilities);
            let capabilities = changed.then(|| Box::new(host.capabilities.clone()));
            tx.put("meta", "host", &host)?;
            tx.emit(Event::HostUpdated {
                host: Box::new(host),
            })?;
            if let Some(capabilities) = capabilities {
                tx.emit(Event::CapabilitiesChanged { capabilities })?;
            }
            tx.put("meta", "library_roots", &roots)?;
            tx.emit(Event::LibraryRootsChanged { roots })?;
            Ok(())
        })?;
        self.publish_targets(targets)
    }
    pub(crate) async fn query_impl(
        &self,
        caller: &Caller,
        query: Query,
    ) -> Result<QueryData, ProtocolError> {
        let caller = self.authorize(caller, |g| domain::grants::query(g, &query))?;
        match query {
            Query::ContributionDelivery { packet_json, handoff_url } => Ok(QueryData::ContributionDelivery { handoff_json: self.contribution_delivery(&caller, &packet_json, &handoff_url).await? }),
            Query::ContributionCapabilities => Ok(QueryData::ContributionCapabilities { capabilities: self.contribution_capabilities() }),
            Query::ContributionOperation { operation_id } => Ok(QueryData::ContributionOperation { operation: self.contribution_view(&caller, &operation_id)? }),
            Query::ContributionPolicy { operation_id, path } => Ok(QueryData::ContributionPolicy { document: self.contribution_policy(&caller, &operation_id, &path).await? }),
            Query::ProjectFolders { path, after, show_hidden, search } => Ok(QueryData::ProjectFolders { listing: self.project_folders(&caller, path, after, show_hidden, search).await? }),
            Query::Snapshot { reads_withheld } => self.read(|r| { let caller = Self::current_caller(r,&caller)?; let g = caller.grants; let mut snapshot = crate::history::always(r,&g)?; if matches!(caller.principal, Principal::Channel { .. }) { crate::history::fill_whole(r,&g,&mut snapshot)?; snapshot.attention.retain(|item| !crate::report_inbox::is_inbox(item)); } else { crate::history::fill(r,&g,&mut snapshot,reads_withheld == Some(true))?; } Ok(QueryData::Snapshot(Box::new(snapshot))) }),
            Query::TaskHistory { project_id, from, limit } => self.read(|r| crate::history::page(r, &caller.grants, project_id, from, limit)),
            Query::Task { task_id } => self.read(|r| crate::history::task(r, &caller.grants, task_id)),
            Query::Run { run_id } => self.read(|r| crate::history::run(r, &caller.grants, run_id)),
            Query::Approval { approval_id } => self.read(|r| Ok(QueryData::Approval { approval: Box::new(r.require("approval", approval_id)?) })),
            Query::CommandStatus { command_id } => self.read(|r| {
                // Recheck grants under the same lock as the stored result. Like
                // event replay, withhold the whole record, including its receipt.
                let caller = Self::current_caller(r, &caller)?;
                let c = r.command(command_id)?
                    .filter(|c| c.principal == caller.principal || caller.grants.admin);
                if c.as_ref().is_some_and(|c| !domain::grants::command(&caller.grants, &c.envelope.body)) {
                    // An empty record means "never received" to recovery clients.
                    // Denial must keep the original command id and unknown outcome.
                    return Err(error(ErrorCode::PermissionDenied, "Access to this command's status is not permitted").into());
                }
                Ok(QueryData::CommandStatus {
                    stage: c.as_ref().map(|c| c.stage),
                    result: c.as_ref().and_then(|c| c.result.clone()).map(Box::new),
                    error: c.as_ref().and_then(|c| c.error.clone()).map(Box::new),
                    receipt: c.map(|c| c.receipt),
                })
            }),
            Query::TaskTimeline { task_id,before_seq,limit } => self.read(|r| { let _: Task = r.require("task",task_id)?; let (items,has_more) = r.timeline(task_id,before_seq,limit.clamp(1,1000) as usize)?; Ok(timeline_page(items,has_more)) }),
            Query::ProjectStages { project_id, before_revision, limit } => self.read(|r| Ok(QueryData::ProjectStages { history: r.project_stages(project_id, before_revision, limit)? })),
            Query::ObservedSessions => Ok(QueryData::ObservedSessions { view: self.observed_sessions().await? }),
            Query::Harnesses => self.read(|r| Ok(QueryData::Harnesses { harnesses:r.get("meta","harnesses")?.unwrap_or_default() })),
            Query::ControlTargets => { let targets=self.desktop()?.targets().await.map_err(platform)?; Ok(QueryData::ControlTargets { targets }) },
            Query::LibraryList { root_id,dir } => { relative(&dir,true)?; let listing=self.library()?.list(root_id,&dir).await.map_err(library_error)?; Ok(QueryData::LibraryList { listing }) },
            Query::LibraryRead { root_id,path } => { relative(&path,false)?; let file=self.library()?.read(root_id,&path).await.map_err(library_error)?; Ok(QueryData::LibraryRead { file }) },
            Query::LibrarySearch { root_id,text,limit } => { if text.len()>4096 { return Err(error(ErrorCode::LimitExceeded,"Search text too long")); } let hits=self.library()?.search(root_id,&text,limit.min(1000)).await.map_err(library_error)?; Ok(QueryData::LibrarySearch { hits }) },
            Query::Instructions { project_id, task_id } => self.instructions(&caller, project_id, task_id).await,
            Query::Terminals => { let terminals = self.inner.terminal.as_ref().ok_or_else(|| unsupported("Terminal"))?.list().await; Ok(QueryData::Terminals { terminals: self.read(|r| Self::own_terminals(r, &caller, terminals))? }) },
            Query::Previews { project_id } => self.read(|r| Ok(QueryData::Previews { previews:r.list::<PreviewEndpoint>("preview")?.into_iter().filter(|p|p.project_id==project_id).collect() })),
            Query::ProjectBranches { project_id } => { let project: Project = self.read(|r| r.require("project", project_id))?; Ok(QueryData::ProjectBranches { listing: crate::git::branches(std::path::Path::new(&project.host_path)).await }) },
            Query::Findings { project_id } => { let project: Project = self.read(|r| r.require("project", project_id))?; Ok(QueryData::Findings { text: crate::findings::read(std::path::Path::new(&project.host_path)).await? }) },
            Query::Catalog { project_id } => Ok(QueryData::Catalog { catalog: self.catalog(project_id).await? }),
            Query::AgentFiles => self.agent_files().await,
            Query::DisabledInOphio => Ok(QueryData::DisabledInOphio { items: self.disabled_items().await? }),
            Query::SystemStats => { let sampler = self.inner.system.as_ref().ok_or_else(|| unsupported("System stats"))?; Ok(QueryData::SystemStats { stats: Box::new(sampler.stats(&*self.inner.clock).await) }) },
            Query::Readiness => self.read(|r| Ok(QueryData::Readiness { report:r.get("meta","readiness")?.unwrap_or(ReadinessReport { checked_at:self.inner.started,checks:Vec::new() }) })),
            Query::Devices => self.read(|r| { let devices=r.list::<crate::identity::Device>("device")?.into_iter().map(|d| { let mut d=d.record; d.is_current=matches!(caller.principal, Principal::Device { device_id } if device_id==d.device_id); d }).collect(); Ok(QueryData::Devices { devices,identities:self.channel_identities(r)? }) }),
            Query::ChannelLink { link_code } => Ok(QueryData::ChannelLink { link: self.pending_link(&link_code)? }),
            Query::Transfers => self.read(|r| { let mut transfers = Vec::new(); for t in r.list::<protocol::transfer::TransferRecord>("transfer")? { if Self::sees_transfer(r, &caller, t.transfer_id)? { transfers.push(t); } } Ok(QueryData::Transfers { transfers }) }),
            Query::Queue { project_id } => Ok(QueryData::Queue { queue: self.queue_view(project_id)? }),
            Query::LibraryTrash => Ok(QueryData::LibraryTrash { items: self.library()?.recoverable().await.map_err(library_error)? }),
            Query::LibraryTrashRead { trash_id } => Ok(QueryData::LibraryTrashRead { file: self.library()?.read_trash(&trash_id).await.map_err(library_error)? }),
            Query::ReusableDraft { task_id } => Ok(QueryData::ReusableDraft { text: self.reusable_text(task_id)? }),
            Query::UploadDestination { name, destination } => Ok(QueryData::UploadDestination { check: self.check_upload_destination(&caller, &name, &destination).await? }),
            Query::TaskArtifacts { task_id, before, limit } => Ok(QueryData::TaskArtifacts { history: self.task_artifacts(task_id, before, limit)? }),
            Query::ProjectShown { project_id, before, limit } => Ok(QueryData::ProjectShown { history: self.project_shown(project_id, before, limit)? }),
            Query::Voice { session_id } => Ok(QueryData::Voice { view: Box::new(self.voice_view(&caller, session_id.as_deref()).await?) }),
            Query::AgentUpdates => Ok(QueryData::AgentUpdates { view: self.agent_updates_view()? }),
            Query::AgentPreferences => Ok(QueryData::AgentPreferences { view: self.agent_preferences_view()? }),
            Query::Checklists => Ok(QueryData::Checklists { checklists: self.checklists_view()? }),
            Query::Checklist { project_id } => Ok(QueryData::Checklist { checklist: self.checklist_view(project_id)? }),
            Query::FileDelivery => Ok(QueryData::FileDelivery { view: self.file_delivery_view().await? }),
            Query::NotificationSettings => Ok(QueryData::NotificationSettings { view: self.notification_settings_view()? }),
            Query::Reports => Ok(QueryData::Reports { view: self.reports_view()? }),
            Query::InboxReportCode { report_id } => Ok(QueryData::InboxReportCode { code: self.inbox_report_code(&caller, report_id)? }),
            Query::Report { report_id } => self.read(|r| Ok(QueryData::Report { detail: r.report_detail(report_id)?.map(Box::new) })),
            Query::ReportPreview { salt, extras } => Ok(QueryData::ReportPreview { part: Box::new(self.report_preview(&salt, &extras).await?) }),
            Query::Windows { search } => Ok(QueryData::Windows { catalog: self.window_catalog(&search).await? }),
            Query::Accounts => Ok(QueryData::Accounts { view: Box::new(self.accounts()?.view().await) }),
            Query::AgentLogins => Ok(QueryData::AgentLogins { view: Box::new(self.agent_logins()?.view().await) }),
            Query::AgentLoginPassword { login_id } => Ok(QueryData::AgentLoginPassword { password: self.agent_login_password(&caller, &login_id).await? }),
            Query::ReusableFormats { task_id, harness } => self.reusable_formats(task_id, harness),
            Query::LibraryRecoveryList => Ok(QueryData::LibraryRecoveryList { items: self.library()?.list_recovery().await.map_err(library_error)? }),
            Query::LibraryRecoveryRead { root_id, recovery_id } => Ok(QueryData::LibraryRecoveryRead { recovery: Box::new(self.library()?.inspect_recovery(root_id, &recovery_id).await.map_err(library_error)?) }),
            Query::GithubProject { project_id } => Ok(QueryData::GithubProject { view: Box::new(self.github_project(project_id).await?) }),
            Query::GithubItems { project_id, list, filter } => Ok(QueryData::GithubItems { items: self.github_items(project_id, list, filter.as_deref()).await? }),
            Query::GithubPr { project_id, number } => Ok(QueryData::GithubPr { pr: Box::new(self.github_pr(project_id, number).await?) }),
            Query::Artifact { artifact_id } => self.read(|r| { let artifact = crate::expiry::require_artifact(r, artifact_id)?; if !Self::sees_artifact(r, &caller, &artifact)? { return Err(error(ErrorCode::NotFound, "Record not found").into()); } Ok(QueryData::Artifact { artifact }) }),
        }
    }
    pub(crate) fn library(&self) -> Result<&Arc<dyn ports::LibraryFs>, ProtocolError> {
        self.inner
            .library
            .as_ref()
            .ok_or_else(|| unsupported("Library"))
    }
    pub(crate) async fn plumbing_command(
        &self,
        caller: &Caller,
        command: &Command,
    ) -> Result<Option<CommandResult>, ProtocolError> {
        let mut result = None;
        match command {
            Command::LibrarySave {
                root_id,
                path,
                text,
                base_revision,
                newline,
                bom,
            } => {
                relative(path, false)?;
                let outcome = self
                    .library()?
                    .save(
                        *root_id,
                        path,
                        ports::SaveRequest {
                            text: text.clone(),
                            base_revision: base_revision.clone(),
                            newline: *newline,
                            bom: *bom,
                        },
                    )
                    .await
                    .map_err(library_error)?;
                if let protocol::library::SaveOutcome::Saved { revision } = &outcome {
                    self.library_changed(*root_id, path, Some(revision.clone()))?;
                }
                result = Some(CommandResult::Saved {
                    outcome: conflict_fitted(outcome),
                });
            }

            Command::CreateAgent { name, description } => {
                result = Some(self.create_agent(name, description.as_deref()).await?);
            }
            Command::CopySetupSkill { id } => {
                result = Some(self.copy_setup_skill(id).await?);
            }
            Command::ChooseSetupSkillVersion { id, keep_mine } => {
                result = Some(self.choose_setup_skill_version(id, *keep_mine).await?);
            }
            Command::LibraryCreate {
                root_id,
                path,
                text,
            } => {
                relative(path, false)?;
                let revision = self
                    .library()?
                    .create(*root_id, path, text)
                    .await
                    .map_err(library_error)?;
                self.library_changed(*root_id, path, Some(revision.clone()))?;
                result = Some(CommandResult::Created {
                    path: path.clone(),
                    revision,
                });
            }
            Command::LibraryRename {
                root_id,
                from,
                to,
                base_revision,
            } => {
                relative(from, false)?;
                relative(to, false)?;
                let revision = self
                    .library()?
                    .rename(*root_id, from, to, base_revision)
                    .await
                    .map_err(library_error)?;
                self.library_changed(*root_id, from, None)?;
                self.library_changed(*root_id, to, Some(revision))?;
            }
            Command::LibraryDelete {
                root_id,
                path,
                base_revision,
            } => {
                relative(path, false)?;
                let trash = self
                    .library()?
                    .trash(*root_id, path, base_revision)
                    .await
                    .map_err(library_error)?;
                self.library_changed(*root_id, path, None)?;
                let item = self
                    .library()?
                    .recoverable()
                    .await
                    .map_err(library_error)?
                    .into_iter()
                    .find(|i| i.trash_id == trash.trash_id)
                    .ok_or_else(|| {
                        error(
                            ErrorCode::OutcomeUnknown,
                            "Deletion completed but its recovery record could not be confirmed",
                        )
                    })?;
                result = Some(CommandResult::LibraryDeleted { item });
            }
            Command::MakeReusable {
                task_id,
                kind,
                name,
            } => {
                relative(name, false)?;
                if name.contains('/') {
                    return Err(error(
                        ErrorCode::PathNotAllowed,
                        "Draft name must be a single filename",
                    ));
                }
                let library = self.library()?;
                let root = library
                    .roots()
                    .await
                    .into_iter()
                    .find(|r| r.kind == RootKind::Drafts && r.writable)
                    .ok_or_else(|| unsupported("Writable Drafts root"))?;
                let text = self.reusable_text(*task_id)?;
                if *kind != protocol::command::ReusableKind::Note {
                    return Err(error(
                        ErrorCode::InvalidRequest,
                        "Choose an agent and review its supported format before saving a reusable draft",
                    ));
                }
                let path = if name.ends_with(".md") {
                    name.clone()
                } else {
                    format!("{name}.md")
                };
                let revision = library
                    .create(root.root_id, &path, &text)
                    .await
                    .map_err(library_error)?;
                self.library_changed(root.root_id, &path, Some(revision))?;
                result = Some(CommandResult::DraftCreated {
                    root_id: root.root_id,
                    path,
                });
            }
            Command::LibraryRestore {
                trash_id,
                base_revision,
                copy_path,
            } => {
                let change = self
                    .library()?
                    .restore_checked(trash_id, base_revision, copy_path.as_deref())
                    .await
                    .map_err(library_error)?;
                self.library_changed(change.root_id, &change.path, change.revision.clone())?;
                result = Some(CommandResult::LibraryRestored {
                    root_id: change.root_id,
                    path: change.path,
                    revision: change.revision.unwrap_or_default(),
                });
            }
            Command::CreateReusableDraft {
                task_id,
                root_id,
                path,
                text,
            } => return self.create_reusable(*task_id, *root_id, path, text).await,
            Command::LibraryRecoveryDiscard {
                root_id,
                recovery_id,
            } => {
                self.library()?
                    .discard_recovery_checked(*root_id, recovery_id)
                    .await
                    .map_err(library_error)?;
                result = Some(CommandResult::LibraryRecoveryDiscarded {
                    recovery_id: recovery_id.clone(),
                });
            }
            Command::OpenTerminal {
                project_id,
                cwd: requested_cwd,
                harness,
                cols,
                rows,
            } => {
                if *cols == 0 || *rows == 0 || *cols > 1000 || *rows > 1000 {
                    return Err(error(
                        ErrorCode::InvalidRequest,
                        "Invalid terminal dimensions",
                    ));
                }
                let cwd = if let Some(id) = project_id {
                    PathBuf::from(
                        self.read(|r| r.require::<Project>("project", id))?
                            .host_path,
                    )
                } else {
                    self.inner.data.clone()
                };
                let cwd = if let Some(requested) = requested_cwd {
                    let path = PathBuf::from(requested);
                    if !path.is_absolute()
                        || !tokio::fs::metadata(&path).await.is_ok_and(|m| m.is_dir())
                    {
                        return Err(error(
                            ErrorCode::InvalidRequest,
                            "The terminal folder is no longer available. Open a terminal from Control to use the project folder.",
                        ));
                    }
                    path
                } else {
                    cwd
                };
                let name = match harness {
                    Some(HarnessKind::ClaudeCode) => Some("claude"),
                    Some(HarnessKind::Codex) => Some("codex"),
                    Some(_) => return Err(unsupported("Interactive harness command")),
                    None => None,
                };
                // Øphio's own copy while it keeps one, as its tasks use.
                let copy =
                    harness.and_then(|kind| self.inner.agent_updater.as_ref()?.program(kind));
                // Started here, an agent is kept from what is disabled in
                // Ophio as its tasks are.
                let args = match harness.filter(|_| name.is_some()) {
                    Some(kind) => self.terminal_block(kind, &cwd)?,
                    None => Vec::new(),
                };
                let (program, env) = match (name, copy) {
                    (Some(_), Some((path, env))) => (Some((path, args)), env),
                    (name, _) => (name.map(|name| (PathBuf::from(name), args)), Vec::new()),
                };
                let terminal = self
                    .inner
                    .terminal
                    .as_ref()
                    .ok_or_else(|| unsupported("Terminal"))?;
                let opened = terminal
                    .open(ports::TerminalSpec {
                        title: "Terminal".into(),
                        cwd,
                        program,
                        env,
                        project_id: *project_id,
                        harness: *harness,
                        cols: *cols,
                        rows: *rows,
                    })
                    .await
                    .map_err(terminal_error)?;
                let id = opened.terminal_id;
                result = Some(CommandResult::TerminalOpened { terminal: opened });
                let terminals = terminal.list().await;
                let recorded = self.write(|tx| {
                    // Revocation closes only the shells it finds recorded, so a
                    // device that lost the terminal while this one started gets none.
                    let current = Self::current_caller(&tx.read(), caller)?;
                    if !domain::grants::command(&current.grants, command) {
                        return Err(error(ErrorCode::PermissionDenied, "Grant was revoked").into());
                    }
                    tx.put("terminal_owner", id, &caller.principal)?;
                    tx.emit(Event::TerminalsChanged { terminals })?;
                    Ok(())
                });
                if let Err(e) = recorded {
                    // A shell no one owns could never be reached or closed.
                    let _ = terminal.close(id).await;
                    return Err(e);
                }
            }
            Command::CloseTerminal { terminal_id } => {
                let terminal = self
                    .inner
                    .terminal
                    .as_ref()
                    .ok_or_else(|| unsupported("Terminal"))?;
                self.terminal_opener(caller, *terminal_id)?;
                terminal.close(*terminal_id).await.map_err(terminal_error)?;
                let terminals = terminal.list().await;
                self.write(|tx| {
                    tx.emit(Event::TerminalsChanged { terminals })?;
                    Ok(())
                })?;
            }
            Command::RunReadinessCheck => {
                self.refresh_catalog().await?;
                self.refresh_host().await?;
                self.refresh_readiness().await?;
            }
            _ => {
                return Err(error(
                    ErrorCode::InvalidRequest,
                    "Not a library, terminal, or readiness command",
                ));
            }
        }
        let _ = caller;
        Ok(result)
    }
    pub async fn refresh_readiness(&self) -> Result<(), ProtocolError> {
        let caps = self.host_capabilities().await;
        let mut checks = vec![
            readiness(
                ReadinessArea::Terminal,
                "Terminal",
                ("You can open terminals", "You can’t open terminals"),
                &caps.terminal,
            ),
            readiness(
                ReadinessArea::Graphics,
                "Capture",
                ("You can watch the desktop", "You can’t watch the desktop"),
                &caps.capture,
            ),
            readiness(
                ReadinessArea::Graphics,
                "Input",
                (
                    "You can use the mouse and keyboard",
                    "You can’t use the mouse and keyboard",
                ),
                &caps.input,
            ),
            readiness(
                ReadinessArea::Graphics,
                "Lock session",
                ("You can lock the computer", "You can’t lock the computer"),
                &caps.lock_session,
            ),
            readiness(
                ReadinessArea::Graphics,
                "Computer use",
                ("Agents can use the desktop", "Agents can’t use the desktop"),
                &caps.computer_use,
            ),
            readiness(
                ReadinessArea::Speech,
                "Voice",
                (
                    "You can speak instead of typing",
                    "You can’t speak instead of typing",
                ),
                &caps.speech,
            ),
            readiness(
                ReadinessArea::Storage,
                "Library",
                ("You can open Library files", "You can’t open Library files"),
                &caps.library,
            ),
            readiness(
                ReadinessArea::Storage,
                "Transfers",
                (
                    "You can send and receive files",
                    "You can’t send or receive files",
                ),
                &caps.transfers,
            ),
            readiness(
                ReadinessArea::Channels,
                "Telegram",
                (
                    "You can reach agents from Telegram",
                    "You can’t reach agents from Telegram",
                ),
                &caps.telegram,
            ),
            readiness(
                ReadinessArea::Channels,
                "Discord",
                (
                    "You can reach agents from Discord",
                    "You can’t reach agents from Discord",
                ),
                &caps.discord,
            ),
            ReadinessCheck {
                area: ReadinessArea::Network,
                label: "Private network reachability".into(),
                status: CheckStatus::Unknown,
                detail: Some(
                    "Reachability depends on the device and address. Check the connection on the device.".into(),
                ),
                remedy: None,
                summary: None,
            },
        ];
        // What Chat's access bar and GitHub's outside requests rely on.
        if let Some(access) = &caps.conversation_access {
            checks.push(readiness(
                ReadinessArea::Accounts,
                "Agent limits",
                (
                    "Agents can be held to what you let them reach",
                    "Agents can’t be held to what you let them reach",
                ),
                access,
            ));
        }
        let harnesses: Vec<HarnessInfo> =
            self.read(|r| Ok(r.get("meta", "harnesses")?.unwrap_or_default()))?;
        for h in harnesses {
            if h.integration == protocol::capability::IntegrationLevel::NotIntegrated {
                continue;
            }
            let name = &h.display_name;
            checks.push(readiness(
                ReadinessArea::Accounts,
                &format!("{name} installation"),
                (
                    &format!("{name} is installed"),
                    &format!("{name} isn’t installed"),
                ),
                &h.installed,
            ));
            for (label, (can, cannot), availability) in [
                (
                    "start run",
                    (
                        format!("{name} can start tasks"),
                        format!("{name} can’t start tasks"),
                    ),
                    &h.capabilities.start_run,
                ),
                (
                    "steer running",
                    (
                        format!("You can steer {name} while it works"),
                        format!("You can’t steer {name} while it works"),
                    ),
                    &h.capabilities.steer_running,
                ),
                (
                    "interrupt",
                    (
                        format!("You can stop {name} mid-task"),
                        format!("You can’t stop {name} mid-task"),
                    ),
                    &h.capabilities.interrupt,
                ),
                (
                    "approvals",
                    (
                        format!("You can approve or deny what {name} asks to do"),
                        format!("You can’t approve or deny what {name} asks to do"),
                    ),
                    &h.capabilities.native_approvals,
                ),
                (
                    "resume session",
                    (
                        format!("You can continue earlier {name} sessions"),
                        format!("You can’t continue earlier {name} sessions"),
                    ),
                    &h.capabilities.resume_session,
                ),
                (
                    "child agents",
                    (
                        format!("You can see {name}’s helper agents"),
                        format!("You can’t see {name}’s helper agents"),
                    ),
                    &h.capabilities.child_agents,
                ),
                (
                    "computer use",
                    (
                        format!("{name} can use the computer for you"),
                        format!("{name} can’t use the computer for you"),
                    ),
                    &h.capabilities.computer_use_delegation,
                ),
                (
                    "model selection",
                    (
                        format!("You can choose {name}’s model"),
                        format!("You can’t choose {name}’s model"),
                    ),
                    &h.capabilities.model_selection,
                ),
                (
                    "effort selection",
                    (
                        format!("You can choose how hard {name} thinks"),
                        format!("You can’t choose how hard {name} thinks"),
                    ),
                    &h.capabilities.effort_selection,
                ),
            ] {
                checks.push(readiness(
                    ReadinessArea::Accounts,
                    &format!("{name} {label}"),
                    (&can, &cannot),
                    availability,
                ));
            }
            for mode in &h.modes {
                let (label, can) = match mode.mode {
                    TaskMode::Ask => ("Ask", "answer questions without changing anything"),
                    TaskMode::Plan => ("Plan", "propose a plan without changing anything"),
                    TaskMode::Work => ("Work", "edit files and run commands"),
                };
                checks.push(readiness(
                    ReadinessArea::Accounts,
                    &format!("{name} {label}"),
                    (
                        &format!("{name} can {can}"),
                        &format!("{name} can’t take {label} tasks"),
                    ),
                    &mode.availability,
                ));
            }
            checks.push(ReadinessCheck {
                area: ReadinessArea::Accounts,
                label: format!("{} sign-in", h.display_name),
                status: match &h.account {
                    AccountReadiness::SignedIn { .. } => CheckStatus::Passed,
                    AccountReadiness::SignedOut { .. } => CheckStatus::Failed,
                    AccountReadiness::Unknown { .. } => CheckStatus::Unknown,
                },
                detail: match &h.account {
                    AccountReadiness::Unknown { reason } => Some(reason.clone()),
                    _ => None,
                },
                remedy: match &h.account {
                    AccountReadiness::SignedOut { remedy } => Some(remedy.clone()),
                    _ => None,
                },
                summary: Some(match &h.account {
                    AccountReadiness::SignedIn { .. } => format!("{name} is signed in"),
                    AccountReadiness::SignedOut { .. } => format!("{name} is signed out"),
                    AccountReadiness::Unknown { .. } => {
                        format!("Not known whether {name} is signed in")
                    }
                }),
            });
        }
        let report = ReadinessReport {
            checked_at: self.inner.clock.now(),
            checks,
        };
        self.write(|tx| {
            tx.put("meta", "readiness", &report)?;
            tx.emit(Event::ReadinessReported { report })?;
            Ok(())
        })?;
        Ok(())
    }
    pub(crate) fn library_changed(
        &self,
        root_id: LibraryRootId,
        path: &str,
        revision: Option<String>,
    ) -> Result<(), ProtocolError> {
        self.write(|tx| {
            tx.emit(Event::LibraryFileChanged {
                root_id,
                path: path.into(),
                revision,
            })?;
            Ok(())
        })
    }
    /// A terminal belongs to whoever opened it; only the owner at the
    /// computer also reaches the others.
    pub(crate) fn opened_terminal(
        r: &store::Read<'_>,
        caller: &Caller,
        id: TerminalId,
    ) -> store::Result<bool> {
        Ok(caller.principal == Principal::LocalOwner
            || r.get::<Principal>("terminal_owner", id)? == Some(caller.principal))
    }
    pub(crate) fn own_terminals(
        r: &store::Read<'_>,
        caller: &Caller,
        terminals: Vec<protocol::terminal::TerminalInfo>,
    ) -> store::Result<Vec<protocol::terminal::TerminalInfo>> {
        let mut own = Vec::with_capacity(terminals.len());
        for terminal in terminals {
            if Self::opened_terminal(r, caller, terminal.terminal_id)? {
                own.push(terminal);
            }
        }
        Ok(own)
    }
    /// No recorded opener means no terminal: none outlives a restart, and
    /// saying another device has it would send the person back to reconnect.
    fn terminal_opener(&self, caller: &Caller, id: TerminalId) -> Result<(), ProtocolError> {
        let (opened, recorded) = self.read(|r| {
            Ok((
                Self::opened_terminal(r, caller, id)?,
                r.get::<Principal>("terminal_owner", id)?.is_some(),
            ))
        })?;
        if opened {
            return Ok(());
        }
        Err(if recorded {
            error(
                ErrorCode::PermissionDenied,
                "Another device opened this terminal.",
            )
        } else {
            error(
                ErrorCode::NotFound,
                "This terminal is no longer on the computer.",
            )
        })
    }
    /// Whether the owner revokes a device or pairs another in its place, the
    /// device lets go of the desktop and voice and its shells end. Each is
    /// attempted even if the other fails.
    pub(crate) async fn end_device_access(
        &self,
        device_id: protocol::DeviceId,
    ) -> Result<(), ProtocolError> {
        let leases = self.revoke_device_leases(device_id).await;
        let terminals = self.close_device_terminals(device_id).await;
        leases.and(terminals)
    }
    /// A revoked device's shells end with it; no other device may reach them.
    pub(crate) async fn close_device_terminals(
        &self,
        device_id: protocol::DeviceId,
    ) -> Result<(), ProtocolError> {
        let Some(host) = self.inner.terminal.clone() else {
            return Ok(());
        };
        let device = Principal::Device { device_id };
        let opened: Vec<TerminalId> = self.read(|r| {
            Ok(r.entries::<Principal>("terminal_owner")?
                .into_iter()
                .filter(|(_, owner)| *owner == device)
                .filter_map(|(id, _)| id.parse().ok())
                .collect())
        })?;
        if opened.is_empty() {
            return Ok(());
        }
        for id in &opened {
            let _ = host.close(*id).await;
        }
        let terminals = host.list().await;
        self.write(|tx| {
            tx.emit(Event::TerminalsChanged { terminals })?;
            Ok(())
        })
    }
    pub(crate) async fn terminal_attachment(
        &self,
        caller: &Caller,
        id: TerminalId,
        offset: Option<u64>,
    ) -> Result<TerminalAttachment, ProtocolError> {
        let caller = self.authorize(caller, |g| g.terminal)?;
        self.terminal_opener(&caller, id)?;
        let host = self
            .inner
            .terminal
            .as_ref()
            .ok_or_else(|| unsupported("Terminal"))?
            .clone();
        let terminal = host
            .info(id)
            .await
            .ok_or_else(|| error(ErrorCode::NotFound, "Terminal not found"))?;
        let (replay, mut live) = host.attach(id, offset).await.map_err(terminal_error)?;
        let (tx, output) = mpsc::channel(64);
        let (detached, mut detached_rx) = tokio::sync::watch::channel(false);
        let mut shutdown = self.inner.shutdown.subscribe();
        let input = Arc::new(TerminalWriter {
            hub: self.clone(),
            caller: caller.clone(),
            host,
            id,
            detached,
        });
        let hub = self.clone();
        let replay_from = replay.from;
        let replay_truncated = replay.truncated;
        let weak = Arc::downgrade(&input);
        self.spawn(async move {
            let mut next = replay.from + replay.bytes.len() as u64;
            if !replay.bytes.is_empty()
                && tx
                    .send(TerminalOut::Output {
                        offset: replay.from,
                        bytes: replay.bytes,
                    })
                    .await
                    .is_err()
            {
                return;
            }
            loop {
                let event = tokio::select! { _ = detached_rx.changed() => break, _ = shutdown.changed() => break, _ = tx.closed() => break, event = live.recv() => match event { Ok(event) => event, Err(_) => break } };
                if hub.authorize(&caller, |g| g.terminal).is_err()
                    || weak
                        .upgrade()
                        .is_none_or(|w| *w.detached.borrow())
                {
                    break;
                }
                let out = match event {
                    ports::TerminalEvent::Output { offset, mut bytes } => {
                        if offset > next {
                            break;
                        }
                        let overlap = next.saturating_sub(offset) as usize;
                        if overlap >= bytes.len() {
                            continue;
                        }
                        bytes = bytes.slice(overlap..);
                        let offset = next;
                        next += bytes.len() as u64;
                        TerminalOut::Output { offset, bytes }
                    }
                    ports::TerminalEvent::Resized { cols, rows } => {
                        TerminalOut::Resized { cols, rows }
                    }
                    ports::TerminalEvent::Exited { code } => TerminalOut::Exited { code },
                };
                let ended = matches!(out, TerminalOut::Exited { .. });
                if tx.send(out).await.is_err() || ended {
                    break;
                }
            }
        });
        Ok(TerminalAttachment {
            terminal,
            replay_from,
            replay_truncated,
            output,
            input,
        })
    }
    pub(crate) async fn media_session(
        &self,
        caller: &Caller,
        id: TargetId,
        params: StreamParams,
    ) -> Result<MediaSession, ProtocolError> {
        let caller = self.authorize(caller, |g| g.view)?;
        validate_params(params)?;
        if params.reopen == Some(true) {
            self.authorize(&caller, |g| g.control_input)?;
            self.reopen_desktop_capture().await?;
        }
        let _operation = self.inner.desktop_operation.read().await;
        let mut desktop_changed = self.inner.desktop_changed.subscribe();
        let desktop = self.desktop()?;
        let target = desktop
            .targets()
            .await
            .map_err(platform)?
            .into_iter()
            .find(|t| t.target_id == id)
            .ok_or_else(|| error(ErrorCode::NotFound, "Target not found"))?;
        let stream = desktop
            .open_stream(id, params.max_fps)
            .await
            .map_err(platform)?;
        let mut encoded = self
            .inner
            .media
            .as_ref()
            .ok_or_else(|| unsupported("Media encoder"))?
            .start(stream, params);
        // The encoder owns tile size. Read its first frame instead of guessing
        // a value that would disagree with a different encoder implementation.
        let first = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while let Some(out) = encoded.output.recv().await {
                match out {
                    ports::EncodedOut::Frame(frame) => return Ok(frame),
                    ports::EncodedOut::Ended { .. } => break,
                    _ => {}
                }
            }
            Err(error(
                ErrorCode::TemporarilyUnavailable,
                "Media stream ended before its first frame",
            ))
        })
        .await
        .map_err(|_| error(ErrorCode::TemporarilyUnavailable, "No media frame received"))??;
        let (header, _) = parse_tile_frame(&first).map_err(|_| {
            error(
                ErrorCode::NativeFailure,
                "Encoder produced an invalid frame",
            )
        })?;
        let mut geometry = target.geometry;
        geometry.revision = header.geometry_revision;
        geometry.width = header.src_width;
        geometry.height = header.src_height;
        let (tx, output) = mpsc::channel(8);
        {
            let mut streams = self.inner.active_media.lock().map_err(internal)?;
            streams.retain(|s| s.strong_count() > 0);
            streams.push(Arc::downgrade(&encoded.control));
        }
        let control = Arc::new(MediaWriter {
            hub: self.clone(),
            caller: caller.clone(),
            inner: encoded.control.clone(),
        });
        tx.send(MediaOut::Control(MediaServerFrame::Opened {
            session_id: MediaSessionId::new(),
            target_id: id,
            geometry,
            tile: header.tile,
        }))
        .await
        .map_err(internal)?;
        tx.send(MediaOut::Frame(first)).await.map_err(internal)?;
        let hub = self.clone();
        self.spawn(async move {
            loop {
                let out = tokio::select! {
                    biased;
                    _ = desktop_changed.changed() => {
                        encoded.control.close().await;
                        let _ = tokio::time::timeout(std::time::Duration::from_secs(1), tx.send(MediaOut::Control(MediaServerFrame::Ended { reason: "Desktop sharing ended on the computer".into() }))).await;
                        break;
                    }
                    _ = tx.closed() => break,
                    out = encoded.output.recv() => match out { Some(out) => out, None => break },
                };
                if hub.authorize(&caller, |g| g.view).is_err() {
                    break;
                }
                let out = match out {
                    ports::EncodedOut::Frame(bytes) => MediaOut::Frame(bytes),
                    ports::EncodedOut::GeometryChanged(geometry) => {
                        if let Ok(targets) = desktop.targets().await {
                            let _ = hub.publish_targets(targets);
                        }
                        MediaOut::Control(MediaServerFrame::GeometryChanged { geometry })
                    }
                    ports::EncodedOut::Paused { reason } => {
                        MediaOut::Control(MediaServerFrame::Paused { reason })
                    }
                    ports::EncodedOut::Resumed => MediaOut::Control(MediaServerFrame::Resumed),
                    ports::EncodedOut::Ended { reason } => {
                        MediaOut::Control(MediaServerFrame::Ended { reason })
                    }
                };
                tokio::select! {
                    biased;
                    _ = desktop_changed.changed() => {
                        encoded.control.close().await;
                        let _ = tokio::time::timeout(std::time::Duration::from_secs(1), tx.send(MediaOut::Control(MediaServerFrame::Ended { reason: "Desktop sharing ended on the computer".into() }))).await;
                        break;
                    }
                    result = tx.send(out) => if result.is_err() { break; },
                }
            }
            encoded.control.close().await;
        });
        Ok(MediaSession { output, control })
    }
    pub(crate) async fn speech_stream(
        &self,
        caller: &Caller,
        language: Option<String>,
        keep: bool,
    ) -> Result<SpeechStream, ProtocolError> {
        let caller = self.authorize(caller, |g| g.tasks)?;
        if *self.inner.shutdown.borrow() {
            return Err(error(
                ErrorCode::Expired,
                "Øphio is shutting down on the computer",
            ));
        }
        let engine = self.speech_engine()?;
        let availability = engine.availability().await;
        if !availability.is_available() {
            return Err(crate::runtime::availability_error(&availability));
        }
        let session = engine
            .start(language)
            .await
            .map_err(crate::runtime::speech_error)?;
        let (tx, output) = mpsc::channel(256);
        tx.send(SpeechOut::Ready {
            engine: engine.name(),
        })
        .await
        .map_err(internal)?;
        let input: Arc<dyn SpeechInput> = Arc::new(SpeechWriter {
            hub: self.clone(),
            caller,
            state: Mutex::new(SpeechState {
                session: Some(session),
                engine: Some(engine),
                audio: if keep { Some(Vec::new()) } else { None },
                total: 0,
            }),
            tx,
        });
        self.register_speech(&input)?;
        Ok(SpeechStream { output, input })
    }
}
struct TerminalWriter {
    hub: CompanionHub,
    caller: Caller,
    host: Arc<dyn ports::TerminalHost>,
    id: TerminalId,
    detached: tokio::sync::watch::Sender<bool>,
}
#[async_trait]
impl TerminalInput for TerminalWriter {
    async fn write(&self, bytes: &[u8]) -> Result<(), ProtocolError> {
        self.hub.authorize(&self.caller, |g| g.terminal)?;
        if *self.detached.borrow() {
            return Err(error(ErrorCode::Conflict, "Terminal detached"));
        }
        if bytes.len() > 256 * 1024 {
            return Err(error(ErrorCode::LimitExceeded, "Terminal input too large"));
        }
        self.host
            .write(self.id, bytes)
            .await
            .map_err(terminal_error)
    }
    async fn resize(&self, cols: u16, rows: u16) -> Result<(), ProtocolError> {
        self.hub.authorize(&self.caller, |g| g.terminal)?;
        if cols == 0 || rows == 0 || cols > 1000 || rows > 1000 {
            return Err(error(ErrorCode::InvalidRequest, "Invalid terminal size"));
        }
        self.host
            .resize(self.id, cols, rows)
            .await
            .map_err(terminal_error)
    }
    async fn detach(&self) {
        let _ = self.detached.send(true);
    }
}
struct MediaWriter {
    hub: CompanionHub,
    caller: Caller,
    inner: Arc<dyn ports::EncodedStreamControl>,
}
#[async_trait]
impl MediaControl for MediaWriter {
    async fn ack(&self, seq: u64) {
        if self.hub.authorize(&self.caller, |g| g.view).is_ok() {
            self.inner.ack(seq).await;
        } else {
            self.inner.close().await;
        }
    }
    async fn adjust(&self, params: StreamParams) {
        if validate_params(params).is_ok() && self.hub.authorize(&self.caller, |g| g.view).is_ok() {
            self.inner.adjust(params).await;
        }
    }
    async fn request_keyframe(&self) {
        if self.hub.authorize(&self.caller, |g| g.view).is_ok() {
            self.inner.request_keyframe().await;
        }
    }
    async fn close(&self) {
        self.inner.close().await;
    }
}
struct SpeechState {
    engine: Option<Arc<dyn ports::SpeechEngine>>,
    session: Option<Box<dyn ports::SpeechSession>>,
    audio: Option<Vec<u8>>,
    total: u64,
}
struct SpeechWriter {
    hub: CompanionHub,
    caller: Caller,
    state: Mutex<SpeechState>,
    tx: mpsc::Sender<SpeechOut>,
}
#[async_trait]
impl SpeechInput for SpeechWriter {
    async fn push(&self, pcm: &[u8]) -> Result<(), ProtocolError> {
        self.hub.authorize(&self.caller, |g| g.tasks)?;
        if !pcm.len().is_multiple_of(2) || pcm.len() > 256 * 1024 {
            return Err(error(
                ErrorCode::InvalidRequest,
                "Expected bounded PCM16LE audio",
            ));
        }
        let mut state = self.state.lock().await;
        if state.total + pcm.len() as u64 > 32 * 1024 * 1024 {
            return Err(error(
                ErrorCode::LimitExceeded,
                "Speech session reached its audio limit",
            ));
        }
        let samples: Vec<_> = pcm
            .as_chunks::<2>()
            .0
            .iter()
            .map(|b| i16::from_le_bytes([b[0], b[1]]))
            .collect();
        let mut shutdown = self.hub.inner.shutdown.subscribe();
        if *shutdown.borrow() {
            return Err(error(
                ErrorCode::Expired,
                "Øphio is shutting down on the computer",
            ));
        }
        let session = state
            .session
            .as_mut()
            .ok_or_else(|| error(ErrorCode::AlreadyResolved, "Speech session ended"))?;
        let result = tokio::select! { result = session.push_pcm(&samples) => result.map_err(crate::runtime::speech_error), _ = shutdown.changed() => Err(error(ErrorCode::Expired, "Øphio is shutting down on the computer")) };
        let segments = match result {
            Ok(segments) => segments,
            Err(error) => {
                state.session = None;
                state.engine = None;
                state.audio = None;
                let _ = self.tx.try_send(SpeechOut::Error {
                    message: error.message.clone(),
                });
                return Err(error);
            }
        };
        state.total += pcm.len() as u64;
        if let Some(audio) = &mut state.audio {
            audio.extend_from_slice(pcm);
        }
        for segment in segments {
            self.tx
                .try_send(SpeechOut::Segment(segment))
                .map_err(|_| error(ErrorCode::LimitExceeded, "Speech receiver fell behind"))?;
        }
        Ok(())
    }
    async fn finish(&self) {
        let mut state = self.state.lock().await;
        let Some(mut session) = state.session.take() else {
            return;
        };
        let _engine = state.engine.take();
        if self.hub.authorize(&self.caller, |g| g.tasks).is_err() {
            state.audio = None;
            let _ = self.tx.try_send(SpeechOut::Error {
                message: "This device no longer has the Agent tasks permission".into(),
            });
            return;
        }
        let mut shutdown = self.hub.inner.shutdown.subscribe();
        if *shutdown.borrow() {
            state.audio = None;
            return;
        }
        let finished = tokio::select! { result = session.finish() => result, _ = shutdown.changed() => { state.audio = None; return; } };
        match finished {
            Ok(segments) => {
                for s in segments {
                    if self.tx.try_send(SpeechOut::Segment(s)).is_err() {
                        return;
                    }
                }
            }
            Err(error) => {
                state.audio = None;
                let _ = self.tx.try_send(SpeechOut::Error {
                    message: crate::runtime::speech_error(error).message,
                });
                return;
            }
        }
        let artifact = if let Some(audio) = state.audio.take() {
            match self
                .hub
                .put_artifact(&wav(&audio), "audio/wav", None, self.caller.principal)
                .await
            {
                Ok(a) => Some(a.artifact_id),
                Err(_) => {
                    let _ = self.tx.try_send(SpeechOut::Error {
                        message: "Audio retention failed".into(),
                    });
                    return;
                }
            }
        } else {
            None
        };
        let _ = self.tx.try_send(SpeechOut::Finished {
            audio_artifact: artifact,
        });
    }
    async fn cancel(&self) {
        let mut state = self.state.lock().await;
        state.session = None;
        state.engine = None;
        state.audio = None;
    }
}
/// A page travels in one control frame of at most 1 MiB, and its items carry
/// agent output. It holds the newest items that fit this budget, always at
/// least one, and reports the rest as more.
const TIMELINE_PAGE_BYTES: usize = 768 * 1024;
/// The file a save conflict carries, whole in its encoding, that one control
/// frame can carry once wrapped in the save's progress event.
const CONFLICT_BYTES: usize = 768 * 1024;
/// A save's result is journaled, so a conflict over a note too large for a
/// frame would close the phone's connection at every replay. The note is left
/// out instead; the phone reads it, and that read fails cleanly if too large.
fn conflict_fitted(outcome: protocol::library::SaveOutcome) -> protocol::library::SaveOutcome {
    use protocol::library::SaveOutcome::Conflict;
    match outcome {
        Conflict {
            current: Some(file),
        } if !serde_json::to_vec(&file).is_ok_and(|v| v.len() <= CONFLICT_BYTES) => {
            Conflict { current: None }
        }
        outcome => outcome,
    }
}
fn timeline_page(mut items: Vec<TimelineItem>, has_more: bool) -> QueryData {
    let (mut kept, mut size) = (0, 0usize);
    for item in items.iter().rev() {
        size = size.saturating_add(serde_json::to_vec(item).map_or(usize::MAX, |v| v.len() + 1));
        if kept > 0 && size > TIMELINE_PAGE_BYTES {
            break;
        }
        kept += 1;
    }
    let has_more = has_more || items.len() > kept;
    items.drain(..items.len() - kept);
    QueryData::TaskTimeline { items, has_more }
}
pub(crate) fn library_error(e: ports::LibraryError) -> ProtocolError {
    error(e.code(), "Library operation failed")
}
pub(crate) fn terminal_error(e: ports::TerminalError) -> ProtocolError {
    if matches!(e, ports::TerminalError::Exited) {
        error(e.code(), "Terminal has exited")
            .with_detail(serde_json::json!({"terminal_state": "exited"}))
    } else {
        error(e.code(), "Terminal operation failed")
    }
}
fn validate_params(p: StreamParams) -> Result<(), ProtocolError> {
    if p.max_width == 0
        || p.max_height == 0
        || !(1..=100).contains(&p.quality)
        || !(1..=60).contains(&p.max_fps)
        || !(1..=8).contains(&p.window)
        || p.region.is_some_and(|r| !r.valid())
    {
        Err(error(ErrorCode::InvalidRequest, "Invalid media parameters"))
    } else {
        Ok(())
    }
}
/// A capability check the person can read: the summary says what they can
/// or cannot do; the tool's own reason stays in `detail`, shown on request.
fn readiness(
    area: ReadinessArea,
    label: &str,
    (can, cannot): (&str, &str),
    a: &Availability,
) -> ReadinessCheck {
    let summary = match a {
        Availability::Available => can.to_owned(),
        Availability::TemporarilyUnavailable { .. } => format!("{cannot} right now"),
        Availability::Incompatible { .. } => format!("{cannot} with these versions"),
        Availability::Unknown { .. } => {
            format!("Not known whether {}", can.replacen("You ", "you ", 1))
        }
        Availability::Unsupported { .. }
        | Availability::PermissionMissing { .. }
        | Availability::Revoked => cannot.to_owned(),
    };
    ReadinessCheck {
        area,
        label: label.into(),
        status: if a.is_available() {
            CheckStatus::Passed
        } else if matches!(a, Availability::Unknown { .. }) {
            CheckStatus::Unknown
        } else if matches!(
            a,
            Availability::PermissionMissing { .. } | Availability::Unsupported { .. }
        ) {
            CheckStatus::NotRun
        } else {
            CheckStatus::Failed
        },
        detail: match a {
            Availability::PermissionMissing { .. } => Some("Needs setup.".into()),
            Availability::Unsupported { reason }
            | Availability::TemporarilyUnavailable { reason }
            | Availability::Incompatible { reason }
            | Availability::Unknown { reason } => Some(reason.clone()),
            _ => None,
        },
        remedy: match a {
            Availability::PermissionMissing { remedy } => Some(remedy.clone()),
            // The desktop ends its sharing, at a screen lock for one; the
            // command that started it starts it again.
            Availability::Revoked if area == ReadinessArea::Graphics => Some(format!(
                "Run {} desktop enable on the computer to share the desktop again.",
                ports::COMMAND
            )),
            Availability::Revoked => Some("Enable the feature again on the computer".into()),
            _ => None,
        },
        summary: Some(summary),
    }
}
fn wav(pcm: &[u8]) -> Vec<u8> {
    let mut b = Vec::with_capacity(44 + pcm.len());
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(36 + pcm.len() as u32).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&16000u32.to_le_bytes());
    b.extend_from_slice(&32000u32.to_le_bytes());
    b.extend_from_slice(&2u16.to_le_bytes());
    b.extend_from_slice(&16u16.to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&(pcm.len() as u32).to_le_bytes());
    b.extend_from_slice(pcm);
    b
}

#[cfg(test)]
mod readiness_tests {
    use super::*;

    #[test]
    fn setup_and_unsupported_checks_are_neutral_and_do_not_repeat_remedies() {
        let setup = readiness(
            ReadinessArea::Graphics,
            "Desktop",
            ("You can watch the desktop", "You can’t watch the desktop"),
            &Availability::PermissionMissing {
                remedy: "Run ophio desktop enable on the computer.".into(),
            },
        );
        assert_eq!(setup.status, CheckStatus::NotRun);
        assert_eq!(setup.detail.as_deref(), Some("Needs setup."));
        assert_ne!(setup.detail, setup.remedy);
        let unsupported = readiness(
            ReadinessArea::Accounts,
            "Unsupported agent",
            ("Codex can start tasks", "Codex can’t start tasks"),
            &Availability::Unsupported {
                reason: "This version does not support this agent.".into(),
            },
        );
        assert_eq!(unsupported.status, CheckStatus::NotRun);
        assert_eq!(
            unsupported.detail.as_deref(),
            Some("This version does not support this agent.")
        );
        assert_eq!(unsupported.remedy, None);
        let stopped = readiness(
            ReadinessArea::Graphics,
            "Input",
            (
                "You can use the mouse and keyboard",
                "You can’t use the mouse and keyboard",
            ),
            &Availability::TemporarilyUnavailable {
                reason: "The input service stopped.".into(),
            },
        );
        assert_eq!(stopped.status, CheckStatus::Failed);
        // The person reads what they can do; the tool's reason stays in detail.
        assert_eq!(
            [&setup, &unsupported, &stopped].map(|c| c.summary.as_deref().unwrap()),
            [
                "You can’t watch the desktop",
                "Codex can’t start tasks",
                "You can’t use the mouse and keyboard right now",
            ]
        );
        assert_eq!(
            stopped.detail.as_deref(),
            Some("The input service stopped.")
        );
        let unknown = readiness(
            ReadinessArea::Graphics,
            "Capture",
            ("You can watch the desktop", "You can’t watch the desktop"),
            &Availability::Unknown {
                reason: "The portal did not answer.".into(),
            },
        );
        assert_eq!(
            unknown.summary.as_deref(),
            Some("Not known whether you can watch the desktop")
        );
        let ended = readiness(
            ReadinessArea::Graphics,
            "Capture",
            ("You can watch the desktop", "You can’t watch the desktop"),
            &Availability::Revoked,
        );
        assert_eq!(ended.status, CheckStatus::Failed);
        assert_eq!(
            ended.remedy,
            Some(format!(
                "Run {} desktop enable on the computer to share the desktop again.",
                ports::COMMAND
            ))
        );
        let revoked = readiness(
            ReadinessArea::Channels,
            "Telegram",
            (
                "You can reach agents from Telegram",
                "You can’t reach agents from Telegram",
            ),
            &Availability::Revoked,
        );
        assert_eq!(
            revoked.remedy.as_deref(),
            Some("Enable the feature again on the computer")
        );
    }

    #[tokio::test]
    async fn missing_bridge_blocks_work_and_approvals_but_not_starting_read_only_modes() {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/readiness-tests")
            .join(uuid::Uuid::now_v7().to_string());
        let hub = CompanionHub::builder()
            .data_dir(&path)
            .clock(std::sync::Arc::new(crate::testing::FakeClock::default()))
            .harness(std::sync::Arc::new(crate::testing::FakeHarness::default()))
            .build()
            .await
            .unwrap();
        hub.refresh_readiness().await.unwrap();
        let QueryData::Harnesses { harnesses } = hub
            .query(&Caller::local_owner(), Query::Harnesses)
            .await
            .unwrap()
        else {
            panic!("catalog")
        };
        let h = &harnesses[0];
        assert!(h.capabilities.start_run.is_available());
        let work = h.modes.iter().find(|m| m.mode == TaskMode::Work).unwrap();
        assert!(matches!(
            work.availability,
            Availability::PermissionMissing { .. }
        ));
        assert_eq!(work.availability, h.capabilities.native_approvals);
        assert_eq!(work.availability, h.capabilities.computer_use_delegation);
        let QueryData::Readiness { report } = hub
            .query(&Caller::local_owner(), Query::Readiness)
            .await
            .unwrap()
        else {
            panic!("report")
        };
        let check = report
            .checks
            .iter()
            .find(|c| c.label.ends_with(" Work"))
            .unwrap();
        assert_eq!(check.status, CheckStatus::NotRun);
        assert!(
            check
                .remedy
                .as_deref()
                .unwrap()
                .contains("permission prompts")
        );
        let delegation = report
            .checks
            .iter()
            .find(|c| c.label.ends_with(" computer use") && c.area == ReadinessArea::Accounts)
            .unwrap();
        assert!(
            delegation
                .summary
                .as_deref()
                .unwrap()
                .ends_with(" can’t use the computer for you")
        );
        for check in report.checks {
            assert!(check.detail.is_none() || check.detail != check.remedy);
            assert!(check.summary.is_some() || check.area == ReadinessArea::Network);
        }
        hub.shutdown().await.unwrap();
        drop(hub);
        std::fs::remove_dir_all(path).unwrap();
    }
}
