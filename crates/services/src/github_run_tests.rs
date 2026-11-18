// SPDX-License-Identifier: Apache-2.0
//! Run all on the Issues tab: every issue the agent can take fixed in
//! turn with the project's own choice of agents, sorted first where it
//! isn't, until none is left or the person stops it.
use super::*;
use crate::github::{TRIAGE, Triaged, item_status};

impl Repo {
    /// The open issues, all mine, as the list and each fix read them, and
    /// sorted as `groups` says.
    fn issues(&self, groups: &[(u64, Option<TriageGroup>)]) {
        let nodes: Vec<Value> = (groups.iter())
            .map(|(number, _)| {
                json!({
                    "number": number, "title": format!("Item {number}"),
                    "url": format!("https://github.com/octo/app/issues/{number}"),
                    "updatedAt": "2026-10-02T10:00:00Z", "authorAssociation": "OWNER",
                    "author": { "login": "me" }, "labels": { "nodes": [] },
                })
            })
            .collect();
        self.gh.query(
            "issues(states:OPEN",
            json!({
                "viewer": { "login": "me" },
                "repository": { "issues": { "totalCount": nodes.len(), "nodes": nodes } },
            }),
        );
        for (number, _) in groups {
            self.mine(*number, &format!("Item {number}"));
        }
        let sorted: Vec<Triaged> = (groups.iter())
            .filter_map(|(number, group)| {
                group.map(|group| Triaged {
                    pr: false,
                    number: *number,
                    group,
                    note: "Sorted.".into(),
                    pick: None,
                    by_you: false,
                })
            })
            .collect();
        let project_id = self.project_id;
        (self.f.hub.write(|tx| tx.put(TRIAGE, project_id, &sorted))).unwrap();
    }
    async fn run_all(&self, step: RunStep) -> Result<(), ErrorCode> {
        let project_id = self.project_id;
        let action = GithubAction::RunIssues {
            project_id,
            step,
            origin: None,
        };
        self.act(&Caller::local_owner(), action).await
    }
    fn issue_run(&self) -> IssueRun {
        let project_id = self.project_id;
        let run = (self.f.hub).read(|r| crate::github_run::issue_run(r, project_id));
        run.unwrap().unwrap()
    }
    fn next_up(&self) -> QueueView {
        (self.f.hub.read(|r| r.require("queue", self.project_id))).unwrap()
    }
    /// The fix for issue `number`, once Run all has it.
    fn fix(&self, number: u64) -> Option<Task> {
        let run = self.issue_run();
        let fix = run.fixes.iter().find(|f| f.number == number)?;
        Some(self.get(fix.task_id))
    }
    /// The run of whichever of Run all's fixes starts next.
    async fn next_fix(&self) -> (TaskId, RunId) {
        let mut more = waiting();
        loop {
            let working = (self.issue_run().fixes.iter())
                .map(|f| self.get(f.task_id))
                .find(|t| matches!(t.state, TaskState::Dispatching | TaskState::Working));
            if let Some(task) = working {
                return (task.task_id, run_started(&self.f, task.task_id).await);
            }
            assert!(more(), "no fix started: {:?}", self.next_up());
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    }
    async fn until(&self, what: &str, done: impl Fn(&Self) -> bool) {
        let mut more = waiting();
        while !done(self) {
            let fixes: Vec<_> = (self.issue_run().fixes.iter())
                .map(|f| {
                    let t = self.get(f.task_id);
                    (f.number, t.state, t.current_run, t.status_line)
                })
                .collect();
            assert!(more(), "{what}: {:?} {fixes:?}", self.issue_run());
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    }
    async fn exits(&self, run: RunId, exit: ports::RunExit) {
        let exited = ports::HarnessEvent::Exited {
            exit,
            outcome: None,
        };
        self.f.harness.emit(run, exited).await;
    }
}

fn agents() -> GithubSettings {
    GithubSettings {
        workflow: GithubWorkflow {
            fix_with: Some(LaunchChoice {
                launch_policy: Some(LaunchPolicy::BypassPermissions),
                ..claude()
            }),
            review_with: Some(claude()),
            ..Default::default()
        },
        ..drafts()
    }
}

#[tokio::test]
async fn run_all_fixes_every_issue_the_agent_can_take_and_then_ends() {
    let r = repo(true).await;
    r.set(drafts()).await.unwrap();
    use TriageGroup::{AgentCanTake, NeedsYou};
    r.issues(&[
        (4, Some(AgentCanTake)),
        (5, Some(AgentCanTake)),
        (6, Some(NeedsYou)),
        (7, Some(AgentCanTake)),
    ]);
    // It asks nothing at the tap, so who fixes is chosen once beforehand.
    assert_eq!(
        r.run_all(RunStep::Start).await,
        Err(ErrorCode::InvalidRequest)
    );
    r.set(agents()).await.unwrap();
    // #7's fix waits first in Next up for a go-ahead, as one asked to wait
    // would; it held up everything behind it.
    let waiting_fix = r
        .link(
            GithubLink::Issue {
                number: 7,
                review_with: None,
                when_green: None,
            },
            "Fix it",
        )
        .await
        .unwrap();
    assert_eq!(item_status(&waiting_fix), Some(ItemStatus::Queued));

    r.run_all(RunStep::Start).await.unwrap();
    let run = r.issue_run();
    let mut numbers: Vec<u64> = run.fixes.iter().map(|f| f.number).collect();
    numbers.sort_unstable();
    assert_eq!(numbers, [4, 5, 7]);
    assert!(run.ended_at.is_none() && !run.paused && run.sorting.is_none());
    // The tap was the go-ahead for the fix already waiting, too.
    assert_eq!(r.fix(7).unwrap().task_id, waiting_fix.task_id);
    let fix = r.fix(4).unwrap();
    let github = fix.github.as_deref().unwrap();
    assert_eq!(
        github.link,
        GithubLink::Issue {
            number: 4,
            review_with: Some(claude()),
            when_green: None,
        }
    );
    assert_eq!(fix.launch_policy, Some(LaunchPolicy::BypassPermissions));

    // One at a time, each in turn, until none is left.
    for _ in 0..3 {
        let (task, run) = r.next_fix().await;
        let left = (r.next_up().items.iter())
            .filter(|i| i.is_pending())
            .count();
        assert!(left < 3);
        r.ends(task, run, Some("Done.")).await;
    }
    r.until("the run didn't end", |r| r.issue_run().ended_at.is_some())
        .await;
    let run = r.issue_run();
    assert!(!run.stopped);
    for number in [4, 5, 7] {
        assert_eq!(r.fix(number).unwrap().state, TaskState::Completed);
    }
    // Needs you was left for the person.
    let tasks = r.f.hub.read(|x| x.list::<Task>("task")).unwrap();
    assert_eq!(tasks.len(), 3);
    // Tapped again, there is nothing to do.
    assert_eq!(
        r.run_all(RunStep::Start).await,
        Err(ErrorCode::InvalidRequest)
    );
    r.f.close().await;
}

#[tokio::test]
async fn pause_stops_the_fix_under_way_resume_carries_it_on_and_stop_ends_it_all() {
    let r = repo(true).await;
    r.set(agents()).await.unwrap();
    use TriageGroup::AgentCanTake;
    r.issues(&[(4, Some(AgentCanTake)), (5, Some(AgentCanTake))]);
    r.run_all(RunStep::Start).await.unwrap();
    let (first, run) = r.next_fix().await;

    r.run_all(RunStep::Pause).await.unwrap();
    assert!(r.issue_run().paused);
    assert!(r.next_up().paused);
    r.exits(run, ports::RunExit::Interrupted).await;
    r.until("the fix didn't stop", |r| {
        r.get(first).state == TaskState::Interrupted
    })
    .await;
    assert_eq!(item_status(&r.get(first)), Some(ItemStatus::Stopped));
    // Paused, nothing follows it and the run waits.
    tokio::time::sleep(Duration::from_millis(50)).await;
    let other = (r.issue_run().fixes.iter())
        .find(|f| f.task_id != first)
        .map(|f| r.get(f.task_id))
        .unwrap();
    assert!(other.current_run.is_none());
    assert!(r.issue_run().ended_at.is_none());

    r.run_all(RunStep::Resume).await.unwrap();
    r.until("the fix wasn't carried on", |r| {
        r.get(first).current_run.is_some_and(|c| c != run)
    })
    .await;
    let again = run_started(&r.f, first).await;
    assert!(
        (r.spec(again).prompt.text).contains("Carry on where you stopped."),
        "{:?}",
        r.spec(again)
    );
    assert!(!r.issue_run().paused && !r.next_up().paused);

    // Stop ends the run at once, takes out the fix that hadn't started, and
    // stops the one under way.
    r.run_all(RunStep::Stop).await.unwrap();
    let ended = r.issue_run();
    assert!(ended.stopped && ended.ended_at.is_some());
    assert_eq!(r.get(other.task_id).state, TaskState::Archived);
    assert!(!(r.next_up().items.iter()).any(|i| i.task_id == other.task_id && i.is_pending()));
    r.exits(again, ports::RunExit::Interrupted).await;
    r.until("the fix didn't stop", |r| {
        r.get(first).state == TaskState::Interrupted
    })
    .await;
    r.f.close().await;
}

#[tokio::test]
async fn issues_not_sorted_are_sorted_first_and_what_the_agent_can_take_joins_the_run() {
    let r = repo(true).await;
    r.set(agents()).await.unwrap();
    r.issues(&[(8, None)]);
    r.gh.answer(
        "GET /repos/octo/app/issues?state=open&sort=updated&per_page=100&page=1",
        json!([{ "number": 8, "title": "Crash on save", "user": { "login": "me" }, "author_association": "OWNER",
                 "author_association": "OWNER", "labels": [], "body": "Steps." }]),
    );
    r.run_all(RunStep::Start).await.unwrap();
    let run = r.issue_run();
    assert!(run.fixes.is_empty());
    let sort = r.get(run.sorting.unwrap());
    assert_eq!(sort.mode, TaskMode::Plan);
    assert_eq!(sort.github.as_deref().unwrap().link, GithubLink::Triage);
    let sort_run = run_started(&r.f, sort.task_id).await;
    let said = "```ophio-triage\n[{\"kind\": \"issue\", \"number\": 8, \"group\": \"agent_can_take\", \"note\": \"Clear steps.\"}]\n```";
    r.ends(sort.task_id, sort_run, Some(said)).await;
    r.until("the sorted issue didn't join", |r| r.fix(8).is_some())
        .await;
    let (task, run) = r.next_fix().await;
    assert_eq!(r.fix(8).unwrap().task_id, task);
    assert!(r.issue_run().sorting.is_none());
    r.ends(task, run, Some("Done.")).await;
    r.until("the run didn't end", |r| r.issue_run().ended_at.is_some())
        .await;
    r.f.close().await;
}

#[tokio::test]
async fn a_sort_that_finds_nothing_to_take_ends_the_run() {
    let r = repo(true).await;
    r.set(agents()).await.unwrap();
    r.issues(&[(8, None)]);
    r.gh.answer(
        "GET /repos/octo/app/issues?state=open&sort=updated&per_page=100&page=1",
        json!([{ "number": 8, "title": "Pick a colour", "user": { "login": "me" }, "author_association": "OWNER",
                 "author_association": "OWNER", "labels": [], "body": "Which?" }]),
    );
    r.run_all(RunStep::Start).await.unwrap();
    let sort = r.issue_run().sorting.unwrap();
    let sort_run = run_started(&r.f, sort).await;
    let said = "```ophio-triage\n[{\"kind\": \"issue\", \"number\": 8, \"group\": \"needs_you\", \"note\": \"Which colour?\"}]\n```";
    r.ends(sort, sort_run, Some(said)).await;
    r.until("the run didn't end", |r| r.issue_run().ended_at.is_some())
        .await;
    assert!(r.issue_run().fixes.is_empty());
    r.f.close().await;
}

#[tokio::test]
async fn where_the_person_puts_an_issue_stands_over_the_sort() {
    let r = repo(true).await;
    r.set(agents()).await.unwrap();
    use TriageGroup::{AgentCanTake, NeedsYou};
    r.issues(&[(4, Some(AgentCanTake)), (5, None)]);
    let fix = r
        .link(
            GithubLink::Issue {
                number: 4,
                review_with: None,
                when_green: None,
            },
            "Fix it",
        )
        .await
        .unwrap();
    let project_id = r.project_id;
    let group = |number: u64, group: Option<TriageGroup>| GithubAction::SetGroup {
        project_id,
        number,
        pr: false,
        group,
    };
    let owner = Caller::local_owner();
    // Told to wait for the person, its fix that hadn't started is set aside.
    r.act(&owner, group(4, Some(NeedsYou))).await.unwrap();
    assert_eq!(r.get(fix.task_id).state, TaskState::Archived);
    assert!(
        !(r.next_up().items.iter()).any(|i| i.task_id == fix.task_id),
        "{:?}",
        r.next_up()
    );
    r.act(&owner, group(5, Some(AgentCanTake))).await.unwrap();
    let kept: Vec<Triaged> = (r.f.hub).read(|x| x.require(TRIAGE, project_id)).unwrap();
    assert!(kept.iter().all(|t| t.by_you), "{kept:?}");

    // A sort after doesn't move them back.
    r.gh.answer(
        "GET /repos/octo/app/issues?state=open&sort=updated&per_page=100&page=1",
        json!([]),
    );
    let (task, run) = r.start(GithubLink::Triage, "Sort them").await;
    let said = "```ophio-triage\n[{\"kind\": \"issue\", \"number\": 4, \"group\": \"agent_can_take\", \"note\": \"Easy.\"}, {\"kind\": \"issue\", \"number\": 5, \"group\": \"needs_you\", \"note\": \"Which?\"}]\n```";
    r.ends(task, run, Some(said)).await;
    let kept: Vec<Triaged> = (r.f.hub).read(|x| x.require(TRIAGE, project_id)).unwrap();
    let group_of = |number: u64| kept.iter().find(|t| t.number == number).map(|t| t.group);
    assert_eq!(group_of(4), Some(NeedsYou));
    assert_eq!(group_of(5), Some(AgentCanTake));
    assert_eq!(kept.len(), 2);

    // Back to Not sorted, the next sort decides.
    r.act(&owner, group(5, None)).await.unwrap();
    let kept: Vec<Triaged> = (r.f.hub).read(|x| x.require(TRIAGE, project_id)).unwrap();
    assert!(!kept.iter().any(|t| t.number == 5));
    r.f.close().await;
}

#[tokio::test]
async fn an_issue_an_agent_checked_before_waits_for_the_person() {
    let r = repo(true).await;
    r.set(agents()).await.unwrap();
    use TriageGroup::{AgentCanTake, NeedsYou};
    r.issues(&[(4, Some(AgentCanTake)), (5, Some(AgentCanTake)), (6, None)]);
    // #4 and #6 carry the label a fixer leaves when it couldn't reproduce
    // an issue or found it fixed; their tasks are archived.
    let node = |number: u64, labels: Value| {
        json!({
            "number": number, "title": format!("Item {number}"),
            "url": format!("https://github.com/octo/app/issues/{number}"),
            "updatedAt": "2026-10-02T10:00:00Z", "authorAssociation": "OWNER",
            "author": { "login": "me" }, "labels": { "nodes": labels },
        })
    };
    let checked = json!([{ "name": "agent" }, { "name": "agent-not-reproduced" }]);
    r.gh.query(
        "issues(states:OPEN",
        json!({
            "viewer": { "login": "me" },
            "repository": { "issues": { "totalCount": 3, "nodes": [
                node(4, checked.clone()), node(5, json!([])), node(6, checked),
            ] } },
        }),
    );
    let project_id = r.project_id;
    let items = || {
        let hub = r.f.hub.clone();
        async move {
            let items = hub.github_items(project_id, GithubItemKind::Issues, None);
            items.await.unwrap().items
        }
    };
    let listed = items().await;
    let item = |number: u64| listed.iter().find(|i| i.number == number).unwrap().clone();
    for number in [4, 6] {
        assert_eq!(item(number).group, Some(NeedsYou));
        assert!(
            item(number)
                .triage_note
                .unwrap()
                .contains("checked this before")
        );
        assert!(!item(number).grouped_by_you);
    }
    assert_eq!(item(5).group, Some(AgentCanTake));

    // Run all leaves them to the person.
    r.run_all(RunStep::Start).await.unwrap();
    r.next_fix().await;
    let fixed: Vec<u64> = r.issue_run().fixes.iter().map(|f| f.number).collect();
    assert_eq!(fixed, [5]);

    // Put back by the person, it's theirs to say.
    let back = GithubAction::SetGroup {
        project_id,
        number: 4,
        pr: false,
        group: Some(AgentCanTake),
    };
    r.act(&Caller::local_owner(), back).await.unwrap();
    let listed = items().await;
    let four = listed.iter().find(|i| i.number == 4).unwrap();
    assert_eq!(four.group, Some(AgentCanTake));
    assert!(four.grouped_by_you);
    r.f.close().await;
}

#[tokio::test]
async fn a_fix_asked_to_wait_leaves_the_run() {
    let r = repo(true).await;
    r.set(agents()).await.unwrap();
    use TriageGroup::AgentCanTake;
    r.issues(&[(4, Some(AgentCanTake)), (5, Some(AgentCanTake))]);
    r.run_all(RunStep::Start).await.unwrap();
    let (first, run) = r.next_fix().await;
    let other = (r.issue_run().fixes.iter())
        .find(|f| f.task_id != first)
        .unwrap()
        .task_id;
    let item = (r.next_up().items.iter())
        .find(|i| i.task_id == other)
        .unwrap()
        .item_id;
    // Run all keeps its go-ahead as Next up is edited around it…
    let queue = r.next_up();
    assert_eq!(
        (queue.items.iter())
            .find(|i| i.item_id == item)
            .unwrap()
            .authorization,
        QueueAuthorization::AutoStart
    );
    // …but not once the person asks it to wait.
    let ask = edit(
        Command::QueueSetAuthorization {
            item_id: item,
            authorization: QueueAuthorization::AskBeforeStart,
        },
        queue.revision,
    );
    let receipt = r.f.hub.submit(&Caller::local_owner(), ask).await;
    assert!(receipt.error.is_none(), "{receipt:?}");
    assert!(!r.issue_run().fixes.iter().any(|f| f.task_id == other));
    r.ends(first, run, Some("Done.")).await;
    r.until("the run didn't end", |r| r.issue_run().ended_at.is_some())
        .await;
    assert!(r.get(other).current_run.is_none());
    r.f.close().await;
}

impl Repo {
    /// A task under way, holding the project's writer until its run ends.
    async fn busy(&self) -> RunId {
        let owner = Caller::local_owner();
        let (task, _) = (self.f)
            .task(self.project_id, TaskPlacement::StartNow, &owner, false)
            .await;
        run_started(&self.f, task).await
    }
    /// What Next up asks the person about, if anything.
    fn asked(&self) -> Option<AttentionItem> {
        let project_id = self.project_id;
        let id = (self
            .f
            .hub
            .read(|r| r.get::<AttentionId>("queue_attention", project_id)))
        .unwrap()?;
        let item: AttentionItem = (self.f.hub.read(|r| r.require("attention", id))).unwrap();
        item.resolved_at.is_none().then_some(item)
    }
}

#[tokio::test]
async fn a_go_ahead_that_no_longer_holds_asks_and_taking_it_out_lets_the_run_go_on() {
    let r = repo(true).await;
    r.set(agents()).await.unwrap();
    let busy = r.busy().await;
    // A task waits to carry on by itself after the one under way…
    let owner = Caller::local_owner();
    let by_itself = TaskPlacement::Queue {
        position: None,
        authorization: QueueAuthorization::AutoStart,
    };
    let (head, _) = r.f.task(r.project_id, by_itself, &owner, false).await;
    use TriageGroup::AgentCanTake;
    r.issues(&[(4, Some(AgentCanTake))]);
    r.run_all(RunStep::Start).await.unwrap();
    // …and changes after its go-ahead was given, so the go-ahead no longer
    // covers it.
    (r.f.hub.write(|tx| {
        let mut task: Task = tx.read().require("task", head)?;
        task.revision += 1;
        tx.put("task", head, &task)
    }))
    .unwrap();
    r.f.hub.finish_run(busy, RunState::Completed, None).unwrap();
    r.f.hub.advance_queues().await.unwrap();
    // It doesn't sit at the front as if about to start, holding up the run
    // with nothing said: the person is asked.
    let queue = r.next_up();
    let front = queue.items.iter().find(|i| i.is_pending()).unwrap();
    assert_eq!(front.task_id, head);
    assert_eq!(front.authorization, QueueAuthorization::AskBeforeStart);
    assert_eq!(front.state, QueueItemState::AwaitingStart);
    assert_eq!(r.asked().and_then(|a| a.task_id), Some(head));
    assert!(r.fix(4).unwrap().current_run.is_none());
    // Taken out of the way, the run goes on with nothing else to do.
    let task = r.get(head);
    let archive = edit(Command::ArchiveTask { task_id: head }, task.revision);
    let receipt = r.f.hub.submit(&owner, archive).await;
    assert!(receipt.error.is_none(), "{receipt:?}");
    let (fix, _) = r.next_fix().await;
    assert_eq!(fix, r.fix(4).unwrap().task_id);
    r.f.close().await;
}

#[tokio::test]
async fn a_go_ahead_outlasts_what_ophio_notes_of_github_meanwhile() {
    let r = repo(true).await;
    r.set(agents()).await.unwrap();
    r.mine(7, "Item 7");
    let busy = r.busy().await;
    let fix = r
        .link(
            GithubLink::Issue {
                number: 7,
                review_with: None,
                when_green: None,
            },
            "Fix it",
        )
        .await
        .unwrap();
    let queue = r.next_up();
    let item = (queue.items.iter())
        .find(|i| i.task_id == fix.task_id)
        .unwrap()
        .item_id;
    let go = edit(
        Command::QueueSetAuthorization {
            item_id: item,
            authorization: QueueAuthorization::AutoStart,
        },
        queue.revision,
    );
    let receipt = r.f.hub.submit(&Caller::local_owner(), go).await;
    assert!(receipt.error.is_none(), "{receipt:?}");
    let resume = edit(
        Command::QueueResume {
            project_id: r.project_id,
        },
        r.next_up().revision,
    );
    let receipt = r.f.hub.submit(&Caller::local_owner(), resume).await;
    assert!(receipt.error.is_none(), "{receipt:?}");
    assert!(r.get(fix.task_id).current_run.is_none());
    // What Ophio notes of GitHub while it waits isn't the person's to
    // approve again.
    r.alter(fix.task_id, |g| g.repo = Some("octo/app".into()));
    r.f.hub.finish_run(busy, RunState::Completed, None).unwrap();
    r.f.hub.advance_queues().await.unwrap();
    run_started(&r.f, fix.task_id).await;
    assert!(r.asked().is_none());
    r.f.close().await;
}

impl Repo {
    /// What Run all told the person, with whether it was quiet.
    fn run_all_told(&self) -> Vec<(String, Option<String>, bool)> {
        let items = self.f.hub.read(|r| r.list::<AttentionItem>("attention"));
        (items.unwrap().into_iter())
            .filter(|a| a.title.starts_with("Run all"))
            .map(|a| (a.title, a.body, a.quiet))
            .collect()
    }
    fn tell_run_all(&self) {
        self.f.hub.tell_run_all(self.project_id).unwrap();
    }
    /// `task`'s items of `kind`, with whether each was quiet.
    fn raised(&self, task: TaskId, kind: AttentionKind) -> Vec<bool> {
        (self.attention(task).into_iter())
            .filter(|a| a.kind == kind)
            .map(|a| a.quiet)
            .collect()
    }
}

#[tokio::test]
async fn run_all_alerts_once_when_every_fix_is_through() {
    let r = repo(true).await;
    r.set(agents()).await.unwrap();
    use TriageGroup::AgentCanTake;
    r.issues(&[(4, Some(AgentCanTake)), (5, Some(AgentCanTake))]);
    r.run_all(RunStep::Start).await.unwrap();
    let mut fixes = Vec::new();
    for _ in 0..2 {
        let (task, run) = r.next_fix().await;
        r.ends(task, run, Some("Done.")).await;
        // Each fix's end is listed, without an alert of its own, and the run
        // isn't told while some of it is still to go.
        assert_eq!(r.raised(task, AttentionKind::Completed), [true]);
        fixes.push(task);
        if fixes.len() == 1 {
            r.tell_run_all();
            assert_eq!(r.run_all_told(), []);
        }
    }
    r.until("the run didn't end", |r| r.issue_run().ended_at.is_some())
        .await;
    r.tell_run_all();
    r.tell_run_all();
    // Told as what the Run all came to, not as a task that finished.
    let items = r.f.hub.read(|r| r.list::<AttentionItem>("attention"));
    let told: Vec<_> = (items.unwrap().into_iter())
        .filter(|a| a.title.starts_with("Run all"))
        .map(|a| protocol::notifications::AlertType::of(&a, false))
        .collect();
    assert_eq!(told, [Some(protocol::notifications::AlertType::Batch)]);
    assert_eq!(
        r.run_all_told(),
        [(
            "Run all done: 2 finished".into(),
            Some("Finished without a pull request to merge: #4 and #5.".into()),
            false
        )]
    );
    r.f.close().await;
}

#[tokio::test]
async fn a_fix_that_fails_alerts_at_once_and_is_counted_when_run_all_is_told() {
    let r = repo(true).await;
    r.set(agents()).await.unwrap();
    use TriageGroup::AgentCanTake;
    r.issues(&[(4, Some(AgentCanTake)), (5, Some(AgentCanTake))]);
    r.run_all(RunStep::Start).await.unwrap();
    let (task, run) = r.next_fix().await;
    r.exits(
        run,
        ports::RunExit::Failed {
            reason: "boom".into(),
        },
    )
    .await;
    r.until("the fix didn't fail", |r| {
        r.get(task).state == TaskState::Failed
    })
    .await;
    assert_eq!(r.raised(task, AttentionKind::Failure), [false]);
    r.run_all(RunStep::Stop).await.unwrap();
    r.tell_run_all();
    let told = r.run_all_told();
    let [(title, _, false)] = told.as_slice() else {
        panic!("{told:?}")
    };
    assert_eq!(title, "Run all stopped: 1 failed, 1 stopped");
    r.f.close().await;
}

#[tokio::test]
async fn stopped_before_anything_came_of_it_run_all_tells_nothing() {
    let r = repo(true).await;
    r.set(agents()).await.unwrap();
    use TriageGroup::AgentCanTake;
    r.issues(&[(4, Some(AgentCanTake)), (5, Some(AgentCanTake))]);
    r.run_all(RunStep::Start).await.unwrap();
    let (task, run) = r.next_fix().await;
    r.run_all(RunStep::Stop).await.unwrap();
    r.exits(run, ports::RunExit::Interrupted).await;
    r.until("the fix didn't stop", |r| {
        r.get(task).state == TaskState::Interrupted
    })
    .await;
    r.tell_run_all();
    assert_eq!(r.run_all_told(), []);
    r.f.close().await;
}

/// Run all offered on a chat's card starts from that card once, only by the
/// person, and never from a chat that has been archived since.
#[tokio::test]
async fn run_all_from_a_chats_card_starts_once_and_not_from_an_archived_chat() {
    use protocol::work_cards::TaskOrigin;
    let r = repo(true).await;
    r.set(agents()).await.unwrap();
    r.issues(&[(4, Some(TriageGroup::AgentCanTake))]);
    let owner = Caller::local_owner();
    // A chat whose answer offered to fix every issue, and one that offered a task.
    let mut chats = vec![];
    for all_issues in [true, false] {
        let (chat, _) = (r.f)
            .task(r.project_id, TaskPlacement::StartNow, &owner, false)
            .await;
        let task: Task = r.f.hub.read(|x| x.require("task", chat)).unwrap();
        let run = task.current_run.unwrap();
        (r.f.hub)
            .write(|tx| {
                let mut task: Task = tx.read().require("task", chat)?;
                task.conversation = true;
                tx.put("task", chat, &task)
            })
            .unwrap();
        let task: Task = r.f.hub.read(|x| x.require("task", chat)).unwrap();
        let tasks = if all_issues {
            vec![]
        } else {
            vec![json!("Something else")]
        };
        let call = ports::bridge::WorkCall::Propose {
            tasks,
            in_order: false,
            all_issues,
        };
        r.f.hub
            .work_request(run, &task, call.clone())
            .await
            .unwrap();
        if all_issues {
            // Once a turn: the chat hears one Run all report for it.
            let again = r.f.hub.work_request(run, &task, call).await.unwrap_err();
            assert!(
                again
                    .message
                    .contains("already proposed fixing every issue"),
                "{again:?}"
            );
        }
        r.f.hub.finish_run(run, RunState::Completed, None).unwrap();
        chats.push((chat, run));
    }
    let start = |origin: TaskOrigin| GithubAction::RunIssues {
        project_id: r.project_id,
        step: RunStep::Start,
        origin: Some(origin),
    };
    let card = |(chat, run): (TaskId, RunId)| TaskOrigin {
        chat,
        run,
        index: 0,
        auto: false,
        reviewed: false,
    };
    // Only the person, and only what the card offered.
    let auto = TaskOrigin {
        auto: true,
        reviewed: false,
        ..card(chats[0])
    };
    assert_eq!(
        r.act(&owner, start(auto)).await,
        Err(ErrorCode::InvalidRequest)
    );
    assert_eq!(
        r.act(&owner, start(card(chats[1]))).await,
        Err(ErrorCode::InvalidRequest)
    );
    // An archived chat's card starts nothing.
    let archive = |state| {
        let chat = chats[0].0;
        (r.f.hub)
            .write(move |tx| {
                let mut task: Task = tx.read().require("task", chat)?;
                task.state = state;
                tx.put("task", chat, &task)
            })
            .unwrap();
    };
    archive(TaskState::Archived);
    assert_eq!(
        r.act(&owner, start(card(chats[0]))).await,
        Err(ErrorCode::InvalidRequest)
    );
    assert!(
        r.f.hub
            .read(|x| crate::github_run::issue_run(x, r.project_id))
            .unwrap()
            .is_none()
    );
    archive(TaskState::Completed);
    r.act(&owner, start(card(chats[0]))).await.unwrap();
    assert_eq!(r.issue_run().origin, Some(card(chats[0])));
    // Started once: its card can't start it again.
    assert_eq!(
        r.act(&owner, start(card(chats[0]))).await,
        Err(ErrorCode::Conflict)
    );
    r.f.close().await;
}

/// A finished but untold run is folded into the next one. The chat that
/// started it must hear the combined result even if Tasks starts the next.
#[tokio::test]
async fn an_untold_chats_run_follows_a_later_tasks_run_into_its_chat() {
    let r = repo(true).await;
    r.set(agents()).await.unwrap();
    let origin = run_all_card(&r).await;
    r.issues(&[(4, Some(TriageGroup::AgentCanTake))]);
    start_from_card(&r, origin.clone()).await;
    let (first, first_run) = r.next_fix().await;
    r.run_all(RunStep::Stop).await.unwrap();
    r.issues(&[(5, Some(TriageGroup::AgentCanTake))]);
    r.run_all(RunStep::Start).await.unwrap();
    r.exits(first_run, ports::RunExit::Interrupted).await;
    r.until("the first fix didn't stop", |r| {
        r.get(first).state == TaskState::Interrupted
    })
    .await;
    r.f.hub.advance_queues().await.unwrap();
    let (second, second_run) = r.next_fix().await;
    r.ends(second, second_run, Some("Done.")).await;
    r.until("the second run didn't end", |r| {
        r.issue_run().ended_at.is_some()
    })
    .await;
    r.tell_run_all();
    r.tell_run_all();
    assert_eq!(run_all_notes(&r, origin.chat).len(), 1);
    assert!(run_all_notes(&r, origin.chat)[0].contains("#4"));
    assert!(run_all_notes(&r, origin.chat)[0].contains("#5"));
    let news =
        r.f.hub
            .read(|x| crate::chat_news::pending(x, origin.chat))
            .unwrap();
    assert_eq!(news.len(), 1);
    assert_eq!(news[0].text, run_all_notes(&r, origin.chat)[0]);
    r.f.close().await;
}

#[tokio::test]
async fn two_chats_hear_the_same_combined_run_all_report_once_each() {
    let r = repo(true).await;
    r.set(agents()).await.unwrap();
    let first_chat = run_all_card(&r).await;
    let second_chat = run_all_card(&r).await;
    r.issues(&[(4, Some(TriageGroup::AgentCanTake))]);
    start_from_card(&r, first_chat.clone()).await;
    let (first, first_run) = r.next_fix().await;
    r.run_all(RunStep::Stop).await.unwrap();
    r.issues(&[(5, Some(TriageGroup::AgentCanTake))]);
    start_from_card(&r, second_chat.clone()).await;
    r.exits(first_run, ports::RunExit::Interrupted).await;
    r.until("the first fix didn't stop", |r| {
        r.get(first).state == TaskState::Interrupted
    })
    .await;
    r.f.hub.advance_queues().await.unwrap();
    let (second, second_run) = r.next_fix().await;
    r.ends(second, second_run, Some("Done.")).await;
    r.until("the second run didn't end", |r| {
        r.issue_run().ended_at.is_some()
    })
    .await;
    r.tell_run_all();
    r.tell_run_all();
    let first_notes = run_all_notes(&r, first_chat.chat);
    let second_notes = run_all_notes(&r, second_chat.chat);
    assert_eq!(first_notes, second_notes);
    assert_eq!(first_notes.len(), 1);
    assert!(first_notes[0].contains("#4") && first_notes[0].contains("#5"));
    for origin in [first_chat, second_chat] {
        let news =
            r.f.hub
                .read(|x| crate::chat_news::pending(x, origin.chat))
                .unwrap();
        assert_eq!(news.len(), 1);
        assert_eq!(news[0].text, first_notes[0]);
    }
    r.f.close().await;
}

#[tokio::test]
async fn tasks_only_run_all_does_not_post_to_any_chat() {
    let r = repo(true).await;
    r.set(agents()).await.unwrap();
    let chat = run_all_card(&r).await;
    r.issues(&[(4, Some(TriageGroup::AgentCanTake))]);
    r.run_all(RunStep::Start).await.unwrap();
    let (fix, fix_run) = r.next_fix().await;
    r.ends(fix, fix_run, Some("Done.")).await;
    r.until("the run didn't end", |r| r.issue_run().ended_at.is_some())
        .await;
    r.tell_run_all();
    assert!(run_all_notes(&r, chat.chat).is_empty());
    assert!(
        r.f.hub
            .read(|x| crate::chat_news::pending(x, chat.chat))
            .unwrap()
            .is_empty()
    );
    r.f.close().await;
}

async fn run_all_card(r: &Repo) -> protocol::work_cards::TaskOrigin {
    let owner = Caller::local_owner();
    let (chat, _) =
        r.f.task(r.project_id, TaskPlacement::StartNow, &owner, false)
            .await;
    let task: Task = r.f.hub.read(|x| x.require("task", chat)).unwrap();
    let run = task.current_run.unwrap();
    r.f.hub
        .write(|tx| {
            let mut task: Task = tx.read().require("task", chat)?;
            task.conversation = true;
            tx.put("task", chat, &task)
        })
        .unwrap();
    let task: Task = r.f.hub.read(|x| x.require("task", chat)).unwrap();
    r.f.hub
        .work_request(
            run,
            &task,
            ports::bridge::WorkCall::Propose {
                tasks: vec![],
                in_order: false,
                all_issues: true,
            },
        )
        .await
        .unwrap();
    r.f.hub.finish_run(run, RunState::Completed, None).unwrap();
    protocol::work_cards::TaskOrigin {
        chat,
        run,
        index: 0,
        auto: false,
        reviewed: false,
    }
}

async fn start_from_card(r: &Repo, origin: protocol::work_cards::TaskOrigin) {
    r.act(
        &Caller::local_owner(),
        GithubAction::RunIssues {
            project_id: r.project_id,
            step: RunStep::Start,
            origin: Some(origin),
        },
    )
    .await
    .unwrap();
}

fn run_all_notes(r: &Repo, chat: TaskId) -> Vec<String> {
    let (items, _) = r.f.hub.read(|x| x.timeline(chat, None, 500)).unwrap();
    items
        .into_iter()
        .filter_map(|item| match item.item {
            TimelineBody::Note { text } if text.starts_with("Run all") => Some(text),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn run_all_card_failed_admission_can_be_retried_successfully() {
    let r = repo(true).await;
    r.set(agents()).await.unwrap();
    r.issues(&[(4, Some(TriageGroup::AgentCanTake))]);
    let origin = run_all_card(&r).await;
    r.gh.refuse("GET /repos/octo/app/issues/4", 502);
    let caller = Caller::local_owner();
    let failed =
        r.f.hub
            .run_issues(&caller, r.project_id, RunStep::Start, Some(&origin))
            .await
            .unwrap_err();
    assert_eq!(failed.code, ErrorCode::NativeFailure);
    let key = format!("{}/{}/{}", origin.chat, origin.run, origin.index);
    assert!(
        r.f.hub
            .read(|x| x.get::<TaskId>("task_origin", &key))
            .unwrap()
            .is_none()
    );
    assert!(r.issue_run().origin.is_none());
    assert!(r.issue_run().ended_at.is_some());
    r.tell_run_all();
    assert!(run_all_notes(&r, origin.chat).is_empty());
    r.mine(4, "Item 4");
    // No clock advance: the retry must not replay the rejected fix command.
    start_from_card(&r, origin.clone()).await;
    assert_eq!(r.issue_run().origin, Some(origin));
    assert_eq!(r.issue_run().fixes.len(), 1);
    assert!(r.fix(4).is_some());
    assert!(
        r.f.hub
            .read(|x| x.get::<TaskId>("task_origin", &key))
            .unwrap()
            .is_some()
    );
    r.f.close().await;
}

#[tokio::test]
async fn run_all_card_empty_result_can_be_retried_without_a_chat_completion() {
    let r = repo(true).await;
    r.set(agents()).await.unwrap();
    r.issues(&[]);
    let origin = run_all_card(&r).await;
    let caller = Caller::local_owner();
    let empty =
        r.f.hub
            .run_issues(&caller, r.project_id, RunStep::Start, Some(&origin))
            .await
            .unwrap_err();
    assert_eq!(empty.code, ErrorCode::InvalidRequest);
    assert!(empty.message.starts_with("Nothing to fix:"));
    let key = format!("{}/{}/{}", origin.chat, origin.run, origin.index);
    assert!(
        r.f.hub
            .read(|x| x.get::<TaskId>("task_origin", &key))
            .unwrap()
            .is_none()
    );
    assert!(r.issue_run().origin.is_none());
    r.tell_run_all();
    r.tell_run_all();
    assert!(run_all_notes(&r, origin.chat).is_empty());
    assert!(r.run_all_told().is_empty());
    r.issues(&[(4, Some(TriageGroup::AgentCanTake))]);
    start_from_card(&r, origin.clone()).await;
    assert_eq!(r.issue_run().origin, Some(origin));
    assert_eq!(r.issue_run().fixes.len(), 1);
    assert!(r.fix(4).is_some());
    r.f.close().await;
}

#[tokio::test]
async fn run_all_card_concurrent_starts_consume_it_once() {
    let r = repo(true).await;
    r.set(agents()).await.unwrap();
    r.issues(&[(4, Some(TriageGroup::AgentCanTake)), (5, None)]);
    let origin = run_all_card(&r).await;
    let (arrived, release) = r.gh.pause("/issues/4");
    let caller = Caller::local_owner();
    let first =
        r.f.hub
            .run_issues(&caller, r.project_id, RunStep::Start, Some(&origin));
    let second = async {
        arrived.await.unwrap();
        let result =
            r.f.hub
                .run_issues(&caller, r.project_id, RunStep::Start, Some(&origin))
                .await;
        release.send(()).unwrap();
        result
    };
    let (first, second) = tokio::join!(first, second);
    let results = [first, second];
    assert_eq!(
        results.iter().filter(|r| r.is_ok()).count(),
        1,
        "{results:?}"
    );
    let refused = results.iter().find_map(|r| r.as_ref().err()).unwrap();
    assert_eq!(refused.code, ErrorCode::Conflict);
    assert_eq!(refused.message, "This was already started.");
    assert_eq!(r.issue_run().origin, Some(origin.clone()));
    assert_eq!(r.issue_run().fixes.len(), 1);
    let tasks = r.f.hub.read(|x| x.list::<Task>("task")).unwrap();
    assert_eq!(
        tasks
            .iter()
            .filter(|t| t.github.as_deref().is_some_and(|g| g.link
                == GithubLink::Issue {
                    number: 4,
                    review_with: Some(claude()),
                    when_green: None
                }))
            .count(),
        1
    );
    let sorts: Vec<_> = tasks
        .iter()
        .filter(|t| {
            t.github
                .as_deref()
                .is_some_and(|g| g.link == GithubLink::Triage)
        })
        .collect();
    assert_eq!(
        sorts.len(),
        1,
        "A refused start must not admit another Sort"
    );
    assert_eq!(r.issue_run().sorting, Some(sorts[0].task_id));
    let again =
        r.f.hub
            .run_issues(&caller, r.project_id, RunStep::Start, Some(&origin))
            .await
            .unwrap_err();
    assert_eq!(again.code, ErrorCode::Conflict);
    r.f.close().await;
}

#[tokio::test]
async fn a_run_all_fix_and_its_chat_survive_restart_before_start_finishes() {
    let r = repo(true).await;
    r.set(agents()).await.unwrap();
    r.issues(&[
        (4, Some(TriageGroup::AgentCanTake)),
        (5, Some(TriageGroup::AgentCanTake)),
    ]);
    let origin = run_all_card(&r).await;
    let first;
    {
        // The first task has been admitted, but the second is still being
        // prepared. Dropping Start here models losing the process before
        // start_run_all's final membership write.
        let (arrived, _release) = r.gh.pause("GET /repos/octo/app/issues/5");
        let caller = Caller::local_owner();
        let start =
            r.f.hub
                .run_issues(&caller, r.project_id, RunStep::Start, Some(&origin));
        tokio::pin!(start);
        tokio::select! {
            result = arrived => result.unwrap(),
            result = &mut start => panic!("Start finished before the second admission: {result:?}"),
        }
        let tasks = r.f.hub.read(|x| x.list::<Task>("task")).unwrap();
        first = tasks
            .iter()
            .find(|t| {
                t.github
                    .as_deref()
                    .is_some_and(|g| matches!(g.link, GithubLink::Issue { number: 4, .. }))
            })
            .unwrap()
            .task_id;
        assert_eq!(r.issue_run().fixes.len(), 1);
        assert_eq!(r.issue_run().fixes[0].task_id, first);
        assert_eq!(r.issue_run().origin, Some(origin.clone()));
        let run = run_started(&r.f, first).await;
        r.f.hub.finish_run(run, RunState::Completed, None).unwrap();
    }
    let Repo { f, gh, project_id } = r;
    let port = gh.clone();
    let r = Repo {
        f: f.reopened(|b| b.github(port)).await,
        gh,
        project_id,
    };
    assert_eq!(r.issue_run().fixes.len(), 1);
    assert_eq!(r.issue_run().fixes[0].task_id, first);
    assert_eq!(r.issue_run().origin, Some(origin.clone()));
    // Work really was started, so this card is used; its admitted work must
    // be in the report even though Start never reached its final write.
    let again =
        r.f.hub
            .run_issues(
                &Caller::local_owner(),
                project_id,
                RunStep::Start,
                Some(&origin),
            )
            .await
            .unwrap_err();
    assert_eq!(again.code, ErrorCode::Conflict);
    r.f.clock.advance(Duration::from_secs(11 * 60));
    r.f.hub.advance_queues().await.unwrap();
    assert!(r.issue_run().ended_at.is_some());
    r.tell_run_all();
    r.tell_run_all();
    let notes = run_all_notes(&r, origin.chat);
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(notes[0].contains("#4"), "{notes:?}");
    assert!(!notes[0].contains("#5"), "{notes:?}");
    r.f.close().await;
}
