// SPDX-License-Identifier: Apache-2.0
//! Native application composition. All remote and owner transports share this Hub.
use crate::{
    Error, Result,
    config::{Config, Paths},
    identity::Identity,
    local::{OwnerRequest, OwnerResponse, OwnerRuntime},
};
use async_trait::async_trait;
use ports::{Clock, HarnessAdapter, LibraryFs};
use protocol::{ErrorCode, ProjectId, ProtocolError, auth::ChannelKind, capability::Availability};
use services::{CompanionHub, Hub};
use std::{collections::BTreeMap, path::PathBuf, sync::Arc};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

#[cfg(not(windows))]
pub const DESKTOP_REMEDY: &str =
    "Run ophio desktop enable on the computer to permit desktop capture and input";
#[cfg(windows)]
pub const DESKTOP_REMEDY: &str =
    "Run .\\ophio.exe desktop enable on the computer to permit desktop capture and input";
const DESKTOP_WITHDRAWN: &str =
    "Desktop sharing was not turned on: it was turned off before it was confirmed";

/// Explicit process inputs; tests use an empty environment and authored CLI binaries.
#[derive(Clone)]
pub struct ComposeOptions {
    pub environment: BTreeMap<String, String>,
    pub executable_dir: PathBuf,
    pub clock: Arc<dyn Clock>,
    pub claude_binary: Option<PathBuf>,
    pub codex_binary: Option<PathBuf>,
    pub desktop_factory: Option<Arc<dyn DesktopFactory>>,
    pub speech_factory: Arc<dyn crate::frontdoors::SpeechFactory>,
    /// Manage the computer's real accounts: its keyring, browser and sign-in
    /// tools. Tests leave it off so nothing touches the owner's own stores.
    pub accounts: bool,
}
/// What composition reads of the companion's own environment. The agents'
/// homes are among it: the agent tools inherit them, so the agents and
/// skills Ophio checks are the ones the tools load.
const PROCESS_ENVIRONMENT: &[&str] = &[
    "HOME",
    "USERPROFILE",
    "PATH",
    "XDG_SESSION_TYPE",
    "XDG_CURRENT_DESKTOP",
    "XDG_RUNTIME_DIR",
    "WAYLAND_DISPLAY",
    "DISPLAY",
    "DBUS_SESSION_BUS_ADDRESS",
    "XDG_SESSION_ID",
    "HYPRLAND_INSTANCE_SIGNATURE",
    "SWAYSOCK",
    "XDG_DATA_HOME",
    "XDG_DATA_DIRS",
    "APPDATA",
    "PROGRAMDATA",
    "CLAUDE_CONFIG_DIR",
    "CODEX_HOME",
    "OPHIO_INTAKE_URL",
];
impl ComposeOptions {
    pub fn from_process() -> Result<Self> {
        let environment = PROCESS_ENVIRONMENT
            .iter()
            .copied()
            .filter_map(|key| std::env::var(key).ok().map(|value| (key.into(), value)))
            .collect();
        let executable_dir = std::env::current_exe()?
            .parent()
            .ok_or_else(|| Error::Config("executable directory unavailable".into()))?
            .to_owned();
        Ok(Self {
            environment,
            executable_dir,
            clock: Arc::new(ports::SystemClock),
            claude_binary: None,
            codex_binary: None,
            desktop_factory: None,
            speech_factory: Arc::new(crate::frontdoors::LocalSpeech),
            accounts: true,
        })
    }
}
#[async_trait]
pub trait DesktopFactory: Send + Sync {
    async fn open(&self, paths: &Paths, options: &ComposeOptions) -> Result<DesktopSession>;
}
pub struct DesktopSession {
    pub backend: Arc<dyn ports::DesktopBackend>,
    #[cfg(target_os = "linux")]
    pub portal: Option<platform_linux::portal::PortalDriver>,
    #[cfg(windows)]
    pub windows: Option<platform_windows::desktop::WindowsDesktop>,
}
impl DesktopSession {
    async fn close(&self) -> Result<()> {
        #[cfg(windows)]
        if let Some(windows) = &self.windows {
            windows.close().await.map_err(desktop_error)?;
        }
        #[cfg(target_os = "linux")]
        if let Some(portal) = &self.portal {
            portal
                .close()
                .await
                .map_err(|_| Error::Config("Could not close desktop permission session".into()))?;
        }
        Ok(())
    }
}

pub struct Application {
    pub hub: Arc<dyn Hub>,
    pub owner: Arc<dyn OwnerRuntime>,
}
impl Application {
    pub async fn shutdown(&self) -> Result<()> {
        self.owner.shutdown().await
    }
}
pub struct ApplicationFactory {
    pub options: ComposeOptions,
}
#[async_trait]
impl crate::cli::HubFactory for ApplicationFactory {
    async fn build(
        &self,
        paths: &Paths,
        config: &Config,
        identity: &Identity,
    ) -> Result<Application> {
        let (build, _, _) = crate::reports::install_info(&self.options.executable_dir);
        crate::reports::install_panic_hook(paths.data.clone(), build);
        let runtime = Runtime::build(
            paths.clone(),
            config.clone(),
            identity,
            self.options.clone(),
        )
        .await?;
        Ok(Application {
            hub: runtime.hub.clone(),
            owner: runtime,
        })
    }
}

