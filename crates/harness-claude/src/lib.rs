// SPDX-License-Identifier: Apache-2.0
//! Native Claude Code streaming adapter. No inference is performed by probe.
mod background;
mod catalog;
mod delegation;
mod io;
mod model;
mod private;
mod results;
mod stream;
#[cfg(windows)]
#[allow(unsafe_code)]
mod windows;

use async_trait::async_trait;
use base64::Engine;
use ports::*;
use protocol::{
    capability::*,
    command::{ApprovalDecision, MessageDelivery},
    model::{DeliveryState, LaunchPolicy, TaskMode},
};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::{Path, PathBuf},
    process::ExitStatus,
    sync::Arc,
    time::Duration,
};
use tokio::{
    process::{Child, ChildStdin},
    sync::{mpsc, oneshot, watch},
};

#[derive(Debug, Clone, Default)]
pub struct ClaudeAdapter {
    binary: Option<PathBuf>,
    settings: Option<PathBuf>,
    copy: Option<PathBuf>,
}
impl ClaudeAdapter {
    pub fn new() -> Self {
        Self {
            settings: catalog::settings_path(),
            ..Self::default()
        }
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
        io::locate(&self.binary, "claude")
    }
}

/// The computer's own `claude` program, as a task would find it without a
/// path given or a copy of Øphio's.
pub fn installed() -> Option<PathBuf> {
    io::locate(&None, "claude")
}

/// Time allowed for EOF-driven native cleanup after a turn boundary.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(3);
/// A native interrupt must reach a result within this interval.
const STOP_TIMEOUT: Duration = Duration::from_secs(5);

const READ_ONLY_ENFORCEMENT: &str = "Claude Code can only read and search files. Additional tools and permission changes are disabled.";

async fn mode_support(binary: &std::path::Path) -> Vec<ModeSupport> {
    modes_in(io::inspect(binary, &["--help"]).await.as_ref())
}
/// The modes `claude --help`, as `help` printed it, shows the CLI can enforce.
fn modes_in(help: Option<&std::process::Output>) -> Vec<ModeSupport> {
    let help = help
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
        .unwrap_or_default();
    let permission = help
        .split("--permission-mode")
        .nth(1)
        .and_then(|text| text.split("\n  -").next())
        .unwrap_or_default();
    let read_only = permission
        .split(|c: char| !c.is_alphanumeric())
        .any(|word| word == "plan")
        && [
            "--tools",
            "--safe-mode",
            "--strict-mcp-config",
            "--mcp-config",
            "--permission-prompts",
            "--append-system-prompt",
            "--system-prompt-snapshot",
            "--disallowedTools",
        ]
        .iter()
        .all(|flag| help.contains(flag));
    [TaskMode::Ask, TaskMode::Plan, TaskMode::Work].into_iter().map(|mode| ModeSupport {
        mode,
        availability: if mode == TaskMode::Work || read_only { Availability::Available } else {
            Availability::Unsupported { reason: "This version of Claude Code cannot confirm that Ask and Plan will leave files unchanged. Update Claude Code on the computer to use these modes.".into() }
        },
        enforcement: if mode == TaskMode::Work {
            "Work uses the task’s saved native launch permissions. Review its launch flags before starting.".into()
        } else { READ_ONLY_ENFORCEMENT.into() },
    }).collect()
}

async fn account(binary: &std::path::Path, cwd: Option<&std::path::Path>) -> AccountReadiness {
    let Some(output) = io::inspect_in(binary, &["auth", "status", "--json"], cwd).await else {
        return AccountReadiness::Unknown {
            reason:
                "Run claude auth login on the computer; authentication status could not be checked"
                    .into(),
        };
    };
    let value: Value = serde_json::from_slice(&output.stdout).unwrap_or(Value::Null);
    if value["loggedIn"] == false {
        return AccountReadiness::SignedOut {
            remedy: "Run claude auth login on the computer".into(),
        };
    }
    if output.status.success() && value["loggedIn"] == true && value["authMethod"] == "claude.ai" {
        AccountReadiness::SignedIn {
            account_label: value["email"].as_str().map(str::to_owned),
            plan: value["subscriptionType"].as_str().map(str::to_owned),
        }
    } else {
        AccountReadiness::Unknown { reason:"A native Claude subscription login is required; API or unrecognized authentication is not used".into() }
    }
}

