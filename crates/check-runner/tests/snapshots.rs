// SPDX-License-Identifier: Apache-2.0
use std::{
    fs,
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use check_runner::snapshot::{
    EntryKind, FileChange, Manifest, SnapshotStore, validate_globs, validate_input,
};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

const MIB: usize = 1024 * 1024;

fn fixture() -> (TempDir, SnapshotStore) {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join("project")).unwrap();
    let store = SnapshotStore::new(dir.path().join("data")).unwrap();
    (dir, store)
}

fn write(project: &Path, name: &str, bytes: impl AsRef<[u8]>) {
    let path = project.join(name);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}

fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[test]
fn captures_hidden_untracked_and_ignore_rules_without_git() {
    let (dir, store) = fixture();
    let project = dir.path().join("project");
    write(&project, ".gitignore", "ignored.txt\n");
    write(&project, ".ignore", "also-ignored.txt\n");
    for name in [
        "tracked-looking.txt",
        "untracked.txt",
        ".hidden",
        "nested/.hidden",
    ] {
        write(&project, name, "keep");
    }
    write(&project, "ignored.txt", "ignore");
    write(&project, "also-ignored.txt", "ignore");
    let manifest = store.capture(&project, None).unwrap();
    for name in [
        "tracked-looking.txt",
        "untracked.txt",
        ".hidden",
        "nested/.hidden",
        ".gitignore",
        ".ignore",
    ] {
        assert!(manifest.files.contains_key(name), "missing {name}");
    }
    assert!(!manifest.files.contains_key("ignored.txt"));
    assert!(!manifest.files.contains_key("also-ignored.txt"));
    assert!(!project.join(".git").exists());
}

#[test]
fn forcibly_prunes_dependency_and_build_folders_at_any_depth() {
    let (dir, store) = fixture();
    let project = dir.path().join("project");
    for name in [
        "node_modules",
        "target",
        ".venv",
        "build",
        "dist",
        ".gradle",
        ".git",
    ] {
        write(&project, &format!("{name}/secret"), "skip");
        write(&project, &format!("nested/{name}/secret"), "skip");
    }
    write(&project, "nested/source.rs", "keep");
    let manifest = store.capture(&project, None).unwrap();
    assert_eq!(
        manifest
            .files
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        vec!["nested/source.rs"]
    );
}

#[cfg(unix)]
#[test]
fn detects_files_created_and_changed_by_a_shell() {
    let (dir, store) = fixture();
    let project = dir.path().join("project");
    write(&project, "existing.txt", "before");
    let before = store.capture(&project, None).unwrap();
    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg("printf 'changed by shell' > existing.txt; printf 'created by shell' > new.txt")
        .current_dir(&project)
        .status()
        .unwrap();
    assert!(status.success());
    let after = store.capture(&project, Some(&before)).unwrap();
    let diff = before.diff(&after).unwrap();
    assert!(matches!(
        diff.changes.get("existing.txt"),
        Some(FileChange::Modified)
    ));
    assert!(matches!(
        diff.changes.get("new.txt"),
        Some(FileChange::Added)
    ));
}

#[test]
fn stores_text_blobs_by_full_hash_and_round_trips_manifest() {
    let (dir, store) = fixture();
    let project = dir.path().join("project");
    let bytes = b"hello snapshot\n";
    write(&project, "hello.txt", bytes);
    let manifest = store.capture(&project, None).unwrap();
    let entry = &manifest.files["hello.txt"];
    let digest = hash(bytes);
    assert_eq!(entry.path, "hello.txt");
    assert_eq!(entry.size, bytes.len() as u64);
    assert_eq!(entry.hash.as_deref(), Some(digest.as_str()));
    assert!(entry.blob);
    assert!(matches!(entry.kind, EntryKind::File));
    assert_eq!(store.read_blob(&digest).unwrap(), bytes);
    assert_eq!(
        fs::read(dir.path().join("data/blobs").join(&digest)).unwrap(),
        bytes
    );
    assert!(
        dir.path()
            .join("data/manifests")
            .join(format!("{}.json", manifest.id))
            .is_file()
    );
    let loaded = store.load_manifest(&manifest.id).unwrap();
    assert_eq!(loaded.id, manifest.id);
    assert_eq!(loaded.root, fs::canonicalize(project).unwrap());
    assert_eq!(loaded.captured_at_secs, manifest.captured_at_secs);
    assert_eq!(loaded.files["hello.txt"].hash, entry.hash);
    assert!(manifest.diff(&loaded).unwrap().is_empty());
}

