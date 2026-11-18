// SPDX-License-Identifier: Apache-2.0
//! Which alerts reach the person's phone: a default for every project, and
//! each project's own choices over it.
//!
//! Alerts the person turned off still count as raised on a phone that says
//! it knows them (see [`crate::alerts::Alert::quiet`]), so turning a topic
//! back on never raises the ones it held back.
use crate::{ProjectId, model::AttentionKind};
use serde::{Deserialize, Serialize};

/// The most projects with choices of their own.
pub const MAX_PROJECT_NOTIFICATIONS: usize = 1000;

/// The computer's word that it labels each alert with its [`AlertType`],
/// keeps each project's [`Tier`] for a type, takes the phone's word that an
/// alert was handled, and takes `SetTaskNotify`.
pub const ALERT_TYPES_FEATURE: &str = "alert_types_v1";

/// What a notification is about, in the words the person chooses by. Each
/// has its own Android notification category and its own choice of
/// [`Tier`] on each phone, and each project may choose its own.
///
/// By default, what waits on the person alerts, as do replies in a chat
/// they aren't viewing. Other things worth a look show silently, and
/// routine progress waits in the app.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
    ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum AlertType {
    /// An agent asked something and waits for the answer.
    Question,
    /// An agent waits for leave to do something.
    Approval,
    /// An agent can't work: it isn't signed in on the computer.
    SignIn,
    /// A task waits for a decision: a conflict, an end Ophio can't confirm,
    /// the screen to watch, or the agent waiting on the person.
    Decision,
    /// A task, or a review, stopped with a problem.
    Failed,
    /// A pull request is stuck on the person: checks fail, a conflict, a
    /// review asks for changes, or some were left out of a merge.
    PrNeedsYou,
    /// A problem report the person sent has a question for them.
    ReportQuestion,
    /// A task in Next up waits for the go-ahead to start.
    GoAhead,
    /// An agent replied in a chat.
    Reply,
    /// A pull request passed its checks and waits to be merged.
    PrReady,
    /// What a Run all came to, as a whole.
    Batch,
    /// An issue waits for a call: it couldn't be reproduced, or looks
    /// fixed already.
    IssueCall,
    /// A problem report moved on, or someone sent one.
    Reports,
    /// A computer offers a newer Øphio for this phone. The phone's own.
    AppUpdate,
    /// A newer Øphio is out for a computer, to update it with.
    NewVersion,
    /// Agent updates and newly available models on a computer.
    AgentUpdate,
    /// A task finished its work.
    Finished,
    /// A pull request merged.
    Merged,
    /// Issues were found and filed, or closed as duplicates.
    Issues,
    /// A step is taking long.
    Slow,
}

/// How a notification of a type shows.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
    ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum Tier {
    /// No notification: it waits in the app.
    Off,
    /// In the shade, with no sound, vibration or pop-up.
    Silent,
    /// Sound, vibration and a pop-up.
    Alert,
}

/// Where Settings lists a type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TypeGroup {
    NeedsYou,
    WorthALook,
    InTheApp,
}

/// A type as Settings and Android's categories name it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct TypeInfo {
    #[serde(rename = "type")]
    pub alert_type: AlertType,
    pub name: &'static str,
    pub description: &'static str,
    pub group: TypeGroup,
    /// The recommended choice.
    pub tier: Tier,
    /// A project may choose its own for it: its notifications belong to a
    /// project's work.
    pub project: bool,
}

impl AlertType {
    /// Every type, in the order Settings lists them.
    pub const ALL: [AlertType; 20] = [
        AlertType::Question,
        AlertType::Approval,
        AlertType::SignIn,
        AlertType::Decision,
        AlertType::Failed,
        AlertType::PrNeedsYou,
        AlertType::ReportQuestion,
        AlertType::GoAhead,
        AlertType::Reply,
        AlertType::PrReady,
        AlertType::Batch,
        AlertType::IssueCall,
        AlertType::Reports,
        AlertType::AppUpdate,
        AlertType::NewVersion,
        AlertType::AgentUpdate,
        AlertType::Finished,
        AlertType::Merged,
        AlertType::Issues,
        AlertType::Slow,
    ];