#[async_trait]
impl HarnessAdapter for ClaudeAdapter {
    fn kind(&self) -> HarnessKind {
        HarnessKind::ClaudeCode
    }
    async fn probe(&self) -> HarnessInfo {
        let binary = self.program();
        let version = if let Some(path) = &binary {
            io::inspect(path, &["--version"])
                .await
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        } else {
            None
        };
        let installed = if version.is_some() {
            Availability::Available
        } else {
            Availability::Unsupported {
                reason: "Install Claude Code on the computer or configure its executable path"
                    .into(),
            }
        };
        // The account, the help text and the model list are independent; the
        // modes and delegation are read from one `--help`, and so are the
        // models of a CLI that doesn't list them.
        let (account, help, listed) = if let Some(path) = &binary {
            tokio::join!(
                account(path, None),
                io::inspect(path, &["--help"]),
                catalog::listed(path)
            )
        } else {
            (
                AccountReadiness::Unknown {
                    reason: "Claude Code is not installed".into(),
                },
                None,
                None,
            )
        };
        let modes = if version.is_some() {
            modes_in(help.as_ref())
        } else {
            Vec::new()
        };
        let (models, catalog) = if binary.is_some() {
            let (models, observation) =
                catalog::probe(listed, help.as_ref(), self.settings.clone()).await;
            (models, Some(observation))
        } else {
            (Vec::new(), None)
        };
        let efforts = models
            .first()
            .and_then(|m| m.details.as_ref())
            .map(|d| d.efforts.iter().map(|e| e.id.clone()).collect())
            .unwrap_or_default();
        HarnessInfo { catalog, usage_limit: None, kind:self.kind(), display_name:"Claude Code".into(), native_version:version, integration:IntegrationLevel::Managed, installed: installed.clone(), account, modes, models, efforts, capabilities:HarnessCapabilities {
                cancel_queued_message: Some(installed.clone()),
            start_run:installed, steer_running:Availability::Unknown { reason:"Messages can be delivered, but Claude Code does not confirm whether they change the current turn.".into() }, interrupt:Availability::Available,
            native_approvals:Availability::Available, resume_session:Availability::Available, child_agents:Availability::Available,
            computer_use_delegation: if binary.is_some() { delegation::availability_in(help.as_ref()) } else { Availability::Unsupported { reason: "Claude Code is not installed".into() } }, model_selection:Availability::Available, effort_selection:Availability::Available,
        } }
    }
    async fn start_run(&self, spec: RunSpec) -> Result<RunStarted, HarnessError> {
        let binary = self
            .program()
            .ok_or_else(|| HarnessError::NotInstalled("Claude Code".into()))?;
        if spec.bridge.as_ref().is_some_and(|b| b.computer_use)
            && let Availability::Unsupported { reason } = delegation::availability(&binary).await
        {
            return Err(HarnessError::Unsupported(reason));
        }
        if spec.mode != TaskMode::Work {
            let support = mode_support(&binary).await;
            if let Some(ModeSupport {
                availability: Availability::Unsupported { reason },
                ..
            }) = support.iter().find(|s| s.mode == spec.mode)
            {
                return Err(HarnessError::Unsupported(reason.clone()));
            }
            if spec
                .bridge
                .as_ref()
                .is_some_and(|bridge| bridge.computer_use)
            {
                return Err(HarnessError::Unsupported(
                    "Computer use needs Work mode.".into(),
                ));
            }
        }
        if relay(&spec).is_none()
            && !matches!(
                account(&binary, Some(&spec.cwd)).await,
                AccountReadiness::SignedIn { .. }
            )
        {
            return Err(HarnessError::AccountNotReady(
                "Sign in to Claude Code on this computer with a Claude subscription.".into(),
            ));
        }
        if spec.mission.as_ref().is_some_and(|m| {
            m.bounds
                .iter()
                .any(|b| b.enforcement == protocol::model::Enforcement::Enforced)
        }) {
            return Err(HarnessError::Unsupported(
                "Claude Code can’t enforce boundaries; they can only be requested.".into(),
            ));
        }
        if spec.mode == TaskMode::Work
            && matches!(
                spec.launch_policy,
                Some(LaunchPolicy::BypassPermissions | LaunchPolicy::AcceptEdits)
            )
        {
            let help = io::inspect(&binary, &["--help"]).await;
            let help = help
                .as_ref()
                .filter(|h| h.status.success())
                .map(|h| String::from_utf8_lossy(&h.stdout))
                .unwrap_or_default();
            let supported = match spec.launch_policy {
                Some(LaunchPolicy::BypassPermissions) => {
                    help.contains("--dangerously-skip-permissions")
                }
                Some(LaunchPolicy::AcceptEdits) => help.contains("acceptEdits"),
                _ => true,
            };
            if !supported {
                return Err(HarnessError::Unsupported(
                    "Update Claude Code to use the selected launch flags. Nothing was sent.".into(),
                ));
            }
        }
        let mut launch = Launch::build(&spec)?;
        let mut first = spec.prompt.clone();
        if let Some(skill) = invoked_skill(&spec) {
            first.text = format!("/{} {}", skill, first.text);
        }
        let input = prompt(&first, &spec.cwd, &uuid::Uuid::new_v4().to_string()).await?;
        let visible: Vec<_> = launch
            .config
            .as_ref()
            .map(|file| file.path().to_owned())
            .into_iter()
            .chain(
                launch
                    .settings_file
                    .as_ref()
                    .map(|file| file.path().to_owned()),
            )
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
        // The computer's accounts first, so nothing they set can replace
        // what the launch itself needs.
        command
            .current_dir(&spec.cwd)
            .args(&launch.args)
            .envs(
                spec.env
                    .0
                    .iter()
                    .filter(|_| !wrapped.clear_env)
                    .map(|(k, v)| (k, v)),
            )
            .envs(launch.env.iter().copied());
        // A computer-use specialist's server is defined on the command line, so
        // it can only inherit its credentials; that run has no tool that runs
        // commands. Otherwise they reach the bridge alone, through its config.
        if let Some(bridge) = &spec.bridge
            && spec.mode == TaskMode::Work
            && bridge.computer_use
        {
            for (key, value) in &bridge.env {
                command.env(key, value);
            }
        }
        command.envs(wrapped.env);
        if let Some(endpoint) = relay(&spec) {
            command
                .env("ANTHROPIC_BASE_URL", endpoint.base_url())
                .env("ANTHROPIC_API_KEY", "ophio-relay-not-a-key")
                .env("CLAUDE_CODE_SIMPLE", "1")
                .env("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1")
                .env("DISABLE_TELEMETRY", "1")
                .env("DISABLE_ERROR_REPORTING", "1")
                .env("DISABLE_AUTOUPDATER", "1");
        }
        if model::exact(spec.model.as_deref()).is_some() {
            // Native consent can otherwise replace even an explicit model after
            // startup. Fail the request instead of accepting that substitution.
            command.env("CLAUDE_CODE_NO_MODEL_FALLBACK", "1");
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
        let (background, background_rx) = background::Background::new();
        let admission = Arc::new(private::Admission::default());
        let control = Arc::new(Control {
            normal: normal_tx,
            urgent: urgent_tx,
            done: done_rx,
            cwd: spec.cwd.clone(),
            process_id: child.id(),
            restricted: spec.bridge.as_ref().is_some_and(|b| b.computer_use),
            background: background_rx,
            admission: admission.clone(),
            container: container.clone(),
        });
        let guard = containment::Guard::new(container);
        tokio::spawn(async move {
            let _config = launch.config.take();
            let _settings = launch.settings_file.take();
            let mut actor = Actor {
                sandbox: spec.sandbox.clone(),
                child,
                stdin: Some(stdin),
                frames: io::Frames::new(stdout),
                events: events_tx,
                queue: VecDeque::new(),
                pending: HashMap::new(),
                busy: true,
                sequence: 1,
                state: stream::StreamState::new(spec.mode),
                exact_model: model::exact(spec.model.as_deref()).map(str::to_owned),
                sent_messages: HashMap::new(),
                interrupted: false,
                turn_exit: None,
                background,
                parent_ended: false,
                admission,
            };
            // Declared after the agent's handle, so a dropped task kills the
            // container before that handle lets the agent be reaped.
            let guard = guard;
            let exit = match actor.run(input, normal_rx, urgent_rx).await {
                Ok(exit) => exit,
                Err(error) => {
                    io::kill(&mut actor.child);
                    actor.turn_exit.clone().unwrap_or_else(|| RunExit::Failed {
                        reason: error.to_string(),
                    })
                }
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
            let exit = if matches!(exit, RunExit::Completed) && !actor.queue.is_empty() {
                RunExit::Failed { reason: "Claude Code closed before your follow-up messages reached it; send them again to continue".into() }
            } else {
                exit
            };
            let _ = done_tx.send(true);
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
            if let Some(sandbox) = &spec.sandbox
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

fn relay(spec: &RunSpec) -> Option<ports::sandbox::RelayEndpoint> {
    spec.sandbox.as_ref().and_then(|s| s.relay())
}

struct Launch {
    args: Vec<String>,
    env: Vec<(&'static str, &'static str)>,
    config: Option<tempfile::NamedTempFile>,
    settings_file: Option<tempfile::NamedTempFile>,
}
impl Launch {
    fn build(spec: &RunSpec) -> Result<Self, HarnessError> {
        if spec
            .launch_policy
            .is_some_and(|p| !p.supports(HarnessKind::ClaudeCode))
            || (spec.launch_policy.is_some()
                && spec.bridge.as_ref().is_some_and(|b| b.computer_use))
        {
            return Err(HarnessError::Unsupported(
                "These launch flags are not supported for this Claude task.".into(),
            ));
        }
        let bypass = spec.mode == TaskMode::Work
            && spec.launch_policy == Some(LaunchPolicy::BypassPermissions);
        // Plan mode refuses every MCP tool, even one allowed by name, so a
        // read-only turn answering on the checklist asks for permission as
        // usual instead: it has only reading tools and those two, and with
        // no prompts anything else is refused.
        let answering = spec.mode != TaskMode::Work
            && spec.bridge.as_ref().is_some_and(|b| b.checklist_answers);
        let mut args = vec![
            "--print",
            "--output-format",
            "stream-json",
            "--input-format",
            "stream-json",
            "--verbose",
            "--include-partial-messages",
            "--replay-user-messages",
            "--permission-mode",
            if answering {
                "manual"
            } else if spec.mode != TaskMode::Work {
                "plan"
            } else if bypass {
                "bypassPermissions"
            } else if spec.launch_policy == Some(LaunchPolicy::AcceptEdits) {
                "acceptEdits"
            } else {
                "manual"
            },
        ]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
        if let Some(endpoint) = relay(spec) {
            if endpoint.provider != ports::sandbox::RelayProvider::Anthropic
                || spec.bridge.is_some()
                || spec.resume_session.is_some()
            {
                return Err(HarnessError::Unsupported(
                    "Relay runs need the matching provider, fresh state and no host tool bridge."
                        .into(),
                ));
            }
            args.extend(["--bare".into(), "--setting-sources".into(), String::new()]);
        }
        if bypass {
            args.push("--dangerously-skip-permissions".into());
        }
        for (flag, value) in [
            ("--model", &spec.model),
            ("--effort", &spec.effort),
            ("--resume", &spec.resume_session),
        ] {
            if let Some(value) = value {
                if value.is_empty() {
                    return Err(HarnessError::Protocol("empty native option".into()));
                }
                args.extend([flag.into(), value.clone()]);
            }
        }
        let mut config = None;
        let mut env = Vec::new();
        if spec.mode == TaskMode::Work
            && !spec.bridge.as_ref().is_some_and(|b| b.computer_use)
            && !spec.project_settings
        {
            // With --print, Claude Code applies a project's committed settings,
            // hooks included, without asking whether the folder is trusted.
            // They wait for the owner's choice; the project folder's own
            // CLAUDE.md files still apply, read from it as an added directory.
            let cwd = spec
                .cwd
                .to_str()
                .ok_or_else(|| HarnessError::Protocol("project folder is not UTF-8".into()))?;
            args.extend([
                "--setting-sources".into(),
                "user".into(),
                "--add-dir".into(),
                cwd.into(),
            ]);
            env.push(("CLAUDE_CODE_ADDITIONAL_DIRECTORIES_CLAUDE_MD", "1"));
        }
        if spec.mode == TaskMode::Work
            && let Some(scratch) = &spec.scratch
        {
            // Files written to the scratch folder are inside the task's
            // folders, not outside them.
            let scratch = scratch
                .to_str()
                .ok_or_else(|| HarnessError::Protocol("scratch folder is not UTF-8".into()))?;
            args.extend(["--add-dir".into(), scratch.into()]);
        }
        if spec.sandbox.is_some() && spec.mode == TaskMode::Work {
            // Cloud connectors and hosted search do not run in our namespace.
            // Network access uses Bash/curl and the live CONNECT policy instead.
            args.extend([
                "--strict-mcp-config".into(),
                "--tools".into(),
                "Bash,Read,Write,Edit,Glob,Grep,TaskOutput,TaskStop".into(),
            ]);
            if spec.bridge.is_none() {
                let mut file = tempfile::Builder::new()
                    .prefix("ophio-mcp-")
                    .suffix(".json")
                    .tempfile()?;
                use std::io::Write;
                file.write_all(b"{\"mcpServers\":{}}")?;
                file.flush()?;
                args.extend([
                    "--mcp-config".into(),
                    file.path().to_string_lossy().into_owned(),
                ]);
                config = Some(file);
            }
        }
        if spec.mode != TaskMode::Work {
            // Plan alone is not an arbitrary-command sandbox. Remove every
            // executable/write/custom tool and disable settings/hooks/plugins.
            // A turn answering on the checklist gets the bridge, which
            // offers it only the checklist's read and reply tools. Safe mode
            // would turn the bridge off with every other server, so it reads
            // no settings files instead, which hold hooks and plugins.
            let answering = (spec.bridge.as_ref()).filter(|_| answering);
            let file = match answering {
                Some(bridge) => bridge_config(bridge)?,
                None => {
                    let mut file = tempfile::Builder::new()
                        .prefix("ophio-mcp-")
                        .suffix(".json")
                        .tempfile()?;
                    use std::io::Write;
                    file.write_all(b"{\"mcpServers\":{}}")?;
                    file.flush()?;
                    file
                }
            };
            args.extend([
                "--system-prompt-snapshot".into(),
                "off".into(),
                "--tools".into(),
                "Read,Glob,Grep".into(),
            ]);
            match answering {
                Some(_) => args.extend(["--setting-sources".into(), String::new()]),
                None => args.push("--safe-mode".into()),
            }
            args.extend([
                "--strict-mcp-config".into(),
                "--mcp-config".into(),
                file.path().to_string_lossy().into_owned(),
                "--permission-prompts".into(),
                "none".into(),
            ]);
            if spec.mode == TaskMode::Ask && answering.is_none() {
                args.extend(["--disallowedTools".into(), "ExitPlanMode".into()]);
            }
            if let Some(bridge) = answering {
                args.extend([
                    "--allowedTools".into(),
                    allowed_tools(bridge, ports::bridge::ANSWER_TOOLS),
                ]);
            }
            config = Some(file);
        } else if spec.bridge.as_ref().is_some_and(|b| b.computer_use) {
            args.extend(delegation::arguments(spec)?);
        } else if let Some(bridge) = &spec.bridge {
            let file = bridge_config(bridge)?;
            args.extend([
                "--mcp-config".into(),
                file.path().to_string_lossy().into_owned(),
            ]);
            // A turn answering on the checklist is never asked for
            // permission: its bridge has no prompt tool.
            if let Some(tool) =
                (bridge.permission_tool.as_ref()).filter(|_| !bridge.checklist_answers)
            {
                io::bridge_name(tool)?;
                let tool = format!("mcp__{}__{tool}", bridge.server_name);
                // Claude Code calls the prompt tool itself and resolves it
                // from every MCP tool, while a deny rule removes it from the
                // model's tools. A call from the model would only raise an
                // approval card that authorizes nothing.
                args.extend([
                    "--permission-prompt-tool".into(),
                    tool.clone(),
                    "--disallowedTools".into(),
                    tool,
                ]);
            }
            // The browser's tools keep their own rules: a login is filled
            // only where it belongs, and the owner is asked when the login
            // says so. The GitHub tools ask the owner where a post needs it.
            let mut allowed: Vec<&str> = ports::bridge::PERSON_TOOLS.to_vec();
            if bridge.browser {
                allowed.extend(ports::bridge::BROWSER_TOOLS);
                allowed.push(ports::bridge::HUMAN_TOOL);
            }
            if bridge.github {
                allowed.extend(ports::bridge::GITHUB_TOOLS);
                allowed.extend(ports::bridge::ASK_TOOLS);
            } else {
                allowed.extend(ports::bridge::WORK_TOOLS);
            }
            if bridge.checklist_answers {
                allowed = ports::bridge::ANSWER_TOOLS.to_vec();
            }
            args.extend(["--allowedTools".into(), allowed_tools(bridge, &allowed)]);
            config = Some(file);
        }
        let native = ports::harness::native_start(spec, HarnessKind::ClaudeCode);
        let loaded = ports::harness::start_loaded(spec, HarnessKind::ClaudeCode);
        match &spec.start {
            Some(ports::RunStart::Agent { .. }) if let Some(agent) = native => {
                args.extend(["--agent".into(), agent.name.clone()]);
            }
            Some(start @ ports::RunStart::Skill { .. }) if !loaded => {
                // Its instructions are read from its own folder.
                let folder = start
                    .skill_folder()
                    .and_then(|f| f.to_str())
                    .ok_or_else(|| HarnessError::Protocol("skill folder is not UTF-8".into()))?;
                args.extend(["--add-dir".into(), folder.into()]);
            }
            _ => {}
        }
        let started = (spec.start.as_ref())
            .and_then(|start| ports::harness::start_instruction(start, loaded));
        if let Some(model) = model::exact(spec.model.as_deref()) {
            // A persisted fallback chain must not change a pinned task's model.
            // Preserve the computer specialist's existing hook restrictions.
            args.extend(["--fallback-model".into(), model.into()]);
            merge_settings(&mut args, json!({"switchModelsOnFlag":false}))?;
        }
        let disabled = disabled_settings(&spec.disabled)?;
        if disabled != json!({}) {
            merge_settings(&mut args, disabled)?;
        }
        let mut instructions = String::new();
        if let Some(mission) = &spec.mission {
            let mut text = format!("Mission: {}", mission.goal);
            for bound in &mission.bounds {
                text.push_str(&format!("\nRequested constraint: {}", bound.text));
            }
            for check in &mission.completion_checks {
                text.push_str(&format!("\nCompletion check: {check}"));
            }
            if let Some(stop) = &mission.stop_point {
                text.push_str(&format!("\nStop point: {stop}"));
            }
            instructions = text;
        }
        let branch = spec.branch.as_ref().map(ports::harness::branch_instruction);
        let scratch = ports::harness::scratch_instruction(spec);
        for framing in [
            started.as_deref(),
            ports::harness::task_mode_instruction(spec.mode),
            branch.as_deref(),
            spec.github.as_deref(),
            ports::harness::person_instruction(spec),
            ports::harness::conduct_instruction(spec),
            scratch.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            if !instructions.is_empty() {
                instructions.push_str("\n\n");
            }
            instructions.push_str(framing);
        }
        if !instructions.is_empty() || spec.mission.is_some() {
            args.extend(["--append-system-prompt".into(), instructions]);
        }
        // Keep one merged settings flag at the end, regardless of its source.
        if let Some(index) = args.iter().position(|arg| arg == "--settings") {
            let settings = args.drain(index..index + 2).collect::<Vec<_>>();
            args.extend(settings);
        }
        // Merge inline sources first; the single final argument is a pathname,
        // not JSON (which can exceed the process argument size limit).
        let settings_file = if let Some(index) = args.iter().position(|arg| arg == "--settings") {
            let mut file = tempfile::Builder::new()
                .prefix("ophio-settings-")
                .suffix(".json")
                .tempfile()?;
            use std::io::Write;
            file.write_all(args[index + 1].as_bytes())?;
            file.flush()?;
            args[index + 1] = file.path().to_string_lossy().into_owned();
            Some(file)
        } else {
            None
        };
        if let Some(bridge) = &spec.bridge {
            io::no_token_in_arguments(&args, &bridge.env)?;
        }
        Ok(Self {
            args,
            env,
            config,
            settings_file,
        })
    }
}

/// Produce settings only for installed resources Claude Code can use.
fn disabled_settings(disabled: &ports::Disabled) -> Result<Value, HarnessError> {
    let mut settings = json!({});
    let mut skills = HashSet::new();
    for entry in &disabled.skills {
        if entry
            .harness
            .is_none_or(|kind| kind == HarnessKind::ClaudeCode)
            && skills.insert(&entry.name)
        {
            settings["skillOverrides"][&entry.name] = json!("off");
        }
    }
    let mut names = HashSet::new();
    let agents: Vec<String> = disabled
        .agents
        .iter()
        .filter(|entry| {
            entry
                .harness
                .is_none_or(|kind| kind == HarnessKind::ClaudeCode)
        })
        .filter(|entry| names.insert(&entry.name))
        .map(|entry| format!("Agent({})", entry.name))
        .collect();
    if !agents.is_empty() {
        settings["permissions"]["deny"] = json!(agents);
    }
    let mut paths = HashSet::new();
    let files: Vec<String> = disabled
        .files
        .iter()
        .filter(|path| path.exists())
        .map(|path| {
            path.to_str().map(escape_glob).ok_or_else(|| {
                HarnessError::Protocol("disabled instruction path is not UTF-8".into())
            })
        })
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .filter(|path| paths.insert(path.clone()))
        .collect();
    if !files.is_empty() {
        settings["claudeMdExcludes"] = json!(files);
    }
    if disabled.memory {
        settings["autoMemoryEnabled"] = json!(false);
    }
    if disabled.output_style {
        settings["outputStyle"] = json!("default");
    }
    Ok(settings)
}

// Claude Code normalizes backslashes to separators before picomatch; bracket
// classes quote metacharacters without relying on backslash escapes.
fn escape_glob(path: &str) -> String {
    let mut escaped = String::with_capacity(path.len());
    for ch in path.chars() {
        match ch {
            '[' => escaped.push_str("[[]"),
            ']' => escaped.push_str("[]]"),
            '{' => escaped.push_str("[{]"),
            '}' => escaped.push_str("[}]"),
            '*' => escaped.push_str("[*]"),
            '?' => escaped.push_str("[?]"),
            '(' => escaped.push_str("[(]"),
            ')' => escaped.push_str("[)]"),
            '!' => escaped.push_str("[!]"),
            '+' => escaped.push_str("[+]"),
            '@' => escaped.push_str("[@]"),
            _ => escaped.push(ch),
        }
    }
    escaped
}

/// Settings for an interactive Claude terminal persist for its entire session.
pub fn terminal_arguments(
    cwd: &Path,
    disabled: &ports::Disabled,
) -> Result<Vec<String>, HarnessError> {
    let _ = cwd;
    let settings = disabled_settings(disabled)?;
    if settings == json!({}) {
        return Ok(Vec::new());
    }
    use sha2::{Digest, Sha256};
    let content = settings.to_string();
    let hash = Sha256::digest(content.as_bytes());
    let suffix: String = hash[..8].iter().map(|byte| format!("{byte:02x}")).collect();
    let path = std::env::temp_dir().join(format!("ophio-disabled-{suffix}.json"));
    let same_content = std::fs::read(&path).ok().as_deref() == Some(content.as_bytes());
    #[cfg(unix)]
    let owner_only = {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(&path)
            .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o077 == 0)
    };
    #[cfg(not(unix))]
    let owner_only = path.is_file();
    if !same_content || !owner_only {
        let mut file = tempfile::Builder::new()
            .prefix("ophio-disabled-")
            .suffix(".json")
            .tempfile_in(std::env::temp_dir())?;
        use std::io::Write;
        file.write_all(content.as_bytes())?;
        file.flush()?;
        file.persist(&path)
            .map_err(|error| HarnessError::Io(error.error))?;
    }
    Ok(vec![
        "--settings".into(),
        path.to_string_lossy().into_owned(),
    ])
}

fn merge_settings(args: &mut Vec<String>, override_value: Value) -> Result<(), HarnessError> {
    if let Some(index) = args.iter().position(|arg| arg == "--settings") {
        let current = args.get(index + 1).ok_or_else(model::unverified)?;
        let mut settings: Value = serde_json::from_str(current).map_err(|_| model::unverified())?;
        merge_json(&mut settings, override_value);
        args[index + 1] = settings.to_string();
    } else {
        args.extend(["--settings".into(), override_value.to_string()]);
    }
    Ok(())
}

fn merge_json(target: &mut Value, source: Value) {
    match (target, source) {
        (Value::Object(target), Value::Object(source)) => {
            for (key, value) in source {
                merge_json(target.entry(key).or_insert(Value::Null), value);
            }
        }
        (Value::Array(target), Value::Array(source)) => target.extend(source),
        (target, source) => *target = source,
    }
}

/// The owner-only MCP config naming the bridge. Its credentials go to its
/// process alone, through this file, never to the agent and the commands
/// it runs.
fn bridge_config(bridge: &ports::ToolBridge) -> Result<tempfile::NamedTempFile, HarnessError> {
    io::bridge_name(&bridge.server_name)?;
    io::validate_env(&bridge.env)?;
    let mut bridge_args = bridge.args.clone();
    bridge_args.push(bridge.command.to_string_lossy().into_owned());
    bridge_args.push(bridge.server_name.clone());
    if let Some(tool) = &bridge.permission_tool {
        bridge_args.push(tool.clone());
    }
    io::no_token_in_arguments(&bridge_args, &bridge.env)?;
    let mut file = tempfile::Builder::new()
        .prefix("ophio-mcp-")
        .suffix(".json")
        .tempfile()?;
    let env: serde_json::Map<_, _> = bridge
        .env
        .iter()
        .map(|(key, value)| (key.clone(), Value::from(value.as_str())))
        .collect();
    let value = json!({"mcpServers":{&bridge.server_name:{"type":"stdio","command":bridge.command,"args":bridge.args,"env":env}}});
    use std::io::Write;
    file.write_all(value.to_string().as_bytes())?;
    file.flush()?;
    Ok(file)
}

/// `tools` of the bridge as `--allowedTools` names them.
fn allowed_tools(bridge: &ports::ToolBridge, tools: &[&str]) -> String {
    (tools.iter())
        .map(|tool| format!("mcp__{}__{tool}", bridge.server_name))
        .collect::<Vec<_>>()
        .join(",")
}

/// A skill Claude Code finds itself, invoked by the first prompt of a new
/// session. A resumed session is given its instructions instead.
fn invoked_skill(spec: &RunSpec) -> Option<&str> {
    match (&spec.start, &spec.resume_session) {
        (Some(ports::RunStart::Skill { .. }), None) => {
            ports::harness::native_start(spec, HarnessKind::ClaudeCode).map(|n| n.name.as_str())
        }
        _ => None,
    }
}

async fn prompt(
    input: &PromptInput,
    cwd: &std::path::Path,
    id: &str,
) -> Result<Value, HarnessError> {
    let mut content = vec![json!({"type":"text","text":input.text})];
    for image in &input.images {
        if !matches!(
            image.media_type.as_str(),
            "image/png" | "image/jpeg" | "image/gif" | "image/webp"
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
        let file = tokio::fs::File::open(path).await?;
        use tokio::io::AsyncReadExt;
        let mut bytes = Vec::new();
        file.take(20 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .await?;
        if bytes.len() > 20 * 1024 * 1024 {
            return Err(HarnessError::Unsupported("image exceeds 20 MiB".into()));
        }
        content.push(json!({"type":"image","source":{"type":"base64","media_type":image.media_type,"data":base64::engine::general_purpose::STANDARD.encode(bytes)}}));
    }
    let value = json!({"type":"user","uuid":id,"message":{"role":"user","content":content},"parent_tool_use_id":null,"session_id":""});
    // The message travels as one frame. Refuse it here, before it reaches the
    // running agent, whose stream would otherwise end on the oversized write.
    if serde_json::to_vec(&value).map_or(true, |bytes| bytes.len() >= io::MAX_MESSAGE) {
        return Err(HarnessError::Unsupported(
            "The images in this message are too large to send together. Send fewer or smaller images.".into(),
        ));
    }
    Ok(value)
}

type Reply<T> = oneshot::Sender<Result<T, HarnessError>>;
enum Normal {
    Send(MessageRef, Value, MessageDelivery, Reply<DeliveryState>),
}
enum Urgent {
    Interrupt(Reply<()>),
    Cancel(MessageRef, Reply<()>),
    Shutdown,
    Terminate(Reply<()>),
    StopChild(String, Reply<()>),
}
struct Control {
    normal: mpsc::Sender<Normal>,
    urgent: mpsc::Sender<Urgent>,
    done: watch::Receiver<bool>,
    cwd: PathBuf,
    process_id: Option<u32>,
    restricted: bool,
    background: watch::Receiver<background::Snapshot>,
    admission: Arc<private::Admission>,
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
    fn child_stop_availability(&self, native_ref: &str) -> Availability {
        self.child_availability(native_ref)
    }
    async fn stop_child(
        &self,
        native_ref: &str,
    ) -> Result<ports::harness::ChildStopResult, HarnessError> {
        self.stop_native_child(native_ref).await
    }
    async fn send_message(
        &self,
        message_ref: MessageRef,
        input: PromptInput,
        delivery: MessageDelivery,
    ) -> Result<DeliveryState, HarnessError> {
        // Resolve and validate image data outside the urgent-control actor.
        self.admission.check()?;
        let prepared = prompt(&input, &self.cwd, "pending").await?;
        self.admission.check()?;
        let (tx, rx) = oneshot::channel();
        self.normal
            .send(Normal::Send(message_ref, prepared, delivery, tx))
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
        self.admission.close();
    }
    async fn resolve_approval(&self, _: &str, _: &ApprovalDecision) -> Result<(), HarnessError> {
        Err(HarnessError::Unsupported(
            "Claude permissions are resolved through the bridge handler".into(),
        ))
    }
    async fn pause_for_private(&self) -> Result<(), HarnessError> {
        if !self.restricted {
            return Err(HarnessError::Unsupported(
                "Private pause requires the restricted computer specialist".into(),
            ));
        }
        self.admission.close();
        #[cfg(windows)]
        {
            return self.pause_windows().await;
        }
        #[cfg(not(windows))]
        {
            let tree = private::Tree::capture(self.process_id.ok_or(HarnessError::NotActive)?)?;
            // Interrupt closes admission and discards held follow-ups. Even a failed
            // interrupt is followed by shutdown; only independent exit evidence wins.
            let _ = self.interrupt().await;
            tokio::time::timeout(Duration::from_secs(8), self.shutdown())
                .await
                .map_err(|_| HarnessError::Protocol("Native session pause timed out".into()))??;
            tree.confirm()
        }
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

struct Actor {
    sandbox: Option<Arc<dyn ports::sandbox::Sandbox>>,
    exact_model: Option<String>,
    child: Child,
    stdin: Option<ChildStdin>,
    frames: io::Frames<tokio::process::ChildStdout>,
    events: mpsc::Sender<HarnessEvent>,
    queue: VecDeque<(MessageRef, Value)>,
    pending: HashMap<String, Option<Reply<()>>>,
    busy: bool,
    sequence: u64,
    state: stream::StreamState,
    sent_messages: HashMap<String, MessageRef>,
    interrupted: bool,
    turn_exit: Option<RunExit>,
    background: background::Background,
    parent_ended: bool,
    admission: Arc<private::Admission>,
}
impl Actor {
    async fn stop_background(&mut self, id: &str) -> Result<(), HarnessError> {
        self.background.request(id)?;
        let request_id = format!("stop-task-{}", self.sequence);
        self.sequence += 1;
        self.write(&json!({"type":"control_request","request_id":request_id,"request":{"subtype":"stop_task","task_id":id}})).await
    }

    fn emit(&self, event: HarnessEvent) -> Result<(), HarnessError> {
        if let HarnessEvent::SessionStarted { native_session, .. } = &event
            && let Some(sandbox) = &self.sandbox
        {
            sandbox.session_started(native_session)?;
        }
        self.events.try_send(event).map_err(|_| {
            HarnessError::Protocol("event receiver stalled or closed; run stopped".into())
        })
    }
    /// Stop withdraws every held follow-up; none is written after it.
    fn withdraw_queue(&mut self) -> Result<(), HarnessError> {
        for (message_ref, _) in std::mem::take(&mut self.queue) {
            self.sent_messages.retain(|_, id| *id != message_ref);
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
    async fn write_message(
        &mut self,
        message_ref: MessageRef,
        value: &Value,
    ) -> Result<(), HarnessError> {
        self.admission.check()?;
        let result = self.write(value).await;
        self.emit(HarnessEvent::MessageDelivery {
            message_ref,
            // A failed/partial write does not prove that the message stayed queued.
            state: if result.is_ok() {
                DeliveryState::HostAccepted
            } else {
                DeliveryState::Unknown
            },
        })?;
        result
    }
    async fn interrupt(&mut self, reply: Option<Reply<()>>) -> Result<(), HarnessError> {
        if self.pending.len() >= 128 {
            return Err(HarnessError::Protocol(
                "too many pending native interrupts".into(),
            ));
        }
        let id = format!("interrupt-{}", self.sequence);
        self.sequence += 1;
        self.write(
            &json!({"type":"control_request","request_id":id,"request":{"subtype":"interrupt"}}),
        )
        .await?;
        self.interrupted = true;
        self.pending.insert(id, reply);
        Ok(())
    }
    async fn run(
        &mut self,
        initial: Value,
        mut normal: mpsc::Receiver<Normal>,
        mut urgent: mpsc::Receiver<Urgent>,
    ) -> Result<RunExit, HarnessError> {
        if let Err(error) = self.admission.check() {
            // Stop was accepted before the prompt reached the agent.
            self.turn_exit = Some(RunExit::Interrupted);
            return Err(error);
        }
        let mut verification = self.exact_model.as_deref().map(model::Verification::new);
        let mut initial = Some(initial);
        if let Some(check) = &verification {
            self.write(&check.first()).await?;
        } else {
            self.write(&initial.take().ok_or_else(model::unverified)?)
                .await?;
        }
        let model_deadline = tokio::time::sleep(model::CHECK_TIMEOUT);
        tokio::pin!(model_deadline);
        let mut status: Option<ExitStatus> = None;
        let mut leader = io::Leader::new(&self.child)?;
        let mut eof = false;
        let mut shutting = false;
        let mut stopping = false;
        let stop_deadline = tokio::time::sleep(Duration::from_secs(86400));
        tokio::pin!(stop_deadline);
        let deadline = tokio::time::sleep(Duration::from_secs(86400));
        tokio::pin!(deadline);
        loop {
            if eof && status.is_some() {
                break;
            }
            // Closing admission and rejecting its buffered requests is atomic with
            // the boundary decision: no late message can be acknowledged then lost.
            if shutting || self.parent_ended || status.is_some() || eof {
                normal.close();
                while let Some(Normal::Send(_, _, _, reply)) = normal.recv().await {
                    let _ = reply.send(Err(HarnessError::NotActive));
                }
            }
            tokio::select! {
                biased;
                _ = &mut model_deadline, if initial.is_some() && !stopping && !shutting => {
                    return Err(model::unverified());
                },
                _ = &mut stop_deadline, if stopping => {
                    io::kill(&mut self.child);
                    return Ok(self.turn_exit.clone().unwrap_or(RunExit::Interrupted));
                },
                _ = &mut deadline, if shutting || status.is_some() || eof => {
                    io::kill(&mut self.child);
                    return Ok(self.turn_exit.clone().unwrap_or_else(|| if stopping {
                        // Interrupted means execution was stopped, not that earlier effects were undone.
                        RunExit::Interrupted
                    } else {
                        RunExit::Unknown { reason:"native process did not report a turn result before shutdown".into() }
                    }));
                },
                message = urgent.recv(), if !shutting => match message {
                    Some(Urgent::Interrupt(reply)) => {
                        normal.close();
                        self.withdraw_queue()?;
                        while let Ok(Normal::Send(_, _, _, reply)) = normal.try_recv() {
                            let _ = reply.send(Err(HarnessError::NotActive));
                        }
                        if initial.is_some() {
                            // Nothing has reached inference; do not wait for a
                            // native turn/interrupt acknowledgement that cannot exist.
                            match io::kill_owned(&mut self.child, &mut leader).await {
                                Ok(()) => { let _ = reply.send(Ok(())); return Ok(RunExit::Interrupted); },
                                Err(error) => { let _ = reply.send(Err(error)); continue; },
                            }
                        }
                        if !stopping {
                            stopping = true;
                            self.interrupted = true;
                            stop_deadline.as_mut().reset(tokio::time::Instant::now() + STOP_TIMEOUT);
                        }
                        if self.parent_ended {
                            let _ = reply.send(Ok(()));
                            stopping = false;
                        } else {
                            self.interrupt(Some(reply)).await?;
                        }
                        for id in self.background.live() { self.stop_background(&id).await?; }
                    },
                    Some(Urgent::Cancel(message_ref, reply)) => {
                        if let Some(index) = self.queue.iter().position(|(id, _)| *id == message_ref) {
                            self.queue.remove(index);
                            self.sent_messages.retain(|_, id| *id != message_ref);
                            self.emit(HarnessEvent::MessageDelivery { message_ref, state: DeliveryState::Cancelled })?;
                            let _ = reply.send(Ok(()));
                        } else {
                            let _ = reply.send(Err(HarnessError::MessageNotQueued));
                        }
                    },
                    Some(Urgent::StopChild(id, reply)) => {
                        let result = self.stop_background(&id).await;
                        let _ = reply.send(result);
                    },
                    Some(Urgent::Terminate(reply)) => {
                        normal.close();
                        while let Ok(Normal::Send(_, _, _, reply)) = normal.try_recv() {
                            let _ = reply.send(Err(HarnessError::NotActive));
                        }
                        self.withdraw_queue()?;
                        // Only a confirmed kill ends the run; otherwise it stays open to try again.
                        match io::kill_owned(&mut self.child, &mut leader).await {
                            Ok(()) => { let _ = reply.send(Ok(())); return Ok(RunExit::Interrupted); },
                            Err(error) => { let _ = reply.send(Err(error)); },
                        }
                    },
                    Some(Urgent::Shutdown) | None => { shutting=true; self.stdin.take(); self.queue.clear(); deadline.as_mut().reset(tokio::time::Instant::now()+SHUTDOWN_GRACE); }
                },
                result = leader.exited(&mut self.child), if status.is_none() => { status=Some(result?); self.stdin.take(); if !shutting && !eof { deadline.as_mut().reset(tokio::time::Instant::now()+SHUTDOWN_GRACE); } },
                frame = self.frames.next(), if !eof => match frame? {
                    None => { eof=true; if !shutting && status.is_none() { deadline.as_mut().reset(tokio::time::Instant::now()+SHUTDOWN_GRACE); } },
                    Some(bytes) => {
                        let value:Value = match serde_json::from_slice(&bytes) { Ok(v)=>v, Err(_)=> { self.emit(HarnessEvent::Warning { text:"invalid stream-json line ignored".into() })?; continue; } };
                        if value["type"] == "control_response" {
                            if initial.is_some() && !stopping && !shutting
                                && let Some(check) = &mut verification
                                && check.matches(&value) {
                                    if let Some(next) = check.accept(&value)? {
                                        self.write(&next).await?;
                                    } else {
                                        self.admission.check()?;
                                        self.write(&initial.take().ok_or_else(model::unverified)?).await?;
                                    }
                                    continue;
                            }
                            if let Some(id) = value["response"]["request_id"].as_str()
                                && let Some(reply) = self.pending.remove(id) {
                                    let success = value["response"]["subtype"] == "success";
                                    if let Some(reply) = reply { let _ = reply.send(if success { Ok(()) } else { Err(HarnessError::Protocol("native interrupt rejected".into())) }); }
                                }
                        } else if value["type"] == "control_request" {
                            // Unsupported host callbacks must fail, never silently approve.
                            if !shutting {
                                self.write(&json!({"type":"control_response","response":{"subtype":"error","request_id":value["request_id"],"error":"unsupported companion control callback"}})).await?;
                            }
                            if !self.state.expected_plan_callback(&value) {
                                self.emit(HarnessEvent::Warning { text:"The agent asked for something this app can't answer yet · refused".into() })?;
                            }
                        } else {
                            model::check_event(self.exact_model.as_deref(), &value)?;
                            // Resumed, Claude Code first reports background work the last
                            // process lost, in a turn of its own. That turn's result is not
                            // the reply to the message still in flight.
                            let notice = self.busy && value["type"] == "result" && value["origin"]["kind"] == "task-notification";
                            let completed = !notice && value["type"] == "result" && value["parent_tool_use_id"].as_str().is_none_or(str::is_empty);
                            if value["type"] == "user"
                                && value["message"]["content"].as_array().is_some_and(|a| a.iter().any(|v| v["type"] == "text"))
                                && let Some(message_ref) = value["uuid"].as_str().and_then(|id| self.sent_messages.remove(id)) {
                                self.emit(HarnessEvent::MessageDelivery { message_ref, state:DeliveryState::NativeDelivered })?;
                            }
                            let background_events = self.background.observe(&value)?;
                            self.background.publish();
                            for event in background_events { self.emit(event)?; }
                            if stopping && value["subtype"] == "background_tasks_changed" {
                                for id in self.background.unrequested() { self.stop_background(&id).await?; }
                            }
                            self.state.interrupted = self.interrupted;
                            if !notice { for event in self.state.parse(&value) { self.emit(event)?; } }
                            if self.parent_ended && self.background.live().is_empty() && !shutting {
                                shutting = true;
                                self.stdin.take();
                                deadline.as_mut().reset(tokio::time::Instant::now() + SHUTDOWN_GRACE);
                            }
                            if completed {
                                for event in self.state.end_turn_helpers(self.interrupted) { self.emit(event)?; }
                                self.busy = false;
                                stopping = false;
                                self.turn_exit = Some(stream::result_exit(&value, self.interrupted));
                                // After Stop closes admission, a held message is withdrawn, never written.
                                if self.admission.check().is_err() { self.withdraw_queue()?; }
                                if !shutting && status.is_none() && let Some((message_ref, input)) = self.queue.pop_front() {
                                    self.turn_exit = None;
                                    self.write_message(message_ref, &input).await?;
                                    self.busy = true;
                                    self.interrupted = false;
                                    stopping = false;
                                } else if !shutting && !self.background.live().is_empty() {
                                    self.parent_ended = true;
                                    self.emit(HarnessEvent::ParentEnded { exit: self.turn_exit.clone().ok_or(HarnessError::NotActive)? })?;
                                } else if !shutting {
                                    shutting = true;
                                    self.stdin.take();
                                    deadline.as_mut().reset(tokio::time::Instant::now() + SHUTDOWN_GRACE);
                                }
                            }
                        }
                    }
                },
                Some(Normal::Send(message_ref, input, delivery, reply)) = normal.recv(), if initial.is_none() && !shutting && !self.parent_ended && status.is_none() => {
                    if let Err(error) = self.admission.check() { let _ = reply.send(Err(error)); continue; }
                    if self.sent_messages.len() >= 128 { let _ = reply.send(Err(HarnessError::Protocol("too many unacknowledged user messages".into()))); continue; }
                    if self.busy && delivery != MessageDelivery::Steer && self.queue.len() >= 32 { let _ = reply.send(Err(HarnessError::Protocol("follow-up queue is full".into()))); continue; }
                    let mut value = input;
                    let message_id = uuid::Uuid::new_v4().to_string();
                    value["uuid"] = json!(message_id.clone());
                    self.sent_messages.insert(message_id, message_ref);
                    if self.busy && delivery != MessageDelivery::Steer {
                        if delivery == MessageDelivery::InterruptAndSend { self.interrupt(None).await?; self.queue.push_front((message_ref, value)); } else { self.queue.push_back((message_ref, value)); }
                        let _ = reply.send(Ok(DeliveryState::QueuedForBoundary));
                    } else {
                        self.turn_exit = None;
                        self.write_message(message_ref, &value).await?; self.busy=true; self.interrupted=false;
                        let _ = reply.send(Ok(DeliveryState::HostAccepted));
                    }
                }
            }
        }
        if initial.is_some() && !stopping && !shutting {
            return Err(model::unverified());
        }
        let status = status
            .ok_or_else(|| HarnessError::Protocol("native exit status unavailable".into()))?;
        Ok(self.turn_exit.clone().unwrap_or_else(|| {
            if stopping {
                RunExit::Interrupted
            } else if !status.success() {
                RunExit::Failed {
                    reason: format!("Claude Code exited with {status}"),
                }
            } else {
                RunExit::Unknown {
                    reason: "native process exited without a completed turn".into(),
                }
            }
        }))
    }
}

#[cfg(all(test, not(windows)))]
mod tests;
#[cfg(all(test, windows))]
use windows::tests;
