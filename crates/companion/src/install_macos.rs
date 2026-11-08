// SPDX-License-Identifier: Apache-2.0
//! The login service on a Mac: a launchd agent in the person's own session, so
//! it can reach their screen. Started only by explicit commands.
use super::{POLL, Started, WAIT, running, started};
use crate::{
    Error, Result,
    config::{APP_NAME, Paths},
    local::{OwnerClient, OwnerRequest, OwnerResponse},
};
use std::{
    path::{Path, PathBuf},
    time::Instant,
};

pub(crate) const LABEL: &str = "app.ophio";

/// launchd gives its agents only the system's own folders, and people's tools
/// are in Homebrew's. An agent's own folder is found separately.
const SERVICE_PATH: &str = "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin";

fn header() -> String {
    format!("<!-- Managed by {APP_NAME}; remove with the service uninstall command. -->")
}

fn xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// What launchd reads: start at login and at once, and again after a failure
/// but not after being asked to stop, in the graphical session.
pub(crate) fn agent_text(executable: &Path) -> Result<String> {
    let path = executable
        .to_str()
        .ok_or_else(|| Error::Config("service executable must be valid UTF-8".into()))?;
    if path.chars().any(char::is_control) {
        return Err(Error::Config("invalid executable path".into()));
    }
    let path = xml(path);
    let header = header();
    Ok(format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
{header}
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>{LABEL}</string>
	<key>ProgramArguments</key>
	<array>
		<string>{path}</string>
		<string>run</string>
	</array>
	<key>RunAtLoad</key>
	<true/>
	<key>KeepAlive</key>
	<dict>
		<key>SuccessfulExit</key>
		<false/>
	</dict>
	<key>ThrottleInterval</key>
	<integer>3</integer>
	<key>Umask</key>
	<integer>63</integer>
	<key>ProcessType</key>
	<string>Interactive</string>
	<key>LimitLoadToSessionType</key>
	<string>Aqua</string>
	<key>EnvironmentVariables</key>
	<dict>
		<key>PATH</key>
		<string>{SERVICE_PATH}</string>
		<key>LANG</key>
		<string>en_US.UTF-8</string>
	</dict>
</dict>
</plist>
"#
    ))
}

fn agents() -> Result<PathBuf> {
    let base = directories::BaseDirs::new()
        .ok_or_else(|| Error::Config("home directory unavailable".into()))?;
    Ok(base.home_dir().join("Library/LaunchAgents"))
}

fn agent_path() -> Result<PathBuf> {
    Ok(agents()?.join(format!("{LABEL}.plist")))
}

