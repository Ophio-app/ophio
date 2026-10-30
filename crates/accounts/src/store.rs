// SPDX-License-Identifier: Apache-2.0
//! What Ophio remembers about its accounts between restarts: the services
//! the owner added, which accounts are paused, the last check of each and
//! the history. No key, token or code is ever written here.
use crate::files::{private_dir, write_private};
use crate::profiles::Profile;
use protocol::accounts::AccountHistoryEntry;
use protocol::time::Timestamp;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, VecDeque};
use std::marker::PhantomData;
use std::path::{Path, PathBuf};

const HISTORY: usize = 200;

/// The last status check of an account.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Status {
    #[default]
    Unchecked,
    SignedIn,
    SignedOut,
    Attention {
        reason: String,
    },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Kept {
    #[serde(default)]
    pub paused: bool,
    #[serde(default)]
    pub status: Status,
    pub identity: Option<String>,
    pub checked_at: Option<Timestamp>,
    /// An OAuth client is in the vault.
    #[serde(default)]
    pub client_set: bool,
    /// Signed in, but short of something Ophio asks for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Saved {
    #[serde(default)]
    pub added: Vec<Profile>,
    #[serde(default)]
    pub accounts: BTreeMap<String, Kept>,
    /// Newest last.
    #[serde(default)]
    pub history: VecDeque<AccountHistoryEntry>,
    #[serde(default)]
    pub password_manager_done: bool,
}
impl Saved {
    pub fn record(&mut self, entry: AccountHistoryEntry) {
        self.history.push_back(entry);
        while self.history.len() > HISTORY {
            self.history.pop_front();
        }
    }
}

#[derive(Debug)]
pub struct Store<T = Saved> {
    path: PathBuf,
    saved: PhantomData<fn() -> T>,
}
impl Store {
    pub fn new(dir: &Path) -> Self {
        Self::named(dir, "accounts.json")
    }
}
impl<T: Serialize + DeserializeOwned + Default> Store<T> {
    /// `file` in `dir`, holding a `T`.
    pub fn named(dir: &Path, file: &str) -> Self {
        Self {
            path: dir.join(file),
            saved: PhantomData,
        }
    }
    /// What was saved, or None when there is a file that can't be read or
    /// is damaged. That file is not trusted: which accounts were paused is
    /// unknown. A damaged file is copied aside and stays until a paused
    /// state is saved over it, and with no file, a copy still aside means
    /// that save never happened.
    pub fn load(&self) -> Option<T> {
        let damaged = self.path.with_extension("json.damaged");
        let bytes = match std::fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return match std::fs::symlink_metadata(&damaged) {
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some(T::default()),
                    _ => None,
                };
            }
            Err(e) => {
                tracing::warn!("{}: the saved state can't be read: {e}", self.what());
                return None;
            }
        };
        match serde_json::from_slice(&bytes) {
            Ok(saved) => Some(saved),
            Err(e) => {
                tracing::warn!(
                    "{}: the saved state is damaged, keeping a copy aside: {e}",
                    self.what()
                );
                if let Err(e) = write_private(&damaged, &bytes) {
                    tracing::warn!("{}: could not keep the damaged copy: {e}", self.what());
                }
                None
            }
        }
    }
    pub fn save(&self, saved: &T) -> std::io::Result<()> {
        if let Some(dir) = self.path.parent() {
            private_dir(dir)?;
        }
        let bytes = serde_json::to_vec_pretty(saved).map_err(std::io::Error::other)?;
        write_private(&self.path, &bytes)
    }
    /// The file's name without `.json`, for the log.
    fn what(&self) -> String {
        self.path
            .file_stem()
            .map_or_else(String::new, |s| s.to_string_lossy().into_owned())
    }
}
