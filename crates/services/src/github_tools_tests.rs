// SPDX-License-Identifier: Apache-2.0
//! What a run linked to GitHub posts through its bridge, against GitHub as
//! recorded.
use super::*;
use crate::github::segment;
use crate::github_watch::{DECIDING, Deciding, HANDOVER, REVIEW, deciding_key};
use ports::accounts::MASK;
use ports::bridge::{BridgeBinding, BridgeHandler, BridgeRequest, BridgeResponse, GithubCall};

impl Repo {
    /// A run of a task linked to `link`, started now, and its bridge.
    pub(super) async fn started(&self, link: GithubLink) -> (Task, BridgeBinding) {
        let f = &self.f;
        let r = (f.hub)
            .submit(
                &Caller::local_owner(),
                CommandEnvelope::new(Command::CreateTask {
                    github: Some(Box::new(link)),
                    agent_chosen: None,
                    mode: None,
                    project_id: self.project_id,
                    kind: TaskKind::Coding,
                    title: None,
                    prompt: "Fix it".into(),
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
        (self.task(task_id), binding)
    }
    fn task(&self, task_id: TaskId) -> Task {
        self.f.hub.read(|r| r.require("task", task_id)).unwrap()
    }
    /// An open issue of the person's own.
    pub(super) fn mine(&self, number: u64, title: &str) {
        self.gh.answer(
            &format!("GET /repos/octo/app/issues/{number}"),
            json!({
                "number": number, "state": "open", "title": title,
                "user": { "login": "me" }, "author_association": "OWNER",
            }),
        );
    }
    pub(super) fn times(&self, call: &str) -> usize {
        let calls = self.gh.calls.lock().unwrap();
        calls.iter().filter(|(c, _)| c == call).count()
    }
    /// Checks out `branch` in the project, as the agent would.
    fn on(&self, branch: &str) {
        let status = std::process::Command::new("git")
            .args(["symbolic-ref", "HEAD", &format!("refs/heads/{branch}")])
            .current_dir(self.f.project_dir())
            .status()
            .unwrap();
        assert!(status.success());
    }
}

pub(super) async fn call(
    f: &Fixture,
    binding: &BridgeBinding,
    action: GithubCall,
) -> Result<String, String> {
    result(
        f.hub
            .handle(binding, BridgeRequest::Github { action })
            .await,
    )
}
/// Calls in the background, for an answer that waits on the person.
fn calling(
    f: &Fixture,
    binding: &BridgeBinding,
    action: GithubCall,
) -> tokio::task::JoinHandle<BridgeResponse> {
    let hub = f.hub.clone();
    let binding = binding.clone();
    tokio::spawn(async move { hub.handle(&binding, BridgeRequest::Github { action }).await })
}
fn result(response: BridgeResponse) -> Result<String, String> {
    match response {
        BridgeResponse::GithubResult { text } => Ok(text),
        BridgeResponse::Refused { reason } => Err(reason),
        other => panic!("{other:?}"),
    }
}
async fn pending(f: &Fixture) -> Approval {
    let mut more = waiting();
    loop {
        let found = (f.hub.read(|r| r.list::<Approval>("approval")).unwrap())
            .into_iter()
            .find(|a| a.kind == ApprovalKind::GithubPost && a.state == ApprovalState::Pending);
        if let Some(found) = found {
            return found;
        }
        assert!(more(), "no GitHub post approval");
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
}
async fn decide(f: &Fixture, approval: &Approval, decision: ApprovalDecision) {
    (f.hub)
        .resolve_approval(&Caller::local_owner(), approval.approval_id, &decision)
        .await
        .unwrap();
}
/// A token of GitHub's form, made here so no scanner takes it for one.
fn token() -> String {
    format!("ghp_{}", "a1".repeat(18))
}

#[test]
fn github_tokens_are_hidden_where_a_word_starts_with_one() {
    let pat = format!("github_pat_{}", "B2_".repeat(10));
    let joined = format!("xghp_{}", "c".repeat(30));
    let hidden = crate::accounts::hide_github_tokens(&format!(
        "push {} ({pat}), {joined} ghp_short",
        token()
    ));
    assert_eq!(hidden, format!("push {MASK} ({MASK}), {joined} ghp_short"));
}

#[tokio::test]
async fn a_linked_run_opens_its_draft_pull_request_and_keeps_it_current() {
    let r = repo(true).await;
    r.set(drafts()).await.unwrap();
    r.mine(12, "Crash on save");
    let (task, binding) = r
        .started(GithubLink::Issue {
            number: 12,
            review_with: None,
        })
        .await;
    assert!(binding.github);
    {
        let specs = r.f.harness.specs.lock().unwrap();
        let spec = specs.last().unwrap();
        assert!(spec.bridge.as_ref().unwrap().github);
        let told = spec.github.as_deref().unwrap();
        assert!(
            told.contains("octo/app") && told.contains("issue #12"),
            "{told}"
        );
        let env = |name: &str| {
            (spec.env.0.iter())
                .find(|(n, _)| n == name)
                .map(|(_, v)| v.clone())
        };
        // Its `gh` comes first, and its pushes pass the check.
        let path = env("PATH").unwrap();
        let shim = std::env::split_paths(&path).next().unwrap();
        assert!(shim.ends_with("github-shim") && shim.join("gh").is_file());
        let key = (spec.env.0.iter())
            .find(|(_, v)| v == "hook.ophio.command")
            .map(|(n, _)| n.replace("KEY", "VALUE"))
            .unwrap();
        assert!(
            env(&key)
                .unwrap()
                .ends_with(" pre-push 'main' 'https://github.com/octo/app'")
        );
    }
    let head = task.branch.as_ref().unwrap().name.clone();
    r.on(&head);
    let pr = || GithubCall::Pr {
        title: "fix: saving no longer crashes".into(),
        body: format!(
            "Problem\n\nIt pushed with {}.\n🤖 Claude Code in Øphio, working for @someone",
            token()
        ),
    };
    let e = call(&r.f, &binding, pr()).await.unwrap_err();
    assert!(e.contains(&format!("git push -u origin {head}")), "{e}");

    let branch = segment(&head);
    r.gh.answer(&format!("GET /repos/octo/app/branches/{branch}"), json!({}));
    r.gh.answer(
        &format!("GET /repos/octo/app/pulls?head=octo:{branch}&state=open&per_page=5"),
        json!([]),
    );
    let opened = json!({
        "number": 21, "state": "open", "draft": true,
        "html_url": "https://github.com/octo/app/pull/21",
        "user": { "login": "me" }, "author_association": "OWNER", "head": { "ref": head, "sha": "aaa111" },
    });
    r.gh.answer("POST /repos/octo/app/pulls", opened.clone());
    r.gh.answer(
        "GET /repos/octo/app/labels/agent",
        json!({ "name": "agent" }),
    );
    assert_eq!(
        call(&r.f, &binding, pr()).await.unwrap(),
        "Opened draft pull request #21: https://github.com/octo/app/pull/21"
    );
    let sent = r.gh.called("POST /repos/octo/app/pulls").unwrap();
    assert_eq!(
        (&sent["draft"], &sent["base"], &sent["head"]),
        (&json!(true), &json!("main"), &json!(head))
    );
    // Its keys are hidden, and the only disclosure is Øphio's.
    let body = sent["body"].as_str().unwrap();
    assert!(!body.contains(&token()) && body.contains(MASK), "{body}");
    assert!(
        body.ends_with("\n\n🤖 Claude Code in Øphio, working for @me"),
        "{body}"
    );
    assert_eq!(body.matches('🤖').count(), 1, "{body}");
    assert_eq!(
        r.gh.called("POST /repos/octo/app/issues/21/labels")
            .unwrap(),
        json!({ "labels": ["agent"] })
    );
    let github = r.task(task.task_id).github.unwrap();
    assert_eq!(github.pr.unwrap().number, 21);
    assert_eq!(github.stage, Some(PrStage::Babysitting));

    // Called again after a push, it edits the same one, which keeps its
    // check.
    r.alter(task.task_id, |g| {
        g.checked_by = Some(claude());
        g.rounds = 2;
    });
    r.gh.answer("GET /repos/octo/app/pulls/21", opened.clone());
    r.gh.answer("PATCH /repos/octo/app/pulls/21", opened.clone());
    assert_eq!(
        call(&r.f, &binding, pr()).await.unwrap(),
        "Updated pull request #21: https://github.com/octo/app/pull/21"
    );
    assert_eq!(r.times("POST /repos/octo/app/pulls"), 1);
    let github = r.task(task.task_id).github.unwrap();
    assert_eq!((github.checked_by, github.rounds), (Some(claude()), 2));

    // A new head is babysat again, but a hold, or a check, put on
    // meanwhile stays.
    let mut pushed = opened;
    for (head, before, after) in [
        ("bbb222", PrStage::Capped, PrStage::Capped),
        ("ccc333", PrStage::Checking, PrStage::Checking),
        ("ddd444", PrStage::Ready, PrStage::Babysitting),
    ] {
        r.alter(task.task_id, |g| g.stage = Some(before));
        pushed["head"]["sha"] = json!(head);
        r.gh.answer("GET /repos/octo/app/pulls/21", pushed.clone());
        r.gh.answer("PATCH /repos/octo/app/pulls/21", pushed.clone());
        call(&r.f, &binding, pr()).await.unwrap();
        let github = r.task(task.task_id).github.unwrap();
        assert_eq!(github.pr.unwrap().head, head);
        assert_eq!(
            (github.stage, github.checked_by),
            (Some(after), Some(claude()))
        );
    }
    r.gh.answer(
        "POST /repos/octo/app/issues/21/comments",
        json!({ "html_url": "https://github.com/octo/app/pull/21#issuecomment-1" }),
    );
    let comment = GithubCall::Comment {
        body: "The lint failure was real; fixed.".into(),
        thread: None,
        resolve: false,
    };
    call(&r.f, &binding, comment.clone()).await.unwrap();
    assert_eq!(
        r.gh.called("POST /repos/octo/app/issues/21/comments")
            .unwrap(),
        json!({ "body": "🤖 Agent (Claude Code in Øphio) for @me\n\nThe lint failure was real; fixed." })
    );
    // In Drafts the person marks it ready and merges it.
    for refused in [
        GithubCall::Ready,
        GithubCall::Merge {
            expected_head: "aaa111".into(),
        },
    ] {
        let e = call(&r.f, &binding, refused).await.unwrap_err();
        assert!(e.contains("In Drafts"), "{e}");
    }
    // Another pull request, from another branch, starts over: its own
    // check, its own rounds, nothing of the last one's to hand over or
    // waiting on, and no hold put on the last one. A check under way on
    // the last one stays that one's.
    let deciding = Deciding {
        check: TaskId::new(),
        run: None,
        at: r.f.hub.inner.clock.now(),
        fixer: Some(task.task_id),
        repo: "octo/app".into(),
    };
    let on_21 = deciding_key(r.project_id, "octo/app", 21);
    (r.f.hub.write(|tx| {
        tx.put(DECIDING, &on_21, &deciding)?;
        tx.put(REVIEW, task.task_id, &"Rename it.".to_string())
    }))
    .unwrap();
    r.alter(task.task_id, |g| {
        g.stage = Some(PrStage::Capped);
        g.approval = Some(protocol::review::ApprovalNeed {
            areas: vec![protocol::review::SensitiveArea::Signing],
            paths: vec!["app.jks".into()],
            head: "ddd444".into(),
        });
    });
    r.on("save-fix-2");
    r.gh.answer("GET /repos/octo/app/branches/save-fix-2", json!({}));
    r.gh.answer(
        "GET /repos/octo/app/pulls?head=octo:save-fix-2&state=open&per_page=5",
        json!([]),
    );
    r.gh.answer(
        "POST /repos/octo/app/pulls",
        json!({
            "number": 22, "state": "open", "draft": true,
            "html_url": "https://github.com/octo/app/pull/22",
            "user": { "login": "me" }, "author_association": "OWNER", "head": { "ref": "save-fix-2", "sha": "bbb222" },
        }),
    );
    assert_eq!(
        call(&r.f, &binding, pr()).await.unwrap(),
        "Opened draft pull request #22: https://github.com/octo/app/pull/22"
    );
    let github = r.task(task.task_id).github.unwrap();
    assert_eq!(
        (github.pr.unwrap().number, github.checked_by, github.rounds),
        (22, None, 0)
    );
    assert_eq!(github.approval, None);
    assert_eq!(github.stage, Some(PrStage::Babysitting));
    let review: Option<String> = r.f.hub.read(|x| x.get(REVIEW, task.task_id)).unwrap();
    assert!(review.is_none());
    let now = r.task(task.task_id);
    assert!(
        (r.f.hub.read(|x| crate::github_watch::deciding(x, &now, 22)))
            .unwrap()
            .is_none()
    );
    let kept: Option<Deciding> = r.f.hub.read(|x| x.get(DECIDING, &on_21)).unwrap();
    assert_eq!(kept.unwrap().check, deciding.check);
    // Turned off, the run's tools stop with it.
    let off = GithubSettings {
        mode: GithubMode::Off,
        ..Default::default()
    };
    r.set(off).await.unwrap();
    let e = call(&r.f, &binding, comment).await.unwrap_err();
    assert!(e.contains("off for this project"), "{e}");
    r.f.close().await;
}

#[tokio::test]
async fn a_runs_tools_stop_once_origin_names_another_repository() {
    let r = repo(true).await;
    r.set(drafts()).await.unwrap();
    r.gh.answer("GET /repos/octo/app/pulls/7", pull(7, "octo/app", "me"));
    let (task, binding) = r.started(GithubLink::Pr { number: 7 }).await;
    assert_eq!(task.github.unwrap().repo.as_deref(), Some("octo/app"));
    r.origin("https://github.com/octo/other.git");
    let comment = GithubCall::Comment {
        body: "Done.".into(),
        thread: None,
        resolve: false,
    };
    let e = call(&r.f, &binding, comment).await.unwrap_err();
    assert!(
        e.contains("linked on octo/app") && e.contains("octo/other now"),
        "{e}"
    );
    assert_eq!(r.times("GET /repos/octo/other"), 0);
    r.f.close().await;
}

#[tokio::test]
async fn another_persons_pull_request_keeps_their_description() {
    let r = repo(true).await;
    r.set(drafts()).await.unwrap();
    r.gh.answer(
        "GET /repos/octo/app/pulls/7",
        pull(7, "octo/app", "teammate"),
    );
    r.gh.answer(
        "GET /repos/octo/app/collaborators/teammate/permission",
        json!({ "permission": "write" }),
    );
    let (task, binding) = r.started(GithubLink::Pr { number: 7 }).await;
    assert_eq!(task.mode, TaskMode::Work);
    r.on("save-fix");
    r.gh.answer("GET /repos/octo/app/branches/save-fix", json!({}));
    let pr = GithubCall::Pr {
        title: "fix: atomic saves".into(),
        body: "Rewritten.".into(),
    };
    let e = call(&r.f, &binding, pr).await.unwrap_err();
    assert!(
        e.contains("@teammate's") && e.contains("github_comment"),
        "{e}"
    );
    assert!(r.gh.called("PATCH /repos/octo/app/pulls/7").is_none());
    r.f.close().await;
}

#[tokio::test]
async fn findings_are_filed_once_each_and_within_the_runs_limit() {
    let r = repo(true).await;
    let settings = GithubSettings {
        workflow: GithubWorkflow {
            findings_per_run: 2,
            ..Default::default()
        },
        ..drafts()
    };
    r.set(settings).await.unwrap();
    r.mine(12, "Crash on save");
    let (_, binding) = r
        .started(GithubLink::Issue {
            number: 12,
            review_with: None,
        })
        .await;
    let issue = |title: &str, security| GithubCall::Issue {
        title: title.into(),
        body: "Steps: open, save.".into(),
        security,
        distinct_from: vec![],
    };
    // An open issue saying the same isn't filed again, or counted.
    r.open_issues(json!([{
        "number": 5, "title": "export  drops ROWS", "state": "open",
        "html_url": "https://github.com/octo/app/issues/5",
    }]));
    let text = call(&r.f, &binding, issue("Export drops rows", false)).await;
    assert_eq!(
        text.unwrap(),
        "Not filed: #5 already says this (https://github.com/octo/app/issues/5)."
    );
    assert!(r.gh.called("POST /repos/octo/app/issues").is_none());

    r.gh.answer(
        "POST /repos/octo/app/issues",
        json!({ "number": 31, "html_url": "https://github.com/octo/app/issues/31" }),
    );
    r.gh.answer("POST /repos/octo/app/labels", json!({}));
    let text = call(&r.f, &binding, issue("Undo loses the cursor", false)).await;
    assert_eq!(
        text.unwrap(),
        "Filed #31: https://github.com/octo/app/issues/31"
    );
    let made = r.gh.called("POST /repos/octo/app/issues").unwrap();
    assert_eq!(
        made["body"],
        "Steps: open, save.\n\n🤖 Found by Claude Code in Øphio, working for @me"
    );
    // Its labels are made on first use, in the person's own repository.
    assert_eq!(r.times("POST /repos/octo/app/labels"), 2);
    assert_eq!(
        r.gh.called("POST /repos/octo/app/issues/31/labels")
            .unwrap(),
        json!({ "labels": ["agent", "agent-found"] })
    );

    // A security finding on a public repository goes privately.
    r.gh.answer(
        "POST /repos/octo/app/security-advisories/reports",
        json!({ "html_url": "https://github.com/octo/app/security/advisories/GHSA-1" }),
    );
    let open = "GET /repos/octo/app/issues?state=open&sort=updated&per_page=100&page=1";
    let read = r.times(open);
    let text = call(&r.f, &binding, issue("Token in the logs", true)).await;
    assert!(
        text.unwrap()
            .starts_with("Reported privately to octo/app's maintainers")
    );
    assert_eq!(r.times(open), read);

    let e = call(&r.f, &binding, issue("One more", false))
        .await
        .unwrap_err();
    assert!(e.contains("filed its 2 findings"), "{e}");
    r.f.close().await;
}

#[tokio::test]
async fn issues_that_read_alike_are_read_before_one_is_filed() {
    let r = repo(true).await;
    r.set(drafts()).await.unwrap();
    r.mine(12, "Crash on save");
    let (_, binding) = r
        .started(GithubLink::Issue {
            number: 12,
            review_with: None,
        })
        .await;
    let issue = |title: &str, distinct_from: &[u64]| GithubCall::Issue {
        title: title.into(),
        body: "SUN_LEN is 108 bytes.".into(),
        security: false,
        distinct_from: distinct_from.to_vec(),
    };
    // The clock reads 2023-11-14T22:13:20Z.
    r.gh.answer(
        "GET /repos/octo/app/issues?state=open&sort=updated&per_page=100&page=1",
        json!([
            { "number": 18, "title": "Force-stop tests fail when the temporary directory path is long",
              "state": "open", "html_url": "https://github.com/octo/app/issues/18" },
            { "number": 3, "title": "Dark mode", "state": "open",
              "html_url": "https://github.com/octo/app/issues/3" },
        ]),
    );
    r.gh.answer(
        "GET /repos/octo/app/issues?state=closed&sort=updated&per_page=100&page=1",
        json!([
            { "number": 9, "title": "Force stop tests fail on long temp paths", "state": "closed",
              "html_url": "https://github.com/octo/app/issues/9",
              "closed_at": "2023-11-10T00:00:00Z", "updated_at": "2023-11-10T00:00:00Z" },
        ]),
    );
    r.gh.answer(
        "POST /repos/octo/app/issues",
        json!({ "number": 21, "html_url": "https://github.com/octo/app/issues/21" }),
    );
    r.gh.answer("POST /repos/octo/app/labels", json!({}));

    // Said in other words, the agent is shown them to read, and nothing is
    // filed or counted.
    let title = "Force-stop tests fail when the temporary directory has a long path";
    let text = call(&r.f, &binding, issue(title, &[])).await.unwrap();
    assert!(
        text.starts_with("Not filed yet: these issues may say this"),
        "{text}"
    );
    assert!(
        text.contains(
            "- #18 `Force-stop tests fail when the temporary directory path is long` (open)"
        ) && text.contains("- #9 `Force stop tests fail on long temp paths` (closed lately)")
            && text.contains("`distinct_from`")
            && !text.contains("#3"),
        "{text}"
    );
    assert!(
        text.find("#18").unwrap() < text.find("#9").unwrap(),
        "{text}"
    );
    assert!(r.gh.called("POST /repos/octo/app/issues").is_none());

    // Having read only one of them, it's shown the other.
    let text = call(&r.f, &binding, issue(title, &[18])).await.unwrap();
    assert!(text.contains("#9") && !text.contains("#18"), "{text}");
    // An issue with its very title is never filed again, read or not.
    let same = "Force-stop tests fail when the temporary directory path is long";
    let text = call(&r.f, &binding, issue(same, &[18, 9])).await.unwrap();
    assert_eq!(
        text,
        "Not filed: #18 has this title already. If yours is a different problem, give it a title that says how."
    );
    assert!(r.gh.called("POST /repos/octo/app/issues").is_none());

    // Read and found to be another problem, it's filed.
    let text = call(&r.f, &binding, issue(title, &[9, 18])).await.unwrap();
    assert_eq!(text, "Filed #21: https://github.com/octo/app/issues/21");
    assert_eq!(r.times("POST /repos/octo/app/issues"), 1);
    r.f.close().await;
}

#[tokio::test]
async fn elsewhere_each_post_waits_for_the_person() {
    let r = repo(false).await;
    r.set(drafts()).await.unwrap();
    r.mine(12, "Crash on save");
    let (task, binding) = r
        .started(GithubLink::Issue {
            number: 12,
            review_with: None,
        })
        .await;
    let comments = "POST /repos/octo/app/issues/12/comments";
    let comment = GithubCall::Comment {
        body: "Can't see it on 1.2.".into(),
        thread: None,
        resolve: false,
    };
    let waiting = calling(&r.f, &binding, comment);
    let approval = pending(&r.f).await;
    assert_eq!(approval.title, "Post on octo/app as @me?");
    let detail = approval.detail.clone().unwrap();
    assert!(detail.contains("isn't yours") && detail.contains("Can't see it on 1.2."));
    decide(&r.f, &approval, ApprovalDecision::Deny { reason: None }).await;
    let e = result(waiting.await.unwrap()).unwrap_err();
    assert!(e.contains("declined"), "{e}");
    assert!(r.gh.called(comments).is_none());

    r.gh.answer(
        comments,
        json!({ "html_url": "https://github.com/octo/app/issues/12#c" }),
    );
    let tried = GithubCall::CannotReproduce {
        tried: "Saved 40 files with ÜTF names.".into(),
        already_fixed: false,
    };
    let waiting = calling(&r.f, &binding, tried);
    let approval = pending(&r.f).await;
    decide(&r.f, &approval, ApprovalDecision::Allow).await;
    let text = result(waiting.await.unwrap()).unwrap();
    assert!(text.contains("Stop here"), "{text}");
    let posted = r.gh.called(comments).unwrap();
    assert!(
        posted["body"]
            .as_str()
            .unwrap()
            .contains("Couldn't reproduce this. What was tried:\n\nSaved 40 files"),
        "{posted}"
    );
    // Labels aren't made in a repository that isn't theirs.
    assert!(r.gh.called("POST /repos/octo/app/labels").is_none());
    let github = r.task(task.task_id).github.unwrap();
    assert_eq!(github.stage, Some(PrStage::NotReproduced));
    // What it found is kept for the phone, in its own words.
    assert_eq!(
        github.finding.as_deref(),
        Some("Saved 40 files with ÜTF names.")
    );
    assert!(!github.already_fixed);
    let item = r.waiting_on(task.task_id).unwrap();
    assert_eq!(item.title, "Couldn't reproduce #12");
    assert_eq!(item.resolved_at, None);
    r.f.close().await;
}

impl Repo {
    /// What the task asked of the person, still open or not.
    fn waiting_on(&self, task_id: TaskId) -> Option<AttentionItem> {
        let items = (self.f.hub)
            .read(|x| x.list::<AttentionItem>("attention"))
            .unwrap();
        (items.into_iter())
            .find(|i| i.task_id == Some(task_id) && i.kind == AttentionKind::WaitingForYou)
    }
    async fn found_fixed(&self, binding: &BridgeBinding, number: u64, already_fixed: bool) {
        self.gh.answer(
            &format!("POST /repos/octo/app/issues/{number}/comments"),
            json!({ "html_url": format!("https://github.com/octo/app/issues/{number}#c") }),
        );
        let found = GithubCall::CannotReproduce {
            tried: "Fixed by #27, which made the socket path short.".into(),
            already_fixed,
        };
        let text = call(&self.f, binding, found).await.unwrap();
        assert!(text.contains("Stop here"), "{text}");
    }
}

#[tokio::test]
async fn an_issue_found_fixed_already_is_closed_in_the_persons_name() {
    let r = repo(true).await;
    r.set(drafts()).await.unwrap();
    r.mine(12, "Crash on save");
    let (task, binding) = r
        .started(GithubLink::Issue {
            number: 12,
            review_with: None,
        })
        .await;
    r.found_fixed(&binding, 12, true).await;
    let posted =
        r.gh.called("POST /repos/octo/app/issues/12/comments")
            .unwrap();
    let body = posted["body"].as_str().unwrap();
    assert!(
        body.contains("This looks fixed already. What was checked:\n\nFixed by #27"),
        "{body}"
    );
    let github = r.task(task.task_id).github.unwrap();
    assert!(github.already_fixed);
    assert_eq!(
        r.waiting_on(task.task_id).unwrap().title,
        "#12 looks fixed already: close it?"
    );

    // A pull request isn't closed as an issue.
    r.gh.answer(
        "GET /repos/octo/app/issues/13",
        json!({ "number": 13, "state": "open", "pull_request": {}, "user": { "login": "me" } }),
    );
    let close = |number: u64, repo: &str| GithubAction::CloseIssue {
        project_id: r.project_id,
        repo: repo.into(),
        number,
        done: true,
    };
    let owner = Caller::local_owner();
    let e = r.act(&owner, close(13, "octo/app")).await.unwrap_err();
    assert_eq!(e, ErrorCode::InvalidRequest);
    // Nor one in a repository the person no longer has in front of them.
    let e = r.act(&owner, close(12, "octo/other")).await.unwrap_err();
    assert_eq!(e, ErrorCode::Conflict);
    assert!(r.gh.called("PATCH /repos/octo/app/issues/12").is_none());

    r.gh.answer(
        "PATCH /repos/octo/app/issues/12",
        json!({ "state": "closed" }),
    );
    r.act(&owner, close(12, "octo/app")).await.unwrap();
    let patched = r.gh.called("PATCH /repos/octo/app/issues/12").unwrap();
    assert_eq!(
        patched,
        json!({ "state": "closed", "state_reason": "completed" })
    );
    // What it asked is answered.
    assert_eq!(
        r.task(task.task_id).github.unwrap().stage,
        Some(PrStage::Closed)
    );
    assert!(r.waiting_on(task.task_id).unwrap().resolved_at.is_some());
    assert_eq!(r.task(task.task_id).attention_count, 0);
    r.f.close().await;
}

#[tokio::test]
async fn trying_an_issue_again_answers_the_agent_that_couldnt_reproduce_it() {
    let r = repo(true).await;
    r.set(drafts()).await.unwrap();
    r.mine(12, "Crash on save");
    let (task, binding) = r
        .started(GithubLink::Issue {
            number: 12,
            review_with: None,
        })
        .await;
    r.found_fixed(&binding, 12, false).await;
    assert!(r.waiting_on(task.task_id).unwrap().resolved_at.is_none());
    let again = r
        .link(
            GithubLink::Issue {
                number: 12,
                review_with: None,
            },
            "Fix #12: Crash on save\n\nIt still crashes with a full disk.",
        )
        .await
        .unwrap();
    assert!(r.waiting_on(task.task_id).unwrap().resolved_at.is_some());
    // The new fix speaks for the issue now.
    let stage = r.task(task.task_id).github.unwrap().stage;
    assert_eq!(stage, Some(PrStage::NotReproduced));

    // Closed meanwhile, the new fix waiting in Next up leaves it.
    r.gh.answer(
        "PATCH /repos/octo/app/issues/12",
        json!({ "state": "closed" }),
    );
    let close = GithubAction::CloseIssue {
        project_id: r.project_id,
        repo: "octo/app".into(),
        number: 12,
        done: false,
    };
    r.act(&Caller::local_owner(), close).await.unwrap();
    let patched = r.gh.called("PATCH /repos/octo/app/issues/12").unwrap();
    assert_eq!(patched["state_reason"], "not_planned");
    assert_eq!(r.task(again.task_id).state, TaskState::Archived);
    r.f.close().await;
}

#[tokio::test]
async fn through_merge_lets_the_run_merge_the_head_it_checked() {
    let r = repo(true).await;
    let through = GithubSettings {
        mode: GithubMode::ThroughMerge,
        ..Default::default()
    };
    r.set(through).await.unwrap();
    r.gh.answer("GET /repos/octo/app/pulls/7", pull(7, "octo/app", "me"));
    let (task, binding) = r.started(GithubLink::Pr { number: 7 }).await;
    let e = call(&r.f, &binding, GithubCall::Ready).await.unwrap_err();
    assert!(e.contains("hasn't answered"), "{e}");
    let keep = GithubAction::AnswerWarning {
        project_id: r.project_id,
        repo: "octo/app".into(),
        keep_through_merge: true,
    };
    r.act(&Caller::local_owner(), keep).await.unwrap();

    // Not before its change is checked, while it is, after the person was
    // asked, or while what the check found is unfixed.
    let e = call(&r.f, &binding, GithubCall::Ready).await.unwrap_err();
    assert!(e.contains("reviewed before it merges"), "{e}");
    let held = [
        (Some(PrStage::Checking), false, "is checking"),
        (Some(PrStage::Capped), false, "was asked to look"),
        (None, true, "found problems"),
    ];
    for (stage, review, says) in held {
        r.alter(task.task_id, |g| {
            g.stage = stage;
            g.checked_by = Some(claude());
        });
        if review {
            (r.f.hub
                .write(|tx| tx.put(REVIEW, task.task_id, &"Rename it.".to_string())))
            .unwrap();
        }
        let e = call(&r.f, &binding, GithubCall::Ready).await.unwrap_err();
        assert!(e.contains(says), "{e}");
    }
    (r.f.hub.write(|tx| tx.delete(REVIEW, task.task_id))).unwrap();
    // A round charged with them and not yet started holds them too.
    let charged = handover(task.task_id, 0, Timestamp(0));
    (r.f.hub.write(|tx| tx.put(HANDOVER, task.task_id, &charged))).unwrap();
    let e = call(&r.f, &binding, GithubCall::Ready).await.unwrap_err();
    assert!(e.contains("found problems"), "{e}");
    (r.f.hub.write(|tx| tx.delete(HANDOVER, task.task_id))).unwrap();

    // Not while a review thread is open.
    r.gh.query("pullRequest(number", pr_answer("abc", true, "DRAFT"));
    let e = call(&r.f, &binding, GithubCall::Ready).await.unwrap_err();
    assert!(e.contains("1 review thread is open"), "{e}");
    let green = |head: &str, draft, state: &str| {
        let mut answer = pr_answer(head, draft, state);
        answer["repository"]["pullRequest"]["reviewThreads"]["nodes"] = json!([]);
        answer
    };
    // Nor from a fork, whose branch the agent doesn't keep.
    let mut forked = green("abc", true, "DRAFT");
    forked["repository"]["pullRequest"]["isCrossRepository"] = json!(true);
    r.gh.query("pullRequest(number", forked);
    let e = call(&r.f, &binding, GithubCall::Ready).await.unwrap_err();
    assert!(e.contains("in a fork"), "{e}");
    r.gh.query("pullRequest(number", green("abc", true, "DRAFT"));
    r.gh.query("markPullRequestReadyForReview", json!({}));
    let text = call(&r.f, &binding, GithubCall::Ready).await.unwrap();
    assert!(text.contains("head abc"), "{text}");
    assert_eq!(
        r.task(task.task_id).github.unwrap().stage,
        Some(PrStage::Ready)
    );

    // A commit landed after the run looked.
    r.gh.query("pullRequest(number", green("def", false, "CLEAN"));
    let merge = |head: &str| GithubCall::Merge {
        expected_head: head.into(),
    };
    let e = call(&r.f, &binding, merge("abc")).await.unwrap_err();
    assert!(e.contains("head is def"), "{e}");
    assert!(r.gh.called("PUT /repos/octo/app/pulls/7/merge").is_none());
    r.gh.answer(
        "PUT /repos/octo/app/pulls/7/merge",
        json!({ "merged": true }),
    );
    // Stacked on another pull request, it waits for that one.
    let mut stacked = green("def", false, "CLEAN");
    stacked["repository"]["pullRequest"]["baseRefName"] = json!("lower");
    r.gh.query("pullRequest(number", stacked);
    let e = call(&r.f, &binding, merge("def")).await.unwrap_err();
    assert!(e.contains("targets `lower`"), "{e}");
    assert!(r.gh.called("PUT /repos/octo/app/pulls/7/merge").is_none());
    r.gh.query("pullRequest(number", green("def", false, "CLEAN"));
    let text = call(&r.f, &binding, merge("def")).await.unwrap();
    assert!(text.contains("is waiting to merge"), "{text}");
    // Not while its own run is still going: it waits in line.
    assert!(r.gh.called("PUT /repos/octo/app/pulls/7/merge").is_none());
    assert_eq!(
        r.task(task.task_id).github.unwrap().stage,
        Some(PrStage::Merging)
    );
    r.f.close().await;
}

#[tokio::test]
async fn what_the_person_changes_while_a_merge_is_read_holds_it() {
    let r = repo(true).await;
    let through = || async {
        let settings = GithubSettings {
            mode: GithubMode::ThroughMerge,
            ..Default::default()
        };
        r.set(settings).await.unwrap();
        let keep = GithubAction::AnswerWarning {
            project_id: r.project_id,
            repo: "octo/app".into(),
            keep_through_merge: true,
        };
        r.act(&Caller::local_owner(), keep).await.unwrap();
    };
    through().await;
    r.gh.answer("GET /repos/octo/app/pulls/7", pull(7, "octo/app", "me"));
    let (task, binding) = r.started(GithubLink::Pr { number: 7 }).await;
    r.alter(task.task_id, |g| g.checked_by = Some(claude()));
    let green = |head: &str, draft, state: &str| {
        let mut answer = pr_answer(head, draft, state);
        answer["repository"]["pullRequest"]["reviewThreads"]["nodes"] = json!([]);
        answer
    };
    r.gh.query("markPullRequestReadyForReview", json!({}));
    r.gh.answer(
        "PUT /repos/octo/app/pulls/7/merge",
        json!({ "merged": true }),
    );
    // The person turns it back to Drafts while the pull request is read.
    let to_drafts = || {
        let hub = r.f.hub.clone();
        let project_id = r.project_id;
        move || {
            let settings = drafts();
            (hub.write(|tx| tx.put(crate::github::SETTINGS, project_id, &settings))).unwrap();
        }
    };
    r.gh.query("pullRequest(number", green("abc", true, "DRAFT"));
    r.gh.meanwhile("pullRequest(number", to_drafts());
    let e = call(&r.f, &binding, GithubCall::Ready).await.unwrap_err();
    assert!(e.contains("In Drafts"), "{e}");
    let marked = "GRAPHQL mutation($id:ID!){markPullRequestReadyForReview(input:{pullRequestId:$id}){pullRequest{isDraft}}}";
    assert!(r.gh.called(marked).is_none());

    through().await;
    r.gh.query("pullRequest(number", green("def", false, "CLEAN"));
    let merge = GithubCall::Merge {
        expected_head: "def".into(),
    };
    r.gh.meanwhile("pullRequest(number", to_drafts());
    let e = call(&r.f, &binding, merge.clone()).await.unwrap_err();
    assert!(e.contains("In Drafts"), "{e}");
    assert!(r.gh.called("PUT /repos/octo/app/pulls/7/merge").is_none());

    // Or the task opens another pull request meanwhile.
    through().await;
    let hub = r.f.hub.clone();
    let task_id = task.task_id;
    r.gh.meanwhile("pullRequest(number", move || {
        let now = hub.inner.clock.now();
        (hub.write(|tx| {
            crate::github_watch::change_link(tx, task_id, now, |g| {
                g.pr.as_mut().unwrap().number = 22;
            })
        }))
        .unwrap();
    });
    let e = call(&r.f, &binding, merge).await.unwrap_err();
    assert!(e.contains("is #22 now, not #7"), "{e}");
    assert!(r.gh.called("PUT /repos/octo/app/pulls/7/merge").is_none());
    r.f.close().await;
}

#[tokio::test]
async fn ready_keeps_a_hold_put_on_while_the_pull_request_was_read() {
    let r = repo(true).await;
    let settings = GithubSettings {
        mode: GithubMode::ThroughMerge,
        ..Default::default()
    };
    r.set(settings).await.unwrap();
    let keep = GithubAction::AnswerWarning {
        project_id: r.project_id,
        repo: "octo/app".into(),
        keep_through_merge: true,
    };
    r.act(&Caller::local_owner(), keep).await.unwrap();
    r.gh.answer("GET /repos/octo/app/pulls/7", pull(7, "octo/app", "me"));
    // An older task on the pull request is held for the person.
    let older = (r.link(GithubLink::Pr { number: 7 }, "Earlier"))
        .await
        .unwrap();
    r.alter(older.task_id, |g| g.stage = Some(PrStage::Capped));
    r.f.clock.advance(Duration::from_secs(60));
    let (task, binding) = r.started(GithubLink::Pr { number: 7 }).await;
    r.alter(task.task_id, |g| g.checked_by = Some(claude()));
    let mut green = pr_answer("def", false, "CLEAN");
    green["repository"]["pullRequest"]["reviewThreads"]["nodes"] = json!([]);
    r.gh.query("pullRequest(number", green);
    // A check fails while the pull request, already ready, is read.
    let hub = r.f.hub.clone();
    let task_id = task.task_id;
    r.gh.meanwhile("pullRequest(number", move || {
        (hub.cap(task_id, 7, "The check found problems.".into())).unwrap();
    });
    let e = call(&r.f, &binding, GithubCall::Ready).await.unwrap_err();
    assert!(e.contains("The person was asked"), "{e}");
    assert_eq!(
        r.task(task.task_id).github.unwrap().stage,
        Some(PrStage::Capped)
    );

    // Picked up again, it is Ready, and the older task stays held.
    r.alter(task.task_id, |g| g.stage = Some(PrStage::Babysitting));
    call(&r.f, &binding, GithubCall::Ready).await.unwrap();
    assert_eq!(
        r.task(task.task_id).github.unwrap().stage,
        Some(PrStage::Ready)
    );
    assert_eq!(
        r.task(older.task_id).github.unwrap().stage,
        Some(PrStage::Capped)
    );
    r.f.close().await;
}

#[tokio::test]
async fn the_agent_doesnt_take_a_pull_request_through_without_a_check_while_the_workflows_are_unknown()
 {
    let r = repo(true).await;
    r.gh.fail_query("object(expression:$expression");
    let settings = GithubSettings {
        mode: GithubMode::ThroughMerge,
        ..Default::default()
    };
    r.set(settings).await.unwrap();
    let keep = GithubAction::AnswerWarning {
        project_id: r.project_id,
        repo: "octo/app".into(),
        keep_through_merge: true,
    };
    r.act(&Caller::local_owner(), keep).await.unwrap();
    r.gh.answer("GET /repos/octo/app/pulls/7", pull(7, "octo/app", "me"));
    let (task, binding) = r.started(GithubLink::Pr { number: 7 }).await;
    r.alter(task.task_id, |g| g.checked_by = Some(claude()));
    // No check has reported on it.
    let mut unchecked = pr_answer("def", false, "CLEAN");
    let pr = &mut unchecked["repository"]["pullRequest"];
    pr["reviewThreads"]["nodes"] = json!([]);
    pr["commits"]["nodes"][0]["commit"]["statusCheckRollup"]["contexts"]["nodes"] = json!([]);
    r.gh.query("pullRequest(number", unchecked);
    r.gh.answer(
        "PUT /repos/octo/app/pulls/7/merge",
        json!({ "merged": true }),
    );
    for refused in [
        GithubCall::Ready,
        GithubCall::Merge {
            expected_head: "def".into(),
        },
    ] {
        let e = call(&r.f, &binding, refused).await.unwrap_err();
        assert!(e.contains("no check has reported"), "{e}");
    }
    assert!(r.gh.called("PUT /repos/octo/app/pulls/7/merge").is_none());
    // Known to run none, it goes through.
    r.gh.query(
        "object(expression:$expression",
        json!({ "repository": { "object": null } }),
    );
    call(&r.f, &binding, GithubCall::Ready).await.unwrap();
    r.f.close().await;
}

#[tokio::test]
async fn a_merge_reads_the_pull_request_of_the_repository_it_was_allowed_on() {
    let r = repo(true).await;
    let settings = GithubSettings {
        mode: GithubMode::ThroughMerge,
        ..Default::default()
    };
    r.set(settings).await.unwrap();
    let keep = GithubAction::AnswerWarning {
        project_id: r.project_id,
        repo: "octo/app".into(),
        keep_through_merge: true,
    };
    r.act(&Caller::local_owner(), keep).await.unwrap();
    r.gh.answer("GET /repos/octo/app/pulls/7", pull(7, "octo/app", "me"));
    let (task, binding) = r.started(GithubLink::Pr { number: 7 }).await;
    r.alter(task.task_id, |g| g.checked_by = Some(claude()));
    let mut green = pr_answer("def", false, "CLEAN");
    green["repository"]["pullRequest"]["reviewThreads"]["nodes"] = json!([]);
    r.gh.query("pullRequest(number", green);
    r.gh.answer(
        "PUT /repos/octo/app/pulls/7/merge",
        json!({ "merged": true }),
    );
    // `origin` names another repository once the run was let through.
    let dir = r.f.project_dir();
    r.gh.meanwhile("GET /user", move || {
        let status = std::process::Command::new("git")
            .args([
                "remote",
                "set-url",
                "origin",
                "git@github.com:octo/other.git",
            ])
            .current_dir(dir)
            .status()
            .unwrap();
        assert!(status.success());
    });
    let merge = GithubCall::Merge {
        expected_head: "def".into(),
    };
    call(&r.f, &binding, merge).await.unwrap();
    let read: Vec<Value> = (r.gh.calls.lock().unwrap().iter())
        .filter(|(c, _)| c.starts_with("GRAPHQL ") && c.contains("pullRequest(number"))
        .map(|(_, v)| v.clone())
        .collect();
    assert!(!read.is_empty());
    for v in read {
        assert_eq!(
            (v["owner"].as_str(), v["name"].as_str()),
            (Some("octo"), Some("app"))
        );
    }
    assert_eq!(
        r.task(task.task_id).github.unwrap().stage,
        Some(PrStage::Merging)
    );
    r.f.close().await;
}

#[tokio::test]
async fn an_older_task_doesnt_merge_around_a_newer_one_on_its_pull_request() {
    let r = repo(true).await;
    let settings = GithubSettings {
        mode: GithubMode::ThroughMerge,
        ..Default::default()
    };
    r.set(settings).await.unwrap();
    let keep = GithubAction::AnswerWarning {
        project_id: r.project_id,
        repo: "octo/app".into(),
        keep_through_merge: true,
    };
    r.act(&Caller::local_owner(), keep).await.unwrap();
    r.gh.answer("GET /repos/octo/app/pulls/7", pull(7, "octo/app", "me"));
    let (task, binding) = r.started(GithubLink::Pr { number: 7 }).await;
    r.alter(task.task_id, |g| g.checked_by = Some(claude()));
    // A newer task on the pull request is held for the person.
    r.f.clock.advance(Duration::from_secs(60));
    let newer = (r.link(GithubLink::Pr { number: 7 }, "Finish that").await).unwrap();
    r.alter(newer.task_id, |g| g.stage = Some(PrStage::Capped));
    let mut green = pr_answer("def", false, "CLEAN");
    green["repository"]["pullRequest"]["reviewThreads"]["nodes"] = json!([]);
    r.gh.query("pullRequest(number", green);
    r.gh.answer(
        "PUT /repos/octo/app/pulls/7/merge",
        json!({ "merged": true }),
    );
    let merge = GithubCall::Merge {
        expected_head: "def".into(),
    };
    let e = call(&r.f, &binding, merge).await.unwrap_err();
    assert!(e.contains("A newer task is on this pull request"), "{e}");
    assert!(r.gh.called("PUT /repos/octo/app/pulls/7/merge").is_none());
    r.f.close().await;
}

#[tokio::test]
async fn a_stacked_pull_request_starts_from_the_one_under_it() {
    let r = repo(true).await;
    r.set(drafts()).await.unwrap();
    r.gh.answer("GET /repos/octo/app/pulls/7", pull(7, "octo/app", "me"));
    let (task, _binding) = r.started(GithubLink::Stack { number: 7 }).await;
    assert_eq!(task.github.unwrap().base.as_deref(), Some("save-fix"));
    let told = (r.f.harness.specs.lock().unwrap().last())
        .and_then(|spec| spec.github.clone())
        .unwrap();
    assert!(told.contains("from `save-fix`"), "{told}");
    assert!(told.contains("This stacks on pull request #7"), "{told}");

    // A fork's branch can't be a base here.
    r.gh.answer(
        "GET /repos/octo/app/pulls/8",
        pull(8, "teammate/app", "teammate"),
    );
    let e = r
        .link(GithubLink::Stack { number: 8 }, "Next")
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::InvalidRequest);
    r.f.close().await;
}

#[tokio::test]
async fn a_tasks_pull_request_is_never_another_tasks() {
    let r = repo(true).await;
    r.set(drafts()).await.unwrap();
    r.mine(13, "Plain pages");
    let (task, binding) = r
        .started(GithubLink::Issue {
            number: 13,
            review_with: None,
        })
        .await;
    let head = task.branch.as_ref().unwrap().name.clone();
    // The project folder is on another task's branch, whose pull request
    // is open: this task's agent worked in a folder of its own.
    let mut other = task.clone();
    other.task_id = TaskId::new();
    other.title = "Make saving atomic".into();
    other.branch = None;
    other.current_run = None;
    (r.f.hub)
        .write(|tx| tx.put("task", other.task_id, &other))
        .unwrap();
    r.alter(other.task_id, |g| {
        g.pr = Some(PrSummary {
            number: 9,
            url: "https://github.com/octo/app/pull/9".into(),
            state: PrState::Open,
            draft: false,
            head: "1111111aaaa".into(),
            passed: 0,
            failed: 0,
            pending: 0,
            unresolved: 0,
            conflict: false,
        })
    });
    r.on("save-fix");
    r.gh.answer("GET /repos/octo/app/branches/save-fix", json!({}));
    r.gh.answer(
        "GET /repos/octo/app/pulls?head=octo:save-fix&state=open&per_page=5",
        json!([pull(9, "octo/app", "me")]),
    );
    let pr = || GithubCall::Pr {
        title: "fix: plain pages".into(),
        body: "Pages render plainly.".into(),
    };
    let e = call(&r.f, &binding, pr()).await.unwrap_err();
    assert!(
        e.contains("#9") && e.contains("another task's") && e.contains("Make saving atomic"),
        "{e}"
    );
    assert!(e.contains(&format!("`{head}`")), "{e}");

    // Known as the other's branch, the folder's is passed over for its own.
    r.f.hub
        .write(|tx| {
            let mut t: Task = tx.read().require("task", other.task_id)?;
            t.branch = Some(TaskBranch {
                name: "save-fix".into(),
                new: false,
            });
            tx.put("task", t.task_id, &t)
        })
        .unwrap();
    let e = call(&r.f, &binding, pr()).await.unwrap_err();
    assert!(e.contains(&format!("git push -u origin {head}")), "{e}");
    let branch = segment(&head);
    r.gh.answer(&format!("GET /repos/octo/app/branches/{branch}"), json!({}));
    r.gh.answer(
        &format!("GET /repos/octo/app/pulls?head=octo:{branch}&state=open&per_page=5"),
        json!([]),
    );
    let opened = json!({
        "number": 21, "state": "open", "draft": true,
        "html_url": "https://github.com/octo/app/pull/21",
        "user": { "login": "me" }, "author_association": "OWNER", "head": { "ref": head, "sha": "aaa111" },
    });
    r.gh.answer("POST /repos/octo/app/pulls", opened);
    r.gh.answer(
        "GET /repos/octo/app/labels/agent",
        json!({ "name": "agent" }),
    );
    assert_eq!(
        call(&r.f, &binding, pr()).await.unwrap(),
        "Opened draft pull request #21: https://github.com/octo/app/pull/21"
    );
    let sent = r.gh.called("POST /repos/octo/app/pulls").unwrap();
    assert_eq!(sent["head"], json!(head));
    assert!(r.gh.called("PATCH /repos/octo/app/pulls/9").is_none());
    r.f.close().await;
}

#[tokio::test]
async fn a_branch_already_merged_as_it_is_opens_no_second_pull_request() {
    let r = repo(true).await;
    r.set(drafts()).await.unwrap();
    r.mine(13, "Plain pages");
    let (task, binding) = r
        .started(GithubLink::Issue {
            number: 13,
            review_with: None,
        })
        .await;
    let head = task.branch.as_ref().unwrap().name.clone();
    r.on(&head);
    let branch = segment(&head);
    r.gh.answer(
        &format!("GET /repos/octo/app/branches/{branch}"),
        json!({ "commit": { "sha": "aaa111" } }),
    );
    r.gh.answer(
        &format!("GET /repos/octo/app/pulls?head=octo:{branch}&state=open&per_page=5"),
        json!([]),
    );
    // #21 was squash-merged from the branch as it still is: GitHub would
    // let a second one open with the same commits.
    let closed = format!("GET /repos/octo/app/pulls?head=octo:{branch}&state=closed&per_page=5");
    let settled = |merged: bool, sha: &str| {
        json!([{
            "number": 21, "state": "closed", "merged_at": if merged { json!("2026-10-12T20:00:00Z") } else { json!(null) },
            "html_url": "https://github.com/octo/app/pull/21",
            "user": { "login": "me" }, "author_association": "OWNER", "head": { "ref": head, "sha": sha },
        }])
    };
    r.gh.answer(&closed, settled(true, "aaa111"));
    let pr = || GithubCall::Pr {
        title: "fix: plain pages".into(),
        body: "Pages render plainly.".into(),
    };
    let e = call(&r.f, &binding, pr()).await.unwrap_err();
    assert!(
        e.contains("#21") && e.contains("already merged") && e.contains("github_comment"),
        "{e}"
    );
    r.gh.answer(&closed, settled(false, "aaa111"));
    let e = call(&r.f, &binding, pr()).await.unwrap_err();
    assert!(e.contains("#21") && e.contains("was closed"), "{e}");
    assert!(r.gh.called("POST /repos/octo/app/pulls").is_none());

    // New commits since are new work, in a pull request of their own.
    r.gh.answer(&closed, settled(true, "000aaa"));
    r.gh.answer(
        "POST /repos/octo/app/pulls",
        json!({
            "number": 22, "state": "open", "draft": true,
            "html_url": "https://github.com/octo/app/pull/22",
            "user": { "login": "me" }, "author_association": "OWNER", "head": { "ref": head, "sha": "aaa111" },
        }),
    );
    r.gh.answer(
        "GET /repos/octo/app/labels/agent",
        json!({ "name": "agent" }),
    );
    assert_eq!(
        call(&r.f, &binding, pr()).await.unwrap(),
        "Opened draft pull request #22: https://github.com/octo/app/pull/22"
    );
    r.f.close().await;
}

#[tokio::test]
async fn a_task_on_github_asks_another_agent_but_plans_no_work() {
    use super::work_tools_tests::work;
    use ports::bridge::{AgentChoice, WorkCall};
    for link in [
        GithubLink::Issue {
            number: 7,
            review_with: None,
        },
        GithubLink::Pr { number: 7 },
    ] {
        let r = repo(true).await;
        r.set(drafts()).await.unwrap();
        r.mine(7, "Crash on save");
        r.gh.answer("GET /repos/octo/app/pulls/7", pull(7, "octo/app", "me"));
        let (task, binding) = r.started(link).await;
        assert!(binding.github);
        // Exercise the bridge, not just the work handler: these tools must
        // get past the broker's gate before their own checks can run.
        let agents = work(&r.f, &binding, WorkCall::Agents).await.unwrap();
        assert!(agents.contains("ask_agent asks one of these"), "{agents}");
        assert!(!agents.contains("GitHub:"), "{agents}");
        let start = WorkCall::StartIssue {
            number: 3,
            agent: None,
            start: false,
            note: None,
            reviewer: None,
        };
        let refused = work(&r.f, &binding, start).await.unwrap_err();
        assert!(refused.contains("doesn't plan other work"), "{refused}");
        let ask = WorkCall::Ask {
            agent: AgentChoice {
                agent: HarnessKind::ClaudeCode,
                model: None,
                effort: None,
            },
            prompt: "Is this fix complete?".into(),
            title: Some("Second opinion on #7".into()),
        };
        let said = work(&r.f, &binding, ask).await.unwrap();
        assert!(
            said.starts_with("Asked Claude Code in “Second opinion on #7”."),
            "{said}"
        );
        let asked =
            (r.f.hub
                .read(|r| r.list::<Task>("task"))
                .unwrap()
                .into_iter())
            .find(|t| t.asked_by == Some(task.task_id))
            .expect("the task asked");
        // Not linked: it reads the project, and posts nothing to GitHub.
        assert_eq!((asked.mode, asked.github.is_none()), (TaskMode::Ask, true));
        assert_eq!(asked.project_id, task.project_id);
        assert!(!asked.browser);
        assert_eq!(asked.launch_policy, None);
        let run = run_started(&r.f, asked.task_id).await;
        let spec = r.spec(run);
        assert_eq!(spec.mode, TaskMode::Ask);
        assert!(spec.bridge.is_none());
        assert!(spec.github.is_none());
        r.f.close().await;
    }
}
