// SPDX-License-Identifier: Apache-2.0
//! Attention items as the phone's background watcher shows them.
use crate::{CompanionHub, api::Caller};
use ports::tool_approval::plain_tool_title;
use protocol::{
    alerts::{
        Alert, AlertQuestion, MAX_ALERT_CHOICE, MAX_ALERT_CHOICES, MAX_ALERT_QUESTION,
        MAX_OPEN_ALERTS, alert_text, cut,
    },
    model::{
        Approval, ApprovalKind, ApprovalState, AttentionItem, AttentionKind, Project, Run, Task,
    },
    notifications::{AlertTopic, AlertType, NotificationSettings, Tier},
    *,
};

/// The companion's own heading for an attention item, by its id, when the
/// outcome isn't the task's own (a PR merged, a find run's issues filed).
pub(crate) const HEADLINE: &str = "alert_headline";

/// Whether an alert is sent quiet: the computer raised it quietly, one of
/// the person's devices handled its notification already, or the person
/// put it away on this computer (`held`, see [`held_back`]). Every phone is
/// told the same. One that chooses by type keeps its own choice for each
/// type besides, and puts away a known alert that turns quiet in a fresh
/// open list, so muting a project clears what it already shows.
fn quiet(item: &AttentionItem, held: bool) -> bool {
    item.quiet || item.alert_handled_at.is_some() || held
}

/// Whether the person put an alert of `alert_type` in `project_id` away on
/// this computer: the project is muted, or it chose Off for the type. A
/// task's "Tell me when it's done" (`follow`) is heard even so; the person
/// asked for that one. The topic switches older phones saved are brought
/// over as these choices (see [`crate::notifications::bring_over`]), so
/// they count here too.
pub(crate) fn held_back(
    settings: &NotificationSettings,
    project_id: ProjectId,
    alert_type: Option<AlertType>,
    follow: bool,
) -> bool {
    !follow && alert_type.and_then(|t| settings.project_tier(project_id, t)) == Some(Tier::Off)
}

/// Whether an older phone, which can't choose by type, is told an alert is
/// quiet because its type is off by default and its project didn't choose
/// otherwise: so an older phone and a newer one with the recommended
/// choices show the same.
fn off_by_default(
    settings: &NotificationSettings,
    project_id: ProjectId,
    alert_type: Option<AlertType>,
    follow: bool,
) -> bool {
    let Some(t) = alert_type else {
        return false;
    };
    !follow
        && settings
            .project_tier(project_id, t)
            .unwrap_or(t.default_tier())
            == Tier::Off
}

