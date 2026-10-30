// SPDX-License-Identifier: Apache-2.0
use super::*;
use crate::Hub;
use protocol::{Command, CommandEnvelope, Query, QueryData, auth::Grants};

struct Fixture {
    _temp: tempfile::TempDir,
    home: PathBuf,
    hub: CompanionHub,
}
impl Fixture {
    async fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        std::fs::create_dir_all(home.join("Projects")).unwrap();
        let hub = CompanionHub::builder()
            .data_dir(home.join(".ophio-private"))
            .home_dir(&home)
            .build()
            .await
            .unwrap();
        Self {
            _temp: temp,
            home: ports::library::canonical(&home).unwrap(),
            hub,
        }
    }
    async fn list(
        &self,
        path: Option<&Path>,
        after: Option<String>,
        hidden: bool,
        search: &str,
    ) -> Result<ProjectFolderListing, ProtocolError> {
        let QueryData::ProjectFolders { listing } = self
            .hub
            .query(&Caller::local_owner(), query(path, after, hidden, search))
            .await?
        else {
            panic!("folder listing")
        };
        Ok(listing)
    }
}
fn query(path: Option<&Path>, after: Option<String>, hidden: bool, search: &str) -> Query {
    Query::ProjectFolders {
        path: path.map(|p| p.to_str().unwrap().into()),
        after,
        show_hidden: hidden,
        search: search.into(),
    }
}

#[tokio::test]
async fn places_and_navigation_do_not_register_or_expose_files() {
    let f = Fixture::new().await;
    std::fs::create_dir(f.home.join("Projects").join("My app")).unwrap();
    std::fs::write(f.home.join("Projects").join("private.txt"), "not a folder").unwrap();
    let before = f.hub.journal_head();
    let places = f.list(None, None, false, "").await.unwrap();
    assert!(places.current.is_none());
    assert!(
        places
            .folders
            .iter()
            .any(|p| p.name == "Home" && p.unavailable.is_some())
    );
    assert!(
        places
            .folders
            .iter()
            .any(|p| p.name == "Projects" && p.unavailable.is_none())
    );
    let listing = f
        .list(Some(&f.home.join("Projects")), None, false, "")
        .await
        .unwrap();
    assert_eq!(listing.folders.len(), 1);
    assert_eq!(listing.folders[0].name, "My app");
    assert!(
        listing
            .ancestors
            .iter()
            .any(|p| p.path == f.home.to_str().unwrap())
    );
    assert_eq!(f.hub.journal_head(), before);
    // Only Home, which every computer has.
    let projects = f.hub.read(|r| r.list::<Project>("project")).unwrap();
    assert!(projects.iter().all(|p| p.home), "{projects:?}");
    f.hub.shutdown().await.unwrap();
}

#[tokio::test]
async fn only_managers_can_browse_even_with_other_grants() {
    let f = Fixture::new().await;
    let mut caller = Caller::local_owner();
    caller.grants = Grants {
        admin: false,
        ..Grants::owner()
    };
    let err = f
        .hub
        .query(
            &caller,
            query(Some(Path::new("does-not-exist")), None, false, ""),
        )
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::PermissionDenied);
    assert!(!domain::grants::query(
        &domain::grants::effective(
            Grants::owner(),
            protocol::auth::Principal::Channel {
                identity_id: protocol::ChannelIdentityId::new()
            }
        ),
        &query(None, None, false, "")
    ));
    f.hub.shutdown().await.unwrap();
}

#[tokio::test]
async fn private_directories_are_hidden_even_when_hidden_folders_are_shown() {
    let f = Fixture::new().await;
    std::fs::create_dir(f.home.join(".notes")).unwrap();
    let listing = f.list(Some(&f.home), None, true, "").await.unwrap();
    assert!(listing.folders.iter().any(|e| e.name == ".notes"));
    assert!(!listing.folders.iter().any(|e| e.name == ".ophio-private"));
    assert_eq!(
        f.list(Some(&f.home.join(".ophio-private")), None, true, "")
            .await
            .unwrap_err()
            .code,
        ErrorCode::PathNotAllowed
    );
    assert!(
        !f.list(Some(&f.home), None, false, "")
            .await
            .unwrap()
            .folders
            .iter()
            .any(|e| e.name.starts_with('.'))
    );
    f.hub.shutdown().await.unwrap();
}

#[tokio::test]
async fn pages_are_bounded_and_filters_reach_later_names() {
    let f = Fixture::new().await;
    let root = f.home.join("Projects");
    for i in 0..205 {
        std::fs::create_dir(root.join(format!("project-{i:03}"))).unwrap();
    }
    let a = f.list(Some(&root), None, false, "").await.unwrap();
    let b = f
        .list(Some(&root), a.next_after.clone(), false, "")
        .await
        .unwrap();
    let c = f
        .list(Some(&root), b.next_after.clone(), false, "")
        .await
        .unwrap();
    assert_eq!(
        (a.folders.len(), b.folders.len(), c.folders.len()),
        (100, 100, 5)
    );
    assert!(c.next_after.is_none());
    assert!(a.folders.last().unwrap().name < b.folders[0].name);
    let filtered = f
        .list(Some(&root), None, false, "PROJECT-204")
        .await
        .unwrap();
    assert_eq!(filtered.folders.len(), 1);
    assert_eq!(filtered.folders[0].name, "project-204");
    assert!(serde_json::to_vec(&a).unwrap().len() < 768 * 1024);
    f.hub.shutdown().await.unwrap();
}

