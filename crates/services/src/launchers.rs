// SPDX-License-Identifier: Apache-2.0
//! Agents and skills a task can start from. They are read from the computer's
//! own folders each time the catalog is asked for, and read again when a task
//! starts, which is refused if the file is no longer what was shown.
use crate::setup_agents::{setup_agent_text, setup_agents};
use crate::{CompanionHub, error, internal};
use ports::{NativeEntry, RootSpec, RunStart};
use protocol::{
    ErrorCode, ProjectId, ProtocolError, TaskId,
    capability::HarnessKind,
    command::CommandResult,
    launchers::*,
    library::{LibraryRoot, RootKind},
    model::{LaunchPolicy, Project, Task},
};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    path::{Component, Path, PathBuf},
    sync::Arc,
};

/// Where the person's agents and skills live on this computer. A folder left
/// unset is not looked in.
#[derive(Debug, Clone, Default)]
pub struct LauncherDirs {
    /// Claude Code's own folder: `~/.claude`, or `CLAUDE_CONFIG_DIR`.
    pub claude_home: Option<PathBuf>,
    /// Codex's own folder: `~/.codex`, or `CODEX_HOME`.
    pub codex_home: Option<PathBuf>,
    /// The shared `~/.agents` folder Codex also reads skills from.
    pub agents_home: Option<PathBuf>,
    /// Hermes's own folder: `~/.hermes`, or `HERMES_HOME`.
    pub hermes_home: Option<PathBuf>,
    /// Agents made in Ophio, and the person's own copies of Øphio's
    /// built-in agents, to edit: a task starts from the copy while there is
    /// one.
    pub ophio_agents: Option<PathBuf>,
    /// The person's own copies of Øphio's built-in skills, to edit: a task
    /// starts from the copy while there is one.
    pub ophio_skills: Option<PathBuf>,
    /// Where tasks started "On its own" get their folders.
    pub one_offs: Option<PathBuf>,
    /// Where Øphio's built-in agents and skills are written for the tasks
    /// that use them. Unset, they aren't offered.
    pub setup_skills: Option<PathBuf>,
}

/// Øphio's built-in skills, for setting it up, for work on GitHub and for
/// testing apps, by id, each followed by the rules its kind shares. Each is
/// offered as it is here, and written to `setup_skills` when a task starts
/// from it, unless the person has their own copy in `ophio_skills`, which is
/// used instead. Its built-in agents, in `setup_agents`, work the same way,
/// with copies in `ophio_agents`.
const SETUP_SKILLS: [(&str, &str); 17] = [
    (
        "setup-phone",
        concat!(
            include_str!("setup_skills/setup-phone.md"),
            include_str!("setup_skills/rules.md")
        ),
    ),
    (
        "setup-projects",
        concat!(
            include_str!("setup_skills/setup-projects.md"),
            include_str!("setup_skills/rules.md")
        ),
    ),
    (
        "setup-tailscale",
        concat!(
            include_str!("setup_skills/setup-tailscale.md"),
            include_str!("setup_skills/rules.md")
        ),
    ),
    (
        "setup-voice",
        concat!(
            include_str!("setup_skills/setup-voice.md"),
            include_str!("setup_skills/rules.md")
        ),
    ),
    (
        "setup-telegram",
        concat!(
            include_str!("setup_skills/setup-telegram.md"),
            include_str!("setup_skills/rules.md")
        ),
    ),
    (
        "setup-discord",
        concat!(
            include_str!("setup_skills/setup-discord.md"),
            include_str!("setup_skills/rules.md")
        ),
    ),
    (
        "setup-desktop",
        concat!(
            include_str!("setup_skills/setup-desktop.md"),
            include_str!("setup_skills/rules.md")
        ),
    ),
    (
        "setup-accounts",
        concat!(
            include_str!("setup_skills/setup-accounts.md"),
            include_str!("setup_skills/rules.md")
        ),
    ),
    (
        "setup-fix",
        concat!(
            include_str!("setup_skills/setup-fix.md"),
            include_str!("setup_skills/rules.md")
        ),
    ),
    (
        "setup-everything",
        concat!(
            include_str!("setup_skills/setup-everything.md"),
            include_str!("setup_skills/rules.md")
        ),
    ),
    (
        "github-prepare-pr",
        concat!(
            include_str!("setup_skills/github-prepare-pr.md"),
            include_str!("setup_skills/github-rules.md")
        ),
    ),
    (
        "github-babysit",
        concat!(
            include_str!("setup_skills/github-babysit.md"),
            include_str!("setup_skills/github-rules.md")
        ),
    ),
    (
        "github-audit",
        concat!(
            include_str!("setup_skills/github-audit.md"),
            include_str!("setup_skills/github-rules.md")
        ),
    ),
    (
        "github-triage",
        concat!(
            include_str!("setup_skills/github-triage.md"),
            include_str!("setup_skills/github-rules.md")
        ),
    ),
    (
        "github-find",
        concat!(
            include_str!("setup_skills/github-find.md"),
            include_str!("setup_skills/github-rules.md")
        ),
    ),
    ("live-testing", crate::setup_agents::LIVE_TESTING),
    ("checklist", crate::checklist::SKILL),
];

/// Entries listed at most; more set `truncated`.
const MAX_ENTRIES: usize = 500;
/// Names read from one folder at most, counted as they are read.
const MAX_LISTED: usize = 2000;
/// Files opened by one look through the folders at most.
const MAX_READ: usize = 2000;
/// An agent's file or a skill's `SKILL.md` larger than this is not read. A
/// Claude Code agent's instructions go on its command line, where one
/// argument can't pass 128 KiB, beside the task's other instructions.
const MAX_FILE: u64 = 64 * 1024;
const MAX_DESCRIPTION: usize = 1000;
const MAX_FIELD: usize = 100;

const SOURCES: [(CatalogKind, CatalogSource); 7] = [
    (CatalogKind::Agent, CatalogSource::Ophio),
    (CatalogKind::Agent, CatalogSource::ClaudeUser),
    (CatalogKind::Agent, CatalogSource::ClaudeProject),
    (CatalogKind::Skill, CatalogSource::ClaudeUser),
    (CatalogKind::Skill, CatalogSource::ClaudeProject),
    (CatalogKind::Skill, CatalogSource::CodexUser),
    (CatalogKind::Skill, CatalogSource::CodexProject),
];

fn project_source(source: CatalogSource) -> bool {
    matches!(
        source,
        CatalogSource::ClaudeProject | CatalogSource::CodexProject
    )
}

/// The folders an entry of this kind and source is looked for in, first
/// first: an id found in an earlier folder hides the same id in a later one.
fn folders(
    dirs: &LauncherDirs,
    project: Option<&Path>,
    kind: CatalogKind,
    source: CatalogSource,
) -> Vec<PathBuf> {
    use CatalogKind::*;
    use CatalogSource::*;
    let under = |base: Option<&Path>, rest: &[&str]| {
        base.map(|b| rest.iter().fold(b.to_path_buf(), |p, r| p.join(r)))
    };
    let claude = dirs.claude_home.as_deref();
    let codex = dirs.codex_home.as_deref();
    match (kind, source) {
        (Agent, Ophio) => dirs.ophio_agents.clone().into_iter().collect(),
        (Agent, ClaudeUser) => under(claude, &["agents"]).into_iter().collect(),
        (Agent, ClaudeProject) => under(project, &[".claude", "agents"]).into_iter().collect(),
        (Skill, ClaudeUser) => under(claude, &["skills"]).into_iter().collect(),
        (Skill, ClaudeProject) => under(project, &[".claude", "skills"]).into_iter().collect(),
        (Skill, CodexUser) => [
            under(codex, &["skills"]),
            under(codex, &["skills", ".system"]),
            under(dirs.agents_home.as_deref(), &["skills"]),
        ]
        .into_iter()
        .flatten()
        .collect(),
        (Skill, CodexProject) => [
            under(project, &[".agents", "skills"]),
            under(project, &[".codex", "skills"]),
        ]
        .into_iter()
        .flatten()
        .collect(),
        // Built-in agents and skills aren't read from a folder.
        (Agent, CodexUser | CodexProject | OphioSetup) | (Skill, Ophio | OphioSetup) => Vec::new(),
    }
}

/// One plain name: no separators, not hidden, not `..`.
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && !id.starts_with('.')
        && !id
            .chars()
            .any(|c| c.is_control() || matches!(c, '/' | '\\' | ':'))
        && matches!(
            Path::new(id).components().collect::<Vec<_>>()[..],
            [Component::Normal(_)]
        )
}

/// A name a harness can be asked for on its command line or in a prompt.
fn native_name(name: &str) -> bool {
    let mut chars = name.chars();
    name.len() <= 64
        && chars.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'))
}

struct Found {
    entry: CatalogEntry,
    /// The text after the frontmatter.
    body: String,
    /// The frontmatter's `name`, which a harness knows it by.
    named: Option<String>,
    path: PathBuf,
}

fn file_of(kind: CatalogKind, folder: &Path, id: &str) -> PathBuf {
    match kind {
        CatalogKind::Agent => folder.join(format!("{id}.md")),
        CatalogKind::Skill => folder.join(id).join("SKILL.md"),
    }
}

