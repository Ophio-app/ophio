// SPDX-License-Identifier: Apache-2.0
//! The person's own instructions and skills, which a sandboxed run reads and
//! never changes.
//!
//! A sandboxed run gets a settings folder of its own conversation's, so the
//! agent would otherwise start without the instructions and skills the person
//! keeps in their real one. Only what [`CLAUDE`] and [`CODEX`] name is shared,
//! and only read-only: settings, tool servers, hooks, plugins, subagents,
//! commands, sessions, memories, history and databases never are.
//!
//! A link is followed once, to the real place it names, and only if that place
//! has no further link in it. Nothing shared may be the whole computer, the
//! home folder or a folder holding it or the working folder, or be, hold or sit
//! inside anything private: either agent's settings folder (but for what is
//! really in this agent's own `skills/` and `rules/`), Ophio's data, signed-in
//! accounts and the private paths the run is given. Something refused is left
//! out, noted once in the log by name only, and the run starts without it.
//! Links inside a shared skill folder are not followed out of it: the agent
//! sees them as the sandbox does, where they lead nowhere outside its reach.
use crate::sys::Kind;
use protocol::capability::HarnessKind;
use std::{
    collections::{HashMap, HashSet},
    ffi::{OsStr, OsString},
    fs::File,
    io::{self, Read},
    os::unix::fs::MetadataExt,
    path::{Component, Path, PathBuf},
    sync::{Mutex, OnceLock},
};

/// Something in an agent's settings folder that is shared when it is there.
#[derive(Clone, Copy, Debug)]
enum Item {
    /// A file of instructions the agent reads as it starts.
    File(&'static str),
    /// A folder whose entries are each shared under the same name, but for
    /// the names the agent keeps up itself.
    Folder(&'static str, Entries, &'static [&'static str]),
}
#[derive(Clone, Copy, Debug, PartialEq)]
enum Entries {
    /// Folders the agent finds its skills in, each with a `SKILL.md`.
    Skills,
    /// Further files of instructions, and folders of them.
    Rules,
}

/// Claude Code reads `CLAUDE.md` and the files in `rules/` as the person's own
/// instructions, and finds their skills in `skills/`. It fills
/// `skills/synced/` itself from the person's account as it starts, so that is
/// left to the conversation's own copy, where it can write.
const CLAUDE: &[Item] = &[
    Item::File("CLAUDE.md"),
    Item::Folder("rules", Entries::Rules, &[]),
    Item::Folder("skills", Entries::Skills, &["synced"]),
];
/// Codex reads `AGENTS.override.md`, or else `AGENTS.md`, and finds skills in
/// `skills/`. Its `rules/` is command-approval policy, not instructions.
const CODEX: &[Item] = &[
    Item::File("AGENTS.md"),
    Item::File("AGENTS.override.md"),
    Item::Folder("skills", Entries::Skills, &[]),
];

/// How many entries of one folder are looked at.
const MOST: usize = 256;
/// How much of a `SKILL.md` is read to find its front matter.
const HEAD: u64 = 16 * 1024;
/// The longest name of a folder entry, in bytes: 255 on Linux, and on a Mac
/// up to 255 characters of up to 4 bytes each.
const NAME: usize = 255 * 4;
/// How much of the record of what was shared last time is read: enough for
/// the longest list [`select`] can give, every folder at its fullest and
/// every name at its longest, each on a line of its own.
const RECORD_MOST: u64 = {
    let mut most = 0;
    let lists = [CLAUDE, CODEX];
    let mut l = 0;
    while l < lists.len() {
        let mut size = 0;
        let mut i = 0;
        while i < lists[l].len() {
            size += match lists[l][i] {
                Item::File(name) => name.len() + 1,
                Item::Folder(name, _, _) => MOST * (name.len() + 1 + NAME + 1),
            };
            i += 1;
        }
        if size > most {
            most = size;
        }
        l += 1;
    }
    most as u64
};
/// Where the names shared last time are kept, beside the conversation's
/// settings folder and out of the agent's reach.
pub const RECORD: &str = "shared-setup";
/// Carries [`expected`] to the sandbox's own program, which removes it before
/// it starts the agent.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub const VARIABLE: &str = "OPHIO_SANDBOX_SHARED";

/// What a shared item must keep clear of.
#[derive(Debug)]
pub struct Bounds<'a> {
    pub home: &'a Path,
    pub cwd: &'a Path,
    /// Paths nothing shared may be, hold or be inside.
    pub private: &'a [PathBuf],
    /// Whether the run may change any of the person's files. A run that can't
    /// is not shown skills that say they change files.
    pub writes: bool,
    /// What the person disabled in Øphio, from [`withheld`]: nothing shared
    /// is, holds or leads to one of these.
    pub withheld: &'a [PathBuf],
}

/// One of the person's instructions files or skills, checked and held open.
#[derive(Debug)]
pub struct Shared {
    /// Where the agent finds it, relative to its settings folder.
    pub relative: PathBuf,
    /// Where it really is, with no link in it.
    pub source: PathBuf,
    pub directory: bool,
    /// What was checked. Linux mounts by path and checks, before the agent
    /// starts, that the path still leads to this; holding it open keeps its
    /// number from being reused. A Mac's sandbox checks the real path
    /// itself as each file is opened, so only Linux needs these.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub dev: u64,
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub ino: u64,
    _held: File,
}

/// The paths [`Bounds::private`] needs besides the run's own private list:
/// both agents' default settings folders and Claude Code's account file, each
/// in its real spelling too.
pub fn private(home: &Path, given: impl IntoIterator<Item = PathBuf>) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = given.into_iter().collect();
    paths.extend([
        home.join(".claude"),
        home.join(".codex"),
        home.join(".claude.json"),
    ]);
    let real: Vec<PathBuf> = paths.iter().filter_map(|p| p.canonicalize().ok()).collect();
    paths.extend(real);
    paths.sort();
    paths.dedup();
    paths
}

/// The paths of what the run is told to leave out, each in its real
/// spelling too: Disable in Øphio names a file by the path the person's agent
/// finds it at, which the sandbox's copy doesn't have, so it isn't shared.
pub fn withheld(disabled: &ports::Disabled) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = disabled
        .files
        .iter()
        .chain(disabled.skills.iter().map(|entry| &entry.path))
        .cloned()
        .collect();
    let real: Vec<PathBuf> = paths.iter().filter_map(|p| p.canonicalize().ok()).collect();
    paths.extend(real);
    paths.sort();
    paths.dedup();
    paths
}

