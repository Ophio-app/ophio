// SPDX-License-Identifier: Apache-2.0
//! Explicit real-volume qualification. These tests do not create or format a
//! volume: the test environment supplies fresh, separately verified fixtures.
#![cfg(windows)]

use library::{FsLibrary, LibraryConfig};
use ports::{
    clock::Clock,
    library::{LibraryError, LibraryFs, RootSpec, SaveRequest},
};
use protocol::{
    ids::LibraryRootId,
    library::{NewlineStyle, RootKind, SaveOutcome},
    time::Timestamp,
};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::Instant,
};

const UNSUPPORTED_MUTATION: &str =
    "This drive's format does not support the safe saving Ophio needs. Reading still works.";

#[derive(Debug)]
struct TestClock(Instant);
impl Clock for TestClock {
    fn now(&self) -> Timestamp {
        Timestamp(1000)
    }
    fn monotonic(&self) -> Instant {
        self.0
    }
}

fn spec(path: PathBuf, only: Option<Vec<String>>) -> RootSpec {
    RootSpec {
        root_id: LibraryRootId::new(),
        path,
        label: "Volume fixture".into(),
        kind: RootKind::Project,
        writable: true,
        project_id: None,
        harness: None,
        only,
    }
}

fn request(base_revision: String, text: &str) -> SaveRequest {
    SaveRequest {
        text: text.into(),
        base_revision,
        newline: NewlineStyle::Lf,
        bom: false,
    }
}

fn assert_unsupported(error: LibraryError) {
    assert!(
        matches!(error, LibraryError::UnsupportedVolume),
        "{error:?}"
    );
    assert_eq!(error.code(), protocol::ErrorCode::Unsupported);
    assert_eq!(error.to_string(), UNSUPPORTED_MUTATION);
}

fn snapshot(root: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    fn visit(root: &Path, path: &Path, tree: &mut BTreeMap<PathBuf, Option<Vec<u8>>>) {
        for entry in fs::read_dir(path).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path).unwrap();
            assert!(!metadata.file_type().is_symlink());
            if metadata.is_dir() {
                tree.insert(path.strip_prefix(root).unwrap().to_owned(), None);
                visit(root, &path, tree);
            } else {
                tree.insert(
                    path.strip_prefix(root).unwrap().to_owned(),
                    Some(fs::read(&path).unwrap()),
                );
            }
        }
    }
    let mut tree = BTreeMap::new();
    visit(root, root, &mut tree);
    tree
}

#[allow(unsafe_code)]
fn verify_real_volume(path: &Path, expected: &str) {
    use cap_std::fs::Dir;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::{
        Storage::FileSystem::{
            FILE_ID_INFO, FileIdInfo, GetFileInformationByHandleEx, GetVolumeInformationByHandleW,
        },
        System::SystemServices::{FILE_PERSISTENT_ACLS, FILE_SUPPORTS_HARD_LINKS},
    };
    let directory = Dir::open_ambient_dir(path, cap_std::ambient_authority()).unwrap();
    let file = directory.into_std_file();
    let mut name = [0u16; 256];
    let mut flags = 0;
    // SAFETY: the file stays open and both outputs are correctly sized.
    assert_ne!(
        unsafe {
            GetVolumeInformationByHandleW(
                file.as_raw_handle(),
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &raw mut flags,
                name.as_mut_ptr(),
                name.len() as u32,
            )
        },
        0,
        "real-volume filesystem query failed: {}",
        std::io::Error::last_os_error()
    );
    let end = name.iter().position(|&unit| unit == 0).unwrap();
    let actual = String::from_utf16(&name[..end]).unwrap();
    assert!(
        actual.eq_ignore_ascii_case(expected),
        "expected {expected}, found {actual}"
    );
    let mut id = FILE_ID_INFO::default();
    // SAFETY: the open handle and correctly sized output remain alive.
    let result = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileIdInfo,
            (&raw mut id).cast(),
            size_of::<FILE_ID_INFO>() as u32,
        )
    };
    let error = std::io::Error::last_os_error();
    let required = FILE_PERSISTENT_ACLS | FILE_SUPPORTS_HARD_LINKS;
    if expected == "NTFS" {
        assert_ne!(
            result, 0,
            "NTFS fixture must provide full identity: {error}"
        );
        assert_eq!(flags & required, required);
    } else {
        assert_eq!(
            result, 0,
            "the fixture must exercise the real legacy fallback"
        );
        assert!(matches!(error.raw_os_error(), Some(1 | 50 | 87)), "{error}");
        assert_ne!(flags & required, required);
        eprintln!("real {actual}: FileIdInfo unsupported with {error}; volume flags=0x{flags:08x}");
    }
}

