// SPDX-License-Identifier: Apache-2.0
//! Publish complete downloads without losing their staged bytes on failure.
use std::{fs::File, io, path::Path};

/// Copies a finished download into the task's folder under a safe, unused
/// name. The staged file is removed only after the complete copy is published.
pub(super) async fn deliver(staged: &Path, folder: &Path, suggested: &str) -> io::Result<String> {
    let staged = staged.to_owned();
    let folder = folder.to_owned();
    let suggested = suggested.to_owned();
    tokio::task::spawn_blocking(move || {
        deliver_with(&staged, &folder, &suggested, |source, destination| {
            io::copy(source, destination)
        })
    })
    .await
    .map_err(io::Error::other)?
}

fn deliver_with(
    staged: &Path,
    folder: &Path,
    suggested: &str,
    copy: impl FnOnce(&mut File, &mut File) -> io::Result<u64>,
) -> io::Result<String> {
    std::fs::create_dir_all(folder)?;
    let mut source = File::open(staged)?;
    // Keep incomplete bytes under a temporary name on the destination's
    // filesystem, even when Chromium stages downloads on another volume.
    let mut temporary = tempfile::Builder::new()
        .prefix(".ophio-download-")
        .tempfile_in(folder)?;
    copy(&mut source, temporary.as_file_mut())?;
    temporary.as_file().sync_all()?;
    drop(source);
    let name = safe_name(suggested);
    let (stem, extension) = match name.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() => (stem.to_owned(), format!(".{extension}")),
        _ => (name.clone(), String::new()),
    };
    for n in 1..1000 {
        let candidate = if n == 1 {
            name.clone()
        } else {
            format!("{stem} ({n}){extension}")
        };
        // Publishing claims the name atomically: no placeholder or partial
        // final file, and no overwrite if another download won the name.
        match temporary.persist_noclobber(folder.join(&candidate)) {
            Ok(_) => {
                let _ = std::fs::remove_file(staged);
                return Ok(candidate);
            }
            Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => {
                temporary = error.file;
            }
            Err(error) => return Err(error.error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "No unused download name is available",
    ))
}

