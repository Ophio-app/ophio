// SPDX-License-Identifier: Apache-2.0
use crate::{
    Core, lock,
    path::{Root, normalize},
};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use ports::library::LibraryError;
use protocol::ids::LibraryRootId;
#[cfg(windows)]
use std::sync::atomic::AtomicBool;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

/// A refresh may acknowledge only requests that preceded its registration.
/// A Boolean cleared after registration loses errors arriving during startup.
#[derive(Debug, Default)]
pub(crate) struct Rescan {
    requested: AtomicU64,
    acknowledged: AtomicU64,
    #[cfg(windows)]
    repair: AtomicBool,
}

impl Rescan {
    pub(crate) fn mark(&self) {
        self.requested.fetch_add(1, Ordering::AcqRel);
    }

    #[cfg(windows)]
    fn watch_failed(&self) {
        // Keep this request even if the bounded event queue is full. The
        // worker retries registration independently of change subscribers.
        self.repair.store(true, Ordering::Release);
        self.mark();
    }

    pub(crate) fn required(&self) -> bool {
        self.requested.load(Ordering::Acquire) != self.acknowledged.load(Ordering::Acquire)
    }

    pub(crate) fn refresh<T, E>(&self, install: impl FnOnce() -> Result<T, E>) -> Result<T, E> {
        let before = self.requested.load(Ordering::Acquire);
        let result = install()?;
        // Root refreshes are serialized by Core.state. A later request remains
        // outstanding even if the callback runs just before this store.
        self.acknowledged.store(before, Ordering::Release);
        Ok(result)
    }
}

pub(crate) fn watcher(
    core: &Core,
    roots: &BTreeMap<LibraryRootId, Arc<Root>>,
) -> Result<RecommendedWatcher, LibraryError> {
    let sender = core.watch_queue.clone();
    let overflow = core.rescan_required.clone();
    #[cfg(windows)]
    let scopes: Vec<_> = roots
        .values()
        .map(|root| WatchScope {
            path: root.canonical.clone(),
            only: root.spec.only.clone(),
        })
        .collect();
    let mut watcher = RecommendedWatcher::new(
        move |event: notify::Result<notify::Event>| {
            #[cfg(windows)]
            let event = match event {
                Ok(mut event) => {
                    if !retain_granted_paths(&mut event, &scopes) {
                        return;
                    }
                    Ok(event)
                }
                Err(error) => {
                    overflow.watch_failed();
                    Err(error)
                }
            };
            if sender.try_send(event).is_err() {
                overflow.mark();
            }
        },
        notify::Config::default().with_follow_symlinks(false),
    )
    .map_err(|_| std::io::Error::other("could not initialize Library watcher"))?;
    if !register_paths(&mut watcher, roots) {
        #[cfg(windows)]
        core.rescan_required.watch_failed();
        #[cfg(not(windows))]
        core.rescan_required.mark();
    }
    Ok(watcher)
}

fn register_paths(watcher: &mut impl Watcher, roots: &BTreeMap<LibraryRootId, Arc<Root>>) -> bool {
    let mut complete = true;
    // FSEvents restarts its stream for every watch() call. Register all paths
    // together so refreshing Library roots pays that startup cost only once,
    // rather than making a new agent or editable copy wait for every path.
    let mut paths = watcher.paths_mut();
    #[cfg(windows)]
    let groups = {
        let plan = windows_watch_groups(roots);
        if !plan.complete {
            complete = false;
            tracing::warn!(
                "Library grant could not be inspected; some outside changes will not be announced"
            );
        }
        plan.groups
    };
    #[cfg(not(windows))]
    let groups = roots.values().map(|root| watched(root));
    for group in groups {
        // A Windows path failure must not suppress a later shared path whose
        // duplicate was removed from another root's group. Try every unique
        // path there; other platforms retain their existing per-root stop.
        #[cfg(windows)]
        let failed = {
            let mut failed = false;
            for (path, mode) in group {
                if paths.add(&path, mode).is_err() {
                    failed = true;
                }
            }
            failed
        };
        #[cfg(not(windows))]
        let failed = group
            .into_iter()
            .any(|(path, mode)| paths.add(&path, mode).is_err());
        if failed {
            complete = false;
            tracing::warn!(
                "Library root is only partly watched; some outside changes will not be announced"
            );
        }
    }
    if paths.commit().is_err() {
        complete = false;
        tracing::warn!("Library watcher could not start; outside changes will not be announced");
    }
    complete
}

/// Only a full-access recursive root covers nested roots. Limited roots keep
/// independent top-folder and granted-subfolder watches, so unrelated trees
/// cannot consume the same native buffer as a nested project.
#[cfg(any(windows, test))]
fn windows_watch_paths(roots: &BTreeMap<LibraryRootId, Arc<Root>>) -> BTreeSet<std::path::PathBuf> {
    let recursive: BTreeSet<_> = roots
        .values()
        .filter(|root| root.spec.only.is_none())
        .map(|root| root.canonical.clone())
        .collect();
    roots
        .values()
        .map(|root| &root.canonical)
        .filter(|path| {
            !path
                .ancestors()
                .skip(1)
                .any(|parent| recursive.contains(parent))
        })
        .cloned()
        .collect()
}

/// notify's Windows backend overwrites an existing registration without closing
/// its old native handle. Register each exact path once across all roots and
/// grants, selecting recursive coverage if any grant requires it. Keep groups
/// for partial-registration diagnostics without retrying a duplicate path.
#[cfg(any(windows, test))]
struct WindowsWatchPlan {
    groups: Vec<BTreeSet<(std::path::PathBuf, RecursiveMode)>>,
    complete: bool,
}

