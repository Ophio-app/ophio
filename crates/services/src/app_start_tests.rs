// SPDX-License-Identifier: Apache-2.0
//! A review that uses the app has it started first: an agent works out how,
//! read-only, the person allows the command once, and Øphio runs it in a
//! terminal of its own before the reviewer, which has only the screen.
use super::super::watch_tests::{ended, notes, run, task, until};
use super::*;
use crate::app_start::{AppStart, START};
use ports::TerminalHost as _;

/// An app that answers once anything has been typed into a terminal.
#[derive(Debug)]
struct Answers(Arc<FakeTerminal>);
#[async_trait::async_trait]
impl crate::preview::PreviewChecker for Answers {
    async fn check(&self, _: &url::Url) -> Result<bool, ProtocolError> {
        let typed = (self.0.list().await.iter()).any(|t| !self.0.input(t.terminal_id).is_empty());
        Ok(typed)
    }
}

struct App<T = FakeTerminal> {
    r: Repo,
    terminal: Arc<T>,
}

async fn app() -> App {
    let terminal = Arc::new(FakeTerminal::default());
    let page = Arc::new(Answers(terminal.clone()));
    let host = terminal.clone();
    let r = repo_with(true, move |b| b.terminal(host).preview_checker(page)).await;
    r.set(drafts()).await.unwrap();
    App { r, terminal }
}

impl<T: ports::TerminalHost> App<T> {
    /// A review by a novice user of the app, asked to start now.
    async fn review(&self, prompt: &str) -> TaskId {
        let target = self.r.f.desktop.target.lock().unwrap().target_id;
        let created = (self.r.f.hub)
            .submit(
                &Caller::local_owner(),
                CommandEnvelope::new(Command::CreateTask {
                    github: Some(Box::new(GithubLink::Find {
                        scope: FindScope::Project,
                        focus: Some(FindFocus::NoviceUser),
                        max_issues: None,
                        fix_findings: None,
                        fix_with: claude(),
                    })),
                    agent_chosen: None,
                    mode: Some(TaskMode::Work),
                    project_id: self.r.project_id,
                    kind: TaskKind::ComputerUse,
                    title: None,
                    prompt: prompt.into(),
                    bundle: None,
                    harness: HarnessKind::ClaudeCode,
                    model: None,
                    effort: None,
                    launch_policy: None,
                    branch: None,
                    mission: None,
                    target_id: Some(target),
                    placement: TaskPlacement::StartNow,
                    start: None,
                    place: None,
                    browser: None,
                    conversation: None,
                    access: None,
                    review: None,
                }),
            )
            .await;
        let Some(CommandResult::TaskCreated { task_id, .. }) = created.result else {
            panic!("{created:?}")
        };
        task_id
    }

    /// The run holding the screen's lease, if any.
    fn screen_held_by(&self) -> Option<LeaseOwner> {
        let target = self.r.f.desktop.target.lock().unwrap().target_id;
        let view: TargetControlView = self.r.f.hub.read(|r| r.require("control", target)).unwrap();
        view.lease.map(|l| l.owner)
    }

    fn started(&self) -> Option<AppStart> {
        (self.r.f.hub)
            .read(|r| r.get(START, self.r.project_id))
            .unwrap()
    }

    /// What `review`'s approvals asked.
    fn asked(&self, review: TaskId) -> Vec<Approval> {
        (self
            .r
            .f
            .hub
            .read(|r| r.list::<Approval>("approval"))
            .unwrap())
        .into_iter()
        .filter(|a| a.task_id == review)
        .collect()
    }

    async fn decide(&self, approval: &Approval, decision: ApprovalDecision) {
        let mut request = CommandEnvelope::new(Command::ResolveApproval {
            approval_id: approval.approval_id,
            decision,
        });
        request.expected_revision = Some(approval.revision.into());
        let id = request.command_id;
        let r = self.r.f.hub.submit(&Caller::local_owner(), request).await;
        assert_eq!(r.status, ReceiptStatus::Accepted, "{r:?}");
        settle(&self.r.f.hub, id).await;
    }

    async fn close(self) {
        self.r.f.close().await;
    }
}

