// SPDX-License-Identifier: Apache-2.0
//! Setup's [`Host`] on this computer: the running Øphio, through its owner
//! channel, and the computer's own programs and network.
use super::{
    Progress,
    flow::{Answered, Asked, Host, Mark, Network, Run, Service, Survey, Tailscale, TaskView},
};
use crate::{
    Error, Result,
    cli::{self, finish_command, paired_devices, query, run_command_at},
    config::{Config, Paths},
    install,
    local::{OwnerClient, OwnerRequest, OwnerResponse},
    network::Discovery,
    phone::{self, AlertsAllowed, Installed, Off, Opened, Phones, QrPairing, SystemAdb},
};
use protocol::{
    Command, Query, QueryData,
    accounts::{AccountAction, SignInStage},
    agent_updates::AgentUpdatesView,
    auth::{DeviceRecord, Grants, PairingOffer},
    capability::{HarnessInfo, HarnessKind},
    checklist::{FileDelivery, FileDeliveryView},
    command::{ApprovalDecision, CommandResult, MessageDelivery, TaskPlacement},
    github::{GithubAction, GithubProjectView},
    ids::{ApprovalId, DeviceId, ProjectId, TaskId},
    launchers::{CatalogEntry, CatalogKind, CatalogSource, TaskPlace, TaskStart},
    model::{
        ActivityStatus, Approval, ApprovalKind, ApprovalLineChange, ApprovalState, LaunchPolicy,
        ReadinessReport, TaskKind, TaskMode, TaskState, TimelineBody, TimelineItem,
    },
    time::Timestamp,
};
use services::api::PairingPrompt;
use std::{
    net::IpAddr,
    path::{Path, PathBuf},
};

pub(crate) struct Real {
    paths: Paths,
    client: Option<OwnerClient>,
    /// The phones Google's `adb` reaches, once it has been found.
    phones: Option<Phones<SystemAdb>>,
}

impl Real {
    pub fn new(paths: Paths) -> Result<Self> {
        Ok(Self {
            paths,
            client: None,
            phones: None,
        })
    }

    async fn client(&mut self) -> Result<&mut OwnerClient> {
        if self.client.is_none() {
            self.client = Some(OwnerClient::connect(&self.paths.socket()).await?);
        }
        Ok(self.client.as_mut().expect("connected above"))
    }

    /// Asks the running Øphio, connecting again next time if the
    /// connection was lost.
    async fn owner(&mut self, request: OwnerRequest) -> Result<OwnerResponse> {
        let result = self.client().await?.request(&request).await;
        self.forget_if_lost(&result);
        result
    }

    fn forget_if_lost<T>(&mut self, result: &Result<T>) {
        if matches!(result, Err(Error::Closed | Error::Io(_))) {
            self.client = None;
        }
    }

    async fn command(&mut self, command: Command) -> Result<protocol::CommandReceipt> {
        self.command_at(command, None).await
    }

    async fn command_at(
        &mut self,
        command: Command,
        revision: Option<u64>,
    ) -> Result<protocol::CommandReceipt> {
        let result = run_command_at(self.client().await?, command, revision).await;
        self.forget_if_lost(&result);
        result
    }

    async fn finish(&mut self, command: Command) -> Result<QueryData> {
        let result = finish_command(self.client().await?, command).await;
        self.forget_if_lost(&result);
        result
    }

    fn phones_mut(&mut self) -> Result<&mut Phones<SystemAdb>> {
        if self.phones.is_none() {
            self.phones = Some(phone::system()?);
        }
        Ok(self.phones.as_mut().expect("found above"))
    }

    async fn query(&mut self, wanted: Query) -> Result<QueryData> {
        let result = query(self.client().await?, wanted).await;
        self.forget_if_lost(&result);
        result
    }
}

impl Host for Real {
    fn save(&mut self, progress: &Progress) -> Result<()> {
        super::save(&self.paths, progress)
    }

    fn survey(&mut self) -> Survey {
        Survey {
            name: hostname::get()
                .ok()
                .and_then(|name| name.into_string().ok())
                .unwrap_or_else(|| "This computer".into()),
            claude: program("claude").is_some(),
            codex: program("codex").is_some(),
            tailscale: program("tailscale").is_some(),
            npm: program("npm").is_some(),
        }
    }

    async fn service(&mut self) -> Result<Service> {
        let running = match self.client().await {
            Ok(_) => true,
            Err(Error::NotRunning) => false,
            Err(error) => return Err(error),
        };
        Ok(Service {
            running,
            at_login: install::starts_at_login().await?,
        })
    }

