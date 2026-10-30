// SPDX-License-Identifier: Apache-2.0
//! Native metadata only. No prompt is sent by a catalog probe or launch check.
use crate::catalog::Listed;
use ports::HarnessError;
use serde_json::{Value, json};
use std::time::Duration;

pub(crate) const CHECK_TIMEOUT: Duration = Duration::from_secs(8);
const INITIALIZE: &str = "ophio-model-initialize";
const SETTINGS: &str = "ophio-model-settings";

pub(crate) fn exact(model: Option<&str>) -> Option<&str> {
    model.filter(|id| id.starts_with("claude-"))
}

fn request(id: &str, subtype: &str) -> Value {
    json!({"type":"control_request", "request_id":id, "request":{"subtype":subtype}})
}

fn response<'a>(value: &'a Value, id: &str) -> Result<&'a Value, HarnessError> {
    if value["type"] != "control_response"
        || value["response"]["request_id"] != id
        || value["response"]["subtype"] != "success"
    {
        return Err(unverified());
    }
    Ok(&value["response"]["response"])
}

pub(crate) fn unverified() -> HarnessError {
    HarnessError::Protocol("native exact model could not be verified before prompt delivery".into())
}

/// Check the same process that will consume the prompt, including resumes and
/// project/managed model overrides. Never treat a CLI argument as actual evidence.
pub(crate) struct Verification {
    expected: String,
    waiting: &'static str,
}

impl Verification {
    pub fn new(expected: &str) -> Self {
        Self {
            expected: expected.into(),
            waiting: INITIALIZE,
        }
    }

    pub fn first(&self) -> Value {
        request(INITIALIZE, "initialize")
    }

    pub fn matches(&self, value: &Value) -> bool {
        value["response"]["request_id"] == self.waiting
    }

    /// Some(next request) or None once all checks pass. No settings are written.
    pub fn accept(&mut self, value: &Value) -> Result<Option<Value>, HarnessError> {
        let data = response(value, self.waiting)?;
        if self.waiting == INITIALIZE {
            let models: Vec<Listed> =
                serde_json::from_value(data["models"].clone()).map_err(|_| unverified())?;
            if !models
                .iter()
                .any(|m| m.resolved_model.as_deref() == Some(&self.expected))
            {
                return Err(unverified());
            }
            self.waiting = SETTINGS;
            Ok(Some(request(SETTINGS, "get_settings")))
        } else {
            if data["applied"]["model"].as_str() != Some(&self.expected)
                || data["effective"]["switchModelsOnFlag"] != false
            {
                return Err(unverified());
            }
            Ok(None)
        }
    }
}

/// Stop if the native main conversation later reports a different model. Child
/// agents have their own selection and must not replace the parent's identity.
pub(crate) fn check_event(expected: Option<&str>, value: &Value) -> Result<(), HarnessError> {
    let Some(expected) = expected else {
        return Ok(());
    };
    if value["parent_tool_use_id"]
        .as_str()
        .is_some_and(|id| !id.is_empty())
    {
        return Ok(());
    }
    let model = match value["type"].as_str() {
        Some("system") if value["subtype"] == "init" => Some(&value["model"]),
        Some("system")
            if matches!(
                value["subtype"].as_str(),
                Some("model_consent_fallback" | "model_fallback")
            ) =>
        {
            Some(&value["fallback_model"])
        }
        // Native API errors use a sentinel, not a generated response's model.
        // Let StreamState preserve their warning and the terminal native reason.
        Some("assistant")
            if value["is_api_error_message"] == true
                && value["message"]["model"] == "<synthetic>" =>
        {
            None
        }
        Some("assistant") => value["message"].get("model"),
        Some("stream_event") if value["event"]["type"] == "message_start" => {
            value["event"]["message"].get("model")
        }
        _ => None,
    };
    if model.is_some_and(|model| model.as_str() != Some(expected)) {
        return Err(HarnessError::Protocol(
            "native model changed after exact selection".into(),
        ));
    }
    Ok(())
}
