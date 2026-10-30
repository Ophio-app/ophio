// SPDX-License-Identifier: Apache-2.0
//! Linux descriptor-relative materialization. No symlink is followed at any
//! level, and every created file is exclusive. Other platforms fail closed.
use super::{Code, pack, validate};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Binding {
    pub path: String,
    pub device: u64,
    pub inode: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct Manifest {
    pub binding: Binding,
    pub files: BTreeMap<String, (String, bool)>,
    pub config_hash: String,
    pub index_hash: String,
    #[serde(default)]
    pub object_ids: BTreeMap<String, String>,
}
#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use flate2::{Compression, write::ZlibEncoder};
    use rustix::fs::{Mode, OFlags, ResolveFlags};
    use std::{
        fs::File,
        io::{Read, Write},
        os::unix::fs::MetadataExt,
        path::Path,
    };
    fn io(_: impl std::fmt::Debug) -> Code {
        Code::WorkspaceConflict
    }
    fn open(base: &File, path: &str, dir: bool) -> Result<File, Code> {
        rustix::fs::openat2(
            base,
            path,
            OFlags::RDONLY
                | OFlags::CLOEXEC
                | OFlags::NONBLOCK
                | if dir {
                    OFlags::DIRECTORY
                } else {
                    OFlags::empty()
                },
            Mode::empty(),
            ResolveFlags::BENEATH
                | ResolveFlags::NO_SYMLINKS
                | ResolveFlags::NO_MAGICLINKS
                | ResolveFlags::NO_XDEV,
        )
        .map(File::from)
        .map_err(io)
    }
    fn absolute(path: &str) -> Result<File, Code> {
        rustix::fs::openat2(
            rustix::fs::CWD,
            path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
            ResolveFlags::NO_SYMLINKS | ResolveFlags::NO_MAGICLINKS,
        )
        .map(File::from)
        .map_err(io)
    }
    fn binding(file: &File, path: String) -> Result<Binding, Code> {
        let meta = file.metadata().map_err(io)?;
        Ok(Binding {
            path,
            device: meta.dev(),
            inode: meta.ino(),
        })
    }
    pub(in crate::contribution) fn bind(path: &str) -> Result<Binding, Code> {
        if !Path::new(path).is_absolute() {
            return Err(Code::RootChanged);
        }
        binding(&absolute(path)?, path.into())
    }
    pub(in crate::contribution) fn available() -> bool {
        absolute(".").is_ok()
    }
    fn root(b: &Binding) -> Result<File, Code> {
        let file = absolute(&b.path)?;
        if binding(&file, b.path.clone())? != *b {
            return Err(Code::RootChanged);
        }
        Ok(file)
    }
    pub(in crate::contribution) fn free_bytes(b: &Binding) -> Result<u128, Code> {
        let stat = rustix::fs::fstatvfs(root(b)?).map_err(io)?;
        Ok((stat.f_bavail as u128) * (stat.f_frsize as u128))
    }

    pub(in crate::contribution) fn create(b: &Binding, name: &str) -> Result<Binding, Code> {
        let r = root(b)?;
        rustix::fs::mkdirat(&r, name, Mode::from_raw_mode(0o700)).map_err(io)?;
        r.sync_all().map_err(io)?;
        binding(&open(&r, name, true)?, format!("{}/{name}", b.path))
    }
    fn directory(base: &File, path: &str) -> Result<File, Code> {
        let mut parent = base.try_clone().map_err(io)?;
        if !path.is_empty() {
            for part in path.split('/') {
                match rustix::fs::mkdirat(&parent, part, Mode::from_raw_mode(0o700)) {
                    Ok(()) => parent.sync_all().map_err(io)?,
                    Err(rustix::io::Errno::EXIST) => {}
                    Err(e) => return Err(io(e)),
                }
                parent = open(&parent, part, true)?;
            }
        }
        Ok(parent)
    }
    fn write(
        base: &File,
        path: &str,
        bytes: &[u8],
        executable: bool,
        check: pack::Check<'_>,
    ) -> Result<(), Code> {
        check()?;
        let (parent, name) = path.rsplit_once('/').unwrap_or(("", path));
        let parent = directory(base, parent)?;
        let fd = rustix::fs::openat2(
            &parent,
            name,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC,
            Mode::from_raw_mode(if executable { 0o700 } else { 0o600 }),
            ResolveFlags::BENEATH
                | ResolveFlags::NO_SYMLINKS
                | ResolveFlags::NO_MAGICLINKS
                | ResolveFlags::NO_XDEV,
        )
        .map_err(io)?;
        let mut file = File::from(fd);
        for chunk in bytes.chunks(65_536) {
            check()?;
            file.write_all(chunk).map_err(io)?;
        }
        file.sync_all().map_err(io)?;
        parent.sync_all().map_err(io)?;
        Ok(())
    }
    fn read(base: &File, path: &str, limit: usize) -> Result<Vec<u8>, Code> {
        let mut f = open(base, path, false)?;
        let before = f.metadata().map_err(io)?;
        if !before.is_file() || before.nlink() != 1 || before.len() > limit as u64 {
            return Err(Code::WorkspaceConflict);
        }
        let mut bytes = Vec::new();
        (&mut f)
            .take(limit as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(io)?;
        let after = f.metadata().map_err(io)?;
        if bytes.len() > limit
            || before.len() != after.len()
            || before.mtime() != after.mtime()
            || before.mtime_nsec() != after.mtime_nsec()
            || before.ctime() != after.ctime()
            || before.ctime_nsec() != after.ctime_nsec()
        {
            return Err(Code::WorkspaceConflict);
        }
        Ok(bytes)
    }
    pub(in crate::contribution) fn policies(
        tree: &pack::Tree,
        h: &validate::Handoff,
    ) -> Result<(), Code> {
        let mut total = 0;
        for policy in &h.policies {
            let bytes = tree
                .files
                .get(&policy.path)
                .and_then(|f| tree.objects.get(&f.oid))
                .map(|o| &o.bytes)
                .ok_or(Code::PolicyMismatch)?;
            total += bytes.len();
            if bytes.len() > 256 * 1024
                || total > 2 * 1024 * 1024
                || validate::digest(bytes) != policy.sha256
            {
                return Err(Code::PolicyMismatch);
            }
        }
        Ok(())
    }
    pub(in crate::contribution) fn materialize(
        b: &Binding,
        tree: &pack::Tree,
        h: &validate::Handoff,
        check: pack::Check<'_>,
    ) -> Result<Manifest, Code> {
        let base = root(b)?;
        policies(tree, h)?;
        let mut files = BTreeMap::new();
        let mut index = b"DIRC\0\0\0\x02".to_vec();
        index.extend_from_slice(&(tree.files.len() as u32).to_be_bytes());
        for (path, f) in &tree.files {
            check()?;
            let bytes = &tree.objects.get(&f.oid).ok_or(Code::UnsafeObject)?.bytes;
            write(&base, path, bytes, f.executable, check)?;
            files.insert(path.clone(), (validate::digest(bytes), f.executable));
            let start = index.len();
            index.extend_from_slice(&[0; 24]); // ctime, mtime, dev, ino: force Git to stat
            index.extend_from_slice(
                &(if f.executable {
                    0o100755u32
                } else {
                    0o100644u32
                })
                .to_be_bytes(),
            );
            index.extend_from_slice(&[0; 8]); // uid, gid
            index.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
            for i in (0..40).step_by(2) {
                index.push(u8::from_str_radix(&f.oid[i..i + 2], 16).map_err(io)?);
            }
            index.extend_from_slice(&(path.len() as u16).to_be_bytes());
            index.extend_from_slice(path.as_bytes());
            index.push(0);
            while !(index.len() - start).is_multiple_of(8) {
                index.push(0);
            }
        }
        let checksum = pack::sha1(&[&index], check)?;
        index.extend_from_slice(&checksum);
        for (hash, o) in &tree.objects {
            check()?;
            let header = format!("{} {}\0", pack::kind(o.kind)?, o.bytes.len());
            let mut z = ZlibEncoder::new(Vec::new(), Compression::fast());
            z.write_all(header.as_bytes()).map_err(io)?;
            for chunk in o.bytes.chunks(65_536) {
                check()?;
                z.write_all(chunk).map_err(io)?;
            }
            let bytes = z.finish().map_err(io)?;
            write(
                &base,
                &format!(".git/objects/{}/{}", &hash[..2], &hash[2..]),
                &bytes,
                false,
                check,
            )?;
        }
        let config = format!(
            "[core]\n\trepositoryformatversion = 0\n\tbare = false\n\tfilemode = true\n\thooksPath = /dev/null\n[remote \"origin\"]\n\turl = {}\n\tfetch = +refs/heads/*:refs/remotes/origin/*\n",
            h.source.repository_url
        );
        for (path, bytes) in [
            (".git/config", config.as_bytes()),
            (".git/HEAD", b"ref: refs/heads/contribution\n".as_slice()),
            (".git/refs/heads/contribution", h.source.commit.as_bytes()),
            (".git/shallow", h.source.commit.as_bytes()),
            (".git/index", &index),
        ] {
            write(&base, path, bytes, false, check)?;
        }
        base.sync_all().map_err(io)?;
        check()?;
        if bind(&b.path)? != *b {
            return Err(Code::RootChanged);
        }
        Ok(Manifest {
            binding: b.clone(),
            files,
            config_hash: validate::digest(config),
            index_hash: validate::digest(&index),
            object_ids: tree
                .files
                .iter()
                .map(|(path, file)| (path.clone(), file.oid.clone()))
                .collect(),
        })
    }
    pub(in crate::contribution) fn verify(
        m: &Manifest,
        h: &validate::Handoff,
        check: pack::Check<'_>,
    ) -> Result<bool, Code> {
        let base = root(&m.binding)?;
        if read(&base, ".git/HEAD", 256)? != b"ref: refs/heads/contribution\n"
            || std::str::from_utf8(&read(&base, ".git/refs/heads/contribution", 128)?)
                .map_err(io)?
                .trim()
                != h.source.commit
            || validate::digest(read(&base, ".git/config", 4096)?) != m.config_hash
        {
            return Err(Code::WorkspaceConflict);
        }
        for p in &h.policies {
            check()?;
            if validate::digest(read(&base, &p.path, 256 * 1024).map_err(|_| Code::PolicyMismatch)?)
                != p.sha256
            {
                return Err(Code::PolicyMismatch);
            }
        }
        let index = read(&base, ".git/index", pack::ENTRIES * (64 + 512 + 8) + 32)?;
        let mut dirty = false;
        let mut count = 0;
        let mut bytes = 0;
        inspect(&base, "", m, &mut dirty, &mut count, &mut bytes, check)?;
        let mut sizes = BTreeMap::new();
        for path in m.files.keys() {
            check()?;
            match open(&base, path, false).and_then(|f| f.metadata().map_err(io)) {
                Ok(meta) if meta.is_file() && meta.nlink() == 1 => {
                    sizes.insert(path.clone(), u32::try_from(meta.len()).map_err(io)?);
                }
                _ => dirty = true,
            }
        }
        if !dirty {
            // A clean tree supplies the original sizes, even if Git's racy
            // stat cache records zero. Normalize only cache metadata, keeping
            // staged content/modes/paths bound to the original manifest.
            dirty = !crate::contribution::index::unchanged(&index, &m.index_hash, &sizes, check)?;
        }
        if bind(&m.binding.path)? != m.binding {
            return Err(Code::RootChanged);
        }
        Ok(dirty)
    }
    /// Inspect only bounded, descriptor-relative files. Git configuration and
    /// hooks are never executed, and native paths never enter the result.
    pub(in crate::contribution) fn details(
        m: &Manifest,
        check: pack::Check<'_>,
    ) -> Result<protocol::contribution::ContributionConflict, Code> {
        use std::collections::BTreeSet;
        let base = root(&m.binding)?;
        let mut detail = protocol::contribution::ContributionConflict {
            changed: None,
            untracked: None,
            branch: None,
            revision: None,
            project_id: None,
        };
        if let Ok(head) =
            read(&base, ".git/HEAD", 256).and_then(|v| String::from_utf8(v).map_err(io))
        {
            let head = head.trim();
            if validate::hex(head, 40) {
                detail.revision = Some(head.into());
            } else if let Some(branch) = head.strip_prefix("ref: refs/heads/")
                && branch.len() <= 80
                && validate::path(branch).is_ok()
                && branch
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-/".contains(&b))
            {
                detail.branch = Some(branch.into());
                if let Ok(revision) = read(&base, &format!(".git/refs/heads/{branch}"), 128)
                    .and_then(|v| String::from_utf8(v).map_err(io))
                    && validate::hex(revision.trim(), 40)
                {
                    detail.revision = Some(revision.trim().into());
                }
            }
        }
        let mut seen = BTreeSet::new();
        let mut changed = BTreeSet::new();
        let mut untracked = BTreeSet::new();
        let mut count = 0;
        let mut bytes = 0;
        detail_files(
            &base,
            "",
            m,
            &mut seen,
            &mut changed,
            &mut untracked,
            &mut count,
            &mut bytes,
            check,
        )?;
        for path in m.files.keys() {
            if !seen.contains(path) {
                changed.insert(path.clone());
            }
        }
        // Include staged-only differences. Old journals lack object IDs: don't
        // invent counts if their staged tree cannot be compared safely.
        if let Ok(index) = read(&base, ".git/index", pack::ENTRIES * (64 + 512 + 8) + 32)
            && let Some(entries) = crate::contribution::index::entries(&index, check)?
            && m.object_ids.len() == m.files.len()
        {
            for (path, (.., executable)) in &m.files {
                let expected_mode: u32 = if *executable { 0o100755 } else { 0o100644 };
                if entries.get(path).is_none_or(|e| {
                    pack::hex(&e[40..60]) != m.object_ids[path]
                        || e[24..28] != expected_mode.to_be_bytes()
                }) {
                    changed.insert(path.clone());
                }
            }
            for path in entries.keys().filter(|p| !m.files.contains_key(*p)) {
                changed.insert(path.clone());
                untracked.remove(path);
            }
            detail.changed = Some(changed.len() as u32);
            detail.untracked = Some(untracked.len() as u32);
        }
        if bind(&m.binding.path)? != m.binding {
            return Err(Code::RootChanged);
        }
        Ok(detail)
    }
    #[allow(clippy::too_many_arguments)]
    fn detail_files(
        base: &File,
        prefix: &str,
        m: &Manifest,
        seen: &mut std::collections::BTreeSet<String>,
        changed: &mut std::collections::BTreeSet<String>,
        untracked: &mut std::collections::BTreeSet<String>,
        count: &mut usize,
        bytes: &mut usize,
        check: pack::Check<'_>,
    ) -> Result<(), Code> {
        if prefix.split('/').count() > 33 {
            return Err(Code::LimitExceeded);
        }
        let dir = if prefix.is_empty() {
            base.try_clone().map_err(io)?
        } else {
            open(base, prefix.trim_end_matches('/'), true)?
        };
        for entry in rustix::fs::Dir::read_from(&dir).map_err(io)? {
            check()?;
            let entry = entry.map_err(io)?;
            let name = entry.file_name().to_str().map_err(io)?;
            if name == "." || name == ".." || (prefix.is_empty() && name == ".git") {
                continue;
            }
            *count += 1;
            if *count > pack::ENTRIES {
                return Err(Code::LimitExceeded);
            }
            let path = format!("{prefix}{name}");
            if path.len() > 512 {
                return Err(Code::LimitExceeded);
            }
            if entry.file_type() == rustix::fs::FileType::Directory {
                detail_files(
                    base,
                    &format!("{path}/"),
                    m,
                    seen,
                    changed,
                    untracked,
                    count,
                    bytes,
                    check,
                )?;
            } else if let Some((hash, executable)) = m.files.get(&path) {
                seen.insert(path.clone());
                let differs = match read(base, &path, pack::OBJECT) {
                    Ok(data) => {
                        *bytes += data.len();
                        if *bytes > pack::EXPANDED {
                            return Err(Code::LimitExceeded);
                        }
                        let mode = open(base, &path, false)
                            .and_then(|f| f.metadata().map_err(io))
                            .map(|m| m.mode() & 0o111 != 0);
                        validate::digest(data) != *hash || mode != Ok(*executable)
                    }
                    Err(_) => true,
                };
                if differs {
                    changed.insert(path);
                }
            } else {
                untracked.insert(path);
            }
        }
        Ok(())
    }
    fn inspect(
        base: &File,
        prefix: &str,
        m: &Manifest,
        dirty: &mut bool,
        count: &mut usize,
        bytes: &mut usize,
        check: pack::Check<'_>,
    ) -> Result<(), Code> {
        if prefix.split('/').count() > 33 {
            return Err(Code::LimitExceeded);
        }
        let dir = if prefix.is_empty() {
            base.try_clone().map_err(io)?
        } else {
            open(base, prefix.trim_end_matches('/'), true)?
        };
        let entries = rustix::fs::Dir::read_from(&dir).map_err(io)?;
        for entry in entries {
            check()?;
            let entry = entry.map_err(io)?;
            let name = entry.file_name().to_str().map_err(io)?;
            if name == "." || name == ".." || (prefix.is_empty() && name == ".git") {
                continue;
            }
            *count += 1;
            if *count > pack::ENTRIES {
                return Err(Code::LimitExceeded);
            }
            let path = format!("{prefix}{name}");
            if entry.file_type() == rustix::fs::FileType::Directory {
                inspect(base, &format!("{path}/"), m, dirty, count, bytes, check)?;
            } else if let Some((hash, executable)) = m.files.get(&path) {
                match read(base, &path, pack::OBJECT) {
                    Ok(data) => {
                        *bytes += data.len();
                        if *bytes > pack::EXPANDED {
                            return Err(Code::LimitExceeded);
                        }
                        *dirty |= validate::digest(&data) != *hash;
                        let mode = open(base, &path, false)
                            .and_then(|f| f.metadata().map_err(io))
                            .map(|m| m.mode() & 0o111 != 0);
                        *dirty |= mode != Ok(*executable);
                    }
                    Err(_) => *dirty = true,
                }
            } else {
                *dirty = true;
            }
        }
        Ok(())
    }
}
#[cfg(target_os = "linux")]
pub(super) use linux::{available, bind, create, details, materialize, verify};
#[cfg(not(target_os = "linux"))]
pub(super) fn available() -> bool {
    false
}
#[cfg(not(target_os = "linux"))]
pub(super) fn bind(_: &str) -> Result<Binding, Code> {
    Err(Code::CompatibilityUnsupported)
}
#[cfg(not(target_os = "linux"))]
pub(super) fn create(_: &Binding, _: &str) -> Result<Binding, Code> {
    Err(Code::CompatibilityUnsupported)
}
#[cfg(not(target_os = "linux"))]
pub(super) fn materialize(
    _: &Binding,
    _: &pack::Tree,
    _: &validate::Handoff,
    _: pack::Check<'_>,
) -> Result<Manifest, Code> {
    Err(Code::CompatibilityUnsupported)
}
#[cfg(not(target_os = "linux"))]
pub(super) fn verify(
    _: &Manifest,
    _: &validate::Handoff,
    _: pack::Check<'_>,
) -> Result<bool, Code> {
    Err(Code::CompatibilityUnsupported)
}

/// Free-space accounting is injectable without bypassing the real writer or
/// inode/containment checks. The portable record cannot select an implementation.
pub(super) trait Space: std::fmt::Debug + Send + Sync {
    fn available_bytes(&self, root: &Binding) -> Result<u128, Code>;
}
#[derive(Debug)]
pub(super) struct SystemSpace;
impl Space for SystemSpace {
    fn available_bytes(&self, root: &Binding) -> Result<u128, Code> {
        #[cfg(target_os = "linux")]
        {
            linux::free_bytes(root)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = root;
            Err(Code::CompatibilityUnsupported)
        }
    }
}

#[cfg(not(target_os = "linux"))]
pub(super) fn details(
    _: &Manifest,
    _: pack::Check<'_>,
) -> Result<protocol::contribution::ContributionConflict, Code> {
    Err(Code::CompatibilityUnsupported)
}