pub struct Runtime {
    pub hub: Arc<CompanionHub>,
    paths: Paths,
    options: ComposeOptions,
    config: Mutex<Config>,
    /// The settings last applied, for reads that must not wait behind a
    /// change still in progress.
    applied: tokio::sync::watch::Sender<Config>,
    desktop: Mutex<Option<DesktopSession>>,
    desktop_order: DesktopOrder,
    channels: Mutex<Option<ChannelRuntime>>,
    bridge_stop: CancellationToken,
    protection: Mutex<Option<crate::local_protection::ProtectionTask>>,
    failure: CancellationToken,
    bridge_task:
        Mutex<Option<tokio::task::JoinHandle<std::result::Result<(), computer_mcp::BridgeError>>>>,
    accounts: Option<accounts::ComputerAccounts>,
    agent_browser: Option<Arc<browser::agent::AgentBrowser>>,
    /// Checks each account's status again every [`ACCOUNT_CHECKS`].
    account_checks: std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
}
/// How often the computer checks that its accounts are still signed in.
const ACCOUNT_CHECKS: std::time::Duration = std::time::Duration::from_secs(15 * 60);
/// Orders desktop sharing changes. Turning sharing off withdraws any enable
/// still waiting for the person, so it never queues behind a dialog nobody
/// answers, and an enable that comes after a turn-off waits for it, even
/// when it reaches the settings first.
struct DesktopOrder {
    /// Withdraws the enables that came before the latest turn-off. Turning
    /// sharing off cancels it and puts a fresh one in its place.
    withdraw: std::sync::Mutex<CancellationToken>,
    /// Turn-offs not finished yet. Counted while `withdraw` is held, so an
    /// enable holding a fresh token always sees the turn-off that made it.
    disabling: tokio::sync::watch::Sender<usize>,
}
impl Default for DesktopOrder {
    fn default() -> Self {
        Self {
            withdraw: Default::default(),
            disabling: tokio::sync::watch::Sender::new(0),
        }
    }
}
/// A turn-off in progress; later enables wait until it is dropped.
struct Disabling<'a>(&'a DesktopOrder);
impl Drop for Disabling<'_> {
    fn drop(&mut self) {
        self.0.disabling.send_modify(|n| *n -= 1);
    }
}
impl DesktopOrder {
    /// Takes an enable's place: the token is cancelled by any later turn-off.
    fn enable(&self) -> CancellationToken {
        self.withdraw
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
    /// Takes a turn-off's place, withdrawing every earlier enable.
    fn disable(&self) -> Disabling<'_> {
        let mut withdraw = self
            .withdraw
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        std::mem::take(&mut *withdraw).cancel();
        self.disabling.send_modify(|n| *n += 1);
        Disabling(self)
    }
    /// The settings, once an enable may change them, or `None` when a
    /// turn-off withdrew it first.
    async fn settle<'a, T>(
        &self,
        settings: &'a Mutex<T>,
        withdrawn: &CancellationToken,
    ) -> Option<tokio::sync::MutexGuard<'a, T>> {
        let mut disabling = self.disabling.subscribe();
        loop {
            let guard = settings.lock().await;
            if withdrawn.is_cancelled() {
                return None;
            }
            if *disabling.borrow_and_update() == 0 {
                return Some(guard);
            }
            drop(guard);
            // The sender lives in `self`, which outlives this wait.
            let _ = disabling.wait_for(|n| *n == 0).await;
        }
    }
}
impl Runtime {
    pub async fn build(
        paths: Paths,
        mut config: Config,
        identity: &Identity,
        options: ComposeOptions,
    ) -> Result<Arc<Self>> {
        paths.initialize()?;
        config.validate(&paths)?;
        let home = options
            .environment
            .get("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| paths.data.join("instruction-home-unavailable"));
        let library_dir = paths.data.join("library");
        crate::identity::private_dir(&library_dir)?;
        let library = Arc::new(
            library::FsLibrary::new(
                library::LibraryConfig::new(library_dir, home),
                options.clock.clone(),
            )
            .map_err(|_| Error::Config("Cannot initialize Library storage".into()))?,
        );
        library
            .set_roots(config.root_specs())
            .await
            .map_err(|_| Error::Config("Cannot install Library roots".into()))?;
        // Øphio's own copies of the agents, while the owner keeps them up
        // to date; a program given by path is always used as given.
        let agents = paths.data.join("agents");
        let updater = Arc::new(crate::agent_updates::NpmUpdater::new(agents.clone()));
        let claude = Arc::new(options.claude_binary.as_ref().map_or_else(
            || {
                harness_claude::ClaudeAdapter::new()
                    .with_copy(agents.join(crate::agent_updates::Agent::CLAUDE.folder))
            },
            harness_claude::ClaudeAdapter::with_binary,
        ));
        let codex = options.codex_binary.as_ref().map_or_else(
            || {
                harness_codex::CodexAdapter::new()
                    .with_copy(agents.join(crate::agent_updates::Agent::CODEX.folder))
            },
            harness_codex::CodexAdapter::with_binary,
        );
        let named_path = options.environment.get("PATH").cloned().unwrap_or_default();
        let executable = bridge_executable(&options.executable_dir, cfg!(windows));
        let bridge_available = executable_is_usable(&executable);
        // A boot-specific socket avoids replacing another listener or trusting a stale socket.
        let (socket, socket_dir) = if bridge_available {
            prepare_bridge_endpoint(&paths.runtime)?
        } else {
            (
                bridge_endpoint(&paths.runtime, protocol::BootEpoch::new(), cfg!(windows)),
                None,
            )
        };
        let jev = Arc::new(
            ::jev::Jev::new(Arc::new(crate::jev::JevKeys::new(&paths)))
                .map_err(|_| Error::Config("Cannot initialize Jev".into()))?,
        );
        let (build, previous_build, update) = crate::reports::install_info(&options.executable_dir);
        let mut builder = CompanionHub::builder()
            .reports(services::reports::ReportsConfig {
                intake_url: options
                    .environment
                    .get("OPHIO_INTAKE_URL")
                    .cloned()
                    .or_else(|| config.intake_url.clone())
                    .filter(|u| !u.is_empty()),
                build,
                previous_build,
                data_dir: Some(paths.data.clone()),
                os: std::env::consts::OS.into(),
            })
            .sandboxes(Arc::new(sandbox::Bubblewrap::new(
                options.executable_dir.join("ophio-sandbox-shim"),
            )))
            .data_dir(paths.database())
            .host_id(identity.host_id)
            .host_name(
                hostname::get()
                    .ok()
                    .and_then(|h| h.into_string().ok())
                    .unwrap_or_else(|| "This computer".into()),
            )
            .clock(options.clock.clone())
            .agent_updater(updater)
            .app_updates({
                let dir = options.executable_dir.clone();
                tokio::task::spawn_blocking(move || crate::update::app_offers(&dir))
                    .await
                    .unwrap_or_default()
            })
            .retention(config.retention())
            .harness(Arc::new(ManagedClaude(claude)))
            .harness(Arc::new(codex))
            // An agent started in a terminal is kept from what is disabled
            // in Ophio as its tasks are.
            .terminal_block(ports::TerminalBlock(Arc::new(
                |kind, cwd, disabled| match kind {
                    protocol::capability::HarnessKind::ClaudeCode => {
                        harness_claude::terminal_arguments(cwd, disabled)
                    }
                    protocol::capability::HarnessKind::Codex => {
                        harness_codex::terminal_arguments(cwd, disabled)
                    }
                    _ => Ok(Vec::new()),
                },
            )))
            .harness(Arc::new(crate::named_targets::NamedTarget::new(
                protocol::capability::HarnessKind::Hermes,
                named_path.clone().into(),
            )))
            .harness(Arc::new(crate::named_targets::NamedTarget::new(
                protocol::capability::HarnessKind::OpenClaw,
                named_path.clone().into(),
            )))
            .harness(Arc::new(crate::named_targets::NamedTarget::new(
                protocol::capability::HarnessKind::Pi,
                named_path.into(),
            )))
            .library(library)
            .terminal(Arc::new(
                terminal::PtyHost::new(terminal::PtyConfig::default(), options.clock.clone())
                    .map_err(|_| Error::Config("Cannot initialize terminals".into()))?,
            ))
            .media(Arc::new(media::TileEncoder))
            .preview_checker(Arc::new(services::preview::HttpPreviewChecker::new()?))
            .desktop_unavailable(Availability::PermissionMissing {
                remedy: DESKTOP_REMEDY.into(),
            });
        if let Some(inbox) = config.resolved_inbox_directory(&paths)? {
            builder = builder.inbox_dir(inbox);
        }
        for private in [&paths.data, &paths.config, &paths.runtime] {
            builder = builder.private_dir(private);
        }
        if let Some(home) = options
            .environment
            .get("HOME")
            .or_else(|| options.environment.get("USERPROFILE"))
        {
            builder = builder.home_dir(home);
        }
        builder = builder.launchers(services::LauncherDirs {
            setup_skills: Some(paths.data.join("setup-skills")),
            ..launcher_dirs(&config, &options.environment)
        });
        if bridge_available {
            builder = builder.bridge(services::bridge::BridgeConfig::new(
                executable,
                socket.to_string_lossy(),
            ));
        }

        #[cfg(target_os = "linux")]
        {
            builder = builder.window_actions(Arc::new(
                platform_linux::windows::LinuxWindows::from_environment(
                    &options.environment,
                    options.clock.clone(),
                ),
            ));
        }
        #[cfg(windows)]
        {
            builder = builder.window_actions(Arc::new(
                platform_windows::windows::WindowsActions::from_environment(
                    &options.environment,
                    options.clock.clone(),
                ),
            ));
        }
        // Read only while someone has Control's System section open.
        #[cfg(target_os = "linux")]
        {
            builder = builder.system_monitor(Arc::new(platform_linux::system::LinuxSystem::new()));
        }
        #[cfg(windows)]
        {
            builder =
                builder.system_monitor(Arc::new(platform_windows::system::WindowsSystem::new()));
        }
        // A site served from a project's folder joins its previews by itself.
        // Linux only: Windows doesn't say which folder a program works in.
        #[cfg(target_os = "linux")]
        {
            builder = builder.local_servers(Arc::new(platform_linux::servers::LinuxServers::new()));
        }
        // Agents browse in a browser of their own, through the bridge's
        // tools. It starts when a task first needs it.
        let browser_executable =
            browser::discover().filter(|_| options.accounts && bridge_available);
        let agent_browser = browser_executable.as_ref().map(|executable| {
            Arc::new(browser::agent::AgentBrowser::new(
                executable.clone(),
                &paths.data,
            ))
        });
        if let Some(agent_browser) = &agent_browser {
            builder = builder.agent_browser(agent_browser.clone());
        }
        let accounts = if options.accounts {
            let config = accounts::Config::computer(&paths.data).await;
            // Agent logins keep their passwords in the accounts' vault, so
            // forgetting it forgets them too.
            let logins =
                accounts::logins::ComputerAgentLogins::new(accounts::logins::LoginsConfig::new(
                    config.dir.clone(),
                    config.vault.clone(),
                    browser_executable.as_deref().map(browser::name),
                ));
            let held = logins.clone();
            tokio::spawn(async move { held.hold_all().await });
            builder = builder.agent_logins(Arc::new(logins));
            let accounts = accounts::ComputerAccounts::new(config);
            builder = builder
                .accounts(Arc::new(accounts.clone()))
                .github(Arc::new(accounts::github::GithubApi::new(accounts.clone())));
            Some(accounts)
        } else {
            None
        };
        let hub = Arc::new(builder.build().await?);
        if let Some(update) = update {
            hub.record_report_update(update);
        }
        if let Some(pipeline) = config.pipeline.clone().filter(|p| p.enabled) {
            hub.start_report_fixes(*pipeline, crate::config::running_build());
        }
        if let Some(inbox) = config.report_inbox.clone() {
            hub.start_report_inbox(inbox);
        }
        // Again, now with the agents and one-offs folders that join them.
        hub.set_library_roots(config.root_specs()).await?;
        crate::voice::configure(&hub, paths.data.clone(), jev).await?;
        if let Some(home) = options
            .environment
            .get("HOME")
            .or_else(|| options.environment.get("USERPROFILE"))
        {
            hub.set_session_observer(Arc::new(crate::observed::NativeSessions::new(home.into())));
        }
        let speech =
            crate::frontdoors::open_speech(&paths, &config, options.speech_factory.clone()).await;
        let (engine, availability) = match speech {
            Ok(speech) => speech,
            Err(error) => {
                let _ = hub.shutdown().await;
                return Err(error);
            }
        };
        if let Err(error) = hub.set_speech(engine, availability).await {
            let _ = hub.shutdown().await;
            return Err(error.into());
        }
        let runtime = Arc::new(Self {
            hub,
            paths,
            options,
            applied: tokio::sync::watch::Sender::new(config.clone()),
            config: Mutex::new(config),
            desktop: Mutex::new(None),
            desktop_order: DesktopOrder::default(),
            channels: Mutex::new(None),
            bridge_stop: CancellationToken::new(),
            protection: Mutex::new(None),
            failure: CancellationToken::new(),
            bridge_task: Mutex::new(None),
            account_checks: std::sync::Mutex::new(
                accounts.as_ref().map(|a| a.spawn_checks(ACCOUNT_CHECKS)),
            ),
            accounts,
            agent_browser,
        });
        runtime
            .hub
            .set_chat_project_settings(Arc::new(ChatSettings(Arc::downgrade(&runtime))));
        if bridge_available {
            let (ready, bound) = tokio::sync::oneshot::channel();
            let stop = runtime.bridge_stop.clone();
            let handler = runtime.hub.clone();
            let failure = runtime.failure.clone();
            let task = tokio::spawn(async move {
                let _failure_on_exit = failure.drop_guard();
                // Keep the private fallback directory until the listener and its
                // connections have closed, including startup errors/cancellation.
                let _socket_dir = socket_dir;
                computer_mcp::serve::serve_bridge_ready(
                    socket,
                    handler,
                    stop.cancelled(),
                    Some(ready),
                )
                .await
            });
            if bound.await.is_err() {
                let cause = match task.await {
                    Ok(Err(error)) => error.to_string(),
                    _ => "listener stopped before binding".into(),
                };
                let _ = runtime.hub.shutdown().await;
                return Err(Error::Config(format!(
                    "Cannot bind the private tool bridge listener: {cause}"
                )));
            }
            *runtime.bridge_task.lock().await = Some(task);
        }
        *runtime.protection.lock().await = Some(
            crate::local_protection::start(runtime.hub.clone(), &runtime.options.environment).await,
        );
        let desktop_enabled = runtime.config.lock().await.desktop_enabled;
        if desktop_enabled && let Err(error) = runtime.apply_desktop(true).await {
            let _ = runtime.shutdown().await;
            return Err(error);
        }
        let channel_config = runtime.config.lock().await.clone();
        if let Err(error) = runtime.apply_channels(&channel_config).await {
            let _ = runtime.shutdown().await;
            return Err(error);
        }
        Ok(runtime)
    }
    /// Turning sharing off takes no part in any other setting, so one that
    /// has become invalid since, such as a Library folder that was removed,
    /// never keeps sharing on. Every step is tried even when one before it
    /// fails, and every failure is reported together.
    async fn turn_desktop_off(&self, config: &mut Config) -> Result<OwnerResponse> {
        let mut problems = Vec::new();
        let (fenced, closed) = self.close_desktop(&mut *self.desktop.lock().await).await;
        let held = fenced.as_ref().is_err_and(input_may_be_held);
        if let Err(error) = fenced {
            problems.push(unfenced_turn_off(&error));
        }
        match closed {
            Ok(()) => {
                // Consent is forgotten once the session has closed, even
                // when the phone could not be told.
                #[cfg(target_os = "linux")]
                forget_for_turn_off(&self.paths, &mut problems);
            }
            Err(error) => problems.push(format!(
                "the desktop's own sharing session could not be closed ({error})"
            )),
        }
        config.desktop_enabled = false;
        self.applied.send_replace(config.clone());
        if let Err(error) = config.save(&self.paths.config_file()) {
            problems.push(unsaved_turn_off(&error));
        }
        turned_off(problems, held)?;
        Ok(OwnerResponse::Settings {
            config: config.clone(),
            applied_live: true,
            desktop: self.hub.capabilities().await.capture,
        })
    }
    /// Withdraws the Hub's desktop access, then closes the native session.
    /// The session is closed even when the Hub could not record the change,
    /// since the Hub lets go of the desktop before it records anything. The
    /// two results stay apart so that consent can follow closure alone.
    async fn close_desktop(&self, active: &mut Option<DesktopSession>) -> (Result<()>, Result<()>) {
        let disabled = Availability::PermissionMissing {
            remedy: DESKTOP_REMEDY.into(),
        };
        let fenced = self
            .hub
            .set_desktop(None, disabled)
            .await
            .map_err(Error::from);
        // Keep the portal session retryable if close fails or is cancelled;
        // consent must not be forgotten before closure is acknowledged.
        #[cfg(target_os = "linux")]
        let closed = match active.as_ref() {
            Some(previous) => previous.close().await,
            None => Ok(()),
        }
        .map(|()| *active = None);
        #[cfg(not(target_os = "linux"))]
        let closed = match active.take() {
            Some(previous) => previous.close().await,
            None => Ok(()),
        };
        (fenced, closed)
    }
    /// Closes any sharing session and, when `enabled`, opens a new one. A
    /// session that could not be opened, because the person refused it or
    /// the desktop failed, is returned as `Ok(Some(reason))` once the phone
    /// has been told why; nothing is shared then.
    async fn apply_desktop(&self, enabled: bool) -> Result<Option<Error>> {
        let mut active = self.desktop.lock().await;
        let (fenced, closed) = self.close_desktop(&mut active).await;
        fenced.and(closed)?;
        if enabled {
            let session = match &self.options.desktop_factory {
                Some(factory) => factory.open(&self.paths, &self.options).await,
                None => native_desktop(&self.paths, &self.options).await,
            };
            match session {
                Ok(session) => {
                    let installed = self
                        .hub
                        .set_desktop(
                            Some(session.backend.clone()),
                            Availability::Unknown {
                                reason: "Desktop backend unavailable".into(),
                            },
                        )
                        .await;
                    if let Err(error) = installed {
                        session.close().await?;
                        return Err(error.into());
                    }
                    *active = Some(session);
                }
                Err(error) => {
                    self.hub.set_desktop(None, desktop_failure(&error)).await?;
                    return Ok(Some(error));
                }
            }
        }
        Ok(None)
    }
}
#[async_trait]
impl OwnerRuntime for Runtime {
    async fn request(&self, request: OwnerRequest) -> Result<OwnerResponse> {
        self.request_progress(
            request,
            tokio::sync::mpsc::channel(1).0,
            CancellationToken::new(),
        )
        .await
    }
    async fn request_progress(
        &self,
        request: OwnerRequest,
        progress: tokio::sync::mpsc::Sender<OwnerResponse>,
        cancel: CancellationToken,
    ) -> Result<OwnerResponse> {
        if let OwnerRequest::StopTask { task_id, force } = request {
            return Ok(OwnerResponse::TaskStop(
                self.hub
                    .submit(
                        &services::Caller::local_owner(),
                        protocol::command::CommandEnvelope::new(protocol::Command::StopTask {
                            task_id,
                            force,
                        }),
                    )
                    .await,
            ));
        }
        if let OwnerRequest::ProjectSettings { project_id, apply } = request {
            return Ok(OwnerResponse::ProjectSettings {
                project: self.hub.set_project_settings(project_id, apply)?,
                apply,
            });
        }
        if let OwnerRequest::Checklist {
            project_id,
            put,
            remove,
        } = request
        {
            let said = self.hub.owner_checklist(project_id, put, remove).await?;
            return Ok(OwnerResponse::Checklist(said));
        }
        if let OwnerRequest::AccountToken {
            account,
            run: Some(run),
        } = &request
        {
            // From inside a task: only what that task was given.
            let token = self.hub.renew_for_run(run.expose(), account).await?;
            return Ok(OwnerResponse::AccountToken(token));
        }
        if let OwnerRequest::AccountToken { account, run: None } = request {
            let Some(accounts) = &self.accounts else {
                return Err(Error::Ended(
                    "Accounts are not managed on this computer.".into(),
                ));
            };
            let token = accounts
                .token(&account)
                .await
                .map_err(|e| Error::Ended(e.to_string()))?;
            return Ok(OwnerResponse::AccountToken(token.as_str().to_owned()));
        }
        if matches!(request, OwnerRequest::Stop) {
            return Ok(OwnerResponse::ControlStopped {
                tasks_requested: self.hub.stop_all_control().await?,
            });
        }
        use crate::frontdoors::{ChannelCommand, VoiceCommand};
        // Reading settings never waits behind a change still being applied,
        // such as a desktop sharing dialog the person has not answered yet.
        if let OwnerRequest::Jev { operation } = request {
            let _guard = match operation {
                crate::jev::JevOperation::Status => None,
                _ => Some(self.config.lock().await),
            };
            return crate::jev::apply(&self.paths, &self.hub, operation).await;
        }
        match &request {
            OwnerRequest::FrontdoorStatus
            | OwnerRequest::Voice {
                action: VoiceCommand::Status,
            }
            | OwnerRequest::Channels {
                action: ChannelCommand::Status,
            } => {
                let config = self.applied.borrow().clone();
                return Ok(OwnerResponse::Frontdoors(
                    self.frontdoor_status(&config).await,
                ));
            }
            OwnerRequest::RootsList | OwnerRequest::Desktop { enabled: None } => {
                let config = self.applied.borrow().clone();
                return Ok(OwnerResponse::Settings {
                    config,
                    applied_live: true,
                    desktop: self.hub.capabilities().await.capture,
                });
            }
            _ => {}
        }
        let frontdoor = matches!(
            &request,
            OwnerRequest::Voice { .. }
                | OwnerRequest::Channels { .. }
                | OwnerRequest::TokenSet { .. }
                | OwnerRequest::TokenClear { .. }
                | OwnerRequest::FrontdoorStatus
        );
        let voice_changed = matches!(
            &request,
            OwnerRequest::Voice {
                action: VoiceCommand::Install { .. }
                    | VoiceCommand::Remove { .. }
                    | VoiceCommand::Use { .. }
            }
        );
        let channels_changed = matches!(&request, OwnerRequest::Channels { action } if !matches!(action, ChannelCommand::Status))
            || matches!(
                &request,
                OwnerRequest::TokenSet { .. } | OwnerRequest::TokenClear { .. }
            );
        let enabling = matches!(
            &request,
            OwnerRequest::Desktop {
                enabled: Some(true)
            }
        );
        let disabling = matches!(
            &request,
            OwnerRequest::Desktop {
                enabled: Some(false)
            }
        )
        .then(|| self.desktop_order.disable());
        let withdrawn = self.desktop_order.enable();
        let mut config = if enabling {
            self.desktop_order
                .settle(&self.config, &withdrawn)
                .await
                .ok_or_else(|| Error::Config(DESKTOP_WITHDRAWN.into()))?
        } else {
            self.config.lock().await
        };
        if disabling.is_some() {
            return self.turn_desktop_off(&mut config).await;
        }
        if let OwnerRequest::Channels {
            action: ChannelCommand::Map { project_id, .. },
        } = &request
        {
            let protocol::QueryData::Snapshot(snapshot) = self
                .hub
                .query(
                    &services::api::Caller::local_owner(),
                    protocol::Query::Snapshot {
                        reads_withheld: Some(true),
                    },
                )
                .await?
            else {
                return Err(Error::Closed);
            };
            if !snapshot
                .projects
                .iter()
                .any(|p| p.project_id == *project_id)
            {
                return Err(Error::Config(
                    "Project is not registered on this computer".into(),
                ));
            }
        }
        crate::frontdoors::apply_files(
            &self.paths,
            &request,
            self.options.speech_factory.as_ref(),
            progress,
            cancel.clone(),
        )
        .await?;
        let mut next = config.clone();
        let changed = next.apply(request, &self.paths)?;
        if changed || channels_changed {
            // Persist before acknowledgement. Failure never leaves an unrecorded grant live.
            if enabling {
                // Enabling closes any current session before the person is
                // asked, and sharing is recorded on only once they confirm,
                // so a stop or crash while the dialog is open restarts off.
                let mut unconfirmed = next.clone();
                unconfirmed.desktop_enabled = false;
                unconfirmed.save(&self.paths.config_file())?;
                self.applied.send_replace(unconfirmed);
            } else {
                next.save(&self.paths.config_file())?;
            }
            // Why sharing could not be opened, told to the phone again once
            // the failed change has been undone.
            let mut refused = None;
            let result = async {
                if frontdoor {
                    if voice_changed {
                        self.apply_speech(&next).await?;
                    }
                    if channels_changed {
                        self.apply_channels(&next).await?;
                    }
                } else {
                    if let Some(inbox) = next.resolved_inbox_directory(&self.paths)? {
                        self.hub.set_inbox_folder(&inbox).await?;
                    }
                    self.hub.set_library_roots(next.root_specs()).await?;
                    self.hub.set_retention(next.retention())?;
                }
                if enabling {
                    // The sharing dialog can wait on the person indefinitely.
                    // Stopping Ophio, or the command that asked going away,
                    // abandons the grant.
                    tokio::select! {
                        biased;
                        applied = self.apply_desktop(true) => if let Some(error) = applied? {
                            refused = Some(desktop_failure(&error));
                            return Err(error);
                        },
                        _ = cancel.cancelled() => return Err(Error::Config("Desktop sharing was not turned on: the request ended before it was confirmed".into())),
                        _ = withdrawn.cancelled() => return Err(Error::Config(DESKTOP_WITHDRAWN.into())),
                    }
                    next.save(&self.paths.config_file())?;
                }
                Ok::<_, Error>(())
            }
            .await;
            if let Err(error) = result {
                // Restore durable desired state; fail closed for desktop on any
                // partial failure, and record it closed so neither the status
                // nor the next start claims sharing is on. Live cleanup and
                // the published state do not depend on the save succeeding.
                if !frontdoor {
                    config.desktop_enabled = false;
                }
                let saved = config.save(&self.paths.config_file());
                // What undoing the change could not finish, said after why
                // it failed; input that may still be held is said first.
                let mut unreleased = None;
                let mut problems = Vec::new();
                if frontdoor {
                    if voice_changed {
                        let _ = self.apply_speech(&config).await;
                    }
                    if channels_changed {
                        let _ = self.apply_channels(&config).await;
                    }
                } else {
                    if let Ok(Some(inbox)) = config.resolved_inbox_directory(&self.paths) {
                        let _ = self.hub.set_inbox_folder(&inbox).await;
                    }
                    let _ = self.hub.set_library_roots(config.root_specs()).await;
                    let _ = self.hub.set_retention(config.retention());
                    let (fenced, closed) =
                        self.close_desktop(&mut *self.desktop.lock().await).await;
                    match fenced {
                        Err(
                            error @ Error::Protocol(ProtocolError {
                                code: ErrorCode::OutcomeUnknown,
                                ..
                            }),
                        ) => unreleased = Some(unfenced_turn_off(&error)),
                        Err(error) => problems.push(unfenced_turn_off(&error)),
                        Ok(()) => {}
                    }
                    if let Err(error) = closed {
                        problems.push(format!(
                            "the desktop's own sharing session could not be closed ({error})"
                        ));
                    }
                    if let Some(unavailable) = refused {
                        let _ = self.hub.set_desktop(None, unavailable).await;
                    }
                    self.applied.send_replace(config.clone());
                }
                if let Err(unsaved) = saved {
                    problems.push(format!(
                        "the previous settings could not be saved again: {unsaved}"
                    ));
                }
                if unreleased.is_none() && problems.is_empty() {
                    return Err(error);
                }
                problems.insert(
                    0,
                    match error {
                        Error::Config(message) => message,
                        other => other.to_string(),
                    },
                );
                return Err(match unreleased {
                    Some(released) => ProtocolError::new(
                        ErrorCode::OutcomeUnknown,
                        format!("{released}; {}", problems.join("; ")),
                    )
                    .into(),
                    None => Error::Config(problems.join("; ")),
                });
            }
            *config = next;
            self.applied.send_replace(config.clone());
        }
        if frontdoor {
            return Ok(OwnerResponse::Frontdoors(
                self.frontdoor_status(&config).await,
            ));
        }
        Ok(OwnerResponse::Settings {
            config: config.clone(),
            applied_live: true,
            desktop: self.hub.capabilities().await.capture,
        })
    }
    async fn shutdown(&self) -> Result<()> {
        let _config = self.config.lock().await;
        // Windows console close has a short OS deadline. Fence desktop access
        // and release native resources before waiting for channels or agents.
        #[cfg(windows)]
        let desktop_result = {
            let revoke = self
                .hub
                .set_desktop(
                    None,
                    Availability::TemporarilyUnavailable {
                        reason: "Øphio is shutting down on the computer".into(),
                    },
                )
                .await;
            let closed = if let Some(session) = self.desktop.lock().await.take() {
                session.close().await
            } else {
                Ok(())
            };
            revoke.map_err(Error::from).and(closed)
        };
        if let Some(channels) = self.channels.lock().await.take() {
            channels.shutdown().await;
        }
        self.stop_account_checks();
        self.bridge_stop.cancel();
        let bridge_result = if let Some(task) = self.bridge_task.lock().await.take() {
            match task.await {
                Ok(result) => {
                    result.map_err(|_| Error::Config("Tool bridge listener failed".into()))
                }
                Err(_) => Err(Error::Config("Tool bridge task failed".into())),
            }
        } else {
            Ok(())
        };
        let hub_result = self.hub.shutdown().await;
        if let Some(agent_browser) = &self.agent_browser {
            agent_browser.shut_down().await;
        }
        if let Some(protection) = self.protection.lock().await.take() {
            protection.close().await;
        }
        if let Some(session) = self.desktop.lock().await.take() {
            session.close().await?;
        }
        #[cfg(windows)]
        desktop_result?;
        hub_result?;
        bridge_result
    }
    async fn failed(&self) {
        self.failure.cancelled().await;
    }
}
impl Drop for Runtime {
    fn drop(&mut self) {
        self.bridge_stop.cancel();
        self.stop_account_checks();
    }
}
impl Runtime {
    fn stop_account_checks(&self) {
        if let Some(checks) = self
            .account_checks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            checks.abort();
        }
    }
}
fn executable_is_usable(path: &std::path::Path) -> bool {
    let Ok(meta) = path.metadata() else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.is_file() && meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        meta.is_file() && windows_executable_path(path)
    }
}
/// Where agents, skills and one-offs are on this computer. Ophio's own
/// agents folder is made now, so Library can hold it from the start.
fn launcher_dirs(
    config: &Config,
    environment: &BTreeMap<String, String>,
) -> services::LauncherDirs {
    let home = environment
        .get("HOME")
        .or_else(|| environment.get("USERPROFILE"))
        .map(PathBuf::from)
        .filter(|home| home.is_absolute());
    // A tool's own folder named relative to where the tool runs is a
    // different folder for every task, so it isn't known: none of its agents
    // or skills are offered, and the tool loads none of them itself.
    let own = |variable: &str, default: &str| match environment.get(variable) {
        Some(path) => Some(PathBuf::from(path)).filter(|path| path.is_absolute()),
        None => home.as_ref().map(|home| home.join(default)),
    };
    // Outside Øphio's own folders, which Library never reaches.
    let ophio = home.as_ref().map(|home| home.join("Ophio"));
    services::LauncherDirs {
        claude_home: own("CLAUDE_CONFIG_DIR", ".claude"),
        codex_home: own("CODEX_HOME", ".codex"),
        agents_home: home.as_ref().map(|home| home.join(".agents")),
        hermes_home: own("HERMES_HOME", ".hermes"),
        ophio_agents: ophio.as_ref().map(|ophio| ophio.join("Agents")),
        ophio_skills: ophio.as_ref().map(|ophio| ophio.join("Skills")),
        one_offs: config.one_offs_path.clone().or(ophio),
        // Inside Øphio's data, which the caller knows.
        setup_skills: None,
    }
}

