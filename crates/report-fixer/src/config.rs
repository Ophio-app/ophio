// SPDX-License-Identifier: Apache-2.0
use report_pipeline::budget::BudgetConfig;
use serde::{Deserialize, Serialize};
use std::{
    io::Read,
    path::{Component, Path, PathBuf},
};

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    #[default]
    Openai,
    Anthropic,
}
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AgentCli {
    #[default]
    Codex,
    Claude,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub enabled: bool,
    pub intake_url: String,
    pub intake_token_file: PathBuf,
    pub provider: Provider,
    pub provider_key_file: PathBuf,
    pub agent_cli: AgentCli,
    pub agent_program: PathBuf,
    pub triage_model: String,
    pub fix_model: String,
    pub review_model: String,
    pub repository: String,
    pub mirror: PathBuf,
    pub state: PathBuf,
    pub shim: PathBuf,
    pub cargo_cache: PathBuf,
    pub npm_cache: PathBuf,
    pub poll_seconds: u64,
    pub paused: bool,
    pub budgets: BudgetConfig,
    pub limits: crate::execution::Limits,
    pub recovery: RecoveryConfig,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RecoveryConfig {
    pub question_timeout_seconds: u32,
    pub retry_limit: u32,
    pub retry_initial_seconds: u32,
    pub retry_max_seconds: u32,
}
impl Default for RecoveryConfig {
    fn default() -> Self {
        Self {
            question_timeout_seconds: 7 * 24 * 60 * 60,
            retry_limit: 5,
            retry_initial_seconds: 5,
            retry_max_seconds: 300,
        }
    }
}
impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: false,
            intake_url: String::new(),
            intake_token_file: PathBuf::new(),
            provider: Provider::default(),
            provider_key_file: PathBuf::new(),
            agent_cli: AgentCli::default(),
            agent_program: PathBuf::new(),
            triage_model: String::new(),
            fix_model: String::new(),
            review_model: String::new(),
            repository: String::new(),
            mirror: PathBuf::new(),
            state: PathBuf::new(),
            shim: PathBuf::new(),
            cargo_cache: PathBuf::new(),
            npm_cache: PathBuf::new(),
            poll_seconds: 30,
            paused: false,
            budgets: BudgetConfig::default(),
            limits: crate::execution::Limits::default(),
            recovery: RecoveryConfig::default(),
        }
    }
}
impl Config {
    /// Only reads explicitly configured paths. Never searches the person's accounts.
    pub fn readiness(&self) -> std::result::Result<(), &'static str> {
        if !self.enabled {
            return Err("disabled");
        }
        if self.limits.wall.is_zero()
            || self.limits.cpu_seconds == 0
            || self.limits.memory_bytes == 0
            || self.limits.output_bytes == 0
            || self.limits.disk_bytes < 65536
            || self.limits.disk_entries == 0
        {
            return Err("limits must be positive and disk_bytes at least 65536");
        }
        let parts: Vec<_> = self.repository.split('/').collect();
        if parts.len() != 2
            || parts.iter().any(|s| {
                s.is_empty()
                    || !s
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
            })
        {
            return Err("repository must be owner/name");
        }
        if !(5..=60).contains(&self.poll_seconds) {
            return Err("polling cadence must be 5 to 60 seconds");
        }
        if !self.mirror.is_absolute()
            || !self.state.is_absolute()
            || self.mirror.starts_with(&self.state)
            || self.state.starts_with(&self.mirror)
        {
            return Err("mirror and state need separate absolute paths");
        }
        for path in [&self.agent_program, &self.shim] {
            if !path.is_absolute() || !path.is_file() || std::fs::File::open(path).is_err() {
                return Err("a required credential or program file is missing or unreadable");
            }
            if path.starts_with(&self.mirror) {
                return Err("credentials and programs must be outside the mirror");
            }
        }
        if !self.cargo_cache.is_absolute()
            || !self.cargo_cache.is_dir()
            || !self.npm_cache.is_absolute()
            || !self.npm_cache.is_dir()
        {
            return Err("offline caches are missing");
        }
        self.credential_paths()?;
        if !matches!(
            (self.provider, self.agent_cli),
            (Provider::Openai, AgentCli::Codex) | (Provider::Anthropic, AgentCli::Claude)
        ) {
            return Err("agent and relay provider must match");
        }
        for m in [&self.triage_model, &self.fix_model, &self.review_model] {
            if m.is_empty()
                || m.len() > 128
                || !m
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-._:/".contains(&b))
            {
                return Err("stage models must be explicit provider model ids");
            }
        }
        intake::client::Client::new(&self.intake_url, "validation-only")
            .map_err(|_| "invalid intake URL")?;
        Ok(())
    }
}

