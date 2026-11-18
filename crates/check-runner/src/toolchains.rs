// SPDX-License-Identifier: Apache-2.0
//! Home-installed toolchains that a check may read without widening task access.
//!
//! System tool directories belong to `sandbox::runtime_paths`, not this list.
//! This only discovers directories: it never runs tools or reads PATH/configuration.

use std::{
    collections::BTreeSet,
    fs, io,
    path::{Component, Path, PathBuf},
};

// Do not add manager configuration roots here. In particular, .cargo can hold
// registry credentials, .deno can hold auth tokens, and .local/bin is arbitrary.
const TOOLCHAINS: &[&str] = &[
    ".rustup",
    ".cargo/bin",
    ".cargo/registry",
    ".cargo/git",
    ".nvm/versions",
    ".fnm/node-versions",
    ".local/share/fnm/node-versions",
    ".volta/tools",
    ".bun/bin",
    ".bun/install/cache",
    ".deno/bin",
    ".pyenv/versions",
    ".local/share/uv/python",
    ".local/share/mise/installs",
];

const PRIVATE_ROOTS: &[&str] = &[
    ".ssh",
    ".config",
    ".npmrc",
    ".netrc",
    ".gitconfig",
    ".aws",
    ".azure",
    ".claude",
    ".codex",
    ".docker",
    ".kube",
    ".gnupg",
    ".cargo/credentials",
    ".cargo/credentials.toml",
    ".deno",
];

/// Return existing, canonical home toolchain directories within the task's reads.
///
/// `home` must already be canonical. `task_read` contains the agent sandbox's
/// canonical directory grants; noncanonical entries are ignored, not resolved
/// into additional grants. A grant inside a toolchain exposes only that subtree.
/// `whole_read` means the agent already has whole-filesystem read access, but
/// does not bypass home containment or the private-configuration exclusions.
///
/// Discovery is conservative: aliases outside the conventional toolchain trees
/// are omitted, even if their targets are otherwise readable. Callers must still
/// enforce these reads with the sandbox; discovery cannot prevent later changes
/// to the filesystem between this call and sandbox creation.
pub fn readable_toolchains(
    home: &Path,
    task_read: &[PathBuf],
    whole_read: bool,
) -> io::Result<Vec<PathBuf>> {
    let canonical_home = fs::canonicalize(home)?;
    if !home.is_absolute() || !same_canonical_spelling(home, &canonical_home) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "The toolchain home directory must be canonical",
        ));
    }
    if !home.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "The toolchain home must be a directory",
        ));
    }

    // Keep one spelling internally, including Windows' extended prefix. The
    // caller may use its equivalent ordinary drive/UNC prefix, but not aliases.
    let supplied_home = home;
    let home = canonical_home.as_path();
    let roots: Vec<_> = TOOLCHAINS.iter().map(|path| home.join(path)).collect();
    let mut private: Vec<_> = PRIVATE_ROOTS.iter().map(|path| home.join(path)).collect();
    for relative in PRIVATE_ROOTS {
        if let Some(path) = canonical_existing(&home.join(relative))? {
            private.push(path);
        }
    }
    // Deno's bin is deliberately allowed, but its enclosing auth/config tree
    // is not. Handle this exception below rather than exposing all of .deno.
    let deno = home.join(".deno");
    let deno_bin = home.join(".deno/bin");
    let safe = |path: &Path| {
        path != home
            && path.starts_with(home)
            && roots.iter().any(|root| path.starts_with(root))
            && !has_private_component(path.strip_prefix(home).unwrap_or(path))
            && !private.iter().any(|root| {
                if root == &deno && path.starts_with(&deno_bin) {
                    false
                } else {
                    path.starts_with(root) || root.starts_with(path)
                }
            })
    };

    let mut grants = Vec::new();
    if !whole_read {
        for grant in task_read {
            if !grant.is_absolute()
                || grant
                    .components()
                    .any(|part| matches!(part, Component::CurDir | Component::ParentDir))
            {
                continue;
            }
            if let Some(canonical) = canonical_existing(grant)?
                && same_canonical_spelling(grant, &canonical)
                && canonical.is_dir()
            {
                grants.push(canonical);
            }
        }
    }

    let mut readable = BTreeSet::new();
    for root in roots.iter() {
        let Some(candidate) = canonical_existing(root)? else {
            continue;
        };
        if !candidate.is_dir() || !safe(&candidate) {
            continue;
        }
        if whole_read {
            readable.insert(candidate);
            continue;
        }
        for grant in &grants {
            if candidate.starts_with(grant) {
                readable.insert(candidate.clone());
            } else if grant.starts_with(&candidate) && safe(grant) {
                readable.insert(grant.clone());
            }
        }
    }

    // Mount each subtree only once, including when several managers alias it.
    let mut minimal: Vec<PathBuf> = Vec::new();
    for path in readable {
        if !minimal.iter().any(|parent| path.starts_with(parent)) {
            minimal.push(path);
        }
    }
    // Return the caller's home prefix so grants remain comparable with the
    // application's ordinary Windows paths, without weakening validation.
    Ok(minimal
        .into_iter()
        .map(|path| supplied_home.join(path.strip_prefix(home).expect("validated toolchain home")))
        .collect())
}

