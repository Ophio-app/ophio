// SPDX-License-Identifier: Apache-2.0
//! Mac conversation isolation, through the system's own sandbox. A profile that
//! denies everything is applied before the agent's program runs and can't be
//! changed afterwards, so what the agent may open is fixed for the turn: a change
//! of folders ends the turn and the next one starts with the new profile. Its
//! internet goes through the same proxy as on Linux, whose policy can change at
//! any time.
use crate::{
    error,
    history::History,
    program,
    proxy::{Policy, Proxy},
};
use ports::{
    HarnessError,
    sandbox::{
        Applied, Reach, RelayEndpoint, RelayTotals, Sandbox, SandboxSpec, WrapRequest, Wrapped,
    },
};
use protocol::capability::{Availability, HarnessKind};
use std::{
    io::{self, Write},
    net::{SocketAddr, TcpStream},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex},
};

const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

/// The system's own certificates as one file, inside `/private/etc`.
const CERTIFICATES: &str = "/private/etc/ssl/cert.pem";

/// What every program needs to start and run, and nothing of the person's.
const RUNTIME: &[&str] = &[
    "/usr",
    "/bin",
    "/sbin",
    "/System",
    "/Library/Apple",
    "/Library/Developer/CommandLineTools",
    "/Applications/Xcode.app",
    // Homebrew's programs and libraries, and the certificates they connect with.
    // Not its `var` and the rest of `etc`, where services keep their data.
    "/opt/homebrew/bin",
    "/opt/homebrew/sbin",
    "/opt/homebrew/lib",
    "/opt/homebrew/libexec",
    "/opt/homebrew/opt",
    "/opt/homebrew/Cellar",
    "/opt/homebrew/Caskroom",
    "/opt/homebrew/Frameworks",
    "/opt/homebrew/include",
    "/opt/homebrew/share",
    "/opt/homebrew/etc/ca-certificates",
    "/private/etc",
    "/private/var/db/timezone",
    "/private/var/db/xcode_select_link",
    "/private/var/select",
    // Xcode's tools check here that its licence was accepted.
    "/Library/Preferences/com.apple.dt.Xcode.plist",
];

/// Inside `RUNTIME`, what services keep rather than what programs run from.
const RUNTIME_DATA: &[&str] = &["/usr/local/var", "/usr/local/etc"];

