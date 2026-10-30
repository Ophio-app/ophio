// SPDX-License-Identifier: Apache-2.0
//! Starting a project's app before a review that uses it. The reviewer has
//! only the screen, and it holds the computer's sign-ins, so it is given no
//! shell to start the app from. Instead an agent first reads the project,
//! read-only, and says how it is started; the person allows that command
//! once; and Ophio runs it before each such review in a terminal of its own
//! in the project's folder, which the person can open in Control.

use crate::github_after::fenced;
use crate::github_watch::as_creator;
use crate::{CompanionHub, error};
use ports::tool_approval::ToolApproval;
use protocol::auth::Principal;
use protocol::command::{ApprovalDecision, Command, CommandEnvelope, CommandResult, TaskPlacement};
use protocol::model::{
    Approval, ApprovalDetailLine, ApprovalKind, Project, Run, RunState, Task, TaskKind, TaskMode,
    TaskState, TimelineBody, TimelineItem,
};
use protocol::terminal::TerminalState;
use protocol::time::Timestamp;
use protocol::{ApprovalId, ErrorCode, Event, ProtocolError, RunId, TaskId, TerminalId};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio::time::Instant;

/// The review each helper works out the start for, by the helper.
const SETUP: &str = "app_setup";
/// Reviews already sent to work out the start, so each is sent once.
const SET_UP: &str = "app_set_up";
/// What a helper said, by the review it is for, until that review starts.
const PROPOSED: &str = "app_start_proposed";
/// How each project's app is started, as the person allowed it.
pub(crate) const START: &str = "app_start";

/// How long a started app has to answer at its address: it may be built
/// and have what it needs installed first.
const STARTING: Duration = Duration::from_secs(300);
/// [`STARTING`] as the person and the reviewer are told it.
fn waited() -> String {
    format!("{} minutes", STARTING.as_secs() / 60)
}
/// How often a starting app is asked.
const ASKED_EVERY: Duration = Duration::from_secs(1);
/// How long an app with no address is given before the review starts.
const SETTLING: Duration = if cfg!(test) {
    Duration::from_millis(50)
} else {
    Duration::from_secs(20)
};
/// How the person's own direction is put after a review's prompt. The phone
/// writes it as `OPEN_DIRECTION` in client/src/app/github.ts; change both together.
const DIRECTION: &str = "How to open it: ";

#[derive(Serialize, Deserialize)]
struct Setup {
    review: TaskId,
    run: RunId,
}

/// What the helper said: a command, or why it has none.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Proposal {
    command: Option<String>,
    url: Option<String>,
    why: Option<String>,
    /// The helper's title.
    #[serde(default)]
    by: String,
}

/// How a project's app is started, and who allowed it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct AppStart {
    pub command: String,
    pub url: Option<String>,
    pub allowed_by: Principal,
    pub allowed_at: Timestamp,
}

/// Whether `task` reviews its project by using the app on a screen.
fn uses_the_app(task: &Task) -> bool {
    task.kind == TaskKind::ComputerUse && (task.github.as_deref()).is_some_and(|g| g.link.uses_it())
}

/// What the person said about opening the app, after the review's prompt.
fn direction(prompt: &str) -> Option<&str> {
    (prompt.rsplit_once(DIRECTION))
        .map(|(_, said)| said.trim())
        .filter(|said| !said.is_empty())
}

/// How the reviewer gets `at` into a browser when the screen shows none,
/// with the opener of the computer Ophio runs on.
fn browser_hint(at: &str) -> String {
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(windows) {
        "start"
    } else {
        "xdg-open"
    };
    format!(
        "When no browser is open there, start one from the app menu or launcher, or run `{opener} {at}` in a terminal."
    )
}

/// `text` cut to `max` characters.
fn cut(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", text[..i].trim_end()),
        None => text.to_owned(),
    }
}