    /// Its words in Settings and Android's categories, and its
    /// recommended choice.
    pub fn info(self) -> TypeInfo {
        use AlertType::*;
        use Tier::*;
        use TypeGroup::*;
        let (name, description, group, tier) = match self {
            Question => (
                "Questions",
                "An agent asked you something and is waiting for your answer.",
                NeedsYou,
                Alert,
            ),
            Approval => (
                "Approvals",
                "An agent wants your OK before it does something.",
                NeedsYou,
                Alert,
            ),
            SignIn => (
                "Agent signed out",
                "An agent can’t work because it isn’t signed in on a computer.",
                NeedsYou,
                Alert,
            ),
            Decision => (
                "Tasks that need a decision",
                "A conflict, an end Øphio can’t confirm, or a screen to watch.",
                NeedsYou,
                Alert,
            ),
            Failed => (
                "Tasks that stopped with a problem",
                "A task or a review couldn’t finish.",
                NeedsYou,
                Alert,
            ),
            PrNeedsYou => (
                "Pull requests that need you",
                "Checks fail, a conflict, or a review asks for changes.",
                NeedsYou,
                Alert,
            ),
            ReportQuestion => (
                "Questions about your reports",
                "A problem report you sent has a question for you.",
                NeedsYou,
                Alert,
            ),
            GoAhead => (
                "Next up go-aheads",
                "A task in Next up is waiting for your go-ahead to start.",
                WorthALook,
                Silent,
            ),
            Reply => (
                "Chat replies",
                "An agent replied in a chat you aren’t viewing.",
                WorthALook,
                Alert,
            ),
            PrReady => (
                "Pull requests ready to merge",
                "Checks passed and it’s waiting for you to merge it.",
                WorthALook,
                Silent,
            ),
            Batch => (
                "Run all results",
                "What a Run all came to, once it’s through.",
                WorthALook,
                Silent,
            ),
            IssueCall => (
                "Issues that need your call",
                "An issue couldn’t be reproduced, or looks fixed already.",
                WorthALook,
                Silent,
            ),
            Reports => (
                "Problem report updates",
                "A report you sent moved on, or someone sent you one.",
                WorthALook,
                Silent,
            ),
            AppUpdate => (
                "Øphio updates",
                "A computer has a newer Øphio for this phone.",
                WorthALook,
                Silent,
            ),
            NewVersion => (
                "New Øphio versions",
                "A newer Øphio is out for one of your computers.",
                WorthALook,
                Silent,
            ),
            AgentUpdate => (
                "Agent updates and new models",
                "Claude Code or Codex has an update or new models on a computer.",
                WorthALook,
                Silent,
            ),
            Finished => (
                "Finished tasks",
                "A task finished. Turn on “Tell me when it’s done” on a task to hear about that one.",
                InTheApp,
                Off,
            ),
            Merged => (
                "Merged pull requests",
                "A pull request merged.",
                InTheApp,
                Off,
            ),
            Issues => (
                "Issues filed",
                "Issues were found and filed, or closed as duplicates.",
                InTheApp,
                Off,
            ),
            Slow => (
                "Slow steps",
                "A step is taking longer than usual.",
                InTheApp,
                Off,
            ),
        };
        TypeInfo {
            alert_type: self,
            name,
            description,
            group,
            tier,
            project: self.in_projects(),
        }
    }

    /// Whether a project may choose for this type. A newer Øphio for the
    /// phone or the computer belongs to no project; a choice for one is
    /// never kept, which also keeps a type older versions don't know out of
    /// the settings they read.
    pub fn in_projects(self) -> bool {
        !matches!(
            self,
            AlertType::AppUpdate | AlertType::NewVersion | AlertType::AgentUpdate
        )
    }

    /// The recommended choice for this type.
    pub fn default_tier(self) -> Tier {
        self.info().tier
    }

