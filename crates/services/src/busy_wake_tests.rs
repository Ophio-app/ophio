// SPDX-License-Identifier: Apache-2.0
//! A wake is an intent to start the next turn when it is safe, not a request
//! to append to a different turn or to compete with the person's Next up.
use super::*;
use crate::followups::{FOLLOWUP, Followup, wake_key};
use ports::Clock;

fn saved(f: &Fixture, task_id: TaskId) -> Followup {
    f.hub
        .read(|r| r.require(FOLLOWUP, wake_key(task_id)))
        .unwrap()
}

fn no_attempt(f: &Fixture, followup: &Followup) {
    assert_eq!(saved(f, followup.task_id).command_id, followup.command_id);
    assert!(
        f.hub
            .read(|r| r.command(followup.command_id))
            .unwrap()
            .is_none(),
        "a busy wake must not submit even a rejected command"
    );
}

async fn finished(f: &Fixture, task_id: TaskId, run_id: RunId) {
    f.harness
        .emit(
            run_id,
            ports::HarnessEvent::Exited {
                exit: ports::RunExit::Completed,
                outcome: None,
            },
        )
        .await;
    super::watch_tests::until("turn and its cleanup finished", || {
        let task: Task = f.hub.read(|r| r.require("task", task_id)).unwrap();
        let run: Run = f.hub.read(|r| r.require("run", run_id)).unwrap();
        task.state == TaskState::Completed && !crate::runs::still_finishing(&run)
    })
    .await;
}

async fn completed_task(f: &Fixture, project: ProjectId) -> (TaskId, RunId) {
    let (task_id, _) = f
        .task(
            project,
            TaskPlacement::StartNow,
            &Caller::local_owner(),
            false,
        )
        .await;
    let run_id = run_started(f, task_id).await;
    finished(f, task_id, run_id).await;
    (task_id, run_id)
}

fn wake(f: &Fixture, task_id: TaskId, text: &str) -> Followup {
    let now = f.clock.now();
    let followup = Followup::wake(task_id, text.into(), Timestamp(now.0 + 60_000), now);
    f.hub
        .write(|tx| {
            followup.put(tx)?;
            let mut task: Task = tx.read().require("task", task_id)?;
            task.wake = Some(TaskWake {
                at: followup.due_at,
                reason: text.into(),
                answer: false,
            });
            tx.put("task", task_id, &task)
        })
        .unwrap();
    f.clock.advance(Duration::from_secs(60));
    followup
}

async fn assert_delivered_once(f: &Fixture, previous: &Followup, old_run: RunId, text: &str) {
    run_started(f, previous.task_id).await;
    assert!(
        f.hub
            .read(|r| r.get::<Followup>(FOLLOWUP, &previous.key))
            .unwrap()
            .is_none()
    );
    let task: Task = f.hub.read(|r| r.require("task", previous.task_id)).unwrap();
    assert!(task.wake.is_none());
    assert_ne!(task.current_run, Some(old_run));
    assert_eq!(
        f.hub
            .read(|r| r.command(previous.command_id))
            .unwrap()
            .unwrap()
            .receipt
            .status,
        ReceiptStatus::Accepted
    );
    let prompts = f
        .harness
        .specs
        .lock()
        .unwrap()
        .iter()
        .map(|spec| spec.prompt.text.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        prompts
            .iter()
            .filter(|prompt| prompt.starts_with(text))
            .count(),
        1,
        "exactly one resumed turn contains the latest wake; prompts: {prompts:?}"
    );
}

