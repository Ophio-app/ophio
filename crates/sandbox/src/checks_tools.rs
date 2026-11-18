// SPDX-License-Identifier: Apache-2.0
use super::preferences::{self, Manager, Preference};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

pub(super) fn toolchain_search_paths(
    read: &[PathBuf],
    original: Option<&std::ffi::OsStr>,
    project: &Path,
    home: &Path,
    system: &[PathBuf],
) -> Vec<PathBuf> {
    // Canonical grants may have lost uv's conventional spelling through a
    // root link. Exclude that known install root too, without granting it.
    let uv_root = home.join(".local/share/uv/python").canonicalize().ok();
    let is_uv = |path: &Path| {
        uv_managed(path) || uv_root.as_ref().is_some_and(|root| path.starts_with(root))
    };
    let original: Vec<_> = original
        .into_iter()
        .flat_map(std::env::split_paths)
        .filter(|path| path.is_absolute() && !is_uv(path))
        .filter_map(|path| {
            read.iter()
                .filter(|grant| path.starts_with(grant))
                .find_map(|grant| granted_directory(&path, grant))
        })
        .filter(|path| !is_uv(path))
        .collect();
    let mut candidates = Vec::new();
    for grant in read.iter().filter(|grant| grant.is_absolute()) {
        discover_manager_bins(grant, &mut candidates);
    }
    candidates.retain(|candidate| !is_uv(&candidate.bin));
    // Configuration is read by Ophio, not mounted in the sandbox. Its contents
    // are only selectors over the already-granted candidates above.
    let hints = preferences::read(project, home);
    let mut defaults: Vec<_> = hints.defaults.iter().collect();
    defaults.sort_by_key(|hint| manager_path_rank(hint.manager, &candidates, &original));
    let mut selected = BTreeMap::new();
    let mut chosen = Vec::new();
    let mut search = Vec::new();
    for hint in hints.project.iter().chain(defaults) {
        if selected.contains_key(&hint.tool) {
            continue;
        }
        if let Some(candidate) = select(hint, &candidates, &original, read) {
            selected.insert(hint.tool.clone(), candidate);
            chosen.push(candidate);
        }
    }
    // A separate npm/yarn/pnpm pin overrides the copy bundled with Node.
    chosen.sort_by_key(|candidate| !matches!(candidate.tool.as_str(), "npm" | "yarn" | "pnpm"));
    for candidate in chosen {
        push_unique(&mut search, candidate.bin.clone());
    }
    // Keep explicit, non-version-manager tools such as cargo. A service's
    // inherited version-manager PATH is not an interactive-shell selection.
    for path in original {
        if !managed_path(&path) {
            push_unique(&mut search, path);
        }
    }
    for grant in read.iter().filter(|grant| grant.is_absolute()) {
        if managed_path(grant) || is_uv(grant) {
            continue;
        }
        let bin = if grant.file_name().is_some_and(|name| name == "bin") {
            grant.clone()
        } else {
            grant.join("bin")
        };
        if let Some(bin) = granted_directory(&bin, grant)
            && !managed_path(&bin)
            && !is_uv(&bin)
        {
            push_unique(&mut search, bin);
        }
    }
    for path in system {
        push_unique(&mut search, path.clone());
    }
    // No pin/default: leave the system interpreter in charge. Only one stable
    // installed version per tool is available as a last-resort fallback.
    let mut fallbacks: BTreeMap<&str, &Candidate> = BTreeMap::new();
    for candidate in &candidates {
        if selected.contains_key(&candidate.tool) || stable_version(&candidate.version).is_none() {
            continue;
        }
        let previous = fallbacks.get(candidate.tool.as_str()).copied();
        if previous.is_none_or(|old| compare(candidate, old, &[]).is_gt()) {
            fallbacks.insert(&candidate.tool, candidate);
        }
    }
    for candidate in fallbacks.values() {
        push_unique(&mut search, candidate.bin.clone());
    }
    search
}

