// SPDX-License-Identifier: Apache-2.0
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
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicI64, Ordering},
    },
    time::{Duration, Instant},
};

#[derive(Debug)]
struct FakeClock {
    now: AtomicI64,
    monotonic: Instant,
}
impl Clock for FakeClock {
    fn now(&self) -> Timestamp {
        Timestamp(self.now.load(Ordering::SeqCst))
    }
    fn monotonic(&self) -> Instant {
        self.monotonic
    }
}
struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    id: LibraryRootId,
    fs: FsLibrary,
    clock: Arc<FakeClock>,
    config: LibraryConfig,
}
impl Fixture {
    async fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        std::fs::create_dir(&root).unwrap();
        let clock = Arc::new(FakeClock {
            now: AtomicI64::new(1000),
            monotonic: Instant::now(),
        });
        let mut config = LibraryConfig::new(temp.path().join("data"), temp.path().join("home"));
        config.max_file_bytes = 4096;
        config.retention = Duration::from_secs(1);
        let fs = FsLibrary::new(config.clone(), clock.clone()).unwrap();
        let id = LibraryRootId::new();
        fs.set_roots(vec![spec(id, root.clone())]).await.unwrap();
        Self {
            _temp: temp,
            root,
            id,
            fs,
            clock,
            config,
        }
    }
    fn write(&self, path: &str, bytes: impl AsRef<[u8]>) {
        let path = self.root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }
}
fn spec(id: LibraryRootId, path: PathBuf) -> RootSpec {
    RootSpec {
        root_id: id,
        path,
        label: "Fixture".into(),
        kind: RootKind::Project,
        writable: true,
        project_id: None,
        harness: None,
        only: None,
    }
}
fn request(revision: String, text: &str) -> SaveRequest {
    SaveRequest {
        text: text.into(),
        base_revision: revision,
        newline: NewlineStyle::Lf,
        bom: false,
    }
}

#[tokio::test]
async fn portable_path_escapes_and_aliases_are_rejected() {
    let f = Fixture::new().await;
    f.write("hello.md", "hello");
    for path in [
        "../hello.md",
        "/hello.md",
        "a/../../b",
        "hello\0.md",
        "C:/hello.md",
        "C:hello.md",
        "\\\\server\\share",
        "//server/share",
        "foo\\bar",
        "name:stream",
        "NUL",
        "CON.txt",
        "LPT1.md",
        "name.",
        "name ",
    ] {
        assert!(
            matches!(
                f.fs.read(f.id, path).await,
                Err(LibraryError::PathNotAllowed(_))
            ),
            "{path:?}"
        );
        assert!(
            matches!(
                f.fs.create(f.id, path, "x").await,
                Err(LibraryError::PathNotAllowed(_))
            ),
            "{path:?}"
        );
    }
    assert_eq!(f.fs.read(f.id, "./hello.md").await.unwrap().text, "hello");
}

#[cfg(unix)]
#[tokio::test]
async fn symlinks_loops_replaced_roots_hardlinks_and_special_files_are_rejected() {
    use std::os::unix::{ffi::OsStringExt, fs::symlink};
    let f = Fixture::new().await;
    let outside = f._temp.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("secret.md"), "outside").unwrap();
    symlink(&outside, f.root.join("escape")).unwrap();
    symlink("loop", f.root.join("loop")).unwrap();
    symlink("inside.md", f.root.join("alias")).unwrap();
    f.write("inside.md", "inside");
    for path in ["escape/secret.md", "loop", "alias"] {
        assert!(matches!(
            f.fs.read(f.id, path).await,
            Err(LibraryError::PathNotAllowed(_))
        ));
    }
    assert!(f.fs.create(f.id, "escape/new.md", "x").await.is_err());
    std::fs::hard_link(outside.join("secret.md"), f.root.join("hard.md")).unwrap();
    assert!(matches!(
        f.fs.read(f.id, "hard.md").await,
        Err(LibraryError::PathNotAllowed(_))
    ));
    rustix::fs::mknodat(
        rustix::fs::CWD,
        f.root.join("pipe"),
        rustix::fs::FileType::Fifo,
        rustix::fs::Mode::RUSR,
        0,
    )
    .unwrap();
    assert!(matches!(
        f.fs.read(f.id, "pipe").await,
        Err(LibraryError::PathNotAllowed(_))
    ));
    // A name that is not UTF-8 cannot be a path; it is left out of the
    // listing, as search leaves it out, and the rest of the folder still lists.
    std::fs::write(f.root.join(std::ffi::OsString::from_vec(vec![0xff])), "x").unwrap();
    let names: Vec<_> =
        f.fs.list(f.id, "")
            .await
            .unwrap()
            .entries
            .into_iter()
            .map(|e| e.name)
            .collect();
    assert_eq!(names, ["alias", "escape", "hard.md", "inside.md", "loop"]);
    std::fs::rename(&f.root, f._temp.path().join("old-root")).unwrap();
    symlink(&outside, &f.root).unwrap();
    assert!(matches!(
        f.fs.read(f.id, "secret.md").await,
        Err(LibraryError::PathNotAllowed(_))
    ));
    assert!(f.fs.create(f.id, "new.md", "x").await.is_err());
    assert!(!outside.join("new.md").exists());
}

