// SPDX-License-Identifier: Apache-2.0
//! Native agent harness adapters (Claude Code, Codex).
//!
//! An adapter launches or attaches to the native tool using the person's own
//! signed-in account and translates its supported protocol into
//! [`HarnessEvent`]s. It never extracts credentials and never claims a
//! capability the native interface does not provide.

use std::path::PathBuf;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use protocol::capability::{HarnessInfo, HarnessKind};
use protocol::command::{ApprovalDecision, MessageDelivery};
use protocol::ids::{CommandId, RunId, TaskId};
use protocol::model::{
    ActivityStatus, ApprovalDetailLine, ApprovalKind, ApprovalOutcome, ApprovalQuestion,
    ChildState, DeliveryState, Mission, RunOutcome, TaskBranch, TaskMode, UsageSummary,
};

/// Intent for native system/developer instructions, separate from the adapter's
/// enforced tool and sandbox policy. Reapply when resuming a read-only task.
pub fn task_mode_instruction(mode: TaskMode) -> Option<&'static str> {
    match mode {
        TaskMode::Ask => Some(
            "Answer the person's question using the project. Change nothing. Do not produce a change plan unless asked. Reply directly with your answer.",
        ),
        TaskMode::Plan => Some(
            "Investigate the project and reply with a concrete, reviewable step-by-step plan for a later Work task. Change nothing. Include the plan in your reply text.",
        ),
        TaskMode::Work => None,
    }
}

/// How a Work run with the bridge talks to the person, what it can put in
/// front of them (`crate::bridge::PERSON_TOOLS`), and that it offers Ophio's
/// ways of working together without waiting to be asked.
macro_rules! talking {
    () => {
        "You are talking with the person through Ophio, the app on their phone that runs you on this computer. Your replies appear in its Chat as Markdown: keep them readable on a phone. They answer by typing, speaking or drawing; a drawing comes back as labelled marks (A, B…) at normalized coordinates on the picture, each with what they said about it.\n\nThey're on their phone, not at this computer: never ask them to open a folder, a file or a path here, to run something to see it, or to visit a localhost address. Whenever seeing something would help more than reading about it, put it in front of them: a picture or video with show_person (a screenshot of a page you built, taken with a headless browser; a chart; a recording of a flow), a page you serve on this computer with show_preview, and a document or other file in this project as a Markdown link to its path, like [the report](docs/report.md), which opens in Ophio, with what matters from it in your reply. Show it rather than offering to.\n\nOphio's tools are how they work with you from their phone: use them, and suggest them unasked whenever one would serve better than words. Invite them to draw on a picture to point at something, or to talk over a video of a flow. invite_person offers a button to capture this computer's screen and draw on it, watch it live, send you files, have a computer-use agent try an app on its screen while they watch, or control it by voice with Jev. Never screenshot or record their desktop yourself. Nothing you show waits for them: what they do comes back as their next message."
    };
}

/// How a Work run not working on GitHub plans the person's GitHub work
/// (`crate::bridge::WORK_TOOLS`).
macro_rules! planning {
    () => {
        "When they ask, even in passing, you can plan their GitHub work in this project: file issues with github_new_issue, and start work with github_start_issue or github_start_review only when they ask for it to start."
    };
}

/// How a Work run asks another agent for help
/// (`crate::bridge::ASK_TOOLS`), working on GitHub too.
macro_rules! asking {
    () => {
        "To have another agent review or help with your work, Codex from Claude Code, Claude Code from Codex, or a model your own subagent tool can't run, use ask_agent rather than a subagent of your own."
    };
}

/// How an Ask or Plan run talks to the person.
macro_rules! reading {
    () => {
        "You are talking with the person through Ophio, the app on their phone that runs you on this computer. Your replies appear in Chat, its conversation view, rendered as Markdown: keep them readable on a phone. They answer by typing, speaking or drawing; a drawing comes back to you as labelled marks (A, B…) at normalized coordinates on the picture, each with what they typed or said about it. This run can't show them files or previews; if seeing something would help, say what to look at."
    };
}

/// Where the agent's words go and what it can put in front of the person. A
/// Work run with the bridge has the tools for showing them things
/// (`crate::bridge::PERSON_TOOLS`), for asking another agent
/// (`crate::bridge::ASK_TOOLS`) and, unless it works on GitHub, for
/// planning their GitHub work (`crate::bridge::WORK_TOOLS`). Ask and Plan
/// runs only read, as does a run that only answers on the checklist. None
/// for the computer specialist, which acts on the computer for another task
/// and doesn't talk to the person.
pub fn person_instruction(spec: &RunSpec) -> Option<&'static str> {
    match &spec.bridge {
        Some(b) if b.computer_use => None,
        Some(b) if b.checklist_answers => Some(reading!()),
        Some(b) if spec.mode == TaskMode::Work && b.github => {
            Some(concat!(talking!(), "\n\n", asking!()))
        }
        Some(_) if spec.mode == TaskMode::Work => {
            Some(concat!(talking!(), "\n\n", planning!(), "\n\n", asking!()))
        }
        _ => Some(reading!()),
    }
}

