// SPDX-License-Identifier: Apache-2.0
use super::*;
use voice_control::{
    Launcher,
    apps::{Resolution, resolve},
};
/// Authority that allows, or that has ended.
struct Allow;
impl Authority for Allow {
    fn check(&self) -> Result<(), Error> {
        Ok(())
    }
}
struct Ended;
impl Authority for Ended {
    fn check(&self) -> Result<(), Error> {
        Err(Error::Stopped)
    }
}

fn entry(dir: &Path, file: &str, body: &str) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(
        dir.join(file),
        format!("[Desktop Entry]\nType=Application\n{body}\n"),
    )
    .unwrap();
}
/// A desktop like a typical one: two browsers, two terminals, GNOME Files,
/// Telegram and a hidden URL handler.
struct Fixture {
    _root: tempfile::TempDir,
    environment: BTreeMap<String, String>,
    roots: Vec<PathBuf>,
    config: PathBuf,
}
fn fixture() -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let data = root.path().join("share/applications");
    entry(
        &data,
        "chromium.desktop",
        "Name=Chromium\nGenericName=Web Browser\nExec=/usr/bin/chromium %U\nCategories=Network;WebBrowser;\nMimeType=text/html;x-scheme-handler/http;x-scheme-handler/https;",
    );
    entry(
        &data,
        "firefox.desktop",
        "Name=Firefox\nGenericName=Web Browser\nExec=firefox %u\nCategories=Network;WebBrowser;\nMimeType=text/html;x-scheme-handler/https;",
    );
    entry(
        &data,
        "Alacritty.desktop",
        "Name=Alacritty\nGenericName=Terminal\nExec=alacritty\nCategories=System;TerminalEmulator;",
    );
    entry(
        &data,
        "kitty.desktop",
        "Name=kitty\nGenericName=Terminal emulator\nExec=kitty\nCategories=System;TerminalEmulator;",
    );
    entry(
        &data,
        "org.gnome.Nautilus.desktop",
        "Name=Files\nKeywords=folder;manager;explore;disk;filesystem;nautilus;\nExec=nautilus --new-window %U\nCategories=GNOME;GTK;Utility;Core;FileManager;\nMimeType=inode/directory;",
    );
    entry(
        &data,
        "org.telegram.desktop.desktop",
        "Name=Telegram Desktop\nExec=env QT_QPA_PLATFORM=wayland telegram-desktop -- %u\nCategories=Chat;Network;",
    );
    entry(
        &data,
        "code-oss.desktop",
        "Name=Code - OSS\nKeywords=vscode;\nExec=/usr/bin/code-oss %F\nCategories=Utility;TextEditor;Development;IDE;",
    );
    entry(
        &data,
        "code-url-handler.desktop",
        "Name=Code - URL Handler\nExec=/usr/bin/code-oss --open-url %U\nNoDisplay=true\nMimeType=x-scheme-handler/vscode;",
    );
    let config = home.join(".config");
    std::fs::create_dir_all(&config).unwrap();
    let environment = BTreeMap::from([
        ("HOME".to_owned(), home.to_string_lossy().into_owned()),
        (
            "XDG_CONFIG_DIRS".to_owned(),
            root.path().join("etc").to_string_lossy().into_owned(),
        ),
        (
            "XDG_DATA_DIRS".to_owned(),
            root.path().join("share").to_string_lossy().into_owned(),
        ),
        ("XDG_CURRENT_DESKTOP".to_owned(), "Hyprland".to_owned()),
    ]);
    Fixture {
        roots: apps::roots(&environment),
        _root: root,
        environment,
        config,
    }
}
impl Fixture {
    fn catalog(&self, terminal: Option<Vec<String>>) -> Vec<LaunchApp> {
        let desktops = apps::desktops(&self.roots, &self.environment).unwrap();
        let mut preferences = Preferences::read(&self.environment);
        if let Some(words) = terminal {
            preferences.terminal.insert(0, Pick::Command(words));
        }
        catalog(desktops, &preferences)
            .into_iter()
            .map(|(_, app)| app)
            .collect()
    }
}
fn one(phrase: &str, apps: &[LaunchApp]) -> String {
    match resolve(phrase, apps) {
        Resolution::One(app) => app.name,
        other => panic!("{phrase}: {other:?}"),
    }
}
#[test]
fn the_browser_is_the_https_default_and_falls_back_to_a_known_one() {
    let f = fixture();
    // No mimeapps.list default: the first entry that handles https.
    assert_eq!(one("the browser", &f.catalog(None)), "Chromium");
    std::fs::write(
        f.config.join("mimeapps.list"),
        "[Default Applications]\nx-scheme-handler/https=missing.desktop;firefox.desktop;\n",
    )
    .unwrap();
    let apps = f.catalog(None);
    assert_eq!(one("browser", &apps), "Firefox");
    assert_eq!(one("web browser", &apps), "Firefox");
    // Naming one opens that one, whatever the default.
    assert_eq!(one("chromium", &apps), "Chromium");
}
#[test]
fn the_terminal_is_the_one_the_desktop_names() {
    let f = fixture();
    // Nothing names one: the first of the well-known terminals installed.
    assert_eq!(one("a terminal", &f.catalog(None)), "Alacritty");
    // Hyprland binds Super+Return to a session launcher that runs kitty.
    let bound = ["uwsm", "app", "--", "kitty"].map(String::from).to_vec();
    assert_eq!(one("terminal", &f.catalog(Some(bound.clone()))), "kitty");
    // xdg-terminal-exec's list comes after the shortcut a person uses.
    std::fs::write(
        f.config.join("hyprland-xdg-terminals.list"),
        "# preferred\n-kitty.desktop\nAlacritty.desktop\n",
    )
    .unwrap();
    assert_eq!(one("the terminal", &f.catalog(None)), "Alacritty");
    assert_eq!(one("terminal", &f.catalog(Some(bound))), "kitty");
    let mut f = fixture();
    f.environment
        .insert("TERMINAL".into(), "/usr/bin/kitty".into());
    assert_eq!(one("console", &f.catalog(None)), "kitty");
    // KDE names it in kdeglobals.
    let f = fixture();
    std::fs::write(
        f.config.join("kdeglobals"),
        "[General]\nTerminalApplication=kitty\n",
    )
    .unwrap();
    assert_eq!(one("command line", &f.catalog(None)), "kitty");
}
#[test]
fn apps_answer_to_their_names_ids_programs_and_keywords() {
    let f = fixture();
    let apps = f.catalog(None);
    assert_eq!(one("telegram", &apps), "Telegram Desktop");
    assert_eq!(one("Telegram Desktop", &apps), "Telegram Desktop");
    assert_eq!(one("files", &apps), "Files");
    assert_eq!(one("the file manager", &apps), "Files");
    assert_eq!(one("nautilus", &apps), "Files");
    assert_eq!(one("vs code", &apps), "Code - OSS");
    assert_eq!(one("code", &apps), "Code - OSS");
    assert_eq!(one("alacrity", &apps), "Alacritty");
    // Hidden handlers never open by name.
    assert!(matches!(
        resolve("code url handler", &apps),
        Resolution::None(_)
    ));
    assert!(matches!(resolve("blender", &apps), Resolution::None(_)));
    assert!(
        matches!(resolve("calculator", &apps), Resolution::None(r) if r.contains("calculator"))
    );
    let telegram = apps.iter().find(|a| a.name == "Telegram Desktop").unwrap();
    assert!(telegram.aliases.contains(&"telegram-desktop".into()));
}
#[test]
fn super_return_reads_from_hyprland_binds() {
    let binds = r#"[{"modmask":64,"key":"B","dispatcher":"exec","arg":"uwsm app -- chromium"},
        {"modmask":65,"key":"Return","dispatcher":"exec","arg":"other"},
        {"modmask":64,"key":"RETURN","dispatcher":"exec","arg":"uwsm app -- /usr/bin/alacritty --working-directory ~"}]"#;
    assert_eq!(
        hyprland_bind(binds).unwrap(),
        ["uwsm", "app", "--", "alacritty", "--working-directory", "~"]
    );
    assert!(hyprland_bind("[]").is_none());
    assert!(hyprland_bind("not json").is_none());
}
/// A stand-in `gio` and `systemd-run` record what they were asked; nothing
/// opens on the screen.
#[cfg(unix)]
#[tokio::test]
async fn an_app_opens_through_gio_in_its_own_scope_and_only_by_its_listed_id() {
    use std::os::unix::fs::PermissionsExt;
    let f = fixture();
    let bin = f._root.path().join("bin");
    let runtime = f._root.path().join("run");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::create_dir_all(runtime.join("systemd")).unwrap();
    std::fs::write(runtime.join("systemd/private"), "").unwrap();
    let log = f._root.path().join("log");
    for tool in ["gio", "systemd-run"] {
        let path = bin.join(tool);
        std::fs::write(
            &path,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"{tool} $*\" >> '{}'\n[ \"{tool}\" = systemd-run ] || exit 0\nwhile [ \"$1\" != -- ]; do shift; done; shift; exec \"$@\"\n",
                log.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let mut environment = f.environment.clone();
    environment.insert("PATH".into(), bin.to_string_lossy().into_owned());
    environment.insert(
        "XDG_RUNTIME_DIR".into(),
        runtime.to_string_lossy().into_owned(),
    );
    let launcher = LinuxLauncher::new(environment, f.roots.clone());
    let apps = launcher.apps().await.unwrap();
    let Resolution::One(app) = resolve("the terminal", &apps) else {
        panic!()
    };
    launcher
        .launch(
            &VoiceApp {
                id: app.id.clone(),
                name: app.name.clone(),
            },
            &Allow,
        )
        .await
        .unwrap();
    launcher
        .open_url("https://example.com/a?b=c", &Allow)
        .await
        .unwrap();
    let lines: Vec<String> = std::fs::read_to_string(&log)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect();
    assert_eq!(lines.len(), 4, "{lines:?}");
    assert!(lines[0].starts_with(
        "systemd-run --user --scope --quiet --collect --slice=app.slice --unit=app-ophio-Alacritty-"
    ));
    assert!(lines[1].starts_with("gio launch ") && lines[1].ends_with("/Alacritty.desktop"));
    assert_eq!(lines[3], "gio open https://example.com/a?b=c");
    // An id is only ever a listed entry's; a renamed one is refused.
    for wrong in [
        VoiceApp {
            id: "../../bin/sh".into(),
            name: "Alacritty".into(),
        },
        VoiceApp {
            id: app.id.clone(),
            name: "Something else".into(),
        },
    ] {
        assert!(matches!(
            launcher.launch(&wrong, &Allow).await,
            Err(Error::StaleTarget)
        ));
    }
    for url in ["file:///etc/passwd", "javascript:alert(1)", "https://a b"] {
        assert!(launcher.open_url(url, &Allow).await.is_err());
    }
    assert_eq!(std::fs::read_to_string(&log).unwrap().lines().count(), 4);
}
/// The real computer's own app list and defaults, read only. Run by hand on
/// a desktop: `cargo test -p platform-linux -- --ignored real_desktop`.
#[tokio::test]
#[ignore = "reads this computer's own desktop entries"]
async fn real_desktop_resolves_its_browser_terminal_and_apps() {
    let environment: BTreeMap<String, String> = [
        "HOME",
        "PATH",
        "XDG_CURRENT_DESKTOP",
        "XDG_DATA_HOME",
        "XDG_DATA_DIRS",
        "XDG_CONFIG_HOME",
        "XDG_CONFIG_DIRS",
        "HYPRLAND_INSTANCE_SIGNATURE",
        "TERMINAL",
    ]
    .iter()
    .filter_map(|k| std::env::var(k).ok().map(|v| ((*k).to_owned(), v)))
    .collect();
    let launcher = LinuxLauncher::from_environment(&environment);
    let apps = launcher.apps().await.unwrap();
    for phrase in [
        "the browser",
        "a terminal",
        "the file manager",
        "telegram",
        "blender",
    ] {
        eprintln!("{phrase} → {:?}", resolve(phrase, &apps));
    }
    let browser = std::process::Command::new("xdg-settings")
        .args(["get", "default-web-browser"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .unwrap_or_default();
    if !browser.is_empty() {
        let Resolution::One(app) = resolve("the browser", &apps) else {
            panic!("no browser")
        };
        assert_eq!(app.id, browser);
    }
    assert!(matches!(resolve("a terminal", &apps), Resolution::One(_)));
}
/// On a real session: an app opened this way runs in a scope of its own,
/// outside the caller's service, so restarting Øphio never closes it. The
/// "app" is `sleep`, so nothing appears on the screen.
#[cfg(target_os = "linux")]
#[tokio::test]
#[ignore = "starts a short-lived program in this session's systemd scope"]
async fn real_session_opens_an_app_in_its_own_scope() {
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("applications");
    entry(
        &data,
        "ophio-sleepy.desktop",
        "Name=Ophio Sleepy\nExec=sleep 31.4159",
    );
    let mut environment: BTreeMap<String, String> = [
        "HOME",
        "PATH",
        "XDG_RUNTIME_DIR",
        "DBUS_SESSION_BUS_ADDRESS",
    ]
    .iter()
    .filter_map(|k| std::env::var(k).ok().map(|v| ((*k).to_owned(), v)))
    .collect();
    environment.insert(
        "XDG_DATA_HOME".into(),
        root.path().to_string_lossy().into_owned(),
    );
    environment.insert("XDG_DATA_DIRS".into(), "/nonexistent".into());
    let launcher = LinuxLauncher::from_environment(&environment);
    let apps = launcher.apps().await.unwrap();
    let Resolution::One(app) = resolve("ophio sleepy", &apps) else {
        panic!("{apps:?}")
    };
    launcher
        .launch(
            &VoiceApp {
                id: app.id,
                name: app.name,
            },
            &Allow,
        )
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let mut found = None;
    for dir in std::fs::read_dir("/proc").unwrap().flatten() {
        let cmdline = std::fs::read(dir.path().join("cmdline")).unwrap_or_default();
        let words: Vec<&[u8]> = cmdline.split(|b| *b == 0).collect();
        if words.len() == 3 && words[0].ends_with(b"sleep") && words[1] == b"31.4159" {
            found = Some((
                dir.file_name().to_string_lossy().into_owned(),
                std::fs::read_to_string(dir.path().join("cgroup")).unwrap_or_default(),
            ));
        }
    }
    let (pid, cgroup) = found.expect("the program started");
    let _ = std::process::Command::new("kill").arg(&pid).status();
    assert!(
        cgroup.contains("/app.slice/app-ophio-ophio-sleepy-"),
        "{cgroup}"
    );
}
#[test]
fn a_flatpak_and_a_native_app_of_one_name_are_both_offered() {
    let mut f = fixture();
    let flatpak = f._root.path().join("flatpak/exports/share");
    entry(
        &flatpak.join("applications"),
        "org.mozilla.firefox.desktop",
        "Name=Firefox\nExec=/usr/bin/flatpak run org.mozilla.firefox %u\nCategories=Network;WebBrowser;",
    );
    f.roots.push(flatpak.join("applications"));
    let apps = f.catalog(None);
    let Resolution::Several(both) = resolve("firefox", &apps) else {
        panic!("{:?}", resolve("firefox", &apps))
    };
    let labels: Vec<String> = both
        .iter()
        .map(|a| voice_control::apps::label(a, &both))
        .collect();
    assert_eq!(labels, ["Firefox", "Firefox (Flatpak)"]);
    assert_eq!(source(Path::new("/usr/share/applications/x.desktop")), None);
}
/// A launch the desktop doesn't confirm in time is stopped, its helper and
/// scope with it, before it is reported: nothing can open after voice
/// control's stop barrier has let go of the display.
#[cfg(unix)]
#[tokio::test]
async fn a_launch_that_hangs_is_stopped_before_its_outcome_is_reported() {
    use std::os::unix::fs::PermissionsExt;
    let f = fixture();
    let bin = f._root.path().join("bin");
    let runtime = f._root.path().join("run");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::create_dir_all(runtime.join("systemd")).unwrap();
    std::fs::write(runtime.join("systemd/private"), "").unwrap();
    let log = f._root.path().join("log");
    let pid = f._root.path().join("gio.pid");
    let tools = [
        (
            "gio",
            format!("echo $$ > '{}'\nexec sleep 30", pid.display()),
        ),
        (
            "systemd-run",
            format!(
                "printf '%s\\n' \"systemd-run $*\" >> '{}'\nwhile [ \"$1\" != -- ]; do shift; done; shift; exec \"$@\"",
                log.display()
            ),
        ),
        (
            "systemctl",
            // The killed scope is gone afterwards.
            format!(
                "printf '%s\\n' \"systemctl $*\" >> '{}'\n[ \"$2\" = is-active ] && echo inactive && exit 3\nexit 0",
                log.display()
            ),
        ),
    ];
    for (tool, body) in tools {
        let path = bin.join(tool);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let mut environment = f.environment.clone();
    environment.insert("PATH".into(), bin.to_string_lossy().into_owned());
    environment.insert(
        "XDG_RUNTIME_DIR".into(),
        runtime.to_string_lossy().into_owned(),
    );
    let launcher = LinuxLauncher::new(environment, f.roots.clone());
    let apps = launcher.apps().await.unwrap();
    let Resolution::One(app) = resolve("the terminal", &apps) else {
        panic!()
    };
    let outcome = launcher
        .launch(
            &VoiceApp {
                id: app.id,
                name: app.name,
            },
            &Allow,
        )
        .await;
    assert!(matches!(outcome, Err(Error::OutcomeUnknown)), "{outcome:?}");
    let helper: i32 = std::fs::read_to_string(&pid)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let alive = std::fs::read_to_string(format!("/proc/{helper}/stat"))
        .is_ok_and(|stat| !stat.contains(") Z "));
    if alive {
        let _ = std::process::Command::new("kill")
            .arg(helper.to_string())
            .status();
    }
    assert!(!alive, "the helper still runs");
    let lines = std::fs::read_to_string(&log).unwrap();
    let unit = lines
        .lines()
        .next()
        .and_then(|l| l.split(' ').find_map(|w| w.strip_prefix("--unit=")))
        .unwrap()
        .to_owned();
    assert!(
        lines
            .lines()
            .any(|l| l == format!("systemctl --user kill --signal=SIGKILL {unit}")),
        "{lines}"
    );
}
/// Authority is the last thing checked, after the app list and the
/// session's environment are read: once it has ended, nothing starts.
#[cfg(unix)]
#[tokio::test]
async fn nothing_starts_once_authority_has_ended() {
    use std::os::unix::fs::PermissionsExt;
    let f = fixture();
    let bin = f._root.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let log = f._root.path().join("log");
    let gio = bin.join("gio");
    std::fs::write(
        &gio,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"gio $*\" >> '{}'\n",
            log.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&gio, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut environment = f.environment.clone();
    environment.insert("PATH".into(), bin.to_string_lossy().into_owned());
    let launcher = LinuxLauncher::new(environment, f.roots.clone());
    let apps = launcher.apps().await.unwrap();
    let Resolution::One(app) = resolve("the browser", &apps) else {
        panic!()
    };
    let app = VoiceApp {
        id: app.id,
        name: app.name,
    };
    assert!(matches!(
        launcher.launch(&app, &Ended).await,
        Err(Error::Stopped)
    ));
    assert!(matches!(
        launcher.open_url("https://example.com", &Ended).await,
        Err(Error::Stopped)
    ));
    assert!(!log.exists());
    launcher.launch(&app, &Allow).await.unwrap();
    assert_eq!(std::fs::read_to_string(&log).unwrap().lines().count(), 1);
}
/// A stand-in `gio` that starts a descendant (the app it opens) and then
/// hangs; both record their process ids.
#[cfg(unix)]
fn hanging_gio(bin: &Path, root: &Path) -> (PathBuf, PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let (helper, child) = (root.join("gio.pid"), root.join("child.pid"));
    let path = bin.join("gio");
    std::fs::write(
        &path,
        format!(
            "#!/bin/sh\necho $$ > '{}'\nsleep 31 &\necho $! > '{}'\nexec sleep 32\n",
            helper.display(),
            child.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    (helper, child)
}
#[cfg(unix)]
fn running(pid_file: &Path) -> bool {
    let Ok(pid) = std::fs::read_to_string(pid_file) else {
        return false;
    };
    let pid = pid.trim().to_owned();
    let alive = std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .is_ok_and(|stat| !stat.contains(") Z "));
    if alive {
        let _ = std::process::Command::new("kill").arg(&pid).status();
    }
    alive
}
/// Without systemd, the helper runs in a process group of its own, and the
/// whole group is stopped: nothing it started can open the app later.
#[cfg(unix)]
#[tokio::test]
async fn without_systemd_a_hung_launch_is_stopped_with_everything_it_started() {
    let f = fixture();
    let bin = f._root.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let (helper, child) = hanging_gio(&bin, f._root.path());
    let mut environment = f.environment.clone();
    environment.insert("PATH".into(), bin.to_string_lossy().into_owned());
    let launcher = LinuxLauncher::new(environment, f.roots.clone());
    let apps = launcher.apps().await.unwrap();
    let Resolution::One(app) = resolve("the terminal", &apps) else {
        panic!()
    };
    let outcome = launcher
        .launch(
            &VoiceApp {
                id: app.id,
                name: app.name,
            },
            &Allow,
        )
        .await;
    // It can't be confirmed stopped without a scope, but its group is gone.
    assert!(
        matches!(outcome, Err(Error::LaunchUnstopped)),
        "{outcome:?}"
    );
    tokio::time::sleep(Duration::from_millis(100)).await;
    let (h, c) = (running(&helper), running(&child));
    assert!(!h && !c, "helper running: {h}, its descendant running: {c}");
}
/// With systemd, a scope that can't be confirmed stopped is reported as
/// such, never as stopped: voice control's barrier must not claim success.
#[cfg(unix)]
#[tokio::test]
async fn a_scope_that_cannot_be_stopped_is_reported_not_ignored() {
    use std::os::unix::fs::PermissionsExt;
    let f = fixture();
    let bin = f._root.path().join("bin");
    let runtime = f._root.path().join("run");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::create_dir_all(runtime.join("systemd")).unwrap();
    std::fs::write(runtime.join("systemd/private"), "").unwrap();
    let (helper, child) = hanging_gio(&bin, f._root.path());
    for (tool, body) in [
        (
            "systemd-run",
            "while [ \"$1\" != -- ]; do shift; done; shift; exec \"$@\"",
        ),
        // Its kill fails and the scope stays active.
        ("systemctl", "[ \"$2\" = is-active ] && exit 0\nexit 1"),
    ] {
        let path = bin.join(tool);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let mut environment = f.environment.clone();
    environment.insert("PATH".into(), bin.to_string_lossy().into_owned());
    environment.insert(
        "XDG_RUNTIME_DIR".into(),
        runtime.to_string_lossy().into_owned(),
    );
    let launcher = LinuxLauncher::new(environment, f.roots.clone());
    let apps = launcher.apps().await.unwrap();
    let Resolution::One(app) = resolve("the terminal", &apps) else {
        panic!()
    };
    let outcome = launcher
        .launch(
            &VoiceApp {
                id: app.id,
                name: app.name,
            },
            &Allow,
        )
        .await;
    let _ = (running(&helper), running(&child));
    assert!(
        matches!(outcome, Err(Error::LaunchUnstopped)),
        "{outcome:?}"
    );
}
/// A scope whose state can't be read (the service manager is unreachable)
/// is not confirmed stopped: the launch is reported as one that may still
/// open an app.
#[cfg(unix)]
#[tokio::test]
async fn a_scope_whose_state_cannot_be_read_is_not_taken_for_stopped() {
    use std::os::unix::fs::PermissionsExt;
    let f = fixture();
    let bin = f._root.path().join("bin");
    let runtime = f._root.path().join("run");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::create_dir_all(runtime.join("systemd")).unwrap();
    std::fs::write(runtime.join("systemd/private"), "").unwrap();
    let (helper, child) = hanging_gio(&bin, f._root.path());
    for (tool, body) in [
        (
            "systemd-run",
            "while [ \"$1\" != -- ]; do shift; done; shift; exec \"$@\"",
        ),
        (
            "systemctl",
            "echo 'Failed to connect to bus: No such file or directory' >&2\nexit 1",
        ),
    ] {
        let path = bin.join(tool);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let mut environment = f.environment.clone();
    environment.insert("PATH".into(), bin.to_string_lossy().into_owned());
    environment.insert(
        "XDG_RUNTIME_DIR".into(),
        runtime.to_string_lossy().into_owned(),
    );
    let launcher = LinuxLauncher::new(environment, f.roots.clone());
    let apps = launcher.apps().await.unwrap();
    let Resolution::One(app) = resolve("the terminal", &apps) else {
        panic!()
    };
    let outcome = launcher
        .launch(
            &VoiceApp {
                id: app.id,
                name: app.name,
            },
            &Allow,
        )
        .await;
    let _ = (running(&helper), running(&child));
    assert!(
        matches!(outcome, Err(Error::LaunchUnstopped)),
        "{outcome:?}"
    );
}
/// Without a systemd scope Ophio can't see what a hung launch started once
/// it left the process group (a `setsid` child), so it never claims the
/// launch stopped, then or later: it kills the group, reports that an app
/// may still open, and offers no way to check it again.
#[cfg(unix)]
#[tokio::test]
async fn without_a_scope_a_hung_launch_is_never_confirmed_stopped() {
    use std::os::unix::fs::PermissionsExt;
    let f = fixture();
    let bin = f._root.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let escaped = f._root.path().join("escaped.pid");
    let gio = bin.join("gio");
    std::fs::write(
        &gio,
        format!(
            "#!/bin/sh\nsetsid sh -c 'echo $$ > \"{}\"; exec sleep 33' &\nexec sleep 34\n",
            escaped.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&gio, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut environment = f.environment.clone();
    environment.insert("PATH".into(), format!("{}:/usr/bin:/bin", bin.display()));
    let launcher = LinuxLauncher::new(environment, f.roots.clone());
    let apps = launcher.apps().await.unwrap();
    let Resolution::One(app) = resolve("the terminal", &apps) else {
        panic!()
    };
    let reported = Reported::default();
    let outcome = launcher
        .launch(
            &VoiceApp {
                id: app.id,
                name: app.name,
            },
            &reported,
        )
        .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let _ = running(&escaped);
    assert!(
        matches!(outcome, Err(Error::LaunchUnstopped)),
        "{outcome:?}"
    );
    assert!(reported.0.lock().unwrap().is_none());
}
/// Keeps how a launch that couldn't be stopped can be checked again.
#[derive(Default)]
struct Reported(std::sync::Mutex<Option<std::sync::Arc<dyn ports::LaunchCheck>>>);
impl Authority for Reported {
    fn check(&self) -> Result<(), Error> {
        Ok(())
    }
    fn unstopped(&self, launch: std::sync::Arc<dyn ports::LaunchCheck>) {
        *self.0.lock().unwrap() = Some(launch);
    }
}
/// A scope that couldn't be confirmed stopped is reported with a way to
/// ask systemd again. Asked later, it proves the launch stopped only once
/// systemd says the scope is inactive: not while it says the app is
/// running, nor while it can't be reached. Asked to stop again, it kills
/// the scope first, as the launch's cleanup does.
#[cfg(unix)]
#[tokio::test]
async fn a_scope_not_confirmed_stopped_can_be_checked_again() {
    use std::os::unix::fs::PermissionsExt;
    let f = fixture();
    let bin = f._root.path().join("bin");
    let runtime = f._root.path().join("run");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::create_dir_all(runtime.join("systemd")).unwrap();
    std::fs::write(runtime.join("systemd/private"), "").unwrap();
    let (helper, child) = hanging_gio(&bin, f._root.path());
    let (answer, calls) = (f._root.path().join("answer"), f._root.path().join("calls"));
    std::fs::write(&answer, "active").unwrap();
    for (tool, body) in [
        (
            "systemd-run",
            "while [ \"$1\" != -- ]; do shift; done; shift; exec \"$@\"".to_owned(),
        ),
        // Every kill fails; is-active says what the answer file says.
        (
            "systemctl",
            format!(
                "echo \"$2\" >> '{}'\n[ \"$2\" = is-active ] || exit 1\ncase $(cat '{}') in\nactive) echo active; exit 0 ;;\ninactive) echo inactive; exit 3 ;;\n*) echo 'Failed to connect to bus' >&2; exit 1 ;;\nesac",
                calls.display(),
                answer.display()
            ),
        ),
    ] {
        let path = bin.join(tool);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let mut environment = f.environment.clone();
    environment.insert("PATH".into(), bin.to_string_lossy().into_owned());
    environment.insert(
        "XDG_RUNTIME_DIR".into(),
        runtime.to_string_lossy().into_owned(),
    );
    let launcher = LinuxLauncher::new(environment, f.roots.clone());
    let apps = launcher.apps().await.unwrap();
    let Resolution::One(app) = resolve("the terminal", &apps) else {
        panic!()
    };
    let reported = Reported::default();
    let outcome = launcher
        .launch(
            &VoiceApp {
                id: app.id,
                name: app.name,
            },
            &reported,
        )
        .await;
    let _ = (running(&helper), running(&child));
    assert!(
        matches!(outcome, Err(Error::LaunchUnstopped)),
        "{outcome:?}"
    );
    let check = reported.0.lock().unwrap().clone().expect("a check");
    // The app is running in the scope.
    assert!(!check.stopped(false).await);
    // systemd can't be reached.
    std::fs::write(&answer, "unreachable").unwrap();
    assert!(!check.stopped(false).await);
    // The scope is gone.
    std::fs::write(&answer, "inactive").unwrap();
    std::fs::write(&calls, "").unwrap();
    assert!(check.stopped(false).await);
    assert_eq!(std::fs::read_to_string(&calls).unwrap(), "is-active\n");
    assert!(check.stopped(true).await);
    assert_eq!(
        std::fs::read_to_string(&calls).unwrap(),
        "is-active\nkill\nis-active\n"
    );
}
/// Records when it was last asked: the launch's final authority check.
#[derive(Default)]
struct Timed(std::sync::Mutex<Option<std::time::Instant>>);
impl Authority for Timed {
    fn check(&self) -> Result<(), Error> {
        *self.0.lock().unwrap() = Some(std::time::Instant::now());
        Ok(())
    }
}
/// A hung launch whose service manager doesn't answer at all still answers
/// within what voice control and releases wait for, from its final check
/// and overall, and never claims it stopped.
#[cfg(unix)]
#[tokio::test]
async fn a_hung_launch_answers_in_time_even_when_the_service_manager_hangs() {
    use std::os::unix::fs::PermissionsExt;
    let f = fixture();
    let bin = f._root.path().join("bin");
    let runtime = f._root.path().join("run");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::create_dir_all(runtime.join("systemd")).unwrap();
    std::fs::write(runtime.join("systemd/private"), "").unwrap();
    let (helper, child) = hanging_gio(&bin, f._root.path());
    for (tool, body) in [
        (
            "systemd-run",
            "while [ \"$1\" != -- ]; do shift; done; shift; exec \"$@\"",
        ),
        // Neither the kill nor the question about the scope is answered.
        ("systemctl", "exec sleep 35"),
    ] {
        let path = bin.join(tool);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let mut environment = f.environment.clone();
    environment.insert("PATH".into(), format!("{}:/usr/bin:/bin", bin.display()));
    environment.insert(
        "XDG_RUNTIME_DIR".into(),
        runtime.to_string_lossy().into_owned(),
    );
    let launcher = LinuxLauncher::new(environment, f.roots.clone());
    let apps = launcher.apps().await.unwrap();
    let Resolution::One(app) = resolve("the terminal", &apps) else {
        panic!()
    };
    let timed = Timed::default();
    let began = std::time::Instant::now();
    let outcome = launcher
        .launch(
            &VoiceApp {
                id: app.id,
                name: app.name,
            },
            &timed,
        )
        .await;
    let ended = std::time::Instant::now();
    let _ = (running(&helper), running(&child));
    assert!(
        matches!(outcome, Err(Error::LaunchUnstopped)),
        "{outcome:?}"
    );
    let checked = timed.0.lock().unwrap().unwrap();
    assert!(
        ended - checked <= voice_control::LAUNCH_AFTER_CHECK,
        "{:?} after its check",
        ended - checked
    );
    assert!(
        ended - began <= voice_control::LAUNCH_LIMIT,
        "{:?}",
        ended - began
    );
}
/// Desktop entries that can't be read in time (a stuck file system) refuse
/// the launch, and nothing starts.
#[cfg(unix)]
#[tokio::test]
async fn entries_that_cannot_be_read_in_time_refuse_the_launch() {
    use std::os::unix::fs::PermissionsExt;
    let f = fixture();
    let bin = f._root.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let log = f._root.path().join("log");
    let gio = bin.join("gio");
    std::fs::write(
        &gio,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"gio $*\" >> '{}'\n",
            log.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&gio, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut environment = f.environment.clone();
    environment.insert("PATH".into(), bin.to_string_lossy().into_owned());
    let launcher = LinuxLauncher::new(environment, f.roots.clone());
    let apps = launcher.apps().await.unwrap();
    let Resolution::One(app) = resolve("the terminal", &apps) else {
        panic!()
    };
    // An entry that blocks whoever opens it: a named pipe nobody writes.
    let stuck = f._root.path().join("share/applications/stuck.desktop");
    rustix::fs::mknodat(
        rustix::fs::CWD,
        &stuck,
        rustix::fs::FileType::Fifo,
        rustix::fs::Mode::from_raw_mode(0o600),
        0,
    )
    .unwrap();
    let began = std::time::Instant::now();
    let outcome = launcher
        .launch(
            &VoiceApp {
                id: app.id,
                name: app.name,
            },
            &Allow,
        )
        .await;
    let waited = began.elapsed();
    // Lets the abandoned read finish, if it is still waiting.
    {
        use std::os::unix::fs::OpenOptionsExt;
        drop(
            std::fs::OpenOptions::new()
                .write(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(&stuck),
        );
    }
    assert!(
        matches!(&outcome, Err(Error::Unavailable(m)) if m.contains("too long")),
        "{outcome:?}"
    );
    assert!(waited < Duration::from_secs(2), "{waited:?}");
    assert!(!log.exists());
}
/// Finding the programs a launch runs, and the home folder it starts in,
/// is bounded too: on a stuck file system the launch is refused in time as
/// one that didn't start, and nothing starts once the lookup ends late.
#[cfg(unix)]
#[tokio::test]
async fn programs_that_cannot_be_found_in_time_refuse_the_launch() {
    use std::os::unix::fs::PermissionsExt;
    let f = fixture();
    let bin = f._root.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let log = f._root.path().join("log");
    let gio = bin.join("gio");
    std::fs::write(
        &gio,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"gio $*\" >> '{}'\n",
            log.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&gio, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut environment = f.environment.clone();
    environment.insert("PATH".into(), bin.to_string_lossy().into_owned());
    let apps = LinuxLauncher::new(environment.clone(), f.roots.clone())
        .apps()
        .await
        .unwrap();
    let Resolution::One(app) = resolve("the terminal", &apps) else {
        panic!()
    };
    let lookup = 1200;
    environment.insert(SLOW_LOOKUP.into(), lookup.to_string());
    let launcher = LinuxLauncher::new(environment, f.roots.clone());
    let app = VoiceApp {
        id: app.id,
        name: app.name,
    };
    for url in [false, true] {
        let began = std::time::Instant::now();
        let outcome = if url {
            launcher.open_url("https://example.com", &Allow).await
        } else {
            launcher.launch(&app, &Allow).await
        };
        // A launch that didn't start: neither unknown nor unstopped.
        assert!(
            matches!(&outcome, Err(Error::Unavailable(m)) if m.contains("too long") && !m.contains("unavailable:")),
            "{outcome:?}"
        );
        let waited = began.elapsed();
        assert!(waited < PROGRAMS + Duration::from_millis(300), "{waited:?}");
    }
    // The lookups end late, and start nothing.
    tokio::time::sleep(Duration::from_millis(lookup + 300)).await;
    assert!(!log.exists());
}
