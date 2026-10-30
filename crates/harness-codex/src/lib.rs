// SPDX-License-Identifier: Apache-2.0
//! Native Codex app-server adapter; authentication remains inside Codex.
mod approval;
mod catalog;
mod computer;
mod disabled;
pub use disabled::{terminal_arguments, terminal_arguments_at};
mod io;
mod launch;
mod limits;
mod modes;
mod notifications;
mod readiness;
mod results;
mod servers;
mod wire;

use approval::PendingApproval;
use async_trait::async_trait;
use base64::Engine;
use ports::*;
use protocol::{
    capability::*,
    command::{ApprovalDecision, MessageDelivery},
    model::{DeliveryState, TaskMode, UsageSummary},
};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::{Path, PathBuf},
    process::ExitStatus,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{
    process::{Child, ChildStdin},
    sync::{mpsc, oneshot, watch},
};
use wire::{Envelope, Id, Input, ThreadResponse, TurnResponse};

/// EOF is the stdio app-server shutdown mechanism (no shutdown RPC in v2).
const SHUTDOWN_GRACE: Duration = Duration::from_secs(3);
/// Bound Stop even when the app-server acknowledges but never completes the turn.
const STOP_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Default)]
pub struct CodexAdapter {
    binary: Option<PathBuf>,
    copy: Option<PathBuf>,
    readiness_warning: Arc<std::sync::Mutex<Option<String>>>,
}
impl CodexAdapter {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn with_binary(binary: impl Into<PathBuf>) -> Self {
        Self {
            binary: Some(binary.into()),
            ..Self::default()
        }
    }
    /// Starts Øphio's own copy in `folder` while there is one (see
    /// [`ports::agent_copy`]); a binary given by path still comes first.
    pub fn with_copy(mut self, folder: impl Into<PathBuf>) -> Self {
        self.copy = Some(folder.into());
        self
    }
    fn program(&self) -> Option<PathBuf> {
        if self.binary.is_none()
            && let Some(copy) = self.copy.as_deref().and_then(ports::agent_copy::current)
        {
            return Some(copy);
        }
        io::locate(&self.binary, "codex")
    }
}

