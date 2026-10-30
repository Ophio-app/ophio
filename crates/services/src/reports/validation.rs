// SPDX-License-Identifier: Apache-2.0
use crate::error;
use base64::{Engine, engine::general_purpose::STANDARD};
use protocol::{ErrorCode, ProtocolError, reports::*};

fn invalid() -> ProtocolError {
    error(
        ErrorCode::InvalidRequest,
        "The report contains an invalid field",
    )
}
pub(crate) fn validate(bundle: &ReportBundle) -> Result<(), ProtocolError> {
    let bytes = serde_json::to_vec(bundle).map_err(|_| invalid())?;
    if bytes.len() > MAX_REPORT_BYTES {
        return Err(error(ErrorCode::LimitExceeded, "The report is too large"));
    }
    if bundle.schema != REPORT_SCHEMA
        || bundle
            .words
            .as_ref()
            .is_some_and(|s| s.chars().count() > MAX_REPORT_WORDS)
        || bundle.phone_trail.len() > MAX_TRAIL_EVENTS
        || bundle.computer_trail.len() > MAX_TRAIL_EVENTS
        || bundle.crashes.iter().any(|c| c.frames.len() > MAX_FRAMES)
    {
        return Err(invalid());
    }
    let value = serde_json::to_value(bundle).map_err(|_| invalid())?;
    // Only explicitly consented text and image bytes may exceed code length.
    fn lengths(v: &serde_json::Value, path: &str) -> bool {
        match v {
            serde_json::Value::String(s) => {
                let free = path == "words"
                    || path == "screenshot.data"
                    || (path.starts_with("extras.") && path.ends_with(".entries.text"));
                free || s.chars().count() <= MAX_REPORT_CODE
            }
            serde_json::Value::Array(a) => a.iter().all(|v| lengths(v, path)),
            serde_json::Value::Object(o) => o.iter().all(|(k, v)| {
                lengths(
                    v,
                    &if path.is_empty() {
                        k.clone()
                    } else {
                        format!("{path}.{k}")
                    },
                )
            }),
            _ => true,
        }
    }
    if !lengths(&value, "") || protocol::integer::validate(&value).is_err() {
        return Err(invalid());
    }
    if let Some(image) = &bundle.screenshot {
        if image.width == 0
            || image.height == 0
            || image.data.len() > MAX_SCREENSHOT_BYTES.div_ceil(3) * 4
        {
            return Err(invalid());
        }
        let data = STANDARD.decode(&image.data).map_err(|_| invalid())?;
        let magic = match image.media_type.as_str() {
            "image/png" => data.starts_with(b"\x89PNG\r\n\x1a\n"),
            "image/jpeg" => data.starts_with(&[0xff, 0xd8, 0xff]),
            "image/webp" => data.starts_with(b"RIFF") && data.get(8..12) == Some(b"WEBP"),
            _ => false,
        };
        if data.len() > MAX_SCREENSHOT_BYTES || !magic {
            return Err(invalid());
        }
    }
    Ok(())
}
