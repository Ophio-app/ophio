// SPDX-License-Identifier: Apache-2.0
//! Installation is an explicit local-owner operation; recognition never calls it.
use crate::catalog::{CatalogEntry, ModelFile, reviewed};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error("model is not in the reviewed catalog")]
    UnknownModel,
    #[error("model is already installed; remove it explicitly before installing again")]
    AlreadyInstalled,
    #[error("model location must be a regular directory without symbolic links")]
    InvalidPath,
    #[error("model file has the wrong size: {file}")]
    Size { file: &'static str },
    #[error("model file failed SHA-256 verification: {file}")]
    Hash { file: &'static str },
    #[error("model filesystem operation failed: {0}")]
    Io(#[from] std::io::Error),
    // Do not propagate reqwest's final redirect URL (it can contain signed URLs).
    #[error("model download failed")]
    Download,
    #[error("could not set owner-only model permissions")]
    Permissions,
}

#[derive(Debug, Clone, Copy)]
pub struct InstallProgress {
    pub model: &'static str,
    pub file: &'static str,
    pub downloaded_bytes: u64,
    pub total_bytes: u64,
}

/// Only call from an explicit, authenticated local-owner install action.
/// Downloads use HTTPS, contain no audio, and require no account or credentials.
/// All three verified files become visible together via a directory rename.
pub async fn install(
    model: &CatalogEntry,
    model_dir: &Path,
    progress: impl FnMut(InstallProgress) + Send,
) -> Result<PathBuf, InstallError> {
    if !reviewed(model) {
        return Err(InstallError::UnknownModel);
    }
    let client = reqwest::Client::builder()
        .https_only(true)
        .connect_timeout(Duration::from_secs(30))
        .timeout(Duration::from_secs(1800))
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()
        .map_err(|_| InstallError::Download)?;
    install_from(model, model_dir, progress, &client).await
}

async fn install_from(
    model: &CatalogEntry,
    model_dir: &Path,
    mut progress: impl FnMut(InstallProgress) + Send,
    client: &reqwest::Client,
) -> Result<PathBuf, InstallError> {
    ensure_dir(model_dir)?;
    let destination = model_dir.join(model.name);
    if destination.symlink_metadata().is_ok() {
        return Err(InstallError::AlreadyInstalled);
    }
    let staging = tempfile::Builder::new()
        .prefix(".install-")
        .tempdir_in(model_dir)?;
    owner_only(staging.path(), true)?;
    let total = model.size_bytes();
    let mut downloaded = 0;
    for file in model.files {
        let mut response = client
            .get(file.url)
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(|_| InstallError::Download)?;
        if response
            .content_length()
            .is_some_and(|size| size != file.size_bytes)
        {
            return Err(InstallError::Size { file: file.name });
        }
        let path = staging.path().join(file.name);
        let mut out = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        owner_only(&path, false)?;
        let mut hash = Sha256::new();
        let mut size = 0;
        while let Some(chunk) = response.chunk().await.map_err(|_| InstallError::Download)? {
            size += chunk.len() as u64;
            if size > file.size_bytes {
                return Err(InstallError::Size { file: file.name });
            }
            out.write_all(&chunk)?;
            hash.update(&chunk);
            downloaded += chunk.len() as u64;
            progress(InstallProgress {
                model: model.name,
                file: file.name,
                downloaded_bytes: downloaded,
                total_bytes: total,
            });
        }
        check(file, size, &hash.finalize())?;
        out.sync_all()?;
    }
    sync_dir(staging.path())?;
    if destination.symlink_metadata().is_ok() {
        return Err(InstallError::AlreadyInstalled);
    }
    fs::rename(staging.path(), &destination)?;
    sync_dir(model_dir)?;
    Ok(destination)
}

/// Remove only the selected catalog model. Existing loaded engines retain their
/// weights until dropped; the composition root must replace its engine as well.
pub fn remove(model: &CatalogEntry, model_dir: &Path) -> Result<bool, InstallError> {
    if !reviewed(model) {
        return Err(InstallError::UnknownModel);
    }
    let path = model_dir.join(model.name);
    let metadata = match path.symlink_metadata() {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e.into()),
    };
    regular_dir(model_dir)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(InstallError::InvalidPath);
    }
    // Never recursively delete unknown user data or follow links.
    for entry in fs::read_dir(&path)? {
        let entry = entry?;
        if !model.files.iter().any(|f| entry.file_name() == f.name) || !entry.file_type()?.is_file()
        {
            return Err(InstallError::InvalidPath);
        }
    }
    for file in model.files {
        match fs::remove_file(path.join(file.name)) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    fs::remove_dir(path)?;
    sync_dir(model_dir)?;
    Ok(true)
}

pub(crate) fn verified_read(dir: &Path, file: &ModelFile) -> Result<Vec<u8>, InstallError> {
    regular_dir(dir)?;
    let path = dir.join(file.name);
    let meta = path.symlink_metadata()?;
    if !meta.is_file() || meta.file_type().is_symlink() {
        return Err(InstallError::InvalidPath);
    }
    if meta.len() != file.size_bytes {
        return Err(InstallError::Size { file: file.name });
    }
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(file.size_bytes + 1)
        .read_to_end(&mut bytes)?;
    check(file, bytes.len() as u64, &Sha256::digest(&bytes))?;
    Ok(bytes)
}

fn check(file: &ModelFile, size: u64, hash: &[u8]) -> Result<(), InstallError> {
    if size != file.size_bytes {
        return Err(InstallError::Size { file: file.name });
    }
    let expected = file.sha256.as_bytes();
    if expected.len() != 64
        || !hash.iter().enumerate().all(|(i, b)| {
            let hex = b"0123456789abcdef";
            expected[2 * i] == hex[(b >> 4) as usize]
                && expected[2 * i + 1] == hex[(b & 15) as usize]
        })
    {
        return Err(InstallError::Hash { file: file.name });
    }
    Ok(())
}

fn ensure_dir(path: &Path) -> Result<(), InstallError> {
    if !path.try_exists()? {
        fs::create_dir_all(path)?;
        owner_only(path, true)?;
    }
    regular_dir(path)
}

fn regular_dir(path: &Path) -> Result<(), InstallError> {
    let m = path.symlink_metadata()?;
    if !m.is_dir() || m.file_type().is_symlink() {
        return Err(InstallError::InvalidPath);
    }
    Ok(())
}

#[cfg(unix)]
fn owner_only(path: &Path, directory: bool) -> Result<(), InstallError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(
        path,
        fs::Permissions::from_mode(if directory { 0o700 } else { 0o600 }),
    )?;
    Ok(())
}

