// SPDX-License-Identifier: Apache-2.0
//! A request the person can still answer outlives the agent's wait for it.
//! Codex's tool runner may give up on a call that waits on the person, end
//! its turn and close Ophio's bridge; the request stays open as one given
//! back after ten minutes does, and a later answer is the agent's next
//! message. A person's Stop, or archiving, still withdraws it.
use super::conversation_access::Backend;
use super::more_access_tests::{
    all_modes, approval_where, bound, create, decide, next_spec, started,
};
use super::watch_tests::{ended, notes, run, task, until};
use super::*;
use crate::carry_on::{CARRY_ON_AFTER, GUIDANCE};
use ports::bridge::{BridgeBinding, BridgeHandler, BridgeRequest, BridgeResponse};
use protocol::access::ConversationAccess;

fn approval(f: &Fixture, id: ApprovalId) -> Approval {
    f.hub.read(|r| r.require("approval", id)).unwrap()
}

fn carried_on(f: &Fixture, id: ApprovalId) -> bool {
    f.hub.read(|r| crate::carry_on::carried_on(r, id)).unwrap()
}

/// What `task_id` may do once more without a card.
fn passes(f: &Fixture, task_id: TaskId) -> Vec<crate::switch_work::Preapproved> {
    (f.hub
        .read(|r| crate::switch_work::passes(r, task_id))
        .unwrap()
        .into_iter())
    .map(|(_, pass)| pass)
    .collect()
}

/// Whether the phone still shows `id` as needing the person.
fn still_raised(f: &Fixture, id: ApprovalId) -> bool {
    f.hub
        .read(|r| r.list::<AttentionItem>("attention"))
        .unwrap()
        .iter()
        .any(|a| a.approval_id == Some(id) && a.resolved_at.is_none())
}

/// A chat held to reading its project, as a Codex chat starts.
async fn chat(f: &Fixture) -> (ProjectId, TaskId, ports::RunSpec) {
    held(f, true, |_| {}).await
}

/// A chat (or, with `conversation` false, a task carried on from one) held
/// to reading its project and the internet, as `more` changes that.
async fn held(
    f: &Fixture,
    conversation: bool,
    more: impl FnOnce(&mut ConversationAccess),
) -> (ProjectId, TaskId, ports::RunSpec) {
    all_modes(f);
    let project = f.project().await;
    let mut access = ConversationAccess {
        read: vec![project],
        ..Default::default()
    };
    more(&mut access);
    let mode = match conversation {
        true => TaskMode::Ask,
        false => TaskMode::Work,
    };
    let (task_id, spec) = started(f, create(project, mode, Some(access))).await;
    if !conversation {
        f.hub
            .write(|tx| {
                let mut t: Task = tx.read().require("task", task_id)?;
                t.conversation = false;
                tx.put("task", task_id, &t)
            })
            .unwrap();
    }
    (project, task_id, spec)
}

/// Changes `task_id`'s access behind the card's back, as the access bar
/// does.
fn access_now(f: &Fixture, task_id: TaskId, change: impl Fn(&mut ConversationAccess)) {
    f.hub
        .write(|tx| {
            let mut t: Task = tx.read().require("task", task_id)?;
            change(t.access.as_mut().expect("held"));
            tx.put("task", task_id, &t)
        })
        .unwrap();
}

/// A late Allow refused because the access changed: nothing changes, the
/// card is withdrawn and the task's notes say why.
async fn refused_as_changed(f: &Fixture, task_id: TaskId, id: ApprovalId) {
    let before = task(f, task_id).access;
    let receipt = decide(f, &approval(f, id), ApprovalDecision::Allow).await;
    assert_eq!(receipt.status, ReceiptStatus::Rejected, "{receipt:?}");
    let refused = receipt.error.unwrap();
    assert_eq!(refused.code, ErrorCode::Conflict);
    assert_eq!(refused.message, crate::more_access::ACCESS_CHANGED);
    assert_eq!(task(f, task_id).access, before, "nothing was turned on");
    let gone = approval(f, id);
    assert_eq!(gone.state, ApprovalState::Cancelled);
    assert!(!carried_on(f, id));
    assert!(!still_raised(f, id), "the stale card was withdrawn");
    assert!(
        (notes(f, task_id).await.iter()).any(|n| n.starts_with("The access changed after")),
        "{:?}",
        notes(f, task_id).await
    );
}

/// The agent asks for `needs` through the bridge, and its tool call stops
/// waiting: the connection closes, as Codex's does once its turn ends.
async fn asked_then_let_go(
    f: &Fixture,
    spec: &ports::RunSpec,
    needs: Vec<AccessNeed>,
) -> (BridgeBinding, Approval) {
    let binding = bound(f, spec).await;
    let hub = f.hub.clone();
    let asking_as = binding.clone();
    let asking = tokio::spawn(async move {
        hub.handle(
            &asking_as,
            BridgeRequest::RequestAccess {
                needs,
                reason: "to fix the build".into(),
            },
        )
        .await
    });
    let a = approval_where(f, |a| a.access_request.is_some()).await;
    asking.abort();
    let _ = asking.await;
    BridgeHandler::unbound(&f.hub, &binding).await;
    (binding, a)
}

/// (a) The agent's tool call stops waiting on a chat's request for access
/// and its connection to Ophio closes; then its turn ends. The request
/// stays open and the phone still shows it; a later Allow turns on exactly
/// what the card said and starts the next turn with the decision.
#[tokio::test]
async fn an_access_request_outlives_its_closed_connection_and_a_late_allow_applies_it() {
    let backend = Arc::new(Backend::default());
    let f = Fixture::with(|b| b.sandboxes(backend.clone())).await;
    let (project, task_id, spec) = chat(&f).await;
    let (_, a) = asked_then_let_go(&f, &spec, vec![AccessNeed::ChangeFiles]).await;
    let open = approval(&f, a.approval_id);
    assert_eq!(open.state, ApprovalState::Pending);
    assert!(carried_on(&f, a.approval_id), "it went back to the agent");
    assert!(still_raised(&f, a.approval_id), "the phone still shows it");
    assert!(
        f.hub.inner.waits.lock().await.is_empty(),
        "nothing waits on it any more"
    );
    assert!(
        (notes(&f, task_id).await.iter())
            .any(|n| n.starts_with("The agent stopped waiting before anyone answered")),
        "{:?}",
        notes(&f, task_id).await
    );

    ended(&f, spec.run_id, ports::RunExit::Completed).await;
    assert_eq!(
        approval(&f, a.approval_id).state,
        ApprovalState::Pending,
        "the turn's end leaves it open"
    );
    assert!(still_raised(&f, a.approval_id));
    let finished = f
        .hub
        .read(|r| r.list::<AttentionItem>("attention"))
        .unwrap()
        .into_iter()
        .find(|i| i.run_id == Some(spec.run_id) && i.kind == AttentionKind::Completed)
        .expect("the turn's end was told");
    assert_eq!(
        finished.body.as_deref(),
        Some("Finished what it could. It still needs your answer.")
    );

    let receipt = decide(&f, &approval(&f, a.approval_id), ApprovalDecision::Allow).await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    until("the Allow was recorded", || {
        approval(&f, a.approval_id).state == ApprovalState::Allowed
    })
    .await;
    assert!(!still_raised(&f, a.approval_id));
    // Exactly what the card showed, and nothing else.
    let access = task(&f, task_id).access.unwrap();
    assert_eq!(
        access,
        ConversationAccess {
            read: vec![project],
            write: vec![project],
            ..Default::default()
        }
        .normalized()
    );
    f.hub.pass_on_followups().await.unwrap();
    let next = next_spec(&f, task_id, spec.run_id).await;
    assert!(
        next.prompt.text.contains("About your earlier request, “")
            && next.prompt.text.contains("”: yes, it's allowed now."),
        "{}",
        next.prompt.text
    );
    f.close().await;
}

