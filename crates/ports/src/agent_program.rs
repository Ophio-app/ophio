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
//! Windows has a lookup of its own, [`windows_find`], with its own folders,
//! and finds only real programs (`.exe`): behind npm's `.cmd` scripts, the
//! program in the npm package.
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

/// Folders under the home folder where Windows installers put agents for a
/// person without admin rights, in the order they are searched:
///
/// - `.local/bin`: Claude Code's own installer (`claude.exe`).
/// - `.cargo/bin`: programs installed with Cargo.
/// - `AppData/Local/Programs`: per-person installs.
/// - `AppData/Roaming/npm`: npm's global packages (`%APPDATA%\npm`).
/// - `AppData/Local/Programs/OpenAI/Codex/bin`: Codex's own installer
///   (`codex.exe`), which adds it to `PATH` only from the next login.
///
/// The first four, in that order, are where tasks looked before.
pub const WINDOWS_HOME_FOLDERS: [&str; 5] = [
    ".local/bin",
    ".cargo/bin",
    "AppData/Local/Programs",
    "AppData/Roaming/npm",
    "AppData/Local/Programs/OpenAI/Codex/bin",
];

/// Where npm puts each agent's real program, under the `node_modules` folder
/// beside npm's `<name>.cmd` script, in the order they are tried. npm's
/// script starts Node.js, which starts this program; starting it directly
/// is what Øphio's own downloaded copies do.
///
/// - `codex`: `@openai/codex`'s `bin/codex.js` starts the program in the
///   package for this computer, `@openai/codex-win32-x64`, which it finds
///   with Node.js's `require`: inside `@openai/codex`'s own `node_modules`,
///   or hoisted beside it.
/// - `claude`: `@anthropic-ai/claude-code` names its program, `bin/claude.exe`,
///   as its `bin`.
const NPM_PROGRAMS: [(&str, &[&str]); 2] = [
    (
        "codex",
        &[
            "@openai/codex/node_modules/@openai/codex-win32-x64/vendor/x86_64-pc-windows-msvc/bin/codex.exe",
            "@openai/codex-win32-x64/vendor/x86_64-pc-windows-msvc/bin/codex.exe",
        ],
    ),
    ("claude", &["@anthropic-ai/claude-code/bin/claude.exe"]),
];

/// On Windows, the program called `name` in the first folder that has it:
/// each folder on `path`, then [`WINDOWS_HOME_FOLDERS`] under each of
/// `homes` in turn. Only a real program is found, never a script, so that
/// Øphio starts, and contains, the agent itself (see [`windows_program_in`]).
/// An empty or relative entry on `path`, such as one a trailing `;` leaves,
/// is skipped, as Windows' own program launcher in Rust skips it: it would
/// name the current folder, which for Øphio is its own program's.
///
/// Written for any computer, so that it can be tested anywhere; only
/// Windows uses it.
pub fn windows_find(name: &str, path: Option<&OsStr>, homes: &[PathBuf]) -> Option<PathBuf> {
    let on_path = path
        .into_iter()
        .flat_map(std::env::split_paths)
        .filter(|folder| folder.is_absolute());
    let in_homes = homes
        .iter()
        .flat_map(|home| WINDOWS_HOME_FOLDERS.map(|folder| joined(home, folder)));
    on_path
        .chain(in_homes)
        .find_map(|folder| windows_program_in(&folder, name))
}

/// [`windows_find`] with this process's own `PATH` and home folders: `HOME`,
/// when set, then `USERPROFILE`, as tasks have always looked.
pub fn on_this_windows(name: &str) -> Option<PathBuf> {
    let homes = windows_homes(|name| std::env::var_os(name));
    windows_find(name, std::env::var_os("PATH").as_deref(), &homes)
}

/// The home folders [`windows_find`] searches, from the environment `var`
/// reads: `HOME`, when set, then `USERPROFILE`.
pub fn windows_homes(var: impl Fn(&str) -> Option<OsString>) -> Vec<PathBuf> {
    let mut homes: Vec<PathBuf> = Vec::new();
    for home in ["HOME", "USERPROFILE"]
        .into_iter()
        .filter_map(var)
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
    {
        if !homes.contains(&home) {
            homes.push(home);
        }
    }
    homes
}

/// The Windows program `name` in `folder`: `<name>.exe`, or else, when npm's
/// `<name>.cmd` script is there, the program in the npm package it starts
/// (see [`NPM_PROGRAMS`]). Never the script itself.
pub fn windows_program_in(folder: &Path, name: &str) -> Option<PathBuf> {
    let program = folder.join(format!("{name}.exe"));
    if program.is_file() {
        return Some(program);
    }
    if !folder.join(format!("{name}.cmd")).is_file() {
        return None;
    }
    let packages = folder.join("node_modules");
    NPM_PROGRAMS
        .iter()
        .filter(|(agent, _)| *agent == name)
        .flat_map(|(_, programs)| programs.iter())
        .map(|program| joined(&packages, program))
        .find(|program| program.is_file())
}