    async fn install_service(&mut self) -> Result<Option<String>> {
        self.client = None;
        install::login_service(&self.paths).await
    }

    async fn start(&mut self) -> Result<()> {
        self.client = None;
        install::start_now(&self.paths).await
    }

    async fn restart(&mut self) -> Result<()> {
        self.client = None;
        install::stop_now(&self.paths).await?;
        install::start_now(&self.paths).await
    }

    async fn network(&mut self) -> Result<Network> {
        let discovery = Discovery::detect().await;
        let tailscale = if !discovery.tailscale_ips.is_empty() {
            Tailscale::Running {
                name: discovery
                    .dns_name
                    .map(|name| name.trim_end_matches('.').to_owned()),
                ips: discovery.tailscale_ips.clone(),
            }
        } else if program("tailscale").is_some() {
            Tailscale::Stopped
        } else {
            Tailscale::Missing
        };
        let config = Config::load(&self.paths.config_file())?;
        Ok(Network {
            tailscale,
            wifi: cli::local_address().filter(|ip| !discovery.tailscale_ips.contains(ip)),
            allow_lan: config.allow_lan,
            listen: config.listen,
            port: config.port,
        })
    }

    async fn allow_wifi(&mut self, ip: IpAddr) -> Result<()> {
        self.client = None;
        let was = install::stop_now(&self.paths).await?;
        {
            // No Øphio starts on a half-changed config.
            let _lock = self.paths.lock()?;
            let file = self.paths.config_file();
            let mut config = Config::load(&file)?;
            config.allow_lan = true;
            if !config.listen.contains(&ip) {
                config.listen.push(ip);
            }
            config.validate(&self.paths)?;
            config.save(&file)?;
        }
        if was {
            install::start_now(&self.paths).await?;
        }
        Ok(())
    }

    async fn agents(&mut self) -> Result<Vec<HarnessInfo>> {
        // The catalog is kept from the last check, which may be from before
        // an install or a sign-in.
        self.finish(Command::RunReadinessCheck).await?;
        let QueryData::Harnesses { harnesses } = self.query(Query::Harnesses).await? else {
            return Err(crate::error::invalid("unexpected harnesses response"));
        };
        Ok(harnesses
            .into_iter()
            .filter(|h| matches!(h.kind, HarnessKind::ClaudeCode | HarnessKind::Codex))
            .collect())
    }

    async fn agent_updates(&mut self) -> Result<Option<AgentUpdatesView>> {
        match self.query(Query::AgentUpdates).await {
            Ok(QueryData::AgentUpdates { view }) => Ok(Some(view)),
            // An Øphio from before it kept the agents up to date.
            Err(Error::Protocol(error))
                if matches!(
                    error.code,
                    protocol::ErrorCode::InvalidRequest | protocol::ErrorCode::Unsupported
                ) =>
            {
                Ok(None)
            }
            Ok(_) => Err(crate::error::invalid("unexpected agent updates response")),
            Err(error) => Err(error),
        }
    }

    async fn set_agent_updates(&mut self, enabled: bool, revision: u64) -> Result<()> {
        self.command_at(Command::SetAgentUpdates { enabled }, Some(revision))
            .await
            .map(drop)
    }

    async fn file_delivery(&mut self) -> Result<Option<FileDeliveryView>> {
        match self.query(Query::FileDelivery).await {
            Ok(QueryData::FileDelivery { view }) => Ok(Some(view)),
            // An Øphio from before checklists.
            Err(Error::Protocol(error))
                if matches!(
                    error.code,
                    protocol::ErrorCode::InvalidRequest | protocol::ErrorCode::Unsupported
                ) =>
            {
                Ok(None)
            }
            Ok(_) => Err(crate::error::invalid("unexpected file delivery response")),
            Err(error) => Err(error),
        }
    }

    async fn set_file_delivery(&mut self, delivery: FileDelivery, revision: u64) -> Result<()> {
        self.command_at(Command::SetFileDelivery { delivery }, Some(revision))
            .await
            .map(drop)
    }

    async fn sign_in(&mut self, account_id: &str) -> Result<()> {
        self.command(Command::Account {
            action: AccountAction::SignIn {
                account_id: account_id.into(),
            },
        })
        .await
        .map(drop)
    }

