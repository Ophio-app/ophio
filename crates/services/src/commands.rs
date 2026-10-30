// SPDX-License-Identifier: Apache-2.0
use crate::{CompanionHub, api::Caller, error, unsupported};
use protocol::{auth::*, command::*, model::*, queue::*, *};
use std::path::PathBuf;

#[derive(Default)]
struct Prepared {
    project_path: Option<PathBuf>,
    vcs: Option<VcsInfo>,
    download: Option<crate::transfers::DownloadPrepared>,
    /// What a new task starts from, read again from the computer.
    start: Option<crate::launchers::Resolved>,
    /// The new folder of a task started on its own.
    one_off: Option<PathBuf>,
    /// The folder of a one-off being deleted, still there.
    delete_folder: Option<PathBuf>,
    /// What a task linked to GitHub starts with, read from GitHub.
    github: Option<crate::github::PreparedLink>,
    /// Where a checklist file went.
    delivered: Option<String>,
    /// What disabling an item in Ophio keeps of it.
    disable: Option<crate::disabled::Stored>,
    /// The project's review rules, for a review task.
    review_rules: Option<crate::review_rules::LoadedRules>,
}

impl CompanionHub {
    pub(crate) async fn admit(&self, caller: &Caller, envelope: CommandEnvelope) -> CommandReceipt {
        self.admit_received(caller, envelope, self.inner.clock.now(), None)
            .await
    }

    pub(crate) async fn admit_received(
        &self,
        caller: &Caller,
        envelope: CommandEnvelope,
        received: protocol::time::Timestamp,
        refusal: Option<ProtocolError>,
    ) -> CommandReceipt {
        self.admit_with_followup(caller, envelope, received, refusal, None)
            .await
    }

