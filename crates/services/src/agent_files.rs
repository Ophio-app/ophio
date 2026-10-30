// SPDX-License-Identifier: Apache-2.0
//! The Markdown files agents are given: their instructions, agents, skills,
//! commands, rules and memory, kept in each agent's own folder and in each
//! project. Each folder they are kept in joins Library reaching only them,
//! never the settings, credentials or past sessions beside them, and they
//! are listed by what they are and where they apply.
use crate::{CompanionHub, LauncherDirs, internal};
use ports::RootSpec;
use protocol::{
    ProjectId, ProtocolError, QueryData,
    capability::HarnessKind,
    library::{AgentFile, AgentFileGroup, LibraryRoot, RootKind},
    model::Project,
    time::Timestamp,
};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

/// How long what was found is listed before the folders are looked through
/// again.
const FRESH_FOR: Duration = Duration::from_secs(30);
/// How deep into a project its folders' own instructions are looked for, and
/// in how many of its folders.
const PROJECT_DEPTH: usize = 6;
const PROJECT_FOLDERS: usize = 1500;
/// Instructions a folder deeper in a project has for the work in it.
const NESTED: [&str; 4] = [
    "CLAUDE.md",
    "CLAUDE.local.md",
    "AGENTS.md",
    "AGENTS.override.md",
];
/// Folders tools make or fetch into, which no one keeps instructions in.
const SKIPPED: [&str; 10] = [
    "node_modules",
    "target",
    "dist",
    "build",
    "vendor",
    "out",
    "__pycache__",
    "venv",
    "graphify-out",
    "site-packages",
];
/// How deep, and through how many folders, a folder of files is read.
const FOLDER_DEPTH: usize = 8;
const LISTED_FOLDERS: usize = 3000;
/// How many files are listed at most, and how many a linked folder adds.
const LISTED_FILES: usize = 5000;
const LINKED_FILES: usize = 200;
/// How many names a folder is read for at most.
const NAMES: usize = 4096;
/// How many folders are read to find which folder a memory is for.
const SLUG_READS: usize = 2000;

/// What the files in one place are, and where they apply.
#[derive(Debug, Clone, PartialEq)]
struct Kind {
    group: AgentFileGroup,
    scope: String,
    everywhere: bool,
    project_id: Option<ProjectId>,
    harness: Option<HarnessKind>,
}

fn everywhere(group: AgentFileGroup, scope: &str, harness: Option<HarnessKind>) -> Kind {
    Kind {
        group,
        scope: scope.to_owned(),
        everywhere: true,
        project_id: None,
        harness,
    }
}

/// A file agents are given, or a folder of them, on the computer.
#[derive(Debug, Clone, PartialEq)]
struct Place {
    host: PathBuf,
    /// Where the agent finds it: `host` before any link on the way was
    /// followed. Claude Code names a linked skill, and keeps a linked
    /// folder's memory, by this.
    listed: PathBuf,
    /// What a file is shown as: this, or for a folder, where the file is in
    /// it after this.
    shown: String,
    kind: Kind,
}

/// A folder places are kept in, which joins Library reaching only them.
#[derive(Debug, Clone, PartialEq)]
struct Folder {
    label: String,
    harness: Option<HarnessKind>,
    project_id: Option<ProjectId>,
    entries: BTreeSet<String>,
}

/// Where agent files are kept on this computer, and the folders that hold
/// them.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct AgentFolders {
    places: Vec<Place>,
    folders: BTreeMap<PathBuf, Folder>,
}

/// What was last found, the roots wanted for it, and when.
#[derive(Debug, Default)]
pub(crate) struct Found {
    folders: AgentFolders,
    roots: Vec<RootSpec>,
    at: Option<Instant>,
}

