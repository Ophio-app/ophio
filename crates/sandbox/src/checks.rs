// SPDX-License-Identifier: Apache-2.0
//! A single-use sandbox for a project check, without agent state or sign-ins.
//!
//! System runtimes are the same read-only roots used by agent sandboxes.
//! Additional toolchain paths must come from the task's existing read access;
//! this module never discovers or grants a user's whole home directory.
use std::{
    ffi::OsString,
    io,
    path::{Path, PathBuf},
    process::Command,
};

#[path = "checks_tool_preferences.rs"]
mod preferences;
#[path = "checks_tools.rs"]
mod tools;
use tools::toolchain_search_paths;

#[cfg(target_os = "linux")]
#[path = "checks_linux.rs"]
mod platform;
#[cfg(target_os = "macos")]
#[path = "checks_macos.rs"]
mod platform;

/// The protection actually applied, not what the caller requested.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SandboxLevel {
    Contained,
    ContainedWithoutNetworkLimit,
    Unsandboxed { reason: String },
}

/// Access for one check. `read` is restricted to already-permitted toolchains,
/// not other projects, sign-in folders or the agent's writable state.
#[derive(Debug, Clone)]
pub struct CheckSandboxSpec {
    pub project: PathBuf,
    pub state: PathBuf,
    pub read: Vec<PathBuf>,
    /// The caller must intersect the approved check's and task's permissions.
    pub network: bool,
    /// False only when the task's agent genuinely ran without a sandbox.
    pub network_restricted: bool,
    /// The task's private-network permission. The proxy still denies host
    /// loopback; on Mac, direct loopback follows the agent's existing access.
    pub local_network: bool,
}

#[derive(Debug)]
pub(super) struct Paths {
    pub project: PathBuf,
    pub scratch: PathBuf,
    pub home: PathBuf,
    pub read: Vec<PathBuf>,
    #[cfg(unix)]
    pub network: bool,
    #[cfg(target_os = "linux")]
    pub proxy_socket: Option<PathBuf>,
    #[cfg(target_os = "linux")]
    pub shim: Option<PathBuf>,
    #[cfg(target_os = "macos")]
    pub proxy_port: Option<u16>,
    #[cfg(target_os = "macos")]
    pub loopback: bool,
}

/// Keeps fresh scratch storage alive until the command and its tree stop.
#[derive(Debug)]
pub struct CheckSandbox {
    command: Option<Command>,
    level: SandboxLevel,
    #[cfg(unix)]
    _proxy: Option<crate::proxy::Proxy>,
    _scratch: tempfile::TempDir,
}
impl CheckSandbox {
    pub fn level(&self) -> &SandboxLevel {
        &self.level
    }
    /// Unix reserves stdout for a trusted, sandboxed startup record. The check
    /// itself receives stderr for both output streams, never this status pipe.
    pub fn startup_marker(&self) -> Option<&str> {
        if cfg!(unix) {
            Some("OPHIO_CHECK_STARTED\n")
        } else {
            None
        }
    }
    /// A sandbox can launch exactly once.
    pub fn take_command(&mut self) -> io::Result<Command> {
        self.command
            .take()
            .ok_or_else(|| io::Error::other("This check sandbox has already been used"))
    }
}

