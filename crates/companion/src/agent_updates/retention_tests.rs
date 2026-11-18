// SPDX-License-Identifier: Apache-2.0
//! Shared retention tests: all programs are inert files, never executed.
use super::*;
use axum::{Router, extract::State, http::StatusCode, routing::get};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

const CODEX: Agent = Agent {
    installed: || true,
    ..Agent::CODEX
};
const OLD: &str = "2.0.0";
const BEFORE: &str = "1.0.0";
const OLDER: &str = "0.9.0";
const OLD_BYTES: &[u8] = b"inert selected program; must not be executed";
const BEFORE_BYTES: &[u8] = b"inert previous program; must not be executed";
const OLDER_BYTES: &[u8] = b"inert older program; must not be executed";
type Files = Arc<Mutex<HashMap<String, Vec<u8>>>>;

async fn registry() -> (String, Files) {
    let files: Files = Arc::default();
    let app = Router::new()
        .fallback(get(
            |State(files): State<Files>, uri: axum::http::Uri| async move {
                match files.lock().unwrap().get(&uri.to_string()) {
                    Some(body) => (StatusCode::OK, body.clone()),
                    None => (StatusCode::NOT_FOUND, Vec::new()),
                }
            },
        ))
        .with_state(files.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (base, files)
}

/// Valid metadata, but the archive is deliberately absent. A failed old-program
/// probe must not discard anything before this download fails.
fn publish_missing_archive(base: &str, files: &Files, platform: Platform, version: &str) {
    let package = CODEX.system_package(platform);
    let latest = serde_json::json!({
        "version": version,
        "optionalDependencies": { package.clone(): version },
    });
    let release = serde_json::json!({
        "dist": {
            "tarball": format!("{base}/missing-{version}.tgz"),
            "integrity": format!("sha512-{}", base64::engine::general_purpose::STANDARD.encode([0_u8; 64])),
        },
    });
    let mut files = files.lock().unwrap();
    files.insert(
        format!("/{}/latest", CODEX.package),
        latest.to_string().into_bytes(),
    );
    files.insert(
        format!("/{package}/{version}"),
        release.to_string().into_bytes(),
    );
}

fn managed_package(directory: &Path, relative: &str, bytes: &[u8]) -> PathBuf {
    let program = directory.join(relative);
    identity::private_dir(program.parent().unwrap()).unwrap();
    std::fs::write(&program, bytes).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    std::fs::write(
        program.parent().unwrap().join(agent_copy::MARKER),
        bytes.len().to_string(),
    )
    .unwrap();
    program
}

fn selected(folder: &Path, relative: &str) -> PathBuf {
    let program = managed_package(&folder.join(OLD), relative, OLD_BYTES);
    // Include a newline so retention means preserving the actual bytes, not
    // merely rewriting an equivalent pointer.
    std::fs::write(
        folder.join(agent_copy::CURRENT),
        format!("{OLD}/{relative}\n"),
    )
    .unwrap();
    assert_eq!(agent_copy::current(folder), Some(program.clone()));
    program
}

fn previous_generations(folder: &Path, relative: &str) {
    managed_package(&folder.join(BEFORE), relative, BEFORE_BYTES);
    managed_package(&folder.join(OLDER), relative, OLDER_BYTES);
    std::fs::write(folder.join(PREVIOUS), format!("{BEFORE}\n")).unwrap();
}

fn directories(folder: &Path) -> Vec<String> {
    let mut names: Vec<_> = std::fs::read_dir(folder)
        .unwrap()
        .map(|entry| entry.unwrap())
        .filter(|entry| entry.file_type().unwrap().is_dir())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

fn assert_retained(folder: &Path, relative: &str, current: &[u8], previous: &[u8]) {
    assert_eq!(
        std::fs::read(folder.join(agent_copy::CURRENT)).unwrap(),
        current
    );
    assert_eq!(std::fs::read(folder.join(PREVIOUS)).unwrap(), previous);
    for (version, bytes) in [
        (OLD, OLD_BYTES),
        (BEFORE, BEFORE_BYTES),
        (OLDER, OLDER_BYTES),
    ] {
        assert!(folder.join(version).is_dir(), "lost generation {version}");
        assert_eq!(
            std::fs::read(folder.join(version).join(relative)).unwrap(),
            bytes
        );
        assert_eq!(
            std::fs::read_to_string(
                folder
                    .join(version)
                    .join(relative)
                    .parent()
                    .unwrap()
                    .join(agent_copy::MARKER)
            )
            .unwrap(),
            bytes.len().to_string(),
        );
    }
    assert_eq!(
        agent_copy::current(folder),
        Some(folder.join(OLD).join(relative))
    );
}

async fn failed_install_retains_generations(
    first_attempt: bool,
    probe: Option<bool>,
    latest: &str,
) {
    let platform = Platform::detect().expect("retention tests require a supported host");
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("agents");
    let folder = root.join(CODEX.folder);
    let relative = CODEX.program(platform);
    let program = selected(&folder, &relative);
    let (base, files) = registry().await;
    let mut updater = NpmUpdater::with(root, base.clone(), Some(platform), vec![CODEX]);
    if !first_attempt {
        // Exercise the public API rather than depending on the lock's type or
        // on the private agent() signature. Nothing is downloaded or executed.
        publish_missing_archive(&base, &files, platform, OLD);
        updater.probe = Some(Some(true));
        let warmup = updater.apply(true).await;
        assert_eq!(warmup.len(), 1);
        assert_eq!(warmup[0].version.as_deref(), Some(OLD));
        assert_eq!(warmup[0].problem, None);
    }
    previous_generations(&folder, &relative);
    let current = std::fs::read(folder.join(agent_copy::CURRENT)).unwrap();
    let previous = std::fs::read(folder.join(PREVIOUS)).unwrap();
    let generations = directories(&folder);
    publish_missing_archive(&base, &files, platform, latest);
    updater.probe = Some(probe);

    let states = updater.apply(true).await;
    assert_eq!(states.len(), 1);
    let state = &states[0];
    assert!(state.checked);
    assert_eq!(state.latest_version.as_deref(), Some(latest));
    assert!(
        state.problem.is_some(),
        "the missing archive must fail installation"
    );
    assert_eq!(state.version.as_deref(), Some(OLD));
    assert_retained(&folder, &relative, &current, &previous);
    assert_eq!(
        directories(&folder),
        generations,
        "failed installs must not prune or leak staging directories"
    );
    assert_eq!(
        updater.program(HarnessKind::Codex),
        Some((program, Vec::new()))
    );
}

#[tokio::test]
async fn failed_old_probe_and_failed_update_retain_generations_on_first_attempt() {
    failed_install_retains_generations(true, Some(false), "3.0.0").await;
}

#[tokio::test]
async fn failed_old_probe_and_failed_update_retain_generations_on_later_attempt() {
    failed_install_retains_generations(false, Some(false), "3.0.0").await;
}

#[tokio::test]
async fn inconclusive_old_probe_and_failed_update_retain_generations_after_restart() {
    failed_install_retains_generations(true, None, "3.0.0").await;
}

#[tokio::test]
async fn inconclusive_old_probe_and_failed_update_retain_generations_on_later_attempt() {
    failed_install_retains_generations(false, None, "3.0.0").await;
}

#[tokio::test]
async fn failed_same_version_repair_retains_generations() {
    failed_install_retains_generations(true, Some(false), OLD).await;
}

#[test]
fn successful_activation_prunes_only_superseded_generations() {
    let platform = Platform::detect().expect("retention tests require a supported host");
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join(CODEX.folder);
    let relative = CODEX.program(platform);
    selected(&folder, &relative);
    previous_generations(&folder, &relative);
    let work = tempfile::Builder::new()
        .prefix(".download-")
        .tempdir_in(&folder)
        .unwrap();
    let unpacked = work.path().join("unpacked");
    let new_bytes = b"inert verified replacement";
    managed_package(&unpacked, &relative, new_bytes);

    activate(&folder, &unpacked, &relative, "3.0.0", Some(OLD)).unwrap();

    assert_eq!(held(&folder).as_deref(), Some("3.0.0"));
    assert_eq!(
        std::fs::read_to_string(folder.join(PREVIOUS))
            .unwrap()
            .trim(),
        OLD
    );
    assert_eq!(
        std::fs::read(agent_copy::current(&folder).unwrap()).unwrap(),
        new_bytes
    );
    assert_eq!(
        std::fs::read(folder.join(OLD).join(&relative)).unwrap(),
        OLD_BYTES
    );
    assert!(!folder.join(BEFORE).exists());
    assert!(!folder.join(OLDER).exists());
    assert!(!unpacked.exists());
    // The caller still owns the temporary work directory if activation does
    // not remove it during pruning.
    drop(work);
    assert_eq!(directories(&folder), [OLD, "3.0.0"]);
}

#[test]
fn failed_same_version_activation_preserves_selected_package_and_cleans_new_generation() {
    let platform = Platform::detect().expect("retention tests require a supported host");
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join(CODEX.folder);
    let relative = CODEX.program(platform);
    selected(&folder, &relative);
    previous_generations(&folder, &relative);
    let current = std::fs::read(folder.join(agent_copy::CURRENT)).unwrap();
    let previous = std::fs::read(folder.join(PREVIOUS)).unwrap();
    // This fails only after the verified replacement has been moved into its
    // final generation, without relying on permission behavior or open files.
    identity::private_dir(&folder.join(".current.new")).unwrap();
    let generations = directories(&folder);
    let selected_children = directories(&folder.join(OLD));
    let work = tempfile::Builder::new()
        .prefix(".download-")
        .tempdir_in(&folder)
        .unwrap();
    let unpacked = work.path().join("unpacked");
    managed_package(&unpacked, &relative, b"inert same-version replacement");

    assert!(activate(&folder, &unpacked, &relative, OLD, Some(OLD)).is_err());

    assert_retained(&folder, &relative, &current, &previous);
    drop(work);
    assert_eq!(
        directories(&folder),
        generations,
        "the unselected repair generation must be removed"
    );
    assert_eq!(directories(&folder.join(OLD)), selected_children);
}

#[test]
fn failed_new_version_activation_preserves_both_pointers_and_packages() {
    let platform = Platform::detect().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join(CODEX.folder);
    let relative = CODEX.program(platform);
    selected(&folder, &relative);
    previous_generations(&folder, &relative);
    let current = std::fs::read(folder.join(agent_copy::CURRENT)).unwrap();
    let previous = std::fs::read(folder.join(PREVIOUS)).unwrap();
    identity::private_dir(&folder.join(".current.new")).unwrap();
    let work = tempfile::Builder::new()
        .prefix(".download-")
        .tempdir_in(&folder)
        .unwrap();
    let unpacked = work.path().join("unpacked");
    managed_package(&unpacked, &relative, b"inert verified replacement");
    assert!(activate(&folder, &unpacked, &relative, "3.0.0", Some(OLD)).is_err());
    assert_retained(&folder, &relative, &current, &previous);
    assert!(!folder.join("3.0.0").exists());
}

#[test]
fn failed_previous_pointer_commit_keeps_all_packages_after_switch() {
    let platform = Platform::detect().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join(CODEX.folder);
    let relative = CODEX.program(platform);
    selected(&folder, &relative);
    previous_generations(&folder, &relative);
    std::fs::remove_file(folder.join(PREVIOUS)).unwrap();
    identity::private_dir(&folder.join(PREVIOUS)).unwrap();
    let work = tempfile::Builder::new()
        .prefix(".download-")
        .tempdir_in(&folder)
        .unwrap();
    let unpacked = work.path().join("unpacked");
    managed_package(&unpacked, &relative, b"inert verified replacement");
    activate(&folder, &unpacked, &relative, "3.0.0", Some(OLD)).unwrap();
    assert_eq!(held(&folder).as_deref(), Some("3.0.0"));
    for version in [OLD, BEFORE, OLDER] {
        assert!(folder.join(version).is_dir());
    }
}
