// SPDX-License-Identifier: Apache-2.0
//! Manage-only directory names. No file contents, Git commands, recursive scan,
//! Library grants or registrations are produced by browsing.
use crate::{CompanionHub, api::Caller, error, internal};
use cap_std::fs::Dir;
use protocol::{ErrorCode, ProtocolError, model::Project, project_folders::*};
use std::{
    collections::BTreeMap,
    path::{Component, Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

const PAGE: usize = 100;
const SCAN_LIMIT: usize = 100_000;
const PATH_LIMIT: usize = 4096;

impl CompanionHub {
    pub(crate) async fn project_folders(
        &self,
        caller: &Caller,
        path: Option<String>,
        after: Option<String>,
        show_hidden: bool,
        search: String,
    ) -> Result<ProjectFolderListing, ProtocolError> {
        self.authorize(caller, |g| g.admin)?;
        if path
            .as_ref()
            .is_some_and(|p| p.len() > PATH_LIMIT || p.contains('\0'))
            || after.as_ref().is_some_and(|a| a.len() > 1024)
            || search.len() > 256
        {
            return Err(error(
                ErrorCode::InvalidRequest,
                "Folder request is too long",
            ));
        }
        // A stalled removable/network disk cannot grow an unbounded blocking pool.
        // A timed-out worker retains its slot until the OS call actually returns.
        let permit = self
            .inner
            .folder_browsers
            .clone()
            .try_acquire_owned()
            .map_err(|_| {
                error(
                    ErrorCode::TemporarilyUnavailable,
                    "The computer is still reading folders. Try again shortly.",
                )
            })?;
        let hub = self.clone();
        let work = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            hub.list_project_folders(path.as_deref(), after.as_deref(), show_hidden, &search)
        });
        let result = tokio::time::timeout(Duration::from_secs(5), work)
            .await
            .map_err(|_| {
                error(
                    ErrorCode::TemporarilyUnavailable,
                    "This disk is taking too long to respond. Try another location.",
                )
            })?
            .map_err(|_| internal("Folder reader stopped unexpectedly"))?;
        // A grant withdrawn while the disk was being read must not disclose the result.
        self.authorize(caller, |g| g.admin)?;
        let listing = result?;
        if serde_json::to_vec(&listing).map_err(internal)?.len() > 512 * 1024 {
            return Err(error(
                ErrorCode::LimitExceeded,
                "These folder paths are too large to display. Add a specific project using Enter a path instead.",
            ));
        }
        Ok(listing)
    }

    fn folder_entry(
        &self,
        path: &Path,
        name: String,
        projects: &[Project],
    ) -> Option<ProjectFolder> {
        let path = path.to_str()?;
        if path.len() > PATH_LIMIT {
            return None;
        }
        Some(ProjectFolder {
            name,
            path: path.into(),
            project_id: projects
                .iter()
                .find(|p| Path::new(&p.host_path) == Path::new(path))
                .map(|p| p.project_id),
            unavailable: self
                .project_folder(Path::new(path))
                .err()
                .map(|e| e.message),
        })
    }

    fn browse_allowed(&self, path: &Path) -> bool {
        !self.inner.private.iter().any(|p| path.starts_with(p))
    }

    fn list_project_folders(
        &self,
        path: Option<&str>,
        after: Option<&str>,
        show_hidden: bool,
        search: &str,
    ) -> Result<ProjectFolderListing, ProtocolError> {
        let projects = self.read(|r| r.list::<Project>("project"))?;
        let mut listing = ProjectFolderListing {
            current: None,
            ancestors: vec![],
            folders: vec![],
            next_after: None,
        };
        let Some(path) = path else {
            let mut places = Vec::new();
            if let Some(home) = &self.inner.home {
                places.push(("Home".to_owned(), home.clone()));
                for name in ["Projects", "Code", "code", "Documents", "Desktop"] {
                    places.push((name.into(), home.join(name)));
                }
            }
            places.extend(disks());
            for (name, path) in places {
                let Ok(path) = ports::library::canonical(&path) else {
                    continue;
                };
                if !self.browse_allowed(&path) || !path.is_dir() {
                    continue;
                }
                if let Some(entry) = self.folder_entry(&path, name, &projects)
                    && !listing.folders.iter().any(|e| e.path == entry.path)
                {
                    listing.folders.push(entry);
                }
            }
            return Ok(listing);
        };
        let requested = Path::new(path);
        if !requested.is_absolute()
            || requested
                .components()
                .any(|c| matches!(c, Component::ParentDir))
        {
            return Err(error(
                ErrorCode::PathNotAllowed,
                "Choose an absolute folder path without parent segments",
            ));
        }
        let path = ports::library::canonical(requested).map_err(folder_error)?;
        if !self.browse_allowed(&path) {
            return Err(error(
                ErrorCode::PathNotAllowed,
                "Ophio’s private folders cannot be browsed",
            ));
        }
        let dir =
            Dir::open_ambient_dir(&path, cap_std::ambient_authority()).map_err(folder_error)?;
        // Check the opened handle as well as the input spelling, including a
        // replacement during open. This is not a filesystem sandbox.
        let opened = opened_path(&dir).map_err(folder_error)?;
        if opened != path || !self.browse_allowed(&opened) {
            return Err(error(
                ErrorCode::PathNotAllowed,
                "This folder changed. Open it again.",
            ));
        }
        listing.current = self.folder_entry(&path, folder_name(&path), &projects);
        for ancestor in path
            .ancestors()
            .skip(1)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
        {
            if let Some(entry) = self.folder_entry(ancestor, folder_name(ancestor), &projects) {
                listing.ancestors.push(entry);
            }
        }
        let started = Instant::now();
        let search = search.to_lowercase();
        // Keep just one page plus the next name, even for a very large directory.
        let mut page = BTreeMap::new();
        for (index, entry) in dir.entries().map_err(folder_error)?.enumerate() {
            if index >= SCAN_LIMIT || started.elapsed() > Duration::from_secs(3) {
                return Err(error(
                    ErrorCode::LimitExceeded,
                    "This folder is too large or slow to list. Add a specific project using Enter a path instead.",
                ));
            }
            let entry = entry.map_err(folder_error)?;
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if (!show_hidden && name.starts_with('.'))
                || after.is_some_and(|a| name.as_str() <= a)
                || !name.to_lowercase().contains(&search)
            {
                continue;
            }
            let meta = match dir.symlink_metadata(&name) {
                Ok(meta) => meta,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(folder_error(e)),
            };
            if !meta.is_dir() || meta.file_type().is_symlink() {
                continue;
            }
            #[cfg(windows)]
            {
                use cap_std::fs::MetadataExt;
                // Junctions/reparse points are not followed during discovery.
                if meta.file_attributes() & 0x400 != 0
                    || (!show_hidden && meta.file_attributes() & 2 != 0)
                {
                    continue;
                }
            }
            let child = path.join(&name);
            if !self.browse_allowed(&child) {
                continue;
            }
            if let Some(entry) = self.folder_entry(&child, name.clone(), &projects) {
                page.insert(name, entry);
                if page.len() > PAGE + 1 {
                    page.pop_last();
                }
            }
        }
        if opened_path(&dir).map_err(folder_error)? != path {
            return Err(error(
                ErrorCode::PathNotAllowed,
                "This folder moved while it was being read. Open it again.",
            ));
        }
        let more = page.len() > PAGE;
        listing.folders = page.into_values().take(PAGE).collect();
        listing.next_after = more.then(|| listing.folders.last().expect("full page").name.clone());
        Ok(listing)
    }
}

