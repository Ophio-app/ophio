// SPDX-License-Identifier: Apache-2.0
//! Ophio's agent browser and the task's logins, as a run's bridge reaches
//! them. A password or code goes from the logins straight into the page: the
//! agent hears only what was done, and stored secrets are hidden from all it
//! reads.
use crate::{
    Caller, CompanionHub, agent_logins::agent_login_error, error, hub::LoginAsk, unsupported,
};
use ports::agent_browser::{AgentBrowsing, AgentError, PageInfo, TaskBrowsing};
use ports::agent_logins::{Access, LoginUser, UsableLogin};
use ports::bridge::{BridgeResponse, BrowserAction};
use ports::harness::RunStart;
use ports::tool_approval::ToolApproval;
use protocol::{
    ApprovalId, CommandId, ErrorCode, ProtocolError, RunId, TaskId,
    accounts::Secret,
    agent_logins::{AgentLoginEvent, AgentLoginHistoryEntry},
    capability::HarnessKind,
    command::ApprovalDecision,
    event::Event,
    model::*,
};
use serde_json::{Value, json};
use std::{collections::HashMap, path::Path, sync::Arc};
use tokio::sync::{OwnedRwLockReadGuard, RwLock};
use zeroize::Zeroizing;

/// The folder in the task's own where its downloads land.
const DOWNLOADS: &str = "downloads";

/// Why an owner's answer about a login that changed while they were asked is
/// not used.
const CHANGED: &str = "changed while the owner was asked";

/// The task as its logins know it, and the name its history gives the agent.
struct Browsing {
    browser: Arc<dyn AgentBrowsing>,
    user: LoginUser,
    by: String,
}

/// Takes a run's login request out of the waiting ones however it ends, so
/// a code the owner typed isn't kept.
struct Asking<'a> {
    hub: &'a CompanionHub,
    id: ApprovalId,
}
impl Drop for Asking<'_> {
    fn drop(&mut self) {
        self.hub
            .inner
            .login_asks
            .lock()
            .expect("login asks poisoned")
            .remove(&self.id);
    }
}

impl CompanionHub {
    /// Whether a run of `task` gets the agent browser and the task's logins:
    /// conversations, and coding tasks doing Work, that kept the browser on.
    /// One held to its access browses only while that access reaches what
    /// the computer does; the bridge checks that as each page is asked for.
    pub(crate) fn browses(&self, task: &Task) -> bool {
        self.inner.agent_browser.is_some()
            && task.kind == TaskKind::Coding
            && (task.mode == TaskMode::Work || task.conversation)
            && task.browser
    }

    /// Makes `run_id` the run browsing for its task, and returns the one it
    /// replaces: a previous run that has not yet been let go.
    pub(crate) fn claim_browser(&self, task_id: TaskId, run_id: RunId) -> Option<RunId> {
        self.inner
            .browser_runs
            .lock()
            .expect("browser runs poisoned")
            .insert(task_id, run_id)
            .filter(|previous| *previous != run_id)
    }

    /// Lets the run's task browse in its own tabs, with downloads in its
    /// folder and the sites of logins it may not use closed to it. The
    /// `previous` run of the task loses its tabs, and what it was let use.
    pub(crate) async fn start_browsing(
        &self,
        run_id: RunId,
        task: &Task,
        project: &Project,
        previous: Option<RunId>,
    ) {
        let Some(browser) = self.inner.agent_browser.clone() else {
            return;
        };
        // Once the previous run's requests are done with the browser.
        let turns = self.browser_turns(task.task_id);
        let _handover = turns.write().await;
        // A bind that got here late, after its run ended or a later run took
        // the browser, leaves the tabs and what they're let reach as they are.
        if !self.browsing_now(run_id, task) {
            return;
        }
        let user = match self.login_user(task, run_id) {
            Ok((user, _)) => user,
            Err(_) => LoginUser {
                task_id: task.task_id,
                run_id,
                project_id: task.project_id,
                agent: None,
            },
        };
        if let Some(previous) = previous {
            if let Some(logins) = &self.inner.agent_logins {
                logins.run_ended(previous);
            }
            browser.close_task(task.task_id).await;
        }
        let logins = self.inner.agent_logins.clone();
        browser.register(
            task.task_id,
            TaskBrowsing {
                blocked: Arc::new(move || {
                    logins
                        .as_ref()
                        .map(|l| l.blocked_sites(&user))
                        .unwrap_or_default()
                }),
                downloads: Path::new(&project.host_path).join(DOWNLOADS),
            },
        );
    }