    /// Recheck a queued delivery in the same transaction that admits its turn.
    pub(crate) async fn admit_with_followup(
        &self,
        caller: &Caller,
        envelope: CommandEnvelope,
        received: protocol::time::Timestamp,
        refusal: Option<ProtocolError>,
        followup: Option<&crate::followups::Followup>,
    ) -> CommandReceipt {
        let id = envelope.command_id;
        // Set on this computer's clock as the command arrives, whatever the
        // sender's clock says, when the sender gives a time to live.
        let deadline = envelope.deadline(received);
        let reject = |e| CommandReceipt {
            command_id: id,
            status: ReceiptStatus::Rejected,
            seq: None,
            result: None,
            error: Some(e),
        };
        if *self.inner.shutdown.borrow() {
            return reject(error(
                ErrorCode::TemporarilyUnavailable,
                "Øphio is shutting down on the computer",
            ));
        }
        // Check identity before returning a deduplicated receipt: command ids
        // are not bearer capabilities and may not reveal another caller's work.
        let caller = match self.authorize(caller, |g| domain::grants::command(g, &envelope.body)) {
            Ok(c) => c,
            Err(e) => return reject(e),
        };
        match self.duplicate(&caller, &envelope) {
            Ok(Some(r)) => return r,
            Err(e) => return reject(e),
            Ok(None) => {}
        }
        if let Some(error) = refusal {
            return self.record_rejection(&caller, envelope, error);
        }
        if deadline.is_some_and(|t| t <= self.inner.clock.now()) {
            return self.record_rejection(
                &caller,
                envelope,
                error(ErrorCode::Expired, "Command expired before admission"),
            );
        }
        let prepared = match self.prepare(&caller, &envelope).await {
            Ok(p) => p,
            Err(e) => return self.record_rejection(&caller, envelope, e),
        };
        // A one-off's folder is made before admission; one no task came to
        // use, because admission failed or another copy of the command won,
        // is removed again.
        let one_off = prepared.one_off.clone();
        let mut kept = false;
        let now = self.inner.clock.now();
        let result = self.write(|tx| {
            let caller = Self::current_caller(&tx.read(), &caller)?;
            if !domain::grants::command(&caller.grants, &envelope.body) {
                return Err(error(ErrorCode::PermissionDenied, "Grant was revoked").into());
            }
            if let Some(existing) = tx.read().command(id)? {
                return Ok((dedupe(&tx.read(), existing, &caller, &envelope)?, false));
            }
            if deadline.is_some_and(|expires| now >= expires) {
                return Err(error(ErrorCode::Expired, "Command expired before admission").into());
            }
            if let Some(f) = followup {
                f.require_current(&tx.read())?;
                if f.kind == crate::followups::FollowupKind::Wake
                    && matches!(envelope.body, Command::ResumeTask { .. })
                {
                    let task: Task = tx.read().require("task", f.task_id)?;
                    if crate::followups::waiting_for_turn(&tx.read(), &task, true)? {
                        // A writer or queued turn may have arrived after the
                        // guard's read. Even a read-only wake waits its turn.
                        return Err(error(
                            ErrorCode::Conflict,
                            "The wake is waiting for its next safe turn",
                        )
                        .into());
                    }
                }
            }
            let (immediate, pending) = self.apply(tx, &caller, &envelope, prepared)?;
            kept = true;
            let seq = tx.emit(Event::CommandProgress {
                result: None,
                error: None,
                command_id: id,
                stage: CommandStage::HostAccepted,
                detail: None,
            })?;
            let receipt = CommandReceipt {
                command_id: id,
                status: ReceiptStatus::Accepted,
                seq: Some(seq),
                result: immediate,
                error: None,
            };
            tx.command(&store::StoredCommand {
                result: None,
                error: None,
                envelope: retained_envelope(&tx.read(), &envelope)?,
                principal: caller.principal,
                receipt: receipt.clone(),
                stage: if pending {
                    CommandStage::HostAccepted
                } else {
                    CommandStage::Observed
                },
                pending,
            })?;
            if !pending {
                tx.emit(Event::CommandProgress {
                    command_id: id,
                    stage: CommandStage::Observed,
                    result: receipt.result.clone(),
                    error: None,
                    detail: None,
                })?;
            }
            Ok((receipt, pending))
        });
        if let Some(folder) = one_off
            && (result.is_err() || !kept)
        {
            let _ = std::fs::remove_dir(folder);
        }
        match result {
            Ok((receipt, pending)) => {
                if kept && matches!(envelope.body, Command::SubmitReport { .. }) {
                    self.clear_carried_crashes();
                }
                if kept
                    && matches!(envelope.body, Command::WithdrawReport { .. })
                    && let Ok(mut db) = self.inner.db.lock()
                    && let Err(e) = db.forget_report_bytes()
                {
                    tracing::error!(error = %e, "Report storage cleanup failed");
                }
                if kept
                    && matches!(
                        envelope.body,
                        Command::SetReportVerdict { .. } | Command::LookIntoReport { .. }
                    )
                {
                    self.inner.reports.inbox_wake.notify_one();
                }
                // The copies follow the setting in the background.
                if kept && matches!(envelope.body, Command::SetAgentUpdates { .. }) {
                    self.update_agents();
                }
                // A project's agent files are looked for when next listed.
                if kept
                    && matches!(
                        envelope.body,
                        Command::AddProject { .. }
                            | Command::RemoveProject { .. }
                            | Command::DeleteOneOff { .. }
                            | Command::PromoteOneOff { .. }
                    )
                {
                    self.forget_agent_files();
                }
                // What the person did on a checklist goes to its agent now.
                if kept
                    && matches!(
                        envelope.body,
                        Command::ChecklistTick { .. }
                            | Command::ChecklistAnswer { .. }
                            | Command::ChecklistAsk { .. }
                    )
                {
                    self.wake_guard();
                }
                // A local review's verifier starts, or what it wrote is
                // looked at, once the person has said how its run ended.
                if kept && let Command::ReconcileTask { task_id, .. } = &envelope.body {
                    self.reconciled_locally(*task_id);
                }
                // What waited behind the task taken out, or moved, may start now.
                if kept
                    && matches!(
                        envelope.body,
                        Command::ArchiveTask { .. }
                            | Command::QueueRemove { .. }
                            | Command::QueueMove { .. }
                            | Command::QueueSetDependencies { .. }
                    )
                {
                    self.progress_queues();
                }
                if pending {
                    let hub = self.clone();
                    let caller = caller.clone();
                    let receipt = receipt.clone();
                    self.spawn(async move {
                        hub.execute(caller, envelope, deadline, receipt).await;
                    });
                }
                receipt
            }
            Err(e) => self.record_rejection(&caller, envelope, e),
        }
    }
    fn duplicate(
        &self,
        caller: &Caller,
        envelope: &CommandEnvelope,
    ) -> Result<Option<CommandReceipt>, ProtocolError> {
        self.read(|r| {
            r.command(envelope.command_id)?
                .map(|c| dedupe(r, c, caller, envelope).map_err(store::Error::from))
                .transpose()
        })
    }
    fn record_rejection(
        &self,
        caller: &Caller,
        envelope: CommandEnvelope,
        e: ProtocolError,
    ) -> CommandReceipt {
        let receipt = CommandReceipt {
            command_id: envelope.command_id,
            status: ReceiptStatus::Rejected,
            seq: None,
            result: None,
            error: Some(e),
        };
        self.write(|tx| {
            if let Some(existing) = tx.read().command(envelope.command_id)? {
                return Ok(dedupe(&tx.read(), existing, caller, &envelope)?);
            }
            tx.command(&store::StoredCommand {
                result: None,
                error: None,
                envelope: retained_envelope(&tx.read(), &envelope)?,
                principal: caller.principal,
                receipt: receipt.clone(),
                stage: CommandStage::Failed,
                pending: false,
            })?;
            Ok(receipt.clone())
        })
        .unwrap_or(receipt)
    }
    async fn prepare(
        &self,
        caller: &Caller,
        envelope: &CommandEnvelope,
    ) -> Result<Prepared, ProtocolError> {
        let mut result = Prepared::default();
        if matches!(
            envelope.body,
            Command::SetDeviceGrants { expected: None, .. }
        ) {
            return Err(error(
                ErrorCode::IncompatibleVersion,
                "Update the app before changing device permissions; this request is missing the current permissions",
            ));
        }
        // Counted, never copied: a command can carry a password.
        let mut size = Counted(0);
        serde_json::to_writer(&mut size, envelope).map_err(crate::internal)?;
        if size.0 > 4 * 1024 * 1024 {
            return Err(error(ErrorCode::LimitExceeded, "Command is too large"));
        }
        match &envelope.body {
            Command::AddProject { host_path, .. } => {
                let path = (tokio::fs::canonicalize(host_path).await)
                    .map(ports::library::plain)
                    .map_err(|_| {
                        error(
                            ErrorCode::PathNotAllowed,
                            "Project directory is unavailable",
                        )
                    })?;
                if !tokio::fs::metadata(&path)
                    .await
                    .map_err(crate::internal)?
                    .is_dir()
                {
                    return Err(error(
                        ErrorCode::InvalidRequest,
                        "Project path must be a directory",
                    ));
                }
                self.project_folder(&path)?;
                result.vcs = vcs_info(&path).await;
                result.project_path = Some(path);
            }
            Command::DeleteOneOff { project_id } => {
                result.delete_folder = self.one_off_to_delete(*project_id).await?;
            }
            Command::CreateDownload { source, .. } => {
                result.download = Some(self.prepare_download(caller, source).await?)
            }
            Command::ChecklistDeliver { project_id, sha256 } => {
                result.delivered = Some(self.deliver_checklist_file(*project_id, sha256).await?)
            }
            Command::SetDisabledInOphio {
                kind,
                host_path,
                disabled: true,
            } => result.disable = Some(self.disable_target(*kind, host_path).await?),
            Command::CreateTask {
                harness,
                kind,
                mode,
                target_id,
                bundle,
                conversation,
                access,
                github,
                ..
            } => {
                // An agent a conversation asks only reads, and is held to
                // what the conversation reaches. A source review may inherit
                // the same read-only grant while keeping its GitHub Plan mode.
                let restricted_review = github.as_deref().is_some_and(|link| {
                    matches!(link, protocol::github::GithubLink::Find { .. }) && !link.uses_it()
                }) && access.as_ref().is_some_and(|a| !a.writes());
                if access.is_some()
                    && *conversation != Some(true)
                    && *mode != Some(TaskMode::Ask)
                    && !restricted_review
                {
                    return Err(error(
                        ErrorCode::InvalidRequest,
                        "Only a conversation is held to chosen folders; a task follows its mode",
                    ));
                }
                if *kind == TaskKind::ComputerUse && mode.unwrap_or_default() != TaskMode::Work {
                    return Err(error(
                        ErrorCode::InvalidRequest,
                        "Computer-use tasks require Work mode",
                    ));
                }
                if *conversation == Some(true) && *kind != TaskKind::Coding {
                    return Err(error(
                        ErrorCode::InvalidRequest,
                        "A conversation is with a coding agent, not computer use",
                    ));
                }
                if !self.inner.harnesses.iter().any(|a| a.kind() == *harness) {
                    return Err(unsupported("Selected harness"));
                }
                let catalog: Vec<protocol::capability::HarnessInfo> =
                    self.read(|r| Ok(r.get("meta", "harnesses")?.unwrap_or_default()))?;
                let info = catalog
                    .iter()
                    .find(|info| info.kind == *harness)
                    .ok_or_else(|| unsupported("Selected harness readiness"))?;
                domain::availability(&info.capabilities.start_run)?;
                domain::task_mode(info, mode.unwrap_or_default())?;
                if matches!(
                    info.account,
                    protocol::capability::AccountReadiness::SignedOut { .. }
                ) {
                    return Err(error(
                        ErrorCode::AccountNotReady,
                        "Sign in with the native harness before starting work",
                    ));
                }
                if *kind == TaskKind::ComputerUse
                    && !info.capabilities.computer_use_delegation.is_available()
                {
                    return Err(unsupported(
                        "Native computer-use delegation for the selected harness",
                    ));
                }
                if *kind == TaskKind::ComputerUse
                    && (target_id.is_none()
                        || self.desktop_option().is_none()
                        || self.inner.media.is_none()
                        || self.inner.bridge.is_none())
                {
                    return Err(unsupported("Computer use target and bridge"));
                }
                if bundle.is_some() {
                    self.validate_bundle_for(caller, bundle.as_deref().expect("checked above"))?;
                }
            }
            Command::SendMessage {
                bundle: Some(bundle),
                ..
            }
            | Command::QueueContinuation {
                bundle: Some(bundle),
                ..
            } => self.validate_bundle_for(caller, bundle)?,
            // An annotation alone is a complete message, as it is for a new task.
            Command::SendMessage {
                text, bundle: None, ..
            } if text.trim().is_empty() => {
                return Err(error(
                    ErrorCode::InvalidRequest,
                    "A message requires text or an annotation",
                ));
            }
            Command::ResumeTask {
                task_id,
                bundle,
                mode,
                model,
                effort,
                carried_plan,
                read_only,
                ..
            } => {
                let task: Task = self.read(|r| r.require("task", task_id))?;
                let catalog: Vec<protocol::capability::HarnessInfo> =
                    self.read(|r| Ok(r.get("meta", "harnesses")?.unwrap_or_default()))?;
                let info = catalog
                    .iter()
                    .find(|info| info.kind == task.harness)
                    .ok_or_else(|| unsupported("Selected harness readiness"))?;
                if *read_only {
                    self.read(|r| {
                        read_only_turn(r, &task, mode.is_some() || carried_plan.is_some())
                    })?;
                }
                if model.is_some() || effort.is_some() {
                    if *read_only {
                        return Err(error(
                            ErrorCode::InvalidRequest,
                            "A read-only turn keeps the task's model",
                        ));
                    }
                    if model.is_some() {
                        domain::availability(&info.capabilities.model_selection)?;
                    }
                    if effort.is_some() {
                        domain::availability(&info.capabilities.effort_selection)?;
                    }
                    let (model, effort) = switched(&task, model, effort);
                    domain::model_choice(info, model.as_deref(), effort.as_deref())?;
                }
                domain::task_mode(
                    info,
                    match read_only {
                        true => TaskMode::Ask,
                        false => mode.unwrap_or(task.mode),
                    },
                )?;
                if let Some(bundle) = bundle {
                    self.validate_bundle_for(caller, bundle)?;
                }
            }
            Command::LibrarySave { .. }
            | Command::LibraryCreate { .. }
            | Command::LibraryRename { .. }
            | Command::LibraryDelete { .. }
            | Command::MakeReusable { .. }
            | Command::LibraryRestore { .. }
            | Command::CreateReusableDraft { .. }
            | Command::LibraryRecoveryDiscard { .. }
                if self.inner.library.is_none() =>
            {
                return Err(unsupported("Library"));
            }
            Command::OpenTerminal { .. } | Command::CloseTerminal { .. }
                if self.inner.terminal.is_none() =>
            {
                return Err(unsupported("Terminal"));
            }
            Command::RequestHumanLease { .. }
            | Command::ReleaseLease { .. }
            | Command::PauseInput { .. }
            | Command::TakeOver { .. }
            | Command::ReturnToAgent { .. }
            | Command::StartPrivateStep { .. }
            | Command::EndPrivateStep { .. }
            | Command::WithdrawObservation { .. }
            | Command::LockDesktop
            | Command::UnlockDesktop { .. }
            | Command::CaptureTarget { .. }
                if self.desktop_option().is_none() =>
            {
                self.desktop()?;
            }
            Command::RegisterPreview { url, .. } => {
                crate::preview::validate(url)?;
            }
            Command::CheckPreview { .. } if self.inner.preview.is_none() => {
                return Err(unsupported("Preview checker"));
            }
            Command::CheckPreview { start: true, .. } if self.inner.terminal.is_none() => {
                return Err(unsupported("Terminal"));
            }
            Command::ApproveChannelLink { link_code }
            | Command::RefuseChannelLink { link_code } => self.check_link_code(link_code)?,
            Command::SetChannelProject { .. } => {
                self.chat_project_settings()?;
            }
            Command::Github { action } => {
                self.github()?;
                if let protocol::github::GithubAction::SetSettings { settings, .. } = action {
                    crate::github::valid_settings(settings)?;
                }
            }
            _ => {}
        }
        if let Command::CreateTask {
            project_id,
            kind,
            harness,
            place,
            github: Some(link),
            ..
        } = &envelope.body
        {
            if place.is_some() {
                return Err(error(
                    ErrorCode::InvalidRequest,
                    "A task on its own isn't in the project's repository, so it can't be linked to GitHub",
                ));
            }
            let catalog: Vec<protocol::capability::HarnessInfo> =
                self.read(|r| Ok(r.get("meta", "harnesses")?.unwrap_or_default()))?;
            let info = catalog.iter().find(|info| info.kind == *harness);
            result.github = Some(self.prepare_link(*project_id, *kind, info, link).await?);
        }
        // Last, once nothing else can refuse the task, as it may make a folder.
        if let Command::CreateTask {
            project_id,
            kind,
            start,
            place,
            placement,
            ..
        } = &envelope.body
            && (start.is_some() || place.is_some())
        {
            if *kind != TaskKind::Coding {
                return Err(unsupported(
                    "An agent, skill or one-off for this kind of task",
                ));
            }
            if let Some(protocol::launchers::TaskPlace::OneOff { name, .. }) = place {
                valid_label(name)?;
                if !matches!(placement, TaskPlacement::StartNow) {
                    return Err(error(
                        ErrorCode::InvalidRequest,
                        "A task on its own starts now; it has no Next up to wait in",
                    ));
                }
            }
            let (start, folder) = self
                .prepare_start(*project_id, start.as_deref(), place.as_ref())
                .await?;
            result.start = start;
            result.one_off = folder;
        }
        // A review task is given the project's review rules, read from its
        // default branch, never from what is being reviewed. A conversation
        // is never given them.
        if let Command::CreateTask {
            project_id,
            review: Some(role),
            conversation,
            github,
            ..
        } = &envelope.body
            && *conversation != Some(true)
            // A verifier is given the rules of the review it checks.
            && !(*role == protocol::review::ReviewRole::Verify
                && self.read(|r| verified_review(r, envelope.command_id))?.is_some())
        {
            let folder = match result.one_off.clone() {
                Some(folder) => Some(folder),
                None => (self.read(|r| r.get::<Project>("project", *project_id)))?
                    .map(|p| PathBuf::from(p.host_path)),
            };
            // A check of a pull request, or a finder looking at one,
            // follows the rules of the branch it goes into, for the files
            // it changes.
            use protocol::github::{FindScope, GithubLink};
            let scope = match github.as_deref() {
                Some(
                    GithubLink::Check { number }
                    | GithubLink::Find {
                        scope: FindScope::Pr { number },
                        ..
                    },
                ) => self.pr_scope(*project_id, *number).await,
                _ => None,
            };
            let (base, changed) = scope.unzip();
            let changed = changed.unwrap_or_default();
            if let Some(folder) = folder {
                let rules = crate::review_rules::load(&folder, base.as_deref(), &changed).await;
                result.review_rules = Some(rules);
            }
        }
        Ok(result)
    }
    fn apply(
        &self,
        tx: &mut store::Write<'_>,
        caller: &Caller,
        envelope: &CommandEnvelope,
        prepared: Prepared,
    ) -> store::Result<(Option<CommandResult>, bool)> {
        use Command::*;
        let now = self.inner.clock.now();
        let expected = envelope.expected_revision;
        let mut result = None;
        let mut pending = false;
        // A rename to a longer title makes a task heavier in every first load.
        let mut heavier = false;
        match &envelope.body {
            ReviewContribution { input } => {
                result = Some(self.review_contribution(tx, caller, input)?);
            }
            PrepareContribution {
                operation_id,
                approval,
                workspace_choice,
            } => {
                let (view, run) = self.prepare_contribution(
                    tx,
                    caller,
                    operation_id,
                    approval,
                    workspace_choice.as_ref(),
                    envelope.command_id,
                )?;
                result = Some(view);
                pending = run;
            }
            SetContributionRoot { root_id } => {
                self.set_contribution_root(tx, caller, *root_id)?;
            }
            CancelContribution { operation_id } => {
                result = Some(self.cancel_contribution(tx, caller, operation_id)?);
            }
            SetVoice { browser, desktop } => {
                self.set_voice_settings(tx, *browser, *desktop, expected)?
            }
            Voice { .. } => pending = true,
            SetAgentUpdates { enabled } => self.set_agent_updates(tx, *enabled, expected)?,
            SetAgentPreferences { questions } => {
                self.set_agent_preferences(tx, *questions, expected)?
            }
            SetLaunchDefaults { defaults } => self.set_launch_defaults(tx, defaults, expected)?,
            SetDisabledInOphio {
                host_path,
                disabled,
                ..
            } => Self::set_disabled(tx, host_path, prepared.disable.filter(|_| *disabled))?,
            ChecklistTick {
                project_id,
                item_id,
                step_id,
                done,
            } => crate::checklist::tick(tx, *project_id, item_id, step_id.as_deref(), *done, now)?,
            ChecklistAnswer {
                project_id,
                item_id,
                answer,
            } => crate::checklist::answer(tx, *project_id, item_id, answer, now)?,
            ChecklistAsk {
                project_id,
                item_id,
                text,
            } => {
                let note_id = envelope.command_id.to_string();
                crate::checklist::ask(tx, *project_id, item_id.as_deref(), text, note_id, now)?
            }
            ChecklistDeliver { .. } => {
                let to = prepared
                    .delivered
                    .ok_or_else(|| error(ErrorCode::Internal, "Delivery missing"))?;
                result = Some(CommandResult::Delivered { to });
            }
            RemoveChecklist { project_id } => crate::checklist::remove(tx, *project_id, now)?,
            SetFileDelivery { delivery } => {
                let home = self.inner.home.as_deref();
                crate::checklist::set_file_delivery(tx, delivery, home, expected)?
            }
            SetNotificationSettings {
                off,
                bundle,
                projects,
            } => self.set_notification_settings(tx, off, *bundle, projects, expected)?,
            SubmitReport { bundle } => self.submit_report(tx, bundle)?,
            AnswerReportQuestion {
                report_id,
                question_id,
                answer,
            } => self.answer_report(tx, *report_id, *question_id, answer)?,
            WithdrawReport { report_id } => self.withdraw_report(tx, *report_id)?,
            SetReportVerdict { report_id, verdict } => {
                self.set_report_verdict(tx, caller, *report_id, verdict)?
            }
            LookIntoReport { report_id } => self.look_into_report(tx, caller, *report_id)?,
            SetReportSettings { sending, ring } => {
                self.set_report_settings(tx, *sending, *ring, expected)?
            }
            AddProject { name, .. } => {
                let path = prepared
                    .project_path
                    .ok_or_else(|| error(ErrorCode::Internal, "Missing canonical project path"))?;
                if tx
                    .read()
                    .list::<Project>("project")?
                    .iter()
                    .any(|p| p.host_path == path)
                {
                    return Err(error(ErrorCode::Conflict, "Project is already registered").into());
                }
                let project_id = ProjectId::new();
                let name = name.clone().unwrap_or_else(|| {
                    path.file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned()
                });
                valid_label(&name)?;
                let project = Project {
                    project_id,
                    name,
                    host_path: path.to_string_lossy().into_owned(),
                    default_harness: None,
                    default_model: None,
                    default_effort: None,
                    active_writer: None,
                    vcs: prepared.vcs,
                    stage: None,
                    created_at: now,
                    one_off: false,
                    home: false,
                };
                add_project(tx, project, now)?;
                result = Some(CommandResult::ProjectAdded { project_id });
            }
            UpdateProjectDefaults {
                project_id,
                harness,
                model,
                effort,
            } => {
                let mut p: Project = tx.read().require("project", project_id)?;
                for label in [model, effort].into_iter().flatten() {
                    valid_label(label)?;
                }
                p.default_harness = *harness;
                p.default_model = model.clone();
                p.default_effort = effort.clone();
                tx.put("project", project_id, &p)?;
                tx.emit(Event::ProjectUpserted { project: p })?;
            }
            PromoteOneOff { project_id } => {
                let mut p: Project = tx.read().require("project", project_id)?;
                if !p.one_off {
                    return Err(error(ErrorCode::Conflict, "This project isn’t a one-off").into());
                }
                p.one_off = false;
                tx.put("project", project_id, &p)?;
                tx.emit(Event::ProjectUpserted { project: p })?;
            }
            RemoveProject { project_id } => {
                let p: Project = tx.read().require("project", project_id)?;
                if p.home {
                    return Err(error(ErrorCode::Conflict, "Home is always there").into());
                }
                Self::forget_project(tx, *project_id, "remove the project")?
            }
            DeleteOneOff { project_id } => {
                let p: Project = tx.read().require("project", project_id)?;
                if !p.one_off {
                    return Err(error(ErrorCode::Conflict, "This project isn’t a one-off").into());
                }
                Self::forget_project(tx, *project_id, "delete the one-off")?;
                // Its history goes with it, out of every list but Archived.
                for mut task in tx
                    .read()
                    .list::<Task>("task")?
                    .into_iter()
                    .filter(|t| t.project_id == *project_id && t.state != TaskState::Archived)
                {
                    task.state = TaskState::Archived;
                    task.revision += 1;
                    task.updated_at = now;
                    tx.put("task", task.task_id, &task)?;
                    tx.emit(Event::TaskUpserted { task })?;
                }
                if let Some(folder) = prepared.delete_folder {
                    if folder != std::path::Path::new(&p.host_path) {
                        return Err(
                            error(ErrorCode::Internal, "The one-off’s folder changed").into()
                        );
                    }
                    tx.put(DELETE_FOLDER, envelope.command_id, &folder)?;
                    pending = true;
                }
            }
            CreateTask {
                project_id,
                kind,
                mode,
                title,
                prompt,
                harness,
                model,
                effort,
                launch_policy,
                branch,
                mission,
                target_id,
                placement,
                bundle,
                start: _,
                place,
                browser,
                conversation,
                access,
                agent_chosen,
                github: _,
                review,
            } => {
                let p: Project = match (place, prepared.one_off) {
                    (None, _) => tx.read().require("project", project_id)?,
                    (Some(protocol::launchers::TaskPlace::OneOff { name, .. }), Some(folder)) => {
                        if tx.read().get::<Project>("project", project_id)?.is_some() {
                            return Err(error(
                                ErrorCode::Conflict,
                                "A one-off needs a new project id",
                            )
                            .into());
                        }
                        let project = Project {
                            project_id: *project_id,
                            name: name.trim().to_owned(),
                            host_path: folder.to_string_lossy().into_owned(),
                            default_harness: None,
                            default_model: None,
                            default_effort: None,
                            active_writer: None,
                            vcs: None,
                            stage: None,
                            created_at: now,
                            one_off: true,
                            home: false,
                        };
                        add_project(tx, project.clone(), now)?;
                        project
                    }
                    (Some(_), None) => {
                        return Err(error(ErrorCode::Internal, "Missing one-off folder").into());
                    }
                };
                let link = prepared.github;
                // A pull request taken over is worked on where it is.
                let pr_branch = (link.as_ref())
                    .filter(|l| matches!(l.github.link, protocol::github::GithubLink::Pr { .. }))
                    .and_then(|l| l.branch.clone());
                if let (Some(pr), Some(given)) = (&pr_branch, branch)
                    && pr.name != given.name
                {
                    return Err(error(
                        ErrorCode::InvalidRequest,
                        format!("This pull request's work goes on `{}`", pr.name),
                    )
                    .into());
                }
                let branch = pr_branch
                    .or_else(|| branch.clone())
                    .or_else(|| link.as_ref().and_then(|l| l.branch.clone()));
                if let Some(branch) = &branch {
                    if *kind != TaskKind::Coding {
                        return Err(unsupported("A branch for this kind of task").into());
                    }
                    if !crate::git::branch_name_ok(&branch.name) {
                        return Err(error(
                            ErrorCode::InvalidRequest,
                            "That isn’t a branch name Git accepts",
                        )
                        .into());
                    }
                }
                if *browser == Some(true) && *kind != TaskKind::Coding {
                    return Err(unsupported("The agent browser for this kind of task").into());
                }
                if launch_policy
                    .is_some_and(|policy| *kind != TaskKind::Coding || !policy.supports(*harness))
                {
                    return Err(unsupported("Launch flags for this task and agent").into());
                }
                if mission.as_ref().is_some_and(|m| {
                    m.bounds
                        .iter()
                        .any(|b| b.enforcement == Enforcement::Enforced)
                }) {
                    return Err(unsupported("Native proof of mission-bound enforcement").into());
                }
                if prompt.trim().is_empty() && bundle.is_none() {
                    return Err(error(
                        ErrorCode::InvalidRequest,
                        "A task requires a prompt or annotation",
                    )
                    .into());
                }
                let title = (title.clone())
                    .or_else(|| link.as_ref().and_then(|l| l.title.clone()))
                    .unwrap_or_else(|| prompt.chars().take(80).collect());
                valid_label(&title)?;
                for label in [model, effort].into_iter().flatten() {
                    valid_label(label)?;
                }
                // Read-only work, and work for strangers, begins in Plan.
                let mode = match &link {
                    Some(l) if l.plan => TaskMode::Plan,
                    _ => mode.unwrap_or_default(),
                };
                // The project's own model and effort fill in only for its usual
                // agent, and only when the person didn't choose for themselves.
                let fill = p.default_harness == Some(*harness) && *agent_chosen != Some(true);
                let mut task = Task {
                    github: link.map(|l| Box::new(l.github)),
                    task_id: TaskId::new(),
                    project_id: *project_id,
                    kind: *kind,
                    mode,
                    title,
                    state: TaskState::Accepted,
                    revision: 1,
                    harness: *harness,
                    model: model.clone().or(p.default_model.filter(|_| fill)),
                    effort: effort.clone().or(p.default_effort.filter(|_| fill)),
                    launch_policy: *launch_policy,
                    branch,
                    created_by: caller.principal,
                    created_at: now,
                    updated_at: now,
                    current_run: None,
                    target_id: *target_id,
                    mission: mission.clone(),
                    attention_count: 0,
                    status_line: None,
                    wake: None,
                    asked_by: None,
                    start: None,
                    browser: *kind == TaskKind::Coding && browser.unwrap_or(true),
                    conversation: conversation.unwrap_or(false),
                    access: (access.clone()).map(protocol::access::ConversationAccess::normalized),
                    access_blocked: None,
                    review: None,
                };
                // Its part in a review, with the rules it is given. Only the
                // first prompt of each new session carries them.
                if let Some(role) = review.filter(|_| !task.conversation) {
                    // A verifier is given the very rules of the review it
                    // checks; any other review those read for it now.
                    let of = match role {
                        protocol::review::ReviewRole::Verify => {
                            verified_review(&tx.read(), envelope.command_id)?
                        }
                        _ => None,
                    };
                    let (section, used) = match of {
                        Some(of) => {
                            let section: Option<String> =
                                tx.read().get(crate::review_rules::PROMPT, of)?;
                            let reviewed: Option<Task> = tx.read().get("task", of)?;
                            let used = (reviewed.and_then(|t| t.review)).and_then(|r| r.rules);
                            (section, used)
                        }
                        None => {
                            let rules = prepared.review_rules;
                            let section = (rules.as_ref()).map(crate::review_rules::prompt_section);
                            (section, rules.map(|r| r.used))
                        }
                    };
                    if let Some(section) = section.filter(|s| !s.is_empty()) {
                        tx.put(crate::review_rules::PROMPT, task.task_id, &section)?;
                    }
                    task.review = Some(Box::new(protocol::review::TaskReview {
                        role,
                        rules: used,
                        of_task: None,
                        verified_by: None,
                        findings: Vec::new(),
                        not_run: Vec::new(),
                    }));
                }
                if let Some(start) = prepared.start {
                    // Every run of the task starts from this, as it was read now.
                    tx.put("task_start", task.task_id, &start.run)?;
                    task.start = Some(start.shown);
                }
                let catalog: Vec<protocol::capability::HarnessInfo> =
                    tx.read().get("meta", "harnesses")?.unwrap_or_default();
                if let Some(h) = catalog.iter().find(|h| h.kind == task.harness) {
                    domain::model_choice(h, task.model.as_deref(), task.effort.as_deref())?;
                }
                // A check of a pull request, the watcher's or the person's,
                // holds its merge and decides it from now, even waiting in
                // Next up.
                crate::github_watch::asked_to_check(tx, &task, None, now)?;
                crate::github::tried_again(tx, &task, now)?;
                tx.put("prompt", task.task_id, &(prompt.clone(), bundle.clone()))?;
                let mut queue_item_id = None;
                match placement {
                    TaskPlacement::StartNow => {
                        self.new_run(tx, &mut task, envelope.command_id, None, None, false)?;
                        pending = true;
                        // A finder started now whose fixes start by themselves
                        // resumes a paused Next up, where they would otherwise
                        // wait unseen. Its review says so.
                        let finds = (task.github.as_ref()).and_then(|g| match g.link {
                            protocol::github::GithubLink::Find { fix_findings, .. } => {
                                Some(fix_findings)
                            }
                            _ => None,
                        });
                        if let Some(chosen) = finds
                            && crate::github::fixes_by_itself(&tx.read(), *project_id, chosen)?
                        {
                            let mut q: QueueView = tx.read().require("queue", project_id)?;
                            if q.paused {
                                q.paused = false;
                                q.pause_reason = None;
                                crate::runs::bump_queue_revision(tx, &mut q)?;
                                tx.put("queue", project_id, &q)?;
                                crate::runs::sync_queue_attention(tx, &q, now)?;
                                tx.emit(Event::QueueChanged { queue: q })?;
                            }
                        }
                    }
                    TaskPlacement::Queue {
                        position,
                        authorization,
                    } => {
                        pending = *authorization == QueueAuthorization::AutoStart;
                        queue_item_id = Some(self.enqueue(
                            tx,
                            caller,
                            &task,
                            *position,
                            *authorization,
                            QueuedRequest {
                                prompt: prompt.clone(),
                                bundle: bundle.clone(),
                            },
                        )?);
                    }
                }
                if let Some(reviewed) = bundle.as_deref() {
                    crate::results::link_bundle(tx, task.task_id, task.current_run, reviewed)?;
                }
                tx.put("task", task.task_id, &task)?;
                tx.emit(Event::TaskUpserted { task: task.clone() })?;
                crate::checklist::made(tx, envelope.command_id, task.task_id)?;
                let message_seq = tx.emit(Event::TimelineAppended {
                    task_id: task.task_id,
                    item: TimelineItem {
                        seq: 0,
                        at: now,
                        run_id: task.current_run,
                        item: TimelineBody::UserMessage {
                            text: prompt.clone(),
                            bundle: bundle.clone(),
                            delivery: DeliveryState::HostAccepted,
                            carried_plan: None,
                        },
                    },
                })?;
                self.track_start_message(tx, task.task_id, task.current_run, message_seq)?;
                result = Some(CommandResult::TaskCreated {
                    task_id: task.task_id,
                    queue_item_id,
                });
            }
            SetConversationAccess { task_id, access } => {
                let mut t: Task = tx.read().require("task", task_id)?;
                // A task carried on from a conversation keeps its access.
                if !t.conversation && t.access.is_none() {
                    return Err(error(
                        ErrorCode::InvalidRequest,
                        "Only a conversation is held to chosen folders; a task follows its mode",
                    )
                    .into());
                }
                self.access_reach(&tx.read(), access)?;
                t.access = Some(access.clone().normalized());
                pending = true;
                // What it was kept from may be let now; it asks again if not.
                t.access_blocked = None;
                t.revision += 1;
                t.updated_at = now;
                tx.put("task", t.task_id, &t)?;
                tx.emit(Event::TaskUpserted { task: t })?;
            }
            RenameTask { task_id, .. } | ArchiveTask { task_id } => {
                let mut t: Task = tx.read().require("task", task_id)?;
                match &envelope.body {
                    RenameTask {
                        expected_title: Some(replaced),
                        ..
                    } => {
                        if *replaced != t.title {
                            return Err(error(
                                ErrorCode::StaleRevision,
                                format!(
                                    "Not renamed: this task was renamed to “{}” in the meantime.",
                                    t.title
                                ),
                            )
                            .with_detail(serde_json::json!({ "current_title": t.title }))
                            .into());
                        }
                    }
                    _ => domain::revision(expected, t.revision)?,
                }
                if let RenameTask { title, .. } = &envelope.body {
                    valid_label(title)?;
                    heavier = crate::history::size(title) > crate::history::size(&t.title);
                    t.title = title.clone();
                } else {
                    if t.current_run.is_some_and(|id| {
                        tx.read()
                            .get::<Run>("run", id)
                            .ok()
                            .flatten()
                            .is_some_and(|r| {
                                !r.state.is_terminal() || crate::runs::still_finishing(&r)
                            })
                    }) {
                        return Err(error(
                            ErrorCode::Conflict,
                            "Active or uncertain tasks cannot be archived",
                        )
                        .into());
                    }
                    // Kept so Undo can bring it back to where it was.
                    if t.state != TaskState::Archived {
                        tx.put(ARCHIVED_FROM, task_id, &t.state)?;
                    }
                    t.state = TaskState::Archived;
                    // Nothing is passed on to an archived task.
                    crate::carry_on::let_go(tx, *task_id, now)?;
                    crate::asking::let_go(tx, *task_id)?;
                    crate::checklist::let_go(tx, *task_id, now)?;
                    if t.wake.take().is_some() {
                        tx.delete(
                            crate::followups::FOLLOWUP,
                            crate::followups::wake_key(*task_id),
                        )?;
                    }
                }
                t.revision += 1;
                t.updated_at = now;
                tx.put("task", task_id, &t)?;
                if let Some(mut q) = tx.read().get::<QueueView>("queue", t.project_id)?
                    && q.items
                        .iter()
                        .any(|item| item.task_id == *task_id && item.is_pending())
                {
                    if t.state == TaskState::Archived {
                        // An archived task never starts. Left in Next up, it
                        // would hold every task behind it.
                        let removed: Vec<_> = (q.items.iter())
                            .filter(|item| item.task_id == *task_id && item.is_pending())
                            .map(|item| item.item_id)
                            .collect();
                        q.items
                            .retain(|item| item.task_id != *task_id || !item.is_pending());
                        // Only what waited for it asks again: the rest keep
                        // the go-ahead they were given, Fix by itself's too.
                        let withdrawn = domain::queue::withdraw(&mut q, &removed);
                        for item_id in removed.into_iter().chain(withdrawn) {
                            tx.delete("queue_permit", item_id)?;
                        }
                    } else {
                        // A new name changes nothing the person let start.
                        crate::runs::carry_permits(tx, &t, t.revision - 1)?;
                    }
                    crate::runs::bump_queue_revision(tx, &mut q)?;
                    self.refresh_queue(tx, &mut q)?;
                    tx.put("queue", q.project_id, &q)?;
                    crate::runs::sync_queue_attention(tx, &q, now)?;
                    tx.emit(Event::QueueChanged { queue: q })?;
                }
                if t.state == TaskState::Archived {
                    crate::runs::resolve_task_attention(tx, *task_id, now)?;
                }
                t.attention_count = crate::runs::blocking_attention_count(&tx.read(), *task_id)?;
                tx.put("task", task_id, &t)?;
                tx.emit(Event::TaskUpserted { task: t })?;
            }
            UnarchiveTask { task_id } => {
                let mut t: Task = tx.read().require("task", task_id)?;
                if t.state != TaskState::Archived {
                    return Err(error(ErrorCode::Conflict, "This task isn't archived").into());
                }
                // Only a task that had ended comes back: one archived while it
                // waited in Next up has lost its place there.
                let was = tx
                    .read()
                    .get::<TaskState>(ARCHIVED_FROM, task_id)?
                    .filter(|s| {
                        matches!(
                            s,
                            TaskState::Completed
                                | TaskState::Failed
                                | TaskState::Interrupted
                                | TaskState::ReconciliationRequired
                        )
                    })
                    .ok_or_else(|| {
                        error(
                            ErrorCode::Conflict,
                            "This task can't be brought back; it stays in Archived to read",
                        )
                    })?;
                tx.delete(ARCHIVED_FROM, task_id)?;
                t.state = was;
                t.revision += 1;
                t.updated_at = now;
                t.attention_count = crate::runs::blocking_attention_count(&tx.read(), *task_id)?;
                tx.put("task", task_id, &t)?;
                tx.emit(Event::TaskUpserted { task: t })?;
            }
            ResumeTask {
                task_id,
                text,
                bundle,
                mode,
                model,
                effort,
                carried_plan,
                read_only,
            } => {
                let mut t: Task = tx.read().require("task", task_id)?;
                domain::revision(expected, t.revision)?;
                if *read_only {
                    read_only_turn(&tx.read(), &t, mode.is_some() || carried_plan.is_some())?;
                }
                if !matches!(
                    t.state,
                    TaskState::Accepted
                        | TaskState::Interrupted
                        | TaskState::Completed
                        | TaskState::Failed
                ) {
                    return Err(error(
                        ErrorCode::Conflict,
                        "Task cannot be resumed in its current state",
                    )
                    .into());
                }
                if t.kind == TaskKind::ComputerUse && !caller.grants.computer_use {
                    return Err(error(
                        ErrorCode::PermissionDenied,
                        "This device needs the AI computer use permission",
                    )
                    .into());
                }
                if let Some(mode) = mode {
                    // A conversation carried on in Work becomes a task, with
                    // all that was said.
                    let carried_on = t.conversation && *mode == TaskMode::Work;
                    if !carried_on
                        && (t.mode != TaskMode::Plan
                            || *mode != TaskMode::Work
                            || t.state != TaskState::Completed)
                    {
                        return Err(error(
                            ErrorCode::InvalidRequest,
                            "Only a finished Plan can be carried out in Work mode",
                        )
                        .into());
                    }
                    // What a finder, triage or check reports is acted on by
                    // the issues and fixes it leads to, never carried out.
                    if crate::github::writes_nothing(&t) {
                        return Err(error(
                            ErrorCode::InvalidRequest,
                            "This task only reads and reports, so it isn't carried out in Work mode",
                        )
                        .into());
                    }
                    t.mode = *mode;
                    // It stays held to what the conversation was, and may
                    // now change its own project: what it was carried on to do.
                    if t.conversation
                        && let Some(mut access) = t.access.take()
                    {
                        access.write.push(t.project_id);
                        t.access = Some(access.normalized());
                        t.access_blocked = None;
                    }
                    t.conversation = false;
                }
                if model.is_some() || effort.is_some() {
                    (t.model, t.effort) = switched(&t, model, effort);
                }
                // Continuing a pull request the watcher stopped on starts its
                // rounds again, and answers what it raised. A check asked for
                // meanwhile still decides whether it merges.
                let deciding = match t.github.as_deref().and_then(|g| g.pr.as_ref()) {
                    Some(pr) => crate::github_watch::deciding(&tx.read(), &t, pr.number)?.is_some(),
                    None => false,
                };
                if let Some(github) = t.github.as_deref_mut()
                    && github.stage == Some(protocol::github::PrStage::Capped)
                {
                    github.stage = Some(match deciding {
                        true => protocol::github::PrStage::Checking,
                        false => protocol::github::PrStage::Babysitting,
                    });
                    github.rounds = 0;
                    // The person picked it up: the review starts its rounds
                    // again, keeping what it found.
                    if let Some(review) = github.review.as_mut() {
                        review.round = 0;
                        review.escalated = None;
                    }
                    let watch = crate::github_watch::WATCH;
                    if let Some(mut w) = tx
                        .read()
                        .get::<crate::github_watch::Watch>(watch, task_id)?
                    {
                        w.progress_at = now;
                        tx.put(watch, task_id, &w)?;
                    }
                    for mut item in tx.read().list::<AttentionItem>("attention")? {
                        if item.task_id == Some(*task_id)
                            && item.kind == AttentionKind::WaitingForYou
                            && item.resolved_at.is_none()
                            && tx
                                .read()
                                .get::<String>(crate::alerts::HEADLINE, item.attention_id)?
                                .is_some()
                        {
                            item.resolved_at = Some(now);
                            tx.put("attention", item.attention_id, &item)?;
                            tx.emit(Event::AttentionResolved {
                                attention_id: item.attention_id,
                            })?;
                        }
                    }
                }
                if let Some(carried) = carried_plan {
                    let from_task = tx
                        .read()
                        .get::<Run>("run", carried.run_id)?
                        .is_some_and(|run| run.task_id == *task_id);
                    if *mode != Some(TaskMode::Work)
                        || !from_task
                        || !text
                            .as_deref()
                            .is_some_and(|text| quotes_plan(text, &carried.text))
                    {
                        return Err(error(
                            ErrorCode::InvalidRequest,
                            "The plan to carry out doesn't match the message",
                        )
                        .into());
                    }
                }
                // A task that has run starts its next turn when the person
                // writes to it, taking along the message waiting for it in
                // Next up.
                if t.state == TaskState::Accepted {
                    self.check_pending_start(tx, &t, false)?;
                }
                let run_id = self.new_run(
                    tx,
                    &mut t,
                    envelope.command_id,
                    text.clone(),
                    bundle.clone(),
                    *read_only,
                )?;
                if text.is_some() || bundle.is_some() {
                    // Persist with admission, before dispatch, so dedupe and recovery
                    // cannot append this message a second time or lose its markup.
                    let message_seq = tx.emit(Event::TimelineAppended {
                        task_id: *task_id,
                        item: TimelineItem {
                            seq: 0,
                            at: now,
                            run_id: Some(run_id),
                            item: TimelineBody::UserMessage {
                                text: text.clone().unwrap_or_default(),
                                bundle: bundle.clone(),
                                delivery: DeliveryState::HostAccepted,
                                carried_plan: carried_plan.clone(),
                            },
                        },
                    })?;
                    self.track_start_message(tx, *task_id, Some(run_id), message_seq)?;
                }
                pending = true;
                t.attention_count = crate::runs::blocking_attention_count(&tx.read(), *task_id)?;
                tx.put("task", task_id, &t)?;
                tx.emit(Event::TaskUpserted { task: t })?;
            }
            CheckHelpers { run_id } => {
                self.check_helpers(tx, *run_id, expected, caller.principal)?;
            }
            ReconcileTask {
                task_id,
                resolution,
            } => {
                let mut task: Task = tx.read().require("task", task_id)?;
                domain::revision(expected, task.revision)?;
                if task.state != TaskState::ReconciliationRequired {
                    return Err(
                        error(ErrorCode::Conflict, "Task does not require reconciliation").into(),
                    );
                }
                let id = task
                    .current_run
                    .ok_or_else(|| error(ErrorCode::Conflict, "No run to reconcile"))?;
                let mut run: Run = tx.read().require("run", id)?;
                if *resolution == Reconciliation::RetryAsNewRun {
                    return Err(error(ErrorCode::Conflict, "First reconcile the old run as completed or failed, then explicitly resume").into());
                }
                run.state = if *resolution == Reconciliation::MarkCompleted {
                    RunState::Completed
                } else {
                    RunState::Failed
                };
                run.human_resolution = Some(HumanResolution {
                    resolution: *resolution,
                    at: now,
                    by: caller.principal,
                });
                run.holds_writer = false;
                run.ended_at = Some(now);
                // The person has checked what the run left running.
                run.cleanup_pending = false;
                self.finish_message_delivery(tx, id, *task_id)?;
                task.state = if run.state == RunState::Completed {
                    TaskState::Completed
                } else {
                    TaskState::Failed
                };
                if let Some(before) = tx.read().get::<TaskState>(crate::runs::SIDE_TURN, id)? {
                    task.state = before;
                    tx.delete(crate::runs::SIDE_TURN, id)?;
                }
                task.revision += 1;
                task.updated_at = now;
                for mut item in tx.read().list::<AttentionItem>("attention")? {
                    // Older helper notices lacked a run link. Only include those
                    // created during the run the person is deciding now.
                    if (item.run_id == Some(id)
                        || (item.run_id.is_none()
                            && item.task_id == Some(*task_id)
                            && item.created_at >= run.admitted_at))
                        && item.kind == AttentionKind::ReconciliationRequired
                        && item.resolved_at.is_none()
                    {
                        item.resolved_at = Some(now);
                        tx.put("attention", item.attention_id, &item)?;
                        tx.emit(Event::AttentionResolved {
                            attention_id: item.attention_id,
                        })?;
                    }
                }
                let mode = crate::runs::run_mode(&run, &task);
                let state = run.state;
                tx.put("run", id, &run)?;
                tx.emit(Event::RunUpserted { run: Box::new(run) })?;
                // A local review, or its verifier, goes on as if its run had
                // ended this way by itself: findings are checked before a fix
                // waiting for them starts, and ones that weren't hold it.
                let epoch = self.inner.epoch;
                crate::review_loop::reviewed_locally(tx, &task, id, (state, mode), (epoch, now))?;
                self.unfinished_locally(tx, &task, (state, mode), now)?;
                crate::review_loop::local_verifier_ended(tx, &task, id, state, epoch)?;
                if mode == TaskMode::Work {
                    self.release_writer(tx, task.project_id, id)?;
                } else {
                    self.tidy_queue(tx, task.project_id)?;
                }
                task.attention_count = crate::runs::blocking_attention_count(&tx.read(), *task_id)?;
                tx.put("task", task_id, &task)?;
                tx.emit(Event::TaskUpserted { task })?;
            }
            // Between turns, a task that is watching stops watching.
            StopTask { task_id, .. } if crate::watching::between_turns(&tx.read(), *task_id)? => {
                let stopped: Task = tx.read().require("task", task_id)?;
                let answer = stopped.wake.as_ref().is_some_and(|w| w.answer);
                let listed =
                    !crate::checklist::waited_on(&tx.read(), stopped.project_id, *task_id)?
                        .is_empty();
                crate::watching::stop_watching(tx, *task_id, now)?;
                crate::asking::let_go(tx, *task_id)?;
                crate::checklist::let_go(tx, *task_id, now)?;
                let text = match answer {
                    true if listed => {
                        "Stopped waiting. The checklist keeps its items, and ticking them off won't start the agent again."
                    }
                    true => {
                        "Stopped waiting. The other agent's answer stays in its own task, and Ophio won't wake the agent."
                    }
                    false => "Stopped watching. Ophio won't wake the agent.",
                };
                tx.emit(Event::TimelineAppended {
                    task_id: *task_id,
                    item: TimelineItem {
                        seq: 0,
                        at: now,
                        run_id: None,
                        item: TimelineBody::Note { text: text.into() },
                    },
                })?;
            }
            StopTask { task_id, force } => {
                let mut task: Task = tx.read().require("task", task_id)?;
                // Ophio's own stop of a turn that held the project too long
                // leaves what it went on without, and what it asked to be
                // woken for, and Next up carries on.
                let by_guard = tx
                    .read()
                    .get::<bool>(crate::watchdog::GUARD_STOP, envelope.command_id)?
                    .is_some();
                let run_id = task
                    .current_run
                    .ok_or_else(|| error(ErrorCode::Conflict, "Task has no active run"))?;
                if let Some(expected) = expected
                    && !crate::runs::names_run(&tx.read(), &task, run_id, expected)?
                {
                    domain::revision(Some(expected), task.revision)?;
                }
                let mut run: Run = tx.read().require("run", run_id)?;
                // What a finished turn left running is still stoppable, and
                // stopping it leaves the turn's outcome as it was.
                let helpers_only = crate::runs::still_finishing(&run);
                if run.state.is_terminal() && !helpers_only {
                    return Err(error(ErrorCode::AlreadyResolved, "Run already ended").into());
                }
                if *force && !run.force_stoppable {
                    return Err(error(ErrorCode::Unsupported, "Force stop is only available for an agent process that Øphio started. Stop observed sessions in the agent’s terminal on the computer.").into());
                }
                let count = tx.read().get::<u32>("stop_count", run_id)?.unwrap_or(0);
                tx.put(
                    "stop_count",
                    run_id,
                    &count.saturating_add(u32::from(!force)),
                )?;
                if *force {
                    tx.put("force_stopped", run_id, &true)?;
                }
                tx.put("stopped", run_id, &true)?;
                // Stop acts on the run the person stopped, even if another
                // turn of the task has started by the time it runs.
                tx.put("stop_run", envelope.command_id, &run_id)?;
                for mut a in tx.read().list::<Approval>("approval")? {
                    let carried = crate::carry_on::carried_on(&tx.read(), a.approval_id)?;
                    if a.run_id == run_id
                        && a.state == ApprovalState::Pending
                        && !(by_guard && carried)
                    {
                        tx.delete(crate::carry_on::CARRIED_ON, a.approval_id)?;
                        a.state = ApprovalState::Cancelled;
                        a.revision += 1;
                        a.resolved_at = Some(now);
                        tx.put("approval", a.approval_id, &a)?;
                        tx.emit(Event::ApprovalUpserted {
                            approval: a.clone(),
                        })?;
                        crate::runs::resolve_attention(tx, a.approval_id, now)?;
                    }
                }
                tx.delete("run_token", run_id)?;
                if !helpers_only {
                    run.state = RunState::Stopping;
                    tx.put("run", run_id, &run)?;
                    tx.emit(Event::RunUpserted { run: Box::new(run) })?;
                }
                if !by_guard && task.wake.take().is_some() {
                    tx.delete(
                        crate::followups::FOLLOWUP,
                        crate::followups::wake_key(*task_id),
                    )?;
                }
                if !by_guard {
                    crate::asking::let_go(tx, *task_id)?;
                }
                task.revision += 1;
                task.updated_at = now;
                tx.put("task", task_id, &task)?;
                tx.emit(Event::TaskUpserted { task: task.clone() })?;
                if !by_guard {
                    crate::checklist::let_go(tx, *task_id, now)?;
                    let mut q: QueueView = tx.read().require("queue", task.project_id)?;
                    q.paused = true;
                    q.pause_reason = Some(format!(
                        "Next up paused when you stopped “{}”, so nothing starts by itself. Resume when you’re ready.",
                        task.title
                    ));
                    domain::queue::invalidate(&mut q);
                    tx.put("queue", q.project_id, &q)?;
                    crate::runs::sync_queue_attention(tx, &q, now)?;
                    tx.emit(Event::QueueChanged { queue: q })?;
                }
                pending = true;
            }
            SendMessage {
                task_id,
                text,
                bundle,
                ..
            } => {
                let task: Task = tx.read().require("task", task_id)?;
                if task.kind == TaskKind::ComputerUse && !caller.grants.computer_use {
                    return Err(error(
                        ErrorCode::PermissionDenied,
                        "This device needs the AI computer use permission",
                    )
                    .into());
                }
                if let Some(expected) = expected {
                    domain::revision(Some(expected), task.revision)?;
                }
                if !matches!(task.state, TaskState::Working | TaskState::AwaitingDecision) {
                    return Err(error(ErrorCode::Conflict, "Task has no running turn").into());
                }
                let run_id = task
                    .current_run
                    .ok_or_else(|| error(ErrorCode::Conflict, "Task has no running turn"))?;
                // The message goes to the turn it was accepted for. A turn
                // started after that one ended never receives it.
                tx.put("message_run", envelope.command_id, &run_id)?;
                self.admit_followup(tx, envelope.command_id, &task, text, bundle.as_deref())?;
                pending = true;
            }
            ResolveApproval {
                approval_id,
                decision,
            } => {
                let a: Approval = tx.read().require("approval", approval_id)?;
                if a.state == ApprovalState::Expired || a.expires_at.is_some_and(|t| t <= now) {
                    return Err(error(ErrorCode::Expired, "Approval expired").into());
                }
                if a.state == ApprovalState::Cancelled {
                    return Err(error(ErrorCode::Expired, crate::runs::ARRIVED_LATE).into());
                }
                if a.state != ApprovalState::Pending {
                    return Err(
                        error(ErrorCode::AlreadyResolved, "Approval already resolved").into(),
                    );
                }
                domain::revision(expected, a.revision.into())?;
                // A code goes into the code prompt, which fills it.
                if a.kind == ApprovalKind::LoginCode
                    && !matches!(decision, ApprovalDecision::Deny { .. })
                {
                    return Err(error(
                        ErrorCode::InvalidRequest,
                        "Type the code in the code prompt.",
                    )
                    .into());
                }
                // A login, and a post on GitHub as the person, are allowed
                // from the phone.
                if let Some(what) = match a.kind {
                    ApprovalKind::LoginUse => Some("a login"),
                    ApprovalKind::GithubPost => Some("a post on GitHub"),
                    _ => None,
                } {
                    match decision {
                        ApprovalDecision::Deny { .. } => {}
                        ApprovalDecision::Allow
                            if !matches!(caller.principal, Principal::Channel { .. }) => {}
                        ApprovalDecision::Allow => {
                            return Err(error(
                                ErrorCode::PermissionDenied,
                                format!("Allow {what} on your phone, not in a chat."),
                            )
                            .into());
                        }
                        ApprovalDecision::Answer { .. } => {
                            return Err(error(
                                ErrorCode::InvalidRequest,
                                format!("Allow or decline {what}."),
                            )
                            .into());
                        }
                    }
                }
                let task: Task = tx.read().require("task", a.task_id)?;
                let mode = match tx.read().get::<Run>("run", a.run_id)? {
                    Some(run) => crate::runs::run_mode(&run, &task),
                    None => task.mode,
                };
                // Denying only stops an effect; anything else directs the desktop agent.
                if task.kind == TaskKind::ComputerUse
                    && !caller.grants.computer_use
                    && !matches!(decision, ApprovalDecision::Deny { .. })
                {
                    return Err(error(
                        ErrorCode::PermissionDenied,
                        "This device needs the AI computer use permission",
                    )
                    .into());
                }
                if mode != TaskMode::Work
                    && !matches!(decision, ApprovalDecision::Deny { .. })
                    && !(a.kind == ApprovalKind::Question
                        && matches!(decision, ApprovalDecision::Answer { .. }))
                {
                    return Err(error(ErrorCode::Unsupported,
                        "Ask and Plan cannot approve writes or expanded permissions; start a Work task instead").into());
                }
                if let ApprovalDecision::Answer { answers, .. } = decision
                    && !answers.is_empty()
                    && !answers_fit(&a.questions, answers)
                {
                    return Err(error(
                        ErrorCode::InvalidRequest,
                        "Answer each question once, with one of its options or your own words",
                    )
                    .into());
                }
                if a.questions.iter().any(|q| q.secret)
                    && matches!(decision, ApprovalDecision::Answer { .. })
                    && crate::carry_on::carried_on(&tx.read(), *approval_id)?
                {
                    return Err(
                        error(ErrorCode::Expired, crate::carry_on::PRIVATE_ANSWER_EXPIRED).into(),
                    );
                }
                if tx
                    .read()
                    .get::<CommandId>("approval_claim", approval_id)?
                    .is_some()
                {
                    return Err(error(
                        ErrorCode::AlreadyResolved,
                        "Approval decision already admitted",
                    )
                    .into());
                }
                tx.put("approval_claim", approval_id, &envelope.command_id)?;
                pending = true;
            }
            AcknowledgeAttention { attention_id } => {
                let mut a: AttentionItem = tx.read().require("attention", attention_id)?;
                if a.report_id.is_some()
                    && (a.task_id.is_some() || crate::report_inbox::is_inbox(&a))
                    && !matches!(
                        caller.principal,
                        Principal::Device { .. } | Principal::LocalOwner
                    )
                {
                    return Err(error(
                        ErrorCode::PermissionDenied,
                        "Only the person can close a report's request",
                    )
                    .into());
                }
                a.resolved_at = Some(now);
                tx.put("attention", attention_id, &a)?;
                tx.emit(Event::AttentionResolved {
                    attention_id: *attention_id,
                })?;
                if let Some(task_id) = a.task_id {
                    crate::runs::recount_attention(tx, task_id)?;
                }
            }
            QueueMove { item_id, .. }
            | QueueRemove { item_id }
            | QueueSetDependencies { item_id, .. }
            | QueueSetAuthorization { item_id, .. } => {
                let mut q = tx
                    .read()
                    .list::<QueueView>("queue")?
                    .into_iter()
                    .find(|q| q.items.iter().any(|i| i.item_id == *item_id))
                    .ok_or_else(|| error(ErrorCode::NotFound, "Queue item not found"))?;
                domain::revision(expected, q.revision)?;
                let index = q
                    .items
                    .iter()
                    .position(|i| i.item_id == *item_id)
                    .ok_or_else(|| error(ErrorCode::NotFound, "Queue item not found"))?;
                if !q.items[index].is_pending() {
                    return Err(error(
                        ErrorCode::Conflict,
                        "An admitted queue item cannot be edited",
                    )
                    .into());
                }
                match &envelope.body {
                    QueueMove { position, .. } => {
                        if *position as usize >= q.items.len() {
                            return Err(
                                error(ErrorCode::InvalidRequest, "Invalid queue position").into()
                            );
                        }
                        let item = q.items.remove(index);
                        q.items.insert(*position as usize, item);
                        domain::queue::validate_order(&q)?;
                        domain::queue::invalidate(&mut q);
                    }
                    QueueRemove { .. } => {
                        let item = q.items.remove(index);
                        let mut task: Task = tx.read().require("task", item.task_id)?;
                        let continuation = crate::runs::queue_completion::CONTINUATION;
                        if task.state != TaskState::Accepted
                            && crate::runs::queue_completion::waits_to_start(&tx.read(), &task)?
                        {
                            // Only the message waiting goes; the task stays
                            // as it stopped.
                            tx.delete(continuation, task.task_id)?;
                            tx.delete(
                                crate::runs::queue_completion::CONTINUATION_BUNDLE,
                                task.task_id,
                            )?;
                            if let Some(message_seq) =
                                tx.read().get::<u64>("queued_start_message", task.task_id)?
                            {
                                tx.delete("queued_start_message", task.task_id)?;
                                tx.emit(Event::TimelineAppended {
                                    task_id: task.task_id,
                                    item: TimelineItem {
                                        seq: 0,
                                        at: now,
                                        run_id: None,
                                        item: TimelineBody::Delivery {
                                            message_seq,
                                            state: DeliveryState::Cancelled,
                                            withdrawn_by_stop: false,
                                            withdrawn_by_revocation: false,
                                        },
                                    },
                                })?;
                            }
                            task.revision += 1;
                            task.updated_at = now;
                            crate::github_run::let_go(tx, q.project_id, task.task_id)?;
                            tx.put("task", task.task_id, &task)?;
                            tx.emit(Event::TaskUpserted { task })?;
                            tx.delete("queue_permit", item.item_id)?;
                        } else {
                            if task.state != TaskState::Accepted || task.current_run.is_some() {
                                return Err(error(
                                    ErrorCode::Conflict,
                                    "This task already started",
                                )
                                .into());
                            }
                            task.state = TaskState::Archived;
                            task.revision += 1;
                            task.updated_at = now;
                            task.status_line = Some("Removed from Next up before starting".into());
                            crate::github_run::let_go(tx, q.project_id, task.task_id)?;
                            tx.put("task", task.task_id, &task)?;
                            tx.emit(Event::TaskUpserted { task })?;
                            tx.delete("queue_permit", item.item_id)?;
                        }
                        // Only what waited for it asks again.
                        for item_id in domain::queue::withdraw(&mut q, &[item.item_id]) {
                            tx.delete("queue_permit", item_id)?;
                        }
                        crate::runs::bump_queue_revision(tx, &mut q)?;
                    }
                    QueueSetDependencies { depends_on, .. } => {
                        domain::queue::validate_dependencies(&q, *item_id, depends_on)?;
                        q.items[index].depends_on = depends_on.clone();
                        // It, and what waits on it, ask again.
                        for item_id in domain::queue::withdraw(&mut q, &[*item_id]) {
                            tx.delete("queue_permit", item_id)?;
                        }
                        crate::runs::bump_queue_revision(tx, &mut q)?;
                    }
                    QueueSetAuthorization { authorization, .. } => {
                        crate::runs::bump_queue_revision(tx, &mut q)?;
                        q.items[index].authorization = *authorization;
                        q.items[index].authorization_expires_at =
                            (*authorization == QueueAuthorization::AutoStart).then_some(
                                protocol::time::Timestamp(now.0 + crate::runs::QUEUE_PERMIT_MS),
                            );
                        if *authorization == QueueAuthorization::AutoStart {
                            let task: Task = tx.read().require("task", q.items[index].task_id)?;
                            tx.put(
                                "queue_permit",
                                item_id,
                                &crate::runs::QueuePermit {
                                    task_revision: task.revision,
                                    queue_revision: q.revision,
                                    remaining: 1,
                                    principal: caller.principal,
                                    expires_at: Some(protocol::time::Timestamp(
                                        now.0 + crate::runs::QUEUE_PERMIT_MS,
                                    )),
                                },
                            )?;
                        } else {
                            tx.delete("queue_permit", item_id)?;
                            // Asked to wait for the person, it leaves Run all,
                            // which would only give it the go-ahead again.
                            crate::github_run::let_go(tx, q.project_id, q.items[index].task_id)?;
                        }
                    }
                    _ => unreachable!(),
                }
                // What the removed or edited item followed may be needed no longer.
                crate::runs::forget_finished(tx, &mut q)?;
                self.refresh_queue(tx, &mut q)?;
                tx.put("queue", q.project_id, &q)?;
                crate::runs::sync_queue_attention(tx, &q, now)?;
                tx.emit(Event::QueueChanged { queue: q })?;
                pending = matches!(
                    envelope.body,
                    QueueSetAuthorization {
                        authorization: QueueAuthorization::AutoStart,
                        ..
                    }
                );
            }
            QueuePause { project_id } | QueueResume { project_id } => {
                let mut q: QueueView = tx.read().require("queue", project_id)?;
                domain::revision(expected, q.revision)?;
                q.paused = matches!(envelope.body, QueuePause { .. });
                q.pause_reason = q
                    .paused
                    .then(|| "Next up is paused. Resume it to let tasks start.".into());
                crate::runs::bump_queue_revision(tx, &mut q)?;
                if q.paused {
                    tx.emit(Event::InterventionRecorded { task_id: None, target_id: None, receipt: Box::new(InterventionReceipt { kind: InterventionKind::PauseQueue, requested_at: now, facts: vec![ReceiptFact { label: "Future queue admission paused".into(), status: FactStatus::Done, detail: None }, ReceiptFact { label: "Already admitted writer".into(), status: if q.writer.is_some() { FactStatus::StillActive } else { FactStatus::NotApplicable }, detail: q.writer.map(|id| id.to_string()) }], summary: "Future queue admission is paused; admitted work keeps its existing state".into() }) })?;
                }
                pending = !q.paused;
                tx.put("queue", project_id, &q)?;
                crate::runs::sync_queue_attention(tx, &q, now)?;
                tx.emit(Event::QueueChanged { queue: q })?;
            }
            CancelMessage {
                task_id,
                message_seq,
            } => {
                self.held_message(tx, *task_id, *message_seq)?;
                pending = true;
            }
            QueueEditPrompt { item_id, prompt } => {
                self.edit_queue_prompt(tx, *item_id, prompt, expected)?;
            }
            QueueStart {
                item_id,
                task_revision,
            } => {
                self.start_queue_item(tx, caller, envelope, *item_id, *task_revision)?;
                pending = true;
            }
            QueueContinuation {
                task_id,
                text,
                bundle,
                authorization,
            } => {
                self.queue_continuation(
                    tx,
                    caller,
                    *task_id,
                    text,
                    bundle.as_deref(),
                    *authorization,
                    expected,
                )?;
                pending = *authorization == QueueAuthorization::AutoStart;
            }
            CreateUpload {
                name,
                size,
                sha256,
                destination,
                evidence,
            } => {
                if let Some(link) = evidence {
                    if !matches!(
                        destination,
                        protocol::transfer::TransferDestination::Artifact
                    ) {
                        return Err(error(
                            ErrorCode::InvalidRequest,
                            "Task evidence uploads must use artifact storage",
                        )
                        .into());
                    }
                    crate::results::validate_device_link(&tx.read(), link)?;
                }
                let record =
                    self.declare_upload(tx, name, *size, sha256, destination, caller.principal)?;
                if let Some(link) = evidence {
                    tx.put("transfer_evidence", record.transfer_id, link)?;
                }
                result = Some(CommandResult::Transfer { transfer: record });
            }
            CreateUploadReviewed {
                task_id,
                name,
                size,
                sha256,
                destination,
                collision,
            } => {
                let chosen = match collision {
                    protocol::transfer::UploadCollision::KeepBoth { name } => name,
                    _ => name,
                };
                let link = crate::results::upload_source(&tx.read(), *task_id, destination)?;
                let record =
                    self.declare_upload(tx, chosen, *size, sha256, destination, caller.principal)?;
                if let Some(link) = link {
                    tx.put("transfer_evidence", record.transfer_id, &link)?;
                }
                tx.put("upload_collision", record.transfer_id, collision)?;
                result = Some(CommandResult::Transfer { transfer: record });
            }
            CreateDownload {
                source,
                shown_in_task,
            } => {
                let p = prepared
                    .download
                    .ok_or_else(|| error(ErrorCode::Internal, "Download preparation missing"))?;
                let record = self.declare_download(tx, p, *shown_in_task, caller.principal)?;
                // Fetched to look at or keep, what an agent made stays a
                // week longer.
                if let protocol::command::DownloadSource::Artifact { artifact_id } = source {
                    crate::expiry::seen(tx, *artifact_id, self.inner.clock.now())?;
                }
                result = Some(CommandResult::Transfer { transfer: record });
            }
            CancelTransfer { transfer_id } => {
                let _: protocol::transfer::TransferRecord =
                    tx.read().require("transfer", transfer_id)?;
                pending = true;
            }
            DeleteArtifact { artifact_id } => {
                Self::delete_artifact(tx, caller, *artifact_id)?;
                pending = true;
            }
            RegisterPreview {
                project_id,
                label,
                url,
            } => {
                let _: Project = tx.read().require("project", project_id)?;
                valid_label(label)?;
                let p = PreviewEndpoint {
                    preview_id: PreviewId::new(),
                    project_id: *project_id,
                    label: label.clone(),
                    url: crate::preview::validate(url)?.to_string(),
                    reachable: None,
                    checked_at: None,
                    automatic: false,
                    start: None,
                };
                tx.put("preview", p.preview_id, &p)?;
                emit_previews(tx, *project_id)?;
                result = Some(CommandResult::PreviewRegistered {
                    preview_id: p.preview_id,
                });
            }
            RemovePreview { preview_id } => {
                let p: PreviewEndpoint = tx.read().require("preview", preview_id)?;
                tx.delete("preview", preview_id)?;
                emit_previews(tx, p.project_id)?;
                if p.automatic
                    && let Some(finder) = &self.inner.finder
                {
                    finder.removed(tx, &p)?;
                }
            }
            CheckPreview { preview_id, .. } => {
                let _: PreviewEndpoint = tx.read().require("preview", preview_id)?;
                pending = true;
            }
            RenameDevice { device_id, name } => {
                valid_label(name)?;
                // As at pairing: the name is shown in the owner's terminal,
                // where control characters could rewrite or hide what is printed.
                if name.chars().any(char::is_control) {
                    return Err(error(
                        ErrorCode::InvalidRequest,
                        "A device name can't contain line breaks or control characters",
                    )
                    .into());
                }
                let mut d: crate::identity::Device = tx.read().require("device", device_id)?;
                d.record.name = name.clone();
                tx.put("device", device_id, &d)?;
                crate::identity::emit_devices(tx)?;
            }
            RevokeDevice { device_id } => {
                let mut d: crate::identity::Device = tx.read().require("device", device_id)?;
                protect_last_manager(tx, caller, &d, false)?;
                d.record.revoked_at = Some(now);
                tx.put("device", device_id, &d)?;
                crate::identity::emit_devices(tx)?;
                pending = true;
            }
            SetDeviceGrants { device_id, .. } | ChangeDeviceGrants { device_id, .. } => {
                let mut d: crate::identity::Device = tx.read().require("device", device_id)?;
                if d.record.revoked_at.is_some() {
                    return Err(error(
                        ErrorCode::Conflict,
                        "This device's access was revoked. Pair it again to give it access",
                    )
                    .into());
                }
                let grants = match &envelope.body {
                    SetDeviceGrants {
                        expected, grants, ..
                    } => {
                        if Some(&d.record.grants) != expected.as_ref() {
                            return Err(error(ErrorCode::DeviceGrantsChanged,
                                "This device's permissions changed while you were editing. Review the current permissions and save again.").into());
                        }
                        grants.clone()
                    }
                    ChangeDeviceGrants { mask, allowed, .. } => {
                        let mut grants = d.record.grants.clone();
                        grants.apply_delta(mask, *allowed);
                        grants
                    }
                    _ => unreachable!(),
                };
                protect_last_manager(tx, caller, &d, grants.admin)?;
                // Accounts is turned on at the computer, for one device at a time.
                if grants.accounts
                    && !d.record.grants.accounts
                    && caller.principal != Principal::LocalOwner
                {
                    return Err(error(
                        ErrorCode::PermissionDenied,
                        "Only the computer can give a device access to Accounts. Run “ophio devices allow <device> accounts” there.",
                    )
                    .into());
                }
                // Losing view or input ends that device's supervision and input leases.
                pending = (d.record.grants.view && !grants.view)
                    || (d.record.grants.control_input && !grants.control_input);
                d.record.grants = grants;
                tx.put("device", device_id, &d)?;
                crate::identity::emit_devices(tx)?;
            }
            ApproveChannelLink { link_code } => {
                let p: crate::identity::PendingLink = tx.read().require("link", link_code)?;
                if p.expires <= now {
                    return Err(error(ErrorCode::Expired, "Channel link expired").into());
                }
                let outcome = crate::identity::LinkOutcome {
                    channel: p.channel,
                    approved: true,
                    expires: p.expires,
                };
                let c = ChannelIdentity {
                    id: ChannelIdentityId::new(),
                    channel: p.channel,
                    external_user: p.user,
                    external_chat: p.chat,
                    display_name: p.name,
                    linked_at: now,
                    revoked_at: None,
                    grants: domain::grants::channel_defaults(),
                    project_id: None,
                };
                tx.delete("link", link_code)?;
                tx.put("link_outcome", link_code, &outcome)?;
                tx.put("channel", c.id, &c)?;
                self.emit_identities(tx)?;
            }
            RefuseChannelLink { link_code } => {
                let p: crate::identity::PendingLink = tx.read().require("link", link_code)?;
                if p.expires <= now {
                    return Err(error(ErrorCode::Expired, "Channel link expired").into());
                }
                tx.delete("link", link_code)?;
                tx.put(
                    "link_outcome",
                    link_code,
                    &crate::identity::LinkOutcome {
                        channel: p.channel,
                        approved: false,
                        expires: p.expires,
                    },
                )?;
            }
            RevokeChannelIdentity { identity_id } => {
                let mut c: ChannelIdentity = tx.read().require("channel", identity_id)?;
                c.revoked_at = Some(now);
                tx.put("channel", identity_id, &c)?;
                self.emit_identities(tx)?;
                pending = true;
            }
            SetChannelProject {
                identity_id,
                project_id,
            } => {
                let c: ChannelIdentity = tx.read().require("channel", identity_id)?;
                if c.revoked_at.is_some() {
                    return Err(error(
                        ErrorCode::Conflict,
                        "This chat's access was revoked. Link it again to choose a project",
                    )
                    .into());
                }
                if let Some(project_id) = project_id
                    && tx.read().get::<Project>("project", project_id)?.is_none()
                {
                    return Err(error(
                        ErrorCode::NotFound,
                        "That project is no longer on this computer",
                    )
                    .into());
                }
                pending = true;
            }
            RequestHumanLease { .. }
            | ReleaseLease { .. }
            | PauseInput { .. }
            | TakeOver { .. }
            | ReturnToAgent { .. }
            | StartPrivateStep { .. }
            | EndPrivateStep { .. }
            | WithdrawObservation { .. }
            | LockDesktop
            | UnlockDesktop { .. }
            | CaptureTarget { .. }
            | LibrarySave { .. }
            | LibraryCreate { .. }
            | LibraryRename { .. }
            | LibraryDelete { .. }
            | MakeReusable { .. }
            | LibraryRestore { .. }
            | CreateReusableDraft { .. }
            | LibraryRecoveryDiscard { .. }
            | CreateAgent { .. }
            | CopySetupSkill { .. }
            | ChooseSetupSkillVersion { .. }
            | OpenTerminal { .. }
            | CloseTerminal { .. }
            | RunReadinessCheck => pending = true,
            WindowAction { .. } | Account { .. } | AgentLogin { .. } | Github { .. } => {
                pending = true
            }
            StopObservedSession {
                force,
                observed_id,
                confirmed,
            } => {
                if *force {
                    return Err(error(ErrorCode::Unsupported, "Observed sessions cannot be force-stopped. Stop it in the agent’s terminal on the computer.").into());
                }
                self.admit_observed_stop(
                    tx,
                    envelope.command_id,
                    observed_id,
                    *confirmed,
                    expected,
                )?;
                pending = true;
            }
            StopChild { run_id, child_id } => {
                self.admit_child_stop(tx, *run_id, *child_id, expected)?;
                pending = true;
            }
            SetProjectStage {
                project_id,
                stage,
                note,
            } => {
                result = Some(self.set_project_stage(
                    tx,
                    caller,
                    *project_id,
                    *stage,
                    note.as_deref(),
                    expected,
                )?);
            }
        }
        if heavier
            || matches!(
                envelope.body,
                CreateTask { .. }
                    | ResumeTask { .. }
                    | QueueStart { .. }
                    | QueueEditPrompt { .. }
                    | QueueContinuation { .. }
                    | QueueSetDependencies { .. }
            )
        {
            crate::history::admit(&tx.read())?;
        }
        if matches!(
            envelope.body,
            AddProject { .. } | UpdateProjectDefaults { .. } | SetProjectStage { .. }
        ) {
            crate::history::admit_projects(&tx.read())?;
        }
        Ok((result, pending))
    }
    pub(crate) async fn execute(
        &self,
        caller: Caller,
        envelope: CommandEnvelope,
        deadline: Option<protocol::time::Timestamp>,
        receipt: CommandReceipt,
    ) {
        // Admission remains durable for recovery, but shutdown never starts a new effect.
        if *self.inner.shutdown.borrow() {
            return;
        }
        // The intent is already durable. Never retry on failure or uncertainty.
        let id = envelope.command_id;
        let outcome = self
            .execute_effect(&caller, &envelope, deadline, &receipt)
            .await;
        if outcome.is_ok()
            && matches!(
                envelope.body,
                Command::CreateTask { .. } | Command::ResumeTask { .. }
            )
        {
            return;
        }
        // Only the run this command admitted. Another device may have
        // started the task again since, and that run is not this failure's.
        if outcome.is_err()
            && matches!(
                envelope.body,
                Command::CreateTask { .. }
                    | Command::ResumeTask { .. }
                    | Command::QueueStart { .. }
            )
            && let Ok(Some(run_id)) = self.read(|r| r.get::<RunId>("start_run", id))
        {
            let _ = self.finish_run(run_id, RunState::ReconciliationRequired, None);
            let _ = self.revoke_run(run_id).await;
        }
        let (stage, result, failure) = match outcome {
            Ok(Some(CommandResult::Intervention { receipt })) => {
                let uncertain = receipt.facts.iter().any(|fact| {
                    matches!(
                        fact.status,
                        FactStatus::Unknown | FactStatus::StillActive | FactStatus::Pending
                    )
                });
                (
                    if uncertain {
                        CommandStage::ReconciliationRequired
                    } else {
                        CommandStage::Observed
                    },
                    Some(CommandResult::Intervention { receipt }),
                    uncertain.then(|| {
                        error(
                            ErrorCode::OutcomeUnknown,
                            "Stop is not confirmed; check its receipt",
                        )
                    }),
                )
            }
            Ok(result) => (CommandStage::Observed, result, None),
            Err(e) => (
                if e.code == ErrorCode::OutcomeUnknown {
                    CommandStage::ReconciliationRequired
                } else {
                    CommandStage::Failed
                },
                None,
                Some(e),
            ),
        };
        let detail = failure.as_ref().map(|e| e.message.clone());
        let _ = self.write(|tx| {
            if let Some(mut work) = tx.read().command(id)? {
                // Settle accepted words that never reached the adapter.
                if matches!(envelope.body, Command::SendMessage { .. }) {
                    crate::runs::fail_undispatched_followup(tx, id, self.inner.clock.now())?;
                }
                work.pending = false;
                work.stage = stage;
                work.result = result.clone();
                work.error = failure.clone();
                tx.command(&work)?;
                tx.emit(Event::CommandProgress {
                    result,
                    error: failure,
                    command_id: id,
                    stage,
                    detail,
                })?;
            }
            Ok(())
        });
    }
    /// The run a start command admitted, recorded with its admission.
    fn start_run(&self, command_id: CommandId) -> Result<RunId, ProtocolError> {
        self.read(|r| r.get::<RunId>("start_run", command_id))?
            .ok_or_else(|| error(ErrorCode::Conflict, "No admitted run"))
    }
    async fn execute_effect(
        &self,
        caller: &Caller,
        envelope: &CommandEnvelope,
        deadline: Option<protocol::time::Timestamp>,
        receipt: &CommandReceipt,
    ) -> Result<Option<CommandResult>, ProtocolError> {
        use Command::*;
        // The receiver finalizes its durable reservation even when authority or
        // the command deadline was revoked between admission and dispatch.
        if matches!(envelope.body, PrepareContribution { .. })
            && let Some(CommandResult::Contribution { operation }) = &receipt.result
        {
            return self
                .execute_contribution(
                    caller,
                    &operation.operation_id,
                    envelope.command_id,
                    deadline,
                )
                .await
                .map(Some);
        }
        // Once admitted, cleanup must still finish if the caller revokes its
        // own credential. New effects always recheck current authority.
        if !matches!(
            envelope.body,
            StopTask { .. }
                | StopChild { .. }
                | RevokeDevice { .. }
                | SetDeviceGrants { .. }
                | ChangeDeviceGrants { .. }
                | PauseInput { .. }
                | ReleaseLease { .. }
                | WithdrawObservation { .. }
                | RevokeChannelIdentity { .. }
        ) {
            self.authorize(caller, |g| domain::grants::command(g, &envelope.body))?;
        }
        if deadline.is_some_and(|t| t <= self.inner.clock.now()) {
            return Err(error(ErrorCode::Expired, "Command expired before dispatch"));
        }
        match &envelope.body {
            Voice { operation } => {
                return self
                    .voice_command(caller, operation, envelope.command_id)
                    .await;
            }
            WindowAction { control, action } => {
                return self.window_action(caller, control, action).await;
            }
            Account { action } => {
                return self.account_action(caller, action).await;
            }
            Github { action } => {
                return self.github_action(caller, action).await;
            }
            AgentLogin { action } => {
                return self
                    .agent_login_action(caller, envelope.command_id, action)
                    .await;
            }
            SetConversationAccess { task_id, .. } => {
                self.apply_conversation_access(*task_id, envelope.command_id)
                    .await?;
            }
            CreateTask { .. } => {
                if let Some(CommandResult::TaskCreated {
                    task_id,
                    queue_item_id: None,
                }) = receipt.result
                {
                    self.dispatch_task(task_id, self.start_run(envelope.command_id)?)
                        .await?;
                    // A finder may have resumed Next up as it started.
                    if let CreateTask {
                        github: Some(link), ..
                    } = &envelope.body
                        && matches!(**link, protocol::github::GithubLink::Find { .. })
                    {
                        self.progress_queues();
                    }
                } else {
                    self.advance_queues().await?;
                }
            }
            ResumeTask { task_id, .. } => {
                self.dispatch_task(*task_id, self.start_run(envelope.command_id)?)
                    .await?
            }
            SendMessage {
                text,
                bundle,
                delivery,
                ..
            } => {
                let run = self
                    .read(|r| r.get::<RunId>("message_run", envelope.command_id))?
                    .ok_or_else(|| error(ErrorCode::Conflict, "Task has no running turn"))?;
                self.send_message(
                    caller,
                    envelope.command_id,
                    run,
                    text,
                    bundle.as_deref(),
                    *delivery,
                )
                .await?
            }
            StopObservedSession { .. } => {
                return self.stop_observed(envelope.command_id).await.map(Some);
            }
            StopTask { task_id, force } => {
                let run = self.read(|r| r.get::<RunId>("stop_run", envelope.command_id))?;
                self.stop_task(*task_id, run, *force).await?
            }
            StopChild { run_id, child_id } => {
                return self.stop_child(*run_id, *child_id).await.map(Some);
            }
            ResolveApproval {
                approval_id,
                decision,
            } => {
                self.resolve_approval(caller, *approval_id, decision)
                    .await?
            }
            QueueResume { .. } | QueueSetAuthorization { .. } | QueueContinuation { .. } => {
                self.advance_queues().await?
            }
            CancelMessage {
                task_id,
                message_seq,
            } => self.cancel_message(*task_id, *message_seq, false).await?,
            RevokeChannelIdentity { identity_id } => self.withdraw_revoked(*identity_id).await?,
            QueueStart { item_id, .. } => {
                let task_id = self.read(|r| {
                    r.list::<QueueView>("queue")?
                        .into_iter()
                        .flat_map(|q| q.items)
                        .find(|i| i.item_id == *item_id)
                        .map(|i| i.task_id)
                        .ok_or_else(|| error(ErrorCode::NotFound, "Queue item not found").into())
                })?;
                self.dispatch_task(task_id, self.start_run(envelope.command_id)?)
                    .await?;
            }
            RevokeDevice { device_id } => self.end_device_access(*device_id).await?,
            SetDeviceGrants { device_id, .. } | ChangeDeviceGrants { device_id, .. } => {
                self.revoke_device_leases(*device_id).await?
            }
            DeleteOneOff { .. } => self.delete_one_off_folder(envelope.command_id).await?,
            CancelTransfer { transfer_id } => self.cancel_upload(caller, *transfer_id).await?,
            DeleteArtifact { .. } => self.collect_blobs().await?,
            SetChannelProject {
                identity_id,
                project_id,
            } => {
                let c: ChannelIdentity = self.read(|r| r.require("channel", identity_id))?;
                let settings = self.chat_project_settings()?;
                // The companion keeps its settings locked while it shuts down,
                // and shutdown waits for this command, so neither may wait on the other.
                let mut shutdown = self.inner.shutdown.subscribe();
                tokio::select! {
                    result = settings.set(c.channel, &c.external_chat, *project_id) => result?,
                    _ = shutdown.changed() => return Err(error(ErrorCode::Expired, "Øphio is shutting down on the computer")),
                }
            }
            CheckPreview { preview_id, start } => {
                let p: PreviewEndpoint = self.read(|r| r.require("preview", preview_id))?;
                let checker = self
                    .inner
                    .preview
                    .as_ref()
                    .ok_or_else(|| unsupported("Preview checker"))?;
                let url = crate::preview::validate(&p.url)?;
                // Shown from a sandboxed turn that's still going, its server
                // answers on that sandbox's loopback.
                let sandbox = self.preview_sandbox(*preview_id);
                let addresses = crate::preview_forward::loopback_sockets(&p.url).ok();
                let sandboxed = match (sandbox, addresses) {
                    (Some(sandbox), Some(addresses)) => {
                        crate::preview_forward::in_sandbox(&*sandbox, &addresses)
                            .await
                            .is_some()
                    }
                    _ => false,
                };
                let mut reachable = sandboxed
                    || tokio::time::timeout(std::time::Duration::from_secs(5), checker.check(&url))
                        .await
                        .map_err(|_| {
                            error(ErrorCode::TemporarilyUnavailable, "Preview check timed out")
                        })??;
                let mut failed = None;
                if !reachable && *start {
                    match (self.start_preview_server(
                        caller,
                        &envelope.body,
                        &p,
                        &url,
                        checker.as_ref(),
                    ))
                    .await
                    {
                        Ok(()) => reachable = true,
                        Err(e) => failed = Some(e),
                    }
                }
                self.write(|tx| {
                    if let Some(mut current) =
                        tx.read().get::<PreviewEndpoint>("preview", preview_id)?
                    {
                        let now = self.inner.clock.now();
                        current.reachable = Some(reachable);
                        current.checked_at = Some(now);
                        tx.put("preview", preview_id, &current)?;
                        emit_previews(tx, current.project_id)?;
                        // Opened, a page an agent showed stays a week longer.
                        crate::expiry::page_seen(tx, *preview_id, false, now)?;
                    }
                    Ok(())
                })?;
                if let Some(e) = failed {
                    return Err(e);
                }
            }
            RequestHumanLease { .. }
            | ReleaseLease { .. }
            | PauseInput { .. }
            | TakeOver { .. }
            | ReturnToAgent { .. }
            | StartPrivateStep { .. }
            | EndPrivateStep { .. }
            | WithdrawObservation { .. }
            | LockDesktop
            | UnlockDesktop { .. }
            | CaptureTarget { .. } => return self.control_command(caller, &envelope.body).await,
            LibrarySave { .. }
            | LibraryCreate { .. }
            | LibraryRename { .. }
            | LibraryDelete { .. }
            | MakeReusable { .. }
            | LibraryRestore { .. }
            | CreateReusableDraft { .. }
            | LibraryRecoveryDiscard { .. }
            | CreateAgent { .. }
            | CopySetupSkill { .. }
            | ChooseSetupSkillVersion { .. }
            | OpenTerminal { .. }
            | CloseTerminal { .. }
            | RunReadinessCheck => return self.plumbing_command(caller, &envelope.body).await,
            _ => {}
        }
        Ok(None)
    }
}

