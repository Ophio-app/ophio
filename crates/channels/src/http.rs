// SPDX-License-Identifier: Apache-2.0
use crate::{BotToken, Error, Result};
use reqwest::{Client, Method};
use serde_json::Value;
use std::{collections::HashMap, sync::Arc, time::Duration};
use tokio::{sync::Mutex, time::Instant};
use tokio_util::sync::CancellationToken;
use zeroize::Zeroizing;

#[derive(Default)]
struct Rates {
    deadlines: HashMap<String, Instant>,
    buckets: HashMap<String, String>,
    global: Option<Instant>,
}
impl Rates {
    /// Holds `key` until `deadline`. Limits that have passed are dropped with
    /// the buckets that pointed at them, so keys for chats that were limited
    /// once don't pile up.
    fn hold(&mut self, key: String, deadline: Instant) {
        let now = Instant::now();
        self.deadlines.retain(|_, d| *d > now);
        self.deadlines.insert(key, deadline);
        let deadlines = &self.deadlines;
        self.buckets
            .retain(|_, bucket| deadlines.contains_key(bucket));
    }
}
/// How a call meets a limit on its own route. A global limit is always waited out.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Pace {
    /// Wait until the platform allows the call: calls about the bot itself,
    /// and the rest of a message whose first part is already posted.
    Wait,
    /// Wait out ordinary pacing only. A longer limit on the call's chat is
    /// returned as `Error::RateLimited`, so the caller serves other chats
    /// meanwhile instead of holding them up.
    Yield,
}
/// Waits this short are ordinary pacing: cheaper to wait out in place than to
/// retry, and they hold other chats up only briefly.
const PACING: Duration = Duration::from_secs(1);
#[derive(Clone)]
pub(crate) struct Http {
    client: Client,
    token: Arc<BotToken>,
    base: String,
    telegram: bool,
    rates: Arc<Mutex<Rates>>,
    stop: CancellationToken,
}
impl Http {
    pub fn new(
        base: String,
        token: impl Into<Arc<BotToken>>,
        telegram: bool,
        stop: CancellationToken,
    ) -> Result<Self> {
        let client = Client::builder()
            .use_rustls_tls()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(40))
            .build()
            .map_err(|_| Error::Connection)?;
        Ok(Self {
            client,
            token: token.into(),
            base: base.trim_end_matches('/').into(),
            telegram,
            rates: Arc::new(Mutex::new(Rates::default())),
            stop,
        })
    }
    pub fn token(&self) -> &str {
        self.token.expose()
    }
    #[cfg(test)]
    pub(crate) async fn rate_limit_deadlines(
        &self,
        route: &str,
    ) -> (Option<Instant>, Option<Instant>) {
        let rates = self.rates.lock().await;
        (rates.deadlines.get(route).copied(), rates.global)
    }
    pub(crate) async fn wait_for_rate_limit(
        &self,
        path: &str,
        route: &str,
        pace: Pace,
    ) -> Result<()> {
        let (held, global) = {
            let rates = self.rates.lock().await;
            let bucket = rates
                .buckets
                .get(route)
                .map(String::as_str)
                .unwrap_or(route);
            (
                rates.deadlines.get(bucket).copied(),
                if path.starts_with("interactions/") || path.starts_with("webhooks/") {
                    None
                } else {
                    rates.global
                },
            )
        };
        if pace == Pace::Yield
            && let Some(wait) = held
                .map(|held| held.saturating_duration_since(Instant::now()))
                .filter(|wait| *wait > PACING)
        {
            return Err(Error::RateLimited(wait));
        }
        if let Some(deadline) = held.into_iter().chain(global).max() {
            tokio::select! {_=self.stop.cancelled()=>return Err(Error::Shutdown),_=tokio::time::sleep_until(deadline)=>{}}
        }
        Ok(())
    }
    pub async fn request(
        &self,
        method: Method,
        path: &str,
        route: &str,
        body: Option<Value>,
        pace: Pace,
    ) -> Result<Value> {
        let url = Zeroizing::new(if self.telegram {
            format!("{}/bot{}/{}", self.base, self.token.expose(), path)
        } else {
            format!("{}/{}", self.base, path)
        });
        loop {
            self.wait_for_rate_limit(path, route, pace).await?;
            let mut request = self.client.request(method.clone(), url.as_str());
            // Interaction tokens authenticate their own webhook/callback route.
            if !self.telegram
                && !path.starts_with("interactions/")
                && !path.starts_with("webhooks/")
            {
                let authorization = Zeroizing::new(format!("Bot {}", self.token.expose()));
                let mut header = reqwest::header::HeaderValue::from_str(&authorization)
                    .map_err(|_| Error::Configuration("invalid bot token format"))?;
                // reqwest/http owns this copy; sensitive suppresses debug output.
                header.set_sensitive(true);
                request = request.header(reqwest::header::AUTHORIZATION, header);
            }
            if let Some(body) = &body {
                request = request.json(body);
            }
            let mut response = tokio::select! {_=self.stop.cancelled()=>return Err(Error::Shutdown),response=request.send()=>response.map_err(|_|Error::Connection)?};
            let status = response.status().as_u16();
            // Conflict is terminal even if a proxy replaced the JSON body.
            if self.telegram && status == 409 {
                return Err(Error::Conflict);
            }
            // These statuses need no response content. Preserve their policy even
            // when an error page is oversized or its body connection drops.
            if matches!(status, 401 | 403 | 500..=599) {
                return Err(Error::Http(status));
            }
            let headers = response.headers().clone();
            let mut bytes = Vec::new();
            loop {
                let chunk = tokio::select! {_=self.stop.cancelled()=>return Err(Error::Shutdown),chunk=response.chunk()=>chunk.map_err(|_|Error::Connection)?};
                let Some(chunk) = chunk else {
                    break;
                };
                if bytes.len() + chunk.len() > 4 * 1024 * 1024 {
                    return Err(if status == 429 {
                        Error::Http(429)
                    } else {
                        Error::PlatformProtocol
                    });
                }
                bytes.extend_from_slice(&chunk);
            }
            let value: Value = if bytes.is_empty() {
                Value::Null
            } else {
                match serde_json::from_slice(&bytes) {
                    Ok(value) => value,
                    Err(_) if !(200..300).contains(&status) => Value::Null,
                    Err(_) => return Err(Error::PlatformProtocol),
                }
            };
            let code = if self.telegram {
                value["error_code"]
                    .as_u64()
                    .and_then(|v| u16::try_from(v).ok())
                    .unwrap_or(status)
            } else {
                status
            };
            if self.telegram && code == 409 {
                return Err(Error::Conflict);
            }
            if code == 429 {
                let seconds = if self.telegram {
                    value["parameters"]["retry_after"].as_f64()
                } else {
                    value["retry_after"].as_f64()
                }
                .or_else(|| {
                    headers
                        .get("retry-after")
                        .and_then(|h| h.to_str().ok())
                        .and_then(|h| h.parse().ok())
                })
                .ok_or(Error::Http(429))?;
                let duration = delay(seconds).map_err(|_| Error::Http(429))?;
                let mut rates = self.rates.lock().await;
                let deadline = Instant::now() + duration;
                if value["global"].as_bool() == Some(true) {
                    rates.global = Some(deadline);
                } else {
                    let key = rates
                        .buckets
                        .get(route)
                        .cloned()
                        .unwrap_or_else(|| route.into());
                    rates.hold(key, deadline);
                }
                continue;
            }
            if !self.telegram {
                let mut rates = self.rates.lock().await;
                if headers
                    .get("x-ratelimit-remaining")
                    .and_then(|h| h.to_str().ok())
                    == Some("0")
                    // An unusable reset time sets no wait; it does not fail
                    // a request that the platform has already carried out.
                    && let Some(wait) = headers
                        .get("x-ratelimit-reset-after")
                        .and_then(|h| h.to_str().ok())
                        .and_then(|s| s.parse().ok())
                        .and_then(|seconds| delay(seconds).ok())
                {
                    // Discord bucket identities exclude major parameters; scope by
                    // route to avoid mixing independent channels/webhook credentials.
                    // A bucket is remembered only while it holds calls back.
                    let key = match headers
                        .get("x-ratelimit-bucket")
                        .and_then(|h| h.to_str().ok())
                    {
                        Some(bucket) => {
                            let bucket = format!("{route}:{bucket}");
                            rates.buckets.insert(route.into(), bucket.clone());
                            bucket
                        }
                        None => rates
                            .buckets
                            .get(route)
                            .cloned()
                            .unwrap_or_else(|| route.into()),
                    };
                    rates.hold(key, Instant::now() + wait);
                }
            }
            if !(200..300).contains(&code) {
                // Idempotent edits may be replayed after a local crash.
                if self.telegram
                    && code == 400
                    && value["description"]
                        .as_str()
                        .is_some_and(|s| s.contains("message is not modified"))
                {
                    return Ok(Value::Null);
                }
                return Err(Error::Http(code));
            }
            if self.telegram {
                if value["ok"].as_bool() != Some(true) {
                    return Err(Error::PlatformProtocol);
                }
                return Ok(value["result"].clone());
            }
            return Ok(value);
        }
    }
}
fn delay(seconds: f64) -> Result<Duration> {
    if !seconds.is_finite() || !(0.0..=86400.0).contains(&seconds) {
        return Err(Error::PlatformProtocol);
    }
    Ok(Duration::from_secs_f64(seconds.max(0.01)))
}
pub(crate) async fn backoff(stop: &CancellationToken, seconds: u64) -> Result<()> {
    tokio::select! {_=stop.cancelled()=>Err(Error::Shutdown),_=tokio::time::sleep(Duration::from_secs(seconds))=>Ok(())}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_that_have_passed_are_dropped_with_their_buckets() {
        let mut rates = Rates::default();
        let now = Instant::now();
        for chat in 0..100 {
            let bucket = format!("messages:{chat}:synthetic");
            rates
                .buckets
                .insert(format!("messages:{chat}"), bucket.clone());
            rates.hold(bucket, now);
        }
        rates.hold("messages:100".into(), now + Duration::from_secs(60));
        assert_eq!(rates.deadlines.len(), 1);
        assert!(rates.buckets.is_empty());
        rates
            .buckets
            .insert("messages:101".into(), "messages:101:synthetic".into());
        rates.hold(
            "messages:101:synthetic".into(),
            now + Duration::from_secs(60),
        );
        assert_eq!(rates.deadlines.len(), 2);
        assert_eq!(rates.buckets.len(), 1);
    }
}
