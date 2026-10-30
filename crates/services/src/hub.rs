// SPDX-License-Identifier: Apache-2.0
use crate::{api::*, error, internal, stored};
use async_trait::async_trait;
use protocol::{
    auth::*, capability::*, command::*, control::*, event::LiveEvent, model::*, time::Timestamp, *,
};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::sync::{Mutex as AsyncMutex, broadcast};

#[derive(Clone)]
pub struct CompanionHub {
    pub(crate) inner: Arc<Inner>,
}
impl std::fmt::Debug for CompanionHub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompanionHub")
            .field("host_id", &self.inner.host_id)
            .field("boot_epoch", &self.inner.epoch)
            .finish_non_exhaustive()
    }
}
/// A subscription, with its caller as last resolved against the store.
pub(crate) struct Subscriber {
    pub caller: Caller,
    pub sender: broadcast::Sender<Outbound>,
    /// This connection was opened from inside a run, even if its caller is the owner.
    agent: bool,
    /// `current_caller` of `caller` while `Inner::credentials` is unchanged.
    current: Option<(u64, Caller)>,
}
impl Subscriber {
    /// `current_caller`, resolved again only once a device or chat identity
    /// has changed. A failure is not kept, so the next commit retries it.
    fn current(&mut self, r: &store::Read<'_>, credentials: u64) -> Option<Caller> {
        if self
            .current
            .as_ref()
            .is_none_or(|(at, _)| *at != credentials)
        {
            self.current = CompanionHub::current_caller(r, &self.caller)
                .ok()
                .map(|current| (credentials, current));
        }
        self.current.as_ref().map(|(_, current)| current.clone())
    }
}
/// A run asking the owner about a login: to use it, or for a code.
pub(crate) struct LoginAsk {
    pub login_id: String,
    pub task_id: TaskId,
    pub run_id: RunId,
    /// The login's revision when the owner was asked.
    pub revision: u64,
    /// The code the owner typed, and who typed it.
    pub code: Option<(zeroize::Zeroizing<String>, String)>,
}
pub(crate) struct Inner {
    pub reports: crate::reports::Runtime,
    pub contributions: crate::contribution::Runtime,
    pub db: Mutex<store::Store>,
    pub subscribers: Mutex<Vec<Subscriber>>,
    /// Counts commits that changed a device or chat identity, the records
    /// `current_caller` reads. It changes only while `db` is held.
    pub credentials: AtomicU64,
    pub head: AtomicU64,
    pub host_id: HostId,
    pub epoch: BootEpoch,
    pub name: String,
    pub started: Timestamp,
    pub clock: Arc<dyn ports::Clock>,
    pub data: PathBuf,
    /// Ophio's own folders, the data directory first, canonical.
    pub private: Vec<PathBuf>,
    pub home: Option<PathBuf>,
    pub folder_browsers: Arc<tokio::sync::Semaphore>,
    pub inbox: Mutex<Option<PathBuf>>,
    pub harnesses: Vec<Arc<dyn ports::HarnessAdapter>>,
    pub sandboxes: Option<Arc<dyn ports::sandbox::Sandboxes>>,
    pub sandbox_runs: Mutex<HashMap<RunId, Arc<dyn ports::sandbox::Sandbox>>>,
    /// The sandboxed run that last showed each preview: its server is on
    /// that sandbox's loopback while the run lasts.
    pub preview_runs: Mutex<HashMap<PreviewId, RunId>>,
    pub access_changes: Mutex<HashMap<TaskId, std::sync::Weak<AsyncMutex<()>>>>,
    pub desktop: Mutex<Option<Arc<dyn ports::DesktopBackend>>>,
    pub desktop_absent: Mutex<Availability>,
    pub desktop_changed: tokio::sync::watch::Sender<u64>,
    pub runtime_change: AsyncMutex<()>,
    /// Wakes the expiry tick when a lease deadline or voice session starts.
    pub expiry: Arc<tokio::sync::Notify>,
    /// Wakes the guard over runs when one starts or a follow-up is stored.
    pub guard: Arc<tokio::sync::Notify>,
    /// Held while follow-ups are passed on, so none goes twice.
    pub followups: AsyncMutex<()>,
    /// One look by the guard at a time.
    pub guarding: AsyncMutex<()>,
    /// One step of Merge together at a time.
    pub together: AsyncMutex<()>,
    /// What each running turn was last seen doing, for the guard.
    pub liveness: Mutex<HashMap<RunId, crate::watchdog::Liveness>>,
    pub protection: AsyncMutex<crate::runtime::ProtectionSlot>,
    pub local_stops: std::sync::atomic::AtomicUsize,
    pub desktop_operation: tokio::sync::RwLock<()>,
    /// When recent tries to unlock the computer that didn't unlock it were
    /// made, read from the store on the first unlock.
    pub unlock_tries: AsyncMutex<Option<Vec<std::time::Instant>>>,
    /// Held from when an unlock's password is found right until the computer
    /// has released its lock, so desktop sharing can't end between the two.
    pub unlock_release: tokio::sync::RwLock<()>,
    pub active_media: Mutex<Vec<std::sync::Weak<dyn ports::EncodedStreamControl>>>,
    pub media: Option<Arc<dyn ports::MediaEncoder>>,
    pub library: Option<Arc<dyn ports::LibraryFs>>,
    pub terminal: Option<Arc<dyn ports::TerminalHost>>,
    pub speech: Mutex<crate::runtime::SpeechSlot>,
    pub preview: Option<Arc<dyn crate::preview::PreviewChecker>>,
    pub bridge: Option<crate::bridge::BridgeConfig>,
    pub runs: AsyncMutex<HashMap<RunId, Arc<dyn ports::RunControl>>>,
    pub message_locks: Mutex<HashMap<RunId, std::sync::Weak<AsyncMutex<()>>>>,
    pub controls: AsyncMutex<HashMap<TargetId, Arc<AsyncMutex<crate::control::Target>>>>,
    pub waits: AsyncMutex<HashMap<ApprovalId, tokio::sync::oneshot::Sender<ApprovalDecision>>>,
    pub transfer_locks: Mutex<HashMap<TransferId, std::sync::Weak<AsyncMutex<()>>>>,
    /// Held for reading while a blob is published, for writing while blobs are collected.
    pub blobs: Arc<tokio::sync::RwLock<()>>,
    /// Held for reading while a run's scratch folder is made, for writing
    /// while scratch folders are collected.
    pub scratch: tokio::sync::RwLock<()>,
    pub renewal: Duration,
    pub max_upload: u64,
    /// Bytes each volume has promised to publications still copying.
    pub copying: Mutex<HashMap<u64, u64>>,
    /// Uploads whose copy to their folder has claimed its room or been
    /// written, and so no longer hold room for a copy to come.
    pub copies: Mutex<std::collections::HashSet<TransferId>>,
    pub shutdown: tokio::sync::watch::Sender<bool>,
    pub jobs: Mutex<Vec<tokio::task::JoinHandle<()>>>,
    pub windows: Option<Arc<dyn ports::windows::WindowActions>>,
    pub accounts: Option<Arc<dyn ports::accounts::Accounts>>,
    pub agent_logins: Option<Arc<dyn ports::agent_logins::AgentLogins>>,
    /// Keeps Øphio's own copies of the agents' programs.
    pub agent_updater: Option<Arc<dyn ports::agent_copy::AgentUpdater>>,
    /// What Claude Code and Codex started in a terminal are given to keep
    /// what is disabled in Ophio from them.
    pub terminal_block: Option<ports::TerminalBlock>,
    /// The phone apps this install offers its devices, and their files.
    pub app_updates: Vec<(protocol::capability::AppOffer, std::path::PathBuf)>,
    /// Held through each bringing of the copies in line with the setting.
    pub agent_updates: AsyncMutex<()>,
    /// Held while an agent adds work the person asked for to Next up.
    pub planning: AsyncMutex<()>,
    /// Checks of the copies started and not yet finished.
    pub agent_checks: std::sync::atomic::AtomicUsize,
    pub agent_browser: Option<Arc<dyn ports::agent_browser::AgentBrowsing>>,
    pub github: Option<Arc<dyn ports::github::Github>>,
    pub system: Option<crate::system::SystemSampler>,
    /// Finds sites served in projects' folders, where the computer can tell.
    pub finder: Option<crate::found_previews::Finder>,
    /// The run each browsing task's tabs belong to.
    pub browser_runs: Mutex<HashMap<TaskId, RunId>>,
    /// Each browsing task's turns at the browser: a run's request holds one
    /// to read while it uses the browser, and the task's tabs change hands
    /// under one held to write.
    pub browser_turns: Mutex<HashMap<TaskId, Arc<tokio::sync::RwLock<()>>>>,
    /// Runs' requests about a login waiting on the owner, by approval.
    pub login_asks: Mutex<HashMap<ApprovalId, LoginAsk>>,
    pub launchers: crate::LauncherDirs,
    /// The Library roots last set, before automatic ones were added.
    pub configured_roots: Mutex<Option<Vec<ports::RootSpec>>>,
    /// Where agent files were last found, and the roots wanted for them.
    pub(crate) agent_files: Mutex<crate::agent_files::Found>,
    /// The keys and tokens each running task was given, masked in what it says.
    pub(crate) run_secrets: Mutex<HashMap<RunId, crate::accounts::RunSecrets>>,
    pub preview_forwards: crate::preview_forward::PreviewForwards,
    /// The terminal each preview's server was last started in by Ophio,
    /// held while one is started so an Open tapped twice starts it once.
    pub preview_servers: AsyncMutex<HashMap<PreviewId, TerminalId>>,
    /// The terminal each project's app was last started in for a review
    /// that uses it, and the command it was started with, held while one is
    /// started so two reviews start it once.
    pub(crate) app_terminals: AsyncMutex<HashMap<ProjectId, (TerminalId, String)>>,
    pub voice: crate::voice::VoiceRuntime,
    pub observed: crate::observed::Observer,
    pub chat_projects: Mutex<crate::identity::ChatProjectSlot>,
    pub pairing: Mutex<Option<crate::identity::WaitingPairing>>,
    _instance_lock: store::InstanceLock,
}