#[tokio::test]
async fn open_gives_the_exact_bytes_of_any_file_inside_its_root() {
    use std::io::Read;
    let f = Fixture::new().await;
    // Neither text nor within the editing size, as a scan or picture is.
    let bytes: Vec<u8> = (0..10_000u32).map(|i| (i % 251) as u8 ^ 0x80).collect();
    std::fs::write(f.root.join("scan.pdf"), &bytes).unwrap();
    assert!(f.fs.read(f.id, "scan.pdf").await.is_err());
    let mut opened = Vec::new();
    f.fs.open(f.id, "scan.pdf")
        .await
        .unwrap()
        .read_to_end(&mut opened)
        .unwrap();
    assert!(opened == bytes);
    let outside = f._temp.path().join("outside.bin");
    std::fs::write(&outside, b"private").unwrap();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&outside, f.root.join("escape.bin")).unwrap();
        assert!(f.fs.open(f.id, "escape.bin").await.is_err());
    }
    assert!(f.fs.open(f.id, "../outside.bin").await.is_err());
    assert!(f.fs.open(LibraryRootId::new(), "scan.pdf").await.is_err());
}

#[tokio::test]
async fn saves_are_revisioned_and_preserve_source_fidelity() {
    let f = Fixture::new().await;
    f.write(
        "note.md",
        b"\xef\xbb\xbf---\r\ncustom: untouched\r\n---\r\nbody\r\n",
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            f.root.join("note.md"),
            std::fs::Permissions::from_mode(0o640),
        )
        .unwrap();
    }
    let original = f.fs.read(f.id, "note.md").await.unwrap();
    assert!(original.has_bom);
    assert_eq!(original.newline, NewlineStyle::CrLf);
    let mut save = request(
        original.revision.clone(),
        "---\ncustom: untouched\n---\nchanged\n",
    );
    save.bom = true;
    save.newline = NewlineStyle::CrLf;
    assert!(matches!(
        f.fs.save(f.id, "note.md", save).await.unwrap(),
        SaveOutcome::Saved { .. }
    ));
    assert_eq!(
        std::fs::read(f.root.join("note.md")).unwrap(),
        b"\xef\xbb\xbf---\r\ncustom: untouched\r\n---\r\nchanged\r\n"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(f.root.join("note.md"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o640
        );
    }
    f.write("note.md", "external editor");
    let result =
        f.fs.save(f.id, "note.md", request(original.revision, "draft"))
            .await
            .unwrap();
    let SaveOutcome::Conflict {
        current: Some(current),
    } = result
    else {
        panic!("expected conflict")
    };
    assert_eq!(current.text, "external editor");
    assert_eq!(
        std::fs::read_to_string(f.root.join("note.md")).unwrap(),
        "external editor"
    );
    assert!(matches!(
        f.fs.save(f.id, "gone.md", request(String::new(), "x"))
            .await
            .unwrap(),
        SaveOutcome::Missing
    ));
}

#[tokio::test]
async fn mixed_newlines_are_lossless_and_binary_size_readonly_errors_are_typed() {
    let f = Fixture::new().await;
    f.write("mixed.md", "a\r\nb\nc\r\n");
    let file = f.fs.read(f.id, "mixed.md").await.unwrap();
    assert_eq!(file.newline, NewlineStyle::Mixed);
    let mut req = request(file.revision, &file.text);
    req.newline = NewlineStyle::Mixed;
    f.fs.save(f.id, "mixed.md", req).await.unwrap();
    assert_eq!(
        std::fs::read(f.root.join("mixed.md")).unwrap(),
        b"a\r\nb\nc\r\n"
    );
    f.write("binary.md", b"a\0b");
    f.write("utf16.md", [0xff, 0xfe, 0]);
    f.write("large.md", vec![b'a'; 4097]);
    assert!(matches!(
        f.fs.read(f.id, "binary.md").await,
        Err(LibraryError::NotEditable)
    ));
    assert!(matches!(
        f.fs.read(f.id, "utf16.md").await,
        Err(LibraryError::NotEditable)
    ));
    assert!(matches!(
        f.fs.read(f.id, "large.md").await,
        Err(LibraryError::TooLarge(4097))
    ));
    let mut root = spec(f.id, f.root.clone());
    root.writable = false;
    f.fs.set_roots(vec![root]).await.unwrap();
    assert!(matches!(
        f.fs.create(f.id, "new.md", "x").await,
        Err(LibraryError::ReadOnly)
    ));
    assert!(matches!(
        f.fs.read(LibraryRootId::new(), "new.md").await,
        Err(LibraryError::UnknownRoot)
    ));
}