fn bridge_executable(directory: &std::path::Path, windows: bool) -> PathBuf {
    directory.join(format!(
        "{}{}",
        computer_mcp::BINARY_NAME,
        if windows { ".exe" } else { "" }
    ))
}
fn bridge_endpoint(runtime: &std::path::Path, boot: protocol::BootEpoch, windows: bool) -> PathBuf {
    if windows {
        PathBuf::from(format!(
            r"\\.\pipe\{}-bridge-{boot}",
            crate::config::APP_NAME
        ))
    } else {
        runtime.join(format!("b{}", boot.to_string().replace('-', "")))
    }
}
fn prepare_bridge_endpoint(
    runtime: &std::path::Path,
) -> Result<(PathBuf, Option<tempfile::TempDir>)> {
    let boot = protocol::BootEpoch::new();
    let path = bridge_endpoint(runtime, boot, cfg!(windows));
    #[cfg(unix)]
    {
        use std::os::unix::{ffi::OsStrExt, fs::PermissionsExt};
        // sockaddr_un reserves one of its 108 bytes for the terminating NUL.
        if path.as_os_str().as_bytes().len() >= 108 {
            // Ignore TMPDIR: it can be as long as the runtime path. tempfile
            // creates this directory atomically with mode 0700.
            let private = tempfile::Builder::new()
                .prefix("cb-")
                .tempdir_in("/tmp")
                .map_err(|e| Error::Config(format!("Tool bridge socket path exceeds 107 bytes; cannot create a short private directory: {e}")))?;
            std::fs::set_permissions(private.path(), std::fs::Permissions::from_mode(0o700))?;
            let short = private.path().join("b");
            if short.as_os_str().as_bytes().len() >= 108 {
                return Err(Error::Config("Tool bridge socket path exceeds the 107-byte Unix socket limit, including the fallback path".into()));
            }
            return Ok((short, Some(private)));
        }
    }
    Ok((path, None))
}

