// SPDX-License-Identifier: Apache-2.0
//! The project's checklist: what an agent needs the person to do, kept
//! where they find it. Ticking off what an agent waits on starts it again at
//! once, a question reaches the agent that wrote the item, and its files go
//! where the person keeps files.
use super::watch_tests::{ended, notes, queue, running_project, task, texts, until, working};
use super::*;
use crate::followups::{FOLLOWUP, Followup};
use ports::bridge::{BridgeHandler, PersonCall};
use protocol::checklist::{
    Checklist, ChecklistOwner, ChecklistPriority, FileDelivery, ItemDraft, NoteFrom, StepDraft,
};

/// The fixture's agent answers in Ask mode as well as Work.
fn asks(f: &Fixture) {
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
}

fn item(id: &str, title: &str, waiting: Option<&str>) -> ItemDraft {
    ItemDraft {
        id: id.into(),
        title: title.into(),
        why: None,
        minutes: Some(5),
        priority: Default::default(),
        due: None,
        after: Vec::new(),
        steps: vec![
            StepDraft {
                id: None,
                text: "Open the console's keys page.".into(),
                url: Some("https://console.example.com/keys".into()),
                copy: None,
                file: None,
            },
            StepDraft {
                id: None,
                text: "Upload the settings file.".into(),
                url: None,
                copy: None,
                file: Some("setup/settings.json".into()),
            },
        ],
        ask: None,
        waiting: waiting.map(Into::into),
        reopen: false,
        owner: None,
        done: None,
    }
}

/// The agent's own item `id`, which comes after `after`.
fn own(id: &str, title: &str, after: &[&str]) -> ItemDraft {
    let mut item = item(id, title, None);
    item.owner = Some(ChecklistOwner::Agent);
    item.after = after.iter().map(|a| a.to_string()).collect();
    item.steps.truncate(1);
    item.steps[0].url = None;
    item
}

fn checklist(f: &Fixture, project: ProjectId) -> Checklist {
    f.hub
        .read(|r| r.require(crate::checklist::CHECKLIST, project))
        .unwrap()
}

fn last_prompt(f: &Fixture) -> String {
    let specs = f.harness.specs.lock().unwrap();
    specs.last().unwrap().prompt.text.clone()
}

async fn ok(f: &Fixture, command: Command) -> Option<CommandResult> {
    let r = f
        .hub
        .submit(&Caller::local_owner(), CommandEnvelope::new(command))
        .await;
    assert_eq!(r.status, ReceiptStatus::Accepted, "{r:?}");
    r.result
}

/// A Work task whose agent put two items on the checklist, waiting on the
/// first, and ended its turn.
async fn waiting_on_the_person(f: &Fixture) -> (ProjectId, TaskId, RunId) {
    let owner = Caller::local_owner();
    let project = running_project(f).await;
    std::fs::create_dir_all(f.project_dir().join("setup")).unwrap();
    std::fs::write(
        f.project_dir().join("setup/settings.json"),
        b"{\"region\":\"eu\"}",
    )
    .unwrap();
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
            PersonCall::ChecklistAdd {
                title: Some("Launch".into()),
                summary: None,
                theme: None,
                section: Some("Accounts".into()),
                items: vec![
                    item("api", "Grant API access", Some("deploy the service")),
                    item("guide", "Read the guide", None),
                ],
            },
        )
        .await
        .unwrap();
    assert!(
        said.starts_with("Made the checklist with 2 items on the project's checklist.")
            && said.contains("end your turn now"),
        "{said}"
    );
    ended(f, run_id, ports::RunExit::Completed).await;
    (project, task_id, run_id)
}

#[tokio::test]
async fn ticking_off_what_an_agent_waits_on_starts_it_again_at_once() {
    let f = Fixture::new().await;
    let (project, task_id, run_id) = waiting_on_the_person(&f).await;

    // The file is kept in the checklist, so the project can change.
    let list = checklist(&f, project);
    let api = list.item("api").unwrap();
    let file = api.steps[1].file.clone().expect("the file is kept");
    assert_eq!((file.name.as_str(), file.size), ("settings.json", 15));
    assert_eq!(list.sections[0].title, "Accounts");

    // The project is free, and the person is told what the agent waits on.
    assert!(queue(&f, project).writer.is_none());
    let wake = task(&f, task_id).wake.expect("waiting");
    assert!(wake.answer);
    assert_eq!(
        wake.reason,
        "you to finish “Grant API access” on the checklist"
    );
    let raised = f
        .hub
        .read(|r| r.list::<AttentionItem>("attention"))
        .unwrap()
        .into_iter()
        .find(|a| a.run_id == Some(run_id) && a.kind == AttentionKind::Completed)
        .expect("the person is told");
    assert_eq!(
        raised.body.as_deref(),
        Some(
            "Waiting for you to finish “Grant API access” on the checklist. The agent carries on as soon as you tick it off; the project is free meanwhile."
        )
    );

    // A step is not the item, and what nothing waits on wakes nothing.
    let tick = |item: &str, step: Option<&str>| Command::ChecklistTick {
        project_id: project,
        item_id: item.into(),
        step_id: step.map(Into::into),
        done: true,
    };
    ok(&f, tick("api", Some("1"))).await;
    ok(&f, tick("guide", None)).await;
    assert_eq!(f.hub.pass_on_followups().await.unwrap(), 0);
    assert_eq!(task(&f, task_id).current_run, Some(run_id));

    // The last step finishes the item, and the agent carries on.
    ok(&f, tick("api", Some("2"))).await;
    assert!(checklist(&f, project).item("api").unwrap().is_done());
    f.hub.pass_on_followups().await.unwrap();
    until("the agent was started again", || {
        task(&f, task_id).current_run != Some(run_id)
    })
    .await;
    run_started(&f, task_id).await;
    assert!(
        last_prompt(&f).starts_with(
            "The person finished “Grant API access” (item `api`) on the project's checklist. You were waiting on it to deploy the service.\nCarry on with that now."
        ),
        "{}",
        last_prompt(&f)
    );
    assert!(task(&f, task_id).wake.is_none());
    assert!(
        f.hub
            .read(|r| r.entries::<Followup>(FOLLOWUP))
            .unwrap()
            .is_empty()
    );
    f.close().await;
}

