// SPDX-License-Identifier: Apache-2.0
use super::github_after_tests::{test_check, watched_pr};
use super::github_tools_tests::call;
use super::github_train_tests::{read_at_def, waiting_7_and_8};
use super::*;
use crate::report_fixes::{GithubForge, OwnerApprovals};
use ports::bridge::GithubCall;
use report_fixer::{ApprovalView, Approvals, Forge, Proposal, PullRequest};

fn forge(r: &Repo) -> GithubForge {
    GithubForge {
        github: r.gh.clone(),
        repository: "octo/app".into(),
        mirror: r.f.path.with_extension("mirror"),
        hub: r.f.hub.clone(),
    }
}
fn remember(r: &Repo, head: &str) {
    r.f.hub
        .remember_report_fix("octo/app", head, Some(31))
        .unwrap();
}
fn copied(head: &str, commits: &[&str]) -> Value {
    let mut read = read_at_def(Some("SUCCESS"));
    let pr = &mut read["repository"]["pullRequest"];
    pr["headRefName"] = json!("fix/copied");
    pr["headRefOid"] = json!(head);
    pr["reportCommits"] = json!({
        "totalCount": commits.len(),
        "nodes": commits.iter().map(|oid| json!({"commit": {"oid": oid}})).collect::<Vec<_>>(),
    });
    read
}

#[tokio::test]
async fn report_provenance_precedes_publication_and_survives_restart() {
    let r = repo(true).await;
    let proposal = Proposal {
        reference: "A7K3QX".into(),
        branch: "report/a7k3qx".into(),
        head: "a".repeat(40),
        title: "Fix report A7K3QX".into(),
        body: "Owner review required".into(),
    };
    let hub = r.f.hub.clone();
    let head = proposal.head.clone();
    r.gh.meanwhile("GIT octo/app", move || {
        assert_eq!(
            hub.report_fix_identity("octo/app", None, &[&head]).unwrap(),
            (true, true)
        );
    });
    // Unknown creation completion must leave the pushed head protected.
    r.gh.refuse("POST /repos/octo/app/pulls", 502);
    assert!(forge(&r).push_open(&proposal).await.is_err());
    r.gh.answer(
        "POST /repos/octo/app/pulls",
        json!({"number":31,"head":{"sha":proposal.head}}),
    );
    forge(&r).push_open(&proposal).await.unwrap();
    let mut next = proposal.clone();
    next.head = "b".repeat(40);
    r.gh.answer(
        "POST /repos/octo/app/pulls",
        json!({"number":31,"head":{"sha":next.head}}),
    );
    forge(&r).push_open(&next).await.unwrap();
    let Repo { f, gh, project_id } = r;
    let port = gh.clone();
    let f = f.reopened(|b| b.github(port)).await;
    let r = Repo { f, gh, project_id };
    for head in [&proposal.head, &next.head] {
        let error =
            r.f.hub
                .refuse_report_automation("OCTO", "APP", &copied(head, &[head])["repository"])
                .await
                .unwrap_err();
        assert_eq!(error.code, ErrorCode::PermissionDenied);
    }
    assert_eq!(
        r.f.hub
            .report_fix_identity("octo/app", Some(31), &[])
            .unwrap(),
        (true, true)
    );
    assert_eq!(
        r.f.hub
            .report_fix_identity("octo/other", None, &[&proposal.head])
            .unwrap(),
        (false, false)
    );
    r.f.close().await;
}

#[tokio::test]
async fn report_commits_in_a_composed_pr_and_incomplete_inventories_hold_merges() {
    let r = repo(true).await;
    remember(&r, "report-commit");
    for commits in [vec!["report-commit", "def"], vec!["def"]] {
        let mut read = copied("def", &commits);
        if commits.len() == 1 {
            read["repository"]["pullRequest"]["reportCommits"]["totalCount"] = json!(101);
        }
        assert_eq!(
            r.f.hub
                .refuse_report_automation("octo", "app", &read["repository"])
                .await
                .unwrap_err()
                .code,
            ErrorCode::PermissionDenied
        );
    }
    r.f.hub
        .refuse_report_automation(
            "octo",
            "app",
            &copied("def", &["ordinary", "def"])["repository"],
        )
        .await
        .unwrap();
    assert!(r.gh.calls.lock().unwrap().is_empty());

    let mut rest = pull(7, "octo/app", "me");
    rest["head"]["sha"] = json!("def");
    rest["commits"] = json!(2);
    r.gh.answer(
        "GET /repos/octo/app/pulls/7/commits?per_page=100",
        json!([{"sha":"report-commit"},{"sha":"def"}]),
    );
    assert_eq!(
        r.f.hub
            .refuse_report_automation("octo", "app", &rest)
            .await
            .unwrap_err()
            .code,
        ErrorCode::PermissionDenied
    );
    r.gh.answer(
        "GET /repos/octo/app/pulls/7/commits?per_page=100",
        json!([{"sha":"def"}]),
    );
    rest["commits"] = json!(101);
    assert_eq!(
        r.f.hub
            .refuse_report_automation("octo", "app", &rest)
            .await
            .unwrap_err()
            .code,
        ErrorCode::PermissionDenied
    );
    rest["commits"] = json!(1);
    r.f.hub
        .refuse_report_automation("octo", "app", &rest)
        .await
        .unwrap();
    assert_eq!(r.calls_to("GET /repos/octo/app/pulls/7/commits?").len(), 3);
    r.f.close().await;
}

