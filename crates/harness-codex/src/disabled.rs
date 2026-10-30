// SPDX-License-Identifier: Apache-2.0
//! Per-process Codex exclusions; project documents stay on disk untouched.
use crate::toml_string;
use ports::{Disabled, RunSpec};
use protocol::capability::HarnessKind;
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

const DEFAULT_DOCUMENT_BUDGET: usize = 32 * 1024;
#[cfg(unix)]
const MAX_DEVELOPER_ARG_BYTES: usize = 100 * 1024;
#[cfg(windows)]
const MAX_DEVELOPER_ARG_BYTES: usize = 24 * 1024;
#[cfg(not(any(unix, windows)))]
const MAX_DEVELOPER_ARG_BYTES: usize = 24 * 1024;

fn config(home: Option<&Path>) -> Option<toml::Value> {
    home.and_then(|home| fs::read_to_string(home.join("config.toml")).ok())
        .and_then(|text| toml::from_str(&text).ok())
}

fn personal_developer_instructions(config: Option<&toml::Value>) -> &str {
    config
        .and_then(|config| config.get("developer_instructions"))
        .and_then(toml::Value::as_str)
        .unwrap_or("")
}

fn fitting_prefix<'a>(text: &'a str, prefix: &str) -> &'a str {
    // Codex budgets project docs at UTF-8 boundaries; do the same after TOML
    // escaping, since Linux limits each argv entry and Windows the command line.
    let boundaries: Vec<usize> = text
        .char_indices()
        .map(|(index, _)| index)
        .chain(std::iter::once(text.len()))
        .collect();
    let mut low = 0;
    let mut high = boundaries.len();
    while low + 1 < high {
        let mid = (low + high) / 2;
        let candidate = format!(
            "developer_instructions={}",
            toml_string(&format!("{prefix}{}", &text[..boundaries[mid]]))
        );
        if candidate.len() < MAX_DEVELOPER_ARG_BYTES {
            low = mid;
        } else {
            high = mid;
        }
    }
    &text[..boundaries[low]]
}

fn developer_argument(personal: &str, chain: &str) -> Option<String> {
    if personal.is_empty() && chain.is_empty() {
        return None;
    }
    let fitted_personal = fitting_prefix(personal, "");
    if fitted_personal.len() != personal.len() {
        return Some(format!(
            "developer_instructions={}",
            toml_string(fitted_personal)
        ));
    }
    let separator = if personal.is_empty() || chain.is_empty() {
        ""
    } else {
        "\n\n"
    };
    let chain = fitting_prefix(chain, &format!("{personal}{separator}"));
    let combined = if chain.is_empty() {
        personal.to_owned()
    } else {
        format!("{personal}{separator}{chain}")
    };
    Some(format!("developer_instructions={}", toml_string(&combined)))
}

fn codex_home() -> Option<PathBuf> {
    std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".codex")))
}