/// The computer's own `codex` program, as a task would find it without a
/// path given or a copy of Øphio's.
pub fn installed() -> Option<PathBuf> {
    io::locate(&None, "codex")
}
#[async_trait]
impl HarnessAdapter for CodexAdapter {
    fn kind(&self) -> HarnessKind {
        HarnessKind::Codex
    }
    async fn probe(&self) -> HarnessInfo {
        let binary = self.program();
        // Each check is a process of its own, and none needs another's answer.
        let (version, account, catalog, capability, modes) = match &binary {
            Some(path) => {
                let (version, account, catalog, capability, modes) = tokio::join!(
                    version(path),
                    account(path),
                    catalog::probe(path),
                    readiness::inspect(path),
                    modes::probe(path),
                );
                (version, account, Some(catalog), Some(capability), modes)
            }
            None => (
                None,
                AccountReadiness::Unknown {
                    reason: NO_LOGIN_STATUS.into(),
                },
                None,
                None,
                Vec::new(),
            ),
        };
        let installed = if version.is_some() {
            Availability::Available
        } else {
            Availability::Unsupported {
                reason: "Install Codex on the computer or configure its executable path".into(),
            }
        };
        let start_run = if let (Some(binary), Some(capability)) = (&binary, capability) {
            let warning = self
                .readiness_warning
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            readiness::report(
                binary,
                version.as_deref(),
                capability,
                readiness::candidates(),
                warning,
            )
            .await
        } else {
            installed.clone()
        };
        let (models, catalog) = match (catalog, &binary) {
            (Some((models, mut observation)), Some(binary)) => {
                observation.source = format!(
                    "{} · {} · {}",
                    binary.display(),
                    version.as_deref().unwrap_or("version unknown"),
                    observation.source
                );
                (models, Some(observation))
            }
            _ => (Vec::new(), None),
        };
        HarnessInfo {
            catalog,
            usage_limit: None,
            kind: self.kind(),
            display_name: "Codex".into(),
            native_version: version.clone(),
            integration: IntegrationLevel::Managed,
            installed: installed.clone(),
            account,
            models,
            efforts: vec![],
            modes: if version.is_some() { modes } else { Vec::new() },
            capabilities: HarnessCapabilities {
                cancel_queued_message: Some(installed.clone()),
                start_run,
                steer_running: Availability::Available,
                interrupt: Availability::Available,
                native_approvals: Availability::Available,
                resume_session: Availability::Available,
                child_agents: Availability::Available,
                computer_use_delegation: if version.is_none() {
                    installed.clone()
                } else if computer::supported(version.as_deref()) {
                    Availability::Available
                } else {
                    Availability::Unsupported {
                        reason: format!(
                            "Update Codex to {}.{} or later to use it for computer use",
                            computer::VERSION.0,
                            computer::VERSION.1
                        ),
                    }
                },
                model_selection: Availability::Available,
                effort_selection: Availability::Available,
            },
        }
    }
    async fn start_run(&self, mut spec: RunSpec) -> Result<RunStarted, HarnessError> {
        let binary = self
            .program()
            .ok_or_else(|| HarnessError::NotInstalled("Codex".into()))?;
        let carrying_plan = leaves_read_only(&spec);
        let native_skill = matches!(spec.start, Some(ports::RunStart::Skill { .. }))
            && ports::harness::native_start(&spec, HarnessKind::Codex).is_some();
        let mut policy = None;
        if native_skill {
            // A version that can't be sent the skill is given it instead, in
            // every run: a resumed one too, which sends no skill.
            let read = policy.insert(modes::inspect(&binary).await);
            if !read.skill_input
                && let Some(start) = &mut spec.start
            {
                start.drop_native();
            }
        }
        let native_instructions = if spec.mode != TaskMode::Work || carrying_plan {
            let policy = match policy {
                Some(policy) => policy,
                None => modes::inspect(&binary).await,
            };
            if carrying_plan && !policy.work_resume {
                return Err(HarnessError::Unsupported("This Codex version cannot confirm a change from a read-only session to Work. Update Codex before continuing this task.".into()));
            }
            if !policy.read_only {
                return Err(HarnessError::Unsupported(modes::MISSING.into()));
            }
            policy.developer_instructions
        } else {
            true
        };
        let computer_use = spec.bridge.as_ref().is_some_and(|b| b.computer_use);
        if computer_use && spec.mode != TaskMode::Work {
            return Err(HarnessError::Unsupported(
                "Computer use needs Work mode.".into(),
            ));
        }
        if computer_use && (spec.launch_policy.is_some() || carrying_plan) {
            return Err(HarnessError::Unsupported(
                "Computer use runs with its own permissions, so launch flags and plans don’t apply to it.".into(),
            ));
        }
        if spec.mission.as_ref().is_some_and(|m| {
            m.bounds
                .iter()
                .any(|b| b.enforcement == protocol::model::Enforcement::Enforced)
        }) {
            return Err(HarnessError::Unsupported(
                "Codex can’t enforce boundaries; they can only be requested.".into(),
            ));
        }
        // Read-only runs and computer use don't reach the person's own servers.
        let own = if launch::relay(&spec).is_some() {
            vec![]
        } else if spec.mode != TaskMode::Work || computer_use || spec.sandbox.is_some() {
            servers::listed(&binary, &spec).await?
        } else {
            vec![]
        };
        let computer = match spec.bridge.as_ref().filter(|b| b.computer_use) {
            Some(bridge) => {
                Some(computer::Launch::prepare(&spec, &bridge.server_name, &own).await?)
            }
            None => None,
        };
        // The specialist asks through the bridge's request_human instead.
        let asks = computer.is_none() && modes::asks(&binary).await;
        let fallback = if native_instructions {
            None
        } else {
            run_instructions(&spec, asks)?
        };
        let mut initial = prepare_framed(&spec.prompt, &spec.cwd, fallback.as_deref()).await?;
        if let Some((name, path)) = invoked_skill(&spec) {
            if let Some(Input::Text { text, .. }) = initial.first_mut() {
                *text = format!("${name} {text}");
            }
            initial.push(Input::Skill {
                name: name.to_owned(),
                path: path.to_owned(),
            });
        }
        let bridge = bridge_server(&spec)?;
        let visible: Vec<_> = bridge
            .as_ref()
            .map(|(file, _)| file.path().to_owned())
            .into_iter()
            .chain(spec.prompt.images.iter().map(|i| i.path.clone()))
            .collect();
        let wrapped = match &spec.sandbox {
            Some(sandbox) => sandbox.wrap(ports::sandbox::WrapRequest {
                program: &binary,
                visible: &visible,
                env: &spec.env,
                resume: spec.resume_session.as_deref(),
            })?,
            None => ports::sandbox::unwrapped(&binary),
        };
        let mut command = io::command(&wrapped.program);
        if wrapped.clear_env {
            command.env_clear();
        }
        command.args(&wrapped.args);
        if spec.sandbox.is_some() {
            command.arg(&binary);
        }
        command
            .current_dir(&spec.cwd)
            .args(arguments(
                &spec,
                bridge.as_ref().map(|(_, server)| server.as_str()),
                asks,
                computer.as_ref(),
                &own,
            )?)
            .envs(
                spec.env
                    .0
                    .iter()
                    .filter(|_| !wrapped.clear_env)
                    .map(|(k, v)| (k, v)),
            );
        command.envs(wrapped.env);
        if launch::relay(&spec).is_some() {
            command.env(launch::RELAY_KEY_ENV, "ophio-relay-not-a-key");
        }
        if let Some(sandbox) = &spec.sandbox {
            sandbox.prepare(command.as_std_mut())?;
        }
        let container = Arc::new(containment::Container::new()?);
        let mut child = container.spawn(&mut command).await?;
        if let Some(sandbox) = &spec.sandbox
            && let Err(error) = sandbox.started().await
        {
            let _ = container.end().await;
            let _ = child.wait().await;
            return Err(error);
        }

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| HarnessError::Protocol("missing native stdin".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| HarnessError::Protocol("missing native stdout".into()))?;
        let (events_tx, events) = mpsc::channel(256);
        let (normal_tx, normal_rx) = mpsc::channel(32);
        let (urgent_tx, urgent_rx) = mpsc::channel(8);
        let (done_tx, done_rx) = watch::channel(false);
        let closed = Arc::new(AtomicBool::new(false));
        let control = Arc::new(Control {
            normal: normal_tx,
            urgent: urgent_tx,
            done: done_rx,
            cwd: spec.cwd.clone(),
            fallback,
            process_id: child.id(),
            closed: closed.clone(),
            container: container.clone(),
        });
        let readiness_warning = self.readiness_warning.clone();
        let guard = containment::Guard::new(container);
        tokio::spawn(async move {
            let mut actor = Actor {
                child,
                stdin: Some(stdin),
                frames: io::Frames::new(stdout),
                events: events_tx,
                spec,
                native_instructions,
                asks,
                computer,
                initial: Some(initial),
                sequence: 0,
                pending: HashMap::new(),
                approvals: HashMap::new(),
                queue: VecDeque::new(),
                thread: None,
                turn: None,
                starting: false,
                usage: None,
                results: results::Results::default(),
                turn_exit: None,
                account_label: None,
                finished_turns: VecDeque::new(),
                deferred_interrupts: Vec::new(),
                cancelled_before_start: false,
                unknown: HashSet::new(),
                file_changes: HashMap::new(),
                readiness_warning,
                closed,
                rate_limits: limits::RateWindows::default(),
                usage_limited: false,
            };
            // Kept until the run ends, so a bridge never started still has
            // its file removed.
            let _bridge = bridge;
            // Declared after the agent's handle, so a dropped task kills the
            // container before that handle lets the agent be reaped.
            let guard = guard;
            let exit = match actor.run(normal_rx, urgent_rx).await {
                Ok(exit) => exit,
                Err(error) => {
                    io::kill(&mut actor.child);
                    actor.turn_exit.clone().unwrap_or_else(|| RunExit::Failed {
                        reason: error.to_string(),
                    })
                }
            };
            let exit = if matches!(exit, RunExit::Completed) && !actor.queue.is_empty() {
                RunExit::Failed { reason: "Codex closed before your follow-up messages reached it; send them again to continue".into() }
            } else {
                exit
            };
            let ended = guard.end().await;
            if let Err(error) = &ended {
                tracing::warn!("{error}");
                let _ = actor
                    .events
                    .send(HarnessEvent::Warning {
                        text: containment::STILL_RUNNING.into(),
                    })
                    .await;
            }
            let _ = done_tx.send(true);
            for (_, approval) in actor.approvals.drain() {
                let _ = actor
                    .events
                    .send(HarnessEvent::ApprovalClosed {
                        native_request_id: approval.id.key(),
                        outcome: None,
                    })
                    .await;
            }
            if ended.is_err() {
                // The agent, or what it started, may not have ended. The run
                // stays open until that is confirmed; Stop and Force stop try
                // again at once.
                let _ = actor.events.send(HarnessEvent::CleanupPending).await;
                guard.confirm_end().await;
            }
            // Only now is the agent reaped: the container's confirmed end
            // includes the agent's exit, and until then its id had to keep
            // naming the run's process group.
            let _ = actor.child.start_kill();
            let _ = actor.child.wait().await;
            if let Some(sandbox) = &actor.spec.sandbox
                && let Err(error) = sandbox.finished().await
            {
                let _ = actor
                    .events
                    .send(HarnessEvent::Warning {
                        // Said in full by the sandbox; the agent did start.
                        text: match error {
                            HarnessError::SpawnFailed(why) => why,
                            other => other.to_string(),
                        },
                    })
                    .await;
            }

            let _ = actor
                .events
                .send(HarnessEvent::Exited {
                    exit,
                    outcome: None,
                })
                .await;
        });
        Ok(RunStarted { control, events })
    }
}

const NO_LOGIN_STATUS: &str =
    "Run codex login on the computer with ChatGPT; native login status was unavailable";

async fn version(binary: &Path) -> Option<String> {
    io::inspect(binary, &["--version"])
        .await
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
}

