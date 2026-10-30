// SPDX-License-Identifier: Apache-2.0
//! Service operations are invoked only by explicit CLI subcommands.
#[cfg(any(target_os = "linux", windows))]
use crate::local::{OwnerClient, OwnerRequest, OwnerResponse};
use crate::{Error, Result, config::APP_NAME};
use ports::PRODUCT_NAME;
use std::path::Path;
#[cfg(any(target_os = "linux", windows))]
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnitTarget {
    GraphicalSession,
    Default,
}
impl UnitTarget {
    pub fn from_graphical_session(active: Option<bool>) -> Self {
        match active {
            Some(true) => Self::GraphicalSession,
            Some(false) | None => Self::Default,
        }
    }

    pub fn note(self) -> Option<&'static str> {
        match self {
            Self::GraphicalSession => None,
            Self::Default => Some(
                "Desktop viewing and control will be unavailable when Øphio starts this way; start ophio run from the desktop's own autostart for desktop access.",
            ),
        }
    }
}

pub fn unit_text(executable: &Path, target: UnitTarget) -> Result<String> {
    let path = executable
        .to_str()
        .ok_or_else(|| Error::Config("service executable must be valid UTF-8".into()))?;
    if path.chars().any(|c| c.is_control()) {
        return Err(Error::Config("invalid executable path".into()));
    }
    // systemd expands percent specifiers and environment variables in ExecStart.
    let path = path
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('%', "%%")
        .replace('$', "$$");
    let (ordering, wanted_by) = match target {
        UnitTarget::GraphicalSession => (
            "After=graphical-session.target\nPartOf=graphical-session.target\n",
            "graphical-session.target",
        ),
        UnitTarget::Default => ("", "default.target"),
    };
    Ok(format!(
        "# Managed by {APP_NAME}; remove with the service uninstall command.\n[Unit]\nDescription={PRODUCT_NAME}\n{ordering}\n[Service]\nType=simple\nExecStart=\"{path}\" run\nRestart=on-failure\nRestartSec=3\nUMask=0077\nDelegate=yes\n\n[Install]\nWantedBy={wanted_by}\n"
    ))
}

#[derive(Debug, Clone, Copy, clap::Subcommand)]
pub enum ServiceCommand {
    /// Start Øphio now and whenever you log in.
    ///
    /// On Linux, this installs a systemd user service and starts it now. An
    /// existing Øphio service, or the earlier companion program's, is replaced
    /// and restarted. On Windows, this registers Øphio to start when you log
    /// in and, unless it is running already, starts it now; either way it runs
    /// in the background, without a window.
    Install,
    /// Stop starting Øphio at login, and stop it.
    ///
    /// On Linux, this stops the service, and the earlier companion program's.
    /// On Windows, this stops Øphio if it is running. Settings, paired devices
    /// and data are kept; `ophio uninstall-data` deletes them.
    Uninstall,
    /// Show the state of the login service.
    Status,
}

pub async fn service(command: ServiceCommand) -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        if let Some(note) = linux_service(
            command,
            &units()?,
            &std::env::current_exe()?,
            &UserSystemctl,
        )
        .await?
        {
            println!("{note}");
        }
        Ok(())
    }
    #[cfg(windows)]
    {
        println!(
            "{}",
            windows::service(command, &crate::config::Paths::discover()?).await?
        );
        Ok(())
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        let _ = command;
        Err(Error::Unsupported(
            "startup registration is not supported on this platform",
        ))
    }
}

/// Setup's way to start Øphio now, as the login service would where there is
/// one, and to wait until it answers.
pub(crate) async fn start_now(paths: &crate::config::Paths) -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        if running(paths).await? {
            return Ok(());
        }
        let mut child = if managed(&unit_path()?, APP_NAME)? == Some(true) {
            systemctl(&UserSystemctl, &["start", &unit_name()]).await?;
            None
        } else {
            Some(detached(&std::env::current_exe()?)?)
        };
        match started(paths, child.as_mut()).await? {
            Started::Running => Ok(()),
            Started::Starting => Err(Error::Ended(
                "Øphio is still starting. Try again in a moment; `ophio status` says when it is running.".into(),
            )),
            Started::Ended(status) => Err(Error::Ended(format!(
                "Øphio stopped as it started ({status}). `ophio run` shows what stopped it."
            ))),
        }
    }
    #[cfg(windows)]
    {
        windows::start_now(paths, &std::env::current_exe()?).await
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        let _ = paths;
        Err(Error::Unsupported(
            "starting in the background is not supported on this platform",
        ))
    }
}