/// The agent working out how to start `review`'s app says `said` and ends.
/// Its brief.
pub(super) async fn set_up(r: &Repo, review: TaskId, said: &str) -> String {
    let f = &r.f;
    let mut helper = None;
    until("an agent works out how to start the app", || {
        helper = (f.hub.read(|r| r.list::<Task>("task")).unwrap().into_iter())
            .find(|t| t.asked_by == Some(review));
        helper.is_some()
    })
    .await;
    let helper = helper.unwrap();
    // The review waits for it, not yet started.
    let waiting = task(f, review).current_run.unwrap();
    assert_eq!(run(f, waiting).state, RunState::Admitted);
    let helper_run = run_started(f, helper.task_id).await;
    let spec = r.spec(helper_run);
    assert_eq!(spec.mode, TaskMode::Ask);
    assert_eq!(spec.launch_policy, None);
    let message = ports::HarnessEvent::AgentMessage {
        message_key: "m".into(),
        text: said.into(),
    };
    f.harness.emit(helper_run, message).await;
    ended(f, helper_run, ports::RunExit::Completed).await;
    // What it said is for the review, not the person.
    let raised = (f
        .hub
        .read(|r| r.list::<AttentionItem>("attention"))
        .unwrap())
    .into_iter()
    .any(|a| a.run_id == Some(helper_run));
    assert!(!raised);
    spec.prompt.text
}

const VITE: &str = "```ophio-start\n{\"command\": \"npm ci && npm run dev\", \"url\": \"http://localhost:5173/\"}\n```";

#[tokio::test]
async fn the_app_is_started_for_a_review_once_the_person_allows_how() {
    let a = app().await;
    let f = &a.r.f;
    let review = a.review("Try the app as a novice user.").await;
    let brief = set_up(&a.r, review, &format!("It's a Vite app.\n\n{VITE}")).await;
    for said in [
        "Ophio is about to review this project",
        "You're read-only here",
        "```ophio-start",
        "package.json",
    ] {
        assert!(brief.contains(said), "{said}: {brief}");
    }
    let folder = std::fs::canonicalize(f.project_dir()).unwrap();
    assert!(brief.contains(&*folder.to_string_lossy()), "{brief}");
    assert!(
        (notes(f, review).await.iter())
            .any(|n| n.starts_with("Working out how to start Test first")),
        "{:?}",
        notes(f, review).await
    );

    // The person reads the command and where it runs before anything does.
    let mut asked = Vec::new();
    until("the person is asked", || {
        asked = a.asked(review);
        !asked.is_empty()
    })
    .await;
    let [approval] = &asked[..] else { panic!() };
    assert_eq!(approval.title, "Start Test with this command?");
    assert_eq!(approval.kind, ApprovalKind::Command);
    let lines: Vec<_> = (approval.detail_lines.iter())
        .map(|l| (l.label.clone().unwrap(), l.text.clone()))
        .collect();
    assert_eq!(lines[0], ("Command".into(), "npm ci && npm run dev".into()));
    assert_eq!(
        lines[2],
        ("Address".into(), "http://localhost:5173/".into())
    );
    assert!(a.terminal.list().await.is_empty());
    assert_eq!(task(f, review).state, TaskState::AwaitingDecision);
    // The screen isn't granted while the person is asked, or it lapses
    // before the reviewer could use it.
    assert_eq!(a.screen_held_by(), None);

    a.decide(approval, ApprovalDecision::Allow).await;
    let started = run_started(f, review).await;
    assert_eq!(
        a.screen_held_by(),
        Some(LeaseOwner::Agent { run_id: started })
    );
    let saved = a.started().expect("kept for the project");
    assert_eq!(saved.command, "npm ci && npm run dev");
    assert_eq!(saved.url.as_deref(), Some("http://localhost:5173/"));
    assert_eq!(saved.allowed_by, Principal::LocalOwner);
    // Run in its own terminal in the project's folder, before the reviewer.
    let [t] = &a.terminal.list().await[..] else {
        panic!()
    };
    assert_eq!(t.title, "App: Test");
    assert_eq!(
        std::fs::canonicalize(&t.cwd).unwrap(),
        std::fs::canonicalize(f.project_dir()).unwrap()
    );
    assert_eq!(a.terminal.input(t.terminal_id), b"npm ci && npm run dev\r");
    let told = a.r.spec(started).github.unwrap();
    for said in [
        "Ophio has started the app for this review",
        "`npm ci && npm run dev`",
        "Open http://localhost:5173/ in the browser on the screen",
        "When no browser is open there, start one",
        "When the app isn't up, bring it up yourself",
    ] {
        assert!(told.contains(said), "{said}: {told}");
    }
    assert!(
        (notes(f, review).await.iter())
            .any(|n| n.contains("in the terminal “App: Test” in Control. It answers at")),
        "{:?}",
        notes(f, review).await
    );
    let open = (f
        .hub
        .read(|r| r.list::<AttentionItem>("attention"))
        .unwrap())
    .into_iter()
    .any(|i| i.approval_id == Some(approval.approval_id) && i.resolved_at.is_none());
    assert!(!open, "the question leaves the phone once answered");
    a.close().await;
}

