// SPDX-License-Identifier: Apache-2.0
//! Deliberately typed: adding a new error source requires deciding what is safe
//! to log. Generic Display/Debug may contain stored secrets or request bodies.
pub(crate) trait SafeCause {
    fn safe_cause(&self) -> String;
}
impl SafeCause for store::Error {
    fn safe_cause(&self) -> String {
        self.diagnostic()
    }
}
impl SafeCause for std::io::Error {
    fn safe_cause(&self) -> String {
        store::io_error_summary(self)
    }
}
impl SafeCause for serde_json::Error {
    fn safe_cause(&self) -> String {
        store::json_error_summary(self)
    }
}
impl<T> SafeCause for std::sync::PoisonError<T> {
    fn safe_cause(&self) -> String {
        "shared service state lock is poisoned".into()
    }
}
impl<T> SafeCause for tokio::sync::mpsc::error::SendError<T> {
    fn safe_cause(&self) -> String {
        "service channel is closed".into()
    }
}
impl SafeCause for getrandom::Error {
    fn safe_cause(&self) -> String {
        format!("secure random source: {self}")
    }
}
impl SafeCause for &'static str {
    fn safe_cause(&self) -> String {
        (*self).into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn error_causes_are_useful_without_disclosing_values() {
        let json = r#"{"command":"synthetic-secret-token"}"#;
        let error = serde_json::from_str::<protocol::Command>(json).unwrap_err();
        let mapped = crate::internal(error);
        assert!(mapped.message.contains("unknown variant"));
        assert!(!mapped.message.contains("synthetic-secret-token"));
        let mapped = crate::internal(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "secret-path",
        ));
        assert!(mapped.message.contains("PermissionDenied"));
        assert!(!mapped.message.contains("secret-path"));
        let error = store::Error::Sql(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(8),
            Some("secret-sql".into()),
        ));
        assert!(error.safe_cause().contains("readonly"));
        assert!(!error.safe_cause().contains("secret-sql"));
    }
}
