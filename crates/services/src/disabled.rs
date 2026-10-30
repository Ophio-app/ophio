// SPDX-License-Identifier: Apache-2.0
//! Disable in Ophio (D-071): skills, agents and agent files the person keeps
//! from every agent Ophio runs. The block is Ophio's own, kept in its state:
//! each run is told what to leave out (see [`ports::Disabled`]), no task
//! starts from a disabled entry, and the files and the agents' own settings
//! are never changed, so every other way of running the agents still finds
//! them.
//!
//! What can be hidden, and how, depends on where an item is kept and which
//! agent reads it; [`Block`] says how each is kept from the agents, and an
//! item that can't be is listed with the reason.
use crate::{CompanionHub, LauncherDirs, error};
use protocol::{
    ErrorCode, ProtocolError,
    capability::HarnessKind,
    disabled::{Disable, DisabledItem, DisabledKind},
    launchers::{CatalogEntry, CatalogKind, CatalogSource},
    library::{AgentFile, AgentFileGroup, LibraryRoot},
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Where the list is kept: `("meta", STORED)`.
const STORED: &str = "disabled_in_ophio";
/// Items kept at most.
const MAX_ITEMS: usize = 1000;

pub(crate) const BUILT_IN: &str =
    "Built into Øphio, whose own setup and GitHub work start from it.";
pub(crate) const PLUGIN: &str =
    "Claude Code can hide a plugin’s skills and commands only by turning the whole plugin off.";
pub(crate) const PART_OF_SKILL: &str =
    "Part of a skill. Disable the skill itself, from its SKILL.md.";
pub(crate) const NOT_RUN: &str = "No agent Øphio runs reads this file.";
pub(crate) const CODEX_GLOBAL: &str = "Codex always reads this from its own folder, so Øphio would have to change that folder to hide it.";
pub(crate) const CODEX_PROMPTS: &str =
    "Agents never see Codex prompts. Only you can send one, by typing its name in Codex.";
pub(crate) const MEMORY_TOPIC: &str = "Claude Code finds this through the MEMORY.md beside it. Disable that to keep this folder’s memory from agents in Øphio.";

/// How Ophio keeps an item from its agents.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "by", rename_all = "snake_case")]
pub(crate) enum Block {
    /// A Claude Code skill or command, by the name Claude Code lists it as.
    ClaudeSkill { name: String },
    /// A Claude Code subagent, by its name.
    ClaudeAgent { name: String },
    /// A Claude Code instruction or rule file.
    ClaudeFile,
    /// Claude Code's memory of one folder, by the name Claude Code keeps it
    /// under.
    ClaudeMemory { slug: String },
    /// A Claude Code output style, by its name.
    ClaudeOutputStyle { name: String },
    /// A Codex skill, by its `SKILL.md`.
    CodexSkill { name: String },
    /// One of a project's AGENTS.md files.
    CodexDoc,
    /// One of Ophio's own agents, which no agent lists itself: Ophio never
    /// starts from it.
    Ophio,
}

impl Block {
    fn kind(&self) -> DisabledKind {
        match self {
            Block::ClaudeSkill { .. } | Block::CodexSkill { .. } => DisabledKind::Skill,
            Block::ClaudeAgent { .. } | Block::Ophio => DisabledKind::Agent,
            Block::ClaudeFile
            | Block::ClaudeMemory { .. }
            | Block::ClaudeOutputStyle { .. }
            | Block::CodexDoc => DisabledKind::File,
        }
    }
}

/// One disabled item as kept: what is shown of it, and how it is blocked.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct Stored {
    pub item: DisabledItem,
    pub block: Block,
    /// The other paths it was listed at, before links were followed. An
    /// agent may know it by any of them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub listed: Vec<String>,
}

/// What can be disabled of a catalog entry, as what kind, and how.
pub(crate) fn entry_block(entry: &CatalogEntry) -> Result<Block, &'static str> {
    use CatalogKind::*;
    use CatalogSource::*;
    if entry.copy.is_some() || entry.source == OphioSetup {
        return Err(BUILT_IN);
    }
    Ok(match (entry.kind, entry.source) {
        (Agent, Ophio) => Block::Ophio,
        (Agent, ClaudeUser | ClaudeProject) => Block::ClaudeAgent {
            name: entry.name.clone(),
        },
        (Skill, ClaudeUser | ClaudeProject) => Block::ClaudeSkill {
            name: entry.id.clone(),
        },
        (Skill, CodexUser | CodexProject) => Block::CodexSkill {
            name: entry.name.clone(),
        },
        (Skill, Ophio) | (Agent, CodexUser | CodexProject) | (_, OphioSetup) => {
            return Err(BUILT_IN);
        }
    })
}