#[cfg(any(not(unix), test))]
fn windows_executable_path(path: &std::path::Path) -> bool {
    path.extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("exe"))
}
#[derive(Debug)]
struct ManagedClaude(Arc<harness_claude::ClaudeAdapter>);
#[async_trait]
impl HarnessAdapter for ManagedClaude {
    fn kind(&self) -> protocol::capability::HarnessKind {
        self.0.kind()
    }
    async fn probe(&self) -> protocol::capability::HarnessInfo {
        self.0.probe().await
    }
    async fn start_run(
        &self,
        spec: ports::RunSpec,
    ) -> std::result::Result<ports::RunStarted, ports::HarnessError> {
        if let Some(missing) = missing_bridge(&spec) {
            return Err(missing);
        }
        self.0.start_run(spec).await
    }
}

/// Only Work routes permission prompts through the bridge. Ask and Plan run
/// with read-only tools and no prompts, so they start without it, and so does
/// a run that skips permission prompts, such as a conversation held to what
/// its access bar allows.
fn missing_bridge(spec: &ports::RunSpec) -> Option<ports::HarnessError> {
    (spec.mode == protocol::model::TaskMode::Work
        && spec.launch_policy != Some(protocol::model::LaunchPolicy::BypassPermissions)
        && spec.bridge.is_none())
    .then(|| {
        ports::HarnessError::Unsupported(
            "A task that can change things asks you before each step, and those questions reach your phone through computer-mcp, a helper that comes with Øphio. Øphio can’t find it on the computer, so nothing was sent. Update or reinstall Øphio on the computer, then try again.".into(),
        )
    })
}

