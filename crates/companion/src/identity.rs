// SPDX-License-Identifier: Apache-2.0
//! Persisted host identity. Secret storage can be replaced with an OS store.
use crate::{Error, Result};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    path::{Path, PathBuf},
    sync::Arc,
};
#[cfg(unix)]
use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
};
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

pub trait SecretStore: Send + Sync {
    fn load(&self) -> Result<Option<Zeroizing<Vec<u8>>>>;
    fn save(&self, bytes: &[u8], replace: bool) -> Result<()>;
}

#[derive(Debug)]
pub struct FileSecretStore {
    path: PathBuf,
}
impl FileSecretStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }
}
impl SecretStore for FileSecretStore {
    fn load(&self) -> Result<Option<Zeroizing<Vec<u8>>>> {
        match protected_read(&self.path) {
            Ok(bytes) => {
                #[cfg(windows)]
                let bytes = platform_windows::dpapi::unprotect(&bytes)?;
                Ok(Some(bytes))
            }
            Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }
    fn save(&self, bytes: &[u8], replace: bool) -> Result<()> {
        #[cfg(windows)]
        {
            let sealed = platform_windows::dpapi::protect(bytes)?;
            atomic_write(&self.path, &sealed, replace)
        }
        #[cfg(not(windows))]
        {
            atomic_write(&self.path, bytes, replace)
        }
    }
}

#[derive(Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
struct StoredIdentity {
    #[zeroize(skip)]
    host_id: protocol::HostId,
    cert: String,
    key: String,
}

pub struct Identity {
    pub host_id: protocol::HostId,
    pub certificate: CertificateDer<'static>,
    key: PrivatePkcs8KeyDer<'static>,
}
impl std::fmt::Debug for Identity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Identity")
            .field("host_id", &self.host_id)
            .field("fingerprint", &self.fingerprint())
            .finish_non_exhaustive()
    }
}
impl Identity {
    pub fn load_or_create(store: &dyn SecretStore, names: Vec<String>) -> Result<Self> {
        if let Some(bytes) = store.load()? {
            return Self::decode(&bytes);
        }
        let identity = Self::generate(protocol::HostId::new(), names)?;
        store.save(&identity.encode()?, false)?;
        Ok(identity)
    }
    pub fn rotate(store: &dyn SecretStore, names: Vec<String>) -> Result<Self> {
        let previous = store.load()?.ok_or(Error::Identity)?;
        let host_id = Self::decode(&previous)?.host_id;
        let identity = Self::generate(host_id, names)?;
        store.save(&identity.encode()?, true)?;
        Ok(identity)
    }
    pub fn generate(host_id: protocol::HostId, mut names: Vec<String>) -> Result<Self> {
        names.extend(["localhost".into(), "127.0.0.1".into(), "::1".into()]);
        names.sort();
        names.dedup();
        let key =
            rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).map_err(|_| Error::Tls)?;
        let params = rcgen::CertificateParams::new(names).map_err(|_| Error::Tls)?;
        let cert = params.self_signed(&key).map_err(|_| Error::Tls)?;
        Ok(Self {
            host_id,
            certificate: cert.der().clone(),
            key: PrivatePkcs8KeyDer::from(key.serialize_der()),
        })
    }
    fn decode(bytes: &[u8]) -> Result<Self> {
        let stored: StoredIdentity = serde_json::from_slice(bytes).map_err(|_| Error::Identity)?;
        let certificate = CertificateDer::from(
            URL_SAFE_NO_PAD
                .decode(&stored.cert)
                .map_err(|_| Error::Identity)?,
        );
        let key = PrivatePkcs8KeyDer::from(
            URL_SAFE_NO_PAD
                .decode(&stored.key)
                .map_err(|_| Error::Identity)?,
        );
        let identity = Self {
            host_id: stored.host_id,
            certificate,
            key,
        };
        identity.tls_config()?;
        Ok(identity)
    }
    fn encode(&self) -> Result<Zeroizing<Vec<u8>>> {
        let stored = StoredIdentity {
            host_id: self.host_id,
            cert: URL_SAFE_NO_PAD.encode(&self.certificate),
            key: URL_SAFE_NO_PAD.encode(self.key.secret_pkcs8_der()),
        };
        serde_json::to_vec(&stored)
            .map(Zeroizing::new)
            .map_err(|_| Error::Identity)
    }
    pub fn fingerprint(&self) -> String {
        fingerprint(&self.certificate)
    }
    pub fn tls_config(&self) -> Result<Arc<rustls::ServerConfig>> {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let mut config = rustls::ServerConfig::builder_with_provider(provider)
            .with_protocol_versions(&[&rustls::version::TLS13])
            .map_err(|_| Error::Tls)?
            .with_no_client_auth()
            .with_single_cert(vec![self.certificate.clone()], self.key.clone_key().into())
            .map_err(|_| Error::Tls)?;
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        config.max_early_data_size = 0;
        Ok(Arc::new(config))
    }
}

pub fn fingerprint(der: &[u8]) -> String {
    Sha256::digest(der)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

pub fn private_dir(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, MetadataExt};
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)?;
        let metadata = fs::symlink_metadata(path)?;
        if !metadata.is_dir()
            || metadata.uid() != rustix::process::geteuid().as_raw()
            || metadata.mode() & 0o077 != 0
        {
            return Err(Error::Identity);
        }
        Ok(())
    }
    #[cfg(windows)]
    {
        platform_windows::fs::private_dir(path).map_err(Error::Io)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        Err(Error::Unsupported("owner-only storage is unavailable"))
    }
}