/// What can be disabled of an agent file `listed` where the agent finds it
/// (before any link is followed: Claude Code names a linked skill, and keeps
/// a linked folder's memory, by that path), as what kind, and how.
/// `built_in_agents` are the ids of Øphio's built-in agents, whose copies
/// are kept with Ophio's own.
pub(crate) fn file_block(
    file: &AgentFile,
    listed: &Path,
    dirs: &LauncherDirs,
    built_in_agents: &[&str],
) -> Result<Block, &'static str> {
    let host = listed;
    use AgentFileGroup::*;
    let name = host.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let stem = name.rsplit_once('.').map_or(name, |(stem, _)| stem);
    let plugin = file.scope.ends_with(" plugin");
    let plugin_name = plugin.then(|| file.scope.trim_end_matches(" plugin").to_owned());
    // An output style is a file directly in Claude Code's own
    // `output-styles` folder or a project's `.claude/output-styles`.
    let style = host.parent().is_some_and(|folder| {
        let homes = dirs.claude_home.iter().flat_map(|h| [h.clone(), real(h)]);
        homes.into_iter().any(|h| folder == h.join("output-styles"))
            || folder.ends_with(Path::new(".claude").join("output-styles"))
    });
    match (file.group, file.harness) {
        (_, Some(HarnessKind::Hermes)) | (_, Some(HarnessKind::OpenClaw | HarnessKind::Pi)) => {
            Err(NOT_RUN)
        }
        (Skills, Some(harness)) => {
            if !name.eq_ignore_ascii_case("SKILL.md") {
                return Err(PART_OF_SKILL);
            }
            let folder = host
                .parent()
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str())
                .unwrap_or_default()
                .to_owned();
            match harness {
                HarnessKind::ClaudeCode if plugin => Err(PLUGIN),
                HarnessKind::ClaudeCode => Ok(Block::ClaudeSkill { name: folder }),
                HarnessKind::Codex => Ok(Block::CodexSkill {
                    name: file.title.clone().unwrap_or(folder),
                }),
                _ => Err(NOT_RUN),
            }
        }
        (Agents, None) if file.scope == "Ophio" => {
            if built_in_agents.contains(&stem) {
                Err(BUILT_IN)
            } else {
                Ok(Block::Ophio)
            }
        }
        (Agents, Some(HarnessKind::ClaudeCode)) => {
            let own = file.title.clone().unwrap_or_else(|| stem.to_owned());
            Ok(Block::ClaudeAgent {
                name: match plugin_name {
                    Some(plugin) => format!("{plugin}:{own}"),
                    None => own,
                },
            })
        }
        (Commands, Some(HarnessKind::ClaudeCode)) => {
            if plugin {
                return Err(PLUGIN);
            }
            // A command in a folder is listed by the folder too.
            let shown = file.shown.trim_end_matches(".md");
            Ok(Block::ClaudeSkill {
                name: shown.replace('/', ":"),
            })
        }
        (Commands, Some(HarnessKind::Codex)) => Err(CODEX_PROMPTS),
        (Instructions, Some(HarnessKind::ClaudeCode)) => Ok(Block::ClaudeFile),
        (Instructions, Some(HarnessKind::Codex)) if file.everywhere => Err(CODEX_GLOBAL),
        (Instructions, Some(HarnessKind::Codex)) => Ok(Block::CodexDoc),
        (Rules, Some(HarnessKind::ClaudeCode)) if style => Ok(Block::ClaudeOutputStyle {
            name: file.title.clone().unwrap_or_else(|| stem.to_owned()),
        }),
        (Rules, Some(HarnessKind::ClaudeCode)) => Ok(Block::ClaudeFile),
        (Memory, Some(HarnessKind::ClaudeCode)) => {
            // `projects/<slug>/memory/MEMORY.md` in Claude Code's folder.
            let slug = host
                .parent()
                .filter(|memory| memory.file_name().is_some_and(|n| n == "memory"))
                .and_then(Path::parent)
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str());
            match slug {
                Some(slug) if name == "MEMORY.md" => Ok(Block::ClaudeMemory {
                    slug: slug.to_owned(),
                }),
                _ => Err(MEMORY_TOPIC),
            }
        }
        _ => Err(NOT_RUN),
    }
}

/// `block` as what the phone is told, given whether it is disabled.
pub(crate) fn disable(block: &Result<Block, &str>, disabled: bool, key: &Path) -> Disable {
    match block {
        Ok(block) => Disable::Can {
            kind: block.kind(),
            disabled,
            key: Some(key.to_string_lossy().into_owned()),
        },
        Err(reason) => Disable::Cannot {
            reason: (*reason).to_owned(),
        },
    }
}

/// A path as it is compared: where it leads, when it can be followed.
pub(crate) fn real(path: &Path) -> PathBuf {
    ports::library::canonical(path).unwrap_or_else(|_| path.to_path_buf())
}

