// SPDX-License-Identifier: Apache-2.0
//! Which files a sandboxed agent writes are only its conversation's own.
//!
//! A sandboxed run's agent works in a settings folder of its conversation's,
//! which it sees where its real one is (Linux) or at the folder's own place
//! (a Mac). A note it writes there changes nothing of the person's, so a
//! receipt that listed it would claim a change that didn't happen. A few
//! paths in that folder are the person's own, though: the sign-in file and
//! the instructions and skills shared read-only, and this conversation's
//! transcript, which is copied back to the real folder once the run ends.
//! Those, and anything not known for certain, are not the agent's own.
use protocol::capability::HarnessKind;
use std::{
    os::unix::fs::MetadataExt,
    path::{Component, Path, PathBuf},
};

/// Where a sandboxed run's own settings are, recorded as its sandbox is built.
#[derive(Debug, Clone)]
pub(crate) struct OwnSettings {
    pub harness: HarnessKind,
    /// The agent's settings folder, as the agent names it.
    pub folder: PathBuf,
    /// Other private files the agent finds outside that folder, as it names
    /// them, such as Claude Code's account file.
    pub files: Vec<PathBuf>,
    /// Where a path the agent names really is on this computer: each place
    /// as the agent sees it, and what is there. The longest that holds a
    /// path decides, as the innermost mount does.
    pub places: Vec<(PathBuf, PathBuf)>,
    /// Folders and files on this computer that hold only this conversation's
    /// own state, with no link in their paths.
    pub own: Vec<PathBuf>,
    /// The working folder, for a path the agent names relative to it.
    pub cwd: PathBuf,
}

impl OwnSettings {
    /// Whether `path`, as the agent named it, is a file only of this
    /// conversation's own settings. False whenever that isn't certain.
    pub fn holds(&self, path: &Path) -> bool {
        let path = if path.is_absolute() {
            path.to_owned()
        } else {
            self.cwd.join(path)
        };
        // `..` or `.` could step out of what is compared below.
        if path
            .components()
            .any(|c| !matches!(c, Component::RootDir | Component::Normal(_)))
        {
            return false;
        }
        match path.strip_prefix(&self.folder) {
            Ok(relative) => {
                if relative.as_os_str().is_empty() || carried(self.harness, relative) {
                    return false;
                }
            }
            Err(_) if self.files.contains(&path) => {}
            Err(_) => return false,
        }
        let Some((seen, real)) = self
            .places
            .iter()
            .filter(|(seen, _)| path.starts_with(seen))
            .max_by_key(|(seen, _)| seen.components().count())
        else {
            return false;
        };
        let Ok(rest) = path.strip_prefix(seen) else {
            return false;
        };
        let real = if rest.as_os_str().is_empty() {
            real.clone()
        } else {
            real.join(rest)
        };
        // Followed through any link, as the agent's write was: a file that
        // is, or shares its contents with, anything elsewhere is not its own.
        let Ok(metadata) = std::fs::metadata(&real) else {
            return false;
        };
        if !metadata.is_file() || metadata.nlink() != 1 {
            return false;
        }
        let Ok(real) = std::fs::canonicalize(&real) else {
            return false;
        };
        self.own.iter().any(|own| real.starts_with(own))
    }
}

/// Files in the agent's settings folder that go back to the person's real
/// one: the sign-in, renewed in a run, and the conversation's transcript,
/// which [`crate::history::History::finished`] copies there.
fn carried(harness: HarnessKind, relative: &Path) -> bool {
    if relative == Path::new(crate::history::credential(harness)) {
        return true;
    }
    let names: Vec<_> = relative.components().collect();
    match harness {
        HarnessKind::Codex => names.first() == Some(&Component::Normal("sessions".as_ref())),
        _ => {
            names.len() == 3
                && names[0] == Component::Normal("projects".as_ref())
                && relative.extension().is_some_and(|e| e == "jsonl")
        }
    }
}