    /// The run has ended: what it was let use goes with it, and its task's
    /// tabs close, unless a later run of the task has them now.
    pub(crate) async fn end_browsing(&self, run_id: RunId) {
        let Some(browser) = self.inner.agent_browser.clone() else {
            return;
        };
        let Ok(run) = self.read(|r| r.require::<Run>("run", run_id)) else {
            return;
        };
        let browsing = |runs: &HashMap<TaskId, RunId>| runs.get(&run.task_id) == Some(&run_id);
        // A browsing run's tabs close once its requests are done with them.
        let turns = browsing(
            &self
                .inner
                .browser_runs
                .lock()
                .expect("browser runs poisoned"),
        )
        .then(|| self.browser_turns(run.task_id));
        let _handover = match &turns {
            Some(turns) => Some(turns.write().await),
            None => None,
        };
        let ours = {
            let mut runs = self
                .inner
                .browser_runs
                .lock()
                .expect("browser runs poisoned");
            let ours = browsing(&runs);
            if ours {
                runs.remove(&run.task_id);
            }
            if let Some(logins) = &self.inner.agent_logins {
                logins.run_ended(run_id);
            }
            ours
        };
        if ours {
            browser.close_task(run.task_id).await;
            if let Some(logins) = &self.inner.agent_logins {
                logins.task_ended(run.task_id);
            }
        }
    }

    /// Whether `run_id` is the run browsing for its task now.
    fn browsing_now(&self, run_id: RunId, task: &Task) -> bool {
        self.inner
            .browser_runs
            .lock()
            .expect("browser runs poisoned")
            .get(&task.task_id)
            == Some(&run_id)
    }

    /// Refuses a request of a run that is no longer browsing for its task.
    fn still_browsing(&self, run_id: RunId, task: &Task) -> Result<(), ProtocolError> {
        if self.browsing_now(run_id, task) {
            Ok(())
        } else {
            Err(crate::accounts::run_ended())
        }
    }

    fn browser_turns(&self, task_id: TaskId) -> Arc<RwLock<()>> {
        (self
            .inner
            .browser_turns
            .lock()
            .expect("browser turns poisoned"))
        .entry(task_id)
        .or_default()
        .clone()
    }

    /// A turn at the browser for a run still browsing for its task. Its task's
    /// tabs don't change hands until the turn is over, so nothing the run
    /// asked reaches a later run's tabs; a request lets go of its turn while
    /// it waits on the owner.
    async fn browser_turn(
        &self,
        run_id: RunId,
        task: &Task,
    ) -> Result<OwnedRwLockReadGuard<()>, ProtocolError> {
        let turn = self.browser_turns(task.task_id).read_owned().await;
        self.still_browsing(run_id, task)?;
        Ok(turn)
    }

    /// The task as its logins see it, and its agent's name.
    fn login_user(&self, task: &Task, run_id: RunId) -> Result<(LoginUser, String), ProtocolError> {
        let start = self.read(|r| r.get::<RunStart>("task_start", task.task_id))?;
        let (agent, by) = match start {
            Some(RunStart::Agent { name, file, .. }) => (
                file.file_stem().map(|s| s.to_string_lossy().into_owned()),
                name,
            ),
            _ => (None, harness_name(task.harness).into()),
        };
        Ok((
            LoginUser {
                task_id: task.task_id,
                run_id,
                project_id: task.project_id,
                agent,
            },
            by,
        ))
    }

