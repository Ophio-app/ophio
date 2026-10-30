// SPDX-License-Identifier: Apache-2.0
//! Setup's steps, as screens to show and answers to act on.
//!
//! Nothing here draws, reads a terminal or touches the computer: a [`Ui`]
//! shows each [`Screen`] and returns its [`Answer`], and a [`Host`] does the
//! work. Every screen is data, so the plain and the full-screen setup show
//! the same things, and tests read what would have been shown.
use super::{Finished, Outcome, Pick, Progress, Start, Step};
use crate::{
    Error, Result,
    cli::{self, ago, may},
    phone::{self, AlertsAllowed, Installed, Off, Opened, QrPairing},
    readable::shown,
};
use ports::{COMMAND, PRODUCT_NAME};
use protocol::{
    accounts::SignInStage,
    agent_updates::AgentUpdatesView,
    auth::{DeviceRecord, Grants, PairingOffer},
    capability::{AccountReadiness, Availability, HarnessInfo, HarnessKind},
    checklist::{FileDelivery, FileDeliveryView},
    command::{ApprovalDecision, QuestionAnswer},
    github::{GithubAction, GithubMode, GithubProjectView, GithubRepo, GithubSettings},
    ids::{ApprovalId, DeviceId, TaskId},
    launchers::CatalogEntry,
    model::{ApprovalQuestion, CheckStatus, ReadinessReport, TaskState},
    time::Timestamp,
};
use services::api::PairingPrompt;
use std::{
    net::IpAddr,
    path::{Path, PathBuf},
    time::Duration,
};

/// How a fact stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mark {
    Ok,
    /// Wants attention; nothing is broken.
    Warn,
    /// Broken or refused.
    Stop,
    Info,
    /// Under way.
    Wait,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Block {
    Text(String),
    /// "label: detail", marked.
    Fact {
        mark: Mark,
        label: String,
        detail: String,
    },
    /// A command, as it is typed.
    Command(String),
    /// A QR code holding `data`, with `caption` beneath it.
    Qr {
        data: String,
        caption: String,
    },
    Link {
        label: String,
        url: String,
    },
    /// Instructions to follow in order.
    Steps(Vec<String>),
}

/// Commands shown together, as they are drawn, and whether they are drawn
/// with escapes. They are drawn as they are when every one is all plain
/// letters, digits, symbols and spaces and none ends in a space. Otherwise
/// every one of them is drawn with each other character, each backslash and
/// each space at its end written as its escape, and says so beneath: then
/// what is drawn stands for one set of commands only, whatever the colours.
pub(crate) fn exact<'a>(commands: &[&'a str]) -> (Vec<std::borrow::Cow<'a, str>>, bool) {
    use std::fmt::Write;
    let plain = |c: char| matches!(c, ' '..='~');
    if commands
        .iter()
        .all(|command| command.chars().all(plain) && !command.ends_with(' '))
    {
        return (commands.iter().map(|&c| c.into()).collect(), false);
    }
    let escape = |command: &str| {
        let end = command.trim_end_matches(' ').len();
        let mut out = String::with_capacity(command.len() + 16);
        for (at, c) in command.char_indices() {
            match c {
                '\\' => out.push_str("\\\\"),
                ' ' if at >= end => out.push_str("\\u{20}"),
                c if plain(c) => out.push(c),
                c => {
                    let _ = write!(out, "\\u{{{:x}}}", u32::from(c));
                }
            }
        }
        out.into()
    };
    (commands.iter().map(|c| escape(c)).collect(), true)
}

/// What is said beneath commands drawn with escapes.
pub(crate) const ESCAPED: &str =
    "Shown with escapes: each \\u{…} is one character, and \\\\ is a backslash.";

fn text(text: impl Into<String>) -> Block {
    Block::Text(text.into())
}
fn fact(mark: Mark, label: impl Into<String>, detail: impl Into<String>) -> Block {
    Block::Fact {
        mark,
        label: label.into(),
        detail: detail.into(),
    }
}
fn link(label: impl Into<String>, url: impl Into<String>) -> Block {
    Block::Link {
        label: label.into(),
        url: url.into(),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Choice {
    /// The key that picks it: a lower-case letter or a digit.
    pub key: char,
    /// What the answer says was chosen.
    pub id: &'static str,
    pub label: String,
    pub detail: Option<String>,
    /// What Enter alone picks.
    pub recommended: bool,
}
impl Choice {
    pub fn new(key: char, id: &'static str, label: impl Into<String>) -> Self {
        Self {
            key,
            id,
            label: label.into(),
            detail: None,
            recommended: false,
        }
    }
    pub fn recommended(mut self) -> Self {
        self.recommended = true;
        self
    }
    pub fn detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }
}
fn later() -> Choice {
    Choice::new('l', LATER, "Leave this for later")
}
fn skip() -> Choice {
    Choice::new('x', SKIP, "Skip the rest of setup")
}
fn back() -> Choice {
    Choice::new('b', "back", "Back")
}
fn carry_on() -> Choice {
    Choice::new('n', "next", "Continue").recommended()
}
const UPDATES_ON: &str = "Øphio keeps its own copies up to date";
const UPDATES_OFF: &str = "Tasks use the versions installed on this computer";
const LATER: &str = "later";
const SKIP: &str = "skip";
const AGENT: &str = "agent";
/// What the person asks the agent, when the skill says the rest.
const SET_UP: &str = "Set this up for me, please.";
/// The ids of the options of a question an agent asks, in order.
const OPTIONS: [&str; 9] = ["1", "2", "3", "4", "5", "6", "7", "8", "9"];

/// One of a list that can be ticked.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Tick {
    pub id: &'static str,
    pub label: String,
    pub detail: Option<String>,
    pub on: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Ask {
    /// Nothing: the screen stays while something happens.
    Nothing,
    Choose(Vec<Choice>),
    /// A line of text; an empty one when there is no more.
    Text {
        prompt: String,
    },
    /// Which of `ticks` are on, or one of `choices` instead.
    Ticks {
        ticks: Vec<Tick>,
        choices: Vec<Choice>,
    },
}

/// Where a step stands, for the list of steps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Spot {
    Done,
    Later,
    Here,
    Ahead,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Screen {
    pub step: Step,
    pub title: String,
    pub blocks: Vec<Block>,
    pub ask: Ask,
    /// Every numbered step and where it stands.
    pub map: Vec<(Step, Spot)>,
    /// What it asks about, such as one approval as it stands: what was
    /// chosen, ticked or typed is never carried to a screen about another.
    pub about: Option<String>,
}
impl Screen {
    pub fn choices(&self) -> &[Choice] {
        match &self.ask {
            Ask::Choose(choices) | Ask::Ticks { choices, .. } => choices,
            Ask::Nothing | Ask::Text { .. } => &[],
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Answer {
    Choice(&'static str),
    Text(String),
    /// The ids of the ticks left on.
    Ticks(Vec<&'static str>),
    /// The wait ran out with no answer; also the answer to [`Ask::Nothing`].
    Waited,
    /// Ctrl-C, or the end of input.
    Quit,
}

/// Shows screens.
pub(crate) trait Ui {
    /// Shows `screen` and returns its answer. Given `wait`, returns
    /// [`Answer::Waited`] once it has passed; the same screen is usually asked
    /// again straight away, and is not drawn again unless it changed. A
    /// screen that asks [`Ask::Nothing`] is shown and answered at once.
    async fn ask(&mut self, screen: &Screen, wait: Option<Duration>) -> Answer;
    /// Lends the terminal to a program setup runs.
    async fn suspend(&mut self);
    /// Takes the terminal back once that program has ended.
    async fn resume(&mut self);
}

/// What setup finds on this computer before it starts.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Survey {
    pub name: String,
    pub claude: bool,
    pub codex: bool,
    pub tailscale: bool,
    pub npm: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Service {
    pub running: bool,
    pub at_login: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Tailscale {
    Missing,
    Stopped,
    Running {
        ips: Vec<IpAddr>,
        name: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Network {
    pub tailscale: Tailscale,
    /// This computer's address on the local network, if it has one.
    pub wifi: Option<IpAddr>,
    pub allow_lan: bool,
    pub listen: Vec<IpAddr>,
    pub port: u16,
}
impl Network {
    fn wifi_allowed(&self) -> bool {
        self.wifi.is_some_and(|ip| {
            self.allow_lan
                && self.listen.iter().any(|listen| {
                    *listen == ip || listen.is_unspecified() && listen.is_ipv4() == ip.is_ipv4()
                })
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Answered {
    Paired(DeviceRecord),
    Refused,
    /// The device stopped asking.
    Gone,
}

/// A program setup runs in the terminal, once it has been shown and agreed.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Run {
    /// This program, with these arguments.
    Ophio(Vec<String>),
    Program {
        program: String,
        args: Vec<String>,
        /// As the person would type it.
        shown: String,
    },
}
impl Run {
    fn ophio(args: &[&str]) -> Self {
        Self::Ophio(args.iter().map(|a| (*a).to_owned()).collect())
    }
    fn shown(&self) -> String {
        match self {
            Self::Ophio(args) => format!("{COMMAND} {}", args.join(" ")),
            Self::Program { shown, .. } => shown.clone(),
        }
    }
}

/// A setup task, as its pane shows it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TaskView {
    pub state: TaskState,
    /// The agent's own word on how it stands, when it gave one.
    pub status: Option<String>,
    /// What the agent said last.
    pub said: Option<String>,
    /// What it has done since it was last asked, the newest last.
    pub doing: Vec<(Mark, String)>,
    /// What it waits on the person for, if anything.
    pub asks: Option<Asked>,
}

/// Something a setup task asks the person to allow or answer.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Asked {
    pub id: ApprovalId,
    pub revision: u32,
    /// A question to answer, rather than something to allow.
    pub question: bool,
    pub what: String,
    /// What it would do, line by line, each marked when it is code: a
    /// command, a path, exactly as it would run.
    pub detail: Vec<(bool, String)>,
    /// The whole request, when `detail` had to be cut short.
    pub whole: Option<String>,
    /// The one multiple-choice question asked, when that is all it asks.
    pub choice: Option<ApprovalQuestion>,
}

/// Does setup's work on this computer.
pub(crate) trait Host {
    fn save(&mut self, progress: &Progress) -> Result<()>;
    fn survey(&mut self) -> Survey;
    async fn service(&mut self) -> Result<Service>;
    /// Starts Øphio now and at every login; what to tell the person, if
    /// anything.
    async fn install_service(&mut self) -> Result<Option<String>>;
    async fn start(&mut self) -> Result<()>;
    async fn restart(&mut self) -> Result<()>;
    async fn network(&mut self) -> Result<Network>;
    /// Listens on `ip` too, restarting Øphio if it was running.
    async fn allow_wifi(&mut self, ip: IpAddr) -> Result<()>;
    /// Claude Code and Codex, as Øphio sees them now.
    async fn agents(&mut self) -> Result<Vec<HarnessInfo>>;
    /// Whether Øphio keeps its own copies of the agents up to date; `None`
    /// from an Øphio that can't.
    async fn agent_updates(&mut self) -> Result<Option<AgentUpdatesView>>;
    async fn set_agent_updates(&mut self, enabled: bool, revision: u64) -> Result<()>;
    async fn sign_in(&mut self, account_id: &str) -> Result<()>;
    /// The stage of a sign-in to `account_id` begun at `since` or later.
    async fn sign_in_stage(
        &mut self,
        account_id: &str,
        since: Timestamp,
    ) -> Result<Option<SignInStage>>;
    async fn cancel_sign_in(&mut self) -> Result<()>;
    async fn open_pairing(&mut self) -> Result<PairingOffer>;
    async fn waiting_pairing(&mut self) -> Result<Option<PairingPrompt>>;
    async fn answer_pairing(&mut self, code: &str, confirm: bool) -> Result<Answered>;
    async fn devices(&mut self) -> Result<Vec<DeviceRecord>>;
    async fn set_grants(&mut self, device: DeviceId, grants: Grants) -> Result<()>;
    /// Adds the folder as a project; the folder as it was added.
    async fn add_project(&mut self, folder: &str, agent: Option<HarnessKind>) -> Result<String>;
    /// Whether it ended well.
    async fn run(&mut self, run: &Run) -> Result<bool>;
    async fn readiness(&mut self) -> Result<ReadinessReport>;
    fn now(&self) -> Timestamp;
    /// This computer's offset from UTC at that moment, if it can tell.
    fn local_offset(&self, at: time::OffsetDateTime) -> Option<time::UtcOffset>;
    fn config_file(&self) -> PathBuf;
    /// The Øphio app for phones that came with this download, if one did.
    fn phone_app(&self) -> Option<PathBuf>;
    /// Whether Google's `adb` is on this computer.
    fn adb(&mut self) -> bool;
    /// The phones `adb` reaches now.
    async fn phones(&mut self) -> Result<Vec<phone::Device>>;
    /// A new name and password for Android's pairing QR code.
    fn adb_pairing(&mut self) -> Result<QrPairing>;
    /// One look for the phone that scanned `pairing`'s code; once one has,
    /// pairs with it and connects.
    async fn adb_pair_qr(&mut self, pairing: &QrPairing) -> Result<Option<phone::Device>>;
    async fn adb_pair_code(&mut self, address: Option<&str>, code: &str) -> Result<phone::Device>;
    async fn phone_install(&mut self, device: &phone::Device, app: &Path) -> Result<Installed>;
    async fn phone_open(&mut self, device: &phone::Device, app: phone::Open) -> Result<Opened>;
    /// Opens Øphio on the phone with `link`, a pairing link, filled in.
    async fn phone_link(&mut self, device: &phone::Device, link: &str) -> Result<()>;
    async fn phone_alerts(&mut self, device: &phone::Device) -> Result<AlertsAllowed>;
    async fn phone_off(&mut self, device: &phone::Device) -> Result<Off>;
    /// Øphio's built-in setup skill `id`, as its catalog lists it.
    async fn setup_skill(&mut self, id: &str) -> Result<Option<CatalogEntry>>;
    /// Starts `skill` as a one-off task `agent` does, asked `prompt`.
    async fn start_setup_task(
        &mut self,
        skill: &CatalogEntry,
        agent: HarnessKind,
        prompt: &str,
    ) -> Result<TaskId>;
    async fn task_view(&mut self, task: TaskId) -> Result<TaskView>;
    /// Says `text` to the task's agent: at once while it works, otherwise
    /// as its next turn.
    async fn tell_task(&mut self, task: TaskId, text: &str) -> Result<()>;
    /// Decides the request at the revision it was shown at, so a request
    /// that changed since is refused rather than decided unseen.
    async fn decide(
        &mut self,
        approval: ApprovalId,
        revision: u32,
        decision: ApprovalDecision,
    ) -> Result<()>;
    async fn stop_task(&mut self, task: TaskId) -> Result<()>;
    /// The Git projects that aren't one-offs, by name, each with its GitHub
    /// workflow and repository as read now.
    async fn github(&mut self) -> Result<Vec<(String, GithubProjectView)>>;
    /// Changes a project's GitHub workflow, once the change is made.
    async fn github_action(&mut self, action: GithubAction) -> Result<()>;
    /// Where a checklist's files go and where they could; None from an
    /// Øphio before checklists.
    async fn file_delivery(&mut self) -> Result<Option<FileDeliveryView>>;
    /// Changes where they go, once the change is kept.
    async fn set_file_delivery(&mut self, delivery: FileDelivery, revision: u64) -> Result<()>;
}

/// Why a step stopped before its end.
#[derive(Debug)]
pub(crate) enum Halt {
    /// Skip the rest of setup.
    Skip,
    /// Leave setup now.
    Quit,
    /// Leave this for later and go on.
    Later,
    Failed(Error),
}
impl From<Error> for Halt {
    fn from(error: Error) -> Self {
        Self::Failed(error)
    }
}
type Go<T> = std::result::Result<T, Halt>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Ending {
    Done,
    Skipped,
    /// Left part way; the next setup carries on.
    Paused,
    /// A setup gone through again, left part way.
    Stopped,
}
impl Ending {
    pub fn message(self) -> String {
        match self {
            Self::Done => format!(
                "{PRODUCT_NAME} is set up. Change any of it in the app's Settings, or run `{COMMAND} setup` again."
            ),
            Self::Skipped => format!("Setup skipped. Run `{COMMAND} setup` whenever you want it."),
            Self::Paused => {
                format!("Setup is paused. Run `{COMMAND} setup` to carry on where you left off.")
            }
            Self::Stopped => format!(
                "Setup stopped. Nothing you set up is undone; run `{COMMAND} setup` to go through it again."
            ),
        }
    }
}

/// Where the app is downloaded from, until it is in a store.
pub(crate) const APP_DOWNLOAD_URL: &str = "https://ophio.app/download";
const TAILSCALE_URL: &str = "https://tailscale.com/download";
const NODE_URL: &str = "https://nodejs.org/en/download";
const CODEX_URL: &str = "https://github.com/openai/codex";
const NPM_EACCES_URL: &str =
    "https://docs.npmjs.com/resolving-eacces-permissions-errors-when-installing-packages-globally";
/// How long a screen that watches something waits between looks.
const POLL: Duration = Duration::from_secs(1);
/// Looks at a sign-in that has not begun before giving up on it.
const SIGN_IN_START: u32 = 30;

#[cfg(windows)]
const LOGIN_NOTE: &str = "It starts with Windows: Settings › Apps › Startup lists it. To stop that, run `.\\ophio.exe service uninstall`.";
#[cfg(not(windows))]
const LOGIN_NOTE: &str = "It runs as your own systemd service, ophio.service. To stop that, run `ophio service uninstall`.";
#[cfg(windows)]
const TAILSCALE_UP: &str =
    "Start it from its icon in the corner of the taskbar and sign in, then check again.";
#[cfg(not(windows))]
const TAILSCALE_UP: &str =
    "Start it with `sudo tailscale up`, or from its menu, and sign in; then check again.";

pub(crate) fn title(step: Step) -> &'static str {
    match step {
        Step::Welcome => "Welcome to Øphio",
        Step::Background => "Keep Øphio running",
        Step::Reach => "How your phone reaches this computer",
        Step::Agent => "Connect your agents",
        Step::Phone => "Your phone",
        Step::Options => "Extras",
        Step::Readiness => "Check everything works",
        Step::Done => "Done",
    }
}

fn agent_name(kind: HarnessKind) -> &'static str {
    match kind {
        HarnessKind::Codex => "Codex",
        _ => "Claude Code",
    }
}
/// The account Øphio signs this agent in with.
fn account_id(kind: HarnessKind) -> &'static str {
    match kind {
        HarnessKind::Codex => "codex",
        _ => "claude",
    }
}
const AGENTS: [HarnessKind; 2] = [HarnessKind::ClaudeCode, HarnessKind::Codex];

fn mode_id(mode: GithubMode) -> &'static str {
    match mode {
        GithubMode::Off => "off",
        GithubMode::ThroughMerge => "through_merge",
        GithubMode::Drafts => "drafts",
    }
}
fn mode_words(mode: GithubMode) -> &'static str {
    match mode {
        GithubMode::Off => "Off",
        GithubMode::ThroughMerge => "Through merge",
        GithubMode::Drafts => "Drafts",
    }
}
/// What each GitHub workflow does, as the app says it.
fn mode_hint(mode: GithubMode) -> &'static str {
    match mode {
        GithubMode::Off => "Tasks aren't linked to GitHub.",
        GithubMode::ThroughMerge => {
            "The agent opens a draft pull request, keeps its checks passing, then marks it ready and merges it."
        }
        GithubMode::Drafts => {
            "The agent opens a draft pull request and keeps its checks passing. You mark it ready and merge it."
        }
    }
}

const PICKS: [Pick; 10] = [
    Pick::Projects,
    Pick::Files,
    Pick::Desktop,
    Pick::Voice,
    Pick::Telegram,
    Pick::Discord,
    Pick::Accounts,
    Pick::Github,
    Pick::AgentLogins,
    Pick::VoiceControl,
];
impl Pick {
    fn id(self) -> &'static str {
        match self {
            Pick::Projects => "projects",
            Pick::Files => "files",
            Pick::Desktop => "desktop",
            Pick::Voice => "voice",
            Pick::Telegram => "telegram",
            Pick::Discord => "discord",
            Pick::Accounts => "accounts",
            Pick::Github => "github",
            Pick::AgentLogins => "agent_logins",
            Pick::VoiceControl => "voice_control",
        }
    }
    pub(crate) fn label(self) -> &'static str {
        match self {
            Pick::Projects => "Add your projects",
            Pick::Files => "Choose where files go",
            Pick::Desktop => "Watch and control this desktop",
            Pick::Voice => "Talk to your agents",
            Pick::Telegram => "Message your agents on Telegram",
            Pick::Discord => "Message your agents on Discord",
            Pick::Accounts => "Give tasks your other accounts",
            Pick::Github => "Let agents open pull requests",
            Pick::AgentLogins => "Let agents sign in to websites",
            Pick::VoiceControl => "Control the computer by voice",
        }
    }
    /// The built-in skill an agent sets this up with, if there is one.
    fn skill(self) -> Option<&'static str> {
        match self {
            Pick::Projects => Some("setup-projects"),
            Pick::Desktop => Some("setup-desktop"),
            Pick::Voice => Some("setup-voice"),
            Pick::Telegram => Some("setup-telegram"),
            Pick::Discord => Some("setup-discord"),
            Pick::Accounts => Some("setup-accounts"),
            // Where files go, how far an agent may go on GitHub, accepting
            // the logins' notice and typing a key are all the person's.
            Pick::Files | Pick::Github | Pick::AgentLogins | Pick::VoiceControl => None,
        }
    }
    fn detail(self) -> &'static str {
        match self {
            Pick::Projects => "The folders your agents work in",
            Pick::Files => "Files from your checklists: this phone, Taildrop, a folder or a drive",
            Pick::Desktop => "From your phone, once you allow it",
            Pick::Voice => "A 151 MB speech model, recognised on this computer",
            Pick::Telegram => "Through a bot of your own",
            Pick::Discord => "Through a bot of your own",
            Pick::Accounts => "GitHub, Google and others, kept on this computer",
            Pick::Github => "On GitHub, as the account signed in under Accounts",
            Pick::AgentLogins => "Passwords kept on this computer; read its notice first",
            Pick::VoiceControl => "Through TypeSafe's Jev, at your own cost",
        }
    }
}

