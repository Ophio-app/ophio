// SPDX-License-Identifier: Apache-2.0
//! A run's agent browser and logins, through its bridge, with a stand-in for
//! the browser.
use super::*;
use ports::agent_browser::{
    AgentBrowsing, AgentError, Control, NAME_SHOWN, Observation, PageInfo, PageMark, TabInfo,
    TaskBrowsing,
};
use ports::agent_logins::AgentLogins as _;
use ports::bridge::{BridgeBinding, BridgeHandler, BridgeRequest, BridgeResponse, BrowserAction};
use protocol::agent_logins::{LoginDetails, LoginHow};
use protocol::ids::TargetId;
use std::{collections::HashMap, sync::Mutex};
use tokio::sync::oneshot;

/// Keeps what it was given, so a test can see what reached the page.
#[derive(Debug, Default)]
struct FakeBrowser {
    browsing: Mutex<HashMap<TaskId, TaskBrowsing>>,
    /// The tasks given tabs, each time.
    registered: Mutex<Vec<TaskId>>,
    closed: Mutex<Vec<TaskId>>,
    url: Mutex<String>,
    /// Usernames and passwords put into a page.
    filled: Mutex<Vec<(String, String)>>,
    codes: Mutex<Vec<String>>,
    /// The page the tab shows; a test changes it to navigate.
    document: Mutex<String>,
    /// The sites signed out of, each time.
    signed_out: Mutex<Vec<Vec<String>>>,
    /// Signing out fails while set.
    cant_sign_out: Mutex<bool>,
    /// Signing out says it has started, then waits to be let go, once set.
    held_sign_out: Mutex<Option<(oneshot::Sender<()>, oneshot::Receiver<()>)>>,
}
impl FakeBrowser {
    fn page(&self) -> PageInfo {
        PageInfo {
            tab: "t1".into(),
            url: self.url.lock().unwrap().clone(),
            title: "Sign in".into(),
        }
    }
    fn blocked(&self, task: TaskId) -> Vec<String> {
        (self.browsing.lock().unwrap()[&task].blocked)()
    }
}
#[async_trait::async_trait]
impl AgentBrowsing for FakeBrowser {
    fn register(&self, task: TaskId, browsing: TaskBrowsing) {
        self.registered.lock().unwrap().push(task);
        self.browsing.lock().unwrap().insert(task, browsing);
    }
    async fn close_task(&self, task: TaskId) {
        self.closed.lock().unwrap().push(task);
    }
    async fn open(&self, task: TaskId, url: &str, _: bool) -> Result<PageInfo, AgentError> {
        let host = url::Url::parse(url).unwrap().host_str().unwrap().to_owned();
        if self.blocked(task).contains(&host) {
            return Err(AgentError::Refused(format!(
                "{host} is closed to this task"
            )));
        }
        *self.url.lock().unwrap() = url.into();
        Ok(self.page())
    }
    async fn tabs(&self, _: TaskId) -> Result<Vec<TabInfo>, AgentError> {
        Ok(Vec::new())
    }
    async fn switch_tab(&self, _: TaskId, _: &str) -> Result<PageInfo, AgentError> {
        Ok(self.page())
    }
    async fn close_tab(&self, _: TaskId, _: Option<&str>) -> Result<(), AgentError> {
        Ok(())
    }
    async fn back(&self, _: TaskId) -> Result<PageInfo, AgentError> {
        Ok(self.page())
    }
    async fn page(&self, _: TaskId) -> Result<PageInfo, AgentError> {
        Ok(self.page())
    }
    async fn observe(&self, _: TaskId) -> Result<Observation, AgentError> {
        // A page that shows back what was typed into it.
        let shown = self
            .filled
            .lock()
            .unwrap()
            .last()
            .cloned()
            .unwrap_or_default();
        let code = self
            .codes
            .lock()
            .unwrap()
            .last()
            .cloned()
            .unwrap_or_default();
        // A long label ending in the password, which showing only its start
        // would cut three characters in.
        let long = format!("{}  {}", "a".repeat(NAME_SHOWN - 4), shown.1);
        Ok(Observation {
            tab: "t1".into(),
            url: self.url.lock().unwrap().clone(),
            title: format!(
                "Welcome {}, your password is {}; code {code}",
                shown.0, shown.1
            ),
            screenshot: "c2NyZWVu".into(),
            screenshot_scale: 1.0,
            controls: vec![Control {
                reference: "e1".into(),
                role: "button".into(),
                name: long,
                value: None,
                bounds: [0; 4],
                disabled: false,
                focused: false,
                checked: None,
            }],
            downloads: vec!["report.pdf".into()],
            notice: None,
        })
    }
    async fn click(&self, _: TaskId, _: &str) -> Result<PageInfo, AgentError> {
        Ok(self.page())
    }
    async fn type_text(&self, _: TaskId, _: &str, _: &str, _: bool) -> Result<(), AgentError> {
        Ok(())
    }
    async fn press(&self, _: TaskId, _: &str) -> Result<PageInfo, AgentError> {
        Ok(self.page())
    }
    async fn scroll(&self, _: TaskId, _: f64, _: Option<&str>) -> Result<(), AgentError> {
        Ok(())
    }
    async fn fill_login(
        &self,
        _: TaskId,
        sites: &[String],
        username: &str,
        password: &str,
        _: bool,
    ) -> Result<PageInfo, AgentError> {
        let page = self.page();
        let host = (url::Url::parse(&page.url).ok())
            .and_then(|u| u.host_str().map(str::to_owned))
            .unwrap_or_default();
        if !sites.contains(&host) {
            return Err(AgentError::Refused(format!("This login isn't for {host}")));
        }
        (self.filled.lock().unwrap()).push((username.into(), password.into()));
        Ok(page)
    }
    async fn mark(&self, _: TaskId) -> Result<PageMark, AgentError> {
        Ok(PageMark {
            tab: "t1".into(),
            document: self.document.lock().unwrap().clone(),
            url: self.url.lock().unwrap().clone(),
        })
    }
    async fn fill_code(
        &self,
        task: TaskId,
        _: &[String],
        code: &str,
        _: bool,
        at: Option<&PageMark>,
    ) -> Result<PageInfo, AgentError> {
        if let Some(at) = at
            && *at != self.mark(task).await?
        {
            return Err(AgentError::Refused("The page changed".into()));
        }
        self.codes.lock().unwrap().push(code.into());
        // A page that says the code back.
        Ok(PageInfo {
            title: format!("Invalid code {code}"),
            ..self.page()
        })
    }
    async fn sign_out(&self, sites: &[String]) -> Result<(), AgentError> {
        let held = self.held_sign_out.lock().unwrap().take();
        if let Some((started, let_go)) = held {
            started.send(()).unwrap();
            let_go.await.unwrap();
        }
        if *self.cant_sign_out.lock().unwrap() {
            return Err(AgentError::Failed("The browser is busy".into()));
        }
        self.signed_out.lock().unwrap().push(sites.to_vec());
        Ok(())
    }
    fn set_taken_over(&self, _: TargetId, _: bool) -> bool {
        false
    }
}

