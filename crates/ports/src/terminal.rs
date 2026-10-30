// SPDX-License-Identifier: Apache-2.0
//! Host terminal sessions (PTY on Unix, ConPTY on Windows).
//!
//! Terminals outlive phone connections: detaching never kills. Output is
//! addressed by a monotonically increasing byte offset and retained in a
//! bounded scrollback so a reattaching client can resume exactly or learn
//! that bytes were dropped.

use std::path::PathBuf;

use async_trait::async_trait;
use tokio::sync::broadcast;

use protocol::capability::HarnessKind;
use protocol::ids::{ProjectId, TerminalId};
use protocol::terminal::TerminalInfo;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalSpec {
    pub title: String,
    pub cwd: PathBuf,
    /// Program and arguments; `None` = the user's login shell.
    pub program: Option<(PathBuf, Vec<String>)>,
    pub env: Vec<(String, String)>,
    pub project_id: Option<ProjectId>,
    pub harness: Option<HarnessKind>,
    pub cols: u16,
    pub rows: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalEvent {
    Output { offset: u64, bytes: bytes::Bytes },
    Resized { cols: u16, rows: u16 },
    Exited { code: Option<i32> },
}

/// Retained output from `from_offset` (or the oldest retained byte).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Replay {
    pub from: u64,
    pub truncated: bool,
    pub bytes: bytes::Bytes,
}

#[async_trait]
pub trait TerminalHost: Send + Sync + std::fmt::Debug + 'static {
    async fn open(&self, spec: TerminalSpec) -> Result<TerminalInfo, TerminalError>;
    async fn list(&self) -> Vec<TerminalInfo>;
    async fn info(&self, id: TerminalId) -> Option<TerminalInfo>;
    /// Whether processes other than the terminal's shell remain in its
    /// container. A replaced shell or uncertain container membership returns
    /// `None` (or an error), never `Some(false)`: callers must not treat an
    /// inconclusive check as an exited app.
    async fn has_running_children(&self, _id: TerminalId) -> Result<Option<bool>, TerminalError> {
        Ok(None)
    }
    /// Atomically snapshot retained output and subscribe to what follows, so
    /// no byte is lost or duplicated between replay and live output.
    async fn attach(
        &self,
        id: TerminalId,
        from_offset: Option<u64>,
    ) -> Result<(Replay, broadcast::Receiver<TerminalEvent>), TerminalError>;
    async fn write(&self, id: TerminalId, bytes: &[u8]) -> Result<(), TerminalError>;
    async fn resize(&self, id: TerminalId, cols: u16, rows: u16) -> Result<(), TerminalError>;
    /// Terminate the process tree and release the PTY.
    async fn close(&self, id: TerminalId) -> Result<(), TerminalError>;
}

#[derive(Debug, thiserror::Error)]
pub enum TerminalError {
    #[error("terminal not found")]
    NotFound,
    #[error("terminal has exited")]
    Exited,
    #[error("too many terminals")]
    Limit,
    #[error("spawn failed: {0}")]
    Spawn(String),
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
}

impl TerminalError {
    pub fn code(&self) -> protocol::ErrorCode {
        use protocol::ErrorCode;
        match self {
            TerminalError::NotFound => ErrorCode::NotFound,
            TerminalError::Exited => ErrorCode::Conflict,
            TerminalError::Limit => ErrorCode::LimitExceeded,
            TerminalError::Spawn(_) | TerminalError::Io(_) => ErrorCode::NativeFailure,
        }
    }
}