#[cfg(any(windows, test))]
fn windows_watch_groups(roots: &BTreeMap<LibraryRootId, Arc<Root>>) -> WindowsWatchPlan {
    let selected = windows_watch_paths(roots);
    let mut complete = true;
    let groups: Vec<_> = roots
        .values()
        .filter(|root| {
            selected.contains(&root.canonical)
                && !(root.spec.only.is_some()
                    && roots.values().any(|other| {
                        other.canonical == root.canonical && other.spec.only.is_none()
                    }))
        })
        .map(|root| {
            let (paths, inspected) = windows_watched(root);
            complete &= inspected;
            paths
        })
        .collect();
    let mut pending = BTreeMap::new();
    for (path, mode) in groups.iter().flatten() {
        let selected = pending.entry(path.clone()).or_insert(*mode);
        if *mode == RecursiveMode::Recursive {
            *selected = RecursiveMode::Recursive;
        }
    }
    let groups = groups
        .into_iter()
        .map(|group| {
            group
                .into_iter()
                .filter_map(|(path, _)| pending.remove(&path).map(|mode| (path, mode)))
                .collect()
        })
        .collect();
    WindowsWatchPlan { groups, complete }
}

#[cfg(any(windows, test))]
struct WatchScope {
    path: std::path::PathBuf,
    only: Option<Vec<String>>,
}

/// A recursive Windows root watch also sees siblings of limited paths. Discard
/// those paths before they can fill the queue or wake the hashing worker. Keep
/// ancestor-folder changes that can affect granted descendants, and rescan
/// signals even when they have no granted path attached.
#[cfg(any(windows, test))]
fn retain_granted_paths(event: &mut notify::Event, scopes: &[WatchScope]) -> bool {
    event.paths.retain(|path| {
        scopes.iter().any(|scope| {
            let Some(relative) = path.strip_prefix(&scope.path).ok().and_then(|p| p.to_str())
            else {
                return false;
            };
            let Ok(relative) = normalize(&relative.replace(std::path::MAIN_SEPARATOR, "/")) else {
                return false;
            };
            relative.is_empty() || protocol::library::reaches(scope.only.as_deref(), &relative)
        })
    });
    !event.paths.is_empty() || event.need_rescan()
}

/// Windows verbatim paths do not interpret a slash inside a pushed string as
/// a separator. Wire grants use slashes, so append one validated part at a time.
#[cfg(any(windows, test))]
fn grant_path(root: &std::path::Path, grant: &str) -> std::path::PathBuf {
    grant.split('/').fold(root.to_owned(), |mut path, part| {
        path.push(part);
        path
    })
}

#[cfg(any(windows, test))]
fn windows_watched(root: &Root) -> (BTreeSet<(std::path::PathBuf, RecursiveMode)>, bool) {
    windows_watched_with(root, |path| std::fs::symlink_metadata(path))
}

#[cfg(any(windows, test))]
fn windows_watched_with(
    root: &Root,
    mut inspect: impl FnMut(&std::path::Path) -> std::io::Result<std::fs::Metadata>,
) -> (BTreeSet<(std::path::PathBuf, RecursiveMode)>, bool) {
    let Some(only) = &root.spec.only else {
        return (
            BTreeSet::from([(root.canonical.clone(), RecursiveMode::Recursive)]),
            true,
        );
    };
    let mut paths = BTreeSet::from([(root.canonical.clone(), RecursiveMode::NonRecursive)]);
    let mut complete = true;
    for entry in only {
        let path = grant_path(&root.canonical, entry);
        match inspect(&path) {
            Ok(meta) if meta.is_dir() && !crate::path::is_link(&meta) => {
                paths.insert((path, RecursiveMode::Recursive));
            }
            Ok(meta) if meta.is_file() => {
                if let Some(parent) = path.parent() {
                    paths.insert((parent.to_owned(), RecursiveMode::NonRecursive));
                }
            }
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => complete = false,
            // Missing grants are rechecked at root refresh. Links and special
            // files are intentionally not watched through.
            _ => {}
        }
    }
    (paths, complete)
}

/// A file is watched through its folder, as an editor may replace it. For
/// limited roots, only existing granted subfolders receive recursive handles;
/// siblings never share those handles' native buffers. One made later is watched
/// once the roots are set again.
#[cfg(not(windows))]
fn watched(root: &Root) -> BTreeSet<(std::path::PathBuf, RecursiveMode)> {
    let Some(only) = &root.spec.only else {
        return BTreeSet::from([(root.canonical.clone(), RecursiveMode::Recursive)]);
    };
    let mut watched = BTreeSet::from([(root.canonical.clone(), RecursiveMode::NonRecursive)]);
    for entry in only {
        let path = root.canonical.join(entry);
        match std::fs::symlink_metadata(&path) {
            Ok(meta) if meta.is_dir() && !crate::path::is_link(&meta) => {
                watched.insert((path, RecursiveMode::Recursive));
            }
            Ok(meta) if meta.is_file() => {
                if let Some(parent) = path.parent() {
                    watched.insert((parent.to_owned(), RecursiveMode::NonRecursive));
                }
            }
            _ => {}
        }
    }
    watched
}

/// Retirement normally closes old directory handles before refresh returns.
/// A failed or stuck backend must not prevent installing the replacement.
#[cfg(windows)]
pub(crate) fn retire_watcher(core: &Core, state: &mut crate::State) {
    if let Some(mut watcher) = state.watcher.take() {
        // Capture paths only: keeping old Root Arcs in the retirement task would
        // also keep their strict identity pins alive if native cancellation hangs.
        let paths = retirement_paths(&state.roots);
        if !retire_bounded(
            move || retire_paths(&mut watcher, paths),
            Duration::from_secs(2),
        ) {
            core.rescan_required.mark();
            tracing::warn!(
                "Previous Library watches did not confirm retirement; old handles may still block folder moves"
            );
        }
    }
}

