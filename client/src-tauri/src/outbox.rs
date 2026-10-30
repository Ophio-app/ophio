// SPDX-License-Identifier: Apache-2.0
//
// Sends whose fate the phone may not learn. Each record is on disk (synced)
// before its command leaves the phone, so an app killed mid-send asks the
// computer about that command id on its next start instead of sending anew.
use crate::error::{Error, Result};
use std::{fs, io::Write, path::PathBuf};

const MAX_RECORD: usize = 256 * 1024;
const MAX_RECORDS: usize = 64;

pub struct Outbox {
    root: PathBuf,
}

/// Profile and command ids are generated identifiers; anything else is refused
/// so a record can never name a path outside its directory.
fn name(id: &str) -> Result<&str> {
    let ok = !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    if ok {
        Ok(id)
    } else {
        Err(Error::Invalid("That identifier isn’t valid."))
    }
}

fn unrecorded() -> Error {
    Error::Native("The phone could not record this send.".into())
}

fn sync_dir(dir: &std::path::Path) -> Result<()> {
    fs::File::open(dir)
        .and_then(|f| f.sync_all())
        .map_err(|_| unrecorded())
}

impl Outbox {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root: root.join("outbox"),
        }
    }

    fn dir(&self, profile: &str) -> Result<PathBuf> {
        Ok(self.root.join(name(profile)?))
    }

    pub fn put(&self, profile: &str, id: &str, record: &str) -> Result<()> {
        if record.len() > MAX_RECORD {
            return Err(Error::Invalid("That send is too large to record."));
        }
        let dir = self.dir(profile)?;
        let path = dir.join(format!("{}.json", name(id)?));
        fs::create_dir_all(&dir).map_err(|_| unrecorded())?;
        if !path.exists() && self.ids(&dir)?.len() >= MAX_RECORDS {
            return Err(Error::Invalid(
                "Too many sends are waiting for an answer from this computer.",
            ));
        }
        let temp = path.with_extension("pending");
        let mut opts = fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut file = opts.open(&temp).map_err(|_| unrecorded())?;
        file.write_all(record.as_bytes())
            .and_then(|()| file.sync_all())
            .map_err(|_| unrecorded())?;
        fs::rename(&temp, &path).map_err(|_| unrecorded())?;
        sync_dir(&dir)
    }

    fn ids(&self, dir: &std::path::Path) -> Result<Vec<String>> {
        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(_) => return Err(unrecorded()),
        };
        let mut ids = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|_| unrecorded())?;
            let file = entry.file_name();
            if let Some(id) = file.to_str().and_then(|n| n.strip_suffix(".json"))
                && name(id).is_ok()
            {
                ids.push(id.to_owned());
            }
        }
        ids.sort();
        Ok(ids)
    }

    pub fn list(&self, profile: &str) -> Result<Vec<String>> {
        let dir = self.dir(profile)?;
        let mut records = Vec::new();
        for id in self.ids(&dir)? {
            let bytes = fs::read(dir.join(format!("{id}.json"))).map_err(|_| unrecorded())?;
            if bytes.len() <= MAX_RECORD
                && let Ok(text) = String::from_utf8(bytes)
            {
                records.push(text);
            }
        }
        Ok(records)
    }

    pub fn remove(&self, profile: &str, id: &str) -> Result<()> {
        let dir = self.dir(profile)?;
        match fs::remove_file(dir.join(format!("{}.json", name(id)?))) {
            Ok(()) => sync_dir(&dir),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(unrecorded()),
        }
    }

    /// Forgetting a computer forgets what the phone was waiting to hear from it.
    pub fn forget(&self, profile: &str) -> Result<()> {
        match fs::remove_dir_all(self.dir(profile)?) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(unrecorded()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("outbox-{}", uuid_like()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }
    fn uuid_like() -> String {
        use std::time::{SystemTime, UNIX_EPOCH};
        format!(
            "{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        )
    }

    #[test]
    fn a_record_survives_reopening_until_removed() {
        let root = temp();
        Outbox::new(root.clone())
            .put("p1", "0190-a", r#"{"a":1}"#)
            .unwrap();
        let reopened = Outbox::new(root.clone());
        assert_eq!(reopened.list("p1").unwrap(), vec![r#"{"a":1}"#.to_owned()]);
        assert!(reopened.list("p2").unwrap().is_empty());
        reopened.remove("p1", "0190-a").unwrap();
        reopened.remove("p1", "0190-a").unwrap();
        assert!(reopened.list("p1").unwrap().is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn identifiers_cannot_leave_the_outbox() {
        let root = temp();
        let outbox = Outbox::new(root.clone());
        for bad in ["../x", "a/b", "", ".", "a.b"] {
            assert!(outbox.put("p1", bad, "{}").is_err(), "{bad}");
            assert!(outbox.put(bad, "id", "{}").is_err(), "{bad}");
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn bounded_and_forgotten_with_the_computer() {
        let root = temp();
        let outbox = Outbox::new(root.clone());
        assert!(
            outbox
                .put("p1", "big", &"x".repeat(MAX_RECORD + 1))
                .is_err()
        );
        for i in 0..MAX_RECORDS {
            outbox.put("p1", &format!("id{i}"), "{}").unwrap();
        }
        assert!(outbox.put("p1", "one-more", "{}").is_err());
        // Rewriting an existing record is not a new one.
        outbox.put("p1", "id0", r#"{"b":2}"#).unwrap();
        outbox.forget("p1").unwrap();
        assert!(outbox.list("p1").unwrap().is_empty());
        fs::remove_dir_all(root).unwrap();
    }
}