struct Flow<'a, H, U> {
    host: &'a mut H,
    ui: &'a mut U,
    progress: Progress,
    rerun: bool,
    resumed: bool,
    /// How many pairing requests have been asked about, so that each is
    /// asked about as itself: nothing begun on one answers the next.
    pairings: u64,
}

/// Goes through setup from `start`, saving its progress after every step.
pub(crate) async fn run<H: Host, U: Ui>(host: &mut H, ui: &mut U, start: Start) -> Result<Ending> {
    let (progress, rerun, resumed) = match start {
        Start::First(progress) => (progress, false, false),
        Start::Resume(progress) => (progress, false, true),
        Start::Rerun(progress) => (progress, true, false),
    };
    let mut flow = Flow {
        host,
        ui,
        progress,
        rerun,
        resumed,
        pairings: 0,
    };
    if let Err(halt) = flow.welcome().await {
        return flow.halted(halt);
    }
    for step in Step::NUMBERED {
        if !flow.rerun && flow.progress.steps.contains_key(&step) {
            continue;
        }
        loop {
            let outcome = match flow.step(step).await {
                Ok(outcome) => outcome,
                Err(Halt::Later) => Outcome::Later,
                Err(Halt::Failed(error)) => match flow.trouble(step, &error).await {
                    Ok(()) => continue,
                    Err(Halt::Later) => Outcome::Later,
                    Err(halt) => return flow.halted(halt),
                },
                Err(halt) => return flow.halted(halt),
            };
            flow.record(step, outcome);
            break;
        }
        flow.save()?;
    }
    match flow.done().await {
        Ok(()) | Err(Halt::Quit | Halt::Later | Halt::Skip) => Ok(Ending::Done),
        Err(Halt::Failed(error)) => Err(error),
    }
}

