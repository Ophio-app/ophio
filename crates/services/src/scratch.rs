// SPDX-License-Identifier: Apache-2.0
//! A task's scratch folder: where its runs put the screenshots, recordings,
//! builds and logs nobody needs to keep, and the temporary folder of what
//! they start. Ophio deletes it once the task is done with it, so they don't
//! pile up in the project, or in a temporary folder held in memory.

use crate::{CompanionHub, internal};
use protocol::{
    ProtocolError,
    ids::TaskId,
    model::{Run, Task, TaskState},
    queue::QueueView,
};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

/// Under the data folder.
const SCRATCH: &str = "scratch";
/// Kept this long after its task finishes, for a look back or a follow-up.
pub(crate) const KEPT_FOR: Duration = Duration::from_secs(2 * 24 * 60 * 60);
/// Past this in all, finished tasks' folders go sooner, oldest first.
const MAX_TOTAL: u64 = 10 * 1024 * 1024 * 1024;
/// A folder whose task isn't known is left this long: its task may have
/// been made after the tasks were read.
const ORPHAN_AFTER: Duration = Duration::from_secs(60 * 60);
/// Programs put sockets in the temporary folder, and a socket's path can't
/// pass 103 bytes on macOS, 107 on Linux; Chromium's is 46 more than the
/// folder's. A longer folder is still the scratch folder, just not the
/// temporary one.
#[cfg(unix)]
const MAX_TMPDIR: usize = 56;

impl CompanionHub {
    /// The task's scratch folder, named for the random end of its id: short
    /// enough to be a temporary folder.
    pub(crate) fn scratch_dir(&self, task_id: TaskId) -> PathBuf {
        self.inner.data.join(SCRATCH).join(name(task_id))
    }
    /// Makes the task's scratch folder for a run and points the run's
    /// environment at it. None if it can't be made: the run goes on without.
    pub(crate) async fn prepare_scratch(
        &self,
        task_id: TaskId,
        env: &mut ports::RunEnv,
    ) -> Option<PathBuf> {
        let _making = self.inner.scratch.read().await;
        let dir = self.scratch_dir(task_id);
        let mut builder = tokio::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        builder.mode(0o700);
        if let Err(error) = builder.create(&dir).await {
            tracing::warn!(%error, "scratch folder couldn't be made");
            return None;
        }
        scratch_env(dir.to_str()?, env);
        Some(dir)
    }
    /// Deletes the scratch folders tasks are done with: an archived task's,
    /// a finished one's after [`KEPT_FOR`], and finished ones' sooner while
    /// they come to more than [`MAX_TOTAL`]. A task that is running, waiting
    /// or queued keeps its folder whatever its size.
    pub(crate) async fn collect_scratch(&self) -> Result<(), ProtocolError> {
        let _collecting = self.inner.scratch.write().await;
        let now = self.inner.clock.now().millis();
        let tasks = self.read(|r| {
            // Continuations keep the previous terminal task state while
            // waiting in Next up, so task state alone cannot show need.
            let queued: HashSet<_> = r
                .list::<QueueView>("queue")?
                .into_iter()
                .flat_map(|queue| queue.items)
                .filter(|item| item.is_pending())
                .map(|item| item.task_id)
                .collect();
            let mut tasks = HashMap::new();
            for task in r.list::<Task>("task")? {
                let need = match task.state {
                    _ if queued.contains(&task.task_id) || running(r, &task) => Need::Running,
                    TaskState::Archived => Need::Done,
                    TaskState::Completed | TaskState::Failed | TaskState::Interrupted => {
                        Need::Finished(task.updated_at.millis())
                    }
                    _ => Need::Running,
                };
                tasks.insert(name(task.task_id), need);
            }
            Ok(tasks)
        })?;
        let root = self.inner.data.join(SCRATCH);
        tokio::task::spawn_blocking(move || sweep(&root, &tasks, now, SystemTime::now(), MAX_TOTAL))
            .await
            .map_err(|_| internal("Scratch cleanup stopped"))?
            .map_err(internal)
    }
}

/// Whether the task's current run is still at work or still finishing.
pub(crate) fn running(r: &store::Read<'_>, task: &Task) -> bool {
    task.current_run.is_some_and(|id| {
        r.get::<Run>("run", id)
            .ok()
            .flatten()
            .is_some_and(|run| !run.state.is_terminal() || crate::runs::still_finishing(&run))
    })
}

/// Points a run's environment at its scratch folder, `path`: its name, and
/// the temporary folder of what it starts where that can be.
pub(crate) fn scratch_env(path: &str, env: &mut ports::RunEnv) {
    #[cfg(unix)]
    if path.len() <= MAX_TMPDIR {
        env.0.push(("TMPDIR".into(), path.into()));
    }
    #[cfg(windows)]
    for name in ["TEMP", "TMP"] {
        env.0.push((name.into(), path.into()));
    }
    env.0.push((ports::SCRATCH_ENV.into(), path.into()));
}