#[cfg(any(windows, test))]
fn retirement_paths(roots: &BTreeMap<LibraryRootId, Arc<Root>>) -> BTreeSet<std::path::PathBuf> {
    let mut paths = BTreeSet::new();
    for root in roots.values() {
        paths.insert(root.canonical.clone());
        for entry in root.spec.only.iter().flatten() {
            let path = grant_path(&root.canonical, entry);
            if let Some(parent) = path.parent() {
                paths.insert(parent.to_owned());
            }
            paths.insert(path);
        }
    }
    paths
}

#[cfg(any(windows, test))]
fn retire_bounded(
    retire: impl FnOnce() -> notify::Result<()> + Send + 'static,
    timeout: Duration,
) -> bool {
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    if std::thread::Builder::new()
        .name("library-watch-retire".into())
        .spawn(move || {
            let _ = sender.send(retire());
        })
        .is_err()
    {
        return false;
    }
    matches!(receiver.recv_timeout(timeout), Ok(Ok(())))
}

#[cfg(any(windows, test))]
fn retire_paths(
    watcher: &mut impl Watcher,
    paths: BTreeSet<std::path::PathBuf>,
) -> notify::Result<()> {
    // Do not use watched(root): a registered directory may have disappeared or
    // changed type since registration. notify keys its handles by the original
    // path, and its Windows backend accepts removal of an absent registration.
    for path in paths {
        watcher.unwatch(&path)?;
    }
    // notify 8.2.0 processes this acknowledgment after all preceding unwatch
    // actions have cancelled I/O and closed their directory handles. Windows
    // returns Ok(false) because configuration itself is a no-op; that is still
    // a successful acknowledgment, not a failure to retire the old handles.
    watcher.configure(notify::Config::default())?;
    Ok(())
}

#[cfg(any(windows, test))]
fn repair_due(last_attempt: Option<std::time::Instant>, now: std::time::Instant) -> bool {
    last_attempt.is_none_or(|last| now.saturating_duration_since(last) >= SWEEP_EVERY)
}

#[cfg(windows)]
fn repair_watcher(core: &Core, last_attempt: &mut Option<std::time::Instant>) {
    let now = std::time::Instant::now();
    // An inaccessible grant must not rebuild every healthy registration on
    // every event batch. Keep the request pending until the next retry window.
    if !repair_due(*last_attempt, now) || !core.rescan_required.repair.swap(false, Ordering::AcqRel)
    {
        return;
    }
    *last_attempt = Some(now);
    let mut state = lock(&core.state);
    // Rebuild from the current authorized roots, not stale paths carried by an
    // error. Do not acknowledge the rescan: the person still needs a fresh view.
    match watcher(core, &state.roots) {
        Ok(watcher) => {
            retire_watcher(core, &mut state);
            state.watcher = Some(watcher);
        }
        Err(_) => {
            core.rescan_required.watch_failed();
            tracing::warn!("Library watcher recovery failed; registration will be retried");
        }
    }
}

const SWEEP_EVERY: Duration = Duration::from_secs(60);

