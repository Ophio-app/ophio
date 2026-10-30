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
use crate::identity;
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

const UNSUPPORTED: &str =
    "Claude Code and Codex don't publish programs for this computer's system.";
const UNREACHABLE: &str = "Couldn't reach the update site to check for a new version.";
const UNREADABLE: &str = "The update site's answer about the newest version couldn't be read.";
const NO_BUILD: &str = "The newest version has no program for this computer's system.";
const INTERRUPTED: &str = "The download stopped before it finished.";
const TOO_LARGE: &str = "The download was larger than expected, so it wasn't used.";
const CHECKSUM: &str =
    "The download didn't match what the update site published, so it wasn't used.";
const UNSAFE: &str = "The download couldn't be unpacked safely, so it wasn't used.";
const NO_START: &str = "The new version didn't start when tried, so it wasn't used.";
const DISK: &str = "Couldn't save the new version on this computer.";

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
    /// Held through each apply; whether one has run in this process.
    applied: tokio::sync::Mutex<bool>,
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
            .build()
            .unwrap_or_default();
        Self {
            root,
            registry: registry.trim_end_matches('/').into(),
            platform,
            agents,
            client,
            applied: tokio::sync::Mutex::new(false),
        }
    }
    /// The folder holding `kind`'s copies, for the harness to start from.
    pub fn folder(&self, kind: HarnessKind) -> Option<PathBuf> {
        self.agents
            .iter()
            .find(|agent| agent.kind == kind)
            .map(|agent| self.root.join(agent.folder))
    }

    async fn agent(&self, agent: &Agent, keep: bool, first: bool) -> Option<CopyState> {
        let folder = self.root.join(agent.folder);
        if first {
            let _ = std::fs::remove_file(folder.join(PREVIOUS));
        }
        let held = held(&folder);
        if !keep {
            if let Some(version) = &held
                && std::fs::remove_file(folder.join(agent_copy::CURRENT)).is_ok()
            {
                let _ = std::fs::write(folder.join(PREVIOUS), version);
            }
            prune(&folder);
            return None;
        }
        if held.is_none() && !(agent.installed)() {
            return None;
        }
        let state = |version, checked, problem: Option<&str>| CopyState {
            harness: agent.kind,
            version,
            checked,
            problem: problem.map(Into::into),
        };
        let Some(platform) = self.platform else {
            return Some(state(held, false, Some(UNSUPPORTED)));
        };
        if identity::private_dir(&folder).is_err() {
            return Some(state(held, false, Some(DISK)));
        }
        // A copy that no longer starts goes, so tasks use the computer's
        // program until a whole one replaces it.
        let held = match held {
            Some(version) if !self.still_starts(agent, &folder, &version).await => {
                tracing::warn!(agent = agent.folder, version, "agent copy no longer starts");
                let _ = std::fs::remove_file(folder.join(agent_copy::CURRENT));
                None
            }
            held => held,
        };
        let outcome = self.update(agent, platform, &folder, held.as_deref()).await;
        prune(&folder);
        Some(match outcome {
            Ok(version) => state(Some(version), true, None),
            Err(problem) => {
                tracing::warn!(agent = agent.folder, problem, "agent copy not updated");
                state(held, false, Some(problem))
            }
        })
    }

    async fn still_starts(&self, agent: &Agent, folder: &Path, version: &str) -> bool {
        match agent_copy::current(folder) {
            Some(program) => starts(&program, &agent.env(), version).await,
            None => false,
        }
    }

    /// Brings `folder` to the newest release, returning its version.
    async fn update(
        &self,
        agent: &Agent,
        platform: Platform,
        folder: &Path,
        held: Option<&str>,
    ) -> Result<String, &'static str> {
        let latest = self.document(agent.package, "latest").await?;
        let version = text(&latest, &["version"])
            .filter(|version| plain(version))
            .ok_or(UNREADABLE)?;
        if held == Some(version.as_str()) {
            return Ok(version);
        }
        let key = agent.system_package(platform);
        let spec = text(&latest, &["optionalDependencies", &key]).ok_or(NO_BUILD)?;
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
            .map_err(|_| DISK)?;
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
        let size = std::fs::metadata(&program).map_err(|_| UNSAFE)?.len();
        std::fs::write(parent.join(agent_copy::MARKER), size.to_string()).map_err(|_| DISK)?;
        if !starts(&program, &agent.env(), &version).await {
            return Err(NO_START);
        }
        let target = folder.join(&version);
        if target.exists() {
            std::fs::remove_dir_all(&target).map_err(|_| DISK)?;
        }
        std::fs::rename(&unpacked, &target).map_err(|_| DISK)?;
        let named = folder.join(".current.new");
        std::fs::write(&named, format!("{version}/{relative}")).map_err(|_| DISK)?;
        std::fs::rename(&named, folder.join(agent_copy::CURRENT)).map_err(|_| DISK)?;
        if let Some(before) = held {
            let _ = std::fs::write(folder.join(PREVIOUS), before);
        }
        tracing::info!(agent = agent.folder, version, "agent copy updated");
        Ok(version)
    }

    async fn document(&self, name: &str, version: &str) -> Result<serde_json::Value, &'static str> {
        let url = format!("{}/{name}/{version}", self.registry);
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
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| UNREACHABLE)? {
            body.extend_from_slice(&chunk);
            if body.len() > DOCUMENT_LIMIT {
                return Err(UNREADABLE);
            }
        }
        serde_json::from_slice(&body).map_err(|_| UNREADABLE)
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
        let mut file = tokio::fs::File::create(to).await.map_err(|_| DISK)?;
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
            file.write_all(&chunk).await.map_err(|_| DISK)?;
        }
        file.sync_all().await.map_err(|_| DISK)?;
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
    async fn apply(&self, keep: bool) -> Vec<CopyState> {
        let mut applied = self.applied.lock().await;
        let first = !*applied;
        *applied = true;
        let mut states = Vec::new();
        for agent in &self.agents {
            states.extend(self.agent(agent, keep, first).await);
        }
        states
    }
    fn program(&self, kind: HarnessKind) -> Option<(PathBuf, Vec<(String, String)>)> {
        let agent = self.agents.iter().find(|agent| agent.kind == kind)?;
        let program = agent_copy::current(&self.root.join(agent.folder))?;
        Some((program, agent.env()))
    }
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
fn prune(folder: &Path) {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return;
    };
    let keep: Vec<String> = [
        held(folder),
        std::fs::read_to_string(folder.join(PREVIOUS))
            .ok()
            .map(|version| version.trim().to_owned()),
    ]
    .into_iter()
    .flatten()
    .collect();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == agent_copy::CURRENT || name == PREVIOUS || keep.contains(&name) {
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
    let file = std::fs::File::open(archive).map_err(|_| DISK)?;
    let allowance = Rc::new(Cell::new(0));
    let mut tar = tar::Archive::new(Budgeted {
        inner: flate2::read::GzDecoder::new(file),
        allowance: allowance.clone(),
    });
    identity::private_dir(into).map_err(|_| DISK)?;
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
            identity::private_dir(&target).map_err(|_| DISK)?;
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
            identity::private_dir(parent).map_err(|_| DISK)?;
        }
        let executable = entry.header().mode().is_ok_and(|mode| mode & 0o111 != 0);
        let mut out = create(&target, executable).map_err(|_| UNSAFE)?;
        allowance.set(size);
        let copied = std::io::copy(&mut (&mut entry).take(size), &mut out).map_err(|_| UNSAFE)?;
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

