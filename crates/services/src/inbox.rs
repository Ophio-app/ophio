// SPDX-License-Identifier: Apache-2.0
use crate::{CompanionHub, error, internal};
use protocol::{ErrorCode, ProtocolError};
#[cfg(not(windows))]
use std::path::Component;
use std::path::{Path, PathBuf};

/// Resolve existing symlinks without creating the destination. New directories
/// are created only when receiving the first file, using normal filesystem permissions.
pub fn outside_data(path: &Path, data: &Path) -> Result<PathBuf, ProtocolError> {
    #[cfg(not(windows))]
    fn resolve(path: &Path) -> std::io::Result<PathBuf> {
        let path = if path.is_absolute() {
            path.to_owned()
        } else {
            std::env::current_dir()?.join(path)
        };
        let mut result = PathBuf::new();
        for component in path.components() {
            match component {
                Component::ParentDir => {
                    result.pop();
                }
                Component::CurDir => {}
                component => result.push(component.as_os_str()),
            }
            match result.symlink_metadata() {
                Ok(_) => result = ports::library::canonical(&result)?,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        Ok(result)
    }
    // Resolving a Windows drive prefix on its own can expand a SUBST drive
    // before RootDir is appended, losing the mapped folder. Resolve the whole
    // path instead, and compare both sides in the same non-display spelling.
    #[cfg(windows)]
    let folder = ports::library::comparison(path).map_err(internal)?;
    #[cfg(windows)]
    let data = ports::library::comparison(data).map_err(internal)?;
    #[cfg(not(windows))]
    let folder = resolve(path).map_err(internal)?;
    #[cfg(not(windows))]
    let data = resolve(data).map_err(internal)?;
    if folder.starts_with(&data) {
        return Err(error(
            ErrorCode::InvalidRequest,
            "Received-files folder must be outside Øphio's data folder",
        ));
    }
    if folder.to_str().is_none() || (folder.exists() && !folder.is_dir()) {
        return Err(error(
            ErrorCode::InvalidRequest,
            "Received-files destination must be a directory with a UTF-8 path",
        ));
    }
    #[cfg(windows)]
    let folder = ports::library::plain(folder);
    Ok(folder)
}

impl CompanionHub {
    pub fn inbox_folder(&self) -> Result<PathBuf, ProtocolError> {
        self.inner
            .inbox
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
            .ok_or_else(|| {
                error(
                    ErrorCode::Unsupported,
                    "Received-files folder is not configured",
                )
            })
    }
    pub async fn set_inbox_folder(&self, path: &Path) -> Result<(), ProtocolError> {
        let path = outside_data(path, &self.inner.data)?;
        *self
            .inner
            .inbox
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(path);
        self.refresh_host().await
    }
}
