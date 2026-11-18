// SPDX-License-Identifier: Apache-2.0
use super::*;
use protocol::alerts::MAX_OPEN_ALERTS;

fn attention(task_id: TaskId, kind: AttentionKind, at: i64, resolved: bool) -> AttentionItem {
    AttentionItem {
        child_id: None,
        attention_id: AttentionId::new(),
        kind,
        title: "Synthetic".into(),
        body: None,
        task_id: Some(task_id),
        run_id: None,
        approval_id: None,
        created_at: Timestamp(at),
        resolved_at: resolved.then_some(Timestamp(at + 1)),
        priority: 1,
        topic: None,
        quiet: false,
        report_id: None,
        account_id: None,
        alert: None,
        alert_handled_at: None,
        agent_update: None,
        new_version: None,
    }
}
fn store(f: &Fixture, items: &[AttentionItem]) {
    f.hub
        .write(|tx| {
            for item in items {
                tx.put("attention", item.attention_id, item)?;
            }
            Ok(())
        })
        .unwrap();
}

#[tokio::test]
async fn the_open_list_is_open_task_items_oldest_first_and_the_newest_when_long() {
    let f = Fixture::new().await;
    let owner = Caller::local_owner();
    let device = f.device().await;
    let project = f.project().await;
    let (task, _) = f
        .task(project, TaskPlacement::StartNow, &owner, false)
        .await;
    let later = attention(task, AttentionKind::Completed, 1_000_000, false);
    let earlier = attention(task, AttentionKind::Question, 900_000, false);
    let answered = attention(task, AttentionKind::Approval, 950_000, true);
    let network = attention(task, AttentionKind::ConnectionIssue, 960_000, false);
    let mut untasked = attention(task, AttentionKind::WaitingForYou, 970_000, false);
    untasked.task_id = None;
    store(
        &f,
        &[
            later.clone(),
            earlier.clone(),
            answered.clone(),
            network.clone(),
            untasked,
        ],
    );

    let (alerts, head) = f.hub.open_alerts(&device, false).await.unwrap();
    assert!(head > 0);
    let ids: Vec<_> = alerts.iter().map(|a| a.attention_id).collect();
    assert_eq!(ids, vec![earlier.attention_id, later.attention_id]);
    assert_eq!(alerts[0].project, "Test");
    assert_eq!(alerts[0].task, "Test task");
    assert_eq!(alerts[0].harness, HarnessKind::ClaudeCode);
    assert_eq!(alerts[0].kind, AttentionKind::Question);

    // One by one: the same rule.
    assert!(
        f.hub
            .alert(&device, later.attention_id, false)
            .await
            .unwrap()
            .is_some()
    );
    for gone in [
        answered.attention_id,
        network.attention_id,
        AttentionId::new(),
    ] {
        assert!(f.hub.alert(&device, gone, false).await.unwrap().is_none());
    }

    let many: Vec<_> = (0..MAX_OPEN_ALERTS as i64 + 5)
        .map(|n| attention(task, AttentionKind::Failure, 2_000_000 + n, false))
        .collect();
    store(&f, &many);
    let (alerts, _) = f.hub.open_alerts(&device, false).await.unwrap();
    assert_eq!(alerts.len(), MAX_OPEN_ALERTS);
    assert_eq!(alerts.first().unwrap().attention_id, many[5].attention_id);
    assert_eq!(
        alerts.last().unwrap().attention_id,
        many.last().unwrap().attention_id
    );
    f.close().await;
}

#[tokio::test]
async fn a_chat_notification_names_the_agent_that_raised_it_not_the_next_agent() {
    let f = Fixture::new().await;
    let owner = Caller::local_owner();
    let project = f.project().await;
    let (task_id, _) = f
        .task(project, TaskPlacement::StartNow, &owner, false)
        .await;
    let mut item = attention(task_id, AttentionKind::Question, 1_000_000, false);
    f.hub
        .write(|tx| {
            let mut task: Task = tx.read().require("task", task_id)?;
            item.run_id = task.current_run;
            task.conversation = true;
            task.harness = HarnessKind::Codex;
            tx.put("task", task_id, &task)
        })
        .unwrap();
    store(&f, std::slice::from_ref(&item));
    let alert = f
        .hub
        .alert(&owner, item.attention_id, false)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(alert.harness, HarnessKind::ClaudeCode);
    f.close().await;
}

#[tokio::test]
async fn a_device_that_may_not_see_tasks_gets_no_alerts() {
    let f = Fixture::new().await;
    let owner = Caller::local_owner();
    let device = f.device().await;
    let project = f.project().await;
    let (task, _) = f
        .task(project, TaskPlacement::StartNow, &owner, false)
        .await;
    let item = attention(task, AttentionKind::Question, 1_000_000, false);
    store(&f, std::slice::from_ref(&item));
    let Principal::Device { device_id } = device.principal else {
        panic!("device")
    };
    let receipt = f
        .hub
        .submit(
            &owner,
            CommandEnvelope::new(Command::SetDeviceGrants {
                device_id,
                expected: Some(device.grants.clone()),
                grants: Grants {
                    tasks: false,
                    ..device.grants.clone()
                },
            }),
        )
        .await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    // The stored grants decide, not the ones the connection began with.
    assert!(
        f.hub
            .open_alerts(&device, false)
            .await
            .unwrap()
            .0
            .is_empty()
    );
    assert!(
        f.hub
            .alert(&device, item.attention_id, false)
            .await
            .unwrap()
            .is_none()
    );
    f.close().await;
}

