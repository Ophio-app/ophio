// SPDX-License-Identifier: Apache-2.0
//! Setup driven by a fake computer and scripted answers: nothing here
//! installs, signs in, starts or stops anything real.
use super::{
    Finished, Outcome, Pick, Progress, Start, Step,
    flow::{
        self, APP_DOWNLOAD_URL, Answer, Answered, Ask, Asked, Block, Choice, Ending, Host, Mark,
        Network, Run, Screen, Service, Survey, Tailscale, TaskView, Tick, Ui,
    },
    host::{self as real, expand_home, grant_changes, telling},
    plain::{Input, Lines, Plain},
};
use crate::{
    Error, Result,
    config::Paths,
    phone::{self, AlertsAllowed, Installed, Off, Opened, QrPairing},
};
use ports::COMMAND;
use protocol::{
    ErrorCode, ProtocolError, ProtocolVersion,
    accounts::SignInStage,
    agent_updates::AgentUpdatesView,
    auth::{DeviceRecord, Grants, PairingOffer},
    capability::{
        AccountReadiness, Availability, HarnessCapabilities, HarnessInfo, HarnessKind,
        IntegrationLevel,
    },
    checklist::{DeliveryFolder, FileDelivery, FileDeliverySettings, FileDeliveryView},
    command::ApprovalDecision,
    github::{GithubAction, GithubMode, GithubProjectView, GithubRepo, GithubSettings},
    ids::{ApprovalId, DeviceId, ProjectId, RunId, TaskId},
    launchers::{CatalogEntry, CatalogKind, CatalogSource},
    model::{
        ActivityStatus, Approval, ApprovalDetailLine, ApprovalKind, ApprovalLineChange,
        ApprovalQuestion, ApprovalQuestionOption, ApprovalState, CheckStatus, DeliveryState,
        ReadinessArea, ReadinessCheck, ReadinessReport, TaskState, TimelineBody, TimelineItem,
    },
    time::Timestamp,
};
use services::api::PairingPrompt;
use std::{
    cell::Cell,
    collections::VecDeque,
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    path::{Path, PathBuf},
    time::Duration,
};

mod tui;

const CLOCK: i64 = 1_900_000_000_000;
const TAILNET: IpAddr = IpAddr::V4(Ipv4Addr::new(100, 64, 0, 1));
const WIFI: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 20));

/// Values given out in turn, the last one again and again.
struct Script<T>(VecDeque<T>);
impl<T: Clone> Script<T> {
    fn of(values: impl IntoIterator<Item = T>) -> Self {
        Self(values.into_iter().collect())
    }
    fn next(&mut self) -> T {
        if self.0.len() > 1 {
            self.0.pop_front().expect("a value")
        } else {
            self.0.front().cloned().expect("an empty script")
        }
    }
    fn last(&self) -> T {
        self.0.back().cloned().expect("an empty script")
    }
    fn set(&mut self, value: T) {
        self.0 = VecDeque::from([value]);
    }
}

/// A computer as setup sees it, which records what setup did to it.
struct Fake {
    /// What setup did, in order.
    calls: Vec<String>,
    saved: Vec<Progress>,
    survey: Survey,
    service: Service,
    networks: Script<Network>,
    agents: Script<Vec<HarnessInfo>>,
    stages: Script<Option<SignInStage>>,
    /// Where pairing codes say this computer is.
    addresses: Vec<String>,
    /// Where they say it is once Øphio restarts.
    restarted: Option<Vec<String>>,
    /// How long a pairing code lasts, in milliseconds.
    offer_life: i64,
    waiting: Script<Option<PairingPrompt>>,
    /// What answering a pairing comes to; otherwise what was answered.
    answers: VecDeque<Answered>,
    /// What a device paired here may do at first.
    paired_grants: Grants,
    devices: Script<Vec<DeviceRecord>>,
    granted: Vec<Grants>,
    /// Whether each program run ends well; well when none are left.
    runs: VecDeque<bool>,
    reports: Script<ReadinessReport>,
    /// Calls that fail, once each.
    fail: Vec<&'static str>,
    clock: Cell<i64>,
    /// The phone app that came with this download.
    phone_app: Option<PathBuf>,
    /// Whether `adb` is found, look by look.
    adb: Script<bool>,
    /// The phones `adb` reaches.
    phones: Vec<phone::Device>,
    /// The phone that scanned the pairing QR code, look by look.
    scanned: Script<Option<phone::Device>>,
    installs: Script<Installed>,
    opened: Opened,
    /// A setup task, look by look.
    views: Script<TaskView>,
    /// How turning Wireless debugging off ends.
    off: Off,
    /// The Git projects and their GitHub workflows, look by look.
    github: Script<Vec<(String, GithubProjectView)>>,
    /// Whether Øphio keeps the agents up to date; none from an Øphio that
    /// can't.
    updates: Option<AgentUpdatesView>,
    /// What turning that on or off is refused with, once.
    updates_refused: Option<ErrorCode>,
    /// Where a checklist's files go; none from an Øphio before checklists.
    delivery: Option<FileDeliveryView>,
}

impl Default for Fake {
    fn default() -> Self {
        Self {
            calls: Vec::new(),
            saved: Vec::new(),
            survey: Survey {
                name: "desk".into(),
                claude: true,
                codex: false,
                tailscale: true,
                npm: true,
            },
            service: Service {
                running: true,
                at_login: true,
            },
            networks: Script::of([network(
                Tailscale::Running {
                    ips: vec![TAILNET],
                    name: Some("desk.tail.ts.net".into()),
                },
                false,
                &[IpAddr::V4(Ipv4Addr::LOCALHOST), TAILNET],
            )]),
            agents: Script::of([vec![agent(HarnessKind::ClaudeCode, Some(signed_in()))]]),
            stages: Script::of([None]),
            addresses: vec!["100.64.0.1:47100".into(), "desk.tail.ts.net:47100".into()],
            restarted: None,
            offer_life: 600_000,
            waiting: Script::of([None]),
            answers: VecDeque::new(),
            paired_grants: Grants::default(),
            devices: Script::of([Vec::new()]),
            granted: Vec::new(),
            runs: VecDeque::new(),
            reports: Script::of([report(&[
                ("Tailscale", CheckStatus::Passed),
                ("Terminal", CheckStatus::Passed),
            ])]),
            fail: Vec::new(),
            clock: Cell::new(CLOCK),
            phone_app: None,
            adb: Script::of([true]),
            phones: Vec::new(),
            scanned: Script::of([None]),
            installs: Script::of([Installed::Done]),
            opened: Opened::Tailscale,
            views: Script::of([view(TaskState::Completed)]),
            off: Off::Done,
            github: Script::of([Vec::new()]),
            updates: None,
            updates_refused: None,
            delivery: None,
        }
    }
}

impl Fake {
    /// A look that changes nothing; fails if asked to.
    fn look(&mut self, name: &str) -> Result<()> {
        match self.fail.iter().position(|failing| *failing == name) {
            Some(at) => {
                self.fail.remove(at);
                Err(Error::Config(format!("{name} broke")))
            }
            None => Ok(()),
        }
    }
    /// Something done, which is recorded.
    fn act(&mut self, call: String) -> Result<()> {
        let name = call.split(' ').next().unwrap_or_default().to_owned();
        self.calls.push(call);
        self.look(&name)
    }
    fn progress(&self) -> &Progress {
        self.saved.last().expect("progress saved")
    }
}

impl Host for Fake {
    fn save(&mut self, progress: &Progress) -> Result<()> {
        self.saved.push(progress.clone());
        Ok(())
    }
    fn survey(&mut self) -> Survey {
        self.survey.clone()
    }
    async fn service(&mut self) -> Result<Service> {
        self.look("service")?;
        Ok(self.service)
    }
    async fn install_service(&mut self) -> Result<Option<String>> {
        self.act("install_service".into())?;
        self.service = Service {
            running: true,
            at_login: true,
        };
        Ok(None)
    }
    async fn start(&mut self) -> Result<()> {
        self.act("start".into())?;
        self.service.running = true;
        Ok(())
    }
    async fn restart(&mut self) -> Result<()> {
        self.act("restart".into())?;
        if let Some(addresses) = self.restarted.take() {
            self.addresses = addresses;
        }
        Ok(())
    }
    async fn network(&mut self) -> Result<Network> {
        self.look("network")?;
        Ok(self.networks.next())
    }
    async fn allow_wifi(&mut self, ip: IpAddr) -> Result<()> {
        self.act(format!("allow_wifi {ip}"))?;
        let mut network = self.networks.last();
        network.allow_lan = true;
        network.listen.push(ip);
        self.networks.set(network);
        Ok(())
    }
    async fn agents(&mut self) -> Result<Vec<HarnessInfo>> {
        self.look("agents")?;
        Ok(self.agents.next())
    }
    async fn agent_updates(&mut self) -> Result<Option<AgentUpdatesView>> {
        self.look("agent_updates")?;
        Ok(self.updates.clone())
    }
    async fn set_agent_updates(&mut self, enabled: bool, revision: u64) -> Result<()> {
        self.act(format!("set_agent_updates {enabled} {revision}"))?;
        if let Some(code) = self.updates_refused.take() {
            return Err(Error::Protocol(ProtocolError::new(code, "refused")));
        }
        let view = self.updates.as_mut().expect("asked only when it can");
        assert_eq!(view.revision, revision);
        view.enabled = enabled;
        view.revision += 1;
        Ok(())
    }
    async fn sign_in(&mut self, account_id: &str) -> Result<()> {
        self.act(format!("sign_in {account_id}"))
    }
    async fn sign_in_stage(
        &mut self,
        _account_id: &str,
        since: Timestamp,
    ) -> Result<Option<SignInStage>> {
        assert!(since.0 <= self.clock.get());
        self.look("sign_in_stage")?;
        Ok(self.stages.next())
    }
    async fn cancel_sign_in(&mut self) -> Result<()> {
        self.act("cancel_sign_in".into())?;
        self.stages.set(Some(SignInStage::Cancelled));
        Ok(())
    }
    async fn open_pairing(&mut self) -> Result<PairingOffer> {
        self.act("open_pairing".into())?;
        Ok(offer(
            self.addresses.clone(),
            self.clock.get() + self.offer_life,
        ))
    }
    async fn waiting_pairing(&mut self) -> Result<Option<PairingPrompt>> {
        self.look("waiting_pairing")?;
        // A second passes between looks.
        self.clock.set(self.clock.get() + 1_000);
        Ok(self.waiting.next())
    }
    async fn answer_pairing(&mut self, code: &str, confirm: bool) -> Result<Answered> {
        self.act(format!("answer_pairing {code} {confirm}"))?;
        self.waiting.set(None);
        let answered = self.answers.pop_front().unwrap_or_else(|| {
            if confirm {
                Answered::Paired(device("Pixel 9", self.paired_grants.clone()))
            } else {
                Answered::Refused
            }
        });
        if let Answered::Paired(device) = &answered {
            let mut devices = self.devices.last();
            devices.push(device.clone());
            self.devices.set(devices);
        }
        Ok(answered)
    }
    async fn devices(&mut self) -> Result<Vec<DeviceRecord>> {
        self.look("devices")?;
        Ok(self.devices.next())
    }
    async fn set_grants(&mut self, _device: DeviceId, grants: Grants) -> Result<()> {
        self.act("set_grants".into())?;
        self.granted.push(grants);
        Ok(())
    }
    async fn add_project(&mut self, folder: &str, agent: Option<HarnessKind>) -> Result<String> {
        self.act(format!("add_project {folder} {agent:?}"))?;
        match folder {
            "/missing" => Err(Error::Ended("There is no folder at /missing.".into())),
            "/broken" => Err(Error::Config("the library broke".into())),
            _ => Ok(folder.into()),
        }
    }
    async fn run(&mut self, run: &Run) -> Result<bool> {
        let shown = match run {
            Run::Ophio(args) => format!("ophio {}", args.join(" ")),
            Run::Program { shown, .. } => shown.clone(),
        };
        self.act(format!("run {shown}"))?;
        Ok(self.runs.pop_front().unwrap_or(true))
    }
    async fn readiness(&mut self) -> Result<ReadinessReport> {
        self.look("readiness")?;
        Ok(self.reports.next())
    }
    fn now(&self) -> Timestamp {
        Timestamp(self.clock.get())
    }
    // Fixed, so the screens read the same in any time zone.
    fn local_offset(&self, _: time::OffsetDateTime) -> Option<time::UtcOffset> {
        time::UtcOffset::from_hms(-4, 0, 0).ok()
    }
    fn config_file(&self) -> PathBuf {
        PathBuf::from("/home/you/.config/ophio/config.toml")
    }
    fn phone_app(&self) -> Option<PathBuf> {
        self.phone_app.clone()
    }
    fn adb(&mut self) -> bool {
        self.adb.next()
    }
    async fn phones(&mut self) -> Result<Vec<phone::Device>> {
        self.look("phones")?;
        Ok(self.phones.clone())
    }
    fn adb_pairing(&mut self) -> Result<QrPairing> {
        Ok(QrPairing::fixed("ophio-test", "password"))
    }
    async fn adb_pair_qr(&mut self, pairing: &QrPairing) -> Result<Option<phone::Device>> {
        assert_eq!(pairing.code(), "WIFI:T:ADB;S:ophio-test;P:password;;");
        self.look("adb_pair_qr")?;
        let scanned = self.scanned.next();
        if let Some(device) = &scanned {
            self.calls.push(format!("adb_pair_qr {}", device.serial));
        }
        Ok(scanned)
    }
    async fn adb_pair_code(&mut self, address: Option<&str>, code: &str) -> Result<phone::Device> {
        self.act(format!("adb_pair_code {address:?} {code}"))?;
        Ok(phone_device("192.168.1.30:40000"))
    }
    async fn phone_install(&mut self, device: &phone::Device, app: &Path) -> Result<Installed> {
        self.act(format!("phone_install {} {}", device.serial, app.display()))?;
        Ok(self.installs.next())
    }
    async fn phone_open(&mut self, device: &phone::Device, app: phone::Open) -> Result<Opened> {
        self.act(format!("phone_open {} {app:?}", device.serial))?;
        Ok(self.opened)
    }
    async fn phone_link(&mut self, device: &phone::Device, link: &str) -> Result<()> {
        assert!(link.starts_with("ophio://pair?") && link.contains("&alerts="));
        self.act(format!("phone_link {}", device.serial))
    }
    async fn phone_alerts(&mut self, device: &phone::Device) -> Result<AlertsAllowed> {
        self.act(format!("phone_alerts {}", device.serial))?;
        Ok(AlertsAllowed {
            notifications: true,
            battery: true,
        })
    }
    async fn phone_off(&mut self, device: &phone::Device) -> Result<Off> {
        self.act(format!("phone_off {}", device.serial))?;
        Ok(self.off.clone())
    }
    async fn setup_skill(&mut self, id: &str) -> Result<Option<CatalogEntry>> {
        self.look("setup_skill")?;
        Ok(id.starts_with("setup-").then(|| CatalogEntry {
            kind: CatalogKind::Skill,
            source: CatalogSource::OphioSetup,
            id: id.into(),
            name: format!("Skill {id}"),
            description: Some(format!("What {id} does.")),
            digest: "d1g35t".into(),
            host_path: format!("/data/setup-skills/{id}/SKILL.md"),
            library: None,
            harness: None,
            model: None,
            effort: None,
            launch_policy: None,
            copy: None,
            disable: None,
        }))
    }
    async fn start_setup_task(
        &mut self,
        skill: &CatalogEntry,
        agent: HarnessKind,
        prompt: &str,
    ) -> Result<TaskId> {
        self.act(format!("start_setup_task {} {agent:?} {prompt}", skill.id))?;
        Ok(TaskId::new())
    }
    async fn task_view(&mut self, _task: TaskId) -> Result<TaskView> {
        self.look("task_view")?;
        Ok(self.views.next())
    }
    async fn tell_task(&mut self, _task: TaskId, text: &str) -> Result<()> {
        self.act(format!("tell {text}"))?;
        if text == "refused" {
            return Err(ProtocolError::new(ErrorCode::Conflict, "The task is archived.").into());
        }
        Ok(())
    }
    async fn decide(
        &mut self,
        _approval: ApprovalId,
        revision: u32,
        decision: ApprovalDecision,
    ) -> Result<()> {
        self.act(format!("decide {decision:?} at {revision}"))
    }
    async fn stop_task(&mut self, _task: TaskId) -> Result<()> {
        self.act("stop".into())
    }
    async fn github(&mut self) -> Result<Vec<(String, GithubProjectView)>> {
        self.look("github")?;
        Ok(self.github.next())
    }
    async fn file_delivery(&mut self) -> Result<Option<FileDeliveryView>> {
        self.look("file_delivery")?;
        Ok(self.delivery.clone())
    }
    async fn set_file_delivery(&mut self, delivery: FileDelivery, revision: u64) -> Result<()> {
        self.act(format!("set_file_delivery {delivery:?} {revision}"))?;
        if let Some(view) = &mut self.delivery {
            view.settings = FileDeliverySettings {
                delivery,
                revision: revision + 1,
            };
        }
        Ok(())
    }
    async fn github_action(&mut self, action: GithubAction) -> Result<()> {
        self.act(match action {
            GithubAction::SetSettings { settings, .. } => {
                format!("github_mode {:?}", settings.mode)
            }
            GithubAction::AnswerWarning {
                repo,
                keep_through_merge,
                ..
            } => format!("github_warning {repo} keep={keep_through_merge}"),
            other => format!("github {other:?}"),
        })
    }
}

