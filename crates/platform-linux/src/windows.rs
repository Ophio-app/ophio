// SPDX-License-Identifier: Apache-2.0
//! Explicit session inputs only. Construction never connects to a desktop.
mod apps;
#[cfg(target_os = "linux")]
mod kwin;
use async_trait::async_trait;
use ports::{Clock, PlatformError, windows::WindowActions};
use protocol::{capability::Availability, windows::*};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::io::AsyncReadExt;

#[derive(Clone, Debug)]
pub enum Backend {
    Hyprland { socket: String },
    Sway { socket: String },
    Kwin { bus: String },
    X11 { display: String },
    Gnome,
    Unavailable,
}
impl Backend {
    fn name(&self) -> &'static str {
        match self {
            Self::Hyprland { .. } => "hyprland",
            Self::Sway { .. } => "sway",
            Self::Kwin { .. } => "kde",
            Self::X11 { .. } => "x11",
            Self::Gnome => "gnome",
            Self::Unavailable => "none",
        }
    }
}
#[derive(Debug)]
pub struct LinuxWindows {
    backend: Backend,
    environment: BTreeMap<String, String>,
    roots: Vec<PathBuf>,
    clock: Arc<dyn Clock>,
    catalog: tokio::sync::Mutex<BTreeMap<String, NativeWindow>>,
    applications: tokio::sync::Mutex<BTreeMap<String, apps::Entry>>,
}
#[derive(Clone, Debug, PartialEq)]
struct NativeWindow {
    info: WindowInfo,
    native: String,
    pid: Option<u64>,
}
impl LinuxWindows {
    pub fn from_environment(environment: &BTreeMap<String, String>, clock: Arc<dyn Clock>) -> Self {
        let get = |key| environment.get(key).cloned().unwrap_or_default();
        let desktop = get("XDG_CURRENT_DESKTOP").to_ascii_lowercase();
        let backend =
            if desktop.contains("hyprland") && !get("HYPRLAND_INSTANCE_SIGNATURE").is_empty() {
                Backend::Hyprland {
                    socket: get("HYPRLAND_INSTANCE_SIGNATURE"),
                }
            } else if !get("SWAYSOCK").is_empty() {
                Backend::Sway {
                    socket: get("SWAYSOCK"),
                }
            } else if desktop.contains("gnome") {
                Backend::Gnome
            } else if desktop.contains("kde") && !get("DBUS_SESSION_BUS_ADDRESS").is_empty() {
                Backend::Kwin {
                    bus: get("DBUS_SESSION_BUS_ADDRESS"),
                }
            } else if get("XDG_SESSION_TYPE") == "x11" && !get("DISPLAY").is_empty() {
                Backend::X11 {
                    display: get("DISPLAY"),
                }
            } else {
                Backend::Unavailable
            };
        Self::new(
            backend,
            environment.clone(),
            apps::roots(environment),
            clock,
        )
    }
    pub fn new(
        backend: Backend,
        environment: BTreeMap<String, String>,
        roots: Vec<PathBuf>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            backend,
            environment,
            roots,
            clock,
            catalog: Default::default(),
            applications: Default::default(),
        }
    }
    fn program(&self, name: &str) -> Result<PathBuf, PlatformError> {
        apps::program(&self.environment, name).ok_or_else(|| {
            PlatformError::Unsupported(format!(
                "Install {name} on the computer to use {} window actions",
                self.backend.name()
            ))
        })
    }
    async fn command(
        &self,
        name: &str,
        args: &[&str],
        effect: bool,
    ) -> Result<String, PlatformError> {
        run(&self.program(name)?, args, &self.environment, effect).await
    }
    async fn native_windows(&self) -> Result<Vec<NativeWindow>, PlatformError> {
        match &self.backend {
            Backend::Hyprland { socket } => {
                let clients = self.command("hyprctl", &["-i", socket, "-j", "clients"], false).await?;
                let active = self.command("hyprctl", &["-i", socket, "-j", "activewindow"], false).await?;
                parse_hyprland(&clients, &active)
            }
            Backend::Sway { socket } => parse_sway(&self.command("swaymsg", &["-s", socket, "-r", "-t", "get_tree"], false).await?),
            #[cfg(target_os = "linux")]
            Backend::Kwin { bus } => parse_kwin(&kwin::call(bus, None, None).await?),
            Backend::X11 { display } => self.x11_windows(display).await,
            _ => Err(PlatformError::Unsupported("This desktop does not expose a public window list. Opening installed apps is still available.".into())),
        }
    }
    async fn x11_windows(&self, display: &str) -> Result<Vec<NativeWindow>, PlatformError> {
        let root = self
            .command(
                "xprop",
                &[
                    "-display",
                    display,
                    "-root",
                    "_NET_CLIENT_LIST",
                    "_NET_ACTIVE_WINDOW",
                ],
                false,
            )
            .await?;
        let ids = x_ids(
            root.lines()
                .find(|l| l.starts_with("_NET_CLIENT_LIST"))
                .unwrap_or(""),
        );
        let active = x_ids(
            root.lines()
                .find(|l| l.starts_with("_NET_ACTIVE_WINDOW"))
                .unwrap_or(""),
        )
        .into_iter()
        .next();
        let mut windows = Vec::new();
        for id in ids.into_iter().take(512) {
            if let Ok(properties) = self
                .command(
                    "xprop",
                    &[
                        "-display",
                        display,
                        "-id",
                        &id,
                        "_NET_WM_NAME",
                        "WM_NAME",
                        "WM_CLASS",
                        "_NET_WM_DESKTOP",
                        "_NET_WM_PID",
                    ],
                    false,
                )
                .await
            {
                windows.push(parse_x11(
                    &id,
                    &properties,
                    active.as_deref() == Some(id.as_str()),
                ));
            }
        }
        Ok(windows)
    }
}
#[async_trait]
impl WindowActions for LinuxWindows {
    async fn capabilities(&self) -> WindowCapabilities {
        let unavailable = |reason: &str| Availability::Unsupported {
            reason: reason.into(),
        };
        let windows = match &self.backend {
            Backend::Gnome => unavailable(
                "GNOME does not provide a public window list or focus API to other clients without an extension. You can still open an app.",
            ),
            Backend::Unavailable => unavailable("No supported desktop session was found"),
            Backend::Hyprland { .. } => match self.program("hyprctl") {
                Ok(_) => Availability::Available,
                Err(e) => unavailable(&e.to_string()),
            },
            Backend::Sway { .. } => match self.program("swaymsg") {
                Ok(_) => Availability::Available,
                Err(e) => unavailable(&e.to_string()),
            },
            Backend::X11 { .. } => {
                if self.program("xprop").is_ok() && self.program("wmctrl").is_ok() {
                    Availability::Available
                } else {
                    unavailable("X11 window actions need xprop and wmctrl on the computer")
                }
            }
            Backend::Kwin { .. } => Availability::Unknown {
                reason: "KWin’s scripting interface is checked when the window list is refreshed"
                    .into(),
            },
        };
        WindowCapabilities {
            backend: self.backend.name().into(),
            windows,
            open_apps: if self.program("gio").is_ok() {
                Availability::Available
            } else {
                unavailable("Opening desktop entries needs gio on the computer")
            },
        }
    }
    async fn catalog(&self, search: &str) -> Result<WindowCatalog, PlatformError> {
        let mut capabilities = self.capabilities().await;
        let mut catalog = self.catalog.lock().await;
        let mut native = match self.native_windows().await {
            Ok(windows) => {
                capabilities.windows = Availability::Available;
                windows
            }
            Err(error) => {
                if !matches!(capabilities.windows, Availability::Unsupported { .. }) {
                    capabilities.windows = Availability::TemporarilyUnavailable {
                        reason: error.to_string(),
                    };
                }
                Vec::new()
            }
        };
        let query = search.to_lowercase();
        let mut next = BTreeMap::new();
        for w in &mut native {
            let previous = catalog.iter().find(|(_, old)| {
                old.native == w.native && old.pid == w.pid && old.info.app == w.info.app
            });
            w.info.window_id = previous
                .map(|(id, _)| id.clone())
                .unwrap_or_else(|| protocol::CommandId::new().to_string());
            next.insert(w.info.window_id.clone(), w.clone());
        }
        *catalog = next;
        let windows = native
            .into_iter()
            .map(|w| w.info)
            .filter(|w| {
                format!("{} {}", w.title, w.app)
                    .to_lowercase()
                    .contains(&query)
            })
            .collect::<Vec<_>>();
        let entries = apps::discover(&self.roots, &self.environment)?;
        let mut applications = self.applications.lock().await;
        let mut next = BTreeMap::new();
        for entry in entries {
            let key = applications
                .iter()
                .find(|(_, old)| old.path == entry.path && old.fingerprint == entry.fingerprint)
                .map(|(id, _)| id.clone())
                .unwrap_or_else(|| protocol::CommandId::new().to_string());
            next.insert(key, entry);
        }
        *applications = next;
        let apps: Vec<_> = applications
            .iter()
            .filter(|(_, e)| e.name.to_lowercase().contains(&query))
            .map(|(id, e)| InstalledApp {
                app_id: id.clone(),
                name: e.name.clone(),
            })
            .collect();
        let truncated = windows.len() > 200 || apps.len() > 200;
        Ok(WindowCatalog {
            capabilities,
            windows: windows.into_iter().take(200).collect(),
            apps: apps.into_iter().take(200).collect(),
            observed_at: self.clock.now(),
            truncated,
        })
    }
    async fn act(
        &self,
        action: &WindowAction,
        authority: Arc<dyn ports::windows::WindowAuthority>,
    ) -> Result<(WindowOutcome, String), PlatformError> {
        if let WindowAction::OpenApp { app_id } = action {
            let entry = self
                .applications
                .lock()
                .await
                .get(app_id)
                .cloned()
                .ok_or(PlatformError::TargetNotFound)?;
            apps::revalidate(&entry)?;
            authority.check()?;
            self.command(
                "gio",
                &[
                    "launch",
                    entry.path.to_str().ok_or(PlatformError::TargetNotFound)?,
                ],
                true,
            )
            .await?;
            return Ok((
                WindowOutcome::AppLaunchRequested,
                "Asked the desktop to open the app. A window may take time to appear.".into(),
            ));
        }
        let (id, close) = match action {
            WindowAction::Focus { window_id } => (window_id, false),
            WindowAction::Close { window_id } => (window_id, true),
            _ => unreachable!(),
        };
        let previous = self
            .catalog
            .lock()
            .await
            .get(id)
            .cloned()
            .ok_or(PlatformError::TargetNotFound)?;
        let current = self
            .native_windows()
            .await?
            .into_iter()
            .find(|w| {
                w.native == previous.native
                    && w.pid == previous.pid
                    && w.info.app == previous.info.app
            })
            .ok_or(PlatformError::TargetNotFound)?;
        authority.check()?;
        match &self.backend {
            Backend::Hyprland { socket } => {
                let reply = self
                    .command(
                        "hyprctl",
                        &[
                            "-i",
                            socket,
                            "dispatch",
                            if close { "closewindow" } else { "focuswindow" },
                            &format!("address:{}", current.native),
                        ],
                        true,
                    )
                    .await?;
                if reply.trim() != "ok" {
                    return Err(PlatformError::OutcomeUnknown(
                        "The compositor didn’t confirm this window action".into(),
                    ));
                }
            }
            Backend::Sway { socket } => {
                let reply = self
                    .command(
                        "swaymsg",
                        &[
                            "-s",
                            socket,
                            "-r",
                            "-t",
                            "command",
                            &format!(
                                "[con_id={}] {}",
                                current.native,
                                if close { "kill" } else { "focus" }
                            ),
                        ],
                        true,
                    )
                    .await?;
                let results = json(&reply)?;
                if !results
                    .as_array()
                    .is_some_and(|a| !a.is_empty() && a.iter().all(|r| r["success"] == true))
                {
                    return Err(PlatformError::OutcomeUnknown(
                        "Sway didn’t confirm this window action".into(),
                    ));
                }
            }
            #[cfg(target_os = "linux")]
            Backend::Kwin { bus } => {
                kwin::call(
                    bus,
                    Some((&current.native, close)),
                    Some(authority.as_ref()),
                )
                .await?;
            }
            Backend::X11 { display } => {
                let mut environment = self.environment.clone();
                environment.insert("DISPLAY".into(), display.clone());
                run(
                    &self.program("wmctrl")?,
                    &["-i", if close { "-c" } else { "-a" }, &current.native],
                    &environment,
                    true,
                )
                .await?;
            }
            _ => {
                return Err(PlatformError::Unsupported(
                    "Window actions are unavailable on this desktop".into(),
                ));
            }
        }
        Ok(if close {
            (
                WindowOutcome::CloseRequested,
                "Close requested. Unsaved work may prompt on the computer.".into(),
            )
        } else {
            (
                WindowOutcome::FocusRequested,
                "Asked the desktop to focus the window. Refresh to check which window is focused."
                    .into(),
            )
        })
    }
}
async fn run(
    program: &Path,
    args: &[&str],
    environment: &BTreeMap<String, String>,
    effect: bool,
) -> Result<String, PlatformError> {
    use std::process::Stdio;
    let mut child = tokio::process::Command::new(program)
        .args(args)
        .env_clear()
        .envs(environment)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| PlatformError::Native("Window helper output unavailable".into()))?;
    let result = tokio::time::timeout(Duration::from_secs(2), async {
        let mut bytes = Vec::new();
        stdout
            .take(4 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .await?;
        if bytes.len() > 4 * 1024 * 1024 {
            return Err(std::io::ErrorKind::FileTooLarge.into());
        }
        let status = child.wait().await?;
        if !status.success() {
            return Err(std::io::ErrorKind::Other.into());
        }
        Ok::<_, std::io::Error>(bytes)
    })
    .await;
    match result {
        Ok(Ok(bytes)) => String::from_utf8(bytes)
            .map_err(|_| PlatformError::Native("Invalid window helper response".into())),
        _ => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            Err(if effect {
                PlatformError::OutcomeUnknown(
                    "The desktop did not confirm the action. Check before trying again.".into(),
                )
            } else {
                PlatformError::TemporarilyUnavailable(
                    "The desktop did not answer its window query".into(),
                )
            })
        }
    }
}
fn json(text: &str) -> Result<Value, PlatformError> {
    serde_json::from_str(text)
        .map_err(|_| PlatformError::Native("Invalid window list response".into()))
}
fn string(value: &Value) -> String {
    value
        .as_str()
        .unwrap_or_default()
        .chars()
        .take(1024)
        .collect()
}
fn hex_id(id: &str) -> bool {
    id.starts_with("0x")
        && id.len() > 2
        && id.len() <= 18
        && id[2..].bytes().all(|c| c.is_ascii_hexdigit())
}
fn parse_hyprland(clients: &str, active: &str) -> Result<Vec<NativeWindow>, PlatformError> {
    let clients = json(clients)?;
    let active = json(active)?;
    let clients = clients
        .as_array()
        .ok_or_else(|| PlatformError::Native("Invalid Hyprland clients".into()))?;
    Ok(clients
        .iter()
        .filter(|c| c["mapped"] != false && hex_id(&string(&c["address"])))
        .map(|c| NativeWindow {
            native: string(&c["address"]),
            pid: c["pid"].as_u64(),
            info: WindowInfo {
                window_id: String::new(),
                title: string(&c["title"]),
                app: string(&c["class"]),
                workspace: c["workspace"]["name"].as_str().map(str::to_owned),
                monitor: c["monitor"].as_i64().map(|id| id.to_string()),
                focused: c["address"] == active["address"],
            },
        })
        .collect())
}
fn parse_sway(text: &str) -> Result<Vec<NativeWindow>, PlatformError> {
    fn visit(
        node: &Value,
        workspace: Option<String>,
        monitor: Option<String>,
        depth: usize,
        out: &mut Vec<NativeWindow>,
    ) {
        if depth > 64 || out.len() >= 2048 {
            return;
        }
        let kind = node["type"].as_str().unwrap_or("");
        let workspace = if kind == "workspace" {
            Some(string(&node["name"]))
        } else {
            workspace
        };
        let monitor = if kind == "output" {
            Some(string(&node["name"]))
        } else {
            monitor
        };
        if matches!(kind, "con" | "floating_con")
            && (node["app_id"].is_string() || node["window"].as_u64().is_some())
            && let Some(id) = node["id"].as_u64()
        {
            out.push(NativeWindow {
                native: id.to_string(),
                pid: node["pid"].as_u64(),
                info: WindowInfo {
                    window_id: String::new(),
                    title: string(&node["name"]),
                    app: node["app_id"]
                        .as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| string(&node["window_properties"]["class"])),
                    workspace: workspace.clone(),
                    monitor: monitor.clone(),
                    focused: node["focused"] == true,
                },
            });
        }
        for child in ["nodes", "floating_nodes"]
            .into_iter()
            .flat_map(|field| node[field].as_array().into_iter().flatten())
        {
            visit(child, workspace.clone(), monitor.clone(), depth + 1, out);
        }
    }
    let mut out = Vec::new();
    visit(&json(text)?, None, None, 0, &mut out);
    Ok(out)
}
fn parse_kwin(text: &str) -> Result<Vec<NativeWindow>, PlatformError> {
    let value = json(text)?;
    let entries = value
        .as_array()
        .ok_or_else(|| PlatformError::Native("Invalid KWin window list".into()))?;
    Ok(entries
        .iter()
        .filter(|v| {
            v["id"]
                .as_str()
                .is_some_and(|s| !s.is_empty() && s.len() < 128)
        })
        .map(|v| NativeWindow {
            native: string(&v["id"]),
            pid: v["pid"].as_u64(),
            info: WindowInfo {
                window_id: String::new(),
                title: string(&v["title"]),
                app: string(&v["app"]),
                workspace: v["workspace"].as_str().map(str::to_owned),
                monitor: v["monitor"].as_str().map(str::to_owned),
                focused: v["focused"] == true,
            },
        })
        .collect())
}
fn x_ids(line: &str) -> Vec<String> {
    line.split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|s| hex_id(s))
        .map(str::to_owned)
        .collect()
}
fn parse_x11(id: &str, text: &str, focused: bool) -> NativeWindow {
    let property = |key: &str| {
        text.lines()
            .find(|l| l.starts_with(&format!("{key}(")))
            .and_then(|l| l.split_once(" = "))
            .map(|(_, v)| v.trim_matches('"').to_owned())
    };
    NativeWindow {
        native: id.into(),
        pid: property("_NET_WM_PID").and_then(|p| p.parse().ok()),
        info: WindowInfo {
            window_id: String::new(),
            title: property("_NET_WM_NAME")
                .or_else(|| property("WM_NAME"))
                .unwrap_or_default(),
            app: property("WM_CLASS").unwrap_or_default(),
            workspace: property("_NET_WM_DESKTOP"),
            monitor: None,
            focused,
        },
    }
}
#[cfg(test)]
mod tests;
