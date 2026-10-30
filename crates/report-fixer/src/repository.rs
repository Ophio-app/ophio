// SPDX-License-Identifier: Apache-2.0
//! Git object transfer never imports an agent's config, hooks, refs or object store.
use crate::{Error, Result};
use std::{
    collections::BTreeMap,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

pub fn oid(s: &str) -> Result<()> {
    if !matches!(s.len(), 40 | 64) || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(Error::Operation("invalid commit"));
    }
    Ok(())
}
pub fn command(repo: &Path) -> Command {
    let mut c = Command::new("git");
    c.env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_AUTHOR_NAME", "Report fixes")
        .env("GIT_AUTHOR_EMAIL", "reports@example.invalid")
        .env("GIT_COMMITTER_NAME", "Report fixes")
        .env("GIT_COMMITTER_EMAIL", "reports@example.invalid")
        .arg("-C")
        .arg(repo)
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "protocol.file.allow=never",
            "-c",
            "protocol.ext.allow=never",
        ]);
    c
}
pub fn git(repo: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let out = command(repo).args(args).output()?;
    if !out.status.success() || out.stdout.len() > 32 * 1024 * 1024 {
        return Err(Error::Operation("git object operation"));
    }
    Ok(out.stdout)
}
pub fn text(repo: &Path, args: &[&str]) -> Result<String> {
    String::from_utf8(git(repo, args)?)
        .map(|s| s.trim().into())
        .map_err(|_| Error::Operation("git response"))
}
pub fn safe_relative(path: &str) -> bool {
    !path.is_empty()
        && !Path::new(path).is_absolute()
        && !path
            .split('/')
            .any(|p| p.is_empty() || p == "." || p == ".." || p.eq_ignore_ascii_case(".git"))
        && !path.chars().any(char::is_control)
}
#[derive(Debug)]
struct Blob {
    mode: String,
    bytes: Vec<u8>,
}
fn tree(repo: &Path, commit: &str) -> Result<BTreeMap<String, Blob>> {
    oid(commit)?;
    let mut map = BTreeMap::new();
    let mut total = 0;
    for row in git(repo, &["ls-tree", "-rz", commit])?
        .split(|b| *b == 0)
        .filter(|b| !b.is_empty())
    {
        let row = std::str::from_utf8(row).map_err(|_| Error::Operation("tree path"))?;
        let (metadata, path) = row.split_once('\t').ok_or(Error::Operation("tree entry"))?;
        let fields: Vec<_> = metadata.split_whitespace().collect();
        if fields.len() != 3
            || fields[1] != "blob"
            || !matches!(fields[0], "100644" | "100755")
            || !safe_relative(path)
        {
            return Err(Error::Operation("unsupported base tree"));
        }
        let bytes = git(repo, &["cat-file", "blob", fields[2]])?;
        total += bytes.len();
        if total > 512 * 1024 * 1024 || bytes.len() > 32 * 1024 * 1024 || map.len() > 100_000 {
            return Err(Error::Operation("tree size"));
        }
        map.insert(
            path.into(),
            Blob {
                mode: fields[0].into(),
                bytes,
            },
        );
    }
    Ok(map)
}
pub fn snapshot(repo: &Path, commit: &str, dest: &Path) -> Result<()> {
    std::fs::create_dir_all(dest)?;
    for (path, blob) in tree(repo, commit)? {
        let p = dest.join(path);
        std::fs::create_dir_all(p.parent().ok_or(Error::Operation("snapshot parent"))?)?;
        std::fs::write(&p, blob.bytes)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                p,
                std::fs::Permissions::from_mode(if blob.mode == "100755" { 0o755 } else { 0o644 }),
            )?;
        }
    }
    Ok(())
}
fn files(
    root: &Path,
    dir: &Path,
    map: &mut BTreeMap<String, Blob>,
    total: &mut usize,
) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let name = path
            .strip_prefix(root)
            .map_err(|_| Error::Operation("workspace path"))?
            .to_str()
            .ok_or(Error::Operation("workspace encoding"))?
            .replace('\\', "/");
        *total = total.saturating_add(16 * 1024);
        if *total > 512 * 1024 * 1024 || name.len() > 1024 || name.matches('/').count() > 32 {
            return Err(Error::Operation("workspace inventory cap"));
        }
        let m = std::fs::symlink_metadata(&path)?;
        if !safe_relative(&name) || m.file_type().is_symlink() {
            return Err(Error::Signal(
                report_pipeline::signals::Signal::GateTampering,
            ));
        }
        if m.is_dir() {
            files(root, &path, map, total)?;
        } else if m.is_file() {
            *total += usize::try_from(m.len()).map_err(|_| Error::Operation("workspace size"))?;
            if *total > 512 * 1024 * 1024 || m.len() > 32 * 1024 * 1024 || map.len() > 100_000 {
                return Err(Error::Operation("workspace size"));
            }
            #[cfg(unix)]
            let executable = {
                use std::os::unix::fs::PermissionsExt;
                m.permissions().mode() & 0o111 != 0
            };
            #[cfg(not(unix))]
            let executable = false;
            map.insert(
                name,
                Blob {
                    mode: if executable { "100755" } else { "100644" }.into(),
                    bytes: std::fs::read(path)?,
                },
            );
        } else {
            return Err(Error::Operation("workspace special file"));
        }
    }
    Ok(())
}
pub fn scan_workspace(
    root: &Path,
    canaries: &report_pipeline::honeytoken::Honeytokens,
    ignored: &[PathBuf],
) -> Result<()> {
    let mut map = BTreeMap::new();
    files(root, root, &mut map, &mut 0)?;
    for (name, blob) in map {
        if !ignored.contains(&root.join(&name))
            && (canaries.scan(name.as_bytes()).is_some() || canaries.scan(&blob.bytes).is_some())
        {
            return Err(Error::Signal(report_pipeline::signals::Signal::CanarySeen));
        }
    }
    Ok(())
}
/// Freeze the agent's ordinary files into a fresh host-created commit.
pub fn import(
    repo: &Path,
    base: &str,
    workspace: &Path,
    frozen_path: &str,
    frozen: &[u8],
    reduced: &[u8],
    ignored: &[PathBuf],
) -> Result<String> {
    register_repro(workspace, frozen_path)?;
    let before = tree(repo, base)?;
    let mut after = BTreeMap::new();
    files(workspace, workspace, &mut after, &mut 0)?;
    for p in ignored {
        if let Ok(p) = p.strip_prefix(workspace) {
            after.remove(p.to_str().ok_or(Error::Operation("decoy path"))?);
        }
    }
    if after.get(frozen_path).is_none_or(|b| b.bytes != reduced) {
        return Err(Error::Signal(
            report_pipeline::signals::Signal::GateTampering,
        ));
    }
    after.insert(
        frozen_path.into(),
        Blob {
            mode: "100644".into(),
            bytes: frozen.to_vec(),
        },
    );
    let index_dir = tempfile::tempdir()?;
    let index = index_dir.path().join("index");
    let run = |args: &[&str], input: Option<&[u8]>| -> Result<Vec<u8>> {
        let mut c = command(repo);
        c.env("GIT_INDEX_FILE", &index)
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        if input.is_some() {
            c.stdin(Stdio::piped());
        }
        let mut child = c.spawn()?;
        if let Some(bytes) = input {
            child
                .stdin
                .take()
                .ok_or(Error::Operation("git input"))?
                .write_all(bytes)?;
        }
        let out = child.wait_with_output()?;
        if !out.status.success() {
            return Err(Error::Operation("commit import"));
        }
        Ok(out.stdout)
    };
    run(&["read-tree", base], None)?;
    for (path, old) in &before {
        if !after.contains_key(path) {
            run(&["update-index", "--force-remove", "--", path], None)?;
        } else if after
            .get(path)
            .is_some_and(|new| new.mode == old.mode && new.bytes == old.bytes)
        {
            continue;
        }
    }
    for (path, blob) in after {
        if before
            .get(&path)
            .is_some_and(|old| old.mode == blob.mode && old.bytes == blob.bytes)
        {
            continue;
        }
        let id = String::from_utf8(run(&["hash-object", "-w", "--stdin"], Some(&blob.bytes))?)
            .map_err(|_| Error::Operation("blob hash"))?;
        run(
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &blob.mode,
                id.trim(),
                &path,
            ],
            None,
        )?;
    }
    let tree = String::from_utf8(run(&["write-tree"], None)?)
        .map_err(|_| Error::Operation("tree hash"))?;
    let commit = String::from_utf8(run(
        &["commit-tree", tree.trim(), "-p", base],
        Some(b"Fix reproduced report\n"),
    )?)
    .map_err(|_| Error::Operation("commit hash"))?;
    let commit = commit.trim().to_string();
    oid(&commit)?;
    // Keep it reachable across garbage collection and restarts.
    git(
        repo,
        &[
            "update-ref",
            &format!("refs/report-fixer/{commit}"),
            &commit,
        ],
    )?;
    Ok(commit)
}