#[tokio::test]
async fn a_tick_reaches_an_agent_still_in_its_turn() {
    let f = Fixture::new().await;
    let owner = Caller::local_owner();
    let project = running_project(&f).await;
    std::fs::create_dir_all(f.project_dir().join("setup")).unwrap();
    std::fs::write(f.project_dir().join("setup/settings.json"), b"{}").unwrap();
    let (task_id, _) = f
        .task(project, TaskPlacement::StartNow, &owner, false)
        .await;
    let run_id = run_started(&f, task_id).await;
    working(&f, run_id).await;
    let mut ask = item("pick", "Choose the region", Some("set the region"));
    ask.steps.clear();
    ask.ask = Some(protocol::checklist::AskDraft {
        question: "Which region?".into(),
        options: Vec::new(),
    });
    f.hub
        .person_request(
            run_id,
            &task(&f, task_id),
            PersonCall::ChecklistAdd {
                title: None,
                summary: None,
                theme: None,
                section: None,
                items: vec![ask],
            },
        )
        .await
        .unwrap();
    // A question isn't ticked off before it is answered; answering it is.
    let early = f
        .hub
        .submit(
            &owner,
            CommandEnvelope::new(Command::ChecklistTick {
                project_id: project,
                item_id: "pick".into(),
                step_id: None,
                done: true,
            }),
        )
        .await;
    assert_eq!(early.status, ReceiptStatus::Rejected, "{early:?}");
    ok(
        &f,
        Command::ChecklistAnswer {
            project_id: project,
            item_id: "pick".into(),
            answer: "Frankfurt".into(),
        },
    )
    .await;
    f.hub.pass_on_followups().await.unwrap();
    let told = texts(&f);
    assert!(
        told.iter()
            .any(|t| t.starts_with("The person finished “Choose the region”")
                && t.contains("They answered “Which region?”:\n\nFrankfurt\n")),
        "{told:?}"
    );
    assert_eq!(task(&f, task_id).current_run, Some(run_id));
    assert_eq!(checklist(&f, project).title, "Test checklist");
    f.close().await;
}

#[tokio::test]
async fn a_question_reaches_the_agent_and_its_answer_shows_under_the_item() {
    let f = Fixture::new().await;
    let (project, task_id, run_id) = waiting_on_the_person(&f).await;
    ok(
        &f,
        Command::ChecklistAsk {
            project_id: project,
            item_id: Some("api".into()),
            text: "Which account should I use?".into(),
        },
    )
    .await;
    f.hub.pass_on_followups().await.unwrap();
    until("the agent was asked", || {
        task(&f, task_id).current_run != Some(run_id)
    })
    .await;
    let next = run_started(&f, task_id).await;
    let prompt = last_prompt(&f);
    assert!(
        prompt.starts_with("The person asks about “Grant API access” (item `api`)")
            && prompt.contains("Which account should I use?")
            && prompt.contains("checklist_reply, item \"api\""),
        "{prompt}"
    );
    working(&f, next).await;
    let said = f
        .hub
        .person_request(
            next,
            &task(&f, task_id),
            PersonCall::ChecklistReply {
                item: Some("api".into()),
                text: "The company one, not your own.".into(),
            },
        )
        .await
        .unwrap();
    assert!(said.contains("under “Grant API access”"), "{said}");
    let notes = checklist(&f, project).item("api").unwrap().notes.clone();
    assert_eq!(
        notes
            .iter()
            .map(|n| (n.from, n.text.as_str()))
            .collect::<Vec<_>>(),
        vec![
            (NoteFrom::Person, "Which account should I use?"),
            (NoteFrom::Agent, "The company one, not your own."),
        ]
    );
    // What the agent reads says what is still unanswered, and is not now.
    let read = f
        .hub
        .person_request(next, &task(&f, task_id), PersonCall::ChecklistRead)
        .await
        .unwrap();
    assert!(
        read.contains("  an agent answered: The company one, not your own.\n")
            && !read.contains("(unanswered"),
        "{read}"
    );
    // How to write it comes with it, without how to from outside Ophio.
    assert!(read.contains("\n# How to use the checklist\n\n"), "{read}");
    assert!(read.contains("Give waiting on each item"), "{read}");
    assert!(!read.contains("ophio checklist put"), "{read}");
    f.close().await;
}

#[tokio::test]
async fn a_question_for_an_agent_put_away_goes_to_another() {
    let f = Fixture::new().await;
    asks(&f);
    let (project, task_id, _) = waiting_on_the_person(&f).await;
    let revision = task(&f, task_id).revision;
    let archive = f
        .hub
        .submit(
            &Caller::local_owner(),
            edit(Command::ArchiveTask { task_id }, revision),
        )
        .await;
    assert_eq!(archive.status, ReceiptStatus::Accepted, "{archive:?}");
    // Archiving lets the checklist go: nothing waits on it now.
    assert!(
        checklist(&f, project)
            .item("api")
            .unwrap()
            .waiting
            .is_none()
    );
    ok(
        &f,
        Command::ChecklistAsk {
            project_id: project,
            item_id: None,
            text: "Is the guide still current?".into(),
        },
    )
    .await;
    f.hub.pass_on_followups().await.unwrap();
    let helper = until_task(&f, "Question about the checklist").await;
    assert_eq!(helper.mode, TaskMode::Ask);
    assert_eq!(helper.project_id, project);
    let prompt = last_prompt(&f);
    assert!(
        prompt.starts_with("The agent that wrote this project's checklist has finished")
            && prompt.contains("Is the guide still current?"),
        "{prompt}"
    );
    f.close().await;
}

async fn until_task(f: &Fixture, title: &str) -> Task {
    let find =
        || (f.hub.read(|r| r.list::<Task>("task")).unwrap().into_iter()).find(|t| t.title == title);
    until("the task was made", || {
        find().is_some_and(|t| t.current_run.is_some())
    })
    .await;
    let task = find().unwrap();
    run_started(f, task.task_id).await;
    task
}