    async fn sign_in_stage(
        &mut self,
        account_id: &str,
        since: Timestamp,
    ) -> Result<Option<SignInStage>> {
        let QueryData::Accounts { view } = self.query(Query::Accounts).await? else {
            return Err(crate::error::invalid("unexpected accounts response"));
        };
        Ok(view
            .sign_in
            .filter(|s| s.account_id == account_id && s.started_at.0 >= since.0)
            .map(|s| s.stage))
    }

    async fn cancel_sign_in(&mut self) -> Result<()> {
        self.command(Command::Account {
            action: AccountAction::CancelSignIn,
        })
        .await
        .map(drop)
    }

    async fn open_pairing(&mut self) -> Result<PairingOffer> {
        match self.owner(OwnerRequest::OpenPairing).await? {
            OwnerResponse::PairingOffer(offer) => Ok(offer),
            _ => Err(Error::Closed),
        }
    }

    async fn waiting_pairing(&mut self) -> Result<Option<PairingPrompt>> {
        match self.owner(OwnerRequest::WaitingPairing).await? {
            OwnerResponse::PairingWaiting(prompt) => Ok(prompt),
            _ => Err(Error::Closed),
        }
    }

    async fn answer_pairing(&mut self, code: &str, confirm: bool) -> Result<Answered> {
        let request = OwnerRequest::AnswerPairing {
            code: code.into(),
            confirm,
        };
        match self.owner(request).await {
            Ok(OwnerResponse::PairingAnswered(Some(device))) => Ok(Answered::Paired(device)),
            Ok(OwnerResponse::PairingAnswered(None)) => Ok(Answered::Refused),
            Err(Error::Protocol(error)) if error.code == protocol::ErrorCode::NotFound => {
                Ok(Answered::Gone)
            }
            Err(error) => Err(error),
            Ok(_) => Err(Error::Closed),
        }
    }

    async fn devices(&mut self) -> Result<Vec<DeviceRecord>> {
        let result = paired_devices(self.client().await?).await;
        self.forget_if_lost(&result);
        result
    }

    async fn set_grants(&mut self, device_id: DeviceId, grants: Grants) -> Result<()> {
        for (mask, allowed) in grant_changes(grants) {
            self.command(Command::ChangeDeviceGrants {
                device_id,
                mask,
                allowed,
            })
            .await?;
        }
        Ok(())
    }

