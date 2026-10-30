// SPDX-License-Identifier: Apache-2.0
//! Diagnostics must never include stored values, SQL parameters or paths.
use crate::Error;

pub fn io_error_summary(error: &std::io::Error) -> String {
    match error.raw_os_error() {
        Some(code) => format!("filesystem {:?} (OS error {code})", error.kind()),
        None => format!("filesystem {:?}", error.kind()),
    }
}

pub fn json_error_summary(error: &serde_json::Error) -> String {
    // Serde's Display can quote the offending string (a token, chat id, etc.).
    // Missing-field names originate in the schema; all other data stays out.
    let message = error.to_string();
    let problem = if let Some(field) = message
        .strip_prefix("missing field `")
        .and_then(|s| s.split('`').next())
        .filter(|s| s.len() <= 64 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'))
    {
        format!("missing field `{field}`")
    } else {
        [
            "unknown variant",
            "unknown field",
            "duplicate field",
            "invalid type",
            "invalid value",
        ]
        .into_iter()
        .find(|prefix| message.starts_with(prefix))
        .unwrap_or(match error.classify() {
            serde_json::error::Category::Io => "JSON I/O error",
            serde_json::error::Category::Syntax => "invalid JSON syntax",
            serde_json::error::Category::Data => "JSON does not match the stored schema",
            serde_json::error::Category::Eof => "unexpected end of JSON",
        })
        .to_owned()
    };
    format!(
        "{problem} at line {} column {}",
        error.line(),
        error.column()
    )
}

impl Error {
    pub fn diagnostic(&self) -> String {
        match self {
            Self::Json(e) => format!("stored value is invalid: {}", json_error_summary(e)),
            Self::Io(e) => io_error_summary(e),
            Self::Sql(rusqlite::Error::SqliteFailure(e, _)) => format!("database: {e}"),
            Self::Sql(_) => "database query or value conversion failed".into(),
            Self::IntegerExhausted | Self::FutureSchema => self.to_string(),
            Self::Rule(e) => format!("storage rule {:?}", e.code),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn invalid_values_never_reach_decode_diagnostics() {
        for json in [
            r#"{"command":"secret-command"}"#,
            r#"{"command":"set_agent_updates","enabled":"secret-value"}"#,
        ] {
            let error = serde_json::from_str::<protocol::Command>(json).unwrap_err();
            let summary = json_error_summary(&error);
            assert!(!summary.contains("secret"));
            assert!(summary.starts_with("unknown variant") || summary.starts_with("invalid type"));
        }
    }
}