/// The name Claude Code keeps a folder's sessions and memory under.
fn slug(path: &str) -> String {
    path.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// `outputStyle` in a Claude Code settings file, when it sets one.
fn style_in(file: &Path) -> Option<String> {
    let meta = std::fs::metadata(file).ok()?;
    if meta.len() > 1 << 20 {
        return None;
    }
    let value: serde_json::Value = serde_json::from_slice(&std::fs::read(file).ok()?).ok()?;
    value.get("outputStyle")?.as_str().map(str::to_owned)
}

/// The output style a Claude Code run in `cwd` uses: the project's local
/// settings, then its committed settings, when the run reads them, then the
/// person's own.
fn output_style(dirs: &LauncherDirs, cwd: &Path, project_settings: bool) -> Option<String> {
    let project = [".claude/settings.local.json", ".claude/settings.json"]
        .into_iter()
        .filter(|_| project_settings)
        .map(|file| cwd.join(file));
    let user = dirs
        .claude_home
        .iter()
        .map(|home| home.join("settings.json"));
    project.chain(user).find_map(|file| style_in(&file))
}

/// The folder Claude Code keeps a run's memory for: the main working tree
/// of the git repository `cwd` is in (a worktree's too), or `cwd`.
pub(crate) fn memory_root(cwd: &Path) -> PathBuf {
    for dir in cwd.ancestors() {
        let git = dir.join(".git");
        let Ok(meta) = std::fs::symlink_metadata(&git) else {
            continue;
        };
        if meta.is_dir() {
            return dir.to_path_buf();
        }
        // A worktree's `.git` names its folder in the main repository's.
        if let Ok(text) = std::fs::read_to_string(&git)
            && let Some(gitdir) = text.lines().find_map(|l| l.strip_prefix("gitdir:"))
        {
            let gitdir = dir.join(gitdir.trim());
            let common = match std::fs::read_to_string(gitdir.join("commondir")) {
                Ok(common) => gitdir.join(common.trim()),
                Err(_) => gitdir,
            };
            let common = real(&common);
            if common.file_name().is_some_and(|n| n == ".git")
                && let Some(main) = common.parent()
            {
                return main.to_path_buf();
            }
        }
        return dir.to_path_buf();
    }
    cwd.to_path_buf()
}

/// Where a run is, and what it reads, for working out what applies to it.
pub(crate) struct RunPlace<'a> {
    pub cwd: &'a Path,
    /// Every project whose folder holds the run's folder.
    pub project_ids: Vec<protocol::ProjectId>,
    /// Claude Code reads the project's own settings.
    pub project_settings: bool,
}

/// Every path a kept item may be known by: as kept, as listed, and with any
/// of the agents' own folders or the run's folder written as it was set
/// rather than where it leads (`bases` pairs where one leads with how it
/// was set).
fn known_as(stored: &Stored, bases: &[(PathBuf, PathBuf)]) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    let mut add = |p: PathBuf| {
        if !out.contains(&p) {
            out.push(p);
        }
    };
    for path in std::iter::once(&stored.item.host_path).chain(&stored.listed) {
        let path = PathBuf::from(path);
        for (resolved, set) in bases {
            if let Ok(rest) = path.strip_prefix(resolved) {
                add(set.join(rest));
            }
        }
        add(path);
    }
    out
}

/// Whether a file at one of `paths` can be read by a run in `cwd`: it is in
/// an agent's own folder, in a folder above the run, or in the run's own.
fn reaches(paths: &[PathBuf], cwd: &Path, homes: &[PathBuf]) -> bool {
    let cwd_real = real(cwd);
    paths.iter().any(|p| {
        homes.iter().any(|h| p.starts_with(h))
            || p.parent().is_some_and(|folder| {
                [cwd, cwd_real.as_path()]
                    .iter()
                    .any(|c| c.starts_with(folder) || folder.starts_with(c))
            })
    })
}

