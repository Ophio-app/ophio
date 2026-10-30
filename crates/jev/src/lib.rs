// SPDX-License-Identifier: Apache-2.0
//! Direct TypeSafe System One v1, which voice control asks what a person
//! meant. Endpoint, schema and model are one reviewed unit; the key is the
//! person's own and stays on the computer.
use async_trait::async_trait;
use protocol::time::Timestamp;
use serde::{Deserialize, Serialize};
use std::{sync::Arc, time::Duration};
use zeroize::Zeroizing;

pub mod questions;

pub const ENDPOINT: &str = "https://api.typesafe.ai/v1/systemone";
/// The one model whose answers are accepted.
pub const MODEL: &str = "jev-1.13.0";

/// Why Jev gave no answer to act on. Nothing is acted on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("Jev is not linked on this computer")]
    NotConnected,
    #[error("Jev could not be reached")]
    Unavailable,
    #[error("Jev did not answer in time")]
    Timeout,
    #[error("Jev's answer could not be read")]
    Malformed,
    #[error("Jev refused the request")]
    Refused,
}

/// Whether the computer holds a Jev key.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Connection {
    Connected,
    #[default]
    NotConnected,
    LastCheckFailed,
}
/// Jev access as the computer last knew it. A failed check stands until the
/// key is linked again. The key itself is never part of it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Access {
    pub connection: Connection,
    pub checked_at: Option<Timestamp>,
}

pub struct Key(Zeroizing<String>);
impl std::fmt::Debug for Key {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Key([redacted])")
    }
}
impl Key {
    pub fn new(value: String) -> Result<Self, Error> {
        let value = Zeroizing::new(value);
        if value.is_empty() || value.len() > 512 || !value.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(Error::NotConnected);
        }
        Ok(Self(value))
    }
}
#[async_trait]
pub trait KeySource: Send + Sync + std::fmt::Debug {
    async fn load(&self) -> Result<Option<Key>, Error>;
}

#[derive(Debug)]
pub struct Jev {
    client: reqwest::Client,
    keys: Arc<dyn KeySource>,
    endpoint: String,
}
impl Jev {
    pub fn new(keys: Arc<dyn KeySource>) -> Result<Self, Error> {
        Self::build(keys, ENDPOINT.into(), Duration::from_secs(8), true)
    }
    #[cfg(feature = "testing")]
    pub fn loopback(
        keys: Arc<dyn KeySource>,
        endpoint: String,
        timeout: Duration,
    ) -> Result<Self, Error> {
        let url = reqwest::Url::parse(&endpoint).map_err(|_| Error::Malformed)?;
        if url.scheme() != "http"
            || url
                .host_str()
                .and_then(|s| s.parse::<std::net::IpAddr>().ok())
                .is_none_or(|ip| !ip.is_loopback())
        {
            return Err(Error::Malformed);
        }
        Self::build(keys, endpoint, timeout, false)
    }
    fn build(
        keys: Arc<dyn KeySource>,
        endpoint: String,
        timeout: Duration,
        https: bool,
    ) -> Result<Self, Error> {
        let client = reqwest::Client::builder()
            .https_only(https)
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(timeout)
            .connect_timeout(timeout)
            .build()
            .map_err(|_| Error::Unavailable)?;
        Ok(Self {
            client,
            keys,
            endpoint,
        })
    }
    /// Whether a key is held, without asking Jev anything.
    pub async fn connection(&self) -> Connection {
        match self.keys.load().await {
            Ok(Some(_)) => Connection::Connected,
            Ok(None) => Connection::NotConnected,
            Err(_) => Connection::LastCheckFailed,
        }
    }
    /// Posts one request and reads at most `limit` bytes of the answer.
    async fn post(&self, body: &serde_json::Value, limit: usize) -> Result<Vec<u8>, Error> {
        let key = self.keys.load().await?.ok_or(Error::NotConnected)?;
        let failure = |e: reqwest::Error| {
            if e.is_timeout() {
                Error::Timeout
            } else {
                Error::Unavailable
            }
        };
        let mut response = self
            .client
            .post(&self.endpoint)
            .bearer_auth(key.0.as_str())
            .json(body)
            .send()
            .await
            .map_err(failure)?;
        if !response.status().is_success() {
            return Err(Error::Refused);
        }
        if response.content_length().is_some_and(|n| n > limit as u64) {
            return Err(Error::Malformed);
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(failure)? {
            if bytes.len() + chunk.len() > limit {
                return Err(Error::Malformed);
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests;
