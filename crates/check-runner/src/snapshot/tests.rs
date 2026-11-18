// SPDX-License-Identifier: Apache-2.0
use super::*;

#[cfg(unix)]
#[test]
fn symlink_swap_after_metadata_cannot_copy_a_secret_blob() {
    use std::os::unix::fs::symlink;
    let base = tempfile::tempdir().unwrap();
    let project = base.path().join("project");
    fs::create_dir(&project).unwrap();
    fs::write(project.join("file"), "before").unwrap();
    fs::write(base.path().join("secret"), "outside secret").unwrap();
    let root = Dir::open_ambient_dir(&project, cap_std::ambient_authority()).unwrap();
    let metadata = root.symlink_metadata("file").unwrap();
    fs::remove_file(project.join("file")).unwrap();
    symlink("../secret", project.join("file")).unwrap();
    let store = SnapshotStore::new(base.path().join("data")).unwrap();
    assert!(store.capture_file(&root, "file", &metadata, None).is_err());
    assert!(!store.data_dir().exists());
}

// Rustix does not expose mkfifoat on Apple platforms.
#[cfg(target_os = "linux")]
#[test]
fn replacing_a_file_by_a_pipe_does_not_block_capture() {
    let base = tempfile::tempdir().unwrap();
    let project = base.path().join("project");
    fs::create_dir(&project).unwrap();
    fs::write(project.join("file"), "before").unwrap();
    let root = Dir::open_ambient_dir(&project, cap_std::ambient_authority()).unwrap();
    let metadata = root.symlink_metadata("file").unwrap();
    fs::remove_file(project.join("file")).unwrap();
    rustix::fs::mkfifoat(
        &root,
        "file",
        rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
    )
    .unwrap();
    let store = SnapshotStore::new(base.path().join("data")).unwrap();
    assert!(store.capture_file(&root, "file", &metadata, None).is_err());
    let manifest = store.capture(&project, None).unwrap();
    assert_eq!(manifest.files.len(), 1);
    let pipe = &manifest.files["file"];
    assert_eq!(pipe.kind, EntryKind::Fifo);
    assert!(pipe.hash.is_none());
    assert!(pipe.unreadable.is_none());
    assert!(manifest.issues.is_empty());
    let repeated = store.capture(&project, Some(&manifest)).unwrap();
    assert!(manifest.diff(&repeated).unwrap().is_empty());
}

#[test]
fn missing_blob_is_rebuilt_instead_of_reusing_an_incomplete_cache() {
    let base = tempfile::tempdir().unwrap();
    let project = base.path().join("project");
    fs::create_dir(&project).unwrap();
    fs::write(project.join("file"), "saved text").unwrap();
    let store = SnapshotStore::new(base.path().join("data")).unwrap();
    let before = store.capture(&project, None).unwrap();
    let hash = before.files["file"].hash.as_ref().unwrap();
    fs::remove_file(store.data_dir().join("blobs").join(hash)).unwrap();
    let after = store.capture(&project, Some(&before)).unwrap();
    assert_eq!(after.reused_hashes, 0);
    assert_eq!(store.read_blob(hash).unwrap(), b"saved text");
    assert!(before.diff(&after).unwrap().is_empty());
}

#[test]
fn untrusted_cache_hashes_are_validated_before_any_file_lookup() {
    let base = tempfile::tempdir().unwrap();
    let project = base.path().join("project");
    fs::create_dir(&project).unwrap();
    fs::write(project.join("file"), "text").unwrap();
    let store = SnapshotStore::new(base.path().join("data")).unwrap();
    let mut before = store.capture(&project, None).unwrap();
    before.files.get_mut("file").unwrap().hash = Some("../../secret".into());
    assert!(store.capture(&project, Some(&before)).is_err());
    assert!(store.read_blob("../../secret").is_err());
    assert!(store.load_manifest("../manifest").is_err());
}

#[test]
fn forced_skips_are_not_reenabled_by_ignore_negations() {
    let base = tempfile::tempdir().unwrap();
    let project = base.path().join("project");
    fs::create_dir_all(project.join("node_modules")).unwrap();
    fs::write(project.join("node_modules/secret"), "dependency").unwrap();
    fs::write(
        project.join(".gitignore"),
        "!node_modules/\n!node_modules/**\n",
    )
    .unwrap();
    let store = SnapshotStore::new(base.path().join("data")).unwrap();
    let manifest = store.capture(&project, None).unwrap();
    assert!(!manifest.files.contains_key("node_modules/secret"));
}