/// A setup task that has said and done nothing yet.
fn view(state: TaskState) -> TaskView {
    TaskView {
        state,
        status: None,
        said: None,
        doing: Vec::new(),
        asks: None,
    }
}

/// A setup task asking to run `command`.
fn asking(command: &str) -> TaskView {
    TaskView {
        asks: Some(Asked {
            id: ApprovalId::new(),
            revision: 7,
            question: false,
            what: "Run a command".into(),
            detail: vec![(true, command.into())],
            whole: None,
            choice: None,
        }),
        ..view(TaskState::AwaitingDecision)
    }
}

/// A setup task asking which of `options` to pick.
fn question(options: &[&str], multi_select: bool) -> TaskView {
    TaskView {
        asks: Some(Asked {
            id: ApprovalId::new(),
            revision: 3,
            question: true,
            what: "A question".into(),
            detail: vec![(false, "Which?".into())],
            whole: None,
            choice: Some(ApprovalQuestion {
                id: None,
                header: None,
                question: "Which folders?".into(),
                options: options
                    .iter()
                    .map(|label| ApprovalQuestionOption {
                        label: (*label).into(),
                        description: None,
                    })
                    .collect(),
                multi_select,
                secret: false,
            }),
        }),
        ..view(TaskState::AwaitingDecision)
    }
}

/// A phone `adb` reaches, at `serial`.
fn phone_device(serial: &str) -> phone::Device {
    phone::Device {
        serial: serial.into(),
        state: "device".into(),
        model: Some("Pixel_9".into()),
    }
}

fn network(tailscale: Tailscale, allow_lan: bool, listen: &[IpAddr]) -> Network {
    Network {
        tailscale,
        wifi: Some(WIFI),
        allow_lan,
        listen: listen.to_vec(),
        port: 47100,
    }
}

fn signed_in() -> AccountReadiness {
    AccountReadiness::SignedIn {
        account_label: Some("me@example.com".into()),
        plan: Some("Max".into()),
    }
}
fn signed_out() -> AccountReadiness {
    AccountReadiness::SignedOut {
        remedy: "Run `claude` and type /login.".into(),
    }
}

/// An agent; not installed without an account.
fn agent(kind: HarnessKind, account: Option<AccountReadiness>) -> HarnessInfo {
    let a = Availability::Available;
    HarnessInfo {
        kind,
        display_name: format!("{kind:?}"),
        native_version: None,
        integration: IntegrationLevel::Managed,
        installed: if account.is_some() {
            a.clone()
        } else {
            Availability::Unsupported {
                reason: "Not installed".into(),
            }
        },
        account: account.unwrap_or(AccountReadiness::Unknown {
            reason: "Not installed".into(),
        }),
        models: Vec::new(),
        efforts: Vec::new(),
        capabilities: HarnessCapabilities {
            start_run: a.clone(),
            steer_running: a.clone(),
            interrupt: a.clone(),
            native_approvals: a.clone(),
            resume_session: a.clone(),
            child_agents: a.clone(),
            computer_use_delegation: a.clone(),
            model_selection: a.clone(),
            effort_selection: a,
            cancel_queued_message: None,
        },
        modes: Vec::new(),
        catalog: None,
        usage_limit: None,
    }
}

fn device(name: &str, grants: Grants) -> DeviceRecord {
    DeviceRecord {
        device_id: DeviceId::new(),
        name: name.into(),
        platform: "android".into(),
        paired_at: Timestamp(CLOCK - 60_000),
        last_seen_at: None,
        revoked_at: None,
        grants,
        is_current: false,
    }
}

fn prompt(shows_code: bool) -> PairingPrompt {
    PairingPrompt {
        device_name: "Pixel 9".into(),
        device_platform: "android".into(),
        code: "123456".into(),
        shows_code,
        expires_at: Timestamp(CLOCK + 300_000),
        replaces: None,
    }
}

fn offer(addresses: Vec<String>, expires_at: i64) -> PairingOffer {
    PairingOffer {
        host_id: "0f8b6c1e-2d3a-4b5c-8d9e-0a1b2c3d4e5f".parse().unwrap(),
        host_name: "desk".into(),
        addresses,
        cert_sha256: "5f1c9a0e7d3b2a4c6e8f0a1b3c5d7e9f2a4b6c8d0e1f3a5b7c9d1e3f5a7b9c0d".into(),
        pairing_secret: "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8".into(),
        expires_at: Timestamp(expires_at),
        protocol: ProtocolVersion::CURRENT,
    }
}

fn report(checks: &[(&str, CheckStatus)]) -> ReadinessReport {
    ReadinessReport {
        checked_at: Timestamp(CLOCK),
        checks: checks
            .iter()
            .map(|&(label, status)| ReadinessCheck {
                area: ReadinessArea::Network,
                label: label.into(),
                status,
                detail: None,
                remedy: None,
                summary: None,
            })
            .collect(),
    }
}

/// Answers in turn, each given to the first screen it fits; a screen that
/// waits for something is told its wait ran out until one fits.
#[derive(Default)]
struct Scripted {
    answers: VecDeque<Answer>,
    /// Every screen shown, once each time it changed.
    screens: Vec<Screen>,
    lent: Vec<&'static str>,
    polls: usize,
}

impl Ui for Scripted {
    async fn ask(&mut self, screen: &Screen, wait: Option<Duration>) -> Answer {
        if self.screens.last() != Some(screen) {
            self.screens.push(screen.clone());
        }
        if screen.ask == Ask::Nothing {
            return Answer::Waited;
        }
        // A scripted wait lets a screen that would take the next answer
        // time out first.
        if wait.is_some() && self.answers.front() == Some(&Answer::Waited) {
            return self.answers.pop_front().expect("an answer");
        }
        if self
            .answers
            .front()
            .is_some_and(|answer| fits(screen, answer))
        {
            return self.answers.pop_front().expect("an answer");
        }
        assert!(
            wait.is_some(),
            "no answer for {}; next is {:?}",
            words(screen),
            self.answers.front()
        );
        self.polls += 1;
        assert!(self.polls < 500, "waited too long at {}", words(screen));
        Answer::Waited
    }
    async fn suspend(&mut self) {
        self.lent.push("suspend");
    }
    async fn resume(&mut self) {
        self.lent.push("resume");
    }
}

fn fits(screen: &Screen, answer: &Answer) -> bool {
    match (answer, &screen.ask) {
        (Answer::Quit, _) => true,
        (Answer::Choice(id), _) => screen.choices().iter().any(|choice| choice.id == *id),
        (Answer::Text(_), Ask::Text { .. }) | (Answer::Ticks(_), Ask::Ticks { .. }) => true,
        _ => false,
    }
}

/// A screen's words, to look things up in.
fn words(screen: &Screen) -> String {
    let mut words = vec![screen.title.clone()];
    for block in &screen.blocks {
        words.push(match block {
            Block::Text(text) | Block::Command(text) => text.clone(),
            Block::Fact { label, detail, .. } => format!("{label}: {detail}"),
            Block::Qr { data, caption } => format!("{data} {caption}"),
            Block::Link { label, url } => format!("{label}: {url}"),
            Block::Steps(steps) => steps.join("\n"),
        });
    }
    for choice in screen.choices() {
        words.push(choice.label.clone());
        words.extend(choice.detail.clone());
    }
    words.join("\n")
}

impl Scripted {
    fn saw(&self, needle: &str) -> bool {
        self.screens
            .iter()
            .any(|screen| words(screen).contains(needle))
    }
    fn screen(&self, needle: &str) -> &Screen {
        self.screens
            .iter()
            .find(|screen| words(screen).contains(needle))
            .unwrap_or_else(|| panic!("no screen says {needle:?}"))
    }
}

fn c(id: &'static str) -> Answer {
    Answer::Choice(id)
}
fn text(text: &str) -> Answer {
    Answer::Text(text.into())
}

/// Goes through setup, checking every answer was used.
async fn go(host: &mut Fake, start: Start, answers: Vec<Answer>) -> (Result<Ending>, Scripted) {
    let mut ui = Scripted {
        answers: answers.into(),
        ..Scripted::default()
    };
    let ending = flow::run(host, &mut ui, start).await;
    assert!(ui.answers.is_empty(), "answers left: {:?}", ui.answers);
    (ending, ui)
}

/// A setup left part way, every step before `step` done.
fn at(step: Step) -> Start {
    let mut progress = Progress::default();
    for done in Step::NUMBERED.into_iter().take_while(|done| *done != step) {
        progress.steps.insert(done, Outcome::Done);
    }
    Start::Resume(progress)
}

#[tokio::test]
async fn a_first_setup_goes_through_every_step() {
    let mut host = Fake {
        service: Service {
            running: false,
            at_login: false,
        },
        waiting: Script::of([None, None, Some(prompt(true))]),
        ..Fake::default()
    };
    let answers = vec![
        c("start"),
        c("login"),
        c("next"),
        c("next"),
        c("next"),
        c("have"),
        c("on"),
        c("pair"),
        c("full"),
        c("next"),
        Answer::Ticks(Vec::new()),
        c("next"),
        c("finish"),
    ];
    let (ending, ui) = go(&mut host, Start::First(Progress::default()), answers).await;
    assert_eq!(ending.unwrap(), Ending::Done);
    assert_eq!(
        host.calls,
        [
            "install_service",
            "open_pairing",
            "answer_pairing 123456 true",
            "set_grants"
        ]
    );
    assert_eq!(host.granted, [Grants::owner()]);
    let progress = host.progress();
    assert_eq!(progress.finished, Some(Finished::Done));
    assert!(
        Step::NUMBERED
            .iter()
            .all(|step| progress.steps.get(step) == Some(&Outcome::Done))
    );
    assert_eq!(progress.alerts, Some(true));
    assert_eq!(progress.agent, Some(HarnessKind::ClaudeCode));
    // Saved after every step, so a setup left part way carries on.
    assert!(host.saved[0].steps.contains_key(&Step::Background));
    assert!(host.saved.len() > Step::NUMBERED.len());
    // The phone learns whether it gets alerts from the code it scans.
    let waiting = ui.screen("Waiting for your phone");
    let qr = waiting.blocks.iter().find_map(|block| match block {
        Block::Qr { data, .. } => Some(data.clone()),
        _ => None,
    });
    assert!(qr.unwrap().ends_with("&alerts=on"));
    assert!(words(waiting).contains("&alerts=on"));
    assert!(ui.saw("Everything checked works."));
    assert!(ui.saw("2 checks passed"));
    // Each screen is in its step, in order.
    let steps: Vec<Step> = ui.screens.iter().map(|screen| screen.step).collect();
    let mut sorted = steps.clone();
    sorted.sort();
    assert_eq!(steps, sorted);
}

#[tokio::test]
async fn skipping_at_the_start_records_it() {
    let mut host = Fake::default();
    let (ending, ui) = go(
        &mut host,
        Start::First(Progress::default()),
        vec![c("skip")],
    )
    .await;
    assert_eq!(ending.unwrap(), Ending::Skipped);
    assert_eq!(host.progress().finished, Some(Finished::Skipped));
    assert!(host.calls.is_empty());
    assert!(ui.saw("Claude Code: Found"));
    assert!(ui.saw("Codex: Not found"));
}

#[tokio::test]
async fn quitting_pauses_a_first_setup() {
    let mut host = Fake::default();
    let (ending, _) = go(
        &mut host,
        Start::First(Progress::default()),
        vec![Answer::Quit],
    )
    .await;
    assert_eq!(ending.unwrap(), Ending::Paused);
    assert_eq!(host.progress().finished, None);
}

#[tokio::test]
async fn a_setup_left_part_way_carries_on_where_it_was() {
    let mut host = Fake::default();
    let mut progress = Progress::default();
    progress.steps.insert(Step::Background, Outcome::Done);
    progress.steps.insert(Step::Reach, Outcome::Later);
    let (ending, ui) = go(
        &mut host,
        Start::Resume(progress),
        vec![c("start"), Answer::Quit],
    )
    .await;
    assert_eq!(ending.unwrap(), Ending::Paused);
    assert!(ui.saw("2 of 6 steps are behind you"));
    assert!(ui.saw("Carry on"));
    assert_eq!(ui.screens[1].step, Step::Agent);
    let progress = host.progress();
    assert_eq!(progress.steps.get(&Step::Reach), Some(&Outcome::Later));
    assert_eq!(progress.finished, None);
}