pub fn prepare(spec: CheckSandboxSpec, shell_command: &str) -> io::Result<CheckSandbox> {
    if shell_command.trim().is_empty() || shell_command.contains('\0') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "A check needs a nonempty command without a NUL character",
        ));
    }
    let project = spec.project.canonicalize()?;
    if !project.is_dir() {
        return Err(io::Error::other("The project folder is not a directory"));
    }
    let home = user_home()?;
    if home.starts_with(&project) || project.parent().is_none() {
        return Err(io::Error::other(
            "A check must run in a project folder, not the home or system root",
        ));
    }
    // Resolve an existing ancestor before creating anything: rejecting overlap
    // must itself never write into the project.
    let state = resolve_new(&spec.state)?;
    if overlaps(&project, &state) {
        return Err(io::Error::other(
            "Check storage must be outside the project folder",
        ));
    }
    let protected = protected_paths(&home, &state);
    if protected.iter().any(|p| overlaps(p, &project)) {
        return Err(io::Error::other(
            "The project overlaps private sign-in or check storage",
        ));
    }
    let mut read = Vec::new();
    for path in spec.read {
        let path = path.canonicalize()?;
        if protected.iter().any(|p| overlaps(p, &path))
            || path == home
            || home.starts_with(&path)
            || !path.is_dir()
        {
            return Err(io::Error::other(
                "A toolchain folder overlaps private sign-ins, storage or a home directory",
            ));
        }
        // Cargo's root can contain credentials and registry sign-in settings.
        // Only its bin, registry and git cache folders may be granted.
        if path.file_name().is_some_and(|n| n == ".cargo") {
            return Err(io::Error::other(
                "Grant Cargo's tool and cache folders separately, not its sign-in folder",
            ));
        }
        read.push(path);
    }
    read.sort();
    read.dedup();
    std::fs::create_dir_all(&state)?;
    let scratch = tempfile::Builder::new()
        .prefix("check-")
        .tempdir_in(&state)?;
    for name in ["home", "tmp", "home/.cargo", "cache"] {
        std::fs::create_dir_all(scratch.path().join(name))?;
    }
    // npm refuses to load one filename twice. Use distinct fresh empty files,
    // not the same null device for both user and global configuration.
    for name in ["npm-user-config", "npm-global-config"] {
        std::fs::write(scratch.path().join(name), [])?;
    }
    #[cfg(unix)]
    let proxy = if spec.network && spec.network_restricted {
        Some(crate::proxy::Proxy::start(
            scratch.path(),
            // Internet is explicitly approved; no provider-only exceptions or
            // authentication from the agent's separate proxy are reused.
            protocol::capability::HarnessKind::Codex,
            crate::proxy::Policy {
                internet: true,
                local: spec.local_network,
            },
            None,
        )?)
    } else {
        None
    };
    let paths = Paths {
        project,
        scratch: scratch.path().canonicalize()?,
        home,
        read,
        #[cfg(unix)]
        network: spec.network,
        #[cfg(target_os = "linux")]
        proxy_socket: proxy.as_ref().map(crate::proxy::Proxy::socket),
        #[cfg(target_os = "linux")]
        shim: if proxy.is_some() {
            Some(check_shim()?)
        } else {
            None
        },
        #[cfg(target_os = "macos")]
        proxy_port: proxy.as_ref().map(crate::proxy::Proxy::port),
        #[cfg(target_os = "macos")]
        loopback: spec.network && spec.local_network,
    };
    let mut command = platform_command(&paths, shell_command)?;
    command.current_dir(&paths.project).env_clear();
    for (key, value) in environment(&paths)? {
        command.env(key, value);
    }
    #[cfg(unix)]
    if let Some(proxy) = &proxy {
        #[cfg(target_os = "linux")]
        let address = "http://127.0.0.1:18080".to_owned();
        #[cfg(target_os = "macos")]
        let address = format!("http://127.0.0.1:{}", proxy.port());
        #[cfg(target_os = "linux")]
        let _ = proxy;
        for key in [
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "ALL_PROXY",
            "http_proxy",
            "https_proxy",
            "all_proxy",
        ] {
            command.env(key, &address);
        }
    }
    if spec.network {
        for key in ["NO_PROXY", "no_proxy"] {
            command.env(key, "localhost,127.0.0.1,::1");
        }
    }
    let level = if cfg!(windows) {
        SandboxLevel::Unsandboxed { reason: "Windows does not yet limit check file or network access. Its Job Object stops the process tree.".into() }
    } else if spec.network && !spec.network_restricted {
        SandboxLevel::ContainedWithoutNetworkLimit
    } else {
        SandboxLevel::Contained
    };
    Ok(CheckSandbox {
        command: Some(command),
        level,
        #[cfg(unix)]
        _proxy: proxy,
        _scratch: scratch,
    })
}