pub(crate) fn start(
    core: &Arc<Core>,
    receiver: std::sync::mpsc::Receiver<notify::Result<notify::Event>>,
) -> Result<(), LibraryError> {
    let core = Arc::downgrade(core);
    std::thread::Builder::new()
        .name("library-watch".into())
        .spawn(move || {
            let mut swept_at = std::time::Instant::now();
            #[cfg(windows)]
            let mut last_repair_attempt = None;
            loop {
                #[cfg(windows)]
                {
                    let Some(core) = core.upgrade() else { break };
                    repair_watcher(&core, &mut last_repair_attempt);
                }
                // Also expire confirmed saves while idle and without clients.
                // Busy filesystem events cannot postpone this check.
                if swept_at.elapsed() >= SWEEP_EVERY {
                    let Some(core) = core.upgrade() else { break };
                    if core.sweep_confirmed(&lock(&core.state)).is_err() {
                        tracing::warn!("Could not sweep confirmed Library saves");
                    }
                    swept_at = std::time::Instant::now();
                }
                // Dropping the library drops every sender, which ends this
                // wait at once; otherwise sleep until the next sweep.
                let idle = SWEEP_EVERY.saturating_sub(swept_at.elapsed());
                let event = match receiver.recv_timeout(idle) {
                    Ok(event) => event,
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                        if core.strong_count() == 0 {
                            break;
                        }
                        continue;
                    }
                    Err(_) => break,
                };
                let Some(core) = core.upgrade() else { break };
                let mut paths = BTreeSet::new();
                let collect =
                    |event: notify::Result<notify::Event>,
                     paths: &mut BTreeSet<std::path::PathBuf>| {
                        match event {
                            Ok(event) => {
                                if event.need_rescan() {
                                    core.rescan_required.mark();
                                }
                                // Reads generate access notifications on some backends.
                                if event.kind.is_access() {
                                    return;
                                }
                                for path in event.paths {
                                    if paths.len() < 4096 {
                                        paths.insert(path);
                                    } else {
                                        core.rescan_required.mark();
                                    }
                                }
                            }
                            Err(_) => {
                                #[cfg(windows)]
                                core.rescan_required.watch_failed();
                                #[cfg(not(windows))]
                                core.rescan_required.mark();
                            }
                        }
                    };
                collect(event, &mut paths);
                // A fixed window cannot be extended indefinitely by a busy writer.
                std::thread::sleep(Duration::from_millis(80));
                for event in receiver.try_iter().take(4096) {
                    collect(event, &mut paths);
                }
                if core.events.receiver_count() == 0 {
                    continue;
                }
                let state = lock(&core.state);
                for path in paths {
                    for root in state.roots.values() {
                        let Ok(relative) = path.strip_prefix(&root.canonical) else {
                            continue;
                        };
                        let Some(relative) = relative.to_str() else {
                            continue;
                        };
                        let Ok(relative) =
                            normalize(&relative.replace(std::path::MAIN_SEPARATOR, "/"))
                        else {
                            continue;
                        };
                        // A change beside a limited root's paths is none
                        // of its own.
                        if relative.is_empty() || !root.holds(&relative) {
                            continue;
                        }
                        match core.read(root, &relative) {
                            Ok(file) => {
                                core.emit(root.spec.root_id, &relative, Some(file.revision))
                            }
                            Err(LibraryError::NotFound) => {
                                core.emit(root.spec.root_id, &relative, None)
                            }
                            Err(_) => core.rescan_required.mark(),
                        }
                    }
                }
            }
        })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::{PathsMut, WatcherKind};
    use ports::library::RootSpec;
    use protocol::library::RootKind;
    use std::path::{Path, PathBuf};

    #[derive(Default)]
    struct FakeWatcher {
        added: Vec<(PathBuf, RecursiveMode)>,
        commits: usize,
        fail: Option<PathBuf>,
        fail_commit: bool,
    }

    struct FakePaths<'a>(&'a mut FakeWatcher);

    impl PathsMut for FakePaths<'_> {
        fn add(&mut self, path: &Path, mode: RecursiveMode) -> notify::Result<()> {
            if self.0.fail.as_deref() == Some(path) {
                return Err(notify::Error::generic("watch limit"));
            }
            self.0.added.push((path.to_owned(), mode));
            Ok(())
        }

        fn remove(&mut self, _: &Path) -> notify::Result<()> {
            panic!("registration must not remove paths")
        }

        fn commit(self: Box<Self>) -> notify::Result<()> {
            self.0.commits += 1;
            if self.0.fail_commit {
                return Err(notify::Error::generic("stream startup failed"));
            }
            Ok(())
        }
    }

    impl Watcher for FakeWatcher {
        fn new<F: notify::EventHandler>(_: F, _: notify::Config) -> notify::Result<Self> {
            Ok(Self::default())
        }

        fn watch(&mut self, _: &Path, _: RecursiveMode) -> notify::Result<()> {
            panic!("registration must batch paths, not call watch directly")
        }

        fn unwatch(&mut self, _: &Path) -> notify::Result<()> {
            panic!("registration must not unwatch paths")
        }

        fn paths_mut(&mut self) -> Box<dyn PathsMut + '_> {
            Box::new(FakePaths(self))
        }

        fn kind() -> WatcherKind {
            WatcherKind::NullWatcher
        }
    }

    #[test]
    fn windows_retirement_removes_stale_paths_before_acknowledging_and_propagates_failure() {
        #[derive(Default)]
        struct RetiringWatcher {
            actions: Vec<Option<PathBuf>>,
            fail_remove: bool,
            fail_acknowledgment: bool,
        }
        impl Watcher for RetiringWatcher {
            fn new<F: notify::EventHandler>(_: F, _: notify::Config) -> notify::Result<Self> {
                Ok(Self::default())
            }

            fn watch(&mut self, _: &Path, _: RecursiveMode) -> notify::Result<()> {
                panic!("retirement must not register new paths")
            }

            fn unwatch(&mut self, path: &Path) -> notify::Result<()> {
                self.actions.push(Some(path.to_owned()));
                if self.fail_remove {
                    Err(notify::Error::generic("removal channel failed"))
                } else {
                    Ok(())
                }
            }

            fn configure(&mut self, _: notify::Config) -> notify::Result<bool> {
                self.actions.push(None);
                if self.fail_acknowledgment {
                    Err(notify::Error::generic("acknowledgment channel failed"))
                } else {
                    Ok(false)
                }
            }

            fn kind() -> WatcherKind {
                WatcherKind::NullWatcher
            }
        }

        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("root");
        std::fs::create_dir_all(path.join("removed/deep")).unwrap();
        let id = LibraryRootId::new();
        let root = Arc::new(
            Root::new(RootSpec {
                root_id: id,
                path,
                label: "limited".into(),
                kind: RootKind::Project,
                writable: true,
                project_id: None,
                harness: None,
                only: Some(vec!["removed/deep".into(), "leaf.md".into()]),
            })
            .unwrap(),
        );
        std::fs::remove_dir_all(root.canonical.join("removed")).unwrap();
        let expected: BTreeSet<_> = [
            root.canonical.clone(),
            root.canonical.join("removed"),
            root.canonical.join("removed/deep"),
            root.canonical.join("leaf.md"),
        ]
        .into_iter()
        .map(Some)
        .collect();
        let roots = BTreeMap::from([(id, root)]);
        let mut watcher = RetiringWatcher::default();
        retire_paths(&mut watcher, retirement_paths(&roots)).unwrap();
        assert_eq!(watcher.actions.pop(), Some(None));
        assert_eq!(watcher.actions, expected.into_iter().collect::<Vec<_>>());

        let mut watcher = RetiringWatcher {
            fail_remove: true,
            ..RetiringWatcher::default()
        };
        assert!(retire_paths(&mut watcher, retirement_paths(&roots)).is_err());
        assert!(watcher.actions.iter().all(Option::is_some));
        let mut watcher = RetiringWatcher {
            fail_acknowledgment: true,
            ..RetiringWatcher::default()
        };
        assert!(retire_paths(&mut watcher, retirement_paths(&roots)).is_err());
        assert_eq!(watcher.actions.last(), Some(&None));
    }

    #[test]
    fn retirement_wait_reports_success_failure_and_timeout() {
        assert!(retire_bounded(|| Ok(()), Duration::from_secs(2)));
        assert!(!retire_bounded(
            || Err(notify::Error::generic("backend command channel is dead")),
            Duration::from_secs(2),
        ));
        let (release, blocked) = std::sync::mpsc::channel();
        let (finished, completion) = std::sync::mpsc::channel();
        let started = std::time::Instant::now();
        assert!(!retire_bounded(
            move || {
                blocked.recv().unwrap();
                finished.send(()).unwrap();
                Ok(())
            },
            Duration::from_millis(30),
        ));
        assert!(started.elapsed() < Duration::from_secs(2));
        // Release the authored stuck backend so this test leaves no thread.
        release.send(()).unwrap();
        completion.recv_timeout(Duration::from_secs(2)).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn windows_verbatim_grants_append_native_path_components() {
        for root in [
            PathBuf::from(r"\\?\UNC\server\share\project"),
            PathBuf::from(format!(r"\\?\C:\{}", "long-root".repeat(40))),
        ] {
            let joined = grant_path(&root, ".claude/agents");
            assert_eq!(joined, root.join(".claude").join("agents"));
            assert!(!joined.as_os_str().to_string_lossy().contains('/'));
        }
    }

    #[test]
    fn windows_missing_grants_are_distinct_from_uninspectable_grants() {
        let temp = tempfile::tempdir().unwrap();
        let root = Root::new(RootSpec {
            root_id: LibraryRootId::new(),
            path: temp.path().to_owned(),
            label: "Test".into(),
            kind: RootKind::Project,
            writable: true,
            project_id: None,
            harness: None,
            only: Some(vec![".claude/agents".into(), ".claude/skills".into()]),
        })
        .unwrap();
        let (paths, complete) = windows_watched_with(&root, |_| {
            Err(std::io::Error::from(std::io::ErrorKind::NotFound))
        });
        assert!(complete);
        assert_eq!(
            paths,
            BTreeSet::from([(root.canonical.clone(), RecursiveMode::NonRecursive)])
        );
        for kind in [
            std::io::ErrorKind::PermissionDenied,
            std::io::ErrorKind::InvalidInput,
        ] {
            let mut inspected = 0;
            let (paths, complete) = windows_watched_with(&root, |_| {
                inspected += 1;
                Err(std::io::Error::from(kind))
            });
            assert!(!complete, "an inspection failure must require a rescan");
            assert_eq!(
                inspected, 2,
                "a failed grant must not suppress later grants"
            );
            assert_eq!(paths.len(), 1);
        }
    }

    #[test]
    fn refresh_acknowledges_only_preexisting_rescan_requests_and_only_after_success() {
        let rescan = Rescan::default();
        assert!(!rescan.required());
        rescan.mark();
        rescan.refresh(|| Ok::<_, ()>(())).unwrap();
        assert!(!rescan.required());

        rescan.mark();
        assert!(rescan.refresh(|| Err::<(), _>(())).is_err());
        assert!(rescan.required());

        // Model an error/overflow callback while registration is in progress.
        // Installation must not erase the newer request.
        rescan
            .refresh(|| {
                rescan.mark();
                Ok::<_, ()>(())
            })
            .unwrap();
        assert!(rescan.required());
        rescan.refresh(|| Ok::<_, ()>(())).unwrap();
        assert!(!rescan.required());
        rescan.mark();
        assert!(rescan.required());
    }

    #[test]
    fn failed_batch_commit_is_reported_as_incomplete() {
        let mut watcher = FakeWatcher {
            fail_commit: true,
            ..FakeWatcher::default()
        };
        assert!(!register_paths(&mut watcher, &BTreeMap::new()));
        assert_eq!(watcher.commits, 1);
    }

    #[test]
    fn registration_batches_every_root_and_commits_even_after_a_failed_add_or_empty_set() {
        let mut watcher = FakeWatcher::default();
        assert!(register_paths(&mut watcher, &BTreeMap::new()));
        assert!(watcher.added.is_empty());
        assert_eq!(watcher.commits, 1);

        let temp = tempfile::tempdir().unwrap();
        let mut roots = BTreeMap::new();
        for name in ["first", "second"] {
            let path = temp.path().join(name);
            std::fs::create_dir_all(path.join("agents")).unwrap();
            let id = LibraryRootId::new();
            let spec = RootSpec {
                root_id: id,
                path,
                label: name.into(),
                kind: RootKind::Project,
                writable: true,
                project_id: None,
                harness: None,
                only: Some(vec!["agents".into()]),
            };
            roots.insert(id, Arc::new(Root::new(spec).unwrap()));
        }
        let mut ordered = roots.values();
        let failed = ordered.next().unwrap();
        let good = ordered.next().unwrap();
        watcher.fail = Some(failed.canonical.clone());
        assert!(!register_paths(&mut watcher, &roots));
        assert_eq!(watcher.commits, 2);
        let expected = vec![
            (good.canonical.clone(), RecursiveMode::NonRecursive),
            (good.canonical.join("agents"), RecursiveMode::Recursive),
        ];
        #[cfg(windows)]
        let expected = std::iter::once((failed.canonical.join("agents"), RecursiveMode::Recursive))
            .chain(expected)
            .collect::<Vec<_>>();
        assert_eq!(watcher.added, expected);
    }

    #[test]
    fn windows_registration_uses_only_top_level_roots_without_duplicate_handles() {
        let temp = tempfile::tempdir().unwrap();
        let mut roots = BTreeMap::new();
        for name in ["outer", "outer/nested", "outer", "peer"] {
            let path = temp.path().join(name);
            std::fs::create_dir_all(&path).unwrap();
            let id = LibraryRootId::new();
            let spec = RootSpec {
                root_id: id,
                path,
                label: "Test".into(),
                kind: RootKind::Project,
                writable: true,
                project_id: None,
                harness: None,
                only: None,
            };
            roots.insert(id, Arc::new(Root::new(spec).unwrap()));
        }
        assert_eq!(
            windows_watch_paths(&roots),
            BTreeSet::from([
                ports::library::canonical(&temp.path().join("outer")).unwrap(),
                ports::library::canonical(&temp.path().join("peer")).unwrap(),
            ])
        );
        // On Windows also exercise the registration path, not just selection.
        if cfg!(windows) {
            let mut watcher = FakeWatcher::default();
            assert!(register_paths(&mut watcher, &roots));
            assert_eq!(watcher.commits, 1);
            assert_eq!(watcher.added.len(), 2);
            assert_eq!(
                watcher.added.into_iter().collect::<BTreeSet<_>>(),
                windows_watch_paths(&roots)
                    .into_iter()
                    .map(|path| (path, RecursiveMode::Recursive))
                    .collect()
            );
        }
    }

    #[test]
    fn windows_limited_roots_do_not_subsume_nested_project_watches() {
        let temp = tempfile::tempdir().unwrap();
        let mut roots = BTreeMap::new();
        for (name, only) in [
            ("limited", Some(vec!["agents".into()])),
            ("limited/project", None),
            ("limited/project/nested", None),
            ("limited/other", Some(vec!["skills".into()])),
        ] {
            let path = temp.path().join(name);
            std::fs::create_dir_all(path.join("agents")).unwrap();
            std::fs::create_dir_all(path.join("skills")).unwrap();
            let id = LibraryRootId::new();
            roots.insert(
                id,
                Arc::new(
                    Root::new(RootSpec {
                        root_id: id,
                        path,
                        label: "Test".into(),
                        kind: RootKind::Project,
                        writable: true,
                        project_id: None,
                        harness: None,
                        only,
                    })
                    .unwrap(),
                ),
            );
        }
        let canonical = ports::library::canonical(&temp.path().join("limited")).unwrap();
        assert_eq!(
            windows_watch_paths(&roots),
            BTreeSet::from([
                canonical.clone(),
                canonical.join("project"),
                canonical.join("other"),
            ])
        );
        if cfg!(windows) {
            let mut watcher = FakeWatcher::default();
            assert!(register_paths(&mut watcher, &roots));
            assert_eq!(watcher.commits, 1);
            assert_eq!(
                watcher.added.into_iter().collect::<BTreeSet<_>>(),
                BTreeSet::from([
                    (canonical.clone(), RecursiveMode::NonRecursive),
                    (canonical.join("agents"), RecursiveMode::Recursive),
                    (canonical.join("project"), RecursiveMode::Recursive),
                    (canonical.join("other"), RecursiveMode::NonRecursive),
                    (canonical.join("other/skills"), RecursiveMode::Recursive),
                ])
            );
        }
    }

    #[test]
    fn windows_overlapping_grants_register_each_path_once_with_recursive_precedence() {
        let temp = tempfile::tempdir().unwrap();
        let mut roots = BTreeMap::new();
        for (name, only) in [
            (
                "root",
                Some(vec!["agents".into(), "agents/deep/file.md".into()]),
            ),
            ("root/agents", Some(vec!["deep".into()])),
            ("root/agents/deep", None),
            ("root", Some(vec!["other".into()])),
        ] {
            let path = temp.path().join(name);
            std::fs::create_dir_all(&path).unwrap();
            let id = LibraryRootId::new();
            roots.insert(
                id,
                Arc::new(
                    Root::new(RootSpec {
                        root_id: id,
                        path,
                        label: "Test".into(),
                        kind: RootKind::Project,
                        writable: true,
                        project_id: None,
                        harness: None,
                        only,
                    })
                    .unwrap(),
                ),
            );
        }
        let canonical = ports::library::canonical(&temp.path().join("root")).unwrap();
        std::fs::create_dir(canonical.join("other")).unwrap();
        std::fs::write(canonical.join("agents/deep/file.md"), "fixture").unwrap();
        let plan = windows_watch_groups(&roots);
        assert!(plan.complete);
        let registered: Vec<_> = plan.groups.into_iter().flatten().collect();
        let expected = BTreeSet::from([
            (canonical.clone(), RecursiveMode::NonRecursive),
            (canonical.join("agents"), RecursiveMode::Recursive),
            (canonical.join("agents/deep"), RecursiveMode::Recursive),
            (canonical.join("other"), RecursiveMode::Recursive),
        ]);
        // Check the raw length first: collecting straight into a set would
        // conceal duplicate registrations and notify's leaked native handles.
        assert_eq!(registered.len(), expected.len());
        assert_eq!(registered.into_iter().collect::<BTreeSet<_>>(), expected);
        if cfg!(windows) {
            let mut watcher = FakeWatcher::default();
            assert!(register_paths(&mut watcher, &roots));
            assert_eq!(watcher.added.len(), expected.len());
            assert_eq!(watcher.added.into_iter().collect::<BTreeSet<_>>(), expected);
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_failed_grant_does_not_suppress_a_later_shared_root_watch() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("root");
        std::fs::create_dir_all(path.join("agents")).unwrap();
        std::fs::create_dir_all(path.join("skills")).unwrap();
        let mut ids = [LibraryRootId::new(), LibraryRootId::new()];
        ids.sort();
        let mut roots = BTreeMap::new();
        for (id, path, only) in [
            (
                ids[0],
                path.clone(),
                Some(vec!["agents".into(), "skills".into()]),
            ),
            (ids[1], path.join("skills"), None),
        ] {
            roots.insert(
                id,
                Arc::new(
                    Root::new(RootSpec {
                        root_id: id,
                        path,
                        label: "Test".into(),
                        kind: RootKind::Project,
                        writable: true,
                        project_id: None,
                        harness: None,
                        only,
                    })
                    .unwrap(),
                ),
            );
        }
        let canonical = roots[&ids[0]].canonical.clone();
        let mut watcher = FakeWatcher {
            fail: Some(canonical.join("agents")),
            ..FakeWatcher::default()
        };
        assert!(!register_paths(&mut watcher, &roots));
        assert_eq!(watcher.commits, 1);
        assert_eq!(
            watcher.added,
            vec![
                (canonical.clone(), RecursiveMode::NonRecursive),
                (canonical.join("skills"), RecursiveMode::Recursive),
            ]
        );
    }

    #[test]
    fn windows_failed_watch_repair_is_bounded_under_continuous_events() {
        let now = std::time::Instant::now();
        assert!(repair_due(None, now));
        for elapsed in [
            Duration::ZERO,
            Duration::from_millis(80),
            Duration::from_secs(59),
        ] {
            assert!(!repair_due(Some(now), now + elapsed));
        }
        assert!(repair_due(Some(now), now + SWEEP_EVERY));
        assert!(repair_due(Some(now), now + SWEEP_EVERY * 2));
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn windows_reported_watch_error_re_registers_without_subscribers() {
        use crate::{FsLibrary, LibraryConfig};
        use ports::{clock::SystemClock, library::LibraryFs};

        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("root");
        std::fs::create_dir_all(&path).unwrap();
        let fs = FsLibrary::new(
            LibraryConfig::new(temp.path().join("data"), temp.path().join("home")),
            Arc::new(SystemClock),
        )
        .unwrap();
        let id = LibraryRootId::new();
        fs.set_roots(vec![RootSpec {
            root_id: id,
            path: path.clone(),
            label: "Test".into(),
            kind: RootKind::Project,
            writable: true,
            project_id: None,
            harness: None,
            only: None,
        }])
        .await
        .unwrap();
        assert_eq!(fs.core.events.receiver_count(), 0);
        drop(lock(&fs.core.state).watcher.take());
        assert!(lock(&fs.core.state).watcher.is_none());
        // notify 8.2's Windows backend currently never emits runtime Err; inject
        // the public callback shape so any future reported failure is covered.
        fs.core
            .watch_queue
            .send(Err(
                notify::Error::generic("authored watch failure").add_path(path.clone())
            ))
            .unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            while lock(&fs.core.state).watcher.is_none() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("reported errors must restart watching without a subscriber");
        assert!(
            fs.rescan_required(),
            "repair must not acknowledge a missing snapshot"
        );
        let mut events = fs.changes();
        std::fs::write(path.join("after.md"), "after repair").unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let event = events.recv().await.unwrap();
                if event.path == "after.md" {
                    assert_eq!(event.root_id, id);
                    assert!(event.revision.is_some());
                    break;
                }
            }
        })
        .await
        .expect("the replacement watcher must report subsequent outside edits");
    }

    #[test]
    fn windows_event_filter_retains_ancestor_moves_and_deletions_for_limited_paths() {
        use notify::event::{ModifyKind, RemoveKind, RenameMode};

        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        // Neither the file-only grant nor the deeper subtree needs to exist.
        for (grant, ancestors) in [
            ("agents/settings.md", vec!["agents"]),
            ("agents/missing/subdir", vec!["agents", "agents/missing"]),
        ] {
            let scopes = [WatchScope {
                path: root.clone(),
                only: Some(vec![grant.into()]),
            }];
            for kind in [
                notify::EventKind::Remove(RemoveKind::Folder),
                notify::EventKind::Modify(ModifyKind::Name(RenameMode::From)),
            ] {
                let expected: Vec<_> = ancestors.iter().map(|path| root.join(path)).collect();
                let mut event = notify::Event::new(kind);
                event.paths = expected
                    .iter()
                    .cloned()
                    .chain([root.join("agents/sibling"), root.join("agentsx")])
                    .collect();
                assert!(retain_granted_paths(&mut event, &scopes));
                assert_eq!(event.paths, expected);
            }
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_recursive_watch_does_not_follow_junctions_or_expose_ungranted_names() {
        use std::{os::windows::process::CommandExt, process::Command, time::Instant};

        let temp = tempfile::tempdir().unwrap();
        let root_path = temp.path().join("root");
        let outside = temp.path().join("outside");
        let shared_prefix = temp.path().join("root-extra");
        for path in [
            root_path.join("agents"),
            root_path.join("agentsx"),
            outside.clone(),
            shared_prefix.clone(),
        ] {
            std::fs::create_dir_all(path).unwrap();
        }
        let junction = root_path.join("agents").join("escape");
        // A junction needs no symlink privilege. Failure is a test failure,
        // never a reason to silently skip the containment fixture.
        let output = Command::new("cmd.exe")
            .args(["/d", "/c", "mklink", "/J"])
            .arg(&junction)
            .arg(&outside)
            .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "could not create test junction: {output:?}"
        );
        assert!(crate::path::is_link(
            &std::fs::symlink_metadata(&junction).unwrap()
        ));

        let id = LibraryRootId::new();
        let root = Arc::new(
            Root::new(RootSpec {
                root_id: id,
                path: root_path,
                label: "Test".into(),
                kind: RootKind::Project,
                writable: true,
                project_id: None,
                harness: None,
                // Exercise a full-access recursive native watch. Its raw stream
                // deliberately exceeds the limited scopes below, so every
                // original sibling-filter and junction assertion stays live.
                only: None,
            })
            .unwrap(),
        );
        let canonical = root.canonical.clone();
        let roots = BTreeMap::from([(id, root)]);
        let scopes = [WatchScope {
            path: canonical.clone(),
            only: Some(vec!["agents".into()]),
        }];
        let ready = canonical.join("agents/ready.md");
        let after = canonical.join("agents/after.md");
        let junction = canonical.join("agents").join("escape");
        let inner_sibling = canonical.join("agentsx/denied-name.md");
        let permitted = [
            canonical.clone(),
            canonical.join("agents"),
            junction.clone(),
            ready.clone(),
            after.clone(),
        ];

        // Observe raw events as well as the production filter. Otherwise a
        // later refused read could conceal that the native backend followed
        // the junction and exposed outside filenames to the event queue.
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut watcher = RecommendedWatcher::new(
            move |event| {
                let _ = sender.send(event);
            },
            notify::Config::default().with_follow_symlinks(false),
        )
        .unwrap();
        assert!(register_paths(&mut watcher, &roots));
        let inspect = |mut event: notify::Event| {
            assert!(
                !event.need_rescan(),
                "fixture requires a complete native stream"
            );
            for path in &event.paths {
                assert!(path.starts_with(&canonical), "outside-root event: {path:?}");
                assert!(
                    !path.starts_with(&junction) || *path == junction,
                    "native watcher followed the junction: {path:?}"
                );
            }
            let saw_inner_sibling = event.paths.contains(&inner_sibling);
            let saw_ready = event.paths.contains(&ready);
            let saw_after = event.paths.contains(&after);
            if retain_granted_paths(&mut event, &scopes) {
                for path in &event.paths {
                    assert!(
                        permitted.contains(path),
                        "ungranted filename reached the queue: {path:?}"
                    );
                }
            } else {
                assert!(event.paths.is_empty());
            }
            (saw_ready, saw_after, saw_inner_sibling)
        };
        std::fs::write(&ready, "watcher ready").unwrap();
        let ready_deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let event = receiver
                .recv_timeout(ready_deadline.saturating_duration_since(Instant::now()))
                .expect("native watcher did not announce the readiness marker")
                .expect("native watcher failed");
            if inspect(event).0 {
                break;
            }
        }

        std::fs::write(outside.join("outside-name.md"), "private").unwrap();
        std::fs::write(shared_prefix.join("shared-prefix-name.md"), "private").unwrap();
        std::fs::write(junction.join("through-junction-name.md"), "private").unwrap();
        std::fs::write(&inner_sibling, "private").unwrap();
        std::fs::write(&after, "watcher still ready").unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut drain_until = None;
        let mut saw_inner_sibling = false;
        loop {
            let until = drain_until.unwrap_or(deadline);
            match receiver.recv_timeout(until.saturating_duration_since(Instant::now())) {
                Ok(event) => {
                    let (_, saw_after, saw_sibling) =
                        inspect(event.expect("native watcher failed"));
                    saw_inner_sibling |= saw_sibling;
                    if saw_after && drain_until.is_none() {
                        // A fixed post-marker window cannot be extended by
                        // more events, but catches queued late notifications.
                        drain_until = Some(Instant::now() + Duration::from_millis(500));
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => break,
                Err(error) => panic!("native watcher disconnected: {error}"),
            }
        }
        assert!(
            drain_until.is_some(),
            "native watcher did not announce the final marker"
        );
        assert!(
            saw_inner_sibling,
            "fixture must exercise filtering of real recursive sibling events"
        );
        drop(watcher);
        std::fs::remove_dir(&junction).unwrap();
    }

    #[test]
    fn windows_event_filter_keeps_grants_and_rescans_but_discards_siblings() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        let scopes = [
            WatchScope {
                path: root.clone(),
                only: Some(vec!["agents".into()]),
            },
            WatchScope {
                path: root.join("nested"),
                only: Some(vec!["note.md".into()]),
            },
        ];
        let mut event = notify::Event::new(notify::EventKind::Any);
        event.paths = [
            "agents/note.md",
            "agentsx/note.md",
            "sibling/note.md",
            "nested/note.md",
            "nested/private.md",
            "agents/../private.md",
            "agents/.ophio-private",
        ]
        .into_iter()
        .map(|path| root.join(path))
        .chain([
            temp.path().join("outside/note.md"),
            temp.path().join("root-extra/agents/note.md"),
        ])
        .collect();
        assert!(retain_granted_paths(&mut event, &scopes));
        assert_eq!(
            event.paths,
            [root.join("agents/note.md"), root.join("nested/note.md")]
        );

        let mut outside =
            notify::Event::new(notify::EventKind::Any).add_path(root.join("sibling/note.md"));
        assert!(!retain_granted_paths(&mut outside, &scopes));
        assert!(outside.paths.is_empty());
        let mut rescan = outside.set_flag(notify::event::Flag::Rescan);
        assert!(retain_granted_paths(&mut rescan, &scopes));
        assert!(rescan.need_rescan());
        let mut root_change = notify::Event::new(notify::EventKind::Any).add_path(root.clone());
        assert!(retain_granted_paths(&mut root_change, &scopes));
        assert_eq!(root_change.paths, [root]);
    }
}