struct ChannelRuntime {
    handle: channels::ChannelsHandle,
    stop: CancellationToken,
    monitor: tokio::task::JoinHandle<()>,
}
impl ChannelRuntime {
    async fn shutdown(self) {
        self.stop.cancel();
        self.handle.shutdown().await;
        let _ = self.monitor.await;
    }
}
impl Runtime {
    async fn apply_speech(&self, config: &Config) -> Result<()> {
        // Release the root's prior engine before loading its replacement. Existing
        // sessions retain their own engine and finish without interruption.
        self.hub
            .set_speech(
                None,
                Availability::Unknown {
                    reason: "Loading the selected local voice model".into(),
                },
            )
            .await?;
        let (engine, availability) = crate::frontdoors::open_speech(
            &self.paths,
            config,
            self.options.speech_factory.clone(),
        )
        .await?;
        self.hub.set_speech(engine, availability).await?;
        Ok(())
    }
    async fn apply_channels(&self, config: &Config) -> Result<()> {
        let mut active = self.channels.lock().await;
        if let Some(previous) = active.take() {
            previous.shutdown().await;
        }
        let stop = CancellationToken::new();
        let handle = spawn_channels(self.hub.clone(), &self.paths, config, stop.clone()).await?;
        for (kind, chats) in [
            (ChannelKind::Telegram, &config.channels.telegram),
            (ChannelKind::Discord, &config.channels.discord),
        ] {
            if let Err(error) = self.hub.set_chat_projects(kind, chats.projects.clone()) {
                handle.shutdown().await;
                return Err(error.into());
            }
        }
        let mut updates = handle.subscribe_status();
        let initial = updates.borrow_and_update().clone();
        for state in initial {
            if let Err(error) = self
                .hub
                .set_channel_availability(
                    state.channel,
                    crate::frontdoors::channel_availability(&state),
                )
                .await
            {
                handle.shutdown().await;
                return Err(error.into());
            }
        }
        let hub = self.hub.clone();
        let cancelled = stop.clone();
        let failure = self.failure.clone();
        let monitor = tokio::spawn(async move {
            loop {
                tokio::select! { biased; _ = cancelled.cancelled() => break, changed = updates.changed() => if changed.is_err() { break; } }
                let states = updates.borrow_and_update().clone();
                for state in states {
                    if hub
                        .set_channel_availability(
                            state.channel,
                            crate::frontdoors::channel_availability(&state),
                        )
                        .await
                        .is_err()
                    {
                        failure.cancel();
                        return;
                    }
                }
            }
        });
        *active = Some(ChannelRuntime {
            handle,
            stop,
            monitor,
        });
        Ok(())
    }
    async fn frontdoor_status(&self, config: &Config) -> crate::frontdoors::FrontdoorStatus {
        let channels = self
            .channels
            .lock()
            .await
            .as_ref()
            .map(|c| crate::frontdoors::decorate_status(c.handle.status()))
            .unwrap_or_default();
        crate::frontdoors::status(config, self.hub.capabilities().await.speech, channels, true)
    }
}
/// Lets the app choose where a linked chat works, through the same checks,
/// file and live restart as `ophio channels map` and `unmap`.
struct ChatSettings(std::sync::Weak<Runtime>);
#[async_trait]
impl services::api::ChatProjects for ChatSettings {
    async fn set(
        &self,
        channel: ChannelKind,
        chat: &str,
        project: Option<ProjectId>,
    ) -> std::result::Result<(), ProtocolError> {
        use crate::frontdoors::ChannelCommand;
        let runtime = self.0.upgrade().ok_or_else(|| {
            ProtocolError::new(ErrorCode::Expired, "Øphio is shutting down on the computer")
        })?;
        let (connector, chat_id) = (channel.into(), chat.to_owned());
        let action = match project {
            Some(project_id) => ChannelCommand::Map {
                connector,
                chat_id,
                project_id,
            },
            None => ChannelCommand::Unmap { connector, chat_id },
        };
        match runtime.request(OwnerRequest::Channels { action }).await {
            Ok(_) => Ok(()),
            Err(Error::Io(_)) => Err(ProtocolError::new(
                ErrorCode::Internal,
                "The chat settings could not be saved on this computer",
            )),
            Err(error) => Err(error.protocol()),
        }
    }
}
pub async fn spawn_channels(
    hub: Arc<dyn Hub>,
    paths: &Paths,
    config: &Config,
    shutdown: CancellationToken,
) -> Result<channels::ChannelsHandle> {
    let directory = paths.data.join("channels");
    crate::identity::private_dir(&directory)?;
    crate::identity::private_dir(&paths.data.join("secrets"))?;
    channels::spawn(
        hub,
        config.channels.clone(),
        Arc::new(crate::frontdoors::FileTokens::new(paths)),
        directory,
        shutdown,
    )
    .await
    .map_err(|e| Error::Config(e.to_string()))
}

