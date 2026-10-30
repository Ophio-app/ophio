// SPDX-License-Identifier: Apache-2.0
//! The line of pull requests waiting to merge in Through merge: tested
//! together with the default branch once nothing in the project is running
//! or waiting in Next up, merged in turn on green, halved on red.
use super::github_after_tests::{test_check, watched_pr};
use super::*;
use crate::github_train::{TRAIN, Train};
use crate::github_watch::MERGE_CHECK;
use protocol::queue::QueueView;

impl Repo {
    async fn advance_train(&self) {
        let settings = self.settings();
        (self.f.hub)
            .advance_train(self.project_id, &settings)
            .await
            .unwrap();
    }
    fn train(&self) -> Option<Train> {
        self.f.hub.read(|r| r.get(TRAIN, self.project_id)).unwrap()
    }
    /// The calls asked so far that start with `prefix`, with what was sent.
    pub(super) fn calls_to(&self, prefix: &str) -> Vec<(String, Value)> {
        let calls = self.gh.calls.lock().unwrap();
        (calls.iter())
            .filter(|(c, _)| c.starts_with(prefix))
            .cloned()
            .collect()
    }
}

/// Through merge on octo/app, with #7 and #8 checked and waiting to merge
/// at `def`, their tasks' runs over, and the merge check's making answered.
pub(super) async fn waiting_7_and_8() -> (Repo, TaskId, TaskId) {
    let r = repo(true).await;
    let through = GithubSettings {
        mode: GithubMode::ThroughMerge,
        ..Default::default()
    };
    r.set(through).await.unwrap();
    let keep = GithubAction::AnswerWarning {
        project_id: r.project_id,
        repo: "octo/app".into(),
        keep_through_merge: true,
    };
    r.act(&Caller::local_owner(), keep).await.unwrap();
    let mut tasks = Vec::new();
    for number in [7, 8] {
        let pr = format!("GET /repos/octo/app/pulls/{number}");
        r.gh.answer(&pr, pull(number, "octo/app", "me"));
        let (task, run) = r.start(GithubLink::Pr { number }, "Finish it").await;
        r.ends(task, run, None).await;
        r.alter(task, |g| {
            g.checked_by = Some(claude());
            g.stage = Some(PrStage::Merging);
            g.pr.as_mut().unwrap().head = "def".into();
        });
        tasks.push(task);
    }
    r.gh.answer("POST /repos/octo/app/git/refs", json!({}));
    r.gh.answer("POST /repos/octo/app/merges", json!({ "sha": "tested" }));
    r.gh.answer("POST /repos/octo/app/pulls", json!({ "number": 50 }));
    (r, tasks[0], tasks[1])
}

#[tokio::test]
async fn report_branch_never_enters_an_ordinary_merge_check() {
    let (r, _, _) = waiting_7_and_8().await;
    let mut report = pull(7, "octo/app", "me");
    report["head"]["ref"] = json!("report/a7k3qx");
    r.gh.answer("GET /repos/octo/app/pulls/7", report);
    let error =
        r.f.hub
            .advance_train(r.project_id, &r.settings())
            .await
            .unwrap_err();
    assert_eq!(error.code, ErrorCode::PermissionDenied);
    assert!(r.calls_to("POST /repos/octo/app/merges").is_empty());
    assert!(r.calls_to("PUT /repos/octo/app/pulls/").is_empty());
    r.f.close().await;
}

/// A pull request's read at `def`, its required check `test` ended as
/// `conclusion`, or still running without one. The merge check and the
/// pull requests in it read the same.
pub(super) fn read_at_def(conclusion: Option<&str>) -> Value {
    let mut read = pr_answer("def", false, "CLEAN");
    let pr = &mut read["repository"]["pullRequest"];
    pr["reviewThreads"]["nodes"] = json!([]);
    let test =
        &mut pr["commits"]["nodes"][0]["commit"]["statusCheckRollup"]["contexts"]["nodes"][0];
    match conclusion {
        Some(conclusion) => test["conclusion"] = json!(conclusion),
        None => {
            test["status"] = json!("IN_PROGRESS");
            test["conclusion"] = Value::Null;
        }
    }
    read
}

fn numbers(cars: &[crate::github_train::Car]) -> Vec<u64> {
    cars.iter().map(|c| c.number).collect()
}