/// Whether a file exists and is the one this program wrote: a regular file,
/// not a link or directory, with its header.
fn managed(path: &Path) -> Result<Option<bool>> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => Ok(Some(
            metadata.is_file() && std::fs::read_to_string(path)?.contains(&header()),
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn domain() -> String {
    format!("gui/{}", rustix::process::getuid().as_raw())
}

fn service_name() -> String {
    format!("{}/{LABEL}", domain())
}

async fn launchctl(args: &[&str]) -> std::io::Result<std::process::Output> {
    tokio::process::Command::new("/bin/launchctl")
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .await
}

/// Whether launchd holds the agent in this session.
async fn loaded() -> Result<bool> {
    Ok(launchctl(&["print", &service_name()])
        .await?
        .status
        .success())
}

fn failed(what: &str, output: &std::process::Output) -> Error {
    let said = String::from_utf8_lossy(&output.stderr);
    let said = said.trim();
    Error::Ended(format!("launchctl could not {what}: {said}"))
}

/// Has launchd start the agent. It starts at once, as it would at login.
async fn load(path: &Path) -> Result<()> {
    let path = path
        .to_str()
        .ok_or_else(|| Error::Config("service path must be valid UTF-8".into()))?;
    let output = launchctl(&["bootstrap", &domain(), path]).await?;
    if output.status.success() {
        return Ok(());
    }
    // There's no graphical session to start it in until someone logs in.
    if output.status.code() == Some(125) {
        return Err(Error::Ended(
            "No one is logged in to this Mac's desktop. Log in, or turn on automatic login, and try again.".into(),
        ));
    }
    Err(failed("start Øphio", &output))
}

/// Has launchd stop the agent and forget it until the next login or load, and
/// waits until its process has let go of the data directory.
async fn unload(paths: &Paths) -> Result<()> {
    let output = launchctl(&["bootout", &service_name()]).await?;
    // Already gone, in launchd's words: no such process or service.
    if !output.status.success() && loaded().await? {
        return Err(failed("stop Øphio", &output));
    }
    let deadline = Instant::now() + WAIT;
    loop {
        match paths.lock() {
            Ok(_) => return Ok(()),
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

async fn write_agent(path: &Path, text: &str) -> Result<()> {
    use std::io::Write;
    let directory = path
        .parent()
        .ok_or_else(|| Error::Config("service path has no folder".into()))?;
    std::fs::create_dir_all(directory)?;
    let mut temp = tempfile::NamedTempFile::new_in(directory)?;
    temp.write_all(text.as_bytes())?;
    temp.as_file().sync_all()?;
    // launchd ignores an agent others could write to, and this one is the
    // person's to read like any other in the folder.
    use std::os::unix::fs::PermissionsExt;
    temp.as_file()
        .set_permissions(std::fs::Permissions::from_mode(0o644))?;
    temp.persist(path).map_err(|e| e.error)?;
    Ok(())
}

pub(super) async fn service(command: super::ServiceCommand, paths: &Paths) -> Result<String> {
    use super::ServiceCommand::{Install, Status, Uninstall};
    let path = agent_path()?;
    match command {
        Install => {
            install(paths, &path).await?;
            match started(paths, None).await? {
                Started::Running | Started::Ended(_) => Ok(
                    "Øphio is running in the background now, and will start whenever you log in."
                        .into(),
                ),
                Started::Starting => Ok("Øphio is still starting in the background, and will start whenever you log in. `ophio service status` shows when it is ready.".into()),
            }
        }
        Uninstall => {
            if managed(&path)? == Some(false) {
                return Err(Error::Config(
                    "refusing to remove a login agent Øphio did not write".into(),
                ));
            }
            if loaded().await? {
                unload(paths).await?;
            }
            stop_now(paths).await?;
            if managed(&path)?.is_some() {
                std::fs::remove_file(&path)?;
            }
            Ok("Øphio is stopped and no longer starts when you log in. Your settings, paired devices and data are kept; `ophio uninstall-data` deletes them.".into())
        }
        Status => {
            let now = if running(paths).await? {
                "It is running now."
            } else {
                "It is not running now."
            };
            Ok(match managed(&path)? {
                None => format!("Øphio does not start when you log in. {now}"),
                Some(true) => format!("Øphio starts whenever you log in. {now}"),
                Some(false) => {
                    format!(
                        "A login agent with this name exists, but Øphio did not write it. {now}"
                    )
                }
            })
        }
    }
}

/// Writes the agent for this program and has launchd start it, in place of any
/// Øphio running already, which would hold the data directory.
async fn install(paths: &Paths, path: &Path) -> Result<()> {
    if managed(path)? == Some(false) {
        return Err(Error::Config(
            "refusing to replace a login agent Øphio did not write".into(),
        ));
    }
    let text = agent_text(&crate::config::running_program()?)?;
    if loaded().await? {
        unload(paths).await?;
    } else {
        stop_now(paths).await?;
    }
    write_agent(path, &text).await?;
    load(path).await
}

/// Starts Øphio now, as the login service would where there is one.
pub(super) async fn start_now(paths: &Paths) -> Result<()> {
    if running(paths).await? {
        return Ok(());
    }
    let path = agent_path()?;
    let mut child = if managed(&path)? == Some(true) {
        if loaded().await? {
            let output = launchctl(&["kickstart", &service_name()]).await?;
            if !output.status.success() {
                return Err(failed("start Øphio", &output));
            }
        } else {
            load(&path).await?;
        }
        None
    } else {
        Some(super::detached(&crate::config::running_program()?)?)
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

/// Stops a running Øphio, however it was started, and waits until its process
/// has ended. False when none was running.
pub(super) async fn stop_now(paths: &Paths) -> Result<bool> {
    // launchd would otherwise be left holding an agent that isn't running, and
    // the next start would have to tell the two apart.
    if managed(&agent_path()?)? == Some(true) && loaded().await? && running(paths).await? {
        unload(paths).await?;
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

/// Starts a one-off job in this session that launchd keeps apart from whatever
/// started it, for an update that must outlive the login service and this
/// terminal. `file` is where its description is kept. A finished one from before
/// is taken away first. Whether it started.
pub(crate) async fn run_job(
    label: &str,
    file: &Path,
    program: &Path,
    args: &[String],
) -> Result<bool> {
    let name = format!("{}/{label}", domain());
    let _ = launchctl(&["bootout", &name]).await?;
    let program = program
        .to_str()
        .ok_or_else(|| Error::Config("update program must be valid UTF-8".into()))?;
    let arguments: String = std::iter::once(program)
        .chain(args.iter().map(String::as_str))
        .map(|argument| format!("\t\t<string>{}</string>\n", xml(argument)))
        .collect();
    let text = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>{}</string>
	<key>ProgramArguments</key>
	<array>
{arguments}	</array>
	<key>RunAtLoad</key>
	<true/>
	<key>Umask</key>
	<integer>63</integer>
	<key>AbandonProcessGroup</key>
	<true/>
</dict>
</plist>
"#,
        xml(label),
    );
    write_agent(file, &text).await?;
    let file = file
        .to_str()
        .ok_or_else(|| Error::Config("update path must be valid UTF-8".into()))?;
    Ok(launchctl(&["bootstrap", &domain(), file])
        .await?
        .status
        .success())
}

/// After an update: the agent as this version writes it, if it runs this copy.
/// Another copy's agent is left alone, and what to say about it is returned.
pub(super) async fn refresh(executable: &Path) -> Result<Option<String>> {
    let path = agent_path()?;
    if managed(&path)? != Some(true) {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&path)?;
    let wanted = agent_text(executable)?;
    if text == wanted {
        return Ok(None);
    }
    let program = |agent: &str| {
        agent
            .lines()
            .skip_while(|line| !line.contains("<key>ProgramArguments</key>"))
            .nth(2)
            .map(|line| line.trim().to_owned())
    };
    if program(&text) != program(&wanted) {
        return Ok(Some(
            "The login agent runs another copy of Øphio, so it was left alone. `ophio service install` from this one makes it run this install.".into(),
        ));
    }
    write_agent(&path, &wanted).await?;
    Ok(Some("Updated the login agent.".into()))
}

/// Whether Øphio starts whenever this user logs in.
pub(super) fn starts_at_login() -> Result<bool> {
    Ok(managed(&agent_path()?)? == Some(true))
}

/// Setup's `service install`: Øphio starts now and at every login, and this
/// waits until it answers. What it says afterwards, if anything.
pub(super) async fn login_service(paths: &Paths) -> Result<Option<String>> {
    install(paths, &agent_path()?).await?;
    match started(paths, None).await? {
        Started::Running | Started::Ended(_) => Ok(None),
        Started::Starting => Ok(Some(
            "Øphio is still starting in the background. `ophio service status` shows when it is ready.".into(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_agent_starts_this_program_in_the_graphical_session() {
        let text = agent_text(Path::new("/Users/sam/.local/bin/ophio")).unwrap();
        assert!(text.contains("<string>/Users/sam/.local/bin/ophio</string>"));
        assert!(text.contains("<string>run</string>"));
        assert!(text.contains("<string>Aqua</string>"));
        assert!(text.contains(&format!("<string>{LABEL}</string>")));
        assert!(text.contains(&header()));
    }

    #[test]
    fn launchd_itself_accepts_the_agent() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("agent.plist");
        let text = agent_text(Path::new("/Applications/A & <B>/ophio")).unwrap();
        std::fs::write(&path, &text).unwrap();
        let checked = std::process::Command::new("/usr/bin/plutil")
            .arg("-lint")
            .arg(&path)
            .output()
            .unwrap();
        assert!(
            checked.status.success(),
            "{}",
            String::from_utf8_lossy(&checked.stdout)
        );
        assert!(text.contains("A &amp; &lt;B&gt;"));
    }

    #[test]
    fn a_path_that_could_change_the_agent_is_refused() {
        assert!(agent_text(Path::new("/tmp/a\nb")).is_err());
    }

    #[test]
    fn only_the_agent_this_program_wrote_counts_as_its_own() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("agent.plist");
        assert_eq!(managed(&path).unwrap(), None);
        std::fs::write(&path, "someone else's").unwrap();
        assert_eq!(managed(&path).unwrap(), Some(false));
        std::fs::write(&path, agent_text(Path::new("/bin/ophio")).unwrap()).unwrap();
        assert_eq!(managed(&path).unwrap(), Some(true));
        let link = directory.path().join("link.plist");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert_eq!(managed(&link).unwrap(), Some(false));
    }
}