/// What a turn-off says when Ophio could not finish letting go of the
/// desktop. Input that may still be held is said as such, since it matters
/// more than a phone that may not have heard.
pub(crate) fn unfenced_turn_off(error: &Error) -> String {
    if input_may_be_held(error) {
        format!("input to the desktop may not have been released ({error})")
    } else {
        format!("the phone may not have been told ({error})")
    }
}
/// Whether letting go of the desktop failed in a way that may leave its
/// input held.
fn input_may_be_held(error: &Error) -> bool {
    matches!(error, Error::Protocol(e) if e.code == ErrorCode::OutcomeUnknown)
}
/// What a turn-off says when its setting could not be saved.
pub(crate) fn unsaved_turn_off(error: &Error) -> String {
    format!(
        "the setting could not be saved, so it may turn on again when Øphio next starts ({error})"
    )
}
/// Forgets consent for a turn-off, saying so when it could not.
#[cfg(target_os = "linux")]
pub(crate) fn forget_for_turn_off(paths: &Paths, problems: &mut Vec<String>) {
    if let Err(error) = forget_desktop_consent(paths) {
        problems.push(format!(
            "its permission could not be forgotten, so it may resume without asking ({error})"
        ));
    }
}
/// Reports what a turn-off could not finish, once every step has been tried.
/// Input that may still be held keeps its unknown outcome, so it is never
/// mistaken for a setting that was merely refused.
pub(crate) fn turned_off(problems: Vec<String>, held: bool) -> Result<()> {
    if problems.is_empty() {
        return Ok(());
    }
    let message = format!(
        "Desktop sharing is off in Øphio, but {}. Turn it off again once this is fixed.",
        problems.join("; ")
    );
    Err(if held {
        ProtocolError::new(ErrorCode::OutcomeUnknown, message).into()
    } else {
        Error::Config(message)
    })
}

/// Called only for an explicit disable, after the portal session has closed
/// (or while the CLI holds the stopped companion's instance lock).
#[cfg(target_os = "linux")]
pub(crate) fn forget_desktop_consent(paths: &Paths) -> Result<()> {
    let directory = paths.data.join("desktop-consent");
    match std::fs::symlink_metadata(&directory) {
        Ok(metadata) if metadata.is_dir() => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        _ => {
            return Err(Error::Config(
                "Cannot access desktop consent storage".into(),
            ));
        }
    }
    match std::fs::remove_file(directory.join("desktop-restore-token")) {
        Ok(()) => std::fs::File::open(directory)?.sync_all()?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err(Error::Config("Could not forget desktop consent".into())),
    }
    Ok(())
}