#[tokio::test]
async fn a_waiting_question_comes_with_its_words_and_choices_when_the_phone_can_answer_it() {
    use protocol::alerts::{AlertQuestion, MAX_ALERT_CHOICES};
    let f = Fixture::new().await;
    let owner = Caller::local_owner();
    let device = f.device().await;
    let project_id = f.project().await;
    let (task_id, _) = f
        .task(project_id, TaskPlacement::StartNow, &owner, false)
        .await;
    let option = |label: &str| ApprovalQuestionOption {
        label: label.into(),
        description: None,
    };
    let asked = |question: &str, options: Vec<ApprovalQuestionOption>| ApprovalQuestion {
        id: None,
        header: Some("Repo".into()),
        question: question.into(),
        options,
        multi_select: false,
        secret: false,
    };
    let approval = |questions: Vec<ApprovalQuestion>, detail: Option<&str>| Approval {
        headline: None,
        facts: Vec::new(),
        approval_id: ApprovalId::new(),
        run_id: RunId::new(),
        task_id,
        project_id,
        native_request_id: "native".into(),
        kind: ApprovalKind::Question,
        title: "Create a private clankerhub repo?".into(),
        detail: detail.map(Into::into),
        detail_lines: vec![],
        questions,
        raw_request: None,
        scope: "This request once".into(),
        shortened: false,
        revision: 3,
        state: ApprovalState::Pending,
        access_request: None,
        actions: None,
        delivery: None,
        outcome: None,
        requested_at: Timestamp(1_000_000),
        expires_at: None,
        resolved_by: None,
        resolved_at: None,
    };
    let ask = |a: &Approval| AttentionItem {
        approval_id: Some(a.approval_id),
        ..attention(task_id, AttentionKind::Question, 1_000_000, false)
    };
    let alert_of = async |a: &Approval, caller: &Caller| {
        let item = ask(a);
        f.hub
            .write(|tx| {
                tx.put("approval", a.approval_id, a)?;
                tx.put("attention", item.attention_id, &item)
            })
            .unwrap();
        f.hub
            .alert(caller, item.attention_id, false)
            .await
            .unwrap()
            .unwrap()
    };

    // One question: its words, its choices, and what to answer it by.
    let one = approval(
        vec![asked(
            "Create a private clankerhub repo?",
            vec![option("Yes, private"), option("Not yet")],
        )],
        None,
    );
    assert_eq!(
        alert_of(&one, &device).await.question,
        Some(AlertQuestion {
            approval_id: one.approval_id,
            revision: 3,
            text: "Create a private clankerhub repo?".into(),
            asked: Some("Create a private clankerhub repo?".into()),
            choices: vec!["Yes, private".into(), "Not yet".into()],
        })
    );

    // Choices that don't fit whole, or several to pick, leave words only.
    let long = "x".repeat(protocol::alerts::MAX_ALERT_CHOICE + 1);
    let many: Vec<_> = (0..=MAX_ALERT_CHOICES)
        .map(|n| option(&n.to_string()))
        .collect();
    let mut multi = asked("Which reports?", vec![option("Invoices")]);
    multi.multi_select = true;
    for question in [
        asked("Which name?", vec![option("Short"), option(&long)]),
        asked("Which number?", many),
        multi,
    ] {
        let words = alert_of(&approval(vec![question.clone()], None), &device).await;
        let words = words.question.unwrap();
        assert_eq!(words.asked, Some(question.question.clone()));
        assert!(words.choices.is_empty(), "{question:?}");
    }

    // Asked in words: the words, answered in words.
    let plain = approval(vec![], Some("Which branch should I use?"));
    let words = alert_of(&plain, &device).await.question.unwrap();
    assert_eq!(words.text, "Which branch should I use?");
    assert_eq!(words.asked, None);
    let bare = approval(vec![], None);
    let words = alert_of(&bare, &device).await.question.unwrap();
    assert_eq!(words.text, "Create a private clankerhub repo?");
    let long = approval(vec![], Some(&"y".repeat(5000)));
    let words = alert_of(&long, &device).await.question.unwrap();
    assert_eq!(
        words.text.chars().count(),
        protocol::alerts::MAX_ALERT_QUESTION + 1
    );

    // Several questions, a secret, one already answered, or a phone that may
    // not answer: the notification only says there's a question.
    let mut secret = asked("What's the staging password?", vec![]);
    secret.secret = true;
    let mut settled = one.clone();
    settled.approval_id = ApprovalId::new();
    settled.state = ApprovalState::Allowed;
    for a in [
        approval(vec![asked("One?", vec![]), asked("Two?", vec![])], None),
        approval(vec![secret], None),
        settled,
    ] {
        let alert = alert_of(&a, &device).await;
        assert_eq!(alert.kind, AttentionKind::Question);
        assert_eq!(alert.question, None, "{a:?}");
    }
    let Principal::Device { device_id } = device.principal else {
        panic!("device")
    };
    let receipt = f
        .hub
        .submit(
            &owner,
            CommandEnvelope::new(Command::SetDeviceGrants {
                device_id,
                expected: Some(device.grants.clone()),
                grants: Grants {
                    approvals: false,
                    ..device.grants.clone()
                },
            }),
        )
        .await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    let other = approval(one.questions.clone(), None);
    assert_eq!(alert_of(&other, &device).await.question, None);
    f.close().await;
}