#[tokio::test]
async fn copied_report_head_refuses_agent_ready_and_through_merge() {
    let r = repo(true).await;
    r.set(GithubSettings {
        mode: GithubMode::ThroughMerge,
        ..Default::default()
    })
    .await
    .unwrap();
    r.act(
        &Caller::local_owner(),
        GithubAction::AnswerWarning {
            project_id: r.project_id,
            repo: "octo/app".into(),
            keep_through_merge: true,
        },
    )
    .await
    .unwrap();
    r.gh.answer("GET /repos/octo/app/pulls/7", pull(7, "octo/app", "me"));
    let (task, binding) = r.started(GithubLink::Pr { number: 7 }).await;
    r.alter(task.task_id, |g| g.checked_by = Some(claude()));
    remember(&r, "def");
    r.gh.query("pullRequest(number", copied("def", &["def"]));
    for action in [
        GithubCall::Ready,
        GithubCall::Merge {
            expected_head: "def".into(),
        },
    ] {
        let error = call(&r.f, &binding, action).await.unwrap_err();
        assert!(error.contains("owner's own Merge tap"), "{error}");
    }
    assert!(r.calls_to("PUT /repos/octo/app/pulls/").is_empty());
    assert!(r.calls_to("POST /repos/octo/app/merges").is_empty());
    r.f.close().await;
}

#[tokio::test]
async fn copied_report_commit_never_enters_a_merge_check_or_train() {
    let (r, _, _) = waiting_7_and_8().await;
    remember(&r, "report-commit");
    let mut rest = pull(7, "octo/app", "me");
    rest["head"]["ref"] = json!("fix/copied");
    rest["head"]["sha"] = json!("def");
    rest["commits"] = json!(2);
    r.gh.answer("GET /repos/octo/app/pulls/7", rest);
    r.gh.answer(
        "GET /repos/octo/app/pulls/7/commits?per_page=100",
        json!([{"sha":"report-commit"},{"sha":"def"}]),
    );
    assert_eq!(
        r.f.hub
            .advance_train(r.project_id, &r.settings())
            .await
            .unwrap_err()
            .code,
        ErrorCode::PermissionDenied
    );
    assert!(r.calls_to("POST /repos/octo/app/merges").is_empty());
    assert!(r.calls_to("PUT /repos/octo/app/pulls/").is_empty());
    r.f.close().await;
}

#[tokio::test]
async fn copied_report_head_refuses_merge_together_and_owner_merge_without_review() {
    let r = repo(true).await;
    r.set(drafts()).await.unwrap();
    remember(&r, "def");
    r.gh.query("pullRequest(number", copied("def", &["def"]));
    for action in [
        GithubAction::MergeTogether {
            project_id: r.project_id,
            repo: "octo/app".into(),
            prs: vec![ChosenPr {
                number: 7,
                head: "def".into(),
                approval_shown: None,
            }],
        },
        owner_merge(&r),
    ] {
        assert_eq!(
            r.act(&Caller::local_owner(), action).await.unwrap_err(),
            ErrorCode::PermissionDenied
        );
    }
    assert!(r.calls_to("POST /repos/octo/app/merges").is_empty());
    assert!(r.calls_to("PUT /repos/octo/app/pulls/").is_empty());
    r.f.close().await;
}