#[tokio::test]
async fn going_through_again_shows_every_step_and_undoes_nothing() {
    let mut host = Fake {
        service: Service {
            running: true,
            at_login: false,
        },
        ..Fake::default()
    };
    let mut progress = Progress {
        finished: Some(Finished::Done),
        ..Progress::default()
    };
    for step in Step::NUMBERED {
        progress.steps.insert(step, Outcome::Done);
    }
    let (ending, ui) = go(
        &mut host,
        Start::Rerun(progress),
        vec![c("start"), c("later"), c("skip")],
    )
    .await;
    assert_eq!(ending.unwrap(), Ending::Stopped);
    assert!(ui.saw("every step is shown again"));
    assert_eq!(ui.screens[1].step, Step::Background);
    let progress = host.progress();
    assert_eq!(progress.steps.get(&Step::Background), Some(&Outcome::Done));
    assert_eq!(progress.finished, Some(Finished::Done));
}

#[tokio::test]
async fn just_for_now_starts_ophio_and_leaves_the_step_for_later() {
    let mut host = Fake {
        service: Service {
            running: false,
            at_login: false,
        },
        ..Fake::default()
    };
    let (_, ui) = go(
        &mut host,
        at(Step::Background),
        vec![c("start"), c("now"), Answer::Quit],
    )
    .await;
    assert_eq!(host.calls, ["start"]);
    assert_eq!(
        host.progress().steps.get(&Step::Background),
        Some(&Outcome::Later)
    );
    assert!(ui.saw("Now: Not running"));
}

#[tokio::test]
async fn ophio_already_running_at_login_needs_nothing() {
    let mut host = Fake::default();
    let (_, ui) = go(
        &mut host,
        at(Step::Background),
        vec![c("start"), c("next"), Answer::Quit],
    )
    .await;
    assert!(host.calls.is_empty());
    assert!(ui.saw("Running now, and starts whenever you log in."));
    assert!(
        ui.screens[1]
            .choices()
            .iter()
            .all(|choice| choice.id != "login")
    );
    assert_eq!(
        host.progress().steps.get(&Step::Background),
        Some(&Outcome::Done)
    );
}

#[tokio::test]
async fn wifi_is_allowed_once_agreed() {
    let mut host = Fake {
        networks: Script::of([network(
            Tailscale::Missing,
            false,
            &[IpAddr::V4(Ipv4Addr::LOCALHOST)],
        )]),
        ..Fake::default()
    };
    let answers = vec![c("start"), c("wifi"), c("allow"), c("next"), Answer::Quit];
    let (_, ui) = go(&mut host, at(Step::Reach), answers).await;
    assert_eq!(host.calls, ["allow_wifi 192.168.1.20"]);
    assert!(ui.saw("Tailscale: Not installed on this computer"));
    assert!(ui.saw("/home/you/.config/ophio/config.toml"));
    assert!(ui.saw("Phones on this network can reach 192.168.1.20, port 47100"));
    assert_eq!(
        host.progress().steps.get(&Step::Reach),
        Some(&Outcome::Done)
    );
}

#[tokio::test]
async fn going_back_from_wifi_allows_nothing() {
    let mut host = Fake {
        networks: Script::of([network(Tailscale::Stopped, false, &[])]),
        ..Fake::default()
    };
    let answers = vec![c("start"), c("wifi"), c("back"), c("later"), Answer::Quit];
    let (_, ui) = go(&mut host, at(Step::Reach), answers).await;
    assert!(host.calls.is_empty());
    assert!(ui.saw("Installed, not connected"));
    assert_eq!(
        host.progress().steps.get(&Step::Reach),
        Some(&Outcome::Later)
    );
}

#[tokio::test]
async fn wifi_counts_as_allowed_when_listening_everywhere_on_its_family() {
    for (listen, allow_lan, allowed) in [
        (IpAddr::V4(Ipv4Addr::UNSPECIFIED), true, true),
        (WIFI, true, true),
        (IpAddr::V4(Ipv4Addr::UNSPECIFIED), false, false),
        (IpAddr::V6(Ipv6Addr::UNSPECIFIED), true, false),
    ] {
        let mut host = Fake {
            networks: Script::of([network(Tailscale::Stopped, allow_lan, &[listen])]),
            ..Fake::default()
        };
        let (_, ui) = go(&mut host, at(Step::Reach), vec![c("start"), Answer::Quit]).await;
        let reach = &ui.screens[1];
        let offered = reach.choices().iter().any(|choice| choice.id == "wifi");
        assert_eq!(offered, !allowed, "{listen} {allow_lan}");
        assert_eq!(words(reach).contains("Use this Wi‑Fi"), allowed);
    }
}

#[tokio::test]
async fn checking_again_sees_tailscale_come_up() {
    let mut host = Fake {
        networks: Script::of([
            network(Tailscale::Missing, false, &[]),
            network(
                Tailscale::Running {
                    ips: vec![TAILNET],
                    name: None,
                },
                false,
                &[],
            ),
        ]),
        ..Fake::default()
    };
    let answers = vec![c("start"), c("check"), c("next"), Answer::Quit];
    let (_, ui) = go(&mut host, at(Step::Reach), answers).await;
    assert!(ui.saw("Connected as 100.64.0.1"));
    assert_eq!(
        host.progress().steps.get(&Step::Reach),
        Some(&Outcome::Done)
    );
}

#[tokio::test]
async fn an_agent_is_installed_and_signed_in() {
    let claude = HarnessKind::ClaudeCode;
    let mut host = Fake {
        agents: Script::of([
            vec![agent(claude, None), agent(HarnessKind::Codex, None)],
            vec![agent(claude, Some(signed_out()))],
            vec![agent(claude, Some(signed_in()))],
        ]),
        stages: Script::of([
            None,
            Some(SignInStage::Browser {
                url_host: "claude.ai".into(),
            }),
            Some(SignInStage::Code {
                code: "ABCD-1234".into(),
                url_host: "claude.ai".into(),
            }),
            Some(SignInStage::Done {
                identity: Some("me@example.com".into()),
            }),
        ]),
        ..Fake::default()
    };
    let answers = vec![
        c("start"),
        c("install-claude"),
        c("run"),
        c("next"),
        c("sign-in-claude"),
        c("next"),
        c("next"),
        Answer::Quit,
    ];
    let (_, ui) = go(&mut host, at(Step::Agent), answers).await;
    assert_eq!(host.calls.len(), 2);
    assert!(host.calls[0].starts_with("run ") && host.calls[0].contains("claude.ai/install"));
    assert_eq!(host.calls[1], "sign_in claude");
    assert_eq!(ui.lent, ["suspend", "resume"]);
    let first = &ui.screens[1];
    let next = first
        .choices()
        .iter()
        .find(|choice| choice.id == "next")
        .unwrap();
    assert!(!next.recommended);
    assert!(words(first).contains("Install Codex"));
    assert!(ui.saw("Claude Code: Installed"));
    assert!(ui.saw("Sign in on claude.ai in this computer's browser."));
    assert!(ui.saw("Enter ABCD-1234 on claude.ai"));
    assert!(ui.saw("Signed in as me@example.com"));
    let progress = host.progress();
    assert_eq!(progress.steps.get(&Step::Agent), Some(&Outcome::Done));
    assert_eq!(progress.agent, Some(claude));
}

#[tokio::test]
async fn codex_without_npm_points_at_node() {
    let mut host = Fake {
        survey: Survey {
            npm: false,
            ..Fake::default().survey
        },
        ..Fake::default()
    };
    let answers = vec![
        c("start"),
        c("install-codex"),
        c("check"),
        c("next"),
        Answer::Quit,
    ];
    let (_, ui) = go(&mut host, at(Step::Agent), answers).await;
    assert!(host.calls.is_empty());
    assert!(ui.lent.is_empty());
    assert!(ui.saw("https://nodejs.org/en/download"));
}

#[tokio::test]
async fn an_installer_that_fails_is_said_to() {
    let mut host = Fake {
        agents: Script::of([vec![agent(HarnessKind::ClaudeCode, None)]]),
        runs: VecDeque::from([false]),
        ..Fake::default()
    };
    let answers = vec![
        c("start"),
        c("install-claude"),
        c("run"),
        c("next"),
        c("next"),
        Answer::Quit,
    ];
    let (_, ui) = go(&mut host, at(Step::Agent), answers).await;
    assert!(ui.saw("The installer stopped before it finished"));
    // With no agent ready, going on leaves the step for later.
    assert_eq!(
        host.progress().steps.get(&Step::Agent),
        Some(&Outcome::Later)
    );
    assert_eq!(host.progress().agent, None);
}

#[tokio::test]
async fn a_sign_in_can_be_cancelled() {
    let mut host = Fake {
        agents: Script::of([vec![agent(HarnessKind::ClaudeCode, Some(signed_out()))]]),
        stages: Script::of([Some(SignInStage::Browser {
            url_host: "claude.ai".into(),
        })]),
        ..Fake::default()
    };
    let answers = vec![
        c("start"),
        c("sign-in-claude"),
        c("cancel"),
        c("next"),
        c("next"),
        Answer::Quit,
    ];
    let (_, ui) = go(&mut host, at(Step::Agent), answers).await;
    assert_eq!(host.calls, ["sign_in claude", "cancel_sign_in"]);
    assert!(ui.saw("Sign-in cancelled"));
    assert!(ui.saw("You can also sign in yourself: Run `claude` and type /login."));
}

#[tokio::test]
async fn a_sign_in_that_never_starts_is_given_up() {
    let mut host = Fake {
        agents: Script::of([vec![agent(HarnessKind::ClaudeCode, Some(signed_out()))]]),
        ..Fake::default()
    };
    let answers = vec![
        c("start"),
        c("sign-in-claude"),
        c("next"),
        c("later"),
        Answer::Quit,
    ];
    let (_, ui) = go(&mut host, at(Step::Agent), answers).await;
    assert!(ui.saw("Claude Code: The sign-in didn't start."));
    assert_eq!(ui.polls, 30);
}

#[tokio::test]
async fn new_projects_start_with_the_agent_chosen() {
    let mut host = Fake {
        agents: Script::of([vec![
            agent(HarnessKind::ClaudeCode, Some(signed_in())),
            agent(HarnessKind::Codex, Some(signed_in())),
        ]]),
        ..Fake::default()
    };
    let answers = vec![
        c("start"),
        c("default"),
        c("codex"),
        c("next"),
        Answer::Quit,
    ];
    let (_, ui) = go(&mut host, at(Step::Agent), answers).await;
    assert!(ui.saw("New projects start with: Claude Code"));
    assert!(ui.saw("New projects start with: Codex"));
    let default = ui.screen("Which agent should new projects start with?");
    let claude = default
        .choices()
        .iter()
        .find(|choice| choice.id == "claude")
        .unwrap();
    assert!(claude.recommended);
    assert_eq!(host.progress().agent, Some(HarnessKind::Codex));
}

#[tokio::test]
async fn an_agent_no_longer_ready_is_not_kept_as_the_default() {
    let mut host = Fake {
        agents: Script::of([vec![
            agent(HarnessKind::ClaudeCode, Some(signed_in())),
            agent(HarnessKind::Codex, Some(signed_out())),
        ]]),
        ..Fake::default()
    };
    let Start::Resume(mut progress) = at(Step::Agent) else {
        unreachable!()
    };
    progress.agent = Some(HarnessKind::Codex);
    let (_, ui) = go(
        &mut host,
        Start::Resume(progress),
        vec![c("start"), c("next"), Answer::Quit],
    )
    .await;
    assert_eq!(host.progress().agent, Some(HarnessKind::ClaudeCode));
    // One agent ready: there is nothing to choose between.
    assert!(!ui.saw("Choose which new projects start with"));
}

fn updates(enabled: bool) -> AgentUpdatesView {
    AgentUpdatesView {
        enabled,
        revision: 3,
        supported: true,
        ..AgentUpdatesView::default()
    }
}

#[tokio::test]
async fn keeping_the_agents_up_to_date_is_asked_once_and_recommended() {
    let mut host = Fake {
        updates: Some(updates(false)),
        ..Fake::default()
    };
    let answers = vec![
        c("start"),
        c("next"),
        c("back"),
        c("next"),
        c("on"),
        Answer::Quit,
    ];
    let (_, ui) = go(&mut host, at(Step::Agent), answers).await;
    assert!(ui.saw("Updates: Tasks use the versions installed on this computer"));
    let asked = ui.screen("Keep Claude Code and Codex up to date?");
    let on = asked
        .choices()
        .iter()
        .find(|choice| choice.id == "on")
        .unwrap();
    assert!(on.recommended);
    assert_eq!(host.calls, ["set_agent_updates true 3"]);
    assert_eq!(host.progress().updates, Some(true));
    assert_eq!(
        host.progress().steps.get(&Step::Agent),
        Some(&Outcome::Done)
    );

    // Asked and answered: going through the step again doesn't ask.
    let mut again = Fake {
        updates: Some(updates(false)),
        ..Fake::default()
    };
    let Start::Resume(mut progress) = at(Step::Agent) else {
        unreachable!()
    };
    progress.updates = Some(false);
    let (_, ui) = go(
        &mut again,
        Start::Resume(progress),
        vec![c("start"), c("next"), Answer::Quit],
    )
    .await;
    assert!(!ui.saw("Keep Claude Code and Codex up to date?"));
    assert!(again.calls.is_empty());
}

#[tokio::test]
async fn keeping_the_agents_up_to_date_is_turned_off_and_on_in_the_step() {
    let mut host = Fake {
        updates: Some(updates(true)),
        updates_refused: Some(ErrorCode::StaleRevision),
        ..Fake::default()
    };
    let answers = vec![
        c("start"),
        c("updates"),
        c("next"),
        c("updates"),
        c("updates"),
        c("next"),
        Answer::Quit,
    ];
    let (_, ui) = go(&mut host, at(Step::Agent), answers).await;
    assert!(ui.saw("Updates: Øphio keeps its own copies up to date"));
    assert!(ui.saw("Use the versions installed on this computer instead"));
    assert!(ui.saw("Updates: It was changed in the app just now, so it was left as it is."));
    assert!(ui.saw("Keep Claude Code and Codex up to date"));
    assert_eq!(
        host.calls,
        [
            "set_agent_updates false 3",
            "set_agent_updates false 3",
            "set_agent_updates true 4",
        ]
    );
    assert_eq!(host.progress().updates, Some(true));
    // Turned on in the step: nothing more to ask.
    assert!(!ui.saw("Keep Claude Code and Codex up to date?"));
}