// Resolve existing symlinks even when a future state or mirror does not exist yet.
fn resolved(path: &Path) -> std::io::Result<PathBuf> {
    let mut result = PathBuf::new();
    for part in path.components() {
        match part {
            Component::ParentDir => {
                result.pop();
            }
            Component::CurDir => {}
            other => {
                result.push(other.as_os_str());
                match std::fs::symlink_metadata(&result) {
                    Ok(_) => result = result.canonicalize()?,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e),
                }
            }
        }
    }
    Ok(result)
}

impl Config {
    fn credential_paths(&self) -> std::result::Result<(), &'static str> {
        let mut mounted = sandbox::runtime_paths()
            .iter()
            .map(PathBuf::from)
            .filter(|p| p.exists())
            .collect::<Vec<_>>();
        // All per-run workspaces, homes, output and provider state live under state.
        mounted.extend([
            self.mirror.clone(),
            self.cargo_cache.clone(),
            self.npm_cache.clone(),
        ]);
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default();
        mounted.extend(
            sandbox::program_paths(&self.agent_program, &home)
                .map_err(|_| "agent_program cannot be resolved")?,
        );
        mounted.push(self.shim.clone());
        let mounted = mounted
            .iter()
            .map(|p| resolved(p))
            .collect::<std::io::Result<Vec<_>>>()
            .map_err(|_| "sandbox mount paths cannot be resolved")?;
        let mirror = resolved(&self.mirror).map_err(|_| "mirror cannot be resolved")?;
        let state = resolved(&self.state).map_err(|_| "state cannot be resolved")?;
        if mirror.starts_with(&state) || state.starts_with(&mirror) {
            return Err("mirror and state need separate absolute paths");
        }
        for (path, error) in [
            (
                &self.intake_token_file,
                "intake_token_file must be private and outside sandbox mounts",
            ),
            (
                &self.provider_key_file,
                "provider_key_file must be private and outside sandbox mounts",
            ),
        ] {
            if !path.is_absolute() || !path.is_file() {
                return Err(error);
            }
            let path = path.canonicalize().map_err(|_| error)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                let metadata = path.metadata().map_err(|_| error)?;
                // A second name could be hidden anywhere inside a mounted tree.
                if metadata.nlink() != 1
                    || mounted.iter().any(|p| {
                        p.metadata().is_ok_and(|m| {
                            m.is_file() && m.dev() == metadata.dev() && m.ino() == metadata.ino()
                        })
                    })
                {
                    return Err(error);
                }
            }
            if path.starts_with(&state) || mounted.iter().any(|p| path.starts_with(p)) {
                return Err(error);
            }
            let mut text = String::new();
            std::fs::File::open(path)
                .map_err(|_| error)?
                .take(16385)
                .read_to_string(&mut text)
                .map_err(|_| error)?;
            if text.trim().is_empty() || text.len() > 16384 {
                return Err(error);
            }
        }
        // The signing key is implicit, rather than a configurable credential path.
        let signing = resolved(&state.join("report-pipeline.ed25519"))
            .map_err(|_| "state signing key cannot be resolved")?;
        if mounted.iter().any(|p| signing.starts_with(p)) {
            return Err("state signing key must be outside sandbox mounts");
        }
        Ok(())
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn fixture() -> (tempfile::TempDir, Config) {
        let root = tempfile::tempdir().unwrap();
        let path = root.path();
        for name in ["cargo", "npm"] {
            std::fs::create_dir(path.join(name)).unwrap();
        }
        for name in ["intake", "provider"] {
            std::fs::write(path.join(name), "obviously-fake-private-token").unwrap();
        }
        for name in ["agent", "shim"] {
            use std::os::unix::fs::PermissionsExt;
            std::fs::write(path.join(name), b"ELF fake executable").unwrap();
            std::fs::set_permissions(path.join(name), std::fs::Permissions::from_mode(0o700))
                .unwrap();
        }
        let config = Config {
            enabled: true,
            intake_url: "https://intake.example.invalid".into(),
            repository: "owner/repository".into(),
            intake_token_file: path.join("intake"),
            provider_key_file: path.join("provider"),
            agent_program: path.join("agent"),
            shim: path.join("shim"),
            cargo_cache: path.join("cargo"),
            npm_cache: path.join("npm"),
            mirror: path.join("mirror"),
            state: path.join("state"),
            triage_model: "fake-model".into(),
            fix_model: "fake-model".into(),
            review_model: "fake-model".into(),
            ..Default::default()
        };
        (root, config)
    }

    #[test]
    fn partial_limits_configuration_preserves_defaults_and_rejects_zero_budgets() {
        let parsed: Config =
            toml::from_str("[limits]\ndisk_bytes = 4294967296\ndisk_entries = 100000\n").unwrap();
        assert_eq!(parsed.limits.disk_bytes, 4 * 1024 * 1024 * 1024);
        assert_eq!(parsed.limits.disk_entries, 100000);
        assert_eq!(
            parsed.limits.memory_bytes,
            crate::execution::Limits::default().memory_bytes
        );
        let (_root, mut config) = fixture();
        config.limits.disk_bytes = 0;
        assert!(config.readiness().unwrap_err().starts_with("limits"));
    }
    #[test]
    fn credentials_outside_mounts_allow_future_state_and_mirror() {
        let (_root, config) = fixture();
        assert_eq!(config.readiness(), Ok(()));
    }

    #[test]
    fn every_credential_rejects_runtime_caches_programs_mirror_and_state() {
        for setting in ["intake_token_file", "provider_key_file"] {
            for location in [
                "runtime", "cargo", "npm", "program", "shim", "mirror", "state",
            ] {
                let (root, mut config) = fixture();
                let secret = match location {
                    "runtime" => PathBuf::from("/etc/hostname"),
                    "program" => config.agent_program.clone(),
                    "shim" => config.shim.clone(),
                    other => {
                        let parent = match other {
                            "cargo" => &config.cargo_cache,
                            "npm" => &config.npm_cache,
                            "mirror" => &config.mirror,
                            _ => &config.state,
                        };
                        std::fs::create_dir_all(parent).unwrap();
                        let secret = parent.join("fake-token");
                        std::fs::write(&secret, "obviously-fake-token").unwrap();
                        secret
                    }
                };
                let alias = root.path().join("secret-alias");
                symlink(&secret, &alias).unwrap();
                if setting == "intake_token_file" {
                    config.intake_token_file = alias;
                } else {
                    config.provider_key_file = alias;
                }
                assert!(
                    config.readiness().unwrap_err().starts_with(setting),
                    "{setting} {location}"
                );
            }
        }
    }

    #[test]
    fn credential_and_mount_dotdot_and_symlinks_are_resolved() {
        let (root, mut config) = fixture();
        let cache = config.cargo_cache.clone();
        std::fs::write(cache.join("fake-token"), "obviously-fake-token").unwrap();
        std::fs::create_dir(cache.join("child")).unwrap();
        config.intake_token_file = cache.join("child/../fake-token");
        symlink(&cache, root.path().join("cache-alias")).unwrap();
        config.cargo_cache = root.path().join("cache-alias");
        assert!(
            config
                .readiness()
                .unwrap_err()
                .starts_with("intake_token_file")
        );
    }

    #[test]
    fn hardlinked_credentials_are_refused_even_with_a_hidden_mounted_alias() {
        let (_root, config) = fixture();
        std::fs::hard_link(&config.provider_key_file, config.npm_cache.join("alias")).unwrap();
        assert!(
            config
                .readiness()
                .unwrap_err()
                .starts_with("provider_key_file")
        );
    }

    #[test]
    fn signing_key_and_canonical_state_mirror_overlap_are_refused() {
        let (root, mut config) = fixture();
        config.state = config.cargo_cache.join("state");
        assert_eq!(
            config.readiness(),
            Err("state signing key must be outside sandbox mounts")
        );
        config.state = root.path().join("state");
        std::fs::create_dir(&config.state).unwrap();
        symlink(&config.state, &config.mirror).unwrap();
        assert_eq!(
            config.readiness(),
            Err("mirror and state need separate absolute paths")
        );
    }
}