impl<H: Host, U: Ui> Flow<'_, H, U> {
    fn save(&mut self) -> Result<()> {
        self.host.save(&self.progress)
    }

    /// A step done stays done when it is gone through again and left.
    fn record(&mut self, step: Step, outcome: Outcome) {
        match outcome {
            Outcome::Done => {
                self.progress.steps.insert(step, outcome);
            }
            Outcome::Later => {
                self.progress.steps.entry(step).or_insert(outcome);
            }
        }
    }

    fn halted(&mut self, halt: Halt) -> Result<Ending> {
        match halt {
            Halt::Failed(error) => {
                // The error is the one worth reporting.
                let _ = self.save();
                Err(error)
            }
            Halt::Skip => {
                self.progress.finished.get_or_insert(Finished::Skipped);
                self.save()?;
                Ok(if self.rerun {
                    Ending::Stopped
                } else {
                    Ending::Skipped
                })
            }
            Halt::Quit | Halt::Later => {
                self.save()?;
                Ok(if self.rerun {
                    Ending::Stopped
                } else {
                    Ending::Paused
                })
            }
        }
    }

    fn map(&self, here: Step) -> Vec<(Step, Spot)> {
        Step::NUMBERED
            .iter()
            .map(|&step| {
                let spot = if step == here {
                    Spot::Here
                } else {
                    match self.progress.steps.get(&step) {
                        Some(Outcome::Done) => Spot::Done,
                        Some(Outcome::Later) => Spot::Later,
                        None => Spot::Ahead,
                    }
                };
                (step, spot)
            })
            .collect()
    }

    fn screen(&self, step: Step, blocks: Vec<Block>, ask: Ask) -> Screen {
        Screen {
            step,
            title: title(step).into(),
            blocks,
            ask,
            map: self.map(step),
            about: None,
        }
    }

    /// The answer, unless it leaves the step or setup.
    async fn reply(&mut self, screen: &Screen, wait: Option<Duration>) -> Go<Answer> {
        match self.ui.ask(screen, wait).await {
            Answer::Choice(LATER) => Err(Halt::Later),
            Answer::Choice(SKIP) => Err(Halt::Skip),
            Answer::Quit => Err(Halt::Quit),
            Answer::Waited if wait.is_none() => Err(Halt::Quit),
            answer => Ok(answer),
        }
    }

    async fn choose(
        &mut self,
        step: Step,
        blocks: Vec<Block>,
        choices: Vec<Choice>,
    ) -> Go<&'static str> {
        let screen = self.screen(step, blocks, Ask::Choose(choices));
        match self.reply(&screen, None).await? {
            Answer::Choice(id) => Ok(id),
            _ => Err(Halt::Quit),
        }
    }

    /// Shows what was done, until the person goes on.
    async fn notice(&mut self, step: Step, blocks: Vec<Block>) -> Go<()> {
        self.choose(step, blocks, vec![carry_on()]).await.map(drop)
    }

    /// Shows that something is under way, while it is.
    async fn busy(&mut self, step: Step, doing: &str) {
        let screen = self.screen(step, vec![fact(Mark::Wait, doing, "")], Ask::Nothing);
        self.ui.ask(&screen, None).await;
    }

    /// Runs a program in the terminal; whether it ended well.
    async fn run_program(&mut self, run: &Run) -> Go<bool> {
        self.ui.suspend().await;
        let ended = self.host.run(run).await;
        self.ui.resume().await;
        Ok(ended?)
    }

    /// What went wrong, and whether to try again: `Ok` to try again.
    async fn trouble(&mut self, step: Step, error: &Error) -> Go<()> {
        let blocks = vec![
            text("Something went wrong."),
            fact(Mark::Stop, "Problem", shown(&error.to_string())),
        ];
        let choices = vec![
            Choice::new('r', "retry", "Try again").recommended(),
            later(),
            skip(),
        ];
        self.choose(step, blocks, choices).await.map(drop)
    }

    async fn ensure_running(&mut self, step: Step) -> Go<()> {
        while !self.host.service().await?.running {
            let blocks = vec![text(format!(
                "This step needs {PRODUCT_NAME} running, and it isn't."
            ))];
            let choices = vec![
                Choice::new('s', "start", format!("Start {PRODUCT_NAME} now"))
                    .recommended()
                    .detail("Until you log out. Keep Øphio running, the first step, starts it at every login."),
                later(),
                skip(),
            ];
            self.choose(step, blocks, choices).await?;
            self.busy(step, "Starting Øphio…").await;
            self.host.start().await?;
        }
        Ok(())
    }

    /// Offers to have the connected agent do it, when one is connected.
    fn let_agent(&self, label: &str) -> Option<Choice> {
        let agent = self.progress.agent?;
        Some(Choice::new('g', AGENT, label).detail(format!(
            "{} does it in a task you follow here. It asks you before it installs or changes anything.",
            agent_name(agent)
        )))
    }

    /// Has the agent do something with one of setup's own skills, in a task
    /// the person follows here: how the task stood when they left it, or
    /// `None` if it never started.
    async fn agent_task(&mut self, step: Step, skill: &str, prompt: &str) -> Go<Option<TaskState>> {
        let Some(agent) = self.progress.agent else {
            return Ok(None);
        };
        self.ensure_running(step).await?;
        let Some(skill) = self.host.setup_skill(skill).await? else {
            return Err(Error::Ended(format!(
                "This {PRODUCT_NAME} has no setup skill called {skill}."
            ))
            .into());
        };
        let name = shown(&skill.name);
        let blocks = vec![
            fact(
                Mark::Info,
                name.clone(),
                skill.description.as_deref().map(shown).unwrap_or_default(),
            ),
            text(format!(
                "{} does this as a task, in a folder of its own; you follow it here and in the app. Before it runs a command that installs or changes anything, it asks you here and shows exactly what would run.",
                agent_name(agent)
            )),
            text(
                "Confirming a pairing, choosing what a device may do, signing in, and typing passwords or tokens stay with you.",
            ),
        ];
        let choices = vec![Choice::new('s', "start", "Start it").recommended(), back()];
        if self.choose(step, blocks, choices).await? == "back" {
            return Ok(None);
        }
        self.busy(step, "Starting the task…").await;
        let task = self.host.start_setup_task(&skill, agent, prompt).await?;
        self.follow(step, &name, task).await.map(Some)
    }

    /// Shows a setup task as it goes, and passes on what the person says to
    /// it, until they leave it: how it stood then.
    async fn follow(&mut self, step: Step, name: &str, task: TaskId) -> Go<TaskState> {
        // What became of the last thing the person did, when it went wrong.
        let mut news: Option<Block> = None;
        loop {
            // A device the task brought to pair is the person's to confirm.
            match self.host.waiting_pairing().await? {
                Some(prompt) if !prompt.shows_code => {
                    self.host.answer_pairing(&prompt.code, false).await?;
                    news = Some(fact(
                        Mark::Warn,
                        "Not paired",
                        cli::pairing_too_old(&prompt),
                    ));
                }
                Some(prompt) => {
                    if let Answered::Paired(device) = self.confirm(step, &prompt).await? {
                        self.grant(step, device).await?;
                    }
                }
                None => {}
            }
            let view = self.host.task_view(task).await?;
            let mut blocks = vec![fact(Mark::Info, name, ""), standing(&view)];
            blocks.extend(view.said.as_deref().map(|said| text(lines(said))));
            blocks.extend(
                view.doing
                    .iter()
                    .map(|(mark, done)| fact(*mark, shown(done), "")),
            );
            blocks.extend(news.clone());
            let mut choices = Vec::new();
            let mut ticks = None;
            let question = view
                .asks
                .as_ref()
                .and_then(|asked| asked.choice.as_ref())
                .filter(|choice| (1..=OPTIONS.len()).contains(&choice.options.len()));
            match (&view.asks, question) {
                (Some(asked), _) if !asked.question => {
                    blocks.push(fact(Mark::Warn, "It asks to", shown(&asked.what)));
                    blocks.extend(detail(asked));
                    // Neither is the default: what it asks has to be read first.
                    choices.push(Choice::new('a', "allow", "Allow"));
                    choices.push(Choice::new('d', "deny", "Don't allow"));
                }
                (Some(_), Some(question)) => {
                    blocks.push(fact(Mark::Warn, "It asks", lines(&question.question)));
                    let options = question.options.iter().zip(OPTIONS);
                    if question.multi_select {
                        ticks = Some(
                            options
                                .map(|(option, id)| Tick {
                                    id,
                                    label: shown(&option.label),
                                    detail: option.description.as_deref().map(shown),
                                    on: false,
                                })
                                .collect(),
                        );
                    } else {
                        choices.extend(options.map(|(option, id)| {
                            let key = id.chars().next().unwrap_or('1');
                            let choice = Choice::new(key, id, shown(&option.label));
                            match &option.description {
                                Some(about) => choice.detail(shown(about)),
                                None => choice,
                            }
                        }));
                    }
                    choices.push(Choice::new('a', "words", "Answer in your own words"));
                }
                (Some(asked), None) => {
                    blocks.push(fact(Mark::Warn, "It asks", shown(&asked.what)));
                    blocks.extend(detail(asked));
                    choices.push(Choice::new('a', "words", "Answer"));
                }
                (None, _) => choices.push(Choice::new('r', "reply", "Reply")),
            }
            if going(view.state) {
                choices.push(Choice::new('s', "stop", "Stop it"));
                choices.push(
                    Choice::new('b', "leave", "Leave it running")
                        .detail("It carries on; follow it in the app."),
                );
            } else {
                choices.push(Choice::new('n', "leave", "Go on").recommended());
            }
            let ask = match ticks {
                Some(ticks) => Ask::Ticks { ticks, choices },
                None => Ask::Choose(choices),
            };
            let asked = view.asks.as_ref();
            let about = asked.map(|asked| format!("{} {}", asked.id, asked.revision));
            let screen = Screen {
                about: about.clone(),
                ..self.screen(step, blocks.clone(), ask)
            };
            let sent = match self.reply(&screen, Some(POLL)).await? {
                Answer::Choice("leave") => return Ok(view.state),
                Answer::Choice("stop") => self.host.stop_task(task).await,
                Answer::Choice(said @ ("allow" | "deny")) => {
                    let Some(asked) = asked else { continue };
                    let decision = if said == "allow" {
                        ApprovalDecision::Allow
                    } else {
                        ApprovalDecision::Deny { reason: None }
                    };
                    self.host.decide(asked.id, asked.revision, decision).await
                }
                Answer::Choice(said @ ("reply" | "words")) => {
                    let prompt = if said == "words" {
                        "Your answer (Enter alone to go back)"
                    } else {
                        "Your reply (Enter alone to go back)"
                    };
                    let screen = Screen {
                        about,
                        ..self.screen(
                            step,
                            blocks,
                            Ask::Text {
                                prompt: prompt.into(),
                            },
                        )
                    };
                    let Answer::Text(words) = self.reply(&screen, None).await? else {
                        continue;
                    };
                    if words.is_empty() {
                        continue;
                    }
                    match asked {
                        Some(asked) if said == "words" => {
                            let decision = ApprovalDecision::Answer {
                                text: words,
                                answers: vec![],
                            };
                            self.host.decide(asked.id, asked.revision, decision).await
                        }
                        _ => self.host.tell_task(task, &words).await,
                    }
                }
                Answer::Choice(option) => {
                    let picked = OPTIONS.iter().position(|id| *id == option);
                    let (Some(asked), Some(question), Some(picked)) = (asked, question, picked)
                    else {
                        continue;
                    };
                    let decision = answer(question, &[picked]);
                    self.host.decide(asked.id, asked.revision, decision).await
                }
                Answer::Ticks(on) => {
                    let picked: Vec<usize> = OPTIONS
                        .iter()
                        .enumerate()
                        .filter(|(_, id)| on.contains(id))
                        .map(|(n, _)| n)
                        .collect();
                    let (Some(asked), Some(question)) = (asked, question) else {
                        continue;
                    };
                    if picked.is_empty() {
                        continue;
                    }
                    let decision = answer(question, &picked);
                    self.host.decide(asked.id, asked.revision, decision).await
                }
                _ => continue,
            };
            // Øphio turning it down is said here; anything worse ends setup.
            let refused = match sent {
                Ok(()) => None,
                Err(Error::Protocol(error)) => Some(error.message),
                Err(Error::Ended(message)) => Some(message),
                Err(error) => return Err(error.into()),
            };
            news = refused.map(|why| fact(Mark::Warn, "Not sent", shown(&why)));
        }
    }

    async fn step(&mut self, step: Step) -> Go<Outcome> {
        match step {
            Step::Background => self.background().await,
            Step::Reach => self.reach().await,
            Step::Agent => self.agents().await,
            Step::Phone => self.phone().await,
            Step::Options => self.extras().await,
            Step::Readiness => self.readiness().await,
            Step::Welcome | Step::Done => Ok(Outcome::Done),
        }
    }

    async fn welcome(&mut self) -> Go<()> {
        let survey = self.host.survey();
        let found = |name: &str, found: bool| {
            if found {
                fact(Mark::Ok, name, "Found")
            } else {
                fact(Mark::Info, name, "Not found")
            }
        };
        let mut blocks = vec![
            text(format!(
                "{PRODUCT_NAME} connects your phone or tablet to this computer: run Claude Code and Codex tasks on it, answer them, and watch its desktop, from anywhere."
            )),
            text(
                "Setup keeps Øphio running, works out how your phone reaches this computer, connects your agents, pairs your phone and checks that it all works. It takes a few minutes, and any step can be left for later.",
            ),
            fact(Mark::Info, "This computer", shown(&survey.name)),
            found("Claude Code", survey.claude),
            found("Codex", survey.codex),
            found("Tailscale", survey.tailscale),
        ];
        let start = if self.rerun {
            blocks.push(text(
                "Setup has been through before, so every step is shown again. Nothing you set up is undone unless you change it.",
            ));
            "Start setup"
        } else if self.resumed {
            let behind = self.progress.steps.len();
            blocks.push(text(format!(
                "Setup carries on where it was left: {behind} of {} steps are behind you.",
                Step::NUMBERED.len()
            )));
            "Carry on"
        } else {
            "Start setup"
        };
        let choices = vec![
            Choice::new('s', "start", start).recommended(),
            Choice::new('x', SKIP, "Skip setup")
                .detail(format!("Run `{COMMAND} setup` whenever you want it.")),
        ];
        self.choose(Step::Welcome, blocks, choices).await.map(drop)
    }

    async fn background(&mut self) -> Go<Outcome> {
        const STEP: Step = Step::Background;
        let service = self.host.service().await?;
        if service.running && service.at_login {
            let blocks = vec![
                fact(
                    Mark::Ok,
                    PRODUCT_NAME,
                    "Running now, and starts whenever you log in.",
                ),
                text(LOGIN_NOTE),
            ];
            let choices = vec![Choice::new('n', "next", "Next").recommended(), skip()];
            self.choose(STEP, blocks, choices).await?;
            return Ok(Outcome::Done);
        }
        let blocks = vec![
            text(
                "Your phone can reach this computer only while Øphio is running on it. It can start whenever you log in, so it's there when you need it.",
            ),
            if service.running {
                fact(Mark::Ok, "Now", "Running")
            } else {
                fact(Mark::Warn, "Now", "Not running")
            },
            fact(Mark::Warn, "When you log in", "Doesn't start"),
            text(LOGIN_NOTE),
        ];
        let choices = vec![
            Choice::new('s', "login", "Start now and at every login").recommended(),
            Choice::new('j', "now", "Just for now")
                .detail("Until you log out. Your phone can't reach this computer after that."),
            later(),
            skip(),
        ];
        match self.choose(STEP, blocks, choices).await? {
            "login" => {
                self.busy(STEP, "Setting Øphio to start at every login…")
                    .await;
                let note = self.host.install_service().await?;
                let mut blocks = vec![fact(
                    Mark::Ok,
                    PRODUCT_NAME,
                    "Running now, and starts whenever you log in.",
                )];
                blocks.extend(note.map(text));
                self.notice(STEP, blocks).await?;
                Ok(Outcome::Done)
            }
            _ => {
                if !service.running {
                    self.busy(STEP, "Starting Øphio…").await;
                    self.host.start().await?;
                }
                Ok(Outcome::Later)
            }
        }
    }

    async fn reach(&mut self) -> Go<Outcome> {
        const STEP: Step = Step::Reach;
        loop {
            let network = self.host.network().await?;
            let mut blocks = vec![text(
                "Your phone reaches this computer through Tailscale, a private network between your own devices that works wherever you are, or through the Wi‑Fi you share at home.",
            )];
            match &network.tailscale {
                Tailscale::Missing => {
                    blocks.push(fact(
                        Mark::Warn,
                        "Tailscale",
                        "Not installed on this computer",
                    ));
                    blocks.push(link("Get it at", TAILSCALE_URL));
                    blocks.push(text(
                        "Install it here and on your phone, sign in to the same account on both, then check again.",
                    ));
                }
                Tailscale::Stopped => {
                    blocks.push(fact(Mark::Warn, "Tailscale", "Installed, not connected"));
                    blocks.push(text(TAILSCALE_UP));
                }
                Tailscale::Running { ips, name } => {
                    let who = name.clone().unwrap_or_else(|| {
                        ips.iter()
                            .map(ToString::to_string)
                            .collect::<Vec<_>>()
                            .join(", ")
                    });
                    blocks.push(fact(
                        Mark::Ok,
                        "Tailscale",
                        format!("Connected as {}", shown(&who)),
                    ));
                    blocks.push(text(
                        "Your phone needs Tailscale too, signed in to the same account.",
                    ));
                }
            }
            let allowed = network.wifi_allowed();
            blocks.push(match network.wifi {
                Some(ip) if allowed => fact(
                    Mark::Ok,
                    "Wi‑Fi",
                    format!("Phones on this network can reach {ip}, port {}", network.port),
                ),
                Some(ip) => fact(
                    Mark::Info,
                    "Wi‑Fi",
                    format!("This computer is {ip} here; phones on this network can't reach it unless you allow it"),
                ),
                None => fact(Mark::Info, "Wi‑Fi", "No local network found"),
            });
            let tailscale = matches!(network.tailscale, Tailscale::Running { .. });
            let mut choices = Vec::new();
            if tailscale {
                choices.push(Choice::new('n', "next", "Use Tailscale").recommended());
            } else if allowed {
                choices.push(Choice::new('n', "next", "Use this Wi‑Fi").recommended());
                choices.push(Choice::new('c', "check", "Check Tailscale again"));
            } else {
                choices.push(Choice::new('c', "check", "Check again").recommended());
            }
            let wifi = network.wifi.filter(|_| !allowed);
            if wifi.is_some() {
                choices.push(
                    Choice::new('w', "wifi", "This Wi‑Fi only")
                        .detail("Your phone reaches this computer only at home, on this network."),
                );
            }
            if !tailscale {
                choices.extend(self.let_agent("Let the agent set up Tailscale"));
            }
            choices.extend([later(), skip()]);
            let done = match (self.choose(STEP, blocks, choices).await?, wifi) {
                ("next", _) => true,
                ("wifi", Some(ip)) => self.allow_wifi(ip, network.port).await?,
                (AGENT, _) => {
                    self.agent_task(STEP, "setup-tailscale", SET_UP).await?;
                    false
                }
                _ => false,
            };
            if done {
                return Ok(Outcome::Done);
            }
        }
    }

    /// Whether the Wi‑Fi was allowed.
    async fn allow_wifi(&mut self, ip: IpAddr, port: u16) -> Go<bool> {
        const STEP: Step = Step::Reach;
        let mut blocks = vec![
            text(format!(
                "{PRODUCT_NAME} will also listen at {ip}, port {port}, so that a phone on this Wi‑Fi can reach it."
            )),
            text(
                "Only phones you pair can connect, and everything between them is encrypted. Allow it only on a network you trust, such as your home's.",
            ),
        ];
        if cfg!(windows) {
            blocks.push(text("Windows may ask whether to let Øphio through its firewall: allow it on private networks, and check that this Wi‑Fi is a private network in Settings › Network & internet."));
        }
        blocks.push(text(format!(
            "This restarts Øphio if it is running, and is saved in {}. If this computer's address on the network changes, run `{COMMAND} setup` again.",
            self.host.config_file().display()
        )));
        let choices = vec![
            Choice::new('y', "allow", "Allow this Wi‑Fi").recommended(),
            back(),
        ];
        if self.choose(STEP, blocks, choices).await? != "allow" {
            return Ok(false);
        }
        self.busy(STEP, "Allowing this Wi‑Fi…").await;
        self.host.allow_wifi(ip).await?;
        self.notice(
            STEP,
            vec![fact(
                Mark::Ok,
                "Wi‑Fi",
                format!("Phones on this network can reach {ip}, port {port}"),
            )],
        )
        .await?;
        Ok(true)
    }

    async fn agents(&mut self) -> Go<Outcome> {
        const STEP: Step = Step::Agent;
        self.ensure_running(STEP).await?;
        loop {
            let agents = self.host.agents().await?;
            let updates = self
                .host
                .agent_updates()
                .await?
                .filter(|view| view.supported);
            let found = |kind| agents.iter().find(|a| a.kind == kind);
            let ready: Vec<HarnessKind> = AGENTS
                .into_iter()
                .filter(|&kind| found(kind).is_some_and(ready))
                .collect();
            if !self.progress.agent.is_some_and(|a| ready.contains(&a)) {
                self.progress.agent = ready.first().copied();
            }
            let mut blocks = vec![text(
                "Øphio runs tasks with the agents installed on this computer, signed in to your own accounts. One is enough.",
            )];
            let mut choices = Vec::new();
            for (kind, key) in AGENTS.into_iter().zip(['1', '2']) {
                let name = agent_name(kind);
                let Some(agent) = found(kind).filter(|a| a.installed == Availability::Available)
                else {
                    blocks.push(fact(Mark::Info, name, "Not installed"));
                    choices.push(Choice::new(
                        key,
                        install_id(kind),
                        format!("Install {name}"),
                    ));
                    continue;
                };
                match &agent.account {
                    AccountReadiness::SignedIn {
                        account_label,
                        plan,
                    } => {
                        let who = match (account_label, plan) {
                            (Some(label), Some(plan)) => {
                                format!("Signed in as {} ({})", shown(label), shown(plan))
                            }
                            (Some(label), None) => format!("Signed in as {}", shown(label)),
                            (None, Some(plan)) => format!("Signed in ({})", shown(plan)),
                            (None, None) => "Signed in".into(),
                        };
                        blocks.push(fact(Mark::Ok, name, who));
                    }
                    AccountReadiness::SignedOut { .. } => {
                        blocks.push(fact(Mark::Warn, name, "Installed, not signed in"));
                        choices.push(Choice::new(
                            key,
                            sign_in_id(kind),
                            format!("Sign in to {name}"),
                        ));
                    }
                    AccountReadiness::Unknown { reason } => {
                        blocks.push(fact(Mark::Info, name, shown(reason)));
                    }
                }
            }
            if ready.len() > 1
                && let Some(agent) = self.progress.agent
            {
                blocks.push(fact(
                    Mark::Info,
                    "New projects start with",
                    agent_name(agent),
                ));
                choices.push(Choice::new(
                    'd',
                    "default",
                    "Choose which new projects start with",
                ));
            }
            let installed = agents
                .iter()
                .any(|agent| agent.installed == Availability::Available);
            if let Some(view) = updates.as_ref().filter(|_| installed) {
                if view.enabled {
                    blocks.push(fact(Mark::Ok, "Updates", UPDATES_ON));
                    choices.push(Choice::new(
                        'u',
                        "updates",
                        "Use the versions installed on this computer instead",
                    ));
                } else {
                    blocks.push(fact(Mark::Info, "Updates", UPDATES_OFF));
                    choices.push(Choice::new(
                        'u',
                        "updates",
                        "Keep Claude Code and Codex up to date",
                    ));
                }
            }
            let next = if ready.is_empty() {
                Choice::new('n', "next", "Next")
                    .detail("No agent is ready yet; tasks can't start until one is.")
            } else {
                Choice::new('n', "next", "Next").recommended()
            };
            choices.insert(0, next);
            choices.extend([later(), skip()]);
            let chosen = self.choose(STEP, blocks, choices).await?;
            match chosen {
                "next" if ready.is_empty() => return Ok(Outcome::Later),
                "next" => {
                    if let Some(view) = updates.filter(|view| {
                        installed && !view.enabled && self.progress.updates.is_none()
                    }) && !self.ask_updates(view.revision).await?
                    {
                        continue;
                    }
                    return Ok(Outcome::Done);
                }
                "default" => self.choose_default(&ready).await?,
                "updates" => {
                    if let Some(view) = updates {
                        self.set_updates(!view.enabled, view.revision).await?;
                    }
                }
                _ => {
                    for kind in AGENTS {
                        if chosen == install_id(kind) {
                            self.install(kind).await?;
                        } else if chosen == sign_in_id(kind) {
                            let remedy = found(kind).and_then(|a| match &a.account {
                                AccountReadiness::SignedOut { remedy } => Some(remedy.clone()),
                                _ => None,
                            });
                            self.sign_in(kind, remedy).await?;
                        }
                    }
                }
            }
        }
    }

    /// Asks whether Øphio keeps the agents up to date; false on Back.
    async fn ask_updates(&mut self, revision: u64) -> Go<bool> {
        let blocks = vec![
            text("Keep Claude Code and Codex up to date?"),
            text(
                "Øphio can keep its own copies of them and update the copies from Anthropic and OpenAI as new versions come out, so new models show up without you updating anything. The versions installed on this computer stay as they are.",
            ),
            text("The copies take up to about 1.5 GB of disk space."),
        ];
        let choices = vec![
            Choice::new('y', "on", "Keep them up to date")
                .recommended()
                .detail(
                    "Øphio checks every few hours and tries each new version before tasks use it.",
                ),
            Choice::new('n', "off", "Use the versions installed on this computer")
                .detail("Turn this on later in the app's Settings › Agent apps, or in this step."),
            back(),
        ];
        let enabled = match self.choose(Step::Agent, blocks, choices).await? {
            "on" => true,
            "off" => false,
            _ => return Ok(false),
        };
        self.set_updates(enabled, revision).await?;
        Ok(true)
    }

    async fn set_updates(&mut self, enabled: bool, revision: u64) -> Go<()> {
        match self.host.set_agent_updates(enabled, revision).await {
            Ok(()) => self.progress.updates = Some(enabled),
            // The step shows where it stands now.
            Err(Error::Protocol(error)) => {
                let why = if error.code == protocol::ErrorCode::StaleRevision {
                    "It was changed in the app just now, so it was left as it is.".into()
                } else {
                    shown(&error.message)
                };
                self.notice(Step::Agent, vec![fact(Mark::Warn, "Updates", why)])
                    .await?;
            }
            Err(error) => return Err(error.into()),
        }
        Ok(())
    }

    async fn choose_default(&mut self, ready: &[HarnessKind]) -> Go<()> {
        let blocks = vec![text(
            "Which agent should new projects start with? Each project and each task can still use either.",
        )];
        let mut choices: Vec<Choice> = ready
            .iter()
            .zip(['1', '2'])
            .map(|(&kind, key)| {
                let choice = Choice::new(key, account_id(kind), agent_name(kind));
                if Some(kind) == self.progress.agent {
                    choice.recommended()
                } else {
                    choice
                }
            })
            .collect();
        choices.push(back());
        let chosen = self.choose(Step::Agent, blocks, choices).await?;
        if let Some(&kind) = ready.iter().find(|&&kind| account_id(kind) == chosen) {
            self.progress.agent = Some(kind);
        }
        Ok(())
    }

    async fn install(&mut self, kind: HarnessKind) -> Go<()> {
        const STEP: Step = Step::Agent;
        let name = agent_name(kind);
        let run = match kind {
            HarnessKind::Codex if !self.host.survey().npm => {
                let blocks = vec![
                    text(
                        "Codex installs with npm, which comes with Node.js, and this computer doesn't have it. Install Node.js, then check again.",
                    ),
                    link("Node.js", NODE_URL),
                    link("Other ways to install Codex", CODEX_URL),
                ];
                let choices = vec![
                    Choice::new('c', "check", "Check again").recommended(),
                    back(),
                ];
                self.choose(STEP, blocks, choices).await?;
                return Ok(());
            }
            HarnessKind::Codex => codex_installer(),
            _ => claude_installer(),
        };
        let mut blocks = vec![
            text(format!(
                "This installs {name} on this computer with its maker's own installer:"
            )),
            Block::Command(run.shown()),
            text("Its output shows here as it runs."),
        ];
        if kind == HarnessKind::Codex && !cfg!(windows) {
            blocks.push(text(
                "If npm says EACCES, it needs a folder of your own for global packages:",
            ));
            blocks.push(link("npm's guide", NPM_EACCES_URL));
        }
        let choices = vec![Choice::new('y', "run", "Run it").recommended(), back()];
        if self.choose(STEP, blocks, choices).await? != "run" {
            return Ok(());
        }
        let notice = if self.run_program(&run).await? {
            fact(Mark::Ok, name, "Installed")
        } else {
            fact(
                Mark::Warn,
                name,
                "The installer stopped before it finished; its output says why.",
            )
        };
        self.notice(STEP, vec![notice]).await
    }

    async fn sign_in(&mut self, kind: HarnessKind, remedy: Option<String>) -> Go<()> {
        const STEP: Step = Step::Agent;
        let (name, id) = (agent_name(kind), account_id(kind));
        let since = self.host.now();
        self.host.sign_in(id).await?;
        let (mut looks, mut cancelled) = (0, false);
        let stage = loop {
            let stage = self.host.sign_in_stage(id, since).await?;
            match &stage {
                Some(stage) if stage.finished() => break stage.clone(),
                None if looks >= SIGN_IN_START => {
                    break SignInStage::Failed {
                        reason: "The sign-in didn't start.".into(),
                    };
                }
                _ => {}
            }
            looks += 1;
            let doing = match &stage {
                None | Some(SignInStage::Starting) => fact(Mark::Wait, "Starting", ""),
                Some(SignInStage::Browser { url_host }) => fact(
                    Mark::Wait,
                    "In the browser",
                    format!("Sign in on {} in this computer's browser.", shown(url_host)),
                ),
                Some(SignInStage::Code { code, url_host }) => fact(
                    Mark::Info,
                    "Your code",
                    format!(
                        "Enter {} on {} in this computer's browser.",
                        shown(code),
                        shown(url_host)
                    ),
                ),
                _ => fact(Mark::Wait, "Checking who signed in", ""),
            };
            let blocks = vec![
                text(format!("Signing in to {name} with its own sign-in.")),
                doing,
            ];
            let choices = if cancelled {
                Vec::new()
            } else {
                vec![Choice::new('c', "cancel", "Cancel sign-in")]
            };
            let screen = self.screen(STEP, blocks, Ask::Choose(choices));
            if let Answer::Choice("cancel") = self.reply(&screen, Some(POLL)).await? {
                self.host.cancel_sign_in().await?;
                cancelled = true;
            }
        };
        let mut blocks = vec![match stage {
            SignInStage::Done {
                identity: Some(who),
            } => fact(Mark::Ok, name, format!("Signed in as {}", shown(&who))),
            SignInStage::Done { identity: None } => fact(Mark::Ok, name, "Signed in"),
            SignInStage::Cancelled => fact(Mark::Info, name, "Sign-in cancelled"),
            SignInStage::TimedOut => fact(Mark::Warn, name, "The sign-in timed out"),
            SignInStage::Failed { reason } => fact(Mark::Stop, name, shown(&reason)),
            _ => fact(Mark::Warn, name, "The sign-in ended"),
        }];
        if !matches!(blocks[0], Block::Fact { mark: Mark::Ok, .. })
            && let Some(remedy) = remedy
        {
            blocks.push(text(format!(
                "You can also sign in yourself: {}",
                shown(&remedy)
            )));
        }
        self.notice(STEP, blocks).await
    }

    async fn phone(&mut self) -> Go<Outcome> {
        const STEP: Step = Step::Phone;
        self.ensure_running(STEP).await?;
        let now = self.host.now();
        let paired: Vec<DeviceRecord> = self
            .host
            .devices()
            .await?
            .into_iter()
            .filter(|d| d.revoked_at.is_none())
            .collect();
        // A phone this computer reaches over Wireless debugging.
        let mut wired = None;
        if paired.is_empty() {
            match self.get_the_app().await? {
                GotApp::Wired(device) => wired = Some(device),
                GotApp::Elsewhere => {}
                GotApp::Paired => return Ok(Outcome::Done),
            }
        } else {
            let mut blocks = vec![text("Paired with this computer:")];
            blocks.extend(paired.iter().map(|d| {
                fact(
                    Mark::Ok,
                    shown(&d.name),
                    format!("Paired {}; it {}.", ago(d.paired_at, now), may(&d.grants)),
                )
            }));
            let choices = vec![
                Choice::new('n', "next", "Next").recommended(),
                Choice::new('p', "pair", "Pair another phone or tablet"),
                later(),
                skip(),
            ];
            if self.choose(STEP, blocks, choices).await? == "next" {
                return Ok(Outcome::Done);
            }
        }
        let paired = self.pair_and_grant(wired.as_ref()).await;
        if let Some(wired) = &wired
            && !matches!(paired, Err(Halt::Quit | Halt::Failed(_)))
        {
            self.debugging_off(wired).await?;
        }
        paired?;
        Ok(Outcome::Done)
    }

    async fn pair_and_grant(&mut self, wired: Option<&phone::Device>) -> Go<()> {
        if self.rerun || self.progress.alerts.is_none() {
            self.alerts().await?;
        }
        if let Some(wired) = wired
            && self.progress.alerts == Some(true)
        {
            // The app asks for whatever this couldn't allow.
            let _ = self.host.phone_alerts(wired).await;
        }
        let device = self.pair(wired).await?;
        self.grant(Step::Phone, device).await
    }

    async fn get_the_app(&mut self) -> Go<GotApp> {
        const STEP: Step = Step::Phone;
        loop {
            let blocks = vec![text(
                "Your phone needs the Øphio app, for Android phones and tablets.",
            )];
            let app = self.host.phone_app();
            let mut choices =
                vec![Choice::new('i', "how", "Show me how to install it").recommended()];
            if app.is_some() {
                choices.push(
                    Choice::new('w', "wireless", "Install it from this computer").detail(
                        "Over your phone's Wireless debugging. Setup fills in the app's pairing too.",
                    ),
                );
            }
            choices.extend(self.let_agent("Let the agent do it"));
            choices.extend([
                Choice::new('h', "have", "My phone already has Øphio"),
                later(),
                skip(),
            ]);
            match (self.choose(STEP, blocks, choices).await?, app) {
                ("have", _) => return Ok(GotApp::Elsewhere),
                ("wireless", Some(app)) => match self.install_wirelessly(&app).await? {
                    Some(device) => return Ok(GotApp::Wired(device)),
                    None => continue,
                },
                (AGENT, _) => {
                    let before: Vec<DeviceId> = self
                        .host
                        .devices()
                        .await?
                        .into_iter()
                        .map(|device| device.device_id)
                        .collect();
                    self.agent_task(STEP, "setup-phone", SET_UP).await?;
                    if self.newly_paired(&before).await?.is_some() {
                        return Ok(GotApp::Paired);
                    }
                    continue;
                }
                _ => {}
            }
            let blocks = vec![
                Block::Qr {
                    data: APP_DOWNLOAD_URL.into(),
                    caption: "Scan this with your phone's camera.".into(),
                },
                link("Or open", APP_DOWNLOAD_URL),
                Block::Steps(vec![
                    "Download the app on your phone.".into(),
                    "Open the download. If Android asks, let your browser install unknown apps: Settings › Apps › Special app access › Install unknown apps.".into(),
                    "Tap Install, then open Øphio.".into(),
                ]),
            ];
            let choices = vec![
                Choice::new('n', "next", "It's installed").recommended(),
                back(),
            ];
            if self.choose(STEP, blocks, choices).await? == "next" {
                return Ok(GotApp::Elsewhere);
            }
        }
    }

    /// Installs the app over Wireless debugging; the phone, or `None` to go
    /// back.
    async fn install_wirelessly(&mut self, app: &Path) -> Go<Option<phone::Device>> {
        const STEP: Step = Step::Phone;
        while !self.host.adb() {
            let blocks = vec![
                text(
                    "Installing from this computer needs Google's adb, from Android's SDK Platform-Tools.",
                ),
                Block::Steps(vec![
                    "Install your system's android-tools package, or download Platform-Tools from Google; you accept Google's terms there.".into(),
                    "If you downloaded them, unpack them into Android/Sdk/platform-tools in your home folder, or a folder on your PATH.".into(),
                ]),
                link("Platform-Tools", phone::PLATFORM_TOOLS_URL),
            ];
            let mut choices = vec![Choice::new('r', "again", "Check again").recommended()];
            choices.extend(self.let_agent("Let the agent install adb"));
            choices.push(back());
            match self.choose(STEP, blocks, choices).await? {
                "back" => return Ok(None),
                AGENT => {
                    let prompt = "Only get Google's adb working on this computer for me, then stop; I'll go on with setup myself.";
                    self.agent_task(STEP, "setup-phone", prompt).await?;
                }
                _ => {}
            }
        }
        let Some(device) = self.connect_phone().await? else {
            return Ok(None);
        };
        loop {
            self.busy(
                STEP,
                &format!(
                    "Installing Øphio on {}… If your phone asks to scan the app, let it.",
                    shown(&device.name())
                ),
            )
            .await;
            let problem = match self.host.phone_install(&device, app).await {
                Ok(Installed::Done) => break,
                Ok(Installed::Newer) => {
                    let blocks = vec![fact(
                        Mark::Info,
                        "Already installed",
                        format!(
                            "{} has a newer Øphio, so it was left as it is.",
                            shown(&device.name())
                        ),
                    )];
                    self.notice(STEP, blocks).await?;
                    break;
                }
                Ok(Installed::OtherKey) => phone::other_key(&device),
                Ok(Installed::Refused(reason)) => format!(
                    "{} refused the install ({reason}). If it asked, allow it; some phones also need Install via USB turned on in Developer options.",
                    device.name()
                ),
                Err(error) => error.to_string(),
            };
            let blocks = vec![fact(Mark::Stop, "Not installed", shown(&problem))];
            let choices = vec![
                Choice::new('r', "retry", "Try again").recommended(),
                back(),
                later(),
                skip(),
            ];
            if self.choose(STEP, blocks, choices).await? == "back" {
                self.debugging_off(&device).await?;
                return Ok(None);
            }
        }
        self.offer_tailscale(&device).await?;
        Ok(Some(device))
    }

    /// A phone connected over Wireless debugging; `None` to go back.
    async fn connect_phone(&mut self) -> Go<Option<phone::Device>> {
        const STEP: Step = Step::Phone;
        let ready: Vec<_> = self
            .host
            .phones()
            .await?
            .into_iter()
            .filter(phone::Device::ready)
            .collect();
        if let [device] = ready.as_slice() {
            let name = shown(&device.name());
            let blocks = vec![fact(
                Mark::Ok,
                "Connected",
                format!("{name} is connected to this computer."),
            )];
            let choices = vec![
                Choice::new('u', "use", format!("Install on {name}")).recommended(),
                Choice::new('p', "pair", "Pair another phone"),
                back(),
            ];
            match self.choose(STEP, blocks, choices).await? {
                "use" => return Ok(Some(device.clone())),
                "back" => return Ok(None),
                _ => {}
            }
        }
        let pairing = self.host.adb_pairing()?;
        let mut news: Option<Block> = None;
        loop {
            let mut blocks = vec![
                Block::Steps(vec![
                    "On your phone, open Settings › About phone and tap Build number seven times to turn on Developer options. On Samsung phones it's in About phone › Software information.".into(),
                    "Open Developer options, in Settings or under Settings › System, turn on Wireless debugging and allow it on this network.".into(),
                    "Tap Wireless debugging, then Pair device with QR code, and scan this code.".into(),
                ]),
                Block::Qr {
                    data: pairing.code(),
                    caption: "Keep your phone on the same Wi-Fi as this computer.".into(),
                },
                text(
                    "Wireless debugging lets this computer install apps and change settings on your phone. Setup uses it to install Øphio and fill in its pairing, then offers to turn it off.",
                ),
            ];
            blocks.extend(news.clone());
            blocks.push(fact(Mark::Wait, "Waiting for your phone", ""));
            let choices = vec![
                Choice::new('c', "code", "Pair with a code instead"),
                back(),
                later(),
                skip(),
            ];
            let screen = self.screen(STEP, blocks, Ask::Choose(choices));
            match self.reply(&screen, Some(POLL)).await? {
                Answer::Choice("back") => return Ok(None),
                Answer::Choice("code") => {
                    if let Some(device) = self.pair_with_code().await? {
                        return Ok(Some(device));
                    }
                }
                _ => match self.host.adb_pair_qr(&pairing).await {
                    Ok(Some(device)) => return Ok(Some(device)),
                    Ok(None) => {}
                    Err(error) => {
                        news = Some(fact(Mark::Stop, "Not paired", shown(&error.to_string())));
                    }
                },
            }
        }
    }

    /// Pairs with the code the phone shows; `None` to go back.
    async fn pair_with_code(&mut self) -> Go<Option<phone::Device>> {
        const STEP: Step = Step::Phone;
        let mut news: Option<Block> = None;
        loop {
            let mut blocks = vec![Block::Steps(vec![
                "On your phone, in Wireless debugging, tap Pair device with pairing code.".into(),
                "Type the six-digit code it shows. If setup can't find your phone, add the IP address and port it shows, after a space.".into(),
            ])];
            blocks.extend(news.take());
            let ask = Ask::Text {
                prompt: "Pairing code".into(),
            };
            let screen = self.screen(STEP, blocks, ask);
            let Answer::Text(typed) = self.reply(&screen, None).await? else {
                return Err(Halt::Quit);
            };
            if typed.trim().is_empty() {
                return Ok(None);
            }
            let (address, code): (Vec<&str>, Vec<&str>) = typed
                .split_whitespace()
                .partition(|word| word.contains(':'));
            self.busy(STEP, "Pairing with your phone…").await;
            match self
                .host
                .adb_pair_code(address.first().copied(), &code.concat())
                .await
            {
                Ok(device) => return Ok(Some(device)),
                Err(error) => {
                    news = Some(fact(Mark::Stop, "Not paired", shown(&error.to_string())));
                }
            }
        }
    }

    /// Offers to open Tailscale on the phone, when that is how it reaches
    /// this computer.
    async fn offer_tailscale(&mut self, device: &phone::Device) -> Go<()> {
        const STEP: Step = Step::Phone;
        let network = self.host.network().await?;
        if !matches!(network.tailscale, Tailscale::Running { .. }) || network.wifi_allowed() {
            return Ok(());
        }
        let blocks = vec![text(
            "Your phone reaches this computer through Tailscale, so it needs the Tailscale app, signed in to the same account as this computer.",
        )];
        let choices = vec![
            Choice::new('t', "open", "Open Tailscale on my phone")
                .recommended()
                .detail("Or its store page, if your phone doesn't have it yet."),
            Choice::new('h', "have", "It's signed in already"),
        ];
        if self.choose(STEP, blocks, choices).await? != "open" {
            return Ok(());
        }
        let said = match self.host.phone_open(device, phone::Open::Tailscale).await {
            Ok(Opened::Store) => fact(
                Mark::Info,
                "Tailscale",
                "Its store page is open on your phone. Install it, then sign in with the same account as this computer.",
            ),
            Ok(_) => fact(
                Mark::Info,
                "Tailscale",
                "It's open on your phone. Sign in with the same account as this computer, and turn it on.",
            ),
            Err(error) => fact(Mark::Warn, "Tailscale", shown(&error.to_string())),
        };
        let choices = vec![Choice::new('n', "next", "Done").recommended()];
        self.choose(STEP, vec![said], choices).await.map(drop)
    }

    /// Offers to turn Wireless debugging off, once setup is done with it.
    async fn debugging_off(&mut self, device: &phone::Device) -> Go<()> {
        const STEP: Step = Step::Phone;
        if !device.wireless() {
            return Ok(());
        }
        let blocks = vec![text(format!(
            "Setup is done with Wireless debugging on {}.",
            shown(&device.name())
        ))];
        let choices = vec![
            Choice::new('o', "off", "Turn Wireless debugging off")
                .recommended()
                .detail("Your phone keeps this computer under Wireless debugging › Paired devices until you forget it there."),
            Choice::new('k', "keep", "Leave it on"),
        ];
        if self.choose(STEP, blocks, choices).await? != "off" {
            return Ok(());
        }
        const CHECK: &str =
            "Turn it off on the phone, under Settings › Developer options › Wireless debugging.";
        let warning = match self.host.phone_off(device).await {
            Ok(Off::Done | Off::Cable) => return Ok(()),
            Ok(Off::Failed(why)) => fact(Mark::Warn, "Still on", format!("{why} {CHECK}")),
            Ok(Off::Unsure) => fact(
                Mark::Warn,
                "Maybe still on",
                format!(
                    "The phone stopped answering before it said Wireless debugging was off. {CHECK}"
                ),
            ),
            Err(error) => fact(
                Mark::Warn,
                "Still on",
                format!("{} {CHECK}", shown(&error.to_string())),
            ),
        };
        self.notice(STEP, vec![warning]).await?;
        Ok(())
    }

    async fn alerts(&mut self) -> Go<()> {
        let blocks = vec![
            text(
                "Øphio can tell your phone when an agent needs you or a task ends, even while the app is closed.",
            ),
            text(
                "Nothing stays in the status bar while it waits. For alerts after you swipe the app away, the phone asks to let Øphio run without battery limits.",
            ),
        ];
        let choices = vec![
            Choice::new('y', "on", "Turn on alerts").recommended(),
            Choice::new('n', "off", "Not now")
                .detail("Turn them on later in the app's Settings › Notifications."),
        ];
        let on = self.choose(Step::Phone, blocks, choices).await? == "on";
        self.progress.alerts = Some(on);
        Ok(())
    }

    /// Pairs a phone, offering new codes as each runs out. Each code goes
    /// to `wired`, when setup reaches the phone, as well as on screen.
    async fn pair(&mut self, wired: Option<&phone::Device>) -> Go<DeviceRecord> {
        const STEP: Step = Step::Phone;
        let previous: Vec<DeviceId> = self
            .host
            .devices()
            .await?
            .into_iter()
            .map(|d| d.device_id)
            .collect();
        let mut healed = false;
        loop {
            let mut offer = self.host.open_pairing().await?;
            if !healed && self.misses_tailnet(&offer).await? {
                // Øphio works out its addresses when it starts, which was
                // before Tailscale was up.
                healed = true;
                self.busy(
                    STEP,
                    "Restarting Øphio so the code includes its Tailscale address…",
                )
                .await;
                self.host.restart().await?;
                offer = self.host.open_pairing().await?;
            }
            let ended = self.wait_for_phone(&offer, &previous, wired).await?;
            match ended {
                Waited::Paired(device) => return Ok(device),
                Waited::Expired => {
                    let blocks = vec![fact(
                        Mark::Warn,
                        "Pairing code",
                        "This code has expired. Nothing was paired.",
                    )];
                    let choices = vec![
                        Choice::new('r', "again", "New code").recommended(),
                        later(),
                        skip(),
                    ];
                    self.choose(STEP, blocks, choices).await?;
                }
                Waited::Refused(name) => {
                    let blocks = vec![fact(
                        Mark::Info,
                        "Not paired",
                        format!(
                            "Refused {}. Nothing was paired, and that code no longer works.",
                            shown(&name)
                        ),
                    )];
                    let choices = vec![
                        Choice::new('r', "again", "New code").recommended(),
                        later(),
                        skip(),
                    ];
                    self.choose(STEP, blocks, choices).await?;
                }
            }
        }
    }

    /// Whether Tailscale is up but the offer has no address on it.
    async fn misses_tailnet(&mut self, offer: &PairingOffer) -> Go<bool> {
        let Tailscale::Running { ips, .. } = self.host.network().await?.tailscale else {
            return Ok(false);
        };
        let offered = offer.addresses.iter().filter_map(|address| {
            let host = address
                .rsplit_once(':')
                .map_or(address.as_str(), |(host, _)| host);
            host.trim_start_matches('[')
                .trim_end_matches(']')
                .parse::<IpAddr>()
                .ok()
        });
        Ok(!offered.into_iter().any(|ip| ips.contains(&ip)))
    }

    async fn wait_for_phone(
        &mut self,
        offer: &PairingOffer,
        previous: &[DeviceId],
        wired: Option<&phone::Device>,
    ) -> Go<Waited> {
        const STEP: Step = Step::Phone;
        let alerts = if self.progress.alerts == Some(true) {
            "on"
        } else {
            "off"
        };
        let full = format!("{}&alerts={alerts}", cli::pairing_link(offer)?);
        let packed = cli::pairing_qr_link(offer).map_or_else(
            || full.clone(),
            |packed| format!("{packed}&alerts={alerts}"),
        );
        let caption = time::OffsetDateTime::from_unix_timestamp_nanos(
            i128::from(offer.expires_at.0) * 1_000_000,
        )
        .map(|expires| {
            cli::pairing_expiry(expires, self.host.now(), self.host.local_offset(expires))
        })
        .unwrap_or_default();
        let local = self
            .host
            .network()
            .await
            .ok()
            .and_then(|network| network.wifi);
        let unreachable = cli::unreachable_offer(&offer.addresses, &self.host.config_file(), local);
        let mut news: Option<Block> = None;
        let mut steps = vec![
            "Open Øphio on your phone and tap Pair a computer.".to_owned(),
            "Scan this code, or paste the link below.".into(),
            "Check that the fingerprint your phone shows is the one here.".into(),
        ];
        if let Some(wired) = wired {
            match self.host.phone_link(wired, &packed).await {
                Ok(()) => {
                    steps = vec![
                        "Øphio is open on your phone with this computer's pairing link filled in.".into(),
                        "Check that the fingerprint your phone shows is the one here, then tap Pair.".into(),
                    ];
                }
                Err(error) => {
                    news = Some(fact(
                        Mark::Warn,
                        "Couldn't open Øphio on your phone",
                        shown(&error.to_string()),
                    ));
                }
            }
        }
        loop {
            if self.host.now().0 >= offer.expires_at.0 {
                return Ok(Waited::Expired);
            }
            match self.host.waiting_pairing().await? {
                Some(prompt) if !prompt.shows_code => {
                    // The computer never pairs it; refusing lets it stop waiting.
                    self.host.answer_pairing(&prompt.code, false).await?;
                    news = Some(fact(
                        Mark::Warn,
                        "Not paired",
                        cli::pairing_too_old(&prompt),
                    ));
                }
                Some(prompt) => match self.confirm(STEP, &prompt).await? {
                    Answered::Paired(device) => return Ok(Waited::Paired(device)),
                    Answered::Refused => return Ok(Waited::Refused(prompt.device_name)),
                    Answered::Gone => {
                        if let Some(device) = self.newly_paired(previous).await? {
                            return Ok(Waited::Paired(device));
                        }
                        news = Some(fact(
                            Mark::Info,
                            "Not paired",
                            cli::pairing_withdrawn(&prompt.device_name),
                        ));
                    }
                },
                None => {
                    // `ophio pair confirm` may have answered it.
                    if let Some(device) = self.newly_paired(previous).await? {
                        return Ok(Waited::Paired(device));
                    }
                }
            }
            let mut blocks = vec![
                Block::Steps(steps.clone()),
                Block::Qr {
                    data: packed.clone(),
                    caption: caption.clone(),
                },
                link("Link", full.clone()),
                fact(
                    Mark::Info,
                    "Fingerprint",
                    cli::fingerprint_words(&offer.cert_sha256),
                ),
            ];
            blocks.extend(
                unreachable
                    .as_deref()
                    .map(|note| fact(Mark::Warn, "Can't be reached", note)),
            );
            blocks.extend(news.clone());
            blocks.push(fact(Mark::Wait, "Waiting for your phone", ""));
            let screen = self.screen(STEP, blocks, Ask::Choose(vec![later(), skip()]));
            self.reply(&screen, Some(POLL)).await?;
        }
    }

    async fn newly_paired(&mut self, previous: &[DeviceId]) -> Go<Option<DeviceRecord>> {
        Ok(self
            .host
            .devices()
            .await?
            .into_iter()
            .find(|d| !previous.contains(&d.device_id) && d.revoked_at.is_none()))
    }

    /// Asks the owner about a device asking to pair, for as long as it asks.
    async fn confirm(&mut self, step: Step, prompt: &PairingPrompt) -> Go<Answered> {
        let blocks = vec![text(cli::pairing_request(prompt, true, self.host.now()))];
        // Neither is the default: the codes have to be compared first.
        let choices = vec![
            Choice::new('y', "pair", "Pair it").detail("Only if your phone shows the same code."),
            Choice::new('n', "refuse", "Don't pair"),
        ];
        self.pairings += 1;
        let screen = Screen {
            about: Some(format!("pairing {}", self.pairings)),
            ..self.screen(step, blocks, Ask::Choose(choices))
        };
        loop {
            match self.reply(&screen, Some(POLL)).await? {
                Answer::Choice("pair") => {
                    return Ok(self.host.answer_pairing(&prompt.code, true).await?);
                }
                Answer::Choice("refuse") => {
                    return Ok(match self.host.answer_pairing(&prompt.code, false).await? {
                        Answered::Gone => Answered::Gone,
                        _ => Answered::Refused,
                    });
                }
                _ => {
                    if self.host.waiting_pairing().await?.as_ref() != Some(prompt) {
                        return Ok(Answered::Gone);
                    }
                    if self.host.now().0 >= prompt.expires_at.0 {
                        return Ok(Answered::Gone);
                    }
                }
            }
        }
    }

    /// Chooses what a device just paired may do.
    async fn grant(&mut self, step: Step, device: DeviceRecord) -> Go<()> {
        let name = shown(&device.name);
        let grants = loop {
            let blocks = vec![
                fact(Mark::Ok, "Paired", name.clone()),
                text(format!(
                    "What may {name} do on this computer? Right now it {}.",
                    may(&device.grants)
                )),
            ];
            let choices = vec![
                Choice::new('f', "full", "Everything")
                    .recommended()
                    .detail("Right for your own phone, which you keep locked."),
                Choice::new('e', "everyday", "Everyday").detail(
                    "Run and answer agent tasks, watch the desktop and read the Library. Nothing that controls this computer or changes who has access.",
                ),
                Choice::new('c', "choose", "Choose each"),
                Choice::new('k', "keep", "Keep it as it is"),
            ];
            match self.choose(step, blocks, choices).await? {
                "full" => break Grants::owner(),
                "everyday" => {
                    break Grants {
                        view: true,
                        tasks: true,
                        approvals: true,
                        library_read: true,
                        ..Grants::default()
                    };
                }
                "choose" => {
                    if let Some(grants) = self.choose_grants(step, &name, &device.grants).await? {
                        break grants;
                    }
                }
                _ => return Ok(()),
            }
        };
        self.host
            .set_grants(device.device_id, grants.clone())
            .await?;
        self.notice(
            step,
            vec![
                fact(Mark::Ok, name, format!("It {}.", may(&grants))),
                text(format!(
                    "Change this whenever you like in the app, under Connections, or with `{COMMAND} devices allow` and `{COMMAND} devices deny`."
                )),
            ],
        )
        .await
    }

    async fn choose_grants(
        &mut self,
        step: Step,
        name: &str,
        current: &Grants,
    ) -> Go<Option<Grants>> {
        let g = current;
        let ticks = [
            (
                "view",
                "Watch the desktop",
                "See this computer's screen",
                g.view,
            ),
            (
                "control_input",
                "Control the desktop",
                "Use its mouse and keyboard",
                g.control_input,
            ),
            (
                "terminal",
                "Open terminals",
                "Run any command as you",
                g.terminal,
            ),
            (
                "tasks",
                "Run agent tasks",
                "Start, continue and stop them",
                g.tasks,
            ),
            (
                "approvals",
                "Answer agents",
                "Approve or refuse what they ask",
                g.approvals,
            ),
            (
                "computer_use",
                "Let an agent use the desktop",
                "An agent sees the screen and works it",
                g.computer_use,
            ),
            (
                "library_read",
                "Read the Library",
                "Browse and open files",
                g.library_read,
            ),
            (
                "library_write",
                "Edit the Library",
                "Change, move and delete files",
                g.library_write,
            ),
            (
                "transfers",
                "Transfer files",
                "Copy files to and from the phone",
                g.transfers,
            ),
            (
                "admin",
                "Manage access",
                "Pair, remove and change devices",
                g.admin,
            ),
            (
                "accounts",
                "Manage accounts",
                "Sign in and out of the accounts tasks use",
                g.accounts,
            ),
        ]
        .into_iter()
        .map(|(id, label, detail, on)| Tick {
            id,
            label: label.into(),
            detail: Some(detail.into()),
            on,
        })
        .collect();
        let blocks = vec![text(format!("Tick what {name} may do."))];
        let screen = self.screen(
            step,
            blocks,
            Ask::Ticks {
                ticks,
                choices: vec![back()],
            },
        );
        let Answer::Ticks(on) = self.reply(&screen, None).await? else {
            return Ok(None);
        };
        let on = |id| on.contains(&id);
        Ok(Some(Grants {
            view: on("view"),
            control_input: on("control_input"),
            terminal: on("terminal"),
            tasks: on("tasks"),
            approvals: on("approvals"),
            computer_use: on("computer_use"),
            library_read: on("library_read"),
            library_write: on("library_write"),
            transfers: on("transfers"),
            admin: on("admin"),
            accounts: on("accounts"),
        }))
    }

    async fn extras(&mut self) -> Go<Outcome> {
        const STEP: Step = Step::Options;
        let ticks = PICKS
            .into_iter()
            .map(|pick| Tick {
                id: pick.id(),
                label: pick.label().into(),
                detail: Some(pick.detail().into()),
                on: self.progress.left.contains(&pick)
                    || (pick == Pick::Files && !self.progress.files),
            })
            .collect();
        let blocks = vec![text(format!(
            "Tick any you want to set up now. Each can be set up later, in the app or with `{COMMAND} setup`."
        ))];
        let screen = self.screen(
            STEP,
            blocks,
            Ask::Ticks {
                ticks,
                choices: vec![later(), skip()],
            },
        );
        let Answer::Ticks(on) = self.reply(&screen, None).await? else {
            return Err(Halt::Quit);
        };
        self.progress.files = true;
        for pick in PICKS.into_iter().filter(|pick| on.contains(&pick.id())) {
            loop {
                match self.extra(pick).await {
                    Ok(()) => self.progress.left.retain(|left| *left != pick),
                    Err(Halt::Later) => self.leave(pick),
                    Err(Halt::Failed(error)) => match self.trouble(STEP, &error).await {
                        Ok(()) => continue,
                        Err(Halt::Later) => self.leave(pick),
                        Err(halt) => return Err(halt),
                    },
                    Err(halt) => return Err(halt),
                }
                break;
            }
            self.save()?;
        }
        Ok(Outcome::Done)
    }

    fn leave(&mut self, pick: Pick) {
        if !self.progress.left.contains(&pick) {
            self.progress.left.push(pick);
        }
    }

    async fn extra(&mut self, pick: Pick) -> Go<()> {
        match pick {
            Pick::Projects => self.projects().await,
            Pick::Files => self.files().await,
            Pick::Desktop => {
                let mut blocks = vec![
                    text(
                        "Your phone can watch this computer's screen and, with your say-so, control it. Each phone also needs permission to watch or to control.",
                    ),
                    Block::Command(Run::ophio(&["desktop", "enable"]).shown()),
                ];
                if cfg!(target_os = "linux") {
                    blocks.push(text("The first time a phone looks, your desktop may ask here whether to share the screen."));
                }
                self.command(
                    pick,
                    blocks,
                    "Turn it on",
                    &[Run::ophio(&["desktop", "enable"])],
                )
                .await
            }
            Pick::Voice => {
                let blocks = vec![
                    text(
                        "Talk instead of typing. What you say is recognised on this computer, by a 151 MB speech model that is downloaded once.",
                    ),
                    Block::Command(Run::ophio(&["voice", "install", "tiny.en"]).shown()),
                ];
                self.command(
                    pick,
                    blocks,
                    "Download it",
                    &[Run::ophio(&["voice", "install", "tiny.en"])],
                )
                .await
            }
            Pick::Telegram => {
                let blocks = vec![
                    text("Message your agents through a Telegram bot of your own."),
                    Block::Steps(vec![
                        "In Telegram, message @BotFather and send /newbot.".into(),
                        "Choose a name, then copy the token it gives you.".into(),
                        "Paste it when asked below; it isn't shown as you type.".into(),
                        "Message your bot. It answers with a one-time link code: enter it in the app, under Connections.".into(),
                    ]),
                ];
                self.command(pick, blocks, "Enter the token", &channel("telegram"))
                    .await
            }
            Pick::Discord => {
                let blocks = vec![
                    text("Message your agents through a Discord bot of your own."),
                    Block::Steps(vec![
                        "In Discord's Developer Portal, make a new application and add a bot to it.".into(),
                        "Copy the bot's token. It needs no privileged intents.".into(),
                        "Invite it to your server with the bot and applications.commands scopes, and the View Channels and Send Messages permissions.".into(),
                        "Paste the token when asked below; it isn't shown as you type.".into(),
                        "Message your bot. It answers with a one-time link code: enter it in the app, under Connections.".into(),
                    ]),
                    link("Developer Portal", "https://discord.com/developers/applications"),
                ];
                self.command(pick, blocks, "Enter the token", &channel("discord"))
                    .await
            }
            Pick::Accounts => {
                let blocks = vec![
                    text(
                        "Tasks can use your other accounts, such as GitHub or Google. Their keys and sign-ins stay on this computer; your phone sees only their names and states.",
                    ),
                    text(
                        "Add them in the app, under Settings › Accounts. Signing in happens in this computer's browser.",
                    ),
                ];
                let mut choices = vec![carry_on()];
                choices.extend(self.let_agent("Let the agent help"));
                choices.push(later());
                match self.choose(Step::Options, blocks, choices).await? {
                    AGENT => self.by_agent(pick).await,
                    _ => Ok(()),
                }
            }
            Pick::Github => self.github_workflow().await,
            Pick::AgentLogins => {
                let blocks = vec![
                    text(
                        "Agents can sign in to websites with usernames and passwords you add, in Øphio's own browser on this computer. Øphio types a password into the page itself, so the agent never sees it.",
                    ),
                    text(
                        "It stays off until you accept its notice. The command below shows the notice and turns agent logins on when you type yes. Add the logins afterwards, in the app under Settings › Agent logins.",
                    ),
                    Block::Command(Run::ophio(&["agent-logins", "enable"]).shown()),
                ];
                self.command(
                    pick,
                    blocks,
                    "Read the notice",
                    &[Run::ophio(&["agent-logins", "enable"])],
                )
                .await
            }
            Pick::VoiceControl => {
                let blocks = vec![
                    text(
                        "Say what to do in a browser or on the desktop, and Øphio does it. TypeSafe's Jev works out what you meant: it is sent what you said and the names of the controls on screen, nothing else.",
                    ),
                    text(
                        "It needs a TypeSafe key of your own. Charges go to your own Jev access, separately from your Claude and Codex plans.",
                    ),
                    link("Pricing", "https://docs.typesafe.ai/models"),
                    Block::Command(Run::ophio(&["jev", "connect"]).shown()),
                ];
                self.command(pick, blocks, "Link it", &[Run::ophio(&["jev", "connect"])])
                    .await
            }
        }
    }

    /// Chooses where a checklist's files go, as Settings › Where files go
    /// in the app does.
    async fn files(&mut self) -> Go<()> {
        const STEP: Step = Step::Options;
        const KEYS: [(char, &str); 9] = [
            ('1', "1"),
            ('2', "2"),
            ('3', "3"),
            ('4', "4"),
            ('5', "5"),
            ('6', "6"),
            ('7', "7"),
            ('8', "8"),
            ('9', "9"),
        ];
        self.ensure_running(STEP).await?;
        self.busy(STEP, "Looking for where files can go…").await;
        let Some(view) = self.host.file_delivery().await? else {
            let why = "This Øphio doesn't keep checklists yet. Update it, then run setup again.";
            return self
                .notice(STEP, vec![fact(Mark::Warn, Pick::Files.label(), why)])
                .await;
        };
        let now = &view.settings.delivery;
        let mut offered = vec![(
            FileDelivery::Phone,
            "To the phone that asks".to_owned(),
            "Saved where the phone keeps downloads".to_owned(),
        )];
        for device in view.taildrop.iter().flatten() {
            let to = FileDelivery::Taildrop {
                device: device.clone(),
            };
            offered.push((to, shown(device), "By Taildrop".into()));
        }
        for folder in &view.folders {
            let to = FileDelivery::Folder {
                path: folder.path.clone(),
            };
            offered.push((to, shown(&folder.label), shown(&folder.path)));
        }
        for remote in view.drives.iter().flatten() {
            let folder = match now {
                FileDelivery::Drive { remote: r, folder } if r == remote => folder.clone(),
                _ => "Ophio".into(),
            };
            let detail = format!("By rclone, into its {} folder", shown(&folder));
            offered.push((
                FileDelivery::Drive {
                    remote: remote.clone(),
                    folder,
                },
                shown(remote),
                detail,
            ));
        }
        offered.truncate(KEYS.len());
        let mut blocks = vec![
            text(
                "When a checklist has a file for you, such as a settings file to put on a server, where should it go? Each project's files go in a folder of their own.",
            ),
            text("Change it any time in the app, under Settings › Where files go."),
        ];
        if view.taildrop.is_none() {
            blocks.push(fact(
                Mark::Info,
                "Taildrop",
                "Needs Tailscale on this computer, signed in",
            ));
        }
        if view.drives.is_none() {
            blocks.push(fact(Mark::Info, "Drives", "Google Drive, Dropbox, OneDrive and others need rclone set up here, or their app's synced folder"));
        }
        let mut choices: Vec<Choice> = (offered.iter().zip(KEYS))
            .map(|((to, label, detail), (key, id))| {
                let choice = Choice::new(key, id, label).detail(detail);
                if to == now {
                    choice.recommended()
                } else {
                    choice
                }
            })
            .collect();
        let elsewhere =
            matches!(now, FileDelivery::Folder { .. }) && !offered.iter().any(|(to, ..)| to == now);
        let mut other = Choice::new('o', "other", "Another folder on this computer");
        if let (true, FileDelivery::Folder { path }) = (elsewhere, now) {
            other = other.detail(format!("Now {}", shown(path))).recommended();
        }
        choices.extend([other, later()]);
        let delivery = match self.choose(STEP, blocks, choices).await? {
            "other" => {
                let screen = self.screen(
                    STEP,
                    vec![text("Which folder? Give its full path, or one from your home folder such as ~/Documents/Checklists.")],
                    Ask::Text { prompt: "Folder (Enter alone to go back)".into() },
                );
                let Answer::Text(path) = self.reply(&screen, None).await? else {
                    return Ok(());
                };
                if path.trim().is_empty() {
                    return Ok(());
                }
                FileDelivery::Folder {
                    path: path.trim().into(),
                }
            }
            id => match KEYS.iter().position(|(_, key)| *key == id) {
                Some(at) if at < offered.len() => offered.swap_remove(at).0,
                _ => return Ok(()),
            },
        };
        self.host
            .set_file_delivery(delivery, view.settings.revision)
            .await?;
        Ok(())
    }

    /// Chooses each Git project's GitHub workflow, as Tasks › GitHub in
    /// the app does.
    async fn github_workflow(&mut self) -> Go<()> {
        const STEP: Step = Step::Options;
        self.ensure_running(STEP).await?;
        let (projects, login) = loop {
            self.busy(STEP, "Reading your projects on GitHub…").await;
            let projects = self.host.github().await?;
            let login = projects.iter().find_map(|(_, view)| view.login.clone());
            if let Some(login) = login {
                break (projects, login);
            }
            if projects.is_empty() {
                let blocks = vec![
                    fact(Mark::Info, Pick::Github.label(), "No Git projects"),
                    text(
                        "Add a project whose folder is a Git repository with its origin on GitHub, then come back to this.",
                    ),
                ];
                self.notice(STEP, blocks).await?;
                return Err(Halt::Later);
            }
            let problem = projects
                .iter()
                .find_map(|(_, view)| view.problem.as_deref());
            let blocks = vec![
                fact(Mark::Warn, "GitHub", shown(problem.unwrap_or_default())),
                text(
                    "The agent works as the GitHub account signed in under Accounts. Sign in to GitHub in the app, under Settings › Accounts, then check again.",
                ),
            ];
            let choices = vec![
                Choice::new('c', "check", "Check again").recommended(),
                later(),
            ];
            self.choose(STEP, blocks, choices).await?;
        };
        let mut blocks = vec![text(
            "Change any of these in the app, under Tasks › GitHub.",
        )];
        let mut on_github = false;
        for (name, view) in &projects {
            let name = shown(name);
            match &view.repo {
                Some(repo) => {
                    on_github = true;
                    let mode = self.github_mode(&name, view, repo, &login).await?;
                    blocks.push(fact(Mark::Ok, name, mode_words(mode)));
                }
                None => blocks.push(fact(
                    Mark::Info,
                    name,
                    shown(view.problem.as_deref().unwrap_or_default()),
                )),
            }
        }
        if !on_github {
            blocks[0] = text("Øphio found none of your Git projects on GitHub.");
        }
        self.notice(STEP, blocks).await?;
        if on_github { Ok(()) } else { Err(Halt::Later) }
    }

    /// One project's GitHub workflow, as it is once chosen.
    async fn github_mode(
        &mut self,
        name: &str,
        view: &GithubProjectView,
        repo: &GithubRepo,
        login: &str,
    ) -> Go<GithubMode> {
        const STEP: Step = Step::Options;
        let settings = &view.settings;
        let now = settings.mode;
        let branch = shown(&repo.default_branch);
        let login = shown(login);
        let mut blocks = vec![
            fact(
                Mark::Info,
                shown(&format!("{}/{}", repo.owner, repo.name)),
                format!("{name}, working as @{login}"),
            ),
            fact(Mark::Info, "GitHub workflow now", mode_words(now)),
            match repo.protected {
                Some(true) => fact(Mark::Ok, format!("{branch} is protected"), ""),
                Some(false) => fact(
                    Mark::Warn,
                    format!("{branch} isn't protected"),
                    "nothing stops a push that skips a pull request. Protect it in the app, under Tasks › GitHub.",
                ),
                None => fact(
                    Mark::Info,
                    format!("Whether {branch} is protected"),
                    "couldn't be read",
                ),
            },
        ];
        if !repo.owned {
            blocks.push(text(
                "It isn't your repository, so the agent asks you before posting there.",
            ));
        }
        if !repo.admin {
            blocks.push(text(format!(
                "Through merge is offered where @{login} administers the repository."
            )));
        }
        let mut choices = Vec::new();
        for (key, mode) in [
            ('o', GithubMode::Off),
            ('m', GithubMode::ThroughMerge),
            ('d', GithubMode::Drafts),
        ] {
            if mode == GithubMode::ThroughMerge && !repo.admin && now != mode {
                continue;
            }
            let choice = Choice::new(key, mode_id(mode), mode_words(mode)).detail(mode_hint(mode));
            choices.push(if mode == now {
                choice.recommended()
            } else {
                choice
            });
        }
        choices.push(later());
        let id = self.choose(STEP, blocks, choices).await?;
        let mode = [
            GithubMode::Off,
            GithubMode::ThroughMerge,
            GithubMode::Drafts,
        ]
        .into_iter()
        .find(|mode| mode_id(*mode) == id)
        .unwrap_or(now);
        if mode != now {
            let settings = GithubSettings {
                mode,
                ..settings.clone()
            };
            self.host
                .github_action(GithubAction::SetSettings {
                    project_id: view.project_id,
                    settings,
                })
                .await?;
        }
        // Asked when Through merge is newly chosen, as the companion asks it,
        // and again while it is unanswered.
        if mode != GithubMode::ThroughMerge || (mode == now && settings.warning_answered) {
            return Ok(mode);
        }
        let named = format!("{}/{}", repo.owner, repo.name);
        let mut blocks = vec![
            fact(Mark::Warn, "Before it merges by itself", ""),
            text(format!(
                "The agent will merge its own pull requests into {branch} on {} when every check passes and no review comment is open, without asking you first. A merge can be undone with a revert, but anything that deploys from {branch} deploys it.",
                shown(&named)
            )),
        ];
        blocks.extend(view.warnings.iter().map(|w| fact(Mark::Warn, shown(w), "")));
        blocks.push(text("Nothing merges until you choose."));
        let choices = vec![
            Choice::new('k', "keep", "Keep Through merge"),
            Choice::new('d', "use_drafts", "Use Drafts"),
            later(),
        ];
        let keep = self.choose(STEP, blocks, choices).await? == "keep";
        self.host
            .github_action(GithubAction::AnswerWarning {
                project_id: view.project_id,
                repo: named,
                keep_through_merge: keep,
            })
            .await?;
        Ok(if keep {
            GithubMode::ThroughMerge
        } else {
            GithubMode::Drafts
        })
    }

    /// Shows an extra, and runs its commands if the person agrees.
    async fn command(
        &mut self,
        pick: Pick,
        blocks: Vec<Block>,
        action: &str,
        runs: &[Run],
    ) -> Go<()> {
        const STEP: Step = Step::Options;
        let mut blocks = blocks;
        blocks.insert(0, fact(Mark::Info, pick.label(), ""));
        let mut choices = vec![Choice::new('y', "run", action).recommended()];
        if pick.skill().is_some() {
            choices.extend(self.let_agent("Let the agent do it"));
        }
        choices.push(later());
        if self.choose(STEP, blocks, choices).await? == AGENT {
            return self.by_agent(pick).await;
        }
        if matches!(
            pick,
            Pick::Telegram | Pick::Discord | Pick::AgentLogins | Pick::VoiceControl
        ) {
            self.ensure_running(STEP).await?;
        }
        for run in runs {
            if !self.run_program(run).await? {
                let blocks = vec![fact(
                    Mark::Warn,
                    pick.label(),
                    "That didn't finish; its output says why.",
                )];
                let choices = vec![
                    Choice::new('r', "retry", "Try again").recommended(),
                    later(),
                    Choice::new('n', "next", "Go on"),
                ];
                return match self.choose(STEP, blocks, choices).await? {
                    "retry" => Box::pin(self.extra(pick)).await,
                    _ => Err(Halt::Later),
                };
            }
        }
        self.notice(STEP, vec![fact(Mark::Ok, pick.label(), "Done")])
            .await
    }

    /// Has the agent set an extra up: done once its task has done its turn.
    async fn by_agent(&mut self, pick: Pick) -> Go<()> {
        let Some(skill) = pick.skill() else {
            return Box::pin(self.extra(pick)).await;
        };
        match self.agent_task(Step::Options, skill, SET_UP).await? {
            Some(TaskState::Completed) => Ok(()),
            Some(_) => Err(Halt::Later),
            // Back to how it's done by hand.
            None => Box::pin(self.extra(pick)).await,
        }
    }

    async fn projects(&mut self) -> Go<()> {
        const STEP: Step = Step::Options;
        self.ensure_running(STEP).await?;
        let agent = self.progress.agent;
        if let Some(choice) = self.let_agent("Let the agent find them") {
            let blocks = vec![text(
                "Add the folders your agents work in. Each becomes a project in the app.",
            )];
            let choices = vec![
                Choice::new('t', "type", "Type the folders").recommended(),
                choice.detail("It looks for folders kept in Git, and asks you which to add."),
                later(),
            ];
            if self.choose(STEP, blocks, choices).await? == AGENT {
                return self.by_agent(Pick::Projects).await;
            }
        }
        let mut added = Vec::new();
        loop {
            let mut blocks = vec![text(
                "Add the folders your agents work in, one at a time. Each becomes a project in the app.",
            )];
            if let Some(agent) = agent {
                blocks.push(fact(Mark::Info, "They start with", agent_name(agent)));
            }
            blocks.extend(added.iter().cloned());
            let screen = self.screen(
                STEP,
                blocks,
                Ask::Text {
                    prompt: "Folder (Enter alone when done)".into(),
                },
            );
            let Answer::Text(folder) = self.reply(&screen, None).await? else {
                return Ok(());
            };
            if folder.is_empty() {
                return Ok(());
            }
            match self.host.add_project(&folder, agent).await {
                Ok(path) => added.push(fact(Mark::Ok, "Added", shown(&path))),
                Err(error @ (Error::Protocol(_) | Error::Ended(_))) => {
                    added.push(fact(Mark::Warn, shown(&folder), shown(&error.to_string())));
                }
                Err(error) => return Err(error.into()),
            }
        }
    }

    async fn readiness(&mut self) -> Go<Outcome> {
        const STEP: Step = Step::Readiness;
        self.ensure_running(STEP).await?;
        loop {
            self.busy(STEP, "Checking…").await;
            let report = self.host.readiness().await?;
            let mut blocks = Vec::new();
            let mut passed = 0;
            for check in &report.checks {
                let said = |text: &Option<String>| text.as_deref().map(shown);
                let detail = said(&check.summary)
                    .or_else(|| said(&check.detail))
                    .unwrap_or_default();
                let mark = match check.status {
                    CheckStatus::Passed => {
                        passed += 1;
                        continue;
                    }
                    CheckStatus::Failed => Mark::Stop,
                    CheckStatus::NotRun | CheckStatus::Unknown => Mark::Info,
                };
                let detail = match said(&check.remedy) {
                    Some(remedy) if detail.is_empty() => remedy,
                    Some(remedy) => format!("{detail} {remedy}"),
                    None => detail,
                };
                blocks.push(fact(mark, shown(&check.label), detail));
            }
            let failed = report
                .checks
                .iter()
                .any(|c| c.status == CheckStatus::Failed);
            if passed > 0 {
                let checks = if passed == 1 { "check" } else { "checks" };
                blocks.insert(0, fact(Mark::Ok, format!("{passed} {checks} passed"), ""));
            }
            if !failed {
                blocks.insert(0, text("Everything checked works."));
            }
            let mut choices = vec![
                Choice::new('n', "next", "Next").recommended(),
                Choice::new('r', "again", "Check again"),
            ];
            if failed {
                choices.extend(self.let_agent("Ask the agent to fix this"));
            }
            choices.extend([later(), skip()]);
            match self.choose(STEP, blocks, choices).await? {
                "next" => return Ok(Outcome::Done),
                AGENT => {
                    let found: Vec<&str> = report
                        .checks
                        .iter()
                        .filter(|c| c.status == CheckStatus::Failed)
                        .map(|c| c.label.as_str())
                        .collect();
                    let prompt = format!(
                        "{PRODUCT_NAME}'s readiness check says these need attention: {}. Fix what you can, please.",
                        found.join("; ")
                    );
                    self.agent_task(STEP, "setup-fix", &prompt).await?;
                }
                _ => {}
            }
        }
    }

    async fn done(&mut self) -> Go<()> {
        self.progress.finished = Some(Finished::Done);
        self.save()?;
        let mut blocks: Vec<Block> = Step::NUMBERED
            .iter()
            .map(|&step| match self.progress.steps.get(&step) {
                Some(Outcome::Done) => fact(Mark::Ok, title(step), "Done"),
                _ => fact(Mark::Info, title(step), "Left for later"),
            })
            .collect();
        if !self.progress.left.is_empty() {
            let left: Vec<&str> = self.progress.left.iter().map(|p| p.label()).collect();
            blocks.push(fact(Mark::Info, "Extras left for later", left.join(", ")));
        }
        blocks.push(text(format!(
            "Change any of this in the app's Settings, or run `{COMMAND} setup` again."
        )));
        let choices = vec![Choice::new('n', "finish", "Finish").recommended()];
        self.choose(Step::Done, blocks, choices).await.map(drop)
    }
}