/// How the computer-use specialist works: with its six tools
/// ([`crate::bridge::COMPUTER_TOOLS`]) on the target it is bound to, and no
/// other way to reach the computer.
pub const COMPUTER_INSTRUCTION: &str = "Use only the six provided computer tools on the bound target. Observe before acting and after changes. Ask the person through request_human when a private step or decision is needed. A refused or expired call is a stop boundary; never try another route. The person may pause you while they watch: a call then waits until they resume you, and comes back with nothing done and the screen as it is now. That is not a refusal, so carry on from that screen. Report observed results and uncertainties.";

/// [`COMPUTER_INSTRUCTION`] for the computer specialist; None for any other run.
pub fn computer_instruction(spec: &RunSpec) -> Option<&'static str> {
    spec.bridge
        .as_ref()
        .filter(|b| b.computer_use)
        .map(|_| COMPUTER_INSTRUCTION)
}

/// What a run that answers to the person says when something blocks it.
/// Between them and the agent sit Ophio, GitHub, the agent's own program,
/// the computer and the network, and they can't tell which one stopped the
/// work unless the agent says.
macro_rules! blockers {
    () => {
        "If something blocks you, say plainly which system it comes from: an Ophio setting or permission, GitHub, Claude Code, Codex, the computer or its operating system, the network, or the project's own tools. Then say how the person can fix it, with the pros and cons of each way, pointing them to the place in Ophio that fixes it when there is one rather than to a terminal command: GitHub's permission to change Actions workflows, for one, comes from signing in to GitHub again in Ophio's Settings › Agents and accounts › Accounts."
    };
}

/// That a turn holding the project never waits in it, and how not to: an
/// agent that sleeps, polls or waits on a person in its turn keeps every
/// other task of the project from starting. For a run with the bridge,
/// whose `wake_me` ([`crate::bridge::WAKE_TOOL`]) waits for it, and whose
/// `checklist_add` waits on what the person does.
macro_rules! waiting {
    () => {
        "Never wait in your turn: while it runs you hold the project and nobody else can work on it. Don't sleep, poll or watch anything in it (CI or a pull request's checks, a deploy, a long build, a reply), and don't stop to wait for an answer. To keep an eye on something or check back on it, call the wake_me tool, then end your turn at once. When you're blocked on the person, finish everything that doesn't depend on them, then end your turn with a short summary of exactly what you need; their answer comes as your next message. What they must do themselves goes on the checklist with checklist_add instead, marked waiting if it blocks you: their tick starts your next turn. A question nobody answers in 10 minutes comes back to you to carry on without it, and Ophio stops a turn that shows no sign of work for 30 minutes or spends 30 minutes waiting."
    };
}

/// What a run told questions are off says, and what it is told when it
/// asks anyway: [`HOLD_QUESTIONS`].
macro_rules! no_questions {
    () => {
        "The person has turned questions off in Ophio: don't ask them anything while you work. Hold every question until the end of your turn. When you can answer one yourself, by looking it up or by making the best choice, do that and carry on, and say in your summary what you chose and why, so they can change it. Otherwise finish all the work you can without it, then end your turn with a short summary that lists the questions you held."
    };
}

/// What a run with the bridge, whose `can_ask`
/// ([`crate::bridge::QUESTIONS_TOOL`]) says whether questions are on now,
/// does before it asks: the person can turn them off or on while it works.
macro_rules! asking {
    () => {
        "Before you ask the person anything, call can_ask: they can turn questions off or on while you work."
    };
}

/// How a run runs the project's checks so that Ophio can tell from each
/// command's exit status whether it passed (`crate::checks`): a check piped
/// into `grep` or followed by other commands has its status hidden.
#[cfg(not(windows))]
macro_rules! checks {
    () => {
        "Run each check (tests, build, lint) alone or after &&, never piped into grep or head: Ophio reads whether it passed from its exit status. To shorten its output, start with set -o pipefail; and pipe it into tail."
    };
}
/// On Windows, Codex runs commands in PowerShell, which has no `pipefail`
/// and, before version 7, no `&&`; Claude Code runs them in Git Bash.
#[cfg(windows)]
macro_rules! checks {
    () => {
        "Run each check (tests, build, lint) on its own, never piped into another command: Ophio reads whether it passed from its exit status."
    };
}

/// How a run tells Øphio what its turn came to, so the person is offered
/// what comes next: an answer, carrying on, new work to start, or archiving
/// it. Read by the companion's `reported` module.
macro_rules! outcome {
    () => {
        "End the last message of every turn, after your summary for the person, with a report for Ophio: a fenced code block tagged ophio-outcome holding one JSON object. Ophio hides it from them and uses it to offer what comes next. Leave out what is empty:\n- status: \"done\" when what was asked is finished; \"needs_you\" when you can't go on without the person's answer, decision or action; \"unfinished\" when you stopped with work left that you could do yourself.\n- questions: what you need them to answer or do, one short line each.\n- remaining: what is left undone, one short line each.\n- new_work: each separate piece of work this turn found or proposed that should be a task of its own, as {\"title\", \"brief\", \"mode\"}, the brief written for an agent that never saw this conversation; mode \"work\" to change things, \"plan\" to plan first, \"ask\" to find something out; \"issue\" with its number if it is a GitHub issue you filed. Not a plan you were asked to write: Ophio offers to carry that out. Nothing here starts by itself, so ask them in your summary to start what should come next.\nFor example:\n```ophio-outcome\n{\"status\":\"needs_you\",\"questions\":[\"Should the old export stay?\"]}\n```"
    };
}