/// `folder` with `relative`'s parts, written with `/`, added one at a time,
/// so that the path has the computer's own separators.
fn joined(folder: &Path, relative: &str) -> PathBuf {
    let mut path = folder.to_path_buf();
    path.extend(relative.split('/'));
    path
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

    const NESTED: &str = "node_modules/@openai/codex/node_modules/@openai/codex-win32-x64/vendor/x86_64-pc-windows-msvc/bin";
    const HOISTED: &str = "node_modules/@openai/codex-win32-x64/vendor/x86_64-pc-windows-msvc/bin";

    #[test]
    fn windows_finds_the_program_behind_npm_s_script() {
        let home = Home::new("windows-npm");
        let npm = "AppData/Roaming/npm";
        let homes = [home.0.clone()];
        let found = |name| windows_find(name, None, &homes);

        // npm's script alone, with no package behind it: nothing to start.
        home.program(npm, "codex.cmd");
        home.program(npm, "codex.ps1");
        home.program(npm, "codex");
        assert_eq!(found("codex"), None);

        // The program hoisted beside @openai/codex, then inside its own
        // node_modules: the newest layout wins.
        let hoisted = home.program(&format!("{npm}/{HOISTED}"), "codex.exe");
        assert_eq!(found("codex"), Some(hoisted));
        let nested = home.program(&format!("{npm}/{NESTED}"), "codex.exe");
        assert_eq!(found("codex"), Some(nested.clone()));
        assert_eq!(
            nested,
            home.0.join(npm).join(NESTED).join("codex.exe"),
            "in the computer's own separators"
        );

        // Claude Code installed with npm.
        home.program(npm, "claude.cmd");
        assert_eq!(found("claude"), None);
        let claude = home.program(
            &format!("{npm}/node_modules/@anthropic-ai/claude-code/bin"),
            "claude.exe",
        );
        assert_eq!(found("claude"), Some(claude));

        // A program called `codex.exe` beside the script comes first.
        let beside = home.program(npm, "codex.exe");
        assert_eq!(found("codex"), Some(beside));
        // Only the agents' packages are known.
        home.program(npm, "other.cmd");
        assert_eq!(found("other"), None);
    }

    #[test]
    fn windows_searches_path_then_the_installers_folders() {
        let home = Home::new("windows-order");
        let homes = [home.0.clone()];
        let tools = home.0.join("tools");
        let path = std::env::join_paths([&tools]).unwrap();
        let found = |name| windows_find(name, Some(&path), &homes);
        assert_eq!(found("claude"), None);

        // Each home folder is found on its own, and an earlier one wins.
        let mut expected = Vec::new();
        for folder in WINDOWS_HOME_FOLDERS.iter().rev() {
            expected.push(home.program(folder, "claude.exe"));
            assert_eq!(found("claude").as_ref(), expected.last(), "{folder}");
        }
        assert_eq!(
            expected.last(),
            Some(&home.0.join(".local").join("bin").join("claude.exe")),
            "Claude Code's own installer's folder comes first"
        );
        // Where tasks looked before keeps its order, and Codex's own
        // installer's folder comes after them.
        assert_eq!(
            WINDOWS_HOME_FOLDERS,
            [
                ".local/bin",
                ".cargo/bin",
                "AppData/Local/Programs",
                "AppData/Roaming/npm",
                "AppData/Local/Programs/OpenAI/Codex/bin",
            ]
        );
        let codex = home.program("AppData/Local/Programs/OpenAI/Codex/bin", "codex.exe");
        assert_eq!(found("codex"), Some(codex));

        // PATH comes before the home folder, and npm's packages count in a
        // folder on PATH too.
        home.program("tools", "claude.cmd");
        let on_path = home.program(
            "tools/node_modules/@anthropic-ai/claude-code/bin",
            "claude.exe",
        );
        assert_eq!(found("claude"), Some(on_path));
        let program = home.program("tools", "claude.exe");
        assert_eq!(found("claude"), Some(program));

        // A relative entry on PATH, as an empty one names the current
        // folder, is never searched: here, the same folder written relative
        // to the current one.
        let folder = home.program("relative", "standin.exe");
        let folder = folder.parent().unwrap();
        let normal = |path: &Path| -> Vec<OsString> {
            path.components()
                .filter_map(|c| match c {
                    Component::Normal(part) => Some(part.to_owned()),
                    _ => None,
                })
                .collect()
        };
        let up = normal(&std::env::current_dir().unwrap()).len();
        let relative: PathBuf = std::iter::repeat_n(OsString::from(".."), up)
            .chain(normal(folder))
            .collect();
        assert!(relative.is_relative() && relative.join("standin.exe").is_file());
        let joined = |entries: &[&Path]| std::env::join_paths(entries).unwrap();
        assert_eq!(
            windows_find("standin", Some(&joined(&[&relative])), &[]),
            None
        );
        assert_eq!(
            windows_find("standin", Some(&joined(&[Path::new(""), folder])), &[]),
            Some(folder.join("standin.exe")),
            "an empty entry is passed over"
        );

        // A program without `.exe` isn't one Windows starts.
        home.program(".local/bin", "other");
        assert_eq!(found("other"), None);
        // No home: only PATH.
        assert_eq!(windows_find("codex", Some(&path), &[]), None);
        assert_eq!(windows_find("codex", None, &[]), None);
    }

    #[test]
    fn windows_homes_are_home_then_the_user_profile() {
        let homes = |home: Option<&str>, profile: Option<&str>| {
            windows_homes(|name| match name {
                "HOME" => home.map(OsString::from),
                "USERPROFILE" => profile.map(OsString::from),
                _ => None,
            })
        };
        let you = r"C:\Users\you";
        assert_eq!(homes(None, Some(you)), [PathBuf::from(you)]);
        assert_eq!(homes(Some(you), Some(you)), [PathBuf::from(you)]);
        assert_eq!(
            homes(Some("/home/you"), Some(you)),
            [PathBuf::from("/home/you"), PathBuf::from(you)]
        );
        assert_eq!(homes(Some(""), None), Vec::<PathBuf>::new());
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