/// A one-off's folder that is deleted once the one-off is forgotten.
const DELETE_FOLDER: &str = "delete_folder";
/// The state an archived task was archived from, for Undo.
const ARCHIVED_FROM: &str = "task_archived_from";

impl CompanionHub {
    /// Forget a project once nothing of its tasks is still running, so that
    /// the person can `what`.
    fn forget_project(
        tx: &mut store::Write<'_>,
        project_id: ProjectId,
        what: &str,
    ) -> store::Result<()> {
        let p: Project = tx.read().require("project", project_id)?;
        if p.active_writer.is_some() {
            return Err(error(
                ErrorCode::Conflict,
                format!("Reconcile the active writer, then {what}"),
            )
            .into());
        }
        for task in tx
            .read()
            .list::<Task>("task")?
            .into_iter()
            .filter(|t| t.project_id == project_id)
        {
            let Some(run) = (task.current_run)
                .map(|id| tx.read().get::<Run>("run", id))
                .transpose()?
                .flatten()
            else {
                continue;
            };
            if !run.state.is_terminal() {
                return Err(error(
                    ErrorCode::Conflict,
                    format!("Finish or reconcile active runs, then {what}"),
                )
                .into());
            }
            // Stop reaches what a finished turn left through the project's
            // Next up, so the project stays until it ends.
            if crate::runs::still_finishing(&run) {
                return Err(error(
                    ErrorCode::Conflict,
                    format!("Something a task in this project started hasn’t ended yet. Stop it, or wait for it to end, then {what}."),
                )
                .into());
            }
        }
        tx.delete("project", project_id)?;
        tx.delete("queue", project_id)?;
        tx.delete("project_settings", project_id)?;
        crate::checklist::put_away(tx, project_id)?;
        Self::forget_github(tx, project_id)?;
        tx.emit(Event::ProjectRemoved { project_id })?;
        Ok(())
    }
    /// Delete a forgotten one-off's folder, and everything in it. Links in it
    /// are deleted, never followed.
    async fn delete_one_off_folder(&self, command_id: CommandId) -> Result<(), ProtocolError> {
        let Some(folder) = self.read(|r| r.get::<PathBuf>(DELETE_FOLDER, command_id))? else {
            return Ok(());
        };
        let shown = folder.display().to_string();
        let deleted = tokio::task::spawn_blocking(move || match std::fs::remove_dir_all(&folder) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        })
        .await
        .map_err(|_| crate::internal("Deleting the one-off’s folder stopped"))?;
        self.write(|tx| tx.delete(DELETE_FOLDER, command_id))?;
        deleted.map_err(|e| {
            error(
                ErrorCode::TemporarilyUnavailable,
                format!("The one-off is gone from Øphio, but its folder {shown} couldn’t be deleted: {e}. Delete what is left of it on the computer."),
            )
        })
    }
}