#[test]
fn binary_and_size_boundaries_control_hashes_and_blobs() {
    let (dir, store) = fixture();
    let project = dir.path().join("project");
    write(&project, "contains-nul.bin", b"text\0with nul");
    write(&project, "invalid.bin", [0xff, 0xfe]);
    write(&project, "text-limit.txt", vec![b'a'; MIB]);
    write(&project, "over-text-limit.txt", vec![b'b'; MIB + 1]);
    write(&project, "hash-limit.txt", vec![b'c'; 5 * MIB]);
    write(&project, "over-hash-limit.txt", vec![b'd'; 5 * MIB + 1]);
    let manifest = store.capture(&project, None).unwrap();
    assert!(manifest.files["text-limit.txt"].blob);
    for name in [
        "contains-nul.bin",
        "invalid.bin",
        "over-text-limit.txt",
        "hash-limit.txt",
    ] {
        let entry = &manifest.files[name];
        assert!(entry.hash.is_some(), "must hash {name}");
        assert!(!entry.blob, "must not save content of {name}");
        assert!(store.read_blob(entry.hash.as_ref().unwrap()).is_err());
    }
    let oversized = &manifest.files["over-hash-limit.txt"];
    assert_eq!(oversized.size, (5 * MIB + 1) as u64);
    assert!(oversized.hash.is_none());
    assert!(!oversized.blob);
    assert!(oversized.unreadable.is_none());
    assert!(manifest.issues.is_empty());
    assert_eq!(
        store
            .read_blob(manifest.files["text-limit.txt"].hash.as_ref().unwrap())
            .unwrap()
            .len(),
        MIB
    );
}

#[test]
fn reuses_unchanged_hashes_but_hashes_changed_files_again() {
    let (dir, store) = fixture();
    let project = dir.path().join("project");
    write(&project, "same.txt", "same");
    write(&project, "change.txt", "old");
    let before = store.capture(&project, None).unwrap();
    let unchanged = store.capture(&project, Some(&before)).unwrap();
    assert_eq!(unchanged.reused_hashes, 2);
    assert!(before.diff(&unchanged).unwrap().is_empty());
    write(&project, "change.txt", "different length");
    let after = store.capture(&project, Some(&unchanged)).unwrap();
    assert_eq!(after.reused_hashes, 1);
    assert_eq!(
        after.files["change.txt"].hash.as_deref(),
        Some(hash(b"different length").as_str())
    );
    assert_ne!(
        before.files["change.txt"].hash,
        after.files["change.txt"].hash
    );
}

#[test]
fn diff_reports_removal_and_filters_changed_paths_and_inputs() {
    let (dir, store) = fixture();
    let project = dir.path().join("project");
    write(&project, "src/removed.rs", "old");
    write(&project, "config.toml", "old");
    let before = store.capture(&project, None).unwrap();
    fs::remove_file(project.join("src/removed.rs")).unwrap();
    write(&project, "config.toml", "new config");
    write(&project, "src/added.rs", "new");
    let after = store.capture(&project, Some(&before)).unwrap();
    let diff = before.diff(&after).unwrap();
    assert!(matches!(
        diff.changes.get("src/removed.rs"),
        Some(FileChange::Removed)
    ));
    assert_eq!(
        diff.changed_paths().collect::<Vec<_>>(),
        vec!["config.toml", "src/added.rs", "src/removed.rs"]
    );
    assert!(diff.matches_globs(&["src/**/*.rs".into()]).unwrap());
    assert!(!diff.matches_globs(&["docs/**".into()]).unwrap());
    assert_eq!(
        diff.changed_inputs(&[
            "config.toml".into(),
            "unchanged.txt".into(),
            "src/removed.rs".into()
        ])
        .unwrap(),
        vec!["config.toml", "src/removed.rs"]
    );
    assert!(diff.matches_globs(&["[".into()]).is_err());
    assert!(diff.changed_inputs(&["../secret".into()]).is_err());
}

