// SPDX-License-Identifier: Apache-2.0
//! Where the agents' programs (`claude`, `codex`) are on this computer.
//!
//! Setup, sign-in and the task runners all look here, so that what setup
//! reports as found is what sign-in and tasks start. Øphio runs as a service
//! whose `PATH` is the service manager's, not the person's shell's: right
//! after an installer adds its folder to the shell's start-up files, or for a
//! Node.js kept by nvm, that `PATH` doesn't have it. So after `PATH` come the
//! folders that installers use for a person without admin rights.
//!
//! Windows keeps its own lookups, in each caller, as they were.
use std::ffi::{OsStr, OsString};
use std::path::{Component, Path, PathBuf};

/// Folders under the home folder where installers put agents for a person
/// without admin rights, in the order they are searched:
///
/// - `.local/bin`: Claude Code's native installer, and Codex's standalone
///   installer's link to its program.
/// - `.cargo/bin`: programs installed with Cargo.
/// - `.claude/local`: Claude Code's older local installs.
/// - `.npm-global/bin`: the folder of one's own that npm's guide to its
///   EACCES error suggests for global packages.
///
/// The first two, in that order, are where tasks looked before.
pub const HOME_FOLDERS: [&str; 4] = [
    ".local/bin",
    ".cargo/bin",
    ".claude/local",
    ".npm-global/bin",
];

/// The folders a program started without `PATH` finds others in.
pub const DEFAULT_PATH: [&str; 3] = ["/usr/local/bin", "/usr/bin", "/bin"];

/// The folders searched for a program, in order: each folder on `path`, then
/// [`HOME_FOLDERS`] under `home`, then the `bin` folder of nvm's default
/// Node.js. Folders may not exist.
pub fn folders(path: Option<&OsStr>, home: Option<&Path>) -> Vec<PathBuf> {
    let mut folders: Vec<PathBuf> = path
        .map(|path| std::env::split_paths(path).collect())
        .unwrap_or_default();
    if let Some(home) = home {
        folders.extend(HOME_FOLDERS.iter().map(|folder| home.join(folder)));
        folders.extend(nvm_bin(home));
    }
    folders
}

/// The program called `name` in the first of [`folders`] that has it.
pub fn find(name: &str, path: Option<&OsStr>, home: Option<&Path>) -> Option<PathBuf> {
    folders(path, home)
        .into_iter()
        .map(|folder| folder.join(name))
        .find(|program| program.is_file())
}

/// [`find`] with this process's own `PATH` and home folder.
pub fn on_this_computer(name: &str) -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    find(name, std::env::var_os("PATH").as_deref(), home.as_deref())
}

/// The `PATH` to start `program` with, when it was found outside `path`:
/// `path` with the program's own folder added at the end. A program installed
/// with npm is a script that starts `node` from `PATH`, and with nvm that
/// `node` is beside it. Without `path`, the system's usual folders
/// ([`DEFAULT_PATH`]) come first. `None` when `path` already has its folder.
pub fn path_for(program: &Path, path: Option<&OsStr>) -> Option<OsString> {
    let folder = program.parent().filter(|f| !f.as_os_str().is_empty())?;
    let mut folders: Vec<PathBuf> = match path.filter(|path| !path.is_empty()) {
        Some(path) => std::env::split_paths(path).collect(),
        None => DEFAULT_PATH.map(PathBuf::from).into(),
    };
    if folders.iter().any(|f| f == folder) {
        return None;
    }
    folders.push(folder.to_path_buf());
    std::env::join_paths(folders).ok()
}

/// The `bin` folder of the Node.js nvm (in `~/.nvm`) starts new shells with:
/// the version its `default` alias names, or else the newest installed.
/// nvm is set up only in the shell's start-up file, which a service never
/// reads.
fn nvm_bin(home: &Path) -> Option<PathBuf> {
    let nvm = home.join(".nvm");
    let versions = nvm.join("versions").join("node");
    let mut installed: Vec<(Vec<u64>, String)> = std::fs::read_dir(&versions)
        .ok()?
        .filter_map(|entry| {
            let name = entry.ok()?.file_name().into_string().ok()?;
            Some((version(&name)?, name))
        })
        .collect();
    installed.sort();
    let chosen = match nvm_alias(&nvm, "default") {
        // nvm leaves Node.js to the system.
        Some(Wanted::System) => return None,
        Some(Wanted::Version(wanted)) => installed
            .iter()
            .rev()
            .find(|(number, _)| number.starts_with(&wanted)),
        Some(Wanted::Newest) | None => None,
    }
    .or(installed.last())?;
    Some(versions.join(&chosen.1).join("bin"))
}

enum Wanted {
    System,
    Newest,
    /// The leading parts of a version number, such as `[22]` or `[22, 3, 0]`.
    Version(Vec<u64>),
}

/// What nvm's alias `name` stands for, following the aliases it names in
/// turn (`default` → `lts/*` → `lts/jod` → `v22.3.0`).
fn nvm_alias(nvm: &Path, name: &str) -> Option<Wanted> {
    let aliases = nvm.join("alias");
    let mut value = name.to_owned();
    for _ in 0..8 {
        let relative = Path::new(&value);
        // Only a name inside nvm's alias folder.
        if !relative
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
        {
            break;
        }
        match std::fs::read_to_string(aliases.join(relative)) {
            Ok(next) => value = next.trim().to_owned(),
            Err(_) => break,
        }
    }
    match value.as_str() {
        "system" => Some(Wanted::System),
        "node" | "stable" => Some(Wanted::Newest),
        _ => version(&value).map(Wanted::Version),
    }
}