#[test]
fn ordinary_files_named_like_build_folders_are_still_captured() {
    let base = tempfile::tempdir().unwrap();
    let project = base.path().join("project");
    fs::create_dir(&project).unwrap();
    fs::write(project.join("build"), "source script").unwrap();
    fs::write(project.join(".git"), "gitdir: ../external-git\n").unwrap();
    let store = SnapshotStore::new(base.path().join("data")).unwrap();
    let manifest = store.capture(&project, None).unwrap();
    assert!(manifest.files.contains_key("build"));
    assert!(!manifest.files.contains_key(".git"));
}

#[test]
fn missing_or_corrupt_blobs_do_not_prevent_hash_only_capture_or_diffs() {
    let base = tempfile::tempdir().unwrap();
    let project = base.path().join("project");
    fs::create_dir(&project).unwrap();
    fs::write(project.join("file"), "text").unwrap();
    let store = SnapshotStore::new(base.path().join("data")).unwrap();
    let before = store.capture(&project, None).unwrap();
    let hash = before.files["file"].hash.as_ref().unwrap();
    // Preserve the length to exercise cheap cache-existence checking rather
    // than triggering reconstruction because the cached size differs.
    fs::write(store.data_dir().join("blobs").join(hash), "oops").unwrap();
    assert!(matches!(
        store.read_blob(hash),
        Err(SnapshotError::ContentUnavailable { .. })
    ));
    let after = store.capture(&project, Some(&before)).unwrap();
    assert!(before.diff(&after).unwrap().is_empty());
    assert_eq!(after.files["file"].hash.as_ref(), Some(hash));
    fs::remove_file(store.data_dir().join("blobs").join(hash)).unwrap();
    assert!(matches!(
        store.read_blob(hash),
        Err(SnapshotError::ContentUnavailable { .. })
    ));
    let repaired = store.capture(&project, Some(&after)).unwrap();
    assert!(after.diff(&repaired).unwrap().is_empty());
    assert_eq!(store.read_blob(hash).unwrap(), b"text");
}

#[test]
fn binary_control_characters_are_hashed_but_not_saved_as_text() {
    let base = tempfile::tempdir().unwrap();
    let project = base.path().join("project");
    fs::create_dir(&project).unwrap();
    fs::write(project.join("binary"), [1, 2, 3, 0x7f]).unwrap();
    let store = SnapshotStore::new(base.path().join("data")).unwrap();
    let manifest = store.capture(&project, None).unwrap();
    assert!(manifest.files["binary"].hash.is_some());
    assert!(!manifest.files["binary"].blob);
}

#[test]
fn oversized_diffs_compare_size_mtime_and_identity_independently() {
    let base = tempfile::tempdir().unwrap();
    let project = base.path().join("project");
    fs::create_dir(&project).unwrap();
    fs::File::create(project.join("large"))
        .unwrap()
        .set_len(MAX_FILE_BYTES + 1)
        .unwrap();
    let store = SnapshotStore::new(base.path().join("data")).unwrap();
    let before = store.capture(&project, None).unwrap();
    assert!(before.diff(&before).unwrap().is_empty());
    for field in ["size", "mtime", "identity"] {
        let mut after = before.clone();
        let entry = after.files.get_mut("large").unwrap();
        match field {
            "size" => entry.size += 1,
            "mtime" => entry.modified.as_mut().unwrap().seconds += 1,
            "identity" => entry.identity.inode = Some(entry.identity.inode.unwrap_or(0) + 1),
            _ => unreachable!(),
        }
        let diff = before.diff(&after).unwrap();
        assert!(!diff.incomplete);
        assert_eq!(
            diff.changes.get("large"),
            Some(&FileChange::Modified),
            "{field}"
        );
    }
}

#[cfg(unix)]
#[test]
fn special_diffs_compare_type_and_identity_not_content_metadata() {
    let base = tempfile::tempdir().unwrap();
    let project = base.path().join("project");
    fs::create_dir(&project).unwrap();
    let _socket = std::os::unix::net::UnixListener::bind(project.join("special")).unwrap();
    let store = SnapshotStore::new(base.path().join("data")).unwrap();
    let before = store.capture(&project, None).unwrap();
    let mut metadata_changed = before.clone();
    let entry = metadata_changed.files.get_mut("special").unwrap();
    entry.size += 1;
    entry.modified.as_mut().unwrap().seconds += 1;
    assert!(before.diff(&metadata_changed).unwrap().is_empty());
    for field in ["type", "identity"] {
        let mut after = before.clone();
        let entry = after.files.get_mut("special").unwrap();
        match field {
            "type" => entry.kind = EntryKind::Fifo,
            "identity" => entry.identity.inode = Some(entry.identity.inode.unwrap() + 1),
            _ => unreachable!(),
        }
        let diff = before.diff(&after).unwrap();
        assert!(!diff.incomplete);
        assert_eq!(
            diff.changes.get("special"),
            Some(&FileChange::Modified),
            "{field}"
        );
    }
}