/// Setup's way to stop a running Øphio, however it was started, and to wait
/// until its process has ended. False when none was running.
pub(crate) async fn stop_now(paths: &crate::config::Paths) -> Result<bool> {
    #[cfg(target_os = "linux")]
    {
        // The service would otherwise be started again by what stops it.
        if managed(&unit_path()?, APP_NAME)? == Some(true)
            && UserSystemctl.run(&["is-active", &unit_name()]).await?
        {
            systemctl(&UserSystemctl, &["stop", &unit_name()]).await?;
            return Ok(true);
        }
        let mut client = match OwnerClient::connect(&paths.socket()).await {
            Ok(client) => client,
            Err(Error::NotRunning) => return Ok(false),
            Err(error) => return Err(error),
        };
        let OwnerResponse::Quitting = client.request(&OwnerRequest::Quit).await? else {
            return Err(Error::Closed);
        };
        drop(client);
        // Its instance lock is let go only when its process ends.
        let deadline = Instant::now() + WAIT;
        loop {
            match paths.lock() {
                Ok(_) => return Ok(true),
                Err(Error::Running) if Instant::now() < deadline => {
                    tokio::time::sleep(POLL).await;
                }
                Err(Error::Running) => {
                    return Err(Error::Ended(
                        "Øphio is still stopping. Try again in a moment.".into(),
                    ));
                }
                Err(error) => return Err(error),
            }
        }
    }
    #[cfg(windows)]
    {
        windows::stop(paths).await
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        let _ = paths;
        Ok(false)
    }
}

/// Whether Øphio starts whenever this user logs in.
pub(crate) async fn starts_at_login() -> Result<bool> {
    #[cfg(target_os = "linux")]
    {
        Ok(managed(&unit_path()?, APP_NAME)? == Some(true)
            && UserSystemctl
                .run(&["is-enabled", "--quiet", &unit_name()])
                .await?)
    }
    #[cfg(windows)]
    {
        Ok(platform_windows::startup::status()? == platform_windows::startup::Status::Registered)
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        Ok(false)
    }
}

/// Setup's `service install`: Øphio starts now and at every login, and this
/// waits until it answers. What it says afterwards, if anything.
pub(crate) async fn login_service(paths: &crate::config::Paths) -> Result<Option<String>> {
    #[cfg(target_os = "linux")]
    {
        // An Øphio started some other way holds the data directory, so the
        // service would fail to start, and fail again, until it stopped.
        let unit = managed(&unit_path()?, APP_NAME)? == Some(true)
            && UserSystemctl.run(&["is-active", &unit_name()]).await?;
        if !unit {
            stop_now(paths).await?;
        }
        let note = linux_service(
            ServiceCommand::Install,
            &units()?,
            &std::env::current_exe()?,
            &UserSystemctl,
        )
        .await?;
        match started(paths, None).await? {
            Started::Running | Started::Ended(_) => Ok(note.map(String::from)),
            Started::Starting => Ok(Some(
                "Øphio is still starting in the background. `ophio service status` shows when it is ready.".into(),
            )),
        }
    }
    #[cfg(windows)]
    {
        windows::service(ServiceCommand::Install, paths)
            .await
            .map(Some)
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        let _ = paths;
        Err(Error::Unsupported(
            "startup registration is not supported on this platform",
        ))
    }
}

/// How long setup and `service install` wait for an Øphio they started to
/// answer, and for a running one to finish stopping.
#[cfg(any(target_os = "linux", windows))]
const WAIT: Duration = Duration::from_secs(30);
#[cfg(any(target_os = "linux", windows))]
const POLL: Duration = Duration::from_millis(200);

/// Whether this user's Øphio answers on its owner socket or pipe.
#[cfg(any(target_os = "linux", windows))]
async fn running(paths: &crate::config::Paths) -> Result<bool> {
    match OwnerClient::connect(&paths.socket()).await {
        Ok(_) => Ok(true),
        Err(Error::NotRunning) => Ok(false),
        Err(error) => Err(error),
    }
}

#[cfg(any(target_os = "linux", windows))]
enum Started {
    Running,
    /// Not answering yet after [`WAIT`].
    Starting,
    Ended(std::process::ExitStatus),
}

