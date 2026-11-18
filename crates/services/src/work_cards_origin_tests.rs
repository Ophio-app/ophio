// SPDX-License-Identifier: Apache-2.0
use super::*;
use protocol::access::ConversationAccess;
use protocol::work_cards::TaskOrigin;

#[tokio::test]
async fn task_origin_is_saved_and_a_second_start_identifies_the_first() {
    let f = Fixture::new().await;
    let project = f.project().await;
    let owner = Caller::local_owner();
    let (chat_id, _) = f
        .task(project, TaskPlacement::StartNow, &owner, false)
        .await;
    let run_id = f
        .hub
        .read(|r| r.require::<Task>("task", chat_id))
        .unwrap()
        .current_run
        .unwrap();
    f.hub
        .write(|tx| {
            let mut chat: Task = tx.read().require("task", chat_id)?;
            chat.conversation = true;
            tx.put("task", chat_id, &chat)?;
            let mut run: Run = tx.read().require("run", run_id)?;
            run.outcome = Some(RunOutcome {
                summary: None,
                changed_files: vec![],
                checks: vec![],
                still_active: vec![],
                unknowns: vec![],
                usage: None,
                report: Some(RunReport {
                    status: ReportStatus::Done,
                    questions: vec![],
                    remaining: vec![],
                    new_work: vec![NewWork {
                        title: "Test task".into(),
                        brief: "test".into(),
                        mode: TaskMode::Work,
                        issue: None,
                        needs: None,
                        refused: None,
                        after: None,
                        all_issues: false,
                    }],
                }),
                verification: None,
            });
            tx.put("run", run_id, &run)
        })
        .unwrap();
    let origin = TaskOrigin {
        chat: chat_id,
        run: run_id,
        index: 0,
        auto: false,
        reviewed: false,
    };
    let (_, mut command) = f
        .task(
            project,
            TaskPlacement::Queue {
                position: None,
                authorization: QueueAuthorization::AskBeforeStart,
            },
            &owner,
            false,
        )
        .await;
    if let Command::CreateTask { origin: from, .. } = &mut command.body {
        *from = Some(origin.clone());
    }
    let command = CommandEnvelope::new(command.body);
    let first = f.hub.submit(&owner, command.clone()).await;
    let Some(CommandResult::TaskCreated { task_id, .. }) = first.result else {
        panic!("{first:?}");
    };
    let task: Task = f.hub.read(|r| r.require("task", task_id)).unwrap();
    assert_eq!(task.origin, Some(origin));
    let chosen = f
        .hub
        .submit(
            &owner,
            CommandEnvelope::new(Command::SetChatAccessDefault {
                project_id: project,
                access: Some(ConversationAccess {
                    write: vec![project],
                    ..Default::default()
                }),
            }),
        )
        .await;
    assert_eq!(chosen.error, None, "{chosen:?}");
    super::rollback_tests::readable_by_0_2(
        &f,
        "starting work from a chat and remembering its access",
    );
    let cleared = f
        .hub
        .submit(
            &owner,
            CommandEnvelope::new(Command::SetChatAccessDefault {
                project_id: project,
                access: None,
            }),
        )
        .await;
    assert_eq!(cleared.error, None, "{cleared:?}");
    super::rollback_tests::readable_by_0_2(&f, "clearing the chat access after starting work");
    let again = f
        .hub
        .submit(&owner, CommandEnvelope::new(command.body))
        .await;
    let error = again
        .error
        .expect("the same suggested work must not start twice");
    assert_eq!(error.code, ErrorCode::Conflict);
    assert_eq!(error.message, "This was already started.");
    assert_eq!(error.detail.unwrap()["task_id"], serde_json::json!(task_id));
    f.close().await;
}