impl CompanionHub {
    /// `types` is whether the phone chooses by type itself, as its `Watch`
    /// said; an older phone is told quiet what the type's recommended
    /// choice turns off, as a newer phone would hold it back.
    pub(crate) fn alerts_open(
        &self,
        caller: &Caller,
        types: bool,
    ) -> Result<(Vec<Alert>, u64), ProtocolError> {
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
                alerts.extend(Self::as_alert(r, &current, &settings, item, types)?);
            }
            alerts.reverse();
            Ok((alerts, head))
        })
    }
    pub(crate) fn alert_for(
        &self,
        caller: &Caller,
        attention_id: AttentionId,
        types: bool,
    ) -> Result<Option<Alert>, ProtocolError> {
        self.read(|r| {
            let current = Self::current_caller(r, caller)?;
            let settings = Self::notification_settings(r)?;
            match r.get::<AttentionItem>("attention", attention_id)? {
                Some(item) => Self::as_alert(r, &current, &settings, item, types),
                None => Ok(None),
            }
        })
    }
    /// One of `caller`'s devices handled these alerts' notifications: each
    /// is marked handled, so it goes from every device and isn't raised
    /// again. Only open alerts `caller` could be sent count; the rest, and
    /// those handled already, are passed over without a word, as a phone
    /// may name one that closed or that it no longer may see.
    pub(crate) fn mark_alerts_handled(
        &self,
        caller: &Caller,
        attention_ids: &[AttentionId],
    ) -> Result<(), ProtocolError> {
        let now = self.inner.clock.now();
        let ids: std::collections::BTreeSet<AttentionId> = (attention_ids.iter().copied())
            .take(protocol::alerts::MAX_SHOWING)
            .collect();
        if ids.is_empty() {
            return Ok(());
        }
        self.write(|tx| {
            let current = Self::current_caller(&tx.read(), caller)?;
            let settings = Self::notification_settings(&tx.read())?;
            for id in ids {
                let Some(mut item) = tx.read().get::<AttentionItem>("attention", id)? else {
                    continue;
                };
                if item.resolved_at.is_some() || item.alert_handled_at.is_some() {
                    continue;
                }
                let sent = Self::as_alert(&tx.read(), &current, &settings, item.clone(), true)?;
                if sent.is_none() {
                    continue;
                }
                item.alert_handled_at = Some(now);
                tx.put("attention", id, &item)?;
                tx.emit(Event::AttentionUpserted { item })?;
            }
            Ok(())
        })
    }
    /// An open item about a task, if `caller` may see it as an event today,
    /// marked as the person's notification settings take it.
    fn as_alert(
        r: &store::Read<'_>,
        caller: &Caller,
        settings: &NotificationSettings,
        item: AttentionItem,
        types: bool,
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
                headline: Some(alert_text(&plain_tool_title(&item.title))),
                question: None,
                topic: Some(AlertTopic::Reports),
                // In no project: only each phone's own choice applies.
                quiet: quiet(&item, false),
                bundle: false,
                report: None,
                sign_in: None,
                agent_update: None,
                new_version: None,
                alert_type: AlertType::of(&item, false),
                project_tier: None,
                follow: false,
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
                topic: Some(AlertTopic::Reports),
                // In no project: only each phone's own choice applies.
                quiet: quiet(&item, false),
                bundle: false,
                alert_type: AlertType::of(&item, false),
                report: Some(Box::new(report)),
                sign_in: None,
                agent_update: None,
                new_version: None,
                project_tier: None,
                follow: false,
            }));
        }
        // Agent release/model news is host-wide, never a fabricated task.
        if let Some(update) = &item.agent_update
            && item.task_id.is_none()
        {
            if item.resolved_at.is_some()
                || !Self::visible(r, caller, &Event::AttentionUpserted { item: item.clone() })?
            {
                return Ok(None);
            }
            let mut update = update.clone();
            update.version = update.version.as_deref().map(alert_text);
            update.models = update
                .models
                .iter()
                .take(20)
                .map(|m| alert_text(m))
                .collect();
            return Ok(Some(Alert {
                attention_id: item.attention_id,
                kind: item.kind,
                project_id: ProjectId(uuid::Uuid::nil()),
                project: String::new(),
                task_id: TaskId(uuid::Uuid::nil()),
                task: String::new(),
                harness: update.harness,
                created_at: item.created_at,
                headline: Some(alert_text(&plain_tool_title(&item.title))),
                question: None,
                topic: None,
                quiet: quiet(&item, false),
                bundle: false,
                alert_type: AlertType::of(&item, false),
                report: None,
                sign_in: None,
                new_version: None,
                agent_update: Some(Box::new(update)),
                project_tier: None,
                follow: false,
            }));
        }
        // A newer Øphio out for this computer.
        if let Some(version) = &item.new_version
            && item.task_id.is_none()
        {
            if item.resolved_at.is_some()
                || !Self::visible(r, caller, &Event::AttentionUpserted { item: item.clone() })?
            {
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
                headline: Some(alert_text(&plain_tool_title(&item.title))),
                question: None,
                topic: None,
                // In no project: only each phone's own choice applies.
                quiet: quiet(&item, false),
                bundle: false,
                alert_type: AlertType::of(&item, false),
                report: None,
                sign_in: None,
                agent_update: None,
                new_version: Some(Box::new(protocol::alerts::NewVersionAlert {
                    version: alert_text(version),
                })),
                project_tier: None,
                follow: false,
            }));
        }
        // An agent that isn't signed in on this computer.
        if let Some(account_id) = &item.account_id
            && item.task_id.is_none()
        {
            let Some(harness) = crate::sign_in_watch::harness_of(account_id) else {
                return Ok(None);
            };
            if item.resolved_at.is_some()
                || !Self::visible(r, caller, &Event::AttentionUpserted { item: item.clone() })?
            {
                return Ok(None);
            }
            let service = crate::sign_in_watch::account_of(harness)
                .map(|(_, name)| name)
                .unwrap_or_default();
            return Ok(Some(Alert {
                attention_id: item.attention_id,
                kind: item.kind,
                project_id: ProjectId(uuid::Uuid::nil()),
                project: String::new(),
                task_id: TaskId(uuid::Uuid::nil()),
                task: String::new(),
                harness,
                created_at: item.created_at,
                headline: Some(alert_text(&plain_tool_title(&item.title))),
                question: None,
                topic: Some(AlertTopic::NeedsYou),
                quiet: quiet(&item, false),
                bundle: false,
                alert_type: AlertType::of(&item, false),
                report: None,
                sign_in: Some(Box::new(protocol::alerts::SignInAlert {
                    account_id: account_id.clone(),
                    service: service.into(),
                })),
                agent_update: None,
                new_version: None,
                project_tier: None,
                follow: false,
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
        let alert_type = AlertType::of(&item, task.conversation);
        let harness = match item.run_id {
            Some(id) => r
                .get::<Run>("run", id)?
                .map(|run| run.harness)
                .unwrap_or(task.harness),
            None => task.harness,
        };
        let follow = task.notify_done && alert_type.is_some_and(AlertType::follows_task);
        Ok(Some(Alert {
            topic: item.topic(),
            quiet: quiet(
                &item,
                held_back(settings, project.project_id, alert_type, follow)
                    || (!types && off_by_default(settings, project.project_id, alert_type, follow)),
            ),
            alert_type,
            // A muted project says Off, whatever the type.
            project_tier: alert_type.and_then(|t| settings.project_tier(project.project_id, t)),
            follow,
            bundle: settings.bundles(project.project_id),
            attention_id: item.attention_id,
            kind: item.kind,
            project_id: project.project_id,
            project: alert_text(&project.name),
            task_id,
            task: alert_text(&task.title),
            harness,
            created_at: item.created_at,
            headline: r.get::<String>(HEADLINE, item.attention_id)?,
            question,
            report: None,
            sign_in: None,
            agent_update: None,
            new_version: None,
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
        text: cut(&plain_tool_title(text), MAX_ALERT_QUESTION),
        asked,
        choices,
    })
}