/// Waits for an Øphio just started to answer, so that a failure to start is
/// reported rather than lost. `child` is its process, when this started it.
#[cfg(any(target_os = "linux", windows))]
async fn started(
    paths: &crate::config::Paths,
    mut child: Option<&mut std::process::Child>,
) -> Result<Started> {
    let deadline = Instant::now() + WAIT;
    loop {
        if running(paths).await? {
            return Ok(Started::Running);
        }
        if let Some(child) = child.as_deref_mut()
            && let Some(status) = child.try_wait()?
        {
            return Ok(Started::Ended(status));
        }
        if Instant::now() >= deadline {
            return Ok(Started::Starting);
        }
        tokio::time::sleep(POLL).await;
    }
}

/// Starts `executable run` in a process group of its own, with no input or
/// output, so that it carries on after the terminal that started it closes.
#[cfg(target_os = "linux")]
pub(crate) fn detached(executable: &Path) -> std::io::Result<std::process::Child> {
    use std::os::unix::process::CommandExt;
    let mut command = std::process::Command::new(executable);
    command
        .arg("run")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .process_group(0);
    // Not the folder setup was started from, which it would keep in use.
    if let Some(base) = directories::BaseDirs::new() {
        command.current_dir(base.home_dir());
    }
    command.spawn()
}

#[cfg(target_os = "linux")]
fn units() -> Result<std::path::PathBuf> {
    let base = directories::BaseDirs::new()
        .ok_or_else(|| Error::Config("home directory unavailable".into()))?;
    Ok(base.config_dir().join("systemd/user"))
}
#[cfg(target_os = "linux")]
fn unit_name() -> String {
    format!("{APP_NAME}.service")
}
#[cfg(target_os = "linux")]
fn unit_path() -> Result<std::path::PathBuf> {
    Ok(units()?.join(unit_name()))
}

/// Starts `executable run` with no console, apart from this process's console
/// and, where the job it runs in allows, from that job, so that closing the
/// terminal or program that started it does not end Øphio.
#[cfg(windows)]
pub(crate) fn start_in_background(executable: &Path) -> std::io::Result<std::process::Child> {
    use std::os::windows::process::CommandExt;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
    // Øphio outlives this command, so it must not inherit the command's output.
    platform_windows::background::keep_std_handles();
    let start = |flags: u32| {
        let mut command = std::process::Command::new(executable);
        command
            .arg("run")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | flags);
        // Not the folder it was started from, which it would keep in use.
        if let Some(folder) = executable.parent() {
            command.current_dir(folder);
        }
        command.spawn()
    };
    start(CREATE_BREAKAWAY_FROM_JOB).or_else(|_| start(0))
}

/// Logon startup on Windows is a per-user Run entry, never a Session-0
/// service; `run` started by it goes on without a window.
#[cfg(windows)]
mod windows {
    use super::*;
    use crate::config::Paths;
    use platform_windows::{background, startup};

    const KEPT: &str =
        "Settings, paired devices and data are kept; `.\\ophio.exe uninstall-data` deletes them.";

    pub(super) async fn service(command: ServiceCommand, paths: &Paths) -> Result<String> {
        match command {
            ServiceCommand::Install => {
                let executable = std::env::current_exe()?;
                startup::install(&executable)?;
                start(paths, &executable).await
            }
            ServiceCommand::Uninstall => {
                startup::uninstall()?;
                Ok(if stop(paths).await? {
                    format!("Øphio has stopped, and will no longer start when you log in. {KEPT}")
                } else {
                    format!("Øphio will no longer start when you log in. {KEPT}")
                })
            }
            ServiceCommand::Status => {
                let now = if running(paths).await? {
                    "It is running now."
                } else {
                    "It is not running now."
                };
                Ok(match startup::status()? {
                    startup::Status::Absent => {
                        format!("Øphio does not start when you log in. {now}")
                    }
                    startup::Status::Registered => format!(
                        "Øphio starts whenever you log in, unless that is turned off in Settings › Apps › Startup. {now}"
                    ),
                    startup::Status::Unmanaged => format!(
                        "A startup entry with this name exists, but Øphio did not create it. {now}"
                    ),
                })
            }
        }
    }