/// What a run is kept from: only what applies to it, so a long list never
/// makes its command line too long.
pub(crate) fn for_run(
    stored: &[Stored],
    at: &RunPlace<'_>,
    dirs: &LauncherDirs,
) -> ports::Disabled {
    let mut out = ports::Disabled::default();
    let cwd = at.cwd;
    let set: Vec<PathBuf> = [&dirs.claude_home, &dirs.codex_home, &dirs.agents_home]
        .into_iter()
        .flatten()
        .cloned()
        .collect();
    let bases: Vec<(PathBuf, PathBuf)> = set
        .iter()
        .chain(std::iter::once(&cwd.to_path_buf()))
        .map(|p| (real(p), p.clone()))
        .filter(|(resolved, p)| resolved != p)
        .collect();
    let homes: Vec<PathBuf> = set.iter().flat_map(|h| [h.clone(), real(h)]).collect();
    let root = memory_root(cwd);
    let slugs: Vec<String> = [root.clone(), real(&root)]
        .iter()
        .filter_map(|p| p.to_str().map(slug))
        .collect();
    let mut style = None;
    let folders = SkillFolders::of(cwd, dirs);
    for s in stored {
        // Every other name and path a skill or command is reachable by
        // from this run, whichever was listed when it was disabled.
        match &s.block {
            Block::ClaudeSkill { .. } | Block::CodexSkill { .. } => {
                folders.aliases(Path::new(&s.item.host_path), &mut out);
            }
            _ => {}
        }
        // Another project's own skill, agent or file isn't this run's.
        if s.item.project_id.is_some()
            && !s
                .item
                .project_id
                .is_some_and(|id| at.project_ids.contains(&id))
        {
            continue;
        }
        let paths = known_as(s, &bases);
        if !paths.iter().any(|p| p.exists()) {
            continue;
        }
        let entries = |name: &str, harness| {
            let name = name.to_owned();
            paths.iter().map(move |path| ports::DisabledEntry {
                path: path.clone(),
                name: name.clone(),
                harness: Some(harness),
            })
        };
        match &s.block {
            Block::ClaudeSkill { name } => {
                out.skills.extend(entries(name, HarnessKind::ClaudeCode))
            }
            Block::CodexSkill { name } => out.skills.extend(entries(name, HarnessKind::Codex)),
            Block::ClaudeAgent { name } => {
                out.agents.extend(entries(name, HarnessKind::ClaudeCode))
            }
            Block::ClaudeFile | Block::CodexDoc => {
                if reaches(&paths, cwd, &homes) {
                    out.files.extend(paths.iter().cloned());
                }
            }
            Block::ClaudeMemory { slug } => out.memory |= slugs.contains(slug),
            Block::ClaudeOutputStyle { name } => {
                let chosen =
                    style.get_or_insert_with(|| output_style(dirs, cwd, at.project_settings));
                out.output_style |= chosen.as_deref() == Some(name.as_str());
            }
            Block::Ophio => {}
        }
    }
    dedupe(&mut out.skills);
    dedupe(&mut out.agents);
    out
}

fn dedupe(entries: &mut Vec<ports::DisabledEntry>) {
    let mut seen = Vec::new();
    entries.retain(|e| {
        let key = (e.path.clone(), e.name.clone(), e.harness);
        let new = !seen.contains(&key);
        if new {
            seen.push(key);
        }
        new
    });
}

/// The folders a run finds skills and commands in, by the agent that reads
/// them: Claude Code's own and the project's, Codex's own, `~/.agents` and
/// the project's from the repository's root down to the run's folder.
struct SkillFolders {
    claude_skills: Vec<PathBuf>,
    claude_commands: Vec<PathBuf>,
    codex_skills: Vec<PathBuf>,
}

impl SkillFolders {
    fn of(cwd: &Path, dirs: &LauncherDirs) -> Self {
        let both = |p: PathBuf| {
            let resolved = real(&p);
            if resolved == p {
                vec![p]
            } else {
                vec![p, resolved]
            }
        };
        let home = |base: &Option<PathBuf>, rest: &str| {
            base.iter()
                .flat_map(|b| both(b.join(rest)))
                .collect::<Vec<_>>()
        };
        let root = memory_root(cwd);
        let up: Vec<PathBuf> = cwd
            .ancestors()
            .take_while(|d| d.starts_with(&root))
            .map(Path::to_path_buf)
            .collect();
        let mut codex_skills = home(&dirs.codex_home, "skills");
        codex_skills.extend(home(&dirs.codex_home, "skills/.system"));
        codex_skills.extend(home(&dirs.agents_home, "skills"));
        for dir in &up {
            codex_skills.extend(both(dir.join(".agents/skills")));
            codex_skills.extend(both(dir.join(".codex/skills")));
        }
        let mut claude_skills = home(&dirs.claude_home, "skills");
        claude_skills.extend(both(cwd.join(".claude/skills")));
        let mut claude_commands = home(&dirs.claude_home, "commands");
        claude_commands.extend(both(cwd.join(".claude/commands")));
        Self {
            claude_skills,
            claude_commands,
            codex_skills,
        }
    }

    /// Each name and path in these folders that leads to the same skill
    /// folder, or command file, as `kept`, for the agent that reads it.
    fn aliases(&self, kept: &Path, out: &mut ports::Disabled) {
        let target = real(kept);
        let skill = target
            .file_name()
            .is_some_and(|n| n.eq_ignore_ascii_case("SKILL.md"));
        let entries = |folder: &Path| -> Vec<(String, PathBuf)> {
            std::fs::read_dir(folder)
                .into_iter()
                .flatten()
                .flatten()
                .filter_map(|e| Some((e.file_name().into_string().ok()?, e.path())))
                .collect()
        };
        let push = |out: &mut Vec<ports::DisabledEntry>, name: String, path: PathBuf, harness| {
            out.push(ports::DisabledEntry {
                path,
                name,
                harness: Some(harness),
            });
        };
        if skill {
            let Some(folder) = target.parent() else {
                return;
            };
            for dir in &self.claude_skills {
                for (name, path) in entries(dir) {
                    if real(&path) == folder {
                        push(
                            &mut out.skills,
                            name,
                            path.join("SKILL.md"),
                            HarnessKind::ClaudeCode,
                        );
                    }
                }
            }
            for dir in &self.codex_skills {
                for (name, path) in entries(dir) {
                    if real(&path) == folder {
                        push(
                            &mut out.skills,
                            name,
                            path.join("SKILL.md"),
                            HarnessKind::Codex,
                        );
                    }
                }
            }
        } else {
            for dir in &self.claude_commands {
                for (file, path) in entries(dir) {
                    if real(&path) == target
                        && let Some(stem) = file.strip_suffix(".md")
                    {
                        push(
                            &mut out.skills,
                            stem.to_owned(),
                            path,
                            HarnessKind::ClaudeCode,
                        );
                    }
                }
            }
        }
    }
}