fn retained_envelope(
    r: &store::Read<'_>,
    envelope: &CommandEnvelope,
) -> store::Result<CommandEnvelope> {
    let approval = match &envelope.body {
        Command::ResolveApproval { approval_id, .. } => {
            r.get::<Approval>("approval", approval_id)?
        }
        _ => None,
    };
    let mut kept = envelope.without_secrets_for(approval.as_ref());
    // Content belongs only to the removable report documents. Dedupe keeps
    // an opaque digest, so withdrawal leaves no second copy in commands.
    match &mut kept.body {
        Command::SubmitReport { bundle } => {
            use sha2::Digest;
            let digest = format!("{:x}", sha2::Sha256::digest(serde_json::to_vec(bundle)?));
            let id = bundle.report_id;
            **bundle = protocol::reports::ReportBundle {
                schema: protocol::reports::REPORT_SCHEMA,
                report_id: id,
                created_at: protocol::time::Timestamp(0),
                origin: protocol::reports::ReportOrigin::Settings,
                words: None,
                signature: protocol::reports::ReportSignature {
                    code: digest,
                    screen: String::new(),
                    build: String::new(),
                },
                versions: Default::default(),
                context: Default::default(),
                phone_trail: Vec::new(),
                computer_trail: Vec::new(),
                state: None,
                crashes: Vec::new(),
                screenshot: None,
                extras: Vec::new(),
                test: None,
            };
        }
        Command::AnswerReportQuestion { answer, .. } => {
            if let Some(words) = &mut answer.words {
                use sha2::Digest;
                *words = format!("{:x}", sha2::Sha256::digest(words.as_bytes()));
            }
        }
        _ => {}
    }
    Ok(kept)
}

