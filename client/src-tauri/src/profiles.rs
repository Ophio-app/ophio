// SPDX-License-Identifier: Apache-2.0
use crate::{
    error::{Error, Result},
    keys::ProtectedKey,
};
use protocol::{DeviceId, HostId};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
};

/// Longest name the person can give a computer on this device.
const NAME_MAX: usize = 64;
/// Most computers this device remembers.
const MAX_COMPUTERS: usize = 32;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostProfile {
    pub profile_id: String,
    pub host_id: HostId,
    pub name: String,
    pub addresses: Vec<String>,
    pub cert_sha256: String,
    pub paired_at: i64,
    pub device_id: DeviceId,
    pub key_protection: String,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Record {
    pub profile: HostProfile,
    pub key: ProtectedKey,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    version: u32,
    profiles: BTreeMap<String, Record>,
}

pub struct Profiles {
    path: PathBuf,
    records: BTreeMap<String, Record>,
}
impl Profiles {
    pub fn open(root: PathBuf) -> Result<Self> {
        fs::create_dir_all(&root).map_err(|_| Error::Storage)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&root, fs::Permissions::from_mode(0o700))
                .map_err(|_| Error::Storage)?;
        }
        let path = root.join("profiles.json");
        let records = match fs::read(&path) {
            Ok(bytes) if bytes.len() <= 1024 * 1024 => {
                let file: File = serde_json::from_slice(&bytes).map_err(|_| Error::Storage)?;
                if file.version != 1 {
                    return Err(Error::Storage);
                }
                file.profiles
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            _ => return Err(Error::Storage),
        };
        Ok(Self { path, records })
    }
    pub fn list(&self) -> Vec<HostProfile> {
        self.records.values().map(|r| r.profile.clone()).collect()
    }
    pub fn get(&self, id: &str) -> Result<Record> {
        self.records.get(id).cloned().ok_or(Error::NotPaired)
    }
    /// Whether this device may pair with a computer, checked before it dials
    /// so the computer never enrolls a key this device then throws away. A
    /// computer already paired here is paired again only in place of its
    /// earlier profile, when the person chose that. Returns the device that
    /// profile paired as, which the computer's owner is asked to replace.
    pub fn admits(&self, host_id: &HostId, replace: bool) -> Result<Option<DeviceId>> {
        let earlier = self.earlier(host_id).count();
        if earlier > 0 && !replace {
            return Err(Error::Invalid(
                "This phone is already paired with that computer. Choose to replace it to pair again.",
            ));
        }
        if self.records.len() - earlier >= MAX_COMPUTERS {
            return Err(Error::Invalid(
                "This phone already remembers 32 computers. Forget one before pairing another.",
            ));
        }
        Ok(self
            .earlier(host_id)
            .max_by_key(|r| r.profile.paired_at)
            .map(|r| r.profile.device_id))
    }
    /// Keeps a newly paired computer in place of every earlier profile of it,
    /// and returns their ids so their keys and unsent messages can go too.
    pub fn insert(&mut self, record: Record, replace: bool) -> Result<Vec<String>> {
        self.admits(&record.profile.host_id, replace)?;
        let replaced: Vec<String> = self
            .earlier(&record.profile.host_id)
            .map(|r| r.profile.profile_id.clone())
            .collect();
        let mut next = self.records.clone();
        for id in &replaced {
            next.remove(id);
        }
        next.insert(record.profile.profile_id.clone(), record);
        self.commit(next)?;
        Ok(replaced)
    }
    fn earlier<'a>(&'a self, host_id: &'a HostId) -> impl Iterator<Item = &'a Record> {
        self.records
            .values()
            .filter(move |r| r.profile.host_id == *host_id)
    }
    /// The name this device shows for a computer. The computer keeps its own.
    pub fn rename(&mut self, id: &str, name: &str) -> Result<HostProfile> {
        let name = name.trim();
        if name.is_empty() || name.chars().count() > NAME_MAX || name.chars().any(char::is_control)
        {
            return Err(Error::Invalid(
                "Give the computer a name of up to 64 characters.",
            ));
        }
        let mut next = self.records.clone();
        let record = next.get_mut(id).ok_or(Error::NotPaired)?;
        record.profile.name = name.to_owned();
        let profile = record.profile.clone();
        self.commit(next)?;
        Ok(profile)
    }
    pub fn remove(&mut self, id: &str) -> Result<()> {
        let mut next = self.records.clone();
        next.remove(id);
        self.commit(next)
    }
    fn commit(&mut self, next: BTreeMap<String, Record>) -> Result<()> {
        let bytes = serde_json::to_vec(&File {
            version: 1,
            profiles: next.clone(),
        })
        .map_err(|_| Error::Storage)?;
        write_private(&self.path, &bytes)?;
        self.records = next;
        Ok(())
    }
}
/// Replaces `path` with `bytes` all at once, readable only by this app.
pub(crate) fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let temp = path.with_extension("pending");
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut file = opts.open(&temp).map_err(|_| Error::Storage)?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|_| Error::Storage)?;
    fs::rename(&temp, path).map_err(|_| Error::Storage)?;
    if let Some(parent) = path.parent() {
        fs::File::open(parent)
            .and_then(|f| f.sync_all())
            .map_err(|_| Error::Storage)?;
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    fn record(id: &str, host_id: HostId, name: &str) -> Record {
        Record {
            profile: HostProfile {
                profile_id: id.into(),
                host_id,
                name: name.into(),
                addresses: vec!["127.0.0.1:47100".into()],
                cert_sha256: "ab".repeat(32),
                paired_at: 1,
                device_id: DeviceId::new(),
                key_protection: "keystore".into(),
            },
            key: ProtectedKey::Keystore,
        }
    }
    #[test]
    fn a_computer_has_one_profile_and_is_replaced_only_when_the_person_chose_to() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Profiles::open(dir.path().to_owned()).unwrap();
        let desk = HostId::new();
        assert_eq!(store.admits(&desk, false).unwrap(), None);
        let old = record("old", desk, "Desk");
        let earlier = old.profile.device_id;
        store.insert(old, false).unwrap();
        assert!(matches!(store.admits(&desk, false), Err(Error::Invalid(_))));
        // Replacing names the device the earlier profile paired as.
        assert_eq!(store.admits(&desk, true).unwrap(), Some(earlier));
        assert!(matches!(
            store.insert(record("new", desk, "Desk"), false),
            Err(Error::Invalid(_))
        ));
        assert_eq!(
            store.insert(record("new", desk, "Desk"), true).unwrap(),
            ["old"]
        );
        let store = Profiles::open(dir.path().to_owned()).unwrap();
        let ids: Vec<_> = store.list().into_iter().map(|p| p.profile_id).collect();
        assert_eq!(ids, ["new"]);
    }
    #[test]
    fn the_limit_of_32_computers_is_known_before_dialing() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Profiles::open(dir.path().to_owned()).unwrap();
        let hosts: Vec<_> = (0..32).map(|_| HostId::new()).collect();
        for (n, host) in hosts.iter().enumerate() {
            store
                .insert(record(&n.to_string(), *host, "Computer"), false)
                .unwrap();
        }
        let another = HostId::new();
        assert!(matches!(
            store.admits(&another, false),
            Err(Error::Invalid(_))
        ));
        assert!(matches!(
            store.admits(&another, true),
            Err(Error::Invalid(_))
        ));
        // Pairing again with one of them keeps the count.
        store.admits(&hosts[5], true).unwrap();
        assert_eq!(
            store
                .insert(record("again", hosts[5], "Computer"), true)
                .unwrap(),
            ["5"]
        );
        assert_eq!(store.list().len(), 32);
    }
    #[test]
    fn rejects_corruption_instead_of_forgetting_devices() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("profiles.json"), b"bad").unwrap();
        assert!(Profiles::open(dir.path().to_owned()).is_err());
    }
    #[test]
    fn rename_survives_restart_and_refuses_unusable_names() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Profiles::open(dir.path().to_owned()).unwrap();
        store
            .insert(record("id", HostId::new(), "studio"), false)
            .unwrap();
        assert_eq!(
            store.rename("id", "  Old studio ").unwrap().name,
            "Old studio"
        );
        for bad in ["", "   ", "line\nbreak", &"x".repeat(65)] {
            assert!(matches!(store.rename("id", bad), Err(Error::Invalid(_))));
        }
        assert!(matches!(
            store.rename("other", "Desk"),
            Err(Error::NotPaired)
        ));
        let store = Profiles::open(dir.path().to_owned()).unwrap();
        assert_eq!(store.list()[0].name, "Old studio");
    }
    #[test]
    fn profile_survives_restart_then_forget() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Profiles::open(dir.path().to_owned()).unwrap();
        store
            .insert(record("id", HostId::new(), "Computer"), false)
            .unwrap();
        let mut store = Profiles::open(dir.path().to_owned()).unwrap();
        assert_eq!(store.list().len(), 1);
        store.remove("id").unwrap();
        assert!(
            Profiles::open(dir.path().to_owned())
                .unwrap()
                .list()
                .is_empty()
        );
    }
}