#[cfg(unix)]
#[test]
fn reports_mode_only_changes_even_when_content_hash_is_reused() {
    use std::os::unix::fs::PermissionsExt;
    let (dir, store) = fixture();
    let project = dir.path().join("project");
    write(&project, "script", "echo hello\n");
    fs::set_permissions(project.join("script"), fs::Permissions::from_mode(0o644)).unwrap();
    let before = store.capture(&project, None).unwrap();
    fs::set_permissions(project.join("script"), fs::Permissions::from_mode(0o755)).unwrap();
    let after = store.capture(&project, Some(&before)).unwrap();
    assert_eq!(before.files["script"].hash, after.files["script"].hash);
    assert_ne!(before.files["script"].mode, after.files["script"].mode);
    assert!(matches!(
        before.diff(&after).unwrap().changes.get("script"),
        Some(FileChange::ModeChanged)
    ));
}

#[test]
fn refuses_internal_store_before_creating_any_project_files() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("project");
    fs::create_dir(&project).unwrap();
    write(&project, "keep.txt", "untouched");
    for data in [project.clone(), project.join("new/data")] {
        let store = SnapshotStore::new(&data).unwrap();
        assert!(store.capture(&project, None).is_err());
        assert_eq!(fs::read_dir(&project).unwrap().count(), 1);
        assert_eq!(fs::read(project.join("keep.txt")).unwrap(), b"untouched");
    }
}

#[test]
fn missing_or_non_directory_project_is_fatal() {
    let (dir, store) = fixture();
    assert!(store.capture(dir.path().join("missing"), None).is_err());
    write(dir.path(), "file", "not a directory");
    assert!(store.capture(dir.path().join("file"), None).is_err());
}

#[cfg(unix)]
#[test]
fn symlinks_hash_link_targets_without_reading_outside_secrets() {
    use std::os::unix::fs::symlink;
    let (dir, store) = fixture();
    let project = dir.path().join("project");
    write(dir.path(), "secret.txt", "outside secret");
    symlink("../secret.txt", project.join("input.txt")).unwrap();
    symlink("../missing", project.join("dangling")).unwrap();
    let before = store.capture(&project, None).unwrap();
    let entry = &before.files["input.txt"];
    assert!(matches!(entry.kind, EntryKind::Symlink));
    assert_eq!(entry.hash.as_deref(), Some(hash(b"../secret.txt").as_str()));
    assert_eq!(entry.size, b"../secret.txt".len() as u64);
    assert!(!entry.blob);
    assert!(store.read_blob(entry.hash.as_ref().unwrap()).is_err());
    assert!(before.files["dangling"].hash.is_some());
    write(dir.path(), "secret.txt", "changed outside secret content");
    let after = store.capture(&project, Some(&before)).unwrap();
    assert!(before.diff(&after).unwrap().is_empty());
    assert!(
        before
            .diff(&after)
            .unwrap()
            .changed_inputs(&["input.txt".into()])
            .unwrap()
            .is_empty()
    );
    fs::remove_file(project.join("input.txt")).unwrap();
    symlink("../different-secret", project.join("input.txt")).unwrap();
    let retargeted = store.capture(&project, Some(&after)).unwrap();
    assert!(matches!(
        after.diff(&retargeted).unwrap().changes.get("input.txt"),
        Some(FileChange::Modified)
    ));
}