#[tokio::test]
async fn create_rename_trash_restore_conflict_restart_and_retention() {
    let f = Fixture::new().await;
    let revision = f.fs.create(f.id, "a.md", "hello").await.unwrap();
    assert!(matches!(
        f.fs.create(f.id, "a.md", "clobber").await,
        Err(LibraryError::AlreadyExists)
    ));
    f.fs.create(f.id, "occupied.md", "keep").await.unwrap();
    assert!(matches!(
        f.fs.rename(f.id, "a.md", "occupied.md", &revision).await,
        Err(LibraryError::AlreadyExists)
    ));
    assert!(matches!(
        f.fs.rename(f.id, "a.md", "b.md", "stale").await,
        Err(LibraryError::Conflict)
    ));
    f.fs.rename(f.id, "a.md", "b.md", &revision).await.unwrap();
    assert!(!f.root.join("a.md").exists());
    let entry = f.fs.trash(f.id, "b.md", &revision).await.unwrap();
    assert!(!f.root.join("b.md").exists());
    assert!(entry.stored_at.exists());
    let reopened = FsLibrary::new(f.config.clone(), f.clock.clone()).unwrap();
    reopened
        .set_roots(vec![spec(f.id, f.root.clone())])
        .await
        .unwrap();
    assert_eq!(reopened.trash_entries().await.unwrap().len(), 1);
    reopened.create(f.id, "b.md", "new owner").await.unwrap();
    assert!(matches!(
        reopened.restore(&entry.trash_id).await,
        Err(LibraryError::AlreadyExists)
    ));
    assert!(entry.stored_at.exists());
    std::fs::remove_file(f.root.join("b.md")).unwrap();
    reopened.restore(&entry.trash_id).await.unwrap();
    assert_eq!(
        std::fs::read_to_string(f.root.join("b.md")).unwrap(),
        "hello"
    );
    let entry = reopened.trash(f.id, "b.md", &revision).await.unwrap();
    assert_eq!(reopened.purge_trash().await.unwrap(), 0);
    f.clock.now.store(2001, Ordering::SeqCst);
    assert_eq!(reopened.purge_trash().await.unwrap(), 1);
    assert!(!entry.stored_at.exists());
}

#[tokio::test]
async fn search_is_bounded_ignores_noise_and_lists_hidden_files() {
    let f = Fixture::new().await;
    for path in [
        "a.md",
        "b.md",
        ".hidden.md",
        "sub/c.md",
        "target/a.md",
        "node_modules/a.md",
        ".git/a.md",
        "ignored.md",
        "sub/ignored.md",
    ] {
        f.write(path, "first\nNeedle Phrase\nlast");
    }
    f.write(".gitignore", "ignored.md\n");
    f.write("large.md", vec![b'a'; 4097]);
    f.write("binary.md", b"Needle Phrase\0");
    let hits = f.fs.search(Some(f.id), "nEeDlE pHrAsE", 2).await.unwrap();
    assert_eq!(hits.len(), 2);
    assert!(hits.iter().all(|h| h.line == Some(2)));
    let hits = f.fs.search(None, "Needle Phrase", 100).await.unwrap();
    assert_eq!(hits.len(), 4);
    assert!(hits.iter().all(|h| !h.path.contains("ignored")
        && !h.path.starts_with("target/")
        && !h.path.starts_with("node_modules/")));
    assert!(f.fs.search(None, "needle", 0).await.unwrap().is_empty());
    let list = f.fs.list(f.id, "").await.unwrap();
    assert!(
        list.entries
            .iter()
            .find(|e| e.name == ".hidden.md")
            .unwrap()
            .hidden
    );
}

