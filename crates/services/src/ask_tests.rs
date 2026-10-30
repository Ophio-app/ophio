// SPDX-License-Identifier: Apache-2.0
//! One agent asks another, read-only, and the answer comes back to it as
//! its next message without the person passing it on.
use super::watch_tests::{ended, notes, queue, running_project, task, texts, until, working};
use super::*;
use crate::followups::{FOLLOWUP, Followup};
use ports::bridge::{AgentChoice, WorkCall};
use protocol::capability::HarnessInfo;

/// The fixture's agent answers in Ask mode, read-only, as well as Work.
fn asks(f: &Fixture) {
    f.hub
        .write(|tx| {
            let mut infos: Vec<HarnessInfo> = tx.read().require("meta", "harnesses")?;
            infos[0].modes = [TaskMode::Ask, TaskMode::Plan, TaskMode::Work]
                .into_iter()
                .map(|mode| protocol::capability::ModeSupport {
                    mode,
                    availability: protocol::capability::Availability::Available,
                    enforcement: "Synthetic fixture restriction".into(),
                })
                .collect();
            tx.put("meta", "harnesses", &infos)
        })
        .unwrap();
}

fn agent(agent: HarnessKind) -> AgentChoice {
    AgentChoice {
        agent,
        model: None,
        effort: None,
    }
}

fn ask(title: &str) -> WorkCall {
    WorkCall::Ask {
        agent: agent(HarnessKind::ClaudeCode),
        prompt: "Review the change on this branch.".into(),
        title: Some(title.into()),
    }
}

/// A Work task in the middle of its turn.
async fn asking(f: &Fixture) -> (ProjectId, TaskId, RunId) {
    asks(f);
    let project = running_project(f).await;
    let (task_id, _) = f
        .task(
            project,
            TaskPlacement::StartNow,
            &Caller::local_owner(),
            false,
        )
        .await;
    let run_id = run_started(f, task_id).await;
    working(f, run_id).await;
    (project, task_id, run_id)
}

/// The tasks `caller` asked.
fn helpers(f: &Fixture, caller: TaskId) -> Vec<Task> {
    (f.hub.read(|r| r.list::<Task>("task")).unwrap().into_iter())
        .filter(|t| t.asked_by == Some(caller))
        .collect()
}

/// `caller` asks, and the task asked has started.
async fn asked(f: &Fixture, caller: TaskId, run_id: RunId, title: &str) -> (TaskId, RunId) {
    let said = f
        .hub
        .work_request(run_id, &task(f, caller), ask(title))
        .await
        .unwrap();
    assert!(
        said.starts_with(&format!("Asked Claude Code in “{title}”."))
            && said.contains("Don't wait, sleep or poll for it."),
        "{said}"
    );
    let helper = (helpers(f, caller).into_iter())
        .find(|t| t.title == title)
        .expect("the task asked");
    let helper_run = run_started(f, helper.task_id).await;
    working(f, helper_run).await;
    (helper.task_id, helper_run)
}

/// The agent asked says `answer` and ends its turn.
async fn answers(f: &Fixture, run_id: RunId, answer: &str) {
    f.harness
        .emit(
            run_id,
            ports::HarnessEvent::AgentMessage {
                message_key: "answer".into(),
                text: answer.into(),
            },
        )
        .await;
    ended(f, run_id, ports::RunExit::Completed).await;
}

fn raised(f: &Fixture, run_id: RunId) -> bool {
    (f.hub
        .read(|r| r.list::<AttentionItem>("attention"))
        .unwrap()
        .iter())
    .any(|a| a.run_id == Some(run_id))
}

fn last_prompt(f: &Fixture) -> String {
    let specs = f.harness.specs.lock().unwrap();
    specs.last().unwrap().prompt.text.clone()
}

