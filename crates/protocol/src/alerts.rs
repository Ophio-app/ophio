// SPDX-License-Identifier: Apache-2.0
//! The alerts connection: what a phone's background watcher needs to raise
//! and clear notifications while the app's screen is closed, and nothing else.
//!
//! The watcher signs in on its own connection (`/v1/alerts`), sends
//! [`AlertClientFrame::Watch`], and receives [`AlertServerFrame::Open`]: every
//! alert still open for the device, oldest first. After that it receives each
//! new alert and each one answered anywhere. Every reconnect starts with a
//! fresh `Open`, so the open list is the resume point: an alert raised and
//! answered while the phone was unreachable is never shown, and one still
//! open arrives once, oldest first. The phone remembers which ones it has
//! already shown.
//!
//! The computer keeps the connection alive with WebSocket pings, so the phone
//! sets no timers of its own; it only answers.
//!
//! A device sees an alert only when it may see the attention item today.
//! Alerts carry no task text beyond the title, and no screen or terminal data;
//! a headline is the companion's own words, never an agent's. The one
//! exception is a question the agent asked the person: the owner asked to
//! read and answer it from the notification, so its words and choices come
//! along, cut to length. A secret question never does.
//!
//! The phone answers on its own short control connection with the same
//! `ResolveApproval` command the app sends, so the computer applies all its
//! usual checks; this connection stays receive-only.
use crate::{
    ApprovalId, AttentionId, ProjectId, TaskId, capability::HarnessKind, model::AttentionKind,
    time::Timestamp,
};
use serde::{Deserialize, Serialize};

/// The most alerts `Open` lists; older open ones are left out.
pub const MAX_OPEN_ALERTS: usize = 200;
/// Longest project name or task title an alert carries, in characters.
pub const MAX_ALERT_TEXT: usize = 200;
/// The most alerts `Watch` names as showing.
pub const MAX_SHOWING: usize = 256;
/// Longest question an alert carries, in characters.
pub const MAX_ALERT_QUESTION: usize = 1000;
/// The most choices an alert's question offers.
pub const MAX_ALERT_CHOICES: usize = 8;
/// Longest choice an alert's question offers, in characters. A longer one
/// leaves the choices off: the person answers in words or opens the app.
pub const MAX_ALERT_CHOICE: usize = 60;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(export)]
pub enum AlertClientFrame {
    /// Start watching. Sent once, first. `showing` names the alerts from this
    /// computer the phone still shows or keeps to show, at most
    /// [`MAX_SHOWING`]. The first open list names each no longer open, which
    /// it cannot otherwise say of those it leaves out when full. Older phones
    /// send none.
    Watch {
        #[serde(default)]
        showing: Vec<AttentionId>,
        /// The phone understands [`Alert::quiet`]. Older phones aren't sent
        /// quiet alerts at all.
        #[serde(default)]
        quiet: bool,
        /// The phone understands [`Alert::report`]. Older phones aren't sent
        /// report alerts at all.
        #[serde(default)]
        reports: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(export)]
pub enum AlertServerFrame {
    /// Every alert open for this device, oldest first, at most
    /// [`MAX_OPEN_ALERTS`] of the newest. Replaces any earlier list. The
    /// first after `Watch` also names, in `gone`, those the phone said it
    /// shows that are no longer open, so it settles both at once before
    /// raising any it kept.
    Open {
        alerts: Vec<Alert>,
        now: Timestamp,
        #[serde(default)]
        gone: Vec<AttentionId>,
    },
    /// A new alert.
    Raised { alert: Alert },
    /// An alert answered, dismissed or closed anywhere.
    Resolved { attention_id: AttentionId },
}

/// One attention item as a notification needs it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct Alert {
    pub attention_id: AttentionId,
    pub kind: AttentionKind,
    pub project_id: ProjectId,
    pub project: String,
    pub task_id: TaskId,
    pub task: String,
    /// The task's agent, for "Question from <agent>".
    pub harness: HarnessKind,
    pub created_at: Timestamp,
    /// The companion's own heading for an outcome that isn't the task's,
    /// such as "PR #12 merged" or "Found 4, filed #160 to #163", shown in
    /// place of the kind's usual heading. Older computers send none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub headline: Option<String>,
    /// The question the agent is waiting on, when it asked one the phone can
    /// answer from its notification. Older computers send none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub question: Option<AlertQuestion>,
    /// What the alert is about, for the person's notification settings.
    /// Older computers send none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub topic: Option<crate::notifications::AlertTopic>,
    /// The person turned alerts like this off, or the item alerts as part
    /// of something larger (`AttentionItem::quiet`): the phone counts it as
    /// raised and shows nothing, so turning them back on raises only new
    /// ones. Sent only to a phone whose `Watch` said `quiet`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[ts(as = "Option<bool>", optional)]
    pub quiet: bool,
    /// The person wants one alert for the project: once a second would
    /// show, the phone folds them into one that counts them.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[ts(as = "Option<bool>", optional)]
    pub bundle: bool,
    /// The problem report the alert is about. Its `project_id` and `task_id`
    /// are then the nil id and its `project` and `task` empty. Sent only to a
    /// phone whose `Watch` said `reports`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub report: Option<Box<crate::reports::ReportAlert>>,
}