#[tokio::test]
async fn the_line_waits_until_nothing_runs_or_waits_in_next_up_and_merges_in_order() {
    let (r, seven, eight) = waiting_7_and_8().await;
    // A run going holds it.
    r.gh.answer("GET /repos/octo/app/pulls/9", pull(9, "octo/app", "me"));
    let (other, run) = r
        .start(GithubLink::Pr { number: 9 }, "Something else")
        .await;
    r.advance_train().await;
    assert!(r.calls_to("POST /repos/octo/app/git/refs").is_empty());
    r.ends(other, run, None).await;
    // So does what waits in Next up, paused or not.
    r.gh.answer("GET /repos/octo/app/pulls/10", pull(10, "octo/app", "me"));
    r.link(GithubLink::Pr { number: 10 }, "Later")
        .await
        .unwrap();
    r.advance_train().await;
    assert!(r.calls_to("POST /repos/octo/app/git/refs").is_empty());
    let queue: QueueView = r.f.hub.read(|x| x.require("queue", r.project_id)).unwrap();
    let mut pause = CommandEnvelope::new(Command::QueuePause {
        project_id: r.project_id,
    });
    pause.expected_revision = Some(queue.revision);
    let paused = r.f.hub.submit(&Caller::local_owner(), pause).await;
    assert!(paused.error.is_none(), "{paused:?}");
    r.advance_train().await;
    assert!(r.calls_to("POST /repos/octo/app/git/refs").is_empty());
    assert!(r.train().is_none());

    // Nothing left, the line is tested together with `main`.
    let queue: QueueView = r.f.hub.read(|x| x.require("queue", r.project_id)).unwrap();
    let mut remove = CommandEnvelope::new(Command::QueueRemove {
        item_id: queue.items[0].item_id,
    });
    remove.expected_revision = Some(queue.revision);
    let removed = r.f.hub.submit(&Caller::local_owner(), remove).await;
    assert!(removed.error.is_none(), "{removed:?}");
    r.advance_train().await;
    let made = r.gh.called("POST /repos/octo/app/git/refs").unwrap();
    assert_eq!(made["sha"], "main0");
    let branch = (made["ref"].as_str().unwrap())
        .strip_prefix("refs/heads/")
        .unwrap()
        .to_owned();
    assert!(branch.starts_with("ophio/merge-"), "{branch}");
    let merges = r.calls_to("POST /repos/octo/app/merges");
    let said: Vec<_> = (merges.iter())
        .map(|(_, b)| b["commit_message"].as_str().unwrap())
        .collect();
    assert_eq!(said, ["Øphio merge check: #7", "Øphio merge check: #8"]);
    assert!(
        (merges.iter()).all(|(_, b)| b["base"] == branch.as_str() && b["head"] == "def"),
        "{merges:?}"
    );
    let opened = r.gh.called("POST /repos/octo/app/pulls").unwrap();
    assert_eq!(opened["title"], "Øphio merge check: #7 and #8");
    assert_eq!(
        (opened["head"].as_str(), opened["base"].as_str()),
        (Some(branch.as_str()), Some("main"))
    );
    assert_eq!(opened["draft"], true);
    let batch = r.train().unwrap().batch.unwrap();
    assert_eq!(
        (batch.number, batch.green, numbers(&batch.cars)),
        (Some(50), false, vec![7, 8])
    );
    // It isn't on the list of pull requests.
    let open = |number: u64, head: &str| {
        json!({ "number": number, "title": "t", "url": "u", "updatedAt": "2026-10-11T00:00:00Z",
                "authorAssociation": "OWNER", "isDraft": false, "headRefName": head,
                "author": { "login": "me" }, "labels": { "nodes": [] } })
    };
    let nodes = json!([open(7, "save-fix"), open(50, &branch)]);
    let listed = json!({ "viewer": { "login": "me" }, "repository": { "pullRequests": { "totalCount": 2, "nodes": nodes } } });
    r.gh.query("pullRequests(states", listed);
    let items = (r.f.hub)
        .github_items(r.project_id, GithubItemKind::Prs, None)
        .await
        .unwrap();
    let listed: Vec<u64> = items.items.iter().map(|i| i.number).collect();
    assert_eq!(listed, [7]);

    // Nothing merges while its checks run.
    r.gh.query("pullRequest(number:$number", read_at_def(None));
    r.advance_train().await;
    assert!(r.calls_to("PUT ").is_empty());

    // Green, each merges in turn at the head it was tested at, and the
    // merge check is closed and its branch deleted.
    r.gh.query("pullRequest(number:$number", read_at_def(Some("SUCCESS")));
    r.gh.answer(
        "PUT /repos/octo/app/pulls/7/merge",
        json!({ "merged": true, "sha": "m7" }),
    );
    r.gh.answer(
        "PUT /repos/octo/app/pulls/8/merge",
        json!({ "merged": true, "sha": "m8" }),
    );
    let gh = r.gh.clone();
    r.gh.meanwhile("PUT /repos/octo/app/pulls/7/merge", move || {
        gh.answer(
            "GET /repos/octo/app/git/ref/heads/main",
            json!({ "object": { "sha": "m7" } }),
        );
    });
    r.advance_train().await;
    assert_eq!(r.calls_to("POST /repos/octo/app/git/refs").len(), 1);
    let put = r.calls_to("PUT ");
    let merged: Vec<_> = put.iter().map(|(c, _)| c.as_str()).collect();
    assert_eq!(
        merged,
        [
            "PUT /repos/octo/app/pulls/7/merge",
            "PUT /repos/octo/app/pulls/8/merge"
        ]
    );
    assert!(
        (put.iter()).all(|(_, b)| b["sha"] == "def" && b["merge_method"] == "squash"),
        "{put:?}"
    );
    for task in [seven, eight] {
        assert_eq!(r.get(task).github.unwrap().stage, Some(PrStage::Merged));
    }
    // Each says it merged, and one alert says they all did.
    assert_eq!(said_merged(&r, seven), [("PR #7 merged".into(), true)]);
    assert_eq!(
        said_merged(&r, eight),
        [("PRs #7 and #8 merged".into(), false)]
    );
    assert_eq!(
        r.gh.called("PATCH /repos/octo/app/pulls/50").unwrap()["state"],
        "closed"
    );
    let deleted = format!("DELETE /repos/octo/app/git/refs/heads/{branch}");
    assert_eq!(r.calls_to(&deleted).len(), 1);
    assert!(r.train().is_none());
    r.f.close().await;
}