fn dedupe(
    r: &store::Read<'_>,
    existing: store::StoredCommand,
    caller: &Caller,
    envelope: &CommandEnvelope,
) -> Result<CommandReceipt, ProtocolError> {
    // What is kept has its secrets withheld, so compare like with like.
    if existing.principal != caller.principal
        || !existing
            .envelope
            .matches_request(&retained_envelope(r, envelope).map_err(crate::internal)?)
    {
        return Err(error(
            ErrorCode::Conflict,
            "Command id is already bound to another request",
        ));
    }
    let mut receipt = existing.receipt;
    receipt.status = ReceiptStatus::Duplicate;
    Ok(receipt)
}

/// The model and effort a resume asks for: a new model with its effort, or
/// that model's own default effort; an effort alone keeps the task's model.
fn switched(
    task: &Task,
    model: &Option<String>,
    effort: &Option<String>,
) -> (Option<String>, Option<String>) {
    match model {
        Some(model) => (Some(model.clone()), effort.clone()),
        None => (task.model.clone(), effort.clone().or(task.effort.clone())),
    }
}

/// Whether `task` can take a read-only turn: one in its own session that only
/// reads and answers, as when the person asks what happened. Its last run
/// must have ended with a session to continue. Desktop tasks run only in
/// Work, and every run of a task linked to GitHub decides or posts there.
fn read_only_turn(r: &store::Read<'_>, task: &Task, changes_mode: bool) -> store::Result<()> {
    if changes_mode {
        return Err(error(
            ErrorCode::InvalidRequest,
            "A read-only turn keeps the task's mode and carries no plan",
        )
        .into());
    }
    if task.kind == TaskKind::ComputerUse || task.github.is_some() {
        return Err(unsupported("A read-only turn for this kind of task").into());
    }
    if !matches!(
        task.state,
        TaskState::Interrupted | TaskState::Completed | TaskState::Failed
    ) {
        return Err(error(
            ErrorCode::Conflict,
            "The agent can be asked once this task has stopped or finished",
        )
        .into());
    }
    let session = task
        .current_run
        .map(|id| r.get::<Run>("run", id))
        .transpose()?
        .flatten()
        .and_then(|run| run.native_session);
    if session.is_none() {
        return Err(error(
            ErrorCode::Conflict,
            "The agent's session for this task wasn't kept, so it can't be asked about it",
        )
        .into());
    }
    Ok(())
}
/// Whether `text` ends with `plan` between the markers the phone quotes a
/// carried plan with, so the agent reads exactly the plan that is recorded.
fn quotes_plan(text: &str, plan: &str) -> bool {
    let Some(head) = text
        .strip_suffix("\n--- End of plan ---")
        .and_then(|t| t.strip_suffix(plan))
        .and_then(|t| t.strip_suffix('\n'))
    else {
        return false;
    };
    !plan.trim().is_empty()
        && head
            .rsplit_once("\n\n")
            .and_then(|(_, marker)| marker.strip_prefix("--- Plan from run "))
            .and_then(|marker| marker.strip_suffix(", proposed by the agent ---"))
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}
/// Structured answers name each asked question exactly once. A question that
/// is not multi-select takes one choice: an option or the person's own words.
fn answers_fit(questions: &[ApprovalQuestion], answers: &[QuestionAnswer]) -> bool {
    answers.len() == questions.len()
        && questions.iter().all(|question| {
            let mut given = answers.iter().filter(|a| {
                a.question == question.question
                    && match &a.id {
                        Some(_) => a.id == question.id,
                        None => {
                            questions
                                .iter()
                                .filter(|q| q.question == question.question)
                                .count()
                                == 1
                        }
                    }
            });
            let (Some(answer), None) = (given.next(), given.next()) else {
                return false;
            };
            let distinct: std::collections::HashSet<_> = answer.choices.iter().collect();
            !answer.choices.is_empty()
                && distinct.len() == answer.choices.len()
                && answer
                    .choices
                    .iter()
                    .all(|choice| !choice.trim().is_empty())
                && (question.multi_select || answer.choices.len() == 1)
        })
}
/// Register a project with its Next up, which starts paused.
fn add_project(
    tx: &mut store::Write<'_>,
    project: Project,
    now: protocol::time::Timestamp,
) -> store::Result<()> {
    let project_id = project.project_id;
    tx.put("project", project_id, &project)?;
    tx.emit(Event::ProjectUpserted { project })?;
    let queue = QueueView {
        project_id,
        revision: 1,
        paused: true,
        pause_reason: Some("Next up starts paused. Resume it to let tasks start.".into()),
        writer: None,
        items: Vec::new(),
    };
    tx.put("queue", project_id, &queue)?;
    crate::runs::sync_queue_attention(tx, &queue, now)?;
    tx.emit(Event::QueueChanged { queue })?;
    Ok(())
}

