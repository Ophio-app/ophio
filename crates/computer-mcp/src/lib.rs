// SPDX-License-Identifier: Apache-2.0
//! A run-bound MCP facade. Authentication and all authority stay with the broker.

mod framing;
pub mod guard;
mod mcp;
pub mod serve;
mod socket;

pub use mcp::run_stdio;

/// Provisional executable and server identity.
pub const BINARY_NAME: &str = "computer-mcp";
pub const PERMISSION_TOOL: &str = "permission_prompt";

#[derive(Debug, thiserror::Error)]
pub enum BridgeError {
    #[error("bridge I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid bridge protocol")]
    Protocol,
    #[error("bridge authentication rejected")]
    Authentication,
    #[error("bridge connection closed; this run cannot use tools")]
    Disconnected,
    #[error("bridge operation timed out; outcome unknown")]
    Timeout,
    #[error("missing bridge environment: {0}")]
    Environment(&'static str),
    #[error("bridge directory must be private and owned by the current user")]
    PrivateDirectory,
}
