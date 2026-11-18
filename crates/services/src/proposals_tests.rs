// SPDX-License-Identifier: Apache-2.0
use super::*;
use ports::bridge::WorkCall;
use protocol::work_cards::{AutoStart, TaskOrigin};

fn work(title: &str) -> NewWork {
    NewWork {
        title: title.into(),
        brief: format!("Do {title}"),
        mode: TaskMode::Work,
        issue: None,
        needs: None,
        refused: None,
        after: None,
        all_issues: false,
    }
}

fn outcome(work: Vec<NewWork>) -> RunOutcome {
    RunOutcome {
        summary: Some("Finished".into()),
        changed_files: vec![],
        checks: vec![],
        still_active: vec![],
        unknowns: vec![],
        usage: None,
        report: Some(RunReport {
            status: ReportStatus::Done,
            questions: vec![],
            remaining: vec![],
            new_work: work,
        }),
        verification: None,
    }
}

async fn chat(f: &Fixture, project: ProjectId) -> (TaskId, RunId) {
    let (id, _) = f
        .task(
            project,
            TaskPlacement::StartNow,
            &Caller::local_owner(),
            false,
        )
        .await;
    let task: Task = f.hub.read(|r| r.require("task", id)).unwrap();
    let run = task.current_run.unwrap();
    f.hub
        .write(|tx| {
            let mut task: Task = tx.read().require("task", id)?;
            task.conversation = true;
            tx.put("task", id, &task)
        })
        .unwrap();
    (id, run)
}

fn propose(tasks: Vec<serde_json::Value>, in_order: bool) -> WorkCall {
    WorkCall::Propose {
        tasks,
        in_order,
        all_issues: false,
    }
}

async fn suggest(
    f: &Fixture,
    chat: TaskId,
    run: RunId,
    call: WorkCall,
) -> Result<String, ProtocolError> {
    let task: Task = f.hub.read(|r| r.require("task", chat)).unwrap();
    f.hub.work_request(run, &task, call).await
}

fn reported(f: &Fixture, run: RunId) -> Vec<NewWork> {
    let run: Run = f.hub.read(|r| r.require("run", run)).unwrap();
    run.outcome.unwrap().report.unwrap().new_work
}

fn card(project: ProjectId, origin: TaskOrigin, w: &NewWork, placement: TaskPlacement) -> Command {
    Command::CreateTask {
        origin: Some(origin),
        from_chat: None,
        project_id: project,
        kind: TaskKind::Coding,
        mode: Some(TaskMode::Work),
        title: Some(w.title.clone()),
        prompt: w.brief.clone(),
        bundle: None,
        harness: HarnessKind::ClaudeCode,
        model: None,
        effort: None,
        launch_policy: None,
        branch: None,
        start: None,
        place: None,
        browser: None,
        github: None,
        mission: None,
        target_id: None,
        placement,
        agent_chosen: Some(true),
        conversation: None,
        access: None,
        review: None,
    }
}

#[tokio::test]
async fn proposed_work_leads_the_report_and_survives_an_empty_outcome() {
    let f = Fixture::new().await;
    let project = f.project().await;
    let (chat, run) = chat(&f, project).await;
    suggest(
        &f,
        chat,
        run,
        propose(
            vec![
                serde_json::json!({"title":"First", "brief":"One"}),
                serde_json::json!({"title":"Second", "brief":"Two"}),
            ],
            true,
        ),
    )
    .await
    .unwrap();
    f.hub
        .finish_run(
            run,
            RunState::Completed,
            Some(outcome(vec![work(" second "), work("Third")])),
        )
        .unwrap();
    let cards = reported(&f, run);
    assert_eq!(
        cards.iter().map(|w| w.title.as_str()).collect::<Vec<_>>(),
        ["First", "Second", "Third"]
    );
    assert_eq!(
        cards.iter().map(|w| w.after).collect::<Vec<_>>(),
        [None, Some(0), None]
    );
    assert!(
        f.hub
            .read(|r| r.get::<Vec<NewWork>>("proposed", run))
            .unwrap()
            .is_none()
    );
    let (other, next) = self::chat(&f, project).await;
    suggest(
        &f,
        other,
        next,
        propose(vec![serde_json::json!("Still here")], false),
    )
    .await
    .unwrap();
    f.hub.finish_run(next, RunState::Completed, None).unwrap();
    assert_eq!(reported(&f, next)[0].title, "Still here");
    f.close().await;
}