/// Every computer has the person's home folder as Home, a place to work
/// apart from their projects. Made when Øphio first starts with one, and
/// kept at the home folder if that moves.
pub(crate) fn settle_home(
    tx: &mut store::Write<'_>,
    home: Option<&std::path::Path>,
    now: protocol::time::Timestamp,
) -> store::Result<()> {
    let Some(path) = home.and_then(|h| h.to_str()) else {
        return Ok(());
    };
    match tx
        .read()
        .list::<Project>("project")?
        .into_iter()
        .find(|p| p.home)
    {
        Some(p) if p.host_path == path => Ok(()),
        Some(mut project) => {
            project.host_path = path.into();
            tx.put("project", project.project_id, &project)?;
            tx.emit(Event::ProjectUpserted { project })?;
            Ok(())
        }
        None => {
            let project = Project {
                project_id: ProjectId::new(),
                name: "Home".into(),
                host_path: path.into(),
                default_harness: None,
                default_model: None,
                default_effort: None,
                active_writer: None,
                vcs: None,
                stage: None,
                created_at: now,
                one_off: false,
                home: true,
            };
            add_project(tx, project, now)
        }
    }
}

/// Refuses what would send a file of Home's to or from the phone: the home
/// folder holds the person's keys and Øphio's own files.
pub(crate) fn not_home(project: &Project) -> Result<(), ProtocolError> {
    if project.home {
        return Err(error(
            ErrorCode::PathNotAllowed,
            "Files in your home folder aren’t sent to or from the phone. Choose a project’s folder, or Library’s.",
        ));
    }
    Ok(())
}