/// How the phone came to have the app.
enum GotApp {
    /// Installed from here, on a phone still reached over Wireless debugging.
    Wired(phone::Device),
    /// Some other way; it still needs pairing.
    Elsewhere,
    /// The agent saw to it, pairing too.
    Paired,
}

enum Waited {
    Paired(DeviceRecord),
    Expired,
    Refused(String),
}

/// Whether a task is still under way, or waits on the person.
fn going(state: TaskState) -> bool {
    matches!(
        state,
        TaskState::Accepted
            | TaskState::Dispatching
            | TaskState::Working
            | TaskState::AwaitingDecision
    )
}

/// How a setup task stands.
fn standing(view: &TaskView) -> Block {
    let status = || view.status.as_deref().map(shown).unwrap_or_default();
    match view.state {
        TaskState::Accepted | TaskState::Dispatching => fact(Mark::Wait, "Starting", ""),
        TaskState::Working => fact(Mark::Wait, "Working", status()),
        TaskState::AwaitingDecision => fact(Mark::Warn, "Waiting for you", ""),
        TaskState::Completed => fact(
            Mark::Ok,
            "Its turn is done",
            "If it asked you something, reply.",
        ),
        TaskState::Interrupted => fact(Mark::Info, "Stopped", "Reply to have it carry on."),
        TaskState::Failed => fact(Mark::Stop, "It failed", status()),
        TaskState::ReconciliationRequired => {
            fact(Mark::Warn, "Interrupted", "See how it stands in the app.")
        }
        TaskState::Archived => fact(Mark::Info, "Archived", ""),
    }
}