/// The bytes of the file at `path`, or `None` when no file is there. It is
/// opened once, without waiting (a FIFO with no writer would block), and read
/// from that handle to at most `MAX_FILE`. A project's file is opened through
/// the project's folder, which a link can't lead out of, and only if no other
/// name on the computer is linked to it.
fn read_file(path: &Path, project: Option<&Path>) -> Result<Option<Vec<u8>>, String> {
    use std::io::Read;
    let shown = path.display();
    let outside = || format!("{shown} leads outside the project, so it isn't listed");
    let opened = match project {
        Some(project) => {
            let relative = path.strip_prefix(project).map_err(|_| outside())?;
            let mut options = cap_std::fs::OpenOptions::new();
            options.read(true);
            #[cfg(unix)]
            {
                use cap_std::fs::OpenOptionsExt;
                options.custom_flags(rustix::fs::OFlags::NONBLOCK.bits() as i32);
            }
            crate::project_folders::open_project_root(project)
                .and_then(|dir| dir.open_with(relative, &options))
                .map(cap_std::fs::File::into_std)
        }
        None => {
            let mut options = std::fs::OpenOptions::new();
            options.read(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.custom_flags(rustix::fs::OFlags::NONBLOCK.bits() as i32);
            }
            options.open(path)
        }
    };
    let file = match opened {
        Ok(file) => file,
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            return Ok(None);
        }
        // Said plainly when it is a link out; the reason itself is no help.
        Err(_)
            if project.is_some_and(|project| {
                !ports::library::canonical(path).is_ok_and(|real| real.starts_with(project))
            }) =>
        {
            return Err(outside());
        }
        Err(e) => return Err(format!("Couldn't read {shown}: {e}")),
    };
    let meta = file
        .metadata()
        .map_err(|e| format!("Couldn't read {shown}: {e}"))?;
    if !meta.is_file() {
        return Ok(None);
    }
    let links = crate::transfers::links(&file, &meta);
    if project.is_some() && links.map_err(|e| format!("Couldn't read {shown}: {e}"))? != 1 {
        return Err(format!(
            "{shown} is linked to another name on the computer, so it isn't listed"
        ));
    }
    let too_large = || format!("{shown} is larger than 64 KB, so it isn't listed");
    if meta.len() > MAX_FILE {
        return Err(too_large());
    }
    let mut bytes = Vec::new();
    file.take(MAX_FILE + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("Couldn't read {shown}: {e}"))?;
    if bytes.len() as u64 > MAX_FILE {
        return Err(too_large());
    }
    Ok(Some(bytes))
}

/// The entry `id` in `folder`, or `None` when it isn't there. A file that is
/// there but can't be used is an error saying why. A project's own entry is
/// read only from inside `project`.
fn read_entry(
    kind: CatalogKind,
    source: CatalogSource,
    folder: &Path,
    id: &str,
    project: Option<&Path>,
) -> Result<Option<Found>, String> {
    let path = file_of(kind, folder, id);
    let shown = path.display();
    let within = match project_source(source) {
        true => Some(
            project
                .ok_or_else(|| format!("{shown} leads outside the project, so it isn't listed"))?,
        ),
        false => None,
    };
    let Some(bytes) = read_file(&path, within)? else {
        return Ok(None);
    };
    parse_entry(kind, source, path, id, bytes).map(Some)
}

/// The entry `id` whose file at `path` holds `bytes`.
fn parse_entry(
    kind: CatalogKind,
    source: CatalogSource,
    path: PathBuf,
    id: &str,
    bytes: Vec<u8>,
) -> Result<Found, String> {
    let shown = path.display();
    let digest = format!("{:x}", Sha256::digest(&bytes));
    let text = String::from_utf8(bytes).map_err(|_| format!("{shown} isn't text"))?;
    let (fields, body) = frontmatter(&text);
    let field = |key: &str| {
        fields
            .get(key)
            .map(|v| v.trim())
            .filter(|v| !v.is_empty() && v.chars().count() <= MAX_FIELD)
            .map(str::to_owned)
    };
    let named = field("name");
    let description = fields
        .get("description")
        .map(|d| d.trim())
        .filter(|d| !d.is_empty())
        .map(|d| match d.char_indices().nth(MAX_DESCRIPTION) {
            Some((at, _)) => format!("{}…", &d[..at]),
            None => d.to_owned(),
        });
    let harness = field("harness")
        .and_then(|h| harness_named(&h))
        .or(match source {
            CatalogSource::ClaudeUser | CatalogSource::ClaudeProject => {
                Some(HarnessKind::ClaudeCode)
            }
            CatalogSource::CodexUser | CatalogSource::CodexProject => Some(HarnessKind::Codex),
            CatalogSource::Ophio | CatalogSource::OphioSetup => None,
        });
    let launch_policy = field("launch_policy")
        .and_then(|p| serde_json::from_value(serde_json::Value::String(p)).ok())
        .or_else(|| {
            field("permissionMode").and_then(|p| match p.as_str() {
                "default" => Some(LaunchPolicy::Standard),
                "acceptEdits" => Some(LaunchPolicy::AcceptEdits),
                "bypassPermissions" => Some(LaunchPolicy::BypassPermissions),
                _ => None,
            })
        });
    Ok(Found {
        entry: CatalogEntry {
            kind,
            source,
            id: id.to_owned(),
            name: named.clone().unwrap_or_else(|| id.to_owned()),
            description,
            digest,
            host_path: path.to_string_lossy().into_owned(),
            library: None,
            harness,
            // Claude's "inherit" asks for whatever the task has.
            model: field("model").filter(|m| m != "inherit"),
            effort: field("effort"),
            launch_policy,
            copy: None,
            disable: None,
        },
        body: body.trim().to_owned(),
        named,
        path,
    })
}

/// Every built-in agent and skill, by kind and id.
fn built_in() -> impl Iterator<Item = (CatalogKind, &'static str)> {
    let agents = setup_agents()
        .iter()
        .map(|(id, _)| (CatalogKind::Agent, *id));
    let skills = SETUP_SKILLS.iter().map(|(id, _)| (CatalogKind::Skill, *id));
    agents.chain(skills)
}

/// The built-in agent or skill `id`, as it would be written under `folder`.
fn setup_entry(kind: CatalogKind, folder: &Path, id: &str) -> Option<Found> {
    let text = setup_text(kind, id)?;
    let path = file_of(kind, folder, id);
    let found = parse_entry(
        kind,
        CatalogSource::OphioSetup,
        path,
        id,
        text.as_bytes().to_vec(),
    );
    // Each is checked by a test; one that didn't parse would never ship.
    found.ok()
}

/// The text of the built-in agent or skill `id`.
fn setup_text(kind: CatalogKind, id: &str) -> Option<&'static str> {
    match kind {
        CatalogKind::Agent => setup_agent_text(id),
        CatalogKind::Skill => SETUP_SKILLS
            .iter()
            .find(|(known, _)| *known == id)
            .map(|(_, text)| *text),
    }
}

/// The person's own copy of the built-in agent or skill `id`, when they have
/// made one. A copy that is there but can't be used is an error saying why.
fn own_copy(dirs: &LauncherDirs, kind: CatalogKind, id: &str) -> Result<Option<Found>, String> {
    let folder = match kind {
        CatalogKind::Agent => &dirs.ophio_agents,
        CatalogKind::Skill => &dirs.ophio_skills,
    };
    let (Some(folder), Some(_)) = (folder, setup_text(kind, id)) else {
        return Ok(None);
    };
    let folder = ports::library::canonical(folder).unwrap_or_else(|_| folder.clone());
    let found = read_entry(kind, CatalogSource::OphioSetup, &folder, id, None)?;
    Ok(found.map(|mut found| {
        found.entry.copy = Some(SkillCopy { newer: false });
        found
    }))
}

/// The digest of each built-in agent's or skill's text the person's copy was
/// made from, or last kept against a newer one, by id: no agent has a
/// skill's id. Kept as Ophio's meta.
pub(crate) type CopyBases = HashMap<String, String>;
pub(crate) const COPY_BASES: &str = "setup_skill_copies";

/// Writes the built-in agent or skill `found` to its file, unless the file
/// already holds it: a task reads it from there, and the person can too.
fn write_setup_file(found: &Found) -> std::io::Result<()> {
    let text = setup_text(found.entry.kind, &found.entry.id).ok_or(std::io::ErrorKind::NotFound)?;
    if std::fs::read(&found.path).is_ok_and(|held| held == text.as_bytes()) {
        return Ok(());
    }
    let folder = found.path.parent().ok_or(std::io::ErrorKind::NotFound)?;
    let name = found.path.file_name().ok_or(std::io::ErrorKind::NotFound)?;
    std::fs::create_dir_all(folder)?;
    let temporary = folder.join(format!(
        ".{}.{}",
        name.to_string_lossy(),
        uuid::Uuid::new_v4()
    ));
    std::fs::write(&temporary, text)
        .and_then(|()| std::fs::rename(&temporary, &found.path))
        .inspect_err(|_| {
            let _ = std::fs::remove_file(&temporary);
        })
}

fn harness_named(name: &str) -> Option<HarnessKind> {
    match name.to_ascii_lowercase().replace(['-', ' '], "_").as_str() {
        "claude" | "claude_code" => Some(HarnessKind::ClaudeCode),
        "codex" => Some(HarnessKind::Codex),
        _ => None,
    }
}

/// The top-level fields of a Markdown file's YAML frontmatter, and the text
/// after it. Only what agents and skills use is understood: plain, quoted and
/// block (`|`, `>`) strings. A nested map or a list is skipped.
pub(crate) fn frontmatter(text: &str) -> (HashMap<String, String>, &str) {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut fields = HashMap::new();
    let mut lines = text.split_inclusive('\n');
    if lines.next().map(str::trim_end) != Some("---") {
        return (fields, text);
    }
    let mut offset = text.find('\n').map_or(text.len(), |i| i + 1);
    let mut block: Vec<&str> = Vec::new();
    let mut closed = false;
    for line in lines {
        offset += line.len();
        let bare = line.trim_end();
        if bare == "---" || bare == "..." {
            closed = true;
            break;
        }
        block.push(bare);
    }
    if !closed {
        return (HashMap::new(), text);
    }
    let body = &text[offset.min(text.len())..];
    let mut i = 0;
    while i < block.len() {
        let line = block[i];
        i += 1;
        if line.starts_with([' ', '\t']) || line.starts_with('#') || line.is_empty() {
            continue;
        }
        let Some((key, rest)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim();
        let rest = rest.trim();
        // Lines indented under this key belong to it.
        let start = i;
        while i < block.len() && (block[i].is_empty() || block[i].starts_with([' ', '\t'])) {
            i += 1;
        }
        let more = &block[start..i];
        let value = if let Some(style) = rest.chars().next().filter(|c| matches!(c, '|' | '>')) {
            let lines: Vec<&str> = more.iter().map(|l| l.trim()).collect();
            if style == '|' {
                lines.join("\n").trim_end().to_owned()
            } else {
                fold(&lines)
            }
        } else if let Some(quoted) = rest.strip_prefix('"') {
            let all = std::iter::once(quoted)
                .chain(more.iter().map(|l| l.trim()))
                .collect::<Vec<_>>()
                .join(" ");
            double_quoted(&all)
        } else if let Some(quoted) = rest.strip_prefix('\'') {
            let all = std::iter::once(quoted)
                .chain(more.iter().map(|l| l.trim()))
                .collect::<Vec<_>>()
                .join(" ");
            single_quoted(&all)
        } else if rest.is_empty() {
            // A nested map or a list.
            continue;
        } else {
            let plain = std::iter::once(rest)
                .chain(more.iter().map(|l| l.trim()))
                .filter(|l| !l.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            match plain.find(" #") {
                Some(at) => plain[..at].trim_end().to_owned(),
                None => plain,
            }
        };
        fields.insert(key.to_owned(), value);
    }
    (fields, body)
}

/// A folded block: lines join with spaces, and an empty line is a new line.
fn fold(lines: &[&str]) -> String {
    let mut out = String::new();
    for line in lines {
        if line.is_empty() {
            out.push('\n');
        } else {
            if !out.is_empty() && !out.ends_with('\n') {
                out.push(' ');
            }
            out.push_str(line);
        }
    }
    out.trim_end().to_owned()
}

fn single_quoted(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            // `''` is a quote inside the string.
            '\'' if chars.next_if_eq(&'\'').is_some() => out.push('\''),
            '\'' => break,
            c => out.push(c),
        }
    }
    out
}

