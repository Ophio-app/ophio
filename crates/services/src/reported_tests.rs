// SPDX-License-Identifier: Apache-2.0
//! An agent ends its turn with what it came to, and Øphio keeps that on the
//! run, out of the summary the person reads, and in the notification.
use super::*;
use crate::reported::{ended, headline, read, take};

fn run(f: &Fixture, id: RunId) -> Run {
    f.hub.read(|r| r.require("run", id)).unwrap()
}

async fn until(what: &str, mut done: impl FnMut() -> bool) {
    let mut more = waiting();
    while !done() {
        assert!(more(), "{what}");
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

fn outcome(summary: &str) -> RunOutcome {
    RunOutcome {
        summary: Some(summary.into()),
        changed_files: vec![],
        checks: vec![],
        still_active: vec![],
        unknowns: vec![],
        usage: None,
        report: None,
        verification: None,
    }
}

/// A Work run that says `last` and ends with `summary`.
async fn ends_saying(f: &Fixture, last: &str, summary: &str) -> (TaskId, RunId) {
    let project = f.project().await;
    let (task_id, _) = f
        .task(
            project,
            TaskPlacement::StartNow,
            &Caller::local_owner(),
            false,
        )
        .await;
    let run_id = run_started(f, task_id).await;
    f.harness
        .emit(
            run_id,
            ports::HarnessEvent::AgentMessage {
                message_key: "last".into(),
                text: last.into(),
            },
        )
        .await;
    f.harness
        .emit(
            run_id,
            ports::HarnessEvent::Exited {
                exit: ports::RunExit::Completed,
                outcome: Some(outcome(summary)),
            },
        )
        .await;
    until("the run ended", || run(f, run_id).state.is_terminal()).await;
    (task_id, run_id)
}

fn completed_body(f: &Fixture, run_id: RunId) -> Option<String> {
    f.hub
        .read(|r| r.list::<AttentionItem>("attention"))
        .unwrap()
        .into_iter()
        .find(|a| a.run_id == Some(run_id) && a.kind == AttentionKind::Completed)
        .expect("its end was raised")
        .body
}

const ASKS: &str = "Fixed the export.\n\n```ophio-outcome\n{\"status\":\"needs_you\",\"questions\":[\"Should the old export stay?\"]}\n```";

#[tokio::test]
async fn a_run_s_report_is_kept_and_taken_out_of_what_the_person_reads() {
    let f = Fixture::new().await;
    let (_, run_id) = ends_saying(&f, ASKS, ASKS).await;
    let outcome = run(&f, run_id).outcome.unwrap();
    assert_eq!(outcome.summary.as_deref(), Some("Fixed the export."));
    let report = outcome.report.unwrap();
    assert_eq!(report.status, ReportStatus::NeedsYou);
    assert_eq!(report.questions, ["Should the old export stay?"]);
    assert_eq!(
        completed_body(&f, run_id).as_deref(),
        Some("It needs you: Should the old export stay?")
    );
    f.close().await;
}

#[tokio::test]
async fn a_turn_called_done_with_a_failed_check_or_work_left_is_finished_needing_a_look() {
    let f = Fixture::new().await;
    let said =
        "Fixed the export.\n\n```ophio-outcome\n{\"status\":\"done\",\"new_work\":[\"Tidy\"]}\n```";
    let (_, run_id) = ends_saying(&f, said, said).await;
    let done = run(&f, run_id);
    assert_eq!(
        ended(&done).as_deref(),
        Some("Done. It suggests new work: Tidy")
    );
    let mut failed = done.clone();
    let outcome = failed.outcome.as_mut().unwrap();
    outcome.checks.push(CheckResult {
        name: "unit tests".into(),
        command: Some("cargo test".into()),
        status: CheckStatus::Failed,
        detail: None,
        provenance: Provenance::Observed,
        exit_code: Some(101),
        output: None,
        after_last_change: None,
    });
    outcome.unknowns.push("Whether the deploy took".into());
    assert_eq!(
        ended(&failed).as_deref(),
        Some(
            "Finished, needs a look: 1 check failed · unknowns remain. It suggests new work: Tidy"
        )
    );
    let mut helping = done.clone();
    helping.children.push(
        serde_json::from_value(serde_json::json!({
            "child_id": ChildId::new(), "native_ref": null, "label": "Check the layout",
            "state": "active", "started_at": 1, "ended_at": null, "provenance": "native",
        }))
        .unwrap(),
    );
    let outcome = helping.outcome.as_mut().unwrap();
    outcome.still_active.push("The layout review".into());
    outcome.report = None;
    assert_eq!(
        ended(&helping).as_deref(),
        Some("Finished, needs a look: 1 helper still running · work still active.")
    );
    // What it needs from the person still leads.
    let mut asks = failed.clone();
    asks.outcome.as_mut().unwrap().report =
        Some(read(r#"{"status":"needs_you","questions":["A?"]}"#).unwrap());
    assert_eq!(ended(&asks).as_deref(), Some("It needs you: A?"));
    f.close().await;
}

#[tokio::test]
async fn a_summary_cut_short_of_the_report_finds_it_in_the_last_message() {
    let f = Fixture::new().await;
    let (_, run_id) = ends_saying(&f, ASKS, "Fixed the export.").await;
    let outcome = run(&f, run_id).outcome.unwrap();
    assert_eq!(outcome.summary.as_deref(), Some("Fixed the export."));
    assert_eq!(outcome.report.unwrap().status, ReportStatus::NeedsYou);
    f.close().await;
}

#[tokio::test]
async fn what_a_review_reports_to_ophio_is_taken_out_of_the_summary_too() {
    let f = Fixture::new().await;
    let said = "Nothing worth filing.\n\n```ophio-findings\n[]\n```\n\n```ophio-outcome\n{\"status\":\"done\"}\n```";
    let (task, run_id) = ends_saying(&f, said, said).await;
    let outcome = run(&f, run_id).outcome.unwrap();
    assert_eq!(outcome.summary.as_deref(), Some("Nothing worth filing."));
    assert_eq!(outcome.report.unwrap().status, ReportStatus::Done);
    // Ophio still reads the findings from what it said.
    let told = f
        .hub
        .read(|r| crate::asking::last_message(r, task, run_id))
        .unwrap()
        .unwrap();
    assert!(told.contains("```ophio-findings\n[]\n```"), "{told}");
    f.close().await;
}

#[tokio::test]
async fn a_run_that_says_nothing_of_its_outcome_keeps_its_summary() {
    let f = Fixture::new().await;
    let (_, run_id) = ends_saying(&f, "All done.", "All done.").await;
    let outcome = run(&f, run_id).outcome.unwrap();
    assert_eq!(outcome.summary.as_deref(), Some("All done."));
    assert!(outcome.report.is_none());
    assert_eq!(completed_body(&f, run_id), None);
    f.close().await;
}

#[test]
fn only_a_block_of_its_own_is_taken() {
    let (rest, block) = take("Done.\n\n```ophio-outcome json\n{\"status\":\"done\"}\n```\n");
    assert_eq!(rest, "Done.");
    assert_eq!(block.as_deref(), Some("{\"status\":\"done\"}"));
    // One quoted inside another block is part of that block.
    let quoted = "Use this:\n````md\n```ophio-outcome\n{}\n```\n````";
    assert_eq!(take(quoted), (quoted.to_owned(), None));
    // A mention in passing isn't a block.
    let mention = "It ends with ```ophio-outcome blocks.";
    assert_eq!(take(mention), (mention.to_owned(), None));
    // The last one counts; one left open runs to the end.
    let (rest, block) = take(
        "A\n```ophio-outcome\n{\"status\":\"done\"}\n```\nB\n```ophio-outcome\n{\"status\":\"unfinished\"}",
    );
    assert_eq!(rest, "A\nB");
    assert_eq!(block.as_deref(), Some("{\"status\":\"unfinished\"}"));
}

#[test]
fn a_report_is_read_leniently_and_kept_small() {
    let report = read(
        r##"{"status": "Needs You", "questions": ["Which branch?", {"text": "Sign in to GitHub"}],
            "actions": ["Sign in to GitHub", "Approve the deploy"], "remaining": ["  "],
            "new_work": ["Tidy the README", {"title": "Plan the export", "mode": "plan", "issue": "#12"},
                         {"brief": "Find out why"}, {"title": " "}, 5]}"##,
    )
    .unwrap();
    assert_eq!(report.status, ReportStatus::NeedsYou);
    assert_eq!(
        report.questions,
        ["Which branch?", "Sign in to GitHub", "Approve the deploy"]
    );
    assert!(report.remaining.is_empty());
    let work: Vec<_> = (report.new_work.iter())
        .map(|w| (w.title.as_str(), w.brief.as_str(), w.mode, w.issue))
        .collect();
    assert_eq!(
        work,
        [
            ("Tidy the README", "Tidy the README", TaskMode::Work, None),
            (
                "Plan the export",
                "Plan the export",
                TaskMode::Plan,
                Some(12)
            ),
            ("Find out why", "Find out why", TaskMode::Work, None),
        ]
    );
    for status in ["completed", "finished", "done"] {
        let block = format!("{{\"status\":\"{status}\"}}");
        assert_eq!(read(&block).unwrap().status, ReportStatus::Done);
    }
    assert_eq!(
        read(r#"{"status":"in progress"}"#).unwrap().status,
        ReportStatus::Unfinished
    );
    assert!(read(r#"{"status":"great"}"#).is_none());
    assert!(read(r#"["done"]"#).is_none());
    assert!(read("not json").is_none());
    let many: Vec<String> = (0..20).map(|i| format!("\"q{i}\"")).collect();
    let long = "x".repeat(2000);
    let report = read(&format!(
        "{{\"status\":\"unfinished\",\"remaining\":[{}],\"questions\":[\"{long}\"]}}",
        many.join(",")
    ))
    .unwrap();
    assert_eq!(report.remaining.len(), 8);
    assert!(report.questions[0].chars().count() <= 601);
}

#[test]
fn the_notification_says_what_the_person_is_needed_for() {
    let report = |block: &str| read(block).unwrap();
    assert_eq!(
        headline(&report(r#"{"status":"needs_you","questions":["A?","B?"]}"#)).as_deref(),
        Some("It needs you: A? (and 1 more)")
    );
    assert_eq!(
        headline(&report(
            r#"{"status":"unfinished","remaining":["The tests"]}"#
        ))
        .as_deref(),
        Some("Not finished. Left: The tests")
    );
    assert_eq!(
        headline(&report(r#"{"status":"done","new_work":["Tidy","Plan"]}"#)).as_deref(),
        Some("Done. It suggests 2 new tasks.")
    );
    assert_eq!(headline(&report(r#"{"status":"done"}"#)), None);
}

fn changed(path: &str) -> ChangedFile {
    ChangedFile {
        path: path.into(),
        change: FileChange::Modified,
        added: None,
        removed: None,
        diff: None,
        provenance: Some(Provenance::Native),
    }
}

/// Continues `task_id` with a run that ends with `outcome`.
async fn next_run_ends(f: &Fixture, task_id: TaskId, outcome: RunOutcome) -> RunId {
    let revision = f
        .hub
        .read(|r| r.require::<Task>("task", task_id))
        .unwrap()
        .revision;
    let receipt = f
        .hub
        .submit(
            &Caller::local_owner(),
            edit(
                Command::ResumeTask {
                    task_id,
                    text: Some("It still breaks on an empty file.".into()),
                    bundle: None,
                    mode: None,
                    carried_plan: None,
                    read_only: false,
                    model: None,
                    effort: None,
                },
                revision,
            ),
        )
        .await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    let run_id = run_started(f, task_id).await;
    f.harness
        .emit(
            run_id,
            ports::HarnessEvent::Exited {
                exit: ports::RunExit::Completed,
                outcome: Some(outcome),
            },
        )
        .await;
    until("the next run ended", || run(f, run_id).state.is_terminal()).await;
    run_id
}

#[tokio::test]
async fn a_run_called_done_whose_next_run_changed_files_needed_a_follow_up() {
    let f = Fixture::new().await;
    let done = "Fixed the export.\n\n```ophio-outcome\n{\"status\":\"done\"}\n```";
    let (task_id, first) = ends_saying(&f, done, done).await;
    assert_eq!(run(&f, first).follow_up, None);
    let mut fixed = outcome("Fixed it again.");
    fixed.changed_files.push(changed("src/export.rs"));
    let second = next_run_ends(&f, task_id, fixed).await;
    let follow_up = run(&f, first).follow_up.expect("it needed a follow-up");
    assert_eq!(follow_up.run_id, second);
    assert_eq!(follow_up.reason, FollowUpReason::ChangedFiles);
    // The second run's own record is untouched until a third run says otherwise.
    assert_eq!(run(&f, second).follow_up, None);
    // A third run that only looks changes nothing about the second.
    let third = next_run_ends(&f, task_id, outcome("Looked again.")).await;
    assert_eq!(run(&f, second).follow_up, None);
    assert_eq!(run(&f, third).follow_up, None);
    f.close().await;
}

#[tokio::test]
async fn a_check_failing_before_the_next_run_changed_anything_marks_the_done_run() {
    let f = Fixture::new().await;
    let (task_id, first) = ends_saying(&f, "Done.", "Done.").await;
    let mut retested = outcome("The tests fail.");
    retested.verification = Some(RunVerification {
        status: VerificationStatus::Failed,
        passed: 0,
        failed: 1,
        unknown: 0,
        failed_not_rerun: 0,
        failed_before_changes: true,
        incomplete: None,
    });
    next_run_ends(&f, task_id, retested).await;
    assert_eq!(
        run(&f, first).follow_up.map(|f| f.reason),
        Some(FollowUpReason::CheckFailed)
    );
    f.close().await;
}

#[tokio::test]
async fn a_run_that_left_work_expects_the_next_one_to_change_files() {
    let f = Fixture::new().await;
    let said = "Half done.\n\n```ophio-outcome\n{\"status\":\"unfinished\",\"remaining\":[\"Import\"]}\n```";
    let (task_id, first) = ends_saying(&f, said, said).await;
    let mut carried_on = outcome("Did the import.");
    carried_on.changed_files.push(changed("src/import.rs"));
    next_run_ends(&f, task_id, carried_on).await;
    assert_eq!(run(&f, first).follow_up, None);
    f.close().await;
}

#[tokio::test]
async fn only_checks_that_failed_after_the_last_change_are_doubts() {
    let f = Fixture::new().await;
    let (_, run_id) = ends_saying(&f, "Fixed.", "Fixed.").await;
    let mut fixed = run(&f, run_id);
    let outcome = fixed.outcome.as_mut().unwrap();
    outcome.checks.push(CheckResult {
        name: "Check command".into(),
        command: Some("cargo test".into()),
        status: CheckStatus::Failed,
        detail: Some("Exit status 101".into()),
        provenance: Provenance::Native,
        exit_code: Some(101),
        output: None,
        after_last_change: Some(false),
    });
    // Recorded before verification was kept: every failure counts.
    assert_eq!(crate::reported::doubts(&fixed), ["1 check failed"]);
    fixed.outcome.as_mut().unwrap().verification = Some(RunVerification {
        status: VerificationStatus::Verified,
        passed: 1,
        failed: 0,
        unknown: 0,
        failed_not_rerun: 0,
        failed_before_changes: true,
        incomplete: None,
    });
    assert!(crate::reported::doubts(&fixed).is_empty());
    // A failure never run again is still one.
    let v = fixed
        .outcome
        .as_mut()
        .unwrap()
        .verification
        .as_mut()
        .unwrap();
    v.status = VerificationStatus::NotChecked;
    v.passed = 0;
    v.failed_not_rerun = 1;
    assert_eq!(
        crate::reported::doubts(&fixed),
        ["1 check failed and wasn’t run again"]
    );
    assert_eq!(
        crate::reported::ended(&fixed).as_deref(),
        Some("Finished, needs a look: 1 check failed and wasn’t run again.")
    );
    f.close().await;
}

#[tokio::test]
async fn a_next_run_that_was_stopped_says_nothing_of_the_one_before() {
    let f = Fixture::new().await;
    let (task_id, first) = ends_saying(&f, "Done.", "Done.").await;
    let revision = f
        .hub
        .read(|r| r.require::<Task>("task", task_id))
        .unwrap()
        .revision;
    let receipt = f
        .hub
        .submit(
            &Caller::local_owner(),
            edit(
                Command::ResumeTask {
                    task_id,
                    text: Some("Run the tests.".into()),
                    bundle: None,
                    mode: None,
                    carried_plan: None,
                    read_only: false,
                    model: None,
                    effort: None,
                },
                revision,
            ),
        )
        .await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    let second = run_started(&f, task_id).await;
    let mut stopped = outcome("Stopped.");
    stopped.changed_files.push(changed("src/export.rs"));
    stopped.verification = Some(RunVerification {
        status: VerificationStatus::Failed,
        passed: 0,
        failed: 1,
        unknown: 0,
        failed_not_rerun: 0,
        failed_before_changes: true,
        incomplete: None,
    });
    f.harness
        .emit(
            second,
            ports::HarnessEvent::Exited {
                exit: ports::RunExit::Interrupted,
                outcome: Some(stopped),
            },
        )
        .await;
    until("the stopped run ended", || {
        run(&f, second).state.is_terminal()
    })
    .await;
    assert_eq!(run(&f, second).state, RunState::Interrupted);
    assert_eq!(run(&f, first).follow_up, None);
    f.close().await;
}
