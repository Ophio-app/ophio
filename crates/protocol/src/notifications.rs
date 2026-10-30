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
}

impl ProjectNotifications {
    pub fn new(project_id: ProjectId) -> Self {
        Self {
            project_id,
            mute: false,
            on: Vec::new(),
            off: Vec::new(),
            bundle: None,
        }
    }
    /// Says nothing the default doesn't.
    pub fn is_empty(&self) -> bool {
        !self.mute && self.on.is_empty() && self.off.is_empty() && self.bundle.is_none()
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
        let mut projects: Vec<ProjectNotifications> = Vec::new();
        for mut p in self.projects {
            once(&mut p.on);
            once(&mut p.off);
            p.on.retain(|t| !p.off.contains(t));
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
    fn topics_have_their_wire_names() {
        let text = serde_json::to_string(&AlertTopic::PrNeedsYou).unwrap();
        assert_eq!(text, r#""pr_needs_you""#);
        let empty: NotificationSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(empty, NotificationSettings::default());
    }
}
