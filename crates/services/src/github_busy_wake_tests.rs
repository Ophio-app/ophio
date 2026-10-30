// SPDX-License-Identifier: Apache-2.0
//! GitHub news waits for the writer rather than repeatedly attempting a refused resume.
use super::*;
use crate::github_watch::{HANDOVER, WATCH, Watch};
use ports::Clock;
use store::CommandRead;

fn task(r: &Repo, id: TaskId) -> Task {
    r.f.hub.read(|x| x.require("task", id)).unwrap()
}

fn charged(r: &Repo, id: TaskId) -> Option<Handover> {
    r.f.hub.read(|x| x.get(HANDOVER, id)).unwrap()
}

async fn watching() -> (Repo, TaskId, RunId) {
    let r = repo(true).await;
    r.set(drafts()).await.unwrap();
    r.gh.answer("GET /repos/octo/app/pulls/7", pull(7, "octo/app", "me"));
    let (id, run) = r.start(GithubLink::Pr { number: 7 }, "Finish it").await;
    r.ends(id, run, None).await;
    (r, id, run)
}

fn news(r: &Repo, head: &str, check: &str) {
    r.gh.query(
        "viewer{login}",
        super::github_after_tests::watched_pr(
            head,
            json!([{ "__typename": "CheckRun", "name": check,
                     "status": "COMPLETED", "conclusion": "FAILURE",
                     "detailsUrl": format!("https://ci/{check}"), "isRequired": true }]),
            json!([]),
            "BLOCKED",
        ),
    );
}

async fn poll(r: &Repo, id: TaskId) {
    // Recheck without aging a synthetic active writer into the run watchdog.
    r.f.hub
        .write(|tx| {
            if let Some(mut watch) = tx.read().get::<Watch>(WATCH, id)? {
                watch.polled_at = Timestamp(0);
                tx.put(WATCH, id, &watch)?;
            }
            Ok(())
        })
        .unwrap();
    r.f.hub.watch_prs().await.unwrap();
}

fn resume_commands(r: &Repo, id: TaskId) -> usize {
    r.f.hub.read(|x| x.commands()).unwrap().into_iter().filter(|entry| {
        matches!(entry, CommandRead::Decoded(record)
            if matches!(&record.envelope.body, Command::ResumeTask { task_id, .. } if *task_id == id))
    }).count()
}

fn no_charge(r: &Repo, id: TaskId, previous: RunId) {
    assert_eq!(task(r, id).current_run, Some(previous));
    assert_eq!(task(r, id).github.unwrap().rounds, 0);
    assert!(charged(r, id).is_none());
    assert_eq!(
        resume_commands(r, id),
        0,
        "busy polls must not submit rejected resumes"
    );
}

#[tokio::test]
async fn failing_checks_wait_for_another_writer_and_take_only_the_latest_news() {
    let (r, id, previous) = watching().await;
    let (writer, _) =
        r.f.task(
            r.project_id,
            TaskPlacement::StartNow,
            &Caller::local_owner(),
            false,
        )
        .await;
    let writer_run = run_started(&r.f, writer).await;
    news(&r, "a", "old");
    for _ in 0..3 {
        poll(&r, id).await;
        no_charge(&r, id, previous);
    }
    // A new head changes the news while the project is still busy.
    news(&r, "b", "latest");
    for _ in 0..3 {
        poll(&r, id).await;
        no_charge(&r, id, previous);
    }
    r.ends(writer, writer_run, None).await;
    poll(&r, id).await;
    let resumed = r.next_run(id, previous).await;
    let prompt = r.spec(resumed).prompt.text;
    assert!(prompt.contains("`latest` failed"), "{prompt}");
    assert!(!prompt.contains("`old` failed"), "{prompt}");
    assert_eq!(task(&r, id).github.unwrap().rounds, 1);
    assert!(charged(&r, id).is_none());
    poll(&r, id).await;
    assert_eq!(task(&r, id).current_run, Some(resumed));
    assert_eq!(task(&r, id).github.unwrap().rounds, 1);
    r.f.close().await;
}

