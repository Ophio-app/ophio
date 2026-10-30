// SPDX-License-Identifier: Apache-2.0
//! Non-inference app-server model/list and config/read discovery.
use crate::io;
use ports::HarnessError;
use protocol::capability::*;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{path::Path, time::Duration};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Page {
    data: Vec<NativeModel>,
    next_cursor: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NativeModel {
    model: String,
    display_name: String,
    hidden: bool,
    is_default: bool,
    default_reasoning_effort: String,
    supported_reasoning_efforts: Vec<NativeEffort>,
    /// What it reads; versions before images were listed say nothing.
    #[serde(default)]
    input_modalities: Vec<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NativeEffort {
    reasoning_effort: String,
}

pub(crate) fn effort_label(id: &str) -> String {
    match id {
        "none" => "None",
        "minimal" => "Minimal",
        "low" => "Low",
        "medium" => "Medium",
        "high" => "High",
        "xhigh" => "Extra high",
        "max" => "Maximum",
        "ultra" => "Ultra",
        _ => return id.replace(['_', '-'], " "),
    }
    .into()
}
fn parse_page(value: Value) -> Result<(Vec<ModelOption>, Option<String>), HarnessError> {
    let page: Page = serde_json::from_value(value).map_err(|_| failure())?;
    if page.data.len() > 255 {
        return Err(failure());
    }
    let mut models = Vec::new();
    for m in page.data.into_iter().filter(|m| !m.hidden) {
        if m.model.is_empty()
            || m.model.len() > 128
            || m.display_name.is_empty()
            || m.display_name.len() > 256
            || m.supported_reasoning_efforts.len() > 32
        {
            return Err(failure());
        }
        let efforts: Vec<_> = m
            .supported_reasoning_efforts
            .into_iter()
            .map(|e| EffortOption {
                label: effort_label(&e.reasoning_effort),
                id: e.reasoning_effort,
            })
            .collect();
        if efforts.iter().any(|e| e.id.is_empty() || e.id.len() > 64) {
            return Err(failure());
        }
        let default_effort = efforts
            .iter()
            .find(|e| e.id == m.default_reasoning_effort)
            .map(|e| e.id.clone());
        models.push(ModelOption {
            id: m.model,
            label: m.display_name,
            is_default: m.is_default,
            // Codex marks no model as made for computer use; its default
            // one, when it reads pictures of the screen, is the suggestion.
            suggested_for_computer_use: m.is_default
                && m.input_modalities.iter().any(|i| i == "image"),
            details: Some(ModelDetails {
                alias: false,
                efforts,
                default_effort,
            }),
        });
    }
    Ok((models, page.next_cursor))
}
fn failure() -> HarnessError {
    HarnessError::Protocol("Native model catalog could not be read".into())
}

pub(crate) async fn probe(binary: &Path) -> (Vec<ModelOption>, CatalogObservation) {
    let result = inspect(binary).await;
    match result {
        Ok((models, model, effort)) => {
            let observation = CatalogObservation {
                checked_at: protocol::time::Timestamp::now(),
                source: "Codex app-server model/list and config/read".into(),
                availability: Availability::Available,
                default_model: model,
                default_effort: effort,
            };
            (models, observation)
        }
        Err(_) => (
            Vec::new(),
            CatalogObservation {
                checked_at: protocol::time::Timestamp::now(),
                source: "Codex app-server model/list".into(),
                availability: Availability::Unknown {
                    reason: "The agent’s model catalog could not be checked".into(),
                },
                default_model: None,
                default_effort: None,
            },
        ),
    }
}
async fn inspect(
    binary: &Path,
) -> Result<(Vec<ModelOption>, Option<String>, Option<String>), HarnessError> {
    let discovery = tempfile::tempdir()?;
    let mut child = io::command(binary)
        .arg("app-server")
        .current_dir(discovery.path())
        .spawn()?;
    let mut stdin = child.stdin.take().ok_or_else(failure)?;
    let stdout = child.stdout.take().ok_or_else(failure)?;
    let mut frames = io::Frames::new(stdout);
    let result = tokio::time::timeout(Duration::from_secs(5), async {
        async fn response(frames: &mut io::Frames<tokio::process::ChildStdout>, id: u64) -> Result<Value, HarnessError> {
            for _ in 0..64 {
                let bytes = frames.next().await?.ok_or_else(failure)?;
                let v: Value = serde_json::from_slice(&bytes).map_err(|_| failure())?;
                if v["id"] == id {
                    if v.get("error").is_some() { return Err(failure()); }
                    return v.get("result").cloned().ok_or_else(failure);
                }
                // Discovery never acknowledges permission or tool requests.
                if v.get("id").is_some() { return Err(failure()); }
            }
            Err(failure())
        }
        io::write(&mut stdin, &json!({"id":1,"method":"initialize","params":{"clientInfo":{"name":ports::PROGRAM_NAME,"version":env!("CARGO_PKG_VERSION")},"capabilities":{"experimentalApi":true}}})).await?;
        response(&mut frames, 1).await?;
        io::write(&mut stdin, &json!({"method":"initialized"})).await?;
        let mut cursor: Option<String> = None;
        let mut models = Vec::new();
        for id in 2..10 {
            io::write(&mut stdin, &json!({"id":id,"method":"model/list","params":{"limit":100,"cursor":cursor,"includeHidden":false}})).await?;
            let (page, next) = parse_page(response(&mut frames, id).await?)?;
            models.extend(page);
            if models.len() > 255 { return Err(failure()); }
            if next.is_none() { cursor = None; break; }
            if next == cursor { return Err(failure()); }
            cursor = next;
        }
        if cursor.is_some() || models.is_empty() { return Err(failure()); }
        io::write(&mut stdin, &json!({"id":10,"method":"config/read","params":{"includeLayers":false}})).await?;
        let config = response(&mut frames, 10).await?;
        let configured_model = config["config"]["model"].as_str().map(str::to_owned);
        let configured_effort = config["config"]["model_reasoning_effort"].as_str().map(str::to_owned);
        if let Some(model) = &configured_model {
            for m in &mut models { m.is_default = m.id == *model; }
        }
        let default = models.iter().find(|m| m.is_default);
        let model = configured_model.or_else(|| default.map(|m| m.id.clone()));
        let effort = configured_effort.or_else(|| default.and_then(|m| m.details.as_ref()?.default_effort.clone()));
        Ok((models, model, effort))
    }).await.map_err(|_| failure()).and_then(|r| r);
    drop(stdin);
    io::kill(&mut child);
    let _ = child.wait().await;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_efforts_are_per_model_and_hidden_models_stay_hidden() {
        let row = |id, hidden, efforts: Vec<&str>| json!({"model":id,"displayName":"GPT example","hidden":hidden,"isDefault":false,"defaultReasoningEffort":"high","supportedReasoningEfforts":efforts.into_iter().map(|e| json!({"reasoningEffort":e})).collect::<Vec<_>>()});
        let (models, _) = parse_page(json!({"data":[row("a",false,vec!["high","xhigh"]),row("b",false,vec!["low"]),row("hidden",true,vec![])],"nextCursor":null})).unwrap();
        assert_eq!(models.len(), 2);
        assert_eq!(
            models[0].details.as_ref().unwrap().efforts[1].label,
            "Extra high"
        );
        assert_eq!(models[1].details.as_ref().unwrap().efforts.len(), 1);
        assert_eq!(models[1].details.as_ref().unwrap().default_effort, None);
        assert!(parse_page(json!({"data":[{"model":"invented"}]})).is_err());
    }
}
