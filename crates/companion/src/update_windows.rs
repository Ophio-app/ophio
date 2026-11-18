// SPDX-License-Identifier: Apache-2.0
// Windows seams for the shared install, idle, health and rollback contract.
use super::*;

const PENDING: &str = "update-pending.json";
const JOB: &str = "update-job.json";

/// Explain an unsafe legacy install without changing its permissions or contents.
/// Other I/O errors (especially sharing violations) keep their native details.
pub(super) fn install_directory_error(path: &Path, error: Error) -> Error {
    if !matches!(&error, Error::Io(error) if error.kind() == std::io::ErrorKind::PermissionDenied) {
        return error;
    }
    Error::Ended(format!(
        "Windows refused the required owner-only access to {} ({error}). Ophio will not change this folder's permissions.\n\
         For an older ZIP installation only: finish any running work, then open ordinary PowerShell in the new extracted download and run each step only if the previous one succeeds:\n\
         .\\ophio.exe service uninstall\n\
         Rename-Item -LiteralPath \"$env:LOCALAPPDATA\\Programs\\Ophio\" -NewName ('Ophio-backup-' + [guid]::NewGuid().ToString('N'))\n\
         .\\ophio.exe install\n\
         Keep the backup. These steps keep settings, paired devices, conversations and projects stored outside the old programs folder. Do not run uninstall-data. If this is already a managed installation or an interrupted update, leave the folder in place and ask for support instead.",
        path.display()
    ))
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Pending {
    format: u32,
    installation: String,
    id: String,
    release: Release,
    from: Option<Release>,
    before: Before,
    restoring: bool,
}

fn durable_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|_| Error::Closed)?;
    let temp = path.with_extension("json.new");
    super::super::windows_replace::validate_tree(path)?;
    super::super::windows_replace::validate_tree(&temp)?;
    let mut file = std::fs::File::create(&temp)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(temp, path)?;
    Ok(())
}

