// SPDX-License-Identifier: Apache-2.0
//! Shared validation for numeric wire frames and persisted projections.
use crate::{ErrorCode, MAX_SAFE_INTEGER, ProtocolError};

/// Validate a complete JSON frame, including nested commands and annotations.
/// Called before deserializing/dispatching client frames. Messages intentionally
/// omit field values so an invalid request cannot echo private content.
pub fn validate(value: &serde_json::Value) -> Result<(), ProtocolError> {
    use serde_json::Value;
    match value {
        Value::Number(n) => {
            let invalid = if let Some(n) = n.as_u64() {
                n > MAX_SAFE_INTEGER
            } else if let Some(n) = n.as_i64() {
                n.unsigned_abs() > MAX_SAFE_INTEGER
            } else {
                // Also reject exponent notation for an out-of-range integer.
                n.as_f64()
                    .is_some_and(|n| n.fract() == 0.0 && n.abs() > MAX_SAFE_INTEGER as f64)
            };
            if invalid {
                return Err(ProtocolError::new(
                    ErrorCode::InvalidRequest,
                    "Integer exceeds the maximum safe wire value (9007199254740991)",
                ));
            }
        }
        Value::Array(values) => {
            for value in values {
                validate(value)?;
            }
        }
        Value::Object(fields) => {
            for value in fields.values() {
                validate(value)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Schema-only type for integers nested in tuples.
#[allow(dead_code)] // Used only by schema generation.
#[derive(schemars::JsonSchema)]
#[schemars(transparent)]
pub(crate) struct SafeInteger(#[schemars(range(max = crate::MAX_SAFE_INTEGER))] u64);