pub(crate) fn valid_label(s: &str) -> Result<(), ProtocolError> {
    if s.trim().is_empty() || s.len() > 512 || s.contains('\0') {
        Err(error(
            ErrorCode::InvalidRequest,
            "Label is empty or too long",
        ))
    } else {
        Ok(())
    }
}
pub(crate) fn emit_previews(tx: &mut store::Write<'_>, project_id: ProjectId) -> store::Result<()> {
    let previews = tx
        .read()
        .list::<PreviewEndpoint>("preview")?
        .into_iter()
        .filter(|p| p.project_id == project_id)
        .collect();
    tx.emit(Event::PreviewsChanged {
        project_id,
        previews,
    })?;
    Ok(())
}

impl CompanionHub {
    /// Project downloads and uploads reach everything under a project's
    /// folder. The whole disk, the home folder and Ophio's own folders are
    /// not a project.
    pub(crate) fn project_folder(&self, path: &std::path::Path) -> Result<(), ProtocolError> {
        let refuse = |message: &str| Err(error(ErrorCode::PathNotAllowed, message));
        if path.parent().is_none() {
            return refuse(
                "That’s the top of the computer’s disk. Choose the project’s own folder.",
            );
        }
        if self.inner.home.as_deref() == Some(path) {
            return refuse("That’s the home folder. Choose the project’s own folder inside it.");
        }
        if self
            .inner
            .private
            .iter()
            .any(|own| own.starts_with(path) || path.starts_with(own))
        {
            return refuse(
                "Øphio keeps its own files in that folder. Choose the project’s own folder.",
            );
        }
        if self
            .inner
            .home
            .as_deref()
            .is_some_and(|home| home.starts_with(path))
        {
            return refuse("That folder holds the home folder. Choose the project’s own folder.");
        }
        Ok(())
    }
}