/// What `task` was told merged, still open, and whether quietly.
pub(super) fn said_merged(r: &Repo, task: TaskId) -> Vec<(String, bool)> {
    (r.attention(task).into_iter())
        .filter(|a| {
            a.resolved_at.is_none()
                && a.topic() == Some(protocol::notifications::AlertTopic::Merged)
        })
        .map(|a| (a.title, a.quiet))
        .collect()
}

#[tokio::test]
async fn a_line_that_ends_part_way_says_what_merged_once() {
    let (r, seven, eight) = waiting_7_and_8().await;
    r.advance_train().await;
    r.gh.query("pullRequest(number:$number", read_at_def(Some("SUCCESS")));
    r.gh.answer(
        "PUT /repos/octo/app/pulls/7/merge",
        json!({ "merged": true, "sha": "m7" }),
    );
    r.advance_train().await;
    // `main` isn't what #7 made, so the line ends and says what merged.
    assert!(r.train().is_none_or(|t| t.batch.is_none()));
    assert!(r.calls_to("PUT /repos/octo/app/pulls/8/merge").is_empty());
    assert_eq!(said_merged(&r, seven), [("PR #7 merged".into(), false)]);
    assert!(said_merged(&r, eight).is_empty());
    r.f.close().await;
}

#[tokio::test]
async fn one_refused_after_others_merged_says_so_when_it_goes_to_the_person() {
    let (r, seven, eight) = waiting_7_and_8().await;
    r.advance_train().await;
    r.gh.query("pullRequest(number:$number", read_at_def(Some("SUCCESS")));
    r.gh.answer(
        "PUT /repos/octo/app/pulls/7/merge",
        json!({ "merged": true, "sha": "m7" }),
    );
    r.gh.refuse("PUT /repos/octo/app/pulls/8/merge", 405);
    // `main` moves with #7, as GitHub's would.
    let gh = r.gh.clone();
    r.gh.meanwhile("PUT /repos/octo/app/pulls/7/merge", move || {
        gh.answer(
            "GET /repos/octo/app/git/ref/heads/main",
            json!({ "object": { "sha": "m7" } }),
        );
    });
    for _ in 0..5 {
        r.advance_train().await;
    }
    assert_eq!(r.get(eight).github.unwrap().stage, Some(PrStage::Ready));
    let ready = (r.attention(eight).into_iter())
        .find(|a| a.title == "PR #8 is ready")
        .unwrap();
    assert!(!ready.quiet);
    assert!(ready.body.unwrap().ends_with("#7 merged before it."));
    // Told with it: #7's own stays quiet.
    assert_eq!(said_merged(&r, seven), [("PR #7 merged".into(), true)]);
    r.f.close().await;
}

