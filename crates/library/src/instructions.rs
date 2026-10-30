// SPDX-License-Identifier: Apache-2.0
use crate::{
    Core, State, decode,
    path::{Root, normalize},
    revision,
};
use ports::library::RootSpec;
use protocol::{
    capability::HarnessKind,
    ids::LibraryRootId,
    library::{InstructionScope, InstructionSource, InstructionState, RootKind},
};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

/// Host-supplied native discovery settings. No environment or credential/config
/// files are read to infer these values; the caller supplies effective settings.
#[derive(Debug, Clone)]
pub struct InstructionConfig {
    pub home: PathBuf,
    pub claude_home: PathBuf,
    pub codex_home: PathBuf,
    pub project_doc_max_bytes: usize,
    pub project_doc_fallback_filenames: Vec<String>,
    /// Optional boundary for isolated fixtures or a restricted inspector.
    pub ancestor_boundary: Option<PathBuf>,
}
impl InstructionConfig {
    pub fn new(home: PathBuf) -> Self {
        Self {
            claude_home: home.join(".claude"),
            codex_home: home.join(".codex"),
            home,
            project_doc_max_bytes: 32 * 1024,
            project_doc_fallback_filenames: Vec::new(),
            ancestor_boundary: None,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuggestedRoot {
    pub path: PathBuf,
    pub kind: RootKind,
    pub harness: HarnessKind,
}

pub(crate) fn suggested_roots(config: &InstructionConfig, project: &Path) -> Vec<SuggestedRoot> {
    let candidates = [
        (
            config.claude_home.join("skills"),
            RootKind::Skills,
            HarnessKind::ClaudeCode,
        ),
        (
            config.claude_home.join("agents"),
            RootKind::Agents,
            HarnessKind::ClaudeCode,
        ),
        (
            under(project, ".claude/skills"),
            RootKind::Skills,
            HarnessKind::ClaudeCode,
        ),
        (
            under(project, ".claude/agents"),
            RootKind::Agents,
            HarnessKind::ClaudeCode,
        ),
        (
            under(&config.home, ".agents/skills"),
            RootKind::Skills,
            HarnessKind::Codex,
        ),
        (
            under(project, ".agents/skills"),
            RootKind::Skills,
            HarnessKind::Codex,
        ),
        (
            config.codex_home.join("skills"),
            RootKind::Skills,
            HarnessKind::Codex,
        ),
    ];
    candidates
        .into_iter()
        .filter(|(p, _, _)| {
            std::fs::symlink_metadata(p).is_ok_and(|m| m.is_dir() && !crate::path::is_link(&m))
        })
        .map(|(path, kind, harness)| SuggestedRoot {
            path,
            kind,
            harness,
        })
        .collect()
}

struct Inspector<'a> {
    core: &'a Core,
    state: &'a State,
    harness: HarnessKind,
    loaded: &'a [PathBuf],
    out: Vec<InstructionSource>,
    visited: HashSet<PathBuf>,
}
impl Inspector<'_> {
    fn observe(
        &self,
        path: &Path,
    ) -> (
        InstructionState,
        Option<Vec<u8>>,
        Option<LibraryRootId>,
        Option<String>,
    ) {
        for root in self.state.roots.values() {
            if let Some((_, relative)) = root.within(path) {
                let read = root
                    .parent(&relative)
                    .and_then(|(dir, name)| {
                        crate::path::read_file(&dir, &name, self.core.config.max_file_bytes)
                    })
                    .map(|p| p.0);
                return observation(read, Some(root.spec.root_id), Some(relative));
            }
        }
        // The instruction inspector may read only documented instruction/resource
        // names in explicitly supplied native homes. It never reads auth/config.
        let config = &self.core.config.instructions;
        for home in [
            &config.claude_home,
            &config.codex_home,
            &under(&config.home, ".agents/skills"),
        ] {
            if let Ok(relative) = path.strip_prefix(home)
                && let Some(relative) = relative.to_str()
                && relative.to_ascii_lowercase().ends_with(".md")
                && let Ok(relative) = normalize(&relative.replace(std::path::MAIN_SEPARATOR, "/"))
            {
                let read = Root::new(RootSpec {
                    root_id: LibraryRootId::new(),
                    label: String::new(),
                    path: home.to_owned(),
                    kind: RootKind::Instructions,
                    writable: false,
                    project_id: None,
                    harness: Some(self.harness),
                    only: None,
                })
                .and_then(|r| r.parent(&relative))
                .and_then(|(dir, name)| {
                    crate::path::read_file(&dir, &name, self.core.config.max_file_bytes)
                })
                .map(|p| p.0);
                return observation(read, None, None);
            }
        }
        // Elsewhere, such as the project's ancestors above every Library root,
        // a candidate is only checked for existence and never opened: an absent
        // one is missing, and one that is present stays unknown.
        match std::fs::symlink_metadata(path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                (InstructionState::Missing, None, None, None)
            }
            _ => (InstructionState::Unknown, None, None, None),
        }
    }