struct Browsing {
    l: Logins,
    browser: Arc<FakeBrowser>,
}
async fn browsing(how: LoginHow) -> (Browsing, String) {
    let dir = tempfile::tempdir().unwrap();
    let logins = ComputerAgentLogins::new(LoginsConfig::new(
        dir.path().join("accounts"),
        Arc::new(MemoryVault::default()),
        None,
    ));
    let given = Arc::new(logins.clone());
    let browser = Arc::new(FakeBrowser::default());
    let used = browser.clone();
    let f = Fixture::with(move |b| b.agent_logins(given).agent_browser(used)).await;
    let owner = Caller::local_owner();
    let accept = AgentLoginAction::Accept {
        version: DISCLAIMER_VERSION,
    };
    assert!(done(&f, &owner, login(accept)).await.is_none());
    let AgentLoginAction::Add {
        details,
        password,
        code_key,
    } = added()
    else {
        unreachable!()
    };
    let add = AgentLoginAction::Add {
        details: LoginDetails { how, ..details },
        password,
        code_key,
    };
    assert!(done(&f, &owner, login(add)).await.is_none());
    let login_id = view(&f, &owner).await.logins[0].login_id.clone();
    let l = Logins { f, dir, logins };
    (Browsing { l, browser }, login_id)
}
/// Starts a task and connects its run's bridge.
async fn bound(f: &Fixture, browser: Option<bool>) -> (TaskId, BridgeBinding) {
    let project = f.project().await;
    let r = f
        .hub
        .submit(
            &Caller::local_owner(),
            CommandEnvelope::new(Command::CreateTask {
                github: None,
                agent_chosen: None,
                mode: None,
                project_id: project,
                kind: TaskKind::Coding,
                title: Some("Post the notes".into()),
                prompt: "test".into(),
                bundle: None,
                harness: HarnessKind::ClaudeCode,
                model: None,
                effort: None,
                launch_policy: None,
                branch: None,
                mission: None,
                target_id: None,
                placement: TaskPlacement::StartNow,
                start: None,
                place: None,
                browser,
                conversation: None,
                access: None,
                review: None,
            }),
        )
        .await;
    let Some(CommandResult::TaskCreated { task_id, .. }) = r.result else {
        panic!("{r:?}")
    };
    run_started(f, task_id).await;
    let bridge = (f.harness.specs.lock().unwrap().last())
        .and_then(|spec| spec.bridge.clone())
        .expect("a bridge");
    let token = (bridge.env.iter())
        .find(|(key, _)| key == ports::bridge::RUN_TOKEN_ENV)
        .map(|(_, token)| token.clone())
        .unwrap();
    let binding = f.hub.bind(&token).await.expect("bridge binding");
    assert_eq!(bridge.browser, binding.browser);
    (task_id, binding)
}
async fn ask(
    f: &Fixture,
    binding: &BridgeBinding,
    action: BrowserAction,
) -> Result<String, String> {
    match f
        .hub
        .handle(binding, BridgeRequest::Browser { action })
        .await
    {
        BridgeResponse::BrowserResult { text, .. } => Ok(text),
        BridgeResponse::Refused { reason } => Err(reason),
        other => panic!("{other:?}"),
    }
}
/// Asks in the background, for an answer that waits on the owner.
fn asked(
    f: &Fixture,
    binding: &BridgeBinding,
    action: BrowserAction,
) -> tokio::task::JoinHandle<BridgeResponse> {
    let hub = f.hub.clone();
    let binding = binding.clone();
    tokio::spawn(async move {
        hub.handle(&binding, BridgeRequest::Browser { action })
            .await
    })
}
async fn pending(f: &Fixture, kind: ApprovalKind) -> Approval {
    let mut more = waiting();
    loop {
        let found = (f
            .hub
            .read(|r| r.list::<Approval>("approval"))
            .unwrap()
            .into_iter())
        .find(|a| a.kind == kind && a.state == ApprovalState::Pending);
        if let Some(found) = found {
            return found;
        }
        assert!(more(), "no {kind:?} approval");
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
}
fn result(response: BridgeResponse) -> Result<String, String> {
    match response {
        BridgeResponse::BrowserResult { text, .. } => Ok(text),
        BridgeResponse::Refused { reason } => Err(reason),
        other => panic!("{other:?}"),
    }
}
fn open(url: &str) -> BrowserAction {
    BrowserAction::Open {
        url: url.into(),
        new_tab: false,
    }
}

#[tokio::test]
async fn a_browsing_run_fills_a_login_it_never_sees() {
    let (Browsing { l, browser }, login_id) = browsing(LoginHow::Whenever).await;
    let f = &l.f;
    let (task, binding) = bound(f, None).await;
    assert!(binding.browser);
    let downloads = browser.browsing.lock().unwrap()[&task].downloads.clone();
    let project = std::fs::canonicalize(f.project_dir()).unwrap();
    assert_eq!(downloads, project.join("downloads"));
    let listed = ask(f, &binding, BrowserAction::ListLogins).await.unwrap();
    assert!(listed.contains(&login_id) && listed.contains("agent@example.com"));
    assert!(!listed.contains("pässword"), "{listed}");
    // Not on the login's site, so nothing is filled.
    ask(f, &binding, open("https://elsewhere.example.net/"))
        .await
        .unwrap();
    let fill = || BrowserAction::FillLogin {
        login: login_id.clone(),
        submit: true,
    };
    let refused = ask(f, &binding, fill()).await.unwrap_err();
    assert!(refused.contains("isn't for"), "{refused}");
    assert!(browser.filled.lock().unwrap().is_empty());
    ask(f, &binding, open("https://forum.example.com/login"))
        .await
        .unwrap();
    let filled = ask(f, &binding, fill()).await.unwrap();
    assert!(filled.contains("\"filled\""), "{filled}");
    assert_eq!(
        *browser.filled.lock().unwrap(),
        [("agent@example.com".to_owned(), PASSWORD.to_owned())]
    );
    // What the page shows back reaches the agent with the password hidden.
    let seen = match f
        .hub
        .handle(
            &binding,
            BridgeRequest::Browser {
                action: BrowserAction::Observe,
            },
        )
        .await
    {
        BridgeResponse::BrowserResult { text, image_base64 } => {
            assert_eq!(image_base64.as_deref(), Some("c2NyZWVu"));
            text
        }
        other => panic!("{other:?}"),
    };
    assert!(!seen.contains("pässword") && seen.contains(MASK), "{seen}");
    assert!(
        !seen.contains("c2NyZWVu"),
        "the screenshot goes as an image"
    );
    // The label is masked before it is shortened, so not even the
    // password's first three characters are left at the cut.
    assert!(seen.contains("downloads"), "{seen}");
    let seen: serde_json::Value = serde_json::from_str(&seen).unwrap();
    let label = seen["controls"][0]["name"].as_str().unwrap();
    assert!(label.ends_with('…') && !label.contains("syn"), "{label}");
    assert_eq!(label.chars().count(), NAME_SHOWN + 1);
    let history = view(f, &Caller::local_owner()).await.history;
    let used = (history.iter())
        .find(|e| e.event == AgentLoginEvent::Filled)
        .unwrap();
    assert_eq!(used.by, "Claude Code");
    assert_eq!(used.site.as_deref(), Some("forum.example.com"));
    assert_eq!(used.task_id, Some(task));
    let refused = (history.iter())
        .find(|e| e.event == AgentLoginEvent::Refused)
        .unwrap();
    assert_eq!(refused.site.as_deref(), Some("elsewhere.example.net"));
    f.hub.unbound(&binding).await;
    assert_eq!(*browser.closed.lock().unwrap(), [task]);
    l.f.close().await;
}

#[tokio::test]
async fn an_ask_first_login_waits_for_the_owner_and_lasts_for_the_run() {
    let (Browsing { l, browser }, login_id) = browsing(LoginHow::AskFirst).await;
    let f = &l.f;
    let (task, binding) = bound(f, None).await;
    // Its site stays closed until the owner allows it.
    assert_eq!(browser.blocked(task), ["forum.example.com"]);
    let refused = ask(f, &binding, open("https://forum.example.com/login"))
        .await
        .unwrap_err();
    assert!(refused.contains("closed"), "{refused}");
    let fill = || BrowserAction::FillLogin {
        login: login_id.clone(),
        submit: false,
    };
    let waiting_fill = asked(f, &binding, fill());
    let approval = pending(f, ApprovalKind::LoginUse).await;
    assert!(approval.title.contains("Forum"), "{}", approval.title);
    assert!(!waiting_fill.is_finished());
    f.hub
        .resolve_approval(
            &Caller::local_owner(),
            approval.approval_id,
            &ApprovalDecision::Allow,
        )
        .await
        .unwrap();
    // Allowed before the page is open: the agent opens it and fills again.
    let early = result(waiting_fill.await.unwrap());
    assert!(early.is_err(), "{early:?}");
    assert!(browser.blocked(task).is_empty());
    ask(f, &binding, open("https://forum.example.com/login"))
        .await
        .unwrap();
    // Not asked again in this run.
    ask(f, &binding, fill()).await.unwrap();
    assert_eq!(browser.filled.lock().unwrap().len(), 1);
    let history = view(f, &Caller::local_owner()).await.history;
    let approved = (history.iter())
        .find(|e| e.event == AgentLoginEvent::Approved)
        .unwrap();
    assert_eq!(approved.by, "This computer");
    assert_eq!(approved.task_id, Some(task));
    assert!(f.hub.inner.login_asks.lock().unwrap().is_empty());
    // The run is over, and so is what it was allowed.
    f.hub.unbound(&binding).await;
    assert_eq!(browser.blocked(task), ["forum.example.com"]);
    l.f.close().await;
}

#[tokio::test]
async fn a_later_run_is_asked_again_however_late_the_last_one_lets_go() {
    let (Browsing { l, browser }, login_id) = browsing(LoginHow::AskFirst).await;
    let f = &l.f;
    let owner = Caller::local_owner();
    let (task, first) = bound(f, None).await;
    let fill = || BrowserAction::FillLogin {
        login: login_id.clone(),
        submit: false,
    };
    let waiting_fill = asked(f, &first, fill());
    let approval = pending(f, ApprovalKind::LoginUse).await;
    f.hub
        .resolve_approval(&owner, approval.approval_id, &ApprovalDecision::Allow)
        .await
        .unwrap();
    waiting_fill.await.unwrap();
    assert!(browser.blocked(task).is_empty());
    // The run finishes, and the task is continued, before the run is let go.
    f.hub
        .finish_run(first.run_id, RunState::Completed, None)
        .unwrap();
    let current: Task = f.hub.read(|r| r.require("task", task)).unwrap();
    let mut resume = CommandEnvelope::new(Command::ResumeTask {
        mode: None,
        task_id: task,
        text: Some("Post the rest".into()),
        bundle: None,
        carried_plan: None,
        read_only: false,
        model: None,
        effort: None,
    });
    resume.expected_revision = Some(current.revision);
    let r = f.hub.submit(&owner, resume).await;
    assert_eq!(r.status, ReceiptStatus::Accepted, "{r:?}");
    let second_run = run_started(f, task).await;
    assert!(browser.closed.lock().unwrap().is_empty());
    assert_ne!(second_run, first.run_id);
    let bridge = (f.harness.specs.lock().unwrap().last())
        .and_then(|spec| spec.bridge.clone())
        .expect("a bridge");
    let token = (bridge.env.iter())
        .find(|(key, _)| key == ports::bridge::RUN_TOKEN_ENV)
        .map(|(_, token)| token.clone())
        .unwrap();
    let second = f.hub.bind(&token).await.expect("bridge binding");
    assert_eq!(second.run_id, second_run);
    // The first run's tabs closed, and what it was let use went with it.
    assert_eq!(*browser.closed.lock().unwrap(), [task]);
    assert_eq!(browser.blocked(task), ["forum.example.com"]);
    // Its bridge, still open, browses no more, and letting go late leaves
    // the next run's tabs as they are.
    ask(f, &first, open("https://example.com/"))
        .await
        .unwrap_err();
    f.hub.unbound(&first).await;
    assert_eq!(*browser.closed.lock().unwrap(), [task]);
    ask(f, &second, open("https://example.com/")).await.unwrap();
    // The next run is asked about the login again.
    let waiting_fill = asked(f, &second, fill());
    let again = pending(f, ApprovalKind::LoginUse).await;
    assert_eq!(again.run_id, second_run);
    assert!(!waiting_fill.is_finished());
    f.hub
        .resolve_approval(
            &owner,
            again.approval_id,
            &ApprovalDecision::Deny { reason: None },
        )
        .await
        .unwrap();
    result(waiting_fill.await.unwrap()).unwrap_err();
    assert!(browser.filled.lock().unwrap().is_empty());
    l.f.close().await;
}

#[tokio::test]
async fn a_request_held_up_as_its_run_is_replaced_does_nothing() {
    let (Browsing { l, browser }, _) = browsing(LoginHow::Whenever).await;
    let f = &l.f;
    let owner = Caller::local_owner();
    let (task, first) = bound(f, None).await;
    // The run asks for a page while the browser still has to be signed out
    // of a site, which takes a while.
    l.logins
        .owe_sign_out(&["forum.example.com".into()])
        .unwrap();
    let (started, signing_out) = oneshot::channel();
    let (let_go, held) = oneshot::channel();
    *browser.held_sign_out.lock().unwrap() = Some((started, held));
    let waiting_open = asked(f, &first, open("https://example.com/"));
    signing_out.await.unwrap();

    // Meanwhile it finishes, and the task's next run takes the browser.
    f.hub
        .finish_run(first.run_id, RunState::Completed, None)
        .unwrap();
    let current: Task = f.hub.read(|r| r.require("task", task)).unwrap();
    let mut resume = CommandEnvelope::new(Command::ResumeTask {
        mode: None,
        task_id: task,
        text: Some("Post the rest".into()),
        bundle: None,
        carried_plan: None,
        read_only: false,
        model: None,
        effort: None,
    });
    resume.expected_revision = Some(current.revision);
    let r = f.hub.submit(&owner, resume).await;
    assert_eq!(r.status, ReceiptStatus::Accepted, "{r:?}");
    let second_run = run_started(f, task).await;
    let bridge = (f.harness.specs.lock().unwrap().last())
        .and_then(|spec| spec.bridge.clone())
        .expect("a bridge");
    let token = (bridge.env.iter())
        .find(|(key, _)| key == ports::bridge::RUN_TOKEN_ENV)
        .map(|(_, token)| token.clone())
        .unwrap();
    let hub = f.hub.clone();
    let binding = tokio::spawn(async move { hub.bind(&token).await });
    let mut more = waiting();
    while f.hub.inner.browser_runs.lock().unwrap().get(&task) != Some(&second_run) {
        assert!(more(), "the next run never took the browser");
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    // Its tabs stay as they are until the request is done.
    assert!(browser.closed.lock().unwrap().is_empty());

    // Once signed out, the first run's request opens nothing, and the next
    // run gets the task's tabs.
    let_go.send(()).unwrap();
    let refused = result(waiting_open.await.unwrap()).unwrap_err();
    assert!(refused.contains("This run has ended"), "{refused}");
    let second = binding.await.unwrap().expect("bridge binding");
    assert!(browser.url.lock().unwrap().is_empty());
    assert_eq!(*browser.closed.lock().unwrap(), [task]);
    ask(f, &second, open("https://example.com/")).await.unwrap();
    l.f.close().await;
}

#[tokio::test]
async fn a_bind_that_comes_late_leaves_the_next_runs_tabs_alone() {
    let (Browsing { l, browser }, _) = browsing(LoginHow::Whenever).await;
    let f = &l.f;
    let (task_id, first) = bound(f, None).await;
    let task: Task = f.hub.read(|r| r.require("task", task_id)).unwrap();
    let project: Project = f
        .hub
        .read(|r| r.require("project", task.project_id))
        .unwrap();
    // A later run takes the browser while the first run's bind is still on
    // its way to it.
    let second = RunId::new();
    let previous = f.hub.claim_browser(task_id, second);
    assert_eq!(previous, Some(first.run_id));
    f.hub
        .start_browsing(second, &task, &project, previous)
        .await;
    assert_eq!(*browser.closed.lock().unwrap(), [task_id]);
    assert_eq!(browser.registered.lock().unwrap().len(), 2);

    // The first run's bind arrives: it closes nothing and changes nothing.
    f.hub
        .start_browsing(first.run_id, &task, &project, Some(RunId::new()))
        .await;
    assert_eq!(*browser.closed.lock().unwrap(), [task_id]);
    assert_eq!(browser.registered.lock().unwrap().len(), 2);
    let refused = ask(f, &first, BrowserAction::Observe).await.unwrap_err();
    assert!(refused.contains("This run has ended"), "{refused}");
    l.f.close().await;
}

#[tokio::test]
async fn a_page_seen_after_its_runs_codes_are_let_go_is_not_shown() {
    let (Browsing { l, browser }, login_id) = browsing(LoginHow::Whenever).await;
    let f = &l.f;
    let (_, binding) = bound(f, None).await;
    let phone = phone_with_accounts(f).await;
    let waiting_code = asked(
        f,
        &binding,
        BrowserAction::AskOwnerForCode {
            login: login_id,
            why: "Enter the code we emailed you".into(),
            submit: true,
        },
    );
    let approval = pending(f, ApprovalKind::LoginCode).await;
    let answer = login(AgentLoginAction::AnswerCode {
        approval_id: approval.approval_id,
        code: Secret::new("481516"),
    });
    assert!(done(f, &phone, answer).await.is_none());
    result(waiting_code.await.unwrap()).unwrap();

    // The run looks at the page, which says the code back, as its event
    // stream is lost and what it was given is let go.
    l.logins
        .owe_sign_out(&["forum.example.com".into()])
        .unwrap();
    let (started, signing_out) = oneshot::channel();
    let (let_go, held) = oneshot::channel();
    *browser.held_sign_out.lock().unwrap() = Some((started, held));
    let waiting_look = asked(f, &binding, BrowserAction::Observe);
    signing_out.await.unwrap();
    f.hub.release_accounts(binding.run_id);
    let_go.send(()).unwrap();
    let refused = result(waiting_look.await.unwrap()).unwrap_err();
    assert!(
        refused.contains("This run has ended") && !refused.contains("481516"),
        "{refused}"
    );
    l.f.close().await;
}

#[tokio::test]
async fn an_ask_first_login_the_owner_refuses_is_not_filled() {
    let (Browsing { l, browser }, login_id) = browsing(LoginHow::AskFirst).await;
    let f = &l.f;
    let (task, binding) = bound(f, None).await;
    let waiting_fill = asked(
        f,
        &binding,
        BrowserAction::FillLogin {
            login: login_id,
            submit: false,
        },
    );
    let approval = pending(f, ApprovalKind::LoginUse).await;
    f.hub
        .resolve_approval(
            &Caller::local_owner(),
            approval.approval_id,
            &ApprovalDecision::Deny { reason: None },
        )
        .await
        .unwrap();
    let refused = result(waiting_fill.await.unwrap()).unwrap_err();
    assert!(refused.contains("didn't let"), "{refused}");
    assert!(browser.filled.lock().unwrap().is_empty());
    assert_eq!(browser.blocked(task), ["forum.example.com"]);
    let history = view(f, &Caller::local_owner()).await.history;
    assert!(history.iter().any(|e| e.event == AgentLoginEvent::Refused
        && e.detail.as_deref() == Some("Not allowed for this task")));
    l.f.close().await;
}

#[tokio::test]
async fn the_owner_types_a_code_the_agent_never_sees() {
    let (Browsing { l, browser }, login_id) = browsing(LoginHow::Whenever).await;
    let f = &l.f;
    let (_, binding) = bound(f, None).await;
    let phone = phone_with_accounts(f).await;
    let code = |login: &str| BrowserAction::AskOwnerForCode {
        login: login.into(),
        why: "Enter the code we emailed you".into(),
        submit: true,
    };
    let waiting_code = asked(f, &binding, code(&login_id));
    let approval = pending(f, ApprovalKind::LoginCode).await;
    assert!(approval.title.contains("emailed"), "{}", approval.title);
    // Not a code, so the request still waits.
    let answer = |code: &str| {
        login(AgentLoginAction::AnswerCode {
            approval_id: approval.approval_id,
            code: Secret::new(code),
        })
    };
    let wrong = done(f, &phone, answer("12")).await.unwrap();
    assert_eq!(wrong.code, ErrorCode::InvalidRequest);
    assert!(done(f, &phone, answer("481 516")).await.is_none());
    // The page says the code back, and the agent is shown it hidden.
    let given = result(waiting_code.await.unwrap()).unwrap();
    assert!(!given.contains("481516") && given.contains(MASK), "{given}");
    assert_eq!(*browser.codes.lock().unwrap(), ["481516"]);
    let seen = ask(f, &binding, BrowserAction::Observe).await.unwrap();
    assert!(
        !seen.contains("481516") && seen.contains("code [hidden"),
        "{seen}"
    );
    let history = view(f, &phone).await.history;
    let entry = (history.iter())
        .find(|e| e.event == AgentLoginEvent::CodeGiven)
        .unwrap();
    assert_eq!(entry.by, "Phone");
    assert!(f.hub.inner.login_asks.lock().unwrap().is_empty());
    // Once answered, the request takes no other code.
    let late = done(f, &phone, answer("999999")).await.unwrap();
    assert_ne!(late.code, ErrorCode::InvalidRequest, "{late:?}");

    let waiting_code = asked(f, &binding, code(&login_id));
    let approval = pending(f, ApprovalKind::LoginCode).await;
    f.hub
        .resolve_approval(
            &Caller::local_owner(),
            approval.approval_id,
            &ApprovalDecision::Deny { reason: None },
        )
        .await
        .unwrap();
    let refused = result(waiting_code.await.unwrap()).unwrap_err();
    assert!(refused.contains("didn't give a code"), "{refused}");
    assert_eq!(browser.codes.lock().unwrap().len(), 1);
    let unknown = ask(f, &binding, code("0000000000000000"))
        .await
        .unwrap_err();
    assert!(!unknown.is_empty());
    l.f.close().await;
}

#[tokio::test]
async fn a_code_goes_only_where_and_while_it_was_asked_for() {
    let (Browsing { l, browser }, login_id) = browsing(LoginHow::Whenever).await;
    let f = &l.f;
    let (_, binding) = bound(f, None).await;
    let phone = phone_with_accounts(f).await;
    let owner = Caller::local_owner();
    let code = || BrowserAction::AskOwnerForCode {
        login: login_id.clone(),
        why: "Enter the code we texted you".into(),
        submit: false,
    };
    let answer = |approval: &Approval| {
        login(AgentLoginAction::AnswerCode {
            approval_id: approval.approval_id,
            code: Secret::new("481516"),
        })
    };
    // The tab moved on to another page while the owner was asked.
    *browser.document.lock().unwrap() = "first".into();
    let waiting_code = asked(f, &binding, code());
    let approval = pending(f, ApprovalKind::LoginCode).await;
    *browser.document.lock().unwrap() = "second".into();
    assert!(done(f, &phone, answer(&approval)).await.is_none());
    let refused = result(waiting_code.await.unwrap()).unwrap_err();
    assert!(refused.contains("changed"), "{refused}");
    assert!(browser.codes.lock().unwrap().is_empty());

    // The login was paused, and resumed, while the owner was asked.
    let waiting_code = asked(f, &binding, code());
    let approval = pending(f, ApprovalKind::LoginCode).await;
    for action in [
        AgentLoginAction::Pause {
            login_id: login_id.clone(),
        },
        AgentLoginAction::Resume {
            login_id: login_id.clone(),
        },
    ] {
        assert!(done(f, &owner, login(action)).await.is_none());
    }
    assert!(done(f, &phone, answer(&approval)).await.is_none());
    let refused = result(waiting_code.await.unwrap()).unwrap_err();
    assert!(
        refused.contains("changed while the owner was asked"),
        "{refused}"
    );
    assert!(browser.codes.lock().unwrap().is_empty());
    let history = view(f, &owner).await.history;
    assert!(history.iter().any(|e| e.event == AgentLoginEvent::Refused
        && e.detail.as_deref() == Some("It changed while the owner was asked")));
    l.f.close().await;
}

#[tokio::test]
async fn an_ask_first_approval_opens_only_the_login_it_was_asked_about() {
    let (Browsing { l, browser }, login_id) = browsing(LoginHow::AskFirst).await;
    let f = &l.f;
    let owner = Caller::local_owner();
    let (task, binding) = bound(f, None).await;
    let waiting_fill = asked(
        f,
        &binding,
        BrowserAction::FillLogin {
            login: login_id.clone(),
            submit: false,
        },
    );
    let approval = pending(f, ApprovalKind::LoginUse).await;
    // The login changes while the owner is asked about it.
    for action in [
        AgentLoginAction::Pause {
            login_id: login_id.clone(),
        },
        AgentLoginAction::Resume {
            login_id: login_id.clone(),
        },
    ] {
        assert!(done(f, &owner, login(action)).await.is_none());
    }
    f.hub
        .resolve_approval(&owner, approval.approval_id, &ApprovalDecision::Allow)
        .await
        .unwrap();
    let refused = result(waiting_fill.await.unwrap()).unwrap_err();
    assert!(
        refused.contains("changed while the owner was asked"),
        "{refused}"
    );
    assert_eq!(browser.blocked(task), ["forum.example.com"]);
    let history = view(f, &owner).await.history;
    assert!(!history.iter().any(|e| e.event == AgentLoginEvent::Approved));
    l.f.close().await;
}

#[tokio::test]
async fn sites_a_login_stops_covering_are_signed_out_of_first() {
    let (Browsing { l, browser }, login_id) = browsing(LoginHow::Whenever).await;
    let f = &l.f;
    let owner = Caller::local_owner();
    let AgentLoginAction::Add { details, .. } = added() else {
        unreachable!()
    };
    let edit = |sites: &[&str], notes: &str| {
        login(AgentLoginAction::Edit {
            login_id: login_id.clone(),
            details: LoginDetails {
                sites: sites.iter().map(|s| (*s).to_owned()).collect(),
                notes: notes.into(),
                ..details.clone()
            },
        })
    };
    let sites = || async { view(f, &owner).await.logins[0].sites.clone() };

    // Kept sites stay signed in.
    let edited = edit(&["forum.example.com", "help.example.org"], "Kept");
    assert!(done(f, &owner, edited).await.is_none());
    assert!(browser.signed_out.lock().unwrap().is_empty());

    // If the browser can't sign out of a site left out, nothing changes.
    *browser.cant_sign_out.lock().unwrap() = true;
    let refused = done(f, &owner, edit(&["help.example.org"], ""))
        .await
        .unwrap();
    assert!(
        refused.message.contains("nothing was changed"),
        "{refused:?}"
    );
    assert_eq!(sites().await, ["forum.example.com", "help.example.org"]);

    *browser.cant_sign_out.lock().unwrap() = false;
    assert!(
        done(f, &owner, edit(&["help.example.org"], ""))
            .await
            .is_none()
    );
    assert_eq!(sites().await, ["help.example.org"]);
    let signed_out = browser.signed_out.lock().unwrap().clone();
    assert_eq!(signed_out, [["forum.example.com"], ["forum.example.com"]]);

    // Deleting signs out of what is left.
    let delete = login(AgentLoginAction::Delete { login_id });
    assert!(done(f, &owner, delete).await.is_none());
    assert_eq!(
        browser.signed_out.lock().unwrap().last().unwrap(),
        &["help.example.org"]
    );
    l.f.close().await;
}

#[tokio::test]
async fn a_task_without_the_browser_gets_none_of_it() {
    let (Browsing { l, browser }, _) = browsing(LoginHow::Whenever).await;
    let f = &l.f;
    let (task, binding) = bound(f, Some(false)).await;
    assert!(!binding.browser);
    assert!(!browser.browsing.lock().unwrap().contains_key(&task));
    let refused = ask(f, &binding, BrowserAction::ListLogins)
        .await
        .unwrap_err();
    assert!(refused.contains("not enabled"), "{refused}");
    l.f.close().await;

    // Nor does any task on a computer without one.
    let f = Fixture::new().await;
    let (_, binding) = bound(&f, None).await;
    assert!(!binding.browser);
    f.close().await;
}

#[tokio::test]
async fn a_sign_out_made_without_a_browser_is_done_before_anything_browses() {
    // Ophio without its agent browser, as when it finds none.
    let l = with_logins().await;
    let owner = Caller::local_owner();
    let accept = AgentLoginAction::Accept {
        version: DISCLAIMER_VERSION,
    };
    assert!(done(&l.f, &owner, login(accept)).await.is_none());
    assert!(done(&l.f, &owner, login(added())).await.is_none());
    let login_id = view(&l.f, &owner).await.logins[0].login_id.clone();
    let delete = login(AgentLoginAction::Delete { login_id });
    assert!(done(&l.f, &owner, delete).await.is_none());
    assert_eq!(l.logins.owed_sign_outs(), ["forum.example.com"]);
    l.f.close().await;

    // With a browser again, the deleted login keeps no task from its site,
    // so nothing browses before the browser is signed out of it.
    let logins = ComputerAgentLogins::new(LoginsConfig::new(
        l.dir.path().join("accounts"),
        Arc::new(MemoryVault::default()),
        None,
    ));
    let given = Arc::new(logins.clone());
    let browser = Arc::new(FakeBrowser::default());
    let used = browser.clone();
    let f = Fixture::with(move |b| b.agent_logins(given).agent_browser(used)).await;
    let (_, binding) = bound(&f, None).await;
    *browser.cant_sign_out.lock().unwrap() = true;
    let refused = ask(&f, &binding, open("https://forum.example.com/"))
        .await
        .unwrap_err();
    assert!(refused.contains("signed out of sites first"), "{refused}");
    assert!(browser.url.lock().unwrap().is_empty());
    assert_eq!(logins.owed_sign_outs(), ["forum.example.com"]);

    *browser.cant_sign_out.lock().unwrap() = false;
    ask(&f, &binding, open("https://forum.example.com/"))
        .await
        .unwrap();
    assert_eq!(*browser.signed_out.lock().unwrap(), [["forum.example.com"]]);
    assert!(logins.owed_sign_outs().is_empty());
    f.close().await;
}

#[tokio::test]
async fn a_held_chat_browses_only_with_internet_and_local_network() {
    use crate::tests::conversation_access::{allow_ask, create};
    use protocol::access::ConversationAccess;
    let backend = Arc::new(crate::tests::conversation_access::Backend::default());
    let browser = Arc::new(FakeBrowser::default());
    let f = Fixture::with(move |b| b.agent_browser(browser).sandboxes(backend)).await;
    let owner = Caller::local_owner();
    let project = f.project().await;
    let access = ConversationAccess {
        read: vec![project],
        internet: false,
        ..Default::default()
    };
    allow_ask(&f);
    let mut create = create(project, access.clone());
    let Command::CreateTask { browser, .. } = &mut create.body else {
        unreachable!()
    };
    *browser = Some(true);
    let r = f.hub.submit(&owner, create).await;
    let Some(CommandResult::TaskCreated { task_id, .. }) = r.result else {
        panic!("{r:?}")
    };
    run_started(&f, task_id).await;
    let bridge = (f.harness.specs.lock().unwrap().last())
        .and_then(|spec| spec.bridge.clone())
        .expect("a bridge");
    assert!(bridge.browser);
    let token = (bridge.env.iter())
        .find(|(key, _)| key == ports::bridge::RUN_TOKEN_ENV)
        .map(|(_, token)| token.clone())
        .unwrap();
    let binding = f.hub.bind(&token).await.expect("bridge binding");
    // The browser runs on the computer, outside the chat's sandbox, so it
    // waits for both Internet and Local network.
    for access in [
        access.clone(),
        ConversationAccess {
            internet: true,
            ..access.clone()
        },
    ] {
        let set = Command::SetConversationAccess { task_id, access };
        let receipt = f.hub.submit(&owner, CommandEnvelope::new(set)).await;
        assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
        let refused = ask(&f, &binding, BrowserAction::Tabs).await.unwrap_err();
        assert!(refused.contains("outside this chat's sandbox"), "{refused}");
    }
    let access = ConversationAccess {
        internet: true,
        local_network: true,
        ..access
    };
    let set = Command::SetConversationAccess { task_id, access };
    let receipt = f.hub.submit(&owner, CommandEnvelope::new(set)).await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    ask(&f, &binding, BrowserAction::Tabs).await.unwrap();
    f.close().await;
}