#[tokio::test]
async fn a_line_that_breaks_is_halved_until_what_broke_it_goes_back_to_its_task() {
    let (r, seven, eight) = waiting_7_and_8().await;
    // Behind `main`, one alone is tested too.
    (r.gh).answer_prefixed(
        "GET /repos/octo/app/compare/main...",
        json!({ "behind_by": 3 }),
    );
    r.advance_train().await;
    r.gh.query("pullRequest(number:$number", read_at_def(Some("FAILURE")));
    r.advance_train().await;
    let train = r.train().unwrap();
    assert!(train.batch.is_none());
    let halves: Vec<_> = train.split.iter().map(|h| numbers(h)).collect();
    assert_eq!(halves, [vec![7], vec![8]]);
    assert!(r.gh.called("PATCH /repos/octo/app/pulls/50").is_some());

    // #7 alone is still red: it broke it, and is handed back.
    r.gh.answer("POST /repos/octo/app/pulls", json!({ "number": 51 }));
    r.advance_train().await;
    let opened = r.calls_to("POST /repos/octo/app/pulls");
    assert_eq!(opened.last().unwrap().1["title"], "Øphio merge check: #7");
    r.advance_train().await;
    assert_eq!(
        r.get(seven).github.unwrap().stage,
        Some(PrStage::Babysitting)
    );
    let told: String = r.f.hub.read(|x| x.require(MERGE_CHECK, seven)).unwrap();
    assert!(
        told.contains("merge check #51, this failed: `test`."),
        "{told}"
    );
    assert_eq!(r.get(eight).github.unwrap().stage, Some(PrStage::Merging));

    // #8 alone is green, and merges.
    r.gh.answer("POST /repos/octo/app/pulls", json!({ "number": 52 }));
    r.advance_train().await;
    r.gh.query("pullRequest(number:$number", read_at_def(Some("SUCCESS")));
    r.gh.answer("PUT /repos/octo/app/pulls/8/merge", json!({ "sha": "m8" }));
    r.advance_train().await;
    assert!(r.gh.called("PUT /repos/octo/app/pulls/7/merge").is_none());
    assert_eq!(r.get(eight).github.unwrap().stage, Some(PrStage::Merged));
    assert!(r.train().is_none());

    // The watcher hands what broke to #7's task, once.
    let before = r.get(seven).current_run.unwrap();
    let green = watched_pr("def", test_check("SUCCESS"), json!([]), "CLEAN");
    r.gh.query("viewer{login}", green);
    r.f.clock.advance(Duration::from_secs(60));
    r.f.hub.watch_prs().await.unwrap();
    let run = r.next_run(seven, before).await;
    let given = r.spec(run).prompt.text;
    assert!(
        given.contains("merge check #51, this failed: `test`."),
        "{given}"
    );
    let kept: Option<String> = r.f.hub.read(|x| x.get(MERGE_CHECK, seven)).unwrap();
    assert!(kept.is_none());
    r.f.close().await;
}

/// #7 merges, then another maintainer updates main. An uncertain response
/// is confirmed by reading #7, without learning its merge commit.
pub(super) fn concurrent_update(r: &Repo, reply: Result<Value, u16>, during_next_read: bool) {
    let uncertain = reply.is_err();
    match reply {
        Ok(reply) => r.gh.answer("PUT /repos/octo/app/pulls/7/merge", reply),
        Err(code) => r.gh.refuse("PUT /repos/octo/app/pulls/7/merge", code),
    }
    r.gh.answer("PUT /repos/octo/app/pulls/8/merge", json!({ "sha": "m8" }));
    r.gh.answer_prefixed(
        "GET /repos/octo/app/compare/main...",
        json!({ "behind_by": 1 }),
    );
    let gh = r.gh.clone();
    r.gh.meanwhile("PUT /repos/octo/app/pulls/7/merge", move || {
        gh.answer(
            "GET /repos/octo/app/git/ref/heads/main",
            json!({ "object": { "sha": if during_next_read { "m7" } else { "main1" } } }),
        );
        if during_next_read {
            let gh_read = gh.clone();
            gh.meanwhile("pullRequest(number:$number", move || {
                gh_read.answer(
                    "GET /repos/octo/app/git/ref/heads/main",
                    json!({ "object": { "sha": "main1" } }),
                );
            });
        }
        if uncertain {
            let mut merged = read_at_def(Some("SUCCESS"));
            merged["repository"]["pullRequest"]["state"] = json!("MERGED");
            gh.query("pullRequest(number:$number", merged);
        }
        gh.answer("POST /repos/octo/app/pulls", json!({ "number": 51 }));
    });
}

#[tokio::test]
async fn main_moving_between_merges_retests_the_remainder() {
    retests_remainder(Ok(json!({ "sha": "m7" })), false).await;
}

