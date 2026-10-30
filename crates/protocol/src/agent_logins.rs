// SPDX-License-Identifier: Apache-2.0
//! Agent logins: usernames and passwords agents use on websites, in Ophio's
//! own browser.
//!
//! Ophio types a password into the page itself, so no password reaches the
//! model, a transcript, the journal or the phone's task screens. The phone
//! sends one to the computer in [`AgentLoginAction::Add`] or
//! [`AgentLoginAction::SetSecret`], and reads one back only with
//! `Query::AgentLoginPassword`, which the history records.
use crate::accounts::Secret;
use crate::ids::{ApprovalId, ProjectId, TaskId};
use crate::time::Timestamp;
use serde::{Deserialize, Serialize};

/// The disclaimer's wording, by version. Accepting one version does not
/// accept the next.
pub const DISCLAIMER_VERSION: u32 = 1;
pub const DISCLAIMER_TITLE: &str = "Before you give agents logins";
/// Paragraphs; a line starting with "- " is one item of a list.
pub const DISCLAIMER: &[&str] = &[
    "Agents can use the logins you add here to sign in to websites and act as you there. That includes posting, sending messages, buying, and changing or deleting things.",
    "Øphio is built to keep these passwords away from the AI:",
    "- It doesn't send your passwords to the AI model or its provider. Øphio types them into the page itself, and only on the sites you set.",
    "- It stores them encrypted on this computer, so the model can't read them.",
    "This is how Øphio is designed, not a guarantee. We've taken every reasonable measure to make it secure, but no software is perfectly secure. A website, a bug, or an agent working around Øphio could still expose a password. We also can't control what an agent does once it's signed in.",
    "Agent logins is provided as is, without any guarantee. You use it at your own risk. Øphio and its contributors aren't responsible for what agents do with your logins, or for any loss that results.",
    "Only add logins you're willing to let an agent use, and keep bank and payment accounts out.",
];
pub const DISCLAIMER_CONFIRM: &str =
    "I understand that agents act with my logins and that I use this at my own risk.";
/// Shown at the top of Agent logins and on the Add login form.
pub const REMINDER: &str =
    "Agents can act as you on these sites. You use Agent logins at your own risk.";

/// Which tasks may use a login.
#[derive(
    Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[ts(export)]
pub struct LoginWho {
    /// Only tasks in these projects; every project when absent.
    pub projects: Option<Vec<ProjectId>>,
    /// Only tasks started from these agents of the catalog; any task when
    /// absent.
    pub agents: Option<Vec<String>>,
}

#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
    ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum LoginHow {
    /// A task that may use the login uses it whenever it needs to.
    #[default]
    Whenever,
    /// The first time a task needs it, the phone asks; once per task.
    AskFirst,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[serde(tag = "status", rename_all = "snake_case")]
#[ts(export)]
pub enum LoginStatus {
    Ready,
    /// This login or every login is paused: no task uses it.
    Paused,
    NeedsAttention {
        reason: String,
    },
}

/// One login as the phone sees it: never its password or setup key.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct AgentLogin {
    pub login_id: String,
    /// Such as "Facebook".
    pub name: String,
    /// Hosts, the main one first, such as `facebook.com`. A login is filled
    /// on these and their subdomains only.
    pub sites: Vec<String>,
    pub username: String,
    pub has_password: bool,
    /// An authenticator app's setup key is stored, so Ophio makes the codes.
    pub has_code_key: bool,
    /// For the agent, not secret.
    pub notes: String,
    pub who: LoginWho,
    pub how: LoginHow,
    /// Each use raises an alert on the phone.
    pub notify: bool,
    pub paused: bool,
    pub status: LoginStatus,
    pub last_used: Option<Timestamp>,
    /// Tasks running now that used it.
    pub used_by: Vec<TaskId>,
}

/// What [`AgentLoginAction::Add`] and [`AgentLoginAction::Edit`] set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct LoginDetails {
    pub name: String,
    pub sites: Vec<String>,
    pub username: String,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub who: LoginWho,
    #[serde(default)]
    pub how: LoginHow,
    #[serde(default)]
    pub notify: bool,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum AgentLoginEvent {
    /// The disclaimer was accepted; `detail` names its version.
    Accepted,
    Added,
    Edited,
    PasswordSet,
    CodeKeySet,
    CodeKeyRemoved,
    /// The password was shown on a phone.
    Shown,
    Paused,
    Resumed,
    PausedAll,
    ResumedAll,
    /// The agent browser was signed out of the login's sites.
    SignedOut,
    Deleted,
    /// Ophio filled the username and password for a task.
    Filled,
    /// Ophio made and filled an authenticator code.
    CodeFilled,
    /// A task asked the owner for a code.
    CodeAsked,
    /// The owner gave a code, which Ophio filled.
    CodeGiven,
    /// The owner let a task use an "Ask me first" login.
    Approved,
    /// A fill or a site was refused; `detail` says why.
    Refused,
}

