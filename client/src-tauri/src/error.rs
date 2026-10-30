// SPDX-License-Identifier: Apache-2.0
use protocol::{ErrorCode, ProtocolError};
use serde::Serialize;

pub type Result<T> = std::result::Result<T, Error>;

/// Errors contain safe explanations, never raw transport or platform payloads.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(
        "Another device answered at the computer’s address with a different certificate, so nothing was sent to it."
    )]
    CertificateMismatch,
    #[error("Cannot reach the computer. Check its address and private network.")]
    Unreachable,
    #[error("The connection closed before the computer confirmed the operation.")]
    Closed,
    #[error("The computer did not answer in time.")]
    Timeout,
    #[error("This phone is not paired with that computer.")]
    NotPaired,
    #[error("The computer no longer accepts this phone’s device key. Pair again on the computer.")]
    Unauthenticated,
    #[error("This app and Øphio on the computer use incompatible protocol versions.")]
    Incompatible,
    #[error("{0}")]
    Invalid(&'static str),
    #[error("The phone could not save its pairing information.")]
    Storage,
    #[error("The phone could not use its protected device key.")]
    Key,
    #[error("{0}")]
    Native(String),
    #[error("{0}")]
    Remote(ProtocolError),
    #[error("The connection is busy. Try again.")]
    Busy,
}
impl Error {
    pub fn connection_message(&self) -> String {
        if let Self::Remote(error) = self
            && error.code == ErrorCode::IncompatibleVersion
            && let Some(value) = error.detail.as_ref().and_then(|d| d.get("supported"))
            && let Ok(versions) =
                serde_json::from_value::<Vec<protocol::ProtocolVersion>>(value.clone())
            && let Some(peer) = versions.iter().max()
        {
            let app = protocol::ProtocolVersion::CURRENT;
            let side = if peer.major > app.major {
                "the app on this phone"
            } else {
                "Øphio on the computer"
            };
            return format!(
                "The app supports protocol {app}; the computer supports {peer}. Update {side} from its original source."
            );
        }
        match self {
            Self::Remote(error) => error.message.clone(),
            _ => self.to_string(),
        }
    }
    pub fn code(&self) -> ErrorCode {
        match self {
            Self::CertificateMismatch | Self::Unauthenticated => ErrorCode::Unauthenticated,
            Self::Unreachable | Self::Closed | Self::Busy => ErrorCode::TemporarilyUnavailable,
            Self::Timeout => ErrorCode::Expired,
            Self::NotPaired => ErrorCode::NotFound,
            Self::Incompatible => ErrorCode::IncompatibleVersion,
            Self::Invalid(_) => ErrorCode::InvalidRequest,
            Self::Storage | Self::Key | Self::Native(_) => ErrorCode::NativeFailure,
            Self::Remote(e) => e.code,
        }
    }
    /// Preserve the computer's typed refusal, including on older companions.
    pub fn remote_code(&self) -> Option<ErrorCode> {
        match self {
            Self::Remote(error) => Some(error.code),
            _ => None,
        }
    }
    pub fn failure(&self) -> Option<&'static str> {
        match self {
            Self::NotPaired => Some("not_paired"),
            Self::Unauthenticated | Self::Key => Some("unauthenticated"),
            Self::Incompatible => Some("incompatible_version"),
            Self::Remote(e) if e.code == ErrorCode::Unauthenticated => Some("revoked"),
            Self::Remote(e) if e.code == ErrorCode::IncompatibleVersion => {
                Some("incompatible_version")
            }
            _ => None,
        }
    }
}
impl Serialize for Error {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        // A refusal from the computer keeps its code and says why on its own,
        // without the code's name in front of its words.
        ProtocolError::new(self.code(), self.connection_message())
            .with_detail(serde_json::json!({ "reason": self.failure() }))
            .serialize(serializer)
    }
}

#[cfg(test)]
mod compatibility_tests {
    use super::*;
    #[test]
    fn remote_refusal_keeps_its_code_and_message() {
        let error = Error::Remote(ProtocolError::new(
            ErrorCode::NotFound,
            "Terminal not found",
        ));
        assert_eq!(error.remote_code(), Some(ErrorCode::NotFound));
        assert_eq!(error.connection_message(), "Terminal not found");
        assert_eq!(Error::Closed.remote_code(), None);
    }
    #[test]
    fn a_serialized_refusal_is_the_computers_own_words() {
        let error = Error::Remote(ProtocolError::new(ErrorCode::Conflict, "Check it again."));
        let value = serde_json::to_value(&error).unwrap();
        assert_eq!(value["code"], "conflict");
        assert_eq!(value["message"], "Check it again.");
        assert_eq!(
            serde_json::to_value(Error::Timeout).unwrap()["message"],
            "The computer did not answer in time."
        );
    }
    #[test]
    fn incompatibility_names_the_side_that_needs_updating() {
        let error = Error::Remote(
            ProtocolError::new(ErrorCode::IncompatibleVersion, "unsupported")
                .with_detail(serde_json::json!({"supported":[{"major":2,"minor":0}]})),
        );
        assert!(
            error
                .connection_message()
                .contains("Update the app on this phone")
        );
        let old = Error::Remote(
            ProtocolError::new(ErrorCode::IncompatibleVersion, "unsupported")
                .with_detail(serde_json::json!({"supported":[{"major":0,"minor":0}]})),
        );
        assert!(
            old.connection_message()
                .contains("Update Øphio on the computer")
        );
        assert!(
            Error::Incompatible
                .connection_message()
                .contains("incompatible")
        );
    }
}
