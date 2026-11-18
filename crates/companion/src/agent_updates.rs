// SPDX-License-Identifier: Apache-2.0
//! Øphio's own copies of Claude Code and Codex, kept at their makers' newest
//! release when the owner turns that on (see [`ports::agent_copy`]).
//!
//! Both makers publish each release's programs on the npm registry, one
//! package per system. A copy is the package for this computer's system,
//! downloaded only from the registry, checked against the checksum the
//! registry publishes, unpacked as plain files and folders inside the agent's
//! folder, and started once with `--version` before tasks use it. The copy
//! before stays beside it for tasks already running, until the next update.
use crate::{agent_release_notes, identity};
use async_trait::async_trait;
use base64::Engine as _;
use ports::agent_copy::{self, AgentUpdater, CopyState};
use protocol::capability::HarnessKind;
use sha2::{Digest, Sha512};
use std::{
    cell::Cell,
    io::Read,
    path::{Component, Path, PathBuf},
    rc::Rc,
    time::Duration,
};
use tokio::io::AsyncWriteExt;

pub const REGISTRY: &str = "https://registry.npmjs.org";
/// In an agent's folder: the version tasks started with before the current
/// one, kept until the next update.
const PREVIOUS: &str = "previous";
const DOCUMENT_LIMIT: usize = 1 << 20;
const DOWNLOAD_LIMIT: u64 = 768 << 20;
const UNPACKED_LIMIT: u64 = 2 << 30;
const ENTRY_LIMIT: usize = 4096;
/// What the archive may read between one entry's contents and the next:
/// headers, long names and extended headers, which it holds in memory.
const METADATA_LIMIT: u64 = 1 << 20;
const STALL: Duration = Duration::from_secs(60);
const START_LIMIT: Duration = Duration::from_secs(30);

const UNSUPPORTED: &str = "Øphio cannot update agents on this system. Update the agent on the computer using its maker's instructions.";
const PINNED: &str = "This agent uses a program chosen by path. Update that program on the computer; Øphio will not replace it.";
const CLAUDE_NOTES: &str =
    "https://raw.githubusercontent.com/anthropics/claude-code/main/CHANGELOG.md";
const CODEX_NOTES: &str = "https://api.github.com/repos/openai/codex/releases";
const UNREACHABLE: &str = "Couldn’t reach the update site to check for a new version.";
const UNREADABLE: &str = "The update site's answer about the newest version couldn't be read.";
const NO_BUILD: &str = "The newest version has no program for this computer's system.";
const INTERRUPTED: &str = "The download stopped before it finished.";
const TOO_LARGE: &str = "The download was larger than expected, so it wasn't used.";
const CHECKSUM: &str =
    "The download didn't match what the update site published, so it wasn't used.";
const UNSAFE: &str = "The download couldn't be unpacked safely, so it wasn't used.";
const NO_START: &str = "The new version didn't start when tried, so it wasn't used.";
const DISK: &str = "Couldn't save the new version on this computer.";

/// Keeps technical details in the computer's log, not the phone's message.
fn filesystem_error<'a, E: std::fmt::Debug>(
    operation: &'static str,
    path: &'a Path,
    problem: &'static str,
) -> impl FnOnce(E) -> &'static str + 'a {
    move |error| {
        tracing::warn!(operation, path = %path.display(), error = ?error, "agent update filesystem operation failed");
        problem
    }
}

/// The computer's system, as the makers name their packages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Platform {
    /// `linux`, `darwin` or `win32`.
    os: &'static str,
    /// `x64` or `arm64`.
    arch: &'static str,
    /// A Linux whose C library is musl, not glibc.
    musl: bool,
    /// Rust's target name, which Codex's package folders use.
    triple: &'static str,
    exe: &'static str,
}

impl Platform {
    pub fn detect() -> Option<Self> {
        let (arch, cpu) = match std::env::consts::ARCH {
            "x86_64" => ("x64", "x86_64"),
            "aarch64" => ("arm64", "aarch64"),
            _ => return None,
        };
        let (os, triple, exe) = match (std::env::consts::OS, cpu) {
            ("linux", "x86_64") => ("linux", "x86_64-unknown-linux-musl", ""),
            ("linux", _) => ("linux", "aarch64-unknown-linux-musl", ""),
            ("macos", "x86_64") => ("darwin", "x86_64-apple-darwin", ""),
            ("macos", _) => ("darwin", "aarch64-apple-darwin", ""),
            ("windows", "x86_64") => ("win32", "x86_64-pc-windows-msvc", ".exe"),
            ("windows", _) => ("win32", "aarch64-pc-windows-msvc", ".exe"),
            _ => return None,
        };
        let glibc = match cpu {
            "x86_64" => "/lib64/ld-linux-x86-64.so.2",
            _ => "/lib/ld-linux-aarch64.so.1",
        };
        let musl = os == "linux"
            && Path::new(&format!("/lib/ld-musl-{cpu}.so.1")).exists()
            && !Path::new(glibc).exists();
        Some(Self {
            os,
            arch,
            musl,
            triple,
            exe,
        })
    }
}

/// One agent Øphio can keep a copy of.
#[derive(Debug, Clone, Copy)]
pub struct Agent {
    pub kind: HarnessKind,
    /// Its folder under the copies' root.
    pub folder: &'static str,
    /// The npm package whose `latest` names each system's package.
    pub package: &'static str,
    /// Whether the computer has the agent's own program.
    pub installed: fn() -> bool,
}

impl Agent {
    pub const CLAUDE: Self = Self {
        kind: HarnessKind::ClaudeCode,
        folder: "claude",
        package: "@anthropic-ai/claude-code",
        installed: || harness_claude::installed().is_some(),
    };
    pub const CODEX: Self = Self {
        kind: HarnessKind::Codex,
        folder: "codex",
        package: "@openai/codex",
        installed: || harness_codex::installed().is_some(),
    };

    /// The key in `latest`'s `optionalDependencies` for this system.
    fn system_package(&self, platform: Platform) -> String {
        let musl = if platform.musl && self.kind == HarnessKind::ClaudeCode {
            "-musl"
        } else {
            ""
        };
        format!("{}-{}-{}{musl}", self.package, platform.os, platform.arch)
    }
    /// The program inside the unpacked package.
    fn program(&self, platform: Platform) -> String {
        match self.kind {
            HarnessKind::Codex => format!("vendor/{}/bin/codex{}", platform.triple, platform.exe),
            _ => format!("claude{}", platform.exe),
        }
    }
    fn env(&self) -> Vec<(String, String)> {
        match self.kind {
            HarnessKind::ClaudeCode => agent_copy::CLAUDE_ENV
                .iter()
                .map(|(key, value)| ((*key).into(), (*value).into()))
                .collect(),
            _ => Vec::new(),
        }
    }
}

