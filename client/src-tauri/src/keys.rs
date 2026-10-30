// SPDX-License-Identifier: Apache-2.0
//! Private seeds are only transient in native memory. Storage contains either a
//! Keystore alias or authenticated ciphertext bound to that alias.
use crate::error::{Error, Result};
use async_trait::async_trait;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ed25519_dalek::{Signer, SigningKey};
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum ProtectedKey {
    Keystore,
    Wrapped { sealed: String },
}
impl ProtectedKey {
    pub fn protection(&self) -> &'static str {
        match self {
            Self::Keystore => "keystore",
            Self::Wrapped { .. } => "wrapped",
        }
    }
}

#[async_trait]
pub trait KeyVault: Send + Sync {
    /// None means Ed25519 is unavailable, not that all secure storage failed.
    async fn generate(&self, alias: &str) -> Result<Option<[u8; 32]>>;
    async fn sign(&self, alias: &str, transcript: &[u8]) -> Result<[u8; 64]>;
    async fn wrap(&self, alias: &str, seed: &[u8; 32]) -> Result<String>;
    async fn unwrap(&self, alias: &str, sealed: &str) -> Result<Zeroizing<Vec<u8>>>;
    async fn delete(&self, alias: &str) -> Result<()>;
}

pub async fn create(vault: &dyn KeyVault, alias: &str) -> Result<(ProtectedKey, String)> {
    if let Some(public) = vault.generate(alias).await? {
        return Ok((ProtectedKey::Keystore, URL_SAFE_NO_PAD.encode(public)));
    }
    let seed = random::<32>()?;
    let signing = SigningKey::from_bytes(&seed);
    let sealed = vault.wrap(alias, &seed).await?;
    Ok((
        ProtectedKey::Wrapped { sealed },
        URL_SAFE_NO_PAD.encode(signing.verifying_key().to_bytes()),
    ))
}

pub async fn sign(
    vault: &dyn KeyVault,
    alias: &str,
    key: &ProtectedKey,
    transcript: &[u8],
) -> Result<[u8; 64]> {
    match key {
        ProtectedKey::Keystore => vault.sign(alias, transcript).await,
        ProtectedKey::Wrapped { sealed } => {
            let seed = vault.unwrap(alias, sealed).await?;
            let bytes: &[u8; 32] = seed.as_slice().try_into().map_err(|_| Error::Key)?;
            Ok(SigningKey::from_bytes(bytes).sign(transcript).to_bytes())
        }
    }
}

pub fn random<const N: usize>() -> Result<Zeroizing<[u8; N]>> {
    let mut bytes = Zeroizing::new([0; N]);
    SystemRandom::new()
        .fill(bytes.as_mut())
        .map_err(|_| Error::Key)?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signature, VerifyingKey};
    use ring::aead::{AES_256_GCM, Aad, LessSafeKey, Nonce, UnboundKey};
    struct WrappingVault;
    fn aes() -> LessSafeKey {
        LessSafeKey::new(UnboundKey::new(&AES_256_GCM, &[91; 32]).unwrap())
    }
    #[async_trait]
    impl KeyVault for WrappingVault {
        async fn generate(&self, _: &str) -> Result<Option<[u8; 32]>> {
            Ok(None)
        }
        async fn sign(&self, _: &str, _: &[u8]) -> Result<[u8; 64]> {
            Err(Error::Key)
        }
        async fn wrap(&self, alias: &str, seed: &[u8; 32]) -> Result<String> {
            let nonce = random::<12>()?;
            let mut ciphertext = seed.to_vec();
            aes()
                .seal_in_place_append_tag(
                    Nonce::assume_unique_for_key(*nonce),
                    Aad::from(alias.as_bytes()),
                    &mut ciphertext,
                )
                .map_err(|_| Error::Key)?;
            let mut sealed = nonce.to_vec();
            sealed.extend(ciphertext);
            Ok(URL_SAFE_NO_PAD.encode(sealed))
        }
        async fn unwrap(&self, alias: &str, sealed: &str) -> Result<Zeroizing<Vec<u8>>> {
            let sealed = URL_SAFE_NO_PAD.decode(sealed).map_err(|_| Error::Key)?;
            if sealed.len() < 28 {
                return Err(Error::Key);
            }
            let mut out = Zeroizing::new(sealed[12..].to_vec());
            let nonce = Nonce::try_assume_unique_for_key(&sealed[..12]).map_err(|_| Error::Key)?;
            let len = aes()
                .open_in_place(nonce, Aad::from(alias.as_bytes()), &mut out)
                .map_err(|_| Error::Key)?
                .len();
            out.truncate(len);
            Ok(out)
        }
        async fn delete(&self, _: &str) -> Result<()> {
            Ok(())
        }
    }
    #[tokio::test]
    async fn wrapped_key_round_trip_and_context_binding() {
        let (key, public) = create(&WrappingVault, "one").await.unwrap();
        assert_eq!(key.protection(), "wrapped");
        let encoded = serde_json::to_string(&key).unwrap();
        let recovered: ProtectedKey = serde_json::from_str(&encoded).unwrap();
        let sig = sign(&WrappingVault, "one", &recovered, b"transcript")
            .await
            .unwrap();
        let public: [u8; 32] = URL_SAFE_NO_PAD.decode(public).unwrap().try_into().unwrap();
        VerifyingKey::from_bytes(&public)
            .unwrap()
            .verify_strict(b"transcript", &Signature::from_bytes(&sig))
            .unwrap();
        assert!(
            sign(&WrappingVault, "two", &key, b"transcript")
                .await
                .is_err()
        );
        let ProtectedKey::Wrapped { mut sealed } = key else {
            panic!("wrapped")
        };
        sealed.replace_range(20..21, if &sealed[20..21] == "A" { "B" } else { "A" });
        assert!(
            sign(
                &WrappingVault,
                "one",
                &ProtectedKey::Wrapped { sealed },
                b"transcript"
            )
            .await
            .is_err()
        );
    }
}