    /// Starts Øphio unless it is running already, and waits for it to answer,
    /// so that a failure to start is reported here rather than lost.
    async fn start(paths: &Paths, executable: &Path) -> Result<String> {
        if running(paths).await? {
            return Ok("Øphio is already running, and will also start whenever you log in.".into());
        }
        // Øphio and the agents it starts would run as administrator too.
        if background::elevated()? {
            return Ok("Øphio will start the next time you log in. It was not started now because this terminal runs as administrator; `.\\ophio.exe run` in an ordinary terminal starts it now.".into());
        }
        let mut child = start_in_background(executable)?;
        match started(paths, Some(&mut child)).await? {
            Started::Running => Ok(
                "Øphio is running in the background now, and will start whenever you log in."
                    .into(),
            ),
            Started::Ended(status) => Err(Error::Ended(format!(
                "Øphio stopped as it started ({status}). It will still try to start whenever you log in; `.\\ophio.exe run` shows what stopped it."
            ))),
            Started::Starting => Ok("Øphio is still starting in the background, and will start whenever you log in. `.\\ophio.exe service status` shows when it is ready.".into()),
        }
    }

    /// Starts Øphio in the background unless it is running already, and waits
    /// for it to answer, for setup.
    pub(super) async fn start_now(paths: &Paths, executable: &Path) -> Result<()> {
        if running(paths).await? {
            return Ok(());
        }
        if background::elevated()? {
            return Err(Error::Ended("Øphio was not started because this terminal runs as administrator, and the agents it starts would too. Run `.\\ophio.exe setup` in an ordinary terminal instead.".into()));
        }
        let mut child = start_in_background(executable)?;
        match started(paths, Some(&mut child)).await? {
            Started::Running => Ok(()),
            Started::Ended(status) => Err(Error::Ended(format!(
                "Øphio stopped as it started ({status}). `.\\ophio.exe run` shows what stopped it."
            ))),
            Started::Starting => Err(Error::Ended("Øphio is still starting. Try again in a moment; `.\\ophio.exe status` says when it is running.".into())),
        }
    }

    /// Asks a running Øphio to stop and waits until its process has ended,
    /// so that its files can be replaced. False when none was running.
    pub(super) async fn stop(paths: &Paths) -> Result<bool> {
        let mut client = match OwnerClient::connect(&paths.socket()).await {
            Ok(client) => client,
            Err(Error::NotRunning) => return Ok(false),
            Err(error) => return Err(error),
        };
        let process = background::Process::open(client.server())?;
        let OwnerResponse::Quitting = client.request(&OwnerRequest::Quit).await? else {
            return Err(Error::Closed);
        };
        drop(client);
        let deadline = Instant::now() + WAIT;
        while !process.ended() {
            if Instant::now() >= deadline {
                return Err(Error::Ended(
                    "Øphio is still stopping. Wait until it has before replacing its files, or end ophio.exe in Task Manager.".into(),
                ));
            }
            tokio::time::sleep(POLL).await;
        }
        Ok(true)
    }
}
#[cfg(target_os = "linux")]
trait Systemctl {
    async fn run(&self, args: &[&str]) -> std::io::Result<bool>;
}

#[cfg(target_os = "linux")]
struct UserSystemctl;
#[cfg(target_os = "linux")]
impl Systemctl for UserSystemctl {
    async fn run(&self, args: &[&str]) -> std::io::Result<bool> {
        let mut command = tokio::process::Command::new("systemctl");
        command.arg("--user").args(args);
        if args.first() == Some(&"is-active") {
            command
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
        }
        Ok(command.status().await?.success())
    }
}

#[cfg(target_os = "linux")]
async fn systemctl(systemctl: &impl Systemctl, args: &[&str]) -> Result<()> {
    if !systemctl.run(args).await? {
        return Err(Error::Config(
            "systemctl did not complete successfully".into(),
        ));
    }
    Ok(())
}