/// What a helper's answer proposes.
fn proposed(said: &str) -> Proposal {
    #[derive(Deserialize)]
    struct Said {
        command: Option<String>,
        url: Option<String>,
        why: Option<String>,
    }
    let Some(block) = fenced(said, "ophio-start") else {
        return Proposal::none("its answer didn't end with the start command");
    };
    match serde_json::from_str::<Said>(&block) {
        Ok(Said {
            command: Some(command),
            url,
            ..
        }) if !command.trim().is_empty() => Proposal {
            command: Some(command),
            url: url.filter(|u| !u.trim().is_empty()),
            ..Proposal::default()
        },
        Ok(Said { why, .. }) => Proposal::none(
            why.map(|w| cut(w.trim(), 300))
                .filter(|w| !w.is_empty())
                .as_deref()
                .unwrap_or("it gave no reason"),
        ),
        Err(_) => Proposal::none("its start command couldn't be read"),
    }
}

impl Proposal {
    fn none(why: &str) -> Self {
        Self {
            why: Some(why.to_owned()),
            ..Self::default()
        }
    }
}

/// The helper's brief.
fn setup_prompt(project: &Project, direction: Option<&str>) -> String {
    let folder = &project.host_path;
    let direction = match direction {
        Some(said) => format!(
            "The person said how to open it: \"{}\". Give the command that does that when it is one; when it isn't, give none.\n\n",
            cut(said, 2000)
        ),
        None => String::new(),
    };
    format!(
        "Ophio is about to review this project by having another agent use the app on a screen, as the people it is for would. That agent can only see and use the screen, so Ophio starts the app for it first, and your part is to say how. The person reads the command you give, and if they allow it, Ophio runs it in a terminal in the project's folder, `{folder}`, before each review that uses the app.\n\n\
         {direction}\
         1. Read the README, the documentation and the manifests and build files, like package.json and its scripts, Cargo.toml, a Makefile or justfile, pyproject.toml, docker-compose.yml or a Procfile, for how the project says to run it while developing it.\n\
         2. Choose one command line, run from the project's folder by a POSIX shell, that starts the app so a person could use it on this computer: its development server, or the desktop app itself. When what it needs must be installed or it must be built first, join those steps with && in the order the project documents, so the command also works on a fresh checkout. Prefer what the project documents to what you guess.\n\
         3. Keep it to one line of at most 1000 characters. Never put a password, key or token in it: when the app needs one, have it read from where the project keeps it, such as an .env file.\n\
         4. When the app serves a page, give the address it answers at on this computer, like http://localhost:5173/. Give none for a desktop or terminal app.\n\
         5. When the app isn't something this computer starts from the project's folder, such as an app installed some other way, a website hosted elsewhere or a phone app, give no command, and say why.\n\n\
         You're read-only here: read the project; don't change it or run anything.\n\n\
         End your reply with one fenced block, ```ophio-start, holding a JSON object: {{\"command\": \"npm install && npm run dev\", \"url\": \"http://localhost:5173/\"}}, with \"url\": null when it serves no page, or {{\"command\": null, \"why\": \"one short line\"}} when there is no command to give."
    )
}

/// How the app came up after its command was typed.
enum Up {
    /// It answers at its address.
    Answers,
    /// Its terminal remains open, and it answers when it has an address to ask.
    Running,
    /// Nothing answered at its address in time.
    Silent,
    /// It has no address to ask; it was given a while.
    Started,
    /// Its terminal closed.
    Closed,
}

impl CompanionHub {
    /// Has an agent work out how to start the app first, when `task_id`
    /// reviews by using it and the person hasn't allowed a way yet, or has
    /// said how to open it for this review. Its run starts once that agent
    /// has answered. Whether it was sent; anything that stops it from being
    /// sent leaves the review to start as before.
    pub(crate) async fn app_setup_first(&self, task_id: TaskId, run_id: RunId) -> bool {
        if self.inner.terminal.is_none() || self.inner.bridge.is_none() {
            return false;
        }
        match self.ask_app_setup(task_id, run_id).await {
            Ok(sent) => sent,
            Err(e) => {
                tracing::warn!(code = ?e.code, "working out how to start a reviewed app failed");
                false
            }
        }
    }