impl CompanionHub {
    pub(crate) fn disabled_stored(r: &store::Read<'_>) -> store::Result<Vec<Stored>> {
        Ok(r.get("meta", STORED)?.unwrap_or_default())
    }

    /// The host paths disabled in Ophio, as compared.
    pub(crate) fn disabled_paths(&self) -> Result<Vec<PathBuf>, ProtocolError> {
        Ok(self
            .read(Self::disabled_stored)?
            .into_iter()
            .map(|s| PathBuf::from(s.item.host_path))
            .collect())
    }

    /// What a run at `at` is kept from.
    pub(crate) fn disabled_for_run(
        &self,
        at: &RunPlace<'_>,
    ) -> Result<ports::Disabled, ProtocolError> {
        let stored = self.read(Self::disabled_stored)?;
        if stored.is_empty() {
            return Ok(ports::Disabled::default());
        }
        let dirs = self.inner.launchers.clone();
        Ok(for_run(&stored, at, &dirs))
    }

    /// Every project whose folder holds `cwd`, where the paths lead.
    pub(crate) fn projects_at(
        &self,
        cwd: &Path,
    ) -> Result<Vec<protocol::ProjectId>, ProtocolError> {
        let at = real(cwd);
        let projects: Vec<protocol::model::Project> = self.read(|r| r.list("project"))?;
        Ok(projects
            .into_iter()
            .filter(|p| {
                let root = Path::new(&p.host_path);
                at.starts_with(real(root)) || cwd.starts_with(root)
            })
            .map(|p| p.project_id)
            .collect())
    }

    /// The arguments an agent started in a terminal in `cwd` is given. A
    /// terminal reads the project's settings, as Claude Code does by itself.
    /// What applies is worked out from the folder itself: every project
    /// holding it, whatever project the request named.
    pub(crate) fn terminal_block(
        &self,
        kind: HarnessKind,
        cwd: &Path,
    ) -> Result<Vec<String>, ProtocolError> {
        let disabled = self.disabled_for_run(&RunPlace {
            cwd,
            project_ids: self.projects_at(cwd)?,
            project_settings: true,
        })?;
        if disabled.is_empty() {
            return Ok(Vec::new());
        }
        let block = self.inner.terminal_block.as_ref().ok_or_else(|| {
            error(
                ErrorCode::Unsupported,
                "This computer can't start an agent in a terminal with what is disabled in Øphio kept from it.",
            )
        })?;
        (block.0)(kind, cwd, &disabled).map_err(|e| {
            error(
                ErrorCode::Internal,
                format!("Couldn’t keep what is disabled in Øphio from the terminal’s agent: {e}"),
            )
        })
    }

    /// Refuse a start from an agent or skill disabled in Ophio.
    pub(crate) fn refuse_disabled_start(
        &self,
        start: &ports::RunStart,
    ) -> Result<(), ProtocolError> {
        match start {
            ports::RunStart::Agent { name, file, .. } if !file.as_os_str().is_empty() => {
                self.refuse_disabled(file, name)
            }
            ports::RunStart::Skill {
                name, skill_file, ..
            } => self.refuse_disabled(skill_file, name),
            _ => Ok(()),
        }
    }

    /// Refuse what starts from `path` while it is disabled.
    pub(crate) fn refuse_disabled(&self, path: &Path, name: &str) -> Result<(), ProtocolError> {
        let path = real(path);
        if self.disabled_paths()?.contains(&path) {
            return Err(error(
                ErrorCode::Conflict,
                format!("“{name}” is disabled in Øphio. Enable it in Library to start from it."),
            ));
        }
        Ok(())
    }