#[derive(Debug, PartialEq, Eq)]
struct Candidate {
    manager: Manager,
    tool: String,
    version: String,
    version_dir: PathBuf,
    bin: PathBuf,
}

fn select<'a>(
    hint: &Preference,
    candidates: &'a [Candidate],
    original: &[PathBuf],
    read: &[PathBuf],
) -> Option<&'a Candidate> {
    // fnm's default may be a symlink. It must identify this actual installed
    // version/bin, never an ancestor that could accidentally select any version.
    let target = match &hint.target {
        Some(path) => Some(absolute_hint(path)?),
        None => None,
    };
    // Never follow an arbitrary host target merely because a default names it.
    // A narrow bin grant can still match its known version parent by spelling,
    // without reading that parent or any sibling installation.
    let resolved = target.as_ref().and_then(|target| {
        read.iter()
            .filter(|grant| target.starts_with(grant))
            .find_map(|grant| granted_directory(target, grant))
    });
    candidates
        .iter()
        .filter(|candidate| {
            candidate.tool == hint.tool
                && hint
                    .manager
                    .is_none_or(|manager| candidate.manager == manager)
                && target.as_ref().map_or_else(
                    || version_matches(&hint.version, &candidate.version),
                    |target| {
                        target_identifies(target, candidate)
                            || resolved
                                .as_ref()
                                .is_some_and(|path| target_identifies(path, candidate))
                    },
                )
        })
        .max_by(|a, b| compare(a, b, original))
}

fn absolute_hint(path: &Path) -> Option<PathBuf> {
    use std::path::Component;
    if !path.is_absolute() {
        return None;
    }
    let mut normalized = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    return None;
                }
            }
            _ => normalized.push(part.as_os_str()),
        }
    }
    Some(normalized)
}

fn target_identifies(target: &Path, candidate: &Candidate) -> bool {
    target == candidate.bin
        || target == candidate.version_dir
        || (candidate.manager == Manager::Fnm
            && target == candidate.version_dir.join("installation"))
}

fn version_matches(hint: &str, installed: &str) -> bool {
    let hint = hint.strip_prefix('v').unwrap_or(hint);
    let installed = installed.strip_prefix('v').unwrap_or(installed);
    if hint == installed {
        return true;
    }
    let Some(installed) = stable_version(installed) else {
        return false;
    };
    if matches!(hint, "node" | "stable" | "latest") {
        return true;
    }
    let prefix = hint
        .strip_suffix(".x")
        .or_else(|| hint.strip_suffix(".*"))
        .unwrap_or(hint);
    stable_version(prefix).is_some_and(|version| installed.starts_with(&version))
}

fn compare(a: &Candidate, b: &Candidate, original: &[PathBuf]) -> std::cmp::Ordering {
    stable_version(&a.version)
        .cmp(&stable_version(&b.version))
        .then_with(|| {
            manager_path_rank(Some(b.manager), std::slice::from_ref(b), original).cmp(
                &manager_path_rank(Some(a.manager), std::slice::from_ref(a), original),
            )
        })
        .then_with(|| manager_rank(b.manager).cmp(&manager_rank(a.manager)))
        .then_with(|| b.bin.cmp(&a.bin))
}

fn manager_path_rank(
    manager: Option<Manager>,
    candidates: &[Candidate],
    original: &[PathBuf],
) -> usize {
    original
        .iter()
        .position(|path| {
            candidates
                .iter()
                .any(|candidate| Some(candidate.manager) == manager && candidate.bin == *path)
        })
        .unwrap_or(usize::MAX)
}

fn manager_rank(manager: Manager) -> u8 {
    match manager {
        Manager::Nvm => 0,
        Manager::Fnm => 1,
        Manager::Pyenv => 2,
        Manager::Mise => 3,
        Manager::Volta => 4,
    }
}