    async fn ask_app_setup(&self, task_id: TaskId, run_id: RunId) -> Result<bool, ProtocolError> {
        let wanted = self.write(|tx| {
            let task: Task = tx.read().require("task", task_id)?;
            if !uses_the_app(&task) || tx.read().get::<bool>(SET_UP, task_id)?.is_some() {
                return Ok(None);
            }
            let run: Run = tx.read().require("run", run_id)?;
            if run.state != RunState::Admitted
                || tx.read().get::<bool>("stopped", run_id)?.unwrap_or(false)
            {
                return Ok(None);
            }
            let (prompt, _): (String, Option<Box<protocol::annotation::AnnotationBundle>>) =
                tx.read().require("prompt", task_id)?;
            let said = direction(&prompt).map(str::to_owned);
            let allowed = tx.read().get::<AppStart>(START, task.project_id)?.is_some();
            if allowed && said.is_none() {
                return Ok(None);
            }
            let project: Project = tx.read().require("project", task.project_id)?;
            tx.put(SET_UP, task_id, &true)?;
            Ok(Some((task, project, said)))
        })?;
        let Some((task, project, said)) = wanted else {
            return Ok(false);
        };
        let title = cut(&format!("How to start {}", project.name), 79);
        let envelope = CommandEnvelope::new(Command::CreateTask {
            project_id: task.project_id,
            kind: TaskKind::Coding,
            mode: Some(TaskMode::Ask),
            title: Some(title.clone()),
            prompt: setup_prompt(&project, said.as_deref()),
            bundle: None,
            harness: task.harness,
            model: task.model.clone(),
            effort: task.effort.clone(),
            // The person's launch flags could undo read-only.
            launch_policy: None,
            branch: None,
            start: None,
            place: None,
            browser: Some(false),
            github: None,
            mission: None,
            target_id: None,
            placement: TaskPlacement::StartNow,
            agent_chosen: None,
            conversation: None,
            access: None,
            review: None,
        });
        let receipt = self.admit(&as_creator(&task), envelope).await;
        let helper = match (receipt.error, receipt.result) {
            (None, Some(CommandResult::TaskCreated { task_id, .. })) => task_id,
            (e, _) => {
                let why = e.map_or_else(|| "it couldn't be started".to_owned(), |e| e.message);
                self.app_note(
                    task_id,
                    run_id,
                    format!(
                        "Øphio couldn't ask an agent how to start {}: {why} The reviewer brings it up itself.",
                        project.name
                    ),
                )?;
                return Ok(false);
            }
        };
        let now = self.inner.clock.now();
        self.write(|tx| {
            tx.put(
                SETUP,
                helper,
                &Setup {
                    review: task_id,
                    run: run_id,
                },
            )?;
            if let Some(mut asked) = tx.read().get::<Task>("task", helper)? {
                asked.asked_by = Some(task_id);
                tx.put("task", helper, &asked)?;
                tx.emit(Event::TaskUpserted { task: asked })?;
            }
            tx.emit(Event::TimelineAppended {
                task_id,
                item: TimelineItem {
                    seq: 0,
                    at: now,
                    run_id: Some(run_id),
                    item: TimelineBody::Note {
                        text: format!(
                            "Working out how to start {} first, in “{title}”. The review starts once it has answered.",
                            project.name
                        ),
                    },
                },
            })?;
            Ok(())
        })?;
        Ok(true)
    }

    /// Starts the app before `task`'s turn when the person has allowed how,
    /// asking them first about a way just worked out, and says how it went
    /// for the reviewer's brief. Fails only when the run was stopped
    /// meanwhile; anything else leaves the reviewer to bring the app up.
    pub(crate) async fn bring_app_up(
        &self,
        task: &Task,
        project: &Project,
        run_id: RunId,
    ) -> Result<Option<String>, ProtocolError> {
        if !uses_the_app(task) || self.inner.terminal.is_none() {
            return Ok(None);
        }
        let (proposal, allowed) = self.write(|tx| {
            let proposal = tx.read().get::<Proposal>(PROPOSED, task.task_id)?;
            if proposal.is_some() {
                tx.delete(PROPOSED, task.task_id)?;
            }
            Ok((
                proposal,
                tx.read().get::<AppStart>(START, project.project_id)?,
            ))
        })?;
        let start = match proposal {
            Some(proposal) => self.allow_start(task, project, run_id, proposal).await?,
            None => allowed,
        };
        match start {
            Some(start) => self.start_app(task, project, run_id, &start).await,
            None => Ok(None),
        }
    }