#[tokio::test]
async fn copied_report_head_refuses_watch_automation() {
    let r = repo(true).await;
    r.set(GithubSettings {
        mode: GithubMode::ThroughMerge,
        ..Default::default()
    })
    .await
    .unwrap();
    r.act(
        &Caller::local_owner(),
        GithubAction::AnswerWarning {
            project_id: r.project_id,
            repo: "octo/app".into(),
            keep_through_merge: true,
        },
    )
    .await
    .unwrap();
    r.gh.answer("GET /repos/octo/app/pulls/7", pull(7, "octo/app", "me"));
    let (task, run) = r.start(GithubLink::Pr { number: 7 }, "Finish it").await;
    r.ends(task, run, None).await;
    r.alter(task, |g| g.checked_by = Some(claude()));
    remember(&r, "report-commit");
    let before: Task = r.f.hub.read(|x| x.require("task", task)).unwrap();
    let mut read = watched_pr("def", test_check("SUCCESS"), json!([]), "CLEAN");
    read["repository"]["pr7"] =
        copied("def", &["report-commit", "def"])["repository"]["pullRequest"].clone();
    r.gh.query("viewer{login}", read);
    r.f.clock.advance(Duration::from_secs(11 * 60));
    r.f.hub.watch_prs().await.unwrap();
    let after: Task = r.f.hub.read(|x| x.require("task", task)).unwrap();
    assert_eq!(before.github, after.github);
    assert!(r.calls_to("PUT /repos/octo/app/pulls/").is_empty());
    assert!(r.calls_to("POST /repos/octo/app/merges").is_empty());
    r.f.close().await;
}

#[tokio::test]
async fn resumed_merge_together_refuses_report_commits_before_testing_or_merging() {
    use crate::github_together::{TOGETHER, Together};
    for green in [false, true] {
        let r = repo(true).await;
        r.gh.query("pullRequest(number", read_at_def(None));
        r.gh.answer("POST /repos/octo/app/git/refs", json!({}));
        r.gh.answer("POST /repos/octo/app/merges", json!({"sha":"tested"}));
        r.gh.answer("POST /repos/octo/app/pulls", json!({"number":50}));
        r.act(
            &Caller::local_owner(),
            GithubAction::MergeTogether {
                project_id: r.project_id,
                repo: "octo/app".into(),
                prs: vec![
                    ChosenPr {
                        number: 7,
                        head: "def".into(),
                        approval_shown: None,
                    },
                    ChosenPr {
                        number: 8,
                        head: "def".into(),
                        approval_shown: None,
                    },
                ],
            },
        )
        .await
        .unwrap();
        r.f.hub
            .write(|tx| {
                let mut t: Together = tx.read().require(TOGETHER, r.project_id)?;
                assert!(t.batch.is_some());
                if green {
                    t.batch.as_mut().unwrap().green = true;
                } else {
                    t.batch = None;
                    t.groups = vec![vec![7, 8]];
                }
                tx.put(TOGETHER, r.project_id, &t)
            })
            .unwrap();
        remember(&r, "def");
        r.gh.query("pullRequest(number", copied("def", &["def"]));
        r.gh.calls.lock().unwrap().clear();
        let Repo { f, gh, project_id } = r;
        let port = gh.clone();
        let f = f.reopened(|b| b.github(port)).await;
        let r = Repo { f, gh, project_id };
        assert_eq!(
            r.f.hub.advance_together(project_id).await.unwrap_err().code,
            ErrorCode::PermissionDenied
        );
        assert!(r.calls_to("POST /repos/octo/app/merges").is_empty());
        assert!(r.calls_to("PUT /repos/octo/app/pulls/").is_empty());
        r.f.close().await;
    }
}

#[tokio::test]
async fn resumed_green_train_refuses_a_copied_report_head() {
    use crate::github_train::{TRAIN, Train};
    let (r, _, _) = waiting_7_and_8().await;
    r.f.hub
        .advance_train(r.project_id, &r.settings())
        .await
        .unwrap();
    r.f.hub
        .write(|tx| {
            let mut t: Train = tx.read().require(TRAIN, r.project_id)?;
            t.batch.as_mut().unwrap().green = true;
            tx.put(TRAIN, r.project_id, &t)
        })
        .unwrap();
    remember(&r, "def");
    r.gh.query("pullRequest(number", copied("def", &["def"]));
    r.gh.calls.lock().unwrap().clear();
    let Repo { f, gh, project_id } = r;
    let port = gh.clone();
    let f = f.reopened(|b| b.github(port)).await;
    let r = Repo { f, gh, project_id };
    r.f.hub
        .advance_train(r.project_id, &r.settings())
        .await
        .unwrap();
    assert!(r.calls_to("PUT /repos/octo/app/pulls/").is_empty());
    assert!(
        r.calls_to("GRAPHQL query")
            .iter()
            .any(|(query, _)| query.contains("pullRequest(number"))
    );
    let train: Train = r.f.hub.read(|x| x.require(TRAIN, project_id)).unwrap();
    assert_eq!(train.batch.unwrap().refused, 1);
    r.f.close().await;
}

