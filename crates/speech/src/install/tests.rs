// SPDX-License-Identifier: Apache-2.0
use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const BODY: &[u8] = b"verified model fixture";

async fn server(
    body: &'static [u8],
    status: u16,
    advertised: usize,
) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/weights", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = [0; 4096];
        let _ = socket.read(&mut request).await;
        socket.write_all(format!("HTTP/1.1 {status} Test\r\nContent-Length: {advertised}\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
        let _ = socket.write_all(body).await;
    });
    (url, task)
}

fn fixture(url: String, hash: String) -> CatalogEntry {
    CatalogEntry {
        name: "fixture",
        languages: &["en"],
        license: "Apache-2.0",
        source: "test fixture",
        files: Box::leak(
            vec![ModelFile {
                name: "weights",
                size_bytes: BODY.len() as u64,
                sha256: Box::leak(hash.into_boxed_str()),
                url: Box::leak(url.into_boxed_str()),
            }]
            .into_boxed_slice(),
        ),
    }
}
fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
        .collect();
    names.sort();
    names
}

#[tokio::test]
async fn right_hash_publishes_atomically_and_owner_only() {
    let temp = tempfile::tempdir().unwrap();
    let (url, task) = server(BODY, 200, BODY.len()).await;
    let entry = fixture(url, format!("{:x}", Sha256::digest(BODY)));
    let mut updates = 0;
    let path = install_from(
        &entry,
        temp.path(),
        |p| {
            updates += 1;
            assert!(
                !temp.path().join("fixture").exists(),
                "partial install was exposed"
            );
            assert!(p.downloaded_bytes <= p.total_bytes);
        },
        &reqwest::Client::new(),
    )
    .await
    .unwrap();
    task.await.unwrap();
    assert!(updates > 0);
    assert_eq!(names(temp.path()), ["fixture"]);
    assert_eq!(verified_read(&path, &entry.files[0]).unwrap(), BODY);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(path.join("weights"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}

#[tokio::test]
async fn wrong_hash_size_or_http_error_leaves_existing_data_untouched() {
    for (body, advertised, status, hash) in [
        (BODY, BODY.len(), 200, "0".repeat(64)),
        (
            BODY,
            BODY.len() + 1,
            200,
            format!("{:x}", Sha256::digest(BODY)),
        ),
        (
            &BODY[..4],
            BODY.len(),
            200,
            format!("{:x}", Sha256::digest(BODY)),
        ),
        (BODY, BODY.len(), 500, format!("{:x}", Sha256::digest(BODY))),
    ] {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("keep"), b"existing").unwrap();
        let (url, task) = server(body, status, advertised).await;
        let entry = fixture(url, hash);
        assert!(
            install_from(&entry, temp.path(), |_| {}, &reqwest::Client::new())
                .await
                .is_err()
        );
        task.await.unwrap();
        assert_eq!(names(temp.path()), ["keep"]);
        assert_eq!(fs::read(temp.path().join("keep")).unwrap(), b"existing");
    }
}

#[tokio::test]
async fn cancelled_install_cleans_staging() {
    let temp = tempfile::tempdir().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let entry = fixture(
        format!("http://{}/weights", listener.local_addr().unwrap()),
        format!("{:x}", Sha256::digest(BODY)),
    );
    let (ready, rx) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        stream
            .write_all(
                format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\nv", BODY.len()).as_bytes(),
            )
            .await
            .unwrap();
        ready.send(()).unwrap();
        std::future::pending::<()>().await;
    });
    let client = reqwest::Client::new();
    let mut download = Box::pin(install_from(&entry, temp.path(), |_| {}, &client));
    tokio::select! { _ = &mut download => panic!("server never completed"), _ = rx => {} }
    drop(download);
    task.abort();
    assert!(names(temp.path()).is_empty());
}

#[tokio::test]
async fn public_installer_rejects_unreviewed_catalog_without_network_or_writes() {
    let temp = tempfile::tempdir().unwrap();
    let entry = fixture("http://127.0.0.1:1/weights".into(), "0".repeat(64));
    assert!(matches!(
        install(&entry, temp.path(), |_| {}).await,
        Err(InstallError::UnknownModel)
    ));
    assert!(names(temp.path()).is_empty());
}

#[tokio::test]
async fn existing_destination_is_never_overwritten() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join("fixture"), b"keep").unwrap();
    let entry = fixture("http://127.0.0.1:1/weights".into(), "0".repeat(64));
    assert!(matches!(
        install_from(&entry, temp.path(), |_| {}, &reqwest::Client::new()).await,
        Err(InstallError::AlreadyInstalled)
    ));
    assert_eq!(fs::read(temp.path().join("fixture")).unwrap(), b"keep");
}

#[test]
fn removal_is_bounded_and_refuses_unknown_contents() {
    let temp = tempfile::tempdir().unwrap();
    let model = &crate::catalog()[0];
    assert!(!remove(model, temp.path()).unwrap());
    let path = temp.path().join(model.name);
    fs::create_dir(&path).unwrap();
    fs::write(path.join("personal.txt"), b"keep").unwrap();
    assert!(matches!(
        remove(model, temp.path()),
        Err(InstallError::InvalidPath)
    ));
    fs::remove_file(path.join("personal.txt")).unwrap();
    for f in model.files {
        fs::write(path.join(f.name), b"fixture").unwrap();
    }
    assert!(remove(model, temp.path()).unwrap());
    assert!(names(temp.path()).is_empty());
}

#[cfg(unix)]
#[test]
fn symlinks_are_never_verified_or_removed_as_models() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let entry = &crate::catalog()[0];
    std::os::unix::fs::symlink(outside.path(), root.path().join(entry.name)).unwrap();
    assert!(matches!(
        remove(entry, root.path()),
        Err(InstallError::InvalidPath)
    ));
    assert!(matches!(
        verified_read(&root.path().join(entry.name), &entry.files[0]),
        Err(InstallError::InvalidPath)
    ));
}

#[tokio::test]
async fn later_file_failure_rolls_back_the_entire_bundle() {
    let temp = tempfile::tempdir().unwrap();
    let (a, task_a) = server(BODY, 200, BODY.len()).await;
    let (b, task_b) = server(BODY, 200, BODY.len()).await;
    let good = fixture(a, format!("{:x}", Sha256::digest(BODY)));
    let bad = fixture(b, "0".repeat(64));
    let files = Box::leak(
        vec![
            good.files[0],
            ModelFile {
                name: "configuration",
                ..bad.files[0]
            },
        ]
        .into_boxed_slice(),
    );
    let entry = CatalogEntry { files, ..good };
    assert!(matches!(
        install_from(&entry, temp.path(), |_| {}, &reqwest::Client::new()).await,
        Err(InstallError::Hash { .. })
    ));
    task_a.await.unwrap();
    task_b.await.unwrap();
    assert!(names(temp.path()).is_empty());
}

#[test]
fn post_install_corruption_is_rejected_before_model_parsing() {
    let temp = tempfile::tempdir().unwrap();
    let entry = fixture(
        "https://example.invalid/fixture".into(),
        format!("{:x}", Sha256::digest(BODY)),
    );
    let mut corrupted = BODY.to_vec();
    corrupted[0] ^= 1;
    fs::write(temp.path().join("weights"), corrupted).unwrap();
    assert!(matches!(
        verified_read(temp.path(), &entry.files[0]),
        Err(InstallError::Hash { .. })
    ));
}