/// What an agent that asks while questions are off is told, in place of an
/// answer, and `can_ask` says then.
pub const HOLD_QUESTIONS: &str = no_questions!();

/// How a run answers to the person: what it says when something blocks it;
/// with the bridge, never to wait in its turn and to check before it asks
/// whether questions are on; when they have turned questions off in
/// Ophio's settings, to hold its questions for the end of its turn; and to
/// end each turn with its outcome for Ophio. A computer-use run has none of
/// the bridge's tools for waiting or asking, and asks through its own
/// `request_human`, so it is told only what to say when blocked and how to
/// report: Codex's run is the specialist itself, and Claude's is the
/// supervisor that reports the specialist's result.
pub fn conduct_instruction(spec: &RunSpec) -> Option<&'static str> {
    if spec.bridge.as_ref().is_some_and(|b| b.computer_use) {
        return Some(concat!(blockers!(), "\n\n", outcome!()));
    }
    // Only answering on the checklist, it has no tool to wait or ask with.
    let tools = spec.bridge.as_ref().is_some_and(|b| !b.checklist_answers);
    Some(match (tools, spec.questions) {
        (false, true) => concat!(blockers!(), "\n\n", checks!(), "\n\n", outcome!()),
        (false, false) => concat!(
            blockers!(),
            "\n\n",
            no_questions!(),
            "\n\n",
            checks!(),
            "\n\n",
            outcome!()
        ),
        (true, true) => concat!(
            blockers!(),
            "\n\n",
            waiting!(),
            "\n\n",
            asking!(),
            "\n\n",
            checks!(),
            "\n\n",
            outcome!()
        ),
        (true, false) => concat!(
            blockers!(),
            "\n\n",
            waiting!(),
            "\n\n",
            asking!(),
            "\n\n",
            no_questions!(),
            "\n\n",
            checks!(),
            "\n\n",
            outcome!()
        ),
    })
}

/// Where a Work run puts what nobody needs to keep, and what happens to it.
/// Left alone, agents leave screenshots, recordings and builds in the
/// project or in a temporary folder that may be held in memory, where they
/// stay until someone deletes them by hand. None for the computer
/// specialist, which keeps what it sees in memory.
pub fn scratch_instruction(spec: &RunSpec) -> Option<String> {
    let scratch = spec.scratch.as_ref()?;
    if spec.mode != TaskMode::Work || spec.bridge.as_ref().is_some_and(|b| b.computer_use) {
        return None;
    }
    Some(format!(
        "This task has a scratch folder, `{}` (also ${SCRATCH_ENV}). Put files nobody needs to keep there, not in the project or /tmp: screenshots, screen recordings, renders, downloads, test builds, logs. Ophio deletes it with everything in it once the task is archived or two days after it finishes, so save anything worth keeping in the project. To delete from it, write its path out in full rather than ${SCRATCH_ENV}: Claude Code asks the person before any rm on a path that starts with a variable. Stop programs you started once nothing needs them, and don't start any that slip out of this run (systemd-run, a session of their own, containers) unless the person asks: Ophio can't end those for you.",
        scratch.display()
    ))
}

/// The Git branch the person chose for a task. Ophio neither creates nor
/// checks it out: the folder may hold their uncommitted changes, so the agent
/// decides how to get there safely, or says it can't. A new branch is made
/// before any file changes, so read-only runs never make it.
pub fn branch_instruction(branch: &TaskBranch) -> String {
    let name = &branch.name;
    if branch.new {
        format!(
            "The person wants this task's work done on a new Git branch `{name}`. Before changing any file, create it from what is checked out and switch to it (`git switch -c {name}` keeps every uncommitted change). If it already exists and this task didn't make it, tell them before changing anything; if this task made it, switch to it only if that keeps every uncommitted change."
        )
    } else {
        format!(
            "The person wants this task's work done on the Git branch `{name}`. If another branch is checked out, switch only if that keeps every uncommitted change; otherwise tell them before changing anything."
        )
    }
}

/// What a run starts from, as it was read when the task was made. Later runs
/// of the task use the same text even if the file has changed since: the
/// harness loads the file itself only while it is still the one read then.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RunStart {
    Agent {
        name: String,
        /// The agent file's body, without its frontmatter.
        instructions: String,
        native: Option<NativeEntry>,
        /// The agent's file, canonical.
        #[serde(default)]
        file: PathBuf,
        /// SHA-256 of the file's bytes as they were read.
        #[serde(default)]
        digest: String,
    },
    Skill {
        name: String,
        /// Absolute path of its `SKILL.md`.
        skill_file: PathBuf,
        native: Option<NativeEntry>,
        /// Its `SKILL.md` after the frontmatter; none in a start saved
        /// before it was kept.
        #[serde(default)]
        instructions: Option<String>,
        /// SHA-256 of `SKILL.md`'s bytes as they were read.
        #[serde(default)]
        digest: String,
    },
    Scratch,
}