fn doc_settings(home: Option<&Path>) -> (usize, Vec<String>) {
    let config = config(home);
    let budget = config
        .as_ref()
        .and_then(|value| value.get("project_doc_max_bytes"))
        .and_then(toml::Value::as_integer)
        .and_then(|number| usize::try_from(number).ok())
        .unwrap_or(DEFAULT_DOCUMENT_BUDGET);
    let fallbacks = config
        .as_ref()
        .and_then(|value| value.get("project_doc_fallback_filenames"))
        .and_then(toml::Value::as_array)
        .map(|array| {
            array
                .iter()
                .filter_map(toml::Value::as_str)
                .filter(|name| !name.is_empty() && Path::new(name).components().count() == 1)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    (budget, fallbacks)
}

fn skill_arguments(disabled: &Disabled, args: &mut Vec<String>) {
    let mut seen = HashSet::new();
    let skills: Vec<_> = disabled
        .skills
        .iter()
        .filter(|entry| entry.harness.is_none_or(|kind| kind == HarnessKind::Codex))
        .filter(|entry| entry.path.is_file())
        // A linked skill is given by each path it may be found at: Codex
        // matches the path it found it by.
        .filter(|entry| seen.insert(entry.path.clone()))
        .map(|entry| {
            format!(
                "{{path={},enabled=false}}",
                toml_string(&entry.path.to_string_lossy())
            )
        })
        .collect();
    if !skills.is_empty() {
        args.extend(["-c".into(), format!("skills.config=[{}]", skills.join(","))]);
    }
}

pub(crate) fn arguments(spec: &RunSpec, args: &mut Vec<String>) {
    skill_arguments(&spec.disabled, args);
    if document_chain(spec).is_some() {
        args.extend(["-c".into(), "project_doc_max_bytes=0".into()]);
    }
}

/// Per-process overrides for an interactive Codex terminal. Unlike app-server
/// task instructions, terminals receive the restored chain via Codex config.
pub fn terminal_arguments(
    cwd: &Path,
    disabled: &Disabled,
) -> Result<Vec<String>, ports::HarnessError> {
    terminal_arguments_at(cwd, disabled, codex_home().as_deref())
}

/// [`terminal_arguments`] with Codex's own folder given, for checks that
/// keep away from the person's.
#[doc(hidden)]
pub fn terminal_arguments_at(
    cwd: &Path,
    disabled: &Disabled,
    home: Option<&Path>,
) -> Result<Vec<String>, ports::HarnessError> {
    if !cwd.is_dir() {
        return Err(ports::HarnessError::Protocol(
            "Codex working directory does not exist".into(),
        ));
    }
    let mut args = Vec::new();
    skill_arguments(disabled, &mut args);
    if let Some(chain) = document_chain_at(cwd, disabled, home) {
        args.extend(["-c".into(), "project_doc_max_bytes=0".into()]);
        if let Some(argument) = developer_argument(
            personal_developer_instructions(config(home).as_ref()),
            &chain,
        ) {
            args.extend(["-c".into(), argument]);
        }
    }
    Ok(args)
}

/// Returns kept project docs only when an active doc in the loading chain is disabled.
pub(crate) fn document_chain(spec: &RunSpec) -> Option<String> {
    document_chain_at(&spec.cwd, &spec.disabled, codex_home().as_deref())
}

fn document_chain_at(cwd: &Path, disabled: &Disabled, home: Option<&Path>) -> Option<String> {
    let cwd = cwd.canonicalize().ok()?;
    let (budget, fallbacks) = doc_settings(home);
    if budget == 0 {
        return None;
    }
    let root = cwd
        .ancestors()
        .find(|dir| dir.join(".git").exists())
        .unwrap_or(cwd.as_path());
    let mut dirs: Vec<_> = cwd.ancestors().take_while(|dir| *dir != root).collect();
    dirs.push(root);
    dirs.reverse();
    let disabled_paths: HashSet<_> = disabled
        .files
        .iter()
        .filter_map(|path| path.canonicalize().ok())
        .collect();
    let candidates: Vec<_> = dirs
        .iter()
        .map(|dir| {
            let names = ["AGENTS.override.md", "AGENTS.md"]
                .into_iter()
                .chain(fallbacks.iter().map(String::as_str));
            let files: Vec<_> = names
                .enumerate()
                .map(|(index, name)| (index, dir.join(name)))
                .filter(|(index, path)| {
                    fs::metadata(path)
                        .is_ok_and(|meta| meta.is_file() && (*index != 0 || meta.len() > 0))
                })
                .map(|(_, path)| path)
                .collect();
            (*dir, files)
        })
        .collect();
    let mut remaining = budget;
    let mut affected = false;
    let mut text = String::new();
    for (dir, files) in candidates {
        // Codex chooses the first available document for each directory; a
        // disabled candidate within the byte budget requires rebuilding it.
        let original = files.first();
        if remaining == 0 {
            break;
        }
        if original.is_some_and(|path| is_disabled(path, &disabled_paths)) {
            affected = true;
        }
        let chosen = files
            .into_iter()
            .find(|path| !is_disabled(path, &disabled_paths));
        if let Some(doc) = chosen.and_then(|path| fs::read(path).ok()) {
            let length = doc.len().min(remaining);
            let boundary =
                std::str::from_utf8(&doc[..length]).map_or_else(|err| err.valid_up_to(), str::len);
            let content = String::from_utf8_lossy(&doc[..boundary]);
            if !text.is_empty() {
                text.push_str("\n\n");
            }
            text.push_str(&format!(
                "# AGENTS.md instructions for {}\n\n<INSTRUCTIONS>\n{}\n</INSTRUCTIONS>",
                dir.display(),
                content
            ));
            remaining -= length;
        }
    }
    affected.then_some(text)
}

fn is_disabled(path: &Path, disabled: &HashSet<PathBuf>) -> bool {
    path.canonicalize()
        .is_ok_and(|path| disabled.contains(&path))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn personal_instructions_precede_chain() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.toml"),
            "developer_instructions = 'MINE'",
        )
        .unwrap();
        let parsed = config(Some(dir.path()));
        let own = personal_developer_instructions(parsed.as_ref());
        assert_eq!(own, "MINE");
        let arg = developer_argument(own, "CHAIN").unwrap();
        let parsed: toml::Value = toml::from_str(&arg).unwrap();
        assert_eq!(
            parsed["developer_instructions"].as_str(),
            Some("MINE\n\nCHAIN")
        );
    }
    #[test]
    fn developer_argument_limits_escaped_bytes_without_cutting_personal_text() {
        let personal = "MINE-\\\"é";
        let chain = "界\\\n".repeat(MAX_DEVELOPER_ARG_BYTES);
        let arg = developer_argument(personal, &chain).unwrap();
        assert!(arg.len() < MAX_DEVELOPER_ARG_BYTES);
        let value: toml::Value = toml::from_str(&arg).unwrap();
        let text = value["developer_instructions"].as_str().unwrap();
        assert!(text.starts_with(&format!("{personal}\n\n")));
        assert!(chain.starts_with(text.strip_prefix(&format!("{personal}\n\n")).unwrap()));
        let oversized_personal = "é".repeat(MAX_DEVELOPER_ARG_BYTES);
        let arg = developer_argument(&oversized_personal, "NEVER-INCLUDED").unwrap();
        assert!(arg.len() < MAX_DEVELOPER_ARG_BYTES);
        let value: toml::Value = toml::from_str(&arg).unwrap();
        let text = value["developer_instructions"].as_str().unwrap();
        assert!(oversized_personal.starts_with(text));
        assert!(!text.contains("NEVER-INCLUDED"));
    }
    #[test]
    fn configured_zero_never_disables_project_docs() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("config.toml"), "project_doc_max_bytes = 0").unwrap();
        let cwd = dir.path().join("project");
        fs::create_dir(&cwd).unwrap();
        let doc = cwd.join("AGENTS.md");
        fs::write(&doc, "secret").unwrap();
        let disabled = Disabled {
            files: vec![doc],
            ..Disabled::default()
        };
        assert_eq!(document_chain_at(&cwd, &disabled, Some(dir.path())), None);
    }
    #[test]
    fn skills_filter_missing_foreign_and_duplicate_entries() {
        let dir = tempfile::tempdir().unwrap();
        let skill = dir.path().join("SKILL.md");
        fs::write(&skill, "skill").unwrap();
        let entry = |path: PathBuf, harness| ports::DisabledEntry {
            path,
            name: "test".into(),
            harness,
        };
        let disabled = Disabled {
            skills: vec![
                entry(skill.clone(), None),
                entry(skill.clone(), Some(HarnessKind::Codex)),
                entry(dir.path().join("missing.md"), None),
                entry(skill, Some(HarnessKind::ClaudeCode)),
            ],
            ..Disabled::default()
        };
        let mut args = Vec::new();
        skill_arguments(&disabled, &mut args);
        assert_eq!(args.len(), 2);
        assert_eq!(args[1].matches("enabled=false").count(), 1);
    }
    #[test]
    fn terminal_arguments_restore_chain_and_dedupe_skills() {
        let dir = tempfile::tempdir().unwrap();
        let skill = dir.path().join("SKILL.md");
        fs::write(&skill, "skill").unwrap();
        let doc = dir.path().join("AGENTS.md");
        fs::write(&doc, "kept").unwrap();
        let override_doc = dir.path().join("AGENTS.override.md");
        fs::write(&override_doc, "blocked").unwrap();
        let entry = ports::DisabledEntry {
            path: skill.clone(),
            name: "test".into(),
            harness: Some(HarnessKind::Codex),
        };
        let disabled = Disabled {
            skills: vec![entry.clone(), entry],
            files: vec![override_doc],
            ..Disabled::default()
        };
        let args = terminal_arguments_at(dir.path(), &disabled, Some(dir.path())).unwrap();
        assert_eq!(
            args.iter()
                .filter(|arg| arg.starts_with("skills.config="))
                .count(),
            1
        );
        assert!(args.contains(&"project_doc_max_bytes=0".into()));
        assert!(
            args.iter()
                .any(|arg| arg.starts_with("developer_instructions=")
                    && arg.contains("kept")
                    && !arg.contains("blocked"))
        );
    }
    #[test]
    fn fallback_doc_when_disabled_triggers_rebuild() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.toml"),
            "project_doc_fallback_filenames = [\"CLAUDE.md\"]",
        )
        .unwrap();
        let cwd = dir.path().join("project");
        fs::create_dir(&cwd).unwrap();
        let doc = cwd.join("CLAUDE.md");
        fs::write(&doc, "blocked").unwrap();
        let disabled = Disabled {
            files: vec![doc],
            ..Disabled::default()
        };
        assert_eq!(
            document_chain_at(&cwd, &disabled, Some(dir.path())),
            Some(String::new())
        );
    }
    #[test]
    fn configured_budget_and_fallback_chain() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.toml"),
            "project_doc_max_bytes = 4\nproject_doc_fallback_filenames = [\"CLAUDE.md\"]",
        )
        .unwrap();
        let cwd = dir.path().join("project");
        fs::create_dir(&cwd).unwrap();
        fs::write(cwd.join("AGENTS.override.md"), "blocked").unwrap();
        fs::write(cwd.join("AGENTS.md"), "éabcdef").unwrap();
        fs::write(cwd.join("CLAUDE.md"), "fallback").unwrap();
        let disabled = Disabled {
            files: vec![cwd.join("AGENTS.override.md")],
            ..Disabled::default()
        };
        let chain = document_chain_at(&cwd, &disabled, Some(dir.path())).unwrap();
        assert!(chain.contains("éab"));
        assert!(!chain.contains("éabc") && !chain.contains("fallback"));
        let disabled = Disabled {
            files: vec![cwd.join("AGENTS.override.md"), cwd.join("AGENTS.md")],
            ..Disabled::default()
        };
        assert!(
            document_chain_at(&cwd, &disabled, Some(dir.path()))
                .unwrap()
                .contains("fall")
        );
    }
}
