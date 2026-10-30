// SPDX-License-Identifier: Apache-2.0
use super::*;

#[derive(Debug)]
struct TestClock {
    now: Timestamp,
    instant: std::time::Instant,
}
impl Clock for TestClock {
    fn now(&self) -> Timestamp {
        self.now
    }
    fn monotonic(&self) -> std::time::Instant {
        self.instant
    }
}

#[tokio::test]
async fn failure_before_replace_keeps_complete_original_and_recovers_draft() {
    for mode in 0..3 {
        let temp = tempfile::tempdir().unwrap();
        let root_path = temp.path().join("root");
        fs::create_dir(&root_path).unwrap();
        let mut config = LibraryConfig::new(temp.path().join("data"), temp.path().join("home"));
        config.confirmed_save_grace = Duration::ZERO;
        let library = FsLibrary::new(
            config,
            Arc::new(TestClock {
                now: Timestamp(0),
                instant: std::time::Instant::now(),
            }),
        )
        .unwrap();
        let id = LibraryRootId::new();
        library
            .set_roots(vec![RootSpec {
                root_id: id,
                path: root_path.clone(),
                label: "Test".into(),
                kind: RootKind::Project,
                writable: true,
                project_id: None,
                harness: None,
                only: None,
            }])
            .await
            .unwrap();
        fs::write(root_path.join("note.md"), "base").unwrap();
        let request = SaveRequest {
            text: "draft".into(),
            base_revision: revision(b"base"),
            newline: NewlineStyle::Lf,
            bom: false,
        };
        let root = lock(&library.core.state).roots[&id].clone();
        let result = library.core.save(
            &root,
            "note.md",
            request,
            || {
                if mode == 0 {
                    return Err(io::Error::other("simulated disk failure").into());
                }
                if mode == 1 {
                    fs::write(root_path.join("note.md"), "external before preflight")?;
                }
                Ok(())
            },
            || {
                if mode == 2 {
                    fs::write(root_path.join("editor-temp"), "external at exchange")?;
                    fs::rename(root_path.join("editor-temp"), root_path.join("note.md"))?;
                }
                Ok(())
            },
        );
        match mode {
            0 => {
                assert!(result.is_err());
                assert_eq!(fs::read(root_path.join("note.md")).unwrap(), b"base");
            }
            1 => {
                let SaveOutcome::Conflict {
                    current: Some(current),
                } = result.unwrap()
                else {
                    panic!("expected conflict")
                };
                assert_eq!(current.text, "external before preflight");
            }
            _ => {
                let SaveOutcome::Uncertain { recovery_id } = result.unwrap() else {
                    panic!("expected uncertain exchange")
                };
                let recovery = library.recovery(&recovery_id).await.unwrap();
                assert_eq!(recovery.proposed, b"draft");
                assert_eq!(recovery.displaced.unwrap(), b"external at exchange");
                // Even if the bytes later equal base, an Uncertain outcome has
                // no confirmation marker and must survive an expired sweep.
                fs::write(
                    root_path.join(format!(".ophio-save-{recovery_id}-old")),
                    "base",
                )
                .unwrap();
                library.purge_trash().await.unwrap();
                assert_eq!(library.recovery_entries().await.unwrap().len(), 1);
                assert!(
                    root_path
                        .join(format!(".ophio-save-{recovery_id}-old"))
                        .exists()
                );
            }
        }
        let entries = library.recovery_entries().await.unwrap();
        assert_eq!(entries.len(), 1);
        library
            .discard_recovery(&entries[0].recovery_id)
            .await
            .unwrap();
        assert!(library.recovery_entries().await.unwrap().is_empty());
        assert!(fs::read_dir(&root_path).unwrap().all(|e| {
            !e.unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".ophio-")
        }));
    }
}