#[tokio::test]
async fn stop_lets_the_checklist_go_and_ticking_starts_nothing() {
    let f = Fixture::new().await;
    let (project, task_id, run_id) = waiting_on_the_person(&f).await;
    ok(
        &f,
        Command::StopTask {
            task_id,
            force: false,
        },
    )
    .await;
    assert!(task(&f, task_id).wake.is_none());
    assert!(notes(&f, task_id).await.contains(
        &"Stopped waiting. The checklist keeps its items, and ticking them off won't start the agent again."
            .to_string()
    ));
    let list = checklist(&f, project);
    assert_eq!(list.sections[0].items.len(), 2);
    assert!(list.item("api").unwrap().waiting.is_none());
    ok(
        &f,
        Command::ChecklistTick {
            project_id: project,
            item_id: "api".into(),
            step_id: None,
            done: true,
        },
    )
    .await;
    assert_eq!(f.hub.pass_on_followups().await.unwrap(), 0);
    assert_eq!(task(&f, task_id).current_run, Some(run_id));

    // Put away, it takes what it kept with it.
    ok(
        &f,
        Command::RemoveChecklist {
            project_id: project,
        },
    )
    .await;
    assert!(f.hub.checklist_view(project).unwrap().is_none());
    assert!(f.hub.read(crate::checklist::blobs).unwrap().is_empty());
    f.close().await;
}

#[tokio::test]
async fn a_file_goes_into_the_folder_the_person_chose() {
    let home = std::env::temp_dir().join(uuid::Uuid::now_v7().to_string());
    let f = Fixture::with(|b| b.home_dir(&home)).await;
    let (project, _, _) = waiting_on_the_person(&f).await;
    let sha256 = checklist(&f, project).item("api").unwrap().steps[1]
        .file
        .clone()
        .unwrap()
        .sha256;
    let deliver = || Command::ChecklistDeliver {
        project_id: project,
        sha256: sha256.clone(),
    };
    // Sent to the phone, it comes down as a download instead.
    let to_phone = f
        .hub
        .submit(&Caller::local_owner(), CommandEnvelope::new(deliver()))
        .await;
    assert_eq!(to_phone.status, ReceiptStatus::Rejected, "{to_phone:?}");

    // Given from the home folder, as the person types it.
    let folder = home.join("Google Drive");
    std::fs::create_dir_all(&folder).unwrap();
    let set = f
        .hub
        .submit(
            &Caller::local_owner(),
            edit(
                Command::SetFileDelivery {
                    delivery: FileDelivery::Folder {
                        path: "~/Google Drive/".into(),
                    },
                },
                0,
            ),
        )
        .await;
    assert_eq!(set.status, ReceiptStatus::Accepted, "{set:?}");
    let kept_as = f
        .hub
        .read(crate::checklist::file_delivery)
        .unwrap()
        .delivery;
    let path = folder.to_string_lossy().into_owned();
    assert_eq!(kept_as, FileDelivery::Folder { path });
    let Some(CommandResult::Delivered { to }) = ok(&f, deliver()).await else {
        panic!("delivered")
    };
    let kept = folder.join("Test").join("settings.json");
    assert_eq!(to, kept.to_string_lossy());
    assert_eq!(std::fs::read(&kept).unwrap(), b"{\"region\":\"eu\"}");
    // Sent again, the same file is kept rather than copied beside it.
    ok(&f, deliver()).await;
    assert_eq!(std::fs::read_dir(folder.join("Test")).unwrap().count(), 1);
    std::fs::remove_dir_all(home).unwrap();
    f.close().await;
}

#[tokio::test]
async fn the_command_line_writes_the_checklist_for_agents_outside_ophio() {
    let f = Fixture::new().await;
    let (project, task_id, _) = waiting_on_the_person(&f).await;
    let additions = |items: Vec<ItemDraft>| crate::ChecklistAdditions {
        items,
        ..Default::default()
    };

    // It reads as an agent in Ophio reads it, without the guide.
    let shown = f
        .hub
        .owner_checklist(project, None, Vec::new())
        .await
        .unwrap();
    assert!(shown.starts_with("# Launch\n"), "{shown}");
    assert!(
        shown.contains("waiting (an agent): deploy the service"),
        "{shown}"
    );
    assert!(!shown.contains("How to use the checklist"), "{shown}");

    // Nothing in Ophio would carry on, so it waits on nothing.
    let refused = f
        .hub
        .owner_checklist(
            project,
            Some(additions(vec![item(
                "dns",
                "Add the DNS record",
                Some("go live"),
            )])),
            Vec::new(),
        )
        .await
        .unwrap_err();
    assert!(
        refused.message.contains("Leave waiting out of “dns”"),
        "{refused:?}"
    );

    // Its files come from the project's folder, and nowhere else.
    let mut outside = item("dns", "Add the DNS record", None);
    outside.steps[1].file = Some("/etc/hostname".into());
    assert!(
        f.hub
            .owner_checklist(project, Some(additions(vec![outside])), Vec::new())
            .await
            .is_err()
    );
    let said = f
        .hub
        .owner_checklist(
            project,
            Some(additions(vec![item("dns", "Add the DNS record", None)])),
            Vec::new(),
        )
        .await
        .unwrap();
    assert_eq!(said, "Put “Add the DNS record”.\n");
    let list = checklist(&f, project);
    let dns = list.item("dns").unwrap();
    assert_eq!(dns.task_id, None);
    assert_eq!(dns.steps[1].file.as_ref().unwrap().name, "settings.json");
    assert_eq!(
        list.task_id,
        Some(task_id),
        "the agent still answers questions"
    );

    // A blocker goes first; the rest keep the order they were written in.
    let mut blocker = item("dns", "Add the DNS record", None);
    blocker.priority = ChecklistPriority::Blocker;
    f.hub
        .owner_checklist(project, Some(additions(vec![blocker])), Vec::new())
        .await
        .unwrap();
    let order: Vec<String> = (checklist(&f, project).items())
        .map(|i| i.item_id.clone())
        .collect();
    assert_eq!(order, ["dns", "api", "guide"]);
    let shown = (f.hub.owner_checklist(project, None, Vec::new()).await).unwrap();
    assert!(
        shown.contains("3 open, 1 of them blockers") && shown.contains("  priority: blocker\n"),
        "{shown}"
    );

    // Rewriting what an agent waits on lets its wait go.
    assert!(task(&f, task_id).wake.is_some());
    f.hub
        .owner_checklist(
            project,
            Some(additions(vec![item("api", "Grant API access", None)])),
            Vec::new(),
        )
        .await
        .unwrap();
    assert!(task(&f, task_id).wake.is_none());

    let said = f
        .hub
        .owner_checklist(project, None, vec!["guide".into(), "nope".into()])
        .await
        .unwrap();
    assert_eq!(
        said,
        "Took 1 item off the checklist. Not on it: nope. `ophio checklist show` lists the ids.\n"
    );
    f.close().await;
}

