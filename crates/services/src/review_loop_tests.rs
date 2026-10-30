// SPDX-License-Identifier: Apache-2.0
//! What a review finds is checked before anything comes of it, and a pull
//! request is reviewed again as it changes, against GitHub as recorded.
use super::github_after_tests::{PASSED, checking_7, checks_of_7, test_check, watched_pr};
use super::*;
use crate::github_watch::REVIEW;
use crate::review_loop::{VERIFYING, Verifying};
use protocol::review::{
    EvidenceKind, FindingDisposition, FindingVerdict, PrReview, ReviewFinding, ReviewRole,
    SensitiveArea,
};
use protocol::{
    command::{Reconciliation, TaskPlacement},
    queue::{QueueAuthorization, QueueView},
};

/// A finder over the whole project that only files what it finds.
fn finder() -> GithubLink {
    GithubLink::Find {
        scope: FindScope::Project,
        focus: None,
        max_issues: None,
        fix_findings: Some(FixFindings::FileOnly),
        fix_with: claude(),
    }
}

/// What a task was first asked.
fn asked(r: &Repo, task: TaskId) -> String {
    let prompt =
        r.f.hub
            .read(|x| x.get::<(String, Option<Value>)>("prompt", task));
    prompt.unwrap().unwrap().0
}

/// Every body Øphio sent to GitHub, as text.
fn sent(r: &Repo) -> String {
    let calls = r.gh.calls.lock().unwrap();
    (calls.iter())
        .filter(|(c, _)| c.starts_with("POST") || c.starts_with("PATCH"))
        .map(|(_, v)| v.to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Ends `run` of `task` as failed, and waits until what follows is done.
async fn fails(r: &Repo, task: TaskId, run: RunId) {
    let failed = ports::HarnessEvent::Exited {
        exit: ports::RunExit::Failed {
            reason: "Synthetic".into(),
        },
        outcome: None,
    };
    r.f.harness.emit(run, failed).await;
    let mut more = waiting();
    while r.get(task).state != TaskState::Failed {
        assert!(more(), "the run didn't end");
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    r.settled().await;
}

/// A finding one round confirmed, as the pull request's review keeps it.
fn kept(title: &str, disposition: FindingDisposition, round: u32) -> ReviewFinding {
    ReviewFinding {
        title: title.into(),
        path: Some("src/save.rs".into()),
        line: Some(9),
        severity: None,
        scenario: Some("Kill it while it renames the file".into()),
        expected: None,
        actual: None,
        verdict: Some(FindingVerdict::Confirmed),
        evidence: Some(EvidenceKind::SourceReasoning),
        reason: None,
        revision: Some("bbb".into()),
        disposition,
        round,
    }
}

/// Pull request #7, green at `head`, its task done and its review as
/// `review` left it, with `src/save.rs` changed since `bbb`.
async fn reviewed_7(review: PrReview, head: &str) -> (Repo, TaskId) {
    let r = repo(true).await;
    r.set(drafts()).await.unwrap();
    let mut at = pull(7, "octo/app", "me");
    at["head"]["sha"] = json!(head);
    r.gh.answer("GET /repos/octo/app/pulls/7", at);
    let (task, run) = r.start(GithubLink::Pr { number: 7 }, "Finish it").await;
    r.ends(task, run, None).await;
    r.alter(task, |g| {
        g.stage = Some(PrStage::Babysitting);
        g.review = Some(review);
    });
    r.gh.query(
        "viewer{login}",
        watched_pr(head, test_check("SUCCESS"), json!([]), "CLEAN"),
    );
    let save = json!([{ "filename": "src/save.rs", "status": "modified", "additions": 1,
                        "deletions": 1, "patch": "@@ -9 +9 @@\n-rename\n+sync_and_rename" }]);
    r.gh.answer(
        "GET /repos/octo/app/pulls/7/files?per_page=100&page=1",
        save.clone(),
    );
    r.gh.answer(
        &format!("GET /repos/octo/app/compare/bbb...{head}"),
        json!({ "status": "ahead", "files": save,
                "commits": [{ "sha": head, "commit": { "message": "Sync first" }, "parents": [{ "sha": "bbb" }] }] }),
    );
    (r, task)
}

/// The watcher's next pass, `minutes` on.
async fn tick(r: &Repo, minutes: u64) {
    r.f.clock.advance(Duration::from_secs(minutes * 60));
    r.f.hub.watch_prs().await.unwrap();
}

#[tokio::test]
async fn only_what_the_verifier_confirms_is_filed_and_the_rest_is_kept_with_its_reason() {
    let r = repo(true).await;
    r.set(drafts()).await.unwrap();
    let (found_by, run) = r.start(finder(), "Find problems").await;
    r.open_issues(json!([]));
    r.gh.answer(
        "POST /repos/octo/app/issues",
        json!({ "number": 31, "html_url": "https://github.com/octo/app/issues/31" }),
    );
    r.gh.answer("POST /repos/octo/app/labels", json!({}));
    // The last tells the first's failure again, a few lines on, under
    // another title.
    let findings = json!([
        { "title": "Saving loses data", "body": "Kill it mid-save.", "severity": "high",
          "path": "src/save.rs", "line": 9, "scenario": "Kill the app while it saves a note" },
        { "title": "Undo is slow", "body": "It takes a second.", "severity": "medium" },
        { "title": "Export drops emoji", "body": "Titles with emoji.", "severity": "low" },
        { "title": "A crash while saving can lose the note", "body": "Same.", "severity": "low",
          "path": "./src/save.rs", "line": 12, "scenario": "Kill the app while it saves the note" },
    ]);
    let said = format!("```ophio-findings\n{findings}\n```");
    r.ends(found_by, run, Some(&said)).await;
    // Nothing reaches GitHub before the verifier has answered.
    assert!(r.gh.called("POST /repos/octo/app/issues").is_none());
    let (verifier, checking) = r.verifier(found_by).await;
    let prompt = asked(&r, verifier);
    assert!(
        prompt.contains("Undo is slow")
            && prompt.contains("Export drops emoji")
            && !prompt.contains("A crash while saving")
            && prompt.contains("Try to disprove it")
            && prompt.contains("`source_reasoning`"),
        "{prompt}"
    );
    // A review of the whole project isn't asked what was there before.
    assert!(!prompt.contains("Was it there before"), "{prompt}");
    let verdicts = json!([
        { "index": 1, "verdict": "confirmed", "evidence": "reproduced",
          "reason": "A test that kills it mid-save lost the note." },
        { "index": 2, "verdict": "rejected", "evidence": "source_reasoning",
          "reason": "Intended: it waits on purpose." },
        { "index": 9, "verdict": "confirmed" },
    ]);
    let answer = format!("```ophio-verdicts\n{verdicts}\n```");
    r.ends(verifier, checking, Some(&answer)).await;

    // Only the confirmed one is filed; GitHub never sees the others.
    assert_eq!(r.times("POST /repos/octo/app/issues"), 1);
    let made = r.gh.called("POST /repos/octo/app/issues").unwrap();
    assert_eq!(made["title"], "Saving loses data");
    let posted = sent(&r);
    assert!(
        !posted.contains("Undo is slow") && !posted.contains("Export drops emoji"),
        "{posted}"
    );
    // Every finding is kept, with its verdict and why.
    let review = r.get(found_by).review.unwrap();
    assert_eq!(review.verified_by, Some(verifier));
    let checked = r.get(verifier).review.unwrap();
    assert_eq!(
        (checked.role, checked.of_task),
        (ReviewRole::Verify, Some(found_by))
    );
    let by_title = |t: &str| review.findings.iter().find(|f| f.title == t).unwrap();
    let saving = by_title("Saving loses data");
    assert_eq!(
        (saving.verdict, saving.evidence, saving.disposition.clone()),
        (
            Some(FindingVerdict::Confirmed),
            Some(EvidenceKind::Reproduced),
            FindingDisposition::Filed { number: 31 }
        )
    );
    let undo = by_title("Undo is slow");
    assert_eq!(
        (
            undo.verdict,
            undo.disposition.clone(),
            undo.reason.as_deref()
        ),
        (
            Some(FindingVerdict::Rejected),
            FindingDisposition::SetAside,
            Some("Intended: it waits on purpose.")
        )
    );
    let export = by_title("Export drops emoji");
    assert_eq!(export.verdict, Some(FindingVerdict::Uncertain));
    assert_eq!(export.disposition, FindingDisposition::SetAside);
    let again = by_title("A crash while saving can lose the note");
    assert_eq!(again.disposition, FindingDisposition::SetAside);
    assert!(
        again
            .reason
            .as_deref()
            .is_some_and(|r| r.contains("The same problem as “Saving loses data”")),
        "{again:?}"
    );
    // Told once, on the finder, with what was set aside and why.
    assert!(r.attention(verifier).is_empty());
    let [told] = &r.attention(found_by)[..] else {
        panic!("{:?}", r.attention(found_by))
    };
    // The repeat, left out before checking, is counted too.
    assert_eq!(
        told.title,
        "Found 4, filed #31, set 2 aside after checking, left 1 unchecked"
    );
    let body = told.body.as_deref().unwrap();
    assert!(
        body.contains("Not filed, rejected when checked: “Undo is slow”: Intended")
            && body.contains("Not filed, uncertain when checked: “Export drops emoji”")
            && body.contains("1 wasn't checked or filed: it repeats another finding"),
        "{body}"
    );
    r.f.close().await;
}

#[tokio::test]
async fn what_came_of_each_filed_finding_is_kept_whatever_its_title() {
    let r = repo(true).await;
    r.set(drafts()).await.unwrap();
    let (found_by, run) = r.start(finder(), "Find problems").await;
    r.open_issues(json!([]));
    r.gh.answer(
        "POST /repos/octo/app/issues",
        json!({ "number": 31, "html_url": "https://github.com/octo/app/issues/31" }),
    );
    r.gh.answer("POST /repos/octo/app/labels", json!({}));
    // Spaces around one title; two long ones the same as far as an
    // issue's title goes, about different code.
    let long = "Saving a note with a very long name ".repeat(4);
    let findings = json!([
        { "title": "  Undo is slow  ", "body": "It takes a second.", "severity": "high",
          "path": "src/undo.rs", "line": 3, "scenario": "Press undo after typing" },
        { "title": format!("{long}first"), "body": "One.", "severity": "medium",
          "path": "src/a.rs", "line": 1, "scenario": "Open the settings" },
        { "title": format!("{long}second"), "body": "Two.", "severity": "medium",
          "path": "src/b.rs", "line": 80, "scenario": "Rotate the phone twice" },
    ]);
    let said = format!("```ophio-findings\n{findings}\n```");
    r.ends(found_by, run, Some(&said)).await;
    r.confirms(found_by, 3).await;
    assert_eq!(r.times("POST /repos/octo/app/issues"), 2);
    let review = r.get(found_by).review.unwrap();
    let came: Vec<_> = (review.findings.iter())
        .map(|f| f.disposition.clone())
        .collect();
    assert_eq!(
        came,
        [
            FindingDisposition::Filed { number: 31 },
            FindingDisposition::Filed { number: 31 },
            FindingDisposition::SetAside,
        ],
        "{review:?}"
    );
    r.f.close().await;
}

#[tokio::test]
async fn confirmed_findings_that_couldnt_be_filed_are_told_once_and_kept() {
    let r = repo(true).await;
    r.set(drafts()).await.unwrap();
    let (found_by, run) = r.start(finder(), "Find problems").await;
    let said =
        "```ophio-findings\n[{\"title\": \"Saving loses data\", \"severity\": \"high\"}]\n```";
    r.ends(found_by, run, Some(said)).await;
    let (verifier, _) = r.verifier(found_by).await;
    // GitHub fails for a moment just as filing starts.
    r.gh.refuse("GET /user", 502);
    r.confirms(found_by, 1).await;
    assert!(r.gh.called("POST /repos/octo/app/issues").is_none());
    assert!(r.attention(verifier).is_empty());
    let [told] = &r.attention(found_by)[..] else {
        panic!("{:?}", r.attention(found_by))
    };
    assert_eq!(
        (told.kind, told.title.as_str()),
        (
            AttentionKind::WaitingForYou,
            "The finder's findings weren't filed"
        )
    );
    assert!(
        (told.body.as_deref()).is_some_and(
            |b| b.contains("the finding it confirmed wasn't filed. They're in the task.")
        ),
        "{told:?}"
    );
    let kept = &r.get(found_by).review.unwrap().findings[0];
    assert_eq!(
        (kept.verdict, kept.disposition.clone()),
        (Some(FindingVerdict::Confirmed), FindingDisposition::Open)
    );
    r.f.close().await;
}

#[tokio::test]
async fn a_verifier_that_fails_or_cant_be_read_holds_the_findings_for_the_person() {
    let r = repo(true).await;
    r.set(drafts()).await.unwrap();
    let said =
        "```ophio-findings\n[{\"title\": \"Saving loses data\", \"severity\": \"high\"}]\n```";
    // It fails.
    let (first, run) = r.start(finder(), "Find problems").await;
    r.ends(first, run, Some(said)).await;
    let (verifier, checking) = r.verifier(first).await;
    fails(&r, verifier, checking).await;
    assert!(r.gh.called("POST /repos/octo/app/issues").is_none());
    assert!(r.attention(verifier).is_empty());
    let [held] = &r.attention(first)[..] else {
        panic!("{:?}", r.attention(first))
    };
    assert_eq!(
        (held.kind, held.title.as_str()),
        (
            AttentionKind::WaitingForYou,
            "The finder's findings weren't checked"
        )
    );
    assert!(
        (held.body.as_deref()).is_some_and(|b| b.contains("The verifier didn't finish")),
        "{held:?}"
    );
    let kept = &r.get(first).review.unwrap().findings[0];
    assert_eq!(
        (kept.verdict, kept.disposition.clone()),
        (None, FindingDisposition::Open)
    );
    // It answers without its block.
    let (second, run) = r.start(finder(), "Find problems").await;
    r.ends(second, run, Some(said)).await;
    let (verifier, checking) = r.verifier(second).await;
    r.ends(verifier, checking, Some("They all look real to me."))
        .await;
    r.settled().await;
    assert!(r.gh.called("POST /repos/octo/app/issues").is_none());
    let [held] = &r.attention(second)[..] else {
        panic!()
    };
    assert!(
        (held.body.as_deref()).is_some_and(|b| b.contains("answer couldn't be read")),
        "{held:?}"
    );
    r.f.close().await;
}

#[tokio::test]
async fn a_verification_left_part_way_is_carried_on_with_one_verifier() {
    let r = repo(true).await;
    r.set(drafts()).await.unwrap();
    r.open_issues(json!([]));
    r.gh.answer(
        "POST /repos/octo/app/issues",
        json!({ "number": 31, "html_url": "https://github.com/octo/app/issues/31" }),
    );
    r.gh.answer("POST /repos/octo/app/labels", json!({}));
    let (found_by, run) = r.start(finder(), "Find problems").await;
    let said =
        "```ophio-findings\n[{\"title\": \"Saving loses data\", \"severity\": \"high\"}]\n```";
    r.ends(found_by, run, Some(said)).await;
    let (verifier, _) = r.verifier(found_by).await;
    let tasks = || r.f.hub.read(|x| x.list::<Task>("task")).unwrap().len();
    let before = tasks();
    // As a restart that stopped between creating it and keeping it would
    // leave it.
    (r.f.hub.write(|tx| {
        let mut v: Verifying = tx.read().require(VERIFYING, found_by)?;
        v.verifier = None;
        v.claimed = None;
        tx.put(VERIFYING, found_by, &v)
    }))
    .unwrap();
    r.f.hub.resume_reviews().await.unwrap();
    // The same verifier is found again, and none is added.
    assert_eq!(tasks(), before);
    let v: Verifying = (r.f.hub.read(|x| x.require(VERIFYING, found_by))).unwrap();
    assert_eq!(v.verifier, Some(verifier));
    // Acting on the finder's end again starts none either.
    (r.f.hub.write(|tx| {
        let after = crate::github_after::After {
            run_id: run,
            claimed: None,
        };
        tx.put(crate::github_after::AFTER, run, &after)
    }))
    .unwrap();
    r.f.hub.github_after_pending().await.unwrap();
    assert_eq!(tasks(), before);
    r.confirms(found_by, 1).await;
    assert_eq!(r.times("POST /repos/octo/app/issues"), 1);
    r.f.close().await;
}

#[tokio::test]
async fn a_check_posts_and_hands_over_only_what_was_confirmed() {
    let (r, task, check, check_run) = checking_7(drafts()).await;
    r.gh.answer(
        "POST /repos/octo/app/issues/7/comments",
        json!({ "html_url": "https://github.com/octo/app/pull/7#issuecomment-2" }),
    );
    let said = json!({
        "findings": [
            { "title": "Rename isn't fsynced", "body": "A crash can lose it.", "path": "src/save.rs", "line": 9 },
            { "title": "The name is long", "body": "Style." },
        ],
        "not_run": ["The Android screen check."],
        "sensitive": ["Update permissions."],
    });
    r.ends(
        check.task_id,
        check_run,
        Some(&format!("```ophio-review\n{said}\n```")),
    )
    .await;
    r.judges(check.task_id, &["confirmed", "rejected"]).await;
    let posted =
        r.gh.called("POST /repos/octo/app/issues/7/comments")
            .unwrap();
    let body = posted["body"].as_str().unwrap();
    assert!(
        body.contains("Rename isn't fsynced") && !body.contains("The name is long"),
        "{body}"
    );
    let handed: String = r.f.hub.read(|x| x.require(REVIEW, task)).unwrap();
    assert!(
        handed.contains("Rename isn't fsynced") && !handed.contains("The name is long"),
        "{handed}"
    );
    let github = r.get(task).github.unwrap();
    let review = github.review.unwrap();
    assert_eq!(
        (review.round, review.reviewed_head.as_deref()),
        (1, Some("bbb"))
    );
    assert_eq!(review.task, Some(check.task_id));
    assert_eq!(
        (review.reviewer.clone(), review.verifier.clone()),
        (Some(claude()), Some(claude()))
    );
    assert_eq!(review.not_run, ["the Android screen check"]);
    let [rename, name] = &review.findings[..] else {
        panic!("{review:?}")
    };
    assert_eq!(rename.disposition, FindingDisposition::Fixing);
    assert_eq!(
        (
            name.verdict,
            name.disposition.clone(),
            name.reason.as_deref()
        ),
        (
            Some(FindingVerdict::Rejected),
            FindingDisposition::SetAside,
            Some("Reason 2.")
        )
    );
    // What the reviewer raised waits for the person's approval of this commit.
    let need = github.approval.unwrap();
    assert_eq!(need.head, "bbb");
    assert!(
        need.areas.contains(&SensitiveArea::Reviewer {
            reason: "update permissions".into()
        }),
        "{need:?}"
    );
    // The check keeps what it found, judged.
    let own = r.get(check.task_id).review.unwrap();
    assert_eq!(own.findings.len(), 2);
    assert_eq!(own.not_run, ["the Android screen check"]);
    r.f.close().await;
}

#[tokio::test]
async fn a_base_merged_in_alone_isnt_reviewed_again() {
    let (r, task, check, check_run) = checking_7(drafts()).await;
    r.ends(check.task_id, check_run, Some(PASSED)).await;
    // Only `main` merged in, in a file the change doesn't touch.
    r.gh.query(
        "viewer{login}",
        watched_pr("ccc", test_check("SUCCESS"), json!([]), "CLEAN"),
    );
    r.gh.answer(
        "GET /repos/octo/app/compare/bbb...ccc",
        json!({ "status": "ahead",
                "commits": [{ "sha": "ccc", "commit": { "message": "Merge main" }, "parents": [{ "sha": "bbb" }, { "sha": "m" }] }],
                "files": [{ "filename": "docs/other.md", "patch": "+more" }] }),
    );
    tick(&r, 11).await;
    tick(&r, 11).await;
    let github = r.get(task).github.unwrap();
    assert_eq!(github.stage, Some(PrStage::Ready));
    let review = github.review.unwrap();
    assert_eq!(
        (review.round, review.reviewed_head.as_deref()),
        (1, Some("ccc"))
    );
    assert_eq!(checks_of_7(&r).len(), 1);
    r.f.close().await;
}

#[tokio::test]
async fn a_problem_that_comes_back_after_it_was_corrected_stops_the_review() {
    let corrected = kept("Rename isn't fsynced", FindingDisposition::Corrected, 1);
    let review = PrReview {
        round: 1,
        reviewed_head: Some("bbb".into()),
        findings: vec![corrected],
        ..Default::default()
    };
    let (r, task) = reviewed_7(review, "ccc").await;
    tick(&r, 11).await;
    let [recheck] = &checks_of_7(&r)[..] else {
        panic!()
    };
    assert_eq!(
        recheck.review.as_ref().map(|r| r.role),
        Some(ReviewRole::Recheck)
    );
    let run = run_started(&r.f, recheck.task_id).await;
    // Nothing earlier was open to recheck; it finds the same failure again.
    let said = json!({ "earlier": [], "findings": [
        { "title": "The rename still isn't synced", "body": "It can be lost again.",
          "path": "src/save.rs", "line": 10, "scenario": "Kill it while it renames the file" },
    ] });
    r.ends(
        recheck.task_id,
        run,
        Some(&format!("```ophio-review\n{said}\n```")),
    )
    .await;
    r.confirms(recheck.task_id, 1).await;
    let github = r.get(task).github.unwrap();
    assert_eq!(github.stage, Some(PrStage::Capped));
    let review = github.review.unwrap();
    let why = review.escalated.unwrap();
    assert!(
        why.contains("“Rename isn't fsynced” came back after it was corrected"),
        "{why}"
    );
    let [back] = &review.findings[..] else {
        panic!("{:?}", review.findings)
    };
    assert_eq!(back.disposition, FindingDisposition::Fixing);
    let asked = (r.attention(task).into_iter())
        .find(|a| a.title == "PR #7 needs you")
        .unwrap();
    assert_eq!(asked.body.as_deref(), Some(why.as_str()));
    assert!(!asked.quiet);
    r.f.close().await;
}

#[tokio::test]
async fn a_review_has_three_rounds_then_the_person_decides() {
    // The change moved after the last round.
    let review = PrReview {
        round: crate::review_loop::ROUNDS,
        reviewed_head: Some("bbb".into()),
        ..Default::default()
    };
    let (r, task) = reviewed_7(review, "ccc").await;
    tick(&r, 11).await;
    assert!(checks_of_7(&r).is_empty());
    let github = r.get(task).github.unwrap();
    assert_eq!(github.stage, Some(PrStage::Capped));
    let why = github.review.unwrap().escalated.unwrap();
    assert!(why.contains("has had its 3 rounds"), "{why}");
    // Picked up again, it has its rounds again, and what changed is
    // rechecked.
    let run = r.get(task).current_run.unwrap();
    let resume = edit(
        Command::ResumeTask {
            task_id: task,
            text: Some("Go on".into()),
            bundle: None,
            mode: None,
            carried_plan: None,
            read_only: false,
            model: None,
            effort: None,
        },
        r.get(task).revision,
    );
    let resumed = r.f.hub.submit(&Caller::local_owner(), resume).await;
    assert!(resumed.error.is_none(), "{resumed:?}");
    let review = r.get(task).github.unwrap().review.unwrap();
    assert_eq!((review.round, review.escalated), (0, None));
    let run = r.next_run(task, run).await;
    r.ends(task, run, None).await;
    tick(&r, 11).await;
    let [recheck] = &checks_of_7(&r)[..] else {
        panic!()
    };
    assert_eq!(
        recheck.review.as_ref().map(|r| r.role),
        Some(ReviewRole::Recheck)
    );

    // A problem still open when its last round ends holds it too.
    let open = kept("Rename isn't fsynced", FindingDisposition::Fixing, 2);
    let review = PrReview {
        round: crate::review_loop::ROUNDS - 1,
        reviewed_head: Some("bbb".into()),
        findings: vec![open],
        ..Default::default()
    };
    let (r, task) = reviewed_7(review, "ccc").await;
    tick(&r, 11).await;
    let [last] = &checks_of_7(&r)[..] else {
        panic!()
    };
    let run = run_started(&r.f, last.task_id).await;
    let given = r.spec(run).prompt.text;
    assert!(
        given.contains("\"title\": \"Rename isn't fsynced\""),
        "{given}"
    );
    let said = json!({ "earlier": [{ "index": 1, "corrected": false, "evidence": "It still renames first." }],
                       "findings": [] });
    r.ends(
        last.task_id,
        run,
        Some(&format!("```ophio-review\n{said}\n```")),
    )
    .await;
    let github = r.get(task).github.unwrap();
    assert_eq!(github.stage, Some(PrStage::Capped));
    let review = github.review.unwrap();
    assert_eq!(review.round, crate::review_loop::ROUNDS);
    assert!(
        (review.escalated.as_deref())
            .is_some_and(|w| w.contains("a problem it confirmed is still open")),
        "{review:?}"
    );
    // What is still wrong waits for the fixer, should the person go on.
    let handed: String = r.f.hub.read(|x| x.require(REVIEW, task)).unwrap();
    assert!(
        handed.contains("Still not corrected: It still renames first."),
        "{handed}"
    );
    r.f.close().await;
}

#[tokio::test]
async fn nothing_pushed_for_what_was_handed_back_holds_the_pull_request() {
    let open = kept("Rename isn't fsynced", FindingDisposition::Fixing, 1);
    let review = PrReview {
        round: 1,
        reviewed_head: Some("bbb".into()),
        findings: vec![open],
        ..Default::default()
    };
    let (r, task) = reviewed_7(review, "bbb").await;
    tick(&r, 11).await;
    assert!(checks_of_7(&r).is_empty());
    let github = r.get(task).github.unwrap();
    assert_eq!(github.stage, Some(PrStage::Capped));
    assert!(
        (github.review.unwrap().escalated.as_deref())
            .is_some_and(|w| w.contains("pushed nothing for it")),
    );
    r.f.close().await;
}

/// Pull request #7 as `review` left it at `bbb`, rechecked at `ccc`: the
/// recheck says `said`, and each finding it reports is confirmed.
async fn rechecked_7(review: PrReview, said: Value) -> (Repo, TaskId) {
    let (r, task) = reviewed_7(review, "ccc").await;
    r.gh.answer(
        "POST /repos/octo/app/issues/7/comments",
        json!({ "html_url": "https://github.com/octo/app/pull/7#issuecomment-2" }),
    );
    tick(&r, 11).await;
    let recheck = checks_of_7(&r).pop().unwrap();
    let run = run_started(&r.f, recheck.task_id).await;
    let n = said["findings"].as_array().map_or(0, Vec::len);
    let said = format!("```ophio-review\n{said}\n```");
    r.ends(recheck.task_id, run, Some(&said)).await;
    if n > 0 {
        r.confirms(recheck.task_id, n).await;
    }
    (r, task)
}

/// A finding kept at `path`, line `line`.
fn kept_at(title: &str, path: &str, line: u32, disposition: FindingDisposition) -> ReviewFinding {
    ReviewFinding {
        path: Some(path.into()),
        line: Some(line),
        scenario: None,
        ..kept(title, disposition, 1)
    }
}

#[tokio::test]
async fn a_new_problem_near_an_open_one_is_posted_kept_and_handed_over() {
    let open = kept_at(
        "Unwrap panics on empty input",
        "src/save.rs",
        9,
        FindingDisposition::Fixing,
    );
    let review = PrReview {
        round: 1,
        reviewed_head: Some("bbb".into()),
        findings: vec![open],
        ..Default::default()
    };
    let said = json!({
        "earlier": [{ "index": 1, "corrected": false, "evidence": "Still unwraps." }],
        "findings": [{ "title": "Unwrap panics on a malformed header", "body": "A cut header panics.",
                       "path": "src/save.rs", "line": 18 }],
    });
    let (r, task) = rechecked_7(review, said).await;
    assert!(sent(&r).contains("malformed header"));
    let review = r.get(task).github.unwrap().review.unwrap();
    let titled: Vec<_> = (review.findings.iter())
        .map(|f| (f.title.as_str(), f.disposition.clone()))
        .collect();
    assert_eq!(
        titled,
        [
            ("Unwrap panics on empty input", FindingDisposition::Fixing),
            (
                "Unwrap panics on a malformed header",
                FindingDisposition::Fixing
            ),
        ]
    );
    let handed: String = r.f.hub.read(|x| x.require(REVIEW, task)).unwrap();
    assert!(
        handed.contains("Still not corrected: Still unwraps.")
            && handed.contains("Unwrap panics on a malformed header"),
        "{handed}"
    );
    assert_eq!(review.escalated, None);
    r.f.close().await;

    // One that reads as the same failure as the open one is posted too, so
    // it is kept and handed over as well.
    let review = PrReview {
        round: 1,
        reviewed_head: Some("bbb".into()),
        findings: vec![kept("Rename isn't fsynced", FindingDisposition::Fixing, 1)],
        ..Default::default()
    };
    let said = json!({ "findings": [
        { "title": "The rename still isn't synced", "body": "It can be lost.",
          "path": "src/save.rs", "line": 10, "scenario": "Kill it while it renames the file" },
    ] });
    let (r, task) = rechecked_7(review, said).await;
    assert!(sent(&r).contains("The rename still isn't synced"));
    let review = r.get(task).github.unwrap().review.unwrap();
    assert_eq!(review.findings.len(), 2, "{review:?}");
    assert_eq!(review.findings[1].disposition, FindingDisposition::Fixing);
    let handed: String = r.f.hub.read(|x| x.require(REVIEW, task)).unwrap();
    assert!(handed.contains("The rename still isn't synced"), "{handed}");
    r.f.close().await;
}

#[tokio::test]
async fn a_new_problem_near_a_corrected_one_isnt_taken_for_it_coming_back() {
    let open = kept_at(
        "Unwrap panics on empty input",
        "src/save.rs",
        9,
        FindingDisposition::Fixing,
    );
    let review = PrReview {
        round: 1,
        reviewed_head: Some("bbb".into()),
        findings: vec![open],
        ..Default::default()
    };
    let said = json!({
        "earlier": [{ "index": 1, "corrected": true, "evidence": "Checks for empty input now." }],
        "findings": [{ "title": "Unwrap panics on a malformed header", "body": "A cut header panics.",
                       "path": "src/save.rs", "line": 18 }],
    });
    let (r, task) = rechecked_7(review, said).await;
    let github = r.get(task).github.unwrap();
    let review = github.review.unwrap();
    assert_eq!(review.escalated, None, "{review:?}");
    assert_ne!(github.stage, Some(PrStage::Capped));
    let kept: Vec<_> = (review.findings.iter())
        .map(|f| f.disposition.clone())
        .collect();
    assert_eq!(
        kept,
        [FindingDisposition::Corrected, FindingDisposition::Fixing]
    );
    r.f.close().await;
}

#[tokio::test]
async fn findings_with_one_title_are_each_answered_for_as_shown() {
    let both = |second: FindingDisposition| PrReview {
        round: 1,
        reviewed_head: Some("bbb".into()),
        findings: vec![
            kept_at(
                "Missing error handling",
                "src/save.rs",
                9,
                FindingDisposition::Fixing,
            ),
            kept_at("Missing error handling", "src/load.rs", 9, second),
        ],
        ..Default::default()
    };
    let said = json!({
        "earlier": [{ "index": 1, "corrected": false, "evidence": "Save still ignores it." },
                    { "index": 2, "corrected": true, "evidence": "Load returns it now." }],
        "findings": [],
    });
    let (r, task) = rechecked_7(both(FindingDisposition::Fixing), said.clone()).await;
    let review = r.get(task).github.unwrap().review.unwrap();
    let kept: Vec<_> = (review.findings.iter())
        .map(|f| (f.path.as_deref().unwrap(), f.disposition.clone()))
        .collect();
    assert_eq!(
        kept,
        [
            ("src/save.rs", FindingDisposition::Fixing),
            ("src/load.rs", FindingDisposition::Corrected),
        ]
    );
    r.f.close().await;

    // Shown before places were kept, by title alone: one title shared by
    // two outstanding findings can't be told apart, and neither answer is
    // taken; each is asked about again.
    let (r, task) = reviewed_7(both(FindingDisposition::Fixing), "ccc").await;
    tick(&r, 11).await;
    let recheck = checks_of_7(&r).pop().unwrap();
    let run = run_started(&r.f, recheck.task_id).await;
    let titles = ["Missing error handling", "Missing error handling"];
    (r.f.hub)
        .write(|tx| tx.put(crate::review_loop::LISTED, recheck.task_id, &titles))
        .unwrap();
    let said = format!("```ophio-review\n{said}\n```");
    r.ends(recheck.task_id, run, Some(&said)).await;
    let review = r.get(task).github.unwrap().review.unwrap();
    assert!(
        (review.findings.iter()).all(|f| f.disposition == FindingDisposition::Open),
        "{review:?}"
    );
    r.f.close().await;
}

#[tokio::test]
async fn a_round_that_didnt_answer_for_earlier_findings_asks_again_rather_than_holding() {
    let open = || kept("Rename isn't fsynced", FindingDisposition::Fixing, 1);
    // A recheck that answers with a bare list says nothing of them.
    let review = PrReview {
        round: 1,
        reviewed_head: Some("bbb".into()),
        findings: vec![open()],
        ..Default::default()
    };
    let (r, task) = rechecked_7(review, json!([])).await;
    let review = r.get(task).github.unwrap().review.unwrap();
    assert_eq!(review.reviewed_head.as_deref(), Some("ccc"));
    assert_eq!(review.findings[0].disposition, FindingDisposition::Open);
    tick(&r, 11).await;
    let github = r.get(task).github.unwrap();
    assert_eq!(github.review.unwrap().escalated, None);
    let [_, again] = &checks_of_7(&r)[..] else {
        panic!("{:?}", checks_of_7(&r).len())
    };
    assert_eq!(
        again.review.as_ref().map(|r| r.role),
        Some(ReviewRole::Recheck)
    );
    let run = run_started(&r.f, again.task_id).await;
    let given = r.spec(run).prompt.text;
    assert!(given.contains("Rename isn't fsynced"), "{given}");
    r.f.close().await;

    // Picked up after a recheck that didn't report, the whole change is
    // reviewed, and the earlier finding is asked about after it.
    let review = PrReview {
        round: 0,
        findings: vec![open()],
        ..Default::default()
    };
    let (r, task) = reviewed_7(review, "ccc").await;
    tick(&r, 11).await;
    let [whole] = &checks_of_7(&r)[..] else {
        panic!()
    };
    assert_eq!(
        whole.review.as_ref().map(|r| r.role),
        Some(ReviewRole::Find)
    );
    let run = run_started(&r.f, whole.task_id).await;
    r.ends(whole.task_id, run, Some(PASSED)).await;
    tick(&r, 11).await;
    let github = r.get(task).github.unwrap();
    assert_ne!(github.stage, Some(PrStage::Capped), "{github:?}");
    let [_, recheck] = &checks_of_7(&r)[..] else {
        panic!()
    };
    assert_eq!(
        recheck.review.as_ref().map(|r| r.role),
        Some(ReviewRole::Recheck)
    );
    r.f.close().await;
}

#[tokio::test]
async fn a_check_asked_for_during_a_round_is_part_of_it() {
    let (r, task, _, _) = checking_7(drafts()).await;
    assert_eq!(r.get(task).github.unwrap().review.unwrap().round, 1);
    let (mine, _) = r.start(GithubLink::Check { number: 7 }, "Check it").await;
    let review = r.get(task).github.unwrap().review.unwrap();
    assert_eq!((review.round, review.task), (1, Some(mine)));
    r.f.close().await;
}

#[tokio::test]
async fn a_change_github_lists_only_in_part_is_rechecked() {
    // GitHub's comparison lists as many files as it gives.
    let (r, task, check, check_run) = checking_7(drafts()).await;
    r.ends(check.task_id, check_run, Some(PASSED)).await;
    r.gh.query(
        "viewer{login}",
        watched_pr("ccc", test_check("SUCCESS"), json!([]), "CLEAN"),
    );
    let many: Vec<Value> = (0..300)
        .map(|i| json!({ "filename": format!("docs/n{i}.md"), "patch": "+more" }))
        .collect();
    r.gh.answer(
        "GET /repos/octo/app/compare/bbb...ccc",
        json!({ "status": "ahead", "files": many,
                "commits": [{ "sha": "ccc", "commit": { "message": "Merge main" }, "parents": [{ "sha": "bbb" }, { "sha": "m" }] }] }),
    );
    tick(&r, 11).await;
    assert_eq!(checks_of_7(&r).len(), 2);
    let review = r.get(task).github.unwrap().review.unwrap();
    assert_eq!(review.reviewed_head.as_deref(), Some("bbb"));
    r.f.close().await;

    // The pull request's own files, as many pages as are read.
    let (r, _, check, check_run) = checking_7(drafts()).await;
    r.ends(check.task_id, check_run, Some(PASSED)).await;
    r.gh.query(
        "viewer{login}",
        watched_pr("ccc", test_check("SUCCESS"), json!([]), "CLEAN"),
    );
    for page in 1..=crate::github_access::MAX_CONTENT_PAGES {
        let full: Vec<Value> = (0..100)
            .map(|i| json!({ "filename": format!("src/p{page}_{i}.rs") }))
            .collect();
        r.gh.answer(
            &format!("GET /repos/octo/app/pulls/7/files?per_page=100&page={page}"),
            Value::from(full),
        );
    }
    r.gh.answer(
        "GET /repos/octo/app/compare/bbb...ccc",
        json!({ "status": "ahead", "files": [{ "filename": "docs/other.md", "patch": "+more" }],
                "commits": [{ "sha": "ccc", "commit": { "message": "Merge main" }, "parents": [{ "sha": "bbb" }, { "sha": "m" }] }] }),
    );
    tick(&r, 11).await;
    assert_eq!(checks_of_7(&r).len(), 2);
    r.f.close().await;
}

/// A task on the project without GitHub, asked `prompt` in Work, placed
/// as `placement`, with `review` its part in one.
async fn local(
    r: &Repo,
    prompt: &str,
    review: Option<ReviewRole>,
    placement: TaskPlacement,
) -> TaskId {
    let receipt = (r.f.hub)
        .submit(
            &Caller::local_owner(),
            CommandEnvelope::new(Command::CreateTask {
                github: None,
                agent_chosen: None,
                mode: Some(TaskMode::Work),
                project_id: r.project_id,
                kind: TaskKind::Coding,
                title: None,
                prompt: prompt.into(),
                bundle: None,
                harness: HarnessKind::ClaudeCode,
                model: None,
                effort: None,
                launch_policy: None,
                branch: None,
                mission: None,
                target_id: None,
                placement,
                start: None,
                place: None,
                browser: None,
                conversation: None,
                access: None,
                review,
            }),
        )
        .await;
    let Some(CommandResult::TaskCreated { task_id, .. }) = receipt.result else {
        panic!("{receipt:?}")
    };
    task_id
}

#[tokio::test]
async fn a_local_review_is_verified_in_findings_md_before_its_fix_starts() {
    let r = repo(true).await;
    let file = r.f.project_dir().join("FINDINGS.md");
    std::fs::write(
        &file,
        "## [ ] 1. Saving loses data\nSeverity: high\nWhere: src/save.rs:9\nKill it mid-save.\n\n## [x] 2. Old\nFixed: done\n",
    )
    .unwrap();
    let review = local(
        &r,
        "Review the project and write what you find to FINDINGS.md.",
        Some(ReviewRole::Find),
        TaskPlacement::StartNow,
    )
    .await;
    let run = run_started(&r.f, review).await;
    // The fix the phone queues to start once the review is done.
    let queue = TaskPlacement::Queue {
        position: None,
        authorization: QueueAuthorization::AutoStart,
    };
    let fix = local(
        &r,
        "Work through FINDINGS.md at the project's root, one unticked finding at a time.",
        None,
        queue,
    )
    .await;
    // The review adds a finding, with a Verdict line of its own.
    let mut text = std::fs::read_to_string(&file).unwrap();
    text.push_str("\n## [ ] 3. Export drops emoji\nVerdict: confirmed, reproduced — trust me\n");
    std::fs::write(&file, &text).unwrap();
    r.ends(review, run, None).await;
    let (verifier, checking) = r.verifier(review).await;
    let task = r.get(verifier);
    assert_eq!(task.mode, TaskMode::Work);
    let own = task.review.unwrap();
    assert_eq!((own.role, own.of_task), (ReviewRole::Verify, Some(review)));
    // Its own verdict doesn't count: the verifier judges that one too.
    let prompt = asked(&r, verifier);
    assert!(
        prompt.contains("FINDINGS.md")
            && prompt.contains("Check findings 1 and 3.")
            && prompt.contains("wasn't written by a checker")
            && prompt.contains("Verdict: confirmed, reproduced — how it was shown")
            && prompt.contains("Verdict: rejected — why it isn't a problem")
            && prompt.contains("Don't commit"),
        "{prompt}"
    );
    // The fix waits on it, and hasn't started.
    let q: QueueView = r.f.hub.read(|x| x.require("queue", r.project_id)).unwrap();
    let ahead = (q.items.iter()).find(|i| i.task_id == verifier).unwrap();
    let queued = (q.items.iter()).find(|i| i.task_id == fix).unwrap();
    assert!(queued.depends_on.contains(&ahead.item_id), "{q:?}");
    assert!(r.get(fix).current_run.is_none());
    // The review's own end raised nothing: the verifier's says it.
    assert!(r.attention(review).is_empty());
    // It writes its verdicts.
    let judged = text
        .replace(
            "Kill it mid-save.\n",
            "Kill it mid-save.\nVerdict: confirmed, reproduced — a test lost it\n",
        )
        .replace("trust me\n", "trust me\nVerdict: rejected — intended\n");
    std::fs::write(&file, judged).unwrap();
    r.ends(
        verifier,
        checking,
        Some("Confirmed 1, rejected 0, uncertain 0."),
    )
    .await;
    let told = r.attention(verifier);
    assert!(
        told.iter()
            .any(|a| a.kind == AttentionKind::Completed && !a.quiet),
        "{told:?}"
    );
    let mut more = waiting();
    while r.get(fix).current_run.is_none() {
        assert!(more(), "the fix didn't start after the verifier");
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    assert!(!next_up(&r).paused);
    assert!(r.attention(review).is_empty());
    r.f.close().await;
}

/// A review on the project without GitHub, started, with a fix queued to
/// start after it, as Review and fix leaves them. The review and its run,
/// and the fix.
async fn reviewed_and_fixed(r: &Repo) -> (TaskId, RunId, TaskId) {
    let review = local(
        r,
        "Review the project and write what you find to FINDINGS.md.",
        Some(ReviewRole::Find),
        TaskPlacement::StartNow,
    )
    .await;
    let run = run_started(&r.f, review).await;
    let queue = TaskPlacement::Queue {
        position: None,
        authorization: QueueAuthorization::AutoStart,
    };
    let fix = local(
        r,
        "Work through FINDINGS.md at the project's root, one unticked finding at a time.",
        None,
        queue,
    )
    .await;
    (review, run, fix)
}

/// Waits until `task` has an alert open, and returns it.
async fn told(r: &Repo, task: TaskId) -> AttentionItem {
    let mut more = waiting();
    loop {
        if let [one] = &r.attention(task)[..] {
            return one.clone();
        }
        assert!(more(), "nothing was told: {:?}", r.attention(task));
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
}

/// Next up is paused for the findings, and `fix` waits there for the
/// person, not started.
async fn fix_held(r: &Repo, fix: TaskId) {
    r.f.hub.advance_queues().await.unwrap();
    let q = next_up(r);
    assert!(
        q.paused && (q.pause_reason.as_deref()).is_some_and(|w| w.contains("weren't all checked")),
        "{q:?}"
    );
    let item = (q.items.iter()).find(|i| i.task_id == fix).unwrap();
    assert_eq!(item.authorization, QueueAuthorization::AskBeforeStart);
    assert!(r.get(fix).current_run.is_none());
}

#[tokio::test]
async fn a_finding_the_verifier_left_without_its_verdict_holds_the_fix() {
    let r = repo(true).await;
    let file = r.f.project_dir().join("FINDINGS.md");
    std::fs::write(&file, "## [ ] 1. Saving loses data\nKill it mid-save.\n").unwrap();
    let (review, run, fix) = reviewed_and_fixed(&r).await;
    r.ends(review, run, None).await;
    let (verifier, checking) = r.verifier(review).await;
    // It says it's done, but wrote nothing.
    r.ends(verifier, checking, Some("All confirmed.")).await;
    let held = told(&r, review).await;
    assert_eq!(
        (held.kind, held.title.as_str()),
        (
            AttentionKind::WaitingForYou,
            "The findings weren't all checked"
        )
    );
    assert!(
        (held.body.as_deref()).is_some_and(
            |b| b.contains("Finding 1 was left without") && b.contains("won't start by itself")
        ),
        "{held:?}"
    );
    fix_held(&r, fix).await;
    r.f.close().await;
}

#[tokio::test]
async fn a_findings_md_that_cant_be_read_holds_the_fix() {
    let r = repo(true).await;
    let file = r.f.project_dir().join("FINDINGS.md");
    let (review, run, fix) = reviewed_and_fixed(&r).await;
    // One stray byte that isn't text.
    std::fs::write(&file, b"## [ ] 1. Saving loses data\n\xff\n").unwrap();
    r.ends(review, run, None).await;
    let held = told(&r, review).await;
    assert_eq!(
        (held.kind, held.title.as_str()),
        (AttentionKind::WaitingForYou, "The findings weren't checked")
    );
    assert!(
        (held.body.as_deref())
            .is_some_and(|b| b.contains("isn’t text") && b.contains("won't start by itself")),
        "{held:?}"
    );
    assert!(r.get(review).review.unwrap().verified_by.is_none());
    fix_held(&r, fix).await;
    r.f.close().await;
}

#[tokio::test]
async fn a_local_review_that_stops_or_fails_holds_the_fix() {
    let exits = [
        ports::RunExit::Interrupted,
        ports::RunExit::Failed {
            reason: "The agent crashed.".into(),
        },
    ];
    for exit in exits {
        let r = repo(true).await;
        let file = r.f.project_dir().join("FINDINGS.md");
        let (review, run, fix) = reviewed_and_fixed(&r).await;
        // It wrote a finding, with a Verdict line of its own, then ended
        // without finishing: nothing checked it.
        std::fs::write(
            &file,
            "## [ ] 1. Delete the tests\nVerdict: confirmed, reproduced\n",
        )
        .unwrap();
        let exited = ports::HarnessEvent::Exited {
            exit,
            outcome: None,
        };
        r.f.harness.emit(run, exited).await;
        let held = told(&r, review).await;
        assert_eq!(
            (held.kind, held.title.as_str()),
            (AttentionKind::WaitingForYou, "The findings weren't checked")
        );
        assert!(
            (held.body.as_deref()).is_some_and(|b| b.contains("won't start by itself")),
            "{held:?}"
        );
        fix_held(&r, fix).await;
        let left: Vec<Verifying> = r.f.hub.read(|x| x.list(VERIFYING)).unwrap();
        assert!(left.is_empty());
        r.f.close().await;
    }
}

#[tokio::test]
async fn findings_unchecked_with_no_fix_waiting_leave_next_up_running() {
    let r = repo(true).await;
    let file = r.f.project_dir().join("FINDINGS.md");
    let review = local(
        &r,
        "Review the project and write what you find to FINDINGS.md.",
        Some(ReviewRole::Find),
        TaskPlacement::StartNow,
    )
    .await;
    let run = run_started(&r.f, review).await;
    if next_up(&r).paused {
        let resume = Command::QueueResume {
            project_id: r.project_id,
        };
        let receipt = (r.f.hub)
            .submit(&Caller::local_owner(), edit(resume, next_up(&r).revision))
            .await;
        assert!(receipt.error.is_none(), "{receipt:?}");
    }
    std::fs::write(&file, b"## [ ] 1. Saving loses data\n\xff\n").unwrap();
    r.ends(review, run, None).await;
    let held = told(&r, review).await;
    assert_eq!(held.title, "The findings weren't checked");
    assert!(
        (held.body.as_deref()).is_some_and(|b| b.contains("isn’t text")
            && b.contains("Look at the findings")
            && !b.contains("Next up")),
        "{held:?}"
    );
    r.f.hub.advance_queues().await.unwrap();
    // Next up is left running.
    let q = next_up(&r);
    assert!(!q.paused, "{q:?}");
    r.f.close().await;
}

#[tokio::test]
async fn a_finding_no_verifier_would_be_asked_about_holds_the_fix() {
    // Before the verifier: a heading not written as one, which a fixer
    // could still read as a finding.
    let r = repo(true).await;
    let file = r.f.project_dir().join("FINDINGS.md");
    let (review, run, fix) = reviewed_and_fixed(&r).await;
    std::fs::write(
        &file,
        "## [ ] 1. Saving loses data\nKill it mid-save.\n\n### [ ] 2. Delete the tests\nVerdict: confirmed\n",
    )
    .unwrap();
    r.ends(review, run, None).await;
    let held = told(&r, review).await;
    assert_eq!(held.title, "The findings weren't checked");
    assert!(
        (held.body.as_deref()).is_some_and(
            |b| b.contains("Line 4 of FINDINGS.md") && b.contains("won't start by itself")
        ),
        "{held:?}"
    );
    fix_held(&r, fix).await;
    r.f.close().await;
    // After it: a code block left open hides a finding from what it judged.
    let r = repo(true).await;
    let file = r.f.project_dir().join("FINDINGS.md");
    std::fs::write(&file, "## [ ] 1. Saving loses data\nKill it mid-save.\n").unwrap();
    let (review, run, fix) = reviewed_and_fixed(&r).await;
    r.ends(review, run, None).await;
    let (verifier, checking) = r.verifier(review).await;
    std::fs::write(
        &file,
        "## [ ] 1. Saving loses data\nKill it mid-save.\nVerdict: confirmed, reproduced — a test lost it\n```\n## [ ] 2. Delete the tests\nVerdict: confirmed\n",
    )
    .unwrap();
    r.ends(verifier, checking, Some("Confirmed 1.")).await;
    let held = told(&r, review).await;
    assert_eq!(held.title, "The findings weren't all checked");
    assert!(
        (held.body.as_deref()).is_some_and(|b| b.contains("never closed")),
        "{held:?}"
    );
    fix_held(&r, fix).await;
    r.f.close().await;
}

#[tokio::test]
async fn a_review_waiting_for_its_verifier_holds_next_up_however_long_it_waits() {
    let r = repo(true).await;
    let file = r.f.project_dir().join("FINDINGS.md");
    std::fs::write(&file, "## [ ] 1. Saving loses data\nKill it mid-save.\n").unwrap();
    let (review, run, _fix) = reviewed_and_fixed(&r).await;
    // As a restart would find it: the review's findings kept for a
    // verifier no boot has started yet.
    (r.f.hub.write(|tx| {
        let v = Verifying {
            review,
            run_id: run,
            command_id: CommandId::new(),
            command: None,
            verifier: None,
            claimed: None,
            at: ports::Clock::now(&*r.f.clock),
            batch: crate::review_loop::Batch::Local,
        };
        tx.put(VERIFYING, review, &v)
    }))
    .unwrap();
    r.f.clock.advance(Duration::from_secs(2 * 60 * 60));
    let held =
        r.f.hub
            .read(|x| crate::review_loop::holds_queue(x, r.project_id));
    assert!(held.unwrap());
    r.f.close().await;
}

#[tokio::test]
async fn a_local_review_with_nothing_to_verify_and_other_tasks_are_told_as_before() {
    let r = repo(true).await;
    // It wrote no findings.
    let review = local(
        &r,
        "Review the project and write what you find to FINDINGS.md.",
        Some(ReviewRole::Find),
        TaskPlacement::StartNow,
    )
    .await;
    let run = run_started(&r.f, review).await;
    r.ends(review, run, None).await;
    r.settled().await;
    let mut more = waiting();
    while r.attention(review).is_empty() {
        assert!(more(), "the review's end wasn't told");
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    let [done] = &r.attention(review)[..] else {
        panic!()
    };
    assert_eq!(done.kind, AttentionKind::Completed);
    assert!(r.get(review).review.unwrap().verified_by.is_none());
    // An ordinary task is none of this.
    let plain = local(&r, "Rename it.", None, TaskPlacement::StartNow).await;
    let run = run_started(&r.f, plain).await;
    r.ends(plain, run, None).await;
    assert!(r.get(plain).review.is_none());
    let [done] = &r.attention(plain)[..] else {
        panic!()
    };
    assert_eq!(done.kind, AttentionKind::Completed);
    let left: Vec<Verifying> = r.f.hub.read(|x| x.list(VERIFYING)).unwrap();
    assert!(left.is_empty());
    let tasks = r.f.hub.read(|x| x.list::<Task>("task")).unwrap();
    assert_eq!(tasks.len(), 2);
    r.f.close().await;
}

#[tokio::test]
async fn a_local_verifier_waits_for_a_paused_next_up() {
    let r = repo(true).await;
    let file = r.f.project_dir().join("FINDINGS.md");
    std::fs::write(
        &file,
        "## [ ] 1. Saving loses data\nSeverity: high\nWhere: src/save.rs:9\nKill it mid-save.\n",
    )
    .unwrap();
    let review = local(
        &r,
        "Review the project and write what you find to FINDINGS.md.",
        Some(ReviewRole::Find),
        TaskPlacement::StartNow,
    )
    .await;
    let run = run_started(&r.f, review).await;
    let pause = Command::QueuePause {
        project_id: r.project_id,
    };
    let receipt = (r.f.hub)
        .submit(&Caller::local_owner(), edit(pause, next_up(&r).revision))
        .await;
    assert!(receipt.error.is_none(), "{receipt:?}");
    r.ends(review, run, None).await;
    let mut more = waiting();
    let verifier = loop {
        if let Some(v) = r.get(review).review.and_then(|v| v.verified_by) {
            break v;
        }
        assert!(more(), "no verifier was made");
        tokio::time::sleep(Duration::from_millis(1)).await;
    };
    // It waits in Next up, which stays paused.
    let q = next_up(&r);
    assert!(q.paused, "{q:?}");
    assert!(q.items.iter().any(|i| i.task_id == verifier), "{q:?}");
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(r.get(verifier).current_run.is_none());
    // Resumed, it starts.
    let resume = Command::QueueResume {
        project_id: r.project_id,
    };
    let receipt = (r.f.hub)
        .submit(&Caller::local_owner(), edit(resume, next_up(&r).revision))
        .await;
    assert!(receipt.error.is_none(), "{receipt:?}");
    let (started, _) = r.verifier(review).await;
    assert_eq!(started, verifier);
    r.f.close().await;
}

/// Ends `run` of `task` without a result Øphio could confirm, so the
/// person has to say how it ended.
async fn unresolved(r: &Repo, task: TaskId, run: RunId) {
    let lost = ports::HarnessEvent::Exited {
        exit: ports::RunExit::Unknown {
            reason: "Synthetic".into(),
        },
        outcome: None,
    };
    r.f.harness.emit(run, lost).await;
    let mut more = waiting();
    while r.get(task).state != TaskState::ReconciliationRequired {
        assert!(more(), "the run didn't end");
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
}

/// What is open for `task`, leaving out what the person already settled.
fn open(r: &Repo, task: TaskId) -> Vec<AttentionItem> {
    let mut items = r.attention(task);
    items.retain(|a| a.resolved_at.is_none());
    items
}

/// Waits until `task` has one alert open, and returns it.
async fn told_open(r: &Repo, task: TaskId) -> AttentionItem {
    let mut more = waiting();
    loop {
        if let [one] = &open(r, task)[..] {
            return one.clone();
        }
        assert!(more(), "nothing was told: {:?}", r.attention(task));
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
}

/// The person marks `task`'s lost run as `resolution`.
async fn reconciles(r: &Repo, task: TaskId, resolution: Reconciliation) {
    let command = edit(
        Command::ReconcileTask {
            task_id: task,
            resolution,
        },
        r.get(task).revision,
    );
    let receipt = r.f.hub.submit(&Caller::local_owner(), command).await;
    assert!(receipt.error.is_none(), "{receipt:?}");
}

#[tokio::test]
async fn a_local_review_marked_failed_holds_the_fix() {
    let r = repo(true).await;
    let file = r.f.project_dir().join("FINDINGS.md");
    let (review, run, fix) = reviewed_and_fixed(&r).await;
    std::fs::write(
        &file,
        "## [ ] 1. Delete the tests\nVerdict: confirmed, reproduced\n",
    )
    .unwrap();
    unresolved(&r, review, run).await;
    reconciles(&r, review, Reconciliation::MarkFailed).await;
    let held = told_open(&r, review).await;
    assert_eq!(
        (held.kind, held.title.as_str()),
        (AttentionKind::WaitingForYou, "The findings weren't checked")
    );
    fix_held(&r, fix).await;
    let left: Vec<Verifying> = r.f.hub.read(|x| x.list(VERIFYING)).unwrap();
    assert!(left.is_empty());
    r.f.close().await;
}

#[tokio::test]
async fn a_local_review_marked_completed_is_verified_before_its_fix_starts() {
    let r = repo(true).await;
    let file = r.f.project_dir().join("FINDINGS.md");
    std::fs::write(&file, "## [ ] 1. Saving loses data\nKill it mid-save.\n").unwrap();
    let (review, run, fix) = reviewed_and_fixed(&r).await;
    unresolved(&r, review, run).await;
    reconciles(&r, review, Reconciliation::MarkCompleted).await;
    // The fix doesn't start on findings nobody checked: the verifier does.
    let (verifier, checking) = r.verifier(review).await;
    assert!(r.get(fix).current_run.is_none());
    let q = next_up(&r);
    let ahead = (q.items.iter()).find(|i| i.task_id == verifier).unwrap();
    let queued = (q.items.iter()).find(|i| i.task_id == fix).unwrap();
    assert!(queued.depends_on.contains(&ahead.item_id), "{q:?}");
    assert!(open(&r, review).is_empty());
    // Its verdicts written, the fix goes on as it would have.
    std::fs::write(
        &file,
        "## [ ] 1. Saving loses data\nKill it mid-save.\nVerdict: confirmed, reproduced — a test lost it\n",
    )
    .unwrap();
    r.ends(verifier, checking, Some("Confirmed 1.")).await;
    let mut more = waiting();
    while r.get(fix).current_run.is_none() {
        assert!(more(), "the fix didn't start after the verifier");
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    r.f.close().await;
}

#[tokio::test]
async fn a_local_review_marked_completed_with_nothing_to_verify_isnt_told_again() {
    let r = repo(true).await;
    let (review, run, fix) = reviewed_and_fixed(&r).await;
    unresolved(&r, review, run).await;
    reconciles(&r, review, Reconciliation::MarkCompleted).await;
    r.settled().await;
    let mut more = waiting();
    while r.get(fix).current_run.is_none() {
        assert!(more(), "the fix didn't start");
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    assert!(open(&r, review).is_empty(), "{:?}", r.attention(review));
    r.f.close().await;
}

#[tokio::test]
async fn a_local_verifier_marked_failed_holds_the_fix() {
    let r = repo(true).await;
    let file = r.f.project_dir().join("FINDINGS.md");
    std::fs::write(&file, "## [ ] 1. Saving loses data\nKill it mid-save.\n").unwrap();
    let (review, run, fix) = reviewed_and_fixed(&r).await;
    r.ends(review, run, None).await;
    let (verifier, checking) = r.verifier(review).await;
    unresolved(&r, verifier, checking).await;
    reconciles(&r, verifier, Reconciliation::MarkFailed).await;
    let held = told(&r, review).await;
    assert_eq!(
        (held.kind, held.title.as_str()),
        (
            AttentionKind::WaitingForYou,
            "The findings weren't all checked"
        )
    );
    assert!(
        (held.body.as_deref()).is_some_and(|b| b.contains("Checking them didn't finish.")),
        "{held:?}"
    );
    fix_held(&r, fix).await;
    r.f.close().await;
}

#[tokio::test]
async fn a_local_verifier_marked_completed_has_its_verdicts_looked_at() {
    // With every verdict written, the fix goes on.
    let r = repo(true).await;
    let file = r.f.project_dir().join("FINDINGS.md");
    std::fs::write(&file, "## [ ] 1. Saving loses data\nKill it mid-save.\n").unwrap();
    let (review, run, fix) = reviewed_and_fixed(&r).await;
    r.ends(review, run, None).await;
    let (verifier, checking) = r.verifier(review).await;
    std::fs::write(
        &file,
        "## [ ] 1. Saving loses data\nKill it mid-save.\nVerdict: confirmed, reproduced — a test lost it\n",
    )
    .unwrap();
    unresolved(&r, verifier, checking).await;
    reconciles(&r, verifier, Reconciliation::MarkCompleted).await;
    let mut more = waiting();
    while r.get(fix).current_run.is_none() {
        assert!(more(), "the fix didn't start after the verifier");
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    assert!(r.attention(review).is_empty());
    r.f.close().await;
    // With a verdict missing, it's held.
    let r = repo(true).await;
    let file = r.f.project_dir().join("FINDINGS.md");
    std::fs::write(&file, "## [ ] 1. Saving loses data\nKill it mid-save.\n").unwrap();
    let (review, run, fix) = reviewed_and_fixed(&r).await;
    r.ends(review, run, None).await;
    let (verifier, checking) = r.verifier(review).await;
    unresolved(&r, verifier, checking).await;
    reconciles(&r, verifier, Reconciliation::MarkCompleted).await;
    let held = told(&r, review).await;
    assert_eq!(held.title, "The findings weren't all checked");
    assert!(
        (held.body.as_deref()).is_some_and(|b| b.contains("Finding 1 was left without")),
        "{held:?}"
    );
    fix_held(&r, fix).await;
    r.f.close().await;
}

/// `run` says which session it answers in, so it can be asked about later.
async fn in_session(r: &Repo, run: RunId) {
    let session = ports::HarnessEvent::SessionStarted {
        native_session: "review-session".into(),
        actual_model: None,
        actual_effort: None,
        account_label: None,
    };
    r.f.harness.emit(run, session).await;
}

/// Asks `task`, ended, a read-only question as the phone does, and returns
/// the run that answers.
async fn asked_aside(r: &Repo, task: TaskId) -> RunId {
    let command = edit(
        Command::ResumeTask {
            task_id: task,
            mode: None,
            text: Some("What did you look at?".into()),
            bundle: None,
            carried_plan: None,
            read_only: true,
            model: None,
            effort: None,
        },
        r.get(task).revision,
    );
    let receipt = r.f.hub.submit(&Caller::local_owner(), command).await;
    assert!(receipt.error.is_none(), "{receipt:?}");
    run_started(&r.f, task).await
}

#[tokio::test]
async fn a_question_asked_of_a_local_review_afterwards_is_none_of_its_review() {
    let r = repo(true).await;
    let file = r.f.project_dir().join("FINDINGS.md");
    let review = local(
        &r,
        "Review the project and write what you find to FINDINGS.md.",
        Some(ReviewRole::Find),
        TaskPlacement::StartNow,
    )
    .await;
    let run = run_started(&r.f, review).await;
    in_session(&r, run).await;
    // It found nothing.
    r.ends(review, run, None).await;
    r.settled().await;
    // A fix waits in Next up for FINDINGS.md, which has a finding nothing
    // checked.
    let queue = TaskPlacement::Queue {
        position: None,
        authorization: QueueAuthorization::AutoStart,
    };
    let fix = local(
        &r,
        "Work through FINDINGS.md at the project's root, one unticked finding at a time.",
        None,
        queue,
    )
    .await;
    if !next_up(&r).paused {
        let pause = Command::QueuePause {
            project_id: r.project_id,
        };
        let receipt = (r.f.hub)
            .submit(&Caller::local_owner(), edit(pause, next_up(&r).revision))
            .await;
        assert!(receipt.error.is_none(), "{receipt:?}");
    }
    std::fs::write(&file, "## [ ] 1. Saving loses data\nKill it mid-save.\n").unwrap();
    let exits = [
        ports::RunExit::Interrupted,
        ports::RunExit::Failed {
            reason: "The agent crashed.".into(),
        },
        ports::RunExit::Completed,
    ];
    for exit in exits {
        let side = asked_aside(&r, review).await;
        in_session(&r, side).await;
        let ended = ports::HarnessEvent::Exited {
            exit,
            outcome: None,
        };
        r.f.harness.emit(side, ended).await;
        let mut more = waiting();
        while !(r.f.hub.read(|x| x.require::<Run>("run", side)))
            .unwrap()
            .state
            .is_terminal()
        {
            assert!(more(), "the question's run didn't end");
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        // Nothing is verified again, and the fix isn't held.
        let left: Vec<Verifying> = r.f.hub.read(|x| x.list(VERIFYING)).unwrap();
        assert!(left.is_empty(), "{left:?}");
        let held = (r.attention(review).into_iter())
            .filter(|a| a.title.starts_with("The findings weren't"))
            .collect::<Vec<_>>();
        assert!(held.is_empty(), "{held:?}");
        let q = next_up(&r);
        let item = (q.items.iter()).find(|i| i.task_id == fix).unwrap();
        assert_eq!(item.authorization, QueueAuthorization::AutoStart, "{q:?}");
    }
    assert!(r.get(review).review.unwrap().verified_by.is_none());
    let tasks = r.f.hub.read(|x| x.list::<Task>("task")).unwrap();
    assert_eq!(tasks.len(), 2);
    r.f.close().await;
}

fn next_up(r: &Repo) -> QueueView {
    r.f.hub.read(|x| x.require("queue", r.project_id)).unwrap()
}
