// SPDX-License-Identifier: Apache-2.0

//! Portable contracts for the stable Windows entrypoint and external updater.
//!
//! Callers serialize publication with the installation lock. The launch lock is
//! additionally held from stopping the host through replacement and health checks.
//! A restarted host must run directly from bin, without reacquiring that lock.
//! These checks reject links, not hostile concurrent writers to an installation.

use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const IDENTITY: &str = "installation.json";
const JOB: &str = "update-job.json";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Job {
    pub(super) format: u32,
    pub(super) installation: String,
    pub(super) id: String,
    pub(super) now: bool,
    pub(super) helper: String,
    pub(super) sha256: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Installation {
    format: u32,
    id: String,
}

fn invalid(message: &str) -> Error {
    Error::Config(format!("Cannot use the Windows launcher: {message}"))
}

fn validate(path: &Path) -> Result<()> {
    super::windows_replace::validate_tree(path)
}

fn directory(path: &Path) -> Result<()> {
    validate(path)?;
    if !fs::metadata(path)?.is_dir() {
        return Err(invalid("the installation folder is not a directory"));
    }
    Ok(())
}

fn regular_file(path: &Path) -> Result<()> {
    validate(path)?;
    if !fs::metadata(path)?.is_file() {
        return Err(invalid("an expected executable or record is not a file"));
    }
    Ok(())
}

fn optional_file(path: &Path) -> Result<()> {
    validate(path)?;
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_file() => Ok(()),
        Ok(_) => Err(invalid("a publication destination is not a regular file")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn sync_directory(path: &Path) -> Result<()> {
    // Standard Rust cannot flush a Windows directory handle. Every published
    // file is flushed before rename; Unix tests also flush directory entries.
    #[cfg(unix)]
    File::open(path)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

struct Temporary(PathBuf);

impl Drop for Temporary {
    fn drop(&mut self) {
        // Normal errors must not retain an executable-sized copy. Never follow
        // or remove a link substituted for the temporary file.
        if validate(&self.0).is_ok()
            && fs::symlink_metadata(&self.0).is_ok_and(|meta| meta.is_file())
        {
            let _ = fs::remove_file(&self.0);
        }
    }
}

fn recognized_temporary(name: &std::ffi::OsStr) -> bool {
    let Some(id) = name
        .to_str()
        .and_then(|name| name.strip_prefix(".launcher-"))
        .and_then(|name| name.strip_suffix(".new"))
    else {
        return false;
    };
    // HostId is a UUID. Only this module's exact reserved name shape is owned;
    // similar-looking user files, folders and links are never removed.
    id.len() == 36
        && id.bytes().enumerate().all(|(index, byte)| {
            if [8, 13, 18, 23].contains(&index) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

fn cleanup_temporaries(parent: &Path) -> Result<()> {
    // The caller holds the installation lock: no live publication can own one.
    // Bound both scanning and deletion. A heavily littered folder is cleaned in
    // retryable batches, without creating yet another executable-sized copy.
    let mut removed = 0;
    for (index, entry) in fs::read_dir(parent)?.enumerate() {
        if index == 4096 {
            return Err(invalid("too many entries to clean temporary files; retry"));
        }
        let entry = entry?;
        if !recognized_temporary(&entry.file_name()) {
            continue;
        }
        let meta = fs::symlink_metadata(entry.path())?;
        if !meta.is_file() || meta.file_type().is_symlink() {
            continue;
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if meta.file_attributes() & 0x400 != 0 {
                continue;
            }
        }
        if removed == 64 {
            return Err(invalid("temporary files need another cleanup retry"));
        }
        validate(&entry.path())?;
        fs::remove_file(entry.path())?;
        removed += 1;
    }
    if removed != 0 {
        sync_directory(parent)?;
    }
    Ok(())
}

fn temporary(path: &Path) -> Result<(Temporary, File)> {
    let parent = path
        .parent()
        .ok_or_else(|| invalid("missing parent folder"))?;
    cleanup_temporaries(parent)?;
    let name = format!(".launcher-{}.new", protocol::HostId::new());
    let temp = parent.join(name);
    validate(&temp)?;
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)?;
    Ok((Temporary(temp), file))
}

/// Publish only by replacing a directory entry, never by removing the old one.
fn publish(path: &Path, bytes: &[u8]) -> Result<()> {
    optional_file(path)?;
    boundary("before-create")?;
    let (temp, mut file) = temporary(path)?;
    boundary("after-create")?;
    file.write_all(bytes)?;
    boundary("after-write")?;
    file.sync_all()?;
    drop(file);
    boundary("after-flush")?;
    optional_file(path)?;
    boundary("before-rename")?;
    fs::rename(&temp.0, path)?;
    boundary("after-rename")?;
    sync_directory(
        path.parent()
            .ok_or_else(|| invalid("missing parent folder"))?,
    )?;
    boundary("after-directory-flush")
}

fn valid_identity(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        && id.bytes().any(|byte| byte.is_ascii_alphanumeric())
}

pub(super) fn read_identity(opt: &Path) -> Result<String> {
    let path = opt.join(IDENTITY);
    regular_file(&path)?;
    let identity: Installation = serde_json::from_slice(&fs::read(path)?)
        .map_err(|_| invalid("the installation identity is invalid"))?;
    if identity.format != 1 || !valid_identity(&identity.id) {
        return Err(invalid(
            "the installation identity has an unsupported format",
        ));
    }
    Ok(identity.id)
}

/// Create the permanent random identity once. Never regenerate corrupt state.
/// The caller must hold the installation lock, including on first installation.
pub(super) fn ensure_identity(opt: &Path) -> Result<String> {
    directory(opt)?;
    match fs::symlink_metadata(opt.join(IDENTITY)) {
        Ok(_) => read_identity(opt),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let id = protocol::HostId::new().to_string();
            let identity = Installation {
                format: 1,
                id: id.clone(),
            };
            publish(
                &opt.join(IDENTITY),
                &serde_json::to_vec(&identity)
                    .map_err(|_| invalid("cannot encode the installation identity"))?,
            )?;
            Ok(id)
        }
        Err(error) => Err(error.into()),
    }
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .split('-')
            .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
}

fn validate_job(opt: &Path, job: &Job) -> Result<()> {
    directory(opt)?;
    if job.format != 1 {
        return Err(invalid("the update job has an unsupported format"));
    }
    if !valid_id(&job.id) || !valid_id(&job.helper) {
        return Err(invalid("the update job has an invalid identifier"));
    }
    if job.sha256.len() != 64 || !job.sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(invalid("the update job requires a complete SHA-256 digest"));
    }
    if job.installation != read_identity(opt)? {
        return Err(invalid("the update job belongs to another installation"));
    }
    Ok(())
}

fn digest(path: &Path) -> Result<String> {
    regular_file(path)?;
    let mut file = File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

/// Verify immediately before spawning too: publication is not an execution lease.
pub(super) fn pinned_helper(opt: &Path, job: &Job) -> Result<PathBuf> {
    validate_job(opt, job)?;
    let folder = opt.join("update-helpers").join(&job.helper);
    directory(&folder)?;
    let helper = folder.join("ophio.exe");
    if !digest(&helper)?.eq_ignore_ascii_case(&job.sha256) {
        return Err(invalid("the pinned updater executable changed"));
    }
    Ok(helper)
}

/// The helper must already be an independently copied, flushed file outside bin.
/// Both checks are intentional: a helper changed during publication must not run.
pub(super) fn publish_job(opt: &Path, job: &Job) -> Result<()> {
    pinned_helper(opt, job)?;
    publish(
        &opt.join(JOB),
        &serde_json::to_vec(job).map_err(|_| invalid("cannot encode the update job"))?,
    )?;
    pinned_helper(opt, job)?;
    Ok(())
}

pub(super) fn read_job(opt: &Path) -> Result<Job> {
    directory(opt)?;
    let path = opt.join(JOB);
    regular_file(&path)?;
    let job: Job = serde_json::from_slice(&fs::read(path)?)
        .map_err(|_| invalid("the update job is invalid"))?;
    validate_job(opt, &job)?;
    Ok(job)
}

/// Refresh on EVERY update, before moving bin, after the host has stopped.
/// Source is the external helper's own executable. No hard links or writes to
/// the mapped root image are used. A denied replacement leaves the root intact.
/// An incompatible job/identity format fails closed; future bootstraps must keep
/// this contract compatible or be supplied by a new download.
pub(super) fn install_launcher(opt: &Path, source: &Path) -> Result<()> {
    ensure_identity(opt)?;
    regular_file(source)?;
    let root = opt.join("ophio.exe");
    optional_file(&root)?;
    let expected = digest(source)?;
    boundary("before-create")?;
    let (temp, mut output) = temporary(&root)?;
    boundary("after-create")?;
    let mut input = File::open(source)?;
    std::io::copy(&mut input, &mut output)?;
    boundary("after-copy")?;
    output.sync_all()?;
    drop(output);
    drop(input);
    boundary("after-flush")?;
    // Check the source as well as the independent copy, including after flush.
    if digest(&temp.0)? != expected || digest(source)? != expected {
        return Err(invalid("the launcher source changed while copying"));
    }
    optional_file(&root)?;
    boundary("before-rename")?;
    boundary("before-launcher-rename")?;
    fs::rename(&temp.0, &root).map_err(|error| Error::Ended(format!(
        "Cannot refresh the Windows launcher: {error}. Stop programs using this installation and retry; the existing launcher has been kept."
    )))?;
    boundary("after-rename")?;
    sync_directory(opt)?;
    boundary("after-directory-flush")
}

/// A stable, nontruncated lock file. Dropping the returned File releases its lock.
/// Root startup holds this while deciding recovery/forwarding, not for the whole
/// forwarded command lifetime. Update holds it through replacement and health.
pub(super) fn launch_lock(opt: &Path) -> Result<File> {
    launch_lock_for(opt, Duration::from_secs(120))
}

fn launch_lock_for(opt: &Path, timeout: Duration) -> Result<File> {
    directory(opt)?;
    let path = opt.join("launch.lock");
    optional_file(&path)?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    let start = Instant::now();
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(std::fs::TryLockError::WouldBlock) if start.elapsed() < timeout => {
                std::thread::sleep(
                    Duration::from_millis(25).min(timeout.saturating_sub(start.elapsed())),
                );
            }
            Err(std::fs::TryLockError::WouldBlock) => {
                return Err(Error::Ended("The Windows launcher is waiting for an update. Retry after the update finishes; its recovery files have been kept.".into()));
            }
            Err(std::fs::TryLockError::Error(error)) => return Err(error.into()),
        }
    }
}

fn forwarding_command(target: &Path, args: &[OsString]) -> Command {
    let mut command = Command::new(target);
    command
        .args(args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    command
}

/// Forward native arguments without joining, quoting, Unicode conversion or a
/// shell. The caller propagates this status (including nonzero exit codes).
/// Call only after startup recovery and release the launch guard before waiting.
pub(super) fn spawn_forward(target: &Path, args: &[OsString]) -> Result<std::process::Child> {
    Ok(forwarding_command(target, args).spawn()?)
}

#[cfg(test)]
pub(super) fn forward(target: &Path, args: &[OsString]) -> Result<std::process::ExitStatus> {
    Ok(spawn_forward(target, args)?.wait()?)
}

fn boundary(name: &str) -> Result<()> {
    #[cfg(test)]
    tests::boundary(name)?;
    #[cfg(not(test))]
    let _ = name;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[derive(Default)]
    struct Fault {
        stop: Option<usize>,
        count: usize,
        mutate: Option<(String, PathBuf)>,
        deny_rename: bool,
    }
    thread_local! { static FAULT: RefCell<Fault> = RefCell::new(Fault::default()); }

    pub(super) fn boundary(name: &str) -> Result<()> {
        FAULT.with(|state| {
            let mut state = state.borrow_mut();
            if name == "before-launcher-rename" && state.deny_rename {
                return Err(invalid("injected rename denial"));
            }
            if state.mutate.as_ref().is_some_and(|(at, _)| at == name) {
                let (_, path) = state.mutate.take().unwrap();
                fs::write(path, b"changed bytes")?;
            }
            let count = state.count;
            state.count += 1;
            if state.stop == Some(count) {
                return Err(invalid("injected interruption"));
            }
            Ok(())
        })
    }

    fn reset(stop: Option<usize>) {
        FAULT.with(|state| {
            *state.borrow_mut() = Fault {
                stop,
                ..Fault::default()
            }
        });
    }

    fn fixture() -> (tempfile::TempDir, PathBuf, Job) {
        reset(None);
        let temp = tempfile::tempdir().unwrap();
        let opt = temp.path().join("opt");
        fs::create_dir(&opt).unwrap();
        let installation = ensure_identity(&opt).unwrap();
        let folder = opt.join("update-helpers/123-4");
        fs::create_dir_all(&folder).unwrap();
        let helper = folder.join("ophio.exe");
        fs::write(&helper, b"new executable bytes").unwrap();
        let job = Job {
            format: 1,
            installation,
            id: "123-3".into(),
            now: false,
            helper: "123-4".into(),
            sha256: digest(&helper).unwrap(),
        };
        (temp, opt, job)
    }

    #[test]
    fn identity_publication_retries_every_interruption_without_changing_committed_id() {
        let mut complete = false;
        for stop in 0..20 {
            reset(None);
            let temp = tempfile::tempdir().unwrap();
            let opt = temp.path();
            reset(Some(stop));
            let result = ensure_identity(opt);
            reset(None);
            let committed = read_identity(opt).ok();
            let recovered = ensure_identity(opt).unwrap();
            assert_eq!(ensure_identity(opt).unwrap(), recovered);
            if let Some(committed) = committed {
                assert_eq!(committed, recovered);
            }
            if result.is_ok() {
                complete = true;
                break;
            }
        }
        assert!(complete);
    }

    #[test]
    fn job_publication_is_complete_old_or_new_at_every_interruption() {
        let mut complete = false;
        for stop in 0..20 {
            let (_temp, opt, mut job) = fixture();
            publish_job(&opt, &job).unwrap();
            let old = job.clone();
            job.now = true;
            reset(Some(stop));
            let result = publish_job(&opt, &job);
            reset(None);
            let actual = read_job(&opt).unwrap();
            assert!(actual == old || actual == job);
            publish_job(&opt, &job).unwrap();
            assert_eq!(read_job(&opt).unwrap(), job);
            if result.is_ok() {
                complete = true;
                break;
            }
        }
        assert!(complete);
    }

    #[test]
    fn launcher_never_disappears_at_any_refresh_boundary_and_refreshes_again() {
        let mut complete = false;
        for stop in 0..20 {
            let (_temp, opt, job) = fixture();
            let source = pinned_helper(&opt, &job).unwrap();
            let root = opt.join("ophio.exe");
            fs::write(&root, b"old executable bytes").unwrap();
            reset(Some(stop));
            let result = install_launcher(&opt, &source);
            reset(None);
            let actual = fs::read(&root).unwrap();
            assert!(actual == b"old executable bytes" || actual == b"new executable bytes");
            install_launcher(&opt, &source).unwrap();
            assert_eq!(fs::read(&root).unwrap(), b"new executable bytes");
            // A later update must refresh the bootstrap, not freeze the first one.
            fs::write(&source, b"next executable bytes").unwrap();
            install_launcher(&opt, &source).unwrap();
            assert_eq!(fs::read(&root).unwrap(), b"next executable bytes");
            assert!(!opt.join("bin").exists());
            if result.is_ok() {
                complete = true;
                break;
            }
        }
        assert!(complete);
    }

    #[test]
    fn launcher_copy_is_independent_and_source_mutation_fails_before_replace() {
        let (_temp, opt, job) = fixture();
        let source = pinned_helper(&opt, &job).unwrap();
        let root = opt.join("ophio.exe");
        fs::write(&root, b"old").unwrap();
        FAULT.with(|state| state.borrow_mut().mutate = Some(("after-copy".into(), source.clone())));
        assert!(install_launcher(&opt, &source).is_err());
        reset(None);
        assert_eq!(fs::read(&root).unwrap(), b"old");
        install_launcher(&opt, &source).unwrap();
        fs::write(&source, b"later mutation").unwrap();
        assert_eq!(fs::read(&root).unwrap(), b"changed bytes");
    }

    #[test]
    fn helper_mutation_during_job_publication_and_before_execution_is_rejected() {
        let (_temp, opt, job) = fixture();
        let helper = pinned_helper(&opt, &job).unwrap();
        FAULT.with(|state| {
            state.borrow_mut().mutate = Some(("after-rename".into(), helper.clone()))
        });
        assert!(publish_job(&opt, &job).is_err());
        reset(None);
        assert!(pinned_helper(&opt, &read_job(&opt).unwrap()).is_err());
        assert!(publish_job(&opt, &job).is_err());
    }

    #[test]
    fn records_reject_foreign_identity_paths_partial_hashes_and_future_formats() {
        let (_temp, opt, job) = fixture();
        for bad in ["", "../1", "1/2", "1\\2", "-1", "1-", "1--2", "1a", "1\n"] {
            let mut changed = job.clone();
            changed.id = bad.into();
            assert!(publish_job(&opt, &changed).is_err(), "{bad:?}");
            changed = job.clone();
            changed.helper = bad.into();
            assert!(publish_job(&opt, &changed).is_err(), "{bad:?}");
        }
        let mut changed = job.clone();
        changed.format = 2;
        assert!(publish_job(&opt, &changed).is_err());
        changed = job.clone();
        changed.installation = "another-installation".into();
        assert!(publish_job(&opt, &changed).is_err());
        for hash in ["a".repeat(12), "g".repeat(64), "a".repeat(65)] {
            changed = job.clone();
            changed.sha256 = hash;
            assert!(publish_job(&opt, &changed).is_err());
        }
        changed = job.clone();
        changed.helper = "999-1".into();
        assert!(pinned_helper(&opt, &changed).is_err());
        publish_job(&opt, &job).unwrap();
        fs::write(opt.join(IDENTITY), br#"{"format":1,"id":"changed"}"#).unwrap();
        assert!(read_job(&opt).is_err());
        fs::write(opt.join(IDENTITY), br#"{"format":2,"id":"changed"}"#).unwrap();
        assert!(ensure_identity(&opt).is_err());
        fs::write(opt.join(IDENTITY), b"bad JSON").unwrap();
        assert!(ensure_identity(&opt).is_err());
    }

    #[test]
    fn unknown_job_fields_fail_closed() {
        let (_temp, opt, job) = fixture();
        let mut value = serde_json::to_value(&job).unwrap();
        value["extra"] = true.into();
        fs::write(opt.join(JOB), serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(read_job(&opt).is_err());
    }

    #[test]
    fn launch_lock_is_bounded_and_does_not_truncate_its_file() {
        let (_temp, opt, _job) = fixture();
        fs::write(opt.join("launch.lock"), b"retained").unwrap();
        let mut first = launch_lock(&opt).unwrap();
        assert!(launch_lock_for(&opt, Duration::ZERO).is_err());
        // Windows byte-range locks also exclude reads through another handle.
        // Verify preservation through the owning locked handle instead.
        let mut contents = Vec::new();
        first.read_to_end(&mut contents).unwrap();
        assert_eq!(contents, b"retained");
        drop(first);
        launch_lock(&opt).unwrap();
    }

    #[test]
    fn nonfile_entrypoint_is_never_removed() {
        let (_temp, opt, job) = fixture();
        fs::create_dir(opt.join("ophio.exe")).unwrap();
        let helper = pinned_helper(&opt, &job).unwrap();
        assert!(install_launcher(&opt, &helper).is_err());
        assert!(opt.join("ophio.exe").is_dir());
    }

    fn temporary_count(opt: &Path) -> usize {
        fs::read_dir(opt)
            .unwrap()
            .map(|entry| entry.unwrap())
            .filter(|entry| recognized_temporary(&entry.file_name()))
            .count()
    }

    #[test]
    fn denied_launcher_renames_do_not_accumulate_copies() {
        let (_temp, opt, job) = fixture();
        let source = pinned_helper(&opt, &job).unwrap();
        fs::write(opt.join("ophio.exe"), b"old").unwrap();
        FAULT.with(|state| state.borrow_mut().deny_rename = true);
        for _ in 0..8 {
            assert!(install_launcher(&opt, &source).is_err());
            assert_eq!(temporary_count(&opt), 0);
            assert_eq!(fs::read(opt.join("ophio.exe")).unwrap(), b"old");
        }
        reset(None);
        install_launcher(&opt, &source).unwrap();
        assert_eq!(temporary_count(&opt), 0);
    }

    #[test]
    fn abandoned_temporary_cleanup_is_bounded_and_preserves_foreign_entries() {
        let (_temp, opt, job) = fixture();
        let source = pinned_helper(&opt, &job).unwrap();
        for _ in 0..70 {
            fs::write(
                opt.join(format!(".launcher-{}.new", protocol::HostId::new())),
                b"abandoned",
            )
            .unwrap();
        }
        let foreign = opt.join(".launcher-user-notes.new");
        fs::write(&foreign, b"keep").unwrap();
        assert!(install_launcher(&opt, &source).is_err());
        assert_eq!(temporary_count(&opt), 6);
        install_launcher(&opt, &source).unwrap();
        assert_eq!(temporary_count(&opt), 0);
        assert_eq!(fs::read(foreign).unwrap(), b"keep");
    }

    #[cfg(unix)]
    #[test]
    fn temporary_cleanup_never_deletes_links_or_directories() {
        use std::os::unix::fs::symlink;
        let (_temp, opt, _job) = fixture();
        let folder = opt.join(format!(".launcher-{}.new", protocol::HostId::new()));
        fs::create_dir(&folder).unwrap();
        let link = opt.join(format!(".launcher-{}.new", protocol::HostId::new()));
        symlink(&folder, &link).unwrap();
        cleanup_temporaries(&opt).unwrap();
        assert!(folder.is_dir());
        assert!(fs::symlink_metadata(link).unwrap().file_type().is_symlink());
    }

    #[test]
    fn forwarding_preserves_native_arguments_without_shell_quoting() {
        let args: Vec<OsString> = [
            "",
            "two words",
            "a\"quote",
            "'single'",
            "雪",
            "$(not-a-command)",
        ]
        .into_iter()
        .map(Into::into)
        .collect();
        let target = Path::new("fixture-program");
        let command = forwarding_command(target, &args);
        assert_eq!(command.get_program(), target.as_os_str());
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            args.iter().map(|arg| arg.as_os_str()).collect::<Vec<_>>()
        );
    }

    fn fixture_args(name: &str) -> Vec<OsString> {
        let module = module_path!().split_once("::").unwrap().1;
        [
            "--exact".into(),
            format!("{module}::{name}"),
            "--ignored".into(),
            "--nocapture".into(),
            "--test-threads=1".into(),
        ]
        .into_iter()
        .map(Into::into)
        .collect()
    }

    #[test]
    fn forwarding_inherits_standard_streams_and_preserves_exit_status() {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(fixture_args("forwarding_driver_fixture"))
            .env("OPHIO_LAUNCH_FORWARD_FIXTURE", "disposable-test-child")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(b"fixture input")
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert_eq!(output.status.code(), Some(23));
        assert!(String::from_utf8_lossy(&output.stdout).contains("forwarded fixture input"));
        assert!(String::from_utf8_lossy(&output.stderr).contains("forwarded fixture error"));
    }

    #[test]
    #[ignore = "authored disposable forwarding driver; invoked only by its parent test"]
    fn forwarding_driver_fixture() {
        if std::env::var("OPHIO_LAUNCH_FORWARD_FIXTURE").as_deref() != Ok("disposable-test-child") {
            return;
        }
        let status = forward(
            &std::env::current_exe().unwrap(),
            &fixture_args("forwarding_stream_fixture"),
        )
        .unwrap();
        std::process::exit(status.code().unwrap_or(1));
    }

    #[test]
    #[ignore = "authored disposable stream child; invoked only by forwarding driver"]
    fn forwarding_stream_fixture() {
        if std::env::var("OPHIO_LAUNCH_FORWARD_FIXTURE").as_deref() != Ok("disposable-test-child") {
            return;
        }
        let mut input = String::new();
        std::io::stdin().read_to_string(&mut input).unwrap();
        assert_eq!(input, "fixture input");
        println!("forwarded {input}");
        eprintln!("forwarded fixture error");
        std::io::stdout().flush().unwrap();
        std::io::stderr().flush().unwrap();
        std::process::exit(23);
    }

    #[cfg(windows)]
    #[test]
    fn denied_native_launcher_rename_keeps_root_and_can_retry() {
        use std::os::windows::fs::OpenOptionsExt;
        let (_temp, opt, job) = fixture();
        let root = opt.join("ophio.exe");
        fs::write(&root, b"old executable bytes").unwrap();
        let source = pinned_helper(&opt, &job).unwrap();
        // A disposable file handle models Windows refusing replacement. No
        // executable is run and no real installation or session is touched.
        let held = OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(&root)
            .unwrap();
        for _ in 0..8 {
            assert!(install_launcher(&opt, &source).is_err());
            assert_eq!(temporary_count(&opt), 0);
        }
        drop(held);
        assert_eq!(fs::read(&root).unwrap(), b"old executable bytes");
        install_launcher(&opt, &source).unwrap();
        assert_eq!(fs::read(&root).unwrap(), b"new executable bytes");
    }

    #[cfg(unix)]
    #[test]
    fn links_in_helpers_root_identity_and_ancestors_are_rejected() {
        use std::os::unix::fs::symlink;
        for target in ["ophio.exe", IDENTITY, "update-helpers/123-4/ophio.exe"] {
            let (temp, opt, job) = fixture();
            let source = temp.path().join("source");
            fs::write(&source, b"unrelated").unwrap();
            let target = opt.join(target);
            if target.exists() {
                fs::remove_file(&target).unwrap();
            }
            symlink(&source, target).unwrap();
            assert!(install_launcher(&opt, &source).is_err());
            assert!(publish_job(&opt, &job).is_err());
        }
        let (temp, opt, job) = fixture();
        let alias = temp.path().join("alias");
        symlink(&opt, &alias).unwrap();
        assert!(ensure_identity(&alias).is_err());
        assert!(pinned_helper(&alias, &job).is_err());
    }
}
