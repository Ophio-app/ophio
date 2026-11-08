// SPDX-License-Identifier: Apache-2.0
//! Opening installed apps and web addresses for desktop voice control, the
//! way the desktop's own launcher would: from its desktop entries, with the
//! defaults the person chose (xdg MIME defaults, the preferred terminal).
//!
//! Every app opens in a scope of its own under the session's `app.slice`,
//! so it belongs to the person's session rather than to Øphio: restarting
//! or updating Øphio never closes it.
use crate::windows::apps::{self, Desktop};
use async_trait::async_trait;
use ports::PlatformError;
use protocol::voice::VoiceApp;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::Duration,
};
use voice_control::{
    Authority, Error,
    apps::{AppRole, LaunchApp},
};

// How long each step of a launch may take. The display an agent's launch
// is on stays locked throughout, and its renewals wait; no other display's
// do. Before the final authority check, nothing has started:
/// Reading the desktop entries again, and checking the chosen one is
/// unchanged.
const ENTRIES: Duration = Duration::from_millis(1000);
/// Finding the programs a launch runs, along PATH, and the home folder it
/// starts in: a few metadata reads, but a hung network home folder could
/// hold them indefinitely.
const PROGRAMS: Duration = Duration::from_millis(500);
/// Asking the user service manager for the session's environment.
const SESSION_LOOKUP: Duration = Duration::from_millis(1000);
// After it, the helper runs:
/// For the helper to hand the app to the desktop. A healthy one takes a few
/// milliseconds; one still running after this is taken for hung and is
/// stopped, app and all, so a slow service manager mustn't reach it. No
/// longer, because an agent's display stays locked meanwhile, and its lease
/// lapses about two seconds after its last renewal.
const HELPER: Duration = Duration::from_millis(2000);
/// Each of the two calls that stop a hung launch's scope and confirm it is
/// gone, and each later check of one that couldn't be. A few milliseconds
/// when healthy; generous, because a scope that can't be confirmed stopped
/// marks the display until a later check confirms it, or Ophio restarts.
const CLEANUP_STEP: Duration = Duration::from_millis(1000);
/// For the killed helper to be reaped. Killed, it can start nothing more,
/// so a launch never waits on the kernel beyond this to answer.
const REAP: Duration = Duration::from_millis(500);
// Within what every launcher promises voice control and the releases that
// wait for launches.
const _: () = {
    let after_check = HELPER.as_millis() + 2 * CLEANUP_STEP.as_millis() + REAP.as_millis();
    assert!(after_check <= voice_control::LAUNCH_AFTER_CHECK.as_millis());
    assert!(
        ENTRIES.as_millis() + PROGRAMS.as_millis() + SESSION_LOOKUP.as_millis() + after_check
            <= voice_control::LAUNCH_LIMIT.as_millis()
    );
};
#[derive(Debug)]
pub struct LinuxLauncher {
    environment: BTreeMap<String, String>,
    roots: Vec<PathBuf>,
}
impl LinuxLauncher {
    /// Reads nothing until asked. `environment` is the session's: HOME,
    /// XDG directories, the desktop name and the display.
    pub fn from_environment(environment: &BTreeMap<String, String>) -> Self {
        Self::new(environment.clone(), apps::roots(environment))
    }
    pub fn new(environment: BTreeMap<String, String>, roots: Vec<PathBuf>) -> Self {
        Self { environment, roots }
    }
    /// Every app a command may open, with the roles and defaults the
    /// desktop gives them.
    pub(crate) async fn catalog(&self) -> Result<Vec<(Desktop, LaunchApp)>, PlatformError> {
        let hyprland = hyprland_terminal(&self.environment).await;
        let environment = self.environment.clone();
        let roots = self.roots.clone();
        tokio::task::spawn_blocking(move || {
            let desktops = apps::desktops(&roots, &environment)?;
            let mut preferences = Preferences::read(&environment);
            if let Some(words) = hyprland {
                preferences.terminal.insert(0, Pick::Command(words));
            }
            Ok(catalog(desktops, &preferences))
        })
        .await
        .map_err(|_| PlatformError::Native("The app list could not be read".into()))?
    }
}
fn unavailable(e: PlatformError) -> Error {
    match e {
        PlatformError::LaunchUnstopped => Error::LaunchUnstopped,
        PlatformError::OutcomeUnknown(_) => Error::OutcomeUnknown,
        PlatformError::PermissionMissing(_) => Error::Stopped,
        PlatformError::TargetNotFound => Error::StaleTarget,
        PlatformError::TemporarilyUnavailable(reason) => Error::Unavailable(reason),
        other => Error::Unavailable(other.to_string()),
    }
}
#[async_trait]
impl voice_control::Launcher for LinuxLauncher {
    async fn apps(&self) -> Result<Vec<LaunchApp>, Error> {
        if apps::program_path(&self.environment, "gio").is_none() {
            return Err(Error::Unavailable(NO_GIO.into()));
        }
        Ok(self
            .catalog()
            .await
            .map_err(unavailable)?
            .into_iter()
            .map(|(_, app)| app)
            .collect())
    }
    async fn launch(&self, app: &VoiceApp, authority: &dyn Authority) -> Result<(), Error> {
        // The same app as the one chosen, read again now: an entry that
        // has gone or changed name is refused rather than guessed at.
        // Bounded: a stuck file system mustn't keep the display locked. The
        // read itself can't be cancelled, but nothing has started.
        let (roots, environment) = (self.roots.clone(), self.environment.clone());
        let (id, name) = (app.id.clone(), app.name.clone());
        let read = tokio::task::spawn_blocking(move || {
            let desktop = apps::desktops(&roots, &environment)
                .map_err(unavailable)?
                .into_iter()
                .find(|d| d.id == id && d.name == name)
                .ok_or(Error::StaleTarget)?;
            if !apps::unchanged(&desktop.path, desktop.fingerprint) {
                return Err(Error::StaleTarget);
            }
            Ok(desktop)
        });
        let desktop = tokio::time::timeout(ENTRIES, read)
            .await
            .map_err(|_| Error::Unavailable(SLOW_ENTRIES.into()))?
            .map_err(|_| Error::Unavailable("The app list could not be read".into()))??;
        open_entry(
            &self.environment,
            &desktop.path,
            &desktop.name,
            &|| authority.check().map_err(stopped),
            &|launch| authority.unstopped(launch),
        )
        .await
        .map_err(unavailable)
    }
    async fn open_url(&self, url: &str, authority: &dyn Authority) -> Result<(), Error> {
        let lower = url.to_ascii_lowercase();
        if !(lower.starts_with("https://") || lower.starts_with("http://"))
            || url.len() > 2048
            || url.chars().any(|c| c.is_whitespace() || c.is_control())
        {
            return Err(Error::StaleTarget);
        }
        scoped(
            &self.environment,
            "url",
            &["open", url],
            "the address",
            &|| authority.check().map_err(stopped),
            &|launch| authority.unstopped(launch),
        )
        .await
        .map_err(unavailable)
    }
}
/// The check run last, just before the opener's process starts.
pub(crate) type Check<'a> = &'a (dyn Fn() -> Result<(), PlatformError> + Sync);
/// Told of a launch that couldn't be confirmed stopped, with how to check
/// it again.
pub(crate) type Report<'a> = &'a (dyn Fn(std::sync::Arc<dyn ports::LaunchCheck>) + Sync);
fn stopped(_: Error) -> PlatformError {
    PlatformError::PermissionMissing("Control ended before it opened".into())
}
const NO_GIO: &str = "Opening apps by voice needs GLib’s gio on the computer";
const SLOW_ENTRIES: &str =
    "The computer took too long to read its app list. Nothing was opened; ask again";
