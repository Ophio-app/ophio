// SPDX-License-Identifier: Apache-2.0
//! Markdown Library file access under explicitly granted roots.
//!
//! Paths crossing this boundary are root-relative, `/`-separated and already
//! syntactically normalized by the caller; implementations still re-validate
//! containment (symlinks/reparse points, `..`, case and Unicode aliases)
//! against the real filesystem before every operation.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use tokio::sync::broadcast;

use protocol::capability::HarnessKind;
use protocol::ids::{LibraryRootId, ProjectId};
use protocol::library::{
    DirListing, FileContent, InstructionSource, LibraryRoot, NewlineStyle, RootKind, SaveOutcome,
    SearchHit,
};

/// Root configuration the owner granted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootSpec {
    pub root_id: LibraryRootId,
    pub label: String,
    pub path: PathBuf,
    pub kind: RootKind,
    pub writable: bool,
    pub project_id: Option<ProjectId>,
    pub harness: Option<HarnessKind>,
    /// When set, the only paths the root reaches: these root-relative
    /// entries, each a file or a folder with everything in it. The folders
    /// on the way to one are listed, holding nothing else. Unset, the root
    /// reaches everything in it.
    pub only: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaveRequest {
    pub text: String,
    pub base_revision: String,
    pub newline: NewlineStyle,
    pub bom: bool,
}

/// Where a deleted file went, so it can be restored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrashEntry {
    pub trash_id: String,
    pub root_id: LibraryRootId,
    pub original_path: String,
    pub revision: String,
    pub stored_at: PathBuf,
}

/// A change noticed on disk under a root (by anyone, including us).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryChange {
    pub root_id: LibraryRootId,
    pub path: String,
    /// `None` = no longer exists.
    pub revision: Option<String>,
}

#[async_trait]
pub trait LibraryFs: Send + Sync + std::fmt::Debug + 'static {
    /// Replace the set of granted roots (from persisted configuration).
    async fn set_roots(&self, roots: Vec<RootSpec>) -> Result<(), LibraryError>;
    async fn roots(&self) -> Vec<LibraryRoot>;

    async fn list(&self, root: LibraryRootId, dir: &str) -> Result<DirListing, LibraryError>;
    async fn read(&self, root: LibraryRootId, path: &str) -> Result<FileContent, LibraryError>;
    /// The file itself, to send its exact bytes whatever they are. Confined to
    /// the root like `read`, but neither decoded as text nor capped at the
    /// editing size.
    async fn open(&self, _root: LibraryRootId, _path: &str) -> Result<std::fs::File, LibraryError> {
        Err(LibraryError::NotFound)
    }
    /// Compare-then-replace against `base_revision`. Never overwrites a file
    /// whose bytes differ from the base without retaining those bytes for
    /// recovery. Native replacement racing an external writer must report
    /// `SaveOutcome::Uncertain`, never a false no-conflict success.
    async fn save(
        &self,
        root: LibraryRootId,
        path: &str,
        request: SaveRequest,
    ) -> Result<SaveOutcome, LibraryError>;
    /// Create a new file; fails if anything exists at `path`. Returns revision.
    async fn create(
        &self,
        root: LibraryRootId,
        path: &str,
        text: &str,
    ) -> Result<String, LibraryError>;
    /// Rename/move within a root; fails if the target exists or the source
    /// no longer matches `base_revision`.
    async fn rename(
        &self,
        root: LibraryRootId,
        from: &str,
        to: &str,
        base_revision: &str,
    ) -> Result<String, LibraryError>;
    /// Move to the companion-owned trash (recoverable).
    async fn trash(
        &self,
        root: LibraryRootId,
        path: &str,
        base_revision: &str,
    ) -> Result<TrashEntry, LibraryError>;
    async fn restore(&self, trash_id: &str) -> Result<LibraryChange, LibraryError>;

    async fn search(
        &self,
        root: Option<LibraryRootId>,
        text: &str,
        limit: u32,
    ) -> Result<Vec<SearchHit>, LibraryError>;

    /// Instruction files a harness discovers for a project directory, with
    /// their current revisions. `loaded` lists host paths the harness reported
    /// loading, when it does. `project_settings` is whether runs apply the
    /// settings committed in the project folder.
    async fn instruction_sources(
        &self,
        project_dir: &Path,
        harness: HarnessKind,
        loaded: &[PathBuf],
        project_settings: bool,
    ) -> Vec<InstructionSource>;

    async fn recoverable(&self) -> Result<Vec<protocol::library::LibraryTrashItem>, LibraryError> {
        Err(LibraryError::NotFound)
    }
    /// Removes deleted documents kept past their recovery window. The
    /// companion calls this with its hourly transfer collection.
    async fn purge(&self) -> Result<(), LibraryError> {
        Ok(())
    }
    async fn read_trash(&self, _id: &str) -> Result<FileContent, LibraryError> {
        Err(LibraryError::NotFound)
    }
    async fn restore_checked(
        &self,
        _id: &str,
        _revision: &str,
        _copy_path: Option<&str>,
    ) -> Result<LibraryChange, LibraryError> {
        Err(LibraryError::NotFound)
    }

    fn changes(&self) -> broadcast::Receiver<LibraryChange>;

    async fn list_recovery(
        &self,
    ) -> Result<Vec<protocol::library::LibraryRecoveryItem>, LibraryError> {
        Err(LibraryError::NotFound)
    }
    async fn inspect_recovery(
        &self,
        _root: LibraryRootId,
        _id: &str,
    ) -> Result<protocol::library::LibraryRecovery, LibraryError> {
        Err(LibraryError::NotFound)
    }
    /// Explicit cleanup only; the root must still match the snapshot's identity.
    async fn discard_recovery_checked(
        &self,
        _root: LibraryRootId,
        _id: &str,
    ) -> Result<(), LibraryError> {
        Err(LibraryError::NotFound)
    }
}