    /// Every item disabled in Ophio, newest first, with where Library opens
    /// it now and whether it is still there.
    pub(crate) async fn disabled_items(&self) -> Result<Vec<DisabledItem>, ProtocolError> {
        let stored = self.read(Self::disabled_stored)?;
        let roots: Vec<LibraryRoot> = match &self.inner.library {
            Some(library) => library.roots().await,
            None => Vec::new(),
        };
        let mut items = tokio::task::spawn_blocking(move || {
            stored
                .into_iter()
                .map(|s| {
                    let mut item = s.item;
                    let path = Path::new(&item.host_path);
                    item.found = std::fs::metadata(path).is_ok_and(|m| m.is_file());
                    item.library = crate::launchers::library_file(path, &roots);
                    item
                })
                .collect::<Vec<_>>()
        })
        .await
        .map_err(|_| crate::internal("Reading what is disabled"))?;
        items.sort_by_key(|item| std::cmp::Reverse(item.disabled_at));
        Ok(items)
    }

    /// What disabling `host_path` as `kind` keeps, found among what Library
    /// lists now: its agent files, then the catalog, Home's and each
    /// project's.
    pub(crate) async fn disable_target(
        &self,
        kind: DisabledKind,
        host_path: &str,
    ) -> Result<Stored, ProtocolError> {
        let wanted = real(Path::new(host_path));
        let refused = |reason: &str| error(ErrorCode::InvalidRequest, reason.to_owned());
        let now = self.inner.clock.now();
        let make = |block: Block, item: DisabledItem, listed: Vec<String>| {
            if block.kind() != kind {
                return Err(refused(
                    "That isn’t what it was listed as. Look again, then try.",
                ));
            }
            Ok(Stored {
                item,
                block,
                listed,
            })
        };
        // An agent file. Without Library there are none, only the catalog.
        if let Ok(protocol::QueryData::AgentFiles { files, .. }) = self.agent_files().await
            && let Some(file) = files
                .iter()
                .find(|f| real(Path::new(&f.host_path)) == wanted)
        {
            let dirs = self.inner.launchers.clone();
            let built_in: Vec<&str> = (crate::setup_agents::setup_agents().iter())
                .map(|(id, _)| *id)
                .collect();
            let listed = PathBuf::from(&file.host_path);
            let block = file_block(file, &listed, &dirs, &built_in).map_err(refused)?;
            let others = other_paths(&wanted, &[host_path, &file.host_path]);
            let item = DisabledItem {
                kind,
                host_path: wanted.to_string_lossy().into_owned(),
                name: file
                    .title
                    .clone()
                    .unwrap_or_else(|| crate::disabled::shown_name(file)),
                description: file.description.clone(),
                scope: file.scope.clone(),
                harness: file.harness,
                project_id: file.project_id,
                // As it was listed: a Claude Code command is a skill to
                // Claude Code, and still a command.
                group: Some(file.group),
                library: None,
                disabled_at: now,
                found: true,
            };
            return make(block, item, others);
        }
        // A catalog entry: Home's, then each project's.
        let projects: Vec<protocol::model::Project> =
            self.read(|r| r.list("project")).unwrap_or_default();
        let ids = std::iter::once(None).chain(
            projects
                .iter()
                .filter(|p| !p.home)
                .map(|p| Some(p.project_id)),
        );
        for project_id in ids {
            let Ok(catalog) = self.catalog(project_id).await else {
                continue;
            };
            let Some(entry) = catalog
                .entries
                .iter()
                .find(|e| real(Path::new(&e.host_path)) == wanted)
            else {
                continue;
            };
            let block = entry_block(entry).map_err(refused)?;
            let scope = match entry.source {
                CatalogSource::ClaudeUser => "Claude Code".to_owned(),
                CatalogSource::CodexUser => "Codex".to_owned(),
                CatalogSource::Ophio => "Ophio".to_owned(),
                _ => project_id
                    .and_then(|id| projects.iter().find(|p| p.project_id == id))
                    .map(|p| p.name.clone())
                    .unwrap_or_default(),
            };
            let item = DisabledItem {
                kind,
                host_path: wanted.to_string_lossy().into_owned(),
                name: entry.name.clone(),
                description: entry.description.clone(),
                scope,
                harness: entry.harness,
                project_id: project_id.filter(|_| {
                    matches!(
                        entry.source,
                        CatalogSource::ClaudeProject | CatalogSource::CodexProject
                    )
                }),
                group: None,
                library: None,
                disabled_at: now,
                found: true,
            };
            let listed = other_paths(&wanted, &[host_path, &entry.host_path]);
            return make(block, item, listed);
        }
        Err(error(
            ErrorCode::NotFound,
            "Library doesn’t list that now. Look again, then try.",
        ))
    }