#[derive(Debug)]
pub struct NpmUpdater {
    root: PathBuf,
    registry: String,
    platform: Option<Platform>,
    agents: Vec<Agent>,
    client: reqwest::Client,
    /// Serializes checks and downloads. Retention never depends on a restart.
    applied: tokio::sync::Mutex<()>,
    pinned: Vec<HarnessKind>,
    observed: std::sync::Mutex<Vec<(HarnessKind, String)>>,
    claude_notes: String,
    codex_notes: String,
    #[cfg(test)]
    probe: Option<Option<bool>>,
}

impl NpmUpdater {
    /// Copies kept under `root`, one folder per agent.
    pub fn new(root: PathBuf) -> Self {
        Self::with(
            root,
            REGISTRY.into(),
            Platform::detect(),
            vec![Agent::CLAUDE, Agent::CODEX],
        )
    }
    pub fn with(
        root: PathBuf,
        registry: String,
        platform: Option<Platform>,
        agents: Vec<Agent>,
    ) -> Self {
        let client = reqwest::Client::builder()
            .user_agent(concat!("ophio/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(15))
            // Registry programs and official notes never redirect to another source.
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap_or_default();
        Self {
            root,
            registry: registry.trim_end_matches('/').into(),
            platform,
            agents,
            client,
            applied: tokio::sync::Mutex::new(()),
            pinned: Vec::new(),
            observed: std::sync::Mutex::new(Vec::new()),
            claude_notes: CLAUDE_NOTES.into(),
            codex_notes: CODEX_NOTES.into(),
            #[cfg(test)]
            probe: None,
        }
    }
    /// Explicitly selected programs must never be replaced by a managed copy.
    pub fn with_pinned(mut self, pinned: Vec<HarnessKind>) -> Self {
        self.pinned = pinned;
        self
    }
    /// The folder holding `kind`'s copies, for the harness to start from.
    pub fn folder(&self, kind: HarnessKind) -> Option<PathBuf> {
        self.agents
            .iter()
            .find(|agent| agent.kind == kind)
            .map(|agent| self.root.join(agent.folder))
    }

    fn state(&self, agent: &Agent) -> Option<CopyState> {
        let pinned = self.pinned.contains(&agent.kind);
        let version = (!pinned)
            .then(|| held(&self.root.join(agent.folder)))
            .flatten();
        if !pinned
            && version.is_none()
            && !(agent.installed)()
            && self.observed_version(agent.kind).is_none()
        {
            return None;
        }
        Some(CopyState {
            harness: agent.kind,
            version,
            latest_version: None,
            update_supported: !pinned && self.platform.is_some(),
            released_models: Vec::new(),
            checked: false,
            problem: if pinned {
                Some(PINNED.into())
            } else if self.platform.is_none() {
                Some(UNSUPPORTED.into())
            } else {
                None
            },
        })
    }

    fn observed_version(&self, kind: HarnessKind) -> Option<String> {
        self.observed
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .find(|(harness, _)| *harness == kind)
            .and_then(|(_, raw)| agent_copy::normalized_version(kind, raw))
    }

    async fn agent(&self, agent: &Agent, install: bool) -> Option<CopyState> {
        let mut state = self.state(agent)?;
        // Updating pinned or unsupported installations cannot do anything useful.
        // A separate check can still read their latest release metadata.
        if install && !state.update_supported {
            return Some(state);
        }
        let latest = match self.document(agent.package, "latest").await {
            Ok(latest) => latest,
            Err(problem) => {
                state.problem = Some(problem.into());
                return Some(state);
            }
        };
        let Some(version) = text(&latest, &["version"])
            .and_then(|v| agent_copy::normalized_version(agent.kind, &v))
        else {
            state.problem = Some(UNREADABLE.into());
            return Some(state);
        };
        state.latest_version = Some(version.clone());
        state.checked = true;
        let installed = state
            .version
            .clone()
            .or_else(|| self.observed_version(agent.kind));
        if !install && let Some(installed) = installed.as_deref() {
            state.released_models = self.released_models(agent.kind, installed, &version).await;
        }
        if !install
            || installed
                .as_deref()
                .and_then(version_numbers)
                .zip(version_numbers(&version))
                .is_some_and(|(current, newest)| current > newest)
        {
            return Some(state);
        }
        let platform = self.platform.expect("supported install has a platform");
        let folder = self.root.join(agent.folder);
        if identity::private_dir(&folder)
            .map_err(filesystem_error("secure agent directory", &folder, DISK))
            .is_err()
        {
            state.problem = Some(DISK.into());
            return Some(state);
        }
        // A completed unsuccessful or wrong-version response warrants a
        // same-version repair. A timeout or launch error is inconclusive.
        // Neither result removes the selected copy before a replacement wins.
        let broken = if let Some(held) = state.version.as_deref() {
            self.still_starts(agent, &folder, held).await == Some(false)
        } else {
            false
        };
        if state.version.as_deref() == Some(&version) && !broken {
            return Some(state);
        }
        match self
            .install(
                agent,
                platform,
                &folder,
                state.version.as_deref(),
                &latest,
                &version,
            )
            .await
        {
            Ok(version) => {
                state.version = Some(version);
                state.released_models.clear();
            }
            Err(problem) => state.problem = Some(problem.into()),
        }
        Some(state)
    }

    async fn released_models(
        &self,
        kind: HarnessKind,
        installed: &str,
        latest: &str,
    ) -> Vec<String> {
        // Unknown, equal or newer installed versions cannot justify a model claim.
        if !matches!((version_numbers(installed), version_numbers(latest)), (Some(a), Some(b)) if b > a)
        {
            return Vec::new();
        }
        if kind == HarnessKind::Codex {
            // GitHub release JSON includes assets. Read at most thirty releases
            // (12 MiB total), with one deadline for the entire lookback.
            return tokio::time::timeout(
                Duration::from_secs(30),
                self.codex_models(installed, latest),
            )
            .await
            .unwrap_or_default();
        }
        if kind != HarnessKind::ClaudeCode {
            return Vec::new();
        }
        let Ok(body) = self
            .bounded_document(&self.claude_notes, agent_release_notes::LIMIT)
            .await
        else {
            return Vec::new();
        };
        agent_release_notes::claude(&body, installed, latest)
    }

    async fn codex_models(&self, installed: &str, latest: &str) -> Vec<String> {
        let mut models = Vec::new();
        let baseline = version_numbers(installed);
        for page in 1..=6 {
            let url = format!("{}?per_page=5&page={page}", self.codex_notes);
            let Ok(body) = self
                .bounded_document(&url, agent_release_notes::LIMIT)
                .await
            else {
                return Vec::new();
            };
            let Ok(releases) = serde_json::from_slice::<Vec<serde_json::Value>>(&body) else {
                return Vec::new();
            };
            for id in agent_release_notes::codex(&body, installed, latest) {
                if !models.contains(&id) && models.len() < 16 {
                    models.push(id);
                }
            }
            // Release lists are newest first. Reaching the known installed
            // stable release ends the lookback; unknown tags never establish it.
            let reached = releases.iter().any(|release| {
                release.get("draft").and_then(|v| v.as_bool()) == Some(false)
                    && release.get("prerelease").and_then(|v| v.as_bool()) == Some(false)
                    && release
                        .get("tag_name")
                        .and_then(|v| v.as_str())
                        .and_then(|tag| tag.strip_prefix("rust-v"))
                        .and_then(version_numbers)
                        .zip(baseline)
                        .is_some_and(|(version, before)| version <= before)
            });
            if releases.len() < 5 || reached || models.len() == 16 {
                break;
            }
        }
        models
    }

    async fn still_starts(&self, agent: &Agent, folder: &Path, version: &str) -> Option<bool> {
        #[cfg(test)]
        if let Some(result) = self.probe {
            return result;
        }
        match agent_copy::current(folder) {
            Some(program) => starts(&program, &agent.env(), version, agent.kind).await,
            None => Some(false),
        }
    }

    /// Brings `folder` to the newest release, returning its version.
    async fn install(
        &self,
        agent: &Agent,
        platform: Platform,
        folder: &Path,
        held: Option<&str>,
        latest: &serde_json::Value,
        version: &str,
    ) -> Result<String, &'static str> {
        let key = agent.system_package(platform);
        let spec = text(latest, &["optionalDependencies", &key]).ok_or(NO_BUILD)?;
        // Codex's are aliases, `npm:@openai/codex@0.1.0-linux-x64`.
        let (name, package_version) = match spec.strip_prefix("npm:") {
            Some(alias) => alias.rsplit_once('@').ok_or(UNREADABLE)?,
            None => (key.as_str(), spec.as_str()),
        };
        let ours = name == agent.package
            || name
                .strip_prefix(agent.package)
                .and_then(|rest| rest.strip_prefix('-'))
                .is_some_and(plain);
        if !ours || !plain(package_version) {
            return Err(UNREADABLE);
        }
        let package = self.document(name, package_version).await?;
        let tarball = text(&package, &["dist", "tarball"]).ok_or(UNREADABLE)?;
        if !tarball.starts_with(&format!("{}/", self.registry)) {
            return Err(UNREADABLE);
        }
        let digest = text(&package, &["dist", "integrity"])
            .and_then(|integrity| sha512(&integrity))
            .ok_or(UNREADABLE)?;
        let work = tempfile::Builder::new()
            .prefix(".download-")
            .tempdir_in(folder)
            .map_err(filesystem_error("create download directory", folder, DISK))?;
        let archive = work.path().join("package.tgz");
        self.download(&tarball, &archive, &digest).await?;
        let unpacked = work.path().join("unpacked");
        let into = unpacked.clone();
        tokio::task::spawn_blocking(move || unpack(&archive, &into))
            .await
            .map_err(|_| UNSAFE)??;
        let relative = agent.program(platform);
        let program = unpacked.join(&relative);
        let parent = program.parent().ok_or(UNSAFE)?;
        if !program.is_file() {
            return Err(UNSAFE);
        }
        let size = std::fs::metadata(&program)
            .map_err(filesystem_error(
                "inspect extracted program",
                &program,
                UNSAFE,
            ))?
            .len();
        std::fs::write(parent.join(agent_copy::MARKER), size.to_string()).map_err(
            filesystem_error(
                "write program marker",
                &parent.join(agent_copy::MARKER),
                DISK,
            ),
        )?;
        if starts(&program, &agent.env(), version, agent.kind).await != Some(true) {
            return Err(NO_START);
        }
        activate(folder, &unpacked, &relative, version, held)?;
        tracing::info!(agent = agent.folder, version, "agent copy updated");
        Ok(version.into())
    }

    async fn document(&self, name: &str, version: &str) -> Result<serde_json::Value, &'static str> {
        let url = format!("{}/{name}/{version}", self.registry);
        let body = self.bounded_document(&url, DOCUMENT_LIMIT).await?;
        serde_json::from_slice(&body).map_err(|_| UNREADABLE)
    }