    /// Asks the person whether the start command proposed may run, and keeps
    /// it for the project when they allow it.
    async fn allow_start(
        &self,
        task: &Task,
        project: &Project,
        run_id: RunId,
        proposal: Proposal,
    ) -> Result<Option<AppStart>, ProtocolError> {
        let name = &project.name;
        let by = &proposal.by;
        let Some(command) = proposal.command else {
            let why = proposal.why.as_deref().unwrap_or("it gave no reason");
            self.app_note(
                task.task_id,
                run_id,
                format!(
                    "“{by}” gave no command that starts {name}: {why}. The reviewer brings it up itself."
                ),
            )?;
            return Ok(None);
        };
        // The person reads it and a shell runs it: one plain line with
        // nothing Ophio holds in it, and a page on this computer.
        let hide = self.found_masking();
        let command = match crate::shown::start_command(Some(command), &hide) {
            Ok(Some(command)) => Ok(command),
            _ => Err(
                "it isn't one plain line of at most 1000 characters without a key or token in it",
            ),
        };
        let url = match proposal.url.as_deref().map(crate::shown::loopback) {
            None => Ok(None),
            Some(Ok(url)) => {
                let mut seen = url.clone();
                hide(&mut seen);
                if seen == url {
                    Ok(Some(url))
                } else {
                    Err("its address holds a key or token")
                }
            }
            Some(Err(_)) => Err("its address isn't a page on this computer"),
        };
        let (command, url) = match (command, url) {
            (Ok(command), Ok(url)) => (command, url),
            (Err(why), _) | (_, Err(why)) => {
                self.app_note(
                    task.task_id,
                    run_id,
                    format!(
                        "Øphio won't run the start command “{by}” proposed: {why}. The reviewer brings {name} up itself."
                    ),
                )?;
                return Ok(None);
            }
        };
        let mut lines = vec![
            ApprovalDetailLine {
                label: Some("Command".into()),
                text: command.clone(),
                code: true,
                change: None,
            },
            ApprovalDetailLine {
                label: Some("Folder".into()),
                text: project.host_path.clone(),
                code: true,
                change: None,
            },
        ];
        if let Some(url) = &url {
            lines.push(ApprovalDetailLine {
                label: Some("Address".into()),
                text: url.clone(),
                code: false,
                change: None,
            });
        }
        let id = ApprovalId::new();
        let facts = ToolApproval {
            kind: ApprovalKind::Command,
            title: format!("Start {name} with this command?"),
            detail: Some(format!(
                "“{by}” read the project and says this command starts {name}. If you allow it, Øphio runs it in the project's folder, in a terminal named “App: {name}” in Control, before this review and each later one that uses the app. If you don't, the reviewer brings it up itself."
            )),
            detail_lines: lines,
            questions: Vec::new(),
            scope: "This project".into(),
            shortened: false,
        };
        let decision = tokio::select! {
            decision = self.bridge_wait_as(id, run_id, task, facts, None, None, None) => decision,
            _ = self.until_stopped(run_id) => {
                self.inner.waits.lock().await.remove(&id);
                return Err(error(ErrorCode::Expired, "Run was stopped"));
            }
        };
        // Stop cancels the question too, which isn't the person declining.
        if self.run_stopped(run_id)? {
            return Err(error(ErrorCode::Expired, "Run was stopped"));
        }
        self.back_to_starting(run_id, id)?;
        match decision {
            Ok(ApprovalDecision::Allow) => {
                let approval: Approval = self.read(|r| r.require("approval", id))?;
                let start = AppStart {
                    command,
                    url,
                    allowed_by: approval.resolved_by.unwrap_or(task.created_by),
                    allowed_at: self.inner.clock.now(),
                };
                self.write(|tx| tx.put(START, project.project_id, &start))?;
                Ok(Some(start))
            }
            Ok(_) => {
                self.app_note(
                    task.task_id,
                    run_id,
                    format!(
                        "The start command wasn't allowed, so the reviewer brings {name} up itself."
                    ),
                )?;
                Ok(None)
            }
            Err(_) => {
                self.app_note(
                    task.task_id,
                    run_id,
                    format!("Nobody answered about the start command, so the reviewer brings {name} up itself."),
                )?;
                Ok(None)
            }
        }
    }

