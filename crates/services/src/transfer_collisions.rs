// SPDX-License-Identifier: Apache-2.0
use crate::{
    CompanionHub,
    api::Caller,
    error, internal,
    plumbing::library_error,
    transfers::{collision_name, project_folder, relative, valid_name},
};
use cap_std::{ambient_authority, fs::Dir};
use protocol::{library::LibraryRoot, transfer::*, *};
use std::path::Path;

impl CompanionHub {
    pub(crate) async fn replacement_root(
        &self,
        destination: &TransferDestination,
    ) -> Result<LibraryRoot, ProtocolError> {
        let TransferDestination::ProjectDir { project_id, .. } = destination else {
            return Err(error(
                ErrorCode::Unsupported,
                "Replacement is available only in a writable project Library root",
            ));
        };
        let project: protocol::model::Project = self.read(|r| r.require("project", project_id))?;
        self.library()?.roots().await.into_iter().find(|r| r.writable && r.only.is_none()
                    && r.project_id == Some(*project_id) && Path::new(&r.host_path) == Path::new(&project.host_path))
            .ok_or_else(|| error(ErrorCode::Unsupported, "Share the project folder as a writable Library root to enable recoverable replacement"))
    }

    pub(crate) async fn check_upload_destination(
        &self,
        caller: &Caller,
        name: &str,
        destination: &TransferDestination,
    ) -> Result<UploadDestinationCheck, ProtocolError> {
        valid_name(name)?;
        let mut check = UploadDestinationCheck {
            exists: false,
            keep_both_name: name.into(),
            replace_revision: None,
            replace_unavailable: None,
        };
        let (dir, relative_dir) = match destination {
            TransferDestination::Artifact => return Ok(check),
            TransferDestination::Inbox => {
                match Dir::open_ambient_dir(self.inbox_folder()?, ambient_authority()) {
                    Ok(inbox) => (inbox, String::new()),
                    // Publication creates the folder on first use.
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(check),
                    Err(e) => return Err(internal(e)),
                }
            }
            TransferDestination::ProjectDir { project_id, dir } => {
                relative(dir, true)?;
                let project: protocol::model::Project =
                    self.read(|r| r.require("project", project_id))?;
                (project_folder(&project, dir)?, dir.clone())
            }
        };
        check.exists = match dir.symlink_metadata(name) {
            Ok(_) => true,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
            Err(e) => return Err(internal(e)),
        };
        if !check.exists {
            return Ok(check);
        }
        for suffix in 1..10000 {
            let candidate = collision_name(name, suffix);
            match dir.symlink_metadata(&candidate) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    check.keep_both_name = candidate;
                    break;
                }
                Err(e) => return Err(internal(e)),
                Ok(_) => {}
            }
        }
        if check.keep_both_name == name {
            return Err(error(
                ErrorCode::LimitExceeded,
                "Too many filename collisions",
            ));
        }
        let replacement = async {
            self.authorize(caller, |g| g.library_read && g.library_write)?;
            let root = self.replacement_root(destination).await?;
            let path = if relative_dir.is_empty() {
                name.into()
            } else {
                format!("{relative_dir}/{name}")
            };
            self.library()?
                .read(root.root_id, &path)
                .await
                .map_err(library_error)
        }
        .await;
        match replacement {
            Ok(file) => check.replace_revision = Some(file.revision),
            Err(e) => check.replace_unavailable = Some(e.message),
        }
        Ok(check)
    }
}