async fn account(binary: &Path) -> AccountReadiness {
    let Some(output) = io::inspect(binary, &["login", "status"]).await else {
        return AccountReadiness::Unknown {
            reason: NO_LOGIN_STATUS.into(),
        };
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if output.status.success()
        && (stdout.contains("Logged in using ChatGPT")
            || stderr.contains("Logged in using ChatGPT"))
    {
        AccountReadiness::SignedIn {
            account_label: None,
            plan: None,
        }
    } else if stdout.contains("Not logged in") || stderr.contains("Not logged in") {
        AccountReadiness::SignedOut {
            remedy: "Run codex login on the computer".into(),
        }
    } else {
        AccountReadiness::Unknown {
            reason: NO_LOGIN_STATUS.into(),
        }
    }
}

fn arguments(
    spec: &RunSpec,
    bridge: Option<&str>,
    asks: bool,
    computer: Option<&computer::Launch>,
    own: &[String],
) -> Result<Vec<String>, HarnessError> {
    launch::validate(spec)?;
    let computer_use = spec.bridge.as_ref().is_some_and(|b| b.computer_use);
    if computer_use != computer.is_some() || (computer_use && (asks || bridge.is_none())) {
        return Err(HarnessError::Protocol(
            "computer use was not prepared for this run".into(),
        ));
    }
    let mut args = vec![
        "app-server".into(),
        "--listen".into(),
        "stdio://".into(),
        "-c".into(),
        "forced_login_method=\"chatgpt\"".into(),
        "-c".into(),
        "model_provider=\"openai\"".into(),
    ];
    if asks {
        args.extend(["-c".into(), format!("features.{}=true", modes::QUESTIONS)]);
    }
    if let Some(computer) = computer {
        args.extend(computer.arguments());
    }
    if let Some(off) = servers::off(own) {
        args.extend(["-c".into(), off]);
    }
    // Codex asks for approval through app-server requests, never through the
    // bridge's permission tool. A Work run gets a bridge server for the tools
    // it may use, with the permission tool left out: it would only let the
    // model raise approval cards that authorize nothing.
    if let Some(server) = bridge {
        args.extend(["-c".into(), server.into()]);
    }
    if spec.mode != TaskMode::Work || spec.sandbox.is_some() {
        // Scope these overrides to this process, including resumed threads.
        // External tools are not constrained by the command sandbox; the
        // person's own servers are switched off above.
        for setting in [
            "apps={_default={enabled=false}}",
            "features.plugins=false",
            "features.hooks=false",
        ] {
            args.extend(["-c".into(), setting.into()]);
        }
    }
    if spec.sandbox.is_some() {
        // Provider-hosted search bypasses the local network namespace.
        args.extend(["-c".into(), "web_search=\"disabled\"".into()]);
    }
    launch::arguments(spec, &mut args);
    disabled::arguments(spec, &mut args);
    launch::relay_arguments(spec, &mut args)?;
    Ok(args)
}
/// The bridge server giving a Work run the tools that show the person things,
/// the agent browser's tools and the GitHub tools, the computer-use
/// specialist its six computer tools and nothing else, or a read-only turn
/// answering on the checklist the checklist's read and reply tools alone, and the owner-only file
/// holding its credentials. Codex hands a server only the
/// environment written into its configuration, which would put the run
/// token on Codex's command line; the bridge reads this file instead and
/// deletes it. The broker keeps each tool's rules, so Codex runs them
/// without asking.
fn bridge_server(
    spec: &RunSpec,
) -> Result<Option<(tempfile::NamedTempFile, String)>, HarnessError> {
    // A read-only turn has one only to answer on the checklist.
    let Some(bridge) =
        (spec.bridge.as_ref()).filter(|b| spec.mode == TaskMode::Work || b.checklist_answers)
    else {
        return Ok(None);
    };
    if bridge.server_name.is_empty()
        || !(bridge.server_name.chars()).all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(HarnessError::Protocol("invalid bridge server name".into()));
    }
    let value = |name: &str| {
        (bridge.env.iter())
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
            .ok_or_else(|| HarnessError::Protocol("incomplete bridge credentials".into()))
    };
    let details = ports::bridge::BridgeFile {
        token: value(ports::bridge::RUN_TOKEN_ENV)?,
        socket: value(ports::bridge::SOCKET_ENV)?,
    };
    let mut file = tempfile::Builder::new()
        .prefix("ophio-bridge-")
        .suffix(".json")
        .tempfile()?;
    let bytes = serde_json::to_vec(&details)
        .map_err(|_| HarnessError::Protocol("invalid bridge credentials".into()))?;
    use std::io::Write;
    file.write_all(&bytes)?;
    file.flush()?;
    let path = file.path().to_string_lossy();
    let args: Vec<String> = (bridge.args.iter().map(String::as_str))
        .chain([ports::bridge::BRIDGE_FILE_ARG, &path])
        .map(toml_string)
        .collect();
    let tools: Vec<String> = if bridge.checklist_answers {
        ports::bridge::ANSWER_TOOLS.to_vec()
    } else if bridge.computer_use {
        ports::bridge::COMPUTER_TOOLS.to_vec()
    } else {
        let mut tools = ports::bridge::PERSON_TOOLS.to_vec();
        if bridge.browser {
            tools.extend(ports::bridge::BROWSER_TOOLS);
            tools.push(ports::bridge::HUMAN_TOOL);
        }
        if bridge.github {
            tools.extend(ports::bridge::GITHUB_TOOLS);
            tools.extend(ports::bridge::ASK_TOOLS);
        } else {
            tools.extend(ports::bridge::WORK_TOOLS);
        }
        tools
    }
    .into_iter()
    .map(toml_string)
    .collect();
    let server = format!(
        "mcp_servers.{}={{command={},args=[{}],enabled_tools=[{}],\
         default_tools_approval_mode=\"approve\",startup_timeout_sec=20,tool_timeout_sec=900}}",
        bridge.server_name,
        toml_string(&bridge.command.to_string_lossy()),
        args.join(","),
        tools.join(","),
    );
    Ok(Some((file, server)))
}

fn toml_string(text: &str) -> String {
    let mut quoted = String::from('"');
    for c in text.chars() {
        match c {
            '"' => quoted.push_str("\\\""),
            '\\' => quoted.push_str("\\\\"),
            c if c.is_control() => quoted.push_str(&format!("\\u{:04X}", c as u32)),
            c => quoted.push(c),
        }
    }
    quoted.push('"');
    quoted
}

/// A skill Codex finds itself, used by the first prompt of a new thread. A
/// resumed thread is given its instructions instead.
fn invoked_skill(spec: &RunSpec) -> Option<(&str, &Path)> {
    match (&spec.start, &spec.resume_session) {
        (Some(ports::RunStart::Skill { skill_file, .. }), None) => {
            ports::harness::native_start(spec, HarnessKind::Codex)
                .map(|n| (n.name.as_str(), skill_file.as_path()))
        }
        _ => None,
    }
}

