// SPDX-License-Identifier: Apache-2.0
//! Going back to 0.2 after this version ran: whatever the person changed
//! here, 0.2 must still read what it reads, or it can't start.
//!
//! The `v0_2` types are copied from `release/0.2` (crates/protocol: the
//! notification settings, the attention item and the host record) and read
//! as strictly as 0.2 reads them. This version changes what it stores in
//! those, adds `meta/release_check`, which 0.2 never reads, and journals
//! no event 0.2 doesn't know. Stored `SetReleaseCheck` commands are the
//! one thing 0.2 can't decode, and it keeps those as unknown outcomes, as
//! docs/upgrading.md says of every command an older version doesn't know.
use super::*;
use ports::releases::{LatestRelease, ReleaseSource};
use protocol::notifications::{AlertTopic, AlertType, ProjectNotifications, Tier, TypeTier};

#[allow(dead_code)]
mod v0_2 {
    use protocol::{
        ApprovalId, AttentionId, ChildId, ProjectId, ReportId, RunId, TaskId, model::AttentionKind,
        time::Timestamp,
    };
    use serde::Deserialize;

    /// 0.2's project shape, without the new remembered chat access.
    #[derive(Debug, Deserialize)]
    pub struct Project {
        pub project_id: ProjectId,
        pub name: String,
        pub host_path: String,
        pub default_harness: Option<protocol::capability::HarnessKind>,
        pub default_model: Option<String>,
        pub default_effort: Option<String>,
        pub active_writer: Option<RunId>,
        pub vcs: Option<protocol::model::VcsInfo>,
        pub created_at: Timestamp,
        #[serde(default)]
        pub stage: Option<protocol::project::ProjectStageTransition>,
        #[serde(default)]
        pub one_off: bool,
        #[serde(default)]
        pub home: bool,
    }
    /// 0.2's task shape, without the new chat origin.
    #[derive(Debug, Deserialize)]
    pub struct Task {
        pub task_id: TaskId,
        pub project_id: ProjectId,
        pub kind: protocol::model::TaskKind,
        #[serde(default)]
        pub mode: protocol::model::TaskMode,
        pub title: String,
        pub state: protocol::model::TaskState,
        pub revision: u64,
        pub harness: protocol::capability::HarnessKind,
        pub model: Option<String>,
        pub effort: Option<String>,
        #[serde(default)]
        pub launch_policy: Option<protocol::model::LaunchPolicy>,
        #[serde(default)]
        pub branch: Option<protocol::model::TaskBranch>,
        #[serde(default)]
        pub start: Option<protocol::launchers::StartedFrom>,
        #[serde(default)]
        pub browser: bool,
        #[serde(default)]
        pub conversation: bool,
        #[serde(default)]
        pub access: Option<protocol::access::ConversationAccess>,
        #[serde(default)]
        pub access_blocked: Option<Box<protocol::access::AccessBlocked>>,
        #[serde(default)]
        pub github: Option<Box<protocol::github::TaskGithub>>,
        #[serde(default)]
        pub review: Option<Box<protocol::review::TaskReview>>,
        #[serde(default)]
        pub wake: Option<protocol::model::TaskWake>,
        #[serde(default)]
        pub asked_by: Option<TaskId>,
        #[serde(default)]
        pub report: Option<protocol::model::TaskReport>,
        pub created_by: protocol::auth::Principal,
        pub created_at: Timestamp,
        pub updated_at: Timestamp,
        pub current_run: Option<RunId>,
        pub target_id: Option<protocol::TargetId>,
        pub mission: Option<protocol::model::Mission>,
        pub attention_count: u32,
        pub status_line: Option<String>,
        #[serde(default)]
        pub notify_done: bool,
    }
    #[derive(Debug, Deserialize)]
    pub struct RunReport {
        pub status: protocol::model::ReportStatus,
        #[serde(default)]
        pub questions: Vec<String>,
        #[serde(default)]
        pub remaining: Vec<String>,
        #[serde(default)]
        pub new_work: Vec<NewWork>,
    }
    #[derive(Debug, Deserialize)]
    pub struct NewWork {
        pub title: String,
        pub brief: String,
        pub mode: protocol::model::TaskMode,
        #[serde(default)]
        pub issue: Option<u64>,
    }