#[tokio::test]
async fn main_moving_during_the_next_pr_read_retests_the_remainder() {
    retests_remainder(Ok(json!({ "sha": "m7" })), true).await;
}

#[tokio::test]
async fn a_missing_merge_sha_retests_the_remainder() {
    retests_remainder(Ok(json!({ "merged": true })), false).await;
}

#[tokio::test]
async fn an_uncertain_merge_retests_the_remainder() {
    retests_remainder(Err(502), false).await;
}

async fn retests_remainder(reply: Result<Value, u16>, during_next_read: bool) {
    let (r, seven, eight) = waiting_7_and_8().await;
    r.advance_train().await;
    r.gh.query("pullRequest(number:$number", read_at_def(Some("SUCCESS")));
    concurrent_update(&r, reply, during_next_read);
    let gh = r.gh.clone();
    r.gh.meanwhile("PATCH /repos/octo/app/pulls/50", move || {
        gh.query("pullRequest(number:$number", read_at_def(None));
    });
    r.advance_train().await;
    assert_eq!(r.calls_to("PUT /repos/octo/app/pulls/7/merge").len(), 1);
    assert!(r.calls_to("PUT /repos/octo/app/pulls/8/merge").is_empty());
    assert_eq!(r.get(seven).github.unwrap().stage, Some(PrStage::Merged));
    assert_eq!(r.get(eight).github.unwrap().stage, Some(PrStage::Merging));
    assert!(r.train().is_none());
    r.gh.answer("POST /repos/octo/app/pulls", json!({ "number": 51 }));
    r.gh.query("pullRequest(number:$number", read_at_def(None));
    r.advance_train().await;
    let made = r.calls_to("POST /repos/octo/app/git/refs");
    assert_eq!(made.len(), 2);
    assert_eq!(made[1].1["sha"], "main1");
    let batch = r.train().unwrap().batch.unwrap();
    assert_eq!(numbers(&batch.cars), [8]);
    assert!(!batch.green);
    r.advance_train().await;
    assert!(r.calls_to("PUT /repos/octo/app/pulls/8/merge").is_empty());
    r.gh.query("pullRequest(number:$number", read_at_def(Some("SUCCESS")));
    r.advance_train().await;
    assert_eq!(r.get(eight).github.unwrap().stage, Some(PrStage::Merged));
    assert_eq!(r.calls_to("PUT /repos/octo/app/pulls/8/merge").len(), 1);
    r.f.close().await;
}

#[tokio::test]
async fn a_line_tested_on_a_main_that_moved_since_is_tested_again() {
    let (r, seven, _) = waiting_7_and_8().await;
    r.advance_train().await;
    (r.gh).answer(
        "GET /repos/octo/app/git/ref/heads/main",
        json!({ "object": { "sha": "main1" } }),
    );
    r.gh.query("pullRequest(number:$number", read_at_def(Some("SUCCESS")));
    r.gh.answer("PUT /repos/octo/app/pulls/7/merge", json!({ "sha": "m7" }));
    r.advance_train().await;
    assert!(r.calls_to("PUT ").is_empty());
    assert!(r.train().is_none());
    assert_eq!(r.get(seven).github.unwrap().stage, Some(PrStage::Merging));
    r.advance_train().await;
    let made = r.calls_to("POST /repos/octo/app/git/refs");
    assert_eq!(made.len(), 2);
    assert_eq!(made[1].1["sha"], "main1");
    r.f.close().await;
}

#[tokio::test]
async fn through_merge_turned_off_hands_the_line_to_the_person() {
    let (r, seven, eight) = waiting_7_and_8().await;
    r.advance_train().await;
    r.set(drafts()).await.unwrap();
    r.advance_train().await;
    for (task, number) in [(seven, 7), (eight, 8)] {
        assert_eq!(r.get(task).github.unwrap().stage, Some(PrStage::Ready));
        let ready = format!("PR #{number} is ready");
        assert!(r.attention(task).iter().any(|a| a.title == ready));
    }
    assert!(r.gh.called("PATCH /repos/octo/app/pulls/50").is_some());
    assert!(r.calls_to("PUT ").is_empty());
    assert!(r.train().is_none());
    r.f.close().await;
}

