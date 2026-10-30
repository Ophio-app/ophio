// SPDX-License-Identifier: Apache-2.0
//! Øphio's own copies of the agents' programs, kept up to date when the owner
//! turns that on. Each agent's copies sit in a folder of their own, whose
//! [`CURRENT`] file names the program to start. A harness reads it each time
//! it starts the agent, so a copy replaced in the background is used from the
//! next run on, and a folder without one leaves the computer's program.
use async_trait::async_trait;
use protocol::capability::HarnessKind;
use std::path::{Component, Path, PathBuf};

/// In an agent's folder: the program to start, as a path inside the folder.
pub const CURRENT: &str = "current";
/// Beside a copied program, so whatever starts it knows it is Øphio's copy.
/// It holds the program's size in bytes, so a copy cut short isn't started.
pub const MARKER: &str = ".ophio-copy";
/// What Øphio's copy of Claude Code starts with. Its own updater would
/// install over the computer's Claude Code, and Øphio updates the copy.
pub const CLAUDE_ENV: [(&str, &str); 2] = [
    ("DISABLE_AUTOUPDATER", "1"),
    ("DISABLE_INSTALLATION_CHECKS", "1"),
];

/// The program Øphio's copy in `folder` starts, if it has one now that can
/// start: a plain file inside the folder, reached without links, runnable,
/// and as large as when it was copied. Anything else leaves the computer's
/// program.
pub fn current(folder: &Path) -> Option<PathBuf> {
    let named = std::fs::read_to_string(folder.join(CURRENT)).ok()?;
    let relative = Path::new(named.trim_end_matches(['\r', '\n']));
    let mut program = folder.to_path_buf();
    for part in relative.components() {
        let Component::Normal(part) = part else {
            return None;
        };
        program.push(part);
        // A link on the way could lead out of the folder.
        if std::fs::symlink_metadata(&program)
            .ok()?
            .file_type()
            .is_symlink()
        {
            return None;
        }
    }
    let metadata = std::fs::symlink_metadata(&program).ok()?;
    let copied = std::fs::read_to_string(program.parent()?.join(MARKER)).ok()?;
    (metadata.is_file() && runnable(&metadata) && copied.trim().parse() == Ok(metadata.len()))
        .then_some(program)
}

#[cfg(unix)]
fn runnable(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o111 != 0
}
#[cfg(not(unix))]
fn runnable(_: &std::fs::Metadata) -> bool {
    true
}

/// Whether `program` is one of Øphio's copies.
pub fn is_copy(program: &Path) -> bool {
    program
        .parent()
        .is_some_and(|folder| folder.join(MARKER).is_file())
}

/// Where one agent's copy stands after [`AgentUpdater::apply`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyState {
    pub harness: HarnessKind,
    /// The version tasks start with, `None` with no copy.
    pub version: Option<String>,
    /// The agent's maker was asked for its newest release, and answered.
    pub checked: bool,
    /// Why the check or download didn't finish, in words for the owner.
    pub problem: Option<String>,
}

#[async_trait]
pub trait AgentUpdater: Send + Sync + std::fmt::Debug + 'static {
    /// Whether the agents' makers publish programs for this computer's system.
    fn supported(&self) -> bool;
    /// Brings the copies in line with the setting. On, each agent installed
    /// on the computer, or copied before, gets its maker's newest release;
    /// off, tasks go back to the computer's programs and the copies go.
    async fn apply(&self, keep: bool) -> Vec<CopyState>;
    /// The program and environment that start Øphio's copy of an agent in a
    /// terminal, when there is one.
    fn program(&self, kind: HarnessKind) -> Option<(PathBuf, Vec<(String, String)>)>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_names_only_a_program_inside_the_folder() {
        let dir = std::env::temp_dir().join(format!("ophio-copy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let folder = dir.join("claude");
        std::fs::create_dir_all(folder.join("1.0.0")).unwrap();
        let program = folder.join("1.0.0/claude");
        std::fs::write(&program, b"#!/bin/sh\n").unwrap();
        runnable_by_owner(&program, true);
        std::fs::write(dir.join("outside"), b"").unwrap();
        std::fs::write(folder.join(CURRENT), "1.0.0/claude\n").unwrap();
        assert_eq!(
            current(&folder),
            None,
            "no marker: not one of Øphio's copies"
        );
        assert!(!is_copy(&program));
        std::fs::write(folder.join("1.0.0").join(MARKER), b"10").unwrap();
        assert!(is_copy(&program));
        assert_eq!(current(&folder), Some(program.clone()));
        for named in ["../outside", "/etc/passwd", "", "1.0.0/missing", "1.0.0"] {
            std::fs::write(folder.join(CURRENT), named).unwrap();
            assert_eq!(current(&folder), None, "{named}");
        }
        std::fs::write(folder.join(CURRENT), "1.0.0/claude").unwrap();
        std::fs::write(folder.join("1.0.0").join(MARKER), b"11").unwrap();
        assert_eq!(current(&folder), None, "a copy cut short isn't started");
        std::fs::write(folder.join("1.0.0").join(MARKER), b"10").unwrap();
        #[cfg(unix)]
        {
            runnable_by_owner(&program, false);
            assert_eq!(
                current(&folder),
                None,
                "a program that can't run isn't started"
            );
            runnable_by_owner(&program, true);
            // Links, to the program or on the way to it, could lead outside.
            std::os::unix::fs::symlink(&program, folder.join("1.0.0/linked")).unwrap();
            std::fs::write(folder.join(CURRENT), "1.0.0/linked").unwrap();
            assert_eq!(current(&folder), None);
            std::os::unix::fs::symlink(folder.join("1.0.0"), folder.join("2.0.0")).unwrap();
            std::fs::write(folder.join(CURRENT), "2.0.0/claude").unwrap();
            assert_eq!(current(&folder), None);
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    fn runnable_by_owner(program: &Path, runs: bool) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = if runs { 0o700 } else { 0o600 };
            std::fs::set_permissions(program, std::fs::Permissions::from_mode(mode)).unwrap();
        }
        #[cfg(not(unix))]
        let _ = (program, runs);
    }
}