    #[derive(Debug, Deserialize)]
    #[serde(rename_all = "snake_case")]
    pub enum AlertType {
        Question,
        Approval,
        SignIn,
        Decision,
        Failed,
        PrNeedsYou,
        ReportQuestion,
        GoAhead,
        Reply,
        PrReady,
        Batch,
        IssueCall,
        Reports,
        AppUpdate,
        Finished,
        Merged,
        Issues,
        Slow,
    }
    #[derive(Debug, Deserialize)]
    #[serde(rename_all = "snake_case")]
    pub enum Tier {
        Off,
        Silent,
        Alert,
    }
    #[derive(Debug, Deserialize)]
    #[serde(rename_all = "snake_case")]
    pub enum AlertTopic {
        Question,
        Approval,
        NeedsYou,
        Finished,
        Failed,
        PrReady,
        PrNeedsYou,
        Merged,
        Issues,
        SlowWork,
        Reports,
    }
    #[derive(Debug, Deserialize)]
    pub struct TypeTier {
        #[serde(rename = "type")]
        pub alert_type: AlertType,
        pub tier: Tier,
    }
    #[derive(Debug, Deserialize)]
    pub struct NotificationSettings {
        #[serde(default)]
        pub off: Vec<AlertTopic>,
        #[serde(default)]
        pub bundle: bool,
        #[serde(default)]
        pub projects: Vec<ProjectNotifications>,
        #[serde(default)]
        pub tiers: Vec<TypeTier>,
        #[serde(default)]
        pub revision: u64,
    }
    #[derive(Debug, Deserialize)]
    pub struct ProjectNotifications {
        pub project_id: ProjectId,
        #[serde(default)]
        pub mute: bool,
        #[serde(default)]
        pub on: Vec<AlertTopic>,
        #[serde(default)]
        pub off: Vec<AlertTopic>,
        #[serde(default)]
        pub bundle: Option<bool>,
        #[serde(default)]
        pub tiers: Vec<TypeTier>,
    }
    #[derive(Debug, Deserialize)]
    pub struct AttentionItem {
        #[serde(default)]
        pub child_id: Option<ChildId>,
        pub attention_id: AttentionId,
        pub kind: AttentionKind,
        pub title: String,
        pub body: Option<String>,
        pub task_id: Option<TaskId>,
        pub run_id: Option<RunId>,
        pub approval_id: Option<ApprovalId>,
        pub created_at: Timestamp,
        pub resolved_at: Option<Timestamp>,
        pub priority: u8,
        #[serde(default)]
        pub topic: Option<AlertTopic>,
        #[serde(default)]
        pub quiet: bool,
        #[serde(default)]
        pub report_id: Option<ReportId>,
        #[serde(default)]
        pub account_id: Option<String>,
        #[serde(default)]
        pub alert: Option<AlertType>,
        #[serde(default)]
        pub alert_handled_at: Option<Timestamp>,
    }
    /// 0.2's host record. Its capabilities are as this version's, which
    /// this change leaves alone.
    #[derive(Debug, Deserialize)]
    pub struct HostInfo {
        #[serde(default)]
        pub inbox_path: Option<String>,
        pub host_id: protocol::HostId,
        pub name: String,
        #[serde(default)]
        pub renamed: bool,
        pub os_label: String,
        pub companion_version: String,
        pub boot_epoch: protocol::BootEpoch,
        pub started_at: Timestamp,
        pub capabilities: protocol::capability::HostCapabilities,
    }
    /// Every event 0.2 journals, by its wire name.
    pub const EVENTS: &[&str] = &[
        "host_updated",
        "capabilities_changed",
        "harnesses_changed",
        "project_upserted",
        "project_removed",
        "task_upserted",
        "run_upserted",
        "timeline_appended",
        "command_progress",
        "approval_upserted",
        "attention_upserted",
        "attention_resolved",
        "targets_changed",
        "control_changed",
        "intervention_recorded",
        "queue_changed",
        "library_roots_changed",
        "library_file_changed",
        "terminals_changed",
        "transfer_upserted",
        "previews_changed",
        "checklist_changed",
        "artifact_created",
        "artifact_deleted",
        "devices_changed",
        "channel_identities_changed",
        "readiness_reported",
        "accounts_changed",
        "agent_logins_changed",
        "notification_settings_changed",
        "reports_changed",
        "agent_login_used",
        "github_changed",
        "pr_changed",
        "boot_epoch_changed",
    ];
}