#[derive(Debug)]
pub struct HubBuilder {
    reports: crate::reports::ReportsConfig,
    data: Option<PathBuf>,
    private: Vec<PathBuf>,
    home: Option<PathBuf>,
    inbox: Option<PathBuf>,
    name: String,
    host_id: Option<HostId>,
    desktop_absent: Availability,
    clock: Arc<dyn ports::Clock>,
    harnesses: Vec<Arc<dyn ports::HarnessAdapter>>,
    sandboxes: Option<Arc<dyn ports::sandbox::Sandboxes>>,
    desktop: Option<Arc<dyn ports::DesktopBackend>>,
    media: Option<Arc<dyn ports::MediaEncoder>>,
    library: Option<Arc<dyn ports::LibraryFs>>,
    terminal: Option<Arc<dyn ports::TerminalHost>>,
    speech: Option<Arc<dyn ports::SpeechEngine>>,
    preview: Option<Arc<dyn crate::preview::PreviewChecker>>,
    bridge: Option<crate::bridge::BridgeConfig>,
    retention: store::Retention,
    renewal: Duration,
    max_upload: u64,
    windows: Option<Arc<dyn ports::windows::WindowActions>>,
    accounts: Option<Arc<dyn ports::accounts::Accounts>>,
    agent_logins: Option<Arc<dyn ports::agent_logins::AgentLogins>>,
    agent_updater: Option<Arc<dyn ports::agent_copy::AgentUpdater>>,
    terminal_block: Option<ports::TerminalBlock>,
    app_updates: Vec<(protocol::capability::AppOffer, std::path::PathBuf)>,
    agent_browser: Option<Arc<dyn ports::agent_browser::AgentBrowsing>>,
    github: Option<Arc<dyn ports::github::Github>>,
    system: Option<Arc<dyn ports::system::SystemMonitor>>,
    servers: Option<Arc<dyn ports::servers::LocalServers>>,
    launchers: crate::LauncherDirs,
}
impl Default for HubBuilder {
    fn default() -> Self {
        Self {
            reports: Default::default(),
            data: None,
            private: Vec::new(),
            home: None,
            inbox: None,
            host_id: None,
            desktop_absent: Availability::Unsupported {
                reason: "No desktop adapter is configured".into(),
            },
            name: "This computer".into(),
            clock: Arc::new(ports::SystemClock),
            harnesses: Vec::new(),
            sandboxes: None,
            desktop: None,
            media: None,
            library: None,
            terminal: None,
            speech: None,
            preview: None,
            bridge: None,
            retention: store::Retention::default(),
            renewal: Duration::from_secs(3),
            max_upload: 2 * 1024 * 1024 * 1024,
            windows: None,
            accounts: None,
            agent_logins: None,
            agent_updater: None,
            terminal_block: None,
            app_updates: Vec::new(),
            agent_browser: None,
            github: None,
            system: None,
            servers: None,
            launchers: crate::LauncherDirs::default(),
        }
    }
}
impl HubBuilder {
    pub fn reports(mut self, config: crate::reports::ReportsConfig) -> Self {
        self.reports = config;
        self
    }

    pub fn window_actions(mut self, value: Arc<dyn ports::windows::WindowActions>) -> Self {
        self.windows = Some(value);
        self
    }

    pub fn system_monitor(mut self, value: Arc<dyn ports::system::SystemMonitor>) -> Self {
        self.system = Some(value);
        self
    }

    /// Which programs serve on this computer from projects' folders, so
    /// their sites join the projects' previews by themselves.
    pub fn local_servers(mut self, value: Arc<dyn ports::servers::LocalServers>) -> Self {
        self.servers = Some(value);
        self
    }

    pub fn accounts(mut self, value: Arc<dyn ports::accounts::Accounts>) -> Self {
        self.accounts = Some(value);
        self
    }

    pub fn agent_logins(mut self, value: Arc<dyn ports::agent_logins::AgentLogins>) -> Self {
        self.agent_logins = Some(value);
        self
    }

    /// Keeps Øphio's own copies of the agents' programs up to date, while
    /// the owner has that on.
    pub fn agent_updater(mut self, value: Arc<dyn ports::agent_copy::AgentUpdater>) -> Self {
        self.agent_updater = Some(value);
        self
    }
    /// The launch arguments that keep what is disabled in Ophio from Claude
    /// Code and Codex started in a terminal.
    pub fn terminal_block(mut self, value: ports::TerminalBlock) -> Self {
        self.terminal_block = Some(value);
        self
    }

    /// The phone apps this install carries, which its devices may download
    /// to update themselves.
    pub fn app_updates(
        mut self,
        value: Vec<(protocol::capability::AppOffer, std::path::PathBuf)>,
    ) -> Self {
        self.app_updates = value;
        self
    }