/// An agent's question as a notification shows it, answered with
/// `ResolveApproval { approval_id, decision: Answer }`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct AlertQuestion {
    pub approval_id: ApprovalId,
    /// The approval's revision, sent back as the command's expected one.
    pub revision: u32,
    /// The question, at most [`MAX_ALERT_QUESTION`] characters.
    pub text: String,
    /// The question exactly as asked, to answer by; none when it was asked in
    /// words only, so the answer is words only too.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub asked: Option<String>,
    /// Option labels to answer with, at most [`MAX_ALERT_CHOICES`] of
    /// [`MAX_ALERT_CHOICE`] characters, in the order offered. The person may
    /// always answer in their own words instead.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(as = "Option<Vec<String>>", optional)]
    pub choices: Vec<String>,
}

impl AttentionKind {
    /// Whether a background watcher raises a notification for this kind. A
    /// connection problem is the app's own state, not a task's.
    pub fn alerts(self) -> bool {
        !matches!(self, AttentionKind::ConnectionIssue)
    }
}

/// `text` cut to [`MAX_ALERT_TEXT`] characters.
pub fn alert_text(text: &str) -> String {
    cut(text, MAX_ALERT_TEXT)
}

/// `text` cut to `most` characters, marked when cut.
pub fn cut(text: &str, most: usize) -> String {
    match text.char_indices().nth(most) {
        Some((end, _)) => format!("{}…", &text[..end]),
        None => text.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip_with_their_wire_names() {
        // As older phones send it, naming none showing.
        let watch: AlertClientFrame = serde_json::from_str(r#"{"type":"watch"}"#).unwrap();
        assert_eq!(
            watch,
            AlertClientFrame::Watch {
                showing: vec![],
                quiet: false,
                reports: false,
            }
        );
        let id = AttentionId::new();
        let watch = AlertClientFrame::Watch {
            showing: vec![id],
            quiet: true,
            reports: false,
        };
        let text = serde_json::to_string(&watch).unwrap();
        assert_eq!(
            text,
            format!(
                r#"{{"type":"watch","showing":["{}"],"quiet":true,"reports":false}}"#,
                id.as_uuid()
            )
        );
        let resolved = AlertServerFrame::Resolved {
            attention_id: AttentionId::new(),
        };
        let text = serde_json::to_string(&resolved).unwrap();
        assert!(text.starts_with(r#"{"type":"resolved""#));
        assert_eq!(
            serde_json::from_str::<AlertServerFrame>(&text).unwrap(),
            resolved
        );
    }

    #[test]
    fn long_text_is_cut_on_a_character() {
        assert_eq!(alert_text("short"), "short");
        let long = "é".repeat(MAX_ALERT_TEXT + 5);
        let cut = alert_text(&long);
        assert_eq!(cut.chars().count(), MAX_ALERT_TEXT + 1);
        assert!(cut.ends_with('…'));
    }
}