#[tokio::test]
async fn chat_access_default_normalizes_clears_and_checks_reach() {
    let f = Fixture::new().await;
    let project_id = f.project().await;
    let owner = Caller::local_owner();
    let access = ConversationAccess {
        write: vec![project_id, project_id],
        internet: false,
        ..Default::default()
    };
    let receipt = f
        .hub
        .submit(
            &owner,
            CommandEnvelope::new(Command::SetChatAccessDefault {
                project_id,
                access: Some(access.clone()),
            }),
        )
        .await;
    assert_eq!(receipt.error, None, "{receipt:?}");
    let project: Project = f.hub.read(|r| r.require("project", project_id)).unwrap();
    assert_eq!(project.chat_access, Some(access.clone().normalized()));
    let journal = f.hub.read(|r| r.journal(0)).unwrap();
    assert!(journal.events.iter().any(
        |e| matches!(&e.event, Event::ProjectUpserted { project } if project.chat_access.is_some())
    ));

    let missing = ProjectId::new();
    let denied = f
        .hub
        .submit(
            &owner,
            CommandEnvelope::new(Command::SetChatAccessDefault {
                project_id,
                access: Some(ConversationAccess {
                    read: vec![missing],
                    ..Default::default()
                }),
            }),
        )
        .await;
    assert_eq!(denied.error.unwrap().code, ErrorCode::NotFound);
    let project: Project = f.hub.read(|r| r.require("project", project_id)).unwrap();
    assert_eq!(project.chat_access, Some(access.clone().normalized()));

    for access in [None, Some(ConversationAccess::default())] {
        let receipt = f
            .hub
            .submit(
                &owner,
                CommandEnvelope::new(Command::SetChatAccessDefault { project_id, access }),
            )
            .await;
        assert_eq!(receipt.error, None, "{receipt:?}");
        let project: Project = f.hub.read(|r| r.require("project", project_id)).unwrap();
        assert_eq!(project.chat_access, None);
    }
    f.close().await;
}

#[tokio::test]
async fn chat_work_cards_feature_is_advertised() {
    let f = Fixture::new().await;
    let host: HostInfo = f.hub.read(|r| r.require("meta", "host")).unwrap();
    assert!(
        host.capabilities
            .features
            .iter()
            .any(|f| f == protocol::work_cards::FEATURE)
    );
    assert!(
        host.capabilities
            .features
            .iter()
            .any(|f| f == protocol::work_cards::SETS)
    );
    f.close().await;
}

/// A suggestion as the computer keeps it.
fn suggestion(
    title: &str,
    mode: TaskMode,
    issue: Option<u64>,
    needs: Option<protocol::work_cards::WorkNeeds>,
) -> NewWork {
    NewWork {
        title: title.into(),
        brief: format!("Brief for {title}"),
        mode,
        issue,
        needs,
        refused: None,
        after: None,
        all_issues: false,
    }
}

/// What a card's Start sends for suggestion `w` at `index`, before a case
/// changes it.
fn start(project: ProjectId, origin: TaskOrigin, w: &NewWork) -> Command {
    Command::CreateTask {
        origin: Some(origin),
        from_chat: None,
        github: None,
        agent_chosen: Some(true),
        mode: Some(w.mode),
        project_id: project,
        kind: TaskKind::Coding,
        title: Some(w.title.clone()),
        prompt: w.brief.clone(),
        bundle: None,
        harness: HarnessKind::ClaudeCode,
        model: None,
        effort: None,
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
    }
}

