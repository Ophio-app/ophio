// SPDX-License-Identifier: Apache-2.0
//! Computer use through Codex. Codex has no child agent whose tools can be
//! scoped apart from its parent's, so the run is the specialist itself, and
//! everything else it could reach is taken away: its native tools by
//! feature, the tools a model adds by itself through a catalog written for
//! the run, web search and connectors by setting, and the person's own MCP
//! servers one by one ([`crate::servers`]). It keeps the bridge's six computer tools
//! ([`ports::bridge::COMPUTER_TOOLS`]), reading MCP resources, a question
//! tool Codex won't drop (its requests are declined, see `server_request`),
//! and on models that run tools through code, the sandboxed runner that can
//! only call those. The run is read-only and never asks for approval, so
//! nothing that slipped through could act unasked; what does start anyway is
//! stopped as it starts ([`allowed`]).

use ports::{HarnessError, RunSpec};
use serde_json::{Value, json};
use std::{collections::HashSet, path::PathBuf};

/// The first Codex version whose tools, settings and catalog this was
/// checked against.
pub(crate) const VERSION: (u64, u64) = (0, 161);

/// Codex features that give the model a tool of its own, or act for it.
const FEATURES: &[&str] = &[
    "shell_tool",
    "unified_exec",
    "view_image",
    "image_generation",
    "browser_use",
    "browser_use_external",
    "browser_use_full_cdp_access",
    "in_app_browser",
    "computer_use",
    "apps",
    "plugins",
    "remote_plugin",
    "hooks",
    "multi_agent",
    "multi_agent_v2",
    "goals",
    "memories",
    "skill_search",
    "tool_suggest",
    "skill_mcp_dependency_install",
    "workspace_dependencies",
    "worktrees",
    "sleep_tool",
];

/// What the specialist is told about asking, since Codex keeps its own
/// question tool in every run.
pub(crate) const ASKING: &str =
    "The request_user_input tool is unavailable in this run: ask the person through request_human.";

/// Whether `version`, as `codex --version` printed it, is one computer use
/// was checked with.
pub(crate) fn supported(version: Option<&str>) -> bool {
    version
        .and_then(|v| {
            v.split_whitespace()
                .find(|w| w.starts_with(|c: char| c.is_ascii_digit()))
        })
        .and_then(|v| {
            let mut parts = v.split(['.', '-']).map(|p| p.parse::<u64>().ok());
            Some((parts.next()??, parts.next()??))
        })
        .is_some_and(|v| v >= VERSION)
}

/// The settings a computer-use run starts with, and the catalog file they
/// name, kept until the run ends.
pub(crate) struct Launch {
    catalog: tempfile::NamedTempFile,
    /// The models the catalog lists; the run's must be one of them.
    pub(crate) models: HashSet<String>,
}

impl Launch {
    /// Reads the models Codex last fetched. `own` are the person's MCP
    /// servers, which the run switches off.
    pub(crate) async fn prepare(
        spec: &RunSpec,
        bridge: &str,
        own: &[String],
    ) -> Result<Self, HarnessError> {
        let (catalog, models) = catalog(spec).await?;
        if let Some(model) = &spec.model
            && !models.contains(model)
        {
            return Err(HarnessError::Unsupported(format!(
                "Codex on this computer doesn’t list {model}. Choose one of its models for computer use."
            )));
        }
        if own.iter().any(|name| name == bridge) {
            return Err(HarnessError::Unsupported(format!(
                "Codex has an MCP server of its own named {bridge}, the name Ophio’s computer tools use. Rename it in Codex’s settings to use computer use."
            )));
        }
        Ok(Self { catalog, models })
    }

    /// The `-c` settings, each after its flag.
    pub(crate) fn arguments(&self) -> Vec<String> {
        let mut settings: Vec<String> = FEATURES
            .iter()
            .map(|feature| format!("features.{feature}=false"))
            .collect();
        settings.extend([
            "web_search=\"disabled\"".into(),
            "apps={_default={enabled=false}}".into(),
            format!(
                "model_catalog_json={}",
                crate::toml_string(&self.catalog.path().to_string_lossy())
            ),
            "sandbox_mode=\"read-only\"".into(),
            "approval_policy=\"never\"".into(),
        ]);
        settings
            .into_iter()
            .flat_map(|setting| ["-c".into(), setting])
            .collect()
    }
}

/// Where Codex keeps its settings and what it fetched for this run.
fn home(spec: &RunSpec) -> Option<PathBuf> {
    (spec.env.0.iter())
        .find(|(key, _)| key == "CODEX_HOME")
        .map(|(_, value)| PathBuf::from(value))
        .or_else(|| std::env::var_os("CODEX_HOME").map(PathBuf::from))
        .or_else(|| {
            [std::env::var_os("HOME"), std::env::var_os("USERPROFILE")]
                .into_iter()
                .flatten()
                .next()
                .map(|root| PathBuf::from(root).join(".codex"))
        })
}