    /// Ophio's agent browser, for coding tasks doing Work.
    pub fn agent_browser(mut self, value: Arc<dyn ports::agent_browser::AgentBrowsing>) -> Self {
        self.agent_browser = Some(value);
        self
    }
    /// GitHub's API as the account signed in under Accounts, for the GitHub
    /// workflow.
    pub fn github(mut self, value: Arc<dyn ports::github::Github>) -> Self {
        self.github = Some(value);
        self
    }
    /// Where agents, skills and one-offs are on this computer.
    pub fn launchers(mut self, value: crate::LauncherDirs) -> Self {
        self.launchers = value;
        self
    }

    pub fn data_dir(mut self, path: impl Into<PathBuf>) -> Self {
        self.data = Some(path.into());
        self
    }
    pub fn inbox_dir(mut self, path: impl Into<PathBuf>) -> Self {
        self.inbox = Some(path.into());
        self
    }
    /// Another folder of Ophio's own, beside the data directory, that no
    /// project may contain or sit inside.
    pub fn private_dir(mut self, path: impl Into<PathBuf>) -> Self {
        self.private.push(path.into());
        self
    }
    /// The person's home folder, which is never a project by itself.
    pub fn home_dir(mut self, path: impl Into<PathBuf>) -> Self {
        self.home = Some(path.into());
        self
    }
    /// Bind the store to the transport's persisted identity. A mismatch fails closed.
    pub fn host_id(mut self, value: HostId) -> Self {
        self.host_id = Some(value);
        self
    }
    pub fn desktop_unavailable(mut self, value: Availability) -> Self {
        self.desktop_absent = value;
        self
    }
    pub fn host_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }
    pub fn clock(mut self, value: Arc<dyn ports::Clock>) -> Self {
        self.clock = value;
        self
    }
    pub fn sandboxes(mut self, value: Arc<dyn ports::sandbox::Sandboxes>) -> Self {
        self.sandboxes = Some(value);
        self
    }
    pub fn harness(mut self, value: Arc<dyn ports::HarnessAdapter>) -> Self {
        self.harnesses.push(value);
        self
    }
    pub fn desktop(mut self, value: Arc<dyn ports::DesktopBackend>) -> Self {
        self.desktop = Some(value);
        self
    }
    pub fn media(mut self, value: Arc<dyn ports::MediaEncoder>) -> Self {
        self.media = Some(value);
        self
    }
    pub fn library(mut self, value: Arc<dyn ports::LibraryFs>) -> Self {
        self.library = Some(value);
        self
    }
    pub fn terminal(mut self, value: Arc<dyn ports::TerminalHost>) -> Self {
        self.terminal = Some(value);
        self
    }
    pub fn speech(mut self, value: Arc<dyn ports::SpeechEngine>) -> Self {
        self.speech = Some(value);
        self
    }
    pub fn preview_checker(mut self, value: Arc<dyn crate::preview::PreviewChecker>) -> Self {
        self.preview = Some(value);
        self
    }
    pub fn bridge(mut self, value: crate::bridge::BridgeConfig) -> Self {
        self.bridge = Some(value);
        self
    }
    pub fn retention(mut self, value: store::Retention) -> Self {
        self.retention = value;
        self
    }
    pub fn renewal_window(mut self, value: Duration) -> Self {
        self.renewal = value;
        self
    }
    pub fn max_upload_bytes(mut self, value: u64) -> Self {
        self.max_upload = value;
        self
    }
    pub async fn build(self) -> Result<CompanionHub, ProtocolError> {
        let data = self
            .data
            .ok_or_else(|| error(ErrorCode::InvalidRequest, "A data directory is required"))?;
        let inbox = self
            .inbox
            .as_ref()
            .map(|p| crate::inbox::outside_data(p, &data))
            .transpose()?;
        if self.renewal.is_zero() || self.renewal.as_millis() > u32::MAX as u128 {
            return Err(error(ErrorCode::InvalidRequest, "Invalid renewal window"));
        }
        std::fs::create_dir_all(&data).map_err(internal)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&data, std::fs::Permissions::from_mode(0o700))
                .map_err(internal)?;
        }
        let private = std::iter::once(&data)
            .chain(&self.private)
            .map(|p| ports::library::canonical(p).map_err(internal))
            .collect::<Result<Vec<_>, _>>()?;
        let home = self
            .home
            .map(|p| ports::library::canonical(&p).unwrap_or(p));
        let instance_lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(data.join("companion.lock"))
            .map_err(internal)?;
        let instance_lock = store::InstanceLock::acquire(instance_lock).map_err(|_| {
            error(
                ErrorCode::Conflict,
                "Another Øphio process is using this data directory",
            )
        })?;
        let mut db =
            store::Store::open(data.join("companion.sqlite"), self.retention).map_err(stored)?;
        let stored_id: Option<HostId> = db.read(|r| r.get("meta", "host_id")).map_err(stored)?;
        if let (Some(existing), Some(expected)) = (stored_id, self.host_id)
            && existing != expected
        {
            return Err(error(
                ErrorCode::Conflict,
                "Persisted hub host id differs from this computer's identity; restore the matching identity and store",
            ));
        }
        let host_id = stored_id.or(self.host_id).unwrap_or_default();
        let epoch = BootEpoch::new();
        let started = self.clock.now();
        let commit = db.write(started, |tx| {
            crate::contribution::recover(tx)?;
            tx.put("meta", "host_id", &host_id)?; tx.put("meta", "epoch", &epoch)?;
            tx.emit(Event::BootEpochChanged { boot_epoch: epoch })?;
            // No terminal outlives the process that opened it.
            for (id, _) in tx.read().entries::<protocol::auth::Principal>("terminal_owner")? {
                tx.delete("terminal_owner", id)?;
            }
            for mut run in tx.read().list::<Run>("run")? {
                let mut task: Task = tx.read().require("task", run.task_id)?;
                // Repair writer projections saved by versions that gave every
                // mode a slot. Helpers a finished Work turn kept the slot for
                // were never confirmed ended, so it stays until the person
                // reconciles the task. Never disturb a different run's
                // ownership.
                if crate::runs::run_mode(&run, &task) != TaskMode::Work {
                    if run.holds_writer {
                        run.holds_writer = false;
                        tx.put("run", run.run_id, &run)?;
                        tx.emit(Event::RunUpserted { run: Box::new(run.clone()) })?;
                    }
                    if let Some(mut project) = tx.read().get::<Project>("project", task.project_id)?
                        && project.active_writer == Some(run.run_id) {
                        project.active_writer = None;
                        tx.put("project", project.project_id, &project)?;
                        tx.emit(Event::ProjectUpserted { project })?;
                    }
                    if let Some(mut queue) = tx.read().get::<protocol::queue::QueueView>("queue", task.project_id)?
                        && queue.writer == Some(run.run_id) {
                        queue.writer = None;
                        crate::runs::bump_queue_revision(tx, &mut queue)?;
                        tx.put("queue", queue.project_id, &queue)?;
                        crate::runs::sync_queue_attention(tx, &queue, started)?;
                        tx.emit(Event::QueueChanged { queue })?;
                    }
                }
                crate::runs::cleanup_unconfirmed(tx, &mut run, &mut task, started)?;
                let recovered = domain::recover_run(run.state);
                if recovered != run.state {
                    run.state = recovered;
                    tx.put("run", run.run_id, &run)?;
                    tx.emit(Event::RunUpserted { run: Box::new(run.clone()) })?;
                    task.state = TaskState::ReconciliationRequired; task.updated_at = started; task.revision += 1;
                    tx.put("task", task.task_id, &task)?; tx.emit(Event::TaskUpserted { task })?;
                }
            }
            // Normalize projections stored by versions that counted review results.
            for task in tx.read().list::<Task>("task")? {
                crate::runs::recount_attention(tx, task.task_id)?;
            }
            for row in tx.read().commands()? {
                let mut work = match row {
                    store::CommandRead::Decoded(work) => work,
                    store::CommandRead::Unreadable { id, cause } => {
                        // Only UUIDs are safe to put in logs; malformed keys may
                        // contain arbitrary private data.
                        let command_id = id.parse::<CommandId>().map(|id| id.to_string()).unwrap_or_else(|_| "[invalid command id]".into());
                        tracing::error!(%command_id, error = %cause, "Stored command quarantined; outcome unknown; never redispatching");
                        tx.quarantine_command(&id)?;
                        continue;
                    }
                };
                if work.pending && matches!(work.envelope.body, protocol::command::Command::SendMessage { .. }) {
                    crate::runs::fail_undispatched_followup(tx, work.envelope.command_id, started)?;
                }
                if work.pending { work.pending = false; work.stage = CommandStage::ReconciliationRequired; tx.command(&work)?; tx.emit(Event::CommandProgress { result: None, error: None, command_id: work.envelope.command_id, stage: work.stage, detail: Some("Øphio restarted on the computer before the effect was settled; review required".into()) })?; }
            }
            // Backfill existing front-of-queue decisions on upgrade without admitting work.
            for queue in tx.read().list::<protocol::queue::QueueView>("queue")? {
                crate::runs::sync_queue_attention(tx, &queue, started)?;
            }
            for mut view in tx.read().list::<TargetControlView>("control")? {
                view.input_state = InputState::Unknown;
                if let Some(lease) = &mut view.lease { lease.state = LeaseState::Unknown; }
                if let Some(observation) = &mut view.model_observation { observation.state = ObservationState::Withdrawn; }
                if let Some(private) = &mut view.private_step { private.ready = false; private.input_stopped = CoverageFact::Unknown; }
                tx.put("control", view.target_id, &view)?; tx.emit(Event::ControlChanged { view })?;
            }
            for mut transfer in tx.read().list::<protocol::transfer::TransferRecord>("transfer")? {
                use protocol::transfer::{TransferState, TransferDestination, TransferPublication};
                if matches!(transfer.state, TransferState::Completed | TransferState::Cancelled) { continue; }
                let publishing = tx.read().get::<bool>("publication_started", transfer.transfer_id)?.unwrap_or(false);
                if publishing {
                    // Older records lack the exact collision-resolved candidate.
                    // Preserve the intended directory/name and disclose that limit.
                    let legacy = transfer.publication.is_none();
                    if legacy {
                        transfer.publication = match &transfer.destination {
                            Some(TransferDestination::Inbox) => Some(TransferPublication {
                                root: transfer.destination_folder.clone().unwrap_or_else(|| data.join("inbox").to_string_lossy().into_owned()), path: transfer.name.clone(),
                            }),
                            Some(TransferDestination::ProjectDir { project_id, dir }) => tx.read().get::<Project>("project", project_id)?.map(|p| TransferPublication {
                                root: p.host_path, path: std::path::Path::new(dir).join(&transfer.name).to_string_lossy().into_owned(),
                            }),
                            Some(TransferDestination::Artifact) => transfer.sha256.as_ref().filter(|sha| sha.len() == 64 && sha.is_ascii()).map(|sha| TransferPublication {
                                root: data.join("artifacts").to_string_lossy().into_owned(), path: format!("{}/{}", &sha[..2], sha),
                            }),
                            None => None,
                        };
                    }
                    transfer.state = TransferState::ReconciliationRequired { reason: if legacy {
                        "Publication was interrupted; inspect the destination directory, including filename collision suffixes (older record has no exact candidate), then cancel to clear".into()
                    } else {
                        "Publication was interrupted; inspect the recorded destination, then cancel to clear".into()
                    } };
                } else if matches!(transfer.state, TransferState::Pending | TransferState::InProgress | TransferState::Verifying) {
                    transfer.state = TransferState::Paused;
                } else { continue; }
                transfer.updated_at = started;
                tx.put("transfer", transfer.transfer_id, &transfer)?;
                tx.emit(Event::TransferUpserted { transfer })?;
            }
            // One already given back to its agent stays open: answering it starts the task again.
            for mut approval in tx.read().list::<Approval>("approval")? {
                if crate::carry_on::blocks(&tx.read(), &approval)? { approval.state = ApprovalState::Expired; approval.revision += 1; tx.put("approval", approval.approval_id, &approval)?; tx.emit(Event::ApprovalUpserted { approval })?; }
            }
            for token in tx.read().list::<crate::bridge::TokenRecord>("run_token")? { tx.delete("run_token", token.run_id)?; }
            crate::waiting::settle_all(tx, started)?;
            crate::runs::settle_archived_attention(tx, started)?;
            crate::commands::settle_home(tx, home.as_deref(), started)?;
            // Pairing ceremonies never survive a process lifetime.
            tx.delete("meta", "pairing")?;
            Ok(())
        }).map_err(stored)?;
        let (shutdown, _) = tokio::sync::watch::channel(false);
        let hub = CompanionHub {
            inner: Arc::new(Inner {
                reports: crate::reports::Runtime::new(self.reports, &data, started)?,
                contributions: crate::contribution::Runtime::default(),
                db: Mutex::new(db),
                subscribers: Mutex::new(Vec::new()),
                credentials: AtomicU64::new(0),
                observed: crate::observed::Observer::default(),
                chat_projects: Mutex::default(),
                pairing: Mutex::default(),
                preview_forwards: Default::default(),
                preview_servers: AsyncMutex::default(),
                app_terminals: AsyncMutex::default(),
                windows: self.windows,
                accounts: self.accounts,
                agent_logins: self.agent_logins,
                agent_updater: self.agent_updater,
                terminal_block: self.terminal_block,
                app_updates: self.app_updates,
                agent_updates: AsyncMutex::new(()),
                planning: AsyncMutex::new(()),
                agent_checks: std::sync::atomic::AtomicUsize::new(0),
                agent_browser: self.agent_browser,
                github: self.github,
                system: self.system.map(crate::system::SystemSampler::new),
                finder: self.servers.map(crate::found_previews::Finder::new),
                browser_runs: Mutex::default(),
                browser_turns: Mutex::default(),
                login_asks: Mutex::default(),
                launchers: self.launchers,
                configured_roots: Mutex::default(),
                agent_files: Mutex::default(),
                run_secrets: Mutex::default(),
                head: AtomicU64::new(commit.head),
                host_id,
                epoch,
                name: self.name,
                started,
                clock: self.clock,
                data,
                private,
                home,
                folder_browsers: crate::project_folders::slots(),
                inbox: Mutex::new(inbox),
                harnesses: self.harnesses,
                sandboxes: self.sandboxes,
                sandbox_runs: Mutex::default(),
                preview_runs: Mutex::default(),
                access_changes: Mutex::default(),
                voice: Default::default(),
                desktop: Mutex::new(self.desktop),
                desktop_absent: Mutex::new(self.desktop_absent),
                desktop_changed: tokio::sync::watch::channel(0).0,
                runtime_change: AsyncMutex::new(()),
                expiry: Arc::default(),
                guard: Arc::default(),
                followups: AsyncMutex::new(()),
                guarding: AsyncMutex::new(()),
                together: AsyncMutex::new(()),
                liveness: Mutex::new(HashMap::new()),
                protection: AsyncMutex::new(crate::runtime::ProtectionSlot::default()),
                local_stops: std::sync::atomic::AtomicUsize::new(0),
                desktop_operation: tokio::sync::RwLock::new(()),
                unlock_tries: AsyncMutex::new(None),
                unlock_release: tokio::sync::RwLock::new(()),
                active_media: Mutex::new(Vec::new()),
                media: self.media,
                library: self.library,
                terminal: self.terminal,
                speech: Mutex::new(crate::runtime::SpeechSlot::new(self.speech)),
                preview: Some(match self.preview {
                    Some(checker) => checker,
                    None => Arc::new(crate::preview::HttpPreviewChecker::new()?),
                }),
                bridge: self.bridge,
                runs: AsyncMutex::new(HashMap::new()),
                message_locks: Mutex::new(HashMap::new()),
                controls: AsyncMutex::new(HashMap::new()),
                waits: AsyncMutex::new(HashMap::new()),
                transfer_locks: Mutex::new(HashMap::new()),
                blobs: Default::default(),
                scratch: Default::default(),
                renewal: self.renewal,
                max_upload: self.max_upload,
                copying: Mutex::new(HashMap::new()),
                copies: Mutex::new(std::collections::HashSet::new()),
                shutdown,
                jobs: Mutex::new(Vec::new()),
                _instance_lock: instance_lock,
            }),
        };
        for event in &commit.events {
            hub.observe_report_event(event);
        }
        hub.refresh_catalog().await?;
        hub.refresh_host().await?;
        hub.refresh_readiness().await?;
        hub.start_reports().await?;
        hub.start_background();
        // A review's findings left waiting for their verifier when Ophio
        // last stopped go to it now.
        let resuming = hub.clone();
        hub.spawn(async move {
            if let Err(e) = resuming.resume_reviews().await {
                tracing::error!(code=?e.code, "carrying on reviews' verification failed");
            }
        });
        // What was ready to start when Ophio last stopped starts now, not
        // at whatever next moves Next up.
        hub.progress_queues();
        Ok(hub)
    }
}
impl CompanionHub {
    pub(crate) fn spawn(&self, future: impl std::future::Future<Output = ()> + Send + 'static) {
        if let Ok(mut jobs) = self.inner.jobs.lock() {
            if *self.inner.shutdown.borrow() {
                return;
            }
            jobs.retain(|j| !j.is_finished());
            jobs.push(tokio::spawn(future));
        }
    }
    pub fn builder() -> HubBuilder {
        HubBuilder::default()
    }
    pub(crate) fn read<T>(
        &self,
        f: impl FnOnce(&store::Read<'_>) -> store::Result<T>,
    ) -> Result<T, ProtocolError> {
        self.inner
            .db
            .lock()
            .map_err(internal)?
            .read(f)
            .map_err(stored)
    }
    pub(crate) fn write<T>(
        &self,
        f: impl FnOnce(&mut store::Write<'_>) -> store::Result<T>,
    ) -> Result<T, ProtocolError> {
        let mut db = self.inner.db.lock().map_err(internal)?;
        let now = self.inner.clock.now();
        let commit = db
            .write(now, |tx| {
                let value = f(tx)?;
                crate::waiting::settle(tx, now)?;
                Ok(value)
            })
            .map_err(stored)?;
        self.inner.head.store(commit.head, Ordering::Release);
        if commit
            .events
            .iter()
            .any(|e| matches!(e.event, Event::CommandProgress { .. }))
            && (commit.changed(store::reports::QUEUE) || commit.changed(store::reports::META))
        {
            self.inner.reports.wake.notify_one();
        }
        if commit.changed("device") || commit.changed("channel") {
            self.inner.credentials.fetch_add(1, Ordering::Relaxed);
        }
        let credentials = self.inner.credentials.load(Ordering::Relaxed);
        let mut subscribers = self.inner.subscribers.lock().map_err(internal)?;
        subscribers.retain(|s| s.sender.receiver_count() > 0);
        // Every subscriber shares one copy of each event and one read.
        let events: Vec<_> = commit.events.into_iter().map(Arc::new).collect();
        for event in &events {
            let _ = self.inner.reports.events.send(event.clone());
        }
        db.read(|r| {
            for subscriber in subscribers.iter_mut() {
                if let Some(current) = subscriber.current(r, credentials) {
                    for event in &events {
                        if let Some(event) = Self::shown(r, &current, event, subscriber.agent)? {
                            let _ = subscriber.sender.send(Outbound::Event(event));
                        }
                    }
                }
            }
            Ok(())
        })
        .map_err(stored)?;
        Ok(commit.value)
    }
    pub(crate) fn live(&self, event: LiveEvent) {
        if let (Ok(mut db), Ok(mut subscribers)) =
            (self.inner.db.lock(), self.inner.subscribers.lock())
        {
            let event = Arc::new(event);
            let credentials = self.inner.credentials.load(Ordering::Relaxed);
            let _ = db.read(|r| {
                for subscriber in subscribers.iter_mut() {
                    if subscriber
                        .current(r, credentials)
                        .is_some_and(|c| c.grants.tasks)
                    {
                        let _ = subscriber.sender.send(Outbound::Live(event.clone()));
                    }
                }
                Ok(())
            });
        }
    }
    pub(crate) fn current_caller(
        r: &store::Read<'_>,
        caller: &Caller,
    ) -> Result<Caller, ProtocolError> {
        let persisted = match caller.principal {
            Principal::Device { device_id } => {
                let d: crate::identity::Device = r.require("device", device_id).map_err(stored)?;
                if d.record.revoked_at.is_some() {
                    return Err(error(ErrorCode::Unauthenticated, "Device revoked"));
                }
                d.record.grants
            }
            Principal::Channel { identity_id } => {
                let c: ChannelIdentity = r.require("channel", identity_id).map_err(stored)?;
                if c.revoked_at.is_some() {
                    return Err(error(ErrorCode::Unauthenticated, "Channel revoked"));
                }
                c.grants
            }
            Principal::LocalOwner => Grants::owner(),
            Principal::System => {
                return Err(error(
                    ErrorCode::PermissionDenied,
                    "System identity is reserved for internal work",
                ));
            }
        };
        let a = &caller.grants;
        let b = persisted;
        let grants = Grants {
            view: a.view && b.view,
            control_input: a.control_input && b.control_input,
            terminal: a.terminal && b.terminal,
            tasks: a.tasks && b.tasks,
            approvals: a.approvals && b.approvals,
            computer_use: a.computer_use && b.computer_use,
            library_read: a.library_read && b.library_read,
            library_write: a.library_write && b.library_write,
            transfers: a.transfers && b.transfers,
            admin: a.admin && b.admin,
            accounts: a.accounts && b.accounts,
        };
        Ok(Caller {
            principal: caller.principal,
            grants: domain::grants::effective(grants, caller.principal),
        })
    }
    pub(crate) fn authorize(
        &self,
        caller: &Caller,
        permit: impl FnOnce(&Grants) -> bool,
    ) -> Result<Caller, ProtocolError> {
        self.read(|r| {
            let current = Self::current_caller(r, caller)?;
            if !permit(&current.grants) {
                return Err(error(
                    ErrorCode::PermissionDenied,
                    "This device or chat wasn’t given permission for that",
                )
                .into());
            }
            Ok(current)
        })
    }
    /// The event as this caller may see it: withheld, whole, or listing only
    /// the terminals it opened.
    fn shown(
        r: &store::Read<'_>,
        caller: &Caller,
        envelope: &Arc<EventEnvelope>,
        agent: bool,
    ) -> store::Result<Option<Arc<EventEnvelope>>> {
        if agent
            && matches!(&envelope.event, Event::AttentionUpserted { item } if crate::report_inbox::is_inbox(item))
        {
            return Ok(None);
        }
        if !Self::visible(r, caller, &envelope.event)? {
            return Ok(None);
        }
        if let Event::TerminalsChanged { terminals } = &envelope.event {
            let own = Self::own_terminals(r, caller, terminals.clone())?;
            if own.len() != terminals.len() {
                let mut envelope = (**envelope).clone();
                envelope.event = Event::TerminalsChanged { terminals: own };
                return Ok(Some(Arc::new(envelope)));
            }
        }
        Ok(Some(envelope.clone()))
    }
    pub(crate) fn visible(
        r: &store::Read<'_>,
        caller: &Caller,
        event: &Event,
    ) -> store::Result<bool> {
        if !domain::grants::event(&caller.grants, event) {
            return Ok(false);
        }
        match event {
            Event::AttentionUpserted { item }
                if crate::report_inbox::is_inbox(item)
                    && matches!(caller.principal, Principal::Channel { .. }) =>
            {
                return Ok(false);
            }
            Event::TransferUpserted { transfer } => {
                return Self::sees_transfer(r, caller, transfer.transfer_id);
            }
            Event::ArtifactCreated { artifact } => return Self::sees_artifact(r, caller, artifact),
            _ => {}
        }
        if let Event::CommandProgress { command_id, .. } = event {
            if r.command_quarantined(*command_id)? {
                // We cannot establish the original body or owner. Only a
                // manager may see the unknown outcome; never leak old results.
                return Ok(caller.grants.admin
                    && matches!(event,
                    Event::CommandProgress { stage: CommandStage::ReconciliationRequired, error: Some(e), result: None, detail: Some(detail), .. }
                    if *e == store::unknown_command_outcome()
                        && detail == "Stored command is unreadable; outcome unknown; review required"));
            }
            return Ok(r.command(*command_id)?.is_some_and(|c| {
                (c.principal == caller.principal || caller.grants.admin)
                    && domain::grants::command(&caller.grants, &c.envelope.body)
            }));
        }
        Ok(true)
    }
    pub async fn refresh_catalog(&self) -> Result<(), ProtocolError> {
        let mut infos = Vec::new();
        for adapter in &self.inner.harnesses {
            let mut info = adapter.probe().await;
            if let Some(catalog) = &mut info.catalog {
                catalog.checked_at = self.inner.clock.now();
            }
            if info.kind == HarnessKind::ClaudeCode && self.inner.bridge.is_none() {
                let unavailable = Availability::PermissionMissing { remedy: "Work needs the permission bridge. Install computer-mcp beside Øphio on the computer to enable Claude permission prompts.".into() };
                info.capabilities.native_approvals = unavailable.clone();
                // Starting Ask/Plan needs no permission bridge. Gate Work in
                // the mode catalog used by admission and the phone.
                if let Some(work) = info.modes.iter_mut().find(|m| m.mode == TaskMode::Work) {
                    work.availability = unavailable.clone();
                } else {
                    info.modes.push(protocol::capability::ModeSupport {
                        mode: TaskMode::Work,
                        availability: unavailable.clone(),
                        enforcement: "Work needs the permission bridge.".into(),
                    });
                }
                info.capabilities.computer_use_delegation = unavailable;
            }
            infos.push(info);
        }
        let now = self.inner.clock.now();
        self.write(|tx| {
            let stored = tx.read().get::<Vec<HarnessInfo>>("meta", "harnesses")?;
            crate::usage_limits::carry(&mut infos, stored.as_deref().unwrap_or_default(), now);
            if stored.as_ref() != Some(&infos) {
                tx.put("meta", "harnesses", &infos)?;
                tx.emit(Event::HarnessesChanged { harnesses: infos })?;
            }
            Ok(())
        })
    }
    fn start_background(&self) {
        let weak = Arc::downgrade(&self.inner);
        let mut shutdown = self.inner.shutdown.subscribe();
        let expiry = self.inner.expiry.clone();
        self.spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_millis(50));
            loop {
                tokio::select! { _ = shutdown.changed() => break, _ = tick.tick() => { let Some(inner) = weak.upgrade() else { break }; let hub = Self { inner }; if let Err(e) = hub.expire_voice().await { tracing::error!(code=?e.code, "voice expiry failed"); }
 if let Err(e) = hub.expire_leases().await { tracing::error!(code=?e.code, "lease expiry failed"); }
                    // Nothing can expire until a lease or voice session starts.
                    // Sleep until then rather than ticking while idle.
                    if !hub.expiry_pending().await {
                        drop(hub);
                        tokio::select! { _ = shutdown.changed() => break, _ = expiry.notified() => tick.reset() }
                    } } }
            }
        });
        // The guard over running turns: unanswered requests, turns with no
        // sign of work, and what is still to pass on to an agent. It rests
        // while nothing runs and nothing is waiting to go.
        let weak = Arc::downgrade(&self.inner);
        let mut shutdown = self.inner.shutdown.subscribe();
        let guard = self.inner.guard.clone();
        self.spawn(async move {
            loop {
                let Some(inner) = weak.upgrade() else { break };
                let busy = (Self { inner }).guard_once().await;
                if busy {
                    tokio::select! {
                        _ = shutdown.changed() => break,
                        _ = tokio::time::sleep(crate::watchdog::GUARD_EVERY) => {}
                        _ = guard.notified() => {}
                    }
                } else {
                    tokio::select! {
                        _ = shutdown.changed() => break,
                        _ = guard.notified() => {}
                    }
                }
            }
        });
        let weak = Arc::downgrade(&self.inner);
        let mut shutdown = self.inner.shutdown.subscribe();
        self.spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(60 * 60));
            loop {
                tokio::select! { _ = shutdown.changed() => break, _ = tick.tick() => { let Some(inner) = weak.upgrade() else { break }; let hub = Self { inner }; if let Err(e) = hub.expire_shown().await { tracing::error!(code=?e.code, "shown expiry failed"); }
                    if let Err(e) = hub.collect_transfers().await { tracing::error!(code=?e.code, "transfer cleanup failed"); }
                    if let Err(e) = hub.collect_scratch().await { tracing::error!(code=?e.code, "scratch cleanup failed"); }
                    if let Some(library) = &hub.inner.library && let Err(error) = library.purge().await { tracing::error!(%error, "Library cleanup failed"); } }
                }
            }
        });
        let weak = Arc::downgrade(&self.inner);
        let mut shutdown = self.inner.shutdown.subscribe();
        self.spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(300));
            tick.tick().await;
            loop {
                tokio::select! { _ = shutdown.changed() => break, _ = tick.tick() => { let Some(inner) = weak.upgrade() else { break }; if let Err(e) = (Self { inner }).refresh_catalog().await { tracing::error!(code=?e.code, "harness refresh failed"); } } }
            }
        });
        if self.inner.agent_updater.is_some() {
            let weak = Arc::downgrade(&self.inner);
            let mut shutdown = self.inner.shutdown.subscribe();
            self.spawn(async move {
                let mut tick = tokio::time::interval(crate::agent_updates::EVERY);
                tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                loop {
                    tokio::select! {
                        _ = shutdown.changed() => break,
                        _ = tick.tick() => {
                            let Some(inner) = weak.upgrade() else { break };
                            (Self { inner }).update_agents();
                        }
                    }
                }
            });
        }
        // Only with a GitHub port: an idle hub without one never ticks.
        if self.inner.github.is_some() {
            let weak = Arc::downgrade(&self.inner);
            let mut shutdown = self.inner.shutdown.subscribe();
            self.spawn(async move {
                let mut tick = tokio::time::interval(Duration::from_secs(60));
                tick.tick().await;
                loop {
                    tokio::select! {
                        _ = shutdown.changed() => break,
                        _ = tick.tick() => {
                            let Some(inner) = weak.upgrade() else { break };
                            let hub = Self { inner };
                            if let Err(e) = hub.github_after_pending().await {
                                tracing::error!(code=?e.code, "acting on GitHub runs' ends failed");
                            }
                            if let Err(e) = hub.watch_prs().await {
                                tracing::error!(code=?e.code, "pull request watch failed");
                            }
                        }
                    }
                }
            });
        }
        if self.inner.finder.is_some() {
            let weak = Arc::downgrade(&self.inner);
            let mut shutdown = self.inner.shutdown.subscribe();
            self.spawn(async move {
                let mut tick = tokio::time::interval(crate::found_previews::EVERY);
                tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                tick.tick().await;
                loop {
                    tokio::select! {
                        _ = shutdown.changed() => break,
                        _ = tick.tick() => {
                            let Some(inner) = weak.upgrade() else { break };
                            let hub = Self { inner };
                            // A pass may wait on a slow server; shutting down doesn't.
                            tokio::select! {
                                _ = shutdown.changed() => break,
                                found = hub.find_previews() => if let Err(e) = found {
                                    tracing::error!(code=?e.code, "finding projects' sites failed");
                                },
                            }
                        }
                    }
                }
            });
        }
        if let Some(accounts) = &self.inner.accounts {
            let mut changed = accounts.changed();
            let weak = Arc::downgrade(&self.inner);
            let mut shutdown = self.inner.shutdown.subscribe();
            self.spawn(async move {
                loop {
                    tokio::select! {
                        _ = shutdown.changed() => break,
                        next = changed.changed() => {
                            let Some(inner) = weak.upgrade() else { break };
                            if next.is_err() { break }
                            // Devices with the grant read the new view themselves.
                            let _ = (Self { inner }).write(|tx| { tx.emit(Event::AccountsChanged {})?; Ok(()) });
                        }
                    }
                }
            });
        }
        if let Some(logins) = &self.inner.agent_logins {
            let mut changed = logins.changed();
            let weak = Arc::downgrade(&self.inner);
            let mut shutdown = self.inner.shutdown.subscribe();
            self.spawn(async move {
                loop {
                    tokio::select! {
                        _ = shutdown.changed() => break,
                        next = changed.changed() => {
                            let Some(inner) = weak.upgrade() else { break };
                            if next.is_err() { break }
                            let hub = Self { inner };
                            let _ = hub.write(|tx| { tx.emit(Event::AgentLoginsChanged {})?; Ok(()) });
                            // Pages a task may no longer use close now, not
                            // at its next step.
                            if let Some(browser) = &hub.inner.agent_browser {
                                browser.recheck_access().await;
                            }
                        }
                    }
                }
            });
        }
        if let Some(library) = &self.inner.library {
            let mut changes = library.changes();
            let weak = Arc::downgrade(&self.inner);
            let mut shutdown = self.inner.shutdown.subscribe();
            self.spawn(async move {
                loop {
                    tokio::select! { _ = shutdown.changed() => break, change = changes.recv() => { let Some(inner) = weak.upgrade() else { break }; let hub = Self { inner }; match change { Ok(c) => { let _ = hub.library_changed(c.root_id, &c.path, c.revision); }, Err(broadcast::error::RecvError::Lagged(_)) => { if let Some(library) = &hub.inner.library { let roots = library.roots().await; let _ = hub.write(|tx| { tx.put("meta", "library_roots", &roots)?; tx.emit(Event::LibraryRootsChanged { roots })?; Ok(()) }); } }, Err(_) => break } } }
                }
            });
        }
    }
    /// Stop background work and revoke managed input before process shutdown.
    pub async fn shutdown(&self) -> Result<(), ProtocolError> {
        self.report_trail(protocol::reports::Trail::Service {
            state: protocol::reports::ServiceState::Stopping,
        });
        if let Ok(mut trail) = self.inner.reports.trail.lock() {
            trail.flush(true);
        }
        let controls: Vec<_> = {
            let runs = self.inner.runs.lock().await;
            self.inner.shutdown.send_replace(true);
            runs.values().cloned().collect()
        };
        // Run events are no longer recorded, and an accepted Stop may never
        // reach its effect. Nothing held is written from here on.
        for control in &controls {
            control.close_admission();
        }
        let desktop_result = self
            .set_desktop(
                None,
                Availability::TemporarilyUnavailable {
                    reason: "Øphio is shutting down on the computer".into(),
                },
            )
            .await;
        if let Some(terminal) = &self.inner.terminal {
            for info in terminal.list().await {
                let _ = terminal.close(info.terminal_id).await;
            }
        }
        for control in controls {
            let _ = tokio::time::timeout(Duration::from_secs(5), control.shutdown()).await;
        }
        let jobs = std::mem::take(&mut *self.inner.jobs.lock().map_err(internal)?);
        for mut job in jobs {
            if tokio::time::timeout(Duration::from_secs(35), &mut job)
                .await
                .is_err()
            {
                job.abort();
                let _ = job.await;
            }
        }
        if let Some(terminal) = &self.inner.terminal {
            for info in terminal.list().await {
                let _ = terminal.close(info.terminal_id).await;
            }
        }
        self.inner.runs.lock().await.clear();
        self.inner.waits.lock().await.clear();
        desktop_result
    }
}