#[tokio::test]
async fn wake_waits_through_another_writer_for_over_a_day_then_starts_once() {
    let f = Fixture::new().await;
    let project = f.project().await;
    let (task_id, previous_run) = completed_task(&f, project).await;
    let (writer_task, _) = f
        .task(
            project,
            TaskPlacement::StartNow,
            &Caller::local_owner(),
            false,
        )
        .await;
    let writer_run = run_started(&f, writer_task).await;
    let original = wake(&f, task_id, "first wake");
    for _ in 0..30 {
        f.clock.advance(Duration::from_secs(60 * 60));
        assert_eq!(f.hub.pass_on_followups().await.unwrap(), 0);
        no_attempt(&f, &original);
    }
    // The latest wake replaces the old text and delivery ID while busy.
    let latest = Followup::wake(task_id, "newer wake".into(), f.clock.now(), f.clock.now());
    f.hub.write(|tx| latest.put(tx)).unwrap();
    assert_ne!(latest.command_id, original.command_id);
    for _ in 0..12 {
        assert_eq!(f.hub.pass_on_followups().await.unwrap(), 0);
        no_attempt(&f, &latest);
    }
    finished(&f, writer_task, writer_run).await;
    f.hub.pass_on_followups().await.unwrap();
    super::watch_tests::until("wake reached its next turn", || {
        f.hub
            .read(|r| r.get::<Followup>(FOLLOWUP, &latest.key))
            .unwrap()
            .is_none()
    })
    .await;
    assert_delivered_once(&f, &latest, previous_run, "newer wake").await;
    assert!(
        f.harness
            .specs
            .lock()
            .unwrap()
            .iter()
            .all(|spec| !spec.prompt.text.starts_with("first wake"))
    );
    assert_eq!(f.hub.pass_on_followups().await.unwrap(), 0);
    f.close().await;
}

#[tokio::test]
async fn uncertain_writer_does_not_consume_or_queue_a_wake() {
    let f = Fixture::new().await;
    let project = f.project().await;
    let (task_id, previous_run) = completed_task(&f, project).await;
    let (other, _) = f
        .task(
            project,
            TaskPlacement::StartNow,
            &Caller::local_owner(),
            false,
        )
        .await;
    let writer = run_started(&f, other).await;
    f.hub
        .write(|tx| {
            let mut run: Run = tx.read().require("run", writer)?;
            run.state = RunState::ReconciliationRequired;
            tx.put("run", writer, &run)
        })
        .unwrap();
    let followup = wake(&f, task_id, "uncertain writer wake");
    for _ in 0..8 {
        assert_eq!(f.hub.pass_on_followups().await.unwrap(), 0);
        no_attempt(&f, &followup);
    }
    assert!(
        super::watch_tests::queue(&f, project)
            .items
            .iter()
            .all(|i| i.task_id != task_id)
    );
    // Reconciliation releases the writer; the pending wake remains useful.
    f.hub
        .write(|tx| {
            let mut project_row: Project = tx.read().require("project", project)?;
            project_row.active_writer = None;
            tx.put("project", project, &project_row)?;
            let mut q: QueueView = tx.read().require("queue", project)?;
            q.writer = None;
            tx.put("queue", project, &q)
        })
        .unwrap();
    f.hub.pass_on_followups().await.unwrap();
    assert_delivered_once(&f, &followup, previous_run, "uncertain writer wake").await;
    f.close().await;
}

#[tokio::test]
async fn ask_wake_also_waits_for_another_work_writer() {
    let f = Fixture::new().await;
    let project = f.project().await;
    let (task_id, _) = completed_task(&f, project).await;
    f.hub
        .write(|tx| {
            let mut task: Task = tx.read().require("task", task_id)?;
            task.mode = TaskMode::Ask;
            tx.put("task", task_id, &task)
        })
        .unwrap();
    let (other, _) = f
        .task(
            project,
            TaskPlacement::StartNow,
            &Caller::local_owner(),
            false,
        )
        .await;
    run_started(&f, other).await;
    let followup = wake(&f, task_id, "ask after work");
    for _ in 0..8 {
        assert_eq!(f.hub.pass_on_followups().await.unwrap(), 0);
        no_attempt(&f, &followup);
    }
    // The writer appeared after a stale preflight: admission must refuse an
    // Ask wake even though Ask turns do not themselves hold the writer.
    f.hub
        .write(|tx| {
            let mut infos: Vec<protocol::capability::HarnessInfo> =
                tx.read().require("meta", "harnesses")?;
            infos[0].modes.push(protocol::capability::ModeSupport {
                mode: TaskMode::Ask,
                availability: protocol::capability::Availability::Available,
                enforcement: "Synthetic read-only fixture".into(),
            });
            tx.put("meta", "harnesses", &infos)
        })
        .unwrap();
    let previous = super::watch_tests::task(&f, task_id).current_run;
    let receipt = f
        .hub
        .admit_with_followup(
            &Caller::local_owner(),
            edit(
                Command::ResumeTask {
                    task_id,
                    text: Some(followup.text.clone()),
                    bundle: None,
                    mode: None,
                    carried_plan: None,
                    read_only: false,
                    model: None,
                    effort: None,
                },
                super::watch_tests::task(&f, task_id).revision,
            ),
            f.clock.now(),
            None,
            Some(&followup),
        )
        .await;
    assert_eq!(receipt.error.unwrap().code, ErrorCode::Conflict);
    assert_eq!(super::watch_tests::task(&f, task_id).current_run, previous);
    assert_eq!(saved(&f, task_id).command_id, followup.command_id);
    f.close().await;
}