#[cfg(target_os = "linux")]
async fn native_desktop(paths: &Paths, options: &ComposeOptions) -> Result<DesktopSession> {
    use platform_linux::{
        BackendKind, Environment, LinuxDesktop,
        native::{Driver, LogindLocker, SessionLocker},
        portal::{FileTokenStore, PortalDriver},
        wayland::WaylandDriver,
        x11::X11Driver,
    };
    let get = |name: &str| {
        options
            .environment
            .get(name)
            .filter(|v| !v.is_empty())
            .cloned()
    };
    let system = zbus::Connection::system().await.ok();
    // A user service the desktop started (GNOME's, for one) isn't told its
    // login session; logind names it when the user has only one desktop.
    let session_id = match (get("XDG_SESSION_ID"), &system) {
        (Some(id), _) => Some(id),
        (None, Some(system)) => {
            platform_linux::lock::display_session(
                system,
                rustix::process::getuid().as_raw(),
                &get("XDG_SESSION_TYPE").unwrap_or_default(),
            )
            .await
        }
        (None, None) => None,
    };
    let locker = session_id
        .clone()
        .and_then(|id| LogindLocker::new(id).ok())
        .map(|l| Arc::new(l) as Arc<dyn SessionLocker>);
    let mut env = Environment {
        session_type: get("XDG_SESSION_TYPE").unwrap_or_default(),
        current_desktop: get("XDG_CURRENT_DESKTOP").unwrap_or_default(),
        portal_available: get("DBUS_SESSION_BUS_ADDRESS").is_some(),
        x11_available: get("DISPLAY").is_some(),
        session_lock_available: locker.is_some(),
        ..Environment::default()
    };
    let mut wayland = None;
    if env.session_type == "wayland"
        && let Some(display) = get("WAYLAND_DISPLAY")
    {
        let path = PathBuf::from(display);
        let socket = if path.is_absolute() {
            Some(path)
        } else {
            get("XDG_RUNTIME_DIR")
                .map(|dir| PathBuf::from(dir).join(path))
                .filter(|p| p.is_absolute())
        };
        if let Some(socket) = socket {
            let driver = WaylandDriver::new(socket);
            if let Ok(globals) = driver.advertised_globals().await {
                env.wayland_globals = globals;
                wayland = Some(driver);
            }
        }
    }
    let detection = LinuxDesktop::detect(&env);
    let lock = linux_lock_observer(&options.environment, system, session_id.as_deref()).await;
    let mut portal = None;
    let driver: Arc<dyn Driver> = match detection.kind {
        BackendKind::Wlroots => Arc::new(wayland.ok_or_else(|| Error::Config("Wayland socket unavailable".into()))?),
        BackendKind::Portal => {
            let address = get("DBUS_SESSION_BUS_ADDRESS").ok_or_else(|| Error::Config("Session D-Bus address missing".into()))?;
            let connection = zbus::connection::Builder::address(address.as_str()).map_err(|_| Error::Config("Session D-Bus address invalid".into()))?.build().await.map_err(|_| Error::Config("Cannot connect to the explicit session D-Bus address".into()))?;
            let tokens = paths.data.join("desktop-consent");
            crate::identity::private_dir(&tokens)?;
            let mut driver = PortalDriver::request_consent(connection, Arc::new(FileTokenStore::new(tokens))).await.map_err(|_| Error::Config("Approve screen sharing and remote input in the desktop portal dialog".into()))?;
            if let Some(description) = wayland { driver = driver.with_wayland(description); }
            driver = driver.with_desktop_name(&env.current_desktop);
            portal = Some(driver.clone());
            Arc::new(driver)
        }
        BackendKind::X11 => Arc::new(X11Driver::connect(get("DISPLAY").ok_or_else(|| Error::Config("DISPLAY is missing".into()))?).await.map_err(|_| Error::Config("Cannot connect to the explicit X11 display".into()))?),
        BackendKind::Unsupported => return Err(Error::Config("No supported graphical session endpoints in Øphio's environment; start Øphio in a Wayland or X11 login session".into())),
    };
    let mut desktop = LinuxDesktop::new(driver, detection.capabilities, locker)
        .with_clock(options.clock.clone())
        .with_lock_observer(lock);
    if let Ok(unlocker) = platform_linux::unlock::PasswordUnlocker::new(
        session_id,
        get("WAYLAND_DISPLAY"),
        get("XDG_RUNTIME_DIR"),
    ) {
        desktop = desktop.with_unlocker(Arc::new(unlocker));
    }
    Ok(DesktopSession {
        backend: Arc::new(desktop),
        portal,
    })
}
#[cfg(windows)]
fn desktop_error(error: ports::PlatformError) -> Error {
    protocol::ProtocolError::new(error.code(), error.to_string()).into()
}
/// What the phone is told when sharing could not be opened.
#[cfg(windows)]
fn desktop_failure(error: &Error) -> Availability {
    windows_desktop_failure(error)
}
/// What the phone is told when sharing could not be opened.
#[cfg(not(windows))]
fn desktop_failure(error: &Error) -> Availability {
    Availability::PermissionMissing {
        remedy: format!(
            "{error}. Run ophio desktop enable from the computer's graphical login session after fixing it"
        ),
    }
}
#[cfg(any(windows, test))]
fn windows_desktop_failure(error: &Error) -> Availability {
    let reason = error.to_string();
    match error {
        Error::Protocol(error) => match error.code {
            protocol::ErrorCode::OsPermissionMissing => {
                Availability::PermissionMissing { remedy: reason }
            }
            protocol::ErrorCode::Unsupported => Availability::Unsupported { reason },
            protocol::ErrorCode::TemporarilyUnavailable => {
                Availability::TemporarilyUnavailable { reason }
            }
            _ => Availability::Unknown { reason },
        },
        _ => Availability::Unknown { reason },
    }
}
#[cfg(windows)]
async fn native_desktop(_paths: &Paths, options: &ComposeOptions) -> Result<DesktopSession> {
    let desktop = platform_windows::desktop::WindowsDesktop::new()
        .map_err(desktop_error)?
        .with_clock(options.clock.clone());
    Ok(DesktopSession {
        backend: Arc::new(desktop.clone()),
        windows: Some(desktop),
    })
}
#[cfg(not(any(target_os = "linux", windows)))]
async fn native_desktop(_paths: &Paths, _options: &ComposeOptions) -> Result<DesktopSession> {
    Err(Error::Unsupported(
        "desktop composition requires platform integration",
    ))
}