#[tokio::test]
async fn updates_are_not_offered_where_they_cant_be_kept() {
    for (view, agents) in [
        (
            Some(AgentUpdatesView {
                supported: false,
                ..updates(false)
            }),
            vec![agent(HarnessKind::ClaudeCode, Some(signed_in()))],
        ),
        (
            None,
            vec![agent(HarnessKind::ClaudeCode, Some(signed_in()))],
        ),
        // Nothing installed to keep up to date.
        (
            Some(updates(false)),
            vec![agent(HarnessKind::ClaudeCode, None)],
        ),
    ] {
        let mut host = Fake {
            updates: view,
            agents: Script::of([agents]),
            ..Fake::default()
        };
        let (_, ui) = go(
            &mut host,
            at(Step::Agent),
            vec![c("start"), c("next"), Answer::Quit],
        )
        .await;
        assert!(!ui.saw("Updates:"));
        assert!(!ui.saw("Keep Claude Code and Codex up to date"));
        assert!(host.calls.is_empty());
    }
}

#[tokio::test]
async fn a_step_that_needs_ophio_starts_it_first() {
    let mut host = Fake {
        service: Service {
            running: false,
            at_login: false,
        },
        ..Fake::default()
    };
    let answers = vec![c("start"), c("start"), c("next"), Answer::Quit];
    let (_, ui) = go(&mut host, at(Step::Agent), answers).await;
    assert_eq!(host.calls, ["start"]);
    assert!(ui.saw("This step needs Øphio running, and it isn't."));
}

/// A phone setup up to the code, alerts answered `alerts`.
fn to_the_code(alerts: &'static str) -> Vec<Answer> {
    vec![c("start"), c("have"), c(alerts)]
}

#[tokio::test]
async fn a_phone_paired_with_ophio_pair_confirm_is_found() {
    let paired = device("Pixel 9", Grants::default());
    let mut host = Fake {
        devices: Script::of([Vec::new(), Vec::new(), Vec::new(), vec![paired]]),
        ..Fake::default()
    };
    let mut answers = to_the_code("off");
    answers.extend([c("keep"), Answer::Quit]);
    let (_, ui) = go(&mut host, at(Step::Phone), answers).await;
    assert_eq!(host.calls, ["open_pairing"]);
    assert!(host.granted.is_empty());
    assert!(ui.saw("&alerts=off"));
    let progress = host.progress();
    assert_eq!(progress.alerts, Some(false));
    assert_eq!(progress.steps.get(&Step::Phone), Some(&Outcome::Done));
}

#[tokio::test]
async fn a_refused_phone_is_not_paired() {
    let mut host = Fake {
        waiting: Script::of([Some(prompt(true))]),
        ..Fake::default()
    };
    let mut answers = to_the_code("on");
    answers.extend([c("refuse"), c("later"), Answer::Quit]);
    let (_, ui) = go(&mut host, at(Step::Phone), answers).await;
    assert_eq!(host.calls, ["open_pairing", "answer_pairing 123456 false"]);
    assert!(ui.saw("Refused Pixel 9."));
    // The codes are compared before either answer.
    let confirm = ui.screen("Only if your phone shows the same code.");
    assert!(confirm.choices().iter().all(|choice| !choice.recommended));
    assert_eq!(
        host.progress().steps.get(&Step::Phone),
        Some(&Outcome::Later)
    );
}

#[tokio::test]
async fn a_phone_that_stops_asking_is_waited_past() {
    let mut host = Fake {
        waiting: Script::of([Some(prompt(true))]),
        answers: VecDeque::from([Answered::Gone]),
        ..Fake::default()
    };
    let mut answers = to_the_code("on");
    answers.extend([c("pair"), c("later"), Answer::Quit]);
    let (_, ui) = go(&mut host, at(Step::Phone), answers).await;
    assert!(ui.saw("Pixel 9 stopped asking. Nothing was paired."));
    assert!(host.granted.is_empty());
}

#[tokio::test]
async fn a_request_withdrawn_while_asked_about_is_waited_past() {
    let mut host = Fake {
        waiting: Script::of([Some(prompt(true)), None]),
        ..Fake::default()
    };
    let mut answers = to_the_code("on");
    answers.extend([c("later"), Answer::Quit]);
    let (_, ui) = go(&mut host, at(Step::Phone), answers).await;
    assert_eq!(host.calls, ["open_pairing"]);
    assert!(ui.saw("Pixel 9 stopped asking."));
}

#[tokio::test]
async fn each_pairing_request_is_asked_about_as_itself() {
    let other = PairingPrompt {
        device_name: "Galaxy".into(),
        code: "654321".into(),
        ..prompt(true)
    };
    let mut host = Fake {
        waiting: Script::of([Some(prompt(true)), None, Some(other), None]),
        ..Fake::default()
    };
    let mut answers = to_the_code("on");
    // The first request is withdrawn while asked about, then another comes.
    answers.extend([Answer::Waited, c("refuse"), c("later"), Answer::Quit]);
    let (_, ui) = go(&mut host, at(Step::Phone), answers).await;
    assert_eq!(host.calls, ["open_pairing", "answer_pairing 654321 false"]);
    let asked: Vec<_> = ui
        .screens
        .iter()
        .filter(|screen| words(screen).contains("Only if your phone shows the same code."))
        .map(|screen| screen.about.clone())
        .collect();
    assert_eq!(asked.len(), 2, "{asked:?}");
    assert!(asked.iter().all(Option::is_some));
    // So neither UI carries a key begun on the first to the second.
    assert_ne!(asked[0], asked[1]);
}

#[tokio::test]
async fn an_expired_code_is_replaced() {
    let mut host = Fake {
        offer_life: 3_000,
        ..Fake::default()
    };
    let mut answers = to_the_code("on");
    answers.extend([c("again"), c("later"), Answer::Quit]);
    let (_, ui) = go(&mut host, at(Step::Phone), answers).await;
    assert_eq!(host.calls, ["open_pairing", "open_pairing"]);
    assert!(ui.saw("This code has expired. Nothing was paired."));
}

#[tokio::test]
async fn an_app_too_old_to_show_a_code_is_refused() {
    let mut host = Fake {
        waiting: Script::of([Some(prompt(false))]),
        ..Fake::default()
    };
    let mut answers = to_the_code("on");
    answers.extend([c("later"), Answer::Quit]);
    let (_, ui) = go(&mut host, at(Step::Phone), answers).await;
    assert_eq!(host.calls, ["open_pairing", "answer_pairing 123456 false"]);
    assert!(ui.saw("too old to pair safely"));
    assert!(!ui.saw("Only if your phone shows the same code."));
}

#[tokio::test]
async fn a_code_without_the_tailscale_address_restarts_ophio_once() {
    let mut host = Fake {
        addresses: vec!["192.168.1.20:47100".into()],
        offer_life: 2_000,
        ..Fake::default()
    };
    let mut answers = to_the_code("on");
    answers.extend([c("again"), c("later"), Answer::Quit]);
    let (_, ui) = go(&mut host, at(Step::Phone), answers).await;
    assert_eq!(
        host.calls,
        ["open_pairing", "restart", "open_pairing", "open_pairing"]
    );
    assert!(ui.saw("Restarting Øphio so the code includes its Tailscale address"));
}

#[tokio::test]
async fn a_restart_that_adds_the_tailscale_address_is_used() {
    let mut host = Fake {
        addresses: vec!["192.168.1.20:47100".into()],
        restarted: Some(vec!["100.64.0.1:47100".into()]),
        waiting: Script::of([Some(prompt(true))]),
        ..Fake::default()
    };
    let mut answers = to_the_code("on");
    answers.extend([c("pair"), c("keep"), Answer::Quit]);
    assert!(go(&mut host, at(Step::Phone), answers).await.0.is_ok());
    assert_eq!(
        host.calls,
        [
            "open_pairing",
            "restart",
            "open_pairing",
            "answer_pairing 123456 true"
        ]
    );
}

#[tokio::test]
async fn a_tailscale_address_in_brackets_counts() {
    let ip: IpAddr = "fd7a:115c:a1e0::1".parse().unwrap();
    let mut host = Fake {
        networks: Script::of([network(
            Tailscale::Running {
                ips: vec![ip],
                name: None,
            },
            false,
            &[],
        )]),
        addresses: vec!["[fd7a:115c:a1e0::1]:47100".into()],
        ..Fake::default()
    };
    let mut answers = to_the_code("on");
    answers.extend([c("later"), Answer::Quit]);
    assert!(go(&mut host, at(Step::Phone), answers).await.0.is_ok());
    assert_eq!(host.calls, ["open_pairing"]);
}

#[tokio::test]
async fn a_code_only_this_computer_can_reach_says_so() {
    let mut host = Fake {
        networks: Script::of([network(Tailscale::Missing, false, &[])]),
        addresses: vec!["127.0.0.1:47100".into()],
        ..Fake::default()
    };
    let mut answers = to_the_code("on");
    answers.extend([c("later"), Answer::Quit]);
    let (_, ui) = go(&mut host, at(Step::Phone), answers).await;
    assert!(ui.saw("Can't be reached: Your phone can't reach this computer yet"));
}

#[tokio::test]
async fn the_app_is_shown_how_to_get() {
    let mut host = Fake::default();
    let answers = vec![
        c("start"),
        c("how"),
        c("back"),
        c("how"),
        c("next"),
        Answer::Quit,
    ];
    let (_, ui) = go(&mut host, at(Step::Phone), answers).await;
    // Without the app in this download, it can't be installed from here.
    assert!(!ui.saw("Install it from this computer"));
    let qr = ui.screen("Scan this with your phone's camera.");
    assert!(qr.blocks.contains(&Block::Qr {
        data: APP_DOWNLOAD_URL.into(),
        caption: "Scan this with your phone's camera.".into(),
    }));
    // Installed, the next question is about alerts.
    assert!(
        ui.screens
            .last()
            .unwrap()
            .choices()
            .iter()
            .any(|choice| choice.id == "on")
    );
}

/// The phone app this download came with.
const APP: &str = "/opt/ophio/app/ophio.apk";

fn with_the_app() -> Fake {
    Fake {
        phone_app: Some(APP.into()),
        // A look before the phone asks, so the pairing screen shows.
        waiting: Script::of([None, Some(prompt(true))]),
        ..Fake::default()
    }
}

#[tokio::test]
async fn the_app_is_installed_from_this_computer() {
    let phone = phone_device("192.168.1.30:37000");
    let mut host = Fake {
        scanned: Script::of([None, Some(phone)]),
        ..with_the_app()
    };
    let answers = vec![
        c("start"),
        c("wireless"),
        c("open"),
        c("next"),
        c("on"),
        c("pair"),
        c("keep"),
        c("off"),
        Answer::Quit,
    ];
    let (_, ui) = go(&mut host, at(Step::Phone), answers).await;
    let at = "192.168.1.30:37000";
    assert_eq!(
        host.calls,
        [
            format!("adb_pair_qr {at}"),
            format!("phone_install {at} {APP}"),
            format!("phone_open {at} Tailscale"),
            format!("phone_alerts {at}"),
            "open_pairing".into(),
            format!("phone_link {at}"),
            "answer_pairing 123456 true".into(),
            format!("phone_off {at}"),
        ]
    );
    let scan = ui.screen("Pair device with QR code");
    assert!(scan.blocks.contains(&Block::Qr {
        data: "WIFI:T:ADB;S:ophio-test;P:password;;".into(),
        caption: "Keep your phone on the same Wi-Fi as this computer.".into(),
    }));
    assert!(ui.saw("Installing Øphio on Pixel_9…"));
    assert!(ui.saw("It's open on your phone. Sign in with the same account"));
    assert!(ui.saw("Øphio is open on your phone with this computer's pairing link filled in."));
    // The code stays on screen, for a phone the link didn't reach.
    assert!(ui.saw("&alerts=on"));
    assert_eq!(
        host.progress().steps.get(&Step::Phone),
        Some(&Outcome::Done)
    );
}

#[tokio::test]
async fn says_when_wireless_debugging_may_still_be_on() {
    for (off, said) in [
        (Off::Unsure, "Maybe still on"),
        (Off::Failed("Permission denial".into()), "Permission denial"),
    ] {
        let mut host = Fake {
            scanned: Script::of([None, Some(phone_device("192.168.1.30:37000"))]),
            off,
            ..with_the_app()
        };
        let answers = vec![
            c("start"),
            c("wireless"),
            c("open"),
            c("next"),
            c("on"),
            c("pair"),
            c("keep"),
            c("off"),
            c("next"),
            Answer::Quit,
        ];
        let (_, ui) = go(&mut host, at(Step::Phone), answers).await;
        assert!(ui.saw(said));
        assert!(ui.saw("Turn it off on the phone, under Settings › Developer options"));
    }
}

#[tokio::test]
async fn without_adb_setup_says_where_to_get_it() {
    let mut host = Fake {
        adb: Script::of([false, true]),
        ..with_the_app()
    };
    let answers = vec![
        c("start"),
        c("wireless"),
        c("again"),
        c("back"),
        c("have"),
        c("off"),
        Answer::Quit,
    ];
    let (_, ui) = go(&mut host, at(Step::Phone), answers).await;
    assert!(ui.saw(phone::PLATFORM_TOOLS_URL));
    assert!(ui.saw("you accept Google's terms there"));
    assert!(ui.saw("Pair device with QR code"));
    assert_eq!(host.calls, ["open_pairing"]);
}

#[tokio::test]
async fn a_phone_can_be_paired_by_code_and_left_debugging() {
    let mut host = Fake {
        networks: Script::of([network(Tailscale::Missing, true, &[WIFI])]),
        fail: vec!["adb_pair_code", "phone_link"],
        ..with_the_app()
    };
    let answers = vec![
        c("start"),
        c("wireless"),
        c("code"),
        text("123 456"),
        text("123 456 192.168.1.30:41000"),
        c("off"),
        c("pair"),
        c("keep"),
        c("keep"),
        Answer::Quit,
    ];
    let (_, ui) = go(&mut host, at(Step::Phone), answers).await;
    let at = "192.168.1.30:40000";
    assert_eq!(
        host.calls,
        [
            "adb_pair_code None 123456".into(),
            "adb_pair_code Some(\"192.168.1.30:41000\") 123456".into(),
            format!("phone_install {at} {APP}"),
            "open_pairing".into(),
            format!("phone_link {at}"),
            "answer_pairing 123456 true".into(),
        ]
    );
    assert!(ui.saw("Not paired: configuration error: adb_pair_code broke"));
    // Reached over Wi-Fi, the phone needs no Tailscale.
    assert!(!ui.saw("Open Tailscale on my phone"));
    assert!(ui.saw("Couldn't open Øphio on your phone: configuration error: phone_link broke"));
    assert!(ui.saw("Open Øphio on your phone and tap Pair a computer."));
    assert!(ui.saw("Setup is done with Wireless debugging on Pixel_9."));
}