fn same_canonical_spelling(path: &Path, canonical: &Path) -> bool {
    #[cfg(windows)]
    {
        use std::path::Prefix;
        let mut path = path.components();
        let mut canonical = canonical.components();
        let prefix = match (path.next(), canonical.next()) {
            (Some(Component::Prefix(a)), Some(Component::Prefix(b))) => {
                a.kind() == b.kind()
                    || matches!((a.kind(), b.kind()),
                        (Prefix::Disk(a), Prefix::VerbatimDisk(b)) if a == b)
                    || matches!((a.kind(), b.kind()),
                        (Prefix::UNC(a, b), Prefix::VerbatimUNC(c, d)) if a == c && b == d)
            }
            _ => false,
        };
        prefix && path.eq(canonical)
    }
    #[cfg(not(windows))]
    {
        path == canonical
    }
}

fn canonical_existing(path: &Path) -> io::Result<Option<PathBuf>> {
    match fs::canonicalize(path) {
        Ok(path) => Ok(Some(path)),
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
            ) =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

fn has_private_component(path: &Path) -> bool {
    path.components().any(|component| {
        let Component::Normal(name) = component else {
            return true;
        };
        matches!(
            name.to_str(),
            Some(
                ".ssh"
                    | ".config"
                    | ".npmrc"
                    | ".netrc"
                    | ".gitconfig"
                    | ".aws"
                    | ".azure"
                    | ".claude"
                    | ".codex"
                    | ".docker"
                    | ".kube"
                    | ".gnupg"
                    | "credentials"
                    | "credentials.toml"
            )
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn home() -> TempDir {
        tempfile::tempdir().expect("create test home")
    }

    fn directory(home: &Path, relative: &str) -> PathBuf {
        let path = home.join(relative);
        fs::create_dir_all(&path).expect("create toolchain directory");
        fs::canonicalize(path).expect("canonicalize toolchain directory")
    }

    fn canonical_home(home: &TempDir) -> PathBuf {
        fs::canonicalize(home.path()).expect("canonicalize test home")
    }

    #[cfg(windows)]
    #[test]
    fn plain_windows_home_and_grants_match_extended_canonical_paths() {
        let temp = home();
        let canonical = canonical_home(&temp);
        let cargo = directory(&canonical, ".cargo/bin");
        let node = directory(&canonical, ".nvm/versions/node/v22/bin");
        let plain =
            |path: &Path| PathBuf::from(path.to_str().unwrap().strip_prefix(r"\\?\").unwrap());
        let home = plain(&canonical);
        let mut expected = vec![plain(&cargo), plain(&node)];
        expected.sort();
        assert_eq!(
            readable_toolchains(&home, &[plain(&cargo), plain(&node)], false).unwrap(),
            expected
        );
        assert_eq!(
            readable_toolchains(&home, &[], true).unwrap(),
            vec![plain(&cargo), home.join(".nvm/versions")]
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_parent_components_do_not_create_canonical_grants() {
        let temp = home();
        let home = canonical_home(&temp);
        let cargo = directory(&home, ".cargo/bin");
        // Joining onto an extended Windows path normalizes `..` immediately.
        // Append raw text so the validator actually receives a parent component.
        let with_parent = |path: &Path, suffix: &str| {
            let mut text = path.as_os_str().to_owned();
            text.push(suffix);
            let path = PathBuf::from(text);
            assert!(path.components().any(|part| part == Component::ParentDir));
            path
        };
        assert!(readable_toolchains(&with_parent(&home, r"\.cargo\.."), &[], true).is_err());
        assert!(
            readable_toolchains(&home, &[with_parent(&cargo, r"\..\bin")], false)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn no_task_grants_means_no_toolchains() {
        let temp = home();
        let home = canonical_home(&temp);
        directory(&home, ".rustup");
        directory(&home, ".cargo/bin");
        assert!(readable_toolchains(&home, &[], false).unwrap().is_empty());
    }

    #[test]
    fn grants_intersect_instead_of_widening_toolchain_reads() {
        let temp = home();
        let home = canonical_home(&temp);
        let cargo = directory(&home, ".cargo/bin");
        directory(&home, ".cargo/registry");
        let node = directory(&home, ".nvm/versions/node/v22");
        directory(&home, ".nvm/versions/node/v20");
        directory(&home, ".rustup");
        let unrelated = directory(&home, "project");
        let mut expected = vec![cargo.clone(), node.clone()];
        expected.sort();
        assert_eq!(
            readable_toolchains(&home, &[cargo, node, unrelated], false).unwrap(),
            expected
        );
    }

    #[test]
    fn cargo_credentials_never_make_the_cargo_root_readable() {
        let temp = home();
        let home = canonical_home(&temp);
        let mut expected = vec![
            directory(&home, ".cargo/bin"),
            directory(&home, ".cargo/registry"),
            directory(&home, ".cargo/git"),
        ];
        fs::write(home.join(".cargo/credentials.toml"), "secret").unwrap();
        fs::write(home.join(".cargo/config.toml"), "private config").unwrap();
        directory(&home, ".cargo/other");
        expected.sort();
        assert_eq!(readable_toolchains(&home, &[], true).unwrap(), expected);
        assert_eq!(
            readable_toolchains(&home, &[home.join(".cargo")], false).unwrap(),
            expected
        );
    }

    #[test]
    fn missing_toolchains_and_file_candidates_are_ignored() {
        let temp = home();
        let home = canonical_home(&temp);
        fs::write(home.join(".rustup"), "not a directory").unwrap();
        assert!(readable_toolchains(&home, &[], true).unwrap().is_empty());
    }

    #[test]
    fn whole_read_discovers_only_conventional_directories() {
        let temp = home();
        let home = canonical_home(&temp);
        let mut expected: Vec<_> = TOOLCHAINS
            .iter()
            .map(|relative| directory(&home, relative))
            .collect();
        directory(&home, ".local/bin");
        directory(&home, ".deno/auth");
        directory(&home, ".config");
        directory(&home, ".ssh");
        directory(&home, "arbitrary/bin");
        expected.sort();
        assert_eq!(readable_toolchains(&home, &[], true).unwrap(), expected);
    }

    #[test]
    fn returned_roots_are_canonical_and_duplicates_are_removed() {
        let temp = home();
        let home = canonical_home(&temp);
        let root = directory(&home, ".rustup");
        let child = directory(&home, ".rustup/toolchains/stable");
        assert_eq!(
            readable_toolchains(&home, &[child, root.clone(), root.clone()], false).unwrap(),
            vec![root]
        );
    }

    #[cfg(unix)]
    #[test]
    fn toolchain_links_cannot_escape_home_or_alias_private_roots() {
        use std::os::unix::fs::symlink;
        let temp = home();
        let home = canonical_home(&temp);
        let outside = tempfile::tempdir().unwrap();
        symlink(outside.path(), home.join(".rustup")).unwrap();
        directory(&home, ".cargo");
        let config = directory(&home, ".config/tools");
        symlink(&config, home.join(".cargo/bin")).unwrap();
        let ssh = directory(&home, ".ssh");
        symlink(&ssh, home.join(".cargo/registry")).unwrap();
        assert!(readable_toolchains(&home, &[], true).unwrap().is_empty());
        assert!(
            readable_toolchains(&home, std::slice::from_ref(&home), false)
                .unwrap()
                .is_empty()
        );
    }

    #[cfg(unix)]
    #[test]
    fn aliases_to_whole_cargo_and_arbitrary_home_directories_are_denied() {
        use std::os::unix::fs::symlink;
        let temp = home();
        let home = canonical_home(&temp);
        let cargo = directory(&home, ".cargo");
        fs::write(cargo.join("credentials.toml"), "secret").unwrap();
        symlink(cargo, home.join(".rustup")).unwrap();
        directory(&home, ".nvm");
        let arbitrary = directory(&home, "private");
        symlink(arbitrary, home.join(".nvm/versions")).unwrap();
        assert!(readable_toolchains(&home, &[], true).unwrap().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn home_and_grant_aliases_do_not_create_access() {
        use std::os::unix::fs::symlink;
        let temp = home();
        let home = canonical_home(&temp);
        let actual_home = directory(&home, "actual");
        let toolchain = directory(&actual_home, ".rustup");
        let alias = home.join("alias");
        symlink(&actual_home, &alias).unwrap();
        assert_eq!(
            readable_toolchains(&alias, &[], true).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        let grant_alias = home.join("grant");
        symlink(&toolchain, &grant_alias).unwrap();
        assert!(
            readable_toolchains(&actual_home, &[grant_alias], false)
                .unwrap()
                .is_empty()
        );
    }

    #[cfg(unix)]
    #[test]
    fn aliases_to_other_toolchains_are_canonicalized_and_deduplicated() {
        use std::os::unix::fs::symlink;
        let temp = home();
        let home = canonical_home(&temp);
        let cargo_bin = directory(&home, ".cargo/bin");
        symlink(&cargo_bin, home.join(".rustup")).unwrap();
        assert_eq!(
            readable_toolchains(&home, &[], true).unwrap(),
            vec![cargo_bin]
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_private_root_alias_into_a_toolchain_is_still_private() {
        use std::os::unix::fs::symlink;
        let temp = home();
        let home = canonical_home(&temp);
        let private = directory(&home, ".rustup/private");
        symlink(private, home.join(".config")).unwrap();
        assert!(readable_toolchains(&home, &[], true).unwrap().is_empty());
    }
}
