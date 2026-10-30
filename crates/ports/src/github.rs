// SPDX-License-Identifier: Apache-2.0
//! GitHub's API, as the account signed in under Accounts.
//!
//! The companion reads repositories, issues and pull requests through this,
//! and writes what the GitHub workflow publishes. Nothing here hands the
//! account's token to anyone.
use async_trait::async_trait;
use serde_json::Value;

/// An HTTP method the workflow uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
    Patch,
    Put,
    Delete,
}
#[derive(Debug, Clone)]
pub enum ReportGit {
    Fetch,
    Push { commit: String, branch: String },
}
impl Method {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Patch => "PATCH",
            Self::Put => "PUT",
            Self::Delete => "DELETE",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GithubError {
    #[error("Sign in to GitHub under Accounts first.")]
    NotSignedIn,
    /// GitHub answered with an error status; `message` is GitHub's own.
    #[error("GitHub answered {code}: {message}")]
    Status { code: u16, message: String },
    /// GitHub couldn't be reached, or answered with something unreadable.
    #[error("{0}")]
    Failed(String),
}
impl GithubError {
    pub fn status(&self) -> Option<u16> {
        match self {
            Self::Status { code, .. } => Some(*code),
            _ => None,
        }
    }
}

#[async_trait]
pub trait Github: Send + Sync + std::fmt::Debug + 'static {
    /// Host-only object transfer for an owned bare mirror. No credential reaches agents.
    async fn report_git(
        &self,
        _repository: &str,
        _mirror: &std::path::Path,
        _operation: ReportGit,
    ) -> Result<(), GithubError> {
        Err(GithubError::Failed(
            "Report object transfer is unavailable".into(),
        ))
    }
    /// One REST call. `path` starts with `/` and may carry a query string.
    /// An empty answer, like 204's, is `Value::Null`.
    async fn rest(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<Value, GithubError>;
    /// One GraphQL query or mutation; the answer's `data`. GraphQL's own
    /// errors are a `Status` with code 200.
    async fn graphql(&self, query: &str, variables: Value) -> Result<Value, GithubError>;
}
