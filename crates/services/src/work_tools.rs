// SPDX-License-Identifier: Apache-2.0
//! The tools an agent plans the person's GitHub work with when they ask it
//! to, usually in passing from Talk: the agents, models and defaults it may
//! choose from, issues it files on the project's repository, and work it
//! starts on them. Filing starts nothing. Work it starts waits in Next up
//! like any other, and starts by itself only when the person asked for it.
//!
//! A task working a GitHub item has its own tools and doesn't plan others'.
//! Every run with these tools, one working on GitHub too, may list the
//! agents and ask one of them for help (`crate::asking`).
use crate::agent_browser::harness_name;
use crate::github::{REPO, linked_task, login, project_tasks, read_repo, repo_key, segment, text};
use crate::github_tools::{
    AGENT_LABEL, Filed, Posting, checked_body, checked_title, via, without_disclosure,
};
use crate::github_watch::as_creator;
use crate::runs::{QueuePermit, bump_queue_revision, sync_queue_attention};
use crate::{CompanionHub, error};
use ports::bridge::{AgentChoice, WorkCall};
use ports::github::Method;
use protocol::capability::{AccountReadiness, Availability, HarnessInfo, HarnessKind};
use protocol::command::{Command, CommandEnvelope, CommandResult, TaskPlacement};
use protocol::github::*;
use protocol::model::{Project, Task, TaskKind, TaskState};
use protocol::queue::{QueueAuthorization, QueueView};
use protocol::time::Timestamp;
use protocol::{ErrorCode, ProtocolError, QueueItemId, RunId, TaskId};
use serde::{Deserialize, Serialize};
use serde_json::json;

/// Issues a run may file, and work it may start, each.
const MAX_PLANNED: u32 = 20;
/// Agents a run may ask.
const MAX_ASKED: u32 = 10;
/// What each run has filed and started, against `MAX_PLANNED`.
const PLANNED: &str = "work_planned";
/// How long work the person asked to start may start by itself: a day, so
/// a batch queued behind a long task still starts.
const START_PERMIT_MS: i64 = 24 * 3_600_000;
const OFF: &str = "The GitHub workflow is off for this project. Ask the person to turn it on in Tasks › GitHub; nothing was filed or started.";

#[derive(Default, Serialize, Deserialize)]
struct Planned {
    filed: u32,
    started: u32,
    /// What the person asked this run to start by itself.
    #[serde(default)]
    starting: Vec<QueueItemId>,
    #[serde(default)]
    asked: u32,
}

/// Which of a run's allowances a call takes.
#[derive(Clone, Copy)]
pub(crate) enum Allowance {
    File,
    Start,
    Ask,
}

/// Whether an agent can be started now, and if not, why not.
fn ready(h: &HarnessInfo) -> Result<(), String> {
    let unavailable = |a: &Availability| match a {
        Availability::Available => None,
        Availability::Unsupported { reason } | Availability::TemporarilyUnavailable { reason } => {
            Some(reason.clone())
        }
        Availability::PermissionMissing { remedy } => Some(remedy.clone()),
        Availability::Incompatible { reason } | Availability::Unknown { reason } => {
            Some(reason.clone())
        }
        Availability::Revoked => Some("its permission was revoked".into()),
    };
    if let Some(why) = unavailable(&h.installed) {
        return Err(format!("not ready: {why}"));
    }
    if let Some(why) = unavailable(&h.capabilities.start_run) {
        return Err(format!("can't start work: {why}"));
    }
    if let AccountReadiness::SignedOut { remedy } = &h.account {
        return Err(format!("signed out: {remedy}"));
    }
    Ok(())
}