#[cfg(target_os = "linux")]
fn check_shim() -> io::Result<PathBuf> {
    let executable = std::env::current_exe()?;
    let directory = executable
        .parent()
        .ok_or_else(|| io::Error::other("The check's network helper could not be located"))?;
    let shim = directory.join("ophio-sandbox-shim");
    // Cargo puts test executables in deps, alongside the ordinary bin folder.
    let shim = if !shim.exists() && directory.file_name().is_some_and(|name| name == "deps") {
        directory
            .parent()
            .unwrap_or(directory)
            .join("ophio-sandbox-shim")
    } else {
        shim
    };
    shim.canonicalize().map_err(|error| {
        io::Error::other(format!(
            "The check's filtered network helper is unavailable. Reinstall Ophio before running checks that use the internet ({error})"
        ))
    })
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn platform_command(paths: &Paths, shell_command: &str) -> io::Result<Command> {
    // The fixed wrapper runs only after the OS sandbox is established. User
    // shell text is a separate argument, not interpolated into the wrapper.
    // Do not run any user code if the trusted startup record cannot be sent.
    let mut command = platform::command(
        paths,
        "printf 'OPHIO_CHECK_STARTED\\n' || exit 125; exec 1>&2; exec /bin/sh -c \"$1\"",
    )?;
    command.arg("ophio-check").arg(shell_command);
    Ok(command)
}
#[cfg(windows)]
fn platform_command(_paths: &Paths, shell_command: &str) -> io::Result<Command> {
    use std::os::windows::process::CommandExt;
    let system = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
    let mut command = Command::new(PathBuf::from(system).join("System32").join("cmd.exe"));
    // cmd.exe parses its own command language rather than CRT argv quoting.
    // /S removes this one outer pair, preserving the supplied shell text.
    command
        .args(["/D", "/S", "/C"])
        .raw_arg(format!("\"{shell_command}\""));
    Ok(command)
}
#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn platform_command(_: &Paths, _: &str) -> io::Result<Command> {
    Err(io::Error::other(
        "Checks are unsupported on this operating system",
    ))
}

