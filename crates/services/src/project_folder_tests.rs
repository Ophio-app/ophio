// SPDX-License-Identifier: Apache-2.0
use super::*;
use std::path::Path;

async fn add(f: &Fixture, folder: &std::path::Path) -> CommandReceipt {
    f.hub
        .submit(
            &Caller::local_owner(),
            CommandEnvelope::new(Command::AddProject {
                host_path: folder.to_string_lossy().into_owned(),
                name: None,
            }),
        )
        .await
}

#[tokio::test]
async fn a_project_is_not_the_disk_the_home_folder_or_ophios_own() {
    let outer = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/service-tests")
        .join(uuid::Uuid::now_v7().to_string());
    let (home, settings) = (outer.join("users/sam"), outer.join("settings"));
    std::fs::create_dir_all(home.join("site")).unwrap();
    std::fs::create_dir_all(settings.join("keys")).unwrap();
    let f = Fixture::with(|b| b.home_dir(&home).private_dir(&settings)).await;
    std::fs::create_dir_all(f.path.join("artifacts")).unwrap();
    let disk = if cfg!(windows) { "C:\\" } else { "/" };
    let refusals = [
        (
            PathBuf::from(disk),
            "That’s the top of the computer’s disk. Choose the project’s own folder.",
        ),
        (
            home.clone(),
            "That’s the home folder. Choose the project’s own folder inside it.",
        ),
        // Above the home folder, where Ophio keeps none of its own files.
        (
            outer.join("users"),
            "That folder holds the home folder. Choose the project’s own folder.",
        ),
    ];
    let own = "Øphio keeps its own files in that folder. Choose the project’s own folder.";
    let folders = [
        f.path.clone(),
        f.path.join("artifacts"),
        f.path.parent().unwrap().to_owned(),
        settings.clone(),
        settings.join("keys"),
    ];
    for (folder, message) in refusals
        .into_iter()
        .chain(folders.into_iter().map(|folder| (folder, own)))
    {
        let receipt = add(&f, &folder).await;
        let error = receipt.error.expect("refused");
        assert_eq!(error.code, ErrorCode::PathNotAllowed, "{folder:?}");
        assert_eq!(error.message, message, "{folder:?}");
    }
    let receipt = add(&f, &home.join("site")).await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    f.close().await;
    std::fs::remove_dir_all(outer).unwrap();
}

/// Registration, and each run's record of the repository, read it without
/// running the filesystem monitor or the content filter its settings name.
#[cfg(unix)]
#[tokio::test]
async fn registration_reads_git_without_the_repositorys_programs() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new().await;
    let folder = f.project_dir();
    std::fs::create_dir_all(&folder).unwrap();
    let git = |args: &[&str]| {
        let status = std::process::Command::new("git")
            .args([
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.invalid",
            ])
            .args(args)
            .current_dir(&folder)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .status()
            .unwrap();
        assert!(status.success(), "{args:?}");
    };
    git(&["init", "-q"]);
    std::fs::write(folder.join("readme.md"), "hello").unwrap();
    git(&["add", "readme.md"]);
    git(&["commit", "-q", "-m", "first"]);
    let ran = f.path.with_extension("monitor-ran");
    let monitor = f.path.with_extension("monitor");
    std::fs::write(
        &monitor,
        format!("#!/bin/sh\ntouch '{}'\nexit 1\n", ran.display()),
    )
    .unwrap();
    std::fs::set_permissions(&monitor, std::fs::Permissions::from_mode(0o755)).unwrap();
    git(&["config", "core.fsmonitor", &monitor.to_string_lossy()]);
    let filtered = f.path.with_extension("filter-ran");
    git(&[
        "config",
        "filter.planted.clean",
        &format!("touch '{}'; cat", filtered.display()),
    ]);
    std::fs::write(folder.join(".git/info/attributes"), "* filter=planted\n").unwrap();
    std::fs::write(folder.join("readme.md"), "hello again").unwrap();
    std::fs::write(folder.join("notes.md"), "new").unwrap();

    let project_id = f.project().await;
    let project: Project = f.hub.read(|r| r.require("project", project_id)).unwrap();
    let vcs = project.vcs.unwrap();
    assert!(vcs.head.is_some());
    assert_eq!(vcs.dirty_files, 1);
    let revision = crate::results::repository_revision(
        &folder.to_string_lossy(),
        protocol::time::Timestamp(0),
    )
    .await
    .unwrap();
    assert_eq!(revision.dirty, Some(true));
    assert!(!ran.exists(), "the repository's filesystem monitor ran");
    assert!(!filtered.exists(), "the repository's content filter ran");
    f.close().await;
    std::fs::remove_file(monitor).unwrap();
}