    /// Keep `target` from every agent Ophio runs, or stop keeping
    /// `host_path` from them.
    pub(crate) fn set_disabled(
        tx: &mut store::Write<'_>,
        host_path: &str,
        target: Option<Stored>,
    ) -> store::Result<()> {
        let mut stored = Self::disabled_stored(&tx.read())?;
        let path = real(Path::new(host_path));
        // As kept, as listed, or where it leads now: a file gone since can
        // still be enabled by the path it was kept at.
        let was = (stored.iter())
            .position(|s| {
                s.item.host_path == host_path
                    || Path::new(&s.item.host_path) == path
                    || s.listed.iter().any(|l| l == host_path)
            })
            .map(|at| stored.remove(at));
        match (target, was) {
            // Disabled again, it keeps when it was first disabled.
            (Some(mut target), Some(was)) => {
                target.item.disabled_at = was.item.disabled_at;
                stored.push(target);
            }
            (Some(target), None) => {
                if stored.len() >= MAX_ITEMS {
                    return Err(error(
                        ErrorCode::LimitExceeded,
                        "Øphio keeps at most 1,000 disabled items. Enable some first.",
                    )
                    .into());
                }
                stored.push(target);
            }
            (None, Some(_)) => {}
            (None, None) => {
                return Err(error(
                    ErrorCode::NotFound,
                    "That isn’t disabled in Øphio now. Look again.",
                )
                .into());
            }
        }
        tx.put("meta", STORED, &stored)
    }
}

impl CompanionHub {
    /// The host path of `path` in Library's root `root_id`.
    async fn library_host(&self, root_id: protocol::LibraryRootId, path: &str) -> Option<PathBuf> {
        let roots = self.inner.library.as_ref()?.roots().await;
        let root = roots.iter().find(|r| r.root_id == root_id)?;
        Some(real(&Path::new(&root.host_path).join(path)))
    }

    /// Whether `path` in Library's root `root_id` is disabled in Ophio, or
    /// is in a disabled skill's folder.
    async fn library_disabled(
        &self,
        disabled: &[PathBuf],
        root_id: protocol::LibraryRootId,
        path: &str,
    ) -> bool {
        let Some(host) = self.library_host(root_id, path).await else {
            return false;
        };
        held(disabled, &host)
    }

    pub(crate) async fn query_for_agent(
        &self,
        caller: &crate::api::Caller,
        query: protocol::Query,
    ) -> Result<protocol::QueryData, ProtocolError> {
        use protocol::{Query, QueryData};
        // Agents may reuse an owner caller in this path. Reporter-supplied
        // code is only for the person, regardless of disabled-path settings.
        if matches!(query, Query::InboxReportCode { .. }) {
            return Err(crate::error(
                protocol::ErrorCode::PermissionDenied,
                "Only the person may read report code",
            ));
        }
        if matches!(query, Query::Snapshot { .. }) {
            let mut data = self.query_impl(caller, query).await?;
            if let QueryData::Snapshot(snapshot) = &mut data {
                snapshot
                    .attention
                    .retain(|item| !crate::report_inbox::is_inbox(item));
            }
            return Ok(data);
        }
        let disabled = self.disabled_paths()?;
        if disabled.is_empty() {
            return self.query_impl(caller, query).await;
        }
        let refused = match &query {
            Query::LibraryRead { root_id, path } => {
                self.library_disabled(&disabled, *root_id, path).await
            }
            // A deleted copy is matched by where it was.
            Query::LibraryTrashRead { trash_id } => match self.trashed(trash_id).await {
                Some((root_id, path)) => self.library_disabled(&disabled, root_id, &path).await,
                None => false,
            },
            _ => false,
        };
        if refused {
            return Err(kept());
        }
        let off = |d: &Option<Disable>| matches!(d, Some(Disable::Can { disabled: true, .. }));
        Ok(match self.query_impl(caller, query).await? {
            QueryData::Catalog { mut catalog } => {
                catalog.entries.retain(|e| !off(&e.disable));
                QueryData::Catalog { catalog }
            }
            QueryData::AgentFiles {
                mut files,
                truncated,
            } => {
                files.retain(|f| {
                    !off(&f.disable) && !held(&disabled, &real(Path::new(&f.host_path)))
                });
                QueryData::AgentFiles { files, truncated }
            }
            QueryData::Instructions { mut sources } => {
                sources.retain(|s| !s.disabled);
                QueryData::Instructions { sources }
            }
            QueryData::LibrarySearch { hits } => {
                let mut kept = Vec::with_capacity(hits.len());
                for hit in hits {
                    if !self
                        .library_disabled(&disabled, hit.root_id, &hit.path)
                        .await
                    {
                        kept.push(hit);
                    }
                }
                QueryData::LibrarySearch { hits: kept }
            }
            QueryData::LibraryList { mut listing } => {
                let mut kept = Vec::with_capacity(listing.entries.len());
                for entry in std::mem::take(&mut listing.entries) {
                    if !self
                        .library_disabled(&disabled, listing.root_id, &entry.path)
                        .await
                    {
                        kept.push(entry);
                    }
                }
                listing.entries = kept;
                QueryData::LibraryList { listing }
            }
            QueryData::LibraryTrash { items } => {
                let mut kept = Vec::with_capacity(items.len());
                for item in items {
                    if !self
                        .library_disabled(&disabled, item.root_id, &item.path)
                        .await
                    {
                        kept.push(item);
                    }
                }
                QueryData::LibraryTrash { items: kept }
            }
            QueryData::LibraryRecoveryList { items } => {
                let mut kept = Vec::with_capacity(items.len());
                for item in items {
                    if !self
                        .library_disabled(&disabled, item.root_id, &item.path)
                        .await
                    {
                        kept.push(item);
                    }
                }
                QueryData::LibraryRecoveryList { items: kept }
            }
            QueryData::LibraryRecoveryRead { recovery }
                if self
                    .library_disabled(&disabled, recovery.item.root_id, &recovery.item.path)
                    .await =>
            {
                return Err(kept());
            }
            other => other,
        })
    }