/// What a scratch folder's task still needs of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Need {
    /// Running, waiting on the person, queued or uncertain.
    Running,
    /// Finished at this time, in milliseconds.
    Finished(i64),
    /// Archived.
    Done,
}

pub(crate) fn name(task_id: TaskId) -> String {
    let id = task_id.as_uuid().simple().to_string();
    id[id.len() - 12..].to_owned()
}

/// Removes what [`CompanionHub::collect_scratch`] says, from the folders in
/// `root` named for `tasks`, keeping finished ones within `max_total`. `now`
/// is the store's clock, `wall` the file system's. A folder that can't be
/// removed is left and the rest go on.
pub(crate) fn sweep(
    root: &Path,
    tasks: &HashMap<String, Need>,
    now: i64,
    wall: SystemTime,
    max_total: u64,
) -> std::io::Result<()> {
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    let device = filesystem(&std::fs::symlink_metadata(root)?);
    let mut total = 0u64;
    let mut finished = Vec::new();
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let need = tasks.get(&*entry.file_name().to_string_lossy()).copied();
        let remove = match need {
            None => std::fs::symlink_metadata(&path)
                .and_then(|m| m.modified())
                .is_ok_and(|at| wall.duration_since(at).is_ok_and(|age| age >= ORPHAN_AFTER)),
            Some(Need::Done) => true,
            Some(Need::Finished(at)) => now.saturating_sub(at) >= KEPT_FOR.as_millis() as i64,
            Some(Need::Running) => false,
        };
        if remove {
            removed(&path, device);
            continue;
        }
        let size = size(&path, device);
        total = total.saturating_add(size);
        if let Some(Need::Finished(at)) = need {
            finished.push((at, path, size));
        }
    }
    finished.sort_by_key(|(at, ..)| *at);
    for (_, path, size) in finished {
        if total <= max_total {
            break;
        }
        if removed(&path, device) {
            total = total.saturating_sub(size);
        }
    }
    Ok(())
}

fn removed(path: &Path, device: Option<u64>) -> bool {
    match remove_tree(path, device) {
        Ok(()) => true,
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "scratch folder couldn't be removed");
            false
        }
    }
}

/// Removes `path` and everything in it. A link is removed, not followed, and
/// a folder on another file system, as a drive mounted inside, is left with
/// what holds it: what's in it isn't the scratch folder's.
fn remove_tree(path: &Path, device: Option<u64>) -> std::io::Result<()> {
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_dir() {
        return remove_entry(path, &metadata);
    }
    if filesystem(&metadata) != device {
        return Err(std::io::Error::other(
            "another file system is mounted in it",
        ));
    }
    writable(path, &metadata)?;
    let mut outcome = Ok(());
    for entry in std::fs::read_dir(path)? {
        if let Err(e) = entry.and_then(|entry| remove_tree(&entry.path(), device)) {
            outcome = Err(e);
        }
    }
    outcome?;
    std::fs::remove_dir(path)
}

/// Bytes in `path`, on the scratch folder's file system and not through links.
fn size(path: &Path, device: Option<u64>) -> u64 {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return 0;
    };
    if !metadata.is_dir() {
        return metadata.len();
    }
    if filesystem(&metadata) != device {
        return 0;
    }
    std::fs::read_dir(path).map_or(0, |entries| {
        entries
            .flatten()
            .map(|entry| size(&entry.path(), device))
            .fold(0, u64::saturating_add)
    })
}

#[cfg(unix)]
fn filesystem(metadata: &std::fs::Metadata) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    Some(metadata.dev())
}
/// Windows marks a mounted folder as a link, which is never followed.
#[cfg(windows)]
fn filesystem(_: &std::fs::Metadata) -> Option<u64> {
    None
}

/// A folder made read-only, as some tools leave their caches, can't have
/// what's in it removed until it is writable again.
#[cfg(unix)]
fn writable(path: &Path, metadata: &std::fs::Metadata) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = metadata.permissions().mode();
    if mode & 0o700 != 0o700 {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode | 0o700))?;
    }
    Ok(())
}
#[cfg(windows)]
fn writable(_: &Path, _: &std::fs::Metadata) -> std::io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn remove_entry(path: &Path, _: &std::fs::Metadata) -> std::io::Result<()> {
    std::fs::remove_file(path)
}
/// A link to a folder is removed as a folder, and a read-only file is made
/// writable first.
#[cfg(windows)]
fn remove_entry(path: &Path, metadata: &std::fs::Metadata) -> std::io::Result<()> {
    use std::os::windows::fs::FileTypeExt;
    if metadata.file_type().is_symlink_dir() {
        return std::fs::remove_dir(path);
    }
    if metadata.permissions().readonly() {
        let mut permissions = metadata.permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        permissions.set_readonly(false);
        std::fs::set_permissions(path, permissions)?;
    }
    std::fs::remove_file(path)
}
