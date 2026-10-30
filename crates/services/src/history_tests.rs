// SPDX-License-Identifier: Apache-2.0
use super::*;
use std::collections::{HashMap, HashSet};

const FRAME: usize = 1024 * 1024;

/// Task and run records to copy from, as the host writes them.
struct Template {
    task: Task,
    run: Run,
}
impl Template {
    async fn new(f: &Fixture, project: ProjectId) -> Self {
        let (task_id, _) = f
            .task(
                project,
                TaskPlacement::StartNow,
                &Caller::local_owner(),
                false,
            )
            .await;
        let task: Task = f.hub.read(|r| r.require("task", task_id)).unwrap();
        let run = f
            .hub
            .read(|r| r.require("run", task.current_run.unwrap()))
            .unwrap();
        Self { task, run }
    }
    /// A task changed at `at`, whose one run ended with `diff` bytes of
    /// reported changes, and the item raised for its review.
    fn record(
        &self,
        f: &Fixture,
        project: ProjectId,
        state: TaskState,
        at: i64,
        diff: usize,
    ) -> Task {
        let mut task = self.task.clone();
        let mut run = self.run.clone();
        task.task_id = TaskId::new();
        task.project_id = project;
        task.state = state;
        task.updated_at = Timestamp(at);
        run.run_id = RunId::new();
        run.task_id = task.task_id;
        run.state = RunState::Completed;
        run.ended_at = Some(Timestamp(at));
        run.holds_writer = false;
        run.outcome = Some(RunOutcome {
            summary: Some(format!("Finished at {at}")),
            changed_files: vec![ChangedFile {
                path: "src/lib.rs".into(),
                change: FileChange::Modified,
                added: Some(1),
                removed: Some(1),
                diff: Some(ReportedDiff {
                    text: "+".repeat(diff),
                    truncated: false,
                }),
                provenance: Some(Provenance::Native),
            }],
            checks: vec![],
            still_active: vec![],
            unknowns: vec![],
            usage: None,
            report: None,
            verification: None,
        });
        task.current_run = Some(run.run_id);
        let review = AttentionItem {
            child_id: None,
            attention_id: AttentionId::new(),
            kind: AttentionKind::Completed,
            title: task.title.clone(),
            body: None,
            task_id: Some(task.task_id),
            run_id: Some(run.run_id),
            approval_id: None,
            created_at: Timestamp(at),
            resolved_at: None,
            priority: 5,
            topic: None,
            quiet: false,
            report_id: None,
        };
        f.hub
            .write(|tx| {
                tx.put("task", task.task_id, &task)?;
                tx.put("run", run.run_id, &run)?;
                tx.put("attention", review.attention_id, &review)
            })
            .unwrap();
        task
    }
    /// `count` earlier runs of `task`, each ended with a `summary` bytes long
    /// and its review resolved, oldest first.
    fn earlier_runs(&self, f: &Fixture, task: &Task, count: i64, summary: usize) -> Vec<Run> {
        (0..count)
            .map(|n| {
                let mut run = self.run.clone();
                run.run_id = RunId::new();
                run.task_id = task.task_id;
                run.state = RunState::Completed;
                run.admitted_at = Timestamp(n);
                run.ended_at = Some(Timestamp(n));
                run.holds_writer = false;
                run.outcome = Some(RunOutcome {
                    summary: Some("s".repeat(summary)),
                    changed_files: vec![],
                    checks: vec![],
                    still_active: vec![],
                    unknowns: vec![],
                    usage: None,
                    report: None,
                    verification: None,
                });
                let review = AttentionItem {
                    child_id: None,
                    attention_id: AttentionId::new(),
                    kind: AttentionKind::Completed,
                    title: task.title.clone(),
                    body: None,
                    task_id: Some(task.task_id),
                    run_id: Some(run.run_id),
                    approval_id: None,
                    created_at: Timestamp(n),
                    resolved_at: Some(Timestamp(n)),
                    priority: 5,
                    topic: None,
                    quiet: false,
                    report_id: None,
                };
                f.hub
                    .write(|tx| {
                        tx.put("run", run.run_id, &run)?;
                        tx.put("attention", review.attention_id, &review)
                    })
                    .unwrap();
                run
            })
            .collect()
    }
}

fn approval(task: &Task, run: RunId, state: ApprovalState) -> Approval {
    Approval {
        approval_id: ApprovalId::new(),
        run_id: run,
        task_id: task.task_id,
        project_id: task.project_id,
        native_request_id: "native".into(),
        kind: ApprovalKind::Command,
        title: "Run the tests".into(),
        detail: None,
        detail_lines: vec![],
        questions: vec![],
        raw_request: None,
        scope: "This one command".into(),
        shortened: false,
        revision: 1,
        state,
        delivery: None,
        outcome: None,
        requested_at: task.updated_at,
        expires_at: None,
        resolved_by: None,
        resolved_at: None,
    }
}

async fn query(f: &Fixture, query: Query) -> QueryData {
    f.hub.query(&Caller::local_owner(), query).await.unwrap()
}
/// A snapshot as a phone that reads withheld approvals asks for it.
async fn snapshot(f: &Fixture) -> Snapshot {
    snapshot_for(f, Some(true)).await
}
async fn snapshot_for(f: &Fixture, reads_withheld: Option<bool>) -> Snapshot {
    match query(f, Query::Snapshot { reads_withheld }).await {
        QueryData::Snapshot(snapshot) => {
            assert!(serde_json::to_vec(&snapshot).unwrap().len() < FRAME);
            *snapshot
        }
        _ => panic!("snapshot expected"),
    }
}

#[tokio::test]
async fn a_history_far_over_a_frame_loads_with_every_live_item() {
    let f = Fixture::new().await;
    let project = f.project().await;
    let template = Template::new(&f, project).await;
    // Live, but changed before any finished task: a person's decision, a
    // problem to check, helpers still finishing, and the dispatching task.
    let deciding = template.record(&f, project, TaskState::AwaitingDecision, 1, 10);
    let approval = approval(
        &deciding,
        deciding.current_run.unwrap(),
        ApprovalState::Pending,
    );
    let unknown = template.record(&f, project, TaskState::ReconciliationRequired, 2, 10);
    let check = AttentionItem {
        attention_id: AttentionId::new(),
        kind: AttentionKind::ReconciliationRequired,
        task_id: Some(unknown.task_id),
        ..f.hub
            .read(|r| r.list::<AttentionItem>("attention"))
            .unwrap()
            .into_iter()
            .find(|a| a.task_id == Some(unknown.task_id))
            .unwrap()
    };
    let finishing = template.record(&f, project, TaskState::Completed, 3, 10);
    // An Ask turn that ended before what it started was confirmed ended,
    // with only its review raised.
    let unconfirmed = template.record(&f, project, TaskState::Completed, 4, 10);
    f.hub
        .write(|tx| {
            tx.put("approval", approval.approval_id, &approval)?;
            tx.put("attention", check.attention_id, &check)?;
            let mut run: Run = tx.read().require("run", finishing.current_run.unwrap())?;
            run.holds_writer = true;
            tx.put("run", run.run_id, &run)?;
            let mut task: Task = tx.read().require("task", unconfirmed.task_id)?;
            task.mode = TaskMode::Ask;
            tx.put("task", task.task_id, &task)?;
            let mut run: Run = tx.read().require("run", unconfirmed.current_run.unwrap())?;
            run.cleanup_pending = true;
            tx.put("run", run.run_id, &run)
        })
        .unwrap();
    // Many times a frame of finished work, each run with a large diff, and
    // more than a frame even without them.
    let finished: Vec<Task> = (0..800)
        .map(|n| template.record(&f, project, TaskState::Completed, 1_000 + n, 16 * 1024))
        .collect();

    let snapshot = snapshot(&f).await;
    let carried: HashSet<TaskId> = snapshot.tasks.iter().map(|t| t.task_id).collect();
    for task in [
        &template.task,
        &deciding,
        &unknown,
        &finishing,
        &unconfirmed,
    ] {
        assert!(carried.contains(&task.task_id), "{:?}", task.state);
        let run = task.current_run.unwrap();
        assert!(snapshot.runs.iter().any(|r| r.run_id == run));
    }
    // Carried as the phone offers Stop for it.
    let run = unconfirmed.current_run.unwrap();
    assert!((snapshot.runs.iter()).any(|r| r.run_id == run && r.cleanup_pending));
    assert!(snapshot.approvals.contains(&approval));
    assert!(snapshot.attention.contains(&check));
    // The newest finished tasks follow, as many as fit, with their runs and
    // review items but not the diffs their agents reported.
    let newest: Vec<TaskId> = finished.iter().rev().map(|t| t.task_id).collect();
    let recent: Vec<TaskId> = newest
        .iter()
        .copied()
        .take_while(|id| carried.contains(id))
        .collect();
    assert!(recent.len() > 20, "{}", recent.len());
    assert!(
        (newest.iter().skip(recent.len())).all(|id| !carried.contains(id)),
        "finished tasks are carried newest first"
    );
    for id in &recent {
        assert!(snapshot.runs.iter().any(|r| r.task_id == *id));
        assert!(snapshot.attention.iter().any(|a| a.task_id == Some(*id)));
    }
    assert!(
        (snapshot.runs.iter().flat_map(|r| &r.outcome))
            .all(|o| o.changed_files.iter().all(|f| f.diff.is_none()))
    );
    let older = snapshot.older_tasks.unwrap();
    assert_eq!(older.from.task_id, newest[recent.len()]);
    assert_eq!(older.projects, vec![project]);
    // A run read whole keeps its diff.
    let run = finished[0].current_run.unwrap();
    let QueryData::Run { run, attention, .. } = query(&f, Query::Run { run_id: run }).await else {
        panic!("run expected")
    };
    assert_eq!(attention.len(), 1);
    let diff = &run.outcome.unwrap().changed_files[0].diff;
    assert_eq!(diff.as_ref().unwrap().text.len(), 16 * 1024);
    f.close().await;
}