#[tokio::test]
async fn wake_waits_for_own_nonterminal_run_and_ask_cleanup() {
    for ask in [false, true] {
        let f = Fixture::new().await;
        let project = f.project().await;
        let (task_id, previous_run) = completed_task(&f, project).await;
        f.hub
            .write(|tx| {
                let mut task: Task = tx.read().require("task", task_id)?;
                if ask {
                    task.mode = TaskMode::Ask;
                }
                tx.put("task", task_id, &task)?;
                let mut run: Run = tx.read().require("run", previous_run)?;
                if ask {
                    run.mode = Some(TaskMode::Ask);
                    run.cleanup_pending = true;
                } else {
                    run.state = RunState::Working;
                }
                tx.put("run", previous_run, &run)
            })
            .unwrap();
        let followup = wake(&f, task_id, "wait for own run");
        for _ in 0..8 {
            assert_eq!(f.hub.pass_on_followups().await.unwrap(), 0);
            no_attempt(&f, &followup);
        }
        if ask {
            // This fixture's harness does not advertise Ask-mode enforcement,
            // so assert the cleanup gate without attempting an unsupported resume.
            f.close().await;
            continue;
        }
        f.hub
            .write(|tx| {
                let mut run: Run = tx.read().require("run", previous_run)?;
                run.state = RunState::Completed;
                run.cleanup_pending = false;
                tx.put("run", previous_run, &run)
            })
            .unwrap();
        f.hub.pass_on_followups().await.unwrap();
        assert_delivered_once(&f, &followup, previous_run, "wait for own run").await;
        f.close().await;
    }
}

#[tokio::test]
async fn wake_waits_while_own_work_helpers_keep_the_writer() {
    let f = Fixture::new().await;
    let project = f.project().await;
    let (task_id, previous_run) = completed_task(&f, project).await;
    f.hub
        .write(|tx| {
            let mut run: Run = tx.read().require("run", previous_run)?;
            assert!(run.ended_at.is_some());
            run.holds_writer = true;
            tx.put("run", previous_run, &run)?;
            let mut project_row: Project = tx.read().require("project", project)?;
            project_row.active_writer = Some(previous_run);
            tx.put("project", project, &project_row)
        })
        .unwrap();
    assert!(crate::runs::helpers_finishing(&super::watch_tests::run(
        &f,
        previous_run
    )));
    let followup = wake(&f, task_id, "helpers finished");
    for _ in 0..8 {
        assert_eq!(f.hub.pass_on_followups().await.unwrap(), 0);
        no_attempt(&f, &followup);
    }
    f.hub
        .write(|tx| {
            let mut run: Run = tx.read().require("run", previous_run)?;
            run.holds_writer = false;
            tx.put("run", previous_run, &run)?;
            let mut project_row: Project = tx.read().require("project", project)?;
            project_row.active_writer = None;
            tx.put("project", project, &project_row)
        })
        .unwrap();
    f.hub.pass_on_followups().await.unwrap();
    assert_delivered_once(&f, &followup, previous_run, "helpers finished").await;
    f.close().await;
}

#[tokio::test]
async fn direct_person_resume_finishes_before_its_pending_wake() {
    let f = Fixture::new().await;
    let project = f.project().await;
    let (task_id, previous_run) = completed_task(&f, project).await;
    let followup = wake(&f, task_id, "after person's turn");
    // Keep the wake not-yet-due until the person's direct ResumeTask is admitted.
    f.hub
        .write(|tx| {
            let mut later: Followup = tx.read().require(FOLLOWUP, &followup.key)?;
            later.due_at = Timestamp(f.clock.now().0 + 60_000);
            later.put(tx)
        })
        .unwrap();
    let person = f
        .hub
        .submit(
            &Caller::local_owner(),
            edit(
                Command::ResumeTask {
                    task_id,
                    text: Some("person's direct turn".into()),
                    bundle: None,
                    mode: None,
                    carried_plan: None,
                    read_only: false,
                    model: None,
                    effort: None,
                },
                super::watch_tests::task(&f, task_id).revision,
            ),
        )
        .await;
    assert_eq!(person.status, ReceiptStatus::Accepted, "{person:?}");
    let person_run = run_started(&f, task_id).await;
    assert_ne!(person_run, previous_run);
    f.clock.advance(Duration::from_secs(60));
    for _ in 0..8 {
        assert_eq!(f.hub.pass_on_followups().await.unwrap(), 0);
        no_attempt(&f, &followup);
    }
    finished(&f, task_id, person_run).await;
    f.hub.pass_on_followups().await.unwrap();
    assert_delivered_once(&f, &followup, person_run, "after person's turn").await;
    f.close().await;
}