/// Also register the raw test in the final tree, so isolated CI executes it.
pub fn register_repro(workspace: &Path, path: &str) -> Result<()> {
    if !safe_relative(path) {
        return Err(Error::Operation("registration path"));
    }
    let parts: Vec<_> = path.split('/').collect();
    if parts.len() < 4 || parts[0] != "crates" {
        return Ok(());
    }
    if parts[2] == "src" {
        let lib = workspace.join("crates").join(parts[1]).join("src/lib.rs");
        let mut source = std::fs::read_to_string(&lib)?;
        let registration = format!(
            "\n#[cfg(test)]\n#[path = {:?}]\nmod ophio_pipeline_repro;\n",
            parts[3..].join("/")
        );
        if !source.ends_with(&registration) {
            source.push_str(&registration);
            std::fs::write(lib, source)?;
        }
    } else if parts[2] == "tests" && parts.len() > 4 {
        let wrapper = workspace
            .join("crates")
            .join(parts[1])
            .join("tests/ophio_pipeline_repro.rs");
        let bytes = format!(
            "// SPDX-License-Identifier: Apache-2.0\n#[path = {:?}]\nmod ophio_pipeline_repro;\n",
            parts[3..].join("/")
        );
        if wrapper.exists() && std::fs::read(&wrapper)? != bytes.as_bytes() {
            return Err(Error::Operation("test registration collision"));
        }
        std::fs::write(wrapper, bytes)?;
    }
    Ok(())
}