#[tokio::test]
async fn paging_reaches_every_older_task_once() {
    let f = Fixture::new().await;
    let first = f.project().await;
    let second = ProjectId::new();
    let template = Template::new(&f, first).await;
    let mut all: Vec<Task> = (0..800)
        .map(|n| {
            let project = if n % 3 == 0 { second } else { first };
            // Several tasks changed in the same millisecond, told apart by id.
            template.record(&f, project, TaskState::Completed, 5_000 + n / 2, 1024)
        })
        .collect();
    let archived = template.record(&f, first, TaskState::Archived, 4_000, 1024);
    all.push(archived.clone());
    let snapshot = snapshot(&f).await;
    let older = snapshot.older_tasks.clone().unwrap();
    assert_eq!(older.projects, {
        let mut both = vec![first, second];
        both.sort();
        both
    });
    let mut seen: HashMap<TaskId, usize> = HashMap::new();
    let mut runs: HashMap<RunId, usize> = HashMap::new();
    for task in &snapshot.tasks {
        *seen.entry(task.task_id).or_default() += 1;
    }
    for run in &snapshot.runs {
        *runs.entry(run.run_id).or_default() += 1;
    }
    for project in older.projects {
        let mut from = Some(older.from);
        while let Some(cursor) = from {
            let QueryData::TaskHistory { records, next } = query(
                &f,
                Query::TaskHistory {
                    project_id: project,
                    from: cursor,
                    limit: 7,
                },
            )
            .await
            else {
                panic!("history expected")
            };
            assert!(!records.tasks.is_empty() && records.tasks.len() <= 7);
            assert!(records.tasks.iter().all(|t| t.project_id == project));
            assert!(
                (records.runs.iter().flat_map(|r| &r.outcome))
                    .all(|o| o.changed_files.iter().all(|f| f.diff.is_none()))
            );
            for task in &records.tasks {
                *seen.entry(task.task_id).or_default() += 1;
            }
            for run in &records.runs {
                *runs.entry(run.run_id).or_default() += 1;
            }
            from = next;
        }
    }
    for task in &all {
        assert_eq!(seen.get(&task.task_id), Some(&1), "{:?}", task.updated_at);
        assert_eq!(runs.get(&task.current_run.unwrap()), Some(&1));
    }
    // A task the phone does not hold can still be read on its own.
    let QueryData::Task { records } = query(
        &f,
        Query::Task {
            task_id: archived.task_id,
        },
    )
    .await
    else {
        panic!("task expected")
    };
    assert_eq!(records.tasks, vec![archived]);
    assert_eq!(records.runs.len(), 1);
    assert_eq!(records.attention.len(), 1);
    f.close().await;
}

#[tokio::test]
async fn a_long_task_carries_its_latest_runs_and_the_rest_are_read_by_id() {
    let f = Fixture::new().await;
    let project = f.project().await;
    let template = Template::new(&f, project).await;
    // A live conversation continued hundreds of times, well over a frame of
    // runs even without diffs, with a decision on its first run.
    let long = template.record(&f, project, TaskState::Working, 10_000, 10);
    let earlier = template.earlier_runs(&f, &long, 300, 4 * 1024);
    let decided = approval(&long, earlier[0].run_id, ApprovalState::Allowed);
    f.hub
        .write(|tx| tx.put("approval", decided.approval_id, &decided))
        .unwrap();
    let snapshot = snapshot(&f).await;
    assert!(snapshot.tasks.contains(&long));
    let carried: Vec<&Run> = (snapshot.runs.iter())
        .filter(|r| r.task_id == long.task_id)
        .collect();
    assert!(carried.iter().any(|r| Some(r.run_id) == long.current_run));
    assert!(
        carried.len() > 40 && carried.len() < 100,
        "{}",
        carried.len()
    );
    // The latest runs, and the records about those runs only.
    let latest: HashSet<RunId> = (earlier.iter().rev().take(carried.len() - 1))
        .map(|r| r.run_id)
        .collect();
    assert!(
        carried
            .iter()
            .all(|r| latest.contains(&r.run_id) || Some(r.run_id) == long.current_run)
    );
    assert!(!snapshot.approvals.contains(&decided));
    let QueryData::Run {
        run,
        approvals,
        attention,
        withheld_approvals,
    } = query(
        &f,
        Query::Run {
            run_id: earlier[0].run_id,
        },
    )
    .await
    else {
        panic!("run expected")
    };
    assert_eq!(*run, earlier[0]);
    assert_eq!(approvals, vec![decided]);
    assert!(withheld_approvals.is_empty());
    assert_eq!(attention.len(), 1);
    // Finished, it no longer holds up paging.
    let finished = template.record(&f, project, TaskState::Completed, 9_000, 10);
    template.earlier_runs(&f, &finished, 300, 4 * 1024);
    let older: Vec<Task> = (0..3)
        .map(|n| template.record(&f, project, TaskState::Completed, 8_000 - n, 10))
        .collect();
    let mut seen = Vec::new();
    let mut from = Some(TaskCursor {
        updated_at: Timestamp(9_000),
        task_id: finished.task_id,
    });
    while let Some(cursor) = from {
        let QueryData::TaskHistory { records, next } = query(
            &f,
            Query::TaskHistory {
                project_id: project,
                from: cursor,
                limit: 50,
            },
        )
        .await
        else {
            panic!("history expected")
        };
        let answer = QueryData::TaskHistory {
            records: records.clone(),
            next,
        };
        assert!(serde_json::to_vec(&answer).unwrap().len() < FRAME);
        seen.extend(records.tasks.into_iter().map(|t| t.task_id));
        from = next;
    }
    assert!(seen.contains(&finished.task_id));
    assert!(older.iter().all(|t| seen.contains(&t.task_id)));
    f.close().await;
}

