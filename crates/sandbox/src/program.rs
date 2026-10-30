// SPDX-License-Identifier: Apache-2.0
use std::{
    collections::BTreeSet,
    fs::File,
    io::{self, Read},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

/// Resolve executable files without running wrapper text or exposing their folders.
/// Static absolute assignments and exec targets cover installed native launchers.
pub fn files(program: &Path, home: &Path) -> io::Result<Vec<PathBuf>> {
    let mut files = BTreeSet::new();
    let mut pending = vec![program.to_owned()];
    while let Some(next) = pending.pop() {
        if files.len() > 32 || !next.is_absolute() {
            return Err(io::Error::other("unsupported executable wrapper"));
        }
        let resolved = next.canonicalize()?;
        if !files.insert(next) {
            continue;
        }
        files.insert(resolved.clone());
        let metadata = resolved.metadata()?;
        if !metadata.is_file() || metadata.permissions().mode() & 0o111 == 0 {
            return Err(io::Error::other("agent program is not executable"));
        }
        let mut file = File::open(&resolved)?;
        let mut magic = [0; 2];
        file.read_exact(&mut magic)?;
        if &magic != b"#!" {
            if resolved.file_name().is_some_and(|n| n == "codex") {
                for helper in ["codex-code-mode-host", "codex-responses-api-proxy"] {
                    let helper = resolved.with_file_name(helper);
                    if helper.is_file() {
                        files.insert(helper);
                    }
                }
            }
            continue;
        }
        let mut bytes = Vec::new();
        file.take(16 * 1024).read_to_end(&mut bytes)?;
        let text = std::str::from_utf8(&bytes).map_err(io::Error::other)?;
        for line in text.lines() {
            let line = line.trim();
            let token = line
                .strip_prefix("exec ")
                .and_then(|l| l.split_whitespace().next())
                .or_else(|| line.split_once('=').map(|(_, value)| value));
            let Some(token) = token else { continue };
            let token = token.trim_matches(['\'', '"']);
            let path = if let Some(suffix) = token
                .strip_prefix("$HOME/")
                .or_else(|| token.strip_prefix("${HOME}/"))
            {
                home.join(suffix)
            } else {
                PathBuf::from(token)
            };
            if (path.is_absolute() && !token.contains(['`', ';', '$']) && path.is_file())
                || token.starts_with("$HOME/")
                || token.starts_with("${HOME}/")
            {
                pending.push(path);
            }
        }
    }
    Ok(files.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn resolves_home_wrapper_and_bounds_cycles_without_execution() {
        let dir = tempfile::tempdir().unwrap();
        let wrapper = dir.path().join("wrapper");
        let target = dir.path().join("native");
        std::fs::write(&target, b"ELF fake").unwrap();
        std::fs::write(&wrapper, "#!/bin/sh\nexec \"$HOME/native\" \"$@\"\n").unwrap();
        for path in [&wrapper, &target] {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        assert!(files(&wrapper, dir.path()).unwrap().contains(&target));
        std::fs::write(&target, "#!/bin/sh\nexec \"$HOME/wrapper\" \"$@\"\n").unwrap();
        assert!(files(&wrapper, dir.path()).is_ok());
    }
}