    /// `applies` is false for a source runs of this project do not read.
    fn add(
        &mut self,
        path: PathBuf,
        scope: InstructionScope,
        label: String,
        depth: usize,
        imports: bool,
        applies: bool,
    ) -> Option<usize> {
        if self.out.len() >= 1024 || !self.visited.insert(path.clone()) {
            return None;
        }
        let (mut state, bytes, root_id, relative) = self.observe(&path);
        if self.loaded.iter().any(|p| p == &path) {
            state = InstructionState::Loaded;
        } else if !applies && state != InstructionState::Missing {
            state = InstructionState::NotApplied;
        }
        let len = bytes.as_ref().map(Vec::len);
        self.out.push(InstructionSource {
            label,
            root_id,
            path: relative,
            host_path: path.to_str().map(str::to_owned),
            harness: self.harness,
            scope,
            state,
            revision_at_start: None,
            current_revision: bytes.as_ref().map(|b| revision(b)),
            size: len.map(|n| n as u64),
            loaded_reported: self.loaded.iter().any(|p| p == &path),
            disabled: false,
        });
        if imports
            && depth < 4
            && let Some(bytes) = bytes
            && let Ok((text, _, _)) = decode(&bytes)
        {
            for import in imports_in(text) {
                let import = if let Some(relative) = import.strip_prefix("~/") {
                    self.core.config.instructions.home.join(relative)
                } else if Path::new(&import).is_absolute() {
                    PathBuf::from(import)
                } else {
                    path.parent().unwrap_or(Path::new("")).join(import)
                };
                // Resolve lexical parent components without following a symlink.
                let Some(import) = lexical_absolute(&import) else {
                    continue;
                };
                self.add(
                    import,
                    scope,
                    "Imported instructions".into(),
                    depth + 1,
                    true,
                    applies,
                );
            }
        }
        len
    }

    /// A settings file is listed by whether it exists; it is never read.
    fn setting(&mut self, path: PathBuf, label: &str, applies: bool) {
        if self.out.len() >= 1024 || !self.visited.insert(path.clone()) {
            return;
        }
        let state = match std::fs::metadata(&path) {
            Ok(metadata) if metadata.is_file() && applies => InstructionState::Discovered,
            Ok(metadata) if metadata.is_file() => InstructionState::NotApplied,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => InstructionState::Missing,
            _ => InstructionState::Unknown,
        };
        self.out.push(InstructionSource {
            label: label.into(),
            root_id: None,
            path: None,
            host_path: path.to_str().map(str::to_owned),
            harness: self.harness,
            scope: InstructionScope::Settings,
            state,
            revision_at_start: None,
            current_revision: None,
            size: None,
            loaded_reported: false,
            disabled: false,
        });
    }

    fn resources(&mut self, directory: &Path, scope: InstructionScope, applies: bool) {
        let mut anchor = self
            .state
            .roots
            .values()
            .find(|r| r.within(directory).is_some())
            .cloned();
        if anchor.is_none() {
            let config = &self.core.config.instructions;
            for home in [
                &config.claude_home,
                &config.codex_home,
                &under(&config.home, ".agents/skills"),
            ] {
                if directory.starts_with(home) {
                    anchor = Root::new(RootSpec {
                        root_id: LibraryRootId::new(),
                        label: String::new(),
                        path: home.to_owned(),
                        kind: RootKind::Instructions,
                        writable: false,
                        project_id: None,
                        harness: Some(self.harness),
                        only: None,
                    })
                    .ok()
                    .map(std::sync::Arc::new);
                    break;
                }
            }
        }
        let Some(root) = anchor else { return };
        let Some((base, relative)) = root.within(directory) else {
            return;
        };
        let mut pending = vec![(relative, 0)];
        let mut count = 0;
        let mut paths = Vec::new();
        while let Some((relative, depth)) = pending.pop() {
            if count >= 4096 {
                break;
            }
            let Ok(dir) = root.directory(&relative) else {
                continue;
            };
            let Ok(entries) = dir.entries() else { continue };
            for entry in entries.take(4096 - count).flatten() {
                count += 1;
                let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                    continue;
                };
                let Ok(meta) = dir.symlink_metadata(&name) else {
                    continue;
                };
                let path = if relative.is_empty() {
                    name.clone()
                } else {
                    format!("{relative}/{name}")
                };
                if meta.file_type().is_symlink() {
                    continue;
                }
                if meta.is_dir() && depth < 16 {
                    pending.push((path, depth + 1));
                } else if meta.is_file()
                    && ((scope == InstructionScope::Skill && name == "SKILL.md")
                        || (scope == InstructionScope::Agent && name.ends_with(".md")))
                {
                    paths.push(under(&base, &path));
                }
            }
        }
        paths.sort();
        for path in paths {
            self.add(
                path,
                scope,
                match scope {
                    InstructionScope::Skill => "Available skill",
                    _ => "Agent definition",
                }
                .into(),
                0,
                false,
                applies,
            );
        }
    }
}

