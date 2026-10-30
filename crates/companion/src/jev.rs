// SPDX-License-Identifier: Apache-2.0
//! Owner-only Jev access for voice control; the phone receives status, never
//! the key.
use crate::{
    Error, Result,
    config::Paths,
    frontdoors::TokenInput,
    identity::{FileSecretStore, SecretStore},
    local::{OwnerClient, OwnerRequest, OwnerResponse},
};
use async_trait::async_trait;
use clap::Subcommand;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Subcommand)]
pub enum JevCommand {
    /// Store your TypeSafe access key so voice control can work.
    ///
    /// Voice control sends what you said and the names of the controls on
    /// screen to TypeSafe, which says what you meant. Charges go to your own
    /// Jev access, separately from agent subscriptions. The key is read from a
    /// hidden prompt or one line of standard input, and stays on this
    /// computer.
    Connect,
    /// Show whether Jev is linked and when it was last checked.
    Status,
    /// Delete the stored key. Voice control stops until Jev is linked again.
    Disconnect,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum JevOperation {
    Connect { key: TokenInput },
    Status,
    Disconnect,
}
#[derive(Debug, Clone)]
pub(crate) struct JevKeys {
    path: PathBuf,
}
impl JevKeys {
    /// A key kept under the file's earlier name moves here once, so a
    /// computer already linked stays linked.
    pub fn new(paths: &Paths) -> Self {
        let secrets = paths.data.join("secrets");
        let path = secrets.join("jev.key");
        let earlier = secrets.join("routing.key");
        if earlier.symlink_metadata().is_ok_and(|m| m.is_file()) && path.symlink_metadata().is_err()
        {
            let _ = std::fs::rename(&earlier, &path);
        }
        Self { path }
    }
    fn connect(&self, key: &TokenInput) -> Result<()> {
        if self
            .path
            .symlink_metadata()
            .is_ok_and(|m| !m.is_file() || m.file_type().is_symlink())
        {
            return Err(Error::Identity);
        }
        FileSecretStore::new(self.path.clone()).save(key.as_bytes(), true)
    }
    fn disconnect(&self) -> Result<()> {
        match std::fs::remove_file(&self.path) {
            Ok(()) => {
                #[cfg(unix)]
                std::fs::File::open(self.path.parent().ok_or(Error::Identity)?)?.sync_all()?;
                Ok(())
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}
#[async_trait]
impl ::jev::KeySource for JevKeys {
    async fn load(&self) -> std::result::Result<Option<::jev::Key>, ::jev::Error> {
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || match FileSecretStore::new(path).load() {
            Ok(Some(bytes)) => {
                let text = std::str::from_utf8(&bytes).map_err(|_| ::jev::Error::NotConnected)?;
                ::jev::Key::new(text.to_owned()).map(Some)
            }
            Ok(None) => Ok(None),
            Err(_) => Err(::jev::Error::NotConnected),
        })
        .await
        .map_err(|_| ::jev::Error::NotConnected)?
    }
}

pub(crate) async fn execute(paths: &Paths, command: JevCommand, json: bool) -> Result<()> {
    let operation = match command {
        JevCommand::Connect => {
            eprintln!(
                "Voice control asks Jev what you meant. It sends what you said and the names of the controls on screen to TypeSafe. Charges go to your own Jev access, separately from native agent subscriptions. Linking itself makes no network request. Enter your TypeSafe access key; it stays on this computer."
            );
            let key = tokio::task::spawn_blocking(TokenInput::read_stdin)
                .await
                .map_err(|_| Error::Config("Key input failed".into()))??;
            JevOperation::Connect { key }
        }
        JevCommand::Status => JevOperation::Status,
        JevCommand::Disconnect => JevOperation::Disconnect,
    };
    let mut client = OwnerClient::connect(&paths.socket()).await?;
    let response: OwnerResponse = client.request(&OwnerRequest::Jev { operation }).await?;
    let OwnerResponse::Jev(access) = response else {
        return Err(Error::Closed);
    };
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&access).map_err(|_| Error::Closed)?
        );
    } else {
        print!(
            "{}",
            crate::readable::jev(
                &access,
                protocol::time::Timestamp::now(),
                crate::readable::Style::stdout()
            )
        );
    }
    Ok(())
}

pub(crate) async fn apply(
    paths: &Paths,
    hub: &services::CompanionHub,
    operation: JevOperation,
) -> Result<OwnerResponse> {
    let keys = JevKeys::new(paths);
    match operation {
        JevOperation::Connect { key } => {
            tokio::task::spawn_blocking(move || keys.connect(&key))
                .await
                .map_err(|_| Error::Identity)??;
            hub.jev_linked(true)?;
        }
        JevOperation::Disconnect => {
            hub.jev_linked(false)?;
            tokio::task::spawn_blocking(move || keys.disconnect())
                .await
                .map_err(|_| Error::Identity)??;
        }
        JevOperation::Status => {}
    }
    Ok(OwnerResponse::Jev(hub.jev_access().await?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::APP_NAME;
    use clap::Parser;
    #[test]
    fn no_key_argument_and_redacted_owner_request() {
        assert!(crate::cli::Cli::try_parse_from([APP_NAME, "jev", "connect"]).is_ok());
        assert!(
            crate::cli::Cli::try_parse_from([APP_NAME, "jev", "connect", "synthetic-key"]).is_err()
        );
        let request = OwnerRequest::Jev {
            operation: JevOperation::Connect {
                key: TokenInput::new("synthetic-key".into()).unwrap(),
            },
        };
        assert!(!format!("{request:?}").contains("synthetic-key"));
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn private_key_file_rejects_links_and_loose_permissions() {
        use ::jev::KeySource;
        use std::os::unix::fs::{PermissionsExt, symlink};
        let dir = tempfile::tempdir().unwrap();
        let keys = JevKeys {
            path: dir.path().join("secrets/jev.key"),
        };
        assert!(keys.load().await.unwrap().is_none());
        keys.connect(&TokenInput::new("synthetic-key".into()).unwrap())
            .unwrap();
        assert_eq!(
            std::fs::metadata(&keys.path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(keys.load().await.unwrap().is_some());
        assert!(!format!("{:?}", keys.load().await.unwrap()).contains("synthetic-key"));
        std::fs::set_permissions(&keys.path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(keys.load().await.is_err());
        keys.disconnect().unwrap();
        let other = dir.path().join("other");
        std::fs::write(&other, "synthetic-private").unwrap();
        symlink(&other, &keys.path).unwrap();
        assert!(keys.load().await.is_err());
        assert!(
            keys.connect(&TokenInput::new("synthetic-key".into()).unwrap())
                .is_err()
        );
        keys.disconnect().unwrap();
        assert_eq!(std::fs::read_to_string(other).unwrap(), "synthetic-private");
    }
    #[tokio::test]
    async fn a_key_under_the_earlier_name_stays_linked() {
        use ::jev::KeySource;
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths {
            config: dir.path().join("config"),
            data: dir.path().join("data"),
            runtime: dir.path().join("runtime"),
        };
        let secrets = paths.data.join("secrets");
        let earlier = JevKeys {
            path: secrets.join("routing.key"),
        };
        earlier
            .connect(&TokenInput::new("synthetic-key".into()).unwrap())
            .unwrap();
        let keys = JevKeys::new(&paths);
        assert!(keys.load().await.unwrap().is_some());
        assert!(!earlier.path.exists());
        // Once moved, a key linked since is never replaced by an earlier one.
        std::fs::write(&earlier.path, "stale").unwrap();
        JevKeys::new(&paths);
        assert!(earlier.path.exists());
    }
}
