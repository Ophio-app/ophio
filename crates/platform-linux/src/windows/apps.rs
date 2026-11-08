// SPDX-License-Identifier: Apache-2.0
use ports::PlatformError;
use std::{
    collections::{BTreeMap, BTreeSet},
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
};
#[derive(Clone, Debug)]
pub(crate) struct Entry {
    pub path: PathBuf,
    pub name: String,
    pub fingerprint: u64,
}
pub(crate) fn roots(environment: &BTreeMap<String, String>) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(home) = environment
        .get("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            environment
                .get("HOME")
                .map(|h| PathBuf::from(h).join(".local/share"))
        })
        && home.is_absolute()
    {
        roots.push(home.join("applications"));
    }
    for dir in environment
        .get("XDG_DATA_DIRS")
        .map(String::as_str)
        .unwrap_or("/usr/local/share:/usr/share")
        .split(':')
    {
        let path = PathBuf::from(dir);
        if path.is_absolute() {
            roots.push(path.join("applications"));
        }
    }
    roots
}
pub(crate) fn program_path(environment: &BTreeMap<String, String>, name: &str) -> Option<PathBuf> {
    if name.contains('/') {
        let path = PathBuf::from(name);
        return (path.is_absolute() && executable(&path)).then_some(path);
    }
    environment
        .get("PATH")
        .into_iter()
        .flat_map(|p| p.split(':'))
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .map(|p| p.join(name))
        .find(|p| executable(p))
}
fn executable(path: &Path) -> bool {
    path.metadata().is_ok_and(|m| {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            m.is_file() && m.permissions().mode() & 0o111 != 0
        }
        #[cfg(not(unix))]
        {
            m.is_file()
        }
    })
}
fn fingerprint(text: &str) -> u64 {
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hash);
    hash.finish()
}
pub(super) fn revalidate(entry: &Entry) -> Result<(), PlatformError> {
    let text = read(&entry.path)?;
    if fingerprint(&text) != entry.fingerprint {
        return Err(PlatformError::TargetNotFound);
    }
    Ok(())
}
fn read(path: &Path) -> Result<String, PlatformError> {
    use std::io::Read;
    let file = std::fs::File::open(path)?;
    if !file.metadata()?.is_file() {
        return Err(PlatformError::TargetNotFound);
    }
    let mut text = String::new();
    file.take(256 * 1024 + 1).read_to_string(&mut text)?;
    if text.len() > 256 * 1024 {
        return Err(PlatformError::Native("Desktop entry is too large".into()));
    }
    Ok(text)
}
/// One desktop entry, as much of it as choosing and opening an app needs.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Desktop {
    /// Its desktop file id, such as `org.gnome.Nautilus.desktop`.
    pub id: String,
    pub path: PathBuf,
    pub name: String,
    pub generic_name: Option<String>,
    pub keywords: Vec<String>,
    pub categories: Vec<String>,
    pub mime_types: Vec<String>,
    /// The program its Exec line starts, without its directory.
    pub program: Option<String>,
    /// Shown in app lists (not NoDisplay).
    pub listed: bool,
    pub fingerprint: u64,
}
fn unescape(value: &str) -> String {
    value
        .replace("\\s", " ")
        .replace("\\n", " ")
        .replace("\\t", " ")
        .replace("\\\\", "\\")
}
fn list(value: Option<&&str>) -> Vec<String> {
    value
        .map(|v| {
            v.split(';')
                .map(|s| unescape(s.trim()))
                .filter(|s| !s.is_empty())
                .take(64)
                .collect()
        })
        .unwrap_or_default()
}
/// The program an Exec line runs: past `env VAR=…` and quotes, and for
/// Flatpak the app id it runs.
fn program(exec: &str) -> Option<String> {
    let mut words = exec.split_whitespace().map(|w| w.trim_matches('"'));
    let mut word = words.next()?;
    if word.ends_with("/env") || word == "env" {
        word = words.find(|w| !w.contains('=') && !w.starts_with('-'))?;
    }
    let base = word.rsplit('/').next()?.to_owned();
    if base == "flatpak" {
        return exec
            .split_whitespace()
            .filter(|w| !w.starts_with('-') && !w.starts_with('%') && *w != "run")
            .nth(1)
            .map(str::to_owned);
    }
    (!base.is_empty() && !base.starts_with('%')).then_some(base)
}
/// Every desktop entry that may be opened here, including ones app lists
/// hide, in data-directory precedence: an id met earlier hides later ones.
pub(crate) fn desktops(
    roots: &[PathBuf],
    environment: &BTreeMap<String, String>,
) -> Result<Vec<Desktop>, PlatformError> {
    let mut seen = BTreeSet::new();
    let mut entries = Vec::new();
    let mut examined = 0;
    for root in roots {
        let mut dirs = vec![(root.clone(), 0)];
        while let Some((dir, depth)) = dirs.pop() {
            if depth > 8 {
                continue;
            }
            let files = match std::fs::read_dir(&dir) {
                Ok(files) => files,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e.into()),
            };
            let mut files = files.collect::<Result<Vec<_>, _>>()?;
            files.sort_by_key(|e| e.path());
            for file in files {
                examined += 1;
                if examined > 4096 {
                    return Err(PlatformError::TemporarilyUnavailable(
                        "The installed app catalog exceeds its scan limit".into(),
                    ));
                }
                let path = file.path();
                let kind = file.file_type()?;
                if kind.is_dir() {
                    dirs.push((path, depth + 1));
                    continue;
                }
                if path.extension().and_then(|e| e.to_str()) != Some("desktop") {
                    continue;
                }
                let id = path
                    .strip_prefix(root)
                    .map_err(|_| PlatformError::TargetNotFound)?
                    .to_string_lossy()
                    .replace('/', "-");
                if !seen.insert(id.clone()) {
                    continue;
                }
                let Ok(text) = read(&path) else { continue };
                let mut fields = BTreeMap::new();
                let mut entry = false;
                for line in text.lines().map(str::trim) {
                    if line.starts_with('[') {
                        entry = line == "[Desktop Entry]";
                        continue;
                    }
                    if entry
                        && !line.starts_with('#')
                        && let Some((k, v)) = line.split_once('=')
                    {
                        fields.insert(k.trim(), v.trim());
                    }
                }
                if fields.get("Type") != Some(&"Application")
                    || fields.get("Hidden") == Some(&"true")
                {
                    continue;
                }
                let desktops: Vec<_> = environment
                    .get("XDG_CURRENT_DESKTOP")
                    .map(String::as_str)
                    .unwrap_or("")
                    .split(':')
                    .collect();
                if fields
                    .get("OnlyShowIn")
                    .is_some_and(|v| !v.split(';').any(|v| desktops.contains(&v)))
                    || fields.get("NotShowIn").is_some_and(|v| {
                        v.split(';').any(|v| !v.is_empty() && desktops.contains(&v))
                    })
                {
                    continue;
                }
                if fields
                    .get("TryExec")
                    .is_some_and(|v| self::program_path(environment, v).is_none())
                {
                    continue;
                }
                if !fields.contains_key("Exec") && fields.get("DBusActivatable") != Some(&"true") {
                    continue;
                }
                let Some(name) = fields
                    .get("Name")
                    .filter(|s| !s.trim().is_empty() && s.len() <= 1024)
                else {
                    continue;
                };
                entries.push(Desktop {
                    id,
                    path,
                    name: unescape(name),
                    generic_name: fields
                        .get("GenericName")
                        .filter(|s| !s.is_empty() && s.len() <= 1024)
                        .map(|s| unescape(s)),
                    keywords: list(fields.get("Keywords")),
                    categories: list(fields.get("Categories")),
                    mime_types: list(fields.get("MimeType")),
                    program: fields.get("Exec").and_then(|e| program(e)),
                    listed: fields.get("NoDisplay") != Some(&"true"),
                    fingerprint: fingerprint(&text),
                });
            }
        }
    }
    Ok(entries)
}
pub(super) fn discover(
    roots: &[PathBuf],
    environment: &BTreeMap<String, String>,
) -> Result<Vec<Entry>, PlatformError> {
    let mut entries: Vec<Entry> = desktops(roots, environment)?
        .into_iter()
        .filter(|d| d.listed)
        .map(|d| Entry {
            path: d.path,
            name: d.name,
            fingerprint: d.fingerprint,
        })
        .collect();
    entries.sort_by_key(|a| a.name.to_lowercase());
    Ok(entries)
}
/// Whether a desktop entry still reads as it did when it was listed.
pub(crate) fn unchanged(path: &Path, fingerprint: u64) -> bool {
    read(path).is_ok_and(|text| self::fingerprint(&text) == fingerprint)
}