fn double_quoted(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => break,
            '\\' => match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some(other) => out.push(other),
                None => break,
            },
            c => out.push(c),
        }
    }
    out
}

/// Where the file at `path` opens in Library, looked for where it really
/// is: Library opens nothing through a link, so a skill folder linked from
/// elsewhere opens in the root it leads to, if there is one.
fn library_of(path: &Path, roots: &[LibraryRoot]) -> Option<CatalogFile> {
    library_file(&ports::library::canonical(path).ok()?, roots)
}

/// Where `path` can be opened in Library: the deepest root holding it.
pub(crate) fn library_file(path: &Path, roots: &[LibraryRoot]) -> Option<CatalogFile> {
    roots
        .iter()
        .filter_map(|root| {
            let parts = path
                .strip_prefix(&root.host_path)
                .ok()?
                .components()
                .map(|c| match c {
                    Component::Normal(part) => part.to_str(),
                    _ => None,
                })
                .collect::<Option<Vec<_>>>()?;
            let path = parts.join("/");
            root.holds(&path).then_some((root, path))
        })
        .max_by_key(|(root, _)| root.host_path.len())
        .map(|(root, path)| CatalogFile {
            root_id: root.root_id,
            path,
        })
}

/// Library roots for Ophio's agents and skills folders and the one-offs
/// folder, each while it exists and is neither inside nor around one of
/// `roots`. Their ids come from their paths, so what the phone keeps of them
/// lasts.
pub(crate) fn automatic_roots(
    dirs: &LauncherDirs,
    roots: &[RootSpec],
    private: &[PathBuf],
) -> Vec<RootSpec> {
    let overlap = |a: &Path, b: &Path| a.starts_with(b) || b.starts_with(a);
    let mut automatic: Vec<RootSpec> = Vec::new();
    // One-offs first: agents and skills kept inside its folder are reached
    // through it.
    for (folder, kind) in [
        (dirs.one_offs.as_deref(), RootKind::Notes),
        (dirs.ophio_agents.as_deref(), RootKind::Agents),
        (dirs.ophio_skills.as_deref(), RootKind::Skills),
    ] {
        let Some(path) = folder.and_then(|f| ports::library::canonical(f).ok()) else {
            continue;
        };
        if !path.is_dir()
            || path.to_str().is_none()
            || (roots.iter().chain(&automatic))
                .map(|r| r.path.as_path())
                .chain(private.iter().map(PathBuf::as_path))
                .any(|other| overlap(other, &path))
        {
            continue;
        }
        let label = match kind {
            RootKind::Agents => "Ophio agents".to_owned(),
            RootKind::Skills => "Ophio skills".to_owned(),
            _ => path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("One-offs")
                .to_owned(),
        };
        automatic.push(RootSpec {
            root_id: path_id(&path),
            label,
            path,
            kind,
            writable: true,
            project_id: None,
            harness: None,
            only: None,
        });
    }
    automatic
}

/// The id of a root Ophio adds itself, the same for the same folder each
/// time.
pub(crate) fn path_id(path: &Path) -> protocol::LibraryRootId {
    let digest = Sha256::digest(path.as_os_str().as_encoded_bytes());
    let mut bytes = [0; 16];
    bytes.copy_from_slice(&digest[..16]);
    protocol::LibraryRootId(uuid::Builder::from_custom_bytes(bytes).into_uuid())
}

/// A project, with its folder held as it was checked.
type Held<'a> = (&'a Path, &'a cap_std::fs::Dir);

/// Where `path` is within `project`.
fn within(path: &Path, project: &Path) -> std::io::Result<PathBuf> {
    path.strip_prefix(project)
        .map(Path::to_path_buf)
        .map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "It is outside the project",
            )
        })
}

/// The names in `folder`, sorted. No more than `MAX_LISTED` are read; more
/// set `truncated`. A project's folder is read through the folder `held` for
/// the project, which a link can't lead out of.
fn names(folder: &Path, held: Option<Held>, truncated: &mut bool) -> std::io::Result<Vec<String>> {
    let listed: Box<dyn Iterator<Item = Option<std::ffi::OsString>>> = match held {
        Some((project, dir)) => Box::new(
            dir.read_dir(within(folder, project)?)?
                .map(|entry| entry.ok().map(|e| e.file_name())),
        ),
        None => Box::new(std::fs::read_dir(folder)?.map(|entry| entry.ok().map(|e| e.file_name()))),
    };
    let mut names = Vec::new();
    for (n, name) in listed.enumerate() {
        if n == MAX_LISTED {
            *truncated = true;
            break;
        }
        if let Some(name) = name.and_then(|name| name.into_string().ok()) {
            names.push(name);
        }
    }
    names.sort();
    Ok(names)
}

/// The id of an entry listed as `name` in one of `kind`'s folders.
fn entry_id(kind: CatalogKind, name: String) -> Option<String> {
    match kind {
        CatalogKind::Agent => name.strip_suffix(".md").map(str::to_owned),
        CatalogKind::Skill => Some(name),
    }
}

/// How far into an agents folder's own folders the launch check looks.
const MAX_DEPTH: usize = 8;

/// One look through the folders at a time each, at most: a catalog asked
/// for again and again waits rather than reading the folders many times over.
static LOOKS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);

/// Every entry in the computer's folders and, with `project`, the project's.
/// `project` is the project's canonical folder. A built-in agent or skill the
/// person has copied is listed as their copy, saying whether Øphio's is newer
/// than the one in `bases` it was copied from.
pub(crate) fn scan(
    dirs: &LauncherDirs,
    project: Option<&Path>,
    roots: &[LibraryRoot],
    bases: &CopyBases,
) -> Catalog {
    let mut entries = Vec::new();
    let mut problems = Vec::new();
    let mut truncated = false;
    let mut seen = HashSet::new();
    let mut reads = 0;
    if let Some(folder) = &dirs.setup_skills {
        for (kind, id) in built_in() {
            let Some(built_in) = setup_entry(kind, folder, id) else {
                continue;
            };
            match own_copy(dirs, kind, id) {
                Ok(None) => entries.push(built_in.entry),
                Ok(Some(mut copy)) => {
                    // A copy whose source wasn't kept is taken to be of the
                    // version it differs from.
                    let base = bases.get(id).unwrap_or(&copy.entry.digest);
                    copy.entry.copy = Some(SkillCopy {
                        newer: *base != built_in.entry.digest,
                    });
                    copy.entry.library = library_of(&copy.path, roots);
                    entries.push(copy.entry);
                }
                Err(problem) => problems.push(problem),
            }
            // A copy of a built-in agent, in Ophio's agents folder, is
            // listed once, as that copy.
            if kind == CatalogKind::Agent {
                seen.insert((kind, CatalogSource::Ophio, id.to_owned()));
            }
        }
    }
    let root = project.map(crate::project_folders::open_project_root);
    'all: for (kind, source) in SOURCES {
        for folder in folders(dirs, project, kind, source) {
            // A folder that leads elsewhere is read where it leads, so the
            // paths shown and given to Library are real ones. A project's
            // own is read only within the project; where it leads otherwise
            // isn't said.
            let real = ports::library::canonical(&folder).unwrap_or_else(|_| folder.clone());
            let held = match (project_source(source), project, &root) {
                (false, ..) => None,
                (true, Some(project), Some(Ok(dir))) if real.starts_with(project) => {
                    Some((project, dir))
                }
                (true, Some(_), Some(Ok(_))) => {
                    problems.push(format!(
                        "{} leads outside the project, so it isn't listed",
                        folder.display()
                    ));
                    continue;
                }
                // The project's folder is gone or was replaced.
                _ => continue,
            };
            let folder = real;
            let names = match names(&folder, held, &mut truncated) {
                Ok(names) => names,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => {
                    problems.push(format!("Couldn't read {}: {e}", folder.display()));
                    continue;
                }
            };
            for name in names {
                let Some(id) = entry_id(kind, name) else {
                    continue;
                };
                if !valid_id(&id) || seen.contains(&(kind, source, id.clone())) {
                    continue;
                }
                if reads == MAX_READ {
                    truncated = true;
                    break 'all;
                }
                reads += 1;
                match read_entry(kind, source, &folder, &id, project) {
                    Ok(Some(mut found)) => {
                        if entries.len() == MAX_ENTRIES {
                            truncated = true;
                            break 'all;
                        }
                        seen.insert((kind, source, id));
                        found.entry.library = library_of(&found.path, roots);
                        entries.push(found.entry);
                    }
                    Ok(None) => {}
                    Err(problem) => {
                        seen.insert((kind, source, id));
                        problems.push(problem);
                    }
                }
            }
        }
    }
    entries.sort_by(|a, b| {
        (a.kind as u8, a.name.to_lowercase(), a.source as u8).cmp(&(
            b.kind as u8,
            b.name.to_lowercase(),
            b.source as u8,
        ))
    });
    Catalog {
        entries,
        one_offs: dirs
            .one_offs
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned()),
        agents_folder: dirs.ophio_agents.as_deref().and_then(|folder| {
            let folder = ports::library::canonical(folder).ok()?;
            let file = library_file(&folder, roots)?;
            roots
                .iter()
                .any(|r| r.root_id == file.root_id && r.writable)
                .then_some(file)
        }),
        problems,
        truncated,
    }
}

