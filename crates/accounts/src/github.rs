// SPDX-License-Identifier: Apache-2.0
//! GitHub's API as the GitHub account signed in under Accounts, for the
//! GitHub workflow. Each call asks Accounts for the token again, so pausing
//! or signing out stops the next call.
use crate::ComputerAccounts;
use ports::github::{Github, GithubError, Method};
use serde_json::Value;

/// The most of one answer read: a page of a hundred pull requests with
/// their checks fits well within it.
const MAX_ANSWER: usize = 8 * 1024 * 1024;

#[derive(Clone)]
pub struct GithubApi {
    accounts: ComputerAccounts,
    http: reqwest::Client,
    base: String,
}
impl std::fmt::Debug for GithubApi {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GithubApi")
            .field("base", &self.base)
            .finish_non_exhaustive()
    }
}

impl GithubApi {
    pub fn new(accounts: ComputerAccounts) -> Self {
        Self::at(accounts, "https://api.github.com")
    }
    /// The API at `base`, for tests.
    pub fn at(accounts: ComputerAccounts, base: &str) -> Self {
        Self {
            accounts,
            http: crate::http(),
            base: base.trim_end_matches('/').to_owned(),
        }
    }

    async fn send(&self, request: reqwest::RequestBuilder) -> Result<Value, GithubError> {
        let token = self
            .accounts
            .token("github")
            .await
            .map_err(|_| GithubError::NotSignedIn)?;
        let mut answer = request
            .bearer_auth(token.as_str())
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send()
            .await
            .map_err(|e| GithubError::Failed(format!("GitHub couldn't be reached: {e}")))?;
        let status = answer.status();
        let mut body = Vec::new();
        while let Some(chunk) = answer
            .chunk()
            .await
            .map_err(|e| GithubError::Failed(format!("GitHub's answer broke off: {e}")))?
        {
            if body.len() + chunk.len() > MAX_ANSWER {
                return Err(GithubError::Failed("GitHub's answer was too long.".into()));
            }
            body.extend_from_slice(&chunk);
        }
        let value = if body.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&body).map_err(|_| {
                GithubError::Failed(format!(
                    "GitHub answered {status} with something unreadable."
                ))
            })?
        };
        if status.is_success() {
            Ok(value)
        } else {
            Err(GithubError::Status {
                code: status.as_u16(),
                message: message(&value).unwrap_or_else(|| status.to_string()),
            })
        }
    }
}

/// GitHub's own words for an error, with the first detail it gives.
fn message(value: &Value) -> Option<String> {
    let main = value.get("message")?.as_str()?.to_owned();
    let detail = value
        .get("errors")
        .and_then(Value::as_array)
        .and_then(|e| e.first())
        .and_then(|e| e.get("message").or_else(|| e.get("code")))
        .and_then(Value::as_str);
    Some(match detail {
        Some(d) => format!("{main}: {d}"),
        None => main,
    })
}

#[async_trait::async_trait]
impl Github for GithubApi {
    async fn report_git(
        &self,
        repository: &str,
        mirror: &std::path::Path,
        operation: ports::github::ReportGit,
    ) -> Result<(), GithubError> {
        use base64::Engine;
        let parts: Vec<_> = repository.split('/').collect();
        if parts.len() != 2
            || parts.iter().any(|s| {
                s.is_empty()
                    || !s
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
            })
        {
            return Err(GithubError::Failed("Invalid repository".into()));
        }
        let token = self
            .accounts
            .token("github")
            .await
            .map_err(|_| GithubError::NotSignedIn)?;
        let authorization = base64::engine::general_purpose::STANDARD
            .encode(format!("x-access-token:{}", token.as_str()));
        let mut command = tokio::process::Command::new("git");
        command
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_CONFIG_COUNT", "1")
            .env("GIT_CONFIG_KEY_0", "http.https://github.com/.extraheader")
            .env(
                "GIT_CONFIG_VALUE_0",
                format!("Authorization: Basic {authorization}"),
            )
            .arg("-C")
            .arg(mirror)
            .args([
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "core.fsmonitor=false",
                "-c",
                "credential.helper=",
                "-c",
                "protocol.ext.allow=never",
                "-c",
                "protocol.file.allow=never",
            ])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        let remote = format!("https://github.com/{repository}.git");
        match operation {
            ports::github::ReportGit::Fetch => {
                command.args([
                    "fetch",
                    "--no-tags",
                    "--no-recurse-submodules",
                    &remote,
                    "+refs/heads/*:refs/remotes/origin/*",
                ]);
            }
            ports::github::ReportGit::Push { commit, branch } => {
                if !matches!(commit.len(), 40 | 64)
                    || !commit.bytes().all(|b| b.is_ascii_hexdigit())
                    || !branch.starts_with("report/")
                    || branch[7..].is_empty()
                    || !branch[7..]
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
                {
                    return Err(GithubError::Failed("Invalid report branch".into()));
                }
                command.args([
                    "push",
                    "--no-verify",
                    &remote,
                    &format!("{commit}:refs/heads/{branch}"),
                ]);
            }
        }
        let status = tokio::time::timeout(std::time::Duration::from_secs(120), command.status())
            .await
            .map_err(|_| GithubError::Failed("Report object transfer timed out".into()))?
            .map_err(|_| GithubError::Failed("Report object transfer failed".into()))?;
        if !status.success() {
            return Err(GithubError::Failed("Report object transfer failed".into()));
        }
        Ok(())
    }
    async fn rest(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<Value, GithubError> {
        if !path.starts_with('/') || path.starts_with("//") {
            return Err(GithubError::Failed(format!(
                "Not a GitHub API path: {path}"
            )));
        }
        let url = format!("{}{path}", self.base);
        let method = reqwest::Method::from_bytes(method.as_str().as_bytes())
            .expect("the methods are HTTP's");
        let mut request = self.http.request(method, url);
        if let Some(body) = body {
            request = request.json(&body);
        }
        self.send(request).await
    }

    async fn graphql(&self, query: &str, variables: Value) -> Result<Value, GithubError> {
        let request = self
            .http
            .post(format!("{}/graphql", self.base))
            .json(&serde_json::json!({ "query": query, "variables": variables }));
        let mut answer = self.send(request).await?;
        if let Some(errors) = answer.get("errors").and_then(Value::as_array)
            && let Some(first) = errors.first()
        {
            return Err(GithubError::Status {
                code: 200,
                message: first
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("GitHub refused the query")
                    .to_owned(),
            });
        }
        Ok(answer
            .get_mut("data")
            .map(Value::take)
            .unwrap_or(Value::Null))
    }
}