/// One line of the history. No password, key or code.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct AgentLoginHistoryEntry {
    pub at: Timestamp,
    pub login_id: Option<String>,
    pub name: Option<String>,
    pub event: AgentLoginEvent,
    /// The paired device or local user that asked, or the task's agent.
    pub by: String,
    pub task_id: Option<TaskId>,
    pub site: Option<String>,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct AgentLoginsView {
    /// The current disclaimer was accepted on this computer.
    pub enabled: bool,
    /// The version last accepted here, if any.
    pub accepted_version: Option<u32>,
    pub disclaimer_version: u32,
    pub disclaimer_title: String,
    pub disclaimer: Vec<String>,
    pub disclaimer_confirm: String,
    pub reminder: String,
    pub paused_all: bool,
    pub logins: Vec<AgentLogin>,
    /// Newest first.
    pub history: Vec<AgentLoginHistoryEntry>,
    /// Where passwords are kept, such as "Secret Service".
    pub store: String,
    pub store_note: Option<String>,
    /// The browser agents use, or None when none was found.
    pub browser: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
#[ts(export)]
pub enum AgentLoginAction {
    /// Turn Agent logins on, accepting this version of the disclaimer.
    Accept {
        version: u32,
    },
    Add {
        details: LoginDetails,
        password: Option<Secret>,
        code_key: Option<Secret>,
    },
    Edit {
        login_id: String,
        details: LoginDetails,
    },
    /// Replace the password, the setup key, or both. An empty `code_key`
    /// removes it.
    SetSecret {
        login_id: String,
        password: Option<Secret>,
        code_key: Option<Secret>,
    },
    Pause {
        login_id: String,
    },
    Resume {
        login_id: String,
    },
    PauseAll,
    ResumeAll,
    /// Clear the agent browser's cookies and site data for the login's sites.
    SignOut {
        login_id: String,
    },
    /// Remove the login and its secrets, and sign the agent browser out of
    /// its sites.
    Delete {
        login_id: String,
    },
    /// The code a site sent the owner, for the task that asked for it.
    AnswerCode {
        approval_id: ApprovalId,
        code: Secret,
    },
}
impl AgentLoginAction {
    pub fn without_secrets(&self) -> Self {
        let keep = |s: &Option<Secret>| s.as_ref().map(|_| Secret::withheld());
        match self {
            Self::Add {
                details,
                password,
                code_key,
            } => Self::Add {
                details: details.clone(),
                password: keep(password),
                code_key: keep(code_key),
            },
            Self::SetSecret {
                login_id,
                password,
                code_key,
            } => Self::SetSecret {
                login_id: login_id.clone(),
                password: keep(password),
                code_key: keep(code_key),
            },
            Self::AnswerCode { approval_id, .. } => Self::AnswerCode {
                approval_id: *approval_id,
                code: Secret::withheld(),
            },
            other => other.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_are_withheld_where_a_command_is_kept() {
        let actions = [
            AgentLoginAction::Add {
                details: LoginDetails {
                    name: "Facebook".into(),
                    sites: vec!["facebook.com".into()],
                    username: "agent@example.com".into(),
                    notes: String::new(),
                    who: LoginWho::default(),
                    how: LoginHow::Whenever,
                    notify: false,
                },
                password: Some(Secret::new("correct horse battery")),
                code_key: Some(Secret::new("JBSWY3DPEHPK3PXP")),
            },
            AgentLoginAction::SetSecret {
                login_id: "l1".into(),
                password: Some(Secret::new("correct horse battery")),
                code_key: None,
            },
            AgentLoginAction::AnswerCode {
                approval_id: ApprovalId::new(),
                code: Secret::new("482913"),
            },
        ];
        for action in actions {
            let kept = serde_json::to_string(&action.without_secrets()).unwrap();
            assert!(!kept.contains("horse"), "{kept}");
            assert!(!kept.contains("JBSWY3"), "{kept}");
            assert!(!kept.contains("482913"), "{kept}");
            assert!(!format!("{action:?}").contains("horse"));
        }
    }

    #[test]
    fn a_secret_left_out_stays_out_and_one_given_stays_given() {
        let action = AgentLoginAction::SetSecret {
            login_id: "l1".into(),
            password: None,
            code_key: Some(Secret::new("")),
        };
        // Withheld still says a key was sent, so a kept command reads the same.
        assert_eq!(action.without_secrets(), action);
    }
}
