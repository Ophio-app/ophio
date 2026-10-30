// SPDX-License-Identifier: Apache-2.0
//! The computer's own browser: where sign-in pages and providers' pages
//! open, so the password manager there can fill them.
use std::sync::Mutex;

pub trait Opener: Send + Sync + std::fmt::Debug + 'static {
    /// Opens `url` in the computer's default browser.
    fn open(&self, url: &str) -> Result<(), String>;
    /// That browser, as the desktop names it.
    fn browser(&self) -> Option<String>;
}

/// The desktop's own way of opening an address.
#[derive(Debug, Default)]
pub struct Desktop;

impl Opener for Desktop {
    fn open(&self, url: &str) -> Result<(), String> {
        // Only web addresses: what opens them would start a program or file
        // as readily.
        if !url::Url::parse(url).is_ok_and(|u| matches!(u.scheme(), "https" | "http")) {
            return Err("Only a web address can be opened in the browser.".into());
        }
        // Handed over whole, never through `cmd`, which would read `&` in
        // the address as the start of another command.
        #[cfg(windows)]
        return platform_windows::open_url::open_url(url)
            .map_err(|e| format!("The computer's browser could not be opened: {e}"));
        #[cfg(not(windows))]
        {
            let mut command = opener(url);
            command
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
            #[cfg(target_os = "linux")]
            for (name, value) in session_display() {
                command.env(name, value);
            }
            let mut child = command
                .spawn()
                .map_err(|e| format!("The computer's browser could not be opened: {e}"))?;
            // Reap it in the background; xdg-open returns once the browser has it.
            std::thread::spawn(move || {
                let _ = child.wait();
            });
            Ok(())
        }
    }

    fn browser(&self) -> Option<String> {
        #[cfg(target_os = "linux")]
        {
            let out = std::process::Command::new("xdg-settings")
                .args(["get", "default-web-browser"])
                .stderr(std::process::Stdio::null())
                .output()
                .ok()?;
            let id = String::from_utf8_lossy(&out.stdout).trim().to_owned();
            if id.is_empty() {
                return None;
            }
            Some(desktop_name(&id).unwrap_or_else(|| id.trim_end_matches(".desktop").to_owned()))
        }
        #[cfg(not(target_os = "linux"))]
        None
    }
}

#[cfg(not(windows))]
fn opener(url: &str) -> std::process::Command {
    #[cfg(target_os = "linux")]
    {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(url);
        c
    }
    #[cfg(target_os = "macos")]
    {
        let mut c = std::process::Command::new("open");
        c.arg(url);
        c
    }
}

/// The graphical session's display, when the companion was started without
/// one (as a user service started before the desktop).
#[cfg(target_os = "linux")]
fn session_display() -> Vec<(String, String)> {
    if std::env::var_os("WAYLAND_DISPLAY").is_some() || std::env::var_os("DISPLAY").is_some() {
        return Vec::new();
    }
    let Ok(out) = std::process::Command::new("systemctl")
        .args(["--user", "show-environment"])
        .stderr(std::process::Stdio::null())
        .output()
    else {
        return Vec::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| line.split_once('='))
        .filter(|(k, _)| matches!(*k, "WAYLAND_DISPLAY" | "DISPLAY" | "XDG_CURRENT_DESKTOP"))
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect()
}

/// The `Name=` of an installed desktop entry, such as "Firefox".
#[cfg(target_os = "linux")]
fn desktop_name(id: &str) -> Option<String> {
    let mut dirs = vec![
        std::env::var_os("XDG_DATA_HOME")
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|h| std::path::Path::new(&h).join(".local/share"))
            })?,
    ];
    let system =
        std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".into());
    dirs.extend(system.split(':').map(std::path::PathBuf::from));
    dirs.iter().find_map(|dir| {
        let text = std::fs::read_to_string(dir.join("applications").join(id)).ok()?;
        text.lines()
            .skip_while(|l| l.trim() != "[Desktop Entry]")
            .find_map(|l| l.strip_prefix("Name="))
            .map(|n| n.trim().to_owned())
    })
}

/// For tests: remembers what it was asked to open and opens nothing.
#[derive(Debug, Default)]
pub struct Recorder {
    pub opened: Mutex<Vec<String>>,
}
impl Recorder {
    pub fn opened(&self) -> Vec<String> {
        self.opened.lock().expect("recorder poisoned").clone()
    }
}
impl Opener for Recorder {
    fn open(&self, url: &str) -> Result<(), String> {
        self.opened
            .lock()
            .expect("recorder poisoned")
            .push(url.to_owned());
        Ok(())
    }
    fn browser(&self) -> Option<String> {
        Some("Test browser".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_web_addresses_are_opened() {
        // Each is refused before anything is started.
        for url in [
            "file:///etc/passwd",
            "calc.exe",
            r"C:\Windows\System32\calc.exe",
            "javascript:alert(1)",
            "ms-settings:privacy",
        ] {
            assert!(Desktop.open(url).is_err(), "{url}");
        }
    }
}