#[cfg(unix)]
#[test]
fn rejects_store_symlinked_into_project_without_creating_directories() {
    use std::os::unix::fs::symlink;
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("project");
    fs::create_dir(&project).unwrap();
    symlink(&project, dir.path().join("alias")).unwrap();
    let store = SnapshotStore::new(dir.path().join("alias/new/data")).unwrap();
    assert!(store.capture(&project, None).is_err());
    assert_eq!(fs::read_dir(&project).unwrap().count(), 0);
}

#[cfg(unix)]
#[test]
fn unreadable_files_are_recorded_and_always_count_as_changed() {
    use std::os::unix::fs::PermissionsExt;
    let (dir, store) = fixture();
    let project = dir.path().join("project");
    write(&project, "unreadable.txt", "secret");
    write(&project, "readable.txt", "normal");
    let path = project.join("unreadable.txt");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o0)).unwrap();
    // Root can read mode-000 files, so this case needs an ordinary user.
    if fs::read(&path).is_ok() {
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        eprintln!("Unreadable-file test skipped: current user can read mode-000 files.");
        return;
    }
    let before = store.capture(&project, None).unwrap();
    let after = store.capture(&project, Some(&before)).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let entry = &before.files["unreadable.txt"];
    assert!(entry.unreadable.is_some());
    assert!(entry.hash.is_none());
    assert!(!entry.blob);
    assert!(!before.issues.is_empty());
    assert!(before.files["readable.txt"].hash.is_some());
    assert!(matches!(
        before.diff(&after).unwrap().changes.get("unreadable.txt"),
        Some(FileChange::Unreadable)
    ));
    let recovered = store.capture(&project, Some(&after)).unwrap();
    assert!(recovered.files["unreadable.txt"].hash.is_some());
    assert!(matches!(
        after
            .diff(&recovered)
            .unwrap()
            .changes
            .get("unreadable.txt"),
        Some(FileChange::Unreadable)
    ));
}

#[test]
fn oversized_files_skip_unchanged_checks_and_select_only_changed_paths_and_inputs() {
    let (dir, store) = fixture();
    let project = dir.path().join("project");
    let empty = store.capture(&project, None).unwrap();
    write(&project, "fixtures/large.bin", vec![b'a'; 6 * MIB]);
    write(&project, "unchanged.txt", "keep");
    let before = store.capture(&project, Some(&empty)).unwrap();
    assert_eq!(
        empty.diff(&before).unwrap().changes["fixtures/large.bin"],
        FileChange::Added
    );
    let unchanged = store.capture(&project, Some(&before)).unwrap();
    let diff = before.diff(&unchanged).unwrap();
    assert!(diff.is_empty());
    assert!(!diff.incomplete);
    assert!(!diff.matches_globs(&[]).unwrap());
    assert!(!diff.matches_globs(&["fixtures/**".into()]).unwrap());
    assert!(
        diff.changed_inputs(&["fixtures/large.bin".into()])
            .unwrap()
            .is_empty()
    );

    write(&project, "fixtures/large.bin", vec![b'b'; 6 * MIB]);
    // Set a distinct mtime explicitly, rather than relying on clock resolution.
    fs::File::options()
        .write(true)
        .open(project.join("fixtures/large.bin"))
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(1_000_000)))
        .unwrap();
    let after = store.capture(&project, Some(&unchanged)).unwrap();
    let diff = unchanged.diff(&after).unwrap();
    assert!(!diff.is_empty());
    assert!(!diff.incomplete);
    assert_eq!(diff.changes["fixtures/large.bin"], FileChange::Modified);
    assert_eq!(
        diff.changed_paths().collect::<Vec<_>>(),
        vec!["fixtures/large.bin"]
    );
    assert!(diff.matches_globs(&[]).unwrap());
    assert!(diff.matches_globs(&["fixtures/**".into()]).unwrap());
    assert!(!diff.matches_globs(&["unrelated/**".into()]).unwrap());
    assert_eq!(
        diff.changed_inputs(&["fixtures/large.bin".into(), "unchanged.txt".into()])
            .unwrap(),
        vec!["fixtures/large.bin"]
    );
    let repeated = store.capture(&project, Some(&after)).unwrap();
    assert!(after.diff(&repeated).unwrap().is_empty());
    assert_eq!(store.load_manifest(&after.id).unwrap(), after);
    assert!(
        fs::read_dir(store.data_dir().join("blobs"))
            .unwrap()
            .count()
            == 1
    );
    fs::remove_file(project.join("fixtures/large.bin")).unwrap();
    let removed = store.capture(&project, Some(&after)).unwrap();
    assert_eq!(
        after.diff(&removed).unwrap().changes["fixtures/large.bin"],
        FileChange::Removed
    );
}