#[tokio::test]
async fn proposals_refuse_non_chats_invalid_tasks_and_excess_work() {
    let f = Fixture::new().await;
    let project = f.project().await;
    let (chat, run) = chat(&f, project).await;
    let mut ordinary: Task = f.hub.read(|r| r.require("task", chat)).unwrap();
    ordinary.conversation = false;
    let error = f
        .hub
        .work_request(
            run,
            &ordinary,
            propose(vec![serde_json::json!("Task")], false),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::PermissionDenied);
    for count in [0, 9] {
        let error = suggest(
            &f,
            chat,
            run,
            propose(vec![serde_json::json!("Task"); count], false),
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidRequest);
    }
    let error = suggest(
        &f,
        chat,
        run,
        propose(vec![serde_json::json!({"brief":" "})], false),
    )
    .await
    .unwrap_err();
    assert_eq!(error.message, "Task 1 needs a title and a brief.");
    suggest(
        &f,
        chat,
        run,
        propose(vec![serde_json::json!("Task"); 5], false),
    )
    .await
    .unwrap();
    let error = suggest(
        &f,
        chat,
        run,
        propose(vec![serde_json::json!("More"); 4], false),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidRequest);
    let error = suggest(
        &f,
        chat,
        run,
        WorkCall::Propose {
            tasks: vec![serde_json::json!("Task")],
            in_order: false,
            all_issues: true,
        },
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidRequest);
    let error = suggest(
        &f,
        chat,
        run,
        WorkCall::Propose {
            tasks: vec![],
            in_order: false,
            all_issues: true,
        },
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidRequest);
    f.close().await;
}

#[tokio::test]
async fn ordered_cards_wait_for_the_previous_task_and_phone_cannot_claim_auto_start() {
    let f = Fixture::new().await;
    let project = f.project().await;
    let (chat, run) = chat(&f, project).await;
    suggest(
        &f,
        chat,
        run,
        propose(
            vec![serde_json::json!("First"), serde_json::json!("Second")],
            true,
        ),
    )
    .await
    .unwrap();
    f.hub.finish_run(run, RunState::Completed, None).unwrap();
    let cards = reported(&f, run);
    let origin = |index| TaskOrigin {
        chat,
        run,
        index,
        auto: false,
        reviewed: false,
    };
    let queue = || TaskPlacement::Queue {
        position: None,
        authorization: QueueAuthorization::AskBeforeStart,
    };
    let owner = Caller::local_owner();
    let later = card(project, origin(1), &cards[1], queue());
    let refused = f
        .hub
        .submit(&owner, CommandEnvelope::new(later.clone()))
        .await
        .error
        .unwrap();
    assert_eq!(refused.code, ErrorCode::Conflict);
    assert!(refused.message.contains("Start that first"), "{refused:?}");
    let phone = f.device().await;
    let auto = TaskOrigin {
        auto: true,
        reviewed: false,
        ..origin(0)
    };
    let refused = f
        .hub
        .submit(
            &phone,
            CommandEnvelope::new(card(project, auto, &cards[0], queue())),
        )
        .await
        .error
        .unwrap();
    assert_eq!(refused.code, ErrorCode::PermissionDenied, "{refused:?}");
    let first = f
        .hub
        .submit(
            &owner,
            CommandEnvelope::new(card(project, origin(0), &cards[0], queue())),
        )
        .await;
    assert_eq!(first.error, None, "{first:?}");
    let refused = f
        .hub
        .submit(
            &owner,
            CommandEnvelope::new(card(project, origin(1), &cards[1], TaskPlacement::StartNow)),
        )
        .await
        .error
        .unwrap();
    assert_eq!(refused.code, ErrorCode::Conflict);
    assert!(refused.message.contains("Put it in Next up"), "{refused:?}");
    let second = f.hub.submit(&owner, CommandEnvelope::new(later)).await;
    assert_eq!(second.error, None, "{second:?}");
    let items: QueueView = f.hub.read(|r| r.require("queue", project)).unwrap();
    let first_id = match first.result.unwrap() {
        CommandResult::TaskCreated { task_id, .. } => task_id,
        other => panic!("{other:?}"),
    };
    let second_id = match second.result.unwrap() {
        CommandResult::TaskCreated { task_id, .. } => task_id,
        other => panic!("{other:?}"),
    };
    let previous = items.items.iter().find(|i| i.task_id == first_id).unwrap();
    let follower = items.items.iter().find(|i| i.task_id == second_id).unwrap();
    assert_eq!(follower.depends_on, vec![previous.item_id]);
    f.close().await;
}