#[tokio::test]
async fn an_app_signed_with_another_key_is_left_alone() {
    let mut host = Fake {
        phones: vec![phone_device("emulator-5554")],
        installs: Script::of([Installed::OtherKey]),
        ..with_the_app()
    };
    let answers = vec![
        c("start"),
        c("wireless"),
        c("use"),
        c("retry"),
        c("back"),
        c("have"),
        c("off"),
        Answer::Quit,
    ];
    let (_, ui) = go(&mut host, at(Step::Phone), answers).await;
    let install = format!("phone_install emulator-5554 {APP}");
    assert_eq!(host.calls, [install.as_str(), &install, "open_pairing"]);
    assert!(ui.saw("Install on Pixel_9"));
    assert!(ui.saw("signed with a different key"));
    // On a cable, there is no Wireless debugging to turn off.
    assert!(!ui.saw("Turn Wireless debugging off"));
}

#[tokio::test]
async fn a_phone_quit_part_way_keeps_its_debugging_on() {
    let phone = phone_device("192.168.1.30:37000");
    let mut host = Fake {
        scanned: Script::of([Some(phone)]),
        networks: Script::of([network(Tailscale::Missing, true, &[WIFI])]),
        ..with_the_app()
    };
    let answers = vec![c("start"), c("wireless"), c("off"), Answer::Quit];
    let (ending, _) = go(&mut host, at(Step::Phone), answers).await;
    assert!(ending.is_ok());
    assert!(!host.calls.iter().any(|call| call.starts_with("phone_off")));
}

#[tokio::test]
async fn a_phone_already_paired_is_enough() {
    let mut host = Fake {
        devices: Script::of([vec![device("Pixel 9", Grants::owner())]]),
        ..Fake::default()
    };
    let (_, ui) = go(
        &mut host,
        at(Step::Phone),
        vec![c("start"), c("next"), Answer::Quit],
    )
    .await;
    assert!(host.calls.is_empty());
    assert!(ui.saw("Paired with this computer:"));
    assert!(ui.saw("Pixel 9: Paired 1 minute ago"));
    assert_eq!(
        host.progress().steps.get(&Step::Phone),
        Some(&Outcome::Done)
    );
}

#[tokio::test]
async fn a_removed_phone_is_not_counted_as_paired() {
    let mut removed = device("Pixel 9", Grants::owner());
    removed.revoked_at = Some(Timestamp(CLOCK - 1_000));
    let mut host = Fake {
        devices: Script::of([vec![removed]]),
        ..Fake::default()
    };
    let (_, ui) = go(&mut host, at(Step::Phone), vec![c("start"), Answer::Quit]).await;
    assert!(ui.saw("Your phone needs the Øphio app"));
}

#[tokio::test]
async fn pairing_another_phone_asks_about_alerts_only_once() {
    let mut host = Fake {
        devices: Script::of([vec![device("Pixel 9", Grants::owner())]]),
        ..Fake::default()
    };
    let Start::Resume(mut progress) = at(Step::Phone) else {
        unreachable!()
    };
    progress.alerts = Some(true);
    let answers = vec![c("start"), c("pair"), c("later"), Answer::Quit];
    let (_, ui) = go(&mut host, Start::Resume(progress), answers).await;
    assert!(!ui.saw("Turn on alerts"));
    assert!(ui.saw("&alerts=on"));
}

#[tokio::test]
async fn going_through_again_asks_about_alerts_again() {
    let mut host = Fake {
        devices: Script::of([vec![device("Pixel 9", Grants::owner())]]),
        ..Fake::default()
    };
    let Start::Resume(mut progress) = at(Step::Phone) else {
        unreachable!()
    };
    progress.alerts = Some(true);
    progress.finished = Some(Finished::Done);
    let answers = vec![
        c("start"),
        c("next"),
        c("next"),
        c("next"),
        c("pair"),
        c("off"),
        c("later"),
        Answer::Quit,
    ];
    let (ending, ui) = go(&mut host, Start::Rerun(progress), answers).await;
    assert_eq!(ending.unwrap(), Ending::Stopped);
    assert!(ui.saw("&alerts=off"));
    assert_eq!(host.progress().alerts, Some(false));
}

/// Pairs a phone that may only watch, then answers `grants`.
async fn grant(grants: Vec<Answer>) -> (Fake, Scripted) {
    let mut host = Fake {
        waiting: Script::of([Some(prompt(true))]),
        paired_grants: Grants {
            view: true,
            ..Grants::default()
        },
        ..Fake::default()
    };
    let mut answers = to_the_code("on");
    answers.push(c("pair"));
    answers.extend(grants);
    answers.push(Answer::Quit);
    let (_, ui) = go(&mut host, at(Step::Phone), answers).await;
    (host, ui)
}

#[tokio::test]
async fn everyday_grants_leave_out_control() {
    let (host, ui) = grant(vec![c("everyday"), c("next")]).await;
    assert_eq!(
        host.granted,
        [Grants {
            view: true,
            tasks: true,
            approvals: true,
            library_read: true,
            ..Grants::default()
        }]
    );
    assert!(ui.saw("Right now it may watch the desktop."));
}

#[tokio::test]
async fn grants_can_be_chosen_one_by_one() {
    let answers = vec![
        c("choose"),
        c("back"),
        c("choose"),
        Answer::Ticks(vec!["terminal", "tasks"]),
        c("next"),
    ];
    let (host, ui) = grant(answers).await;
    assert_eq!(
        host.granted,
        [Grants {
            terminal: true,
            tasks: true,
            ..Grants::default()
        }]
    );
    let Ask::Ticks { ticks, .. } = &ui.screen("Tick what Pixel 9 may do.").ask else {
        panic!("no ticks")
    };
    assert_eq!(ticks.len(), 11);
    let on: Vec<&str> = ticks
        .iter()
        .filter(|tick| tick.on)
        .map(|tick| tick.id)
        .collect();
    assert_eq!(on, ["view"]);
}

#[tokio::test]
async fn grants_can_be_kept() {
    let (host, _) = grant(vec![c("keep")]).await;
    assert_eq!(host.calls.last().unwrap(), "answer_pairing 123456 true");
    assert!(host.granted.is_empty());
    assert_eq!(
        host.progress().steps.get(&Step::Phone),
        Some(&Outcome::Done)
    );
}

#[test]
fn a_reply_goes_to_a_running_turn_and_otherwise_resumes_the_task_at_its_revision() {
    let task = TaskId::new();
    for state in [TaskState::Working, TaskState::AwaitingDecision] {
        assert!(matches!(
            telling(task, Some((state, 4)), "go on"),
            (protocol::Command::SendMessage { task_id, .. }, None) if task_id == task
        ));
    }
    // A finished task keeps its last run's id; the reply starts a new turn.
    for state in [
        TaskState::Accepted,
        TaskState::Dispatching,
        TaskState::Interrupted,
        TaskState::Completed,
        TaskState::Failed,
        TaskState::ReconciliationRequired,
        TaskState::Archived,
    ] {
        assert!(matches!(
            telling(task, Some((state, 4)), "go on"),
            (protocol::Command::ResumeTask { task_id, text: Some(ref text), .. }, Some(4))
                if task_id == task && text == "go on"
        ));
    }
    // A task that couldn't be read is left for Øphio to refuse.
    assert!(matches!(
        telling(task, None, "go on"),
        (protocol::Command::ResumeTask { .. }, None)
    ));
}

#[test]
fn grant_changes_take_away_before_they_give() {
    assert_eq!(grant_changes(Grants::owner()), [(Grants::owner(), true)]);
    assert_eq!(grant_changes(Grants::default()), [(Grants::owner(), false)]);
    let some = Grants {
        view: true,
        tasks: true,
        ..Grants::default()
    };
    let mut rest = Grants::owner();
    rest.view = false;
    rest.tasks = false;
    assert_eq!(
        grant_changes(some.clone()),
        [(rest, false), (some.clone(), true)]
    );
    // A phone that could do everything is left with exactly these, without
    // control of the desktop.
    let mut device = Grants::owner();
    for (mask, allowed) in grant_changes(some.clone()) {
        device.apply_delta(&mask, allowed);
    }
    assert_eq!(device, some);
}

#[test]
fn a_folder_under_home_is_expanded() {
    let home = directories::BaseDirs::new().unwrap().home_dir().to_owned();
    assert_eq!(expand_home("~/code"), home.join("code"));
    assert_eq!(expand_home("~"), home);
    assert_eq!(expand_home("/srv/code"), PathBuf::from("/srv/code"));
    assert_eq!(expand_home("~other/code"), PathBuf::from("~other/code"));
}

#[tokio::test]
async fn no_extras_is_fine() {
    let mut host = Fake::default();
    let answers = vec![c("start"), Answer::Ticks(Vec::new()), Answer::Quit];
    let (_, ui) = go(&mut host, at(Step::Options), answers).await;
    assert!(host.calls.is_empty());
    let Ask::Ticks { ticks, .. } = &ui.screens[1].ask else {
        panic!("no ticks")
    };
    assert_eq!(ticks.len(), 10);
    // Where files go is offered ticked, until it has been offered once.
    let on: Vec<&str> = ticks
        .iter()
        .filter(|tick| tick.on)
        .map(|tick| tick.id)
        .collect();
    assert_eq!(on, ["files"]);
    assert!(host.progress().files);
    assert_eq!(
        host.progress().steps.get(&Step::Options),
        Some(&Outcome::Done)
    );
}