    /// The type of an item, as the computer labels it: the one it was
    /// raised with, or the one its kind, topic and task say. `conversation`
    /// is whether its task is a chat. None for an item that never alerts.
    pub fn of(item: &crate::model::AttentionItem, conversation: bool) -> Option<AlertType> {
        if let Some(t) = item.alert {
            return Some(t);
        }
        if item.agent_update.is_some() && item.task_id.is_none() {
            return Some(AlertType::AgentUpdate);
        }
        if item.new_version.is_some() && item.task_id.is_none() {
            return Some(AlertType::NewVersion);
        }
        if item.account_id.is_some() && item.task_id.is_none() {
            return Some(AlertType::SignIn);
        }
        if item.report_id.is_some() {
            if item.kind == AttentionKind::Question {
                return Some(AlertType::ReportQuestion);
            }
            if item.task_id.is_none() {
                return Some(AlertType::Reports);
            }
        }
        let topic = item.topic;
        Some(match item.kind {
            AttentionKind::ConnectionIssue => return None,
            AttentionKind::Question => AlertType::Question,
            AttentionKind::Approval => AlertType::Approval,
            AttentionKind::Conflict | AttentionKind::ReconciliationRequired => AlertType::Decision,
            AttentionKind::Failure => AlertType::Failed,
            AttentionKind::Completed => match topic {
                Some(AlertTopic::Merged) => AlertType::Merged,
                Some(AlertTopic::PrReady) => AlertType::PrReady,
                Some(AlertTopic::Issues) => AlertType::Issues,
                Some(AlertTopic::Reports) => AlertType::Reports,
                Some(AlertTopic::PrNeedsYou) => AlertType::PrNeedsYou,
                _ if conversation => AlertType::Reply,
                _ => AlertType::Finished,
            },
            AttentionKind::WaitingForYou => match topic {
                Some(AlertTopic::PrNeedsYou) => AlertType::PrNeedsYou,
                Some(AlertTopic::Issues) => AlertType::IssueCall,
                Some(AlertTopic::SlowWork) => AlertType::Slow,
                // A review that couldn't check or file its findings.
                Some(AlertTopic::Finished) | Some(AlertTopic::Failed) => AlertType::Failed,
                Some(AlertTopic::Reports) => AlertType::Reports,
                Some(AlertTopic::PrReady) => AlertType::PrReady,
                Some(AlertTopic::Merged) => AlertType::Merged,
                _ => AlertType::Decision,
            },
        })
    }

    /// Whether a task's "Tell me when it's done" makes this type alert.
    pub fn follows_task(self) -> bool {
        matches!(
            self,
            AlertType::Finished | AlertType::Failed | AlertType::Reply
        )
    }
}

/// A project's own choice for one type.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[ts(export)]
pub struct TypeTier {
    #[serde(rename = "type")]
    pub alert_type: AlertType,
    pub tier: Tier,
}

/// Choices by type, leaving out any for a type or tier this version doesn't
/// know: a newer version may have kept one, and the rest still holds. Read
/// strictly, one unknown type would make the whole of the settings, and so
/// the computer's start after going back to this version, fail.
fn known_tiers<'de, D>(deserializer: D) -> Result<Vec<TypeTier>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let values = Vec::<serde_json::Value>::deserialize(deserializer)?;
    Ok(values
        .into_iter()
        .filter_map(|v| serde_json::from_value(v).ok())
        .collect())
}

/// What an alert is about. Each can be turned off, everywhere or in one
/// project.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
    ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum AlertTopic {
    /// An agent asks the person a question.
    Question,
    /// An agent waits for leave to do something.
    Approval,
    /// A task waits for the person: to go ahead, to settle a conflict, or to
    /// say how it ended.
    NeedsYou,
    /// A task finished.
    Finished,
    /// A task stopped with a problem.
    Failed,
    /// A pull request is ready to merge.
    PrReady,
    /// A pull request needs the person: a check failed, a conflict, a review.
    PrNeedsYou,
    /// A pull request merged.
    Merged,
    /// Issues found and filed, closed as duplicates, or not reproduced.
    Issues,
    /// A step is taking long, or work stopped for showing no sign of progress.
    SlowWork,
    /// A problem report the person sent was reproduced, fixed or shipped, or
    /// has a question for them.
    Reports,
}