/// How a run with the question tool asks the person. Codex's own guidance
/// keeps the tool to optional questions, since elsewhere nobody may be there
/// to answer; through Ophio someone always is, and a question asked in a
/// message while the run carries on is one they can miss.
const ASKING: &str = "When you need the person's decision or information before you can go on, ask with the request_user_input tool, even when you can't continue without the answer: Ophio puts the question in front of them, on their phone and in a notification, and waits for their answer. Don't ask in a message and keep working. Give two or three short options when they fit; they can always answer in their own words. If it returns no answers, they left it to you: carry on with your best judgment. Never use it to ask for permission to run commands or change files; Ophio asks for those itself.";

/// How a run with the question tool asks, when the person has turned
/// questions off and it asks only when it truly can't choose
/// ([`ports::harness::conduct_instruction`]).
const ASKING_SELDOM: &str = "When you do have to ask, use the request_user_input tool: Ophio puts the question in front of them, on their phone and in a notification, and waits for their answer. Don't ask in a message and keep working. Never use it to ask for permission to run commands or change files; Ophio asks for those itself.";

/// What the run is told: the task's own instructions, then how to ask the
/// person when it has the question tool.
fn run_instructions(spec: &RunSpec, asks: bool) -> Result<Option<String>, HarnessError> {
    let text = task_instructions(spec)?;
    let asking = if spec.questions {
        ASKING
    } else {
        ASKING_SELDOM
    };
    Ok(if asks {
        Some(text.map_or_else(|| asking.into(), |text| format!("{text}\n\n{asking}")))
    } else {
        text
    })
}

fn task_instructions(spec: &RunSpec) -> Result<Option<String>, HarnessError> {
    let mut text = spec
        .mission
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .map_err(|_| HarnessError::Protocol("invalid mission".into()))?;
    let branch = spec.branch.as_ref().map(ports::harness::branch_instruction);
    let loaded = ports::harness::start_loaded(spec, HarnessKind::Codex);
    let started =
        (spec.start.as_ref()).and_then(|start| ports::harness::start_instruction(start, loaded));
    let scratch = ports::harness::scratch_instruction(spec);
    let computer = ports::harness::computer_instruction(spec)
        .map(|text| format!("{text} {}", computer::ASKING));
    for framing in [
        started.as_deref(),
        ports::harness::task_mode_instruction(spec.mode),
        branch.as_deref(),
        spec.github.as_deref(),
        ports::harness::person_instruction(spec),
        ports::harness::conduct_instruction(spec),
        computer.as_deref(),
        scratch.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        if let Some(text) = &mut text {
            text.push_str("\n\n");
            text.push_str(framing);
        } else {
            text = Some(framing.into());
        }
    }
    if leaves_read_only(spec) {
        let framing = match spec.resume_from_mode {
            Some(TaskMode::Plan) => {
                "Work mode: carry out the plan reviewed by the user. The earlier read-only planning instruction has ended. Follow the native permissions selected for this task."
            }
            _ => {
                "Work mode: the earlier read-only instruction has ended. Follow the native permissions selected for this task."
            }
        };
        text = Some(text.map_or_else(
            || framing.to_owned(),
            |mission| format!("{mission}\n\n{framing}"),
        ));
    }
    if let Some(docs) = disabled::document_chain(spec)
        && !docs.is_empty()
    {
        text = Some(text.map_or(docs.clone(), |other| format!("{docs}\n\n{other}")));
    }
    Ok(text)
}

/// Whether a Work turn continues a session whose last turn was read-only: a
/// Plan carried out, or a task continued after it was asked about its work.
/// Codex keeps a resumed thread's sandbox unless it is set again.
fn leaves_read_only(spec: &RunSpec) -> bool {
    spec.mode == TaskMode::Work
        && matches!(spec.resume_from_mode, Some(TaskMode::Plan | TaskMode::Ask))
}

async fn prepare_framed(
    input: &PromptInput,
    cwd: &Path,
    fallback: Option<&str>,
) -> Result<Vec<Input>, HarnessError> {
    let mut prepared = prepare(input, cwd).await?;
    if let Some(framing) = fallback
        && let Some(Input::Text { text, .. }) = prepared.first_mut()
    {
        *text = format!("<task-mode-intent>\n{framing}\n</task-mode-intent>\n\n{text}");
    }
    // The message travels as one frame. Refuse it here, before it reaches the
    // running agent, whose stream would otherwise end on the oversized write.
    if serde_json::to_vec(&prepared).map_or(true, |bytes| bytes.len() >= io::MAX_MESSAGE) {
        return Err(HarnessError::Unsupported(
            "The images in this message are too large to send together. Send fewer or smaller images.".into(),
        ));
    }
    Ok(prepared)
}