#[tokio::test]
async fn many_long_live_tasks_share_one_budget_for_their_earlier_runs() {
    let f = Fixture::new().await;
    let project = f.project().await;
    let template = Template::new(&f, project).await;
    // Five live conversations, each with about a quarter of a frame of
    // earlier runs: more than a frame together.
    let long: Vec<Task> = (0..5)
        .map(|n| {
            let task = template.record(&f, project, TaskState::Working, 10_000 + n, 10);
            template.earlier_runs(&f, &task, 60, 4 * 1024);
            task
        })
        .collect();
    let pending = approval(
        &long[0],
        long[0].current_run.unwrap(),
        ApprovalState::Pending,
    );
    f.hub
        .write(|tx| tx.put("approval", pending.approval_id, &pending))
        .unwrap();
    let snapshot = snapshot(&f).await;
    for task in &long {
        assert!(snapshot.tasks.contains(task));
        assert!(
            snapshot
                .runs
                .iter()
                .any(|r| Some(r.run_id) == task.current_run)
        );
    }
    assert!(snapshot.approvals.contains(&pending));
    assert!(snapshot.older_tasks.is_some());
    f.close().await;
}

#[tokio::test]
async fn a_chat_connector_reads_every_task_within_the_companion() {
    let f = Fixture::new().await;
    let project = f.project().await;
    let template = Template::new(&f, project).await;
    let all: Vec<Task> = (0..800)
        .map(|n| template.record(&f, project, TaskState::Completed, 1_000 + n, 16))
        .collect();
    assert!(snapshot(&f).await.older_tasks.is_some());
    let id = ChannelIdentityId::new();
    f.hub
        .write(|tx| {
            tx.put(
                "channel",
                id,
                &ChannelIdentity {
                    id,
                    channel: ChannelKind::Telegram,
                    external_user: "1".into(),
                    external_chat: "2".into(),
                    display_name: "Synthetic channel".into(),
                    linked_at: Timestamp(0),
                    revoked_at: None,
                    grants: Grants::owner(),
                    project_id: None,
                },
            )
        })
        .unwrap();
    let chat = Caller {
        principal: Principal::Channel { identity_id: id },
        grants: Grants::owner(),
    };
    let QueryData::Snapshot(snapshot) = f
        .hub
        .query(
            &chat,
            Query::Snapshot {
                reads_withheld: None,
            },
        )
        .await
        .unwrap()
    else {
        panic!("snapshot expected")
    };
    let carried: HashSet<TaskId> = snapshot.tasks.iter().map(|t| t.task_id).collect();
    assert!(all.iter().all(|t| carried.contains(&t.task_id)));
    assert!(snapshot.older_tasks.is_none());
    f.close().await;
}

/// Pending approvals, each up to three quarters of a frame, that together
/// are far more than one.
fn large_approvals(f: &Fixture, task: &Task, count: usize) -> Vec<Approval> {
    approvals_of(f, task, count, 600 * 1024)
}

/// `count` pending approvals whose details are `bytes` long.
fn approvals_of(f: &Fixture, task: &Task, count: usize, bytes: usize) -> Vec<Approval> {
    (0..count)
        .map(|_| {
            let mut pending = approval(task, task.current_run.unwrap(), ApprovalState::Pending);
            pending.detail = Some("d".repeat(bytes));
            f.hub
                .write(|tx| tx.put("approval", pending.approval_id, &pending))
                .unwrap();
            pending
        })
        .collect()
}

#[tokio::test]
async fn pending_approvals_too_large_for_one_frame_are_named_and_read_one_at_a_time() {
    let f = Fixture::new().await;
    let project = f.project().await;
    let template = Template::new(&f, project).await;
    let deciding = template.record(&f, project, TaskState::AwaitingDecision, 1, 10);
    let mut pending = large_approvals(&f, &deciding, 3);
    let small = approval(
        &deciding,
        deciding.current_run.unwrap(),
        ApprovalState::Pending,
    );
    f.hub
        .write(|tx| tx.put("approval", small.approval_id, &small))
        .unwrap();
    pending.push(small.clone());
    // Each pending approval comes whole or is named, never both; some are
    // named, and one too large to fit keeps no smaller one out.
    let every = |approvals: &[Approval], withheld: &[ApprovalId]| {
        for approval in &pending {
            let carried = approvals.contains(approval);
            let named = withheld.contains(&approval.approval_id);
            assert!(carried != named, "carried {carried}, named {named}");
        }
        assert!(!withheld.is_empty());
        assert!(approvals.contains(&small));
    };
    let fits = |data: &QueryData| assert!(serde_json::to_vec(data).unwrap().len() < FRAME);

    let snapshot = snapshot(&f).await;
    assert!(snapshot.tasks.contains(&deciding));
    every(&snapshot.approvals, &snapshot.withheld_approvals);
    for &approval_id in &snapshot.withheld_approvals {
        let read = query(&f, Query::Approval { approval_id }).await;
        fits(&read);
        let QueryData::Approval { approval } = read else {
            panic!("approval expected")
        };
        assert!(pending.contains(&approval));
    }

    let task = query(
        &f,
        Query::Task {
            task_id: deciding.task_id,
        },
    )
    .await;
    fits(&task);
    let QueryData::Task { records } = task else {
        panic!("task expected")
    };
    every(&records.approvals, &records.withheld_approvals);

    let history = query(
        &f,
        Query::TaskHistory {
            project_id: project,
            from: TaskCursor {
                updated_at: deciding.updated_at,
                task_id: deciding.task_id,
            },
            limit: 10,
        },
    )
    .await;
    fits(&history);
    let QueryData::TaskHistory { records, .. } = history else {
        panic!("history expected")
    };
    assert_eq!(records.tasks, vec![deciding.clone()]);
    every(&records.approvals, &records.withheld_approvals);

    let run = query(
        &f,
        Query::Run {
            run_id: deciding.current_run.unwrap(),
        },
    )
    .await;
    fits(&run);
    let QueryData::Run {
        approvals,
        withheld_approvals,
        ..
    } = run
    else {
        panic!("run expected")
    };
    every(&approvals, &withheld_approvals);
    f.close().await;
}

#[tokio::test]
async fn a_phone_that_cannot_read_withheld_approvals_gets_every_one_or_a_reason() {
    let f = Fixture::new().await;
    let project = f.project().await;
    let template = Template::new(&f, project).await;
    let deciding = template.record(&f, project, TaskState::AwaitingDecision, 1, 10);
    // Together past the snapshot's budget, but within one frame, where an
    // earlier phone always received them.
    let pending = approvals_of(&f, &deciding, 2, 400 * 1024);
    let snapshot = snapshot_for(&f, None).await;
    assert!(snapshot.withheld_approvals.is_empty());
    assert!(pending.iter().all(|a| snapshot.approvals.contains(a)));
    // A phone that says it reads them has one named instead.
    assert_eq!(
        snapshot_for(&f, Some(true)).await.withheld_approvals.len(),
        1
    );
    // Past one frame, it is refused as needing a newer app rather than sent
    // without one it would never learn of.
    approvals_of(&f, &deciding, 1, 400 * 1024);
    let error = f
        .hub
        .query(
            &Caller::local_owner(),
            Query::Snapshot {
                reads_withheld: None,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::IncompatibleVersion);
    f.close().await;
}

/// Queues a task waiting to be started, with `goal` as its mission and
/// `prompt` as its request, through the command path a phone uses.
async fn queue_task(f: &Fixture, project: ProjectId, goal: &str, prompt: &str) -> CommandReceipt {
    let task = Queued {
        prompt,
        ..Queued::new(project, "Queued task", goal)
    };
    task.submit(f, &Caller::local_owner()).await
}

/// A task to queue through the command path a phone uses.
struct Queued<'a> {
    project: ProjectId,
    title: &'a str,
    goal: &'a str,
    prompt: &'a str,
    /// Whether it uses the computer's desktop, which never starts by itself.
    computer: bool,
    authorization: QueueAuthorization,
}

