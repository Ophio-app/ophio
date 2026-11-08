// SPDX-License-Identifier: Apache-2.0
//! Accounts the computer signs in to once and gives to every task it starts.
//!
//! Secrets stay on the computer. The phone sees services, identities and
//! states; the one value that travels the other way is a key or OAuth client
//! the owner sets, in [`AccountAction::SetKey`], and it is never sent back,
//! stored with the command or journaled.
use crate::time::Timestamp;
use serde::{Deserialize, Serialize};

/// The computer's word that Claude Code and Codex can sign in from the
/// phone's own browser (`SignIn { on_phone }`, `SubmitSignInCode`), and that
/// it raises a Needs-you item when one isn't signed in.
pub const SIGN_IN_ON_PHONE_FEATURE: &str = "sign_in_on_phone_v1";

/// A value that must not be shown, logged or stored outside the computer's
/// keyring. It is plain text on the wire, inside the paired connection, and
/// wiped from memory when dropped.
#[derive(
    Clone, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(transparent)]
#[ts(export)]
pub struct Secret(String);
impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
    pub fn expose(&self) -> &str {
        &self.0
    }
    /// Stands in for the value wherever a command is kept.
    pub fn withheld() -> Self {
        Self::default()
    }
}
impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(…)")
    }
}
impl Drop for Secret {
    fn drop(&mut self) {
        zeroize::Zeroize::zeroize(&mut self.0);
    }
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum AccountKind {
    /// An agent's own login (Claude Code, Codex), kept in the agent's store.
    Agent,
    /// A command-line tool's login, such as GitHub's `gh`.
    Cli,
    /// A standard OAuth service Ophio signs in to itself, such as Gmail.
    Oauth,
    /// A key the owner pastes or the phone's password manager fills.
    ApiKey,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[serde(tag = "state", rename_all = "snake_case")]
#[ts(export)]
pub enum AccountState {
    Active,
    /// Signed in, but Ophio gives it to no task until it is resumed.
    Paused,
    SignedOut,
    NeedsAttention {
        reason: String,
    },
}

/// What an account needs before it can be signed in.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum AccountNeed {
    /// An API key, set with [`AccountAction::SetKey`].
    Key,
    /// The OAuth client's JSON (Google's "Desktop app" download), set with
    /// [`AccountAction::SetKey`].
    Client,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct Account {
    pub account_id: String,
    /// The service's name, such as "GitHub".
    pub service: String,
    pub kind: AccountKind,
    /// Ships with Ophio; it can be signed out but not removed.
    pub builtin: bool,
    /// Who it is signed in as, as the service's own status check reports it.
    pub identity: Option<String>,
    pub state: AccountState,
    pub checked_at: Option<Timestamp>,
    /// Environment variables tasks receive while it is active. Names only.
    pub variables: Vec<String>,
    /// Where the owner can end the session or revoke the key at the provider.
    pub manage_url: Option<String>,
    pub needs: Option<AccountNeed>,
    /// Sign in runs an interactive sign-in on the computer's browser.
    pub browser_sign_in: bool,
    /// Sign in can instead be finished in the phone's own browser, with
    /// `SignIn { on_phone: true }`: Claude Code and Codex. Older computers
    /// send none.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[ts(as = "Option<bool>", optional)]
    pub phone_sign_in: bool,
    /// Signed in, but short of something Ophio asks for, and what to do:
    /// GitHub without permission to change Actions workflows, for one.
    /// Older computers send none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub note: Option<String>,
    /// Tasks running now that were given this account.
    pub used_by: Vec<crate::TaskId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[serde(tag = "stage", rename_all = "snake_case")]
#[ts(export)]
pub enum SignInStage {
    Starting,
    /// The provider's page is open in the computer's browser.
    Browser {
        url_host: String,
    },
    /// The provider asks for this one-time code in the browser.
    Code {
        code: String,
        url_host: String,
    },
    /// A sign-in started with `SignIn { on_phone: true }`: the provider's
    /// page, to open in the phone's own browser. This is the one place the
    /// whole sign-in link leaves the computer; it is never logged, kept or
    /// put in an error. Where `code` is set (Codex) the page asks for it;
    /// where `paste_code` is (Claude Code) the page shows a code to send
    /// back with `SubmitSignInCode`.
    OnPhone {
        link: Secret,
        url_host: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        code: Option<String>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        #[ts(as = "Option<bool>", optional)]
        paste_code: bool,
        /// The code sent last was refused: paste the whole code again.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        #[ts(as = "Option<bool>", optional)]
        code_refused: bool,
    },
    /// The tool finished; its own status check is confirming who signed in.
    Checking,
    Done {
        identity: Option<String>,
    },
    Failed {
        reason: String,
        /// A provider page that fixes the cause, such as the setting that
        /// lets Codex sign in with a code. Older computers send none.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        help_url: Option<String>,
    },
    Cancelled,
    TimedOut,
}
impl SignInStage {
    pub fn finished(&self) -> bool {
        matches!(
            self,
            Self::Done { .. } | Self::Failed { .. } | Self::Cancelled | Self::TimedOut
        )
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct SignIn {
    pub account_id: String,
    pub service: String,
    pub started_at: Timestamp,
    pub stage: SignInStage,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum AccountEvent {
    SignedIn,
    SignInFailed,
    Paused,
    Resumed,
    PausedAll,
    SignedOut,
    Deleted,
    KeySet,
    ServiceAdded,
}

/// One line of the account history kept on the computer. No secret values.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct AccountHistoryEntry {
    pub at: Timestamp,
    pub account_id: String,
    pub service: String,
    pub event: AccountEvent,
    /// The paired device or local user that asked.
    pub by: String,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct AccountsView {
    pub accounts: Vec<Account>,
    /// The sign-in in progress, or the last one until another starts.
    pub sign_in: Option<SignIn>,
    /// Newest first.
    pub history: Vec<AccountHistoryEntry>,
    /// Where keys and tokens are kept, such as "Secret Service".
    pub store: String,
    /// Why keys are kept somewhere weaker than the keyring, if they are.
    pub store_note: Option<String>,
    /// The computer's default browser, as the desktop names it.
    pub browser: Option<String>,
    /// The owner set up a password manager in that browser.
    pub password_manager_done: bool,
}

/// How a service the owner adds signs in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[ts(export)]
pub enum ServiceKind {
    ApiKey {
        /// The environment variable tasks receive, such as `OPENROUTER_API_KEY`.
        variable: String,
        /// An HTTPS address that answers 200 to the key as a bearer token.
        check_url: Option<String>,
        manage_url: Option<String>,
    },
    Oauth {
        authorize_url: String,
        token_url: String,
        scopes: Vec<String>,
        /// The variable tasks receive a fresh access token in.
        variable: String,
        manage_url: Option<String>,
    },
    Cli {
        /// Command lines, run without a shell in the account's own config folder.
        sign_in: String,
        status: String,
        sign_out: Option<String>,
        /// The variable that points the tool at that folder, such as `CLOUDSDK_CONFIG`.
        config_variable: String,
        manage_url: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct ServiceProfile {
    pub name: String,
    pub service: ServiceKind,
}

/// Password managers whose browser extension the owner can be pointed at.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum PasswordManager {
    Bitwarden,
    OnePassword,
    ProtonPass,
    Dashlane,
    LastPass,
    KeePassXc,
}
impl PasswordManager {
    /// The maker's own download page for the browser extension.
    pub fn extension_page(self) -> &'static str {
        match self {
            Self::Bitwarden => "https://bitwarden.com/download/#downloads-web-browser",
            Self::OnePassword => "https://1password.com/downloads/browser-extension",
            Self::ProtonPass => "https://proton.me/pass/download",
            Self::Dashlane => "https://www.dashlane.com/download",
            Self::LastPass => "https://www.lastpass.com/download",
            Self::KeePassXc => {
                "https://keepassxc.org/docs/KeePassXC_GettingStarted#_setup_browser_integration"
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
#[ts(export)]
pub enum AccountAction {
    /// Start the service's own sign-in in the computer's browser, or, with
    /// `on_phone`, one to finish in the phone's own browser (only where the
    /// account says `phone_sign_in`, on a computer that advertises
    /// `sign_in_on_phone_v1`; older ones refuse the field).
    SignIn {
        account_id: String,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        #[ts(as = "Option<bool>", optional)]
        on_phone: bool,
    },
    /// The code the provider's page showed, for the sign-in running now
    /// (`OnPhone { paste_code }`), named by its account and `started_at` as
    /// the view showed them; a code for another sign-in is refused. Written
    /// once to the sign-in and never shown, logged, stored with the command
    /// or journaled.
    SubmitSignInCode {
        account_id: String,
        started_at: Timestamp,
        code: Secret,
    },
    /// Put away the notice that an agent isn't signed in, for this lapse
    /// only: the next lapse raises a new one.
    DismissSignInNotice {
        account_id: String,
    },
    CancelSignIn,
    /// Stop giving the account to tasks, without signing out.
    Pause {
        account_id: String,
    },
    Resume {
        account_id: String,
    },
    PauseAll,
    SignOut {
        account_id: String,
    },
    /// Sign out, forget the account and its keys, and open the provider's
    /// page for ending the session where the tool cannot.
    Delete {
        account_id: String,
    },
    /// The one action that carries a secret to the computer.
    SetKey {
        account_id: String,
        key: Secret,
    },
    AddService {
        service: ServiceProfile,
    },
    /// Run the status checks again now.
    Check {
        account_id: Option<String>,
    },
    /// Open the extension page in the computer's default browser.
    SetUpPasswordManager {
        manager: PasswordManager,
    },
    PasswordManagerDone,
}
impl AccountAction {
    pub fn without_secrets(&self) -> Self {
        match self {
            Self::SetKey { account_id, .. } => Self::SetKey {
                account_id: account_id.clone(),
                key: Secret::withheld(),
            },
            Self::SubmitSignInCode {
                account_id,
                started_at,
                ..
            } => Self::SubmitSignInCode {
                account_id: account_id.clone(),
                started_at: *started_at,
                code: Secret::withheld(),
            },
            other => other.clone(),
        }
    }
    /// Starts a program on the computer that the owner described from the phone.
    pub fn runs_commands(&self) -> bool {
        matches!(
            self,
            Self::AddService {
                service: ServiceProfile {
                    service: ServiceKind::Cli { .. },
                    ..
                }
            }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_secret_never_prints() {
        let action = AccountAction::SetKey {
            account_id: "openrouter".into(),
            key: Secret::new("sk-or-v1-abcdef"),
        };
        assert!(!format!("{action:?}").contains("abcdef"));
        let kept = serde_json::to_string(&action.without_secrets()).unwrap();
        assert!(!kept.contains("abcdef"));
        assert!(serde_json::to_string(&action).unwrap().contains("abcdef"));
    }

    #[test]
    fn a_pasted_sign_in_code_is_sent_but_never_kept_or_printed() {
        let action = AccountAction::SubmitSignInCode {
            account_id: "claude".into(),
            started_at: Timestamp(1),
            code: Secret::new("pasted-code#state"),
        };
        assert!(!format!("{action:?}").contains("pasted-code"));
        let kept = serde_json::to_string(&action.without_secrets()).unwrap();
        assert!(!kept.contains("pasted-code"));
        assert!(
            serde_json::to_string(&action)
                .unwrap()
                .contains("pasted-code")
        );
        // The link a phone sign-in opens doesn't print either.
        let stage = SignInStage::OnPhone {
            link: Secret::new("https://claude.com/cai/oauth/authorize?state=abc"),
            url_host: "claude.com".into(),
            code: None,
            paste_code: true,
            code_refused: false,
        };
        assert!(!format!("{stage:?}").contains("state=abc"));
    }

    #[test]
    fn a_sign_in_without_on_phone_reads_as_before() {
        let old: AccountAction =
            serde_json::from_str(r#"{"action":"sign_in","account_id":"claude"}"#).unwrap();
        assert_eq!(
            old,
            AccountAction::SignIn {
                account_id: "claude".into(),
                on_phone: false
            }
        );
        // And is sent as before, so an older computer takes it.
        assert_eq!(
            serde_json::to_string(&old).unwrap(),
            r#"{"action":"sign_in","account_id":"claude"}"#
        );
    }
}