    /// Does what the run's agent asked of its tabs or the task's logins.
    pub(crate) async fn browser_request(
        &self,
        run_id: RunId,
        task: &Task,
        action: BrowserAction,
    ) -> Result<BridgeResponse, ProtocolError> {
        let browser = self
            .inner
            .agent_browser
            .clone()
            .ok_or_else(|| unsupported("The agent browser"))?;
        let turn = self.browser_turn(run_id, task).await?;
        self.settle_sign_outs(&*browser).await?;
        // The run may have been replaced while the browser was signed out.
        self.still_browsing(run_id, task)?;
        let (user, by) = self.login_user(task, run_id)?;
        let b = Browsing { browser, user, by };
        let id = task.task_id;
        let failed = |e: AgentError| self.browser_error(run_id, e);
        let page = |p: PageInfo| json!(p);
        let mut image = None;
        let mut shown = match action {
            BrowserAction::Open { url, new_tab } => {
                page(b.browser.open(id, &url, new_tab).await.map_err(failed)?)
            }
            BrowserAction::Back => page(b.browser.back(id).await.map_err(failed)?),
            BrowserAction::Tabs => json!(b.browser.tabs(id).await.map_err(failed)?),
            BrowserAction::SwitchTab { tab } => {
                page(b.browser.switch_tab(id, &tab).await.map_err(failed)?)
            }
            BrowserAction::CloseTab { tab } => {
                b.browser
                    .close_tab(id, tab.as_deref())
                    .await
                    .map_err(failed)?;
                json!({ "result": "closed" })
            }
            BrowserAction::Observe => {
                let mut seen = b.browser.observe(id).await.map_err(failed)?;
                // Empty when the browser withheld it: the page holds something
                // Ophio typed into it.
                image = Some(std::mem::take(&mut seen.screenshot)).filter(|s| !s.is_empty());
                self.masked_observation(run_id, &mut seen)?;
                let mut shown = json!(seen);
                if let Some(o) = shown.as_object_mut() {
                    o.remove("screenshot");
                    if !seen.downloads.is_empty() {
                        let folder = self.downloads_folder(task)?;
                        o.insert(
                            "downloaded_to".into(),
                            folder.to_string_lossy().into_owned().into(),
                        );
                    }
                }
                shown
            }
            BrowserAction::Click { control } => {
                page(b.browser.click(id, &control).await.map_err(failed)?)
            }
            BrowserAction::Type {
                control,
                text,
                replace,
            } => {
                b.browser
                    .type_text(id, &control, &text, replace)
                    .await
                    .map_err(failed)?;
                json!({ "result": "typed" })
            }
            BrowserAction::Press { key } => page(b.browser.press(id, &key).await.map_err(failed)?),
            BrowserAction::Scroll { dy, control } => {
                b.browser
                    .scroll(id, dy, control.as_deref())
                    .await
                    .map_err(failed)?;
                json!({ "result": "scrolled" })
            }
            BrowserAction::ListLogins => self.list_logins(&b)?,
            // These may wait on the owner, so take their own turns.
            BrowserAction::FillLogin { login, submit } => {
                drop(turn);
                self.fill_login(run_id, task, &b, &login, submit).await?
            }
            BrowserAction::FillCode { login, submit } => {
                drop(turn);
                self.fill_code(run_id, task, &b, &login, submit).await?
            }
            BrowserAction::AskOwnerForCode { login, why, submit } => {
                drop(turn);
                self.ask_owner_for_code(run_id, task, &b, &login, &why, submit)
                    .await?
            }
        };
        self.masked_result(run_id, &mut shown)?;
        Ok(BridgeResponse::BrowserResult {
            text: shown.to_string(),
            image_base64: image,
        })
    }

    /// Signs the browser out of sites a login left while Ophio had no
    /// browser, before anything browses: the login no longer keeps tasks
    /// from them.
    async fn settle_sign_outs(&self, browser: &dyn AgentBrowsing) -> Result<(), ProtocolError> {
        let Some(logins) = &self.inner.agent_logins else {
            return Ok(());
        };
        let owed = logins.owed_sign_outs();
        if owed.is_empty() {
            return Ok(());
        }
        browser.sign_out(&owed).await.map_err(|_| {
            error(
                ErrorCode::TemporarilyUnavailable,
                "The agent browser has to be signed out of sites first, and couldn't be; \
                 try again shortly",
            )
        })?;
        logins.settled_sign_outs(&owed);
        Ok(())
    }

    fn browser_error(&self, run_id: RunId, e: AgentError) -> ProtocolError {
        match e {
            AgentError::Refused(m) => {
                error(ErrorCode::PermissionDenied, self.masked_message(run_id, &m))
            }
            AgentError::Failed(m) => error(
                ErrorCode::TemporarilyUnavailable,
                self.masked_message(run_id, &m),
            ),
        }
    }