/// Whether `program --version` answers with `version` in time.
async fn starts(program: &Path, env: &[(String, String)], version: &str) -> bool {
    // A program just written can be briefly busy (`ETXTBSY`) while another
    // thread's child still holds the file from before its exec.
    for _ in 0..3 {
        let mut command = tokio::process::Command::new(program);
        command
            .arg("--version")
            .envs(env.iter().map(|(key, value)| (key, value)))
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        match tokio::time::timeout(START_LIMIT, command.output()).await {
            Ok(Ok(output)) => {
                return output.status.success()
                    && String::from_utf8_lossy(&output.stdout).contains(version);
            }
            Ok(Err(_)) => tokio::time::sleep(Duration::from_millis(200)).await,
            Err(_) => return false,
        }
    }
    false
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
                    match files.lock().unwrap().get(uri.path()) {
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
        NpmUpdater::with(root.into(), base.into(), Some(LINUX), vec![CLAUDE, CODEX])
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
    async fn keeps_one_copy_up_to_date_and_lets_go_when_turned_off() {
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

        assert_eq!(updater.apply(false).await, Vec::new());
        assert_eq!(updater.program(HarnessKind::ClaudeCode), None);
        assert_eq!(versions(&root), ["1.2.0"]);
        // A new process lets go of the rest.
        assert_eq!(fresh(&root, &base).apply(false).await, Vec::new());
        assert_eq!(versions(&root), Vec::<String>::new());
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
        assert_eq!(std::fs::read_to_string(&program).unwrap(), script);

        let good = tarball(&[("package/claude", b"#!/bin/sh\necho 2.0.0\n", 0o755)]);
        publish_tarball(&base, &files, "2.0.0", &good, b"something else");
        let state = updater.apply(true).await.remove(0);
        assert_eq!(state.version.as_deref(), Some("1.0.0"));
        assert!(!state.checked);
        assert_eq!(state.problem.as_deref(), Some(CHECKSUM));

        publish(&base, &files, "2.0.0", "1.9.9");
        let state = updater.apply(true).await.remove(0);
        assert_eq!(state.problem.as_deref(), Some(NO_START));
        assert_eq!(
            updater.program(HarnessKind::ClaudeCode).unwrap().0,
            root.join("claude/1.0.0/claude")
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