/// (b) The same for a question asked through the bridge: Claude Code's
/// AskUserQuestion, whose connection closed before anyone answered.
#[tokio::test]
async fn a_bridge_question_outlives_the_turn_and_a_late_answer_starts_it_again() {
    let f = Fixture::new().await;
    let owner = Caller::local_owner();
    let project = super::watch_tests::running_project(&f).await;
    let (task_id, _) = f
        .task(project, TaskPlacement::StartNow, &owner, false)
        .await;
    let run_id = run_started(&f, task_id).await;
    super::watch_tests::working(&f, run_id).await;
    let binding = BridgeBinding {
        checks: false,
        run_id,
        target_id: None,
        computer_use: false,
        browser: false,
        github: false,
        checklist_answers: false,
        read_only: false,
    };
    let hub = f.hub.clone();
    let asking_as = binding.clone();
    let asking = tokio::spawn(async move {
        hub.handle(
            &asking_as,
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
        .await
    });
    let a = approval_where(&f, |a| a.kind == ApprovalKind::Question).await;
    asking.abort();
    let _ = asking.await;
    BridgeHandler::unbound(&f.hub, &binding).await;
    assert_eq!(approval(&f, a.approval_id).state, ApprovalState::Pending);
    assert!(carried_on(&f, a.approval_id));
    assert!(still_raised(&f, a.approval_id));
    // Its tool call is gone, so the turn is working again, not waiting.
    assert_eq!(run(&f, run_id).state, RunState::Working);

    ended(&f, run_id, ports::RunExit::Completed).await;
    assert_eq!(approval(&f, a.approval_id).state, ApprovalState::Pending);
    f.hub
        .resolve_approval(
            &owner,
            a.approval_id,
            &ApprovalDecision::Answer {
                text: "main".into(),
                answers: Vec::new(),
            },
        )
        .await
        .unwrap();
    f.hub.pass_on_followups().await.unwrap();
    until("the task started again", || {
        task(&f, task_id).current_run != Some(run_id)
    })
    .await;
    let next = next_spec(&f, task_id, run_id).await;
    assert!(
        next.prompt.text.contains(
            "Answering your earlier question, “Which branch should the fix go on?”: main"
        ),
        "{}",
        next.prompt.text
    );
    f.close().await;
}

/// (c) A late decline changes nothing and tells the agent, as its next
/// message, to carry on without it.
#[tokio::test]
async fn a_late_decline_changes_nothing_and_tells_the_agent() {
    let backend = Arc::new(Backend::default());
    let f = Fixture::with(|b| b.sandboxes(backend.clone())).await;
    let (project, task_id, spec) = chat(&f).await;
    let (_, a) = asked_then_let_go(&f, &spec, vec![AccessNeed::ChangeFiles]).await;
    ended(&f, spec.run_id, ports::RunExit::Completed).await;
    let receipt = decide(
        &f,
        &approval(&f, a.approval_id),
        ApprovalDecision::Deny { reason: None },
    )
    .await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    until("the decline was recorded", || {
        approval(&f, a.approval_id).state == ApprovalState::Denied
    })
    .await;
    assert!(!still_raised(&f, a.approval_id));
    let access = task(&f, task_id).access.unwrap();
    assert_eq!(access.read, [project]);
    assert!(access.write.is_empty());
    f.hub.pass_on_followups().await.unwrap();
    let next = next_spec(&f, task_id, spec.run_id).await;
    assert!(
        next.prompt.text.contains("”: no. Carry on without it."),
        "{}",
        next.prompt.text
    );
    f.close().await;
}

/// (d) An access request has no deadline of its own. Unanswered for ten
/// minutes it goes back to the agent like a question, which is told the
/// answer will come later (not that the person said no), and an Allow
/// long after still applies.
#[tokio::test]
async fn an_access_request_goes_back_after_ten_minutes_and_never_expires() {
    let backend = Arc::new(Backend::default());
    let f = Fixture::with(|b| b.sandboxes(backend.clone())).await;
    let (project, task_id, spec) = chat(&f).await;
    let binding = bound(&f, &spec).await;
    let hub = f.hub.clone();
    let asking = tokio::spawn(async move {
        hub.handle(
            &binding,
            BridgeRequest::RequestAccess {
                needs: vec![AccessNeed::ChangeFiles],
                reason: "to fix the build".into(),
            },
        )
        .await
    });
    let a = approval_where(&f, |a| a.access_request.is_some()).await;
    assert_eq!(a.expires_at, None, "no deadline of its own");
    f.clock.advance(CARRY_ON_AFTER);
    assert_eq!(f.hub.carry_on_unanswered().await.unwrap(), 1);
    assert!(carried_on(&f, a.approval_id));
    let told = asking.await.unwrap();
    let BridgeResponse::AccessResult { text } = told else {
        panic!("{told:?}")
    };
    assert_eq!(text, GUIDANCE);
    // Well past the old ten-minute limit, it is still the person's to answer.
    f.clock.advance(Duration::from_secs(50 * 60));
    assert_eq!(approval(&f, a.approval_id).state, ApprovalState::Pending);
    ended(&f, spec.run_id, ports::RunExit::Completed).await;
    let receipt = decide(&f, &approval(&f, a.approval_id), ApprovalDecision::Allow).await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    assert_eq!(task(&f, task_id).access.unwrap().write, [project]);
    until("the Allow was recorded", || {
        approval(&f, a.approval_id).state == ApprovalState::Allowed
    })
    .await;
    f.close().await;
}

/// (e) A person's Stop withdraws the request; the bridge closing after it
/// doesn't bring it back, and a late Allow changes nothing.
#[tokio::test]
async fn a_person_stop_withdraws_the_request_for_good() {
    let backend = Arc::new(Backend::default());
    let f = Fixture::with(|b| b.sandboxes(backend.clone())).await;
    let (project, task_id, spec) = chat(&f).await;
    let binding = bound(&f, &spec).await;
    let hub = f.hub.clone();
    let asking_as = binding.clone();
    let asking = tokio::spawn(async move {
        hub.handle(
            &asking_as,
            BridgeRequest::RequestAccess {
                needs: vec![AccessNeed::ChangeFiles],
                reason: "to fix the build".into(),
            },
        )
        .await
    });
    let a = approval_where(&f, |a| a.access_request.is_some()).await;
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
    assert_eq!(approval(&f, a.approval_id).state, ApprovalState::Cancelled);
    asking.abort();
    let _ = asking.await;
    BridgeHandler::unbound(&f.hub, &binding).await;
    ended(&f, spec.run_id, ports::RunExit::Interrupted).await;
    let gone = approval(&f, a.approval_id);
    assert_eq!(gone.state, ApprovalState::Cancelled);
    assert!(!carried_on(&f, a.approval_id));
    assert!(!still_raised(&f, a.approval_id));
    let receipt = decide(&f, &gone, ApprovalDecision::Allow).await;
    assert_eq!(receipt.status, ReceiptStatus::Rejected, "{receipt:?}");
    assert_eq!(receipt.error.map(|e| e.code), Some(ErrorCode::Expired));
    assert!(task(&f, task_id).access.unwrap().write.is_empty());
    let _ = project;
    f.close().await;
}

/// (f) Archiving the chat withdraws what it went on without; a late Allow
/// is refused, saying why, and changes nothing.
#[tokio::test]
async fn an_archived_chat_refuses_a_late_allow() {
    let backend = Arc::new(Backend::default());
    let f = Fixture::with(|b| b.sandboxes(backend.clone())).await;
    let (_, task_id, spec) = chat(&f).await;
    let (_, a) = asked_then_let_go(&f, &spec, vec![AccessNeed::ChangeFiles]).await;
    ended(&f, spec.run_id, ports::RunExit::Completed).await;
    let revision = task(&f, task_id).revision;
    let archive = f
        .hub
        .submit(
            &Caller::local_owner(),
            edit(Command::ArchiveTask { task_id }, revision),
        )
        .await;
    assert_eq!(archive.status, ReceiptStatus::Accepted, "{archive:?}");
    let gone = approval(&f, a.approval_id);
    assert_eq!(gone.state, ApprovalState::Cancelled);
    assert!(!still_raised(&f, a.approval_id));
    let receipt = decide(&f, &gone, ApprovalDecision::Allow).await;
    assert_eq!(receipt.status, ReceiptStatus::Rejected, "{receipt:?}");
    let refused = receipt.error.unwrap();
    assert_eq!(refused.code, ErrorCode::Expired);
    assert!(refused.message.contains("archived"), "{}", refused.message);
    assert!(task(&f, task_id).access.unwrap().write.is_empty());
    f.close().await;
}

/// A Plan task's request to work outlives its turn too: switching it to
/// Work later carries the same session on, told it may now do what it
/// asked for.
#[tokio::test]
async fn a_plan_tasks_request_switches_to_work_after_its_turn_ended() {
    let f = Fixture::new().await;
    all_modes(&f);
    let project = f.project().await;
    let (task_id, spec) = started(&f, create(project, TaskMode::Plan, None)).await;
    let (_, a) = asked_then_let_go(&f, &spec, vec![AccessNeed::ChangeFiles]).await;
    assert!(carried_on(&f, a.approval_id));
    ended(&f, spec.run_id, ports::RunExit::Completed).await;
    let receipt = f
        .hub
        .submit(
            &Caller::local_owner(),
            CommandEnvelope::new(Command::SwitchToWork {
                task_id,
                approval_id: Some(a.approval_id),
                text: None,
            }),
        )
        .await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    assert_eq!(approval(&f, a.approval_id).state, ApprovalState::Allowed);
    assert!(!carried_on(&f, a.approval_id));
    let next = next_spec(&f, task_id, spec.run_id).await;
    assert_eq!(next.mode, TaskMode::Work);
    assert!(
        next.prompt
            .text
            .contains("so you can do what you asked for (to fix the build)"),
        "{}",
        next.prompt.text
    );
    f.close().await;
}

/// What keeps its own time limit (a saved login, say) still ends when its
/// agent stops waiting.
#[tokio::test]
async fn a_login_request_still_expires_when_its_agent_stops_waiting() {
    let f = Fixture::new().await;
    all_modes(&f);
    let project = f.project().await;
    let (task_id, spec) = started(&f, create(project, TaskMode::Work, None)).await;
    let binding = bound(&f, &spec).await;
    let task_now = task(&f, task_id);
    let id = ApprovalId::new();
    let hub = f.hub.clone();
    let run_id = spec.run_id;
    let waiting = tokio::spawn(async move {
        hub.bridge_wait_as(
            id,
            run_id,
            &task_now,
            ports::tool_approval::ToolApproval {
                kind: ApprovalKind::LoginUse,
                title: "Let it sign in?".into(),
                headline: None,
                subject: Default::default(),
                detail: None,
                detail_lines: Vec::new(),
                questions: Vec::new(),
                scope: "This turn only".into(),
                shortened: false,
            },
            None,
            None,
            None,
        )
        .await
    });
    approval_where(&f, |a| a.approval_id == id).await;
    waiting.abort();
    let _ = waiting.await;
    BridgeHandler::unbound(&f.hub, &binding).await;
    assert_eq!(approval(&f, id).state, ApprovalState::Expired);
    assert!(!carried_on(&f, id));
    assert!(!still_raised(&f, id));
    f.close().await;
}

/// The person turned the internet off after the agent asked a chat for its
/// own network, which the card showed alone. A late Allow would turn the
/// internet back on too, so it is refused and the card withdrawn.
#[tokio::test]
async fn a_late_allow_never_turns_back_on_what_the_card_did_not_show() {
    let backend = Arc::new(Backend::default());
    let f = Fixture::with(|b| b.sandboxes(backend.clone())).await;
    let (project, task_id, spec) = held(&f, true, |a| a.internet = true).await;
    let (_, a) = asked_then_let_go(&f, &spec, vec![AccessNeed::LocalNetwork]).await;
    assert_eq!(
        a.access_request.as_ref().unwrap().needs,
        [AccessNeed::LocalNetwork],
        "the card shows its own network alone"
    );
    ended(&f, spec.run_id, ports::RunExit::Completed).await;
    let receipt = f
        .hub
        .submit(
            &Caller::local_owner(),
            CommandEnvelope::new(Command::SetConversationAccess {
                task_id,
                access: ConversationAccess {
                    read: vec![project],
                    internet: false,
                    ..Default::default()
                },
            }),
        )
        .await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    refused_as_changed(&f, task_id, a.approval_id).await;
    let access = task(&f, task_id).access.unwrap();
    assert!(!access.internet && !access.local_network);
    f.close().await;
}

/// A task with no internet asked for the whole computer, which on its own
/// still leaves it off the internet: the card warned of no more. The person
/// then turned the internet on, so a late Allow would lift every limit
/// without the card saying so. It is refused and the card withdrawn.
#[tokio::test]
async fn a_late_allow_never_lifts_every_limit_unwarned() {
    let backend = Arc::new(Backend::default());
    let f = Fixture::with(|b| b.sandboxes(backend.clone())).await;
    let (_, task_id, spec) = held(&f, false, |a| a.internet = false).await;
    let (_, a) = asked_then_let_go(&f, &spec, vec![AccessNeed::WholeComputer]).await;
    assert!(
        !(a.facts.iter()).any(|f| f.text.starts_with("lifts this ")),
        "{:?}",
        a.facts
    );
    ended(&f, spec.run_id, ports::RunExit::Completed).await;
    access_now(&f, task_id, |a| a.internet = true);
    refused_as_changed(&f, task_id, a.approval_id).await;
    let access = task(&f, task_id).access.unwrap();
    assert!(!access.whole_read && !access.whole_write);
    f.close().await;
}

/// With the access as it was when the agent asked, the same late Allow
/// gives the whole computer and leaves the internet off.
#[tokio::test]
async fn a_late_allow_with_the_access_unchanged_applies() {
    let backend = Arc::new(Backend::default());
    let f = Fixture::with(|b| b.sandboxes(backend.clone())).await;
    let (_, task_id, spec) = held(&f, false, |a| a.internet = false).await;
    let (_, a) = asked_then_let_go(&f, &spec, vec![AccessNeed::WholeComputer]).await;
    ended(&f, spec.run_id, ports::RunExit::Completed).await;
    let receipt = decide(&f, &approval(&f, a.approval_id), ApprovalDecision::Allow).await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    let access = task(&f, task_id).access.unwrap();
    assert!(access.whole_read && access.whole_write && !access.internet);
    until("the Allow was recorded", || {
        approval(&f, a.approval_id).state == ApprovalState::Allowed
    })
    .await;
    f.close().await;
}

/// Turning on less than the card showed is fine: the person turned the
/// internet on themselves, and a late Allow adds only the file changes.
#[tokio::test]
async fn a_late_allow_may_turn_on_less_than_the_card_showed() {
    let backend = Arc::new(Backend::default());
    let f = Fixture::with(|b| b.sandboxes(backend.clone())).await;
    let (project, task_id, spec) = held(&f, true, |a| a.internet = false).await;
    let needs = vec![AccessNeed::ChangeFiles, AccessNeed::Internet];
    let (_, a) = asked_then_let_go(&f, &spec, needs).await;
    ended(&f, spec.run_id, ports::RunExit::Completed).await;
    access_now(&f, task_id, |a| a.internet = true);
    let receipt = decide(&f, &approval(&f, a.approval_id), ApprovalDecision::Allow).await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    assert_eq!(
        task(&f, task_id).access.unwrap(),
        ConversationAccess {
            read: vec![project],
            write: vec![project],
            internet: true,
            ..Default::default()
        }
        .normalized()
    );
    f.close().await;
}

/// The access changes while the agent still waits: the Allow is refused
/// the same way, and the agent is told its request was withdrawn rather
/// than declined.
#[tokio::test]
async fn an_allow_after_the_access_changed_mid_wait_is_refused_and_the_agent_told() {
    let backend = Arc::new(Backend::default());
    let f = Fixture::with(|b| b.sandboxes(backend.clone())).await;
    let (_, task_id, spec) = held(&f, true, |a| a.internet = true).await;
    let binding = bound(&f, &spec).await;
    let hub = f.hub.clone();
    let asking = tokio::spawn(async move {
        hub.handle(
            &binding,
            BridgeRequest::RequestAccess {
                needs: vec![AccessNeed::LocalNetwork],
                reason: "to reach the build box".into(),
            },
        )
        .await
    });
    let a = approval_where(&f, |a| a.access_request.is_some()).await;
    access_now(&f, task_id, |a| a.internet = false);
    refused_as_changed(&f, task_id, a.approval_id).await;
    let told = tokio::time::timeout(Duration::from_secs(15), asking)
        .await
        .unwrap()
        .unwrap();
    let BridgeResponse::AccessResult { text } = told else {
        panic!("{told:?}")
    };
    assert_eq!(text, crate::more_access::WITHDRAWN);
    f.close().await;
}

/// An Allow tapped just as the agent stops waiting, before Ophio sees its
/// connection close, can no longer reach the waiting call: it goes to the
/// agent as its next message, here in the same turn, instead of being lost.
#[tokio::test]
async fn an_allow_as_the_agent_stops_waiting_reaches_it_as_its_next_message() {
    let backend = Arc::new(Backend::default());
    let f = Fixture::with(|b| b.sandboxes(backend.clone())).await;
    let (_, task_id, spec) = held(&f, true, |a| a.internet = false).await;
    let binding = bound(&f, &spec).await;
    let hub = f.hub.clone();
    let asking_as = binding.clone();
    let asking = tokio::spawn(async move {
        hub.handle(
            &asking_as,
            BridgeRequest::RequestAccess {
                needs: vec![AccessNeed::Internet],
                reason: "to fix the build".into(),
            },
        )
        .await
    });
    let a = approval_where(&f, |a| a.access_request.is_some()).await;
    // Its call gave up; the connection hasn't been seen to close yet.
    asking.abort();
    let _ = asking.await;
    let receipt = decide(&f, &a, ApprovalDecision::Allow).await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    until("the Allow was recorded", || {
        approval(&f, a.approval_id).state == ApprovalState::Allowed
    })
    .await;
    assert!(!carried_on(&f, a.approval_id));
    assert!(task(&f, task_id).access.unwrap().internet);
    BridgeHandler::unbound(&f.hub, &binding).await;
    // The turn still runs, so it hears the answer there.
    f.hub.pass_on_followups().await.unwrap();
    let told = || {
        (f.harness.controls.texts.lock().unwrap().iter())
            .any(|t| t.contains("”: yes, it's allowed now."))
    };
    until("the agent was told", told).await;
    f.close().await;
}

/// The turn ends while the agent's call still waits, before its connection
/// closes: the turn's end keeps the request open, and the connection
/// closing after doesn't end it either.
#[tokio::test]
async fn a_request_outlives_its_turn_ending_while_the_agent_still_waits() {
    let backend = Arc::new(Backend::default());
    let f = Fixture::with(|b| b.sandboxes(backend.clone())).await;
    let (project, task_id, spec) = chat(&f).await;
    let binding = bound(&f, &spec).await;
    let hub = f.hub.clone();
    let asking_as = binding.clone();
    let asking = tokio::spawn(async move {
        hub.handle(
            &asking_as,
            BridgeRequest::RequestAccess {
                needs: vec![AccessNeed::ChangeFiles],
                reason: "to fix the build".into(),
            },
        )
        .await
    });
    let a = approval_where(&f, |a| a.access_request.is_some()).await;
    ended(&f, spec.run_id, ports::RunExit::Completed).await;
    assert_eq!(approval(&f, a.approval_id).state, ApprovalState::Pending);
    assert!(carried_on(&f, a.approval_id));
    assert!(still_raised(&f, a.approval_id));
    let finished = f
        .hub
        .read(|r| r.list::<AttentionItem>("attention"))
        .unwrap()
        .into_iter()
        .find(|i| i.run_id == Some(spec.run_id) && i.kind == AttentionKind::Completed)
        .expect("the turn's end was told");
    assert_eq!(
        finished.body.as_deref(),
        Some("Finished what it could. It still needs your answer.")
    );
    asking.abort();
    let _ = asking.await;
    BridgeHandler::unbound(&f.hub, &binding).await;
    assert_eq!(approval(&f, a.approval_id).state, ApprovalState::Pending);
    assert!(still_raised(&f, a.approval_id));
    let receipt = decide(&f, &approval(&f, a.approval_id), ApprovalDecision::Allow).await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    until("the Allow was recorded", || {
        approval(&f, a.approval_id).state == ApprovalState::Allowed
    })
    .await;
    assert_eq!(task(&f, task_id).access.unwrap().write, [project]);
    f.close().await;
}

/// A chat's ordinary request it can't allow (a web fetch) offers to turn
/// the chat into a task. Kept after its turn, that button still works:
/// the chat carries on in Work, and the same fetch asked again there is
/// allowed once.
#[tokio::test]
async fn a_kept_ordinary_request_still_turns_the_chat_into_a_task() {
    let backend = Arc::new(Backend::default());
    let f = Fixture::with(|b| b.sandboxes(backend.clone())).await;
    let (_, task_id, spec) = chat(&f).await;
    let binding = bound(&f, &spec).await;
    let hub = f.hub.clone();
    let asking_as = binding.clone();
    let asking = tokio::spawn(async move {
        hub.handle(
            &asking_as,
            BridgeRequest::PermissionPrompt {
                tool_name: "WebFetch".into(),
                input: serde_json::json!({"url": "https://example.com/notes"}),
                tool_use_id: Some("toolu_fetch".into()),
                native_request_id: Some("fetch".into()),
            },
        )
        .await
    });
    let a = approval_where(&f, |a| a.kind == ApprovalKind::Network).await;
    assert_eq!(
        a.actions.as_deref(),
        Some(&[ApprovalAction::TurnIntoTask, ApprovalAction::Deny][..])
    );
    asking.abort();
    let _ = asking.await;
    BridgeHandler::unbound(&f.hub, &binding).await;
    assert!(carried_on(&f, a.approval_id));
    ended(&f, spec.run_id, ports::RunExit::Completed).await;
    let receipt = f
        .hub
        .submit(
            &Caller::local_owner(),
            CommandEnvelope::new(Command::SwitchToWork {
                task_id,
                approval_id: Some(a.approval_id),
                text: None,
            }),
        )
        .await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    assert_eq!(approval(&f, a.approval_id).state, ApprovalState::Allowed);
    assert!(!carried_on(&f, a.approval_id));
    assert!(!still_raised(&f, a.approval_id));
    assert_eq!(
        passes(&f, task_id).len(),
        1,
        "asked again in Work, it is allowed once"
    );
    let next = next_spec(&f, task_id, spec.run_id).await;
    assert_eq!(next.mode, TaskMode::Work);
    assert!(!task(&f, task_id).conversation);
    f.close().await;
}

/// Switching a Plan task to Work by hand leaves a kept request to switch
/// with no way forward: it is withdrawn, not left offering a dead button.
#[tokio::test]
async fn switching_by_hand_withdraws_a_kept_request_to_switch() {
    let f = Fixture::new().await;
    all_modes(&f);
    let project = f.project().await;
    let (task_id, spec) = started(&f, create(project, TaskMode::Plan, None)).await;
    let (_, a) = asked_then_let_go(&f, &spec, vec![AccessNeed::ChangeFiles]).await;
    assert!(carried_on(&f, a.approval_id));
    ended(&f, spec.run_id, ports::RunExit::Completed).await;
    let receipt = f
        .hub
        .submit(
            &Caller::local_owner(),
            CommandEnvelope::new(Command::SwitchToWork {
                task_id,
                approval_id: None,
                text: None,
            }),
        )
        .await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    let gone = approval(&f, a.approval_id);
    assert_eq!(gone.state, ApprovalState::Cancelled);
    assert!(!carried_on(&f, a.approval_id));
    assert!(!still_raised(&f, a.approval_id));
    assert!(
        (notes(&f, task_id).await.iter()).any(|n| n.contains("was withdrawn as the task switched")),
        "{:?}",
        notes(&f, task_id).await
    );
    f.close().await;
}

/// The agent asks the person to run `command` through the bridge as
/// `binding`, as Claude Code's permission prompt does.
fn ask_to_run(
    f: &Fixture,
    binding: &BridgeBinding,
    command: &'static str,
    call: &'static str,
) -> tokio::task::JoinHandle<BridgeResponse> {
    let hub = f.hub.clone();
    let binding = binding.clone();
    tokio::spawn(async move {
        hub.handle(
            &binding,
            BridgeRequest::PermissionPrompt {
                tool_name: "Bash".into(),
                input: serde_json::json!({ "command": command }),
                tool_use_id: Some(call.into()),
                native_request_id: Some(call.into()),
            },
        )
        .await
    })
}

/// A late Allow on a kept permission prompt, in the same turn: unanswered
/// for ten minutes it went back to the agent, which carried on. Told it may,
/// the agent asks again, and the same request goes through once without a
/// second card. Asked a third time, the person is asked as usual.
#[tokio::test]
async fn a_late_allow_lets_the_same_request_through_once() {
    let f = Fixture::new().await;
    all_modes(&f);
    let project = f.project().await;
    let (_, spec) = started(&f, create(project, TaskMode::Work, None)).await;
    let binding = bound(&f, &spec).await;
    let first = ask_to_run(&f, &binding, "make build", "toolu_1");
    let a = approval_where(&f, |a| a.state == ApprovalState::Pending).await;
    assert_eq!(a.kind, ApprovalKind::Command);
    f.clock.advance(CARRY_ON_AFTER);
    assert_eq!(f.hub.carry_on_unanswered().await.unwrap(), 1);
    first.await.unwrap();
    assert!(carried_on(&f, a.approval_id));
    let receipt = decide(&f, &approval(&f, a.approval_id), ApprovalDecision::Allow).await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    until("the Allow was recorded", || {
        approval(&f, a.approval_id).state == ApprovalState::Allowed
    })
    .await;

    let told = tokio::time::timeout(
        Duration::from_secs(15),
        ask_to_run(&f, &binding, "make build", "toolu_2"),
    )
    .await
    .unwrap()
    .unwrap();
    let BridgeResponse::Permission { decision } = told else {
        panic!("{told:?}")
    };
    assert_eq!(decision["behavior"], "allow", "{decision}");
    let approvals: Vec<Approval> = f.hub.read(|r| r.list("approval")).unwrap();
    assert_eq!(approvals.len(), 2);
    assert!(
        (approvals.iter()).all(|a| a.state == ApprovalState::Allowed),
        "{approvals:?}"
    );
    // Only once: a third time, it asks the person.
    let seen: Vec<ApprovalId> = approvals.iter().map(|a| a.approval_id).collect();
    let third = ask_to_run(&f, &binding, "make build", "toolu_3");
    let asked = approval_where(&f, |b| {
        b.state == ApprovalState::Pending && !seen.contains(&b.approval_id)
    })
    .await;
    assert_eq!(asked.kind, ApprovalKind::Command);
    third.abort();
    let _ = third.await;
    assert!(passes(&f, a.task_id).is_empty(), "used once, it is gone");
    f.close().await;
}

/// Its turn over, a late Allow starts the next one; there the same request
/// goes through without a second card, and nothing else does.
#[tokio::test]
async fn a_late_allow_lets_the_same_request_through_in_the_next_turn_only() {
    let f = Fixture::new().await;
    all_modes(&f);
    let project = f.project().await;
    let (task_id, spec) = started(&f, create(project, TaskMode::Work, None)).await;
    let binding = bound(&f, &spec).await;
    let first = ask_to_run(&f, &binding, "make build", "toolu_1");
    let a = approval_where(&f, |a| a.state == ApprovalState::Pending).await;
    first.abort();
    let _ = first.await;
    BridgeHandler::unbound(&f.hub, &binding).await;
    ended(&f, spec.run_id, ports::RunExit::Completed).await;
    assert!(carried_on(&f, a.approval_id));
    let receipt = decide(&f, &approval(&f, a.approval_id), ApprovalDecision::Allow).await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    f.hub.pass_on_followups().await.unwrap();
    let next = next_spec(&f, task_id, spec.run_id).await;
    run_started(&f, task_id).await;
    let binding = bound(&f, &next).await;
    let told = tokio::time::timeout(
        Duration::from_secs(15),
        ask_to_run(&f, &binding, "make build", "toolu_2"),
    )
    .await
    .unwrap()
    .unwrap();
    let BridgeResponse::Permission { decision } = told else {
        panic!("{told:?}")
    };
    assert_eq!(decision["behavior"], "allow", "{decision}");
    let other = ask_to_run(&f, &binding, "make clean", "toolu_3");
    let asked = approval_where(&f, |b| b.state == ApprovalState::Pending).await;
    assert_eq!(asked.run_id, next.run_id);
    assert!(
        (asked.raw_request.as_deref()).is_some_and(|r| r.contains("make clean")),
        "{asked:?}"
    );
    other.abort();
    let _ = other.await;
    f.close().await;
}

/// The person answers `id` as `decision`, and it is recorded.
async fn answered(f: &Fixture, id: ApprovalId, decision: ApprovalDecision) {
    let receipt = decide(f, &approval(f, id), decision).await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    until("the answer was recorded", || {
        approval(f, id).state != ApprovalState::Pending
    })
    .await;
}

/// The next turn the harness was asked for in `task_id`, after `seen`.
async fn spec_after(f: &Fixture, task_id: TaskId, seen: &[RunId]) -> ports::RunSpec {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let found = (f.harness.specs.lock().unwrap().iter())
                .find(|s| s.task_id == task_id && !seen.contains(&s.run_id))
                .cloned();
            if let Some(spec) = found {
                return spec;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("no next turn")
}

/// The person writes to `task_id` again after its turns in `seen` ended:
/// the turn that starts, with its bridge.
async fn resumed(f: &Fixture, task_id: TaskId, seen: &[RunId]) -> BridgeBinding {
    let receipt = f
        .hub
        .submit(
            &Caller::local_owner(),
            edit(
                Command::ResumeTask {
                    observed_reply: None,
                    harness: None,
                    task_id,
                    text: Some("Carry on".into()),
                    bundle: None,
                    mode: None,
                    carried_plan: None,
                    read_only: false,
                    model: None,
                    effort: None,
                },
                task(f, task_id).revision,
            ),
        )
        .await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    let next = spec_after(f, task_id, seen).await;
    run_started(f, task_id).await;
    bound(f, &next).await
}

/// A Work task's first turn asks to run `make build` and stops waiting; the
/// turn ends, and the person allows it late, which leaves it a pass.
async fn allowed_late(f: &Fixture) -> (TaskId, RunId) {
    all_modes(f);
    let project = f.project().await;
    let (task_id, spec) = started(f, create(project, TaskMode::Work, None)).await;
    let binding = bound(f, &spec).await;
    let first = ask_to_run(f, &binding, "make build", "toolu_1");
    let a = approval_where(f, |a| a.state == ApprovalState::Pending).await;
    first.abort();
    let _ = first.await;
    BridgeHandler::unbound(&f.hub, &binding).await;
    ended(f, spec.run_id, ports::RunExit::Completed).await;
    assert!(carried_on(f, a.approval_id));
    answered(f, a.approval_id, ApprovalDecision::Allow).await;
    assert_eq!(passes(f, task_id).len(), 1);
    (task_id, spec.run_id)
}

/// The turn the late answer started, after `first`, ends without asking
/// anything.
async fn answer_turn_ends(f: &Fixture, task_id: TaskId, first: RunId) {
    f.hub.pass_on_followups().await.unwrap();
    let next = next_spec(f, task_id, first).await;
    run_started(f, task_id).await;
    ended(f, next.run_id, ports::RunExit::Completed).await;
}

/// The agent asks `command` again as `binding` and is answered at once, with
/// no card.
async fn let_through(
    f: &Fixture,
    binding: &BridgeBinding,
    command: &'static str,
    call: &'static str,
) {
    let told = tokio::time::timeout(
        Duration::from_secs(15),
        ask_to_run(f, binding, command, call),
    )
    .await
    .expect("it went through without a card")
    .unwrap();
    let BridgeResponse::Permission { decision } = told else {
        panic!("{told:?}")
    };
    assert_eq!(decision["behavior"], "allow", "{decision}");
}

/// The agent asks `command` again as `binding` and the person is asked: a
/// card, not a pass.
async fn asked_again(
    f: &Fixture,
    binding: &BridgeBinding,
    command: &'static str,
    call: &'static str,
) {
    let asking = ask_to_run(f, binding, command, call);
    let asked = approval_where(f, |b| {
        b.state == ApprovalState::Pending && b.native_request_id == call
    })
    .await;
    assert_eq!(asked.run_id, binding.run_id);
    asking.abort();
    let _ = asking.await;
}

/// Two kept requests allowed late each go through once: the second Allow
/// doesn't take the first one's place.
#[tokio::test]
async fn two_late_allows_each_let_their_own_request_through() {
    let f = Fixture::new().await;
    all_modes(&f);
    let project = f.project().await;
    let (task_id, spec) = started(&f, create(project, TaskMode::Work, None)).await;
    let binding = bound(&f, &spec).await;
    let build = ask_to_run(&f, &binding, "make build", "toolu_1");
    let test = ask_to_run(&f, &binding, "make test", "toolu_2");
    let a = approval_where(&f, |a| a.native_request_id == "toolu_1").await;
    let b = approval_where(&f, |a| a.native_request_id == "toolu_2").await;
    f.clock.advance(CARRY_ON_AFTER);
    assert_eq!(f.hub.carry_on_unanswered().await.unwrap(), 2);
    build.await.unwrap();
    test.await.unwrap();
    for id in [a.approval_id, b.approval_id] {
        answered(&f, id, ApprovalDecision::Allow).await;
    }
    assert_eq!(passes(&f, task_id).len(), 2);
    let_through(&f, &binding, "make build", "toolu_3").await;
    let_through(&f, &binding, "make test", "toolu_4").await;
    assert!(passes(&f, task_id).is_empty());
    f.close().await;
}

/// A switch's pass and a late Allow's pass, for different requests, are
/// both kept, whichever came first; and a pass kept by an older version,
/// under its task alone, is still used.
#[tokio::test]
async fn a_switch_and_a_late_allow_keep_each_others_pass() {
    let f = Fixture::new().await;
    all_modes(&f);
    let project = f.project().await;
    let (task_id, spec) = started(&f, create(project, TaskMode::Work, None)).await;
    let binding = bound(&f, &spec).await;
    let asking = ask_to_run(&f, &binding, "make build", "toolu_1");
    let a = approval_where(&f, |a| a.state == ApprovalState::Pending).await;
    asking.abort();
    let _ = asking.await;
    // The same request as `a`, or another, asked by a later turn.
    let like = |command: &str| {
        let mut b = a.clone();
        b.approval_id = ApprovalId::new();
        b.run_id = RunId::new();
        // A later turn is a real stored run of the same agent; an invented
        // run id alone cannot establish which agent may use the permission.
        f.hub
            .write(|tx| {
                let mut later: Run = tx.read().require("run", a.run_id)?;
                later.run_id = b.run_id;
                tx.put("run", later.run_id, &later)
            })
            .unwrap();
        b.raw_request = Some((a.raw_request.as_deref().unwrap()).replace("make build", command));
        b
    };
    let test = like("make test");
    let owner = Principal::LocalOwner;
    let take = |b: &Approval| {
        let now = f.hub.inner.clock.now();
        f.hub
            .write(|tx| crate::switch_work::take_preapproved(tx, b, now))
            .unwrap()
    };
    for switch_first in [true, false] {
        let now = f.hub.inner.clock.now();
        f.hub
            .write(|tx| {
                let (switched, late) = match switch_first {
                    true => (&a, &test),
                    false => (&test, &a),
                };
                crate::switch_work::preapprove(tx, switched, owner, false, now)?;
                crate::switch_work::preapprove(tx, late, owner, true, now)
            })
            .unwrap();
        assert_eq!(passes(&f, task_id).len(), 2);
        assert_eq!(take(&like("make build")), Some(owner));
        assert_eq!(take(&like("make test")), Some(owner));
        assert!(passes(&f, task_id).is_empty());
    }
    // As an older version kept it.
    let now = f.hub.inner.clock.now();
    f.hub
        .write(|tx| {
            tx.put(
                crate::switch_work::PREAPPROVED,
                task_id,
                &serde_json::json!({
                    "request": crate::switch_work::request_key(
                        &a.kind,
                        a.raw_request.as_deref(),
                        &a.title,
                        a.detail.as_deref(),
                    ),
                    "asked_in": a.run_id,
                    "by": owner,
                    "until": Timestamp(now.0 + 60_000),
                }),
            )
        })
        .unwrap();
    assert_eq!(passes(&f, task_id).len(), 1);
    assert_eq!(take(&like("make build")), Some(owner));
    assert!(passes(&f, task_id).is_empty());
    f.close().await;
}

/// Allowed late, then declined on another card for the same request: the
/// last word is no, so the next turn's same request gets a card.
#[tokio::test]
async fn a_later_decline_of_the_same_request_cancels_the_pass() {
    let f = Fixture::new().await;
    all_modes(&f);
    let project = f.project().await;
    let (task_id, spec) = started(&f, create(project, TaskMode::Work, None)).await;
    let binding = bound(&f, &spec).await;
    let first = ask_to_run(&f, &binding, "make build", "toolu_1");
    let second = ask_to_run(&f, &binding, "make build", "toolu_2");
    let a = approval_where(&f, |a| a.native_request_id == "toolu_1").await;
    let b = approval_where(&f, |a| a.native_request_id == "toolu_2").await;
    first.abort();
    second.abort();
    let _ = (first.await, second.await);
    BridgeHandler::unbound(&f.hub, &binding).await;
    ended(&f, spec.run_id, ports::RunExit::Completed).await;
    assert!(carried_on(&f, a.approval_id) && carried_on(&f, b.approval_id));
    answered(&f, a.approval_id, ApprovalDecision::Allow).await;
    assert_eq!(passes(&f, task_id).len(), 1);
    let deny = ApprovalDecision::Deny { reason: None };
    answered(&f, b.approval_id, deny).await;
    assert!(passes(&f, task_id).is_empty());
    f.hub.pass_on_followups().await.unwrap();
    let next = next_spec(&f, task_id, spec.run_id).await;
    run_started(&f, task_id).await;
    let binding = bound(&f, &next).await;
    asked_again(&f, &binding, "make build", "toolu_3").await;
    f.close().await;
}

/// The same when the decline reaches a turn still waiting on it.
#[tokio::test]
async fn a_decline_handed_to_the_agent_cancels_the_pass_too() {
    let f = Fixture::new().await;
    all_modes(&f);
    let project = f.project().await;
    let (task_id, spec) = started(&f, create(project, TaskMode::Work, None)).await;
    let binding = bound(&f, &spec).await;
    let first = ask_to_run(&f, &binding, "make build", "toolu_1");
    let a = approval_where(&f, |a| a.native_request_id == "toolu_1").await;
    f.clock.advance(CARRY_ON_AFTER);
    assert_eq!(f.hub.carry_on_unanswered().await.unwrap(), 1);
    first.await.unwrap();
    let second = ask_to_run(&f, &binding, "make build", "toolu_2");
    let b = approval_where(&f, |a| a.native_request_id == "toolu_2").await;
    answered(&f, a.approval_id, ApprovalDecision::Allow).await;
    assert_eq!(passes(&f, task_id).len(), 1);
    let deny = ApprovalDecision::Deny { reason: None };
    answered(&f, b.approval_id, deny).await;
    let told = second.await.unwrap();
    let BridgeResponse::Permission { decision } = told else {
        panic!("{told:?}")
    };
    assert_eq!(decision["behavior"], "deny", "{decision}");
    assert!(passes(&f, task_id).is_empty());
    BridgeHandler::unbound(&f.hub, &binding).await;
    ended(&f, spec.run_id, ports::RunExit::Completed).await;
    let binding = resumed(&f, task_id, &[spec.run_id]).await;
    asked_again(&f, &binding, "make build", "toolu_3").await;
    f.close().await;
}

/// Allowed late, then the person stops the turn the answer started: what
/// they allowed doesn't go through later without a card.
#[tokio::test]
async fn a_stop_cancels_the_pass() {
    let f = Fixture::new().await;
    let (task_id, first) = allowed_late(&f).await;
    f.hub.pass_on_followups().await.unwrap();
    let second = next_spec(&f, task_id, first).await;
    run_started(&f, task_id).await;
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
    assert!(passes(&f, task_id).is_empty());
    ended(&f, second.run_id, ports::RunExit::Interrupted).await;
    let binding = resumed(&f, task_id, &[first, second.run_id]).await;
    asked_again(&f, &binding, "make build", "toolu_2").await;
    f.close().await;
}

/// Allowed late, then archived: the pass goes with what else was waiting.
#[tokio::test]
async fn archiving_cancels_the_pass() {
    let f = Fixture::new().await;
    let (task_id, first) = allowed_late(&f).await;
    answer_turn_ends(&f, task_id, first).await;
    let revision = task(&f, task_id).revision;
    let archive = f
        .hub
        .submit(
            &Caller::local_owner(),
            edit(Command::ArchiveTask { task_id }, revision),
        )
        .await;
    assert_eq!(archive.status, ReceiptStatus::Accepted, "{archive:?}");
    assert!(passes(&f, task_id).is_empty());
    f.close().await;
}

/// A pass lasts half an hour.
#[tokio::test]
async fn a_late_allows_pass_expires_after_half_an_hour() {
    let f = Fixture::new().await;
    let (task_id, first) = allowed_late(&f).await;
    f.clock.advance(Duration::from_secs(31 * 60));
    f.hub.pass_on_followups().await.unwrap();
    let next = next_spec(&f, task_id, first).await;
    run_started(&f, task_id).await;
    let binding = bound(&f, &next).await;
    asked_again(&f, &binding, "make build", "toolu_2").await;
    assert!(passes(&f, task_id).is_empty());
    f.close().await;
}

/// Another task asking the same thing is asked as usual.
#[tokio::test]
async fn another_task_cannot_use_the_pass() {
    let f = Fixture::new().await;
    let (task_id, first) = allowed_late(&f).await;
    answer_turn_ends(&f, task_id, first).await;
    let project = task(&f, task_id).project_id;
    let (_, other) = started(&f, create(project, TaskMode::Work, None)).await;
    let binding = bound(&f, &other).await;
    asked_again(&f, &binding, "make build", "toolu_2").await;
    assert_eq!(passes(&f, task_id).len(), 1, "still its own task's");
    f.close().await;
}

/// The Allow a pass gave never leaves another, even when the agent's
/// connection closed between the pass being used and that Allow.
#[tokio::test]
async fn a_pass_never_renews_itself() {
    let f = Fixture::new().await;
    all_modes(&f);
    let project = f.project().await;
    let (task_id, spec) = started(&f, create(project, TaskMode::Work, None)).await;
    let binding = bound(&f, &spec).await;
    let asking = ask_to_run(&f, &binding, "make build", "toolu_1");
    let a = approval_where(&f, |a| a.state == ApprovalState::Pending).await;
    let owner = Principal::LocalOwner;
    let now = f.hub.inner.clock.now();
    // A late Allow's pass for it, used as it was asked...
    let by = f
        .hub
        .write(|tx| {
            crate::switch_work::preapprove(tx, &a, owner, true, now)?;
            crate::switch_work::take_preapproved(tx, &a, now)
        })
        .unwrap();
    assert_eq!(by, Some(owner));
    // ...and the connection closes before the Allow it gave.
    asking.abort();
    let _ = asking.await;
    f.hub.inner.waits.lock().await.remove(&a.approval_id);
    f.hub
        .resolve_approval(
            &Caller::local_owner(),
            a.approval_id,
            &ApprovalDecision::Allow,
        )
        .await
        .unwrap();
    assert_eq!(approval(&f, a.approval_id).state, ApprovalState::Allowed);
    assert!(passes(&f, task_id).is_empty(), "{:?}", passes(&f, task_id));
    f.close().await;
}

/// A late Allow on a request for more access applies it, and leaves no pass.
#[tokio::test]
async fn a_late_allow_on_an_access_request_leaves_no_pass() {
    let backend = Arc::new(Backend::default());
    let f = Fixture::with(|b| b.sandboxes(backend.clone())).await;
    let (_, task_id, spec) = chat(&f).await;
    let (_, a) = asked_then_let_go(&f, &spec, vec![AccessNeed::ChangeFiles]).await;
    ended(&f, spec.run_id, ports::RunExit::Completed).await;
    answered(&f, a.approval_id, ApprovalDecision::Allow).await;
    until("the Allow was recorded", || {
        approval(&f, a.approval_id).state == ApprovalState::Allowed
    })
    .await;
    assert!(passes(&f, task_id).is_empty());
    f.close().await;
}

/// A late yes to a question leaves no pass.
#[tokio::test]
async fn a_late_allow_on_a_question_leaves_no_pass() {
    let f = Fixture::new().await;
    all_modes(&f);
    let project = f.project().await;
    let (task_id, spec) = started(&f, create(project, TaskMode::Work, None)).await;
    let binding = bound(&f, &spec).await;
    let hub = f.hub.clone();
    let asking_as = binding.clone();
    let asking = tokio::spawn(async move {
        hub.handle(
            &asking_as,
            BridgeRequest::RequestHuman {
                reason: "Shall I open a pull request?".into(),
            },
        )
        .await
    });
    let a = approval_where(&f, |a| a.kind == ApprovalKind::Question).await;
    asking.abort();
    let _ = asking.await;
    BridgeHandler::unbound(&f.hub, &binding).await;
    ended(&f, spec.run_id, ports::RunExit::Completed).await;
    assert!(carried_on(&f, a.approval_id));
    answered(&f, a.approval_id, ApprovalDecision::Allow).await;
    assert!(passes(&f, task_id).is_empty());
    f.close().await;
}

/// A late Allow on Ophio's own gate before a turn leaves no pass, though
/// it went back unanswered like any other request.
#[tokio::test]
async fn a_late_allow_on_ophios_own_gate_leaves_no_pass() {
    let f = Fixture::new().await;
    all_modes(&f);
    let project = f.project().await;
    let (task_id, spec) = started(&f, create(project, TaskMode::Work, None)).await;
    let binding = bound(&f, &spec).await;
    let asking = ask_to_run(&f, &binding, "make build", "toolu_1");
    let a = approval_where(&f, |a| a.state == ApprovalState::Pending).await;
    // Marked as the gate the turn waits on, as Ophio's own start check is.
    f.hub
        .write(|tx| {
            tx.put(
                crate::github_access::START_APPROVAL,
                spec.run_id,
                &a.approval_id,
            )
        })
        .unwrap();
    f.clock.advance(CARRY_ON_AFTER);
    assert_eq!(f.hub.carry_on_unanswered().await.unwrap(), 1);
    asking.await.unwrap();
    assert!(carried_on(&f, a.approval_id));
    answered(&f, a.approval_id, ApprovalDecision::Allow).await;
    assert!(passes(&f, task_id).is_empty());
    f.close().await;
}

/// A private question with a native route, unanswered for ten minutes,
/// expires: its agent is told to carry on without it, and the card no
/// longer asks for an answer nobody could give it.
#[tokio::test]
async fn a_private_question_expires_rather_than_stay_open() {
    let f = Fixture::new().await;
    let owner = Caller::local_owner();
    let project_id = f.project().await;
    let (task_id, _) = f
        .task(project_id, TaskPlacement::StartNow, &owner, false)
        .await;
    let run_id = run_started(&f, task_id).await;
    let approval_id = ApprovalId::new();
    let asked = private_question(&f, task_id, run_id, approval_id, "native-1");
    f.hub.write(|tx| f.hub.put_approval(tx, &asked)).unwrap();
    f.clock.advance(CARRY_ON_AFTER);
    assert_eq!(f.hub.carry_on_unanswered().await.unwrap(), 1);
    let gone = approval(&f, approval_id);
    assert_eq!(gone.state, ApprovalState::Expired);
    assert!(!carried_on(&f, approval_id));
    assert!(!still_raised(&f, approval_id));
    let declined = f.harness.controls.decisions.lock().unwrap().clone();
    assert_eq!(
        declined,
        vec![ApprovalDecision::Deny {
            reason: Some(crate::carry_on::PRIVATE_GUIDANCE.into())
        }]
    );
    let receipt = decide(
        &f,
        &gone,
        ApprovalDecision::Answer {
            text: "not-a-real-password".into(),
            answers: Vec::new(),
        },
    )
    .await;
    assert_eq!(receipt.status, ReceiptStatus::Rejected, "{receipt:?}");
    f.close().await;
}

/// A private question asked through the bridge isn't kept when its agent
/// stops waiting: it expires, as a login does.
#[tokio::test]
async fn a_private_bridge_question_expires_when_its_agent_stops_waiting() {
    let f = Fixture::new().await;
    all_modes(&f);
    let project = f.project().await;
    let (task_id, spec) = started(&f, create(project, TaskMode::Work, None)).await;
    let binding = bound(&f, &spec).await;
    let approval_id = ApprovalId::new();
    let asked = private_question(
        &f,
        task_id,
        spec.run_id,
        approval_id,
        &format!("bridge:{approval_id}"),
    );
    f.hub
        .write(|tx| {
            tx.put("bridge_approval", approval_id, &true)?;
            f.hub.put_approval(tx, &asked)
        })
        .unwrap();
    BridgeHandler::unbound(&f.hub, &binding).await;
    assert_eq!(approval(&f, approval_id).state, ApprovalState::Expired);
    assert!(!carried_on(&f, approval_id));
    assert!(!still_raised(&f, approval_id));
    f.close().await;
}

/// A question asking for a password, from `run_id`'s agent.
fn private_question(
    f: &Fixture,
    task_id: TaskId,
    run_id: RunId,
    approval_id: ApprovalId,
    native: &str,
) -> Approval {
    let project_id = task(f, task_id).project_id;
    Approval {
        headline: None,
        facts: Vec::new(),
        approval_id,
        task_id,
        run_id,
        project_id,
        native_request_id: native.into(),
        kind: ApprovalKind::Question,
        title: "Private question".into(),
        detail: None,
        detail_lines: Vec::new(),
        questions: vec![protocol::model::ApprovalQuestion {
            id: None,
            header: None,
            question: "Password?".into(),
            options: Vec::new(),
            multi_select: false,
            secret: true,
        }],
        raw_request: None,
        scope: "Once".into(),
        shortened: false,
        revision: 1,
        state: ApprovalState::Pending,
        access_request: None,
        actions: None,
        delivery: None,
        outcome: None,
        requested_at: f.hub.inner.clock.now(),
        expires_at: None,
        resolved_by: None,
        resolved_at: None,
    }
}