impl<'a> Queued<'a> {
    fn new(project: ProjectId, title: &'a str, goal: &'a str) -> Self {
        Self {
            project,
            title,
            goal,
            prompt: "test",
            computer: false,
            authorization: QueueAuthorization::AskBeforeStart,
        }
    }
    async fn submit(&self, f: &Fixture, caller: &Caller) -> CommandReceipt {
        let (project, title, goal, prompt) = (self.project, self.title, self.goal, self.prompt);
        f.hub
            .submit(
                caller,
                CommandEnvelope::new(Command::CreateTask {
                    github: None,
                    agent_chosen: None,
                    mode: None,
                    project_id: project,
                    kind: if self.computer {
                        TaskKind::ComputerUse
                    } else {
                        TaskKind::Coding
                    },
                    title: Some(title.into()),
                    prompt: prompt.into(),
                    bundle: None,
                    harness: HarnessKind::ClaudeCode,
                    model: None,
                    effort: None,
                    launch_policy: None,
                    branch: None,
                    mission: Some(Mission {
                        goal: goal.into(),
                        bounds: vec![],
                        completion_checks: vec![],
                        stop_point: None,
                    }),
                    target_id: (self.computer).then(|| f.desktop.target.lock().unwrap().target_id),
                    placement: TaskPlacement::Queue {
                        position: None,
                        authorization: self.authorization,
                    },
                    start: None,
                    place: None,
                    browser: None,
                    conversation: None,
                    access: None,
                    review: None,
                }),
            )
            .await
    }
}

#[tokio::test]
async fn unfinished_tasks_a_person_adds_never_outgrow_a_first_load() {
    let f = Fixture::new().await;
    let project = f.project().await;
    let mut refused = None;
    let mut created = 0;
    // Each is admitted alone; together they would take a snapshot past a
    // frame, and every one of them is unfinished, so none can wait for a page.
    for _ in 0..8 {
        let receipt = queue_task(&f, project, &"g".repeat(300 * 1024), "test").await;
        if receipt.status == ReceiptStatus::Rejected {
            refused = receipt.error;
            break;
        }
        assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
        created += 1;
    }
    let refused = refused.expect("a task that would crowd the first load is refused");
    assert_eq!(refused.code, ErrorCode::LimitExceeded, "{refused:?}");
    assert!(created > 0);
    for reads_withheld in [Some(true), None] {
        let snapshot = snapshot_for(&f, reads_withheld).await;
        assert_eq!(snapshot.tasks.len(), created);
    }
    f.close().await;
}

#[tokio::test]
async fn next_up_a_person_fills_still_fits_one_reply() {
    let f = Fixture::new().await;
    let project = f.project().await;
    let mut refused = None;
    for _ in 0..8 {
        let receipt = queue_task(&f, project, "Short goal", &"p".repeat(300 * 1024)).await;
        if receipt.status == ReceiptStatus::Rejected {
            refused = receipt.error;
            break;
        }
        assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    }
    let refused = refused.expect("a request that would overfill Next up is refused");
    assert_eq!(refused.code, ErrorCode::LimitExceeded, "{refused:?}");
    let queue = match query(
        &f,
        Query::Queue {
            project_id: project,
        },
    )
    .await
    {
        QueryData::Queue { queue } => queue,
        _ => panic!("queue expected"),
    };
    assert!(serde_json::to_vec(&queue).unwrap().len() < FRAME);
    // Editing a waiting request cannot overfill it either.
    let item = queue.items[0].item_id;
    let receipt = f
        .hub
        .submit(
            &Caller::local_owner(),
            CommandEnvelope {
                expected_revision: Some(queue.revision),
                ..CommandEnvelope::new(Command::QueueEditPrompt {
                    item_id: item,
                    prompt: "e".repeat(900 * 1024),
                })
            },
        )
        .await;
    assert_eq!(receipt.status, ReceiptStatus::Rejected, "{receipt:?}");
    let queue = match query(
        &f,
        Query::Queue {
            project_id: project,
        },
    )
    .await
    {
        QueryData::Queue { queue } => queue,
        _ => panic!("queue expected"),
    };
    assert!(serde_json::to_vec(&queue).unwrap().len() < FRAME);
    snapshot(&f).await;
    f.close().await;
}

#[tokio::test]
async fn project_defaults_are_labels() {
    let f = Fixture::new().await;
    let project = f.project().await;
    let receipt = f
        .hub
        .submit(
            &Caller::local_owner(),
            CommandEnvelope::new(Command::UpdateProjectDefaults {
                project_id: project,
                harness: None,
                model: Some("m".repeat(900 * 1024)),
                effort: None,
            }),
        )
        .await;
    assert_eq!(receipt.status, ReceiptStatus::Rejected, "{receipt:?}");
    snapshot(&f).await;
    f.close().await;
}

async fn queue_of(f: &Fixture, project: ProjectId) -> QueueView {
    match query(
        f,
        Query::Queue {
            project_id: project,
        },
    )
    .await
    {
        QueryData::Queue { queue } => queue,
        _ => panic!("queue expected"),
    }
}

#[tokio::test]
async fn work_that_started_from_next_up_and_ended_leaves_it_room() {
    let f = Fixture::new().await;
    let owner = Caller::local_owner();
    let project = f.project().await;
    // Each request fills most of Next up while it waits. Once its task has
    // started it no longer waits, and once that ends Next up has nothing
    // left of it to carry.
    for state in [
        RunState::Completed,
        RunState::Failed,
        RunState::Interrupted,
        RunState::Completed,
    ] {
        let receipt = queue_task(&f, project, "Short goal", &"p".repeat(500 * 1024)).await;
        assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
        let Some(CommandResult::TaskCreated { task_id, .. }) = receipt.result else {
            panic!("task expected");
        };
        let queue = queue_of(&f, project).await;
        let item = queue.items.iter().find(|i| i.task_id == task_id).unwrap();
        let task: Task = f.hub.read(|r| r.require("task", task_id)).unwrap();
        let receipt = f
            .hub
            .submit(
                &owner,
                edit(
                    Command::QueueStart {
                        item_id: item.item_id,
                        task_revision: task.revision,
                    },
                    queue.revision,
                ),
            )
            .await;
        assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
        settle(&f.hub, receipt.command_id).await;
        let run = run_started(&f, task_id).await;
        let queue = queue_of(&f, project).await;
        let started = queue.items.iter().find(|i| i.task_id == task_id).unwrap();
        assert!(!started.is_pending());
        assert!(
            started.request.is_none(),
            "a started item keeps its request"
        );
        f.hub.finish_run(run, state, None).unwrap();
        let queue = queue_of(&f, project).await;
        assert!(
            queue.items.iter().all(|i| i.task_id != task_id),
            "{state:?} work stays in Next up"
        );
    }
    snapshot(&f).await;
    f.close().await;
}

#[tokio::test]
async fn a_task_names_its_model_and_effort_in_a_label() {
    let f = Fixture::new().await;
    let project = f.project().await;
    for (model, effort) in [(Some("m".repeat(513)), None), (None, Some("e".repeat(513)))] {
        let receipt = f
            .hub
            .submit(
                &Caller::local_owner(),
                CommandEnvelope::new(Command::CreateTask {
                    github: None,
                    agent_chosen: None,
                    mode: None,
                    project_id: project,
                    kind: TaskKind::Coding,
                    title: Some("Short".into()),
                    prompt: "test".into(),
                    bundle: None,
                    harness: HarnessKind::ClaudeCode,
                    model,
                    effort,
                    launch_policy: None,
                    branch: None,
                    mission: None,
                    target_id: None,
                    placement: TaskPlacement::Queue {
                        position: None,
                        authorization: QueueAuthorization::AskBeforeStart,
                    },
                    start: None,
                    place: None,
                    browser: None,
                    conversation: None,
                    access: None,
                    review: None,
                }),
            )
            .await;
        assert_eq!(receipt.error.unwrap().code, ErrorCode::InvalidRequest);
    }
    f.close().await;
}