    fn downloads_folder(&self, task: &Task) -> Result<std::path::PathBuf, ProtocolError> {
        let project: Project = self.read(|r| r.require("project", task.project_id))?;
        Ok(Path::new(&project.host_path).join(DOWNLOADS))
    }

    fn list_logins(&self, b: &Browsing) -> Result<Value, ProtocolError> {
        let logins = self.agent_logins()?;
        let usable = logins.usable(&b.user);
        if usable.is_empty() {
            return Ok(json!({ "logins": [], "note": "This task may use no logins." }));
        }
        Ok(json!({
            "logins": usable.iter().map(|l| json!({
                "login": l.login_id,
                "name": l.name,
                "sites": l.sites,
                "username": l.username,
                "makes_codes": l.has_code_key,
                "notes": l.notes,
                "ask_first": l.ask_first,
            })).collect::<Vec<_>>(),
            "note": "Open the login's sign-in page, then fill_login. A login marked ask_first \
                     asks the owner on its first use, and its sites open once they allow it.",
        }))
    }

    /// The login, once the task may use it: at once, or when the owner
    /// allows it for this task.
    async fn usable_login(
        &self,
        run_id: RunId,
        task: &Task,
        b: &Browsing,
        login_id: &str,
    ) -> Result<UsableLogin, ProtocolError> {
        let logins = self.agent_logins()?;
        let find = || {
            logins
                .usable(&b.user)
                .into_iter()
                .find(|l| l.login_id == login_id)
        };
        match logins.access(login_id, &b.user) {
            Access::Allowed => {}
            Access::Refused(why) => {
                self.login_history(b, login_id, None, AgentLoginEvent::Refused, None, &why);
                return Err(error(ErrorCode::PermissionDenied, why));
            }
            Access::AskFirst => {
                let login =
                    find().ok_or_else(|| error(ErrorCode::NotFound, "There is no such login."))?;
                let site = login.sites.first().cloned().unwrap_or_default();
                let id = ApprovalId::new();
                let _asking = self.asking(id, &login, b);
                let decision = self
                    .bridge_wait_as(
                        id,
                        run_id,
                        task,
                        ToolApproval {
                            kind: ApprovalKind::LoginUse,
                            title: format!(
                                "Let “{}” sign in to {} on {site}?",
                                task.title, login.name
                            ),
                            detail: Some(format!(
                                "{} wants to use the login {} on {}. If you allow it, Ophio \
                                 fills it for this task until the task's turn ends; the agent \
                                 never sees the password.",
                                b.by,
                                login.name,
                                login.sites.join(", ")
                            )),
                            detail_lines: Vec::new(),
                            questions: Vec::new(),
                            scope: "This task".into(),
                            shortened: false,
                        },
                        None,
                        None,
                        None,
                    )
                    .await;
                match decision {
                    // Allowed only if the login wasn't changed while the
                    // owner was asked about it.
                    Ok(ApprovalDecision::Allow)
                        if logins.access(login_id, &b.user) == Access::Allowed => {}
                    Ok(ApprovalDecision::Allow) => {
                        return Err(error(
                            ErrorCode::PermissionDenied,
                            format!("{} {CHANGED}; ask again.", login.name),
                        ));
                    }
                    Ok(_) => {
                        return Err(error(
                            ErrorCode::PermissionDenied,
                            format!("The owner didn't let this task use {}.", login.name),
                        ));
                    }
                    Err(e) => {
                        self.unanswered(b, &login, &e);
                        return Err(e);
                    }
                }
            }
        }
        find().ok_or_else(|| error(ErrorCode::NotFound, "There is no such login."))
    }

    async fn fill_login(
        &self,
        run_id: RunId,
        task: &Task,
        b: &Browsing,
        login_id: &str,
        submit: bool,
    ) -> Result<Value, ProtocolError> {
        let logins = self.agent_logins()?;
        let login = self.usable_login(run_id, task, b, login_id).await?;
        let _turn = self.browser_turn(run_id, task).await?;
        let site = self.site(b, task).await;
        let credentials = logins
            .credentials(login_id, &b.user)
            .await
            .map_err(agent_login_error)?;
        let filled = b
            .browser
            .fill_login(
                task.task_id,
                &credentials.sites,
                &credentials.username,
                &credentials.password,
                submit,
            )
            .await;
        drop(credentials);
        self.filled(
            run_id,
            task,
            b,
            &login,
            site,
            filled,
            AgentLoginEvent::Filled,
            &b.by,
        )
        .await
    }