/// An entry the harness finds itself, by its own name for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeEntry {
    pub harness: HarnessKind,
    pub name: String,
    /// From the project's folder rather than the person's own.
    pub project: bool,
}

impl RunStart {
    /// The entry the harness finds itself, if `harness` is the one it is for.
    pub fn native(&self, harness: HarnessKind) -> Option<&NativeEntry> {
        match self {
            Self::Agent { native, .. } | Self::Skill { native, .. } => {
                native.as_ref().filter(|n| n.harness == harness)
            }
            Self::Scratch => None,
        }
    }
    /// Have the harness be told about the entry rather than load it itself.
    pub fn drop_native(&mut self) {
        if let Self::Agent { native, .. } | Self::Skill { native, .. } = self {
            *native = None;
        }
    }
    /// The skill's own folder, which the agent may read.
    pub fn skill_folder(&self) -> Option<&std::path::Path> {
        match self {
            Self::Skill { skill_file, .. } => skill_file.parent(),
            _ => None,
        }
    }
    /// The file the harness would load itself, and the digest it was read
    /// with, while it is to be loaded natively.
    pub fn native_file(&self) -> Option<(&std::path::Path, &str)> {
        match self {
            Self::Agent {
                native: Some(_),
                file,
                digest,
                ..
            } => Some((file, digest)),
            Self::Skill {
                native: Some(_),
                skill_file,
                digest,
                ..
            } => Some((skill_file, digest)),
            _ => None,
        }
    }
}

/// The entry `spec` starts from that `harness` loads itself in this run. Only
/// in Work: Ask and Plan keep to their own read-only tools and settings. A
/// project's own entry only where the project's settings apply, as the
/// harness otherwise doesn't read the project's folder for it.
pub fn native_start(spec: &RunSpec, harness: HarnessKind) -> Option<&NativeEntry> {
    spec.start
        .as_ref()?
        .native(harness)
        .filter(|n| spec.mode == TaskMode::Work && (!n.project || spec.project_settings))
}

/// Whether the harness has `spec`'s start loaded itself, so its instructions
/// aren't added: an agent it loads for this run, or a skill it invokes with
/// the first prompt of a new session. A resumed session is given a skill's
/// instructions again: the run that began it may not have invoked it, as in a
/// plan carried out by Work.
pub fn start_loaded(spec: &RunSpec, harness: HarnessKind) -> bool {
    match spec.start {
        Some(RunStart::Skill { .. }) if spec.resume_session.is_some() => false,
        _ => native_start(spec, harness).is_some(),
    }
}

/// Added instructions for a run that starts from an agent or a skill, unless
/// `native` says the harness is loading it itself.
pub fn start_instruction(start: &RunStart, native: bool) -> Option<String> {
    match start {
        _ if native => None,
        RunStart::Agent {
            name, instructions, ..
        } => Some(format!(
            "For this task, act as the agent “{name}”, following its instructions below as well as these.\n\n{instructions}"
        )),
        RunStart::Skill {
            name,
            skill_file,
            instructions: None,
            ..
        } => Some(format!(
            "This task uses the skill “{name}”. Before starting, read its instructions in {} and follow them. Paths it mentions are relative to its folder.",
            skill_file.display()
        )),
        RunStart::Skill {
            name,
            instructions: Some(instructions),
            ..
        } => Some(format!(
            "This task uses the skill “{name}”: follow its instructions below as well as these. Paths they mention are relative to {}.\n\n{instructions}",
            start
                .skill_folder()
                .unwrap_or(std::path::Path::new("/"))
                .display()
        )),
        RunStart::Scratch => None,
    }
}

#[async_trait]
pub trait HarnessAdapter: Send + Sync + std::fmt::Debug + 'static {
    fn kind(&self) -> HarnessKind;

    /// Detect installation, version, account readiness, models and the
    /// truthful capability table. Must not start a paid or billable action.
    async fn probe(&self) -> HarnessInfo;

    /// Start a managed run. Returns once the native process is spawned and
    /// the adapter is ready to stream events; native acknowledgement arrives
    /// later as [`HarnessEvent::SessionStarted`].
    async fn start_run(&self, spec: RunSpec) -> Result<RunStarted, HarnessError>;
}