// A file is found by its name, whatever its contents say. A name match has no
// line, so nothing invents one.
#[tokio::test]
async fn search_finds_a_file_by_its_name() {
    let f = Fixture::new().await;
    f.write("release-checklist.md", "Check the build");
    f.write("notes/Release-Notes.md", "");
    f.write("empty-release.md", "");
    f.write("other.md", "release day\nnothing\nRelease again");
    f.write("ignored-release.md", "");
    f.write(".gitignore", "ignored-*\n");
    let hits =
        f.fs.search(Some(f.id), "release-checklist", 10)
            .await
            .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].path, "release-checklist.md");
    assert_eq!(hits[0].line, None);
    assert_eq!(hits[0].snippet, "");
    let mut found: Vec<_> =
        f.fs.search(None, "RELEASE", 100)
            .await
            .unwrap()
            .into_iter()
            .map(|h| (h.path, h.line))
            .collect();
    found.sort();
    assert_eq!(
        found,
        [
            ("empty-release.md".into(), None),
            ("notes/Release-Notes.md".into(), None),
            ("other.md".into(), Some(1)),
            ("other.md".into(), Some(3)),
            ("release-checklist.md".into(), None),
        ]
    );
    // A file whose name and contents both match is listed by name first.
    f.write("build.md", "the build");
    let hits = f.fs.search(Some(f.id), "build", 10).await.unwrap();
    let lines: Vec<_> = hits
        .iter()
        .filter(|h| h.path == "build.md")
        .map(|h| h.line)
        .collect();
    assert_eq!(lines, [None, Some(1)]);
    // The limit counts name matches like any other.
    assert_eq!(f.fs.search(None, "release", 2).await.unwrap().len(), 2);
}

// A folder is listed with the entries the phone can open, the same ones
// search reads. A name that is not UTF-8, or that a path may not use, is
// left out rather than failing the whole folder or showing what cannot open.
#[cfg(unix)]
#[tokio::test]
async fn listing_leaves_out_names_a_path_cannot_use() {
    use std::os::unix::ffi::OsStrExt;
    let f = Fixture::new().await;
    f.write("sub/kept.md", "kept");
    std::fs::write(
        f.root
            .join("sub")
            .join(std::ffi::OsStr::from_bytes(b"\xff.md")),
        "x",
    )
    .unwrap();
    for name in [
        "Meeting: 10am.md",
        "back\\slash.md",
        "trailing.",
        "CON.md",
        "tab\t.md",
    ] {
        f.write(&format!("sub/{name}"), "x");
    }
    let names: Vec<_> =
        f.fs.list(f.id, "sub")
            .await
            .unwrap()
            .entries
            .into_iter()
            .map(|e| e.path)
            .collect();
    assert_eq!(names, ["sub/kept.md"]);
    for path in names {
        f.fs.read(f.id, &path).await.unwrap();
    }
}

// A root's tree belongs to whoever writes into it. A folder the watcher
// cannot watch, such as one without read permission, must not keep the roots
// from being installed, and so the companion from starting. (Run as root,
// nothing is unreadable and this passes either way.)
#[cfg(unix)]
#[tokio::test]
async fn a_folder_that_cannot_be_watched_does_not_refuse_the_root() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new().await;
    f.write("note.md", "kept");
    f.write("closed/inner.md", "x");
    let closed = f.root.join("closed");
    std::fs::set_permissions(&closed, std::fs::Permissions::from_mode(0o000)).unwrap();
    let installed = f.fs.set_roots(vec![spec(f.id, f.root.clone())]).await;
    std::fs::set_permissions(&closed, std::fs::Permissions::from_mode(0o755)).unwrap();
    installed.unwrap();
    assert_eq!(f.fs.read(f.id, "note.md").await.unwrap().text, "kept");
}

// The phone searches every root at once. A root that has gone away, or a
// folder that cannot be read, leaves the rest searchable. (The unreadable
// folder fails only for an ordinary user; root reads everything.)
#[cfg(unix)]
#[tokio::test]
async fn search_goes_on_past_a_missing_root_and_an_unreadable_folder() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new().await;
    f.write("found.md", "needle");
    f.write("closed/hidden.md", "needle");
    let gone = f._temp.path().join("gone");
    std::fs::create_dir(&gone).unwrap();
    f.fs.set_roots(vec![
        spec(f.id, f.root.clone()),
        spec(LibraryRootId::new(), gone.clone()),
    ])
    .await
    .unwrap();
    std::fs::remove_dir(&gone).unwrap();
    let closed = f.root.join("closed");
    std::fs::set_permissions(&closed, std::fs::Permissions::from_mode(0o000)).unwrap();
    let hits = f.fs.search(None, "needle", 10).await;
    std::fs::set_permissions(&closed, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(hits.unwrap().iter().any(|h| h.path == "found.md"));
}