/// The snake-case name an agent passes for `kind`.
fn kind_id(kind: HarnessKind) -> String {
    serde_json::to_value(kind)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// "in about 2 h" from `now`.
fn from_now(now: Timestamp, at: Timestamp) -> String {
    let minutes = (at.0 - now.0).max(0) / 60_000;
    match minutes {
        0 => "now".into(),
        m if m < 90 => format!("in about {m} min"),
        m => format!("in about {} h", (m + 30) / 60),
    }
}

/// One agent, as `ophio_agents` lists it.
fn described(h: &HarnessInfo, now: Timestamp) -> String {
    let mut out = format!("- {} (agent: \"{}\")", h.display_name, kind_id(h.kind));
    match ready(h) {
        Ok(()) => out.push_str(": ready"),
        Err(why) => out.push_str(&format!(": {why}")),
    }
    if let AccountReadiness::SignedIn {
        plan: Some(plan), ..
    } = &h.account
    {
        out.push_str(&format!(", {plan} plan"));
    }
    if let Some(limit) = &h.usage_limit {
        match limit.resets_at {
            Some(at) => out.push_str(&format!(", at its usage limit until {}", from_now(now, at))),
            None => out.push_str(", at its usage limit"),
        }
    }
    for m in &h.models {
        out.push_str(&format!("\n  - model \"{}\" ({})", m.id, m.label));
        if m.is_default {
            out.push_str(", its default");
        }
        if let Some(d) = m.details.as_ref().filter(|d| !d.efforts.is_empty()) {
            let efforts: Vec<String> = (d.efforts.iter())
                .map(
                    |e| match d.default_effort.as_deref() == Some(e.id.as_str()) {
                        true => format!("\"{}\" (default)", e.id),
                        false => format!("\"{}\"", e.id),
                    },
                )
                .collect();
            out.push_str(&format!("; efforts {}", efforts.join(", ")));
        }
    }
    out
}

/// The words for a profile, as the person sees them in Find/Fix › New review.
fn profile_id(focus: Option<FindFocus>) -> String {
    match focus {
        None => "default".into(),
        Some(f) => serde_json::to_value(f)
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_default(),
    }
}

const PROFILES: [Option<FindFocus>; 13] = [
    None,
    Some(FindFocus::Bugs),
    Some(FindFocus::Security),
    Some(FindFocus::UiUx),
    Some(FindFocus::Accessibility),
    Some(FindFocus::Performance),
    Some(FindFocus::Reliability),
    Some(FindFocus::Tests),
    Some(FindFocus::Maintainability),
    Some(FindFocus::Dependencies),
    Some(FindFocus::Privacy),
    Some(FindFocus::Api),
    Some(FindFocus::Docs),
];

/// Why an agent can't start a reviewer that uses the app: it works on a
/// screen the person chooses, and only the phone asks which.
const UAT_FROM_THE_PHONE: &str = "A reviewer that uses the app works on a screen the person chooses, so they start it from Tasks › Find/Fix › New review.";

/// A profile as a review's title names it, like the app's.
fn profile_words(focus: FindFocus) -> &'static str {
    match focus {
        FindFocus::Bugs => "bugs",
        FindFocus::Security => "security",
        FindFocus::Tests => "test",
        FindFocus::Accessibility => "accessibility",
        FindFocus::UiUx => "UI/UX",
        FindFocus::Performance => "performance",
        FindFocus::Reliability => "reliability",
        FindFocus::Maintainability => "code quality",
        FindFocus::Dependencies => "dependency",
        FindFocus::Privacy => "privacy",
        FindFocus::Api => "API",
        FindFocus::Docs => "docs",
        FindFocus::EverydayUser => "everyday user",
        FindFocus::NoviceUser => "novice user",
        FindFocus::PowerUser => "power user",
    }
}

fn then_words(then: FixFindings, fixer: &str) -> String {
    match then {
        FixFindings::ByItself => format!(
            "{fixer} checks and fixes each one, starting by itself in Next up once the project is free"
        ),
        FixFindings::AskFirst => {
            format!("a fix by {fixer} waits in Next up for each one until the person starts it")
        }
        FixFindings::FileOnly => {
            "nothing more happens: the person starts the fixes they want from Tasks › GitHub › Issues".into()
        }
    }
}