/// Makes `task`'s mission goal take everything live to `live` bytes.
fn weigh_live(f: &Fixture, task: TaskId, live: usize) {
    let now = f.hub.read(crate::history::live_weight).unwrap();
    f.hub
        .write(|tx| {
            let mut t: Task = tx.read().require("task", task)?;
            let mission = t.mission.as_mut().unwrap();
            mission.goal = "g".repeat(mission.goal.len() + live - now);
            tx.put("task", task, &t)
        })
        .unwrap();
    assert_eq!(f.hub.read(crate::history::live_weight).unwrap(), live);
}

#[tokio::test]
async fn starting_next_up_never_outgrows_a_first_load() {
    let f = Fixture::new().await;
    let owner = Caller::local_owner();
    let project = f.project().await;
    let receipt = queue_task(&f, project, "Short goal", "test").await;
    let Some(CommandResult::TaskCreated { task_id, .. }) = receipt.result else {
        panic!("task expected: {receipt:?}");
    };
    // Starting adds the run to what is live. With no room for it, a start
    // is refused, and the task waits as it was.
    weigh_live(&f, task_id, crate::history::LIVE_BYTES - 1);
    let queue = queue_of(&f, project).await;
    let task: Task = f.hub.read(|r| r.require("task", task_id)).unwrap();
    let receipt = f
        .hub
        .submit(
            &owner,
            edit(
                Command::QueueStart {
                    item_id: queue.items[0].item_id,
                    task_revision: task.revision,
                },
                queue.revision,
            ),
        )
        .await;
    assert_eq!(receipt.error.unwrap().code, ErrorCode::LimitExceeded);
    let task: Task = f.hub.read(|r| r.require("task", task_id)).unwrap();
    assert_eq!(task.state, TaskState::Accepted);
    assert!(queue_of(&f, project).await.items[0].is_pending());
    snapshot(&f).await;
    f.close().await;
}

#[tokio::test]
async fn next_up_asks_before_a_start_that_would_outgrow_a_first_load() {
    let f = Fixture::new().await;
    let device = f.device().await;
    let project = f.project().await;
    let receipt = f
        .hub
        .submit(
            &device,
            CommandEnvelope::new(Command::CreateTask {
                github: None,
                agent_chosen: None,
                mode: None,
                project_id: project,
                kind: TaskKind::Coding,
                title: Some("Queued task".into()),
                prompt: "test".into(),
                bundle: None,
                harness: HarnessKind::ClaudeCode,
                model: None,
                effort: None,
                launch_policy: None,
                branch: None,
                mission: Some(Mission {
                    goal: "Short goal".into(),
                    bounds: vec![],
                    completion_checks: vec![],
                    stop_point: None,
                }),
                target_id: None,
                placement: TaskPlacement::Queue {
                    position: None,
                    authorization: QueueAuthorization::AutoStart,
                },
                start: None,
                place: None,
                browser: None,
                conversation: None,
                access: None,
                review: None,
            }),
        )
        .await;
    let Some(CommandResult::TaskCreated { task_id, .. }) = receipt.result else {
        panic!("task expected: {receipt:?}");
    };
    f.hub
        .write(|tx| {
            let mut queue: QueueView = tx.read().require("queue", project)?;
            queue.paused = false;
            tx.put("queue", project, &queue)
        })
        .unwrap();
    weigh_live(&f, task_id, crate::history::LIVE_BYTES - 1);
    // Nothing a person could be asked about starts by itself: the task
    // stays in Next up and asks for a go-ahead instead.
    f.hub.advance_queues().await.unwrap();
    let task: Task = f.hub.read(|r| r.require("task", task_id)).unwrap();
    assert_eq!(task.state, TaskState::Accepted);
    assert_eq!(f.harness.starts.load(Ordering::SeqCst), 0);
    let item = queue_of(&f, project).await.items[0].clone();
    assert_eq!(item.state, QueueItemState::AwaitingStart);
    assert_eq!(item.authorization, QueueAuthorization::AskBeforeStart);
    assert!(
        f.hub
            .read(|r| r.get::<crate::runs::QueuePermit>("queue_permit", item.item_id))
            .unwrap()
            .is_none()
    );
    snapshot(&f).await;
    f.close().await;
}

#[tokio::test]
async fn projects_a_person_adds_never_outgrow_a_first_load() {
    let f = Fixture::new().await;
    let owner = Caller::local_owner();
    let label = |c: &str| c.repeat(512);
    // Every project travels in a snapshot, so projects with long names and
    // defaults can fill a frame on their own, with no task at all.
    let mut refused = None;
    for i in 0..600 {
        let dir = f.project_dir().join(i.to_string());
        std::fs::create_dir_all(&dir).unwrap();
        let receipt = f
            .hub
            .submit(
                &owner,
                CommandEnvelope::new(Command::AddProject {
                    host_path: dir.to_string_lossy().into_owned(),
                    name: Some(label("n")),
                }),
            )
            .await;
        let Some(CommandResult::ProjectAdded { project_id }) = receipt.result else {
            refused = Some(receipt);
            break;
        };
        let receipt = f
            .hub
            .submit(
                &owner,
                CommandEnvelope::new(Command::UpdateProjectDefaults {
                    project_id,
                    harness: None,
                    model: Some(label("m")),
                    effort: Some(label("e")),
                }),
            )
            .await;
        if receipt.status != ReceiptStatus::Accepted {
            refused = Some(receipt);
            break;
        }
    }
    let refused = refused.expect("projects that outgrow a first load are refused");
    assert_eq!(refused.error.unwrap().code, ErrorCode::LimitExceeded);
    snapshot(&f).await;
    f.close().await;
}

/// Renames `task` from `from` to `to`, as the phone does.
async fn rename(f: &Fixture, task: TaskId, from: &str, to: &str) -> CommandReceipt {
    f.hub
        .submit(
            &Caller::local_owner(),
            CommandEnvelope::new(Command::RenameTask {
                task_id: task,
                title: to.into(),
                expected_title: Some(from.into()),
            }),
        )
        .await
}

#[tokio::test]
async fn renames_never_outgrow_a_first_load() {
    let f = Fixture::new().await;
    let project = f.project().await;
    let mut tasks = Vec::new();
    for _ in 0..4 {
        let receipt = queue_task(&f, project, "Short goal", "test").await;
        let Some(CommandResult::TaskCreated { task_id, .. }) = receipt.result else {
            panic!("task expected: {receipt:?}");
        };
        tasks.push(task_id);
    }
    // Every live task travels whole in a first load. A title of quotes
    // doubles when encoded, so each rename adds about a kilobyte, and the
    // first as much again to the room Next up keeps for asking about it:
    // two fit in what is left, and the third would not.
    weigh_live(&f, tasks[0], crate::history::LIVE_BYTES - 3500);
    let quotes = "\"".repeat(512);
    for task in &tasks[..2] {
        let receipt = rename(&f, *task, "Queued task", &quotes).await;
        assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    }
    let refused = rename(&f, tasks[2], "Queued task", &quotes).await;
    assert_eq!(refused.error.unwrap().code, ErrorCode::LimitExceeded);
    let task: Task = f.hub.read(|r| r.require("task", tasks[2])).unwrap();
    assert_eq!(task.title, "Queued task");
    assert!(f.hub.read(crate::history::live_weight).unwrap() <= crate::history::LIVE_BYTES);
    snapshot(&f).await;
    // Past the budget, from what the computer reports, a longer title is
    // still refused, and a shorter one never is.
    weigh_live(&f, tasks[0], crate::history::LIVE_BYTES + 100);
    let refused = rename(&f, tasks[3], "Queued task", "A longer name").await;
    assert_eq!(refused.error.unwrap().code, ErrorCode::LimitExceeded);
    let receipt = rename(&f, tasks[1], &quotes, "Short").await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    f.close().await;
}

