// SPDX-License-Identifier: Apache-2.0
//! Each service's own status check, read for who is signed in. Only what the
//! tool reports is believed; its output is never shown or kept.
use crate::profiles::{Method, Profile};
use crate::tool::Tools;
use protocol::capability::HarnessKind;
use std::ffi::OsString;
use std::path::Path;
use std::time::Duration;

const LIMIT: Duration = Duration::from_secs(20);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Checked {
    In {
        identity: Option<String>,
        /// Signed in, but short of something Ophio asks for: what to do.
        note: Option<String>,
    },
    Out,
    Attention(String),
    /// The tool is not on this computer. That needs attention only if the
    /// account was signed in before.
    Missing(String),
    /// The check could not tell; the last answer stands.
    Unknown(String),
}

/// Runs a tool's status command and reads its answer.
pub async fn tool(profile: &Profile, tools: &Tools, config: &Path) -> Checked {
    let (status, extra) = match &profile.method {
        Method::Agent { status, .. } => (status, Vec::new()),
        Method::Cli {
            status,
            config_variable,
            ..
        } => (
            status,
            vec![(config_variable.clone(), OsString::from(config))],
        ),
        _ => return Checked::Unknown("not a tool".into()),
    };
    let output = match tools.run(status, &extra, LIMIT).await {
        Ok(output) => output,
        Err(e) if !tools.find(&status[0]).is_file() => {
            tracing::debug!("accounts: {e}");
            return Checked::Missing(format!("{} is not installed on this computer.", status[0]));
        }
        Err(e) => return Checked::Unknown(e),
    };
    match &profile.method {
        Method::Agent {
            harness: HarnessKind::ClaudeCode,
            ..
        } => claude(&output.stdout),
        Method::Agent {
            harness: HarnessKind::Codex,
            ..
        } => codex(&output.stdout, &output.stderr, output.success, tools),
        Method::Cli { .. } if status[0] == "gh" => gh(&output.stdout),
        _ if output.success => Checked::In {
            identity: None,
            note: None,
        },
        _ => Checked::Out,
    }
}

/// `claude auth status` prints `{"loggedIn": …, "email": …}`.
fn claude(stdout: &str) -> Checked {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(stdout.trim()) else {
        return Checked::Unknown("Claude Code's status was not readable".into());
    };
    match value.get("loggedIn").and_then(|v| v.as_bool()) {
        Some(true) => Checked::In {
            identity: value
                .get("email")
                .and_then(|e| e.as_str())
                .map(str::to_owned),
            note: None,
        },
        Some(false) => Checked::Out,
        None => Checked::Unknown("Claude Code's status was not readable".into()),
    }
}

/// `codex login status` says "Logged in using …" or "Not logged in"; who is
/// in the ID token Codex keeps.
fn codex(stdout: &str, stderr: &str, success: bool, tools: &Tools) -> Checked {
    let said = format!("{stdout}\n{stderr}");
    if said.contains("Not logged in") {
        return Checked::Out;
    }
    if !(success && said.contains("Logged in")) {
        return Checked::Unknown("Codex's status was not readable".into());
    }
    let home = tools
        .var("CODEX_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| tools.home().map(|h| h.join(".codex")));
    let identity = home
        .and_then(|h| std::fs::read(h.join("auth.json")).ok())
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|auth| {
            auth.pointer("/tokens/id_token")
                .and_then(|t| t.as_str())
                .and_then(crate::oauth::email)
        });
    Checked::In {
        identity,
        note: None,
    }
}

/// What agents can't do without, missing from a sign-in made before Ophio
/// asked for it: changing a repository's Actions workflows.
pub const WORKFLOW_NOTE: &str = "Agents can't change GitHub Actions workflows: this sign-in \
     doesn't include GitHub's workflow permission. Sign in again to add it.";