#[cfg(test)]
mod path_tests {
    use super::*;
    #[test]
    fn agents_are_found_in_the_homes_the_agent_tools_use() {
        // Claude Code and Codex inherit the companion's environment.
        let base = tempfile::tempdir().unwrap();
        let path = |name: &str| base.path().join(name).display().to_string();
        let environment: BTreeMap<String, String> = PROCESS_ENVIRONMENT
            .iter()
            .filter_map(|&key| {
                match key {
                    "HOME" => Some(path("home")),
                    "CLAUDE_CONFIG_DIR" => Some(path("claude")),
                    "CODEX_HOME" => Some(path("codex")),
                    _ => None,
                }
                .map(|value| (key.to_owned(), value))
            })
            .collect();
        let dirs = launcher_dirs(&Config::default(), &environment);
        assert_eq!(dirs.claude_home, Some(base.path().join("claude")));
        assert_eq!(dirs.codex_home, Some(base.path().join("codex")));
        assert_eq!(dirs.agents_home, Some(base.path().join("home/.agents")));
        // Not the default folder in place of one the tools find elsewhere.
        let mut relative = environment;
        relative.insert("CLAUDE_CONFIG_DIR".into(), "tool-home".into());
        relative.insert("CODEX_HOME".into(), "codex-home".into());
        let dirs = launcher_dirs(&Config::default(), &relative);
        assert_eq!((dirs.claude_home, dirs.codex_home), (None, None));
        relative.remove("CLAUDE_CONFIG_DIR");
        let dirs = launcher_dirs(&Config::default(), &relative);
        assert_eq!(dirs.claude_home, Some(base.path().join("home/.claude")));
    }
    #[test]
    fn a_turn_off_says_when_input_may_still_be_held() {
        let unreleased = Error::from(ProtocolError::new(
            ErrorCode::OutcomeUnknown,
            "not acknowledged",
        ));
        let unrecorded = Error::from(ProtocolError::new(
            ErrorCode::Internal,
            "database is locked",
        ));
        let held = turned_off(
            vec![unfenced_turn_off(&unreleased)],
            input_may_be_held(&unreleased),
        )
        .unwrap_err();
        assert_eq!(held.protocol().code, ErrorCode::OutcomeUnknown, "{held}");
        let said = held.to_string();
        assert!(
            said.contains("input to the desktop may not have been released"),
            "{said}"
        );
        assert!(!said.contains("may not have been told"), "{said}");
        let untold = turned_off(
            vec![unfenced_turn_off(&unrecorded)],
            input_may_be_held(&unrecorded),
        )
        .unwrap_err();
        assert_eq!(
            untold.protocol().code,
            ErrorCode::InvalidRequest,
            "{untold}"
        );
        assert!(
            untold
                .to_string()
                .contains("the phone may not have been told"),
            "{untold}"
        );
    }
    #[tokio::test]
    async fn an_enable_after_a_turn_off_waits_for_it_even_when_it_locks_first() {
        let (order, settings) = (DesktopOrder::default(), Mutex::new(()));
        let earlier = order.enable();
        let disabling = order.disable();
        // The turn-off is held up before it reaches the settings; a later
        // enable gets there first.
        let later = order.enable();
        let enabling = order.settle(&settings, &later);
        tokio::pin!(enabling);
        assert!(
            tokio::time::timeout(std::time::Duration::ZERO, enabling.as_mut())
                .await
                .is_err(),
            "an enable changed the settings before the turn-off it followed"
        );
        let turning_off = settings
            .try_lock()
            .expect("the turn-off waited behind the enable");
        drop(turning_off);
        drop(disabling);
        assert!(enabling.await.is_some(), "the later enable was withdrawn");
        assert!(
            earlier.is_cancelled(),
            "the earlier enable was not withdrawn"
        );
    }
    #[test]
    fn windows_setup_failures_preserve_their_capability_state() {
        let unavailable = |code| {
            windows_desktop_failure(&protocol::ProtocolError::new(code, "synthetic failure").into())
        };
        assert!(matches!(
            unavailable(protocol::ErrorCode::Unsupported),
            Availability::Unsupported { .. }
        ));
        assert!(matches!(
            unavailable(protocol::ErrorCode::OsPermissionMissing),
            Availability::PermissionMissing { .. }
        ));
        assert!(matches!(
            unavailable(protocol::ErrorCode::TemporarilyUnavailable),
            Availability::TemporarilyUnavailable { .. }
        ));
        assert!(matches!(
            unavailable(protocol::ErrorCode::NativeFailure),
            Availability::Unknown { .. }
        ));
    }
    #[test]
    fn packaged_bridge_uses_only_the_executable_directory_and_platform_suffix() {
        let dir = std::path::Path::new("fixture/bin with spaces");
        assert_eq!(bridge_executable(dir, true), dir.join("computer-mcp.exe"));
        assert_eq!(bridge_executable(dir, false), dir.join("computer-mcp"));
        for valid in [
            "computer-mcp.exe",
            "COMPuTER-MCP.EXE",
            "path with spaces/bridge.ExE",
        ] {
            assert!(windows_executable_path(std::path::Path::new(valid)));
        }
        for invalid in [
            "computer-mcp",
            "computer-mcp.exe.txt",
            "bridge.cmd",
            "bridge.exe ",
            "bridge.exe.",
            "exe",
        ] {
            assert!(!windows_executable_path(std::path::Path::new(invalid)));
        }
    }
    #[test]
    fn directories_and_missing_files_are_not_executables() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("computer-mcp.exe");
        assert!(!executable_is_usable(&path));
        std::fs::create_dir(&path).unwrap();
        assert!(!executable_is_usable(&path));
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn long_runtime_uses_a_private_short_socket_and_cleans_up() {
        use std::os::unix::{ffi::OsStrExt, fs::PermissionsExt};
        let runtime = tempfile::tempdir().unwrap();
        let long = runtime.path().join("long-runtime-".repeat(12));
        let (socket, private) = prepare_bridge_endpoint(&long).unwrap();
        assert!(socket.as_os_str().as_bytes().len() < 108);
        let private = private.unwrap();
        assert_eq!(
            private.path().metadata().unwrap().permissions().mode() & 0o777,
            0o700
        );
        let parent = private.path().to_owned();
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let connection = tokio::net::UnixStream::connect(&socket).await.unwrap();
        let accepted = listener.accept().await.unwrap();
        drop((connection, accepted, listener, private));
        assert!(!parent.exists());
        let (short, private) = prepare_bridge_endpoint(runtime.path()).unwrap();
        assert_eq!(short.parent(), Some(runtime.path()));
        assert!(private.is_none());
    }

    #[test]
    fn only_work_that_prompts_needs_the_permission_bridge() {
        let spec = |mode| ports::RunSpec {
            run_id: protocol::RunId::new(),
            task_id: protocol::TaskId::new(),
            mode,
            cwd: PathBuf::from("fixture/project"),
            prompt: ports::PromptInput {
                text: "List the files".into(),
                images: Vec::new(),
            },
            model: None,
            effort: None,
            launch_policy: None,
            resume_session: None,
            resume_from_mode: None,
            mission: None,
            branch: None,
            bridge: None,
            project_settings: false,
            env: Default::default(),
            github: None,
            scratch: None,
            questions: true,
            sandbox: None,
            web: false,
            disabled: Default::default(),
            start: None,
        };
        assert!(missing_bridge(&spec(protocol::model::TaskMode::Ask)).is_none());
        assert!(missing_bridge(&spec(protocol::model::TaskMode::Plan)).is_none());
        assert!(matches!(
            missing_bridge(&spec(protocol::model::TaskMode::Work)),
            Some(ports::HarnessError::Unsupported(_))
        ));
        // A conversation runs in Work, held by its access bar instead of
        // prompts, and starts without the bridge.
        let conversation = ports::RunSpec {
            launch_policy: Some(protocol::model::LaunchPolicy::BypassPermissions),
            ..spec(protocol::model::TaskMode::Work)
        };
        assert!(missing_bridge(&conversation).is_none());
    }

    #[test]
    fn bridge_endpoint_is_local_and_boot_specific_on_each_platform() {
        let dir = std::path::Path::new("fixture/runtime");
        let boot = protocol::BootEpoch::new();
        let windows = bridge_endpoint(dir, boot, true);
        assert_eq!(
            windows,
            PathBuf::from(format!(r"\\.\pipe\ophio-bridge-{boot}"))
        );
        assert!(!windows.to_string_lossy().contains("fixture"));
        assert_ne!(
            windows,
            bridge_endpoint(dir, protocol::BootEpoch::new(), true)
        );
        assert_eq!(
            bridge_endpoint(dir, boot, false),
            dir.join(format!("b{}", boot.to_string().replace('-', "")))
        );
    }
}

macro_rules! opaque_debug {
    ($($ty:ty),* $(,)?) => { $(impl std::fmt::Debug for $ty {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.debug_struct(stringify!($ty)).finish_non_exhaustive() }
    })* };
}
opaque_debug!(
    ComposeOptions,
    DesktopSession,
    Application,
    ApplicationFactory,
    Runtime
);

#[cfg(target_os = "linux")]
async fn linux_lock_observer(
    environment: &std::collections::BTreeMap<String, String>,
    system: Option<zbus::Connection>,
    session_id: Option<&str>,
) -> Arc<dyn platform_linux::native::LockObserver> {
    let session = if let Some(address) = environment.get("DBUS_SESSION_BUS_ADDRESS") {
        match zbus::connection::Builder::address(address.as_str()) {
            Ok(builder) => builder.build().await.ok(),
            Err(_) => None,
        }
    } else {
        None
    };
    let hyprland = environment
        .get("HYPRLAND_INSTANCE_SIGNATURE")
        .filter(|signature| {
            !signature.is_empty() && !signature.contains('/') && !signature.contains("..")
        })
        .and_then(|signature| {
            environment
                .get("XDG_RUNTIME_DIR")
                .map(|runtime| PathBuf::from(runtime).join("hypr").join(signature))
        });
    Arc::new(
        platform_linux::lock::SessionLockObserver::connect(session, system, session_id, hyprland)
            .await,
    )
}