/// An older phone's settings screen saves topics turned off: they are kept
/// as the choices by type that replaced them, and every phone is told the
/// same alerts are quiet.
#[tokio::test]
async fn notification_settings_mark_the_alerts_they_turn_off() {
    use protocol::notifications::{
        AlertTopic, AlertType, NotificationSettings, ProjectNotifications, Tier, TypeTier,
    };
    let f = Fixture::new().await;
    let owner = Caller::local_owner();
    let device = f.device().await;
    let quiet_project = f.project().await;
    let dir = f.project_dir().join("other");
    std::fs::create_dir_all(&dir).unwrap();
    let added = f
        .hub
        .submit(
            &owner,
            CommandEnvelope::new(Command::AddProject {
                host_path: dir.to_string_lossy().into_owned(),
                name: None,
            }),
        )
        .await;
    let Some(CommandResult::ProjectAdded { project_id: other }) = added.result else {
        panic!("project expected: {added:?}");
    };
    let (in_quiet, _) = f
        .task(quiet_project, TaskPlacement::StartNow, &owner, false)
        .await;
    let (in_other, _) = f.task(other, TaskPlacement::StartNow, &owner, false).await;
    let question = attention(in_quiet, AttentionKind::Question, 900_000, false);
    let ready = AttentionItem {
        topic: Some(AlertTopic::PrReady),
        ..attention(in_quiet, AttentionKind::Completed, 910_000, false)
    };
    let elsewhere = AttentionItem {
        topic: Some(AlertTopic::PrReady),
        ..attention(in_other, AttentionKind::Completed, 920_000, false)
    };
    store(&f, &[question.clone(), ready.clone(), elsewhere.clone()]);
    let read = async || {
        let QueryData::NotificationSettings { view } = f
            .hub
            .query(&device, Query::NotificationSettings)
            .await
            .unwrap()
        else {
            panic!("notification settings")
        };
        view
    };
    assert_eq!(read().await, NotificationSettings::default());
    let (alerts, _) = f.hub.open_alerts(&device, false).await.unwrap();
    assert!(alerts.iter().all(|a| !a.quiet && !a.bundle));
    let topics: Vec<_> = alerts.iter().map(|a| a.topic).collect();
    assert_eq!(
        topics,
        [
            Some(AlertTopic::Question),
            Some(AlertTopic::PrReady),
            Some(AlertTopic::PrReady)
        ]
    );

    // Ready pull requests off everywhere but one project; that one folds
    // its alerts and has questions off.
    let set = |revision| {
        let mut command = CommandEnvelope::new(Command::SetNotificationSettings {
            off: vec![AlertTopic::PrReady, AlertTopic::PrReady],
            bundle: false,
            projects: vec![
                ProjectNotifications {
                    on: vec![AlertTopic::PrReady],
                    off: vec![AlertTopic::Question],
                    bundle: Some(true),
                    ..ProjectNotifications::new(quiet_project)
                },
                ProjectNotifications::new(other),
            ],
            types: false,
        });
        command.expected_revision = Some(revision);
        command
    };
    let mut live = f.hub.subscribe(&device, None).await.unwrap().live;
    let receipt = f.hub.submit(&device, set(0)).await;
    assert_eq!(receipt.error, None);
    // Every watcher is told to list its alerts again.
    let told = tokio::time::timeout(WAIT, async {
        loop {
            if let Ok(crate::api::Outbound::Event(e)) = live.recv().await
                && matches!(e.event, Event::NotificationSettingsChanged {})
            {
                return;
            }
        }
    })
    .await;
    assert!(told.is_ok(), "no NotificationSettingsChanged");
    let settings = read().await;
    assert_eq!(settings.revision, 1);
    let off = |alert_type| TypeTier {
        alert_type,
        tier: Tier::Off,
    };
    assert_eq!(
        settings,
        NotificationSettings {
            off: Vec::new(),
            bundle: false,
            // Ready pull requests off everywhere is the computer's, for
            // projects made later too; the project that turned them back
            // on keeps the recommended choice.
            projects: vec![ProjectNotifications {
                bundle: Some(true),
                tiers: vec![
                    off(AlertType::Question),
                    TypeTier {
                        alert_type: AlertType::PrReady,
                        tier: Tier::Silent,
                    },
                ],
                ..ProjectNotifications::new(quiet_project)
            }],
            tiers: vec![off(AlertType::PrReady)],
            revision: 1,
        }
    );
    for types in [false, true] {
        let quiet = async |id| {
            let alert = f.hub.alert(&device, id, types).await.unwrap().unwrap();
            (alert.quiet, alert.bundle)
        };
        assert_eq!(quiet(question.attention_id).await, (true, true));
        assert_eq!(quiet(ready.attention_id).await, (false, true));
        assert_eq!(quiet(elsewhere.attention_id).await, (true, false));
    }
    // A stale revision is refused rather than undoing a newer choice.
    let stale = f.hub.submit(&device, set(0)).await;
    assert_eq!(stale.error.unwrap().code, ErrorCode::StaleRevision);
    f.close().await;
}

