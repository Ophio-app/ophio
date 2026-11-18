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
    pub(crate) epoch: BootEpoch,
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
        let (task, github, answers, read_only, checks, check_names) = self.write(|tx| {
            let run: Run = tx.read().require("run", run_id)?;
            let task: Task = tx.read().require("task", run.task_id)?;
            let github = self.githubs(&tx.read(), &task)?;
            let answers = crate::checklist::answers(&tx.read(), task.task_id)?;
            let read_only = read_only(&run, &task, answers);
            let checks = crate::checks_bridge::available(&task, answers);
            let check_names = if checks {
                tx.read().require::<Project>("project", task.project_id)?;
                tx.read()
                    .project_checks(task.project_id)?
                    .into_iter()
                    .flat_map(|approved| approved.checks)
                    .filter(|check| {
                        check.enabled && check.when == protocol::checks::CheckWhen::EveryTask
                    })
                    .map(|check| check.name)
                    .collect()
            } else {
                Vec::new()
            };
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
            Ok((task, github, answers, read_only, checks, check_names))
        })?;
        Ok(ports::ToolBridge {
            server_name: "computer".into(),
            command: config.executable.clone(),
            args: Vec::new(),
            env: vec![
                (RUN_TOKEN_ENV.into(), token),
                (SOCKET_ENV.into(), config.socket.clone()),
            ],
            permission_tool: (!read_only).then(|| "permission_prompt".into()),
            computer_use: task.kind == TaskKind::ComputerUse && !answers && !read_only,
            browser: self.browses(&task) && !answers && !read_only,
            github: github && !answers && !read_only,
            checklist_answers: answers,
            read_only,
            chat: task.conversation,
            checks,
            check_names,
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
                || (task.kind == TaskKind::ComputerUse && !answers && !binding.read_only)
                    != binding.computer_use
                || (binding.browser && !self.browses(&task))
                || answers != binding.checklist_answers
                || read_only(&run, &task, answers) != binding.read_only
                || (binding.checks && !crate::checks_bridge::available(&task, answers))
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
        if binding.read_only
            && matches!(
                request,
                BridgeRequest::Checks {
                    action: ChecksCall::Propose { .. }
                }
            )
        {
            return Err(error(
                ErrorCode::PermissionDenied,
                "This turn only reads; drafting checks needs a Work task",
            ));
        }
        // A read-only turn may read checks, ask for more, or propose work,
        // which starts nothing until the person does.
        if binding.read_only
            && !matches!(
                request,
                BridgeRequest::RequestAccess { .. }
                    | BridgeRequest::Checks {
                        action: ChecksCall::Read
                    }
                    | BridgeRequest::Work {
                        action: WorkCall::Propose { .. }
                    }
            )
        {
            return Err(error(
                ErrorCode::PermissionDenied,
                "This turn only reads; ask for more with request_access",
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
                // What the person declined in this turn is declined again
                // at once, without another card.
                let asked = tool_approval(&tool_name, &shown);
                let again = crate::more_access::Declined {
                    kind: asked.kind,
                    access: false,
                    request: crate::more_access::Declined::request_of(
                        &asked.title,
                        Some(&raw_request(&shown)),
                    ),
                    needs: Vec::new(),
                };
                if asked.kind != ApprovalKind::Question
                    && self
                        .read(|r| crate::more_access::declined_before(r, binding.run_id, &again))?
                {
                    let deny = ApprovalDecision::Deny {
                        reason: Some(crate::more_access::ASKED_AGAIN.into()),
                    };
                    return Ok(BridgeResponse::Permission {
                        decision: permission_reply(&tool_name, input, deny),
                    });
                }
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
                            headline: None,
                            subject: Default::default(),
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
                        "Your browser runs on the computer itself, outside this chat's sandbox, so it opens pages only while the person has given this chat both Internet and Local network. Ask for both with request_access, or fetch the page with curl, which stays inside it.",
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
            BridgeRequest::Checks { action } => Ok(BridgeResponse::ChecksResult {
                text: self.checks_request(binding, action)?,
            }),
            BridgeRequest::RequestAccess { needs, reason } => {
                // The specialist acts for another task, and a turn answering
                // on the checklist only answers.
                if binding.computer_use || binding.checklist_answers {
                    return Err(error(
                        ErrorCode::PermissionDenied,
                        "This turn can't ask for more access",
                    ));
                }
                Ok(BridgeResponse::AccessResult {
                    text: self
                        .access_request(binding.run_id, &task, needs, reason)
                        .await?,
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
                // An app is chosen before the display is locked: reading the
                // app list mustn't keep the display's renewals waiting.
                let chosen = match &request {
                    BridgeRequest::OpenApp { name } => Some(self.choose_app(name).await),
                    _ => None,
                };
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
                if let BridgeRequest::OpenApp { .. } = &request {
                    let (launcher, app) = chosen.ok_or_else(|| {
                        error(ErrorCode::InvalidRequest, "Invalid bridge request")
                    })??;
                    return self
                        .open_app(binding.run_id, &mut target, launcher, app)
                        .await;
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
    /// Opens the installed app `name` names, for a computer-use run holding
    /// `target`'s agent lease: the same app list, matching and launcher as
    /// voice control. The name only chooses among installed apps. Several
    /// matches, none, or a power, session or removal tool are refused with
    /// what to do instead; an unconfirmed launch ends the lease, as
    /// unconfirmed input does.
    /// The installed app `name` names, and the launcher to open it with:
    /// the same app list and matching as voice control. Several matches,
    /// none, or a power, session or removal tool are refused with what to do
    /// instead.
    async fn choose_app(
        &self,
        name: &str,
    ) -> Result<
        (
            std::sync::Arc<dyn voice_control::Launcher>,
            voice_control::apps::LaunchApp,
        ),
        ProtocolError,
    > {
        use voice_control::apps::{Resolution, label, resolve, risky};
        let launcher = self
            .voice_config()
            .map_err(|_| unsupported("Opening apps"))?
            .launcher
            .clone();
        let unavailable = |e: voice_control::Error| {
            error(
                ErrorCode::TemporarilyUnavailable,
                format!("Not opened: {e}"),
            )
        };
        let apps = launcher.apps().await.map_err(unavailable)?;
        let app = match resolve(name, &apps) {
            Resolution::One(app) => app,
            Resolution::Several(several) => {
                let names: Vec<String> = several.iter().map(|a| label(a, &several)).collect();
                return Err(error(
                    ErrorCode::InvalidRequest,
                    format!(
                        "Not opened: several apps match “{name}”: {}. Call open_app with one of these names.",
                        names.join(", ")
                    ),
                ));
            }
            Resolution::None(reason) => {
                return Err(error(ErrorCode::NotFound, format!("Not opened: {reason}.")));
            }
        };
        if risky(&app) {
            return Err(error(
                ErrorCode::PermissionDenied,
                format!(
                    "Not opened: {} is a power, session or removal tool. Ask the person with request_human to open it themselves.",
                    app.name
                ),
            ));
        }
        Ok((launcher, app))
    }
    async fn open_app(
        &self,
        run_id: RunId,
        target: &mut crate::control::Target,
        launcher: std::sync::Arc<dyn voice_control::Launcher>,
        app: voice_control::apps::LaunchApp,
    ) -> Result<BridgeResponse, ProtocolError> {
        let unavailable = |e: voice_control::Error| {
            error(
                ErrorCode::TemporarilyUnavailable,
                format!("Not opened: {e}"),
            )
        };
        if self.run_stopped(run_id)? {
            return Err(error(
                ErrorCode::Expired,
                "Run stopped before the app opened",
            ));
        }
        let lease = target
            .view
            .lease
            .as_ref()
            .ok_or_else(|| error(ErrorCode::Expired, "Agent lease is no longer active"))?;
        let authority = AgentAuthority {
            hub: self.clone(),
            run_id,
            lease: target.live.clone(),
            lease_id: lease.lease_id,
            generation: lease.generation,
        };
        // The launch runs in a task of its own, so a call dropped when the
        // agent's connection closes can't abandon it halfway: it finishes,
        // and the display keeps it until it has, and keeps it if it
        // couldn't be stopped.
        let guard = target.launches.begin();
        let chosen = protocol::voice::VoiceApp {
            id: app.id.clone(),
            name: app.name.clone(),
        };
        let launched = tokio::spawn(async move {
            let authority = std::sync::Arc::new(crate::launch_recheck::Recording::owned(
                std::sync::Arc::new(authority),
            ));
            let result = launcher.launch_authorized(&chosen, authority.clone()).await;
            // A panic never gets here: its guard then counts as unstopped.
            guard.ended(
                !matches!(result, Err(voice_control::Error::LaunchUnstopped)),
                authority.take(),
            );
            result
        })
        .await
        .unwrap_or(Err(voice_control::Error::LaunchUnstopped));
        match launched {
            Ok(()) => Ok(BridgeResponse::AppOpened {
                text: format!(
                    "Asked the desktop to open {}. Its window may take a moment to appear: call observe_target.",
                    app.name
                ),
            }),
            // Its processes may still open an app: the agent's control ends
            // now, and no release of this display is confirmed from here on.
            Err(voice_control::Error::LaunchUnstopped) => {
                self.mark_launch(target);
                let released = self.revoke_locked(target, false).await;
                Err(crate::control::unresolved_release(released))
            }
            Err(voice_control::Error::OutcomeUnknown) => {
                self.revoke_locked(target, false).await?;
                Err(error(
                    ErrorCode::OutcomeUnknown,
                    "The desktop didn't confirm opening the app",
                ))
            }
            Err(e) => Err(unavailable(e)),
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
        let now = self.inner.clock.now();
        // A private question expires as a login does: its answer may only
        // go to the live question.
        let carries_on =
            crate::carry_on::carries_on(facts.kind) && !facts.questions.iter().any(|q| q.secret);
        let project: Option<Project> = self.read(|r| r.get("project", task.project_id))?;
        let facts_line = crate::glance::approval_facts(&facts.subject, project.as_ref());
        let approval = Approval {
            approval_id: id,
            run_id,
            task_id: task.task_id,
            project_id: task.project_id,
            native_request_id: native_id.unwrap_or_else(|| format!("bridge:{id}")),
            kind: facts.kind,
            title: facts.title,
            headline: facts.headline,
            facts: facts_line,
            detail: facts.detail,
            detail_lines: facts.detail_lines,
            questions: facts.questions,
            raw_request: raw,
            scope: facts.scope,
            shortened: facts.shortened,
            revision: 1,
            state: ApprovalState::Pending,
            access_request: None,
            actions: None,
            delivery: None,
            outcome: None,
            requested_at: now,
            expires_at: (!carries_on)
                .then(|| protocol::time::Timestamp(now.0.saturating_add(600_000))),
            resolved_by: None,
            resolved_at: None,
        };
        self.bridge_wait_on(approval, tool_call).await
    }
    /// Raises `approval` for its run's agent, which waits on the bridge, and
    /// waits for the person's decision.
    pub(crate) async fn bridge_wait_on(
        &self,
        mut approval: Approval,
        tool_call: Option<String>,
    ) -> Result<ApprovalDecision, ProtocolError> {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let (id, run_id, now) = (approval.approval_id, approval.run_id, approval.requested_at);
        let task: Task = self.read(|r| r.require("task", approval.task_id))?;
        let task = &task;
        // One that may be answered after its agent has moved on keeps no
        // deadline; it goes back to the agent unanswered before this wait
        // would end.
        let carries_on = crate::carry_on::outlives(&approval);
        let wait_for = std::time::Duration::from_secs(if carries_on { 900 } else { 600 });
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
        let admitted =
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
                // The same request a switch to Work or a late Allow allowed
                // is allowed again, once, without asking.
                crate::switch_work::take_preapproved(tx, &approval, now)
            });
        let preapproved = match admitted {
            Ok(by) => by,
            Err(e) => {
                self.inner.waits.lock().await.remove(&id);
                return Err(e);
            }
        };
        if let Some(by) = preapproved {
            let caller = crate::api::Caller {
                principal: by,
                grants: protocol::auth::Grants::owner(),
            };
            // A failure leaves it for the person, as any other request.
            let _ = self
                .resolve_approval(&caller, id, &ApprovalDecision::Allow)
                .await;
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
/// Whether `run` only reads (Plan, Ask, not a conversation's turn, which is
/// held to its access and works), so its bridge offers only
/// `request_access`. A turn answering on the checklist has its own tools.
fn read_only(run: &Run, task: &Task, answers: bool) -> bool {
    !answers
        && crate::runs::run_mode(run, task) != TaskMode::Work
        && !(task.conversation && task.access.is_some())
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
                let read_only = read_only(&run, &task, answers);
                let browser = self.browses(&task) && !answers && !read_only;
                let github = self.githubs(&tx.read(), &task)? && !answers && !read_only;
                // Claimed while the run's credential is sure to stand, so
                // its end, which withdraws that first, lets it go.
                let previous = browser
                    .then(|| self.claim_browser(task.task_id, run.run_id))
                    .flatten();
                Ok(Some((
                    BridgeBinding {
                        run_id: run.run_id,
                        target_id: task.target_id,
                        computer_use: task.kind == TaskKind::ComputerUse && !answers && !read_only,
                        browser,
                        github,
                        checklist_answers: answers,
                        read_only,
                        checks: crate::checks_bridge::available(&task, answers),
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
        // The connection is consumed. Withdraw managed authority. Its
        // agent has stopped waiting on what it asked: what may be answered
        // later stays open as if it had gone back to the agent, unless the
        // person stopped the turn; anything else expires.
        let _ = self.revoke_run(binding.run_id).await;
        let now = self.inner.clock.now();
        let _ = self.write(|tx| crate::carry_on::outlive_wait(tx, binding.run_id, now));
        // Nothing receives an answer on this connection now: one to what
        // stays open is the agent's next message.
        let asked: Vec<ApprovalId> = self
            .read(|r| r.list::<Approval>("approval"))
            .unwrap_or_default()
            .into_iter()
            .filter(|a| a.run_id == binding.run_id)
            .map(|a| a.approval_id)
            .collect();
        {
            let mut waits = self.inner.waits.lock().await;
            for id in &asked {
                waits.remove(id);
            }
        }
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

/// Whether a computer-use run may still open an app, asked by the launcher
/// just before the app starts: no Local Stop or shutdown has begun, its
/// agent lease is the display's active one and the run hasn't stopped.
struct AgentAuthority {
    hub: CompanionHub,
    run_id: RunId,
    lease: std::sync::Arc<crate::control::LiveLease>,
    lease_id: LeaseId,
    generation: u64,
}
impl voice_control::Authority for AgentAuthority {
    fn check(&self) -> Result<(), voice_control::Error> {
        let inner = &self.hub.inner;
        // A Local Stop or shutdown counts the moment it begins, before it
        // reaches the display: it waits for the display's lock, which the
        // launch holds, so the lease still looks active meanwhile.
        if inner.local_stops.load(std::sync::atomic::Ordering::SeqCst) != 0
            || *inner.shutdown.borrow()
            || !self
                .lease
                .holds(self.lease_id, self.generation, inner.clock.monotonic())
            || self.hub.run_stopped(self.run_id).unwrap_or(true)
        {
            return Err(voice_control::Error::Stopped);
        }
        Ok(())
    }
}