#[tokio::test]
async fn the_person_chooses_where_a_checklists_files_go() {
    let view = FileDeliveryView {
        settings: FileDeliverySettings::default(),
        taildrop: Some(vec!["sam-laptop".into()]),
        folders: vec![DeliveryFolder {
            label: "Google Drive".into(),
            path: "/home/sam/Google Drive".into(),
        }],
        drives: None,
    };
    let mut host = Fake {
        delivery: Some(view),
        ..Fake::default()
    };
    let answers = vec![
        c("start"),
        Answer::Ticks(vec!["files"]),
        c("3"),
        Answer::Quit,
    ];
    let (_, ui) = go(&mut host, at(Step::Options), answers).await;
    assert_eq!(
        host.calls,
        [r#"set_file_delivery Folder { path: "/home/sam/Google Drive" } 0"#]
    );
    let Ask::Choose(choices) = &ui.screen("where should it go?").ask else {
        panic!("no choices")
    };
    let offered: Vec<_> = (choices.iter())
        .map(|c| (c.id, c.label.as_str(), c.recommended))
        .collect();
    assert_eq!(
        offered,
        [
            ("1", "To the phone that asks", true),
            ("2", "sam-laptop", false),
            ("3", "Google Drive", false),
            ("other", "Another folder on this computer", false),
            ("later", "Leave this for later", false),
        ]
    );
    assert!(ui.saw("Google Drive, Dropbox, OneDrive and others need rclone set up here"));

    // Gone through again, another folder is typed, from the home folder.
    let answers = vec![
        c("start"),
        Answer::Ticks(vec!["files"]),
        c("other"),
        Answer::Text("~/Checklists ".into()),
        Answer::Quit,
    ];
    assert!(go(&mut host, at(Step::Options), answers).await.0.is_ok());
    assert_eq!(
        host.calls[1],
        r#"set_file_delivery Folder { path: "~/Checklists" } 1"#
    );
}

#[tokio::test]
async fn an_ophio_without_checklists_says_so_where_files_go() {
    let mut host = Fake::default();
    let answers = vec![
        c("start"),
        Answer::Ticks(vec!["files"]),
        c("next"),
        Answer::Quit,
    ];
    let (_, ui) = go(&mut host, at(Step::Options), answers).await;
    assert!(host.calls.is_empty());
    assert!(ui.saw("This Øphio doesn't keep checklists yet."));
}

#[tokio::test]
async fn an_extra_runs_its_command_once_agreed() {
    let mut host = Fake::default();
    let answers = vec![
        c("start"),
        Answer::Ticks(vec!["voice"]),
        c("run"),
        c("next"),
        Answer::Quit,
    ];
    let (_, ui) = go(&mut host, at(Step::Options), answers).await;
    assert_eq!(host.calls, ["run ophio voice install tiny.en"]);
    assert_eq!(ui.lent, ["suspend", "resume"]);
    assert!(ui.saw("Talk to your agents: Done"));
    assert!(host.progress().left.is_empty());
}

#[tokio::test]
async fn a_chat_bot_is_given_its_token_and_turned_on() {
    let mut host = Fake {
        service: Service {
            running: false,
            at_login: false,
        },
        ..Fake::default()
    };
    let answers = vec![
        c("start"),
        Answer::Ticks(vec!["telegram"]),
        c("run"),
        c("start"),
        c("next"),
        Answer::Quit,
    ];
    assert!(go(&mut host, at(Step::Options), answers).await.0.is_ok());
    assert_eq!(
        host.calls,
        [
            "start",
            "run ophio channels token set telegram",
            "run ophio channels enable telegram"
        ]
    );
}

#[tokio::test]
async fn an_extra_that_fails_can_be_tried_again() {
    let mut host = Fake {
        runs: VecDeque::from([false]),
        ..Fake::default()
    };
    let answers = vec![
        c("start"),
        Answer::Ticks(vec!["desktop"]),
        c("run"),
        c("retry"),
        c("run"),
        c("next"),
        Answer::Quit,
    ];
    let (_, ui) = go(&mut host, at(Step::Options), answers).await;
    assert_eq!(
        host.calls,
        ["run ophio desktop enable", "run ophio desktop enable"]
    );
    assert!(ui.saw("That didn't finish; its output says why."));
    assert!(host.progress().left.is_empty());
}

#[tokio::test]
async fn extras_left_for_later_are_listed_and_ticked_next_time() {
    let mut host = Fake {
        runs: VecDeque::from([false]),
        ..Fake::default()
    };
    let answers = vec![
        c("start"),
        Answer::Ticks(vec!["voice", "voice_control"]),
        c("run"),
        c("next"),
        c("later"),
        c("next"),
        c("finish"),
    ];
    let (ending, ui) = go(&mut host, at(Step::Options), answers).await;
    assert_eq!(ending.unwrap(), Ending::Done);
    assert_eq!(host.calls, ["run ophio voice install tiny.en"]);
    let progress = host.progress().clone();
    assert_eq!(progress.left, [Pick::Voice, Pick::VoiceControl]);
    assert!(ui.saw("Extras left for later: Talk to your agents, Control the computer by voice"));

    let mut host = Fake::default();
    let answers = vec![
        c("start"),
        c("next"),
        c("next"),
        c("next"),
        c("later"),
        Answer::Ticks(vec!["voice"]),
        c("run"),
        c("next"),
        Answer::Quit,
    ];
    let (_, ui) = go(&mut host, Start::Rerun(progress), answers).await;
    let Ask::Ticks { ticks, .. } = &ui.screen("Tick any you want to set up now.").ask else {
        panic!("no ticks")
    };
    let on: Vec<&str> = ticks
        .iter()
        .filter(|tick| tick.on)
        .map(|tick| tick.id)
        .collect();
    assert_eq!(on, ["voice", "voice_control"]);
    // Set up now, it is no longer left; the one not ticked still is.
    assert_eq!(host.progress().left, [Pick::VoiceControl]);
}

/// A Git project's GitHub workflow as the companion reads it: on GitHub as
/// `@me`, an admin of it, when `admin`.
fn github_project(name: &str, admin: bool, mode: GithubMode) -> (String, GithubProjectView) {
    let view = GithubProjectView {
        project_id: ProjectId::new(),
        login: Some("me".into()),
        repo: Some(GithubRepo {
            owner: "me".into(),
            name: name.into(),
            url: format!("https://github.com/me/{name}"),
            default_branch: "main".into(),
            private: true,
            admin,
            push: true,
            owned: admin,
            protected: Some(false),
            runs_checks: Some(true),
            deploys: true,
            private_reporting: None,
        }),
        problem: None,
        repo_folders: Vec::new(),
        settings: GithubSettings {
            mode,
            ..GithubSettings::default()
        },
        warnings: vec!["Pushes to `main` trigger a deployment.".into()],
        open_agent_prs: 0,
        run: None,
        together: None,
    };
    (name.into(), view)
}

/// A Git project whose origin isn't on GitHub, or GitHub without an account.
fn not_on_github(name: &str, login: Option<&str>, problem: &str) -> (String, GithubProjectView) {
    let (name, mut view) = github_project(name, false, GithubMode::Off);
    view.login = login.map(Into::into);
    view.repo = None;
    view.problem = Some(problem.into());
    view.warnings.clear();
    (name, view)
}

#[tokio::test]
async fn each_project_on_github_gets_its_workflow() {
    let mut host = Fake {
        github: Script::of([vec![
            github_project("app", true, GithubMode::Off),
            not_on_github("notes", Some("me"), "Its origin isn't on GitHub"),
        ]]),
        ..Fake::default()
    };
    let answers = vec![
        c("start"),
        Answer::Ticks(vec!["github"]),
        c("drafts"),
        c("next"),
        Answer::Quit,
    ];
    let (_, ui) = go(&mut host, at(Step::Options), answers).await;
    assert_eq!(host.calls, ["github_mode Drafts"]);
    let screen = ui.screen("me/app: app, working as @me");
    let Ask::Choose(choices) = &screen.ask else {
        panic!("no choices")
    };
    let ids: Vec<&str> = choices.iter().map(|choice| choice.id).collect();
    assert_eq!(ids, ["off", "through_merge", "drafts", "later"]);
    assert!(choices[0].recommended);
    assert!(ui.saw("main isn't protected: nothing stops a push"));
    assert!(ui.saw("app: Drafts"));
    assert!(ui.saw("notes: Its origin isn't on GitHub"));
    assert!(host.progress().left.is_empty());
}

#[tokio::test]
async fn through_merge_is_asked_about_before_it_merges() {
    for (answer, calls, ends) in [
        (
            "keep",
            [
                "github_mode ThroughMerge",
                "github_warning me/app keep=true",
            ],
            "app: Through merge",
        ),
        (
            "use_drafts",
            [
                "github_mode ThroughMerge",
                "github_warning me/app keep=false",
            ],
            "app: Drafts",
        ),
    ] {
        let mut host = Fake {
            github: Script::of([vec![github_project("app", true, GithubMode::Drafts)]]),
            ..Fake::default()
        };
        let answers = vec![
            c("start"),
            Answer::Ticks(vec!["github"]),
            c("through_merge"),
            c(answer),
            c("next"),
            Answer::Quit,
        ];
        let (_, ui) = go(&mut host, at(Step::Options), answers).await;
        assert_eq!(host.calls, calls);
        let warning = ui.screen("Nothing merges until you choose.");
        assert!(words(warning).contains("Pushes to `main` trigger a deployment."));
        assert!(words(warning).contains("into main on me/app when every check passes"));
        let Ask::Choose(choices) = &warning.ask else {
            panic!("no choices")
        };
        assert!(choices.iter().all(|choice| !choice.recommended));
        assert!(ui.saw(ends));
    }
}

#[tokio::test]
async fn through_merge_left_unanswered_is_asked_about_again() {
    let (name, mut view) = github_project("app", true, GithubMode::ThroughMerge);
    view.settings.warning_answered = false;
    let mut host = Fake {
        github: Script::of([vec![(name, view)]]),
        ..Fake::default()
    };
    let answers = vec![
        c("start"),
        Answer::Ticks(vec!["github"]),
        c("through_merge"),
        c("later"),
        c("next"),
        Answer::Quit,
    ];
    let (_, ui) = go(&mut host, at(Step::Options), answers).await;
    assert!(host.calls.is_empty());
    assert!(ui.saw("Nothing merges until you choose."));
    assert_eq!(host.progress().left, [Pick::Github]);
}

#[tokio::test]
async fn through_merge_needs_an_admin() {
    let mut host = Fake {
        github: Script::of([vec![github_project("theirs", false, GithubMode::Off)]]),
        ..Fake::default()
    };
    let answers = vec![
        c("start"),
        Answer::Ticks(vec!["github"]),
        c("off"),
        c("next"),
        Answer::Quit,
    ];
    let (_, ui) = go(&mut host, at(Step::Options), answers).await;
    assert!(host.calls.is_empty());
    let screen = ui.screen("Through merge is offered where @me administers the repository.");
    assert!(words(screen).contains("It isn't your repository"));
    let Ask::Choose(choices) = &screen.ask else {
        panic!("no choices")
    };
    let ids: Vec<&str> = choices.iter().map(|choice| choice.id).collect();
    assert_eq!(ids, ["off", "drafts", "later"]);
}

#[tokio::test]
async fn github_waits_for_an_account() {
    let mut host = Fake {
        github: Script::of([
            vec![not_on_github("app", None, "No GitHub account is signed in")],
            vec![github_project("app", true, GithubMode::Off)],
        ]),
        ..Fake::default()
    };
    let answers = vec![
        c("start"),
        Answer::Ticks(vec!["github"]),
        c("check"),
        c("off"),
        c("next"),
        Answer::Quit,
    ];
    let (_, ui) = go(&mut host, at(Step::Options), answers).await;
    assert!(ui.saw("GitHub: No GitHub account is signed in"));
    assert!(ui.saw("app: Off"));
    assert!(host.calls.is_empty());
    assert!(host.progress().left.is_empty());
}

#[tokio::test]
async fn github_without_projects_on_it_is_left_for_later() {
    for (projects, says) in [
        (Vec::new(), "Let agents open pull requests: No Git projects"),
        (
            vec![not_on_github(
                "notes",
                Some("me"),
                "Its origin isn't on GitHub",
            )],
            "Øphio found none of your Git projects on GitHub.",
        ),
    ] {
        let mut host = Fake {
            github: Script::of([projects]),
            ..Fake::default()
        };
        let answers = vec![
            c("start"),
            Answer::Ticks(vec!["github"]),
            c("next"),
            Answer::Quit,
        ];
        let (_, ui) = go(&mut host, at(Step::Options), answers).await;
        assert!(ui.saw(says));
        assert!(host.calls.is_empty());
        assert_eq!(host.progress().left, [Pick::Github]);
    }
}

#[tokio::test]
async fn agent_logins_are_turned_on_by_the_person() {
    let mut host = Fake::default();
    let answers = vec![
        c("start"),
        Answer::Ticks(vec!["agent_logins"]),
        c("run"),
        c("next"),
        Answer::Quit,
    ];
    let (_, ui) = go(&mut host, at(Step::Options), answers).await;
    assert_eq!(host.calls, ["run ophio agent-logins enable"]);
    let screen = ui.screen("It stays off until you accept its notice.");
    let Ask::Choose(choices) = &screen.ask else {
        panic!("no choices")
    };
    assert!(choices.iter().all(|choice| choice.id != "agent"));
    assert!(ui.saw("Let agents sign in to websites: Done"));
}

#[tokio::test]
async fn projects_are_added_one_folder_at_a_time() {
    let mut host = Fake::default();
    let Start::Resume(mut progress) = at(Step::Options) else {
        unreachable!()
    };
    progress.agent = Some(HarnessKind::Codex);
    let answers = vec![
        c("start"),
        Answer::Ticks(vec!["projects"]),
        c("type"),
        text("/work/app"),
        text("/missing"),
        text(""),
        Answer::Quit,
    ];
    let (_, ui) = go(&mut host, Start::Resume(progress), answers).await;
    assert_eq!(
        host.calls,
        [
            "add_project /work/app Some(Codex)",
            "add_project /missing Some(Codex)"
        ]
    );
    assert!(ui.saw("They start with: Codex"));
    let last = ui.screen("/missing: There is no folder at /missing.");
    assert!(words(last).contains("Added: /work/app"));
    assert!(last.blocks.contains(&Block::Fact {
        mark: Mark::Warn,
        label: "/missing".into(),
        detail: "There is no folder at /missing.".into(),
    }));
}

#[tokio::test]
async fn a_project_that_breaks_something_can_be_tried_again() {
    let mut host = Fake::default();
    let answers = vec![
        c("start"),
        Answer::Ticks(vec!["projects"]),
        text("/broken"),
        c("retry"),
        text(""),
        Answer::Quit,
    ];
    let (_, ui) = go(&mut host, at(Step::Options), answers).await;
    assert!(ui.saw("Problem: configuration error: the library broke"));
    assert!(host.progress().left.is_empty());
}

#[tokio::test]
async fn readiness_shows_what_fails_and_checks_again() {
    let mut failing = report(&[
        ("Tailscale", CheckStatus::Passed),
        ("Terminal", CheckStatus::Failed),
        ("Speech", CheckStatus::NotRun),
    ]);
    failing.checks[1].detail = Some("No shell found.".into());
    failing.checks[1].remedy = Some("Install bash.".into());
    failing.checks[2].summary = Some("No speech model yet.".into());
    let mut host = Fake {
        reports: Script::of([failing, report(&[("Terminal", CheckStatus::Passed)])]),
        ..Fake::default()
    };
    let answers = vec![c("start"), c("again"), c("next"), Answer::Quit];
    let (_, ui) = go(&mut host, at(Step::Readiness), answers).await;
    let first = ui.screen("Terminal: No shell found. Install bash.");
    assert!(first.blocks.contains(&Block::Fact {
        mark: Mark::Stop,
        label: "Terminal".into(),
        detail: "No shell found. Install bash.".into(),
    }));
    assert!(words(first).contains("1 check passed"));
    assert!(words(first).contains("Speech: No speech model yet."));
    assert!(!words(first).contains("Everything checked works."));
    assert!(ui.saw("Everything checked works."));
}

#[tokio::test]
async fn a_step_that_goes_wrong_can_be_tried_again() {
    let mut host = Fake {
        fail: vec!["network"],
        ..Fake::default()
    };
    let answers = vec![c("start"), c("retry"), c("next"), Answer::Quit];
    let (_, ui) = go(&mut host, at(Step::Reach), answers).await;
    assert!(ui.saw("Problem: configuration error: network broke"));
    assert_eq!(
        host.progress().steps.get(&Step::Reach),
        Some(&Outcome::Done)
    );
}

#[tokio::test]
async fn a_step_that_goes_wrong_can_be_left_for_later() {
    let mut host = Fake {
        fail: vec!["readiness"],
        ..Fake::default()
    };
    let answers = vec![c("start"), c("later"), c("finish")];
    let (ending, _) = go(&mut host, at(Step::Readiness), answers).await;
    assert_eq!(ending.unwrap(), Ending::Done);
    assert_eq!(
        host.progress().steps.get(&Step::Readiness),
        Some(&Outcome::Later)
    );
}

#[tokio::test]
async fn a_step_that_goes_wrong_can_skip_the_rest() {
    let mut host = Fake {
        fail: vec!["agents"],
        ..Fake::default()
    };
    let (ending, _) = go(&mut host, at(Step::Agent), vec![c("start"), c("skip")]).await;
    assert_eq!(ending.unwrap(), Ending::Skipped);
    assert_eq!(host.progress().finished, Some(Finished::Skipped));
}

#[tokio::test]
async fn done_is_saved_before_it_is_shown() {
    let mut host = Fake::default();
    let Start::Resume(mut progress) = at(Step::Done) else {
        unreachable!()
    };
    progress.steps.insert(Step::Reach, Outcome::Later);
    let (ending, ui) = go(
        &mut host,
        Start::Resume(progress),
        vec![c("start"), Answer::Quit],
    )
    .await;
    assert_eq!(ending.unwrap(), Ending::Done);
    assert_eq!(host.progress().finished, Some(Finished::Done));
    assert!(ui.saw("How your phone reaches this computer: Left for later"));
    assert!(ui.saw("Keep Øphio running: Done"));
}

#[test]
fn every_ending_says_how_to_go_on() {
    for ending in [
        Ending::Done,
        Ending::Skipped,
        Ending::Paused,
        Ending::Stopped,
    ] {
        let setup = format!("`{COMMAND} setup`");
        assert!(ending.message().contains(&setup), "{ending:?}");
    }
}

fn paths(root: &std::path::Path, name: &str) -> Paths {
    Paths {
        config: root.join(name).join("config"),
        data: root.join(name).join("data"),
        runtime: root.join(name).join("runtime"),
    }
}

#[test]
fn a_computer_that_never_ran_ophio_starts_afresh() {
    let root = tempfile::tempdir().unwrap();
    let (current, earlier) = (paths(root.path(), "ophio"), paths(root.path(), "earlier"));
    assert_eq!(
        super::start(&current, std::slice::from_ref(&earlier)),
        Start::First(Progress::default())
    );
    // A start leaves its lock, and empty directories, behind.
    std::fs::create_dir_all(current.data.join("logs")).unwrap();
    std::fs::write(current.data.join("instance.lock"), b"").unwrap();
    std::fs::create_dir_all(&earlier.config).unwrap();
    assert!(super::start(&current, std::slice::from_ref(&earlier)).pending());
}

#[test]
fn ophio_used_before_setup_existed_is_not_set_up_again_unasked() {
    let root = tempfile::tempdir().unwrap();
    let (current, earlier) = (paths(root.path(), "ophio"), paths(root.path(), "earlier"));
    std::fs::create_dir_all(&earlier.data).unwrap();
    std::fs::write(earlier.data.join("state.sqlite"), b"x").unwrap();
    let start = super::start(&current, std::slice::from_ref(&earlier));
    assert!(!start.pending());
    let Start::Rerun(progress) = start else {
        panic!("{start:?}")
    };
    assert_eq!(progress.finished, Some(Finished::Skipped));
}

#[test]
fn progress_saved_decides_where_setup_starts() {
    let root = tempfile::tempdir().unwrap();
    let current = paths(root.path(), "ophio");
    let mut progress = Progress::default();
    progress.steps.insert(Step::Background, Outcome::Done);
    progress.left.push(Pick::Voice);
    super::save(&current, &progress).unwrap();
    assert_eq!(super::start(&current, &[]), Start::Resume(progress.clone()));
    progress.finished = Some(Finished::Done);
    super::save(&current, &progress).unwrap();
    assert_eq!(super::start(&current, &[]), Start::Rerun(progress));
    // A file that can't be read is a setup that has begun.
    std::fs::write(current.data.join("setup.json"), b"{not json").unwrap();
    assert_eq!(
        super::start(&current, &[]),
        Start::Resume(Progress::default())
    );
}

/// Lines typed, in turn; the input ends after the last.
#[derive(Default)]
struct Typed {
    inputs: VecDeque<Input>,
    reading: bool,
}
impl Typed {
    fn of(inputs: &[&str]) -> Self {
        Self {
            inputs: inputs
                .iter()
                .map(|line| match *line {
                    "<wait>" => Input::Waited,
                    "<ctrl-c>" => Input::Interrupt,
                    line => Input::Line(format!("{line}\n")),
                })
                .collect(),
            reading: false,
        }
    }
}
impl Lines for Typed {
    async fn next(&mut self, _wait: Option<Duration>) -> Input {
        let input = self.inputs.pop_front().unwrap_or(Input::End);
        self.reading = input == Input::Waited;
        input
    }
    fn reading(&self) -> bool {
        self.reading
    }
}

fn plain(inputs: &[&str]) -> Plain<Typed, Vec<u8>> {
    Plain::new(Typed::of(inputs), Vec::new())
}
fn printed(plain: &Plain<Typed, Vec<u8>>) -> String {
    String::from_utf8(plain.out().clone()).unwrap()
}

fn plain_screen(step: Step, blocks: Vec<Block>, ask: Ask) -> Screen {
    Screen {
        step,
        title: flow::title(step).into(),
        blocks,
        ask,
        map: Vec::new(),
        about: None,
    }
}

fn reach() -> Screen {
    plain_screen(
        Step::Reach,
        vec![
            Block::Text("Your phone reaches this computer.".into()),
            Block::Fact {
                mark: Mark::Ok,
                label: "Tailscale".into(),
                detail: "Connected".into(),
            },
            Block::Fact {
                mark: Mark::Warn,
                label: "Wi‑Fi".into(),
                detail: "Not allowed".into(),
            },
            Block::Fact {
                mark: Mark::Stop,
                label: "Firewall".into(),
                detail: String::new(),
            },
            Block::Command("ophio pair".into()),
            Block::Link {
                label: "Get it at".into(),
                url: "https://tailscale.com/download".into(),
            },
            Block::Steps(vec!["First.".into(), "Then.".into()]),
        ],
        Ask::Choose(vec![
            Choice::new('n', "next", "Use Tailscale")
                .recommended()
                .detail("Wherever you are."),
            Choice::new('l', "later", "Leave this for later"),
        ]),
    )
}

#[tokio::test]
async fn plain_prints_a_screen_and_enter_picks_the_recommended() {
    let mut ui = plain(&[""]);
    assert_eq!(ui.ask(&reach(), None).await, c("next"));
    let out = printed(&ui);
    for line in [
        "Step 2 of 6: How your phone reaches this computer\n",
        "Your phone reaches this computer.\n",
        "OK: Tailscale: Connected\n",
        "Attention: Wi‑Fi: Not allowed\n",
        "Problem: Firewall\n",
        "    ophio pair\n",
        "Get it at: https://tailscale.com/download\n",
        "  1. First.\n  2. Then.\n",
        "  n  Use Tailscale (recommended)\n     Wherever you are.\n",
        "  l  Leave this for later\n",
        "Type a key and press Enter (Enter alone: n): ",
    ] {
        assert!(out.contains(line), "{line:?} not in {out}");
    }
}

#[tokio::test]
async fn plain_shows_a_command_that_is_not_all_plain_with_escapes() {
    let screen = plain_screen(
        Step::Agent,
        vec![
            Block::Command("rm -- 'a b'".into()),
            Block::Command("rm -- 'a\u{b7}b' \\u{b7}".into()),
            Block::Command("cd \u{202e}x\t ".into()),
            Block::Text("Then.".into()),
        ],
        Ask::Choose(vec![Choice::new('a', "allow", "Allow")]),
    );
    let mut ui = plain(&["a"]);
    assert_eq!(ui.ask(&screen, None).await, c("allow"));
    let out = printed(&ui);
    assert!(
        out.contains(&format!(
            "    rm -- 'a b'\n    rm -- 'a\\u{{b7}}b' \\\\u{{b7}}\n    cd \\u{{202e}}x\\u{{9}}\\u{{20}}\n{}\nThen.\n",
            flow::ESCAPED
        )),
        "{out}"
    );
    assert_eq!(out.matches(flow::ESCAPED).count(), 1);
}

#[tokio::test]
async fn plain_says_commands_shown_together_one_way() {
    // A backslash in one command is never taken for an escape another
    // command beside it needed.
    let mut said = Vec::new();
    for group in [
        ["rm -- '/tmp/P\\u{b7}old'", "echo '\u{b7}'"],
        ["rm -- '/tmp/P\u{b7}old'", "echo '\u{b7}'"],
    ] {
        let screen = plain_screen(
            Step::Agent,
            group.iter().map(|&c| Block::Command(c.into())).collect(),
            Ask::Choose(vec![Choice::new('a', "allow", "Allow")]),
        );
        let mut ui = plain(&["a"]);
        assert_eq!(ui.ask(&screen, None).await, c("allow"));
        let out = printed(&ui);
        assert_eq!(out.matches(flow::ESCAPED).count(), 1, "{out}");
        said.push(out);
    }
    assert!(said[0].contains("    rm -- '/tmp/P\\\\u{b7}old'\n    echo '\\u{b7}'\n"));
    assert!(said[1].contains("    rm -- '/tmp/P\\u{b7}old'\n    echo '\\u{b7}'\n"));
}

#[tokio::test]
async fn plain_welcome_has_no_step_number() {
    let mut ui = plain(&["s"]);
    let welcome = plain_screen(
        Step::Welcome,
        Vec::new(),
        Ask::Choose(vec![Choice::new('s', "start", "Start setup")]),
    );
    assert_eq!(ui.ask(&welcome, None).await, c("start"));
    let out = printed(&ui);
    assert!(out.contains("\nWelcome to Øphio\n"));
    assert!(!out.contains("Step "));
    assert!(out.contains("Type a key and press Enter: "));
}

#[tokio::test]
async fn plain_takes_keys_in_either_case_and_says_when_one_is_wrong() {
    let mut ui = plain(&["zz", "q", " L "]);
    assert_eq!(ui.ask(&reach(), None).await, c("later"));
    let out = printed(&ui);
    assert_eq!(
        out.matches("That isn't one of the keys or numbers shown.")
            .count(),
        2
    );
    assert_eq!(out.matches("Enter alone: n").count(), 3);
    // Printed once, however long it took to answer.
    assert_eq!(out.matches("Step 2 of 6").count(), 1);
}

#[tokio::test]
async fn plain_ends_at_ctrl_c_or_the_end_of_input() {
    let mut ui = plain(&["<ctrl-c>"]);
    assert_eq!(ui.ask(&reach(), None).await, Answer::Quit);
    let mut ui = plain(&[]);
    assert_eq!(ui.ask(&reach(), None).await, Answer::Quit);
}

#[tokio::test]
async fn plain_prints_a_screen_again_only_when_it_changes() {
    let mut ui = plain(&["<wait>", "<wait>", "<wait>", "n"]);
    let wait = Some(Duration::from_millis(1));
    assert_eq!(ui.ask(&reach(), wait).await, Answer::Waited);
    assert_eq!(ui.ask(&reach(), wait).await, Answer::Waited);
    assert_eq!(printed(&ui).matches("Step 2 of 6").count(), 1);
    let mut changed = reach();
    changed.blocks.push(Block::Text("Something new.".into()));
    assert_eq!(ui.ask(&changed, wait).await, Answer::Waited);
    assert_eq!(printed(&ui).matches("Step 2 of 6").count(), 2);
    assert_eq!(ui.ask(&changed, wait).await, c("next"));
    // Once answered, the same screen asked again is printed again.
    let mut ui2 = plain(&["n", "n"]);
    ui2.ask(&reach(), None).await;
    ui2.ask(&reach(), None).await;
    assert_eq!(printed(&ui2).matches("Step 2 of 6").count(), 2);
}

#[tokio::test]
async fn plain_never_takes_a_line_begun_on_another_request() {
    let asking = |about: &str| Screen {
        about: Some(about.into()),
        ..plain_screen(
            Step::Agent,
            vec![Block::Command("rm -r build".into())],
            Ask::Choose(vec![
                Choice::new('a', "allow", "Allow"),
                Choice::new('d', "deny", "Don't allow"),
            ]),
        )
    };
    let wait = Some(Duration::from_millis(1));
    // `a` is begun while approval A is asked; B takes its place before Enter.
    let mut ui = plain(&["<wait>", "a", "d"]);
    assert_eq!(ui.ask(&asking("A 1"), wait).await, Answer::Waited);
    assert_eq!(ui.ask(&asking("B 1"), None).await, c("deny"));
    let out = printed(&ui);
    assert_eq!(out.matches("so it wasn't used").count(), 1);
    // B's prompt is shown again after the line that was not used.
    let unused = out.find("so it wasn't used").unwrap();
    assert!(out[unused..].contains("Type a key and press Enter: "));
    // A new revision of the same request is another request too.
    let mut ui = plain(&["<wait>", "a", "a"]);
    assert_eq!(ui.ask(&asking("A 1"), wait).await, Answer::Waited);
    assert_eq!(ui.ask(&asking("A 2"), None).await, c("allow"));
    assert_eq!(printed(&ui).matches("so it wasn't used").count(), 1);
    // The same request, asked again after a screen that asks nothing, still
    // takes the line begun on it.
    let busy = plain_screen(Step::Agent, Vec::new(), Ask::Nothing);
    let mut ui = plain(&["<wait>", "a", "d"]);
    assert_eq!(ui.ask(&asking("A 1"), wait).await, Answer::Waited);
    assert_eq!(ui.ask(&busy, wait).await, Answer::Waited);
    assert_eq!(ui.ask(&asking("A 1"), None).await, c("allow"));
}

#[tokio::test]
async fn plain_reads_text_as_typed() {
    let mut ui = plain(&["  /work/app "]);
    let folder = plain_screen(
        Step::Options,
        Vec::new(),
        Ask::Text {
            prompt: "Folder (Enter alone when done)".into(),
        },
    );
    assert_eq!(ui.ask(&folder, None).await, text("/work/app"));
    assert!(printed(&ui).ends_with("\nFolder (Enter alone when done): "));
}

fn ticks() -> Screen {
    let tick = |id, label: &str, on| Tick {
        id,
        label: label.into(),
        detail: Some(format!("About {label}")),
        on,
    };
    plain_screen(
        Step::Options,
        Vec::new(),
        Ask::Ticks {
            ticks: vec![
                tick("a", "Apples", true),
                tick("b", "Bread", false),
                tick("c", "Cheese", false),
            ],
            choices: vec![Choice::new('b', "back", "Back")],
        },
    )
}

#[tokio::test]
async fn plain_ticks_by_number_and_goes_on_at_enter() {
    let mut ui = plain(&["2, 3", "9", "1", ""]);
    assert_eq!(ui.ask(&ticks(), None).await, Answer::Ticks(vec!["b", "c"]));
    let out = printed(&ui);
    assert!(out.contains("  [x] 1  Apples\n         About Apples\n  [ ] 2  Bread\n"));
    assert!(out.contains("  [x] 2  Bread\n"));
    assert!(out.contains("  [ ] 1  Apples\n"));
    assert_eq!(
        out.matches("That isn't one of the keys or numbers shown.")
            .count(),
        1
    );
    assert!(out.contains("Type numbers to tick or untick them, Enter alone to go on, or a key: "));
}

#[tokio::test]
async fn plain_ticks_start_afresh_each_time_they_are_shown() {
    let mut ui = plain(&["2", "b", ""]);
    assert_eq!(ui.ask(&ticks(), None).await, c("back"));
    assert_eq!(ui.ask(&ticks(), None).await, Answer::Ticks(vec!["a"]));
}

#[tokio::test]
async fn plain_shows_a_screen_that_asks_nothing_and_goes_on() {
    let mut ui = plain(&["n"]);
    let busy = plain_screen(
        Step::Background,
        vec![Block::Fact {
            mark: Mark::Wait,
            label: "Starting Øphio…".into(),
            detail: String::new(),
        }],
        Ask::Nothing,
    );
    assert_eq!(ui.ask(&busy, None).await, Answer::Waited);
    assert!(printed(&ui).contains("\nStarting Øphio…\n"));
    // Nothing typed was taken.
    assert_eq!(ui.ask(&reach(), None).await, c("next"));
}

#[tokio::test]
async fn plain_hands_the_terminal_over_only_once_its_line_is_in() {
    let mut ui = plain(&["<wait>", "", "n"]);
    assert_eq!(
        ui.ask(&reach(), Some(Duration::from_millis(1))).await,
        Answer::Waited
    );
    ui.suspend().await;
    assert!(printed(&ui).contains("Press Enter to continue."));
    ui.resume().await;
    // The line that was being read went to the wait, not to this screen.
    assert_eq!(ui.ask(&reach(), None).await, c("next"));
    assert_eq!(printed(&ui).matches("Step 2 of 6").count(), 2);

    let mut ui = plain(&[]);
    ui.suspend().await;
    assert!(!printed(&ui).contains("Press Enter"));
}

#[tokio::test]
async fn plain_setup_can_be_skipped_at_once() {
    let mut host = Fake::default();
    let mut ui = plain(&["x"]);
    let ending = flow::run(&mut host, &mut ui, Start::First(Progress::default())).await;
    assert_eq!(ending.unwrap(), Ending::Skipped);
    let out = printed(&ui);
    assert!(out.contains("Welcome to Øphio"));
    let skip = format!("  x  Skip setup\n     Run `{COMMAND} setup` whenever you want it.\n");
    assert!(out.contains(&skip));
    assert!(out.contains("OK: Claude Code: Found"));
}

/// A setup left part way at `step`, with Claude Code connected.
fn with_agent(step: Step) -> Start {
    let Start::Resume(mut progress) = at(step) else {
        unreachable!()
    };
    progress.agent = Some(HarnessKind::ClaudeCode);
    Start::Resume(progress)
}

/// Whether a call was made that starts with `start` and has all of `parts`.
fn called(host: &Fake, start: &str, parts: &[&str]) -> bool {
    host.calls
        .iter()
        .any(|call| call.starts_with(start) && parts.iter().all(|part| call.contains(part)))
}

#[tokio::test]
async fn the_agent_is_offered_only_once_one_is_connected() {
    let answers = vec![c("start"), Answer::Ticks(vec!["voice"]), Answer::Quit];
    let (_, ui) = go(&mut Fake::default(), at(Step::Options), answers.clone()).await;
    assert!(!ui.saw("Let the agent do it"));
    let (_, ui) = go(&mut Fake::default(), with_agent(Step::Options), answers).await;
    let voice = ui.screen("Let the agent do it");
    assert!(words(voice).contains("Claude Code does it in a task you follow here."));
    // Doing it by hand stays the default.
    let recommended: Vec<_> = voice
        .choices()
        .iter()
        .filter(|choice| choice.recommended)
        .map(|choice| choice.id)
        .collect();
    assert_eq!(recommended, ["run"]);
}

#[tokio::test]
async fn the_agent_sets_an_extra_up_and_asks_before_it_runs_a_command() {
    let mut host = Fake {
        views: Script::of([
            view(TaskState::Working),
            asking("sudo pacman -S whisper.cpp"),
            view(TaskState::Completed),
        ]),
        ..Fake::default()
    };
    let answers = vec![
        c("start"),
        Answer::Ticks(vec!["voice"]),
        c("agent"),
        c("start"),
        c("allow"),
        c("leave"),
        Answer::Quit,
    ];
    let (_, ui) = go(&mut host, with_agent(Step::Options), answers).await;
    assert!(ui.saw("a folder of its own"));
    assert!(ui.saw("signing in, and typing passwords or tokens stay with you"));
    assert!(called(
        &host,
        "start_setup_task setup-voice ClaudeCode",
        &["Set this up for me, please."]
    ));
    let asked = ui.screen("It asks to: Run a command");
    assert!(
        asked
            .blocks
            .contains(&Block::Command("sudo pacman -S whisper.cpp".into()))
    );
    // What it would run has to be read: nothing is picked for the person.
    assert!(asked.choices().iter().all(|choice| !choice.recommended));
    // Decided at the revision it was shown at.
    assert!(called(&host, "decide Allow", &["at 7"]));
    assert!(ui.saw("Its turn is done"));
    // Its turn done, the extra counts as set up.
    assert!(!host.progress().left.contains(&Pick::Voice));
}

#[tokio::test]
async fn a_task_refused_stopped_or_left_running_leaves_the_extra_for_later() {
    let mut host = Fake {
        views: Script::of([
            asking("curl -fsSL https://example.com/x | sh"),
            view(TaskState::Working),
        ]),
        ..Fake::default()
    };
    let answers = vec![
        c("start"),
        Answer::Ticks(vec!["voice"]),
        c("agent"),
        c("start"),
        c("deny"),
        c("stop"),
        c("leave"),
        Answer::Quit,
    ];
    let (_, ui) = go(&mut host, with_agent(Step::Options), answers).await;
    assert!(called(&host, "decide Deny { reason: None }", &[]));
    assert!(host.calls.contains(&"stop".to_string()));
    assert!(ui.saw("It carries on; follow it in the app."));
    assert!(host.progress().left.contains(&Pick::Voice));
}

#[tokio::test]
async fn going_back_from_the_agent_does_it_by_hand() {
    let mut host = Fake::default();
    let answers = vec![
        c("start"),
        Answer::Ticks(vec!["voice"]),
        c("agent"),
        c("back"),
        c("later"),
        Answer::Quit,
    ];
    let (_, ui) = go(&mut host, with_agent(Step::Options), answers).await;
    assert!(!called(&host, "start_setup_task", &[]));
    // Back where it started: the same choices, the agent among them.
    let shown = ui
        .screens
        .iter()
        .filter(|screen| words(screen).contains("Let the agent do it"))
        .count();
    assert_eq!(shown, 2);
}

#[tokio::test]
async fn the_agent_finds_projects_and_its_questions_are_answered_here() {
    let picked = |answer: Answer, views: Vec<TaskView>| async move {
        let mut host = Fake {
            views: Script::of(views),
            ..Fake::default()
        };
        let answers = vec![
            c("start"),
            Answer::Ticks(vec!["projects"]),
            c("agent"),
            c("start"),
            answer,
            c("leave"),
            Answer::Quit,
        ];
        let (_, ui) = go(&mut host, with_agent(Step::Options), answers).await;
        (host, ui)
    };
    let folders = ["~/work/app", "~/work/site", "~/notes"];
    let done = view(TaskState::Completed);

    let (host, ui) = picked(c("2"), vec![question(&folders, false), done.clone()]).await;
    assert!(ui.saw("It looks for folders kept in Git, and asks you which to add."));
    assert!(called(&host, "start_setup_task setup-projects", &[]));
    assert!(ui.saw("It asks: Which folders?"));
    assert!(called(
        &host,
        "decide Answer",
        &[
            r#"text: "~/work/site""#,
            r#"choices: ["~/work/site"]"#,
            "at 3"
        ]
    ));

    let (host, _) = picked(
        Answer::Ticks(vec!["1", "3"]),
        vec![question(&folders, true), done],
    )
    .await;
    assert!(called(
        &host,
        "decide Answer",
        &[r#"text: "~/work/app, ~/notes""#]
    ));
}

#[tokio::test]
async fn an_answer_and_a_reply_are_typed_to_the_task() {
    let mut host = Fake {
        views: Script::of([question(&["~/work/app"], false), view(TaskState::Completed)]),
        ..Fake::default()
    };
    let answers = vec![
        c("start"),
        Answer::Ticks(vec!["projects"]),
        c("agent"),
        c("start"),
        c("words"),
        text("Only ~/work, please"),
        c("reply"),
        text(""),
        c("reply"),
        text("Add ~/notes too"),
        c("leave"),
        Answer::Quit,
    ];
    let (_, ui) = go(&mut host, with_agent(Step::Options), answers).await;
    assert!(ui.screens.iter().any(|screen| screen.ask
        == Ask::Text {
            prompt: "Your answer (Enter alone to go back)".into()
        }));
    assert!(called(
        &host,
        "decide Answer",
        &[r#"text: "Only ~/work, please""#, "answers: []"]
    ));
    // An empty reply goes back without saying anything.
    let told: Vec<_> = host
        .calls
        .iter()
        .filter(|call| call.starts_with("tell"))
        .collect();
    assert_eq!(told, ["tell Add ~/notes too"]);
}

#[tokio::test]
async fn what_cannot_be_sent_is_said_on_the_pane() {
    let mut host = Fake {
        views: Script::of([view(TaskState::Completed)]),
        ..Fake::default()
    };
    let answers = vec![
        c("start"),
        Answer::Ticks(vec!["voice"]),
        c("agent"),
        c("start"),
        c("reply"),
        text("refused"),
        c("leave"),
        Answer::Quit,
    ];
    let (_, ui) = go(&mut host, with_agent(Step::Options), answers).await;
    assert!(ui.saw("Not sent: The task is archived."));
}

#[tokio::test]
async fn a_phone_the_agent_brings_is_paired_and_granted_by_the_person() {
    let mut host = Fake {
        waiting: Script::of([Some(prompt(true))]),
        paired_grants: Grants {
            view: true,
            ..Grants::default()
        },
        views: Script::of([view(TaskState::Working), view(TaskState::Completed)]),
        ..Fake::default()
    };
    let answers = vec![
        c("start"),
        c("agent"),
        c("start"),
        c("pair"),
        c("keep"),
        c("leave"),
        Answer::Quit,
    ];
    let (_, ui) = go(&mut host, with_agent(Step::Phone), answers).await;
    assert!(called(&host, "start_setup_task setup-phone", &[]));
    assert!(called(&host, "answer_pairing", &["true"]));
    assert!(ui.saw("Only if your phone shows the same code."));
    // Paired by the time the task is left, the phone step is done.
    assert_eq!(
        host.progress().steps.get(&Step::Phone),
        Some(&Outcome::Done)
    );
    assert!(!ui.saw("Turn on alerts"));
}

#[tokio::test]
async fn the_agent_is_asked_to_fix_what_readiness_found() {
    let mut host = Fake {
        reports: Script::of([
            report(&[
                ("Tailscale", CheckStatus::Passed),
                ("Terminal", CheckStatus::Failed),
                ("Speech", CheckStatus::Failed),
            ]),
            report(&[("Terminal", CheckStatus::Passed)]),
        ]),
        ..Fake::default()
    };
    let answers = vec![
        c("start"),
        c("agent"),
        c("start"),
        c("leave"),
        c("next"),
        Answer::Quit,
    ];
    let (_, ui) = go(&mut host, with_agent(Step::Readiness), answers).await;
    assert!(ui.saw("Ask the agent to fix this"));
    assert!(called(
        &host,
        "start_setup_task setup-fix",
        &["need attention: Terminal; Speech. Fix what you can, please."]
    ));
    let (_, ui) = go(
        &mut Fake::default(),
        with_agent(Step::Readiness),
        vec![c("start"), Answer::Quit],
    )
    .await;
    // All passing, there is nothing to fix.
    assert!(!ui.saw("Ask the agent to fix this"));
}

/// A task's timeline, one item after another.
fn timeline(bodies: Vec<TimelineBody>) -> Vec<TimelineItem> {
    bodies
        .into_iter()
        .enumerate()
        .map(|(n, item)| TimelineItem {
            seq: n as u64 + 1,
            at: Timestamp(0),
            run_id: None,
            item,
        })
        .collect()
}

fn said(text: &str) -> TimelineBody {
    TimelineBody::AgentMessage {
        text: text.into(),
        partial: false,
    }
}

fn did(summary: &str, status: ActivityStatus, key: Option<&str>) -> TimelineBody {
    TimelineBody::Activity {
        tool: "Bash".into(),
        summary: summary.into(),
        status,
        detail: None,
        key: key.map(Into::into),
        not_run: None,
    }
}

fn approval(kind: ApprovalKind, lines: Vec<ApprovalDetailLine>) -> Approval {
    Approval {
        outcome: None,
        approval_id: ApprovalId::new(),
        run_id: RunId::new(),
        task_id: TaskId::new(),
        project_id: ProjectId::new(),
        native_request_id: "request".into(),
        kind,
        title: "Run a command".into(),
        detail: Some("sudo pacman -S whisper.cpp\nto install it".into()),
        detail_lines: lines,
        questions: Vec::new(),
        raw_request: Some(r#"{"command":"sudo pacman -S whisper.cpp"}"#.into()),
        scope: "This once".into(),
        shortened: false,
        revision: 1,
        state: ApprovalState::Pending,
        delivery: None,
        requested_at: Timestamp(0),
        expires_at: None,
        resolved_by: None,
        resolved_at: None,
    }
}

#[test]
fn a_task_shows_what_it_last_said_and_its_latest_calls_since_it_was_told_something() {
    let long: Vec<String> = (1..=20).map(|n| format!("line {n}")).collect();
    let items = timeline(vec![
        did("before the reply", ActivityStatus::Succeeded, Some("0")),
        TimelineBody::UserMessage {
            text: "go on".into(),
            bundle: None,
            delivery: DeliveryState::NativeDelivered,
            carried_plan: None,
        },
        said(&long.join("\n")),
        did("ls", ActivityStatus::Succeeded, Some("1")),
        did("pacman -Si whisper.cpp", ActivityStatus::Running, Some("2")),
        did("whoami", ActivityStatus::Succeeded, None),
        did("whoami", ActivityStatus::Failed, None),
        // The call reported again keeps its place, as it now stands.
        did(
            "pacman -Si whisper.cpp",
            ActivityStatus::Succeeded,
            Some("2"),
        ),
        said("  "),
    ]);
    let view = real::view(TaskState::Working, Some("Looking".into()), None, &items);
    assert_eq!(view.status.as_deref(), Some("Looking"));
    let kept: Vec<&str> = view.said.as_deref().unwrap().lines().collect();
    assert_eq!(kept.len(), 15);
    assert_eq!((kept[0], kept[14]), ("line 6", "line 20"));
    assert_eq!(
        view.doing,
        [
            (Mark::Ok, "pacman -Si whisper.cpp".to_string()),
            (Mark::Ok, "whoami".to_string()),
            (Mark::Stop, "whoami".to_string()),
        ]
    );
    assert!(view.asks.is_none());
}

#[test]
fn an_approval_shows_its_lines_as_they_are_marked() {
    let line = |label: Option<&str>, text: &str, code: bool, change| ApprovalDetailLine {
        label: label.map(Into::into),
        text: text.into(),
        code,
        change,
    };
    let asked = |approval: &Approval| {
        real::view(TaskState::AwaitingDecision, None, Some(approval), &[])
            .asks
            .unwrap()
    };
    let mut command = approval(
        ApprovalKind::Command,
        vec![
            line(None, "sudo pacman -S whisper.cpp", true, None),
            line(Some("In"), "/home/you", false, None),
            line(None, "fn main() {}", false, Some(ApprovalLineChange::Added)),
            line(None, "old", false, Some(ApprovalLineChange::Removed)),
        ],
    );
    let shown = asked(&command);
    assert!(!shown.question);
    assert_eq!(shown.what, "Run a command");
    assert_eq!(
        shown.detail,
        [
            (true, "sudo pacman -S whisper.cpp".to_string()),
            (false, "In: /home/you".to_string()),
            (true, "+ fn main() {}".to_string()),
            (true, "- old".to_string()),
        ]
    );
    // The whole request only when the lines were cut.
    assert_eq!(shown.whole, None);
    command.shortened = true;
    assert_eq!(
        asked(&command).whole.as_deref(),
        Some(r#"{"command":"sudo pacman -S whisper.cpp"}"#)
    );
    // Without lines of its own, its detail line by line.
    let plain = approval(ApprovalKind::Question, Vec::new());
    let shown = asked(&plain);
    assert!(shown.question);
    assert_eq!(
        shown.detail,
        [
            (false, "sudo pacman -S whisper.cpp".to_string()),
            (false, "to install it".to_string()),
        ]
    );
    assert!(shown.choice.is_none());
}

#[test]
fn an_extra_no_longer_offered_is_dropped_and_the_rest_kept() {
    let saved = br#"{"steps":{},"agent":"codex","left":["voice","gone"]}"#;
    let progress: Progress = serde_json::from_slice(saved).unwrap();
    assert_eq!(progress.left, [Pick::Voice]);
    assert_eq!(progress.agent, Some(HarnessKind::Codex));
}