    async fn add_project(&mut self, folder: &str, agent: Option<HarnessKind>) -> Result<String> {
        let folder = expand_home(folder);
        let path = ports::library::canonical(&folder).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                Error::Ended(format!("There is no folder at {}.", folder.display()))
            } else {
                error.into()
            }
        })?;
        if !path.is_dir() {
            return Err(Error::Ended(format!("{} is not a folder.", path.display())));
        }
        let host_path = path
            .to_str()
            .ok_or_else(|| Error::Ended("A project's folder needs a name in UTF-8.".into()))?
            .to_owned();
        let receipt = self
            .command(Command::AddProject {
                host_path: host_path.clone(),
                name: None,
            })
            .await?;
        let Some(CommandResult::ProjectAdded { project_id }) = receipt.result else {
            return Err(Error::Closed);
        };
        if agent.is_some() {
            self.command(Command::UpdateProjectDefaults {
                project_id,
                harness: agent,
                model: None,
                effort: None,
            })
            .await?;
        }
        Ok(host_path)
    }

    async fn run(&mut self, run: &Run) -> Result<bool> {
        let mut command = match run {
            Run::Ophio(args) => {
                let mut command = tokio::process::Command::new(std::env::current_exe()?);
                command.args(args);
                command
            }
            Run::Program { program, args, .. } => {
                let mut command = tokio::process::Command::new(program);
                command.args(args);
                command
            }
        };
        // It has the terminal to itself until it ends.
        let status = command.kill_on_drop(true).status().await?;
        Ok(status.success())
    }

    async fn readiness(&mut self) -> Result<ReadinessReport> {
        self.finish(Command::RunReadinessCheck).await?;
        match self.query(Query::Readiness).await? {
            QueryData::Readiness { report } => Ok(report),
            _ => Err(crate::error::invalid("unexpected readiness response")),
        }
    }

    fn now(&self) -> Timestamp {
        Timestamp::now()
    }

    fn local_offset(&self, at: time::OffsetDateTime) -> Option<time::UtcOffset> {
        time::UtcOffset::local_offset_at(at).ok()
    }

    fn phone_app(&self) -> Option<PathBuf> {
        phone::bundled_app()
    }

    fn adb(&mut self) -> bool {
        // Looked for again each time, so one installed meanwhile is found.
        if self.phones.is_none() {
            self.phones = SystemAdb::find().map(Phones::new);
        }
        self.phones.is_some()
    }

    async fn phones(&mut self) -> Result<Vec<phone::Device>> {
        self.phones_mut()?.devices().await
    }

    fn adb_pairing(&mut self) -> Result<QrPairing> {
        QrPairing::new()
    }

    async fn adb_pair_qr(&mut self, pairing: &QrPairing) -> Result<Option<phone::Device>> {
        self.phones_mut()?.pair_qr(pairing).await
    }

    async fn adb_pair_code(&mut self, address: Option<&str>, code: &str) -> Result<phone::Device> {
        self.phones_mut()?.pair_code(address, code).await
    }

    async fn phone_install(&mut self, device: &phone::Device, app: &Path) -> Result<Installed> {
        self.phones_mut()?.install(device, app).await
    }

    async fn phone_open(&mut self, device: &phone::Device, app: phone::Open) -> Result<Opened> {
        self.phones_mut()?.open(device, app).await
    }

    async fn phone_link(&mut self, device: &phone::Device, link: &str) -> Result<()> {
        self.phones_mut()?.link(device, link).await
    }

    async fn phone_alerts(&mut self, device: &phone::Device) -> Result<AlertsAllowed> {
        self.phones_mut()?.alerts(device).await
    }

    async fn phone_off(&mut self, device: &phone::Device) -> Result<Off> {
        self.phones_mut()?.off(device).await
    }

    async fn setup_skill(&mut self, id: &str) -> Result<Option<CatalogEntry>> {
        let QueryData::Catalog { catalog } =
            self.query(Query::Catalog { project_id: None }).await?
        else {
            return Err(crate::error::invalid("unexpected catalog response"));
        };
        Ok(catalog.entries.into_iter().find(|entry| {
            entry.source == CatalogSource::OphioSetup
                && entry.kind == CatalogKind::Skill
                && entry.id == id
        }))
    }

    async fn start_setup_task(
        &mut self,
        skill: &CatalogEntry,
        agent: HarnessKind,
        prompt: &str,
    ) -> Result<TaskId> {
        let receipt = self
            .command(Command::CreateTask {
                github: None,
                // A one-off's project is made with it.
                project_id: ProjectId::new(),
                kind: TaskKind::Coding,
                mode: Some(TaskMode::Work),
                title: Some(skill.name.clone()),
                prompt: prompt.into(),
                bundle: None,
                harness: agent,
                model: None,
                effort: None,
                // The agent asks before what it runs, never bypassing that.
                launch_policy: Some(LaunchPolicy::Standard),
                branch: None,
                start: Some(Box::new(TaskStart::Skill {
                    source: CatalogSource::OphioSetup,
                    id: skill.id.clone(),
                    digest: skill.digest.clone(),
                })),
                place: Some(TaskPlace::OneOff {
                    name: skill.name.clone(),
                    date: None,
                }),
                browser: None,
                mission: None,
                target_id: None,
                placement: TaskPlacement::StartNow,
                agent_chosen: None,
                conversation: None,
                access: None,
                review: None,
            })
            .await?;
        match receipt.result {
            Some(CommandResult::TaskCreated { task_id, .. }) => Ok(task_id),
            _ => Err(Error::Closed),
        }
    }

    async fn task_view(&mut self, task_id: TaskId) -> Result<TaskView> {
        let QueryData::Task { records } = self.query(Query::Task { task_id }).await? else {
            return Err(crate::error::invalid("unexpected task response"));
        };
        let Some(task) = records.tasks.iter().find(|t| t.task_id == task_id) else {
            return Err(Error::Ended("The task is gone.".into()));
        };
        let mut approval = records
            .approvals
            .iter()
            .find(|a| a.task_id == task_id && a.state == ApprovalState::Pending)
            .cloned();
        if approval.is_none()
            && let Some(&approval_id) = records.withheld_approvals.first()
            && let QueryData::Approval { approval: found } =
                self.query(Query::Approval { approval_id }).await?
        {
            approval = Some(*found);
        }
        let QueryData::TaskTimeline { items, .. } = self
            .query(Query::TaskTimeline {
                task_id,
                before_seq: None,
                limit: 60,
            })
            .await?
        else {
            return Err(crate::error::invalid("unexpected timeline response"));
        };
        Ok(view(
            task.state,
            task.status_line.clone(),
            approval.as_ref(),
            &items,
        ))
    }

    async fn tell_task(&mut self, task_id: TaskId, text: &str) -> Result<()> {
        let QueryData::Task { records } = self.query(Query::Task { task_id }).await? else {
            return Err(crate::error::invalid("unexpected task response"));
        };
        let now = records
            .tasks
            .iter()
            .find(|t| t.task_id == task_id)
            .map(|t| (t.state, t.revision));
        let (command, revision) = telling(task_id, now, text);
        self.command_at(command, revision).await.map(drop)
    }

    async fn decide(
        &mut self,
        approval_id: ApprovalId,
        revision: u32,
        decision: ApprovalDecision,
    ) -> Result<()> {
        let command = Command::ResolveApproval {
            approval_id,
            decision,
        };
        self.command_at(command, Some(revision.into()))
            .await
            .map(drop)
    }

    async fn stop_task(&mut self, task_id: TaskId) -> Result<()> {
        self.command(Command::StopTask {
            task_id,
            force: false,
        })
        .await
        .map(drop)
    }

    async fn github(&mut self) -> Result<Vec<(String, GithubProjectView)>> {
        let QueryData::Snapshot(snapshot) = self
            .query(Query::Snapshot {
                reads_withheld: Some(true),
            })
            .await?
        else {
            return Err(Error::Closed);
        };
        let mut projects: Vec<_> = (snapshot.projects.into_iter())
            .filter(|p| p.vcs.as_ref().is_some_and(|vcs| vcs.kind == "git") && !p.one_off)
            .collect();
        projects.sort_by(|a, b| a.name.cmp(&b.name));
        let mut views = Vec::with_capacity(projects.len());
        for project in projects {
            let QueryData::GithubProject { view } = self
                .query(Query::GithubProject {
                    project_id: project.project_id,
                })
                .await?
            else {
                return Err(Error::Closed);
            };
            views.push((project.name, *view));
        }
        Ok(views)
    }

    async fn github_action(&mut self, action: GithubAction) -> Result<()> {
        self.finish(Command::Github { action }).await.map(drop)
    }

    fn config_file(&self) -> PathBuf {
        self.paths.config_file()
    }
}