/// What of the person's own settings folder `config` (in its real spelling)
/// a run of `kind` is given.
pub fn select(kind: HarnessKind, config: &Path, bounds: &Bounds<'_>) -> Vec<Shared> {
    let items = match kind {
        HarnessKind::ClaudeCode => CLAUDE,
        HarnessKind::Codex => CODEX,
        _ => return Vec::new(),
    };
    // The sandbox's list of what to check names each place by this path.
    if config
        .to_str()
        .is_none_or(|s| s.chars().any(char::is_control))
    {
        note("the settings folder", "its path can't be checked");
        return Vec::new();
    }
    let mut shared = Vec::new();
    for item in items {
        match *item {
            Item::File(name) => {
                if let Some(found) = one(config, Path::new(name), None, bounds) {
                    shared.push(found);
                }
            }
            Item::Folder(folder, entries, theirs) => {
                let path = config.join(folder);
                // Only the agent's own folder: one that is a link is not followed.
                match std::fs::symlink_metadata(&path) {
                    Ok(m) if m.is_dir() => {}
                    Ok(_) => {
                        note(folder, "it is not a folder of its own");
                        continue;
                    }
                    Err(_) => continue,
                }
                let Ok((_held, list)) = crate::sys::history_entries(&path) else {
                    note(folder, "it can't be listed");
                    continue;
                };
                let mut names: Vec<_> = list
                    .flatten()
                    .map(|e| e.file_name())
                    // Hidden entries are the agent's own, such as Codex's `.system`.
                    .filter(|n| !n.as_encoded_bytes().starts_with(b"."))
                    .filter(|n| !theirs.iter().any(|t| n == *t))
                    .collect();
                names.sort();
                if names.len() > MOST {
                    note(folder, "it has too many entries; the rest are left out");
                    names.truncate(MOST);
                }
                for name in names {
                    let relative = Path::new(folder).join(name);
                    if let Some(found) = one(config, &relative, Some(entries), bounds) {
                        shared.push(found);
                    }
                }
            }
        }
    }
    shared
}