/// The places in `own` that exist, by their real paths.
pub(crate) fn real(own: impl IntoIterator<Item = PathBuf>) -> Vec<PathBuf> {
    own.into_iter()
        .filter_map(|p| std::fs::canonicalize(p).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        _dir: tempfile::TempDir,
        root: PathBuf,
        settings: OwnSettings,
    }
    /// A Linux-like layout: the agent sees `home/.claude`, which is
    /// `state/native`, with its history from `state/claude-history`, its
    /// sign-in and a shared instructions file from the real folder.
    fn fixture(harness: HarnessKind) -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let folder = root.join("home/.claude");
        let native = root.join("state/native");
        let history = root.join("state/claude-history");
        for path in [
            &folder,
            &native,
            &history.join("projects/-p/memory"),
            &root.join("project"),
        ] {
            std::fs::create_dir_all(path).unwrap();
        }
        std::fs::write(folder.join("CLAUDE.md"), "the person's").unwrap();
        std::fs::write(folder.join(".credentials.json"), "{}").unwrap();
        let settings = OwnSettings {
            harness,
            folder: folder.clone(),
            files: vec![],
            places: vec![
                (folder.clone(), native.clone()),
                (folder.join("projects"), history.join("projects")),
                (folder.join("CLAUDE.md"), folder.join("CLAUDE.md")),
                (
                    folder.join(".credentials.json"),
                    folder.join(".credentials.json"),
                ),
            ],
            own: real([native, history]),
            cwd: root.join("project"),
        };
        Fixture {
            _dir: dir,
            root,
            settings,
        }
    }

    #[test]
    fn a_note_in_the_conversations_own_folder_is_its_own() {
        let f = fixture(HarnessKind::ClaudeCode);
        let note = "state/claude-history/projects/-p/memory/note.md";
        std::fs::write(f.root.join(note), "note").unwrap();
        std::fs::write(f.root.join("state/native/settings.json"), "{}").unwrap();
        assert!(
            f.settings
                .holds(&f.root.join("home/.claude/projects/-p/memory/note.md"))
        );
        assert!(f.settings.holds(&f.root.join("home/.claude/settings.json")));
        // Nothing there now: not known, so listed.
        assert!(
            !f.settings
                .holds(&f.root.join("home/.claude/projects/-p/memory/gone.md"))
        );
    }

    #[test]
    fn the_persons_files_and_what_goes_back_are_not() {
        let f = fixture(HarnessKind::ClaudeCode);
        std::fs::write(f.root.join("project/lib.rs"), "x").unwrap();
        std::fs::write(f.root.join("state/native/.credentials.json"), "{}").unwrap();
        std::fs::write(
            f.root
                .join("state/claude-history/projects/-p/session.jsonl"),
            "{}",
        )
        .unwrap();
        for path in [
            "project/lib.rs",
            "home/.claude/CLAUDE.md",
            "home/.claude/.credentials.json",
            "home/.claude/projects/-p/session.jsonl",
        ] {
            assert!(!f.settings.holds(&f.root.join(path)), "{path}");
        }
        assert!(!f.settings.holds(Path::new("lib.rs")));
        assert!(!f.settings.holds(&f.root.join("home/.claude")));
    }

    #[test]
    fn a_link_or_a_shared_file_out_of_the_folder_is_not_its_own() {
        let f = fixture(HarnessKind::ClaudeCode);
        let target = f.root.join("project/lib.rs");
        std::fs::write(&target, "x").unwrap();
        std::os::unix::fs::symlink(&target, f.root.join("state/native/link")).unwrap();
        std::fs::hard_link(&target, f.root.join("state/native/hard")).unwrap();
        std::fs::write(f.root.join("state/native/plain"), "x").unwrap();
        assert!(!f.settings.holds(&f.root.join("home/.claude/link")));
        assert!(!f.settings.holds(&f.root.join("home/.claude/hard")));
        assert!(f.settings.holds(&f.root.join("home/.claude/plain")));
        assert!(
            !f.settings
                .holds(&f.root.join("home/.claude/projects/../plain"))
        );
    }

    #[test]
    fn codex_keeps_its_sessions_and_sign_in_listed() {
        let f = fixture(HarnessKind::Codex);
        std::fs::create_dir_all(f.root.join("state/native/sessions/2026")).unwrap();
        for path in ["sessions/2026/rollout-1.jsonl", "auth.json", "memo.md"] {
            std::fs::write(f.root.join("state/native").join(path), "x").unwrap();
        }
        assert!(
            !f.settings
                .holds(&f.root.join("home/.claude/sessions/2026/rollout-1.jsonl"))
        );
        assert!(!f.settings.holds(&f.root.join("home/.claude/auth.json")));
        assert!(f.settings.holds(&f.root.join("home/.claude/memo.md")));
    }
}