impl CompanionHub {
    async fn subscribe_for(
        &self,
        caller: &Caller,
        after: Option<u64>,
        agent: bool,
    ) -> Result<Subscription, ProtocolError> {
        let mut db = self.inner.db.lock().map_err(internal)?;
        let (sender, live) = broadcast::channel(1024);
        // Receiver exists before the read transaction; writer and subscriber
        // registration serialize on the same mutex, including publication.
        let (current, journal, events) = db
            .read(|r| {
                let c = Self::current_caller(r, caller)?;
                let j = r.journal(after.unwrap_or(r.head()?))?;
                let mut events = Vec::new();
                for e in &j.events {
                    if let Some(e) = Self::shown(r, &c, &Arc::new(e.clone()), agent)? {
                        events.push(e);
                    }
                }
                Ok((c, j, events))
            })
            .map_err(stored)?;
        let backlog = match after {
            Some(seq) if seq < journal.oldest_retained.saturating_sub(1) => Backlog::Gap {
                requested_from: seq,
                oldest_retained: journal.oldest_retained,
            },
            Some(seq) if seq > journal.head => {
                return Err(error(
                    ErrorCode::InvalidRequest,
                    "Journal cursor is ahead of this computer",
                ));
            }
            _ => Backlog::Events(events),
        };
        self.inner
            .subscribers
            .lock()
            .map_err(internal)?
            .push(Subscriber {
                caller: current,
                sender,
                agent,
                current: None,
            });
        Ok(Subscription {
            backlog,
            head: journal.head,
            live,
        })
    }
    async fn backlog_for(
        &self,
        caller: &Caller,
        after: u64,
        head: u64,
        limit: usize,
        agent: bool,
    ) -> Result<(Backlog, u64), ProtocolError> {
        self.read(|r| {
            let current = Self::current_caller(r, caller)?;
            let journal = r.journal_page(after, limit)?;
            if after < journal.oldest_retained.saturating_sub(1) {
                let gap = Backlog::Gap {
                    requested_from: after,
                    oldest_retained: journal.oldest_retained,
                };
                return Ok((gap, after));
            }
            if after > head {
                return Err(error(
                    ErrorCode::InvalidRequest,
                    "Journal cursor is ahead of this computer",
                )
                .into());
            }
            // A short page read everything committed; so did one that passed `head`.
            let mut through = if journal.events.len() < limit {
                head
            } else {
                after
            };
            let mut events = Vec::new();
            for event in journal.events {
                if event.seq > head {
                    through = head;
                    break;
                }
                through = through.max(event.seq);
                if let Some(event) = Self::shown(r, &current, &Arc::new(event), agent)? {
                    events.push(event);
                }
            }
            Ok((Backlog::Events(events), through))
        })
    }
}

