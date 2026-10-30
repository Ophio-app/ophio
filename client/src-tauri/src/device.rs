// SPDX-License-Identifier: Apache-2.0
use crate::{
    error::{Error, Result},
    keys::KeyVault,
};
use async_trait::async_trait;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::json;
use tauri_plugin_device::DeviceExt;
use zeroize::Zeroizing;

#[derive(Clone)]
pub struct AndroidDevice(pub tauri::AppHandle);
impl AndroidDevice {
    pub async fn call<T: DeserializeOwned + Send + 'static>(
        &self,
        command: &str,
        args: impl Serialize,
    ) -> Result<T> {
        self.0
            .device()
            .call(command, args)
            .await
            .map_err(|e| Error::Native(e.to_string()))
    }
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PublicKey {
    public_key: Option<String>,
}
#[derive(Deserialize)]
struct Signature {
    signature: String,
}
#[derive(Deserialize)]
struct Sealed {
    sealed: String,
}
#[derive(Deserialize)]
struct Seed {
    seed: String,
}

#[async_trait]
impl KeyVault for AndroidDevice {
    async fn generate(&self, alias: &str) -> Result<Option<[u8; 32]>> {
        let result: PublicKey = self.call("keyCreate", json!({"alias":alias})).await?;
        result
            .public_key
            .map(|p| {
                URL_SAFE_NO_PAD
                    .decode(p)
                    .map_err(|_| Error::Key)?
                    .try_into()
                    .map_err(|_| Error::Key)
            })
            .transpose()
    }
    async fn sign(&self, alias: &str, transcript: &[u8]) -> Result<[u8; 64]> {
        let result: Signature = self
            .call(
                "keySign",
                json!({"alias":alias,"transcript":URL_SAFE_NO_PAD.encode(transcript)}),
            )
            .await?;
        URL_SAFE_NO_PAD
            .decode(result.signature)
            .map_err(|_| Error::Key)?
            .try_into()
            .map_err(|_| Error::Key)
    }
    async fn wrap(&self, alias: &str, seed: &[u8; 32]) -> Result<String> {
        let result: Sealed = self
            .call(
                "keyWrap",
                json!({"alias":alias,"seed":URL_SAFE_NO_PAD.encode(seed)}),
            )
            .await?;
        Ok(result.sealed)
    }
    async fn unwrap(&self, alias: &str, sealed: &str) -> Result<Zeroizing<Vec<u8>>> {
        let result: Seed = self
            .call("keyUnwrap", json!({"alias":alias,"sealed":sealed}))
            .await?;
        let encoded = Zeroizing::new(result.seed);
        Ok(Zeroizing::new(
            URL_SAFE_NO_PAD
                .decode(encoded.as_bytes())
                .map_err(|_| Error::Key)?,
        ))
    }
    async fn delete(&self, alias: &str) -> Result<()> {
        self.call("keyDelete", json!({"alias":alias})).await
    }
}

#[derive(Deserialize, Default)]
pub struct AudioChunk {
    #[serde(default)]
    pub pcm: Option<String>,
    #[serde(default)]
    pub rms: f64,
    #[serde(default)]
    pub error: Option<String>,
}
#[async_trait]
pub trait Audio: Send + Sync {
    async fn start(&self, id: u32) -> Result<()>;
    async fn read(&self, id: u32) -> Result<AudioChunk>;
    async fn stop(&self, id: u32, drain: bool) -> Result<Vec<u8>>;
}
#[async_trait]
impl Audio for AndroidDevice {
    async fn start(&self, id: u32) -> Result<()> {
        self.call("micStart", json!({"linkId":id})).await
    }
    async fn read(&self, id: u32) -> Result<AudioChunk> {
        self.call("micRead", json!({"linkId":id})).await
    }
    async fn stop(&self, id: u32, drain: bool) -> Result<Vec<u8>> {
        let r: AudioChunk = self
            .call("micStop", json!({"linkId":id,"drain":drain}))
            .await?;
        URL_SAFE_NO_PAD
            .decode(r.pcm.unwrap_or_default())
            .map_err(|_| Error::Key)
    }
}