#[test]
fn oversized_replacement_with_same_size_and_mtime_changes_file_identity() {
    let (dir, store) = fixture();
    let project = dir.path().join("project");
    let path = project.join("large.bin");
    fs::File::create(&path)
        .unwrap()
        .set_len((6 * MIB) as u64)
        .unwrap();
    let time = fs::metadata(&path).unwrap().modified().unwrap();
    let before = store.capture(&project, None).unwrap();
    // Keep the old file alive so its identity cannot be recycled for the replacement.
    fs::rename(&path, dir.path().join("old-large.bin")).unwrap();
    let replacement = fs::File::create(&path).unwrap();
    replacement.set_len((6 * MIB) as u64).unwrap();
    replacement
        .set_times(fs::FileTimes::new().set_modified(time))
        .unwrap();
    let after = store.capture(&project, Some(&before)).unwrap();
    assert_eq!(
        before.files["large.bin"].size,
        after.files["large.bin"].size
    );
    assert_eq!(
        before.files["large.bin"].modified,
        after.files["large.bin"].modified
    );
    assert_ne!(
        before.files["large.bin"].identity,
        after.files["large.bin"].identity
    );
    let diff = before.diff(&after).unwrap();
    assert!(!diff.incomplete);
    assert_eq!(diff.changes["large.bin"], FileChange::Modified);
}

#[cfg(unix)]
#[test]
fn sockets_and_fifos_skip_unchanged_checks_without_reading_them() {
    let (dir, store) = fixture();
    let project = dir.path().join("project");
    let empty = store.capture(&project, None).unwrap();
    let _socket = std::os::unix::net::UnixListener::bind(project.join("socket")).unwrap();
    assert!(
        std::process::Command::new("mkfifo")
            .arg(project.join("fifo"))
            .status()
            .unwrap()
            .success()
    );
    write(&project, "normal", "keep");
    let manifest = store.capture(&project, Some(&empty)).unwrap();
    let repeated = store.capture(&project, Some(&manifest)).unwrap();
    assert!(manifest.issues.is_empty());
    for name in ["socket", "fifo"] {
        let entry = &manifest.files[name];
        assert_eq!(
            entry.kind,
            if name == "fifo" {
                EntryKind::Fifo
            } else {
                EntryKind::Socket
            }
        );
        assert!(entry.hash.is_none());
        assert!(entry.unreadable.is_none());
        assert!(!entry.blob);
        assert!(entry.modified.is_some());
        assert_eq!(
            empty.diff(&manifest).unwrap().changes[name],
            FileChange::Added
        );
        let diff = manifest.diff(&repeated).unwrap();
        assert!(diff.is_empty());
        assert!(!diff.matches_globs(&[name.into()]).unwrap());
        assert!(!diff.matches_globs(&[]).unwrap());
        assert!(diff.changed_inputs(&[name.into()]).unwrap().is_empty());
        assert!(!diff.incomplete);
    }
    assert!(manifest.files.contains_key("normal"));
    assert_eq!(store.load_manifest(&manifest.id).unwrap(), manifest);
}

