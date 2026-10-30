// SPDX-License-Identifier: Apache-2.0
use super::*;
use ports::agent_copy::{AgentUpdater, CopyState};
use protocol::agent_updates::AgentUpdatesView;
use std::sync::Mutex;

/// Copies that answer at once, noting each setting they were brought in line with.
#[derive(Debug, Default)]
struct Copies {
    asked: Mutex<Vec<bool>>,
}
#[async_trait::async_trait]
impl AgentUpdater for Copies {
    fn supported(&self) -> bool {
        true
    }
    async fn apply(&self, keep: bool) -> Vec<CopyState> {
        self.asked.lock().unwrap().push(keep);
        vec![CopyState {
            harness: HarnessKind::ClaudeCode,
            version: keep.then(|| "2.0.0".into()),
            checked: keep,
            problem: None,
        }]
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
    assert!(on.agents[0].checked_at.is_some());
    assert_eq!(copies.asked.lock().unwrap().last(), Some(&true));

    set(&f, false, 1).await;
    let off = settled(&f, None).await;
    assert!(!off.enabled);
    assert_eq!(copies.asked.lock().unwrap().last(), Some(&false));
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