#[tokio::test]
async fn an_agent_asks_another_and_the_answer_starts_its_next_turn() {
    let f = Fixture::new().await;
    let (project, caller, run_id) = asking(&f).await;
    let (helper, helper_run) = asked(&f, caller, run_id, "Review my change").await;

    // The agent asked works read-only beside the one holding the project,
    // told who asks and that its last message is the answer.
    let spec = f.harness.specs.lock().unwrap().last().cloned().unwrap();
    assert_eq!(spec.mode, TaskMode::Ask);
    assert_eq!(spec.launch_policy, None);
    assert!(
        spec.prompt
            .text
            .contains("Review the change on this branch.")
            && spec.prompt.text.contains("passes your last message back")
            && spec.prompt.text.contains("You're read-only here"),
        "{}",
        spec.prompt.text
    );
    assert_eq!(task(&f, helper).mode, TaskMode::Ask);
    assert!(queue(&f, project).writer.is_some());

    // The one asking waits for the answer, and the person reads that.
    let wake = task(&f, caller).wake.expect("waiting for the answer");
    assert!(wake.answer);
    assert_eq!(wake.reason, "Claude Code’s answer to “Review my change”");
    assert!(
        notes(&f, caller)
            .await
            .contains(&"Asked Claude Code: “Review my change”. Its answer comes back here.".into())
    );
    ended(&f, run_id, ports::RunExit::Completed).await;
    assert!(!raised(&f, run_id));
    assert!(queue(&f, project).writer.is_none());

    // The answer is the asking agent's to read, not the person's.
    answers(
        &f,
        helper_run,
        "Looks right. One nit: the retry loop never backs off.",
    )
    .await;
    assert!(!raised(&f, helper_run));
    f.hub.pass_on_followups().await.unwrap();
    until("the asking agent was started again", || {
        task(&f, caller).current_run != Some(run_id)
    })
    .await;
    run_started(&f, caller).await;
    assert_eq!(
        last_prompt(&f),
        "Claude Code answered what you asked in “Review my change”:\n\nLooks right. One nit: the retry loop never backs off."
    );
    assert!(task(&f, caller).wake.is_none());
    assert!(
        f.hub
            .read(|r| r.entries::<Followup>(FOLLOWUP))
            .unwrap()
            .is_empty()
    );
    f.close().await;
}

#[tokio::test]
async fn an_answer_reaches_an_agent_still_in_its_turn() {
    let f = Fixture::new().await;
    let (_, caller, run_id) = asking(&f).await;
    let (_, helper_run) = asked(&f, caller, run_id, "Check the schema").await;
    answers(&f, helper_run, "The schema is fine.").await;
    f.hub.pass_on_followups().await.unwrap();
    assert!(
        texts(&f).contains(
            &"Claude Code answered what you asked in “Check the schema”:\n\nThe schema is fine."
                .to_string()
        ),
        "{:?}",
        texts(&f)
    );
    assert_eq!(task(&f, caller).current_run, Some(run_id));
    assert!(task(&f, caller).wake.is_none());
    f.close().await;
}

#[tokio::test]
async fn the_asking_agent_waits_for_every_answer_and_hears_of_one_that_failed() {
    let f = Fixture::new().await;
    let (_, caller, run_id) = asking(&f).await;
    let (_, first) = asked(&f, caller, run_id, "Review the API").await;
    let (_, second) = asked(&f, caller, run_id, "Review the tests").await;
    ended(
        &f,
        first,
        ports::RunExit::Failed {
            reason: "crashed".into(),
        },
    )
    .await;
    assert!(!raised(&f, first));
    f.hub.pass_on_followups().await.unwrap();
    assert!(texts(&f).contains(
        &"Claude Code couldn't answer what you asked in “Review the API”: it failed. Carry on without it, or ask again."
            .to_string()
    ));
    // Still waiting on the other.
    assert!(task(&f, caller).wake.is_some_and(|w| w.answer));
    answers(&f, second, "Tests cover it.").await;
    f.hub.pass_on_followups().await.unwrap();
    assert!(task(&f, caller).wake.is_none());
    f.close().await;
}