#[cfg(unix)]
#[test]
fn replaced_special_files_select_only_their_own_paths_and_inputs() {
    let (dir, store) = fixture();
    let project = dir.path().join("project");
    let _old_socket = std::os::unix::net::UnixListener::bind(project.join("socket")).unwrap();
    let _old_fifo = std::os::unix::net::UnixListener::bind(project.join("fifo")).unwrap();
    write(&project, "normal", "keep");
    let before = store.capture(&project, None).unwrap();
    fs::rename(project.join("socket"), dir.path().join("old-socket")).unwrap();
    let _new_socket = std::os::unix::net::UnixListener::bind(project.join("socket")).unwrap();
    fs::remove_file(project.join("fifo")).unwrap();
    assert!(
        std::process::Command::new("mkfifo")
            .arg(project.join("fifo"))
            .status()
            .unwrap()
            .success()
    );
    let after = store.capture(&project, Some(&before)).unwrap();
    assert_ne!(
        before.files["socket"].identity,
        after.files["socket"].identity
    );
    assert_eq!(before.files["fifo"].kind, EntryKind::Socket);
    assert_eq!(after.files["fifo"].kind, EntryKind::Fifo);
    let diff = before.diff(&after).unwrap();
    assert!(!diff.incomplete);
    for name in ["socket", "fifo"] {
        assert_eq!(diff.changes[name], FileChange::Modified);
        assert!(diff.matches_globs(&[name.into()]).unwrap());
    }
    assert!(!diff.matches_globs(&["unrelated/**".into()]).unwrap());
    assert_eq!(
        diff.changed_inputs(&["socket".into(), "fifo".into(), "normal".into()])
            .unwrap(),
        vec!["fifo", "socket"]
    );
    let repeated = store.capture(&project, Some(&after)).unwrap();
    assert!(after.diff(&repeated).unwrap().is_empty());
}

#[test]
fn validates_portable_project_relative_inputs_and_globs() {
    for input in [
        "src/main.rs",
        "./config.toml",
        r"src\main.rs",
        "file name.txt",
    ] {
        assert!(validate_input(input).is_ok(), "valid input {input:?}");
    }
    for input in [
        "",
        "/etc/passwd",
        "../secret",
        "src/../secret",
        r"src\..\secret",
        r"C:\secret",
        "C:secret",
        r"\\server\share\secret",
        "bad\0name",
    ] {
        assert!(validate_input(input).is_err(), "unsafe input {input:?}");
        assert!(
            validate_globs(&[input.into()]).is_err(),
            "unsafe glob {input:?}"
        );
    }
    assert!(validate_globs(&["src/**/*.rs".into(), "./*.toml".into()]).is_ok());
    assert!(validate_globs(&["[".into()]).is_err());
}

#[test]
fn changed_inputs_normalizes_dot_prefixes_and_backslash_separators() {
    let (dir, store) = fixture();
    let project = dir.path().join("project");
    let before = store.capture(&project, None).unwrap();
    write(&project, "src/main.rs", "new");
    let after = store.capture(&project, Some(&before)).unwrap();
    let diff = before.diff(&after).unwrap();
    assert_eq!(
        diff.changed_inputs(&["./src/main.rs".into()])
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        diff.changed_inputs(&[r"src\main.rs".into()]).unwrap().len(),
        1
    );
}

fn set_capture_time(data: &Path, manifest: &Manifest, seconds: u64) {
    let path = data.join("manifests").join(format!("{}.json", manifest.id));
    let mut json: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    json["captured_at_secs"] = seconds.into();
    fs::write(path, serde_json::to_vec(&json).unwrap()).unwrap();
}

fn deterministic_now() -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(2_000_000_000)
}