/// Every place agent files are kept: each agent's own folder, then each
/// project's and the folders above it. Ophio's own agents come first, kept
/// in a root of their own.
pub(crate) fn find(dirs: &LauncherDirs, projects: &[Project], home: Option<&Path>) -> AgentFolders {
    use AgentFileGroup::*;
    use HarnessKind::{ClaudeCode, Codex, Hermes};
    let real = |path: Option<&PathBuf>| path.and_then(|p| ports::library::canonical(p).ok());
    let home = home.and_then(|home| ports::library::canonical(home).ok());
    let mut finder = Finder {
        home: home.as_deref(),
        out: AgentFolders::default(),
        folders_in: HashMap::new(),
        reads: 0,
    };
    if let Some(agents) = real(dirs.ophio_agents.as_ref()) {
        finder.out.places.push(Place {
            listed: dirs.ophio_agents.clone().unwrap_or_else(|| agents.clone()),
            host: agents,
            shown: String::new(),
            kind: everywhere(Agents, "Ophio", None),
        });
    }
    let projects: Vec<(&Project, PathBuf)> = projects
        .iter()
        .filter(|p| !p.one_off && !p.home)
        .filter_map(|p| Some((p, ports::library::canonical(Path::new(&p.host_path)).ok()?)))
        .collect();
    if let Some(claude) = real(dirs.claude_home.as_ref()) {
        for (entry, group) in [
            ("CLAUDE.md", Instructions),
            ("agents", Agents),
            ("skills", Skills),
            ("commands", Commands),
            ("rules", Rules),
            ("output-styles", Rules),
        ] {
            finder.add(
                &claude,
                "Claude Code",
                entry,
                everywhere(group, "Claude Code", Some(ClaudeCode)),
            );
        }
        finder.plugins(&claude);
        finder.memory(&claude, &projects);
    }
    if let Some(codex) = real(dirs.codex_home.as_ref()) {
        for (entry, group) in [
            ("AGENTS.md", Instructions),
            ("AGENTS.override.md", Instructions),
            ("prompts", Commands),
            ("skills", Skills),
        ] {
            finder.add(
                &codex,
                "Codex",
                entry,
                everywhere(group, "Codex", Some(Codex)),
            );
        }
    }
    if let Some(shared) = real(dirs.agents_home.as_ref()) {
        finder.add(
            &shared,
            "Agents",
            "skills",
            everywhere(Skills, "Codex", Some(Codex)),
        );
    }
    if let Some(hermes) = real(dirs.hermes_home.as_ref()) {
        for (entry, group) in [
            ("SOUL.md", Instructions),
            ("memories", Memory),
            ("skills", Skills),
        ] {
            finder.add(
                &hermes,
                "Hermes",
                entry,
                everywhere(group, "Hermes", Some(Hermes)),
            );
        }
    }
    let others: Vec<&Path> = projects.iter().map(|(_, path)| path.as_path()).collect();
    // The other names Codex is set to read a folder's instructions from.
    let fallbacks = codex_fallbacks(dirs.codex_home.as_deref());
    for (project, path) in &projects {
        let kind = |group, harness| Kind {
            group,
            scope: project.name.clone(),
            everywhere: false,
            project_id: Some(project.project_id),
            harness,
        };
        for (entry, group, harness) in [
            ("CLAUDE.md", Instructions, Some(ClaudeCode)),
            (".claude/CLAUDE.md", Instructions, Some(ClaudeCode)),
            ("CLAUDE.local.md", Instructions, Some(ClaudeCode)),
            ("AGENTS.md", Instructions, Some(Codex)),
            ("AGENTS.override.md", Instructions, Some(Codex)),
            (".hermes.md", Instructions, Some(Hermes)),
            ("HERMES.md", Instructions, Some(Hermes)),
            ("GEMINI.md", Instructions, None),
            (".claude/agents", Agents, Some(ClaudeCode)),
            (".claude/skills", Skills, Some(ClaudeCode)),
            (".claude/commands", Commands, Some(ClaudeCode)),
            (".claude/rules", Rules, Some(ClaudeCode)),
            (".claude/output-styles", Rules, Some(ClaudeCode)),
            (".agents/skills", Skills, Some(Codex)),
            (".codex/skills", Skills, Some(Codex)),
        ] {
            finder.add(path, &project.name, entry, kind(group, harness));
        }
        for name in &fallbacks {
            finder.add(path, &project.name, name, kind(Instructions, Some(Codex)));
        }
        // Deeper in, instructions for the work in one of its folders.
        for entry in nested(path, &others, &fallbacks) {
            let harness = if entry.ends_with("CLAUDE.md") || entry.ends_with("CLAUDE.local.md") {
                ClaudeCode
            } else {
                Codex
            };
            finder.add(
                path,
                &project.name,
                &entry,
                kind(Instructions, Some(harness)),
            );
        }
        // Claude Code also reads those in every folder above it.
        for above in path.ancestors().skip(1) {
            for name in ["CLAUDE.md", "CLAUDE.local.md"] {
                if std::fs::symlink_metadata(above.join(name)).is_ok() {
                    let scope = finder.display(above);
                    let kind = Kind {
                        group: Instructions,
                        scope: scope.clone(),
                        everywhere: false,
                        project_id: None,
                        harness: Some(ClaudeCode),
                    };
                    finder.add(above, &scope, name, kind);
                }
            }
        }
    }
    let mut seen = HashSet::new();
    finder
        .out
        .places
        .retain(|place| seen.insert(place.host.clone()));
    finder.out
}