#[derive(Debug, Clone)]
pub struct RunSpec {
    pub run_id: RunId,
    pub task_id: TaskId,
    pub mode: TaskMode,
    pub cwd: PathBuf,
    pub prompt: PromptInput,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub launch_policy: Option<protocol::model::LaunchPolicy>,
    /// Native session to resume, if continuing earlier work.
    pub resume_session: Option<String>,
    /// Mode of the previous run, for explicit permission changes on resume.
    pub resume_from_mode: Option<TaskMode>,
    pub mission: Option<Mission>,
    /// Git branch to tell the agent to work on; see [`branch_instruction`].
    pub branch: Option<TaskBranch>,
    /// The agent or skill the task started from.
    pub start: Option<RunStart>,
    /// MCP bridge that Ophio provides (approval prompts, computer-use tools).
    pub bridge: Option<ToolBridge>,
    /// Whether the harness applies the settings committed in the project
    /// folder, which can allow tools and run commands. Only the owner turns
    /// this on, for one project, at the computer.
    pub project_settings: bool,
    /// What the computer's accounts give the run.
    pub env: RunEnv,
    /// What the run is told about the task's GitHub link: the repository,
    /// what it may post and how. Appended to the task's framing.
    pub github: Option<String>,
    /// The task's scratch folder, for files nobody needs to keep; see
    /// [`scratch_instruction`]. Ophio deletes it once the task is done.
    pub scratch: Option<PathBuf>,
    /// Whether the person welcomes questions, their setting for every task.
    /// Off, the run makes its own choices and asks only when it truly
    /// can't; see [`conduct_instruction`].
    pub questions: bool,
    /// What holds a conversation's agent to the folders and internet the
    /// person chose. None runs it as it is.
    pub sandbox: Option<std::sync::Arc<dyn crate::sandbox::Sandbox>>,
    /// The conversation may reach the internet: an Ask run is given the
    /// agent's web tools.
    pub web: bool,
    /// What the person disabled in Ophio, which the run is never given.
    pub disabled: Disabled,
}

/// Skills, agents and files the person disabled in Ophio (D-071). The
/// harness keeps each from the run, and from the agents it starts: not
/// listed, not callable, not loaded. Nothing outside the run changes, and
/// the files are never touched.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Disabled {
    /// Skills, and Claude Code's commands, which it lists as skills. One
    /// reached through a link is here once for each path, with one name.
    pub skills: Vec<DisabledEntry>,
    /// Subagent definitions.
    pub agents: Vec<DisabledEntry>,
    /// Instruction and rule files, by host path: Claude Code's CLAUDE.md
    /// files and rules, and AGENTS.md files in the run's project. A file
    /// reached through a link is here both where it was found and where it
    /// leads, as an agent may know it by either.
    pub files: Vec<PathBuf>,
    /// Claude Code's memory of the run's folder: its MEMORY.md is disabled,
    /// and Claude Code reads its memory only as a whole.
    pub memory: bool,
    /// The output style Claude Code is set to use is disabled.
    pub output_style: bool,
}

impl Disabled {
    pub fn is_empty(&self) -> bool {
        self.skills.is_empty()
            && self.agents.is_empty()
            && self.files.is_empty()
            && !self.memory
            && !self.output_style
    }
}

/// The launch arguments that keep what is disabled from an interactive
/// session of `harness` started in the folder given: Ophio's terminals start
/// Claude Code and Codex this way. Provided by the composition, which knows
/// the adapters.
#[derive(Clone)]
pub struct TerminalBlock(pub std::sync::Arc<TerminalArguments>);
/// See [`TerminalBlock`].
pub type TerminalArguments = dyn Fn(
        protocol::capability::HarnessKind,
        &std::path::Path,
        &Disabled,
    ) -> Result<Vec<String>, HarnessError>
    + Send
    + Sync;
impl std::fmt::Debug for TerminalBlock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TerminalBlock")
    }
}

/// A disabled skill or agent: its `SKILL.md` or agent file, the name its
/// harness knows it by, and that harness, when only one reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisabledEntry {
    pub path: PathBuf,
    pub name: String,
    pub harness: Option<protocol::capability::HarnessKind>,
}

/// Set in every run to the task's id. Øphio's own command line, run from a
/// task, refuses what only the person at the computer does: pairing, what a
/// device may do, and setup itself.
pub const TASK_ENV: &str = "OPHIO_TASK";

/// Set in a run to its task's scratch folder, [`RunSpec::scratch`].
pub const SCRATCH_ENV: &str = "OPHIO_SCRATCH";

/// Environment variables from the computer's accounts. The values are keys
/// and tokens, so `Debug` shows the names only.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct RunEnv(pub Vec<(String, String)>);
impl std::fmt::Debug for RunEnv {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list()
            .entries(self.0.iter().map(|(name, _)| name))
            .finish()
    }
}

/// How the adapter reaches the companion's local MCP bridge. The bridge is a
/// stdio MCP server process (`command` + `args`) that connects back to the
/// companion over a local socket bound to this run.
#[derive(Debug, Clone)]
pub struct ToolBridge {
    /// MCP server name to register with the harness.
    pub server_name: String,
    pub command: PathBuf,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    /// Tool name the harness should call for permission prompts, if the
    /// harness routes approvals through MCP (Claude Code).
    pub permission_tool: Option<String>,
    /// Computer-use tools are exposed (GUI specialist runs only).
    pub computer_use: bool,
    /// The agent browser's tools and the task's logins are exposed.
    pub browser: bool,
    /// The tools that post on the task's GitHub repository are exposed.
    pub github: bool,
    /// Only the checklist's read and reply tools are exposed
    /// (`crate::bridge::ANSWER_TOOLS`), whatever the mode: a read-only turn
    /// answering the person's question about the checklist.
    pub checklist_answers: bool,
}

