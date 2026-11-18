// SPDX-License-Identifier: Apache-2.0
use super::*;
use ports::agent_copy::{AgentUpdater, CopyState};
use protocol::agent_updates::AgentUpdatesView;
use std::sync::Mutex;

/// Copies that answer at once, noting each setting they were brought in line with.
#[derive(Debug, Default)]
struct Copies {
    asked: Mutex<Vec<bool>>,
    checks: std::sync::atomic::AtomicUsize,
    managed: std::sync::atomic::AtomicBool,
}
#[async_trait::async_trait]
impl AgentUpdater for Copies {
    fn supported(&self) -> bool {
        true
    }
    async fn apply(&self, keep: bool) -> Vec<CopyState> {
        self.asked.lock().unwrap().push(keep);
        self.managed.store(keep, Ordering::SeqCst);
        vec![CopyState {
            harness: HarnessKind::ClaudeCode,
            version: keep.then(|| "2.0.0".into()),
            latest_version: Some("2.0.0".into()),
            update_supported: true,
            released_models: vec![],
            checked: keep,
            problem: None,
        }]
    }
    async fn check(&self) -> Vec<CopyState> {
        self.checks.fetch_add(1, Ordering::SeqCst);
        vec![CopyState {
            harness: HarnessKind::ClaudeCode,
            version: self.managed.load(Ordering::SeqCst).then(|| "2.0.0".into()),
            latest_version: Some("2.0.0".into()),
            update_supported: true,
            released_models: vec![],
            checked: true,
            problem: None,
        }]
    }
    async fn update(&self, harness: HarnessKind) -> Vec<CopyState> {
        assert_eq!(harness, HarnessKind::ClaudeCode);
        self.apply(true).await
    }
    fn program(&self, _: HarnessKind) -> Option<(PathBuf, Vec<(String, String)>)> {
        None
    }
}
/// Both agents are genuinely installed in old, retained copies.
#[derive(Debug, Default)]
struct TwoOldCopies(std::sync::atomic::AtomicUsize);
#[async_trait::async_trait]
impl AgentUpdater for TwoOldCopies {
    fn supported(&self) -> bool {
        true
    }
    async fn apply(&self, _: bool) -> Vec<CopyState> {
        panic!("Notify and Never must not install")
    }
    async fn check(&self) -> Vec<CopyState> {
        self.0.fetch_add(1, Ordering::SeqCst);
        [HarnessKind::ClaudeCode, HarnessKind::Codex]
            .into_iter()
            .map(|harness| CopyState {
                harness,
                version: Some("1.0.0".into()),
                latest_version: Some("2.0.0".into()),
                update_supported: true,
                released_models: vec![],
                checked: true,
                problem: None,
            })
            .collect()
    }
    fn program(&self, _: HarnessKind) -> Option<(PathBuf, Vec<(String, String)>)> {
        None
    }
}

#[derive(Debug, Default)]
struct CheckOnly(Copies);
#[async_trait::async_trait]
impl AgentUpdater for CheckOnly {
    fn supported(&self) -> bool {
        false
    }
    async fn apply(&self, _: bool) -> Vec<CopyState> {
        panic!("unsupported installs must never run")
    }
    async fn check(&self) -> Vec<CopyState> {
        let mut copies = self.0.check().await;
        for copy in &mut copies {
            copy.update_supported = false;
        }
        copies
    }
    fn program(&self, _: HarnessKind) -> Option<(PathBuf, Vec<(String, String)>)> {
        None
    }
}