#[cfg(windows)]
fn owner_only(path: &Path, directory: bool) -> Result<(), InstallError> {
    use std::os::windows::process::CommandExt;
    // Set a protected DACL containing only the current user. The path is passed
    // as data, never interpolated into PowerShell. No profile or visible window.
    let system = std::env::var_os("SystemRoot").ok_or(InstallError::Permissions)?;
    let exe = PathBuf::from(system).join("System32/WindowsPowerShell/v1.0/powershell.exe");
    let script = r#"$ErrorActionPreference='Stop'; $sid=[System.Security.Principal.WindowsIdentity]::GetCurrent().User; if($env:SPEECH_PERMISSION_DIRECTORY -eq '1') {$acl=New-Object System.Security.AccessControl.DirectorySecurity; $rule=New-Object System.Security.AccessControl.FileSystemAccessRule($sid,'FullControl','ContainerInherit,ObjectInherit','None','Allow')} else {$acl=New-Object System.Security.AccessControl.FileSecurity; $rule=New-Object System.Security.AccessControl.FileSystemAccessRule($sid,'FullControl','Allow')}; $acl.SetOwner($sid); $acl.SetAccessRuleProtection($true,$false); $acl.AddAccessRule($rule); Set-Acl -LiteralPath $env:SPEECH_PERMISSION_PATH -AclObject $acl"#;
    let status = std::process::Command::new(exe)
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            script,
        ])
        .env("SPEECH_PERMISSION_PATH", path)
        .env(
            "SPEECH_PERMISSION_DIRECTORY",
            if directory { "1" } else { "0" },
        )
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .creation_flags(0x08000000)
        .status()?;
    if !status.success() {
        return Err(InstallError::Permissions);
    }
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn owner_only(_: &Path, _: bool) -> Result<(), InstallError> {
    Err(InstallError::Permissions)
}

fn sync_dir(path: &Path) -> Result<(), InstallError> {
    #[cfg(unix)]
    fs::File::open(path)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

#[cfg(test)]
mod tests;