#[derive(Debug, Default)]
struct Releases(std::sync::Mutex<String>);
#[async_trait::async_trait]
impl ReleaseSource for Releases {
    async fn latest(&self) -> Result<Option<LatestRelease>, String> {
        let tag = self.0.lock().unwrap().clone();
        Ok(Some(LatestRelease {
            url: format!("https://github.com/Ophio-app/ophio/releases/tag/{tag}"),
            tag,
        }))
    }
}

/// Reads everything 0.2 reads of what this version stored, as 0.2 does.
pub(super) fn readable_by_0_2(f: &Fixture, after: &str) {
    let strict = |what: &str, result: Result<(), serde_json::Error>| {
        if let Err(e) = result {
            panic!("after {after}, 0.2 can't read {what}: {e}");
        }
    };
    let (settings, attention, host, projects, tasks, journal) = f
        .hub
        .read(|r| {
            Ok((
                r.get::<serde_json::Value>("meta", "notification_settings")?,
                r.list::<serde_json::Value>("attention")?,
                r.get::<serde_json::Value>("meta", "host")?,
                r.list::<serde_json::Value>("project")?,
                r.list::<serde_json::Value>("task")?,
                r.journal(0)?,
            ))
        })
        .unwrap();
    if let Some(settings) = settings {
        strict(
            "the notification settings",
            serde_json::from_value::<v0_2::NotificationSettings>(settings).map(drop),
        );
    }
    for item in attention {
        strict(
            "an attention item",
            serde_json::from_value::<v0_2::AttentionItem>(item).map(drop),
        );
    }
    if let Some(host) = host {
        strict(
            "the host record",
            serde_json::from_value::<v0_2::HostInfo>(host).map(drop),
        );
    }
    for project in projects {
        strict(
            "a project",
            serde_json::from_value::<v0_2::Project>(project).map(drop),
        );
    }
    for task in tasks {
        strict(
            "a task",
            serde_json::from_value::<v0_2::Task>(task).map(drop),
        );
    }
    for envelope in journal.events {
        let event = serde_json::to_value(&envelope.event).unwrap();
        let kind = event["type"].as_str().unwrap_or_default().to_owned();
        assert!(
            v0_2::EVENTS.contains(&kind.as_str()),
            "after {after}, the journal holds a {kind} event 0.2 doesn't know"
        );
        match kind.as_str() {
            "attention_upserted" => strict(
                "a journalled attention item",
                serde_json::from_value::<v0_2::AttentionItem>(event["item"].clone()).map(drop),
            ),
            "host_updated" => strict(
                "a journalled host record",
                serde_json::from_value::<v0_2::HostInfo>(event["host"].clone()).map(drop),
            ),
            "project_upserted" => strict(
                "a journalled project",
                serde_json::from_value::<v0_2::Project>(event["project"].clone()).map(drop),
            ),
            "task_upserted" => strict(
                "a journalled task",
                serde_json::from_value::<v0_2::Task>(event["task"].clone()).map(drop),
            ),
            "run_upserted" => {
                if let Some(report) = event["run"]["outcome"]["report"].as_object() {
                    strict(
                        "a journalled run report",
                        serde_json::from_value::<v0_2::RunReport>(serde_json::Value::Object(
                            report.clone(),
                        ))
                        .map(drop),
                    );
                }
            }
            _ => {}
        }
    }
}

