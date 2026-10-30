// SPDX-License-Identifier: Apache-2.0
//! Agent logins, as the phone and the CLI reach them. Passwords come in only
//! to be kept, and go out only to the owner who asks to see one.
use crate::{Caller, CompanionHub, error, unsupported};
use ports::{
    agent_browser::AgentBrowsing,
    agent_logins::{AgentLogins, AgentLoginsError},
};
use protocol::{
    CommandId, ErrorCode, ProtocolError, accounts::Secret, agent_logins::AgentLoginAction,
    auth::Principal, command::CommandResult,
};
use std::sync::Arc;

impl CompanionHub {
    pub(crate) fn agent_logins(&self) -> Result<Arc<dyn AgentLogins>, ProtocolError> {
        self.inner
            .agent_logins
            .clone()
            .ok_or_else(|| unsupported("Agent logins"))
    }

    pub(crate) async fn agent_login_action(
        &self,
        caller: &Caller,
        command_id: CommandId,
        action: &AgentLoginAction,
    ) -> Result<Option<CommandResult>, ProtocolError> {
        let logins = self.agent_logins()?;
        if let AgentLoginAction::AnswerCode { approval_id, code } = action {
            // A code typed into a chat stays in the chat's history.
            if matches!(caller.principal, Principal::Channel { .. }) {
                return Err(error(
                    ErrorCode::PermissionDenied,
                    "Type the code on your phone, not in a chat.",
                ));
            }
            self.answer_login_code(caller, command_id, *approval_id, code)
                .await?;
            return Ok(None);
        }
        let browser = self.inner.agent_browser.as_deref();
        let mut edited = None;
        match action {
            // Deleting a login promises its sessions go too.
            AgentLoginAction::SignOut { login_id } | AgentLoginAction::Delete { login_id } => {
                let sites = login_sites(&*logins, login_id).await?;
                sign_out(&*logins, browser, &sites).await?;
            }
            // So do the sites an edit leaves out, before the login stops
            // keeping other tasks from them.
            AgentLoginAction::Edit { login_id, details } => {
                let sites = login_sites(&*logins, login_id).await?;
                let kept: Vec<String> = (details.sites.iter())
                    .map(|s| s.trim().to_lowercase())
                    .collect();
                let left: Vec<String> = (sites.iter())
                    .filter(|s| !kept.contains(s))
                    .cloned()
                    .collect();
                sign_out(&*logins, browser, &left).await?;
                edited = Some((login_id, sites));
            }
            _ => {}
        }
        let by = self.caller_name(caller)?;
        logins
            .act(action.clone(), &by)
            .await
            .map_err(agent_login_error)?;
        // And once more as saved, for a sign-in made on one meanwhile.
        if let Some((login_id, before)) = edited {
            let after = login_sites(&*logins, login_id).await.unwrap_or_default();
            let left: Vec<String> = before.into_iter().filter(|s| !after.contains(s)).collect();
            if let Err(e) = sign_out(&*logins, browser, &left).await {
                tracing::warn!(error = %e.message, "signing out of sites an edited login left out failed");
            }
        }
        Ok(None)
    }

    /// The password, for the owner who asked to see it; recorded.
    pub(crate) async fn agent_login_password(
        &self,
        caller: &Caller,
        login_id: &str,
    ) -> Result<Secret, ProtocolError> {
        let logins = self.agent_logins()?;
        if matches!(
            caller.principal,
            Principal::Channel { .. } | Principal::System
        ) {
            return Err(error(
                ErrorCode::PermissionDenied,
                "Passwords are shown only on a paired phone or at the computer.",
            ));
        }
        let by = self.caller_name(caller)?;
        logins
            .reveal(login_id, &by)
            .await
            .map_err(agent_login_error)
    }
}

/// The sites a login covers now.
async fn login_sites(
    logins: &dyn AgentLogins,
    login_id: &str,
) -> Result<Vec<String>, ProtocolError> {
    let view = logins.view().await;
    let login = (view.logins.iter())
        .find(|l| l.login_id == login_id)
        .ok_or_else(|| error(ErrorCode::NotFound, "There is no such login."))?;
    Ok(login.sites.clone())
}

/// Signs the agent browser out of `sites`, or says nothing was changed.
/// Without a browser, the sign-out waits for there to be one again: nothing
/// browses before it is done.
async fn sign_out(
    logins: &dyn AgentLogins,
    browser: Option<&dyn AgentBrowsing>,
    sites: &[String],
) -> Result<(), ProtocolError> {
    if sites.is_empty() {
        return Ok(());
    }
    let Some(browser) = browser else {
        return logins.owe_sign_out(sites).map_err(agent_login_error);
    };
    browser.sign_out(sites).await.map_err(|e| {
        error(
            ErrorCode::TemporarilyUnavailable,
            format!(
                "Ophio couldn't sign the agent browser out of {}, so nothing was changed. \
                 Try again, or pause the login. {e}",
                sites.join(", ")
            ),
        )
    })
}

pub(crate) fn agent_login_error(e: AgentLoginsError) -> ProtocolError {
    match e {
        AgentLoginsError::Off(m) | AgentLoginsError::Refused(m) => {
            error(ErrorCode::PermissionDenied, m)
        }
        AgentLoginsError::NotFound(m) => error(ErrorCode::NotFound, m),
        AgentLoginsError::Invalid(m) => error(ErrorCode::InvalidRequest, m),
        AgentLoginsError::Failed(m) => error(ErrorCode::TemporarilyUnavailable, m),
    }
}