#[tokio::test]
async fn owner_can_set_and_clear_auto_start_only_with_valid_limits() {
    let f = Fixture::new().await;
    let project = f.project().await;
    let owner = Caller::local_owner();
    for invalid in [
        AutoStart {
            per_reply: 0,
            at_once: 4,
        },
        AutoStart {
            per_reply: 4,
            at_once: 9,
        },
    ] {
        let receipt = f
            .hub
            .submit(
                &owner,
                CommandEnvelope::new(Command::SetAutoStart {
                    project_id: project,
                    auto_start: Some(invalid),
                }),
            )
            .await;
        assert_eq!(receipt.error.unwrap().code, ErrorCode::InvalidRequest);
    }
    let limits = AutoStart {
        per_reply: 4,
        at_once: 4,
    };
    let set = f
        .hub
        .submit(
            &owner,
            CommandEnvelope::new(Command::SetAutoStart {
                project_id: project,
                auto_start: Some(limits),
            }),
        )
        .await;
    assert_eq!(set.error, None, "{set:?}");
    let saved: Project = f.hub.read(|r| r.require("project", project)).unwrap();
    assert_eq!(saved.auto_start, Some(limits));
    let off = f
        .hub
        .submit(
            &owner,
            CommandEnvelope::new(Command::SetAutoStart {
                project_id: project,
                auto_start: None,
            }),
        )
        .await;
    assert_eq!(off.error, None, "{off:?}");
    let saved: Project = f.hub.read(|r| r.require("project", project)).unwrap();
    assert_eq!(saved.auto_start, None);
    f.close().await;
}