#[test]
fn gc_retains_shared_blobs_referenced_by_recent_manifests() {
    let (dir, store) = fixture();
    let project = dir.path().join("project");
    let data = dir.path().join("data");
    write(&project, "shared.txt", "shared content");
    write(&project, "old-only.txt", "expired content");
    let old = store.capture(&project, None).unwrap();
    fs::remove_file(project.join("old-only.txt")).unwrap();
    write(&project, "new-only.txt", "recent content");
    let recent = store.capture(&project, Some(&old)).unwrap();
    let now = deterministic_now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    set_capture_time(&data, &old, now - 15 * 86400);
    set_capture_time(&data, &recent, now - 86400);
    store.gc_at(14, deterministic_now()).unwrap();
    assert!(store.load_manifest(&old.id).is_err());
    assert!(store.load_manifest(&recent.id).is_ok());
    assert_eq!(
        store
            .read_blob(old.files["shared.txt"].hash.as_ref().unwrap())
            .unwrap(),
        b"shared content"
    );
    assert_eq!(
        store
            .read_blob(recent.files["new-only.txt"].hash.as_ref().unwrap())
            .unwrap(),
        b"recent content"
    );
    assert!(
        store
            .read_blob(old.files["old-only.txt"].hash.as_ref().unwrap())
            .is_err()
    );
    store.gc_at(14, deterministic_now()).unwrap();
}

#[test]
fn gc_corrupt_retained_manifest_fails_before_deleting_any_blobs() {
    let (dir, store) = fixture();
    let project = dir.path().join("project");
    let data = dir.path().join("data");
    write(&project, "old.txt", "old blob must survive failed gc");
    let old = store.capture(&project, None).unwrap();
    fs::remove_file(project.join("old.txt")).unwrap();
    write(&project, "recent.txt", "recent");
    let recent = store.capture(&project, None).unwrap();
    let now = deterministic_now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    set_capture_time(&data, &old, now - 15 * 86400);
    set_capture_time(&data, &recent, now - 86400);
    fs::write(
        data.join("manifests").join(format!("{}.json", recent.id)),
        b"{corrupt",
    )
    .unwrap();
    assert!(store.gc_at(14, deterministic_now()).is_err());
    assert_eq!(
        store
            .read_blob(old.files["old.txt"].hash.as_ref().unwrap())
            .unwrap(),
        b"old blob must survive failed gc"
    );
    assert_eq!(
        store
            .read_blob(recent.files["recent.txt"].hash.as_ref().unwrap())
            .unwrap(),
        b"recent"
    );
}

#[test]
fn read_blob_rejects_corrupted_saved_content() {
    let (dir, store) = fixture();
    let project = dir.path().join("project");
    write(&project, "source.txt", "original content");
    let manifest = store.capture(&project, None).unwrap();
    let digest = manifest.files["source.txt"].hash.as_ref().unwrap();
    fs::write(
        dir.path().join("data/blobs").join(digest),
        b"corrupt content",
    )
    .unwrap();
    assert!(store.read_blob(digest).is_err());
}

