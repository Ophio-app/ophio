// SPDX-License-Identifier: Apache-2.0
use crate::{
    error,
    history::History,
    own::{self, OwnSettings},
    program,
    proxy::{Policy, Proxy},
    relay::Relay,
    setup, sys,
};
use ports::{
    HarnessError,
    sandbox::{
        Applied, Reach, RelayEndpoint, RelayProvider, RelayTotals, Sandbox, SandboxSpec,
        WrapRequest, Wrapped,
    },
};
use protocol::capability::HarnessKind;
use std::{
    ffi::OsString,
    fs::File,
    io::{self, Write},
    net::{SocketAddr, TcpStream},
    os::{
        fd::AsRawFd,
        unix::{fs::FileExt, net::UnixStream},
    },
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

#[derive(Debug)]
struct Run {
    reach: Reach,
    history: Option<History>,
    session: Option<String>,
    namespaces: Option<sys::Namespaces>,
    wrapped: bool,
    prepared: bool,
    restart: bool,
}
#[derive(Debug)]
pub(crate) struct LinuxSandbox {
    spec: SandboxSpec,
    shim: PathBuf,
    disk: Option<Arc<crate::DiskBudget>>,
    proxy: Option<Proxy>,
    relay: Option<Relay>,
    native: Option<tempfile::TempDir>,
    info: File,
    gate_read: UnixStream,
    gate_write: Mutex<UnixStream>,
    run: Mutex<Run>,
    /// Where the agent's own settings are, once its command is built.
    own: Mutex<Option<OwnSettings>>,
}

pub fn runtime_paths() -> &'static [&'static str] {
    &[
        "/usr",
        "/bin",
        "/sbin",
        "/lib",
        "/lib64",
        "/etc",
        "/var/lib/pacman/local",
    ]
}

fn canonical(path: &Path) -> Result<PathBuf, HarnessError> {
    if !path.is_absolute() {
        return Err(error("sandbox paths must be absolute"));
    }
    path.canonicalize()
        .map_err(|e| error(format!("{}: {e}", path.display())))
}
fn normalize(mut reach: Reach) -> Result<Reach, HarnessError> {
    reach.read = reach
        .read
        .iter()
        .map(|p| canonical(p))
        .collect::<Result<_, _>>()?;
    reach.write = reach
        .write
        .iter()
        .map(|p| canonical(p))
        .collect::<Result<_, _>>()?;
    reach.read.sort();
    reach.read.dedup();
    reach.write.sort();
    reach.write.dedup();
    reach.whole_read |= reach.whole_write;
    Ok(reach)
}
fn policy(reach: &Reach) -> Policy {
    Policy {
        internet: reach.internet,
        local: reach.internet && reach.local_network,
    }
}
fn level(reach: &Reach, cwd: &Path, path: &Path) -> u8 {
    if reach.whole_write || reach.write.iter().any(|p| path.starts_with(p)) {
        2
    } else if reach.whole_read
        || path.starts_with(cwd)
        || reach.read.iter().any(|p| path.starts_with(p))
    {
        1
    } else {
        0
    }
}