    /// Where the deleted copy `trash_id` was, in Library.
    async fn trashed(&self, trash_id: &str) -> Option<(protocol::LibraryRootId, String)> {
        let items = self.inner.library.as_ref()?.recoverable().await.ok()?;
        let item = items.into_iter().find(|i| i.trash_id == trash_id)?;
        Some((item.root_id, item.path))
    }

    pub(crate) async fn refusal_for_agent(
        &self,
        command: &protocol::Command,
    ) -> Option<ProtocolError> {
        use protocol::Command::*;
        use protocol::command::DownloadSource;
        let disabled = self.disabled_paths().ok()?;
        if disabled.is_empty() {
            return None;
        }
        let touched: Vec<(protocol::LibraryRootId, String)> = match command {
            LibrarySave { root_id, path, .. }
            | LibraryDelete { root_id, path, .. }
            | LibraryCreate { root_id, path, .. } => vec![(*root_id, path.clone())],
            // Back where it was, or as a copy beside it.
            LibraryRestore {
                trash_id,
                copy_path,
                ..
            } => match self.trashed(trash_id).await {
                Some((root_id, path)) => {
                    let mut all = vec![(root_id, path)];
                    all.extend(copy_path.iter().map(|copy| (root_id, copy.clone())));
                    all
                }
                None => return None,
            },
            CreateDownload {
                source: DownloadSource::ProjectFile { project_id, path },
                ..
            } => {
                let project: protocol::model::Project =
                    self.read(|r| r.get("project", *project_id)).ok()??;
                let host = real(&Path::new(&project.host_path).join(path));
                return held(&disabled, &host).then(kept);
            }
            LibraryRename {
                root_id, from, to, ..
            } => {
                vec![(*root_id, from.clone()), (*root_id, to.clone())]
            }
            CreateDownload {
                source: DownloadSource::LibraryFile { root_id, path },
                ..
            } => vec![(*root_id, path.clone())],
            // A recovery copy is matched by the file it is of.
            LibraryRecoveryDiscard {
                root_id,
                recovery_id,
            } => {
                let items = self.inner.library.as_ref()?.list_recovery().await.ok()?;
                vec![recovered(items, *root_id, recovery_id)?]
            }
            _ => return None,
        };
        for (root_id, path) in touched {
            if self.library_disabled(&disabled, root_id, &path).await {
                return Some(kept());
            }
        }
        None
    }
}

/// Which file the recovery copy `recovery_id` in `root_id` is of.
fn recovered(
    items: Vec<protocol::library::LibraryRecoveryItem>,
    root_id: protocol::LibraryRootId,
    recovery_id: &str,
) -> Option<(protocol::LibraryRootId, String)> {
    let item =
        (items.into_iter()).find(|i| i.root_id == root_id && i.recovery_id == recovery_id)?;
    Some((item.root_id, item.path))
}

/// Whether `host` is disabled, or is in the folder of a disabled skill.
fn held(disabled: &[PathBuf], host: &Path) -> bool {
    disabled.iter().any(|d| {
        d == host
            || (d
                .file_name()
                .is_some_and(|n| n.eq_ignore_ascii_case("SKILL.md"))
                && d.parent().is_some_and(|folder| host.starts_with(folder)))
    })
}

/// What an agent is told of a file the person disabled in Ophio.
fn kept() -> ProtocolError {
    error(
        ErrorCode::PermissionDenied,
        "The person disabled this in Øphio, so agents can’t use it through Øphio.",
    )
}

/// `paths` that aren't `kept`, once each.
fn other_paths(kept: &Path, paths: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for p in paths {
        if Path::new(p) != kept && !out.iter().any(|o| o == p) {
            out.push((*p).to_owned());
        }
    }
    out
}

/// What an agent file is called when its frontmatter doesn't say: a skill
/// by its folder, another file by its name.
pub(crate) fn shown_name(file: &AgentFile) -> String {
    let mut parts = file.shown.rsplit('/');
    let last = parts.next().unwrap_or(&file.shown);
    if last.eq_ignore_ascii_case("SKILL.md")
        && let Some(folder) = parts.next()
    {
        return folder.to_owned();
    }
    last.to_owned()
}

#[cfg(test)]
#[path = "disabled_tests.rs"]
mod tests;