async fn settings_now(
    f: &Fixture,
    caller: &Caller,
) -> protocol::notifications::NotificationSettings {
    let QueryData::NotificationSettings { view } = f
        .hub
        .query(caller, Query::NotificationSettings)
        .await
        .unwrap()
    else {
        panic!("notification settings")
    };
    view
}

async fn save(
    f: &Fixture,
    caller: &Caller,
    off: Vec<AlertTopic>,
    projects: Vec<ProjectNotifications>,
    types: bool,
) {
    let revision = settings_now(f, caller).await.revision;
    let mut command = CommandEnvelope::new(Command::SetNotificationSettings {
        off,
        bundle: true,
        projects,
        types,
    });
    command.expected_revision = Some(revision);
    let receipt = f.hub.submit(caller, command).await;
    assert_eq!(receipt.error, None);
}

#[tokio::test]
async fn version_0_2_reads_everything_this_version_stores_after_any_settings_change() {
    let source = Arc::new(Releases::default());
    *source.0.lock().unwrap() = "v0.3.0".into();
    let check = crate::ReleaseCheck {
        source: source.clone(),
        on_by_default: true,
        installed: "0.2.0".into(),
    };
    let f = Fixture::with(move |b| b.release_check(check)).await;
    let phone = f.device().await;
    let project = f.project().await;
    readable_by_0_2(&f, "starting");

    // A phone choosing every tier for every type, this version's new one
    // included, in a project; then muting it; then an older phone's save,
    // which brings topics over as types; then nothing at all.
    for tier in [Tier::Alert, Tier::Silent, Tier::Off] {
        let tiers = AlertType::ALL
            .iter()
            .map(|&alert_type| TypeTier { alert_type, tier })
            .collect();
        let chosen = ProjectNotifications {
            tiers,
            ..ProjectNotifications::new(project)
        };
        save(&f, &phone, Vec::new(), vec![chosen], true).await;
        readable_by_0_2(&f, &format!("choosing {tier:?} for every type"));
        // An older phone reads the same settings.
        let view = serde_json::to_value(settings_now(&f, &phone).await).unwrap();
        serde_json::from_value::<v0_2::NotificationSettings>(view).unwrap();
    }
    let muted = ProjectNotifications {
        mute: true,
        bundle: Some(false),
        tiers: vec![TypeTier {
            alert_type: AlertType::NewVersion,
            tier: Tier::Alert,
        }],
        ..ProjectNotifications::new(project)
    };
    save(&f, &phone, Vec::new(), vec![muted], true).await;
    readable_by_0_2(&f, "muting a project");
    let older = ProjectNotifications {
        on: vec![AlertTopic::Finished],
        off: vec![AlertTopic::Question],
        ..ProjectNotifications::new(project)
    };
    save(&f, &phone, AlertTopic::ALL.to_vec(), vec![older], false).await;
    readable_by_0_2(&f, "an older phone's save");
    save(&f, &phone, Vec::new(), Vec::new(), true).await;
    readable_by_0_2(&f, "clearing every choice");

    // The check itself: a notice raised, handled, replaced, and the check
    // turned off and on.
    f.hub.check_release().await.unwrap();
    let notice = (f
        .hub
        .read(|r| r.list::<protocol::model::AttentionItem>("attention")))
    .unwrap()
    .into_iter()
    .find(|a| a.new_version.is_some())
    .expect("a notice");
    f.hub
        .mark_alerts_handled(&phone, &[notice.attention_id])
        .unwrap();
    *source.0.lock().unwrap() = "v0.4.0".into();
    f.hub.check_release().await.unwrap();
    readable_by_0_2(&f, "new versions found");
    for (enabled, revision) in [(false, 0), (true, 1)] {
        let mut command = CommandEnvelope::new(Command::SetReleaseCheck { enabled });
        command.expected_revision = Some(revision);
        assert_eq!(f.hub.submit(&phone, command).await.error, None);
    }
    f.hub.check_release().await.unwrap();
    readable_by_0_2(&f, "turning the check off and on");
    f.close().await;
}