#[tokio::test]
async fn pending_continuation_for_same_task_precedes_wake_even_after_restart() {
    let f = Fixture::new().await;
    let project = f.project().await;
    let (task_id, previous_run) = completed_task(&f, project).await;
    // The person's queued continuation gets its own turn first.
    let queued = f
        .hub
        .submit(
            &Caller::local_owner(),
            edit(
                Command::QueueContinuation {
                    task_id,
                    text: "person's queued turn".into(),
                    bundle: None,
                    authorization: QueueAuthorization::AskBeforeStart,
                },
                super::watch_tests::task(&f, task_id).revision,
            ),
        )
        .await;
    assert_eq!(queued.status, ReceiptStatus::Accepted, "{queued:?}");
    let followup = wake(&f, task_id, "person goes first");
    for _ in 0..8 {
        assert_eq!(f.hub.pass_on_followups().await.unwrap(), 0);
        no_attempt(&f, &followup);
    }
    let f = f.reopened(|builder| builder).await;
    for _ in 0..8 {
        assert_eq!(f.hub.pass_on_followups().await.unwrap(), 0);
        no_attempt(&f, &followup);
    }
    let queue = super::watch_tests::queue(&f, project);
    let start = f
        .hub
        .submit(
            &Caller::local_owner(),
            edit(
                Command::QueueStart {
                    item_id: queue.items[0].item_id,
                    task_revision: super::watch_tests::task(&f, task_id).revision,
                },
                queue.revision,
            ),
        )
        .await;
    assert_eq!(start.status, ReceiptStatus::Accepted, "{start:?}");
    let person_run = run_started(&f, task_id).await;
    assert_ne!(person_run, previous_run);
    assert_eq!(f.hub.pass_on_followups().await.unwrap(), 0);
    no_attempt(&f, &followup);
    assert!(
        f.harness
            .specs
            .lock()
            .unwrap()
            .iter()
            .any(|spec| spec.prompt.text.starts_with("person's queued turn"))
    );
    finished(&f, task_id, person_run).await;
    f.hub.pass_on_followups().await.unwrap();
    assert_delivered_once(&f, &followup, person_run, "person goes first").await;
    let starts = f.harness.starts.load(Ordering::SeqCst);
    let f = f.reopened(|builder| builder).await;
    // A crash after admission but before followup cleanup must reuse the
    // accepted command, never start the turn a second time.
    f.hub.write(|tx| followup.put(tx)).unwrap();
    assert_eq!(f.hub.pass_on_followups().await.unwrap(), 1);
    assert!(
        f.hub
            .read(|r| r.get::<Followup>(FOLLOWUP, &followup.key))
            .unwrap()
            .is_none()
    );
    assert_eq!(f.harness.starts.load(Ordering::SeqCst), starts);
    assert_eq!(f.hub.pass_on_followups().await.unwrap(), 0);
    f.close().await;
}

