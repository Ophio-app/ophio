// SPDX-License-Identifier: Apache-2.0
//! Agent logins: usernames and passwords agents use on websites. Passwords,
//! setup keys and codes stay behind this trait; services see names, sites and
//! rules, and take a password only to type it into the agent browser.
use async_trait::async_trait;
use protocol::agent_logins::{AgentLoginAction, AgentLoginHistoryEntry, AgentLoginsView};
use protocol::{ProjectId, RunId, TaskId, accounts::Secret};
use std::sync::Arc;
use tokio::sync::watch;
use zeroize::Zeroizing;

/// The second level of a country's domain, such as `co.uk`: not a site.
pub const COUNTRY_SECOND_LEVELS: &[&str] = &[
    "co", "com", "net", "org", "gov", "edu", "ac", "or", "ne", "go", "gob", "mil", "nic", "ltd",
    "plc", "nom", "sch",
];

/// The whole site `host` is part of: its last two labels, or three under a
/// country's second level such as `co.uk`. Browsers count a site's subdomains
/// as the same site, and send its cookies between them.
pub fn whole_site(host: &str) -> &str {
    let labels: Vec<&str> = host.rsplitn(4, '.').collect();
    let keep = match labels.as_slice() {
        [top, second, _, ..] if top.len() == 2 && COUNTRY_SECOND_LEVELS.contains(second) => 3,
        _ => 2,
    };
    if labels.len() <= keep {
        return host;
    }
    let cut: usize = labels[..keep].iter().map(|l| l.len() + 1).sum();
    &host[host.len() + 1 - cut..]
}

#[derive(Debug, thiserror::Error)]
pub enum AgentLoginsError {
    /// The disclaimer has not been accepted on this computer.
    #[error("{0}")]
    Off(String),
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    Invalid(String),
    /// The task may not use the login, or it is paused.
    #[error("{0}")]
    Refused(String),
    #[error("{0}")]
    Failed(String),
}

/// The task asking to use a login.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginUser {
    pub task_id: TaskId,
    /// The run browsing now. An "Ask me first" approval lasts only for it.
    pub run_id: RunId,
    pub project_id: ProjectId,
    /// The catalog agent the task was started from, if any.
    pub agent: Option<String>,
}

/// A login a task may use, as the agent is told of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsableLogin {
    pub login_id: String,
    pub name: String,
    pub sites: Vec<String>,
    pub username: String,
    pub has_code_key: bool,
    pub notes: String,
    /// The owner is asked before each run first uses it.
    pub ask_first: bool,
    /// Changes whenever the login does, so an approval asked about one
    /// version of it doesn't open another.
    pub revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Access {
    Allowed,
    /// Allowed once the owner approves it for this run.
    AskFirst,
    Refused(String),
}

/// What Ophio types into a page. Never given to an agent.
pub struct Credentials {
    pub name: String,
    pub sites: Vec<String>,
    pub username: String,
    pub password: Zeroizing<String>,
}
impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials")
            .field("name", &self.name)
            .field("sites", &self.sites)
            .finish_non_exhaustive()
    }
}

#[async_trait]
pub trait AgentLogins: Send + Sync + std::fmt::Debug + 'static {
    async fn view(&self) -> AgentLoginsView;
    /// The disclaimer's current version was accepted here.
    fn enabled(&self) -> bool;
    async fn act(&self, action: AgentLoginAction, by: &str) -> Result<(), AgentLoginsError>;
    /// The password, for the owner; recorded in the history.
    async fn reveal(&self, login_id: &str, by: &str) -> Result<Secret, AgentLoginsError>;
    /// The logins `user` may use now, paused ones left out.
    fn usable(&self, user: &LoginUser) -> Vec<UsableLogin>;
    /// Whether `user` may use the login now.
    fn access(&self, login_id: &str, user: &LoginUser) -> Access;
    /// The owner let this run use an "Ask me first" login, as it was at
    /// `revision`. False, and nothing allowed, if it has changed since.
    fn approve(&self, login_id: &str, run_id: RunId, revision: u64) -> bool;
    /// The username and password, when [`Self::access`] allows it.
    async fn credentials(
        &self,
        login_id: &str,
        user: &LoginUser,
    ) -> Result<Credentials, AgentLoginsError>;
    /// The authenticator code now, when [`Self::access`] allows it.
    async fn code(
        &self,
        login_id: &str,
        user: &LoginUser,
    ) -> Result<Zeroizing<String>, AgentLoginsError>;
    /// Hosts of the logins `user` may not use: the agent browser opens none
    /// of them, or their subdomains, for it.
    fn blocked_sites(&self, user: &LoginUser) -> Vec<String>;
    /// Records a use, refusal or code prompt, and marks the login in use by
    /// the entry's task.
    fn record(&self, entry: AgentLoginHistoryEntry);
    /// The task has stopped browsing: it no longer uses any login.
    fn task_ended(&self, task_id: TaskId);
    /// The run has ended: an approval it was given goes with it.
    fn run_ended(&self, run_id: RunId);
    /// Every stored password and setup key, as each may be written, masked
    /// in everything every run says.
    fn secrets(&self) -> Arc<Vec<String>>;
    fn changed(&self) -> watch::Receiver<u64>;
    /// Sites the agent browser is still to be signed out of, before anything
    /// browses: a login was signed out of, deleted or edited when Ophio had
    /// no agent browser to clear.
    fn owed_sign_outs(&self) -> Vec<String>;
    /// Keeps `sites` to be signed out of when there is a browser again.
    fn owe_sign_out(&self, sites: &[String]) -> Result<(), AgentLoginsError>;
    /// The agent browser was signed out of `sites`.
    fn settled_sign_outs(&self, sites: &[String]);
}

#[cfg(test)]
mod tests {
    use super::whole_site;

    #[test]
    fn a_whole_site_keeps_its_last_two_labels_or_three_under_a_country() {
        assert_eq!(whole_site("secure.site.test"), "site.test");
        assert_eq!(whole_site("a.b.facebook.com"), "facebook.com");
        assert_eq!(whole_site("facebook.com"), "facebook.com");
        assert_eq!(whole_site("x.y.example.co.uk"), "example.co.uk");
        assert_eq!(whole_site("example.co.uk"), "example.co.uk");
        assert_eq!(whole_site("mail.example.de"), "example.de");
        assert_eq!(whole_site("localhost"), "localhost");
    }
}