impl AlertTopic {
    pub const ALL: [AlertTopic; 11] = [
        AlertTopic::Question,
        AlertTopic::Approval,
        AlertTopic::NeedsYou,
        AlertTopic::Finished,
        AlertTopic::Failed,
        AlertTopic::PrReady,
        AlertTopic::PrNeedsYou,
        AlertTopic::Merged,
        AlertTopic::Issues,
        AlertTopic::SlowWork,
        AlertTopic::Reports,
    ];

    /// The topic of an item of `kind` that names none of its own; none for a
    /// kind that raises no alert.
    pub fn of(kind: AttentionKind) -> Option<AlertTopic> {
        Some(match kind {
            AttentionKind::Question => AlertTopic::Question,
            AttentionKind::Approval => AlertTopic::Approval,
            AttentionKind::WaitingForYou
            | AttentionKind::Conflict
            | AttentionKind::ReconciliationRequired => AlertTopic::NeedsYou,
            AttentionKind::Completed => AlertTopic::Finished,
            AttentionKind::Failure => AlertTopic::Failed,
            AttentionKind::ConnectionIssue => return None,
        })
    }
}

/// Which alerts reach the person, on every phone paired with the computer.
#[derive(
    Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[ts(export)]
pub struct NotificationSettings {
    /// Topics that alert in no project unless the project turns them on.
    #[serde(default)]
    pub off: Vec<AlertTopic>,
    /// One alert per project: once a second would show, the project's
    /// alerts fold into one that counts them.
    #[serde(default)]
    pub bundle: bool,
    /// Projects with choices of their own.
    #[serde(default)]
    pub projects: Vec<ProjectNotifications>,
    /// Choices by type for every project on this computer, over each
    /// phone's, that a project without its own choice for the type
    /// follows, new projects included. Only ever brought over from a topic
    /// turned off for every project before types existed, and only
    /// stricter: phones don't set it, and saving keeps it.
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "known_tiers"
    )]
    #[ts(as = "Option<Vec<TypeTier>>", optional)]
    pub tiers: Vec<TypeTier>,
    #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
    #[serde(default)]
    pub revision: u64,
}

/// One project's choices, over the default for every project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct ProjectNotifications {
    pub project_id: ProjectId,
    /// No alerts from this project at all.
    #[serde(default)]
    pub mute: bool,
    /// Topics this project alerts for though the default is off.
    #[serde(default)]
    pub on: Vec<AlertTopic>,
    /// Topics this project doesn't alert for though the default is on.
    #[serde(default)]
    pub off: Vec<AlertTopic>,
    /// One alert for this project, or each on its own; none follows the
    /// default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub bundle: Option<bool>,
    /// The project's own choice for a type, over each phone's. Older
    /// computers keep none.
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "known_tiers"
    )]
    #[ts(as = "Option<Vec<TypeTier>>", optional)]
    pub tiers: Vec<TypeTier>,
}

impl ProjectNotifications {
    pub fn new(project_id: ProjectId) -> Self {
        Self {
            project_id,
            mute: false,
            on: Vec::new(),
            off: Vec::new(),
            bundle: None,
            tiers: Vec::new(),
        }
    }
    /// Says nothing the default doesn't.
    pub fn is_empty(&self) -> bool {
        !self.mute
            && self.on.is_empty()
            && self.off.is_empty()
            && self.bundle.is_none()
            && self.tiers.is_empty()
    }
    /// The project's own choice for `alert_type`, if it made one.
    pub fn tier(&self, alert_type: AlertType) -> Option<Tier> {
        (self.tiers.iter())
            .find(|t| t.alert_type == alert_type)
            .map(|t| t.tier)
    }
}