impl CompanionHub {
    pub(crate) async fn work_request(
        &self,
        run_id: RunId,
        task: &Task,
        call: WorkCall,
    ) -> Result<String, ProtocolError> {
        match call {
            WorkCall::Agents => self.agents_text(task).await,
            WorkCall::Ask {
                agent,
                prompt,
                title,
            } => self.ask_agent(run_id, task, agent, prompt, title).await,
            _ if task.github.is_some() => Err(error(
                ErrorCode::PermissionDenied,
                "A task working on GitHub doesn't plan other work.",
            )),
            // Filed on GitHub, it would carry what this chat read past the
            // internet the person kept it from.
            WorkCall::NewIssue { .. } | WorkCall::StartReview { .. }
                if crate::access::held(task).is_some_and(|a| !a.internet) =>
            {
                Err(error(
                    ErrorCode::PermissionDenied,
                    "Issues go to GitHub, and the person hasn't given this chat Internet. Ask them to turn it on in the chat's access, or give them what you'd file.",
                ))
            }
            WorkCall::NewIssue {
                title,
                body,
                labels,
                distinct_from,
            } => {
                let issue = (&*title, &*body, &*distinct_from);
                self.new_issue(run_id, task, issue, &labels).await
            }
            WorkCall::StartIssue {
                number,
                agent,
                start,
                note,
                reviewer,
            } => {
                let agents = (agent, reviewer);
                // Work a chat held to its access plans runs past it, so it
                // waits for the person to start it.
                let held = crate::access::held(task).is_some();
                let mut said = self
                    .start_issue(
                        run_id,
                        task,
                        number,
                        agents,
                        start && !held,
                        note.as_deref(),
                    )
                    .await?;
                if start && held {
                    said.push_str(" This chat is held to the access the person chose, so work it plans waits for them to start it.");
                }
                Ok(said)
            }
            WorkCall::StartReview {
                profile,
                folder,
                reviewer,
                fixer,
                max_issues,
                then,
            } => {
                let link = (profile, folder, max_issues, then);
                self.start_review(run_id, task, link, reviewer, fixer).await
            }
        }
    }

    /// What `ophio_agents` says: the agents there are, the person's
    /// defaults for the project and, to a task that may plan GitHub work,
    /// its GitHub setup.
    async fn agents_text(&self, task: &Task) -> Result<String, ProtocolError> {
        let planning = task.github.is_none();
        let now = self.inner.clock.now();
        let (catalog, project, settings, queue) = self.read(|r| {
            Ok((
                r.get::<Vec<HarnessInfo>>("meta", "harnesses")?
                    .unwrap_or_default(),
                r.require::<Project>("project", task.project_id)?,
                r.get::<GithubSettings>(crate::github::SETTINGS, task.project_id)?
                    .unwrap_or_default(),
                r.get::<QueueView>("queue", task.project_id)?,
            ))
        })?;
        let mut out = vec!["Agents on this computer:".to_owned()];
        out.extend(catalog.iter().map(|h| described(h, now)));
        if catalog.is_empty() {
            out.push("- none found".into());
        }
        out.push(String::new());
        out.push(match project.default_harness {
            Some(kind) => {
                let mut d = format!("The person's default for this project: {}", harness_name(kind));
                if let Some(m) = &project.default_model {
                    d.push_str(&format!(", model \"{m}\""));
                }
                if let Some(e) = &project.default_effort {
                    d.push_str(&format!(", effort \"{e}\""));
                }
                d.push('.');
                if planning {
                    d.push_str(" Name an agent only when they ask for one; with none named, work goes to this one, and parts you leave out of a choice of this agent are these defaults.");
                }
                d
            }
            None if planning => "The person hasn't chosen a default agent for this project, so work you start without naming one goes to the first ready agent above, with its own default model.".into(),
            None => "The person hasn't chosen a default agent for this project.".into(),
        });
        out.push(format!(
            "ask_agent asks one of these, read-only, and brings its answer back to you. You may ask {MAX_ASKED} in this run."
        ));
        if !planning {
            return Ok(out.join("\n"));
        }
        if project.active_writer.is_some() {
            out.push("A run is working in the project now, maybe this one, so work you start waits in Next up until it ends.".into());
        }
        if queue.is_some_and(|q| q.paused) {
            out.push("Next up is paused. Work the person asks you to start resumes it; work that waits for them doesn't.".into());
        }
        out.push(String::new());
        let origin = match self.inner.github.is_some() {
            true => self.on_github(task.project_id).await.ok(),
            false => None,
        };
        match origin {
            None if self.inner.github.is_none() => {
                out.push("GitHub isn't set up in Ophio on this computer, so you can't file or start GitHub work.".into());
            }
            None => out.push(
                "This project's `origin` isn't on GitHub, so you can't file or start GitHub work."
                    .into(),
            ),
            Some((owner, name)) => {
                out.push(format!("GitHub: {owner}/{name}, this project's `origin`."));
                out.push(match settings.mode {
                    GithubMode::Off => OFF.into(),
                    GithubMode::ThroughMerge => "The workflow is on: agents' pull requests merge by themselves once they're green.".into(),
                    GithubMode::Drafts => "The workflow is on: agents' pull requests stay drafts for the person to merge.".into(),
                });
                if settings.mode != GithubMode::Off {
                    let profiles: Vec<String> = PROFILES.iter().map(|p| profile_id(*p)).collect();
                    out.push(format!(
                        "A review files at most {} issues unless you say otherwise (1 to {MAX_FINDINGS_PER_RUN}); then, by the project's setting, {}.",
                        settings.workflow.findings_per_run,
                        then_words(settings.workflow.fix_findings, "the fixer"),
                    ));
                    out.push(format!(
                        "Review profiles: {}. Then: \"by_itself\", \"ask_first\" or \"file_only\".",
                        profiles.join(", ")
                    ));
                }
            }
        }
        out.push(format!(
            "You may file {MAX_PLANNED} issues and start {MAX_PLANNED} pieces of work in this run."
        ));
        Ok(out.join("\n"))
    }