#[tokio::test]
async fn registration_is_separate_and_existing_projects_are_marked() {
    let f = Fixture::new().await;
    let path = f.home.join("Projects").join("app");
    std::fs::create_dir(&path).unwrap();
    let added = f
        .hub
        .submit(
            &Caller::local_owner(),
            CommandEnvelope::new(Command::AddProject {
                host_path: path.to_str().unwrap().into(),
                name: None,
            }),
        )
        .await;
    assert!(added.error.is_none(), "{added:?}");
    let listing = f
        .list(Some(&f.home.join("Projects")), None, false, "")
        .await
        .unwrap();
    assert!(listing.folders[0].project_id.is_some());
    assert!(
        f.list(Some(&path), None, false, "")
            .await
            .unwrap()
            .current
            .unwrap()
            .project_id
            .is_some()
    );
    f.hub.shutdown().await.unwrap();
}

#[tokio::test]
async fn invalid_missing_file_and_oversized_paths_fail_without_listing() {
    let f = Fixture::new().await;
    for path in [
        PathBuf::from("relative"),
        f.home.join(".."),
        PathBuf::from("x".repeat(4097)),
    ] {
        assert!(f.list(Some(&path), None, false, "").await.is_err());
    }
    std::fs::write(f.home.join("file"), "not a directory").unwrap();
    assert!(
        f.list(Some(&f.home.join("file")), None, false, "")
            .await
            .is_err()
    );
    assert_eq!(
        f.list(Some(&f.home.join("missing")), None, false, "")
            .await
            .unwrap_err()
            .code,
        ErrorCode::NotFound
    );
    f.hub.shutdown().await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn symlinks_cannot_reveal_private_directory_contents() {
    let f = Fixture::new().await;
    std::os::unix::fs::symlink(f.home.join(".ophio-private"), f.home.join("alias")).unwrap();
    std::os::unix::fs::symlink(&f.home, f.home.join("loop")).unwrap();
    let listing = f.list(Some(&f.home), None, true, "").await.unwrap();
    assert!(
        !listing
            .folders
            .iter()
            .any(|e| ["alias", "loop"].contains(&e.name.as_str()))
    );
    assert_eq!(
        f.list(Some(&f.home.join("alias")), None, false, "")
            .await
            .unwrap_err()
            .code,
        ErrorCode::PathNotAllowed
    );
    f.hub.shutdown().await.unwrap();
}

#[test]
fn revocation_while_a_listing_is_in_flight_withholds_the_result() {
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    use protocol::auth::{PairRequest, Principal};
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .unwrap();
    runtime.block_on(async {
        let f = Fixture::new().await;
        let owner = Caller::local_owner();
        let ticket = f.hub.open_pairing(&owner).await.unwrap();
        let device = crate::testing::pair_confirmed(
            &f.hub,
            PairRequest {
                pairing_secret: ticket.secret,
                device_public_key: URL_SAFE_NO_PAD.encode([7u8; 32]),
                device_name: "Fixture".into(),
                device_platform: "test".into(),
                client_version: "test".into(),
                protocol: protocol::ProtocolVersion::CURRENT,
                shows_code: true,
                replaces: None,
            },
        )
        .await
        .unwrap();
        let caller = Caller {
            principal: Principal::Device {
                device_id: device.device_id,
            },
            grants: device.grants,
        };
        let (release, wait) = std::sync::mpsc::channel::<()>();
        let blocked = tokio::task::spawn_blocking(move || {
            let _ = wait.recv_timeout(Duration::from_secs(10));
        });
        let hub = f.hub.clone();
        let listing =
            tokio::spawn(async move { hub.query(&caller, query(None, None, false, "")).await });
        tokio::time::timeout(Duration::from_secs(2), async {
            while f.hub.inner.folder_browsers.available_permits() == 2 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let receipt = f
            .hub
            .submit(
                &owner,
                CommandEnvelope::new(Command::RevokeDevice {
                    device_id: device.device_id,
                }),
            )
            .await;
        assert!(receipt.error.is_none());
        release.send(()).unwrap();
        blocked.await.unwrap();
        assert_eq!(
            listing.await.unwrap().unwrap_err().code,
            ErrorCode::Unauthenticated
        );
        f.hub.shutdown().await.unwrap();
    });
}

#[tokio::test]
async fn busy_disks_have_bounded_workers() {
    let f = Fixture::new().await;
    let permits = f
        .hub
        .inner
        .folder_browsers
        .clone()
        .acquire_many_owned(2)
        .await
        .unwrap();
    assert_eq!(
        f.list(None, None, false, "").await.unwrap_err().code,
        ErrorCode::TemporarilyUnavailable
    );
    drop(permits);
    assert!(f.list(None, None, false, "").await.is_ok());
    f.hub.shutdown().await.unwrap();
}