#[test]
fn version_0_2_reads_suggested_work_with_needs_and_refusal() {
    let report = protocol::model::RunReport {
        status: protocol::model::ReportStatus::Done,
        questions: Vec::new(),
        remaining: Vec::new(),
        new_work: vec![protocol::model::NewWork {
            title: "A task".into(),
            brief: "Do it".into(),
            mode: protocol::model::TaskMode::Work,
            issue: None,
            needs: Some(protocol::work_cards::WorkNeeds {
                read: vec!["Another project".into()],
                internet: true,
                ..Default::default()
            }),
            refused: Some("This cannot start as written.".into()),
            after: None,
            all_issues: false,
        }],
    };
    let value = serde_json::to_value(report).unwrap();
    let old: v0_2::RunReport = serde_json::from_value(value).unwrap();
    assert_eq!(old.new_work.len(), 1);
}

#[tokio::test]
async fn version_0_2_reads_project_and_task_records_with_new_optional_fields() {
    let f = Fixture::new().await;
    let project_id = f.project().await;
    let owner = Caller::local_owner();
    let access = protocol::access::ConversationAccess {
        write: vec![project_id],
        ..Default::default()
    };
    let r = f
        .hub
        .submit(
            &owner,
            CommandEnvelope::new(Command::SetChatAccessDefault {
                project_id,
                access: Some(access),
            }),
        )
        .await;
    assert_eq!(r.error, None, "{r:?}");
    let (task_id, _) = f
        .task(project_id, TaskPlacement::StartNow, &owner, false)
        .await;
    let task: Task = f.hub.read(|r| r.require("task", task_id)).unwrap();
    assert_eq!(task.origin, None);
    readable_by_0_2(&f, "saving a chat access default and a task");
    let r = f
        .hub
        .submit(
            &owner,
            CommandEnvelope::new(Command::SetChatAccessDefault {
                project_id,
                access: None,
            }),
        )
        .await;
    assert_eq!(r.error, None, "{r:?}");
    readable_by_0_2(&f, "clearing a chat access default");
    f.close().await;
}

#[tokio::test]
async fn version_0_2_reads_a_task_turned_from_chat() {
    let f = Fixture::new().await;
    let project = f.project().await;
    let owner = Caller::local_owner();
    let (chat_id, _) = f
        .task(project, TaskPlacement::StartNow, &owner, false)
        .await;
    let (task_id, _) = f
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
    f.hub
        .write(|tx| {
            let mut task: Task = tx.read().require("task", task_id)?;
            task.from_chat = Some(chat_id);
            tx.put("task", task_id, &task)
        })
        .unwrap();
    let stored: serde_json::Value = f.hub.read(|r| r.require("task", task_id)).unwrap();
    assert_eq!(stored["from_chat"], serde_json::json!(chat_id));
    let old: v0_2::Task = serde_json::from_value(stored).unwrap();
    assert_eq!(old.task_id, task_id);
    f.close().await;
}

/// The test above would pass if this version never wrote its new type at
/// all; this shows a strict 0.2 reader does refuse one, so it proves
/// something.
#[test]
fn a_strict_0_2_reader_refuses_a_type_it_doesnt_know() {
    let text = serde_json::json!({
        "tiers": [{ "type": "new_version", "tier": "off" }],
    });
    assert!(serde_json::from_value::<v0_2::NotificationSettings>(text).is_err());
}