#[tokio::test]
async fn an_item_raised_quietly_is_a_quiet_alert_whatever_the_settings() {
    let f = Fixture::new().await;
    let owner = Caller::local_owner();
    let device = f.device().await;
    let project = f.project().await;
    let (task, _) = f
        .task(project, TaskPlacement::StartNow, &owner, false)
        .await;
    let held = AttentionItem {
        quiet: true,
        ..attention(task, AttentionKind::Completed, 900_000, false)
    };
    let loud = attention(task, AttentionKind::Question, 910_000, false);
    store(&f, &[held.clone(), loud.clone()]);
    let quiet = async |id| {
        f.hub
            .alert(&device, id, false)
            .await
            .unwrap()
            .unwrap()
            .quiet
    };
    assert!(quiet(held.attention_id).await);
    assert!(!quiet(loud.attention_id).await);
    // Listed all the same.
    let (alerts, _) = f.hub.open_alerts(&device, false).await.unwrap();
    assert_eq!(alerts.len(), 2);
    f.close().await;
}

#[tokio::test]
async fn an_older_phones_save_keeps_each_projects_choices_by_type() {
    use protocol::notifications::{AlertType, ProjectNotifications, Tier, TypeTier};
    let f = Fixture::new().await;
    let device = f.device().await;
    let project = f.project().await;
    let save = async |p: ProjectNotifications, types: bool, revision: u64| {
        let mut command = CommandEnvelope::new(Command::SetNotificationSettings {
            off: Vec::new(),
            bundle: false,
            projects: vec![p],
            types,
        });
        command.expected_revision = Some(revision);
        let receipt = f.hub.submit(&device, command).await;
        assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    };
    let quiet_question = TypeTier {
        alert_type: AlertType::Question,
        tier: Tier::Silent,
    };
    save(
        ProjectNotifications {
            tiers: vec![quiet_question],
            ..ProjectNotifications::new(project)
        },
        true,
        0,
    )
    .await;
    // An older phone folds the project's alerts; it knows nothing of types.
    save(
        ProjectNotifications {
            bundle: Some(true),
            ..ProjectNotifications::new(project)
        },
        false,
        1,
    )
    .await;
    let settings = f.hub.notification_settings_view().unwrap();
    let p = settings.project(project).unwrap();
    assert_eq!(p.bundle, Some(true));
    assert_eq!(p.tiers, vec![quiet_question]);
    // A newer phone's save is the whole of it: cleared, it goes.
    save(ProjectNotifications::new(project), true, 2).await;
    assert!(
        f.hub
            .notification_settings_view()
            .unwrap()
            .project(project)
            .is_none()
    );
    f.close().await;
}

#[tokio::test]
async fn an_older_phone_is_told_quiet_what_the_recommended_choices_turn_off() {
    let f = Fixture::new().await;
    let owner = Caller::local_owner();
    let device = f.device().await;
    let project = f.project().await;
    let (task, _) = f
        .task(project, TaskPlacement::StartNow, &owner, false)
        .await;
    // Finished is off by default; a question alerts.
    let finished = attention(task, AttentionKind::Completed, 900_000, false);
    let question = attention(task, AttentionKind::Question, 910_000, false);
    store(&f, &[finished.clone(), question.clone()]);
    let quiet = async |id, types| {
        f.hub
            .alert(&device, id, types)
            .await
            .unwrap()
            .unwrap()
            .quiet
    };
    // An older phone holds nothing back itself: it is told.
    assert!(quiet(finished.attention_id, false).await);
    assert!(!quiet(question.attention_id, false).await);
    // A phone that chooses by type is sent it, to choose.
    assert!(!quiet(finished.attention_id, true).await);
    // Following the task, the older phone hears it finish too.
    let receipt = f
        .hub
        .submit(
            &device,
            CommandEnvelope::new(Command::SetTaskNotify {
                task_id: task,
                on: true,
            }),
        )
        .await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    assert!(!quiet(finished.attention_id, false).await);
    f.close().await;
}

