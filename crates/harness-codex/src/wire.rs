// SPDX-License-Identifier: Apache-2.0
//! Deliberately small subset of the 0.153.2 generated v2 protocol.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(untagged)]
pub(crate) enum Id {
    Text(String),
    Number(i64),
}
impl Id {
    pub fn key(&self) -> String {
        serde_json::to_string(self).expect("JSON id is serializable")
    }
}
#[derive(Debug, Deserialize)]
pub(crate) struct Envelope {
    pub id: Option<Id>,
    pub method: Option<String>,
    #[serde(default)]
    pub params: Value,
    #[serde(default)]
    pub result: Value,
    pub error: Option<Value>,
}
#[derive(Debug, Serialize)]
#[serde(tag = "type")]
pub(crate) enum Input {
    #[serde(rename = "text")]
    Text {
        text: String,
        text_elements: Vec<Value>,
    },
    #[serde(rename = "image")]
    Image { url: String },
    /// A skill the turn uses, as Codex's own `$name` mention sends it.
    #[serde(rename = "skill")]
    Skill { name: String, path: PathBuf },
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ThreadResponse {
    pub thread: Thread,
    pub model: String,
    pub model_provider: String,
    pub reasoning_effort: Option<String>,
}
#[derive(Debug, Deserialize)]
pub(crate) struct Thread {
    pub id: String,
}
#[derive(Debug, Deserialize)]
pub(crate) struct TurnResponse {
    pub turn: Turn,
}
#[derive(Debug, Deserialize)]
pub(crate) struct Turn {
    pub id: String,
    pub status: String,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}
