// SPDX-License-Identifier: Apache-2.0
//! What each service is and how it signs in: the five that ship with Ophio,
//! and the ones the owner adds from the phone in the same shape.
use protocol::accounts::{AccountKind, AccountNeed, ServiceKind, ServiceProfile};
use protocol::capability::HarnessKind;
use serde::{Deserialize, Serialize};

/// How a profile signs in and is checked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Method {
    /// An agent's own login, kept in the agent's own store.
    Agent {
        harness: HarnessKind,
        sign_in: Vec<String>,
        status: Vec<String>,
        sign_out: Vec<String>,
        /// The tool opens the browser only when it has a terminal.
        pty: bool,
    },
    /// A command-line tool signed in with its own config folder per account.
    Cli {
        sign_in: Vec<String>,
        status: Vec<String>,
        sign_out: Option<Vec<String>>,
        /// Prints the tool's token, so it can be masked in what tasks say.
        token: Option<Vec<String>>,
        config_variable: String,
    },
    Oauth {
        authorize_url: String,
        token_url: String,
        revoke_url: Option<String>,
        scopes: Vec<String>,
        variable: String,
        /// Tasks also receive Google's authorized-user credentials file.
        google: bool,
    },
    ApiKey {
        variable: String,
        check_url: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Profile {
    pub id: String,
    pub name: String,
    pub method: Method,
    pub manage_url: Option<String>,
    #[serde(default)]
    pub builtin: bool,
}

impl Profile {
    pub fn kind(&self) -> AccountKind {
        match self.method {
            Method::Agent { .. } => AccountKind::Agent,
            Method::Cli { .. } => AccountKind::Cli,
            Method::Oauth { .. } => AccountKind::Oauth,
            Method::ApiKey { .. } => AccountKind::ApiKey,
        }
    }
    /// Variables tasks receive while the account is active.
    pub fn variables(&self) -> Vec<String> {
        match &self.method {
            Method::Agent { .. } => Vec::new(),
            Method::Cli {
                config_variable, ..
            } => vec![config_variable.clone()],
            Method::Oauth {
                variable, google, ..
            } => {
                let mut names = vec![variable.clone()];
                if *google {
                    names.push(GOOGLE_CREDENTIALS.into());
                }
                names
            }
            Method::ApiKey { variable, .. } => vec![variable.clone()],
        }
    }
    pub fn needs(&self) -> Option<AccountNeed> {
        match self.method {
            Method::ApiKey { .. } => Some(AccountNeed::Key),
            Method::Oauth { .. } => Some(AccountNeed::Client),
            _ => None,
        }
    }
    pub fn browser_sign_in(&self) -> bool {
        !matches!(self.method, Method::ApiKey { .. })
    }
}

pub const GOOGLE_CREDENTIALS: &str = "GOOGLE_APPLICATION_CREDENTIALS";

fn argv(line: &str) -> Vec<String> {
    line.split(' ').map(str::to_owned).collect()
}

/// The services that ship with Ophio.
pub fn builtins() -> Vec<Profile> {
    vec![
        Profile {
            id: "claude".into(),
            name: "Claude Code".into(),
            method: Method::Agent {
                harness: HarnessKind::ClaudeCode,
                sign_in: argv("claude auth login --claudeai"),
                status: argv("claude auth status"),
                sign_out: argv("claude auth logout"),
                pty: true,
            },
            manage_url: Some("https://claude.ai/settings/account".into()),
            builtin: true,
        },
        Profile {
            id: "codex".into(),
            name: "Codex".into(),
            method: Method::Agent {
                harness: HarnessKind::Codex,
                sign_in: argv("codex login"),
                status: argv("codex login status"),
                sign_out: argv("codex logout"),
                pty: false,
            },
            manage_url: Some("https://chatgpt.com/#settings/Security".into()),
            builtin: true,
        },
        Profile {
            id: "github".into(),
            name: "GitHub".into(),
            method: Method::Cli {
                // The token stays in the account's own private folder, which
                // tasks are pointed at, rather than in the keyring under a
                // name every gh on the computer shares.
                sign_in: argv(
                    "gh auth login --web --hostname github.com --git-protocol https --insecure-storage --scopes workflow",
                ),
                status: argv("gh auth status --json hosts --hostname github.com"),
                sign_out: Some(argv("gh auth logout --hostname github.com")),
                token: Some(argv("gh auth token --hostname github.com")),
                config_variable: "GH_CONFIG_DIR".into(),
            },
            manage_url: Some("https://github.com/settings/applications".into()),
            builtin: true,
        },
        Profile {
            id: "openrouter".into(),
            name: "OpenRouter".into(),
            method: Method::ApiKey {
                variable: "OPENROUTER_API_KEY".into(),
                check_url: Some("https://openrouter.ai/api/v1/key".into()),
            },
            manage_url: Some("https://openrouter.ai/settings/keys".into()),
            builtin: true,
        },
        Profile {
            id: "google".into(),
            name: "Google (Gmail)".into(),
            method: Method::Oauth {
                authorize_url: "https://accounts.google.com/o/oauth2/v2/auth".into(),
                token_url: "https://oauth2.googleapis.com/token".into(),
                revoke_url: Some("https://oauth2.googleapis.com/revoke".into()),
                scopes: vec![
                    "openid".into(),
                    "email".into(),
                    "https://www.googleapis.com/auth/gmail.modify".into(),
                ],
                variable: "GOOGLE_OAUTH_ACCESS_TOKEN".into(),
                google: true,
            },
            manage_url: Some("https://myaccount.google.com/permissions".into()),
            builtin: true,
        },
    ]
}

/// Variables a task needs from the computer or from Ophio itself, which no
/// added service may set.
fn reserved(variable: &str) -> bool {
    const NAMES: &[&str] = &[
        "PATH",
        "HOME",
        "USER",
        "LOGNAME",
        "SHELL",
        "TERM",
        "LANG",
        "PWD",
        "TMPDIR",
        "TEMP",
        "TMP",
        "BROWSER",
        "EDITOR",
        "VISUAL",
        "PAGER",
        "DISPLAY",
        "WAYLAND_DISPLAY",
        "SSH_AUTH_SOCK",
        "NODE_OPTIONS",
        "PYTHONPATH",
        "PYTHONSTARTUP",
        "RUSTC_WRAPPER",
        "GIT_SSH_COMMAND",
        "GIT_ASKPASS",
        "SSH_ASKPASS",
        "CODEX_HOME",
        "CLAUDE_CONFIG_DIR",
        "GH_CONFIG_DIR",
        "GH_TOKEN",
        "GITHUB_TOKEN",
        "COMSPEC",
        "SYSTEMROOT",
        "APPDATA",
        "LOCALAPPDATA",
        "USERPROFILE",
        GOOGLE_CREDENTIALS,
    ];
    NAMES.contains(&variable)
        || [
            "LD_",
            "DYLD_",
            "XDG_",
            "OPHIO_",
            "GIT_CONFIG",
            "BASH_",
            "PS",
        ]
        .iter()
        .any(|p| variable.starts_with(p))
}

fn variable(name: &str) -> Result<String, String> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_uppercase() || c == '_')
        && name
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
    if !ok {
        return Err(format!(
            "“{name}” is not a variable name. Use capital letters, digits and _, such as MY_SERVICE_KEY."
        ));
    }
    if reserved(name) {
        return Err(format!(
            "{name} is needed by the computer or by Ophio, so a service cannot set it."
        ));
    }
    Ok(name.to_owned())
}