/// Text that may run over several lines, each made safe to show.
fn lines(text: &str) -> String {
    text.lines().map(shown).collect::<Vec<_>>().join("\n")
}

/// The most of a whole request shown here; the app shows all of it.
const WHOLE: usize = 2000;

/// What a task asks to do, as it would do it.
fn detail(asked: &Asked) -> Vec<Block> {
    let mut blocks: Vec<Block> = asked
        .detail
        .iter()
        .map(|(code, line)| {
            if *code {
                Block::Command(line.clone())
            } else {
                text(shown(line))
            }
        })
        .collect();
    if let Some(whole) = &asked.whole {
        blocks.push(fact(Mark::Info, "Shortened", "The whole request:"));
        let cut: String = whole.chars().take(WHOLE).collect();
        blocks.extend(cut.lines().map(|line| Block::Command(line.into())));
        if cut.len() < whole.len() {
            blocks.push(text(
                "The rest is in the app, under the request's Exact request.",
            ));
        }
    }
    blocks
}

/// The answer that picks the `picked` options of an agent's question.
fn answer(question: &ApprovalQuestion, picked: &[usize]) -> ApprovalDecision {
    let labels: Vec<String> = picked
        .iter()
        .filter_map(|&n| question.options.get(n))
        .map(|option| option.label.clone())
        .collect();
    ApprovalDecision::Answer {
        text: labels.join(", "),
        answers: vec![QuestionAnswer {
            id: question.id.clone(),
            question: question.question.clone(),
            choices: labels,
        }],
    }
}