#[tokio::test]
async fn merge_rechecks_authorization_after_reading_the_base() {
    for mode in [GithubMode::Drafts, GithubMode::Off] {
        let (r, seven, eight) = waiting_7_and_8().await;
        r.advance_train().await;
        r.gh.query("pullRequest(number:$number", read_at_def(Some("SUCCESS")));
        r.gh.answer("PUT /repos/octo/app/pulls/7/merge", json!({ "sha": "m7" }));
        let hub = r.f.hub.clone();
        let project_id = r.project_id;
        let mut settings = r.settings();
        settings.mode = mode;
        r.gh.meanwhile("GET /repos/octo/app/git/ref/heads/main", move || {
            hub.write(|tx| tx.put(crate::github::SETTINGS, project_id, &settings))
                .unwrap();
        });
        r.advance_train().await;
        assert!(r.calls_to("PUT ").is_empty(), "{mode:?}");
        r.advance_train().await;
        assert!(r.train().is_none());
        for task in [seven, eight] {
            assert_eq!(r.get(task).github.unwrap().stage, Some(PrStage::Ready));
        }
        r.f.close().await;
    }
}

#[tokio::test]
async fn merge_rechecks_a_moved_base_when_pr_validation_fails() {
    for during_next_read in [false, true] {
        let (r, seven, eight) = waiting_7_and_8().await;
        r.advance_train().await;
        r.gh.query("pullRequest(number:$number", read_at_def(Some("SUCCESS")));
        r.gh.answer("PUT /repos/octo/app/pulls/7/merge", json!({ "sha": "m7" }));
        let gh = r.gh.clone();
        r.gh.meanwhile("PUT /repos/octo/app/pulls/7/merge", move || {
            gh.answer(
                "GET /repos/octo/app/git/ref/heads/main",
                json!({ "object": { "sha": if during_next_read { "m7" } else { "main1" } } }),
            );
            let mut behind = read_at_def(Some("SUCCESS"));
            behind["repository"]["pullRequest"]["mergeStateStatus"] = json!("BEHIND");
            gh.query("pullRequest(number:$number", behind);
            if during_next_read {
                let gh_read = gh.clone();
                gh.meanwhile("pullRequest(number:$number", move || {
                    gh_read.answer(
                        "GET /repos/octo/app/git/ref/heads/main",
                        json!({ "object": { "sha": "main1" } }),
                    );
                });
            }
        });
        r.advance_train().await;
        assert_eq!(r.get(seven).github.unwrap().stage, Some(PrStage::Merged));
        assert_eq!(r.get(eight).github.unwrap().stage, Some(PrStage::Merging));
        assert!(r.calls_to("PUT /repos/octo/app/pulls/8/merge").is_empty());
        assert!(r.train().is_none(), "during_next_read={during_next_read}");
        r.gh.answer_prefixed(
            "GET /repos/octo/app/compare/main...",
            json!({ "behind_by": 1 }),
        );
        r.gh.answer("POST /repos/octo/app/pulls", json!({ "number": 51 }));
        r.gh.query("pullRequest(number:$number", read_at_def(None));
        r.advance_train().await;
        let made = r.calls_to("POST /repos/octo/app/git/refs");
        assert_eq!(made.len(), 2);
        assert_eq!(made[1].1["sha"], "main1");
        let batch = r.train().unwrap().batch.unwrap();
        assert_eq!(numbers(&batch.cars), [8]);
        assert!(!batch.green);
        assert_eq!(batch.refused, 0);
        r.advance_train().await;
        assert!(r.calls_to("PUT /repos/octo/app/pulls/8/merge").is_empty());
        r.gh.query("pullRequest(number:$number", read_at_def(Some("SUCCESS")));
        r.gh.answer("PUT /repos/octo/app/pulls/8/merge", json!({ "sha": "m8" }));
        r.advance_train().await;
        assert_eq!(r.get(eight).github.unwrap().stage, Some(PrStage::Merged));
        assert_eq!(r.calls_to("PUT /repos/octo/app/pulls/8/merge").len(), 1);
        r.f.close().await;
    }
}

#[tokio::test]
async fn one_that_conflicts_with_one_ahead_of_it_waits_for_the_next_line() {
    let (r, seven, eight) = waiting_7_and_8().await;
    let gh = r.gh.clone();
    r.gh.meanwhile("POST /repos/octo/app/merges", || {});
    r.gh.meanwhile("POST /repos/octo/app/merges", move || {
        gh.refuse("POST /repos/octo/app/merges", 409);
    });
    r.advance_train().await;
    let opened = r.gh.called("POST /repos/octo/app/pulls").unwrap();
    assert_eq!(opened["title"], "Øphio merge check: #7");
    let batch = r.train().unwrap().batch.unwrap();
    assert_eq!(numbers(&batch.cars), [7]);
    r.gh.query("pullRequest(number:$number", read_at_def(Some("SUCCESS")));
    r.gh.answer("PUT /repos/octo/app/pulls/7/merge", json!({ "sha": "m7" }));
    r.gh.answer("PUT /repos/octo/app/pulls/8/merge", json!({ "sha": "m8" }));
    r.advance_train().await;
    assert_eq!(r.get(seven).github.unwrap().stage, Some(PrStage::Merged));
    assert_eq!(r.get(eight).github.unwrap().stage, Some(PrStage::Merging));
    // Alone and up to date with `main`, it merges without one.
    r.advance_train().await;
    assert_eq!(r.get(eight).github.unwrap().stage, Some(PrStage::Merged));
    assert_eq!(r.calls_to("POST /repos/octo/app/git/refs").len(), 1);
    r.f.close().await;
}