fn stable_version(name: &str) -> Option<Vec<u64>> {
    // Entire version, not just its numeric prefix: reject alpha/beta/rc and
    // free-threaded builds instead of mistaking them for stable interpreters.
    let name = name.strip_prefix('v').unwrap_or(name);
    if name.is_empty() || !name.bytes().all(|c| c.is_ascii_digit() || c == b'.') {
        return None;
    }
    name.split('.')
        .map(str::parse)
        .collect::<Result<Vec<u64>, _>>()
        .ok()
}

fn push_unique(search: &mut Vec<PathBuf>, path: PathBuf) {
    if !search.contains(&path) {
        search.push(path);
    }
}

fn granted_directory(path: &Path, grant: &Path) -> Option<PathBuf> {
    let path = path.canonicalize().ok()?;
    (path.starts_with(grant) && path.is_dir()).then_some(path)
}

fn uv_managed(path: &Path) -> bool {
    path.ancestors()
        .any(|path| path.ends_with(".local/share/uv/python"))
}

fn managed_path(path: &Path) -> bool {
    MANAGER_LAYOUTS
        .iter()
        .any(|layout| path.ancestors().any(|path| path.ends_with(layout.root)))
}

struct ManagerLayout {
    root: &'static str,
    parts: &'static [&'static str],
    manager: Manager,
    tool: Option<&'static str>,
    tool_index: usize,
    version_index: usize,
}

// Fixed shapes only. Never recurse into arbitrary packages or manager settings.
// uv's privately managed Python installations are deliberately absent.
const MANAGER_LAYOUTS: &[ManagerLayout] = &[
    ManagerLayout {
        root: ".nvm/versions",
        parts: &["node", "*", "bin"],
        manager: Manager::Nvm,
        tool: Some("node"),
        tool_index: 0,
        version_index: 1,
    },
    ManagerLayout {
        root: ".nvm/versions",
        parts: &["io.js", "*", "bin"],
        manager: Manager::Nvm,
        tool: Some("node"),
        tool_index: 0,
        version_index: 1,
    },
    ManagerLayout {
        root: ".fnm/node-versions",
        parts: &["*", "installation", "bin"],
        manager: Manager::Fnm,
        tool: Some("node"),
        tool_index: 0,
        version_index: 0,
    },
    ManagerLayout {
        root: ".local/share/fnm/node-versions",
        parts: &["*", "installation", "bin"],
        manager: Manager::Fnm,
        tool: Some("node"),
        tool_index: 0,
        version_index: 0,
    },
    ManagerLayout {
        root: ".pyenv/versions",
        parts: &["*", "bin"],
        manager: Manager::Pyenv,
        tool: Some("python"),
        tool_index: 0,
        version_index: 0,
    },
    ManagerLayout {
        root: ".local/share/mise/installs",
        parts: &["*", "*", "bin"],
        manager: Manager::Mise,
        tool: None,
        tool_index: 0,
        version_index: 1,
    },
    ManagerLayout {
        root: ".volta/tools",
        parts: &["image", "*", "*", "bin"],
        manager: Manager::Volta,
        tool: None,
        tool_index: 1,
        version_index: 2,
    },
];

fn matches_layout(parts: &[&std::ffi::OsStr], layout: &[&str]) -> bool {
    parts.len() <= layout.len()
        && parts
            .iter()
            .zip(layout)
            .all(|(part, expected)| *expected == "*" || *part == std::ffi::OsStr::new(expected))
}

fn discover_manager_bins(grant: &Path, candidates: &mut Vec<Candidate>) {
    if uv_managed(grant) {
        return;
    }
    for layout in MANAGER_LAYOUTS {
        let Some(root) = grant.ancestors().find(|path| path.ends_with(layout.root)) else {
            continue;
        };
        let relative = grant.strip_prefix(root).expect("ancestor");
        let parts: Vec<_> = relative.iter().collect();
        if matches_layout(&parts, layout.parts) {
            // Start at this grant, not its manager root: a narrow version/bin
            // grant must never enumerate any sibling installation.
            expand_layout(
                grant,
                &layout.parts[parts.len()..],
                grant,
                root,
                layout,
                candidates,
            );
        }
    }
}