struct Finder<'a> {
    home: Option<&'a Path>,
    out: AgentFolders,
    /// The folders in each folder read to find which folder a memory is for.
    folders_in: HashMap<PathBuf, Option<Vec<String>>>,
    reads: usize,
}

impl Finder<'_> {
    /// `entry` in `folder`, when it is there: a Markdown file, or a folder
    /// of them. A link is followed to where it leads, and so is each link in
    /// a folder, as skills are often kept elsewhere and linked.
    fn add(&mut self, folder: &Path, label: &str, entry: &str, kind: Kind) {
        if !nameable(entry) {
            return;
        }
        let host = under(folder, entry);
        let Ok(meta) = std::fs::symlink_metadata(&host) else {
            return;
        };
        // A link on the way leads elsewhere, as one at the end does.
        let linked_on_the_way = entry.rsplit_once('/').is_some_and(|(within, _)| {
            let within = under(folder, within);
            ports::library::canonical(&within).ok() != Some(within)
        });
        if meta.file_type().is_symlink() || linked_on_the_way {
            self.link(&host, String::new(), entry.to_owned(), kind);
        } else if meta.is_dir() {
            self.hold(folder, label, entry, &kind);
            let mut links: Vec<(PathBuf, String)> = std::fs::read_dir(&host)
                .into_iter()
                .flatten()
                .take(NAMES)
                .flatten()
                .filter(|e| e.file_type().is_ok_and(|t| t.is_symlink()))
                .filter_map(|e| Some((e.path(), e.file_name().into_string().ok()?)))
                .filter(|(_, name)| !name.starts_with('.') && nameable(name))
                .collect();
            links.sort();
            for (link, name) in links {
                self.link(&link, name.clone(), name, kind.clone());
            }
            self.out.places.push(Place {
                listed: host.clone(),
                host,
                shown: String::new(),
                kind,
            });
        } else if meta.is_file() && markdown(entry) {
            self.hold(folder, label, entry, &kind);
            self.out.places.push(Place {
                listed: host.clone(),
                host,
                shown: entry.to_owned(),
                kind,
            });
        }
    }

    /// Where `link` leads: a folder, whose Markdown files are shown after
    /// `prefix`, or a Markdown file shown as `shown`. Only those files are
    /// reached there.
    fn link(&mut self, link: &Path, prefix: String, shown: String, kind: Kind) {
        let Ok(target) = ports::library::canonical(link) else {
            return;
        };
        let Ok(meta) = std::fs::metadata(&target) else {
            return;
        };
        let name = |path: &Path| path.file_name().and_then(|n| n.to_str()).map(str::to_owned);
        if meta.is_dir() {
            let mut budget = LISTED_FOLDERS / 10;
            let files = markdown_in(&target, &mut budget, LINKED_FILES);
            if files.is_empty() {
                return;
            }
            let label = name(&target).unwrap_or_else(|| self.display(&target));
            for file in &files {
                self.hold(&target, &label, file, &kind);
            }
            self.out.places.push(Place {
                host: target,
                listed: link.to_owned(),
                shown: prefix,
                kind,
            });
        } else if meta.is_file()
            && let (Some(parent), Some(file)) = (target.parent(), name(&target))
            && markdown(&file)
            && nameable(&file)
        {
            let label = name(parent).unwrap_or_else(|| self.display(parent));
            self.hold(parent, &label, &file, &kind);
            self.out.places.push(Place {
                host: target,
                listed: link.to_owned(),
                shown,
                kind,
            });
        }
    }

    fn hold(&mut self, folder: &Path, label: &str, entry: &str, kind: &Kind) {
        let held = self
            .out
            .folders
            .entry(folder.to_owned())
            .or_insert_with(|| Folder {
                label: label.to_owned(),
                harness: kind.harness,
                project_id: kind.project_id,
                entries: BTreeSet::new(),
            });
        if held.harness != kind.harness {
            held.harness = None;
        }
        if held.project_id != kind.project_id {
            held.project_id = None;
        }
        held.entries.insert(entry.to_owned());
    }

    /// The skills, agents and commands of plugins Claude Code keeps in its
    /// own folder.
    fn plugins(&mut self, claude: &Path) {
        let file = claude.join("plugins").join("installed_plugins.json");
        let Some(json) = std::fs::metadata(&file)
            .ok()
            .filter(|meta| meta.len() <= 1 << 20)
            .and_then(|_| std::fs::read(&file).ok())
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        else {
            return;
        };
        let Some(plugins) = json.get("plugins").and_then(|p| p.as_object()) else {
            return;
        };
        for (id, installs) in plugins {
            let scope = format!("{} plugin", id.split('@').next().unwrap_or(id));
            for install in installs.as_array().into_iter().flatten() {
                let Some(within) = install
                    .get("installPath")
                    .and_then(|p| p.as_str())
                    .and_then(|p| ports::library::canonical(Path::new(p)).ok())
                    .and_then(|p| {
                        let within = p.strip_prefix(claude).ok()?.to_str()?;
                        Some(within.replace(std::path::MAIN_SEPARATOR, "/"))
                    })
                    .filter(|within| !within.is_empty())
                else {
                    continue;
                };
                for (folder, group) in [
                    ("skills", AgentFileGroup::Skills),
                    ("agents", AgentFileGroup::Agents),
                    ("commands", AgentFileGroup::Commands),
                ] {
                    let kind = everywhere(group, &scope, Some(HarnessKind::ClaudeCode));
                    self.add(claude, "Claude Code", &format!("{within}/{folder}"), kind);
                }
            }
        }
    }

    /// What Claude Code remembers about each folder it worked in, kept by a
    /// name made from the folder's path. A folder no longer there is left
    /// out, as nothing reads its memory.
    fn memory(&mut self, claude: &Path, projects: &[(&Project, PathBuf)]) {
        let Ok(names) = std::fs::read_dir(claude.join("projects")) else {
            return;
        };
        let mut names: Vec<String> = names
            .take(NAMES)
            .flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .collect();
        names.sort();
        for name in names {
            // A name linked to another is the same memory, listed under that.
            let sessions = claude.join("projects").join(&name);
            if !std::fs::symlink_metadata(&sessions).is_ok_and(|meta| meta.is_dir())
                || !std::fs::symlink_metadata(sessions.join("memory"))
                    .is_ok_and(|meta| meta.is_dir() || meta.file_type().is_symlink())
            {
                continue;
            }
            let project = projects.iter().find(|(project, real)| {
                slug(&project.host_path) == name || real.to_str().is_some_and(|r| slug(r) == name)
            });
            let (scope, project_id) = match project {
                Some((project, _)) => (project.name.clone(), Some(project.project_id)),
                None => match self.unslug(&name) {
                    Some(path) => (self.display(&path), None),
                    None => continue,
                },
            };
            let kind = Kind {
                group: AgentFileGroup::Memory,
                scope,
                everywhere: false,
                project_id,
                harness: Some(HarnessKind::ClaudeCode),
            };
            self.add(
                claude,
                "Claude Code",
                &format!("projects/{name}/memory"),
                kind,
            );
        }
    }

    /// The folder whose path Claude Code turned into `name`, found by
    /// reading the folders it could be from the top.
    fn unslug(&mut self, name: &str) -> Option<PathBuf> {
        if !cfg!(unix) {
            return None;
        }
        let mut tries = 64;
        self.unslug_from(PathBuf::from("/"), name.strip_prefix('-')?, &mut tries)
    }

    fn unslug_from(&mut self, at: PathBuf, rest: &str, tries: &mut usize) -> Option<PathBuf> {
        if rest.is_empty() {
            return Some(at);
        }
        *tries = tries.checked_sub(1)?;
        let mut next: Vec<(String, usize)> = self
            .folders_in(&at)?
            .into_iter()
            .filter_map(|name| {
                let made = slug(&name);
                (rest == made || rest.starts_with(&format!("{made}-")))
                    .then_some((name, made.len()))
            })
            .collect();
        next.sort_by_key(|(_, len)| std::cmp::Reverse(*len));
        for (name, len) in next {
            let rest = rest.get(len + 1..).unwrap_or("");
            if let Some(found) = self.unslug_from(at.join(name), rest, tries) {
                return Some(found);
            }
        }
        None
    }

    fn folders_in(&mut self, path: &Path) -> Option<Vec<String>> {
        if let Some(known) = self.folders_in.get(path) {
            return known.clone();
        }
        if self.reads == SLUG_READS {
            return None;
        }
        self.reads += 1;
        let names = std::fs::read_dir(path).ok().map(|names| {
            names
                .take(NAMES)
                .flatten()
                .filter(|e| std::fs::metadata(e.path()).is_ok_and(|m| m.is_dir()))
                .filter_map(|e| e.file_name().into_string().ok())
                .collect()
        });
        self.folders_in.insert(path.to_owned(), names.clone());
        names
    }

    /// A folder as the person knows it: in their home, from `~`.
    fn display(&self, path: &Path) -> String {
        match self.home.and_then(|home| path.strip_prefix(home).ok()) {
            Some(rest) if rest.as_os_str().is_empty() => "~".to_owned(),
            Some(rest) => format!("~/{}", rest.to_string_lossy()),
            None => path.to_string_lossy().into_owned(),
        }
    }
}