/// Checks the entry at `relative`. `entries` says which folder it is in.
fn one(
    config: &Path,
    relative: &Path,
    entries: Option<Entries>,
    bounds: &Bounds<'_>,
) -> Option<Shared> {
    let label = relative.to_string_lossy();
    let named = config.join(relative);
    let metadata = std::fs::symlink_metadata(&named).ok()?;
    let refuse = |why: &str| {
        note(&label, why);
        None
    };
    // Each name goes into the sandbox's list of what to check, one per line.
    if relative
        .to_str()
        .is_none_or(|s| s.chars().any(char::is_control))
    {
        return refuse("its name can't be checked");
    }
    let source = match real(&named) {
        Ok(source) => source,
        Err(_) => return refuse("its link leads nowhere, or through another link"),
    };
    if bounds
        .withheld
        .iter()
        .any(|w| w.starts_with(&named) || w.starts_with(&source))
    {
        // The person's own choice, so nothing to log.
        return None;
    }
    // Where it would be without any link: the agent's own, in its own folder.
    let natural = source == named;
    if let Some(why) = outside(&source, natural, config, bounds) {
        return refuse(why);
    }
    let skill = entries == Some(Entries::Skills);
    let target = std::fs::metadata(&source).ok()?;
    let (held, directory) = if target.is_dir() {
        if entries.is_none() {
            return refuse("it is a folder, not a file");
        }
        let held = match crate::sys::folder(&source) {
            Ok(held) => held,
            Err(_) => return refuse("it changed while it was checked"),
        };
        if skill {
            match crate::sys::child(&held, "SKILL.md") {
                Ok(file) if !bounds.writes && changes_files(&file) => {
                    hidden(&label);
                    return None;
                }
                Ok(_) => {}
                // A folder of skills, as the agent keeps one it fetched, is shared
                // whole when it is really in the agent's own folder.
                Err(_) if natural && !metadata.is_symlink() => {}
                Err(_) => return refuse("it has no SKILL.md"),
            }
        }
        (held, true)
    } else if target.is_file() {
        if skill {
            // A file beside the skills is not one.
            return None;
        }
        match crate::sys::history_file(&source, false) {
            Ok(held) => (held, false),
            Err(_) => return refuse("it isn't a plain file with one name"),
        }
    } else {
        return refuse("it is neither a file nor a folder");
    };
    let checked = held.metadata().ok()?;
    Some(Shared {
        relative: relative.to_owned(),
        source,
        directory,
        dev: checked.dev(),
        ino: checked.ino(),
        _held: held,
    })
}

/// The real place `path` names. A link is followed once, and the place it
/// names must have no further link in it, so nobody who can change a folder on
/// the way can turn it towards something else.
fn real(path: &Path) -> io::Result<PathBuf> {
    let named = if std::fs::symlink_metadata(path)?.is_symlink() {
        let parent = path.parent().ok_or_else(|| io::Error::other("no parent"))?;
        lexical(&parent.join(std::fs::read_link(path)?))
    } else {
        path.to_owned()
    };
    let source = named.canonicalize()?;
    if source != named {
        return Err(io::Error::other("a link leads through another link"));
    }
    Ok(source)
}

fn lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

/// Why `source` must not be shared, if it mustn't. `natural` is something
/// really in the agent's own settings folder, at the place it is shared at.
fn outside(
    source: &Path,
    natural: bool,
    config: &Path,
    bounds: &Bounds<'_>,
) -> Option<&'static str> {
    if source.parent().is_none() {
        return Some("it is the whole computer");
    }
    if bounds.home.starts_with(source) {
        return Some("it is the home folder or holds it");
    }
    if bounds.cwd.starts_with(source) && bounds.cwd != source {
        return Some("it holds the working folder");
    }
    if [
        "/proc",
        "/sys",
        "/dev",
        "/run",
        "/var/run",
        "/private/var/run",
    ]
    .iter()
    .any(|p| source.starts_with(p))
    {
        return Some("it is the system's running state");
    }
    for private in bounds.private.iter().map(PathBuf::as_path).chain([config]) {
        if natural && private == config {
            continue;
        }
        if source.starts_with(private) || private.starts_with(source) {
            return Some("it is, holds or is inside something private");
        }
    }
    None
}

/// Whether a skill's front matter says it changes files: its `allowed-tools`
/// names a tool that edits them, or its `metadata` has `ophio-access: write`.
/// Only the front matter at the start of the file is read, and one that isn't
/// complete in that much says nothing.
fn changes_files(file: &File) -> bool {
    let mut head = Vec::new();
    if file.take(HEAD).read_to_end(&mut head).is_err() {
        return false;
    }
    declares_writing(&String::from_utf8_lossy(&head))
}