#[tokio::test]
async fn unknown_writer_and_own_finishing_helpers_cannot_consume_news() {
    let (r, id, previous) = watching().await;
    news(&r, "a", "test");
    // A missing writer record is uncertain, not an invitation to issue a resume.
    let unknown = RunId::new();
    r.f.hub
        .write(|tx| {
            let mut project: Project = tx.read().require("project", r.project_id)?;
            project.active_writer = Some(unknown);
            tx.put("project", r.project_id, &project)
        })
        .unwrap();
    for _ in 0..3 {
        poll(&r, id).await;
        no_charge(&r, id, previous);
    }
    r.f.hub
        .write(|tx| {
            let mut project: Project = tx.read().require("project", r.project_id)?;
            project.active_writer = None;
            tx.put("project", r.project_id, &project)?;
            let mut run: Run = tx.read().require("run", previous)?;
            run.holds_writer = true;
            tx.put("run", previous, &run)
        })
        .unwrap();
    for _ in 0..3 {
        poll(&r, id).await;
        no_charge(&r, id, previous);
    }
    r.f.hub
        .write(|tx| {
            let mut run: Run = tx.read().require("run", previous)?;
            run.holds_writer = false;
            tx.put("run", previous, &run)
        })
        .unwrap();
    poll(&r, id).await;
    let resumed = r.next_run(id, previous).await;
    assert!(r.spec(resumed).prompt.text.contains("`test` failed"));
    assert_eq!(task(&r, id).github.unwrap().rounds, 1);
    r.f.close().await;
}

#[tokio::test]
async fn persisted_charged_round_waits_without_a_command_then_delivers_once() {
    let (r, id, previous) = watching().await;
    let (writer, _) =
        r.f.task(
            r.project_id,
            TaskPlacement::StartNow,
            &Caller::local_owner(),
            false,
        )
        .await;
    let writer_run = run_started(&r.f, writer).await;
    // The charged round was made from this already-recorded PR summary.
    // Reading a different summary would correctly invalidate its revision.
    news(&r, "a", "test");
    poll(&r, id).await;
    no_charge(&r, id, previous);
    r.alter(id, |g| {
        g.checked_by = Some(claude());
        g.stage = Some(PrStage::Babysitting);
        g.rounds = 1;
    });
    let mut handover = handover(id, task(&r, id).revision, r.f.clock.now());
    handover.before = r.f.hub.read(|x| x.require(WATCH, id)).unwrap();
    // The charged round already consumed this check's news. Those markers
    // must survive waiting and delivery, or the same check starts a second round.
    let mut consumed = handover.before.clone();
    consumed.failed.insert("test".into());
    r.f.hub
        .write(|tx| {
            tx.put(HANDOVER, id, &handover)?;
            tx.put(WATCH, id, &consumed)
        })
        .unwrap();
    for _ in 0..4 {
        poll(&r, id).await;
        let stored = charged(&r, id).expect("durable handover must remain");
        assert_eq!(stored.command_id, handover.command_id);
        assert!(
            r.f.hub
                .read(|x| x.command(handover.command_id))
                .unwrap()
                .is_none(),
            "busy handover must not issue even a rejected command"
        );
        assert_eq!(task(&r, id).current_run, Some(previous));
        assert_eq!(task(&r, id).github.as_ref().unwrap().rounds, 1);
        let watch: Watch = r.f.hub.read(|x| x.require(WATCH, id)).unwrap();
        assert!(
            watch.failed.contains("test"),
            "charged news must stay consumed"
        );
    }
    r.ends(writer, writer_run, None).await;
    poll(&r, id).await;
    let resumed = r.next_run(id, previous).await;
    assert!(r.spec(resumed).prompt.text.contains("lost rename"));
    assert!(charged(&r, id).is_none());
    assert_eq!(task(&r, id).github.unwrap().rounds, 1);
    r.ends(id, resumed, None).await;
    poll(&r, id).await;
    assert_eq!(task(&r, id).current_run, Some(resumed));
    assert_eq!(task(&r, id).github.as_ref().unwrap().rounds, 1);
    assert!(charged(&r, id).is_none());
    assert_eq!(resume_commands(&r, id), 1);
    assert_eq!(
        r.f.hub
            .read(|x| x.command(handover.command_id))
            .unwrap()
            .unwrap()
            .receipt
            .status,
        ReceiptStatus::Accepted
    );
    r.f.close().await;
}