/// A file name a page suggested, made safe to create in a folder.
pub(super) fn safe_name(suggested: &str) -> String {
    let base = suggested.rsplit(['/', '\\']).next().unwrap_or_default();
    let cleaned: String = base
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, ':' | '*' | '?' | '"' | '<' | '>' | '|') {
                '_'
            } else {
                c
            }
        })
        .take(120)
        .collect();
    let cleaned = cleaned.trim_matches(|c: char| c == '.' || c.is_whitespace());
    if cleaned.is_empty() {
        "download".into()
    } else {
        cleaned.into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    const BYTES: &[u8] = b"a complete download worth keeping";

    #[tokio::test]
    async fn failed_delivery_keeps_bytes_for_retry() {
        let dir = tempfile::tempdir().unwrap();
        let staged = dir.path().join("staged");
        let folder = dir.path().join("downloads");
        std::fs::write(&staged, BYTES).unwrap();
        std::fs::write(&folder, b"a file blocks the destination").unwrap();

        assert!(deliver(&staged, &folder, "report.txt").await.is_err());
        assert_eq!(std::fs::read(&staged).unwrap(), BYTES);
        std::fs::remove_file(&folder).unwrap();
        assert_eq!(
            deliver(&staged, &folder, "report.txt").await.unwrap(),
            "report.txt"
        );
        assert_eq!(std::fs::read(folder.join("report.txt")).unwrap(), BYTES);
        assert!(!staged.exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn unwritable_destination_keeps_bytes() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let staged = dir.path().join("staged");
        let folder = dir.path().join("downloads");
        std::fs::write(&staged, BYTES).unwrap();
        std::fs::create_dir(&folder).unwrap();
        std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o500)).unwrap();
        // Root bypasses mode bits in some CI containers. Other tests exercise
        // delivery failure there without relying on Unix permissions.
        let denied = File::create(folder.join("probe")).is_err();
        let result = if denied {
            Some(deliver(&staged, &folder, "report.txt").await)
        } else {
            None
        };
        std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o700)).unwrap();
        if let Some(result) = result {
            assert_eq!(result.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
            assert_eq!(std::fs::read(&staged).unwrap(), BYTES);
            assert_eq!(std::fs::read_dir(&folder).unwrap().count(), 0);
        }
    }

    #[test]
    fn failed_copy_never_publishes_partial_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let staged = dir.path().join("staged");
        let folder = dir.path().join("downloads");
        std::fs::write(&staged, BYTES).unwrap();
        let error = deliver_with(&staged, &folder, "report.txt", |_, destination| {
            destination.write_all(b"partial")?;
            assert!(!folder.join("report.txt").exists());
            assert_eq!(std::fs::read(&staged).unwrap(), BYTES);
            Err(io::Error::new(
                io::ErrorKind::StorageFull,
                "injected full destination",
            ))
        })
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::StorageFull);
        assert_eq!(std::fs::read(&staged).unwrap(), BYTES);
        assert_eq!(std::fs::read_dir(&folder).unwrap().count(), 0);
    }

    #[test]
    fn publication_preserves_a_name_claimed_while_copying() {
        let dir = tempfile::tempdir().unwrap();
        let staged = dir.path().join("staged");
        let folder = dir.path().join("downloads");
        std::fs::write(&staged, BYTES).unwrap();
        let name = deliver_with(
            &staged,
            &folder,
            "../../report.txt",
            |source, destination| {
                let mut first = [0; 8];
                source.read_exact(&mut first)?;
                destination.write_all(&first)?;
                assert!(!folder.join("report.txt").exists());
                std::fs::write(folder.join("report.txt"), b"another download")?;
                io::copy(source, destination).map(|rest| rest + 8)
            },
        )
        .unwrap();
        assert_eq!(name, "report (2).txt");
        assert_eq!(
            std::fs::read(folder.join("report.txt")).unwrap(),
            b"another download"
        );
        assert_eq!(std::fs::read(folder.join(name)).unwrap(), BYTES);
        assert_eq!(std::fs::read_dir(&folder).unwrap().count(), 2);
        assert!(!staged.exists());
    }

    #[tokio::test]
    async fn exhausted_names_keep_the_staged_file_and_remove_the_copy() {
        let dir = tempfile::tempdir().unwrap();
        let staged = dir.path().join("staged");
        let folder = dir.path().join("downloads");
        std::fs::write(&staged, BYTES).unwrap();
        std::fs::create_dir(&folder).unwrap();
        for n in 1..1000 {
            let name = if n == 1 {
                "report.txt".into()
            } else {
                format!("report ({n}).txt")
            };
            std::fs::write(folder.join(name), b"existing").unwrap();
        }
        assert_eq!(
            deliver(&staged, &folder, "report.txt")
                .await
                .unwrap_err()
                .kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(std::fs::read(&staged).unwrap(), BYTES);
        assert_eq!(std::fs::read_dir(&folder).unwrap().count(), 999);
        assert_eq!(
            std::fs::read(folder.join("report.txt")).unwrap(),
            b"existing"
        );
    }

    #[tokio::test]
    async fn concurrent_deliveries_publish_distinct_complete_files() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("first");
        let second = dir.path().join("second");
        let folder = dir.path().join("downloads");
        std::fs::write(&first, b"first").unwrap();
        std::fs::write(&second, b"second").unwrap();
        let (a, b) = tokio::join!(
            deliver(&first, &folder, "report.txt"),
            deliver(&second, &folder, "report.txt")
        );
        let (a, b) = (a.unwrap(), b.unwrap());
        assert_ne!(a, b);
        assert_eq!(std::fs::read(folder.join(a)).unwrap(), b"first");
        assert_eq!(std::fs::read(folder.join(b)).unwrap(), b"second");
        assert!(!first.exists() && !second.exists());
    }
}
