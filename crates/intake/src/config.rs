// SPDX-License-Identifier: Apache-2.0
use crate::{Error, Result};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use std::{fs, io::Write, net::IpAddr, path::Path};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub retention_days: u32,
    pub reports_per_key: Bucket,
    pub reports_per_ip: Bucket,
    pub requests_per_key: Bucket,
    pub requests_per_ip: Bucket,
    /// Only this immediate peer may supply X-Forwarded-For (one IP, no chain).
    pub trusted_proxy: Option<IpAddr>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bucket {
    pub capacity: u32,
    pub refill_seconds: u32,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            retention_days: 90,
            reports_per_key: Bucket {
                capacity: 4,
                refill_seconds: 900,
            },
            reports_per_ip: Bucket {
                capacity: 24,
                refill_seconds: 150,
            },
            requests_per_key: Bucket {
                capacity: 120,
                refill_seconds: 1,
            },
            requests_per_ip: Bucket {
                capacity: 240,
                refill_seconds: 1,
            },
            trusted_proxy: None,
        }
    }
}
impl Config {
    pub fn load(data: &Path) -> Result<Self> {
        let config: Self = serde_json::from_slice(&fs::read(data.join("config.json"))?)?;
        config.validate()?;
        Ok(config)
    }
    pub fn validate(&self) -> Result<()> {
        for bucket in [
            self.reports_per_key,
            self.reports_per_ip,
            self.requests_per_key,
            self.requests_per_ip,
        ] {
            if bucket.capacity == 0 || bucket.refill_seconds == 0 {
                return Err(Error::Invalid);
            }
        }
        Ok(())
    }
}

/// Create once; never replace an existing database, token or configuration.
pub fn init(data: &Path) -> Result<()> {
    for name in ["maintainer.token", "config.json", "intake.sqlite"] {
        if fs::symlink_metadata(data.join(name)).is_ok() {
            return Err(Error::Invalid);
        }
    }
    fs::create_dir_all(data)?;
    private_dir(data)?;
    let token = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    write_private(
        &data.join("maintainer.token"),
        URL_SAFE_NO_PAD.encode(token).as_bytes(),
    )?;
    write_private(
        &data.join("config.json"),
        &serde_json::to_vec_pretty(&Config::default())?,
    )?;
    crate::Store::open(data.join("intake.sqlite"))?;
    Ok(())
}

pub fn read_token(path: &Path) -> Result<String> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        if fs::symlink_metadata(path)?.file_type().is_symlink() {
            return Err(Error::Invalid);
        }
    }
    let mut file = options.open(path)?;
    if !file.metadata()?.is_file() || file.metadata()?.len() > 257 {
        return Err(Error::Invalid);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if file.metadata()?.permissions().mode() & 0o777 != 0o600 {
            return Err(Error::Invalid);
        }
    }
    use std::io::Read;
    let mut token = String::new();
    Read::by_ref(&mut file)
        .take(257)
        .read_to_string(&mut token)?;
    let token = token.trim().to_owned();
    if !(32..=256).contains(&token.len()) || !token.bytes().all(|b| b.is_ascii_graphic()) {
        return Err(Error::Invalid);
    }
    Ok(token)
}

fn private_dir(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}
fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut options = fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)?.write_all(bytes)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn init_is_private_and_never_replaces_existing_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data");
        init(&path).unwrap();
        let token = read_token(&path.join("maintainer.token")).unwrap();
        assert!(token.len() >= 32);
        assert!(Config::load(&path).is_ok());
        assert!(init(&path).is_err());
        assert_eq!(read_token(&path.join("maintainer.token")).unwrap(), token);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o700
            );
            for name in ["intake.sqlite", "config.json", "maintainer.token"] {
                assert_eq!(
                    fs::metadata(path.join(name)).unwrap().permissions().mode() & 0o777,
                    0o600
                );
            }
            let token_path = path.join("maintainer.token");
            fs::set_permissions(&token_path, fs::Permissions::from_mode(0o644)).unwrap();
            assert!(read_token(&token_path).is_err());
            let link = path.join("token-link");
            std::os::unix::fs::symlink(&token_path, &link).unwrap();
            assert!(read_token(&link).is_err());
        }
    }
    #[test]
    fn partial_init_does_not_create_or_replace_credentials() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("config.json"), b"{}").unwrap();
        assert!(init(dir.path()).is_err());
        assert!(!dir.path().join("maintainer.token").exists());
    }
    #[test]
    fn zero_capacity_and_refill_are_invalid() {
        let mut config = Config::default();
        config.requests_per_ip.capacity = 0;
        assert!(config.validate().is_err());
        config.requests_per_ip.capacity = 1;
        config.requests_per_ip.refill_seconds = 0;
        assert!(config.validate().is_err());
    }
}