#[tokio::test]
async fn non_wake_answer_defers_locally_behind_its_queue_item_or_cleanup() {
    for queued in [false, true] {
        let f = Fixture::new().await;
        let project = f.project().await;
        let (task_id, previous_run) = completed_task(&f, project).await;
        let writer = if queued {
            let receipt = f
                .hub
                .submit(
                    &Caller::local_owner(),
                    edit(
                        Command::QueueContinuation {
                            task_id,
                            text: "person first".into(),
                            bundle: None,
                            authorization: QueueAuthorization::AskBeforeStart,
                        },
                        super::watch_tests::task(&f, task_id).revision,
                    ),
                )
                .await;
            assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
            let (other, _) = f
                .task(
                    project,
                    TaskPlacement::StartNow,
                    &Caller::local_owner(),
                    false,
                )
                .await;
            let run = run_started(&f, other).await;
            Some((other, run))
        } else {
            f.hub
                .write(|tx| {
                    let mut run: Run = tx.read().require("run", previous_run)?;
                    run.cleanup_pending = true;
                    tx.put("run", previous_run, &run)
                })
                .unwrap();
            None
        };
        let followup = Followup::answer(
            task_id,
            ApprovalId::new(),
            "late answer".into(),
            f.clock.now(),
        );
        f.hub.write(|tx| followup.put(tx)).unwrap();
        for _ in 0..8 {
            assert_eq!(f.hub.pass_on_followups().await.unwrap(), 0);
            assert_eq!(
                f.hub
                    .read(|r| r.require::<Followup>(FOLLOWUP, &followup.key))
                    .unwrap()
                    .command_id,
                followup.command_id
            );
            assert!(
                f.hub
                    .read(|r| r.command(followup.command_id))
                    .unwrap()
                    .is_none()
            );
        }
        if let Some((other, writer_run)) = writer {
            finished(&f, other, writer_run).await;
            f.hub.pass_on_followups().await.unwrap();
            let resumed = run_started(&f, task_id).await;
            assert_ne!(resumed, previous_run);
            let prompts = f
                .harness
                .specs
                .lock()
                .unwrap()
                .iter()
                .map(|spec| spec.prompt.text.clone())
                .collect::<Vec<_>>();
            assert_eq!(
                prompts
                    .iter()
                    .filter(|text| text.contains("person first") && text.contains("late answer"))
                    .count(),
                1,
                "queued turn and answer must arrive together: {prompts:?}"
            );
            assert!(
                f.hub
                    .read(|r| r.get::<Followup>(FOLLOWUP, &followup.key))
                    .unwrap()
                    .is_none()
            );
            assert_eq!(f.hub.pass_on_followups().await.unwrap(), 0);
        }
        f.close().await;
    }
}

#[tokio::test]
async fn temporarily_unavailable_harness_keeps_wake_past_a_day_without_attempts() {
    use protocol::capability::{Availability, HarnessInfo};
    for start_run in [true, false] {
        let f = Fixture::new().await;
        let project = f.project().await;
        let (task_id, previous_run) = completed_task(&f, project).await;
        let followup = wake(&f, task_id, "available later");
        f.hub
            .write(|tx| {
                let mut infos: Vec<HarnessInfo> = tx.read().require("meta", "harnesses")?;
                let capability = if start_run {
                    &mut infos[0].capabilities.start_run
                } else {
                    &mut infos[0].capabilities.resume_session
                };
                *capability = Availability::TemporarilyUnavailable {
                    reason: "synthetic outage".into(),
                };
                tx.put("meta", "harnesses", &infos)
            })
            .unwrap();
        f.clock.advance(Duration::from_secs(2 * 24 * 60 * 60));
        for _ in 0..16 {
            assert_eq!(f.hub.pass_on_followups().await.unwrap(), 0);
            no_attempt(&f, &followup);
        }
        f.hub
            .write(|tx| {
                let mut infos: Vec<HarnessInfo> = tx.read().require("meta", "harnesses")?;
                if start_run {
                    infos[0].capabilities.start_run = Availability::Available;
                } else {
                    infos[0].capabilities.resume_session = Availability::Available;
                }
                tx.put("meta", "harnesses", &infos)
            })
            .unwrap();
        f.hub.pass_on_followups().await.unwrap();
        assert_delivered_once(&f, &followup, previous_run, "available later").await;
        f.close().await;
    }
}

#[tokio::test]
async fn non_wake_answer_still_queues_behind_another_writer() {
    let f = Fixture::new().await;
    let project = f.project().await;
    let (task_id, _) = completed_task(&f, project).await;
    let (other, _) = f
        .task(
            project,
            TaskPlacement::StartNow,
            &Caller::local_owner(),
            false,
        )
        .await;
    run_started(&f, other).await;
    let followup = Followup::answer(task_id, ApprovalId::new(), "answer".into(), f.clock.now());
    f.hub.write(|tx| followup.put(tx)).unwrap();
    assert_eq!(f.hub.pass_on_followups().await.unwrap(), 1);
    assert!(
        super::watch_tests::queue(&f, project)
            .items
            .iter()
            .any(|i| i.task_id == task_id && i.is_pending())
    );
    f.close().await;
}