pub fn private_open(path: &Path) -> Result<File> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
            .open(path)?;
        let m = file.metadata()?;
        if !m.is_file()
            || m.uid() != rustix::process::geteuid().as_raw()
            || m.mode() & 0o077 != 0
            || m.nlink() != 1
        {
            return Err(Error::Identity);
        }
        Ok(file)
    }
    #[cfg(not(unix))]
    {
        #[cfg(windows)]
        {
            platform_windows::fs::private_open(path).map_err(Error::Io)
        }
        #[cfg(not(windows))]
        {
            let _ = path;
            Err(Error::Unsupported("owner-only storage is unavailable"))
        }
    }
}
pub(crate) fn protected_read(path: &Path) -> Result<Zeroizing<Vec<u8>>> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
            .open(path)?;
        let m = file.metadata()?;
        if !m.is_file()
            || m.uid() != rustix::process::geteuid().as_raw()
            || m.mode() & 0o077 != 0
            || m.nlink() != 1
            || m.len() > 64 * 1024
        {
            return Err(Error::Identity);
        }
        // Reserve the bounded file size so growing the plaintext buffer cannot
        // leave earlier allocations behind. Recheck after a concurrent write.
        let mut bytes = Zeroizing::new(Vec::with_capacity(64 * 1024 + 1));
        file.take(64 * 1024 + 1).read_to_end(&mut bytes)?;
        if bytes.len() > 64 * 1024 {
            return Err(Error::Identity);
        }
        Ok(bytes)
    }
    #[cfg(windows)]
    {
        platform_windows::fs::protected_read(
            path,
            platform_windows::constants::SEALED_IDENTITY_LIMIT,
        )
        .map_err(Error::Io)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        Err(Error::Unsupported("owner-only storage is unavailable"))
    }
}

#[cfg(unix)]
pub(crate) fn atomic_write(path: &Path, bytes: &[u8], replace: bool) -> Result<()> {
    let parent = path.parent().ok_or(Error::Identity)?;
    private_dir(parent)?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    temp.write_all(bytes)?;
    temp.as_file().sync_all()?;
    if replace {
        temp.persist(path).map_err(|e| e.error)?;
    } else {
        temp.persist_noclobber(path).map_err(|e| e.error)?;
    }
    #[cfg(unix)]
    File::open(parent)?.sync_all()?;
    Ok(())
}

#[cfg(windows)]
pub(crate) fn atomic_write(path: &Path, bytes: &[u8], replace: bool) -> Result<()> {
    platform_windows::fs::atomic_write(path, bytes, replace).map_err(Error::Io)
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn atomic_write(_path: &Path, _bytes: &[u8], _replace: bool) -> Result<()> {
    Err(Error::Unsupported("owner-only storage is unavailable"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn secret_store_requires_explicit_replacement() {
        let dir = tempfile::tempdir().unwrap();
        // Create the protected directory ourselves, including on Windows.
        let store = FileSecretStore::new(dir.path().join("private/secret"));
        assert!(store.load().unwrap().is_none());
        store.save(b"synthetic first", false).unwrap();
        assert!(store.save(b"must not replace", false).is_err());
        assert_eq!(
            store.load().unwrap().unwrap().as_slice(),
            b"synthetic first"
        );
        store.save(b"synthetic replacement", true).unwrap();
        assert_eq!(
            store.load().unwrap().unwrap().as_slice(),
            b"synthetic replacement"
        );
        let new_store = FileSecretStore::new(dir.path().join("private/new-secret"));
        new_store.save(b"create on replace", true).unwrap();
        assert_eq!(
            new_store.load().unwrap().unwrap().as_slice(),
            b"create on replace"
        );
    }

    #[cfg(windows)]
    #[test]
    fn identity_and_config_use_distinct_protected_formats() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("private/identity");
        let store = FileSecretStore::new(path.clone());
        let first = Identity::load_or_create(&store, vec![]).unwrap();
        let second = Identity::load_or_create(&store, vec![]).unwrap();
        assert_eq!(first.fingerprint(), second.fingerprint());
        let sealed = protected_read(&path).unwrap();
        assert!(serde_json::from_slice::<serde_json::Value>(&sealed).is_err());
        assert!(platform_windows::dpapi::unprotect(&sealed).is_ok());
        let rotated = Identity::rotate(&store, vec![]).unwrap();
        assert_eq!(rotated.host_id, first.host_id);
        assert_ne!(rotated.fingerprint(), first.fingerprint());
        let config_path = path.with_file_name("config.toml");
        crate::config::Config::default().save(&config_path).unwrap();
        assert!(crate::config::Config::load(&config_path).is_ok());
        assert!(
            platform_windows::dpapi::unprotect(&protected_read(&config_path).unwrap()).is_err()
        );
        atomic_write(&path, b"unprotected legacy identity", true).unwrap();
        assert!(store.load().is_err());
    }
    #[cfg(unix)]
    #[test]
    fn persisted_identity_and_explicit_rotation() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        // The store creates its own owner-only directory; the temporary one
        // follows the umask.
        let store = FileSecretStore::new(dir.path().join("private/identity.json"));
        let first = Identity::load_or_create(&store, vec!["test-host".into()]).unwrap();
        let second = Identity::load_or_create(&store, vec![]).unwrap();
        assert_eq!(first.host_id, second.host_id);
        assert_eq!(first.fingerprint(), second.fingerprint());
        assert_eq!(
            fs::metadata(&store.path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let rotated = Identity::rotate(&store, vec![]).unwrap();
        assert_eq!(first.host_id, rotated.host_id);
        assert_ne!(first.fingerprint(), rotated.fingerprint());
        fs::set_permissions(&store.path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(store.load().is_err());
    }
}