    async fn bounded_document(&self, url: &str, limit: usize) -> Result<Vec<u8>, &'static str> {
        let mut response = self
            .client
            .get(url)
            .header("accept", "application/json")
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .map_err(|_| UNREACHABLE)?;
        if !response.status().is_success() {
            return Err(UNREACHABLE);
        }
        if response
            .content_length()
            .is_some_and(|size| size > limit as u64)
        {
            return Err(UNREADABLE);
        }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| UNREACHABLE)? {
            if chunk.len() > limit.saturating_sub(body.len()) {
                return Err(UNREADABLE);
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    }

    async fn download(&self, url: &str, to: &Path, digest: &[u8]) -> Result<(), &'static str> {
        let mut response = tokio::time::timeout(STALL, self.client.get(url).send())
            .await
            .map_err(|_| UNREACHABLE)?
            .map_err(|_| UNREACHABLE)?;
        if !response.status().is_success() {
            return Err(UNREACHABLE);
        }
        if response
            .content_length()
            .is_some_and(|length| length > DOWNLOAD_LIMIT)
        {
            return Err(TOO_LARGE);
        }
        let mut file = tokio::fs::File::create(to).await.map_err(filesystem_error(
            "create download",
            to,
            DISK,
        ))?;
        let mut hash = Sha512::new();
        let mut total = 0u64;
        loop {
            let chunk = tokio::time::timeout(STALL, response.chunk())
                .await
                .map_err(|_| INTERRUPTED)?
                .map_err(|_| INTERRUPTED)?;
            let Some(chunk) = chunk else { break };
            total += chunk.len() as u64;
            if total > DOWNLOAD_LIMIT {
                return Err(TOO_LARGE);
            }
            hash.update(&chunk);
            file.write_all(&chunk)
                .await
                .map_err(filesystem_error("write download", to, DISK))?;
        }
        file.sync_all()
            .await
            .map_err(filesystem_error("flush download", to, DISK))?;
        if hash.finalize().as_slice() != digest {
            return Err(CHECKSUM);
        }
        Ok(())
    }
}

#[async_trait]
impl AgentUpdater for NpmUpdater {
    fn supported(&self) -> bool {
        self.platform.is_some()
    }
    fn observe_versions(&self, versions: &[(HarnessKind, String)]) {
        *self.observed.lock().unwrap_or_else(|e| e.into_inner()) = versions.to_vec();
    }
    async fn check(&self) -> Vec<CopyState> {
        let _one = self.applied.lock().await;
        let mut states = Vec::new();
        for agent in &self.agents {
            states.extend(self.agent(agent, false).await);
        }
        states
    }
    async fn update(&self, kind: HarnessKind) -> Vec<CopyState> {
        let _one = self.applied.lock().await;
        let Some(agent) = self.agents.iter().find(|agent| agent.kind == kind) else {
            return Vec::new();
        };
        self.agent(agent, true).await.into_iter().collect()
    }
    async fn apply(&self, keep: bool) -> Vec<CopyState> {
        let _one = self.applied.lock().await;
        if !keep {
            return self
                .agents
                .iter()
                .filter_map(|agent| self.state(agent))
                .collect();
        }
        let mut states = Vec::new();
        for agent in &self.agents {
            states.extend(self.agent(agent, true).await);
        }
        states
    }
    fn program(&self, kind: HarnessKind) -> Option<(PathBuf, Vec<(String, String)>)> {
        if self.pinned.contains(&kind) {
            return None;
        }
        let agent = self.agents.iter().find(|agent| agent.kind == kind)?;
        let program = agent_copy::current(&self.root.join(agent.folder))?;
        Some((program, agent.env()))
    }
}