/// After an update, rewrites the login service's unit as this version writes
/// it, if it is one Øphio wrote for `executable`, and reloads it. A unit for
/// another copy of Øphio is left alone; what to say about that, if anything.
#[cfg(target_os = "linux")]
pub(crate) async fn refresh_unit(executable: &Path) -> Result<Option<String>> {
    let path = unit_path()?;
    if managed(&path, APP_NAME)? != Some(true) {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&path)?;
    let target = if text.contains("WantedBy=graphical-session.target") {
        UnitTarget::GraphicalSession
    } else {
        UnitTarget::Default
    };
    let wanted = unit_text(executable, target)?;
    if text == wanted {
        return Ok(None);
    }
    let runs = |unit: &str| {
        unit.lines()
            .find_map(|line| line.strip_prefix("ExecStart="))
            .map(str::to_owned)
    };
    if runs(&text) != runs(&wanted) {
        return Ok(Some(format!(
            "The login service runs another copy of Øphio ({}), so it was left alone. `ophio service install` from this one makes it run this install.",
            runs(&text).unwrap_or_default()
        )));
    }
    use std::io::Write;
    let directory = units()?;
    let mut temp = tempfile::NamedTempFile::new_in(&directory)?;
    temp.write_all(wanted.as_bytes())?;
    temp.as_file().sync_all()?;
    temp.persist(&path).map_err(|e| e.error)?;
    systemctl(&UserSystemctl, &["daemon-reload"]).await?;
    Ok(Some("Updated the login service.".into()))
}

