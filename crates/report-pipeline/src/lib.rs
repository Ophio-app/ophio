// SPDX-License-Identifier: Apache-2.0
//! Checks for untrusted report-driven fixes. No agent execution or network I/O.
pub mod budget;
pub mod corpus;
pub mod gates;
mod git;
pub mod honeytoken;
pub mod oscillation;
pub mod provenance;
pub mod render;
pub mod schema;
pub mod signals;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("pipeline I/O failed")]
    Io(#[from] std::io::Error),
    #[error("pipeline database failed")]
    Sql(#[from] rusqlite::Error),
    #[error("pipeline JSON failed")]
    Json(#[from] serde_json::Error),
    #[error("agent output rejected")]
    Rejected(signals::Signal),
    #[error("git inspection failed")]
    Git,
    #[error("invalid pipeline data: {0}")]
    Invalid(&'static str),
}
impl Error {
    pub fn signal(&self) -> Option<signals::Signal> {
        if let Self::Rejected(signal) = self {
            Some(*signal)
        } else {
            None
        }
    }
}
pub type Result<T> = std::result::Result<T, Error>;

pub(crate) fn digest(bytes: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes).into()
}
pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