#[cfg(unix)]
#[test]
fn unreadable_directory_makes_all_check_inputs_potentially_changed() {
    use std::os::unix::fs::PermissionsExt;
    let (dir, store) = fixture();
    let project = dir.path().join("project");
    write(&project, "private/hidden.txt", "secret");
    write(&project, "visible.txt", "unchanged");
    let before = store.capture(&project, None).unwrap();
    let private = project.join("private");
    fs::set_permissions(&private, fs::Permissions::from_mode(0o0)).unwrap();
    if fs::read_dir(&private).is_ok() {
        fs::set_permissions(&private, fs::Permissions::from_mode(0o700)).unwrap();
        eprintln!("Unreadable-directory test skipped: current user can read mode-000 directories.");
        return;
    }
    let incomplete = store.capture(&project, Some(&before)).unwrap();
    let repeated = store.capture(&project, Some(&incomplete)).unwrap();
    fs::set_permissions(&private, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(!incomplete.issues.is_empty());
    // Two equally incomplete captures still cannot establish that inputs stayed unchanged.
    let diff = incomplete.diff(&repeated).unwrap();
    assert!(!diff.is_empty());
    assert!(diff.matches_globs(&["unrelated/**/*.rs".into()]).unwrap());
    let inputs = vec!["visible.txt".into(), "unrelated/config.toml".into()];
    let mut changed = diff.changed_inputs(&inputs).unwrap();
    let mut expected = inputs;
    changed.sort();
    expected.sort();
    assert_eq!(changed, expected);
}

#[cfg(unix)]
#[test]
fn external_symlink_directory_is_recorded_without_descending() {
    use std::os::unix::fs::symlink;
    let (dir, store) = fixture();
    let project = dir.path().join("project");
    write(dir.path(), "external/secret.txt", "outside secret content");
    symlink("../external", project.join("linked-dir")).unwrap();
    let manifest = store.capture(&project, None).unwrap();
    assert_eq!(manifest.files.len(), 1);
    let entry = &manifest.files["linked-dir"];
    assert!(matches!(entry.kind, EntryKind::Symlink));
    assert_eq!(entry.hash.as_deref(), Some(hash(b"../external").as_str()));
    assert!(!entry.blob);
    assert!(!manifest.files.contains_key("linked-dir/secret.txt"));
    assert!(store.read_blob(&hash(b"outside secret content")).is_err());
}

#[cfg(unix)]
#[test]
fn replacing_file_with_external_symlink_does_not_reuse_hash_or_blob() {
    use std::os::unix::fs::symlink;
    let (dir, store) = fixture();
    let project = dir.path().join("project");
    // Match the link target's byte length so size alone cannot distinguish the entries.
    write(&project, "input.txt", "original text");
    write(dir.path(), "secret.txt", "external secret");
    let before = store.capture(&project, None).unwrap();
    assert!(before.files["input.txt"].blob);
    fs::remove_file(project.join("input.txt")).unwrap();
    symlink("../secret.txt", project.join("input.txt")).unwrap();
    let after = store.capture(&project, Some(&before)).unwrap();
    assert_eq!(after.reused_hashes, 0);
    let entry = &after.files["input.txt"];
    assert!(matches!(entry.kind, EntryKind::Symlink));
    assert_eq!(entry.hash.as_deref(), Some(hash(b"../secret.txt").as_str()));
    assert_ne!(entry.hash, before.files["input.txt"].hash);
    assert!(!entry.blob);
    assert!(store.read_blob(entry.hash.as_ref().unwrap()).is_err());
}

#[cfg(unix)]
#[test]
fn same_size_edit_with_restored_mtime_does_not_reuse_stale_hash() {
    let (dir, store) = fixture();
    let project = dir.path().join("project");
    write(&project, "input.txt", "before");
    let path = project.join("input.txt");
    let original_time = fs::metadata(&path).unwrap().modified().unwrap();
    let before = store.capture(&project, None).unwrap();
    fs::write(&path, "after!").unwrap();
    fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(original_time))
        .unwrap();
    assert_eq!(
        fs::metadata(&path).unwrap().modified().unwrap(),
        original_time
    );
    let after = store.capture(&project, Some(&before)).unwrap();
    assert_eq!(
        after.files["input.txt"].size,
        before.files["input.txt"].size
    );
    assert_eq!(
        after.files["input.txt"].modified,
        before.files["input.txt"].modified
    );
    assert_eq!(after.reused_hashes, 0);
    assert_eq!(
        after.files["input.txt"].hash.as_deref(),
        Some(hash(b"after!").as_str())
    );
    assert!(matches!(
        before.diff(&after).unwrap().changes.get("input.txt"),
        Some(FileChange::Modified)
    ));
}

#[test]
fn different_project_roots_cannot_be_compared_or_used_for_hash_reuse() {
    let (dir, store) = fixture();
    let first = dir.path().join("project");
    let second = dir.path().join("another-project");
    write(&first, "same-name.txt", "same content");
    write(&second, "same-name.txt", "same content");
    let first_manifest = store.capture(&first, None).unwrap();
    let second_manifest = store.capture(&second, None).unwrap();
    assert!(first_manifest.diff(&second_manifest).is_err());
    assert!(store.capture(&second, Some(&first_manifest)).is_err());
}