/// Whether a unit file exists and is one `program` wrote: a regular file, not
/// a link or directory, that starts with that program's managed-unit header.
#[cfg(target_os = "linux")]
fn managed(path: &Path, program: &str) -> Result<Option<bool>> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => Ok(Some(
            metadata.is_file()
                && std::fs::read_to_string(path)?.starts_with(&format!("# Managed by {program};")),
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

#[cfg(target_os = "linux")]
async fn linux_service(
    command: ServiceCommand,
    directory: &Path,
    executable: &Path,
    manager: &impl Systemctl,
) -> Result<Option<&'static str>> {
    let unit_name = format!("{APP_NAME}.service");
    let unit_path = directory.join(&unit_name);
    // An install from before the rename has its own managed unit, which this
    // program's replaces. Anyone else's unit of that name is left alone.
    use crate::config::EARLIER_APP_NAME;
    let earlier_name = format!("{EARLIER_APP_NAME}.service");
    let earlier_path = directory.join(&earlier_name);
    let earlier = managed(&earlier_path, EARLIER_APP_NAME)? == Some(true);
    match command {
        ServiceCommand::Install => {
            std::fs::create_dir_all(directory)?;
            let replacing = match managed(&unit_path, APP_NAME)? {
                Some(false) => {
                    return Err(Error::Config(
                        "refusing to replace an unmanaged service unit".into(),
                    ));
                }
                found => found.is_some(),
            };
            let target = UnitTarget::from_graphical_session(
                manager
                    .run(&["is-active", "graphical-session.target"])
                    .await
                    .ok(),
            );
            let text = unit_text(executable, target)?;
            // The unit contains no secrets. Do not change permissions on the
            // shared systemd/user directory or any other user configuration.
            use std::io::Write;
            let mut temp = tempfile::NamedTempFile::new_in(directory)?;
            temp.write_all(text.as_bytes())?;
            temp.as_file().sync_all()?;
            if earlier {
                // Stop the earlier program before this one starts on its data.
                systemctl(manager, &["disable", "--now", &earlier_name]).await?;
                std::fs::remove_file(&earlier_path)?;
            }
            if replacing {
                // Remove old WantedBy links before replacing their definition,
                // and restart so the running process also gets the new settings.
                systemctl(manager, &["disable", "--now", &unit_name]).await?;
            }
            temp.persist(&unit_path).map_err(|e| e.error)?;
            systemctl(manager, &["daemon-reload"]).await?;
            systemctl(manager, &["enable", "--now", &unit_name]).await?;
            Ok(target.note())
        }
        ServiceCommand::Uninstall => {
            let mut units = Vec::new();
            match managed(&unit_path, APP_NAME)? {
                Some(false) => {
                    return Err(Error::Config(
                        "refusing to remove an unmanaged service unit".into(),
                    ));
                }
                Some(true) => units.push((&unit_name, &unit_path)),
                None => {}
            }
            if earlier {
                units.push((&earlier_name, &earlier_path));
            }
            if units.is_empty() {
                // Nothing to undo, as after an install that used the desktop's
                // own autostart instead: the documented uninstall goes on.
                return Ok(Some(
                    "Øphio does not start at login through a service; there is nothing to remove.",
                ));
            }
            for (name, path) in units {
                systemctl(manager, &["disable", "--now", name]).await?;
                std::fs::remove_file(path)?;
            }
            systemctl(manager, &["daemon-reload"]).await?;
            Ok(None)
        }
        ServiceCommand::Status => {
            systemctl(manager, &["status", "--no-pager", &unit_name]).await?;
            Ok(None)
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_unit_text_for_both_targets() {
        let executable = Path::new("/opt/has spaces/50%$cash\\\"bin");
        assert_eq!(
            unit_text(executable, UnitTarget::GraphicalSession).unwrap(),
            concat!(
                "# Managed by ophio; remove with the service uninstall command.\n",
                "[Unit]\nDescription=Øphio\n",
                "After=graphical-session.target\nPartOf=graphical-session.target\n\n",
                "[Service]\nType=simple\nExecStart=\"/opt/has spaces/50%%$$cash\\\\\\\"bin\" run\n",
                "Restart=on-failure\nRestartSec=3\nUMask=0077\nDelegate=yes\n\n",
                "[Install]\nWantedBy=graphical-session.target\n",
            )
        );
        assert_eq!(
            unit_text(executable, UnitTarget::Default).unwrap(),
            concat!(
                "# Managed by ophio; remove with the service uninstall command.\n",
                "[Unit]\nDescription=Øphio\n\n",
                "[Service]\nType=simple\nExecStart=\"/opt/has spaces/50%%$$cash\\\\\\\"bin\" run\n",
                "Restart=on-failure\nRestartSec=3\nUMask=0077\nDelegate=yes\n\n",
                "[Install]\nWantedBy=default.target\n",
            )
        );
    }

    #[test]
    fn target_requires_a_confirmed_active_graphical_session() {
        let target = UnitTarget::from_graphical_session(Some(true));
        assert_eq!(target, UnitTarget::GraphicalSession);
        assert_eq!(target.note(), None);
        for active in [Some(false), None] {
            let target = UnitTarget::from_graphical_session(active);
            assert_eq!(target, UnitTarget::Default);
            assert_eq!(
                target.note(),
                Some(
                    "Desktop viewing and control will be unavailable when Øphio starts this way; start ophio run from the desktop's own autostart for desktop access."
                )
            );
        }
    }
    #[test]
    fn unit_escapes_special_characters() {
        let text = unit_text(
            Path::new("/tmp/has spaces/50%$cash\"bin"),
            UnitTarget::Default,
        )
        .unwrap();
        assert!(text.contains("ExecStart=\"/tmp/has spaces/50%%$$cash\\\"bin\" run"));
        assert!(unit_text(Path::new("/tmp/line\nbreak"), UnitTarget::Default).is_err());
    }

    #[cfg(target_os = "linux")]
    mod linux {
        use super::*;
        use std::{cell::RefCell, path::PathBuf};

        struct FakeSystemctl {
            active: Option<bool>,
            fail_on: Option<&'static str>,
            unit: PathBuf,
            calls: RefCell<Vec<Vec<String>>>,
            units: RefCell<Vec<Option<String>>>,
        }
        impl FakeSystemctl {
            fn new(directory: &Path, active: Option<bool>) -> Self {
                Self {
                    active,
                    fail_on: None,
                    unit: directory.join("ophio.service"),
                    calls: RefCell::new(Vec::new()),
                    units: RefCell::new(Vec::new()),
                }
            }
        }
        impl Systemctl for FakeSystemctl {
            async fn run(&self, args: &[&str]) -> std::io::Result<bool> {
                self.calls
                    .borrow_mut()
                    .push(args.iter().map(|s| (*s).into()).collect());
                self.units
                    .borrow_mut()
                    .push(std::fs::read_to_string(&self.unit).ok());
                if args[0] == "is-active" {
                    self.active
                        .ok_or_else(|| std::io::Error::other("manager unavailable"))
                } else {
                    Ok(self.fail_on != Some(args[0]))
                }
            }
        }

        const OLD_UNIT: &str = "# Managed by ophio; remove with the service uninstall command.\n[Unit]\nDescription=Ophio\nAfter=network-online.target\n\n[Service]\nType=simple\nExecStart=\"/opt/ophio\" run\nRestart=on-failure\nRestartSec=3\nUMask=0077\nNoNewPrivileges=yes\n\n[Install]\nWantedBy=default.target\n";

        #[tokio::test]
        async fn reinstall_disables_old_links_before_replacing_and_enabling() {
            for active in [Some(true), Some(false), None] {
                let directory = tempfile::tempdir().unwrap();
                let manager = FakeSystemctl::new(directory.path(), active);
                std::fs::write(&manager.unit, OLD_UNIT).unwrap();
                let executable = Path::new("/opt/new ophio");
                let note = linux_service(
                    ServiceCommand::Install,
                    directory.path(),
                    executable,
                    &manager,
                )
                .await
                .unwrap();
                let target = UnitTarget::from_graphical_session(active);
                let expected = unit_text(executable, target).unwrap();
                assert_eq!(note, target.note());
                assert_eq!(std::fs::read_to_string(&manager.unit).unwrap(), expected);
                assert_eq!(
                    *manager.calls.borrow(),
                    vec![
                        vec!["is-active", "graphical-session.target"],
                        vec!["disable", "--now", "ophio.service"],
                        vec!["daemon-reload"],
                        vec!["enable", "--now", "ophio.service"],
                    ]
                );
                assert_eq!(
                    *manager.units.borrow(),
                    vec![
                        Some(OLD_UNIT.into()),
                        Some(OLD_UNIT.into()),
                        Some(expected.clone()),
                        Some(expected),
                    ]
                );
            }
        }

        #[tokio::test]
        async fn fresh_install_and_uninstall_use_the_managed_unit() {
            let directory = tempfile::tempdir().unwrap();
            let manager = FakeSystemctl::new(directory.path(), Some(true));
            let executable = Path::new("/opt/ophio");
            linux_service(
                ServiceCommand::Install,
                directory.path(),
                executable,
                &manager,
            )
            .await
            .unwrap();
            linux_service(
                ServiceCommand::Status,
                directory.path(),
                executable,
                &manager,
            )
            .await
            .unwrap();
            linux_service(
                ServiceCommand::Uninstall,
                directory.path(),
                executable,
                &manager,
            )
            .await
            .unwrap();
            assert!(!manager.unit.exists());
            assert_eq!(
                *manager.calls.borrow(),
                vec![
                    vec!["is-active", "graphical-session.target"],
                    vec!["daemon-reload"],
                    vec!["enable", "--now", "ophio.service"],
                    vec!["status", "--no-pager", "ophio.service"],
                    vec!["disable", "--now", "ophio.service"],
                    vec!["daemon-reload"],
                ]
            );
        }

        #[tokio::test]
        async fn unmanaged_files_directories_and_symlinks_are_preserved() {
            for kind in ["file", "directory", "symlink"] {
                let directory = tempfile::tempdir().unwrap();
                let manager = FakeSystemctl::new(directory.path(), Some(true));
                let linked = directory.path().join("linked");
                match kind {
                    "file" => std::fs::write(&manager.unit, "# Owner's service\n").unwrap(),
                    "directory" => std::fs::create_dir(&manager.unit).unwrap(),
                    _ => {
                        std::fs::write(&linked, OLD_UNIT).unwrap();
                        std::os::unix::fs::symlink(&linked, &manager.unit).unwrap();
                    }
                }
                for command in [ServiceCommand::Install, ServiceCommand::Uninstall] {
                    assert!(
                        linux_service(command, directory.path(), Path::new("/opt/ophio"), &manager)
                            .await
                            .is_err()
                    );
                    assert!(manager.unit.exists());
                    assert!(manager.calls.borrow().is_empty());
                }
                if kind == "file" {
                    assert_eq!(
                        std::fs::read_to_string(&manager.unit).unwrap(),
                        "# Owner's service\n"
                    );
                } else if kind == "symlink" {
                    assert!(manager.unit.is_symlink());
                    assert_eq!(std::fs::read_to_string(linked).unwrap(), OLD_UNIT);
                }
            }
        }

        #[tokio::test]
        async fn command_failure_stops_reinstallation() {
            for (fail_on, call_count) in [("disable", 2), ("daemon-reload", 3), ("enable", 4)] {
                let directory = tempfile::tempdir().unwrap();
                let mut manager = FakeSystemctl::new(directory.path(), Some(true));
                manager.fail_on = Some(fail_on);
                std::fs::write(&manager.unit, OLD_UNIT).unwrap();
                assert!(
                    linux_service(
                        ServiceCommand::Install,
                        directory.path(),
                        Path::new("/opt/ophio"),
                        &manager
                    )
                    .await
                    .is_err()
                );
                assert_eq!(manager.calls.borrow().len(), call_count);
                if fail_on == "disable" {
                    assert_eq!(std::fs::read_to_string(&manager.unit).unwrap(), OLD_UNIT);
                }
                assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
            }
        }

        const EARLIER_UNIT: &str = "# Managed by companion; remove with the service uninstall command.\n[Unit]\nDescription=Host companion\nAfter=graphical-session.target\nPartOf=graphical-session.target\n\n[Service]\nType=simple\nExecStart=\"/home/sam/.local/opt/companion/bin/companion\" run\nRestart=on-failure\nRestartSec=3\nUMask=0077\n\n[Install]\nWantedBy=graphical-session.target\n";

        #[tokio::test]
        async fn install_stops_and_replaces_the_earlier_managed_unit() {
            let directory = tempfile::tempdir().unwrap();
            let manager = FakeSystemctl::new(directory.path(), Some(true));
            let earlier = directory.path().join("companion.service");
            std::fs::write(&earlier, EARLIER_UNIT).unwrap();
            let executable = Path::new("/home/sam/.local/opt/ophio/bin/ophio");
            linux_service(
                ServiceCommand::Install,
                directory.path(),
                executable,
                &manager,
            )
            .await
            .unwrap();
            assert!(!earlier.exists());
            let expected = unit_text(executable, UnitTarget::GraphicalSession).unwrap();
            assert_eq!(std::fs::read_to_string(&manager.unit).unwrap(), expected);
            assert_eq!(
                *manager.calls.borrow(),
                vec![
                    vec!["is-active", "graphical-session.target"],
                    vec!["disable", "--now", "companion.service"],
                    vec!["daemon-reload"],
                    vec!["enable", "--now", "ophio.service"],
                ]
            );
            // The new unit is written only after the earlier one was stopped.
            assert_eq!(
                *manager.units.borrow(),
                vec![None, None, Some(expected.clone()), Some(expected)]
            );
        }

        #[tokio::test]
        async fn uninstall_removes_both_managed_units() {
            for current in [true, false] {
                let directory = tempfile::tempdir().unwrap();
                let manager = FakeSystemctl::new(directory.path(), Some(true));
                let earlier = directory.path().join("companion.service");
                std::fs::write(&earlier, EARLIER_UNIT).unwrap();
                if current {
                    std::fs::write(&manager.unit, OLD_UNIT).unwrap();
                }
                linux_service(
                    ServiceCommand::Uninstall,
                    directory.path(),
                    Path::new("/opt/ophio"),
                    &manager,
                )
                .await
                .unwrap();
                assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
                let mut expected = vec![vec!["disable", "--now", "companion.service"]];
                if current {
                    expected.insert(0, vec!["disable", "--now", "ophio.service"]);
                }
                expected.push(vec!["daemon-reload"]);
                assert_eq!(*manager.calls.borrow(), expected);
            }
        }

        #[tokio::test]
        async fn uninstall_without_a_unit_succeeds_and_says_so() {
            let directory = tempfile::tempdir().unwrap();
            let manager = FakeSystemctl::new(directory.path(), Some(true));
            let note = linux_service(
                ServiceCommand::Uninstall,
                directory.path(),
                Path::new("/opt/ophio"),
                &manager,
            )
            .await
            .unwrap();
            assert!(note.unwrap().contains("nothing to remove"));
            assert!(manager.calls.borrow().is_empty());
        }

        #[tokio::test]
        async fn an_unmanaged_earlier_unit_is_left_alone() {
            for kind in ["file", "symlink"] {
                let directory = tempfile::tempdir().unwrap();
                let manager = FakeSystemctl::new(directory.path(), Some(true));
                let earlier = directory.path().join("companion.service");
                let linked = directory.path().join("linked");
                if kind == "file" {
                    std::fs::write(&earlier, "# Someone else's companion\n").unwrap();
                } else {
                    std::fs::write(&linked, EARLIER_UNIT).unwrap();
                    std::os::unix::fs::symlink(&linked, &earlier).unwrap();
                }
                for command in [ServiceCommand::Install, ServiceCommand::Uninstall] {
                    linux_service(command, directory.path(), Path::new("/opt/ophio"), &manager)
                        .await
                        .unwrap();
                }
                assert!(
                    manager
                        .calls
                        .borrow()
                        .iter()
                        .flatten()
                        .all(|arg| arg != "companion.service")
                );
                if kind == "file" {
                    assert_eq!(
                        std::fs::read_to_string(&earlier).unwrap(),
                        "# Someone else's companion\n"
                    );
                } else {
                    assert!(earlier.is_symlink());
                    assert_eq!(std::fs::read_to_string(&linked).unwrap(), EARLIER_UNIT);
                }
            }
        }
    }
}