async fn vcs_info(path: &std::path::Path) -> Option<VcsInfo> {
    use crate::git::output as git;
    let head = git(path, &["rev-parse", "HEAD"]).await?;
    let branch = git(path, &["symbolic-ref", "--quiet", "--short", "HEAD"]).await;
    let changed = git(path, &crate::git::CHANGED).await?;
    Some(VcsInfo {
        kind: "git".into(),
        branch,
        head: Some(head),
        dirty_files: changed.lines().count() as u32,
    })
}

/// Checked under the same write transaction as the grant/revocation mutation.
fn protect_last_manager(
    tx: &store::Write<'_>,
    caller: &Caller,
    device: &crate::identity::Device,
    remains_manager: bool,
) -> store::Result<()> {
    if caller.principal == Principal::LocalOwner
        || remains_manager
        || device.record.revoked_at.is_some()
        || !device.record.grants.admin
    {
        return Ok(());
    }
    let others: Vec<_> = tx
        .read()
        .list::<crate::identity::Device>("device")?
        .into_iter()
        .filter(|d| d.record.device_id != device.record.device_id && d.record.revoked_at.is_none())
        .collect();
    if !others.is_empty() && !others.iter().any(|d| d.record.grants.admin) {
        return Err(error(
            ErrorCode::LastManager,
            "Give another device Manage access first",
        )
        .into());
    }
    Ok(())
}

/// Counts the bytes written to it and keeps none.
struct Counted(usize);
impl std::io::Write for Counted {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0 += bytes.len();
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// The review whose findings the verifier created under `command_id` is to
/// check, while they wait for it.
fn verified_review(r: &store::Read<'_>, command_id: CommandId) -> store::Result<Option<TaskId>> {
    let waiting = r.list::<crate::review_loop::Verifying>(crate::review_loop::VERIFYING)?;
    Ok((waiting.into_iter())
        .find(|v| v.command_id == command_id)
        .map(|v| v.review))
}
