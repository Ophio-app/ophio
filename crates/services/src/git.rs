// SPDX-License-Identifier: Apache-2.0
//! Reading a project's repository without running what its settings name.

use std::{path::Path, process::Stdio};

/// Tracked files that differ from `HEAD`, staged or not. `git status` reads
/// a file whose saved times changed to compare its content, and a repository's
/// own settings can name a filter program to run on that content; these
/// commands compare the index's saved times instead. A file touched but not
/// changed counts until the next `git status` in the project refreshes them.
pub(crate) const CHANGED: [&str; 4] = ["diff-index", "--name-only", "--ignore-submodules", "HEAD"];
/// Files that are neither tracked nor ignored.
pub(crate) const UNTRACKED: [&str; 3] = ["ls-files", "--others", "--exclude-standard"];

/// `git` in `path`, for reading. A repository's own settings can name a
/// filesystem monitor to run or caches to write; these options turn both off,
/// and Ophio's environment cannot point it at another repository. Objects
/// are read as stored, never as a replacement the repository names, and
/// each path is read as Ophio spells it, whatever pathspec settings the
/// environment has.
pub(crate) fn read_only(path: impl AsRef<Path>) -> tokio::process::Command {
    let mut command = tokio::process::Command::new("git");
    command
        .args([
            "--no-optional-locks",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.untrackedCache=false",
        ])
        .current_dir(path)
        .kill_on_drop(true)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW: Øphio may run without a console.
    for key in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_LITERAL_PATHSPECS",
        "GIT_GLOB_PATHSPECS",
        "GIT_NOGLOB_PATHSPECS",
        "GIT_ICASE_PATHSPECS",
    ] {
        command.env_remove(key);
    }
    command.env("GIT_NO_REPLACE_OBJECTS", "1");
    command
}

/// Trimmed output of a read-only `git` command in `path`, or None when it
/// fails, takes over two seconds or prints more than 1 MiB.
pub(crate) async fn output(path: &Path, args: &[&str]) -> Option<String> {
    let mut command = read_only(path);
    command.args(args);
    let output = tokio::time::timeout(std::time::Duration::from_secs(2), command.output())
        .await
        .ok()?
        .ok()?;
    if !output.status.success() || output.stdout.len() > 1024 * 1024 {
        return None;
    }
    String::from_utf8(output.stdout)
        .ok()
        .map(|s| s.trim().to_owned())
}

/// Branches offered for a person to choose from, most recently committed first.
pub(crate) const BRANCH_LIMIT: usize = 200;

/// The local branches of the repository at `path`, read now. None when the
/// folder is not a repository Git can read in time.
pub(crate) async fn branches(path: &Path) -> Option<protocol::model::BranchListing> {
    const HEADS: &str = "refs/heads/";
    let count = format!("--count={}", BRANCH_LIMIT + 1);
    let listed = output(
        path,
        &[
            "for-each-ref",
            "--sort=-committerdate",
            &count,
            "--format=%(refname)",
            HEADS,
        ],
    )
    .await?;
    let mut branches: Vec<String> = listed
        .lines()
        .filter_map(|line| line.strip_prefix(HEADS))
        .map(str::to_owned)
        .collect();
    let truncated = branches.len() > BRANCH_LIMIT;
    branches.truncate(BRANCH_LIMIT);
    let current = output(path, &["symbolic-ref", "--quiet", "HEAD"])
        .await
        .and_then(|head| head.strip_prefix(HEADS).map(str::to_owned));
    // A branch with no commits yet, or one past the limit, is still the one
    // checked out.
    if let Some(current) = &current
        && !branches.contains(current)
    {
        branches.insert(0, current.clone());
    }
    let dirty_files = output(path, &CHANGED)
        .await
        .map_or(0, |changed| changed.lines().count() as u32);
    Some(protocol::model::BranchListing {
        current,
        branches,
        dirty_files,
        truncated,
    })
}