    async fn fill_code(
        &self,
        run_id: RunId,
        task: &Task,
        b: &Browsing,
        login_id: &str,
        submit: bool,
    ) -> Result<Value, ProtocolError> {
        let logins = self.agent_logins()?;
        let login = self.usable_login(run_id, task, b, login_id).await?;
        let _turn = self.browser_turn(run_id, task).await?;
        let site = self.site(b, task).await;
        let code = logins
            .code(login_id, &b.user)
            .await
            .map_err(agent_login_error)?;
        self.keep_code_hidden(run_id, &code)?;
        let filled = b
            .browser
            .fill_code(task.task_id, &login.sites, &code, submit, None)
            .await;
        drop(code);
        self.filled(
            run_id,
            task,
            b,
            &login,
            site,
            filled,
            AgentLoginEvent::CodeFilled,
            &b.by,
        )
        .await
    }

    async fn ask_owner_for_code(
        &self,
        run_id: RunId,
        task: &Task,
        b: &Browsing,
        login_id: &str,
        why: &str,
        submit: bool,
    ) -> Result<Value, ProtocolError> {
        let login = self.usable_login(run_id, task, b, login_id).await?;
        let turn = self.browser_turn(run_id, task).await?;
        let site = self.site(b, task).await;
        // The code goes only into the page it was asked for.
        let at = b
            .browser
            .mark(task.task_id)
            .await
            .map_err(|e| self.browser_error(run_id, e))?;
        drop(turn);
        let id = ApprovalId::new();
        let _asking = self.asking(id, &login, b);
        self.login_history(
            b,
            login_id,
            Some(&login.name),
            AgentLoginEvent::CodeAsked,
            site.clone(),
            why,
        );
        let decision = self
            .bridge_wait_as(
                id,
                run_id,
                task,
                ToolApproval {
                    kind: ApprovalKind::LoginCode,
                    title: format!("{}: {why}", login.name),
                    detail: Some(format!(
                        "“{}” is signing in to {} and needs the code the site sent you. Type it \
                         here: Ophio fills it in, and the agent never sees it.",
                        task.title, login.name
                    )),
                    detail_lines: Vec::new(),
                    questions: Vec::new(),
                    scope: "This code once".into(),
                    shortened: false,
                },
                None,
                None,
                None,
            )
            .await
            .inspect_err(|e| self.unanswered(b, &login, e))?;
        let given = self
            .inner
            .login_asks
            .lock()
            .expect("login asks poisoned")
            .get_mut(&id)
            .and_then(|ask| ask.code.take());
        let (code, by) = match (decision, given) {
            (ApprovalDecision::Allow, Some(given)) => given,
            _ => {
                return Err(error(
                    ErrorCode::PermissionDenied,
                    "The owner didn't give a code.",
                ));
            }
        };
        let _turn = self.browser_turn(run_id, task).await?;
        // The login may have been paused or changed while the owner was asked.
        let now =
            self.agent_logins()?.usable(&b.user).into_iter().find(|l| {
                l.login_id == login.login_id && l.revision == login.revision && !l.ask_first
            });
        let Some(login) = now else {
            self.login_history(
                b,
                login_id,
                Some(&login.name),
                AgentLoginEvent::Refused,
                site,
                &format!("It {CHANGED}"),
            );
            return Err(error(
                ErrorCode::PermissionDenied,
                format!("{} {CHANGED}, so the code wasn't typed.", login.name),
            ));
        };
        self.keep_code_hidden(run_id, &code)?;
        let filled = b
            .browser
            .fill_code(task.task_id, &login.sites, &code, submit, Some(&at))
            .await;
        drop(code);
        self.filled(
            run_id,
            task,
            b,
            &login,
            site,
            filled,
            AgentLoginEvent::CodeGiven,
            &by,
        )
        .await
    }