async fn prepare(input: &PromptInput, cwd: &Path) -> Result<Vec<Input>, HarnessError> {
    let mut result = vec![Input::Text {
        text: input.text.clone(),
        text_elements: vec![],
    }];
    for image in &input.images {
        if !matches!(
            image.media_type.as_str(),
            "image/png" | "image/jpeg" | "image/webp" | "image/gif"
        ) {
            return Err(HarnessError::Unsupported(
                "unsupported image media type".into(),
            ));
        }
        let path = if image.path.is_absolute() {
            image.path.clone()
        } else {
            cwd.join(&image.path)
        };
        // Freeze the attachment bytes before dispatch; a mutable path must not
        // silently substitute different image content between turns.
        let file = tokio::fs::File::open(path).await?;
        use tokio::io::AsyncReadExt;
        let mut bytes = Vec::new();
        file.take(20 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .await?;
        if bytes.len() > 20 * 1024 * 1024 {
            return Err(HarnessError::Unsupported("image exceeds 20 MiB".into()));
        }
        result.push(Input::Image {
            url: format!(
                "data:{};base64,{}",
                image.media_type,
                base64::engine::general_purpose::STANDARD.encode(bytes)
            ),
        });
    }
    Ok(result)
}

type Reply<T> = oneshot::Sender<Result<T, HarnessError>>;
enum Normal {
    Send(
        MessageRef,
        Vec<Input>,
        MessageDelivery,
        Reply<DeliveryState>,
    ),
    Approval(String, ApprovalDecision, Reply<()>),
}
enum Urgent {
    Interrupt(Reply<()>),
    Cancel(MessageRef, Reply<()>),
    Shutdown,
    Terminate(Reply<()>),
}
struct Control {
    normal: mpsc::Sender<Normal>,
    urgent: mpsc::Sender<Urgent>,
    done: watch::Receiver<bool>,
    cwd: PathBuf,
    fallback: Option<String>,
    process_id: Option<u32>,
    /// Admission, closed by Stop; shared with the actor.
    closed: Arc<AtomicBool>,
    container: Arc<containment::Container>,
}
impl Control {
    async fn interrupt_agent(&self) -> Result<(), HarnessError> {
        let (tx, rx) = oneshot::channel();
        self.urgent
            .send(Urgent::Interrupt(tx))
            .await
            .map_err(|_| HarnessError::NotActive)?;
        tokio::time::timeout(Duration::from_secs(10), rx)
            .await
            .map_err(|_| {
                HarnessError::Protocol("native interrupt acknowledgement timed out".into())
            })?
            .map_err(|_| HarnessError::NotActive)?
    }
}
#[async_trait]
impl RunControl for Control {
    fn process_id(&self) -> Option<u32> {
        self.process_id
    }
    fn containment(&self) -> Option<Containment> {
        Some(self.container.containment())
    }
    async fn send_message(
        &self,
        message_ref: MessageRef,
        input: PromptInput,
        delivery: MessageDelivery,
    ) -> Result<DeliveryState, HarnessError> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(HarnessError::NotActive);
        }
        let input = prepare_framed(&input, &self.cwd, self.fallback.as_deref()).await?;
        let (tx, rx) = oneshot::channel();
        self.normal
            .send(Normal::Send(message_ref, input, delivery, tx))
            .await
            .map_err(|_| HarnessError::NotActive)?;
        rx.await.map_err(|_| {
            HarnessError::Protocol("message delivery acknowledgement lost; outcome unknown".into())
        })?
    }
    async fn cancel_message(&self, message_ref: MessageRef) -> Result<(), HarnessError> {
        let (tx, rx) = oneshot::channel();
        self.urgent
            .send(Urgent::Cancel(message_ref, tx))
            .await
            .map_err(|_| HarnessError::MessageNotQueued)?;
        rx.await
            .map_err(|_| HarnessError::Protocol("Message withdrawal acknowledgement lost".into()))?
    }
    async fn interrupt(&self) -> Result<(), HarnessError> {
        let interrupted = self.interrupt_agent().await;
        if matches!(interrupted, Err(HarnessError::NotActive)) {
            // The agent has ended, so Stop reaches what it may have left
            // running: end that now rather than at the next try.
            self.container.retry_now();
        }
        interrupted
    }
    fn close_admission(&self) {
        self.closed.store(true, Ordering::SeqCst);
    }
    async fn resolve_approval(
        &self,
        id: &str,
        decision: &ApprovalDecision,
    ) -> Result<(), HarnessError> {
        let (tx, rx) = oneshot::channel();
        self.normal
            .send(Normal::Approval(id.into(), decision.clone(), tx))
            .await
            .map_err(|_| HarnessError::NotActive)?;
        rx.await.map_err(|_| HarnessError::NotActive)?
    }
    /// Join confirmed actor completion, including a natural exit racing Force stop.
    async fn terminate(&self) -> Result<(), HarnessError> {
        let mut done = self.done.clone();
        if !*done.borrow() {
            let (reply, ended) = oneshot::channel();
            if self.urgent.send(Urgent::Terminate(reply)).await.is_ok()
                && let Ok(result) = ended.await
            {
                result?;
            }
            // Admission or the reply can close as the actor exits naturally.
            // Only its completion signal confirms that it actually finished.
            while !*done.borrow() {
                done.changed().await.map_err(|_| HarnessError::NotActive)?;
            }
        }
        let ended = self.container.end().await;
        // Whatever this found, the run's own retries report it at once.
        self.container.retry_now();
        ended
    }
    async fn shutdown(&self) -> Result<(), HarnessError> {
        if *self.done.borrow() {
            return Ok(());
        }
        // The actor may close admission between the done check and this send.
        // In either case join its completion; shutdown is idempotent.
        let _ = self.urgent.send(Urgent::Shutdown).await;
        let mut done = self.done.clone();
        while !*done.borrow() {
            done.changed().await.map_err(|_| HarnessError::NotActive)?;
        }
        Ok(())
    }
}