#[async_trait]
impl Hub for CompanionHub {
    fn host_id(&self) -> HostId {
        self.inner.host_id
    }
    fn host_name(&self) -> String {
        self.inner.name.clone()
    }
    fn boot_epoch(&self) -> BootEpoch {
        self.inner.epoch
    }
    fn journal_head(&self) -> u64 {
        self.inner.head.load(Ordering::Acquire)
    }
    async fn capabilities(&self) -> HostCapabilities {
        self.host_capabilities().await
    }
    async fn open_pairing(&self, c: &Caller) -> Result<PairingTicket, ProtocolError> {
        self.open_ticket(c).await
    }
    async fn request_pairing(
        &self,
        r: PairRequest,
        code: String,
    ) -> Result<PairingClaim, ProtocolError> {
        self.hold_pairing(r, code)
    }
    async fn waiting_pairing(&self, c: &Caller) -> Result<Option<PairingPrompt>, ProtocolError> {
        self.pairing_prompt(c)
    }
    async fn answer_pairing(
        &self,
        c: &Caller,
        code: &str,
        confirm: bool,
    ) -> Result<Option<DeviceRecord>, ProtocolError> {
        let Some((record, replaced)) = self.answer_pairing_request(c, code, confirm)? else {
            return Ok(None);
        };
        // As when the owner revokes it: the replaced device lets go of the
        // desktop and voice, and its shells end. The pairing already stands,
        // so a lease or shell that fails to end here does not undo it; the
        // device can no longer act.
        if let Some(replaced) = replaced {
            let _ = self.end_device_access(replaced).await;
        }
        Ok(Some(record))
    }
    async fn device_credential(&self, id: &DeviceId) -> Option<DeviceCredential> {
        self.credential(*id)
    }
    async fn device_seen(&self, id: &DeviceId) {
        self.seen(*id);
    }
    async fn channel_identity(
        &self,
        kind: ChannelKind,
        user: &str,
        chat: &str,
    ) -> Option<ChannelIdentity> {
        self.find_channel(kind, user, chat)
    }
    async fn request_channel_link(&self, r: ChannelLinkRequest) -> Result<String, ProtocolError> {
        self.link_request(r)
    }
    async fn channel_link_outcome(
        &self,
        kind: ChannelKind,
        code: &str,
    ) -> Option<ChannelLinkOutcome> {
        self.link_outcome(kind, code)
    }
    async fn submit(&self, c: &Caller, e: CommandEnvelope) -> CommandReceipt {
        self.admit(c, e).await
    }
    async fn submit_received(
        &self,
        c: &Caller,
        e: CommandEnvelope,
        received: Timestamp,
    ) -> CommandReceipt {
        self.admit_received(c, e, received, None).await
    }
    async fn reject_command(
        &self,
        c: &Caller,
        e: CommandEnvelope,
        error: ProtocolError,
    ) -> CommandReceipt {
        self.admit_received(c, e, self.inner.clock.now(), Some(error))
            .await
    }
    async fn query(&self, c: &Caller, q: Query) -> Result<QueryData, ProtocolError> {
        self.query_impl(c, q).await
    }
    async fn agent_query(&self, c: &Caller, q: Query) -> Result<QueryData, ProtocolError> {
        self.query_for_agent(c, q).await
    }
    async fn agent_refusal(&self, command: &protocol::Command) -> Option<ProtocolError> {
        self.refusal_for_agent(command).await
    }
    async fn subscribe(
        &self,
        caller: &Caller,
        after: Option<u64>,
    ) -> Result<Subscription, ProtocolError> {
        self.subscribe_for(caller, after, false).await
    }
    async fn agent_subscribe(
        &self,
        caller: &Caller,
        after: Option<u64>,
    ) -> Result<Subscription, ProtocolError> {
        self.subscribe_for(caller, after, true).await
    }
    async fn backlog(
        &self,
        caller: &Caller,
        after: u64,
        head: u64,
        limit: usize,
    ) -> Result<(Backlog, u64), ProtocolError> {
        self.backlog_for(caller, after, head, limit, false).await
    }
    async fn agent_backlog(
        &self,
        caller: &Caller,
        after: u64,
        head: u64,
        limit: usize,
    ) -> Result<(Backlog, u64), ProtocolError> {
        self.backlog_for(caller, after, head, limit, true).await
    }
    async fn input(&self, c: &Caller, e: InputEnvelope) -> InputAck {
        self.human_input(c, e).await
    }
    async fn renew_lease(
        &self,
        c: &Caller,
        id: LeaseId,
        generation: u64,
    ) -> Result<(), ProtocolError> {
        self.renew(c, id, generation).await
    }
    async fn control_disconnected(&self, caller: &Caller) {
        self.inner.preview_forwards.disconnect(caller.principal);
    }
    async fn open_alerts(
        &self,
        caller: &Caller,
    ) -> Result<(Vec<protocol::alerts::Alert>, u64), ProtocolError> {
        self.alerts_open(caller)
    }
    async fn alert(
        &self,
        caller: &Caller,
        attention_id: AttentionId,
    ) -> Result<Option<protocol::alerts::Alert>, ProtocolError> {
        self.alert_for(caller, attention_id)
    }
    async fn open_media(
        &self,
        c: &Caller,
        id: TargetId,
        p: protocol::media::StreamParams,
    ) -> Result<MediaSession, ProtocolError> {
        self.media_session(c, id, p).await
    }
    async fn attach_terminal(
        &self,
        c: &Caller,
        id: TerminalId,
        offset: Option<u64>,
    ) -> Result<TerminalAttachment, ProtocolError> {
        self.terminal_attachment(c, id, offset).await
    }
    async fn upload(
        &self,
        c: &Caller,
        id: TransferId,
    ) -> Result<Box<dyn UploadSink>, ProtocolError> {
        self.begin_upload(c, id).await
    }
    async fn download(
        &self,
        c: &Caller,
        id: TransferId,
        offset: u64,
    ) -> Result<crate::api::DownloadSource, ProtocolError> {
        self.begin_download(c, id, offset).await
    }
    async fn cancel_transfer(&self, c: &Caller, id: TransferId) -> Result<(), ProtocolError> {
        self.cancel_upload(c, id).await
    }
    async fn start_speech(
        &self,
        c: &Caller,
        _: String,
        language: Option<String>,
        keep: bool,
    ) -> Result<SpeechStream, ProtocolError> {
        self.speech_stream(c, language, keep).await
    }
    async fn open_preview_connection(
        &self,
        caller: &Caller,
        preview_id: protocol::PreviewId,
    ) -> Result<crate::api::PreviewConnection, ProtocolError> {
        self.preview_connection(caller, preview_id).await
    }
    async fn open_preview(
        &self,
        caller: &Caller,
        id: PreviewId,
    ) -> Result<tokio::io::DuplexStream, ProtocolError> {
        self.preview_stream(caller, id).await
    }
}