// A trash or recovery folder without a readable record, left when the
// companion stopped between making the folder and writing its record, is
// passed over: the rest is still listed, and old trash is still purged.
#[tokio::test]
async fn a_folder_without_its_record_is_passed_over() {
    let f = Fixture::new().await;
    let revision = f.fs.create(f.id, "a.md", "hello").await.unwrap();
    let entry = f.fs.trash(f.id, "a.md", &revision).await.unwrap();
    let data = entry
        .stored_at
        .ancestors()
        .find(|p| p.ends_with("trash"))
        .and_then(|p| p.parent())
        .unwrap()
        .to_owned();
    for kind in ["trash", "recovery"] {
        std::fs::create_dir_all(data.join(kind).join(uuid::Uuid::new_v4().to_string())).unwrap();
    }
    assert_eq!(f.fs.trash_entries().await.unwrap().len(), 1);
    assert_eq!(f.fs.recoverable().await.unwrap().len(), 1);
    f.fs.recovery_entries().await.unwrap();
    f.clock.now.store(2001, Ordering::SeqCst);
    assert_eq!(f.fs.purge_trash().await.unwrap(), 1);
}

async fn names(fs: &FsLibrary, id: LibraryRootId, dir: &str) -> Vec<String> {
    let mut names: Vec<_> = fs
        .list(id, dir)
        .await
        .unwrap()
        .entries
        .into_iter()
        .map(|e| e.name)
        .collect();
    names.sort();
    names
}

// A root limited to some paths, as an agent's own folder is, lists, opens,
// searches, writes and announces those alone, never what is beside them.
#[tokio::test]
async fn a_limited_root_reaches_only_its_paths() {
    let f = Fixture::new().await;
    for path in [
        "CLAUDE.md",
        "agents/a.md",
        "agentsx/b.md",
        "projects/p/memory/m.md",
        "projects/p/t.jsonl",
        "projects/q/memory/n.md",
        "settings.md",
    ] {
        f.write(path, "Needle");
    }
    let id = LibraryRootId::new();
    let limited = |only: &[&str]| RootSpec {
        only: Some(only.iter().map(|p| (*p).to_owned()).collect()),
        ..spec(id, f.root.clone())
    };
    for bad in [&["../x"][..], &[""], &["a//b"], &["./a"]] {
        assert!(f.fs.set_roots(vec![limited(bad)]).await.is_err(), "{bad:?}");
    }
    f.fs.set_roots(vec![limited(&[
        "CLAUDE.md",
        "agents",
        "projects/p/memory",
        "commands",
    ])])
    .await
    .unwrap();
    assert_eq!(
        names(&f.fs, id, "").await,
        ["CLAUDE.md", "agents", "projects"]
    );
    assert_eq!(names(&f.fs, id, "projects").await, ["p"]);
    assert_eq!(names(&f.fs, id, "projects/p").await, ["memory"]);
    assert_eq!(names(&f.fs, id, "projects/p/memory").await, ["m.md"]);
    let refused = |r: Result<_, LibraryError>| matches!(r, Err(LibraryError::PathNotAllowed(_)));
    assert!(refused(f.fs.list(id, "projects/q").await.map(|_| ())));
    assert!(refused(f.fs.list(id, "agentsx").await.map(|_| ())));
    for path in [
        "settings.md",
        "projects/p/t.jsonl",
        "agentsx/b.md",
        "projects/q/memory/n.md",
    ] {
        assert!(refused(f.fs.read(id, path).await.map(|_| ())), "{path}");
    }
    let revision = f.fs.read(id, "agents/a.md").await.unwrap().revision;
    f.fs.create(id, "agents/new.md", "made").await.unwrap();
    assert!(refused(f.fs.create(id, "new.md", "made").await.map(|_| ())));
    assert!(refused(
        f.fs.rename(id, "agents/a.md", "a.md", &revision)
            .await
            .map(|_| ())
    ));
    assert!(f.root.join("agents/a.md").exists());
    let mut found: Vec<_> =
        f.fs.search(None, "needle", 100)
            .await
            .unwrap()
            .into_iter()
            .map(|h| h.path)
            .collect();
    found.sort();
    assert_eq!(
        found,
        ["CLAUDE.md", "agents/a.md", "projects/p/memory/m.md"]
    );
    // A change beside its paths says nothing; one to them is announced.
    let mut changes = f.fs.changes();
    f.write("settings.md", "after");
    f.write("agents/a.md", "after");
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let event = changes.recv().await.unwrap();
            assert_ne!(event.path, "settings.md");
            if event.path == "agents/a.md" && event.revision.is_some() {
                break;
            }
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn external_edits_emit_revision_and_delete_notifications() {
    let f = Fixture::new().await;
    f.write("watched.md", "before");
    let mut changes = f.fs.changes();
    f.write("watched.md", "after");
    let expected = f.fs.read(f.id, "watched.md").await.unwrap().revision;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let event = changes.recv().await.unwrap();
            if event.path == "watched.md" && event.revision.as_deref() == Some(&expected) {
                break;
            }
        }
    })
    .await
    .unwrap();
    std::fs::remove_file(f.root.join("watched.md")).unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let event = changes.recv().await.unwrap();
            if event.path == "watched.md" && event.revision.is_none() {
                break;
            }
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn same_filesystem_trash_and_restore_retain_open_file_identity() {
    use std::io::Write;
    let f = Fixture::new().await;
    let revision = f.fs.create(f.id, "open.md", "before").await.unwrap();
    let mut writer = std::fs::OpenOptions::new()
        .append(true)
        .open(f.root.join("open.md"))
        .unwrap();
    let entry = f.fs.trash(f.id, "open.md", &revision).await.unwrap();
    writer.write_all(b" during trash").unwrap();
    writer.sync_all().unwrap();
    assert_eq!(
        std::fs::read(&entry.stored_at).unwrap(),
        b"before during trash"
    );
    f.fs.restore(&entry.trash_id).await.unwrap();
    writer.write_all(b" after restore").unwrap();
    writer.sync_all().unwrap();
    assert_eq!(
        f.fs.read(f.id, "open.md").await.unwrap().text,
        "before during trash after restore"
    );
}