    /// Runs the app's start command in its terminal, unless that terminal
    /// still runs it and it still answers when an HTTP check is available,
    /// and waits until it answers or has had a while.
    async fn start_app(
        &self,
        task: &Task,
        project: &Project,
        run_id: RunId,
        start: &AppStart,
    ) -> Result<Option<String>, ProtocolError> {
        let terminal =
            (self.inner.terminal.clone()).ok_or_else(|| crate::unsupported("Terminal"))?;
        let name = &project.name;
        let title = format!("App: {name}");
        let url = (start.url.as_deref()).and_then(|u| crate::preview::validate(u).ok());
        let checker = self.inner.preview.clone().filter(|_| url.is_some());
        let answers = || async {
            match (&checker, &url) {
                (Some(checker), Some(url)) => matches!(
                    tokio::time::timeout(Duration::from_secs(5), checker.check(url)).await,
                    Ok(Ok(true))
                ),
                _ => false,
            }
        };
        let running = |id: TerminalId| {
            let terminal = terminal.clone();
            async move { (terminal.info(id).await).is_some_and(|t| t.state == TerminalState::Running) }
        };
        let opened = {
            let mut terminals = self.inner.app_terminals.lock().await;
            let before = terminals.get(&project.project_id).cloned();
            match before {
                // No HTTP check is not a failure: closing a running desktop
                // app here would kill its process and discard unsaved work.
                Some((id, ran))
                    if ran == start.command
                        && running(id).await
                        && if checker.is_some() {
                            answers().await
                        } else {
                            // A surviving login shell alone is not a running
                            // app. Unknown activity must still preserve it.
                            !matches!(terminal.has_running_children(id).await, Ok(Some(false)))
                        } =>
                {
                    Err(id)
                }
                before => {
                    if let Some((id, _)) = before {
                        terminals.remove(&project.project_id);
                        let _ = terminal.close(id).await;
                    }
                    let command = Command::OpenTerminal {
                        project_id: Some(project.project_id),
                        cwd: None,
                        harness: None,
                        cols: 120,
                        rows: 32,
                    };
                    let id = self
                        .open_project_shell(&as_creator(task), &command, project.project_id, &title)
                        .await;
                    let typed = match id {
                        Ok(id) => {
                            match Self::type_line(terminal.as_ref(), id, &start.command).await {
                                Ok(()) => Ok(id),
                                Err(e) => {
                                    let _ = terminal.close(id).await;
                                    Err(e)
                                }
                            }
                        }
                        Err(e) => Err(e),
                    };
                    match typed {
                        Ok(id) => {
                            terminals.insert(project.project_id, (id, start.command.clone()));
                            Ok(Ok(id))
                        }
                        Err(e) => Ok(Err(e)),
                    }
                }
            }
        };
        let up = match opened {
            Err(_) => Up::Running,
            Ok(Err(e)) => {
                self.app_note(
                    task.task_id,
                    run_id,
                    format!(
                        "Øphio couldn't start {name}: {} The reviewer brings it up itself.",
                        e.message
                    ),
                )?;
                return Ok(None);
            }
            Ok(Ok(id)) => {
                let (wait, asks) = match checker.is_some() {
                    true => (STARTING, true),
                    false => (SETTLING, false),
                };
                let until = Instant::now() + wait;
                loop {
                    if self.run_stopped(run_id)? {
                        return Err(error(ErrorCode::Expired, "Run was stopped"));
                    }
                    if asks && answers().await {
                        break Up::Answers;
                    }
                    if !running(id).await {
                        break Up::Closed;
                    }
                    if Instant::now() >= until {
                        break if asks { Up::Silent } else { Up::Started };
                    }
                    tokio::time::sleep(ASKED_EVERY.min(wait)).await;
                }
            }
        };
        let command = &start.command;
        let at = start.url.as_deref().unwrap_or_default();
        let waited = waited();
        let browser = browser_hint(at);
        let (seen, told) = match up {
            Up::Running => (
                format!(
                    "The terminal “{title}” in Control is still open, so the review uses it as it is."
                ),
                Some(if checker.is_some() {
                    format!(
                        "Ophio has started the app for this review: it runs `{command}` in the project's folder, in a terminal of its own that isn't on the screen, and the app answers at {at}. Open {at} in the browser on the screen, and don't start another copy. {browser} If it stops answering, bring it up yourself as below."
                    )
                } else {
                    format!(
                        "Ophio kept the terminal from the earlier review open: it ran `{command}` in the project's folder, in a terminal of its own that isn't on the screen. This does not confirm that the app is still running. Look for its existing window and use it as it is, preserving any unsaved work; don't start another copy of a running app. If it is still building, wait for it. If the app has exited, bring it up yourself as below."
                    )
                }),
            ),
            Up::Answers => (
                format!(
                    "Started {name} with `{command}` in the terminal “{title}” in Control. It answers at {at}."
                ),
                Some(format!(
                    "Ophio has started the app for this review: it ran `{command}` in the project's folder, in a terminal of its own that isn't on the screen, and the app answers at {at}. Open {at} in the browser on the screen, and don't start another copy. {browser} If it stops answering, bring it up yourself as below."
                )),
            ),
            Up::Silent => (
                format!(
                    "Started {name} with `{command}` in the terminal “{title}” in Control, but nothing answered at {at} within {waited}. The reviewer tries it, and brings it up itself if it isn't there."
                ),
                Some(format!(
                    "Ophio ran `{command}` in the project's folder for this review, in a terminal of its own that isn't on the screen, but nothing answered at {at} within {waited}. Try {at} in the browser on the screen. {browser} When it doesn't answer, bring the app up yourself as below."
                )),
            ),
            Up::Started => (
                format!("Started {name} with `{command}` in the terminal “{title}” in Control."),
                Some(format!(
                    "Ophio has started the app for this review: it ran `{command}` in the project's folder, in a terminal of its own that isn't on the screen. Its window may take a few minutes to appear while it builds: when it isn't on the screen yet, wait and look again before anything else, and don't start another copy. When it still hasn't appeared after a few minutes, bring it up yourself as below."
                )),
            ),
            Up::Closed => (
                format!(
                    "The terminal “{title}” closed after Øphio ran `{command}` in it. The reviewer brings {name} up itself."
                ),
                Some(format!(
                    "Ophio tried to start the app with `{command}` in the project's folder, but that failed, so bring it up yourself as below."
                )),
            ),
        };
        self.app_note(task.task_id, run_id, seen)?;
        Ok(told)
    }