impl NotificationSettings {
    pub fn project(&self, project_id: ProjectId) -> Option<&ProjectNotifications> {
        self.projects.iter().find(|p| p.project_id == project_id)
    }
    /// Whether an alert about `topic` in `project_id` reaches the person.
    pub fn alerts(&self, project_id: ProjectId, topic: AlertTopic) -> bool {
        match self.project(project_id) {
            Some(p) if p.mute || p.off.contains(&topic) => false,
            Some(p) if p.on.contains(&topic) => true,
            _ => !self.off.contains(&topic),
        }
    }
    /// The project's own choice for a type: `Off` when it is muted.
    pub fn project_tier(&self, project_id: ProjectId, alert_type: AlertType) -> Option<Tier> {
        let p = self.project(project_id);
        if p.is_some_and(|p| p.mute) {
            return Some(Tier::Off);
        }
        (p.and_then(|p| p.tier(alert_type))).or_else(|| self.computer_tier(alert_type))
    }
    /// This computer's choice for a type in every project (see
    /// [`Self::tiers`]).
    pub fn computer_tier(&self, alert_type: AlertType) -> Option<Tier> {
        (self.tiers.iter())
            .find(|t| t.alert_type == alert_type)
            .map(|t| t.tier)
    }
    /// Whether `project_id`'s alerts fold into one.
    pub fn bundles(&self, project_id: ProjectId) -> bool {
        self.project(project_id)
            .and_then(|p| p.bundle)
            .unwrap_or(self.bundle)
    }
    /// The same choices, each said once, without projects that say nothing
    /// of their own; `None` when there are too many projects.
    pub fn tidy(mut self) -> Option<Self> {
        fn once(topics: &mut Vec<AlertTopic>) {
            topics.sort();
            topics.dedup();
        }
        once(&mut self.off);
        let mut tiers: Vec<TypeTier> = Vec::new();
        for t in (self.tiers.drain(..).rev()).filter(|t| t.alert_type.in_projects()) {
            if !tiers.iter().any(|k| k.alert_type == t.alert_type) {
                tiers.push(t);
            }
        }
        tiers.sort_by_key(|t| t.alert_type);
        self.tiers = tiers;
        let mut projects: Vec<ProjectNotifications> = Vec::new();
        for mut p in self.projects {
            once(&mut p.on);
            once(&mut p.off);
            p.on.retain(|t| !p.off.contains(t));
            // Each type once, the last choice for it kept, in type order.
            let mut tiers: Vec<TypeTier> = Vec::new();
            for t in (p.tiers.drain(..).rev()).filter(|t| t.alert_type.in_projects()) {
                if !tiers.iter().any(|k| k.alert_type == t.alert_type) {
                    tiers.push(t);
                }
            }
            tiers.sort_by_key(|t| t.alert_type);
            p.tiers = tiers;
            if p.is_empty() {
                continue;
            }
            match projects.iter_mut().find(|q| q.project_id == p.project_id) {
                Some(q) => *q = p,
                None => projects.push(p),
            }
        }
        if projects.len() > MAX_PROJECT_NOTIFICATIONS {
            return None;
        }
        self.projects = projects;
        Some(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_project_choice_wins_over_the_default() {
        let a = ProjectId::new();
        let b = ProjectId::new();
        let settings = NotificationSettings {
            off: vec![AlertTopic::Finished],
            bundle: true,
            projects: vec![
                ProjectNotifications {
                    on: vec![AlertTopic::Finished],
                    off: vec![AlertTopic::Question],
                    bundle: Some(false),
                    ..ProjectNotifications::new(a)
                },
                ProjectNotifications {
                    mute: true,
                    ..ProjectNotifications::new(b)
                },
            ],
            tiers: Vec::new(),
            revision: 0,
        };
        let other = ProjectId::new();
        assert!(!settings.alerts(other, AlertTopic::Finished));
        assert!(settings.alerts(other, AlertTopic::Question));
        assert!(settings.alerts(a, AlertTopic::Finished));
        assert!(!settings.alerts(a, AlertTopic::Question));
        assert!(settings.alerts(a, AlertTopic::Merged));
        assert!(AlertTopic::ALL.iter().all(|t| !settings.alerts(b, *t)));
        assert!(settings.bundles(other) && settings.bundles(b));
        assert!(!settings.bundles(a));
    }

    #[test]
    fn tidy_says_each_choice_once() {
        let a = ProjectId::new();
        let settings = NotificationSettings {
            off: vec![AlertTopic::Merged, AlertTopic::Finished, AlertTopic::Merged],
            projects: vec![
                ProjectNotifications::new(ProjectId::new()),
                ProjectNotifications {
                    on: vec![AlertTopic::Merged, AlertTopic::Finished],
                    off: vec![AlertTopic::Finished],
                    ..ProjectNotifications::new(a)
                },
            ],
            ..NotificationSettings::default()
        }
        .tidy()
        .unwrap();
        assert_eq!(settings.off, [AlertTopic::Finished, AlertTopic::Merged]);
        assert_eq!(settings.projects.len(), 1);
        assert_eq!(settings.projects[0].on, [AlertTopic::Merged]);
        assert_eq!(settings.projects[0].off, [AlertTopic::Finished]);
        let many = NotificationSettings {
            projects: (0..=MAX_PROJECT_NOTIFICATIONS)
                .map(|_| ProjectNotifications {
                    mute: true,
                    ..ProjectNotifications::new(ProjectId::new())
                })
                .collect(),
            ..NotificationSettings::default()
        };
        assert!(many.tidy().is_none());
    }

    #[test]
    fn every_type_is_listed_once_with_words_and_a_choice() {
        let mut seen = std::collections::BTreeSet::new();
        for t in AlertType::ALL {
            assert!(seen.insert(t));
            let info = t.info();
            assert!(!info.name.is_empty() && info.description.ends_with('.'));
        }
        // Replies also alert unless the person is viewing that exact chat.
        assert!(
            (AlertType::ALL.iter())
                .filter(|t| t.default_tier() == Tier::Alert)
                .all(|t| t.info().group == TypeGroup::NeedsYou || *t == AlertType::Reply)
        );
        assert_eq!(AlertType::Finished.default_tier(), Tier::Off);
        assert_eq!(AlertType::GoAhead.default_tier(), Tier::Silent);
        assert_eq!(AlertType::Reply.default_tier(), Tier::Alert);
        assert_eq!(AlertType::AgentUpdate.default_tier(), Tier::Silent);
        assert!(!AlertType::AgentUpdate.info().project);
        assert_eq!(
            serde_json::to_string(&AlertType::AgentUpdate).unwrap(),
            r#""agent_update""#
        );
        assert_eq!(AlertType::NewVersion.default_tier(), Tier::Silent);
        let text = serde_json::to_string(&AlertType::NewVersion).unwrap();
        assert_eq!(text, r#""new_version""#);
        let text = serde_json::to_string(&AlertType::PrNeedsYou).unwrap();
        assert_eq!(text, r#""pr_needs_you""#);
    }

    /// The app's Settings reads every type's words from this file, so they
    /// are said in one place. Like the other generated bindings, a change
    /// here rewrites it and CI fails until it is committed.
    #[test]
    fn the_type_list_is_exported_for_the_app() {
        let list: Vec<TypeInfo> = AlertType::ALL.iter().map(|t| t.info()).collect();
        let text = serde_json::to_string_pretty(&list).unwrap() + "\n";
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../client/src/protocol/generated/alertTypes.json");
        if std::fs::read_to_string(&path).ok().as_deref() != Some(text.as_str()) {
            std::fs::write(&path, text).unwrap();
        }
    }

    /// A newer Øphio belongs to no project, so no choice for it is kept,
    /// and an older version never reads one it doesn't know.
    #[test]
    fn choices_for_types_in_no_project_are_never_kept() {
        let a = ProjectId::new();
        let choice = |alert_type, tier| TypeTier { alert_type, tier };
        let settings = NotificationSettings {
            projects: vec![ProjectNotifications {
                tiers: vec![
                    choice(AlertType::NewVersion, Tier::Off),
                    choice(AlertType::AppUpdate, Tier::Alert),
                    choice(AlertType::Merged, Tier::Off),
                ],
                ..ProjectNotifications::new(a)
            }],
            tiers: vec![choice(AlertType::NewVersion, Tier::Off)],
            ..NotificationSettings::default()
        }
        .tidy()
        .unwrap();
        assert!(settings.tiers.is_empty());
        assert_eq!(
            settings.projects[0].tiers,
            [choice(AlertType::Merged, Tier::Off)]
        );
        let only = NotificationSettings {
            projects: vec![ProjectNotifications {
                tiers: vec![choice(AlertType::NewVersion, Tier::Off)],
                ..ProjectNotifications::new(a)
            }],
            ..NotificationSettings::default()
        };
        assert!(only.tidy().unwrap().projects.is_empty());
        assert!(!AlertType::NewVersion.info().project);
        assert!(AlertType::Finished.info().project);
    }

    #[test]
    fn a_projects_type_choice_is_kept_once_and_mute_wins() {
        let a = ProjectId::new();
        let choice = |alert_type, tier| TypeTier { alert_type, tier };
        let settings = NotificationSettings {
            projects: vec![ProjectNotifications {
                tiers: vec![
                    choice(AlertType::Finished, Tier::Silent),
                    choice(AlertType::Question, Tier::Off),
                    choice(AlertType::Finished, Tier::Alert),
                ],
                ..ProjectNotifications::new(a)
            }],
            ..NotificationSettings::default()
        }
        .tidy()
        .unwrap();
        let p = settings.projects[0].clone();
        assert_eq!(p.tiers.len(), 2);
        let tier = |s: &NotificationSettings, t| s.project_tier(a, t);
        assert_eq!(tier(&settings, AlertType::Finished), Some(Tier::Alert));
        assert_eq!(tier(&settings, AlertType::Merged), None);
        assert_eq!(
            settings.project_tier(ProjectId::new(), AlertType::Merged),
            None
        );
        let muted = NotificationSettings {
            projects: vec![ProjectNotifications { mute: true, ..p }],
            ..settings.clone()
        };
        assert_eq!(tier(&muted, AlertType::Finished), Some(Tier::Off));
    }

    /// A newer version may keep a choice for a type this one doesn't know;
    /// the settings still read, without it, so a computer that went back to
    /// this version still starts, and a phone still reads its settings.
    #[test]
    fn choices_for_unknown_types_are_left_out_when_read() {
        let project = ProjectId::new();
        let text = serde_json::json!({
            "off": [],
            "bundle": false,
            "projects": [{
                "project_id": project,
                "tiers": [
                    { "type": "something_new", "tier": "off" },
                    { "type": "finished", "tier": "alert" },
                    { "type": "merged", "tier": "louder_still" },
                ],
            }],
            "tiers": [
                { "type": "something_new", "tier": "silent" },
                { "type": "slow", "tier": "off" },
                "not a choice",
            ],
            "revision": 3,
        });
        let settings: NotificationSettings = serde_json::from_value(text).unwrap();
        assert_eq!(settings.revision, 3);
        assert_eq!(
            settings.tiers,
            [TypeTier {
                alert_type: AlertType::Slow,
                tier: Tier::Off
            }]
        );
        assert_eq!(
            settings.projects[0].tiers,
            [TypeTier {
                alert_type: AlertType::Finished,
                tier: Tier::Alert
            }]
        );
        // Not a list at all is still an error.
        assert!(serde_json::from_str::<NotificationSettings>(r#"{"tiers":3}"#).is_err());
    }

    #[test]
    fn topics_have_their_wire_names() {
        let text = serde_json::to_string(&AlertTopic::PrNeedsYou).unwrap();
        assert_eq!(text, r#""pr_needs_you""#);
        let empty: NotificationSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(empty, NotificationSettings::default());
    }
}