/// The changes that leave a device with exactly `grants`: what it loses
/// first, so that it never holds more than either before or after.
/// The most of what the agent said last that its pane shows: the end of it.
const SAID: usize = 15;
/// How many of the things it has done its pane shows: the latest.
const DOING: usize = 3;

/// A setup task as its pane shows it, from its record, what it waits on the
/// person for, and the end of its timeline.
pub(super) fn view(
    state: TaskState,
    status: Option<String>,
    approval: Option<&Approval>,
    items: &[TimelineItem],
) -> TaskView {
    let said = items.iter().rev().find_map(|item| match &item.item {
        TimelineBody::AgentMessage { text, .. } if !text.trim().is_empty() => {
            let lines: Vec<&str> = text.trim().lines().collect();
            Some(lines[lines.len().saturating_sub(SAID)..].join("\n"))
        }
        _ => None,
    });
    // What it did since the person last spoke to it; a call reported again
    // keeps its place.
    let since = items
        .iter()
        .rposition(|item| matches!(item.item, TimelineBody::UserMessage { .. }))
        .map_or(0, |at| at + 1);
    let mut doing: Vec<(Option<&str>, Mark, String)> = Vec::new();
    for item in &items[since..] {
        let TimelineBody::Activity {
            summary,
            status,
            key,
            ..
        } = &item.item
        else {
            continue;
        };
        let mark = match status {
            ActivityStatus::Running => Mark::Wait,
            ActivityStatus::Succeeded => Mark::Ok,
            ActivityStatus::Failed => Mark::Stop,
            ActivityStatus::Unknown => Mark::Info,
        };
        let key = key.as_deref();
        match doing.iter_mut().find(|(k, ..)| key.is_some() && *k == key) {
            Some(done) => *done = (key, mark, summary.clone()),
            None => doing.push((key, mark, summary.clone())),
        }
    }
    let doing = doing[doing.len().saturating_sub(DOING)..]
        .iter()
        .map(|(_, mark, summary)| (*mark, summary.clone()))
        .collect();
    TaskView {
        state,
        status,
        said,
        doing,
        asks: approval.map(asked),
    }
}