#[tokio::test]
async fn one_github_keeps_refusing_to_merge_goes_to_the_person() {
    let (r, seven, eight) = waiting_7_and_8().await;
    r.advance_train().await;
    r.gh.query("pullRequest(number:$number", read_at_def(Some("SUCCESS")));
    r.gh.refuse("PUT /repos/octo/app/pulls/7/merge", 405);
    for _ in 0..4 {
        r.advance_train().await;
        assert_eq!(r.get(seven).github.unwrap().stage, Some(PrStage::Merging));
    }
    assert!(r.train().unwrap().batch.is_some());
    r.advance_train().await;
    assert_eq!(r.calls_to("PUT /repos/octo/app/pulls/7/merge").len(), 5);
    assert_eq!(r.get(seven).github.unwrap().stage, Some(PrStage::Ready));
    assert!(
        r.attention(seven)
            .iter()
            .any(|a| a.title == "PR #7 is ready")
    );
    // The rest is tested again.
    assert!(r.gh.called("PATCH /repos/octo/app/pulls/50").is_some());
    assert!(r.train().is_none());
    assert_eq!(r.get(eight).github.unwrap().stage, Some(PrStage::Merging));
    r.f.close().await;
}

#[tokio::test]
async fn helper_findings_hold_merging_until_the_caller_handles_the_answer() {
    use super::work_tools_tests::work;
    use crate::followups::{FOLLOWUP, Followup, FollowupKind};
    use ports::bridge::{AgentChoice, WorkCall};

    for checked in [false, true] {
        let r = repo(true).await;
        // Keep delivery under this test's control: the GitHub watcher is
        // independent and must see the gap after the helper finishes.
        let delivery = r.f.hub.inner.guarding.lock().await;
        let mut settings = GithubSettings {
            mode: GithubMode::ThroughMerge,
            ..Default::default()
        };
        settings.workflow.check_before_merge = checked;
        r.set(settings).await.unwrap();
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
        let caller = task.task_id;
        r.alter(caller, |g| {
            g.checked_by = checked.then(claude);
            g.stage = Some(PrStage::Babysitting);
            g.pr.as_mut().unwrap().head = "def".into();
        });
        let mut green = watched_pr("def", test_check("SUCCESS"), json!([]), "CLEAN");
        green["repository"]["pr7"]["isDraft"] = json!(false);
        r.gh.query("viewer{login}", green);
        r.gh.query("pullRequest(number:$number", read_at_def(Some("SUCCESS")));
        r.gh.answer_prefixed(
            "GET /repos/octo/app/compare/main...",
            json!({ "behind_by": 0 }),
        );
        r.gh.answer(
            "PUT /repos/octo/app/pulls/7/merge",
            json!({ "merged": true, "sha": "m7" }),
        );
        work(
            &r.f,
            &binding,
            WorkCall::Ask {
                agent: AgentChoice {
                    agent: HarnessKind::ClaudeCode,
                    model: None,
                    effort: None,
                },
                prompt: "Check for blocking bugs before this merges".into(),
                title: Some("Check the fix".into()),
            },
        )
        .await
        .unwrap();
        let helper =
            r.f.hub
                .read(|r| r.list::<Task>("task"))
                .unwrap()
                .into_iter()
                .find(|t| t.asked_by == Some(caller))
                .unwrap();
        let helper_run = run_started(&r.f, helper.task_id).await;
        let posting = r.f.hub.posting(&r.get(caller)).await.unwrap();
        let refused = r.f.hub.ready(&r.get(caller), &posting).await.unwrap_err();
        assert!(refused.message.contains("helper"), "{refused:?}");
        assert!(!r.f.hub.board(caller, 7).unwrap());
        r.ends(caller, binding.run_id, None).await;
        r.f.hub.watch_prs().await.unwrap();
        assert!(r.calls_to("PUT ").is_empty());

        r.ends(
            helper.task_id,
            helper_run,
            Some("Blocking bug: saving can lose the draft."),
        )
        .await;
        let replies = r.f.hub.read(|r| r.list::<Followup>(FOLLOWUP)).unwrap();
        assert!(
            replies
                .iter()
                .any(|f| f.task_id == caller && f.kind == FollowupKind::Reply)
        );
        r.f.clock.advance(Duration::from_secs(60));
        r.f.hub.watch_prs().await.unwrap();
        assert!(
            r.calls_to("PUT ").is_empty(),
            "merged before delivering the helper's findings"
        );
        assert_eq!(
            r.get(caller).github.unwrap().stage,
            Some(PrStage::Babysitting)
        );
        assert!(!r.f.hub.board(caller, 7).unwrap());
        let task = r.get(caller);
        let posting = r.f.hub.posting(&task).await.unwrap();
        let refused = r.f.hub.ready(&task, &posting).await.unwrap_err();
        assert!(refused.message.contains("helper"), "{refused:?}");
        let refused = r.f.hub.merge(&task, &posting, "def").await.unwrap_err();
        assert!(refused.message.contains("helper"), "{refused:?}");

        // Even one boarded before the consultation must wait without
        // spending the train's retry allowance on this temporary hold.
        r.alter(caller, |g| g.stage = Some(PrStage::Merging));
        for _ in 0..crate::github_train::MAX_REFUSED {
            r.advance_train().await;
        }
        assert!(r.calls_to("PUT ").is_empty());
        assert_eq!(r.get(caller).github.unwrap().stage, Some(PrStage::Merging));
        assert!(r.train().is_none());

        // Delivery resumes the caller; its active turn still holds merging
        // until it has handled the finding and finished.
        r.f.hub.pass_on_followups().await.unwrap();
        let resumed = r.next_run(caller, binding.run_id).await;
        assert!(
            r.spec(resumed)
                .prompt
                .text
                .contains("Blocking bug: saving can lose the draft.")
        );
        r.advance_train().await;
        assert!(r.calls_to("PUT ").is_empty());
        r.ends(
            caller,
            resumed,
            Some("Handled the helper's finding and checked the fix."),
        )
        .await;
        r.advance_train().await;
        assert_eq!(r.times("PUT /repos/octo/app/pulls/7/merge"), 1);
        drop(delivery);
        r.f.close().await;
    }
}