fn pending(layout: &Layout) -> Result<Option<Pending>> {
    let path = layout.opt.join(PENDING);
    super::super::windows_replace::validate_tree(&path)?;
    match std::fs::read(&path) {
        Ok(bytes) => {
            let value: Pending = serde_json::from_slice(&bytes).map_err(|_| {
                Error::Config("Cannot read the interrupted update's health record.".into())
            })?;
            let identity = super::super::windows_launcher::read_identity(&layout.opt)?;
            if value.format != 1 || value.installation != identity || !valid_id(&value.id) {
                return Err(Error::Config(
                    "Invalid interrupted update identifier.".into(),
                ));
            }
            Ok(Some(value))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn cancel_guard(layout: &Layout) -> Result<std::fs::File> {
    let path = layout.opt.join("update-cancel.lock");
    super::super::windows_replace::validate_tree(&path)?;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    file.lock()?;
    Ok(file)
}

pub(super) fn cancellation_requested(root: &Path) -> Result<bool> {
    let marker = root.join("cancel");
    super::super::windows_replace::validate_tree(&marker)?;
    Ok(marker.is_file())
}

pub(super) fn begin(
    layout: &Layout,
    root: &Path,
    release: &Release,
    before: &Before,
) -> Result<bool> {
    let _cancel = cancel_guard(layout)?;
    if cancellation_requested(root)? {
        return Ok(false);
    }
    if pending(layout)?.is_some() {
        return Err(Error::Ended(
            "An interrupted update needs recovery before another update can start.".into(),
        ));
    }
    let id = root
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|id| valid_id(id))
        .ok_or_else(|| Error::Config("Invalid staged update identifier.".into()))?;
    durable_json(
        &layout.opt.join(PENDING),
        &Pending {
            format: 1,
            installation: super::super::windows_launcher::ensure_identity(&layout.opt)?,
            id: id.into(),
            release: release.clone(),
            from: installed(layout, None),
            before: before.clone(),
            restoring: false,
        },
    )?;
    Ok(true)
}

pub(super) fn restoring(layout: &Layout) -> Result<()> {
    let mut record =
        pending(layout)?.ok_or_else(|| Error::Config("Missing update health record.".into()))?;
    record.restoring = true;
    durable_json(&layout.opt.join(PENDING), &record)
}

pub(super) fn record_outcome(layout: &Layout, outcome: &Outcome) -> Result<()> {
    durable_json(&layout.last(), outcome)
}

pub(super) fn complete(layout: &Layout) -> Result<()> {
    remove_all(&layout.opt.join(PENDING))?;
    Ok(())
}

fn matches_release(layout: &Layout, release: &Release) -> bool {
    let program = layout.bin().join(OPHIO);
    installed(layout, None)
        .is_some_and(|current| current.build == release.build && current.version == release.version)
        && sha256(&program)
            .is_ok_and(|digest| digest.starts_with(&release.build) && release.build.len() == 12)
}

// Shared with the installed child's startup check so recovery tests enforce the
// same expected release before allowing a health-check process to start.
fn matches_pending_release(layout: &Layout) -> Result<bool> {
    Ok(pending(layout)?.is_some_and(|record| {
        let expected = if record.restoring {
            record.from.as_ref()
        } else {
            Some(&record.release)
        };
        expected.is_some_and(|release| matches_release(layout, release))
    }))
}

// Called only under the install lock, with the old process stopped. This also
// covers a helper dying after replacement but before its health result is saved.
async fn resume(layout: &Layout, host: &impl Host) -> Result<Outcome> {
    let mut record = pending(layout)?
        .ok_or_else(|| Error::Config("Missing interrupted update record.".into()))?;
    super::super::windows_replace::finish_replacement(&layout.opt)?;
    let mut result = Result_::NotInstalled;
    let message;
    if record.restoring {
        // A crash can occur just before the rollback journal is published too.
        if !record
            .from
            .as_ref()
            .is_some_and(|old| matches_release(layout, old))
        {
            super::super::windows_replace::replace(&layout.opt, &layout.previous(), false, true)?;
        }
        if record.before.running {
            host.start(&record.before, &layout.bin().join(OPHIO))
                .await?;
            host.healthy(&record.before).await.map_err(Error::Ended)?;
        }
        result = Result_::RolledBack;
        message =
            "Recovered the interrupted rollback; the earlier installation is restored.".into();
    } else if matches_release(layout, &record.release) {
        let healthy = if record.before.running {
            match host.start(&record.before, &layout.bin().join(OPHIO)).await {
                Ok(()) => host.healthy(&record.before).await,
                Err(error) => Err(error.to_string()),
            }
        } else {
            Ok(())
        };
        match healthy {
            Ok(()) => {
                result = Result_::Installed;
                message =
                    "Recovered the interrupted update and checked the installed version.".into();
            }
            Err(problem) => {
                if record.from.is_none() {
                    return Err(Error::Ended(format!(
                        "The recovered update is not healthy ({problem}); there is no earlier installation."
                    )));
                }
                host.stop(&record.before).await?;
                record.restoring = true;
                durable_json(&layout.opt.join(PENDING), &record)?;
                super::super::windows_replace::replace(
                    &layout.opt,
                    &layout.previous(),
                    false,
                    true,
                )?;
                if record.before.running {
                    host.start(&record.before, &layout.bin().join(OPHIO))
                        .await?;
                    host.healthy(&record.before).await.map_err(Error::Ended)?;
                }
                result = Result_::RolledBack;
                message = format!(
                    "The interrupted update failed its health check ({problem}); the earlier installation was restored."
                );
            }
        }
    } else if record
        .from
        .as_ref()
        .is_some_and(|old| matches_release(layout, old))
    {
        // The child's startup check must expect the unchanged old release.
        record.restoring = true;
        durable_json(&layout.opt.join(PENDING), &record)?;
        if record.before.running {
            host.start(&record.before, &layout.bin().join(OPHIO))
                .await?;
            host.healthy(&record.before).await.map_err(Error::Ended)?;
        }
        message = "The interrupted update had not replaced the programs; the earlier installation was kept.".into();
    } else if record.from.is_none() && !layout.bin().exists() {
        message = "The first installation was interrupted before replacing any programs; run install from the download again.".into();
    } else {
        return Err(Error::Config("The interrupted update does not match the installed programs; its recovery record has been kept.".into()));
    }
    let outcome = Outcome {
        id: record.id,
        result,
        release: record.release,
        from: record.from,
        at: now(),
        message,
    };
    record_outcome(layout, &outcome)?;
    complete(layout)?;
    Ok(outcome)
}

pub(super) fn launcher_ready(layout: &Layout) -> bool {
    layout.bin().is_dir()
        || [JOB, PENDING, "replacement.json"]
            .iter()
            .any(|name| layout.opt.join(name).exists())
}

fn validate_pending_job(layout: &Layout, job: &super::super::windows_launcher::Job) -> Result<()> {
    if pending(layout)?.is_some_and(|record| {
        record.id != job.id
            || record.installation != job.installation
            || record.format != job.format
    }) {
        return Err(Error::Config(
            "The health record does not belong to this update job.".into(),
        ));
    }
    Ok(())
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StartRequest {
    format: u32,
    installation: String,
    job: String,
}

fn persist_start(layout: &Layout, job: &super::super::windows_launcher::Job) -> Result<()> {
    durable_json(
        &layout.opt.join("update-start.json"),
        &StartRequest {
            format: 1,
            installation: job.installation.clone(),
            job: job.id.clone(),
        },
    )
}

// Snapshot lifecycle markers, read the job and record a one-shot login command
// under the finalization gate. Otherwise completion can remove the job between
// observing recovery and recording the login command, silently losing `run`.
fn startup_state(
    layout: &Layout,
    run: bool,
) -> Result<(bool, Option<super::super::windows_launcher::Job>)> {
    let _guard = cancel_guard(layout)?;
    let needs_recovery =
        layout.opt.join(PENDING).exists() || layout.opt.join("replacement.json").exists();
    let job = if layout.opt.join(JOB).exists() {
        let job = super::super::windows_launcher::read_job(&layout.opt)?;
        validate_pending_job(layout, &job)?;
        if run {
            persist_start(layout, &job)?;
        }
        Some(job)
    } else {
        None
    };
    Ok((needs_recovery, job))
}

#[cfg(test)]
fn request_start(layout: &Layout, job: &super::super::windows_launcher::Job) -> Result<bool> {
    let _guard = cancel_guard(layout)?;
    if !layout.opt.join(JOB).exists() {
        return Ok(false);
    }
    let current = super::super::windows_launcher::read_job(&layout.opt)?;
    if current != *job {
        return Err(Error::Config(
            "The update changed while queuing startup.".into(),
        ));
    }
    persist_start(layout, job)?;
    Ok(true)
}

fn start_requested(layout: &Layout, job: &super::super::windows_launcher::Job) -> Result<bool> {
    let path = layout.opt.join("update-start.json");
    super::super::windows_replace::validate_tree(&path)?;
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    let record: StartRequest = serde_json::from_slice(&bytes)
        .map_err(|_| Error::Config("Cannot read the deferred startup request.".into()))?;
    if record.format != job.format
        || record.installation != job.installation
        || record.job != job.id
    {
        return Err(Error::Config(
            "The startup request does not belong to this update.".into(),
        ));
    }
    Ok(true)
}

async fn finish_job(
    layout: &Layout,
    job: &super::super::windows_launcher::Job,
    host: &impl Host,
) -> Result<()> {
    let _intent = cancel_guard(layout)?;
    if start_requested(layout, job)? {
        let before = host.before().await?;
        if !before.running {
            host.start(&Before::default(), &layout.bin().join(OPHIO))
                .await?;
        }
        host.healthy(&Before::default())
            .await
            .map_err(Error::Ended)?;
        remove_all(&layout.opt.join("update-start.json"))?;
    }
    remove_all(&layout.opt.join(JOB))?;
    Ok(())
}

// Preserve a legacy flat installation without changing its root executable or
// startup registration. The root becomes the stable launcher only after idle and
// stop, before the first replacement of an existing bin directory.
pub(super) async fn migrate_flat(layout: &Layout, host: &impl Host) -> Result<()> {
    if layout.bin().exists() || !PROGRAMS.iter().all(|name| layout.opt.join(name).is_file()) {
        return Ok(());
    }
    let partial = layout.opt.join("legacy.partial");
    remove_all(&partial)?;
    private_dir(&partial)?;
    gather(&layout.opt, &partial, false)?;
    let program = partial.join("bin").join(OPHIO);
    let apps = std::fs::read_dir(partial.join("app"))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect();
    let release = Release {
        version: host.version(&program).await?,
        build: sha256(&program)?[..12].into(),
        installed_at: now(),
        apps,
    };
    durable_json(&partial.join("installed.json"), &release)?;
    super::super::windows_replace::replace(&layout.opt, &partial, false, false)?;
    remove_all(&partial)?;
    Ok(())
}

#[cfg(windows)]
pub(in crate::update) fn recover_on_start() -> Result<()> {
    runtime::startup_recovery()
}

#[cfg(windows)]
pub(super) use runtime::*;

#[cfg(windows)]
mod runtime {
    use super::*;
    use std::fs::{File, OpenOptions};
    use std::os::windows::process::CommandExt;

    use super::super::super::windows_launcher as launcher;
    use launcher::Job;

    pub(in crate::update::unix) fn ordinary_user() -> Result<()> {
        if platform_windows::background::elevated()? {
            return Err(Error::Ended("Install and update Øphio from an ordinary terminal, not an administrator terminal.".into()));
        }
        Ok(())
    }

    pub(in crate::update::unix) fn unit() -> String {
        "ophio-update".into()
    }
    pub(in crate::update::unix) fn service_name() -> String {
        "ophio".into()
    }

    fn job_lock(layout: &Layout) -> Result<File> {
        private_dir(&layout.opt)
            .map_err(|error| install_directory_error(&layout.opt, error))?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(layout.opt.join("update-job.lock"))?;
        file.try_lock()
            .map_err(|error| Error::Ended(format!("Another update helper is running: {error}")))?;
        Ok(file)
    }

    fn job_running(layout: &Layout) -> bool {
        job_lock(layout).is_err()
    }

    pub(in crate::update::unix) async fn active(name: &str) -> bool {
        if name == unit() {
            return Layout::discover().is_ok_and(|layout| layout.opt.join(JOB).exists());
        }
        let Ok(paths) = Paths::named(APP_NAME) else {
            return false;
        };
        OwnerClient::connect(&paths.socket()).await.is_ok()
    }

    fn read_job(layout: &Layout) -> Result<Job> {
        let job = launcher::read_job(&layout.opt)?;
        validate_pending_job(layout, &job)?;
        Ok(job)
    }

    pub(in crate::update::unix) async fn resume_orphan(
        layout: &Layout,
        host: &System,
    ) -> Result<bool> {
        if layout.opt.join(JOB).exists() && !job_running(layout) {
            recover(layout, host).await?;
            return Ok(true);
        }
        Ok(false)
    }

    fn spawn_helper(layout: &Layout, job: &Job, args: &[String]) -> Result<()> {
        ordinary_user()?;
        let helper = launcher::pinned_helper(&layout.opt, job)?;
        let folder = helper.parent().ok_or(Error::Closed)?;
        // No shell, console, inherited output, or installed working directory.
        // BREAKAWAY is attempted only where the containing job permits it.
        platform_windows::background::keep_std_handles();
        let spawn = |extra: u32| {
            std::process::Command::new(&helper)
                .args(args)
                .current_dir(folder)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .creation_flags(0x0000_0008 | 0x0000_0200 | extra)
                .spawn()
        };
        spawn(0x0100_0000).or_else(|_| spawn(0))?;
        Ok(())
    }

    pub(in crate::update::unix) async fn hand_over(
        layout: &Layout,
        program: &Path,
        id: &str,
        now_: bool,
    ) -> Result<()> {
        if !valid_id(id) {
            return Err(Error::Config("Invalid update identifier.".into()));
        }
        // Caller keeps the install lock from staging through publication.
        if layout.opt.join(JOB).exists() || pending(layout)?.is_some() {
            return Err(Error::Ended(
                "Another update is waiting or needs recovery.".into(),
            ));
        }
        let installation = launcher::ensure_identity(&layout.opt)?;
        let helper_id = format!("{}-{}", now(), std::process::id());
        let folder = layout.opt.join("update-helpers").join(&helper_id);
        private_dir(&folder)?;
        let helper = folder.join("ophio.exe");
        let expected = sha256(program)?;
        let mut source = std::fs::File::open(program)?;
        let mut target = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&helper)?;
        std::io::copy(&mut source, &mut target)?;
        target.sync_all()?;
        drop(target);
        if sha256(program)? != expected || sha256(&helper)? != expected {
            return Err(Error::Config(
                "The external update helper changed while copying.".into(),
            ));
        }
        let job = Job {
            format: 1,
            installation,
            id: id.into(),
            now: now_,
            helper: helper_id,
            sha256: expected,
        };
        launcher::publish_job(&layout.opt, &job)?;
        let mut args = vec!["install".into(), "--apply".into(), id.into()];
        if now_ {
            args.push("--now".into());
        }
        // Keep the published job on failure: another launcher may already have
        // resumed it, and an ordinary retry can recover a failed spawn safely.
        spawn_helper(layout, &job, &args)?;
        say!(
            "The checked update is running in the background. This command must exit so Windows can release its executable. `ophio update` shows progress; `ophio update --cancel` cancels while it waits for idle."
        );
        Ok(())
    }

    fn clean_helpers(layout: &Layout) {
        let current = std::env::current_exe().ok();
        for entry in std::fs::read_dir(layout.opt.join("update-helpers"))
            .into_iter()
            .flatten()
            .flatten()
        {
            if !current
                .as_ref()
                .is_some_and(|program| program.starts_with(entry.path()))
            {
                let _ = remove_all(&entry.path());
            }
        }
    }

    async fn cancelled(layout: &Layout, id: &str) {
        let path = layout.staging().join(id).join("cancel");
        loop {
            if path.is_file() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    pub(in crate::update::unix) async fn applied(
        layout: &Layout,
        id: &str,
        now_: bool,
        host: &System,
    ) -> Result<()> {
        ordinary_user()?;
        let _job = job_lock(layout)?;
        let job = read_job(layout)?;
        let pinned = launcher::pinned_helper(&layout.opt, &job)?;
        if std::env::current_exe()?.canonicalize()? != pinned.canonicalize()? {
            return Err(Error::Config(
                "Only the pinned external helper may apply this update.".into(),
            ));
        }
        if job.id != id || job.now != now_ {
            return Err(Error::Config(
                "The updater does not match the waiting update.".into(),
            ));
        }
        clean_helpers(layout);
        let mut cancel = Box::pin(cancelled(layout, id));
        let result = apply(layout, id, now_, host, &mut cancel).await;
        if pending(layout)?.is_none() && !layout.opt.join("replacement.json").exists() {
            finish_job(layout, &job, host).await?;
        }
        match result? {
            Outcome {
                result: Result_::Installed,
                ..
            } => Ok(()),
            outcome => Err(Error::Ended(outcome.message)),
        }
    }

    pub(in crate::update::unix) async fn cancel(layout: &Layout) -> Result<()> {
        let _cancel = cancel_guard(layout)?;
        if !layout.opt.join(JOB).exists() {
            say!("No update is waiting.");
            return Ok(());
        }
        let job = read_job(layout)?;
        if pending(layout)?.is_some() || layout.opt.join("replacement.json").exists() {
            return Err(Error::Ended("The update has already started replacing files. Let it finish or recover; it can no longer be cancelled.".into()));
        }
        let folder = layout.staging().join(&job.id);
        if !folder.is_dir() {
            return Err(Error::Ended("The waiting update has been interrupted; restart the installed command to recover it.".into()));
        }
        std::fs::write(folder.join("cancel"), b"")?;
        say!(
            "Cancellation requested. Nothing is cancelled once replacement has begun; `ophio update` shows the result."
        );
        Ok(())
    }

    pub(in crate::update::unix) fn replace(
        layout: &Layout,
        source: &Path,
        keep: bool,
        last: bool,
    ) -> Result<()> {
        // The command which requested this update can still be exiting. Retry
        // mapped-image/sharing failures briefly, without deleting the journal.
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        let mut result =
            super::super::super::windows_replace::replace(&layout.opt, source, keep, last);
        while result.is_err()
            && layout.opt.join("replacement.json").exists()
            && std::time::Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(200));
            result = super::super::super::windows_replace::finish_replacement(&layout.opt);
        }
        result
    }

    pub(in crate::update::unix) fn finish_replacement(layout: &Layout) -> Result<()> {
        super::super::super::windows_replace::finish_replacement(&layout.opt)
    }

    pub(in crate::update::unix) fn ensure_no_job(layout: &Layout) -> Result<()> {
        if layout.opt.join(JOB).exists()
            || pending(layout)?.is_some()
            || layout.opt.join("replacement.json").exists()
        {
            return Err(Error::Ended(
                "Another update is waiting or needs recovery.".into(),
            ));
        }
        Ok(())
    }

    pub(in crate::update::unix) fn launch_guard(layout: &Layout) -> Result<File> {
        launcher::launch_lock(&layout.opt)
    }

    pub(in crate::update::unix) fn prepare_launcher(layout: &Layout) -> Result<()> {
        let job = read_job(layout)?;
        let helper = launcher::pinned_helper(&layout.opt, &job)?;
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        loop {
            match launcher::install_launcher(&layout.opt, &helper) {
                Ok(()) => return Ok(()),
                Err(_) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(200))
                }
                Err(error) => return Err(error),
            }
        }
    }

    pub(in crate::update::unix) fn integrate(_layout: &Layout, _root: &Path, _log: &mut Log) {
        // No PATH/registry changes: the executable has a stable per-user path.
    }

    pub(in crate::update::unix) fn next_steps(layout: &Layout, _no_setup: bool) {
        say!(
            "After `ophio update` reports Installed, run {} setup. It can register this installed copy to start at login.",
            layout.opt.join("ophio.exe").display()
        );
    }

    pub(in crate::update::unix) fn startup_recovery() -> Result<()> {
        let layout = Layout::discover()?;
        let program = std::env::current_exe()?;
        let Some(folder) = program.parent() else {
            return Ok(());
        };
        let root = same(folder, &layout.opt)
            && program.file_name().is_some_and(|name| name == "ophio.exe");
        let installed = same(folder, &layout.bin());
        if !root && !installed {
            return Ok(());
        } // Downloads/helpers never recurse.
        if root && !launcher_ready(&layout) {
            return Ok(());
        } // A legacy flat download still runs normally.
        if root {
            launcher::read_identity(&layout.opt)?;
        }
        let (mut needs_recovery, job) = if root {
            startup_state(
                &layout,
                std::env::args_os().nth(1).is_some_and(|arg| arg == "run"),
            )?
        } else {
            // Health-check children must not take the finalization gate: their
            // parent can hold it while waiting for this child to answer.
            let needs =
                layout.opt.join(PENDING).exists() || layout.opt.join("replacement.json").exists();
            let job = if layout.opt.join(JOB).exists() {
                Some(read_job(&layout)?)
            } else {
                None
            };
            (needs, job)
        };
        if let Some(job) = job {
            let orphan = !job_running(&layout);
            if orphan {
                needs_recovery = true;
                spawn_helper(&layout, &job, &["install".into(), "--recover".into()])?;
                if !root {
                    return Err(Error::Ended("An interrupted update is recovering. Retry the installed launcher in a moment.".into()));
                }
            }
        }
        if !root {
            let health_start = job_running(&layout)
                && !layout.opt.join("replacement.json").exists()
                && matches_pending_release(&layout)?;
            if needs_recovery && !health_start {
                return Err(Error::Ended("An update is replacing programs. Run the launcher outside bin once it finishes.".into()));
            }
            return Ok(());
        }
        if needs_recovery {
            return Err(Error::Ended("Update recovery is running in the external helper. This launcher must exit so Windows can refresh it; retry in a moment.".into()));
        }
        // Never wait while mapping the root image: the helper may need to
        // atomically refresh that image before moving bin.
        let lock_path = layout.opt.join("launch.lock");
        super::super::super::windows_replace::validate_tree(&lock_path)?;
        let guard = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(lock_path)?;
        guard.try_lock().map_err(|error| {
            Error::Ended(format!(
                "An update is preparing the launcher; retry when it finishes ({error})."
            ))
        })?;
        if layout.opt.join(PENDING).exists() || layout.opt.join("replacement.json").exists() {
            return Err(Error::Ended("Update recovery is still running or could not finish. Its files have been kept; retry this launcher, or inspect update.log.".into()));
        }
        let target = layout.bin().join(OPHIO);
        super::super::super::windows_replace::validate_tree(&target)?;
        let mut child =
            launcher::spawn_forward(&target, &std::env::args_os().skip(1).collect::<Vec<_>>())?;
        // Inherit stdin/stdout/stderr exactly; no shell/string argument joining.
        // Release the gate after spawning, before a long-lived run command waits.
        drop(guard);
        std::process::exit(child.wait()?.code().unwrap_or(1));
    }

    pub(in crate::update::unix) async fn recover(layout: &Layout, host: &System) -> Result<()> {
        ordinary_user()?;
        let job = read_job(layout)?;
        let helper = launcher::pinned_helper(&layout.opt, &job)?;
        if std::env::current_exe()?.canonicalize()? != helper.canonicalize()? {
            spawn_helper(layout, &job, &["install".into(), "--recover".into()])?;
            say!(
                "Recovery was handed to the verified external updater. The install's update.log records its result."
            );
            return Ok(());
        }
        let _job = job_lock(layout)?;
        clean_helpers(layout);
        if pending(layout)?.is_none() {
            let lock = layout.lock()?;
            let gate = launch_guard(layout)?;
            finish_replacement(layout)?;
            if read_json::<Outcome>(&layout.last()).is_some_and(|outcome| outcome.id == job.id) {
                drop(gate);
                finish_job(layout, &job, host).await?;
                sweep(layout, None)?;
                return Ok(());
            }
            drop(gate);
            drop(lock);
            drop(_job);
            return applied(layout, &job.id, job.now, host).await;
        }
        let _lock = layout.lock()?;
        let before = host.before().await?;
        if before.running {
            let mut cancel = Box::pin(std::future::pending());
            if !idle(host, &mut cancel, &mut Log::open(layout)).await? {
                return Err(Error::Ended(
                    "Recovery is waiting for an idle computer; its record was kept.".into(),
                ));
            }
        }
        let _launch = launch_guard(layout)?;
        if before.running {
            host.stop(&before).await?;
        }
        prepare_launcher(layout)?;
        // Give the installed invocation which started this helper time to exit.
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            match finish_replacement(layout) {
                Ok(()) => break,
                Err(_) if Instant::now() < deadline => {
                    tokio::time::sleep(Duration::from_millis(200)).await
                }
                Err(error) => return Err(error),
            }
        }
        let outcome = resume(layout, host).await?;
        Log::open(layout).line(&outcome.message);
        // No more executable mutation remains. Release the launch gate before
        // clearing the job, so a one-shot login cannot see no job but a busy gate.
        drop(_launch);
        finish_job(layout, &job, host).await?;
        sweep(layout, None)?;
        Ok(())
    }

    // Native listener discovery is supplied by the Windows process adapter.
    async fn observed(host: &System) -> Result<(Snapshot, Vec<String>)> {
        let mut client = OwnerClient::connect(&host.paths.socket()).await?;
        let pid = client.server();
        let process = platform_windows::background::Process::open(pid)?;
        let snapshot = match crate::cli::query(
            &mut client,
            Query::Snapshot {
                reads_withheld: Some(true),
                account_items: false,
            },
        )
        .await?
        {
            QueryData::Snapshot(snapshot) => *snapshot,
            _ => return Err(Error::Closed),
        };
        if process.ended() {
            return Err(Error::Ended(
                "The companion stopped before its listener observation.".into(),
            ));
        }
        let mut listeners: Vec<String> = platform_windows::servers::listening(pid)?
            .into_iter()
            .filter(|socket| !socket.ip().to_canonical().is_loopback())
            .map(|socket| socket.to_string())
            .collect();
        if process.ended() {
            return Err(Error::Ended(
                "The companion stopped during its health observation.".into(),
            ));
        }
        listeners.sort();
        listeners.dedup();
        Ok((snapshot, listeners))
    }

    pub(in crate::update::unix) async fn before(host: &System) -> Result<Before> {
        match observed(host).await {
            Ok((snapshot, listeners)) => Ok(Before {
                running: true,
                service: crate::install::starts_at_login().await?,
                listeners,
                boot: Some(snapshot.host.boot_epoch.to_string()),
            }),
            Err(Error::NotRunning) => Ok(Before::default()),
            Err(error) => Err(error),
        }
    }

    pub(in crate::update::unix) async fn check(host: &System, before: &Before) -> Result<()> {
        let (snapshot, listeners) = observed(host).await?;
        if before
            .boot
            .as_ref()
            .is_some_and(|boot| *boot == snapshot.host.boot_epoch.to_string())
        {
            return Err(Error::Ended("The old version is still answering.".into()));
        }
        if before
            .listeners
            .iter()
            .any(|listener| !listeners.contains(listener))
        {
            return Err(Error::Ended("The restarted companion is not listening for devices on all its previous addresses.".into()));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    #[test]
    fn install_directory_refusal_explains_data_preserving_legacy_reinstall() {
        let path = Path::new(r"C:\Users\sam\AppData\Local\Programs\Ophio");
        let error = install_directory_error(
            path,
            std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "Windows owner security policy refused access",
            )
            .into(),
        );
        let message = error.to_string();
        for expected in [
            "owner-only access",
            "will not change this folder's permissions",
            "ordinary PowerShell",
            "each step only if the previous one succeeds",
            ".\\ophio.exe service uninstall",
            "Rename-Item -LiteralPath \"$env:LOCALAPPDATA\\Programs\\Ophio\"",
            "[guid]::NewGuid()",
            ".\\ophio.exe install",
            "Keep the backup",
            "Do not run uninstall-data",
            "interrupted update, leave the folder in place",
        ] {
            assert!(message.contains(expected), "missing {expected}: {message}");
        }
        assert!(!message.contains("Set-Acl"));
    }

    #[test]
    fn unrelated_install_directory_failures_keep_their_original_error() {
        for code in [2, 32, 112] {
            let error = install_directory_error(
                Path::new("opt"),
                std::io::Error::from_raw_os_error(code).into(),
            );
            let Error::Io(error) = error else {
                panic!("non-permission error was rewritten");
            };
            assert_eq!(error.raw_os_error(), Some(code));
        }
        assert!(matches!(
            install_directory_error(Path::new("opt"), Error::Identity),
            Error::Identity
        ));
    }

    #[cfg(windows)]
    #[test]
    fn inherited_install_directory_stays_refused_and_unchanged() {
        let temp = tempfile::tempdir().unwrap();
        let layout = Layout {
            opt: temp.path().join("old-install"),
            links: temp.path().join("links"),
            icons: temp.path().join("icons"),
        };
        std::fs::create_dir(&layout.opt).unwrap();
        let program = layout.opt.join("ophio.exe");
        std::fs::write(&program, "retained old program").unwrap();
        let message = layout.lock().unwrap_err().to_string();
        assert!(message.contains("owner-only access"), "{message}");
        assert!(message.contains("service uninstall"), "{message}");
        assert_eq!(std::fs::read_to_string(program).unwrap(), "retained old program");
        assert!(!layout.opt.join("update.lock").exists());
        assert_eq!(std::fs::read_dir(&layout.opt).unwrap().count(), 1);
        assert!(platform_windows::fs::private_dir(&layout.opt).is_err());
    }

    struct Fake {
        health_start_layout: Option<Layout>,
        fail_health: Cell<bool>,
        calls: RefCell<Vec<&'static str>>,
    }
    impl Host for Fake {
        async fn version(&self, _: &Path) -> Result<String> {
            Ok("test".into())
        }
        async fn before(&self) -> Result<Before> {
            Ok(Before::default())
        }
        async fn busy(&self) -> Result<Vec<String>> {
            Ok(vec![])
        }
        async fn stop(&self, _: &Before) -> Result<()> {
            self.calls.borrow_mut().push("stop");
            Ok(())
        }
        async fn start(&self, _: &Before, _: &Path) -> Result<()> {
            self.calls.borrow_mut().push("start");
            if let Some(layout) = &self.health_start_layout
                && !matches_pending_release(layout)?
            {
                return Err(Error::Ended(
                    "The installed child rejected the pending release.".into(),
                ));
            }
            Ok(())
        }
        async fn healthy(&self, _: &Before) -> std::result::Result<(), String> {
            self.calls.borrow_mut().push("health");
            if self.fail_health.replace(false) {
                Err("injected startup failure".into())
            } else {
                Ok(())
            }
        }
        async fn refresh_unit(&self, _: &Path) -> Result<Option<String>> {
            Ok(None)
        }
    }
    fn fixture() -> (tempfile::TempDir, Layout, PathBuf, Release, Fake) {
        let temp = tempfile::tempdir().unwrap();
        let layout = Layout {
            opt: temp.path().join("opt"),
            links: temp.path().join("links"),
            icons: temp.path().join("icons"),
        };
        std::fs::create_dir_all(layout.bin()).unwrap();
        std::fs::write(layout.bin().join(OPHIO), "old program").unwrap();
        let release = |build| Release {
            version: "1.0.0".into(),
            build,
            installed_at: 1,
            apps: vec![],
        };
        durable_json(
            &layout.installed(),
            &release(sha256(&layout.bin().join(OPHIO)).unwrap()[..12].into()),
        )
        .unwrap();
        let root = layout.staging().join("123-45");
        std::fs::create_dir_all(root.join("bin")).unwrap();
        std::fs::write(root.join("bin").join(OPHIO), "new program").unwrap();
        let new = release(sha256(&root.join("bin").join(OPHIO)).unwrap()[..12].into());
        durable_json(&root.join("installed.json"), &new).unwrap();
        let fake = Fake {
            health_start_layout: None,
            fail_health: Cell::new(false),
            calls: RefCell::new(vec![]),
        };
        (temp, layout, root, new, fake)
    }
    fn prepare(layout: &Layout, root: &Path, new: &Release) {
        begin(
            layout,
            root,
            new,
            &Before {
                running: true,
                boot: Some("previous-boot".into()),
                ..Before::default()
            },
        )
        .unwrap();
    }
    #[tokio::test]
    async fn interruption_before_swap_keeps_and_restarts_previous_version() {
        let (_temp, layout, root, new, host) = fixture();
        prepare(&layout, &root, &new);
        let outcome = resume(&layout, &host).await.unwrap();
        assert_eq!(outcome.result, Result_::NotInstalled);
        assert_eq!(*host.calls.borrow(), vec!["start", "health"]);
        assert_eq!(
            std::fs::read_to_string(layout.bin().join(OPHIO)).unwrap(),
            "old program"
        );
        assert!(!layout.opt.join(PENDING).exists());
    }
    #[tokio::test]
    async fn unchanged_programs_pass_child_startup_and_recovery_does_not_repeat() {
        let (_temp, layout, root, new, mut host) = fixture();
        prepare(&layout, &root, &new);
        assert!(!matches_pending_release(&layout).unwrap());
        host.health_start_layout = Some(layout.clone());

        let outcome = resume(&layout, &host).await.unwrap();
        assert_eq!(outcome.result, Result_::NotInstalled);
        assert_eq!(*host.calls.borrow(), vec!["start", "health"]);
        assert_eq!(
            std::fs::read_to_string(layout.bin().join(OPHIO)).unwrap(),
            "old program"
        );
        assert!(!layout.opt.join(PENDING).exists());
        let saved = read_json::<Outcome>(&layout.last()).unwrap();
        assert_eq!(saved.result, Result_::NotInstalled);
        assert_eq!(saved.id, outcome.id);
        let (needs_recovery, job) = startup_state(&layout, false).unwrap();
        assert!(!needs_recovery);
        assert!(job.is_none());
        assert!(resume(&layout, &host).await.is_err());
        assert_eq!(*host.calls.borrow(), vec!["start", "health"]);
    }

    #[tokio::test]
    async fn interruption_after_swap_checks_new_version_and_keeps_rollback() {
        let (_temp, layout, root, new, host) = fixture();
        prepare(&layout, &root, &new);
        super::super::super::windows_replace::replace(&layout.opt, &root, true, false).unwrap();
        let outcome = resume(&layout, &host).await.unwrap();
        assert_eq!(outcome.result, Result_::Installed);
        assert!(layout.previous().join("bin").join(OPHIO).is_file());
        assert!(!layout.opt.join(PENDING).exists());
    }
    #[tokio::test]
    async fn failed_recovered_health_rolls_back_and_checks_previous_version() {
        let (_temp, layout, root, new, host) = fixture();
        prepare(&layout, &root, &new);
        super::super::super::windows_replace::replace(&layout.opt, &root, true, false).unwrap();
        host.fail_health.set(true);
        let outcome = resume(&layout, &host).await.unwrap();
        assert_eq!(outcome.result, Result_::RolledBack);
        assert_eq!(
            *host.calls.borrow(),
            vec!["start", "health", "stop", "start", "health"]
        );
        assert_eq!(
            std::fs::read_to_string(layout.bin().join(OPHIO)).unwrap(),
            "old program"
        );
    }
    #[tokio::test]
    async fn rollback_interrupted_before_and_after_replacement_recovers() {
        for already_restored in [false, true] {
            let (_temp, layout, root, new, host) = fixture();
            prepare(&layout, &root, &new);
            super::super::super::windows_replace::replace(&layout.opt, &root, true, false).unwrap();
            restoring(&layout).unwrap();
            if already_restored {
                super::super::super::windows_replace::replace(
                    &layout.opt,
                    &layout.previous(),
                    false,
                    true,
                )
                .unwrap();
            }
            let outcome = resume(&layout, &host).await.unwrap();
            assert_eq!(outcome.result, Result_::RolledBack);
            assert_eq!(
                std::fs::read_to_string(layout.bin().join(OPHIO)).unwrap(),
                "old program"
            );
        }
    }
    #[tokio::test]
    async fn changed_install_or_bad_record_is_visible_and_retained() {
        for corrupt_record in [false, true] {
            let (_temp, layout, root, new, host) = fixture();
            prepare(&layout, &root, &new);
            if corrupt_record {
                std::fs::write(layout.opt.join(PENDING), "bad JSON").unwrap();
            } else {
                std::fs::write(layout.bin().join(OPHIO), "unexpected program").unwrap();
            }
            assert!(resume(&layout, &host).await.is_err());
            assert!(layout.opt.join(PENDING).exists());
            assert!(host.calls.borrow().is_empty());
        }
    }
    #[test]
    fn flat_download_is_not_mistaken_for_a_managed_launcher() {
        let temp = tempfile::tempdir().unwrap();
        let layout = Layout {
            opt: temp.path().to_owned(),
            links: temp.path().join("links"),
            icons: temp.path().join("icons"),
        };
        std::fs::write(layout.opt.join("ophio.exe"), "flat executable").unwrap();
        assert!(!launcher_ready(&layout));
        for marker in [JOB, PENDING, "replacement.json"] {
            std::fs::write(layout.opt.join(marker), "record").unwrap();
            assert!(launcher_ready(&layout));
            std::fs::remove_file(layout.opt.join(marker)).unwrap();
        }
        std::fs::create_dir(layout.bin()).unwrap();
        assert!(launcher_ready(&layout));
    }

    fn fixture_job(layout: &Layout) -> super::super::super::windows_launcher::Job {
        use super::super::super::windows_launcher as launcher;
        let installation = launcher::ensure_identity(&layout.opt).unwrap();
        let helper = layout.opt.join("update-helpers/456-7/ophio.exe");
        std::fs::create_dir_all(helper.parent().unwrap()).unwrap();
        std::fs::write(&helper, "authored helper fixture").unwrap();
        let job = launcher::Job {
            format: 1,
            installation,
            id: "123-45".into(),
            now: false,
            helper: "456-7".into(),
            sha256: sha256(&helper).unwrap(),
        };
        launcher::publish_job(&layout.opt, &job).unwrap();
        job
    }

    #[tokio::test]
    async fn health_record_rejects_foreign_installation_job_and_future_format() {
        for field in ["installation", "id", "format"] {
            let (_temp, layout, root, new, host) = fixture();
            let job = fixture_job(&layout);
            prepare(&layout, &root, &new);
            validate_pending_job(&layout, &job).unwrap();
            let path = layout.opt.join(PENDING);
            let mut value: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            value[field] = match field {
                "format" => 2.into(),
                "id" => "999-9".into(),
                _ => "another-installation".into(),
            };
            std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
            assert!(validate_pending_job(&layout, &job).is_err(), "{field}");
            if field != "id" {
                assert!(resume(&layout, &host).await.is_err());
            }
            assert!(path.exists());
            assert!(host.calls.borrow().is_empty());
        }
    }

    #[test]
    fn persisted_cancellation_prevents_begin_even_for_a_stopped_host() {
        let (_temp, layout, root, new, host) = fixture();
        std::fs::write(root.join("cancel"), "").unwrap();
        assert!(cancellation_requested(&root).unwrap());
        assert!(!begin(&layout, &root, &new, &Before::default()).unwrap());
        assert!(!layout.opt.join(PENDING).exists());
        assert_eq!(
            std::fs::read_to_string(layout.bin().join(OPHIO)).unwrap(),
            "old program"
        );
        assert!(host.calls.borrow().is_empty());
    }

    #[tokio::test]
    async fn deferred_login_request_survives_failed_start_and_is_completed_before_job_removal() {
        let (_temp, layout, _root, _new, host) = fixture();
        let job = fixture_job(&layout);
        let (_, observed) = startup_state(&layout, true).unwrap();
        assert_eq!(observed.as_ref(), Some(&job));
        host.fail_health.set(true);
        assert!(finish_job(&layout, &job, &host).await.is_err());
        assert!(layout.opt.join(JOB).exists());
        assert!(start_requested(&layout, &job).unwrap());
        finish_job(&layout, &job, &host).await.unwrap();
        assert!(!layout.opt.join(JOB).exists());
        assert!(!layout.opt.join("update-start.json").exists());
        assert!(!request_start(&layout, &job).unwrap());
        assert_eq!(
            *host.calls.borrow(),
            vec!["start", "health", "start", "health"]
        );
    }

    #[test]
    fn startup_snapshot_waits_for_finalization_and_never_uses_a_removed_job() {
        let (_temp, layout, root, new, _host) = fixture();
        fixture_job(&layout);
        prepare(&layout, &root, &new);
        let guard = cancel_guard(&layout).unwrap();
        let other = layout.clone();
        let ready = std::sync::Arc::new(std::sync::Barrier::new(2));
        let thread_ready = ready.clone();
        let child = std::thread::spawn(move || {
            thread_ready.wait();
            let (needs, job) = startup_state(&other, true).unwrap();
            assert!(!needs);
            assert!(job.is_none());
            assert!(!other.opt.join("update-start.json").exists());
        });
        ready.wait();
        // Simulate helper completion after the launcher process starts, while
        // holding the same gate used by finish_job for its terminal publication.
        remove_all(&layout.opt.join(PENDING)).unwrap();
        remove_all(&layout.opt.join(JOB)).unwrap();
        drop(guard);
        child.join().unwrap();
    }

    #[tokio::test]
    async fn finishing_an_update_does_not_enable_a_stopped_companion() {
        let (_temp, layout, _root, _new, host) = fixture();
        let job = fixture_job(&layout);
        finish_job(&layout, &job, &host).await.unwrap();
        assert!(host.calls.borrow().is_empty());
        assert!(!layout.opt.join(JOB).exists());
    }

    #[tokio::test]
    async fn corrupt_or_foreign_startup_requests_remain_visible_without_starting() {
        for corrupt in [false, true] {
            let (_temp, layout, _root, _new, host) = fixture();
            let job = fixture_job(&layout);
            request_start(&layout, &job).unwrap();
            let path = layout.opt.join("update-start.json");
            if corrupt {
                std::fs::write(&path, "bad JSON").unwrap();
            } else {
                durable_json(
                    &path,
                    &StartRequest {
                        format: 1,
                        installation: "foreign".into(),
                        job: job.id.clone(),
                    },
                )
                .unwrap();
            }
            assert!(finish_job(&layout, &job, &host).await.is_err());
            assert!(path.exists());
            assert!(layout.opt.join(JOB).exists());
            assert!(host.calls.borrow().is_empty());
        }
    }

    #[tokio::test]
    async fn flat_migration_keeps_root_paths_and_copies_a_complete_independent_previous_install() {
        let (_temp, layout, _root, _new, host) = fixture();
        remove_all(&layout.bin()).unwrap();
        remove_all(&layout.installed()).unwrap();
        #[cfg(target_os = "linux")]
        let program = b"\x7fELFauthored fixture".to_vec();
        #[cfg(target_os = "macos")]
        let program = b"\xcf\xfa\xed\xfeauthored fixture".to_vec();
        #[cfg(windows)]
        let program = {
            let mut bytes = vec![0; 70];
            bytes[..2].copy_from_slice(b"MZ");
            bytes[60..64].copy_from_slice(&64u32.to_le_bytes());
            bytes[64..68].copy_from_slice(b"PE\0\0");
            let machine: u16 = if cfg!(target_arch = "aarch64") {
                0xaa64
            } else {
                0x8664
            };
            bytes[68..70].copy_from_slice(&machine.to_le_bytes());
            bytes
        };
        for name in PROGRAMS {
            std::fs::write(layout.opt.join(name), &program).unwrap();
        }
        std::fs::write(layout.opt.join("ophio.apk"), b"authored APK fixture").unwrap();
        migrate_flat(&layout, &host).await.unwrap();
        assert!(layout.bin().join(OPHIO).is_file());
        assert_eq!(std::fs::read(layout.opt.join(OPHIO)).unwrap(), program);
        assert_eq!(installed(&layout, None).unwrap().version, "test");
        assert_eq!(installed(&layout, None).unwrap().apps, vec!["ophio.apk"]);
        assert_eq!(
            std::fs::read(layout.app().join("ophio.apk")).unwrap(),
            b"authored APK fixture"
        );
        std::fs::write(layout.opt.join(OPHIO), "later root mutation").unwrap();
        assert_eq!(std::fs::read(layout.bin().join(OPHIO)).unwrap(), program);
        assert!(host.calls.borrow().is_empty());
    }

    #[test]
    fn identifiers_never_name_paths() {
        for id in ["", "../1", "1/2", "C:\\tmp", "name", ".", "1\n"] {
            assert!(!valid_id(id), "{id:?}");
        }
        assert!(valid_id("12345-678"));
    }
}