#[tokio::test]
async fn search_deadline_uses_the_injected_clock() {
    #[derive(Debug)]
    struct AdvancingClock {
        instant: std::time::Instant,
        ticks: std::sync::atomic::AtomicU64,
    }
    impl Clock for AdvancingClock {
        fn now(&self) -> Timestamp {
            Timestamp(0)
        }
        fn monotonic(&self) -> std::time::Instant {
            self.instant
                + Duration::from_secs(self.ticks.fetch_add(1, std::sync::atomic::Ordering::SeqCst))
        }
    }
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("root");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("a.md"), "needle").unwrap();
    let mut config = LibraryConfig::new(temp.path().join("data"), temp.path().join("home"));
    config.search_timeout = Duration::from_millis(1);
    let library = FsLibrary::new(
        config,
        Arc::new(AdvancingClock {
            instant: std::time::Instant::now(),
            ticks: 0.into(),
        }),
    )
    .unwrap();
    let id = LibraryRootId::new();
    library
        .set_roots(vec![RootSpec {
            root_id: id,
            path: root,
            label: String::new(),
            kind: RootKind::Project,
            writable: true,
            project_id: None,
            harness: None,
            only: None,
        }])
        .await
        .unwrap();
    assert!(
        library
            .search(Some(id), "needle", 10)
            .await
            .unwrap()
            .is_empty()
    );
}

// Each file read can take up to its size limit, and a folder can hold as
// many files as search lists. A deadline that passes while they are read
// ends the search there too. Every clock reading here is a second later.
#[tokio::test]
async fn search_deadline_also_bounds_reading_a_folder() {
    #[derive(Debug)]
    struct AdvancingClock {
        instant: std::time::Instant,
        ticks: std::sync::atomic::AtomicU64,
    }
    impl Clock for AdvancingClock {
        fn now(&self) -> Timestamp {
            Timestamp(0)
        }
        fn monotonic(&self) -> std::time::Instant {
            self.instant
                + Duration::from_secs(self.ticks.fetch_add(1, std::sync::atomic::Ordering::SeqCst))
        }
    }
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("root");
    fs::create_dir(&root).unwrap();
    for n in 0..20 {
        fs::write(root.join(format!("{n:02}.md")), "needle").unwrap();
    }
    let mut config = LibraryConfig::new(temp.path().join("data"), temp.path().join("home"));
    // Listing the folder reads the clock 22 times; reading it may not take
    // all twenty more.
    config.search_timeout = Duration::from_secs(25);
    let library = FsLibrary::new(
        config,
        Arc::new(AdvancingClock {
            instant: std::time::Instant::now(),
            ticks: 0.into(),
        }),
    )
    .unwrap();
    let id = LibraryRootId::new();
    library
        .set_roots(vec![RootSpec {
            root_id: id,
            path: root,
            label: String::new(),
            kind: RootKind::Project,
            writable: true,
            project_id: None,
            harness: None,
            only: None,
        }])
        .await
        .unwrap();
    let hits = library.search(Some(id), "needle", 100).await.unwrap();
    assert!(!hits.is_empty() && hits.len() < 20, "{} hits", hits.len());
}