#[tokio::test]
async fn an_undelivered_helper_answer_never_expires_into_permission_to_merge() {
    use crate::followups::Followup;

    let (r, caller, _) = waiting_7_and_8().await;
    let delivery = r.f.hub.inner.guarding.lock().await;
    let reply = Followup::reply(
        caller,
        TaskId::new(),
        "Blocking bug: saving can lose the draft.".into(),
        r.f.hub.inner.clock.now(),
    );
    r.f.hub.write(|tx| reply.put(tx)).unwrap();
    r.advance_train().await;
    assert!(r.calls_to("PUT ").is_empty());

    r.f.clock.advance(Duration::from_secs(24 * 60 * 60 + 1));
    r.f.hub.pass_on_followups().await.unwrap();
    r.gh.query("pullRequest(number:$number", read_at_def(Some("SUCCESS")));
    r.gh.answer("PUT /repos/octo/app/pulls/7/merge", json!({ "sha": "m7" }));
    for _ in 0..4 {
        r.advance_train().await;
    }
    assert!(r.calls_to("PUT /repos/octo/app/pulls/7/merge").is_empty());
    assert_eq!(r.get(caller).github.unwrap().stage, Some(PrStage::Capped));
    assert!(
        r.attention(caller)
            .iter()
            .any(|a| a.title == "PR #7 needs you")
    );
    drop(delivery);
    r.f.close().await;
}

#[tokio::test]
async fn a_helper_reply_arriving_during_the_merge_check_is_rechecked() {
    use crate::followups::Followup;
    let (r, caller, _) = waiting_7_and_8().await;
    r.gh.query("pullRequest(number:$number", read_at_def(Some("SUCCESS")));
    let task = r.get(caller);
    let posting = r.f.hub.posting(&task).await.unwrap();
    let hub = r.f.hub.clone();
    r.gh.meanwhile("pullRequest(number:$number", move || {
        let reply = Followup::reply(
            caller,
            TaskId::new(),
            "Blocking bug".into(),
            hub.inner.clock.now(),
        );
        hub.write(|tx| reply.put(tx)).unwrap();
    });
    let Err(refused) = r.f.hub.merge_now(&task, &posting, "def", "main0").await else {
        panic!("a pending helper reply must refuse merging");
    };
    assert!(refused.message.contains("helper"), "{refused:?}");
    assert!(r.calls_to("PUT ").is_empty());
    r.f.close().await;
}
