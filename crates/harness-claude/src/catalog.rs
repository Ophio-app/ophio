// SPDX-License-Identifier: Apache-2.0
//! Choices the installed CLI offers, never a guessed release list: the
//! models Claude Code lists for its own picker, or, from a CLI that doesn't
//! list them, the ones its help documents.
use ports::model_name::model_name;
use protocol::capability::*;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Default, Deserialize)]
struct Settings {
    model: Option<String>,
    #[serde(rename = "effortLevel")]
    effort: Option<String>,
}

/// One model as Claude Code lists it for its picker.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Listed {
    value: String,
    pub(crate) resolved_model: Option<String>,
    #[serde(default)]
    supports_effort: bool,
    #[serde(default)]
    supported_effort_levels: Vec<String>,
}

const LISTING: &str = "ophio-models";

/// The models the CLI at `binary` offers this account, as its own picker
/// lists them. Asking starts no turn, so it spends nothing, and runs no hooks
/// or tools.
pub(crate) async fn listed(binary: &Path) -> Option<Vec<Listed>> {
    let cwd = tempfile::tempdir().ok()?;
    let mut cmd = crate::io::command(binary);
    cmd.current_dir(cwd.path());
    cmd.args([
        "-p",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
        "--safe-mode",
        "--tools",
        "",
        "--setting-sources",
        "user",
        "--settings",
        r#"{"disableAllHooks":true}"#,
        "--strict-mcp-config",
        "--mcp-config",
        r#"{"mcpServers":{}}"#,
        "--no-session-persistence",
    ]);
    let mut child = cmd.spawn().ok()?;
    let mut stdin = child.stdin.take()?;
    let mut frames = crate::io::Frames::new(child.stdout.take()?);
    let ask =
        json!({"type":"control_request","request_id":LISTING,"request":{"subtype":"initialize"}});
    // The child is killed as it drops, answered or not.
    let result = tokio::time::timeout(Duration::from_secs(10), async {
        crate::io::write(&mut stdin, &ask).await.ok()?;
        while let Some(frame) = frames.next().await.ok()? {
            let Ok(value) = serde_json::from_slice::<Value>(&frame) else {
                continue;
            };
            let response = &value["response"];
            if value["type"] == "control_response" && response["request_id"] == LISTING {
                if response["subtype"] != "success" {
                    return None;
                }
                return serde_json::from_value(response["response"]["models"].clone()).ok();
            }
        }
        None
    })
    .await
    .ok()
    .flatten();
    crate::io::kill(&mut child);
    let _ = child.wait().await;
    result
}

/// The models and efforts the CLI offers: `listed`, or, without a list, what
/// `claude --help`, as `help` printed it, documents.
pub(crate) async fn probe(
    listed: Option<Vec<Listed>>,
    help: Option<&std::process::Output>,
    settings: Option<PathBuf>,
) -> (Vec<ModelOption>, CatalogObservation) {
    let help = help
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout.clone()).ok());
    // Read only the two typed preferences. Never export or log settings content.
    let mut settings = match settings {
        Some(path) => read_settings(&path).await.unwrap_or_default(),
        None => Settings::default(),
    };
    let source = if listed.is_some() {
        "Installed Claude Code's model list and user settings"
    } else {
        "Installed Claude Code help and user settings"
    };
    let (models, efforts) = match listed {
        Some(listed) => from_list(listed, settings.model.as_deref()),
        None => help
            .as_deref()
            .map(|h| parse(h, settings.model.as_deref()))
            .unwrap_or_default(),
    };
    if settings.model.is_none() {
        settings.model = models.iter().find(|m| m.is_default).map(|m| m.id.clone());
    }
    let observation = CatalogObservation {
        checked_at: protocol::time::Timestamp::now(),
        source: source.into(),
        availability: if models.is_empty() {
            Availability::Unknown {
                reason: "The installed agent did not document model choices".into(),
            }
        } else {
            Availability::Available
        },
        default_model: settings.model,
        default_effort: settings
            .effort
            .filter(|e| efforts.iter().any(|x| x.id == *e)),
    };
    (models, observation)
}

pub(crate) fn settings_path() -> Option<PathBuf> {
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
                .map(|p| PathBuf::from(p).join(".claude"))
        })
        .map(|p| p.join("settings.json"))
}
async fn read_settings(path: &Path) -> Option<Settings> {
    use tokio::io::AsyncReadExt;
    let file = tokio::fs::File::open(path).await.ok()?;
    let mut bytes = Vec::new();
    file.take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .await
        .ok()?;
    if bytes.len() > 1024 * 1024 {
        return None;
    }
    serde_json::from_slice(&bytes).ok()
}

