// SPDX-License-Identifier: Apache-2.0
//! Merge together: pull requests the person picked, tested together with
//! the default branch on a merge check and then merged in turn, each as
//! itself; red halved until what broke it is left out.
use super::github_train_tests::{concurrent_update, read_at_def, said_merged, waiting_7_and_8};
use super::*;
use crate::github_together::{TOGETHER, Together};

impl Repo {
    /// Let a request reach GitHub, ask to stop while it is in flight, then
    /// return its response. No timing assumptions or background task races.
    async fn stop_during(
        &self,
        word: &'static str,
        advance: impl std::future::Future<Output = ()>,
    ) {
        use std::future::{Future, poll_fn};
        use std::task::Poll;

        let (started, release) = self.gh.pause(word);
        tokio::pin!(advance);
        tokio::select! {
            result = started => result.unwrap(),
            _ = &mut advance => panic!("advancement finished before the request was released"),
        }
        let stop = self.f.hub.stop_together(self.project_id);
        tokio::pin!(stop);
        poll_fn(|cx| {
            assert!(stop.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        assert!(self.together().unwrap().stop_requested);
        release.send(()).unwrap();
        let (_, stopped) = tokio::join!(advance, stop);
        stopped.unwrap();
    }

    fn together(&self) -> Option<Together> {
        self.f
            .hub
            .read(|r| r.get(TOGETHER, self.project_id))
            .unwrap()
    }
    async fn advance_together(&self) {
        (self.f.hub.advance_together(self.project_id))
            .await
            .unwrap();
    }
    /// What became of each picked pull request.
    fn states(&self) -> Vec<(u64, TogetherState)> {
        let t = self.together().unwrap();
        t.view.prs.iter().map(|p| (p.number, p.state)).collect()
    }
    fn why(&self, number: u64) -> String {
        let t = self.together().unwrap();
        let pr = t.view.prs.iter().find(|p| p.number == number).unwrap();
        pr.why.clone().unwrap_or_default()
    }
    fn pick(&self, prs: &[u64], head: &str) -> GithubAction {
        GithubAction::MergeTogether {
            project_id: self.project_id,
            repo: "octo/app".into(),
            prs: (prs.iter())
                .map(|&number| ChosenPr {
                    number,
                    head: head.into(),
                    approval_shown: None,
                })
                .collect(),
        }
    }
    fn stop(&self) -> GithubAction {
        GithubAction::StopTogether {
            project_id: self.project_id,
        }
    }
    async fn project_view(&self) -> GithubProjectView {
        let query = Query::GithubProject {
            project_id: self.project_id,
        };
        let QueryData::GithubProject { view } = (self.f.hub)
            .query(&Caller::local_owner(), query)
            .await
            .unwrap()
        else {
            panic!()
        };
        *view
    }
}

/// octo/app with its pull requests open at `def` and their checks still
/// running, and the merge check's making answered.
async fn open_at_def() -> Repo {
    let r = repo(true).await;
    answer_making(&r);
    r.gh.query("pullRequest(number:$number", read_at_def(None));
    r
}

fn answer_making(r: &Repo) {
    r.gh.answer("POST /repos/octo/app/git/refs", json!({}));
    r.gh.answer("POST /repos/octo/app/merges", json!({ "sha": "tested" }));
    r.gh.answer("POST /repos/octo/app/pulls", json!({ "number": 50 }));
}

use TogetherState::{LeftOut, Merged, Testing, Waiting};

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
    let r = open_at_def().await;
    r.act(&Caller::local_owner(), r.pick(&[7, 8], "def"))
        .await
        .unwrap();
    r.gh.query("pullRequest(number:$number", read_at_def(Some("SUCCESS")));
    concurrent_update(&r, reply, during_next_read);
    let gh = r.gh.clone();
    r.gh.meanwhile("PATCH /repos/octo/app/pulls/50", move || {
        gh.query("pullRequest(number:$number", read_at_def(None));
    });
    r.advance_together().await;
    assert_eq!(r.calls_to("PUT /repos/octo/app/pulls/7/merge").len(), 1);
    assert!(r.calls_to("PUT /repos/octo/app/pulls/8/merge").is_empty());
    assert_eq!(r.states(), [(7, Merged), (8, Testing)]);
    let made = r.calls_to("POST /repos/octo/app/git/refs");
    assert_eq!(made.len(), 2);
    assert_eq!(made[1].1["sha"], "main1");
    let batch = r.together().unwrap().batch.unwrap();
    assert_eq!(batch.prs, [8]);
    assert!(!batch.green);
    r.advance_together().await;
    assert!(r.calls_to("PUT /repos/octo/app/pulls/8/merge").is_empty());
    r.gh.query("pullRequest(number:$number", read_at_def(Some("SUCCESS")));
    r.advance_together().await;
    assert_eq!(r.states(), [(7, Merged), (8, Merged)]);
    assert_eq!(r.calls_to("PUT /repos/octo/app/pulls/8/merge").len(), 1);
    r.f.close().await;
}

#[tokio::test]
async fn picked_pull_requests_are_tested_together_then_merged_each_in_turn() {
    let r = open_at_def().await;
    let me = Caller::local_owner();
    r.act(&me, r.pick(&[8, 7], "def")).await.unwrap();
    let made = r.gh.called("POST /repos/octo/app/git/refs").unwrap();
    assert_eq!(made["sha"], "main0");
    let branch = (made["ref"].as_str().unwrap())
        .strip_prefix("refs/heads/")
        .unwrap()
        .to_owned();
    assert!(branch.starts_with("ophio/merge-together-"), "{branch}");
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
    assert_eq!(r.states(), [(7, Testing), (8, Testing)]);
    let shown = r.project_view().await.together.unwrap();
    assert_eq!(shown.check, Some(50));
    assert_eq!(shown.prs[0].title, "Make saving atomic");
    assert!(shown.ended_at.is_none());

    // A second can't start while it's on.
    let again = r.act(&me, r.pick(&[9], "def")).await.unwrap_err();
    assert_eq!(again, ErrorCode::Conflict);

    // Nothing merges while its checks run.
    r.advance_together().await;
    assert!(r.calls_to("PUT ").is_empty());

    // Green, each merges at the head that was picked, and the merge check
    // is closed and its branch deleted.
    r.gh.query("pullRequest(number:$number", read_at_def(Some("SUCCESS")));
    r.gh.answer("PUT /repos/octo/app/pulls/7/merge", json!({ "sha": "m7" }));
    r.gh.answer("PUT /repos/octo/app/pulls/8/merge", json!({ "sha": "m8" }));
    let gh = r.gh.clone();
    r.gh.meanwhile("PUT /repos/octo/app/pulls/7/merge", move || {
        gh.answer(
            "GET /repos/octo/app/git/ref/heads/main",
            json!({ "object": { "sha": "m7" } }),
        );
    });
    r.advance_together().await;
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
    assert_eq!(
        r.gh.called("PATCH /repos/octo/app/pulls/50").unwrap()["state"],
        "closed"
    );
    let deleted = format!("DELETE /repos/octo/app/git/refs/heads/{branch}");
    assert_eq!(r.calls_to(&deleted).len(), 1);
    assert_eq!(r.states(), [(7, Merged), (8, Merged)]);
    let shown = r.project_view().await.together.unwrap();
    assert!(shown.ended_at.is_some() && !shown.stopped && shown.check.is_none());

    // Ended, a new one can start, and putting it away clears it.
    r.act(&me, r.stop()).await.unwrap();
    assert!(r.together().is_none());
    assert!(r.project_view().await.together.is_none());
    r.f.close().await;
}

#[tokio::test]
async fn red_is_halved_until_what_broke_it_is_left_out_and_the_rest_merge() {
    let r = open_at_def().await;
    r.act(&Caller::local_owner(), r.pick(&[7, 8], "def"))
        .await
        .unwrap();
    r.gh.query("pullRequest(number:$number", read_at_def(Some("FAILURE")));
    r.gh.answer("POST /repos/octo/app/pulls", json!({ "number": 51 }));
    r.advance_together().await;
    // Halved, and the first half is tested alone at once.
    assert!(r.gh.called("PATCH /repos/octo/app/pulls/50").is_some());
    let opened = r.calls_to("POST /repos/octo/app/pulls");
    assert_eq!(opened.last().unwrap().1["title"], "Øphio merge check: #7");
    assert_eq!(r.states(), [(7, Testing), (8, Waiting)]);
    assert_eq!(r.together().unwrap().groups, [vec![8]]);

    // #7 alone is still red: it's left out with what failed, and #8 is
    // tested next.
    let gh = r.gh.clone();
    r.gh.meanwhile("PATCH /repos/octo/app/pulls/51", move || {
        gh.answer("POST /repos/octo/app/pulls", json!({ "number": 52 }));
    });
    r.advance_together().await;
    assert_eq!(r.states(), [(7, LeftOut), (8, Testing)]);
    assert_eq!(
        r.why(7),
        "Tested with `main` on Øphio's merge check #51, this failed: `test`."
    );
    assert_eq!(r.together().unwrap().view.check, Some(52));

    // #8 alone is green, and merges.
    r.gh.query("pullRequest(number:$number", read_at_def(Some("SUCCESS")));
    r.gh.answer("PUT /repos/octo/app/pulls/8/merge", json!({ "sha": "m8" }));
    r.advance_together().await;
    assert!(r.gh.called("PUT /repos/octo/app/pulls/7/merge").is_none());
    assert_eq!(r.states(), [(7, LeftOut), (8, Merged)]);
    assert!(r.together().unwrap().view.ended_at.is_some());
    r.f.close().await;
}

#[tokio::test]
async fn one_that_conflicts_with_one_before_it_is_left_out() {
    let r = open_at_def().await;
    let gh = r.gh.clone();
    r.gh.meanwhile("POST /repos/octo/app/merges", || {});
    r.gh.meanwhile("POST /repos/octo/app/merges", move || {
        gh.refuse("POST /repos/octo/app/merges", 409);
    });
    r.act(&Caller::local_owner(), r.pick(&[7, 8], "def"))
        .await
        .unwrap();
    let opened = r.gh.called("POST /repos/octo/app/pulls").unwrap();
    assert_eq!(opened["title"], "Øphio merge check: #7");
    assert_eq!(r.states(), [(7, Testing), (8, LeftOut)]);
    assert!(
        r.why(8)
            .starts_with("It conflicts with `main` and #7, which merge before it."),
        "{}",
        r.why(8)
    );
    r.gh.query("pullRequest(number:$number", read_at_def(Some("SUCCESS")));
    r.gh.answer("PUT /repos/octo/app/pulls/7/merge", json!({ "sha": "m7" }));
    r.advance_together().await;
    assert!(r.gh.called("PUT /repos/octo/app/pulls/8/merge").is_none());
    assert_eq!(r.states(), [(7, Merged), (8, LeftOut)]);
    r.f.close().await;
}

#[tokio::test]
async fn one_github_keeps_refusing_is_left_out_and_the_rest_tested_again() {
    let r = open_at_def().await;
    r.act(&Caller::local_owner(), r.pick(&[7, 8], "def"))
        .await
        .unwrap();
    r.gh.query("pullRequest(number:$number", read_at_def(Some("SUCCESS")));
    r.gh.refuse("PUT /repos/octo/app/pulls/7/merge", 405);
    r.gh.answer("PUT /repos/octo/app/pulls/8/merge", json!({ "sha": "m8" }));
    for _ in 0..4 {
        r.advance_together().await;
        assert_eq!(r.states(), [(7, Testing), (8, Testing)]);
    }
    // The fifth time, #7 is left out. #8 alone is green and up to date
    // with `main`, so it merges without another merge check.
    r.advance_together().await;
    assert_eq!(r.states(), [(7, LeftOut), (8, Merged)]);
    assert_eq!(r.why(7), "GitHub refused: Synthetic 405");
    assert_eq!(r.calls_to("POST /repos/octo/app/git/refs").len(), 1);
    assert_eq!(r.calls_to("PUT /repos/octo/app/pulls/7/merge").len(), 5);
    r.f.close().await;
}

#[tokio::test]
async fn one_green_and_up_to_date_merges_at_once() {
    let r = repo(true).await;
    answer_making(&r);
    r.gh.query("pullRequest(number:$number", read_at_def(Some("SUCCESS")));
    r.gh.answer("PUT /repos/octo/app/pulls/7/merge", json!({ "sha": "m7" }));
    r.act(&Caller::local_owner(), r.pick(&[7], "def"))
        .await
        .unwrap();
    assert!(r.gh.called("POST /repos/octo/app/git/refs").is_none());
    assert_eq!(r.states(), [(7, Merged)]);
    assert!(r.together().unwrap().view.ended_at.is_some());
    r.f.close().await;
}

#[tokio::test]
async fn it_starts_only_on_what_the_person_picked_as_they_saw_it() {
    let r = open_at_def().await;
    let me = Caller::local_owner();
    let refused = async |action: GithubAction| r.act(&me, action).await.unwrap_err();
    // A new commit since they looked.
    assert_eq!(refused(r.pick(&[7], "old")).await, ErrorCode::Conflict);
    // Another repository than the one they read.
    let mut elsewhere = r.pick(&[7], "def");
    if let GithubAction::MergeTogether { repo, .. } = &mut elsewhere {
        *repo = "octo/other".into();
    }
    assert_eq!(refused(elsewhere).await, ErrorCode::Conflict);
    // None, too many, or one twice.
    assert_eq!(refused(r.pick(&[], "def")).await, ErrorCode::InvalidRequest);
    let many: Vec<u64> = (1..=21).collect();
    assert_eq!(
        refused(r.pick(&many, "def")).await,
        ErrorCode::InvalidRequest
    );
    assert_eq!(
        refused(r.pick(&[7, 7], "def")).await,
        ErrorCode::InvalidRequest
    );
    // A fork's, or one into another branch.
    let mut fork = read_at_def(None);
    fork["repository"]["pullRequest"]["isCrossRepository"] = json!(true);
    r.gh.query("pullRequest(number:$number", fork);
    assert_eq!(
        refused(r.pick(&[7], "def")).await,
        ErrorCode::InvalidRequest
    );
    let mut other = read_at_def(None);
    other["repository"]["pullRequest"]["baseRefName"] = json!("dev");
    r.gh.query("pullRequest(number:$number", other);
    assert_eq!(
        refused(r.pick(&[7], "def")).await,
        ErrorCode::InvalidRequest
    );
    // Not from a chat, even one allowed everything.
    r.gh.query("pullRequest(number:$number", read_at_def(None));
    let chat = Caller {
        principal: Principal::Channel {
            identity_id: ChannelIdentityId::new(),
        },
        grants: Grants::owner(),
    };
    let e = (r.f.hub.github_action(&chat, &r.pick(&[7], "def")))
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::PermissionDenied);
    assert!(r.together().is_none());
    assert!(r.gh.called("POST /repos/octo/app/git/refs").is_none());
    r.f.close().await;
}

#[tokio::test]
async fn stopped_its_merge_check_is_closed_and_nothing_more_merges() {
    let r = open_at_def().await;
    let me = Caller::local_owner();
    r.act(&me, r.pick(&[7, 8], "def")).await.unwrap();
    r.act(&me, r.stop()).await.unwrap();
    assert_eq!(
        r.gh.called("PATCH /repos/octo/app/pulls/50").unwrap()["state"],
        "closed"
    );
    assert_eq!(
        r.calls_to("DELETE /repos/octo/app/git/refs/heads/").len(),
        1
    );
    assert_eq!(r.states(), [(7, LeftOut), (8, LeftOut)]);
    assert_eq!(r.why(8), "You stopped it.");
    let shown = r.project_view().await.together.unwrap();
    assert!(shown.stopped && shown.ended_at.is_some());
    r.gh.query("pullRequest(number:$number", read_at_def(Some("SUCCESS")));
    r.advance_together().await;
    assert!(r.calls_to("PUT ").is_empty());
    r.f.close().await;
}

#[tokio::test]
async fn stop_during_a_merge_keeps_its_result_and_leaves_the_rest_unmerged() {
    let r = open_at_def().await;
    r.act(&Caller::local_owner(), r.pick(&[7, 8], "def"))
        .await
        .unwrap();
    r.gh.query("pullRequest(number:$number", read_at_def(Some("SUCCESS")));
    r.gh.answer("PUT /repos/octo/app/pulls/7/merge", json!({ "sha": "m7" }));
    r.gh.answer("PUT /repos/octo/app/pulls/8/merge", json!({ "sha": "m8" }));
    r.stop_during("PUT /repos/octo/app/pulls/7/merge", r.advance_together())
        .await;
    assert!(r.gh.called("PUT /repos/octo/app/pulls/8/merge").is_none());
    assert_eq!(r.states(), [(7, Merged), (8, LeftOut)]);
    assert_eq!(r.why(8), "You stopped it.");
    let shown = r.project_view().await.together.unwrap();
    assert!(shown.stopped && shown.ended_at.is_some());
    r.advance_together().await;
    assert_eq!(r.calls_to("PUT ").len(), 1);
    r.f.close().await;
}

#[tokio::test]
async fn a_restart_after_stop_reconciles_the_dispatched_merge() {
    for stop_again in [false, true] {
        let r = interrupted_merge_after_stop().await;
        let mut merged = read_at_def(Some("SUCCESS"));
        merged["repository"]["pullRequest"]["state"] = json!("MERGED");
        r.gh.query("pullRequest(number:$number", merged);
        if stop_again {
            r.f.hub.stop_together(r.project_id).await.unwrap();
        } else {
            r.advance_together().await;
        }
        assert_eq!(r.states(), [(7, Merged), (8, LeftOut)]);
        assert_eq!(r.why(8), "You stopped it.");
        let shown = r.project_view().await.together.unwrap();
        assert!(shown.stopped && shown.ended_at.is_some());
        r.advance_together().await;
        assert_eq!(r.calls_to("PUT ").len(), 1);
        r.f.close().await;
    }
}

/// Lose the active merge's response and both callers, then reopen the store.
async fn interrupted_merge_after_stop() -> Repo {
    use std::future::{Future, poll_fn};
    use std::task::Poll;

    let r = open_at_def().await;
    r.act(&Caller::local_owner(), r.pick(&[7, 8], "def"))
        .await
        .unwrap();
    r.gh.query("pullRequest(number:$number", read_at_def(Some("SUCCESS")));
    {
        let (started, _release) = r.gh.pause("PUT /repos/octo/app/pulls/7/merge");
        let advance = r.advance_together();
        tokio::pin!(advance);
        tokio::select! {
            result = started => result.unwrap(),
            _ = &mut advance => panic!("merge finished before its response was released"),
        }
        let stop = r.f.hub.stop_together(r.project_id);
        tokio::pin!(stop);
        poll_fn(|cx| {
            assert!(stop.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        assert!(r.together().unwrap().stop_requested);
        // Neither future is polled again: GitHub can finish the merge while
        // the companion loses its in-memory progress and reopens below.
    }
    let Repo { f, gh, project_id } = r;
    let port = gh.clone();
    Repo {
        f: f.reopened(|b| b.github(port)).await,
        gh,
        project_id,
    }
}

#[tokio::test]
async fn stopped_merge_recovery_waits_for_a_known_outcome_without_merge_writes() {
    for final_state in ["MERGED", "CLOSED"] {
        let r = interrupted_merge_after_stop().await;
        r.gh.fail_query("pullRequest(number:$number");
        assert!(r.f.hub.advance_together(r.project_id).await.is_err());
        let kept = r.together().unwrap();
        assert!(kept.stop_requested && kept.view.stopped && kept.view.ended_at.is_none());
        assert_eq!(kept.dispatched, Some(7));
        assert_eq!(r.states(), [(7, Testing), (8, LeftOut)]);

        // An open PR can still finish the request sent before the restart.
        r.gh.query("pullRequest(number:$number", read_at_def(Some("SUCCESS")));
        r.advance_together().await;
        r.f.hub.stop_together(r.project_id).await.unwrap();
        assert!(r.together().unwrap().view.ended_at.is_none());
        assert_eq!(r.together().unwrap().dispatched, Some(7));
        assert_eq!(r.calls_to("PUT ").len(), 1);
        assert!(r.calls_to("PATCH ").is_empty());

        let mut settled = read_at_def(Some("SUCCESS"));
        settled["repository"]["pullRequest"]["state"] = json!(final_state);
        r.gh.query("pullRequest(number:$number", settled);
        r.advance_together().await;
        assert_eq!(
            r.states(),
            [
                (
                    7,
                    if final_state == "MERGED" {
                        Merged
                    } else {
                        LeftOut
                    }
                ),
                (8, LeftOut)
            ]
        );
        assert!(r.together().unwrap().view.ended_at.is_some());
        assert_eq!(r.together().unwrap().dispatched, None);
        assert_eq!(r.calls_to("PUT ").len(), 1);
        r.f.close().await;
    }
}

#[tokio::test]
async fn stop_during_the_last_merge_keeps_the_finished_result_until_done() {
    let r = open_at_def().await;
    let me = Caller::local_owner();
    r.act(&me, r.pick(&[7], "def")).await.unwrap();
    r.gh.query("pullRequest(number:$number", read_at_def(Some("SUCCESS")));
    r.gh.answer("PUT /repos/octo/app/pulls/7/merge", json!({ "sha": "m7" }));
    r.stop_during("PUT /repos/octo/app/pulls/7/merge", r.advance_together())
        .await;
    assert_eq!(r.states(), [(7, Merged)]);
    let shown = r.project_view().await.together.unwrap();
    assert!(shown.stopped && shown.ended_at.is_some());
    r.act(&me, r.stop()).await.unwrap();
    assert!(r.together().is_none());
    r.f.close().await;
}

#[tokio::test]
async fn stop_before_dispatch_leaves_every_pull_request_unmerged() {
    for during in [
        "GET /repos/octo/app/git/ref/heads/main",
        "pullRequest(number:$number",
        "markPullRequestReadyForReview",
    ] {
        let r = open_at_def().await;
        r.act(&Caller::local_owner(), r.pick(&[7, 8], "def"))
            .await
            .unwrap();
        let mut ready = read_at_def(Some("SUCCESS"));
        ready["repository"]["pullRequest"]["isDraft"] = json!(true);
        r.gh.query("pullRequest(number:$number", ready);
        r.gh.query("markPullRequestReadyForReview", json!({}));
        r.stop_during(during, r.advance_together()).await;
        assert!(r.calls_to("PUT ").is_empty(), "{during}");
        assert_eq!(r.states(), [(7, LeftOut), (8, LeftOut)], "{during}");
        assert!(r.together().unwrap().view.stopped);
        r.f.close().await;
    }
}

#[tokio::test]
async fn stop_during_a_refused_or_uncertain_merge_never_merges_the_next_one() {
    for code in [405, 502] {
        let r = open_at_def().await;
        r.act(&Caller::local_owner(), r.pick(&[7, 8], "def"))
            .await
            .unwrap();
        r.gh.query("pullRequest(number:$number", read_at_def(Some("SUCCESS")));
        r.gh.refuse("PUT /repos/octo/app/pulls/7/merge", code);
        // An uncertain response is reconciled even after Stop.
        if code == 502 {
            let gh = r.gh.clone();
            r.gh.meanwhile("PUT /repos/octo/app/pulls/7/merge", move || {
                let mut merged = read_at_def(Some("SUCCESS"));
                merged["repository"]["pullRequest"]["state"] = json!("MERGED");
                gh.query("pullRequest(number:$number", merged);
            });
        }
        r.stop_during("PUT /repos/octo/app/pulls/7/merge", r.advance_together())
            .await;
        assert_eq!(
            r.states(),
            [
                (7, if code == 502 { Merged } else { LeftOut }),
                (8, LeftOut)
            ]
        );
        assert_eq!(r.calls_to("PUT ").len(), 1);
        assert!(r.together().unwrap().view.stopped);
        r.f.close().await;
    }
}

#[tokio::test]
async fn stop_during_preparation_closes_the_check_without_more_testing() {
    for during in [
        "POST /repos/octo/app/git/refs",
        "POST /repos/octo/app/merges",
        "POST /repos/octo/app/pulls",
    ] {
        let r = open_at_def().await;
        let start = async {
            r.f.hub
                .merge_together(
                    r.project_id,
                    ("octo", "app"),
                    &[
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
                )
                .await
                .unwrap();
        };
        r.stop_during(during, start).await;
        assert!(r.calls_to("PUT ").is_empty());
        assert_eq!(r.states(), [(7, LeftOut), (8, LeftOut)]);
        assert!(r.together().unwrap().view.stopped);
        assert_eq!(
            r.calls_to("DELETE /repos/octo/app/git/refs/heads/").len(),
            1
        );
        match during {
            "POST /repos/octo/app/git/refs" => {
                assert!(r.calls_to("POST /repos/octo/app/merges").is_empty())
            }
            "POST /repos/octo/app/merges" => {
                assert_eq!(r.calls_to("POST /repos/octo/app/merges").len(), 1);
                assert!(r.calls_to("POST /repos/octo/app/pulls").is_empty());
            }
            _ => assert_eq!(
                r.gh.called("PATCH /repos/octo/app/pulls/50").unwrap()["state"],
                "closed"
            ),
        }
        r.f.close().await;
    }
}

#[tokio::test]
async fn a_recorded_stop_survives_its_caller_and_a_new_operation_starts_fresh() {
    use std::future::{Future, poll_fn};
    use std::task::Poll;

    let r = open_at_def().await;
    let me = Caller::local_owner();
    r.act(&me, r.pick(&[7, 8], "def")).await.unwrap();
    {
        let held = r.f.hub.inner.together.lock().await;
        let stop = r.f.hub.stop_together(r.project_id);
        tokio::pin!(stop);
        poll_fn(|cx| {
            assert!(stop.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        drop(held);
        // Drop the caller before cleanup. A later pass uses the stored request.
    }
    r.gh.query("pullRequest(number:$number", read_at_def(Some("SUCCESS")));
    r.advance_together().await;
    assert_eq!(r.states(), [(7, LeftOut), (8, LeftOut)]);
    assert!(r.together().unwrap().view.stopped);
    assert!(r.calls_to("PUT ").is_empty());
    r.gh.answer("PUT /repos/octo/app/pulls/7/merge", json!({ "sha": "m7" }));
    r.act(&me, r.pick(&[7], "def")).await.unwrap();
    assert_eq!(r.states(), [(7, Merged)]);
    assert!(!r.together().unwrap().view.stopped);
    assert!(!r.together().unwrap().stop_requested);
    r.act(&me, r.stop()).await.unwrap();
    assert!(r.together().is_none());
    r.f.close().await;
}

#[tokio::test]
async fn a_merge_check_closed_on_github_stops_it() {
    let r = open_at_def().await;
    r.act(&Caller::local_owner(), r.pick(&[7, 8], "def"))
        .await
        .unwrap();
    let mut closed = read_at_def(Some("SUCCESS"));
    closed["repository"]["pullRequest"]["state"] = json!("CLOSED");
    r.gh.query("pullRequest(number:$number", closed);
    r.advance_together().await;
    assert_eq!(r.states(), [(7, LeftOut), (8, LeftOut)]);
    assert_eq!(
        r.why(7),
        "Øphio's merge check #50 was closed on GitHub, so it stopped."
    );
    assert!(r.calls_to("PUT ").is_empty());
    r.f.close().await;
}

#[tokio::test]
async fn the_line_of_through_merge_waits_while_it_is_on() {
    let (r, _, _) = waiting_7_and_8().await;
    r.gh.query("pullRequest(number:$number", read_at_def(None));
    let me = Caller::local_owner();
    r.act(&me, r.pick(&[7, 8], "def")).await.unwrap();
    assert_eq!(r.calls_to("POST /repos/octo/app/git/refs").len(), 1);
    let settings = r.settings();
    let advance = async || {
        (r.f.hub.advance_train(r.project_id, &settings))
            .await
            .unwrap()
    };
    advance().await;
    assert_eq!(r.calls_to("POST /repos/octo/app/git/refs").len(), 1);
    r.act(&me, r.stop()).await.unwrap();
    advance().await;
    assert_eq!(r.calls_to("POST /repos/octo/app/git/refs").len(), 2);
    r.f.close().await;
}

/// What `task` was told that waits for the person, with whether it was quiet.
fn said_needs_you(r: &Repo, task: TaskId) -> Vec<(String, Option<String>, bool)> {
    (r.attention(task).into_iter())
        .filter(|a| a.resolved_at.is_none() && a.kind == AttentionKind::WaitingForYou)
        .map(|a| (a.title, a.body, a.quiet))
        .collect()
}

#[tokio::test]
async fn all_it_merged_is_told_once_on_a_task_that_speaks_for_one() {
    let (r, seven, eight) = waiting_7_and_8().await;
    r.gh.query("pullRequest(number:$number", read_at_def(None));
    r.act(&Caller::local_owner(), r.pick(&[7, 8], "def"))
        .await
        .unwrap();
    r.gh.query("pullRequest(number:$number", read_at_def(Some("SUCCESS")));
    r.gh.answer("PUT /repos/octo/app/pulls/7/merge", json!({ "sha": "m7" }));
    r.gh.answer("PUT /repos/octo/app/pulls/8/merge", json!({ "sha": "m8" }));
    r.advance_together().await;
    r.advance_together().await;
    assert_eq!(r.states(), [(7, Merged), (8, Merged)]);
    assert_eq!(said_merged(&r, seven), []);
    assert_eq!(
        said_merged(&r, eight),
        [("PRs #7 and #8 merged".into(), false)]
    );
    r.f.close().await;
}

#[tokio::test]
async fn what_it_left_out_is_told_with_why_and_what_merged() {
    let (r, seven, eight) = waiting_7_and_8().await;
    r.gh.query("pullRequest(number:$number", read_at_def(None));
    let gh = r.gh.clone();
    r.gh.meanwhile("POST /repos/octo/app/merges", || {});
    r.gh.meanwhile("POST /repos/octo/app/merges", move || {
        gh.refuse("POST /repos/octo/app/merges", 409);
    });
    r.act(&Caller::local_owner(), r.pick(&[7, 8], "def"))
        .await
        .unwrap();
    r.gh.query("pullRequest(number:$number", read_at_def(Some("SUCCESS")));
    r.gh.answer("PUT /repos/octo/app/pulls/7/merge", json!({ "sha": "m7" }));
    r.advance_together().await;
    assert_eq!(r.states(), [(7, Merged), (8, LeftOut)]);
    let told = said_needs_you(&r, eight);
    let [(title, Some(body), false)] = told.as_slice() else {
        panic!("{told:?}")
    };
    assert_eq!(title, "Merge together: 1 merged, 1 left out");
    assert!(
        body.starts_with("#8: It conflicts with `main` and #7"),
        "{body}"
    );
    assert_eq!(said_merged(&r, seven), []);
    assert_eq!(said_needs_you(&r, seven), []);
    r.f.close().await;
}

#[tokio::test]
async fn stopped_by_the_person_it_tells_them_nothing() {
    let (r, seven, eight) = waiting_7_and_8().await;
    r.gh.query("pullRequest(number:$number", read_at_def(None));
    let me = Caller::local_owner();
    r.act(&me, r.pick(&[7, 8], "def")).await.unwrap();
    r.act(&me, r.stop()).await.unwrap();
    assert_eq!(r.states(), [(7, LeftOut), (8, LeftOut)]);
    for task in [seven, eight] {
        assert_eq!(said_merged(&r, task), []);
        assert_eq!(said_needs_you(&r, task), []);
    }
    r.f.close().await;
}
