// SPDX-License-Identifier: Apache-2.0
use crate::{Error, Result};
use std::{
    io::Read,
    path::Path,
    process::{Command, Stdio},
};

pub(crate) fn run(repo: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let mut child = Command::new("git")
        .arg("--no-pager")
        .arg("--no-replace-objects")
        .arg("-C")
        .arg(repo)
        .args([
            "-c",
            "core.fsmonitor=false",
            "-c",
            "diff.external=",
            "-c",
            "core.attributesFile=/dev/null",
        ])
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_EXTERNAL_DIFF")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let mut bytes = Vec::new();
    let read = child
        .stdout
        .take()
        .ok_or(Error::Git)?
        .take(8 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes);
    if read.is_err() || bytes.len() > 8 * 1024 * 1024 {
        let _ = child.kill();
        let _ = child.wait();
        return Err(Error::Invalid("git output limit or read failure"));
    }
    if !child.wait()?.success() {
        return Err(Error::Git);
    }
    Ok(bytes)
}

pub(crate) fn oid(value: &str) -> Result<()> {
    if !matches!(value.len(), 40 | 64) || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(Error::Invalid("expected full commit hash"));
    }
    Ok(())
}
pub(crate) fn commit(repo: &Path, value: &str) -> Result<()> {
    oid(value)?;
    if run(repo, &["cat-file", "-t", value])? != b"commit\n" {
        return Err(Error::Git);
    }
    Ok(())
}
pub(crate) fn paths(repo: &Path, commit: &str) -> Result<Vec<String>> {
    self::commit(repo, commit)?;
    run(repo, &["ls-tree", "-r", "--name-only", "-z", commit])?
        .split(|b| *b == 0)
        .filter(|p| !p.is_empty())
        .map(|p| String::from_utf8(p.to_vec()).map_err(|_| Error::Invalid("non-UTF-8 path")))
        .collect()
}
pub(crate) fn blob(repo: &Path, object: &str) -> Result<Vec<u8>> {
    let size = run(repo, &["cat-file", "-s", object])?;
    let size: usize = std::str::from_utf8(&size)
        .map_err(|_| Error::Git)?
        .trim()
        .parse()
        .map_err(|_| Error::Git)?;
    if size > 1024 * 1024 {
        return Err(Error::Invalid("blob too large"));
    }
    run(repo, &["cat-file", "blob", object])
}