#[tokio::test]
async fn checked_recovery_survives_reload_refuses_occupants_and_restores_a_copy() {
    let f = Fixture::new().await;
    f.write("guide.md", "original\n");
    let base = f.fs.read(f.id, "guide.md").await.unwrap();
    let deleted = f.fs.trash(f.id, "guide.md", &base.revision).await.unwrap();
    let fs = FsLibrary::new(f.config.clone(), f.clock.clone()).unwrap();
    fs.set_roots(vec![spec(f.id, f.root.clone())])
        .await
        .unwrap();
    let items = fs.recoverable().await.unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].trash_id, deleted.trash_id);
    assert_eq!(items[0].deleted_at, Timestamp(1000));
    assert_eq!(items[0].size, 9);
    assert_eq!(
        fs.read_trash(&deleted.trash_id).await.unwrap().text,
        "original\n"
    );
    assert!(matches!(
        fs.restore_checked(&deleted.trash_id, "stale", None).await,
        Err(LibraryError::Conflict)
    ));
    f.write("guide.md", "new occupant");
    assert!(matches!(
        fs.restore_checked(&deleted.trash_id, &base.revision, None)
            .await,
        Err(LibraryError::AlreadyExists)
    ));
    assert_eq!(
        std::fs::read_to_string(f.root.join("guide.md")).unwrap(),
        "new occupant"
    );
    assert!(
        fs.restore_checked(&deleted.trash_id, &base.revision, Some("../escape.md"))
            .await
            .is_err()
    );
    let restored = fs
        .restore_checked(&deleted.trash_id, &base.revision, Some("guide-copy.md"))
        .await
        .unwrap();
    assert_eq!(restored.path, "guide-copy.md");
    assert_eq!(fs.recoverable().await.unwrap().len(), 1);
    std::fs::remove_file(f.root.join("guide.md")).unwrap();
    fs.restore_checked(&deleted.trash_id, &base.revision, None)
        .await
        .unwrap();
    assert!(fs.recoverable().await.unwrap().is_empty());
}

#[tokio::test]
async fn move_checks_revision_and_never_replaces_a_destination() {
    let f = Fixture::new().await;
    f.write("old.md", "first");
    f.write("docs/taken.md", "existing");
    let old = f.fs.read(f.id, "old.md").await.unwrap();
    assert!(matches!(
        f.fs.rename(f.id, "old.md", "docs/new.md", "stale").await,
        Err(LibraryError::Conflict)
    ));
    assert!(matches!(
        f.fs.rename(f.id, "old.md", "docs/taken.md", &old.revision)
            .await,
        Err(LibraryError::AlreadyExists)
    ));
    f.fs.rename(f.id, "old.md", "docs/new.md", &old.revision)
        .await
        .unwrap();
    assert!(!f.root.join("old.md").exists());
    assert_eq!(f.fs.read(f.id, "docs/new.md").await.unwrap().text, "first");
    assert_eq!(
        f.fs.read(f.id, "docs/taken.md").await.unwrap().text,
        "existing"
    );
}

