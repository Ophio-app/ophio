// SPDX-License-Identifier: Apache-2.0
//! A cgroup v2 of the run's own, made inside the companion's cgroup.
use crate::{END_WITHIN, unconfirmed};
use ports::HarnessError;
use std::{
    collections::BTreeSet,
    fs::File,
    io,
    path::{Path, PathBuf},
    sync::{
        Mutex, PoisonError,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

static NEXT: AtomicU64 = AtomicU64::new(0);
/// Released cgroups something still ran in, and those below one that a try
/// didn't reach, removed once empty.
static LEFT: Mutex<BTreeSet<PathBuf>> = Mutex::new(BTreeSet::new());
/// Most cgroups looked at below one being removed, each time it is tried.
const MOST_NESTED: usize = 1024;

#[derive(Debug)]
pub(crate) struct Cgroup {
    dir: PathBuf,
    /// Opened before any fork, so a new process can move itself in.
    procs: File,
}

impl Cgroup {
    pub(crate) fn limit(&self, limits: crate::ResourceLimits) -> io::Result<()> {
        std::fs::write(self.dir.join("memory.max"), limits.memory_bytes.to_string())?;
        std::fs::write(self.dir.join("memory.swap.max"), "0")?;
        std::fs::write(self.dir.join("pids.max"), limits.processes.to_string())?;
        std::fs::write(
            self.dir.join("cpu.max"),
            format!("{} 100000", u64::from(limits.cpu_millis_per_second) * 100),
        )?;
        Ok(())
    }
    /// The reason is for the person, when the run keeps only its process group.
    pub(crate) fn create() -> Result<Self, String> {
        let own = own().ok_or("this computer doesn’t use cgroup v2")?;
        Self::create_at(&own)
    }
    pub(crate) fn limited(limits: crate::ResourceLimits) -> Result<Self, String> {
        let own = own().ok_or("this computer doesn’t use cgroup v2")?;
        // Domain controllers cannot be enabled below a group holding the supervisor.
        // A delegated unit keeps that process in a named subgroup and jobs beside it.
        let root = if own.file_name().is_some_and(|name| name == "supervisor") {
            let parent = own.parent().ok_or("no delegated resource parent")?;
            let processes = std::fs::read_to_string(parent.join("cgroup.procs"))
                .map_err(|_| "resource parent is unavailable")?;
            if !processes.trim().is_empty() {
                return Err("resource parent must contain no processes".into());
            }
            std::fs::write(parent.join("cgroup.subtree_control"), "+cpu +memory +pids")
                .map_err(|_| "resource controllers are not delegated")?;
            parent
        } else {
            &own
        };
        let group = Self::create_at(root)?;
        group
            .limit(limits)
            .map_err(|_| "resource controllers are not delegated")?;
        Ok(group)
    }
    fn create_at(own: &Path) -> Result<Self, String> {
        sweep(own);
        if !own.join("cgroup.kill").exists() {
            return Err(
                "this Linux kernel can’t end a cgroup at once; that needs 5.14 or later".into(),
            );
        }
        let dir = own.join(format!(
            "{}{}",
            ports::run_cgroup_prefix(std::process::id()),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let refused = |e: io::Error| {
            format!(
                "Øphio can’t make a cgroup for the run in {} ({e})",
                own.display()
            )
        };
        std::fs::create_dir(&dir).map_err(refused)?;
        // Moving a process needs write access to the cgroup it leaves as well.
        let opened = rustix::fs::access(own.join("cgroup.procs"), rustix::fs::Access::WRITE_OK)
            .map_err(io::Error::from)
            .and_then(|()| File::options().write(true).open(dir.join("cgroup.procs")));
        match opened {
            Ok(procs) => Ok(Self { dir, procs }),
            Err(e) => {
                let _ = std::fs::remove_dir(&dir);
                Err(refused(e))
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn dir(&self) -> &Path {
        &self.dir
    }

    /// One that cannot be joined: `procs` refuses every write.
    #[cfg(test)]
    pub(crate) fn unjoinable(dir: PathBuf) -> Self {
        let procs = File::options().write(true).open("/dev/full").unwrap();
        Self { dir, procs }
    }

    /// Have the process `command` starts join the group between fork and exec,
    /// before the program, its loader or anything it starts runs. Should that
    /// fail, the spawn fails and the program never runs. Runs first of the
    /// command's steps between fork and exec that are added after it.
    #[allow(unsafe_code)]
    pub(crate) fn prepare(&self, command: &mut std::process::Command) -> io::Result<()> {
        let procs = self.procs.try_clone()?;
        // SAFETY: between fork and exec the new process only makes one
        // write(2) call, which is async-signal-safe, on a descriptor opened
        // before the fork. Writing 0 moves the writing process itself.
        unsafe {
            std::os::unix::process::CommandExt::pre_exec(command, move || {
                rustix::io::write(&procs, b"0")?;
                Ok(())
            });
        }
        Ok(())
    }

    /// Stop holding it without ending anything inside: it is removed if
    /// empty, and otherwise once what still runs in it has ended.
    pub(crate) fn release(&mut self) {
        let _ = remove(&std::mem::take(&mut self.dir));
    }

    pub(crate) fn has_other_processes(&self, shell: u32) -> io::Result<bool> {
        has_other_processes(&self.dir, shell)
    }

    pub(crate) fn kill(&self) -> Result<(), HarnessError> {
        match std::fs::write(self.dir.join("cgroup.kill"), b"1") {
            // Already ended and removed.
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            result => result.map_err(|e| unconfirmed(format!("Ending the run's cgroup: {e}"))),
        }
    }

    pub(crate) async fn end(&self) -> Result<(), HarnessError> {
        self.kill()?;
        let deadline = tokio::time::Instant::now() + END_WITHIN;
        while populated(&self.dir)? {
            if tokio::time::Instant::now() >= deadline {
                return Err(unconfirmed("The run's cgroup still has processes"));
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        remove(&self.dir).map_err(|e| unconfirmed(format!("Removing the run's cgroup: {e}")))
    }
}

/// Check the whole terminal container before declaring its shell alone.
fn has_other_processes(root: &Path, shell: u32) -> io::Result<bool> {
    scan_processes(root, shell, |dir| {
        std::fs::read_to_string(dir.join("cgroup.procs"))
    })
}

fn scan_processes(
    root: &Path,
    shell: u32,
    mut read_procs: impl FnMut(&Path) -> io::Result<String>,
) -> io::Result<bool> {
    let mut pending = vec![root.to_owned()];
    let mut seen_shell = false;
    let mut visited = 0;
    while let Some(dir) = pending.pop() {
        visited += 1;
        if visited > MOST_NESTED {
            return Err(io::Error::other("too many nested terminal cgroups"));
        }
        for pid in read_procs(&dir)?.lines() {
            let pid = pid
                .parse::<u32>()
                .map_err(|_| io::Error::other("unreadable terminal process identity"))?;
            if pid != shell {
                return Ok(true);
            }
            seen_shell = true;
        }
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                pending.push(entry.path());
            }
        }
    }
    if !seen_shell {
        return Err(io::Error::other("terminal shell no longer in its cgroup"));
    }
    if visited > 1 {
        // A process can move into a group already read. A positive sighting
        // is useful, but a negative traversal is not a consistent snapshot.
        return Err(io::Error::other("nested terminal membership is uncertain"));
    }
    Ok(false)
}

impl Drop for Cgroup {
    /// An ended cgroup is already gone, and a released one no longer held. One
    /// left behind by a cancelled run is killed, and removed once its processes
    /// have left.
    fn drop(&mut self) {
        if !self.dir.exists() || self.kill().is_err() {
            return;
        }
        let dir = std::mem::take(&mut self.dir);
        std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + END_WITHIN;
            while populated(&dir).unwrap_or(true) && std::time::Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
            let _ = remove(&dir);
        });
    }
}

/// Remove the released cgroups that have emptied since, before another is
/// made, so they never use up what the companion may make. Those in `own` that
/// a companion no longer running released, and which have emptied, go too.
fn sweep(own: &Path) {
    let mut left = LEFT.lock().unwrap_or_else(PoisonError::into_inner);
    retry(&mut left);
    let Ok(entries) = std::fs::read_dir(own) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(companion) = name.to_str().and_then(ports::run_cgroup_companion) else {
            continue;
        };
        if companion != std::process::id()
            && !Path::new("/proc").join(companion.to_string()).exists()
            && !left.contains(&entry.path())
        {
            let _ = remove_empty(&entry.path(), &mut left);
        }
    }
}

/// Remove `dir` as [`remove_empty`] does, keeping what is still to go to be
/// tried again before another is made.
fn remove(dir: &Path) -> io::Result<()> {
    let mut left = BTreeSet::new();
    let result = remove_empty(dir, &mut left);
    LEFT.lock()
        .unwrap_or_else(PoisonError::into_inner)
        .append(&mut left);
    result
}

/// Try again each kept in `left`, those furthest down first, so each above
/// them can go in the same try.
fn retry(left: &mut BTreeSet<PathBuf>) {
    for dir in std::mem::take(left).into_iter().rev() {
        let _ = remove_empty(&dir, left);
    }
}

/// Remove `dir` and every empty cgroup below it, those furthest down first.
/// One that something still runs in stays, and so does each above it. At most
/// [`MOST_NESTED`] are looked at each try. What stays goes in `left`, with
/// those below that the try didn't reach, so trying each of those in turn
/// removes the rest however deep it goes.
fn remove_empty(dir: &Path, left: &mut BTreeSet<PathBuf>) -> io::Result<()> {
    let mut found = Vec::new();
    let mut pending = vec![dir.to_owned()];
    while found.len() < MOST_NESTED {
        let Some(group) = pending.pop() else {
            break;
        };
        if let Ok(entries) = std::fs::read_dir(&group) {
            for entry in entries.flatten() {
                if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                    pending.push(entry.path());
                }
            }
        }
        found.push(group);
    }
    // Each is found after the one above it.
    for group in found.iter().skip(1).rev() {
        let _ = std::fs::remove_dir(group);
    }
    left.extend(pending);
    match std::fs::remove_dir(dir) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => {
            left.insert(dir.to_owned());
            Err(e)
        }
        Ok(()) => Ok(()),
    }
}

/// Whether any process is still inside. A removed cgroup has none.
fn populated(dir: &Path) -> Result<bool, HarnessError> {
    match std::fs::read_to_string(dir.join("cgroup.events")) {
        Ok(events) => events
            .lines()
            .find_map(|line| line.strip_prefix("populated "))
            .map(|value| value.trim() != "0")
            .ok_or_else(|| unconfirmed("The run's cgroup doesn’t say whether it is empty")),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(unconfirmed(format!("Reading the run's cgroup: {e}"))),
    }
}

/// The companion's own cgroup directory, from its unified-hierarchy entry and
/// the cgroup2 mount.
fn own() -> Option<PathBuf> {
    let entry = std::fs::read_to_string("/proc/self/cgroup").ok()?;
    let path = entry.lines().find_map(|line| line.strip_prefix("0::"))?;
    let mounts = std::fs::read_to_string("/proc/self/mountinfo").ok()?;
    let mount = cgroup2_mount(&mounts)?;
    let dir = Path::new(mount).join(path.trim_start_matches('/'));
    dir.is_dir().then_some(dir)
}

/// The mount point of a cgroup2 mount of the whole hierarchy.
fn cgroup2_mount(mountinfo: &str) -> Option<&str> {
    mountinfo.lines().find_map(|line| {
        let (mount, filesystem) = line.split_once(" - ")?;
        if filesystem.split(' ').next()? != "cgroup2" {
            return None;
        }
        let mut fields = mount.split(' ').skip(3);
        (fields.next()? == "/").then_some(())?;
        fields.next()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aggregate_limits_include_swap_and_every_descendant() {
        let dir = tempfile::tempdir().unwrap();
        let group = Cgroup::unjoinable(dir.path().join("group"));
        std::fs::create_dir(&group.dir).unwrap();
        group
            .limit(crate::ResourceLimits {
                memory_bytes: 1024,
                cpu_millis_per_second: 4000,
                processes: 256,
            })
            .unwrap();
        for (name, value) in [
            ("memory.max", "1024"),
            ("memory.swap.max", "0"),
            ("pids.max", "256"),
            ("cpu.max", "400000 100000"),
        ] {
            assert_eq!(
                std::fs::read_to_string(group.dir.join(name)).unwrap(),
                value
            );
        }
    }

    #[test]
    fn migration_to_an_already_scanned_group_cannot_prove_app_exit() {
        let root = tempfile::tempdir().unwrap();
        let nested = root.path().join("app");
        std::fs::create_dir(&nested).unwrap();
        std::fs::write(root.path().join("cgroup.procs"), "101\n").unwrap();
        std::fs::write(nested.join("cgroup.procs"), "202\n").unwrap();
        let mut migrated = false;
        let result = scan_processes(root.path(), 101, |dir| {
            if dir == nested {
                // The app remains alive throughout, but moves into the root
                // after it was read and before its old nested group is read.
                std::fs::write(root.path().join("cgroup.procs"), "101\n202\n")?;
                std::fs::write(nested.join("cgroup.procs"), "")?;
                migrated = true;
            }
            std::fs::read_to_string(dir.join("cgroup.procs"))
        });
        assert!(migrated);
        assert!(
            result.is_err(),
            "a negative nested scan is unknown: {result:?}"
        );
    }

    #[test]
    fn a_terminal_shell_alone_is_distinct_from_live_or_detached_children() {
        let root = tempfile::tempdir().unwrap();
        let procs = root.path().join("cgroup.procs");
        std::fs::write(&procs, "101\n").unwrap();
        assert!(!has_other_processes(root.path(), 101).unwrap());

        // cgroup membership does not depend on the app's parent or session.
        std::fs::write(&procs, "101\n202\n").unwrap();
        assert!(has_other_processes(root.path(), 101).unwrap());
        std::fs::write(&procs, "101\n").unwrap();
        let nested = root.path().join("app");
        std::fs::create_dir(&nested).unwrap();
        std::fs::write(nested.join("cgroup.procs"), "202\n").unwrap();
        assert!(has_other_processes(root.path(), 101).unwrap());
        std::fs::write(nested.join("cgroup.procs"), "").unwrap();
        assert!(has_other_processes(root.path(), 101).is_err());

        // Missing or unreadable membership is unknown, never proof of exit.
        std::fs::write(&procs, "").unwrap();
        assert!(has_other_processes(root.path(), 101).is_err());
        std::fs::write(&procs, "101\n").unwrap();
        std::fs::remove_file(nested.join("cgroup.procs")).unwrap();
        assert!(has_other_processes(root.path(), 101).is_err());
        std::fs::write(nested.join("cgroup.procs"), "invalid\n").unwrap();
        assert!(has_other_processes(root.path(), 101).is_err());
    }

    #[test]
    fn the_hierarchy_is_found_where_cgroup2_is_mounted_whole() {
        let mounts = "\
24 1 0:22 / /proc rw,nosuid shared:13 - proc proc rw
30 29 0:26 /sub /mnt/part rw shared:9 - cgroup2 cgroup2 rw
31 24 0:27 / /sys/fs/cgroup rw,nosuid,nodev,noexec shared:10 - cgroup2 cgroup2 rw,nsdelegate
";
        assert_eq!(cgroup2_mount(mounts), Some("/sys/fs/cgroup"));
        assert_eq!(
            cgroup2_mount("25 1 0:23 / /sys/fs/cgroup/memory rw - cgroup cgroup rw,memory"),
            None
        );
    }

    #[test]
    fn removing_a_directory_removes_what_is_below_it_first() {
        // Plain directories stand in for cgroups: both are removed with
        // rmdir, which refuses one that is not empty.
        let root = tempfile::tempdir().unwrap();
        let group = root.path().join("group");
        let mut left = BTreeSet::new();
        std::fs::create_dir_all(group.join("a/b/c")).unwrap();
        std::fs::create_dir_all(group.join("d")).unwrap();
        remove_empty(&group, &mut left).unwrap();
        assert!(!group.exists());
        assert!(left.is_empty());
        // Something still in one keeps it and everything above it.
        std::fs::create_dir_all(group.join("a/b")).unwrap();
        std::fs::create_dir_all(group.join("e")).unwrap();
        std::fs::write(group.join("a/b/running"), b"").unwrap();
        assert!(remove_empty(&group, &mut left).is_err());
        assert!(group.join("a/b").exists());
        assert!(!group.join("e").exists());
        assert_eq!(left, BTreeSet::from([group.clone()]));
        std::fs::remove_file(group.join("a/b/running")).unwrap();
        retry(&mut left);
        assert!(!group.exists());
        assert!(left.is_empty());
        remove_empty(&group, &mut left).unwrap();
    }

    #[test]
    fn more_below_than_one_try_reaches_go_when_tried_again() {
        let root = tempfile::tempdir().unwrap();
        let mut left = BTreeSet::new();
        // Half as deep again as one try reaches, within the longest path.
        let deep = root.path().join("deep");
        let mut deepest = deep.clone();
        for _ in 0..MOST_NESTED + MOST_NESTED / 2 {
            deepest.push("n");
        }
        std::fs::create_dir_all(&deepest).unwrap();
        assert!(remove_empty(&deep, &mut left).is_err());
        assert!(deep.exists());
        assert_eq!(left.len(), 2);
        retry(&mut left);
        assert!(!deep.exists());
        assert!(left.is_empty());
        // Wider than one try reaches.
        let wide = root.path().join("wide");
        for n in 0..MOST_NESTED + 10 {
            std::fs::create_dir_all(wide.join(n.to_string())).unwrap();
        }
        assert!(remove_empty(&wide, &mut left).is_err());
        assert!(wide.exists());
        retry(&mut left);
        assert!(!wide.exists());
        assert!(left.is_empty());
    }
}