/// Adds a project in a folder of its own, the `index`th.
async fn project_at(f: &Fixture, index: usize) -> ProjectId {
    let dir = f.project_dir().join(index.to_string());
    std::fs::create_dir_all(&dir).unwrap();
    let receipt = f
        .hub
        .submit(
            &Caller::local_owner(),
            CommandEnvelope::new(Command::AddProject {
                host_path: dir.to_string_lossy().into_owned(),
                name: None,
            }),
        )
        .await;
    match receipt.result {
        Some(CommandResult::ProjectAdded { project_id }) => project_id,
        _ => panic!("project expected: {receipt:?}"),
    }
}

/// Resumes `project`'s Next up, as the phone does.
async fn resume(f: &Fixture, project: ProjectId) {
    let queue = queue_of(f, project).await;
    let receipt = f
        .hub
        .submit(
            &Caller::local_owner(),
            edit(
                Command::QueueResume {
                    project_id: project,
                },
                queue.revision,
            ),
        )
        .await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
}

/// Fills projects with one waiting task each, titled `title`, until one
/// more is refused, and returns the tasks that were admitted.
async fn fill_projects(
    f: &Fixture,
    caller: &Caller,
    running: bool,
    task: impl Fn(ProjectId) -> Queued<'static>,
) -> Vec<TaskId> {
    let mut admitted = Vec::new();
    for index in 0..64 {
        let project = project_at(f, index).await;
        if running {
            resume(f, project).await;
        }
        let receipt = task(project).submit(f, caller).await;
        let Some(CommandResult::TaskCreated { task_id, .. }) = receipt.result else {
            assert_eq!(receipt.error.unwrap().code, ErrorCode::LimitExceeded);
            return admitted;
        };
        admitted.push(task_id);
    }
    panic!("no task was refused");
}

/// Whether each of `tasks` asks for a go-ahead, and everything live still
/// fits a first load.
async fn each_asks_within_a_first_load(f: &Fixture, tasks: &[TaskId]) {
    let snapshot = snapshot(f).await;
    let asking = (snapshot.attention.iter())
        .filter(|a| a.resolved_at.is_none() && a.task_id.is_some_and(|t| tasks.contains(&t)))
        .count();
    assert_eq!(asking, tasks.len());
    let live = f.hub.read(crate::history::live_weight).unwrap();
    assert!(live <= crate::history::LIVE_BYTES, "{live}");
}

#[tokio::test]
async fn renaming_waiting_work_never_outgrows_a_first_load() {
    let f = Fixture::new().await;
    let device = f.device().await;
    let quotes: &'static str = "\"".repeat(512).leak();
    let goal: &'static str = "g".repeat(20 * 1024).leak();
    // Work that uses the computer never starts by itself, so it waits in a
    // running Next up without asking. A rename keeps its go-ahead; the
    // person taking it back makes each ask.
    let tasks = fill_projects(&f, &device, true, |project| Queued {
        computer: true,
        authorization: QueueAuthorization::AutoStart,
        ..Queued::new(project, quotes, goal)
    })
    .await;
    assert!(tasks.len() > 1);
    for task in &tasks {
        let receipt = rename(&f, *task, quotes, &quotes[1..]).await;
        assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    }
    let snapshot = snapshot(&f).await;
    assert!(snapshot.attention.iter().all(|a| a.resolved_at.is_some()));
    for task in &tasks {
        let project = f
            .hub
            .read(|r| r.require::<Task>("task", *task))
            .unwrap()
            .project_id;
        let queue: QueueView = f.hub.read(|r| r.require("queue", project)).unwrap();
        let mut ask = CommandEnvelope::new(Command::QueueSetAuthorization {
            item_id: queue.items[0].item_id,
            authorization: QueueAuthorization::AskBeforeStart,
        });
        ask.expected_revision = Some(queue.revision);
        let receipt = f.hub.submit(&Caller::local_owner(), ask).await;
        assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    }
    each_asks_within_a_first_load(&f, &tasks).await;
    f.close().await;
}

#[tokio::test]
async fn resuming_next_up_never_outgrows_a_first_load() {
    let f = Fixture::new().await;
    let quotes: &'static str = "\"".repeat(512).leak();
    let goal: &'static str = "g".repeat(20 * 1024).leak();
    // Next up starts paused, so what waits in it asks for nothing until it
    // is resumed.
    let tasks = fill_projects(&f, &Caller::local_owner(), false, |project| {
        Queued::new(project, quotes, goal)
    })
    .await;
    assert!(tasks.len() > 1);
    let projects: Vec<ProjectId> = f
        .hub
        .read(|r| r.list::<Project>("project"))
        .unwrap()
        .iter()
        .map(|p| p.project_id)
        .collect();
    for project in projects {
        resume(&f, project).await;
    }
    each_asks_within_a_first_load(&f, &tasks).await;
    f.close().await;
}