fn section<'a>(help: &'a str, flag: &str) -> &'a str {
    let Some(start) = help
        .lines()
        .find(|line| line.trim_start().starts_with(flag))
    else {
        return "";
    };
    let Some(index) = help.find(start) else {
        return "";
    };
    let rest = &help[index + start.len()..];
    let end = rest.find("\n  -").unwrap_or(rest.len());
    &help[index..index + start.len() + end]
}
fn label(id: &str) -> String {
    id.split('-')
        .map(|part| {
            let mut chars = part.chars();
            chars
                .next()
                .map(|c| c.to_uppercase().collect::<String>() + chars.as_str())
                .unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}
fn effort_label(id: &str) -> String {
    match id {
        "xhigh" => "Extra high".into(),
        "max" => "Maximum".into(),
        _ => label(id),
    }
}
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-._[]".contains(&b))
}

/// The models `listed` offers, by the names their current models go by, with
/// the configured `default` among them. Without one, the default is the model
/// the CLI's own default stands for.
pub(crate) fn from_list(
    listed: Vec<Listed>,
    default: Option<&str>,
) -> (Vec<ModelOption>, Vec<EffortOption>) {
    let native = listed
        .iter()
        .find(|m| m.value == "default")
        .and_then(|m| m.resolved_model.clone());
    let chosen: Vec<_> = listed
        .into_iter()
        .filter(|m| m.value != "default" && valid_id(&m.value))
        .collect();
    let default = default.map(str::to_owned).or_else(|| {
        let native = native.as_deref()?;
        let same = |m: &&Listed| m.resolved_model.as_deref() == Some(native);
        // An alias follows its family's newest model, as the default does.
        chosen
            .iter()
            .filter(same)
            .find(|m| !m.value.starts_with("claude-"))
            .or_else(|| chosen.iter().find(same))
            .map(|m| m.value.clone())
    });
    let mut efforts: Vec<EffortOption> = Vec::new();
    for id in chosen
        .iter()
        .filter(|m| m.supports_effort)
        .flat_map(|m| &m.supported_effort_levels)
    {
        if valid_id(id) && !efforts.iter().any(|e| e.id == *id) {
            efforts.push(EffortOption {
                id: id.clone(),
                label: effort_label(id),
            });
        }
    }
    let names: Vec<_> = chosen
        .iter()
        .map(|m| model_name(m.resolved_model.as_deref().unwrap_or(&m.value)))
        .collect();
    let pins = chosen.clone();
    let mut models: Vec<ModelOption> = chosen
        .into_iter()
        .zip(&names)
        .map(|(m, name)| ModelOption {
            label: if names.iter().filter(|n| *n == name).count() > 1 {
                m.value.clone()
            } else {
                name.clone()
            },
            is_default: default.as_deref() == Some(m.value.as_str()),
            suggested_for_computer_use: false,
            details: Some(ModelDetails {
                alias: !m.value.starts_with("claude-"),
                efforts: if m.supports_effort {
                    efforts
                        .iter()
                        .filter(|e| m.supported_effort_levels.contains(&e.id))
                        .cloned()
                        .collect()
                } else if m.resolved_model.is_none() {
                    // The configured default, which the list didn't describe.
                    efforts.clone()
                } else {
                    Vec::new()
                },
                default_effort: None,
            }),
            id: m.value,
        })
        .collect();
    // Preserve the app's descriptive alias labels and expose observed exact IDs separately.
    for pin in pins {
        let Some(id) = pin
            .resolved_model
            .filter(|id| id.starts_with("claude-") && valid_id(id))
        else {
            continue;
        };
        if models.iter().any(|m| m.id == id) {
            continue;
        }
        let mut model = models
            .iter()
            .find(|m| m.id == pin.value)
            .expect("listed alias")
            .clone();
        model.id = id.clone();
        model.label = id.clone();
        model.is_default = default.as_deref() == Some(&id);
        if let Some(details) = &mut model.details {
            details.alias = false;
        }
        models.push(model);
    }
    (models, efforts)
}

pub(crate) fn parse(help: &str, default: Option<&str>) -> (Vec<ModelOption>, Vec<EffortOption>) {
    let model_help = section(help, "--model ");
    let effort_help = section(help, "--effort ");
    let efforts: Vec<_> = effort_help
        .split(['(', ')'])
        .nth(1)
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|id| valid_id(id))
        .map(|id| EffortOption {
            id: id.into(),
            label: effort_label(id),
        })
        .collect();
    // CLI help uses quoted examples. Only accept documented aliases and full names.
    let mut ids: Vec<_> = model_help
        .split_whitespace()
        .filter(|word| word.starts_with(['\'', '"']))
        .map(|word| {
            word.trim_matches(|c: char| c == '\'' || c == '"' || c == ',' || c == '.' || c == ')')
        })
        .filter(|value| valid_id(value))
        .map(str::to_owned)
        .collect();
    if let Some(default) = default.filter(|id| valid_id(id)) {
        ids.push(default.into());
    }
    ids.sort();
    ids.dedup();
    // A dated id reads like its undated twin; a label must still tell the
    // choices apart, so a shared name falls back to the ids themselves.
    let names: Vec<_> = ids.iter().map(|id| model_name(id)).collect();
    let models = ids
        .into_iter()
        .zip(&names)
        .map(|(id, name)| {
            let alias = !id.starts_with("claude-");
            ModelOption {
                label: if names.iter().filter(|n| *n == name).count() > 1 {
                    id.clone()
                } else {
                    name.clone()
                },
                is_default: default == Some(id.as_str()),
                suggested_for_computer_use: false,
                details: Some(ModelDetails {
                    alias,
                    efforts: efforts.clone(),
                    default_effort: None,
                }),
                id,
            }
        })
        .collect();
    (models, efforts)
}

