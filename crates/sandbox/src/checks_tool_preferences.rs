// SPDX-License-Identifier: Apache-2.0
//! Host-only configuration hints. These never authorize a read or a mount.
use std::path::{Path, PathBuf};

#[derive(Debug, Default)]
pub(super) struct Preferences {
    pub project: Vec<Preference>,
    pub defaults: Vec<Preference>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Preference {
    pub manager: Option<Manager>,
    pub tool: String,
    pub version: String,
    pub target: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Manager {
    Nvm,
    Fnm,
    Pyenv,
    Mise,
    Volta,
}

const MAX_INPUT: u64 = 64 * 1024;
const MAX_HINTS: usize = 128;

/// Read only fixed configuration locations. Neither contents nor configuration
/// paths should be forwarded to the check. The caller must resolve each hint
/// against its existing canonical grants before using an executable directory.
pub(super) fn read(project: &Path, home: &Path) -> Preferences {
    let mut result = Preferences::default();
    for (name, tool, multiple) in [
        (".nvmrc", "node", false),
        (".node-version", "node", false),
        (".python-version", "python", true),
    ] {
        if let Some(text) = read_text(&project.join(name)) {
            append_versions(&mut result.project, None, tool, &text, multiple);
        }
    }
    if let Some(text) = read_text(&project.join(".tool-versions")) {
        let mut hints = Vec::new();
        let valid = text.lines().all(|line| {
            let mut words = uncomment(line).split_whitespace();
            let Some(tool) = words.next() else {
                return true;
            };
            let Some(tool) = tool_name(tool) else {
                return false;
            };
            let versions: Vec<_> = words.collect();
            if versions.is_empty() || !versions.iter().all(|v| valid_version(v)) {
                return false;
            }
            for version in versions {
                push_hint(&mut hints, None, tool, version);
            }
            true
        });
        if valid {
            result.project.extend(hints);
        }
    }
    if let Some(text) = read_text(&project.join("package.json"))
        && let Ok(json) = serde_json::from_str::<serde_json::Value>(&text)
        && let Some(volta) = json.get("volta")
    {
        append_volta(&mut result.project, volta, false);
    }

    if let Some(version) = nvm_default(home) {
        push_hint(&mut result.defaults, Some(Manager::Nvm), "node", &version);
    }
    if let Some(text) = read_text(&home.join(".pyenv/version")) {
        append_versions(
            &mut result.defaults,
            Some(Manager::Pyenv),
            "python",
            &text,
            true,
        );
    }
    if let Some(text) = read_text(&home.join(".volta/tools/user/platform.json"))
        && let Ok(json) = serde_json::from_str::<serde_json::Value>(&text)
    {
        append_volta(&mut result.defaults, &json, true);
    }
    for relative in [".fnm/aliases/default", ".local/share/fnm/aliases/default"] {
        append_fnm_default(&mut result.defaults, &home.join(relative));
    }
    if let Some(text) = read_text(&home.join(".config/mise/config.toml"))
        && let Some(hints) = mise_tools(&text)
    {
        result.defaults.extend(hints);
    }
    result
}

fn read_text(path: &Path) -> Option<String> {
    use std::io::Read;
    // Check before opening as well as afterwards, so ordinary sockets/devices
    // are not opened at all. Unix flags close the symlink/FIFO replacement race.
    let metadata = std::fs::symlink_metadata(path).ok()?;
    if !metadata.is_file() || metadata.len() > MAX_INPUT {
        return None;
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    let file = options.open(path).ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() || metadata.len() > MAX_INPUT {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(MAX_INPUT + 1).read_to_end(&mut bytes).ok()?;
    if bytes.len() as u64 > MAX_INPUT {
        return None;
    }
    String::from_utf8(bytes).ok()
}

fn uncomment(line: &str) -> &str {
    line.split_once('#')
        .map_or(line, |(before, _)| before)
        .trim()
}

fn valid_version(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value != "."
        && value != ".."
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._+-*^~<>=|".contains(&c))
}

fn tool_name(value: &str) -> Option<&str> {
    match value {
        "nodejs" => Some("node"),
        "python3" => Some("python"),
        _ if !value.is_empty()
            && value.len() <= 64
            && value.starts_with(|c: char| c.is_ascii_alphanumeric())
            && value
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"_-.".contains(&c)) =>
        {
            Some(value)
        }
        _ => None,
    }
}

fn push_hint(hints: &mut Vec<Preference>, manager: Option<Manager>, tool: &str, version: &str) {
    if hints.len() < MAX_HINTS && valid_version(version) {
        hints.push(Preference {
            manager,
            tool: tool.into(),
            version: version.into(),
            target: None,
        });
    }
}

fn append_versions(
    hints: &mut Vec<Preference>,
    manager: Option<Manager>,
    tool: &str,
    text: &str,
    multiple: bool,
) {
    let versions: Vec<_> = text
        .lines()
        .flat_map(|line| uncomment(line).split_whitespace())
        .collect();
    if versions.is_empty()
        || (!multiple && versions.len() != 1)
        || !versions.iter().all(|version| valid_version(version))
    {
        return;
    }
    for version in versions {
        push_hint(hints, manager, tool, version);
    }
}

fn append_volta(hints: &mut Vec<Preference>, json: &serde_json::Value, global: bool) {
    // Never honor package.json's `volta.extends`: it names an arbitrary file.
    for tool in ["node", "npm", "yarn", "pnpm"] {
        let Some(value) = json.get(tool) else {
            continue;
        };
        let version = value.as_str().or_else(|| {
            global
                .then(|| value.get("runtime").or_else(|| value.get("version")))
                .flatten()
                .and_then(serde_json::Value::as_str)
        });
        if let Some(version) = version {
            push_hint(hints, Some(Manager::Volta), tool, version);
        }
    }
}

fn nvm_default(home: &Path) -> Option<String> {
    let aliases = home.join(".nvm/alias");
    let mut name = "default".to_owned();
    let mut visited = Vec::new();
    for _ in 0..8 {
        if visited.contains(&name) {
            return None;
        }
        visited.push(name.clone());
        if let Some((directory, _)) = name.split_once('/') {
            // Alias names are at most two components. Do not intentionally
            // follow an lts/ directory link to another configuration location.
            let metadata = std::fs::symlink_metadata(aliases.join(directory)).ok()?;
            if !metadata.is_dir() {
                return None;
            }
        }
        let path = aliases.join(&name);
        let Some(text) = read_text(&path) else {
            return (name != "default" && valid_version(&name)).then_some(name);
        };
        let parts: Vec<_> = text
            .lines()
            .map(uncomment)
            .filter(|v| !v.is_empty())
            .collect();
        if parts.len() != 1 {
            return None;
        }
        let value = parts[0];
        // nvm alias names can be lts/hydrogen or lts/*, but cannot be paths
        // outside the fixed alias directory. No directory enumeration is used.
        let components: Vec<_> = value.split('/').collect();
        if components.len() > 2
            || !components.iter().all(|part| {
                !part.is_empty()
                    && *part != "."
                    && *part != ".."
                    && part
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || b"._-*".contains(&c))
            })
        {
            return None;
        }
        if value.starts_with(|c: char| c.is_ascii_digit())
            || value
                .strip_prefix('v')
                .is_some_and(|v| v.starts_with(|c: char| c.is_ascii_digit()))
        {
            return valid_version(value).then(|| value.to_owned());
        }
        name = value.to_owned();
    }
    None
}

fn append_fnm_default(hints: &mut Vec<Preference>, path: &Path) {
    if let Ok(metadata) = std::fs::symlink_metadata(path)
        && metadata.file_type().is_symlink()
    {
        // fnm stores its chosen version as a link. Read only the link text,
        // never its target. Core selection must check the target against grants.
        let Ok(target) = std::fs::read_link(path) else {
            return;
        };
        if target.as_os_str().len() > MAX_INPUT as usize {
            return;
        }
        let target = if target.is_absolute() {
            target
        } else {
            path.parent().unwrap_or(Path::new("")).join(target)
        };
        let mut version_path = target.as_path();
        for suffix in ["bin", "installation"] {
            if version_path.file_name().is_some_and(|name| name == suffix) {
                let Some(parent) = version_path.parent() else {
                    return;
                };
                version_path = parent;
            }
        }
        let Some(version) = version_path.file_name().and_then(|v| v.to_str()) else {
            return;
        };
        if valid_version(version) && hints.len() < MAX_HINTS {
            hints.push(Preference {
                manager: Some(Manager::Fnm),
                tool: "node".into(),
                version: version.into(),
                target: Some(target),
            });
        }
    } else if let Some(text) = read_text(path) {
        append_versions(hints, Some(Manager::Fnm), "node", &text, false);
    }
}

// A deliberately small, single-line TOML subset: [tools] strings, inline
// { version = "..." }, and [tools.NAME] version entries. Unsupported syntax
// discards mise hints rather than misreading multiline strings or templates.
fn mise_tools(text: &str) -> Option<Vec<Preference>> {
    let mut hints = Vec::new();
    let mut section = String::new();
    let mut sections = std::collections::BTreeSet::new();
    let mut selected = std::collections::BTreeSet::new();
    for raw in text.lines() {
        let line = toml_line(raw)?.trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') {
            if !line.ends_with(']') || line.starts_with("[[") {
                return None;
            }
            section = line[1..line.len() - 1].trim().to_owned();
            if section.is_empty() || !sections.insert(section.clone()) {
                return None;
            }
            continue;
        }
        let (key, value) = line.split_once('=')?;
        let key = toml_key(key.trim())?;
        let value = value.trim();
        let selection = if section == "tools" {
            let version = if value.starts_with('{') && value.ends_with('}') {
                let (key, value) = value[1..value.len() - 1].trim().split_once('=')?;
                if toml_key(key.trim())? != "version" {
                    return None;
                }
                toml_string(value.trim())?
            } else {
                toml_string(value)?
            };
            Some((key, version))
        } else if let Some(tool) = section.strip_prefix("tools.") {
            if key != "version" {
                return None;
            }
            Some((toml_key(tool.trim())?, toml_string(value)?))
        } else {
            // Accept ordinary scalar settings, but do not try to interpret
            // complex unrelated TOML with this limited parser.
            if toml_string(value).is_none()
                && !matches!(value, "true" | "false")
                && value.parse::<i64>().is_err()
            {
                return None;
            }
            None
        };
        if let Some((tool, version)) = selection {
            let tool = tool_name(&tool)?;
            if !valid_version(&version) || !selected.insert(tool.to_owned()) {
                return None;
            }
            push_hint(&mut hints, Some(Manager::Mise), tool, &version);
        }
    }
    Some(hints)
}

fn toml_key(text: &str) -> Option<String> {
    if text.starts_with(['\'', '"']) {
        toml_string(text)
    } else {
        tool_name(text).map(str::to_owned)
    }
}

fn toml_string(text: &str) -> Option<String> {
    if text.starts_with('"') {
        serde_json::from_str::<String>(text).ok()
    } else if text.starts_with('\'') && text.ends_with('\'') && text.len() >= 2 {
        let inner = &text[1..text.len() - 1];
        (!inner.contains('\'')).then(|| inner.to_owned())
    } else {
        None
    }
}

// Strip comments only outside strings, and reject constructs that could carry
// parser state into another line. This stops fake [tools] text in multiline data.
fn toml_line(line: &str) -> Option<&str> {
    let bytes = line.as_bytes();
    let mut quote = None;
    let mut escaped = false;
    let mut brackets = Vec::new();
    for (i, &byte) in bytes.iter().enumerate() {
        if let Some(delimiter) = quote {
            if escaped {
                escaped = false;
            } else if byte == b'\\' && delimiter == b'"' {
                escaped = true;
            } else if byte == delimiter {
                quote = None;
            }
            continue;
        }
        match byte {
            b'#' => return brackets.is_empty().then_some(&line[..i]),
            b'\'' | b'"' => {
                if bytes
                    .get(i..i + 3)
                    .is_some_and(|s| s.iter().all(|&c| c == byte))
                {
                    return None;
                }
                quote = Some(byte);
            }
            b'[' => brackets.push(b']'),
            b'{' => brackets.push(b'}'),
            b']' | b'}' if brackets.pop() != Some(byte) => return None,
            _ => {}
        }
    }
    (quote.is_none() && brackets.is_empty()).then_some(line)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(root: &Path, name: &str, contents: &str) {
        let path = root.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    fn hint(manager: Option<Manager>, tool: &str, version: &str) -> Preference {
        Preference {
            manager,
            tool: tool.into(),
            version: version.into(),
            target: None,
        }
    }

    #[test]
    fn project_hints_preserve_precedence_and_do_not_bind_generic_pins_to_a_manager() {
        let project = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        write(project.path(), ".nvmrc", "v18.20.4\n");
        write(project.path(), ".node-version", "20.18.0\n");
        write(project.path(), ".python-version", "3.12.8\n");
        write(
            project.path(),
            ".tool-versions",
            "# comment\nnodejs 22.1.0\npython 3.13.1 3.11.9 # fallback\n",
        );
        write(
            project.path(),
            "package.json",
            r#"{"volta":{"node":"24.0.0","npm":"11.0.0"}}"#,
        );
        assert_eq!(
            read(project.path(), home.path()).project,
            vec![
                hint(None, "node", "v18.20.4"),
                hint(None, "node", "20.18.0"),
                hint(None, "python", "3.12.8"),
                hint(None, "node", "22.1.0"),
                hint(None, "python", "3.13.1"),
                hint(None, "python", "3.11.9"),
                hint(Some(Manager::Volta), "node", "24.0.0"),
                hint(Some(Manager::Volta), "npm", "11.0.0"),
            ]
        );
    }

    #[test]
    fn nvm_default_is_read_even_when_newer_versions_exist() {
        let project = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        write(home.path(), ".nvm/alias/default", "lts/hydrogen\n");
        write(home.path(), ".nvm/alias/lts/hydrogen", "v18.20.4\n");
        fs::create_dir_all(home.path().join(".nvm/versions/node/v24.0.0/bin")).unwrap();
        assert_eq!(
            read(project.path(), home.path()).defaults,
            vec![hint(Some(Manager::Nvm), "node", "v18.20.4")]
        );
    }

    #[test]
    fn global_pyenv_mise_volta_and_text_fnm_forms_are_read() {
        let project = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        write(home.path(), ".pyenv/version", "3.12.8\n3.11.9\n");
        write(home.path(), ".fnm/aliases/default", "v20.18.0\n");
        write(home.path(), ".local/share/fnm/aliases/default", "v22.1.0\n");
        write(
            home.path(),
            ".volta/tools/user/platform.json",
            r#"{"node":{"runtime":"18.20.4"},"npm":{"version":"10.9.0"},"yarn":null}"#,
        );
        write(
            home.path(),
            ".config/mise/config.toml",
            "[tools]\nnode = \"22.14.0\" # chosen\npython = { version = '3.13.2' }\n",
        );
        let defaults = read(project.path(), home.path()).defaults;
        for expected in [
            hint(Some(Manager::Pyenv), "python", "3.12.8"),
            hint(Some(Manager::Pyenv), "python", "3.11.9"),
            hint(Some(Manager::Fnm), "node", "v20.18.0"),
            hint(Some(Manager::Fnm), "node", "v22.1.0"),
            hint(Some(Manager::Volta), "node", "18.20.4"),
            hint(Some(Manager::Volta), "npm", "10.9.0"),
            hint(Some(Manager::Mise), "node", "22.14.0"),
            hint(Some(Manager::Mise), "python", "3.13.2"),
        ] {
            assert!(defaults.contains(&expected), "missing {expected:?}");
        }
        assert!(!defaults.iter().any(|p| p.tool == "uv"));
    }

    #[test]
    fn volta_accepts_plain_versions_and_version_objects() {
        let project = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        write(
            home.path(),
            ".volta/tools/user/platform.json",
            r#"{"node":"20.1.0","yarn":{"version":"4.0.0"},"pnpm":{"version":"9.0.0"}}"#,
        );
        assert_eq!(
            read(project.path(), home.path()).defaults,
            vec![
                hint(Some(Manager::Volta), "node", "20.1.0"),
                hint(Some(Manager::Volta), "yarn", "4.0.0"),
                hint(Some(Manager::Volta), "pnpm", "9.0.0"),
            ]
        );
    }

    #[test]
    fn aliases_are_bounded_and_cannot_traverse_to_arbitrary_host_files() {
        let project = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        write(home.path(), ".nvm/alias/default", "loop-a");
        write(home.path(), ".nvm/alias/loop-a", "loop-b");
        write(home.path(), ".nvm/alias/loop-b", "loop-a");
        assert!(read(project.path(), home.path()).defaults.is_empty());
        write(home.path(), ".nvm/alias/default", "../../private");
        write(home.path(), "private", "v99.0.0");
        assert!(read(project.path(), home.path()).defaults.is_empty());
    }

    #[test]
    fn malformed_oversized_and_non_utf8_inputs_fall_through() {
        let project = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        write(project.path(), ".nvmrc", &"1".repeat(65_537));
        write(project.path(), ".node-version", "$(touch nope)");
        write(project.path(), ".python-version", "3.12\n../../secret");
        write(project.path(), "package.json", "{broken");
        fs::write(project.path().join(".tool-versions"), [0xff]).unwrap();
        write(home.path(), ".volta/tools/user/platform.json", "{}");
        write(home.path(), ".pyenv/version", "/outside/python");
        let preferences = read(project.path(), home.path());
        assert!(preferences.project.is_empty());
        assert!(preferences.defaults.is_empty());
    }

    #[test]
    fn experimental_versions_are_valid_when_explicitly_selected() {
        let project = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        write(project.path(), ".python-version", "3.14.0a1\n3.13.1t\n");
        assert_eq!(
            read(project.path(), home.path()).project,
            vec![
                hint(None, "python", "3.14.0a1"),
                hint(None, "python", "3.13.1t"),
            ]
        );
    }

    #[test]
    fn unsupported_or_malformed_mise_toml_is_not_guessed() {
        let project = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        for contents in [
            "[tools]\nnode = [\"22\", \"20\"]\n",
            "[tools]\nnode = { version = \"22\", path = \"/private\" }\n",
            "[tools]\nnode = \"22\"\nnode = \"20\"\n",
            "[tools]\nnode = \"22\"\npython = \"unterminated\n",
            "[env]\nFAKE = '''\n[tools]\nnode = \"99\"\n'''\n",
            "[tools]\nnode = \"{{exec(command='node')}}\"\n",
        ] {
            write(home.path(), ".config/mise/config.toml", contents);
            assert!(
                read(project.path(), home.path()).defaults.is_empty(),
                "{contents}"
            );
        }
    }

    #[test]
    fn mise_quoted_keys_literal_strings_and_tool_version_tables_work() {
        let project = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        write(
            home.path(),
            ".config/mise/config.toml",
            "[tools]\n'nodejs' = '20.18.0'\n[tools.python]\nversion = \"3.12.8\"\n",
        );
        assert_eq!(
            read(project.path(), home.path()).defaults,
            vec![
                hint(Some(Manager::Mise), "node", "20.18.0"),
                hint(Some(Manager::Mise), "python", "3.12.8"),
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn nvm_alias_resolution_does_not_follow_an_arbitrary_alias_directory_link() {
        use std::os::unix::fs::symlink;
        let project = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        write(home.path(), ".nvm/alias/default", "lts/hydrogen");
        write(outside.path(), "hydrogen", "v99.0.0");
        symlink(outside.path(), home.path().join(".nvm/alias/lts")).unwrap();
        assert!(read(project.path(), home.path()).defaults.is_empty());
    }

    #[test]
    fn volta_extends_never_loads_an_arbitrary_project_configuration() {
        let project = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        write(
            home.path(),
            "private.json",
            r#"{"volta":{"node":"99.0.0"}}"#,
        );
        let contents = serde_json::json!({"volta": {"extends": home.path().join("private.json")}});
        write(project.path(), "package.json", &contents.to_string());
        assert!(read(project.path(), home.path()).project.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn fnm_symlink_defaults_are_hints_not_followed_reads() {
        use std::os::unix::fs::symlink;
        let project = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let aliases = home.path().join(".fnm/aliases");
        fs::create_dir_all(&aliases).unwrap();
        let target = PathBuf::from("../node-versions/v18.20.4");
        symlink(&target, aliases.join("default")).unwrap();
        let preferences = read(project.path(), home.path());
        assert_eq!(
            preferences.defaults,
            vec![Preference {
                manager: Some(Manager::Fnm),
                tool: "node".into(),
                version: "v18.20.4".into(),
                target: Some(aliases.join(target)),
            }]
        );
    }

    #[cfg(unix)]
    #[test]
    fn no_project_pin_symlink_is_followed_even_inside_the_project() {
        use std::os::unix::fs::symlink;
        let project = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        write(home.path(), "secret", "v99.0.0");
        write(project.path(), "inside", "v22.0.0");
        symlink(home.path().join("secret"), project.path().join(".nvmrc")).unwrap();
        symlink("inside", project.path().join(".node-version")).unwrap();
        symlink(
            home.path().join("secret"),
            project.path().join(".python-version"),
        )
        .unwrap();
        symlink(
            home.path().join("secret"),
            project.path().join(".tool-versions"),
        )
        .unwrap();
        symlink(
            home.path().join("secret"),
            project.path().join("package.json"),
        )
        .unwrap();
        assert!(read(project.path(), home.path()).project.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn special_files_are_never_opened_as_blocking_input() {
        use std::os::unix::net::UnixListener;
        let project = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let _socket = UnixListener::bind(project.path().join(".nvmrc")).unwrap();
        fs::create_dir(project.path().join(".node-version")).unwrap();
        let pipe = project.path().join(".python-version");
        assert!(
            std::process::Command::new("mkfifo")
                .arg(&pipe)
                .status()
                .unwrap()
                .success()
        );
        assert!(read(project.path(), home.path()).project.is_empty());
    }
}