enum Pending {
    Initialize,
    Account,
    Thread,
    Turn(Option<MessageRef>, Option<Reply<DeliveryState>>),
    Steer(MessageRef, String, Reply<DeliveryState>),
    Interrupt(Option<Reply<()>>),
}
struct Actor {
    child: Child,
    stdin: Option<ChildStdin>,
    frames: io::Frames<tokio::process::ChildStdout>,
    events: mpsc::Sender<HarnessEvent>,
    spec: RunSpec,
    native_instructions: bool,
    /// The run has Codex's question tool and is told to ask through it.
    asks: bool,
    /// A computer-use run's settings and catalog, kept while Codex runs.
    computer: Option<computer::Launch>,
    initial: Option<Vec<Input>>,
    sequence: u64,
    pending: HashMap<Id, (tokio::time::Instant, Pending)>,
    approvals: HashMap<String, PendingApproval>,
    queue: VecDeque<(MessageRef, Vec<Input>)>,
    thread: Option<String>,
    turn: Option<String>,
    starting: bool,
    usage: Option<UsageSummary>,
    results: results::Results,
    turn_exit: Option<RunExit>,
    account_label: Option<String>,
    finished_turns: VecDeque<String>,
    deferred_interrupts: Vec<Option<Reply<()>>>,
    cancelled_before_start: bool,
    /// Notification methods already reported as unread, so each is noted once.
    unknown: HashSet<String>,
    /// Planned paths of started fileChange items, shown on their approvals.
    file_changes: HashMap<String, Vec<ports::tool_approval::Preview>>,
    readiness_warning: Arc<std::sync::Mutex<Option<String>>>,
    closed: Arc<AtomicBool>,
    rate_limits: limits::RateWindows,
    /// This turn's refusal for usage was reported; the failed turn repeats it.
    usage_limited: bool,
}
impl Actor {
    fn emit(&self, event: HarnessEvent) -> Result<(), HarnessError> {
        if let HarnessEvent::SessionStarted { native_session, .. } = &event
            && let Some(sandbox) = &self.spec.sandbox
        {
            sandbox.session_started(native_session)?;
        }
        self.events.try_send(event).map_err(|_| {
            HarnessError::Protocol("event receiver stalled or closed; run stopped".into())
        })
    }
    /// Stop withdraws every held follow-up; none starts a turn after it.
    fn withdraw_queue(&mut self) -> Result<(), HarnessError> {
        for (message_ref, _) in std::mem::take(&mut self.queue) {
            self.emit(HarnessEvent::MessageDelivery {
                message_ref,
                state: DeliveryState::Cancelled,
            })?;
        }
        Ok(())
    }
    async fn write(&mut self, value: &Value) -> Result<(), HarnessError> {
        io::write(self.stdin.as_mut().ok_or(HarnessError::NotActive)?, value).await
    }
    async fn request(
        &mut self,
        method: &str,
        params: Value,
        pending: Pending,
    ) -> Result<(), HarnessError> {
        if self.pending.len() >= 128 {
            return Err(HarnessError::Protocol(
                "too many pending native requests".into(),
            ));
        }
        self.sequence += 1;
        let id = Id::Text(format!("companion-{}", self.sequence));
        self.write(&json!({"id":id,"method":method,"params":params}))
            .await?;
        self.pending
            .insert(id, (tokio::time::Instant::now(), pending));
        Ok(())
    }
    async fn start_turn(
        &mut self,
        message_ref: Option<MessageRef>,
        input: Vec<Input>,
        reply: Option<Reply<DeliveryState>>,
    ) -> Result<(), HarnessError> {
        self.starting = true;
        self.turn_exit = None;
        self.usage = None;
        let mut params = json!({"threadId":self.thread,"input":input,"effort":self.spec.effort});
        if self.computer.is_some() {
            params["sandboxPolicy"] = json!({"type":"readOnly","networkAccess":false});
            params["approvalPolicy"] = json!("never");
            params["approvalsReviewer"] = json!("user");
        } else if self.spec.mode != TaskMode::Work {
            params["sandboxPolicy"] = json!({"type":"readOnly","networkAccess":false});
            params["approvalPolicy"] = json!("on-request");
            params["approvalsReviewer"] = json!("user");
        }
        launch::turn(&self.spec, &mut params);
        let result = self
            .request("turn/start", params, Pending::Turn(message_ref, reply))
            .await;
        if let Some(message_ref) = message_ref {
            self.emit(HarnessEvent::MessageDelivery {
                message_ref,
                state: if result.is_ok() {
                    DeliveryState::HostAccepted
                } else {
                    DeliveryState::Unknown
                },
            })?;
        }
        result
    }
    async fn interrupt(&mut self, reply: Option<Reply<()>>) -> Result<(), HarnessError> {
        let Some(turn) = self.turn.clone() else {
            if self.starting {
                // A turn/start has been written but its native id has not yet
                // arrived. Latch Stop and send it as soon as that id is known.
                if self.deferred_interrupts.len() >= 16 {
                    if let Some(reply) = reply {
                        let _ = reply.send(Err(HarnessError::Protocol(
                            "too many pending interrupts".into(),
                        )));
                    }
                } else {
                    self.deferred_interrupts.push(reply);
                }
            } else {
                if self.initial.is_some() {
                    self.cancelled_before_start = true;
                }
                if let Some(reply) = reply {
                    let _ = reply.send(Err(HarnessError::NotActive));
                }
            }
            return Ok(());
        };
        self.request(
            "turn/interrupt",
            json!({"threadId":self.thread,"turnId":turn}),
            Pending::Interrupt(reply),
        )
        .await
    }
    async fn response(
        &mut self,
        id: Id,
        result: Value,
        error: Option<Value>,
    ) -> Result<(), HarnessError> {
        let Some((_, pending)) = self.pending.remove(&id) else {
            self.emit(HarnessEvent::Warning {
                text: "Codex answered a request this app did not make, so the answer was ignored"
                    .into(),
            })?;
            return Ok(());
        };
        if let Some(error) = error {
            // Keep Codex's own reason; it often says what to do.
            let err = || {
                HarnessError::Protocol(match error["message"].as_str() {
                    Some(why) if !why.trim().is_empty() => format!(
                        "app-server rejected the request: {}",
                        why.chars().take(500).collect::<String>()
                    ),
                    _ => "app-server rejected the request".into(),
                })
            };
            if let Pending::Steer(message_ref, ..) | Pending::Turn(Some(message_ref), _) = &pending
            {
                self.emit(HarnessEvent::MessageDelivery {
                    message_ref: *message_ref,
                    state: DeliveryState::Failed,
                })?;
            }
            match pending {
                Pending::Steer(_, _, reply) | Pending::Turn(_, Some(reply)) => {
                    self.starting = false;
                    let _ = reply.send(Err(err()));
                }
                Pending::Interrupt(Some(reply)) => {
                    let _ = reply.send(Err(err()));
                }
                _ => return Err(err()),
            }
            return Ok(());
        }
        match pending {
            Pending::Initialize => {
                self.write(&json!({"method":"initialized"})).await?;
                self.request(
                    "account/read",
                    json!({"refreshToken":false}),
                    Pending::Account,
                )
                .await?;
            }
            Pending::Account => {
                if launch::relay(&self.spec).is_none() && result["account"]["type"] != "chatgpt" {
                    return Err(HarnessError::AccountNotReady(
                        "Sign in to Codex on this computer with ChatGPT. API keys aren’t used."
                            .into(),
                    ));
                }
                self.account_label = if launch::relay(&self.spec).is_none() {
                    result["account"]["email"].as_str().map(str::to_owned)
                } else {
                    None
                };
                let mut params = json!({"cwd":self.spec.cwd,"model":self.spec.model,"modelProvider":launch::provider(&self.spec),"approvalPolicy":"on-request","approvalsReviewer":"user"});
                if self.computer.is_some() {
                    // Nothing it is left can act unasked, and nothing asks.
                    params["sandbox"] = json!("read-only");
                    params["approvalPolicy"] = json!("never");
                } else if self.spec.mode != TaskMode::Work {
                    params["sandbox"] = json!("read-only");
                } else if leaves_read_only(&self.spec) {
                    params["sandbox"] = json!("workspace-write");
                }
                launch::thread(&self.spec, &mut params);
                if let Some(effort) = &self.spec.effort {
                    params["config"] = json!({"model_reasoning_effort":effort});
                }
                if self.native_instructions
                    && let Some(text) = run_instructions(&self.spec, self.asks)?
                {
                    params["developerInstructions"] = json!(text);
                }
                let method = if let Some(id) = &self.spec.resume_session {
                    params["threadId"] = json!(id);
                    // Nothing here reads the earlier turns, and Codex no
                    // longer wants to send a paginated thread's whole history.
                    params["excludeTurns"] = json!(true);
                    "thread/resume"
                } else {
                    "thread/start"
                };
                self.request(method, params, Pending::Thread).await?;
            }
            Pending::Thread => {
                let explicit_policy = launch::policy(&self.spec).is_some();
                if explicit_policy && !launch::confirms(&self.spec, &result) {
                    return Err(HarnessError::Unsupported(
                        "Codex didn’t confirm the selected launch flags, so no task turn was started."
                            .into(),
                    ));
                }
                if leaves_read_only(&self.spec)
                    && !explicit_policy
                    && (result["sandbox"]["type"] != "workspaceWrite"
                        || result["approvalPolicy"] != "on-request"
                        || result["approvalsReviewer"] != "user")
                {
                    return Err(HarnessError::Unsupported(
                        "Codex didn’t confirm Work permissions for this session, so nothing was sent."
                            .into(),
                    ));
                }
                if let Some(computer) = &self.computer {
                    if !modes::computer_response(&result) {
                        return Err(HarnessError::Unsupported(
                            "Codex didn’t confirm it would stay read-only for computer use, so nothing was sent.".into(),
                        ));
                    }
                    if !(result["model"].as_str()).is_some_and(|m| computer.models.contains(m)) {
                        return Err(HarnessError::Unsupported(
                            "Codex chose a model it doesn’t list on this computer, so computer use didn’t start. Choose one of its models.".into(),
                        ));
                    }
                } else if self.spec.mode == TaskMode::Work
                    && !explicit_policy
                    && !modes::asking_response(&result)
                {
                    return Err(HarnessError::Unsupported(
                        "Codex didn’t confirm it would ask you before acting outside its sandbox, so nothing was sent. On the computer, set Codex’s sandbox_mode to workspace-write or read-only.".into(),
                    ));
                }
                if self.spec.mode != TaskMode::Work && !modes::restricted_response(&result) {
                    return Err(HarnessError::Unsupported(
                        "Codex didn’t confirm it would stay read-only, so nothing was sent.".into(),
                    ));
                }
                let response: ThreadResponse = serde_json::from_value(result)
                    .map_err(|_| HarnessError::Protocol("invalid thread response".into()))?;
                if response.model_provider != launch::provider(&self.spec) {
                    return Err(HarnessError::AccountNotReady(
                        "Codex is set to a model provider other than OpenAI, which this app doesn’t use.".into(),
                    ));
                }
                self.thread = Some(response.thread.id.clone());
                self.emit(HarnessEvent::SessionStarted {
                    native_session: response.thread.id,
                    actual_model: Some(response.model),
                    actual_effort: response.reasoning_effort,
                    account_label: self.account_label.clone(),
                })?;
                let input = self.initial.take().ok_or_else(|| {
                    HarnessError::Protocol("initial input already dispatched".into())
                })?;
                if self.cancelled_before_start {
                    self.turn_exit = Some(RunExit::Interrupted);
                } else {
                    self.start_turn(None, input, None).await?;
                }
            }
            Pending::Turn(message_ref, reply) => {
                let response: TurnResponse = serde_json::from_value(result)
                    .map_err(|_| HarnessError::Protocol("invalid turn response".into()))?;
                self.starting = false;
                if response.turn.status == "inProgress"
                    && !self.finished_turns.contains(&response.turn.id)
                {
                    self.turn = Some(response.turn.id);
                }
                self.flush_interrupts().await?;
                if let Some(message_ref) = message_ref {
                    self.emit(HarnessEvent::MessageDelivery {
                        message_ref,
                        state: DeliveryState::NativeDelivered,
                    })?;
                }
                if let Some(reply) = reply {
                    let _ = reply.send(Ok(DeliveryState::NativeDelivered));
                }
            }
            Pending::Steer(message_ref, expected, reply) => {
                if result["turnId"].as_str() != Some(expected.as_str()) {
                    let _ = reply.send(Err(HarnessError::Protocol(
                        "invalid steer acknowledgement".into(),
                    )));
                } else {
                    self.emit(HarnessEvent::MessageDelivery {
                        message_ref,
                        state: DeliveryState::NativeDelivered,
                    })?;
                    let _ = reply.send(Ok(DeliveryState::NativeDelivered));
                }
            }
            Pending::Interrupt(reply) => {
                if let Some(reply) = reply {
                    let _ = reply.send(Ok(()));
                }
            }
        }
        Ok(())
    }
    async fn flush_interrupts(&mut self) -> Result<(), HarnessError> {
        for reply in std::mem::take(&mut self.deferred_interrupts) {
            self.interrupt(reply).await?;
        }
        Ok(())
    }
    async fn server_request(
        &mut self,
        id: Id,
        method: String,
        params: Value,
    ) -> Result<(), HarnessError> {
        if !matches!(
            method.as_str(),
            "item/commandExecution/requestApproval"
                | "item/fileChange/requestApproval"
                | "item/tool/requestUserInput"
                | "item/permissions/requestApproval"
                | "mcpServer/elicitation/request"
        ) {
            self.write(&json!({"id":id,"error":{"code":-32601,"message":"unsupported companion client request"}})).await?;
            self.emit(HarnessEvent::Warning {
                text: "Codex asked for something this app cannot provide, so it was declined"
                    .into(),
            })?;
            return Ok(());
        }
        if self.thread.as_deref() != params["threadId"].as_str() {
            return Err(HarnessError::Protocol(
                "approval belongs to a different native thread".into(),
            ));
        }
        if let Some(turn) = params["turnId"].as_str()
            && self.turn.as_deref() != Some(turn)
        {
            // A refusal Codex is told about but nobody sees would look,
            // from outside, like an agent that stopped for no reason.
            tracing::warn!(
                method,
                turn,
                active = self.turn.as_deref().unwrap_or("none"),
                "refused a Codex request for a turn that is no longer active"
            );
            self.write(&json!({"id":id,"error":{"code":-32602,"message":"approval turn is no longer active"}})).await?;
            return Ok(());
        }
        if self.approvals.len() >= 128 {
            return Err(HarnessError::Protocol(
                "too many pending native approvals".into(),
            ));
        }
        let planned = params["itemId"]
            .as_str()
            .and_then(|item| self.file_changes.get(item))
            .cloned()
            .unwrap_or_default();
        let approval = PendingApproval { id, method, params };
        let key = approval.id.key();
        if self.approvals.contains_key(&key) {
            return Err(HarnessError::Protocol(
                "duplicate pending approval id".into(),
            ));
        }
        if self.computer.is_some() {
            // The specialist has the bridge's tools and nothing to approve;
            // it asks the person through request_human.
            let result = approval.response(&ApprovalDecision::Deny { reason: None })?;
            self.write(&json!({"id":approval.id,"result":result}))
                .await?;
            self.emit(HarnessEvent::Warning {
                text: "Codex asked for something outside computer use, so it was declined".into(),
            })?;
            return Ok(());
        }
        if self.spec.mode != TaskMode::Work && approval.method != "item/tool/requestUserInput" {
            // Ask and Plan can't allow anything, so a request Deny is the only
            // answer to is declined at once rather than left waiting on the
            // person. Elicitation answers can authorize external effects, so
            // only genuine agent questions still reach them.
            let result = approval.response(&ApprovalDecision::Deny { reason: None })?;
            self.write(&json!({"id":approval.id,"result":result}))
                .await?;
            let asked = match approval.method.as_str() {
                "item/commandExecution/requestApproval" => "to run a command",
                "item/fileChange/requestApproval" => "to change files",
                "item/permissions/requestApproval" => "for more access",
                _ => "to use a tool",
            };
            self.emit(HarnessEvent::Warning {
                text: format!("This task is read-only, so Codex's request {asked} was declined"),
            })?;
            return Ok(());
        }
        let event = approval.event(&planned);
        self.emit(HarnessEvent::ApprovalRequested(event))?;
        self.approvals.insert(key, approval);
        Ok(())
    }
    async fn run(
        &mut self,
        mut normal: mpsc::Receiver<Normal>,
        mut urgent: mpsc::Receiver<Urgent>,
    ) -> Result<RunExit, HarnessError> {
        self.request("initialize",json!({"clientInfo":{"name":ports::PROGRAM_NAME,"title":ports::PRODUCT_NAME,"version":env!("CARGO_PKG_VERSION")},"capabilities":{"experimentalApi":true,"requestAttestation":false}}),Pending::Initialize).await?;
        let mut status: Option<ExitStatus> = None;
        let mut leader = io::Leader::new(&self.child)?;
        let mut eof = false;
        let mut shutting = false;
        let mut stopping = false;
        let stop_deadline = tokio::time::sleep(Duration::from_secs(86400));
        tokio::pin!(stop_deadline);
        let deadline = tokio::time::sleep(Duration::from_secs(86400));
        tokio::pin!(deadline);
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        loop {
            if eof && status.is_some() {
                break;
            }
            if self.turn_exit.is_some() && !shutting && status.is_none() {
                stopping = false;
                // After Stop closes admission, a held message is withdrawn, never started.
                if self.closed.load(Ordering::SeqCst) {
                    self.withdraw_queue()?;
                }
                if !self.starting
                    && let Some((message_ref, input)) = self.queue.pop_front()
                {
                    self.start_turn(Some(message_ref), input, None).await?;
                } else if self.queue.is_empty() {
                    shutting = true;
                    self.stdin.take();
                    deadline
                        .as_mut()
                        .reset(tokio::time::Instant::now() + SHUTDOWN_GRACE);
                }
            }
            // Fence admission before waiting for process exit. Buffered requests
            // have not reached native stdin and receive a definite rejection.
            if shutting || status.is_some() || eof {
                normal.close();
                while let Some(message) = normal.recv().await {
                    match message {
                        Normal::Send(_, _, _, reply) => {
                            let _ = reply.send(Err(HarnessError::NotActive));
                        }
                        Normal::Approval(_, _, reply) => {
                            let _ = reply.send(Err(HarnessError::NotActive));
                        }
                    }
                }
            }
            tokio::select! {
                biased;
                _ = &mut stop_deadline, if stopping => {
                    io::kill(&mut self.child);
                    // A forced Stop is Interrupted, not a claim that prior effects
                    // were undone or that the interrupted turn succeeded.
                    return Ok(self.turn_exit.clone().unwrap_or(RunExit::Interrupted));
                },
                _=&mut deadline,if shutting || status.is_some() || eof=> {
                    io::kill(&mut self.child);
                    return Ok(self.turn_exit.clone().unwrap_or_else(|| if stopping { RunExit::Interrupted } else {
                        RunExit::Unknown { reason:"app-server did not report a turn completion before shutdown".into() }
                    }));
                },
                message=urgent.recv(),if !shutting=>match message {
                    Some(Urgent::Interrupt(reply))=> {
                        self.withdraw_queue()?;
                        if !stopping {
                            stopping = true;
                            stop_deadline.as_mut().reset(tokio::time::Instant::now() + STOP_TIMEOUT);
                        }
                        self.interrupt(Some(reply)).await?;
                    },
                    Some(Urgent::Cancel(message_ref, reply)) => {
                        if let Some(index) = self.queue.iter().position(|(id, _)| *id == message_ref) {
                            self.queue.remove(index);
                            self.emit(HarnessEvent::MessageDelivery { message_ref, state: DeliveryState::Cancelled })?;
                            let _ = reply.send(Ok(()));
                        } else {
                            let _ = reply.send(Err(HarnessError::MessageNotQueued));
                        }
                    },
                    Some(Urgent::Terminate(reply)) => {
                        self.withdraw_queue()?;
                        // Only a confirmed kill ends the run; otherwise it stays open to try again.
                        match io::kill_owned(&mut self.child, &mut leader).await {
                            Ok(()) => { let _ = reply.send(Ok(())); return Ok(RunExit::Interrupted); },
                            Err(error) => { let _ = reply.send(Err(error)); },
                        }
                    },
                    Some(Urgent::Shutdown) | None => { shutting=true; self.queue.clear(); self.stdin.take(); deadline.as_mut().reset(tokio::time::Instant::now()+SHUTDOWN_GRACE); },
                },
                _=tick.tick(), if !shutting => { if self.pending.values().any(|(at,_)|at.elapsed()>Duration::from_secs(15)) { return Err(HarnessError::Protocol("native request acknowledgement timed out; outcome unknown".into())); } },
                result=leader.exited(&mut self.child),if status.is_none()=> { status=Some(result?); self.stdin.take(); if !shutting && !eof { deadline.as_mut().reset(tokio::time::Instant::now()+SHUTDOWN_GRACE); } },
                frame=self.frames.next(),if !eof=>match frame? {
                    None=> { eof=true; if !shutting && status.is_none() { deadline.as_mut().reset(tokio::time::Instant::now()+SHUTDOWN_GRACE); } },
                    Some(bytes)=> {
                        let envelope:Envelope=match serde_json::from_slice(&bytes) { Ok(v)=>v,Err(_)=> { self.emit(HarnessEvent::Warning { text:"Codex sent something this app could not read, so it was ignored".into() })?; continue; } };
                        match (envelope.method,envelope.id) {
                            (Some(method),Some(id))=>self.server_request(id,method,envelope.params).await?,
                            (Some(method),None)=> {
                                self.notification(&method,&envelope.params)?;
                                if method == "turn/started" && !shutting { self.flush_interrupts().await?; }
                            },
                            (None,Some(id))=>self.response(id,envelope.result,envelope.error).await?,
                            _=>self.emit(HarnessEvent::Warning { text:"Codex sent an update this app does not recognize, so it is not shown".into() })?,
                        }
                    }
                },
                Some(message)=normal.recv(),if !shutting && status.is_none()=>match message {
                    Normal::Approval(id,decision,reply)=> {
                        let Some(approval)=self.approvals.get(&id) else { let _=reply.send(Err(HarnessError::Protocol("approval is no longer pending".into()))); continue; };
                        if self.spec.mode != TaskMode::Work && !approval.read_only_decision(&decision) {
                            let _ = reply.send(Err(HarnessError::Unsupported("Ask and Plan cannot approve writes, sandbox escapes or external tool effects; start a Work task instead".into())));
                            continue;
                        }
                        let result=match approval.response(&decision) { Ok(v)=>v,Err(error)=> { let _=reply.send(Err(error)); continue; } };
                        let native_id=approval.id.clone();
                        self.write(&json!({"id":native_id,"result":result})).await?;
                        self.approvals.remove(&id); let _=reply.send(Ok(()));
                    },
                    Normal::Send(message_ref,input,delivery,reply)=> {
                        // After Stop nothing new may start a turn.
                        if self.thread.is_none() || stopping || self.closed.load(Ordering::SeqCst) { let _=reply.send(Err(HarnessError::NotActive)); continue; }
                        if self.turn.is_none() && !self.starting { self.start_turn(Some(message_ref),input,Some(reply)).await?; }
                        else if delivery==MessageDelivery::Steer && self.turn.is_some() {
                            self.request("turn/steer",json!({"threadId":self.thread,"expectedTurnId":self.turn,"input":input}),Pending::Steer(message_ref,self.turn.clone().ok_or(HarnessError::NotActive)?,reply)).await?;
                        } else {
                            if self.queue.len()>=32 { let _=reply.send(Err(HarnessError::Protocol("follow-up queue is full".into()))); continue; }
                            if delivery==MessageDelivery::InterruptAndSend { self.interrupt(None).await?; self.queue.push_front((message_ref,input)); } else { self.queue.push_back((message_ref,input)); }
                            let _=reply.send(Ok(DeliveryState::QueuedForBoundary));
                        }
                    },
                }
            }
        }
        let status = status
            .ok_or_else(|| HarnessError::Protocol("native exit status unavailable".into()))?;
        Ok(self.turn_exit.clone().unwrap_or_else(|| {
            if stopping {
                RunExit::Interrupted
            } else if !status.success() {
                RunExit::Failed {
                    reason: format!("Codex exited with {status}"),
                }
            } else {
                RunExit::Unknown {
                    reason: "app-server exited without a completed turn".into(),
                }
            }
        }))
    }
}
#[cfg(test)]
mod tests;