#[tokio::test]
async fn a_dismissed_request_keeps_its_room_until_it_is_answered() {
    let f = Fixture::new().await;
    let owner = Caller::local_owner();
    let project = f.project().await;
    let (task, _) = f
        .task(project, TaskPlacement::StartNow, &owner, false)
        .await;
    let run = run_started(&f, task).await;
    f.harness
        .emit(
            run,
            ports::HarnessEvent::ApprovalRequested(ports::NativeApproval {
                native_request_id: "tool".into(),
                kind: ApprovalKind::Command,
                title: "Run test".into(),
                detail: None,
                detail_lines: Vec::new(),
                questions: Vec::new(),
                raw_request: None,
                scope: "Once".into(),
                shortened: false,
                tool_call: None,
            }),
        )
        .await;
    let notice = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let items = f
                .hub
                .read(|r| r.list::<AttentionItem>("attention"))
                .unwrap();
            if let Some(item) = items.into_iter().find(|a| a.approval_id.is_some()) {
                break item;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let dismissed = f
        .hub
        .submit(
            &owner,
            CommandEnvelope::new(Command::AcknowledgeAttention {
                attention_id: notice.attention_id,
            }),
        )
        .await;
    assert_eq!(dismissed.status, ReceiptStatus::Accepted, "{dismissed:?}");
    // Live tasks then fill the budget but for a byte.
    f.hub
        .write(|tx| {
            let now = crate::history::live_weight(&tx.read())?;
            let mut t: Task = tx.read().require("task", task)?;
            t.title = "t".repeat(t.title.len() + crate::history::LIVE_BYTES - 1 - now);
            tx.put("task", task, &t)
        })
        .unwrap();
    // A decision the agent refuses brings the notice back, as a new one, and
    // the request stays open.
    f.harness
        .controls
        .refuse_resolution
        .store(true, Ordering::SeqCst);
    let approval: Approval = f
        .hub
        .read(|r| r.require("approval", notice.approval_id.unwrap()))
        .unwrap();
    let request = edit(
        Command::ResolveApproval {
            approval_id: approval.approval_id,
            decision: ApprovalDecision::Allow,
        },
        approval.revision.into(),
    );
    let id = request.command_id;
    assert_eq!(
        f.hub.submit(&owner, request).await.status,
        ReceiptStatus::Accepted
    );
    settle(&f.hub, id).await;
    let items: Vec<AttentionItem> = (f.hub.read(|r| r.list::<AttentionItem>("attention")))
        .unwrap()
        .into_iter()
        .filter(|a| a.approval_id == notice.approval_id)
        .collect();
    assert_eq!(items.len(), 2);
    assert!(
        items
            .iter()
            .any(|a| a.attention_id == notice.attention_id && a.resolved_at.is_some())
    );
    assert!(
        items
            .iter()
            .any(|a| a.attention_id != notice.attention_id && a.resolved_at.is_none())
    );
    let approval: Approval = f
        .hub
        .read(|r| r.require("approval", approval.approval_id))
        .unwrap();
    assert_eq!(approval.state, ApprovalState::Pending);
    assert!(f.hub.read(crate::history::live_weight).unwrap() <= crate::history::LIVE_BYTES);
    f.close().await;
}

#[tokio::test]
async fn snapshot_preserves_live_reasons_and_grants() {
    let f = Fixture::new().await;
    let project = f.project().await;
    let template = Template::new(&f, project).await;
    // Each finished task is live for a different reason, beyond its state.
    let pending_task = template.record(&f, project, TaskState::Completed, 1, 0);
    let needs_you = template.record(&f, project, TaskState::Completed, 2, 0);
    let queued = template.record(&f, project, TaskState::Completed, 3, 0);
    let pending = approval(
        &pending_task,
        pending_task.current_run.unwrap(),
        ApprovalState::Pending,
    );
    let notice = AttentionItem {
        attention_id: AttentionId::new(),
        kind: AttentionKind::ReconciliationRequired,
        title: "Check this".into(),
        body: None,
        task_id: Some(needs_you.task_id),
        run_id: needs_you.current_run,
        approval_id: None,
        created_at: Timestamp(2),
        resolved_at: None,
        priority: 5,
        topic: None,
        child_id: None,
        quiet: false,
        report_id: None,
    };
    let orphan = AttentionItem {
        attention_id: AttentionId::new(),
        task_id: Some(TaskId::new()),
        run_id: None,
        ..notice.clone()
    };
    f.hub
        .write(|tx| {
            tx.put("approval", pending.approval_id, &pending)?;
            tx.put("attention", notice.attention_id, &notice)?;
            tx.put("attention", orphan.attention_id, &orphan)?;
            let mut queue: QueueView = tx.read().require("queue", project)?;
            let mut item = dependency_item(QueueItemState::AwaitingStart, vec![]);
            item.task_id = queued.task_id;
            queue.items.push(item);
            tx.put("queue", project, &queue)
        })
        .unwrap();
    // Newer tasks fill the optional budget; none of these old tasks can
    // rely on being reached by the history walk.
    for at in [10_000, 10_001] {
        let recent = template.record(&f, project, TaskState::Completed, at, 0);
        f.hub
            .write(|tx| {
                for _ in 0..10 {
                    let mut settled =
                        approval(&recent, recent.current_run.unwrap(), ApprovalState::Allowed);
                    settled.detail = Some("s".repeat(75 * 1024));
                    tx.put("approval", settled.approval_id, &settled)?;
                }
                Ok(())
            })
            .unwrap();
    }
    for (tasks, approvals) in [(true, true), (true, false), (false, true), (false, false)] {
        let g = Grants {
            tasks,
            approvals,
            ..Grants::default()
        };
        let snapshot = f
            .hub
            .read(|r| {
                let mut snapshot = crate::history::always(r, &g)?;
                crate::history::fill(r, &g, &mut snapshot, true)?;
                Ok(snapshot)
            })
            .unwrap();
        if tasks && approvals {
            assert!(snapshot.older_tasks.as_ref().unwrap().from.updated_at > Timestamp(3));
        }
        assert_eq!(snapshot.approvals.contains(&pending), approvals);
        for task in [&pending_task, &needs_you, &queued] {
            assert_eq!(snapshot.tasks.contains(task), tasks);
            assert_eq!(
                snapshot
                    .runs
                    .iter()
                    .any(|r| Some(r.run_id) == task.current_run),
                tasks
            );
        }
        assert_eq!(snapshot.attention.contains(&notice), tasks);
        assert_eq!(snapshot.attention.contains(&orphan), tasks);
        if !tasks {
            assert!(snapshot.tasks.is_empty());
            assert!(snapshot.runs.is_empty());
            assert!(snapshot.attention.is_empty());
            assert!(snapshot.older_tasks.is_none());
        }
        if !approvals {
            assert!(snapshot.approvals.is_empty());
            assert!(snapshot.withheld_approvals.is_empty());
        }
    }
    f.close().await;
}

#[tokio::test]
async fn snapshot_skips_finished_history_beyond_its_budget() {
    thread_local! {
        static RELATED_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }
    fn trace(event: rusqlite::trace::TraceEvent<'_>) {
        if let rusqlite::trace::TraceEvent::Stmt(statement, _) = event {
            let sql = statement.sql();
            if sql.starts_with("SELECT")
                && (sql.contains("FROM history_runs")
                    || sql.contains("kind='approval'")
                    || sql.contains("kind='attention'"))
            {
                RELATED_READS.with(|n| n.set(n.get() + 1));
            }
        }
    }
    let f = Fixture::new().await;
    let project = f.project().await;
    let template = Template::new(&f, project).await;
    let record = |at| {
        let task = template.record(&f, project, TaskState::Completed, at, 0);
        f.hub
            .write(|tx| {
                for _ in 0..10 {
                    let mut decided =
                        approval(&task, task.current_run.unwrap(), ApprovalState::Allowed);
                    decided.detail = Some("a".repeat(8 * 1024));
                    tx.put("approval", decided.approval_id, &decided)?;
                }
                Ok(())
            })
            .unwrap();
    };
    for n in 0..20 {
        record(10_000 + n);
    }
    let measure = || {
        f.hub.inner.db.lock().unwrap().trace(Some(trace));
        let result = f
            .hub
            .read(|r| {
                let g = protocol::auth::Grants::owner();
                store::take_read_cost();
                RELATED_READS.with(|n| n.set(0));
                let mut snapshot = crate::history::always(r, &g)?;
                crate::history::fill(r, &g, &mut snapshot, true)?;
                let cost = (RELATED_READS.with(|n| n.get()), store::take_read_cost());
                eprintln!("snapshot read cost (related queries, (rows, bytes)): {cost:?}");
                assert!(snapshot.tasks.contains(&template.task));
                assert!(snapshot.older_tasks.is_some());
                assert!(serde_json::to_vec(&snapshot).unwrap().len() < FRAME);
                Ok((snapshot, cost))
            })
            .unwrap();
        f.hub.inner.db.lock().unwrap().trace(None);
        result
    };
    let (before, baseline) = measure();
    for n in 0..2_000 {
        record(n);
    }
    let (after, cost) = measure();
    assert_eq!(
        after, before,
        "older history cannot appear in this snapshot"
    );
    assert_eq!(
        cost, baseline,
        "older history must not be queried or decoded"
    );
    f.close().await;
}

#[tokio::test]
async fn unrelated_finished_history_does_not_increase_read_cost() {
    let f = Fixture::new().await;
    let project = f.project().await;
    let template = Template::new(&f, project).await;
    let other = ProjectId::new();
    let measure = || {
        f.hub
            .read(|r| {
                let g = protocol::auth::Grants::owner();
                store::take_read_cost();
                let start = std::time::Instant::now();
                crate::history::task(r, &g, template.task.task_id)?;
                let task = store::take_read_cost();
                crate::history::page(
                    r,
                    &g,
                    project,
                    TaskCursor {
                        updated_at: Timestamp(i64::MAX),
                        task_id: template.task.task_id,
                    },
                    1,
                )?;
                let page = store::take_read_cost();
                crate::history::live_weight(r)?;
                let admission = store::take_read_cost();
                eprintln!(
                    "history cost: task={task:?} page={page:?} admission={admission:?} lock={:?}",
                    start.elapsed()
                );
                Ok([task, page, admission])
            })
            .unwrap()
    };
    let baseline = measure();
    for range in [0..100, 100..1000] {
        for n in range {
            let task = template.record(&f, other, TaskState::Completed, n, 100 * 1024);
            let decided = approval(&task, task.current_run.unwrap(), ApprovalState::Allowed);
            f.hub
                .write(|tx| tx.put("approval", decided.approval_id, &decided))
                .unwrap();
        }
        assert_eq!(
            measure(),
            baseline,
            "unrelated finished history must not be decoded"
        );
    }
    f.close().await;
}

#[tokio::test]
async fn queue_refresh_skips_unrelated_finished_history() {
    let f = Fixture::new().await;
    let project = f.project().await;
    let template = Template::new(&f, project).await;
    let mut queue: QueueView = f.hub.read(|r| r.require("queue", project)).unwrap();
    queue.items.clear();
    queue.writer = None;
    queue.paused = false;
    let independent = QueueView {
        project_id: ProjectId::new(),
        items: vec![dependency_item(QueueItemState::AwaitingStart, vec![])],
        ..queue.clone()
    };
    let prerequisite = template.record(&f, project, TaskState::Completed, 0, 100 * 1024);
    let parent = dependency_item(
        QueueItemState::Started {
            run_id: prerequisite.current_run.unwrap(),
        },
        vec![],
    );
    let dependent = QueueView {
        project_id: ProjectId::new(),
        items: vec![
            parent.clone(),
            dependency_item(QueueItemState::AwaitingStart, vec![parent.item_id]),
            dependency_item(QueueItemState::AwaitingStart, vec![parent.item_id]),
        ],
        ..queue.clone()
    };
    let queues = [queue, independent, dependent];
    // Every unpaused queue used to decode the same retained history again.
    for queue in &queues {
        f.hub
            .write(|tx| tx.put("queue", queue.project_id, queue))
            .unwrap();
    }
    async fn measure(f: &Fixture, queues: &[QueueView; 3]) -> [(usize, usize); 4] {
        let mut costs = [(0, 0); 4];
        for (index, queue) in queues.iter().enumerate() {
            costs[index] = f
                .hub
                .write(|tx| {
                    store::take_read_cost();
                    f.hub.refresh_queue(tx, &mut queue.clone())?;
                    Ok(store::take_read_cost())
                })
                .unwrap();
        }
        store::take_read_cost();
        f.hub.advance_queues().await.unwrap();
        costs[3] = store::take_read_cost();
        eprintln!("queue cost (empty, independent, dependent, advance_three): {costs:?}");
        costs
    }
    let baseline = measure(&f, &queues).await;
    let other = ProjectId::new();
    for n in 0..1000 {
        template.record(&f, other, TaskState::Completed, n, 100 * 1024);
    }
    let with_history = measure(&f, &queues).await;
    f.close().await;
    assert_eq!(
        with_history, baseline,
        "queue refresh and advancement must not decode unrelated finished history"
    );
}

#[tokio::test]
async fn admission_skips_a_live_tasks_finished_runs_and_resolved_notices() {
    let f = Fixture::new().await;
    let project = f.project().await;
    let template = Template::new(&f, project).await;
    let measure = || {
        f.hub
            .read(|r| {
                store::take_read_cost();
                let weight = crate::history::live_weight(r)?;
                Ok((weight, store::take_read_cost()))
            })
            .unwrap()
    };
    let baseline = measure();
    template.earlier_runs(&f, &template.task, 100, 100 * 1024);
    assert_eq!(measure(), baseline);
    f.close().await;
}

fn dependency_item(state: QueueItemState, depends_on: Vec<QueueItemId>) -> QueueItem {
    QueueItem {
        item_id: QueueItemId::new(),
        task_id: TaskId::new(),
        position: 0,
        depends_on,
        authorization: QueueAuthorization::AskBeforeStart,
        state,
        added_by: Principal::LocalOwner,
        added_at: Timestamp(0),
        request: None,
        authorization_expires_at: None,
    }
}

#[tokio::test]
async fn queue_dependency_summaries_preserve_outcomes_and_skip_diffs() {
    let f = Fixture::new().await;
    let project = f.project().await;
    let template = Template::new(&f, project).await;
    let task = template.record(&f, project, TaskState::Completed, 1, 100 * 1024);
    let run: Run = f
        .hub
        .read(|r| r.require("run", task.current_run.unwrap()))
        .unwrap();
    let unrelated = dependency_item(
        QueueItemState::Started {
            run_id: template.run.run_id,
        },
        vec![],
    );
    // A started item's own dependencies no longer affect its state.
    let parent = dependency_item(
        QueueItemState::Started { run_id: run.run_id },
        vec![unrelated.item_id],
    );
    let mut queue: QueueView = f.hub.read(|r| r.require("queue", project)).unwrap();
    queue.items = vec![
        unrelated,
        parent.clone(),
        dependency_item(QueueItemState::Waiting, vec![parent.item_id]),
        dependency_item(QueueItemState::Waiting, vec![parent.item_id]),
    ];
    let dependents: HashSet<_> = queue
        .items
        .iter()
        .filter(|i| i.is_pending())
        .map(|i| i.item_id)
        .collect();
    let check = |queue: &QueueView, reason: Option<&str>, rows: usize| {
        let mut queue = queue.clone();
        let (decoded, bytes) = f
            .hub
            .write(|tx| {
                store::take_read_cost();
                f.hub.refresh_queue(tx, &mut queue)?;
                Ok(store::take_read_cost())
            })
            .unwrap();
        assert_eq!(
            decoded, rows,
            "read each prerequisite once, even with two dependents"
        );
        assert!(
            bytes < 8 * 1024,
            "dependency lookup decoded {bytes} bytes including a diff"
        );
        for item in queue
            .items
            .iter()
            .filter(|i| dependents.contains(&i.item_id))
        {
            assert_eq!(
                item.state,
                match reason {
                    Some(reason) => QueueItemState::Blocked {
                        reason: reason.into()
                    },
                    None => QueueItemState::AwaitingStart,
                }
            );
        }
    };
    for (state, reason) in [
        (RunState::Completed, None),
        (RunState::Failed, Some("A prerequisite did not succeed")),
        (
            RunState::Interrupted,
            Some("A prerequisite did not succeed"),
        ),
        (
            RunState::ReconciliationRequired,
            Some("A prerequisite requires reconciliation"),
        ),
        (RunState::Admitted, Some("Waiting for a prerequisite")),
        (RunState::Dispatching, Some("Waiting for a prerequisite")),
        (RunState::Working, Some("Waiting for a prerequisite")),
        (
            RunState::AwaitingApproval,
            Some("Waiting for a prerequisite"),
        ),
        (RunState::AwaitingInput, Some("Waiting for a prerequisite")),
        (RunState::Stopping, Some("Waiting for a prerequisite")),
    ] {
        let run = Run {
            state,
            ..run.clone()
        };
        f.hub.write(|tx| tx.put("run", run.run_id, &run)).unwrap();
        check(&queue, reason, 1);
    }
    for field in ["still_active", "unknowns"] {
        let mut changed = run.clone();
        let outcome = changed.outcome.as_mut().unwrap();
        if field == "still_active" {
            outcome.still_active.push("helper".into());
        } else {
            outcome.unknowns.push("helper".into());
        }
        f.hub
            .write(|tx| tx.put("run", changed.run_id, &changed))
            .unwrap();
        check(&queue, Some("A prerequisite requires reconciliation"), 1);
    }
    for state in [
        ChildState::Active,
        ChildState::Unknown,
        ChildState::Completed,
        ChildState::Failed,
        ChildState::Stopped,
    ] {
        let mut changed = run.clone();
        // Child state matters even before an outcome has been reported.
        changed.outcome = None;
        changed.children.push(ChildAgent {
            lifetime: None,
            child_id: ChildId::new(),
            native_ref: None,
            label: "helper".into(),
            state,
            started_at: Timestamp(0),
            ended_at: None,
            provenance: Provenance::Native,
            stop: None,
        });
        f.hub
            .write(|tx| tx.put("run", changed.run_id, &changed))
            .unwrap();
        check(
            &queue,
            matches!(state, ChildState::Active | ChildState::Unknown)
                .then_some("A prerequisite requires reconciliation"),
            1,
        );
    }
    f.hub.write(|tx| tx.delete("run", run.run_id)).unwrap();
    check(&queue, Some("A prerequisite requires reconciliation"), 0);
    queue.items[1].state = QueueItemState::AwaitingStart;
    queue.items[1].depends_on.clear();
    check(&queue, Some("Waiting for a prerequisite"), 0);
    queue.items.remove(1);
    check(&queue, Some("A prerequisite was removed"), 0);
    f.close().await;
}