fn observation(
    result: Result<Vec<u8>, ports::library::LibraryError>,
    root: Option<LibraryRootId>,
    path: Option<String>,
) -> (
    InstructionState,
    Option<Vec<u8>>,
    Option<LibraryRootId>,
    Option<String>,
) {
    match result {
        Ok(bytes) => (InstructionState::Discovered, Some(bytes), root, path),
        Err(ports::library::LibraryError::NotFound) => {
            (InstructionState::Missing, None, root, path)
        }
        Err(_) => (InstructionState::Unknown, None, root, path),
    }
}

pub(crate) fn sources(
    core: &Core,
    state: &State,
    project: &Path,
    harness: HarnessKind,
    loaded: &[PathBuf],
    project_settings: bool,
) -> Vec<InstructionSource> {
    let Some(project) = lexical_absolute(project) else {
        return Vec::new();
    };
    let config = &core.config.instructions;
    let mut inspector = Inspector {
        core,
        state,
        harness,
        loaded,
        out: Vec::new(),
        visited: HashSet::new(),
    };
    let mut ancestors: Vec<_> = project
        .ancestors()
        .take(128)
        .take_while(|p| {
            config
                .ancestor_boundary
                .as_ref()
                .is_none_or(|bound| p.starts_with(bound))
        })
        .map(Path::to_owned)
        .collect();
    let repo = ancestors
        .iter()
        .find(|p| p.join(".git").exists())
        .cloned()
        .unwrap_or_else(|| project.clone());
    ancestors.reverse();
    match harness {
        HarnessKind::ClaudeCode => {
            inspector.add(
                config.claude_home.join("CLAUDE.md"),
                InstructionScope::User,
                "User instructions".into(),
                0,
                true,
                true,
            );
            for directory in &ancestors {
                let scope = if directory == &repo {
                    InstructionScope::Project
                } else {
                    InstructionScope::Directory
                };
                for name in ["CLAUDE.md", ".claude/CLAUDE.md", "CLAUDE.local.md"] {
                    // Without the project's settings, Claude Code reads only the
                    // project folder's own shared instructions.
                    let applies =
                        project_settings || (directory == &project && name != "CLAUDE.local.md");
                    inspector.add(under(directory, name), scope, name.into(), 0, true, applies);
                }
            }
            inspector.resources(
                &config.claude_home.join("skills"),
                InstructionScope::Skill,
                true,
            );
            inspector.resources(
                &config.claude_home.join("agents"),
                InstructionScope::Agent,
                true,
            );
            for directory in ancestors.iter().filter(|p| p.starts_with(&repo)) {
                inspector.resources(
                    &under(directory, ".claude/skills"),
                    InstructionScope::Skill,
                    project_settings,
                );
                inspector.resources(
                    &under(directory, ".claude/agents"),
                    InstructionScope::Agent,
                    project_settings,
                );
            }
            for (name, label) in [
                (".claude/settings.json", "Project settings"),
                (".claude/settings.local.json", "Local project settings"),
            ] {
                inspector.setting(under(&project, name), label, project_settings);
            }
        }
        HarnessKind::Codex => {
            let mut remaining = config.project_doc_max_bytes;
            let dirs = std::iter::once((config.codex_home.clone(), InstructionScope::User)).chain(
                ancestors
                    .into_iter()
                    .filter(|p| p.starts_with(&repo))
                    .map(|p| {
                        let scope = if p == repo {
                            InstructionScope::Project
                        } else {
                            InstructionScope::Directory
                        };
                        (p, scope)
                    }),
            );
            for (directory, scope) in dirs {
                let mut candidates = vec!["AGENTS.override.md".to_owned(), "AGENTS.md".to_owned()];
                if scope != InstructionScope::User {
                    candidates.extend(
                        config
                            .project_doc_fallback_filenames
                            .iter()
                            .filter(|n| normalize(n).is_ok_and(|v| v == **n && !v.contains('/')))
                            .cloned(),
                    );
                }
                // A candidate that exists but can't be read may be the one Codex
                // uses, so it is shown rather than a missing fallback.
                let selected = candidates
                    .iter()
                    .map(|name| directory.join(name))
                    .find(|p| match inspector.observe(p) {
                        (_, Some(bytes), ..) => !bytes.is_empty(),
                        (state, None, ..) => state == InstructionState::Unknown,
                    });
                let path = selected.unwrap_or_else(|| directory.join("AGENTS.md"));
                let length = inspector.observe(&path).1.map(|b| b.len()).unwrap_or(0);
                let used = length.min(remaining);
                let label = if length > used {
                    format!("Instructions ({used} of {length} bytes within configured limit)")
                } else {
                    "Instructions".into()
                };
                let index = inspector.out.len();
                inspector.add(path, scope, label, 0, false, true);
                if remaining == 0
                    && length > 0
                    && let Some(item) = inspector.out.get_mut(index)
                    && item.state != InstructionState::Loaded
                {
                    item.state = InstructionState::Unknown;
                }
                remaining = remaining.saturating_sub(used);
            }
        }
        _ => {
            // No guessed native discovery rules for terminal-only harnesses.
            for path in loaded {
                inspector.add(
                    path.clone(),
                    InstructionScope::Directory,
                    "Harness-reported source".into(),
                    0,
                    false,
                    true,
                );
            }
        }
    }
    for path in loaded {
        if !inspector.visited.contains(path) {
            inspector.add(
                path.clone(),
                InstructionScope::Directory,
                "Harness-reported source".into(),
                0,
                false,
                true,
            );
        }
    }
    inspector.out
}