/// Each place that raises an attention item, with the item built as it
/// builds it, and the notification type the person should be told it is
/// (the plan's "Proposed default" table and `AlertType`'s own words).
#[tokio::test]
async fn every_kind_of_item_is_sent_as_the_type_the_person_chooses_it_by() {
    use crate::github_watch::headline_item;
    use AttentionKind::*;
    use protocol::notifications::{AlertTopic as T, AlertType as A};
    let f = Fixture::new().await;
    let owner = Caller::local_owner();
    let project = f.project().await;
    let (task_id, _) = f
        .task(project, TaskPlacement::StartNow, &owner, false)
        .await;
    let task: Task = f.hub.read(|r| r.require("task", task_id)).unwrap();
    let now = Timestamp(1_000_000);
    let plain = |kind| attention(task_id, kind, now.0, false);
    let headline = |kind, topic| headline_item(task_id, kind, topic, "Heading", None, now);
    let typed = |alert_type, item: AttentionItem| AttentionItem {
        alert: Some(alert_type),
        ..item
    };
    let report = |kind, task_id| AttentionItem {
        report_id: Some(ReportId::new()),
        task_id,
        topic: Some(T::Reports),
        ..plain(kind)
    };
    let cases: Vec<(&str, AttentionItem, bool, Option<A>)> = vec![
        ("an agent asks", plain(Question), false, Some(A::Question)),
        (
            "an agent waits for leave",
            plain(Approval),
            false,
            Some(A::Approval),
        ),
        (
            "an agent isn't signed in",
            AttentionItem {
                task_id: None,
                account_id: Some("claude".into()),
                topic: Some(T::NeedsYou),
                ..plain(WaitingForYou)
            },
            false,
            Some(A::SignIn),
        ),
        ("a conflict", plain(Conflict), false, Some(A::Decision)),
        (
            "an end Øphio can't confirm",
            plain(ReconciliationRequired),
            false,
            Some(A::Decision),
        ),
        (
            "a helper whose end is unknown",
            AttentionItem {
                child_id: Some(ChildId::new()),
                ..plain(ReconciliationRequired)
            },
            false,
            Some(A::Decision),
        ),
        (
            "the agent waits for the person to watch the screen",
            headline(WaitingForYou, T::NeedsYou),
            false,
            Some(A::Decision),
        ),
        (
            "a report to look at before anything is published",
            typed(A::Decision, headline(WaitingForYou, T::Issues)),
            false,
            Some(A::Decision),
        ),
        ("a run failed", plain(Failure), false, Some(A::Failed)),
        (
            "stopped: no sign of work",
            typed(A::Failed, headline(WaitingForYou, T::SlowWork)),
            false,
            Some(A::Failed),
        ),
        (
            "a review's findings weren't checked",
            headline(WaitingForYou, T::Finished),
            false,
            Some(A::Failed),
        ),
        (
            "a finder's findings weren't filed",
            typed(A::Failed, headline(WaitingForYou, T::Issues)),
            false,
            Some(A::Failed),
        ),
        (
            "a pull request needs the person",
            headline(WaitingForYou, T::PrNeedsYou),
            false,
            Some(A::PrNeedsYou),
        ),
        (
            "a report's fix waits on its pull request",
            AttentionItem {
                report_id: Some(ReportId::new()),
                topic: Some(T::PrNeedsYou),
                ..plain(WaitingForYou)
            },
            false,
            Some(A::PrNeedsYou),
        ),
        (
            "a report the person sent has a question",
            report(Question, None),
            false,
            Some(A::ReportQuestion),
        ),
        (
            "Next up waits for a go-ahead",
            crate::runs::go_ahead(&task, now),
            false,
            Some(A::GoAhead),
        ),
        ("a reply in a chat", plain(Completed), true, Some(A::Reply)),
        (
            "a pull request is ready",
            headline(Completed, T::PrReady),
            false,
            Some(A::PrReady),
        ),
        (
            "what a Run all came to",
            typed(A::Batch, headline(Completed, T::PrReady)),
            false,
            Some(A::Batch),
        ),
        (
            "an issue couldn't be reproduced",
            headline(WaitingForYou, T::Issues),
            false,
            Some(A::IssueCall),
        ),
        (
            "a report moved on",
            report(Completed, None),
            false,
            Some(A::Reports),
        ),
        (
            "someone sent a report",
            report(WaitingForYou, None),
            false,
            Some(A::Reports),
        ),
        (
            "a task finished",
            plain(Completed),
            false,
            Some(A::Finished),
        ),
        (
            "a pull request merged",
            headline(Completed, T::Merged),
            false,
            Some(A::Merged),
        ),
        (
            "issues filed, or closed as duplicates",
            headline(Completed, T::Issues),
            false,
            Some(A::Issues),
        ),
        (
            "a step is taking long",
            headline(WaitingForYou, T::SlowWork),
            false,
            Some(A::Slow),
        ),
        ("the network", plain(ConnectionIssue), false, None),
    ];
    for (what, item, conversation, intended) in cases {
        assert_eq!(A::of(&item, conversation), intended, "{what}");
    }
    f.close().await;
}