/// Publishes only a verified package. The current pointer is the commit point:
/// until its atomic replacement, neither retained pointer nor package changes.
fn activate(
    folder: &Path,
    unpacked: &Path,
    relative: &str,
    version: &str,
    held: Option<&str>,
) -> Result<(), &'static str> {
    let version_dir = folder.join(version);
    let (target, named_program) = if version_dir.try_exists().map_err(filesystem_error(
        "inspect version directory",
        &version_dir,
        DISK,
    ))? {
        // A repair must not remove the old executable, even at the same
        // version. Keeping the version as the first component also lets older
        // companions read this pointer without a format change.
        identity::private_dir(&version_dir).map_err(filesystem_error(
            "secure repair directory",
            &version_dir,
            DISK,
        ))?;
        let generation = format!(".copy-{}", protocol::ids::HostId::new());
        (
            version_dir.join(&generation),
            format!("{version}/{generation}/{relative}"),
        )
    } else {
        (version_dir, format!("{version}/{relative}"))
    };
    std::fs::rename(unpacked, &target).map_err(filesystem_error(
        "move extracted version",
        &target,
        DISK,
    ))?;
    let named = folder.join(".current.new");
    let previous = folder.join(".previous.new");
    let switch = (|| {
        // Stage retention before switching, but leave PREVIOUS itself alone.
        if let Some(before) = held {
            std::fs::write(&previous, before).map_err(filesystem_error(
                "stage previous pointer",
                &previous,
                DISK,
            ))?;
        }
        std::fs::write(&named, named_program).map_err(filesystem_error(
            "write current pointer",
            &named,
            DISK,
        ))?;
        std::fs::rename(&named, folder.join(agent_copy::CURRENT)).map_err(filesystem_error(
            "replace current pointer",
            &folder.join(agent_copy::CURRENT),
            DISK,
        ))
    })();
    if let Err(problem) = switch {
        // This directory belongs to this attempt alone, never a retained one.
        let _ = std::fs::remove_dir_all(&target).map_err(filesystem_error(
            "remove unselected replacement",
            &target,
            DISK,
        ));
        let _ = std::fs::remove_file(&previous);
        let _ = std::fs::remove_file(&named);
        return Err(problem);
    }
    // The new copy is now fully installed and selected. If retention cannot
    // be recorded, keep every old directory instead of risking its deletion.
    // The installation itself succeeded; do not report the old version as
    // selected or attempt a fallible rollback after the commit point.
    if held.is_some()
        && std::fs::rename(&previous, folder.join(PREVIOUS))
            .map_err(filesystem_error(
                "replace previous pointer; old copies retained",
                &folder.join(PREVIOUS),
                DISK,
            ))
            .is_err()
    {
        return Ok(());
    }
    let previous_version = match held {
        Some(version) => Some(version.to_owned()),
        None => match std::fs::read_to_string(folder.join(PREVIOUS)) {
            Ok(version) if plain(version.trim()) => Some(version.trim().to_owned()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            // Missing or unreadable retention information never authorizes
            // deleting an old package. The new copy is already selected.
            result => {
                tracing::warn!(path = %folder.join(PREVIOUS).display(), error = ?result, "cannot read previous version; old agent copies retained");
                return Ok(());
            }
        },
    };
    prune(folder, version, previous_version.as_deref());
    Ok(())
}

/// Stable release ordering, never lexical (2.10 is newer than 2.9).
fn version_numbers(version: &str) -> Option<[u64; 3]> {
    let version = agent_copy::normalized_version(HarnessKind::Codex, version)?;
    let numbers: Vec<_> = version
        .split('.')
        .map(str::parse::<u64>)
        .collect::<Result<_, _>>()
        .ok()?;
    numbers.try_into().ok()
}

/// The version `folder`'s current copy is, while it has one.
fn held(folder: &Path) -> Option<String> {
    agent_copy::current(folder)?;
    let named = std::fs::read_to_string(folder.join(agent_copy::CURRENT)).ok()?;
    let version = named.split('/').next()?.trim();
    plain(version).then(|| version.into())
}

/// Removes everything in `folder` but the current and previous copies.
/// What can't go now (a program still running, on Windows) goes next time.
fn prune(folder: &Path, current: &str, previous: Option<&str>) {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return;
    };
    // Use the versions committed by activate, never fallible reads of their
    // pointers or executables: a transient read error must not delete them.
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == agent_copy::CURRENT
            || name == PREVIOUS
            || name == current
            || Some(name.as_str()) == previous
        {
            continue;
        }
        let path = entry.path();
        let _ = match entry.file_type() {
            Ok(kind) if kind.is_dir() => std::fs::remove_dir_all(&path),
            _ => std::fs::remove_file(&path),
        };
    }
}

/// A version or package name part with nothing that could name a path.
fn plain(version: &str) -> bool {
    !version.is_empty()
        && version.len() <= 64
        && !version.starts_with('.')
        && version
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+' | '_'))
}

fn text(document: &serde_json::Value, path: &[&str]) -> Option<String> {
    path.iter()
        .try_fold(document, |value, key| value.get(*key))?
        .as_str()
        .map(Into::into)
}

/// The SHA-512 digest in an npm `integrity` value.
fn sha512(integrity: &str) -> Option<Vec<u8>> {
    integrity.split_whitespace().find_map(|part| {
        let encoded = part.strip_prefix("sha512-")?;
        let digest = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .ok()?;
        (digest.len() == 64).then_some(digest)
    })
}

