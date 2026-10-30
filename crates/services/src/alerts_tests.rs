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

    let (alerts, head) = f.hub.open_alerts(&device).await.unwrap();
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
            .alert(&device, later.attention_id)
            .await
            .unwrap()
            .is_some()
    );
    for gone in [
        answered.attention_id,
        network.attention_id,
        AttentionId::new(),
    ] {
        assert!(f.hub.alert(&device, gone).await.unwrap().is_none());
    }

    let many: Vec<_> = (0..MAX_OPEN_ALERTS as i64 + 5)
        .map(|n| attention(task, AttentionKind::Failure, 2_000_000 + n, false))
        .collect();
    store(&f, &many);
    let (alerts, _) = f.hub.open_alerts(&device).await.unwrap();
    assert_eq!(alerts.len(), MAX_OPEN_ALERTS);
    assert_eq!(alerts.first().unwrap().attention_id, many[5].attention_id);
    assert_eq!(
        alerts.last().unwrap().attention_id,
        many.last().unwrap().attention_id
    );
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
    assert!(f.hub.open_alerts(&device).await.unwrap().0.is_empty());
    assert!(
        f.hub
            .alert(&device, item.attention_id)
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
            .alert(caller, item.attention_id)
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

#[tokio::test]
async fn notification_settings_mark_the_alerts_they_turn_off() {
    use protocol::notifications::{AlertTopic, NotificationSettings, ProjectNotifications};
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
    let merged = AttentionItem {
        topic: Some(AlertTopic::Merged),
        ..attention(in_quiet, AttentionKind::Completed, 910_000, false)
    };
    let elsewhere = AttentionItem {
        topic: Some(AlertTopic::Merged),
        ..attention(in_other, AttentionKind::Completed, 920_000, false)
    };
    store(&f, &[question.clone(), merged.clone(), elsewhere.clone()]);
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
    let (alerts, _) = f.hub.open_alerts(&device).await.unwrap();
    assert!(alerts.iter().all(|a| !a.quiet && !a.bundle));
    let topics: Vec<_> = alerts.iter().map(|a| a.topic).collect();
    assert_eq!(
        topics,
        [
            Some(AlertTopic::Question),
            Some(AlertTopic::Merged),
            Some(AlertTopic::Merged)
        ]
    );

    // Merges off everywhere but one project; that one folds its alerts.
    let set = |revision| {
        let mut command = CommandEnvelope::new(Command::SetNotificationSettings {
            off: vec![AlertTopic::Merged, AlertTopic::Merged],
            bundle: false,
            projects: vec![
                ProjectNotifications {
                    on: vec![AlertTopic::Merged],
                    off: vec![AlertTopic::Question],
                    bundle: Some(true),
                    ..ProjectNotifications::new(quiet_project)
                },
                ProjectNotifications::new(other),
            ],
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
    assert_eq!(settings.off, [AlertTopic::Merged]);
    assert_eq!(
        settings.projects.len(),
        1,
        "a project saying nothing is dropped"
    );
    let quiet = async |id| {
        let alert = f.hub.alert(&device, id).await.unwrap().unwrap();
        (alert.quiet, alert.bundle)
    };
    assert_eq!(quiet(question.attention_id).await, (true, true));
    assert_eq!(quiet(merged.attention_id).await, (false, true));
    assert_eq!(quiet(elsewhere.attention_id).await, (true, false));
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
    let quiet = async |id| f.hub.alert(&device, id).await.unwrap().unwrap().quiet;
    assert!(quiet(held.attention_id).await);
    assert!(!quiet(loud.attention_id).await);
    // Listed all the same.
    let (alerts, _) = f.hub.open_alerts(&device).await.unwrap();
    assert_eq!(alerts.len(), 2);
    f.close().await;
}