/// How a program runs at all. `system.sb` is the system's own list of what its
/// libraries reach for, so it follows the system across updates.
///
/// A terminal may be opened only if this run made it, which is what the system's
/// extension says, so another of the person's terminals can't be written to.
/// Named shared memory isn't given beyond `system.sb`'s own, and named semaphores
/// only under the name Python's locks use: neither is a file or an address, so no
/// other rule keeps them from another program's. Without that one name, Python's
/// multiprocessing can't start at all. Another program's semaphore under that name
/// can be reached if its name is known, which carries no data; Python's own are
/// random and unlinked as soon as they are made.
const BASE: &str = r#"(version 1)
(deny default)
(import "system.sb")
(allow process-fork process-exec)
(allow signal (target same-sandbox))
(allow process-info* (target same-sandbox))
(allow file-read-metadata)
(allow sysctl-read)
(allow system-socket)
(allow ipc-posix-sem (ipc-posix-name-prefix "/mp-"))
(allow file-read* (regex #"^/opt/homebrew/etc/openssl@[0-9.]+(/|$)"))
(allow file-map-executable)
(allow pseudo-tty)
(allow file-read* file-write* file-ioctl
  (literal "/dev/null" "/dev/zero" "/dev/random" "/dev/urandom" "/dev/tty" "/dev/ptmx" "/dev/dtracehelper")
  (subpath "/dev/fd"))
(allow file-read* file-write*
  (require-all (regex #"^/dev/ttys[0-9]+$") (extension "com.apple.sandbox.pty")))
(allow file-ioctl (regex #"^/dev/ttys[0-9]+$"))
"#;

/// Holds agents with the system's sandbox.
#[derive(Debug)]
pub struct Seatbelt {
    probe: std::sync::OnceLock<Availability>,
}
impl Seatbelt {
    pub fn new() -> Self {
        Self {
            probe: std::sync::OnceLock::new(),
        }
    }
}
impl Default for Seatbelt {
    fn default() -> Self {
        Self::new()
    }
}
#[async_trait::async_trait]
impl ports::sandbox::Sandboxes for Seatbelt {
    fn availability(&self) -> Availability {
        self.probe.get_or_init(probe).clone()
    }
    fn create(&self, spec: SandboxSpec) -> Result<Arc<dyn Sandbox>, HarnessError> {
        if spec.reach.unrestricted() {
            return Err(error("unrestricted runs must not create a sandbox"));
        }
        if !self.availability().is_available() {
            return Err(error(format!(
                "sandbox unavailable: {:?}",
                self.availability()
            )));
        }
        Ok(Arc::new(MacSandbox::new(spec)?))
    }
}

fn probe() -> Availability {
    match Command::new(SANDBOX_EXEC)
        .args(["-p", BASE, "/usr/bin/true"])
        .output()
    {
        Ok(output) if output.status.success() => Availability::Available,
        Ok(output) => Availability::Unsupported {
            reason: format!(
                "This Mac's sandbox would not start ({}), so chats can't be limited to a project or kept off the internet.",
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        },
        Err(e) => Availability::Unsupported {
            reason: format!(
                "This Mac has no sandbox program at {SANDBOX_EXEC} ({e}), so chats can't be limited to a project or kept off the internet."
            ),
        },
    }
}

/// Runtime folders that exist here, as real paths.
pub fn runtime_paths() -> &'static [&'static str] {
    RUNTIME
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

/// A path as the profile's language writes a string.
fn quoted(path: &Path) -> String {
    let text = path.to_string_lossy();
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        if c == '"' || c == '\\' {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// The paths and rules a run's profile is made from.
#[derive(Debug, Default)]
struct Profile<'a> {
    whole_read: bool,
    whole_write: bool,
    /// Folders it may read and may change.
    read: Vec<&'a Path>,
    write: Vec<&'a Path>,
    /// Application data, kept from even a run that may read everything.
    private: Vec<&'a Path>,
    /// Patterns for application data that may appear later under a name that
    /// isn't known yet.
    families: Vec<String>,
    /// Folders above private data, which can't be moved.
    above: Vec<&'a Path>,
    /// Folders above private data that don't exist yet. A link made at one of
    /// these names could lead the data somewhere the agent may read.
    missing_above: Vec<&'a Path>,
    /// Given back after `private`: what this run needs whatever its reach.
    own: Vec<&'a Path>,
    /// Given back after `private`, to read only.
    visible: Vec<&'a Path>,
    /// The proxy's port, the only place on the network it may reach.
    proxy: Option<u16>,
    /// This computer's own servers too, along with the person's networks.
    loopback: bool,
}

impl Profile<'_> {
    /// Later rules win, so private data is taken back after folders are given,
    /// and only this run's own files are given again after that.
    fn text(&self) -> String {
        let mut text = String::from(BASE);
        let rule = |text: &mut String, rule: &str, paths: &[&Path], form: &str| {
            if !paths.is_empty() {
                text.push('(');
                text.push_str(rule);
                for path in paths {
                    text.push_str(" (");
                    text.push_str(form);
                    text.push(' ');
                    text.push_str(&quoted(path));
                    text.push(')');
                }
                text.push_str(")\n");
            }
        };
        let runtime: Vec<&Path> = RUNTIME.iter().map(Path::new).collect();
        rule(&mut text, "allow file-read*", &runtime, "subpath");
        let runtime_data: Vec<&Path> = RUNTIME_DATA.iter().map(Path::new).collect();
        rule(&mut text, "deny file-read*", &runtime_data, "subpath");
        rule(&mut text, "allow file-read*", &self.read, "subpath");
        rule(
            &mut text,
            "allow file-read* file-write*",
            &self.write,
            "subpath",
        );
        rule(
            &mut text,
            "deny file-read* file-write*",
            &self.private,
            "subpath",
        );
        if !self.families.is_empty() {
            text.push_str("(deny file-read* file-write*");
            for family in &self.families {
                text.push_str(" (regex #\"");
                text.push_str(family);
                text.push_str("\")");
            }
            text.push_str(")\n");
        }
        rule(
            &mut text,
            "allow file-read* file-write*",
            &self.own,
            "subpath",
        );
        rule(&mut text, "allow network-outbound", &self.own, "subpath");
        rule(&mut text, "allow file-read*", &self.visible, "subpath");
        if self.whole_read {
            let mut whole = String::from("(allow file-read*)\n");
            if self.whole_write {
                whole.push_str("(allow file-write*)\n");
            }
            // In front of the rules that take back private data.
            let at = text
                .find("(deny file-read* file-write*")
                .unwrap_or(text.len());
            text.insert_str(at, &whole);
        }
        if let Some(port) = self.proxy {
            text.push_str(&format!(
                "(allow network-outbound (remote tcp \"localhost:{port}\"))\n"
            ));
        }
        if self.loopback {
            text.push_str(
                "(allow network-outbound (remote ip \"localhost:*\"))\n(allow network-inbound (local ip \"localhost:*\"))\n(allow network-bind (local ip \"localhost:*\"))\n",
            );
        } else {
            // It may serve; only the computer itself can come to it.
            text.push_str(
                "(allow network-inbound (local ip \"localhost:*\"))\n(allow network-bind (local ip \"localhost:*\"))\n",
            );
        }
        // Rules go by path, so moving a folder above private data would take the
        // data out from under them. Last, so nothing given above can undo it.
        for (rule, paths) in [
            ("deny file-write-unlink", &self.above),
            ("deny file-write-create", &self.missing_above),
        ] {
            if !paths.is_empty() {
                text.push('(');
                text.push_str(rule);
                for path in paths {
                    text.push_str(" (literal ");
                    text.push_str(&quoted(path));
                    text.push(')');
                }
                text.push_str(")\n");
            }
        }
        text
    }
}

#[derive(Debug)]
struct Run {
    reach: Reach,
    /// What the running profile lets it reach on this computer's loopback.
    loopback: bool,
    history: Option<History>,
    session: Option<String>,
    /// A sign-in copied for this run, and where it goes back to if it changes.
    keychain: Option<Keychain>,
    credentials: Option<Credentials>,
    wrapped: bool,
    restart: bool,
}

/// The agent's sign-in file, reached from its own folder by a link. A program
/// that replaces the file rather than writing into it replaces the link, so
/// what it wrote is carried back once the run has ended.
#[derive(Debug, Clone)]
struct Credentials {
    link: PathBuf,
    real: PathBuf,
    /// What the real file held when the run began, by its digest. One that
    /// changed since was signed in again meanwhile, from the phone say, and
    /// the run's older sign-in doesn't go back over it.
    began: Option<[u8; 32]>,
}

/// Claude Code keeps its sign-in in the login keychain, which a sandboxed
/// program can't reach. A copy made for the run goes back if the run renewed it.
#[derive(Debug, Clone)]
struct Keychain {
    service: String,
    account: String,
    file: PathBuf,
    given: Vec<u8>,
}

#[derive(Debug)]
pub(crate) struct MacSandbox {
    spec: SandboxSpec,
    proxy: Option<Proxy>,
    temp: tempfile::TempDir,
    native: Option<tempfile::TempDir>,
    run: Mutex<Run>,
}

impl MacSandbox {
    pub(crate) fn new(mut spec: SandboxSpec) -> Result<Self, HarnessError> {
        if !matches!(spec.harness, HarnessKind::ClaudeCode | HarnessKind::Codex) {
            return Err(error(
                "only Claude Code and Codex provider policies are supported",
            ));
        }
        if spec.relay.is_some() {
            return Err(error("model-only runs need Linux"));
        }
        spec.cwd = canonical(&spec.cwd)?;
        spec.reach = normalize(spec.reach)?;
        if spec.reach.unrestricted() {
            return Err(error("unrestricted runs must bypass sandbox creation"));
        }
        crate::sys::directories(&spec.state).map_err(error)?;
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
        if spec.candidates.iter().any(|path| !path.is_dir()) {
            return Err(error("candidate grants must be directories"));
        }
        if spec.offline && (spec.reach.internet || spec.reach.local_network) {
            return Err(error("offline sandboxes cannot enable network access"));
        }
        let native = spec
            .offline
            .then(|| {
                tempfile::Builder::new()
                    .prefix("relay-native-")
                    .tempdir_in(&spec.state)
                    .map_err(error)
            })
            .transpose()?;
        let proxy = (!spec.offline)
            .then(|| {
                Proxy::start(
                    &spec.state,
                    spec.harness,
                    policy(&spec.reach),
                    spec.on_blocked.clone(),
                )
                .map_err(error)
            })
            .transpose()?;
        // Short and its own: a program may put a socket here, and a socket's
        // path has little room on a Mac. /tmp itself is shared with everything.
        let temp = tempfile::Builder::new()
            .prefix("ophio-run-")
            .tempdir_in("/private/tmp")
            .map_err(error)?;
        let loopback = spec.reach.internet && spec.reach.local_network;
        let run = Run {
            reach: spec.reach.clone(),
            loopback,
            history: None,
            session: None,
            keychain: None,
            credentials: None,
            wrapped: false,
            restart: false,
        };
        Ok(Self {
            spec,
            proxy,
            temp,
            native,
            run: Mutex::new(run),
        })
    }

    fn build(&self, request: WrapRequest<'_>, run: &mut Run) -> Result<Wrapped, HarnessError> {
        let reach = run.reach.clone();
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
        let (variable, default) = match self.spec.harness {
            HarnessKind::ClaudeCode => ("CLAUDE_CONFIG_DIR", ".claude"),
            HarnessKind::Codex => ("CODEX_HOME", ".codex"),
            _ => return Err(error("unsupported provider")),
        };
        let named_config = env(variable);
        let host_config = named_config
            .clone()
            .map_or_else(|| home.join(default), PathBuf::from);
        if !host_config.exists() && host_config.is_absolute() {
            use std::os::unix::fs::DirBuilderExt;
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&host_config)
                .map_err(|e| error(format!("{}: {e}", host_config.display())))?;
        }
        let host_config = canonical(&host_config)?;

        // Kept from the agent whether or not they exist yet: a folder made after
        // this has no other rule to hold it.
        let mut required: Vec<PathBuf> = self.spec.private.clone();
        required.extend([
            home.join(".claude"),
            home.join(".codex"),
            home.join(".claude.json"),
            env("CLAUDE_CONFIG_DIR").map_or_else(|| home.join(".claude"), PathBuf::from),
            env("CODEX_HOME").map_or_else(|| home.join(".codex"), PathBuf::from),
        ]);
        required.extend(mac_private_paths(&home));
        let required: Vec<PathBuf> = required.iter().flat_map(|p| spellings(p)).collect();
        let mut private: Vec<PathBuf> = private_paths(&home)?
            .iter()
            .flat_map(|p| spellings(p))
            .chain(required.iter().cloned())
            .collect();
        private.sort();
        private.dedup();
        let families = private_families(&home)?;
        let mut above: Vec<PathBuf> = private
            .iter()
            .flat_map(|path| path.ancestors().skip(1))
            .chain(
                family_folders(&home)
                    .iter()
                    .flat_map(|path| path.ancestors()),
            )
            .filter(|path| path.parent().is_some())
            .map(Path::to_path_buf)
            .collect();
        above.sort();
        above.dedup();
        let mut missing_above: Vec<PathBuf> = required
            .iter()
            .flat_map(|path| path.ancestors().skip(1))
            .filter(|path| path.parent().is_some() && std::fs::symlink_metadata(path).is_err())
            .map(Path::to_path_buf)
            .collect();
        missing_above.sort();
        missing_above.dedup();
        for path in &self.spec.candidates {
            if private.iter().any(|p| path.starts_with(p)) || path.starts_with(&host_config) {
                return Err(error(
                    "grants must not expose provider configuration or Ophio private data",
                ));
            }
        }

        // The agent works from a folder of this conversation's, not its own:
        // only the sign-in and the history of this conversation are in it.
        let mut own: Vec<PathBuf> = vec![canonical(self.temp.path())?];
        let config = if let Some(native) = &self.native {
            if request.resume.is_some() {
                return Err(error("offline runs start with fresh provider state"));
            }
            canonical(native.path())?
        } else {
            let history = History::isolated(
                self.spec.harness,
                &host_config,
                &self.spec.state,
                &self.spec.cwd,
                request.resume,
            )
            .map_err(|e| match e.get_ref() {
                Some(unsaved) if unsaved.is::<crate::history::Unsaved>() => error(unsaved),
                _ => error(format!("native history: {e}")),
            })?;
            let local = canonical(&self.spec.state.join("native"))?;
            for (source, destination) in &history.binds {
                if destination == &host_config {
                    continue;
                }
                let name = destination
                    .strip_prefix(&host_config)
                    .map_err(|_| error("native history outside the agent's folder"))?;
                let link = local.join(name);
                if let Some(parent) = link.parent() {
                    crate::sys::directories(parent).map_err(error)?;
                }
                linked(source, &link).map_err(error)?;
                let source = canonical(source)?;
                if source.is_file() && source.starts_with(&host_config) {
                    run.credentials = Some(Credentials::new(link, source.clone()));
                }
                own.push(source);
            }
            run.history = Some(history);
            if self.spec.harness == HarnessKind::ClaudeCode && run.credentials.is_none() {
                run.keychain =
                    Keychain::copy(named_config.as_deref(), &home, &local).map_err(error)?;
            }
            local
        };
        own.push(config.clone());
        own.extend(self.spec.always.iter().cloned());

        let mut programs = program::files(request.program, &home)
            .map_err(|e| error(format!("agent executable: {e}")))?;
        for other in &self.spec.programs {
            programs.extend(
                program::files(other, &home)
                    .map_err(|e| error(format!("{}: {e}", other.display())))?,
            );
        }
        let mut visible: Vec<PathBuf> = programs
            .iter()
            .map(|path| canonical(path))
            .collect::<Result<_, _>>()?;
        // A program's own folder of libraries, where it lives outside the system's.
        for path in visible.clone() {
            if let Some(package) = package_of(&path) {
                visible.push(package);
            }
        }
        for path in request.visible {
            visible.push(canonical(path)?);
        }
        // An account signed out since the run was given it leaves nothing.
        for path in self.spec.given.iter().filter(|p| p.exists()) {
            visible.push(canonical(path)?);
        }

        let read: Vec<&Path> = reach
            .read
            .iter()
            .chain(std::iter::once(&self.spec.cwd))
            .map(PathBuf::as_path)
            .collect();
        let profile = Profile {
            whole_read: reach.whole_read,
            whole_write: reach.whole_write,
            read,
            write: reach.write.iter().map(PathBuf::as_path).collect(),
            private: private.iter().map(PathBuf::as_path).collect(),
            families,
            above: above.iter().map(PathBuf::as_path).collect(),
            missing_above: missing_above.iter().map(PathBuf::as_path).collect(),
            own: own.iter().map(PathBuf::as_path).collect(),
            visible: visible.iter().map(PathBuf::as_path).collect(),
            proxy: self.proxy.as_ref().map(Proxy::port),
            loopback: run.loopback,
        }
        .text();

        let mut environment: Vec<(String, String)> = match &self.proxy {
            Some(proxy) => {
                let address = format!("http://127.0.0.1:{}", proxy.port());
                [
                    "HTTPS_PROXY",
                    "HTTP_PROXY",
                    "ALL_PROXY",
                    "https_proxy",
                    "http_proxy",
                    "all_proxy",
                ]
                .into_iter()
                .map(|k| (k.into(), address.clone()))
                .collect()
            }
            None => vec![],
        };
        for key in ["NO_PROXY", "no_proxy"] {
            environment.push((key.into(), "localhost,127.0.0.1".into()));
        }
        if self.spec.offline {
            environment.push((
                "PATH".into(),
                std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".into()),
            ));
            environment.push(("LANG".into(), "en_US.UTF-8".into()));
        }
        let temp = canonical(self.temp.path())?;
        environment.push(("TMPDIR".into(), temp.to_string_lossy().into_owned()));
        // Claude Code keeps its own files in the shared /tmp/claude-<uid>, whatever
        // TMPDIR says, and can't start where that is out of reach. Its own
        // setting puts them in this run's folder instead.
        environment.push((
            "CLAUDE_CODE_TMPDIR".into(),
            temp.to_string_lossy().into_owned(),
        ));
        // Programs that check certificates through the system ask the keychain's
        // service, which is held because it would also hand out the person's
        // keychain. The system's own list of certificates, a plain file, does
        // for Codex and others that read SSL_CERT_FILE. A list the person chose
        // is kept.
        if std::env::var_os("SSL_CERT_FILE").is_none() && Path::new(CERTIFICATES).is_file() {
            environment.push(("SSL_CERT_FILE".into(), CERTIFICATES.into()));
        }
        // Xcode's tools otherwise keep a cache of where they are in the person's
        // shared temporary folder, which the person's own runs of them would read.
        environment.push(("xcrun_nocache".into(), "1".into()));
        environment.push((
            "xcrun_db".into(),
            temp.join("xcrun_db").to_string_lossy().into_owned(),
        ));
        environment.push(("HOME".into(), home.to_string_lossy().into_owned()));
        environment.push((variable.into(), config.to_string_lossy().into_owned()));
        Ok(Wrapped {
            program: SANDBOX_EXEC.into(),
            args: vec!["-p".into(), profile.into()],
            env: environment,
            clear_env: self.spec.offline,
        })
    }
}

/// The folder a program's own files are in when it was installed as a package,
/// as npm and Homebrew do: `…/lib/node_modules/x/bin/x.js` needs all of `x`,
/// and `…/Cellar/x/1.0/bin/x` all of that version.
fn package_of(program: &Path) -> Option<PathBuf> {
    let mut parts = program.ancestors();
    for path in parts.by_ref() {
        let Some(parent) = path.parent() else { break };
        let parent_name = parent.file_name().and_then(|n| n.to_str());
        if parent_name == Some("node_modules") {
            return Some(path.to_path_buf());
        }
        if parent_name.is_some_and(|n| n.starts_with('@'))
            && parent
                .parent()
                .and_then(Path::file_name)
                .and_then(|n| n.to_str())
                == Some("node_modules")
        {
            return Some(path.to_path_buf());
        }
        if parent
            .parent()
            .and_then(Path::file_name)
            .and_then(|n| n.to_str())
            == Some("Cellar")
        {
            return Some(path.to_path_buf());
        }
    }
    // Native installers keep a version's helpers beside it.
    let folder = program.parent()?;
    let versioned = folder.file_name().and_then(|n| n.to_str()) == Some("versions");
    versioned.then(|| folder.to_path_buf())
}

/// Makes `link` lead to `source`, in place of anything an earlier turn left.
fn linked(source: &Path, link: &Path) -> io::Result<()> {
    match std::fs::symlink_metadata(link) {
        Ok(metadata) if metadata.is_dir() && !metadata.is_symlink() => {
            std::fs::remove_dir_all(link)?;
        }
        Ok(_) => std::fs::remove_file(link)?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    std::os::unix::fs::symlink(source, link)
}

/// What Linux's folders for Øphio's data hold here, if an install has any, and
/// what exists now under the names [`private_families`] covers.
fn private_paths(home: &Path) -> Result<Vec<PathBuf>, HarnessError> {
    let mut paths: Vec<PathBuf> = [
        home.join(".local/share/ophio"),
        home.join(".cache/ophio"),
        home.join(".config/ophio"),
    ]
    .into_iter()
    .filter(|path| path.exists())
    .collect();
    for (folder, prefixes) in family_prefixes(home) {
        if let Ok(entries) = std::fs::read_dir(folder) {
            for entry in entries {
                let entry = entry.map_err(error)?;
                let name = entry.file_name();
                if prefixes
                    .iter()
                    .any(|prefix| name.as_encoded_bytes().starts_with(prefix.as_bytes()))
                {
                    paths.push(entry.path());
                }
            }
        }
    }
    Ok(paths)
}

/// Where Øphio keeps its data on a Mac, whether or not it is there yet.
fn mac_private_paths(home: &Path) -> Vec<PathBuf> {
    vec![
        home.join("Library/Application Support/ophio"),
        home.join("Library/Caches/ophio"),
        home.join("Library/Preferences/ophio.plist"),
    ]
}

/// Folders that hold Øphio's data under names starting with these.
fn family_prefixes(home: &Path) -> [(PathBuf, &'static [&'static str]); 4] {
    [
        (home.join(".config"), &["ophio"]),
        (
            home.join("Library/Application Support"),
            &["ophio", "app.ophio"],
        ),
        (home.join("Library/Caches"), &["ophio", "app.ophio"]),
        (home.join("Library/Preferences"), &["ophio", "app.ophio"]),
    ]
}

fn family_folders(home: &Path) -> Vec<PathBuf> {
    family_prefixes(home)
        .into_iter()
        .map(|(folder, _)| folder)
        .collect()
}

/// Patterns for everything under those names, so data that appears after the
/// run starts, under a name not seen yet, is covered too.
fn private_families(home: &Path) -> Result<Vec<String>, HarnessError> {
    family_prefixes(home)
        .into_iter()
        .flat_map(|(folder, prefixes)| prefixes.iter().map(move |p| (folder.clone(), *p)))
        .map(|(folder, prefix)| {
            let path = folder.join(prefix);
            let path = path
                .to_str()
                .filter(|path| !path.contains('"'))
                .ok_or_else(|| {
                    error("this home folder's name can't be written in a sandbox rule")
                })?;
            Ok(format!("^{}", regex_escaped(path)))
        })
        .collect()
}

/// A path as a pattern that matches only itself.
fn regex_escaped(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for c in path.chars() {
        if ".^$*+?()[]{}|\\".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// A path with what exists of it resolved, as the system compares it: rules are
/// matched against real paths, so `/tmp/x` in a rule protects nothing, while the
/// file is at `/private/tmp/x`. The rest, which doesn't exist yet, is kept as it is.
fn resolved(path: &Path) -> PathBuf {
    let mut missing = Vec::new();
    let mut here = path;
    loop {
        if let Ok(mut real) = here.canonicalize() {
            real.extend(missing.iter().rev());
            return real;
        }
        match (here.parent(), here.file_name()) {
            (Some(parent), Some(name)) => {
                missing.push(name);
                here = parent;
            }
            _ => return path.to_path_buf(),
        }
    }
}

/// Where a private path is now, and the name it was given, in case a link there
/// comes to lead somewhere else.
fn spellings(path: &Path) -> Vec<PathBuf> {
    let real = resolved(path);
    if real == path {
        vec![real]
    } else {
        vec![real, path.to_path_buf()]
    }
}

impl Keychain {
    /// The keychain entry Claude Code uses: its own folder, when named, is part
    /// of the entry's name.
    fn service(named_config: Option<&str>) -> String {
        use sha2::Digest;
        match named_config {
            None => "Claude Code-credentials".into(),
            Some(folder) => {
                let digest = sha2::Sha256::digest(folder.as_bytes());
                let hex: String = digest.iter().take(4).map(|b| format!("{b:02x}")).collect();
                format!("Claude Code-credentials-{hex}")
            }
        }
    }

    /// Copies the sign-in for the run into its own folder, owner-only, where
    /// Claude Code looks when the keychain doesn't answer. Nothing to copy, as
    /// with no sign-in, is for the agent to say.
    fn copy(named_config: Option<&str>, home: &Path, local: &Path) -> io::Result<Option<Self>> {
        let account = std::env::var("USER")
            .ok()
            .or_else(|| home.file_name().map(|n| n.to_string_lossy().into_owned()))
            .unwrap_or_default();
        let service = Self::service(named_config);
        let output = Command::new("/usr/bin/security")
            .args(["find-generic-password", "-s", &service, "-w"])
            .stdin(std::process::Stdio::null())
            .output()?;
        if !output.status.success() {
            return Ok(None);
        }
        let mut given = output.stdout;
        while given.last().is_some_and(u8::is_ascii_whitespace) {
            given.pop();
        }
        if given.is_empty() {
            return Ok(None);
        }
        let file = local.join(".credentials.json");
        // A file an earlier run left, or a link planted there, is not written through.
        match std::fs::remove_file(&file) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        let mut out = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&file)?;
        out.write_all(&given)?;
        out.sync_all()?;
        Ok(Some(Self {
            service,
            account,
            file,
            given,
        }))
    }

    /// What the keychain holds for the service now; none if the entry is
    /// gone or `security` couldn't say.
    fn held(&self) -> Option<Vec<u8>> {
        let output = Command::new("/usr/bin/security")
            .args(["find-generic-password", "-s", &self.service, "-w"])
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .ok()
            .filter(|o| o.status.success())?;
        let mut held = output.stdout;
        while held.last().is_some_and(u8::is_ascii_whitespace) {
            held.pop();
        }
        Some(held)
    }

    /// Takes the copy away, and carries a renewed sign-in back to the keychain.
    /// `security`'s own prompt takes the value, so it never shows in a process
    /// list. A sign-in made since the run began, from the phone say, stays:
    /// the run's older one doesn't go back over it.
    fn finish(&self) -> io::Result<()> {
        // The agent could write here, so nothing it left is followed: a link or
        // a second name for a private file would carry that file to the
        // keychain, and from there into the next run. Such a file is dropped.
        let now = match read_own(&self.file) {
            Ok(now) => now,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(_) => {
                std::fs::remove_file(&self.file)?;
                return Ok(());
            }
        };
        std::fs::remove_file(&self.file)?;
        if now == self.given || now.is_empty() {
            return Ok(());
        }
        // Only over the sign-in it was given: one replaced, signed out (the
        // entry gone) or that can't be read stays as it is.
        if self.held().as_deref() != Some(self.given.as_slice()) {
            return Ok(());
        }
        let hex: String = now.iter().map(|b| format!("{b:02x}")).collect();
        let mut child = Command::new("/usr/bin/security")
            .arg("-i")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()?;
        if let Some(mut input) = child.stdin.take() {
            writeln!(
                input,
                "add-generic-password -U -a {} -s {} -X {hex}",
                sh_word(&self.account),
                sh_word(&self.service)
            )?;
        }
        if child.wait()?.success() {
            Ok(())
        } else {
            Err(io::Error::other(
                "the keychain would not take the renewed sign-in",
            ))
        }
    }
}

/// A regular file with one name, reached through no link, and only so much of it.
fn read_own(path: &Path) -> io::Result<Vec<u8>> {
    const SIGN_IN_LIMIT: u64 = 1 << 20;
    let file = crate::sys::history_file(path, false)?;
    let mut bytes = Vec::new();
    io::Read::read_to_end(&mut io::Read::take(file, SIGN_IN_LIMIT), &mut bytes)?;
    Ok(bytes)
}

/// A word as `security -i` reads one.
fn sh_word(word: &str) -> String {
    format!("\"{}\"", word.replace('\\', "\\\\").replace('"', "\\\""))
}

impl Credentials {
    fn new(link: PathBuf, real: PathBuf) -> Self {
        let began = Self::digest(&real);
        Self { link, real, began }
    }
    fn digest(path: &Path) -> Option<[u8; 32]> {
        use sha2::Digest;
        read_own(path)
            .ok()
            .map(|bytes| sha2::Sha256::digest(bytes).into())
    }
    /// Carries back a sign-in the agent replaced rather than wrote into,
    /// unless the real one was signed in again since the run began.
    fn finish(&self) -> io::Result<()> {
        let metadata = match std::fs::symlink_metadata(&self.link) {
            Ok(metadata) => metadata,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        };
        if !metadata.is_file() {
            return Ok(());
        }
        // Nothing the agent left is followed, as for the keychain's copy.
        let renewed = read_own(&self.link).unwrap_or_default();
        if !renewed.is_empty() && Self::digest(&self.real) == self.began {
            crate::sys::replace_history(&self.real, |file| file.write_all(&renewed))?;
        }
        std::fs::remove_file(&self.link)
    }
}

fn restart(reason: impl ToString) -> Applied {
    Applied::NeedsRestart(reason.to_string())
}

#[async_trait::async_trait]
impl Sandbox for MacSandbox {
    fn relay(&self) -> Option<RelayEndpoint> {
        None
    }
    fn relay_totals(&self) -> Option<RelayTotals> {
        None
    }
    fn wrap(&self, request: WrapRequest<'_>) -> Result<Wrapped, HarnessError> {
        let mut run = self.run.lock().unwrap();
        if run.wrapped {
            return Err(error("a sandbox is single-use; create one per run"));
        }
        let wrapped = self.build(request, &mut run)?;
        run.wrapped = true;
        Ok(wrapped)
    }
    fn prepare(&self, _command: &mut Command) -> io::Result<()> {
        Ok(())
    }
    async fn started(&self) -> Result<(), HarnessError> {
        Ok(())
    }
    async fn update(&self, reach: Reach) -> Applied {
        if self.spec.offline
            && (reach.internet || reach.local_network || reach.whole_read || reach.whole_write)
        {
            return restart("Offline sandboxes cannot gain network or whole-computer access");
        }
        let reach = match normalize(reach) {
            Ok(reach) => reach,
            Err(e) => return restart(e),
        };
        let mut run = self.run.lock().unwrap();
        if let Some(proxy) = &self.proxy {
            proxy.update(policy(&reach));
        }
        run.reach.internet = reach.internet;
        run.reach.local_network = reach.local_network;
        if run.restart {
            return restart("a previous change requires this turn to end");
        }
        if run.loopback && !(reach.internet && reach.local_network) {
            run.restart = true;
            return restart(
                "a Mac can't take this computer's own servers back from a running program",
            );
        }
        if reach.read != run.reach.read
            || reach.write != run.reach.write
            || reach.whole_read != run.reach.whole_read
            || reach.whole_write != run.reach.whole_write
        {
            run.restart = true;
            return restart("a Mac can't change which folders a running program may open");
        }
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
        let sign_in = run
            .credentials
            .as_ref()
            .map_or(Ok(()), Credentials::finish)
            .and(run.keychain.as_ref().map_or(Ok(()), Keychain::finish));
        if let Some(history) = &run.history {
            history.finished(run.session.as_deref()).map_err(|e| {
                error(format!(
                    "Øphio couldn't copy this turn into the agent's own history on this computer ({e})."
                ))
            })?;
        }
        sign_in.map_err(|e| {
            error(format!(
                "Øphio couldn't keep the agent's renewed sign-in on this computer ({e})."
            ))
        })
    }
    async fn connect(&self, addresses: &[SocketAddr]) -> Result<TcpStream, HarnessError> {
        // A Mac has one loopback, so the computer reaches the agent's server directly.
        if addresses.is_empty() || !addresses.iter().all(|a| a.ip().is_loopback()) {
            return Err(error("only the sandbox's own loopback can be reached"));
        }
        let addresses = addresses.to_vec();
        tokio::task::spawn_blocking(move || TcpStream::connect(addresses.as_slice()))
            .await
            .map_err(error)?
            .map_err(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ports::harness::RunEnv;

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

    /// What the file kept from the agent holds, which no error message says.
    const HIDDEN: &str = "hidden-bytes";

    struct Setup {
        _root: tempfile::TempDir,
        cwd: PathBuf,
        state: PathBuf,
        home: PathBuf,
        outside: PathBuf,
    }

    fn setup() -> Setup {
        let root = tempfile::Builder::new()
            .prefix("sb")
            .tempdir_in("/private/tmp")
            .unwrap();
        let base = root.path().canonicalize().unwrap();
        for name in ["cwd", "state", "home", "outside"] {
            std::fs::create_dir(base.join(name)).unwrap();
        }
        std::fs::write(base.join("outside/secret.txt"), HIDDEN).unwrap();
        std::fs::write(base.join("cwd/here.txt"), "here").unwrap();
        Setup {
            cwd: base.join("cwd"),
            state: base.join("state"),
            home: base.join("home"),
            outside: base.join("outside"),
            _root: root,
        }
    }

    fn spec(setup: &Setup, reach: Reach) -> SandboxSpec {
        SandboxSpec {
            offline: false,
            reach,
            harness: HarnessKind::Codex,
            cwd: setup.cwd.clone(),
            state: setup.state.clone(),
            always: vec![],
            programs: vec![],
            candidates: vec![],
            private: vec![],
            given: vec![],
            on_blocked: None,
            relay: None,
        }
    }

    /// Runs a shell line as an agent would be run: in the sandbox.
    fn run_in(sandbox: &MacSandbox, setup: &Setup, line: &str) -> std::process::Output {
        run_wrapped(&wrapped_shell(sandbox, setup), setup, line)
    }

    fn wrapped_shell(sandbox: &MacSandbox, setup: &Setup) -> Wrapped {
        let env = RunEnv(vec![(
            "HOME".to_string(),
            setup.home.to_string_lossy().into_owned(),
        )]);
        sandbox
            .wrap(WrapRequest {
                program: Path::new("/bin/sh"),
                visible: &[],
                env: &env,
                resume: None,
            })
            .unwrap()
    }

    fn run_wrapped(wrapped: &Wrapped, setup: &Setup, line: &str) -> std::process::Output {
        let mut command = Command::new(&wrapped.program);
        command.args(&wrapped.args);
        command.args(["/bin/sh", "-c", line]);
        command.envs(wrapped.env.iter().map(|(k, v)| (k, v)));
        command.current_dir(&setup.cwd);
        let output = command.output().unwrap();
        // A profile the system can't read refuses everything, which would pass
        // every test here that expects a refusal.
        let said = String::from_utf8_lossy(&output.stderr);
        assert!(!said.contains("sandbox-exec:"), "{said}");
        output
    }

    fn held(setup: &Setup, reach: Reach, line: &str) -> (bool, String) {
        let sandbox = MacSandbox::new(spec(setup, reach)).unwrap();
        let output = run_in(&sandbox, setup, line);
        (
            output.status.success(),
            format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ),
        )
    }

    #[test]
    fn the_sandbox_is_available_here() {
        assert!(probe().is_available(), "{:?}", probe());
    }

    #[test]
    fn it_reads_where_it_works_and_nowhere_else() {
        let s = setup();
        let (ok, said) = held(&s, reach(), "cat here.txt");
        assert!(ok, "{said}");
        assert_eq!(said, "here");
        let line = format!("cat {}/secret.txt", s.outside.display());
        let (ok, said) = held(&s, reach(), &line);
        assert!(!ok, "{said}");
        assert!(said.contains("Operation not permitted"), "{said}");
        let line = format!("ls {}", s.outside.display());
        let (ok, _) = held(&s, reach(), &line);
        assert!(!ok, "a folder's names aren't given either");
    }

    #[test]
    fn a_folder_given_to_read_is_read_and_not_changed() {
        let s = setup();
        let mut given = reach();
        given.read = vec![s.outside.clone()];
        let line = format!("cat {}/secret.txt", s.outside.display());
        let (ok, said) = held(&s, given.clone(), &line);
        assert!(ok, "{said}");
        let line = format!("echo no > {}/new.txt", s.outside.display());
        let (ok, _) = held(&s, given, &line);
        assert!(!ok);
        assert!(!s.outside.join("new.txt").exists());
    }

    #[test]
    fn a_folder_given_to_change_is_changed_and_where_it_works_is_not() {
        let s = setup();
        let mut given = reach();
        given.write = vec![s.outside.clone()];
        let line = format!("echo yes > {}/new.txt", s.outside.display());
        let (ok, said) = held(&s, given.clone(), &line);
        assert!(ok, "{said}");
        assert!(s.outside.join("new.txt").exists());
        let (ok, _) = held(&s, given, "echo no > new.txt");
        assert!(!ok, "where it works is read-only unless given");
        assert!(!s.cwd.join("new.txt").exists());
    }

    #[test]
    fn a_link_leads_nowhere_it_could_not_go_itself() {
        let s = setup();
        let mut given = reach();
        given.write = vec![s.cwd.clone()];
        let secret = s.outside.join("secret.txt");
        // A soft link made inside, and a hard link, are both refused their target.
        let line = format!("ln -s {} soft && cat soft", secret.display());
        let (ok, said) = held(&s, given.clone(), &line);
        assert!(!ok, "{said}");
        assert!(!said.contains(HIDDEN), "{said}");
        let line = format!("ln {} hard && cat hard", secret.display());
        let (ok, said) = held(&s, given, &line);
        assert!(!ok, "{said}");
        assert!(!said.contains(HIDDEN), "{said}");
    }

    #[test]
    fn private_data_stays_hidden_even_from_a_whole_computer_read() {
        let s = setup();
        let mut given = reach();
        given.whole_read = true;
        let mut with_private = spec(&s, given);
        with_private.private = vec![s.outside.clone()];
        let sandbox = MacSandbox::new(with_private).unwrap();
        let line = format!(
            "cat {}/secret.txt; echo; cat {}/here.txt; cat /etc/hosts >/dev/null && echo system",
            s.outside.display(),
            s.cwd.display()
        );
        let output = run_in(&sandbox, &s, &line);
        let said = String::from_utf8_lossy(&output.stdout);
        assert!(!said.contains(HIDDEN), "{said}");
        assert!(said.contains("here"), "{said}");
        assert!(said.contains("system"), "{said}");
    }

    #[test]
    fn the_person_s_home_and_ssh_keys_are_not_given() {
        let s = setup();
        std::fs::create_dir(s.home.join(".ssh")).unwrap();
        std::fs::write(s.home.join(".ssh/id_ed25519"), "key").unwrap();
        let line = format!("cat {}/.ssh/id_ed25519", s.home.display());
        let (ok, said) = held(&s, reach(), &line);
        assert!(!ok, "{said}");
    }

    #[test]
    fn its_own_temporary_folder_is_writable_and_tmp_is_not() {
        let s = setup();
        let (ok, said) = held(
            &s,
            reach(),
            "echo x > \"$TMPDIR/own\" && cat \"$TMPDIR/own\"",
        );
        assert!(ok, "{said}");
        let (ok, _) = held(
            &s,
            reach(),
            "echo x > /private/tmp/ophio-sandbox-test-shared",
        );
        assert!(!ok);
        assert!(!Path::new("/private/tmp/ophio-sandbox-test-shared").exists());
    }

    #[test]
    fn it_reaches_no_network_but_the_proxy() {
        let s = setup();
        // Python speaks to sockets directly, ignoring any proxy setting.
        let connect = |address: &str, port: u16| {
            format!(
                "/usr/bin/python3 -c \"import socket;s=socket.socket();s.settimeout(3);s.connect(('{address}',{port}));print('connected')\""
            )
        };
        let (ok, said) = held(&s, reach(), &connect("1.1.1.1", 443));
        assert!(!ok && !said.contains("connected"), "{said}");
        // This computer's own servers are kept from it too.
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = server.local_addr().unwrap().port();
        let (ok, said) = held(&s, reach(), &connect("127.0.0.1", port));
        assert!(!ok && !said.contains("connected"), "{said}");
        // The proxy is the one place it can go.
        let line = "/usr/bin/python3 -c \"import os,socket;p=int(os.environ['HTTPS_PROXY'].rsplit(':',1)[1]);s=socket.create_connection(('127.0.0.1',p),3);s.sendall(b'CONNECT example.org:443 HTTP/1.1\\r\\n\\r\\n');print(s.recv(32).decode())\"";
        let (ok, said) = held(&s, reach(), line);
        assert!(ok, "{said}");
        assert!(said.contains("403"), "the internet is off: {said}");
    }

    #[test]
    fn with_the_persons_own_network_it_reaches_this_computers_servers() {
        let s = setup();
        let mut given = reach();
        given.internet = true;
        given.local_network = true;
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = server.local_addr().unwrap().port();
        let line = format!(
            "/usr/bin/python3 -c \"import socket;s=socket.socket();s.settimeout(3);s.connect(('127.0.0.1',{port}));print('connected')\""
        );
        let (ok, said) = held(&s, given, &line);
        assert!(ok && said.contains("connected"), "{said}");
    }

    #[test]
    fn it_can_serve_and_the_computer_can_reach_what_it_serves() {
        let s = setup();
        let mut given = reach();
        given.write = vec![s.cwd.clone()];
        let sandbox = MacSandbox::new(spec(&s, given)).unwrap();
        let env = RunEnv(vec![(
            "HOME".to_string(),
            s.home.to_string_lossy().into_owned(),
        )]);
        let wrapped = sandbox
            .wrap(WrapRequest {
                program: Path::new("/bin/sh"),
                visible: &[],
                env: &env,
                resume: None,
            })
            .unwrap();
        let mut child = Command::new(&wrapped.program)
            .args(&wrapped.args)
            .args([
                "/usr/bin/python3",
                "-c",
                "import socket;s=socket.socket();s.bind(('127.0.0.1',0));s.listen();print(s.getsockname()[1],flush=True);c,_=s.accept();c.sendall(b'hi');c.close()",
            ])
            .envs(wrapped.env.iter().map(|(k, v)| (k, v)))
            .current_dir(&s.cwd)
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let mut line = String::new();
        std::io::BufRead::read_line(
            &mut std::io::BufReader::new(child.stdout.take().unwrap()),
            &mut line,
        )
        .unwrap();
        let port: u16 = line.trim().parse().unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let mut stream = runtime
            .block_on(sandbox.connect(&[SocketAddr::from(([127, 0, 0, 1], port))]))
            .unwrap();
        let mut said = String::new();
        std::io::Read::read_to_string(&mut stream, &mut said).unwrap();
        assert_eq!(said, "hi");
        child.wait().unwrap();
    }

    #[test]
    fn a_socket_in_its_own_folder_answers_and_one_elsewhere_does_not() {
        let s = setup();
        let own = s.outside.join("own");
        std::fs::create_dir(&own).unwrap();
        let elsewhere = s.outside.join("elsewhere");
        std::fs::create_dir(&elsewhere).unwrap();
        let _given = std::os::unix::net::UnixListener::bind(own.join("s")).unwrap();
        let _other = std::os::unix::net::UnixListener::bind(elsewhere.join("s")).unwrap();
        let mut with_own = spec(&s, reach());
        with_own.always = vec![own.clone()];
        let sandbox = MacSandbox::new(with_own).unwrap();
        let connect = |path: &Path| {
            format!(
                "/usr/bin/python3 -c \"import socket;s=socket.socket(socket.AF_UNIX);s.connect('{}');print('connected')\"",
                path.display()
            )
        };
        let output = run_in(&sandbox, &s, &connect(&own.join("s")));
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let sandbox = MacSandbox::new(spec(&s, reach())).unwrap();
        let output = run_in(&sandbox, &s, &connect(&elsewhere.join("s")));
        assert!(!output.status.success());
    }

    #[test]
    fn it_cannot_signal_or_inspect_programs_outside() {
        let s = setup();
        let mut outsider = Command::new("/bin/sleep").arg("30").spawn().unwrap();
        let line = format!("kill -9 {}", outsider.id());
        let (ok, _) = held(&s, reach(), &line);
        assert!(!ok);
        assert!(outsider.try_wait().unwrap().is_none(), "still running");
        outsider.kill().unwrap();
        outsider.wait().unwrap();
    }

    #[test]
    fn it_cannot_read_the_keychain() {
        let s = setup();
        let (ok, said) = held(
            &s,
            reach(),
            "/usr/bin/security find-generic-password -s 'Claude Code-credentials' -w",
        );
        assert!(!ok, "{said}");
        let (ok, said) = held(&s, reach(), "/usr/bin/security list-keychains");
        assert!(!ok || !said.contains("login.keychain"), "{said}");
    }

    #[test]
    fn everyday_programs_run_inside() {
        let s = setup();
        let mut given = reach();
        given.write = vec![s.cwd.clone()];
        let (ok, said) = held(&s, given.clone(), "/usr/bin/git --version");
        assert!(ok, "{said}");
        assert!(!said.contains("cache file"), "{said}");
        for line in [
            "/usr/bin/git init -q repo && /usr/bin/git -C repo status --short",
            "/usr/bin/python3 -c 'print(1)'",
            "/bin/ls /usr/bin >/dev/null",
            "/usr/bin/env >/dev/null",
            "/usr/bin/grep -q here here.txt",
            "/bin/date >/dev/null",
            "/usr/bin/script -q /dev/null /bin/echo tty",
        ] {
            let (ok, said) = held(&s, given.clone(), line);
            assert!(ok, "{line}: {said}");
        }
    }

    #[test]
    fn folders_change_only_by_restarting_and_the_internet_at_once() {
        let s = setup();
        let sandbox = MacSandbox::new(spec(&s, reach())).unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let mut internet = reach();
        internet.internet = true;
        assert_eq!(
            runtime.block_on(sandbox.update(internet.clone())),
            Applied::Applied
        );
        let mut more = internet.clone();
        more.read = vec![s.outside.clone()];
        assert!(matches!(
            runtime.block_on(sandbox.update(more)),
            Applied::NeedsRestart(_)
        ));
        // Once it must end, it says so to everything that follows.
        assert!(matches!(
            runtime.block_on(sandbox.update(internet)),
            Applied::NeedsRestart(_)
        ));
    }

    #[test]
    fn taking_this_computers_servers_back_needs_a_restart() {
        let s = setup();
        let mut given = reach();
        given.internet = true;
        given.local_network = true;
        let sandbox = MacSandbox::new(spec(&s, given)).unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let mut less = reach();
        less.internet = true;
        assert!(matches!(
            runtime.block_on(sandbox.update(less)),
            Applied::NeedsRestart(_)
        ));
    }

    #[test]
    fn a_grant_may_not_expose_private_data() {
        let s = setup();
        let mut given = reach();
        given.read = vec![s.outside.clone()];
        let mut with_private = spec(&s, given);
        with_private.private = vec![s.outside.clone()];
        let sandbox = MacSandbox::new(with_private).unwrap();
        let env = RunEnv(vec![(
            "HOME".to_string(),
            s.home.to_string_lossy().into_owned(),
        )]);
        assert!(
            sandbox
                .wrap(WrapRequest {
                    program: Path::new("/bin/sh"),
                    visible: &[],
                    env: &env,
                    resume: None,
                })
                .is_err()
        );
    }

    #[test]
    fn a_relay_run_is_refused_and_an_offline_one_has_no_proxy() {
        let s = setup();
        let mut offline = spec(&s, reach());
        offline.offline = true;
        let sandbox = MacSandbox::new(offline).unwrap();
        let env = RunEnv(vec![(
            "HOME".to_string(),
            s.home.to_string_lossy().into_owned(),
        )]);
        let wrapped = sandbox
            .wrap(WrapRequest {
                program: Path::new("/bin/sh"),
                visible: &[],
                env: &env,
                resume: None,
            })
            .unwrap();
        assert!(wrapped.clear_env);
        assert!(!wrapped.env.iter().any(|(k, _)| k == "HTTPS_PROXY"));
    }

    /// The agents themselves, where they are installed: they start inside, say
    /// who they are, and can't read what they weren't given.
    #[test]
    fn real_agents_start_inside() {
        for (name, kind) in [
            ("codex", HarnessKind::Codex),
            ("claude", HarnessKind::ClaudeCode),
        ] {
            let Some(program) = ["/opt/homebrew/bin", "/usr/local/bin"]
                .iter()
                .map(|folder| Path::new(folder).join(name))
                .find(|path| path.exists())
            else {
                eprintln!("skipped: {name} isn't installed");
                continue;
            };
            let s = setup();
            let mut with_agent = spec(&s, reach());
            with_agent.harness = kind;
            let sandbox = MacSandbox::new(with_agent).unwrap();
            let env = RunEnv(vec![(
                "HOME".to_string(),
                s.home.to_string_lossy().into_owned(),
            )]);
            let wrapped = sandbox
                .wrap(WrapRequest {
                    program: &program,
                    visible: &[],
                    env: &env,
                    resume: None,
                })
                .unwrap();
            let output = Command::new(&wrapped.program)
                .args(&wrapped.args)
                .arg(&program)
                .arg("--version")
                .envs(wrapped.env.iter().map(|(k, v)| (k, v)))
                .env("PATH", "/opt/homebrew/bin:/usr/bin:/bin")
                .current_dir(&s.cwd)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{name}: {}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }

    /// Claude Code starts a real turn inside, which writes its own temporary
    /// files: `--version` alone never reached them. With no input it ends at once,
    /// without asking anything of Anthropic.
    #[test]
    fn real_claude_starts_a_turn_inside() {
        let Some(program) = ["/opt/homebrew/bin", "/usr/local/bin"]
            .iter()
            .map(|folder| Path::new(folder).join("claude"))
            .find(|path| path.exists())
        else {
            eprintln!("skipped: claude isn't installed");
            return;
        };
        let s = setup();
        let mut with_agent = spec(&s, reach());
        with_agent.harness = HarnessKind::ClaudeCode;
        let sandbox = MacSandbox::new(with_agent).unwrap();
        let env = RunEnv(vec![(
            "HOME".to_string(),
            s.home.to_string_lossy().into_owned(),
        )]);
        let wrapped = sandbox
            .wrap(WrapRequest {
                program: &program,
                visible: &[],
                env: &env,
                resume: None,
            })
            .unwrap();
        assert!(
            wrapped.env.iter().any(|(k, v)| k == "CLAUDE_CODE_TMPDIR"
                && wrapped.env.iter().any(|(t, w)| t == "TMPDIR" && w == v)),
            "{:?}",
            wrapped.env
        );
        let output = Command::new(&wrapped.program)
            .args(&wrapped.args)
            .arg(&program)
            .args([
                "--print",
                "--output-format",
                "stream-json",
                "--input-format",
                "stream-json",
                "--verbose",
            ])
            .envs(wrapped.env.iter().map(|(k, v)| (k, v)))
            .env("PATH", "/opt/homebrew/bin:/usr/bin:/bin")
            .current_dir(&s.cwd)
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// Codex checks certificates against the system's own list, which it can
    /// read inside, rather than through the keychain's service, which is held.
    #[test]
    fn certificates_are_checked_against_a_file_inside() {
        let s = setup();
        let (ok, said) = held(
            &s,
            reach(),
            "/usr/bin/grep -c 'BEGIN CERTIFICATE' \"$SSL_CERT_FILE\"",
        );
        assert!(ok, "{said}");
        assert!(said.trim().parse::<u32>().is_ok_and(|n| n > 10), "{said}");
    }

    #[test]
    fn a_path_in_a_pattern_means_only_itself() {
        assert_eq!(
            regex_escaped("/Users/sam/a.b (c)/Library/app.ophio"),
            "/Users/sam/a\\.b \\(c\\)/Library/app\\.ophio"
        );
    }

    #[test]
    fn a_path_is_resolved_as_far_as_it_exists() {
        let s = setup();
        let real = s.home.canonicalize().unwrap();
        let through_link = PathBuf::from(real.to_str().unwrap().strip_prefix("/private").unwrap());
        assert_eq!(resolved(&through_link), real);
        assert_eq!(
            resolved(&through_link.join("not/yet")),
            real.join("not/yet")
        );
        assert_eq!(
            spellings(&through_link),
            vec![real.clone(), through_link.clone()]
        );
        assert_eq!(spellings(&real), vec![real]);
    }

    #[test]
    fn paths_are_quoted_for_the_profile() {
        assert_eq!(quoted(Path::new("/a b")), "\"/a b\"");
        assert_eq!(quoted(Path::new("/a\"b\\c")), "\"/a\\\"b\\\\c\"");
    }

    #[test]
    fn a_folder_with_odd_characters_cannot_change_the_profile() {
        let s = setup();
        let odd = s.outside.join("a\") (allow default");
        std::fs::create_dir(&odd).unwrap();
        std::fs::write(odd.join("in.txt"), "in").unwrap();
        let mut given = reach();
        given.read = vec![odd.clone()];
        let line = format!(
            "cat '{}/in.txt' && cat {}/secret.txt",
            odd.display(),
            s.outside.display()
        );
        let (ok, said) = held(&s, given, &line);
        assert!(said.starts_with("in"), "{said}");
        assert!(!said.contains(HIDDEN), "{said}");
        assert!(!ok);
    }

    #[test]
    fn claudes_keychain_entry_names_its_folder_when_there_is_one() {
        assert_eq!(Keychain::service(None), "Claude Code-credentials");
        let named = Keychain::service(Some("/Users/sam/.claude-work"));
        assert!(named.starts_with("Claude Code-credentials-"));
        assert_eq!(named.len(), "Claude Code-credentials-".len() + 8);
    }

    #[test]
    fn a_replaced_sign_in_goes_back_and_one_written_in_place_stays() {
        let s = setup();
        let real_dir = s.home.join(".codex");
        std::fs::create_dir(&real_dir).unwrap();
        let real = real_dir.join("auth.json");
        std::fs::write(&real, "old").unwrap();
        let link = s.state.join("auth.json");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let credentials = Credentials::new(link.clone(), real.clone());
        // Still a link: whatever it wrote went straight to the real file.
        credentials.finish().unwrap();
        assert_eq!(std::fs::read_to_string(&real).unwrap(), "old");
        assert!(link.is_symlink());
        // Replaced: carried back, and the copy is gone.
        std::fs::remove_file(&link).unwrap();
        std::fs::write(&link, "new").unwrap();
        credentials.finish().unwrap();
        assert_eq!(std::fs::read_to_string(&real).unwrap(), "new");
        assert!(!link.exists());
        // Signed in again while a run went: its older sign-in stays behind.
        let credentials = Credentials::new(link.clone(), real.clone());
        std::fs::write(&link, "the run's").unwrap();
        std::fs::write(&real, "signed in again").unwrap();
        credentials.finish().unwrap();
        assert_eq!(std::fs::read_to_string(&real).unwrap(), "signed in again");
        assert!(!link.exists());
    }

    /// A private file the agent links to from where a sign-in goes must not be
    /// read back out: it would land in the keychain, and be handed to the next run.
    #[test]
    fn a_planted_link_never_carries_a_private_file_out() {
        let s = setup();
        let secret = s.outside.join("secret.txt");
        let file = s.state.join(".credentials.json");
        std::os::unix::fs::symlink(&secret, &file).unwrap();
        let service = format!("ophio-sandbox-planted-{}", std::process::id());
        Keychain {
            service: service.clone(),
            account: "ophio-test".into(),
            file: file.clone(),
            given: b"given".to_vec(),
        }
        .finish()
        .unwrap();
        assert!(
            !file.is_symlink() && !file.exists(),
            "the link is taken away"
        );
        let found = Command::new("/usr/bin/security")
            .args(["find-generic-password", "-s", &service, "-w"])
            .output()
            .unwrap();
        assert!(
            !found.status.success(),
            "nothing was stored: {}",
            String::from_utf8_lossy(&found.stdout)
        );

        // The same for a sign-in file replaced by a second name for the secret.
        let real_dir = s.home.join(".codex");
        std::fs::create_dir(&real_dir).unwrap();
        let real = real_dir.join("auth.json");
        std::fs::write(&real, "kept").unwrap();
        let link = s.state.join("auth.json");
        std::fs::hard_link(&secret, &link).unwrap();
        Credentials::new(link.clone(), real.clone())
            .finish()
            .unwrap();
        assert_eq!(std::fs::read_to_string(&real).unwrap(), "kept");
        assert!(!link.exists());
        assert_eq!(std::fs::read_to_string(&secret).unwrap(), HIDDEN);
    }

    /// Asking something outside the sandbox to do the work is the way out that
    /// file and network rules don't see. Each of these would leave a file behind.
    #[test]
    fn nothing_outside_the_sandbox_can_be_asked_to_run_anything() {
        let s = setup();
        let left = s.outside.join("escaped");
        let label = format!("app.ophio.sandbox-test-{}", std::process::id());
        let write = format!("echo x > {}", left.display());
        let lines = [
            format!("/bin/launchctl submit -l {label} -- /bin/sh -c '{write}'"),
            format!("/bin/launchctl asuser $(/usr/bin/id -u) /bin/sh -c '{write}'"),
            format!("/usr/bin/osascript -e 'do shell script \"{write}\"'"),
            format!("/usr/bin/sudo -n /bin/sh -c '{write}'"),
            format!(
                "echo '#!/bin/sh\n{write}' > \"$TMPDIR/run.command\"; /bin/chmod +x \"$TMPDIR/run.command\"; /usr/bin/open \"$TMPDIR/run.command\""
            ),
            format!("/usr/bin/ssh -o BatchMode=yes -o ConnectTimeout=2 127.0.0.1 '{write}'"),
        ];
        for line in &lines {
            let sandbox = MacSandbox::new(spec(&s, reach())).unwrap();
            let output = run_in(&sandbox, &s, line);
            std::thread::sleep(std::time::Duration::from_secs(2));
            let _ = Command::new("/bin/launchctl")
                .args(["remove", &label])
                .output();
            assert!(
                !left.exists(),
                "{line}: {}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }

    /// Rules are by path, so moving the folder above private data, which a write
    /// grant over it would otherwise allow, would take the data out from under them.
    #[test]
    fn moving_the_folder_above_private_data_does_not_reveal_it() {
        let s = setup();
        let above = s.home.join("above");
        let private = above.join("private");
        std::fs::create_dir_all(&private).unwrap();
        std::fs::write(private.join("kept.txt"), HIDDEN).unwrap();
        let mut given = reach();
        given.write = vec![s.home.clone()];
        let mut with_private = spec(&s, given);
        with_private.private = vec![private.clone()];
        let sandbox = MacSandbox::new(with_private).unwrap();
        let line = format!(
            "cd {home}; mv above moved; cat moved/private/kept.txt; mv above/private out; cat out/kept.txt; ln above/private/kept.txt linked.txt; cat linked.txt; echo fine > above/new.txt; cat above/new.txt",
            home = s.home.display()
        );
        let output = run_in(&sandbox, &s, &line);
        let said = String::from_utf8_lossy(&output.stdout);
        assert!(!said.contains(HIDDEN), "{said}");
        assert_eq!(
            std::fs::read_to_string(private.join("kept.txt")).unwrap(),
            HIDDEN
        );
        assert!(
            said.contains("fine"),
            "what else is in that folder can still change: {said}"
        );
    }

    /// Rules match real paths, so a private folder named through a link, as
    /// everything under `/tmp` on a Mac is, must be protected where it really is.
    #[test]
    fn a_private_folder_named_through_a_link_is_protected_where_it_is() {
        let s = setup();
        let above = s.home.join("above");
        let private = above.join("private");
        std::fs::create_dir_all(&private).unwrap();
        std::fs::write(private.join("kept.txt"), HIDDEN).unwrap();
        let through_link = PathBuf::from(
            private
                .to_str()
                .unwrap()
                .strip_prefix("/private")
                .expect("the test's folders are under /private/tmp"),
        );
        assert!(through_link.exists());
        let mut given = reach();
        given.write = vec![s.home.clone()];
        let mut with_private = spec(&s, given);
        with_private.private = vec![through_link];
        let sandbox = MacSandbox::new(with_private).unwrap();
        let line = format!(
            "cd {home}; cat above/private/kept.txt; mv above moved; cat moved/private/kept.txt",
            home = s.home.display()
        );
        let output = run_in(&sandbox, &s, &line);
        assert!(
            !String::from_utf8_lossy(&output.stdout).contains(HIDDEN),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert!(private.join("kept.txt").exists(), "it was not moved");
    }

    /// A private folder that appears only after the run has started has no other
    /// rule to hold it, and nothing may be done beforehand to lead it astray.
    #[test]
    fn a_private_folder_made_after_the_run_starts_stays_private() {
        let s = setup();
        let above = s.home.join("above");
        let private = above.join("private");
        let mut given = reach();
        given.write = vec![s.home.clone()];
        let mut with_private = spec(&s, given);
        with_private.private = vec![private.clone()];
        let sandbox = MacSandbox::new(with_private).unwrap();
        let wrapped = wrapped_shell(&sandbox, &s);

        // Before it exists, a link can't be put where it will go.
        let home = s.home.display();
        std::fs::create_dir(s.home.join("elsewhere")).unwrap();
        let output = run_wrapped(
            &wrapped,
            &s,
            &format!("ln -s {home}/elsewhere {home}/above"),
        );
        assert!(
            !output.status.success(),
            "a link at a folder that will hold it"
        );
        assert!(std::fs::symlink_metadata(&above).is_err());

        std::fs::create_dir_all(&private).unwrap();
        std::fs::write(private.join("kept.txt"), HIDDEN).unwrap();
        let line = format!(
            "cd {home}; cat above/private/kept.txt; mv above moved; cat moved/private/kept.txt"
        );
        let output = run_wrapped(&wrapped, &s, &line);
        assert!(
            !String::from_utf8_lossy(&output.stdout).contains(HIDDEN),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
    }

    /// Øphio's own data under a name not seen yet, as a later version might use.
    #[test]
    fn ophios_data_under_a_new_name_stays_private() {
        let s = setup();
        let support = s.home.join("Library/Application Support");
        std::fs::create_dir_all(&support).unwrap();
        let mut given = reach();
        given.whole_read = true;
        given.write = vec![s.home.clone()];
        let sandbox = MacSandbox::new(spec(&s, given)).unwrap();
        let wrapped = wrapped_shell(&sandbox, &s);
        for name in ["ophio", "ophio-next", "app.ophio.helper"] {
            std::fs::create_dir_all(support.join(name)).unwrap();
            std::fs::write(support.join(name).join("kept.txt"), HIDDEN).unwrap();
        }
        let home = s.home.display();
        let line = format!(
            "cd '{home}/Library'; for n in ophio ophio-next app.ophio.helper; do cat \"Application Support/$n/kept.txt\"; done; mv 'Application Support' moved; cat moved/ophio/kept.txt; cd ..; mv Library L; cat L/'Application Support'/ophio/kept.txt; echo ok > '{home}/Library/Application Support/other.txt'; cat '{home}/Library/Application Support/other.txt'"
        );
        let output = run_wrapped(&wrapped, &s, &line);
        let said = String::from_utf8_lossy(&output.stdout);
        assert!(!said.contains(HIDDEN), "{said}");
        assert!(
            said.contains("ok"),
            "other apps' folders still work: {said}"
        );
    }

    /// Another terminal of the person's: nothing can be shown in it or changed
    /// about it, and a terminal the run opens for itself still works.
    #[test]
    #[allow(unsafe_code)]
    fn another_terminal_cannot_be_reached() {
        let s = setup();
        let (mut master, mut slave) = (0, 0);
        // SAFETY: both out-parameters outlive the call, and the rest may be null.
        let opened = unsafe {
            libc::openpty(
                &raw mut master,
                &raw mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(opened, 0);
        // SAFETY: a live descriptor; the name is copied before anything else runs.
        let name = unsafe { std::ffi::CStr::from_ptr(libc::ttyname(slave)) }
            .to_string_lossy()
            .into_owned();
        let (ok, said) = held(&s, reach(), &format!("printf intruder > {name}"));
        assert!(!ok, "{name}: {said}");
        let (ok, said) = held(&s, reach(), &format!("/bin/stty -f {name} rows 13 cols 37"));
        assert!(!ok, "{name}: {said}");
        let (ok, said) = held(&s, reach(), "/usr/bin/script -q /dev/null /bin/echo tty");
        assert!(ok && said.contains("tty"), "{said}");
        // SAFETY: both are ours and closed once.
        unsafe {
            libc::close(slave);
            libc::close(master);
        }
    }

    /// Named shared memory and semaphores aren't files or addresses, so no other
    /// rule covers them: another program's can't be opened, and none can be left.
    #[test]
    #[allow(unsafe_code)]
    fn named_memory_and_semaphores_outside_the_run_cannot_be_reached() {
        let s = setup();
        let name = format!("/ophio-proof-{}", std::process::id());
        let c_name = std::ffi::CString::new(name.clone()).unwrap();
        // SAFETY: a nul-terminated name; the mode goes through as C's variadic int.
        let fd = unsafe {
            libc::shm_open(
                c_name.as_ptr(),
                libc::O_CREAT | libc::O_RDWR,
                0o600 as libc::c_uint,
            )
        };
        assert!(fd >= 0);
        let call = |function: &str, flags: i32| {
            format!(
                "/usr/bin/python3 -c \"import ctypes;l=ctypes.CDLL(None);l.{function}.restype=ctypes.c_long;print('got',l.{function}(b'{name}',{flags},0o600,0))\""
            )
        };
        let (_, said) = held(&s, reach(), &call("shm_open", libc::O_RDWR));
        assert!(said.contains("got -1"), "opened another program's: {said}");
        let new_name = format!("{name}-left");
        let leave = call("shm_open", libc::O_CREAT | libc::O_RDWR).replace(&name, &new_name);
        let (_, said) = held(&s, reach(), &leave);
        assert!(said.contains("got -1"), "left one behind: {said}");
        let semaphore = call("sem_open", libc::O_CREAT).replace(&name, &new_name);
        let (_, said) = held(&s, reach(), &semaphore);
        // A failed sem_open returns SEM_FAILED, which is -1.
        assert!(said.contains("got -1"), "left a semaphore behind: {said}");
        // SAFETY: as above.
        unsafe {
            libc::close(fd);
            libc::shm_unlink(c_name.as_ptr());
        }
        // Python's own locks, which tools rely on, still work.
        let (ok, said) = held(
            &s,
            reach(),
            "/usr/bin/python3 -c \"import multiprocessing;multiprocessing.Lock();print('locked')\"",
        );
        assert!(ok && said.contains("locked"), "{said}");
    }

    /// The opening for the proxy is for the proxy: its port on loopback, by TCP,
    /// held on every address that port can be reached at.
    #[test]
    fn the_proxy_port_reaches_nothing_but_the_proxy() {
        let s = setup();
        let sandbox = MacSandbox::new(spec(&s, reach())).unwrap();
        let port = sandbox.proxy.as_ref().unwrap().port();
        let listener = std::net::UdpSocket::bind(("127.0.0.1", port)).unwrap();
        listener
            .set_read_timeout(Some(std::time::Duration::from_millis(500)))
            .unwrap();
        let line = format!(
            "/usr/bin/python3 -c \"import socket;socket.socket(socket.AF_INET,socket.SOCK_DGRAM).sendto(b'x',('127.0.0.1',{port}))\""
        );
        let output = run_in(&sandbox, &s, &line);
        let mut buffer = [0u8; 8];
        assert!(
            listener.recv(&mut buffer).is_err(),
            "a packet got out: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            std::net::TcpListener::bind(("::1", port)).is_err(),
            "another program could take the port on IPv6"
        );
    }

    /// What Homebrew's services keep isn't what a program needs to run, and
    /// neither is what's beside it in the same prefix.
    #[test]
    fn homebrews_data_is_not_given_with_its_programs() {
        let s = setup();
        if !Path::new("/opt/homebrew/var").exists() {
            eprintln!("skipped: no Homebrew here");
            return;
        }
        let (ok, said) = held(&s, reach(), "/bin/ls /opt/homebrew/var");
        assert!(!ok, "{said}");
        let (ok, said) = held(&s, reach(), "/bin/ls /opt/homebrew/bin >/dev/null");
        assert!(ok, "{said}");
        if Path::new("/opt/homebrew/etc/ca-certificates").exists() {
            let (ok, said) = held(&s, reach(), "/bin/ls /opt/homebrew/etc/ca-certificates");
            assert!(ok, "certificates are needed to make connections: {said}");
        }
    }

    /// Root through `sudo` is still inside: the rules are by path, whoever asks.
    #[test]
    fn administrator_rights_do_not_lift_the_sandbox() {
        let s = setup();
        let secret = s.outside.join("secret.txt");
        let (_, said) = held(
            &s,
            reach(),
            &format!("/usr/bin/sudo -n /bin/cat {}", secret.display()),
        );
        assert!(!said.contains(HIDDEN), "{said}");
    }

    /// A login item put in a granted folder would run outside the sandbox at the
    /// next login, but only the person's own folders can hold one.
    #[test]
    fn it_cannot_add_a_login_item_or_change_a_shell_startup_file() {
        let s = setup();
        let home = std::env::var("HOME").unwrap();
        for target in [
            format!("{home}/Library/LaunchAgents/app.ophio.sandbox-test.plist"),
            format!("{home}/.zshenv.ophio-sandbox-test"),
        ] {
            let (ok, said) = held(&s, reach(), &format!("echo x > '{target}'"));
            assert!(!ok, "{said}");
            assert!(!Path::new(&target).exists(), "{target}");
        }
    }

    #[test]
    fn a_keychain_sign_in_is_copied_for_the_run_and_a_renewal_goes_back() {
        let s = setup();
        let service = format!("ophio-sandbox-test-{}", std::process::id());
        let account = "ophio-test";
        let added = Command::new("/usr/bin/security")
            .args([
                "add-generic-password",
                "-U",
                "-a",
                account,
                "-s",
                &service,
                "-w",
                "given",
            ])
            .output()
            .unwrap();
        if !added.status.success() {
            // No login keychain, as for a session with none unlocked.
            eprintln!("skipped: {}", String::from_utf8_lossy(&added.stderr));
            return;
        }
        let file = s.state.join(".credentials.json");
        std::fs::write(&file, "renewed").unwrap();
        let keychain = Keychain {
            service: service.clone(),
            account: account.into(),
            file: file.clone(),
            given: b"given".to_vec(),
        };
        keychain.finish().unwrap();
        assert!(!file.exists());
        let found = Command::new("/usr/bin/security")
            .args(["find-generic-password", "-s", &service, "-w"])
            .output()
            .unwrap();
        let _ = Command::new("/usr/bin/security")
            .args(["delete-generic-password", "-s", &service])
            .output();
        assert_eq!(String::from_utf8_lossy(&found.stdout).trim(), "renewed");
    }

    /// Signed in again while a run went, from the phone say: the run's older
    /// sign-in, renewed or not, doesn't go back over the new one.
    #[test]
    fn a_sign_in_made_during_a_run_stays_in_the_keychain() {
        let s = setup();
        let service = format!("ophio-sandbox-newer-{}", std::process::id());
        let account = "ophio-test";
        let set = |value: &str| {
            Command::new("/usr/bin/security")
                .args([
                    "add-generic-password",
                    "-U",
                    "-a",
                    account,
                    "-s",
                    &service,
                    "-w",
                    value,
                ])
                .output()
                .unwrap()
        };
        let added = set("given");
        if !added.status.success() {
            eprintln!("skipped: {}", String::from_utf8_lossy(&added.stderr));
            return;
        }
        let file = s.state.join(".credentials.json");
        std::fs::write(&file, "the run's renewal").unwrap();
        let keychain = Keychain {
            service: service.clone(),
            account: account.into(),
            file: file.clone(),
            given: b"given".to_vec(),
        };
        assert!(set("signed in again").status.success());
        keychain.finish().unwrap();
        assert!(!file.exists());
        let found = Command::new("/usr/bin/security")
            .args(["find-generic-password", "-s", &service, "-w"])
            .output()
            .unwrap();
        let _ = Command::new("/usr/bin/security")
            .args(["delete-generic-password", "-s", &service])
            .output();
        assert_eq!(
            String::from_utf8_lossy(&found.stdout).trim(),
            "signed in again"
        );
    }

    /// Signed out while a run went: its sign-in doesn't come back.
    #[test]
    fn a_sign_out_during_a_run_is_not_undone() {
        let s = setup();
        let service = format!("ophio-sandbox-gone-{}", std::process::id());
        let added = Command::new("/usr/bin/security")
            .args([
                "add-generic-password",
                "-U",
                "-a",
                "ophio-test",
                "-s",
                &service,
                "-w",
                "given",
            ])
            .output()
            .unwrap();
        if !added.status.success() {
            eprintln!("skipped: {}", String::from_utf8_lossy(&added.stderr));
            return;
        }
        let file = s.state.join(".credentials.json");
        std::fs::write(&file, "the run's renewal").unwrap();
        let keychain = Keychain {
            service: service.clone(),
            account: "ophio-test".into(),
            file: file.clone(),
            given: b"given".to_vec(),
        };
        assert!(
            Command::new("/usr/bin/security")
                .args(["delete-generic-password", "-s", &service])
                .output()
                .unwrap()
                .status
                .success()
        );
        keychain.finish().unwrap();
        assert!(!file.exists());
        let found = Command::new("/usr/bin/security")
            .args(["find-generic-password", "-s", &service, "-w"])
            .output()
            .unwrap();
        let _ = Command::new("/usr/bin/security")
            .args(["delete-generic-password", "-s", &service])
            .output();
        assert!(!found.status.success(), "the sign-out stays");
    }

    #[test]
    fn a_program_installed_as_a_package_brings_its_folder() {
        assert_eq!(
            package_of(Path::new(
                "/opt/homebrew/lib/node_modules/@openai/codex/bin/codex.js"
            ))
            .as_deref(),
            Some(Path::new("/opt/homebrew/lib/node_modules/@openai/codex"))
        );
        assert_eq!(
            package_of(Path::new("/opt/homebrew/Cellar/node/24.1/bin/node")).as_deref(),
            Some(Path::new("/opt/homebrew/Cellar/node/24.1"))
        );
        assert_eq!(
            package_of(Path::new("/Users/sam/.local/share/claude/versions/2.1.0")).as_deref(),
            Some(Path::new("/Users/sam/.local/share/claude/versions"))
        );
        assert_eq!(package_of(Path::new("/Users/sam/bin/tool")), None);
    }
}