/// Unpacks an npm package into `into`: plain files and folders only, all
/// inside it, within the size and count limits.
fn unpack(archive: &Path, into: &Path) -> Result<(), &'static str> {
    let file =
        std::fs::File::open(archive).map_err(filesystem_error("open archive", archive, DISK))?;
    let allowance = Rc::new(Cell::new(0));
    let mut tar = tar::Archive::new(Budgeted {
        inner: flate2::read::GzDecoder::new(file),
        allowance: allowance.clone(),
    });
    identity::private_dir(into).map_err(filesystem_error(
        "secure extraction directory",
        into,
        DISK,
    ))?;
    let mut total = 0u64;
    let mut entries = tar.entries().map_err(|_| UNSAFE)?;
    for count in 0.. {
        allowance.set(METADATA_LIMIT);
        let Some(entry) = entries.next() else { break };
        if count >= ENTRY_LIMIT {
            return Err(UNSAFE);
        }
        let mut entry = entry.map_err(|_| UNSAFE)?;
        let kind = entry.header().entry_type();
        if kind.is_pax_global_extensions() {
            continue;
        }
        let path = entry.path().map_err(|_| UNSAFE)?.into_owned();
        let mut parts = path.components();
        if parts.next() != Some(Component::Normal("package".as_ref())) {
            return Err(UNSAFE);
        }
        let relative = parts.as_path();
        if !relative
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
        {
            return Err(UNSAFE);
        }
        if relative.as_os_str().is_empty() {
            continue;
        }
        let target = into.join(relative);
        if kind.is_dir() {
            identity::private_dir(&target).map_err(filesystem_error(
                "secure archive directory",
                &target,
                DISK,
            ))?;
            continue;
        }
        if !kind.is_file() {
            return Err(UNSAFE);
        }
        let size = entry.size();
        total += size;
        if total > UNPACKED_LIMIT {
            return Err(UNSAFE);
        }
        if let Some(parent) = target.parent() {
            identity::private_dir(parent).map_err(filesystem_error(
                "secure archive parent",
                parent,
                DISK,
            ))?;
        }
        let executable = entry.header().mode().is_ok_and(|mode| mode & 0o111 != 0);
        let mut out = create(&target, executable).map_err(filesystem_error(
            "create archive file",
            &target,
            UNSAFE,
        ))?;
        allowance.set(size);
        let copied = std::io::copy(&mut (&mut entry).take(size), &mut out)
            .map_err(filesystem_error("extract archive file", &target, UNSAFE))?;
        if copied != size {
            return Err(UNSAFE);
        }
    }
    Ok(())
}

/// Reads no more than `allowance` lets it, which [`unpack`] sets before each
/// step, so an archive can't make the reader hold more than its limits.
pub(crate) struct Budgeted<R> {
    pub(crate) inner: R,
    pub(crate) allowance: Rc<Cell<u64>>,
}

impl<R: Read> Read for Budgeted<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let left = self.allowance.get();
        if left == 0 && !buf.is_empty() {
            return Err(std::io::Error::other("more than the archive's limits"));
        }
        let most = buf.len().min(usize::try_from(left).unwrap_or(usize::MAX));
        let read = self.inner.read(&mut buf[..most])?;
        self.allowance.set(left - read as u64);
        Ok(read)
    }
}

/// A new file only the owner can use, which can't already exist.
fn create(path: &Path, executable: bool) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(if executable { 0o700 } else { 0o600 });
    }
    #[cfg(not(unix))]
    let _ = executable;
    options.open(path)
}