/// A start read again from the computer, with what the task shows for it.
#[derive(Debug)]
pub(crate) struct Resolved {
    pub run: RunStart,
    pub shown: protocol::launchers::StartedFrom,
}

/// Read the entry `start` names again, refusing it if its file is gone or
/// is no longer the one whose digest was shown.
pub(crate) fn resolve(
    dirs: &LauncherDirs,
    project: Option<&Path>,
    start: &TaskStart,
) -> Result<Resolved, ProtocolError> {
    let (kind, source, id, digest) = match start {
        TaskStart::Scratch => {
            return Ok(Resolved {
                run: RunStart::Scratch,
                shown: StartedFrom::Scratch,
            });
        }
        TaskStart::Agent { source, id, digest } => (CatalogKind::Agent, *source, id, digest),
        TaskStart::Skill { source, id, digest } => (CatalogKind::Skill, *source, id, digest),
        // Copied from what was saved for that task, never read again.
        TaskStart::SameAs { .. } => return Err(internal("Another task's start")),
    };
    if project_source(source) && project.is_none() {
        return Err(error(
            ErrorCode::InvalidRequest,
            "A project’s own agent or skill starts only in that project",
        ));
    }
    let gone = || {
        error(
            ErrorCode::NotFound,
            "That agent or skill is no longer on the computer. Look again, then start.",
        )
    };
    if !valid_id(id) {
        return Err(gone());
    }
    let mut found = None;
    if let (CatalogSource::OphioSetup, Some(folder)) = (source, &dirs.setup_skills) {
        found = match own_copy(dirs, kind, id)
            .map_err(|problem| error(ErrorCode::InvalidRequest, problem))?
        {
            Some(copy) => Some(copy),
            None => setup_entry(kind, folder, id),
        };
    }
    for folder in folders(dirs, project, kind, source) {
        let folder = ports::library::canonical(&folder).unwrap_or(folder);
        if let Some(entry) = read_entry(kind, source, &folder, id, project)
            .map_err(|problem| error(ErrorCode::InvalidRequest, problem))?
        {
            found = Some(entry);
            break;
        }
    }
    let found = found.ok_or_else(gone)?;
    if found.entry.digest != *digest {
        return Err(error(
            ErrorCode::Conflict,
            format!(
                "“{}” has changed since it was shown. Look at it again, then start.",
                found.entry.name
            ),
        ));
    }
    // The person's copy is theirs, never written over.
    if source == CatalogSource::OphioSetup && found.entry.copy.is_none() {
        write_setup_file(&found).map_err(internal)?;
    }
    let native = |harness| {
        let name = match kind {
            // Claude knows an agent only by the name its file gives.
            CatalogKind::Agent => found.named.clone()?,
            CatalogKind::Skill => found.named.clone().unwrap_or_else(|| id.clone()),
        };
        native_name(&name).then(|| NativeEntry {
            harness,
            name,
            project: project_source(source),
        })
    };
    let native = match (kind, source) {
        (_, CatalogSource::ClaudeUser | CatalogSource::ClaudeProject) => {
            native(HarnessKind::ClaudeCode)
        }
        (CatalogKind::Skill, CatalogSource::CodexUser | CatalogSource::CodexProject) => {
            native(HarnessKind::Codex)
        }
        _ => None,
    };
    let name = found.entry.name.clone();
    let digest = found.entry.digest.clone();
    Ok(match kind {
        CatalogKind::Agent => Resolved {
            run: RunStart::Agent {
                name: name.clone(),
                instructions: found.body,
                native,
                file: found.path,
                digest,
            },
            shown: StartedFrom::Agent { name },
        },
        CatalogKind::Skill => Resolved {
            run: RunStart::Skill {
                name: name.clone(),
                skill_file: found.path,
                native,
                instructions: Some(found.body),
                digest,
            },
            shown: StartedFrom::Skill { name },
        },
    })
}

/// Whether the harness, finding `start`'s entry itself by its name in a run
/// in `project`, finds the file read when the task was made: the file still
/// has the digest it had then, and nothing else in the folders it looks in
/// has that name. When it can't be told, it doesn't.
pub(crate) fn still_native(dirs: &LauncherDirs, project: &Path, start: &RunStart) -> bool {
    let (kind, native, file, digest) = match start {
        RunStart::Agent {
            native: Some(native),
            file,
            digest,
            ..
        } => (CatalogKind::Agent, native, file, digest),
        RunStart::Skill {
            native: Some(native),
            skill_file,
            digest,
            ..
        } => (CatalogKind::Skill, native, skill_file, digest),
        _ => return false,
    };
    let (sources, home) = match (kind, native.harness) {
        (_, HarnessKind::ClaudeCode) => (
            [CatalogSource::ClaudeUser, CatalogSource::ClaudeProject],
            &dirs.claude_home,
        ),
        (CatalogKind::Skill, HarnessKind::Codex) => (
            [CatalogSource::CodexUser, CatalogSource::CodexProject],
            &dirs.codex_home,
        ),
        _ => return false,
    };
    // Without the tool's own folder, what it finds by that name there can't
    // be seen.
    if home.is_none() {
        return false;
    }
    let named = |name: &str| name.eq_ignore_ascii_case(&native.name);
    let root = crate::project_folders::open_project_root(project);
    let (mut ours, mut reads, mut looked) = (false, 0, HashSet::new());
    for source in sources {
        // Claude Code finds agents in the folders within its agents folders
        // too.
        let mut pending: Vec<_> = folders(dirs, Some(project), kind, source)
            .into_iter()
            .map(|folder| (folder, 0))
            .collect();
        while let Some((folder, depth)) = pending.pop() {
            let folder = ports::library::canonical(&folder).unwrap_or(folder);
            // What a project's folder leads to outside it can't be read.
            let held = match (project_source(source), &root) {
                (false, _) => None,
                (true, Ok(dir)) => Some((project, dir)),
                (true, Err(_)) => return false,
            };
            // Two of its folders can be one.
            if !looked.insert(folder.clone()) {
                continue;
            }
            let mut truncated = false;
            let names = match names(&folder, held, &mut truncated) {
                Ok(names) if !truncated => names,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                _ => return false,
            };
            for name in names {
                reads += 1;
                if reads > MAX_READ {
                    return false;
                }
                let is_dir = kind == CatalogKind::Agent
                    && match held {
                        Some((project, dir)) => {
                            match within(&folder.join(&name), project).and_then(|p| dir.metadata(p))
                            {
                                Ok(meta) => meta.is_dir(),
                                Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
                                Err(_) => return false,
                            }
                        }
                        None => folder.join(&name).is_dir(),
                    };
                if is_dir {
                    if depth == MAX_DEPTH {
                        return false;
                    }
                    pending.push((folder.join(&name), depth + 1));
                    continue;
                }
                let Some(id) = entry_id(kind, name) else {
                    continue;
                };
                match read_entry(kind, source, &folder, &id, Some(project)) {
                    Ok(Some(found)) => {
                        if !(named(&id) || found.named.as_deref().is_some_and(named)) {
                            continue;
                        }
                        if found.path != *file || found.entry.digest != *digest || ours {
                            return false;
                        }
                        ours = true;
                    }
                    Ok(None) => {}
                    // What it is called can't be told.
                    Err(_) => return false,
                }
            }
        }
    }
    ours
}

/// `yyyy-mm-dd` of a time in milliseconds since 1970, in UTC.
fn utc_date(ms: i64) -> String {
    // Howard Hinnant's days-to-civil.
    let z = ms.div_euclid(86_400_000) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

fn valid_date(date: &str) -> bool {
    let b = date.as_bytes();
    b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b.iter()
            .enumerate()
            .all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
        && (1..=12).contains(&date[5..7].parse::<u8>().unwrap_or(0))
        && (1..=31).contains(&date[8..10].parse::<u8>().unwrap_or(0))
}

/// A folder name from what the person called the task: lower-case letters
/// and digits joined by hyphens.
pub(crate) fn slug(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
        if out.len() >= 48 {
            break;
        }
    }
    let out = out.trim_matches('-');
    if out.is_empty() {
        "task".into()
    } else {
        out.into()
    }
}

/// The one-offs folder, where it really is, unless it is unset or holds
/// Øphio's own files.
fn one_offs_root(dirs: &LauncherDirs, private: &[PathBuf]) -> Result<PathBuf, ProtocolError> {
    let root = (dirs.one_offs.as_deref())
        .ok_or_else(|| crate::unsupported("A folder for one-offs on this computer"))?;
    // Checked where it really is before anything is made: a missing folder
    // under a link can be in Ophio's own.
    let root = ports::library::resolved(root).map_err(|e| {
        error(
            ErrorCode::PathNotAllowed,
            format!("The one-offs folder {} can’t be used: {e}", root.display()),
        )
    })?;
    if private
        .iter()
        .any(|own| own.starts_with(&root) || root.starts_with(own))
    {
        return Err(error(
            ErrorCode::PathNotAllowed,
            "Øphio keeps its own files in the one-offs folder. Choose another in its settings.",
        ));
    }
    Ok(root)
}