    /// The project's settings, refusing while its workflow is off.
    fn planning_settings(&self, task: &Task) -> Result<GithubSettings, ProtocolError> {
        self.github()?;
        let settings = self.github_settings(task.project_id)?;
        if settings.mode == GithubMode::Off {
            return Err(error(ErrorCode::PermissionDenied, OFF));
        }
        Ok(settings)
    }

    /// Takes one of the run's allowance for `what`.
    pub(crate) fn take_planned(&self, run_id: RunId, what: Allowance) -> Result<(), ProtocolError> {
        self.write(|tx| {
            let mut planned: Planned = tx.read().get(PLANNED, run_id)?.unwrap_or_default();
            let (n, words, max) = match what {
                Allowance::File => (&mut planned.filed, "filed", MAX_PLANNED),
                Allowance::Start => (&mut planned.started, "started", MAX_PLANNED),
                Allowance::Ask => (&mut planned.asked, "asked", MAX_ASKED),
            };
            if *n >= max {
                return Err(error(
                    ErrorCode::Conflict,
                    format!(
                        "This run has {words} {max} already. Tell the person what's left, and they can ask again in a new message."
                    ),
                )
                .into());
            }
            *n += 1;
            tx.put(PLANNED, run_id, &planned)
        })
    }

    pub(crate) fn give_back_planned(&self, run_id: RunId, what: Allowance) {
        let _ = self.write(|tx| {
            let mut planned: Planned = tx.read().get(PLANNED, run_id)?.unwrap_or_default();
            match what {
                Allowance::File => planned.filed = planned.filed.saturating_sub(1),
                Allowance::Start => planned.started = planned.started.saturating_sub(1),
                Allowance::Ask => planned.asked = planned.asked.saturating_sub(1),
            }
            tx.put(PLANNED, run_id, &planned)
        });
    }

