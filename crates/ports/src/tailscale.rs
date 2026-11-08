// SPDX-License-Identifier: Apache-2.0
//! Where the Tailscale program is on this computer.
//!
//! Øphio runs as a service whose `PATH` is the service manager's, not the
//! person's shell's. A Mac's launchd agent has only the system folders, and
//! the Tailscale app keeps its command-line program inside the app. So after
//! `PATH` come the places Tailscale's own installers use on each platform.
//! Everything that runs Tailscale or checks for it looks here.
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

/// What every run of the program is given. On a Mac the program is the
/// Tailscale app itself, which acts as the command-line tool only when it can
/// tell it was started as one: from a terminal (`TERM`), or with this. A
/// service has no terminal, so without it the app tries to open its window,
/// fails, and prints no status. Other platforms' programs ignore it.
pub const AS_CLI: (&str, &str) = ("TAILSCALE_BE_CLI", "1");

/// The program's file name on `os` (as [`std::env::consts::OS`] names it).
pub fn file_name(os: &str) -> &'static str {
    if os == "windows" {
        "tailscale.exe"
    } else {
        "tailscale"
    }
}

/// Where Tailscale's installers put the program on `os`, in the order they
/// are tried after `PATH`. `program_files` is Windows' `ProgramFiles`.
pub fn usual_places(os: &str, program_files: Option<&OsStr>) -> Vec<PathBuf> {
    match os {
        "macos" => [
            // The app from the Mac App Store or Tailscale's own download.
            "/Applications/Tailscale.app/Contents/MacOS/Tailscale",
            "/opt/homebrew/bin/tailscale",
            "/usr/local/bin/tailscale",
        ]
        .map(PathBuf::from)
        .into(),
        "windows" => {
            let base = program_files.unwrap_or(OsStr::new(r"C:\Program Files"));
            vec![Path::new(base).join("Tailscale").join("tailscale.exe")]
        }
        _ => ["/usr/bin/tailscale", "/usr/local/bin/tailscale"]
            .map(PathBuf::from)
            .into(),
    }
}

/// The Tailscale program on `os`: the first on `path`, else the first of
/// [`usual_places`] that `exists` says is there.
pub fn find(
    os: &str,
    path: Option<&OsStr>,
    program_files: Option<&OsStr>,
    exists: impl Fn(&Path) -> bool,
) -> Option<PathBuf> {
    let name = file_name(os);
    path.map(std::env::split_paths)
        .into_iter()
        .flatten()
        .filter(|folder| !folder.as_os_str().is_empty() && folder.is_absolute())
        .map(|folder| folder.join(name))
        .chain(usual_places(os, program_files))
        .find(|program| exists(program))
}

/// The Tailscale program on this computer, if it is installed.
pub fn program() -> Option<PathBuf> {
    let program_files: Option<OsString> = std::env::var_os("ProgramFiles");
    find(
        std::env::consts::OS,
        std::env::var_os("PATH").as_deref(),
        program_files.as_deref(),
        Path::is_file,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn there(paths: &[&str]) -> impl Fn(&Path) -> bool {
        let set: HashSet<PathBuf> = paths.iter().map(PathBuf::from).collect();
        move |p| set.contains(p)
    }

    #[test]
    fn path_comes_first() {
        let path = OsString::from("/home/sam/bin:/usr/bin");
        let found = find(
            "linux",
            Some(&path),
            None,
            there(&["/home/sam/bin/tailscale", "/usr/bin/tailscale"]),
        );
        assert_eq!(found, Some(PathBuf::from("/home/sam/bin/tailscale")));
    }

    /// A Mac's launchd agent has only the system folders on `PATH`; the app
    /// keeps its program inside it.
    #[test]
    fn a_mac_finds_the_app_s_own_program() {
        let path = OsString::from("/usr/bin:/bin:/usr/sbin:/sbin");
        let app = "/Applications/Tailscale.app/Contents/MacOS/Tailscale";
        assert_eq!(
            find("macos", Some(&path), None, there(&[app])),
            Some(PathBuf::from(app))
        );
        assert_eq!(
            find(
                "macos",
                Some(&path),
                None,
                there(&["/opt/homebrew/bin/tailscale"])
            ),
            Some(PathBuf::from("/opt/homebrew/bin/tailscale"))
        );
        assert_eq!(find("macos", Some(&path), None, there(&[])), None);
    }

    #[test]
    fn linux_looks_in_the_system_folders_without_path() {
        assert_eq!(
            find("linux", None, None, there(&["/usr/bin/tailscale"])),
            Some(PathBuf::from("/usr/bin/tailscale"))
        );
        assert_eq!(
            find(
                "linux",
                Some(OsStr::new("")),
                None,
                there(&["/usr/local/bin/tailscale"])
            ),
            Some(PathBuf::from("/usr/local/bin/tailscale"))
        );
    }

    #[test]
    fn windows_looks_in_program_files() {
        let found = find(
            "windows",
            None,
            Some(OsStr::new("D:/Apps")),
            there(&["D:/Apps/Tailscale/tailscale.exe"]),
        );
        assert_eq!(
            found,
            Some(PathBuf::from("D:/Apps/Tailscale/tailscale.exe"))
        );
        assert_eq!(
            usual_places("windows", None),
            vec![
                Path::new(r"C:\Program Files")
                    .join("Tailscale")
                    .join("tailscale.exe")
            ]
        );
    }

    /// A relative or empty `PATH` entry would find a program in whatever
    /// folder Øphio happens to run in.
    #[test]
    fn only_absolute_folders_on_path_count() {
        let path = OsString::from("bin::/usr/bin");
        assert_eq!(
            find(
                "linux",
                Some(&path),
                None,
                there(&["bin/tailscale", "tailscale"])
            ),
            None
        );
    }
}