/// Whether `program --version` answers with `version` in time. An unavailable
/// check is inconclusive, not proof that a held copy is broken.
async fn starts(
    program: &Path,
    env: &[(String, String)],
    version: &str,
    kind: HarnessKind,
) -> Option<bool> {
    // A program just written can be briefly busy (`ETXTBSY`) while another
    // thread's child still holds the file from before its exec.
    for _ in 0..3 {
        if kind == HarnessKind::ClaudeCode {
            // The harness shares its check lock with account and model probes,
            // and waits for credential persistence before releasing it.
            if let Some(found) = harness_claude::version(program).await {
                return Some(
                    found.status.success()
                        && String::from_utf8_lossy(&found.stdout).contains(version),
                );
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
            continue;
        }
        let mut command = tokio::process::Command::new(program);
        command
            .arg("--version")
            .envs(env.iter().map(|(key, value)| (key, value)))
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        let output = containment::inherit::tokio_output(&mut command);
        match tokio::time::timeout(START_LIMIT, output).await {
            Ok(Ok(output)) => {
                return Some(
                    output.status.success()
                        && String::from_utf8_lossy(&output.stdout).contains(version),
                );
            }
            Ok(Err(_)) => tokio::time::sleep(Duration::from_millis(200)).await,
            Err(_) => return None,
        }
    }
    None
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use axum::{Router, extract::State, http::StatusCode, routing::get};
    use std::{
        collections::HashMap,
        sync::{Arc, Mutex},
    };

    const LINUX: Platform = Platform {
        os: "linux",
        arch: "x64",
        musl: false,
        triple: "x86_64-unknown-linux-musl",
        exe: "",
    };
    const CLAUDE: Agent = Agent {
        kind: HarnessKind::ClaudeCode,
        folder: "claude",
        package: "@anthropic-ai/claude-code",
        installed: || true,
    };
    const CODEX: Agent = Agent {
        kind: HarnessKind::Codex,
        folder: "codex",
        package: "@openai/codex",
        installed: || false,
    };

    type Files = Arc<Mutex<HashMap<String, Vec<u8>>>>;

    /// A registry serving what `files` holds by path.
    async fn registry() -> (String, Files) {
        let files: Files = Arc::default();
        let app = Router::new()
            .fallback(get(
                |State(files): State<Files>, uri: axum::http::Uri| async move {
                    match files.lock().unwrap().get(&uri.to_string()) {
                        Some(body) => (StatusCode::OK, body.clone()),
                        None => (StatusCode::NOT_FOUND, Vec::new()),
                    }
                },
            ))
            .with_state(files.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (base, files)
    }

    fn tarball(entries: &[(&str, &[u8], u32)]) -> Vec<u8> {
        let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        let mut tar = tar::Builder::new(gz);
        for (path, body, mode) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(body.len() as u64);
            header.set_mode(*mode);
            header.set_entry_type(tar::EntryType::Regular);
            tar.append_data(&mut header, path, *body).unwrap();
        }
        tar.into_inner().unwrap().finish().unwrap()
    }

    /// Publishes Claude Code `version` whose program prints `prints`.
    fn publish(base: &str, files: &Files, version: &str, prints: &str) -> Vec<u8> {
        let script = format!("#!/bin/sh\necho '{prints} (Claude Code)'\n");
        let body = tarball(&[("package/claude", script.as_bytes(), 0o755)]);
        publish_tarball(base, files, version, &body, &body);
        body
    }
    fn publish_tarball(base: &str, files: &Files, version: &str, body: &[u8], hashed: &[u8]) {
        let integrity = format!(
            "sha512-{}",
            base64::engine::general_purpose::STANDARD.encode(Sha512::digest(hashed))
        );
        let name = "@anthropic-ai/claude-code-linux-x64";
        let path = format!("/{name}/-/claude-code-linux-x64-{version}.tgz");
        let latest = serde_json::json!({
            "version": version,
            "optionalDependencies": { name: version },
        });
        let package = serde_json::json!({
            "dist": { "tarball": format!("{base}{path}"), "integrity": integrity },
        });
        let mut files = files.lock().unwrap();
        files.insert(
            "/@anthropic-ai/claude-code/latest".into(),
            latest.to_string().into(),
        );
        files.insert(format!("/{name}/{version}"), package.to_string().into());
        files.insert(path, body.to_vec());
    }

    fn fresh(root: &Path, base: &str) -> NpmUpdater {
        let mut updater =
            NpmUpdater::with(root.into(), base.into(), Some(LINUX), vec![CLAUDE, CODEX]);
        updater.claude_notes = format!("{base}/claude-notes");
        updater.codex_notes = format!("{base}/codex-notes");
        updater
    }
    fn versions(root: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(root.join("claude"))
            .unwrap()
            .flatten()
            .filter(|entry| entry.file_type().unwrap().is_dir())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[tokio::test]
    async fn an_unavailable_claude_version_check_is_not_a_broken_copy() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            starts(
                &dir.path().join("missing"),
                &[],
                "1.0.0",
                HarnessKind::ClaudeCode
            )
            .await,
            None
        );
        let program = dir.path().join("wrong-version");
        test_support::executable(&program, "#!/bin/sh\nprintf '2.0.0\\n'\n").unwrap();
        assert_eq!(
            starts(&program, &[], "1.0.0", HarnessKind::ClaudeCode).await,
            Some(false)
        );
    }

    #[tokio::test]
    async fn codex_probe_launch_errors_are_inconclusive_but_failed_responses_are_not() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            starts(
                &dir.path().join("missing"),
                &[],
                "1.0.0",
                HarnessKind::Codex
            )
            .await,
            None
        );
        let program = dir.path().join("probe");
        test_support::executable(&program, "#!/bin/sh\nexit 1\n").unwrap();
        assert_eq!(
            starts(&program, &[], "1.0.0", HarnessKind::Codex).await,
            Some(false)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn codex_probe_timeouts_are_inconclusive() {
        let dir = tempfile::tempdir().unwrap();
        let program = dir.path().join("probe");
        let before = tokio::time::Instant::now();
        test_support::executable(&program, "#!/bin/sh\nexec sleep 60\n").unwrap();
        assert_eq!(
            starts(&program, &[], "1.0.0", HarnessKind::Codex).await,
            None
        );
        assert!(before.elapsed() >= START_LIMIT);
    }

    #[tokio::test]
    async fn keeps_one_copy_up_to_date_and_retains_it_when_turned_off() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("agents");
        let (base, files) = registry().await;
        publish(&base, &files, "1.0.0", "1.0.0");
        let updater = fresh(&root, &base);

        let states = updater.apply(true).await;
        assert_eq!(
            states,
            vec![CopyState {
                harness: HarnessKind::ClaudeCode,
                version: Some("1.0.0".into()),
                latest_version: Some("1.0.0".into()),
                update_supported: true,
                released_models: Vec::new(),
                checked: true,
                problem: None,
            }],
            "Codex isn't installed, so it gets no copy"
        );
        let (program, env) = updater.program(HarnessKind::ClaudeCode).unwrap();
        assert_eq!(program, root.join("claude/1.0.0/claude"));
        assert!(agent_copy::is_copy(&program));
        assert!(env.contains(&("DISABLE_AUTOUPDATER".into(), "1".into())));
        assert_eq!(updater.program(HarnessKind::Codex), None);

        // Nothing new: nothing downloaded again.
        files
            .lock()
            .unwrap()
            .retain(|path, _| !path.ends_with(".tgz"));
        assert_eq!(
            updater.apply(true).await[0].version.as_deref(),
            Some("1.0.0")
        );

        publish(&base, &files, "1.1.0", "1.1.0");
        assert_eq!(
            updater.apply(true).await[0].version.as_deref(),
            Some("1.1.0")
        );
        assert_eq!(
            versions(&root),
            ["1.0.0", "1.1.0"],
            "the one before stays for running tasks"
        );
        publish(&base, &files, "1.2.0", "1.2.0");
        updater.apply(true).await;
        assert_eq!(versions(&root), ["1.1.0", "1.2.0"]);
        assert_eq!(
            updater.program(HarnessKind::ClaudeCode).unwrap().0,
            root.join("claude/1.2.0/claude")
        );

        files.lock().unwrap().clear();
        let state = updater.apply(false).await.remove(0);
        assert_eq!(state.version.as_deref(), Some("1.2.0"));
        assert!(!state.checked);
        assert_eq!(state.problem, None, "turning off uses no network");
        assert_eq!(
            updater.program(HarnessKind::ClaudeCode).unwrap().0,
            root.join("claude/1.2.0/claude")
        );
        assert_eq!(versions(&root), ["1.1.0", "1.2.0"]);
        // Restarting with updates off retains both copies too.
        fresh(&root, &base).apply(false).await;
        assert_eq!(versions(&root), ["1.1.0", "1.2.0"]);
    }

    #[tokio::test]
    async fn a_bad_download_or_program_leaves_the_copy_in_use() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("agents");
        let (base, files) = registry().await;
        publish(&base, &files, "1.0.0", "1.0.0");
        let updater = fresh(&root, &base);
        updater.apply(true).await;

        // A copy damaged in place is replaced at the next check.
        let program = root.join("claude/1.0.0/claude");
        let script = std::fs::read_to_string(&program).unwrap();
        std::fs::write(&program, script.replace("echo", "exit")).unwrap();
        let state = updater.apply(true).await.remove(0);
        assert_eq!(
            (state.version.as_deref(), state.problem),
            (Some("1.0.0"), None)
        );
        let repaired = updater.program(HarnessKind::ClaudeCode).unwrap().0;
        assert_ne!(
            repaired, program,
            "repair never overwrites the selected file"
        );
        assert_eq!(std::fs::read_to_string(&repaired).unwrap(), script);
        assert_eq!(
            std::fs::read_to_string(&program).unwrap(),
            script.replace("echo", "exit")
        );

        let good = tarball(&[("package/claude", b"#!/bin/sh\necho 2.0.0\n", 0o755)]);
        publish_tarball(&base, &files, "2.0.0", &good, b"something else");
        let state = updater.apply(true).await.remove(0);
        assert_eq!(state.version.as_deref(), Some("1.0.0"));
        assert!(
            state.checked,
            "metadata check succeeded even though download failed"
        );
        assert_eq!(state.latest_version.as_deref(), Some("2.0.0"));
        assert_eq!(state.problem.as_deref(), Some(CHECKSUM));

        publish(&base, &files, "2.0.0", "1.9.9");
        let state = updater.apply(true).await.remove(0);
        assert_eq!(state.problem.as_deref(), Some(NO_START));
        assert_eq!(
            updater.program(HarnessKind::ClaudeCode).unwrap().0,
            repaired
        );
        assert_eq!(
            versions(&root),
            ["1.0.0"],
            "nothing half-made is left behind"
        );

        for escape in ["package/../claude", "/etc/claude", "other/claude"] {
            let body = tarball_raw(escape);
            publish_tarball(&base, &files, "2.0.0", &body, &body);
            let state = updater.apply(true).await.remove(0);
            assert_eq!(state.problem.as_deref(), Some(UNSAFE), "{escape}");
        }

        let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        let mut tar = tar::Builder::new(gz);
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_size(0);
        tar.append_link(&mut header, "package/claude", "/bin/sh")
            .unwrap();
        let link = tar.into_inner().unwrap().finish().unwrap();
        publish_tarball(&base, &files, "2.0.0", &link, &link);
        assert_eq!(
            updater.apply(true).await[0].problem.as_deref(),
            Some(UNSAFE)
        );

        files.lock().unwrap().clear();
        let state = updater.apply(true).await.remove(0);
        assert_eq!(state.problem.as_deref(), Some(UNREACHABLE));
        assert_eq!(state.version.as_deref(), Some("1.0.0"));
    }

    /// A tarball with one entry named `path` as written, unchecked.
    fn tarball_raw(path: &str) -> Vec<u8> {
        let mut header = [0u8; 512];
        header[..path.len()].copy_from_slice(path.as_bytes());
        header[100..107].copy_from_slice(b"0000755");
        header[108..115].copy_from_slice(b"0000000");
        header[116..123].copy_from_slice(b"0000000");
        let body = b"#!/bin/sh\necho 2.0.0\n";
        header[124..135].copy_from_slice(format!("{:011o}", body.len()).as_bytes());
        header[136..147].copy_from_slice(b"00000000000");
        header[156] = b'0';
        header[257..263].copy_from_slice(b"ustar\0");
        header[263..265].copy_from_slice(b"00");
        header[148..156].copy_from_slice(b"        ");
        let sum: u32 = header.iter().map(|b| u32::from(*b)).sum();
        header[148..155].copy_from_slice(format!("{sum:06o}\0").as_bytes());
        let mut raw = header.to_vec();
        raw.extend_from_slice(body);
        raw.resize(512 + 512, 0);
        raw.extend_from_slice(&[0u8; 1024]);
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        std::io::Write::write_all(&mut gz, &raw).unwrap();
        gz.finish().unwrap()
    }

    #[test]
    fn unpacking_holds_no_more_of_the_archive_than_its_limits() {
        let dir = tempfile::tempdir().unwrap();
        let unpacked = |body: &[u8]| {
            let archive = dir.path().join("archive.tgz");
            std::fs::write(&archive, body).unwrap();
            let into = dir.path().join("unpacked");
            let _ = std::fs::remove_dir_all(&into);
            unpack(&archive, &into)
        };
        // A long name the reader holds in memory, on a header Øphio skips.
        let named = |length: usize| {
            let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
            let mut tar = tar::Builder::new(gz);
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::XGlobalHeader);
            header.set_size(0);
            let path = format!("package/{}", "a".repeat(length));
            tar.append_data(&mut header, path, &[][..]).unwrap();
            let mut header = tar::Header::new_gnu();
            header.set_size(3);
            header.set_mode(0o755);
            header.set_entry_type(tar::EntryType::Regular);
            tar.append_data(&mut header, "package/claude", &b"run"[..])
                .unwrap();
            tar.into_inner().unwrap().finish().unwrap()
        };
        assert_eq!(unpacked(&named(200)), Ok(()));
        assert_eq!(unpacked(&named(2 << 20)), Err(UNSAFE));
    }

    #[tokio::test]
    async fn an_unsupported_system_says_so_and_downloads_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let updater = NpmUpdater::with(
            dir.path().join("agents"),
            "http://127.0.0.1:9".into(),
            None,
            vec![CLAUDE],
        );
        assert!(!updater.supported());
        let state = updater.apply(true).await.remove(0);
        assert_eq!(state.problem.as_deref(), Some(UNSUPPORTED));
        assert_eq!(state.version, None);
    }

    #[tokio::test]
    async fn checking_reads_only_metadata_and_never_starts_or_replaces_a_copy() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("agents");
        let (base, files) = registry().await;
        let updater = fresh(&root, &base);
        publish(&base, &files, "1.0.0", "1.0.0");
        updater.update(HarnessKind::ClaudeCode).await;
        let before = std::fs::read(root.join("claude/1.0.0/claude")).unwrap();
        publish(&base, &files, "2.0.0", "2.0.0");
        // Only metadata remains. A program download would fail.
        files
            .lock()
            .unwrap()
            .retain(|path, _| path.ends_with("/latest"));
        files.lock().unwrap().insert("/claude-notes".into(), b"## 2.0.0\n- Added support for `claude-fable-5-2`\n## 1.0.0\n- Added support for claude-sonnet-4-5".to_vec());
        // A version probe would fail. Checking must not notice or remove it.
        std::fs::write(root.join("claude/1.0.0/claude"), vec![b'x'; before.len()]).unwrap();
        let state = updater.check().await.remove(0);
        assert_eq!(state.version.as_deref(), Some("1.0.0"));
        assert_eq!(state.latest_version.as_deref(), Some("2.0.0"));
        assert_eq!(state.released_models, ["claude-fable-5-2"]);
        assert!(state.checked && state.update_supported);
        assert_eq!(state.problem, None);
        assert_eq!(versions(&root), ["1.0.0"]);
        assert!(updater.program(HarnessKind::ClaudeCode).is_some());
        // Failed or oversized notes cannot manufacture a model announcement.
        files.lock().unwrap().insert(
            "/claude-notes".into(),
            vec![b'x'; agent_release_notes::LIMIT + 1],
        );
        assert!(updater.check().await[0].released_models.is_empty());
        files.lock().unwrap().clear();
        let state = updater.check().await.remove(0);
        assert!(!state.checked);
        assert!(state.latest_version.is_none() && state.released_models.is_empty());
        assert_eq!(state.version.as_deref(), Some("1.0.0"));
    }

    #[tokio::test]
    async fn first_check_uses_observed_version_without_creating_a_copy() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("agents");
        let (base, files) = registry().await;
        publish(&base, &files, "2.0.0", "2.0.0");
        files.lock().unwrap().insert(
            "/claude-notes".into(),
            b"## 2.0.0\n- Added support for claude-opus-5-5".to_vec(),
        );
        let updater = fresh(&root, &base);
        let state = updater.check().await.remove(0);
        assert!(state.version.is_none() && state.released_models.is_empty());
        assert!(!root.exists());
        updater.observe_versions(&[(HarnessKind::ClaudeCode, "1.9.0 (Claude Code)".into())]);
        assert_eq!(
            updater.check().await[0].released_models,
            ["claude-opus-5-5"]
        );
        assert!(!root.exists());
    }

    #[tokio::test]
    async fn updating_one_agent_does_not_update_the_other_or_change_a_pin() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("agents");
        let (base, files) = registry().await;
        publish(&base, &files, "1.0.0", "1.0.0");
        let mut updater = fresh(&root, &base);
        updater.agents[1].installed = || true;
        let states = updater.update(HarnessKind::ClaudeCode).await;
        assert_eq!(states.len(), 1);
        assert!(states[0].problem.is_none());
        assert!(!root.join("codex").exists());
        publish(&base, &files, "2.0.0", "2.0.0");
        let updater = updater.with_pinned(vec![HarnessKind::ClaudeCode]);
        let state = updater.update(HarnessKind::ClaudeCode).await.remove(0);
        assert!(!state.update_supported && !state.checked);
        assert_eq!(state.problem.as_deref(), Some(PINNED));
        assert!(state.version.is_none());
        assert!(updater.program(HarnessKind::ClaudeCode).is_none());
        assert_eq!(held(&root.join("claude")).as_deref(), Some("1.0.0"));
        let state = updater.check().await.remove(0);
        assert_eq!(state.latest_version.as_deref(), Some("2.0.0"));
        assert!(!state.update_supported);
        assert_eq!(state.problem.as_deref(), Some(PINNED));
        assert_eq!(held(&root.join("claude")).as_deref(), Some("1.0.0"));
    }

    #[tokio::test]
    async fn an_unsupported_system_can_check_latest_without_downloading() {
        let dir = tempfile::tempdir().unwrap();
        let (base, files) = registry().await;
        publish(&base, &files, "2.0.0", "2.0.0");
        let mut updater = fresh(&dir.path().join("agents"), &base);
        updater.platform = None;
        let state = updater.check().await.remove(0);
        assert_eq!(state.latest_version.as_deref(), Some("2.0.0"));
        assert!(state.checked && !state.update_supported);
        assert_eq!(state.problem.as_deref(), Some(UNSUPPORTED));
        assert!(!updater.root.exists());
    }

    #[tokio::test]
    async fn codex_looks_back_past_a_fix_only_release_with_a_finite_budget() {
        let dir = tempfile::tempdir().unwrap();
        let (base, files) = registry().await;
        let mut updater = fresh(&dir.path().join("agents"), &base);
        updater.agents = vec![Agent {
            installed: || true,
            ..CODEX
        }];
        updater.observe_versions(&[(HarnessKind::Codex, "codex-cli 0.160.0".into())]);
        files.lock().unwrap().insert(
            "/@openai/codex/latest".into(),
            br#"{"version":"0.162.0"}"#.to_vec(),
        );
        let release = |version: &str, body: &str| {
            serde_json::json!({
                "tag_name":format!("rust-v{version}"), "draft": false, "prerelease": false, "body":body,
            })
        };
        // Five entries force a second page. Earlier announcements must not
        // disappear just because the latest release contains only fixes.
        let page1 = vec![release("0.162.0", "- Fixed a hang"); 5];
        let page2 = vec![
            release("0.161.0", "- Added support for gpt-6-astra"),
            release("0.160.0", "- Added support for gpt-5-codex"),
        ];
        files.lock().unwrap().insert(
            "/codex-notes?per_page=5&page=1".into(),
            serde_json::to_vec(&page1).unwrap(),
        );
        files.lock().unwrap().insert(
            "/codex-notes?per_page=5&page=2".into(),
            serde_json::to_vec(&page2).unwrap(),
        );
        let state = updater.check().await.remove(0);
        assert_eq!(state.released_models, ["gpt-6-astra"]);
        assert!(!updater.root.exists());
        files
            .lock()
            .unwrap()
            .remove("/codex-notes?per_page=5&page=2");
        assert!(
            updater.check().await[0].released_models.is_empty(),
            "failed lookback makes no claim"
        );
        for page in 1..=6 {
            files.lock().unwrap().insert(
                format!("/codex-notes?per_page=5&page={page}"),
                serde_json::to_vec(&page1).unwrap(),
            );
        }
        let state = updater.check().await.remove(0);
        assert!(state.checked && state.released_models.is_empty());
    }

    #[test]
    fn package_names_follow_the_makers() {
        let musl = Platform {
            musl: true,
            ..LINUX
        };
        assert_eq!(
            CLAUDE.system_package(musl),
            "@anthropic-ai/claude-code-linux-x64-musl"
        );
        assert_eq!(CODEX.system_package(musl), "@openai/codex-linux-x64");
        assert_eq!(
            CODEX.program(LINUX),
            "vendor/x86_64-unknown-linux-musl/bin/codex"
        );
        assert!(plain("0.160.1-linux-x64") && !plain("../1") && !plain("a/b") && !plain(""));
        assert_eq!(sha512("sha1-abc sha512-abc"), None);
    }
}