/// A hard link in the project can name a file anywhere on the computer.
#[cfg(unix)]
#[tokio::test]
async fn a_hard_link_in_a_project_is_not_downloaded() {
    let f = Fixture::new().await;
    let project_id = f.project().await;
    let secret = f.path.with_extension("secret");
    std::fs::write(&secret, "private").unwrap();
    std::fs::hard_link(&secret, f.project_dir().join("notes.txt")).unwrap();
    let receipt = f
        .hub
        .submit(
            &Caller::local_owner(),
            CommandEnvelope::new(Command::CreateDownload {
                shown_in_task: None,
                source: DownloadSource::ProjectFile {
                    project_id,
                    path: "notes.txt".into(),
                },
            }),
        )
        .await;
    let error = receipt.error.expect("refused");
    assert_eq!(error.code, ErrorCode::PathNotAllowed);
    assert!(error.message.contains("hard-linked"), "{}", error.message);
    f.close().await;
    std::fs::remove_file(secret).unwrap();
}

/// A project folder replaced by a link leads somewhere else, so nothing is
/// downloaded from it or sent to it.
#[cfg(unix)]
#[tokio::test]
async fn a_project_folder_replaced_by_a_link_is_not_opened() {
    let f = Fixture::new().await;
    let project_id = f.project().await;
    let private = f.path.with_extension("private");
    std::fs::create_dir(&private).unwrap();
    std::fs::write(private.join("notes.txt"), "private").unwrap();
    let moved = f.project_dir().with_extension("moved");
    std::fs::rename(f.project_dir(), &moved).unwrap();
    std::os::unix::fs::symlink(&private, f.project_dir()).unwrap();
    let owner = Caller::local_owner();
    let download = f
        .hub
        .submit(
            &owner,
            CommandEnvelope::new(Command::CreateDownload {
                shown_in_task: None,
                source: DownloadSource::ProjectFile {
                    project_id,
                    path: "notes.txt".into(),
                },
            }),
        )
        .await;
    assert_eq!(
        download.error.expect("refused").code,
        ErrorCode::PathNotAllowed
    );
    let upload = f
        .hub
        .submit(
            &owner,
            CommandEnvelope::new(Command::CreateUpload {
                evidence: None,
                name: "draft.txt".into(),
                size: 1,
                sha256: hash(b"x"),
                destination: TransferDestination::ProjectDir {
                    project_id,
                    dir: String::new(),
                },
            }),
        )
        .await;
    assert_eq!(upload.error.expect("refused").code, ErrorCode::NotFound);
    std::fs::remove_file(f.project_dir()).unwrap();
    std::fs::rename(&moved, f.project_dir()).unwrap();
    std::fs::remove_dir_all(private).unwrap();
    f.close().await;
}

/// The folder at a project's path is the one it was added as only while no
/// part of that path has been swapped for a link.
#[test]
fn a_project_folder_opens_only_where_it_was_added() {
    let base = tempfile::tempdir().unwrap();
    let base = ports::library::plain(base.path().canonicalize().unwrap());
    let project = base.join("work").join("app");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(base.join("elsewhere").join("app")).unwrap();
    let open = |path: &Path| crate::project_folders::open_project_root(path).map(|_| ());
    open(&project).unwrap();
    // An ancestor moved away, and a link to another folder in its place.
    std::fs::rename(base.join("work"), base.join("moved")).unwrap();
    link_folder(&base.join("elsewhere"), &base.join("work"));
    assert_eq!(
        open(&project).unwrap_err().kind(),
        std::io::ErrorKind::NotFound
    );
    // The folder itself replaced by a link to the folder it was.
    remove_link(&base.join("work"));
    std::fs::create_dir(base.join("work")).unwrap();
    link_folder(&base.join("moved").join("app"), &project);
    assert_eq!(
        open(&project).unwrap_err().kind(),
        std::io::ErrorKind::NotFound
    );
    remove_link(&project);
    std::fs::rename(base.join("moved").join("app"), &project).unwrap();
    open(&project).unwrap();
}
#[cfg(unix)]
fn link_folder(target: &Path, link: &Path) {
    std::os::unix::fs::symlink(target, link).unwrap();
}
/// A junction, which needs no privilege to make, unlike a symbolic link.
#[cfg(windows)]
fn link_folder(target: &Path, link: &Path) {
    let made = std::process::Command::new("cmd")
        .arg("/C")
        .arg("mklink")
        .arg("/J")
        .arg(link)
        .arg(target)
        .output()
        .unwrap();
    assert!(made.status.success(), "{made:?}");
}
#[cfg(unix)]
fn remove_link(link: &Path) {
    std::fs::remove_file(link).unwrap();
}
#[cfg(windows)]
fn remove_link(link: &Path) {
    std::fs::remove_dir(link).unwrap();
}

