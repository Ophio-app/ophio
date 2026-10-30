// SPDX-License-Identifier: Apache-2.0
//! Run-bound computer-use and permission broker. The local transport passes only
//! bindings obtained from `bind`; all effects revalidate the persisted run.
use crate::{
    CompanionHub,
    control::platform,
    error,
    identity::{hash, random_secret},
    unsupported,
};
use async_trait::async_trait;
use base64::{Engine, engine::general_purpose::STANDARD};
use ports::bridge::*;
use ports::tool_approval::{ToolApproval, permission_reply, raw_request, tool_approval};
use protocol::{auth::Principal, command::ApprovalDecision, control::*, model::*, *};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use subtle::ConstantTimeEq;

/// A capture of the screen an agent acts on, as it is sent to it.
pub(crate) struct Seen {
    pub image_base64: String,
    pub media_type: String,
    pub width: u32,
    pub height: u32,
    pub geometry_revision: u64,
}

#[derive(Debug, Clone)]
pub struct BridgeConfig {
    pub executable: PathBuf,
    pub socket: String,
    /// Additional capture routes outside the run being paused. The native
    /// session itself is covered only by RunControl::pause_for_private.
    pub uncovered_routes: Vec<String>,
}
impl BridgeConfig {
    pub fn new(executable: impl Into<PathBuf>, socket: impl Into<String>) -> Self {
        Self {
            executable: executable.into(),
            socket: socket.into(),
            uncovered_routes: Vec::new(),
        }
    }
}
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct TokenRecord {
    pub run_id: RunId,
    hash: [u8; 32],
    epoch: BootEpoch,
}