async fn confirmed_save(f: &Fixture) -> (String, PathBuf) {
    f.write("confirmed.md", "base");
    let base = f.fs.read(f.id, "confirmed.md").await.unwrap();
    assert!(matches!(
        f.fs.save(
            f.id,
            "confirmed.md",
            SaveRequest {
                base_revision: base.revision,
                text: "saved".into(),
                newline: NewlineStyle::Lf,
                bom: false,
            }
        )
        .await
        .unwrap(),
        SaveOutcome::Saved { .. }
    ));
    let record = std::fs::read_dir(f.config.data_dir.join("recovery"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .file_name()
        .into_string()
        .unwrap();
    let backup = f.root.join(format!(".ophio-save-{record}-old"));
    assert!(backup.exists());
    assert!(
        f.config
            .data_dir
            .join("recovery")
            .join(&record)
            .join("confirmed")
            .exists()
    );
    (record, backup)
}

#[tokio::test]
async fn confirmed_save_is_quiet_then_removes_backup_and_record_after_grace() {
    let f = Fixture::new().await;
    let (id, backup) = confirmed_save(&f).await;
    assert!(f.fs.list_recovery().await.unwrap().is_empty());
    assert!(
        f.fs.list(f.id, "")
            .await
            .unwrap()
            .entries
            .iter()
            .all(|e| !e.name.starts_with(".ophio-save-"))
    );
    let deadline = 1000 + f.config.confirmed_save_grace.as_millis() as i64;
    f.clock.now.store(deadline - 1, Ordering::SeqCst);
    f.fs.purge_trash().await.unwrap();
    assert!(backup.exists());
    f.clock.now.store(deadline, Ordering::SeqCst);
    f.fs.purge_trash().await.unwrap();
    assert!(!backup.exists());
    assert!(!f.config.data_dir.join("recovery").join(id).exists());
    assert_eq!(
        std::fs::read(f.root.join("confirmed.md")).unwrap(),
        b"saved"
    );
}

#[tokio::test]
async fn files_named_before_the_rename_stay_hidden_and_are_cleaned_up() {
    let f = Fixture::new().await;
    let (id, backup) = confirmed_save(&f).await;
    // The same save's backup and a create's temporary file, as an earlier
    // version named them.
    let earlier = f.root.join(format!(".companion-save-{id}-old"));
    std::fs::rename(&backup, &earlier).unwrap();
    f.write(".companion-create-interrupted", "Needle Phrase");
    let list = f.fs.list(f.id, "").await.unwrap();
    assert_eq!(
        list.entries.iter().map(|e| &e.name[..]).collect::<Vec<_>>(),
        ["confirmed.md"]
    );
    assert!(
        f.fs.search(None, "Needle Phrase", 100)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        f.fs.read(f.id, ".companion-create-interrupted")
            .await
            .is_err()
    );
    f.clock.now.store(
        1000 + f.config.confirmed_save_grace.as_millis() as i64,
        Ordering::SeqCst,
    );
    f.fs.purge_trash().await.unwrap();
    assert!(!earlier.exists());
    assert!(!f.config.data_dir.join("recovery").join(id).exists());
}

#[tokio::test]
async fn own_files_are_recognized_in_any_letter_case() {
    let f = Fixture::new().await;
    f.write(".OPHIO-create-interrupted", "Needle Phrase");
    f.write(".Companion-save-earlier-old", "Needle Phrase");
    f.write(".ophiology.md", "kept");
    let list = f.fs.list(f.id, "").await.unwrap();
    assert_eq!(
        list.entries.iter().map(|e| &e.name[..]).collect::<Vec<_>>(),
        [".ophiology.md"]
    );
    assert!(
        f.fs.search(None, "Needle Phrase", 100)
            .await
            .unwrap()
            .is_empty()
    );
    for path in [
        ".OPHIO-create-interrupted",
        ".Companion-save-earlier-old",
        "notes/.Ophio-save-x-new",
    ] {
        assert!(
            matches!(
                f.fs.read(f.id, path).await,
                Err(LibraryError::PathNotAllowed(_))
            ),
            "{path:?}"
        );
        assert!(
            matches!(
                f.fs.create(f.id, path, "x").await,
                Err(LibraryError::PathNotAllowed(_))
            ),
            "{path:?}"
        );
    }
}

#[tokio::test]
async fn late_write_through_displaced_handle_keeps_recovery_for_review() {
    use std::io::{Seek, Write};
    let f = Fixture::new().await;
    f.write("confirmed.md", "base");
    let mut writer = std::fs::OpenOptions::new()
        .write(true)
        .open(f.root.join("confirmed.md"))
        .unwrap();
    let (id, backup) = confirmed_save(&f).await;
    writer.rewind().unwrap();
    writer.write_all(b"late native words").unwrap();
    writer.sync_all().unwrap();
    // Ordinary confirmed saves remain quiet for the entire grace window.
    assert!(f.fs.list_recovery().await.unwrap().is_empty());
    f.clock.now.store(
        1000 + f.config.confirmed_save_grace.as_millis() as i64,
        Ordering::SeqCst,
    );
    f.fs.purge_trash().await.unwrap();
    let entries = f.fs.list_recovery().await.unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].recovery_id, id);
    let snapshot = f.fs.recovery(&id).await.unwrap();
    assert_eq!(snapshot.displaced.unwrap(), b"late native words");
    assert_eq!(snapshot.base, b"base");
    assert!(backup.exists());
    // Once detected, the discrepancy requires explicit reconciliation.
    std::fs::write(&backup, "base").unwrap();
    f.fs.purge_trash().await.unwrap();
    assert!(backup.exists());
    assert_eq!(f.fs.list_recovery().await.unwrap().len(), 1);
}