#[cfg(test)]
mod tests {
    use super::*;
    const HELP: &str = "  --effort <level> Effort level (low, medium, high, xhigh, max)\n  --model <model> an alias for the latest model (e.g. 'fable', 'opus', or 'sonnet') or a model's full name (e.g. 'claude-fable-5').\n  --resume <id> resume";
    #[test]
    fn only_documented_choices_and_configured_default() {
        let (models, efforts) = parse(HELP, Some("claude-custom-20260901"));
        assert_eq!(models.len(), 5);
        assert!(!models.iter().any(|m| m.id == "haiku"));
        assert!(
            models
                .iter()
                .find(|m| m.id == "opus")
                .unwrap()
                .details
                .as_ref()
                .unwrap()
                .alias
        );
        assert!(
            models
                .iter()
                .find(|m| m.id == "claude-custom-20260901")
                .unwrap()
                .is_default
        );
        let names: Vec<_> = models.iter().map(|m| m.label.as_str()).collect();
        assert_eq!(
            names,
            [
                "claude-custom-20260901",
                "Claude Fable 5",
                "Claude Fable (latest)",
                "Claude Opus (latest)",
                "Claude Sonnet (latest)"
            ]
        );
        assert_eq!(efforts[3].label, "Extra high");
        let (twins, _) = parse(
            "  --model <model> e.g. 'claude-fable-5-1' or 'opus'",
            Some("claude-fable-5-1-20260801"),
        );
        let labels: Vec<_> = twins.iter().map(|m| m.label.as_str()).collect();
        assert_eq!(
            labels,
            [
                "claude-fable-5-1",
                "claude-fable-5-1-20260801",
                "Claude Opus (latest)"
            ]
        );
        assert!(parse("  --model <model> 'opus'", None).1.is_empty());
        assert!(
            parse("  --fallback-model <model> 'invented'", None)
                .0
                .is_empty()
        );
    }
    fn listed(value: &str, resolved: &str, efforts: &[&str]) -> Listed {
        Listed {
            value: value.into(),
            resolved_model: Some(resolved.into()),
            supports_effort: !efforts.is_empty(),
            supported_effort_levels: efforts.iter().map(|e| (*e).into()).collect(),
        }
    }
    #[test]
    fn a_listed_model_goes_by_its_current_models_name() {
        let list = || {
            vec![
                listed("default", "claude-sonnet-5-5", &["low", "high"]),
                listed("opus", "claude-opus-5-5", &["low", "high", "max"]),
                listed("sonnet", "claude-sonnet-5-5", &["low", "high"]),
                listed("haiku", "claude-haiku-5-5", &[]),
                listed("bad id!", "claude-opus-5-5", &[]),
            ]
        };
        let (models, efforts) = from_list(list(), None);
        let shown: Vec<_> = models
            .iter()
            .take(3)
            .map(|m| (m.id.as_str(), m.label.as_str(), m.is_default))
            .collect();
        assert_eq!(
            shown,
            [
                ("opus", "Claude Opus 5.5", false),
                ("sonnet", "Claude Sonnet 5.5", true),
                ("haiku", "Claude Haiku 5.5", false),
            ]
        );
        let ids: Vec<_> = efforts.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, ["low", "high", "max"]);
        assert_eq!(efforts[2].label, "Maximum");
        assert_eq!(models[1].details.as_ref().unwrap().efforts.len(), 2);
        // A configured default cannot restore a model the native list omitted.
        let (models, _) = from_list(list(), Some("claude-custom-20260901"));
        assert!(
            !models
                .iter()
                .any(|m| m.id == "claude-custom-20260901" || m.is_default)
        );
        let (models, _) = from_list(list(), Some("claude-opus-5-5"));
        assert_eq!(models[0].label, "Claude Opus 5.5");
        assert!(
            models
                .iter()
                .any(|m| m.id == "claude-opus-5-5" && m.is_default)
        );
    }
    #[tokio::test]
    async fn an_empty_native_catalog_is_not_replaced_with_help_aliases() {
        let output = std::process::Output {
            status: std::process::ExitStatus::default(),
            stdout: HELP.as_bytes().to_vec(),
            stderr: vec![],
        };
        let (models, _) = probe(Some(vec![]), Some(&output), None).await;
        assert!(models.is_empty());
    }
    #[tokio::test]
    async fn typed_settings_ignore_unrelated_fields() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(
            &path,
            r#"{"model":"opus","effortLevel":"xhigh","unrelated":"synthetic-private-value"}"#,
        )
        .unwrap();
        let settings = read_settings(&path).await.unwrap();
        assert_eq!(settings.model.as_deref(), Some("opus"));
        assert_eq!(settings.effort.as_deref(), Some("xhigh"));
    }
}