#[tokio::test]
async fn an_allowed_start_is_used_again_without_asking() {
    let a = app().await;
    let f = &a.r.f;
    (f.hub)
        .write(|tx| {
            tx.put(
                START,
                a.r.project_id,
                &AppStart {
                    command: "cargo run".into(),
                    url: Some("http://127.0.0.1:8080/".into()),
                    allowed_by: Principal::LocalOwner,
                    allowed_at: protocol::time::Timestamp(1),
                },
            )
        })
        .unwrap();
    let first = a.review("Try the app as a novice user.").await;
    let first_run = run_started(f, first).await;
    assert!(a.asked(first).is_empty());
    let all = f.hub.read(|r| r.list::<Task>("task")).unwrap();
    assert!(
        all.iter().all(|t| t.asked_by.is_none()),
        "nobody else asked"
    );
    let [t] = &a.terminal.list().await[..] else {
        panic!()
    };
    assert_eq!(a.terminal.input(t.terminal_id), b"cargo run\r");
    assert!(
        (a.r.spec(first_run).github.unwrap()).contains("the app answers at http://127.0.0.1:8080/")
    );
    ended(f, first_run, ports::RunExit::Completed).await;

    // Still running and answering, it isn't started twice.
    let second = a.review("Try the app as a novice user.").await;
    let second_run = run_started(f, second).await;
    let [t] = &a.terminal.list().await[..] else {
        panic!()
    };
    assert_eq!(a.terminal.input(t.terminal_id), b"cargo run\r");
    let told = a.r.spec(second_run).github.unwrap();
    assert!(told.contains("it runs `cargo run`"), "{told}");
    ended(f, second_run, ports::RunExit::Completed).await;

    // Said how to open it this time, the review has that worked out again.
    let third = a
        .review("Try the app as a novice user.\n\nHow to open it: run make serve")
        .await;
    let brief = set_up(
        &a.r,
        third,
        "```ophio-start\n{\"command\": \"make serve\", \"url\": null}\n```",
    )
    .await;
    assert!(
        brief.contains("The person said how to open it: \"run make serve\""),
        "{brief}"
    );
    let mut asked = Vec::new();
    until("the person is asked", || {
        asked = a.asked(third);
        !asked.is_empty()
    })
    .await;
    a.decide(&asked[0], ApprovalDecision::Allow).await;
    let third_run = run_started(f, third).await;
    assert_eq!(a.started().unwrap().command, "make serve");
    // The old one is closed and the new command runs in a terminal of its
    // own; with no page to ask, it is given a while.
    let running: Vec<_> = (a.terminal.list().await.into_iter())
        .filter(|t| t.state == protocol::terminal::TerminalState::Running)
        .collect();
    let [t] = &running[..] else {
        panic!("{running:?}")
    };
    assert_eq!(a.terminal.input(t.terminal_id), b"make serve\r");
    let told = a.r.spec(third_run).github.unwrap();
    assert!(told.contains("Its window may take a few minutes"), "{told}");
    a.close().await;
}