const NO_CATALOG: &str =
    "Codex hasn’t fetched its models on this computer yet. Open Codex once, then try again.";

/// The run's catalog: the models Codex last fetched, each without the tools
/// a model brings by itself (its own patch tool, sub-agents, experimental
/// tools and tool search), which no feature setting removes.
async fn catalog(
    spec: &RunSpec,
) -> Result<(tempfile::NamedTempFile, HashSet<String>), HarnessError> {
    let path = home(spec)
        .ok_or_else(|| HarnessError::Unsupported(NO_CATALOG.into()))?
        .join("models_cache.json");
    let bytes = tokio::fs::read(&path)
        .await
        .map_err(|_| HarnessError::Unsupported(NO_CATALOG.into()))?;
    let cache: Value =
        serde_json::from_slice(&bytes).map_err(|_| HarnessError::Unsupported(NO_CATALOG.into()))?;
    let mut models = match &cache["models"] {
        Value::Array(models) if !models.is_empty() => models.clone(),
        _ => return Err(HarnessError::Unsupported(NO_CATALOG.into())),
    };
    let mut slugs = HashSet::new();
    for model in &mut models {
        let Some(slug) = model["slug"].as_str() else {
            return Err(HarnessError::Unsupported(NO_CATALOG.into()));
        };
        slugs.insert(slug.to_owned());
        model["apply_patch_tool_type"] = Value::Null;
        model["multi_agent_version"] = Value::Null;
        model["experimental_supported_tools"] = json!([]);
        model["supports_search_tool"] = json!(false);
    }
    let mut file = tempfile::Builder::new()
        .prefix("ophio-codex-models-")
        .suffix(".json")
        .tempfile()?;
    use std::io::Write;
    serde_json::to_writer(&mut file, &json!({ "models": models }))
        .map_err(|_| HarnessError::Protocol("cannot write the model catalog".into()))?;
    file.flush()?;
    Ok((file, slugs))
}

/// Whether a computer-use run may go on with `item`: a message, its
/// reasoning or plan, or one of the computer tools on the bridge `server`.
pub(crate) fn allowed(item: &Value, server: &str) -> bool {
    match item["type"].as_str().unwrap_or_default() {
        "userMessage" | "agentMessage" | "reasoning" | "plan" | "contextCompaction"
        | "functionCallOutput" => true,
        "mcpToolCall" => {
            item["server"] == server
                && (item["tool"].as_str())
                    .is_some_and(|tool| ports::bridge::COMPUTER_TOOLS.contains(&tool))
        }
        _ => false,
    }
}

/// Why a run [`allowed`] refused `item` was stopped, in the person's words.
pub(crate) fn stopped(item: &Value) -> String {
    let tried = match item["type"].as_str().unwrap_or_default() {
        "commandExecution" => "run a command",
        "fileChange" => "change files",
        "webSearch" => "search the web",
        "collabAgentToolCall" | "subAgentActivity" => "start another agent",
        "imageGeneration" => "make an image",
        _ => "use something other than the computer tools",
    };
    format!("Codex tried to {tried}, which computer use doesn’t allow, so Ophio stopped the task.")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_from_the_checked_one_on_are_supported() {
        assert!(supported(Some("codex-cli 0.161.0")));
        assert!(supported(Some("codex-cli 0.170.2-alpha.1")));
        assert!(supported(Some("codex-cli 1.0.0")));
        assert!(!supported(Some("codex-cli 0.160.9")));
        assert!(!supported(Some("codex-cli 0.153.2 fixture")));
        assert!(!supported(Some("codex-cli")));
        assert!(!supported(None));
    }

    #[test]
    fn only_messages_and_the_bridge_computer_tools_go_on() {
        for item in [
            json!({"type":"agentMessage"}),
            json!({"type":"reasoning"}),
            json!({"type":"mcpToolCall","server":"computer","tool":"observe_target"}),
            json!({"type":"mcpToolCall","server":"computer","tool":"request_human"}),
        ] {
            assert!(allowed(&item, "computer"), "{item}");
        }
        for item in [
            json!({"type":"commandExecution"}),
            json!({"type":"fileChange"}),
            json!({"type":"webSearch"}),
            json!({"type":"collabAgentToolCall"}),
            json!({"type":"dynamicToolCall"}),
            json!({"type":"imageGeneration"}),
            json!({"type":"somethingNew"}),
            json!({"type":"mcpToolCall","server":"computer","tool":"show_person"}),
            json!({"type":"mcpToolCall","server":"other","tool":"observe_target"}),
            json!({"type":"mcpToolCall","tool":"observe_target"}),
        ] {
            assert!(!allowed(&item, "computer"), "{item}");
        }
    }
}