    /// Files `(title, body, distinct_from)`, an issue the person asked
    /// for, on the project's repository.
    async fn new_issue(
        &self,
        run_id: RunId,
        task: &Task,
        (title, body, distinct_from): (&str, &str, &[u64]),
        labels: &[String],
    ) -> Result<String, ProtocolError> {
        let settings = self.planning_settings(task)?;
        let gh = self.github()?;
        let (owner, name) = self.on_github(task.project_id).await?;
        let login = login(&*gh).await?;
        let repo = read_repo(&*gh, &owner, &name, &login).await?;
        self.write(|tx| tx.put(REPO, task.project_id, &repo))?;
        let p = Posting {
            gh,
            login,
            github: TaskGithub {
                link: GithubLink::NewPr,
                issue: None,
                pr: None,
                stage: None,
                found_by: None,
                checked_by: None,
                check_with: None,
                from_stranger: false,
                base: None,
                rounds: 0,
                repo: Some(repo_key(&repo.owner, &repo.name)),
                finding: None,
                already_fixed: false,
                approval: None,
                review: None,
            },
            repo,
            settings,
        };
        let title = checked_title(&self.redacted(run_id, title)?)?;
        let body = self.redacted(run_id, &without_disclosure(body))?;
        let full = format!(
            "{body}\n\n🤖 Filed by {} in Øphio, working for @{}",
            self.this_run(run_id, task)?,
            p.login
        );
        checked_body(&full)?;
        self.take_planned(run_id, Allowance::File)?;
        let read = Some(distinct_from);
        let filed =
            (self.file_issue(run_id, task, &p, &title, &full, false, read, &[AGENT_LABEL])).await;
        if !matches!(filed, Ok(Filed::New { .. })) {
            self.give_back_planned(run_id, Allowance::File);
        }
        let (number, mut answer) = match filed? {
            Filed::Duplicate(answer) => return Ok(answer),
            Filed::New { number, answer } => (number, answer),
        };
        if let Some(number) = number
            && !labels.is_empty()
        {
            let missing = self.existing_labels(&p, number, labels).await;
            if !missing.is_empty() {
                answer.push_str(&format!(
                    " {} has no label {}, so it wasn't added.",
                    p.slug(),
                    (missing.iter().map(|l| format!("“{l}”")))
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
        }
        answer.push_str(" Nothing has started on it: the person can start it from Tasks › GitHub › Issues, or ask you to.");
        Ok(answer)
    }

    /// Puts those of `names` the repository already has on issue `number`,
    /// and returns the others. Labels are a help, so failing fails nothing.
    async fn existing_labels(&self, p: &Posting, number: u64, names: &[String]) -> Vec<String> {
        let base = p.base();
        let (mut have, mut missing) = (Vec::new(), Vec::new());
        for name in names {
            let path = format!("{base}/labels/{}", segment(name));
            match p.gh.rest(Method::Get, &path, None).await {
                Ok(label) => have.push(text(&label["name"])),
                Err(e) if e.status() == Some(404) => missing.push(name.clone()),
                Err(_) => {}
            }
        }
        if !have.is_empty() {
            let path = format!("{base}/issues/{number}/labels");
            let _ = (p.gh)
                .rest(Method::Post, &path, Some(json!({ "labels": have })))
                .await;
        }
        missing
    }

    /// The agent, model and effort work starts with: as the agent chose,
    /// or the project's default agent, or the first that is ready. What it
    /// leaves out, Next up fills in with the project's defaults for its own
    /// agent, and the agent's own for another.
    pub(crate) fn chosen_agent(
        &self,
        task: &Task,
        choice: Option<AgentChoice>,
    ) -> Result<LaunchChoice, ProtocolError> {
        let (catalog, project, defaults) = self.read(|r| {
            Ok((
                r.get::<Vec<HarnessInfo>>("meta", "harnesses")?
                    .unwrap_or_default(),
                r.require::<Project>("project", task.project_id)?,
                Self::agent_preferences(r)?.launch_defaults,
            ))
        })?;
        let (harness, model, effort) = match choice {
            Some(c) => (c.agent, c.model, c.effort),
            None => {
                let ok = |kind: HarnessKind| {
                    (catalog.iter()).any(|h| h.kind == kind && ready(h).is_ok())
                };
                let kind = (project.default_harness.filter(|k| ok(*k)))
                    .or_else(|| catalog.iter().find(|h| ready(h).is_ok()).map(|h| h.kind))
                    .ok_or_else(|| {
                        error(
                            ErrorCode::TemporarilyUnavailable,
                            "No agent on this computer can start work now. ophio_agents says why.",
                        )
                    })?;
                (kind, None, None)
            }
        };
        let name = harness_name(harness);
        let info = (catalog.iter().find(|h| h.kind == harness)).ok_or_else(|| {
            error(
                ErrorCode::Unsupported,
                format!("{name} isn't on this computer. ophio_agents lists the agents there are."),
            )
        })?;
        ready(info).map_err(|why| {
            error(
                ErrorCode::TemporarilyUnavailable,
                format!("{name} is {why}"),
            )
        })?;
        if let Some(m) = &model
            && info.catalog.is_some()
            && !info.models.iter().any(|o| &o.id == m)
        {
            return Err(error(
                ErrorCode::InvalidRequest,
                format!("{name} has no model \"{m}\". ophio_agents lists its models."),
            ));
        }
        domain::model_choice(info, model.as_deref(), effort.as_deref())?;
        // The flags the person runs this agent with here, for the same agent,
        // or else the ones their phone starts it with.
        let launch_policy = (task.launch_policy.filter(|_| harness == task.harness))
            .or_else(|| {
                (defaults.iter())
                    .find(|d| d.harness == harness)
                    .map(|d| d.policy)
            })
            .filter(|p| p.supports(harness));
        Ok(LaunchChoice {
            harness,
            model,
            effort,
            launch_policy,
        })
    }

    /// Queues an agent on an open issue, with another to review its change
    /// when one is named, to start by itself once the project is free, or,
    /// when the person asked for that, to wait for them.
    async fn start_issue(
        &self,
        run_id: RunId,
        task: &Task,
        number: u64,
        (agent, reviewer): (Option<AgentChoice>, Option<AgentChoice>),
        start: bool,
        note: Option<&str>,
    ) -> Result<String, ProtocolError> {
        self.planning_settings(task)?;
        let (owner, name) = self.on_github(task.project_id).await?;
        let here = repo_key(&owner, &name);
        let working = self.read(|r| {
            let tasks = project_tasks(r, task.project_id)?;
            Ok(linked_task(&tasks, &here, number, false)
                .filter(|t| !matches!(t.state, TaskState::Completed | TaskState::Failed))
                .map(|t| t.title.clone()))
        })?;
        if let Some(title) = working {
            return Err(error(
                ErrorCode::Conflict,
                format!(
                    "“{title}” is already on #{number}. Leave it, or tell the person it's there."
                ),
            ));
        }
        let fix = self.chosen_agent(task, agent)?;
        let review = (reviewer.map(|r| self.chosen_agent(task, Some(r)))).transpose()?;
        let mut prompt = format!("Fix issue #{number}.");
        if let Some(note) = note {
            prompt.push_str(&format!("\n\n{}", self.redacted(run_id, note)?));
        }
        self.take_planned(run_id, Allowance::Start)?;
        let reviewed = (review.as_ref())
            .map(|r| format!(", reviewed by {}", via(r.harness, r.model.as_deref())))
            .unwrap_or_default();
        let queued = self
            .queue_issue(run_id, task, number, (&fix, review), prompt, start)
            .await;
        if queued.is_err() {
            self.give_back_planned(run_id, Allowance::Start);
        }
        let (title, model, resumed) = queued?;
        let who = format!("{}{reviewed}", via(fix.harness, model.as_deref()));
        Ok(match (start, resumed) {
            (true, true) => format!(
                "Queued “{title}” for {who} in Next up, and resumed Next up, which was paused. It starts by itself once the project is free."
            ),
            (true, false) => format!(
                "Queued “{title}” for {who} in Next up. It starts by itself once the project is free."
            ),
            (false, _) => format!(
                "Added “{title}” for {who} to Next up. It waits there until the person starts it."
            ),
        })
    }

    /// Adds the fix to Next up and, when it is to start by itself, lets it
    /// for a day and resumes a paused Next up. Its title, its model as Next
    /// up filled it in, and whether Next up was resumed.
    async fn queue_issue(
        &self,
        run_id: RunId,
        task: &Task,
        number: u64,
        (fix, review_with): (&LaunchChoice, Option<LaunchChoice>),
        prompt: String,
        start: bool,
    ) -> Result<(String, Option<String>, bool), ProtocolError> {
        let envelope = CommandEnvelope::new(Command::CreateTask {
            project_id: task.project_id,
            kind: TaskKind::Coding,
            mode: None,
            title: None,
            prompt,
            bundle: None,
            harness: fix.harness,
            model: fix.model.clone(),
            effort: fix.effort.clone(),
            launch_policy: fix.launch_policy,
            branch: None,
            start: None,
            place: None,
            browser: None,
            github: Some(Box::new(GithubLink::Issue {
                number,
                review_with,
            })),
            mission: None,
            target_id: None,
            placement: TaskPlacement::Queue {
                position: None,
                authorization: QueueAuthorization::AskBeforeStart,
            },
            agent_chosen: None,
            conversation: None,
            access: None,
            review: None,
        });
        // Adding to Next up asks again for everything waiting there. What
        // the person asked this run to start was let just before, unless
        // something else changed Next up meanwhile.
        let _adding = self.inner.planning.lock().await;
        let before = (self.read(|r| r.require::<QueueView>("queue", task.project_id)))?.revision;
        let receipt = self.admit(&as_creator(task), envelope).await;
        let (task_id, item_id) = match (receipt.error, receipt.result) {
            (
                None,
                Some(CommandResult::TaskCreated {
                    task_id,
                    queue_item_id: Some(item_id),
                }),
            ) => (task_id, item_id),
            (Some(e), _) => return Err(e),
            _ => return Err(error(ErrorCode::Internal, "The work wasn't queued")),
        };
        let resumed = self.let_start(run_id, task, (task_id, item_id), start, before)?;
        let fixer: Task = self.read(|r| r.require("task", task_id))?;
        self.progress_queues();
        Ok((fixer.title, fixer.model, resumed))
    }

    /// Once item `item_id` is in Next up, lets what the person asked this
    /// run to start start by itself, as the person who made `task`: when
    /// `start`, the item, for a day, resuming Next up if it was paused, and
    /// what it asked for before, which the addition asked again for, if
    /// nothing else changed Next up since revision `before`. Whether Next
    /// up was resumed.
    fn let_start(
        &self,
        run_id: RunId,
        task: &Task,
        (task_id, item_id): (TaskId, QueueItemId),
        start: bool,
        before: u64,
    ) -> Result<bool, ProtocolError> {
        let now = self.inner.clock.now();
        let end = Timestamp(now.0 + START_PERMIT_MS);
        self.write(|tx| {
            let mut planned: Planned = tx.read().get(PLANNED, run_id)?.unwrap_or_default();
            if !start && planned.starting.is_empty() {
                return Ok(false);
            }
            let mut q: QueueView = tx.read().require("queue", task.project_id)?;
            // Only this addition asked again for them.
            let ours = q.revision == before + 1;
            bump_queue_revision(tx, &mut q)?;
            let resumed = start && q.paused;
            if start {
                q.paused = false;
                q.pause_reason = None;
                planned.starting.push(item_id);
            }
            let pending: Vec<QueueItemId> = (q.items.iter())
                .filter(|i| i.is_pending())
                .map(|i| i.item_id)
                .collect();
            planned.starting.retain(|id| pending.contains(id));
            for item in (q.items.iter_mut()).filter(|i| planned.starting.contains(&i.item_id)) {
                let permit = match item.item_id == item_id {
                    true => {
                        let fixer: Task = tx.read().require("task", task_id)?;
                        Some(QueuePermit {
                            task_revision: fixer.revision,
                            queue_revision: q.revision,
                            remaining: 1,
                            principal: task.created_by,
                            expires_at: Some(end),
                        })
                    }
                    // The person deletes the permit of what they stop.
                    false if ours => {
                        let waiting: Task = tx.read().require("task", item.task_id)?;
                        (tx.read().get::<QueuePermit>("queue_permit", item.item_id)?)
                            .filter(|p| {
                                p.queue_revision == before
                                    && p.task_revision == waiting.revision
                                    && p.expires_at.is_some_and(|at| at > now)
                            })
                            .map(|p| QueuePermit {
                                queue_revision: q.revision,
                                ..p
                            })
                    }
                    false => None,
                };
                if let Some(permit) = permit {
                    item.authorization = QueueAuthorization::AutoStart;
                    item.authorization_expires_at = permit.expires_at;
                    tx.put("queue_permit", item.item_id, &permit)?;
                }
            }
            tx.put(PLANNED, run_id, &planned)?;
            self.refresh_queue(tx, &mut q)?;
            tx.put("queue", q.project_id, &q)?;
            sync_queue_attention(tx, &q, now)?;
            tx.emit(protocol::Event::QueueChanged { queue: q })?;
            Ok(resumed)
        })
    }

    /// Starts a review that files what it finds as issues, for the fixer
    /// to check and fix.
    async fn start_review(
        &self,
        run_id: RunId,
        task: &Task,
        (profile, folder, max_issues, then): (
            Option<FindFocus>,
            Option<String>,
            Option<u32>,
            Option<FixFindings>,
        ),
        reviewer: Option<AgentChoice>,
        fixer: Option<AgentChoice>,
    ) -> Result<String, ProtocolError> {
        if profile.is_some_and(FindFocus::uses_it) {
            return Err(error(ErrorCode::InvalidRequest, UAT_FROM_THE_PHONE));
        }
        let settings = self.planning_settings(task)?;
        self.on_github(task.project_id).await?;
        let review = self.chosen_agent(task, reviewer)?;
        let fix = self.chosen_agent(task, fixer)?;
        let (scope, place) = match folder {
            Some(path) => {
                let place = path.clone();
                (FindScope::Folder { path }, place)
            }
            None => (FindScope::Project, "the project".to_owned()),
        };
        let most = max_issues.unwrap_or(settings.workflow.findings_per_run);
        let mut after = then.unwrap_or(settings.workflow.fix_findings);
        let mut then = then;
        // Fixes that start by themselves would run past a chat's access.
        if crate::access::held(task).is_some() && after == FixFindings::ByItself {
            after = FixFindings::AskFirst;
            then = Some(after);
        }
        let prompt = match profile {
            None => format!("Find issues in {place}"),
            Some(f) => format!("Find {} issues in {place}", profile_words(f)),
        };
        let envelope = CommandEnvelope::new(Command::CreateTask {
            project_id: task.project_id,
            kind: TaskKind::Coding,
            mode: None,
            title: None,
            prompt,
            bundle: None,
            harness: review.harness,
            model: review.model.clone(),
            effort: review.effort.clone(),
            launch_policy: None,
            branch: None,
            start: None,
            place: None,
            browser: None,
            github: Some(Box::new(GithubLink::Find {
                scope,
                focus: profile,
                fix_with: fix.clone(),
                max_issues,
                fix_findings: then,
            })),
            mission: None,
            target_id: None,
            placement: TaskPlacement::StartNow,
            agent_chosen: None,
            conversation: None,
            // A review started by a restricted chat reads no more than it does.
            access: crate::access::held_to(crate::access::held(task), true),
            review: Some(protocol::review::ReviewRole::Find),
        });
        self.take_planned(run_id, Allowance::Start)?;
        let receipt = self.admit(&as_creator(task), envelope).await;
        let task_id = match (receipt.error, receipt.result) {
            (None, Some(CommandResult::TaskCreated { task_id, .. })) => task_id,
            (e, _) => {
                self.give_back_planned(run_id, Allowance::Start);
                return Err(
                    e.unwrap_or_else(|| error(ErrorCode::Internal, "The review didn't start"))
                );
            }
        };
        let finder: Task = self.read(|r| r.require("task", task_id))?;
        let fixer = match fix.harness == finder.harness && fix.model.is_none() {
            true => via(fix.harness, finder.model.as_deref()),
            false => via(fix.harness, fix.model.as_deref()),
        };
        let issues = match most {
            1 => "1 issue".to_owned(),
            n => format!("{n} issues"),
        };
        Ok(format!(
            "Started the review “{}” with {}, which reads and doesn't change the project. It files at most {issues}; then {}. The person can follow it in Tasks.",
            finder.title,
            via(finder.harness, finder.model.as_deref()),
            then_words(after, &fixer),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_profile_has_a_name_an_agent_can_pass() {
        let names: Vec<String> = PROFILES.iter().map(|p| profile_id(*p)).collect();
        assert_eq!(names[0], "default");
        assert!(names.contains(&"ui_ux".to_owned()));
        let mut unique = names.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), names.len());
        for name in &names[1..] {
            let back: FindFocus = serde_json::from_value(json!(name)).unwrap();
            assert_eq!(profile_id(Some(back)), *name);
        }
    }

    #[test]
    fn times_read_as_from_now() {
        assert_eq!(from_now(Timestamp(0), Timestamp(0)), "now");
        assert_eq!(
            from_now(Timestamp(0), Timestamp(45 * 60_000)),
            "in about 45 min"
        );
        assert_eq!(
            from_now(Timestamp(0), Timestamp(150 * 60_000)),
            "in about 3 h"
        );
    }
}