async fn view(f: &Fixture) -> AgentUpdatesView {
    let QueryData::AgentUpdates { view } = f
        .hub
        .query(&Caller::local_owner(), Query::AgentUpdates)
        .await
        .unwrap()
    else {
        panic!("agent updates")
    };
    view
}
async fn set(f: &Fixture, enabled: bool, revision: u64) {
    let mut command = CommandEnvelope::new(Command::SetAgentUpdates { enabled });
    command.expected_revision = Some(revision);
    let receipt = f.hub.submit(&Caller::local_owner(), command).await;
    assert_eq!(receipt.error, None);
}
/// Waits until a check has finished with the copies at `version`.
async fn settled(f: &Fixture, version: Option<&str>) -> AgentUpdatesView {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let v = view(f).await;
            if !v.checking && v.agents.first().map(|copy| copy.version.as_deref()) == Some(version)
            {
                return v;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("no check followed the setting")
}

#[tokio::test]
async fn changing_the_setting_starts_a_check() {
    let copies = Arc::new(Copies::default());
    let given = copies.clone();
    let f = Fixture::with(move |b| b.agent_updater(given)).await;

    set(&f, true, 0).await;
    let on = settled(&f, Some("2.0.0")).await;
    assert!(on.enabled);
    assert_eq!(on.mode, protocol::agent_updates::AgentUpdateMode::Automatic);
    assert!(on.agents[0].checked_at.is_some());
    assert_eq!(copies.asked.lock().unwrap().last(), Some(&true));

    set(&f, false, 1).await;
    let off = settled(&f, Some("2.0.0")).await;
    assert!(!off.enabled);
    assert_eq!(off.mode, protocol::agent_updates::AgentUpdateMode::Never);
    assert!(
        copies.asked.lock().unwrap().iter().all(|keep| *keep),
        "Never retains the working copy"
    );
    f.close().await;
}

#[tokio::test]
async fn manual_update_keeps_notify_policy_and_copy_across_modes() {
    use protocol::agent_updates::AgentUpdateMode;
    let copies = Arc::new(Copies::default());
    let given = copies.clone();
    let f = Fixture::with(move |b| b.agent_updater(given)).await;
    assert_eq!(view(&f).await.mode, AgentUpdateMode::Notify);
    assert!(
        copies.asked.lock().unwrap().is_empty(),
        "Notify never installs by itself"
    );
    let receipt = f
        .hub
        .submit(
            &Caller::local_owner(),
            CommandEnvelope::new(Command::UpdateAgent {
                harness: HarnessKind::ClaudeCode,
            }),
        )
        .await;
    assert_eq!(receipt.error, None);
    let updated = settled(&f, Some("2.0.0")).await;
    assert_eq!(updated.mode, AgentUpdateMode::Notify);
    assert!(!updated.enabled);
    let checks = copies.checks.load(Ordering::SeqCst);
    let receipt = f
        .hub
        .submit(
            &Caller::local_owner(),
            CommandEnvelope::new(Command::CheckAgentUpdates {}),
        )
        .await;
    assert_eq!(receipt.error, None);
    tokio::time::timeout(Duration::from_secs(5), async {
        while copies.checks.load(Ordering::SeqCst) == checks {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        settled(&f, Some("2.0.0")).await.mode,
        AgentUpdateMode::Notify
    );
    set(&f, false, 0).await;
    let never = settled(&f, Some("2.0.0")).await;
    assert_eq!(never.mode, AgentUpdateMode::Never);
    let count = copies.asked.lock().unwrap().len();
    f.hub.update_agents();
    settled(&f, Some("2.0.0")).await;
    assert_eq!(copies.asked.lock().unwrap().len(), count);
    assert!(copies.asked.lock().unwrap().iter().all(|keep| *keep));
    f.close().await;
}

#[tokio::test]
async fn scheduled_reminders_share_a_computer_budget_and_handling_survives_restart() {
    const DAY: Duration = Duration::from_secs(24 * 60 * 60);
    let copies = Arc::new(TwoOldCopies::default());
    let given = copies.clone();
    let f = Fixture::with(move |b| b.agent_updater(given)).await;
    settled(&f, Some("1.0.0")).await;
    let first_phone = f.device().await;
    let second_phone = pair(&f.hub, 2).await;
    let items = |f: &Fixture| -> Vec<AttentionItem> {
        f.hub
            .read(|r| r.list::<AttentionItem>("attention"))
            .unwrap()
            .into_iter()
            .filter(|item| item.agent_update.is_some())
            .collect()
    };
    // The scheduled service reads the persisted clock, not elapsed Tokio time.
    for day in 1..=14 {
        let before = copies.0.load(Ordering::SeqCst);
        f.clock.advance(DAY);
        f.hub.update_agents();
        settled(&f, Some("1.0.0")).await;
        assert_eq!(copies.0.load(Ordering::SeqCst), before + 1);
        assert_eq!(items(&f).len(), usize::from(day == 14));
    }
    assert_eq!(view(&f).await.agents.len(), 2);
    let first = items(&f).pop().unwrap();
    assert_eq!(
        first.agent_update.as_ref().unwrap().kind,
        protocol::agent_updates::AgentUpdateNoticeKind::Behind
    );
    for phone in [&first_phone, &second_phone] {
        assert!(
            !f.hub
                .alert_for(phone, first.attention_id, true)
                .unwrap()
                .unwrap()
                .quiet
        );
    }
    f.hub
        .mark_alerts_handled(&first_phone, &[first.attention_id])
        .unwrap();
    let saved: AttentionItem = f
        .hub
        .read(|r| r.require("attention", first.attention_id))
        .unwrap();
    assert!(saved.alert_handled_at.is_some());
    assert!(
        saved.resolved_at.is_none(),
        "handling is not acknowledgement"
    );
    let given = copies.clone();
    let f = f.reopened(move |b| b.agent_updater(given)).await;
    settled(&f, Some("1.0.0")).await;
    for day in 15..44 {
        f.clock.advance(DAY);
        f.hub.update_agents();
        settled(&f, Some("1.0.0")).await;
        assert_eq!(items(&f).len(), 1, "no new notice on day {day}");
        for phone in [&first_phone, &second_phone] {
            assert!(
                f.hub
                    .alert_for(phone, first.attention_id, true)
                    .unwrap()
                    .unwrap()
                    .quiet
            );
        }
    }
    f.clock.advance(DAY);
    f.hub.update_agents();
    settled(&f, Some("1.0.0")).await;
    let reminders = items(&f);
    assert_eq!(
        reminders.len(),
        2,
        "one further computer-wide notice after thirty days"
    );
    assert_eq!(
        reminders
            .iter()
            .filter(|item| item.resolved_at.is_none())
            .count(),
        1
    );
    let next = reminders
        .iter()
        .find(|item| item.resolved_at.is_none())
        .unwrap();
    assert_ne!(next.attention_id, first.attention_id);
    assert!(
        !f.hub
            .alert_for(&second_phone, next.attention_id, true)
            .unwrap()
            .unwrap()
            .quiet
    );
    set(&f, false, 0).await;
    settled(&f, Some("1.0.0")).await;
    let checks = copies.0.load(Ordering::SeqCst);
    f.clock.advance(DAY * 90);
    f.hub.update_agents();
    let never = settled(&f, Some("1.0.0")).await;
    assert_eq!(
        copies.0.load(Ordering::SeqCst),
        checks,
        "Never must not check the network"
    );
    assert_eq!(never.mode, protocol::agent_updates::AgentUpdateMode::Never);
    assert!(
        never
            .agents
            .iter()
            .all(|agent| agent.version.as_deref() == Some("1.0.0"))
    );
    assert!(items(&f).iter().all(|item| item.resolved_at.is_some()));
    f.close().await;
}

#[tokio::test]
async fn explicit_mode_changes_require_the_current_revision() {
    use protocol::agent_updates::AgentUpdateMode;
    let f = Fixture::with(|b| b.agent_updater(Arc::new(Copies::default()))).await;
    let mut command = CommandEnvelope::new(Command::SetAgentUpdateMode {
        mode: AgentUpdateMode::Never,
    });
    command.expected_revision = Some(0);
    assert_eq!(
        f.hub.submit(&Caller::local_owner(), command).await.error,
        None
    );
    let mut stale = CommandEnvelope::new(Command::SetAgentUpdateMode {
        mode: AgentUpdateMode::Automatic,
    });
    stale.expected_revision = Some(0);
    assert!(
        f.hub
            .submit(&Caller::local_owner(), stale)
            .await
            .error
            .is_some()
    );
    assert_eq!(view(&f).await.mode, AgentUpdateMode::Never);
    let mut notify = CommandEnvelope::new(Command::SetAgentUpdateMode {
        mode: AgentUpdateMode::Notify,
    });
    notify.expected_revision = Some(1);
    assert_eq!(
        f.hub.submit(&Caller::local_owner(), notify).await.error,
        None
    );
    let current = view(&f).await;
    assert_eq!(current.mode, AgentUpdateMode::Notify);
    assert_eq!(current.revision, 2);
    assert!(!current.enabled);
    f.close().await;
}

#[tokio::test]
async fn agent_news_is_taskless_and_handled_on_every_device() {
    use protocol::agent_updates::{AgentUpdateNotice, AgentUpdateNoticeKind};
    let f = Fixture::new().await;
    let now = ports::Clock::now(f.clock.as_ref());
    let item = AttentionItem {
        child_id: None,
        attention_id: AttentionId::new(),
        kind: AttentionKind::Completed,
        title: "Codex has new models".into(),
        body: None,
        task_id: None,
        run_id: None,
        approval_id: None,
        created_at: now,
        resolved_at: None,
        priority: 1,
        topic: None,
        quiet: false,
        report_id: None,
        account_id: None,
        alert: None,
        alert_handled_at: None,
        new_version: None,
        agent_update: Some(AgentUpdateNotice {
            harness: HarnessKind::Codex,
            kind: AgentUpdateNoticeKind::NewModelsAvailable,
            version: Some("2.0.0".into()),
            models: vec!["observed-model".into()],
        }),
    };
    f.hub
        .write(|tx| tx.put("attention", item.attention_id, &item))
        .unwrap();
    let caller = Caller::local_owner();
    let (alerts, _) = f.hub.alerts_open(&caller, true).unwrap();
    let alert = alerts
        .iter()
        .find(|a| a.attention_id == item.attention_id)
        .unwrap();
    assert!(alert.task_id.as_uuid().is_nil());
    assert_eq!(
        alert.agent_update.as_ref().unwrap().models,
        ["observed-model"]
    );
    assert!(!alert.quiet);
    f.hub
        .mark_alerts_handled(&caller, &[item.attention_id])
        .unwrap();
    assert!(
        f.hub
            .alert_for(&caller, item.attention_id, true)
            .unwrap()
            .unwrap()
            .quiet
    );
    let receipt = f
        .hub
        .submit(
            &caller,
            CommandEnvelope::new(Command::AcknowledgeAttention {
                attention_id: item.attention_id,
            }),
        )
        .await;
    assert_eq!(receipt.error, None);
    assert!(
        f.hub
            .alert_for(&caller, item.attention_id, true)
            .unwrap()
            .is_none()
    );
    f.close().await;
}

#[tokio::test]
async fn check_now_works_when_the_system_cannot_install_agent_updates() {
    let copies = Arc::new(CheckOnly::default());
    let given = copies.clone();
    let f = Fixture::with(move |b| b.agent_updater(given)).await;
    let checks = copies.0.checks.load(Ordering::SeqCst);
    let receipt = f
        .hub
        .submit(
            &Caller::local_owner(),
            CommandEnvelope::new(Command::CheckAgentUpdates {}),
        )
        .await;
    assert_eq!(receipt.error, None);
    tokio::time::timeout(Duration::from_secs(5), async {
        while copies.0.checks.load(Ordering::SeqCst) == checks {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let receipt = f
        .hub
        .submit(
            &Caller::local_owner(),
            CommandEnvelope::new(Command::UpdateAgent {
                harness: HarnessKind::ClaudeCode,
            }),
        )
        .await;
    assert_eq!(receipt.error.unwrap().code, ErrorCode::Unsupported);
    f.close().await;
}

async fn preferences(f: &Fixture) -> protocol::agent_preferences::AgentPreferences {
    let QueryData::AgentPreferences { view } = f
        .hub
        .query(&Caller::local_owner(), Query::AgentPreferences)
        .await
        .unwrap()
    else {
        panic!("agent preferences")
    };
    view
}
/// What the next run in a new project is told about questions and blockers.
async fn conduct(f: &Fixture) -> (bool, String) {
    let project = f.project().await;
    let (task, _) = f
        .task(
            project,
            TaskPlacement::StartNow,
            &Caller::local_owner(),
            false,
        )
        .await;
    let _run = run_started(f, task).await;
    let spec = f.harness.specs.lock().unwrap().last().cloned().unwrap();
    let told = ports::harness::conduct_instruction(&spec)
        .unwrap()
        .to_owned();
    (spec.questions, told)
}

#[tokio::test]
async fn runs_welcome_questions_until_the_person_turns_them_off() {
    use protocol::agent_preferences::AgentPreferences;
    let f = Fixture::new().await;
    assert_eq!(preferences(&f).await, AgentPreferences::default());
    assert!(preferences(&f).await.questions);
    let (questions, told) = conduct(&f).await;
    assert!(questions);
    assert!(told.contains("which system it comes from"), "{told}");
    assert!(!told.contains("turned questions off"), "{told}");
    assert!(told.contains("say which you would pick and why"), "{told}");
    // Either way it is told never to wait in its turn, and how not to.
    assert!(told.contains("Never wait in your turn"), "{told}");
    assert!(told.contains("call the wake_me tool"), "{told}");
    // And to run checks so their exit status says whether they passed.
    assert!(
        told.contains("Ophio reads whether it passed from its exit status"),
        "{told}"
    );
    f.close().await;

    let f = Fixture::new().await;
    let mut command = CommandEnvelope::new(Command::SetAgentPreferences { questions: false });
    command.expected_revision = Some(0);
    let receipt = f.hub.submit(&Caller::local_owner(), command).await;
    assert_eq!(receipt.error, None);
    assert_eq!(
        preferences(&f).await,
        AgentPreferences {
            questions: false,
            launch_defaults: Vec::new(),
            revision: 1
        }
    );
    // A stale revision is refused rather than undoing a newer choice.
    let mut stale = CommandEnvelope::new(Command::SetAgentPreferences { questions: true });
    stale.expected_revision = Some(0);
    let receipt = f.hub.submit(&Caller::local_owner(), stale).await;
    assert!(receipt.error.is_some());
    let (questions, told) = conduct(&f).await;
    assert!(!questions);
    assert!(told.contains("which system it comes from"), "{told}");
    assert!(told.contains("turned questions off"), "{told}");
    assert!(told.contains("call the wake_me tool"), "{told}");
    // Held or asked, a question names the choices and the one it would pick.
    assert!(told.contains("say which you would pick and why"), "{told}");
    f.close().await;
}

#[tokio::test]
async fn launch_defaults_are_kept_one_for_each_agent() {
    use protocol::{agent_preferences::LaunchDefault, model::LaunchPolicy};
    let f = Fixture::new().await;
    let owner = Caller::local_owner();
    let submit = |defaults: Vec<LaunchDefault>, revision: u64| {
        let mut command = CommandEnvelope::new(Command::SetLaunchDefaults { defaults });
        command.expected_revision = Some(revision);
        f.hub.submit(&owner, command)
    };
    let claude = LaunchDefault {
        harness: HarnessKind::ClaudeCode,
        policy: LaunchPolicy::BypassPermissions,
    };
    let codex = LaunchDefault {
        harness: HarnessKind::Codex,
        policy: LaunchPolicy::Standard,
    };
    assert_eq!(submit(vec![claude, codex], 0).await.error, None);
    let view = preferences(&f).await;
    assert_eq!(view.launch_defaults, vec![claude, codex]);
    assert_eq!(view.revision, 1);
    assert!(view.questions, "the other preferences stay as they were");

    // A stale revision is refused rather than undoing a newer choice.
    assert!(submit(Vec::new(), 0).await.error.is_some());
    // Flags an agent can't run with are refused, and so is a second default
    // for the same agent.
    let edits = LaunchDefault {
        harness: HarnessKind::Codex,
        policy: LaunchPolicy::AcceptEdits,
    };
    let receipt = submit(vec![edits], 1).await;
    assert_eq!(receipt.error.map(|e| e.code), Some(ErrorCode::Unsupported));
    let receipt = submit(vec![claude, claude], 1).await;
    assert_eq!(
        receipt.error.map(|e| e.code),
        Some(ErrorCode::InvalidRequest)
    );
    assert_eq!(preferences(&f).await.launch_defaults, vec![claude, codex]);

    // An empty list clears them.
    assert_eq!(submit(Vec::new(), 1).await.error, None);
    assert_eq!(preferences(&f).await.launch_defaults, Vec::new());
    f.close().await;
}