/// Bytes sent to a folder that can't take them would only be refused, for
/// good, once every one had arrived.
#[tokio::test]
async fn an_upload_names_a_project_folder_it_cannot_use_when_declared() {
    let f = Fixture::new().await;
    let project_id = f.project().await;
    std::fs::write(f.project_dir().join("notes.txt"), "x").unwrap();
    let owner = Caller::local_owner();
    let destination = |dir: &str| TransferDestination::ProjectDir {
        project_id,
        dir: dir.into(),
    };
    let declare = |dir: &str| {
        CommandEnvelope::new(Command::CreateUpload {
            evidence: None,
            name: "draft.txt".into(),
            size: 1,
            sha256: hash(b"x"),
            destination: destination(dir),
        })
    };
    #[cfg_attr(not(unix), allow(unused_mut))]
    let mut refusals = vec![
        (
            "drafts",
            ErrorCode::NotFound,
            "There’s no folder “drafts” in Test. Create it on the computer or choose another folder, then send the file again.",
        ),
        (
            "notes.txt",
            ErrorCode::PathNotAllowed,
            "“notes.txt” in Test isn’t a folder files can be sent to. Choose another folder, then send the file again.",
        ),
    ];
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let sealed = f.project_dir().join("sealed");
        std::fs::create_dir(&sealed).unwrap();
        std::fs::set_permissions(&sealed, std::fs::Permissions::from_mode(0o555)).unwrap();
        // An account that writes there anyway can't show the refusal.
        if std::fs::write(sealed.join("probe"), "").is_err() {
            refusals.push((
                "sealed",
                ErrorCode::PermissionDenied,
                "This computer can’t add files to “sealed” in Test. Choose another folder, or allow changes to it on the computer, then send the file again.",
            ));
        }
    }
    for (dir, code, message) in refusals {
        let receipt = f.hub.submit(&owner, declare(dir)).await;
        let error = receipt.error.expect("refused");
        assert_eq!((error.code, error.message.as_str()), (code, message));
        let error = f
            .hub
            .check_upload_destination(&owner, "draft.txt", &destination(dir))
            .await
            .unwrap_err();
        assert_eq!(error.message, message);
    }
    let declared: Vec<TransferRecord> = f.hub.read(|r| r.list("transfer")).unwrap();
    assert!(declared.is_empty());
    std::fs::create_dir(f.project_dir().join("drafts")).unwrap();
    let receipt = f.hub.submit(&owner, declare("drafts")).await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    let project: Project = f.hub.read(|r| r.require("project", project_id)).unwrap();
    let moved = f.project_dir().with_extension("moved");
    std::fs::rename(f.project_dir(), &moved).unwrap();
    let error = f
        .hub
        .submit(&owner, declare(""))
        .await
        .error
        .expect("refused");
    assert_eq!(error.code, ErrorCode::NotFound);
    assert_eq!(
        error.message,
        format!(
            "The project folder {} can’t be found. Reconnect its drive or move it back, then send the file again.",
            project.host_path
        )
    );
    std::fs::rename(moved, f.project_dir()).unwrap();
    f.close().await;
}

fn homes(f: &Fixture) -> Vec<Project> {
    let projects: Vec<Project> = f.hub.read(|r| r.list("project")).unwrap();
    projects.into_iter().filter(|p| p.home).collect()
}

/// Every computer has its home folder as Home, which stays, follows the
/// home folder and sends none of its files to or from the phone.
#[tokio::test]
async fn home_is_always_there_and_keeps_its_files() {
    let outer = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/service-tests")
        .join(uuid::Uuid::now_v7().to_string());
    std::fs::create_dir_all(outer.join("sam")).unwrap();
    std::fs::create_dir_all(outer.join("sam-moved")).unwrap();
    std::fs::write(outer.join("sam/notes.txt"), "private").unwrap();
    let outer = outer.canonicalize().unwrap();
    let (home, moved) = (outer.join("sam"), outer.join("sam-moved"));
    let f = Fixture::with(|b| b.home_dir(&home)).await;
    let [found] = &homes(&f)[..] else {
        panic!("{:?}", homes(&f))
    };
    assert_eq!(
        (found.name.as_str(), Path::new(&found.host_path)),
        ("Home", home.as_path())
    );
    let owner = Caller::local_owner();
    let remove = Command::RemoveProject {
        project_id: found.project_id,
    };
    let receipt = f.hub.submit(&owner, CommandEnvelope::new(remove)).await;
    assert_eq!(receipt.error.expect("refused").code, ErrorCode::Conflict);
    let download = Command::CreateDownload {
        shown_in_task: None,
        source: DownloadSource::ProjectFile {
            project_id: found.project_id,
            path: "notes.txt".into(),
        },
    };
    let receipt = f.hub.submit(&owner, CommandEnvelope::new(download)).await;
    assert_eq!(
        receipt.error.expect("refused").code,
        ErrorCode::PathNotAllowed
    );

    let f = f.reopened(|b| b.home_dir(&home)).await;
    assert_eq!(homes(&f).as_slice(), std::slice::from_ref(found));
    let f = f.reopened(|b| b.home_dir(&moved)).await;
    let [after] = &homes(&f)[..] else {
        panic!("{:?}", homes(&f))
    };
    assert_eq!(after.project_id, found.project_id);
    assert_eq!(Path::new(&after.host_path), moved);
    f.close().await;
    std::fs::remove_dir_all(outer).unwrap();
}

/// Without a home folder there's no Home.
#[tokio::test]
async fn no_home_folder_no_home() {
    let f = Fixture::new().await;
    assert_eq!(homes(&f), []);
    f.close().await;
}