fn declares_writing(text: &str) -> bool {
    const EDITORS: &[&str] = &["Write", "Edit", "MultiEdit", "NotebookEdit", "apply_patch"];
    let edits = |list: &str| {
        list.split([' ', ',', '[', ']', '"', '\''])
            .map(|tool| tool.split('(').next().unwrap_or_default())
            .any(|tool| EDITORS.contains(&tool))
    };
    let marked = |pair: &str| {
        pair.split_once(':').is_some_and(|(key, value)| {
            key.trim().trim_matches(['"', '\'']) == "ophio-access"
                && value.trim().trim_matches(['"', '\'']) == "write"
        })
    };
    let mut lines = text.strip_prefix('\u{feff}').unwrap_or(text).lines();
    if lines.next().map(str::trim_end) != Some("---") {
        return false;
    }
    let (mut section, mut found) = ("", false);
    for line in lines {
        if line.trim_end() == "---" {
            return found;
        }
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        // A list may sit under its key without indenting.
        if !line.starts_with([' ', '\t', '-']) {
            let (key, value) = line.split_once(':').unwrap_or((line, ""));
            section = key.trim().trim_matches(['"', '\'']);
            let value = value.trim();
            found |= match section {
                "allowed-tools" => edits(value),
                "metadata" => value
                    .strip_prefix('{')
                    .and_then(|v| v.strip_suffix('}'))
                    .is_some_and(|v| v.split(',').any(marked)),
                _ => false,
            };
        } else if section == "allowed-tools" {
            found |= line.trim().strip_prefix('-').is_some_and(edits);
        } else if section == "metadata" {
            found |= marked(line);
        }
    }
    false
}

/// Names `label` in the log once, with why it was left out.
fn note(label: &str, why: &str) {
    if first(format!("{label}\0{why}")) {
        tracing::warn!(item = label, reason = why, "not shared with a limited chat");
    }
}
fn hidden(label: &str) {
    if first(format!("{label}\0hidden")) {
        tracing::info!(
            item = label,
            "skill says it changes files; not shown to a read-only limited chat"
        );
    }
}
fn first(key: String) -> bool {
    static SEEN: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    SEEN.get_or_init(Default::default)
        .lock()
        .map(|mut seen| seen.insert(key))
        .unwrap_or(false)
}

/// Makes the places `shared` appears at in the conversation's settings folder
/// `native`, and clears the places of what was shared last time and is no
/// longer. With `link`, each place is a link to the real one, which a Mac's
/// sandbox resolves before it decides; otherwise it is an empty folder or file
/// for Linux to mount the real one on. What can't be placed is left out.
///
/// The agent may change `native` during a turn, and a process it left behind
/// may still be changing it now. So `native` and each folder in it are opened
/// once, through no link, and every check, removal and creation is made
/// through those handles by single names: a link the agent planted in place of
/// a folder is never followed, and swapping a folder for a link after it was
/// opened redirects nothing. Nothing is removed but a link, an empty folder or
/// an empty file, as long as nothing else is writing the folder at that
/// moment; either way only the conversation's own files can be touched. What
/// else the agent left where a place goes stays under the mount on Linux when
/// it is of the same kind (a folder for a folder, a file for a file), and is
/// otherwise moved aside in the same folder, as it always is on a Mac.
///
/// That makes preparing the places safe. On Linux the shim then checks each
/// mount, so the agent starts only if it sees the person's own item at each
/// place. A Mac has no such check: a process the agent left behind can still
/// replace a link after this returns, and the agent then reads its own
/// content under that name. What a Mac's sandbox guarantees is only that the
/// agent can read nothing it wasn't already allowed to, since it resolves
/// every real path before it decides.
pub fn place(native: &Path, state: &Path, shared: Vec<Shared>, link: bool) -> Vec<Shared> {
    use crate::sys::{folder_at, remove_at};
    let record = state.join(RECORD);
    let before = crate::sys::history_file(&record, false)
        .ok()
        .and_then(|file| {
            let mut text = String::new();
            file.take(RECORD_MOST).read_to_string(&mut text).ok()?;
            Some(text)
        })
        .unwrap_or_default();
    let root = match crate::sys::directories(native).and_then(|()| crate::sys::folder(native)) {
        Ok(root) => root,
        Err(_) => {
            note(
                "the chat's settings folder",
                "it can't be reached without a link",
            );
            return Vec::new();
        }
    };
    let now: HashSet<&Path> = shared.iter().map(|s| s.relative.as_path()).collect();
    for old in before.split('\n').filter(|l| !l.is_empty()).map(Path::new) {
        if now.contains(old) || !ours(old) {
            continue;
        }
        let Some((folder, name)) = parts(old) else {
            continue;
        };
        // A folder that is now a link, or can't be opened, is left as it is.
        let held;
        let parent = match folder {
            None => &root,
            Some(folder) => match folder_at(&root, folder, false) {
                Ok(opened) => {
                    held = opened;
                    &held
                }
                Err(_) => continue,
            },
        };
        // Only an empty place, or a link, is Ophio's: the agent's own things stay.
        let _ = match crate::sys::kind_at(parent, name) {
            Ok(Some(Kind::Link)) => remove_at(parent, name, false),
            Ok(Some(Kind::Folder)) => remove_at(parent, name, true),
            Ok(Some(Kind::File(0))) if !link => remove_at(parent, name, false),
            _ => Ok(()),
        };
    }
    let mut folders: HashMap<OsString, File> = HashMap::new();
    let mut placed = Vec::new();
    for item in shared {
        let done = parts(&item.relative)
            .ok_or_else(|| io::Error::other("not a place of ours"))
            .and_then(|(folder, name)| {
                let parent = match folder {
                    None => &root,
                    Some(folder) => {
                        if !folders.contains_key(folder) {
                            folders.insert(folder.to_owned(), folder_of_its_own(&root, folder)?);
                        }
                        &folders[folder]
                    }
                };
                if link {
                    linked(parent, name, &item.source)
                } else {
                    mount_point(parent, name, item.directory)
                }
            });
        match done {
            Ok(()) => placed.push(item),
            Err(_) => note(&item.relative.to_string_lossy(), "its place can't be made"),
        }
    }
    let mut text = String::new();
    for item in &placed {
        text.push_str(&item.relative.to_string_lossy());
        text.push('\n');
    }
    if let Err(e) = crate::sys::replace_history(&record, |file| {
        std::io::Write::write_all(file, text.as_bytes())
    }) {
        tracing::warn!(error = %e, "couldn't record what a limited chat was given");
    }
    placed
}