fn ready(agent: &HarnessInfo) -> bool {
    agent.installed == Availability::Available
        && matches!(agent.account, AccountReadiness::SignedIn { .. })
}
fn install_id(kind: HarnessKind) -> &'static str {
    match kind {
        HarnessKind::Codex => "install-codex",
        _ => "install-claude",
    }
}
fn sign_in_id(kind: HarnessKind) -> &'static str {
    match kind {
        HarnessKind::Codex => "sign-in-codex",
        _ => "sign-in-claude",
    }
}
fn channel(connector: &str) -> [Run; 2] {
    [
        Run::ophio(&["channels", "token", "set", connector]),
        Run::ophio(&["channels", "enable", connector]),
    ]
}

/// Claude Code's own installer.
fn claude_installer() -> Run {
    if cfg!(windows) {
        let script = "irm https://claude.ai/install.ps1 | iex";
        Run::Program {
            program: "powershell".into(),
            args: vec!["-NoProfile".into(), "-Command".into(), script.into()],
            shown: script.into(),
        }
    } else {
        let script = "curl -fsSL https://claude.ai/install.sh | bash";
        Run::Program {
            program: "bash".into(),
            args: vec!["-c".into(), script.into()],
            shown: script.into(),
        }
    }
}
fn codex_installer() -> Run {
    let script = "npm install -g @openai/codex";
    if cfg!(windows) {
        // npm is a script there, which only a shell runs.
        Run::Program {
            program: "powershell".into(),
            args: vec!["-NoProfile".into(), "-Command".into(), script.into()],
            shown: script.into(),
        }
    } else {
        Run::Program {
            program: "npm".into(),
            args: vec!["install".into(), "-g".into(), "@openai/codex".into()],
            shown: script.into(),
        }
    }
}
