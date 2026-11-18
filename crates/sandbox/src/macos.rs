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
    relay::Relay,
    sign_in::{Kind, Recovered, read_own},
};
use containment::inherit;
use ports::{
    HarnessError,
    sandbox::{
        Applied, Reach, RelayEndpoint, RelayProvider, RelayTotals, Sandbox, SandboxSpec,
        WrapRequest, Wrapped,
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
pub(crate) const RUNTIME_DATA: &[&str] = &["/usr/local/var", "/usr/local/etc"];

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
///
/// Asking the system about a program (its file, folder, open files and use) is
/// allowed by the system unless a profile denies it, even under `deny default`,
/// so it is denied outright and given back for this run's own programs. Listing
/// the numbers of running programs stays, which `lsof` needs even for its own;
/// a number alone says nothing. Another program's command line can't be kept
/// from a run of the same user this way: the system answers `kern.procargs2`,
/// `kern.procargs` and `kern.proc.pid` without asking the profile at all.
pub(crate) const BASE: &str = r#"(version 1)
(deny default)
(import "system.sb")
(allow process-fork process-exec)
(allow signal (target same-sandbox))
(deny process-info*)
(allow process-info* (target same-sandbox))
(allow process-info-listpids)
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
    match inherit::output(Command::new(SANDBOX_EXEC).args(["-p", BASE, "/usr/bin/true"])) {
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
pub(crate) fn quoted(path: &Path) -> String {
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
    /// Model-only runs may neither reach Unix sockets nor serve host connections.
    model_only: bool,
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
        if !self.model_only {
            rule(&mut text, "allow network-outbound", &self.own, "subpath");
        }
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
        if self.model_only {
            // Override any network exception imported by system.sb too. Restore
            // only this run's pinned TCP broker below, never Unix IPC or servers.
            text.push_str("(deny network*)\n");
        }
        if let Some(port) = self.proxy {
            text.push_str(&format!(
                "(allow network-outbound (remote tcp \"localhost:{port}\"))\n"
            ));
        }
        if self.loopback && !self.model_only {
            text.push_str(
                "(allow network-outbound (remote ip \"localhost:*\"))\n(allow network-inbound (local ip \"localhost:*\"))\n(allow network-bind (local ip \"localhost:*\"))\n",
            );
        } else if !self.model_only {
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

/// Build the real agent policy for check/agent network comparisons, without
/// creating an agent session, reading a sign-in or touching the keychain.
#[cfg(test)]
pub(crate) fn check_network_comparison_profile(
    project: &Path,
    scratch: &Path,
    loopback: bool,
    proxy: Option<u16>,
) -> String {
    Profile {
        read: vec![project, scratch],
        write: vec![project, scratch],
        own: vec![scratch],
        loopback,
        proxy,
        ..Profile::default()
    }
    .text()
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
    /// Where the agent's own settings are, once its command is built.
    own: Option<crate::own::OwnSettings>,
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
    /// The sign-in the run was given, by its digest.
    given: crate::sign_in::Digest,
}

#[derive(Debug)]
pub(crate) struct MacSandbox {
    spec: SandboxSpec,
    proxy: Option<Proxy>,
    relay: Option<Relay>,
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
            // Seatbelt protects paths, not arbitrary hard-link aliases. Refuse
            // configured host secrets with other names rather than expose one.
            for path in [&relay.key_file, &relay.log] {
                use std::os::unix::fs::MetadataExt;
                match std::fs::symlink_metadata(path) {
                    Ok(metadata) if metadata.is_file() && metadata.nlink() == 1 => {}
                    Err(e) if e.kind() == io::ErrorKind::NotFound && path == &relay.log => {}
                    _ => {
                        return Err(error(
                            "relay credential and log must be single-link regular files",
                        ));
                    }
                }
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
        let native = (spec.offline || relay.is_some())
            .then(|| {
                tempfile::Builder::new()
                    .prefix("relay-native-")
                    .tempdir_in(&spec.state)
                    .map_err(error)
            })
            .transpose()?;
        let proxy = (!spec.offline && relay.is_none())
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
        let loopback = relay.is_none() && spec.reach.internet && spec.reach.local_network;
        let run = Run {
            reach: spec.reach.clone(),
            loopback,
            history: None,
            session: None,
            keychain: None,
            credentials: None,
            own: None,
            wrapped: false,
            restart: false,
        };
        Ok(Self {
            spec,
            proxy,
            relay,
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
        if self.native.is_none() && !host_config.exists() && host_config.is_absolute() {
            use std::os::unix::fs::DirBuilderExt;
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&host_config)
                .map_err(|e| error(format!("{}: {e}", host_config.display())))?;
        }
        if !host_config.is_absolute() {
            return Err(error("provider configuration must be absolute"));
        }
        let host_config = if self.native.is_some() {
            resolved(&host_config)
        } else {
            canonical(&host_config)?
        };

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
        let mut relay_private = Vec::new();
        if let Some(relay) = &self.spec.relay {
            relay_private.extend([
                relay.key_file.clone(),
                relay.log.clone(),
                host_config.clone(),
                home.join(".claude"),
                home.join(".codex"),
                home.join(".claude.json"),
            ]);
            relay_private.extend(self.spec.given.iter().cloned());
            for variable in ["CLAUDE_CONFIG_DIR", "CODEX_HOME"] {
                if let Some(path) = env(variable) {
                    if !Path::new(&path).is_absolute() {
                        return Err(error("provider configuration must be absolute"));
                    }
                    relay_private.push(PathBuf::from(path));
                }
            }
            relay_private = relay_private.iter().map(|p| resolved(p)).collect();
            required.extend(relay_private.iter().cloned());
        }
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
        let mut shared_sources: Vec<PathBuf> = Vec::new();
        let config = if let Some(native) = &self.native {
            if request.resume.is_some() {
                return Err(error(
                    "model-only and offline runs start with fresh provider state",
                ));
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
            // Opened once, through no link; every link below is made through
            // this handle, so nothing the agent left in it is followed.
            let root = crate::sys::folder(&local).map_err(error)?;
            // What this run is given, noted by digest in case Øphio stops
            // before it ends: the next run then carries a renewed sign-in back.
            let mut noted = Noted::Nothing;
            for (source, destination) in &history.binds {
                if destination == &host_config {
                    continue;
                }
                let name = destination
                    .strip_prefix(&host_config)
                    .map_err(|_| error("native history outside the agent's folder"))?;
                let real = canonical(source)?;
                let sign_in = real.is_file() && real.starts_with(&host_config);
                let link = local.join(name);
                if sign_in
                    && let Recovered::Kept { given, why } =
                        crate::sign_in::recover_file(&self.spec.state, &link, &real)
                {
                    // A sign-in renewed in a run Øphio didn't get to finish
                    // that can't go back yet stays as this run's own, in place
                    // of the link, and this run's end tries again.
                    tracing::warn!(error = %why, "{KEPT}");
                    run.credentials = Some(Credentials {
                        link,
                        real,
                        began: Some(given),
                    });
                    noted = Noted::Unchanged;
                    continue;
                }
                crate::setup::link_within(&root, name, source, sign_in).map_err(error)?;
                if sign_in {
                    let credentials = Credentials::new(link, real.clone());
                    noted = Noted::Given(Kind::File, credentials.began);
                    run.credentials = Some(credentials);
                }
                own.push(real);
            }
            // The sign-in file removed since, as by `codex logout`: what an
            // unfinished run left in its place doesn't keep the agent signed in.
            if run.credentials.is_none()
                && crate::sign_in::recorded_kind(&self.spec.state) == Some(Kind::File)
            {
                let name = crate::history::credential(self.spec.harness);
                match std::fs::symlink_metadata(host_config.join(name)) {
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {
                        match crate::sys::remove_at(&root, std::ffi::OsStr::new(name), false) {
                            Err(e) if e.kind() != io::ErrorKind::NotFound => {
                                tracing::warn!(error = %e, "couldn't remove a limited chat's sign-in after a sign-out");
                            }
                            _ => {}
                        }
                    }
                    // Still there, or it can't be told: left as it is.
                    _ => noted = Noted::Unchanged,
                }
            }
            // The person's own instructions and skills, as links the sandbox
            // lets the agent read through and not write through.
            let mut history = history;
            let forbidden = crate::setup::private(
                &home,
                private
                    .iter()
                    .cloned()
                    .chain([self.spec.state.clone()])
                    .chain(self.spec.given.iter().cloned()),
            );
            let cwd = canonical(&self.spec.cwd).unwrap_or_else(|_| self.spec.cwd.clone());
            let withheld = crate::setup::withheld(request.disabled);
            let bounds = crate::setup::Bounds {
                home: &home,
                cwd: &cwd,
                private: &forbidden,
                writes: reach.writes(),
                withheld: &withheld,
            };
            let shared = crate::setup::select(self.spec.harness, &host_config, &bounds);
            history.shared = crate::setup::place(&local, &self.spec.state, shared, true);
            shared_sources.extend(history.shared.iter().map(|s| s.source.clone()));
            run.history = Some(history);
            if self.spec.harness == HarnessKind::ClaudeCode && run.credentials.is_none() {
                let (keychain, kept) =
                    Keychain::copy(named_config.as_deref(), &home, &local, &self.spec.state)?;
                run.keychain = keychain;
                noted = kept;
            }
            let written = match noted {
                Noted::Given(kind, digest) => {
                    crate::sign_in::remember(&self.spec.state, kind, digest)
                }
                Noted::Nothing => crate::sign_in::forget(&self.spec.state),
                Noted::Unchanged => Ok(()),
            };
            if let Err(e) = written {
                tracing::warn!(error = %e, "couldn't note which sign-in a limited chat was given");
                let _ = crate::sign_in::forget(&self.spec.state);
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
        visible.extend(shared_sources);
        // An account signed out since the run was given it leaves nothing.
        for path in self
            .spec
            .given
            .iter()
            .filter(|p| self.relay.is_none() && p.exists())
        {
            visible.push(canonical(path)?);
        }

        if self.relay.is_some() {
            // Check every restored path, including runtime/package folders: an
            // allow following the private deny must never restore host secrets.
            for source in self
                .spec
                .candidates
                .iter()
                .chain(&own)
                .chain(&visible)
                .cloned()
                .chain(RUNTIME.iter().map(PathBuf::from))
            {
                let source = resolved(&source);
                if relay_private
                    .iter()
                    .any(|secret| source.starts_with(secret) || secret.starts_with(&source))
                {
                    return Err(error(
                        "relay grants must not overlap credentials, audit logs or provider login state",
                    ));
                }
            }
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
            proxy: self
                .proxy
                .as_ref()
                .map(Proxy::port)
                .or_else(|| self.relay.as_ref().map(Relay::port)),
            loopback: run.loopback,
            model_only: self.relay.is_some(),
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
        if self.spec.offline || self.relay.is_some() {
            environment.push((
                "PATH".into(),
                std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".into()),
            ));
            environment.push(("LANG".into(), "en_US.UTF-8".into()));
        }
        if let Some(relay) = &self.relay {
            let key = match self.spec.harness {
                HarnessKind::ClaudeCode => "ANTHROPIC_API_KEY",
                HarnessKind::Codex => "OPHIO_RELAY_DUMMY_KEY",
                _ => unreachable!("provider checked at construction"),
            };
            environment.push((
                key.into(),
                relay
                    .local_key()
                    .expect("Mac relay requires local authentication")
                    .into(),
            ));
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
        // The agent's folder is this conversation's own, at its own place: its
        // links lead to its history, also its own, or out to the person's
        // sign-in and shared instructions, which are not.
        let mut own_places = vec![config.clone()];
        if self.native.is_none() {
            own_places.push(self.spec.state.join("claude-history"));
        }
        run.own = Some(crate::own::OwnSettings {
            harness: self.spec.harness,
            folder: config.clone(),
            files: Vec::new(),
            places: vec![(config.clone(), config)],
            own: crate::own::real(own_places),
            cwd: self.spec.cwd.clone(),
        });
        Ok(Wrapped {
            program: SANDBOX_EXEC.into(),
            args: vec!["-p".into(), profile.into()],
            env: environment,
            clear_env: self.spec.offline || self.relay.is_some(),
            shared_skills: run
                .history
                .as_ref()
                .is_some_and(|h| h.shared.iter().any(|s| s.relative.starts_with("skills"))),
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
    /// Claude Code looks when the keychain doesn't answer, and says what to
    /// note of it. Nothing to copy, as with no sign-in, is for the agent to say.
    fn copy(
        named_config: Option<&str>,
        home: &Path,
        local: &Path,
        state: &Path,
    ) -> Result<(Option<Self>, Noted), HarnessError> {
        let account = std::env::var("USER")
            .ok()
            .or_else(|| home.file_name().map(|n| n.to_string_lossy().into_owned()))
            .unwrap_or_default();
        let service = Self::service(named_config);
        let file = local.join(".credentials.json");
        let kept = |given| Self {
            service: service.clone(),
            account: account.clone(),
            file: file.clone(),
            given,
        };
        let held = match Self::read(&service) {
            Entry::Holds(held) => held,
            Entry::Missing => {
                // Signed out: a sign-in an unfinished run renewed doesn't outlive it.
                if crate::sign_in::recorded_kind(state) == Some(Kind::Keychain) {
                    drop_file(&file).map_err(error)?;
                }
                return Ok((None, Noted::Nothing));
            }
            Entry::Unreadable(why) => {
                // Whether the person signed out isn't known, so what an
                // unfinished run left stays, as this run's sign-in, and this
                // run's end tries again. With nothing left, there's nothing to give.
                let Some(given) = crate::sign_in::recorded_given(state, Kind::Keychain) else {
                    return Ok((None, Noted::Unchanged));
                };
                return match crate::sign_in::read_at_most(&file, KEYCHAIN_MOST) {
                    Ok(left) if !left.is_empty() => {
                        tracing::warn!(error = %why, "{KEPT}");
                        Ok((Some(kept(given)), Noted::Unchanged))
                    }
                    Err(e) if e.kind() == io::ErrorKind::NotFound => Ok((None, Noted::Unchanged)),
                    // Empty, oversized, a link or a second name: not a sign-in.
                    _ => {
                        drop_file(&file).map_err(error)?;
                        Ok((None, Noted::Unchanged))
                    }
                };
            }
        };
        // A sign-in renewed in a run Øphio didn't get to finish goes back
        // before the copy is replaced, and this run is given it. One that
        // can't go back yet stays as this run's own, and its end tries again.
        let given = match crate::sign_in::recover(
            state,
            &file,
            Kind::Keychain,
            KEYCHAIN_MOST,
            || crate::sign_in::Held::Is(crate::sign_in::digest(&held)),
            |renewed| Self::put(&account, &service, renewed),
        ) {
            Recovered::Nothing => held,
            Recovered::Back(renewed) => renewed,
            Recovered::Kept { given, why } => {
                tracing::warn!(error = %why, "{KEPT}");
                return Ok((Some(kept(given)), Noted::Unchanged));
            }
        };
        // A file an earlier run left, or a link planted there, is not written through.
        drop_file(&file).map_err(error)?;
        let mut out = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&file)
            .map_err(error)?;
        out.write_all(&given).map_err(error)?;
        out.sync_all().map_err(error)?;
        let given = crate::sign_in::digest(&given);
        Ok((Some(kept(given)), Noted::Given(Kind::Keychain, Some(given))))
    }

    /// The keychain's entry for `service`. Only an entry that isn't there is
    /// a sign-out; a keychain that is locked, or that `security` couldn't
    /// otherwise read, says nothing either way.
    fn read(service: &str) -> Entry {
        let mut output = Command::new("/usr/bin/security");
        output
            .args(["find-generic-password", "-s", service, "-w"])
            .args(test_keychain())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null());
        let output = inherit::spawn(&mut output).and_then(|child| child.wait_with_output());
        let output = match output {
            Ok(output) => output,
            Err(e) => return Entry::Unreadable(e),
        };
        if !output.status.success() {
            return match output.status.code() {
                Some(NOT_FOUND) => Entry::Missing,
                code => Entry::Unreadable(io::Error::other(format!(
                    "the keychain couldn't be read (security exited {})",
                    code.map_or_else(|| "on a signal".into(), |c| c.to_string())
                ))),
            };
        }
        let mut held = output.stdout;
        while held.last().is_some_and(u8::is_ascii_whitespace) {
            held.pop();
        }
        if held.is_empty() {
            Entry::Missing
        } else {
            Entry::Holds(held)
        }
    }

    /// Takes the copy away, and carries a renewed sign-in back to the keychain.
    /// `security`'s own prompt takes the value, so it never shows in a process
    /// list. A sign-in made since the run began, from the phone say, stays:
    /// the run's older one doesn't go back over it. One that can't go back
    /// yet, or a keychain that can't be read to tell, keeps the copy, and the
    /// next run in the conversation tries again.
    fn finish(&self) -> io::Result<()> {
        // The agent could write here, so nothing it left is followed: a link or
        // a second name for a private file would carry that file to the
        // keychain, and from there into the next run. Such a file is dropped.
        let now = match crate::sign_in::read_at_most(&self.file, KEYCHAIN_MOST) {
            Ok(now) => now,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(_) => return drop_file(&self.file),
        };
        if now.is_empty() || crate::sign_in::digest(&now) == self.given {
            return drop_file(&self.file);
        }
        // Only over the sign-in it was given: one replaced or signed out (the
        // entry gone) stays as it is.
        match Self::read(&self.service) {
            Entry::Holds(held) if crate::sign_in::digest(&held) == self.given => {
                Self::put(&self.account, &self.service, &now)?;
                drop_file(&self.file)
            }
            Entry::Holds(_) | Entry::Missing => drop_file(&self.file),
            Entry::Unreadable(why) => Err(why),
        }
    }

    /// Stores `renewed` as the keychain's sign-in for `service`, through
    /// `security`'s own prompt. That prompt cuts a line at 4,096 bytes and
    /// runs the rest as a command of its own, which would store part of the
    /// sign-in, so a longer line isn't sent: the sign-in is too long for it
    /// rather than ever stored cut.
    fn put(account: &str, service: &str, renewed: &[u8]) -> io::Result<()> {
        let hex: String = renewed.iter().map(|b| format!("{b:02x}")).collect();
        let mut line = format!(
            "add-generic-password -U -a {} -s {} -X {hex}",
            sh_word(account),
            sh_word(service)
        );
        if let Some(keychain) = test_keychain() {
            line.push(' ');
            line.push_str(&sh_word(&keychain.to_string_lossy()));
        }
        line.push('\n');
        if line.len() > LINE_MOST {
            return Err(io::Error::other(
                "the renewed sign-in is too long to hand to the keychain without showing it in a process list",
            ));
        }
        let mut child = inherit::spawn(
            Command::new("/usr/bin/security")
                .arg("-i")
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null()),
        )?;
        if let Some(mut input) = child.stdin.take() {
            input.write_all(line.as_bytes())?;
        }
        let status = child.wait()?;
        if status.success() {
            Ok(())
        } else {
            Err(io::Error::other(format!(
                "the keychain would not take the renewed sign-in (security exited {})",
                status
                    .code()
                    .map_or_else(|| "on a signal".into(), |c| c.to_string())
            )))
        }
    }
}

/// What `security` exits with when the keychain has no such entry: the low
/// byte of `errSecItemNotFound` (-25300). A locked keychain gives 36
/// (`errSecInteractionNotAllowed`) instead, and a read-only one 195.
const NOT_FOUND: i32 = 44;
/// The longest line handed to `security -i`, newline included: it cuts at
/// 4,096 bytes, and Claude Code itself keeps to this much under that. A
/// sign-in of up to about 1,950 bytes fits.
const LINE_MOST: usize = 4032;
/// The most of a copy of the keychain's sign-in that is carried back, or
/// kept for a run: Claude Code's sign-in is under 1 KB, and a few KB with
/// the sign-ins of MCP servers kept beside it. A bigger file isn't one, and
/// is dropped.
const KEYCHAIN_MOST: u64 = 64 << 10;
/// Said, with why, when a renewed sign-in stays for the run.
const KEPT: &str = "a limited chat's renewed sign-in couldn't go back yet; \
                    the run is given it, and its end tries again";

/// A keychain entry, as `security` found it.
enum Entry {
    Holds(Vec<u8>),
    Missing,
    Unreadable(io::Error),
}

/// What the note of a run's sign-in becomes as the run starts.
#[derive(Debug, PartialEq)]
enum Noted {
    /// This run was given a sign-in of this kind, with this digest.
    Given(Kind, Option<crate::sign_in::Digest>),
    /// A renewed sign-in is still owed, or what is owed isn't known yet.
    Unchanged,
    /// No sign-in: nothing to carry back.
    Nothing,
}

#[cfg(test)]
thread_local! {
    /// A keychain a test made, in place of the person's own.
    static TEST_KEYCHAIN: std::cell::RefCell<Option<PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

/// The keychain the test running on this thread made, named to `security`.
#[cfg(test)]
fn test_keychain() -> Option<PathBuf> {
    TEST_KEYCHAIN.with(|k| k.borrow().clone())
}
/// None outside tests: the person's own keychains are searched.
#[cfg(not(test))]
fn test_keychain() -> Option<PathBuf> {
    None
}

/// Removes the file at `path`, if there is one; a link is removed, not followed.
fn drop_file(path: &Path) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// A word as `security -i` reads one.
fn sh_word(word: &str) -> String {
    format!("\"{}\"", word.replace('\\', "\\\\").replace('"', "\\\""))
}

impl Credentials {
    fn new(link: PathBuf, real: PathBuf) -> Self {
        let began = read_own(&real)
            .ok()
            .map(|bytes| crate::sign_in::digest(&bytes));
        Self { link, real, began }
    }
    /// Carries back a sign-in the agent replaced rather than wrote into,
    /// unless the real one was signed in again, or removed, since the run
    /// began. One that can't go back yet, or a real one that can't be read
    /// to tell, stays, and the next run in the conversation tries again.
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
        if !renewed.is_empty() && self.began.is_some() {
            match crate::sign_in::held_file(&self.real) {
                crate::sign_in::Held::Is(now)
                    if Some(now) == self.began && crate::sign_in::digest(&renewed) != now =>
                {
                    crate::sys::replace_history(&self.real, |file| file.write_all(&renewed))?;
                }
                crate::sign_in::Held::Unknown => {
                    return Err(io::Error::other(
                        "the sign-in on this computer couldn't be read",
                    ));
                }
                _ => {}
            }
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
        self.spec
            .relay
            .as_ref()
            .zip(self.relay.as_ref())
            .map(|(spec, relay)| RelayEndpoint {
                provider: spec.provider,
                port: relay.port(),
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
        let wrapped = self.build(request, &mut run)?;
        run.wrapped = true;
        Ok(wrapped)
    }
    fn own_settings_file(&self, path: &Path) -> bool {
        self.run
            .lock()
            .unwrap()
            .own
            .as_ref()
            .is_some_and(|own| own.holds(path))
    }
    fn prepare(&self, _command: &mut Command) -> io::Result<()> {
        Ok(())
    }
    async fn started(&self) -> Result<(), HarnessError> {
        Ok(())
    }
    async fn update(&self, reach: Reach) -> Applied {
        let mut run = self.run.lock().unwrap();
        if self.spec.offline
            && (reach.internet || reach.local_network || reach.whole_read || reach.whole_write)
        {
            run.restart = true;
            return restart("Offline sandboxes cannot gain network or whole-computer access");
        }
        let reach = match normalize(reach) {
            Ok(reach) => reach,
            Err(e) => {
                run.restart = true;
                return restart(e);
            }
        };
        if self.relay.is_some() && (reach.whole_read || reach.whole_write) {
            run.restart = true;
            return restart("relay runs require explicit folder grants");
        }
        if let Some(proxy) = &self.proxy {
            proxy.update(policy(&reach));
        }
        run.reach.internet = reach.internet;
        run.reach.local_network = reach.local_network;
        if run.restart {
            return restart("a previous change requires this turn to end");
        }
        let loopback = self.relay.is_none() && reach.internet && reach.local_network;
        if run.loopback != loopback {
            run.restart = true;
            return restart(
                "a Mac must restart a running program to change access to this computer's own servers",
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
        if let Some(relay) = &self.relay {
            relay.shutdown();
        }
        let run = self.run.lock().unwrap();
        let sign_in = run
            .credentials
            .as_ref()
            .map_or(Ok(()), Credentials::finish)
            .and(run.keychain.as_ref().map_or(Ok(()), Keychain::finish));
        // Settled: the next run has nothing to carry back. One that couldn't
        // be put back keeps its note and its copy, and is tried again then.
        if sign_in.is_ok()
            && (run.credentials.is_some() || run.keychain.is_some())
            && let Err(e) = crate::sign_in::forget(&self.spec.state)
        {
            tracing::warn!(error = %e, "couldn't clear the note of a limited chat's sign-in");
        }
        if let Some(history) = &run.history {
            history.finished(run.session.as_deref()).map_err(|e| {
                error(format!(
                    "Øphio couldn't copy this turn into the agent's own history on this computer ({e})."
                ))
            })?;
        }
        sign_in.map_err(|e| {
            error(format!(
                "Øphio couldn't put the agent's renewed sign-in back on this computer yet ({e}). \
                 It stays for this chat, and its next reply tries again."
            ))
        })
    }
    async fn connect(&self, addresses: &[SocketAddr]) -> Result<TcpStream, HarnessError> {
        if self.relay.is_some() {
            return Err(error("model-only runs cannot serve host connections"));
        }
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
#[path = "macos_relay_tests.rs"]
mod relay_tests;

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
                disabled: &ports::Disabled::default(),
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
        let output = inherit::output(&mut command).unwrap();
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

    /// The person's own instructions and skills are read through links in the
    /// conversation's settings folder and can't be changed through them; the
    /// rest of their settings folder stays out of reach, and a run that can't
    /// change files isn't shown a skill that says it does.
    #[test]
    fn own_instructions_and_skills_are_read_only_and_nothing_else_is_given() {
        for writes in [false, true] {
            let s = setup();
            let config = s.home.join(".claude");
            let tools = s.outside.parent().unwrap().join("tools");
            let skill = "---\nname: s\ndescription: d\n---\n";
            let writer = "---\nname: w\ndescription: d\nallowed-tools: Edit\n---\n";
            for (path, text) in [
                (config.join("CLAUDE.md"), "mine"),
                (config.join("rules/style.md"), "rule"),
                (config.join("skills/mine/SKILL.md"), skill),
                (config.join("settings.json"), HIDDEN),
                (config.join("agents/a.md"), HIDDEN),
                (tools.join("linked/SKILL.md"), skill),
                (tools.join("writer/SKILL.md"), writer),
            ] {
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(path, text).unwrap();
            }
            for name in ["linked", "writer"] {
                std::os::unix::fs::symlink(tools.join(name), config.join("skills").join(name))
                    .unwrap();
            }
            let mut with_agent = spec(
                &s,
                Reach {
                    write: if writes { vec![s.cwd.clone()] } else { vec![] },
                    read: if writes { vec![] } else { vec![s.cwd.clone()] },
                    ..reach()
                },
            );
            with_agent.harness = HarnessKind::ClaudeCode;
            let sandbox = MacSandbox::new(with_agent).unwrap();
            let line = r#"set -u
c="$CLAUDE_CONFIG_DIR"
test "$(cat "$c/CLAUDE.md")" = mine || exit 11
test "$(cat "$c/rules/style.md")" = rule || exit 12
test -f "$c/skills/mine/SKILL.md" -a -f "$c/skills/linked/SKILL.md" || exit 13
echo x >> "$c/CLAUDE.md" 2>/dev/null && exit 14
echo x > "$c/skills/mine/new" 2>/dev/null && exit 15
echo x >> "$c/skills/linked/SKILL.md" 2>/dev/null && exit 16
test -e "$c/settings.json" -o -e "$c/agents" && exit 17
cat "$HOME/.claude/settings.json" 2>/dev/null && exit 18
cat "$HOME/.claude/agents/a.md" 2>/dev/null && exit 19
if test -e "$c/skills/writer"; then echo writer shown; else echo writer hidden; fi"#;
            let output = run_in(&sandbox, &s, line);
            let said = format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(
                output.status.success(),
                "writes={writes}: {:?} {said}",
                output.status
            );
            assert!(!said.contains(HIDDEN), "{said}");
            let expected = if writes {
                "writer shown"
            } else {
                "writer hidden"
            };
            assert!(said.contains(expected), "writes={writes}: {said}");
            assert_eq!(
                std::fs::read_to_string(config.join("CLAUDE.md")).unwrap(),
                "mine"
            );
            assert!(!config.join("skills/mine/new").exists());
        }
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
                disabled: &ports::Disabled::default(),
                program: Path::new("/bin/sh"),
                visible: &[],
                env: &env,
                resume: None,
            })
            .unwrap();
        let mut child = inherit::spawn(Command::new(&wrapped.program)
            .args(&wrapped.args)
            .args([
                "/usr/bin/python3",
                "-c",
                "import socket;s=socket.socket();s.bind(('127.0.0.1',0));s.listen();print(s.getsockname()[1],flush=True);c,_=s.accept();c.sendall(b'hi');c.close()",
            ])
            .envs(wrapped.env.iter().map(|(k, v)| (k, v)))
            .current_dir(&s.cwd)
            .stdout(std::process::Stdio::piped())
        )
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
        let mut outsider = inherit::spawn(Command::new("/bin/sleep").arg("30")).unwrap();
        let line = format!("kill -9 {}", outsider.id());
        let (ok, _) = held(&s, reach(), &line);
        assert!(!ok);
        assert!(outsider.try_wait().unwrap().is_none(), "still running");
        outsider.kill().unwrap();
        outsider.wait().unwrap();
    }

    /// Where another program of the person's runs from, which folder it is in
    /// and what it has open can't be asked from inside, by `lsof` or by asking
    /// the system directly; a run's own programs can still be asked about.
    #[test]
    fn what_another_program_is_doing_cannot_be_asked() {
        let s = setup();
        let marker = format!("made-up-marker-{}", std::process::id());
        let folder = s.outside.join(&marker);
        std::fs::create_dir(&folder).unwrap();
        let mut outsider = inherit::spawn(
            Command::new("/bin/sh")
                .args(["-c", "/bin/sleep 60; :", &marker])
                .current_dir(&folder),
        )
        .unwrap();
        let pid = outsider.id();

        let (_, lsof_other) = held(&s, reach(), &format!("/usr/sbin/lsof -p {pid}"));
        let (ok, said) = held(&s, reach(), "/usr/sbin/lsof -a -d cwd -p $$");
        assert!(ok && said.contains(&*s.cwd.to_string_lossy()), "{said}");

        // This test's own program asks, inside, the way any program could.
        let me = std::env::current_exe().unwrap();
        let mut given = spec(&s, reach());
        given.programs = vec![me.clone()];
        let sandbox = MacSandbox::new(given).unwrap();
        let output = run_in(
            &sandbox,
            &s,
            &format!(
                "OPHIO_ASK_ABOUT={pid} {} --exact macos::tests::ask_about_a_program --nocapture --test-threads=1",
                sh_word(&me.to_string_lossy())
            ),
        );
        outsider.kill().unwrap();
        outsider.wait().unwrap();
        let said = String::from_utf8_lossy(&output.stdout);
        assert!(!lsof_other.contains(&marker), "{lsof_other}");
        for answer in [
            "other path refused",
            "other name refused",
            "other folder refused",
            "other files refused",
            "own path given",
            "own name given",
            "own folder given",
            "own files given",
            "own command line given",
        ] {
            assert!(said.contains(answer), "{answer}: {said}");
        }
    }

    /// Run inside by the test above, as its own program: says what the system
    /// answers about the program it is told of and about one it starts itself.
    /// Does nothing when run as a test of its own.
    #[test]
    #[allow(unsafe_code)]
    fn ask_about_a_program() {
        let Ok(other) = std::env::var("OPHIO_ASK_ABOUT") else {
            return;
        };
        let other: i32 = other.parse().unwrap();
        let mut own = inherit::spawn(Command::new("/bin/sleep").arg("5")).unwrap();
        let own_pid = own.id() as i32;
        let mut buffer = vec![0_u8; 64 * 1024];
        let size = buffer.len();
        let mut ask = |pid: i32, what: &str| -> bool {
            let pointer = buffer.as_mut_ptr().cast();
            // SAFETY: each call writes at most `size` bytes into the buffer.
            unsafe {
                match what {
                    // A bigger buffer is refused, whatever the program.
                    "path" => {
                        libc::proc_pidpath(pid, pointer, libc::PROC_PIDPATHINFO_MAXSIZE as u32) > 0
                    }
                    "name" => libc::proc_name(pid, pointer, size as u32) > 0,
                    "folder" => {
                        libc::proc_pidinfo(
                            pid,
                            libc::PROC_PIDVNODEPATHINFO,
                            0,
                            pointer,
                            size as i32,
                        ) > 0
                    }
                    "files" => {
                        libc::proc_pidinfo(pid, libc::PROC_PIDLISTFDS, 0, pointer, size as i32) > 0
                    }
                    _ => unreachable!(),
                }
            }
        };
        let say = |who: &str, what: &str, given: bool| {
            println!("{who} {what} {}", if given { "given" } else { "refused" });
        };
        for what in ["path", "name", "folder", "files"] {
            say("other", what, ask(other, what));
            say("own", what, ask(own_pid, what));
        }
        let mut name = [
            libc::CTL_KERN,
            libc::KERN_PROCARGS2,
            std::process::id() as i32,
        ];
        let mut length = buffer.len();
        // SAFETY: `length` is the buffer's size, which the call may not exceed.
        let read = unsafe {
            libc::sysctl(
                name.as_mut_ptr(),
                3,
                buffer.as_mut_ptr().cast(),
                &raw mut length,
                std::ptr::null_mut(),
                0,
            )
        } == 0;
        let mine = read
            && buffer[..length]
                .windows(b"ask_about_a_program".len())
                .any(|w| w == b"ask_about_a_program");
        say("own", "command line", mine);
        own.kill().unwrap();
        own.wait().unwrap();
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
                    disabled: &ports::Disabled::default(),
                    program: Path::new("/bin/sh"),
                    visible: &[],
                    env: &env,
                    resume: None,
                })
                .is_err()
        );
    }

    #[test]
    fn an_offline_run_has_no_proxy() {
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
                disabled: &ports::Disabled::default(),
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
                    disabled: &ports::Disabled::default(),
                    program: &program,
                    visible: &[],
                    env: &env,
                    resume: None,
                })
                .unwrap();
            let output = inherit::output(
                Command::new(&wrapped.program)
                    .args(&wrapped.args)
                    .arg(&program)
                    .arg("--version")
                    .envs(wrapped.env.iter().map(|(k, v)| (k, v)))
                    .env("PATH", "/opt/homebrew/bin:/usr/bin:/bin")
                    .current_dir(&s.cwd),
            )
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
                disabled: &ports::Disabled::default(),
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
        let output = inherit::output(
            Command::new(&wrapped.program)
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
                .stdin(std::process::Stdio::null()),
        )
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
        let keychain = TestKeychain::new();
        let service = "ophio-sandbox-planted";
        assert!(keychain.set(service, "given"));
        Keychain {
            service: service.into(),
            account: user(),
            file: file.clone(),
            given: crate::sign_in::digest(b"given"),
        }
        .finish()
        .unwrap();
        assert!(
            !file.is_symlink() && !file.exists(),
            "the link is taken away"
        );
        assert_eq!(keychain.holds(service), Some(b"given".to_vec()));
        drop(keychain);

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

    /// Øphio stopped mid-run after the agent renewed its sign-in: the next
    /// run in the conversation carries it back and is given it, unless the
    /// person signed in again meanwhile. A run that ends leaves no note.
    #[test]
    fn a_sign_in_renewed_in_a_run_ophio_did_not_finish_goes_back_at_the_next_start() {
        let s = setup();
        let config = s.home.join(".codex");
        std::fs::create_dir(&config).unwrap();
        let real = config.join("auth.json");
        std::fs::write(&real, "given").unwrap();
        let place = s.state.join("native/auth.json");
        let note = s.state.join(crate::sign_in::RECORD);
        let runtime = tokio::runtime::Runtime::new().unwrap();

        let first = MacSandbox::new(spec(&s, reach())).unwrap();
        wrapped_shell(&first, &s);
        assert_eq!(std::fs::read_link(&place).unwrap(), real);
        assert!(note.exists());
        // The agent replaces its sign-in, and Øphio stops before the run ends.
        std::fs::remove_file(&place).unwrap();
        std::fs::write(&place, "renewed").unwrap();
        drop(first);

        let second = MacSandbox::new(spec(&s, reach())).unwrap();
        wrapped_shell(&second, &s);
        assert_eq!(std::fs::read_to_string(&real).unwrap(), "renewed");
        assert_eq!(std::fs::read_link(&place).unwrap(), real);
        runtime.block_on(second.finished()).unwrap();
        assert!(!note.exists(), "a finished run leaves no note");

        // Signed in again while an unfinished run's renewal waited: theirs stays.
        let third = MacSandbox::new(spec(&s, reach())).unwrap();
        wrapped_shell(&third, &s);
        std::fs::remove_file(&place).unwrap();
        std::fs::write(&place, "renewed again").unwrap();
        drop(third);
        std::fs::write(&real, "signed in again").unwrap();
        let fourth = MacSandbox::new(spec(&s, reach())).unwrap();
        wrapped_shell(&fourth, &s);
        assert_eq!(std::fs::read_to_string(&real).unwrap(), "signed in again");
        assert_eq!(std::fs::read_link(&place).unwrap(), real);
        runtime.block_on(fourth.finished()).unwrap();
        assert!(!note.exists());
    }

    /// The person signed out (`codex logout` removes the file) while a run's
    /// renewal waited: it doesn't keep the agent signed in.
    #[test]
    fn a_sign_in_file_removed_since_takes_an_unfinished_runs_renewal_with_it() {
        let s = setup();
        let config = s.home.join(".codex");
        std::fs::create_dir(&config).unwrap();
        let real = config.join("auth.json");
        std::fs::write(&real, "given").unwrap();
        let place = s.state.join("native/auth.json");
        let note = s.state.join(crate::sign_in::RECORD);

        let first = MacSandbox::new(spec(&s, reach())).unwrap();
        wrapped_shell(&first, &s);
        std::fs::remove_file(&place).unwrap();
        std::fs::write(&place, "renewed").unwrap();
        drop(first);
        std::fs::remove_file(&real).unwrap();

        let second = MacSandbox::new(spec(&s, reach())).unwrap();
        wrapped_shell(&second, &s);
        assert!(
            std::fs::symlink_metadata(&place).is_err(),
            "nothing is left"
        );
        assert!(!real.exists(), "the sign-out stays");
        assert!(!note.exists());
    }

    /// The renewal of an unfinished run can't go back yet (here the person's
    /// settings folder won't take a file): the next run starts with it as its
    /// own sign-in, keeps the note, and its end puts it back.
    #[test]
    fn a_sign_in_file_that_cant_go_back_yet_is_the_next_runs_own() {
        use std::os::unix::fs::PermissionsExt;
        let s = setup();
        let config = s.home.join(".codex");
        std::fs::create_dir(&config).unwrap();
        let real = config.join("auth.json");
        std::fs::write(&real, "given").unwrap();
        let place = s.state.join("native/auth.json");
        let note = s.state.join(crate::sign_in::RECORD);
        let noted = || std::fs::read(&note).unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap();

        let first = MacSandbox::new(spec(&s, reach())).unwrap();
        wrapped_shell(&first, &s);
        let before = noted();
        std::fs::remove_file(&place).unwrap();
        std::fs::write(&place, "renewed").unwrap();
        drop(first);

        let mode = |m| std::fs::set_permissions(&config, std::fs::Permissions::from_mode(m));
        mode(0o555).unwrap();
        let second = MacSandbox::new(spec(&s, reach())).unwrap();
        let wrapped = wrapped_shell(&second, &s);
        let read = run_wrapped(&wrapped, &s, "cat \"$CODEX_HOME/auth.json\"");
        let unchanged = std::fs::read_to_string(&real).unwrap();
        let ended = runtime.block_on(second.finished());
        let kept = std::fs::read_to_string(&place);
        mode(0o755).unwrap();
        assert_eq!(String::from_utf8_lossy(&read.stdout), "renewed");
        assert_eq!(unchanged, "given");
        assert!(ended.is_err(), "the end tries again and says it couldn't");
        assert_eq!(kept.unwrap(), "renewed", "kept for the next try");
        assert_eq!(noted(), before, "the note is as it was");

        let third = MacSandbox::new(spec(&s, reach())).unwrap();
        wrapped_shell(&third, &s);
        assert_eq!(std::fs::read_to_string(&real).unwrap(), "renewed");
        assert_eq!(std::fs::read_link(&place).unwrap(), real);
        runtime.block_on(third.finished()).unwrap();
        assert!(!note.exists());
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
            let _ = inherit::output(Command::new("/bin/launchctl").args(["remove", &label]));
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

    /// One test with a keychain of its own at a time: each puts the keychain
    /// search list back as it found it.
    static KEYCHAINS: Mutex<()> = Mutex::new(());

    /// A keychain made for one test, with a random password, that [`Keychain`]
    /// uses on the test's thread in place of the person's own. Over SSH the
    /// login keychain takes nothing, and this one does. It is deleted
    /// afterwards, and the search list left exactly as it was found.
    struct TestKeychain {
        path: PathBuf,
        dir: tempfile::TempDir,
        password: String,
        searched: String,
        _one: std::sync::MutexGuard<'static, ()>,
    }

    fn security(args: &[&str]) -> std::process::Output {
        inherit::output(
            Command::new("/usr/bin/security")
                .args(args)
                .stdin(std::process::Stdio::null()),
        )
        .unwrap()
    }

    /// A line for `security`'s own prompt, so a password isn't an argument.
    fn security_line(line: &str) -> std::process::Output {
        let mut child = inherit::spawn(
            Command::new("/usr/bin/security")
                .arg("-i")
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped()),
        )
        .unwrap();
        writeln!(child.stdin.take().unwrap(), "{line}").unwrap();
        child.wait_with_output().unwrap()
    }

    fn search_list() -> String {
        String::from_utf8(security(&["list-keychains", "-d", "user"]).stdout).unwrap()
    }

    fn put_back(list: &str) {
        let paths: Vec<&str> = list.lines().map(|l| l.trim().trim_matches('"')).collect();
        let mut args = vec!["list-keychains", "-d", "user", "-s"];
        args.extend(paths.iter().filter(|p| !p.is_empty()));
        assert!(security(&args).status.success());
    }

    fn user() -> String {
        std::env::var("USER").unwrap()
    }

    impl TestKeychain {
        fn new() -> Self {
            let one = KEYCHAINS.lock().unwrap_or_else(|e| e.into_inner());
            let searched = search_list();
            let dir = tempfile::Builder::new()
                .prefix("kc")
                .tempdir_in("/private/tmp")
                .unwrap();
            let path = dir.path().canonicalize().unwrap().join("test.keychain-db");
            let mut random = [0u8; 24];
            std::io::Read::read_exact(
                &mut std::fs::File::open("/dev/urandom").unwrap(),
                &mut random,
            )
            .unwrap();
            let password: String = random.iter().map(|b| format!("{b:02x}")).collect();
            let made = security_line(&format!(
                "create-keychain -p {} {}",
                sh_word(&password),
                sh_word(&path.to_string_lossy())
            ));
            // Made, it was added to the search list: taken off again at once.
            put_back(&searched);
            assert!(
                made.status.success() && path.exists(),
                "a keychain couldn't be made: {}",
                String::from_utf8_lossy(&made.stderr)
            );
            let keychain = Self {
                path,
                dir,
                password,
                searched,
                _one: one,
            };
            keychain.unlock();
            TEST_KEYCHAIN.with(|k| *k.borrow_mut() = Some(keychain.path.clone()));
            keychain
        }

        /// Stores a made-up value for `service`, under the account the copy names.
        fn set(&self, service: &str, value: &str) -> bool {
            security(&[
                "add-generic-password",
                "-U",
                "-a",
                &user(),
                "-s",
                service,
                "-w",
                value,
                &self.path.to_string_lossy(),
            ])
            .status
            .success()
        }

        fn holds(&self, service: &str) -> Option<Vec<u8>> {
            match Keychain::read(service) {
                Entry::Holds(held) => Some(held),
                Entry::Missing => None,
                Entry::Unreadable(e) => panic!("{e}"),
            }
        }

        fn lock(&self) {
            assert!(
                security(&["lock-keychain", &self.path.to_string_lossy()])
                    .status
                    .success()
            );
        }

        fn unlock(&self) {
            let unlocked = security_line(&format!(
                "unlock-keychain -p {} {}",
                sh_word(&self.password),
                sh_word(&self.path.to_string_lossy())
            ));
            assert!(unlocked.status.success());
        }

        /// Its file and folder can be read and not written.
        fn read_only(&self, read_only: bool) {
            use std::os::unix::fs::PermissionsExt;
            let (file, folder) = if read_only {
                (0o444, 0o555)
            } else {
                (0o644, 0o755)
            };
            let set = |path: &Path, mode| {
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap()
            };
            set(self.dir.path(), folder);
            set(&self.path, file);
        }
    }

    impl Drop for TestKeychain {
        fn drop(&mut self) {
            TEST_KEYCHAIN.with(|k| *k.borrow_mut() = None);
            self.read_only(false);
            let deleted = security(&["delete-keychain", &self.path.to_string_lossy()]);
            put_back(&self.searched);
            let after = search_list();
            eprintln!(
                "test keychain deleted: {}; search list before: {}; after: {}",
                deleted.status.success(),
                self.searched
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" "),
                after.split_whitespace().collect::<Vec<_>>().join(" ")
            );
            if !std::thread::panicking() {
                assert!(deleted.status.success());
                assert!(!self.path.exists());
                assert_eq!(after, self.searched);
            }
        }
    }

    /// The copy of a keychain sign-in a run was given.
    fn given(service: &str, file: &Path, value: &[u8]) -> Keychain {
        Keychain {
            service: service.into(),
            account: user(),
            file: file.to_owned(),
            given: crate::sign_in::digest(value),
        }
    }

    #[test]
    fn a_keychain_sign_in_is_copied_for_the_run_and_a_renewal_goes_back() {
        let keychain = TestKeychain::new();
        let s = setup();
        let service = "ophio-sandbox-test";
        assert!(keychain.set(service, "given"));
        let file = s.state.join(".credentials.json");
        std::fs::write(&file, "renewed").unwrap();
        given(service, &file, b"given").finish().unwrap();
        assert!(!file.exists());
        assert_eq!(keychain.holds(service), Some(b"renewed".to_vec()));
    }

    /// Øphio stopped mid-run after Claude Code renewed the copy of its keychain
    /// sign-in: the next run puts it in the keychain and is given what was put
    /// there, unless the keychain was signed in again meanwhile.
    #[test]
    fn a_keychain_renewal_left_by_an_unfinished_run_goes_back_at_the_next_start() {
        let keychain = TestKeychain::new();
        let s = setup();
        let named = s.state.join("claude-config").to_string_lossy().into_owned();
        let service = Keychain::service(Some(&named));
        assert!(keychain.set(&service, "given"));
        let local = s.state.join("native");
        std::fs::create_dir(&local).unwrap();
        let file = local.join(".credentials.json");
        let noted = |value: &[u8]| {
            crate::sign_in::remember(
                &s.state,
                Kind::Keychain,
                Some(crate::sign_in::digest(value)),
            )
            .unwrap()
        };
        let digest = crate::sign_in::digest;

        noted(b"given");
        std::fs::write(&file, "renewed").unwrap();
        let (run, note) = Keychain::copy(Some(&named), &s.home, &local, &s.state).unwrap();
        assert_eq!(keychain.holds(&service), Some(b"renewed".to_vec()));
        assert_eq!(std::fs::read(&file).unwrap(), b"renewed");
        assert_eq!(run.unwrap().given, digest(b"renewed"));
        assert_eq!(note, Noted::Given(Kind::Keychain, Some(digest(b"renewed"))));

        // Signed in again in between: theirs stays and is what the run gets.
        assert!(keychain.set(&service, "signed in again"));
        noted(b"renewed");
        std::fs::write(&file, "renewed again").unwrap();
        let (run, _) = Keychain::copy(Some(&named), &s.home, &local, &s.state).unwrap();
        assert_eq!(keychain.holds(&service), Some(b"signed in again".to_vec()));
        assert_eq!(std::fs::read(&file).unwrap(), b"signed in again");
        assert_eq!(run.unwrap().given, digest(b"signed in again"));
    }

    /// Signed out of the keychain (no entry, `security` exits 44) while a
    /// run's renewal waited: it doesn't keep the agent signed in.
    #[test]
    fn a_keychain_sign_out_drops_what_an_unfinished_run_left() {
        let _keychain = TestKeychain::new();
        let s = setup();
        let named = s.state.join("claude-config").to_string_lossy().into_owned();
        let local = s.state.join("native");
        std::fs::create_dir(&local).unwrap();
        let file = local.join(".credentials.json");
        crate::sign_in::remember(
            &s.state,
            Kind::Keychain,
            Some(crate::sign_in::digest(b"given")),
        )
        .unwrap();
        std::fs::write(&file, "renewed").unwrap();
        let (run, note) = Keychain::copy(Some(&named), &s.home, &local, &s.state).unwrap();
        assert!(run.is_none());
        assert_eq!(note, Noted::Nothing);
        assert!(!file.exists());
    }

    /// A locked keychain can't be read, which says nothing about a sign-out:
    /// as a run starts, an unfinished run's renewal stays as its sign-in with
    /// the note as it was; as one ends, its copy stays. Each goes back once
    /// the keychain can be read.
    #[test]
    fn a_keychain_that_cant_be_read_is_not_taken_for_a_sign_out() {
        let keychain = TestKeychain::new();
        let s = setup();
        let named = s.state.join("claude-config").to_string_lossy().into_owned();
        let service = Keychain::service(Some(&named));
        assert!(keychain.set(&service, "given"));
        let local = s.state.join("native");
        std::fs::create_dir(&local).unwrap();
        let file = local.join(".credentials.json");
        let note = s.state.join(crate::sign_in::RECORD);
        crate::sign_in::remember(
            &s.state,
            Kind::Keychain,
            Some(crate::sign_in::digest(b"given")),
        )
        .unwrap();
        let before = std::fs::read(&note).unwrap();
        std::fs::write(&file, "renewed").unwrap();

        // At the start.
        keychain.lock();
        assert!(matches!(Keychain::read(&service), Entry::Unreadable(_)));
        let (run, noted) = Keychain::copy(Some(&named), &s.home, &local, &s.state).unwrap();
        let run = run.expect("the renewal is this run's sign-in");
        assert_eq!(noted, Noted::Unchanged);
        assert_eq!(run.given, crate::sign_in::digest(b"given"));
        assert_eq!(std::fs::read(&file).unwrap(), b"renewed");
        assert_eq!(std::fs::read(&note).unwrap(), before);

        // At the end, renewed again and still locked.
        std::fs::write(&file, "renewed again").unwrap();
        assert!(run.finish().is_err());
        assert_eq!(std::fs::read(&file).unwrap(), b"renewed again");
        keychain.unlock();
        assert_eq!(keychain.holds(&service), Some(b"given".to_vec()));
        run.finish().unwrap();
        assert_eq!(keychain.holds(&service), Some(b"renewed again".to_vec()));
        assert!(!file.exists());

        // Locked with nothing left: nothing to give, and the note stays.
        keychain.lock();
        let (run, noted) = Keychain::copy(Some(&named), &s.home, &local, &s.state).unwrap();
        assert!(run.is_none());
        assert_eq!(noted, Noted::Unchanged);
        assert!(note.exists());
    }

    /// A keychain that won't take the renewal (here it can be read but not
    /// written): the end of the run keeps its copy; the next run starts with
    /// it as its own sign-in rather than refusing, and its end puts it back.
    #[test]
    fn a_keychain_that_wont_take_a_renewal_keeps_it_for_the_next_try() {
        let keychain = TestKeychain::new();
        let s = setup();
        let named = s.state.join("claude-config").to_string_lossy().into_owned();
        let service = Keychain::service(Some(&named));
        assert!(keychain.set(&service, "given"));
        let local = s.state.join("native");
        std::fs::create_dir(&local).unwrap();
        let file = local.join(".credentials.json");
        crate::sign_in::remember(
            &s.state,
            Kind::Keychain,
            Some(crate::sign_in::digest(b"given")),
        )
        .unwrap();
        std::fs::write(&file, "renewed").unwrap();

        keychain.read_only(true);
        // The end of a run.
        let ended = given(&service, &file, b"given").finish();
        assert!(ended.is_err());
        assert_eq!(std::fs::read(&file).unwrap(), b"renewed");
        assert_eq!(keychain.holds(&service), Some(b"given".to_vec()));
        // The next start.
        let (run, noted) = Keychain::copy(Some(&named), &s.home, &local, &s.state).unwrap();
        let run = run.expect("the run starts with the renewal");
        assert_eq!(noted, Noted::Unchanged);
        assert_eq!(std::fs::read(&file).unwrap(), b"renewed");
        assert_eq!(keychain.holds(&service), Some(b"given".to_vec()));
        keychain.read_only(false);
        run.finish().unwrap();
        assert_eq!(keychain.holds(&service), Some(b"renewed".to_vec()));
        assert!(!file.exists());
    }

    /// `security`'s prompt cuts a line at 4,096 bytes: a sign-in too long for
    /// it is never stored cut, and stays with the run instead. Made-up values.
    #[test]
    fn a_long_sign_in_is_never_stored_cut() {
        let keychain = TestKeychain::new();
        let s = setup();
        let service = "ophio-sandbox-long";
        assert!(keychain.set(service, "given"));
        let file = s.state.join(".credentials.json");
        for size in [3 << 10, 6 << 10] {
            let long = vec![b'x'; size];
            assert!(Keychain::put(&user(), service, &long).is_err(), "{size}");
            std::fs::write(&file, &long).unwrap();
            assert!(given(service, &file, b"given").finish().is_err(), "{size}");
            assert_eq!(keychain.holds(service), Some(b"given".to_vec()), "{size}");
            assert_eq!(std::fs::read(&file).unwrap().len(), size);
        }
        // The longest that fits goes in whole, and one byte more doesn't.
        let overhead = format!(
            "add-generic-password -U -a {} -s {} -X  {}\n",
            sh_word(&user()),
            sh_word(service),
            sh_word(&keychain.path.to_string_lossy())
        )
        .len();
        let most = (LINE_MOST - overhead) / 2;
        let fits: Vec<u8> = (0..most).map(|i| b'a' + (i % 26) as u8).collect();
        Keychain::put(&user(), service, &fits).unwrap();
        assert_eq!(keychain.holds(service), Some(fits.clone()));
        let over = [fits.as_slice(), b"z"].concat();
        assert!(Keychain::put(&user(), service, &over).is_err());
        assert_eq!(keychain.holds(service), Some(fits));
        eprintln!("the longest sign-in carried back here: {most} bytes");
        // Why: a longer line is cut, and what is past the cut runs as a
        // command of its own (a made-up value; nothing is stored).
        let cut = security_line(&format!(
            "add-generic-password -U -a {} -s {} -X {} {}",
            sh_word(&user()),
            sh_word("ophio-sandbox-cut"),
            "61".repeat(2100),
            sh_word(&keychain.path.to_string_lossy())
        ));
        let said = String::from_utf8_lossy(&cut.stderr);
        assert!(said.contains("unknown command"), "{said}");
    }

    /// Signed in again while a run went, from the phone say: the run's older
    /// sign-in, renewed or not, doesn't go back over the new one.
    #[test]
    fn a_sign_in_made_during_a_run_stays_in_the_keychain() {
        let keychain = TestKeychain::new();
        let s = setup();
        let service = "ophio-sandbox-newer";
        assert!(keychain.set(service, "given"));
        let file = s.state.join(".credentials.json");
        std::fs::write(&file, "the run's renewal").unwrap();
        assert!(keychain.set(service, "signed in again"));
        given(service, &file, b"given").finish().unwrap();
        assert!(!file.exists());
        assert_eq!(keychain.holds(service), Some(b"signed in again".to_vec()));
    }

    /// Signed out while a run went: its sign-in doesn't come back.
    #[test]
    fn a_sign_out_during_a_run_is_not_undone() {
        let keychain = TestKeychain::new();
        let s = setup();
        let service = "ophio-sandbox-gone";
        assert!(keychain.set(service, "given"));
        let file = s.state.join(".credentials.json");
        std::fs::write(&file, "the run's renewal").unwrap();
        assert!(
            security(&[
                "delete-generic-password",
                "-s",
                service,
                &keychain.path.to_string_lossy()
            ])
            .status
            .success()
        );
        given(service, &file, b"given").finish().unwrap();
        assert!(!file.exists());
        assert_eq!(keychain.holds(service), None);
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