#[tokio::test]
async fn recovery_review_is_revision_checked_and_cleanup_is_root_bound() {
    use protocol::library::RecoveryVersion;
    let temp = tempfile::tempdir().unwrap();
    let root_path = temp.path().join("root");
    fs::create_dir(&root_path).unwrap();
    let library = FsLibrary::new(
        LibraryConfig::new(temp.path().join("data"), temp.path().join("home")),
        Arc::new(TestClock {
            now: Timestamp(42),
            instant: std::time::Instant::now(),
        }),
    )
    .unwrap();
    let id = LibraryRootId::new();
    let spec = RootSpec {
        root_id: id,
        path: root_path.clone(),
        label: "Notes".into(),
        kind: RootKind::Notes,
        writable: true,
        project_id: None,
        harness: None,
        only: None,
    };
    library.set_roots(vec![spec.clone()]).await.unwrap();
    fs::write(root_path.join("plan.md"), "base").unwrap();
    let root = lock(&library.core.state).roots[&id].clone();
    let request = SaveRequest {
        text: "my words".into(),
        base_revision: revision(b"base"),
        newline: NewlineStyle::Lf,
        bom: false,
    };
    library
        .core
        .save(
            &root,
            "plan.md",
            request,
            || Err(io::Error::other("injected failure").into()),
            || Ok(()),
        )
        .unwrap_err();
    let entries = library.list_recovery().await.unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].created_at, Timestamp(42));
    let recovery_id = &entries[0].recovery_id;
    let inspected = library.inspect_recovery(id, recovery_id).await.unwrap();
    assert_eq!(inspected.observed_at, Timestamp(42));
    let RecoveryVersion::Text { file: proposed } = inspected.proposed else {
        panic!("proposed text");
    };
    let RecoveryVersion::Text { file: current } = inspected.current else {
        panic!("current text");
    };
    assert_eq!(proposed.text, "my words");
    let save = SaveRequest {
        text: proposed.text.clone(),
        base_revision: current.revision,
        newline: current.newline,
        bom: current.has_bom,
    };
    fs::write(root_path.join("plan.md"), "external edit").unwrap();
    assert!(matches!(
        library.save(id, "plan.md", save.clone()).await.unwrap(),
        SaveOutcome::Conflict { .. }
    ));
    assert_eq!(
        fs::read(root_path.join("plan.md")).unwrap(),
        b"external edit"
    );
    let now = library.read(id, "plan.md").await.unwrap();
    assert!(matches!(
        library
            .save(
                id,
                "plan.md",
                SaveRequest {
                    base_revision: now.revision,
                    ..save
                }
            )
            .await
            .unwrap(),
        SaveOutcome::Saved { .. }
    ));
    assert!(
        library
            .list_recovery()
            .await
            .unwrap()
            .iter()
            .any(|i| i.recovery_id == *recovery_id)
    );
    let other = LibraryRootId::new();
    assert!(library.inspect_recovery(other, recovery_id).await.is_err());
    assert!(
        library
            .discard_recovery_checked(other, recovery_id)
            .await
            .is_err()
    );
    library.set_roots(vec![]).await.unwrap();
    assert!(library.list_recovery().await.unwrap().is_empty());
    assert!(library.inspect_recovery(id, recovery_id).await.is_err());
    assert!(
        library
            .discard_recovery_checked(id, recovery_id)
            .await
            .is_err()
    );
    let alternate = temp.path().join("alternate");
    fs::create_dir(&alternate).unwrap();
    library
        .set_roots(vec![RootSpec {
            path: alternate,
            ..spec.clone()
        }])
        .await
        .unwrap();
    assert!(library.list_recovery().await.unwrap().is_empty());
    assert!(library.inspect_recovery(id, recovery_id).await.is_err());
    assert!(
        library
            .discard_recovery_checked(id, recovery_id)
            .await
            .is_err()
    );
    library.set_roots(vec![spec]).await.unwrap();
    fs::write(root_path.join(".ophio-save-unrelated-old"), "unrelated").unwrap();
    library
        .discard_recovery_checked(id, recovery_id)
        .await
        .unwrap();
    let remaining = library.list_recovery().await.unwrap();
    assert!(remaining.is_empty()); // The later confirmed save is still in its grace window.
    assert_eq!(fs::read(root_path.join("plan.md")).unwrap(), b"my words");
    assert_eq!(
        fs::read(root_path.join(".ophio-save-unrelated-old")).unwrap(),
        b"unrelated"
    );
}

#[tokio::test]
async fn recovery_inspection_bounds_binary_and_growing_displaced_bytes() {
    use protocol::library::RecoveryVersion;
    let temp = tempfile::tempdir().unwrap();
    let root_path = temp.path().join("root");
    fs::create_dir(&root_path).unwrap();
    let library = FsLibrary::new(
        LibraryConfig::new(temp.path().join("data"), temp.path().join("home")),
        Arc::new(TestClock {
            now: Timestamp(42),
            instant: std::time::Instant::now(),
        }),
    )
    .unwrap();
    let id = LibraryRootId::new();
    library
        .set_roots(vec![RootSpec {
            root_id: id,
            path: root_path.clone(),
            label: "Notes".into(),
            kind: RootKind::Notes,
            writable: true,
            project_id: None,
            harness: None,
            only: None,
        }])
        .await
        .unwrap();
    let root = lock(&library.core.state).roots[&id].clone();
    let record = library.core.make_record(&root, "plan.md", "base");
    let storage = library.core.store_record("recovery", &record).unwrap();
    storage.write("base", [0, 255]).unwrap();
    storage.write("proposed", "my words").unwrap();
    let displaced = root_path.join(format!(".ophio-save-{}-old", record.id));
    fs::write(&displaced, vec![b'x'; 256 * 1024 + 1]).unwrap();
    let r = library.inspect_recovery(id, &record.id).await.unwrap();
    assert!(matches!(
        r.base,
        RecoveryVersion::Unavailable { size: Some(2), .. }
    ));
    assert!(matches!(
        r.displaced,
        RecoveryVersion::Unavailable {
            size: Some(262145),
            ..
        }
    ));
    assert!(matches!(r.current, RecoveryVersion::Missing));
    let escaped = "\t".repeat(32 * 1024);
    storage.write("base", &escaped).unwrap();
    storage.write("proposed", &escaped).unwrap();
    fs::write(&displaced, &escaped).unwrap();
    fs::write(root_path.join("plan.md"), &escaped).unwrap();
    let r = library.inspect_recovery(id, &record.id).await.unwrap();
    assert!(serde_json::to_vec(&r).unwrap().len() < 1024 * 1024 - 16 * 1024);
    fs::write(&displaced, "external handle wrote again").unwrap();
    let r = library.inspect_recovery(id, &record.id).await.unwrap();
    assert!(
        matches!(r.displaced, RecoveryVersion::Text { file } if file.text == "external handle wrote again")
    );
}