fn user_home() -> io::Result<PathBuf> {
    let key = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    PathBuf::from(
        std::env::var_os(key)
            .ok_or_else(|| io::Error::other("The user's home folder is unavailable"))?,
    )
    .canonicalize()
}
fn overlaps(a: &Path, b: &Path) -> bool {
    a.starts_with(b) || b.starts_with(a)
}
fn resolve_new(path: &Path) -> io::Result<PathBuf> {
    if !path.is_absolute() {
        return Err(io::Error::other("Check storage must be an absolute path"));
    }
    let mut at = path;
    let mut tail = Vec::new();
    while !at.exists() {
        let name = at
            .file_name()
            .ok_or_else(|| io::Error::other("Check storage has no existing parent"))?;
        tail.push(name.to_owned());
        at = at
            .parent()
            .ok_or_else(|| io::Error::other("Check storage has no parent"))?;
    }
    let mut result = at.canonicalize()?;
    for name in tail.into_iter().rev() {
        result.push(name);
    }
    Ok(result)
}
pub(super) fn protected_paths(home: &Path, state: &Path) -> Vec<PathBuf> {
    let mut paths: Vec<_> = [
        ".claude",
        ".claude.json",
        ".npmrc",
        ".netrc",
        ".gitconfig",
        ".git-credentials",
        ".cargo/credentials",
        ".cargo/credentials.toml",
        ".codex",
        ".ssh",
        ".gnupg",
        ".config/gh",
        ".config/gcloud",
        ".aws",
        ".azure",
        ".local/share/ophio",
        ".config/ophio",
        ".cache/ophio",
        "Library/Keychains",
        "AppData/Roaming/gh",
    ]
    .into_iter()
    .map(|p| home.join(p))
    .collect();
    for key in ["CLAUDE_CONFIG_DIR", "CODEX_HOME", "GH_CONFIG_DIR"] {
        if let Some(path) = std::env::var_os(key) {
            paths.push(PathBuf::from(path));
        }
    }
    paths.push(state.to_owned());
    paths
        .into_iter()
        .map(|p| p.canonicalize().unwrap_or(p))
        .collect()
}
fn environment(paths: &Paths) -> io::Result<Vec<(OsString, OsString)>> {
    let home = paths.scratch.join("home");
    let tmp = paths.scratch.join("tmp");
    let cargo = home.join(".cargo");
    let mut search: Vec<PathBuf> = if cfg!(windows) {
        let root =
            PathBuf::from(std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into()));
        vec![
            root.join("System32"),
            root.clone(),
            root.join("System32/WindowsPowerShell/v1.0"),
        ]
    } else if cfg!(target_os = "macos") {
        [
            "/opt/homebrew/bin",
            "/opt/homebrew/sbin",
            "/usr/local/bin",
            "/usr/bin",
            "/bin",
            "/usr/sbin",
            "/sbin",
        ]
        .into_iter()
        .map(PathBuf::from)
        .collect()
    } else {
        ["/usr/local/bin", "/usr/bin", "/bin", "/usr/sbin", "/sbin"]
            .into_iter()
            .map(PathBuf::from)
            .collect()
    };
    let mut env: Vec<(OsString, OsString)> = vec![
        ("HOME".into(), home.as_os_str().into()),
        ("TMPDIR".into(), tmp.as_os_str().into()),
        ("TMP".into(), tmp.as_os_str().into()),
        ("TEMP".into(), tmp.as_os_str().into()),
        ("LANG".into(), "C.UTF-8".into()),
        ("LC_ALL".into(), "C.UTF-8".into()),
        ("CI".into(), "1".into()),
        ("NO_COLOR".into(), "1".into()),
        ("CARGO_HOME".into(), cargo.as_os_str().into()),
        (
            "GIT_CONFIG_GLOBAL".into(),
            if cfg!(windows) { "NUL" } else { "/dev/null" }.into(),
        ),
        ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
        ("PYTHONNOUSERSITE".into(), "1".into()),
        (
            "XDG_CACHE_HOME".into(),
            paths.scratch.join("cache").into_os_string(),
        ),
        (
            "NPM_CONFIG_CACHE".into(),
            paths.scratch.join("cache/npm").into_os_string(),
        ),
        (
            "UV_CACHE_DIR".into(),
            paths.scratch.join("cache/uv").into_os_string(),
        ),
        (
            "PIP_CACHE_DIR".into(),
            paths.scratch.join("cache/pip").into_os_string(),
        ),
        (
            "NPM_CONFIG_USERCONFIG".into(),
            paths.scratch.join("npm-user-config").into_os_string(),
        ),
        (
            "NPM_CONFIG_GLOBALCONFIG".into(),
            paths.scratch.join("npm-global-config").into_os_string(),
        ),
    ];
    search = toolchain_search_paths(
        &paths.read,
        std::env::var_os("PATH").as_deref(),
        &paths.project,
        &paths.home,
        &search,
    );
    for path in &paths.read {
        if path.file_name().is_some_and(|n| n == ".rustup") {
            env.push(("RUSTUP_HOME".into(), path.as_os_str().into()));
        }
        #[cfg(unix)]
        if path
            .parent()
            .and_then(Path::file_name)
            .is_some_and(|n| n == ".cargo")
            && path
                .file_name()
                .is_some_and(|n| n == "registry" || n == "git")
        {
            std::os::unix::fs::symlink(path, cargo.join(path.file_name().unwrap()))?;
        }
    }
    #[cfg(windows)]
    let search = search.into_iter().map(command_search_path);
    env.push((
        "PATH".into(),
        std::env::join_paths(search).map_err(io::Error::other)?,
    ));
    #[cfg(windows)]
    {
        let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
        env.push(("SystemRoot".into(), root));
        env.push(("USERPROFILE".into(), home.as_os_str().into()));
    }
    Ok(env)
}