#[tokio::test]
async fn with_no_answer_in_a_day_the_asking_agent_is_woken_anyway() {
    let f = Fixture::new().await;
    let (_, caller, run_id) = asking(&f).await;
    let (_, helper_run) = asked(&f, caller, run_id, "Review my change").await;
    ended(&f, run_id, ports::RunExit::Completed).await;

    f.clock.advance(Duration::from_secs(24 * 3600 - 60));
    assert_eq!(f.hub.pass_on_followups().await.unwrap(), 0);
    f.clock.advance(Duration::from_secs(60));
    f.hub.pass_on_followups().await.unwrap();
    until("the asking agent was woken", || {
        task(&f, caller).current_run != Some(run_id)
    })
    .await;
    run_started(&f, caller).await;
    assert!(
        last_prompt(&f).starts_with(
            "At least a day ago you asked Claude Code “Review my change”, and Ophio has had no answer."
        ),
        "{}",
        last_prompt(&f)
    );
    assert!(task(&f, caller).wake.is_none());
    // Still running when that agent answers, it gets the answer in its turn.
    working(&f, task(&f, caller).current_run.unwrap()).await;
    answers(&f, helper_run, "Late, but fine.").await;
    f.hub.pass_on_followups().await.unwrap();
    assert!(
        texts(&f)
            .iter()
            .any(|t| t.ends_with("“Review my change”:\n\nLate, but fine.")),
        "{:?}",
        texts(&f)
    );
    f.close().await;
}

#[tokio::test]
async fn the_person_stopping_the_asking_task_takes_the_answer_themselves() {
    let f = Fixture::new().await;
    let (_, caller, run_id) = asking(&f).await;
    let (helper, helper_run) = asked(&f, caller, run_id, "Review my change").await;
    let stop = f
        .hub
        .submit(
            &Caller::local_owner(),
            CommandEnvelope::new(Command::StopTask {
                task_id: caller,
                force: false,
            }),
        )
        .await;
    assert_eq!(stop.status, ReceiptStatus::Accepted, "{stop:?}");
    ended(&f, run_id, ports::RunExit::Interrupted).await;
    answers(&f, helper_run, "Looks right.").await;
    // Nobody is waiting for it: it is the person's, as any finished task.
    assert!(raised(&f, helper_run));
    assert!(
        f.hub
            .read(|r| r.entries::<Followup>(FOLLOWUP))
            .unwrap()
            .is_empty()
    );
    assert_eq!(task(&f, helper).asked_by, Some(caller));
    f.close().await;
}

#[tokio::test]
async fn stop_waiting_leaves_the_answer_in_the_task_asked() {
    let f = Fixture::new().await;
    let (_, caller, run_id) = asking(&f).await;
    let (_, helper_run) = asked(&f, caller, run_id, "Review my change").await;
    ended(&f, run_id, ports::RunExit::Completed).await;
    let stop = f
        .hub
        .submit(
            &Caller::local_owner(),
            CommandEnvelope::new(Command::StopTask {
                task_id: caller,
                force: false,
            }),
        )
        .await;
    assert_eq!(stop.status, ReceiptStatus::Accepted, "{stop:?}");
    assert!(task(&f, caller).wake.is_none());
    assert!(notes(&f, caller).await.contains(
        &"Stopped waiting. The other agent's answer stays in its own task, and Ophio won't wake the agent.".into()
    ));
    answers(&f, helper_run, "Looks right.").await;
    assert!(raised(&f, helper_run));
    assert_eq!(f.hub.pass_on_followups().await.unwrap(), 0);
    assert_eq!(task(&f, caller).current_run, Some(run_id));
    f.close().await;
}

#[tokio::test]
async fn stop_waiting_cancels_an_answer_already_queued() {
    let f = Fixture::new().await;
    let guard = f.hub.inner.guarding.lock().await;
    let (_, caller, run_id) = asking(&f).await;
    let (helper, helper_run) = asked(&f, caller, run_id, "Review my change").await;
    ended(&f, run_id, ports::RunExit::Completed).await;
    answers(&f, helper_run, "Looks right.").await;
    let stop = f
        .hub
        .submit(
            &Caller::local_owner(),
            CommandEnvelope::new(Command::StopTask {
                task_id: caller,
                force: false,
            }),
        )
        .await;
    assert_eq!(stop.status, ReceiptStatus::Accepted, "{stop:?}");
    f.hub.pass_on_followups().await.unwrap();
    assert_eq!(task(&f, caller).current_run, Some(run_id));
    assert!(task(&f, caller).wake.is_none());
    let (timeline, _) = f.hub.read(|r| r.timeline(helper, None, 100)).unwrap();
    assert!(timeline.iter().any(|item| matches!(&item.item,
        TimelineBody::AgentMessage { text, .. } if text == "Looks right.")));
    drop(guard);
    f.close().await;
}