/// A Work task under way in a project with the settings file its items give
/// the person.
async fn writing(f: &Fixture) -> (ProjectId, TaskId, RunId) {
    let project = running_project(f).await;
    std::fs::create_dir_all(f.project_dir().join("setup")).unwrap();
    std::fs::write(f.project_dir().join("setup/settings.json"), b"{}").unwrap();
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

async fn add(
    f: &Fixture,
    run_id: RunId,
    task_id: TaskId,
    items: Vec<ItemDraft>,
) -> Result<String, ProtocolError> {
    let call = PersonCall::ChecklistAdd {
        title: None,
        summary: None,
        theme: None,
        section: None,
        items,
    };
    f.hub.person_request(run_id, &task(f, task_id), call).await
}

/// What is still to pass on: by key, to whom, and what it says.
fn pending(f: &Fixture) -> Vec<(String, TaskId, String)> {
    let mut all: Vec<_> = (f.hub.read(|r| r.entries::<Followup>(FOLLOWUP)).unwrap())
        .into_iter()
        .map(|(key, f)| (key, f.task_id, f.text))
        .collect();
    all.sort();
    all
}

fn tick(project: ProjectId, item: &str, step: Option<&str>, done: bool) -> Command {
    Command::ChecklistTick {
        project_id: project,
        item_id: item.into(),
        step_id: step.map(Into::into),
        done,
    }
}

#[tokio::test]
async fn an_agent_keeps_its_own_items_and_the_person_sends_feedback_on_them() {
    let f = Fixture::new().await;
    let (project, task_id, run_id) = writing(&f).await;

    // Work nothing the person does holds up isn't for the checklist.
    let refused = add(&f, run_id, task_id, vec![own("deploy", "Deploy it", &[])])
        .await
        .unwrap_err();
    assert_eq!(
        refused.message,
        "Put “deploy” on the checklist only if it waits on something the person has to do: name their open item in after. Work that nothing blocks, just do."
    );
    let said = add(
        &f,
        run_id,
        task_id,
        vec![
            item("api", "Grant API access", None),
            item("guide", "Read the guide", None),
            own("deploy", "Deploy the service", &["api"]),
        ],
    )
    .await
    .unwrap();
    assert!(
        said.ends_with(" Your own items wait on what they come after: Ophio starts your next turn when the person has finished it. Once you've done one, mark it done with done: true."),
        "{said}"
    );
    // The person's counts are theirs alone.
    let summary = f.hub.checklists_view().unwrap().remove(0);
    assert_eq!(
        (summary.open, summary.done, summary.for_agent),
        (2, 0, 1),
        "{summary:?}"
    );
    assert_eq!(summary.next.as_deref(), Some("Grant API access"));

    // Only the person ticks theirs off; an agent's own item has no question
    // or wait, and a new one waits on an item still open.
    ok(&f, tick(project, "guide", None, true)).await;
    let mut done = item("api", "Grant API access", None);
    done.done = Some(true);
    let mut waits = own("later", "Later", &["api"]);
    waits.waiting = Some("go on".into());
    let mut asks = own("later", "Later", &["api"]);
    asks.ask = Some(protocol::checklist::AskDraft {
        question: "Which?".into(),
        options: Vec::new(),
    });
    for (draft, why) in [
        (
            done,
            "Leave done out of “api”: only the person ticks off their own items.",
        ),
        (
            waits,
            "Leave waiting out of “later”: it is your own work. Name what it waits on in after; Ophio starts you when that is done.",
        ),
        (
            asks,
            "Leave ask out of “later”: it is your own work. Ask the person on an item of theirs.",
        ),
        (
            own("later", "Later", &["guide"]),
            "Put “later” on the checklist only if it waits on something the person has to do: name their open item in after. Work that nothing blocks, just do.",
        ),
    ] {
        let refused = add(&f, run_id, task_id, vec![draft]).await.unwrap_err();
        assert_eq!(refused.message, why);
    }

    // The person can't tick the agent's item or its steps.
    for step in [None, Some("1")] {
        let r = f
            .hub
            .submit(
                &Caller::local_owner(),
                CommandEnvelope::new(tick(project, "deploy", step, true)),
            )
            .await;
        assert_eq!(r.status, ReceiptStatus::Rejected, "{r:?}");
        assert_eq!(
            r.error.unwrap().message,
            "“Deploy the service” is the agent's to do. Send it feedback instead."
        );
    }

    // What the agent reads says whose it is.
    let read = (f.hub)
        .person_request(run_id, &task(&f, task_id), PersonCall::ChecklistRead)
        .await
        .unwrap();
    assert!(
        read.contains(
            "- [ ] Deploy the service — id \"deploy\"\n  for: an agent (yours: you keep it)\n"
        ) && read.contains("\nAgents' own items: 1 open.\n")
            && read.contains("Not for work you are about to do"),
        "{read}"
    );
    let shown = f.hub.owner_checklist(project, None, Vec::new()).await;
    assert!(shown.unwrap().contains("id \"deploy\"\n  for: an agent\n"));

    // The person's feedback goes to the agent that keeps it.
    ok(
        &f,
        Command::ChecklistAsk {
            project_id: project,
            item_id: Some("deploy".into()),
            text: "Wait for the new domain.".into(),
        },
    )
    .await;
    let read = (f.hub)
        .person_request(run_id, &task(&f, task_id), PersonCall::ChecklistRead)
        .await
        .unwrap();
    assert!(
        read.contains("  the person's feedback: Wait for the new domain.\n"),
        "{read}"
    );
    let feedback = pending(&f);
    assert_eq!(feedback.len(), 1, "{feedback:?}");
    assert!(
        feedback[0].0.starts_with(&format!("checklist-feedback:{project}:"))
            && feedback[0].1 == task_id
            && feedback[0].2.starts_with(
                "The person sent feedback on your own item “Deploy the service” (`deploy`) on the project's checklist:\n\nWait for the new domain.\n\nAnswer them with checklist_reply, item \"deploy\""
            ),
        "{feedback:?}"
    );

    // It marks the item done itself, and opens it again.
    let mut finished = own("deploy", "Deploy the service", &[]);
    finished.done = Some(true);
    add(&f, run_id, task_id, vec![finished.clone()])
        .await
        .unwrap();
    let deploy = checklist(&f, project).item("deploy").unwrap().clone();
    assert!(deploy.is_done() && deploy.steps[0].done_at.is_some());
    assert_eq!(deploy.notes.len(), 1);
    assert_eq!(checklist(&f, project).summary().for_agent, 0);
    finished.done = Some(false);
    add(&f, run_id, task_id, vec![finished]).await.unwrap();
    let deploy = checklist(&f, project).item("deploy").unwrap().clone();
    assert!(!deploy.is_done() && deploy.steps[0].done_at.is_none());

    // A turn under way gets it as its next message.
    f.hub.pass_on_followups().await.unwrap();
    until("the agent was told", || {
        texts(&f)
            .iter()
            .any(|t| t.starts_with("The person sent feedback on your own item"))
    })
    .await;
    assert!(pending(&f).is_empty());
    f.close().await;
}

#[tokio::test]
async fn finishing_what_an_agents_item_comes_after_starts_the_agent_on_it() {
    let f = Fixture::new().await;
    let (project, task_id, run_id) = writing(&f).await;
    add(
        &f,
        run_id,
        task_id,
        vec![
            item("api", "Grant API access", None),
            item("dns", "Add the DNS record", None),
            own("deploy", "Deploy the service", &["api", "dns"]),
            own("notes", "Write the release notes", &["api"]),
        ],
    )
    .await
    .unwrap();
    ended(&f, run_id, ports::RunExit::Completed).await;
    let ready = |id: &str| format!("checklist-ready:{project}:{id}");
    let keys = |f: &Fixture| pending(f).into_iter().map(|p| p.0).collect::<Vec<_>>();

    // Only an item with nothing else open is ready.
    ok(&f, tick(project, "api", None, true)).await;
    assert_eq!(
        pending(&f),
        [(
            ready("notes"),
            task_id,
            "The person finished “Grant API access” (item `api`) on the project's checklist, which your item “Write the release notes” (`notes`) came after. Nothing it needs is open now.\nDo it now, then mark it done with checklist_add (`done: true`).".into()
        )]
    );
    // Unticked before it went, it isn't ready after all.
    ok(&f, tick(project, "api", None, false)).await;
    assert!(pending(&f).is_empty());
    ok(&f, tick(project, "api", None, true)).await;
    ok(&f, tick(project, "dns", Some("1"), true)).await;
    assert_eq!(keys(&f), [ready("notes")]);
    // The last step finishes the item.
    ok(&f, tick(project, "dns", Some("2"), true)).await;
    assert_eq!(keys(&f), [ready("deploy"), ready("notes")]);

    f.hub.pass_on_followups().await.unwrap();
    until("the agent was started again", || {
        task(&f, task_id).current_run != Some(run_id)
    })
    .await;
    let next = run_started(&f, task_id).await;
    // Each ticked on its own: the one that started the turn, and the other
    // as a message in it, not a turn of its own.
    let first = last_prompt(&f);
    assert!(first.starts_with("The person finished “"), "{first}");
    assert_eq!(pending(&f).len(), 1);
    working(&f, next).await;
    f.hub.pass_on_followups().await.unwrap();
    until("the other reached the turn", || texts(&f).len() == 1).await;
    assert!(pending(&f).is_empty());
    assert_eq!(task(&f, task_id).current_run, Some(next));
    f.close().await;
}

#[tokio::test]
async fn each_thing_one_tick_tells_an_agent_goes_on_its_own() {
    let f = Fixture::new().await;
    let (project, task_id, run_id) = writing(&f).await;
    let mut region = item("region", "Choose the region", None);
    region.steps.clear();
    region.ask = Some(protocol::checklist::AskDraft {
        question: "Which region?".into(),
        options: Vec::new(),
    });
    add(
        &f,
        run_id,
        task_id,
        vec![
            item("key", "Make the key", Some("deploy with the key")),
            own("ship", "Ship it", &["key"]),
            region,
            own("configure", "Configure the region", &["region"]),
        ],
    )
    .await
    .unwrap();
    ended(&f, run_id, ports::RunExit::Completed).await;
    // Nothing is passed on while this looks at what is still to go.
    let holding = f.hub.inner.followups.lock().await;

    // What it waits on and its own item go apart, so unticking takes back
    // each of them alone.
    ok(&f, tick(project, "key", None, true)).await;
    let done = format!("checklist-done:{project}:key");
    let ship = format!("checklist-ready:{project}:ship");
    let keys = |f: &Fixture| pending(f).into_iter().map(|p| p.0).collect::<Vec<_>>();
    assert_eq!(keys(&f), [done.clone(), ship.clone()]);
    let told = pending(&f);
    assert!(
        told[0]
            .2
            .contains("You were waiting on it to deploy with the key.")
            && told[1].2
                == "The person finished “Make the key” (item `key`) on the project's checklist, which your item “Ship it” (`ship`) came after. Nothing it needs is open now.\nDo it now, then mark it done with checklist_add (`done: true`).",
        "{told:?}"
    );
    ok(&f, tick(project, "key", None, false)).await;
    assert!(pending(&f).is_empty());

    // An answer that finishes an item goes on its own too, and stays when
    // the item is unticked and ticked again: only the ready notice goes
    // and comes back.
    ok(
        &f,
        Command::ChecklistAnswer {
            project_id: project,
            item_id: "region".into(),
            answer: "Frankfurt".into(),
        },
    )
    .await;
    let answer = format!("checklist-answer:{project}:region");
    let configure = format!("checklist-ready:{project}:configure");
    assert_eq!(keys(&f), [answer.clone(), configure.clone()]);
    let told = pending(&f);
    assert!(
        told[0]
            .2
            .starts_with("The person answered the checklist item “Choose the region” (`region`)")
            && told[0].2.contains("Frankfurt")
            && told[1].2.starts_with("The person finished “Choose the region” (item `region`) on the project's checklist, which your item “Configure the region” (`configure`) came after."),
        "{told:?}"
    );
    ok(&f, tick(project, "region", None, false)).await;
    assert_eq!(keys(&f), std::slice::from_ref(&answer));
    ok(&f, tick(project, "region", None, true)).await;
    assert_eq!(keys(&f), [answer, configure]);

    // Put away, nothing is left to pass on.
    ok(&f, tick(project, "key", None, true)).await;
    ok(
        &f,
        Command::RemoveChecklist {
            project_id: project,
        },
    )
    .await;
    assert!(pending(&f).is_empty());
    drop(holding);
    f.close().await;
}

#[tokio::test]
async fn stopping_an_agent_lets_its_own_items_go() {
    let f = Fixture::new().await;
    let (project, task_id, run_id) = writing(&f).await;
    add(
        &f,
        run_id,
        task_id,
        vec![
            item("api", "Grant API access", Some("deploy the service")),
            item("guide", "Read the guide", None),
            own("announce", "Announce the launch", &["guide"]),
        ],
    )
    .await
    .unwrap();
    ended(&f, run_id, ports::RunExit::Completed).await;
    ok(&f, tick(project, "guide", None, true)).await;
    assert_eq!(pending(&f).len(), 1);
    ok(
        &f,
        Command::StopTask {
            task_id,
            force: false,
        },
    )
    .await;
    // Nothing is passed on to it, and nothing it kept is its now.
    assert!(pending(&f).is_empty());
    assert_eq!(
        checklist(&f, project).item("announce").unwrap().task_id,
        None
    );
    ok(&f, tick(project, "guide", None, false)).await;
    ok(&f, tick(project, "guide", None, true)).await;
    ok(
        &f,
        Command::ChecklistAsk {
            project_id: project,
            item_id: Some("announce".into()),
            text: "Say it on the blog too.".into(),
        },
    )
    .await;
    assert!(pending(&f).is_empty());
    let announce = checklist(&f, project).item("announce").unwrap().clone();
    assert_eq!(announce.notes[0].text, "Say it on the blog too.");
    // Nobody is told of it, so it isn't counted as waiting for an answer.
    assert_eq!(checklist(&f, project).summary().unanswered, 0);
    assert_eq!(f.hub.pass_on_followups().await.unwrap(), 0);
    assert_eq!(task(&f, task_id).current_run, Some(run_id));
    f.close().await;
}

#[tokio::test]
async fn a_question_for_an_agent_put_away_is_answered_under_its_item() {
    let f = Fixture::new().await;
    asks(&f);
    let (project, task_id, _) = waiting_on_the_person(&f).await;
    let revision = task(&f, task_id).revision;
    let archive = f
        .hub
        .submit(
            &Caller::local_owner(),
            edit(Command::ArchiveTask { task_id }, revision),
        )
        .await;
    assert_eq!(archive.status, ReceiptStatus::Accepted, "{archive:?}");
    ok(
        &f,
        Command::ChecklistAsk {
            project_id: project,
            item_id: Some("api".into()),
            text: "Which account should I use?".into(),
        },
    )
    .await;
    f.hub.pass_on_followups().await.unwrap();
    let helper = until_task(&f, "Question about the checklist").await;
    assert_eq!(helper.mode, TaskMode::Ask);

    // It reads, and is told it can't change anything.
    let prompt = last_prompt(&f);
    assert!(
        prompt.starts_with("The agent that wrote this project's checklist has finished, so Ophio asks you instead. You're read-only here: read the project and the checklist, with checklist_read first, and answer with checklist_reply. You can't change either.\n\nThe person asks about “Grant API access” (item `api`)")
            && prompt.ends_with("they read it under the item. If something on it was unclear, wrong or missing, say so in your answer: you can't change the checklist here.")
            && !prompt.contains("checklist_add"),
        "{prompt}"
    );
    // Its turn has the checklist's read and reply tools, and nothing else.
    let spec = (f.harness.specs.lock().unwrap().last().cloned()).unwrap();
    assert_eq!(spec.mode, TaskMode::Ask);
    let bridge = spec.bridge.expect("the checklist's tools");
    assert!(bridge.checklist_answers && !bridge.browser && !bridge.github && !bridge.computer_use);
    let token = (bridge.env.iter())
        .find(|(key, _)| key == ports::bridge::RUN_TOKEN_ENV)
        .map(|(_, token)| token.clone())
        .unwrap();
    let binding = f.hub.bind(&token).await.expect("bridge binding");
    assert!(binding.checklist_answers);
    let person = |action| ports::bridge::BridgeRequest::Person { action };
    let ports::bridge::BridgeResponse::PersonResult { text } = (f.hub)
        .handle(&binding, person(PersonCall::ChecklistRead))
        .await
    else {
        panic!("the checklist is read")
    };
    assert!(
        text.contains("  the person asks: Which account should I use?\n"),
        "{text}"
    );
    let refused = "This turn only reads the checklist and answers the person on it";
    for request in [
        person(PersonCall::ChecklistAdd {
            title: Some("Mine now".into()),
            summary: None,
            theme: None,
            section: None,
            items: Vec::new(),
        }),
        person(PersonCall::ChecklistRemove {
            items: vec!["api".into()],
        }),
        person(PersonCall::Wake {
            minutes: 5,
            reason: "later".into(),
        }),
        ports::bridge::BridgeRequest::RequestHuman {
            reason: "Help".into(),
        },
    ] {
        assert_eq!(
            f.hub.handle(&binding, request).await,
            ports::bridge::BridgeResponse::Refused {
                reason: refused.into()
            }
        );
    }
    // Its answer lands under the question.
    let ports::bridge::BridgeResponse::PersonResult { text } = (f.hub)
        .handle(
            &binding,
            person(PersonCall::ChecklistReply {
                item: Some("api".into()),
                text: "The company one, not your own.".into(),
            }),
        )
        .await
    else {
        panic!("answered")
    };
    assert!(text.contains("under “Grant API access”"), "{text}");
    let list = checklist(&f, project);
    let notes = &list.item("api").unwrap().notes;
    assert_eq!(
        notes
            .iter()
            .map(|n| (n.from, n.text.as_str()))
            .collect::<Vec<_>>(),
        vec![
            (NoteFrom::Person, "Which account should I use?"),
            (NoteFrom::Agent, "The company one, not your own."),
        ]
    );
    assert_eq!(list.summary().unanswered, 0);
    assert_eq!(list.title, "Launch");

    // Any other task in Ask mode still has none of Ophio's tools.
    let r = f
        .hub
        .submit(
            &Caller::local_owner(),
            CommandEnvelope::new(Command::CreateTask {
                github: None,
                agent_chosen: None,
                mode: Some(TaskMode::Ask),
                project_id: project,
                kind: TaskKind::Coding,
                title: Some("Just asking".into()),
                prompt: "What is on the checklist?".into(),
                bundle: None,
                harness: protocol::capability::HarnessKind::ClaudeCode,
                model: None,
                effort: None,
                launch_policy: None,
                branch: None,
                mission: None,
                target_id: None,
                placement: TaskPlacement::StartNow,
                start: None,
                place: None,
                browser: None,
                conversation: None,
                access: None,
                review: None,
            }),
        )
        .await;
    assert_eq!(r.status, ReceiptStatus::Accepted, "{r:?}");
    until_task(&f, "Just asking").await;
    let spec = (f.harness.specs.lock().unwrap().last().cloned()).unwrap();
    assert!(spec.bridge.is_none());
    f.close().await;
}

/// What is still to pass on to `task_id`, by key.
fn pending_for(f: &Fixture, task_id: TaskId) -> Vec<String> {
    (pending(f).into_iter())
        .filter(|p| p.1 == task_id)
        .map(|p| p.0)
        .collect()
}

#[tokio::test]
async fn an_items_owner_stays_unless_changed_and_its_own_work_keeps_waiting_on_the_person() {
    let f = Fixture::new().await;
    let (project, task_id, run_id) = writing(&f).await;
    add(
        &f,
        run_id,
        task_id,
        vec![
            item("api", "Grant API access", None),
            item("dns", "Add the DNS record", None),
            own("deploy", "Deploy the service", &["api"]),
        ],
    )
    .await
    .unwrap();
    let rewrite = |id: &str, title: &str, after: &[&str]| {
        let mut d = own(id, title, after);
        d.owner = None;
        d
    };
    // Left out, the owner stays as it was.
    add(
        &f,
        run_id,
        task_id,
        vec![rewrite("deploy", "Deploy it", &["api"])],
    )
    .await
    .unwrap();
    assert!(checklist(&f, project).item("deploy").unwrap().for_agent());

    // An edit can't leave it waiting on nothing of the person's.
    let refused = add(
        &f,
        run_id,
        task_id,
        vec![rewrite("deploy", "Deploy it", &[])],
    )
    .await
    .unwrap_err();
    assert_eq!(
        refused.message,
        "Keep the person's item that “deploy” comes after in after, done or not, or mark it done with done: true: it can't wait on nothing."
    );

    // Made the agent's, an item of the person's is held to what a new one is.
    let refused = add(
        &f,
        run_id,
        task_id,
        vec![own("dns", "Add the DNS record", &[])],
    )
    .await
    .unwrap_err();
    assert!(
        refused.message.starts_with(
            "Put “dns” on the checklist only if it waits on something the person has to do"
        ),
        "{refused:?}"
    );
    let said = add(
        &f,
        run_id,
        task_id,
        vec![own("dns", "Add the DNS record", &["api"])],
    )
    .await
    .unwrap();
    assert!(
        said.contains(" “Add the DNS record” is an agent's own work now: the person sees it but doesn't tick it off."),
        "{said}"
    );
    let mut back = item("dns", "Add the DNS record", None);
    back.owner = Some(ChecklistOwner::Person);
    let said = add(&f, run_id, task_id, vec![back]).await.unwrap();
    assert!(
        said.contains(" “Add the DNS record” is the person's to do now: they tick it off."),
        "{said}"
    );
    assert!(!checklist(&f, project).item("dns").unwrap().for_agent());

    // Once what it waits on is done, it can still be edited as it goes.
    ok(&f, tick(project, "api", None, true)).await;
    let said = add(
        &f,
        run_id,
        task_id,
        vec![rewrite("deploy", "Deploy it to staging", &["api"])],
    )
    .await
    .unwrap();
    assert!(!said.contains("is open now"), "{said}");
    assert_eq!(
        checklist(&f, project).item("deploy").unwrap().title,
        "Deploy it to staging"
    );
    f.close().await;
}

#[tokio::test]
async fn each_item_an_agent_keeps_is_told_of_on_its_own_whoever_made_it_ready() {
    let f = Fixture::new().await;
    let (project, first, run_id) = writing(&f).await;
    // "release" comes after the person's "api" and the agent's own "build".
    add(
        &f,
        run_id,
        first,
        vec![
            item("api", "Grant API access", None),
            item("docs", "Check the docs", None),
            own("build", "Build it", &["api"]),
            own("release", "Release it", &["api", "build"]),
            own("notes", "Write the notes", &["api"]),
        ],
    )
    .await
    .unwrap();
    ended(&f, run_id, ports::RunExit::Completed).await;
    // Nothing is passed on while this looks at what is still to go.
    let holding = f.hub.inner.followups.lock().await;
    let ready = |id: &str| format!("checklist-ready:{project}:{id}");
    ok(
        &f,
        Command::ChecklistAsk {
            project_id: project,
            item_id: Some("build".into()),
            text: "Use the release profile.".into(),
        },
    )
    .await;
    let feedback = format!("checklist-feedback:{project}:build:");
    assert!(
        pending_for(&f, first)
            .iter()
            .any(|k| k.starts_with(&feedback))
    );
    ok(&f, tick(project, "api", None, true)).await;
    let keys = pending_for(&f, first);
    assert!(
        keys.contains(&ready("build")) && keys.contains(&ready("notes")),
        "{keys:?}"
    );
    assert!(!keys.contains(&ready("release")));

    // Another task takes "build" over: its notice and the feedback go with
    // it, and it is told in its reply rather than sent a message.
    let (second, _) = f
        .task(
            project,
            TaskPlacement::StartNow,
            &Caller::local_owner(),
            false,
        )
        .await;
    let next = run_started(&f, second).await;
    working(&f, next).await;
    let mut build = own("build", "Build it", &["api"]);
    build.owner = None;
    let said = add(
        &f,
        next,
        second,
        vec![
            build.clone(),
            own("site", "Publish the site", &["docs", "build"]),
        ],
    )
    .await
    .unwrap();
    assert!(
        said.contains(" Nothing your item “Build it” (`build`) comes after is open now: do it now, then mark it done with done: true."),
        "{said}"
    );
    eprintln!(
        "DEBUG {:?} {:?} {:?} {:?}",
        pending(&f),
        texts(&f),
        first,
        second
    );
    let keys = pending_for(&f, first);
    assert!(!keys.contains(&ready("build")), "{keys:?}");
    assert!(!keys.iter().any(|k| k.starts_with(&feedback)), "{keys:?}");
    assert!(
        pending_for(&f, second)
            .iter()
            .any(|k| k.starts_with(&feedback)),
        "the feedback goes to the task that keeps the item now"
    );
    assert!(keys.contains(&ready("notes")));

    // Its own item done, what comes after it is ready: the other task's is
    // sent a message, its own named in its reply.
    ok(&f, tick(project, "docs", None, true)).await;
    build.done = Some(true);
    let said = add(&f, next, second, vec![build]).await.unwrap();
    assert!(
        said.contains(" Nothing your item “Publish the site” (`site`) comes after is open now"),
        "{said}"
    );
    let told = pending(&f);
    let release = told.iter().find(|p| p.0 == ready("release")).expect("told");
    assert_eq!(release.1, first);
    assert_eq!(
        release.2,
        "Nothing your item “Release it” (`release`) on the project's checklist comes after is open now.\nDo it now, then mark it done with checklist_add (`done: true`)."
    );

    // Unticking takes back exactly what needed it; ticking again sends it.
    ok(&f, tick(project, "api", None, false)).await;
    let keys = pending_for(&f, first);
    assert!(!keys.contains(&ready("notes")) && !keys.contains(&ready("release")));
    ok(&f, tick(project, "api", None, true)).await;
    let keys = pending_for(&f, first);
    assert!(keys.contains(&ready("notes")) && keys.contains(&ready("release")));

    // Taken off, one item's notice goes and the others' stay; what was
    // waiting on an item taken off can be done now.
    ok(&f, tick(project, "docs", None, false)).await;
    let said = (f.hub)
        .checklist_remove(next, &task(&f, second), vec!["notes".into(), "docs".into()])
        .unwrap();
    assert!(
        said.contains("Nothing your item “Publish the site” (`site`) comes after is open now"),
        "{said}"
    );
    let keys = pending_for(&f, first);
    assert!(!keys.contains(&ready("notes")) && keys.contains(&ready("release")));
    drop(holding);
    f.close().await;
}

#[tokio::test]
async fn a_stop_that_comes_first_keeps_the_checklist_as_it_was() {
    let f = Fixture::new().await;
    let (project, task_id, run_id) = writing(&f).await;
    add(
        &f,
        run_id,
        task_id,
        vec![item("api", "Grant API access", None)],
    )
    .await
    .unwrap();
    // Stopped after the call was checked but before it was kept.
    let stop = |_: &mut String| {
        f.hub.write(|tx| tx.put("stopped", run_id, &true)).unwrap();
    };
    let late = crate::checklist::Additions {
        items: vec![item("dns", "Add the DNS record", None)],
        ..Default::default()
    };
    let refused = (f.hub)
        .checklist_add(run_id, &task(&f, task_id), late, stop)
        .await
        .unwrap_err();
    assert_eq!(refused.code, ErrorCode::Expired);
    assert!(checklist(&f, project).item("dns").is_none());
    f.close().await;
}

#[tokio::test]
async fn a_question_for_a_held_conversation_put_away_reads_only_what_it_could() {
    use super::conversation_access::{Backend, allow_ask, create, dispatched};
    let backend = std::sync::Arc::new(Backend::default());
    let f = Fixture::with(|b| b.sandboxes(backend.clone())).await;
    let project = running_project(&f).await;
    std::fs::create_dir_all(f.project_dir().join("setup")).unwrap();
    std::fs::write(f.project_dir().join("setup/settings.json"), b"{}").unwrap();
    allow_ask(&f);
    let access = protocol::access::ConversationAccess {
        read: vec![project],
        write: vec![project],
        internet: false,
        ..Default::default()
    };
    let owner = Caller::local_owner();
    let receipt = f.hub.submit(&owner, create(project, access)).await;
    let Some(CommandResult::TaskCreated { task_id, .. }) = receipt.result else {
        panic!("{receipt:?}")
    };
    let run_id = run_started(&f, task_id).await;
    working(&f, run_id).await;
    add(
        &f,
        run_id,
        task_id,
        vec![item("api", "Grant API access", None)],
    )
    .await
    .unwrap();
    ended(&f, run_id, ports::RunExit::Completed).await;
    let revision = task(&f, task_id).revision;
    let archive = f
        .hub
        .submit(&owner, edit(Command::ArchiveTask { task_id }, revision))
        .await;
    assert_eq!(archive.status, ReceiptStatus::Accepted, "{archive:?}");
    ok(
        &f,
        Command::ChecklistAsk {
            project_id: project,
            item_id: Some("api".into()),
            text: "Which account?".into(),
        },
    )
    .await;
    f.hub.pass_on_followups().await.unwrap();
    let helper = until_task(&f, "Question about the checklist").await;
    let spec = dispatched(&f, helper.task_id).await;
    // It runs as a read-only turn with the checklist's two tools, as for any
    // agent put away, never as Work with its native tools and settings.
    assert_eq!(spec.mode, TaskMode::Ask);
    assert_eq!(spec.launch_policy, None);
    assert!(!spec.web);
    let bridge = spec.bridge.expect("the checklist's tools");
    assert!(bridge.checklist_answers && !bridge.browser && !bridge.github);
    // Inside a sandbox that reads what the conversation could, and writes
    // nothing of the project's.
    assert!(spec.sandbox.is_some());
    let held = backend.specs.lock().unwrap().last().cloned().unwrap();
    assert!(held.reach.write.is_empty() && !held.reach.whole_write);
    let read: Vec<_> = (held.reach.read.iter())
        .map(|p| p.canonicalize().unwrap())
        .collect();
    assert_eq!(read, [f.project_dir().canonicalize().unwrap()]);
    assert!(
        held.always
            .contains(&std::path::PathBuf::from("synthetic-socket"))
    );
    f.close().await;
}