impl LinuxSandbox {
    #[cfg(test)]
    pub(crate) fn new(spec: SandboxSpec, shim: PathBuf) -> Result<Self, HarnessError> {
        Self::with_disk_budget(spec, shim, None)
    }
    pub(crate) fn with_disk_budget(
        mut spec: SandboxSpec,
        shim: PathBuf,
        disk: Option<Arc<crate::DiskBudget>>,
    ) -> Result<Self, HarnessError> {
        if !matches!(spec.harness, HarnessKind::ClaudeCode | HarnessKind::Codex) {
            return Err(error(
                "only Claude Code and Codex provider policies are supported",
            ));
        }
        spec.cwd = canonical(&spec.cwd)?;
        spec.reach = normalize(spec.reach)?;
        if spec.reach.unrestricted() {
            return Err(error("unrestricted runs must bypass sandbox creation"));
        }
        sys::directories(&spec.state).map_err(error)?;
        spec.state = canonical(&spec.state)?;
        spec.always = spec
            .always
            .iter()
            .map(|p| canonical(p))
            .collect::<Result<_, _>>()?;
        spec.candidates.extend(spec.reach.read.iter().cloned());
        spec.candidates.extend(spec.reach.write.iter().cloned());
        spec.candidates.push(spec.cwd.clone());
        spec.candidates = spec
            .candidates
            .iter()
            .map(|p| canonical(p))
            .collect::<Result<_, _>>()?;
        spec.candidates.sort();
        spec.candidates.dedup();
        for path in &spec.candidates {
            if !path.is_dir() {
                return Err(error("candidate grants must be directories"));
            }
            if ["/usr", "/etc", "/bin", "/lib", "/lib64", "/proc", "/dev"]
                .iter()
                .any(|p| Path::new(p).starts_with(path) || path.starts_with(p))
            {
                return Err(error(
                    "project grants may not overlap implicit system runtime paths",
                ));
            }
        }
        let shim = canonical(&shim)?;
        if let Some(relay) = &mut spec.relay {
            if (relay.provider == RelayProvider::Anthropic)
                != (spec.harness == HarnessKind::ClaudeCode)
            {
                return Err(error("relay provider does not match the harness"));
            }
            if spec.reach.whole_read || spec.reach.whole_write {
                return Err(error("relay runs require explicit folder grants"));
            }
            relay.key_file = canonical(&relay.key_file)?;
            let name = relay
                .log
                .file_name()
                .ok_or_else(|| error("relay log needs a file name"))?;
            relay.log = canonical(
                relay
                    .log
                    .parent()
                    .ok_or_else(|| error("relay log needs a parent"))?,
            )?
            .join(name);
            if relay.log == relay.key_file {
                return Err(error("relay log and credential must be separate files"));
            }
        }
        if spec.offline && (spec.relay.is_some() || spec.reach.internet || spec.reach.local_network)
        {
            return Err(error("offline sandboxes cannot enable network access"));
        }
        let relay = spec
            .relay
            .clone()
            .map(|relay| Relay::start(&spec.state, relay))
            .transpose()
            .map_err(error)?;
        let native = if relay.is_some() || spec.offline {
            Some(
                tempfile::Builder::new()
                    .prefix("relay-native-")
                    .tempdir_in(&spec.state)
                    .map_err(error)?,
            )
        } else {
            None
        };
        let proxy = if relay.is_none() && !spec.offline {
            Some(
                Proxy::start(
                    &spec.state,
                    spec.harness,
                    policy(&spec.reach),
                    spec.on_blocked.clone(),
                )
                .map_err(error)?,
            )
        } else {
            None
        };
        let (gate_read, gate_write) = UnixStream::pair().map_err(error)?;
        let run = Run {
            reach: spec.reach.clone(),
            history: None,
            session: None,
            namespaces: None,
            wrapped: false,
            prepared: false,
            restart: false,
        };
        Ok(Self {
            disk,
            spec,
            shim,
            proxy,
            relay,
            native,
            info: tempfile::tempfile().map_err(error)?,
            gate_read,
            gate_write: Mutex::new(gate_write),
            run: Mutex::new(run),
            own: Mutex::new(None),
        })
    }
    fn build(
        &self,
        request: WrapRequest<'_>,
        reach: &Reach,
    ) -> Result<(Wrapped, Option<History>), HarnessError> {
        let env = |key: &str| {
            request
                .env
                .0
                .iter()
                .rev()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.clone())
                .or_else(|| std::env::var(key).ok())
        };
        let home = canonical(&PathBuf::from(
            env("HOME").ok_or_else(|| error("HOME is required"))?,
        ))?;
        let config = match self.spec.harness {
            HarnessKind::ClaudeCode => env("CLAUDE_CONFIG_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".claude")),
            HarnessKind::Codex => env("CODEX_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".codex")),
            _ => return Err(error("unsupported provider")),
        };
        let host_config = config;
        let config = if self.native.is_some() {
            PathBuf::from("/run/ophio-provider")
        } else {
            // The agent makes its settings folder on first use; a sandboxed
            // first use needs it to exist already, so it is made here, owner-only.
            if !host_config.exists() && host_config.is_absolute() {
                use std::os::unix::fs::DirBuilderExt;
                std::fs::DirBuilder::new()
                    .recursive(true)
                    .mode(0o700)
                    .create(&host_config)
                    .map_err(|e| error(format!("{}: {e}", host_config.display())))?;
            }
            canonical(&host_config)?
        };
        let mut private = private_paths(&home)?;
        private.extend(self.spec.private.iter().cloned());
        // A home grant must not expose the other provider's conversations.
        for path in [
            home.join(".claude"),
            home.join(".codex"),
            env("CLAUDE_CONFIG_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".claude")),
            env("CODEX_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".codex")),
        ] {
            if path.exists() {
                let path = canonical(&path)?;
                if path != config {
                    private.push(path);
                }
            }
        }
        private.retain(|p| p.exists());
        private.sort();
        private.dedup();
        if self.relay.is_some() {
            let relay = self.spec.relay.as_ref().unwrap();
            let mut protected = vec![
                relay.key_file.clone(),
                relay.log.clone(),
                host_config.clone(),
                home.join(".claude"),
                home.join(".codex"),
                home.join(".claude.json"),
            ];
            protected.extend(self.spec.given.iter().cloned());
            for key in ["CLAUDE_CONFIG_DIR", "CODEX_HOME"] {
                if let Some(path) = env(key) {
                    protected.push(PathBuf::from(path));
                }
            }
            let mut sources: Vec<PathBuf> = self
                .spec
                .candidates
                .iter()
                .chain(&self.spec.always)
                .chain(request.visible)
                .cloned()
                .collect();
            sources.extend(program::files(request.program, &home).map_err(error)?);
            for program in &self.spec.programs {
                sources.extend(program::files(program, &home).map_err(error)?);
            }
            sources.push(self.shim.clone());
            for root in runtime_paths() {
                if Path::new(root).exists() {
                    sources.push(canonical(Path::new(root))?);
                }
            }
            for source in sources {
                let source = canonical(&source)?;
                for secret in &protected {
                    let secret = if secret.exists() {
                        canonical(secret)?
                    } else {
                        secret.clone()
                    };
                    if source.starts_with(&secret) || secret.starts_with(&source) {
                        return Err(error(
                            "relay mounts must not overlap credentials, audit logs or provider login state",
                        ));
                    }
                }
                // Hard links to the key are just as secret as its configured path.
                use std::os::unix::fs::MetadataExt;
                let key = relay.key_file.metadata().map_err(error)?;
                if source.is_file()
                    && source
                        .metadata()
                        .is_ok_and(|m| m.dev() == key.dev() && m.ino() == key.ino())
                {
                    return Err(error(
                        "relay credential must not be mounted through a hard link",
                    ));
                }
            }
        }
        for path in &self.spec.candidates {
            if private.iter().any(|p| path.starts_with(p)) || path.starts_with(&config) {
                return Err(error(
                    "grants must not expose provider configuration or Ophio private data",
                ));
            }
        }
        let mut args: Vec<OsString> = [
            "--unshare-user",
            "--unshare-pid",
            "--unshare-net",
            "--unshare-ipc",
            "--die-with-parent",
            "--cap-drop",
            "ALL",
        ]
        .into_iter()
        .map(Into::into)
        .collect();
        if reach.whole_read {
            bind(
                &mut args,
                if reach.whole_write {
                    "--bind"
                } else {
                    "--ro-bind"
                },
                Path::new("/"),
                Path::new("/"),
            );
        } else {
            // An empty root prevents a project outside HOME from leaking via ro-bind /.
            args.extend(["--tmpfs".into(), "/".into()]);
            for path in runtime_paths() {
                if Path::new(path).exists() {
                    bind(&mut args, "--ro-bind", Path::new(path), Path::new(path));
                }
            }
            args.extend(["--tmpfs".into(), home.as_os_str().into()]);
        }
        args.extend([
            "--tmpfs".into(),
            "/tmp".into(),
            "--tmpfs".into(),
            "/run".into(),
            "--proc".into(),
            "/proc".into(),
            "--dev".into(),
            "/dev".into(),
        ]);
        for path in &self.spec.candidates {
            let access = level(reach, &self.spec.cwd, path);
            bind(
                &mut args,
                if access == 2 { "--bind" } else { "--ro-bind" },
                path,
                path,
            );
            if access == 0 {
                cover(&mut args, path);
            }
        }
        // Mask private host state before restoring only this run's explicit files.
        for path in &private {
            cover(&mut args, path);
        }
        for path in &self.spec.always {
            bind(&mut args, "--bind", path, path);
        }
        for path in request.visible {
            let path = canonical(path)?;
            bind(&mut args, "--ro-bind", &path, &path);
        }
        // An account signed out since the run was given it leaves nothing.
        for path in self
            .spec
            .given
            .iter()
            .filter(|p| self.relay.is_none() && p.exists())
        {
            let path = canonical(path)?;
            bind(&mut args, "--ro-bind", &path, &path);
        }
        let mut programs = program::files(request.program, &home)
            .map_err(|e| error(format!("agent executable: {e}")))?;
        for other in &self.spec.programs {
            programs.extend(
                program::files(other, &home)
                    .map_err(|e| error(format!("{}: {e}", other.display())))?,
            );
        }
        programs.push(self.shim.clone());
        for path in programs {
            let real = canonical(&path)?;
            if real != path {
                // A path reached through a link (its own name or a folder on
                // the way) must lead to the real file inside the sandbox, not
                // have the file bound onto it: /proc/self/exe would then name
                // the alias, and Codex looks for its helper beside that.
                // The real file is bound by its own entry in this list.
                link(&mut args, &path, &real)?;
                continue;
            }
            if ["/usr", "/bin", "/sbin", "/lib", "/lib64"]
                .iter()
                .any(|root| path.starts_with(root))
            {
                continue;
            }
            bind(&mut args, "--ro-bind", &path, &path);
        }
        let mut shared_check = String::new();
        // Each place in the agent's own settings, as it sees it, and what is there.
        let mut places: Vec<(PathBuf, PathBuf)> = Vec::new();
        let mut own_places: Vec<PathBuf> = Vec::new();
        let mut own_files: Vec<PathBuf> = Vec::new();
        let history = if let Some(native) = &self.native {
            if request.resume.is_some() {
                return Err(error("relay runs start with fresh provider state"));
            }
            bind(&mut args, "--bind", native.path(), &config);
            places.push((config.clone(), native.path().to_owned()));
            own_places.push(native.path().to_owned());
            None
        } else {
            let history = History::isolated(
                self.spec.harness,
                &config,
                &self.spec.state,
                &self.spec.cwd,
                request.resume,
            )
            .map_err(|e| match e.get_ref() {
                Some(unsaved) if unsaved.is::<crate::history::Unsaved>() => error(unsaved),
                _ => error(format!("native history: {e}")),
            })?;
            for (source, destination) in &history.binds {
                bind(&mut args, "--bind", source, destination);
                places.push((destination.clone(), source.clone()));
            }
            own_places.extend([
                self.spec.state.join("native"),
                self.spec.state.join("claude-history"),
            ]);
            // The person's own instructions and skills, read-only, over this
            // conversation's settings folder.
            let mut history = history;
            let mut forbidden = private.clone();
            forbidden.push(self.spec.state.clone());
            forbidden.extend(self.spec.given.iter().cloned());
            forbidden.extend(
                ["CLAUDE_CONFIG_DIR", "CODEX_HOME"]
                    .into_iter()
                    .filter_map(|k| env(k).map(PathBuf::from)),
            );
            let forbidden = setup::private(&home, forbidden);
            let cwd = canonical(&self.spec.cwd).unwrap_or_else(|_| self.spec.cwd.clone());
            let withheld = setup::withheld(request.disabled);
            let bounds = setup::Bounds {
                home: &home,
                cwd: &cwd,
                private: &forbidden,
                writes: reach.writes(),
                withheld: &withheld,
            };
            let shared = setup::select(self.spec.harness, &config, &bounds);
            let shared = setup::place(
                &self.spec.state.join("native"),
                &self.spec.state,
                shared,
                false,
            );
            for item in &shared {
                bind(
                    &mut args,
                    "--ro-bind",
                    &item.source,
                    &config.join(&item.relative),
                );
                places.push((config.join(&item.relative), item.source.clone()));
            }
            shared_check = setup::expected(&config, &shared);
            history.shared = shared;
            Some(history)
        };
        // Legacy Claude account/project metadata must not reappear through a
        // home/whole-computer grant. Authentication lives in .credentials.json.
        let legacy = home.join(".claude.json");
        if self.relay.is_none() && legacy.exists() {
            let profile = self.spec.state.join("claude-profile.json");
            let mut file = sys::history_file(&profile, true).map_err(error)?;
            if file.metadata().map_err(error)?.len() == 0 {
                file.write_all(b"{}").map_err(error)?;
            }
            bind(&mut args, "--bind", &profile, &legacy);
            places.push((legacy.clone(), profile.clone()));
            own_places.push(profile);
            own_files.push(legacy);
        }
        // Parent masks stay writable only during construction, so nested
        // scratch/program bind destinations can be created. Seal before exec.
        for path in self
            .spec
            .candidates
            .iter()
            .filter(|p| level(reach, &self.spec.cwd, p) == 0)
            .chain(&private)
        {
            args.extend(["--remount-ro".into(), path.as_os_str().into()]);
        }
        if let Some(socket) = self
            .relay
            .as_ref()
            .map(Relay::socket)
            .or_else(|| self.proxy.as_ref().map(Proxy::socket))
        {
            bind(
                &mut args,
                "--ro-bind",
                &socket,
                Path::new("/run/ophio-connect.sock"),
            );
        }
        if let Some(disk) = &self.disk {
            disk.configure(&mut args).map_err(error)?;
        }
        args.extend([
            "--chdir".into(),
            self.spec.cwd.as_os_str().into(),
            "--info-fd".into(),
            self.info.as_raw_fd().to_string().into(),
            "--block-fd".into(),
            self.gate_read.as_raw_fd().to_string().into(),
            "--".into(),
            self.shim.as_os_str().into(),
            "run".into(),
            if self.spec.offline {
                "-".into()
            } else {
                "/run/ophio-connect.sock".into()
            },
        ]);
        let mut env: Vec<(String, String)> = if self.relay.is_some() || self.spec.offline {
            vec![]
        } else {
            [
                "HTTPS_PROXY",
                "HTTP_PROXY",
                "ALL_PROXY",
                "https_proxy",
                "http_proxy",
                "all_proxy",
            ]
            .into_iter()
            .map(|k| (k.into(), "http://127.0.0.1:18080".into()))
            .collect()
        };
        for key in ["NO_PROXY", "no_proxy"] {
            env.push((key.into(), "localhost,127.0.0.1".into()));
        }
        if self.relay.is_some() || self.spec.offline {
            env.push((
                "PATH".into(),
                std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".into()),
            ));
            env.push(("LANG".into(), "C.UTF-8".into()));
        }
        // Always set, empty when nothing is shared, so a copy in Ophio's own
        // environment never reaches the sandbox's check.
        env.push((setup::VARIABLE.into(), shared_check));
        env.push(("HOME".into(), home.to_string_lossy().into_owned()));
        env.push((
            if self.spec.harness == HarnessKind::ClaudeCode {
                "CLAUDE_CONFIG_DIR"
            } else {
                "CODEX_HOME"
            }
            .into(),
            config.to_string_lossy().into_owned(),
        ));
        *self.own.lock().unwrap() = Some(OwnSettings {
            harness: self.spec.harness,
            folder: config,
            files: own_files,
            places,
            own: own::real(own_places),
            cwd: self.spec.cwd.clone(),
        });
        Ok((
            Wrapped {
                program: "/usr/bin/bwrap".into(),
                args,
                env,
                clear_env: self.relay.is_some() || self.spec.offline,
                shared_skills: history
                    .as_ref()
                    .is_some_and(|h| h.shared.iter().any(|s| s.relative.starts_with("skills"))),
            },
            history,
        ))
    }
}
fn bind(args: &mut Vec<OsString>, mode: &str, source: &Path, destination: &Path) {
    args.extend([
        mode.into(),
        source.as_os_str().into(),
        destination.as_os_str().into(),
    ]);
}
fn cover(args: &mut Vec<OsString>, path: &Path) {
    args.extend(["--tmpfs".into(), path.as_os_str().into()]);
}
/// What the sandbox will have at `path` once bubblewrap has applied `args`.
/// `path`'s folders must be real ones (no links), which is how [`link`] asks.
#[derive(Debug, PartialEq)]
enum Shown {
    /// The host's own entry, through a folder bound onto itself.
    Host,
    /// An entry these arguments make there themselves: a link, a folder or
    /// a mount point.
    Made,
    /// Nothing: the path lies under a covering empty folder.
    Absent,
    /// Something other than the host's entry, under a folder bound from
    /// elsewhere. Nothing may be created there.
    Foreign,
}
fn shown(args: &[OsString], path: &Path) -> Shown {
    let mut shown = Shown::Absent;
    let mut i = 0;
    while i < args.len() {
        let operands = |n: usize| args.get(i + 1..i + 1 + n).unwrap_or_default();
        // Operands of these options are absolute paths, never option names,
        // and the other options' operands ("ALL", descriptor numbers) are
        // not these names either, so stepping one word otherwise is exact.
        let step = match args[i].to_str() {
            Some(
                "--bind" | "--ro-bind" | "--dev-bind" | "--bind-try" | "--ro-bind-try"
                | "--dev-bind-try",
            ) => {
                if let [source, destination] = operands(2)
                    && path.starts_with(destination)
                {
                    shown = if source == destination {
                        Shown::Host
                    } else if path == Path::new(destination) {
                        Shown::Made
                    } else {
                        Shown::Foreign
                    };
                }
                3
            }
            Some("--tmpfs" | "--proc" | "--dev" | "--mqueue" | "--dir") => {
                if let [destination] = operands(1)
                    && path.starts_with(destination)
                {
                    shown = if path == Path::new(destination) {
                        Shown::Made
                    } else {
                        Shown::Absent
                    };
                }
                2
            }
            Some("--symlink") => {
                if let [_, destination] = operands(2)
                    && path.starts_with(destination)
                {
                    shown = if path == Path::new(destination) {
                        Shown::Made
                    } else {
                        Shown::Foreign
                    };
                }
                3
            }
            _ => 1,
        };
        i += step;
    }
    shown
}
/// Make `path`, which the host resolves to `real` through one or more links,
/// resolve the same way inside the sandbox.
///
/// The host's own resolution is walked one name at a time. A link the
/// sandbox will already show (the folder holding it is readable) is left
/// alone and followed, so nothing is placed on top of it; a link it will
/// not show is created there, pointing at that link's real target. Links are
/// only ever created in folders whose own path has no links, under an empty
/// covering folder, so bubblewrap never meets an existing destination, never
/// creates folders through a link, and never writes into a host folder. No
/// file or folder becomes readable that was not already: the real file is
/// bound by its own entry.
fn link(args: &mut Vec<OsString>, path: &Path, real: &Path) -> Result<(), HarnessError> {
    use std::path::Component;
    let failed = |e: io::Error| error(format!("{}: {e}", path.display()));
    let mut at = PathBuf::from("/");
    let mut rest: std::collections::VecDeque<PathBuf> = path
        .components()
        .map(|c| PathBuf::from(c.as_os_str()))
        .collect();
    let mut hops = 0;
    while let Some(part) = rest.pop_front() {
        match part.components().next() {
            Some(Component::RootDir) => at = PathBuf::from("/"),
            Some(Component::ParentDir) => {
                // Going up needs the folder itself to exist in the sandbox.
                if matches!(shown(args, &at), Shown::Absent) {
                    args.extend(["--dir".into(), at.as_os_str().into()]);
                }
                at.pop();
            }
            Some(Component::Normal(name)) => {
                let entry = at.join(name);
                if !std::fs::symlink_metadata(&entry)
                    .map_err(failed)?
                    .file_type()
                    .is_symlink()
                {
                    at = entry;
                    continue;
                }
                match shown(args, &entry) {
                    Shown::Host => {
                        hops += 1;
                        if hops > 40 {
                            return Err(error(format!("{}: too many links", path.display())));
                        }
                        let text = std::fs::read_link(&entry).map_err(failed)?;
                        for part in text.components().rev() {
                            rest.push_front(PathBuf::from(part.as_os_str()));
                        }
                    }
                    Shown::Made => at = canonical(&entry)?,
                    Shown::Absent => {
                        let target = canonical(&entry)?;
                        bind(args, "--symlink", &target, &entry);
                        at = target;
                    }
                    Shown::Foreign => {
                        return Err(error(format!(
                            "{}: the agent program is reached through a folder the sandbox replaces",
                            path.display()
                        )));
                    }
                }
            }
            _ => {}
        }
    }
    if at != real {
        return Err(error(format!(
            "{}: the agent program changed while its sandbox was prepared",
            path.display()
        )));
    }
    Ok(())
}
fn private_paths(home: &Path) -> Result<Vec<PathBuf>, HarnessError> {
    let mut paths = vec![
        home.join(".local/share/ophio"),
        home.join(".cache/ophio"),
        home.join(".config/ophio"),
    ];
    if let Ok(entries) = std::fs::read_dir(home.join(".config")) {
        for entry in entries {
            let entry = entry.map_err(error)?;
            if entry.file_name().as_encoded_bytes().starts_with(b"ophio") {
                paths.push(entry.path());
            }
        }
    }
    paths.sort();
    paths.dedup();
    Ok(paths)
}
fn restart(reason: impl ToString) -> Applied {
    Applied::NeedsRestart(reason.to_string())
}
fn mount_failure(e: io::Error) -> Applied {
    if e.raw_os_error() == Some(libc::EBUSY) {
        restart(
            "An open writer prevents a read-only remount. Stop the contained turn to close its descriptors; SIGSTOP alone cannot clear EBUSY.",
        )
    } else {
        restart(format!("Mount change failed; stop the contained turn: {e}"))
    }
}

#[async_trait::async_trait]
impl Sandbox for LinuxSandbox {
    fn relay(&self) -> Option<RelayEndpoint> {
        self.spec.relay.as_ref().map(|relay| RelayEndpoint {
            provider: relay.provider,
            port: 18080,
        })
    }
    fn relay_totals(&self) -> Option<RelayTotals> {
        self.relay.as_ref().map(Relay::totals)
    }
    fn wrap(&self, request: WrapRequest<'_>) -> Result<Wrapped, HarnessError> {
        let mut run = self.run.lock().unwrap();
        if run.wrapped {
            return Err(error("a sandbox is single-use; create one per run"));
        }
        let (wrapped, history) = self.build(request, &run.reach)?;
        run.history = history;
        run.wrapped = true;
        Ok(wrapped)
    }
    fn own_settings_file(&self, path: &Path) -> bool {
        self.own
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|own| own.holds(path))
    }
    fn prepare(&self, command: &mut Command) -> io::Result<()> {
        let mut run = self.run.lock().unwrap();
        if !run.wrapped || run.prepared {
            return Err(io::Error::other("wrap then prepare exactly once"));
        }
        sys::inherit(
            command,
            vec![self.info.as_raw_fd(), self.gate_read.as_raw_fd()],
        );
        run.prepared = true;
        Ok(())
    }
    async fn started(&self) -> Result<(), HarnessError> {
        if !self.run.lock().unwrap().prepared {
            return Err(error("prepare must precede started"));
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let mut bytes = [0; 4096];
            let count = self.info.read_at(&mut bytes, 0).map_err(error)?;
            if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes[..count]) {
                let pid = value["child-pid"]
                    .as_u64()
                    .and_then(|p| u32::try_from(p).ok())
                    .filter(|p| *p > 0)
                    .ok_or_else(|| error("bwrap did not report a child pid"))?;
                let namespaces = sys::Namespaces::open(pid).map_err(error)?;
                if let Some(disk) = &self.disk {
                    disk.attach(namespaces.root()).map_err(error)?;
                }
                self.run.lock().unwrap().namespaces = Some(namespaces);
                self.gate_write
                    .lock()
                    .unwrap()
                    .write_all(b"1")
                    .map_err(error)?;
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(error("timed out waiting for bwrap namespace setup"));
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
    async fn update(&self, reach: Reach) -> Applied {
        if self.spec.offline
            && (reach.internet || reach.local_network || reach.whole_read || reach.whole_write)
        {
            return Applied::NeedsRestart(
                "Offline sandboxes cannot gain network or whole-computer access".into(),
            );
        }
        let reach = match normalize(reach) {
            Ok(r) => r,
            Err(e) => return restart(e),
        };
        let mut run = self.run.lock().unwrap();
        if self.relay.is_some() && (reach.whole_read || reach.whole_write) {
            return restart("relay runs require explicit folder grants");
        }
        if let Some(proxy) = &self.proxy {
            proxy.update(policy(&reach));
        }
        run.reach.internet = reach.internet;
        run.reach.local_network = reach.local_network;
        if run.restart {
            return restart("a previous filesystem change requires this turn to end");
        }
        if reach.whole_read != run.reach.whole_read || reach.whole_write != run.reach.whole_write {
            run.restart = true;
            return restart("whole-computer access changes require rebuilding the root filesystem");
        }
        if reach
            .read
            .iter()
            .chain(&reach.write)
            .any(|p| !self.spec.candidates.contains(p))
        {
            run.restart = true;
            return restart("folder was not pre-bound as a candidate");
        }
        for path in &self.spec.candidates {
            if level(&reach, &self.spec.cwd, path) == 0
                && level(&run.reach, &self.spec.cwd, path) > 0
            {
                run.restart = true;
                return restart(
                    "read revocation requires ending the contained turn: a cover cannot revoke open descriptors, cwd, or mapped files",
                );
            }
        }
        if run.namespaces.is_none() {
            return restart("sandbox has not started");
        }
        for parent in &self.spec.candidates {
            if self
                .spec
                .candidates
                .iter()
                .any(|p| p != parent && p.starts_with(parent))
                && self.spec.candidates.iter().any(|p| {
                    p.starts_with(parent)
                        && level(&reach, &self.spec.cwd, p) != level(&run.reach, &self.spec.cwd, p)
                })
            {
                run.restart = true;
                return restart("nested project grants require rebuilding mounts");
            }
        }
        for path in &self.spec.candidates {
            let before = level(&run.reach, &self.spec.cwd, path);
            let after = level(&reach, &self.spec.cwd, path);
            let operations: &[&str] = match (before, after) {
                (0, 1) => &["uncover"],
                (0, 2) => &["uncover", "write"],
                (1, 2) => &["write"],
                (2, 1) => &["read"],
                _ => &[],
            };
            for operation in operations {
                if let Err(e) = run
                    .namespaces
                    .as_ref()
                    .unwrap()
                    .change(&self.shim, operation, path)
                {
                    run.restart = true;
                    return mount_failure(e);
                }
            }
        }
        run.reach = reach;
        Applied::Applied
    }
    fn session_started(&self, id: &str) -> Result<(), HarnessError> {
        if id.is_empty()
            || id.len() > 128
            || !id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err(error("invalid native session id"));
        }
        self.run.lock().unwrap().session = Some(id.into());
        Ok(())
    }
    async fn finished(&self) -> Result<(), HarnessError> {
        let run = self.run.lock().unwrap();
        if let Some(history) = &run.history {
            history.finished(run.session.as_deref()).map_err(|e| {
                error(format!(
                    "Øphio couldn't copy this turn into the agent's own history on this computer ({e})."
                ))
            })?;
        }
        Ok(())
    }
    async fn connect(&self, addresses: &[SocketAddr]) -> Result<TcpStream, HarnessError> {
        if addresses.is_empty() || !addresses.iter().all(|a| a.ip().is_loopback()) {
            return Err(error("only the sandbox's own loopback can be reached"));
        }
        let namespaces = match &self.run.lock().unwrap().namespaces {
            Some(namespaces) => namespaces.try_clone().map_err(error)?,
            None => return Err(error("sandbox has not started")),
        };
        let shim = self.shim.clone();
        let addresses = addresses.to_vec();
        tokio::task::spawn_blocking(move || namespaces.connect(&shim, &addresses))
            .await
            .map_err(error)?
            .map_err(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn reach() -> Reach {
        Reach {
            read: vec![],
            write: vec![],
            whole_read: false,
            whole_write: false,
            internet: false,
            local_network: false,
        }
    }
    struct RelayFixture {
        _root: tempfile::TempDir,
        spec: SandboxSpec,
        env: ports::RunEnv,
    }
    impl RelayFixture {
        fn new() -> Self {
            use ports::sandbox::{RelayBudget, RelayPolicy, RelaySpec};
            let root = tempfile::tempdir().unwrap();
            let home = root.path().join("home");
            let cwd = home.join("project");
            let given = home.join("account");
            for path in [&cwd, &given, &home.join(".claude"), &home.join(".codex")] {
                std::fs::create_dir_all(path).unwrap();
            }
            std::fs::write(given.join("auth.json"), "fixture-account-key").unwrap();
            std::fs::write(home.join(".claude/.credentials.json"), "fixture-login").unwrap();
            let key_file = root.path().join("key");
            std::fs::write(&key_file, "fixture-provider-key").unwrap();
            let spec = SandboxSpec {
                offline: false,
                reach: reach(),
                harness: HarnessKind::ClaudeCode,
                cwd,
                state: root.path().join("state"),
                always: vec![],
                programs: vec![],
                candidates: vec![],
                private: vec![],
                given: vec![given],
                on_blocked: None,
                relay: Some(RelaySpec {
                    provider: RelayProvider::Anthropic,
                    key_file,
                    budget: RelayBudget {
                        max_requests: 10,
                        max_input_tokens: 1000,
                        max_output_tokens: 1000,
                        deadline: None,
                    },
                    policy: RelayPolicy::default(),
                    log: root.path().join("audit.jsonl"),
                    on_signal: None,
                    inspect: None,
                    test_upstream: None,
                }),
            };
            let env = ports::RunEnv(vec![
                ("HOME".into(), home.to_str().unwrap().into()),
                ("ANTHROPIC_API_KEY".into(), "fixture-account-key".into()),
            ]);
            Self {
                _root: root,
                spec,
                env,
            }
        }
    }
    #[test]
    fn offline_mounts_no_egress_socket_and_uses_fresh_provider_state() {
        let f = RelayFixture::new();
        let mut spec = f.spec.clone();
        spec.offline = true;
        spec.relay = None;
        spec.given.clear();
        let sandbox = LinuxSandbox::new(spec, "/usr/bin/true".into()).unwrap();
        assert!(sandbox.proxy.is_none());
        assert!(sandbox.relay.is_none());
        let (wrapped, history) = sandbox
            .build(
                WrapRequest {
                    disabled: &ports::Disabled::default(),
                    program: Path::new("/bin/sh"),
                    visible: &[],
                    env: &f.env,
                    resume: None,
                },
                &reach(),
            )
            .unwrap();
        assert!(history.is_none());
        assert!(wrapped.clear_env);
        assert!(!wrapped.args.iter().any(|a| a == "/run/ophio-connect.sock"));
        assert!(
            !wrapped
                .env
                .iter()
                .any(|(k, _)| k.contains("PROXY") && k != "NO_PROXY")
        );
    }
    #[test]
    fn offline_starts_with_no_provider_directory_in_a_fresh_home() {
        let f = RelayFixture::new();
        let home = f._root.path().join("fresh-home");
        std::fs::create_dir(&home).unwrap();
        for harness in [HarnessKind::Codex, HarnessKind::ClaudeCode] {
            let mut spec = f.spec.clone();
            spec.offline = true;
            spec.relay = None;
            spec.given.clear();
            spec.harness = harness;
            let env = ports::RunEnv(vec![
                ("HOME".into(), home.to_string_lossy().into_owned()),
                (
                    "CODEX_HOME".into(),
                    home.join(".codex").to_string_lossy().into_owned(),
                ),
                (
                    "CLAUDE_CONFIG_DIR".into(),
                    home.join(".claude").to_string_lossy().into_owned(),
                ),
            ]);
            let sandbox = LinuxSandbox::new(spec, "/usr/bin/true".into()).unwrap();
            let (wrapped, history) = sandbox
                .build(
                    WrapRequest {
                        disabled: &ports::Disabled::default(),
                        program: Path::new("/bin/sh"),
                        visible: &[],
                        env: &env,
                        resume: None,
                    },
                    &reach(),
                )
                .unwrap();
            assert!(history.is_none());
            assert!(wrapped.env.iter().any(|(key, value)| {
                (key == "CODEX_HOME" || key == "CLAUDE_CONFIG_DIR")
                    && value == "/run/ophio-provider"
            }));
            // Nothing is shared, and the check list says so whatever Ophio's
            // own environment holds.
            assert!(
                wrapped
                    .env
                    .iter()
                    .any(|(key, value)| key == setup::VARIABLE && value.is_empty())
            );
        }
        assert!(!home.join(".codex").exists());
        assert!(!home.join(".claude").exists());
    }
    #[test]
    fn relay_mounts_no_accounts_login_folders_key_or_log_and_has_no_proxy() {
        let f = RelayFixture::new();
        let mut spec = f.spec.clone();
        spec.reach.internet = true;
        spec.reach.local_network = true;
        let sandbox = LinuxSandbox::new(spec, "/usr/bin/true".into()).unwrap();
        assert!(sandbox.proxy.is_none());
        assert!(sandbox.relay.is_some());
        assert_eq!(sandbox.relay().unwrap().port, 18080);
        let (wrapped, history) = sandbox
            .build(
                WrapRequest {
                    disabled: &ports::Disabled::default(),
                    program: Path::new("/usr/bin/true"),
                    visible: &[],
                    env: &f.env,
                    resume: None,
                },
                &f.spec.reach,
            )
            .unwrap();
        assert!(history.is_none());
        assert!(wrapped.clear_env);
        assert!(
            !wrapped
                .env
                .iter()
                .any(|(k, _)| k.ends_with("API_KEY") || k == "HTTPS_PROXY")
        );
        assert!(
            wrapped
                .env
                .iter()
                .any(|(k, v)| k == "CLAUDE_CONFIG_DIR" && v == "/run/ophio-provider")
        );
        let relay = f.spec.relay.as_ref().unwrap();
        let args: Vec<_> = wrapped.args.iter().map(PathBuf::from).collect();
        for bind in args
            .windows(3)
            .filter(|w| w[0] == Path::new("--bind") || w[0] == Path::new("--ro-bind"))
        {
            for secret in [&relay.key_file, &relay.log, &f.spec.given[0]] {
                assert!(
                    !secret.starts_with(&bind[1]),
                    "{} exposes {}",
                    bind[1].display(),
                    secret.display()
                );
            }
            assert!(!bind[1].ends_with(".credentials.json"));
            assert!(!bind[1].ends_with("auth.json"));
        }
    }
    #[test]
    fn relay_refuses_explicit_or_parent_mounts_exposing_credentials_or_logs() {
        for kind in [
            "key",
            "key-parent",
            "log",
            "given",
            "provider",
            "visible",
            "whole",
        ] {
            let f = RelayFixture::new();
            let mut spec = f.spec.clone();
            let key = spec.relay.as_ref().unwrap().key_file.clone();
            let mut visible = vec![];
            match kind {
                "key" => spec.always.push(key.clone()),
                "key-parent" => spec.always.push(key.parent().unwrap().to_owned()),
                "log" => {
                    let log = spec.relay.as_ref().unwrap().log.clone();
                    std::fs::write(&log, "").unwrap();
                    spec.always.push(log);
                }
                "given" => spec.always.extend(spec.given.clone()),
                "provider" => spec.always.push(spec.cwd.parent().unwrap().join(".claude")),
                "visible" => visible.push(key),
                _ => spec.reach.whole_read = true,
            }
            let sandbox = LinuxSandbox::new(spec, "/usr/bin/true".into());
            if kind == "whole" {
                assert!(sandbox.is_err());
                continue;
            }
            assert!(
                sandbox
                    .unwrap()
                    .build(
                        WrapRequest {
                            disabled: &ports::Disabled::default(),
                            program: Path::new("/usr/bin/true"),
                            visible: &visible,
                            env: &f.env,
                            resume: None,
                        },
                        &f.spec.reach
                    )
                    .is_err(),
                "{kind}"
            );
        }
    }
    #[test]
    fn cwd_is_always_readable_and_write_implies_read() {
        let cwd = Path::new("/project");
        let mut r = reach();
        assert_eq!(level(&r, cwd, cwd), 1);
        assert_eq!(level(&r, cwd, Path::new("/project/child")), 1);
        assert_eq!(level(&r, cwd, Path::new("/other")), 0);
        r.write.push("/other".into());
        assert_eq!(level(&r, cwd, Path::new("/other/sub")), 2);
    }
    #[test]
    fn busy_requires_closing_writers_not_suspending_them() {
        assert!(
            matches!(mount_failure(io::Error::from_raw_os_error(libc::EBUSY)), Applied::NeedsRestart(reason) if reason.contains("SIGSTOP") && reason.contains("close"))
        );
    }
    #[test]
    fn unsaved_resume_keeps_plain_words_and_other_history_errors_keep_detail() {
        for kind in [HarnessKind::ClaudeCode, HarnessKind::Codex] {
            let f = RelayFixture::new();
            let mut spec = f.spec.clone();
            spec.relay = None;
            spec.harness = kind;
            let sandbox = LinuxSandbox::new(spec, PathBuf::from("/usr/bin/true")).unwrap();
            let build = || {
                sandbox.build(
                    WrapRequest {
                        disabled: &ports::Disabled::default(),
                        program: Path::new("/usr/bin/true"),
                        visible: &[],
                        env: &f.env,
                        resume: Some("one"),
                    },
                    &reach(),
                )
            };
            let HarnessError::SpawnFailed(why) = build().unwrap_err() else {
                panic!("expected a start refusal");
            };
            assert_eq!(why, crate::history::Unsaved.to_string());
            let broken = f.spec.state.join(if kind == HarnessKind::ClaudeCode {
                "claude-history/projects"
            } else {
                "native/sessions"
            });
            if broken.exists() {
                std::fs::remove_dir_all(&broken).unwrap();
            }
            std::os::unix::fs::symlink("missing", &broken).unwrap();
            let HarnessError::SpawnFailed(why) = build().unwrap_err() else {
                panic!("expected a start refusal");
            };
            assert!(why.starts_with("native history: "), "{why}");
            assert!(!why.contains(&crate::history::Unsaved.to_string()));
        }
    }
    #[test]
    fn a_missing_agent_settings_folder_is_made_owner_only() {
        missing_settings_folder(".claude", HarnessKind::ClaudeCode, "CLAUDE_CONFIG_DIR");
    }

    #[test]
    fn missing_parents_of_agent_settings_are_made_owner_only() {
        missing_settings_folder(
            "settings/claude",
            HarnessKind::ClaudeCode,
            "CLAUDE_CONFIG_DIR",
        );
        missing_settings_folder("settings/codex", HarnessKind::Codex, "CODEX_HOME");
    }

    fn missing_settings_folder(relative: &str, harness: HarnessKind, variable: &str) {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let config = home.join(relative);
        let cwd = dir.path().join("project");
        for path in [&home, &cwd] {
            std::fs::create_dir_all(path).unwrap();
        }
        let spec = SandboxSpec {
            offline: false,
            reach: reach(),
            harness,
            cwd: cwd.clone(),
            state: dir.path().join("state"),
            always: vec![],
            programs: vec![],
            candidates: vec![],
            private: vec![],
            given: vec![],
            relay: None,
            on_blocked: None,
        };
        let sandbox = LinuxSandbox::new(spec, PathBuf::from("/usr/bin/true")).unwrap();
        let env = ports::harness::RunEnv(vec![
            ("HOME".into(), home.to_str().unwrap().into()),
            (variable.into(), config.to_str().unwrap().into()),
        ]);
        sandbox
            .build(
                WrapRequest {
                    disabled: &ports::Disabled::default(),
                    program: Path::new("/usr/bin/true"),
                    visible: &[],
                    env: &env,
                    resume: None,
                },
                &reach(),
            )
            .unwrap();
        for folder in config.ancestors().take_while(|p| *p != home) {
            let mode = std::fs::metadata(folder).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o700);
        }
    }
    #[test]
    fn only_what_the_agent_writes_in_its_own_settings_is_its_own() {
        for harness in [HarnessKind::ClaudeCode, HarnessKind::Codex] {
            let dir = tempfile::tempdir().unwrap();
            let root = std::fs::canonicalize(dir.path()).unwrap();
            let home = root.join("home");
            let claude = harness == HarnessKind::ClaudeCode;
            let config = home.join(if claude { ".claude" } else { ".codex" });
            let cwd = root.join("project");
            let state = root.join("state");
            for path in [&config, &cwd] {
                std::fs::create_dir_all(path).unwrap();
            }
            let instructions = if claude { "CLAUDE.md" } else { "AGENTS.md" };
            std::fs::write(config.join(instructions), "the person's own").unwrap();
            let credential = crate::history::credential(harness);
            std::fs::write(config.join(credential), "{}").unwrap();
            std::fs::write(home.join(".claude.json"), "{}").unwrap();
            let spec = SandboxSpec {
                offline: false,
                reach: reach(),
                harness,
                cwd: cwd.clone(),
                state: state.clone(),
                always: vec![],
                programs: vec![],
                candidates: vec![],
                private: vec![],
                given: vec![],
                relay: None,
                on_blocked: None,
            };
            let sandbox = LinuxSandbox::new(spec, PathBuf::from("/usr/bin/true")).unwrap();
            // As the agent names it, and where it really lands.
            let (note, kept) = if claude {
                (
                    config.join("projects/-p/memory/note.md"),
                    state.join("claude-history/projects/-p/memory/note.md"),
                )
            } else {
                (
                    config.join("memories/note.md"),
                    state.join("native/memories/note.md"),
                )
            };
            std::fs::create_dir_all(kept.parent().unwrap()).unwrap();
            std::fs::write(&kept, "remember this").unwrap();
            assert!(
                !sandbox.own_settings_file(&note),
                "not known before it is built"
            );
            let env = ports::harness::RunEnv(vec![("HOME".into(), home.to_str().unwrap().into())]);
            let (_, history) = sandbox
                .build(
                    WrapRequest {
                        disabled: &ports::Disabled::default(),
                        program: Path::new("/usr/bin/true"),
                        visible: &[],
                        env: &env,
                        resume: None,
                    },
                    &reach(),
                )
                .unwrap();
            assert!(
                history
                    .unwrap()
                    .shared
                    .iter()
                    .any(|s| s.relative == Path::new(instructions))
            );
            // What bubblewrap leaves where it mounts a file of the person's.
            for name in [instructions, credential] {
                std::fs::write(state.join("native").join(name), "").unwrap();
            }
            std::fs::write(cwd.join("lib.rs"), "fn main() {}").unwrap();
            assert!(sandbox.own_settings_file(&note), "{harness:?}");
            assert!(sandbox.own_settings_file(&home.join(".claude.json")));
            for path in [
                config.join(instructions),
                config.join(credential),
                cwd.join("lib.rs"),
                config.join("missing.md"),
            ] {
                assert!(!sandbox.own_settings_file(&path), "{}", path.display());
            }
        }
    }
    #[test]
    fn command_masks_candidates_and_binds_only_native_history() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let config = home.join(".claude");
        let cwd = dir.path().join("project");
        let other = dir.path().join("other");
        for path in [&config, &cwd, &other] {
            std::fs::create_dir_all(path).unwrap();
        }
        let spec = SandboxSpec {
            offline: false,
            reach: reach(),
            harness: HarnessKind::ClaudeCode,
            cwd: cwd.clone(),
            state: dir.path().join("state"),
            always: vec![],
            programs: vec![],
            candidates: vec![other.clone()],
            private: vec![],
            given: vec![],
            relay: None,
            on_blocked: None,
        };
        let sandbox = LinuxSandbox::new(spec, PathBuf::from("/usr/bin/true")).unwrap();
        let env = ports::harness::RunEnv(vec![
            ("HOME".into(), home.to_str().unwrap().into()),
            ("CLAUDE_CONFIG_DIR".into(), config.to_str().unwrap().into()),
        ]);
        let (wrapped, history) = sandbox
            .build(
                WrapRequest {
                    disabled: &ports::Disabled::default(),
                    program: Path::new("/usr/bin/true"),
                    visible: &[],
                    env: &env,
                    resume: None,
                },
                &reach(),
            )
            .unwrap();
        let args: Vec<_> = wrapped
            .args
            .iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        assert!(
            args.windows(3)
                .any(|w| w == ["--ro-bind", cwd.to_str().unwrap(), cwd.to_str().unwrap()])
        );
        assert!(
            args.windows(2)
                .any(|w| w == ["--tmpfs", other.to_str().unwrap()])
        );
        assert!(
            args.windows(2)
                .any(|w| w == ["--remount-ro", other.to_str().unwrap()])
        );
        assert!(!args.windows(3).any(|w| w == ["--ro-bind", "/", "/"]));
        assert!(args.iter().any(|s| s == "--unshare-net"));
        assert!(args.iter().any(|s| s == "--info-fd"));
        assert!(
            history
                .unwrap()
                .binds
                .iter()
                .any(|(_, p)| p == &config.join("projects"))
        );
        assert_eq!(
            wrapped
                .env
                .iter()
                .filter(|(k, _)| k.to_ascii_lowercase().ends_with("_proxy"))
                .count(),
            8
        );
        assert!(
            sandbox
                .wrap(WrapRequest {
                    disabled: &ports::Disabled::default(),
                    program: Path::new("/usr/bin/true"),
                    visible: &[],
                    env: &env,
                    resume: None
                })
                .is_ok()
        );
        assert!(
            sandbox
                .wrap(WrapRequest {
                    disabled: &ports::Disabled::default(),
                    program: Path::new("/usr/bin/true"),
                    visible: &[],
                    env: &env,
                    resume: None
                })
                .is_err()
        );
    }
    #[test]
    fn given_account_folders_show_through_private_data() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let config = home.join(".claude");
        let cwd = dir.path().join("project");
        let data = dir.path().join("data");
        let github = data.join("accounts/config/github");
        let gone = data.join("accounts/runs/signed-out.json");
        for path in [&config, &cwd, &github] {
            std::fs::create_dir_all(path).unwrap();
        }
        let spec = SandboxSpec {
            offline: false,
            reach: reach(),
            harness: HarnessKind::ClaudeCode,
            cwd,
            state: dir.path().join("state"),
            always: vec![],
            programs: vec![],
            candidates: vec![],
            private: vec![data.clone()],
            given: vec![github.clone(), gone.clone()],
            relay: None,
            on_blocked: None,
        };
        let sandbox = LinuxSandbox::new(spec, PathBuf::from("/usr/bin/true")).unwrap();
        let env = ports::harness::RunEnv(vec![
            ("HOME".into(), home.to_str().unwrap().into()),
            ("CLAUDE_CONFIG_DIR".into(), config.to_str().unwrap().into()),
        ]);
        let (wrapped, _) = sandbox
            .build(
                WrapRequest {
                    disabled: &ports::Disabled::default(),
                    program: Path::new("/usr/bin/true"),
                    visible: &[],
                    env: &env,
                    resume: None,
                },
                &reach(),
            )
            .unwrap();
        let args: Vec<_> = wrapped
            .args
            .iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        let at = |w: &[&str]| args.windows(w.len()).position(|x| x == w);
        let github = github.to_str().unwrap();
        let masked = at(&["--tmpfs", data.to_str().unwrap()]).expect("private data masked");
        let shown = at(&["--ro-bind", github, github]).expect("sign-in folder shown");
        assert!(
            masked < shown,
            "shown after the mask, so the mask can't hide it"
        );
        assert!(!args.iter().any(|a| a == gone.to_str().unwrap()));
    }
    #[test]
    fn symlinked_codex_keeps_its_native_helper_beside_the_executed_binary() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let cwd = home.join("project");
        let native = dir.path().join("install/bin");
        let alias_dir = home.join("bin");
        for path in [&cwd, &native, &alias_dir] {
            std::fs::create_dir_all(path).unwrap();
        }
        let codex = native.join("codex");
        let helper = native.join("codex-code-mode-host");
        for executable in [&codex, &helper] {
            std::fs::write(executable, b"native executable fixture").unwrap();
            std::fs::set_permissions(executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let alias = alias_dir.join("codex");
        symlink(&codex, &alias).unwrap();
        let spec = SandboxSpec {
            offline: true,
            reach: reach(),
            harness: HarnessKind::Codex,
            cwd,
            state: dir.path().join("state"),
            always: vec![],
            programs: vec![],
            candidates: vec![],
            private: vec![],
            given: vec![],
            relay: None,
            on_blocked: None,
        };
        let sandbox = LinuxSandbox::new(spec, "/usr/bin/true".into()).unwrap();
        let env = ports::RunEnv(vec![("HOME".into(), home.to_string_lossy().into_owned())]);
        let (wrapped, _) = sandbox
            .build(
                WrapRequest {
                    disabled: &ports::Disabled::default(),
                    program: &alias,
                    visible: &[],
                    env: &env,
                    resume: None,
                },
                &reach(),
            )
            .unwrap();
        let args: Vec<_> = wrapped.args.iter().map(PathBuf::from).collect();
        assert!(
            args.windows(3)
                .any(|w| { w[0] == Path::new("--symlink") && w[1] == codex && w[2] == alias })
        );
        assert!(
            args.windows(3)
                .any(|w| { w[0] == Path::new("--ro-bind") && w[1] == helper && w[2] == helper })
        );
        assert!(
            !args
                .windows(3)
                .any(|w| { w[0] == Path::new("--ro-bind") && w[1] == codex && w[2] == alias })
        );
    }
    /// A Codex install with its helper, and a home folder to reach it from.
    struct Install {
        _dir: tempfile::TempDir,
        root: PathBuf,
        home: PathBuf,
        codex: PathBuf,
        helper: PathBuf,
    }
    impl Install {
        fn new() -> Self {
            Self::within(&std::env::temp_dir())
        }
        fn within(base: &Path) -> Self {
            use std::os::unix::fs::PermissionsExt;
            let dir = tempfile::tempdir_in(base).unwrap();
            let root = dir.path().canonicalize().unwrap();
            let home = root.join("home");
            let native = root.join("install/bin");
            for path in [&home.join("project"), &home.join("bin"), &native] {
                std::fs::create_dir_all(path).unwrap();
            }
            let codex = native.join("codex");
            let helper = native.join("codex-code-mode-host");
            for executable in [&codex, &helper] {
                std::fs::write(executable, b"native executable fixture").unwrap();
                std::fs::set_permissions(executable, std::fs::Permissions::from_mode(0o700))
                    .unwrap();
            }
            Self {
                _dir: dir,
                root,
                home,
                codex,
                helper,
            }
        }
        fn args(&self, reach: Reach, program: &Path) -> Vec<PathBuf> {
            let spec = SandboxSpec {
                offline: true,
                reach: reach.clone(),
                harness: HarnessKind::Codex,
                cwd: self.home.join("project"),
                state: self.root.join("state"),
                always: vec![],
                programs: vec![],
                candidates: vec![],
                private: vec![],
                given: vec![],
                relay: None,
                on_blocked: None,
            };
            let sandbox = LinuxSandbox::new(spec, "/usr/bin/true".into()).unwrap();
            let env = ports::RunEnv(vec![(
                "HOME".into(),
                self.home.to_string_lossy().into_owned(),
            )]);
            let (wrapped, _) = sandbox
                .build(
                    WrapRequest {
                        disabled: &ports::Disabled::default(),
                        program,
                        visible: &[],
                        env: &env,
                        resume: None,
                    },
                    &sandbox.spec.reach,
                )
                .unwrap();
            wrapped.args.iter().map(PathBuf::from).collect()
        }
    }
    fn has(args: &[PathBuf], op: &str, source: &Path, destination: &Path) -> bool {
        args.windows(3)
            .any(|w| w[0] == Path::new(op) && w[1] == source && w[2] == destination)
    }
    /// Whether any operation puts something at `path`.
    fn touches(args: &[PathBuf], path: &Path) -> bool {
        args.windows(3).any(|w| {
            ["--symlink", "--ro-bind", "--bind"]
                .iter()
                .any(|op| w[0] == Path::new(op))
                && w[2] == path
        })
    }
    fn reading(folder: &Path) -> Reach {
        Reach {
            read: vec![folder.to_owned()],
            ..reach()
        }
    }
    #[test]
    fn a_shortcut_in_a_readable_folder_is_left_to_the_hosts_own_link() {
        use std::os::unix::fs::symlink;
        for relative in [false, true] {
            let f = Install::new();
            let alias = f.home.join("bin/codex");
            if relative {
                symlink("../../install/bin/codex", &alias).unwrap();
            } else {
                symlink(&f.codex, &alias).unwrap();
            }
            let args = f.args(reading(&f.home), &alias);
            // A link or a bind there meets the host's own link and stops the run.
            assert!(!touches(&args, &alias), "relative {relative}: {args:?}");
            assert!(has(&args, "--ro-bind", &f.codex, &f.codex));
            assert!(has(&args, "--ro-bind", &f.helper, &f.helper));
        }
    }
    #[test]
    fn a_shortcut_under_whole_computer_read_is_left_to_the_hosts_own_link() {
        use std::os::unix::fs::symlink;
        // Whole-computer read still masks these host folders. Choose a writable
        // location independently of the test binary, which may itself be in /tmp.
        // Canonicalize first so a symlink into a masked folder cannot qualify.
        let masked = ["/tmp", "/run", "/dev", "/proc"];
        let base = [
            Some(std::env::temp_dir()),
            std::env::var_os("HOME").map(PathBuf::from),
            Some(PathBuf::from(env!("CARGO_MANIFEST_DIR"))),
            Some(PathBuf::from("/var/tmp")),
        ]
        .into_iter()
        .flatten()
        .find_map(|candidate| {
            let candidate = candidate.canonicalize().ok()?;
            if masked.iter().any(|p| candidate.starts_with(p)) {
                return None;
            }
            tempfile::tempdir_in(candidate).ok()
        })
        .expect("whole-computer read fixture needs a writable, unmasked host folder");
        assert!(!masked.iter().any(|p| base.path().starts_with(p)));
        let f = Install::within(base.path());
        let alias = f.home.join("bin/codex");
        symlink("../../install/bin/codex", &alias).unwrap();
        let whole = Reach {
            whole_read: true,
            ..reach()
        };
        let args = f.args(whole, &alias);
        assert!(!touches(&args, &alias), "{args:?}");
        assert!(has(&args, "--ro-bind", &f.codex, &f.codex));
    }
    #[test]
    fn a_real_file_reached_through_a_folder_link_runs_as_the_real_file() {
        use std::os::unix::fs::symlink;
        let f = Install::new();
        let tools = f.home.join("tools");
        symlink(f.root.join("install"), &tools).unwrap();
        let program = tools.join("bin/codex");
        // Hidden: the folder link is made, not the file bound onto the alias.
        let args = f.args(reach(), &program);
        assert!(has(&args, "--symlink", &f.root.join("install"), &tools));
        assert!(!touches(&args, &program), "{args:?}");
        assert!(has(&args, "--ro-bind", &f.codex, &f.codex));
        assert!(has(&args, "--ro-bind", &f.helper, &f.helper));
        // Readable: the host's own folder link is already there.
        let args = f.args(reading(&f.home), &program);
        assert!(!touches(&args, &tools) && !touches(&args, &program));
        assert!(has(&args, "--ro-bind", &f.codex, &f.codex));
    }
    #[test]
    fn a_shown_shortcut_whose_way_passes_a_hidden_link_gets_only_that_link() {
        use std::os::unix::fs::symlink;
        let f = Install::new();
        let versions = f.root.join("opt");
        std::fs::create_dir_all(&versions).unwrap();
        std::fs::rename(f.root.join("install"), versions.join("1.0")).unwrap();
        let current = versions.join("current");
        symlink("1.0", &current).unwrap();
        let alias = f.home.join("bin/codex");
        symlink(current.join("bin/codex"), &alias).unwrap();
        let codex = versions.join("1.0/bin/codex");
        let args = f.args(reading(&f.home), &alias);
        assert!(!touches(&args, &alias), "{args:?}");
        assert!(has(&args, "--symlink", &versions.join("1.0"), &current));
        assert!(has(&args, "--ro-bind", &codex, &codex));
    }
    #[test]
    fn a_shortcut_behind_private_data_is_made_there_and_not_left_dangling() {
        use std::os::unix::fs::symlink;
        // Read: Home, but the shortcut sits under a folder kept private.
        let f = Install::new();
        let private = f.home.join(".local/share/ophio/agents");
        std::fs::create_dir_all(&private).unwrap();
        let alias = private.join("codex");
        symlink(&f.codex, &alias).unwrap();
        let args = f.args(reading(&f.home), &alias);
        assert!(has(&args, "--symlink", &f.codex, &alias), "{args:?}");
    }
    #[test]
    fn what_the_sandbox_shows_follows_the_last_operation_over_a_path() {
        let args: Vec<OsString> = [
            "--tmpfs",
            "/",
            "--ro-bind",
            "/h",
            "/h",
            "--tmpfs",
            "/h/p",
            "--ro-bind",
            "/h/p/s",
            "/h/p/s",
            "--bind",
            "/x",
            "/h/y",
            "--symlink",
            "/t",
            "/h/p/l",
            "--cap-drop",
            "ALL",
        ]
        .into_iter()
        .map(Into::into)
        .collect();
        for (path, expected) in [
            ("/a", Shown::Absent),
            ("/h", Shown::Host),
            ("/h/q", Shown::Host),
            ("/h/p", Shown::Made),
            ("/h/p/q", Shown::Absent),
            ("/h/p/s/q", Shown::Host),
            ("/h/y", Shown::Made),
            ("/h/y/q", Shown::Foreign),
            ("/h/p/l", Shown::Made),
        ] {
            assert_eq!(shown(&args, Path::new(path)), expected, "{path}");
        }
    }
    #[test]
    fn accepts_nested_candidates_for_restart_based_changes() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("nested");
        std::fs::create_dir(&nested).unwrap();
        let spec = SandboxSpec {
            offline: false,
            reach: reach(),
            harness: HarnessKind::Codex,
            cwd: nested,
            state: dir.path().join("state"),
            always: vec![],
            programs: vec![],
            candidates: vec![dir.path().to_owned()],
            private: vec![],
            given: vec![],
            relay: None,
            on_blocked: None,
        };
        assert!(LinuxSandbox::new(spec, "/usr/bin/true".into()).is_ok());
    }
}