/// A phone that chooses by type keeps the person's choices itself, so it
/// is told each alert's type, its project's own choice and whether its
/// task is followed. The computer's own quiet mark, a notification handled
/// already, and what the person put away on the computer make an alert
/// quiet, for every phone alike.
#[tokio::test]
async fn a_phone_choosing_by_type_is_told_the_type_its_projects_choice_and_follow() {
    use protocol::notifications::{
        AlertType, NotificationSettings, ProjectNotifications, Tier, TypeTier,
    };
    let f = Fixture::new().await;
    let owner = Caller::local_owner();
    let device = f.device().await;
    let mine = f.project().await;
    let dir = f.project_dir().join("muted");
    std::fs::create_dir_all(&dir).unwrap();
    let added = f
        .hub
        .submit(
            &owner,
            CommandEnvelope::new(Command::AddProject {
                host_path: dir.to_string_lossy().into_owned(),
                name: None,
            }),
        )
        .await;
    let Some(CommandResult::ProjectAdded { project_id: muted }) = added.result else {
        panic!("project expected: {added:?}");
    };
    let (followed, _) = f.task(mine, TaskPlacement::StartNow, &owner, false).await;
    let (elsewhere, _) = f.task(muted, TaskPlacement::StartNow, &owner, false).await;
    let receipt = f
        .hub
        .submit(
            &device,
            CommandEnvelope::new(Command::SetTaskNotify {
                task_id: followed,
                on: true,
            }),
        )
        .await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    let settings = NotificationSettings {
        projects: vec![
            ProjectNotifications {
                tiers: vec![TypeTier {
                    alert_type: AlertType::Finished,
                    tier: Tier::Alert,
                }],
                ..ProjectNotifications::new(mine)
            },
            ProjectNotifications {
                mute: true,
                ..ProjectNotifications::new(muted)
            },
        ],
        ..NotificationSettings::default()
    };
    f.hub
        .write(|tx| tx.put("meta", "notification_settings", &settings))
        .unwrap();
    let question = attention(followed, AttentionKind::Question, 900_000, false);
    let finished = attention(followed, AttentionKind::Completed, 910_000, false);
    let failed = attention(elsewhere, AttentionKind::Failure, 920_000, false);
    let held = AttentionItem {
        quiet: true,
        ..attention(followed, AttentionKind::Failure, 930_000, false)
    };
    let handled = AttentionItem {
        alert_handled_at: Some(Timestamp(940_001)),
        ..attention(followed, AttentionKind::Approval, 940_000, false)
    };
    store(
        &f,
        &[
            question.clone(),
            finished.clone(),
            failed.clone(),
            held.clone(),
            handled.clone(),
        ],
    );
    let told = async |types: bool| {
        let (alerts, _) = f.hub.open_alerts(&device, types).await.unwrap();
        (alerts.into_iter())
            .map(|a| (a.alert_type, a.project_tier, a.follow, a.quiet))
            .collect::<Vec<_>>()
    };
    // What each is, and the project's word for it, whichever the phone.
    let both = [
        (Some(AlertType::Question), None, false, false),
        (Some(AlertType::Finished), Some(Tier::Alert), true, false),
        // A muted project says Off, and is quiet.
        (Some(AlertType::Failed), Some(Tier::Off), false, true),
        // Raised quietly, or handled on a device.
        (Some(AlertType::Failed), None, true, true),
        (Some(AlertType::Approval), None, false, true),
    ];
    assert_eq!(told(false).await, both);
    assert_eq!(told(true).await, both);
    let one = f
        .hub
        .alert(&device, finished.attention_id, true)
        .await
        .unwrap()
        .unwrap();
    assert_eq!((one.follow, one.quiet), (true, false));
    f.close().await;
}