/// The folder a place of ours is in, if any, and its own name.
fn parts(relative: &Path) -> Option<(Option<&OsStr>, &OsStr)> {
    let names: Vec<&OsStr> = relative
        .components()
        .map(|part| match part {
            Component::Normal(name) => Some(name),
            _ => None,
        })
        .collect::<Option<_>>()?;
    match names[..] {
        [name] => Some((None, name)),
        [folder, name] => Some((Some(folder), name)),
        _ => None,
    }
}

/// Only names this module could have written: one or two plain parts, under
/// an item in the lists.
fn ours(relative: &Path) -> bool {
    let parts: Vec<_> = relative.components().collect();
    let first = match parts.first() {
        Some(Component::Normal(name)) => name.to_str().unwrap_or_default(),
        _ => return false,
    };
    let known = CLAUDE.iter().chain(CODEX).any(|item| match item {
        Item::File(name) => parts.len() == 1 && *name == first,
        Item::Folder(name, _, _) => parts.len() == 2 && *name == first,
    });
    known && parts.iter().all(|p| matches!(p, Component::Normal(_)))
}

/// Opens `name` in the folder held by `root` as a real folder, in place of a
/// link or an empty file an earlier turn left there; anything else there is
/// moved aside.
fn folder_of_its_own(root: &File, name: &OsStr) -> io::Result<File> {
    make_way(root, name, Some(Kind::Folder))?;
    crate::sys::folder_at(root, name, true)
}

/// Clears `name` in the folder held by `parent` for a new place, unless what
/// is there is already `keep`. A link, an empty folder or an empty file is
/// removed; anything else is the agent's and is moved aside.
fn make_way(parent: &File, name: &OsStr, keep: Option<Kind>) -> io::Result<bool> {
    use crate::sys::remove_at;
    let found = crate::sys::kind_at(parent, name)?;
    match found {
        None => return Ok(false),
        Some(Kind::Folder) if keep == Some(Kind::Folder) => return Ok(true),
        Some(Kind::File(_)) if keep == Some(Kind::File(0)) => return Ok(true),
        Some(Kind::Link | Kind::Other | Kind::File(0)) => remove_at(parent, name, false)?,
        Some(Kind::Folder) => {
            if remove_at(parent, name, true).is_err() {
                aside(parent, name)?;
            }
        }
        Some(Kind::File(_)) => aside(parent, name)?,
    }
    Ok(false)
}