/// Make a new folder for a one-off in `root`, `<date>-<slug>`, numbered if
/// the name is taken. Returns its canonical path.
pub(crate) fn make_one_off(
    root: &Path,
    name: &str,
    date: Option<&str>,
    now_ms: i64,
) -> Result<PathBuf, ProtocolError> {
    let date = match date {
        Some(date) if valid_date(date) => date.to_owned(),
        Some(_) => {
            return Err(error(
                ErrorCode::InvalidRequest,
                "The date for the folder’s name isn’t yyyy-mm-dd",
            ));
        }
        None => utc_date(now_ms),
    };
    std::fs::create_dir_all(root).map_err(|e| {
        error(
            ErrorCode::TemporarilyUnavailable,
            format!("Couldn't make the one-offs folder {}: {e}", root.display()),
        )
    })?;
    let base = format!("{date}-{}", slug(name));
    for n in 1..=99 {
        let folder = match n {
            1 => root.join(&base),
            n => root.join(format!("{base}-{n}")),
        };
        match std::fs::create_dir(&folder) {
            Ok(()) => return ports::library::canonical(&folder).map_err(internal),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => {
                return Err(error(
                    ErrorCode::TemporarilyUnavailable,
                    format!("Couldn't make the folder {}: {e}", folder.display()),
                ));
            }
        }
    }
    Err(error(
        ErrorCode::Conflict,
        "There are already 99 one-offs with that name today. Choose another name.",
    ))
}

