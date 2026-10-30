// SPDX-License-Identifier: Apache-2.0
//! Durable, code-gated report fixes. Agents have no host credentials or merge authority.
pub mod config;
pub mod execution;
mod machine;
pub mod repository;
mod seams;

pub use machine::{CheckStep, Dependencies, Orchestrator, State, Step, Stop};
pub use seams::*;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("pipeline storage failed")]
    Sql(#[from] rusqlite::Error),
    #[error("pipeline filesystem failed")]
    Io(#[from] std::io::Error),
    #[error("pipeline artifact rejected")]
    Json(#[from] serde_json::Error),
    #[error("pipeline check failed")]
    Safety(#[from] report_pipeline::Error),
    #[error("intake unavailable")]
    Intake(#[from] intake::client::Error),
    #[error("pipeline operation failed: {0}")]
    Operation(&'static str),
    #[error("pipeline read temporarily unavailable: {0}")]
    Transient(&'static str),
    #[error("pipeline signal: {0:?}")]
    Signal(report_pipeline::signals::Signal),
    /// A contained process ran past its wall-clock limit. It stops a fix the
    /// way a spent budget does; a test submission's check records it.
    #[error("pipeline process timed out")]
    TimedOut,
    /// A contained process printed more than the output cap. Ours to report
    /// in a fix; the submission's own doing in a check.
    #[error("pipeline process output cap")]
    OutputCap,
}
impl Error {
    fn polling(error: intake::client::Error) -> Self {
        match error {
            intake::client::Error::Transport
            | intake::client::Error::Status(408 | 429 | 500..=599) => {
                Self::Transient("intake read")
            }
            other => Self::Intake(other),
        }
    }
}
pub type Result<T> = std::result::Result<T, Error>;

pub fn hash(bytes: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes).into()
}