#[tokio::test]
async fn startup_sweeps_existing_confirmed_save_only_after_matching_root_is_registered() {
    let f = Fixture::new().await;
    let (id, backup) = confirmed_save(&f).await;
    f.clock.now.store(
        1000 + f.config.confirmed_save_grace.as_millis() as i64,
        Ordering::SeqCst,
    );
    let config = f.config.clone();
    let clock = f.clock.clone();
    drop(f.fs);
    let reopened = FsLibrary::new(config.clone(), clock).unwrap();
    // Persisted paths are not grants. Also refuse a remapped/read-only root.
    reopened.set_roots(vec![]).await.unwrap();
    assert!(backup.exists());
    let mut read_only = spec(f.id, f.root.clone());
    read_only.writable = false;
    reopened.set_roots(vec![read_only]).await.unwrap();
    assert!(backup.exists());
    reopened
        .set_roots(vec![spec(f.id, f.root.clone())])
        .await
        .unwrap();
    assert!(!backup.exists());
    assert!(!config.data_dir.join("recovery").join(id).exists());
    assert!(reopened.list_recovery().await.unwrap().is_empty());
}

#[tokio::test]
async fn unmarked_legacy_save_is_never_inferred_to_be_confirmed() {
    let f = Fixture::new().await;
    let (id, backup) = confirmed_save(&f).await;
    std::fs::remove_file(
        f.config
            .data_dir
            .join("recovery")
            .join(&id)
            .join("confirmed"),
    )
    .unwrap();
    f.clock.now.store(i64::MAX, Ordering::SeqCst);
    f.fs.purge_trash().await.unwrap();
    assert!(backup.exists());
    assert_eq!(f.fs.list_recovery().await.unwrap()[0].recovery_id, id);
}

#[tokio::test]
async fn interrupted_cleanup_resumes_but_rechecks_any_remaining_backup() {
    for backup_remains in [false, true] {
        let f = Fixture::new().await;
        let (id, backup) = confirmed_save(&f).await;
        let record = f.config.data_dir.join("recovery").join(&id);
        std::fs::write(record.join("cleanup"), "").unwrap();
        if backup_remains {
            // A writer changed the inode after the pre-crash check.
            std::fs::write(&backup, "late words after cleanup began").unwrap();
        } else {
            std::fs::remove_file(&backup).unwrap();
        }
        f.clock.now.store(i64::MAX, Ordering::SeqCst);
        let config = f.config.clone();
        let clock = f.clock.clone();
        drop(f.fs);
        let reopened = FsLibrary::new(config, clock).unwrap();
        reopened
            .set_roots(vec![spec(f.id, f.root.clone())])
            .await
            .unwrap();
        assert_eq!(record.exists(), backup_remains);
        assert_eq!(backup.exists(), backup_remains);
        assert_eq!(
            reopened.list_recovery().await.unwrap().len(),
            usize::from(backup_remains)
        );
    }
}

#[tokio::test]
async fn nested_roots_must_share_one_writability() {
    let f = Fixture::new().await;
    f.write("accounts/ledger.md", "kept");
    let accounts = |writable| RootSpec {
        writable,
        ..spec(LibraryRootId::new(), f.root.join("accounts"))
    };
    let outer = |writable| RootSpec {
        writable,
        ..spec(f.id, f.root.clone())
    };
    for (outer_writable, inner_writable) in [(true, false), (false, true)] {
        let refused =
            f.fs.set_roots(vec![outer(outer_writable), accounts(inner_writable)])
                .await;
        assert!(refused.is_err(), "{outer_writable} {inner_writable}");
    }
    // The earlier grant stays in force after a refused change.
    assert_eq!(f.fs.roots().await.len(), 1);
    f.fs.set_roots(vec![outer(false), accounts(false)])
        .await
        .unwrap();
    assert_eq!(f.fs.roots().await.len(), 2);
}