/// One line of frontmatter text, double-quoted.
pub(crate) fn quoted_line(text: &str) -> String {
    let mut out = String::from("\"");
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if c.is_control() => out.push(' '),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A new agent's id and file, in the format Claude Code reads, so it also
/// works when copied there.
pub(crate) fn agent_file(
    name: &str,
    description: Option<&str>,
) -> Result<(String, String), ProtocolError> {
    let name = name.trim();
    if !name.chars().any(|c| c.is_ascii_alphanumeric()) || name.chars().count() > MAX_FIELD {
        return Err(error(
            ErrorCode::InvalidRequest,
            "Give the agent a name of up to 100 characters with a letter or digit in it",
        ));
    }
    let description = description.map(str::trim).filter(|d| !d.is_empty());
    if description.is_some_and(|d| d.chars().count() > MAX_DESCRIPTION) {
        return Err(error(
            ErrorCode::InvalidRequest,
            "Keep the description to 1000 characters",
        ));
    }
    let id = slug(name);
    let text = format!(
        "---\nname: {id}\ndescription: {}\n---\n\nDescribe who this agent is and how it works. Everything below the line above is given to the model when a task starts from it.\n",
        quoted_line(description.unwrap_or(name)),
    );
    Ok((id, text))
}

/// One of Ophio's folders for the person's own files, as what is said
/// about it names it.
#[derive(Debug, Clone, Copy)]
pub(crate) struct OwnFolder {
    /// "agents" or "skills".
    what: &'static str,
    /// What is made in it: "a new agent".
    made: &'static str,
}

const AGENTS: OwnFolder = OwnFolder {
    what: "agents",
    made: "a new agent",
};
const AGENT_COPIES: OwnFolder = OwnFolder {
    what: "agents",
    made: "a copy of an agent",
};
const SKILLS: OwnFolder = OwnFolder {
    what: "skills",
    made: "a copy of a skill",
};

/// Where the person's copy of a built-in agent or skill is kept.
struct CopyPlace {
    library: Arc<dyn ports::LibraryFs>,
    root: LibraryRoot,
    /// The copy's file, in `root`.
    path: String,
    /// Øphio's text of it.
    text: &'static str,
    /// "agent" or "skill".
    what: &'static str,
}

fn read_only(folder: &Path, own: OwnFolder) -> ProtocolError {
    error(
        ErrorCode::PermissionDenied,
        format!(
            "Ophio's {} folder {} is in a Library folder that can't be written, so {} can't be made there.",
            own.what,
            folder.display(),
            own.made
        ),
    )
}

/// Make `folder`, one of Ophio's folders for the person's own files, if it
/// is missing, where it would join Library: never in or around Ophio's own
/// folders, and in one of `roots` only if it can be written. It is made from
/// the deepest folder that is there, which a link on the way can't lead out
/// of.
fn make_own_folder(
    folder: &Path,
    own: OwnFolder,
    roots: &[LibraryRoot],
    private: &[PathBuf],
) -> Result<(), ProtocolError> {
    if std::fs::symlink_metadata(folder).is_ok() {
        return Ok(());
    }
    let refused = || {
        error(
            ErrorCode::PathNotAllowed,
            format!(
                "Ophio's {} folder {} isn't there, and Ophio doesn't make it there. Make it, then try again.",
                own.what,
                folder.display()
            ),
        )
    };
    let (mut there, mut rest) = (folder, Vec::new());
    let base = loop {
        match ports::library::canonical(there) {
            Ok(real) => break real,
            Err(_) => {
                rest.push(there.file_name().ok_or_else(refused)?);
                there = there.parent().ok_or_else(refused)?;
            }
        }
    };
    let rest: PathBuf = rest.into_iter().rev().collect();
    let real = base.join(&rest);
    let overlap = |a: &Path, b: &Path| a.starts_with(b) || b.starts_with(a);
    if private.iter().any(|p| overlap(p, &real)) {
        return Err(refused());
    }
    let holder = roots
        .iter()
        .filter(|r| {
            real.strip_prefix(&r.host_path)
                .ok()
                .and_then(Path::to_str)
                .is_some_and(|within| r.holds(&within.replace(std::path::MAIN_SEPARATOR, "/")))
        })
        .max_by_key(|r| r.host_path.len());
    let (from, rest) = match holder {
        Some(root) if !root.writable => return Err(read_only(folder, own)),
        Some(root) => {
            let from = PathBuf::from(&root.host_path);
            let rest = real
                .strip_prefix(&from)
                .map_err(|_| internal("Library root"))?
                .to_path_buf();
            (from, rest)
        }
        None => (base, rest),
    };
    cap_std::fs::Dir::open_ambient_dir(&from, cap_std::ambient_authority())
        .and_then(|dir| dir.create_dir_all(&rest))
        .map_err(|e| {
            error(
                ErrorCode::TemporarilyUnavailable,
                format!(
                    "Couldn't make the {} folder {}: {e}",
                    own.what,
                    folder.display()
                ),
            )
        })
}

impl CompanionHub {
    pub(crate) async fn catalog(
        &self,
        project_id: Option<ProjectId>,
    ) -> Result<Catalog, ProtocolError> {
        let project = match project_id {
            // Home's own folders are the person's, listed as theirs.
            Some(id) => {
                let project: Project = self.read(|r| r.require("project", id))?;
                (!project.home).then(|| PathBuf::from(project.host_path))
            }
            None => None,
        };
        let (roots, bases): (Vec<LibraryRoot>, CopyBases) = self.read(|r| {
            Ok((
                r.get("meta", "library_roots")?.unwrap_or_default(),
                r.get("meta", COPY_BASES)?.unwrap_or_default(),
            ))
        })?;
        let dirs = self.inner.launchers.clone();
        let disabled = self.disabled_paths()?;
        // Held until the look itself ends, not only until this waits for it.
        let look = LOOKS
            .acquire()
            .await
            .map_err(|_| internal("Looking for agents and skills"))?;
        tokio::task::spawn_blocking(move || {
            let _look = look;
            let mut catalog = scan(&dirs, project.as_deref(), &roots, &bases);
            for entry in &mut catalog.entries {
                let block = crate::disabled::entry_block(entry);
                let path = crate::disabled::real(Path::new(&entry.host_path));
                let off = block.is_ok() && disabled.contains(&path);
                entry.disable = Some(crate::disabled::disable(&block, off, &path));
            }
            catalog
        })
        .await
        .map_err(|_| internal("Reading agents and skills stopped"))
    }

    /// What the task `task_id` started from, as it was saved for it, for a
    /// new task in the same project. None if it started from nothing.
    fn same_start(
        &self,
        project_id: ProjectId,
        task_id: TaskId,
    ) -> Result<Option<Resolved>, ProtocolError> {
        let (task, run) = self.read(|r| {
            Ok((
                r.get::<Task>("task", task_id)?,
                r.get::<RunStart>("task_start", task_id)?,
            ))
        })?;
        let Some(task) = task.filter(|t| t.project_id == project_id) else {
            return Err(error(
                ErrorCode::NotFound,
                "The task this was to start as is no longer in this project",
            ));
        };
        match (task.start, run) {
            (None, _) => Ok(None),
            (Some(shown), Some(run)) => Ok(Some(Resolved { run, shown })),
            (Some(_), None) => Err(internal("A task's saved start")),
        }
    }

    /// What a new task starts from, read again now, and, for one started on
    /// its own, its new folder.
    pub(crate) async fn prepare_start(
        &self,
        project_id: ProjectId,
        start: Option<&TaskStart>,
        place: Option<&TaskPlace>,
    ) -> Result<(Option<Resolved>, Option<PathBuf>), ProtocolError> {
        let dirs = self.inner.launchers.clone();
        let private = self.inner.private.clone();
        let project = match place {
            Some(TaskPlace::OneOff { .. }) => {
                if self
                    .read(|r| r.get::<Project>("project", project_id))?
                    .is_some()
                {
                    return Err(error(
                        ErrorCode::Conflict,
                        "A one-off needs a new project id",
                    ));
                }
                None
            }
            None => {
                let project: Project = self.read(|r| r.require("project", project_id))?;
                Some(PathBuf::from(project.host_path))
            }
        };
        if let Some(TaskStart::SameAs { task_id }) = start {
            if project.is_none() {
                return Err(error(
                    ErrorCode::InvalidRequest,
                    "A task on its own can’t start as another task did",
                ));
            }
            let same = self.same_start(project_id, *task_id)?;
            if let Some(same) = &same {
                self.refuse_disabled_start(&same.run)?;
            }
            return Ok((same, None));
        }
        let start = start.cloned();
        let place = place.cloned();
        let now = self.inner.clock.now().0;
        let (resolved, folder, first) = tokio::task::spawn_blocking(move || {
            let resolved = start
                .as_ref()
                .map(|s| resolve(&dirs, project.as_deref(), s))
                .transpose()?;
            let mut first = false;
            let folder = match &place {
                Some(TaskPlace::OneOff { name, date }) => {
                    let root = one_offs_root(&dirs, &private)?;
                    first = !root.is_dir();
                    Some(make_one_off(&root, name, date.as_deref(), now)?)
                }
                None => None,
            };
            Ok::<_, ProtocolError>((resolved, folder, first))
        })
        .await
        .map_err(|_| internal("Reading what the task starts from stopped"))??;
        if let Some(resolved) = &resolved {
            self.refuse_disabled_start(&resolved.run)?;
        }
        if first {
            // The one-offs folder is new, and joins Library now. Library
            // without it is no reason to refuse the task.
            let _ = self.refresh_automatic_roots().await;
        }
        if let Some(folder) = &folder
            && let Err(e) = self.project_folder(folder)
        {
            let _ = std::fs::remove_dir(folder);
            return Err(e);
        }
        Ok((resolved, folder))
    }
    /// The folder of a one-off about to be deleted, or `None` when it is
    /// already gone. Only a folder of its own, directly in the one-offs
    /// folder, is ever deleted.
    pub(crate) async fn one_off_to_delete(
        &self,
        project_id: ProjectId,
    ) -> Result<Option<PathBuf>, ProtocolError> {
        let project: Project = self.read(|r| r.require("project", project_id))?;
        if !project.one_off {
            return Err(error(ErrorCode::Conflict, "This project isn’t a one-off"));
        }
        let dirs = self.inner.launchers.clone();
        let private = self.inner.private.clone();
        tokio::task::spawn_blocking(move || {
            let folder = PathBuf::from(&project.host_path);
            match std::fs::symlink_metadata(&folder) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(e) => {
                    return Err(error(
                        ErrorCode::TemporarilyUnavailable,
                        format!("Couldn’t read the folder {}: {e}", folder.display()),
                    ));
                }
                Ok(m) if m.is_dir() => {}
                Ok(_) => {
                    return Err(error(
                        ErrorCode::PathNotAllowed,
                        format!("{} is no longer a folder, so Øphio won’t delete it", folder.display()),
                    ));
                }
            }
            let root = one_offs_root(&dirs, &private)?;
            // Where it really is now: none of the folders above it may have
            // become a link to somewhere else.
            let real = ports::library::canonical(&folder).map_err(internal)?;
            if real != folder || folder.parent() != Some(root.as_path()) {
                return Err(error(
                    ErrorCode::PathNotAllowed,
                    format!(
                        "{} isn’t in the one-offs folder {}, so Øphio won’t delete it. Remove it instead, which keeps its files.",
                        folder.display(),
                        root.display()
                    ),
                ));
            }
            Ok(Some(folder))
        })
        .await
        .map_err(|_| internal("Checking the one-off’s folder stopped"))?
    }
    /// Where `folder`, one of Ophio's folders for the person's own files, is
    /// in Library: made when it is missing, and joining Library when it's
    /// new, so what is made in it is written only where Library may write.
    async fn own_folder(
        &self,
        folder: &Path,
        own: OwnFolder,
    ) -> Result<(Arc<dyn ports::LibraryFs>, LibraryRoot, CatalogFile), ProtocolError> {
        let library = self.library()?.clone();
        let roots = library.roots().await;
        let private = self.inner.private.clone();
        let made = folder.to_path_buf();
        tokio::task::spawn_blocking(move || make_own_folder(&made, own, &roots, &private))
            .await
            .map_err(|_| internal("Making a folder stopped"))??;
        // A new folder joins Library now.
        self.refresh_automatic_roots().await?;
        let roots = library.roots().await;
        let private = &self.inner.private;
        let place = ports::library::canonical(folder)
            .ok()
            // Never where a link from it leads into Ophio's own folders.
            .filter(|real| !private.iter().any(|p| p.starts_with(real) || real.starts_with(p)))
            .and_then(|real| library_file(&real, &roots))
            .ok_or_else(|| {
                error(
                    ErrorCode::PathNotAllowed,
                    format!(
                        "Ophio's {} folder {} isn't a folder Library can write in, so {} can't be made there.",
                        own.what,
                        folder.display(),
                        own.made
                    ),
                )
            })?;
        let root = roots
            .iter()
            .find(|r| r.root_id == place.root_id)
            .cloned()
            .ok_or_else(|| internal("Library root"))?;
        if !root.writable {
            return Err(read_only(folder, own));
        }
        Ok((library, root, place))
    }

    /// Make a new agent in Ophio's agents folder, through Library, so it is
    /// written only where Library may write. The folder joins Library when
    /// it's new, so the agent opens there to be written.
    pub(crate) async fn create_agent(
        &self,
        name: &str,
        description: Option<&str>,
    ) -> Result<CommandResult, ProtocolError> {
        let folder = self
            .inner
            .launchers
            .ophio_agents
            .clone()
            .ok_or_else(|| crate::unsupported("A folder for agents on this computer"))?;
        let (id, text) = agent_file(name, description)?;
        // Its file would be taken for the person's copy of the built-in one.
        if let Some(built_in) =
            setup_agent_text(&id).filter(|_| self.inner.launchers.setup_skills.is_some())
        {
            let (fields, _) = frontmatter(built_in);
            let named = fields.get("name").map_or(id.as_str(), String::as_str);
            return Err(error(
                ErrorCode::Conflict,
                format!(
                    "Øphio already has an agent called {named}. Open it to make your own copy, or choose another name."
                ),
            ));
        }
        let (library, root, place) = self.own_folder(&folder, AGENTS).await?;
        let path = match place.path.as_str() {
            "" => format!("{id}.md"),
            dir => format!("{dir}/{id}.md"),
        };
        let revision = library
            .create(place.root_id, &path, &text)
            .await
            .map_err(|e| match e {
                ports::LibraryError::AlreadyExists => error(
                    ErrorCode::Conflict,
                    format!("There's already an agent called {id}. Choose another name."),
                ),
                ports::LibraryError::ReadOnly => read_only(&folder, AGENTS),
                e => error(e.code(), format!("Couldn't make the agent {id}: {e}")),
            })?;
        let host_path = PathBuf::from(&root.host_path).join(&path);
        self.library_changed(place.root_id, &path, Some(revision))?;
        Ok(CommandResult::AgentCreated {
            id,
            host_path: host_path.to_string_lossy().into_owned(),
            file: Some(CatalogFile {
                root_id: place.root_id,
                path,
            }),
        })
    }

    /// Where the person's copy of the built-in agent or skill `id` is kept,
    /// through Library: an agent's file in Ophio's agents folder, or a
    /// skill's folder made in its skills folder. Said with it is what `id`
    /// is, "agent" or "skill", and Øphio's text of it.
    async fn setup_copy_place(&self, id: &str) -> Result<CopyPlace, ProtocolError> {
        let dirs = &self.inner.launchers;
        let (kind, text) = [CatalogKind::Skill, CatalogKind::Agent]
            .into_iter()
            .find_map(|kind| Some((kind, setup_text(kind, id)?)))
            .ok_or_else(|| {
                error(
                    ErrorCode::NotFound,
                    "Øphio has no built-in agent or skill by that name",
                )
            })?;
        let (folder, own, file, what) = match kind {
            CatalogKind::Agent => (
                dirs.ophio_agents
                    .clone()
                    .ok_or_else(|| crate::unsupported("A folder for agents on this computer"))?,
                AGENT_COPIES,
                format!("{id}.md"),
                "agent",
            ),
            CatalogKind::Skill => (
                dirs.ophio_skills
                    .clone()
                    .ok_or_else(|| crate::unsupported("A folder for skills on this computer"))?
                    .join(id),
                SKILLS,
                "SKILL.md".to_owned(),
                "skill",
            ),
        };
        let (library, root, place) = self.own_folder(&folder, own).await?;
        let path = match place.path.as_str() {
            "" => file,
            dir => format!("{dir}/{file}"),
        };
        Ok(CopyPlace {
            library,
            root,
            path,
            text,
            what,
        })
    }

    /// Keep `text`'s digest as what the copy of `id` was made from.
    fn copied_from(&self, id: &str, text: &str) -> Result<(), ProtocolError> {
        let digest = format!("{:x}", Sha256::digest(text.as_bytes()));
        self.write(|tx| {
            let mut bases: CopyBases = tx.read().get("meta", COPY_BASES)?.unwrap_or_default();
            bases.insert(id.to_owned(), digest);
            tx.put("meta", COPY_BASES, &bases)
        })
    }

    fn skill_copied(id: &str, root: &LibraryRoot, path: String) -> CommandResult {
        CommandResult::SetupSkillCopied {
            id: id.to_owned(),
            host_path: PathBuf::from(&root.host_path)
                .join(&path)
                .to_string_lossy()
                .into_owned(),
            file: Some(CatalogFile {
                root_id: root.root_id,
                path,
            }),
        }
    }

    /// Make the person's own copy of the built-in agent or skill `id`,
    /// through Library, to edit; a copy already there is theirs and is kept.
    pub(crate) async fn copy_setup_skill(&self, id: &str) -> Result<CommandResult, ProtocolError> {
        let CopyPlace {
            library,
            root,
            path,
            text,
            what,
        } = self.setup_copy_place(id).await?;
        match library.create(root.root_id, &path, text).await {
            Ok(revision) => {
                self.copied_from(id, text)?;
                self.library_changed(root.root_id, &path, Some(revision))?;
            }
            Err(ports::LibraryError::AlreadyExists) => {}
            Err(e) => {
                return Err(error(
                    e.code(),
                    format!("Couldn't copy the {what} {id}: {e}"),
                ));
            }
        }
        Ok(Self::skill_copied(id, &root, path))
    }

    /// For the person's copy of the built-in agent or skill `id`, when
    /// Øphio's is newer: keep theirs and stop offering this version, or move
    /// theirs to Library's recently deleted, where it can be restored, and
    /// copy the new version in its place.
    pub(crate) async fn choose_setup_skill_version(
        &self,
        id: &str,
        keep_mine: bool,
    ) -> Result<CommandResult, ProtocolError> {
        let CopyPlace {
            library,
            root,
            path,
            text,
            ..
        } = self.setup_copy_place(id).await?;
        if keep_mine {
            self.copied_from(id, text)?;
            return Ok(Self::skill_copied(id, &root, path));
        }
        match library.read(root.root_id, &path).await {
            Ok(mine) if mine.text == text => {}
            Ok(mine) => {
                library
                    .trash(root.root_id, &path, &mine.revision)
                    .await
                    .map_err(crate::plumbing::library_error)?;
                self.library_changed(root.root_id, &path, None)?;
            }
            Err(ports::LibraryError::NotFound) => {}
            Err(e) => return Err(crate::plumbing::library_error(e)),
        }
        match library.create(root.root_id, &path, text).await {
            Ok(revision) => self.library_changed(root.root_id, &path, Some(revision))?,
            // Already the new version.
            Err(ports::LibraryError::AlreadyExists) => {}
            Err(e) => return Err(crate::plumbing::library_error(e)),
        }
        self.copied_from(id, text)?;
        Ok(Self::skill_copied(id, &root, path))
    }

    /// The task's saved start, for a run in `cwd`. The harness finds the
    /// entry itself only while it would find the file read when the task was
    /// made; otherwise the run is given the saved copy.
    pub(crate) async fn run_start(
        &self,
        task_id: protocol::TaskId,
        cwd: &Path,
    ) -> Result<Option<RunStart>, ProtocolError> {
        let Some(mut start) = self.read(|r| r.get::<RunStart>("task_start", task_id))? else {
            return Ok(None);
        };
        // Disabled since the task started from it, its later runs aren't
        // given it again.
        if self.refuse_disabled_start(&start).is_err() {
            tracing::info!(%task_id, "What the task started from is disabled in Ophio; the run isn't given it");
            return Ok(None);
        }
        if start.native_file().is_some() {
            let dirs = self.inner.launchers.clone();
            // As kept, not resolved: a project folder replaced by a link is
            // then found replaced, not taken for where the link leads.
            let cwd = cwd.to_path_buf();
            let checked = start.clone();
            let look = LOOKS
                .acquire()
                .await
                .map_err(|_| internal("Looking for agents and skills"))?;
            let kept = tokio::task::spawn_blocking(move || {
                let _look = look;
                still_native(&dirs, &cwd, &checked)
            })
            .await
            .unwrap_or(false);
            if !kept {
                tracing::info!(%task_id, "Its agent or skill changed or is ambiguous; the run is given the saved copy");
                start.drop_native();
            }
        }
        Ok(Some(start))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_built_in_agent_and_skill_reads_as_one() {
        let folder = Path::new("setup-skills");
        let mut ids = HashSet::new();
        for (kind, id) in built_in() {
            assert!(valid_id(id), "{id}");
            // One copy-bases map holds both, by id.
            assert!(ids.insert(id), "{id} is both an agent and a skill");
            let text = setup_text(kind, id).unwrap();
            let found = setup_entry(kind, folder, id).unwrap_or_else(|| panic!("{id}"));
            assert!(
                found.named.is_some() && found.entry.description.is_some(),
                "{id}"
            );
            assert!(
                text.ends_with('\n') && text.matches("\n---\n").count() == 1,
                "{id}"
            );
            assert!(text.len() as u64 <= MAX_FILE, "{id}");
            assert_eq!(found.entry.launch_policy, None, "{id}");
            assert_eq!(found.entry.harness, None, "{id}");
            let lower = text.to_lowercase();
            for word in ["fung", "mycel", "spore", "hypha", "mushroom"] {
                assert!(!lower.contains(word), "{id} says {word}");
            }
        }
        assert_eq!(ids.len(), 33);
    }

    #[test]
    fn built_in_agents_review_as_find_and_fix_does() {
        let text = |id| setup_text(CatalogKind::Agent, id).unwrap();
        let (_, method) = frontmatter(crate::setup_agents::LIVE_TESTING);
        let method = method.trim();
        for id in ["everyday-user", "novice-user", "power-user"] {
            assert!(text(id).contains(method), "{id} tests as the skill says");
            assert!(!text(id).contains("## How to work"), "{id}");
        }
        for id in ["senior-engineer", "bug-hunter", "docs-reviewer"] {
            assert!(text(id).contains("## How to work"), "{id}");
            assert!(!text(id).contains(method), "{id}");
            // Nothing is on GitHub here.
            assert!(!text(id).contains("GitHub"), "{id}");
        }
        assert!(
            text("bug-hunter").contains(crate::github_after::profile_brief(Some(
                protocol::github::FindFocus::Bugs
            )))
        );
        assert!(text("senior-engineer").contains(crate::github_after::profile_brief(None)));
    }

    #[test]
    fn frontmatter_reads_what_agents_and_skills_use() {
        let text = "---\nname: \"imagegen\"\ndescription: >\n  Makes images\n  and more.\n\n  Second.\nmetadata:\n  short-description: x\nmodel: opus # best\neffort: 'it''s high'\ntools: [Read, Grep]\n---\n\nBody here.\n";
        let (fields, body) = frontmatter(text);
        assert_eq!(fields["name"], "imagegen");
        assert_eq!(fields["description"], "Makes images and more.\nSecond.");
        assert_eq!(fields["model"], "opus");
        assert_eq!(fields["effort"], "it's high");
        assert!(!fields.contains_key("metadata"));
        assert!(!fields.contains_key("short-description"));
        assert_eq!(body.trim(), "Body here.");
        let (fields, body) = frontmatter("No frontmatter\n---\n");
        assert!(fields.is_empty());
        assert_eq!(body, "No frontmatter\n---\n");
        let (fields, _) = frontmatter("---\nname: open\n");
        assert!(fields.is_empty(), "an unclosed block is not frontmatter");
        let (fields, _) =
            frontmatter("---\ndescription: |\n  line one\n  line two\nname: a\n---\n");
        assert_eq!(fields["description"], "line one\nline two");
        assert_eq!(fields["name"], "a");
    }

    #[test]
    fn ids_are_plain_names() {
        for bad in ["", ".hidden", "..", "a/b", "a\\b", "c:", "x\u{0}"] {
            assert!(!valid_id(bad), "{bad:?}");
        }
        assert!(valid_id("launch-video-clone"));
        assert!(valid_id("Video producer"));
        assert!(native_name("launch-video-clone"));
        assert!(!native_name("Video producer"));
    }

    #[cfg(unix)]
    #[test]
    fn a_file_is_read_once_bounded_and_never_from_outside_its_project() {
        let base = ports::library::canonical(&std::env::temp_dir())
            .unwrap()
            .join(format!("ophio-read-{}", uuid::Uuid::now_v7()));
        let (project, outside) = (base.join("project"), base.join("outside"));
        let agents = project.join(".claude/agents");
        std::fs::create_dir_all(&agents).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let secret = outside.join("secret.md");
        std::fs::write(&secret, "---\nname: secret\n---\n").unwrap();
        let read = |name: &str| read_file(&agents.join(name), Some(&project));

        std::fs::write(agents.join("plain.md"), "Plain.").unwrap();
        assert_eq!(read("plain.md").unwrap().unwrap(), b"Plain.");
        assert_eq!(read("missing.md").unwrap(), None);
        // A link out of the project, or another name for a file outside it.
        std::os::unix::fs::symlink(&secret, agents.join("link.md")).unwrap();
        assert!(
            read("link.md")
                .unwrap_err()
                .contains("leads outside the project")
        );
        std::fs::hard_link(&secret, agents.join("hard.md")).unwrap();
        assert!(
            read("hard.md")
                .unwrap_err()
                .contains("linked to another name")
        );
        // A folder on the way that leads out.
        let escaped = project.join("escaped");
        std::os::unix::fs::symlink(&outside, &escaped).unwrap();
        assert!(
            read_file(&escaped.join("secret.md"), Some(&project))
                .unwrap_err()
                .contains("leads outside the project")
        );
        // The person's own folders may lead anywhere.
        assert!(read_file(&agents.join("link.md"), None).unwrap().is_some());
        // A FIFO with no writer isn't waited on.
        rustix::fs::mknodat(
            rustix::fs::CWD,
            agents.join("fifo.md"),
            rustix::fs::FileType::Fifo,
            rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
            0,
        )
        .unwrap();
        assert_eq!(read("fifo.md").unwrap(), None);
        // Up to 64 KB.
        std::fs::write(agents.join("full.md"), vec![b'x'; MAX_FILE as usize]).unwrap();
        assert_eq!(read("full.md").unwrap().unwrap().len(), MAX_FILE as usize);
        std::fs::write(agents.join("over.md"), vec![b'x'; MAX_FILE as usize + 1]).unwrap();
        assert!(read("over.md").unwrap_err().contains("larger than 64 KB"));
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn a_full_folder_is_listed_in_part() {
        let base = ports::library::canonical(&std::env::temp_dir())
            .unwrap()
            .join(format!("ophio-full-{}", uuid::Uuid::now_v7()));
        let agents = base.join("claude/agents");
        std::fs::create_dir_all(&agents).unwrap();
        for n in 0..MAX_LISTED + 10 {
            std::fs::write(agents.join(format!("a{n:05}.txt")), "").unwrap();
        }
        let dirs = LauncherDirs {
            claude_home: Some(base.join("claude")),
            ..LauncherDirs::default()
        };
        let mut truncated = false;
        assert_eq!(
            names(&agents, None, &mut truncated).unwrap().len(),
            MAX_LISTED
        );
        assert!(truncated);
        let catalog = scan(&dirs, None, &[], &CopyBases::new());
        assert!(catalog.truncated && catalog.entries.is_empty());
        // Files opened are counted across folders too.
        std::fs::remove_dir_all(&agents).unwrap();
        std::fs::create_dir_all(&agents).unwrap();
        let half = MAX_READ / 2 + 10;
        // None of them an entry, so only the files opened add up.
        for n in 0..half {
            std::fs::create_dir_all(agents.join(format!("a{n:05}.md"))).unwrap();
            std::fs::create_dir_all(base.join(format!("claude/skills/s{n:05}"))).unwrap();
        }
        let catalog = scan(&dirs, None, &[], &CopyBases::new());
        assert!(catalog.truncated && catalog.entries.is_empty());
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn a_harness_whose_own_folder_is_unknown_loads_nothing_itself() {
        let base = ports::library::canonical(&std::env::temp_dir())
            .unwrap()
            .join(format!("ophio-unknown-home-{}", uuid::Uuid::now_v7()));
        let project = base.join("project");
        let agents = project.join(".claude/agents");
        std::fs::create_dir_all(&agents).unwrap();
        std::fs::create_dir_all(base.join("claude")).unwrap();
        std::fs::write(agents.join("reviewer.md"), "Be exacting.\n").unwrap();
        let found = read_entry(
            CatalogKind::Agent,
            CatalogSource::ClaudeProject,
            &agents,
            "reviewer",
            Some(&project),
        )
        .unwrap()
        .unwrap();
        let start = RunStart::Agent {
            name: "reviewer".into(),
            instructions: "Be exacting.".into(),
            native: Some(NativeEntry {
                harness: HarnessKind::ClaudeCode,
                name: "reviewer".into(),
                project: true,
            }),
            file: found.path,
            digest: found.entry.digest,
        };
        let mut dirs = LauncherDirs {
            claude_home: Some(base.join("claude")),
            ..LauncherDirs::default()
        };
        assert!(still_native(&dirs, &project, &start));
        // Its own agents, wherever they are, could have the same name.
        dirs.claude_home = None;
        assert!(!still_native(&dirs, &project, &start));
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_projects_folders_are_listed_only_within_it() {
        let base = ports::library::canonical(&std::env::temp_dir())
            .unwrap()
            .join(format!("ophio-listed-{}", uuid::Uuid::now_v7()));
        let (project, outside) = (base.join("project"), base.join("private"));
        let agents = project.join(".claude/agents");
        std::fs::create_dir_all(&agents).unwrap();
        std::fs::create_dir_all(project.join(".claude")).unwrap();
        std::fs::create_dir_all(outside.join("tax-return-2026")).unwrap();
        std::fs::write(outside.join("tax-return-2026/SKILL.md"), "Secret.\n").unwrap();
        std::os::unix::fs::symlink(&outside, project.join(".claude/skills")).unwrap();
        std::fs::write(agents.join("reviewer.md"), "Be exacting.\n").unwrap();
        let dirs = LauncherDirs {
            claude_home: Some(base.join("claude")),
            ..LauncherDirs::default()
        };
        let catalog = scan(&dirs, Some(&project), &[], &CopyBases::new());
        // Its own agent is listed; what its skills folder leads to isn't
        // read, and neither where it leads nor what is there is said.
        assert_eq!(
            catalog
                .entries
                .iter()
                .map(|e| e.id.as_str())
                .collect::<Vec<_>>(),
            ["reviewer"]
        );
        assert_eq!(
            catalog.problems,
            [format!(
                "{} leads outside the project, so it isn't listed",
                project.join(".claude/skills").display()
            )]
        );
        let shown = catalog.problems.join("\n");
        assert!(!shown.contains("private") && !shown.contains("tax-return"));

        // An agents folder within it that leads out can't be checked.
        let found = read_entry(
            CatalogKind::Agent,
            CatalogSource::ClaudeProject,
            &agents,
            "reviewer",
            Some(&project),
        )
        .unwrap()
        .unwrap();
        let start = RunStart::Agent {
            name: "reviewer".into(),
            instructions: "Be exacting.".into(),
            native: Some(NativeEntry {
                harness: HarnessKind::ClaudeCode,
                name: "reviewer".into(),
                project: true,
            }),
            file: found.path,
            digest: found.entry.digest,
        };
        std::fs::create_dir_all(base.join("claude")).unwrap();
        std::fs::create_dir_all(agents.join("team")).unwrap();
        // A link to nothing is no folder, and nothing the harness finds.
        std::os::unix::fs::symlink("missing", agents.join("team/stale")).unwrap();
        assert!(still_native(&dirs, &project, &start));
        // Even with nothing there to find, what is outside isn't looked in.
        let empty = base.join("empty");
        std::fs::create_dir(&empty).unwrap();
        std::os::unix::fs::symlink(&empty, agents.join("team/private")).unwrap();
        assert!(!still_native(&dirs, &project, &start));
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_replaced_project_folder_lists_and_vouches_for_nothing() {
        let base = ports::library::canonical(&std::env::temp_dir())
            .unwrap()
            .join(format!("ophio-replaced-{}", uuid::Uuid::now_v7()));
        let (project, moved) = (base.join("project"), base.join("moved"));
        std::fs::create_dir_all(&project).unwrap();
        let mine = base.join("claude/agents");
        std::fs::create_dir_all(&mine).unwrap();
        std::fs::write(mine.join("helper.md"), "Help.\n").unwrap();
        let dirs = LauncherDirs {
            claude_home: Some(base.join("claude")),
            ..LauncherDirs::default()
        };
        let found = read_entry(
            CatalogKind::Agent,
            CatalogSource::ClaudeUser,
            &mine,
            "helper",
            None,
        )
        .unwrap()
        .unwrap();
        let start = RunStart::Agent {
            name: "helper".into(),
            instructions: "Help.".into(),
            native: Some(NativeEntry {
                harness: HarnessKind::ClaudeCode,
                name: "helper".into(),
                project: false,
            }),
            file: found.path,
            digest: found.entry.digest,
        };
        assert!(still_native(&dirs, &project, &start));
        // The project's folder moved and a link to it in its place: what the
        // harness would find there can't be told, even with nothing there.
        std::fs::rename(&project, &moved).unwrap();
        std::os::unix::fs::symlink(&moved, &project).unwrap();
        assert!(!still_native(&dirs, &project, &start));
        // Nothing of what is there is listed or said.
        std::fs::create_dir_all(moved.join(".claude/agents")).unwrap();
        std::fs::write(moved.join(".claude/agents/reviewer.md"), "Be exacting.\n").unwrap();
        assert!(!still_native(&dirs, &project, &start));
        let catalog = scan(&dirs, Some(&project), &[], &CopyBases::new());
        assert_eq!(
            catalog
                .entries
                .iter()
                .map(|e| e.id.as_str())
                .collect::<Vec<_>>(),
            ["helper"]
        );
        assert!(catalog.problems.is_empty(), "{:?}", catalog.problems);
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn dates_and_slugs() {
        assert_eq!(utc_date(0), "1970-01-01");
        assert_eq!(utc_date(1_790_985_600_000), "2026-10-03");
        assert_eq!(utc_date(951_782_400_000), "2000-02-29");
        assert!(valid_date("2026-10-03"));
        assert!(!valid_date("2026-13-03"));
        assert!(!valid_date("2026/10/03"));
        assert_eq!(
            slug("30-second launch video for Øphio!"),
            "30-second-launch-video-for-phio"
        );
        assert_eq!(slug("…"), "task");
        assert!(slug(&"long ".repeat(40)).len() <= 48);
    }

    #[test]
    fn automatic_roots_never_overlap_the_persons_own_or_ophios_private_folders() {
        let base = ports::library::canonical(&std::env::temp_dir())
            .unwrap()
            .join(format!("ophio-roots-{}", uuid::Uuid::now_v7()));
        let (one_offs, agents) = (base.join("Ophio"), base.join("Agents"));
        std::fs::create_dir_all(&one_offs).unwrap();
        let dirs = LauncherDirs {
            ophio_agents: Some(agents.clone()),
            one_offs: Some(one_offs.clone()),
            ..LauncherDirs::default()
        };
        let spec = |path: &Path| RootSpec {
            root_id: protocol::LibraryRootId::new(),
            label: "Mine".into(),
            path: path.to_path_buf(),
            kind: RootKind::Notes,
            writable: true,
            project_id: None,
            harness: None,
            only: None,
        };
        let paths = |roots: &[RootSpec]| roots.iter().map(|r| r.path.clone()).collect::<Vec<_>>();
        // A folder not made yet isn't added.
        assert_eq!(
            paths(&automatic_roots(&dirs, &[], &[])),
            std::slice::from_ref(&one_offs)
        );
        std::fs::create_dir_all(&agents).unwrap();
        let both = automatic_roots(&dirs, &[], &[]);
        assert_eq!(paths(&both), [one_offs.clone(), agents.clone()]);
        assert_eq!(both[0].kind, RootKind::Notes);
        assert_eq!(both[0].label, "Ophio");
        assert_eq!(
            (both[1].kind, both[1].label.as_str()),
            (RootKind::Agents, "Ophio agents")
        );
        assert!(both.iter().all(|r| r.writable && r.project_id.is_none()));
        // Ids come from the paths, so they last.
        assert_eq!(
            both.iter().map(|r| r.root_id).collect::<Vec<_>>(),
            automatic_roots(&dirs, &[], &[])
                .iter()
                .map(|r| r.root_id)
                .collect::<Vec<_>>()
        );
        assert_ne!(both[0].root_id, both[1].root_id);
        // Inside or around one of the person's own roots, or a private folder,
        // a folder is left out.
        assert_eq!(
            paths(&automatic_roots(&dirs, &[spec(&base)], &[])),
            Vec::<PathBuf>::new()
        );
        assert_eq!(
            paths(&automatic_roots(
                &dirs,
                &[spec(&one_offs.join("deep"))],
                &[]
            )),
            std::slice::from_ref(&agents)
        );
        assert_eq!(
            paths(&automatic_roots(&dirs, &[], &[agents.join("private")])),
            std::slice::from_ref(&one_offs)
        );
        // Agents kept in the one-offs folder are reached through it.
        let nested = LauncherDirs {
            ophio_agents: Some(one_offs.join("Agents")),
            ..dirs.clone()
        };
        std::fs::create_dir_all(one_offs.join("Agents")).unwrap();
        assert_eq!(
            paths(&automatic_roots(&nested, &[], &[])),
            std::slice::from_ref(&one_offs)
        );
        // A file where a folder should be is no folder.
        let file = LauncherDirs {
            one_offs: Some(base.join("file")),
            ..dirs.clone()
        };
        std::fs::write(base.join("file"), "x").unwrap();
        assert_eq!(
            paths(&automatic_roots(&file, &[], &[])),
            std::slice::from_ref(&agents)
        );
        // Skills copied to edit have a root of their own too.
        let skills = base.join("Skills");
        std::fs::create_dir_all(&skills).unwrap();
        let with_skills = LauncherDirs {
            ophio_skills: Some(skills.clone()),
            ..dirs.clone()
        };
        let all = automatic_roots(&with_skills, &[], &[]);
        assert_eq!(paths(&all), [one_offs.clone(), agents.clone(), skills]);
        assert_eq!(
            (all[2].kind, all[2].label.as_str()),
            (RootKind::Skills, "Ophio skills")
        );
        std::fs::remove_dir_all(&base).unwrap();
    }
}