#[cfg(all(test, windows, target_arch = "x86_64"))]
mod windows_tests {
    use super::*;

    #[test]
    #[ignore = "requires OPHIO_TEST_CODEX_ARCHIVE pointing to the real npm Windows archive"]
    fn published_codex_windows_archive_extracts() {
        let _log = tracing_subscriber::fmt().with_test_writer().try_init();
        let archive = std::env::var_os("OPHIO_TEST_CODEX_ARCHIVE").expect("archive path");
        let root = tempfile::tempdir().unwrap();
        let into = root.path().join("unpacked");
        unpack(Path::new(&archive), &into).unwrap();
        assert!(
            into.join(Agent::CODEX.program(Platform::detect().unwrap()))
                .is_file()
        );
    }
    #[tokio::test]
    #[ignore = "requires OPHIO_TEST_CODEX_ARCHIVE pointing to the real npm Windows archive"]
    async fn published_codex_windows_archive_installs() {
        let _log = tracing_subscriber::fmt().with_test_writer().try_init();
        let archive = std::env::var_os("OPHIO_TEST_CODEX_ARCHIVE").expect("archive path");
        let bytes = std::fs::read(archive).unwrap();
        let integrity = format!(
            "sha512-{}",
            base64::engine::general_purpose::STANDARD.encode(Sha512::digest(&bytes))
        );
        assert_eq!(
            integrity,
            "sha512-AIkl2U3kjC4Ac9fINmSt8kf1W0lWqFfNZUpqtHd+UDvskYYZxx7Vga7RcAtOVNJ2v4BXb1QqpCqcTOGPHLIwRw=="
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let package = serde_json::json!({"dist": {"tarball": format!("{base}/package.tgz"), "integrity": integrity}}).to_string();
        let latest = serde_json::json!({"version": "0.167.2", "optionalDependencies": {"@openai/codex-win32-x64": "npm:@openai/codex@0.167.2-win32-x64"}}).to_string();
        let app = axum::Router::new()
            .route(
                "/package.tgz",
                axum::routing::get(move || {
                    let bytes = bytes.clone();
                    async move { bytes }
                }),
            )
            .route(
                "/@openai/codex/0.167.2-win32-x64",
                axum::routing::get(move || {
                    let package = package.clone();
                    async move { package }
                }),
            )
            .route(
                "/@openai/codex/latest",
                axum::routing::get(move || {
                    let latest = latest.clone();
                    async move { latest }
                }),
            );
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("AppData/Local/ophio/agents");
        let updater = NpmUpdater::with(
            root.clone(),
            base,
            Platform::detect(),
            vec![Agent {
                installed: || true,
                ..Agent::CODEX
            }],
        );
        let states = updater.apply(true).await;
        assert_eq!(states[0].problem, None, "{states:?}");
        assert_eq!(states[0].version.as_deref(), Some("0.167.2"));
        let (program, _) = updater.program(HarnessKind::Codex).unwrap();
        assert_eq!(
            starts(&program, &[], "0.167.2", HarnessKind::Codex).await,
            Some(true)
        );
        server.abort();
    }
}

#[cfg(test)]
#[path = "agent_updates/retention_tests.rs"]
mod retention_tests;