/// A phone says it handled notifications: those it could be sent are marked
/// handled once, published, and kept across a restart; the rest are passed
/// over without a word.
#[tokio::test]
async fn handled_alerts_are_kept_for_what_the_phone_could_be_sent() {
    let f = Fixture::new().await;
    let owner = Caller::local_owner();
    let device = f.device().await;
    let project = f.project().await;
    let (task, _) = f
        .task(project, TaskPlacement::StartNow, &owner, false)
        .await;
    let open = attention(task, AttentionKind::Question, 900_000, false);
    let closed = attention(task, AttentionKind::Approval, 910_000, true);
    let network = attention(task, AttentionKind::ConnectionIssue, 920_000, false);
    // An agent's sign-in is only for devices that manage accounts.
    let sign_in = AttentionItem {
        task_id: None,
        account_id: Some("claude".into()),
        topic: Some(protocol::notifications::AlertTopic::NeedsYou),
        ..attention(task, AttentionKind::WaitingForYou, 930_000, false)
    };
    store(
        &f,
        &[
            open.clone(),
            closed.clone(),
            network.clone(),
            sign_in.clone(),
        ],
    );
    let mut live = f.hub.subscribe(&device, None).await.unwrap().live;
    f.clock.advance(Duration::from_secs(5));
    let at = f.hub.inner.clock.now();
    let ids = [
        open.attention_id,
        open.attention_id,
        closed.attention_id,
        network.attention_id,
        sign_in.attention_id,
        AttentionId::new(),
    ];
    f.hub.mark_alerts_handled(&device, &ids).unwrap();
    let item = |id| {
        f.hub
            .read(|r| r.require::<AttentionItem>("attention", id))
            .unwrap()
    };
    assert_eq!(item(open.attention_id).alert_handled_at, Some(at));
    for passed in [&closed, &network, &sign_in] {
        assert_eq!(
            item(passed.attention_id).alert_handled_at,
            None,
            "{passed:?}"
        );
    }
    // Published once, so every phone puts it away.
    let upserted = tokio::time::timeout(WAIT, async {
        loop {
            if let Ok(crate::api::Outbound::Event(e)) = live.recv().await
                && let Event::AttentionUpserted { item } = &e.event
            {
                return item.clone();
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(upserted.attention_id, open.attention_id);
    assert_eq!(upserted.alert_handled_at, Some(at));
    // Handled again later: the first time stands, and nothing is published.
    f.clock.advance(Duration::from_secs(5));
    f.hub
        .mark_alerts_handled(&device, &[open.attention_id])
        .unwrap();
    assert_eq!(item(open.attention_id).alert_handled_at, Some(at));
    assert!(
        tokio::time::timeout(Duration::from_millis(50), async {
            loop {
                if let Ok(crate::api::Outbound::Event(e)) = live.recv().await
                    && matches!(e.event, Event::AttentionUpserted { .. })
                {
                    return;
                }
            }
        })
        .await
        .is_err()
    );
    // A device that manages accounts may handle the sign-in.
    f.hub
        .mark_alerts_handled(&owner, &[sign_in.attention_id])
        .unwrap();
    assert!(item(sign_in.attention_id).alert_handled_at.is_some());
    // Kept across a restart, and every phone's list says it quietly.
    let f = f.reopened(|b| b).await;
    assert_eq!(item_after(&f, open.attention_id).alert_handled_at, Some(at));
    for types in [false, true] {
        let alert = f
            .hub
            .alert(&device, open.attention_id, types)
            .await
            .unwrap()
            .unwrap();
        assert!(alert.quiet, "types {types}");
    }
    f.close().await;
}
fn item_after(f: &Fixture, id: AttentionId) -> AttentionItem {
    f.hub
        .read(|r| r.require::<AttentionItem>("attention", id))
        .unwrap()
}

/// "Tell me when it's done" is kept on the task and published with it,
/// once per change; an unknown task is refused.
#[tokio::test]
async fn tell_me_when_its_done_is_kept_on_the_task() {
    let f = Fixture::new().await;
    let owner = Caller::local_owner();
    let device = f.device().await;
    let project = f.project().await;
    let (task_id, _) = f
        .task(project, TaskPlacement::StartNow, &owner, false)
        .await;
    let task = || -> Task { f.hub.read(|r| r.require("task", task_id)).unwrap() };
    let before = task();
    assert!(!before.notify_done);
    let mut live = f.hub.subscribe(&device, None).await.unwrap().live;
    let set = |on| CommandEnvelope::new(Command::SetTaskNotify { task_id, on });
    let receipt = f.hub.submit(&device, set(true)).await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    let after = task();
    assert!(after.notify_done);
    assert_eq!(after.revision, before.revision + 1);
    let published = tokio::time::timeout(WAIT, async {
        loop {
            if let Ok(crate::api::Outbound::Event(e)) = live.recv().await
                && let Event::TaskUpserted { task } = &e.event
                && task.task_id == task_id
            {
                return task.clone();
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(published, after);
    // Asked again, nothing changes.
    let receipt = f.hub.submit(&device, set(true)).await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    assert_eq!(task().revision, after.revision);
    let receipt = f.hub.submit(&device, set(false)).await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    assert!(!task().notify_done);
    assert_eq!(task().revision, after.revision + 1);
    let unknown = f
        .hub
        .submit(
            &device,
            CommandEnvelope::new(Command::SetTaskNotify {
                task_id: TaskId::new(),
                on: true,
            }),
        )
        .await;
    assert_eq!(unknown.error.unwrap().code, ErrorCode::NotFound);
    f.close().await;
}

/// Muting a project, or turning a type Off in it, makes what it already
/// raised quiet for every phone, so each puts it away; a task the person
/// asked to hear about is heard even so, and unmuting makes the rest loud
/// again.
#[tokio::test]
async fn muting_a_project_quiets_what_it_raised_but_not_a_followed_task() {
    use protocol::notifications::{AlertType, ProjectNotifications, Tier, TypeTier};
    let f = Fixture::new().await;
    let owner = Caller::local_owner();
    let device = f.device().await;
    let project = f.project().await;
    let (followed, _) = f
        .task(project, TaskPlacement::StartNow, &owner, false)
        .await;
    let receipt = f
        .hub
        .submit(
            &device,
            CommandEnvelope::new(Command::SetTaskNotify {
                task_id: followed,
                on: true,
            }),
        )
        .await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    // The task is followed, but a question or an approval is no finish.
    let question = attention(followed, AttentionKind::Question, 900_000, false);
    let finished = attention(followed, AttentionKind::Completed, 910_000, false);
    let failed = attention(followed, AttentionKind::Failure, 920_000, false);
    let asked = attention(followed, AttentionKind::Approval, 930_000, false);
    store(
        &f,
        &[
            question.clone(),
            finished.clone(),
            failed.clone(),
            asked.clone(),
        ],
    );
    let quiet = async |types| {
        let (alerts, _) = f.hub.open_alerts(&device, types).await.unwrap();
        (alerts.into_iter())
            .map(|a| (a.attention_id, a.quiet))
            .collect::<Vec<_>>()
    };
    let expect = |q: [bool; 4]| {
        vec![
            (question.attention_id, q[0]),
            (finished.attention_id, q[1]),
            (failed.attention_id, q[2]),
            (asked.attention_id, q[3]),
        ]
    };
    for types in [false, true] {
        assert_eq!(quiet(types).await, expect([false; 4]), "types {types}");
    }
    let mut live = f.hub.subscribe(&device, None).await.unwrap().live;
    let mut revision = 0;
    let mut set = async |p: ProjectNotifications| {
        let mut command = CommandEnvelope::new(Command::SetNotificationSettings {
            off: Vec::new(),
            bundle: false,
            projects: vec![p],
            types: true,
        });
        command.expected_revision = Some(revision);
        let receipt = f.hub.submit(&device, command).await;
        assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
        revision += 1;
        // Every watcher is told to list its alerts again.
        tokio::time::timeout(WAIT, async {
            loop {
                if let Ok(crate::api::Outbound::Event(e)) = live.recv().await
                    && matches!(e.event, Event::NotificationSettingsChanged {})
                {
                    return;
                }
            }
        })
        .await
        .expect("no NotificationSettingsChanged");
    };

    set(ProjectNotifications {
        mute: true,
        ..ProjectNotifications::new(project)
    })
    .await;
    for types in [false, true] {
        assert_eq!(
            quiet(types).await,
            expect([true, false, false, true]),
            "types {types}"
        );
    }
    // Off for one type only: the rest of the project is heard.
    set(ProjectNotifications {
        tiers: vec![TypeTier {
            alert_type: AlertType::Question,
            tier: Tier::Off,
        }],
        ..ProjectNotifications::new(project)
    })
    .await;
    for types in [false, true] {
        assert_eq!(
            quiet(types).await,
            expect([true, false, false, false]),
            "types {types}"
        );
    }
    set(ProjectNotifications::new(project)).await;
    for types in [false, true] {
        assert_eq!(quiet(types).await, expect([false; 4]), "types {types}");
    }
    f.close().await;
}

/// Topic switches saved before notification types existed are brought
/// over as Øphio starts, once: the computer keeps them as stricter choices
/// by type, tells every phone, and older and newer phones are told the
/// same alerts are quiet.
#[tokio::test]
async fn old_switches_are_brought_over_once_as_ophio_starts() {
    use protocol::notifications::{
        AlertTopic, AlertType, NotificationSettings, ProjectNotifications, Tier, TypeTier,
    };
    let f = Fixture::new().await;
    let owner = Caller::local_owner();
    let device = f.device().await;
    let project = f.project().await;
    let (task, _) = f
        .task(project, TaskPlacement::StartNow, &owner, false)
        .await;
    let question = attention(task, AttentionKind::Question, 900_000, false);
    let approval = attention(task, AttentionKind::Approval, 910_000, false);
    let failed = attention(task, AttentionKind::Failure, 920_000, false);
    store(&f, &[question.clone(), approval.clone(), failed.clone()]);
    // As an older computer saved them: questions off everywhere, back on
    // in this project, which turned approvals off; failures on.
    let old = NotificationSettings {
        off: vec![AlertTopic::Question, AlertTopic::Merged],
        projects: vec![ProjectNotifications {
            on: vec![AlertTopic::Question, AlertTopic::Failed],
            off: vec![AlertTopic::Approval],
            ..ProjectNotifications::new(project)
        }],
        revision: 3,
        ..NotificationSettings::default()
    };
    f.hub
        .write(|tx| tx.put("meta", "notification_settings", &old))
        .unwrap();
    let head = f.hub.subscribe(&device, None).await.unwrap().head;

    let f = f.reopened(|b| b).await;
    let settings = f.hub.notification_settings_view().unwrap();
    // Questions off everywhere stay so for every project, new ones too;
    // this one turned them back on, and keeps them as recommended.
    let expected = NotificationSettings {
        projects: vec![ProjectNotifications {
            tiers: vec![
                TypeTier {
                    alert_type: AlertType::Question,
                    tier: Tier::Alert,
                },
                TypeTier {
                    alert_type: AlertType::Approval,
                    tier: Tier::Off,
                },
            ],
            ..ProjectNotifications::new(project)
        }],
        tiers: vec![TypeTier {
            alert_type: AlertType::Question,
            tier: Tier::Off,
        }],
        revision: 4,
        ..NotificationSettings::default()
    };
    assert_eq!(settings, expected);
    let Backlog::Events(events) = f.hub.subscribe(&device, Some(head)).await.unwrap().backlog
    else {
        panic!("backlog expected")
    };
    assert!(
        (events.iter()).any(|e| matches!(e.event, Event::NotificationSettingsChanged {})),
        "phones are told"
    );
    for types in [false, true] {
        let (alerts, _) = f.hub.open_alerts(&device, types).await.unwrap();
        let quiet: Vec<_> = alerts.iter().map(|a| (a.attention_id, a.quiet)).collect();
        assert_eq!(
            quiet,
            [
                (question.attention_id, false),
                (approval.attention_id, true),
                (failed.attention_id, false),
            ],
            "types {types}"
        );
    }

    // A project made afterwards follows the computer: questions off.
    assert_eq!(
        settings.project_tier(ProjectId::new(), AlertType::Question),
        Some(Tier::Off)
    );

    // Started again, nothing is brought over twice.
    let f = f.reopened(|b| b).await;
    assert_eq!(f.hub.notification_settings_view().unwrap(), expected);
    f.close().await;
}