fn https(url: &str, what: &str) -> Result<String, String> {
    match url::Url::parse(url) {
        Ok(u) if u.scheme() == "https" && u.host_str().is_some() && u.password().is_none() => {
            Ok(url.to_owned())
        }
        _ => Err(format!("The {what} must be an https:// address.")),
    }
}

fn command(line: &str, what: &str) -> Result<Vec<String>, String> {
    let words = shell_words::split(line)
        .map_err(|_| format!("The {what} command has an unclosed quote."))?;
    if words.is_empty() {
        return Err(format!("The {what} command is empty."));
    }
    if words.len() > 32 || line.len() > 1024 {
        return Err(format!("The {what} command is too long."));
    }
    Ok(words)
}

/// Two names that differ only in case, spacing or punctuation read as one.
fn same_name(a: &str, b: &str) -> bool {
    let letters = |s: &str| -> String {
        s.chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect()
    };
    let (x, y) = (letters(a), letters(b));
    if x.is_empty() || y.is_empty() {
        a.trim().to_lowercase() == b.trim().to_lowercase()
    } else {
        x == y
    }
}

fn slug(name: &str) -> String {
    name.to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

/// A slug for an added service's name, unique among `taken`.
fn id_for(name: &str, taken: &[String]) -> String {
    let base = slug(name);
    let base = if base.is_empty() {
        "service".to_owned()
    } else {
        base.chars().take(40).collect()
    };
    let mut id = base.clone();
    let mut n = 2;
    while taken.contains(&id) {
        id = format!("{base}-{n}");
        n += 1;
    }
    id
}

/// Checks what the owner described and turns it into a profile.
pub fn added(service: &ServiceProfile, existing: &[Profile]) -> Result<Profile, String> {
    let name = service.name.trim();
    if name.is_empty() || name.chars().count() > 60 || name.chars().any(char::is_control) {
        return Err("Give the service a name of up to 60 characters.".into());
    }
    if existing.iter().any(|p| same_name(&p.name, name)) {
        return Err(format!("There is already a service called {name}."));
    }
    let manage = |url: &Option<String>| {
        url.as_deref()
            .map(|u| https(u, "management page"))
            .transpose()
    };
    let (method, manage_url) = match &service.service {
        ServiceKind::ApiKey {
            variable: v,
            check_url,
            manage_url,
        } => (
            Method::ApiKey {
                variable: variable(v)?,
                check_url: check_url
                    .as_deref()
                    .map(|u| https(u, "check address"))
                    .transpose()?,
            },
            manage(manage_url)?,
        ),
        ServiceKind::Oauth {
            authorize_url,
            token_url,
            scopes,
            variable: v,
            manage_url,
        } => {
            if scopes.len() > 32
                || scopes
                    .iter()
                    .any(|s| s.is_empty() || s.len() > 256 || s.contains(char::is_whitespace))
            {
                return Err("List each scope on its own, without spaces.".into());
            }
            (
                Method::Oauth {
                    authorize_url: https(authorize_url, "sign-in address")?,
                    token_url: https(token_url, "token address")?,
                    revoke_url: None,
                    scopes: scopes.clone(),
                    variable: variable(v)?,
                    google: false,
                },
                manage(manage_url)?,
            )
        }
        ServiceKind::Cli {
            sign_in,
            status,
            sign_out,
            config_variable,
            manage_url,
        } => (
            Method::Cli {
                sign_in: command(sign_in, "sign-in")?,
                status: command(status, "status")?,
                sign_out: sign_out
                    .as_deref()
                    .filter(|s| !s.trim().is_empty())
                    .map(|s| command(s, "sign-out"))
                    .transpose()?,
                token: None,
                config_variable: variable(config_variable)?,
            },
            manage(manage_url)?,
        ),
    };
    let taken: Vec<String> = existing.iter().map(|p| p.id.clone()).collect();
    Ok(Profile {
        id: id_for(name, &taken),
        name: name.to_owned(),
        method,
        manage_url,
        builtin: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(name: &str, variable: &str) -> ServiceProfile {
        ServiceProfile {
            name: name.into(),
            service: ServiceKind::ApiKey {
                variable: variable.into(),
                check_url: None,
                manage_url: None,
            },
        }
    }

    #[test]
    fn an_added_service_cannot_take_what_the_computer_or_ophio_needs() {
        let existing = builtins();
        for bad in [
            "PATH",
            "LD_PRELOAD",
            "OPHIO_TOKEN",
            "GH_TOKEN",
            "lower",
            "1KEY",
            "",
        ] {
            assert!(added(&key("Thing", bad), &existing).is_err(), "{bad}");
        }
        assert!(added(&key("github", "X_KEY"), &existing).is_err());
        let ok = added(&key("Groq Cloud!", "GROQ_API_KEY"), &existing).unwrap();
        assert_eq!(ok.id, "groq-cloud");
        assert_eq!(ok.variables(), ["GROQ_API_KEY"]);
        let mut more = existing.clone();
        more.push(ok);
        assert_eq!(
            added(&key("Groq-Cloud", "GROQ2_API_KEY"), &more)
                .map(|p| p.id)
                .unwrap_err(),
            "There is already a service called Groq-Cloud."
        );
    }

    #[test]
    fn addresses_must_be_https_and_commands_are_split_without_a_shell() {
        let existing = builtins();
        let oauth = |url: &str| ServiceProfile {
            name: "Thing".into(),
            service: ServiceKind::Oauth {
                authorize_url: url.into(),
                token_url: "https://example.com/token".into(),
                scopes: vec!["read".into()],
                variable: "THING_TOKEN".into(),
                manage_url: None,
            },
        };
        assert!(added(&oauth("http://example.com/auth"), &existing).is_err());
        assert!(added(&oauth("https://u:p@example.com/auth"), &existing).is_err());
        assert!(added(&oauth("https://example.com/auth"), &existing).is_ok());
        let cli = ServiceProfile {
            name: "Cloud".into(),
            service: ServiceKind::Cli {
                sign_in: "gcloud auth login --brief 'a b'; rm -rf ~".into(),
                status: "gcloud auth list".into(),
                sign_out: Some(" ".into()),
                config_variable: "CLOUDSDK_CONFIG".into(),
                manage_url: None,
            },
        };
        let Method::Cli {
            sign_in, sign_out, ..
        } = added(&cli, &existing).unwrap().method
        else {
            panic!("not a CLI");
        };
        assert_eq!(
            sign_in,
            [
                "gcloud", "auth", "login", "--brief", "a b;", "rm", "-rf", "~"
            ]
        );
        assert_eq!(sign_out, None);
    }
}