    /// Records a fill, or its refusal, and tells the agent.
    #[allow(clippy::too_many_arguments)]
    async fn filled(
        &self,
        run_id: RunId,
        task: &Task,
        b: &Browsing,
        login: &UsableLogin,
        site: Option<String>,
        filled: Result<PageInfo, AgentError>,
        event: AgentLoginEvent,
        by: &str,
    ) -> Result<Value, ProtocolError> {
        let page = match filled {
            Ok(page) => page,
            Err(e) => {
                if let AgentError::Refused(why) = &e {
                    self.login_history(
                        b,
                        &login.login_id,
                        Some(&login.name),
                        AgentLoginEvent::Refused,
                        site,
                        why,
                    );
                }
                return Err(self.browser_error(run_id, e));
            }
        };
        if let Ok(logins) = self.agent_logins() {
            logins.record(AgentLoginHistoryEntry {
                at: self.inner.clock.now(),
                login_id: Some(login.login_id.clone()),
                name: Some(login.name.clone()),
                event,
                by: by.to_owned(),
                task_id: Some(task.task_id),
                site: site.clone(),
                detail: None,
            });
            let notify = logins
                .view()
                .await
                .logins
                .iter()
                .any(|l| l.login_id == login.login_id && l.notify);
            let _ = self.write(|tx| {
                tx.emit(Event::AgentLoginUsed {
                    login_id: login.login_id.clone(),
                    name: login.name.clone(),
                    task_id: task.task_id,
                    site: site.clone().unwrap_or_default(),
                    notify,
                })?;
                Ok(())
            });
        }
        Ok(json!({ "result": "filled", "tab": page.tab, "url": page.url, "title": page.title }))
    }

    /// The host of the task's current page, for the history.
    async fn site(&self, b: &Browsing, task: &Task) -> Option<String> {
        let page = b.browser.page(task.task_id).await.ok()?;
        url::Url::parse(&page.url)
            .ok()?
            .host_str()
            .map(str::to_owned)
    }

    /// Records a request the owner let lapse.
    /// Adds a code to what the run's output is masked for, before it is
    /// typed: a page can show it back.
    fn keep_code_hidden(&self, run_id: RunId, code: &str) -> Result<(), ProtocolError> {
        if self.add_run_code(run_id, code) {
            return Ok(());
        }
        Err(error(
            ErrorCode::TemporarilyUnavailable,
            "This run can't keep another code hidden, so none was typed.",
        ))
    }

    fn unanswered(&self, b: &Browsing, login: &UsableLogin, e: &ProtocolError) {
        if e.code == ErrorCode::Expired {
            self.login_history(
                b,
                &login.login_id,
                Some(&login.name),
                AgentLoginEvent::Refused,
                None,
                "The owner didn't answer.",
            );
        }
    }