pub async fn spawn_config(
    hub: &CompanionHub,
    run: RunId,
) -> Result<ports::ToolBridge, ProtocolError> {
    hub.spawn_config(run).await
}
impl CompanionHub {
    pub async fn spawn_config(&self, run_id: RunId) -> Result<ports::ToolBridge, ProtocolError> {
        let config = self
            .inner
            .bridge
            .as_ref()
            .ok_or_else(|| unsupported("Local tool bridge"))?;
        let token = random_secret()?;
        let (task, github, answers) = self.write(|tx| {
            let run: Run = tx.read().require("run", run_id)?;
            let task: Task = tx.read().require("task", run.task_id)?;
            let github = self.githubs(&tx.read(), &task)?;
            let answers = crate::checklist::answers(&tx.read(), task.task_id)?;
            if run.state != RunState::Dispatching {
                return Err(error(
                    ErrorCode::Conflict,
                    "Bridge credentials are issued only during dispatch",
                )
                .into());
            }
            if tx.read().get::<TokenRecord>("run_token", run_id)?.is_some() {
                return Err(error(
                    ErrorCode::Conflict,
                    "Bridge credentials were already issued",
                )
                .into());
            }
            tx.put(
                "run_token",
                run_id,
                &TokenRecord {
                    run_id,
                    hash: hash(&token),
                    epoch: self.inner.epoch,
                },
            )?;
            Ok((task, github, answers))
        })?;
        Ok(ports::ToolBridge {
            server_name: "computer".into(),
            command: config.executable.clone(),
            args: Vec::new(),
            env: vec![
                (RUN_TOKEN_ENV.into(), token),
                (SOCKET_ENV.into(), config.socket.clone()),
            ],
            permission_tool: Some("permission_prompt".into()),
            computer_use: task.kind == TaskKind::ComputerUse && !answers,
            browser: self.browses(&task) && !answers,
            github: github && !answers,
            checklist_answers: answers,
        })
    }
    async fn broker_request(
        &self,
        binding: &BridgeBinding,
        request: BridgeRequest,
    ) -> Result<BridgeResponse, ProtocolError> {
        let task = self.read(|r| {
            let token: TokenRecord = r.require("run_token", binding.run_id)?;
            if token.epoch != self.inner.epoch {
                return Err(error(ErrorCode::Expired, "Bridge belongs to an earlier boot").into());
            }
            let run: Run = r.require("run", binding.run_id)?;
            let task: Task = r.require("task", run.task_id)?;
            let answers = crate::checklist::answers(r, task.task_id)?;
            if task.target_id != binding.target_id
                || (task.kind == TaskKind::ComputerUse && !answers) != binding.computer_use
                || (binding.browser && !self.browses(&task))
                || answers != binding.checklist_answers
            {
                return Err(error(ErrorCode::PermissionDenied, "Bridge binding mismatch").into());
            }
            Ok(task)
        })?;
        if self.run_stopped(binding.run_id)? {
            return Err(error(ErrorCode::Expired, "Run is no longer active"));
        }
        // A turn answering on the checklist reads it and answers, and does
        // nothing else.
        if binding.checklist_answers
            && !matches!(
                request,
                BridgeRequest::Person {
                    action: PersonCall::ChecklistRead | PersonCall::ChecklistReply { .. }
                }
            )
        {
            return Err(error(
                ErrorCode::PermissionDenied,
                "This turn only reads the checklist and answers the person on it",
            ));
        }
        let annotation = match &request {
            BridgeRequest::ObserveEvidence { annotation, .. } => Some(annotation.clone()),
            _ => None,
        };
        match request {
            BridgeRequest::Hello { .. } => {
                Err(error(ErrorCode::InvalidRequest, "Bridge is already bound"))
            }
            BridgeRequest::PermissionPrompt {
                tool_name,
                input,
                tool_use_id,
                native_request_id,
            } => {
                // The MCP request id is always present; tool_use_id is not.
                // What is shown is drawn from the input with keys hidden; the
                // tool is answered from its own.
                let shown = self.masked_input(binding.run_id, &input);
                if tool_name == "AskUserQuestion"
                    && !binding.computer_use
                    && !self.questions_on()?
                {
                    let asked = tool_approval(&tool_name, &shown);
                    self.held_question(task.task_id, binding.run_id, &asked.title)?;
                    let hold = ApprovalDecision::Deny {
                        reason: Some(ports::harness::HOLD_QUESTIONS.into()),
                    };
                    return Ok(BridgeResponse::Permission {
                        decision: permission_reply(&tool_name, input, hold),
                    });
                }
                let decision = self
                    .bridge_wait(
                        binding.run_id,
                        &task,
                        tool_approval(&tool_name, &shown),
                        Some(raw_request(&shown)),
                        native_request_id.or(tool_use_id.clone()),
                        tool_use_id,
                    )
                    .await?;
                Ok(BridgeResponse::Permission {
                    decision: permission_reply(&tool_name, input, decision),
                })
            }
            BridgeRequest::RequestHuman { reason } => {
                let answer = self
                    .bridge_wait(
                        binding.run_id,
                        &task,
                        ToolApproval {
                            kind: ApprovalKind::Question,
                            title: reason,
                            detail: None,
                            detail_lines: Vec::new(),
                            questions: Vec::new(),
                            scope: "This request once".into(),
                            shortened: false,
                        },
                        None,
                        None,
                        None,
                    )
                    .await?;
                Ok(BridgeResponse::HumanResult {
                    text: match answer {
                        ApprovalDecision::Answer { text, .. } => text,
                        ApprovalDecision::Allow => "The person acknowledged the request".into(),
                        ApprovalDecision::Deny { reason } => {
                            reason.unwrap_or_else(|| "The person declined".into())
                        }
                    },
                })
            }
            BridgeRequest::ObserveTarget { max_dim }
            | BridgeRequest::ObserveEvidence { max_dim, .. } => {
                if !binding.computer_use {
                    return Err(error(
                        ErrorCode::PermissionDenied,
                        "This run has no computer-use grant",
                    ));
                }
                let id = binding
                    .target_id
                    .ok_or_else(|| error(ErrorCode::NotFound, "Target not bound"))?;
                let target = self.target(id).await?;
                let mut target = target.lock().await;
                let seen = self
                    .observe_locked(binding.run_id, &task, &mut target, max_dim, annotation)
                    .await?;
                Ok(BridgeResponse::Observation {
                    image_base64: seen.image_base64,
                    media_type: seen.media_type,
                    width: seen.width,
                    height: seen.height,
                    geometry_revision: seen.geometry_revision,
                })
            }
            BridgeRequest::Browser { action } => {
                if !binding.browser {
                    return Err(error(
                        ErrorCode::PermissionDenied,
                        "The agent browser is not enabled for this task",
                    ));
                }
                // The browser runs on the computer, outside the sandbox, so
                // it reaches what the computer does.
                if crate::access::held(&task).is_some_and(|a| !(a.internet && a.local_network)) {
                    return Err(error(
                        ErrorCode::PermissionDenied,
                        "Your browser runs on the computer itself, outside this chat's sandbox, so it opens pages only while the person has given this chat both Internet and Local network. Ask them to turn those on in the chat's access, or fetch the page with curl, which stays inside it.",
                    ));
                }
                self.browser_request(binding.run_id, &task, action).await
            }
            BridgeRequest::Github { action } => {
                if !binding.github {
                    return Err(error(
                        ErrorCode::PermissionDenied,
                        "This task is not linked to GitHub",
                    ));
                }
                Ok(BridgeResponse::GithubResult {
                    text: self.github_request(binding.run_id, &task, action).await?,
                })
            }
            BridgeRequest::Person { action } => {
                // The specialist acts on the computer for another task; it
                // doesn't talk to the person.
                if binding.computer_use {
                    return Err(error(
                        ErrorCode::PermissionDenied,
                        "The computer specialist doesn't show the person things",
                    ));
                }
                Ok(BridgeResponse::PersonResult {
                    text: self.person_request(binding.run_id, &task, action).await?,
                })
            }
            BridgeRequest::Work { action } => {
                // Computer specialists cannot plan or ask for help. Other
                // runs reach the work handler, which lets GitHub tasks list
                // agents and ask read-only helpers but rejects their planning.
                if binding.computer_use {
                    return Err(error(
                        ErrorCode::PermissionDenied,
                        "This run doesn't plan the person's work",
                    ));
                }
                Ok(BridgeResponse::WorkResult {
                    text: self.work_request(binding.run_id, &task, action).await?,
                })
            }
            request => {
                if !binding.computer_use {
                    return Err(error(
                        ErrorCode::PermissionDenied,
                        "This run has no computer-use grant",
                    ));
                }
                let id = binding
                    .target_id
                    .ok_or_else(|| error(ErrorCode::NotFound, "Target not bound"))?;
                let target = self.target(id).await?;
                let (mut target, resumed) =
                    self.agent_input(binding.run_id, &task, &target).await?;
                if resumed {
                    return self.resumed(binding.run_id, &task, &mut target).await;
                }
                if target.observed_generation != Some(target.generation) {
                    return Err(error(
                        ErrorCode::StaleRevision,
                        "Not sent: you haven't looked at the screen since you were given it. Call observe_target, then act on what it shows.",
                    ));
                }
                let current = self
                    .desktop()?
                    .targets()
                    .await
                    .map_err(platform)?
                    .into_iter()
                    .find(|t| t.target_id == id)
                    .ok_or_else(|| error(ErrorCode::NotFound, "Target disappeared"))?;
                if current.geometry != target.geometry {
                    self.revoke_locked(&mut target, false).await?;
                    return Err(error(ErrorCode::StaleRevision, "Target geometry changed"));
                }
                let (action, revision) = match request {
                    BridgeRequest::Pointer {
                        action,
                        at,
                        observation_revision,
                    } => (
                        match action {
                            BridgePointer::Move => InputAction::PointerMove { to: at },
                            BridgePointer::Click { button, count } => {
                                InputAction::Click { button, at, count }
                            }
                            BridgePointer::Down { button } => InputAction::PointerButton {
                                button,
                                pressed: true,
                                at,
                            },
                            BridgePointer::Up { button } => InputAction::PointerButton {
                                button,
                                pressed: false,
                                at,
                            },
                        },
                        Some(observation_revision),
                    ),
                    BridgeRequest::Key { keys } => (InputAction::KeyChord { keys }, None),
                    BridgeRequest::TypeText { text } => (InputAction::Text { text }, None),
                    BridgeRequest::Scroll {
                        at,
                        dx,
                        dy,
                        observation_revision,
                    } => (
                        InputAction::Scroll { at, dx, dy },
                        Some(observation_revision),
                    ),
                    _ => return Err(error(ErrorCode::InvalidRequest, "Invalid bridge request")),
                };
                if revision.is_some_and(|r| r != target.geometry.revision) {
                    return Err(error(
                        ErrorCode::StaleRevision,
                        "Observation geometry is stale",
                    ));
                }
                if !domain::lease::valid_action(&action) {
                    return Err(error(ErrorCode::InvalidRequest, "Invalid input action"));
                }
                if self.run_stopped(binding.run_id)? {
                    return Err(error(
                        ErrorCode::Expired,
                        "Run stopped before input admission",
                    ));
                }
                let geometry = target.geometry;
                let delivered = tokio::time::timeout(
                    std::time::Duration::from_millis(500),
                    target.sink.inject(&action, &geometry),
                )
                .await;
                if matches!(delivered, Ok(Err(ports::desktop::PlatformError::Locked))) {
                    self.apply_desktop_lock(
                        &mut target,
                        protocol::control::DesktopLockState::Locked,
                    )
                    .await?;
                    return Err(error(
                        ErrorCode::TemporarilyUnavailable,
                        "Computer is locked",
                    ));
                }
                if let Ok(Err(ports::desktop::PlatformError::TextUnavailable {
                    characters, ..
                })) = &delivered
                {
                    return Err(error(
                        ErrorCode::Unsupported,
                        format!(
                            "Nothing was typed. The computer’s keyboard cannot type: {characters}"
                        ),
                    ));
                }
                if !matches!(delivered, Ok(Ok(()))) {
                    self.revoke_locked(&mut target, false).await?;
                    return Err(error(
                        ErrorCode::OutcomeUnknown,
                        "Input delivery was not acknowledged",
                    ));
                }
                Ok(BridgeResponse::Done)
            }
        }
    }
    /// Captures `target` for `run_id`, while it may still look at it, and
    /// counts the capture as what its next action is placed on.
    pub(crate) async fn observe_locked(
        &self,
        run_id: RunId,
        task: &Task,
        target: &mut crate::control::Target,
        max_dim: Option<u32>,
        annotation: Option<ArtifactAnnotation>,
    ) -> Result<Seen, ProtocolError> {
        let id = target.view.target_id;
        if target.view.private_step.is_some()
            || !target
                .view
                .model_observation
                .as_ref()
                .is_some_and(|o| o.run_id == run_id && o.state == ObservationState::Allowed)
        {
            return Err(error(
                ErrorCode::PermissionDenied,
                "Model observation withdrawn",
            ));
        }
        let frame = self.desktop()?.capture_still(id).await.map_err(platform)?;
        let encoded = self
            .inner
            .media
            .as_ref()
            .ok_or_else(|| unsupported("Media encoder"))?
            .encode_still(
                &frame,
                ports::StillFormat::Png,
                Some(max_dim.unwrap_or(1600).clamp(64, 4096)),
            )
            .map_err(platform)?;
        if self.run_stopped(run_id)? {
            return Err(error(
                ErrorCode::Expired,
                "Run stopped while capture was in progress",
            ));
        }
        self.put_linked_artifact(
            &encoded.bytes,
            encoded.media_type,
            Some(protocol::model::ArtifactLink {
                task_id: task.task_id,
                run_id: Some(run_id),
                role: if annotation.is_some() {
                    protocol::model::ArtifactRole::After
                } else {
                    protocol::model::ArtifactRole::Observation
                },
                annotation,
            }),
            Principal::System,
            None,
        )
        .await?;
        if let Some(o) = &mut target.view.model_observation {
            o.delivered_count = o.delivered_count.saturating_add(1);
        }
        target.observed_generation =
            (frame.geometry == target.geometry).then_some(target.generation);
        self.save_target(target)?;
        Ok(Seen {
            image_base64: STANDARD.encode(encoded.bytes),
            media_type: encoded.media_type.into(),
            width: encoded.width,
            height: encoded.height,
            geometry_revision: frame.geometry.revision,
        })
    }
    async fn bridge_wait(
        &self,
        run_id: RunId,
        task: &Task,
        facts: ToolApproval,
        raw: Option<String>,
        native_id: Option<String>,
        tool_call: Option<String>,
    ) -> Result<ApprovalDecision, ProtocolError> {
        self.bridge_wait_as(
            ApprovalId::new(),
            run_id,
            task,
            facts,
            raw,
            native_id,
            tool_call,
        )
        .await
    }
    /// [`Self::bridge_wait`] under an id the asker chose, so it can tell the
    /// approval apart when the owner answers.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn bridge_wait_as(
        &self,
        id: ApprovalId,
        run_id: RunId,
        task: &Task,
        facts: ToolApproval,
        raw: Option<String>,
        native_id: Option<String>,
        tool_call: Option<String>,
    ) -> Result<ApprovalDecision, ProtocolError> {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let now = self.inner.clock.now();
        // One that may be answered after its agent has moved on keeps no
        // deadline; it goes back to the agent unanswered before this wait
        // would end.
        let carries_on = crate::carry_on::carries_on(facts.kind);
        let wait_for = std::time::Duration::from_secs(if carries_on { 900 } else { 600 });
        let mut approval = Approval {
            approval_id: id,
            run_id,
            task_id: task.task_id,
            project_id: task.project_id,
            native_request_id: native_id.unwrap_or_else(|| format!("bridge:{id}")),
            kind: facts.kind,
            title: facts.title,
            detail: facts.detail,
            detail_lines: facts.detail_lines,
            questions: facts.questions,
            raw_request: raw,
            scope: facts.scope,
            shortened: facts.shortened,
            revision: 1,
            state: ApprovalState::Pending,
            delivery: None,
            outcome: None,
            requested_at: now,
            expires_at: (!carries_on)
                .then(|| protocol::time::Timestamp(now.0.saturating_add(600_000))),
            resolved_by: None,
            resolved_at: None,
        };
        // The tool is answered from its own request; what is shown and kept
        // has the run's keys and tokens hidden.
        self.mask_approval(run_id, &mut approval);
        // Measured as recorded: the phone decides only on a request it can
        // show whole, within one control frame.
        if approval.title.len() > 8192 || !crate::native_text::approval_fits(&approval) {
            return Err(error(
                ErrorCode::LimitExceeded,
                "Approval request is too large",
            ));
        }
        {
            let mut waits = self.inner.waits.lock().await;
            if waits.len() >= 256 {
                return Err(error(
                    ErrorCode::LimitExceeded,
                    "Too many pending human decisions",
                ));
            }
            waits.insert(id, sender);
        }
        if let Err(e) =
            self.write(|tx| {
                let run: Run = tx.read().require("run", run_id)?;
                if !matches!(
                    run.state,
                    RunState::Dispatching
                        | RunState::Working
                        | RunState::AwaitingApproval
                        | RunState::AwaitingInput
                ) || (tx.read().get::<TokenRecord>("run_token", run_id)?.is_none()
                    && tx
                        .read()
                        .get::<ApprovalId>(crate::github_access::START_APPROVAL, run_id)?
                        != Some(id))
                {
                    return Err(
                        error(ErrorCode::Expired, "Run stopped before approval admission").into(),
                    );
                }
                if tx.read().list::<Approval>("approval")?.iter().any(|a| {
                    a.run_id == run_id && a.native_request_id == approval.native_request_id
                }) {
                    return Err(error(
                        ErrorCode::AlreadyResolved,
                        "This native request already has an approval record",
                    )
                    .into());
                }
                tx.put("bridge_approval", id, &true)?;
                self.put_approval(tx, &approval)?;
                if let Some(call) = &tool_call {
                    tx.put(crate::runs::APPROVAL_TOOL_CALL, id, call)?;
                }
                let mut run: Run = tx.read().require("run", run_id)?;
                let mut task: Task = tx.read().require("task", task.task_id)?;
                run.state = RunState::AwaitingApproval;
                task.state = TaskState::AwaitingDecision;
                task.revision += 1;
                task.updated_at = now;
                tx.put("run", run_id, &run)?;
                tx.put("task", task.task_id, &task)?;
                tx.emit(Event::RunUpserted { run: Box::new(run) })?;
                tx.emit(Event::TaskUpserted { task: task.clone() })?;
                tx.emit(Event::TimelineAppended {
                    task_id: task.task_id,
                    item: TimelineItem {
                        seq: 0,
                        at: now,
                        run_id: Some(run_id),
                        item: TimelineBody::Approval { approval_id: id },
                    },
                })?;
                Ok(())
            })
        {
            self.inner.waits.lock().await.remove(&id);
            return Err(e);
        }
        let decision = tokio::time::timeout(wait_for, receiver).await;
        self.inner.waits.lock().await.remove(&id);
        match decision {
            Ok(Ok(d)) => Ok(d),
            _ => {
                self.write(|tx| {
                    let mut a: Approval = tx.read().require("approval", id)?;
                    if crate::carry_on::blocks(&tx.read(), &a)? {
                        a.state = ApprovalState::Expired;
                        a.revision += 1;
                        tx.put("approval", id, &a)?;
                        tx.emit(Event::ApprovalUpserted { approval: a })?;
                    }
                    Ok(())
                })?;
                Err(error(ErrorCode::Expired, "Human decision was not received"))
            }
        }
    }
}
#[async_trait]
impl BridgeHandler for CompanionHub {
    async fn bind(&self, token: &str) -> Option<BridgeBinding> {
        if token.len() > 100 {
            return None;
        }
        let digest = hash(token);
        let bound = self
            .write(|tx| {
                let record = tx
                    .read()
                    .list::<TokenRecord>("run_token")?
                    .into_iter()
                    .find(|t| bool::from(t.hash.ct_eq(&digest)) && t.epoch == self.inner.epoch);
                let Some(record) = record else {
                    return Ok(None);
                };
                let run: Run = tx.read().require("run", record.run_id)?;
                if tx
                    .read()
                    .get::<bool>("bridge_bound", run.run_id)?
                    .unwrap_or(false)
                    || tx
                        .read()
                        .get::<bool>("stopped", run.run_id)?
                        .unwrap_or(false)
                    || !matches!(
                        run.state,
                        RunState::Dispatching
                            | RunState::Working
                            | RunState::AwaitingApproval
                            | RunState::AwaitingInput
                    )
                {
                    return Ok(None);
                }
                let task: Task = tx.read().require("task", run.task_id)?;
                let project: Project = tx.read().require("project", task.project_id)?;
                tx.put("bridge_bound", run.run_id, &true)?;
                let answers = crate::checklist::answers(&tx.read(), task.task_id)?;
                let browser = self.browses(&task) && !answers;
                let github = self.githubs(&tx.read(), &task)? && !answers;
                // Claimed while the run's credential is sure to stand, so
                // its end, which withdraws that first, lets it go.
                let previous = browser
                    .then(|| self.claim_browser(task.task_id, run.run_id))
                    .flatten();
                Ok(Some((
                    BridgeBinding {
                        run_id: run.run_id,
                        target_id: task.target_id,
                        computer_use: task.kind == TaskKind::ComputerUse && !answers,
                        browser,
                        github,
                        checklist_answers: answers,
                    },
                    task,
                    project,
                    previous,
                )))
            })
            .ok()
            .flatten();
        let (binding, task, project, previous) = bound?;
        if binding.browser {
            self.start_browsing(binding.run_id, &task, &project, previous)
                .await;
        }
        Some(binding)
    }
    async fn handle(&self, binding: &BridgeBinding, request: BridgeRequest) -> BridgeResponse {
        match self.broker_request(binding, request).await {
            Ok(r) => r,
            Err(e) => BridgeResponse::Refused { reason: e.message },
        }
    }
    async fn unbound(&self, binding: &BridgeBinding) {
        // The connection is consumed. Cancel its approvals and withdraw managed authority.
        let _ = self.revoke_run(binding.run_id).await;
        let _ = self.write(|tx| {
            for mut approval in tx.read().list::<Approval>("approval")? {
                // One given back to its agent stays open for an answer.
                if approval.run_id == binding.run_id
                    && crate::carry_on::blocks(&tx.read(), &approval)?
                    && tx
                        .read()
                        .get::<bool>("bridge_approval", approval.approval_id)?
                        .unwrap_or(false)
                {
                    approval.state = ApprovalState::Expired;
                    approval.revision += 1;
                    tx.put("approval", approval.approval_id, &approval)?;
                    tx.emit(Event::ApprovalUpserted {
                        approval: approval.clone(),
                    })?;
                    crate::runs::resolve_attention(
                        tx,
                        approval.approval_id,
                        self.inner.clock.now(),
                    )?;
                }
            }
            Ok(())
        });
    }
}