/// `gh auth status --json hosts` lists each account on the host, with the
/// token's scopes ("gist, read:org, repo").
fn gh(stdout: &str) -> Checked {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(stdout.trim()) else {
        return Checked::Unknown("GitHub's status was not readable".into());
    };
    let entries: Vec<&serde_json::Value> = value
        .get("hosts")
        .and_then(|h| h.as_object())
        .map(|hosts| {
            hosts
                .values()
                .filter_map(|v| v.as_array())
                .flatten()
                .collect()
        })
        .unwrap_or_default();
    let active = entries
        .iter()
        .find(|e| e.get("active").and_then(|a| a.as_bool()) == Some(true))
        .or(entries.first());
    match active {
        None => Checked::Out,
        Some(entry) if entry.get("state").and_then(|s| s.as_str()) == Some("success") => {
            // A token without listed scopes (a fine-grained one) is taken
            // as it is.
            let scopes = entry.get("scopes").and_then(|s| s.as_str()).unwrap_or("");
            let workflow = scopes.is_empty() || scopes.split(',').any(|s| s.trim() == "workflow");
            Checked::In {
                identity: entry
                    .get("login")
                    .and_then(|l| l.as_str())
                    .map(str::to_owned),
                note: (!workflow).then(|| WORKFLOW_NOTE.to_owned()),
            }
        }
        Some(_) => {
            Checked::Attention("GitHub no longer accepts this sign-in. Sign in again.".into())
        }
    }
}

/// Sends the key as a bearer token to the service's own check address.
pub async fn key(http: &reqwest::Client, check_url: Option<&str>, key: Option<&str>) -> Checked {
    let Some(key) = key else {
        return Checked::Out;
    };
    let Some(url) = check_url else {
        return Checked::In {
            identity: None,
            note: None,
        };
    };
    match http.get(url).bearer_auth(key).timeout(LIMIT).send().await {
        Ok(r) if r.status().is_success() => Checked::In {
            identity: None,
            note: None,
        },
        Ok(r) if matches!(r.status().as_u16(), 401 | 403) => {
            Checked::Attention("The service refused the key. Set a new one.".into())
        }
        Ok(r) => Checked::Unknown(format!("the service answered {}", r.status())),
        Err(e) => Checked::Unknown(format!(
            "the service could not be reached: {}",
            e.without_url()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_each_tools_answer() {
        assert_eq!(
            claude(r#"{"loggedIn":true,"email":"me@example.com","authMethod":"claude.ai"}"#),
            Checked::In {
                identity: Some("me@example.com".into()),
                note: None,
            }
        );
        assert_eq!(
            claude(r#"{"loggedIn":false,"authMethod":"none"}"#),
            Checked::Out
        );
        assert!(matches!(claude("Usage: claude"), Checked::Unknown(_)));

        assert_eq!(gh(r#"{"hosts":{}}"#), Checked::Out);
        assert_eq!(
            gh(
                r#"{"hosts":{"github.com":[{"state":"success","active":true,"host":"github.com","login":"octo","tokenSource":"/x/hosts.yml","scopes":"gist, read:org, repo, workflow"}]}}"#
            ),
            Checked::In {
                identity: Some("octo".into()),
                note: None,
            }
        );
        // Signed in before Ophio asked for workflows: still in, with a note.
        assert_eq!(
            gh(
                r#"{"hosts":{"github.com":[{"state":"success","active":true,"host":"github.com","login":"octo","scopes":"gist, read:org, repo"}]}}"#
            ),
            Checked::In {
                identity: Some("octo".into()),
                note: Some(WORKFLOW_NOTE.into()),
            }
        );
        assert!(matches!(
            gh(
                r#"{"hosts":{"github.com":[{"state":"error","active":true,"host":"github.com","login":"octo"}]}}"#
            ),
            Checked::Attention(_)
        ));

        let tools = Tools::default();
        assert_eq!(codex("", "Not logged in\n", false, &tools), Checked::Out);
        assert!(matches!(
            codex("", "boom", false, &tools),
            Checked::Unknown(_)
        ));
    }
}