/// Whether Git would accept `name` as a branch (`git check-ref-format
/// --branch`), so a request cannot pass an option or a revision expression.
pub(crate) fn branch_name_ok(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && name != "@"
        && name != "HEAD"
        && !name.starts_with('-')
        && !name.ends_with('.')
        && !name.contains("..")
        && !name.contains("@{")
        && !name
            .chars()
            .any(|c| c.is_control() || " ~^:?*[\\".contains(c))
        && name
            .split('/')
            .all(|part| !part.is_empty() && !part.starts_with('.') && !part.ends_with(".lock"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_ignore_pathspec_settings_and_replaced_objects() {
        let command = read_only(".");
        let envs: Vec<_> = command.as_std().get_envs().collect();
        for key in [
            "GIT_LITERAL_PATHSPECS",
            "GIT_GLOB_PATHSPECS",
            "GIT_NOGLOB_PATHSPECS",
            "GIT_ICASE_PATHSPECS",
        ] {
            assert!(envs.contains(&(key.as_ref(), None)), "{key}");
        }
        let replace = std::ffi::OsStr::new("1");
        assert!(envs.contains(&("GIT_NO_REPLACE_OBJECTS".as_ref(), Some(replace))));
    }

    #[test]
    fn branch_names_follow_gits_rules() {
        for ok in [
            "main",
            "feature/search",
            "fix-1.2",
            "Ünïcode",
            "a@b",
            "x/y/z",
        ] {
            assert!(branch_name_ok(ok), "{ok}");
        }
        for bad in [
            "",
            "@",
            "HEAD",
            "-rf",
            "--force",
            "a..b",
            "a@{1}",
            "end.",
            "end/",
            "/start",
            "a//b",
            ".hidden",
            "a/.b",
            "x.lock",
            "a/b.lock/c",
            "sp ace",
            "tab\t",
            "a~1",
            "a^",
            "a:b",
            "a?",
            "a*",
            "a[",
            "a\\b",
            "nul\0",
        ] {
            assert!(!branch_name_ok(bad), "{bad:?}");
        }
        assert!(!branch_name_ok(&"b".repeat(256)));
    }

    #[tokio::test]
    async fn branches_list_recent_first_with_the_current_one() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(branches(&dir.path().join("missing")).await, None);
        let git = |args: &[&str], date: &str| {
            let output = std::process::Command::new("git")
                .args([
                    "-c",
                    "user.name=Test",
                    "-c",
                    "user.email=test@example.invalid",
                ])
                .args([
                    "-c",
                    "commit.gpgsign=false",
                    "-c",
                    "core.hooksPath=/dev/null",
                ])
                .args(args)
                .current_dir(dir.path())
                .env("GIT_COMMITTER_DATE", date)
                .env_remove("GIT_DIR")
                .env_remove("GIT_WORK_TREE")
                .env_remove("GIT_INDEX_FILE")
                .output()
                .unwrap();
            assert!(output.status.success(), "{args:?}");
        };
        let old = "2001-01-01T00:00:00Z";
        let new = "2002-01-01T00:00:00Z";
        git(&["init", "--quiet", "--initial-branch=main"], old);
        // A new repository has its branch checked out before any commit.
        let unborn = branches(dir.path()).await.unwrap();
        assert_eq!(unborn.current.as_deref(), Some("main"));
        assert_eq!(unborn.branches, ["main"]);
        std::fs::write(dir.path().join("file"), "a").unwrap();
        git(&["add", "file"], old);
        git(&["commit", "--quiet", "-m", "one"], old);
        git(&["branch", "aa-older"], old);
        git(&["checkout", "--quiet", "-b", "zz/newer"], old);
        git(&["commit", "--quiet", "--allow-empty", "-m", "two"], new);
        git(&["checkout", "--quiet", "main"], old);
        std::fs::write(dir.path().join("file"), "b").unwrap();
        let listing = branches(dir.path()).await.unwrap();
        assert_eq!(listing.current.as_deref(), Some("main"));
        assert_eq!(listing.branches, ["zz/newer", "aa-older", "main"]);
        assert_eq!(listing.dirty_files, 1);
        assert!(!listing.truncated);
        git(&["checkout", "--quiet", "--detach"], old);
        assert_eq!(branches(dir.path()).await.unwrap().current, None);
    }
}