/// The numbers of a version such as `v22.3.0`, `22.3` or `22`.
fn version(text: &str) -> Option<Vec<u64>> {
    let digits = text.strip_prefix('v').unwrap_or(text);
    let numbers: Option<Vec<u64>> = digits.split('.').map(|part| part.parse().ok()).collect();
    numbers.filter(|numbers| !numbers.is_empty() && numbers.len() <= 3)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Home(PathBuf);
    impl Home {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir()
                .join(format!("ophio-agent-program-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
        fn program(&self, folder: &str, name: &str) -> PathBuf {
            let folder = self.0.join(folder);
            std::fs::create_dir_all(&folder).unwrap();
            let program = folder.join(name);
            std::fs::write(&program, b"#!/bin/sh\n").unwrap();
            program
        }
        fn write(&self, file: &str, text: &str) {
            let file = self.0.join(file);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, text).unwrap();
        }
        fn node(&self, version: &str) -> PathBuf {
            self.program(&format!(".nvm/versions/node/{version}/bin"), "node")
                .parent()
                .unwrap()
                .to_path_buf()
        }
    }
    impl Drop for Home {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn path_comes_first_then_the_installers_folders_in_order() {
        let home = Home::new("order");
        let on_path = home.program("tools", "claude");
        let path = OsString::from(home.0.join("tools"));
        let found = |name| find(name, Some(&path), Some(&home.0));
        assert_eq!(found("claude"), Some(on_path));
        assert_eq!(found("codex"), None);

        // Where tasks looked before keeps its order: ~/.local/bin, then
        // ~/.cargo/bin, then the folders added after them.
        assert_eq!(
            HOME_FOLDERS,
            [
                ".local/bin",
                ".cargo/bin",
                ".claude/local",
                ".npm-global/bin"
            ]
        );
        // Each folder is found on its own, and an earlier one wins.
        let mut expected = Vec::new();
        for folder in HOME_FOLDERS.iter().rev() {
            expected.push(home.program(folder, "codex"));
            assert_eq!(found("codex").as_ref(), expected.last(), "{folder}");
        }
        assert_eq!(
            folders(Some(&path), Some(&home.0)),
            [home.0.join("tools")]
                .into_iter()
                .chain(HOME_FOLDERS.map(|folder| home.0.join(folder)))
                .collect::<Vec<_>>(),
            "without nvm, nothing more"
        );
        // Not a folder of programs, and no home: only PATH.
        assert_eq!(find("codex", Some(&path), None), None);
        assert_eq!(find("codex", None, None), None);
    }

    #[test]
    fn nvm_s_default_node_is_searched_last() {
        let home = Home::new("nvm-last");
        let older = home.node("v20.11.1");
        home.program(".nvm/versions/node/v20.11.1/bin", "codex");
        assert_eq!(folders(None, Some(&home.0)).last(), Some(&older));
        assert_eq!(
            find("codex", None, Some(&home.0)),
            Some(older.join("codex"))
        );
        let local = home.program(".local/bin", "codex");
        assert_eq!(find("codex", None, Some(&home.0)), Some(local));
    }

    #[test]
    fn nvm_s_default_alias_names_the_version() {
        let home = Home::new("nvm-alias");
        let bin = |home: &Home| folders(None, Some(&home.0)).pop();
        // No default alias: the newest installed, by number, not by name.
        let v9 = home.node("v9.11.2");
        let v22 = home.node("v22.3.0");
        let v26 = home.node("v26.13.0");
        home.node("not-a-version");
        assert_eq!(bin(&home), Some(v26.clone()));

        for (default, expected) in [
            ("v22.3.0\n", &v22),
            ("22", &v22),
            ("22.3", &v22),
            ("9", &v9),
            ("node", &v26),
            ("stable", &v26),
            // A version that isn't installed, or an alias nvm can't follow.
            ("18", &v26),
            ("lts/unknown", &v26),
            ("../../../etc/passwd", &v26),
        ] {
            home.write(".nvm/alias/default", default);
            assert_eq!(bin(&home).as_ref(), Some(expected), "default {default:?}");
        }

        // nvm's own chain for `nvm install --lts`: default → lts/* → lts/jod.
        home.write(".nvm/alias/default", "lts/*");
        home.write(".nvm/alias/lts/*", "lts/jod");
        home.write(".nvm/alias/lts/jod", "v22.3.0");
        assert_eq!(bin(&home), Some(v22));

        // Node.js left to the system: no nvm folder at all.
        home.write(".nvm/alias/default", "system");
        assert_eq!(bin(&home), Some(home.0.join(".npm-global/bin")));
    }

    #[test]
    fn a_program_found_outside_path_starts_with_its_folder_on_path() {
        let path = std::env::join_paths(["/usr/bin", "/bin"]).unwrap();
        assert_eq!(
            path_for(Path::new("/usr/bin/claude"), Some(&path)),
            None,
            "already on PATH"
        );
        let added = path_for(
            Path::new("/home/you/.nvm/versions/node/v22.3.0/bin/codex"),
            Some(&path),
        )
        .unwrap();
        assert_eq!(
            std::env::split_paths(&added).collect::<Vec<_>>(),
            [
                "/usr/bin",
                "/bin",
                "/home/you/.nvm/versions/node/v22.3.0/bin"
            ]
            .map(PathBuf::from),
            "added at the end, so it never shadows the computer's own programs"
        );
        // No PATH at all: the system's folders still come first.
        for unset in [None, Some(OsStr::new(""))] {
            assert_eq!(
                path_for(Path::new("/home/you/.local/bin/claude"), unset),
                Some(OsString::from(
                    "/usr/local/bin:/usr/bin:/bin:/home/you/.local/bin"
                )),
                "{unset:?}"
            );
        }
        assert_eq!(path_for(Path::new("claude"), Some(&path)), None);
    }
}