fn folder_name(path: &Path) -> String {
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}
fn folder_error(e: std::io::Error) -> ProtocolError {
    match e.kind() {
        std::io::ErrorKind::PermissionDenied => error(
            ErrorCode::PermissionDenied,
            "The computer’s account cannot open this folder. Choose another location.",
        ),
        std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory => error(
            ErrorCode::NotFound,
            "This folder is no longer available. Reconnect its drive or choose another location.",
        ),
        _ => error(
            ErrorCode::TemporarilyUnavailable,
            "The computer could not read this folder. Try again or choose another location.",
        ),
    }
}

/// Opens a project's folder at the path it was added with. A folder since
/// replaced by a link, or under an ancestor swapped for one, leads somewhere
/// else, so the opened folder itself is checked, not only its spelling.
pub(crate) fn open_project_root(host_path: &Path) -> std::io::Result<Dir> {
    let dir = Dir::open_ambient_dir(host_path, cap_std::ambient_authority())?;
    if opened_path(&dir)? != host_path {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "The project folder was replaced",
        ));
    }
    Ok(dir)
}

#[cfg(target_os = "linux")]
fn opened_path(dir: &Dir) -> std::io::Result<PathBuf> {
    use std::os::fd::AsRawFd;
    std::fs::read_link(format!("/proc/self/fd/{}", dir.as_raw_fd()))
}
#[cfg(windows)]
#[allow(unsafe_code)]
fn opened_path(dir: &Dir) -> std::io::Result<PathBuf> {
    use std::os::windows::{ffi::OsStringExt, io::AsRawHandle};
    use windows_sys::Win32::Storage::FileSystem::GetFinalPathNameByHandleW;
    let mut buffer = vec![0u16; 32768];
    // SAFETY: dir owns the live handle, buffer has the stated writable length.
    let size = unsafe {
        GetFinalPathNameByHandleW(
            dir.as_raw_handle(),
            buffer.as_mut_ptr(),
            buffer.len() as u32,
            0,
        )
    } as usize;
    if size == 0 {
        return Err(std::io::Error::last_os_error());
    }
    if size >= buffer.len() {
        return Err(std::io::ErrorKind::InvalidInput.into());
    }
    Ok(ports::library::plain(PathBuf::from(
        std::ffi::OsString::from_wide(&buffer[..size]),
    )))
}
#[cfg(not(any(target_os = "linux", windows)))]
fn opened_path(_: &Dir) -> std::io::Result<PathBuf> {
    Err(std::io::ErrorKind::Unsupported.into())
}

#[cfg(not(windows))]
fn disks() -> Vec<(String, PathBuf)> {
    vec![
        ("Computer".into(), "/".into()),
        ("Mounted drives".into(), "/mnt".into()),
        ("Removable drives".into(), "/media".into()),
    ]
}
#[cfg(windows)]
#[allow(unsafe_code)]
fn disks() -> Vec<(String, PathBuf)> {
    // SAFETY: GetLogicalDrives has no arguments or borrowed memory.
    let mask = unsafe { windows_sys::Win32::Storage::FileSystem::GetLogicalDrives() };
    (0..26)
        .filter(|i| mask & (1 << i) != 0)
        .map(|i| {
            let drive = format!("{}:\\", char::from(b'A' + i));
            (drive.clone(), drive.into())
        })
        .collect()
}

pub(crate) fn slots() -> Arc<tokio::sync::Semaphore> {
    Arc::new(tokio::sync::Semaphore::new(2))
}

#[cfg(test)]
mod tests;