/// A Unicode direction mark, embedding, override or isolate. Unseen, it
/// reorders the text around it, so `photo<U+202E>gnp.exe` reads as
/// `photoexe.png`. The joiners and variation selectors emoji use are not.
pub fn direction_control(c: char) -> bool {
    matches!(c, '\u{061C}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

/// Whether Windows keeps `name` for a device, with or without an extension:
/// `CON`, `nul.txt`, `COM1`, `LPT²`. No file can be made or opened under such
/// a name there, so neither a Library path nor a transferred file uses one.
/// Windows reads the device from the name up to its first dot or colon, less
/// trailing spaces, so `con .txt` and `NUL:` name devices too.
pub fn reserved_on_windows(name: &str) -> bool {
    let stem = name
        .split(['.', ':'])
        .next()
        .unwrap_or("")
        .trim_end_matches(' ')
        .to_ascii_uppercase();
    matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || ["COM", "LPT"].iter().any(|p| {
        stem.strip_prefix(p).is_some_and(|s| {
            matches!(
                s,
                "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            )
        })
    })
}

#[derive(Debug, thiserror::Error)]
pub enum LibraryError {
    #[error("unknown library root")]
    UnknownRoot,
    #[error("path escapes its root or is not allowed: {0}")]
    PathNotAllowed(String),
    #[error("not found")]
    NotFound,
    #[error("already exists")]
    AlreadyExists,
    #[error("root is read-only")]
    ReadOnly,
    #[error("file changed since it was read")]
    Conflict,
    /// A filesystem mutation started but its final state could not be proven.
    /// Keep the client draft and re-read before retrying.
    #[error("filesystem outcome requires reconciliation")]
    OutcomeUnknown,
    #[error("not an editable UTF-8 Markdown file")]
    NotEditable,
    #[error("file too large ({0} bytes)")]
    TooLarge(u64),
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
}

impl LibraryError {
    pub fn code(&self) -> protocol::ErrorCode {
        use protocol::ErrorCode;
        match self {
            LibraryError::UnknownRoot | LibraryError::NotFound => ErrorCode::NotFound,
            LibraryError::PathNotAllowed(_) => ErrorCode::PathNotAllowed,
            LibraryError::ReadOnly => ErrorCode::PermissionDenied,
            LibraryError::AlreadyExists | LibraryError::Conflict => ErrorCode::Conflict,
            LibraryError::OutcomeUnknown => ErrorCode::OutcomeUnknown,
            LibraryError::NotEditable | LibraryError::TooLarge(_) => ErrorCode::InvalidRequest,
            LibraryError::Io(_) => ErrorCode::NativeFailure,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{canonical, reserved_on_windows, resolved};

    #[test]
    fn device_names_are_reserved_however_windows_spells_them() {
        for name in [
            "CON",
            "prn",
            "Aux",
            "NUL",
            "COM1",
            "com9",
            "LPT1",
            "lpt9",
            "COM²",
            "CONIN$",
            "nul.txt",
            "Com1.tar.gz",
            "CON.",
            "CON..",
            "CON ",
            "con .txt",
            "LPT3  .md",
            "AUX. ",
            "NUL:",
            "nul:stream",
        ] {
            assert!(reserved_on_windows(name), "{name:?}");
        }
    }

    #[test]
    fn names_that_only_resemble_a_device_are_not_reserved() {
        for name in [
            "console.txt",
            "CONX",
            "COM0",
            "COM10",
            "LPT",
            "lpt10.md",
            " CON",
            "my CON.txt",
            "NUL-notes.md",
            "",
        ] {
            assert!(!reserved_on_windows(name), "{name:?}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_missing_folder_under_a_link_resolves_to_where_it_would_be_made() {
        let base = std::env::temp_dir().join(format!("ophio-resolved-{}", std::process::id()));
        let own = base.join("own");
        std::fs::create_dir_all(&own).unwrap();
        std::os::unix::fs::symlink(&own, base.join("link")).unwrap();
        let real = canonical(&own).unwrap();
        assert_eq!(
            resolved(&base.join("link/new/Ophio")).unwrap(),
            real.join("new/Ophio")
        );
        assert_eq!(resolved(&base.join("link")).unwrap(), real);
        // A missing part can't be stepped back out of.
        assert!(resolved(&base.join("link/new/../x")).is_err());
        std::fs::remove_dir_all(&base).unwrap();
    }
}

/// Where `path` leads, named as people and other programs name it. Windows
/// writes a folder's canonical form with a `\\?\` prefix that a harness's
/// working directory, a session's recorded folder and a typed path all lack,
/// so a drive path comes back without it wherever that names the same place.
/// Whether a root at `root`, limited to `only` or to nothing when it is
/// unset, reaches `path` on the computer or anything in it.
pub fn meets(root: &Path, only: Option<&[String]>, path: &Path) -> bool {
    match path.strip_prefix(root) {
        Ok(rest) => rest.to_str().is_none_or(|rest| {
            protocol::library::reaches(only, &rest.replace(std::path::MAIN_SEPARATOR, "/"))
        }),
        Err(_) => root.starts_with(path),
    }
}

pub fn canonical(path: &Path) -> std::io::Result<PathBuf> {
    std::fs::canonicalize(path).map(plain)
}

/// Where `path` really is, though its end may not exist yet: the deepest part
/// that exists, its links followed, with the missing rest after it. A missing
/// folder under a link is checked where it would be made.
pub fn resolved(path: &Path) -> std::io::Result<PathBuf> {
    use std::path::Component;
    let mut missing = Vec::new();
    let mut existing = path;
    loop {
        match canonical(existing) {
            Ok(mut real) => {
                for part in missing.iter().rev() {
                    real.push(part);
                }
                return Ok(real);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        match (existing.components().next_back(), existing.parent()) {
            (Some(Component::Normal(part)), Some(parent)) => {
                missing.push(part.to_owned());
                existing = parent;
            }
            _ => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("{} can’t be followed to where it would be", path.display()),
                ));
            }
        }
    }
}

/// `path` without Windows' `\\?\` prefix where that names the same place;
/// any other path as it is.
pub fn plain(path: PathBuf) -> PathBuf {
    #[cfg(windows)]
    {
        if let Some(rest) = path.to_str().and_then(plain_drive_path) {
            return PathBuf::from(rest);
        }
    }
    path
}

/// The drive path a `\\?\` path names, when Windows reads it the same way
/// without the prefix: short enough, and no part a device name, ending in a
/// dot or space, or holding a character only the prefix allows.
#[cfg_attr(not(windows), allow(dead_code))]
fn plain_drive_path(path: &str) -> Option<&str> {
    let rest = path.strip_prefix(r"\\?\")?;
    let drive = rest.as_bytes();
    if drive.len() < 3 || !drive[0].is_ascii_alphabetic() || &drive[1..3] != b":\\" {
        return None;
    }
    if rest.len() >= 260 {
        return None;
    }
    let ordinary = |part: &str| {
        !part.is_empty()
            && !reserved_on_windows(part)
            && !part.ends_with(['.', ' '])
            && !part.contains(|c: char| c.is_control() || "<>:\"/|?*".contains(c))
    };
    let parts = &rest[3..];
    (parts.is_empty() || parts.split('\\').all(ordinary)).then_some(rest)
}

#[cfg(test)]
mod plain_tests {
    use super::plain_drive_path;

    #[test]
    fn only_a_drive_path_windows_reads_the_same_way_loses_its_prefix() {
        let plain = [
            (r"\\?\C:\work\project", r"C:\work\project"),
            (r"\\?\d:\", r"d:\"),
            (r"\\?\C:\notes\console\icon.d", r"C:\notes\console\icon.d"),
            (r"\\?\C:\a€", r"C:\a€"),
            (r"\\?\C:\cöm1\€uro", r"C:\cöm1\€uro"),
            (r"\\?\C:\work\com10", r"C:\work\com10"),
            (r"\\?\C:\work\com0", r"C:\work\com0"),
        ];
        for (path, expected) in plain {
            assert_eq!(plain_drive_path(path), Some(expected), "{path}");
        }
        let long = format!(r"\\?\C:\{}", "a".repeat(260));
        let kept = [
            r"C:\work",
            r"\\?\UNC\server\share\project",
            r"\\?\Volume{0}\project",
            r"\\?\C:\trailing.\project",
            r"\\?\C:\trailing \project",
            r"\\?\C:\work\con",
            r"\\?\C:\work\con.d",
            r"\\?\C:\work\NUL.txt",
            r"\\?\C:\work\com1",
            r"\\?\C:\work\lpt9.log",
            r"\\?\C:\work\LPT².txt",
            r"\\?\C:\work\com³",
            r"\\?\C:\work\a|b",
            r"\\?\C:\work\\double",
            &long,
        ];
        for path in kept {
            assert_eq!(plain_drive_path(path), None, "{path}");
        }
    }
}