#[tokio::test]
async fn trusted_project_queues_an_ordered_proposal_without_asking() {
    let f = Fixture::new().await;
    let project = f.project().await;
    let owner = Caller::local_owner();
    let receipt = f
        .hub
        .submit(
            &owner,
            CommandEnvelope::new(Command::SetAutoStart {
                project_id: project,
                auto_start: Some(AutoStart {
                    per_reply: 4,
                    at_once: 4,
                }),
            }),
        )
        .await;
    assert_eq!(receipt.error, None, "{receipt:?}");
    let (chat, run) = chat(&f, project).await;
    // The chat keeps the project busy while its turn finishes; nothing can
    // dispatch before both proposed tasks have reached Next up.
    let q: QueueView = f.hub.read(|r| r.require("queue", project)).unwrap();
    let resume = f
        .hub
        .submit(
            &owner,
            edit(
                Command::QueueResume {
                    project_id: project,
                },
                q.revision,
            ),
        )
        .await;
    assert_eq!(resume.error, None, "{resume:?}");
    suggest(
        &f,
        chat,
        run,
        propose(
            vec![serde_json::json!("First"), serde_json::json!("Second")],
            true,
        ),
    )
    .await
    .unwrap();
    f.hub.finish_run(run, RunState::Completed, None).unwrap();
    let tasks = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let tasks: Vec<Task> = f.hub.read(|r| r.list("task")).unwrap();
            let made: Vec<Task> = tasks
                .into_iter()
                .filter(|t| t.origin.as_ref().is_some_and(|o| o.auto && o.run == run))
                .collect();
            if made.len() == 2 {
                break made;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("both suggestions should start from the trusted chat");
    assert!(tasks.iter().all(|t| t.origin.as_ref().unwrap().auto));
    let queue: QueueView = f.hub.read(|r| r.require("queue", project)).unwrap();
    let first = tasks
        .iter()
        .find(|t| t.origin.as_ref().unwrap().index == 0)
        .unwrap();
    let second = tasks
        .iter()
        .find(|t| t.origin.as_ref().unwrap().index == 1)
        .unwrap();
    let before = queue
        .items
        .iter()
        .find(|i| i.task_id == first.task_id)
        .unwrap();
    let after = queue
        .items
        .iter()
        .find(|i| i.task_id == second.task_id)
        .unwrap();
    assert_eq!(after.depends_on, vec![before.item_id]);
    f.close().await;
}

#[tokio::test]
async fn automatic_work_waits_when_the_person_has_not_allowed_it() {
    for reason in [
        "off",
        "paused",
        "held",
        "too_large",
        "at_once",
        "other_project",
        "whole_computer",
        "failed",
    ] {
        let f = Fixture::new().await;
        let project = f.project().await;
        let owner = Caller::local_owner();
        if reason != "off" {
            let limits = AutoStart {
                per_reply: if reason == "too_large" { 1 } else { 4 },
                at_once: if reason == "at_once" { 1 } else { 4 },
            };
            let receipt = f
                .hub
                .submit(
                    &owner,
                    CommandEnvelope::new(Command::SetAutoStart {
                        project_id: project,
                        auto_start: Some(limits),
                    }),
                )
                .await;
            assert_eq!(receipt.error, None, "{reason}: {receipt:?}");
        }
        let (chat, run) = chat(&f, project).await;
        if reason != "paused" {
            let q: QueueView = f.hub.read(|r| r.require("queue", project)).unwrap();
            let receipt = f
                .hub
                .submit(
                    &owner,
                    edit(
                        Command::QueueResume {
                            project_id: project,
                        },
                        q.revision,
                    ),
                )
                .await;
            assert_eq!(receipt.error, None, "{reason}: {receipt:?}");
        }
        if reason == "held" {
            f.hub
                .write(|tx| {
                    let mut task: Task = tx.read().require("task", chat)?;
                    task.access = Some(protocol::access::ConversationAccess {
                        read: vec![project],
                        ..Default::default()
                    });
                    tx.put("task", chat, &task)
                })
                .unwrap();
        }
        let needs = match reason {
            "other_project" => {
                serde_json::json!({"title":"First", "brief":"First", "needs":{"read":["Other project"]}})
            }
            "whole_computer" => {
                serde_json::json!({"title":"First", "brief":"First", "needs":{"whole_computer":true}})
            }
            _ => serde_json::json!("First"),
        };
        let tasks = if matches!(reason, "too_large" | "at_once") {
            vec![needs, serde_json::json!("Second")]
        } else {
            vec![needs]
        };
        let count = tasks.len();
        suggest(&f, chat, run, propose(tasks, true)).await.unwrap();
        f.hub
            .finish_run(
                run,
                if reason == "failed" {
                    RunState::Failed
                } else {
                    RunState::Completed
                },
                None,
            )
            .unwrap();
        if reason != "failed" {
            // The spawned attempt and this explicit attempt share the planning lock.
            f.hub.start_proposed(run, vec![true; count]).await.unwrap();
        } else {
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
        let made: Vec<Task> = f.hub.read(|r| r.list("task")).unwrap();
        assert!(
            !made
                .iter()
                .any(|t| t.origin.as_ref().is_some_and(|o| o.auto && o.run == run)),
            "{reason}"
        );
        assert_eq!(
            reported(&f, run).len(),
            count,
            "{reason}: the cards still appear"
        );
        f.close().await;
    }
}

/// Lets `project`'s chats start what they propose, within `per_reply` and
/// `at_once`, and resumes its Next up.
async fn trust(f: &Fixture, project: ProjectId, per_reply: u32, at_once: u32) {
    let owner = Caller::local_owner();
    let receipt = f
        .hub
        .submit(
            &owner,
            CommandEnvelope::new(Command::SetAutoStart {
                project_id: project,
                auto_start: Some(AutoStart { per_reply, at_once }),
            }),
        )
        .await;
    assert_eq!(receipt.error, None, "{receipt:?}");
    let q: QueueView = f.hub.read(|r| r.require("queue", project)).unwrap();
    let resume = f
        .hub
        .submit(
            &owner,
            edit(
                Command::QueueResume {
                    project_id: project,
                },
                q.revision,
            ),
        )
        .await;
    assert_eq!(resume.error, None, "{resume:?}");
}

/// What run `run` started without asking, once its start is over.
async fn auto_started(f: &Fixture, run: RunId) -> Vec<Task> {
    // An attempt holds the planning lock to its end; one after the turn's
    // own has nothing left to start.
    drop(f.hub.inner.planning.lock().await);
    let tasks: Vec<Task> = f.hub.read(|r| r.list("task")).unwrap();
    tasks
        .into_iter()
        .filter(|t| t.origin.as_ref().is_some_and(|o| o.auto && o.run == run))
        .collect()
}

/// The chat's turn after `before`, once it has begun.
async fn next_turn(f: &Fixture, chat: TaskId, before: RunId) -> RunId {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let task: Task = f.hub.read(|r| r.require("task", chat)).unwrap();
            if let Some(run) = task.current_run.filter(|r| *r != before) {
                return run;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the chat should start another turn")
}

/// The person resumes chat `chat` with a message of their own.
async fn person_says(f: &Fixture, chat: TaskId, read_only: bool) -> RunId {
    let task: Task = f.hub.read(|r| r.require("task", chat)).unwrap();
    let before = task.current_run.unwrap();
    let receipt = f
        .hub
        .submit(
            &Caller::local_owner(),
            edit(
                Command::ResumeTask {
                    observed_reply: None,
                    harness: None,
                    mode: None,
                    task_id: chat,
                    text: Some("Go on".into()),
                    bundle: None,
                    carried_plan: None,
                    read_only,
                    model: None,
                    effort: None,
                },
                task.revision,
            ),
        )
        .await;
    assert_eq!(receipt.error, None, "{receipt:?}");
    next_turn(f, chat, before).await
}

/// Ends run `run` well, and lets the turn's own start attempt run.
async fn finish(f: &Fixture, run: RunId) -> Vec<Task> {
    f.hub.finish_run(run, RunState::Completed, None).unwrap();
    // The turn's attempt, if it makes one, is spawned as it ends; wait for
    // it to take the planning lock or to have been skipped.
    tokio::time::sleep(Duration::from_millis(50)).await;
    auto_started(f, run).await
}

#[tokio::test]
async fn only_a_turn_the_person_started_starts_work_by_itself() {
    use crate::followups::Followup;
    for kind in ["wake", "helper_reply", "checklist"] {
        let f = Fixture::new().await;
        let project = f.project().await;
        trust(&f, project, 4, 4).await;
        let (chat, first) = chat(&f, project).await;
        assert!(finish(&f, first).await.is_empty());
        let now = f.hub.inner.clock.now();
        let followup = match kind {
            "wake" => Followup::wake(chat, "Time to check".into(), now, now),
            "helper_reply" => {
                Followup::reply(chat, TaskId::new(), "Here's what I found".into(), now)
            }
            _ => Followup::checklist("checklist:test".into(), chat, "Ticked: tests".into(), now),
        };
        f.hub.write(|tx| followup.put(tx)).unwrap();
        f.hub.pass_on_followups().await.unwrap();
        let run = next_turn(&f, chat, first).await;
        let said = suggest(
            &f,
            chat,
            run,
            propose(vec![serde_json::json!("First")], false),
        )
        .await
        .unwrap();
        assert!(said.contains("nothing will by itself"), "{kind}: {said}");
        assert!(!said.contains("may start by itself"), "{kind}: {said}");
        assert!(finish(&f, run).await.is_empty(), "{kind}");
        // Even told otherwise, the end of such a turn starts nothing.
        f.hub.start_proposed(run, vec![true]).await.unwrap();
        assert!(auto_started(&f, run).await.is_empty(), "{kind}");
        assert_eq!(reported(&f, run).len(), 1, "{kind}: its card still appears");

        // The person's own message, in the same chat, still starts work.
        let run = person_says(&f, chat, false).await;
        let said = suggest(
            &f,
            chat,
            run,
            propose(vec![serde_json::json!("Second")], false),
        )
        .await
        .unwrap();
        assert!(said.contains("may start by itself"), "{kind}: {said}");
        let started = tokio::time::timeout(Duration::from_secs(10), async {
            f.hub.finish_run(run, RunState::Completed, None).unwrap();
            loop {
                let made = auto_started(&f, run).await;
                if !made.is_empty() {
                    break made;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the person's turn should start what it proposed");
        assert_eq!(started.len(), 1, "{kind}");
        f.close().await;
    }
}

#[tokio::test]
async fn a_read_only_turn_of_an_older_chat_starts_nothing_by_itself() {
    for mode in [None, Some(TaskMode::Plan)] {
        let f = Fixture::new().await;
        let project = f.project().await;
        trust(&f, project, 4, 4).await;
        let (chat, first) = chat(&f, project).await;
        assert!(finish(&f, first).await.is_empty());
        // Older chats kept no access of their own; this one may plan only.
        f.hub
            .write(|tx| {
                let mut infos: Vec<protocol::capability::HarnessInfo> =
                    tx.read().require("meta", "harnesses")?;
                infos[0].modes = [TaskMode::Ask, TaskMode::Plan, TaskMode::Work]
                    .into_iter()
                    .map(|mode| protocol::capability::ModeSupport {
                        mode,
                        availability: protocol::capability::Availability::Available,
                        enforcement: "Synthetic fixture restriction".into(),
                    })
                    .collect();
                tx.put("meta", "harnesses", &infos)?;
                let mut turn: Run = tx.read().require("run", first)?;
                turn.native_session = Some("older-chat-session".into());
                tx.put("run", first, &turn)?;
                let mut task: Task = tx.read().require("task", chat)?;
                task.access = None;
                if let Some(mode) = mode {
                    task.mode = mode;
                }
                tx.put("task", chat, &task)
            })
            .unwrap();
        // An Ask turn, or a Plan chat's turn: the person's own, but read-only.
        let run = person_says(&f, chat, mode.is_none()).await;
        let turn: Run = f.hub.read(|r| r.require("run", run)).unwrap();
        assert_ne!(turn.mode, Some(TaskMode::Work), "{mode:?}");
        let said = suggest(
            &f,
            chat,
            run,
            propose(vec![serde_json::json!("First")], false),
        )
        .await
        .unwrap();
        assert!(said.contains("nothing will by itself"), "{mode:?}: {said}");
        assert!(finish(&f, run).await.is_empty(), "{mode:?}");
        f.hub.start_proposed(run, vec![true]).await.unwrap();
        assert!(auto_started(&f, run).await.is_empty(), "{mode:?}");
        f.close().await;
    }
}

#[tokio::test]
async fn one_reply_starts_no_more_than_its_limit_however_it_proposes() {
    let f = Fixture::new().await;
    let project = f.project().await;
    trust(&f, project, 1, 8).await;
    let (chat, run) = chat(&f, project).await;
    let eight = (1..=8)
        .map(|n| serde_json::json!(format!("Task {n}")))
        .collect();
    suggest(&f, chat, run, propose(eight, false)).await.unwrap();
    f.hub.finish_run(run, RunState::Completed, None).unwrap();
    let started = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let made = auto_started(&f, run).await;
            if !made.is_empty() {
                break made;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the first should start");
    // The attempt that made it held the planning lock until it was done.
    assert_eq!(started.len(), 1, "exactly one of the eight starts");
    assert_eq!(started[0].origin.as_ref().unwrap().index, 0);
    assert_eq!(reported(&f, run).len(), 8, "the rest wait on their cards");
    f.close().await;
}

#[tokio::test]
async fn what_doesnt_fit_holds_back_everything_proposed_after_it() {
    let f = Fixture::new().await;
    let project = f.project().await;
    trust(&f, project, 2, 8).await;
    let (chat, run) = chat(&f, project).await;
    let calls = [
        // Needs the person's tap: passed over.
        propose(
            vec![
                serde_json::json!({"title":"Local", "brief":"Local", "needs":{"local_network":true}}),
            ],
            false,
        ),
        propose(vec![serde_json::json!("Alone")], false),
        // Two in order, with one left of the reply's two: it doesn't fit.
        propose(
            vec![serde_json::json!("Then one"), serde_json::json!("Then two")],
            true,
        ),
        // Would fit, but doesn't jump ahead of the set before it.
        propose(vec![serde_json::json!("Last")], false),
    ];
    for call in calls {
        suggest(&f, chat, run, call).await.unwrap();
    }
    f.hub.finish_run(run, RunState::Completed, None).unwrap();
    let started = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let made = auto_started(&f, run).await;
            if !made.is_empty() {
                break made;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("one should start");
    assert_eq!(
        started.iter().map(|t| t.title.as_str()).collect::<Vec<_>>(),
        ["Alone"]
    );
    f.close().await;
}

/// Waits until `check` holds of the store.
async fn until(f: &Fixture, what: &str, check: impl Fn(&store::Read<'_>) -> bool) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if f.hub.read(|r| Ok(check(r))).unwrap() {
                return;
            }
            f.hub.progress_queues();
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{what}"));
}

#[tokio::test]
async fn a_failed_step_lets_go_of_the_rest_of_its_set_in_next_up() {
    let f = Fixture::new().await;
    let project = f.project().await;
    trust(&f, project, 4, 4).await;
    let (chat, run) = chat(&f, project).await;
    suggest(
        &f,
        chat,
        run,
        propose(
            vec![
                serde_json::json!("First"),
                serde_json::json!("Second"),
                serde_json::json!("Third"),
            ],
            true,
        ),
    )
    .await
    .unwrap();
    f.hub.finish_run(run, RunState::Completed, None).unwrap();
    let step = |index: u32| {
        let tasks: Vec<Task> = f.hub.read(|r| r.list("task")).unwrap();
        tasks.into_iter().find(|t| {
            t.origin
                .as_ref()
                .is_some_and(|o| o.run == run && o.index == index)
        })
    };
    until(&f, "the first step should start", |r| {
        (r.list::<Task>("task").unwrap().into_iter()).any(|t| {
            t.origin
                .as_ref()
                .is_some_and(|o| o.run == run && o.index == 0)
                && t.current_run.is_some()
        })
    })
    .await;
    assert!(step(2).is_some(), "the whole set waits in Next up");
    let first = step(0).unwrap();
    let first_run = first.current_run.unwrap();
    run_started(&f, first.task_id).await;
    f.hub.finish_run(first_run, RunState::Failed, None).unwrap();
    until(&f, "the rest of the set should be put away", |r| {
        let q: QueueView = r.require("queue", project).unwrap();
        !q.items.iter().any(|i| i.is_pending())
    })
    .await;
    for index in [1, 2] {
        let t = step(index).unwrap();
        assert_eq!(t.state, TaskState::Archived, "{index}");
        assert_eq!(t.current_run, None, "{index}");
        assert_eq!(
            t.status_line.as_deref(),
            Some("Not started: “First” didn't succeed"),
            "{index}"
        );
    }
    let cards = reported(&f, run);
    let owner = Caller::local_owner();
    let origin = |index| TaskOrigin {
        chat,
        run,
        index,
        auto: false,
        reviewed: false,
    };
    // A failed Work run pauses Next up, as it always has; the put-away steps
    // no longer wait there in front of anything once the person resumes.
    let q: QueueView = f.hub.read(|r| r.require("queue", project)).unwrap();
    assert!(q.paused);
    // In order, a later step won't start after the one that failed...
    let refused = f
        .hub
        .submit(
            &owner,
            CommandEnvelope::new(card(
                project,
                origin(1),
                &cards[1],
                TaskPlacement::Queue {
                    position: None,
                    authorization: QueueAuthorization::AskBeforeStart,
                },
            )),
        )
        .await
        .error
        .unwrap();
    assert_eq!(refused.code, ErrorCode::Conflict);
    assert!(
        refused.message.contains("“First” didn't succeed"),
        "{refused:?}"
    );
    // ...but the person may start it by hand, on its own.
    let again = f
        .hub
        .submit(
            &owner,
            CommandEnvelope::new(card(project, origin(1), &cards[1], TaskPlacement::StartNow)),
        )
        .await;
    assert_eq!(again.error, None, "{again:?}");
    let restarted = match again.result.unwrap() {
        CommandResult::TaskCreated { task_id, .. } => task_id,
        other => panic!("{other:?}"),
    };
    let restarted: Task = f.hub.read(|r| r.require("task", restarted)).unwrap();
    assert!(restarted.current_run.is_some());
    // A step that did start isn't started twice.
    let twice = f
        .hub
        .submit(
            &owner,
            CommandEnvelope::new(card(project, origin(0), &cards[0], TaskPlacement::StartNow)),
        )
        .await
        .error
        .unwrap();
    assert!(twice.message.contains("already started"), "{twice:?}");
    f.close().await;
}

#[tokio::test]
async fn a_step_the_person_took_out_lets_go_of_what_follows_it() {
    let f = Fixture::new().await;
    let project = f.project().await;
    let (chat, run) = chat(&f, project).await;
    suggest(
        &f,
        chat,
        run,
        propose(
            vec![
                serde_json::json!("First"),
                serde_json::json!("Second"),
                serde_json::json!("Third"),
            ],
            true,
        ),
    )
    .await
    .unwrap();
    f.hub.finish_run(run, RunState::Completed, None).unwrap();
    let cards = reported(&f, run);
    let owner = Caller::local_owner();
    let mut made = Vec::new();
    // Start all, from the card: each waits in Next up for the one before.
    for (index, w) in cards.iter().enumerate() {
        let origin = TaskOrigin {
            chat,
            run,
            index: index as u32,
            auto: false,
            reviewed: false,
        };
        let receipt = f
            .hub
            .submit(
                &owner,
                CommandEnvelope::new(card(
                    project,
                    origin,
                    w,
                    TaskPlacement::Queue {
                        position: None,
                        authorization: QueueAuthorization::AskBeforeStart,
                    },
                )),
            )
            .await;
        match receipt.result {
            Some(CommandResult::TaskCreated {
                task_id,
                queue_item_id: Some(item),
            }) => made.push((task_id, item)),
            ref other => panic!("{other:?} {:?}", receipt.error),
        }
    }
    let q: QueueView = f.hub.read(|r| r.require("queue", project)).unwrap();
    let removed = f
        .hub
        .submit(
            &owner,
            edit(Command::QueueRemove { item_id: made[0].1 }, q.revision),
        )
        .await;
    assert_eq!(removed.error, None, "{removed:?}");
    until(&f, "what followed it should be put away", |r| {
        let q: QueueView = r.require("queue", project).unwrap();
        q.items.is_empty()
    })
    .await;
    for (task_id, _) in &made[1..] {
        let t: Task = f.hub.read(|r| r.require("task", *task_id)).unwrap();
        assert_eq!(t.state, TaskState::Archived);
        assert_eq!(
            t.status_line.as_deref(),
            Some("Not started: “First” was taken out of Next up")
        );
    }
    f.close().await;
}