async fn exercise_volume(variable: &str, filesystem: &str) {
    let volume =
        PathBuf::from(std::env::var_os(variable).expect("real-volume fixture is required"));
    assert!(volume.is_absolute() && volume.is_dir());
    verify_real_volume(&volume, filesystem);
    let volume_temp = tempfile::tempdir_in(&volume).unwrap();
    let ntfs_temp = tempfile::tempdir().unwrap();
    verify_real_volume(ntfs_temp.path(), "NTFS");
    let root_path = volume_temp.path().join("root");
    fs::create_dir_all(root_path.join("allowed")).unwrap();
    fs::write(root_path.join("allowed/note.md"), "before").unwrap();
    fs::write(root_path.join("forbidden.md"), "not granted").unwrap();
    fs::write(volume_temp.path().join("outside.md"), "outside").unwrap();
    let native_root = ntfs_temp.path().join("ntfs-root");
    fs::create_dir(&native_root).unwrap();
    fs::write(native_root.join("note.md"), "ntfs").unwrap();
    let library = FsLibrary::new(
        LibraryConfig::new(ntfs_temp.path().join("data"), ntfs_temp.path().join("home")),
        Arc::new(TestClock(Instant::now())),
    )
    .unwrap();
    let volume_spec = spec(root_path.clone(), Some(vec!["allowed".into()]));
    let ntfs_spec = spec(native_root, None);
    let id = volume_spec.root_id;

    library.set_roots(vec![volume_spec.clone()]).await.unwrap();
    assert_eq!(
        library.read(id, "allowed/note.md").await.unwrap().text,
        "before"
    );
    assert_eq!(library.list(id, "allowed").await.unwrap().entries.len(), 1);
    for path in ["forbidden.md", "../outside.md"] {
        assert!(matches!(
            library.read(id, path).await,
            Err(LibraryError::PathNotAllowed(_))
        ));
        assert!(library.create(id, path, "must not write").await.is_err());
    }
    assert!(matches!(
        library.list(id, "not-granted").await,
        Err(LibraryError::PathNotAllowed(_))
    ));
    assert!(
        library
            .save(
                id,
                "forbidden.md",
                request("unauthorized".into(), "must not write")
            )
            .await
            .is_err()
    );

    let volume_before = snapshot(volume_temp.path());
    let data_before = snapshot(ntfs_temp.path());
    let before = library.read(id, "allowed/note.md").await.unwrap();
    assert_unsupported(
        library
            .save(
                id,
                "allowed/note.md",
                request(before.revision, "must not save"),
            )
            .await
            .unwrap_err(),
    );
    assert_unsupported(
        library
            .create(id, "allowed/created.md", "must not create")
            .await
            .unwrap_err(),
    );
    assert_eq!(
        snapshot(volume_temp.path()),
        volume_before,
        "no partial or temporary files may appear on the target volume"
    );
    assert_eq!(
        snapshot(ntfs_temp.path()),
        data_before,
        "unsupported writes must not create recovery or temporary data"
    );
    assert_eq!(
        library.read(id, "allowed/note.md").await.unwrap().text,
        "before"
    );

    let moved = volume_temp.path().join("moved");
    assert_eq!(
        fs::rename(&root_path, &moved).unwrap_err().raw_os_error(),
        Some(32),
        "legacy root must retain its strict pin"
    );
    library
        .set_roots(vec![volume_spec, ntfs_spec.clone()])
        .await
        .unwrap();
    assert_eq!(library.roots().await.len(), 2);
    assert_eq!(
        library.read(id, "allowed/note.md").await.unwrap().text,
        "before"
    );
    let ntfs_before = library.read(ntfs_spec.root_id, "note.md").await.unwrap();
    assert_eq!(ntfs_before.text, "ntfs");
    let saved = library
        .save(
            ntfs_spec.root_id,
            "note.md",
            request(ntfs_before.revision, "ntfs saved"),
        )
        .await
        .unwrap();
    assert!(matches!(saved, SaveOutcome::Saved { .. }), "{saved:?}");
    assert_eq!(
        library
            .read(ntfs_spec.root_id, "note.md")
            .await
            .unwrap()
            .text,
        "ntfs saved"
    );
    library
        .create(ntfs_spec.root_id, "created.md", "ntfs created")
        .await
        .unwrap();
    assert_eq!(
        library
            .read(ntfs_spec.root_id, "created.md")
            .await
            .unwrap()
            .text,
        "ntfs created"
    );
    assert_eq!(
        fs::rename(&root_path, &moved).unwrap_err().raw_os_error(),
        Some(32)
    );
    assert_eq!(
        snapshot(volume_temp.path()),
        volume_before,
        "mixed-root writes must not alter the unsupported volume"
    );
    library.set_roots(vec![]).await.unwrap();
    drop(library);
    fs::rename(&root_path, &moved).unwrap();
    eprintln!(
        "real {filesystem}: reads/listing, grant refusal, early mutation refusal without partial files, strict pin, mixed roots and NTFS writes passed"
    );
}

#[tokio::test]
#[ignore = "requires an explicitly prepared FAT32 volume in OPHIO_LIBRARY_TEST_FAT32"]
async fn real_fat32_volume_keeps_legacy_identity_and_grants() {
    exercise_volume("OPHIO_LIBRARY_TEST_FAT32", "FAT32").await;
}

#[tokio::test]
#[ignore = "requires an explicitly prepared exFAT volume in OPHIO_LIBRARY_TEST_EXFAT"]
async fn real_exfat_volume_keeps_legacy_identity_and_grants() {
    exercise_volume("OPHIO_LIBRARY_TEST_EXFAT", "exFAT").await;
}