const SLOW_PROGRAMS: &str =
    "The computer took too long to find the programs that open apps. Nothing was opened; ask again";
/// The programs a launch runs and the folder it starts in, found before
/// anything starts.
struct Programs {
    gio: PathBuf,
    /// `systemd-run`, when the user's service manager is there to run a
    /// scope.
    systemd: Option<PathBuf>,
    systemctl: Option<PathBuf>,
    home: PathBuf,
}
/// Makes a test's lookup take this many milliseconds, as on a hung mount.
#[cfg(test)]
const SLOW_LOOKUP: &str = "OPHIO_TEST_SLOW_LOOKUP_MS";
impl Programs {
    fn find(environment: &BTreeMap<String, String>) -> Result<Self, PlatformError> {
        #[cfg(test)]
        if let Some(ms) = environment.get(SLOW_LOOKUP).and_then(|ms| ms.parse().ok()) {
            std::thread::sleep(Duration::from_millis(ms));
        }
        let gio = apps::program_path(environment, "gio")
            .ok_or_else(|| PlatformError::Unsupported(NO_GIO.into()))?;
        let systemd = apps::program_path(environment, "systemd-run").filter(|_| {
            environment
                .get("XDG_RUNTIME_DIR")
                .is_some_and(|dir| Path::new(dir).join("systemd/private").exists())
        });
        Ok(Self {
            gio,
            systemd,
            systemctl: apps::program_path(environment, "systemctl"),
            home: environment
                .get("HOME")
                .map(PathBuf::from)
                .filter(|h| h.is_absolute() && h.is_dir())
                .unwrap_or_else(|| PathBuf::from("/")),
        })
    }
    /// Found within [`PROGRAMS`], or the launch is refused. The lookup
    /// can't be cancelled, but nothing has started: what it finds late is
    /// dropped.
    async fn bounded(environment: &BTreeMap<String, String>) -> Result<Self, PlatformError> {
        let environment = environment.clone();
        tokio::time::timeout(
            PROGRAMS,
            tokio::task::spawn_blocking(move || Self::find(&environment)),
        )
        .await
        .map_err(|_| PlatformError::TemporarilyUnavailable(SLOW_PROGRAMS.into()))?
        .map_err(|_| {
            PlatformError::Native("The programs that open apps could not be found".into())
        })?
    }
}
/// Opens one desktop entry the way the desktop's launcher would.
pub(crate) async fn open_entry(
    environment: &BTreeMap<String, String>,
    path: &Path,
    name: &str,
    check: Check<'_>,
    report: Report<'_>,
) -> Result<(), PlatformError> {
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or(PlatformError::TargetNotFound)?;
    let path = path.to_str().ok_or(PlatformError::TargetNotFound)?;
    scoped(environment, stem, &["launch", path], name, check, report).await
}
/// Runs `gio <args>` in a scope of its own, with the session's environment.
async fn scoped(
    environment: &BTreeMap<String, String>,
    stem: &str,
    args: &[&str],
    what: &str,
    check: Check<'_>,
    report: Report<'_>,
) -> Result<(), PlatformError> {
    use std::process::Stdio;
    let Programs {
        gio,
        systemd,
        systemctl,
        home,
    } = Programs::bounded(environment).await?;
    let session = session_environment(environment).await;
    let unit = unit(stem);
    let mut command = match &systemd {
        Some(run) => {
            let mut command = tokio::process::Command::new(run);
            command
                .args([
                    "--user",
                    "--scope",
                    "--quiet",
                    "--collect",
                    "--slice=app.slice",
                ])
                .arg(format!("--unit={unit}"))
                .arg("--")
                .arg(&gio);
            command
        }
        None => tokio::process::Command::new(&gio),
    };
    command
        .args(args)
        .env_clear()
        .envs(&session)
        .current_dir(home)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        // A launch abandoned halfway must not finish later.
        .kill_on_drop(true);
    // Its own process group, so whatever it starts can be stopped with it.
    #[cfg(unix)]
    command.process_group(0);
    // Everything is ready: authority is the last thing before it starts.
    check()?;
    let mut child = command.spawn()?;
    // From here a hung launch answers within HELPER, two CLEANUP_STEPs and
    // REAP (4.5 s); with the entries, the programs and the session lookup
    // before it, 7 s.
    match tokio::time::timeout(HELPER, child.wait()).await {
        Ok(Ok(status)) if status.success() => Ok(()),
        Ok(Ok(_)) => Err(PlatformError::Native(format!(
            "The desktop could not open {what}"
        ))),
        Ok(Err(e)) => Err(e.into()),
        Err(_) => {
            // Stop it, everything it started and its scope, before saying
            // so: once voice control's stop barrier has let go of the
            // display, nothing may still open. A scope that can't be
            // confirmed stopped is reported as such, never as stopped.
            // Without a scope, whatever left the helper's process group
            // (a `setsid` child, say) can't be seen, so the group is killed
            // but the launch is never confirmed stopped, then or later.
            let scope = systemd.as_ref().and(systemctl).map(|systemctl| ScopeCheck {
                systemctl,
                session: session.clone(),
                unit: unit.clone(),
            });
            let stopped = match &scope {
                Some(scope) => scope.gone(true).await,
                None => false,
            };
            kill_group(&mut child).await;
            if !stopped {
                // systemd can be asked again later, and its word then is
                // the same proof as now.
                if let Some(scope) = scope {
                    report(std::sync::Arc::new(scope));
                }
                return Err(PlatformError::LaunchUnstopped);
            }
            Err(PlatformError::OutcomeUnknown(format!(
                "The desktop has not confirmed opening {what}. Check the computer before asking again"
            )))
        }
    }
}
/// Kills the helper's whole process group, then reaps the helper, waiting
/// at most [`REAP`]: one the kernel holds (on a hung mount, say) is reaped
/// later by the runtime.
async fn kill_group(child: &mut tokio::process::Child) {
    #[cfg(unix)]
    if let Some(group) = child
        .id()
        .and_then(|id| i32::try_from(id).ok())
        .and_then(rustix::process::Pid::from_raw)
    {
        let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
    }
    let _ = tokio::time::timeout(REAP, child.kill()).await;
}
/// A launch's scope, as systemd can be asked about it again.
#[derive(Debug)]
pub(crate) struct ScopeCheck {
    systemctl: PathBuf,
    session: BTreeMap<String, String>,
    unit: String,
}
#[async_trait]
impl ports::LaunchCheck for ScopeCheck {
    async fn stopped(&self, stop: bool) -> bool {
        self.gone(stop).await
    }
}
impl ScopeCheck {
    /// Kills the launch's scope first with `kill`, and confirms it is gone:
    /// true only when systemd itself says the scope is inactive. Each call
    /// to systemd takes at most [`CLEANUP_STEP`].
    async fn gone(&self, kill: bool) -> bool {
        let (systemctl, session, unit) = (&self.systemctl, &self.session, self.unit.as_str());
        let run = |args: Vec<&str>| {
            let mut command = tokio::process::Command::new(systemctl);
            command
                .args(args)
                .env_clear()
                .envs(session)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .kill_on_drop(true);
            async move {
                tokio::time::timeout(CLEANUP_STEP, command.output())
                    .await
                    .ok()
                    .and_then(Result::ok)
            }
        };
        // A scope already gone fails to be killed; what counts is that systemd
        // then says it is no longer running. Only its own word for that counts:
        // a query that fails (the service manager unreachable, say) or answers
        // anything else can't confirm it.
        if kill {
            let _ = run(vec!["--user", "kill", "--signal=SIGKILL", unit]).await;
        }
        run(vec!["--user", "is-active", unit])
            .await
            .is_some_and(|out| {
                !out.status.success()
                    && matches!(
                        String::from_utf8_lossy(&out.stdout).trim(),
                        "inactive" | "failed"
                    )
            })
    }
}
/// A unit name systemd accepts, unique to this launch.
fn unit(stem: &str) -> String {
    let stem: String = stem
        .chars()
        .take(64)
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!(
        "app-ophio-{stem}-{}.scope",
        protocol::CommandId::new().to_string().replace('-', "")
    )
}
/// Variables the session's apps need from Øphio's own environment, when
/// the session manager doesn't hold them.
const SESSION: &[&str] = &[
    "HOME",
    "PATH",
    "XDG_RUNTIME_DIR",
    "WAYLAND_DISPLAY",
    "DISPLAY",
    "DBUS_SESSION_BUS_ADDRESS",
    "XDG_CURRENT_DESKTOP",
    "XDG_SESSION_TYPE",
    "HYPRLAND_INSTANCE_SIGNATURE",
    "SWAYSOCK",
    "XDG_DATA_HOME",
    "XDG_DATA_DIRS",
    "XDG_CONFIG_HOME",
    "XDG_CONFIG_DIRS",
];
/// The environment the session's own launcher gives apps: the user service
/// manager's, which the desktop session fills in. Øphio's service settings
/// stay out of it. The display comes from Øphio's environment when the
/// manager lacks it.
async fn session_environment(environment: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    let mut session: BTreeMap<String, String> = manager_environment()
        .await
        .unwrap_or_default()
        .into_iter()
        .filter_map(|line| {
            let (k, v) = line.split_once('=')?;
            Some((k.to_owned(), v.to_owned()))
        })
        .collect();
    for key in SESSION {
        if let Some(value) = environment.get(*key) {
            match *key {
                // The session's own search path and lists win.
                "PATH" | "XDG_DATA_DIRS" | "XDG_CONFIG_DIRS" => {
                    session
                        .entry((*key).into())
                        .or_insert_with(|| value.clone());
                }
                _ => {
                    session.insert((*key).into(), value.clone());
                }
            }
        }
    }
    session
}
#[cfg(target_os = "linux")]
async fn manager_environment() -> Option<Vec<String>> {
    tokio::time::timeout(SESSION_LOOKUP, async {
        let bus = zbus::Connection::session().await.ok()?;
        let proxy = zbus::Proxy::new(
            &bus,
            "org.freedesktop.systemd1",
            "/org/freedesktop/systemd1",
            "org.freedesktop.systemd1.Manager",
        )
        .await
        .ok()?;
        proxy.get_property::<Vec<String>>("Environment").await.ok()
    })
    .await
    .ok()
    .flatten()
}
#[cfg(not(target_os = "linux"))]
async fn manager_environment() -> Option<Vec<String>> {
    None
}
/// What the program bound to Super+Return runs, as words, on Hyprland: the
/// terminal people there open with that shortcut.
async fn hyprland_terminal(environment: &BTreeMap<String, String>) -> Option<Vec<String>> {
    let socket = environment.get("HYPRLAND_INSTANCE_SIGNATURE")?;
    let hyprctl = apps::program_path(environment, "hyprctl")?;
    let output = tokio::time::timeout(
        Duration::from_secs(2),
        tokio::process::Command::new(hyprctl)
            .args(["-i", socket, "-j", "binds"])
            .env_clear()
            .envs(environment)
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .ok()?
    .ok()?;
    if !output.status.success() || output.stdout.len() > 4 * 1024 * 1024 {
        return None;
    }
    hyprland_bind(&String::from_utf8(output.stdout).ok()?)
}
pub(crate) fn hyprland_bind(binds: &str) -> Option<Vec<String>> {
    const SUPER: u64 = 64;
    let binds: serde_json::Value = serde_json::from_str(binds).ok()?;
    binds.as_array()?.iter().find_map(|b| {
        (b["modmask"].as_u64() == Some(SUPER)
            && b["key"]
                .as_str()
                .is_some_and(|k| k.eq_ignore_ascii_case("return"))
            && b["dispatcher"].as_str() == Some("exec"))
        .then(|| {
            b["arg"]
                .as_str()
                .unwrap_or("")
                .split_whitespace()
                .take(32)
                .map(|w| w.rsplit('/').next().unwrap_or(w).to_lowercase())
                .collect()
        })
    })
}
/// One way the desktop names its preferred terminal.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Pick {
    /// A desktop entry id.
    Id(String),
    /// A program name, such as `$TERMINAL` or KDE's TerminalApplication.
    Program(String),
    /// The words of a command line; the first that is a terminal's program.
    Command(Vec<String>),
}
#[derive(Debug, Default)]
pub(crate) struct Preferences {
    /// mimeapps.list `[Default Applications]`, then `[Added Associations]`,
    /// in lookup order.
    pub defaults: Vec<BTreeMap<String, Vec<String>>>,
    pub added: Vec<BTreeMap<String, Vec<String>>>,
    /// Ways the desktop names its terminal, strongest first.
    pub terminal: Vec<Pick>,
    /// The desktop's names, lowercase: `hyprland`, `gnome`, `kde`.
    pub desktops: Vec<String>,
}
fn home(environment: &BTreeMap<String, String>, key: &str, fallback: &str) -> Option<PathBuf> {
    environment
        .get(key)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| {
            environment
                .get("HOME")
                .map(|h| PathBuf::from(h).join(fallback))
        })
}
fn dirs(environment: &BTreeMap<String, String>, key: &str, fallback: &str) -> Vec<PathBuf> {
    environment
        .get(key)
        .map(String::as_str)
        .unwrap_or(fallback)
        .split(':')
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .collect()
}
/// Groups of an ini-style file: group → key → value.
fn ini(path: &Path) -> BTreeMap<String, BTreeMap<String, String>> {
    let mut groups: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    let Ok(text) = std::fs::read_to_string(path) else {
        return groups;
    };
    if text.len() > 1024 * 1024 {
        return groups;
    }
    let mut group = String::new();
    for line in text.lines().map(str::trim) {
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            group = name.into();
        } else if !line.starts_with('#')
            && let Some((k, v)) = line.split_once('=')
        {
            groups
                .entry(group.clone())
                .or_default()
                .insert(k.trim().into(), v.trim().into());
        }
    }
    groups
}
fn ids(value: &str) -> Vec<String> {
    value
        .split(';')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}
