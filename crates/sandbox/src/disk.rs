// SPDX-License-Identifier: Apache-2.0
use std::{
    collections::HashSet,
    ffi::OsString,
    fs::{File, OpenOptions},
    io,
    os::{
        fd::AsRawFd,
        unix::fs::{MetadataExt, OpenOptionsExt},
    },
    path::{Path, PathBuf},
    sync::Mutex,
};

/// A sampled aggregate budget for host writable binds, plus kernel size caps
/// on anonymous scratch mounts. Check throughout execution and after tree cleanup.
#[derive(Debug)]
pub struct DiskBudget {
    bytes: u64,
    entries: u64,
    mounts: Mutex<Mounts>,
}
#[derive(Debug, Default)]
struct Mounts {
    host: Vec<PathBuf>,
    scratch_paths: Vec<PathBuf>,
    scratch: Vec<File>,
    volumes: Vec<(File, u64, u64)>,
}
impl DiskBudget {
    pub fn new(bytes: u64, entries: u64) -> io::Result<Self> {
        if bytes == 0 || entries == 0 {
            return Err(io::Error::other("disk limits must be positive"));
        }
        Ok(Self {
            bytes,
            entries,
            mounts: Mutex::new(Mounts::default()),
        })
    }
    pub(crate) fn configure(&self, args: &mut Vec<OsString>) -> io::Result<()> {
        // /dev itself is sealed; its shared-memory directory is separate scratch.
        args.extend([
            "--remount-ro".into(),
            "/dev".into(),
            "--tmpfs".into(),
            "/dev/shm".into(),
            "--disable-userns".into(),
        ]);
        let mut mounts = self.mounts.lock().unwrap();
        if !mounts.scratch_paths.is_empty() {
            return Err(io::Error::other("disk budget is single-use"));
        }
        for pair in args.windows(2) {
            if pair[0] == "--tmpfs" {
                mounts.scratch_paths.push(PathBuf::from(&pair[1]));
            }
            if pair[0] == "--bind" {
                mounts.host.push(PathBuf::from(&pair[1]));
            }
        }
        mounts.host.sort();
        let mut roots: Vec<PathBuf> = vec![];
        for path in mounts.host.drain(..) {
            if !roots.iter().any(|p| path.starts_with(p)) {
                roots.push(path);
            }
        }
        mounts.host = roots;
        // Also watch volume consumption: unlinked-but-open files evade directory walks.
        let mut devices = HashSet::new();
        for path in mounts.host.clone() {
            let file = File::open(&path)?;
            if devices.insert(file.metadata()?.dev()) {
                let (bytes, entries) = available(&file)?;
                if bytes < self.bytes.saturating_add(512 * 1024 * 1024)
                    || entries < self.entries.saturating_add(1024)
                {
                    return Err(io::Error::other("not enough free space for disk budget"));
                }
                mounts.volumes.push((file, bytes, entries));
            }
        }
        let per_mount = (self.bytes / mounts.scratch_paths.len() as u64 / 4096) * 4096;
        if per_mount < 4096 {
            return Err(io::Error::other(
                "disk budget is too small for scratch mounts",
            ));
        }
        let original = std::mem::take(args);
        for arg in original {
            if arg == "--tmpfs" {
                args.extend(["--size".into(), per_mount.to_string().into()]);
            }
            args.push(arg);
        }
        Ok(())
    }
    pub(crate) fn attach(&self, root: &File) -> io::Result<()> {
        let mut mounts = self.mounts.lock().unwrap();
        let host_devices = mounts
            .host
            .iter()
            .map(|p| p.metadata().map(|m| m.dev()))
            .collect::<io::Result<HashSet<_>>>()?;
        let mut seen = HashSet::new();
        for path in mounts.scratch_paths.clone() {
            let relative = path.strip_prefix("/").map_err(io::Error::other)?;
            let file = File::open(
                PathBuf::from(format!("/proc/self/fd/{}", root.as_raw_fd())).join(relative),
            )?;
            let device = file.metadata()?.dev();
            // Later binds can cover a tmpfs; never charge an entire host volume
            // or count the same anonymous mount through several destinations.
            if !host_devices.contains(&device) && is_tmpfs(&file)? && seen.insert(device) {
                mounts.scratch.push(file);
            }
        }
        Ok(())
    }
    /// Errors mean the budget cannot be verified and must stop untrusted work.
    pub fn exceeded(&self) -> io::Result<bool> {
        let mounts = self.mounts.lock().unwrap();
        let mut usage = Usage {
            bytes: 0,
            entries: 0,
            seen: HashSet::new(),
        };
        for file in &mounts.scratch {
            let (free_bytes, free_entries) = available(file)?;
            if free_bytes < 4096 || free_entries == 0 {
                return Ok(true);
            }
            let (bytes, entries) = used(file)?;
            usage.bytes = usage.bytes.saturating_add(bytes);
            usage.entries = usage.entries.saturating_add(entries);
        }
        for root in &mounts.host {
            if self.walk(root, 0, &mut usage)? {
                return Ok(true);
            }
        }
        for (file, bytes, entries) in &mounts.volumes {
            let (now_bytes, now_entries) = available(file)?;
            if now_bytes < 512 * 1024 * 1024
                || now_entries < 1024
                || bytes.saturating_sub(now_bytes) >= self.bytes
                || entries.saturating_sub(now_entries) >= self.entries
            {
                return Ok(true);
            }
        }
        Ok(self.full(&usage))
    }
    fn full(&self, usage: &Usage) -> bool {
        usage.bytes >= self.bytes || usage.entries >= self.entries
    }
    fn walk(&self, path: &Path, depth: usize, usage: &mut Usage) -> io::Result<bool> {
        if depth > 128 {
            return Ok(true);
        }
        let metadata = match std::fs::symlink_metadata(path) {
            Ok(m) => m,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(e),
        };
        usage.entries = usage.entries.saturating_add(1);
        if self.full(usage) {
            return Ok(true);
        }
        if !usage.seen.insert((metadata.dev(), metadata.ino())) {
            return Ok(false);
        }
        usage.bytes = usage
            .bytes
            .saturating_add(metadata.len().max(metadata.blocks().saturating_mul(512)));
        if self.full(usage) {
            return Ok(true);
        }
        if metadata.is_dir() {
            // Pin each directory without following a symlink swapped in by the agent.
            let dir = match OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(path)
            {
                Ok(dir) => dir,
                Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
                Err(e) => return Err(e),
            };
            for entry in std::fs::read_dir(format!("/proc/self/fd/{}", dir.as_raw_fd()))? {
                if self.walk(&entry?.path(), depth + 1, usage)? {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }
}
struct Usage {
    bytes: u64,
    entries: u64,
    seen: HashSet<(u64, u64)>,
}
#[allow(unsafe_code)]
fn stat(file: &File) -> io::Result<libc::statvfs> {
    let mut stat = std::mem::MaybeUninit::uninit();
    // SAFETY: the descriptor is live and stat points to enough writable memory.
    if unsafe { libc::fstatvfs(file.as_raw_fd(), stat.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a successful fstatvfs initialized the structure.
    Ok(unsafe { stat.assume_init() })
}
#[allow(unsafe_code)]
fn is_tmpfs(file: &File) -> io::Result<bool> {
    let mut stat = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: the descriptor is live and stat points to enough writable memory.
    if unsafe { libc::fstatfs(file.as_raw_fd(), stat.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a successful fstatfs initialized the structure.
    Ok(unsafe { stat.assume_init() }.f_type == libc::TMPFS_MAGIC)
}
fn available(file: &File) -> io::Result<(u64, u64)> {
    let s = stat(file)?;
    Ok((
        s.f_bavail.saturating_mul(s.f_frsize),
        if s.f_files == 0 { u64::MAX } else { s.f_favail },
    ))
}
fn used(file: &File) -> io::Result<(u64, u64)> {
    let s = stat(file)?;
    Ok((
        s.f_blocks
            .saturating_sub(s.f_bfree)
            .saturating_mul(s.f_frsize),
        s.f_files.saturating_sub(s.f_ffree),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn budget(root: &Path, bytes: u64, entries: u64) -> DiskBudget {
        let budget = DiskBudget::new(bytes, entries).unwrap();
        budget.mounts.lock().unwrap().host.push(root.into());
        budget
    }
    #[test]
    fn covered_tmpfs_destinations_never_charge_a_host_volume() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        std::fs::create_dir(&home).unwrap();
        let budget = budget(&home, 65536, 1000);
        budget
            .mounts
            .lock()
            .unwrap()
            .scratch_paths
            .push("/home".into());
        budget.attach(&File::open(root.path()).unwrap()).unwrap();
        assert!(budget.mounts.lock().unwrap().scratch.is_empty());
        assert!(!budget.exceeded().unwrap());
    }
    #[test]
    fn many_small_files_share_one_byte_and_inode_budget() {
        let root = tempfile::tempdir().unwrap();
        let bytes = budget(root.path(), 65536, 1000);
        let inodes = budget(root.path(), 1024 * 1024, 10);
        assert!(!bytes.exceeded().unwrap());
        assert!(!inodes.exceeded().unwrap());
        for i in 0..20 {
            std::fs::write(root.path().join(i.to_string()), [0; 4096]).unwrap();
        }
        assert!(bytes.exceeded().unwrap());
        assert!(inodes.exceeded().unwrap());
    }
    #[test]
    fn many_hardlink_names_cannot_make_a_scan_unbounded() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("original");
        std::fs::write(&file, b"fake data").unwrap();
        let budget = budget(root.path(), 1024 * 1024, 10);
        for n in 0..20 {
            std::fs::hard_link(&file, root.path().join(n.to_string())).unwrap();
        }
        assert!(budget.exceeded().unwrap());
    }
    #[test]
    fn symlinks_never_escape_and_sparse_files_count_their_length() {
        let root = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink("/", root.path().join("outside")).unwrap();
        let budget = budget(root.path(), 65536, 1000);
        assert!(!budget.exceeded().unwrap());
        File::create(root.path().join("sparse"))
            .unwrap()
            .set_len(100000)
            .unwrap();
        assert!(budget.exceeded().unwrap());
    }
    #[test]
    fn all_writable_mounts_are_registered_and_scratch_sizes_sum_to_the_budget() {
        let root = tempfile::tempdir().unwrap();
        let budget = DiskBudget::new(65536, 1000).unwrap();
        let mut args = vec![
            "--tmpfs".into(),
            "/".into(),
            "--tmpfs".into(),
            "/tmp".into(),
            "--bind".into(),
            root.path().into(),
            "/work".into(),
        ];
        budget.configure(&mut args).unwrap();
        assert_eq!(budget.mounts.lock().unwrap().host, [root.path()]);
        let sizes: Vec<u64> = args
            .windows(2)
            .filter(|p| p[0] == "--size")
            .map(|p| p[1].to_str().unwrap().parse().unwrap())
            .collect();
        assert_eq!(sizes.len(), 3);
        assert!(sizes.iter().sum::<u64>() <= 65536);
        assert!(args.iter().any(|a| a == "--disable-userns"));
    }
}