    fn asking(&self, id: ApprovalId, login: &UsableLogin, b: &Browsing) -> Asking<'_> {
        self.inner
            .login_asks
            .lock()
            .expect("login asks poisoned")
            .insert(
                id,
                LoginAsk {
                    login_id: login.login_id.clone(),
                    task_id: b.user.task_id,
                    run_id: b.user.run_id,
                    revision: login.revision,
                    code: None,
                },
            );
        Asking { hub: self, id }
    }

    fn login_history(
        &self,
        b: &Browsing,
        login_id: &str,
        name: Option<&str>,
        event: AgentLoginEvent,
        site: Option<String>,
        detail: &str,
    ) {
        if let Some(logins) = &self.inner.agent_logins {
            logins.record(AgentLoginHistoryEntry {
                at: self.inner.clock.now(),
                login_id: Some(login_id.to_owned()),
                name: name.map(str::to_owned),
                event,
                by: b.by.clone(),
                task_id: Some(b.user.task_id),
                site,
                detail: Some(detail.to_owned()),
            });
        }
    }

    /// The owner decided a run's login request: an allowed login is the
    /// run's to use before it hears of it, unless it has ended meanwhile.
    pub(crate) fn login_decided(
        &self,
        caller: &Caller,
        approval: &Approval,
        decision: &ApprovalDecision,
    ) {
        let Some(logins) = &self.inner.agent_logins else {
            return;
        };
        let Some((login_id, task_id, run_id, revision)) = self
            .inner
            .login_asks
            .lock()
            .expect("login asks poisoned")
            .get(&approval.approval_id)
            .map(|ask| (ask.login_id.clone(), ask.task_id, ask.run_id, ask.revision))
        else {
            return;
        };
        let by = self
            .caller_name(caller)
            .unwrap_or_else(|_| "The owner".into());
        let (event, detail) = match (approval.kind, decision) {
            (ApprovalKind::LoginUse, ApprovalDecision::Allow) => {
                // Held while approving, so a run that ends meanwhile leaves
                // nothing approved behind.
                let runs = self
                    .inner
                    .browser_runs
                    .lock()
                    .expect("browser runs poisoned");
                if runs.get(&task_id) != Some(&run_id) {
                    (
                        AgentLoginEvent::Refused,
                        Some("The run had ended".to_owned()),
                    )
                } else if logins.approve(&login_id, run_id, revision) {
                    (AgentLoginEvent::Approved, None)
                } else {
                    (AgentLoginEvent::Refused, Some(format!("It {CHANGED}")))
                }
            }
            (ApprovalKind::LoginUse, ApprovalDecision::Deny { .. }) => (
                AgentLoginEvent::Refused,
                Some("Not allowed for this task".to_owned()),
            ),
            (ApprovalKind::LoginCode, ApprovalDecision::Deny { .. }) => {
                (AgentLoginEvent::Refused, Some("No code given".to_owned()))
            }
            _ => return,
        };
        logins.record(AgentLoginHistoryEntry {
            at: self.inner.clock.now(),
            login_id: Some(login_id),
            name: None,
            event,
            by,
            task_id: Some(task_id),
            site: None,
            detail,
        });
    }

    /// The code a site sent the owner, for the run that asked for it.
    pub(crate) async fn answer_login_code(
        &self,
        caller: &Caller,
        command_id: CommandId,
        approval_id: ApprovalId,
        code: &Secret,
    ) -> Result<(), ProtocolError> {
        let code: Zeroizing<String> = Zeroizing::new(
            code.expose()
                .chars()
                .filter(|c| !matches!(c, ' ' | '-'))
                .collect(),
        );
        if !(3..=12).contains(&code.len()) || !code.chars().all(|c| c.is_ascii_alphanumeric()) {
            return Err(error(
                ErrorCode::InvalidRequest,
                "A code is 3 to 12 letters and digits.",
            ));
        }
        let now = self.inner.clock.now();
        self.write(|tx| {
            let a: Approval = tx.read().require("approval", approval_id)?;
            if a.kind != ApprovalKind::LoginCode {
                return Err(error(ErrorCode::NotFound, "There is no such code request.").into());
            }
            if a.state != ApprovalState::Pending || a.expires_at.is_some_and(|t| t <= now) {
                return Err(error(ErrorCode::Expired, "The code request has ended.").into());
            }
            if tx
                .read()
                .get::<CommandId>("approval_claim", approval_id)?
                .is_some()
            {
                return Err(error(
                    ErrorCode::AlreadyResolved,
                    "Approval decision already admitted",
                )
                .into());
            }
            tx.put("approval_claim", approval_id, &command_id)?;
            Ok(())
        })?;
        let by = self.caller_name(caller)?;
        {
            let mut asks = self.inner.login_asks.lock().expect("login asks poisoned");
            let Some(ask) = asks.get_mut(&approval_id) else {
                drop(asks);
                let _ = self.write(|tx| {
                    tx.delete("approval_claim", approval_id)?;
                    Ok(())
                });
                return Err(error(ErrorCode::Expired, "The code request has ended."));
            };
            ask.code = Some((code, by));
        }
        let resolved = self
            .resolve_approval(caller, approval_id, &ApprovalDecision::Allow)
            .await;
        if resolved.is_err()
            && let Some(ask) = self
                .inner
                .login_asks
                .lock()
                .expect("login asks poisoned")
                .get_mut(&approval_id)
        {
            ask.code = None;
        }
        resolved
    }
}

pub(crate) fn harness_name(harness: HarnessKind) -> &'static str {
    match harness {
        HarnessKind::ClaudeCode => "Claude Code",
        HarnessKind::Codex => "Codex",
        HarnessKind::Hermes => "Hermes",
        HarnessKind::OpenClaw => "OpenClaw",
        HarnessKind::Pi => "Pi",
    }
}
