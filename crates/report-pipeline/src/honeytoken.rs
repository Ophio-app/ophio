// SPDX-License-Identifier: Apache-2.0
use crate::{Error, Result, hex, signals::Signal};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Component, Path, PathBuf},
};
use zeroize::Zeroizing;

/// Debug deliberately omits all active values.
#[derive(Clone)]
pub struct Honeytokens {
    marker: Zeroizing<Vec<u8>>,
    values: Vec<Zeroizing<String>>,
    failure: Vec<usize>,
    watched: Vec<(Zeroizing<Vec<u8>>, Vec<usize>)>,
}
impl std::fmt::Debug for Honeytokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Honeytokens").finish_non_exhaustive()
    }
}
#[derive(Debug, Clone)]
pub struct Planted {
    pub files: Vec<PathBuf>,
}
impl Honeytokens {
    pub fn generate() -> Result<Self> {
        let mut random = [0u8; 24];
        getrandom::fill(&mut random).map_err(|_| Error::Invalid("randomness unavailable"))?;
        let marker = format!("GENERATED_CANARY_{}", hex(&random));
        let values = vec![
            format!("sk-ant-{marker}-INVALID"),
            format!("sk-proj-{marker}-INVALID"),
            format!("ghp_{marker}_INVALID"),
            format!("android-keystore-{marker}-INVALID"),
            format!(
                "{}\n{marker}\nINVALID GENERATED DECOY\n{}",
                ["-----BEGIN OPENSSH", "PRIVATE KEY-----"].join(" "),
                ["-----END OPENSSH", "PRIVATE KEY-----"].join(" ")
            ),
        ];
        let marker = marker.into_bytes();
        let failure = failure_table(&marker);
        Ok(Self {
            marker: Zeroizing::new(marker),
            values: values.into_iter().map(Zeroizing::new).collect(),
            failure,
            watched: vec![],
        })
    }
    /// Additional host-only values. Neither Debug nor scan results expose them.
    pub fn watch(&mut self, bytes: &[u8]) {
        if !bytes.is_empty()
            && !self
                .watched
                .iter()
                .any(|(value, _)| value.as_slice() == bytes)
        {
            self.watched
                .push((Zeroizing::new(bytes.to_vec()), failure_table(bytes)));
        }
    }
    /// Linear-time matching with no allocation, suitable for every relay body.
    /// Scan decoded bodies and reassembled streams, not just transport chunks.
    pub fn scan(&self, bytes: &[u8]) -> Option<Signal> {
        (contains(bytes, &self.marker, &self.failure)
            || self
                .watched
                .iter()
                .any(|(value, failure)| contains(bytes, value, failure)))
        .then_some(Signal::CanarySeen)
    }
    /// Only use a fresh sandbox staging root. Never pass a real home or checkout.
    /// Files are created exclusively; symlinks and existing files are rejected.
    pub fn plant(&self, dir: &Path) -> Result<Planted> {
        let root = dir.canonicalize()?;
        if root != dir || !root.is_dir() {
            return Err(Error::Invalid("plant root must be absolute and canonical"));
        }
        let specs = [
            ("home/.config/claude/credentials.json", 0),
            ("home/.codex/auth.json", 1),
            ("clone/.env", 2),
            ("clone/release-signing/storepass.txt", 3),
            ("home/.ssh/id_ed25519", 4),
        ];
        let mut files = vec![];
        for (relative, index) in specs {
            let path = root.join(relative);
            let parent = path.parent().ok_or(Error::Invalid("decoy parent"))?;
            let mut current = root.clone();
            for part in parent
                .strip_prefix(&root)
                .map_err(|_| Error::Invalid("decoy root"))?
                .components()
            {
                let Component::Normal(p) = part else {
                    return Err(Error::Invalid("decoy path"));
                };
                current.push(p);
                match fs::create_dir(&current) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(e) => return Err(e.into()),
                }
                let m = fs::symlink_metadata(&current)?;
                if !m.is_dir() || m.file_type().is_symlink() {
                    return Err(Error::Invalid("decoy symlink"));
                }
            }
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&path)?;
            writeln!(file, "{}", self.values[index].as_str())?;
            file.sync_all()?;
            files.push(path);
        }
        Ok(Planted { files })
    }
}

fn failure_table(marker: &[u8]) -> Vec<usize> {
    let mut failure = vec![0; marker.len()];
    let mut j = 0;
    for i in 1..marker.len() {
        while j > 0 && marker[i] != marker[j] {
            j = failure[j - 1];
        }
        if marker[i] == marker[j] {
            j += 1;
        }
        failure[i] = j;
    }
    failure
}
fn contains(bytes: &[u8], marker: &[u8], failure: &[usize]) -> bool {
    let mut j = 0;
    for b in bytes {
        while j > 0 && *b != marker[j] {
            j = failure[j - 1];
        }
        if *b == marker[j] {
            j += 1;
        }
        if j == marker.len() {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn watches_real_values_without_planting_or_formatting_them() {
        let mut scanner = Honeytokens::generate().unwrap();
        scanner.watch(b"");
        scanner.watch(b"fake-private-intake-token");
        scanner.watch(b"fake-private-intake-token");
        let root = tempfile::tempdir().unwrap();
        let planted = scanner.plant(root.path()).unwrap();
        assert!(
            scanner
                .scan(b"prefix fake-private-intake-token suffix")
                .is_some()
        );
        assert!(scanner.scan(b"fake-private-intake-toke").is_none());
        assert!(!format!("{scanner:?}").contains("fake-private"));
        for file in planted.files {
            assert!(
                !std::fs::read_to_string(file)
                    .unwrap()
                    .contains("fake-private")
            );
        }
    }
}