/// Moves `name` to a hidden name in the same folder, which is never one of
/// the person's shared names and never in the record.
fn aside(parent: &File, name: &OsStr) -> io::Result<()> {
    for n in 0..100 {
        let mut to = OsString::from(".ophio-aside-");
        to.push(name);
        if n > 0 {
            to.push(format!("-{n}"));
        }
        if crate::sys::kind_at(parent, &to)?.is_none() {
            crate::sys::rename_at(parent, name, &to)?;
            note(
                &name.to_string_lossy(),
                "the agent's own was there; it was moved aside",
            );
            return Ok(());
        }
    }
    Err(io::Error::other("no free name to move it aside to"))
}

/// An empty folder or file at `name` to mount on. A mount over a link fails,
/// so a link is replaced; an agent's own folder or file of the right kind
/// stays under the mount.
fn mount_point(parent: &File, name: &OsStr, directory: bool) -> io::Result<()> {
    let keep = if directory {
        Kind::Folder
    } else {
        Kind::File(0)
    };
    if make_way(parent, name, Some(keep))? {
        return Ok(());
    }
    if directory {
        crate::sys::folder_at(parent, name, true).map(drop)
    } else {
        crate::sys::new_file_at(parent, name)
    }
}

/// A link at `name` to `source`, in place of anything an earlier turn left.
fn linked(parent: &File, name: &OsStr, source: &Path) -> io::Result<()> {
    if crate::sys::kind_at(parent, name)? == Some(Kind::Link)
        && crate::sys::read_link_at(parent, name)? == source
    {
        return Ok(());
    }
    make_way(parent, name, None)?;
    crate::sys::link_at(parent, name, source)
}

/// A link at `relative` under the folder held by `root`, leading to `source`,
/// for the Mac's sign-in and history links. Each folder on the way is walked
/// through a handle and made a real folder of the conversation's own: a link
/// the agent planted there is replaced, never followed, so it can neither
/// redirect the link nor stop its chat from starting. At the last name a link
/// that already leads to `source` is kept, a link, an empty folder or an empty
/// file is removed, and anything else is moved aside. Failing that, the error
/// comes back and the run is refused.
///
/// For the `sign_in` link a plain file left at the last name is removed
/// rather than moved aside: it would be a copy of a sign-in, and a copy kept
/// under a hidden name would stay in the conversation's folder for good.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn link_within(root: &File, relative: &Path, source: &Path, sign_in: bool) -> io::Result<()> {
    use std::path::Component;
    let mut names = Vec::new();
    for part in relative.components() {
        match part {
            Component::Normal(name) => names.push(name),
            _ => return Err(io::Error::other("not a plain name under the folder")),
        }
    }
    let Some((last, folders)) = names.split_last() else {
        return Err(io::Error::other("no name to link at"));
    };
    let mut held: Option<File> = None;
    for name in folders {
        let next = folder_of_its_own(held.as_ref().unwrap_or(root), name)?;
        held = Some(next);
    }
    let parent = held.as_ref().unwrap_or(root);
    if sign_in && matches!(crate::sys::kind_at(parent, last)?, Some(Kind::File(_))) {
        crate::sys::remove_at(parent, last, false)?;
    }
    linked(parent, last, source)
}

/// What the sandbox's own program checks before it starts the agent: each
/// shared place, as the agent will see it, and what it must be.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn expected(config: &Path, shared: &[Shared]) -> String {
    shared
        .iter()
        .map(|s| {
            // `select` shares nothing unless every path is plain text.
            let place = config.join(&s.relative);
            format!("{} {} {}\n", s.dev, s.ino, place.to_string_lossy())
        })
        .collect()
}

/// Inside the sandbox: every shared place is what was checked outside it, so
/// nothing changed between the check and the mount.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn verify(expected: &str) -> io::Result<()> {
    // Split on line feeds only: a name may not hold one, nor any other control.
    for line in expected.split('\n').filter(|line| !line.is_empty()) {
        let mut parts = line.splitn(3, ' ');
        let (Some(dev), Some(ino), Some(path)) = (parts.next(), parts.next(), parts.next()) else {
            return Err(io::Error::other("malformed shared setup list"));
        };
        let metadata = std::fs::symlink_metadata(path)?;
        if metadata.dev().to_string() != dev || metadata.ino().to_string() != ino {
            return Err(io::Error::other(
                "one of the person's instructions or skills changed as the run started",
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "setup_tests.rs"]
mod tests;