// Hold delivery to another live task after the worker has copied its batch.
// Stop must invalidate both the copied timeout wake and the copied reply.
#[tokio::test]
async fn stop_waiting_invalidates_a_delivery_batch_already_read() {
    let f = Fixture::new().await;
    let guard = f.hub.inner.guarding.lock().await;
    let (_, caller, run_id) = asking(&f).await;
    let (blocker, _) = asked(&f, caller, run_id, "A separate question").await;
    let first = Followup::answer(
        blocker,
        ApprovalId::new(),
        "Unrelated answer".into(),
        f.hub.inner.clock.now(),
    );
    f.hub.write(|tx| first.put(tx)).unwrap();
    f.clock.advance(Duration::from_secs(1));
    let (_, helper_run) = asked(&f, caller, run_id, "Review my change").await;
    ended(&f, run_id, ports::RunExit::Completed).await;
    answers(&f, helper_run, "Looks right.").await;
    f.clock.advance(Duration::from_secs(24 * 3600 - 1));
    // The caller's wake becomes due without expiring the first delivery.
    f.hub
        .write(|tx| {
            let mut wake: Followup = tx
                .read()
                .require(FOLLOWUP, crate::followups::wake_key(caller))?;
            wake.due_at = f.hub.inner.clock.now();
            wake.put(tx)
        })
        .unwrap();
    let held = f.hub.inner.runs.lock().await;
    let hub = f.hub.clone();
    let delivery = hub.pass_on_followups();
    tokio::pin!(delivery);
    tokio::select! {
        biased;
        result = &mut delivery => panic!("delivery must be blocked: {result:?}"),
        _ = std::future::ready(()) => {}
    }
    let stop = f
        .hub
        .submit(
            &Caller::local_owner(),
            CommandEnvelope::new(Command::StopTask {
                task_id: caller,
                force: false,
            }),
        )
        .await;
    assert_eq!(stop.status, ReceiptStatus::Accepted, "{stop:?}");
    drop(held);
    assert_eq!(delivery.await.unwrap(), 1);
    assert_eq!(texts(&f), vec!["Unrelated answer"]);
    assert_eq!(task(&f, caller).current_run, Some(run_id));
    assert!(
        f.hub
            .read(|r| r.entries::<Followup>(FOLLOWUP))
            .unwrap()
            .is_empty()
    );
    drop(guard);
    f.close().await;
}

#[tokio::test]
async fn cancelled_reply_cannot_be_admitted_after_a_worker_checked_it() {
    let f = Fixture::new().await;
    let guard = f.hub.inner.guarding.lock().await;
    let (_, caller, run_id) = asking(&f).await;
    let (_, helper_run) = asked(&f, caller, run_id, "Review my change").await;
    ended(&f, run_id, ports::RunExit::Completed).await;
    answers(&f, helper_run, "Looks right.").await;
    let reply = f
        .hub
        .read(|r| r.entries::<Followup>(FOLLOWUP))
        .unwrap()
        .into_iter()
        .map(|(_, f)| f)
        .find(|f| f.kind == crate::followups::FollowupKind::Reply)
        .unwrap();
    assert!(f.hub.read(|r| reply.is_current(r)).unwrap());
    let stop = f
        .hub
        .submit(
            &Caller::local_owner(),
            CommandEnvelope::new(Command::StopTask {
                task_id: caller,
                force: false,
            }),
        )
        .await;
    assert_eq!(stop.status, ReceiptStatus::Accepted);
    for command in [
        Command::ResumeTask {
            task_id: caller,
            text: Some(reply.text.clone()),
            bundle: None,
            mode: None,
            carried_plan: None,
            read_only: false,
            model: None,
            effort: None,
        },
        Command::QueueContinuation {
            task_id: caller,
            text: reply.text.clone(),
            bundle: None,
            authorization: protocol::queue::QueueAuthorization::AutoStart,
        },
    ] {
        let receipt = f
            .hub
            .admit_with_followup(
                &Caller::local_owner(),
                CommandEnvelope::new(command),
                f.hub.inner.clock.now(),
                None,
                Some(&reply),
            )
            .await;
        assert_eq!(receipt.status, ReceiptStatus::Rejected, "{receipt:?}");
        assert_eq!(receipt.error.unwrap().code, ErrorCode::Expired);
    }
    assert_eq!(task(&f, caller).current_run, Some(run_id));
    drop(guard);
    f.close().await;
}