fn expand_layout(
    path: &Path,
    remaining: &[&str],
    grant: &Path,
    root: &Path,
    layout: &ManagerLayout,
    candidates: &mut Vec<Candidate>,
) {
    let Some(directory) = granted_directory(path, grant) else {
        return;
    };
    let Some((next, rest)) = remaining.split_first() else {
        let Ok(relative) = directory.strip_prefix(root) else {
            return;
        };
        let parts: Vec<_> = relative.iter().collect();
        if parts.len() != layout.parts.len() || !matches_layout(&parts, layout.parts) {
            return;
        }
        let Some(version) = parts[layout.version_index].to_str() else {
            return;
        };
        let Some(tool) = layout.tool.or_else(|| parts[layout.tool_index].to_str()) else {
            return;
        };
        let tool = match tool {
            "nodejs" => "node",
            "python3" => "python",
            _ => tool,
        };
        let candidate = Candidate {
            manager: layout.manager,
            tool: tool.into(),
            version: version.into(),
            version_dir: root.join(parts[..=layout.version_index].iter().collect::<PathBuf>()),
            bin: directory,
        };
        if !candidates.contains(&candidate) {
            candidates.push(candidate);
        }
        return;
    };
    if *next != "*" {
        expand_layout(&directory.join(next), rest, grant, root, layout, candidates);
        return;
    }
    let Ok(entries) = std::fs::read_dir(&directory) else {
        return;
    };
    let mut names: Vec<_> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.file_name())
        .collect();
    names.sort();
    for name in names {
        expand_layout(&directory.join(name), rest, grant, root, layout, candidates);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn toolchain_search_paths(
        read: &[PathBuf],
        original: Option<&std::ffi::OsStr>,
    ) -> Vec<PathBuf> {
        let empty = tempfile::tempdir().unwrap();
        super::toolchain_search_paths(read, original, empty.path(), empty.path(), &[])
    }

    #[test]
    fn original_search_paths_include_only_existing_granted_toolchains() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().canonicalize().unwrap();
        let nvm = home.join(".nvm/versions");
        let node = nvm.join("node/v22/bin");
        let uv = home.join(".local/share/uv/python");
        let python = uv.join("cpython-3.13/bin");
        let outside = home.join("private/bin");
        for path in [&node, &python, &outside] {
            std::fs::create_dir_all(path).unwrap();
        }
        let missing = nvm.join("node/missing/bin");
        let file = nvm.join("not-a-directory");
        std::fs::write(&file, "not a directory").unwrap();
        let original = std::env::join_paths([
            &outside,
            &node,
            &missing,
            &python,
            &file,
            &node,
            Path::new("relative/bin"),
        ])
        .unwrap();
        assert_eq!(
            toolchain_search_paths(&[nvm, uv], Some(&original)),
            vec![node]
        );
    }

    #[test]
    fn direct_bin_grants_work_without_an_original_search_path() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().canonicalize().unwrap();
        let cargo = home.join(".cargo/bin");
        let version = home.join("node/v22");
        std::fs::create_dir_all(&cargo).unwrap();
        std::fs::create_dir_all(version.join("bin")).unwrap();
        let search = toolchain_search_paths(&[cargo.clone(), version.clone()], None);
        assert!(search.contains(&cargo));
        assert!(search.contains(&version.join("bin")));
    }

    fn directory(home: &Path, relative: &str) -> PathBuf {
        let path = home.join(relative);
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn manager_versions_are_discovered_without_parent_path() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().canonicalize().unwrap();
        for (root, relative) in [
            (".nvm/versions", "node/v22.14.0/bin"),
            (".fnm/node-versions", "v22.14.0/installation/bin"),
            (
                ".local/share/fnm/node-versions",
                "v22.14.0/installation/bin",
            ),
            (".pyenv/versions", "3.13.2/bin"),
            (".local/share/mise/installs", "node/22.14.0/bin"),
            (".volta/tools", "image/node/22.14.0/bin"),
        ] {
            let root = home.join(root);
            let bin = directory(&root, relative);
            assert_eq!(toolchain_search_paths(&[root], None), vec![bin]);
        }
    }

    #[test]
    fn manager_fallback_is_newest_stable_even_if_the_service_inherits_an_old_version() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().canonicalize().unwrap();
        let root = home.join(".nvm/versions");
        let old = directory(&root, "node/v9.11.2/bin");
        let recent = directory(&root, "node/v22.3.0/bin");
        let newest = directory(&root, "node/v22.13.0/bin");
        assert_eq!(
            toolchain_search_paths(std::slice::from_ref(&root), None),
            vec![newest.clone()]
        );
        let original = std::env::join_paths([&old, &recent]).unwrap();
        assert_eq!(
            toolchain_search_paths(&[root], Some(&original)),
            vec![newest]
        );
    }

    #[cfg(unix)]
    #[test]
    fn manager_aliases_and_version_links_cannot_escape_their_grants() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().canonicalize().unwrap();
        for (root, versions) in [
            (".nvm/versions", "node"),
            (".fnm/node-versions", ""),
            (".pyenv/versions", ""),
            (".local/share/mise/installs", "node"),
            (".volta/tools", "image/node"),
        ] {
            let root = home.join(root);
            let versions = root.join(versions);
            let tail = if root.ends_with("node-versions") {
                "installation/bin"
            } else {
                "bin"
            };
            directory(&versions.join("v9.0.0"), tail);
            let newest = directory(&versions.join("v22.0.0"), tail);
            symlink(versions.join("v9.0.0"), versions.join("current")).unwrap();
            symlink(versions.join("v9.0.0"), versions.join("default")).unwrap();
            let outside = directory(&home, "private/v99.0.0");
            directory(&outside, tail);
            symlink(&outside, versions.join("v99.0.0")).unwrap();
            assert_eq!(toolchain_search_paths(&[root], None), vec![newest]);
        }
    }

    #[cfg(unix)]
    #[test]
    fn aliases_do_not_override_real_pins_or_defaults() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().canonicalize().unwrap();
        let root = home.join(".nvm/versions");
        let versions = root.join("node");
        for (alias, version) in [
            ("current", "v20.0.0"),
            ("default", "v18.0.0"),
            ("latest", "v22.0.0"),
            ("stable", "v24.0.0"),
        ] {
            directory(&versions.join(version), "bin");
            symlink(versions.join(version), versions.join(alias)).unwrap();
        }
        let newest = directory(&versions, "v26.0.0/bin");
        assert_eq!(toolchain_search_paths(&[root], None), vec![newest]);
    }

    #[test]
    fn narrowed_manager_grants_do_not_discover_sibling_versions() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().canonicalize().unwrap();
        let root = home.join(".nvm/versions/node");
        let allowed = directory(&root, "v20/bin");
        directory(&root, "v22/bin");
        assert_eq!(
            toolchain_search_paths(&[root.join("v20")], None),
            vec![allowed]
        );
    }

    #[cfg(unix)]
    #[test]
    fn fallback_bin_links_cannot_escape_grants() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().canonicalize().unwrap();
        let grant = directory(&home, "version");
        let outside = directory(&home, "private/bin");
        std::os::unix::fs::symlink(outside, grant.join("bin")).unwrap();
        assert!(toolchain_search_paths(&[grant], None).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn original_search_path_links_cannot_escape_grants() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().canonicalize().unwrap();
        let grant = home.join("versions");
        let outside = home.join("private/bin");
        std::fs::create_dir_all(&grant).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let link = grant.join("linked/bin");
        std::fs::create_dir_all(link.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        let original = std::env::join_paths([&link]).unwrap();
        assert!(toolchain_search_paths(&[grant], Some(&original)).is_empty());
    }

    fn contextual_search(home: &Path, project: &Path, read: &[PathBuf]) -> Vec<PathBuf> {
        super::toolchain_search_paths(read, None, project, home, &[PathBuf::from("/system/bin")])
    }

    #[test]
    fn uv_managed_python_never_enters_the_search_path() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().canonicalize().unwrap();
        let project = directory(&home, "project");
        let uv = home.join(".local/share/uv/python");
        let bin = directory(&uv, "cpython-3.13.2-linux-x86_64-gnu/bin");
        let original = std::env::join_paths([bin]).unwrap();
        assert_eq!(
            super::toolchain_search_paths(
                &[uv],
                Some(&original),
                &project,
                &home,
                &[PathBuf::from("/system/bin")]
            ),
            vec![PathBuf::from("/system/bin")]
        );
    }

    #[test]
    fn nvm_global_default_beats_a_newer_install_and_system_tools() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().canonicalize().unwrap();
        let project = directory(&home, "project");
        let nvm = home.join(".nvm/versions");
        let selected = directory(&nvm, "node/v18.20.5/bin");
        directory(&nvm, "node/v22.14.0/bin");
        directory(&home, ".nvm/alias");
        std::fs::write(home.join(".nvm/alias/default"), "18\n").unwrap();
        assert_eq!(
            contextual_search(&home, &project, &[nvm]),
            vec![selected, "/system/bin".into()]
        );
    }

    #[test]
    fn project_pin_beats_the_manager_default_and_system_tools() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().canonicalize().unwrap();
        let project = directory(&home, "project");
        let nvm = home.join(".nvm/versions");
        directory(&nvm, "node/v18.20.5/bin");
        let selected = directory(&nvm, "node/v20.18.3/bin");
        directory(&nvm, "node/v22.14.0/bin");
        directory(&home, ".nvm/alias");
        std::fs::write(home.join(".nvm/alias/default"), "18\n").unwrap();
        std::fs::write(project.join(".nvmrc"), "20\n").unwrap();
        assert_eq!(
            contextual_search(&home, &project, &[nvm]),
            vec![selected, "/system/bin".into()]
        );
    }

    #[test]
    fn pyenv_is_selected_without_putting_uv_on_the_path() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().canonicalize().unwrap();
        let project = directory(&home, "project");
        let pyenv = home.join(".pyenv/versions");
        let selected = directory(&pyenv, "3.12.9/bin");
        let uv = home.join(".local/share/uv/python");
        directory(&uv, "cpython-3.14.0-linux-x86_64-gnu/bin");
        std::fs::write(project.join(".python-version"), "3.12.9\n").unwrap();
        assert_eq!(
            contextual_search(&home, &project, &[uv, pyenv]),
            vec![selected, "/system/bin".into()]
        );
    }

    #[test]
    fn an_unselected_alpha_or_free_threaded_build_never_beats_stable_python() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().canonicalize().unwrap();
        let project = directory(&home, "project");
        let pyenv = home.join(".pyenv/versions");
        directory(&pyenv, "3.14.0a1/bin");
        directory(&pyenv, "3.15.0+freethreaded/bin");
        directory(&pyenv, "3.13.4t/bin");
        let stable = directory(&pyenv, "3.13.4/bin");
        directory(&pyenv, "3.12.9/bin");
        assert_eq!(
            contextual_search(&home, &project, &[pyenv]),
            vec!["/system/bin".into(), stable]
        );
    }

    #[test]
    fn an_unresolved_project_pin_falls_through_to_the_global_default() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().canonicalize().unwrap();
        let project = directory(&home, "project");
        let nvm = home.join(".nvm/versions");
        let selected = directory(&nvm, "node/v18.20.5/bin");
        directory(&home, ".nvm/alias");
        std::fs::write(home.join(".nvm/alias/default"), "18\n").unwrap();
        directory(&nvm, "node/v22.14.0/bin");
        std::fs::write(project.join(".nvmrc"), "../../private\n").unwrap();
        assert_eq!(
            contextual_search(&home, &project, &[nvm]),
            vec![selected, "/system/bin".into()]
        );
    }

    #[test]
    fn an_unresolved_default_uses_system_then_only_the_newest_stable_version() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().canonicalize().unwrap();
        let project = directory(&home, "project");
        let nvm = home.join(".nvm/versions");
        directory(&nvm, "node/v18.20.5/bin");
        let stable = directory(&nvm, "node/v22.14.0/bin");
        directory(&nvm, "node/v24.0.0-rc1/bin");
        directory(&home, ".nvm/alias");
        std::fs::write(home.join(".nvm/alias/default"), "99\n").unwrap();
        assert_eq!(
            contextual_search(&home, &project, &[nvm]),
            vec!["/system/bin".into(), stable]
        );
    }

    #[test]
    fn each_managers_global_default_selects_only_its_granted_installed_version() {
        for (root, old, new, config, contents) in [
            (
                ".pyenv/versions",
                "3.12.9/bin",
                "3.13.2/bin",
                ".pyenv/version",
                "3.12.9\n",
            ),
            (
                ".fnm/node-versions",
                "v18.20.5/installation/bin",
                "v22.14.0/installation/bin",
                ".fnm/aliases/default",
                "v18.20.5\n",
            ),
            (
                ".local/share/mise/installs",
                "python/3.12.9/bin",
                "python/3.13.2/bin",
                ".config/mise/config.toml",
                "[tools]\npython = '3.12.9'\n",
            ),
            (
                ".volta/tools",
                "image/node/18.20.5/bin",
                "image/node/22.14.0/bin",
                ".volta/tools/user/platform.json",
                r#"{"node":{"runtime":"18.20.5"}}"#,
            ),
        ] {
            let temp = tempfile::tempdir().unwrap();
            let home = temp.path().canonicalize().unwrap();
            let project = directory(&home, "project");
            let grant = home.join(root);
            let selected = directory(&grant, old);
            directory(&grant, new);
            let config = home.join(config);
            std::fs::create_dir_all(config.parent().unwrap()).unwrap();
            std::fs::write(config, contents).unwrap();
            assert_eq!(
                contextual_search(&home, &project, &[grant]),
                vec![selected, "/system/bin".into()],
                "{root}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn fnm_default_link_must_identify_an_actual_granted_version() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().canonicalize().unwrap();
        let project = directory(&home, "project");
        let grant = home.join(".fnm/node-versions");
        let selected = directory(&grant, "v18.20.5/installation/bin");
        let newer = directory(&grant, "v22.14.0/installation/bin");
        directory(&home, ".fnm/aliases");
        let alias = home.join(".fnm/aliases/default");
        std::os::unix::fs::symlink("../node-versions/v18.20.5", &alias).unwrap();
        assert_eq!(
            contextual_search(&home, &project, std::slice::from_ref(&grant)),
            vec![selected, "/system/bin".into()]
        );
        std::fs::remove_file(&alias).unwrap();
        std::os::unix::fs::symlink("../node-versions", &alias).unwrap();
        assert_eq!(
            contextual_search(&home, &project, &[grant]),
            vec!["/system/bin".into(), newer]
        );
    }

    #[test]
    fn a_pin_cannot_select_an_ungranted_sibling_installation() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().canonicalize().unwrap();
        let project = directory(&home, "project");
        let root = home.join(".pyenv/versions");
        let allowed = directory(&root, "3.12.9/bin");
        directory(&root, "3.13.2/bin");
        std::fs::write(project.join(".python-version"), "3.13.2\n").unwrap();
        assert_eq!(
            contextual_search(&home, &project, std::slice::from_ref(&allowed)),
            vec!["/system/bin".into(), allowed]
        );
    }

    #[test]
    fn only_an_explicit_experimental_pin_can_select_an_experimental_build() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().canonicalize().unwrap();
        let project = directory(&home, "project");
        let root = home.join(".pyenv/versions");
        let alpha = directory(&root, "3.14.0a1/bin");
        let stable = directory(&root, "3.13.2/bin");
        std::fs::write(project.join(".python-version"), "3.14\n").unwrap();
        assert_eq!(
            contextual_search(&home, &project, std::slice::from_ref(&root)),
            vec!["/system/bin".into(), stable]
        );
        std::fs::write(project.join(".python-version"), "3.14.0a1\n").unwrap();
        assert_eq!(
            contextual_search(&home, &project, &[root]),
            vec![alpha, "/system/bin".into()]
        );
    }

    #[cfg(unix)]
    #[test]
    fn inherited_links_cannot_borrow_a_different_grant_to_gain_priority() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().canonicalize().unwrap();
        let first = directory(&home, "first");
        let first_bin = directory(&first, "bin");
        let second = directory(&home, "second");
        let second_bin = directory(&second, "bin");
        let link = first.join("linked-bin");
        std::os::unix::fs::symlink(&second_bin, &link).unwrap();
        let original = std::env::join_paths([link]).unwrap();
        assert_eq!(
            toolchain_search_paths(&[first, second], Some(&original)),
            vec![first_bin, second_bin]
        );
    }

    #[test]
    fn an_explicit_volta_npm_pin_precedes_nodes_bundled_npm() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().canonicalize().unwrap();
        let project = directory(&home, "project");
        let grant = home.join(".volta/tools");
        let node = directory(&grant, "image/node/22.14.0/bin");
        let npm = directory(&grant, "image/npm/10.9.2/bin");
        std::fs::write(
            project.join("package.json"),
            r#"{"volta":{"node":"22.14.0","npm":"10.9.2"}}"#,
        )
        .unwrap();
        assert_eq!(
            contextual_search(&home, &project, &[grant]),
            vec![npm, node, "/system/bin".into()]
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_fnm_hint_outside_grants_is_not_followed_even_if_it_links_back_inside() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().canonicalize().unwrap();
        let project = directory(&home, "project");
        let grant = home.join(".fnm/node-versions");
        directory(&grant, "v18.20.5/installation/bin");
        let newer = directory(&grant, "v22.14.0/installation/bin");
        let outside = directory(&home, "private").join("version-alias");
        std::os::unix::fs::symlink(grant.join("v18.20.5"), &outside).unwrap();
        directory(&home, ".fnm/aliases");
        std::os::unix::fs::symlink(outside, home.join(".fnm/aliases/default")).unwrap();
        assert_eq!(
            contextual_search(&home, &project, &[grant]),
            vec!["/system/bin".into(), newer]
        );
    }

    #[cfg(unix)]
    #[test]
    fn uv_is_excluded_even_when_its_installation_root_is_a_link() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().canonicalize().unwrap();
        let project = directory(&home, "project");
        let storage = directory(&home, "managed-python");
        let bin = directory(&storage, "cpython-3.13.2-linux-x86_64-gnu/bin");
        directory(&home, ".local/share/uv");
        std::os::unix::fs::symlink(&storage, home.join(".local/share/uv/python")).unwrap();
        let original = std::env::join_paths([bin.clone()]).unwrap();
        for grant in [storage, bin] {
            assert_eq!(
                super::toolchain_search_paths(
                    &[grant],
                    Some(&original),
                    &project,
                    &home,
                    &["/system/bin".into()]
                ),
                vec![PathBuf::from("/system/bin")]
            );
        }
    }
}