#[derive(Debug, Clone, Default)]
pub struct PromptInput {
    pub text: String,
    pub images: Vec<ImageInput>,
}

#[derive(Debug, Clone)]
pub struct ImageInput {
    /// File on the host (an artifact) — adapters pass paths or read bytes as
    /// their native interface requires.
    pub path: PathBuf,
    pub media_type: String,
}

pub struct RunStarted {
    pub control: std::sync::Arc<dyn RunControl>,
    pub events: mpsc::Receiver<HarnessEvent>,
}

impl std::fmt::Debug for RunStarted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunStarted").finish_non_exhaustive()
    }
}

/// Stable companion reference for a follow-up, independent of native queue order.
/// Initial and resume prompts have no reference. Adapters return this unchanged
/// with every later delivery update, even after the send future is cancelled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MessageRef(pub CommandId);

/// Control surface of one live run.
#[async_trait]
pub trait RunControl: Send + Sync + 'static {
    /// Deliver a follow-up/steering message. Returns the delivery state the
    /// native interface can actually confirm. Later confirmations must also
    /// emit `MessageDelivery` for this reference, even if this future was dropped.
    async fn send_message(
        &self,
        message_ref: MessageRef,
        input: PromptInput,
        delivery: MessageDelivery,
    ) -> Result<DeliveryState, HarnessError>;

    /// Native interrupt of the current turn. Must not wait behind any queue.
    /// Withdraws every held message first, each reported as
    /// MessageDelivery::Cancelled before the run's exit; none is written later.
    async fn interrupt(&self) -> Result<(), HarnessError>;

    /// Refuse every later message and write no held one, from this call on.
    /// Stop and Force stop call it before anything that can wait, so a turn
    /// ending in the meantime cannot hand a held message to the agent. Each
    /// held message is withdrawn by the next turn end, `interrupt` or
    /// `terminate`, reported as MessageDelivery::Cancelled before the run's
    /// exit. A message already written is never reported withdrawn. Never waits.
    fn close_admission(&self);

    /// Answer a native approval request raised by this run (Codex JSON-RPC
    /// requests). Approvals that arrive through the MCP bridge are answered by
    /// the bridge, not here.
    async fn resolve_approval(
        &self,
        native_request_id: &str,
        decision: &ApprovalDecision,
    ) -> Result<(), HarnessError>;

    /// End the native session/process cleanly (after interrupt if needed).
    async fn shutdown(&self) -> Result<(), HarnessError>;

    /// Force termination of the owned native process group. Success confirms
    /// termination, not just delivery of a request. Detached/remote children
    /// remain separate facts. Ordinary companion shutdown must use `shutdown`.
    /// Held messages are withdrawn exactly as for `interrupt`.
    async fn terminate(&self) -> Result<(), HarnessError> {
        Err(HarnessError::Unsupported(
            "Owned process termination cannot be confirmed by this adapter".into(),
        ))
    }

    /// Remove exactly this message while still held in the adapter queue.
    /// A successful withdrawal also emits MessageDelivery::Cancelled, even
    /// when the caller drops this future. After native write, refuse it.
    async fn cancel_message(&self, _message_ref: MessageRef) -> Result<(), HarnessError> {
        Err(HarnessError::Unsupported(
            "Queued message withdrawal".into(),
        ))
    }

    /// Close message admission, discard queued work, and pause this native
    /// session. Success certifies that its process tree has exited or that the
    /// native session is idle with no running tool. An interrupt acknowledgement
    /// alone is insufficient. The caller fences input and observation first.
    /// Continuing requires an explicit new run with `resume_session`.
    async fn pause_for_private(&self) -> Result<(), HarnessError> {
        Err(HarnessError::Unsupported(
            "The adapter cannot confirm that the native session and its tools are paused".into(),
        ))
    }

    /// Managed native process identity, used only to exclude it from outside observations.
    fn process_id(&self) -> Option<u32> {
        None
    }

    /// How much of what the run started ends with it. `None` when the adapter
    /// does not keep the run's processes together.
    fn containment(&self) -> Option<Containment> {
        None
    }

    /// Native, child-scoped control. A process-group interrupt is not a substitute.
    fn child_stop_availability(&self, _native_ref: &str) -> protocol::capability::Availability {
        protocol::capability::Availability::Unsupported {
            reason: "This agent cannot stop this helper separately. Stop it in the agent’s terminal on the computer.".into(),
        }
    }

    /// Return only the result actually observed for this child after a bounded wait.
    async fn stop_child(&self, _native_ref: &str) -> Result<ChildStopResult, HarnessError> {
        Err(HarnessError::Unsupported(
            "Stop this helper in the agent’s terminal on the computer".into(),
        ))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum HarnessEvent {
    /// Native tool acknowledged the run and reported its identity.
    SessionStarted {
        native_session: String,
        actual_model: Option<String>,
        actual_effort: Option<String>,
        account_label: Option<String>,
    },
    TurnStarted,
    AgentTextDelta {
        message_key: String,
        text: String,
    },
    AgentMessage {
        message_key: String,
        text: String,
    },
    Activity {
        key: String,
        tool: String,
        summary: String,
        status: ActivityStatus,
        detail: Option<String>,
    },
    ApprovalRequested(NativeApproval),
    /// The native tool resolved/cancelled a request by itself (timeout,
    /// terminal answer, turn ended).
    ApprovalClosed {
        native_request_id: String,
        /// None when native resolution/cleanup does not disclose a decision.
        outcome: Option<ApprovalOutcome>,
    },
    Child {
        lifetime: Option<protocol::model::ChildLifetime>,
        native_ref: String,
        label: String,
        state: ChildState,
    },
    /// A follow-up update. `QueuedForBoundary` is unwritten; once dispatched,
    /// report `HostAccepted` (or `Unknown` for an uncertain write) before exit.
    /// Only a native acknowledgement establishes `NativeDelivered`.
    MessageDelivery {
        message_ref: MessageRef,
        state: DeliveryState,
    },
    TurnCompleted {
        usage: Option<UsageSummary>,
    },
    /// Process/session ended, and so has everything a contained run started.
    /// Always the last event.
    Exited {
        exit: RunExit,
        outcome: Option<RunOutcome>,
    },
    /// Non-fatal protocol oddity worth recording.
    Warning {
        text: String,
    },
    /// Cumulative native result evidence for this run. Emitted before turn completion.
    Results {
        outcome: RunOutcome,
    },
    /// Parent turn ended, but native background work still needs this controller.
    /// Child events may follow; Exited remains the final process event.
    ParentEnded {
        exit: RunExit,
    },
    /// The agent's process ended, but what the run started is not yet
    /// confirmed ended. The adapter keeps trying, sooner when asked to stop,
    /// and sends Exited once it is.
    CleanupPending,
    /// Native configuration prevents new work; also update readiness.
    ReadinessWarning {
        reason: String,
    },
    /// The account behind this run reached its usage limit. A model reply on
    /// a later run is the proof that it lifted.
    UsageLimited {
        resets_at: Option<protocol::time::Timestamp>,
    },
}

impl HarnessEvent {
    /// Calls `f` on every piece of text the event shows a person, so a value
    /// can be masked before the event is kept or sent. Identifiers the run
    /// is answered by are left alone.
    pub fn visit_text(&mut self, f: &mut dyn FnMut(&mut String)) {
        fn exit(exit: &mut RunExit, f: &mut dyn FnMut(&mut String)) {
            if let RunExit::Failed { reason } | RunExit::Unknown { reason } = exit {
                f(reason);
            }
        }
        match self {
            Self::SessionStarted { account_label, .. } => account_label.iter_mut().for_each(f),
            Self::AgentTextDelta { text, .. }
            | Self::AgentMessage { text, .. }
            | Self::Warning { text }
            | Self::ReadinessWarning { reason: text }
            | Self::Child { label: text, .. } => f(text),
            Self::Activity {
                summary, detail, ..
            } => {
                f(summary);
                detail.iter_mut().for_each(f);
            }
            Self::ApprovalRequested(native) => {
                f(&mut native.title);
                native.detail.iter_mut().for_each(&mut *f);
                native.raw_request.iter_mut().for_each(&mut *f);
                visit_serialized(&mut native.detail_lines, f);
                visit_question_text(&mut native.questions, f);
            }
            Self::Exited { exit: e, outcome } => {
                exit(e, f);
                visit_serialized(outcome, f);
            }
            Self::Results { outcome } => visit_serialized(outcome, f),
            Self::ParentEnded { exit: e } => exit(e, f),
            Self::TurnStarted
            | Self::ApprovalClosed { .. }
            | Self::MessageDelivery { .. }
            | Self::TurnCompleted { .. }
            | Self::CleanupPending
            | Self::UsageLimited { .. } => {}
        }
    }
}

/// Visit what the person reads, leaving the adapter's opaque question ids
/// intact. These ids must never be derived from native text or secrets.
pub fn visit_question_text(questions: &mut [ApprovalQuestion], f: &mut dyn FnMut(&mut String)) {
    for question in questions {
        question.header.iter_mut().for_each(&mut *f);
        f(&mut question.question);
        for option in &mut question.options {
            f(&mut option.label);
            option.description.iter_mut().for_each(&mut *f);
        }
    }
}

/// Visits every string inside a value the protocol already serializes.
pub fn visit_serialized<T>(value: &mut T, f: &mut dyn FnMut(&mut String))
where
    T: serde::Serialize + serde::de::DeserializeOwned,
{
    fn texts<'a>(value: &'a mut serde_json::Value, out: &mut Vec<&'a mut String>) {
        match value {
            serde_json::Value::String(text) => out.push(text),
            serde_json::Value::Array(items) => items.iter_mut().for_each(|v| texts(v, out)),
            serde_json::Value::Object(fields) => fields.values_mut().for_each(|v| texts(v, out)),
            _ => {}
        }
    }
    let Ok(mut tree) = serde_json::to_value(&*value) else {
        return;
    };
    let mut visited = tree.clone();
    let mut changed = Vec::new();
    let mut all = Vec::new();
    texts(&mut visited, &mut all);
    for (i, text) in all.into_iter().enumerate() {
        let before = text.clone();
        f(text);
        if *text != before {
            changed.push((i, text.clone()));
        }
    }
    if changed.is_empty() {
        return;
    }
    if let Ok(typed) = T::deserialize(&visited) {
        *value = typed;
        return;
    }
    // A changed string the type refuses is one of its own names, such as an
    // enum's, not text it carries: only that is left. Every other change is
    // kept, each checked with those kept before it.
    for (i, text) in changed {
        let mut next = tree.clone();
        let mut all = Vec::new();
        texts(&mut next, &mut all);
        *all.swap_remove(i) = text;
        if let Ok(typed) = T::deserialize(&next) {
            *value = typed;
            tree = next;
        }
    }
}