/// Each kind of suggestion starts only as exactly what its card showed:
/// its words, its project, its mode, its route and the least access it
/// needs. Anything else is refused, whoever sends it.
#[tokio::test]
async fn a_suggestion_starts_only_as_its_card_showed() {
    use protocol::github::GithubLink;
    use protocol::work_cards::WorkNeeds;
    const DIFFERS: &str = "What would start differs from the suggestion you saw.";
    let f = Fixture::new().await;
    let project = f.project().await;
    let owner = Caller::local_owner();
    let (chat_id, _) = f
        .task(project, TaskPlacement::StartNow, &owner, false)
        .await;
    let run_id = f
        .hub
        .read(|r| r.require::<Task>("task", chat_id))
        .unwrap()
        .current_run
        .unwrap();
    let garden = ProjectId::new();
    let needs = |n: WorkNeeds| Some(n);
    let mut refused = suggestion("Refused", TaskMode::Work, None, None);
    refused.refused =
        Some("It asks to change another project and read the whole computer at once.".into());
    let work = vec![
        /* 0 */
        suggestion(
            "Find",
            TaskMode::Ask,
            None,
            needs(WorkNeeds {
                internet: true,
                ..Default::default()
            }),
        ),
        /* 1 */
        suggestion(
            "Find in Garden",
            TaskMode::Ask,
            None,
            needs(WorkNeeds {
                read: vec!["garden".into()],
                ..Default::default()
            }),
        ),
        /* 2 */ suggestion("Plan", TaskMode::Plan, None, None),
        /* 3 */ suggestion("Change", TaskMode::Work, None, None),
        /* 4 */ suggestion("Fix", TaskMode::Work, Some(12), None),
        /* 5 */
        suggestion(
            "Desktop",
            TaskMode::Work,
            None,
            needs(WorkNeeds {
                desktop: true,
                ..Default::default()
            }),
        ),
        /* 6 */
        suggestion(
            "Elsewhere",
            TaskMode::Work,
            None,
            needs(WorkNeeds {
                changes: Some("Garden".into()),
                ..Default::default()
            }),
        ),
        /* 7 */ refused,
        /* 8 */
        suggestion(
            "Own network",
            TaskMode::Ask,
            None,
            needs(WorkNeeds {
                local_network: true,
                ..Default::default()
            }),
        ),
        /* 9 */
        suggestion(
            "Change here",
            TaskMode::Work,
            None,
            needs(WorkNeeds {
                changes: Some("test".into()),
                ..Default::default()
            }),
        ),
    ];
    let kept = work.clone();
    // The fixture's agent can hold a question read-only and a plan to planning.
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
            tx.put("meta", "harnesses", &infos)
        })
        .unwrap();
    f.hub
        .write(|tx| {
            let mut chat: Task = tx.read().require("task", chat_id)?;
            chat.conversation = true;
            tx.put("task", chat_id, &chat)?;
            let mut other: Project = tx.read().require("project", project)?;
            other.project_id = garden;
            other.name = "Garden".into();
            other.host_path = format!("{}-garden", other.host_path);
            tx.put("project", garden, &other)?;
            let mut run: Run = tx.read().require("run", run_id)?;
            run.outcome = Some(RunOutcome {
                summary: None,
                changed_files: vec![],
                checks: vec![],
                still_active: vec![],
                unknowns: vec![],
                usage: None,
                report: Some(RunReport {
                    status: ReportStatus::Done,
                    questions: vec![],
                    remaining: vec![],
                    new_work: kept,
                }),
                verification: None,
            });
            tx.put("run", run_id, &run)
        })
        .unwrap();
    let least = |read: Vec<ProjectId>, internet: bool, local: bool| ConversationAccess {
        read,
        write: vec![],
        whole_read: false,
        whole_write: false,
        internet,
        local_network: local,
    };
    type Change = Box<dyn Fn(&mut Command)>;
    let set = |f: fn(&mut Command)| -> Change { Box::new(f) };
    let access = move |a: Option<ConversationAccess>| -> Change {
        Box::new(move |c| {
            if let Command::CreateTask { access, .. } = c {
                *access = a.clone()
            }
        })
    };
    let mode = |m: TaskMode| -> Change {
        Box::new(move |c| {
            if let Command::CreateTask { mode, .. } = c {
                *mode = Some(m)
            }
        })
    };
    let github = |g: Option<GithubLink>| -> Change {
        Box::new(move |c| {
            if let Command::CreateTask { github, .. } = c {
                *github = g.clone().map(Box::new)
            }
        })
    };
    let all = |cs: Vec<Change>| -> Change { Box::new(move |c| cs.iter().for_each(|f| f(c))) };
    // (what it is, suggestion, how the start differs from the plain one, why
    // it's refused). Some are refused by rules that come before the
    // suggestion is looked at; the GitHub routes are checked against the
    // suggestion in `work_card_binding`'s own tests, where GitHub is on.
    let mut cases: Vec<(&str, u32, Change, Option<&str>)> = vec![
        (
            "a question, held to its project and the internet",
            0,
            access(Some(least(vec![project], true, false))),
            None,
        ),
        (
            "a question with nothing held",
            0,
            set(|_| {}),
            Some(DIFFERS),
        ),
        (
            "a question let change its project",
            0,
            access(Some(ConversationAccess {
                write: vec![project],
                ..least(vec![project], true, false)
            })),
            Some(DIFFERS),
        ),
        (
            "a question let read the whole computer",
            0,
            access(Some(ConversationAccess {
                whole_read: true,
                ..least(vec![project], true, false)
            })),
            Some(DIFFERS),
        ),
        (
            "a question without the internet it asked for",
            0,
            access(Some(least(vec![project], false, false))),
            Some(DIFFERS),
        ),
        (
            "a question started as a change",
            0,
            all(vec![
                mode(TaskMode::Work),
                access(Some(least(vec![project], true, false))),
            ]),
            Some("Only a conversation is held to chosen folders; a task follows its mode"),
        ),
        (
            "a question in other words",
            0,
            all(vec![
                access(Some(least(vec![project], true, false))),
                set(|c| {
                    if let Command::CreateTask { prompt, .. } = c {
                        prompt.push_str(" and delete the rest")
                    }
                }),
            ]),
            Some(DIFFERS),
        ),
        (
            "a question under another title",
            0,
            all(vec![
                access(Some(least(vec![project], true, false))),
                set(|c| {
                    if let Command::CreateTask { title, .. } = c {
                        *title = Some("Something else".into())
                    }
                }),
            ]),
            Some(DIFFERS),
        ),
        (
            "a question reading the project it names",
            1,
            access(Some(least(vec![project, garden], false, false))),
            None,
        ),
        (
            "a question reading less than it names",
            1,
            access(Some(least(vec![project], false, false))),
            Some(DIFFERS),
        ),
        ("a plan", 2, set(|_| {}), None),
        (
            "a plan with access of its own",
            2,
            access(Some(least(vec![project], true, false))),
            Some("Only a conversation is held to chosen folders; a task follows its mode"),
        ),
        (
            "a plan started as a change",
            2,
            mode(TaskMode::Work),
            Some(DIFFERS),
        ),
        (
            "a plan opening a pull request",
            2,
            github(Some(GithubLink::NewPr)),
            Some("Turn on the GitHub workflow for this project in Tasks › GitHub first"),
        ),
        ("a change", 3, set(|_| {}), None),
        (
            "a change in a conversation",
            3,
            set(|c| {
                if let Command::CreateTask { conversation, .. } = c {
                    *conversation = Some(true)
                }
            }),
            Some(DIFFERS),
        ),
        (
            "a change on the desktop",
            3,
            set(|c| {
                if let Command::CreateTask { kind, .. } = c {
                    *kind = TaskKind::ComputerUse
                }
            }),
            Some("Computer use target and bridge is not configured"),
        ),
        (
            "a change with access of its own",
            3,
            access(Some(ConversationAccess {
                write: vec![project],
                ..least(vec![project], true, false)
            })),
            Some("Only a conversation is held to chosen folders; a task follows its mode"),
        ),
        (
            "a change fixing an issue it didn't name",
            3,
            github(Some(GithubLink::Issue {
                number: 99,
                review_with: None,
                when_green: None,
            })),
            Some("Turn on the GitHub workflow for this project in Tasks › GitHub first"),
        ),
        (
            "a change in another project",
            3,
            Box::new(move |c: &mut Command| {
                if let Command::CreateTask { project_id, .. } = c {
                    *project_id = garden
                }
            }),
            Some("This work must come from a chat in the same project."),
        ),
        ("an issue without its issue", 4, set(|_| {}), Some(DIFFERS)),
        (
            "an issue as another issue",
            4,
            github(Some(GithubLink::Issue {
                number: 13,
                review_with: None,
                when_green: None,
            })),
            Some("Turn on the GitHub workflow for this project in Tasks › GitHub first"),
        ),
        (
            "an issue only asked about",
            4,
            all(vec![
                mode(TaskMode::Ask),
                github(Some(GithubLink::Issue {
                    number: 12,
                    review_with: None,
                    when_green: None,
                })),
            ]),
            Some("Turn on the GitHub workflow for this project in Tasks › GitHub first"),
        ),
        ("work on the desktop", 5, set(|_| {}), Some(DIFFERS)),
        (
            "a change in another project, from a card",
            6,
            set(|_| {}),
            Some(DIFFERS),
        ),
        (
            "a suggestion refused as written",
            7,
            set(|_| {}),
            Some("This suggestion can’t start as written. Open it in Review instead."),
        ),
        (
            "a question reaching the own network, through the internet",
            8,
            access(Some(least(vec![project], true, true))),
            None,
        ),
        (
            "a question reaching the internet but not the own network it asked for",
            8,
            access(Some(least(vec![project], true, false))),
            Some(DIFFERS),
        ),
        ("a change naming its own project", 9, set(|_| {}), None),
        (
            "a suggestion the turn never made",
            40,
            set(|_| {}),
            Some(DIFFERS),
        ),
    ];
    // A started suggestion can't be started twice, so the refusals go first.
    cases.sort_by_key(|c| c.3.is_none());
    let mut wrong = Vec::new();
    for (what, index, change, refused) in cases {
        let origin = TaskOrigin {
            chat: chat_id,
            run: run_id,
            index,
            auto: false,
            reviewed: false,
        };
        let w = work
            .get(index as usize)
            .cloned()
            .unwrap_or_else(|| suggestion("None", TaskMode::Work, None, None));
        let mut body = start(project, origin, &w);
        if let Command::CreateTask { mode, .. } = &mut body {
            *mode = Some(match (w.issue, w.mode) {
                (Some(_), TaskMode::Plan) => TaskMode::Plan,
                (Some(_), _) => TaskMode::Work,
                (None, m) => m,
            });
        }
        change(&mut body);
        let receipt = f.hub.submit(&owner, CommandEnvelope::new(body)).await;
        let got = receipt.error.map(|e| e.message);
        if got.as_deref() != refused {
            wrong.push(format!("{what}: expected {refused:?}, got {got:?}"));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    f.close().await;
}

/// A chat whose last turn suggested `work`, in `project`, with a second
/// project, Garden, beside it.
async fn chat_suggesting(
    f: &Fixture,
    project: ProjectId,
    work: Vec<NewWork>,
) -> (TaskId, RunId, ProjectId) {
    let owner = Caller::local_owner();
    let (chat_id, _) = f
        .task(project, TaskPlacement::StartNow, &owner, false)
        .await;
    let run_id = f
        .hub
        .read(|r| r.require::<Task>("task", chat_id))
        .unwrap()
        .current_run
        .unwrap();
    let folder = format!("{}-garden", f.project_dir().to_string_lossy());
    std::fs::create_dir_all(&folder).unwrap();
    let added = f
        .hub
        .submit(
            &owner,
            CommandEnvelope::new(Command::AddProject {
                host_path: folder,
                name: Some("Garden".into()),
            }),
        )
        .await;
    let Some(CommandResult::ProjectAdded {
        project_id: garden, ..
    }) = added.result
    else {
        panic!("{added:?}");
    };
    f.hub
        .write(|tx| {
            // The fixture's agent can hold a plan to planning.
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
            let mut chat: Task = tx.read().require("task", chat_id)?;
            chat.conversation = true;
            tx.put("task", chat_id, &chat)?;
            let mut run: Run = tx.read().require("run", run_id)?;
            run.outcome = Some(RunOutcome {
                summary: None,
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
            });
            tx.put("run", run_id, &run)
        })
        .unwrap();
    (chat_id, run_id, garden)
}

/// A card can't start from a chat that is archived, by its Start or from
/// Review, and says so plainly, as turning the chat into a task does.
#[tokio::test]
async fn an_archived_chats_card_does_not_start() {
    let f = Fixture::new().await;
    let project = f.project().await;
    let owner = Caller::local_owner();
    let w = suggestion("Change", TaskMode::Work, None, None);
    let (chat_id, run_id, _) = chat_suggesting(&f, project, vec![w.clone()]).await;
    f.hub
        .write(|tx| {
            let mut chat: Task = tx.read().require("task", chat_id)?;
            chat.state = TaskState::Archived;
            tx.put("task", chat_id, &chat)
        })
        .unwrap();
    for reviewed in [false, true] {
        let origin = TaskOrigin {
            chat: chat_id,
            run: run_id,
            index: 0,
            auto: false,
            reviewed,
        };
        let r = f
            .hub
            .submit(&owner, CommandEnvelope::new(start(project, origin, &w)))
            .await;
        let error = r.error.expect("an archived chat's card must not start");
        assert_eq!(error.code, ErrorCode::InvalidRequest, "reviewed {reviewed}");
        assert!(
            error.message.starts_with("This chat is archived."),
            "{}",
            error.message
        );
    }
    f.close().await;
}

/// A card opened in Review and started from there is still that card's
/// task: what the person changed in Review starts, in another project too,
/// it is linked to the chat, and it starts once, either way round.
#[tokio::test]
async fn a_card_started_from_review_is_still_that_cards_task() {
    let f = Fixture::new().await;
    let project = f.project().await;
    let owner = Caller::local_owner();
    let here = suggestion("Change", TaskMode::Work, None, None);
    let mut elsewhere = suggestion("Elsewhere", TaskMode::Work, None, None);
    elsewhere.needs = Some(protocol::work_cards::WorkNeeds {
        changes: Some("Garden".into()),
        ..Default::default()
    });
    let (chat_id, run_id, garden) =
        chat_suggesting(&f, project, vec![here.clone(), elsewhere.clone()]).await;
    let origin = |index, reviewed| TaskOrigin {
        chat: chat_id,
        run: run_id,
        index,
        auto: false,
        reviewed,
    };
    // Edited in Review: other words, a plan first.
    let mut edited = start(project, origin(0, true), &here);
    if let Command::CreateTask {
        prompt,
        title,
        mode,
        ..
    } = &mut edited
    {
        *prompt = "The person's own words".into();
        *title = Some("Their title".into());
        *mode = Some(TaskMode::Plan);
    }
    // The card's own Start would refuse it as it differs.
    let mut as_card = edited.clone();
    if let Command::CreateTask { origin: o, .. } = &mut as_card {
        *o = Some(origin(0, false));
    }
    let refused = f.hub.submit(&owner, CommandEnvelope::new(as_card)).await;
    assert_eq!(
        refused.error.map(|e| e.message),
        Some(crate::work_card_binding::DIFFERS.to_owned())
    );
    let r = f.hub.submit(&owner, CommandEnvelope::new(edited)).await;
    let Some(CommandResult::TaskCreated { task_id, .. }) = r.result else {
        panic!("{r:?}");
    };
    let task: Task = f.hub.read(|r| r.require("task", task_id)).unwrap();
    assert_eq!(task.origin, Some(origin(0, true)));
    assert_eq!(task.title, "Their title");
    // Once only: the card's Start now finds it started, and Review again too.
    for reviewed in [false, true] {
        let again = f
            .hub
            .submit(
                &owner,
                CommandEnvelope::new(start(project, origin(0, reviewed), &here)),
            )
            .await;
        let error = again.error.expect("a card starts once");
        assert_eq!(error.message, "This was already started.");
        assert_eq!(error.detail.unwrap()["task_id"], serde_json::json!(task_id));
    }
    // Another project only from Review.
    let mut there = start(garden, origin(1, false), &elsewhere);
    let refused = f
        .hub
        .submit(&owner, CommandEnvelope::new(there.clone()))
        .await;
    assert!(refused.error.is_some(), "{refused:?}");
    if let Command::CreateTask { origin: o, .. } = &mut there {
        *o = Some(origin(1, true));
    }
    let r = f.hub.submit(&owner, CommandEnvelope::new(there)).await;
    let Some(CommandResult::TaskCreated { task_id, .. }) = r.result else {
        panic!("{r:?}");
    };
    let task: Task = f.hub.read(|r| r.require("task", task_id)).unwrap();
    assert_eq!(
        (task.project_id, task.origin),
        (garden, Some(origin(1, true)))
    );
    f.close().await;
}

#[tokio::test]
async fn reviewed_card_starts_are_advertised() {
    let f = Fixture::new().await;
    let host: HostInfo = f.hub.read(|r| r.require("meta", "host")).unwrap();
    assert!(
        host.capabilities
            .features
            .iter()
            .any(|f| f == protocol::work_cards::REVIEWED)
    );
    f.close().await;
}

/// Started from Review, a card for fixing every issue still starts no task,
/// and a set's later task keeps its order: in the same project it waits in
/// Next up for the one before; moved to another project, whose Next up
/// can't hold it after that one, it waits until the one before has finished.
#[tokio::test]
async fn review_starts_keep_run_all_and_a_sets_order() {
    let f = Fixture::new().await;
    let project = f.project().await;
    let owner = Caller::local_owner();
    let first = suggestion("First", TaskMode::Work, None, None);
    let mut second = suggestion("Second", TaskMode::Work, None, None);
    second.after = Some(0);
    let mut every = suggestion("Every issue", TaskMode::Work, None, None);
    every.all_issues = true;
    let (chat_id, run_id, garden) = chat_suggesting(
        &f,
        project,
        vec![first.clone(), second.clone(), every.clone()],
    )
    .await;
    let origin = |index| TaskOrigin {
        chat: chat_id,
        run: run_id,
        index,
        auto: false,
        reviewed: true,
    };
    let submit = |c: Command| f.hub.submit(&owner, CommandEnvelope::new(c));
    let refused = submit(start(project, origin(2), &every)).await;
    assert_eq!(
        refused.error.map(|e| e.message),
        Some(crate::work_card_binding::DIFFERS.to_owned())
    );
    let r = submit(start(project, origin(0), &first)).await;
    let Some(CommandResult::TaskCreated {
        task_id: before, ..
    }) = r.result
    else {
        panic!("{r:?}");
    };
    let refused = submit(start(garden, origin(1), &second)).await;
    let message = refused.error.expect("waits for the one before").message;
    assert!(message.contains("still going outside Next up"), "{message}");
    let r = submit(start(project, origin(1), &second)).await;
    let Some(CommandResult::TaskCreated { task_id: after, .. }) = r.result else {
        panic!("{r:?}");
    };
    let queue: QueueView = f.hub.read(|r| r.require("queue", project)).unwrap();
    let item = |id| queue.items.iter().find(|i| i.task_id == id).unwrap();
    assert_eq!(item(after).depends_on, vec![item(before).item_id]);
    f.close().await;
}