/// The Markdown files Codex is set to read a folder's instructions from
/// when it has no AGENTS.md (`project_doc_fallback_filenames` in its
/// `config.toml`), each a plain file name.
fn codex_fallbacks(codex_home: Option<&Path>) -> Vec<String> {
    let Some(file) = codex_home.map(|home| home.join("config.toml")) else {
        return Vec::new();
    };
    if std::fs::metadata(&file).map_or(true, |m| m.len() > 1 << 20) {
        return Vec::new();
    }
    let Some(config) = std::fs::read_to_string(&file)
        .ok()
        .and_then(|text| toml::from_str::<toml::Value>(&text).ok())
    else {
        return Vec::new();
    };
    let names = config
        .get("project_doc_fallback_filenames")
        .and_then(toml::Value::as_array);
    names
        .into_iter()
        .flatten()
        .filter_map(toml::Value::as_str)
        .filter(|name| markdown(name) && nameable(name) && !name.contains(['/', '\\']))
        .filter(|name| !NESTED.contains(name))
        .map(str::to_owned)
        .collect()
}

/// The name Claude Code keeps a folder's sessions and memory under: its
/// path with each character but letters and digits made `-`.
fn slug(path: &str) -> String {
    path.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

fn markdown(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name.ends_with(".md") || name.ends_with(".markdown")
}

/// Whether Library can name `relative`: each part one a file can have on
/// any computer.
fn nameable(relative: &str) -> bool {
    !relative.is_empty()
        && relative.split('/').all(|part| {
            !part.is_empty()
                && part != "."
                && part != ".."
                && !part.contains(['\\', ':', '\0'])
                && !part.ends_with(['.', ' '])
                && !part.chars().any(char::is_control)
                && !ports::library::reserved_on_windows(part)
        })
}

/// `relative`, written with `/`, inside `folder`.
fn under(folder: &Path, relative: &str) -> PathBuf {
    relative
        .split('/')
        .fold(folder.to_owned(), |path, part| path.join(part))
}

/// The instructions kept deeper in `project`, outside the folders tools
/// make and the projects inside it.
fn nested(project: &Path, others: &[&Path], fallbacks: &[String]) -> Vec<String> {
    let mut found = Vec::new();
    let mut queue = VecDeque::from([(project.to_owned(), String::new(), 0)]);
    let mut read = 0;
    while let Some((folder, within, depth)) = queue.pop_front() {
        if read == PROJECT_FOLDERS || found.len() == LINKED_FILES {
            break;
        }
        read += 1;
        let Ok(names) = std::fs::read_dir(&folder) else {
            continue;
        };
        for entry in names.take(NAMES).flatten() {
            let (Ok(name), Ok(kind)) = (entry.file_name().into_string(), entry.file_type()) else {
                continue;
            };
            let path = if within.is_empty() {
                name.clone()
            } else {
                format!("{within}/{name}")
            };
            if kind.is_dir() {
                let host = entry.path();
                if depth < PROJECT_DEPTH
                    && !name.starts_with('.')
                    && !SKIPPED.contains(&name.as_str())
                    && !others.contains(&host.as_path())
                {
                    queue.push_back((host, path, depth + 1));
                }
            } else if kind.is_file()
                && !within.is_empty()
                && (NESTED.contains(&name.as_str()) || fallbacks.contains(&name))
            {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

/// The Markdown files in `folder` and the folders in it, as paths within
/// it: at most `files`, through no more folders than `budget` allows. Links
/// aren't followed; hidden files and folders, and those tools make, are
/// passed by.
fn markdown_in(folder: &Path, budget: &mut usize, files: usize) -> Vec<String> {
    let mut found = Vec::new();
    let mut queue = VecDeque::from([(folder.to_owned(), String::new(), 0)]);
    while let Some((folder, within, depth)) = queue.pop_front() {
        if *budget == 0 {
            break;
        }
        *budget -= 1;
        let Ok(names) = std::fs::read_dir(&folder) else {
            continue;
        };
        let mut names: Vec<_> = names
            .take(NAMES)
            .flatten()
            .filter_map(|e| Some((e.file_name().into_string().ok()?, e)))
            .collect();
        names.sort_by(|a, b| a.0.cmp(&b.0));
        for (name, entry) in names {
            if name.starts_with('.') || !nameable(&name) {
                continue;
            }
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let path = if within.is_empty() {
                name.clone()
            } else {
                format!("{within}/{name}")
            };
            if kind.is_dir() && depth < FOLDER_DEPTH && !SKIPPED.contains(&name.as_str()) {
                queue.push_back((entry.path(), path, depth + 1));
            } else if kind.is_file() && markdown(&name) {
                if found.len() == files {
                    return found;
                }
                found.push(path);
            }
        }
    }
    found
}

/// The roots that let Library reach the folders agent files are kept in,
/// each limited to them. A folder already a root of `taken` is reached
/// through it, and what is in or around Ophio's own folders, or a root of
/// `taken` that can't be written, is left out.
pub(crate) fn roots(
    found: &AgentFolders,
    taken: &[RootSpec],
    private: &[PathBuf],
) -> Vec<RootSpec> {
    let overlap = |a: &Path, b: &Path| a.starts_with(b) || b.starts_with(a);
    let taken: Vec<(PathBuf, bool)> = taken
        .iter()
        .map(|r| {
            let path = ports::library::canonical(&r.path).unwrap_or_else(|_| r.path.clone());
            (path, r.writable)
        })
        .collect();
    found
        .folders
        .iter()
        .filter(|(path, _)| path.to_str().is_some() && !taken.iter().any(|(p, _)| p == *path))
        .filter_map(|(path, folder)| {
            let only: Vec<String> = (folder.entries.iter())
                .filter(|entry| {
                    let at = under(path, entry);
                    !private.iter().any(|p| overlap(p, &at))
                        && !taken
                            .iter()
                            .any(|(p, writable)| !writable && overlap(p, &at))
                })
                .cloned()
                .collect();
            (!only.is_empty()).then(|| RootSpec {
                root_id: crate::launchers::path_id(path),
                label: folder.label.clone(),
                path: path.clone(),
                kind: RootKind::AgentFiles,
                writable: true,
                project_id: folder.project_id,
                harness: folder.harness,
                only: Some(only),
            })
        })
        .collect()
}

/// Every agent file in `places` that one of `roots` holds, each once, and
/// whether there were more than could be listed.
fn list(
    places: &[Place],
    roots: &[LibraryRoot],
    dirs: &LauncherDirs,
    disabled: &[PathBuf],
) -> (Vec<AgentFile>, bool) {
    let built_in: Vec<&str> = (crate::setup_agents::setup_agents().iter())
        .map(|(id, _)| *id)
        .collect();
    let mut files = Vec::new();
    let mut seen = HashSet::new();
    let mut budget = LISTED_FOLDERS;
    let mut truncated = false;
    'places: for place in places {
        let Ok(meta) = std::fs::symlink_metadata(&place.host) else {
            continue;
        };
        let found: Vec<(PathBuf, PathBuf, String)> = if meta.is_file() {
            vec![(
                place.host.clone(),
                place.listed.clone(),
                place.shown.clone(),
            )]
        } else if meta.is_dir() {
            let room = LISTED_FILES + 1 - files.len();
            let within = markdown_in(&place.host, &mut budget, room);
            truncated |= budget == 0;
            within
                .into_iter()
                .map(|within| {
                    let shown = if place.shown.is_empty() {
                        within.clone()
                    } else {
                        format!("{}/{within}", place.shown)
                    };
                    (
                        under(&place.host, &within),
                        under(&place.listed, &within),
                        shown,
                    )
                })
                .collect()
        } else {
            continue;
        };
        for (host, listed, shown) in found {
            if files.len() == LISTED_FILES {
                truncated = true;
                break 'places;
            }
            if !seen.insert(host.clone()) {
                continue;
            }
            let (Some(at), Ok(meta)) = (
                crate::launchers::library_file(&host, roots),
                std::fs::symlink_metadata(&host),
            ) else {
                continue;
            };
            let (title, description) = match place.kind.group {
                AgentFileGroup::Instructions => (None, None),
                _ => front(&host),
            };
            let title = title.or_else(|| named(place.kind.group, &shown));
            let mut file = AgentFile {
                root_id: at.root_id,
                path: at.path,
                shown,
                title,
                description,
                group: place.kind.group,
                scope: place.kind.scope.clone(),
                everywhere: place.kind.everywhere,
                project_id: place.kind.project_id,
                harness: place.kind.harness,
                size: meta.len(),
                modified_at: meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .and_then(|d| i64::try_from(d.as_millis()).ok())
                    .map(Timestamp),
                // As the agent finds it; `disable` carries where it leads.
                host_path: listed.to_string_lossy().into_owned(),
                disable: None,
            };
            let block = crate::disabled::file_block(&file, &listed, dirs, &built_in);
            let off = disabled.contains(&host);
            file.disable = Some(crate::disabled::disable(&block, off, &host));
            files.push(file);
        }
    }
    files.sort_by(|a, b| {
        let key = |f: &AgentFile| {
            (
                f.group,
                !f.everywhere,
                f.project_id.is_none(),
                f.scope.to_lowercase(),
                f.shown.to_lowercase(),
            )
        };
        key(a).cmp(&key(b))
    });
    (files, truncated)
}

/// What a skill, agent or command is called when it doesn't say: a skill
/// by its folder, the others by their file.
fn named(group: AgentFileGroup, shown: &str) -> Option<String> {
    let mut parts = shown.rsplit('/');
    let file = parts.next()?;
    match group {
        AgentFileGroup::Skills if file.eq_ignore_ascii_case("SKILL.md") => {
            parts.next().map(str::to_owned)
        }
        AgentFileGroup::Agents | AgentFileGroup::Commands => {
            file.rsplit_once('.').map(|(stem, _)| stem.to_owned())
        }
        _ => None,
    }
}

/// The `name` and `description` a file's frontmatter gives, from its start.
fn front(path: &Path) -> (Option<String>, Option<String>) {
    use std::io::Read;
    let mut head = Vec::new();
    if std::fs::File::open(path)
        .and_then(|file| file.take(4096).read_to_end(&mut head))
        .is_err()
    {
        return (None, None);
    }
    let text = String::from_utf8_lossy(&head);
    let mut lines = text.trim_start_matches('\u{feff}').lines();
    if lines.next().map(str::trim_end) != Some("---") {
        return (None, None);
    }
    let (mut name, mut description) = (None, None);
    for line in lines {
        if line.trim_end() == "---" {
            break;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim().trim_matches(['"', '\'']).trim();
        if value.is_empty() || value.starts_with(['|', '>']) {
            continue;
        }
        let value = Some(value.chars().take(300).collect());
        match key {
            "name" => name = value,
            "description" => description = value,
            _ => {}
        }
    }
    (name, description)
}

impl CompanionHub {
    /// Look through the computer's folders for agent files, away from the
    /// work.
    pub(crate) async fn find_agent_files(&self) -> AgentFolders {
        let projects = self
            .read(|r| r.list::<Project>("project"))
            .unwrap_or_default();
        let (dirs, home) = (self.inner.launchers.clone(), self.inner.home.clone());
        tokio::task::spawn_blocking(move || find(&dirs, &projects, home.as_deref()))
            .await
            .unwrap_or_default()
    }

    /// Keep what was found, with the roots wanted for it.
    pub(crate) fn found_agent_files(
        &self,
        folders: AgentFolders,
        roots: Vec<RootSpec>,
    ) -> Result<(), ProtocolError> {
        *self.inner.agent_files.lock().map_err(internal)? = Found {
            folders,
            roots,
            at: Some(self.inner.clock.monotonic()),
        };
        Ok(())
    }

    /// Look for agent files again when they are next listed.
    pub(crate) fn forget_agent_files(&self) {
        if let Ok(mut found) = self.inner.agent_files.lock() {
            found.at = None;
        }
    }

    /// Look for agent files again, and set Library's roots again only if the
    /// folders it should reach have changed.
    pub(crate) async fn refresh_agent_files(&self) -> Result<(), ProtocolError> {
        if self
            .inner
            .configured_roots
            .lock()
            .map_err(internal)?
            .is_none()
        {
            return Ok(());
        }
        let folders = self.find_agent_files().await;
        let _change = self.inner.runtime_change.lock().await;
        let Some(roots) = self
            .inner
            .configured_roots
            .lock()
            .map_err(internal)?
            .clone()
        else {
            return Ok(());
        };
        let wanted = self.agent_file_roots(&roots, &folders);
        if self.inner.agent_files.lock().map_err(internal)?.roots == wanted {
            self.found_agent_files(folders, wanted)
        } else {
            self.apply_library_roots(roots, folders).await
        }
    }

    /// The roots for the folders agent files are kept in, beside the
    /// person's `roots` and the automatic ones.
    pub(crate) fn agent_file_roots(
        &self,
        roots: &[RootSpec],
        folders: &AgentFolders,
    ) -> Vec<RootSpec> {
        let automatic =
            crate::launchers::automatic_roots(&self.inner.launchers, roots, &self.inner.private);
        let taken: Vec<RootSpec> = roots.iter().cloned().chain(automatic).collect();
        self::roots(folders, &taken, &self.inner.private)
    }

    pub(crate) async fn agent_files(&self) -> Result<QueryData, ProtocolError> {
        let library = self.library()?.clone();
        let stale = (self.inner.agent_files.lock().map_err(internal)?.at).is_none_or(|at| {
            self.inner.clock.monotonic().saturating_duration_since(at) >= FRESH_FOR
        });
        if stale {
            self.refresh_agent_files().await?;
        }
        let places = self
            .inner
            .agent_files
            .lock()
            .map_err(internal)?
            .folders
            .places
            .clone();
        let roots = library.roots().await;
        let dirs = self.inner.launchers.clone();
        let disabled = self.disabled_paths()?;
        let (files, truncated) =
            tokio::task::spawn_blocking(move || list(&places, &roots, &dirs, &disabled))
                .await
                .map_err(|_| internal("Listing agent files"))?;
        Ok(QueryData::AgentFiles { files, truncated })
    }
}

#[cfg(test)]
#[path = "agent_files_tests.rs"]
mod tests;
