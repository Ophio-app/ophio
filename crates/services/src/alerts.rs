// SPDX-License-Identifier: Apache-2.0
//! Attention items as the phone's background watcher shows them.
use crate::{CompanionHub, api::Caller};
use protocol::{
    alerts::{
        Alert, AlertQuestion, MAX_ALERT_CHOICE, MAX_ALERT_CHOICES, MAX_ALERT_QUESTION,
        MAX_OPEN_ALERTS, alert_text, cut,
    },
    model::{Approval, ApprovalKind, ApprovalState, AttentionItem, AttentionKind, Project, Task},
    notifications::NotificationSettings,
    *,
};

/// The companion's own heading for an attention item, by its id, when the
/// outcome isn't the task's own (a PR merged, a find run's issues filed).
pub(crate) const HEADLINE: &str = "alert_headline";

impl CompanionHub {
    pub(crate) fn alerts_open(&self, caller: &Caller) -> Result<(Vec<Alert>, u64), ProtocolError> {
        self.read(|r| {
            let current = Self::current_caller(r, caller)?;
            let settings = Self::notification_settings(r)?;
            let head = r.head()?;
            let mut open: Vec<AttentionItem> = (r.list::<AttentionItem>("attention")?.into_iter())
                .filter(|item| item.resolved_at.is_none() && item.kind.alerts())
                .collect();
            open.sort_by_key(|item| (item.created_at, item.attention_id));
            let mut alerts = Vec::new();
            for item in open.into_iter().rev() {
                if alerts.len() == MAX_OPEN_ALERTS {
                    break;
                }
                alerts.extend(Self::as_alert(r, &current, &settings, item)?);
            }
            alerts.reverse();
            Ok((alerts, head))
        })
    }
    pub(crate) fn alert_for(
        &self,
        caller: &Caller,
        attention_id: AttentionId,
    ) -> Result<Option<Alert>, ProtocolError> {
        self.read(|r| {
            let current = Self::current_caller(r, caller)?;
            let settings = Self::notification_settings(r)?;
            match r.get::<AttentionItem>("attention", attention_id)? {
                Some(item) => Self::as_alert(r, &current, &settings, item),
                None => Ok(None),
            }
        })
    }
    /// An open item about a task, if `caller` may see it as an event today,
    /// marked as the person's notification settings take it.
    fn as_alert(
        r: &store::Read<'_>,
        caller: &Caller,
        settings: &NotificationSettings,
        item: AttentionItem,
    ) -> store::Result<Option<Alert>> {
        if crate::report_inbox::is_inbox(&item) {
            if item.resolved_at.is_some()
                || !Self::visible(r, caller, &Event::AttentionUpserted { item: item.clone() })?
            {
                return Ok(None);
            }
            return Ok(Some(Alert {
                attention_id: item.attention_id,
                kind: item.kind,
                project_id: ProjectId(uuid::Uuid::nil()),
                project: "Problem reports".into(),
                task_id: TaskId(uuid::Uuid::nil()),
                task: String::new(),
                harness: protocol::capability::HarnessKind::ClaudeCode,
                created_at: item.created_at,
                headline: Some(alert_text(&item.title)),
                question: None,
                topic: Some(protocol::notifications::AlertTopic::Reports),
                quiet: item.quiet
                    || settings
                        .off
                        .contains(&protocol::notifications::AlertTopic::Reports),
                bundle: false,
                report: None,
            }));
        }
        // A fix's request names its task, and alerts as any task's item.
        if item.report_id.is_some() && item.task_id.is_none() {
            if item.resolved_at.is_some()
                || !item.kind.alerts()
                || !Self::visible(r, caller, &Event::AttentionUpserted { item: item.clone() })?
            {
                return Ok(None);
            }
            let Some(report) =
                r.get::<protocol::reports::ReportAlert>("report_alert", item.attention_id)?
            else {
                return Ok(None);
            };
            if !report.status.alerts() {
                return Ok(None);
            }
            return Ok(Some(Alert {
                attention_id: item.attention_id,
                kind: item.kind,
                project_id: ProjectId(uuid::Uuid::nil()),
                project: String::new(),
                task_id: TaskId(uuid::Uuid::nil()),
                task: String::new(),
                harness: protocol::capability::HarnessKind::ClaudeCode,
                created_at: item.created_at,
                headline: None,
                question: None,
                topic: Some(protocol::notifications::AlertTopic::Reports),
                quiet: item.quiet
                    || settings
                        .off
                        .contains(&protocol::notifications::AlertTopic::Reports),
                bundle: false,
                report: Some(Box::new(report)),
            }));
        }
        let Some(task_id) = item.task_id else {
            return Ok(None);
        };
        if item.resolved_at.is_some() || !item.kind.alerts() {
            return Ok(None);
        }
        let event = Event::AttentionUpserted { item: item.clone() };
        if !Self::visible(r, caller, &event)? {
            return Ok(None);
        }
        let Some(task) = r.get::<Task>("task", task_id)? else {
            return Ok(None);
        };
        let Some(project) = r.get::<Project>("project", task.project_id)? else {
            return Ok(None);
        };
        let question = match item.approval_id {
            Some(id) if item.kind == AttentionKind::Question && caller.grants.approvals => r
                .get::<Approval>("approval", id)?
                .as_ref()
                .and_then(as_question),
            _ => None,
        };
        let topic = item.topic();
        Ok(Some(Alert {
            topic,
            quiet: item.quiet || topic.is_some_and(|t| !settings.alerts(project.project_id, t)),
            bundle: settings.bundles(project.project_id),
            attention_id: item.attention_id,
            kind: item.kind,
            project_id: project.project_id,
            project: alert_text(&project.name),
            task_id,
            task: alert_text(&task.title),
            harness: task.harness,
            created_at: item.created_at,
            headline: r.get::<String>(HEADLINE, item.attention_id)?,
            question,
            report: None,
        }))
    }
}

/// A waiting question as its notification asks it: one asked in words, or
/// a single structured one. Several questions, or a secret one, are answered
/// in the app.
fn as_question(approval: &Approval) -> Option<AlertQuestion> {
    if approval.kind != ApprovalKind::Question || approval.state != ApprovalState::Pending {
        return None;
    }
    let (text, asked, choices) = match approval.questions.as_slice() {
        // Asked in words, so answered in words.
        [] => {
            let detail = (approval.detail.as_deref().map(str::trim)).filter(|d| !d.is_empty());
            (detail.unwrap_or(&approval.title), None, Vec::new())
        }
        [question] if !question.secret => {
            let labels: Vec<String> = question.options.iter().map(|o| o.label.clone()).collect();
            // Choices that would be cut, or more than fit, can't be offered
            // whole, and several can't be picked from a notification.
            let fits = !question.multi_select
                && labels.len() <= MAX_ALERT_CHOICES
                && labels
                    .iter()
                    .all(|label| label.chars().count() <= MAX_ALERT_CHOICE);
            let choices = if fits { labels } else { Vec::new() };
            (
                question.question.as_str(),
                Some(question.question.clone()),
                choices,
            )
        }
        _ => return None,
    };
    Some(AlertQuestion {
        approval_id: approval.approval_id,
        revision: approval.revision,
        text: cut(text, MAX_ALERT_QUESTION),
        asked,
        choices,
    })
}