/// What ending a run reaches among the processes it started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Containment {
    /// Every process the run started, including work it moved into the
    /// background or detached from its session.
    Complete,
    /// Only processes that stayed in the agent's process group. `reason` says
    /// why nothing wider was available on this computer.
    ProcessGroup { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildStopResult {
    pub state: ChildState,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NativeApproval {
    pub native_request_id: String,
    pub kind: ApprovalKind,
    pub title: String,
    pub detail: Option<String>,
    pub detail_lines: Vec<ApprovalDetailLine>,
    /// Multiple-choice questions the agent asked, answered by choice.
    pub questions: Vec<ApprovalQuestion>,
    pub raw_request: Option<String>,
    pub scope: String,
    /// `title` or `detail` is a cut preview of `raw_request`.
    pub shortened: bool,
    /// The tool call this request is for, when the harness names it: the key
    /// its activity reports under.
    pub tool_call: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RunExit {
    Completed,
    Interrupted,
    Failed {
        reason: String,
    },
    /// Exit status could not be established.
    Unknown {
        reason: String,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum HarnessError {
    #[error("{0} is not installed")]
    NotInstalled(String),
    #[error("account not ready: {0}")]
    AccountNotReady(String),
    #[error("unsupported by this harness: {0}")]
    Unsupported(String),
    /// The process could not be spawned; no prompt was handed to it.
    #[error("could not start agent: {0}")]
    SpawnFailed(String),
    #[error("run is not active")]
    NotActive,
    #[error("native protocol error: {0}")]
    Protocol(String),
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    #[error("the message is no longer held; it may already have reached the agent")]
    MessageNotQueued,
}

impl HarnessError {
    pub fn code(&self) -> protocol::ErrorCode {
        use protocol::ErrorCode;
        match self {
            HarnessError::NotInstalled(_) => ErrorCode::Unsupported,
            HarnessError::AccountNotReady(_) => ErrorCode::AccountNotReady,
            HarnessError::Unsupported(_) => ErrorCode::Unsupported,
            HarnessError::NotActive | HarnessError::MessageNotQueued => ErrorCode::Conflict,
            HarnessError::Protocol(_) => ErrorCode::NativeFailure,
            HarnessError::SpawnFailed(_) | HarnessError::Io(_) => ErrorCode::NativeFailure,
        }
    }
}

/// Process metadata only; command lines, environment and memory are excluded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentProcess {
    pub pid: u32,
    pub ancestors: Vec<u32>,
    pub process_group: Option<u32>,
    pub start_key: String,
    pub harness: HarnessKind,
    pub executable: PathBuf,
    pub cwd: Option<PathBuf>,
    pub started_at: Option<protocol::time::Timestamp>,
}
#[derive(Debug, Clone)]
pub struct AgentSessionRecord {
    pub harness: HarnessKind,
    pub session_id: String,
    pub cwd: PathBuf,
    pub started_at: Option<protocol::time::Timestamp>,
    pub last_activity: Option<protocol::time::Timestamp>,
}
#[derive(Debug, Default)]
pub struct AgentSessionScan {
    pub processes: Vec<AgentProcess>,
    pub records: Vec<AgentSessionRecord>,
    pub coverage: Vec<String>,
}
/// Injectable operating-system table; implementations must revalidate start
/// identity and process-group scope before signalling an unmanaged process.
pub trait AgentProcessTable: Send + Sync + std::fmt::Debug + 'static {
    fn processes(&self) -> Result<Vec<AgentProcess>, HarnessError>;
    fn stop_group(&self, process: &AgentProcess) -> Result<ChildStopResult, HarnessError>;
}
#[async_trait]
pub trait AgentSessionSource: Send + Sync + std::fmt::Debug + 'static {
    async fn scan(&self) -> Result<AgentSessionScan, HarnessError>;
    async fn stop(&self, process: &AgentProcess) -> Result<ChildStopResult, HarnessError>;
}