impl Preferences {
    pub(crate) fn read(environment: &BTreeMap<String, String>) -> Self {
        let desktops: Vec<String> = environment
            .get("XDG_CURRENT_DESKTOP")
            .map(String::as_str)
            .unwrap_or("")
            .split(':')
            .filter(|d| !d.is_empty())
            .map(str::to_lowercase)
            .collect();
        let config: Vec<PathBuf> = home(environment, "XDG_CONFIG_HOME", ".config")
            .into_iter()
            .chain(dirs(environment, "XDG_CONFIG_DIRS", "/etc/xdg"))
            .collect();
        let data: Vec<PathBuf> = home(environment, "XDG_DATA_HOME", ".local/share")
            .into_iter()
            .chain(dirs(
                environment,
                "XDG_DATA_DIRS",
                "/usr/local/share:/usr/share",
            ))
            .map(|d| d.join("applications"))
            .collect();
        let named = |dir: &Path, file: &str| -> Vec<PathBuf> {
            desktops
                .iter()
                .map(|d| dir.join(format!("{d}-{file}")))
                .chain(std::iter::once(dir.join(file)))
                .collect()
        };
        let mut preferences = Self {
            desktops: desktops.clone(),
            ..Self::default()
        };
        for path in config
            .iter()
            .chain(data.iter())
            .flat_map(|dir| named(dir, "mimeapps.list"))
        {
            let groups = ini(&path);
            let read = |group: &str| -> BTreeMap<String, Vec<String>> {
                groups
                    .get(group)
                    .map(|g| g.iter().map(|(k, v)| (k.clone(), ids(v))).collect())
                    .unwrap_or_default()
            };
            preferences.defaults.push(read("Default Applications"));
            preferences.added.push(read("Added Associations"));
        }
        // xdg-terminal-exec's own preference lists.
        for path in config
            .iter()
            .flat_map(|dir| named(dir, "xdg-terminals.list"))
        {
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            for line in text.lines().take(256).map(str::trim) {
                if line.is_empty() || line.starts_with('#') || line.starts_with('-') {
                    continue;
                }
                let id = line.trim_start_matches('+');
                let id = id.split(':').next().unwrap_or(id);
                preferences.terminal.push(Pick::Id(id.into()));
            }
        }
        if let Some(home) = config.first() {
            let kde = ini(&home.join("kdeglobals"));
            if let Some(general) = kde.get("General") {
                if let Some(id) = general.get("TerminalService") {
                    preferences.terminal.push(Pick::Id(id.clone()));
                }
                if let Some(program) = general.get("TerminalApplication") {
                    preferences.terminal.push(Pick::Command(
                        program.split_whitespace().map(str::to_lowercase).collect(),
                    ));
                }
            }
            if let Some(program) = ini(&home.join("xfce4/helpers.rc"))
                .get("")
                .and_then(|g| g.get("TerminalEmulator"))
            {
                preferences
                    .terminal
                    .push(Pick::Program(program.to_lowercase()));
            }
        }
        if let Some(program) = environment
            .get("TERMINAL")
            .and_then(|t| t.split_whitespace().next())
        {
            preferences.terminal.push(Pick::Program(
                program.rsplit('/').next().unwrap_or(program).to_lowercase(),
            ));
        }
        preferences
    }
    fn mime(&self, mime: &str, desktops: &[Desktop]) -> Option<usize> {
        let installed = |id: &str| desktops.iter().position(|d| d.id == id);
        self.defaults
            .iter()
            .chain(self.added.iter())
            .filter_map(|group| group.get(mime))
            .flatten()
            .find_map(|id| installed(id))
            .or_else(|| {
                desktops
                    .iter()
                    .position(|d| d.mime_types.iter().any(|m| m == mime))
            })
    }
}
/// Terminals people know by name, in the order a default is chosen when
/// the desktop names none.
const TERMINALS: &[&str] = &[
    "alacritty",
    "kitty",
    "foot",
    "wezterm",
    "ghostty",
    "ptyxis",
    "kgx",
    "gnome-terminal",
    "konsole",
    "xfce4-terminal",
    "tilix",
    "terminator",
    "xterm",
];
const BROWSERS: &[&str] = &[
    "firefox",
    "chromium",
    "google-chrome",
    "brave-browser",
    "microsoft-edge",
    "vivaldi",
    "epiphany",
];
/// A desktop entry's id without vendor prefix or suffix: `org.telegram.
/// desktop.desktop` is `telegram`.
fn stem(id: &str) -> String {
    let id = id.strip_suffix(".desktop").unwrap_or(id);
    let parts: Vec<&str> = id.split('.').collect();
    parts
        .iter()
        .rev()
        .find(|p| !matches!(p.to_lowercase().as_str(), "desktop" | "app" | "application"))
        .unwrap_or(&id)
        .to_lowercase()
}
fn roles(d: &Desktop) -> Vec<AppRole> {
    let has = |c: &str| d.categories.iter().any(|x| x == c);
    let mut roles = Vec::new();
    let mut add = |yes: bool, role| {
        if yes {
            roles.push(role);
        }
    };
    add(has("WebBrowser"), AppRole::Browser);
    add(has("TerminalEmulator"), AppRole::Terminal);
    add(has("FileManager"), AppRole::FileManager);
    add(has("TextEditor"), AppRole::TextEditor);
    add(has("Email"), AppRole::Email);
    add(has("Calculator"), AppRole::Calculator);
    add(
        has("DesktopSettings") || (has("Settings") && d.name.to_lowercase().contains("settings")),
        AppRole::Settings,
    );
    add(
        has("Music") || (has("Audio") && has("Player")),
        AppRole::Music,
    );
    add(has("Video") && has("Player"), AppRole::Video);
    add(has("Viewer") && has("Graphics"), AppRole::Images);
    add(
        d.mime_types.iter().any(|m| m == "application/pdf") && has("Viewer"),
        AppRole::Documents,
    );
    add(has("Calendar"), AppRole::Calendar);
    roles
}
/// The desktop's default for each role, as an index into `desktops`.
fn defaults(desktops: &[Desktop], preferences: &Preferences) -> BTreeMap<AppRole, usize> {
    let mut chosen = BTreeMap::new();
    let mimes: &[(AppRole, &[&str])] = &[
        (
            AppRole::Browser,
            &[
                "x-scheme-handler/https",
                "x-scheme-handler/http",
                "text/html",
            ],
        ),
        (AppRole::FileManager, &["inode/directory"]),
        (AppRole::TextEditor, &["text/plain"]),
        (AppRole::Email, &["x-scheme-handler/mailto"]),
        (AppRole::Music, &["audio/mpeg", "audio/flac"]),
        (AppRole::Video, &["video/mp4", "video/x-matroska"]),
        (AppRole::Images, &["image/png", "image/jpeg"]),
        (AppRole::Documents, &["application/pdf"]),
        (AppRole::Calendar, &["text/calendar"]),
    ];
    for (role, types) in mimes {
        if let Some(i) = types.iter().find_map(|m| preferences.mime(m, desktops)) {
            chosen.insert(*role, i);
        }
    }
    let kind = |role: AppRole| -> Vec<usize> {
        (0..desktops.len())
            .filter(|i| roles(&desktops[*i]).contains(&role))
            .collect()
    };
    let program = |i: usize| {
        desktops[i]
            .program
            .clone()
            .unwrap_or_default()
            .to_lowercase()
    };
    let terminals = kind(AppRole::Terminal);
    let by_pick = |pick: &Pick| -> Option<usize> {
        match pick {
            Pick::Id(id) => desktops.iter().position(|d| &d.id == id),
            Pick::Program(name) => terminals
                .iter()
                .copied()
                .find(|i| program(*i) == *name || stem(&desktops[*i].id) == *name),
            Pick::Command(words) => words.iter().find_map(|w| {
                terminals
                    .iter()
                    .copied()
                    .find(|i| program(*i) == *w || stem(&desktops[*i].id) == *w)
            }),
        }
    };
    let desktop_terminals: &[&str] = if preferences.desktops.iter().any(|d| d == "gnome") {
        &[
            "org.gnome.Ptyxis.desktop",
            "org.gnome.Console.desktop",
            "org.gnome.Terminal.desktop",
        ]
    } else if preferences.desktops.iter().any(|d| d == "kde") {
        &["org.kde.konsole.desktop"]
    } else if preferences.desktops.iter().any(|d| d == "xfce") {
        &["xfce4-terminal.desktop"]
    } else {
        &[]
    };
    let terminal = preferences
        .terminal
        .iter()
        .find_map(by_pick)
        .or_else(|| {
            desktop_terminals
                .iter()
                .find_map(|id| desktops.iter().position(|d| d.id == *id))
        })
        .or_else(|| {
            TERMINALS
                .iter()
                .find_map(|name| by_pick(&Pick::Program((*name).into())))
        })
        .or_else(|| terminals.first().copied());
    if let Some(i) = terminal {
        chosen.insert(AppRole::Terminal, i);
    }
    // Without a MIME default, a well-known browser, then any of each kind.
    let browsers = kind(AppRole::Browser);
    let browser = BROWSERS
        .iter()
        .find_map(|name| browsers.iter().copied().find(|i| program(*i) == *name))
        .or_else(|| browsers.first().copied());
    let file_manager = kind(AppRole::FileManager).first().copied();
    for (role, fallback) in [
        (AppRole::Browser, browser),
        (AppRole::FileManager, file_manager),
    ] {
        if let Some(i) = fallback {
            chosen.entry(role).or_insert(i);
        }
    }
    chosen
}
/// Where a desktop entry comes from, when that tells two apps of one name
/// apart.
pub(crate) fn source(path: &Path) -> Option<String> {
    let path = path.to_string_lossy();
    if path.contains("/flatpak/exports/") {
        Some("Flatpak".into())
    } else if path.contains("/snapd/") {
        Some("Snap".into())
    } else if path.contains("/.local/share/") {
        Some("in your home folder".into())
    } else {
        None
    }
}
pub(crate) fn catalog(
    desktops: Vec<Desktop>,
    preferences: &Preferences,
) -> Vec<(Desktop, LaunchApp)> {
    let defaults = defaults(&desktops, preferences);
    desktops
        .into_iter()
        .enumerate()
        .map(|(i, d)| {
            let mut aliases: Vec<String> = d.generic_name.iter().cloned().collect();
            aliases.extend(d.keywords.iter().cloned());
            aliases.extend(d.program.iter().cloned());
            aliases.push(stem(&d.id));
            aliases.retain(|a| !a.is_empty());
            aliases.dedup();
            let app = LaunchApp {
                id: d.id.clone(),
                name: d.name.clone(),
                aliases,
                roles: roles(&d),
                defaults: defaults
                    .iter()
                    .filter(|(_, at)| **at == i)
                    .map(|(role, _)| *role)
                    .collect(),
                listed: d.listed,
                detail: source(&d.path),
            };
            (d, app)
        })
        .collect()
}

#[cfg(test)]
mod tests;
