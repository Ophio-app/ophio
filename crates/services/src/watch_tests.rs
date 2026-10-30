// SPDX-License-Identifier: Apache-2.0
//! No turn waits while it holds the project: unanswered requests go back to
//! the agent, quiet or waiting turns are told and then stopped, and an agent
//! asked to keep an eye on something asks to be woken instead.
use super::*;
use crate::{
    carry_on::{CARRY_ON_AFTER, GUIDANCE},
    followups::{FOLLOWUP, Followup, wake_key},
    watchdog::{QUIET_NUDGE, QUIET_STOP, WAIT_NUDGE},
};
use ports::{Clock, bridge::PersonCall};

/// Waits, on the wall clock, until `done` holds.
pub(super) async fn until(what: &str, mut done: impl FnMut() -> bool) {
    let mut more = waiting();
    while !done() {
        assert!(more(), "{what}");
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

pub(super) fn task(f: &Fixture, id: TaskId) -> Task {
    f.hub.read(|r| r.require("task", id)).unwrap()
}

pub(super) fn run(f: &Fixture, id: RunId) -> Run {
    f.hub.read(|r| r.require("run", id)).unwrap()
}

fn approval(f: &Fixture, id: ApprovalId) -> Approval {
    f.hub.read(|r| r.require("approval", id)).unwrap()
}

pub(super) fn texts(f: &Fixture) -> Vec<String> {
    f.harness.controls.texts.lock().unwrap().clone()
}

pub(super) async fn notes(f: &Fixture, task_id: TaskId) -> Vec<String> {
    let QueryData::TaskTimeline { items, .. } = f
        .hub
        .query(
            &Caller::local_owner(),
            Query::TaskTimeline {
                task_id,
                before_seq: None,
                limit: 200,
            },
        )
        .await
        .unwrap()
    else {
        panic!("timeline")
    };
    items
        .into_iter()
        .filter_map(|i| match i.item {
            TimelineBody::Note { text } => Some(text),
            _ => None,
        })
        .collect()
}

/// The agent's session has started: its turn is under way.
pub(super) async fn working(f: &Fixture, run_id: RunId) {
    f.harness
        .emit(
            run_id,
            ports::HarnessEvent::SessionStarted {
                native_session: "watched".into(),
                actual_model: None,
                actual_effort: None,
                account_label: None,
            },
        )
        .await;
    until("the turn is under way", || {
        run(f, run_id).state == RunState::Working
    })
    .await;
}

/// A project whose Next up runs.
pub(super) async fn running_project(f: &Fixture) -> ProjectId {
    let project = f.project().await;
    let resumed = f
        .hub
        .submit(
            &Caller::local_owner(),
            edit(
                Command::QueueResume {
                    project_id: project,
                },
                queue(f, project).revision,
            ),
        )
        .await;
    assert_eq!(resumed.status, ReceiptStatus::Accepted, "{resumed:?}");
    assert!(!queue(f, project).paused);
    project
}

/// A running Work task whose agent asked the person a question.
async fn asked(f: &Fixture) -> (ProjectId, TaskId, RunId, ApprovalId) {
    let owner = Caller::local_owner();
    let project = running_project(f).await;
    let (task_id, _) = f
        .task(project, TaskPlacement::StartNow, &owner, false)
        .await;
    let run_id = run_started(f, task_id).await;
    working(f, run_id).await;
    f.harness
        .emit(
            run_id,
            ports::HarnessEvent::ApprovalRequested(ports::NativeApproval {
                native_request_id: "question".into(),
                kind: ApprovalKind::Question,
                title: "The agent asked a question".into(),
                detail: Some("Which branch should the fix go on?".into()),
                detail_lines: Vec::new(),
                questions: Vec::new(),
                raw_request: None,
                scope: "This question".into(),
                shortened: false,
                tool_call: None,
            }),
        )
        .await;
    let find = || {
        f.hub
            .read(|r| r.list::<Approval>("approval"))
            .unwrap()
            .into_iter()
            .find(|a| a.native_request_id == "question")
    };
    until("the question was recorded", || find().is_some()).await;
    until("the task waits on the person", || {
        task(f, task_id).state == TaskState::AwaitingDecision
    })
    .await;
    (project, task_id, run_id, find().unwrap().approval_id)
}

async fn carried_on(f: &Fixture, run_id: RunId, approval_id: ApprovalId) {
    f.clock.advance(CARRY_ON_AFTER);
    f.hub.carry_on_unanswered().await.unwrap();
    assert!(
        f.hub
            .read(|r| crate::carry_on::carried_on(r, approval_id))
            .unwrap(),
        "the question went back to the agent"
    );
    assert_eq!(run(f, run_id).state, RunState::Working);
}

pub(super) async fn ended(f: &Fixture, run_id: RunId, exit: ports::RunExit) {
    f.harness
        .emit(
            run_id,
            ports::HarnessEvent::Exited {
                exit,
                outcome: None,
            },
        )
        .await;
    until("the run ended", || run(f, run_id).state.is_terminal()).await;
}

pub(super) fn queue(f: &Fixture, project: ProjectId) -> QueueView {
    f.hub.read(|r| r.require("queue", project)).unwrap()
}

#[tokio::test]
async fn resolved_approval_history_does_not_increase_guard_read_cost() {
    let f = Fixture::new().await;
    let (_, task_id, run_id, approval_id) = asked(&f).await;
    carried_on(&f, run_id, approval_id).await;
    assert_eq!(queue(&f, task(&f, task_id).project_id).writer, Some(run_id));

    async fn measure(f: &Fixture) -> (usize, usize) {
        store::take_read_cost();
        let start = std::time::Instant::now();
        assert!(f.hub.guard_once().await);
        let cost = store::take_read_cost();
        eprintln!("guard read cost: {cost:?}, elapsed: {:?}", start.elapsed());
        cost
    }
    let baseline = measure(&f).await;
    let mut finished = task(&f, task_id);
    finished.task_id = TaskId::new();
    finished.project_id = ProjectId::new();
    finished.state = TaskState::Completed;
    finished.current_run = None;
    let mut historical = approval(&f, approval_id);
    historical.project_id = finished.project_id;
    historical.task_id = finished.task_id;
    historical.run_id = RunId::new();
    historical.state = ApprovalState::Allowed;
    historical.resolved_at = Some(f.clock.now());
    historical.raw_request = Some("x".repeat(8 * 1024));
    f.hub
        .write(|tx| {
            tx.put("task", finished.task_id, &finished)?;
            for _ in 0..1000 {
                historical.approval_id = ApprovalId::new();
                tx.put("approval", historical.approval_id, &historical)?;
            }
            Ok(())
        })
        .unwrap();
    let with_history = measure(&f).await;
    f.close().await;
    assert_eq!(
        with_history, baseline,
        "resolved history must not be decoded"
    );
}

#[tokio::test]
async fn an_unanswered_question_goes_back_to_the_agent_and_a_late_answer_follows_it() {
    let f = Fixture::new().await;
    let (_, task_id, run_id, approval_id) = asked(&f).await;

    // Not yet: the person has ten minutes.
    f.clock.advance(CARRY_ON_AFTER - Duration::from_secs(1));
    assert_eq!(f.hub.carry_on_unanswered().await.unwrap(), 0);
    assert_eq!(task(&f, task_id).state, TaskState::AwaitingDecision);
    f.clock.advance(Duration::from_secs(1));
    f.hub.carry_on_unanswered().await.unwrap();

    // The agent is declined with word to carry on, and told so in its turn.
    assert_eq!(
        *f.harness.controls.decisions.lock().unwrap(),
        vec![ApprovalDecision::Deny {
            reason: Some(GUIDANCE.into())
        }]
    );
    assert_eq!(texts(&f), vec![GUIDANCE.to_string()]);
    assert_eq!(run(&f, run_id).state, RunState::Working);
    assert_eq!(task(&f, task_id).state, TaskState::Working);
    // The question stays open for the person.
    assert_eq!(approval(&f, approval_id).state, ApprovalState::Pending);
    assert!(
        notes(&f, task_id)
            .await
            .iter()
            .any(|n| n.starts_with("Nobody answered “The agent asked a question” in 10 minutes"))
    );
    // It goes back once.
    assert_eq!(f.hub.carry_on_unanswered().await.unwrap(), 0);

    // The answer reaches the agent in its turn, as a message.
    f.hub
        .resolve_approval(
            &Caller::local_owner(),
            approval_id,
            &ApprovalDecision::Answer {
                text: "main".into(),
                answers: Vec::new(),
            },
        )
        .await
        .unwrap();
    assert_eq!(approval(&f, approval_id).state, ApprovalState::Allowed);
    f.hub.pass_on_followups().await.unwrap();
    until("the answer was passed on", || texts(&f).len() == 2).await;
    assert_eq!(
        texts(&f)[1],
        "Answering your earlier question, “Which branch should the fix go on?”: main"
    );
    assert_eq!(f.harness.controls.resolutions.load(Ordering::SeqCst), 1);
    assert!(
        f.hub
            .read(|r| r.entries::<Followup>(FOLLOWUP))
            .unwrap()
            .is_empty()
    );
    f.close().await;
}

#[tokio::test]
async fn delayed_repeated_questions_keep_each_answers_identity_and_header() {
    let f = Fixture::new().await;
    let (_, _, run_id, approval_id) = asked(&f).await;
    f.hub
        .write(|tx| {
            let mut a: Approval = tx.read().require("approval", approval_id)?;
            a.title = "Answer 2 questions".into();
            a.questions = ["Production", "Staging"]
                .into_iter()
                .enumerate()
                .map(|(i, header)| ApprovalQuestion {
                    id: Some(format!("ophio-question-{i}")),
                    header: Some(header.into()),
                    question: "Deploy?".into(),
                    options: vec![],
                    multi_select: false,
                    secret: false,
                })
                .collect();
            tx.put("approval", approval_id, &a)
        })
        .unwrap();
    carried_on(&f, run_id, approval_id).await;
    let a = approval(&f, approval_id);
    let answers = a
        .questions
        .iter()
        .zip(["No", "Yes"])
        .rev()
        .map(|(q, choice)| protocol::command::QuestionAnswer {
            id: q.id.clone(),
            question: q.question.clone(),
            choices: vec![choice.into()],
        })
        .collect();
    let receipt = f
        .hub
        .submit(
            &Caller::local_owner(),
            edit(
                Command::ResolveApproval {
                    approval_id,
                    decision: ApprovalDecision::Answer {
                        text: "Production: No; Staging: Yes".into(),
                        answers,
                    },
                },
                a.revision.into(),
            ),
        )
        .await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    until("the late answer was recorded", || {
        approval(&f, approval_id).state == ApprovalState::Allowed
    })
    .await;
    f.hub.pass_on_followups().await.unwrap();
    until("the answer was passed on", || texts(&f).len() == 2).await;
    assert_eq!(
        texts(&f)[1],
        "Answering your earlier question, “Answer 2 questions”:\nQuestion 1 [ophio-question-0] (Production) Deploy?: No\nQuestion 2 [ophio-question-1] (Staging) Deploy?: Yes"
    );
    f.close().await;
}

#[tokio::test]
async fn a_late_answer_starts_the_ended_task_again() {
    let f = Fixture::new().await;
    let (project, task_id, run_id, approval_id) = asked(&f).await;
    carried_on(&f, run_id, approval_id).await;
    ended(&f, run_id, ports::RunExit::Completed).await;

    // The turn ended and freed the project; the question is still open.
    assert_eq!(task(&f, task_id).state, TaskState::Completed);
    assert!(queue(&f, project).writer.is_none());
    assert_eq!(approval(&f, approval_id).state, ApprovalState::Pending);
    let finished = f
        .hub
        .read(|r| r.list::<AttentionItem>("attention"))
        .unwrap()
        .into_iter()
        .find(|a| a.run_id == Some(run_id) && a.kind == AttentionKind::Completed)
        .unwrap();
    assert_eq!(
        finished.body.as_deref(),
        Some("Finished what it could. It still needs your answer.")
    );

    f.hub
        .resolve_approval(
            &Caller::local_owner(),
            approval_id,
            &ApprovalDecision::Allow,
        )
        .await
        .unwrap();
    f.hub.pass_on_followups().await.unwrap();
    until("the task started again", || {
        task(&f, task_id).current_run != Some(run_id)
    })
    .await;
    let next = run_started(&f, task_id).await;
    assert_ne!(next, run_id);
    let spec = f
        .harness
        .specs
        .lock()
        .unwrap()
        .last()
        .unwrap()
        .prompt
        .text
        .clone();
    assert!(
        spec.contains("About your earlier question, “Which branch should the fix go on?”: yes."),
        "{spec}"
    );
    f.close().await;
}

#[tokio::test]
async fn a_late_answer_waits_in_next_up_for_the_project_to_be_free() {
    let f = Fixture::new().await;
    let owner = Caller::local_owner();
    let (project, task_id, run_id, approval_id) = asked(&f).await;
    carried_on(&f, run_id, approval_id).await;
    ended(&f, run_id, ports::RunExit::Completed).await;
    // Another task holds the project meanwhile.
    let (other, _) = f
        .task(project, TaskPlacement::StartNow, &owner, false)
        .await;
    let other_run = run_started(&f, other).await;
    f.hub
        .resolve_approval(
            &owner,
            approval_id,
            &ApprovalDecision::Answer {
                text: "main".into(),
                answers: Vec::new(),
            },
        )
        .await
        .unwrap();
    // It waits in Next up, where the person sees it, rather than out of sight.
    assert_eq!(f.hub.pass_on_followups().await.unwrap(), 1);
    assert_eq!(task(&f, task_id).current_run, Some(run_id));
    assert!(
        f.hub
            .read(|r| r.entries::<Followup>(FOLLOWUP))
            .unwrap()
            .is_empty()
    );
    assert!(
        queue(&f, project)
            .items
            .iter()
            .any(|i| i.task_id == task_id && i.is_pending())
    );
    ended(&f, other_run, ports::RunExit::Completed).await;
    f.hub.pass_on_followups().await.unwrap();
    until("the task started again", || {
        task(&f, task_id).current_run != Some(run_id)
    })
    .await;
    f.close().await;
}

#[tokio::test]
async fn a_person_stop_withdraws_what_the_agent_went_on_without() {
    let f = Fixture::new().await;
    let (project, task_id, run_id, approval_id) = asked(&f).await;
    carried_on(&f, run_id, approval_id).await;
    let stop = f
        .hub
        .submit(
            &Caller::local_owner(),
            CommandEnvelope::new(Command::StopTask {
                task_id,
                force: false,
            }),
        )
        .await;
    assert_eq!(stop.status, ReceiptStatus::Accepted, "{stop:?}");
    assert_eq!(approval(&f, approval_id).state, ApprovalState::Cancelled);
    assert!(queue(&f, project).paused, "a person's Stop pauses Next up");
    f.close().await;
}

#[tokio::test]
async fn a_quiet_turn_is_reminded_then_stopped_and_next_up_goes_on() {
    let f = Fixture::new().await;
    let owner = Caller::local_owner();
    let project = running_project(&f).await;
    let (task_id, _) = f
        .task(project, TaskPlacement::StartNow, &owner, false)
        .await;
    let run_id = run_started(&f, task_id).await;
    working(&f, run_id).await;

    f.clock.advance(QUIET_NUDGE - Duration::from_secs(1));
    f.hub.guard_liveness().await.unwrap();
    assert!(texts(&f).is_empty());
    f.clock.advance(Duration::from_secs(1));
    f.hub.guard_liveness().await.unwrap();
    let told = texts(&f);
    assert_eq!(told.len(), 1);
    assert!(told[0].contains("call the wake_me tool"), "{}", told[0]);
    // Told once.
    f.hub.guard_liveness().await.unwrap();
    assert_eq!(texts(&f).len(), 1);

    // Work it shows puts the clock back.
    let before = task(&f, task_id).revision;
    f.harness
        .emit(
            run_id,
            ports::HarnessEvent::Activity {
                key: "read".into(),
                tool: "Read".into(),
                summary: "Read".into(),
                status: ActivityStatus::Succeeded,
                detail: None,
            },
        )
        .await;
    until("the guard saw it", || task(&f, task_id).revision > before).await;
    f.clock.advance(QUIET_STOP - QUIET_NUDGE);
    f.hub.guard_liveness().await.unwrap();
    assert_eq!(f.harness.controls.interrupts.load(Ordering::SeqCst), 0);

    // Thirty quiet minutes and it is stopped, with Next up left to go on.
    f.clock.advance(QUIET_STOP);
    f.hub.guard_liveness().await.unwrap();
    until("the guard stopped it", || {
        f.harness.controls.interrupts.load(Ordering::SeqCst) == 1
    })
    .await;
    assert!(!queue(&f, project).paused);
    let headline = f
        .hub
        .read(|r| r.list::<AttentionItem>("attention"))
        .unwrap()
        .into_iter()
        .find(|a| {
            a.task_id == Some(task_id)
                && a.resolved_at.is_none()
                && a.kind == AttentionKind::WaitingForYou
        })
        .unwrap();
    assert_eq!(
        headline
            .body
            .as_deref()
            .map(|b| b.starts_with("It showed no sign of work for 30 minutes")),
        Some(true)
    );
    ended(&f, run_id, ports::RunExit::Interrupted).await;
    assert!(queue(&f, project).writer.is_none());
    assert!(!queue(&f, project).paused);
    f.close().await;
}

#[tokio::test]
async fn a_turn_that_waits_is_told_to_ask_to_be_woken_and_one_waiting_on_a_person_is_left_alone() {
    let f = Fixture::new().await;
    let owner = Caller::local_owner();
    let project = running_project(&f).await;
    let (task_id, _) = f
        .task(project, TaskPlacement::StartNow, &owner, false)
        .await;
    let run_id = run_started(&f, task_id).await;
    working(&f, run_id).await;
    let before = task(&f, task_id).revision;
    f.harness
        .emit(
            run_id,
            ports::HarnessEvent::Activity {
                key: "watch".into(),
                tool: "Bash".into(),
                summary: "Bash".into(),
                status: ActivityStatus::Running,
                detail: Some("gh run watch 12345".into()),
            },
        )
        .await;
    until("the guard saw it", || task(&f, task_id).revision > before).await;
    f.clock.advance(WAIT_NUDGE);
    f.hub.guard_liveness().await.unwrap();
    let told = texts(&f);
    assert_eq!(told.len(), 1);
    assert!(
        told[0].starts_with("You've spent 15 minutes of this turn sleeping, watching or polling")
    );
    f.close().await;

    // A turn waiting on the person isn't stopped for it.
    let f = Fixture::new().await;
    let (_, task_id, run_id, _) = asked(&f).await;
    f.clock.advance(QUIET_STOP * 2);
    f.hub.guard_liveness().await.unwrap();
    assert_eq!(f.harness.controls.interrupts.load(Ordering::SeqCst), 0);
    assert_eq!(run(&f, run_id).state, RunState::AwaitingApproval);
    assert_eq!(task(&f, task_id).state, TaskState::AwaitingDecision);
    f.close().await;
}

/// The agent asks to be woken in 20 minutes for `reason`, and ends its turn.
async fn watching(f: &Fixture, reason: &str) -> (ProjectId, TaskId, RunId) {
    let owner = Caller::local_owner();
    let project = running_project(f).await;
    let (task_id, _) = f
        .task(project, TaskPlacement::StartNow, &owner, false)
        .await;
    let run_id = run_started(f, task_id).await;
    working(f, run_id).await;
    let said = f
        .hub
        .person_request(
            run_id,
            &task(f, task_id),
            PersonCall::Wake {
                minutes: 20,
                reason: reason.into(),
            },
        )
        .await
        .unwrap();
    assert!(
        said.starts_with("Ophio will wake you in 20 minutes. End your turn now"),
        "{said}"
    );
    let wake = task(f, task_id).wake.expect("watching");
    assert_eq!(wake.reason, reason);
    assert_eq!(
        wake.at.0 - ports::Clock::now(f.clock.as_ref()).0,
        20 * 60_000
    );
    ended(f, run_id, ports::RunExit::Completed).await;
    (project, task_id, run_id)
}

#[tokio::test]
async fn an_agent_asked_to_watch_ends_its_turn_and_is_woken_when_it_asked() {
    let f = Fixture::new().await;
    let (project, task_id, run_id) = watching(&f, "the checks on PR #12").await;
    // The project is free, and the person reads what it is watching. It
    // goes on by itself, so nothing is raised for them.
    assert!(queue(&f, project).writer.is_none());
    assert!(task(&f, task_id).wake.is_some());
    assert!(
        !f.hub
            .read(|r| r.list::<AttentionItem>("attention"))
            .unwrap()
            .iter()
            .any(|a| a.run_id == Some(run_id))
    );
    assert!(notes(&f, task_id).await.contains(
        &"Watching: the checks on PR #12. Ophio wakes the agent in 20 minutes.".to_string()
    ));

    // Not before its time.
    f.clock.advance(Duration::from_secs(19 * 60));
    assert_eq!(f.hub.pass_on_followups().await.unwrap(), 0);
    assert_eq!(task(&f, task_id).current_run, Some(run_id));
    f.clock.advance(Duration::from_secs(60));
    f.hub.pass_on_followups().await.unwrap();
    until("the agent was woken", || {
        task(&f, task_id).current_run != Some(run_id)
    })
    .await;
    run_started(&f, task_id).await;
    let prompt = f
        .harness
        .specs
        .lock()
        .unwrap()
        .last()
        .unwrap()
        .prompt
        .text
        .clone();
    assert!(
        prompt.starts_with("You asked Ophio to wake you: the checks on PR #12\nCheck on it now."),
        "{prompt}"
    );
    assert!(task(&f, task_id).wake.is_none());
    assert!(
        f.hub
            .read(|r| r.get::<Followup>(FOLLOWUP, &wake_key(task_id)))
            .unwrap()
            .is_none()
    );
    f.close().await;
}

#[tokio::test]
async fn stop_on_a_watching_task_stops_the_watching_and_starts_nothing() {
    let f = Fixture::new().await;
    let (project, task_id, run_id) = watching(&f, "the deploy").await;
    let stop = f
        .hub
        .submit(
            &Caller::local_owner(),
            CommandEnvelope::new(Command::StopTask {
                task_id,
                force: false,
            }),
        )
        .await;
    assert_eq!(stop.status, ReceiptStatus::Accepted, "{stop:?}");
    let stopped = task(&f, task_id);
    assert!(stopped.wake.is_none());
    assert_eq!(stopped.state, TaskState::Completed);
    assert!(!queue(&f, project).paused);
    assert!(
        notes(&f, task_id)
            .await
            .contains(&"Stopped watching. Ophio won't wake the agent.".to_string())
    );
    f.clock.advance(Duration::from_secs(21 * 60));
    assert_eq!(f.hub.pass_on_followups().await.unwrap(), 0);
    assert_eq!(task(&f, task_id).current_run, Some(run_id));
    f.close().await;
}

#[tokio::test]
async fn archiving_withdraws_the_wake_and_what_the_agent_went_on_without() {
    let f = Fixture::new().await;
    let (_, task_id, run_id, approval_id) = asked(&f).await;
    carried_on(&f, run_id, approval_id).await;
    f.hub
        .person_request(
            run_id,
            &task(&f, task_id),
            PersonCall::Wake {
                minutes: 30,
                reason: "the release".into(),
            },
        )
        .await
        .unwrap();
    ended(&f, run_id, ports::RunExit::Completed).await;
    // Watching, it goes on by itself: what it went on without was raised
    // as it was asked, and the turn's end raises nothing more.
    let raised = f
        .hub
        .read(|r| r.list::<AttentionItem>("attention"))
        .unwrap();
    assert!(
        !(raised.iter()).any(|a| a.run_id == Some(run_id) && a.kind == AttentionKind::Completed)
    );
    assert_eq!(approval(&f, approval_id).state, ApprovalState::Pending);
    let revision = task(&f, task_id).revision;
    let archive = f
        .hub
        .submit(
            &Caller::local_owner(),
            edit(Command::ArchiveTask { task_id }, revision),
        )
        .await;
    assert_eq!(archive.status, ReceiptStatus::Accepted, "{archive:?}");
    assert!(task(&f, task_id).wake.is_none());
    assert_eq!(approval(&f, approval_id).state, ApprovalState::Cancelled);
    assert!(
        f.hub
            .read(|r| r.entries::<Followup>(FOLLOWUP))
            .unwrap()
            .is_empty()
    );
    f.close().await;
}

#[tokio::test]
async fn a_guard_stop_keeps_the_wake_the_agent_asked_for() {
    let f = Fixture::new().await;
    let owner = Caller::local_owner();
    let project = running_project(&f).await;
    let (task_id, _) = f
        .task(project, TaskPlacement::StartNow, &owner, false)
        .await;
    let run_id = run_started(&f, task_id).await;
    working(&f, run_id).await;
    // It asked to be woken, then kept waiting anyway.
    f.hub
        .person_request(
            run_id,
            &task(&f, task_id),
            PersonCall::Wake {
                minutes: 60,
                reason: "CI".into(),
            },
        )
        .await
        .unwrap();
    f.clock.advance(QUIET_STOP);
    f.hub.guard_liveness().await.unwrap();
    until("the guard stopped it", || {
        f.harness.controls.interrupts.load(Ordering::SeqCst) == 1
    })
    .await;
    ended(&f, run_id, ports::RunExit::Interrupted).await;
    assert!(task(&f, task_id).wake.is_some());
    assert!(!queue(&f, project).paused);
    f.clock.advance(Duration::from_secs(30 * 60));
    f.hub.pass_on_followups().await.unwrap();
    until("the agent was woken", || {
        task(&f, task_id).current_run != Some(run_id)
    })
    .await;
    f.close().await;
}

#[test]
fn waits_reads_sleeps_and_watches() {
    use crate::watchdog::waits;
    for waiting in [
        "gh run watch 123",
        "gh pr checks 12 --watch",
        "sleep 60",
        "sleep 5m && gh pr checks",
        "while true; do gh pr checks 12; sleep 10; done",
        "until curl -sf localhost:3000; do sleep 2; done",
        "tail -f build.log",
        "kubectl logs --follow api",
    ] {
        assert!(waits(Some(waiting)), "{waiting}");
    }
    for working in [
        "cargo test",
        "sleep 1",
        "sleep 0.5 && ls",
        "grep -rn sleep src",
        "npm run build",
    ] {
        assert!(!waits(Some(working)), "{working}");
    }
    assert!(!waits(None));
}

#[test]
fn time_spent_waiting_on_the_person_is_not_quiet_time() {
    use crate::watchdog::{Liveness, Verdict, Why};
    let start = std::time::Instant::now();
    let at = |minutes: u64| start + Duration::from_secs(minutes * 60);
    let mut l = Liveness::new(at(0));
    // Quiet 5 minutes, then asked; answered an hour later.
    l.hold(at(5));
    l.release(at(65));
    assert_eq!(l.verdict(at(79), false), Verdict::Fine);
    assert_eq!(l.verdict(at(80), false), Verdict::Remind(Why::Quiet));

    // Work it showed while held counts from when it was seen.
    let mut l = Liveness::new(at(0));
    l.hold(at(5));
    l.saw(
        &ports::HarnessEvent::Activity {
            key: "edit".into(),
            tool: "Edit".into(),
            summary: "Edit".into(),
            status: ActivityStatus::Succeeded,
            detail: None,
        },
        at(60),
    );
    l.release(at(61));
    assert_eq!(l.verdict(at(79), false), Verdict::Fine);
    assert_eq!(l.verdict(at(80), false), Verdict::Remind(Why::Quiet));
    assert_eq!(l.verdict(at(90), false), Verdict::Stop(Why::Quiet));
    // Asked to stop, it's given time to end before it is forced.
    assert_eq!(l.verdict(at(90), true), Verdict::Fine);
}

async fn set_questions(f: &Fixture, questions: bool) {
    let revision = f.hub.agent_preferences_view().unwrap().revision;
    let mut command = CommandEnvelope::new(Command::SetAgentPreferences { questions });
    command.expected_revision = Some(revision);
    let receipt = f.hub.submit(&Caller::local_owner(), command).await;
    assert_eq!(receipt.error, None);
}

fn native_question(id: &str, questions: Vec<ApprovalQuestion>) -> ports::HarnessEvent {
    ports::HarnessEvent::ApprovalRequested(ports::NativeApproval {
        native_request_id: id.into(),
        kind: ApprovalKind::Question,
        title: "Which branch should the fix go on?".into(),
        detail: None,
        detail_lines: Vec::new(),
        questions,
        raw_request: None,
        scope: "This question".into(),
        shortened: false,
        tool_call: None,
    })
}

#[tokio::test]
async fn the_agent_checks_whether_it_may_ask_and_a_question_while_they_are_off_goes_back() {
    use ports::bridge::{BridgeBinding, BridgeHandler, BridgeRequest, BridgeResponse};
    use ports::harness::HOLD_QUESTIONS;
    let f = Fixture::new().await;
    let owner = Caller::local_owner();
    let project = running_project(&f).await;
    let (task_id, _) = f
        .task(project, TaskPlacement::StartNow, &owner, false)
        .await;
    let run_id = run_started(&f, task_id).await;
    working(&f, run_id).await;
    let asking = task(&f, task_id);
    let can_ask = || f.hub.person_request(run_id, &asking, PersonCall::CanAsk);
    assert!(can_ask().await.unwrap().starts_with("Questions are on"));

    // The person turns questions off while the agent works.
    set_questions(&f, false).await;
    assert_eq!(can_ask().await.unwrap(), HOLD_QUESTIONS);

    // Asked through Claude Code's prompt, it is answered at once.
    let binding = BridgeBinding {
        run_id,
        target_id: None,
        computer_use: false,
        browser: false,
        github: false,
        checklist_answers: false,
    };
    let asked = f
        .hub
        .handle(
            &binding,
            BridgeRequest::PermissionPrompt {
                tool_name: "AskUserQuestion".into(),
                input: serde_json::json!({"questions":[{
                    "question":"Which branch should the fix go on?",
                    "header":"Branch",
                    "options":[{"label":"main","description":"The default"},{"label":"dev","description":"Next release"}],
                    "multiSelect":false
                }]}),
                tool_use_id: Some("toolu_ask".into()),
                native_request_id: Some("ask".into()),
            },
        )
        .await;
    let BridgeResponse::Permission { decision } = asked else {
        panic!("{asked:?}")
    };
    assert_eq!(
        decision,
        serde_json::json!({"behavior":"deny","message":HOLD_QUESTIONS})
    );

    // Asked natively, it is declined, and the agent told why in its turn.
    let options = vec![
        ApprovalQuestionOption {
            label: "main".into(),
            description: None,
        },
        ApprovalQuestionOption {
            label: "dev".into(),
            description: None,
        },
    ];
    f.harness
        .emit(
            run_id,
            native_question(
                "native",
                vec![ApprovalQuestion {
                    id: None,
                    header: None,
                    question: "Which branch should the fix go on?".into(),
                    options,
                    multi_select: false,
                    secret: false,
                }],
            ),
        )
        .await;
    until("the agent was told to hold it", || texts(&f).len() == 1).await;
    assert_eq!(texts(&f), vec![HOLD_QUESTIONS.to_string()]);
    assert_eq!(
        *f.harness.controls.decisions.lock().unwrap(),
        vec![ApprovalDecision::Deny {
            reason: Some(HOLD_QUESTIONS.into())
        }]
    );
    // Neither asked the person: nothing to answer, and the turn goes on.
    assert!(
        f.hub
            .read(|r| r.list::<Approval>("approval"))
            .unwrap()
            .is_empty()
    );
    assert_eq!(task(&f, task_id).state, TaskState::Working);
    let held = "Questions are off, so Ophio gave the agent its question back, to answer itself or keep for the end of its turn: “Which branch should the fix go on?”";
    assert_eq!(
        notes(&f, task_id)
            .await
            .iter()
            .filter(|n| n.as_str() == held)
            .count(),
        2
    );

    // A tool's own request for input is still the person's to answer.
    f.harness
        .emit(run_id, native_question("elicitation", Vec::new()))
        .await;
    until("the tool's request was recorded", || {
        task(&f, task_id).state == TaskState::AwaitingDecision
    })
    .await;

    // Turned back on, the agent may ask again.
    set_questions(&f, true).await;
    assert!(can_ask().await.unwrap().starts_with("Questions are on"));
    f.close().await;
}
