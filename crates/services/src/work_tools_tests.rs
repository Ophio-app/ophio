// SPDX-License-Identifier: Apache-2.0
//! The tools an agent in Talk plans the person's GitHub work with, against
//! GitHub as recorded.
use super::*;
use ports::bridge::{
    AgentChoice, BridgeBinding, BridgeHandler, BridgeRequest, BridgeResponse, WorkCall,
};
use protocol::queue::{QueueAuthorization, QueueItem, QueueView};

impl Repo {
    /// A Work task with no link, as Talk starts one, and its bridge.
    async fn talking(&self) -> (Task, BridgeBinding) {
        self.talking_with(None).await
    }
    /// The same, run with `launch_policy`.
    async fn talking_with(
        &self,
        launch_policy: Option<protocol::model::LaunchPolicy>,
    ) -> (Task, BridgeBinding) {
        let f = &self.f;
        let r = (f.hub)
            .submit(
                &Caller::local_owner(),
                CommandEnvelope::new(Command::CreateTask {
                    github: None,
                    agent_chosen: None,
                    mode: None,
                    project_id: self.project_id,
                    kind: TaskKind::Coding,
                    title: None,
                    prompt: "Let's plan the week".into(),
                    bundle: None,
                    harness: HarnessKind::ClaudeCode,
                    model: None,
                    effort: None,
                    launch_policy,
                    branch: None,
                    mission: None,
                    target_id: None,
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
        let task = f.hub.read(|r| r.require("task", task_id)).unwrap();
        (task, binding)
    }
    fn tasks(&self) -> Vec<Task> {
        self.f.hub.read(|r| r.list::<Task>("task")).unwrap()
    }
    fn queue(&self) -> QueueView {
        self.f
            .hub
            .read(|r| r.require("queue", self.project_id))
            .unwrap()
    }
    /// Next up's item for the task linked to issue `number`.
    fn queued(&self, number: u64) -> (Task, QueueItem) {
        let task = (self.tasks().into_iter())
            .find(|t| {
                (t.github.as_deref()).is_some_and(
                    |g| matches!(g.link, GithubLink::Issue { number: n, .. } if n == number),
                )
            })
            .unwrap();
        let item = (self.queue().items.into_iter())
            .find(|i| i.task_id == task.task_id)
            .unwrap();
        (task, item)
    }
}

pub(super) async fn work(
    f: &Fixture,
    binding: &BridgeBinding,
    action: WorkCall,
) -> Result<String, String> {
    match f.hub.handle(binding, BridgeRequest::Work { action }).await {
        BridgeResponse::WorkResult { text } => Ok(text),
        BridgeResponse::Refused { reason } => Err(reason),
        other => panic!("{other:?}"),
    }
}

fn new_issue(title: &str, labels: &[&str]) -> WorkCall {
    WorkCall::NewIssue {
        title: title.into(),
        body: "After a timeout the app asks to sign in again, forever.".into(),
        labels: labels.iter().map(|l| l.to_string()).collect(),
        distinct_from: vec![],
    }
}

fn start_issue(number: u64, start: bool) -> WorkCall {
    WorkCall::StartIssue {
        number,
        agent: None,
        start,
        note: None,
        reviewer: None,
    }
}

#[tokio::test]
async fn talk_files_issues_that_start_nothing_until_the_person_asks() {
    let r = repo(true).await;
    r.set(drafts()).await.unwrap();
    let (talk, binding) = r.talking().await;
    // A Work run with no link is told it can plan, and may.
    let spec = r.f.harness.specs.lock().unwrap().last().cloned().unwrap();
    let told = ports::harness::person_instruction(&spec).unwrap();
    assert!(told.contains("github_new_issue"), "{told}");

    let said = work(&r.f, &binding, WorkCall::Agents).await.unwrap();
    for words in [
        "- Synthetic harness (agent: \"claude_code\"): ready",
        "hasn't chosen a default agent",
        "GitHub: octo/app",
        "stay drafts for the person to merge.\n",
        "Review profiles: default, bugs, security, ui_ux,",
        "You may file 20 issues and start 20",
    ] {
        assert!(said.contains(words), "{words}: {said}");
    }

    r.open_issues(json!([]));
    r.gh.answer(
        "POST /repos/octo/app/issues",
        json!({ "number": 41, "html_url": "https://github.com/octo/app/issues/41" }),
    );
    r.gh.answer(
        "GET /repos/octo/app/labels/agent",
        json!({ "name": "agent" }),
    );
    r.gh.answer("GET /repos/octo/app/labels/bug", json!({ "name": "bug" }));
    let said = work(
        &r.f,
        &binding,
        new_issue("Sign-in loops", &["bug", "wishlist"]),
    )
    .await
    .unwrap();
    assert!(
        said.starts_with("Filed #41: https://github.com/octo/app/issues/41")
            && said.contains("octo/app has no label “wishlist”")
            && said.contains("Nothing has started on it"),
        "{said}"
    );
    let made = r.gh.called("POST /repos/octo/app/issues").unwrap();
    assert_eq!(made["title"], "Sign-in loops");
    let body = made["body"].as_str().unwrap();
    assert!(
        body.starts_with("After a timeout") && body.ends_with("in Øphio, working for @me"),
        "{body}"
    );
    let labelled: Vec<Value> = (r.gh.calls.lock().unwrap().iter())
        .filter(|(c, _)| c == "POST /repos/octo/app/issues/41/labels")
        .map(|(_, v)| v["labels"].clone())
        .collect();
    assert_eq!(labelled, [json!(["agent"]), json!(["bug"])]);
    // Filing started nothing.
    assert_eq!(r.tasks().len(), 1);

    // Asked to start one while Next up is paused, it resumes it and lets
    // the fix start by itself, as the person, once Talk is done.
    r.mine(41, "Sign-in loops");
    let pause = edit(
        Command::QueuePause {
            project_id: r.project_id,
        },
        r.queue().revision,
    );
    let receipt = r.f.hub.submit(&Caller::local_owner(), pause).await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    assert!(r.queue().paused);
    let note = WorkCall::StartIssue {
        number: 41,
        agent: Some(AgentChoice {
            agent: HarnessKind::ClaudeCode,
            model: None,
            effort: None,
        }),
        start: true,
        note: Some("Start with the timeout handler.".into()),
        reviewer: None,
    };
    let said = work(&r.f, &binding, note).await.unwrap();
    assert!(
        said.starts_with("Queued “")
            && said.contains("resumed Next up, which was paused")
            && said.contains("starts by itself once the project is free"),
        "{said}"
    );
    assert!(!r.queue().paused);
    let (fix, item) = r.queued(41);
    assert_eq!(item.authorization, QueueAuthorization::AutoStart);
    let now = r.f.hub.inner.clock.now();
    assert_eq!(
        item.authorization_expires_at,
        Some(Timestamp(now.0 + 24 * 3_600_000))
    );
    assert_eq!(fix.created_by, talk.created_by);
    assert!(fix.current_run.is_none(), "Talk holds the project");
    // It is on #41 already, so it isn't started twice.
    let again = work(&r.f, &binding, start_issue(41, true))
        .await
        .unwrap_err();
    assert!(again.contains("is already on #41"), "{again}");
    // One the person didn't ask to start waits for them.
    r.mine(42, "Export drops rows");
    let said = work(&r.f, &binding, start_issue(42, false)).await.unwrap();
    assert!(
        said.starts_with("Added “") && said.contains("waits there until the person starts it"),
        "{said}"
    );
    let (_, waits) = r.queued(42);
    assert_eq!(waits.authorization, QueueAuthorization::AskBeforeStart);
    assert_eq!(waits.authorization_expires_at, None);
    // Adding it didn't take back what the person asked to start.
    let (_, still) = r.queued(41);
    assert_eq!(still.authorization, QueueAuthorization::AutoStart);
    assert_eq!(
        still.authorization_expires_at,
        Some(Timestamp(now.0 + 24 * 3_600_000))
    );

    // Once Talk ends, the fix starts by itself, told what Talk added.
    let exited = ports::HarnessEvent::Exited {
        exit: ports::RunExit::Completed,
        outcome: None,
    };
    r.f.harness.emit(binding.run_id, exited).await;
    let mut more = waiting();
    while r.get(fix.task_id).current_run.is_none() {
        assert!(more(), "the fix didn't start");
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    let run = run_started(&r.f, fix.task_id).await;
    let spec = r.spec(run);
    assert_eq!(spec.mode, TaskMode::Work);
    assert!(
        spec.prompt.text.starts_with("Fix issue #41.")
            && spec.prompt.text.contains("Start with the timeout handler."),
        "{spec:?}"
    );
    let (waiting_fix, _) = r.queued(42);
    assert!(waiting_fix.current_run.is_none());
    r.f.close().await;
}

#[tokio::test]
async fn work_an_agent_starts_takes_the_persons_launch_flags() {
    use protocol::{agent_preferences::LaunchDefault, model::LaunchPolicy};
    let r = repo(true).await;
    r.set(drafts()).await.unwrap();
    // Without defaults from the person's phone, it starts without flags.
    let (_, binding) = r.talking().await;
    r.mine(41, "Sign-in loops");
    work(&r.f, &binding, start_issue(41, false)).await.unwrap();
    assert_eq!(r.queued(41).0.launch_policy, None);

    // With them, the agent's default is what it starts with, though the
    // task that started it had none.
    let mut command = CommandEnvelope::new(Command::SetLaunchDefaults {
        defaults: vec![LaunchDefault {
            harness: HarnessKind::ClaudeCode,
            policy: LaunchPolicy::BypassPermissions,
        }],
    });
    command.expected_revision = Some(0);
    let receipt = r.f.hub.submit(&Caller::local_owner(), command).await;
    assert_eq!(receipt.error, None);
    r.mine(42, "Export drops rows");
    work(&r.f, &binding, start_issue(42, false)).await.unwrap();
    assert_eq!(
        r.queued(42).0.launch_policy,
        Some(LaunchPolicy::BypassPermissions)
    );
    r.f.close().await;

    // Flags the person ran the same agent with stay ahead of the default.
    let r = repo(true).await;
    r.set(drafts()).await.unwrap();
    let mut command = CommandEnvelope::new(Command::SetLaunchDefaults {
        defaults: vec![LaunchDefault {
            harness: HarnessKind::ClaudeCode,
            policy: LaunchPolicy::BypassPermissions,
        }],
    });
    command.expected_revision = Some(0);
    let receipt = r.f.hub.submit(&Caller::local_owner(), command).await;
    assert_eq!(receipt.error, None);
    let (_, binding) = r.talking_with(Some(LaunchPolicy::AcceptEdits)).await;
    r.mine(43, "Dark mode flickers");
    work(&r.f, &binding, start_issue(43, false)).await.unwrap();
    assert_eq!(
        r.queued(43).0.launch_policy,
        Some(LaunchPolicy::AcceptEdits)
    );
    r.f.close().await;
}

#[tokio::test]
async fn planning_waits_for_the_workflow_and_an_agent_that_can_start() {
    let r = repo(true).await;
    let (_, binding) = r.talking().await;
    let said = work(&r.f, &binding, WorkCall::Agents).await.unwrap();
    assert!(said.contains("The GitHub workflow is off"), "{said}");
    assert!(!said.contains("Review profiles"), "{said}");
    let review = WorkCall::StartReview {
        profile: None,
        folder: None,
        reviewer: None,
        fixer: None,
        max_issues: None,
        then: None,
    };
    for call in [
        new_issue("Sign-in loops", &[]),
        start_issue(41, true),
        review,
    ] {
        let refused = work(&r.f, &binding, call).await.unwrap_err();
        assert!(
            refused.contains("turn it on in Tasks › GitHub"),
            "{refused}"
        );
    }
    assert_eq!(r.times("POST /repos/octo/app/issues"), 0);
    assert_eq!(r.tasks().len(), 1);

    r.set(drafts()).await.unwrap();
    r.mine(41, "Sign-in loops");
    let codex = WorkCall::StartIssue {
        number: 41,
        agent: Some(AgentChoice {
            agent: HarnessKind::Codex,
            model: None,
            effort: None,
        }),
        start: false,
        note: None,
        reviewer: None,
    };
    let refused = work(&r.f, &binding, codex).await.unwrap_err();
    assert!(refused.contains("isn't on this computer"), "{refused}");
    assert_eq!(r.tasks().len(), 1);
    r.f.close().await;
}

#[tokio::test]
async fn a_task_working_on_github_doesnt_plan_other_work() {
    let r = repo(true).await;
    r.set(drafts()).await.unwrap();
    r.mine(9, "Typo in help");
    let (task, binding) = r
        .started(GithubLink::Issue {
            number: 9,
            review_with: None,
        })
        .await;
    let spec = r.f.harness.specs.lock().unwrap().last().cloned().unwrap();
    let told = ports::harness::person_instruction(&spec).unwrap();
    assert!(!told.contains("github_new_issue"), "{told}");
    for action in [
        new_issue("Unrelated work", &[]),
        start_issue(3, true),
        WorkCall::StartReview {
            profile: None,
            folder: None,
            reviewer: None,
            fixer: None,
            max_issues: None,
            then: None,
        },
    ] {
        let refused = work(&r.f, &binding, action).await.unwrap_err();
        assert!(refused.contains("doesn't plan"), "{refused}");
    }
    assert!(r.gh.called("POST /repos/octo/app/issues").is_none());
    assert_eq!(r.tasks().len(), 1, "{task:?}");
    r.f.close().await;
}

#[tokio::test]
async fn a_review_started_from_talk_files_what_it_finds_and_only_that_when_asked() {
    let r = repo(true).await;
    std::fs::create_dir(r.f.project_dir().join("client")).unwrap();
    r.set(drafts()).await.unwrap();
    let (talk, binding) = r.talking().await;
    for most in [0, 21] {
        let over = WorkCall::StartReview {
            profile: None,
            folder: None,
            reviewer: None,
            fixer: None,
            max_issues: Some(most),
            then: None,
        };
        let refused = work(&r.f, &binding, over).await.unwrap_err();
        assert!(refused.contains("files 1 to 20 issues"), "{refused}");
    }
    // A reviewer that uses the app needs a screen only the person can choose.
    let uat = WorkCall::StartReview {
        profile: Some(FindFocus::NoviceUser),
        folder: None,
        reviewer: None,
        fixer: None,
        max_issues: None,
        then: None,
    };
    let refused = work(&r.f, &binding, uat).await.unwrap_err();
    assert!(
        refused.contains("Tasks › Find/Fix › New review"),
        "{refused}"
    );
    assert_eq!(r.tasks().len(), 1);
    let review = WorkCall::StartReview {
        profile: Some(FindFocus::UiUx),
        folder: Some("client".into()),
        reviewer: None,
        fixer: None,
        max_issues: Some(1),
        then: Some(FixFindings::FileOnly),
    };
    let said = work(&r.f, &binding, review).await.unwrap();
    assert!(
        said.contains("files at most 1 issue; then nothing more happens"),
        "{said}"
    );
    let finder = (r.tasks().into_iter())
        .find(|t| t.task_id != talk.task_id)
        .unwrap();
    assert_eq!(finder.created_by, talk.created_by);
    let github = finder.github.as_deref().unwrap();
    let GithubLink::Find {
        scope,
        focus,
        max_issues,
        fix_findings,
        ..
    } = &github.link
    else {
        panic!("{github:?}")
    };
    assert_eq!(
        (scope, *focus, *max_issues, *fix_findings),
        (
            &FindScope::Folder {
                path: "client".into()
            },
            Some(FindFocus::UiUx),
            Some(1),
            Some(FixFindings::FileOnly)
        )
    );
    // It reads alongside Talk, briefed as the profile asks.
    let run = run_started(&r.f, finder.task_id).await;
    let spec = r.spec(run);
    assert_eq!(spec.mode, TaskMode::Plan);
    assert!(
        (spec.github.as_deref()).is_some_and(|t| t.contains("as a UI/UX developer")),
        "{spec:?}"
    );

    r.open_issues(json!([]));
    r.gh.answer(
        "POST /repos/octo/app/issues",
        json!({ "number": 51, "html_url": "https://github.com/octo/app/issues/51" }),
    );
    r.gh.answer("POST /repos/octo/app/labels", json!({}));
    let findings = json!([
        { "title": "Save button hides on small screens", "body": "At 360px it's off screen.", "severity": "high", "confidence": "high" },
        { "title": "No empty state", "body": "The list is blank.", "severity": "low" },
    ]);
    let said = format!("Done.\n\n```ophio-findings\n{findings}\n```\n");
    r.ends(finder.task_id, run, Some(&said)).await;
    // Filed once a verifier, reading no more than the review did, has
    // checked them.
    let verifier = r.confirms(finder.task_id, 2).await;
    assert_eq!(
        r.get(verifier).access,
        r.get(finder.task_id)
            .access
            .map(|a| a.normalized())
            .and_then(|a| crate::access::held_to(Some(&a), true))
    );
    // At most the one asked for is filed, and no fix is queued for it.
    assert_eq!(r.times("POST /repos/octo/app/issues"), 1);
    let made = r.gh.called("POST /repos/octo/app/issues").unwrap();
    assert_eq!(made["title"], "Save button hides on small screens");
    assert_eq!(r.tasks().len(), 3);
    let attention = r.attention(finder.task_id);
    let [found] = &attention[..] else {
        panic!("{attention:?}")
    };
    let body = found.body.as_deref().unwrap();
    assert!(
        body.contains("at most 1 a run") && body.contains("Their fixes weren't queued"),
        "{found:?}"
    );
    r.f.close().await;
}

/// A chat held to the access the person chose plans their GitHub work only
/// as far as that access goes, and what it plans waits for them to start.
#[tokio::test]
async fn a_held_chat_plans_only_within_its_access() {
    use protocol::access::ConversationAccess;
    let backend = Arc::new(crate::tests::conversation_access::Backend::default());
    let r = repo_with(true, |b| b.sandboxes(backend.clone())).await;
    r.set(drafts()).await.unwrap();
    let owner = Caller::local_owner();
    let access = ConversationAccess {
        read: vec![r.project_id],
        internet: false,
        ..Default::default()
    };
    let receipt = (r.f.hub)
        .submit(
            &owner,
            CommandEnvelope::new(Command::CreateTask {
                github: None,
                agent_chosen: None,
                mode: Some(TaskMode::Ask),
                project_id: r.project_id,
                kind: TaskKind::Coding,
                title: Some("Conversation".into()),
                prompt: "Let's plan the week".into(),
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
                browser: None,
                conversation: Some(true),
                access: Some(access.clone()),
                review: None,
            }),
        )
        .await;
    let Some(CommandResult::TaskCreated { task_id, .. }) = receipt.result else {
        panic!("{receipt:?}")
    };
    run_started(&r.f, task_id).await;
    let spec = r.f.harness.specs.lock().unwrap().last().cloned().unwrap();
    assert!(spec.sandbox.is_some());
    let told = ports::harness::person_instruction(&spec).unwrap();
    assert!(
        told.contains("invite_person") && told.contains("Jev"),
        "{told}"
    );
    let bridge = spec.bridge.expect("a bridge");
    let token = (bridge.env.iter())
        .find(|(key, _)| key == ports::bridge::RUN_TOKEN_ENV)
        .map(|(_, token)| token.clone())
        .unwrap();
    let binding = r.f.hub.bind(&token).await.expect("bridge binding");
    let review = |then| WorkCall::StartReview {
        profile: None,
        folder: None,
        reviewer: None,
        fixer: None,
        max_issues: None,
        then,
    };
    // Without Internet, nothing goes to GitHub.
    for call in [new_issue("Sign-in loops", &[]), review(None)] {
        let refused = work(&r.f, &binding, call).await.unwrap_err();
        assert!(
            refused.contains("hasn't given this chat Internet"),
            "{refused}"
        );
    }
    assert_eq!(r.times("POST /repos/octo/app/issues"), 0);

    let receipt = (r.f.hub)
        .submit(
            &owner,
            CommandEnvelope::new(Command::SetConversationAccess {
                task_id,
                access: ConversationAccess {
                    internet: true,
                    ..access
                },
            }),
        )
        .await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    // With Internet, it plans, but what it starts waits for the person.
    r.mine(9, "Typo in help");
    let said = work(&r.f, &binding, start_issue(9, true)).await.unwrap();
    assert!(said.contains("waits for them to start it"), "{said}");
    let (_, item) = r.queued(9);
    assert_eq!(item.authorization, QueueAuthorization::AskBeforeStart);
    work(&r.f, &binding, review(Some(FixFindings::ByItself)))
        .await
        .unwrap();
    let finder = (r.tasks().into_iter())
        .find(|t| (t.github.as_deref()).is_some_and(|g| matches!(g.link, GithubLink::Find { .. })))
        .unwrap();
    let Some(GithubLink::Find { fix_findings, .. }) = finder.github.as_deref().map(|g| &g.link)
    else {
        unreachable!()
    };
    assert_eq!(*fix_findings, Some(FixFindings::AskFirst));
    let run = run_started(&r.f, finder.task_id).await;
    let spec = r.spec(run);
    assert_eq!(
        finder.access,
        Some(ConversationAccess {
            read: vec![r.project_id],
            internet: true,
            ..Default::default()
        }),
        "an automatically started review must retain the chat's read restrictions"
    );
    assert!(spec.sandbox.is_some(), "the review must run in a sandbox");
    r.f.close().await;
}

#[tokio::test]
async fn a_review_cannot_name_a_folder_outside_its_project() {
    let r = repo(true).await;
    r.set(drafts()).await.unwrap();
    let (_, binding) = r.talking().await;
    let private = tempfile::tempdir().unwrap();
    std::fs::write(private.path().join("secret"), "synthetic private content").unwrap();
    for folder in [
        private.path().to_string_lossy().into_owned(),
        "../private".into(),
        "client/../../private".into(),
        "client/../private".into(),
        r"..\private".into(),
        r"C:\private".into(),
        r"C:private".into(),
        r"\\server\private".into(),
        "".into(),
        "missing".into(),
        ".git/config".into(),
    ] {
        let call = WorkCall::StartReview {
            profile: None,
            folder: Some(folder.clone()),
            reviewer: None,
            fixer: None,
            max_issues: None,
            then: None,
        };
        let result = work(&r.f, &binding, call).await;
        assert!(
            result.is_err(),
            "review scope {folder:?} was accepted: {result:?}"
        );
        assert_eq!(r.tasks().len(), 1, "a refused review must create no task");
    }
    r.f.close().await;
}

#[cfg(unix)]
#[tokio::test]
async fn a_review_folder_must_resolve_inside_the_project() {
    let r = repo(true).await;
    r.set(drafts()).await.unwrap();
    let (_, binding) = r.talking().await;
    let private = tempfile::tempdir().unwrap();
    let project = r.f.project_dir();
    std::fs::create_dir(project.join("src")).unwrap();
    std::os::unix::fs::symlink(private.path(), project.join("outside")).unwrap();
    std::os::unix::fs::symlink("src", project.join("inside")).unwrap();
    std::os::unix::fs::symlink(private.path().join("missing"), project.join("dangling")).unwrap();
    let review = |path: &str| WorkCall::StartReview {
        profile: None,
        folder: Some(path.into()),
        reviewer: None,
        fixer: None,
        max_issues: None,
        then: None,
    };
    for path in ["outside", "outside/missing", "dangling"] {
        let refused = work(&r.f, &binding, review(path)).await.unwrap_err();
        assert!(refused.contains("inside the project"), "{refused}");
        assert_eq!(r.tasks().len(), 1);
    }
    work(&r.f, &binding, review("inside")).await.unwrap();
    let finder = r.tasks().into_iter().find(|t| t.github.is_some()).unwrap();
    let run = run_started(&r.f, finder.task_id).await;
    assert_eq!(r.spec(run).mode, TaskMode::Plan);
    r.f.close().await;
}

/// Exercise the dispatched review's real sandbox with synthetic files and a
/// shell, without launching an agent or contacting GitHub. Build the sandbox
/// shim first and point OPHIO_TEST_SANDBOX_SHIM at that executable.
#[cfg(target_os = "linux")]
#[tokio::test]
#[ignore = "requires user namespaces and OPHIO_TEST_SANDBOX_SHIM"]
async fn a_restricted_review_cannot_read_private_files_with_native_tools() {
    use ports::sandbox::{Sandbox, SandboxSpec, Sandboxes, WrapRequest};
    use protocol::access::ConversationAccess;
    #[derive(Debug)]
    struct NativeSandbox {
        backend: sandbox::Bubblewrap,
        state: tempfile::TempDir,
    }
    impl Sandboxes for NativeSandbox {
        fn availability(&self) -> Availability {
            self.backend.availability()
        }
        fn create(&self, mut spec: SandboxSpec) -> Result<Arc<dyn Sandbox>, ports::HarnessError> {
            // The service fixture's state path has parent segments and is
            // too long for Unix sockets. Relocate only state, preserving grants.
            spec.state = self.state.path().to_owned();
            self.backend.create(spec)
        }
    }
    let shim = std::env::var_os("OPHIO_TEST_SANDBOX_SHIM").expect("build the sandbox shim first");
    let backend = Arc::new(NativeSandbox {
        backend: sandbox::Bubblewrap::new(shim),
        state: tempfile::tempdir().unwrap(),
    });
    let r = repo_with(true, |b| b.sandboxes(backend)).await;
    r.set(drafts()).await.unwrap();
    let (mut talk, _) = r.talking().await;
    // The calling agent is synthetic; only the review's native read runs.
    talk.access = Some(
        ConversationAccess {
            write: vec![r.project_id],
            ..Default::default()
        }
        .normalized(),
    );
    let private = tempfile::tempdir().unwrap();
    std::fs::write(private.path().join("secret"), "synthetic secret").unwrap();
    std::fs::write(r.f.project_dir().join("permitted"), "synthetic source").unwrap();
    r.f.hub
        .work_request(
            talk.current_run.unwrap(),
            &talk,
            WorkCall::StartReview {
                profile: None,
                folder: None,
                reviewer: None,
                fixer: None,
                max_issues: None,
                then: Some(FixFindings::ByItself),
            },
        )
        .await
        .unwrap();
    let finder = r.tasks().into_iter().find(|t| t.github.is_some()).unwrap();
    assert_eq!(
        finder.access,
        Some(ConversationAccess {
            read: vec![r.project_id],
            ..Default::default()
        })
    );
    let run = run_started(&r.f, finder.task_id).await;
    let spec = r.spec(run);
    let sandbox = spec
        .sandbox
        .expect("the dispatched review must be sandboxed");
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir(home.path().join(".claude")).unwrap();
    let env = ports::RunEnv(vec![(
        "HOME".into(),
        home.path().to_string_lossy().into_owned(),
    )]);
    let wrapped = sandbox
        .wrap(WrapRequest {
            program: Path::new("/bin/sh"),
            visible: &[],
            env: &env,
            resume: None,
        })
        .unwrap();
    let mut command = tokio::process::Command::new(wrapped.program);
    command
        .args(wrapped.args)
        .args([
            "/bin/sh",
            "-c",
            "cat \"$1/permitted\" && ! cat \"$2/secret\" && ! (echo changed > \"$1/permitted\")",
            "review-read-check",
        ])
        .arg(&spec.cwd)
        .arg(private.path())
        .envs(env.0)
        .envs(wrapped.env)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    sandbox.prepare(command.as_std_mut()).unwrap();
    let child = command.spawn().unwrap();
    sandbox.started().await.unwrap();
    let output = child.wait_with_output().await.unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"synthetic source");
    assert_eq!(
        std::fs::read(private.path().join("secret")).unwrap(),
        b"synthetic secret"
    );
    assert_eq!(
        std::fs::read(spec.cwd.join("permitted")).unwrap(),
        b"synthetic source"
    );
    r.f.close().await;
}

#[tokio::test]
async fn stopped_runs_and_computer_specialists_cannot_ask_agents() {
    for computer_use in [false, true] {
        let r = repo(true).await;
        let (task, binding) = if computer_use {
            let (task_id, _) =
                r.f.task(
                    r.project_id,
                    TaskPlacement::StartNow,
                    &Caller::local_owner(),
                    true,
                )
                .await;
            run_started(&r.f, task_id).await;
            let token =
                r.f.harness
                    .specs
                    .lock()
                    .unwrap()
                    .last()
                    .unwrap()
                    .bridge
                    .as_ref()
                    .unwrap()
                    .env
                    .iter()
                    .find(|(key, _)| key == ports::bridge::RUN_TOKEN_ENV)
                    .unwrap()
                    .1
                    .clone();
            let binding = r.f.hub.bind(&token).await.unwrap();
            let task = r.f.hub.read(|r| r.require("task", task_id)).unwrap();
            (task, binding)
        } else {
            let (task, binding) = r.talking().await;
            r.f.hub
                .write(|tx| tx.put("stopped", binding.run_id, &true))
                .unwrap();
            (task, binding)
        };
        assert_eq!(binding.computer_use, computer_use);
        for action in [
            WorkCall::Agents,
            WorkCall::Ask {
                agent: AgentChoice {
                    agent: HarnessKind::ClaudeCode,
                    model: None,
                    effort: None,
                },
                prompt: "Review the change".into(),
                title: None,
            },
        ] {
            let refused = work(&r.f, &binding, action).await.unwrap_err();
            let reason = if computer_use {
                "doesn't plan"
            } else {
                "no longer active"
            };
            assert!(refused.contains(reason), "{refused}");
        }
        assert_eq!(r.tasks().len(), 1, "{task:?}");
        r.f.close().await;
    }
}