/// `relative`, written with `/`, inside `directory`. Each part is joined on
/// its own, so a Windows path the phone shows uses one separator throughout.
fn under(directory: &Path, relative: &str) -> PathBuf {
    relative
        .split('/')
        .fold(directory.to_owned(), |path, part| path.join(part))
}

fn lexical_absolute(path: &Path) -> Option<PathBuf> {
    if !path.is_absolute() || path.to_str().is_none() {
        return None;
    }
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                if !out.pop() {
                    return None;
                }
            }
            std::path::Component::CurDir => {}
            other => out.push(other),
        }
    }
    Some(out)
}

/// Conservative import tokenizer: skip fenced and inline code, accept standalone
/// @paths and quoted paths, never treat email addresses as imports.
fn imports_in(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut fenced = false;
    let mut comment = false;
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            continue;
        }
        if trimmed.starts_with("<!--") {
            comment = true;
        }
        if comment {
            if line.contains("-->") {
                comment = false;
            }
            continue;
        }
        let mut code = false;
        let chars: Vec<_> = line.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            if chars[i] == '`' {
                code = !code;
            }
            if !code
                && chars[i] == '@'
                && (i == 0 || chars[i - 1].is_whitespace() || chars[i - 1] == '(')
            {
                i += 1;
                let quoted = chars.get(i) == Some(&'"');
                if quoted {
                    i += 1;
                }
                let start = i;
                while i < chars.len()
                    && if quoted {
                        chars[i] != '"'
                    } else {
                        !chars[i].is_whitespace() && !matches!(chars[i], ')' | '`' | ',' | ';')
                    }
                {
                    i += 1;
                }
                let path: String = chars[start..i].iter().collect();
                if !path.is_empty() {
                    out.push(path);
                }
                // What ended the path is read next: a backtick still opens or
                // closes a code span.
                continue;
            }
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod import_tests {
    use super::imports_in;

    // The character that ends an import's path is read like any other, so a
    // backtick right after one still opens or closes a code span.
    #[test]
    fn the_character_after_an_import_still_counts() {
        assert_eq!(imports_in("@a.md`x` @b.md"), ["a.md", "b.md"]);
        assert_eq!(imports_in("@a.md` @b.md`"), ["a.md"]);
        assert_eq!(imports_in("(@a.md) @b.md; `@c.md`"), ["a.md", "b.md"]);
        assert_eq!(imports_in("@\"with space.md\"`@c.md`"), ["with space.md"]);
    }
}