/// Says `text` to a task, given its state and revision as last read: to its
/// turn while one runs, and otherwise in a turn it is resumed with, at that
/// revision. A finished task keeps its last run's id, so whether a turn runs
/// is told by its state.
pub(super) fn telling(
    task_id: TaskId,
    now: Option<(TaskState, u64)>,
    text: &str,
) -> (Command, Option<u64>) {
    match now {
        Some((TaskState::Working | TaskState::AwaitingDecision, _)) => (
            Command::SendMessage {
                task_id,
                text: text.into(),
                bundle: None,
                delivery: MessageDelivery::Steer,
            },
            None,
        ),
        _ => (
            Command::ResumeTask {
                task_id,
                mode: None,
                text: Some(text.into()),
                bundle: None,
                carried_plan: None,
                read_only: false,
                model: None,
                effort: None,
            },
            now.map(|(_, revision)| revision),
        ),
    }
}

/// What an approval asks of the person.
fn asked(approval: &Approval) -> Asked {
    let detail = if approval.detail_lines.is_empty() {
        approval
            .detail
            .as_deref()
            .unwrap_or_default()
            .lines()
            .map(|line| (false, line.to_owned()))
            .collect()
    } else {
        approval
            .detail_lines
            .iter()
            .map(|line| match (&line.label, line.change) {
                (_, Some(change)) => {
                    let mark = match change {
                        ApprovalLineChange::Added => "+ ",
                        ApprovalLineChange::Removed => "- ",
                        ApprovalLineChange::Unchanged => "  ",
                        ApprovalLineChange::Omitted => "⋯ ",
                    };
                    (true, format!("{mark}{}", line.text))
                }
                (Some(label), None) => (false, format!("{label}: {}", line.text)),
                (None, None) => (line.code, line.text.clone()),
            })
            .collect()
    };
    Asked {
        id: approval.approval_id,
        revision: approval.revision,
        question: approval.kind == ApprovalKind::Question,
        what: approval.title.clone(),
        detail,
        whole: approval.raw_request.clone().filter(|_| approval.shortened),
        choice: match approval.questions.as_slice() {
            [only] => Some(only.clone()),
            _ => None,
        },
    }
}

pub(super) fn grant_changes(grants: Grants) -> Vec<(Grants, bool)> {
    let g = &grants;
    // Field by field: `apply_delta` would tidy the mask too, and keep control
    // in it only while watching stays.
    let rest = Grants {
        view: !g.view,
        control_input: !g.control_input,
        terminal: !g.terminal,
        tasks: !g.tasks,
        approvals: !g.approvals,
        computer_use: !g.computer_use,
        library_read: !g.library_read,
        library_write: !g.library_write,
        transfers: !g.transfers,
        admin: !g.admin,
        accounts: !g.accounts,
    };
    [(rest, false), (grants, true)]
        .into_iter()
        .filter(|(mask, _)| *mask != Grants::default())
        .collect()
}

fn home() -> Option<PathBuf> {
    directories::BaseDirs::new().map(|dirs| dirs.home_dir().to_owned())
}

pub(super) fn expand_home(folder: &str) -> PathBuf {
    match (
        folder.strip_prefix("~/").or(folder.strip_prefix("~\\")),
        home(),
    ) {
        (Some(rest), Some(home)) => home.join(rest),
        _ if folder == "~" => home().unwrap_or_else(|| folder.into()),
        _ => folder.into(),
    }
}

/// Where a program is, looking where its installer puts it as well as on
/// `PATH`: an installer run by setup changes no `PATH` setup can see.
fn program(name: &str) -> Option<PathBuf> {
    let mut folders: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect())
        .unwrap_or_default();
    if let Some(home) = home() {
        folders.extend(
            [
                ".local/bin",
                ".claude/local",
                ".cargo/bin",
                ".npm-global/bin",
                "AppData/Local/Programs",
                "AppData/Roaming/npm",
            ]
            .map(|folder| home.join(folder)),
        );
    }
    if cfg!(windows) {
        folders.push(PathBuf::from(r"C:\Program Files\Tailscale"));
    }
    let names: Vec<String> = if cfg!(windows) {
        ["exe", "cmd", "bat"]
            .iter()
            .map(|extension| format!("{name}.{extension}"))
            .collect()
    } else {
        vec![name.to_owned()]
    };
    folders
        .iter()
        .flat_map(|folder| names.iter().map(move |name| folder.join(name)))
        .find(|path| path.is_file())
}