#[tokio::test]
async fn a_running_desktop_app_is_preserved_for_the_next_review() {
    let a = app().await;
    let f = &a.r.f;
    f.hub
        .write(|tx| {
            tx.put(
                START,
                a.r.project_id,
                &AppStart {
                    command: "cargo run".into(),
                    url: None,
                    allowed_by: Principal::LocalOwner,
                    allowed_at: protocol::time::Timestamp(1),
                },
            )
        })
        .unwrap();

    let first = a.review("Try the desktop app as a novice user.").await;
    let first_run = run_started(f, first).await;
    assert!(a.asked(first).is_empty());
    let [original] = &a.terminal.list().await[..] else {
        panic!()
    };
    let original_id = original.terminal_id;
    assert_eq!(a.terminal.input(original_id), b"cargo run\r");
    ended(f, first_run, ports::RunExit::Completed).await;

    let second = a.review("Try the desktop app as a novice user.").await;
    let second_run = run_started(f, second).await;
    assert!(a.asked(second).is_empty());
    assert_eq!(
        a.terminal.info(original_id).await.unwrap().state,
        protocol::terminal::TerminalState::Running,
        "a second review must not close the original desktop app"
    );
    let terminals = a.terminal.list().await;
    assert_eq!(terminals.len(), 1, "no replacement terminal is opened");
    assert_eq!(terminals[0].terminal_id, original_id);
    assert_eq!(a.terminal.input(original_id), b"cargo run\r");
    let told = a.r.spec(second_run).github.unwrap();
    assert!(told.contains("it ran `cargo run`"), "{told}");
    assert!(told.contains("If the app has exited"), "{told}");
    assert!(told.contains("don't start another copy"), "{told}");
    assert!(told.contains("window"), "{told}");
    assert!(!told.contains("the app answers at"), "{told}");
    assert!(!told.contains("Open  in the browser"), "{told}");
    assert!(
        notes(f, second)
            .await
            .iter()
            .any(|n| n.contains("the review uses it as it is"))
    );
    ended(f, second_run, ports::RunExit::Completed).await;

    // Reuse still requires a running terminal; a closed app is started again.
    a.terminal.close(original_id).await.unwrap();
    let third = a.review("Try the desktop app as a novice user.").await;
    let third_run = run_started(f, third).await;
    let terminals = a.terminal.list().await;
    assert_eq!(terminals.len(), 2);
    let running: Vec<_> = terminals
        .iter()
        .filter(|t| t.state == protocol::terminal::TerminalState::Running)
        .collect();
    let [replacement] = &running[..] else {
        panic!("{running:?}")
    };
    assert_ne!(replacement.terminal_id, original_id);
    assert_eq!(a.terminal.input(replacement.terminal_id), b"cargo run\r");
    let told = a.r.spec(third_run).github.unwrap();
    assert!(told.contains("Its window may take a few minutes"), "{told}");
    a.close().await;
}

#[tokio::test]
#[cfg(target_os = "linux")]
async fn an_exec_replaced_shell_is_preserved_for_the_next_review() {
    let terminal = Arc::new(::terminal::PtyHost::default());
    let host = terminal.clone();
    let r = repo_with(true, move |b| b.terminal(host)).await;
    r.set(drafts()).await.unwrap();
    let a = App { r, terminal };
    let f = &a.r.f;
    f.hub
        .write(|tx| {
            tx.put(
                START,
                a.r.project_id,
                &AppStart {
                    command: "exec /bin/cat".into(),
                    url: None,
                    allowed_by: Principal::LocalOwner,
                    allowed_at: protocol::time::Timestamp(1),
                },
            )
        })
        .unwrap();
    let first = a.review("Try the desktop app as a novice user.").await;
    let first_run = run_started(f, first).await;
    let original = a.terminal.list().await[0].terminal_id;
    ended(f, first_run, ports::RunExit::Completed).await;

    // cat has no children and occupies the original shell's PID after exec.
    // A real second review must preserve it, not mistake it for an idle shell.
    let second = a.review("Try the desktop app as a novice user.").await;
    let second_run = run_started(f, second).await;
    let state = a.terminal.info(original).await.unwrap().state;
    let terminals = a.terminal.list().await;
    ended(f, second_run, ports::RunExit::Completed).await;
    for t in &terminals {
        a.terminal.close(t.terminal_id).await.unwrap();
    }
    a.close().await;
    assert_eq!(
        state,
        protocol::terminal::TerminalState::Running,
        "the app that replaced its shell must survive the next review"
    );
    assert_eq!(terminals.len(), 1, "no replacement terminal is opened");
}

#[tokio::test]
async fn an_exited_desktop_command_restarts_even_when_its_shell_stays_open() {
    let a = app().await;
    let f = &a.r.f;
    f.hub
        .write(|tx| {
            tx.put(
                START,
                a.r.project_id,
                &AppStart {
                    command: "cargo run".into(),
                    url: None,
                    allowed_by: Principal::LocalOwner,
                    allowed_at: protocol::time::Timestamp(1),
                },
            )
        })
        .unwrap();
    let first = a.review("Try the desktop app as a novice user.").await;
    let first_run = run_started(f, first).await;
    let original = a.terminal.list().await[0].terminal_id;
    ended(f, first_run, ports::RunExit::Completed).await;

    // An app can quit without its interactive shell exiting. Only a known
    // empty process container establishes that it is safe to restart.
    a.terminal.set_running_children(original, Some(false));
    assert_eq!(
        a.terminal.info(original).await.unwrap().state,
        protocol::terminal::TerminalState::Running
    );
    let second = a.review("Try the desktop app as a novice user.").await;
    let second_run = run_started(f, second).await;
    let terminals = a.terminal.list().await;
    let running: Vec<_> = terminals
        .iter()
        .filter(|t| t.state == protocol::terminal::TerminalState::Running)
        .collect();
    let [replacement] = &running[..] else {
        panic!("{running:?}")
    };
    assert_ne!(
        replacement.terminal_id, original,
        "an idle shell is not a running app"
    );
    assert_eq!(a.terminal.input(replacement.terminal_id), b"cargo run\r");
    assert!(
        a.r.spec(second_run)
            .github
            .unwrap()
            .contains("Its window may take a few minutes")
    );

    a.terminal
        .set_running_children(replacement.terminal_id, Some(true));
    ended(f, second_run, ports::RunExit::Completed).await;
    let third = a.review("Try the desktop app as a novice user.").await;
    run_started(f, third).await;
    assert_eq!(
        a.terminal.list().await.len(),
        2,
        "the live replacement is reused"
    );
    assert_eq!(
        a.terminal
            .info(replacement.terminal_id)
            .await
            .unwrap()
            .state,
        protocol::terminal::TerminalState::Running
    );
    a.close().await;
}