#[tokio::test]
async fn recovery_finds_files_named_before_the_rename() {
    use protocol::library::RecoveryVersion;
    let temp = tempfile::tempdir().unwrap();
    let root_path = temp.path().join("root");
    fs::create_dir(&root_path).unwrap();
    let library = FsLibrary::new(
        LibraryConfig::new(temp.path().join("data"), temp.path().join("home")),
        Arc::new(TestClock {
            now: Timestamp(42),
            instant: std::time::Instant::now(),
        }),
    )
    .unwrap();
    let id = LibraryRootId::new();
    library
        .set_roots(vec![RootSpec {
            root_id: id,
            path: root_path.clone(),
            label: "Notes".into(),
            kind: RootKind::Notes,
            writable: true,
            project_id: None,
            harness: None,
            only: None,
        }])
        .await
        .unwrap();
    for half in ["old", "new"] {
        // An uncertain save an earlier version recorded, with its displaced
        // file under the name that version used.
        let root = lock(&library.core.state).roots[&id].clone();
        let record = library.core.make_record(&root, "plan.md", "base");
        // Closed before the discard, which Windows refuses while it is open.
        {
            let storage = library.core.store_record("recovery", &record).unwrap();
            storage.write("base", "base").unwrap();
            storage.write("proposed", "my words").unwrap();
        }
        fs::write(root_path.join("plan.md"), "my words").unwrap();
        let displaced = root_path.join(format!(".companion-save-{}-{half}", record.id));
        fs::write(&displaced, "their words").unwrap();
        let listed = library.list_recovery().await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].recovery_id, record.id);
        let snapshot = library.recovery(&record.id).await.unwrap();
        assert_eq!(snapshot.displaced.unwrap(), b"their words");
        let inspected = library.inspect_recovery(id, &record.id).await.unwrap();
        assert!(
            matches!(inspected.displaced, RecoveryVersion::Text { file } if file.text == "their words")
        );
        library
            .discard_recovery_checked(id, &record.id)
            .await
            .unwrap();
        assert!(!displaced.exists());
        assert!(library.list_recovery().await.unwrap().is_empty());
    }
}

#[tokio::test]
async fn recovery_cleanup_allows_a_missing_destination_directory() {
    let temp = tempfile::tempdir().unwrap();
    let root_path = temp.path().join("root");
    fs::create_dir(&root_path).unwrap();
    let library = FsLibrary::new(
        LibraryConfig::new(temp.path().join("data"), temp.path().join("home")),
        Arc::new(TestClock {
            now: Timestamp(42),
            instant: std::time::Instant::now(),
        }),
    )
    .unwrap();
    let id = LibraryRootId::new();
    library
        .set_roots(vec![RootSpec {
            root_id: id,
            path: root_path.clone(),
            label: "Notes".into(),
            kind: RootKind::Notes,
            writable: true,
            project_id: None,
            harness: None,
            only: None,
        }])
        .await
        .unwrap();
    let root = lock(&library.core.state).roots[&id].clone();
    let record = library.core.make_record(&root, "removed/plan.md", "base");
    // Closed before the discard, which Windows refuses while it is open.
    {
        let storage = library.core.store_record("recovery", &record).unwrap();
        storage.write("base", "base").unwrap();
        storage.write("proposed", "my words").unwrap();
    }
    let observed = library.inspect_recovery(id, &record.id).await.unwrap();
    assert!(matches!(
        observed.current,
        protocol::library::RecoveryVersion::Missing
    ));
    library
        .discard_recovery_checked(id, &record.id)
        .await
        .unwrap();
    assert!(library.list_recovery().await.unwrap().is_empty());
    assert!(!root_path.join("removed").exists());
}
