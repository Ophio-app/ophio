// SPDX-License-Identifier: Apache-2.0
//! Private report intake. This crate performs no model calls or report-driven execution.
pub mod client;
pub mod config;
pub mod maintainer;
mod server;
mod storage;
pub mod validate;

pub use server::Intake;
pub use storage::Store;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("database operation failed")]
    Sql(#[from] rusqlite::Error),
    #[error("JSON operation failed")]
    Json(#[from] serde_json::Error),
    #[error("filesystem operation failed")]
    Io(#[from] std::io::Error),
    #[error("invalid configuration or request")]
    Invalid,
    #[error("request refused")]
    Refused,
    #[error("not found")]
    NotFound,
    #[error("intake closed")]
    Closed,
    #[error("rate limited")]
    Limited(u64),
    #[error("authentication failed")]
    Unauthorized,
    #[error("worker failed")]
    Worker,
}
pub type Result<T> = std::result::Result<T, Error>;
