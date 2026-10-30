// SPDX-License-Identifier: Apache-2.0
//! The `gh` shim and the pre-push check, run as a linked run runs them:
//! through git's config-defined hook and the real executable.
#![cfg(unix)]
use std::path::Path;
use std::process::{Command, Output};

const EXE: &str = env!("CARGO_BIN_EXE_computer-mcp");

/// A run's checkout of `remote`, with the hook the companion sets.
struct Run<'a> {
    remote: &'a Path,
}

impl Run<'_> {
    fn git(&self, dir: &Path, args: &[&str]) -> Output {
        Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "Fixture")
            .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
            .env("GIT_COMMITTER_NAME", "Fixture")
            .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
            // As the companion sets it for a linked run.
            .env("GIT_CONFIG_COUNT", "2")
            .env("GIT_CONFIG_KEY_0", "hook.ophio.event")
            .env("GIT_CONFIG_VALUE_0", "pre-push")
            .env("GIT_CONFIG_KEY_1", "hook.ophio.command")
            .env(
                "GIT_CONFIG_VALUE_1",
                format!("'{EXE}' pre-push 'main' '{}'", self.remote.display()),
            )
            .output()
            .unwrap()
    }

    fn ok(&self, dir: &Path, args: &[&str]) {
        let out = self.git(dir, args);
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn refused(&self, dir: &Path, args: &[&str], why: &str) {
        let out = self.git(dir, args);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "git {args:?} was allowed");
        assert!(stderr.contains(why), "git {args:?}: {stderr}");
    }
}

/// Whether this git runs hooks defined in its config, as Git 2.54 and later do.
fn runs_config_hooks() -> bool {
    let out = Command::new("git").arg("--version").output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    let mut parts = (text.split_whitespace().nth(2).unwrap_or(""))
        .split('.')
        .map(|n| n.parse::<u32>().unwrap_or(0));
    (parts.next().unwrap_or(0), parts.next().unwrap_or(0)) >= (2, 54)
}

#[test]
fn pushes_that_rewrite_delete_or_reach_the_default_branch_are_refused() {
    if !runs_config_hooks() {
        eprintln!("SKIPPED: this git runs no hooks defined in its config; Git 2.54 or later does");
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let remote = root.path().join("remote.git");
    let other = root.path().join("other.git");
    let work = root.path().join("work");
    let run = Run { remote: &remote };
    for bare in [&remote, &other] {
        run.ok(
            root.path(),
            &["init", "-q", "--bare", "-b", "main", bare.to_str().unwrap()],
        );
    }
    run.ok(
        root.path(),
        &["init", "-q", "-b", "main", work.to_str().unwrap()],
    );
    run.ok(
        &work,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
    run.ok(&work, &["commit", "-q", "--allow-empty", "-m", "first"]);
    run.refused(
        &work,
        &["push", "-q", "origin", "main"],
        "Don't push to `main`",
    );

    run.ok(&work, &["switch", "-q", "-c", "ophio/fix"]);
    run.ok(&work, &["commit", "-q", "--allow-empty", "-m", "fix"]);
    run.ok(&work, &["push", "-q", "-u", "origin", "ophio/fix"]);
    run.ok(&work, &["commit", "-q", "--allow-empty", "-m", "more"]);
    run.ok(&work, &["push", "-q"]);

    // Only to the run's repository.
    run.refused(
        &work,
        &["push", "-q", other.to_str().unwrap(), "ophio/fix"],
        "This run pushes to",
    );
    run.ok(
        &work,
        &["remote", "add", "elsewhere", other.to_str().unwrap()],
    );
    run.refused(
        &work,
        &["push", "-q", "elsewhere", "ophio/fix"],
        "This run pushes to",
    );

    // The default branch as it is now, as well as when the run began.
    let bare = ["--git-dir", remote.to_str().unwrap()];
    run.ok(
        root.path(),
        &[&bare[..], &["branch", "production", "ophio/fix"]].concat(),
    );
    run.ok(
        root.path(),
        &[
            &bare[..],
            &["symbolic-ref", "HEAD", "refs/heads/production"],
        ]
        .concat(),
    );
    run.ok(&work, &["commit", "-q", "--allow-empty", "-m", "after"]);
    run.refused(
        &work,
        &["push", "-q", "origin", "HEAD:production"],
        "Don't push to `production`",
    );
    run.refused(
        &work,
        &["push", "-q", "origin", "HEAD:main"],
        "Don't push to `main`",
    );
    run.ok(&work, &["push", "-q", "origin", "HEAD:ophio/other"]);

    run.ok(&work, &["reset", "-q", "--hard", "HEAD~2"]);
    run.ok(&work, &["commit", "-q", "--allow-empty", "-m", "rewritten"]);
    run.refused(&work, &["push", "-q", "--force"], "never force-push");
    run.refused(
        &work,
        &["push", "-q", "origin", "--delete", "ophio/fix"],
        "Don't delete branches",
    );
    run.ok(&work, &["tag", "v1"]);
    run.refused(&work, &["push", "-q", "origin", "v1"], "Don't push tags");
    // --no-verify skips it: the default branch's protection is the net.
    let out = run.git(&work, &["push", "-q", "--force", "--no-verify"]);
    assert!(out.status.success());
}

#[test]
fn gh_writes_are_refused_and_reads_reach_the_real_gh() {
    let out = Command::new(EXE)
        .args(["gh", "pr", "create", "--fill"])
        .env("OPHIO_REAL_GH", "/bin/echo")
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("github_pr"));
    assert!(out.stdout.is_empty());

    let out = Command::new(EXE)
        .args(["gh", "pr", "view", "5"])
        .env("OPHIO_REAL_GH", "/bin/echo")
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout), "pr view 5\n");

    let out = Command::new(EXE)
        .args(["gh", "pr", "view", "5"])
        .env_remove("OPHIO_REAL_GH")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(127));
}