#[tokio::test]
async fn stop_cancels_a_reply_waiting_to_reach_the_running_agent() {
    let f = Fixture::new().await;
    let guard = f.hub.inner.guarding.lock().await;
    let (_, caller, run_id) = asking(&f).await;
    let (_, helper_run) = asked(&f, caller, run_id, "Review my change").await;
    answers(&f, helper_run, "Looks right.").await;
    let held = f.hub.inner.runs.lock().await;
    let hub = f.hub.clone();
    let delivery = hub.pass_on_followups();
    tokio::pin!(delivery);
    tokio::select! {
        biased;
        result = &mut delivery => panic!("delivery must be blocked: {result:?}"),
        _ = std::future::ready(()) => {}
    }
    let stop = f
        .hub
        .submit(
            &Caller::local_owner(),
            CommandEnvelope::new(Command::StopTask {
                task_id: caller,
                force: false,
            }),
        )
        .await;
    assert_eq!(stop.status, ReceiptStatus::Accepted);
    drop(held);
    assert_eq!(delivery.await.unwrap(), 0);
    assert!(texts(&f).is_empty());
    assert_eq!(task(&f, caller).current_run, Some(run_id));
    assert!(
        f.hub
            .read(|r| r.entries::<Followup>(FOLLOWUP))
            .unwrap()
            .is_empty()
    );
    drop(guard);
    f.close().await;
}

#[tokio::test]
async fn an_agent_asks_only_agents_that_are_here_and_ten_at_most_in_a_run() {
    let f = Fixture::new().await;
    let (_, caller, run_id) = asking(&f).await;
    let codex = WorkCall::Ask {
        agent: agent(HarnessKind::Codex),
        prompt: "Review it.".into(),
        title: None,
    };
    let e = f
        .hub
        .work_request(run_id, &task(&f, caller), codex)
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Unsupported, "{e:?}");
    let empty = WorkCall::Ask {
        agent: agent(HarnessKind::ClaudeCode),
        prompt: "Review it.".into(),
        title: None,
    };
    // An agent that can't answer read-only isn't started, and the
    // question doesn't count.
    f.hub
        .write(|tx| {
            let mut infos: Vec<HarnessInfo> = tx.read().require("meta", "harnesses")?;
            infos[0].modes.retain(|m| m.mode != TaskMode::Ask);
            tx.put("meta", "harnesses", &infos)
        })
        .unwrap();
    let refused = f
        .hub
        .work_request(run_id, &task(&f, caller), empty.clone())
        .await;
    assert!(refused.is_err(), "{refused:?}");
    assert!(helpers(&f, caller).is_empty());
    asks(&f);
    for _ in 0..10 {
        f.hub
            .work_request(run_id, &task(&f, caller), empty.clone())
            .await
            .unwrap();
    }
    let e = f
        .hub
        .work_request(run_id, &task(&f, caller), empty)
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Conflict, "{e:?}");
    let asked = helpers(&f, caller);
    assert_eq!(asked.len(), 10);
    assert!(asked.iter().all(|t| t.title == "Question from “Test task”"));
    f.close().await;
}
