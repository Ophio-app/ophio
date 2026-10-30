// SPDX-License-Identifier: Apache-2.0
use protocol::{ErrorCode, ProtocolError};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("I/O failure: {0}")]
    Io(#[from] std::io::Error),
    #[error("configuration error: {0}")]
    Config(String),
    #[error("identity storage is invalid or insufficiently protected")]
    Identity,
    #[error("TLS setup failed")]
    Tls,
    #[error("{0}")]
    Protocol(#[from] ProtocolError),
    #[error("connection closed")]
    Closed,
    #[error("connection deadline elapsed")]
    Timeout,
    #[error("{0}")]
    Unsupported(&'static str),
    #[error("another Øphio process is using this data directory")]
    Running,
    /// A command that needs the running Ophio found nothing to connect to.
    #[error(
        "{product} is not running. Run `{program} run` to start it here, or `{program} service install` to start it now and whenever you log in.",
        product = ports::PRODUCT_NAME,
        program = ports::COMMAND
    )]
    NotRunning,
    /// A command that ended without doing its work, said in full.
    #[error("{0}")]
    Ended(String),
}

impl Error {
    pub(crate) fn protocol(&self) -> ProtocolError {
        match self {
            Self::Protocol(e) => e.clone(),
            Self::Config(message) => ProtocolError::new(ErrorCode::InvalidRequest, message.clone()),
            Self::Running => ProtocolError::new(
                ErrorCode::Conflict,
                "another Øphio process is using this data directory",
            ),
            Self::Timeout => ProtocolError::new(ErrorCode::Expired, "connection deadline elapsed"),
            Self::Unsupported(message) => ProtocolError::new(ErrorCode::Unsupported, *message),
            _ => ProtocolError::new(ErrorCode::Internal, "connection failed"),
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

pub(crate) fn invalid(message: &'static str) -> Error {
    ProtocolError::new(ErrorCode::InvalidRequest, message).into()
}