    /// Resolves once `run_id` has been stopped.
    async fn until_stopped(&self, run_id: RunId) {
        loop {
            if self.run_stopped(run_id).unwrap_or(true) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    /// After the person was asked, the run is starting again, as it was
    /// before: its agent hasn't started yet.
    fn back_to_starting(&self, run_id: RunId, asked: ApprovalId) -> Result<(), ProtocolError> {
        let now = self.inner.clock.now();
        self.write(|tx| {
            crate::runs::resolve_attention(tx, asked, now)?;
            let mut run: Run = tx.read().require("run", run_id)?;
            if !matches!(run.state, RunState::AwaitingApproval | RunState::Working)
                || tx.read().get::<bool>("stopped", run_id)?.unwrap_or(false)
            {
                return Ok(());
            }
            run.state = RunState::Dispatching;
            tx.put("run", run_id, &run)?;
            let mut task: Task = tx.read().require("task", run.task_id)?;
            tx.emit(Event::RunUpserted { run: Box::new(run) })?;
            task.state = TaskState::Dispatching;
            task.revision += 1;
            task.updated_at = now;
            tx.put("task", task.task_id, &task)?;
            tx.emit(Event::TaskUpserted { task })?;
            Ok(())
        })
    }

    fn app_note(&self, task_id: TaskId, run_id: RunId, text: String) -> Result<(), ProtocolError> {
        let now = self.inner.clock.now();
        self.write(|tx| {
            tx.emit(Event::TimelineAppended {
                task_id,
                item: TimelineItem {
                    seq: 0,
                    at: now,
                    run_id: Some(run_id),
                    item: TimelineBody::Note { text },
                },
            })?;
            Ok(())
        })
    }
}

/// When `task` worked out how to start a review's app, keeps what it said
/// for that review and hands back the review's run to start now. Whatever
/// it said is the review's, not the person's, to act on.
pub(crate) fn answered(
    tx: &mut store::Write<'_>,
    task: &Task,
    run_id: RunId,
    state: RunState,
) -> store::Result<Option<(TaskId, RunId)>> {
    let Some(setup) = tx.read().get::<Setup>(SETUP, task.task_id)? else {
        return Ok(None);
    };
    tx.delete(SETUP, task.task_id)?;
    let waiting = (tx.read().get::<Run>("run", setup.run)?)
        .is_some_and(|run| run.state == RunState::Admitted);
    if !waiting {
        return Ok(None);
    }
    let mut proposal = match state {
        RunState::Completed => match crate::asking::last_message(&tx.read(), task.task_id, run_id)?
        {
            Some(said) => proposed(&said),
            None => Proposal::none("it ended without an answer"),
        },
        RunState::Interrupted => Proposal::none("it was stopped"),
        RunState::Failed => Proposal::none("it failed"),
        _ => Proposal::none("how it ended isn't known"),
    };
    proposal.by = task.title.clone();
    tx.put(PROPOSED, setup.review, &proposal)?;
    Ok(Some((setup.review, setup.run)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_helper_answer_reads_as_a_command_or_why_there_is_none() {
        let p = proposed(
            "It's a Vite app.\n\n```ophio-start\n{\"command\": \"npm ci && npm run dev\", \"url\": \"http://localhost:5173/\"}\n```",
        );
        assert_eq!(p.command.as_deref(), Some("npm ci && npm run dev"));
        assert_eq!(p.url.as_deref(), Some("http://localhost:5173/"));
        let p = proposed("```ophio-start\n{\"command\": \"cargo run\", \"url\": null}\n```");
        assert_eq!(p.command.as_deref(), Some("cargo run"));
        assert_eq!(p.url, None);
        let p =
            proposed("```ophio-start\n{\"command\": null, \"why\": \"It's a phone app.\"}\n```");
        assert_eq!(p.command, None);
        assert_eq!(p.why.as_deref(), Some("It's a phone app."));
        let p = proposed("Run npm start.");
        assert_eq!(p.command, None);
        assert!(p.why.unwrap().contains("didn't end with"));
        let p = proposed("```ophio-start\n{\"command\": \n```");
        assert!(p.why.unwrap().contains("couldn't be read"));
    }

    #[test]
    fn the_persons_direction_is_read_from_after_the_prompt() {
        assert_eq!(
            direction("Try the app.\n\nHow to open it: run make dev"),
            Some("run make dev")
        );
        assert_eq!(direction("Try the app."), None);
        assert_eq!(direction("Try the app.\n\nHow to open it:  "), None);
    }

    /// The phone writes the person's direction with this marker, and the
    /// computer finds it by the same text.
    #[test]
    fn the_phone_marks_the_direction_as_it_is_read() {
        let github = include_str!("../../../client/src/app/github.ts");
        let literal = format!("OPEN_DIRECTION = \"{DIRECTION}\"");
        assert!(github.contains(&literal), "{literal}");
        let review = include_str!("../../../client/src/screens/github/GithubNew.tsx");
        assert!(review.contains("${OPEN_DIRECTION}"));
        assert!(!review.contains(&format!("`{DIRECTION}")));
    }

    #[test]
    fn the_wait_is_said_as_it_is() {
        assert_eq!(waited(), "5 minutes");
    }
}