#[cfg(windows)]
fn command_search_path(path: PathBuf) -> PathBuf {
    use std::path::{Component, Prefix};
    // Validate and compare canonical paths first. cmd.exe's executable search
    // does not understand their extended prefixes, so change spelling only in
    // PATH, preserving the exact validated directories and their order.
    let mut parts = path.components();
    let Some(Component::Prefix(prefix)) = parts.next() else {
        return path;
    };
    let mut ordinary = match prefix.kind() {
        Prefix::VerbatimDisk(drive) => PathBuf::from(format!("{}:", char::from(drive))),
        Prefix::VerbatimUNC(server, share) => {
            let mut prefix = OsString::from(r"\\");
            prefix.push(server);
            prefix.push(r"\");
            prefix.push(share);
            PathBuf::from(prefix)
        }
        _ => return path,
    };
    ordinary.push(parts.as_path());
    ordinary
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn command_search_understands_drive_and_unc_prefixes() {
        for (canonical, ordinary) in [
            (r"\\?\C:\Users\you\tools\bin", r"C:\Users\you\tools\bin"),
            (
                r"\\?\UNC\server\share\tools\bin",
                r"\\server\share\tools\bin",
            ),
            (r"C:\tools\bin", r"C:\tools\bin"),
        ] {
            assert_eq!(
                command_search_path(canonical.into()),
                PathBuf::from(ordinary)
            );
        }
    }

    #[test]
    fn storage_overlap_is_rejected_before_creation() {
        let project = tempfile::tempdir().unwrap();
        let state = project.path().join("new/data");
        assert!(
            prepare(
                CheckSandboxSpec {
                    project: project.path().into(),
                    state: state.clone(),
                    read: vec![],
                    network: false,
                    network_restricted: true,
                    local_network: false,
                },
                "echo hi"
            )
            .is_err()
        );
        assert!(!state.exists());
    }
    #[cfg(unix)]
    #[test]
    fn network_on_bypasses_only_loopback_in_both_proxy_spellings() {
        let project = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        for restricted in [false, true] {
            let mut sandbox = prepare(
                CheckSandboxSpec {
                    project: project.path().into(),
                    state: state.path().into(),
                    read: vec![],
                    network: true,
                    network_restricted: restricted,
                    local_network: false,
                },
                "true",
            )
            .unwrap();
            let command = sandbox.take_command().unwrap();
            for name in ["NO_PROXY", "no_proxy"] {
                let value = command.get_envs().find(|(key, _)| *key == name).unwrap().1;
                assert_eq!(value.unwrap(), "localhost,127.0.0.1,::1");
            }
        }
    }

    #[test]
    fn command_is_single_use_and_environment_is_fixed() {
        let project = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let mut sandbox = prepare(
            CheckSandboxSpec {
                project: project.path().into(),
                state: state.path().into(),
                read: vec![],
                network: false,
                network_restricted: true,
                local_network: false,
            },
            "echo hi",
        )
        .unwrap();
        let command = sandbox.take_command().unwrap();
        let keys: Vec<_> = command
            .get_envs()
            .map(|(k, _)| k.to_string_lossy().into_owned())
            .collect();
        for key in [
            "GH_TOKEN",
            "GITHUB_TOKEN",
            "ANTHROPIC_API_KEY",
            "OPENAI_API_KEY",
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "ALL_PROXY",
            "CLAUDE_CONFIG_DIR",
            "CODEX_HOME",
        ] {
            assert!(!keys.iter().any(|k| k == key));
        }
        let config_paths: Vec<_> = ["NPM_CONFIG_USERCONFIG", "NPM_CONFIG_GLOBALCONFIG"]
            .into_iter()
            .map(|name| {
                PathBuf::from(
                    command
                        .get_envs()
                        .find(|(key, _)| *key == name)
                        .unwrap()
                        .1
                        .unwrap(),
                )
            })
            .collect();
        // npm rejects loading one path twice, even when it is the null device.
        assert_ne!(config_paths[0], config_paths[1]);
        for path in config_paths {
            assert!(path.starts_with(sandbox._scratch.path().canonicalize().unwrap()));
            assert!(std::fs::read(path).unwrap().is_empty());
        }
        assert!(sandbox.take_command().is_err());
    }
}
