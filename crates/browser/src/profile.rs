// SPDX-License-Identifier: Apache-2.0
//! Every voice-control window gets a fresh profile under `voice-browser`. Its
//! cookies and sign-ins stay on disk only while a browser process uses them.
use std::{
    path::{Path, PathBuf},
    sync::{
        Weak,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

/// Chromium helpers, such as the network service that holds the cookie
/// store, can outlive the main process by a few seconds.
const HELPERS_EXIT: Duration = Duration::from_secs(30);
/// How often a profile a helper still used after that wait is checked again.
const HELPERS_RECHECK: Duration = Duration::from_secs(30);

pub(crate) fn directory(data_dir: &Path) -> PathBuf {
    data_dir.join("voice-browser")
}

/// Removes the profiles of windows that closed while Ophio was not watching
/// them. A profile a live process still uses is kept.
pub async fn sweep(data_dir: &Path) {
    let Ok(mut entries) = tokio::fs::read_dir(directory(data_dir)).await else {
        return;
    };
    let mut profiles = Vec::new();
    while let Ok(Some(entry)) = entries.next_entry().await {
        if entry.file_type().await.is_ok_and(|t| t.is_dir()) {
            profiles.push(entry.path());
        }
    }
    remove_unused(profiles).await;
}

/// Removes a closed window's `profile` once its browser's processes have all
/// exited, then marks the window `closed`; a `keep`ed profile is left where
/// it is. A helper that outlives the wait is checked on for as long as the
/// window is kept.
pub(crate) async fn close_when_unused(profile: PathBuf, keep: bool, closed: Weak<AtomicBool>) {
    close_within(profile, keep, closed, HELPERS_EXIT, HELPERS_RECHECK).await
}

async fn close_within(
    profile: PathBuf,
    keep: bool,
    closed: Weak<AtomicBool>,
    wait: Duration,
    again: Duration,
) {
    let mut gone = settle_within(profile.clone(), keep, wait).await;
    while !gone {
        tokio::time::sleep(again).await;
        // A window nobody keeps leaves its profile to the next startup's sweep.
        if closed.strong_count() == 0 {
            return;
        }
        gone = settled(profile.clone(), keep).await;
    }
    if let Some(closed) = closed.upgrade() {
        closed.store(true, Ordering::Release);
    }
}

/// Removes `profile` once its browser's processes have all exited, and says
/// whether it went.
pub(crate) async fn remove_when_unused(profile: PathBuf) -> bool {
    settle_within(profile, false, HELPERS_EXIT).await
}

/// Waits up to `wait` for no process to use `profile`, removing it then
/// unless it is `keep`ed, and says whether that happened.
async fn settle_within(profile: PathBuf, keep: bool, wait: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + wait;
    // What is still in use when the wait ends is left for the next sweep.
    loop {
        if settled(profile.clone(), keep).await {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// Whether no process uses `profile`, which is removed then unless it is
/// `keep`ed.
async fn settled(profile: PathBuf, keep: bool) -> bool {
    if keep {
        return tokio::task::spawn_blocking(move || !unused::in_use(&profile))
            .await
            .unwrap_or(false);
    }
    remove_unused(vec![profile]).await.is_empty()
}

/// Removes the profiles no process uses and returns those it kept.
async fn remove_unused(profiles: Vec<PathBuf>) -> Vec<PathBuf> {
    let checked = profiles.clone();
    tokio::task::spawn_blocking(move || unused::remove(profiles))
        .await
        .unwrap_or(checked)
}

#[cfg(target_os = "linux")]
mod unused {
    use std::{
        os::unix::ffi::OsStrExt,
        path::{Path, PathBuf},
    };

    pub(super) fn remove(profiles: Vec<PathBuf>) -> Vec<PathBuf> {
        let (mut kept, idle): (Vec<_>, Vec<_>) = profiles
            .into_iter()
            .filter(|profile| profile.exists())
            .partition(|profile| singleton_alive(profile));
        let used = used_by_a_process(&idle);
        for (profile, used) in idle.into_iter().zip(used) {
            if used || std::fs::remove_dir_all(&profile).is_err() {
                kept.push(profile);
            }
        }
        kept
    }

    pub(super) fn in_use(profile: &Path) -> bool {
        profile.exists()
            && (singleton_alive(profile) || used_by_a_process(&[profile.to_path_buf()])[0])
    }

    /// Chromium's `SingletonLock` links to `<host>-<pid>` while a browser runs
    /// the profile. One naming another computer is kept, as it can't be checked.
    fn singleton_alive(profile: &Path) -> bool {
        let Ok(target) = std::fs::read_link(profile.join("SingletonLock")) else {
            return false;
        };
        let target = target.to_string_lossy();
        let Some((host, pid)) = target.rsplit_once('-') else {
            return true;
        };
        let this_host = std::fs::read_to_string("/proc/sys/kernel/hostname").unwrap_or_default();
        host != this_host.trim() || pid.parse::<u32>().map_or(true, alive)
    }

    /// A browser that crashed stays a zombie until Ophio reaps it; a zombie
    /// holds no files.
    fn alive(pid: u32) -> bool {
        std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
            stat.rsplit_once(')')
                .is_some_and(|(_, fields)| !fields.trim_start().starts_with(['Z', 'X']))
        })
    }

    /// For each profile, whether any process names it on its command line,
    /// works in it or holds a file in it. Chromium passes the profile to every
    /// helper, and helpers keep files such as the cookie store open.
    fn used_by_a_process(profiles: &[PathBuf]) -> Vec<bool> {
        if profiles.is_empty() {
            return Vec::new();
        }
        let Ok(processes) = std::fs::read_dir("/proc") else {
            return vec![true; profiles.len()];
        };
        let canonical: Vec<PathBuf> = profiles
            .iter()
            .map(|profile| profile.canonicalize().unwrap_or_else(|_| profile.clone()))
            .collect();
        let mut used = vec![false; profiles.len()];
        for process in processes.flatten() {
            if !process
                .file_name()
                .as_bytes()
                .iter()
                .all(u8::is_ascii_digit)
            {
                continue;
            }
            let dir = process.path();
            let cmdline = std::fs::read(dir.join("cmdline")).unwrap_or_default();
            let mut open: Vec<PathBuf> = std::fs::read_link(dir.join("cwd")).into_iter().collect();
            if let Ok(files) = std::fs::read_dir(dir.join("fd")) {
                open.extend(
                    files
                        .flatten()
                        .filter_map(|file| std::fs::read_link(file.path()).ok()),
                );
            }
            for ((profile, canonical), used) in profiles.iter().zip(&canonical).zip(&mut used) {
                *used = *used
                    || [profile, canonical]
                        .iter()
                        .any(|path| names(&cmdline, path))
                    || open.iter().any(|path| path.starts_with(canonical));
            }
        }
        used
    }

    fn names(cmdline: &[u8], path: &Path) -> bool {
        let path = path.as_os_str().as_bytes();
        cmdline.windows(path.len()).any(|window| window == path)
    }
}

#[cfg(windows)]
mod unused {
    use std::path::{Path, PathBuf};

    /// A profile that can be moved aside and back is one no process uses.
    pub(super) fn in_use(profile: &Path) -> bool {
        let mut aside = profile.as_os_str().to_owned();
        aside.push(".checking");
        if std::fs::rename(profile, &aside).is_err() {
            return profile.exists();
        }
        std::fs::rename(&aside, profile).is_err()
    }

    /// Windows refuses to rename a folder while a process holds a file in it,
    /// so a profile that can be moved aside is one no process uses.
    pub(super) fn remove(profiles: Vec<PathBuf>) -> Vec<PathBuf> {
        profiles
            .into_iter()
            .filter(|profile| profile.exists())
            .filter(|profile| {
                let mut aside = profile.as_os_str().to_owned();
                aside.push(".removing");
                std::fs::rename(profile, &aside).is_err() || std::fs::remove_dir_all(aside).is_err()
            })
            .collect()
    }
}

/// Without a way to tell which processes use a profile, it is kept.
#[cfg(not(any(target_os = "linux", windows)))]
mod unused {
    use std::path::{Path, PathBuf};

    /// Once the browser itself has exited, a kept profile is taken as free.
    pub(super) fn in_use(_: &Path) -> bool {
        false
    }

    pub(super) fn remove(profiles: Vec<PathBuf>) -> Vec<PathBuf> {
        profiles.into_iter().filter(|p| p.exists()).collect()
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::{
        io::{BufRead, BufReader},
        os::unix::fs::symlink,
        process::{Command, Stdio},
        sync::Arc,
    };

    fn host() -> String {
        std::fs::read_to_string("/proc/sys/kernel/hostname")
            .unwrap()
            .trim()
            .to_owned()
    }

    fn profile(data: &Path, name: &str, lock: Option<String>) -> PathBuf {
        let profile = directory(data).join(name);
        std::fs::create_dir_all(profile.join("Default")).unwrap();
        std::fs::write(profile.join("Default/Cookies"), "signed in").unwrap();
        if let Some(lock) = lock {
            symlink(lock, profile.join("SingletonLock")).unwrap();
        }
        profile
    }

    #[tokio::test]
    async fn startup_removes_only_profiles_no_live_process_uses() {
        let data = tempfile::tempdir().unwrap();
        let mut exited = Command::new("true").spawn().unwrap();
        let gone = exited.id();
        exited.wait().unwrap();
        // A crashed browser nobody has reaped yet.
        let mut zombie = Command::new("true").spawn().unwrap();
        let stat = format!("/proc/{}/stat", zombie.id());
        while !std::fs::read_to_string(&stat).unwrap().contains(") Z") {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        profile(data.path(), "closed", None);
        profile(data.path(), "exited", Some(format!("{}-{gone}", host())));
        profile(
            data.path(),
            "crashed",
            Some(format!("{}-{}", host(), zombie.id())),
        );
        profile(
            data.path(),
            "running",
            Some(format!("{}-{}", host(), std::process::id())),
        );
        profile(data.path(), "elsewhere", Some("another-computer-1".into()));
        let open = profile(data.path(), "open", None);
        let _cookies = std::fs::File::open(open.join("Default/Cookies")).unwrap();
        let named = profile(data.path(), "named", None);
        let mut helper = Command::new("sh")
            .args(["-c", "read line", "helper"])
            .arg(format!("--user-data-dir={}", named.display()))
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();

        sweep(data.path()).await;

        let mut left: Vec<_> = std::fs::read_dir(directory(data.path()))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        left.sort();
        drop(helper.stdin.take());
        helper.wait().unwrap();
        zombie.wait().unwrap();
        assert_eq!(left, ["elsewhere", "named", "open", "running"]);
    }

    #[tokio::test]
    async fn a_closed_window_s_profile_goes_once_its_helpers_exit() {
        let data = tempfile::tempdir().unwrap();
        let profile = profile(data.path(), "window", None);
        // Only an open file ties this helper to the profile.
        let mut helper = Command::new("sh")
            .args(["-c", "exec 3<Default/Cookies; cd /; echo ready; sleep 1"])
            .current_dir(&profile)
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut ready = String::new();
        BufReader::new(helper.stdout.take().unwrap())
            .read_line(&mut ready)
            .unwrap();

        assert_eq!(
            remove_unused(vec![profile.clone()]).await,
            std::slice::from_ref(&profile)
        );
        assert!(remove_when_unused(profile.clone()).await);

        assert!(helper.try_wait().unwrap().is_some());
        assert!(!profile.exists());
    }

    #[tokio::test]
    async fn a_window_whose_helper_outlives_the_wait_closes_when_it_exits() {
        let data = tempfile::tempdir().unwrap();
        let profile = profile(data.path(), "window", None);
        let mut helper = Command::new("sh")
            .args(["-c", "exec 3<Default/Cookies; cd /; echo ready; sleep 1"])
            .current_dir(&profile)
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut ready = String::new();
        BufReader::new(helper.stdout.take().unwrap())
            .read_line(&mut ready)
            .unwrap();
        let closed = Arc::new(AtomicBool::new(false));
        let short = Duration::from_millis(100);

        tokio::time::timeout(
            Duration::from_secs(10),
            close_within(
                profile.clone(),
                false,
                Arc::downgrade(&closed),
                short,
                short,
            ),
        )
        .await
        .unwrap();

        helper.wait().unwrap();
        assert!(closed.load(Ordering::Acquire));
        assert!(!profile.exists());
    }

    #[tokio::test]
    async fn a_window_nobody_keeps_stops_being_checked() {
        let data = tempfile::tempdir().unwrap();
        let profile = profile(data.path(), "window", None);
        let _cookies = std::fs::File::open(profile.join("Default/Cookies")).unwrap();
        let closed = Arc::new(AtomicBool::new(false));
        let short = Duration::from_millis(100);
        let check = tokio::spawn(close_within(
            profile.clone(),
            false,
            Arc::downgrade(&closed),
            short,
            short,
        ));

        drop(closed);

        tokio::time::timeout(Duration::from_secs(10), check)
            .await
            .unwrap()
            .unwrap();
        assert!(profile.exists());
    }
}
