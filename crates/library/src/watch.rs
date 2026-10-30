// SPDX-License-Identifier: Apache-2.0
use crate::{
    Core, lock,
    path::{Root, normalize},
};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use ports::library::LibraryError;
use protocol::ids::LibraryRootId;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, atomic::Ordering},
    time::Duration,
};

pub(crate) fn watcher(
    core: &Core,
    roots: &BTreeMap<LibraryRootId, Arc<Root>>,
) -> Result<RecommendedWatcher, LibraryError> {
    let sender = core.watch_queue.clone();
    let overflow = core.rescan_required.clone();
    let mut watcher = RecommendedWatcher::new(
        move |event| {
            if sender.try_send(event).is_err() {
                overflow.store(true, Ordering::Release);
            }
        },
        notify::Config::default().with_follow_symlinks(false),
    )
    .map_err(|_| std::io::Error::other("could not initialize Library watcher"))?;
    for root in roots.values() {
        // A folder that cannot be watched (unreadable, or past the system's
        // watch limit) stops the walk there. Outside changes in the rest of
        // the tree go unannounced; the root still reads and saves, and the
        // companion still starts.
        if watched(root)
            .into_iter()
            .any(|(path, mode)| watcher.watch(&path, mode).is_err())
        {
            tracing::warn!(
                "Library root is only partly watched; some outside changes will not be announced"
            );
        }
    }
    Ok(watcher)
}

/// What to watch for a root: all of it, or for a root limited to some paths,
/// only those that exist, so a busy folder beside them never wakes this. A
/// file is watched through its folder, as an editor may replace it. One made
/// later is watched once the roots are set again.
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
            loop {
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
                                    core.rescan_required.store(true, Ordering::Release);
                                }
                                // Reads generate access notifications on some backends.
                                if event.kind.is_access() {
                                    return;
                                }
                                for path in event.paths {
                                    if paths.len() < 4096 {
                                        paths.insert(path);
                                    } else {
                                        core.rescan_required.store(true, Ordering::Release);
                                    }
                                }
                            }
                            Err(_) => core.rescan_required.store(true, Ordering::Release),
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
                            Err(_) => core.rescan_required.store(true, Ordering::Release),
                        }
                    }
                }
            }
        })?;
    Ok(())
}