#[tokio::test]
async fn a_start_the_person_declines_or_nobody_gives_leaves_the_reviewer_to_it() {
    let a = app().await;
    let f = &a.r.f;
    let review = a.review("Try the app as a novice user.").await;
    set_up(&a.r, review, VITE).await;
    let mut asked = Vec::new();
    until("the person is asked", || {
        asked = a.asked(review);
        !asked.is_empty()
    })
    .await;
    a.decide(&asked[0], ApprovalDecision::Deny { reason: None })
        .await;
    let started = run_started(f, review).await;
    assert!(a.started().is_none());
    assert!(a.terminal.list().await.is_empty());
    let told = a.r.spec(started).github.unwrap();
    assert!(!told.contains("Ophio has started"), "{told}");
    assert!(
        told.contains("Bring the app up yourself before you try anything"),
        "{told}"
    );
    assert!(
        (notes(f, review).await.iter()).any(|n| n.contains("The start command wasn't allowed")),
        "{:?}",
        notes(f, review).await
    );
    ended(f, started, ports::RunExit::Completed).await;

    // Declined, it's worked out again next time: perhaps better.
    let next = a.review("Try the app as a novice user.").await;
    set_up(
        &a.r,
        next,
        "```ophio-start\n{\"command\": null, \"why\": \"It's a phone app.\"}\n```",
    )
    .await;
    let started = run_started(f, next).await;
    assert!(a.asked(next).is_empty());
    assert!(a.terminal.list().await.is_empty());
    assert!(
        (notes(f, next).await.iter())
            .any(|n| n.contains("gave no command that starts Test: It's a phone app.")),
        "{:?}",
        notes(f, next).await
    );
    ended(f, started, ports::RunExit::Completed).await;

    // One Øphio won't run isn't put to the person.
    let last = a.review("Try the app as a novice user.").await;
    set_up(
        &a.r,
        last,
        "```ophio-start\n{\"command\": \"npm run dev\", \"url\": \"https://example.com/\"}\n```",
    )
    .await;
    run_started(f, last).await;
    assert!(a.asked(last).is_empty());
    assert!(
        (notes(f, last).await.iter())
            .any(|n| n.contains("its address isn't a page on this computer")),
        "{:?}",
        notes(f, last).await
    );
    a.close().await;
}

#[tokio::test]
async fn a_review_stopped_while_the_person_is_asked_ends_without_starting_the_app() {
    let a = app().await;
    let f = &a.r.f;
    let review = a.review("Try the app as a novice user.").await;
    set_up(&a.r, review, VITE).await;
    let mut asked = Vec::new();
    until("the person is asked", || {
        asked = a.asked(review);
        !asked.is_empty()
    })
    .await;
    let stop = (f.hub)
        .submit(
            &Caller::local_owner(),
            CommandEnvelope::new(Command::StopTask {
                task_id: review,
                force: false,
            }),
        )
        .await;
    assert_eq!(stop.status, ReceiptStatus::Accepted, "{stop:?}");
    let stopped = task(f, review).current_run.unwrap();
    until("the review ended", || run(f, stopped).state.is_terminal()).await;
    assert_eq!(run(f, stopped).state, RunState::Interrupted);
    assert!(a.started().is_none());
    assert!(a.terminal.list().await.is_empty());
    assert!(
        !f.hub
            .inner
            .waits
            .lock()
            .await
            .contains_key(&asked[0].approval_id)
    );
    a.close().await;
}