fn owner_merge(r: &Repo) -> GithubAction {
    GithubAction::MergePr {
        project_id: r.project_id,
        repo: "octo/app".into(),
        number: 7,
        expected_head: "def".into(),
        method: MergeMethod::Squash,
        approval_shown: None,
    }
}
async fn owner_review(r: &Repo) {
    remember(r, "def");
    let view = ApprovalView {
        report_id: ReportId::new(),
        reference: "A7K3QX".into(),
        area: report_pipeline::schema::Area::Services,
        test: "crates/example/tests/repro.rs".into(),
        files: 2,
        lines: 10,
        gates: Default::default(),
        conflict: false,
        pr: PullRequest {
            number: 7,
            head: "def".into(),
        },
    };
    OwnerApprovals {
        hub: r.f.hub.clone(),
        project_id: r.project_id,
        repository: "octo/app".into(),
        forge: Arc::new(forge(r)),
    }
    .raise(&view)
    .await
    .unwrap();
    r.gh.answer(
        "GET /repos/octo/app/pulls/7",
        json!({"head":{"sha":"def","ref":"report/a7k3qx"},"auto_merge":null}),
    );
    r.gh.answer(
        "GET /repos/octo/app/commits/def/check-runs?per_page=100",
        json!({"check_runs":[{"status":"completed","conclusion":"success"}]}),
    );
    r.gh.answer("GET /repos/octo/app/actions/workflows/checks.yml/runs?head_sha=def&per_page=100", json!({"workflow_runs":[{"id":42,"head_sha":"def","event":"pull_request","status":"completed","conclusion":"success"}]}));
    r.gh.answer(
        "GET /repos/octo/app/actions/runs/42/jobs?per_page=100",
        json!({"jobs":[{"name":"linux","conclusion":"success","labels":["ophio-isolated"]}]}),
    );
    let mut read = copied("def", &["def"]);
    read["repository"]["pullRequest"]["headRefName"] = json!("report/a7k3qx");
    r.gh.query("pullRequest(number", read);
    r.gh.answer("PUT /repos/octo/app/pulls/7/merge", json!({"merged":true}));
}

#[tokio::test]
async fn owner_merge_still_merges_a_clean_report_fix() {
    let r = repo(true).await;
    r.set(drafts()).await.unwrap();
    owner_review(&r).await;
    r.act(&Caller::local_owner(), owner_merge(&r))
        .await
        .unwrap();
    assert_eq!(
        r.gh.called("PUT /repos/octo/app/pulls/7/merge").unwrap()["sha"],
        "def"
    );
    let tap: crate::report_fixes::MergeTap =
        r.f.hub
            .read(|x| x.require("report_fix_merge_tap", "octo/app/7"))
            .unwrap();
    assert_eq!(tap.head, "def");
    r.f.close().await;
}

#[tokio::test]
async fn report_fix_with_auto_merge_enabled_refuses_the_owner_merge() {
    let r = repo(true).await;
    r.set(drafts()).await.unwrap();
    owner_review(&r).await;
    let mut read = copied("def", &["def"]);
    read["repository"]["pullRequest"]["autoMergeRequest"] =
        json!({"enabledAt":"2026-10-16T00:00:00Z"});
    r.gh.query("pullRequest(number", read);
    assert_eq!(
        r.act(&Caller::local_owner(), owner_merge(&r))
            .await
            .unwrap_err(),
        ErrorCode::PermissionDenied
    );
    // The final REST read catches auto-merge enabled after the sheet was read.
    r.gh.query("pullRequest(number", copied("def", &["def"]));
    r.gh.answer("GET /repos/octo/app/pulls/7", json!({"head":{"sha":"def","ref":"report/a7k3qx"},"auto_merge":{"enabled_by":{"login":"me"}}}));
    assert_eq!(
        r.f.hub
            .check_report_fix_merge("octo", "app", 7, "def")
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    assert_eq!(
        r.act(&Caller::local_owner(), owner_merge(&r))
            .await
            .unwrap_err(),
        ErrorCode::Conflict
    );
    assert!(r.calls_to("PUT /repos/octo/app/pulls/").is_empty());
    assert!(
        r.f.hub
            .read(|x| x.get::<crate::report_fixes::MergeTap>("report_fix_merge_tap", "octo/app/7"))
            .unwrap()
            .is_none()
    );
    r.f.close().await;
}
